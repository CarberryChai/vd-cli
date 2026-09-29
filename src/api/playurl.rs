//! `x/player/wbi/playurl`：取流。
//!
//! MVP 只实现 WEB 一种模式。两轮请求：`qn=0` 拿可用轨道，`qn=127` 拿免二压。

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::{
    Api, check_biz_code, check_play_limit, collect_urls, get_frame_rate, get_str, get_u32, get_u64,
};
use crate::error::{Error, Result};
use crate::model::{AudioTrack, Dash, VideoTrack, normalize_audio_codecs};
use crate::wbi;

/// 第一轮请求：拿可用轨道与可用清晰度列表。
pub const FIRST_ROUND_QN: u32 = 0;
/// 第二轮请求：请求最高画质，触发"免二压"返回原始码率版本。
pub const SECOND_ROUND_QN: u32 = 127;
/// 第二轮用的最高画质码（= 8K），保留别名便于阅读。
pub const MAX_QN: u32 = SECOND_ROUND_QN;

impl Api {
    /// 取流：先 `qn=0`，再尝试 `qn=127` 拿免二压。
    ///
    /// 第二轮的失败（拒绝、超时、解析失败、`dash.video` 为空）都只 warn 并沿用
    /// 第一轮结果；但用户取消必须传播。
    pub async fn playurl(&self, aid: u64, cid: u64, mixin_key: &str, fnval: u32) -> Result<Dash> {
        self.playurl_with(aid, cid, mixin_key, fnval, &CancellationToken::new())
            .await
    }

    pub async fn playurl_with(
        &self,
        aid: u64,
        cid: u64,
        mixin_key: &str,
        fnval: u32,
        cancel: &CancellationToken,
    ) -> Result<Dash> {
        let first = self
            .playurl_once(aid, cid, FIRST_ROUND_QN, mixin_key, fnval)
            .await?;

        match self
            .playurl_once(aid, cid, SECOND_ROUND_QN, mixin_key, fnval)
            .await
        {
            Ok(second) if !second.video.is_empty() => Ok(second),
            Ok(_) => {
                tracing::warn!("qn=127 没有返回视频轨，沿用第一轮结果");
                Ok(first)
            }
            Err(Error::Interrupted) if cancel.is_cancelled() => Err(Error::Interrupted),
            Err(e) => {
                // 第二轮失败不得吞掉用户取消
                if cancel.is_cancelled() {
                    return Err(Error::Interrupted);
                }
                tracing::warn!("qn=127 失败（{e}），沿用第一轮结果");
                Ok(first)
            }
        }
    }

    /// 单轮取流。
    async fn playurl_once(
        &self,
        aid: u64,
        cid: u64,
        qn: u32,
        mixin_key: &str,
        fnval: u32,
    ) -> Result<Dash> {
        // 参数顺序即签名顺序，不要重排
        let mut query = format!(
            "support_multi_audio=true&from_client=BROWSER&avid={aid}&cid={cid}\
             &fnval={fnval}&fnver=0&fourk=1&otype=json&qn={qn}&wts={}",
            wbi::now_unix()
        );
        // 无 Cookie 且非 DRM 时追加 try_look=1
        if self.cookie().is_none() {
            query.push_str("&try_look=1");
        }
        let query = wbi::sign(&query, mixin_key);
        let url = format!("{}/x/player/wbi/playurl?{query}", self.bases().api);

        let resp = self.get_json(&url).await?;
        parse_playurl(&resp, cid)
    }
}

/// 是否 DRM 内容。看 DASH 根节点、`dash` 自身与顶层三处，响应版本不同位置也不同。
fn is_drm(root: &Value, dash: &Value) -> bool {
    dash["is_drm"].as_bool().unwrap_or(false)
        || root["is_drm"].as_bool().unwrap_or(false)
        || dash["drm_tech_type"].as_i64().is_some_and(|n| n != 0)
        || root["drm_tech_type"].as_i64().is_some_and(|n| n != 0)
}

/// 响应里是否带 `v_voucher`（风控验证码凭证，表示这次取流被拦了）。
fn has_v_voucher(resp: &Value) -> bool {
    resp["data"]["v_voucher"].is_string()
        || resp["result"]["v_voucher"].is_string()
        || resp["v_voucher"].is_string()
}

/// 定位 DASH 根节点：`result.video_info ?? result`，否则 `data`，否则顶层。
pub fn dash_root(resp: &Value) -> &Value {
    if let Some(result) = resp.get("result").filter(|r| r.is_object()) {
        if let Some(video_info) = result.get("video_info").filter(|v| v.is_object()) {
            return video_info;
        }
        return result;
    }
    if let Some(data) = resp.get("data").filter(|d| d.is_object()) {
        return data;
    }
    resp
}

