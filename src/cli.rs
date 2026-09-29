//! clap 定义与参数解析后的配置结构。

use std::path::PathBuf;

use clap::{Parser, ValueEnum};

use crate::error::{Error, Result};
use crate::model::{Codec, QUALITY_MAP};
use crate::select::Prefs;

/// `--fnval` 的默认值。
///
/// 位掩码，按位或：1(FLV) 16(DASH) 64(HDR) 128(4K) 256(杜比音频) 512(杜比视界)
/// 1024(8K) 2048(AV1) = 4048。语义若变更，用 `--fnval` 覆盖。
pub const DEFAULT_FNVAL: u32 = 4048;

#[derive(Debug, Parser)]
#[command(
    name = "vd",
    version,
    about = "Bilibili 视频下载 CLI",
    long_about = "给一个链接、BV 号或 av 号，得到可播放的 mp4。\n\
                  支持视频内分 P、合集与系列，用 ffmpeg 混流 DASH 视频轨与音频轨。",
    after_help = "示例:\n  \
                  vd \"https://www.bilibili.com/video/BV1qt4y1X7TW\"\n  \
                  vd BV1qt4y1X7TW -q 720p -o ~/Downloads\n  \
                  vd av114514 -p 1-3\n  \
                  vd \"https://space.bilibili.com/23630128/channel/collectiondetail?sid=2045\""
)]
pub struct Cli {
    /// 视频链接、BV 号、av 号，或合集/系列链接
    #[arg(value_name = "URL|ID")]
    pub input: String,

    /// 输出目录
    #[arg(short, long, value_name = "DIR", default_value = ".")]
    pub output_dir: PathBuf,

    /// 清晰度上限
    #[arg(short, long, value_name = "Q", default_value = "max")]
    pub quality: Quality,

    /// 视频编码优先级
    #[arg(long, value_name = "C", default_value = "avc")]
    pub codec: CodecArg,

    /// 分P选择
    #[arg(short, long, value_name = "SPEC", default_value = "all")]
    pub pages: String,

    /// B 站 Cookie 字符串，用于解锁登录后才能拿到的高清档位
    #[arg(long, value_name = "STR")]
    pub cookie: Option<String>,

    /// 保留分离的 .mp4/.m4a，不调用 ffmpeg
    #[arg(long)]
    pub no_mux: bool,

    /// 只解析并打印将要下载的内容，不实际下载
    #[arg(long)]
    pub dry_run: bool,

    /// 以 JSON 输出结果，便于脚本处理
    #[arg(long)]
    pub json: bool,

    /// 打印调试日志
    #[arg(short, long)]
    pub verbose: bool,

    /// 覆盖 DASH 请求掩码（默认 4048）
    #[arg(long, value_name = "N", default_value_t = DEFAULT_FNVAL, hide_default_value = true)]
    pub fnval: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Quality {
    Max,
    #[value(name = "1080p")]
    P1080,
    #[value(name = "720p")]
    P720,
    #[value(name = "480p")]
    P480,
    #[value(name = "360p")]
    P360,
}

impl Quality {
    /// 到权重上限的映射，见 spec §7.6。
    pub fn limit(self) -> u32 {
        match self {
            Quality::Max => u32::MAX,
            Quality::P1080 => 55,
            Quality::P720 => 35,
            Quality::P480 => 20,
            Quality::P360 => 10,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Quality::Max => "max",
            Quality::P1080 => "1080p",
            Quality::P720 => "720p",
            Quality::P480 => "480p",
            Quality::P360 => "360p",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CodecArg {
    #[value(name = "avc")]
    Avc,
    #[value(name = "hevc")]
    Hevc,
    #[value(name = "av1")]
    Av1,
}

impl From<CodecArg> for Codec {
    fn from(value: CodecArg) -> Self {
        match value {
            CodecArg::Avc => Codec::Avc,
            CodecArg::Hevc => Codec::Hevc,
            CodecArg::Av1 => Codec::Av1,
        }
    }
}

impl CodecArg {
    pub fn as_str(self) -> &'static str {
        Codec::from(self).as_str()
    }
}

/// 分 P 选择。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PagesSpec {
    All,
    /// 1 基序号集合，升序去重
    Only(Vec<u32>),
}

impl PagesSpec {
    /// 解析 `all|1|1,3,5|1-5`（也接受混合的 `1-3,7`）。
    pub fn parse(spec: &str) -> Result<Self> {
        let spec = spec.trim();
        if spec.is_empty() {
            return Err(Error::BadArgs("--pages 不能为空".into()));
        }
        if spec.eq_ignore_ascii_case("all") {
            return Ok(PagesSpec::All);
        }

        let mut picked: Vec<u32> = Vec::new();
        for part in spec.split(',') {
            let part = part.trim();
            if part.is_empty() {
                return Err(Error::BadArgs(format!("--pages \"{spec}\" 里有空的片段")));
            }
            match part.split_once('-') {
                Some((lo, hi)) => {
                    let lo = parse_index(lo, spec)?;
                    let hi = parse_index(hi, spec)?;
                    if lo > hi {
                        return Err(Error::BadArgs(format!(
                            "--pages \"{spec}\" 的区间 {lo}-{hi} 起点大于终点"
                        )));
                    }
                    picked.extend(lo..=hi);
                }
                None => picked.push(parse_index(part, spec)?),
            }
        }
        picked.sort_unstable();
        picked.dedup();
        Ok(PagesSpec::Only(picked))
    }

