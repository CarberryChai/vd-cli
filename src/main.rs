//! `vd` —— Bilibili 视频下载 CLI。
//!
//! 流程见 spec §5：解析参数 → resolve → 取任务列表 → 过滤分 P → 初始化 WBI →
//! 逐个视频（取流 → 选轨 → 下载 → 混流）→ 汇总退出码。

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use vd_cli::api::{Api, Bases, playurl};
use vd_cli::cli::{Cli, PagesSpec};
use vd_cli::error::{Error, Result};
use vd_cli::model::{AudioTrack, Page, VideoTrack, quality_weight};
use vd_cli::path as output_path;
use vd_cli::resolve::{self, Target};
use vd_cli::{download, mux, select};

/// 部分失败（合集场景：至少一个视频失败）。
const EXIT_PARTIAL: u8 = 5;
/// 短链最多跟随的跳数。
const MAX_SHORT_LINK_HOPS: usize = 5;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing(cli.verbose);
    match run(cli).await {
        Ok(code) => ExitCode::from(code),
        Err(err) => {
            report(&err);
            ExitCode::from(err.exit_code() as u8)
        }
    }
}

fn init_tracing(verbose: bool) {
    let level = if verbose { "debug" } else { "warn" };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .try_init();
}

/// 打印本机检测到的浏览器，以及哪个能读到 Cookie。
///
/// 这个子功能很有必要：macOS 上 Chrome 的目录默认被 TCC 保护，用户需要先知道
/// 「是没装、还是没权限」，而不是看一个含糊的读取失败。
fn list_browsers() {
    use vd_cli::browser::Browser;
    println!("本机浏览器检测：\n");
    for browser in [
        Browser::Chrome,
        Browser::Chromium,
        Browser::Brave,
        Browser::Edge,
        Browser::Vivaldi,
        Browser::Opera,
        Browser::Firefox,
        Browser::Safari,
    ] {
        let Some(root) = browser.profile_root() else {
            continue;
        };
        let name = browser.as_str();
        if !root.exists() {
            println!("  {name:10} 未安装");
            continue;
        }
        if matches!(browser, Browser::Firefox | Browser::Safari) {
            println!("  {name:10} 已安装（暂不支持自动读取，请用 --cookie）");
            continue;
        }
        // 真正试探一下能不能读
        match vd_cli::browser::load(browser, None) {
            Ok(cookie) => {
                let count = cookie.split(';').count();
                println!("  {name:10} 可用（读到 {count} 个 bilibili.com Cookie）");
            }
            Err(err) => {
                let reason = err
                    .to_string()
                    .lines()
                    .next()
                    .unwrap_or("未知错误")
                    .to_string();
                println!("  {name:10} 不可用：{reason}");
            }
        }
    }
    // 说明默认的 auto 模式会选谁
    match vd_cli::browser::load_auto(None) {
        Ok((browser, _)) => println!(
            "默认（--cookies-from-browser auto）会使用：{}",
            browser.as_str()
        ),
        Err(e) => {
            println!("默认（--cookies-from-browser auto）当前读不到，会以降级方式继续：\n  {e}")
        }
    }
    println!();
}

fn report(err: &Error) {
    eprintln!("error: {err}");
    for hint in err.hints() {
        eprintln!("  → {hint}");
    }
}

