//! 输入 → 内部标识。
//!
//! `main → resolve → api` 的第一环，只做字符串/URL 层面的识别，发网络请求
//! 的唯一出口是 `b23.tv` 短链（且逐跳校验 host）。

use url::Url;

use crate::bv::{bvid_to_aid, parse_aid};
use crate::error::{Error, Result};

/// 合集还是系列。接口只有 `type` 参数不同（合集 8，系列 5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListKind {
    Collection,
    Series,
}

impl ListKind {
    pub fn api_type(self) -> u32 {
        match self {
            ListKind::Collection => 8,
            ListKind::Series => 5,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ListKind::Collection => "合集",
            ListKind::Series => "系列",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Video { aid: u64 },
    List { biz_id: u64, kind: ListKind },
}

/// 是否需要发网络请求跟随短链。
pub fn is_short_link(input: &str) -> bool {
    match Url::parse(input) {
        Ok(u) => u.host_str() == Some("b23.tv"),
        Err(_) => false,
    }
}

/// 主机是否可信：`bilibili.com` 及其子域，或 `b23.tv`。
///
/// 注意用 host 精确比较，不要用 `contains("b23.tv")`——`evilb23.tv` 与
/// `b23.tv.evil.com` 都会命中。
pub fn is_trusted_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    host == "bilibili.com"
        || host.ends_with(".bilibili.com")
        || host == "b23.tv"
        || host.ends_with(".b23.tv")
}

/// 把输入解析成内部标识。`input` 可能是短链跟随后的 URL。
pub fn resolve(input: &str) -> Result<Target> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err(Error::BadInput("输入为空".into()));
    }

    // 裸 BV 号 / av 号
    if !raw.contains("://") && !raw.contains('/') && !raw.contains('?') {
        return resolve_bare(raw);
    }

    let url = Url::parse(raw).map_err(|_| Error::BadInput(raw.to_string()))?;
    let host = url.host_str().unwrap_or("").to_ascii_lowercase();
    if host.is_empty() {
        return Err(Error::BadInput(raw.to_string()));
    }

    // 合集 / 系列必须在「UP 主空间」之前判断，否则会被误判
    if let Some(kind_id) = resolve_space_list(&url) {
        return Ok(kind_id);
    }
    if let Some(list) = resolve_medialist(&url) {
        return Ok(list);
    }
    // bilibili.com/video/BV... 或 av...
    if let Some(video) = resolve_video_path(&url) {
        return Ok(video);
    }

    Err(Error::BadInput(raw.to_string()))
}

/// 去掉大小写不敏感的前缀。
fn strip_ci_prefix<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

/// 裸输入：`BV...` / `av123` / `123`。
fn resolve_bare(raw: &str) -> Result<Target> {
    if raw.len() == 12 && strip_ci_prefix(raw, "BV").is_some() {
        return Ok(Target::Video {
            aid: bvid_to_aid(raw)?,
        });
    }
    if let Some(rest) = strip_ci_prefix(raw, "av")
        && !rest.is_empty()
        && rest.bytes().all(|b| b.is_ascii_digit())
    {
        return Ok(Target::Video {
            aid: parse_aid(raw)?,
        });
    }
    if raw.bytes().all(|b| b.is_ascii_digit()) && !raw.is_empty() {
        return Ok(Target::Video {
            aid: parse_aid(raw)?,
        });
    }
    if strip_ci_prefix(raw, "BV").is_some() {
        // 长度不对的 BV 号给出更具体的错误
        return Err(bvid_to_aid(raw).unwrap_err().into());
    }
    Err(Error::BadInput(raw.to_string()))
}

/// `space.bilibili.com/{mid}/channel/collectiondetail?sid=` 等四种形态。
fn resolve_space_list(url: &Url) -> Option<Target> {
    let host = url.host_str()?.to_ascii_lowercase();
    if host != "space.bilibili.com" && !host.ends_with(".space.bilibili.com") {
        return None;
    }
    let path = url.path();
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let query = |key: &str| -> Option<String> {
        url.query_pairs()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
    };

    // /{mid}/channel/collectiondetail?sid={sid}
    // /{mid}/channel/seriesdetail?sid={sid}
    if segments.len() == 3 && segments[1] == "channel" {
        let kind = match segments[2] {
            "collectiondetail" => ListKind::Collection,
            "seriesdetail" => ListKind::Series,
            _ => return None,
        };
        let sid = query("sid")?.parse::<u64>().ok()?;
        return Some(Target::List { biz_id: sid, kind });
    }

    // /{mid}/lists/{sid}?type=season|series
    if segments.len() == 3 && segments[1] == "lists" {
        let sid = segments[2].parse::<u64>().ok()?;
        let kind = match query("type").as_deref() {
            Some("season") => ListKind::Collection,
            Some("series") => ListKind::Series,
            _ => return None,
        };
        return Some(Target::List { biz_id: sid, kind });
    }

    None
}

