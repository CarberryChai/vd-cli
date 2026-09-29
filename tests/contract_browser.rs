//! 契约测试：从 Chromium 形状的 Cookie 数据库里读 B 站 Cookie。
//!
//! 这里自己搭一个真实的 SQLite 库（表结构与 Chrome 一致），并自己加密 Cookie
//! 值，验证「读库 → 解密 → 拼 Cookie 串」这条完整链路。
//! 真机上还会多一步「从钥匙串取口令」，那步需要用户授权，测不了。

use std::path::Path;

use aes::cipher::{BlockEncryptMut, KeyIvInit};
use vd_cli::browser::{Browser, decrypt_chromium_value, load_with_key};
use vd_cli::error::Error;

/// 造一个 Chrome 形状的 Cookies 库。
fn make_db(
    dir: &Path,
    name: &str,
    rows: &[(&str, &str, Option<&str>)],
    key: &[u8],
) -> std::path::PathBuf {
    let profile = dir.join(name);
    std::fs::create_dir_all(&profile).unwrap();
    let db = profile.join("Cookies");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE cookies (
            creation_utc INTEGER, host_key TEXT, top_frame_sid TEXT, name TEXT,
            value TEXT, encrypted_value BLOB, path TEXT, expires_utc INTEGER,
            is_secure INTEGER, is_httponly INTEGER, last_access_utc INTEGER,
            has_expires INTEGER, is_persistent INTEGER, priority INTEGER,
            samesite INTEGER, source_scheme INTEGER, last_update_utc INTEGER
        );",
    )
    .unwrap();
    for (host, name, plain_or_secret) in rows {
        let (plain, encrypted) = match plain_or_secret {
            Some(v) if *v == "PLAIN" => (String::new(), vec![]), // 由下面的 value 列给出
            Some(v) => (String::new(), encrypt(v, key)),
            None => (String::from("plain-value"), vec![]),
        };
        conn.execute(
            "INSERT INTO cookies (host_key, name, value, encrypted_value, path, is_secure,
                                  is_httponly, has_expires, is_persistent, priority, samesite,
                                  source_scheme)
             VALUES (?1, ?2, ?3, ?4, '/', 1, 1, 1, 1, 1, 0, 2)",
            rusqlite::params![host, name, plain, encrypted],
        )
        .unwrap();
    }
    db
}

/// 用 v10 方案加密（与 Chrome 一致：AES-128-CBC，IV 为 16 个空格，明文前置 32 字节摘要）。
fn encrypt(value: &str, key: &[u8]) -> Vec<u8> {
    type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;
    let mut plain = vec![0x11u8; 32];
    plain.extend_from_slice(value.as_bytes());
    let pad = 16 - (plain.len() % 16);
    plain.extend(std::iter::repeat_n(pad as u8, pad));
    let n = plain.len();
    let iv = [b' '; 16];
    let out = Aes128CbcEnc::new_from_slices(key, &iv)
        .unwrap()
        .encrypt_padded_mut::<aes::cipher::block_padding::NoPadding>(&mut plain, n)
        .unwrap()
        .to_vec();
    let mut blob = b"v10".to_vec();
    blob.extend_from_slice(&out);
    blob
}

const KEY: &[u8] = b"0123456789abcdef";

/// 调整文件修改时间（秒），用于稳定「取最近活动的 profile」这个行为。
fn bump_mtime(path: &Path, delta_secs: i64) {
    let now = std::time::SystemTime::now();
    let t = if delta_secs >= 0 {
        now + std::time::Duration::from_secs(delta_secs as u64)
    } else {
        now - std::time::Duration::from_secs((-delta_secs) as u64)
    };
    let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_modified(t).unwrap();
}

#[test]
fn reads_encrypted_bilibili_cookies() {
    let tmp = tempfile::tempdir().unwrap();
    make_db(
        tmp.path(),
        "Default",
        &[
            (".bilibili.com", "SESSDATA", Some("my-sessdata")),
            (".bilibili.com", "bili_jct", Some("my-jct")),
            (".bilibili.com", "DedeUserID", Some("12345")),
            // 其它站点的 Cookie 不该被带进来
            (".example.com", "tracking", Some("nope")),
        ],
        KEY,
    );

    let cookie = load_with_key(Browser::Chrome, tmp.path(), None, Some(KEY)).unwrap();
    assert!(cookie.contains("SESSDATA=my-sessdata"), "{cookie}");
    assert!(cookie.contains("bili_jct=my-jct"), "{cookie}");
    assert!(!cookie.contains("tracking"), "不能带上别的站点: {cookie}");
    assert!(!cookie.contains("example.com"), "{cookie}");
}