async fn run(cli: Cli) -> Result<u8> {
    // --list-browsers：只做检测，不碰网络也不要求 ffmpeg
    if cli.list_browsers {
        list_browsers();
        return Ok(0);
    }

    // ---- 1. 参数与前置检查（退出码 2 / EX_FFMPEG 都在这一步暴露）----
    let pages_spec = PagesSpec::parse(&cli.pages)?;
    if !cli.quality_is_reachable() {
        tracing::warn!(
            "--quality {} 在源码里没有精确对应的档位，将降级到可用的最高档位",
            cli.quality.as_str()
        );
    }
    if !cli.no_mux {
        mux::ensure_ffmpeg().await?;
    }

    // Ctrl+C → CancellationToken：下载与混流都会尽快退出（退出码 130）
    let cancel = CancellationToken::new();
    {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                cancel.cancel();
            }
        });
    }

    // UA 在进程内固定
    let ua = vd_cli::api::pick_user_agent();
    let (cookie, cookie_source) = cli.resolve_cookie()?;
    tracing::debug!("Cookie 来源: {cookie_source}");
    let mut api = Api::new(Bases::default(), ua, cookie.as_deref())?;

    // 取流接口要求带前端指纹 cookie buvid3，缺了会拿到风控响应（code: 0 + v_voucher）。
    // download 与 mux 都要用补齐后的 Cookie，所以必须在建这些之前做。
    api.ensure_buvid3().await;

    // ---- 2/3. resolve + 取任务列表 ----
    let input = cli.input()?;
    let target = resolve_target(&api, input).await?;
    let is_collection = matches!(target, Target::List { .. });
    let (container, all_pages, owner) = match target {
        Target::Video { aid } => {
            let info = api.view(aid).await?;
            tracing::debug!("{} / {}", info.title, info.owner.as_deref().unwrap_or("-"));
            (info.title, info.pages, info.owner)
        }
        Target::List { biz_id, kind } => {
            let info = api.list(biz_id, kind).await?;
            tracing::debug!(
                "{} {}: {} 个分 P",
                kind.as_str(),
                info.title,
                info.pages.len()
            );
            (info.title, info.pages, None)
        }
    };

    // ---- 4. 按 -p 过滤分 P ----
    let selected: Vec<Page> = pages_spec.apply(&all_pages)?.into_iter().cloned().collect();
    let multi_page = all_pages.len() > 1;

    // ---- 5. WBI 密钥取一次，全局复用 ----
    let mixin_key = playurl::init_mixin_key(&api).await?;

    // ---- 6. 逐个分 P 处理 ----
    let mut items: Vec<Item> = Vec::new();
    let mut failures = 0usize;
    for (i, page) in selected.iter().enumerate() {
        if cancel.is_cancelled() {
            return Err(Error::Interrupted);
        }
        // 视频内分 P 用分 P 序号；合集用条目在合集里的位置（spec §11）
        let number = if is_collection {
            i as u32 + 1
        } else {
            page.index
        };
        let layout = output_path::plan(
            &cli.output_dir,
            &container,
            page,
            number,
            selected.len(),
            multi_page,
        );
        let label = format!(
            "[{}/{}] {}",
            i + 1,
            selected.len(),
            truncate_label(&page.title, 40)
        );

        let outcome = process_page(
            &api,
            page,
            owner.as_deref(),
            &mixin_key,
            &cli,
            &layout,
            &label,
            &cancel,
        )
        .await;
        match outcome {
            Ok(done) => {
                if !cli.json && !cli.dry_run {
                    // 进度条结束后再打印结果行
                    println!("完成 {}", layout.final_path.display());
                }
                items.push(done);
            }
            Err(Error::Interrupted) => return Err(Error::Interrupted),
            Err(err) => {
                // 进度条与错误信息都写 stderr，stdout 留给结果
                report(&err);
                failures += 1;
                if let Some(fatal) = fatal_error(&err, selected.len(), failures) {
                    return Err(fatal);
                }
            }
        }
    }

    print_summary(&cli, &container, &selected, &items, failures);

    Ok(if failures == 0 { 0 } else { EXIT_PARTIAL })
}

/// 一个分 P 的处理结果（用于汇总与 JSON 输出）。
struct Item {
    title: String,
    aid: u64,
    cid: u64,
    duration: u32,
    quality: String,
    codec: String,
    path: PathBuf,
    size: u64,
    /// 该视频实际可用的「清晰度/编码」组合，`--dry-run` 用来告诉用户有什么可选
    available: Vec<String>,
}

/// 单视频时立刻抛错（保留原始退出码）；合集里只在全局性问题或首个就失败时抛错。
fn fatal_error(err: &Error, total: usize, failures: usize) -> Option<Error> {
    if total == 1 {
        return Some(err.clone());
    }
    if matches!(
        err,
        Error::NeedLogin
            | Error::Forbidden
            | Error::VipRequired
            | Error::RiskControl
            | Error::SignCheckFailed
            | Error::NotJson { .. }
            | Error::Interrupted
    ) {
        return Some(err.clone());
    }
    if failures == 1 && matches!(err, Error::Network(_) | Error::Retries { .. }) {
        // 第一个视频就是网络问题，多半是环境问题，不必跑完剩下的
        return Some(err.clone());
    }
    None
}

