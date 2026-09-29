//! 从本机浏览器读取 Cookie（`--cookies-from-browser`）。
//!
//! 目标是省掉「手动从开发者工具复制 Cookie」这一步。Chromium 系浏览器把 Cookie
//! 存在 SQLite 里，值用 `v10` 前缀的 AES-CBC 加密，密钥来自系统钥匙串（macOS）
//! 或本地固定 key（Linux 且未启用 keyring 时）。
//!
//! 注意 macOS 的限制：`~/Library/Application Support/Google/Chrome` 受 TCC 保护，
//! 进程需要「完全磁盘访问」才能读；没有权限时这里会给出可读的报错与操作指引。

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{Error, Result};

/// 支持自动读取的浏览器。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Browser {
    Chrome,
    Chromium,
    Brave,
    Edge,
    Vivaldi,
    Opera,
    Firefox,
    Safari,
}

impl Browser {
    pub fn parse(name: &str) -> Option<Browser> {
        match name.trim().to_ascii_lowercase().as_str() {
            "chrome" | "google-chrome" | "googlechrome" => Some(Browser::Chrome),
            "chromium" => Some(Browser::Chromium),
            "brave" | "brave-browser" => Some(Browser::Brave),
            "edge" | "microsoft-edge" | "msedge" => Some(Browser::Edge),
            "vivaldi" => Some(Browser::Vivaldi),
            "opera" => Some(Browser::Opera),
            "firefox" => Some(Browser::Firefox),
            "safari" => Some(Browser::Safari),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Browser::Chrome => "chrome",
            Browser::Chromium => "chromium",
            Browser::Brave => "brave",
            Browser::Edge => "edge",
            Browser::Vivaldi => "vivaldi",
            Browser::Opera => "opera",
            Browser::Firefox => "firefox",
            Browser::Safari => "safari",
        }
    }

    /// 本机是否装了 / 有配置目录。
    pub fn is_present(self) -> bool {
        self.profile_root().is_some_and(|p| p.exists())
    }

    /// 浏览器配置目录。
    pub fn profile_root(self) -> Option<PathBuf> {
        let home = dirs_home()?;
        let support = home.join("Library/Application Support");
        Some(match self {
            Browser::Chrome => support.join("Google/Chrome"),
            Browser::Chromium => support.join("Chromium"),
            Browser::Brave => support.join("BraveSoftware/Brave-Browser"),
            Browser::Edge => support.join("Microsoft Edge"),
            Browser::Vivaldi => support.join("Vivaldi"),
            Browser::Opera => support.join("com.operasoftware.Opera"),
            Browser::Firefox => support.join("Firefox"),
            // Safari 的 Cookie 存在 ~/Library/Cookies（二进制格式）与容器里
            Browser::Safari => home.join("Library/Cookies"),
        })
    }

    /// 钥匙串里那条加密密钥的标签。
    fn keychain_label(self) -> Option<&'static str> {
        match self {
            Browser::Chrome => Some("Chrome Safe Storage"),
            Browser::Chromium => Some("Chromium Safe Storage"),
            Browser::Brave => Some("Brave Safe Storage"),
            Browser::Edge => Some("Microsoft Edge Safe Storage"),
            Browser::Vivaldi => Some("Vivaldi Safe Storage"),
            Browser::Opera => Some("Opera Safe Storage"),
            _ => None,
        }
    }

    /// Linux 上未启用 keyring 时用的固定口令。
    fn linux_fallback_password(self) -> Option<&'static str> {
        match self {
            Browser::Chrome => Some("peanuts"),
            Browser::Chromium => Some("peanuts"),
            Browser::Brave => Some("peanuts"),
            Browser::Edge => Some("peanuts"),
            Browser::Vivaldi => Some("peanuts"),
            Browser::Opera => Some("peanuts"),
            _ => None,
        }
    }
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// 自动模式下的浏览器优先顺序。
///
/// Chrome 排第一（用户请求的默认），后面是常见的 Chromium 系。
/// Firefox / Safari 不在列表里——它们暂不支持自动读取。
pub const AUTO_ORDER: &[Browser] = &[
    Browser::Chrome,
    Browser::Edge,
    Browser::Brave,
    Browser::Vivaldi,
    Browser::Chromium,
    Browser::Opera,
];

