//! clap 子命令、`Ctx`（可注入的一次会话）、菜单循环与 `run()`。
//!
//! 所有会改机器的子命令都经 `apply_with_ufw`：装内核 → 按模式同步 UFW → `Engine::apply`。
//! root 检查只在 [`run`] 里做，`dispatch` 保持纯注入，单元测试直接调它。

use crate::check::{self, Runtime, Verdict};
use crate::engine::{Applied, Engine};
use crate::menu::{self, Action, Prompt, Status};
use crate::net::Net;
use crate::paths::{Paths, UNIT_MAIN, UNIT_TIMER};
use crate::profiles::{profile_name, rfc3339, Mode, Panel, Profile, Profiles, Source, Upsert};
use crate::source::{self, Fetched};
use crate::sys::{systemd, Sys};
use crate::{import_v3, ufw, uninstall, update, Error, Result};
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "bui-c",
    version,
    about = "B-UI Linux 客户端（单引擎 sing-box）"
)]
pub struct Cli {
    /// 机器可读输出（只对 status / list 生效）
    #[arg(long, global = true)]
    pub json: bool,
    /// 跳过所有确认
    #[arg(short = 'y', long, global = true)]
    pub yes: bool,
    #[command(subcommand)]
    pub cmd: Option<Cmd>,
}

#[derive(Subcommand, Debug, PartialEq, Eq)]
pub enum Cmd {
    /// 当前节点、模式与服务状态
    Status,
    /// 列出所有节点
    List,
    /// 切换到指定节点
    Switch { name: String },
    /// 切换 socks / tun 模式
    Mode { mode: ModeArg },
    /// 导入节点：粘贴 URI，或 --panel/--user，或 --sub
    Import {
        /// hysteria2:// 或 vless:// 链接。推荐写 `-` 从标准输入逐行读：
        /// 位置参数会把 HY2 密码留在 shell 历史与 ps 输出里（全局约束「凭据不进 argv」）
        uri: Option<String>,
        #[arg(long)]
        panel: Option<String>,
        #[arg(long)]
        user: Option<String>,
        #[arg(long)]
        sub: Option<String>,
        #[arg(long)]
        activate: bool,
    },
    /// 巡检（bui-c.timer 每分钟调用）
    Check,
    /// 按 manifest 升级自身与 sing-box；`--auto on|off` 只开关每日自动更新
    Update {
        #[arg(long)]
        check_only: bool,
        /// 开/关每日自动更新（spec §6「可关」的 CLI 入口）。给了这个参数就只改开关、不联网
        #[arg(long, value_name = "on|off")]
        auto: Option<Switch>,
    },
    /// 从 v3 的 /opt/hysteria-client 导入并卸载旧单元
    ImportV3 {
        #[arg(long)]
        base: Option<PathBuf>,
        /// 面板地址（取 manifest 与内核；默认用 v3 记录的 server_address，也可用环境变量 BUI_C_PANEL）
        #[arg(long)]
        panel: Option<String>,
        /// 覆盖升级后的模式（默认沿用 v3：bui-tun 曾 enable 则 tun，否则 socks）
        #[arg(long)]
        mode: Option<ModeArg>,
    },
    /// 卸载
    Uninstall {
        #[arg(long)]
        purge_bin: bool,
    },
    /// 进数字菜单（无参数时的默认行为）
    Menu,
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeArg {
    Socks,
    Tun,
}

/// `update --auto on|off`：每日自动更新的开关。
#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Switch {
    On,
    Off,
}

impl From<ModeArg> for Mode {
    fn from(m: ModeArg) -> Self {
        match m {
            ModeArg::Socks => Mode::Socks,
            ModeArg::Tun => Mode::Tun,
        }
    }
}

pub struct Ctx<'a, S: Sys, N: Net, P: Prompt> {
    pub sys: &'a S,
    pub net: &'a N,
    pub paths: &'a Paths,
    pub prompt: &'a mut P,
    pub json: bool,
    pub yes: bool,
    /// 待打印缓冲；`flush()` 打印并清空
    pub out: String,
    /// 说过的全部话，`flush()` 不清空。菜单循环里每轮都 flush，测试只能靠它断言
    pub transcript: String,
}

impl<'a, S: Sys, N: Net, P: Prompt> Ctx<'a, S, N, P> {
    pub fn new(
        sys: &'a S,
        net: &'a N,
        paths: &'a Paths,
        prompt: &'a mut P,
        json: bool,
        yes: bool,
    ) -> Self {
        Self {
            sys,
            net,
            paths,
            prompt,
            json,
            yes,
            out: String::new(),
            transcript: String::new(),
        }
    }
    pub fn say(&mut self, line: impl AsRef<str>) {
        self.out.push_str(line.as_ref());
        self.out.push('\n');
        self.transcript.push_str(line.as_ref());
        self.transcript.push('\n');
    }
    pub fn flush(&mut self) {
        if !self.out.is_empty() {
            print!("{}", self.out);
            self.out.clear();
        }
    }
}

fn engine_status<S: Sys, N: Net, P: Prompt>(ctx: &Ctx<'_, S, N, P>, prof: &Profiles) -> Status {
    let p = prof.active_profile();
    Status {
        node: p.map(|x| x.name.clone()).unwrap_or_default(),
        label: p.map(|x| x.node.label.clone()).unwrap_or_default(),
        mode: prof.mode,
        service_running: systemd::is_active(ctx.sys, UNIT_MAIN),
        tun_up: Engine::new(ctx.sys, ctx.paths).tun_up(),
        socks_port: prof.socks_port,
        http_port: prof.http_port,
        // 上一次检查更新的结论，不为渲染一屏菜单去联网（写在 `update` / 巡检自更新里）
        update_available: Runtime::load(ctx.sys, ctx.paths).update_available,
        auto_update: prof.auto_update,
    }
}

/// 这次检查之后「还有新版没装」吗——菜单 `[6] ★ 有新版` 的口径。
///
/// 刚刚自替换成 manifest 版本时要算「没有新版」：本进程的 `VERSION` 还是旧二进制的，
/// 光比版本号会让菜单一直挂着 ★，直到下次检查。
fn new_version_pending(r: &update::Report) -> bool {
    r.manifest_version != crate::VERSION && !r.self_updated
}