#[allow(clippy::too_many_arguments)] // 流程的各段参数，拆结构体反而更难读
async fn process_page(
    api: &Api,
    page: &Page,
    owner: Option<&str>,
    mixin_key: &str,
    cli: &Cli,
    layout: &output_path::Layout,
    label: &str,
    cancel: &CancellationToken,
) -> Result<Item> {
    // a/b. 取流（两轮：qn=0 拿轨道，qn=127 拿免二压）
    let dash = api
        .playurl_with(page.aid, page.cid, mixin_key, cli.fnval, cancel)
        .await?;

    // c. 选轨
    let prefs = cli.prefs();
    let video = select::select_video(&dash.video, &prefs).ok_or(Error::NoVideoStream)?;
    let audio = if dash.audio.is_empty() {
        None
    } else {
        Some(select::select_audio(&dash.audio, &prefs).ok_or(Error::NoAudioStream)?)
    };
    tracing::debug!("{}", describe_choice(video, audio));

    let mut item = Item {
        title: page.title.clone(),
        aid: page.aid,
        cid: page.cid,
        duration: page.duration,
        quality: video.quality_name(),
        codec: video.codec_name(),
        path: layout.final_path.clone(),
        size: 0,
        available: available_tracks(&dash.video),
    };

    if cli.dry_run {
        return Ok(item);
    }

    // d/e/f. 下载 + 混流
    let client = api.media_client()?;
    let parent = layout
        .stem
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| cli.output_dir.clone());
    let stem = file_stem(&layout.stem);
    let show_progress = !cli.json;
    // 进度条标签带上「第几个 / 共几个」，合集里才分得清当前在下载什么
    let pb = download::progress_bar(&truncate_label(label, 48), show_progress);

    let (video_path, audio_path) = if cli.no_mux {
        // 保留分离的 .mp4 / .m4a
        (
            layout.final_path.clone(),
            layout.final_path.with_extension("m4a"),
        )
    } else {
        (
            parent.join(format!("{stem}.video.mp4")),
            parent.join(format!("{stem}.audio.m4a")),
        )
    };

    let video_done = download::download(
        &client,
        &video.urls,
        &video_path,
        video.size,
        pb.as_ref(),
        cancel,
    )
    .await?;

    let audio_done = match audio {
        Some(audio) => {
            if let Some(pb) = &pb {
                pb.set_message(truncate_label(&format!("{label} (音频)"), 48));
            }
            Some(
                download::download(&client, &audio.urls, &audio_path, 0, pb.as_ref(), cancel)
                    .await?,
            )
        }
        None => None,
    };

    if let Some(pb) = &pb {
        pb.finish_and_clear();
    }

    match (cli.no_mux, audio_done) {
        (true, _) => {
            // 分离轨道：视频为 .mp4，音频（若有）为 .m4a。这里不再打印，
            // 汇总由调用方负责，避免同一行结果出现两次。
        }
        (false, None) => {
            // 没有音频轨（纯画面 / 静音），视频轨直接当成品
            if video_done.path != layout.final_path {
                tokio::fs::rename(&video_done.path, &layout.final_path).await?;
            }
            tracing::warn!("该视频没有音频轨，产出只有画面的 mp4");
        }
        (false, Some(audio_done)) => {
            mux::mux(
                &video_done.path,
                &audio_done.path,
                &layout.final_path,
                &page.title,
                owner.or(page.upper.as_deref()),
                cancel,
            )
            .await?;
            // 混流成功后删除中间文件
            let _ = tokio::fs::remove_file(&video_done.path).await;
            let _ = tokio::fs::remove_file(&audio_done.path).await;
        }
    }

    item.size = tokio::fs::metadata(&layout.final_path)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    Ok(item)
}

/// 去重后的「清晰度/编码」清单，按清晰度权重与码率降序。
fn available_tracks(video: &[VideoTrack]) -> Vec<String> {
    let mut seen: Vec<(u32, u32, String)> = Vec::new();
    for track in video {
        let entry = format!("{} / {}", track.quality_name(), track.codec_name());
        let key = (track.id, track.codecid, entry.clone());
        if !seen
            .iter()
            .any(|(id, codecid, _)| *id == key.0 && *codecid == key.1)
        {
            seen.push(key);
        }
    }
    seen.sort_by(|a, b| {
        quality_weight(b.0)
            .cmp(&quality_weight(a.0))
            .then_with(|| a.1.cmp(&b.1))
    });
    seen.into_iter().map(|(_, _, name)| name).collect()
}

