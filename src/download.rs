//! 文件下载：单连接顺序下载。
//!
//! 关闭重定向的客户端见 `api::Api::media_client`——媒体请求带着 Cookie，
//! 不能跟随 3xx 到任意主机。

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::header::{CONTENT_LENGTH, RETRY_AFTER};
use reqwest::{Client, StatusCode};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

use crate::error::{Error, Result};

/// 网络错误重试次数与退避（1s / 2s / 4s）。
const MAX_RETRIES: u32 = 3;
const BACKOFF_SECS: [u64; 3] = [1, 2, 4];

/// 一次下载的结果。
#[derive(Debug, Clone)]
pub struct Downloaded {
    pub path: PathBuf,
    pub bytes: u64,
}

/// 下载一个 URL 到 `path`（会先写 `{path}.part` 再改名）。
///
/// 输入是一组按优先级排好的候选地址：失败时按顺序换源。
pub async fn download(
    client: &Client,
    urls: &[String],
    path: &Path,
    total_bytes_hint: u64,
    progress: Option<&ProgressBar>,
    cancel: &CancellationToken,
) -> Result<Downloaded> {
    let mut last_err: Option<Error> = None;
    for (i, url) in urls.iter().enumerate() {
        if cancel.is_cancelled() {
            return Err(Error::Interrupted);
        }
        if urls.len() > 1 {
            tracing::debug!("尝试第 {} 个源: {url}", i + 1);
        }
        match download_one(client, url, path, total_bytes_hint, progress, cancel).await {
            Ok(done) => return Ok(done),
            // 用户中断 / 已取消不换源重试
            Err(e @ Error::Interrupted) => return Err(e),
            Err(e) => {
                tracing::warn!("下载失败（{url}）: {e}");
                if let Some(pb) = progress {
                    pb.abandon();
                }
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or(Error::Network("没有可用的下载地址".into())))
}

async fn download_one(
    client: &Client,
    url: &str,
    path: &Path,
    total_bytes_hint: u64,
    progress: Option<&ProgressBar>,
    cancel: &CancellationToken,
) -> Result<Downloaded> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let part_path = part_path(path);

    let mut attempt = 0u32;
    loop {
        if cancel.is_cancelled() {
            cleanup(&part_path).await;
            return Err(Error::Interrupted);
        }
        match download_attempt(client, url, &part_path, total_bytes_hint, progress, cancel).await {
            Ok(bytes) => {
                if let Err(e) = tokio::fs::rename(&part_path, path).await {
                    cleanup(&part_path).await;
                    return Err(Error::Io(e));
                }
                if let Some(pb) = progress {
                    pb.finish_and_clear();
                }
                return Ok(Downloaded {
                    path: path.to_path_buf(),
                    bytes,
                });
            }
            Err(Error::Interrupted) => {
                cleanup(&part_path).await;
                return Err(Error::Interrupted);
            }
            Err(e) if e.is_retryable() && attempt < MAX_RETRIES => {
                cleanup(&part_path).await;
                if let Some(pb) = progress {
                    pb.set_message(format!("重试 {}/{}", attempt + 1, MAX_RETRIES));
                }
                // 429 优先听服务器的 Retry-After
                let wait = e.retry_after_secs().unwrap_or_else(|| {
                    BACKOFF_SECS
                        .get(attempt as usize)
                        .copied()
                        .unwrap_or(*BACKOFF_SECS.last().unwrap())
                });
                tracing::warn!("{e}；{wait}s 后重试（{}/{}）", attempt + 1, MAX_RETRIES);
                if sleep_or_cancel(Duration::from_secs(wait), cancel).await {
                    return Err(Error::Interrupted);
                }
                attempt += 1;
            }
            Err(e) if e.is_retryable() => {
                cleanup(&part_path).await;
                return Err(Error::Retries {
                    attempts: MAX_RETRIES,
                    last: e.to_string(),
                });
            }
            Err(e) => {
                cleanup(&part_path).await;
                return Err(e);
            }
        }
    }
}

/// 单次尝试：HEAD 探长度（失败则边收边写），GET 流式写入 .part，最后校验字节数。
async fn download_attempt(
    client: &Client,
    url: &str,
    part_path: &Path,
    total_bytes_hint: u64,
    progress: Option<&ProgressBar>,
    cancel: &CancellationToken,
) -> Result<u64> {
    // HEAD 拿 Content-Length；405/403/501 等不支持 HEAD 的情况直接退化到 GET
    let head_len = match cancel.run_until_cancelled(head_length(client, url)).await {
        Some(Ok(len)) => len,
        Some(Err(e)) => {
            tracing::debug!("HEAD 不可用（{e}），退化为 GET");
            None
        }
        // 用户已经按了 Ctrl+C：不要再去发 GET
        None => return Err(Error::Interrupted),
    };
    // 响应头可能等很久（慢 CDN / 被限速），等待期间同样要能被取消
    let resp = match cancel.run_until_cancelled(client.get(url).send()).await {
        Some(Ok(resp)) => resp,
        Some(Err(e)) => return Err(network_err(e)),
        None => return Err(Error::Interrupted),
    };
    if !resp.status().is_success() {
        return Err(status_error(
            resp.status(),
            url,
            resp.headers().get(RETRY_AFTER),
        ));
    }
    let declared = resp
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());

    // 取所有已知长度的最大值作为预期值：HEAD 说 4096 而 GET 只给 100 字节时，
    // 那是被截断的传输，必须报错而不是产出一个悄悄变短的文件。
    let expected = [head_len, declared]
        .into_iter()
        .flatten()
        .max()
        // 两处都没给长度时才退回 DASH 清单里的 size（仅供进度条与兜底校验）
        .or((total_bytes_hint > 0).then_some(total_bytes_hint));

    if let Some(pb) = progress
        && let Some(total) = expected
    {
        pb.set_length(total);
    }

    let mut file = tokio::fs::File::create(part_path).await?;
    let mut written: u64 = 0;
    let mut stream = resp.bytes_stream();
    loop {
        let chunk = tokio::select! {
            _ = cancel.cancelled() => {
                drop(file);
                cleanup(part_path).await;
                return Err(Error::Interrupted);
            }
            chunk = stream.next() => chunk,
        };
        let Some(chunk) = chunk else { break };
        let chunk = chunk.map_err(network_err)?;
        file.write_all(&chunk).await?;
        written += chunk.len() as u64;
        if let Some(pb) = progress {
            pb.set_position(written);
        }
    }
    file.flush().await?;
    drop(file);

    // 绝不产出长度不对的文件
    if let Some(expected) = expected
        && written != expected
    {
        cleanup(part_path).await;
        return Err(Error::SizeMismatch {
            expected,
            actual: written,
        });
    }
    Ok(written)
}

