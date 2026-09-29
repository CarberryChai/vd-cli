//! 契约测试的公共脚手架。
//!
//! 每个集成测试文件都会单独编译一份，用不到的辅助函数不应该报警告。
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde_json::Value;
use vd_cli::api::{Api, Bases};

/// 测试用的固定 UA（进程内固定是 spec 的要求，测试里也一样）。
pub const TEST_UA: &str = "vd-test/1.0 (contract tests)";

pub fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

pub fn fixture_raw(name: &str) -> String {
    std::fs::read_to_string(fixture_path(name))
        .unwrap_or_else(|e| panic!("读取 fixture {name} 失败: {e}"))
}

pub fn fixture_json(name: &str) -> Value {
    serde_json::from_str(&fixture_raw(name))
        .unwrap_or_else(|e| panic!("解析 fixture {name} 失败: {e}"))
}

/// 把 `playurl` 参数里的 `w_rid` 换成占位符，便于断言签名按固定顺序生成。
pub fn scrub_w_rid(query: &str) -> String {
    match query.split_once("&w_rid=") {
        Some((head, _)) => format!("{head}&w_rid=<hex32>"),
        None => query.to_string(),
    }
}

/// 指向 mock 服务器的 API 客户端。
pub fn api_for(server: &wiremock::MockServer) -> Api {
    Api::new(
        Bases {
            api: server.uri(),
            web: server.uri(),
        },
        TEST_UA,
        None,
    )
    .expect("构建 Api")
}

/// 指向 mock 服务器、带 Cookie 的客户端。
pub fn api_for_with_cookie(server: &wiremock::MockServer, cookie: &str) -> Api {
    Api::new(
        Bases {
            api: server.uri(),
            web: server.uri(),
        },
        TEST_UA,
        Some(cookie),
    )
    .expect("构建 Api")
}

static TRACING: OnceLock<()> = OnceLock::new();

/// 让测试里的 `tracing::warn!` 有个归宿（只在第一次调用时初始化）。
pub fn init_tracing() {
    TRACING.get_or_init(|| {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("vd_cli=debug")
            .with_test_writer()
            .try_init();
    });
}
