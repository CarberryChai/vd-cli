//! 契约测试：`playurl` 的 DASH 解析、错误映射与两轮请求。

mod common;

use common::{
    TEST_UA, api_for, api_for_with_cookie, fixture_json, fixture_raw, init_tracing, scrub_w_rid,
};
use vd_cli::api::playurl::{MAX_QN, SECOND_ROUND_QN, parse_playurl};
use vd_cli::error::Error;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn json_response(body: String) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "application/json")
        .set_body_string(body)
}

#[test]
fn dash_response_yields_all_tracks_and_urls() {
    init_tracing();
    let resp = fixture_json("playurl_dash.json");
    let dash = parse_playurl(&resp, 220_355_130).unwrap();

    // 4 条视频轨：1080P(avc) / 1080P(hevc) / 720P(avc) / 360P(只有 PCDN)
    assert_eq!(dash.video.len(), 4);
    assert_eq!(dash.duration, 226);

    let by_qn = |qn: u32| dash.video.iter().find(|t| t.id == qn).unwrap();

    let best = by_qn(80);
    assert_eq!(best.codecid, 7);
    assert_eq!(best.width, 1920);
    assert_eq!(best.height, 1080);
    assert_eq!(best.bandwidth, 1_500_000);
    assert_eq!(best.kbps(), 1500);
    assert_eq!(best.size, 42_000_000);
    assert_eq!(best.codec_name(), "AVC");
    // 主地址 + 备用地址都要收集，失败时按顺序换源
    assert_eq!(best.urls.len(), 2, "{:?}", best.urls);
    assert!(best.urls[0].contains("mirrorali"));
    assert!(best.urls[1].contains("mirrorcos"));

    assert_eq!(
        dash.video
            .iter()
            .find(|t| t.codecid == 12)
            .unwrap()
            .codec_name(),
        "HEVC"
    );

    // 只有 PCDN 字面量地址的轨道：过滤后回退到原始列表（宁可试差的）
    let pcdn = by_qn(16);
    assert_eq!(pcdn.urls.len(), 1);
    assert!(pcdn.urls[0].starts_with("http://1.2.3.4:8080"));
}

#[test]
fn dash_audio_is_normalized_and_includes_dolby_and_flac() {
    let resp = fixture_json("playurl_dash.json");
    let dash = parse_playurl(&resp, 1).unwrap();

    // audio[] 三条 + dolby.audio 一条 + flac.audio 一条
    let codecs: Vec<&str> = dash.audio.iter().map(|a| a.codecs.as_str()).collect();
    assert!(codecs.contains(&"M4A"), "{codecs:?}");
    assert!(codecs.contains(&"E-AC-3"), "{codecs:?}");
    assert!(codecs.contains(&"FLAC"), "{codecs:?}");
    // mp4a.40.2 / mp4a.40.5 都归一化成 M4A
    assert_eq!(
        dash.audio.iter().filter(|a| a.codecs == "M4A").count(),
        2,
        "{codecs:?}"
    );

    // 选轨结果：FLAC 优先
    let prefs = vd_cli::select::Prefs {
        quality_limit: u32::MAX,
        codec: vd_cli::model::Codec::Avc,
    };
    let picked = vd_cli::select::select_audio(&dash.audio, &prefs).unwrap();
    assert_eq!(picked.codecs, "FLAC");
    assert_eq!(picked.id, 30251);
}

#[test]
fn durl_only_response_reports_legacy_format() {
    let resp = fixture_json("playurl_durl.json");
    let err = parse_playurl(&resp, 1).unwrap_err();
    assert!(matches!(err, Error::LegacyFormat), "{err}");
    assert!(err.to_string().contains("老格式"), "{err}");
    assert_eq!(err.exit_code(), 1);
}

#[test]
fn top_level_dural_reports_legacy_format() {
    let resp = fixture_json("playurl_dural.json");
    let err = parse_playurl(&resp, 1).unwrap_err();
    assert!(matches!(err, Error::LegacyFormat), "{err}");
}

#[test]
fn play_check_limit_maps_to_readable_error() {
    let resp = fixture_json("playurl_play_check.json");
    let err = parse_playurl(&resp, 1).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("87008"), "{msg}");
    assert!(msg.contains("无法播放"), "{msg}");
}