/// 用 HEAD 探长度。返回 `None` 表示服务器没给 Content-Length。
async fn head_length(client: &Client, url: &str) -> Result<Option<u64>> {
    let resp = client.head(url).send().await.map_err(network_err)?;
    let status = resp.status();
    if status.is_success() {
        return Ok(resp
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok()));
    }
    // 不支持 HEAD 的情况退化为 GET
    if matches!(
        status,
        StatusCode::METHOD_NOT_ALLOWED
            | StatusCode::FORBIDDEN
            | StatusCode::NOT_IMPLEMENTED
            | StatusCode::BAD_REQUEST
    ) {
        return Ok(None);
    }
    Err(status_error(status, url, resp.headers().get(RETRY_AFTER)))
}

fn status_error(
    status: StatusCode,
    url: &str,
    retry_after: Option<&reqwest::header::HeaderValue>,
) -> Error {
    let retry_after = retry_after
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    if let Some(secs) = retry_after {
        tracing::warn!("服务器要求 {secs}s 后再试（Retry-After）");
    }
    Error::Http {
        status: status.as_u16(),
        url: url.to_string(),
        retry_after_secs: retry_after,
    }
}

fn part_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".part");
    PathBuf::from(name)
}

async fn cleanup(part_path: &Path) {
    let _ = tokio::fs::remove_file(part_path).await;
}

/// 睡一会儿，但可以被取消打断。返回 `true` 表示被取消了。
async fn sleep_or_cancel(duration: Duration, cancel: &CancellationToken) -> bool {
    tokio::select! {
        _ = cancel.cancelled() => true,
        _ = tokio::time::sleep(duration) => false,
    }
}

fn network_err(e: reqwest::Error) -> Error {
    if e.is_timeout() {
        return Error::Network(format!("超时: {e}"));
    }
    Error::Network(e.to_string())
}

/// 建进度条。非 TTY（管道、CI）时 indicatif 会自动降级为纯文本行为。
pub fn progress_bar(label: &str, enabled: bool) -> Option<ProgressBar> {
    if !enabled {
        return None;
    }
    let pb = ProgressBar::new(0);
    let style = ProgressStyle::with_template(
        "{spinner:.green} {msg} [{bar:32.cyan/blue}] {bytes}/{total_bytes} {bytes_per_sec} ETA {eta}",
    )
    .unwrap_or_else(|_| ProgressStyle::default_bar())
    .progress_chars("=>-");
    pb.set_style(style);
    pb.set_message(label.to_string());
    Some(pb)
}

/// 便于阅读用的字节数。
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_bytes_formats() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(999), "999 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(1024 * 1024), "1.0 MiB");
    }

    #[test]
    fn part_path_appends_suffix() {
        assert_eq!(
            part_path(Path::new("/out/a.mp4")),
            PathBuf::from("/out/a.mp4.part")
        );
    }

    #[test]
    fn client_errors_are_not_retryable_except_429() {
        assert!(!http(403).is_retryable());
        assert!(http(429).is_retryable());
        assert!(http(408).is_retryable());
        assert!(http(503).is_retryable());
        assert!(!http(404).is_retryable());
    }

    fn http(status: u16) -> Error {
        Error::Http {
            status,
            url: "u".into(),
            retry_after_secs: None,
        }
    }

    #[test]
    fn retry_after_is_surfaced() {
        let e = Error::Http {
            status: 429,
            url: "u".into(),
            retry_after_secs: Some(30),
        };
        assert_eq!(e.retry_after_secs(), Some(30));
        assert!(e.is_retryable());
    }
}
