// Task 17 收口：Task 1 的**无条件** `#![allow(dead_code)]` 已按计划删掉，这里换成只作用于
// 非 test 构建的一条。理由（计划 Step 5 没预见到的情况）：
//
// `bui` 是 bin-only crate，`dead_code` 分析从 `main` 出发，而 `--all-targets` 会**同时**编
// 非 test 的 bin。剩下的十三处告警（`Store::update` 与它的备份轮转 / `CmdOut::success` /
// `sys::cmd_line` / `Host::is_symlink` / `ReconcileReport::is_clean` / `Module::name` /
// `Event::StateChanged` / `CoreFilesModule::with_handle` / `Verify::service` / …）**全部**
// 有通过的单元测试，或是 P2/P3 明文要消费的契约面：它们不是「真正没人用的函数」，删掉就等于
// 毁掉 Task 2/3 的交付。唯一真正谁都没用的 `Store::path` 已就地删除。
//
// 所以这条 allow 收窄成 `not(test)`：test 构建（含全部单元测试）仍然严格拦 dead_code，
// P2/P3 把这些调用方补上之后就该整条删掉。
#![cfg_attr(not(test), allow(dead_code))]

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
use std::path::PathBuf;

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
        Command::Install {
            domain,
            port,
            admin_password_stdin,
            import_v3,
            non_interactive,
            answers,
            yes,
        } => {
            commands::install::run(
                commands::install::InstallOpts {
                    domain,
                    port,
                    admin_password_stdin,
                    import_v3,
                    non_interactive,
                    answers,
                    yes,
                    // socket 路径由调用方传入，install 自己不读常量（单元测试因此不碰真实 socket）
                    socket: PathBuf::from(paths::SOCKET_PATH),
                },
                bui_schema::paths::Paths::default_server(),
                std::sync::Arc::new(sys::real::RealHost::new()),
            )
            .await
        }
        Command::Upgrade {
            rollback,
            version,
            manifest_url,
        } => {
            commands::upgrade::run(
                rollback,
                version,
                manifest_url,
                bui_schema::paths::Paths::default_server(),
                std::sync::Arc::new(sys::real::RealHost::new()),
            )
            .await
        }
        Command::Serve => {
            serve::run(
                bui_schema::paths::Paths::default_server(),
                std::sync::Arc::new(sys::real::RealHost::new()),
            )
            .await
        }
        Command::Reconcile { force, dry_run } => {
            serve::reconcile_cli(
                bui_schema::paths::Paths::default_server(),
                std::sync::Arc::new(sys::real::RealHost::new()),
                PathBuf::from(paths::SOCKET_PATH),
                force,
                dry_run,
            )
            .await
        }
        Command::Status { json } => {
            commands::status::run(
                json,
                bui_schema::paths::Paths::default_server(),
                std::sync::Arc::new(sys::real::RealHost::new()),
            )
            .await
        }
        Command::ImportV3 { dir, out } => {
            commands::import_v3::run(
                dir,
                out,
                bui_schema::paths::Paths::default_server(),
                std::sync::Arc::new(sys::real::RealHost::new()),
            )
            .await
        }
        Command::AuthHook { .. } => unreachable!("auth-hook 已在 main 里提前返回"),
        Command::Menu => {
            commands::menu::run(
                bui_schema::paths::Paths::default_server(),
                std::sync::Arc::new(sys::real::RealHost::new()),
            )
            .await
        }
        Command::HardenSsh => {
            commands::harden_ssh::run(
                bui_schema::paths::Paths::default_server(),
                std::sync::Arc::new(sys::real::RealHost::new()),
            )
            .await
        }
    }
}
