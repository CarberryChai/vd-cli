//! 契约测试：合集 / 系列列表与翻页边界。

mod common;

use common::{api_for, fixture_json, fixture_raw, init_tracing};
use vd_cli::api::list::parse_list_page;
use vd_cli::error::Error;
use vd_cli::resolve::ListKind;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn json_response(body: String) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "application/json")
        .set_body_string(body)
}

#[test]
fn page_parsing_uses_last_item_id_as_cursor_and_skips_invalid() {
    init_tracing();
    let resp = fixture_json("medialist_page1.json");
    let page = parse_list_page(&resp).unwrap();

    // 第一条与第三条有效；第二条 attr != 0 被跳过
    assert_eq!(page.pages.len(), 3, "{:?}", page.pages);
    let titles: Vec<&str> = page.pages.iter().map(|p| p.title.as_str()).collect();
    assert!(titles.contains(&"合集第一集"), "{titles:?}");
    assert!(titles.contains(&"合集第二集_P1_第二集"), "{titles:?}");
    assert!(titles.contains(&"合集第二集_P2_第二集下"), "{titles:?}");
    assert!(
        !titles.iter().any(|t| t.contains("已失效")),
        "attr != 0 的条目必须跳过: {titles:?}"
    );

    // 游标必须取本页**最后一条**的 id，哪怕它被 attr 跳过了
    assert_eq!(page.cursor, "2003");
    assert!(page.has_more);

    // 单 P 用视频标题，多 P 拼上分 P 信息
    let single = page.pages.iter().find(|p| p.aid == 2001).unwrap();
    assert_eq!(single.title, "合集第一集");
    assert_eq!(single.index, 1);
    assert_eq!(single.cid, 9001);
    assert_eq!(single.upper.as_deref(), Some("合集UP"));
}

#[test]
fn cursor_comes_from_last_item_even_when_whole_page_is_invalid() {
    init_tracing();
    let resp = fixture_json("medialist_stuck.json");
    let page = parse_list_page(&resp).unwrap();
    assert!(page.pages.is_empty(), "整页失效");
    // 游标仍然前进到 3002，否则会无限循环请求同一页
    assert_eq!(page.cursor, "3002");
    assert!(page.has_more);
}

#[test]
fn empty_page_has_no_cursor() {
    let resp = fixture_json("medialist_empty.json");
    let page = parse_list_page(&resp).unwrap();
    assert!(page.pages.is_empty());
    assert!(page.cursor.is_empty());
    assert!(page.has_more, "接口仍说 has_more，兜底逻辑必须拦住");
}

#[tokio::test]
async fn list_walks_two_pages_and_stops() {
    init_tracing();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/v1/medialist/info"))
        .and(query_param("type", "8"))
        .and(query_param("biz_id", "2045"))
        .and(query_param("tid", "0"))
        .respond_with(json_response(fixture_raw("medialist_info.json")))
        .mount(&server)
        .await;

    // 第 1 页：oid 为空
    Mock::given(method("GET"))
        .and(path("/x/v2/medialist/resource/list"))
        .and(query_param("type", "8"))
        .and(query_param("biz_id", "2045"))
        .and(query_param("otype", "2"))
        .and(query_param("with_current", "true"))
        .and(query_param("mobi_app", "web"))
        .and(query_param("ps", "20"))
        .and(query_param("direction", "false"))
        .and(query_param("sort_field", "1"))
        .and(query_param("tid", "0"))
        .and(query_param("oid", ""))
        .respond_with(json_response(fixture_raw("medialist_page1.json")))
        .mount(&server)
        .await;
    // 第 2 页：oid 必须是上一页最后一条的 id
    Mock::given(method("GET"))
        .and(path("/x/v2/medialist/resource/list"))
        .and(query_param("oid", "2003"))
        .respond_with(json_response(fixture_raw("medialist_page2.json")))
        .mount(&server)
        .await;

    let api = api_for(&server);
    let info = api.list(2045, ListKind::Collection).await.unwrap();
    assert_eq!(info.title, "测试合集");
    assert_eq!(info.biz_id, 2045);
    // 3（第 1 页有效分 P）+ 1（第 2 页）= 4
    assert_eq!(info.pages.len(), 4, "{:?}", info.pages);
    let aids: Vec<u64> = info.pages.iter().map(|p| p.aid).collect();
    assert_eq!(aids, vec![2001, 2003, 2003, 2004]);

    // 恰好两次请求，没有多翻一页也没有死循环
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 3, "1 次 info + 2 次 list");
}

