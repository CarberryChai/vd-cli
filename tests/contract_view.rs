//! 契约测试：`view` 与 `nav`（mock HTTP）。

mod common;

use common::{api_for, fixture_json, fixture_raw, init_tracing};
use vd_cli::api::playurl::init_mixin_key;
use vd_cli::error::Error;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn nav_anonymous_still_yields_wbi_key() {
    init_tracing();
    // ⚠️ 未登录时 nav 返回 code: -101 但依然包含 wbi_img —— 密钥必须在判断登录状态之前提取
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/nav"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_string(fixture_raw("nav_anonymous.json")),
        )
        .mount(&server)
        .await;

    let api = api_for(&server);
    let key = init_mixin_key(&api).await.expect("未登录也要能拿到密钥");
    assert_eq!(key, "ea1db124af3c7062474693fa704f4ff8");
    assert_eq!(key.len(), 32);
}

#[tokio::test]
async fn nav_carries_wbi_img_even_with_negative_code() {
    let fixture = fixture_json("nav_anonymous.json");
    assert_eq!(fixture["code"], -101, "fixture 必须保留未登录场景");
    assert!(fixture["data"]["wbi_img"]["img_url"].is_string());
    assert!(fixture["data"]["wbi_img"]["sub_url"].is_string());
}

#[tokio::test]
async fn view_single_page_uses_video_title_as_file_name() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/view"))
        .and(query_param("aid", "2"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_string(fixture_raw("view_single.json")),
        )
        .mount(&server)
        .await;

    let api = api_for(&server);
    let info = api.view(2).await.unwrap();
    assert_eq!(info.title, "字幕君交流场所");
    assert_eq!(info.pages.len(), 1);
    assert_eq!(info.owner.as_deref(), Some("UP主名称"));
    // 单 P：pages[0].part 是 "P1" 这种占位符，文件名必须用视频标题
    assert_eq!(info.pages[0].title, "字幕君交流场所");
    assert_eq!(info.pages[0].cid, 1001);
    assert_eq!(info.pages[0].index, 1);
    assert_eq!(info.pages[0].duration, 120);
}

#[tokio::test]
async fn view_multi_page_keeps_part_titles() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/view"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_string(fixture_raw("view_video.json")),
        )
        .mount(&server)
        .await;

    let api = api_for(&server);
    let info = api.view(170_001).await.unwrap();
    assert!(info.pages.len() > 1, "fixture 应当是多 P 视频");
    assert_eq!(info.pages[0].index, 1);
    // 多 P 时用 part 作为分 P 标题（不是视频标题）
    assert_ne!(info.pages[0].title, info.title);
    assert!(!info.pages[0].title.trim().is_empty());
    // 分 P 的 aid 都是同一个 aid，cid 各不相同
    assert!(info.pages.iter().all(|p| p.aid == 170_001));
    let mut cids: Vec<u64> = info.pages.iter().map(|p| p.cid).collect();
    let before = cids.len();
    cids.sort_unstable();
    cids.dedup();
    assert_eq!(cids.len(), before, "cid 不应重复");
}

#[tokio::test]
async fn view_404_maps_to_not_found() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/view"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_string(fixture_raw("view_404.json")),
        )
        .mount(&server)
        .await;

    let api = api_for(&server);
    let err = api.view(999_999).await.unwrap_err();
    assert!(matches!(err, Error::NotFound), "{err}");
    assert_eq!(err.exit_code(), 1);
}

#[tokio::test]
async fn view_without_owner_field_is_fine() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/view"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_string(fixture_raw("view_no_owner.json")),
        )
        .mount(&server)
        .await;

    let api = api_for(&server);
    let info = api.view(2).await.unwrap();
    assert_eq!(info.owner, None);
    assert_eq!(info.pages.len(), 1);
    assert_eq!(info.pages[0].upper, None);
}

#[tokio::test]
async fn view_sends_referer_and_fixed_ua() {
    use wiremock::matchers::{header, header_exists};
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/view"))
        .and(header("referer", "https://www.bilibili.com/"))
        .and(header("user-agent", common::TEST_UA))
        .and(header_exists("accept"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_string(fixture_raw("view_single.json")),
        )
        .mount(&server)
        .await;

    let api = api_for(&server);
    // 如果缺少 Referer / UA，这个 mock 不会命中，请求会 404
    let info = api.view(2).await.unwrap();
    assert_eq!(info.pages.len(), 1);
}

#[tokio::test]
async fn view_sends_cookie_when_provided() {
    use wiremock::matchers::header;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/view"))
        .and(header("cookie", "SESSDATA=secret; bili_jct=token"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_string(fixture_raw("view_single.json")),
        )
        .mount(&server)
        .await;

    let api = common::api_for_with_cookie(&server, "SESSDATA=secret; bili_jct=token");
    assert_eq!(api.view(2).await.unwrap().pages.len(), 1);
}