/// 解析取流响应。
pub fn parse_playurl(resp: &Value, cid: u64) -> Result<Dash> {
    // 两层校验：先业务码，再播放限制
    check_biz_code(resp)?;
    let root = dash_root(resp);
    check_play_limit(root)?;

    // 老视频只有 FLV 分段（`dural[]`），MVP 直接报错退出
    for node in [root, resp] {
        for key in ["dural", "durl"] {
            let Some(arr) = node.get(key).and_then(|d| d.as_array()) else {
                continue;
            };
            if !arr.is_empty() && root.get("dash").is_none() {
                return Err(Error::LegacyFormat);
            }
        }
    }
    if root.get("dash").is_none() && root.get("durl").is_some() {
        return Err(Error::LegacyFormat);
    }

    // `code: 0` 但没有 dash、只有 v_voucher —— B 站的风控"索要验证码"响应。
    // 不识别的话会糊成"响应里没有 dash 字段"，用户无法知道该干什么。
    if has_v_voucher(resp) {
        return Err(Error::RiskControl);
    }

    let dash = root
        .get("dash")
        .filter(|d| d.is_object())
        .ok_or_else(|| Error::Api {
            code: 0,
            message: "响应里没有 dash 字段".into(),
        })?;

    // DRM 内容直接报错退出：明文拿不到流，解密有法律风险
    if is_drm(root, dash) {
        return Err(Error::Drm);
    }

    let mut video: Vec<VideoTrack> = Vec::new();
    for node in dash["video"].as_array().cloned().unwrap_or_default() {
        if node["is_drm"].as_bool().unwrap_or(false) {
            return Err(Error::Drm);
        }
        let urls = collect_urls(&node);
        if urls.is_empty() {
            continue;
        }
        let Some(id) = get_u32(&node, "id") else {
            continue;
        };
        video.push(VideoTrack {
            id,
            codecid: get_u32(&node, "codecid").unwrap_or(7),
            bandwidth: get_u64(&node, "bandwidth").unwrap_or(0),
            width: get_u32(&node, "width").unwrap_or(0),
            height: get_u32(&node, "height").unwrap_or(0),
            frame_rate: get_frame_rate(&node),
            size: get_u64(&node, "size").unwrap_or(0),
            urls,
        });
    }

    let mut audio: Vec<AudioTrack> = Vec::new();
    for node in dash["audio"].as_array().cloned().unwrap_or_default() {
        if let Some(track) = audio_track(&node) {
            audio.push(track);
        }
    }
    // 杜比与 Hi-Res 可能只有一个对象（不是数组），也可能缺失
    for path in [["dolby", "audio"], ["flac", "audio"]] {
        let node = &dash[path[0]][path[1]];
        if node.is_object()
            && let Some(track) = audio_track(node)
        {
            audio.push(track);
        }
    }
    // 同一 cid 的多个音频源可能重复
    audio.dedup_by(|a, b| a.id == b.id && a.bandwidth == b.bandwidth && a.urls == b.urls);

    let duration = get_u32(dash, "duration")
        .or_else(|| get_u64(root, "timelength").map(|ms| (ms / 1000) as u32))
        .unwrap_or(0);

    if video.is_empty() {
        // 拿到响应了但没有可用视频轨：这是"没有可用视频流"而不是解析失败
        return Err(Error::NoVideoStream);
    }
    if audio.is_empty() {
        tracing::warn!("cid={cid} 没有音频轨（可能是纯音乐/静音视频）");
    }

    Ok(Dash {
        video,
        audio,
        duration,
    })
}

fn audio_track(node: &Value) -> Option<AudioTrack> {
    let urls = collect_urls(node);
    if urls.is_empty() {
        return None;
    }
    // 有些响应把音频放在 audio[] 里但不给 id
    let id = get_u32(node, "id").unwrap_or(0);
    Some(AudioTrack {
        id,
        codecs: normalize_audio_codecs(get_str(node, "codecs").unwrap_or("")),
        bandwidth: get_u64(node, "bandwidth").unwrap_or(0),
        urls,
    })
}

/// 初始化 WBI 密钥。
///
/// ⚠️ 未登录时 `nav` 返回 `code: -101`，但**依然包含 `wbi_img`**。必须在判断登录
/// 状态之前提取密钥，否则未登录用户拿不到密钥，后续所有签名请求都会被 -352 拒绝。
pub async fn init_mixin_key(api: &Api) -> Result<String> {
    let url = format!("{}/x/web-interface/nav", api.bases().api);
    let (resp, headers) = api.get_json_full(&url).await?;
    // 签名有效期约 60 秒，容器/虚拟机时钟漂移会直接导致 -352
    if let Some(date) = headers
        .get(reqwest::header::DATE)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_http_date)
    {
        let skew = clock_skew(date, wbi::now_unix());
        if skew.abs() > 30 {
            tracing::warn!("本地时钟与服务器相差 {skew} 秒，签名可能失败；请检查系统时间");
        }
    }
    let img = resp["data"]["wbi_img"]["img_url"].as_str().unwrap_or("");
    let sub = resp["data"]["wbi_img"]["sub_url"].as_str().unwrap_or("");
    if img.is_empty() || sub.is_empty() {
        return Err(Error::Api {
            code: resp["code"].as_i64().unwrap_or(0),
            message: "nav 响应里没有 wbi_img".into(),
        });
    }
    Ok(wbi::mixin_key(img, sub)?)
}

