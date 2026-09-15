//! CLI 定义。子命令与选项名**逐字照总纲 C5**，P5 的脚本按 C5 调用，不得自造参数名。
use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// b-ui v4 单二进制控制器（守护进程 + CLI）。
#[derive(Debug, Parser)]
#[command(
    name = "bui",
    about = "b-ui v4 期望态控制器",
    subcommand_required = false,
    arg_required_else_help = false,
    // 总纲 C5：`bui --version` 只打印版本号，所以关掉 clap 自带的 --version（它会打印 "bui 4.0.0"）
    disable_version_flag = true
)]
pub struct Cli {
    /// 日志级别（覆盖 RUST_LOG），例如 info / debug
    #[arg(long, global = true)]
    pub log: Option<String>,
    /// 打印版本号后退出（只打印 `4.0.0`，不带程序名）
    #[arg(long, short = 'V')]
    pub version: bool,
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// 全部子命令（形状是 C5 的字面约定）。
#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum Command {
    /// 首次安装：问答式收集关键信息（域名必填，其余每题回车即默认）→ 写 state.json → 对账
    Install {
        /// 面板域名（唯一必填项；不给就在问答里问。也可用 $BUI_DOMAIN，但那条写法等同 --yes：一个问题都不问）
        #[arg(long)]
        domain: Option<String>,
        /// Hysteria2 直连端口（默认 10000）
        #[arg(long)]
        port: Option<u16>,
        /// 从 stdin 读管理员密码（凭据不进 argv）
        #[arg(long)]
        admin_password_stdin: bool,
        /// 从 v3 安装目录导入（不带值时用 /opt/b-ui）
        #[arg(long, value_name = "DIR", num_args = 0..=1, default_missing_value = "/opt/b-ui")]
        import_v3: Option<PathBuf>,
        /// 非交互安装：一个问题都不问（总纲 C5）
        #[arg(long)]
        non_interactive: bool,
        /// 非交互安装的答案文件（JSON；形状见 Task 16 的 `Answers` / `load_answers`）
        #[arg(long, value_name = "FILE", requires = "non_interactive")]
        answers: Option<PathBuf>,
        /// 非交互：缺失项用默认值（`--non-interactive` 的简写别名，两者都接受）
        #[arg(long, short = 'y')]
        yes: bool,
        /// 覆盖 manifest 地址：http(s) URL、`file://…` 或本地路径（与 `bui upgrade` 的同名
        /// 选项同义，总纲 C4；`install.sh` 走 `$BUI_MANIFEST_URL` 把它选定的地址传下来）
        #[arg(long, value_name = "URL|FILE")]
        manifest_url: Option<String>,
    },
    /// 升级 bui 与内核二进制
    Upgrade {
        /// 回滚到上一版二进制与最近一份 state 备份
        #[arg(long)]
        rollback: bool,
        /// 升级到指定版本（manifest 取 `releases/download/v<x.y.z>/manifest.json`）
        #[arg(long, value_name = "X.Y.Z")]
        version: Option<String>,
        /// 覆盖 manifest 地址：http(s) URL、`file://…` 或本地路径（总纲 C4，M5 演练用）
        #[arg(long, value_name = "URL|FILE")]
        manifest_url: Option<String>,
    },
    /// 运行守护进程（systemd 用）
    Serve,
    /// 手动对账一次
    Reconcile {
        /// 连非受管的漂移项一起清理
        #[arg(long)]
        force: bool,
        /// 只打印将要做的改动，不落盘
        #[arg(long)]
        dry_run: bool,
    },
    /// 打印状态与体检
    Status {
        /// 输出 `/api/health` 的 JSON
        #[arg(long)]
        json: bool,
    },
    /// 日志哨兵的事件（新的在前）
    Incidents {
        /// 输出 JSON（`{"incidents": [...], "source": "daemon" | "runtime.json"}`）
        #[arg(long)]
        json: bool,
        /// 显示最近几条
        #[arg(short = 'n', long = "count", default_value_t = 20)]
        n: usize,
    },
    /// 只从 v3 生成 state.json（不对账、不卸载 v3）
    ImportV3 {
        /// v3 安装目录
        #[arg(long, default_value = "/opt/b-ui")]
        dir: PathBuf,
        /// 输出路径（默认写 <base>/state.json）
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Hysteria2 auth.command 钩子（逻辑由 P2 实现）
    AuthHook {
        /// hysteria 传进来的位置参数，原样收集
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Hysteria2 启动前清理**本实例**残留的端口跳跃 nat 链（两个 hysteria 单元的
    /// `ExecStartPre=-`，也被 watchdog 的自愈分支复用）。永远退 0。
    Hy2Prestart {
        /// 该实例的配置路径（`/opt/b-ui/config.yaml` 或 `config-residential.yaml`），
        /// 从它的 `listen:` 行取本实例的 base 端口与跳跃区间
        config: PathBuf,
    },
    /// 住宅出口（上游池 / 体检 / 切换 / 黑名单）
    Residential {
        #[command(subcommand)]
        cmd: crate::modules::residential::cli::ResidentialCmd,
    },
    /// 改一项系统开关（`bui set <项> <值>`）
    Set {
        #[command(subcommand)]
        cmd: SetCmd,
    },
    /// 数字菜单（sudo b-ui 的符号链接目标）
    Menu,
    /// 只做 SSH 硬化
    HardenSsh,
}

/// `bui set` 的子命令。
#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum SetCmd {
    /// Hysteria2 鉴权方式：`http`（默认，守护进程进程内应答）/ `command`（退路，每条连接 fork 钩子）。
    /// 改完会重渲染两份 hysteria 配置并各重启一次实例。
    Hy2Auth {
        #[arg(value_parser = ["http", "command"])]
        mode: String,
    },
    /// 旧「用户名订阅链接」的全局宽限期：`off` 立刻停用全部用户名链接（只认随机 token），
    /// 或给一个 RFC3339 时刻（如 `2026-09-21T00:00:00Z`）改期。
    LegacySub {
        /// `off` 或 RFC3339 时刻；取值合法性由 `commands::config::parse_legacy_sub` 判。
        value: String,
    },
    /// HY2 混淆（salamander）开关：`on` / `off`，覆盖直连与全部住宅 HY2 实例。
    /// 开启或关闭后，所有用户的 HY2 节点要更新一次订阅才能连上；Reality 节点不受影响。
    Obfs {
        /// `on` 或 `off`；取值合法性由 `commands::config::parse_obfs` 判。
        value: String,
    },
}

/// `/usr/local/bin/b-ui` 这个符号链接裸跑时进菜单（spec §1、§2.4）。
///
/// 以 `bui` 名字裸跑则返回 `None`，由 `main` 打印 help。
pub fn default_command(argv0: &str) -> Option<Command> {
    let name = std::path::Path::new(argv0)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    (name == "b-ui").then_some(Command::Menu)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use pretty_assertions::assert_eq;

    #[test]
    fn parses_install_with_bare_import_flag() {
        let cli = Cli::try_parse_from(["bui", "install", "--domain", "example.com", "--import-v3"])
            .unwrap();
        assert_eq!(
            cli.command,
            Some(Command::Install {
                domain: Some("example.com".into()),
                port: None,
                admin_password_stdin: false,
                import_v3: Some(PathBuf::from("/opt/b-ui")),
                non_interactive: false,
                answers: None,
                yes: false,
                manifest_url: None,
            })
        );
    }

    #[test]
    fn parses_non_interactive_install_with_an_answers_file() {
        // 总纲 C5 的 `bui install [--non-interactive --answers <file>]`：P5 的 v3-cutover.sh 只准用这一条
        let cli = Cli::try_parse_from([
            "bui",
            "install",
            "--non-interactive",
            "--answers",
            "/root/answers.json",
            "--admin-password-stdin",
        ])
        .unwrap();
        assert_eq!(
            cli.command,
            Some(Command::Install {
                domain: None,
                port: None,
                admin_password_stdin: true,
                import_v3: None,
                non_interactive: true,
                answers: Some(PathBuf::from("/root/answers.json")),
                yes: false,
                manifest_url: None,
            })
        );
    }

    #[test]
    fn an_answers_file_without_non_interactive_is_rejected() {
        assert!(
            Cli::try_parse_from(["bui", "install", "--answers", "/root/answers.json"]).is_err()
        );
    }

    #[test]
    fn parses_install_with_import_dir_and_password_stdin() {
        let cli = Cli::try_parse_from([
            "bui",
            "install",
            "--domain",
            "example.com",
            "--port",
            "10000",
            "--admin-password-stdin",
            "--import-v3",
            "/tmp/old",
            "-y",
        ])
        .unwrap();
        assert_eq!(
            cli.command,
            Some(Command::Install {
                domain: Some("example.com".into()),
                port: Some(10000),
                admin_password_stdin: true,
                import_v3: Some(PathBuf::from("/tmp/old")),
                non_interactive: false,
                answers: None,
                yes: true,
                manifest_url: None,
            })
        );
    }

    /// `bui install --manifest-url`：与 `bui upgrade` 的同名选项同义（总纲 C4）。
    /// `install.sh` 平时走 `$BUI_MANIFEST_URL`，M5 演练与离线源要能在 argv 上直接给。
    #[test]
    fn parses_install_with_a_manifest_url_override() {
        let cli = Cli::try_parse_from([
            "bui",
            "install",
            "--domain",
            "example.com",
            "--manifest-url",
            "http://127.0.0.1:8000/manifest.json",
            "--yes",
        ])
        .unwrap();
        assert_eq!(
            cli.command,
            Some(Command::Install {
                domain: Some("example.com".into()),
                port: None,
                admin_password_stdin: false,
                import_v3: None,
                non_interactive: false,
                answers: None,
                yes: true,
                manifest_url: Some("http://127.0.0.1:8000/manifest.json".into()),
            })
        );
    }

    #[test]
    fn rejects_password_on_argv() {
        // 凭据不进 argv：没有 --admin-password 这个选项
        assert!(Cli::try_parse_from(["bui", "install", "--admin-password", "x"]).is_err());
    }

    #[test]
    fn parses_upgrade_flags_per_c5_and_reconcile_flags() {
        assert_eq!(
            Cli::try_parse_from(["bui", "upgrade", "--rollback"])
                .unwrap()
                .command,
            Some(Command::Upgrade {
                rollback: true,
                version: None,
                manifest_url: None
            })
        );
        assert_eq!(
            Cli::try_parse_from(["bui", "upgrade", "--version", "4.0.1"])
                .unwrap()
                .command,
            Some(Command::Upgrade {
                rollback: false,
                version: Some("4.0.1".into()),
                manifest_url: None
            })
        );
        assert_eq!(
            Cli::try_parse_from([
                "bui",
                "upgrade",
                "--manifest-url",
                "http://127.0.0.1:8000/manifest.json"
            ])
            .unwrap()
            .command,
            Some(Command::Upgrade {
                rollback: false,
                version: None,
                manifest_url: Some("http://127.0.0.1:8000/manifest.json".into()),
            })
        );
        assert!(
            Cli::try_parse_from(["bui", "upgrade", "--channel", "beta"]).is_err(),
            "--channel 已废弃"
        );
        assert_eq!(
            Cli::try_parse_from(["bui", "reconcile", "--force", "--dry-run"])
                .unwrap()
                .command,
            Some(Command::Reconcile {
                force: true,
                dry_run: true
            })
        );
        assert_eq!(
            Cli::try_parse_from(["bui", "import-v3"]).unwrap().command,
            Some(Command::ImportV3 {
                dir: PathBuf::from("/opt/b-ui"),
                out: None
            })
        );
    }

    /// 单元里写的是 `ExecStartPre=-/opt/b-ui/bin/bui hy2-prestart /opt/b-ui/config.yaml`：
    /// 子命令名与位置参数的形状必须和 `modules::units` 渲染的那一行逐字对得上。
    #[test]
    fn parses_hy2_prestart_with_the_config_path() {
        assert_eq!(
            Cli::try_parse_from(["bui", "hy2-prestart", "/opt/b-ui/config-residential.yaml"])
                .unwrap()
                .command,
            Some(Command::Hy2Prestart {
                config: PathBuf::from("/opt/b-ui/config-residential.yaml")
            })
        );
        assert!(
            Cli::try_parse_from(["bui", "hy2-prestart"]).is_err(),
            "配置路径是必填：不给就不知道清哪个实例的链"
        );
    }

    #[test]
    fn auth_hook_collects_trailing_args() {
        let cli = Cli::try_parse_from(["bui", "auth-hook", "alice", "pw"]).unwrap();
        assert_eq!(
            cli.command,
            Some(Command::AuthHook {
                args: vec!["alice".into(), "pw".into()]
            })
        );
    }

    /// `bui set hy2-auth http|command`（2026-09-13 裁决的退路开关）。别的值必须被 clap 挡掉：
    /// 写错一个字就意味着两份 hysteria 配置里出现一个内核不认识的 `auth.type`。
    #[test]
    fn parses_the_hy2_auth_switch_and_rejects_anything_else() {
        for mode in ["http", "command"] {
            assert_eq!(
                Cli::try_parse_from(["bui", "set", "hy2-auth", mode])
                    .unwrap()
                    .command,
                Some(Command::Set {
                    cmd: SetCmd::Hy2Auth { mode: mode.into() }
                })
            );
        }
        assert!(Cli::try_parse_from(["bui", "set", "hy2-auth", "userpass"]).is_err());
        assert!(Cli::try_parse_from(["bui", "set", "hy2-auth"]).is_err());
        assert!(Cli::try_parse_from(["bui", "set"]).is_err());
    }

    /// `bui set legacy-sub off|<RFC3339>`（2026-09-14 裁决的收口开关）。取值形状不由 clap 管
    /// （RFC3339 写不进 `value_parser`），但**必须收到一个值**：`bui set legacy-sub` 裸跑要报错，
    /// 不能被当成「off」把所有人的旧链接一把掐掉。
    #[test]
    fn parses_the_legacy_sub_switch_and_needs_a_value() {
        for value in ["off", "2026-09-21T00:00:00Z"] {
            assert_eq!(
                Cli::try_parse_from(["bui", "set", "legacy-sub", value])
                    .unwrap()
                    .command,
                Some(Command::Set {
                    cmd: SetCmd::LegacySub {
                        value: value.into()
                    }
                })
            );
        }
        assert!(Cli::try_parse_from(["bui", "set", "legacy-sub"]).is_err());
    }

    /// `bui set obfs on|off`（2026-09-15 裁决）。取值由 `commands::config::parse_obfs` 一处判，
    /// clap 只管**必须收到一个值**；`bui set --help` 里看得到这一项。
    #[test]
    fn parses_the_obfs_switch_and_needs_a_value() {
        for value in ["on", "off"] {
            assert_eq!(
                Cli::try_parse_from(["bui", "set", "obfs", value])
                    .unwrap()
                    .command,
                Some(Command::Set {
                    cmd: SetCmd::Obfs {
                        value: value.into()
                    }
                })
            );
        }
        assert!(Cli::try_parse_from(["bui", "set", "obfs"]).is_err());
        let help = Cli::try_parse_from(["bui", "set", "--help"])
            .unwrap_err()
            .to_string();
        assert!(help.contains("obfs"), "{help}");
    }

    #[test]
    fn bare_invocation_parses_and_leaves_the_command_empty() {
        // spec §2.4：`sudo b-ui` 无参也要能跑（进菜单），所以子命令不是必填
        assert_eq!(Cli::try_parse_from(["b-ui"]).unwrap().command, None);
        assert_eq!(Cli::try_parse_from(["bui"]).unwrap().command, None);
    }

    #[test]
    fn version_is_our_own_flag_so_it_can_print_just_the_number() {
        // 总纲 C5：`bui --version` 只打印 `4.0.0`。clap 自带的 version 会打印 `bui 4.0.0`，
        // 所以 `#[command(...)]` 里 `disable_version_flag = true`，改由 main 自己处理这个 bool。
        let cli = Cli::try_parse_from(["bui", "--version"]).unwrap();
        assert!(cli.version);
        assert_eq!(cli.command, None);
        assert!(!Cli::try_parse_from(["bui", "status"]).unwrap().version);
    }

    #[test]
    fn the_b_ui_alias_defaults_to_the_menu() {
        assert_eq!(default_command("/usr/local/bin/b-ui"), Some(Command::Menu));
        assert_eq!(default_command("b-ui"), Some(Command::Menu));
        assert_eq!(
            default_command("/opt/b-ui/bin/bui"),
            None,
            "以 bui 名字裸跑打印 help"
        );
        assert_eq!(default_command(""), None);
    }

    #[test]
    fn parses_incidents_with_a_count_and_json() {
        assert_eq!(
            Cli::try_parse_from(["bui", "incidents"]).unwrap().command,
            Some(Command::Incidents { json: false, n: 20 })
        );
        assert_eq!(
            Cli::try_parse_from(["bui", "incidents", "-n", "5", "--json"])
                .unwrap()
                .command,
            Some(Command::Incidents { json: true, n: 5 })
        );
        assert!(Cli::try_parse_from(["bui", "incidents", "-n", "x"]).is_err());
    }
}
