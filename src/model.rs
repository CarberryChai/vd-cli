//! 领域模型：分 P、轨道、清晰度表。

/// 清晰度表：`(qn 码, 显示名, 排序权重)`，见 spec §7.6。
pub const QUALITY_MAP: &[(&str, &str, u32)] = &[
    ("127", "8K 超高清", 80),
    ("126", "杜比视界", 75),
    ("125", "HDR 真彩", 70),
    ("120", "4K 超清", 60),
    ("116", "1080P 高帧率", 55),
    ("112", "1080P 高码率", 50),
    ("100", "智能修复", 45),
    ("80", "1080P 高清", 40),
    ("74", "720P 高帧率", 35),
    ("64", "720P 高清", 30),
    ("48", "720P 高清", 30),
    ("32", "480P 清晰", 20),
    ("16", "360P 流畅", 10),
    ("6", "240P 流畅", 5),
    ("5", "144P 流畅", 5),
];

/// 清晰度 qn 码 → 显示名。未知码返回 `清晰度 {id}`。
pub fn quality_name(qn: u32) -> String {
    QUALITY_MAP
        .iter()
        .find(|(code, _, _)| code.parse::<u32>() == Ok(qn))
        .map(|(_, name, _)| (*name).to_string())
        .unwrap_or_else(|| format!("清晰度 {qn}"))
}

/// 清晰度 qn 码 → 排序权重。未知码按 0 处理（会被明确的 `--quality` 上限挡掉）。
pub fn quality_weight(qn: u32) -> u32 {
    QUALITY_MAP
        .iter()
        .find(|(code, _, _)| code.parse::<u32>() == Ok(qn))
        .map(|(_, _, weight)| *weight)
        .unwrap_or(0)
}

/// 视频编码。`codecid`：7=AVC，12=HEVC，13=AV1。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    Avc,
    Hevc,
    Av1,
}

impl Codec {
    pub fn as_str(self) -> &'static str {
        match self {
            Codec::Avc => "AVC",
            Codec::Hevc => "HEVC",
            Codec::Av1 => "AV1",
        }
    }

    pub fn codecid(self) -> u32 {
        match self {
            Codec::Avc => 7,
            Codec::Hevc => 12,
            Codec::Av1 => 13,
        }
    }

    pub fn from_codecid(id: u32) -> Option<Codec> {
        match id {
            7 => Some(Codec::Avc),
            12 => Some(Codec::Hevc),
            13 => Some(Codec::Av1),
            _ => None,
        }
    }
}

/// 一个分 P（或合集中的一条视频的分 P）。
#[derive(Debug, Clone)]
pub struct Page {
    /// 分 P 序号，从 1 开始
    pub index: u32,
    pub aid: u64,
    pub cid: u64,
    /// 最终用于文件名的主体（单 P 时是视频标题，多 P 时是分 P 标题）
    pub title: String,
    pub duration: u32,
    /// UP 主名，用于写入 movie 元数据
    pub upper: Option<String>,
}

/// `view` 接口的结果。
#[derive(Debug, Clone)]
pub struct VideoInfo {
    pub aid: u64,
    pub title: String,
    pub owner: Option<String>,
    pub duration: u32,
    pub pages: Vec<Page>,
}

/// `medialist` 接口的结果。
#[derive(Debug, Clone)]
pub struct ListInfo {
    pub biz_id: u64,
    pub title: String,
    pub pages: Vec<Page>,
}

/// 一条视频轨。
#[derive(Debug, Clone)]
pub struct VideoTrack {
    pub id: u32,
    pub codecid: u32,
    pub bandwidth: u64,
    pub width: u32,
    pub height: u32,
    pub frame_rate: String,
    pub size: u64,
    /// 主地址 + 备用 CDN，按顺序尝试
    pub urls: Vec<String>,
}

impl VideoTrack {
    pub fn codec(&self) -> Option<Codec> {
        Codec::from_codecid(self.codecid)
    }

    pub fn codec_name(&self) -> String {
        self.codec()
            .map(|c| c.as_str().to_string())
            .unwrap_or_else(|| format!("codecid={}", self.codecid))
    }

    pub fn quality_name(&self) -> String {
        quality_name(self.id)
    }

    pub fn kbps(&self) -> u64 {
        self.bandwidth / 1000
    }
}

/// 一条音频轨。`codecs` 已归一化为 `M4A` / `E-AC-3` / `FLAC`。
#[derive(Debug, Clone)]
pub struct AudioTrack {
    pub id: u32,
    pub codecs: String,
    pub bandwidth: u64,
    pub urls: Vec<String>,
}

impl AudioTrack {
    pub fn kbps(&self) -> u64 {
        self.bandwidth / 1000
    }
}

/// DASH 清单。
#[derive(Debug, Clone, Default)]
pub struct Dash {
    pub video: Vec<VideoTrack>,
    pub audio: Vec<AudioTrack>,
    pub duration: u32,
}

/// 归一化音频 `codecs` 字段，否则选轨时匹配不上。
pub fn normalize_audio_codecs(raw: &str) -> String {
    let lower = raw.trim().to_ascii_lowercase();
    match lower.as_str() {
        "mp4a.40.2" | "mp4a.40.5" => "M4A".into(),
        "ec-3" => "E-AC-3".into(),
        "flac" => "FLAC".into(),
        other => {
            if other.starts_with("mp4a") {
                "M4A".into()
            } else {
                raw.trim().to_string()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_lookup() {
        assert_eq!(quality_name(80), "1080P 高清");
        assert_eq!(quality_weight(80), 40);
        assert_eq!(quality_weight(127), 80);
        assert_eq!(quality_name(12345), "清晰度 12345");
        assert_eq!(quality_weight(12345), 0);
    }

    #[test]
    fn audio_codecs_normalization() {
        assert_eq!(normalize_audio_codecs("mp4a.40.2"), "M4A");
        assert_eq!(normalize_audio_codecs("mp4a.40.5"), "M4A");
        assert_eq!(normalize_audio_codecs("ec-3"), "E-AC-3");
        assert_eq!(normalize_audio_codecs("fLaC"), "FLAC");
        assert_eq!(normalize_audio_codecs(" EC-3 "), "E-AC-3");
    }

    #[test]
    fn codec_mapping() {
        assert_eq!(Codec::from_codecid(7), Some(Codec::Avc));
        assert_eq!(Codec::from_codecid(12), Some(Codec::Hevc));
        assert_eq!(Codec::from_codecid(13), Some(Codec::Av1));
        assert_eq!(Codec::from_codecid(11), None);
    }
}