/// 用 `/x/web-interface/nav` 的响应头 `Date` 校正本地时钟漂移。
///
/// 返回值是 `本地时间 - 服务器时间`，也就是本地时钟「快了多少」秒。
pub fn clock_skew(resp_date_secs: u64, local_now_secs: u64) -> i64 {
    local_now_secs as i64 - resp_date_secs as i64
}

/// 解析 HTTP 日期头（`Mon, 29 Sep 2026 01:23:45 GMT`）为 Unix 秒。
pub fn parse_http_date(value: &str) -> Option<u64> {
    let value = value.trim();
    let rest = value
        .split_once(',')
        .map(|(_, r)| r)
        .unwrap_or(value)
        .trim();
    let mut parts = rest.split_whitespace();
    let day: u32 = parts.next()?.parse().ok()?;
    let month = match parts.next()? {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year: i64 = parts.next()?.parse().ok()?;
    let mut hms = parts.next()?.split(':');
    let hour: i64 = hms.next()?.parse().ok()?;
    let minute: i64 = hms.next()?.parse().ok()?;
    let second: i64 = hms.next()?.parse().ok()?;
    if !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    Some(civil_to_unix(year, month, day, hour, minute, second))
}

/// Howard Hinnant 的 `days_from_civil`，避免为了一个时间戳引入 chrono。
fn civil_to_unix(year: i64, month: u32, day: u32, hour: i64, minute: i64, second: i64) -> u64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    (days * 86_400 + hour * 3600 + minute * 60 + second).max(0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn v_voucher_response_is_risk_control_not_a_dash_parse_error() {
        let resp: Value = serde_json::from_str(
            &std::fs::read_to_string(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/playurl_v_voucher.json"),
            )
            .unwrap(),
        )
        .unwrap();
        let err = parse_playurl(&resp, 1).unwrap_err();
        assert!(matches!(err, Error::RiskControl), "{err}");
        assert!(err.hints().iter().any(|h| h.contains("稍后重试")));
    }

    #[test]
    fn http_date_parsing() {
        // 2026-09-29T00:00:00Z
        assert_eq!(
            parse_http_date("Tue, 29 Sep 2026 00:00:00 GMT"),
            Some(1_790_640_000)
        );
        assert_eq!(parse_http_date("garbage"), None);
    }

    #[test]
    fn drm_content_is_rejected() {
        let resp = json!({"code": 0, "message": "OK", "data": {"dash": {
            "duration": 10, "is_drm": true,
            "video": [{"id": 80, "base_url": "https://a/v.m4s", "bandwidth": 1, "codecid": 7}],
            "audio": []}}});
        let err = parse_playurl(&resp, 1).unwrap_err();
        assert!(matches!(err, Error::Drm), "{err}");
        assert!(err.to_string().contains("DRM"));

        // 单条轨道标记 is_drm 也一样
        let resp = json!({"code": 0, "message": "OK", "data": {"dash": {
            "duration": 10,
            "video": [{"id": 80, "base_url": "https://a/v.m4s", "bandwidth": 1,
                       "codecid": 7, "is_drm": true}],
            "audio": []}}});
        assert!(matches!(parse_playurl(&resp, 1).unwrap_err(), Error::Drm));

        // 非 DRM 不受影响
        let resp = json!({"code": 0, "message": "OK", "data": {"dash": {
            "duration": 10, "is_drm": false,
            "video": [{"id": 80, "base_url": "https://a/v.m4s", "bandwidth": 1, "codecid": 7}],
            "audio": []}}});
        assert!(parse_playurl(&resp, 1).is_ok());
    }

    #[test]
    fn clock_skew_sign() {
        assert_eq!(clock_skew(100, 130), 30);
        assert_eq!(clock_skew(130, 100), -30);
    }

    #[test]
    fn missing_dash_without_voucher_is_still_reported() {
        // 既没有 dash 也不是老格式：说清楚缺了什么，而不是抛一个费解的解析错误
        let resp = json!({"code": 0, "message": "OK", "data": {}});
        let err = parse_playurl(&resp, 1).unwrap_err();
        assert!(err.to_string().contains("dash"), "{err}");
    }
}