/// `bilibili.com/medialist/play/...?business=space_collection|space_series&business_id={id}`
fn resolve_medialist(url: &Url) -> Option<Target> {
    let host = url.host_str()?.to_ascii_lowercase();
    if !(host == "bilibili.com" || host.ends_with(".bilibili.com")) {
        return None;
    }
    if !url.path().starts_with("/medialist/play") {
        return None;
    }
    let business = url
        .query_pairs()
        .find(|(k, _)| k == "business")
        .map(|(_, v)| v.into_owned())?;
    let kind = match business.as_str() {
        "space_collection" => ListKind::Collection,
        "space_series" => ListKind::Series,
        _ => return None,
    };
    let biz_id = url
        .query_pairs()
        .find(|(k, _)| k == "business_id")
        .map(|(_, v)| v.into_owned())?
        .parse::<u64>()
        .ok()?;
    Some(Target::List { biz_id, kind })
}

/// `bilibili.com/video/BV...` / `bilibili.com/video/av...`
fn resolve_video_path(url: &Url) -> Option<Target> {
    let host = url.host_str()?.to_ascii_lowercase();
    if !(host == "bilibili.com" || host.ends_with(".bilibili.com")) {
        return None;
    }
    let segments: Vec<&str> = url.path().split('/').filter(|s| !s.is_empty()).collect();
    let pos = segments.iter().position(|s| *s == "video")?;
    let id = segments.get(pos + 1)?;
    // 去掉可能的 .html 后缀
    let id = id.trim_end_matches(".html");
    if id.len() >= 2 && id[..2].eq_ignore_ascii_case("BV") {
        return bvid_to_aid(id).ok().map(|aid| Target::Video { aid });
    }
    if strip_ci_prefix(id, "av").is_some() || id.bytes().all(|b| b.is_ascii_digit()) {
        return parse_aid(id).ok().map(|aid| Target::Video { aid });
    }
    None
}

