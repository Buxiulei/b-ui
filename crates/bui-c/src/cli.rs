//! clap 子命令、`Ctx`（可注入的一次会话）、菜单循环与 `run()`。
//!
//! 所有会改机器的子命令都经 `apply_with_ufw`：装内核 → 按模式同步 UFW → `Engine::apply`。
//! root 检查只在 [`run`] 里做，`dispatch` 保持纯注入，单元测试直接调它。

use crate::check::{self, Runtime, Verdict};
use crate::engine::{Applied, Engine};
use crate::menu::{self, Action, Prompt, Status};
use crate::net::Net;
use crate::paths::{Paths, UNIT_MAIN, UNIT_TIMER};
use crate::profiles::{
    profile_name, rfc3339, same_account, Mode, Panel, Profile, Profiles, Source, Upsert,
};
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
    /// [`say`](Self::say) 每行前加的缩进。默认顶格；菜单循环里设成两列，让「无效选项」
    /// 「已切到…」这类结果行跟菜单对齐，而不是顶在最左边。
    pub indent: &'static str,
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
            indent: "",
        }
    }
    /// 说一句（可多行）：每个非空行前加 [`indent`](Self::indent)，空行不留尾随空格。
    pub fn say(&mut self, line: impl AsRef<str>) {
        let mut text = String::new();
        for l in line.as_ref().split('\n') {
            if !l.is_empty() {
                text.push_str(self.indent);
            }
            text.push_str(l);
            text.push('\n');
        }
        self.emit(&text);
    }
    /// 原样打一块已经排好版的文字（菜单、节点列表、状态块），不叠 `indent`。
    pub fn show(&mut self, block: impl AsRef<str>) {
        let mut text = block.as_ref().to_string();
        text.push('\n');
        self.emit(&text);
    }
    fn emit(&mut self, text: &str) {
        self.out.push_str(text);
        self.transcript.push_str(text);
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

/// profile 名是 upsert 的主键，名字按连接身份定：
///
/// 1. 已有同一个连接（[`Profiles::find_same_endpoint`]）→ 沿用它的名字，原地更新
///    label / hop / 分流 / 来源（import-v3 的 `hysteria2-<ts>` 经面板导入不会多出一份）；
/// 2. 否则取 [`profile_name`]：同名的是同一账号（[`same_account`]，换了密码或主机）→
///    凭据轮换，替换；同名的是**另一个**账号 → `-2`、`-3` 另起，绝不覆盖——面板用户名
///    全是中文时名字回落成 `<主机名>-<kind>`，同一台服务器上的家人账号必然撞名。
///
/// upsert 逐个做，同一批里后一个节点看到的「已有同名」就包括前一个，规则一样成立。
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
        let name = match prof.find_same_endpoint(node) {
            Some(same) => same.name.clone(),
            None => {
                let wanted = profile_name(&f.user, node);
                match prof.profiles.iter().find(|p| p.name == wanted) {
                    Some(taken) if !same_account(&taken.node, node) => {
                        let fresh = prof.free_name(&wanted);
                        ctx.say(format!(
                            "节点名 {wanted} 已被另一个账号占用，新节点命名为 {fresh}"
                        ));
                        fresh
                    }
                    _ => wanted,
                }
            }
        };
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

/// 取到手、还没落盘的一批节点：来源与要记下的面板。
struct Incoming {
    fetched: Fetched,
    src: Source,
    panel: Option<Panel>,
}

/// 面板 `/api/nodes/<user>`：唯一带服务端分流规则的来源。
fn fetch_panel<N: Net>(net: &N, base: &str, user: &str) -> Result<Incoming> {
    Ok(Incoming {
        fetched: source::from_panel(net, base, user)?,
        src: Source::ApiNodes,
        panel: Some(Panel {
            base_url: base.trim_end_matches('/').to_string(),
            username: user.to_string(),
        }),
    })
}

/// 订阅地址（base64 URI 列表）。
fn fetch_sub<N: Net>(net: &N, url: &str) -> Result<Incoming> {
    let fetched = source::from_subscription(net, url)?;
    // 从订阅 URL 反推面板地址：不记的话每日自更新会跳过面板源，只打 GitHub
    let panel = source::origin(url).map(|base_url| Panel {
        base_url,
        username: fetched.user.clone(),
    });
    Ok(Incoming {
        fetched,
        src: Source::Subscription,
        panel,
    })
}

/// 落盘一批节点；首次导入或 `activate` 时激活第一个并 apply。
fn save_import<S: Sys, N: Net, P: Prompt>(
    ctx: &mut Ctx<'_, S, N, P>,
    inc: Incoming,
    activate: bool,
) -> Result<()> {
    let mut prof = Profiles::load(ctx.sys, ctx.paths)?;
    let had_active = prof.active_profile().is_some();
    let stored = store_fetched(ctx, &mut prof, &inc.fetched, inc.src, inc.panel);
    if activate || !had_active {
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
    if activate || !had_active {
        apply_with_ufw(ctx, &prof)?;
        ctx.say(format!(
            "当前节点：{}",
            prof.active.clone().unwrap_or_default()
        ));
    }
    Ok(())
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
                let text = menu::render_status(&st);
                ctx.show(text.trim_end());
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
                let text = menu::render_nodes(&prof, false);
                ctx.show(text.trim_end());
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
            let inc = if let (Some(base), Some(u)) = (panel.as_ref(), user.as_ref()) {
                fetch_panel(ctx.net, base, u)?
            } else if let Some(url) = sub.as_ref() {
                fetch_sub(ctx.net, url)?
            } else if let Some(raw) = uri.as_ref() {
                if raw != "-" {
                    ctx.say("提示：位置参数会把 HY2 密码留在 shell 历史与 ps 里，下次用 `bui-c import -` 从标准输入粘贴");
                }
                let lines = if raw == "-" {
                    ctx.prompt.lines_until_blank("粘贴节点链接")?
                } else {
                    vec![raw.clone()]
                };
                Incoming {
                    fetched: source::from_uris(&lines)?,
                    src: Source::Paste,
                    panel: None,
                }
            } else {
                return Err(Error::msg(
                    "给一个节点链接，或用 --panel <地址> --user <用户名>，或 --sub <订阅地址>",
                ));
            };
            save_import(ctx, inc, *activate)
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
                        ctx.say(format!("  - {f}"));
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
                    for f in failures {
                        ctx.say(format!("  - {f}"));
                    }
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
            // v3 目录是回滚素材、按约定留着，所以这条命令（菜单 [7]）在已迁移的机器上
            // 随时可能被再按一次。没有新节点就没什么要 apply 的：省掉 ufw/engine 那趟
            // 往返，也就不会出现「配置字节不变→不重启」的窗口。
            if r.imported.is_empty() {
                let tail = if r.removed_units.is_empty() {
                    "未做任何改动".to_string()
                } else {
                    format!("清掉 {} 个残留的 v3 单元", r.removed_units.len())
                };
                ctx.say(format!(
                    "v3 的 {} 个节点都已导入过（{}），{tail}",
                    r.existing.len(),
                    r.existing.join("、")
                ));
                for s in &r.skipped {
                    ctx.say(format!("跳过：{s}"));
                }
                if r.ufw_restored {
                    ctx.say("已恢复被 v3 关掉的 UFW");
                }
                return Ok(());
            }
            ctx.say(format!(
                "导入 {} 个节点，卸载 {} 个旧单元",
                r.imported.len(),
                r.removed_units.len()
            ));
            if !r.existing.is_empty() {
                ctx.say(format!("{} 个已存在，跳过", r.existing.len()));
            }
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
    // 菜单里的结果行缩进两列跟菜单对齐；任何出口（含 `?` 冒上来的错误）都恢复，
    // 否则 run() 接着打的「错误：…」会沿用菜单缩进
    let saved = std::mem::replace(&mut ctx.indent, "  ");
    let r = menu_body(ctx);
    ctx.indent = saved;
    r
}

fn menu_body<S: Sys, N: Net, P: Prompt>(ctx: &mut Ctx<'_, S, N, P>) -> Result<()> {
    offer_v3_import(ctx)?;
    loop {
        let prof = Profiles::load(ctx.sys, ctx.paths)?;
        let st = engine_status(ctx, &prof);
        let screen = menu::render(&st);
        ctx.show(screen.trim_end());
        ctx.flush();
        // EOF（Ctrl-D、stdin=/dev/null）→ 退出，不留在死循环里；直接回车只重画，
        // 不打「无效选项」——手滑多按一下回车不该把人踢出菜单
        let Some(choice) = ctx.prompt.read("选择 [0-9]")? else {
            return Ok(());
        };
        if choice.is_empty() {
            continue;
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
                let list = menu::render_nodes(&prof, true);
                ctx.show(list.trim_end());
                let len = prof.profiles.len();
                if len == 0 {
                    None // 列表里已经给了「先导入」的引导，没有编号可选
                } else {
                    ctx.flush();
                    let pick = ctx.prompt.line("选择节点编号")?;
                    if menu::is_back(&pick) {
                        None
                    } else if let Some(i) = menu::pick_index(&pick, len) {
                        Some(Cmd::Switch {
                            name: prof.profiles[i].name.clone(),
                        })
                    } else {
                        ctx.say(format!("无效编号：{pick}（可选 1-{len}，0 返回）"));
                        None
                    }
                }
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
                    ctx.say("已取消，模式未变");
                    None
                }
            }
            Action::ImportNode => {
                menu_import(ctx)?;
                None
            }
            Action::Service => {
                // 单元还没建就别进子菜单——systemd 只会回 `Unit bui-c.service not found`，
                // 用户看不出该干什么（缺陷 5）。
                if ctx.sys.exists(&ctx.paths.unit(UNIT_MAIN)) {
                    service_menu(ctx, prof.mode)?;
                } else {
                    ctx.say("还没有安装引擎与单元：先导入节点（菜单 3 / 7）或跑 `bui-c update`");
                }
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
            // 没有 v3 目录不是失败：菜单里如实说一句。命令行 `bui-c import-v3` 照旧报错，
            // 脚本要靠退出码
            Action::ImportV3
                if !import_v3::detect(ctx.sys, std::path::Path::new(import_v3::V3_BASE)) =>
            {
                ctx.say(format!(
                    "这台机器上没有 v3 客户端（{} 不存在），不需要导入",
                    import_v3::V3_BASE
                ));
                None
            }
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

/// 菜单 `[3]` 的粘贴提示（`lines_until_blank` 自己补「每行一个，空行结束」）。
const PASTE_PROMPT: &str = "粘贴 hysteria2:// 或 vless:// 节点链接，或面板给的订阅地址";

/// 菜单 `[3] 导入节点`：节点链接与面板 / 订阅地址都从这一个口子进。
///
/// 导入失败只打「失败：…」留在菜单里。导入了新节点、而活动节点不在其中时追问一次要不要
/// 切过去——命令行 `bui-c import` 不问，保持非交互。
fn menu_import<S: Sys, N: Net, P: Prompt>(ctx: &mut Ctx<'_, S, N, P>) -> Result<()> {
    let lines: Vec<String> = ctx
        .prompt
        .lines_until_blank(PASTE_PROMPT)?
        .into_iter()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() {
        ctx.say("已取消，没有导入任何节点");
        return Ok(());
    }
    let before: Vec<String> = Profiles::load(ctx.sys, ctx.paths)?
        .profiles
        .into_iter()
        .map(|p| p.name)
        .collect();
    let imported = match lines.as_slice() {
        [url] if source::is_http_url(url) => import_http(ctx, url),
        _ => source::from_uris(&lines).and_then(|fetched| {
            let inc = Incoming {
                fetched,
                src: Source::Paste,
                panel: None,
            };
            save_import(ctx, inc, false)
        }),
    };
    if let Err(e) = imported {
        ctx.say(format!("失败：{e}"));
        return Ok(());
    }
    let after = Profiles::load(ctx.sys, ctx.paths)?;
    let fresh: Vec<&str> = after
        .profiles
        .iter()
        .map(|p| p.name.as_str())
        .filter(|n| !before.iter().any(|b| b == n))
        .collect();
    let Some(first) = fresh.first() else {
        return Ok(());
    };
    if after.active.as_deref().is_some_and(|a| fresh.contains(&a)) {
        return Ok(()); // 首次导入已经激活了新节点
    }
    ctx.flush();
    if ctx.prompt.confirm(&format!("切换到新导入的 {first}？"))? {
        let sub = Cli {
            json: false,
            yes: ctx.yes,
            cmd: Some(Cmd::Switch {
                name: first.to_string(),
            }),
        };
        if let Err(e) = dispatch(&sub, ctx) {
            ctx.say(format!("失败：{e}"));
        }
    }
    Ok(())
}

/// 单独一行的 http(s) 地址：面板的四种按用户地址走 `/api/nodes`（带分流规则），
/// 其余当订阅地址。`/api/sub/` 在面板接口取不到时退回订阅——v3 面板（bwg-tizi）
/// 没有 `/api/nodes`，但 `/api/sub` 在。只在「取」失败时退回：面板节点取到了、
/// 后面 apply 失败时再按订阅导一遍，会把服务端分流规则换成默认表。
fn import_http<S: Sys, N: Net, P: Prompt>(ctx: &mut Ctx<'_, S, N, P>, url: &str) -> Result<()> {
    let inc = match source::panel_link(url) {
        Some(link) => match fetch_panel(ctx.net, &link.base_url, &link.user) {
            Ok(inc) => inc,
            Err(e) if link.path == source::PanelPath::Sub => {
                ctx.say(format!("面板接口取不到（{e}），改用订阅地址导入"));
                ctx.flush();
                fetch_sub(ctx.net, url)?
            }
            Err(e) => return Err(e),
        },
        None => fetch_sub(ctx.net, url)?,
    };
    save_import(ctx, inc, false)
}

/// `[4] 服务控制` 的二级菜单：重启 / 看日志 / 返回（v3 的服务子菜单大半是死代码，
/// 这里只留两个真实动作）。
fn service_menu<S: Sys, N: Net, P: Prompt>(ctx: &mut Ctx<'_, S, N, P>, mode: Mode) -> Result<()> {
    ctx.show(menu::render_service_options().trim_end());
    ctx.flush();
    let pick = ctx.prompt.line("选择 [0-2]")?;
    match menu::parse_service_choice(&pick) {
        Some(menu::ServiceAction::Back) => {}
        None => ctx.say(format!("无效选项：{pick}")),
        Some(menu::ServiceAction::Restart) => restart_service(ctx, mode),
        Some(menu::ServiceAction::Logs) => match journal_tail(ctx.sys, menu::SERVICE_LOG_LINES) {
            Ok(text) => ctx.show(text),
            Err(why) => ctx.say(why),
        },
    }
    Ok(())
}

/// 菜单里的「重启」：TUN 模式要等接口起来才算数（is-active 在 exec 后立刻为真），
/// 没起来就把最近几行日志带出来，省得用户再去翻 journalctl。
fn restart_service<S: Sys, N: Net, P: Prompt>(ctx: &mut Ctx<'_, S, N, P>, mode: Mode) {
    if let Err(e) = systemd::restart(ctx.sys, UNIT_MAIN) {
        ctx.say(format!("失败：{e}"));
        return;
    }
    if mode == Mode::Socks {
        ctx.say("已重启 bui-c.service");
        return;
    }
    if Engine::new(ctx.sys, ctx.paths).wait_tun_ready() {
        ctx.say("已重启 bui-c.service，bui-tun 已就绪");
        return;
    }
    ctx.say(format!(
        "已重启 bui-c.service，但 bui-tun 接口 {} 秒内没起来，最近日志：",
        crate::engine::TUN_READY_WAIT_S
    ));
    match journal_tail(ctx.sys, 10) {
        Ok(text) => ctx.show(text),
        Err(why) => ctx.say(why),
    }
}

/// `journalctl` 取 bui-c.service 最近 `n` 行，去掉 ANSI 颜色；取不到时 `Err` 是一句给用户看的说明。
fn journal_tail<S: Sys>(sys: &S, n: u32) -> std::result::Result<String, String> {
    let n = n.to_string();
    let args = ["-u", UNIT_MAIN, "-n", &n, "--no-pager", "--output", "cat"];
    match sys.run("journalctl", &args) {
        Ok(o) if o.ok() => {
            let text = menu::strip_ansi(o.stdout.trim_end());
            if text.trim().is_empty() {
                Err(format!("journalctl 里还没有 {UNIT_MAIN} 的日志"))
            } else {
                Ok(text)
            }
        }
        Ok(o) => {
            let detail = match o.stderr.trim() {
                "" => String::new(),
                e => format!("：{e}"),
            };
            Err(format!(
                "读不到 {UNIT_MAIN} 的日志（journalctl 退出码 {}）{detail}",
                o.code
            ))
        }
        Err(e) => Err(format!("读不到 {UNIT_MAIN} 的日志（{e}）")),
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
    fn status_one_shot_has_no_option_block() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["status"]), &mut ctx).unwrap();
        assert!(ctx.out.contains("alice-hy2-direct"), "{}", ctx.out);
        assert!(
            !ctx.out.contains("[1] 切换节点"),
            "一次性 status 不打菜单块：{}",
            ctx.out
        );
        assert!(!ctx.out.contains("[0] 退出"), "{}", ctx.out);
    }

    #[test]
    fn list_one_shot_has_no_back_row() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["list"]), &mut ctx).unwrap();
        assert!(ctx.out.contains("[1] alice-hy2-direct"), "{}", ctx.out);
        assert!(
            !ctx.out.contains("[0] 返回"),
            "一次性 list 没有可返回的地方：{}",
            ctx.out
        );
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

    /// 合成的 HY2 直连账号：host/port/hop/sni 都同 [`hy2_direct_node`]，只有凭据不同。
    fn hy2_account(username: &str, password: &str) -> bui_schema::nodes::Node {
        bui_schema::nodes::Node {
            transport: bui_schema::nodes::Transport::Hysteria2 {
                username: username.into(),
                password: password.into(),
                sni: "panel.example.com".into(),
                obfs_password: None,
            },
            ..hy2_direct_node()
        }
    }

    fn hy2_credentials(p: &crate::profiles::Profile) -> (&str, &str) {
        match &p.node.transport {
            bui_schema::nodes::Transport::Hysteria2 {
                username, password, ..
            } => (username, password),
            other => panic!("不是 HY2 节点：{other:?}"),
        }
    }

    /// `bui-c import --panel https://panel.example.com --user <user>`，返回这次会话说过的话。
    fn import_from_panel(
        s: &FakeSys,
        pp: &Paths,
        user: &str,
        nodes: Vec<bui_schema::nodes::Node>,
    ) -> String {
        let n = FakeNet::new();
        n.route(
            &crate::source::nodes_url("https://panel.example.com", user),
            nodes_payload(user, nodes),
        );
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(s, &n, pp, &mut p, false, false);
        dispatch(
            &parse(&[
                "import",
                "--panel",
                "https://panel.example.com",
                "--user",
                user,
            ]),
            &mut ctx,
        )
        .unwrap();
        ctx.transcript.clone()
    }

    /// 缺陷：面板用户名全是中文，`profile_name` 回落成 `<主机名>-<kind>`，同一台服务器上
    /// 第二个账号（家人）的节点跟第一个同名，`upsert` 直接 Replace——第一个账号能用的
    /// 凭据被悄悄换掉。
    #[test]
    fn importing_a_second_account_on_the_same_host_does_not_overwrite_the_first() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        import_from_panel(&s, &pp, "示例用户甲", vec![hy2_account("u1", "pw1")]);
        let t = import_from_panel(&s, &pp, "示例用户", vec![hy2_account("u2", "pw2")]);
        assert_eq!(
            names(&s, &pp),
            vec![
                "panel.example.com-hy2-direct",
                "panel.example.com-hy2-direct-2"
            ],
            "{t}"
        );
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(
            hy2_credentials(&saved.profiles[0]),
            ("u1", "pw1"),
            "第一个账号的凭据不能被第二个账号覆盖：\n{t}"
        );
        assert_eq!(hy2_credentials(&saved.profiles[1]), ("u2", "pw2"));
        assert!(
            t.contains(
                "节点名 panel.example.com-hy2-direct 已被另一个账号占用，新节点命名为 panel.example.com-hy2-direct-2"
            ),
            "{t}"
        );
        assert_eq!(
            saved.active.as_deref(),
            Some("panel.example.com-hy2-direct"),
            "已有活动节点，不抢"
        );
    }

    /// 缺陷：import-v3 导进来的节点 label 是 v3 备注，面板给的是 `HY2直连`，整个 `Node`
    /// 不相等就被当成新节点——同一个连接在列表里出现两份。
    #[test]
    fn reimporting_a_v3_node_from_the_panel_updates_it_in_place() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        let mut prof = Profiles::new_default();
        prof.upsert(crate::profiles::Profile {
            name: "hysteria2-1785892136".into(),
            node: bui_schema::nodes::Node {
                label: "示例专用名-小组".into(),
                ..hy2_account("u1", "pw1")
            },
            split: crate::profiles::default_split(),
            source: crate::profiles::Source::V3,
            imported_at: "2026-09-11T00:00:00Z".into(),
        });
        prof.active = Some("hysteria2-1785892136".into());
        prof.save(&s, &pp).unwrap();

        let t = import_from_panel(&s, &pp, "示例专用名", vec![hy2_account("u1", "pw1")]);
        assert_eq!(names(&s, &pp), vec!["hysteria2-1785892136"], "{t}");
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(saved.profiles[0].node.label, "HY2直连", "{t}");
        assert_eq!(saved.profiles[0].source, crate::profiles::Source::ApiNodes);
        assert!(t.contains("更新节点 hysteria2-1785892136"), "{t}");
    }

    /// 凭据轮换：同名、同一账号（username 相同）换了密码 → 原地替换，不另起 `-2`。
    #[test]
    fn password_rotation_for_the_same_account_replaces_in_place() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        import_from_panel(&s, &pp, "示例用户甲", vec![hy2_account("u1", "pw1")]);
        let t = import_from_panel(&s, &pp, "示例用户甲", vec![hy2_account("u1", "pw-rotated")]);
        assert_eq!(names(&s, &pp), vec!["panel.example.com-hy2-direct"], "{t}");
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(hy2_credentials(&saved.profiles[0]), ("u1", "pw-rotated"));
        assert!(t.contains("更新节点 panel.example.com-hy2-direct"), "{t}");
        assert!(!t.contains("占用"), "同一账号不算占用：{t}");
    }

    /// 同一批里两个账号定出同一个名字（粘贴 / 订阅可能混入多个用户）：upsert 逐个做，
    /// 第二个看到的「已有同名」就是第一个，按 same_account 判定另起 `-2`。
    #[test]
    fn two_accounts_in_one_pasted_batch_under_the_same_name_are_both_kept() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        let n = FakeNet::new();
        let mut p = Scripted::from([
            "hysteria2://u1:pw1@panel.example.com:10000/?sni=panel.example.com&mport=20000-30000#u1-HY2",
            "hysteria2://u2:pw2@panel.example.com:10000/?sni=panel.example.com&mport=20000-30000#u2-HY2",
            "",
        ]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["import", "-"]), &mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert_eq!(
            names(&s, &pp),
            vec![
                "panel.example.com-hy2-direct",
                "panel.example.com-hy2-direct-2"
            ],
            "{t}"
        );
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(hy2_credentials(&saved.profiles[0]), ("u1", "pw1"));
        assert_eq!(hy2_credentials(&saved.profiles[1]), ("u2", "pw2"));
        assert!(t.contains("已被另一个账号占用"), "{t}");
        assert!(t.contains("导入 2 个新节点，共 2 个"), "{t}");
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
    fn check_lists_every_failure_in_chinese_when_restarting_and_when_waiting() {
        // 真机：`systemctl stop bui-c` 后巡检日志里打的是 `- UnitDown` / `- TunMissing`
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        s.reply("systemctl is-active --quiet bui-c.service", 3, "");
        let mut prof = profiles_socks();
        prof.auto_update = false; // 只看巡检结论，不去碰更新源
        prof.save(&s, &pp).unwrap();
        let n = FakeNet::new();
        n.route(crate::check::PROBE_URL, FakeReply::Status(502));
        let mut p = Scripted::from([]);

        // 第一次：重启
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["check"]), &mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert!(t.contains("已重启 bui-c.service"), "{t}");
        let items: Vec<&str> = t.lines().filter(|l| l.starts_with("  - ")).collect();
        assert_eq!(
            items,
            vec![
                "  - bui-c.service 没在运行",
                "  - 探测 www.gstatic.com/generate_204 返回 HTTP 502"
            ],
            "{t}"
        );

        // 紧接着再巡检（TUN 模式、探测连不上）：退避中，也要逐条列出
        let mut prof = crate::testutil::profiles_tun();
        prof.auto_update = false;
        prof.save(&s, &pp).unwrap();
        let n = FakeNet::new();
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["check"]), &mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert!(t.contains("退避中"), "{t}");
        let items: Vec<&str> = t.lines().filter(|l| l.starts_with("  - ")).collect();
        assert_eq!(
            items,
            vec![
                "  - bui-c.service 没在运行",
                "  - 探测 www.gstatic.com/generate_204 超时或连不上",
                "  - bui-tun 接口不存在",
                "  - 默认路由没有指向 bui-tun",
            ],
            "{t}"
        );
        for debug in ["UnitDown", "Probe", "TunMissing", "TunNoDefaultRoute"] {
            assert!(!t.contains(debug), "不打 Rust Debug 名 {debug}：{t}");
        }
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
        assert_eq!(saved.active.as_deref(), Some("hysteria2-1"));
        assert!(s.called("systemctl stop hysteria-client.service"));
        assert!(s.exists(std::path::Path::new("/opt/bui-c/config.json")));
        assert!(ctx.out.contains("导入 1 个节点"));
    }

    /// 菜单 [7] 在已迁移的机器上被再按一次：v3 目录按约定保留着，`detect` 恒为真。
    #[test]
    fn import_v3_second_run_reports_already_imported_and_does_not_apply() {
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
        let after_first = Profiles::load(&s, &pp).unwrap();
        let before = s.calls().len();

        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["import-v3"]), &mut ctx).unwrap();
        assert!(ctx.out.contains("已导入过"), "{}", ctx.out);
        let second: Vec<String> = s.calls().into_iter().skip(before).collect();
        for c in &second {
            assert!(
                !c.starts_with("systemctl restart") && !c.starts_with("systemctl enable"),
                "无事可做就不该走 apply_with_ufw：{second:?}"
            );
        }
        let after_second = Profiles::load(&s, &pp).unwrap();
        assert_eq!(
            after_second.profiles.len(),
            after_first.profiles.len(),
            "不该冒出重复节点"
        );
        assert_eq!(after_second, after_first, "profiles.json 一个字段都不该动");
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
        assert!(n2.log().is_empty(), "渲染菜单不该联网");
        // ★ 只在菜单选项块里（一次性 status 不打菜单块），同样只读 runtime.json
        let prof2 = Profiles::load(&s, &pp).unwrap();
        assert!(menu::render_options(&engine_status(&ctx, &prof2)).contains("★ 有新版"));

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
        let prof3 = Profiles::load(&s, &pp).unwrap();
        assert!(!menu::render_options(&engine_status(&ctx, &prof3)).contains("★ 有新版"));
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
        assert_eq!(saved.active.as_deref(), Some("hysteria2-1"));
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
    fn say_indents_every_non_empty_line() {
        let pp = paths();
        let s = FakeSys::new();
        let n = FakeNet::new();
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        assert_eq!(ctx.indent, "", "默认顶格：一次性命令不缩进");
        ctx.say("顶格");
        ctx.indent = "  ";
        ctx.say("一\n\n二");
        assert_eq!(ctx.out, "顶格\n  一\n\n  二\n", "空行不留尾随空格");
        assert_eq!(ctx.transcript, ctx.out, "transcript 也带缩进");
    }

    #[test]
    fn menu_loop_indents_result_lines_like_the_menu() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        s.reply("ip link show bui-tun", 0, "5: bui-tun");
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        // x = 无效选项 → 2 切到 TUN → y 确认 → 0 退出
        let mut p = Scripted::from(["x", "2", "y", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert!(
            t.lines().any(|l| l == "  无效选项：x"),
            "结果行跟菜单一样缩进两列：\n{t}"
        );
        assert!(t.lines().any(|l| l == "  已切到 TUN 模式"), "{t}");
        // 菜单本身已经排好版，不能再叠一层缩进
        assert!(t.lines().any(|l| l.starts_with("  ─────  B-UI")), "{t}");
        assert!(t.lines().any(|l| l.starts_with("     [1] ")), "{t}");
        assert!(!t.lines().any(|l| l.starts_with("    ─────  B-UI")), "{t}");
        assert_eq!(ctx.indent, "", "退出菜单后恢复顶格");
    }

    #[test]
    fn menu_loop_restores_indent_even_when_it_errors() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        s.put("/opt/bui-c/profiles.json", "{ 不是 json");
        let n = FakeNet::new();
        let mut p = Scripted::from(["0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        assert!(menu_loop(&mut ctx).is_err());
        assert_eq!(
            ctx.indent, "",
            "run() 接着打的「错误：…」要顶格，不能沿用菜单缩进"
        );
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
    fn menu_loop_redraws_on_blank_line_and_only_eof_or_zero_quits() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        s.reply("ip link show bui-tun", 0, "5: bui-tun");
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        // 回车 → 重画（不退出）→ 2 切到 TUN → y → 队列空 = EOF 退出
        let mut p = Scripted::from(["", "2", "y"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        assert_eq!(
            Profiles::load(&s, &pp).unwrap().mode,
            Mode::Tun,
            "空行只是重画，后面的 2 照样生效\n{}",
            ctx.transcript
        );
        let t = ctx.transcript.clone();
        assert_eq!(t.matches("B-UI 客户端").count(), 3, "三屏菜单：\n{t}");
        assert!(!t.contains("无效选项"), "空行不打任何字：\n{t}");

        // 0 仍然退出：后面的 2 不该被读到
        let mut p = Scripted::from(["", "0", "2", "y"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        assert_eq!(Profiles::load(&s, &pp).unwrap().mode, Mode::Tun);
        assert_eq!(ctx.transcript.matches("B-UI 客户端").count(), 2);
    }

    fn two_nodes(s: &FakeSys, pp: &Paths) {
        let mut prof = profiles_socks();
        prof.upsert(crate::profiles::Profile {
            name: "alice-reality-direct".into(),
            node: reality_direct_node(),
            split: split_keywords(),
            source: crate::profiles::Source::ApiNodes,
            imported_at: "2026-09-11T00:00:00Z".into(),
        });
        prof.save(s, pp).unwrap();
    }

    #[test]
    fn menu_node_pick_blank_or_zero_returns_silently_and_junk_is_reported() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        two_nodes(&s, &pp);
        let n = FakeNet::new();
        let mut p = Scripted::from([
            "1", "", // 空行：静默返回
            "1", "０", // 全角 0 也是返回
            "1", "9", // 越界
            "1", "abc", // 不是数字
            "0",
        ]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert_eq!(
            t.lines().filter(|l| l.contains("无效")).collect::<Vec<_>>(),
            vec![
                "  无效编号：9（可选 1-2，0 返回）",
                "  无效编号：abc（可选 1-2，0 返回）"
            ],
            "{t}"
        );
        assert_eq!(
            Profiles::load(&s, &pp).unwrap().active.as_deref(),
            Some("alice-hy2-direct")
        );
        assert_eq!(t.matches("B-UI 客户端").count(), 5, "每次都回到主菜单：{t}");
    }

    #[test]
    fn menu_node_pick_with_no_profiles_does_not_ask_for_a_number() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s); // 没有 profiles.json，也没有 v3 目录
        let n = FakeNet::new();
        let mut p = Scripted::from(["1", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert!(t.contains("没有节点，先导入（主菜单 3）"), "{t}");
        assert!(
            !p.asked.iter().any(|q| q == "选择节点编号"),
            "空列表没有编号可选（不能出现「可选 1-0」）：{:?}",
            p.asked
        );
        assert_eq!(t.matches("B-UI 客户端").count(), 2, "0 被主菜单读走：{t}");
    }

    #[test]
    fn menu_toggle_mode_declined_says_so() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        let mut p = Scripted::from(["2", "n", "2", "", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert_eq!(
            t.lines().filter(|l| *l == "  已取消，模式未变").count(),
            2,
            "答 n 与直接回车都要有回应：{t}"
        );
        assert_eq!(Profiles::load(&s, &pp).unwrap().mode, Mode::Socks);
    }

    #[test]
    fn menu_import_v3_without_a_v3_client_is_not_a_failure() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        let mut p = Scripted::from(["7", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert!(
            t.lines()
                .any(|l| l
                    == "  这台机器上没有 v3 客户端（/opt/hysteria-client 不存在），不需要导入"),
            "{t}"
        );
        assert!(!t.contains("失败"), "{t}");

        // 命令行仍然报错：脚本靠退出码判断
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        assert!(dispatch(&parse(&["import-v3"]), &mut ctx).is_err());
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

    const BOB_REALITY: &str = "vless://11111111-1111-4111-8111-111111111111@panel.example.com:10001?encryption=none&security=reality&sni=www.bing.com&fp=chrome&pbk=PUB&sid=0123456789abcdef&flow=xtls-rprx-vision&type=tcp#bob-Reality%E7%9B%B4%E8%BF%9E";

    fn nodes_payload(user: &str, nodes: Vec<bui_schema::nodes::Node>) -> FakeReply {
        FakeReply::Text(
            serde_json::to_string(&crate::source::NodesPayload {
                user: user.into(),
                split: split_keywords(),
                nodes,
            })
            .unwrap(),
        )
    }

    fn b64(text: &str) -> FakeReply {
        FakeReply::Text(base64::engine::general_purpose::STANDARD.encode(text))
    }

    fn names(s: &FakeSys, pp: &Paths) -> Vec<String> {
        Profiles::load(s, pp)
            .unwrap()
            .profiles
            .iter()
            .map(|p| p.name.clone())
            .collect()
    }

    #[test]
    fn menu_import_with_nothing_pasted_is_a_cancel_not_a_failure() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        // 3 = 导入节点 → 直接空行 → 0 退出
        let mut p = Scripted::from(["3", "", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert!(t.lines().any(|l| l == "  已取消，没有导入任何节点"), "{t}");
        assert!(!t.contains("失败"), "什么都没粘贴不是失败：{t}");
        assert!(
            p.asked.contains(
                &"粘贴 hysteria2:// 或 vless:// 节点链接，或面板给的订阅地址".to_string()
            ),
            "提示要说清楚两种都能贴：{:?}",
            p.asked
        );
    }

    #[test]
    fn menu_import_panel_url_goes_through_api_nodes_and_offers_the_switch() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        profiles_socks().save(&s, &pp).unwrap(); // 已有活动节点 alice-hy2-direct
        let n = FakeNet::new();
        n.route(
            "https://panel.example.com:8443/api/nodes/alice",
            nodes_payload("alice", vec![reality_direct_node(), hy2_direct_node()]),
        );
        // 带尾斜杠与 query 的 /api/subscription 地址也认成面板 → y 切到新节点 → 0 退出
        let mut p = Scripted::from([
            "3",
            "https://panel.example.com:8443/api/subscription/alice/?format=json",
            "",
            "y",
            "0",
        ]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert!(
            n.log().contains(
                &"GET https://panel.example.com:8443/api/nodes/alice via Direct".to_string()
            ),
            "{:?}\n{t}",
            n.log()
        );
        assert!(
            !n.log().iter().any(|l| l.contains("/api/subscription/")),
            "面板导入不去取 sing-box 配置：{:?}",
            n.log()
        );
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(
            saved
                .panel
                .as_ref()
                .map(|x| (x.base_url.as_str(), x.username.as_str())),
            Some(("https://panel.example.com:8443", "alice"))
        );
        assert_eq!(saved.profiles[0].source, crate::profiles::Source::ApiNodes);
        assert_eq!(
            saved.active.as_deref(),
            Some("alice-reality-direct"),
            "答 y 切到第一个新节点：\n{t}"
        );
        assert!(s.called("systemctl restart bui-c.service"), "切换要生效");
        assert!(!t.contains("失败"), "{t}");
        assert!(
            p.asked
                .contains(&"切换到新导入的 alice-reality-direct？".to_string()),
            "{:?}",
            p.asked
        );
    }

    #[test]
    fn menu_import_percent_decodes_the_username_and_n_keeps_the_active_node() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        n.route(
            "https://panel.example.com/api/nodes/%E5%BC%A0%E4%B8%89",
            nodes_payload("张三", vec![reality_direct_node()]),
        );
        let mut p = Scripted::from([
            "3",
            "https://panel.example.com/api/clash/%E5%BC%A0%E4%B8%89",
            "",
            "n",
            "0",
        ]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(
            saved.panel.as_ref().map(|x| x.username.as_str()),
            Some("张三"),
            "落盘的是解码后的用户名：\n{t}"
        );
        assert_eq!(saved.profiles.len(), 2, "{t}");
        assert_eq!(
            saved.active.as_deref(),
            Some("alice-hy2-direct"),
            "答 n 不切"
        );
        assert!(!s.called("systemctl restart bui-c.service"));
    }

    #[test]
    fn menu_import_api_sub_url_falls_back_to_the_subscription_when_the_panel_has_no_api_nodes() {
        // bwg-tizi 还是 v3 面板：没有 /api/nodes，但 /api/sub 在
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        n.route(
            "https://panel.example.com/api/nodes/alice",
            FakeReply::Status(404),
        );
        n.route("https://panel.example.com/api/sub/alice", b64(BOB_REALITY));
        let mut p = Scripted::from(["3", "https://panel.example.com/api/sub/alice", "", "n", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        let line = t
            .lines()
            .find(|l| l.contains("面板接口取不到"))
            .unwrap_or_else(|| panic!("要说明为什么改走订阅：\n{t}"));
        assert!(line.starts_with("  面板接口取不到（"), "{line}");
        assert!(line.ends_with("），改用订阅地址导入"), "{line}");
        assert!(line.contains("HTTP 404"), "带上原因：{line}");
        assert!(!line.contains("alice"), "原因里不带用户名：{line}");
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(saved.profiles.len(), 2, "{t}");
        assert_eq!(
            saved.profiles[1].source,
            crate::profiles::Source::Subscription
        );
        assert!(
            !t.lines().any(|l| l.starts_with("  失败：")),
            "退回订阅成功就不算失败（原因里的「请求…失败」除外）：{t}"
        );
    }

    #[test]
    fn menu_import_only_api_sub_falls_back() {
        // /api/clash 返回 YAML，没法当订阅解析：面板取不到就直接报失败
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        let mut p = Scripted::from(["3", "https://panel.example.com/api/clash/alice", "", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert!(t.lines().any(|l| l.starts_with("  失败：")), "{t}");
        assert!(!t.contains("改用订阅地址导入"), "{t}");
        assert_eq!(n.log().len(), 1, "只试了面板接口：{:?}", n.log());
    }

    #[test]
    fn menu_import_other_http_url_is_imported_as_a_subscription() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        n.route("https://sub.example.com/link/abc?token=1", b64(BOB_REALITY));
        let mut p = Scripted::from([
            "3",
            "https://sub.example.com/link/abc?token=1",
            "",
            "n",
            "0",
        ]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert_eq!(
            n.log(),
            vec!["GET https://sub.example.com/link/abc?token=1 via Direct".to_string()],
            "{t}"
        );
        assert_eq!(names(&s, &pp).len(), 2, "{t}");
    }

    #[test]
    fn menu_import_pasted_uris_offer_to_switch_to_the_first_new_node() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        let mut p = Scripted::from(["3", BOB_REALITY, "", "y", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(saved.profiles.len(), 2, "{t}");
        assert_eq!(
            saved.active.as_deref(),
            Some(saved.profiles[1].name.as_str()),
            "{t}"
        );
        assert!(n.log().is_empty(), "粘贴的节点链接不联网");
    }

    #[test]
    fn menu_import_first_nodes_are_activated_without_asking() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s); // 没有 profiles.json，也没有 v3 目录
        let n = FakeNet::new();
        // 没有追问：粘贴 → 空行 → 0 直接退出；要是追问了，0 会被当成「否」吞掉，
        // 队列耗尽后菜单照样退出，所以另外断言 0 是被主菜单读走的（屏数）
        let mut p = Scripted::from(["3", BOB_REALITY, "", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(saved.profiles.len(), 1, "{t}");
        assert_eq!(
            saved.active.as_deref(),
            Some(saved.profiles[0].name.as_str()),
            "首次导入自动激活：\n{t}"
        );
        assert!(!t.contains("失败"), "{t}");
        assert!(
            !p.asked.iter().any(|q| q.starts_with("切换到新导入的")),
            "新节点已经是活动节点，不必追问：{:?}",
            p.asked
        );
    }

    #[test]
    fn cli_import_never_asks_to_switch() {
        // 命令行 `bui-c import` 保持非交互：脚本里跑它不能卡在一个 [y/N] 上
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        let mut p = Scripted::from([BOB_REALITY, ""]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["import", "-"]), &mut ctx).unwrap();
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(saved.profiles.len(), 2);
        assert_eq!(saved.active.as_deref(), Some("alice-hy2-direct"));
        assert_eq!(p.asked, vec!["粘贴节点链接".to_string()], "只有粘贴那一问");
    }

    /// 单元文件已经在（引擎装好了）的机器。
    fn with_unit(s: &FakeSys) {
        s.put("/etc/systemd/system/bui-c.service", "[Unit]");
    }

    const JOURNAL_50: &str = "journalctl -u bui-c.service -n 50 --no-pager --output cat";
    const JOURNAL_10: &str = "journalctl -u bui-c.service -n 10 --no-pager --output cat";

    #[test]
    fn menu_service_control_is_a_numbered_submenu() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        with_unit(&s);
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        // 4 = 服务控制 → 0 返回（不重启）→ 0 退出
        let mut p = Scripted::from(["4", "0", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        for row in [
            "  [1] 重启 bui-c.service",
            "  [2] 最近 50 行日志",
            "  [0] 返回",
        ] {
            assert!(t.lines().any(|l| l == row), "缺 {row:?}：\n{t}");
        }
        assert!(
            !s.called("systemctl restart bui-c.service"),
            "进子菜单不等于重启"
        );
        assert_eq!(
            t.matches("B-UI 客户端").count(),
            2,
            "返回后重画主菜单：\n{t}"
        );
    }

    #[test]
    fn menu_service_submenu_blank_returns_and_junk_is_reported() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        with_unit(&s);
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        let mut p = Scripted::from(["4", "", "4", "x", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert_eq!(
            t.lines()
                .filter(|l| l.contains("无效选项"))
                .collect::<Vec<_>>(),
            vec!["  无效选项：x"],
            "空行静默返回，x 报一次：\n{t}"
        );
        assert!(!s.called("systemctl restart bui-c.service"));
        assert_eq!(t.matches("B-UI 客户端").count(), 3, "{t}");
    }

    #[test]
    fn menu_service_restart_in_socks_mode_does_not_wait_for_tun() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        with_unit(&s);
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        let mut p = Scripted::from(["4", "1", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        assert!(s.called("systemctl restart bui-c.service"));
        let t = ctx.transcript.clone();
        assert!(t.lines().any(|l| l == "  已重启 bui-c.service"), "{t}");
        assert!(
            t.contains("[2] 最近 50 行日志"),
            "先出子菜单，1 是子菜单里的「重启」：\n{t}"
        );
        assert!(
            !t.contains("[1] alice-hy2-direct"),
            "1 不能漏到主菜单去切节点：\n{t}"
        );
        assert!(s.sleeps().is_empty(), "SOCKS 模式没有接口可等");
    }

    #[test]
    fn menu_service_restart_in_tun_mode_reports_the_interface() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        with_unit(&s);
        s.reply("ip link show bui-tun", 0, "5: bui-tun");
        crate::testutil::profiles_tun().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        let mut p = Scripted::from(["4", "1", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        assert!(s.called("systemctl restart bui-c.service"));
        let t = ctx.transcript.clone();
        assert!(
            t.lines()
                .any(|l| l == "  已重启 bui-c.service，bui-tun 已就绪"),
            "{t}"
        );
        assert!(
            !s.sleeps().is_empty(),
            "要等接口起来，不能 restart 完就报好"
        );
        assert!(!s.called(JOURNAL_10), "就绪了不必翻日志");
    }

    #[test]
    fn menu_service_restart_in_tun_mode_shows_logs_when_the_interface_never_comes_up() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        with_unit(&s);
        s.reply("ip link show bui-tun", 1, "");
        s.reply(
            JOURNAL_10,
            0,
            "\u{1b}[31mFATAL\u{1b}[0m[0000] start service: open tun: operation not permitted\n",
        );
        crate::testutil::profiles_tun().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        let mut p = Scripted::from(["4", "1", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert!(
            t.lines()
                .any(|l| l == "  已重启 bui-c.service，但 bui-tun 接口 5 秒内没起来，最近日志："),
            "{t}"
        );
        assert!(
            t.lines()
                .any(|l| l == "FATAL[0000] start service: open tun: operation not permitted"),
            "日志原样打出、去掉颜色：\n{t}"
        );
        assert!(!t.contains('\u{1b}'), "菜单约定无 ANSI");
        assert_eq!(
            s.sleeps().len(),
            crate::engine::APPLY_POLL_STEPS as usize,
            "与 apply 同一段轮询：10 × 500ms"
        );
    }

    #[test]
    fn menu_service_logs_are_printed_without_ansi() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        with_unit(&s);
        s.reply(
            JOURNAL_50,
            0,
            "\u{1b}[36mINFO\u{1b}[0m[0000] sing-box started (0.12s)\n\u{1b}[33mWARN\u{1b}[0m[0001] inbound/mixed[mixed-in]: 127.0.0.1:1080\n",
        );
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        let mut p = Scripted::from(["4", "2", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        assert!(s.called(JOURNAL_50), "{:?}", s.calls());
        assert!(!s.called("systemctl restart bui-c.service"));
        let t = ctx.transcript.clone();
        assert!(
            t.lines()
                .any(|l| l == "INFO[0000] sing-box started (0.12s)"),
            "{t}"
        );
        assert!(
            t.lines()
                .any(|l| l == "WARN[0001] inbound/mixed[mixed-in]: 127.0.0.1:1080"),
            "{t}"
        );
        assert!(!t.contains('\u{1b}'), "菜单约定无 ANSI：{t:?}");
    }

    #[test]
    fn menu_service_logs_explain_when_journalctl_fails() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        with_unit(&s);
        s.reply(JOURNAL_50, 1, "");
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        let mut p = Scripted::from(["4", "2", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert!(
            t.lines()
                .any(|l| l.starts_with("  读不到 bui-c.service 的日志")),
            "{t}"
        );
        assert!(!t.contains("失败："), "不当成菜单操作失败：{t}");
    }

    #[test]
    fn menu_service_control_without_units_points_to_install() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s); // ready 只登记 systemctl 回答，不建单元文件
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        let mut p = Scripted::from(["4", "0"]); // 4 = 服务控制 → 0 退出
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        assert!(
            ctx.transcript.contains("bui-c update"),
            "单元不存在时应引导先装引擎：{}",
            ctx.transcript
        );
        assert!(
            !ctx.transcript.contains("最近 50 行日志"),
            "没有单元就不进子菜单：{}",
            ctx.transcript
        );
        assert!(!s.called("systemctl restart bui-c.service"));
    }
}
