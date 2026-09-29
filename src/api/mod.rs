//! 共享的 HTTP 客户端与 JSON 辅助。
//!
//! 基址都做成可注入的字段（默认是线上地址），契约测试里指向本地 mock 服务器。

pub mod list;
pub mod playurl;
pub mod view;

use std::sync::Arc;
use std::time::Duration;

use reqwest::Client;
use reqwest::header::{ACCEPT, HeaderMap, HeaderValue, REFERER, USER_AGENT};
use reqwest::redirect::Policy;
use serde_json::Value;

use crate::error::{Error, Result};

/// 进程内固定的 User-Agent。不同请求用不同 UA 是最明显的爬虫特征。
pub const USER_AGENTS: &[&str] = &[
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36",
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/125.0.0.0 Safari/537.36",
];

/// API 基址。测试里换成 mock 服务器。
#[derive(Debug, Clone)]
pub struct Bases {
    /// 例如 `https://api.bilibili.com`
    pub api: String,
    /// 例如 `https://www.bilibili.com`
    pub web: String,
}

impl Default for Bases {
    fn default() -> Self {
        // 允许用环境变量把基址指到本地 mock 服务器，方便端到端测试自己的 CLI。
        Self {
            api: std::env::var("VD_API_BASE")
                .unwrap_or_else(|_| "https://api.bilibili.com".to_string()),
            web: std::env::var("VD_WEB_BASE")
                .unwrap_or_else(|_| "https://www.bilibili.com".to_string()),
        }
    }
}

/// 带 Cookie / UA 的共享客户端。
#[derive(Debug, Clone)]
pub struct Api {
    client: Client,
    bases: Arc<Bases>,
    ua: Arc<str>,
    cookie: Option<Arc<str>>,
}

impl Api {
    /// 建一个 API 客户端。`ua` 由调用方在启动时随机挑一个后固定下来。
    pub fn new(bases: Bases, ua: &str, cookie: Option<&str>) -> Result<Self> {
        let client = Client::builder()
            .user_agent(ua)
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(30))
            // API 请求允许跳到可信域名（b23.tv → bilibili.com），逐跳校验
            .redirect(Policy::custom(|attempt| {
                let hops = attempt.previous().len();
                let url = attempt.url().clone();
                let trusted = crate::resolve::is_trusted_host(url.host_str().unwrap_or(""));
                if hops > 3 {
                    return attempt.error(Error::TooManyRedirects(hops));
                }
                if !trusted {
                    return attempt.error(Error::UntrustedRedirect(url.as_str().to_string()));
                }
                attempt.follow()
            }))
            .build()
            .map_err(|e| Error::Network(e.to_string()))?;
        Ok(Self {
            client,
            bases: Arc::new(bases),
            ua: Arc::from(ua),
            cookie: cookie.map(Arc::from),
        })
    }

    pub fn bases(&self) -> &Bases {
        &self.bases
    }

    pub fn cookie(&self) -> Option<&str> {
        self.cookie.as_deref()
    }

    /// UA，供下载模块复用（同一进程固定）。
    pub fn user_agent(&self) -> &str {
        &self.ua
    }

    /// 带默认头的 GET。
    pub async fn get(&self, url: &str) -> Result<reqwest::Response> {
        let mut req = self
            .client
            .get(url)
            .header(REFERER, "https://www.bilibili.com/")
            .header(ACCEPT, "application/json, text/plain, */*");
        if let Some(cookie) = &self.cookie {
            req = req.header(reqwest::header::COOKIE, cookie.as_ref());
        }
        let resp = req.send().await.map_err(network_err)?;
        let status = resp.status();
        if !status.is_success() {
            // HTTP 层的限流/权限信号与业务码语义一致，先映射掉
            return Err(match status.as_u16() {
                412 => Error::RiskControl,
                403 => Error::Forbidden,
                404 => Error::NotFound,
                // 5xx / 408 / 429 值得重试
                code => Error::Http {
                    status: code,
                    url: url.to_string(),
                    retry_after_secs: resp
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.trim().parse().ok()),
                },
            });
        }
        Ok(resp)
    }

    /// GET 并把响应体解析成 JSON。
    ///
    /// 非 JSON 响应（风控页 / 登录页是 HTML）给出可读错误，而不是 JSON 解析失败。
    pub async fn get_json(&self, url: &str) -> Result<Value> {
        self.get_json_full(url).await.map(|(value, _)| value)
    }

    /// 同 `get_json`，但把响应头一并返回（`nav` 需要用 `Date` 校正时钟）。
    pub async fn get_json_full(&self, url: &str) -> Result<(Value, HeaderMap)> {
        let resp = self.get(url).await?;
        let headers = resp.headers().clone();
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let body = resp.text().await.map_err(network_err)?;
        parse_json(&body, content_type, url).map(|value| (value, headers))
    }

    /// 请求头构造（下载模块需要一致的 Referer/UA/Cookie）。
    pub fn media_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            REFERER,
            HeaderValue::from_static("https://www.bilibili.com/"),
        );
        if let Ok(ua) = HeaderValue::from_str(&self.ua) {
            headers.insert(USER_AGENT, ua);
        }
        if let Some(value) = self
            .cookie
            .as_deref()
            .and_then(|cookie| HeaderValue::from_str(cookie).ok())
        {
            headers.insert(reqwest::header::COOKIE, value);
        }
        headers
    }

    /// 下载/媒体用的客户端：**禁止自动跟随重定向**，避免把 Cookie 带到任意主机。
    pub fn media_client(&self) -> Result<Client> {
        Client::builder()
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(30))
            .default_headers(self.media_headers())
            .build()
            .map_err(|e| Error::Network(e.to_string()))
    }
}