/// `BUI_C_PANEL=<url>`：本次进程里把面板地址换成它（只在内存里，不落盘），给预发布期
/// 「v3 推导的面板没有 `/packages`」这类情况一个显式出口。经 `Sys::env` 读（决策 11，
/// 测试可注入）。只换 `base_url`，`username` 照旧——它是订阅路径，跟 manifest 来源无关。
fn with_panel_override<S: Sys>(sys: &S, prof: &Profiles) -> Profiles {
    let Some(url) = sys
        .env("BUI_C_PANEL")
        .map(|u| u.trim().trim_end_matches('/').to_string())
        .filter(|u| !u.is_empty())
    else {
        return prof.clone();
    };
    let mut out = prof.clone();
    out.panel = Some(Panel {
        base_url: url,
        username: out
            .panel
            .as_ref()
            .map(|p| p.username.clone())
            .unwrap_or_default(),
    });
    out
}

/// 唯一的「改机器」路径：装内核 → 同步 UFW → apply。
fn apply_with_ufw<S: Sys, N: Net, P: Prompt>(
    ctx: &mut Ctx<'_, S, N, P>,
    prof: &Profiles,
) -> Result<Applied> {
    if update::ensure_kernel(
        ctx.sys,
        ctx.net,
        ctx.paths,
        &with_panel_override(ctx.sys, prof),
    )? {
        ctx.say("已安装 sing-box 内核");
    }
    let mut rt = Runtime::load(ctx.sys, ctx.paths);
    match prof.mode {
        Mode::Tun => {
            if ufw::allow_tun(ctx.sys)? && !rt.ufw_rules {
                rt.ufw_rules = true;
                rt.save(ctx.sys, ctx.paths)?;
                ctx.say("已为 bui-tun 接口放行 UFW（含 route allow）");
            }
        }
        Mode::Socks => {
            if rt.ufw_rules {
                ufw::revoke_tun(ctx.sys)?;
                rt.ufw_rules = false;
                rt.save(ctx.sys, ctx.paths)?;
                ctx.say("已撤回 bui-tun 的 UFW 放行规则");
            }
        }
    }
    let applied = Engine::new(ctx.sys, ctx.paths).apply(prof)?;
    if applied.tun_ready == Some(false) {
        ctx.say("警告：bui-c.service 已启动但 bui-tun 接口未就绪，查 `journalctl -u bui-c`");
    }
    Ok(applied)
}

struct Stored {
    added: usize,
    names: Vec<String>,
}

/// profile 名即主键：同名视为「同一个节点位」，凭据轮换直接替换，不堆重复条目。
fn store_fetched<S: Sys, N: Net, P: Prompt>(
    ctx: &mut Ctx<'_, S, N, P>,
    prof: &mut Profiles,
    f: &Fetched,
    src: Source,
    panel: Option<Panel>,
) -> Stored {
    let mut out = Stored {
        added: 0,
        names: Vec::with_capacity(f.nodes.len()),
    };
    for node in &f.nodes {
        // 同一个节点已在别的名字下 → 沿用那个名字，不新建
        let name = prof
            .find_by_node(node)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| profile_name(&f.user, node));
        let r = prof.upsert(Profile {
            name: name.clone(),
            node: node.clone(),
            split: f.split.clone(),
            source: src,
            imported_at: rfc3339(ctx.sys),
        });
        match r {
            Upsert::Added => out.added += 1,
            Upsert::Replaced => ctx.say(format!("更新节点 {name}")),
            Upsert::Unchanged => ctx.say(format!("节点 {name} 无变化")),
        }
        out.names.push(name);
    }
    if let Some(p) = panel {
        prof.panel = Some(p);
    }
    for skip in &f.skipped {
        ctx.say(format!("跳过无法解析的行：{skip}…"));
    }
    out
}