/// 自动挑一个能读出 B 站 Cookie 的浏览器。
///
/// 按 `AUTO_ORDER` 顺序尝试，第一个成功的就是结果。全都读不出来时返回**优先级最高
/// 的那个错误**（也就是列表里第一个装了却读不出来的浏览器的错误）——用户最可能关心
/// 的正是它，报最后一个（往往是个没登录过的浏览器）只会误导。
pub fn load_auto(profile: Option<&str>) -> Result<(Browser, String)> {
    let mut first_error: Option<(Browser, Error)> = None;
    let mut tried = 0usize;
    for &browser in AUTO_ORDER {
        // 只看装了的浏览器，避免无谓的系统调用
        let Some(root) = browser.profile_root() else {
            continue;
        };
        if !root.exists() {
            continue;
        }
        tried += 1;
        match load_from_root(browser, &root, profile) {
            Ok(cookie) => {
                tracing::debug!("自动选中 {} 的 Cookie", browser.as_str());
                return Ok((browser, cookie));
            }
            Err(e) => {
                tracing::debug!("{} 读 Cookie 失败: {e}", browser.as_str());
                first_error.get_or_insert((browser, e));
            }
        }
    }
    match first_error {
        Some((browser, e)) => {
            tracing::debug!("自动模式下试过 {tried} 个浏览器，都没读出来");
            Err(Error::BrowserCookie(format!(
                "{} 读不到 Cookie：{e}",
                browser.as_str()
            )))
        }
        None => Err(Error::BrowserCookie(
            "本机没有检测到可读取 Cookie 的浏览器（支持 Chrome / Chromium / Brave / \
             Edge / Vivaldi / Opera）；请用 --cookie 手动传入"
                .into(),
        )),
    }
}

/// 从浏览器里取出 B 站相关的 Cookie，拼成请求头用的字符串。
pub fn load(browser: Browser, profile: Option<&str>) -> Result<String> {
    let root = browser
        .profile_root()
        .ok_or_else(|| Error::BrowserCookie(format!("找不到 {} 的配置目录", browser.as_str())))?;
    load_from_root(browser, &root, profile).map_err(|e| match e {
        // 「哪个浏览器」要出现在错误里，否则单看消息不知道是谁的问题。
        // 用前缀判断避免重复套娃。
        Error::BrowserCookie(msg) if !msg.starts_with(browser.as_str()) => {
            Error::BrowserCookie(format!("{} 读 Cookie 失败：{msg}", browser.as_str()))
        }
        other => other,
    })
}

/// 同 `load`，但配置目录由调用方给定（测试用）。密钥仍从钥匙串取。
pub fn load_from_root(browser: Browser, root: &Path, profile: Option<&str>) -> Result<String> {
    load_with_key(browser, root, profile, None)
}

/// 同上，但可以注入解密密钥。
///
/// `key` 为 `None` 时才去问钥匙串——测试里传入固定密钥，避免依赖用户授权。
pub fn load_with_key(
    browser: Browser,
    root: &Path,
    profile: Option<&str>,
    key: Option<&[u8]>,
) -> Result<String> {
    if matches!(browser, Browser::Firefox) {
        return Err(Error::BrowserCookie(
            "暂不支持从 Firefox 读取（它的 Cookie 存在 cookies.sqlite 里且加密方式不同）；\
             请用 --cookie 手动传入，或改用 Chromium 系浏览器"
                .into(),
        ));
    }
    if matches!(browser, Browser::Safari) {
        return Err(Error::BrowserCookie(
            "暂不支持从 Safari 读取（它的 Cookie 是二进制格式且受 TCC 保护）；\
             请用 --cookie 手动传入"
                .into(),
        ));
    }

    let cookie_db = find_cookie_db(root, profile)?;
    tracing::debug!("读取 Cookie 数据库: {}", cookie_db.display());

    let mut pairs = read_chromium_cookies(&cookie_db, browser, key)?;
    if pairs.is_empty() {
        // 分两种原因给不同的话术：目录读不到 vs 真的没登录
        let dir_readable = std::fs::read_dir(root).is_ok();
        if !dir_readable {
            return Err(cookie_read_error(
                root,
                std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            ));
        }
        let has_login = session_like_names(&cookie_db);
        let extra = if has_login {
            "  → 数据库里有登录态 Cookie，但域名不是 bilibili.com（可能登录的是其它站）"
        } else {
            "  → 数据库里没有任何登录态 Cookie，请先在浏览器里登录 bilibili.com"
        };
        return Err(Error::BrowserCookie(format!(
            "{} 里没有读到 bilibili.com 的 Cookie。请确认：\n{extra}\n  \
             → 用的是正确的 profile（当前看的是 {}）\n  \
             → 如果配置了多个 profile，用 --browser-profile 指定",
            browser.as_str(),
            cookie_db.display()
        )));
    }
    // 同一名字可能有多个域（.bilibili.com 与 www.bilibili.com），保留先出现的
    let mut seen = std::collections::HashSet::new();
    pairs.retain(|(name, _)| seen.insert(name.clone()));
    Ok(pairs
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("; "))
}

