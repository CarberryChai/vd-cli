//! 轨道选择（纯函数，无网络）。

use crate::model::{AudioTrack, Codec, VideoTrack, quality_weight};

/// 选轨偏好。
#[derive(Debug, Clone, Copy)]
pub struct Prefs {
    /// 清晰度权重上限，`u32::MAX` 表示不限
    pub quality_limit: u32,
    pub codec: Codec,
}

/// 音频编码优先级：FLAC > E-AC-3 > M4A。
fn audio_rank(codecs: &str) -> u8 {
    match codecs {
        "FLAC" => 3,
        "E-AC-3" => 2,
        "M4A" => 1,
        _ => 0,
    }
}

/// 选一条视频轨。
///
/// 排序键（从高到低）：清晰度权重（且必须 ≤ `--quality` 上限）→ 编码优先级 → 码率。
/// 先按「清晰度上限 + 编码偏好」筛，没有结果就放宽编码偏好再筛一次（打 warn）。
pub fn select_video<'a>(tracks: &'a [VideoTrack], prefs: &Prefs) -> Option<&'a VideoTrack> {
    let sort = |list: &mut Vec<&'a VideoTrack>| {
        list.sort_by(|a, b| {
            quality_weight(b.id)
                .cmp(&quality_weight(a.id))
                .then_with(|| {
                    let rank = |t: &VideoTrack| u8::from(t.codec() == Some(prefs.codec));
                    rank(b).cmp(&rank(a))
                })
                .then_with(|| b.bandwidth.cmp(&a.bandwidth))
        });
        list.first().copied()
    };

    let mut under_limit: Vec<&VideoTrack> = tracks
        .iter()
        .filter(|t| quality_weight(t.id) <= prefs.quality_limit)
        .collect();
    if let Some(pick) = sort(&mut under_limit) {
        // 上限是「上限」而不是精确匹配：源里没有对应档位就降级到可用的最高档位
        let exact = quality_weight(pick.id) == prefs.quality_limit;
        if !exact && prefs.quality_limit != u32::MAX {
            tracing::warn!(
                "指定的清晰度上限没有对应档位，降级到 {}",
                pick.quality_name()
            );
        }
        return Some(pick);
    }

    // 放宽编码偏好：清晰度上限仍然生效
    let mut relaxed: Vec<&VideoTrack> = tracks
        .iter()
        .filter(|t| quality_weight(t.id) <= prefs.quality_limit)
        .collect();
    if relaxed.is_empty() {
        return None;
    }
    tracing::warn!(
        "没有 {} 编码的可用视频流，放宽编码偏好",
        prefs.codec.as_str()
    );
    relaxed.sort_by(|a, b| {
        quality_weight(b.id)
            .cmp(&quality_weight(a.id))
            .then_with(|| b.bandwidth.cmp(&a.bandwidth))
    });
    relaxed.first().copied()
}

