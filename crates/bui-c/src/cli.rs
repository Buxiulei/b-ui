//! clap 子命令、`Ctx`（可注入的一次会话）、菜单循环与 `run()`。
//!
//! 所有会改机器的子命令都经 `apply_with_ufw`：装内核 → 按模式同步 UFW → `Engine::apply`。
//! root 检查只在 [`run`] 里做，`dispatch` 保持纯注入，单元测试直接调它。

use crate::check::{self, Runtime, Verdict};
use crate::delete::{self, PlanKind};
use crate::engine::{Applied, Engine};
use crate::lock::{self, LockGuard};
use crate::menu::{self, Action, Prompt, Status};
use crate::net::Net;
use crate::paths::{Paths, UNIT_MAIN, UNIT_TIMER};
use crate::profiles::{
    https_base, kind_slug, profile_name, rfc3339, same_account, Mode, Panel, Profile, Profiles,
    Source, Upsert,
};
use crate::source::{self, Fetched};
use crate::sys::{systemd, Sys};
use crate::{import_v3, ufw, uninstall, update, Error, Result};
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use std::time::Duration;

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
    /// 删除节点（可以给多个名字）；删到当前节点时用 --switch-to 指定换到哪个
    Delete {
        /// 节点名，至少一个（`bui-c list` 看名字）。重复的去重
        #[arg(required = true, num_args = 1..)]
        names: Vec<String>,
        /// 删到当前节点时换到这个节点；不给就按 delete::default_to 挑
        #[arg(long)]
        switch_to: Option<String>,
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
        /// 面板地址，须 https（取 manifest 与内核；默认用 v3 记录的 server_address，也可用环境变量 BUI_C_PANEL）
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
    /// stdout 已经被关掉（`bui-c list | head -1`）：之后的输出直接丢，菜单循环就此收场。
    pub stdout_closed: bool,
    /// [`clear_screen`](Self::clear_screen) 真正清过几次屏。测试靠它断言「只在交互终端里清、
    /// 清几次」：清屏序列不进 `transcript`，没别的地方看得出来。
    pub clears: usize,
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
            stdout_closed: false,
            clears: 0,
        }
    }
    /// 终端列数；拿不到按 80。不缓存，每次画屏前重新取（窗口缩放、手机转屏立刻生效）。
    pub fn width(&self) -> usize {
        self.sys
            .term_size()
            .map(|(cols, _)| cols)
            .filter(|&c| c > 0)
            .map_or(80, usize::from)
    }
    /// 终端行数；拿不到按 24。报 0 行（只设了列数的 pty）也算拿不到。
    /// 算一屏还能放几行时一律写 `rows().saturating_sub(2)`（留给提示符与最后一行），
    /// 不指望这里的回落值防下溢。
    pub fn rows(&self) -> usize {
        self.sys
            .term_size()
            .map(|(_, rows)| rows)
            .filter(|&r| r > 0)
            .map_or(24, usize::from)
    }
    /// 能不能清屏重画：stdin 有人在看（[`Prompt::interactive`]），且 stdout 是终端。
    /// 管道、重定向、timer 下都为假，清屏序列不会混进输出。
    pub fn screen_ctl(&self) -> bool {
        self.prompt.interactive() && self.sys.term_size().is_some()
    }
    /// 清屏序列：光标回左上角、清可见区（`ESC[H ESC[2J`）。**不发 `ESC[3J`**：只清可见区、
    /// 保留回滚，与 v3 的 `clear` 一致；`ESC[3J` 在 tmux 与手机客户端上行为不一，还会把
    /// 操作记录抹掉（spec §4.1）。
    pub const CLEAR_SCREEN: &'static str = "\x1b[H\x1b[2J";
    /// 清屏：只在 [`screen_ctl`](Self::screen_ctl) 为真（stdin、stdout 都是终端）时清，
    /// 管道、重定向、测试里的 `FakeSys`（默认拿不到终端尺寸）一律不清。
    ///
    /// 先把待打印缓冲冲出去，再把序列**直接**写到 stdout——不经过 `emit`，所以 `out` 与
    /// `transcript` 永远不含 ESC。测试构建里写进 `sink`，只计数。
    pub fn clear_screen(&mut self) {
        #[cfg(not(test))]
        let mut w = std::io::stdout();
        #[cfg(test)]
        let mut w = std::io::sink();
        self.clear_into(&mut w);
    }
    /// [`clear_screen`](Self::clear_screen) 的本体；测试传缓冲进来逐字节核对序列。
    fn clear_into<W: std::io::Write>(&mut self, w: &mut W) {
        if !self.screen_ctl() {
            return;
        }
        self.flush();
        if self.stdout_closed {
            return;
        }
        if menu::write_out(w, Self::CLEAR_SCREEN).is_err() {
            self.stdout_closed = true;
            return;
        }
        self.clears += 1;
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
    /// 打印并清空待打印缓冲。
    pub fn flush(&mut self) {
        #[cfg(not(test))]
        let mut w = std::io::stdout().lock();
        #[cfg(test)]
        let mut w = CapturedStdout;
        self.flush_into(&mut w);
    }
    /// 把待打印缓冲写进 `w` 并清空。写失败（被关掉的管道是 `BrokenPipe`，终端挂断是
    /// `EIO`）静默丢弃并记下 [`stdout_closed`](Self::stdout_closed)，之后不再尝试——
    /// 以前的 `print!` 在这里 panic，`bui-c list | head -1` 以退出码 101 收场。
    pub fn flush_into<W: std::io::Write>(&mut self, w: &mut W) {
        if self.out.is_empty() {
            return;
        }
        if !self.stdout_closed && menu::write_out(w, &self.out).is_err() {
            self.stdout_closed = true;
        }
        self.out.clear();
    }
}

/// 单元测试里 [`Ctx::flush`] 仍经 `print!`：直接写 `stdout().lock()` 会绕过 libtest 的
/// 输出捕获，`cargo test` 会满屏菜单。
#[cfg(test)]
struct CapturedStdout;

#[cfg(test)]
impl std::io::Write for CapturedStdout {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        print!("{}", String::from_utf8_lossy(buf));
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// 收尾：先把正常输出冲到 stdout，错误单独写 `err`（真机是 stderr）——
/// `bui-c list --json | jq` 这类管道里 stdout 只该有结果。
fn finish<S: Sys, N: Net, P: Prompt, W: std::io::Write>(
    ctx: &mut Ctx<'_, S, N, P>,
    r: Result<()>,
    err: &mut W,
) -> std::process::ExitCode {
    ctx.flush();
    match r {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            let _ = writeln!(err, "错误：{e}");
            // 用法错误 2、执行失败 1（spec §0.2 R15）
            std::process::ExitCode::from(e.exit_code())
        }
    }
}