pub fn dispatch<S: Sys, N: Net, P: Prompt>(cli: &Cli, ctx: &mut Ctx<'_, S, N, P>) -> Result<()> {
    match cli.cmd.as_ref().unwrap_or(&Cmd::Menu) {
        Cmd::Menu => menu_loop(ctx),
        Cmd::Status => {
            let prof = Profiles::load(ctx.sys, ctx.paths)?;
            let st = engine_status(ctx, &prof);
            if ctx.json {
                let v = serde_json::json!({
                    "active": st.node, "label": st.label,
                    "mode": match prof.mode { Mode::Tun => "tun", Mode::Socks => "socks" },
                    "service": if st.service_running { "running" } else { "stopped" },
                    "timer": if systemd::is_active(ctx.sys, UNIT_TIMER) { "active" } else { "inactive" },
                    "tun": if st.tun_up { "up" } else { "down" },
                    "socks_port": st.socks_port, "http_port": st.http_port,
                    "profiles": prof.profiles.len(), "auto_update": prof.auto_update,
                    "version": crate::VERSION,
                });
                ctx.out = serde_json::to_string_pretty(&v)
                    .map_err(|e| Error::parse("status", e.to_string()))?;
            } else {
                let text = menu::render(&st);
                ctx.say(text.trim_end());
            }
            Ok(())
        }
        Cmd::List => {
            let prof = Profiles::load(ctx.sys, ctx.paths)?;
            if ctx.json {
                let rows: Vec<_> = prof
                    .profiles
                    .iter()
                    .map(|p| {
                        serde_json::json!({
                            "name": p.name, "label": p.node.label, "host": p.node.host, "port": p.node.port,
                            "source": p.source, "active": prof.active.as_deref() == Some(p.name.as_str()),
                        })
                    })
                    .collect();
                ctx.out = serde_json::to_string_pretty(&rows)
                    .map_err(|e| Error::parse("list", e.to_string()))?;
            } else {
                let text = menu::render_nodes(&prof);
                ctx.say(text.trim_end());
            }
            Ok(())
        }
        Cmd::Switch { name } => {
            let mut prof = Profiles::load(ctx.sys, ctx.paths)?;
            if !prof.profiles.iter().any(|p| &p.name == name) {
                return Err(Error::msg(format!(
                    "节点 {name} 不存在，用 `bui-c list` 看可用节点"
                )));
            }
            if prof.active.as_deref() == Some(name.as_str()) {
                ctx.say(format!("已是当前节点：{name}"));
                return Ok(());
            }
            prof.active = Some(name.clone());
            prof.save(ctx.sys, ctx.paths)?;
            apply_with_ufw(ctx, &prof)?;
            ctx.say(format!("已切换到 {name}"));
            Ok(())
        }
        Cmd::Mode { mode } => {
            let mut prof = Profiles::load(ctx.sys, ctx.paths)?;
            let want: Mode = (*mode).into();
            prof.mode = want;
            prof.save(ctx.sys, ctx.paths)?;
            apply_with_ufw(ctx, &prof)?;
            ctx.say(match want {
                Mode::Tun => "已切到 TUN 模式",
                Mode::Socks => "已切到 SOCKS 模式",
            });
            Ok(())
        }
        Cmd::Import {
            uri,
            panel,
            user,
            sub,
            activate,
        } => {
            let mut prof = Profiles::load(ctx.sys, ctx.paths)?;
            let had_active = prof.active_profile().is_some();
            let (fetched, src, panel_rec) = if let (Some(base), Some(u)) =
                (panel.as_ref(), user.as_ref())
            {
                let f = source::from_panel(ctx.net, base, u)?;
                (
                    f,
                    Source::ApiNodes,
                    Some(Panel {
                        base_url: base.trim_end_matches('/').to_string(),
                        username: u.clone(),
                    }),
                )
            } else if let Some(url) = sub.as_ref() {
                let f = source::from_subscription(ctx.net, url)?;
                // 从订阅 URL 反推面板地址：不记的话每日自更新会跳过面板源，只打 GitHub
                let rec = source::origin(url).map(|base_url| Panel {
                    base_url,
                    username: f.user.clone(),
                });
                (f, Source::Subscription, rec)
            } else if let Some(raw) = uri.as_ref() {
                if raw != "-" {
                    ctx.say("提示：位置参数会把 HY2 密码留在 shell 历史与 ps 里，下次用 `bui-c import -` 从标准输入粘贴");
                }
                let lines = if raw == "-" {
                    ctx.prompt.lines_until_blank("粘贴节点链接")?
                } else {
                    vec![raw.clone()]
                };
                (source::from_uris(&lines)?, Source::Paste, None)
            } else {
                return Err(Error::msg(
                    "给一个节点链接，或用 --panel <地址> --user <用户名>，或 --sub <订阅地址>",
                ));
            };
            let stored = store_fetched(ctx, &mut prof, &fetched, src, panel_rec);
            if *activate || !had_active {
                if let Some(name) = stored.names.first() {
                    prof.active = Some(name.clone());
                }
            }
            prof.save(ctx.sys, ctx.paths)?;
            ctx.say(format!(
                "导入 {} 个新节点，共 {} 个",
                stored.added,
                prof.profiles.len()
            ));
            if *activate || !had_active {
                apply_with_ufw(ctx, &prof)?;
                ctx.say(format!(
                    "当前节点：{}",
                    prof.active.clone().unwrap_or_default()
                ));
            }
            Ok(())
        }
        Cmd::Check => {
            let v = check::run(ctx.sys, ctx.net, ctx.paths)?;
            match &v {
                Verdict::NoProfile => ctx.say("没有激活的节点，巡检跳过"),
                Verdict::Ok => ctx.say("正常：单元在跑、204 探测通过"),
                Verdict::Restarted {
                    failures,
                    next_backoff_min,
                } => {
                    ctx.say(format!(
                        "发现 {} 项异常，已重启 bui-c.service，下次退避 {next_backoff_min} 分钟",
                        failures.len()
                    ));
                    for f in failures {
                        ctx.say(format!("  - {f:?}"));
                    }
                }
                Verdict::Waiting {
                    failures,
                    remaining_s,
                } => {
                    ctx.say(format!(
                        "仍有 {} 项异常，退避中，{remaining_s}s 后再试",
                        failures.len()
                    ));
                }
            }
            // 每日自更新：失败只记日志，不影响巡检结论与退出码
            let prof = Profiles::load(ctx.sys, ctx.paths)?;
            let mut rt = Runtime::load(ctx.sys, ctx.paths);
            if check::update_due(ctx.sys, &rt, &prof) {
                // 先把「尝试过」落盘再联网：面板与 GitHub 都不可达时按 check::UPDATE_RETRY_S
                // 退避 1 小时，否则离线机器每分钟白等两个源各 15s
                rt.last_update_attempt_at = Some(ctx.sys.now().unix_timestamp());
                rt.save(ctx.sys, ctx.paths)?;
                match update::run(
                    ctx.sys,
                    ctx.net,
                    ctx.paths,
                    &with_panel_override(ctx.sys, &prof),
                    false,
                ) {
                    Ok(r) => {
                        rt.last_update_at = Some(ctx.sys.now().unix_timestamp());
                        // 自更新也是一次「检查更新」：装完了就把菜单上的 ★ 摘掉
                        rt.update_available = new_version_pending(&r);
                        rt.update_checked_at = rt.last_update_at;
                        rt.save(ctx.sys, ctx.paths)?;
                        if r.self_updated || r.kernel_updated {
                            ctx.say(format!(
                                "自更新：bui-c={} 内核={}",
                                r.self_updated, r.kernel_updated
                            ));
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "自更新失败，下轮再试"),
                }
            }
            Ok(())
        }
        Cmd::Update { check_only, auto } => {
            let mut prof = Profiles::load(ctx.sys, ctx.paths)?;
            // --auto 只翻开关、不联网：spec §6「每日 timer 自动，可关」的 CLI 入口
            if let Some(sw) = auto {
                prof.auto_update = matches!(sw, Switch::On);
                prof.save(ctx.sys, ctx.paths)?;
                ctx.say(format!(
                    "每日自动更新：{}",
                    if prof.auto_update { "开" } else { "关" }
                ));
                return Ok(());
            }
            let r = update::run(
                ctx.sys,
                ctx.net,
                ctx.paths,
                &with_panel_override(ctx.sys, &prof),
                *check_only,
            )?;
            ctx.say(format!(
                "manifest {}（来源 {}）",
                r.manifest_version, r.manifest_source
            ));
            if *check_only {
                ctx.say(if r.manifest_version == crate::VERSION {
                    "已是最新"
                } else {
                    "有新版，跑 `bui-c update` 升级"
                });
            } else {
                ctx.say(format!(
                    "自身更新={} 内核更新={} 已重启={}",
                    r.self_updated, r.kernel_updated, r.restarted
                ));
            }
            let mut rt = Runtime::load(ctx.sys, ctx.paths);
            let now = ctx.sys.now().unix_timestamp();
            rt.update_available = new_version_pending(&r);
            rt.update_checked_at = Some(now);
            if !*check_only {
                rt.last_update_at = Some(now);
                rt.last_update_attempt_at = Some(now);
            }
            rt.save(ctx.sys, ctx.paths)?;
            Ok(())
        }
        Cmd::ImportV3 { base, panel, mode } => {
            let mut prof = Profiles::load(ctx.sys, ctx.paths)?;
            let dir = base
                .clone()
                .unwrap_or_else(|| PathBuf::from(import_v3::V3_BASE));
            let opts = import_v3::RunOpts {
                panel: panel.clone().or_else(|| ctx.sys.env("BUI_C_PANEL")),
                mode: mode.map(Into::into),
            };
            let r = import_v3::run(ctx.sys, ctx.net, ctx.paths, &dir, &mut prof, &opts)?;
            if r.kernel_installed {
                ctx.say("已安装 sing-box 内核");
            }
            ctx.say(format!(
                "导入 {} 个节点，卸载 {} 个旧单元",
                r.imported.len(),
                r.removed_units.len()
            ));
            for s in &r.skipped {
                ctx.say(format!("跳过：{s}"));
            }
            if r.ufw_restored {
                ctx.say("已恢复被 v3 关掉的 UFW");
            }
            apply_with_ufw(ctx, &prof)?;
            ctx.say(format!(
                "当前节点：{}",
                prof.active.clone().unwrap_or_default()
            ));
            Ok(())
        }
        Cmd::Uninstall { purge_bin } => {
            if !ctx.yes && !ctx.prompt.confirm("确认卸载 bui-c（节点配置一并删除）？")?
            {
                ctx.say("已取消");
                return Ok(());
            }
            let r = uninstall::run(ctx.sys, ctx.paths, *purge_bin)?;
            ctx.say(format!(
                "已删除 {} 个单元，配置目录={} 二进制={}",
                r.removed_units.len(),
                r.purged_base,
                r.purged_bin
            ));
            Ok(())
        }
    }
}

/// 决策 10 / spec §6「import-v3：首次运行从 /opt/hysteria-client/ 导入」的接线点：
/// 没有任何 profile 且机器上有 v3 客户端目录时，进菜单前问一次。只问一次；
/// 答否就给出手动入口。`check` / `update` 这类非交互路径**不**走这里——
/// 让 timer 悄悄改用户配置是更坏的行为。
fn offer_v3_import<S: Sys, N: Net, P: Prompt>(ctx: &mut Ctx<'_, S, N, P>) -> Result<()> {
    let prof = Profiles::load(ctx.sys, ctx.paths)?;
    if !prof.profiles.is_empty() {
        return Ok(());
    }
    let base = PathBuf::from(import_v3::V3_BASE);
    if !import_v3::detect(ctx.sys, &base) {
        return Ok(());
    }
    ctx.say(format!("发现 v3 客户端目录 {}", base.display()));
    ctx.flush();
    if ctx.prompt.confirm("现在导入 v3 的节点并卸载旧单元吗？")? {
        let sub = Cli {
            json: false,
            yes: ctx.yes,
            cmd: Some(Cmd::ImportV3 {
                base: None,
                panel: None,
                mode: None,
            }),
        };
        if let Err(e) = dispatch(&sub, ctx) {
            ctx.say(format!("导入失败：{e}"));
        }
    } else {
        ctx.say("已跳过。随时可以跑 `bui-c import-v3`，或在菜单里选 7");
    }
    ctx.flush();
    Ok(())
}

pub fn menu_loop<S: Sys, N: Net, P: Prompt>(ctx: &mut Ctx<'_, S, N, P>) -> Result<()> {
    offer_v3_import(ctx)?;
    loop {
        let prof = Profiles::load(ctx.sys, ctx.paths)?;
        let st = engine_status(ctx, &prof);
        let screen = menu::render(&st);
        ctx.say(screen.trim_end());
        ctx.flush();
        let choice = ctx.prompt.line("选择 [0-9]")?;
        if choice.is_empty() {
            return Ok(()); // EOF / 直接回车 → 退出，不留在死循环里
        }
        let action = match menu::parse_choice(&choice) {
            Some(a) => a,
            None => {
                ctx.say(format!("无效选项：{choice}"));
                ctx.flush();
                continue;
            }
        };
        let cmd = match action {
            Action::Quit => return Ok(()),
            Action::SwitchNode => {
                let list = menu::render_nodes(&prof);
                ctx.say(list.trim_end());
                ctx.flush();
                let pick = ctx.prompt.line("选择节点编号")?;
                menu::pick_index(&pick, prof.profiles.len()).map(|i| Cmd::Switch {
                    name: prof.profiles[i].name.clone(),
                })
            }
            Action::ToggleMode => {
                let want = match prof.mode {
                    Mode::Socks => ModeArg::Tun,
                    Mode::Tun => ModeArg::Socks,
                };
                let q = match want {
                    ModeArg::Tun => "切换到 TUN 全局模式？",
                    ModeArg::Socks => "切换到 SOCKS 模式？",
                };
                if ctx.prompt.confirm(q)? {
                    Some(Cmd::Mode { mode: want })
                } else {
                    None
                }
            }
            Action::ImportNode => Some(Cmd::Import {
                uri: Some("-".into()),
                panel: None,
                user: None,
                sub: None,
                activate: false,
            }),
            Action::Service => {
                // 服务控制：只保留「重启当前配置」这一个真实动作（v3 的服务子菜单大半是死代码）
                systemd::restart(ctx.sys, UNIT_MAIN)?;
                ctx.say("已重启 bui-c.service");
                None
            }
            Action::Check => Some(Cmd::Check),
            Action::Update => Some(Cmd::Update {
                check_only: false,
                auto: None,
            }),
            Action::AutoUpdate => Some(Cmd::Update {
                check_only: false,
                auto: Some(if prof.auto_update {
                    Switch::Off
                } else {
                    Switch::On
                }),
            }),
            Action::ImportV3 => Some(Cmd::ImportV3 {
                base: None,
                panel: None,
                mode: None,
            }),
            Action::Uninstall => Some(Cmd::Uninstall { purge_bin: false }),
        };
        if let Some(cmd) = cmd {
            let sub = Cli {
                json: false,
                yes: ctx.yes,
                cmd: Some(cmd),
            };
            if let Err(e) = dispatch(&sub, ctx) {
                ctx.say(format!("失败：{e}"));
            }
        }
        ctx.flush();
    }
}

pub fn is_root() -> bool {
    nix::unistd::Uid::effective().is_root()
}

pub fn run() -> std::process::ExitCode {
    let cli = Cli::parse();
    let level = match std::env::var("BUI_C_LOG").as_deref() {
        Ok("debug") => tracing::Level::DEBUG,
        Ok("warn") => tracing::Level::WARN,
        _ => tracing::Level::INFO,
    };
    // journald 自己带时间戳，这里不重复
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(level)
        .without_time()
        .try_init();

    if !is_root() {
        eprintln!("bui-c 需要 root（要写 /etc/systemd/system 与建 TUN 接口）：加 sudo 重试");
        return std::process::ExitCode::from(2);
    }

    let sys = crate::sys::RealSys;
    let net = crate::net::ReqwestNet;
    let paths = Paths::from_env();
    let mut prompt = menu::Stdin;
    let mut ctx = Ctx::new(&sys, &net, &paths, &mut prompt, cli.json, cli.yes);
    let rc = match dispatch(&cli, &mut ctx) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            ctx.say(format!("错误：{e}"));
            std::process::ExitCode::FAILURE
        }
    };
    ctx.flush();
    rc
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::Runtime;
    use crate::fake::{FakeNet, FakeReply, FakeSys};
    use crate::menu::Scripted;
    use crate::profiles::{Mode, Profiles};
    use crate::testutil::{hy2_direct_node, profiles_socks, reality_direct_node, split_keywords};
    use base64::Engine as _;
    use pretty_assertions::assert_eq;

    fn paths() -> Paths {
        Paths::new("/opt/bui-c", "/etc/systemd/system")
    }

    fn ready(s: &FakeSys) {
        s.put("/opt/bui-c/bin/sing-box", "ELF");
        s.reply("systemctl is-active --quiet bui-c.service", 0, "");
        s.reply("systemctl is-enabled --quiet bui-c.service", 0, "");
        s.reply("systemctl is-active --quiet bui-c.timer", 0, "");
        s.reply("systemctl is-enabled --quiet bui-c.timer", 0, "");
        s.reply("ufw status", 127, "");
    }

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("bui-c").chain(args.iter().copied())).unwrap()
    }

    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory as _;
        Cli::command().debug_assert();
    }

    #[test]
    fn every_subcommand_parses() {
        assert_eq!(parse(&["status"]).cmd, Some(Cmd::Status));
        assert!(parse(&["list", "--json"]).json);
        assert_eq!(
            parse(&["switch", "alice-hy2-direct"]).cmd,
            Some(Cmd::Switch {
                name: "alice-hy2-direct".into()
            })
        );
        assert_eq!(
            parse(&["mode", "tun"]).cmd,
            Some(Cmd::Mode { mode: ModeArg::Tun })
        );
        assert_eq!(
            parse(&[
                "import",
                "--panel",
                "https://panel.example.com",
                "--user",
                "alice",
                "--activate"
            ])
            .cmd,
            Some(Cmd::Import {
                uri: None,
                panel: Some("https://panel.example.com".into()),
                user: Some("alice".into()),
                sub: None,
                activate: true
            })
        );
        assert_eq!(
            parse(&["import", "hysteria2://x@h:1#a"]).cmd,
            Some(Cmd::Import {
                uri: Some("hysteria2://x@h:1#a".into()),
                panel: None,
                user: None,
                sub: None,
                activate: false
            })
        );
        assert_eq!(parse(&["check"]).cmd, Some(Cmd::Check));
        assert_eq!(
            parse(&["update", "--check-only"]).cmd,
            Some(Cmd::Update {
                check_only: true,
                auto: None
            })
        );
        assert_eq!(
            parse(&["update", "--auto", "off"]).cmd,
            Some(Cmd::Update {
                check_only: false,
                auto: Some(Switch::Off)
            })
        );
        assert_eq!(
            parse(&["update", "--auto", "on"]).cmd,
            Some(Cmd::Update {
                check_only: false,
                auto: Some(Switch::On)
            })
        );
        assert_eq!(
            parse(&["import-v3"]).cmd,
            Some(Cmd::ImportV3 {
                base: None,
                panel: None,
                mode: None
            })
        );
        assert_eq!(
            parse(&["import-v3", "--panel", "https://p", "--mode", "socks"]).cmd,
            Some(Cmd::ImportV3 {
                base: None,
                panel: Some("https://p".into()),
                mode: Some(ModeArg::Socks)
            })
        );
        assert_eq!(
            parse(&["uninstall", "--purge-bin", "-y"]).cmd,
            Some(Cmd::Uninstall { purge_bin: true })
        );
        assert_eq!(parse(&[]).cmd, None, "无参数进菜单");
    }

    #[test]
    fn status_json_has_the_documented_shape() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, true, false);
        dispatch(&parse(&["status", "--json"]), &mut ctx).unwrap();
        let v: serde_json::Value = serde_json::from_str(&ctx.out).unwrap();
        assert_eq!(v["active"], "alice-hy2-direct");
        assert_eq!(v["label"], "HY2直连");
        assert_eq!(v["mode"], "socks");
        assert_eq!(v["service"], "running");
        assert_eq!(v["timer"], "active");
        assert_eq!(v["tun"], "down");
        assert_eq!(v["socks_port"], 1080);
        assert_eq!(v["http_port"], 8080);
        assert_eq!(v["profiles"], 1);
        assert_eq!(v["auto_update"], true);
        assert_eq!(v["version"], crate::VERSION);
        // "tun": "down" 靠的是 FakeSys 对未登记 `ip link show bui-tun` 的默认非零
    }

    #[test]
    fn switch_sets_active_persists_and_applies() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        let mut prof = profiles_socks();
        prof.upsert(crate::profiles::Profile {
            name: "alice-reality-direct".into(),
            node: reality_direct_node(),
            split: split_keywords(),
            source: crate::profiles::Source::ApiNodes,
            imported_at: "2026-09-11T00:00:00Z".into(),
        });
        prof.save(&s, &pp).unwrap();
        let n = FakeNet::new();
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["switch", "alice-reality-direct"]), &mut ctx).unwrap();
        assert_eq!(
            Profiles::load(&s, &pp).unwrap().active.as_deref(),
            Some("alice-reality-direct")
        );
        assert!(
            s.called("systemctl restart bui-c.service"),
            "配置变了要重启"
        );
        assert!(ctx.out.contains("alice-reality-direct"));
    }

    #[test]
    fn switch_unknown_name_errors_without_touching_state() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        let e = dispatch(&parse(&["switch", "nope"]), &mut ctx).unwrap_err();
        assert!(e.to_string().contains("nope"), "{e}");
        assert_eq!(
            Profiles::load(&s, &pp).unwrap().active.as_deref(),
            Some("alice-hy2-direct")
        );
        assert!(!s.called("systemctl restart bui-c.service"));
    }

    #[test]
    fn mode_tun_adds_ufw_rules_and_socks_revokes_them() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        s.reply("ufw status", 0, "Status: active\n");
        s.reply("ip link show bui-tun", 0, "5: bui-tun");
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        let mut p = Scripted::from([]);

        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["mode", "tun"]), &mut ctx).unwrap();
        assert_eq!(Profiles::load(&s, &pp).unwrap().mode, Mode::Tun);
        assert!(s.called("ufw allow in on bui-tun"));
        assert!(s.called("ufw route allow in on bui-tun"));
        assert!(Runtime::load(&s, &pp).ufw_rules);
        assert!(!s.calls().iter().any(|c| c == "ufw disable"), "v4 不关整墙");

        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["mode", "socks"]), &mut ctx).unwrap();
        assert_eq!(Profiles::load(&s, &pp).unwrap().mode, Mode::Socks);
        assert!(s.called("ufw delete allow in on bui-tun"));
        assert!(!Runtime::load(&s, &pp).ufw_rules);
    }

    #[test]
    fn import_from_panel_saves_profiles_and_activates_the_first() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        let n = FakeNet::new();
        let payload = crate::source::NodesPayload {
            user: "alice".into(),
            split: split_keywords(),
            nodes: vec![reality_direct_node(), hy2_direct_node()],
        };
        n.route(
            "https://panel.example.com/api/nodes/alice",
            FakeReply::Text(serde_json::to_string(&payload).unwrap()),
        );
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(
            &parse(&[
                "import",
                "--panel",
                "https://panel.example.com",
                "--user",
                "alice",
            ]),
            &mut ctx,
        )
        .unwrap();
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(
            saved
                .profiles
                .iter()
                .map(|x| x.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alice-reality-direct", "alice-hy2-direct"]
        );
        assert_eq!(
            saved.active.as_deref(),
            Some("alice-reality-direct"),
            "首次导入自动激活第一个"
        );
        assert_eq!(
            saved.panel.as_ref().unwrap().base_url,
            "https://panel.example.com"
        );
        assert_eq!(
            saved.profiles[0].split,
            split_keywords(),
            "住宅分流规则来自面板"
        );
        assert!(s.exists(std::path::Path::new("/opt/bui-c/config.json")));
    }

    #[test]
    fn import_pasted_uri_does_not_activate_without_the_flag() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        let uri = "vless://11111111-1111-4111-8111-111111111111@panel.example.com:10001?encryption=none&security=reality&sni=www.bing.com&fp=chrome&pbk=PUB&sid=0123456789abcdef&flow=xtls-rprx-vision&type=tcp#bob-Reality%E7%9B%B4%E8%BF%9E";
        dispatch(&parse(&["import", uri]), &mut ctx).unwrap();
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(saved.profiles.len(), 2);
        assert_eq!(
            saved.active.as_deref(),
            Some("alice-hy2-direct"),
            "已有 active，不抢"
        );
        assert!(
            !s.called("systemctl restart bui-c.service"),
            "只导入不切换 → 不动服务"
        );
        assert!(
            ctx.out.contains("bui-c import -"),
            "位置参数要提醒改用标准输入：{}",
            ctx.out
        );
    }

    #[test]
    fn check_dispatch_reports_verdict_and_runs_due_update() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        let mut prof = profiles_socks();
        prof.panel = Some(crate::profiles::Panel {
            base_url: "https://panel.example.com".into(),
            username: "alice".into(),
        });
        prof.save(&s, &pp).unwrap();
        s.reply(
            "/opt/bui-c/bin/sing-box version",
            0,
            "sing-box version 1.14.5\n",
        );
        let n = FakeNet::new();
        n.route(crate::check::PROBE_URL, FakeReply::Status(204));
        n.route(
            "https://panel.example.com/packages/manifest.json",
            FakeReply::Text(format!(
                r#"{{"version":"{}","kernels":{{"client_sing_box":"1.14.5"}},"artifacts":{{}}}}"#,
                crate::VERSION
            )),
        );
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["check"]), &mut ctx).unwrap();
        assert!(ctx.out.contains("正常"), "{}", ctx.out);
        assert!(
            n.log().iter().any(|l| l.contains("manifest.json")),
            "首次巡检顺手自更新"
        );
        let rt = Runtime::load(&s, &pp);
        assert!(rt.last_update_at.is_some());
        assert!(
            rt.last_update_attempt_at.is_some(),
            "尝试时间也要落盘，供 1 小时退避用"
        );
    }

    #[test]
    fn check_does_not_fail_when_the_update_source_is_down() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        let mut prof = profiles_socks();
        prof.panel = Some(crate::profiles::Panel {
            base_url: "https://panel.example.com".into(),
            username: "alice".into(),
        });
        prof.save(&s, &pp).unwrap();
        let n = FakeNet::new();
        n.route(crate::check::PROBE_URL, FakeReply::Status(204));
        // manifest 所有源都没登记 → fetch_manifest 报错
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["check"]), &mut ctx).unwrap();
        assert!(
            ctx.out.contains("正常"),
            "巡检结论不受自更新失败影响：{}",
            ctx.out
        );
    }

    #[test]
    fn uninstall_needs_confirmation_unless_yes() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        s.put("/etc/systemd/system/bui-c.service", "[Unit]");
        let n = FakeNet::new();

        let mut p = Scripted::from(["n"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["uninstall"]), &mut ctx).unwrap();
        assert!(
            s.exists(std::path::Path::new("/etc/systemd/system/bui-c.service")),
            "回答 n 不卸载"
        );
        assert!(ctx.out.contains("已取消"));

        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, true);
        dispatch(&parse(&["uninstall", "-y"]), &mut ctx).unwrap();
        assert!(!s.exists(std::path::Path::new("/etc/systemd/system/bui-c.service")));
    }

    #[test]
    fn import_v3_dispatch_imports_and_applies() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        s.put(
            "/opt/hysteria-client/configs/hysteria2-1/uri.txt",
            "hysteria2://alice:hy2-pw@panel.example.com:10000/?sni=panel.example.com&mport=20000-30000#alice-HY2%E7%9B%B4%E8%BF%9E",
        );
        s.put(
            "/opt/hysteria-client/configs/hysteria2-1/meta.json",
            r#"{"socks_port":1080,"http_port":8080}"#,
        );
        s.put("/opt/hysteria-client/active", "hysteria2-1");
        let n = FakeNet::new();
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["import-v3"]), &mut ctx).unwrap();
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(saved.profiles.len(), 1);
        assert_eq!(saved.active.as_deref(), Some("alice-hy2-direct"));
        assert!(s.called("systemctl stop hysteria-client.service"));
        assert!(s.exists(std::path::Path::new("/opt/bui-c/config.json")));
        assert!(ctx.out.contains("导入 1 个节点"));
    }

    #[test]
    fn update_check_only_caches_the_new_version_flag_for_the_menu() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        let mut prof = profiles_socks();
        prof.panel = Some(crate::profiles::Panel {
            base_url: "https://panel.example.com".into(),
            username: "alice".into(),
        });
        prof.save(&s, &pp).unwrap();
        let n = FakeNet::new();
        let manifest = |ver: &str| {
            FakeReply::Text(format!(
                r#"{{"version":"{ver}","kernels":{{"client_sing_box":"1.14.5"}},"artifacts":{{}}}}"#
            ))
        };
        n.route(
            "https://panel.example.com/packages/manifest.json",
            manifest("9.9.9"),
        );

        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["update", "--check-only"]), &mut ctx).unwrap();
        let rt = Runtime::load(&s, &pp);
        assert!(rt.update_available, "检查到新版要落盘给菜单用");
        assert_eq!(rt.update_checked_at, Some(s.now().unix_timestamp()));

        // 菜单 / status 从 runtime.json 读这个标记，不再联网
        let n2 = FakeNet::new(); // 一个源都没登记：真联网必然报错
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n2, &pp, &mut p, false, false);
        dispatch(&parse(&["status"]), &mut ctx).unwrap();
        assert!(ctx.out.contains("★ 有新版"), "{}", ctx.out);
        assert!(n2.log().is_empty(), "渲染菜单不该联网");

        // 再查一次，manifest 与本机同版 → 标记清掉
        n.route(
            "https://panel.example.com/packages/manifest.json",
            manifest(crate::VERSION),
        );
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["update", "--check-only"]), &mut ctx).unwrap();
        assert!(!Runtime::load(&s, &pp).update_available);
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n2, &pp, &mut p, false, false);
        dispatch(&parse(&["status"]), &mut ctx).unwrap();
        assert!(!ctx.out.contains("★ 有新版"), "{}", ctx.out);
    }

    #[test]
    fn update_auto_off_only_flips_the_switch() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new(); // 一个源都没登记：真去拉 manifest 必然报错
        let mut p = Scripted::from([]);

        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["update", "--auto", "off"]), &mut ctx).unwrap();
        assert!(!Profiles::load(&s, &pp).unwrap().auto_update);
        assert!(n.log().is_empty(), "只改开关，不联网");
        assert!(ctx.out.contains("每日自动更新：关"), "{}", ctx.out);

        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["update", "--auto", "on"]), &mut ctx).unwrap();
        assert!(Profiles::load(&s, &pp).unwrap().auto_update);
        assert!(n.log().is_empty());
    }

    #[test]
    fn import_sub_records_the_panel_for_later_updates() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        let n = FakeNet::new();
        let uris = "hysteria2://alice:hy2-pw@panel.example.com:10000/?sni=panel.example.com&mport=20000-30000#alice-HY2%E7%9B%B4%E8%BF%9E";
        n.route(
            "https://panel.example.com/api/sub/alice",
            FakeReply::Text(base64::engine::general_purpose::STANDARD.encode(uris)),
        );
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(
            &parse(&["import", "--sub", "https://panel.example.com/api/sub/alice"]),
            &mut ctx,
        )
        .unwrap();
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(saved.profiles.len(), 1);
        // 面板地址从订阅 URL 反推：不记的话每日自更新只会去打 GitHub
        assert_eq!(
            saved.panel.as_ref().map(|x| x.base_url.as_str()),
            Some("https://panel.example.com")
        );
        assert_eq!(
            saved.panel.as_ref().map(|x| x.username.as_str()),
            Some("alice")
        );
    }

    #[test]
    fn check_skips_the_self_update_inside_the_retry_window() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        let mut prof = profiles_socks();
        prof.panel = Some(crate::profiles::Panel {
            base_url: "https://panel.example.com".into(),
            username: "alice".into(),
        });
        prof.save(&s, &pp).unwrap();
        let n = FakeNet::new();
        n.route(crate::check::PROBE_URL, FakeReply::Status(204));
        // manifest 的两个源都没登记 → 自更新失败，但「尝试过」要落盘
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["check"]), &mut ctx).unwrap();
        let tried = n.log().len();
        assert_eq!(tried, 3, "204 探测 + 面板 + GitHub：{:?}", n.log());
        assert!(Runtime::load(&s, &pp).last_update_attempt_at.is_some());

        // 一分钟后再巡检：还在 1 小时退避窗口里 → 不再打更新源
        s.advance(60);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["check"]), &mut ctx).unwrap();
        assert_eq!(n.log().len(), tried + 1, "只多了一次 204 探测");
    }

    #[test]
    fn first_run_offers_the_v3_import_once() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        s.put(
            "/opt/hysteria-client/configs/hysteria2-1/uri.txt",
            "hysteria2://alice:hy2-pw@panel.example.com:10000/?sni=panel.example.com&mport=20000-30000#alice-HY2%E7%9B%B4%E8%BF%9E",
        );
        s.put("/opt/hysteria-client/active", "hysteria2-1");
        let n = FakeNet::new();
        // 没有 profiles.json + 有 v3 目录 → 进菜单前问一次；y 导入，再 0 退出
        let mut p = Scripted::from(["y", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(saved.profiles.len(), 1);
        assert_eq!(saved.active.as_deref(), Some("alice-hy2-direct"));
        assert!(s.called("systemctl stop hysteria-client.service"));

        // 答 n：不导入，只提示一次，菜单照常进（ctx.out 会被 flush 清空，断言看 transcript）
        let s2 = FakeSys::new();
        ready(&s2);
        s2.put(
            "/opt/hysteria-client/configs/hysteria2-1/uri.txt",
            "hysteria2://alice:hy2-pw@panel.example.com:10000/?sni=panel.example.com#alice-HY2%E7%9B%B4%E8%BF%9E",
        );
        let mut p2 = Scripted::from(["n", "0"]);
        let mut ctx2 = Ctx::new(&s2, &n, &pp, &mut p2, false, false);
        menu_loop(&mut ctx2).unwrap();
        assert!(Profiles::load(&s2, &pp).unwrap().profiles.is_empty());
        assert!(
            ctx2.transcript.contains("bui-c import-v3"),
            "{}",
            ctx2.transcript
        );
        assert!(!s2.called("systemctl stop hysteria-client.service"));
    }

    #[test]
    fn menu_loop_toggles_mode_then_quits() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        s.reply("ip link show bui-tun", 0, "5: bui-tun");
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        // 2 = 切换模式 → 确认 y → 0 = 退出
        let mut p = Scripted::from(["2", "y", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        assert_eq!(Profiles::load(&s, &pp).unwrap().mode, Mode::Tun);
    }

    #[test]
    fn menu_loop_exits_on_eof() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        let mut p = Scripted::from([]); // 立刻 EOF
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        assert_eq!(
            Profiles::load(&s, &pp).unwrap().mode,
            Mode::Socks,
            "什么都没改"
        );
    }

    #[test]
    fn menu_switch_node_by_number() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        let mut prof = profiles_socks();
        prof.upsert(crate::profiles::Profile {
            name: "alice-reality-direct".into(),
            node: reality_direct_node(),
            split: split_keywords(),
            source: crate::profiles::Source::ApiNodes,
            imported_at: "2026-09-11T00:00:00Z".into(),
        });
        prof.save(&s, &pp).unwrap();
        let n = FakeNet::new();
        let mut p = Scripted::from(["1", "2", "0"]); // 1 = 切换节点 → 选第 2 个 → 0 退出
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        assert_eq!(
            Profiles::load(&s, &pp).unwrap().active.as_deref(),
            Some("alice-reality-direct")
        );
    }

    #[test]
    fn import_v3_panel_flag_is_persisted_and_used_for_the_manifest() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        // 把 ready 预置的内核拿掉：这条用例要的就是「内核得现取」那条路
        s.remove_file(std::path::Path::new("/opt/bui-c/bin/sing-box"))
            .unwrap();
        s.put(
            "/opt/hysteria-client/configs/hysteria2-1/uri.txt",
            "hysteria2://alice:hy2-pw@panel.example.com:10000/?sni=panel.example.com&mport=20000-30000#alice-HY2%E7%9B%B4%E8%BF%9E",
        );
        s.put("/opt/hysteria-client/active", "hysteria2-1");
        // v3 推导出来的面板是 v3 面板，没有 /packages —— 只登记 --panel 那个源
        s.put("/opt/hysteria-client/server_address", "v3.example.com");
        let n = FakeNet::new();
        let bin = b"ELF-sing-box".to_vec();
        n.route(
            "https://other.example.com/packages/manifest.json",
            FakeReply::Text(format!(
                r#"{{"version":"{}","kernels":{{"client_sing_box":"1.14.5"}},"artifacts":{{"sing-box-linux-{a}":{{"url":"https://example.com/sing-box-linux-{a}","sha256":"{sha}"}}}}}}"#,
                crate::VERSION,
                a = update::arch_suffix(),
                sha = update::sha256_hex(&bin),
            )),
        );
        n.route(
            &format!(
                "https://other.example.com/packages/sing-box-linux-{}",
                update::arch_suffix()
            ),
            FakeReply::Bytes(bin),
        );
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(
            &parse(&["import-v3", "--panel", "https://other.example.com"]),
            &mut ctx,
        )
        .unwrap();
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(
            saved.panel.as_ref().map(|x| x.base_url.as_str()),
            Some("https://other.example.com"),
            "--panel 覆盖 v3 推导出的面板并落盘"
        );
        assert!(
            ctx.transcript.contains("已安装 sing-box 内核"),
            "{}",
            ctx.transcript
        );
    }

    #[test]
    fn bui_c_panel_env_overrides_the_manifest_source_without_persisting() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        s.set_env("BUI_C_PANEL", "https://env.example.com");
        let mut prof = profiles_socks();
        prof.panel = Some(crate::profiles::Panel {
            base_url: "https://panel.example.com".into(),
            username: "alice".into(),
        });
        prof.save(&s, &pp).unwrap();
        let n = FakeNet::new();
        // 只有环境变量指向的那个源可达：没生效就会走 panel.example.com 与 GitHub，双双失败
        n.route(
            "https://env.example.com/packages/manifest.json",
            FakeReply::Text(format!(
                r#"{{"version":"{}","kernels":{{"client_sing_box":"1.14.5"}},"artifacts":{{}}}}"#,
                crate::VERSION
            )),
        );
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["update", "--check-only"]), &mut ctx).unwrap();
        assert_eq!(
            Profiles::load(&s, &pp)
                .unwrap()
                .panel
                .as_ref()
                .map(|x| x.base_url.as_str()),
            Some("https://panel.example.com"),
            "只在内存里覆盖，不改 profiles.json"
        );
    }
}
