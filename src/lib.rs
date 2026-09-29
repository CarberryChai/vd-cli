//! `vd` —— Bilibili 视频下载 CLI。
//!
//! 模块调用是单向的：`main → resolve → api → model/select → download → mux`。
//! `bv`、`wbi`、`select`、`path` 是纯函数，不依赖网络。

pub mod api;
pub mod browser;
pub mod buvid;
pub mod bv;
pub mod cli;
pub mod download;
pub mod error;
pub mod model;
pub mod mux;
pub mod path;
pub mod resolve;
pub mod select;
pub mod wbi;

pub use error::{Error, Result};