/// 跟随短链时逐跳校验：每一跳的 host 都必须在可信域内。
pub fn check_redirect(from: &str, to: &str) -> Result<Url> {
    let target = Url::parse(to).map_err(|_| Error::UntrustedRedirect(to.to_string()))?;
    let host = target.host_str().unwrap_or("");
    if !is_trusted_host(host) {
        return Err(Error::UntrustedRedirect(format!(
            "{from} → {}",
            target.as_str()
        )));
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_forms() {
        assert_eq!(
            resolve("BV1qt4y1X7TW").unwrap(),
            Target::Video { aid: 626_497_566 }
        );
        assert_eq!(resolve("av114514").unwrap(), Target::Video { aid: 114_514 });
        assert_eq!(resolve("114514").unwrap(), Target::Video { aid: 114_514 });
        assert_eq!(
            resolve(" AV114514 ").unwrap(),
            Target::Video { aid: 114_514 }
        );
    }

    #[test]
    fn bad_bare_forms() {
        assert!(matches!(
            resolve("BV1xx"),
            Err(Error::Bv(_)) | Err(Error::BadInput(_))
        ));
        assert!(matches!(resolve("av"), Err(Error::BadInput(_))));
        assert!(matches!(resolve("hello"), Err(Error::BadInput(_))));
        assert!(matches!(resolve(""), Err(Error::BadInput(_))));
    }

    #[test]
    fn video_links() {
        for input in [
            "https://www.bilibili.com/video/BV1qt4y1X7TW",
            "https://www.bilibili.com/video/BV1qt4y1X7TW/",
            "https://www.bilibili.com/video/BV1qt4y1X7TW/?spm_id_from=333.999",
            "https://www.bilibili.com/video/bv1qt4y1X7TW",
            "https://www.bilibili.com/video/av114514",
            "https://bilibili.com/video/av114514.html",
        ] {
            assert_eq!(
                resolve(input).unwrap(),
                Target::Video {
                    aid: if input.contains("av") {
                        114_514
                    } else {
                        626_497_566
                    }
                },
                "{input}"
            );
        }
    }

    #[test]
    fn collection_links() {
        assert_eq!(
            resolve("https://space.bilibili.com/23630128/channel/collectiondetail?sid=2045")
                .unwrap(),
            Target::List {
                biz_id: 2045,
                kind: ListKind::Collection
            }
        );
        assert_eq!(
            resolve("https://space.bilibili.com/23630128/channel/seriesdetail?sid=2045").unwrap(),
            Target::List {
                biz_id: 2045,
                kind: ListKind::Series
            }
        );
        assert_eq!(
            resolve("https://space.bilibili.com/123/lists/456?type=season").unwrap(),
            Target::List {
                biz_id: 456,
                kind: ListKind::Collection
            }
        );
        assert_eq!(
            resolve("https://space.bilibili.com/123/lists/456?type=series").unwrap(),
            Target::List {
                biz_id: 456,
                kind: ListKind::Series
            }
        );
    }

    #[test]
    fn collection_wins_over_space_page() {
        // 合集规则必须先于「UP 主空间」规则，否则 sid 会被丢掉
        assert_eq!(
            resolve("https://space.bilibili.com/123/channel/collectiondetail?sid=456").unwrap(),
            Target::List {
                biz_id: 456,
                kind: ListKind::Collection
            }
        );
    }

    #[test]
    fn space_page_without_sid_is_unrecognized() {
        assert!(matches!(
            resolve("https://space.bilibili.com/123/video"),
            Err(Error::BadInput(_))
        ));
        // 有 sid 但没有识别到具体形态
        assert!(matches!(
            resolve("https://space.bilibili.com/123/channel/collectiondetail"),
            Err(Error::BadInput(_))
        ));
    }

    #[test]
    fn medialist_links() {
        assert_eq!(
            resolve(
                "https://www.bilibili.com/medialist/play/123?business=space_collection&business_id=456"
            )
            .unwrap(),
            Target::List { biz_id: 456, kind: ListKind::Collection }
        );
        assert_eq!(
            resolve("https://www.bilibili.com/medialist/play/ml999?business=space_series&business_id=789")
                .unwrap(),
            Target::List { biz_id: 789, kind: ListKind::Series }
        );
        // business 不是合集/系列
        assert!(matches!(
            resolve(
                "https://www.bilibili.com/medialist/play/123?business=something&business_id=456"
            ),
            Err(Error::BadInput(_))
        ));
    }

    #[test]
    fn short_link_detection_uses_exact_host() {
        assert!(is_short_link("https://b23.tv/abc"));
        assert!(is_short_link("https://b23.tv/"));
        assert!(!is_short_link("https://evilb23.tv/abc"));
        assert!(!is_short_link("https://b23.tv.evil.com/abc"));
        assert!(!is_short_link(
            "https://www.bilibili.com/video/BV1qt4y1X7TW"
        ));
    }

    #[test]
    fn trusted_hosts() {
        assert!(is_trusted_host("b23.tv"));
        assert!(is_trusted_host("www.bilibili.com"));
        assert!(is_trusted_host("bilibili.com"));
        assert!(is_trusted_host("BILIBILI.COM."));
        assert!(!is_trusted_host("evilb23.tv"));
        assert!(!is_trusted_host("b23.tv.evil.com"));
        assert!(!is_trusted_host("evil.com"));
    }

    #[test]
    fn redirects_are_validated_per_hop() {
        assert!(
            check_redirect(
                "https://b23.tv/x",
                "https://www.bilibili.com/video/BV1qt4y1X7TW"
            )
            .is_ok()
        );
        assert!(check_redirect("https://b23.tv/x", "https://b23.tv/y").is_ok());
        let err = check_redirect("https://b23.tv/x", "http://127.0.0.1:8080/ssrf").unwrap_err();
        assert!(matches!(err, Error::UntrustedRedirect(_)), "{err}");
        // 可信域名的开放重定向也不能导向内网
        let err = check_redirect("https://b23.tv/x", "https://b23.tv.evil.com/y").unwrap_err();
        assert!(matches!(err, Error::UntrustedRedirect(_)), "{err}");
    }

    #[test]
    fn untrusted_hosts_are_not_videos() {
        assert!(matches!(
            resolve("https://evil.com/video/BV1qt4y1X7TW"),
            Err(Error::BadInput(_))
        ));
    }
}