/// 找 Cookie 数据库：优先用户指定的 profile，否则取「最近活动」的那个。
fn find_cookie_db(root: &Path, profile: Option<&str>) -> Result<PathBuf> {
    let candidates = if let Some(name) = profile {
        vec![root.join(name)]
    } else {
        // 找出所有 profile：Default + Profile N + 浏览器自带的其它命名
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(root)
            .map_err(|e| cookie_read_error(root, e))?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .filter(|p| {
                let name = p.file_name().unwrap_or_default().to_string_lossy();
                name == "Default" || name.starts_with("Profile ")
            })
            .collect();
        // 按 Cookie 数据库的修改时间倒序：用户最近在用的 profile 最可能是登录着的
        dirs.sort_by_key(|p| {
            std::cmp::Reverse(
                std::fs::metadata(p.join("Cookies"))
                    .and_then(|m| m.modified())
                    .ok(),
            )
        });
        dirs
    };

    for dir in candidates {
        let db = dir.join("Cookies");
        if db.exists() {
            return Ok(db);
        }
    }
    Err(Error::BrowserCookie(format!(
        "{} 下没找到 Cookies 数据库（profile: {}）",
        root.display(),
        profile.unwrap_or("自动")
    )))
}

/// 库里是否存在登录态 Cookie 的名字（只看名字，不解密）。
fn session_like_names(db: &Path) -> bool {
    let Ok(conn) =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        return false;
    };
    let Ok(mut stmt) = conn
        .prepare("SELECT COUNT(*) FROM cookies WHERE name IN ('SESSDATA','bili_jct','DedeUserID')")
    else {
        return false;
    };
    stmt.query_row([], |row| row.get::<_, i64>(0))
        .map(|n| n > 0)
        .unwrap_or(false)
}

/// 读 Chromium 系 Cookie 数据库。
fn read_chromium_cookies(
    db: &Path,
    browser: Browser,
    injected_key: Option<&[u8]>,
) -> Result<Vec<(String, String)>> {
    // 复制一份再读：Chrome 运行时会对原库加锁，直接读可能失败
    let tmp = temp_copy(db)?;
    let conn =
        rusqlite::Connection::open_with_flags(&tmp, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| Error::BrowserCookie(format!("打开 Cookie 数据库失败: {e}")))?;

    let mut stmt = conn
        .prepare(
            "SELECT name, value, encrypted_value FROM cookies WHERE host_key LIKE '%bilibili.com'",
        )
        .map_err(|e| Error::BrowserCookie(format!("查询 Cookie 表失败: {e}")))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Vec<u8>>(2)?,
            ))
        })
        .map_err(|e| Error::BrowserCookie(format!("读取 Cookie 行失败: {e}")))?;

    let mut key: Option<Vec<u8>> = None;
    let mut out = Vec::new();
    let mut unsupported: Option<String> = None;
    for row in rows {
        let (name, plain, encrypted) =
            row.map_err(|e| Error::BrowserCookie(format!("解析 Cookie 行失败: {e}")))?;
        if !plain.is_empty() {
            out.push((name, plain));
            continue;
        }
        if encrypted.is_empty() {
            continue;
        }
        // 先看格式：遇到解不了的版本就别白折腾钥匙串（可能弹框）
        if let ValueFormat::Unsupported(prefix) = value_format(&encrypted) {
            tracing::debug!("Cookie {name} 用了不支持的加密格式 {prefix}");
            unsupported.get_or_insert(prefix);
            continue;
        }
        // 懒取密钥：只有真的遇到 v10 密文时才去问钥匙串（可能弹窗）
        if key.is_none() {
            key = Some(match injected_key {
                Some(k) => k.to_vec(),
                None => derived_key(browser)?,
            });
        }
        match decrypt_chromium_value(&encrypted, key.as_deref().unwrap_or_default()) {
            Some(value) => out.push((name, value)),
            None => tracing::debug!("Cookie {name} 解密失败，跳过"),
        }
    }
    cleanup(&tmp);

    if out.is_empty()
        && let Some(prefix) = unsupported
    {
        return Err(Error::BrowserCookie(format!(
            "{} 的 Cookie 用了我们解不了的加密格式（{prefix}）。\
             这通常是新版浏览器启用了更严格的加密；请用 --cookie 手动传入",
            browser.as_str()
        )));
    }
    Ok(out)
}