pub fn network_err(e: reqwest::Error) -> Error {
    if e.is_timeout() {
        return Error::Network(format!("超时: {e}"));
    }
    Error::Network(e.to_string())
}

/// 解析 JSON 文本；HTML 之类的响应体给出带片段的可读错误。
pub fn parse_json(body: &str, content_type: Option<String>, url: &str) -> Result<Value> {
    match serde_json::from_str::<Value>(body) {
        Ok(value) => Ok(value),
        Err(_) => {
            let snippet: String = body.chars().take(200).collect();
            let looks_like_html = body.trim_start().starts_with('<')
                || content_type
                    .as_deref()
                    .is_some_and(|ct| ct.contains("text/html"));
            if looks_like_html {
                tracing::debug!("响应疑似风控/登录页: {url}");
            }
            Err(Error::NotJson {
                content_type,
                snippet,
            })
        }
    }
}

/// 业务码 → 错误。
pub fn map_biz_code(code: i64, message: &str) -> Result<()> {
    match code {
        0 => Ok(()),
        -404 => Err(Error::NotFound),
        -403 => Err(Error::Forbidden),
        -412 => Err(Error::RiskControl),
        -101 => Err(Error::NeedLogin),
        -10403 => Err(Error::VipRequired),
        -86038 => Err(Error::RegionRestricted),
        -352 => Err(Error::SignCheckFailed),
        other => Err(Error::Api {
            code: other,
            message: message.to_string(),
        }),
    }
}

/// 顶层业务码校验（先看 `code`，再看 `data`——顺序反了会得到与真实原因无关的报错）。
pub fn check_biz_code(root: &Value) -> Result<()> {
    let code = root["code"].as_i64().unwrap_or(0);
    let message = root["message"].as_str().unwrap_or("未知错误");
    map_biz_code(code, message)
}

/// 播放限制（仅番剧响应有；UGC 可跳过但保留防御）。
pub fn check_play_limit(root: &Value) -> Result<()> {
    let limit = &root["play_check"]["limit_play_reason"];
    if limit.is_null() {
        return Ok(());
    }
    let reason = limit
        .as_i64()
        .or_else(|| limit.as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(-1);
    if reason == 0 {
        return Ok(());
    }
    let msg = root["play_check"]["limit_play_reason_msg"]
        .as_str()
        .unwrap_or("播放受限");
    Err(Error::Api {
        code: reason,
        message: format!("播放受限: {msg}"),
    })
}

/// 数字字段，宽进（u64 / i64 / 字符串都吃）严出。
pub fn get_u64(node: &Value, key: &str) -> Option<u64> {
    let v = &node[key];
    v.as_u64()
        .or_else(|| v.as_i64().and_then(|i| u64::try_from(i).ok()))
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

pub fn get_u32(node: &Value, key: &str) -> Option<u32> {
    get_u64(node, key).and_then(|n| u32::try_from(n).ok())
}

pub fn get_str<'a>(node: &'a Value, key: &str) -> Option<&'a str> {
    node[key].as_str()
}

/// `frame_rate` 有时是字符串有时是数字。
pub fn get_frame_rate(node: &Value) -> String {
    match &node["frame_rate"] {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// PCDN 节点形如 `http://1.2.3.4:8080/...`，可用性差，优先过滤掉。
///
/// 规则就是 spec §7.5 给的正则 `^https?://[^/:]+:\d+`：带显式端口的地址基本都是
/// PCDN 边缘节点（`[^/:]+` 不含 `:`，所以 IPv6 字面量不会被误伤）。
pub fn is_host_port_literal(url: &str) -> bool {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^https?://[^/:]+:\d+").expect("内置正则应当可编译")
    });
    RE.is_match(url)
}

