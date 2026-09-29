//! WBI 签名（纯函数，无网络）。
//!
//! `x/player/wbi/*` 系列接口要求查询串带 `w_rid`。密钥从 `nav` 接口的
//! `wbi_img.img_url` / `sub_url` 派生，进程启动时取一次并全局复用。

use md5::{Digest, Md5};

/// 密钥重排表。最大索引是 58，所以原始素材长度必须 >= 59。
const MIXIN_KEY_ENC_TAB: [usize; 32] = [
    46, 47, 18, 2, 53, 8, 23, 32, 15, 50, 10, 31, 58, 3, 45, 35, 27, 43, 5, 49, 33, 9, 42, 19, 29,
    28, 14, 39, 12, 38, 41, 13,
];

#[derive(Debug, Clone, Copy, thiserror::Error, PartialEq, Eq)]
pub enum WbiError {
    #[error("WBI 密钥素材过短：{0} 字节（至少需要 59）")]
    KeyMaterialTooShort(usize),
    #[error("WBI 密钥素材包含非 ASCII 字符")]
    KeyMaterialNotAscii,
}

/// 取 URL 中最后一个 '/' 之后、最后一个 '.' 之前的部分。
///
/// 上游给的是 `https://i0.hdslb.com/bfs/wbi/<32位hex>.png`，这里要的是那 32 位 hex。
fn file_stem(url: &str) -> &str {
    let s = url.rsplit('/').next().unwrap_or("");
    // 只切最后一个 '.'：文件名本身可能有多个点
    match s.rsplit_once('.') {
        Some((stem, _ext)) => stem,
        None => s,
    }
}

/// 从 `wbi_img` 的两个 URL 派生 mixin key。
pub fn mixin_key(img_url: &str, sub_url: &str) -> Result<String, WbiError> {
    let orig = format!("{}{}", file_stem(img_url), file_stem(sub_url));
    // 表中最大索引是 58，所以长度必须 >= 59。写成 58 会在 orig[58] 处越界。
    if orig.len() < 59 {
        return Err(WbiError::KeyMaterialTooShort(orig.len()));
    }
    let bytes = orig.as_bytes();
    if !bytes.is_ascii() {
        return Err(WbiError::KeyMaterialNotAscii);
    }
    Ok(MIXIN_KEY_ENC_TAB
        .iter()
        .map(|&i| bytes[i] as char)
        .collect())
}

/// 对原始查询串签名。
///
/// 输入是已经拼好的查询串（不含 `?`），保持构造顺序——**不要排序**：
/// 上游就是按客户端给的顺序重算 `md5(query + mixin_key)`。
pub fn sign(query: &str, mixin_key: &str) -> String {
    let w_rid = hex::encode(Md5::digest(format!("{query}{mixin_key}").as_bytes()));
    format!("{query}&w_rid={w_rid}")
}

/// 当前 Unix 秒。签名里的 `wts` 用它。
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const IMG: &str = "https://i0.hdslb.com/bfs/wbi/7cd084941338484aae1ad9425b84077c.png";
    const SUB: &str = "https://i0.hdslb.com/bfs/wbi/4932caff0ff746eab6f01bf08b70ac45.png";

    #[test]
    fn mixin_key_standard_vector() {
        assert_eq!(
            mixin_key(IMG, SUB).unwrap(),
            "ea1db124af3c7062474693fa704f4ff8"
        );
    }

    #[test]
    fn mixin_key_too_short_errors_without_panic() {
        // 58 字节素材：表里 58 这个索引会越界，必须报错而不是 panic
        let short = "a".repeat(58);
        assert_eq!(
            mixin_key(&short, ""),
            Err(WbiError::KeyMaterialTooShort(58))
        );
        // 59 字节刚好可用
        let ok = "a".repeat(59);
        assert!(mixin_key(&ok, "").is_ok());
    }

    #[test]
    fn mixin_key_rejects_non_ascii() {
        let s = "中".repeat(70);
        assert!(matches!(
            mixin_key(&s, ""),
            Err(WbiError::KeyMaterialNotAscii)
        ));
    }

    #[test]
    fn file_stem_takes_last_dot() {
        assert_eq!(file_stem("https://i0.hdslb.com/bfs/wbi/abc.png"), "abc");
        assert_eq!(file_stem("https://x/a.b.c.png"), "a.b.c");
        assert_eq!(file_stem("no-slash-no-dot"), "no-slash-no-dot");
        assert_eq!(file_stem("https://x/trailing/"), "");
    }

    #[test]
    fn sign_known_vector() {
        let key = "ea1db124af3c7062474693fa704f4ff8";
        let query = "foo=114&bar=514&zab=1919810&wts=1702204169";
        assert_eq!(
            sign(query, key),
            "foo=114&bar=514&zab=1919810&wts=1702204169&w_rid=e3f673073e5e79ab48a4d51633305d28"
        );
    }

    #[test]
    fn sign_is_lowercase_hex_32() {
        let signed = sign("a=1", "key");
        let rid = signed.rsplit("w_rid=").next().unwrap();
        assert_eq!(rid.len(), 32, "{rid}");
        assert!(rid.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(rid.to_lowercase(), rid, "必须是 32 位小写十六进制");
    }

    #[test]
    fn sign_preserves_parameter_order() {
        // 同样的参数换顺序必须产生不同的签名——即签名过程不排序
        let key = "ea1db124af3c7062474693fa704f4ff8";
        assert_ne!(sign("a=1&b=2", key), sign("b=2&a=1", key));
        // 且不改变输出里的参数顺序
        let out = sign("b=2&a=1", key);
        assert!(out.starts_with("b=2&a=1&w_rid="));
    }

    #[test]
    fn now_unix_is_plausible() {
        // 2020-01-01 之后、2100 之前
        assert!(now_unix() > 1_577_836_800);
        assert!(now_unix() < 4_102_444_800);
    }
}