#[test]
fn keeps_plaintext_values_too() {
    // 有些条目 value 列就是明文（macOS 上少见但存在）
    let tmp = tempfile::tempdir().unwrap();
    let profile = tmp.path().join("Default");
    std::fs::create_dir_all(&profile).unwrap();
    let db = profile.join("Cookies");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE cookies (host_key TEXT, name TEXT, value TEXT, encrypted_value BLOB);",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO cookies VALUES ('.bilibili.com', 'buvid3', 'plain-buvid', x'')",
        [],
    )
    .unwrap();
    drop(conn);

    let cookie = load_with_key(Browser::Chrome, tmp.path(), None, Some(KEY)).unwrap();
    assert_eq!(cookie, "buvid3=plain-buvid");
}

#[test]
fn picks_the_most_recently_used_profile() {
    let tmp = tempfile::tempdir().unwrap();
    make_db(
        tmp.path(),
        "Default",
        &[(".bilibili.com", "SESSDATA", Some("old-account"))],
        KEY,
    );
    make_db(
        tmp.path(),
        "Profile 1",
        &[(".bilibili.com", "SESSDATA", Some("new-account"))],
        KEY,
    );
    // 文件系统的 mtime 粒度可能到秒，显式把两个库的时间差拉开，测试才稳定
    bump_mtime(&tmp.path().join("Default/Cookies"), -120);

    // 默认取最近活动的 profile
    let cookie = load_with_key(Browser::Chrome, tmp.path(), None, Some(KEY)).unwrap();
    assert!(cookie.contains("new-account"), "{cookie}");

    // 显式指定时以指定为准
    let cookie = load_with_key(Browser::Chrome, tmp.path(), Some("Default"), Some(KEY)).unwrap();
    assert!(cookie.contains("old-account"), "{cookie}");
}

#[test]
fn deduplicates_repeated_names() {
    let tmp = tempfile::tempdir().unwrap();
    make_db(
        tmp.path(),
        "Default",
        &[
            (".bilibili.com", "SESSDATA", Some("first")),
            ("www.bilibili.com", "SESSDATA", Some("second")),
        ],
        KEY,
    );
    let cookie = load_with_key(Browser::Chrome, tmp.path(), None, Some(KEY)).unwrap();
    assert_eq!(cookie.matches("SESSDATA=").count(), 1, "{cookie}");
}

#[test]
fn missing_login_gives_actionable_error() {
    let tmp = tempfile::tempdir().unwrap();
    // 库存在，但没有 bilibili 的记录
    make_db(
        tmp.path(),
        "Default",
        &[(".example.com", "foo", Some("bar"))],
        KEY,
    );
    let err = load_with_key(Browser::Chrome, tmp.path(), None, Some(KEY)).unwrap_err();
    assert!(matches!(err, Error::BrowserCookie(_)), "{err}");
    let msg = err.to_string();
    assert!(msg.contains("登录"), "{msg}");
    assert!(msg.contains("--browser-profile"), "要给出下一步: {msg}");
}

#[test]
fn missing_profile_gives_actionable_error() {
    let tmp = tempfile::tempdir().unwrap();
    make_db(
        tmp.path(),
        "Default",
        &[(".bilibili.com", "SESSDATA", Some("x"))],
        KEY,
    );
    let err = load_with_key(Browser::Chrome, tmp.path(), Some("Profile 9"), Some(KEY)).unwrap_err();
    assert!(err.to_string().contains("Profile 9"), "{err}");
}

#[test]
fn no_cookie_database_at_all() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("Default")).unwrap();
    let err = load_with_key(Browser::Chrome, tmp.path(), None, Some(KEY)).unwrap_err();
    assert!(err.to_string().contains("没找到"), "{err}");
}

#[test]
fn end_to_end_round_trip_through_the_real_decryptor() {
    // 直接验证解密函数与造库用的加密是对称的
    let blob = encrypt("SESSDATA-value-xyz", KEY);
    assert_eq!(
        decrypt_chromium_value(&blob, KEY).as_deref(),
        Some("SESSDATA-value-xyz")
    );

    // 换错 key 必须解不出来（而不是返回乱码）
    let wrong = b"fedcba9876543210";
    assert_ne!(
        decrypt_chromium_value(&blob, wrong).as_deref(),
        Some("SESSDATA-value-xyz")
    );
}
