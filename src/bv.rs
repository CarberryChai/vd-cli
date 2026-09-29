//! BV 号 ↔ av 号互转（纯函数，无网络）。
//!
//! BV 号的后 10 位是 av 号的 base58 编码，编码前后会把第 3/9 位、第 4/7 位
//! 两对字符交换。参考 bilibili-API-collect 的 BV 号算法。

const BV_TABLE: &[u8; 58] = b"FcwAPNKTMug3GV5Lj7EJnHpWsx4tb8haYeviqBz6rkCy12mUSDQX9RdoZf";
const XOR_CODE: u64 = 23_442_827_791_579;
const MASK_CODE: u64 = (1u64 << 51) - 1;
const MAX_AID: u64 = 1u64 << 51;
const BASE: u64 = 58;
const BV_LEN: usize = 12;
/// 参与编码的位置对：(3,9) 与 (4,7)，编码/解码时各交换一次即可还原。
const SWAP: [(usize, usize); 2] = [(3, 9), (4, 7)];

#[derive(Debug, Clone, Copy, thiserror::Error, PartialEq, Eq)]
pub enum BvError {
    #[error("BV 号为空")]
    Empty,
    #[error("不是合法的 BV 号：缺少 BV 前缀")]
    BadPrefix,
    #[error("BV 号必须是 12 位，实际 {0} 位")]
    BadLength(usize),
    #[error("BV 号包含非法字符 '{0}'")]
    BadChar(char),
    #[error("av 号必须是不含前导符号的十进制数字")]
    BadAid,
}

/// BV 号 → av 号。
pub fn bvid_to_aid(bvid: &str) -> Result<u64, BvError> {
    if bvid.is_empty() {
        return Err(BvError::Empty);
    }
    let mut chars: Vec<char> = bvid.chars().collect();
    if chars.len() != BV_LEN {
        return Err(BvError::BadLength(chars.len()));
    }
    // 前缀大小写不敏感（BV1xx / bv1xx）；后面的 base58 字符仍然大小写敏感
    if !chars[0].eq_ignore_ascii_case(&'B') || !chars[1].eq_ignore_ascii_case(&'V') {
        return Err(BvError::BadPrefix);
    }
    for (a, b) in SWAP {
        chars.swap(a, b);
    }
    let mut acc: u64 = 0;
    for &c in &chars[3..] {
        let idx = BV_TABLE
            .iter()
            .position(|&t| t as char == c)
            .ok_or(BvError::BadChar(c))?;
        acc = acc * BASE + idx as u64;
    }
    Ok((acc & MASK_CODE) ^ XOR_CODE)
}

/// av 号 → BV 号。
pub fn aid_to_bvid(aid: u64) -> String {
    let mut chars: Vec<u8> = b"BV1000000000".to_vec();
    let mut t = (MAX_AID | aid) ^ XOR_CODE;
    for i in (3..BV_LEN).rev() {
        chars[i] = BV_TABLE[(t % BASE) as usize];
        t /= BASE;
    }
    for (a, b) in SWAP {
        chars.swap(a, b);
    }
    // 表里全是 ASCII，不可能失败
    String::from_utf8(chars).expect("BV 字符表是 ASCII")
}

/// 解析 `av123` / `123` 形式的 av 号。
pub fn parse_aid(s: &str) -> Result<u64, BvError> {
    let digits = s
        .strip_prefix("av")
        .or_else(|| s.strip_prefix("AV"))
        .unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(BvError::BadAid);
    }
    digits.parse::<u64>().map_err(|_| BvError::BadAid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vector() {
        // spec §12.1 指定的已知向量
        assert_eq!(bvid_to_aid("BV1qt4y1X7TW").unwrap(), 626_497_566);
    }

    #[test]
    fn roundtrip_aid_to_bvid_to_aid() {
        for aid in [
            0u64,
            1,
            2,
            170_001,
            114_514,
            455_017_605,
            99_999_999,
            626_497_566,
        ] {
            let bvid = aid_to_bvid(aid);
            assert_eq!(bvid.len(), 12, "{bvid}");
            assert!(bvid.starts_with("BV1"), "{bvid}");
            assert_eq!(bvid_to_aid(&bvid).unwrap(), aid, "{bvid}");
        }
    }

    #[test]
    fn known_small_vectors() {
        assert_eq!(aid_to_bvid(1), "BV1xx411c7mQ");
        assert_eq!(aid_to_bvid(0), "BV1xx411c7mX");
        assert_eq!(bvid_to_aid("BV17x411w7KC").unwrap(), 170_001);
    }

    #[test]
    fn rejects_bad_length() {
        assert_eq!(bvid_to_aid("BV1qt4y1X7T"), Err(BvError::BadLength(11)));
        assert_eq!(bvid_to_aid("BV1qt4y1X7TWX"), Err(BvError::BadLength(13)));
        assert_eq!(bvid_to_aid(""), Err(BvError::Empty));
    }

    #[test]
    fn rejects_bad_prefix() {
        assert_eq!(bvid_to_aid("AV1qt4y1X7TW"), Err(BvError::BadPrefix));
    }

    #[test]
    fn rejects_bad_chars() {
        // 'l' / '0' / 'O' / 'I' 都不在 base58 表里
        assert_eq!(bvid_to_aid("BV1qt4y1XlTW"), Err(BvError::BadChar('l')));
        assert_eq!(bvid_to_aid("BV1qt4y1X0TW"), Err(BvError::BadChar('0')));
        assert_eq!(bvid_to_aid("BV1qt4y1X中TW"), Err(BvError::BadChar('中')));
    }

    #[test]
    fn length_counts_chars_not_bytes() {
        // 多字节字符按「字符数」计长度，避免按字节切出半个字符
        assert_eq!(bvid_to_aid("BV1qt4y1X7中"), Err(BvError::BadLength(11)));
        // 长度对了但字符不在 base58 表里
        assert_eq!(bvid_to_aid("BV1qt4y1X7T中"), Err(BvError::BadChar('中')));
    }

    #[test]
    fn lowercase_prefix_is_accepted() {
        assert_eq!(bvid_to_aid("bv1qt4y1X7TW").unwrap(), 626_497_566);
        // 但 base58 字符大小写敏感：换成小写 x 就不是原视频了
        assert_ne!(bvid_to_aid("bv1qt4y1x7TW").unwrap(), 626_497_566);
    }

    #[test]
    fn parse_aid_forms() {
        assert_eq!(parse_aid("114514").unwrap(), 114_514);
        assert_eq!(parse_aid("av114514").unwrap(), 114_514);
        assert_eq!(parse_aid("av0").unwrap(), 0);
        assert!(parse_aid("av").is_err());
        assert!(parse_aid("").is_err());
        assert!(parse_aid("114514x").is_err());
        assert!(parse_aid("-1").is_err());
    }
}
