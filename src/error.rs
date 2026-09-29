//! 错误类型与退出码映射。
//!
//! 内部使用 `thiserror` 定义枚举，`main` 负责把枚举映射成进程退出码，
//! 并在打印时补上「下一步该做什么」的提示。

use crate::bv::BvError;
use crate::wbi::WbiError;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// 输入无法识别为受支持的目标。
    #[error("输入无法识别: {0}")]
    BadInput(String),

    #[error("参数错误: {0}")]
    BadArgs(String),

    #[error("视频不存在或已删除")]
    NotFound,

    #[error("需要登录")]
    NeedLogin,

    #[error("权限不足")]
    Forbidden,

    #[error("大会员专享内容")]
    VipRequired,

    #[error("触发风控")]
    RiskControl,

    #[error("区域限制")]
    RegionRestricted,

    #[error("接口签名校验失败")]
    SignCheckFailed,

    #[error("接口返回错误 code={code}: {message}")]
    Api { code: i64, message: String },

    #[error(transparent)]
    Bv(#[from] BvError),

    #[error(transparent)]
    Wbi(#[from] WbiError),

    #[error("没有可用的视频流")]
    NoVideoStream,

    #[error("没有可用的音频流")]
    NoAudioStream,

    #[error("该视频不支持（老格式），MVP 不处理 FLV 分段")]
    LegacyFormat,

    #[error("该内容是 DRM 加密的，无法下载")]
    Drm,

    #[error("接口返回了非 JSON 内容（可能被风控或需要登录）")]
    NotJson {
        content_type: Option<String>,
        snippet: String,
    },

    #[error("HTTP {status} <- {url}")]
    Http {
        status: u16,
        url: String,
        /// 服务器给的 `Retry-After`（秒），429 时优先用它
        retry_after_secs: Option<u64>,
    },

    #[error("网络错误: {0}")]
    Network(String),

    #[error("重试 {attempts} 次后仍然失败: {last}")]
    Retries { attempts: u32, last: String },

    #[error("重定向到不受信任的主机: {0}")]
    UntrustedRedirect(String),

    #[error("重定向次数过多（超过 {0} 跳）")]
    TooManyRedirects(usize),

    #[error("内容长度校验失败: 期望 {expected} 字节，实际 {actual} 字节")]
    SizeMismatch { expected: u64, actual: u64 },

    #[error("EX_FFMPEG: 未找到 ffmpeg")]
    FfmpegMissing,

    #[error("ffmpeg 运行失败: {0}")]
    FfmpegFailed(String),

    #[error("ffmpeg 超时（30 分钟），已强制终止")]
    FfmpegTimeout,

    #[error("用户中断")]
    Interrupted,

    /// 文案由构造点自己写完整（每个构造点都带「哪个浏览器 / 为什么」），
    /// 这里不再加统一前缀，否则会叠出"读取浏览器 Cookie 失败: ... 读取浏览器 Cookie 失败: ..."
    #[error("{0}")]
    BrowserCookie(String),

    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// `std::io::Error` 不是 `Clone`，手工重建一份。
impl Clone for Error {
    fn clone(&self) -> Self {
        match self {
            Error::BadInput(s) => Error::BadInput(s.clone()),
            Error::BadArgs(s) => Error::BadArgs(s.clone()),
            Error::NotFound => Error::NotFound,
            Error::NeedLogin => Error::NeedLogin,
            Error::Forbidden => Error::Forbidden,
            Error::VipRequired => Error::VipRequired,
            Error::RiskControl => Error::RiskControl,
            Error::RegionRestricted => Error::RegionRestricted,
            Error::SignCheckFailed => Error::SignCheckFailed,
            Error::Api { code, message } => Error::Api {
                code: *code,
                message: message.clone(),
            },
            Error::Bv(e) => Error::Bv(*e),
            Error::Wbi(e) => Error::Wbi(*e),
            Error::NoVideoStream => Error::NoVideoStream,
            Error::NoAudioStream => Error::NoAudioStream,
            Error::LegacyFormat => Error::LegacyFormat,
            Error::Drm => Error::Drm,
            Error::NotJson {
                content_type,
                snippet,
            } => Error::NotJson {
                content_type: content_type.clone(),
                snippet: snippet.clone(),
            },
            Error::Http {
                status,
                url,
                retry_after_secs,
            } => Error::Http {
                status: *status,
                url: url.clone(),
                retry_after_secs: *retry_after_secs,
            },
            Error::Network(s) => Error::Network(s.clone()),
            Error::Retries { attempts, last } => Error::Retries {
                attempts: *attempts,
                last: last.clone(),
            },
            Error::UntrustedRedirect(s) => Error::UntrustedRedirect(s.clone()),
            Error::TooManyRedirects(n) => Error::TooManyRedirects(*n),
            Error::SizeMismatch { expected, actual } => Error::SizeMismatch {
                expected: *expected,
                actual: *actual,
            },
            Error::FfmpegMissing => Error::FfmpegMissing,
            Error::FfmpegFailed(s) => Error::FfmpegFailed(s.clone()),
            Error::FfmpegTimeout => Error::FfmpegTimeout,
            Error::Interrupted => Error::Interrupted,
            Error::BrowserCookie(s) => Error::BrowserCookie(s.clone()),
            Error::Io(e) => Error::Io(std::io::Error::new(e.kind(), e.to_string())),
        }
    }
}

impl Error {
    /// 进程退出码，见 spec §3。
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::BadInput(_) | Error::BadArgs(_) => 2,
            Error::NeedLogin | Error::Forbidden | Error::VipRequired => 4,
            Error::RiskControl | Error::Network(_) | Error::Retries { .. } => 3,
            Error::Interrupted => 130,
            _ => 1,
        }
    }

    /// 这条错误是否值得重试（网络抖动、5xx、429/408）。
    pub fn is_retryable(&self) -> bool {
        match self {
            Error::Network(_) | Error::Retries { .. } | Error::Io(_) => true,
            Error::Http { status, .. } => *status >= 500 || *status == 408 || *status == 429,
            _ => false,
        }
    }

    /// 服务器要求的等待秒数（`Retry-After`）。
    pub fn retry_after_secs(&self) -> Option<u64> {
        match self {
            Error::Http {
                retry_after_secs, ..
            } => *retry_after_secs,
            _ => None,
        }
    }

    /// 面向用户的「下一步」提示，逐行打印。
    pub fn hints(&self) -> Vec<String> {
        match self {
            Error::SignCheckFailed => vec![
                "请检查系统时间是否准确（签名有效期约 60 秒）".into(),
                "或稍后重试".into(),
            ],
            Error::NeedLogin => vec![
                "加上 --cookie 传入浏览器里的 Cookie 后重试（未登录时清晰度通常最高只有 480P）"
                    .into(),
            ],
            Error::Forbidden => vec!["该内容需要登录或更高权限，加上 --cookie 后重试".into()],
            Error::VipRequired => {
                vec!["该清晰度是大会员专享，--quality 降到 1080p 或更低，或用大会员 Cookie".into()]
            }
            Error::RiskControl => vec![
                "触发风控，请稍后重试；不要并发下载，单条链接之间留出间隔".into(),
                "如果持续出现，换个网络或稍等几分钟".into(),
            ],
            Error::NotFound => vec!["检查链接、BV 号或 av 号是否写对".into()],
            Error::RegionRestricted => vec!["该内容在当前地区不可用".into()],
            Error::Bv(_) => vec!["BV 号形如 BV1qt4y1X7TW（12 位），或直接用视频链接".into()],
            Error::LegacyFormat => {
                vec!["这是 2018 年前的老视频，只有 FLV 分段格式，请用其它工具下载".into()]
            }
            Error::Drm => vec!["DRM 加密内容本工具不解密，请用官方客户端观看".into()],
            Error::NotJson { .. } => {
                vec!["通常是触发风控或需要登录：稍后重试，或加上 --cookie".into()]
            }
            Error::FfmpegMissing => vec![install_hint()],
            Error::NoVideoStream => {
                vec!["可能需要在 --cookie 下重试，或换一个 --quality 档位".into()]
            }
            Error::Retries { .. } | Error::Network(_) => {
                vec!["检查网络连接；B 站 CDN 偶发失败，直接重跑一次通常就好".into()]
            }
            Error::UntrustedRedirect(_) | Error::TooManyRedirects(_) => {
                vec!["短链解析失败，改用完整链接（www.bilibili.com/video/BV...）".into()]
            }
            _ => vec![],
        }
    }
}

/// 针对当前平台的 ffmpeg 安装提示。
pub fn install_hint() -> String {
    if cfg!(target_os = "macos") {
        "安装 ffmpeg: brew install ffmpeg".into()
    } else if cfg!(target_os = "windows") {
        "安装 ffmpeg: winget install Gyan.FFmpeg（或 scoop install ffmpeg）".into()
    } else {
        "安装 ffmpeg: sudo apt install ffmpeg（Debian/Ubuntu）或 sudo dnf install ffmpeg（Fedora）"
            .into()
    }
}
