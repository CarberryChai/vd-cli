//! 手动端到端测试（默认 `#[ignore]`，CI 上不跑）。
//!
//! 打真实 B 站接口，需要网络、ffmpeg 与 ffprobe。本地验证用：
//!
//! ```bash
//! cargo test --test manual_e2e -- --ignored --nocapture
//! ```

use std::process::Command;

/// 一个长期稳定的公开视频（短、无版权争议）。
const VIDEO: &str = "BV1qt4y1X7TW";

fn have(program: &str) -> bool {
    Command::new(program)
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[test]
#[ignore = "打真实 B 站接口，需要网络与 ffmpeg"]
fn downloads_a_public_video_and_ffprobe_reads_it_back() {
    assert!(have("ffmpeg"), "需要 ffmpeg");
    assert!(have("ffprobe"), "需要 ffprobe");

    let dir = tempfile::tempdir().expect("临时目录");
    let out = Command::new(env!("CARGO_BIN_EXE_vd"))
        .args(["-q", "360p", "-o"])
        .arg(dir.path())
        .arg(VIDEO)
        .output()
        .expect("运行 vd");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "vd 失败（{}）\nstdout: {stdout}\nstderr: {stderr}",
        out.status.code().unwrap_or(-1)
    );

    // 找出产出的 mp4
    let mp4 = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|ext| ext == "mp4"))
        .expect("目录里应当有 .mp4");

    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_type",
            "-of",
            "csv=p=0",
        ])
        .arg(&mp4)
        .output()
        .expect("运行 ffprobe");
    assert!(
        String::from_utf8_lossy(&probe.stdout).contains("video"),
        "mp4 里应当有视频轨: {}",
        String::from_utf8_lossy(&probe.stderr)
    );

    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=codec_type",
            "-of",
            "csv=p=0",
        ])
        .arg(&mp4)
        .output()
        .expect("运行 ffprobe");
    assert!(
        String::from_utf8_lossy(&probe.stdout).contains("audio"),
        "mp4 里应当有音频轨: {}",
        String::from_utf8_lossy(&probe.stderr)
    );

    // 时长：与接口给的 226 秒对比，允许 ffmpeg 的取整误差
    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(&mp4)
        .output()
        .expect("运行 ffprobe");
    let duration: f64 = String::from_utf8_lossy(&probe.stdout)
        .trim()
        .parse()
        .expect("解析时长");
    assert!(
        (duration - 226.0).abs() < 5.0,
        "时长应当接近 226 秒，实际 {duration}"
    );
}
