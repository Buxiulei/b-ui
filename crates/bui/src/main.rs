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

/// argv[0] 的文件名是否 `bui-auth-hook`（= Hysteria2 `auth.command` 指向的那条符号链接）。
///
/// 事故（2026-09-12 bwg-rick）：`auth.command` 只接受**单个可执行路径**——内核的
/// `CommandAuthenticator` 直接 `exec.Command(a.Cmd, addr, auth, tx)`，不过 shell、不拆空格，
/// 所以 `command: /opt/b-ui/bin/bui auth-hook` 被当成一个文件名带空格的可执行文件，钩子从未
/// 被调用、两台 Hysteria2 实例全员鉴权失败（调研 H15）。正解是给 `bui` 加一个多调用名：
/// `<base>/bin/bui-auth-hook` → `bui`，靠 argv[0] 分发。
fn is_auth_hook_argv0(argv0: &std::ffi::OsStr) -> bool {
    std::path::Path::new(argv0).file_name()
        == Some(std::ffi::OsStr::new(bui_schema::paths::AUTH_HOOK_BIN))
}

// main 是**同步**的：spec §3.2 要求钩子不初始化 tokio、不初始化 tracing、不加载 state
// （M5 有 200 建连/秒、p99 < 20ms 的门槛，每次建连都要 fork 一个 bui），所以两条钩子入口
// （argv[0] = `bui-auth-hook`，以及保留的 `bui auth-hook` 子命令）都在建 runtime 之前摘出去。
// 其余子命令再建 runtime、初始化日志。
fn main() -> Result<()> {
    // argv[0] 分发必须在 clap **之前**：内核调的是 `bui-auth-hook <addr> <auth> <tx>`，
    // 里面没有子命令，交给 clap 只会被判成未知参数（退出码 2 = 拒绝，但连日志都没有）。
    let mut argv = std::env::args_os();
    let argv0 = argv.next().unwrap_or_default();
    if is_auth_hook_argv0(&argv0) {
        let args: Vec<String> = argv
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        // 与下面那一支同样的极简路径：不建 tokio runtime、不初始化 tracing、不加载 state
        std::process::exit(modules::panel::auth_hook::run(&args));
    }
    let argv0 = argv0.to_string_lossy().into_owned();
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
        // spec §3.2：不建 tokio runtime、不初始化 tracing、不加载 state。
        // 退出码即判定结果（0 = 放行），放行时 stdout 已打印 user_id。
        std::process::exit(modules::panel::auth_hook::run(args));
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
            manifest_url,
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
                    manifest_url,
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
        Command::Incidents { json, n } => {
            commands::incidents::run(
                json,
                n,
                bui_schema::paths::Paths::default_server(),
                PathBuf::from(paths::SOCKET_PATH),
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
        Command::Hy2Prestart { config } => {
            modules::portjump::run(&sys::real::RealHost::new(), &config)
        }
        Command::Residential { cmd } => {
            modules::residential::cli::run(cmd, PathBuf::from(paths::SOCKET_PATH)).await
        }
        Command::Set { cmd } => match cmd {
            cli::SetCmd::Hy2Auth { mode } => {
                commands::config::run_hy2_auth(
                    &mode,
                    bui_schema::paths::Paths::default_server(),
                    std::sync::Arc::new(sys::real::RealHost::new()),
                )
                .await
            }
            cli::SetCmd::LegacySub { value } => {
                commands::config::run_legacy_sub(
                    &value,
                    bui_schema::paths::Paths::default_server(),
                    std::sync::Arc::new(sys::real::RealHost::new()),
                )
                .await
            }
            cli::SetCmd::Obfs { value } => {
                commands::config::run_obfs(
                    &value,
                    bui_schema::paths::Paths::default_server(),
                    std::sync::Arc::new(sys::real::RealHost::new()),
                )
                .await
            }
        },
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    /// 事故回归（2026-09-12）：`auth.command` 只能是一个**不带参数**的可执行路径，
    /// 所以钩子的入口是 `bin/bui-auth-hook` 这个符号链接 + argv[0] 分发。
    /// 判定只看**文件名**（内核给的是绝对路径），且必须在 clap 解析之前生效：
    /// `bui-auth-hook <addr> <auth> <tx>` 里没有子命令，交给 clap 只会被判成未知参数。
    #[test]
    fn recognizes_the_auth_hook_entry_by_its_file_name() {
        for yes in [
            "bui-auth-hook",
            "/opt/b-ui/bin/bui-auth-hook",
            "./bui-auth-hook",
            "/tmp/x/bui-auth-hook",
        ] {
            assert!(is_auth_hook_argv0(OsStr::new(yes)), "{yes} 应该进钩子");
        }
        for no in [
            "bui",
            "/opt/b-ui/bin/bui",
            "b-ui",
            "/usr/local/bin/b-ui",
            "bui-auth-hook2",
            "bui-auth-hookx",
            "xbui-auth-hook",
            "bui auth-hook", // 事故现场那个「文件名带空格」的兜底包装脚本
            "",
            "/opt/b-ui/bin/",
        ] {
            assert!(!is_auth_hook_argv0(OsStr::new(no)), "{no} 不该进钩子");
        }
        // 非 UTF-8 的 argv[0] 也不能 panic（file_name 比对走字节）
        assert!(!is_auth_hook_argv0(OsStr::from_bytes(b"/bin/\xff\xfe")));
        assert!(is_auth_hook_argv0(
            bui_schema::paths::Paths::default_server()
                .auth_hook_bin()
                .as_os_str()
        ));
    }
}
