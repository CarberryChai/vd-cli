//! 前端指纹 cookie `buvid3`（纯函数，无网络）。
//!
//! B 站的取流接口要求请求带 `buvid3`。没有它时 `x/player/wbi/playurl` 大概率返回
//! `code: 0` + 只有 `data.v_voucher` 的响应（也就是风控），实测命中率约 4/5。
//! 可以先用 `x/frontend/finger/spi` 向服务器要一个，拿不到时本地按同样格式生成。

/// `buvid3` 的长度上限：`8-4-4-4-12` 的 32 位十六进制 + `33` + 3 位数字 + `infoc`。
pub const MAX_LEN: usize = 46;

/// Cookie 串里是否已经带了 `buvid3`。
pub fn has_buvid3(cookie: &str) -> bool {
    cookie
        .split(';')
        .any(|part| part.trim().starts_with("buvid3="))
}

/// 从 Cookie 串里取出 `buvid3` 的值。
pub fn get_buvid3(cookie: &str) -> Option<&str> {
    cookie
        .split(';')
        .map(str::trim)
        .find_map(|part| part.strip_prefix("buvid3="))
        .filter(|v| !v.is_empty())
}

/// 把 `buvid3` 并进 Cookie 串（已存在时不覆盖用户的）。
pub fn with_buvid3(cookie: &str, buvid3: &str) -> String {
    if has_buvid3(cookie) || buvid3.is_empty() {
        return cookie.trim_matches(';').trim().to_string();
    }
    let cookie = cookie.trim().trim_end_matches(';').trim();
    if cookie.is_empty() {
        format!("buvid3={buvid3}")
    } else {
        format!("{cookie}; buvid3={buvid3}")
    }
}

/// 本地按 B 站格式生成一个 `buvid3`。
///
/// 形如 `E0C0AF89-703C-ECB5-4A2C-D9B4E732954033250infoc`。
pub fn generate() -> String {
    const HEX: &[u8] = b"0123456789ABCDEF";
    let mut rng = Rng::from_time();
    let mut hex = String::with_capacity(32);
    for _ in 0..32 {
        hex.push(HEX[(rng.next() % 16) as usize] as char);
    }
    let formatted = format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    );
    format!("{formatted}33{:03}infoc", rng.next() % 1000)
}

/// 极小的 xorshift PRNG。只为生成一个看起来随机的指纹，不需要密码学强度。
struct Rng(u64);

impl Rng {
    fn from_time() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x2545_F491_4F6C_DD1D);
        // 再混入进程 id 与地址随机化，避免同一纳秒起多个进程时撞同一个指纹
        let mix = std::process::id() as u64
            ^ (nanos.rotate_left(17))
            ^ (std::ptr::from_ref(&nanos) as u64).rotate_left(31);
        Rng(mix | 1)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_matches_bilibili_shape() {
        for _ in 0..50 {
            let id = generate();
            assert_eq!(id.len(), MAX_LEN, "{id}");
            assert!(id.ends_with("infoc"), "{id}");
            let head = id.trim_end_matches("infoc");
            // 32 位十六进制 + 4 个分隔 + 5 位数字后缀
            let hex_part = &head[..head.len() - 5];
            assert_eq!(hex_part.len(), 36, "{id}");
            assert!(
                hex_part.chars().all(|c| c.is_ascii_hexdigit() || c == '-'),
                "{id}"
            );
            let digits = &head[head.len() - 5..];
            assert!(digits.starts_with("33"), "{id}");
            assert!(digits[2..].chars().all(|c| c.is_ascii_digit()), "{id}");
        }
    }

    #[test]
    fn generate_is_not_constant() {
        let a = generate();
        let b = generate();
        assert_ne!(a, b, "连续两次生成的指纹不应相同");
    }

    #[test]
    fn detects_existing_buvid3() {
        assert!(has_buvid3("SESSDATA=x; buvid3=abc"));
        assert!(has_buvid3("buvid3=abc"));
        assert!(has_buvid3(" buvid3=abc ; bili_jct=y"));
        assert!(!has_buvid3("SESSDATA=x; bili_jct=y"));
        assert!(!has_buvid3(""));
        // 前缀相同但不是同一个 key
        assert!(!has_buvid3("buvid3X=abc"));
        // 子串出现在值里也不算
        assert!(!has_buvid3("foo=abuvid3=1"));
    }

    #[test]
    fn get_buvid3_value() {
        assert_eq!(
            get_buvid3("SESSDATA=x; buvid3=abc; bili_jct=y"),
            Some("abc")
        );
        assert_eq!(get_buvid3("buvid3="), None);
        assert_eq!(get_buvid3("SESSDATA=x"), None);
    }

    #[test]
    fn with_buvid3_appends_to_existing_cookie() {
        assert_eq!(
            with_buvid3("SESSDATA=x; bili_jct=y", "NEW"),
            "SESSDATA=x; bili_jct=y; buvid3=NEW"
        );
        assert_eq!(with_buvid3("SESSDATA=x;", "NEW"), "SESSDATA=x; buvid3=NEW");
    }

    #[test]
    fn with_buvid3_works_on_empty_cookie() {
        assert_eq!(with_buvid3("", "NEW"), "buvid3=NEW");
        assert_eq!(with_buvid3("   ", "NEW"), "buvid3=NEW");
    }

    #[test]
    fn with_buvid3_never_overrides_user_value() {
        // 用户自己带了 buvid3 就用用户的，不要顶掉
        assert_eq!(with_buvid3("buvid3=USER", "NEW"), "buvid3=USER");
        assert_eq!(
            with_buvid3("SESSDATA=x; buvid3=USER", "NEW"),
            "SESSDATA=x; buvid3=USER"
        );
    }

    #[test]
    fn with_buvid3_is_idempotent() {
        let once = with_buvid3("SESSDATA=x", "NEW");
        assert_eq!(with_buvid3(&once, "OTHER"), once);
    }
}