fn engine_status<S: Sys, N: Net, P: Prompt>(ctx: &Ctx<'_, S, N, P>, prof: &Profiles) -> Status {
    let p = prof.active_profile();
    Status {
        node: p.map(|x| x.name.clone()).unwrap_or_default(),
        label: p.map(|x| x.node.label.clone()).unwrap_or_default(),
        kind: p
            .map(|x| kind_slug(x.node.kind).to_string())
            .unwrap_or_default(),
        host_port: p
            .map(|x| format!("{}:{}", x.node.host, x.node.port))
            .unwrap_or_default(),
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
/// 口径与 [`update::self_build_differs`] 相同：版本不同，或同版本但不是同一份构建（rc 通道重建）。
/// 刚刚自替换成 manifest 那一份时要算「没有新版」：本进程的 `VERSION` 还是旧二进制的，
/// 不看 `self_updated` 的话菜单会一直挂着 ★，直到下次检查。
fn new_version_pending(r: &update::Report) -> bool {
    r.self_outdated && !r.self_updated
}

/// `BUI_C_PANEL=<url>`：本次进程里把面板地址换成它（只在内存里，不落盘），给预发布期
/// 「v3 推导的面板没有 `/packages`」这类情况一个显式出口。经 `Sys::env` 读（决策 11，
/// 测试可注入）。只换 `base_url`，`username` 照旧——它是订阅路径，跟 manifest 来源无关。
///
/// 只认 https（[`https_base`]）：这里换掉的正是 root 自更新的来源，明文地址忽略并记一条警告。
fn with_panel_override<S: Sys>(sys: &S, prof: &Profiles) -> Profiles {
    let Some(raw) = sys.env("BUI_C_PANEL").filter(|u| !u.trim().is_empty()) else {
        return prof.clone();
    };
    let Some(url) = https_base(&raw) else {
        tracing::warn!(
            panel = %crate::error::redact_url(raw.trim()),
            "BUI_C_PANEL 不是 https 地址，已忽略"
        );
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

/// apply 之后 bui-tun 接口没起来时打的那一行。菜单切节点靠它认出「切了，但 TUN 没通」
/// （[`switch_node`]），所以打印与识别共用这一个常量。
const TUN_NOT_READY: &str =
    "警告：bui-c.service 已启动但 bui-tun 接口未就绪，查 `journalctl -u bui-c`";

/// 唯一的「改机器」路径：装内核 → 同步 UFW → apply。持锁才能调用：`LockGuard` 由顶层入口
/// 拿（[`take_lock`]），这里只是收下凭证，自己绝不拿锁（spec §0.2 R11）。
fn apply_with_ufw<S: Sys, N: Net, P: Prompt>(
    ctx: &mut Ctx<'_, S, N, P>,
    prof: &Profiles,
    _: &LockGuard,
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
        ctx.say(TUN_NOT_READY);
    }
    Ok(applied)
}

struct Stored {
    added: usize,
    names: Vec<String>,
    /// upsert 结果为 [`Upsert::Replaced`] 的 profile 名：原地更新了活动节点就得 apply
    replaced: Vec<String>,
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
        replaced: Vec::new(),
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
            Upsert::Replaced => {
                ctx.say(format!("更新节点 {name}"));
                out.replaced.push(name.clone());
            }
            Upsert::Unchanged => ctx.say(format!("节点 {name} 无变化")),
        }
        out.names.push(name);
    }
    // panel 是 root 自更新的来源：换掉它必须让人看见。同一个面板只跟着改用户名，不出声
    if let Some(p) = panel {
        match prof.panel.as_mut() {
            Some(cur) if cur.base_url == p.base_url => cur.username = p.username,
            _ => {
                ctx.say(format!("自动更新来源改为 {}", p.base_url));
                prof.panel = Some(p);
            }
        }
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

/// 面板 `/api/nodes/<user>`：唯一带服务端分流规则的来源，也是唯一会记下 panel 的来源。
///
/// panel 是 root 每日自更新的 manifest 与二进制首选来源（sha256 出自同一份 manifest，
/// 没有签名），所以要两样都成立才记：`/api/nodes` 返回了合法载荷（证明是 v4 面板），
/// 且地址是 https。`http://` 面板照样导节点，只是不当自更新来源。
fn fetch_panel<S: Sys, N: Net, P: Prompt>(
    ctx: &mut Ctx<'_, S, N, P>,
    base: &str,
    user: &str,
) -> Result<Incoming> {
    let fetched = source::from_panel(ctx.net, base, user)?;
    let panel = match https_base(base) {
        Some(base_url) => Some(Panel {
            base_url,
            username: user.to_string(),
        }),
        None => {
            ctx.say("面板地址不是 https，不作为自动更新来源");
            None
        }
    };
    Ok(Incoming {
        fetched,
        src: Source::ApiNodes,
        panel,
    })
}

/// 订阅地址（base64 URI 列表）。不记 panel：订阅主机可能是任意第三方（机场、转换服务），
/// 能返回节点列表证明不了它是 v4 面板，不能让它成为 root 自更新来源。
fn fetch_sub<N: Net>(net: &N, url: &str) -> Result<Incoming> {
    Ok(Incoming {
        fetched: source::from_subscription(net, url)?,
        src: Source::Subscription,
        panel: None,
    })
}

/// 落盘一批节点；首次导入或 `activate` 时激活第一个并 apply，原地更新了活动节点也 apply。
fn save_import<S: Sys, N: Net, P: Prompt>(
    ctx: &mut Ctx<'_, S, N, P>,
    inc: Incoming,
    activate: bool,
) -> Result<()> {
    let mut prof = Profiles::load(ctx.sys, ctx.paths)?;
    let loaded = prof.clone();
    let had_active = prof.active_profile().is_some();
    let stored = store_fetched(ctx, &mut prof, &inc.fetched, inc.src, inc.panel);
    if activate || !had_active {
        if let Some(name) = stored.names.first() {
            prof.active = Some(name.clone());
        }
    }
    // 什么都没变（重复导入同一个面板）就不重写 profiles.json
    if prof != loaded {
        prof.save(ctx.sys, ctx.paths)?;
    }
    ctx.say(format!(
        "导入 {} 个新节点，共 {} 个",
        stored.added,
        prof.profiles.len()
    ));
    if activate || !had_active {
        let g = take_lock(ctx)?;
        apply_with_ufw(ctx, &prof, &g)?;
        ctx.say(format!(
            "{CURRENT_NODE_HEAD}{}",
            prof.active.clone().unwrap_or_default()
        ));
    } else if prof
        .active
        .as_ref()
        .is_some_and(|a| stored.replaced.contains(a))
    {
        // 活动节点被原地更新（凭据轮换、端口变了）：不 apply 的话还跑着旧配置。
        // 只改了 label 时渲出的配置字节不变，engine 不会重启
        let g = take_lock(ctx)?;
        apply_with_ufw(ctx, &prof, &g)?;
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
                ctx.out.push('\n');
            } else {
                // 终端里跟着宽度走，管道里（拿不到尺寸）按 80 列排
                let text = menu::render_status(&st, ctx.width());
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
                ctx.out.push('\n');
            } else {
                let text = menu::render_nodes(&prof, false, ctx.width());
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
                // 兜底 apply 一次：配置丢了、单元没建时这是唯一不绕路的补救；配置没变就不重启
                let g = take_lock(ctx)?;
                apply_with_ufw(ctx, &prof, &g)?;
                ctx.say(format!("已是当前节点：{name}"));
                return Ok(());
            }
            prof.active = Some(name.clone());
            prof.save(ctx.sys, ctx.paths)?;
            let g = take_lock(ctx)?;
            apply_with_ufw(ctx, &prof, &g)?;
            ctx.say(format!("已切到 {name}"));
            Ok(())
        }
        Cmd::Mode { mode } => {
            let mut prof = Profiles::load(ctx.sys, ctx.paths)?;
            let want: Mode = (*mode).into();
            prof.mode = want;
            prof.save(ctx.sys, ctx.paths)?;
            let label = match want {
                Mode::Tun => "TUN",
                Mode::Socks => "SOCKS",
            };
            // 还没有节点：只记下选择。apply 会先去下内核、改 UFW，最后才因为没有节点失败
            if prof.active_profile().is_none() {
                ctx.say(format!("已记下 {label} 模式，导入节点后生效"));
                return Ok(());
            }
            let g = take_lock(ctx)?;
            apply_with_ufw(ctx, &prof, &g)?;
            ctx.say(format!("已切到 {label} 模式"));
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
                fetch_panel(ctx, base, u)?
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
        // `bui-c delete <名字>... [--switch-to <名字>] [-y] [--json]`（spec §5.10、§0.2 R7 / R15）
        Cmd::Delete { names, switch_to } => {
            // 用法错误先判（退出码 2）：与机器状态无关，也不该让人白等一次读盘
            if ctx.json && !ctx.yes {
                return Err(Error::usage("--json 要和 -y 一起用"));
            }
            let mut want: Vec<String> = Vec::new();
            for n in names {
                if !want.contains(n) {
                    want.push(n.clone());
                }
            }
            // 不是终端就直接报错，绝不从管道里读确认（spec §5.10，D10）
            if !ctx.yes && !ctx.prompt.interactive() {
                let list = want
                    .iter()
                    .map(|n| menu::sanitize(n).into_owned())
                    .collect::<Vec<_>>()
                    .join("、");
                return Err(Error::usage(format!(
                    "要删除 {} 个节点：{list}；不是在终端里运行，请加 -y 确认",
                    want.len()
                )));
            }
            let prof = Profiles::load(ctx.sys, ctx.paths)?;
            // 名字找不到、--switch-to 无效：什么都不删（退出码 1）
            let plan = delete::plan(&prof, &want, switch_to.as_deref())?;
            // 校验过了、但没有作用对象：不报用法错误（R15 只认三种退出码 2），说一句就好
            if switch_to.is_some() && matches!(plan.kind, PlanKind::Passive) {
                ctx.say(delete::SWITCH_TO_UNUSED);
            }
            let seen = delete::snapshot(&prof);
            if !ctx.yes {
                let picks: Vec<usize> = plan
                    .targets
                    .iter()
                    .filter_map(|n| prof.profiles.iter().position(|p| &p.name == n))
                    .collect();
                let to = match &plan.kind {
                    PlanKind::Switch { to } => prof.profiles.iter().position(|p| &p.name == to),
                    _ => None,
                };
                // 命令行版不列可换节点（换目标用 --switch-to），其余与菜单逐字相同
                let c = menu::delete_confirm_cli(&prof, &picks, to, ctx.width());
                debug_assert_eq!(
                    c.needs_word,
                    !matches!(plan.kind, PlanKind::Passive),
                    "确认块与 plan 判形态的算法必须一致"
                );
                ctx.show(c.body.trim_end());
                ctx.flush();
                let ans = ctx.prompt.line(&c.question)?;
                // 只认 y / yes：编号在这里没有意义，Pick 一律算取消。在终端里答否是退出码 0
                match menu::parse_confirm(&ans, prof.profiles.len(), c.needs_word) {
                    menu::ConfirmInput::Yes => {}
                    menu::ConfirmInput::NeedWord => {
                        ctx.say(delete::NEEDS_YES);
                        ctx.say(delete::CANCELLED);
                        return Ok(());
                    }
                    // 从菜单养成的习惯是打编号：照 §5.10 算取消，但说清命令行换目标的办法
                    menu::ConfirmInput::Pick(_) => {
                        ctx.say(delete::CLI_NO_NUMBER);
                        ctx.say(delete::CANCELLED);
                        return Ok(());
                    }
                    _ => {
                        ctx.say(delete::CANCELLED);
                        return Ok(());
                    }
                }
            }
            let r = match delete_nodes(ctx, &want, switch_to.as_deref(), &seen) {
                Ok(r) => r,
                Err(e) => {
                    // 失败也不许往 stdout 漏字（`apply_with_ufw` 那几句 say 不看 ctx.json）：
                    // 文案走 stderr + 退出码 1（spec §5.10）
                    if ctx.json {
                        ctx.out.clear();
                    }
                    return Err(e);
                }
            };
            if ctx.json {
                // stdout 只有一个对象加换行（spec §5.10）
                ctx.out =
                    serde_json::to_string(&r).map_err(|e| Error::parse("delete", e.to_string()))?;
                ctx.out.push('\n');
            } else {
                // 命令行按 §5.4 说全：没有「上次：」行，R6 的短式不适用
                ctx.say(delete::cli_summary(&r));
            }
            Ok(())
        }
        Cmd::Check => run_check(ctx, false),
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
                ctx.say(if !r.self_outdated {
                    "已是最新"
                } else if r.manifest_version == crate::VERSION {
                    "有同版本的新构建（rc 通道重建），跑 `bui-c update` 升级"
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
                // 不串名字：真机 9 个节点 join 起来一行两百多列，名字 `bui-c list` 里都看得到
                ctx.say(format!(
                    "v3 的 {} 个节点都已导入过，{tail}",
                    r.existing.len()
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
            let g = take_lock(ctx)?;
            apply_with_ufw(ctx, &prof, &g)?;
            ctx.say(format!(
                "{CURRENT_NODE_HEAD}{}",
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

/// 连接检查：`bui-c check`（timer，`manual = false`）与菜单 `[5]`（`manual = true`）共用。
///
/// 手动检查不受退避约束（[`check::run_manual`]），结果行只说「已重启」，不提「下次退避」——
/// 那是 timer 的节奏，对刚点了 [5] 的人没有意义。
fn run_check<S: Sys, N: Net, P: Prompt>(ctx: &mut Ctx<'_, S, N, P>, manual: bool) -> Result<()> {
    let v = if manual {
        check::run_manual(ctx.sys, ctx.net, ctx.paths)?
    } else {
        check::run(ctx.sys, ctx.net, ctx.paths)?
    };
    match &v {
        Verdict::NoProfile => ctx.say("没有激活的节点，巡检跳过"),
        Verdict::Ok => ctx.say("正常：单元在跑、204 探测通过"),
        Verdict::Restarted {
            failures,
            next_backoff_min,
        } => {
            if manual {
                ctx.say(format!(
                    "发现 {} 项异常，已重启 bui-c.service",
                    failures.len()
                ));
            } else {
                ctx.say(format!(
                    "发现 {} 项异常，已重启 bui-c.service，下次退避 {next_backoff_min} 分钟",
                    failures.len()
                ));
            }
            for f in failures {
                ctx.say(format!("  - {f}"));
            }
            // is-active 在 exec 之后立刻为真，TUN 接口还没起来；timer 巡检也一样等
            // （最多 5 秒，只在刚重启过时发生）
            if Profiles::load(ctx.sys, ctx.paths)?.mode == Mode::Tun {
                if Engine::new(ctx.sys, ctx.paths).wait_tun_ready() {
                    ctx.say("bui-tun 已就绪");
                } else {
                    ctx.say(format!(
                        "bui-tun 接口 {} 秒内没起来，查 [4] 服务控制 → 最近日志",
                        crate::engine::TUN_READY_WAIT_S
                    ));
                }
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

/// 决策 10 / spec §6「import-v3：首次运行从 /opt/hysteria-client/ 导入」的接线点：
/// 没有任何 profile、机器上有 v3 客户端目录、且 v3 主单元文件至少还剩一个（还没迁移）时，
/// 进菜单前问一次。已迁移的机器按约定留着 v3 目录当回滚素材，单元文件已卸掉，删光节点后
/// 不再邀请（spec §5.7）。只问一次；答否就给出手动入口。`check` / `update` 这类非交互路径
/// **不**走这里——让 timer 悄悄改用户配置是更坏的行为。
///
/// 问了就返回 `Some(Outcome)`，由 [`menu_body`] 照主循环的规矩收尾：进循环的第一件事就是
/// 清屏，导入失败的原因不停下来就被抹掉了，而这正是迁移那一刻。导入经 [`run_sub`]（`yes: false`）。
fn offer_v3_import<S: Sys, N: Net, P: Prompt>(
    ctx: &mut Ctx<'_, S, N, P>,
) -> Result<Option<Outcome>> {
    let prof = Profiles::load(ctx.sys, ctx.paths)?;
    if !prof.profiles.is_empty() {
        return Ok(None);
    }
    let base = PathBuf::from(import_v3::V3_BASE);
    // 先看 v3 目录：新装机没有它，就不必每次进菜单再去 stat 三个单元文件
    if !import_v3::detect(ctx.sys, &base) {
        return Ok(None);
    }
    let unmigrated = import_v3::V3_UNITS
        .iter()
        .any(|u| ctx.sys.exists(&ctx.paths.unit(u)));
    if !unmigrated {
        return Ok(None);
    }
    ctx.say(format!("发现 v3 客户端目录 {}", base.display()));
    ctx.flush();
    let out = if ctx.prompt.confirm("现在导入 v3 的节点并卸载旧单元吗？")? {
        run_sub(
            ctx,
            Cmd::ImportV3 {
                base: None,
                panel: None,
                mode: None,
            },
        )
    } else {
        note(
            ctx,
            "已跳过。随时可以跑 `bui-c import-v3`，或在菜单里选 [7] 从 v3 导入",
        )
    };
    Ok(Some(out))
}

/// 菜单里一个动作做完之后怎么回主菜单（spec §4.1–§4.3）：停不停、「上次：」行写什么。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// 结果只有一行：直接清屏回主菜单，这一行进「上次：」行。
    Note(String),
    /// 失败，或打了不止一行：先停下来等回车（看完再清屏），摘要进「上次：」行。
    Pause(String),
    /// 返回、空输入：「上次：」行不变。
    Nothing,
    /// 退出菜单。
    Exit,
}

impl Outcome {
    /// 只改摘要，停不停不变。
    fn map_summary(self, f: impl FnOnce(String) -> String) -> Self {
        match self {
            Self::Note(s) => Self::Note(f(s)),
            Self::Pause(s) => Self::Pause(f(s)),
            other => other,
        }
    }
}

/// 首次导入、从 v3 导入之后打的「当前节点：X」。回主菜单后状态区本来就显示当前节点，
/// 它不算附加行，不因为它停（审查裁定；[`outcome_since`] 数行时跳过它）。
const CURRENT_NODE_HEAD: &str = "当前节点：";

/// 从 transcript 的 `start` 起新打的内容定 [`Outcome`]，照 spec §4.3 的一条标准：失败，或者
/// 这个动作自己打了不止一行 → 停；否则不停。有「失败：」行 → 停，摘要就是它；多于一行 → 停，
/// 摘要取第一行；只有一行 → 不停，它进「上次：」行；什么都没打 → 「上次：」行不变。空行与
/// [`CURRENT_NODE_HEAD`] 行不算。
fn outcome_since<S: Sys, N: Net, P: Prompt>(ctx: &Ctx<'_, S, N, P>, start: usize) -> Outcome {
    // transcript 只追加不截断，`start` 一定落在字符边界上
    let new: Vec<&str> = ctx.transcript[start..]
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with(CURRENT_NODE_HEAD))
        .collect();
    if let Some(fail) = new.iter().find(|l| l.starts_with("失败：")) {
        return Outcome::Pause(fail.to_string());
    }
    match new.as_slice() {
        [] => Outcome::Nothing,
        [one] => Outcome::Note(one.to_string()),
        [first, ..] => Outcome::Pause(first.to_string()),
    }
}

/// 菜单里转手给 [`dispatch`] 的动作（spec §4.2）：记下 transcript 当前长度再执行，按这次新打的
/// 内容定 [`Outcome`]，现有命令一行都不用改。出错照常先打「失败：…」。
///
/// 子命令一律 `yes: false`（D11）。真正起作用的是 [`menu_loop`] 进门就把 `ctx.yes` 换成假：
/// `dispatch` 读的是 `ctx.yes`，不是子 `Cli` 的 `yes`（spec §0.2 R2、R11）。
fn run_sub<S: Sys, N: Net, P: Prompt>(ctx: &mut Ctx<'_, S, N, P>, cmd: Cmd) -> Outcome {
    let start = ctx.transcript.len();
    let sub = Cli {
        json: false,
        yes: false,
        cmd: Some(cmd),
    };
    if let Err(e) = dispatch(&sub, ctx) {
        ctx.say(format!("失败：{e}"));
    }
    outcome_since(ctx, start)
}

/// 菜单自己说的一行结果：照常打出来（管道里、transcript 里都在），同时进「上次：」行——
/// 交互终端里接着就清屏重画了。
fn note<S: Sys, N: Net, P: Prompt>(ctx: &mut Ctx<'_, S, N, P>, text: impl Into<String>) -> Outcome {
    let text = text.into();
    ctx.say(&text);
    Outcome::Note(text)
}

/// 菜单里切节点（[1] 选编号、[3] 导入后答 y）：经 [`run_sub`] 转手 `Cmd::Switch`，停不停照
/// [`outcome_since`]。摘要显式写切换的结果（spec §11.2），不取第一行：TUN 下第一次切换时第一行
/// 是「已为 bui-tun 接口放行 UFW…」。失败照旧用「失败：…」那一行。切过去了但 apply 报了
/// [`TUN_NOT_READY`]：摘要写「已切到 X，但 bui-tun 没起来」——「上次：」行是给回到主菜单的人
/// 记成败的（spec §4.2），不能把没通说成切好了。
///
/// 摘要里的节点名按当前宽度单独中间截断（spec §0.2 R6）：整行截尾会把 `…-reality-direct`
/// 与 `…-reality-resi` 截成一个样子。
fn switch_node<S: Sys, N: Net, P: Prompt>(ctx: &mut Ctx<'_, S, N, P>, name: String) -> Outcome {
    let width = ctx.width();
    let already = Profiles::load(ctx.sys, ctx.paths)
        .is_ok_and(|p| p.active.as_deref() == Some(name.as_str()));
    let start = ctx.transcript.len();
    let out = run_sub(ctx, Cmd::Switch { name: name.clone() });
    // transcript 只追加不截断，`start` 一定落在字符边界上
    let tun_down = ctx.transcript[start..]
        .lines()
        .any(|l| l.trim() == TUN_NOT_READY);
    let done = if already {
        format!("已是当前节点：{name}")
    } else if tun_down {
        format!("已切到 {name}，但 bui-tun 没起来")
    } else {
        format!("已切到 {name}")
    };
    out.map_summary(|s| {
        let s = if s.starts_with("失败：") { s } else { done };
        menu::fit_name_in_last(&s, &name, width)
    })
}

// ───────────── 删除节点（spec §5，菜单 [6] / `bui-c delete` / [9] 测速结果页共用） ─────────────

/// 拿锁最多等多久（spec §8.3）。菜单与命令行都用 `Wait`，timer 的巡检用 `Once`。
const LOCK_WAIT: Duration = Duration::from_secs(15);
/// 等不到锁：别的 bui-c 正在改东西（退出码 1，spec §0.2 R15）。
const LOCK_BUSY: &str = "另一个 bui-c 正在改配置，等了 15 秒还没轮到，稍后再试";

/// 顶层入口拿锁（spec §0.2 R11）。本任务是桩（[`lock::acquire`] 永远给得到），调用点与顺序
/// 现在就排好；T12a 换成真的 flock 之后这里会真的等，也会真的等不到。
fn take_lock<S: Sys, N: Net, P: Prompt>(ctx: &Ctx<'_, S, N, P>) -> Result<LockGuard> {
    lock::acquire(ctx.sys, ctx.paths, lock::How::Wait(LOCK_WAIT))?
        .ok_or_else(|| Error::msg(LOCK_BUSY))
}

/// 删除流程里说一句：按当前宽度折行、缩进两列（`SelError::message` 那种 59 列的句子在 40 列
/// 终端上要折，spec §0.2 R1）。`--json` 下一个字都不打——那时 stdout 只放一个 Report。
fn tell<S: Sys, N: Net, P: Prompt>(ctx: &mut Ctx<'_, S, N, P>, text: impl AsRef<str>) {
    if ctx.json {
        return;
    }
    let width = ctx.width();
    ctx.show(delete::page(&[text.as_ref().to_string()], width).trim_end());
}

/// 删除失败：把停顿页打出来（spec §5.5 的失败列、§3c-60-7，下一步照 R1），返回给「上次：」行
/// 用的短摘要。`head` 是第一行，`rest` 是原因、安抚与下一步。
fn delete_failed<S: Sys, N: Net, P: Prompt>(
    ctx: &mut Ctx<'_, S, N, P>,
    head: &str,
    rest: &[String],
    summary: String,
) -> Error {
    let width = ctx.width();
    let mut lines = vec![head.to_string()];
    lines.extend_from_slice(rest);
    if !ctx.json {
        let page = delete::page(&lines, width);
        ctx.show(page.trim_end());
    }
    Error::msg(summary)
}

/// Switch 失败之后的回滚（spec §0.2 R10）：一律 `Engine::apply(&old)`，**不**经 `ensure_kernel`
/// 与 UFW 前置——失败发生在写 `config.json` 之前时，这一次逐字节比对下来什么都不改、也不重启。
/// 返回失败页里跟在原因后面的那一两行。
fn roll_back<S: Sys, N: Net, P: Prompt>(ctx: &mut Ctx<'_, S, N, P>, old: &Profiles) -> Vec<String> {
    match Engine::new(ctx.sys, ctx.paths).apply(old) {
        // 旧配置写回去了、接口还是没起来：R10 对 apply 的判据（Err 或 `tun_ready == Some(false)`
        // 都算失败）在回滚方向一样算数。不能在断网状态下报「节点都还在」了事，出路也要给
        Ok(a) if a.tun_ready == Some(false) => vec![
            delete::ROLLED_BACK_TUN_DOWN.to_string(),
            delete::ROLLBACK_NEXT.to_string(),
        ],
        Ok(_) => vec![delete::ROLLED_BACK.to_string()],
        Err(e) => {
            tracing::warn!(error = %e, "删除失败后换回原配置也失败");
            vec![
                format!("{}：{e}", delete::ROLLBACK_FAILED),
                delete::ROLLBACK_NEXT.to_string(),
            ]
        }
    }
}

/// 删光节点时拆掉数据面（spec §0.2 R10）：只做 [`Engine::teardown_main`]——stop 后仍 active 就
/// 报错中止，调用方据此不写 `profiles.json`。撤 UFW、把 runtime 的重启记账清零由
/// [`release_after_teardown`] 在写完 `profiles.json` **之后**尽力而为（R10 把这两件排在最后）。
///
/// **顺序只有这一份**：`teardown_all` -> `save`（如需）-> `release_after_teardown`。T12c 的
/// `converge` 照同一顺序调这两个函数，别把收尾提到落盘前面去。
fn teardown_all<S: Sys, N: Net, P: Prompt>(
    ctx: &mut Ctx<'_, S, N, P>,
    _: &LockGuard,
) -> Result<()> {
    Engine::new(ctx.sys, ctx.paths).teardown_main()
}

/// 拆完数据面、`profiles.json` 也写好之后的收尾（spec §0.2 R10「撤 UFW、写 runtime 放在最后
/// 尽力而为」）：撤掉 bui-tun 的 UFW 放行、把重启记账清零。两件都失败也不算删除失败——所以
/// 不返回 `Result`：规则留着，等下次切 SOCKS 或收敛时再撤。
fn release_after_teardown<S: Sys, N: Net, P: Prompt>(ctx: &mut Ctx<'_, S, N, P>) {
    let mut rt = Runtime::load(ctx.sys, ctx.paths);
    if rt.ufw_rules {
        match ufw::revoke_tun(ctx.sys) {
            Ok(_) => rt.ufw_rules = false,
            Err(e) => {
                tracing::warn!(error = %e, "撤 bui-tun 的 UFW 放行失败");
                tell(ctx, delete::UFW_LEFT);
            }
        }
    }
    rt.fail_streak = 0;
    rt.last_restart_at = None;
    if let Err(e) = rt.save(ctx.sys, ctx.paths) {
        tracing::warn!(error = %e, "写 runtime.json 失败");
    }
}

/// 删除的执行编排（spec §5.5，顺序照 §0.2 R2 / R10 / R11）。三个入口共用：菜单 `[6]`、
/// `bui-c delete`、`[9]` 测速结果页的「删除不通的」。调用方负责先在屏幕上确认。
///
/// 1. **锁外**：Switch 形态且内核缺失时 `update::ensure_kernel`（唯一联网的一步）；
/// 2. 拿锁（最多等 15 秒）；
/// 3. 重读 `profiles.json`，与 `seen` 比快照，不一致就什么都不做；
/// 4. 锁内按名字重算 `plan`；
/// 5. Switch 形态做 `Engine::preflight`（清残留 → 渲染 → `sing-box check`，不装内核）；
/// 6. 数据面：Passive 什么都不做；Switch `apply_with_ufw(next)`，失败或 TUN 5 秒没就绪都回滚；
///    Empty `teardown_all`，失败就此中止；
/// 7. 写 `profiles.json`（`profiles` 与 `active` 同一次原子写）。Switch 写盘失败立刻回滚。
///
/// 任何失败都是「什么都没删」：停顿页已经打好，返回的 `Err` 是给「上次：」行的短摘要。
fn delete_nodes<S: Sys, N: Net, P: Prompt>(
    ctx: &mut Ctx<'_, S, N, P>,
    names: &[String],
    switch_to: Option<&str>,
    seen: &delete::Snapshot,
) -> Result<delete::Report> {
    // ① 锁外：只有「删到当前节点、要切过去」且内核缺失时才联网装内核（preflight 不装）
    let outside = Profiles::load(ctx.sys, ctx.paths)?;
    // 这一步只判「要不要先装内核」，成败一律交给锁内：别的会话刚好把要删的节点删掉时，
    // 裸 `?` 会先报「没有叫 X 的节点」，把并发说成输错名字，用户拿不到「请重新选」
    if matches!(
        delete::plan(&outside, names, switch_to).map(|p| p.kind),
        Ok(PlanKind::Switch { .. })
    ) && !ctx.sys.exists(&ctx.paths.singbox())
    {
        match update::ensure_kernel(
            ctx.sys,
            ctx.net,
            ctx.paths,
            &with_panel_override(ctx.sys, &outside),
        ) {
            Ok(true) => tell(ctx, "已安装 sing-box 内核"),
            Ok(false) => {}
            // 装不上不在这里报死：进锁后 preflight 会以「内核缺失」中止，文案与下一步统一在那里
            Err(e) => tracing::warn!(error = %e, "删除前装内核失败"),
        }
    }

    // ② 拿锁：从这里到放锁之间绝不提问（spec §8.3）
    let g = take_lock(ctx)?;

    // ③ 重读并比对快照：菜单上的编号对应的是屏幕上那一刻的列表
    let old = Profiles::load(ctx.sys, ctx.paths)?;
    if delete::snapshot(&old) != *seen {
        return Err(delete_failed(
            ctx,
            delete::SNAPSHOT_CHANGED,
            &[],
            // 「上次：」行只有 31 列（40 列终端）：长句会被砍掉「请重新选」（R6）
            delete::SNAPSHOT_CHANGED_SHORT.to_string(),
        ));
    }

    // ④ 锁内重算（纯函数，算出来必然一样，只是不拿锁外的结果去执行）
    let plan = delete::plan(&old, names, switch_to)?;
    let mut report = delete::Report {
        deleted: plan.targets.clone(),
        active: plan.next.active.clone(),
        switched: false,
        stopped: false,
        remaining: plan.next.profiles.len(),
    };
    match &plan.kind {
        // 不 apply、不重启：`config.json` 里只有活动节点，删别的对数据面没有影响
        PlanKind::Passive => {
            // 数据面没动过（§5.5 Passive「报错、什么都没变」），但也给停顿页：裸 `?` 只抛一句
            // 原始 IO 错误，还会从 `delete_menu` 冒到 `menu_action` 把人踢回 shell
            if let Err(e) = plan.next.save(ctx.sys, ctx.paths) {
                return Err(delete_failed(
                    ctx,
                    delete::SAVE_FAILED,
                    &[e.to_string(), delete::STILL_THERE.to_string()],
                    delete::SAVE_FAILED.to_string(),
                ));
            }
        }
        PlanKind::Switch { to } => {
            report.switched = true;
            // ⑤ 预检：动数据面之前挡住「内核缺失」「check 不通过」
            if let Err(e) = Engine::new(ctx.sys, ctx.paths).preflight(&plan.next) {
                let kernel = e.to_string().starts_with(crate::engine::KERNEL_MISSING);
                let head = if kernel {
                    format!("删除没做：内核缺失，切不到 {to}")
                } else {
                    format!("删除没做：切到 {to} 的配置校验不通过")
                };
                let next = if kernel {
                    delete::NEXT_KERNEL
                } else {
                    delete::NEXT_PICK_ANOTHER
                };
                let rest = [
                    e.to_string(),
                    delete::STILL_THERE.to_string(),
                    next.to_string(),
                ];
                return Err(delete_failed(
                    ctx,
                    &head,
                    &rest,
                    format!("删除没做：切到 {to} 失败"),
                ));
            }
            // ⑥ 数据面：TUN 5 秒没就绪也算失败（R10）
            let done = match apply_with_ufw(ctx, &plan.next, &g) {
                Ok(a) if a.tun_ready == Some(false) => Err(Error::msg(format!(
                    "bui-tun 接口 {} 秒内没起来",
                    crate::engine::TUN_READY_WAIT_S
                ))),
                Ok(_) => Ok(()),
                Err(e) => Err(e),
            };
            if let Err(e) = done {
                let mut rest = vec![e.to_string()];
                rest.extend(roll_back(ctx, &old));
                let head = format!("删除没做：切到 {to} 失败");
                return Err(delete_failed(ctx, &head, &rest, head.clone()));
            }
            // ⑦ 落盘：profiles 与 active 同一次原子写
            if let Err(e) = plan.next.save(ctx.sys, ctx.paths) {
                let mut rest = vec![e.to_string()];
                rest.extend(roll_back(ctx, &old));
                return Err(delete_failed(
                    ctx,
                    delete::SAVE_FAILED,
                    &rest,
                    delete::SAVE_FAILED.to_string(),
                ));
            }
        }
        PlanKind::Empty => {
            report.stopped = true;
            report.active = None;
            if let Err(e) = teardown_all(ctx, &g) {
                return Err(delete_failed(
                    ctx,
                    &format!("删除没做：{e}"),
                    &[delete::STILL_THERE.to_string()],
                    // 「上次：」行放不下完整原因（40 列只有 31 列），页上那一行才是全的
                    delete::TEARDOWN_FAILED_SHORT.to_string(),
                ));
            }
            // teardown 成功就走完了不可回头段：这里写盘失败是唯一一种「代理已经不在、节点
            // 条目还列着」的状态，不能沿用「节点都还在」，要说清现状与下一步（§5.5 Empty
            // 那一行要求 Pause 报出是哪一步失败）
            if let Err(e) = plan.next.save(ctx.sys, ctx.paths) {
                return Err(delete_failed(
                    ctx,
                    delete::STOPPED_NOT_SAVED_HEAD,
                    &[e.to_string(), delete::STOPPED_NOT_SAVED.to_string()],
                    delete::STOPPED_NOT_SAVED_SHORT.to_string(),
                ));
            }
            // 撤 UFW、写 runtime 排在写 profiles 之后，尽力而为（R10）
            release_after_teardown(ctx);
            // 被删节点的凭据可能留在写 profiles.json 的临时文件里（R10）
            if let Err(e) = Engine::new(ctx.sys, ctx.paths).clear_profile_leftovers() {
                tracing::warn!(error = %e, "清 profiles.json 的临时文件失败");
            }
        }
    }
    Ok(report)
}

/// 菜单 `[6] 删除节点`（spec §5.1–§5.3）：清屏 → 列表 + 写法 → 选编号（整行作废、原地重问）
/// → 确认块（认 y/yes、认编号改替换目标）→ 执行。确认之后才拿锁。
///
/// 本任务不接进主菜单（T9 接），测试直接调它。
#[allow(dead_code)]
fn delete_menu<S: Sys, N: Net, P: Prompt>(ctx: &mut Ctx<'_, S, N, P>) -> Result<Outcome> {
    let prof = Profiles::load(ctx.sys, ctx.paths)?;
    if prof.profiles.is_empty() {
        return Ok(note(ctx, delete::NO_NODES));
    }
    let len = prof.profiles.len();
    let width = ctx.width();
    ctx.clear_screen();
    // 过渡提示（`（检查更新挪到了 [7] 更新与维护）`）与「每个会话只显示一次」留给 T9
    ctx.show(menu::render_delete_picker(&prof, width, None).trim_end());

    // ① 选编号：空行、`0`、EOF 返回主菜单
    let picks = loop {
        ctx.flush();
        let line = ctx.prompt.line("删除哪几个")?;
        match menu::parse_selection(&line, len) {
            Ok(menu::Selection::Back) => return Ok(Outcome::Nothing),
            Ok(menu::Selection::Picks(v)) if !v.is_empty() => break v,
            Ok(menu::Selection::Picks(_)) => return Ok(Outcome::Nothing),
            // 文案本身 ≤ 59 列，40 列终端上要折行再打（spec §0.2 R1、第 7 条）
            Err(e) => tell(ctx, e.message(len)),
        }
    };
    let targets: Vec<String> = picks
        .iter()
        .map(|&i| prof.profiles[i].name.clone())
        .collect();
    let kind = delete::plan(&prof, &targets, None)?.kind;
    let switch_form = matches!(kind, PlanKind::Switch { .. });
    let mut to = match &kind {
        PlanKind::Switch { to } => prof.profiles.iter().position(|p| &p.name == to),
        _ => None,
    };

    // ② 确认：`rows` 传 ctx.rows() 的原值，delete_confirm 自己减 2（第 1 条）
    let c = menu::delete_confirm(&prof, &picks, to, width, ctx.rows());
    debug_assert_eq!(
        c.needs_word,
        !matches!(kind, PlanKind::Passive),
        "确认块与 plan 判形态的算法必须一致（第 2 条）"
    );
    ctx.show(c.body.trim_end());
    loop {
        ctx.flush();
        let ans = ctx.prompt.line(&c.question)?;
        match menu::parse_confirm(&ans, len, c.needs_word) {
            menu::ConfirmInput::Yes => break,
            // 会断网的删除只输了 y：按取消处理，并说清要输入什么（R1）
            menu::ConfirmInput::NeedWord => {
                tell(ctx, delete::NEEDS_YES);
                return Ok(Outcome::Pause(delete::CANCELLED.to_string()));
            }
            menu::ConfirmInput::Cancel => return Ok(note(ctx, delete::CANCELLED)),
            // 不在「删完切到」形态：这一步只认 y / yes（Empty 要输 yes，第 4 条）
            menu::ConfirmInput::Pick(_) if !switch_form => {
                tell(ctx, delete::only_y(c.needs_word));
                return Ok(Outcome::Pause(delete::CANCELLED.to_string()));
            }
            // 位数溢出时 parse_confirm 交回 Pick(len)，那不是用户打的数：回显原始输入（第 5 条）
            menu::ConfirmInput::Pick(i) if i >= len => {
                let e = menu::SelError::OutOfRange(vec![ans.trim().to_string()]);
                tell(ctx, e.message(len));
            }
            menu::ConfirmInput::Pick(i) if picks.contains(&i) => {
                tell(ctx, delete::also_a_target(i))
            }
            menu::ConfirmInput::Pick(i) => {
                to = Some(i);
                ctx.show(menu::render_switch_to(&prof, i, width).trim_end());
            }
        }
    }

    // ③ 执行
    let seen = delete::snapshot(&prof);
    let switch_to = to
        .filter(|_| switch_form)
        .map(|i| prof.profiles[i].name.clone());
    match &kind {
        PlanKind::Switch { .. } => tell(
            ctx,
            format!("正在切到 {}…", switch_to.clone().unwrap_or_default()),
        ),
        PlanKind::Empty => tell(ctx, "正在停止代理…"),
        PlanKind::Passive => {}
    }
    let start = ctx.transcript.len();
    match delete_nodes(ctx, &targets, switch_to.as_deref(), &seen) {
        Ok(r) => {
            let done = delete::summary(&r, width);
            ctx.say(&done);
            // 只打了这一行就直接回主菜单；装内核、UFW 这类附加行照规矩先停一下
            Ok(outcome_since(ctx, start).map_summary(|_| done))
        }
        Err(e) => {
            let summary = match switch_to.as_deref() {
                Some(name) => menu::fit_name_in_last(&e.to_string(), name, width),
                None => e.to_string(),
            };
            Ok(Outcome::Pause(summary))
        }
    }
}

pub fn menu_loop<S: Sys, N: Net, P: Prompt>(ctx: &mut Ctx<'_, S, N, P>) -> Result<()> {
    // 菜单里的结果行缩进两列跟菜单对齐；菜单里的确认只听键盘、不认全局 `-y`——`bui-c -y`
    // 进菜单按 8 以前会直接卸载（spec §0.2 R11）。任何出口（含 `?` 冒上来的错误）都恢复
    // 两者，否则调用方接着 say 的话会沿用菜单缩进
    let saved_indent = std::mem::replace(&mut ctx.indent, "  ");
    let saved_yes = std::mem::replace(&mut ctx.yes, false);
    let r = menu_body(ctx);
    ctx.indent = saved_indent;
    ctx.yes = saved_yes;
    r
}

/// 主循环（spec §4.1）：交互终端里每次重画前清屏，永远只有一屏主菜单；上一个动作的一行
/// 摘要显示在底部的「上次：」行。
fn menu_body<S: Sys, N: Net, P: Prompt>(ctx: &mut Ctx<'_, S, N, P>) -> Result<()> {
    // 只活在这一次菜单会话里，不落盘：明天另一个家人打开菜单，看到别人的「上次」只会困惑
    let mut last: Option<String> = None;
    // 进菜单前的 v3 导入邀请也照规矩收尾：下面第一件事就是清屏
    if let Some(out) = offer_v3_import(ctx)? {
        if settle(ctx, out, &mut last)? {
            return Ok(());
        }
    }
    let mut redraw = true;
    loop {
        let prof = Profiles::load(ctx.sys, ctx.paths)?;
        if redraw {
            ctx.clear_screen();
            let st = engine_status(ctx, &prof);
            // 每次重画都重新取宽度：窗口缩放、手机转屏立刻生效
            let screen = menu::render(&st, ctx.width(), last.as_deref());
            ctx.show(screen.trim_end());
        }
        ctx.flush();
        if ctx.stdout_closed {
            return Ok(()); // 没人看得到输出，不再接着读选择、执行命令
        }
        // EOF（Ctrl-D、stdin=/dev/null）→ 退出，不留在死循环里
        let Some(choice) = ctx.prompt.read("选择 [0-9]")? else {
            // Ctrl-D 不回显换行：先换一行，别让 shell 提示符接在「▸ 选择 [0-9]：」后面
            if ctx.prompt.interactive() {
                ctx.show("");
            }
            return Ok(());
        };
        // 直接回车 = 刷新，「上次：」行保留；不打「无效选项」——手滑多按一下回车不该挨说
        if choice.is_empty() {
            redraw = true;
            continue;
        }
        // 输错原地重问：只打一行错误，不清屏、不重画，「上次：」行也不动（spec §3a-60-输错）
        let Some(action) = menu::parse_choice(&choice) else {
            ctx.say(menu::invalid_choice(&choice, ctx.width()));
            redraw = false;
            continue;
        };
        let out = menu_action(ctx, &prof, action)?;
        if settle(ctx, out, &mut last)? {
            return Ok(());
        }
        redraw = true;
    }
}

/// 按 [`Outcome`] 收尾（spec §4.1）：`Pause` 先停下来等回车，`Note` / `Pause` 的摘要进
/// 「上次：」行，`Nothing` 不动它。返回 `true` 表示退出菜单。
fn settle<S: Sys, N: Net, P: Prompt>(
    ctx: &mut Ctx<'_, S, N, P>,
    out: Outcome,
    last: &mut Option<String>,
) -> Result<bool> {
    match out {
        Outcome::Note(s) => *last = Some(s),
        Outcome::Pause(s) => {
            ctx.flush();
            ctx.prompt.pause("回车返回菜单")?;
            *last = Some(s);
        }
        Outcome::Nothing => {}
        Outcome::Exit => return Ok(true),
    }
    Ok(false)
}

/// 主菜单的一个选择。做完返回 [`Outcome`]，由 [`menu_body`] 决定停不停、「上次：」行写什么。
/// 进子页（[1] 节点列表、[3] 粘贴、[4] 服务控制）前清屏，子页只显示自己（spec §4.1）；
/// 选完编号后的确认、进度与结果接在子页下面，不清。
fn menu_action<S: Sys, N: Net, P: Prompt>(
    ctx: &mut Ctx<'_, S, N, P>,
    prof: &Profiles,
    action: Action,
) -> Result<Outcome> {
    Ok(match action {
        Action::Quit => Outcome::Exit,
        Action::SwitchNode => {
            ctx.clear_screen();
            let list = menu::render_node_picker(prof, ctx.width());
            ctx.show(list.trim_end());
            if prof.profiles.is_empty() {
                // 列表里已经给了「先导入」的引导，没有编号可选。回主菜单要清屏，
                // 引导跟着进「上次：」行，不然一闪就没了
                Outcome::Note(menu::NO_NODES.to_string())
            } else {
                match pick_node(ctx, prof)? {
                    Some(name) => switch_node(ctx, name),
                    None => Outcome::Nothing,
                }
            }
        }
        Action::ToggleMode => {
            let (want, q) = match prof.mode {
                Mode::Socks => (ModeArg::Tun, "切换到 TUN 全局模式？"),
                Mode::Tun => (ModeArg::Socks, "切换到 SOCKS 模式？"),
            };
            if ctx.prompt.confirm(q)? {
                run_sub(ctx, Cmd::Mode { mode: want })
            } else {
                note(ctx, "已取消，模式未变")
            }
        }
        Action::ImportNode => {
            ctx.clear_screen();
            menu_import(ctx)?
        }
        Action::Service => {
            // 单元还没建就别进子菜单——systemd 只会回 `Unit bui-c.service not found`，
            // 用户看不出该干什么（缺陷 5）。引擎与单元是导入节点时 apply 装上的，
            // `bui-c update` 在新机器上只装内核、不建单元，不是出路。
            if ctx.sys.exists(&ctx.paths.unit(UNIT_MAIN)) {
                service_menu(ctx, prof.mode)?
            } else {
                let v3 = import_v3::detect(ctx.sys, std::path::Path::new(import_v3::V3_BASE));
                note(
                    ctx,
                    format!(
                        "还没有安装引擎与单元：先用 [3] 导入节点{}",
                        if v3 {
                            "（v3 客户端用 [7] 从 v3 导入）"
                        } else {
                            ""
                        }
                    ),
                )
            }
        }
        // 手动检查：不走 timer 的退避（`Cmd::Check` 是给 bui-c.timer 的）。这一版探测完才打字，
        // 进页就清屏会让人对着空屏干等；清屏留给边做边打的新报告（spec §6.4）
        Action::Check => {
            let start = ctx.transcript.len();
            if let Err(e) = run_check(ctx, true) {
                ctx.say(format!("失败：{e}"));
            }
            outcome_since(ctx, start)
        }
        Action::Update => run_sub(
            ctx,
            Cmd::Update {
                check_only: false,
                auto: None,
            },
        ),
        Action::AutoUpdate => run_sub(
            ctx,
            Cmd::Update {
                check_only: false,
                auto: Some(if prof.auto_update {
                    Switch::Off
                } else {
                    Switch::On
                }),
            },
        ),
        // 没有 v3 目录不是失败：菜单里如实说一句。命令行 `bui-c import-v3` 照旧报错，
        // 脚本要靠退出码
        Action::ImportV3
            if !import_v3::detect(ctx.sys, std::path::Path::new(import_v3::V3_BASE)) =>
        {
            note(
                ctx,
                format!(
                    "这台机器上没有 v3 客户端（{} 不存在），不需要导入",
                    import_v3::V3_BASE
                ),
            )
        }
        Action::ImportV3 => run_sub(
            ctx,
            Cmd::ImportV3 {
                base: None,
                panel: None,
                mode: None,
            },
        ),
        Action::Uninstall => run_sub(ctx, Cmd::Uninstall { purge_bin: false }),
    })
}

/// 菜单 `[1]` 列表下的选编号：输错只提示、原地重问（不重画列表）；空行、`0`、EOF 返回主菜单。
/// 选中返回节点名。
fn pick_node<S: Sys, N: Net, P: Prompt>(
    ctx: &mut Ctx<'_, S, N, P>,
    prof: &Profiles,
) -> Result<Option<String>> {
    let len = prof.profiles.len();
    loop {
        ctx.flush();
        let pick = ctx.prompt.line("选择节点编号")?;
        if menu::is_back(&pick) {
            return Ok(None);
        }
        if let Some(i) = menu::pick_index(&pick, len) {
            return Ok(Some(prof.profiles[i].name.clone()));
        }
        // 回显先净化再截到行宽：方向键是 `ESC [ A`，误贴的整条链接也只占一行
        ctx.say(menu::invalid_pick(&pick, len, ctx.width()));
    }
}

/// 菜单 `[3]` 的粘贴提示（`lines_until_blank` 自己补「每行一个，空行结束」）。
/// 要在 80 列里放下：支持哪些 scheme 留给失败行去说。
const PASTE_PROMPT: &str = "粘贴节点链接或订阅地址";

/// 菜单 `[3] 导入节点`：节点链接与面板 / 订阅地址都从这一个口子进。
///
/// 导入失败只打「失败：…」留在菜单里。导入了新节点、而活动节点不在其中时追问一次要不要
/// 切过去——命令行 `bui-c import` 不问，保持非交互。
///
/// 回主菜单停不停照 [`outcome_since`]：失败、有附加行（面板退回订阅、跳过…）就停。追问过
/// 「切换到新导入的 X？」的，人已经在提问处看过导入结果：答 y 用切换的结果，答否不再停。
fn menu_import<S: Sys, N: Net, P: Prompt>(ctx: &mut Ctx<'_, S, N, P>) -> Result<Outcome> {
    let start = ctx.transcript.len();
    let lines: Vec<String> = ctx
        .prompt
        .lines_until_blank(PASTE_PROMPT)?
        .into_iter()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() {
        return Ok(note(ctx, "已取消，没有导入任何节点"));
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
    match imported {
        Ok(()) => {}
        // 人就在菜单 [3] 里：不提命令行、不提「菜单 [3]」
        Err(Error::NoNodes {
            skipped,
            schemes,
            has_http,
        }) => {
            ctx.say(format!(
                "失败：{}",
                crate::error::no_nodes_summary(skipped, &schemes)
            ));
            if has_http {
                ctx.say("订阅地址请单独粘贴一行再回车");
            }
            return Ok(outcome_since(ctx, start));
        }
        Err(e) => {
            ctx.say(format!("失败：{e}"));
            return Ok(outcome_since(ctx, start));
        }
    }
    let after = Profiles::load(ctx.sys, ctx.paths)?;
    let fresh: Vec<&str> = after
        .profiles
        .iter()
        .map(|p| p.name.as_str())
        .filter(|n| !before.iter().any(|b| b == n))
        .collect();
    let Some(first) = fresh.first() else {
        return Ok(outcome_since(ctx, start));
    };
    if after.active.as_deref().is_some_and(|a| fresh.contains(&a)) {
        return Ok(outcome_since(ctx, start)); // 首次导入已经激活了新节点
    }
    ctx.flush();
    let shown = outcome_since(ctx, start);
    if ctx.prompt.confirm(&format!("切换到新导入的 {first}？"))? {
        return Ok(switch_node(ctx, first.to_string()));
    }
    // 提问本身就是停顿：导入结果已经在提问处看过了，答否回主菜单不再停
    Ok(match shown {
        Outcome::Pause(s) => Outcome::Note(s),
        other => other,
    })
}

/// 单独一行的 http(s) 地址：面板的四种按用户地址走 `/api/nodes`（带分流规则），
/// 其余当订阅地址。`/api/sub/` 在面板接口取不到时退回订阅——v3 面板（bwg-tizi）
/// 没有 `/api/nodes`，但 `/api/sub` 在。只在「取」失败时退回：面板节点取到了、
/// 后面 apply 失败时再按订阅导一遍，会把服务端分流规则换成默认表。
///
/// 面板的业务错误（`Error::Msg`，如「节点列表为空」）不退回：接口在、面板说这个用户没东西，
/// 按订阅再导一遍只会绕开这句话。
fn import_http<S: Sys, N: Net, P: Prompt>(ctx: &mut Ctx<'_, S, N, P>, url: &str) -> Result<()> {
    let inc = match source::panel_link(url) {
        Some(link) => match fetch_panel(ctx, &link.base_url, &link.user) {
            Ok(inc) => inc,
            Err(e) if link.path == source::PanelPath::Sub && !matches!(e, Error::Msg(_)) => {
                ctx.say(panel_fallback_notice(&e));
                ctx.flush();
                fetch_sub(ctx.net, url)?
            }
            Err(e) => return Err(e),
        },
        None => fetch_sub(ctx.net, url)?,
    };
    save_import(ctx, inc, false)
}

/// `/api/sub` 退回订阅时的那句话。401 / 404 是 v3 面板的常态（没有 `/api/nodes`），
/// 说成「还没有节点接口」，不报状态码；别的原因只留一句去掉 URL 的简短说明。
fn panel_fallback_notice(e: &Error) -> String {
    let reason = match e {
        Error::Net { detail, .. } if detail == "HTTP 401" || detail == "HTTP 404" => {
            return "这个面板还没有节点接口，改用订阅地址导入".to_string();
        }
        Error::Net { detail, .. } => without_urls(detail),
        Error::Parse { .. } => "返回的不是节点列表".to_string(),
        other => without_urls(&other.to_string()),
    };
    let reason = if reason.is_empty() {
        "网络错误".to_string()
    } else {
        reason
    };
    format!("面板接口取不到（{reason}），改用订阅地址导入")
}

/// 去掉错误文字里的 URL：reqwest 写成 `error sending request for url (https://…/api/nodes/<用户名>)`，
/// 路径末段的用户名等价凭据。先删 ` for url (…)`，再丢掉其余带 `://` 的词。
fn without_urls(detail: &str) -> String {
    let mut s = detail.to_string();
    while let Some(i) = s.find(" for url (") {
        let end = s[i..].find(')').map_or(s.len(), |j| i + j + 1);
        s.replace_range(i..end, "");
    }
    s.split_whitespace()
        .filter(|w| !w.contains("://"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `[4] 服务控制` 的二级菜单：重启 / 看日志 / 返回（v3 的服务子菜单大半是死代码，
/// 这里只留两个真实动作）。进页清屏；输错只提示、原地重问；空行、`0`、EOF 与做完一个动作
/// 都回主菜单。重启的结果照 [`outcome_since`] 定停不停（TUN 没起来会带出日志，要停）。
fn service_menu<S: Sys, N: Net, P: Prompt>(
    ctx: &mut Ctx<'_, S, N, P>,
    mode: Mode,
) -> Result<Outcome> {
    ctx.clear_screen();
    ctx.show(menu::render_service_options().trim_end());
    loop {
        ctx.flush();
        let pick = ctx.prompt.line("选择 [0-2]")?;
        match menu::parse_service_choice(&pick) {
            Some(menu::ServiceAction::Back) => return Ok(Outcome::Nothing),
            // 回显先净化再截到行宽：方向键是 `ESC [ A`，误贴的整条链接也只占一行
            None => ctx.say(menu::invalid_service_choice(&pick, ctx.width())),
            Some(menu::ServiceAction::Restart) => {
                let start = ctx.transcript.len();
                restart_service(ctx, mode);
                return Ok(outcome_since(ctx, start));
            }
            Some(menu::ServiceAction::Logs) => {
                show_journal(ctx, menu::SERVICE_LOG_LINES);
                // 50 行日志一屏装不下，回主菜单又要清屏：停一下，看完再回去。
                // 看日志不算做了什么，「上次：」行不变
                ctx.flush();
                ctx.prompt.pause("回车返回菜单")?;
                return Ok(Outcome::Nothing);
            }
        }
    }
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
        "已重启 bui-c.service，但 bui-tun 接口 {} 秒内没起来",
        crate::engine::TUN_READY_WAIT_S
    ));
    show_journal(ctx, 10);
}

/// 空一行、标题行，再把日志缩进 2 列打出来；取不到就说一句原因。
fn show_journal<S: Sys, N: Net, P: Prompt>(ctx: &mut Ctx<'_, S, N, P>, n: u32) {
    match journal_tail(ctx.sys, n) {
        Ok(text) => {
            let mut block = format!(
                "\n{}",
                menu::title_bar(&format!("{UNIT_MAIN} 最近 {n} 行日志"))
            );
            for l in text.lines() {
                block.push('\n');
                if !l.is_empty() {
                    block.push_str("  ");
                    block.push_str(l);
                }
            }
            ctx.show(block);
        }
        Err(why) => ctx.say(why),
    }
}

/// `journalctl -o short-iso` 取 bui-c.service 最近 `n` 行，每行压成 `<时间> <消息>` 并去掉
/// ANSI 颜色；取不到时 `Err` 是一句给用户看的说明。
fn journal_tail<S: Sys>(sys: &S, n: u32) -> std::result::Result<String, String> {
    let n = n.to_string();
    let args = ["-u", UNIT_MAIN, "-n", &n, "--no-pager", "-o", "short-iso"];
    match sys.run("journalctl", &args) {
        Ok(o) if o.ok() => {
            let text = o
                .stdout
                .trim_end()
                .lines()
                .map(|l| menu::strip_ansi(&menu::compact_journal_line(l)))
                .collect::<Vec<_>>()
                .join("\n");
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
    let r = dispatch(&cli, &mut ctx);
    finish(&mut ctx, r, &mut std::io::stderr())
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
            parse(&["delete", "HY2"]).cmd,
            Some(Cmd::Delete {
                names: vec!["HY2".into()],
                switch_to: None
            })
        );
        assert_eq!(
            parse(&[
                "delete",
                "HY2",
                "reality-Reality",
                "--switch-to",
                "alice-hy2-direct",
                "-y"
            ])
            .cmd,
            Some(Cmd::Delete {
                names: vec!["HY2".into(), "reality-Reality".into()],
                switch_to: Some("alice-hy2-direct".into())
            })
        );
        assert!(
            Cli::try_parse_from(["bui-c", "delete"]).is_err(),
            "至少要给一个名字"
        );
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
        assert!(ctx.out.starts_with("  ★ alice-hy2-direct\n"), "{}", ctx.out);
        assert!(
            !ctx.out.contains("[1]"),
            "`bui-c switch` 只认名字，一次性 list 不打编号：{}",
            ctx.out
        );
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
    fn mode_without_any_node_only_records_the_choice() {
        // 新机器还没导入节点就按 [2]：以前先下 81MB 内核、改 UFW，最后才报「没有激活的节点」
        let pp = paths();
        let s = FakeSys::new(); // 没有内核、没有单元、没有 profiles.json
        let n = FakeNet::new();
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["mode", "tun"]), &mut ctx).unwrap();
        assert_eq!(Profiles::load(&s, &pp).unwrap().mode, Mode::Tun);
        assert!(n.log().is_empty(), "不该去下内核：{:?}", n.log());
        assert!(
            !s.calls()
                .iter()
                .any(|c| c.starts_with("systemctl") || c.starts_with("ufw")),
            "没有节点就不动服务与防火墙：{:?}",
            s.calls()
        );
        assert!(
            ctx.transcript.contains("导入节点后生效"),
            "{}",
            ctx.transcript
        );
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

    /// 同一个 ASCII 用户名在两台服务器上各有一个账号（这一家同时跑着 bwg-tizi 与 bwg-rick）：
    /// `profile_name` 都是 `alice-hy2-direct`，但那是两台机器上的两个账号，第二台不能把第一台换掉。
    #[test]
    fn the_same_ascii_username_on_two_servers_keeps_both_nodes() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        import_from_panel(&s, &pp, "alice", vec![hy2_account("alice", "pw-a")]);
        let t = import_from_panel(
            &s,
            &pp,
            "alice",
            vec![bui_schema::nodes::Node {
                host: "other.example.com".into(),
                ..hy2_account("alice", "pw-b")
            }],
        );
        assert_eq!(
            names(&s, &pp),
            vec!["alice-hy2-direct", "alice-hy2-direct-2"],
            "{t}"
        );
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(saved.profiles[0].node.host, "panel.example.com");
        assert_eq!(hy2_credentials(&saved.profiles[0]), ("alice", "pw-a"));
        assert_eq!(saved.profiles[1].node.host, "other.example.com");
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

    /// 原地更新了活动节点（面板轮换了密码）：新凭据要立刻生效，不能等下次切换。
    /// 只改了 label 的也走一遍 apply，但配置字节没变，不重启。
    #[test]
    fn updating_the_active_node_in_place_applies_it() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        import_from_panel(&s, &pp, "alice", vec![hy2_account("alice", "pw1")]);
        assert!(s
            .get("/opt/bui-c/config.json")
            .is_some_and(|c| c.contains("pw1")));

        let before = s.calls().len();
        let t = import_from_panel(&s, &pp, "alice", vec![hy2_account("alice", "pw2")]);
        let calls: Vec<String> = s.calls().into_iter().skip(before).collect();
        assert!(t.contains("更新节点 alice-hy2-direct"), "{t}");
        assert!(
            s.get("/opt/bui-c/config.json")
                .is_some_and(|c| c.contains("pw2") && !c.contains("pw1")),
            "活动节点换了密码要重渲配置：\n{t}"
        );
        assert!(
            calls.iter().any(|c| c == "systemctl restart bui-c.service"),
            "配置变了要重启：{calls:?}\n{t}"
        );

        // 只改 label：apply 一遍，配置不变 → 不重启
        let relabelled = bui_schema::nodes::Node {
            label: "改过的备注".into(),
            ..hy2_account("alice", "pw2")
        };
        let before = s.calls().len();
        let t = import_from_panel(&s, &pp, "alice", vec![relabelled.clone()]);
        let calls: Vec<String> = s.calls().into_iter().skip(before).collect();
        assert!(t.contains("更新节点 alice-hy2-direct"), "{t}");
        assert!(
            calls.iter().any(|c| c.contains("sing-box check")),
            "要走 apply：{calls:?}"
        );
        assert!(
            !calls.iter().any(|c| c == "systemctl restart bui-c.service"),
            "只改 label 配置字节不变，不重启：{calls:?}"
        );

        // 更新的不是活动节点：不碰服务
        let before = s.calls().len();
        import_from_panel(
            &s,
            &pp,
            "alice",
            vec![relabelled.clone(), reality_direct_node()],
        );
        import_from_panel(
            &s,
            &pp,
            "alice",
            vec![
                relabelled,
                bui_schema::nodes::Node {
                    label: "另一个备注".into(),
                    ..reality_direct_node()
                },
            ],
        );
        let calls: Vec<String> = s.calls().into_iter().skip(before).collect();
        assert!(
            !calls.iter().any(|c| c.contains("sing-box check")),
            "非活动节点的更新不 apply：{calls:?}"
        );
    }

    /// `bui-c switch <当前节点>`：以前只说「已是当前节点」就返回，配置丢了、单元没建的机器
    /// 永远拉不回来。现在兜底 apply 一次（配置没变就不重启）。
    #[test]
    fn switching_to_the_current_node_still_applies_it() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        profiles_socks().save(&s, &pp).unwrap(); // 活动节点在，但 config.json 与单元都没有
        let n = FakeNet::new();
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["switch", "alice-hy2-direct"]), &mut ctx).unwrap();
        assert!(s.exists(std::path::Path::new("/opt/bui-c/config.json")));
        assert!(s.exists(std::path::Path::new("/etc/systemd/system/bui-c.service")));
        assert_eq!(ctx.out, "已是当前节点：alice-hy2-direct\n");

        // 再切一次：配置没变，不重启
        let before = s.calls().len();
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["switch", "alice-hy2-direct"]), &mut ctx).unwrap();
        let calls: Vec<String> = s.calls().into_iter().skip(before).collect();
        assert!(
            !calls.iter().any(|c| c == "systemctl restart bui-c.service"),
            "{calls:?}"
        );
        assert_eq!(ctx.out, "已是当前节点：alice-hy2-direct\n");
    }

    /// 换掉自动更新来源（= root 自更新的 manifest 与二进制来源）要让人看见；同一个面板重复
    /// 导入不出声，也不动 profiles.json 的一个字节。
    #[test]
    fn changing_the_update_source_is_announced_and_a_repeat_import_changes_nothing() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        let t = import_from_panel(&s, &pp, "alice", vec![hy2_direct_node()]);
        assert!(
            t.lines()
                .any(|l| l == "自动更新来源改为 https://panel.example.com"),
            "从无到有也是换来源：\n{t}"
        );

        let before = s.get("/opt/bui-c/profiles.json").unwrap();
        let t = import_from_panel(&s, &pp, "alice", vec![hy2_direct_node()]);
        assert!(!t.contains("自动更新来源"), "来源没变不出声：\n{t}");
        assert!(t.contains("节点 alice-hy2-direct 无变化"), "{t}");
        assert_eq!(
            s.get("/opt/bui-c/profiles.json").unwrap(),
            before,
            "节点无变化且面板相同：profiles.json 逐字节不变"
        );

        // 同一个面板换了用户名：只更新 username，不提示
        let t = import_from_panel(&s, &pp, "bob", vec![hy2_direct_node()]);
        assert!(!t.contains("自动更新来源"), "{t}");
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(
            saved
                .panel
                .as_ref()
                .map(|x| (x.base_url.as_str(), x.username.as_str())),
            Some(("https://panel.example.com", "bob"))
        );

        // 换了面板：写并提示
        let n = FakeNet::new();
        n.route(
            "https://other.example.com/api/nodes/bob",
            nodes_payload("bob", vec![hy2_direct_node()]),
        );
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(
            &parse(&[
                "import",
                "--panel",
                "https://other.example.com",
                "--user",
                "bob",
            ]),
            &mut ctx,
        )
        .unwrap();
        let t = ctx.transcript.clone();
        assert!(
            t.lines()
                .any(|l| l == "自动更新来源改为 https://other.example.com"),
            "{t}"
        );
        assert_eq!(
            Profiles::load(&s, &pp).unwrap().panel.map(|x| x.base_url),
            Some("https://other.example.com".to_string())
        );
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
        // 盘上已装的正是 manifest 里那一份构建：这轮自更新什么都不换，但照样算「更新过」
        s.put(crate::paths::SELF_BIN, "bui-c-installed");
        let n = FakeNet::new();
        n.route(crate::check::PROBE_URL, FakeReply::Status(204));
        n.route(
            "https://panel.example.com/packages/manifest.json",
            FakeReply::Text(format!(
                r#"{{"version":"{ver}","kernels":{{"client_sing_box":"1.14.5"}},"artifacts":{{"bui-c-linux-{arch}":{{"url":"https://github.com/x/bui-c","sha256":"{sha}"}}}}}}"#,
                ver = crate::VERSION,
                arch = crate::update::arch_suffix(),
                sha = crate::update::sha256_hex(b"bui-c-installed"),
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
        assert!(
            t.lines()
                .any(|l| l == "发现 2 项异常，已重启 bui-c.service，下次退避 1 分钟"),
            "timer 巡检保留退避说法（菜单 [5] 才不提）：{t}"
        );
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

    /// 菜单 `[5]` 是人手动点的：发现异常就直接重启，不受 timer 的 1/2/4 分钟退避约束，
    /// 结果行也不提「下次退避」。timer 触发的 `bui-c check` 保持原样。
    #[test]
    fn menu_check_restarts_right_away_and_does_not_talk_about_backoff() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        s.reply("systemctl is-active --quiet bui-c.service", 3, "");
        let mut prof = profiles_socks();
        prof.auto_update = false;
        prof.save(&s, &pp).unwrap();
        let n = FakeNet::new();
        n.route(crate::check::PROBE_URL, FakeReply::Status(502));
        // 连按两次 [5]：第二次还在 timer 的退避窗口里。结果不止一行，每次都先停下来等回车
        let mut p = Scripted::from(["5", "", "5", "", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert_eq!(
            t.lines()
                .filter(|l| *l == "  发现 2 项异常，已重启 bui-c.service")
                .count(),
            2,
            "两次都直接重启：\n{t}"
        );
        assert_eq!(
            p.asked.iter().filter(|q| *q == "回车返回菜单").count(),
            2,
            "异常逐条列出，看完回车再回主菜单：{:?}",
            p.asked
        );
        assert!(
            t.lines()
                .any(|l| l == "  上次：发现 2 项异常，已重启 bui-c.service"),
            "{t}"
        );
        assert_eq!(
            s.calls()
                .iter()
                .filter(|c| *c == "systemctl restart bui-c.service")
                .count(),
            2,
            "{t}"
        );
        assert!(!t.contains("退避"), "手动检查不提退避：\n{t}");
        let items: Vec<&str> = t.lines().filter(|l| l.starts_with("    - ")).collect();
        assert_eq!(
            items[..2],
            [
                "    - bui-c.service 没在运行",
                "    - 探测 www.gstatic.com/generate_204 返回 HTTP 502"
            ],
            "逐条列异常：\n{t}"
        );

        // timer 触发的巡检照旧退避
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["check"]), &mut ctx).unwrap();
        assert!(ctx.transcript.contains("退避中"), "{}", ctx.transcript);
    }

    /// `[5] 连接检查` 与 timer 巡检：TUN 模式下重启完 is-active 立刻为真，接口还没起来。
    /// 跟菜单里的「重启」一样等接口，就绪 / 超时各补一句。
    #[test]
    fn check_restart_in_tun_mode_waits_for_the_interface() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        s.reply("ip link show bui-tun", 0, "5: bui-tun");
        let mut prof = crate::testutil::profiles_tun();
        prof.auto_update = false;
        prof.save(&s, &pp).unwrap();
        let n = FakeNet::new();
        n.route(crate::check::PROBE_URL, FakeReply::Status(502));
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["check"]), &mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert!(t.contains("已重启 bui-c.service"), "{t}");
        assert!(!s.sleeps().is_empty(), "重启后要等接口：{t}");
        assert_eq!(
            t.lines().last(),
            Some("bui-tun 已就绪"),
            "就绪补一句：\n{t}"
        );

        // 接口一直没起来：等满 5 秒，指到 [4] 的日志
        let s = FakeSys::new();
        ready(&s);
        s.reply("ip link show bui-tun", 1, "");
        prof.save(&s, &pp).unwrap();
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["check"]), &mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert_eq!(
            s.sleeps().len(),
            crate::engine::APPLY_POLL_STEPS as usize,
            "{t}"
        );
        assert_eq!(
            t.lines().last(),
            Some("bui-tun 接口 5 秒内没起来，查 [4] 服务控制 → 最近日志"),
            "\n{t}"
        );

        // SOCKS 模式没有接口可等
        let s = FakeSys::new();
        ready(&s);
        let mut socks = profiles_socks();
        socks.auto_update = false;
        socks.save(&s, &pp).unwrap();
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["check"]), &mut ctx).unwrap();
        assert!(ctx.transcript.contains("已重启 bui-c.service"));
        assert!(s.sleeps().is_empty());
        assert!(!ctx.transcript.contains("bui-tun"), "{}", ctx.transcript);
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
        // 真机 9 个节点时这一行把全部名字 join 起来，一行两百多列
        assert_eq!(
            ctx.out, "v3 的 1 个节点都已导入过，未做任何改动\n",
            "不串节点名"
        );

        // 有残留的 v3 单元（比如手动装回来过）：同样不串名字
        s.put("/etc/systemd/system/xray-client.service", "[Unit]");
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["import-v3"]), &mut ctx).unwrap();
        assert_eq!(
            ctx.out,
            "v3 的 1 个节点都已导入过，清掉 1 个残留的 v3 单元\n"
        );
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
        // 盘上已装的 bui-c；manifest 里本机架构的 bui-c 资产 sha 与它相同 = 同一份构建
        s.put(crate::paths::SELF_BIN, "bui-c-installed");
        let installed = crate::update::sha256_hex(b"bui-c-installed");
        let manifest = |ver: &str, sha: &str| {
            FakeReply::Text(format!(
                r#"{{"version":"{ver}","kernels":{{"client_sing_box":"1.14.5"}},"artifacts":{{"bui-c-linux-{arch}":{{"url":"https://github.com/x/bui-c","sha256":"{sha}"}}}}}}"#,
                arch = crate::update::arch_suffix()
            ))
        };
        n.route(
            "https://panel.example.com/packages/manifest.json",
            manifest("9.9.9", &installed),
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
        assert!(menu::render_options(&engine_status(&ctx, &prof2), 80).contains("★ 有新版"));

        // 再查一次，manifest 与本机同版、且盘上就是那一份构建 → 标记清掉
        n.route(
            "https://panel.example.com/packages/manifest.json",
            manifest(crate::VERSION, &installed),
        );
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["update", "--check-only"]), &mut ctx).unwrap();
        assert!(ctx.transcript.contains("已是最新"), "{}", ctx.transcript);
        assert!(!Runtime::load(&s, &pp).update_available);
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n2, &pp, &mut p, false, false);
        dispatch(&parse(&["status"]), &mut ctx).unwrap();
        let prof3 = Profiles::load(&s, &pp).unwrap();
        assert!(!menu::render_options(&engine_status(&ctx, &prof3), 80).contains("★ 有新版"));
    }

    /// 与服务端 `kernels::bui_build_differs` 同口径：版本相同、但 manifest 里本机架构的 bui-c
    /// 不是盘上这一份（rc 通道的同版本重建）也算有新版——菜单挂 ★，`--check-only` 说清是同版本的
    /// 新构建，而不是说「已是最新」。
    #[test]
    fn update_check_only_reports_a_same_version_rebuild() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        let mut prof = profiles_socks();
        prof.panel = Some(crate::profiles::Panel {
            base_url: "https://panel.example.com".into(),
            username: "alice".into(),
        });
        prof.save(&s, &pp).unwrap();
        s.put(crate::paths::SELF_BIN, "bui-c-rc6");
        let n = FakeNet::new();
        n.route(
            "https://panel.example.com/packages/manifest.json",
            FakeReply::Text(format!(
                r#"{{"version":"{ver}","kernels":{{"client_sing_box":"1.14.5"}},"artifacts":{{"bui-c-linux-{arch}":{{"url":"https://github.com/x/bui-c","sha256":"{sha}"}}}}}}"#,
                ver = crate::VERSION,
                arch = crate::update::arch_suffix(),
                sha = crate::update::sha256_hex(b"bui-c-rc7"),
            )),
        );
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["update", "--check-only"]), &mut ctx).unwrap();
        assert!(
            ctx.transcript.contains("同版本的新构建"),
            "{}",
            ctx.transcript
        );
        assert!(!ctx.transcript.contains("已是最新"), "{}", ctx.transcript);
        assert!(Runtime::load(&s, &pp).update_available, "菜单要挂 ★");
        assert_eq!(
            s.get(crate::paths::SELF_BIN).unwrap(),
            "bui-c-rc6",
            "只检查不换"
        );
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

    /// `Profiles.panel` 是 root 每日自更新的 manifest 与二进制首选来源，sha256 也出自同一份
    /// manifest（没有签名）：谁被记成 panel，谁就能在这台机器上以 root 跑代码。订阅地址可以是
    /// 任意第三方主机（机场、转换服务），光凭「能返回 base64 节点列表」证明不了它是 v4 面板，
    /// 所以 `--sub` 只导节点、不记 panel。
    #[test]
    fn import_sub_does_not_record_the_panel_as_an_update_source() {
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
        assert_eq!(saved.panel, None, "订阅主机不能成为 root 自更新来源");
    }

    /// 菜单 `[3]` 里 `/api/sub` 在面板接口取不到时退回订阅、以及其它 http(s) 订阅地址：
    /// 同样只导节点，不记 panel（理由见上一条）。已有的 panel 也不被清掉。
    #[test]
    fn menu_subscription_imports_never_record_the_panel() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        profiles_socks().save(&s, &pp).unwrap(); // panel 为空
        let n = FakeNet::new();
        n.route(
            "https://sub.example.com/api/nodes/alice",
            FakeReply::Status(404),
        );
        n.route("https://sub.example.com/api/sub/alice", b64(BOB_REALITY));
        let mut p = Scripted::from(["3", "https://sub.example.com/api/sub/alice", "", "n", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(saved.profiles.len(), 2, "{t}");
        assert_eq!(saved.panel, None, "/api/sub 回退不证明是 v4 面板：\n{t}");

        let n = FakeNet::new();
        n.route("https://other.example.com/link/abc", b64(BOB_REALITY));
        let mut p = Scripted::from(["3", "https://other.example.com/link/abc", "", "n", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        assert_eq!(
            Profiles::load(&s, &pp).unwrap().panel,
            None,
            "{}",
            ctx.transcript
        );
    }

    /// `http://` 面板照样能导节点（`/api/nodes` 证明是 v4 面板），但明文源不当自更新来源。
    #[test]
    fn import_from_an_http_panel_keeps_the_nodes_but_not_the_update_source() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        let n = FakeNet::new();
        n.route(
            "http://panel.example.com/api/nodes/alice",
            nodes_payload("alice", vec![hy2_direct_node()]),
        );
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(
            &parse(&[
                "import",
                "--panel",
                "http://panel.example.com",
                "--user",
                "alice",
            ]),
            &mut ctx,
        )
        .unwrap();
        let t = ctx.transcript.clone();
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(saved.profiles.len(), 1, "{t}");
        assert_eq!(saved.panel, None, "{t}");
        assert!(
            t.lines()
                .any(|l| l == "面板地址不是 https，不作为自动更新来源"),
            "{t}"
        );

        // https 面板照旧记下
        let n = FakeNet::new();
        n.route(
            "https://panel.example.com/api/nodes/alice",
            nodes_payload("alice", vec![hy2_direct_node()]),
        );
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
        assert_eq!(
            Profiles::load(&s, &pp).unwrap().panel.map(|x| x.base_url),
            Some("https://panel.example.com".to_string())
        );
        assert!(!ctx.transcript.contains("不是 https"), "{}", ctx.transcript);
    }

    /// `BUI_C_PANEL=http://…` 不能把 root 自更新引到明文源上：忽略它，照旧用记下的 https 面板。
    #[test]
    fn bui_c_panel_env_is_ignored_unless_https() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        s.set_env("BUI_C_PANEL", "http://env.example.com");
        let mut prof = profiles_socks();
        prof.panel = Some(crate::profiles::Panel {
            base_url: "https://panel.example.com".into(),
            username: "alice".into(),
        });
        prof.save(&s, &pp).unwrap();
        let n = FakeNet::new();
        let manifest = FakeReply::Text(format!(
            r#"{{"version":"{}","kernels":{{"client_sing_box":"1.14.5"}},"artifacts":{{}}}}"#,
            crate::VERSION
        ));
        n.route(
            "http://env.example.com/packages/manifest.json",
            manifest.clone(),
        );
        n.route("https://panel.example.com/packages/manifest.json", manifest);
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["update", "--check-only"]), &mut ctx).unwrap();
        assert!(
            !n.log().iter().any(|l| l.contains("env.example.com")),
            "{:?}",
            n.log()
        );
        assert!(ctx.transcript.contains("来源 面板"), "{}", ctx.transcript);
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
        s.put("/etc/systemd/system/hysteria-client.service", "[Unit]");
        let n = FakeNet::new();
        // 没有 profiles.json + 有 v3 目录 + v3 单元还在 → 进菜单前问一次；y 导入，再 0 退出
        let mut p = Scripted::from(["y", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(saved.profiles.len(), 1);
        assert_eq!(saved.active.as_deref(), Some("hysteria2-1"));
        assert!(s.called("systemctl stop hysteria-client.service"));
        assert!(
            !s.exists(&pp.unit("hysteria-client.service")),
            "迁移把 v3 单元删了"
        );
        // 导入成功：「当前节点：…」不算附加行（状态区本来就显示），不停，结果进「上次：」行
        assert_eq!(pauses(&p.asked), 0, "{:?}", p.asked);
        assert!(
            t.lines().any(|l| l.starts_with("  上次：导入 1 个节点")),
            "{t}"
        );

        // 迁移 → 删光节点 → 再进菜单：v3 目录按约定留着，单元已卸，不再邀请
        let mut emptied = Profiles::load(&s, &pp).unwrap();
        emptied.profiles.clear();
        emptied.active = None;
        emptied.save(&s, &pp).unwrap();
        let r = run_menu(&s, &n, &pp, &["0"], true);
        assert!(!r.t.contains("发现 v3 客户端目录"), "{}", r.t);
        assert!(
            !r.asked.iter().any(|q| q.contains("导入 v3 的节点")),
            "{:?}",
            r.asked
        );

        // 答 n：不导入，只提示一次，菜单照常进（ctx.out 会被 flush 清空，断言看 transcript）
        let s2 = FakeSys::new();
        ready(&s2);
        s2.put(
            "/opt/hysteria-client/configs/hysteria2-1/uri.txt",
            "hysteria2://alice:hy2-pw@panel.example.com:10000/?sni=panel.example.com#alice-HY2%E7%9B%B4%E8%BF%9E",
        );
        s2.put("/etc/systemd/system/hysteria-client.service", "[Unit]");
        let mut p2 = Scripted::from(["n", "0"]);
        let mut ctx2 = Ctx::new(&s2, &n, &pp, &mut p2, false, false);
        menu_loop(&mut ctx2).unwrap();
        assert!(Profiles::load(&s2, &pp).unwrap().profiles.is_empty());
        // 还没进菜单：命令与菜单项都给，菜单项按统一叫法写成「[7] 从 v3 导入」
        assert!(
            ctx2.transcript.lines().any(|l| l
                == "  已跳过。随时可以跑 `bui-c import-v3`，或在菜单里选 [7] 从 v3 导入"),
            "{}",
            ctx2.transcript
        );
        assert!(!s2.called("systemctl stop hysteria-client.service"));
    }

    /// 已迁移的机器：v3 目录按约定留着当回滚素材，v3 单元已被卸掉。删光节点后再进菜单，
    /// 不该再邀请从 v3 导入（spec §5.7 表第 4 行）。只认三个主单元，health 定时器的残留不算。
    #[test]
    fn first_run_offer_needs_a_live_v3_unit() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        s.put(
            "/opt/hysteria-client/configs/hysteria2-1/uri.txt",
            "hysteria2://alice:hy2-pw@panel.example.com:10000/?sni=panel.example.com#alice-HY2%E7%9B%B4%E8%BF%9E",
        );
        s.put("/etc/systemd/system/hysteria-health.timer", "[Unit]");
        let n = FakeNet::new();

        // 有 v3 目录，主单元一个都不在 → 不问，直接进菜单；0 退出
        let mut p = Scripted::from(["0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert!(!t.contains("发现 v3 客户端目录"), "{t}");
        assert!(
            !p.asked.iter().any(|q| q.contains("导入 v3 的节点")),
            "{:?}",
            p.asked
        );

        // 补上一个主单元文件（还没迁移）→ 问；答 n，再 0 退出
        s.put("/etc/systemd/system/xray-client.service", "[Unit]");
        let mut p = Scripted::from(["n", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert!(
            t.lines()
                .any(|l| l == "  发现 v3 客户端目录 /opt/hysteria-client"),
            "{t}"
        );
        assert!(
            p.asked.iter().any(|q| q.contains("导入 v3 的节点")),
            "{:?}",
            p.asked
        );
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
    fn ctx_size_falls_back_to_80_by_24_and_follows_the_injected_size() {
        let pp = paths();
        let s = FakeSys::new();
        let n = FakeNet::new();
        let mut p = Scripted::from([]);
        let ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        assert_eq!((ctx.width(), ctx.rows()), (80, 24));
        assert!(!ctx.screen_ctl(), "拿不到尺寸就不清屏");
        s.set_term_size(Some((47, 17)));
        assert_eq!((ctx.width(), ctx.rows()), (47, 17));
        assert!(ctx.screen_ctl());
    }

    #[test]
    fn ctx_screen_ctl_also_needs_someone_at_the_keyboard() {
        // stdout 是终端、stdin 是管道：排版照终端宽度，但不清屏
        let pp = paths();
        let s = FakeSys::new();
        s.set_term_size(Some((60, 20)));
        let n = FakeNet::new();
        let mut p = Piped(Scripted::from([]));
        let ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        assert_eq!((ctx.width(), ctx.rows()), (60, 20));
        assert!(!ctx.screen_ctl());
    }

    #[test]
    fn ctx_size_treats_a_zero_count_as_unknown() {
        // 有的 pty 只设了列数、行数报 0：按拿不到回落到 24 行。
        // 用行数算可用高度的地方另外一律写 `rows().saturating_sub(2)`，不靠这里防下溢
        let pp = paths();
        let s = FakeSys::new();
        let n = FakeNet::new();
        let mut p = Scripted::from([]);
        let ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        s.set_term_size(Some((100, 0)));
        assert_eq!((ctx.width(), ctx.rows()), (100, 24));
        s.set_term_size(Some((0, 30)));
        assert_eq!((ctx.width(), ctx.rows()), (80, 30));
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
            t.lines().any(|l| l == "  无效选项：x（请输入 0-9 的数字）"),
            "结果行跟菜单一样缩进两列：\n{t}"
        );
        assert!(t.lines().any(|l| l == "  已切到 TUN 模式"), "{t}");
        // 菜单本身已经排好版，不能再叠一层缩进
        assert!(t.lines().any(|l| l.starts_with("  ── B-UI")), "{t}");
        assert!(t.lines().any(|l| l.starts_with("     [1] ")), "{t}");
        assert!(!t.lines().any(|l| l.starts_with("    ── B-UI")), "{t}");
        assert_eq!(ctx.indent, "", "退出菜单后恢复顶格");
    }

    #[test]
    fn clear_screen_sends_exactly_home_and_erase_display() {
        // 逐字节钉住序列：ESC[H ESC[2J，防止混进会抹掉回滚的 ESC[3J
        let pp = paths();
        let s = FakeSys::new();
        s.set_term_size(Some((60, 20)));
        let n = FakeNet::new();
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        ctx.say("上一屏的最后一行");
        let mut w = Vec::new();
        ctx.clear_into(&mut w);
        assert_eq!(w, [0x1b_u8, 0x5b, 0x48, 0x1b, 0x5b, 0x32, 0x4a]);
        assert_eq!(ctx.clears, 1);
        assert!(ctx.out.is_empty(), "清屏前先把待打印缓冲冲出去");
        assert!(!ctx.transcript.contains('\u{1b}'), "序列不经过 emit");

        // stdin 是管道：一个字节都不写，也不计数
        let mut p = Scripted::from([]);
        p.tty = false;
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        let mut w = Vec::new();
        ctx.clear_into(&mut w);
        assert!(w.is_empty());
        assert_eq!(ctx.clears, 0);
    }

    #[test]
    fn menu_does_not_clear_when_stdin_is_not_a_terminal() {
        // `printf '1\n0\n0\n' | sudo bui-c`：stdout 是终端，stdin 是管道
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        two_nodes(&s, &pp);
        s.set_term_size(Some((60, 30)));
        let n = FakeNet::new();
        let mut p = Scripted::from(["1", "0", "0"]);
        p.tty = false;
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        assert_eq!(ctx.clears, 0);
        assert_eq!(
            ctx.transcript.matches("B-UI 客户端").count(),
            2,
            "照样重画，只是不清屏"
        );
    }

    #[test]
    fn the_last_line_keeps_both_ends_of_a_long_node_name_at_40_columns() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        let mut prof = crate::testutil::baiyi_like();
        prof.mode = Mode::Socks; // 不等 TUN 就绪，切换只打一行
        prof.save(&s, &pp).unwrap();
        s.set_term_size(Some((40, 30)));
        let n = FakeNet::new();
        // [4] 是 rick-node.example-a.net-reality-direct（38 列），[5] 是同一台服务器的 -reality-resi
        let mut p = Scripted::from(["1", "4", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert_eq!(
            Profiles::load(&s, &pp).unwrap().active.as_deref(),
            Some("rick-node.example-a.net-reality-direct")
        );
        let last = t
            .lines()
            .find(|l| l.starts_with("  上次："))
            .unwrap_or_else(|| panic!("{t}"));
        assert!(last.contains('…'), "{last}");
        assert!(
            last.ends_with("direct"),
            "尾巴要留住 direct / resi 的区别：{last}"
        );
        assert!(menu::budget_width(last) <= menu::line_limit(40), "{last}");
        assert_eq!(last, "  上次：已切到 rick-nod…reality-direct");
        assert!(
            !p.asked.iter().any(|q| q == "回车返回菜单"),
            "切节点只打一行，不停：{:?}",
            p.asked
        );
    }

    #[test]
    fn a_typo_keeps_the_last_line_and_never_echoes_escape_bytes() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        s.reply("ip link show bui-tun", 0, "5: bui-tun");
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        // 2 → y 切到 TUN（一行结果，不停）→ 方向键 ↑ 再回车 → 直接回车刷新 → 0
        let mut p = Scripted::from(["2", "y", "\u{1b}[A", "", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert!(!t.contains('\u{1b}'), "{t:?}");
        assert!(
            t.lines()
                .any(|l| l == "  无效选项：?[A（请输入 0-9 的数字）"),
            "{t}"
        );
        assert_eq!(
            t.matches("B-UI 客户端").count(),
            3,
            "进菜单、切完、回车刷新；输错不重画：\n{t}"
        );
        assert_eq!(
            t.lines()
                .filter(|l| *l == "  上次：已切到 TUN 模式")
                .count(),
            2,
            "输错不动它，回车刷新也保留：\n{t}"
        );
        assert!(
            !p.asked.iter().any(|q| q == "回车返回菜单"),
            "一行结果不停：{:?}",
            p.asked
        );
    }

    #[test]
    fn a_failure_pauses_before_the_redraw_and_stays_in_the_last_line() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new(); // manifest 的源一个都没登记：检查更新失败
        let mut p = Scripted::from(["6", "", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert!(t.lines().any(|l| l.starts_with("  失败：")), "{t}");
        assert!(t.lines().any(|l| l.starts_with("  上次：失败：")), "{t}");
        assert_eq!(
            p.asked,
            vec!["选择 [0-9]", "回车返回菜单", "选择 [0-9]"],
            "失败先停，看完回车再清屏回主菜单，0 由主菜单读走：\n{t}"
        );
    }

    #[test]
    fn status_list_and_menu_follow_the_terminal_width() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        two_nodes(&s, &pp);
        let n = FakeNet::new();
        let run = |args: &[&str]| {
            let mut p = Scripted::from([]);
            let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
            dispatch(&parse(args), &mut ctx).unwrap();
            ctx.transcript
        };
        // 管道里拿不到尺寸：按 80 列排，标准版式，详情行带 kind
        let status = run(&["status"]);
        assert!(
            status.contains(
                "   节点   ●  运行中  alice-hy2-direct\n          HY2直连  hy2-direct  panel.example.com:10000\n"
            ),
            "{status}"
        );
        let list = run(&["list"]);
        assert!(
            list.contains("    HY2直连  hy2-direct  panel.example.com:10000\n"),
            "{list}"
        );
        // 40 列终端：窄版式，每一行按容量口径都不超过 39 列
        s.set_term_size(Some((40, 20)));
        let status = run(&["status"]);
        assert!(
            status.contains("   节点 ● 运行中\n        alice-hy2-direct\n"),
            "{status}"
        );
        assert!(
            status.contains("   代理 SOCKS5 :1080  HTTP :8080\n"),
            "{status}"
        );
        let list = run(&["list"]);
        assert!(
            list.contains("    Reality直连\n"),
            "放不下服务器就只留 label：{list}"
        );
        // 1 进节点列表 → 0 返回 → 0 退出
        let mut p = Scripted::from(["1", "0", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert!(
            t.lines().any(|l| l == "  [0] 返回"),
            "节点列表用窄版式：\n{t}"
        );
        for text in [&status, &list, &t] {
            for l in text.lines() {
                assert!(menu::budget_width(l) <= 39, "{l:?}\n{text}");
            }
        }
    }

    #[test]
    fn menu_loop_restores_indent_even_when_it_errors() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        s.put("/opt/bui-c/profiles.json", "{ 不是 json");
        let n = FakeNet::new();
        let mut p = Scripted::from(["0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, true); // bui-c -y
        assert!(menu_loop(&mut ctx).is_err());
        assert_eq!(
            ctx.indent, "",
            "run() 接着打的「错误：…」要顶格，不能沿用菜单缩进"
        );
        assert!(ctx.yes, "菜单里换成假的 -y，出错退出也要还原");
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
    fn json_output_ends_with_a_newline() {
        // `bui-c status --json` 以前最后一行是 `}`，shell 提示符接在它后面
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        for (args, tail) in [(["status", "--json"], "}\n"), (["list", "--json"], "]\n")] {
            let mut p = Scripted::from([]);
            let mut ctx = Ctx::new(&s, &n, &pp, &mut p, true, false);
            dispatch(&parse(&args), &mut ctx).unwrap();
            assert!(ctx.out.ends_with(tail), "{args:?}：{:?}", ctx.out);
            serde_json::from_str::<serde_json::Value>(&ctx.out).unwrap();
        }
    }

    /// 管道喂进来的 stdin：`Stdin` 不打提示（`interactive` 为假）、停顿不读，其余照 [`Scripted`]。
    struct Piped(Scripted);
    impl Prompt for Piped {
        fn read(&mut self, prompt: &str) -> Result<Option<String>> {
            self.0.read(prompt)
        }
        fn lines_until_blank(&mut self, prompt: &str) -> Result<Vec<String>> {
            self.0.lines_until_blank(prompt)
        }
        fn confirm(&mut self, prompt: &str) -> Result<bool> {
            self.0.confirm(prompt)
        }
        /// 与真 `Stdin` 在管道里一样：没人会按回车，直接返回、不读（spec §4.4）。
        fn pause(&mut self, _prompt: &str) -> Result<()> {
            Ok(())
        }
        fn interactive(&self) -> bool {
            false
        }
    }

    #[test]
    fn menu_eof_ends_the_prompt_line_before_exiting() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new();

        // Ctrl-D：终端不回显换行，shell 提示符会接在「▸ 选择 [0-9]：」后面
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        assert!(
            ctx.transcript.ends_with("     [0] 退出\n\n"),
            "EOF 退出先补一个换行：{:?}",
            ctx.transcript
        );

        // 输入 0 退出：回车已经换过行，不补
        let mut p = Scripted::from(["0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        assert!(ctx.transcript.ends_with("     [0] 退出\n"));
        assert!(!ctx.transcript.ends_with("\n\n"), "{:?}", ctx.transcript);

        // stdin 不是终端：没打过提示，也不补
        let mut p = Piped(Scripted::from([]));
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        assert!(ctx.transcript.ends_with("     [0] 退出\n"));
        assert!(!ctx.transcript.ends_with("\n\n"), "{:?}", ctx.transcript);
    }

    /// 已经被关掉的 stdout（`bui-c list | head -1`）：每次写都 BrokenPipe，记下被写了几次。
    struct ClosedPipe(usize);
    impl std::io::Write for ClosedPipe {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            self.0 += 1;
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn flush_swallows_a_broken_pipe_and_stops_writing() {
        let pp = paths();
        let s = FakeSys::new();
        let n = FakeNet::new();
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        let mut w = ClosedPipe(0);
        ctx.say("第一行");
        ctx.flush_into(&mut w); // 以前的 print! 在这里 panic（退出码 101）
        assert!(ctx.stdout_closed, "记下 stdout 已关");
        assert!(ctx.out.is_empty());
        ctx.say("第二行");
        ctx.flush_into(&mut w);
        assert_eq!(w.0, 1, "关掉之后不再尝试写");
        assert!(ctx.transcript.contains("第二行"));
    }

    #[test]
    fn menu_loop_ends_quietly_once_stdout_is_closed() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        let mut p = Scripted::from(["2", "y", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        ctx.stdout_closed = true;
        menu_loop(&mut ctx).unwrap();
        assert_eq!(
            Profiles::load(&s, &pp).unwrap().mode,
            Mode::Socks,
            "没人看得到输出，不该接着执行菜单命令"
        );
        assert!(p.asked.is_empty(), "{:?}", p.asked);
    }

    #[test]
    fn errors_go_to_stderr_and_stdout_keeps_only_normal_output() {
        let pp = paths();
        let s = FakeSys::new();
        let n = FakeNet::new();
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        ctx.say("正常输出");
        let mut err = Vec::new();
        let rc = finish(&mut ctx, Err(Error::msg("节点 nope 不存在")), &mut err);
        assert_eq!(String::from_utf8(err).unwrap(), "错误：节点 nope 不存在\n");
        assert!(
            !ctx.transcript.contains("错误"),
            "stdout 只放正常输出：{}",
            ctx.transcript
        );
        assert_eq!(rc, std::process::ExitCode::FAILURE);

        let mut err = Vec::new();
        assert_eq!(
            finish(&mut ctx, Ok(()), &mut err),
            std::process::ExitCode::SUCCESS
        );
        assert!(err.is_empty());
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

    /// 跑完一遍菜单留下的东西（spec §12.1 的测试基础设施）。
    struct Ran {
        /// 屏上的全部文字（transcript）
        t: String,
        /// 按顺序被问过的提示
        asked: Vec<String>,
        clears: usize,
    }

    /// 跑一遍菜单：`tty` 为假按管道形态（不清屏；`Scripted` 的停顿照样消费一行，脚本里写
    /// `""` 代表回车）。跑完统一断言 transcript 不含 ESC。
    fn run_menu(s: &FakeSys, n: &FakeNet, pp: &Paths, inputs: &[&str], tty: bool) -> Ran {
        let mut p = Scripted {
            queue: inputs.iter().map(|x| x.to_string()).collect(),
            asked: Vec::new(),
            tty,
        };
        let mut ctx = Ctx::new(s, n, pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let (t, clears) = (ctx.transcript.clone(), ctx.clears);
        assert!(!t.contains('\u{1b}'), "transcript 不能含 ESC：{t:?}");
        Ran {
            t,
            asked: p.asked,
            clears,
        }
    }

    /// 停过几次：「回车返回菜单」被问了几次（spec §4.5）。
    fn pauses(asked: &[String]) -> usize {
        asked.iter().filter(|q| *q == "回车返回菜单").count()
    }

    /// 进菜单前的 v3 导入邀请：导入失败要先停下来看原因，摘要进「上次：」行——
    /// 否则接下来第一次清屏就把它抹掉了，而这正是迁移那一刻。
    #[test]
    fn a_failed_v3_import_at_the_invitation_pauses_before_the_first_clear() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        // v3 目录在、单元还在，但节点链接解析不了 → 导入报「…下没有可导入的节点」
        s.put(
            "/opt/hysteria-client/configs/hysteria2-1/uri.txt",
            "not-a-node-uri",
        );
        s.put("/etc/systemd/system/hysteria-client.service", "[Unit]");
        s.set_term_size(Some((60, 30)));
        let n = FakeNet::new();
        let r = run_menu(&s, &n, &pp, &["y", "", "0"], true);
        assert_eq!(pauses(&r.asked), 1, "{:?}", r.asked);
        let fail = r.t.find("  失败：").unwrap_or_else(|| panic!("{}", r.t));
        assert!(
            fail < r.t.find("B-UI 客户端").unwrap(),
            "先打失败、停下来，再清屏画主菜单：\n{}",
            r.t
        );
        assert!(
            r.t.lines()
                .any(|l| l.starts_with("  上次：失败：") && l.contains("没有可导入的节点")),
            "{}",
            r.t
        );
        assert_eq!(r.clears, 1, "只清过主菜单那一次");
        assert!(
            s.exists(&pp.unit("hysteria-client.service")),
            "失败时 v3 原样保留"
        );

        // 答否：说一句就进菜单，不停，这句留在「上次：」行
        let r = run_menu(&s, &n, &pp, &["n", "0"], true);
        assert_eq!(pauses(&r.asked), 0, "{:?}", r.asked);
        assert!(
            r.t.lines().any(|l| l.starts_with("  上次：已跳过。")),
            "{}",
            r.t
        );
    }

    /// `printf '5\n0\n' | sudo bui-c`：没人按回车，停顿不读，`0` 留给主菜单（spec §4.4）。
    /// `Piped` 的 `pause` 与真 `Stdin` 在管道里的行为一致：直接返回。
    #[test]
    fn a_piped_script_is_not_eaten_by_a_pause() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        s.reply("systemctl is-active --quiet bui-c.service", 3, "");
        let mut prof = profiles_socks();
        prof.auto_update = false;
        prof.save(&s, &pp).unwrap();
        let n = FakeNet::new();
        n.route(crate::check::PROBE_URL, FakeReply::Status(502));
        let mut p = Piped(Scripted::from(["5", "0"]));
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert!(
            t.lines()
                .any(|l| l == "  发现 2 项异常，已重启 bui-c.service"),
            "多行结果，交互终端里会停：\n{t}"
        );
        assert_eq!(
            p.0.asked,
            vec!["选择 [0-9]", "选择 [0-9]"],
            "0 由主菜单读走"
        );
        assert_eq!(t.matches("B-UI 客户端").count(), 2, "{t}");
        assert!(
            t.lines()
                .any(|l| l == "  上次：发现 2 项异常，已重启 bui-c.service"),
            "{t}"
        );
    }

    /// [3] 粘贴失败时列出的 scheme 是用户贴的原文：误粘进来的颜色码先换成 `?`，
    /// ESC 不能写进 transcript 与终端（run_menu 里统一断言）。
    #[test]
    fn a_pasted_scheme_with_escape_bytes_is_sanitized_in_the_failure_line() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        let r = run_menu(
            &s,
            &n,
            &pp,
            &["3", "\u{1b}[31mss://secret@h:1#x", "", "", "0"],
            true,
        );
        assert!(
            r.t.lines()
                .any(|l| l == "  失败：1 行都不是 hysteria2:// 或 vless:// 链接（?[31mss://）"),
            "{}",
            r.t
        );
        assert!(!r.t.contains("secret"), "{}", r.t);
    }

    /// [1] 与 [4] 输错时的回显照主菜单的做法：净化再截到行宽，误贴一整条链接也只占一行。
    #[test]
    fn junk_in_the_node_list_and_the_service_page_is_echoed_on_one_line() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        with_unit(&s);
        two_nodes(&s, &pp);
        s.set_term_size(Some((40, 30)));
        let n = FakeNet::new();
        let junk = "https://panel.example.com/api/sub/示例用户甲\u{1b}[A";
        let r = run_menu(&s, &n, &pp, &["1", junk, "0", "4", junk, "0", "0"], true);
        let echoes: Vec<&str> = r.t.lines().filter(|l| l.starts_with("  无效")).collect();
        assert_eq!(echoes.len(), 2, "{}", r.t);
        for l in &echoes {
            assert!(menu::budget_width(l) <= menu::line_limit(40), "{l}");
            assert!(l.contains('…'), "{l}");
        }
        assert_eq!(echoes[0], "  无效编号：https…（可选 1-2，0 返回）");
        assert!(
            echoes[1].starts_with("  无效选项：https://"),
            "{}",
            echoes[1]
        );
    }

    /// TUN 下第一次切换：先打「已为 bui-tun 接口放行 UFW…」再打「已切到 X」。两行要停
    /// （spec §4.3），但「上次：」行写切换的结果，不是第一行。
    #[test]
    fn the_switch_summary_says_the_switch_even_when_ufw_speaks_first() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        s.reply("ufw status", 0, "Status: active");
        s.reply("ip link show bui-tun", 0, "5: bui-tun");
        two_nodes(&s, &pp);
        let mut prof = Profiles::load(&s, &pp).unwrap();
        prof.mode = Mode::Tun;
        prof.save(&s, &pp).unwrap();
        let n = FakeNet::new();
        let r = run_menu(&s, &n, &pp, &["1", "2", "", "0"], true);
        assert!(
            r.t.lines()
                .any(|l| l == "  已为 bui-tun 接口放行 UFW（含 route allow）"),
            "{}",
            r.t
        );
        assert_eq!(pauses(&r.asked), 1, "两行结果照 §4.3 要停：{:?}", r.asked);
        assert!(
            r.t.lines()
                .any(|l| l == "  上次：已切到 alice-reality-direct"),
            "「上次：」行写切换的结果，不是第一行：\n{}",
            r.t
        );
    }

    /// 切完 TUN 没起来：先打「警告：…bui-tun 接口未就绪…」再打「已切到 X」。两行照 §4.3 停一次，
    /// 但「上次：」行不能只写「已切到 X」——回到主菜单的人会以为切成了（spec §4.2）。
    #[test]
    fn the_switch_summary_admits_the_tun_never_came_up() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        s.reply("ip link show bui-tun", 1, "");
        two_nodes(&s, &pp);
        let mut prof = Profiles::load(&s, &pp).unwrap();
        prof.mode = Mode::Tun;
        prof.save(&s, &pp).unwrap();
        let n = FakeNet::new();
        let r = run_menu(&s, &n, &pp, &["1", "2", "", "0"], true);
        assert!(r.t.contains("bui-tun 接口未就绪"), "{}", r.t);
        assert_eq!(pauses(&r.asked), 1, "两行结果照 §4.3 要停：{:?}", r.asked);
        assert!(
            r.t.lines()
                .any(|l| l == "  上次：已切到 alice-reality-direct，但 bui-tun 没起来"),
            "「上次：」行要说 TUN 没起来：\n{}",
            r.t
        );
    }

    /// 同上，40 列、名字 38 列：名字单独中间截断（spec §0.2 R6），「，但 bui-tun 没起来」整句
    /// 留住，整行按容量口径不超过行宽上限。
    #[test]
    fn the_tun_down_switch_summary_fits_40_columns_and_keeps_bui_tun() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        s.reply("ip link show bui-tun", 1, "");
        let mut prof = crate::testutil::baiyi_like();
        prof.mode = Mode::Tun;
        prof.save(&s, &pp).unwrap();
        s.set_term_size(Some((40, 30)));
        let n = FakeNet::new();
        // [4] 是 rick-node.example-a.net-reality-direct（38 列）
        let r = run_menu(&s, &n, &pp, &["1", "4", "", "0"], true);
        assert_eq!(
            Profiles::load(&s, &pp).unwrap().active.as_deref(),
            Some("rick-node.example-a.net-reality-direct")
        );
        assert_eq!(pauses(&r.asked), 1, "{:?}", r.asked);
        let last =
            r.t.lines()
                .find(|l| l.starts_with("  上次："))
                .unwrap_or_else(|| panic!("{}", r.t));
        assert!(menu::budget_width(last) <= menu::line_limit(40), "{last}");
        assert!(last.contains("bui-tun"), "{last}");
        assert!(
            last.starts_with("  上次：已切到 ") && last.ends_with("，但 bui-tun 没起来"),
            "名字单独中间截断，后半句不被尾截：{last}"
        );
        // 名字的预算 = 39 −「  上次：」8 −「已切到 」7 −「，但 bui-tun 没起来」19 = 5
        assert_eq!(last, "  上次：已切到 r…ct，但 bui-tun 没起来");
    }

    #[test]
    fn menu_clears_only_on_a_terminal_and_the_transcript_has_no_escape() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        two_nodes(&s, &pp);
        let n = FakeNet::new();
        s.set_term_size(Some((60, 30)));
        let mut p = Scripted::from(["1", "0", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        assert_eq!(ctx.clears, 3, "进菜单、进列表、回主菜单");
        assert!(!ctx.transcript.contains('\u{1b}'));
        let s2 = FakeSys::new();
        ready(&s2);
        two_nodes(&s2, &pp);
        let mut p = Scripted::from(["1", "0", "0"]);
        let mut ctx = Ctx::new(&s2, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        assert_eq!(ctx.clears, 0, "拿不到终端尺寸就不清屏");
    }

    #[test]
    fn a_typo_reasks_without_redrawing_and_the_last_line_reports_the_previous_action() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        two_nodes(&s, &pp);
        let n = FakeNet::new();
        let mut p = Scripted::from(["abc", "1", "2", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert!(t.contains("  无效选项：abc（请输入 0-9 的数字）"));
        assert_eq!(
            t.matches("B-UI 客户端").count(),
            2,
            "输错不重画；切换后重画一次"
        );
        assert!(
            t.lines()
                .any(|l| l.starts_with("  上次：已切到 alice-reality-direct")),
            "{t}"
        );
    }

    #[test]
    fn the_menu_ignores_the_global_yes_for_uninstall() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        two_nodes(&s, &pp);
        let n = FakeNet::new();
        let mut p = Scripted::from(["8", "", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, true); // bui-c -y
        menu_loop(&mut ctx).unwrap();
        // 先看 ctx.yes：ctx 借着 &mut p，用完它才能读 p.asked
        assert!(ctx.yes, "出了菜单恢复原值");
        assert!(
            p.asked.iter().any(|q| q.contains("确认卸载")),
            "{:?}",
            p.asked
        );
        assert!(s.exists(&pp.profiles()), "答空行就不卸载");
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
            "1", "9", "abc", "0", // 越界、不是数字：留在原地重问，0 才返回
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
        assert_eq!(t.matches("B-UI 客户端").count(), 4, "输错不回主菜单：{t}");
        assert_eq!(
            t.matches("     [0] 返回").count(),
            3,
            "重问不重画节点列表：{t}"
        );
        assert_eq!(
            p.asked.iter().filter(|q| *q == "选择节点编号").count(),
            5,
            "{:?}",
            p.asked
        );
    }

    /// 菜单 `[1]` 的节点列表跟服务控制子菜单一个样式：前空一行、两列缩进的标题。
    /// 以前编号行直接接在「▸ 选择 [0-9]：1」下面，看着像主菜单多出来的几行。
    #[test]
    fn menu_node_list_has_a_title_like_the_service_submenu() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        two_nodes(&s, &pp);
        let n = FakeNet::new();
        let mut p = Scripted::from(["1", "0", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        let lines: Vec<&str> = t.lines().collect();
        let at = lines
            .iter()
            .position(|l| *l == "  切换节点")
            .unwrap_or_else(|| panic!("缺标题：\n{t}"));
        assert_eq!(
            lines[at - 1..at + 2],
            ["", "  切换节点", "     [1] ★ alice-hy2-direct"],
            "\n{t}"
        );

        // 没有节点：同样有标题，下面是引导
        let s = FakeSys::new();
        ready(&s);
        let mut p = Scripted::from(["1", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        let lines: Vec<&str> = t.lines().collect();
        let at = lines
            .iter()
            .position(|l| *l == "  切换节点")
            .unwrap_or_else(|| panic!("缺标题：\n{t}"));
        assert_eq!(
            lines[at - 1..at + 2],
            ["", "  切换节点", "  没有节点，先用 [3] 导入节点"],
            "\n{t}"
        );
    }

    #[test]
    fn menu_node_pick_asks_again_after_junk_until_a_valid_number_or_eof() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        two_nodes(&s, &pp);
        let n = FakeNet::new();
        // 输错一次，接着给对的编号：直接切过去，不必再从主菜单按 1
        let mut p = Scripted::from(["1", "x", "2", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert!(
            t.lines().any(|l| l == "  无效编号：x（可选 1-2，0 返回）"),
            "{t}"
        );
        assert_eq!(
            Profiles::load(&s, &pp).unwrap().active.as_deref(),
            Some("alice-reality-direct"),
            "{t}"
        );
        assert!(!t.contains("无效选项"), "2 不能漏到主菜单：{t}");

        // 输错后 EOF：回主菜单（主菜单再读到 EOF 退出），不死循环
        let mut p = Scripted::from(["1", "x"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        assert_eq!(ctx.transcript.matches("B-UI 客户端").count(), 2);
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
        assert!(
            t.lines().any(|l| l == "  没有节点，先用 [3] 导入节点"),
            "{t}"
        );
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
            t.lines().any(|l| l == "  上次：已取消，没有导入任何节点"),
            "{t}"
        );
        assert_eq!(pauses(&p.asked), 0, "{:?}", p.asked);
        assert!(
            p.asked.contains(&"粘贴节点链接或订阅地址".to_string()),
            "提示要说清楚两种都能贴：{:?}",
            p.asked
        );
    }

    /// 菜单 `[3]` 的粘贴说明与「失败」行在 80 列终端里不折行（含 2 列缩进）：
    /// 以前分别是 82 列与 84 列。
    #[test]
    fn menu_import_prompt_and_failure_lines_fit_in_80_columns() {
        let head = menu::paste_head(PASTE_PROMPT);
        assert_eq!(head, "  粘贴节点链接或订阅地址（每行一个，空行结束）");
        assert!(
            menu::display_width(&head) <= 80,
            "{} 列：{head}",
            menu::display_width(&head)
        );

        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        // 失败要停：粘贴结束的空行之后再给一个回车，0 才轮到主菜单
        let mut p = Scripted::from([
            "3",
            "ss://secret@h:1#x",
            "https://panel.example.com/api/sub/alice",
            "",
            "",
            "0",
        ]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        let fail = t
            .lines()
            .find(|l| l.starts_with("  失败："))
            .unwrap_or_else(|| panic!("{t}"));
        assert!(
            menu::display_width(fail) <= 80,
            "{} 列：{fail}",
            menu::display_width(fail)
        );
        assert_eq!(pauses(&p.asked), 1, "{:?}", p.asked);
    }

    #[test]
    fn menu_import_unparsable_paste_says_what_to_do_in_menu_terms() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        // 两行一起贴：不支持的 ss:// + 订阅地址（单独一行才会被当成订阅）；失败要停，再给一个回车
        let mut p = Scripted::from([
            "3",
            "ss://secret@h:1#x",
            "https://panel.example.com/api/sub/alice",
            "",
            "",
            "0",
        ]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        let at = t
            .lines()
            .position(|l| l.starts_with("  失败："))
            .unwrap_or_else(|| panic!("{t}"));
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(
            lines[at..at + 2],
            [
                "  失败：2 行都不是 hysteria2:// 或 vless:// 链接（ss://、https://）",
                "  订阅地址请单独粘贴一行再回车",
            ],
            "\n{t}"
        );
        assert!(!t.contains("bui-c import"), "菜单里不提命令行：{t}");
        assert!(!t.contains("菜单 [3]"), "人就在菜单 [3] 里：{t}");
        assert!(!t.contains("节点："), "不出现双冒号：{t}");
        assert!(!t.contains("secret"), "{t}");
        // 失败先停，摘要进「上次：」行
        assert_eq!(pauses(&p.asked), 1, "{:?}", p.asked);
        assert!(
            t.lines().any(|l| l
                == "  上次：失败：2 行都不是 hysteria2:// 或 vless:// 链接（ss://、https://）"),
            "{t}"
        );

        // 没有 http(s) 行：只有第一行
        let mut p = Scripted::from(["3", "trojan://secret@h:2#y", "", "", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert!(
            t.lines()
                .any(|l| l == "  失败：1 行都不是 hysteria2:// 或 vless:// 链接（trojan://）"),
            "{t}"
        );
        assert!(!t.contains("订阅地址"), "{t}");
        assert_eq!(pauses(&p.asked), 1, "{:?}", p.asked);
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
        // 答 y 用切换的结果：一行，不停
        assert_eq!(pauses(&p.asked), 0, "{:?}", p.asked);
        assert!(
            t.lines()
                .any(|l| l == "  上次：已切到 alice-reality-direct"),
            "{t}"
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
        // 答否：提问处已经看过导入结果，不再停，0 由主菜单读走
        assert_eq!(pauses(&p.asked), 0, "{:?}", p.asked);
        assert_eq!(t.matches("B-UI 客户端").count(), 2, "{t}");
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
        // v3 面板没有 /api/nodes 是意料之中的：不报状态码、不带 URL，免得像出了故障
        assert!(
            t.lines()
                .any(|l| l == "  这个面板还没有节点接口，改用订阅地址导入"),
            "要说明为什么改走订阅：\n{t}"
        );
        assert!(!t.contains("HTTP 404"), "{t}");
        assert!(!t.contains("面板接口取不到"), "{t}");
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
        // 退回订阅的说明 + 导入结果是两行，本该停；追问切换时人已经看过了，答否不再停
        assert_eq!(pauses(&p.asked), 0, "{:?}", p.asked);
        assert_eq!(t.matches("B-UI 客户端").count(), 2, "0 由主菜单读走：\n{t}");
        assert!(
            t.lines()
                .any(|l| l == "  上次：这个面板还没有节点接口，改用订阅地址导入"),
            "{t}"
        );
    }

    /// `/api/sub` 退回订阅时按原因给不同的话：401/404 是「没有节点接口」（v3 面板），
    /// 别的网络错误带上去掉 URL 的简短原因，面板的业务错误（节点列表为空）不退回、直接报。
    #[test]
    fn menu_import_api_sub_fallback_wording_depends_on_the_cause() {
        let pp = paths();
        // 退回订阅的三种会追问切换，答 n、不停；面板说节点列表为空是失败，停下来等回车
        let run = |reply: FakeReply, answer: &str| {
            let s = FakeSys::new();
            ready(&s);
            profiles_socks().save(&s, &pp).unwrap();
            let n = FakeNet::new();
            n.route("https://panel.example.com/api/nodes/alice", reply);
            n.route("https://panel.example.com/api/sub/alice", b64(BOB_REALITY));
            let url = "https://panel.example.com/api/sub/alice";
            let r = run_menu(&s, &n, &pp, &["3", url, "", answer, "0"], true);
            (r.t, n.log(), names(&s, &pp).len(), pauses(&r.asked))
        };

        let (t, _, count, stops) = run(FakeReply::Status(401), "n");
        assert_eq!(stops, 0, "{t}");
        assert!(
            t.lines()
                .any(|l| l == "  这个面板还没有节点接口，改用订阅地址导入"),
            "\n{t}"
        );
        assert_eq!(count, 2, "{t}");

        // 真机上 reqwest 的错误文字自带完整 URL（末段是用户名，等价凭据）
        let (t, _, count, stops) = run(
            FakeReply::Fail(
                "error sending request for url (https://panel.example.com/api/nodes/alice)".into(),
            ),
            "n",
        );
        assert_eq!(stops, 0, "{t}");
        assert!(
            t.lines()
                .any(|l| l == "  面板接口取不到（error sending request），改用订阅地址导入"),
            "\n{t}"
        );
        for l in t.lines().filter(|l| l.contains("面板接口取不到")) {
            assert!(
                !l.contains("alice") && !l.contains("://"),
                "简短原因不带 URL：{l}"
            );
        }
        assert_eq!(count, 2, "{t}");

        // 200 但不是节点列表（面板把未知路径兜底成网页）：也退回订阅
        let (t, _, count, stops) = run(FakeReply::Text("<html></html>".into()), "n");
        assert_eq!(stops, 0, "{t}");
        assert!(
            t.lines()
                .any(|l| l == "  面板接口取不到（返回的不是节点列表），改用订阅地址导入"),
            "\n{t}"
        );
        assert_eq!(count, 2, "{t}");

        // 面板接口在、但这个用户没有节点：退回订阅也拿不到正经东西，直接报
        let (t, log, count, stops) = run(nodes_payload("alice", vec![]), "");
        assert_eq!(stops, 1, "失败先停：{t}");
        assert!(t.lines().any(|l| l.starts_with("  上次：失败：")), "{t}");
        assert!(
            t.lines()
                .any(|l| l == "  失败：面板返回的节点列表为空：该用户可能没有任何权益"),
            "\n{t}"
        );
        assert!(!t.contains("改用订阅地址导入"), "{t}");
        assert!(!log.iter().any(|l| l.contains("/api/sub/")), "{log:?}");
        assert_eq!(count, 1, "{t}");
    }

    #[test]
    fn menu_import_only_api_sub_falls_back() {
        // /api/clash 返回 YAML，没法当订阅解析：面板取不到就直接报失败
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        let mut p = Scripted::from([
            "3",
            "https://panel.example.com/api/clash/alice",
            "",
            "",
            "0",
        ]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert!(t.lines().any(|l| l.starts_with("  失败：")), "{t}");
        assert!(!t.contains("改用订阅地址导入"), "{t}");
        assert_eq!(n.log().len(), 1, "只试了面板接口：{:?}", n.log());
        assert_eq!(pauses(&p.asked), 1, "失败先停：{:?}", p.asked);
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
        assert_eq!(pauses(&p.asked), 0, "答否不停：{:?}", p.asked);
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
        assert_eq!(
            pauses(&p.asked),
            0,
            "答 y：切换只打一行，不停：{:?}",
            p.asked
        );
        assert!(t.lines().any(|l| l.starts_with("  上次：已切到 ")), "{t}");
    }

    #[test]
    fn menu_import_first_nodes_are_activated_without_asking() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s); // 没有 profiles.json，也没有 v3 目录
        let n = FakeNet::new();
        // 没有追问、也不停：粘贴 → 空行 → 0 直接退出。追问或停顿都会把 0 吞掉，而队列耗尽后
        // 菜单照样退出、屏数也一样，所以看 p.asked：既没有「切换到新导入的…」，也没有「回车返回菜单」
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
        // 首次导入固定多打一行「当前节点：X」：状态区本来就显示当前节点，不算附加行，不停
        assert_eq!(pauses(&p.asked), 0, "{:?}", p.asked);
        assert!(
            t.lines().any(|l| l == "  上次：导入 1 个新节点，共 1 个"),
            "{t}"
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

    const JOURNAL_50: &str = "journalctl -u bui-c.service -n 50 --no-pager -o short-iso";
    const JOURNAL_10: &str = "journalctl -u bui-c.service -n 10 --no-pager -o short-iso";

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
        let lines: Vec<&str> = t.lines().collect();
        let at = lines
            .iter()
            .position(|l| *l == "  服务控制")
            .unwrap_or_else(|| panic!("缺标题：\n{t}"));
        assert_eq!(
            lines[at - 1..at + 4],
            [
                "",
                "  服务控制",
                "     [1] 重启 bui-c.service",
                "     [2] 最近 50 行日志",
                "     [0] 返回",
            ],
            "样式与主菜单一致：\n{t}"
        );
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
    fn menu_service_submenu_asks_again_after_junk() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        with_unit(&s);
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        // x 输错 → 留在子菜单 → 1 重启 → 回主菜单 → 0 退出
        let mut p = Scripted::from(["4", "x", "1", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert!(t.lines().any(|l| l == "  无效选项：x"), "{t}");
        assert!(
            s.called("systemctl restart bui-c.service"),
            "1 是子菜单里的重启：\n{t}"
        );
        assert_eq!(
            p.asked.iter().filter(|q| *q == "选择 [0-2]").count(),
            2,
            "{:?}",
            p.asked
        );
        assert_eq!(
            t.matches("最近 50 行日志").count(),
            1,
            "重问不重画子菜单：\n{t}"
        );
        assert_eq!(t.matches("B-UI 客户端").count(), 2, "{t}");
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
            !p.asked.iter().any(|q| q == "选择节点编号"),
            "1 不能漏到主菜单去切节点：{:?}\n{t}",
            p.asked
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
            "2026-09-13T10:15:30+08:00 baiyi sing-box[4242]: \u{1b}[31mFATAL\u{1b}[0m[0000] start service: open tun: operation not permitted\n",
        );
        crate::testutil::profiles_tun().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        // 4 → 1 重启 → 接口没起来、带出日志：停下来等回车 → 0 退出
        let mut p = Scripted::from(["4", "1", "", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        let lines: Vec<&str> = t.lines().collect();
        let at = lines
            .iter()
            .position(|l| *l == "  已重启 bui-c.service，但 bui-tun 接口 5 秒内没起来")
            .unwrap_or_else(|| panic!("{t}"));
        assert_eq!(
            lines[at + 1..at + 4],
            [
                "",
                "  ── bui-c.service 最近 10 行日志 ──",
                "  2026-09-13T10:15:30+08:00 FATAL[0000] start service: open tun: operation not permitted",
            ],
            "空行 + 标题行，日志缩进 2 列、去掉主机名与 ident、去掉颜色：\n{t}"
        );
        assert!(!t.contains('\u{1b}'), "菜单约定无 ANSI");
        assert_eq!(
            s.sleeps().len(),
            crate::engine::APPLY_POLL_STEPS as usize,
            "与 apply 同一段轮询：10 × 500ms"
        );
        assert_eq!(
            p.asked,
            vec!["选择 [0-9]", "选择 [0-2]", "回车返回菜单", "选择 [0-9]"],
            "接口没起来、还带出了日志：先停，看完回车再清屏回主菜单"
        );
        assert!(
            t.lines()
                .any(|l| l == "  上次：已重启 bui-c.service，但 bui-tun 接口 5 秒内没起来"),
            "{t}"
        );
    }

    #[test]
    fn menu_service_logs_have_a_title_compact_lines_and_wait_for_enter() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        with_unit(&s);
        s.reply(
            JOURNAL_50,
            0,
            "2026-09-13T10:15:30+08:00 baiyi sing-box[4242]: \u{1b}[36mINFO\u{1b}[0m[0000] sing-box started (0.12s)\n\
             2026-09-13T10:15:31+08:00 baiyi sing-box[4242]: \u{1b}[33mWARN\u{1b}[0m[0001] inbound/mixed[mixed-in]: 127.0.0.1:1080\n\
             -- Boot 0123456789abcdef --\n",
        );
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        // 4 → 2 看日志 → 回车 → 0 退出
        let mut p = Scripted::from(["4", "2", "", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        assert!(s.called(JOURNAL_50), "{:?}", s.calls());
        assert!(!s.called("systemctl restart bui-c.service"));
        let t = ctx.transcript.clone();
        let lines: Vec<&str> = t.lines().collect();
        let at = lines
            .iter()
            .position(|l| *l == "  ── bui-c.service 最近 50 行日志 ──")
            .unwrap_or_else(|| panic!("缺标题行：\n{t}"));
        assert_eq!(lines[at - 1], "", "标题前空一行：\n{t}");
        assert_eq!(
            lines[at + 1..at + 4],
            [
                "  2026-09-13T10:15:30+08:00 INFO[0000] sing-box started (0.12s)",
                "  2026-09-13T10:15:31+08:00 WARN[0001] inbound/mixed[mixed-in]: 127.0.0.1:1080",
                "  -- Boot 0123456789abcdef --",
            ],
            "每行 <时间> <消息>，缩进 2 列；解析不了的行原样保留：\n{t}"
        );
        assert!(!t.contains("baiyi"), "去掉主机名：{t}");
        assert!(!t.contains("sing-box[4242]"), "去掉 ident[pid]：{t}");
        assert!(!t.contains('\u{1b}'), "菜单约定无 ANSI：{t:?}");
        assert_eq!(
            p.asked,
            vec!["选择 [0-9]", "选择 [0-2]", "回车返回菜单", "选择 [0-9]"],
            "打完日志停下来等回车，0 由主菜单读走"
        );
        assert_eq!(t.matches("B-UI 客户端").count(), 2, "{t}");
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
        let mut p = Scripted::from(["4", "2", "", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert!(
            t.lines()
                .any(|l| l.starts_with("  读不到 bui-c.service 的日志")),
            "{t}"
        );
        assert!(!t.contains("失败："), "不当成菜单操作失败：{t}");
        assert!(
            p.asked.iter().any(|q| q == "回车返回菜单"),
            "读不到也停一下，别让菜单重画把原因顶上去：{:?}",
            p.asked
        );
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
        let t = ctx.transcript.clone();
        // 新机器上 `bui-c update` 走不通（没单元可重启），装引擎与单元的路是导入节点
        assert!(
            t.lines()
                .any(|l| l == "  还没有安装引擎与单元：先用 [3] 导入节点"),
            "单元不存在时应引导先导入节点：\n{t}"
        );
        assert!(!t.contains("bui-c update"), "{t}");
        assert!(
            !t.contains("v3 客户端用 [7]"),
            "没有 v3 目录就不提 [7]：\n{t}"
        );
        assert!(!t.contains("最近 50 行日志"), "没有单元就不进子菜单：\n{t}");
        assert!(!s.called("systemctl restart bui-c.service"));

        // 机器上有 v3 客户端：补一句 [7]
        s.put(
            "/opt/hysteria-client/configs/hysteria2-1/uri.txt",
            "hysteria2://alice:hy2-pw@panel.example.com:10000/?sni=panel.example.com#a",
        );
        let mut p = Scripted::from(["4", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert!(
            t.lines()
                .any(|l| l
                    == "  还没有安装引擎与单元：先用 [3] 导入节点（v3 客户端用 [7] 从 v3 导入）"),
            "\n{t}"
        );
    }

    // ───────────── T7：删除节点与 `bui-c delete`（spec §5） ─────────────

    const ACTIVE: &str = "hysteria2-1778329470";
    const RICK_REALITY: &str = "rick-node.example-a.net-reality-direct";

    /// 装好的机器 + baiyi 形态的 9 个节点，**并且已经 apply 过一次**：`config.json` 就是当前
    /// 节点渲出来的那一份，回滚时的逐字节比对才有意义。返回落盘的那份 profiles。
    fn nine_nodes(s: &FakeSys, pp: &Paths, mode: Mode) -> Profiles {
        ready(s);
        if mode == Mode::Tun {
            s.reply("ip link show bui-tun", 0, "5: bui-tun");
        }
        let mut prof = crate::testutil::baiyi_like();
        prof.mode = mode;
        prof.save(s, pp).unwrap();
        Engine::new(s, pp).apply(&prof).unwrap();
        prof
    }

    fn count_calls(s: &FakeSys, cmd: &str) -> usize {
        s.calls().iter().filter(|c| *c == cmd).count()
    }

    fn restarts(s: &FakeSys) -> usize {
        count_calls(s, "systemctl restart bui-c.service")
    }

    /// 直接跑执行编排（三个入口共用的那一段），不经菜单也不提问。
    fn del(
        s: &FakeSys,
        n: &FakeNet,
        pp: &Paths,
        names: &[&str],
        switch_to: Option<&str>,
        seen: &delete::Snapshot,
    ) -> (Result<delete::Report>, String) {
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(s, n, pp, &mut p, false, false);
        let names: Vec<String> = names.iter().map(|x| x.to_string()).collect();
        let r = delete_nodes(&mut ctx, &names, switch_to, seen);
        (r, ctx.transcript.clone())
    }

    /// 跑一遍 `[6]` 删除页；返回 [`Outcome`]、屏上的文字与被问过的提示。
    fn run_delete_menu(
        s: &FakeSys,
        n: &FakeNet,
        pp: &Paths,
        inputs: &[&str],
    ) -> (Outcome, String, Vec<String>) {
        let mut p = Scripted {
            queue: inputs.iter().map(|x| x.to_string()).collect(),
            asked: Vec::new(),
            tty: true,
        };
        let mut ctx = Ctx::new(s, n, pp, &mut p, false, false);
        let out = delete_menu(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert!(!t.contains('\u{1b}'), "transcript 不能含 ESC：{t:?}");
        (out, t, p.asked)
    }

    #[test]
    fn deleting_a_passive_node_does_not_apply_or_restart() {
        let pp = paths();
        let s = FakeSys::new();
        let n = FakeNet::new();
        let prof = nine_nodes(&s, &pp, Mode::Socks);
        let before = restarts(&s);
        let config = s.get("/opt/bui-c/config.json").unwrap();
        let checks = s.calls().iter().filter(|c| c.contains("check -c")).count();
        let seen = delete::snapshot(&prof);
        let (r, _) = del(&s, &n, &pp, &["HY2"], None, &seen);
        assert_eq!(
            r.unwrap(),
            delete::Report {
                deleted: vec!["HY2".to_string()],
                active: Some(ACTIVE.to_string()),
                switched: false,
                stopped: false,
                remaining: 8,
            }
        );
        assert_eq!(restarts(&s), before, "不 apply、不重启");
        assert_eq!(
            s.calls().iter().filter(|c| c.contains("check -c")).count(),
            checks,
            "Passive 形态不预检：{:?}",
            s.calls()
        );
        assert_eq!(
            s.get("/opt/bui-c/config.json").unwrap(),
            config,
            "config.json 里只有活动节点，删别的对数据面没有影响"
        );
        let left = names(&s, &pp);
        assert_eq!(left.len(), 8);
        assert!(!left.contains(&"HY2".to_string()));
        assert_eq!(
            Profiles::load(&s, &pp).unwrap().active.as_deref(),
            Some(ACTIVE)
        );
        assert!(
            !s.exists(std::path::Path::new("/opt/bui-c/profiles.json.bak")),
            "不写 .bak：盘上不留第二份被删节点的凭据（spec §5.8、§12.1）"
        );
    }

    /// Passive 形态写盘失败：数据面没动过，但也要给停顿页与安抚行，不能把一句原始 IO 错误
    /// 裸抛出去（spec §5.5 Passive 那一行；裸 `?` 还会从 `delete_menu` 冒出去把人踢回 shell）。
    #[test]
    fn a_passive_delete_that_cannot_save_gets_a_page_not_a_raw_error() {
        let pp = paths();
        let s = FakeSys::new();
        let n = FakeNet::new();
        let prof = nine_nodes(&s, &pp, Mode::Socks);
        let before = restarts(&s);
        s.fail_write("/opt/bui-c/profiles.json");
        let seen = delete::snapshot(&prof);
        let (r, t) = del(&s, &n, &pp, &["HY2"], None, &seen);
        assert_eq!(r.unwrap_err().to_string(), delete::SAVE_FAILED);
        assert!(t.contains(delete::SAVE_FAILED), "{t}");
        assert!(t.contains("permission denied"), "要报出是哪一步失败：{t}");
        assert!(t.contains(delete::STILL_THERE), "什么都没变：{t}");
        assert_eq!(restarts(&s), before, "数据面一点都没动");
        assert_eq!(names(&s, &pp).len(), 9);
    }

    #[test]
    fn deleting_the_active_node_applies_the_replacement_then_saves() {
        let pp = paths();
        let s = FakeSys::new();
        let n = FakeNet::new();
        let prof = nine_nodes(&s, &pp, Mode::Socks);
        // 上一轮崩在半路留下的两种临时文件（0600 的完整配置，含凭据）：预检要清掉
        s.put("/opt/bui-c/.config.json.new", "残留");
        s.put("/opt/bui-c/.config.json.777.tmp", "残留");
        let writes = s.writes("/opt/bui-c/profiles.json");
        let seen = delete::snapshot(&prof);
        let (r, _) = del(&s, &n, &pp, &[ACTIVE], None, &seen);
        let r = r.unwrap();
        assert!(r.switched && !r.stopped);
        assert_eq!(r.active.as_deref(), Some(RICK_REALITY));
        assert_eq!(r.remaining, 8);
        assert_eq!(r.deleted, vec![ACTIVE.to_string()]);
        // 第 13 条：profiles 与 active 同一次 save
        assert_eq!(
            s.writes("/opt/bui-c/profiles.json") - writes,
            1,
            "只写一次 profiles.json"
        );
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(saved.active.as_deref(), Some(RICK_REALITY));
        assert_eq!(saved.profiles.len(), 8);
        assert!(!saved.profiles.iter().any(|p| p.name == ACTIVE));
        // 先预检、再动数据面
        let calls = s.calls();
        let at = |c: &str| {
            calls
                .iter()
                .rposition(|x| x.contains(c))
                .unwrap_or_else(|| panic!("没有调用 {c}：{calls:?}"))
        };
        assert!(at("check -c") < at("restart bui-c.service"));
        assert!(s
            .get("/opt/bui-c/config.json")
            .unwrap()
            .contains("rick-node.example-a.net"));
        assert!(
            !s.exists(std::path::Path::new("/opt/bui-c/.config.json.new")),
            "校验用的临时文件要删掉"
        );
        assert!(
            !s.exists(std::path::Path::new("/opt/bui-c/.config.json.777.tmp")),
            "残留的临时文件要清掉（R10）"
        );
    }

    #[test]
    fn deleting_the_active_node_rolls_back_when_apply_fails() {
        let pp = paths();
        let s = FakeSys::new();
        let n = FakeNet::new();
        let prof = nine_nodes(&s, &pp, Mode::Socks);
        let config = s.get("/opt/bui-c/config.json").unwrap();
        let before = restarts(&s);
        // 写 config.json 失败（磁盘满、只读文件系统）
        s.fail_write("/opt/bui-c/config.json");
        let seen = delete::snapshot(&prof);
        let (r, t) = del(&s, &n, &pp, &[ACTIVE], None, &seen);
        assert_eq!(
            r.unwrap_err().to_string(),
            format!("删除没做：切到 {RICK_REALITY} 失败")
        );
        assert!(t.contains("删除没做：切到"), "{t}");
        assert!(t.contains(delete::ROLLED_BACK), "{t}");
        assert_eq!(
            s.get("/opt/bui-c/config.json").unwrap(),
            config,
            "回滚之后还是原来的配置"
        );
        assert_eq!(
            restarts(&s),
            before,
            "配置没写进去，回滚这一次逐字节比对下来什么都不改、也不重启"
        );
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(saved.profiles.len(), 9, "一个都没删");
        assert_eq!(saved.active.as_deref(), Some(ACTIVE));
    }

    #[test]
    fn deleting_the_active_node_rolls_back_when_tun_is_not_ready() {
        let pp = paths();
        let s = FakeSys::new();
        let n = FakeNet::new();
        let prof = nine_nodes(&s, &pp, Mode::Tun);
        let config = s.get("/opt/bui-c/config.json").unwrap();
        let before = restarts(&s);
        // 换过去之后接口再也起不来：TUN 5 秒没就绪也算失败（R10）
        s.reply("ip link show bui-tun", 1, "");
        let seen = delete::snapshot(&prof);
        let (r, t) = del(&s, &n, &pp, &[ACTIVE], None, &seen);
        assert_eq!(
            r.unwrap_err().to_string(),
            format!("删除没做：切到 {RICK_REALITY} 失败")
        );
        assert!(t.contains("bui-tun 接口 5 秒内没起来"), "{t}");
        // 旧配置的 TUN 也没起来：R10 对 apply 的判据（Err 或 tun_ready == Some(false)）
        // 在回滚方向一样算数，不能在断网状态下报「节点都还在」了事
        assert!(t.contains(delete::ROLLED_BACK_TUN_DOWN), "{t}");
        assert!(
            !t.contains(delete::ROLLED_BACK),
            "TUN 没起来就不能说一切正常：{t}"
        );
        assert!(t.contains(delete::ROLLBACK_NEXT), "要给出路：{t}");
        assert_eq!(
            s.get("/opt/bui-c/config.json").unwrap(),
            config,
            "换回原来的配置"
        );
        assert_eq!(restarts(&s) - before, 2, "切过去一次、换回来一次");
        assert_eq!(names(&s, &pp).len(), 9);
        assert_eq!(
            Profiles::load(&s, &pp).unwrap().active.as_deref(),
            Some(ACTIVE)
        );
    }

    #[test]
    fn deleting_the_active_node_rolls_back_when_saving_profiles_fails() {
        let pp = paths();
        let s = FakeSys::new();
        let n = FakeNet::new();
        let prof = nine_nodes(&s, &pp, Mode::Socks);
        let config = s.get("/opt/bui-c/config.json").unwrap();
        s.fail_write("/opt/bui-c/profiles.json");
        let seen = delete::snapshot(&prof);
        let (r, t) = del(&s, &n, &pp, &[ACTIVE], None, &seen);
        assert_eq!(
            r.unwrap_err().to_string(),
            "删除没做：写 profiles.json 失败"
        );
        assert!(
            t.contains("permission denied") || t.contains("profiles.json"),
            "{t}"
        );
        assert!(t.contains(delete::ROLLED_BACK), "{t}");
        assert_eq!(
            s.get("/opt/bui-c/config.json").unwrap(),
            config,
            "写盘失败立刻换回原配置（R10）"
        );
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(saved.profiles.len(), 9);
        assert_eq!(saved.active.as_deref(), Some(ACTIVE));
    }

    /// 最坏那条路：restart 在前向与回滚两次都失败。此前 `ROLLBACK_FAILED` / `ROLLBACK_NEXT`
    /// 只在宽度守门表里出现过，一条行为测试都没有（§12.1 的 T7 行点名 restart 回 1 这条路）。
    #[test]
    fn a_rollback_that_also_fails_says_so_and_offers_a_way_out() {
        let pp = paths();
        let s = FakeSys::new();
        let n = FakeNet::new();
        let prof = nine_nodes(&s, &pp, Mode::Socks);
        let config = s.get("/opt/bui-c/config.json").unwrap();
        let before = restarts(&s);
        // 写 config 之后才失败：前向 apply 写了新配置、restart 回 1；回滚把旧配置写回去，
        // restart 又回 1
        s.reply("systemctl restart bui-c.service", 1, "Job failed");
        let seen = delete::snapshot(&prof);
        let (r, t) = del(&s, &n, &pp, &[ACTIVE], None, &seen);
        assert_eq!(
            r.unwrap_err().to_string(),
            format!("删除没做：切到 {RICK_REALITY} 失败")
        );
        assert!(t.contains(delete::ROLLBACK_FAILED), "{t}");
        assert!(t.contains(delete::ROLLBACK_NEXT), "{t}");
        assert!(
            !t.contains(delete::ROLLED_BACK),
            "换回也失败，别说换回好了：{t}"
        );
        assert_eq!(
            s.get("/opt/bui-c/config.json").unwrap(),
            config,
            "config.json 还是回到旧配置（§12.1 的 T7 行）"
        );
        assert_eq!(restarts(&s) - before, 2, "切过去一次、换回来一次");
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(saved.profiles.len(), 9, "一个都没删");
        assert_eq!(saved.active.as_deref(), Some(ACTIVE));
    }

    #[test]
    fn deleting_everything_tears_down_the_main_unit_and_keeps_the_timer() {
        let pp = paths();
        let s = FakeSys::new();
        let n = FakeNet::new();
        let prof = nine_nodes(&s, &pp, Mode::Tun);
        let mut rt = Runtime::load(&s, &pp);
        rt.ufw_rules = true;
        rt.fail_streak = 3;
        rt.last_restart_at = Some(1_760_000_000);
        rt.save(&s, &pp).unwrap();
        s.reply("ufw status", 0, "Status: active\n");
        // stop 之后就不 active 了（复查通过，进不可回头段）
        s.reply("systemctl is-active --quiet bui-c.service", 3, "");
        let all: Vec<&str> = prof.profiles.iter().map(|p| p.name.as_str()).collect();
        let seen = delete::snapshot(&prof);
        let (r, _) = del(&s, &n, &pp, &all, None, &seen);
        let r = r.unwrap();
        assert!(r.stopped && !r.switched);
        assert_eq!(r.active, None);
        assert_eq!(r.remaining, 0);
        assert_eq!(r.deleted.len(), 9);
        for c in [
            "systemctl stop bui-c.service",
            "systemctl disable bui-c.service",
            "systemctl reset-failed bui-c.service",
            "systemctl daemon-reload",
            "ip link delete bui-tun",
            "ufw delete allow in on bui-tun",
        ] {
            assert!(s.called(c), "缺 {c}：{:?}", s.calls());
        }
        assert!(!s.exists(&pp.unit(UNIT_MAIN)), "主单元文件删掉");
        assert!(s.exists(&pp.unit(UNIT_TIMER)), "timer 留着（R10）");
        assert!(!s.called("systemctl stop bui-c.timer"));
        assert!(!s.called("systemctl disable bui-c.timer"));
        assert!(!s.exists(&pp.config()));
        assert!(s.exists(&pp.singbox()), "内核留着");
        let saved = Profiles::load(&s, &pp).unwrap();
        assert!(saved.profiles.is_empty() && saved.active.is_none());
        assert_eq!(saved.mode, Mode::Tun, "模式与端口都保留");
        assert_eq!(saved.socks_port, prof.socks_port);
        assert_eq!(saved.auto_update, prof.auto_update);
        let rt = Runtime::load(&s, &pp);
        assert!(!rt.ufw_rules, "撤掉了就记下来");
        assert_eq!(rt.fail_streak, 0);
        assert_eq!(rt.last_restart_at, None);
    }

    #[test]
    fn deleting_everything_aborts_without_touching_profiles_if_the_service_will_not_stop() {
        let pp = paths();
        let s = FakeSys::new();
        let n = FakeNet::new();
        // ready() 把 is-active 登记成 0：stop 两次也停不下来
        let prof = nine_nodes(&s, &pp, Mode::Socks);
        let reloads = count_calls(&s, "systemctl daemon-reload");
        let all: Vec<&str> = prof.profiles.iter().map(|p| p.name.as_str()).collect();
        let seen = delete::snapshot(&prof);
        let (r, t) = del(&s, &n, &pp, &all, None, &seen);
        let e = r.unwrap_err();
        // 「上次：」行 40 列只有 31 列：长句会被砍掉「还在跑」，所以摘要单配短式（R6）
        assert_eq!(e.to_string(), delete::TEARDOWN_FAILED_SHORT);
        assert!(t.contains("停止代理失败"), "页上仍是完整的原因：{t}");
        assert!(t.contains(delete::STILL_THERE), "{t}");
        assert_eq!(names(&s, &pp).len(), 9, "profiles.json 一个字都不改");
        assert!(s.exists(&pp.unit(UNIT_MAIN)) && s.exists(&pp.config()));
        assert_eq!(
            count_calls(&s, "systemctl stop bui-c.service"),
            2,
            "复查没过就再停一次，然后中止"
        );
        assert_eq!(
            count_calls(&s, "systemctl daemon-reload"),
            reloads,
            "不可回头段一步都没走"
        );
        assert!(!s.called("systemctl disable bui-c.service"));
    }

    /// 删光时数据面拆完了、`profiles.json` 却写不进去：这时**不能**说「节点都还在」（条目在、
    /// 代理不在），要说清现状与下一步；并且 R10 把撤 UFW、写 runtime 排在写 profiles **之后**
    /// 尽力而为，所以这一步失败时这两件根本还没做——UFW 规则与重启记账都得原样留着。
    #[test]
    fn deleting_everything_but_failing_to_save_leaves_the_ufw_rule_and_the_runtime_alone() {
        let pp = paths();
        let s = FakeSys::new();
        let n = FakeNet::new();
        let prof = nine_nodes(&s, &pp, Mode::Tun);
        let mut rt = Runtime::load(&s, &pp);
        rt.ufw_rules = true;
        rt.fail_streak = 3;
        rt.save(&s, &pp).unwrap();
        s.reply("ufw status", 0, "Status: active\n");
        s.reply("systemctl is-active --quiet bui-c.service", 3, "");
        s.fail_write("/opt/bui-c/profiles.json");
        let all: Vec<&str> = prof.profiles.iter().map(|p| p.name.as_str()).collect();
        let seen = delete::snapshot(&prof);
        let (r, t) = del(&s, &n, &pp, &all, None, &seen);
        assert_eq!(r.unwrap_err().to_string(), delete::STOPPED_NOT_SAVED_SHORT);
        assert!(t.contains(delete::STOPPED_NOT_SAVED_HEAD), "{t}");
        assert!(t.contains("permission denied"), "报出是哪一步失败：{t}");
        assert!(t.contains(delete::STOPPED_NOT_SAVED), "要给下一步：{t}");
        assert!(
            !t.contains(delete::STILL_THERE),
            "条目在、代理不在：「节点都还在」在这里是错的：{t}"
        );
        assert!(!s.exists(&pp.unit(UNIT_MAIN)), "不可回头段已经走完");
        assert_eq!(names(&s, &pp).len(), 9, "profiles 没写成，条目还列着");
        assert!(
            !s.called("ufw delete allow in on bui-tun"),
            "撤 UFW 排在写 profiles 之后（R10），这一步失败时还没轮到：{:?}",
            s.calls()
        );
        let rt = Runtime::load(&s, &pp);
        assert!(rt.ufw_rules, "规则原样留着");
        assert_eq!(rt.fail_streak, 3, "重启记账也还没清零（R10 的顺序）");
    }

    #[test]
    fn ufw_failure_after_teardown_is_best_effort() {
        let pp = paths();
        let s = FakeSys::new();
        let n = FakeNet::new();
        let prof = nine_nodes(&s, &pp, Mode::Tun);
        let mut rt = Runtime::load(&s, &pp);
        rt.ufw_rules = true;
        rt.save(&s, &pp).unwrap();
        s.reply("ufw status", 0, "Status: active\n");
        s.reply("ufw delete allow in on bui-tun", 1, "ERROR: 无效的规则");
        s.reply("systemctl is-active --quiet bui-c.service", 3, "");
        let all: Vec<&str> = prof.profiles.iter().map(|p| p.name.as_str()).collect();
        let seen = delete::snapshot(&prof);
        let (r, t) = del(&s, &n, &pp, &all, None, &seen);
        assert!(r.unwrap().stopped, "撤 UFW 失败不影响删除");
        assert!(t.contains(delete::UFW_LEFT), "{t}");
        assert!(Profiles::load(&s, &pp).unwrap().profiles.is_empty());
        assert!(!s.exists(&pp.unit(UNIT_MAIN)));
        assert!(
            Runtime::load(&s, &pp).ufw_rules,
            "规则没撤掉就留着 true，等下次切 SOCKS 或收敛时再撤（R10）"
        );
    }

    #[test]
    fn a_changed_snapshot_aborts_the_delete() {
        let pp = paths();
        let s = FakeSys::new();
        let n = FakeNet::new();
        let prof = nine_nodes(&s, &pp, Mode::Socks);
        let seen = delete::snapshot(&prof);
        // 另一个会话删掉了最后一个节点
        let mut other = prof.clone();
        other.profiles.pop();
        other.save(&s, &pp).unwrap();
        let (r, t) = del(&s, &n, &pp, &["HY2"], None, &seen);
        // 页上是完整的长句，「上次：」行拿短式（40 列只有 31 列，长句会丢掉「请重新选」）
        assert_eq!(r.unwrap_err().to_string(), delete::SNAPSHOT_CHANGED_SHORT);
        assert!(t.contains(delete::SNAPSHOT_CHANGED), "{t}");
        assert!(t.contains("请重新选"), "{t}");
        assert_eq!(names(&s, &pp).len(), 8, "这次什么都没删");
        // 只是活动节点换了也算「被别处改过」
        let mut moved = prof.clone();
        moved.active = Some("HY2".to_string());
        moved.save(&s, &pp).unwrap();
        let (r, _) = del(&s, &n, &pp, &["reality-Reality"], None, &seen);
        assert_eq!(r.unwrap_err().to_string(), delete::SNAPSHOT_CHANGED_SHORT);
        assert_eq!(names(&s, &pp).len(), 9);
    }

    /// 别的会话刚好把要删的那个节点删掉了：锁外那一步只是判要不要装内核，不能让它先以
    /// 「没有叫 X 的节点」报错——那是把并发说成了输错名字，用户拿不到「请重新选」。
    #[test]
    fn a_node_another_session_removed_reports_the_snapshot_not_a_bad_name() {
        let pp = paths();
        let s = FakeSys::new();
        let n = FakeNet::new();
        let prof = nine_nodes(&s, &pp, Mode::Socks);
        let seen = delete::snapshot(&prof);
        let mut other = prof.clone();
        other.profiles.retain(|p| p.name != "HY2");
        other.save(&s, &pp).unwrap();
        let (r, t) = del(&s, &n, &pp, &["HY2"], None, &seen);
        assert_eq!(r.unwrap_err().to_string(), delete::SNAPSHOT_CHANGED_SHORT);
        assert!(t.contains("请重新选"), "{t}");
        assert!(!t.contains("没有叫"), "不是输错名字：{t}");
        assert_eq!(names(&s, &pp).len(), 8, "这次什么都没删");
    }

    /// 预检失败的停顿页要说清下一步（spec §0.2 R1）。
    #[test]
    fn the_preflight_failure_page_says_what_to_do_next() {
        let pp = paths();
        // ① sing-box check 不通过
        let s = FakeSys::new();
        let n = FakeNet::new();
        let prof = nine_nodes(&s, &pp, Mode::Socks);
        let config = s.get("/opt/bui-c/config.json").unwrap();
        let before = restarts(&s);
        s.reply(
            "/opt/bui-c/bin/sing-box check -c /opt/bui-c/.config.json.new",
            1,
            "outbounds[0]: 解析失败",
        );
        let seen = delete::snapshot(&prof);
        let (r, t) = del(&s, &n, &pp, &[ACTIVE], None, &seen);
        assert_eq!(
            r.unwrap_err().to_string(),
            format!("删除没做：切到 {RICK_REALITY} 失败")
        );
        assert!(t.contains("的配置校验不通过"), "{t}");
        assert!(t.contains("sing-box check 不通过"), "{t}");
        assert!(t.contains(delete::STILL_THERE), "{t}");
        assert!(t.contains(delete::NEXT_PICK_ANOTHER), "{t}");
        assert_eq!(restarts(&s), before, "数据面一点都没动");
        assert_eq!(s.get("/opt/bui-c/config.json").unwrap(), config);
        assert_eq!(names(&s, &pp).len(), 9);
        assert!(!s.exists(std::path::Path::new("/opt/bui-c/.config.json.new")));

        // ② 内核缺失（锁外也装不上：FakeNet 一个 URL 都没登记）
        let s = FakeSys::new();
        let prof = nine_nodes(&s, &pp, Mode::Socks);
        s.remove_file(std::path::Path::new("/opt/bui-c/bin/sing-box"))
            .unwrap();
        let seen = delete::snapshot(&prof);
        let (r, t) = del(&s, &n, &pp, &[ACTIVE], None, &seen);
        assert!(r.is_err());
        assert!(t.contains("内核缺失"), "{t}");
        assert!(t.contains(delete::NEXT_KERNEL), "{t}");
        assert!(t.contains("[7] 更新与维护 → [1] 检查更新"), "{t}");
        assert!(!t.contains(delete::NEXT_PICK_ANOTHER), "{t}");
        assert_eq!(names(&s, &pp).len(), 9);
    }

    #[test]
    fn menu_delete_of_the_active_node_needs_the_word_yes() {
        let pp = paths();
        let s = FakeSys::new();
        let n = FakeNet::new();
        nine_nodes(&s, &pp, Mode::Socks);
        let before = restarts(&s);
        let mut p = Scripted::from(["2", "y", "2", "yes"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        // 第一次：会断网的删除只输了 y → 取消，并说清要输入什么（R1）
        let first = delete_menu(&mut ctx).unwrap();
        assert_eq!(first, Outcome::Pause(delete::CANCELLED.to_string()));
        assert!(
            ctx.transcript.contains(delete::NEEDS_YES),
            "{}",
            ctx.transcript
        );
        assert_eq!(names(&s, &pp).len(), 9, "一个都没删");
        assert_eq!(restarts(&s), before, "取消了就不该动数据面");
        // 第二次：yes 才真删
        let mark = ctx.transcript.len();
        let second = delete_menu(&mut ctx).unwrap();
        assert_eq!(
            second,
            Outcome::Note(format!("已删 1 个，切到 {RICK_REALITY}"))
        );
        assert!(
            ctx.transcript[mark..].contains("正在切到 rick-node"),
            "{}",
            &ctx.transcript[mark..]
        );
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(saved.profiles.len(), 8);
        assert_eq!(saved.active.as_deref(), Some(RICK_REALITY));
        assert_eq!(restarts(&s) - before, 1, "切到替换节点，重启一次");
        drop(ctx);
        // 提示也要写 yes（R1）：两次问的都是同一句
        let asked: Vec<&String> = p
            .asked
            .iter()
            .filter(|q| q.starts_with("确认删除"))
            .collect();
        assert_eq!(asked.len(), 2, "{:?}", p.asked);
        assert!(asked.iter().all(|q| q.ends_with("[yes/N]")), "{asked:?}");
    }

    /// 第 1 条：`delete_confirm` 的 `rows` 传 `ctx.rows()` 的原值（函数内部自己减 2）。
    #[test]
    fn the_delete_confirm_block_gets_the_raw_row_count() {
        let pp = paths();
        let s = FakeSys::new();
        let n = FakeNet::new();
        // 手机横屏：40 列 × 17 行
        s.set_term_size(Some((40, 17)));
        let prof = nine_nodes(&s, &pp, Mode::Tun);
        let (out, t, _) = run_delete_menu(&s, &n, &pp, &["2", "n"]);
        assert_eq!(out, Outcome::Note(delete::CANCELLED.to_string()));
        let want = menu::delete_confirm(&prof, &[1], Some(3), 40, 17);
        assert!(t.contains(want.body.trim_end()), "{t}");
        assert!(t.contains("可换："), "17 行放不下就压成一行：{t}");
        assert!(!t.contains("想换就输入下面的编号"), "{t}");
        assert_ne!(
            want.body,
            menu::delete_confirm(&prof, &[1], Some(3), 40, 40).body,
            "传 rows − 2 会是另一副样子，这条测试才有意义"
        );
        for l in t.lines() {
            assert!(
                menu::budget_width(l) <= menu::line_limit(40),
                "{l:?} = {}",
                menu::budget_width(l)
            );
        }
    }

    /// 第 5 条：`parse_confirm` 位数溢出时交回 `Pick(len)`，报越界要回显用户的原始输入。
    #[test]
    fn an_out_of_range_replacement_number_echoes_the_input() {
        let pp = paths();
        let s = FakeSys::new();
        let n = FakeNet::new();
        nine_nodes(&s, &pp, Mode::Socks);
        let raw = "99999999999999999999";
        let (out, t, asked) = run_delete_menu(&s, &n, &pp, &["2", "12", raw, "3", ""]);
        assert_eq!(out, Outcome::Note(delete::CANCELLED.to_string()));
        assert!(t.contains("没有编号 12（可选 1-9）"), "{t}");
        let want = menu::SelError::OutOfRange(vec![raw.to_string()]).message(9);
        assert!(t.contains(&want), "要回显原始输入：{want}\n{t}");
        assert!(!t.contains("没有编号 10"), "不能拿 i + 1 拼文案：{t}");
        // 3 不在删除之列：改成替换目标，补打一行「删完切到」
        assert!(t.contains("删完切到 [3] reality-Reality"), "{t}");
        assert_eq!(
            asked.iter().filter(|q| q.starts_with("确认删除")).count(),
            4,
            "错三次、最后答空行：同一个提问问了 4 次 {asked:?}"
        );
        assert_eq!(names(&s, &pp).len(), 9);
    }

    /// 第 7 条：`SelError::message` 本身 ≤ 59 列，40 列终端要折行再打。
    #[test]
    fn a_selection_error_wraps_at_40_columns() {
        let pp = paths();
        let s = FakeSys::new();
        let n = FakeNet::new();
        s.set_term_size(Some((40, 24)));
        nine_nodes(&s, &pp, Mode::Socks);
        let long = "1 ".to_string() + &"9".repeat(40);
        let (out, t, _) = run_delete_menu(&s, &n, &pp, &["1 a 3", "5-3", &long, "0"]);
        assert_eq!(out, Outcome::Nothing, "0 返回主菜单");
        assert!(t.contains("看不懂「a」"), "{t}");
        assert!(t.contains("范围写反了：5-3"), "{t}");
        assert!(t.contains("没有编号"), "{t}");
        for l in t.lines() {
            assert!(
                menu::budget_width(l) <= menu::line_limit(40),
                "{l:?} = {}",
                menu::budget_width(l)
            );
        }
    }

    /// spec §5.3 的输入表：编号在删除之列、不在「删完切到」形态时打编号、空行取消。
    /// Empty 形态的文案要说 yes（第 4 条）。
    #[test]
    fn the_delete_menu_follows_the_confirm_input_table() {
        let pp = paths();
        let n = FakeNet::new();
        // 打的编号也在要删的节点里
        let s = FakeSys::new();
        nine_nodes(&s, &pp, Mode::Socks);
        let (out, t, asked) = run_delete_menu(&s, &n, &pp, &["2 3", "3", ""]);
        assert_eq!(out, Outcome::Note(delete::CANCELLED.to_string()));
        assert!(t.contains(&delete::also_a_target(2)), "{t}");
        assert_eq!(
            asked.iter().filter(|q| q.starts_with("确认删除")).count(),
            2
        );
        assert_eq!(names(&s, &pp).len(), 9);

        // Passive 形态打编号：这一步只认 y
        let s = FakeSys::new();
        nine_nodes(&s, &pp, Mode::Socks);
        let (out, t, asked) = run_delete_menu(&s, &n, &pp, &["1", "4"]);
        assert_eq!(out, Outcome::Pause(delete::CANCELLED.to_string()));
        assert!(t.contains(&delete::only_y(false)), "{t}");
        assert!(
            asked.iter().any(|q| q.ends_with("[y/N]")),
            "不断网的删除只要 y：{asked:?}"
        );
        assert!(!asked.iter().any(|q| q.contains("[yes/N]")), "{asked:?}");
        assert_eq!(names(&s, &pp).len(), 9);

        // Empty 形态打编号：文案改说 yes
        let s = FakeSys::new();
        nine_nodes(&s, &pp, Mode::Socks);
        let (out, t, asked) = run_delete_menu(&s, &n, &pp, &["1-9", "5"]);
        assert_eq!(out, Outcome::Pause(delete::CANCELLED.to_string()));
        assert!(t.contains(&delete::only_y(true)), "{t}");
        assert!(
            asked.iter().any(|q| q.contains("[yes/N]")),
            "Empty 也要输 yes：{asked:?}"
        );
        assert_eq!(names(&s, &pp).len(), 9);

        // Empty 形态只输 y：提示要输 yes，什么都不拆
        let s = FakeSys::new();
        nine_nodes(&s, &pp, Mode::Socks);
        let (out, t, _) = run_delete_menu(&s, &n, &pp, &["1-9", "y"]);
        assert_eq!(out, Outcome::Pause(delete::CANCELLED.to_string()));
        assert!(t.contains(delete::NEEDS_YES), "{t}");
        assert!(s.exists(&pp.unit(UNIT_MAIN)) && s.exists(&pp.config()));
        assert_eq!(names(&s, &pp).len(), 9);

        // 一个节点都没有：不进这一页
        let s = FakeSys::new();
        ready(&s);
        let (out, t, asked) = run_delete_menu(&s, &n, &pp, &[]);
        assert_eq!(out, Outcome::Note(delete::NO_NODES.to_string()));
        assert!(!t.contains("删除哪几个"), "{t}");
        assert!(asked.is_empty(), "{asked:?}");
    }

    /// 第 6 条：`bui-c delete` 用 `delete_confirm_cli`——不列可换节点，换目标只能用 `--switch-to`。
    #[test]
    fn cli_delete_confirm_does_not_offer_to_change_the_target_by_number() {
        let pp = paths();
        let s = FakeSys::new();
        let n = FakeNet::new();
        nine_nodes(&s, &pp, Mode::Socks);
        let mut p = Scripted::from(["yes"]);
        let t = {
            let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
            dispatch(&parse(&["delete", ACTIVE]), &mut ctx).unwrap();
            ctx.transcript.clone()
        };
        assert!(t.contains(&format!("删完切到 [4] {RICK_REALITY}")), "{t}");
        assert!(t.contains("将删除 1 个节点，含当前节点："), "{t}");
        assert!(!t.contains("想换就输入下面的编号"), "命令行不认编号：{t}");
        assert!(!t.contains("可换："), "{t}");
        assert!(
            p.asked.iter().any(|q| q.contains("[yes/N]")),
            "{:?}",
            p.asked
        );
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(saved.profiles.len(), 8);
        assert_eq!(saved.active.as_deref(), Some(RICK_REALITY));

        // 会断网的删除只输 y：取消，退出码 0
        let s = FakeSys::new();
        nine_nodes(&s, &pp, Mode::Socks);
        let mut p = Scripted::from(["y"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["delete", ACTIVE]), &mut ctx).unwrap();
        assert!(
            ctx.transcript.contains(delete::NEEDS_YES),
            "{}",
            ctx.transcript
        );
        assert!(
            ctx.transcript.contains(delete::CANCELLED),
            "{}",
            ctx.transcript
        );
        assert_eq!(names(&s, &pp).len(), 9);

        // 打编号：照旧算取消（§5.10），但要说清命令行换目标的办法
        let s = FakeSys::new();
        nine_nodes(&s, &pp, Mode::Socks);
        let mut p = Scripted::from(["4"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["delete", ACTIVE]), &mut ctx).unwrap();
        assert!(
            ctx.transcript.contains(delete::CLI_NO_NUMBER),
            "{}",
            ctx.transcript
        );
        assert!(
            ctx.transcript.contains(delete::CANCELLED),
            "{}",
            ctx.transcript
        );
        assert_eq!(names(&s, &pp).len(), 9, "什么都没删");
    }

    /// 命令行成功那一句按 §5.4 说全（不是「上次：」行的短式，也不把菜单键漏进命令行）。
    #[test]
    fn cli_delete_spells_out_the_result_in_all_three_forms() {
        let pp = paths();
        let n = FakeNet::new();
        // Passive
        let s = FakeSys::new();
        nine_nodes(&s, &pp, Mode::Socks);
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, true);
        dispatch(&parse(&["delete", "HY2", "-y"]), &mut ctx).unwrap();
        assert!(
            ctx.transcript.contains("已删除 1 个节点，还剩 8 个"),
            "{}",
            ctx.transcript
        );
        // Empty：命令行里没有菜单键 [3]
        let s = FakeSys::new();
        let prof = nine_nodes(&s, &pp, Mode::Socks);
        s.reply("systemctl is-active --quiet bui-c.service", 3, "");
        let all: Vec<&str> = prof.profiles.iter().map(|p| p.name.as_str()).collect();
        let mut args = vec!["delete"];
        args.extend(all.iter().copied());
        args.push("-y");
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, true);
        dispatch(&parse(&args), &mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert!(
            t.contains("已删除全部 9 个节点，代理已停止；用 bui-c import 重新导入"),
            "{t}"
        );
        assert!(!t.contains("按 [3] 导入"), "菜单键不进命令行：{t}");
    }

    /// Passive 形态给了 `--switch-to`：校验照做（打错名字要早报），但它没有作用对象——
    /// 不报用法错误（R15 只认三种退出码 2），打一行说清「删了、没切」。
    #[test]
    fn cli_delete_says_switch_to_is_unused_when_nothing_active_was_deleted() {
        let pp = paths();
        let s = FakeSys::new();
        let n = FakeNet::new();
        nine_nodes(&s, &pp, Mode::Socks);
        let before = restarts(&s);
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, true);
        dispatch(
            &parse(&["delete", "HY2", "--switch-to", "reality-Reality", "-y"]),
            &mut ctx,
        )
        .unwrap();
        assert!(
            ctx.transcript.contains(delete::SWITCH_TO_UNUSED),
            "{}",
            ctx.transcript
        );
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(saved.active.as_deref(), Some(ACTIVE), "当前节点不变");
        assert_eq!(saved.profiles.len(), 8);
        assert_eq!(restarts(&s), before, "Passive 不重启");
    }

    #[test]
    fn cli_delete_without_yes_off_a_terminal_is_a_usage_error_with_exit_code_2() {
        let pp = paths();
        let s = FakeSys::new();
        let n = FakeNet::new();
        nine_nodes(&s, &pp, Mode::Socks);
        let mut p = Scripted {
            queue: ["yes".to_string()].into_iter().collect(),
            asked: Vec::new(),
            tty: false,
        };
        let (rc, err) = {
            let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
            let r = dispatch(&parse(&["delete", "HY2", "reality-Reality"]), &mut ctx);
            let mut err = Vec::new();
            let rc = finish(&mut ctx, r, &mut err);
            (rc, String::from_utf8(err).unwrap())
        };
        assert_eq!(rc, std::process::ExitCode::from(2));
        assert!(
            err.contains("要删除 2 个节点：HY2、reality-Reality"),
            "{err}"
        );
        assert!(err.contains("请加 -y 确认"), "{err}");
        assert!(p.asked.is_empty(), "绝不从管道里读确认：{:?}", p.asked);
        assert_eq!(names(&s, &pp).len(), 9);
        assert_eq!(p.queue.len(), 1, "脚本里的那一行留给别人");
    }

    #[test]
    fn cli_delete_json_requires_yes_and_prints_one_report() {
        let pp = paths();
        let s = FakeSys::new();
        let n = FakeNet::new();
        nine_nodes(&s, &pp, Mode::Socks);
        // --json 没带 -y：用法错误，退出码 2
        let mut p = Scripted::from([]);
        let (rc, err) = {
            let mut ctx = Ctx::new(&s, &n, &pp, &mut p, true, false);
            let r = dispatch(&parse(&["delete", "HY2", "--json"]), &mut ctx);
            let mut err = Vec::new();
            let rc = finish(&mut ctx, r, &mut err);
            (rc, String::from_utf8(err).unwrap())
        };
        assert_eq!(rc, std::process::ExitCode::from(2));
        assert!(err.contains("--json 要和 -y 一起用"), "{err}");
        assert_eq!(names(&s, &pp).len(), 9);
        // --json -y：stdout 只有一个对象，以换行结尾
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, true, true);
        dispatch(&parse(&["delete", "HY2", "--json", "-y"]), &mut ctx).unwrap();
        assert!(ctx.out.ends_with('\n'));
        assert_eq!(ctx.out.lines().count(), 1, "{:?}", ctx.out);
        let v: serde_json::Value = serde_json::from_str(&ctx.out).unwrap();
        assert_eq!(v["deleted"], serde_json::json!(["HY2"]));
        assert_eq!(v["active"], ACTIVE);
        assert_eq!(v["switched"], false);
        assert_eq!(v["stopped"], false);
        assert_eq!(v["remaining"], 8);
        assert_eq!(names(&s, &pp).len(), 8);
    }

    #[test]
    fn cli_delete_unknown_name_deletes_nothing() {
        let pp = paths();
        let s = FakeSys::new();
        let n = FakeNet::new();
        nine_nodes(&s, &pp, Mode::Socks);
        let before = restarts(&s);
        let mut p = Scripted::from([]);
        let (rc, err) = {
            let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, true);
            let r = dispatch(&parse(&["delete", "HY2", "nope", "-y"]), &mut ctx);
            let mut err = Vec::new();
            let rc = finish(&mut ctx, r, &mut err);
            (rc, String::from_utf8(err).unwrap())
        };
        assert_eq!(rc, std::process::ExitCode::FAILURE, "执行失败是 1");
        assert!(err.contains("没有叫 nope 的节点"), "{err}");
        assert!(err.contains("什么都没删"), "{err}");
        assert_eq!(names(&s, &pp).len(), 9, "不做「删掉找得到的那几个」");
        assert_eq!(restarts(&s), before);
        // --switch-to 无效也一样
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, true);
        let e = dispatch(
            &parse(&["delete", "HY2", "--switch-to", "nope", "-y"]),
            &mut ctx,
        )
        .unwrap_err();
        assert!(e.to_string().contains("--switch-to nope 不存在"), "{e}");
        assert_eq!(e.exit_code(), 1);
        assert_eq!(names(&s, &pp).len(), 9);
        // --switch-to 指到正在删的节点
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, true);
        let e = dispatch(
            &parse(&["delete", "HY2", "--switch-to", "HY2", "-y"]),
            &mut ctx,
        )
        .unwrap_err();
        assert!(e.to_string().contains("也在要删的节点里"), "{e}");
        assert_eq!(names(&s, &pp).len(), 9);
    }

    /// `--switch-to` 指定的替换节点要真的用上（spec §5.4 的命令行一行）。
    #[test]
    fn cli_delete_switch_to_picks_the_named_replacement() {
        let pp = paths();
        let s = FakeSys::new();
        let n = FakeNet::new();
        nine_nodes(&s, &pp, Mode::Socks);
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, true);
        dispatch(
            &parse(&["delete", ACTIVE, "--switch-to", "HY2", "-y"]),
            &mut ctx,
        )
        .unwrap();
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(
            saved.active.as_deref(),
            Some("HY2"),
            "不是 default_to 挑的那个"
        );
        assert_eq!(saved.profiles.len(), 8);
        assert!(
            ctx.transcript.contains("当前节点已删除，切到 HY2"),
            "命令行按 §5.4 说全，不用「上次：」行的短式：{}",
            ctx.transcript
        );
    }

    /// Switch 形态的 `--json`：成功时 stdout 只有一个 Report 加换行（`apply_with_ufw` 里
    /// 「已安装内核」「已放行 UFW」这些话不许漏出去），失败时 stdout 一个字都没有
    /// （文案走 stderr + 退出码 1，spec §5.10）。
    #[test]
    fn cli_delete_json_stays_pure_when_the_active_node_goes() {
        let pp = paths();
        let n = FakeNet::new();
        // 成功：TUN 模式会走 ufw allow，那几句 say 不能进 stdout
        let s = FakeSys::new();
        nine_nodes(&s, &pp, Mode::Tun);
        s.reply("ufw status", 0, "Status: active\n");
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, true, true);
        dispatch(&parse(&["delete", ACTIVE, "--json", "-y"]), &mut ctx).unwrap();
        assert!(ctx.out.ends_with('\n'));
        assert_eq!(ctx.out.lines().count(), 1, "{:?}", ctx.out);
        let v: serde_json::Value = serde_json::from_str(&ctx.out).unwrap();
        assert_eq!(v["switched"], true);
        assert_eq!(v["active"], RICK_REALITY);
        assert_eq!(v["remaining"], 8);
        // 失败：stdout 空，话都在 stderr
        let s = FakeSys::new();
        nine_nodes(&s, &pp, Mode::Tun);
        s.reply("ufw status", 0, "Status: active\n");
        s.reply("ip link show bui-tun", 1, "");
        let mut p = Scripted::from([]);
        let (rc, err, out) = {
            let mut ctx = Ctx::new(&s, &n, &pp, &mut p, true, true);
            let r = dispatch(&parse(&["delete", ACTIVE, "--json", "-y"]), &mut ctx);
            // finish 会把 ctx.out 冲到 stdout 再清空：先留一份，那就是 stdout 会收到的字节
            let out = ctx.out.clone();
            let mut err = Vec::new();
            let rc = finish(&mut ctx, r, &mut err);
            (rc, String::from_utf8(err).unwrap(), out)
        };
        assert_eq!(rc, std::process::ExitCode::FAILURE);
        assert!(err.contains("切到"), "{err}");
        assert!(out.is_empty(), "stdout 一个字都没有：{out:?}");
        assert_eq!(names(&s, &pp).len(), 9);
    }
}
