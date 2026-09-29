//! 契约测试：下载行为（mock HTTP）。

mod common;

use std::path::Path;
use std::time::Duration;

use common::{TEST_UA, api_for};
use tokio_util::sync::CancellationToken;
use vd_cli::download::download;
use vd_cli::error::Error;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

#[tokio::test]
async fn downloads_body_and_verifies_length() {
    let server = MockServer::start().await;
    let payload = vec![7u8; 4096];
    Mock::given(method("HEAD"))
        .and(path("/v.m4s"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-length", "4096"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v.m4s"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(payload.clone()))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("v.mp4");
    let api = api_for(&server);
    let client = api.media_client().unwrap();
    let done = download(
        &client,
        &[format!("{}/v.m4s", server.uri())],
        &out,
        0,
        None,
        &CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(done.bytes, 4096);
    assert_eq!(std::fs::read(&out).unwrap().len(), 4096);
    // .part 必须已经被改名（不残留）
    assert!(!Path::new(&format!("{}.part", out.display())).exists());
}

#[tokio::test]
async fn size_mismatch_fails_without_leaving_a_file() {
    let server = MockServer::start().await;
    // HEAD 声称 4096 字节，GET 却只给 100（两个响应各自都合法，模拟 CDN 撒谎/截断）
    Mock::given(method("HEAD"))
        .and(path("/v.m4s"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-length", "4096"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v.m4s"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0u8; 100]))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("v.mp4");
    let api = api_for(&server);
    let client = api.media_client().unwrap();
    let err = download(
        &client,
        &[format!("{}/v.m4s", server.uri())],
        &out,
        0,
        None,
        &CancellationToken::new(),
    )
    .await
    .unwrap_err();

    assert!(matches!(err, Error::SizeMismatch { .. }), "{err}");
    assert!(!out.exists(), "不能留下长度不对的文件");
    assert!(
        !Path::new(&format!("{}.part", out.display())).exists(),
        ".part 也要清掉"
    );
}

#[tokio::test]
async fn falls_back_to_next_url_on_failure() {
    let server = MockServer::start().await;
    Mock::given(method("HEAD"))
        .and(path("/bad.m4s"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-length", "10"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/bad.m4s"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![1u8; 3]))
        .mount(&server)
        .await;
    Mock::given(method("HEAD"))
        .and(path("/good.m4s"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-length", "5"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/good.m4s"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"hello".to_vec()))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("v.mp4");
    let api = api_for(&server);
    let client = api.media_client().unwrap();
    let done = download(
        &client,
        &[
            format!("{}/bad.m4s", server.uri()),
            format!("{}/good.m4s", server.uri()),
        ],
        &out,
        0,
        None,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(done.bytes, 5);
    assert_eq!(std::fs::read(&out).unwrap(), b"hello");
}

/// 前两次 GET 失败（5xx），第三次成功——验证重试。
struct FlakyThenOk;

impl Respond for FlakyThenOk {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        // 用同一个 MockServer 的请求计数来模拟抖动
        static COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        if req.url.path() == "/flaky.m4s" {
            let n = COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n < 2 {
                return ResponseTemplate::new(503);
            }
            return ResponseTemplate::new(200).set_body_bytes(b"payload".to_vec());
        }
        ResponseTemplate::new(404)
    }
}

#[tokio::test]
async fn retries_transient_5xx_then_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("HEAD"))
        .and(path("/flaky.m4s"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-length", "7"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/flaky.m4s"))
        .respond_with(FlakyThenOk)
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("v.mp4");
    let api = api_for(&server);
    let client = api.media_client().unwrap();
    let done = download(
        &client,
        &[format!("{}/flaky.m4s", server.uri())],
        &out,
        0,
        None,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(done.bytes, 7);
}

