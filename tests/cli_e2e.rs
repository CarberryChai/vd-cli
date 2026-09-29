//! 端到端：跑真正的 `vd` 二进制，对着 mock 服务器走完整条链路。

mod common;

use std::process::Command;

use common::{fixture_raw, init_tracing};
use serde_json::Value;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn json_response(body: String) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "application/json")
        .set_body_string(body)
}

struct Output {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// 跑 `vd`，把 API 基址指向 mock 服务器。
fn run_vd(server: &MockServer, args: &[&str]) -> Output {
    let out = Command::new(env!("CARGO_BIN_EXE_vd"))
        .args(args)
        .env("VD_API_BASE", server.uri())
        .env("VD_WEB_BASE", server.uri())
        // 不继承调用者的 RUST_LOG，否则日志级别会影响断言
        .env("RUST_LOG", "warn")
        .output()
        .expect("运行 vd 二进制");
    Output {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
    }
}

/// 挂上单 P 视频 + nav + playurl 三个 mock。
async fn mount_single_video(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/x/web-interface/view"))
        .respond_with(json_response(fixture_raw("view_single.json")))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/nav"))
        .respond_with(json_response(fixture_raw("nav_anonymous.json")))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/x/player/wbi/playurl"))
        .respond_with(json_response(fixture_raw("playurl_dash.json")))
        .mount(server)
        .await;
}

#[tokio::test]
async fn dry_run_json_reports_resolved_page() {
    init_tracing();
    let server = MockServer::start().await;
    mount_single_video(&server).await;

    let out = run_vd(&server, &["--dry-run", "--json", "av2"]);
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    let parsed: Value = serde_json::from_str(out.stdout.trim()).expect("stdout 应当是 JSON");
    assert_eq!(parsed["total"], 1);
    assert_eq!(parsed["succeeded"], 1);
    assert_eq!(parsed["failed"], 0);
    assert_eq!(parsed["items"][0]["title"], "字幕君交流场所");
    assert_eq!(parsed["items"][0]["cid"], 1001);
    // --dry-run 不下载，输出里不应有 size
    assert!(parsed["items"][0].get("size").is_none());
}

#[tokio::test]
async fn dry_run_honours_quality_and_codec() {
    init_tracing();
    let server = MockServer::start().await;
    mount_single_video(&server).await;

    let out = run_vd(&server, &["--dry-run", "--json", "-q", "720p", "av2"]);
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    let parsed: Value = serde_json::from_str(out.stdout.trim()).unwrap();
    // 720p 上限（权重 35）→ 720P 高清
    assert_eq!(parsed["items"][0]["quality"], "720P 高清");

    let out = run_vd(&server, &["--dry-run", "--json", "--codec", "hevc", "av2"]);
    let parsed: Value = serde_json::from_str(out.stdout.trim()).unwrap();
    assert_eq!(parsed["items"][0]["codec"], "HEVC");

    // 480p 上限（权重 20）：这个源里没有 480P 档位，降级到可用的最高档位 360P
    let out = run_vd(&server, &["--dry-run", "--json", "-q", "480p", "av2"]);
    let parsed: Value = serde_json::from_str(out.stdout.trim()).unwrap();
    assert_eq!(parsed["items"][0]["quality"], "360P 流畅");
    // 是降级而不是报错
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
}

#[tokio::test]
async fn bad_pages_spec_exits_with_2() {
    let server = MockServer::start().await;
    mount_single_video(&server).await;
    let out = run_vd(&server, &["--dry-run", "-p", "0", "av2"]);
    assert_eq!(
        out.code,
        Some(2),
        "参数错误应当是退出码 2；stderr: {}",
        out.stderr
    );
    assert!(out.stderr.contains("1 开始"), "{}", out.stderr);
}

#[tokio::test]
async fn unrecognized_input_exits_with_2() {
    let server = MockServer::start().await;
    let out = run_vd(&server, &["--dry-run", "not-a-video"]);
    assert_eq!(out.code, Some(2), "stderr: {}", out.stderr);
    assert!(out.stderr.contains("输入无法识别"), "{}", out.stderr);
}

#[tokio::test]
async fn risk_control_exits_with_3() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/view"))
        .respond_with(json_response(fixture_raw("view_404.json")))
        .mount(&server)
        .await;
    let out = run_vd(&server, &["--dry-run", "av2"]);
    // 视频不存在 → 退出码 1
    assert_eq!(out.code, Some(1), "stderr: {}", out.stderr);

