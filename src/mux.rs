//! ffmpeg 混流。
//!
//! 只做 `-c copy`（下载器不是转码器），`-movflags +faststart` 让 moov 前置。

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

use crate::error::{Error, Result, install_hint};

/// ffmpeg 整体超时。损坏的输入会让它挂死。
const FFMPEG_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// 元数据值长度上限。
const META_MAX_CHARS: usize = 200;

/// 启动时探测 ffmpeg。缺失则给出平台相关的安装提示。
pub async fn ensure_ffmpeg() -> Result<()> {
    let program = if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    match Command::new(program)
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .status()
        .await
    {
        Ok(status) if status.success() => Ok(()),
        _ => {
            tracing::debug!("{}", install_hint());
            Err(Error::FfmpegMissing)
        }
    }
}

/// 清洗要写进 `-metadata` 的值。
///
/// 标题里含 `\n` 可以注入伪造的元数据行，必须先替换掉再截断长度。
pub fn clean_metadata(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|c| {
            if c == '\n' || c == '\r' || c.is_control() {
                ' '
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned.trim();
    let truncated: String = trimmed.chars().take(META_MAX_CHARS).collect();
    truncated
}

/// 混流参数（除了文件名，便于测试断言）。
pub fn ffmpeg_args(
    video: &Path,
    audio: &Path,
    output: &Path,
    title: &str,
    artist: Option<&str>,
) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-i".into(),
        video.display().to_string(),
        "-i".into(),
        audio.display().to_string(),
        "-map".into(),
        "0:v:0".into(),
        "-map".into(),
        "1:a:0".into(),
        "-c".into(),
        "copy".into(),
        "-movflags".into(),
        "+faststart".into(),
        "-metadata".into(),
        format!("title={}", clean_metadata(title)),
    ];
    if let Some(artist) = artist {
        let artist = clean_metadata(artist);
        if !artist.is_empty() {
            args.push("-metadata".into());
            args.push(format!("artist={artist}"));
        }
    }
    args.push("-y".into());
    args.push(output.display().to_string());
    args
}

/// 执行混流。
pub async fn mux(
    video: &Path,
    audio: &Path,
    output: &Path,
    title: &str,
    artist: Option<&str>,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<()> {
    if let Some(parent) = output.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let args = ffmpeg_args(video, audio, output, title, artist);
    let mut child = Command::new("ffmpeg")
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| Error::FfmpegMissing)?;

    let stderr = child.stderr.take();
    let collect_stderr = async {
        let mut buf = String::new();
        if let Some(mut stderr) = stderr {
            use tokio::io::AsyncReadExt;
            let _ = stderr.read_to_string(&mut buf).await;
        }
        buf
    };

    let outcome = tokio::select! {
        _ = cancel.cancelled() => {
            let _ = child.kill().await;
            return Err(Error::Interrupted);
        }
        _ = tokio::time::sleep(FFMPEG_TIMEOUT) => {
            let _ = child.kill().await;
            return Err(Error::FfmpegTimeout);
        }
        status = async {
            let status = child.wait().await;
            let stderr = collect_stderr.await;
            (status, stderr)
        } => status,
    };

    let (status, stderr) = outcome;
    let status = status.map_err(Error::Io)?;
    if !status.success() {
        let tail: String = stderr.trim().chars().take(500).collect();
        return Err(Error::FfmpegFailed(if tail.is_empty() {
            format!("退出码 {:?}", status.code())
        } else {
            tail
        }));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn args_to_string(args: &[String]) -> String {
        args.join(" ")
    }

    #[test]
    fn metadata_newline_is_neutralized() {
        assert_eq!(clean_metadata("a\nb"), "a b");
        assert_eq!(clean_metadata("a\r\nb"), "a  b");
        assert_eq!(clean_metadata("a\u{0}b"), "a b");
        assert!(!clean_metadata("evil\ntitle=injected").contains('\n'));
    }

    #[test]
    fn metadata_is_truncated() {
        let long = "字".repeat(1000);
        let out = clean_metadata(&long);
        assert_eq!(out.chars().count(), META_MAX_CHARS);
    }

    #[test]
    fn args_use_copy_and_faststart() {
        let args = ffmpeg_args(
            &PathBuf::from("/tmp/a.mp4"),
            &PathBuf::from("/tmp/a.m4a"),
            &PathBuf::from("/out/t.mp4"),
            "标题",
            Some("UP主"),
        );
        let line = args_to_string(&args);
        assert!(line.contains("-c copy"), "{line}");
        assert!(line.contains("-movflags +faststart"), "{line}");
        assert!(line.contains("-map 0:v:0 -map 1:a:0"), "{line}");
        assert!(line.contains("-metadata title=标题"), "{line}");
        assert!(line.contains("-metadata artist=UP主"), "{line}");
        // 输出必须是最后一个参数
        assert_eq!(args.last().unwrap(), "/out/t.mp4");
    }

    #[test]
    fn args_omit_empty_artist() {
        let args = ffmpeg_args(
            &PathBuf::from("/tmp/a.mp4"),
            &PathBuf::from("/tmp/a.m4a"),
            &PathBuf::from("/out/t.mp4"),
            "标题",
            Some(""),
        );
        assert!(!args_to_string(&args).contains("artist="));
        let args = ffmpeg_args(
            &PathBuf::from("/tmp/a.mp4"),
            &PathBuf::from("/tmp/a.m4a"),
            &PathBuf::from("/out/t.mp4"),
            "标题",
            None,
        );
        assert!(!args_to_string(&args).contains("artist="));
    }
}