#[tokio::test]
async fn does_not_retry_4xx() {
    let server = MockServer::start().await;
    Mock::given(method("HEAD"))
        .and(path("/nope.m4s"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-length", "7"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/nope.m4s"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("v.mp4");
    let api = api_for(&server);
    let client = api.media_client().unwrap();
    let started = std::time::Instant::now();
    let err = download(
        &client,
        &[format!("{}/nope.m4s", server.uri())],
        &out,
        0,
        None,
        &CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, Error::Http { status: 403, .. }), "{err}");
    // 4xx 不重试，所以不会花掉退避时间（1+2+4 秒）
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn head_failure_405_falls_back_to_plain_get() {
    let server = MockServer::start().await;
    // 一些 CDN 不支持 HEAD
    Mock::given(method("HEAD"))
        .and(path("/v.m4s"))
        .respond_with(ResponseTemplate::new(405))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v.m4s"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-length", "11")
                .set_body_bytes(b"hello world".to_vec()),
        )
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("v.mp4");
    let api = api_for(&server);
    let client = api.media_client().unwrap();
    let done = download(
        &client,
        &[format!("{}/v.m4s", server.uri())],
        &out,
        0,
        None,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(done.bytes, 11);
}

#[tokio::test]
async fn media_requests_do_not_follow_redirects() {
    let server = MockServer::start().await;
    Mock::given(method("HEAD"))
        .and(path("/v.m4s"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-length", "100"))
        .mount(&server)
        .await;
    // 媒体请求带着 Cookie，3xx 不能跟随到任意主机
    Mock::given(method("GET"))
        .and(path("/v.m4s"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", "http://169.254.169.254/latest"),
        )
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("v.mp4");
    let api = common::api_for_with_cookie(&server, "SESSDATA=secret");
    let client = api.media_client().unwrap();
    let err = download(
        &client,
        &[format!("{}/v.m4s", server.uri())],
        &out,
        0,
        None,
        &CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, Error::Http { status: 302, .. }), "{err}");
    assert!(!out.exists());
}

#[tokio::test]
async fn media_requests_carry_referer_ua_and_cookie() {
    let server = MockServer::start().await;
    Mock::given(method("HEAD"))
        .and(path("/v.m4s"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-length", "2"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v.m4s"))
        .and(header("referer", "https://www.bilibili.com/"))
        .and(header("user-agent", TEST_UA))
        .and(header("cookie", "SESSDATA=secret"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"ok".to_vec()))
        .mount(&server)
        .await;

    let api = common::api_for_with_cookie(&server, "SESSDATA=secret");
    let client = api.media_client().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("v.mp4");
    let done = download(
        &client,
        &[format!("{}/v.m4s", server.uri())],
        &out,
        0,
        None,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    // 缺少 Referer/UA/Cookie 时 mock 不命中，这里就拿不到 2 字节
    assert_eq!(done.bytes, 2);
}

fn out_exists(path: &std::path::Path) -> bool {
    path.exists()
}

#[tokio::test]
async fn cancellation_aborts_download() {
    let server = MockServer::start().await;
    Mock::given(method("HEAD"))
        .and(path("/slow.m4s"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-length", "1000"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/slow.m4s"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-length", "1000")
                .set_body_bytes(vec![0u8; 1000])
                // 延迟远大于取消时刻，避免在负载高的机器上"下载先跑完"的竞态
                .set_delay(Duration::from_secs(30)),
        )
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("v.mp4");
    let api = api_for(&server);
    let client = api.media_client().unwrap();
    let cancel = CancellationToken::new();
    let handle = {
        let cancel = cancel.clone();
        let url = format!("{}/slow.m4s", server.uri());
        let out = out.clone();
        tokio::spawn(async move { download(&client, &[url], &out, 0, None, &cancel).await })
    };
    // 等客户端真正进入"收数据"阶段再取消：mock 的延迟是 3s，这里 1s 足够，
    // 但在慢机器上也不至于早于连接建立。
    tokio::time::sleep(Duration::from_millis(1000)).await;
    cancel.cancel();
    let res = tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("取消后应尽快结束")
        .unwrap();
    let err = match res {
        Err(e) => e,
        Ok(done) => panic!("取消后不应产出文件，实际完成 {:?}", done.path),
    };
    assert!(matches!(err, Error::Interrupted), "{err:?}");
    assert_eq!(err.exit_code(), 130);
    assert!(!out_exists(&out), "取消后不能留下成品文件");
}

#[tokio::test]
async fn empty_url_list_is_an_error_not_a_panic() {
    let api = api_for(&MockServer::start().await);
    let client = api.media_client().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let err = download(
        &client,
        &[],
        &dir.path().join("v.mp4"),
        0,
        None,
        &CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, Error::Network(_)), "{err}");
}

#[tokio::test]
async fn range_header_is_not_sent() {
    // MVP 不做断点续传：请求里不应该出现 Range
    let server = MockServer::start().await;
    Mock::given(method("HEAD"))
        .and(path("/v.m4s"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-length", "2"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v.m4s"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"hi".to_vec()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v.m4s"))
        .and(query_param("x", "1"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let api = api_for(&server);
    let client = api.media_client().unwrap();
    let dir = tempfile::tempdir().unwrap();
    download(
        &client,
        &[format!("{}/v.m4s", server.uri())],
        &dir.path().join("v.mp4"),
        0,
        None,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    for req in server.received_requests().await.unwrap() {
        assert!(req.headers.get("range").is_none(), "{:?}", req.headers);
    }
}