#[tokio::test]
async fn list_stops_when_cursor_does_not_advance() {
    init_tracing();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/v1/medialist/info"))
        .respond_with(json_response(fixture_raw("medialist_info.json")))
        .mount(&server)
        .await;

    // 两页都被 attr 跳过：游标会前进，所以走完两页后 has_more 仍由 fixture 决定
    Mock::given(method("GET"))
        .and(path("/x/v2/medialist/resource/list"))
        .and(query_param("oid", ""))
        .respond_with(json_response(fixture_raw("medialist_stuck.json")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/x/v2/medialist/resource/list"))
        .and(query_param("oid", "3002"))
        .respond_with(json_response(fixture_raw("medialist_empty.json")))
        .mount(&server)
        .await;

    let api = api_for(&server);
    // 没有可下载的条目 → 明确报错，而不是返回空列表或死循环
    let res = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        api.list_pages(2045, ListKind::Collection),
    )
    .await
    .expect("必须尽快结束，不能死循环");
    let err = res.unwrap_err();
    assert!(err.to_string().contains("没有可下载的视频"), "{err}");

    // 空页 + has_more 时停住：最多 3 次 list 请求（空 oid → 3002 → 再空）
    let list_requests = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path().ends_with("/resource/list"))
        .count();
    assert!(list_requests <= 3, "翻页次数应当有界，实际 {list_requests}");
}

#[tokio::test]
async fn list_stops_when_api_repeats_same_page() {
    init_tracing();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/v1/medialist/info"))
        .respond_with(json_response(fixture_raw("medialist_info.json")))
        .mount(&server)
        .await;
    // 无论 oid 是什么都返回同一页（且 has_more = true）——最典型的死循环诱因
    Mock::given(method("GET"))
        .and(path("/x/v2/medialist/resource/list"))
        .respond_with(json_response(fixture_raw("medialist_page1.json")))
        .mount(&server)
        .await;

    let api = api_for(&server);
    let pages = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        api.list_pages(2045, ListKind::Collection),
    )
    .await
    .expect("必须靠游标兜底结束，不能死循环")
    .unwrap();
    // 去重后 3 个分 P（第二条 attr != 0 不算）
    assert_eq!(pages.len(), 3, "{pages:?}");
}

#[tokio::test]
async fn series_uses_type_5() {
    init_tracing();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/v1/medialist/info"))
        .and(query_param("type", "5"))
        .respond_with(json_response(fixture_raw("medialist_info.json")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/x/v2/medialist/resource/list"))
        .and(query_param("type", "5"))
        .respond_with(json_response(fixture_raw("medialist_page2.json")))
        .mount(&server)
        .await;

    let api = api_for(&server);
    assert_eq!(api.list_pages(1, ListKind::Series).await.unwrap().len(), 1);
    assert_eq!(ListKind::Series.api_type(), 5);
    assert_eq!(ListKind::Collection.api_type(), 8);
    assert_eq!(ListKind::Series.as_str(), "系列");
}

#[tokio::test]
async fn list_error_code_is_surfaced() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/v1/medialist/info"))
        .respond_with(json_response(
            serde_json::json!({"code": -404, "message": "合集不存在"}).to_string(),
        ))
        .mount(&server)
        .await;
    let api = api_for(&server);
    let err = api.list(1, ListKind::Collection).await.unwrap_err();
    assert!(matches!(err, Error::NotFound), "{err}");
}