fn describe_choice(video: &VideoTrack, audio: Option<&AudioTrack>) -> String {
    let audio = audio
        .map(|a| format!("{} / {}kbps", a.codecs, a.kbps()))
        .unwrap_or_else(|| "无".into());
    format!(
        "选中 {} / {} / {}kbps / {}x{} / 音频 {} / {} 个备用源",
        video.quality_name(),
        video.codec_name(),
        video.kbps(),
        video.width,
        video.height,
        audio,
        video.urls.len()
    )
}

fn file_stem(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "video".to_string())
}

fn truncate_label(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn print_summary(cli: &Cli, container: &str, pages: &[Page], items: &[Item], failures: usize) {
    if cli.json {
        let entries: Vec<Value> = if cli.dry_run {
            // --dry-run --json：只描述将要下载的内容
            items
                .iter()
                .map(|item| {
                    json!({
                        "title": item.title,
                        "aid": item.aid,
                        "cid": item.cid,
                        "duration": item.duration,
                        "quality": item.quality,
                        "codec": item.codec,
                        "available": item.available,
                        "output": item.path.display().to_string(),
                    })
                })
                .collect()
        } else {
            items
                .iter()
                .map(|item| {
                    json!({
                        "title": item.title,
                        "aid": item.aid,
                        "cid": item.cid,
                        "duration": item.duration,
                        "quality": item.quality,
                        "codec": item.codec,
                        "path": item.path.display().to_string(),
                        "size": item.size,
                    })
                })
                .collect()
        };
        println!(
            "{}",
            json!({
                "container": container,
                "total": pages.len(),
                "succeeded": items.len(),
                "failed": failures,
                "items": entries,
            })
        );
        return;
    }

    if cli.dry_run {
        for item in items {
            println!(
                "标题: {}\naid/cid: {}/{}\n时长: {}s\n选中: {} / {}\n可用: {}\n输出: {}\n",
                item.title,
                item.aid,
                item.cid,
                item.duration,
                item.quality,
                item.codec,
                item.available.join("、"),
                item.path.display()
            );
        }
        println!("dry-run: 共 {} 项，未下载", pages.len());
        return;
    }

    if pages.len() > 1 || cli.output_dir != Path::new(".") {
        println!("输出目录: {}", cli.output_dir.display());
    }
    match (items.len(), failures) {
        (0, f) if f > 0 => eprintln!("全部失败：{f} 个"),
        (_, 0) if pages.len() > 1 => println!("全部完成，共 {} 个", items.len()),
        (_, 0) => {}
        (ok, f) => eprintln!("完成 {ok} 个，失败 {f} 个"),
    }
}

/// 解析输入；`b23.tv` 短链逐跳跟随重定向，每跳都校验 host。
async fn resolve_target(api: &Api, input: &str) -> Result<Target> {
    if !resolve::is_short_link(input) {
        return resolve::resolve(input);
    }
    tracing::debug!("短链，逐跳跟随重定向: {input}");
    // 媒体客户端禁止重定向，这里手工跟随并逐跳校验
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| Error::Network(e.to_string()))?;
    let mut current = input.trim().to_string();
    for _ in 0..MAX_SHORT_LINK_HOPS {
        let resp = client
            .get(&current)
            .header(reqwest::header::USER_AGENT, api.user_agent())
            .header(reqwest::header::REFERER, "https://www.bilibili.com/")
            .send()
            .await
            .map_err(vd_cli::api::network_err)?;
        let status = resp.status();
        if status.is_redirection() {
            let location = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| Error::BadInput(format!("短链 {current} 重定向但缺少 Location")))?;
            let next = resp
                .url()
                .join(location)
                .map_err(|_| Error::UntrustedRedirect(location.to_string()))?;
            current = resolve::check_redirect(&current, next.as_str())?.to_string();
            continue;
        }
        if !status.is_success() {
            return Err(Error::Http {
                status: status.as_u16(),
                url: current,
                retry_after_secs: resp
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.trim().parse().ok()),
            });
        }
        return resolve::resolve(&current);
    }
    Err(Error::TooManyRedirects(MAX_SHORT_LINK_HOPS))
}