#[test]
fn biz_error_codes_map_to_expected_variants() {
    for (fixture, check) in [
        ("playurl_risk.json", "RiskControl"),
        ("playurl_need_login.json", "NeedLogin"),
        ("playurl_vip.json", "VipRequired"),
        ("playurl_forbidden.json", "Forbidden"),
        ("playurl_region.json", "RegionRestricted"),
        ("playurl_sign.json", "SignCheckFailed"),
    ] {
        let resp = fixture_json(fixture);
        let err = parse_playurl(&resp, 1).unwrap_err();
        assert!(
            format!("{err:?}").contains(check),
            "{fixture} → {err:?}（期望 {check}）"
        );
    }
    // 退出码语义
    assert_eq!(
        parse_playurl(&fixture_json("playurl_need_login.json"), 1)
            .unwrap_err()
            .exit_code(),
        4
    );
    assert_eq!(
        parse_playurl(&fixture_json("playurl_risk.json"), 1)
            .unwrap_err()
            .exit_code(),
        3
    );
}

#[test]
fn sign_failure_error_mentions_both_causes() {
    let err = parse_playurl(&fixture_json("playurl_sign.json"), 1).unwrap_err();
    let hints = err.hints().join("\n");
    assert!(hints.contains("系统时间"), "{hints}");
    assert!(hints.contains("稍后重试"), "{hints}");
}

#[tokio::test]
async fn html_risk_page_is_not_a_json_parse_failure() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/player/wbi/playurl"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/html; charset=utf-8")
                .set_body_string(fixture_raw("playurl_risk.html")),
        )
        .mount(&server)
        .await;

    let api = api_for(&server);
    let err = api.playurl(1, 2, "key", 4048).await.unwrap_err();
    match err {
        Error::NotJson { ref snippet, .. } => {
            assert!(snippet.contains("拦截"), "{snippet}");
        }
        other => panic!("期望 NotJson，实际 {other}"),
    }
    assert!(!err.hints().is_empty(), "要告诉用户下一步");
}

#[tokio::test]
async fn two_round_requests_use_documented_parameters() {
    init_tracing();
    let server = MockServer::start().await;

    // 第一轮 qn=0
    Mock::given(method("GET"))
        .and(path("/x/player/wbi/playurl"))
        .and(query_param("qn", "0"))
        .and(query_param("avid", "626497566"))
        .and(query_param("cid", "220355130"))
        .and(query_param("fnval", "4048"))
        .and(query_param("fnver", "0"))
        .and(query_param("fourk", "1"))
        .and(query_param("otype", "json"))
        .and(query_param("support_multi_audio", "true"))
        .and(query_param("from_client", "BROWSER"))
        .and(query_param("try_look", "1"))
        .and(header("referer", "https://www.bilibili.com/"))
        .and(header("user-agent", TEST_UA))
        .respond_with(json_response(fixture_raw("playurl_dash.json")))
        .mount(&server)
        .await;

    // 第二轮 qn=127，返回一份只有 8K 轨的响应证明它被采用了
    let second = serde_json::json!({
        "code": 0, "message": "OK",
        "data": {"dash": {"duration": 226, "audio": [],
            "video": [{"id": 127, "base_url": "https://a.example/8k.m4s",
                       "bandwidth": 9_000_000, "codecid": 7,
                       "width": 7680, "height": 4320, "frame_rate": "60", "size": 1}]}}
    });
    Mock::given(method("GET"))
        .and(path("/x/player/wbi/playurl"))
        .and(query_param("qn", "127"))
        .respond_with(json_response(second.to_string()))
        .mount(&server)
        .await;

    let api = api_for(&server);
    let dash = api
        .playurl(626_497_566, 220_355_130, "key", 4048)
        .await
        .unwrap();
    // 第二轮有非空 dash.video → 整体替换第一轮结果
    assert_eq!(dash.video.len(), 1);
    assert_eq!(dash.video[0].id, 127);

    // 两轮都带上了签名字段
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2, "应当请求两轮");
    for req in &requests {
        let q = scrub_w_rid(req.url.query().unwrap_or(""));
        assert!(q.contains("&w_rid=<hex32>"), "{q}");
        assert_eq!(
            q,
            format!(
                "support_multi_audio=true&from_client=BROWSER&avid=626497566&cid=220355130\
                 &fnval=4048&fnver=0&fourk=1&otype=json&qn={}&wts={}&try_look=1&w_rid=<hex32>",
                q.split("qn=").nth(1).unwrap().split('&').next().unwrap(),
                q.split("wts=").nth(1).unwrap().split('&').next().unwrap(),
            ),
            "参数顺序必须与文档一致（签名按构造顺序）"
        );
    }
}