    /// 按序号过滤分 P 列表。越界序号被忽略；结果为空时报参数错误。
    pub fn apply<'a>(
        &self,
        pages: &'a [crate::model::Page],
    ) -> Result<Vec<&'a crate::model::Page>> {
        match self {
            PagesSpec::All => Ok(pages.iter().collect()),
            PagesSpec::Only(indices) => {
                let picked: Vec<&crate::model::Page> = indices
                    .iter()
                    .filter_map(|i| pages.iter().find(|p| p.index == *i))
                    .collect();
                if picked.is_empty() {
                    let available: Vec<u32> = pages.iter().map(|p| p.index).collect();
                    return Err(Error::BadArgs(format!(
                        "--pages 指定的分 P 在该视频里不存在（可用: {}）",
                        join_indices(&available)
                    )));
                }
                let missing: Vec<String> = indices
                    .iter()
                    .filter(|i| !pages.iter().any(|p| p.index == **i))
                    .map(|i| i.to_string())
                    .collect();
                if !missing.is_empty() {
                    tracing::warn!("分 P {} 不存在，已跳过", missing.join(","));
                }
                Ok(picked)
            }
        }
    }
}

fn join_indices(indices: &[u32]) -> String {
    if indices.is_empty() {
        return "无".into();
    }
    indices
        .iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn parse_index(s: &str, whole: &str) -> Result<u32> {
    let s = s.trim();
    let n: u32 = s
        .parse()
        .map_err(|_| Error::BadArgs(format!("--pages \"{whole}\" 里的 \"{s}\" 不是正整数")))?;
    if n == 0 {
        return Err(Error::BadArgs(format!(
            "--pages \"{whole}\" 里的分 P 序号从 1 开始"
        )));
    }
    Ok(n)
}

impl Cli {
    pub fn prefs(&self) -> Prefs {
        Prefs {
            quality_limit: self.quality.limit(),
            codec: self.codec.into(),
        }
    }

    /// `--quality` 是否要求了源码里根本没有的档位（用于打印提示）。
    /// `max` 不限上限，永远可达。
    pub fn quality_is_reachable(&self) -> bool {
        self.quality == Quality::Max
            || QUALITY_MAP
                .iter()
                .any(|(_, _, w)| *w == self.quality.limit())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Page;

    fn page(index: u32) -> Page {
        Page {
            index,
            aid: 1,
            cid: 100 + index as u64,
            title: format!("P{index}"),
            duration: 60,
            upper: None,
        }
    }

    #[test]
    fn pages_all() {
        let pages: Vec<Page> = (1..=3).map(page).collect();
        assert_eq!(PagesSpec::parse("all").unwrap(), PagesSpec::All);
        assert_eq!(PagesSpec::parse("ALL").unwrap(), PagesSpec::All);
        assert_eq!(PagesSpec::All.apply(&pages).unwrap().len(), 3);
    }

    #[test]
    fn pages_single_and_list() {
        let pages: Vec<Page> = (1..=5).map(page).collect();
        assert_eq!(PagesSpec::parse("1").unwrap(), PagesSpec::Only(vec![1]));
        assert_eq!(
            PagesSpec::parse("1,3,5").unwrap(),
            PagesSpec::Only(vec![1, 3, 5])
        );
        let picked = PagesSpec::parse("1,3,5").unwrap().apply(&pages).unwrap();
        assert_eq!(
            picked.iter().map(|p| p.index).collect::<Vec<_>>(),
            vec![1, 3, 5]
        );
    }

    #[test]
    fn pages_range_and_mixed() {
        assert_eq!(
            PagesSpec::parse("1-5").unwrap(),
            PagesSpec::Only((1..=5).collect())
        );
        assert_eq!(
            PagesSpec::parse("1-3,7").unwrap(),
            PagesSpec::Only(vec![1, 2, 3, 7])
        );
        // 重复与乱序都归一化
        assert_eq!(
            PagesSpec::parse("3,1,1,2-2").unwrap(),
            PagesSpec::Only(vec![1, 2, 3])
        );
    }

    #[test]
    fn pages_out_of_range() {
        let pages: Vec<Page> = (1..=3).map(page).collect();
        // 部分越界：跳过并保留命中的
        let picked = PagesSpec::parse("2,9").unwrap().apply(&pages).unwrap();
        assert_eq!(picked.iter().map(|p| p.index).collect::<Vec<_>>(), vec![2]);
        // 全部越界：报参数错误
        let err = PagesSpec::parse("9").unwrap().apply(&pages).unwrap_err();
        assert!(matches!(err, Error::BadArgs(_)), "{err}");
        assert!(err.to_string().contains("1,2,3"));
    }

    #[test]
    fn pages_bad_specs() {
        for bad in ["", "0", "-1", "1-", "-2", "a", "1,", "1-a", "3-1"] {
            assert!(PagesSpec::parse(bad).is_err(), "\"{bad}\" 应当被拒绝");
        }
    }

    #[test]
    fn max_quality_is_always_reachable() {
        assert!(Cli::parse_from(["vd", "x", "-q", "max"]).quality_is_reachable());
        // 1080p 对应权重 55（1080P 高帧率）
        assert!(Cli::parse_from(["vd", "x", "-q", "1080p"]).quality_is_reachable());
        assert!(Cli::parse_from(["vd", "x", "-q", "720p"]).quality_is_reachable());
        assert!(Cli::parse_from(["vd", "x", "-q", "480p"]).quality_is_reachable());
        assert!(Cli::parse_from(["vd", "x", "-q", "360p"]).quality_is_reachable());
    }

    #[test]
    fn quality_limits() {
        assert_eq!(Quality::Max.limit(), u32::MAX);
        assert_eq!(Quality::P1080.limit(), 55);
        assert_eq!(Quality::P720.limit(), 35);
        assert_eq!(Quality::P480.limit(), 20);
        assert_eq!(Quality::P360.limit(), 10);
    }
}