    let server2 = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/view"))
        .respond_with(json_response(fixture_raw("view_single.json")))
        .mount(&server2)
        .await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/nav"))
        .respond_with(json_response(fixture_raw("nav_anonymous.json")))
        .mount(&server2)
        .await;
    Mock::given(method("GET"))
        .and(path("/x/player/wbi/playurl"))
        .respond_with(json_response(fixture_raw("playurl_risk.json")))
        .mount(&server2)
        .await;
    let out = run_vd(&server2, &["--dry-run", "av2"]);
    assert_eq!(
        out.code,
        Some(3),
        "风控应当是退出码 3；stderr: {}",
        out.stderr
    );
    assert!(out.stderr.contains("稍后重试"), "{}", out.stderr);
}

#[tokio::test]
async fn need_login_exits_with_4() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/view"))
        .respond_with(json_response(fixture_raw("view_single.json")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/nav"))
        .respond_with(json_response(fixture_raw("nav_anonymous.json")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/x/player/wbi/playurl"))
        .respond_with(json_response(fixture_raw("playurl_need_login.json")))
        .mount(&server)
        .await;

    let out = run_vd(&server, &["--dry-run", "av2"]);
    assert_eq!(out.code, Some(4), "stderr: {}", out.stderr);
    // 必须告诉用户下一步：加 --cookie
    assert!(out.stderr.contains("--cookie"), "{}", out.stderr);
}

#[tokio::test]
async fn collection_download_dry_run_lists_every_item() {
    init_tracing();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x/v1/medialist/info"))
        .respond_with(json_response(fixture_raw("medialist_info.json")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/x/v2/medialist/resource/list"))
        .and(query_param("oid", ""))
        .respond_with(json_response(fixture_raw("medialist_page1.json")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/x/v2/medialist/resource/list"))
        .and(query_param("oid", "2003"))
        .respond_with(json_response(fixture_raw("medialist_page2.json")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/nav"))
        .respond_with(json_response(fixture_raw("nav_anonymous.json")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/x/player/wbi/playurl"))
        .respond_with(json_response(fixture_raw("playurl_dash.json")))
        .mount(&server)
        .await;

    let out = run_vd(
        &server,
        &[
            "--dry-run",
            "--json",
            "https://space.bilibili.com/1/channel/collectiondetail?sid=2045",
        ],
    );
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    let parsed: Value = serde_json::from_str(out.stdout.trim()).unwrap();
    assert_eq!(parsed["container"], "测试合集");
    assert_eq!(parsed["total"], 4);
    assert_eq!(parsed["items"].as_array().unwrap().len(), 4);
    // 合集里的输出路径带目录与序号
    let first = parsed["items"][0]["output"].as_str().unwrap();
    assert!(first.contains("测试合集"), "{first}");
    assert!(first.contains("[1]"), "{first}");
}

#[tokio::test]
async fn no_mux_skips_the_ffmpeg_probe() {
    init_tracing();
    let server = MockServer::start().await;
    mount_single_video(&server).await;
    // --no-mux 且 --dry-run：完全不碰 ffmpeg，也不下载
    let out = run_vd(&server, &["--no-mux", "--dry-run", "--json", "av2"]);
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
}

/// 造一小段真实的 mp4 / m4a，用来验证「下载 → ffmpeg 混流 → 文件名落盘」这条链路。
fn make_sample(ffmpeg: &str, args: &[&str], out: &std::path::Path) {
    let status = Command::new(ffmpeg)
        .args(args)
        .arg("-y")
        .arg(out)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("运行 ffmpeg 造样本");
    assert!(status.success(), "造样本失败: {}", out.display());
}

fn have(program: &str) -> bool {
    Command::new(program)
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[tokio::test]
async fn downloads_and_muxes_a_real_file() {
    init_tracing();
    if !have("ffmpeg") || !have("ffprobe") {
        eprintln!("跳过：需要 ffmpeg 与 ffprobe");
        return;
    }
    let samples = tempfile::tempdir().unwrap();
    let video_src = samples.path().join("v.mp4");
    let audio_src = samples.path().join("a.m4a");
    make_sample(
        "ffmpeg",
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=160x120:rate=10:duration=1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ],
        &video_src,
    );
    make_sample(
        "ffmpeg",
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=1",
            "-c:a",
            "aac",
        ],
        &audio_src,
    );
    let video_bytes = std::fs::read(&video_src).unwrap();
    let audio_bytes = std::fs::read(&audio_src).unwrap();

    let server = MockServer::start().await;
    let cdn = server.uri();

    // 视频轨 / 音频轨直接指向 mock CDN
    let playurl = serde_json::json!({
        "code": 0, "message": "OK",
        "data": {"dash": {"duration": 1,
            "video": [{"id": 80, "base_url": format!("{cdn}/v.m4s"),
                       "backup_url": [format!("{cdn}/v-backup.m4s")],
                       "bandwidth": 500_000, "codecid": 7, "width": 160, "height": 120,
                       "frame_rate": "10", "size": video_bytes.len()}],
            "audio": [{"id": 30280, "base_url": format!("{cdn}/a.m4s"),
                       "bandwidth": 64_000, "codecs": "mp4a.40.2"}]}}
    });
    Mock::given(method("GET"))
        .and(path("/v.m4s"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(video_bytes.clone()))
        .mount(&server)
        .await;
    Mock::given(method("HEAD"))
        .and(path("/v.m4s"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-length", video_bytes.len().to_string()),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/a.m4s"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(audio_bytes.clone()))
        .mount(&server)
        .await;
    Mock::given(method("HEAD"))
        .and(path("/a.m4s"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-length", audio_bytes.len().to_string()),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/view"))
        .respond_with(json_response(fixture_raw("view_single.json")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/nav"))
        .respond_with(json_response(fixture_raw("nav_anonymous.json")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/x/player/wbi/playurl"))
        .respond_with(json_response(playurl.to_string()))
        .mount(&server)
        .await;

    let out_dir = tempfile::tempdir().unwrap();
    let out = run_vd(
        &server,
        &[
            "--json",
            "-q",
            "1080p",
            "-o",
            out_dir.path().to_str().unwrap(),
            "av2",
        ],
    );
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    let parsed: Value = serde_json::from_str(out.stdout.trim()).expect("--json 输出必须可解析");
    assert_eq!(parsed["succeeded"], 1);

    // 文件名来自视频标题，落在指定目录里
    let mp4 = std::fs::read_dir(out_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "mp4"))
        .expect("应当产出 mp4");
    assert_eq!(
        mp4.file_name().unwrap().to_string_lossy(),
        "字幕君交流场所.mp4"
    );
    // 混流后的文件同时有视频轨与音频轨
    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type",
            "-of",
            "csv=p=0",
        ])
        .arg(&mp4)
        .output()
        .expect("运行 ffprobe");
    let streams = String::from_utf8_lossy(&probe.stdout);
    assert!(streams.contains("video"), "{streams}");
    assert!(streams.contains("audio"), "{streams}");

    // 中间文件必须清干净
    let leftovers: Vec<String> = std::fs::read_dir(out_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.contains(".video.") || n.contains(".audio.") || n.ends_with(".part"))
        .collect();
    assert!(leftovers.is_empty(), "残留中间文件: {leftovers:?}");
}

#[tokio::test]
async fn no_mux_keeps_separate_tracks() {
    init_tracing();
    if !have("ffmpeg") {
        eprintln!("跳过：需要 ffmpeg");
        return;
    }
    let samples = tempfile::tempdir().unwrap();
    let video_src = samples.path().join("v.mp4");
    let audio_src = samples.path().join("a.m4a");
    make_sample(
        "ffmpeg",
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=160x120:rate=10:duration=1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ],
        &video_src,
    );
    make_sample(
        "ffmpeg",
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=1",
            "-c:a",
            "aac",
        ],
        &audio_src,
    );
    let video_bytes = std::fs::read(&video_src).unwrap();
    let audio_bytes = std::fs::read(&audio_src).unwrap();

    let server = MockServer::start().await;
    let cdn = server.uri();
    let playurl = serde_json::json!({
        "code": 0, "message": "OK",
        "data": {"dash": {"duration": 1,
            "video": [{"id": 80, "base_url": format!("{cdn}/v.m4s"), "bandwidth": 500_000,
                       "codecid": 7, "width": 160, "height": 120, "size": video_bytes.len()}],
            "audio": [{"id": 30280, "base_url": format!("{cdn}/a.m4s"),
                       "bandwidth": 64_000, "codecs": "mp4a.40.2"}]}}
    });
    Mock::given(method("GET"))
        .and(path("/v.m4s"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(video_bytes.clone()))
        .mount(&server)
        .await;
    Mock::given(method("HEAD"))
        .and(path("/v.m4s"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-length", video_bytes.len().to_string()),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/a.m4s"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(audio_bytes.clone()))
        .mount(&server)
        .await;
    Mock::given(method("HEAD"))
        .and(path("/a.m4s"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-length", audio_bytes.len().to_string()),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/view"))
        .respond_with(json_response(fixture_raw("view_single.json")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/nav"))
        .respond_with(json_response(fixture_raw("nav_anonymous.json")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/x/player/wbi/playurl"))
        .respond_with(json_response(playurl.to_string()))
        .mount(&server)
        .await;

    let out_dir = tempfile::tempdir().unwrap();
    let out = run_vd(
        &server,
        &[
            "--no-mux",
            "-q",
            "1080p",
            "-o",
            out_dir.path().to_str().unwrap(),
            "av2",
        ],
    );
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    let mut names: Vec<String> = std::fs::read_dir(out_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "字幕君交流场所.m4a".to_string(),
            "字幕君交流场所.mp4".to_string()
        ],
        "--no-mux 应当保留分离的 .mp4 与 .m4a"
    );
}

/// 匹配「Cookie 头里包含某段子串」。
///
/// 不能用 wiremock 的 `header(k, v)`：那是全等匹配，而客户端还会补上 buvid3。
struct CookieContains(&'static str);

impl wiremock::Match for CookieContains {
    fn matches(&self, request: &wiremock::Request) -> bool {
        request
            .headers
            .get("cookie")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains(self.0))
    }
}

/// 造一个假 Chrome profile，里面放明文 Cookie（明文不需要钥匙串，测试才能脱离授权跑）。
fn fake_chrome_home(cookies: &[(&str, &str)]) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let profile = home
        .path()
        .join("Library/Application Support/Google/Chrome/Default");
    std::fs::create_dir_all(&profile).unwrap();
    let db = profile.join("Cookies");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE cookies (host_key TEXT, name TEXT, value TEXT, encrypted_value BLOB);",
    )
    .unwrap();
    for (name, value) in cookies {
        conn.execute(
            "INSERT INTO cookies VALUES ('.bilibili.com', ?1, ?2, x'')",
            rusqlite::params![name, value],
        )
        .unwrap();
    }
    drop(conn);
    home
}

/// 跑 vd，并覆盖 HOME（让浏览器探测指向我们的假 profile）。
fn run_vd_with_home(server: &MockServer, home: &std::path::Path, args: &[&str]) -> Output {
    run_vd_with_home_log(server, home, args, None)
}

/// 同上，并可指定日志级别。
///
/// 必须显式设置 `RUST_LOG`：外层环境里的值会被子进程继承，测试不能依赖调用者的环境。
fn run_vd_with_home_log(
    server: &MockServer,
    home: &std::path::Path,
    args: &[&str],
    log: Option<&str>,
) -> Output {
    let out = Command::new(env!("CARGO_BIN_EXE_vd"))
        .args(args)
        .env("VD_API_BASE", server.uri())
        .env("VD_WEB_BASE", server.uri())
        .env("HOME", home)
        .env("RUST_LOG", log.unwrap_or("warn"))
        .output()
        .expect("运行 vd");
    Output {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
    }
}

#[tokio::test]
async fn default_mode_reads_cookies_from_the_browser() {
    init_tracing();
    let server = MockServer::start().await;
    // 只有带上浏览器里的 Cookie，view 才会命中这个 mock
    Mock::given(method("GET"))
        .and(path("/x/web-interface/view"))
        .and(CookieContains("SESSDATA=from-browser; bili_jct=jct-value"))
        .respond_with(json_response(fixture_raw("view_single.json")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/nav"))
        .respond_with(json_response(fixture_raw("nav_anonymous.json")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/x/player/wbi/playurl"))
        .respond_with(json_response(fixture_raw("playurl_dash.json")))
        .mount(&server)
        .await;

    let home = fake_chrome_home(&[("SESSDATA", "from-browser"), ("bili_jct", "jct-value")]);

    // 默认（不传 --cookies-from-browser）就该读到浏览器的 Cookie
    let out = run_vd_with_home(&server, home.path(), &["--dry-run", "--json", "av2"]);
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    let parsed: Value = serde_json::from_str(out.stdout.trim()).unwrap();
    assert_eq!(parsed["succeeded"], 1);

    // 并且 -v 时日志里说清了来源。
    // 注意 RUST_LOG 要同时覆盖二进制（vd）与库（vd_cli）：main.rs 里的日志属于前者。
    let out = run_vd_with_home_log(
        &server,
        home.path(),
        &["--dry-run", "av2"],
        Some("vd=debug,vd_cli=debug"),
    );
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("Cookie 来源") && out.stderr.contains("自动选择"),
        "应当说明 Cookie 来源: {}",
        out.stderr
    );
}

#[tokio::test]
async fn no_cookies_flag_omits_the_cookie_header() {
    init_tracing();
    let server = MockServer::start().await;
    // 这个 mock 只在**没有** Cookie 时命中
    Mock::given(method("GET"))
        .and(path("/x/web-interface/view"))
        .respond_with(json_response(fixture_raw("view_single.json")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/x/web-interface/nav"))
        .respond_with(json_response(fixture_raw("nav_anonymous.json")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/x/player/wbi/playurl"))
        .respond_with(json_response(fixture_raw("playurl_dash.json")))
        .mount(&server)
        .await;

    let home = fake_chrome_home(&[("SESSDATA", "from-browser")]);
    let out = run_vd_with_home(
        &server,
        home.path(),
        &["--no-cookies-from-browser", "--dry-run", "av2"],
    );
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);

    // view 请求里不该出现 SESSDATA
    let view = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.url.path() == "/x/web-interface/view")
        .expect("应当请求过 view");
    let cookie = view
        .headers
        .get("cookie")
        .map(|v| v.to_str().unwrap_or(""))
        .unwrap_or("");
    assert!(
        !cookie.contains("SESSDATA"),
        "--no-cookies-from-browser 却带了登录 Cookie: {cookie}"
    );
}

#[tokio::test]
async fn explicit_browser_that_cannot_be_read_is_a_hard_error() {
    init_tracing();
    let server = MockServer::start().await;
    let home = tempfile::tempdir().unwrap(); // 空 HOME：没有任何浏览器
    // 显式点名了浏览器，读不到就该报错，而不是静默降级
    let out = run_vd_with_home(
        &server,
        home.path(),
        &["--cookies-from-browser", "chrome", "--dry-run", "av2"],
    );
    assert_eq!(out.code, Some(1), "stderr: {}", out.stderr);
    assert!(out.stderr.contains("chrome"), "{}", out.stderr);
}

#[tokio::test]
async fn auto_mode_degrades_gracefully_when_nothing_is_readable() {
    init_tracing();
    let server = MockServer::start().await;
    mount_single_video(&server).await;
    let home = tempfile::tempdir().unwrap(); // 空 HOME
    // 默认 auto：读不到也要能跑，只是警告 + 未登录
    let out = run_vd_with_home(&server, home.path(), &["--dry-run", "--json", "av2"]);
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("未登录") || out.stderr.contains("浏览器"),
        "应当提示已降级: {}",
        out.stderr
    );
}