/// 复制到一个临时文件，避免与运行中的浏览器争锁。
fn temp_copy(db: &Path) -> Result<PathBuf> {
    let dest = std::env::temp_dir().join(format!(
        "vd-cookies-{}-{}.sqlite",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::copy(db, &dest).map_err(|e| cookie_read_error(db, e))?;
    Ok(dest)
}

fn cleanup(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// 读不到 Cookie 库时，八成是 macOS 的 TCC 权限问题，给可操作的提示。
fn cookie_read_error(path: &Path, err: std::io::Error) -> Error {
    let hint = if err.kind() == std::io::ErrorKind::PermissionDenied {
        "\n  → macOS 会保护浏览器数据目录。请给运行 vd 的程序（终端 / Codex）开启\
         「完全磁盘访问」：\n     系统设置 → 隐私与安全性 → 完全磁盘访问权限 → 勾选你的终端 → 完全退出并重开\n  \
         → 不想改权限的话，也可以用 --cookie 手动传入"
            .to_string()
    } else {
        String::new()
    };
    Error::BrowserCookie(format!("读取 {} 失败: {err}{hint}", path.display()))
}

// ---------------------------------------------------------------- 解密

/// 拿到 Chromium 的 AES 密钥（PBKDF2-SHA1, 1003 轮, salt "saltysalt", 16 字节）。
fn derived_key(browser: Browser) -> Result<Vec<u8>> {
    let password = keychain_password(browser)?;
    let mut key = [0u8; 16];
    pbkdf2::pbkdf2_hmac::<sha1::Sha1>(password.as_bytes(), b"saltysalt", 1003, &mut key);
    Ok(key.to_vec())
}

/// 从系统钥匙串取口令。macOS 上可能弹一次授权框。
fn keychain_password(browser: Browser) -> Result<String> {
    let Some(label) = browser.keychain_label() else {
        return Ok(browser.linux_fallback_password().unwrap_or("").to_string());
    };

    if cfg!(target_os = "macos") {
        // 只取口令数据；-w 输出到 stdout
        let out = Command::new("/usr/bin/security")
            .args(["find-generic-password", "-w", "-s", label])
            .output();
        match out {
            Ok(out) if out.status.success() => {
                let pw = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if pw.is_empty() {
                    return Err(Error::BrowserCookie(format!(
                        "钥匙串里 {label} 的密码是空的。请确认浏览器已在本机启动过至少一次"
                    )));
                }
                Ok(pw)
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
                // 用户拒绝授权时 security 会失败
                Err(Error::BrowserCookie(format!(
                    "从钥匙串读取 {label} 失败: {stderr}\n  → 弹出的授权框需要选择「允许」，\
                     否则无法解密 Cookie\n  → 或者用 --cookie 手动传入"
                )))
            }
            Err(e) => Err(Error::BrowserCookie(format!("执行 security 命令失败: {e}"))),
        }
    } else {
        // Linux：未启用 keyring 时 Chromium 用固定口令
        Ok(browser.linux_fallback_password().unwrap_or("").to_string())
    }
}

/// 加密前缀的解析结果。
#[derive(Debug, PartialEq, Eq)]
pub enum ValueFormat {
    /// 明文（没有前缀）。
    Plaintext,
    /// `v10` / `v11`：AES-128-CBC，我们可以解。
    V10,
    /// 其它 `vNN`（如新版 Windows 的 app-bound 加密 `v20`）——解不了。
    Unsupported(String),
}

/// 识别 Cookie 值的加密格式。
pub fn value_format(encrypted: &[u8]) -> ValueFormat {
    if encrypted.len() >= 3
        && encrypted[0] == b'v'
        && encrypted[1].is_ascii_digit()
        && encrypted[2].is_ascii_digit()
    {
        return match &encrypted[..3] {
            b"v10" | b"v11" => ValueFormat::V10,
            other => ValueFormat::Unsupported(String::from_utf8_lossy(other).to_string()),
        };
    }
    ValueFormat::Plaintext
}

/// 解密 Chromium 的 Cookie 值。
///
/// 格式：`v10`/`v11` 前缀 + AES-128-CBC，IV 全是空格。解密后前面还有 32 字节的
/// SHA256 摘要（新版本还会再跟一段 32 字节的域名绑定哈希），要按需剥掉。
pub fn decrypt_chromium_value(encrypted: &[u8], key: &[u8]) -> Option<String> {
    if key.is_empty() {
        return None;
    }
    if value_format(encrypted) != ValueFormat::V10 {
        return None;
    }

    use aes::cipher::{BlockDecryptMut, KeyIvInit};
    type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

    let iv = [b' '; 16];
    let body = &encrypted[3..];
    // CBC 要求整块，截掉不足一块的尾巴
    let usable = body.len() - (body.len() % 16);
    if usable == 0 {
        return None;
    }
    let mut buf = body[..usable].to_vec();
    let decrypted = Aes128CbcDec::new_from_slices(key, &iv)
        .ok()?
        .decrypt_padded_mut::<aes::cipher::block_padding::Pkcs7>(&mut buf)
        .ok()?;

    // 剥掉元数据前缀。老版本是 32 字节摘要；新版本会再跟 32 字节域名哈希。
    // 用「剥掉之后是不是可打印文本」来判断该剥 32 还是 64 字节。
    for skip in [32usize, 64] {
        if decrypted.len() > skip
            && let Some(text) = as_cookie_text(&decrypted[skip..])
        {
            return Some(text);
        }
    }
    // 兜底：整段就是值（极少见）
    as_cookie_text(decrypted)
}

/// 把字节当 Cookie 值来解释：必须是可打印 ASCII 且非空。
///
/// Cookie 值只可能是可见 ASCII（B 站的是 base64/十六进制/百分号编码），
/// 所以这个判据足够区分「剥对了」和「还留着一截二进制哈希」。
fn as_cookie_text(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() {
        return None;
    }
    if !bytes.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
        return None;
    }
    String::from_utf8(bytes.to_vec()).ok()
}

// ---------------------------------------------------------------- Firefox（占位）

/// Firefox 的 Cookie 库路径（当前不支持读取，仅用于 `--list-browsers` 展示）。
pub fn firefox_cookie_paths(browser: Browser) -> Vec<PathBuf> {
    let Some(root) = browser.profile_root() else {
        return Vec::new();
    };
    std::fs::read_dir(root.join("Profiles"))
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path().join("cookies.sqlite"))
                .filter(|p| p.exists())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_parsing() {
        assert_eq!(Browser::parse("chrome"), Some(Browser::Chrome));
        assert_eq!(Browser::parse("  Chrome "), Some(Browser::Chrome));
        assert_eq!(Browser::parse("google-chrome"), Some(Browser::Chrome));
        assert_eq!(Browser::parse("Brave"), Some(Browser::Brave));
        assert_eq!(Browser::parse("microsoft-edge"), Some(Browser::Edge));
        assert_eq!(Browser::parse("firefox"), Some(Browser::Firefox));
        assert_eq!(Browser::parse("netscape"), None);
        for name in [
            "chrome", "chromium", "brave", "edge", "vivaldi", "opera", "firefox", "safari",
        ] {
            let b = Browser::parse(name).unwrap();
            assert_eq!(Browser::parse(b.as_str()), Some(b));
        }
    }

    #[test]
    fn only_chromium_family_has_keychain_label() {
        assert_eq!(
            Browser::Chrome.keychain_label(),
            Some("Chrome Safe Storage")
        );
        assert_eq!(Browser::Brave.keychain_label(), Some("Brave Safe Storage"));
        assert_eq!(Browser::Firefox.keychain_label(), None);
    }

    #[test]
    fn unsupported_browsers_give_actionable_errors() {
        let err = load(Browser::Firefox, None).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Firefox"), "{msg}");
        assert!(msg.contains("--cookie"), "要告诉用户退路: {msg}");

        let err = load(Browser::Safari, None).unwrap_err();
        assert!(err.to_string().contains("--cookie"), "{err}");
    }

    #[test]
    fn decrypts_a_round_trip_value() {
        // 自己加密再解密，验证 v10 + AES-128-CBC + 32 字节摘要前缀这条链路
        use aes::cipher::{BlockEncryptMut, KeyIvInit};
        type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;

        let key = b"0123456789abcdef"; // 16 字节
        let cookie_value = "SESSDATA_like_value_123";
        let mut plain = vec![0xABu8; 32]; // 摘要前缀占位
        plain.extend_from_slice(cookie_value.as_bytes());

        let iv = [b' '; 16];
        // 手工做 PKCS7 填充，与解密端对称
        let pad = 16 - (plain.len() % 16);
        let mut buf = plain.clone();
        buf.extend(std::iter::repeat_n(pad as u8, pad));
        let n = buf.len();
        let encrypted = Aes128CbcEnc::new_from_slices(key, &iv)
            .unwrap()
            .encrypt_padded_mut::<aes::cipher::block_padding::NoPadding>(&mut buf, n)
            .unwrap()
            .to_vec();

        let mut blob = b"v10".to_vec();
        blob.extend_from_slice(&encrypted);
        assert_eq!(
            decrypt_chromium_value(&blob, key).as_deref(),
            Some(cookie_value)
        );
    }

    #[test]
    fn decrypt_rejects_garbage() {
        assert_eq!(decrypt_chromium_value(&[], b"0123456789abcdef"), None);
        assert_eq!(decrypt_chromium_value(b"v10", b"0123456789abcdef"), None);
        // 错误的 key 会破坏 padding
        assert_eq!(
            decrypt_chromium_value(b"v10xxxxxxxxxxxxxxxx", b"0123456789abcdef"),
            None
        );
        // 没有 key 时不硬解
        assert_eq!(decrypt_chromium_value(b"v10xxxxxxxxxxxxxxxx", b""), None);
    }

    #[test]
    fn value_format_detection() {
        assert_eq!(value_format(b"v10abc"), ValueFormat::V10);
        assert_eq!(value_format(b"v11abc"), ValueFormat::V10);
        assert_eq!(
            value_format(b"v20abc"),
            ValueFormat::Unsupported("v20".into())
        );
        assert_eq!(value_format(b"plain"), ValueFormat::Plaintext);
        assert_eq!(value_format(b""), ValueFormat::Plaintext);
        assert_eq!(value_format(b"v1"), ValueFormat::Plaintext);
        // 只有 v + 两位数字才当作版本前缀
        assert_eq!(value_format(b"vault"), ValueFormat::Plaintext);
    }

    #[test]
    fn encrypts_and_decrypts_with_extra_domain_hash() {
        // 新版 Chrome 会在摘要后再跟 32 字节域名哈希；两段都要能剥掉
        use aes::cipher::{BlockEncryptMut, KeyIvInit};
        type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;
        let key = b"0123456789abcdef";
        let value = "SESSDATA_with_domain_hash";
        let mut plain = vec![0xABu8; 32]; // 摘要
        plain.extend(std::iter::repeat_n(0xCDu8, 32)); // 域名哈希
        plain.extend_from_slice(value.as_bytes());
        let pad = 16 - (plain.len() % 16);
        plain.extend(std::iter::repeat_n(pad as u8, pad));
        let n = plain.len();
        let blob_body = Aes128CbcEnc::new_from_slices(key, &[b' '; 16])
            .unwrap()
            .encrypt_padded_mut::<aes::cipher::block_padding::NoPadding>(&mut plain, n)
            .unwrap()
            .to_vec();
        let mut blob = b"v10".to_vec();
        blob.extend_from_slice(&blob_body);
        assert_eq!(
            decrypt_chromium_value(&blob, key).as_deref(),
            Some(value),
            "应当剥掉 32+32 字节元数据"
        );
    }

    #[test]
    fn unsupported_version_does_not_decrypt() {
        // v20（新版 Windows 的 app-bound 加密）我们不硬解，交给上层报错
        assert_eq!(decrypt_chromium_value(b"v20abcdefghijklmnop", b"key"), None);
    }

    #[test]
    fn plaintext_without_prefix_is_not_decrypted_by_the_cipher() {
        // 没有前缀的值不是密文，不该走解密路径（调用方直接当明文用）
        assert_eq!(value_format(b"plaincookie"), ValueFormat::Plaintext);
        assert_eq!(decrypt_chromium_value(b"plaincookie", b"key"), None);
    }
}
