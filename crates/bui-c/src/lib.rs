//! B-UI Linux 客户端：单引擎 sing-box 子进程的期望态控制器。
//!
//! 渲染全部来自 `bui_schema::render::client`；本 crate 只组装 `ClientOpts`、
//! 校验后落盘、管三个 systemd 单元、探测自愈、下载校验与菜单交互。
pub mod error;
pub mod fake;
pub mod net;
pub mod paths;
pub mod sys;
pub mod testutil;

pub use error::{Error, Result};

/// 与 Cargo.toml 的 version 同源；`update` 拿它和 manifest 比。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
