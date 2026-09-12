// 分任务落地期间，被调用方常常先于调用方合并（例如 Task 2 的 Store 在 Task 13 之前），
// 非 test 构建下它们还没人用，`-D warnings` 会因 dead_code 直接失败。
// 这一行在 Task 17 收口（最后一个子命令接上）时删除，并修掉真正的死代码。
#![allow(dead_code)]

mod api;
mod cli;
mod commands;
mod ipc;
mod kernels;
mod logging;
mod modules;
mod paths;
mod reconcile;
mod redact;
mod serve;
mod state;
mod sys;
#[cfg(test)]
mod testutil;
mod util;

use anyhow::Result;
use clap::{CommandFactory, Parser};
use cli::{Cli, Command};

// main 是**同步**的：spec §3.2 要求 `bui auth-hook` 不初始化 tokio、不初始化 tracing、不加载 state
// （M5 有 200 建连/秒、p99 < 20ms 的门槛，每次建连都要 fork 一个 bui），所以派发在建 runtime 之前
// 先把 AuthHook 摘出去。其余子命令再建 runtime、初始化日志。P2 落地钩子时只替换下面那一支，
// 不必回头重构入口。
fn main() -> Result<()> {
    let argv0 = std::env::args().next().unwrap_or_default();
    let args = Cli::parse();
    // 总纲 C5：只打印版本号
    if args.version {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let Some(command) = args.command.or_else(|| cli::default_command(&argv0)) else {
        Cli::command().print_help()?;
        println!();
        return Ok(());
    };
    if let Command::AuthHook { args } = &command {
        let _ = args;
        // P2 的交付物；P1 在这里就返回，连 runtime 与日志都不初始化
        anyhow::bail!("auth-hook 由 P2 实现");
    }
    logging::init(args.log.as_deref());
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(dispatch(command))
}

async fn dispatch(command: Command) -> Result<()> {
    match command {
        Command::Install { .. } => not_yet("install"),
        Command::Upgrade { .. } => not_yet("upgrade"),
        Command::Serve => not_yet("serve"),
        Command::Reconcile { .. } => not_yet("reconcile"),
        Command::Status { .. } => not_yet("status"),
        Command::ImportV3 { .. } => not_yet("import-v3"),
        Command::AuthHook { .. } => unreachable!("auth-hook 已在 main 里提前返回"),
        Command::Menu => not_yet("menu"),
        Command::HardenSsh => {
            commands::harden_ssh::run(
                bui_schema::paths::Paths::default_server(),
                std::sync::Arc::new(sys::real::RealHost::new()),
            )
            .await
        }
    }
}

/// 脚手架：后续任务逐个替换对应 arm；最后一个 arm 被替换时连这个函数一起删（Task 17）。
fn not_yet(what: &str) -> Result<()> {
    anyhow::bail!("子命令 {what} 尚未在本分支实现")
}