/// 选一条音频轨：FLAC > E-AC-3 > M4A，同级按码率降序。
pub fn select_audio<'a>(tracks: &'a [AudioTrack], _prefs: &Prefs) -> Option<&'a AudioTrack> {
    tracks.iter().max_by(|a, b| {
        audio_rank(&a.codecs)
            .cmp(&audio_rank(&b.codecs))
            .then_with(|| a.bandwidth.cmp(&b.bandwidth))
            // 完全同码率时用 id 保证结果稳定（不依赖输入顺序）
            .then_with(|| b.id.cmp(&a.id))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AudioTrack, VideoTrack};

    fn v(id: u32, codecid: u32, bandwidth: u64) -> VideoTrack {
        VideoTrack {
            id,
            codecid,
            bandwidth,
            width: 1920,
            height: 1080,
            frame_rate: "30".into(),
            size: 0,
            urls: vec!["https://example.com/v".into()],
        }
    }

    fn a(codecs: &str, bandwidth: u64, id: u32) -> AudioTrack {
        AudioTrack {
            id,
            codecs: codecs.into(),
            bandwidth,
            urls: vec!["https://example.com/a".into()],
        }
    }

    fn prefs(limit: u32, codec: Codec) -> Prefs {
        Prefs {
            quality_limit: limit,
            codec,
        }
    }

    #[test]
    fn quality_is_an_upper_bound() {
        let tracks = vec![
            v(80, 7, 1_000_000), // 1080P 高清 weight 40
            v(64, 7, 800_000),   // 720P weight 30
            v(32, 7, 400_000),   // 480P weight 20
        ];
        // 上限 720p（35）→ 只能选 720P
        assert_eq!(
            select_video(&tracks, &prefs(35, Codec::Avc)).unwrap().id,
            64
        );
        // 上限 1080p（55）→ 1080P
        assert_eq!(
            select_video(&tracks, &prefs(55, Codec::Avc)).unwrap().id,
            80
        );
        // 不限 → 最高
        assert_eq!(
            select_video(&tracks, &prefs(u32::MAX, Codec::Avc))
                .unwrap()
                .id,
            80
        );
    }

    #[test]
    fn downgrades_when_no_exact_tier() {
        // 只有 480P，却要求 1080p 上限 → 降级到 480P 而不是报错
        let tracks = vec![v(32, 7, 400_000)];
        assert_eq!(
            select_video(&tracks, &prefs(55, Codec::Avc)).unwrap().id,
            32
        );
    }

    #[test]
    fn codec_preference_breaks_ties() {
        let tracks = vec![v(80, 7, 2_000_000), v(80, 12, 1_000_000)];
        assert_eq!(
            select_video(&tracks, &prefs(u32::MAX, Codec::Hevc))
                .unwrap()
                .codecid,
            12
        );
        assert_eq!(
            select_video(&tracks, &prefs(u32::MAX, Codec::Avc))
                .unwrap()
                .codecid,
            7
        );
    }

    #[test]
    fn quality_beats_codec() {
        // 清晰度权重优先于编码偏好
        let tracks = vec![v(80, 12, 1_000_000), v(64, 7, 9_000_000)];
        assert_eq!(
            select_video(&tracks, &prefs(u32::MAX, Codec::Avc))
                .unwrap()
                .id,
            80
        );
    }

    #[test]
    fn bandwidth_breaks_ties() {
        let tracks = vec![v(80, 7, 1_000_000), v(80, 7, 3_000_000)];
        assert_eq!(
            select_video(&tracks, &prefs(u32::MAX, Codec::Avc))
                .unwrap()
                .bandwidth,
            3_000_000
        );
    }

    #[test]
    fn empty_video_list_is_none() {
        assert!(select_video(&[], &prefs(u32::MAX, Codec::Avc)).is_none());
    }

    #[test]
    fn nothing_under_limit_is_none() {
        let tracks = vec![v(80, 7, 1_000_000)]; // weight 40
        assert!(select_video(&tracks, &prefs(10, Codec::Avc)).is_none());
    }

    #[test]
    fn audio_prefers_flac_then_eac3_then_m4a() {
        let tracks = vec![a("M4A", 999_999, 30280), a("E-AC-3", 1, 30250)];
        assert_eq!(
            select_audio(&tracks, &prefs(0, Codec::Avc)).unwrap().codecs,
            "E-AC-3"
        );
        let tracks = vec![
            a("M4A", 999_999, 30280),
            a("E-AC-3", 1, 30250),
            a("FLAC", 1, 30251),
        ];
        assert_eq!(
            select_audio(&tracks, &prefs(0, Codec::Avc)).unwrap().codecs,
            "FLAC"
        );
        let tracks = vec![a("M4A", 100, 30216), a("M4A", 200, 30232)];
        assert_eq!(
            select_audio(&tracks, &prefs(0, Codec::Avc)).unwrap().id,
            30232
        );
        assert!(select_audio(&[], &prefs(0, Codec::Avc)).is_none());
    }
}