#[tokio::test]
async fn second_round_without_video_keeps_first_round() {
    init_tracing();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/player/wbi/playurl"))
        .and(query_param("qn", "0"))
        .respond_with(json_response(fixture_raw("playurl_dash.json")))
        .mount(&server)
        .await;
    // 第二轮返回 dash.video 为空
    Mock::given(method("GET"))
        .and(path("/x/player/wbi/playurl"))
        .and(query_param("qn", "127"))
        .respond_with(json_response(
            serde_json::json!({"code": 0, "message": "OK",
                "data": {"dash": {"duration": 226, "video": [], "audio": []}}})
            .to_string(),
        ))
        .mount(&server)
        .await;

    let api = api_for(&server);
    let dash = api.playurl(1, 2, "key", 4048).await.unwrap();
    assert_eq!(dash.video.len(), 4, "应沿用第一轮结果");
}

#[tokio::test]
async fn second_round_failure_keeps_first_round() {
    init_tracing();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/player/wbi/playurl"))
        .and(query_param("qn", "0"))
        .respond_with(json_response(fixture_raw("playurl_dash.json")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/x/player/wbi/playurl"))
        .and(query_param("qn", "127"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let api = api_for(&server);
    // 第二轮 5xx 不得让整个视频失败
    let dash = api.playurl(1, 2, "key", 4048).await.unwrap();
    assert_eq!(dash.video.len(), 4);
}

#[tokio::test]
async fn second_round_failure_still_propagates_cancel() {
    init_tracing();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/player/wbi/playurl"))
        .and(query_param("qn", "0"))
        .respond_with(json_response(fixture_raw("playurl_dash.json")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/x/player/wbi/playurl"))
        .and(query_param("qn", "127"))
        .respond_with(ResponseTemplate::new(500).set_delay(std::time::Duration::from_millis(300)))
        .mount(&server)
        .await;

    let cancel = tokio_util::sync::CancellationToken::new();
    let api = api_for(&server);
    let handle = {
        let cancel = cancel.clone();
        let api = api.clone();
        tokio::spawn(async move { api.playurl_with(1, 2, "key", 4048, &cancel).await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    cancel.cancel();
    // 取消后第二次请求的失败不能被当成"沿用第一轮结果"咽掉，
    // 否则用户按了 Ctrl+C 流程还会继续跑
    let res = tokio::time::timeout(std::time::Duration::from_secs(5), handle)
        .await
        .expect("不应超时")
        .unwrap();
    assert!(
        matches!(res, Err(Error::Interrupted)),
        "取消后必须直接向上传播 Interrupted，实际 {res:?}"
    );
}

#[tokio::test]
async fn cookie_on_media_requests_and_no_try_look() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/player/wbi/playurl"))
        .and(query_param("qn", "0"))
        .and(header("cookie", "SESSDATA=x"))
        .respond_with(json_response(fixture_raw("playurl_dash.json")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/x/player/wbi/playurl"))
        .and(query_param("qn", "127"))
        .respond_with(json_response(
            "{\"code\":0,\"data\":{\"dash\":{\"video\":[]}}}".to_string(),
        ))
        .mount(&server)
        .await;

    let api = api_for_with_cookie(&server, "SESSDATA=x");
    let dash = api.playurl(1, 2, "key", 4048).await.unwrap();
    assert_eq!(dash.video.len(), 4);

    // 有 Cookie 时不追加 try_look
    for req in server.received_requests().await.unwrap() {
        let q = req.url.query().unwrap_or("");
        assert!(!q.contains("try_look"), "{q}");
        assert!(q.contains("w_rid="), "{q}");
        assert_eq!(
            req.headers.get("referer").unwrap(),
            "https://www.bilibili.com/"
        );
    }
}

#[tokio::test]
async fn fnval_can_be_overridden() {
    // 风险 2 的缓解：fnval 是常量但可覆盖
    assert_eq!(vd_cli::cli::DEFAULT_FNVAL, 4048);
    assert_eq!(MAX_QN, 127);
    assert_eq!(SECOND_ROUND_QN, 127);
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/player/wbi/playurl"))
        .and(query_param("fnval", "16"))
        .respond_with(json_response(fixture_raw("playurl_dash.json")))
        .mount(&server)
        .await;
    let api = api_for(&server);
    assert_eq!(api.playurl(1, 2, "key", 16).await.unwrap().video.len(), 4);
}
