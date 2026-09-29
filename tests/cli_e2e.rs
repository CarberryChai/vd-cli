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