/// 收集轨道候选地址：`base_url` + `backup_url[]`，过滤 PCDN 字面量 IP 节点。
///
/// 过滤后若为空则回退到原始列表——宁可试一个差的也不要不试。
pub fn collect_urls(node: &Value) -> Vec<String> {
    let mut urls: Vec<String> = Vec::new();
    if let Some(u) = node["base_url"].as_str() {
        urls.push(u.to_string());
    }
    // 有些响应用 baseUrl / backupUrl
    if urls.is_empty()
        && let Some(u) = node["baseUrl"].as_str()
    {
        urls.push(u.to_string());
    }
    for key in ["backup_url", "backupUrl"] {
        if let Some(arr) = node[key].as_array() {
            urls.extend(arr.iter().filter_map(|v| v.as_str().map(String::from)));
        }
    }
    urls.dedup();
    let filtered: Vec<String> = urls
        .iter()
        .filter(|u| !is_host_port_literal(u))
        .cloned()
        .collect();
    if filtered.is_empty() { urls } else { filtered }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn biz_code_mapping() {
        for (code, expect) in [
            (-404i64, "NotFound"),
            (-403, "Forbidden"),
            (-412, "RiskControl"),
            (-101, "NeedLogin"),
            (-10403, "VipRequired"),
            (-86038, "RegionRestricted"),
            (-352, "SignCheckFailed"),
        ] {
            let root = json!({"code": code, "message": "x"});
            let err = check_biz_code(&root).unwrap_err();
            assert!(format!("{err:?}").contains(expect), "{code} → {err}");
        }
        assert!(check_biz_code(&json!({"code": 0})).is_ok());
        let err = check_biz_code(&json!({"code": -999, "message": "自定义"})).unwrap_err();
        assert!(matches!(err, Error::Api { code: -999, .. }), "{err}");
        // 缺少 code 字段按成功处理（有些接口不返回 code）
        assert!(check_biz_code(&json!({"data": {}})).is_ok());
    }

    #[test]
    fn code_checked_before_data() {
        // -404 响应里没有 data，先判断 code 才能得到 NotFound
        let root = json!({"code": -404, "message": "啥都木有"});
        assert!(matches!(check_biz_code(&root), Err(Error::NotFound)));
    }

    #[test]
    fn html_body_is_readable_error() {
        let err = parse_json(
            "<html><body>风控</body></html>",
            Some("text/html".into()),
            "u",
        )
        .unwrap_err();
        match err {
            Error::NotJson { snippet, .. } => assert!(snippet.contains("风控")),
            other => panic!("{other}"),
        }
    }

    #[test]
    fn json_body_parses() {
        assert_eq!(parse_json(r#"{"a":1}"#, None, "u").unwrap()["a"], 1);
    }

    #[test]
    fn play_limit_detection() {
        assert!(check_play_limit(&json!({})).is_ok());
        assert!(check_play_limit(&json!({"play_check": {"limit_play_reason": 0}})).is_ok());
        let err =
            check_play_limit(&json!({"play_check": {"limit_play_reason": 87008}})).unwrap_err();
        assert!(err.to_string().contains("87008"), "{err}");
    }

    #[test]
    fn host_port_literal_filter() {
        // spec §7.5：带端口的一律按 PCDN 边缘节点处理
        assert!(is_host_port_literal("http://1.2.3.4:8080/v.mp4"));
        assert!(is_host_port_literal("https://10.0.0.1:443/v.mp4?x=1"));
        assert!(is_host_port_literal(
            "https://upos-sz-mirrorali.bilivideo.com:8080/x"
        ));
        // 不含端口的地址不被过滤
        assert!(!is_host_port_literal(
            "https://upos-sz-mirrorali.bilivideo.com/x"
        ));
        assert!(!is_host_port_literal("http://1.2.3.4/v.mp4"));
        // IPv6 字面量里含 ':'，正则的 [^/:]+ 不匹配，因此不会被误砍
        assert!(!is_host_port_literal("https://[2001:db8::1]:8080/v"));
        assert!(!is_host_port_literal("not a url"));
    }

    #[test]
    fn collects_base_and_backup_urls() {
        let node = json!({
            "base_url": "https://a.example/v.m4s",
            "backup_url": ["https://b.example/v.m4s", "https://c.example/v.m4s"]
        });
        let urls = collect_urls(&node);
        assert_eq!(urls.len(), 3);
        assert_eq!(urls[0], "https://a.example/v.m4s");
        assert_eq!(urls[2], "https://c.example/v.m4s");
    }

    #[test]
    fn filters_pcdn_but_keeps_others() {
        let node = json!({
            "base_url": "http://1.2.3.4:8080/v.m4s",
            "backup_url": ["https://b.example/v.m4s"]
        });
        assert_eq!(collect_urls(&node), vec!["https://b.example/v.m4s"]);
    }

    #[test]
    fn falls_back_to_raw_when_all_filtered() {
        let node = json!({ "base_url": "http://1.2.3.4:8080/v.m4s" });
        assert_eq!(collect_urls(&node), vec!["http://1.2.3.4:8080/v.m4s"]);
    }

    #[test]
    fn numeric_fields_are_lenient() {
        let node = json!({"a": 5, "b": "7", "c": -1, "d": 1.5});
        assert_eq!(get_u64(&node, "a"), Some(5));
        assert_eq!(get_u64(&node, "b"), Some(7));
        assert_eq!(get_u64(&node, "c"), None);
        assert_eq!(get_u64(&node, "d"), None);
        assert_eq!(get_u32(&node, "b"), Some(7));
    }
}
