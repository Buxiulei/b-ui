//! B-UI Linux 客户端（`bui-c`）：单引擎 sing-box 子进程的期望态控制器。
//!
//! - 渲染：全部来自 [`bui_schema::render::client`]，本 crate 不拼配置字段。
//! - 系统交互：只经 [`sys::Sys`] 与 [`net::Net`]，单元测试注入 [`fake::FakeSys`] / [`fake::FakeNet`]。
//! - 期望态：`profiles.json`（用户意图）+ `runtime.json`（可重建的运行时数据）→ [`engine::Engine::apply`]。
//! - 单元：`bui-c.service`（sing-box）、`bui-c-check.service`（oneshot 巡检）、`bui-c.timer`（每分钟）。
pub mod check;
pub mod cli;
pub mod engine;
pub mod error;
pub mod fake;
pub mod import_v3;
pub mod menu;
pub mod net;
pub mod nettest;
pub mod paths;
pub mod profiles;
pub mod source;
pub mod sys;
pub mod testutil;
pub mod ufw;
pub mod uninstall;
pub mod units;
pub mod update;

pub use error::{Error, Result};

/// 与 Cargo.toml 的 version 同源；`update` 拿它和 manifest 比。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
