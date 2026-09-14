//! clap 子命令、`Ctx`（可注入的一次会话）、菜单循环与 `run()`。
//!
//! 所有会改机器的子命令都经 `apply_with_ufw`：装内核 → 按模式同步 UFW → `Engine::apply`。
//! root 检查只在 [`run`] 里做，`dispatch` 保持纯注入，单元测试直接调它。

use crate::check::{self, Runtime, Verdict};
use crate::delete::{self, PlanKind};
use crate::engine::{Applied, Engine};
use crate::lock::{self, LockGuard};
use crate::menu::{self, Action, MaintAction, MaintStatus, NextStep, Prompt, Status};
use crate::net::Net;
use crate::nettest::{self, Event, Hooks, Painter};
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
        /// 面板用户名或 token。写 - 从标准输入读，避免 token 留在 shell 历史
        #[arg(long)]
        user: Option<String>,
        /// 订阅地址（从面板复制的整条链接）。写 - 从标准输入读，避免 token 留在 shell 历史
        #[arg(long)]
        sub: Option<String>,
        #[arg(long)]
        activate: bool,
        /// 连删过的节点一起导（默认先跳过它们，spec §5.7）
        #[arg(long)]
        with_deleted: bool,
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
        /// 连删过的节点一起导（默认先跳过它们，spec §5.7）
        #[arg(long)]
        with_deleted: bool,
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
    /// [`say_aside`](Self::say_aside) 打过的行在 `transcript` 里的起始位置。
    pub asides: Vec<usize>,
    /// [`say_result`](Self::say_result) 打过的结果行：起始位置 + 原文（不含缩进）。
    pub results: Vec<(usize, String)>,
    /// 本菜单会话里哪几条旧键过渡提示已经显示过（按位记，[`menu::HINT_DELETE`] 等；spec §0.2 R1）。
    /// 记的是「第一次进这一页」，不是「每次画这一页」：清屏重画、回主菜单再进来都不再显示。
    pub hints_shown: u8,
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
            asides: Vec::new(),
            results: Vec::new(),
            hints_shown: 0,
        }
    }
    /// 这条过渡提示本会话还没显示过：返回 `true` 并记下（调用方接着就把它打出来）。
    pub fn first_time_hint(&mut self, bit: u8) -> bool {
        let first = self.hints_shown & bit == 0;
        self.hints_shown |= bit;
        first
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
    /// 说一句**不算「附加行」**的话：[`outcome_since`] 数行时跳过它，所以只因为它不会停下来
    /// 等回车。给「当前节点：X」这类回主菜单后状态区本来就显示的内容用。
    ///
    /// 由打印方标出来，不在 [`outcome_since`] 里按文案前缀猜（T7b 审查裁定）。只收单行。
    pub fn say_aside(&mut self, line: impl AsRef<str>) {
        self.asides.push(self.transcript.len());
        self.say(line);
    }
    /// 说一句**「这次动作的结果」**：[`outcome_since`] 拿它当「上次：」行的摘要，而不是取第一行。
    ///
    /// 导入的结果行前面可能先打别的话（面板接口退回订阅、跳过无法解析的行、墓碑那一问之后的
    /// 第二趟导入），取第一行会把结果挤掉。标了不止一句就以最后一句为准。只收单行。
    pub fn say_result(&mut self, line: impl AsRef<str>) {
        let line = line.as_ref().to_string();
        self.results.push((self.transcript.len(), line.clone()));
        self.say(line);
    }
    /// 原样打一块已经排好版的文字（菜单、节点列表、状态块），不叠 `indent`。
    pub fn show(&mut self, block: impl AsRef<str>) {
        let mut text = block.as_ref().to_string();
        text.push('\n');
        self.emit(&text);
    }
    /// 原样打一段已经排好版的字（不补换行、不叠缩进），并立刻冲出去：连接检查边做边打，
    /// 先出标签、等结果（spec §6.4）。transcript 里照样是完整的一行。
    pub fn part(&mut self, text: impl AsRef<str>) {
        self.emit(text.as_ref());
        self.flush();
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
    }
}

/// 这次检查之后「还有能装的新东西没装」吗——主菜单 `[7] 更新与维护 ★` 的口径（spec §8.1、§0.2 R4）。
///
/// 自身：版本不同，或同版本但不是同一份构建（rc 通道重建）。读不到本机二进制、manifest 缺本机
/// 架构的产物**不**点亮 ★——前者说不清是不是新版，后者进去也装不了。内核：本机版本不是 manifest
/// 要的（只换内核的 manifest 也挂 ★）。
/// 刚刚自替换成 manifest 那一份时要算「没有新版」：本进程的 `VERSION` 还是旧二进制的，
/// 不看 `self_updated` 的话菜单会一直挂着 ★，直到下次检查。
fn new_version_pending(r: &update::Report) -> bool {
    let own = matches!(
        r.self_reason,
        update::SelfReason::NewVersion | update::SelfReason::Rebuild
    );
    (own && !r.self_updated) || (r.kernel_outdated && !r.kernel_updated)
}

/// 一次检查更新（命令行、菜单 [7] → [1]、巡检里的自更新）落进 runtime.json 的三项：★、检查时间、
/// manifest 版本号。主菜单与 [7] 子页只读它们，不联网。
fn record_update_check(rt: &mut Runtime, r: &update::Report, at: i64) {
    rt.update_available = new_version_pending(r);
    rt.update_checked_at = Some(at);
    rt.update_version = Some(r.manifest_version.clone());
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
    /// 命中墓碑、这次没写入的节点名（墓碑里记的那个名字，spec §5.7）：菜单据此问一句，
    /// 命令行据此打 [`menu::buried_skipped`]
    buried: Vec<String>,
    /// 命中墓碑但按明确意愿加回来的 profile 名（墓碑已清）
    restored: Vec<String>,
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
///
/// `restore` 为假时认墓碑（spec §5.7）：删掉过的节点不写入，名字收进 [`Stored::buried`]；
/// 为真时照常写入并把墓碑清掉（`--with-deleted`、菜单里答了 y、单独粘贴一条链接）。
///
/// 墓碑只挡**新**节点（与 [`import_v3::import`] 同一个顺序：先认已有连接、再认墓碑）。
/// 已经在列表里的节点不是「回来」：住宅换槽位后新旧两个同 key 的节点并存、删掉旧的之后，
/// 新的那个照常刷新，与它同 key 的那条过期墓碑顺手清掉——否则它从此收不到凭据轮换，
/// 每刷新一次还要拿已经删掉的旧名字问一遍。
///
/// 一个节点都没存下（全被墓碑挡下）时不动 panel：什么都没导入，不该换自动更新来源。
fn store_fetched<S: Sys, N: Net, P: Prompt>(
    ctx: &mut Ctx<'_, S, N, P>,
    prof: &mut Profiles,
    f: &Fetched,
    src: Source,
    panel: Option<Panel>,
    restore: bool,
) -> Stored {
    let mut out = Stored {
        added: 0,
        names: Vec::with_capacity(f.nodes.len()),
        replaced: Vec::new(),
        buried: Vec::new(),
        restored: Vec::new(),
    };
    for node in &f.nodes {
        let (name, is_new) = match prof.find_same_endpoint(node) {
            Some(same) => (same.name.clone(), false),
            None => {
                // 删过的节点默认不写回来：面板 / 订阅导入是刷新节点的日常操作，不记墓碑的话
                // 每刷新一次删掉的就全回来（spec §5.7）。名字取墓碑里记的那个——用户删它时
                // 在列表上看到的就是它，这会儿的 profile 名还没算出来
                if !restore {
                    if let Some(t) = prof.tombstone_of(node) {
                        out.buried.push(t.name.clone());
                        continue;
                    }
                }
                let wanted = profile_name(&f.user, node);
                let name = match prof.profiles.iter().find(|p| p.name == wanted) {
                    Some(taken) if !same_account(&taken.node, node) => {
                        let fresh = prof.free_name(&wanted);
                        ctx.say(format!(
                            "节点名 {wanted} 已被另一个账号占用，新节点命名为 {fresh}"
                        ));
                        fresh
                    }
                    _ => wanted,
                };
                (name, true)
            }
        };
        let r = prof.upsert(Profile {
            name: name.clone(),
            node: node.clone(),
            split: f.split.clone(),
            source: src,
            imported_at: rfc3339(ctx.sys),
        });
        // 新节点走到这里就是明确要它：墓碑清掉，下次导入不再跳过（spec §5.7）。已有节点
        // 清的是同 key 的过期墓碑，它本来就在列表里，不算「恢复」
        if prof.forget(node) && is_new {
            out.restored.push(name.clone());
        }
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
    // panel 是 root 自更新的来源：换掉它必须让人看见。同一个面板只跟着改用户名，不出声。
    // 一个都没存下（全被墓碑挡下）就不动它：答 y 加回时拿的是同一批，那一趟再记
    if let Some(p) = panel.filter(|_| !out.names.is_empty()) {
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
///
/// `Clone` 是给墓碑那一问用的：答 y 之后要拿**同一批**节点再导一次，不能再联网一趟。
#[derive(Clone)]
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

/// 存下了、还没 apply 的一次导入：[`store_import`] 交给 [`apply_import`]。
struct Imported {
    stored: Stored,
    prof: Profiles,
    /// 这一趟真的选出了活动节点（首次导入或 `activate`，且至少存下了一个节点）
    activated: bool,
}

/// 落盘一批节点；首次导入或 `activate` 时激活第一个并 apply，原地更新了活动节点也 apply。
///
/// `with_deleted` 为真（`--with-deleted`、菜单里答了 y）时连删过的节点一起导。单独粘贴
/// **一条**节点链接也算「明确要它」，不用这个参数也直接加回（spec §5.7 表第 2 行）。
///
/// 返回 [`Stored`]：被墓碑挡下的名字在 `buried` 里——命令行据此打一行、菜单**放锁之后**
/// 据此问一句（spec §0.2 R11），所以这个函数自己既不打那一行也不问那一句。
///
/// 这是导入这个动作的入口：一进来就拿锁，锁里读 `profiles.json`、落盘、apply，返回时放锁
/// （R11「锁内 save_import + apply」、R16「与持锁重读的 profiles 合并」）。取节点（联网）在锁外。
fn save_import<S: Sys, N: Net, P: Prompt>(
    ctx: &mut Ctx<'_, S, N, P>,
    inc: Incoming,
    activate: bool,
    with_deleted: bool,
) -> Result<Stored> {
    let g = take_lock(ctx)?;
    let imported = store_import(ctx, inc, activate, with_deleted, &g)?;
    apply_import(ctx, &imported, &g)?;
    Ok(imported.stored)
}

/// [`save_import`] 的前半段：读盘、写入这批节点、落盘、打导入结果行，不 apply。持锁才能调用
/// （锁里读 `profiles.json`，spec §0.2 R16），自己绝不拿锁。
///
/// 分出来是给命令行用的：apply 失败时被墓碑挡下的名字照样要打出来（审查 T7b M1）。
///
/// 全部被墓碑挡下时一个节点都没存下：删光之后再从面板 / 订阅导入（spec §5.8 的恢复路、
/// §9 的 `profiles = []`）就是这样。这时不选活动节点，[`apply_import`] 也就不 apply——
/// 否则 engine 报「没有激活的节点」，命令行退出码 1、菜单问不到墓碑那一句。
fn store_import<S: Sys, N: Net, P: Prompt>(
    ctx: &mut Ctx<'_, S, N, P>,
    inc: Incoming,
    activate: bool,
    with_deleted: bool,
    _: &LockGuard,
) -> Result<Imported> {
    let mut prof = Profiles::load(ctx.sys, ctx.paths)?;
    let loaded = prof.clone();
    let had_active = prof.active_profile().is_some();
    // 粘一条就是明确要这一个：直接加回并清墓碑，不必再问（spec §5.7）
    let single = inc.src == Source::Paste && inc.fetched.nodes.len() == 1;
    let stored = store_fetched(
        ctx,
        &mut prof,
        &inc.fetched,
        inc.src,
        inc.panel,
        with_deleted || single,
    );
    let mut activated = false;
    if activate || !had_active {
        if let Some(name) = stored.names.first() {
            prof.active = Some(name.clone());
            activated = true;
        }
    }
    // 什么都没变（重复导入同一个面板、全被墓碑挡下）就不重写 profiles.json
    if prof != loaded {
        prof.save(ctx.sys, ctx.paths)?;
    }
    ctx.say_result(format!(
        "导入 {} 个新节点，共 {} 个",
        stored.added,
        prof.profiles.len()
    ));
    // 粘的这一条之前被删过：说一句，让人知道墓碑起过作用、现在已经清了。菜单里答 y 的第二趟
    // （`with_deleted`）往往也只剩一条，但那是多条粘贴里被挡下的那几个，结果照计数说
    // （spec §5.7 表第 1 行；审查 T7b r2 M6）
    if single && !with_deleted && !stored.restored.is_empty() {
        ctx.say_result(menu::BURIED_RESTORED);
    }
    Ok(Imported {
        stored,
        prof,
        activated,
    })
}

/// [`save_import`] 的后半段：选出了活动节点、或原地更新了活动节点时 apply。持锁才能调用，
/// 与 [`store_import`] 同一把锁（spec §0.2 R11「锁内 save_import + apply」），自己绝不拿锁。
fn apply_import<S: Sys, N: Net, P: Prompt>(
    ctx: &mut Ctx<'_, S, N, P>,
    imported: &Imported,
    g: &LockGuard,
) -> Result<()> {
    let prof = &imported.prof;
    if imported.activated {
        apply_with_ufw(ctx, prof, g)?;
        ctx.say_aside(format!(
            "{CURRENT_NODE_HEAD}{}",
            prof.active.clone().unwrap_or_default()
        ));
    } else if prof
        .active
        .as_ref()
        .is_some_and(|a| imported.stored.replaced.contains(a))
    {
        // 活动节点被原地更新（凭据轮换、端口变了）：不 apply 的话还跑着旧配置。
        // 只改了 label 时渲出的配置字节不变，engine 不会重启
        apply_with_ufw(ctx, prof, g)?;
    }
    Ok(())
}

/// `--user` / `--sub` 的值：写 `-` 从标准输入读一行（去首尾空白），token 不进 argv 与 shell
/// 历史。读到 EOF 或空行就报错，不拿空值去联网。
fn arg_or_stdin<S: Sys, N: Net, P: Prompt>(
    ctx: &mut Ctx<'_, S, N, P>,
    flag: &str,
    raw: &str,
    ask: &str,
) -> Result<String> {
    if raw != "-" {
        return Ok(raw.to_string());
    }
    let line = ctx.prompt.read(ask)?.unwrap_or_default();
    match line.trim() {
        "" => Err(Error::msg(stdin_empty(flag))),
        v => Ok(v.to_string()),
    }
}

/// 命令行从面板取节点失败：面板不认这个地址（401 / 404）时补一句出路，再把错误交出去。
fn relink_if_rejected<S: Sys, N: Net, P: Prompt>(
    ctx: &mut Ctx<'_, S, N, P>,
    fetched: Result<Incoming>,
) -> Result<Incoming> {
    fetched.inspect_err(|e| {
        if link_rejected(e) {
            ctx.say(RELINK_CLI);
        }
    })
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
            // 读 profiles.json 之前拿锁（spec §8.3）：锁外读的那份可能已经被别的会话改过
            let g = take_lock(ctx)?;
            let mut prof = Profiles::load(ctx.sys, ctx.paths)?;
            if !prof.profiles.iter().any(|p| &p.name == name) {
                return Err(Error::msg(format!(
                    "节点 {name} 不存在，用 `bui-c list` 看可用节点"
                )));
            }
            if prof.active.as_deref() == Some(name.as_str()) {
                // 兜底 apply 一次：配置丢了、单元没建时这是唯一不绕路的补救；配置没变就不重启
                apply_with_ufw(ctx, &prof, &g)?;
                ctx.say(format!("已是当前节点：{name}"));
                return Ok(());
            }
            prof.active = Some(name.clone());
            prof.save(ctx.sys, ctx.paths)?;
            apply_with_ufw(ctx, &prof, &g)?;
            ctx.say(format!("已切到 {name}"));
            Ok(())
        }
        Cmd::Mode { mode } => {
            let g = take_lock(ctx)?;
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
            with_deleted,
        } => {
            // 标准输入只有一份：`-` 写在两处是用法错误，先判，一行都不读
            let dashes = [uri, user, sub]
                .iter()
                .filter(|v| v.as_deref() == Some("-"))
                .count();
            if dashes > 1 {
                return Err(Error::usage(STDIN_ONCE));
            }
            let inc = if let (Some(base), Some(u)) = (panel.as_ref(), user.as_ref()) {
                let u = arg_or_stdin(ctx, "--user", u, "面板用户名或 token")?;
                let fetched = fetch_panel(ctx, base, &u);
                relink_if_rejected(ctx, fetched)?
            } else if let Some(url) = sub.as_ref() {
                let url = arg_or_stdin(ctx, "--sub", url, "订阅地址")?;
                // 面板链接与菜单 [3] 同一条路：先试 `/api/nodes`（带分流规则，载荷里有人名，
                // token 链接也拿得到名字），`/api/sub/` 取不到再退回订阅
                let fetched = fetch_http(ctx, &url);
                if source::panel_link(&url).is_some() {
                    relink_if_rejected(ctx, fetched)?
                } else {
                    fetched?
                }
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
                // 一个来源都没给是用法错误（spec §0.2 R15：退出码 2）
                return Err(Error::usage(IMPORT_NO_SOURCE));
            };
            // 取节点（联网）在锁外；锁里读 profiles、落盘、apply（spec §0.2 R11 / R16），
            // 打墓碑那一行在放锁之后
            let g = take_lock(ctx)?;
            let imported = store_import(ctx, inc, *activate, *with_deleted, &g)?;
            let applied = apply_import(ctx, &imported, &g);
            drop(g);
            // 命令行不提问（脚本里跑它不能卡在一个 [y/N] 上）：说清跳了哪几个、怎么加回来。
            // apply 失败也照样打：节点已经存下了，被跳过的是哪几个、怎么加回来仍然要知道
            let buried = &imported.stored.buried;
            if !buried.is_empty() {
                ctx.say(menu::buried_skipped(buried));
            }
            applied
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
        Cmd::Check => run_check(ctx),
        Cmd::Update { check_only, auto } => {
            // 开关写 profiles.json、安装会替换二进制并重启：都在锁里。只查不装不拿锁。
            // T12b 把下载挪到锁外之后，这里只剩安装那一段持锁
            let _g = if auto.is_some() || !*check_only {
                Some(take_lock(ctx)?)
            } else {
                None
            };
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
                // 结论按原因分开说（spec §8.1 末条）：读不到本机二进制、缺本机架构的产物不再
                // 被说成「有同版本的新构建」
                for l in menu::check_only_verdict(&r, update::arch_suffix()) {
                    ctx.say(l);
                }
            } else {
                ctx.say(format!(
                    "自身更新={} 内核更新={} 已重启={}",
                    r.self_updated, r.kernel_updated, r.restarted
                ));
            }
            let mut rt = Runtime::load(ctx.sys, ctx.paths);
            let now = ctx.sys.now().unix_timestamp();
            record_update_check(&mut rt, &r, now);
            if !*check_only {
                rt.last_update_at = Some(now);
                rt.last_update_attempt_at = Some(now);
            }
            rt.save(ctx.sys, ctx.paths)?;
            // manifest 缺本机架构的 bui-c：内核照换了，但 bui-c 没换成，脚本要从退出码看到失败
            // （以前在下载那一步就报「manifest 里没有 … 这个产物」退出）
            if !*check_only && r.self_reason == update::SelfReason::MissingAsset {
                return Err(Error::msg(
                    menu::update_missing_asset(update::arch_suffix()),
                ));
            }
            Ok(())
        }
        Cmd::ImportV3 {
            base,
            panel,
            mode,
            with_deleted,
        } => {
            let r = import_v3_cmd(
                ctx,
                base.clone(),
                panel.clone(),
                mode.map(Into::into),
                *with_deleted,
            )?;
            // 命令行不提问（脚本里跑它不能卡在一个 [y/N] 上）：说清跳了哪几个、怎么加回来
            if !r.buried.is_empty() {
                ctx.say(menu::buried_skipped(&r.buried));
            }
            Ok(())
        }
        Cmd::Uninstall { purge_bin } => {
            if !ctx.yes && !ctx.prompt.confirm("确认卸载 bui-c（节点配置一并删除）？")?
            {
                ctx.say("已取消");
                return Ok(());
            }
            // 清单与确认在上面做完，只在 uninstall::run 外面拿锁（spec §0.2 R11）
            let g = take_lock(ctx)?;
            let r = uninstall::run(ctx.sys, ctx.paths, *purge_bin)?;
            drop(g);
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

/// `bui-c import-v3` 与菜单 `[7]` → `[3]` 共用的那一趟：导入 → 说结果 → apply。
///
/// 墓碑那一问**不在这里**（spec §5.7 表第 3 行、§0.2 R11「放锁之后再问」）：命令行打
/// [`menu::buried_skipped`]、菜单经 [`menu_import_v3`] 问一句，两边都从返回的
/// [`import_v3::Report::buried`] 拿名字。
fn import_v3_cmd<S: Sys, N: Net, P: Prompt>(
    ctx: &mut Ctx<'_, S, N, P>,
    base: Option<PathBuf>,
    panel: Option<String>,
    mode: Option<Mode>,
    with_deleted: bool,
) -> Result<import_v3::Report> {
    // 从 v3 导入这个动作的入口：锁里读 profiles、导入、卸旧单元、apply（spec §0.2 R11「[7]→[3] 同 [3]」）
    let g = take_lock(ctx)?;
    let mut prof = Profiles::load(ctx.sys, ctx.paths)?;
    let dir = base.unwrap_or_else(|| PathBuf::from(import_v3::V3_BASE));
    let opts = import_v3::RunOpts {
        panel: panel.or_else(|| ctx.sys.env("BUI_C_PANEL")),
        mode,
        with_deleted,
    };
    let r = import_v3::run(ctx.sys, ctx.net, ctx.paths, &dir, &mut prof, &opts)?;
    if r.kernel_installed {
        ctx.say("已安装 sing-box 内核");
    }
    // v3 目录是回滚素材、按约定留着，所以这条命令（菜单 [7] → [3]）在已迁移的机器上
    // 随时可能被再按一次。没有新节点就没什么要 apply 的：省掉 ufw/engine 那趟
    // 往返，也就不会出现「配置字节不变→不重启」的窗口。
    if r.imported.is_empty() {
        let tail = if r.removed_units.is_empty() {
            "未做任何改动".to_string()
        } else {
            format!("清掉 {} 个残留的 v3 单元", r.removed_units.len())
        };
        // 不串名字：真机 9 个节点 join 起来一行两百多列，名字 `bui-c list` 里都看得到
        ctx.say_result(if r.existing.is_empty() {
            // 走到这里而 existing 为空，只能是「v3 的节点全被删过」（都没导过又都没得导
            // 的话 import 已经报错了）：别说成「0 个节点都已导入过」
            format!("v3 的节点都被删过，{tail}")
        } else {
            format!("v3 的 {} 个节点都已导入过，{tail}", r.existing.len())
        });
        for s in &r.skipped {
            ctx.say(format!("跳过：{s}"));
        }
        if r.ufw_restored {
            ctx.say("已恢复被 v3 关掉的 UFW");
        }
        return Ok(r);
    }
    ctx.say_result(format!(
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
    apply_with_ufw(ctx, &prof, &g)?;
    ctx.say_aside(format!(
        "{CURRENT_NODE_HEAD}{}",
        prof.active.clone().unwrap_or_default()
    ));
    Ok(r)
}

/// 巡检：`bui-c check`（timer 每分钟一次），只有 204 探测与更新源两类请求。
///
/// 菜单 `[5]` 不走这里，走 [`check_menu`]：人点的是「连接检查」，不该顺带把二进制换掉
/// （spec §6.7）。每日自更新只留在这条路径上。
fn run_check<S: Sys, N: Net, P: Prompt>(ctx: &mut Ctx<'_, S, N, P>) -> Result<()> {
    let v = check::run(ctx.sys, ctx.net, ctx.paths)?;
    match &v {
        Verdict::NoProfile => ctx.say("没有激活的节点，巡检跳过"),
        Verdict::Ok => ctx.say("正常：单元在跑、204 探测通过"),
        Verdict::Restarted {
            failures,
            next_backoff_min,
            tun_ready,
        } => {
            ctx.say(format!(
                "发现 {} 项异常，已重启 bui-c.service，下次退避 {next_backoff_min} 分钟",
                failures.len()
            ));
            for f in failures {
                ctx.say(format!("  - {f}"));
            }
            // is-active 在 exec 之后立刻为真，TUN 接口还没起来：check::run 在重启的那把锁里
            // 等过了（最多 5 秒，只在刚重启过时发生），这里只报结果
            match tun_ready {
                Some(true) => ctx.say("bui-tun 已就绪"),
                Some(false) => ctx.say(format!(
                    "bui-tun 接口 {} 秒内没起来，查 [4] 服务控制 → 最近日志",
                    crate::engine::TUN_READY_WAIT_S
                )),
                None => {}
            }
        }
        // 锁被占着（或拿到锁时节点设置已经变了）：这一轮什么都不做，自更新也等下一轮，
        // 不写 runtime.json（spec §8.3）
        Verdict::Busy => {
            ctx.say("另一个 bui-c 操作进行中，本轮巡检跳过");
            return Ok(());
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
        // 装新版会替换二进制并重启：只试一次锁，拿不到就等下一分钟，也不记「尝试过」——
        // 没真的去试，不该落进 1 小时的失败退避。T12b 把下载挪到锁外
        let Some(_g) = lock::acquire(ctx.sys, ctx.paths, lock::How::Once)? else {
            tracing::info!("另一个 bui-c 操作进行中，本轮自更新跳过");
            return Ok(());
        };
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
                let now = ctx.sys.now().unix_timestamp();
                rt.last_update_at = Some(now);
                // 自更新也是一次「检查更新」：装完了就把菜单上的 ★ 摘掉
                record_update_check(&mut rt, &r, now);
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
/// 清屏，导入失败的原因不停下来就被抹掉了，而这正是迁移那一刻。导入经 [`menu_import_v3`]，与菜单
/// `[7]` → `[3]` 同一趟（失败打「失败：…」、被墓碑挡下的放锁之后问一句）。
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
        // 与菜单 [7] → [3] 同一条路：profiles 为空不等于墓碑为空——拒过邀请、改从面板导入、后来删光的
        // 机器上，v3 的节点会全部命中墓碑（审查 T7b r2 I1）。走 [`menu_import_v3`] 才会问那一句，
        // 也不会在菜单里打出带 `--with-deleted` 的命令行文案
        menu_import_v3(ctx)?
    } else {
        // 交互终端里这一句接着就被清屏抹掉、只活在「上次：」行里：菜单走法写在前头
        note(ctx, menu::V3_SKIPPED)
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
/// 它不算附加行，不因为它停（审查裁定）——所以这几处一律经
/// [`Ctx::say_aside`] 打，而不是让 [`outcome_since`] 按这个前缀去猜。
const CURRENT_NODE_HEAD: &str = "当前节点：";

/// 从 transcript 的 `start` 起新打的内容定 [`Outcome`]，照 spec §4.3 的一条标准：失败，或者
/// 这个动作自己打了不止一行 → 停；否则不停。
///
/// 摘要的来源：有「失败：」行 → 停，摘要就是它；动作自己标过结果行（[`Ctx::say_result`]）→
/// 摘要用最后那一条（导入的「上次：」行必须是导入结果本身，不能被它前面的附加提示占掉）；
/// 没标过就取第一行。停不停只看附加行的条数：多于一行 → 停；只有一行 → 不停；什么都没打 →
/// 「上次：」行不变。空行与 [`Ctx::say_aside`] 标过的行都不算行。
fn outcome_since<S: Sys, N: Net, P: Prompt>(ctx: &Ctx<'_, S, N, P>, start: usize) -> Outcome {
    // transcript 只追加不截断，`start` 一定落在字符边界上；`split_inclusive` 留着换行，
    // 一路加出来的 `at` 就是每一行在 transcript 里的起始位置，正好对上 `asides` 记的那个
    let mut new: Vec<&str> = Vec::new();
    let mut at = start;
    for raw in ctx.transcript[start..].split_inclusive('\n') {
        let line = raw.trim();
        if !line.is_empty() && !ctx.asides.contains(&at) {
            new.push(line);
        }
        at += raw.len();
    }
    if let Some(fail) = new.iter().find(|l| l.starts_with("失败：")) {
        return Outcome::Pause(fail.to_string());
    }
    let marked = ctx
        .results
        .iter()
        .rev()
        .find(|(at, _)| *at >= start)
        .map(|(_, line)| line.clone());
    match (new.as_slice(), marked) {
        ([], _) => Outcome::Nothing,
        ([_], Some(m)) => Outcome::Note(m),
        ([one], None) => Outcome::Note(one.to_string()),
        (_, Some(m)) => Outcome::Pause(m),
        ([first, ..], None) => Outcome::Pause(first.to_string()),
    }
}

/// 提问本身就是停顿：结果已经在提问处看过了，回主菜单不再停（摘要照旧，spec §4.3）。
fn asked_is_a_pause(out: Outcome) -> Outcome {
    match out {
        Outcome::Pause(s) => Outcome::Note(s),
        other => other,
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
/// 第一次没拿到锁时说一句（然后才开始等）。容量口径 51 列。
const LOCK_WAITING: &str = "另一个 bui-c 操作正在进行，等它结束（最多 15 秒）…";
/// 等不到锁：别的 bui-c 正在改东西（退出码 1，spec §0.2 R15）。锁在动手之前拿，所以
/// 「什么都没改」在每个入口都成立。容量口径 51 列。
const LOCK_BUSY: &str = "另一个 bui-c 操作还没结束，这次什么都没改，稍后再试";

/// 顶层入口拿锁（spec §8.3、§0.2 R11）：先试一次；没拿到就说一句「在等」并冲出去（人看得到
/// 为什么卡住），再每 250ms 试一次，最多 15 秒，还拿不到报 [`LOCK_BUSY`]。`--json` 下那一句不打。
///
/// 只在动作的入口拿：拿到的 [`LockGuard`] 按引用递给改机器的函数，它们自己绝不拿锁，
/// 持锁期间也绝不提问（持着锁等人，timer 就跳过一分钟，另一个会话也卡住）。
fn take_lock<S: Sys, N: Net, P: Prompt>(ctx: &mut Ctx<'_, S, N, P>) -> Result<LockGuard> {
    if let Some(g) = lock::acquire(ctx.sys, ctx.paths, lock::How::Once)? {
        return Ok(g);
    }
    tell(ctx, LOCK_WAITING);
    ctx.flush();
    lock::acquire(ctx.sys, ctx.paths, lock::How::Wait(LOCK_WAIT))?
        .ok_or_else(|| Error::msg(LOCK_BUSY))
}

/// 拿锁（[`take_lock`]）、在锁里跑 `f`、`f` 返回就放锁。`f` 里绝不提问。
fn with_lock<'a, S: Sys, N: Net, P: Prompt, T>(
    ctx: &mut Ctx<'a, S, N, P>,
    f: impl FnOnce(&mut Ctx<'a, S, N, P>, &LockGuard) -> Result<T>,
) -> Result<T> {
    let g = take_lock(ctx)?;
    f(ctx, &g)
}

/// 删除与导入流程里说一句：按当前宽度折行、缩进两列（`SelError::message` 那种 59 列的句子、
/// 墓碑那一句里的一串节点名，在 40 列终端上都要折，spec §0.2 R1）。`--json` 下一个字都不打——
/// 那时 stdout 只放一个 Report。
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
    let mut plan = delete::plan(&old, names, switch_to)?;
    // 每个被删节点记一条墓碑，**和 profiles、active 同一次原子写**（spec §5.7、§0.2 R10 第 7 步）：
    // 下次从面板 / 订阅 / v3 导入时先跳过它们，再问一句要不要加回。不记的话，面板导入是刷新
    // 节点的日常操作，每刷新一次删掉的就全回来，删除等于没做
    let at = ctx.sys.now().unix_timestamp();
    for name in &plan.targets {
        if let Some(p) = old.profiles.iter().find(|p| &p.name == name) {
            plan.next.bury(p, at);
        }
    }
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
/// 页顶的旧键过渡提示（[`menu::MOVED_HINT_DELETE`]）每个菜单会话只在第一次进这一页时显示
/// （spec §0.2 R1）；没有节点、不进这一页时不算显示过。
fn delete_menu<S: Sys, N: Net, P: Prompt>(ctx: &mut Ctx<'_, S, N, P>) -> Result<Outcome> {
    let prof = Profiles::load(ctx.sys, ctx.paths)?;
    if prof.profiles.is_empty() {
        return Ok(note(ctx, delete::NO_NODES));
    }
    let len = prof.profiles.len();
    let width = ctx.width();
    ctx.clear_screen();
    let hint = ctx
        .first_time_hint(menu::HINT_DELETE)
        .then_some(menu::MOVED_HINT_DELETE);
    ctx.show(menu::render_delete_picker(&prof, width, hint).trim_end());

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
            } else if import_v3::detect(ctx.sys, std::path::Path::new(import_v3::V3_BASE)) {
                // 有 v3 客户端：另起一行说 [7] 更新与维护 → [3]（拼成一句超 59 列）。两行要看完，
                // 停一下再回主菜单，「上次：」行记第一句
                tell(ctx, menu::NO_UNITS);
                tell(ctx, menu::NO_UNITS_V3);
                Outcome::Pause(menu::NO_UNITS.to_string())
            } else {
                note(ctx, menu::NO_UNITS)
            }
        }
        // 手动检查：边做边打的 11 行报告（spec §6）。不走 timer 的退避，也不顺带每日自更新
        Action::Check => {
            let start = ctx.transcript.len();
            match check_menu(ctx) {
                Ok(o) => o,
                Err(e) => {
                    ctx.say(format!("失败：{e}"));
                    outcome_since(ctx, start)
                }
            }
        }
        // 用户最初要的「数字菜单能删除节点」（spec §5）：清屏、列表、确认都在 delete_menu 里
        Action::DeleteNode => delete_menu(ctx)?,
        Action::Maintenance => maint_menu(ctx, prof)?,
        // T16 之前没有测速：按 9 的多半是 v4 的「自动更新 开/关」老习惯。不翻开关、不联网，
        // 一句话指到新位置（spec §0.2 R15）
        Action::SpeedTest => note(ctx, menu::AUTO_UPDATE_MOVED),
        Action::Uninstall => run_sub(ctx, Cmd::Uninstall { purge_bin: false }),
    })
}

/// 菜单 `[7] 更新与维护`（spec §1.1、§8.4）：清屏 → 三行本地事实 + 三个动作 → 选编号（输错原地
/// 重问，不重画）。空行、`0`、EOF 返回主菜单，「上次：」行不动。
///
/// - [1] 检查更新：先显示查到的版本、说清为什么要换，有能装的才问 y/N（[`maint_check_update`]）。
/// - [2] 自动更新开关：按下即执行——不动数据面、不联网，再按一次就撤回（R14）；一行 Note 回主菜单。
/// - [3] 从 v3 导入：原主菜单 [7]，逻辑不变。
fn maint_menu<S: Sys, N: Net, P: Prompt>(
    ctx: &mut Ctx<'_, S, N, P>,
    prof: &Profiles,
) -> Result<Outcome> {
    ctx.clear_screen();
    let page = menu::render_maint(&maint_status(ctx, prof), ctx.width());
    ctx.show(page.trim_end());
    let action = loop {
        ctx.flush();
        let pick = ctx.prompt.line("选择 [0-3]")?;
        match menu::parse_maint_choice(&pick) {
            Some(a) => break a,
            None => ctx.say(menu::invalid_maint_choice(&pick, ctx.width())),
        }
    };
    Ok(match action {
        MaintAction::Back => Outcome::Nothing,
        MaintAction::CheckUpdate => maint_check_update(ctx)?,
        MaintAction::ToggleAuto => run_sub(
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
        MaintAction::ImportV3
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
        MaintAction::ImportV3 => menu_import_v3(ctx)?,
    })
}

/// `[7]` → `[1]` 检查更新（spec §8.1）：打「正在检查更新…」→ 只读地查一次（不拿锁）→ 结论落盘
/// （★ 与子页的「上次检查」立刻跟上）→ 按原因说清楚 → 有能装的才问 `[y/N]`，默认否。
///
/// - 取不到 manifest：`检查更新失败：…`，停下来。
/// - 已是最新：一行 Note。
/// - manifest 缺本机架构的 bui-c、内核也不用换：说完停下，不问（问了也装不了）。
/// - 答否：一行 Note「没有更新（最新 X）」；答是：[`maint_install_update`]。
///
/// 提问本身就是停顿，所以答否不再停。检查与提问都在锁外（R11）。
fn maint_check_update<S: Sys, N: Net, P: Prompt>(ctx: &mut Ctx<'_, S, N, P>) -> Result<Outcome> {
    tell(ctx, menu::UPDATE_CHECKING);
    ctx.flush();
    let prof = Profiles::load(ctx.sys, ctx.paths)?;
    let r = match update::run(
        ctx.sys,
        ctx.net,
        ctx.paths,
        &with_panel_override(ctx.sys, &prof),
        true,
    ) {
        Ok(r) => r,
        Err(e) => {
            // 错误里不带 URL（`request_error`）；折行打，「上次：」行记同一句
            let line = format!("检查更新失败：{e}");
            tell(ctx, &line);
            return Ok(Outcome::Pause(line));
        }
    };
    let mut rt = Runtime::load(ctx.sys, ctx.paths);
    record_update_check(&mut rt, &r, ctx.sys.now().unix_timestamp());
    rt.save(ctx.sys, ctx.paths)?;
    let restart = ctx.sys.exists(&ctx.paths.unit(UNIT_MAIN));
    match menu::update_offer(&r, crate::VERSION, update::arch_suffix(), restart) {
        menu::UpdateOffer::Current(line) => Ok(note(ctx, line)),
        menu::UpdateOffer::Blocked(line) => {
            tell(ctx, &line);
            Ok(Outcome::Pause(line))
        }
        menu::UpdateOffer::Ask {
            lines,
            question,
            declined,
        } => {
            for l in &lines {
                tell(ctx, l);
            }
            ctx.flush();
            if ctx.prompt.confirm(&question)? {
                Ok(maint_install_update(ctx))
            } else {
                Ok(note(ctx, declined))
            }
        }
    }
}

/// [7] → [1] 答是之后：拿锁 → `update::run(check_only = false)` → 落盘 → 放锁，再把结果打出来停下
/// （spec §8.1 第 5 步、§3e-60-5）。结果说人话（换了什么、重没重启），不转手命令行 `bui-c update`——
/// 那一行 `自身更新=true …` 是给脚本看的，进「上次：」行没人看得懂。
///
/// 失败：停下来，「上次：」行是「失败：…」；runtime 不动（★ 照挂，也不算更新过）。
/// T12b 把下载挪到锁外，这里只剩安装那一段持锁。
fn maint_install_update<S: Sys, N: Net, P: Prompt>(ctx: &mut Ctx<'_, S, N, P>) -> Outcome {
    let done = with_lock(ctx, |ctx, _g| {
        let prof = Profiles::load(ctx.sys, ctx.paths)?;
        let r = update::run(
            ctx.sys,
            ctx.net,
            ctx.paths,
            &with_panel_override(ctx.sys, &prof),
            false,
        )?;
        let mut rt = Runtime::load(ctx.sys, ctx.paths);
        let now = ctx.sys.now().unix_timestamp();
        record_update_check(&mut rt, &r, now);
        rt.last_update_at = Some(now);
        rt.last_update_attempt_at = Some(now);
        rt.save(ctx.sys, ctx.paths)?;
        Ok(r)
    });
    match done {
        Ok(r) => {
            let lines = menu::update_done(&r);
            for l in &lines {
                tell(ctx, l);
            }
            Outcome::Pause(lines[0].clone())
        }
        Err(e) => {
            let line = format!("失败：{e}");
            tell(ctx, &line);
            Outcome::Pause(line)
        }
    }
}

/// `[7]` 子页顶部三行（spec §8.1 第一条）：只读 `runtime.json` 与 `profiles.json`，不联网。
/// 「上次检查」与主菜单的 ★ 同一个来源（`runtime.update_available`），有新版时带上那次查到的版本号。
fn maint_status<S: Sys, N: Net, P: Prompt>(ctx: &Ctx<'_, S, N, P>, prof: &Profiles) -> MaintStatus {
    let rt = Runtime::load(ctx.sys, ctx.paths);
    let verdict = match (rt.update_available, rt.update_version.as_deref()) {
        (true, Some(v)) => format!("有新版 {v}"),
        (true, None) => "有新版".to_string(),
        (false, _) => "已是最新".to_string(),
    };
    let update_line = match rt.update_checked_at {
        Some(at) => {
            let secs = ctx.sys.now().unix_timestamp() - at;
            format!("{verdict}（{}）", menu::ago(secs))
        }
        None if rt.update_available => verdict,
        None => "还没检查过".to_string(),
    };
    MaintStatus {
        version: crate::VERSION.to_string(),
        update_line,
        auto_update: prof.auto_update,
    }
}

/// 菜单 `[5] 连接检查`（spec §6）：清屏后逐行边做边打 11 项，汇总；有计分项失败时给一句人话判断，
/// 需要时再给固定编号的「下一步」小菜单（§6.5、§0.2 R3）。不受 timer 的退避约束，也**不**顺带
/// 每日自更新（§6.7）。没有节点时不进报告页，一行 Note 回主菜单。
fn check_menu<S: Sys, N: Net, P: Prompt>(ctx: &mut Ctx<'_, S, N, P>) -> Result<Outcome> {
    let prof = Profiles::load(ctx.sys, ctx.paths)?;
    if prof.active_profile().is_none() {
        return Ok(note(ctx, "没有节点可检查：先用 [3] 导入节点"));
    }
    'check: loop {
        // 报告是边做边打的：先清屏，第一行立刻出来（spec §4.1、§6.4）
        ctx.clear_screen();
        let width = ctx.width();
        ctx.part(nettest::header(&prof, width));
        let (sys, net, paths) = (ctx.sys, ctx.net, ctx.paths);
        let mut hooks = MenuHooks {
            ctx: &mut *ctx,
            painter: Painter::new(width),
            prof: &prof,
        };
        let sum = nettest::run(sys, net, paths, &prof, &mut hooks)?;
        let saw_info = hooks.painter.saw_info();
        ctx.part(nettest::render_summary(&sum, saw_info, width));
        let last = nettest::last_summary(&sum);
        let Some((sentence, offer)) = nettest::diagnose(&sum) else {
            return Ok(Outcome::Pause(last));
        };
        let sentence = nettest::render_sentence(sentence, width);
        ctx.part(&sentence);
        if !offer {
            return Ok(Outcome::Pause(last));
        }
        ctx.part(menu::render_next_step());
        loop {
            ctx.flush();
            let pick = ctx.prompt.line("选择 [0-3]")?;
            match menu::parse_next_step(&pick) {
                Some(NextStep::Recheck) => continue 'check,
                // T16 之前 [2] 是「换个节点」：进 [1] 同一页。返回时「上次：」行仍是检查的结论
                Some(NextStep::SpeedTest) => {
                    ctx.clear_screen();
                    ctx.show(menu::render_node_picker(&prof, ctx.width()).trim_end());
                    return Ok(match pick_node(ctx, &prof)? {
                        Some(name) => switch_node(ctx, name),
                        None => Outcome::Note(last),
                    });
                }
                // 看完日志回到这个小菜单：日志下面再给一遍判断与选项（spec §0.2 R3）
                Some(NextStep::Journal) => {
                    show_journal(ctx, menu::SERVICE_LOG_LINES);
                    ctx.part(&sentence);
                    ctx.part(menu::render_next_step());
                }
                Some(NextStep::Back) => return Ok(Outcome::Note(last)),
                // 输错原地重问，不重画报告
                None => ctx.say(menu::invalid_next_step(&pick, ctx.width())),
            }
        }
    }
}

/// [`MenuHooks::repair`] 拿到锁后发现节点设置已经变了：不动手。容量口径 34 列，接在「修复失败：」
/// 后面 44 列。
const REPAIR_STALE: &str = "节点设置刚被别处改过，这次没有重启";

/// 菜单 `[5]` 的 [`Hooks`]：事件按当前宽度排好就打出来（边做边打）；修复照 spec §0.2 R13。
struct MenuHooks<'c, 'a, S: Sys, N: Net, P: Prompt> {
    ctx: &'c mut Ctx<'a, S, N, P>,
    painter: Painter,
    prof: &'c Profiles,
}

impl<S: Sys, N: Net, P: Prompt> Hooks for MenuHooks<'_, '_, S, N, P> {
    fn event(&mut self, e: Event) {
        let text = self.painter.paint(&e);
        self.ctx.part(text);
    }

    /// 修一次，整段持锁（spec §0.2 R11「[5] 只在 repair（重启、等就绪）那一段拿锁」）：
    ///
    /// - 要不要修由 `nettest` 查出来的 1–4 项决定，这里**不再探测**（T11 审查 I2：以前走巡检的
    ///   `run_manual` 会再打一次 8 秒的隧道探测，还看不见本地端口，端口没在听时一次都不重启）；
    /// - 锁里重读 `profiles.json`，与检查开始时的快照比，变了就不动手：删光节点的会话刚拆完数据面，
    ///   按旧设置 apply 会把删掉的节点拉起来；
    /// - 重启走 [`check::restart`]：不受退避约束，照样记进 runtime.json，timer 的退避从它算起；
    ///   然后在同一把锁里等就绪（`wait_ready`，T11 审查 M5）；
    /// - 有活动节点、主单元文件却不在（删光没做完留下的）时改做 apply：restart 一个不存在的单元只会
    ///   报 Unit not found（R13）。apply 自己等过 TUN，SOCKS 再等端口。T12c 之后 apply 改走收敛。
    fn repair(&mut self, wait_ready: &mut dyn FnMut()) -> Result<Verdict> {
        let seen = delete::snapshot(self.prof);
        with_lock(self.ctx, |ctx, g| {
            let (sys, paths) = (ctx.sys, ctx.paths);
            let now = Profiles::load(sys, paths)?;
            if delete::snapshot(&now) != seen || now.active_profile().is_none() {
                return Err(Error::msg(REPAIR_STALE));
            }
            if !sys.exists(&paths.unit(UNIT_MAIN)) {
                let applied = Engine::new(sys, paths).apply(&now)?;
                if !applied.restarted {
                    return Ok(Verdict::Ok);
                }
                if now.mode == Mode::Socks {
                    wait_ready();
                }
                return Ok(Verdict::Restarted {
                    failures: Vec::new(),
                    next_backoff_min: 0,
                    tun_ready: applied.tun_ready,
                });
            }
            let v = check::restart(sys, paths, Vec::new(), g)?;
            wait_ready();
            Ok(v)
        })
    }
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
/// 回主菜单停不停照 [`outcome_since`]：失败、有附加行（面板退回订阅、跳过…）就停；「上次：」行
/// 一律是导入结果本身（[`Ctx::say_result`] 标的那一行），不会被它前面的附加提示占掉。追问过
/// 「切换到新导入的 X？」或墓碑那一问的，人已经在提问处看过导入结果：答 y 用切换的结果，答否不再停。
///
/// 墓碑（spec §5.7）：命中的节点先不写入，导入之后、**放锁之后**（`save_import` 里的锁已经放了，
/// spec §0.2 R11）问一句要不要加回，答 y 拿同一批节点再导一次（只导这几个，不再联网）。
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
    // 取节点与落盘分两步：墓碑那一问答 y 时要拿同一批节点再导一次，不能再去联网
    let imported = match lines.as_slice() {
        [url] if source::is_http_url(url) => fetch_http(ctx, url),
        _ => source::from_uris(&lines).map(|fetched| Incoming {
            fetched,
            src: Source::Paste,
            panel: None,
        }),
    }
    .and_then(|inc| {
        let again = inc.clone();
        save_import(ctx, inc, false, false).map(|stored| (stored, again))
    });
    let (stored, again) = match imported {
        Ok(v) => v,
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
            // 面板链接被拒（两边都 404，或不退回订阅的面板路径 404）：链接可能停用了，给出路
            let panel = matches!(lines.as_slice(), [url] if source::panel_link(url).is_some());
            if panel && link_rejected(&e) {
                ctx.say(RELINK_MENU);
            }
            return Ok(outcome_since(ctx, start));
        }
    };
    // 墓碑那一问：答 y 另拿一次锁，只把这几个导回来（spec §5.7、§0.2 R11）
    let mut asked = false;
    if !stored.buried.is_empty() {
        asked = true;
        tell(ctx, menu::buried_head(&stored.buried));
        ctx.flush();
        if ctx.prompt.confirm(menu::BURIED_ASK)? {
            let live = Profiles::load(ctx.sys, ctx.paths)?;
            let mut only = again;
            only.fetched.nodes.retain(|n| live.is_deleted(n));
            if let Err(e) = save_import(ctx, only, false, true) {
                ctx.say(format!("失败：{e}"));
                return Ok(outcome_since(ctx, start));
            }
        }
    }
    let after = Profiles::load(ctx.sys, ctx.paths)?;
    let fresh: Vec<&str> = after
        .profiles
        .iter()
        .map(|p| p.name.as_str())
        .filter(|n| !before.iter().any(|b| b == n))
        .collect();
    let mut shown = outcome_since(ctx, start);
    if asked {
        // 墓碑那一问本身就是停顿：导入结果已经在提问处看过了
        shown = asked_is_a_pause(shown);
    }
    let Some(first) = fresh.first() else {
        return Ok(shown);
    };
    if after.active.as_deref().is_some_and(|a| fresh.contains(&a)) {
        return Ok(shown); // 首次导入已经激活了新节点
    }
    ctx.flush();
    if ctx.prompt.confirm(&format!("切换到新导入的 {first}？"))? {
        return Ok(switch_node(ctx, first.to_string()));
    }
    // 提问本身就是停顿：导入结果已经在提问处看过了，答否回主菜单不再停
    Ok(asked_is_a_pause(shown))
}

/// 菜单 `[7]` → `[3]` 从 v3 导入（spec §5.7 表第 3 行、§0.2 R11）：先照常导一趟，被墓碑挡下的
/// 节点在**放锁之后**问一句，答 y 再导一次——v3 里已经导过的那些照旧按连接身份跳过，所以
/// 第二趟只会把这几个加回来。
///
/// 不走 [`run_sub`]：那条路把 [`import_v3::Report`] 丢掉了，拿不到 `buried`。失败的处理与它一样
/// （打一行「失败：…」，停不停照 [`outcome_since`]）。
fn menu_import_v3<S: Sys, N: Net, P: Prompt>(ctx: &mut Ctx<'_, S, N, P>) -> Result<Outcome> {
    let start = ctx.transcript.len();
    let buried = match import_v3_cmd(ctx, None, None, None, false) {
        Ok(r) => r.buried,
        Err(e) => {
            ctx.say(format!("失败：{e}"));
            return Ok(outcome_since(ctx, start));
        }
    };
    if buried.is_empty() {
        return Ok(outcome_since(ctx, start));
    }
    tell(ctx, menu::buried_head(&buried));
    ctx.flush();
    if !ctx.prompt.confirm(menu::BURIED_ASK)? {
        return Ok(asked_is_a_pause(outcome_since(ctx, start)));
    }
    if let Err(e) = import_v3_cmd(ctx, None, None, None, true) {
        ctx.say(format!("失败：{e}"));
    }
    Ok(outcome_since(ctx, start))
}

/// 单独一行的 http(s) 地址（菜单 `[3]`、命令行 `--sub`）：面板的四种按用户地址走
/// `/api/nodes`（带分流规则），其余当订阅地址。`/api/sub/` 在面板接口取不到时退回
/// 订阅——v3 面板（bwg-tizi）没有 `/api/nodes`，但 `/api/sub` 在。只在「取」失败时退回：面板节点取到了、
/// 后面 apply 失败时再按订阅导一遍，会把服务端分流规则换成默认表。
///
/// 面板的业务错误（`Error::Msg`，如「节点列表为空」）不退回：接口在、面板说这个用户没东西，
/// 按订阅再导一遍只会绕开这句话。
///
/// 只取、不落盘：墓碑那一问答 y 时 [`menu_import`] 要拿同一批节点再导一次，不能再联网一趟
/// （面板这会儿可能已经换了端口，第二趟会导进另一批节点）。
fn fetch_http<S: Sys, N: Net, P: Prompt>(
    ctx: &mut Ctx<'_, S, N, P>,
    url: &str,
) -> Result<Incoming> {
    match source::panel_link(url) {
        Some(link) => match fetch_panel(ctx, &link.base_url, &link.user) {
            Ok(inc) => Ok(inc),
            Err(e) if link.path == source::PanelPath::Sub && !matches!(e, Error::Msg(_)) => {
                ctx.say(panel_fallback_notice(&e));
                ctx.flush();
                fetch_sub(ctx.net, url)
            }
            Err(e) => Err(e),
        },
        None => fetch_sub(ctx.net, url),
    }
}

/// `/api/nodes` 回 401 / 404、改走订阅地址时的那句话。v3 面板没有这个接口，rc12 面板停用了
/// 用户名链接、不认的 token 也是 404，四种 404 逐字节一致：客户端分不出是哪一种，只说接下来
/// 做什么。菜单里经 `say` 原样打，2 列缩进后 40 列也要一行放下。
pub(crate) const PANEL_REJECTED: &str = "面板接口不认这个地址，改用订阅地址";

/// 面板链接两边都 401 / 404 之后的出路（菜单 `[3]`）：链接可能已经停用，重新复制整条。
/// 同样经 `say` 原样打，40 列一行放下。
pub(crate) const RELINK_MENU: &str = "从面板重新复制整条订阅链接，再贴进来";

/// 同上，命令行版：指到能从标准输入读的 `--sub -`，token 不进 shell 历史。
pub(crate) const RELINK_CLI: &str = "从面板重新复制整条订阅链接，用 bui-c import --sub - 粘贴";

/// `bui-c import` 一个来源都没给：先教粘贴整条链接，`--user` 不再只认用户名。
pub(crate) const IMPORT_NO_SOURCE: &str = "从面板复制整条订阅链接，用 bui-c import --sub - 粘贴\n\
                                           也可以 --panel <地址> --user <用户名或 token>\n\
                                           或粘贴节点链接：bui-c import -";

/// `-` 在一条命令里出现了不止一处：标准输入只有一份（用法错误，退出码 2）。
pub(crate) const STDIN_ONCE: &str = "只能有一处写 -：标准输入只有一份";

/// `--user -` / `--sub -` 从标准输入没读到东西。
pub(crate) fn stdin_empty(flag: &str) -> String {
    format!("{flag} - 没读到内容：粘贴后回车，或从管道传入")
}

/// 面板不认这个地址：401 / 404。v3 面板没有 `/api/nodes`、rc12 面板停用了用户名链接、token
/// 不对，都是它，客户端分不出来。
fn link_rejected(e: &Error) -> bool {
    matches!(e, Error::Net { detail, .. } if detail == "HTTP 401" || detail == "HTTP 404")
}

/// `/api/sub` 退回订阅时的那句话。401 / 404 只说改用订阅地址（[`PANEL_REJECTED`]），不报
/// 状态码、不猜原因；别的原因只留一句去掉 URL 的简短说明。
fn panel_fallback_notice(e: &Error) -> String {
    let reason = match e {
        e if link_rejected(e) => return PANEL_REJECTED.to_string(),
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
///
/// 重启与等接口在锁里（spec §8.3「[4] 重启」），看日志在放锁之后。
fn restart_service<S: Sys, N: Net, P: Prompt>(ctx: &mut Ctx<'_, S, N, P>, mode: Mode) {
    let ready = with_lock(ctx, |ctx, _| {
        systemd::restart(ctx.sys, UNIT_MAIN)?;
        Ok((mode == Mode::Tun).then(|| Engine::new(ctx.sys, ctx.paths).wait_tun_ready()))
    });
    match ready {
        Err(e) => ctx.say(format!("失败：{e}")),
        Ok(None) => ctx.say("已重启 bui-c.service"),
        Ok(Some(true)) => ctx.say("已重启 bui-c.service，bui-tun 已就绪"),
        Ok(Some(false)) => {
            ctx.say(format!(
                "已重启 bui-c.service，但 bui-tun 接口 {} 秒内没起来",
                crate::engine::TUN_READY_WAIT_S
            ));
            show_journal(ctx, 10);
        }
    }
}

/// 空一行、标题行，再把日志缩进 2 列打出来；取不到就说一句原因。按当前宽度排：窄屏放不下单元
/// 全名时标题用短名，每行先净化再尾截到行宽（[`nettest::journal_block`]）。
fn show_journal<S: Sys, N: Net, P: Prompt>(ctx: &mut Ctx<'_, S, N, P>, n: u32) {
    match nettest::journal_tail(ctx.sys, n) {
        Ok(lines) => {
            let block = nettest::journal_block(n, &lines, ctx.width());
            ctx.part(block);
        }
        Err(why) => ctx.say(why),
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
                activate: true,
                with_deleted: false
            })
        );
        assert_eq!(
            parse(&["import", "hysteria2://x@h:1#a"]).cmd,
            Some(Cmd::Import {
                uri: Some("hysteria2://x@h:1#a".into()),
                panel: None,
                user: None,
                sub: None,
                activate: false,
                with_deleted: false
            })
        );
        assert_eq!(
            parse(&[
                "import",
                "--sub",
                "https://s.example.test/x",
                "--with-deleted"
            ])
            .cmd,
            Some(Cmd::Import {
                uri: None,
                panel: None,
                user: None,
                sub: Some("https://s.example.test/x".into()),
                activate: false,
                with_deleted: true
            })
        );
        assert_eq!(
            parse(&["import-v3", "--with-deleted"]).cmd,
            Some(Cmd::ImportV3 {
                base: None,
                panel: None,
                mode: None,
                with_deleted: true
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
                mode: None,
                with_deleted: false
            })
        );
        assert_eq!(
            parse(&["import-v3", "--panel", "https://p", "--mode", "socks"]).cmd,
            Some(Cmd::ImportV3 {
                base: None,
                panel: Some("https://p".into()),
                mode: Some(ModeArg::Socks),
                with_deleted: false
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

    /// 菜单 `[5]` 是人手动点的：发现异常就直接修，不受 timer 的 1/2/4 分钟退避约束，
    /// 也不提「下次退避」。timer 触发的 `bui-c check` 保持原样。
    #[test]
    fn menu_check_restarts_right_away_and_does_not_talk_about_backoff() {
        let pp = paths();
        let (s, n) = (FakeSys::new(), FakeNet::new());
        check_ready(&s, &n, &pp, Mode::Socks);
        s.reply("systemctl is-active --quiet bui-c.service", 3, "");
        // 连按两次 [5]：第二次还在 timer 的退避窗口里。服务没起来有小菜单，0 回主菜单
        let mut p = Scripted::from(["5", "0", "5", "0", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert_eq!(
            s.calls()
                .iter()
                .filter(|c| *c == "systemctl restart bui-c.service")
                .count(),
            2,
            "两次都直接重启：\n{t}"
        );
        for want in [
            "  服务       ✗ bui-c.service 没在运行",
            "             已重启，3 秒内还是没起来",
        ] {
            assert_eq!(
                t.lines().filter(|l| *l == want).count(),
                2,
                "{want:?}：\n{t}"
            );
        }
        assert!(
            t.lines().any(|l| l == "  其余各项   - 跳过（服务没起）"),
            "{t}"
        );
        assert!(
            t.lines().any(|l| l == "  服务没起来，网络检查都跳过了。"),
            "{t}"
        );
        assert!(!t.contains("退避"), "手动检查不提退避：\n{t}");
        assert!(
            t.lines()
                .any(|l| l == "  上次：连接检查：失败 1 项（服务）"),
            "{t}"
        );
        assert_eq!(
            Runtime::load(&s, &pp).fail_streak,
            2,
            "手动重启照样记进 runtime.json，timer 的退避从它算起"
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

    /// 菜单 [7] → [3] 在已迁移的机器上被再按一次：v3 目录按约定保留着，`detect` 恒为真。
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
        // 内核也正是 manifest 要的版本：只换内核的 manifest 也算有东西可更新（spec §8.1）
        s.reply(
            "/opt/bui-c/bin/sing-box version",
            0,
            "sing-box version 1.14.5\n",
        );
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
        assert!(menu::render_options(&engine_status(&ctx, &prof2), 80).contains("[7] 更新与维护 ★"));

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
        assert!(!menu::render_options(&engine_status(&ctx, &prof3), 80).contains('★'));
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

    /// [7] → [1] 与 `update --check-only` 用例共用的机器：面板 profile（SOCKS）、盘上 bui-c 是
    /// [`INSTALLED`]；manifest 版本 `ver`，本机架构 bui-c 的内容 `bui_c`（`None` = manifest 里没有
    /// 这个产物），内核要 1.14.5、本机是 `kernel`（`None` = 没装内核）。两个产物都能从面板下载。
    fn update_machine(ver: &str, bui_c: Option<&str>, kernel: Option<&str>) -> (FakeSys, FakeNet) {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        let mut prof = profiles_socks();
        prof.panel = Some(crate::profiles::Panel {
            base_url: "https://panel.example.com".into(),
            username: "alice".into(),
        });
        prof.save(&s, &pp).unwrap();
        s.put(crate::paths::SELF_BIN, INSTALLED);
        match kernel {
            Some(v) => s.reply(
                "/opt/bui-c/bin/sing-box version",
                0,
                &format!("sing-box version {v}\n"),
            ),
            None => s
                .remove_file(std::path::Path::new("/opt/bui-c/bin/sing-box"))
                .unwrap(),
        }
        let arch = update::arch_suffix();
        let n = FakeNet::new();
        let mut artifacts = vec![format!(
            r#""sing-box-linux-{arch}":{{"url":"https://github.com/x/sing-box","sha256":"{}"}}"#,
            update::sha256_hex(NEW_KERNEL.as_bytes())
        )];
        n.route(
            &format!("https://panel.example.com/packages/sing-box-linux-{arch}"),
            FakeReply::Bytes(NEW_KERNEL.as_bytes().to_vec()),
        );
        if let Some(b) = bui_c {
            artifacts.push(format!(
                r#""bui-c-linux-{arch}":{{"url":"https://github.com/x/bui-c","sha256":"{}"}}"#,
                update::sha256_hex(b.as_bytes())
            ));
            n.route(
                &format!("https://panel.example.com/packages/bui-c-linux-{arch}"),
                FakeReply::Bytes(b.as_bytes().to_vec()),
            );
        }
        n.route(
            "https://panel.example.com/packages/manifest.json",
            FakeReply::Text(format!(
                r#"{{"version":"{ver}","kernels":{{"client_sing_box":"1.14.5"}},"artifacts":{{{}}}}}"#,
                artifacts.join(",")
            )),
        );
        (s, n)
    }
    const INSTALLED: &str = "bui-c-installed";
    const NEW_KERNEL: &str = "ELF-sing-box-1.14.5";

    fn last_line(t: &str) -> &str {
        t.lines()
            .rev()
            .find(|l| l.starts_with("  上次："))
            .unwrap_or_else(|| panic!("没有「上次：」行：\n{t}"))
    }

    /// `update --check-only` 的结论行按原因分开说（spec §8.1 末条）：第一行 `manifest …（来源 …）`
    /// 不变；要升级的给出命令，没法升级的不给；内核要换单独说。
    #[test]
    fn check_only_cli_line_follows_the_reason() {
        let pp = paths();
        let arch = update::arch_suffix();
        let v = crate::VERSION;
        let run = |s: &FakeSys, n: &FakeNet| {
            let mut p = Scripted::from([]);
            let mut ctx = Ctx::new(s, n, &pp, &mut p, false, false);
            dispatch(&parse(&["update", "--check-only"]), &mut ctx).unwrap();
            ctx.transcript
                .lines()
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        // (manifest 版本, manifest 里的 bui-c, 本机内核, 盘上有没有 bui-c, 结论要含, 给不给命令)
        let cases = [
            (
                v,
                Some(INSTALLED),
                "1.14.5",
                true,
                "已是最新".to_string(),
                false,
            ),
            (
                "9.9.9",
                Some("bui-c-new"),
                "1.14.5",
                true,
                "有新版".into(),
                true,
            ),
            (
                v,
                Some("bui-c-rc7"),
                "1.14.5",
                true,
                "同版本的新构建".into(),
                true,
            ),
            (
                v,
                Some("bui-c-new"),
                "1.14.5",
                false,
                "读不到本机的 bui-c".into(),
                true,
            ),
            (
                "9.9.9",
                None,
                "1.14.5",
                true,
                format!("bui-c-linux-{arch}"),
                false,
            ),
            (v, Some(INSTALLED), "1.13.19", true, "sing-box".into(), true),
        ];
        for (ver, bui_c, kernel, on_disk, want, cmd) in cases {
            let (s, n) = update_machine(ver, bui_c, Some(kernel));
            if !on_disk {
                s.remove_file(std::path::Path::new(crate::paths::SELF_BIN))
                    .unwrap();
            }
            let lines = run(&s, &n);
            assert_eq!(
                lines[0],
                format!("manifest {ver}（来源 面板）"),
                "{lines:?}"
            );
            let verdict = lines[1..].join("\n");
            assert!(verdict.contains(&want), "{want}：{lines:?}");
            assert_eq!(verdict.contains("bui-c update"), cmd, "{want}：{lines:?}");
            if want != "已是最新" {
                assert!(!verdict.contains("已是最新"), "{want}：{lines:?}");
            }
        }
        // 内核要换：说出要换成的版本
        let (s, n) = update_machine(v, Some(INSTALLED), Some("1.13.19"));
        assert!(run(&s, &n).join("\n").contains("1.14.5"));
    }

    /// manifest 里没有本机架构的 bui-c（spec §0.2 R4）：如实说没法更新，不挂 ★——挂了 ★ 进去也
    /// 什么都装不了。
    #[test]
    fn check_only_reports_missing_asset_without_star() {
        let pp = paths();
        let (s, n) = update_machine("9.9.9", None, Some("1.14.5"));
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["update", "--check-only"]), &mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert!(
            t.contains(&format!("bui-c-linux-{}", update::arch_suffix())),
            "{t}"
        );
        assert!(!t.contains("有新版"), "{t}");
        let rt = Runtime::load(&s, &pp);
        assert!(!rt.update_available, "缺产物不挂 ★：{rt:?}");
        assert_eq!(rt.update_version.as_deref(), Some("9.9.9"));
        let prof = Profiles::load(&s, &pp).unwrap();
        assert!(!menu::render_options(&engine_status(&ctx, &prof), 80).contains('★'));

        // 内核要换时照样挂 ★：只换内核的 manifest 也是「有东西可更新」
        let (s, n) = update_machine("9.9.9", None, Some("1.13.19"));
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["update", "--check-only"]), &mut ctx).unwrap();
        assert!(Runtime::load(&s, &pp).update_available);
    }

    /// 命令行 `bui-c update` 遇到缺本机架构的 bui-c：内核照换（已经下载得到），但退出码仍是失败——
    /// 脚本要知道 bui-c 这次没换成。runtime 照实落盘。
    #[test]
    fn update_with_a_missing_asset_replaces_the_kernel_and_still_fails() {
        let pp = paths();
        let (s, n) = update_machine("9.9.9", None, Some("1.13.19"));
        with_unit(&s);
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        let e = dispatch(&parse(&["update"]), &mut ctx).unwrap_err();
        let arch = update::arch_suffix();
        assert!(
            e.to_string().contains(&format!("bui-c-linux-{arch}")),
            "{e}"
        );
        assert_eq!(s.get(crate::paths::SELF_BIN).unwrap(), INSTALLED);
        assert_eq!(s.get("/opt/bui-c/bin/sing-box").unwrap(), NEW_KERNEL);
        assert!(s.called("systemctl restart bui-c.service"));
        let rt = Runtime::load(&s, &pp);
        assert!(!rt.update_available, "{rt:?}");
        assert!(rt.last_update_at.is_some(), "{rt:?}");
    }

    /// [7] → [1]（spec §8.1）：先打「正在检查更新」、再说查到了什么，有东西可更新才问 y/N；答否什么都
    /// 不换、不拿锁、不下载，「上次：」行说没有更新。子页的「上次检查」带上这次查到的版本号。
    #[test]
    fn maint_check_update_asks_before_updating_and_no_means_nothing_changes() {
        let pp = paths();
        let (s, n) = update_machine("9.9.9", Some("bui-c-new"), Some("1.14.5"));
        let r = run_menu(&s, &n, &pp, &["7", "1", "n", "7", "0", "0"], true);
        let t = &r.t;
        let at = |needle: &str| t.find(needle).unwrap_or_else(|| panic!("{needle}\n{t}"));
        assert!(at("正在检查更新") < at("9.9.9（来源 面板）"), "{t}");
        assert!(at("9.9.9（来源 面板）") < at("更新会替换 bui-c"), "{t}");
        assert_eq!(r.asked.len(), 6, "{:?}", r.asked);
        assert!(
            r.asked[2].starts_with("现在更新到") && r.asked[2].contains("9.9.9"),
            "{:?}",
            r.asked
        );
        assert_eq!(pauses(&r.asked), 0, "问过就不再停：{:?}", r.asked);
        assert_eq!(s.get(crate::paths::SELF_BIN).unwrap(), INSTALLED, "{t}");
        assert!(!s.calls().iter().any(|c| c == "lock"), "{:?}", s.calls());
        assert!(
            n.log().iter().all(|l| l.contains("manifest.json")),
            "答否不下载：{:?}",
            n.log()
        );
        let last = last_line(t);
        assert!(last.contains("没有更新") && last.contains("9.9.9"), "{t}");
        let rt = Runtime::load(&s, &pp);
        assert!(rt.update_available, "★ 照挂：{rt:?}");
        assert_eq!(rt.update_version.as_deref(), Some("9.9.9"));
        assert!(rt.last_update_at.is_none(), "{rt:?}");
        let second = t.split("\n  更新与维护\n").nth(2).unwrap();
        assert!(second.contains("有新版 9.9.9"), "{second}");
    }

    /// 同版本、sha256 不同（rc 通道重建）：说「有新构建」，不说「已是最新」，照样挂 ★、照样问。
    #[test]
    fn maint_check_update_says_rebuild_for_same_version_new_sha() {
        let pp = paths();
        let (s, n) = update_machine(crate::VERSION, Some("bui-c-rc7"), Some("1.14.5"));
        let r = run_menu(&s, &n, &pp, &["7", "1", "n", "0"], true);
        assert!(r.t.contains("有新构建"), "{}", r.t);
        assert!(!r.t.contains("已是最新"), "{}", r.t);
        assert!(
            r.asked
                .iter()
                .any(|q| q.starts_with("现在更新到") && q.contains(crate::VERSION)),
            "{:?}",
            r.asked
        );
        assert!(Runtime::load(&s, &pp).update_available);
        assert_eq!(s.get(crate::paths::SELF_BIN).unwrap(), INSTALLED);
    }

    /// 已是最新：一行 Note，不问、不停，「上次：」行写版本与来源。
    #[test]
    fn maint_check_update_up_to_date_is_a_one_line_note() {
        let pp = paths();
        let (s, n) = update_machine(crate::VERSION, Some(INSTALLED), Some("1.14.5"));
        let r = run_menu(&s, &n, &pp, &["7", "1", "0"], true);
        assert_eq!(
            r.asked,
            vec!["选择 [0-9]", "选择 [0-3]", "选择 [0-9]"],
            "{}",
            r.t
        );
        let last = last_line(&r.t);
        assert!(
            last.contains("已是最新") && last.contains(crate::VERSION),
            "{}",
            r.t
        );
        assert!(!Runtime::load(&s, &pp).update_available);
    }

    /// 答 y：拿锁装，结果停下来给人看；「上次：」行说人话（换了什么、重没重启），不是 `manifest …`
    /// 或 `自身更新=true` 这种命令行输出。换了自身要说新菜单下次打开生效。
    #[test]
    fn maint_check_update_yes_installs_and_the_last_line_is_plain_words() {
        let pp = paths();
        let (s, n) = update_machine("9.9.9", Some("bui-c-new"), Some("1.13.19"));
        with_unit(&s);
        let r = run_menu(&s, &n, &pp, &["7", "1", "y", "", "0"], true);
        let t = &r.t;
        assert_eq!(s.get(crate::paths::SELF_BIN).unwrap(), "bui-c-new", "{t}");
        assert_eq!(s.get("/opt/bui-c/bin/sing-box").unwrap(), NEW_KERNEL, "{t}");
        assert!(s.called("systemctl restart bui-c.service"), "{t}");
        assert_eq!(pauses(&r.asked), 1, "结果停下来：{:?}", r.asked);
        let last = last_line(t);
        assert!(last.contains("已更新"), "{t}");
        assert!(
            !last.contains("manifest") && !last.contains('='),
            "上次行要说人话：{last}"
        );
        assert!(t.contains("新版菜单下次打开生效"), "{t}");
        assert!(!t.contains("自身更新="), "菜单不打命令行那一行：{t}");
        let rt = Runtime::load(&s, &pp);
        assert!(!rt.update_available, "装完摘 ★：{rt:?}");
        assert!(rt.last_update_at.is_some(), "{rt:?}");
    }

    /// manifest 缺本机架构的 bui-c：不问 bui-c（问了也装不了）；内核也不用换就说完停下，
    /// 内核要换就只问内核、答 y 只换内核（spec §8.1 第 5 步）。
    #[test]
    fn maint_check_update_missing_asset_only_offers_the_kernel() {
        let pp = paths();
        let arch = update::arch_suffix();

        let (s, n) = update_machine("9.9.9", None, Some("1.14.5"));
        let r = run_menu(&s, &n, &pp, &["7", "1", "", "0"], true);
        assert_eq!(
            r.asked,
            vec!["选择 [0-9]", "选择 [0-3]", "回车返回菜单", "选择 [0-9]"],
            "{}",
            r.t
        );
        assert!(
            last_line(&r.t).contains(&format!("bui-c-linux-{arch}")),
            "{}",
            r.t
        );
        assert!(!Runtime::load(&s, &pp).update_available);

        let (s, n) = update_machine("9.9.9", None, Some("1.13.19"));
        with_unit(&s);
        let r = run_menu(&s, &n, &pp, &["7", "1", "y", "", "0"], true);
        let q = r
            .asked
            .iter()
            .find(|q| q.starts_with("现在更新"))
            .unwrap_or_else(|| panic!("{:?}", r.asked));
        assert!(q.contains("内核") && q.contains("1.14.5"), "{q}");
        assert!(r.t.contains(&format!("bui-c-linux-{arch}")), "{}", r.t);
        assert_eq!(s.get("/opt/bui-c/bin/sing-box").unwrap(), NEW_KERNEL);
        assert_eq!(s.get(crate::paths::SELF_BIN).unwrap(), INSTALLED);
        let last = last_line(&r.t);
        assert!(
            last.contains("sing-box") && !last.contains("失败"),
            "{}",
            r.t
        );
        assert!(!Runtime::load(&s, &pp).update_available);
    }

    /// 答 y 但下载失败：停下来说失败，「上次：」行是失败；盘上 bui-c 原样、★ 照挂、不算更新过，
    /// 锁拿了也放了。
    #[test]
    fn maint_check_update_install_failure_pauses_and_keeps_the_star() {
        let pp = paths();
        let (s, n) = update_machine("9.9.9", Some("bui-c-new"), Some("1.14.5"));
        n.route(
            &format!(
                "https://panel.example.com/packages/bui-c-linux-{}",
                update::arch_suffix()
            ),
            FakeReply::Status(404),
        );
        let r = run_menu(&s, &n, &pp, &["7", "1", "y", "", "0"], true);
        assert_eq!(pauses(&r.asked), 1, "{:?}", r.asked);
        assert!(last_line(&r.t).starts_with("  上次：失败："), "{}", r.t);
        assert_eq!(s.get(crate::paths::SELF_BIN).unwrap(), INSTALLED);
        let rt = Runtime::load(&s, &pp);
        assert!(rt.update_available, "没装成，★ 照挂：{rt:?}");
        assert!(rt.last_update_at.is_none(), "{rt:?}");
        let calls = s.calls();
        assert_eq!(
            (
                calls.iter().filter(|c| *c == "lock").count(),
                calls.iter().filter(|c| *c == "unlock").count()
            ),
            (1, 1),
            "{calls:?}"
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
    /// 所以 `--sub` 退回订阅（或本来就不是面板链接）时只导节点、不记 panel。`/api/nodes` 返回了
    /// 合法载荷才算 v4 面板，与 `--panel` 和菜单 `[3]` 同一个判据。
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
        // 还没进菜单：菜单走法在前（[7] 更新与维护 → [3]），命令在后
        let t2 = ctx2.transcript.clone();
        let said = t2
            .lines()
            .find(|l| l.starts_with("  已跳过"))
            .unwrap_or_else(|| panic!("{t2}"));
        let (menu_at, cmd_at) = (
            said.find("[7] 更新与维护 → [3]"),
            said.find("bui-c import-v3"),
        );
        assert!(
            menu_at.is_some() && cmd_at.is_some() && menu_at < cmd_at,
            "{said}"
        );
        assert!(!t2.contains("[7] 从 v3 导入"), "[7] 现在是更新与维护：{t2}");
        assert!(!s2.called("systemctl stop hysteria-client.service"));

        // 交互终端里这句接着就被清屏抹掉，只活在「上次：」行里：40 列尾截之后走法还在
        let s3 = FakeSys::new();
        ready(&s3);
        s3.put(
            "/opt/hysteria-client/configs/hysteria2-1/uri.txt",
            "hysteria2://alice:hy2-pw@panel.example.com:10000/?sni=panel.example.com#alice-HY2%E7%9B%B4%E8%BF%9E",
        );
        s3.put("/etc/systemd/system/hysteria-client.service", "[Unit]");
        s3.set_term_size(Some((40, 24)));
        let r = run_menu(&s3, &n, &pp, &["n", "0"], true);
        let last =
            r.t.lines()
                .find(|l| l.starts_with("  上次："))
                .unwrap_or_else(|| panic!("{}", r.t));
        assert!(
            last.contains("已跳过") && last.contains("[7] 更新与维护 → [3]"),
            "{last}"
        );
        assert!(menu::budget_width(last) <= menu::line_limit(40), "{last}");
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
        let mut p = Scripted::from(["7", "1", "", "0"]); // [7] 更新与维护 → [1] 检查更新
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert!(t.lines().any(|l| l.starts_with("  检查更新失败：")), "{t}");
        assert!(
            t.lines().any(|l| l.starts_with("  上次：检查更新失败：")),
            "{t}"
        );
        assert_eq!(
            p.asked,
            vec!["选择 [0-9]", "选择 [0-3]", "回车返回菜单", "选择 [0-9]"],
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
            r.t.lines().any(|l| l.starts_with("  上次：已跳过")),
            "{}",
            r.t
        );
    }

    /// `printf '5\n0\n' | sudo bui-c`：没人按回车，停顿不读（spec §4.4）。检查失败之后的
    /// 「下一步」小菜单与别的子提问一样会消费一行：`0` 被它当「返回菜单」读走，主菜单接着读到
    /// EOF 退出——终态与没有小菜单时一样（T11 审查裁定）。`Piped` 的 `pause` 与真 `Stdin`
    /// 在管道里的行为一致：直接返回。
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
        // T11 之后 [5] 是逐项报告：服务没起是计分项失败，报告是多行，交互终端里会停
        assert!(
            t.lines().any(|l| l.contains("bui-c.service 没在运行")),
            "多行结果，交互终端里会停：\n{t}"
        );
        assert_eq!(pauses(&p.0.asked), 0, "管道里停顿不读：{:?}", p.0.asked);
        // `0` 不是被停顿吃掉，而是被失败后的「下一步」小菜单当成「返回菜单」读走；
        // 主菜单第二次读到 EOF 退出
        assert_eq!(
            p.0.asked,
            vec!["选择 [0-9]", "选择 [0-3]", "选择 [0-9]"],
            "0 由小菜单读走，主菜单读到 EOF"
        );
        assert_eq!(t.matches("B-UI 客户端").count(), 2, "{t}");
        assert!(
            t.lines()
                .any(|l| l == "  上次：连接检查：失败 1 项（服务）"),
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

    /// spec §0.2 R11：[6] 接进主菜单之后，`bui-c -y` 进菜单删节点也照样要问，答空行不删。
    #[test]
    fn the_menu_ignores_the_global_yes_for_delete() {
        let pp = paths();
        let s = FakeSys::new();
        let n = FakeNet::new();
        nine_nodes(&s, &pp, Mode::Socks);
        let mut p = Scripted::from(["6", "1", "", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, true); // bui-c -y
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert!(ctx.yes, "出了菜单恢复原值");
        assert!(
            p.asked.iter().any(|q| q.starts_with("确认删除")),
            "{:?}",
            p.asked
        );
        assert_eq!(names(&s, &pp).len(), 9, "答空行就不删：{t}");
        assert!(
            t.lines()
                .any(|l| l == format!("  上次：{}", delete::CANCELLED)),
            "{t}"
        );
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
        // 7 = 更新与维护 → 3 = 从 v3 导入
        let mut p = Scripted::from(["7", "3", "0"]);
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

    // ───────────── T9：主菜单重排（[6] 删除节点、[7] 更新与维护、按 9 的 Note） ─────────────

    /// spec §0.2 R15：T16 之前主菜单没有 [9] 行；按 9（v4 的「自动更新 开/关」）不翻开关、
    /// 不联网，一句 Note 指到 [7] 更新与维护 → [2]。
    #[test]
    fn pressing_9_before_speedtest_points_to_maintenance() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        let mut prof = profiles_socks();
        prof.auto_update = true;
        prof.save(&s, &pp).unwrap();
        let n = FakeNet::new();
        let r = run_menu(&s, &n, &pp, &["9", "0"], true);
        assert!(
            Profiles::load(&s, &pp).unwrap().auto_update,
            "按 9 不再翻开关：{}",
            r.t
        );
        assert!(n.log().is_empty(), "不联网：{:?}", n.log());
        let last =
            r.t.lines()
                .find(|l| l.starts_with("  上次："))
                .unwrap_or_else(|| panic!("{}", r.t));
        assert!(last.contains("[7] 更新与维护 → [2]"), "{last}");
        assert_eq!(
            r.asked,
            vec!["选择 [0-9]", "选择 [0-9]"],
            "一行 Note，不停、不进子页"
        );
        assert!(!r.t.contains("[9]"), "主菜单不显示 [9] 行：{}", r.t);
    }

    /// [7] 更新与维护子页（spec §8.4、§0.2 R14）：[2] 按下即翻开关（不联网、不问），一行 Note 回
    /// 主菜单；再进子页文案跟着变，再按一次就撤回。空行、0 返回主菜单，「上次：」行不动；
    /// 输错原地重问，不重画子页。
    #[test]
    fn maint_page_toggles_auto_update_and_goes_back() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        let mut prof = profiles_socks();
        prof.auto_update = true;
        prof.save(&s, &pp).unwrap();
        let n = FakeNet::new();
        let r = run_menu(&s, &n, &pp, &["7", "x", "2", "7", "0", "7", "", "0"], true);
        assert!(!Profiles::load(&s, &pp).unwrap().auto_update, "{}", r.t);
        assert!(n.log().is_empty(), "开关不联网：{:?}", n.log());
        assert_eq!(
            r.asked,
            vec![
                "选择 [0-9]",
                "选择 [0-3]",
                "选择 [0-3]",
                "选择 [0-9]",
                "选择 [0-3]",
                "选择 [0-9]",
                "选择 [0-3]",
                "选择 [0-9]"
            ],
            "{}",
            r.t
        );
        assert_eq!(pauses(&r.asked), 0, "{:?}", r.asked);
        // 进了三次子页，每次一屏；输错那一下没有重画
        let pages: Vec<&str> = r.t.split("\n  更新与维护\n").skip(1).collect();
        assert_eq!(pages.len(), 3, "{}", r.t);
        assert!(pages[0].contains("[2] 关闭自动更新"), "{}", pages[0]);
        assert!(pages[0].contains("无效选项：x"), "{}", pages[0]);
        assert!(pages[1].contains("[2] 开启自动更新"), "{}", pages[1]);
        assert!(pages[1].contains("自动更新   关"), "{}", pages[1]);
        // 「上次：」行：翻完是结果；之后两次返回都不动它
        let lasts: Vec<&str> = r.t.lines().filter(|l| l.starts_with("  上次：")).collect();
        assert_eq!(lasts, vec!["  上次：每日自动更新：关"; 3], "{}", r.t);

        // 再按一次就撤回
        let r = run_menu(&s, &n, &pp, &["7", "2", "0"], true);
        assert!(Profiles::load(&s, &pp).unwrap().auto_update, "{}", r.t);
        assert!(
            r.t.lines().any(|l| l == "  上次：每日自动更新：开"),
            "{}",
            r.t
        );
    }

    /// 用户最初那句需求：数字菜单要能删除节点。主菜单按 6 → 选编号 → 确认 → 删掉，「上次：」行
    /// 是删除的结果；失败时停下来，节点都还在，「上次：」行写没删成。
    #[test]
    fn the_main_menu_deletes_a_node_with_6() {
        let pp = paths();
        let n = FakeNet::new();

        // Passive：删一个非活动节点，一行结果，不停
        let s = FakeSys::new();
        nine_nodes(&s, &pp, Mode::Socks);
        let r = run_menu(&s, &n, &pp, &["6", "1", "y", "0"], true);
        let left = names(&s, &pp);
        assert_eq!(left.len(), 8, "{}", r.t);
        assert!(!left.contains(&"HY2".to_string()), "{}", r.t);
        assert!(r.t.contains("删除节点（共 9 个"), "{}", r.t);
        assert!(
            r.asked
                .iter()
                .any(|q| q.starts_with("确认删除") && q.ends_with("[y/N]")),
            "{:?}",
            r.asked
        );
        assert!(r.t.lines().any(|l| l == "  上次：已删 1 个"), "{}", r.t);
        assert_eq!(pauses(&r.asked), 0, "{:?}", r.asked);

        // Switch：删当前节点，输 yes，切到替换节点；「上次：」行写切到了谁
        let s = FakeSys::new();
        nine_nodes(&s, &pp, Mode::Socks);
        let r = run_menu(&s, &n, &pp, &["6", "2", "yes", "", "0"], true);
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(saved.profiles.len(), 8, "{}", r.t);
        assert_eq!(saved.active.as_deref(), Some(RICK_REALITY), "{}", r.t);
        assert!(
            r.t.lines()
                .any(|l| l == format!("  上次：已删 1 个，切到 {RICK_REALITY}")),
            "{}",
            r.t
        );

        // 切换失败：已换回，节点都在，当前节点与配置都不变；停下来，「上次：」行写删除没做
        let s = FakeSys::new();
        nine_nodes(&s, &pp, Mode::Socks);
        let config = s.get("/opt/bui-c/config.json").unwrap();
        s.fail_write("/opt/bui-c/config.json");
        let r = run_menu(&s, &n, &pp, &["6", "2", "yes", "", "0"], true);
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(saved.profiles.len(), 9, "{}", r.t);
        assert_eq!(saved.active.as_deref(), Some(ACTIVE), "{}", r.t);
        assert_eq!(s.get("/opt/bui-c/config.json").unwrap(), config, "{}", r.t);
        assert!(r.t.contains(delete::ROLLED_BACK), "{}", r.t);
        assert_eq!(pauses(&r.asked), 1, "{:?}", r.t);
        assert!(
            r.t.lines().any(|l| l.starts_with("  上次：删除没做")),
            "{}",
            r.t
        );
    }

    /// spec §0.2 R1：[6] 删除页顶的过渡提示每个菜单会话只在第一次进这一页时显示
    /// （`Ctx.hints_shown`）；回主菜单清屏重画、再进 [6] 不再出现，新开一个会话又会显示。
    #[test]
    fn delete_page_shows_the_moved_hint_once_per_session() {
        let pp = paths();
        let s = FakeSys::new();
        let n = FakeNet::new();
        nine_nodes(&s, &pp, Mode::Socks);
        s.set_term_size(Some((40, 24)));
        let r = run_menu(&s, &n, &pp, &["6", "", "6", "0", "0"], true);
        assert_eq!(
            r.asked.iter().filter(|q| *q == "删除哪几个").count(),
            2,
            "{:?}",
            r.asked
        );
        assert!(r.clears >= 4, "两次进删除页之间清屏重画过：{}", r.clears);
        assert_eq!(r.t.matches(menu::MOVED_HINT_DELETE).count(), 1, "{}", r.t);
        // 第一次：紧跟标题，40 列一行放下
        assert!(
            r.t.contains(&format!(
                "删除节点（共 9 个，★ 为当前）\n  {}\n",
                menu::MOVED_HINT_DELETE
            )),
            "{}",
            r.t
        );
        assert_eq!(names(&s, &pp).len(), 9);

        // 新会话：又显示一次
        let r = run_menu(&s, &n, &pp, &["6", "", "0"], true);
        assert_eq!(r.t.matches(menu::MOVED_HINT_DELETE).count(), 1, "{}", r.t);

        // 没有节点时不进这一页，提示没显示过，不算数
        let s = FakeSys::new();
        ready(&s);
        let mut p = Scripted::from(["", ""]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        assert_eq!(
            delete_menu(&mut ctx).unwrap(),
            Outcome::Note(delete::NO_NODES.to_string())
        );
        nine_nodes(&s, &pp, Mode::Socks);
        assert_eq!(delete_menu(&mut ctx).unwrap(), Outcome::Nothing);
        assert_eq!(
            ctx.transcript.matches(menu::MOVED_HINT_DELETE).count(),
            1,
            "{}",
            ctx.transcript
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
        // v3 面板没有 /api/nodes 是意料之中的：不报状态码、不带 URL，免得像出了故障。
        // 这句也不说「面板没有节点接口」：rc12 面板停用的链接同样是 404，分不出来
        assert!(
            t.lines().any(|l| l == format!("  {PANEL_REJECTED}")),
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
        // 「上次：」行是导入结果本身，不是结果前面那句退回订阅的说明（T7b 审查第 1 条：
        // 结果行由 `Ctx::say_result` 标出来，`outcome_since` 不再取第一行）
        assert!(
            t.lines().any(|l| l == "  上次：导入 1 个新节点，共 2 个"),
            "{t}"
        );
    }

    /// `/api/sub` 退回订阅时按原因给不同的话：401/404 只说改用订阅地址（不猜是哪种面板），
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
            t.lines().any(|l| l == format!("  {PANEL_REJECTED}")),
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

    /// 示例订阅 token：明显是假的。rc12 面板的订阅链接末段是它，不再是用户名。
    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    /// Ttoken 接缝 1：`/api/nodes` 的 404 分不出「v3 面板没有这个接口」与「这个链接停用了 /
    /// token 不对」（服务端四种 404 逐字节一致），所以不说原因、只说接下来做什么；两边都 404
    /// 之后给出路：从面板重新复制整条链接。
    #[test]
    fn menu_import_a_rejected_panel_link_only_says_what_to_do_next() {
        let pp = paths();
        let run = |url: &str, rejected: &[&str]| {
            let s = FakeSys::new();
            ready(&s);
            profiles_socks().save(&s, &pp).unwrap();
            let n = FakeNet::new();
            for r in rejected {
                n.route(r, FakeReply::Status(404));
            }
            let r = run_menu(&s, &n, &pp, &["3", url, "", "", "0"], true);
            (r, names(&s, &pp).len())
        };
        let relink = format!("  {RELINK_MENU}");

        // 旧的用户名链接：/api/nodes 与 /api/sub 都 404
        let (r, count) = run(
            "https://panel.example.com/api/sub/alice",
            &[
                "https://panel.example.com/api/nodes/alice",
                "https://panel.example.com/api/sub/alice",
            ],
        );
        assert!(
            r.t.lines().any(|l| l == format!("  {PANEL_REJECTED}")),
            "\n{}",
            r.t
        );
        assert!(r.t.lines().any(|l| l == relink), "要给出路：\n{}", r.t);
        assert!(
            !r.t.contains("节点接口"),
            "404 分不出面板有没有接口：\n{}",
            r.t
        );
        assert!(r.t.lines().any(|l| l.starts_with("  失败：")), "{}", r.t);
        assert_eq!(count, 1, "{}", r.t);
        assert_eq!(pauses(&r.asked), 1, "失败先停：{:?}", r.asked);

        // 不退回订阅的面板路径（这里是 /api/nodes/<token>）404：同样给出路
        let url = format!("https://panel.example.com/api/nodes/{TOKEN}");
        let (r, _) = run(&url, &[url.as_str()]);
        assert!(r.t.lines().any(|l| l == relink), "\n{}", r.t);
        assert!(!r.t.contains(PANEL_REJECTED), "没有退回订阅：\n{}", r.t);

        // 第三方订阅地址 404：不是面板链接，不叫人去面板复制
        let url = "https://sub.example.com/link/abc";
        let (r, _) = run(url, &[url]);
        assert!(r.t.lines().any(|l| l.starts_with("  失败：")), "{}", r.t);
        assert!(!r.t.contains(RELINK_MENU), "\n{}", r.t);
    }

    /// 接缝 1 命令行版：`--sub <面板链接>` 两边都 404、`--panel … --user …` 404，都指到
    /// 重新复制整条链接、用 `--sub -` 粘贴。
    #[test]
    fn cli_import_a_rejected_panel_link_points_to_copying_the_whole_link() {
        let pp = paths();
        let run = |args: &[&str], rejected: &[&str]| {
            let s = FakeSys::new();
            ready(&s);
            let n = FakeNet::new();
            for r in rejected {
                n.route(r, FakeReply::Status(404));
            }
            let mut p = Scripted::from([]);
            let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
            let r = dispatch(&parse(args), &mut ctx);
            (r, ctx.transcript.clone(), names(&s, &pp).len())
        };

        let (r, t, count) = run(
            &["import", "--sub", "https://panel.example.com/api/sub/alice"],
            &[
                "https://panel.example.com/api/nodes/alice",
                "https://panel.example.com/api/sub/alice",
            ],
        );
        assert!(r.is_err(), "{t}");
        assert!(t.lines().any(|l| l == RELINK_CLI), "要给出路：\n{t}");
        assert!(!t.contains("节点接口"), "\n{t}");
        assert_eq!(count, 0, "{t}");

        let (r, t, _) = run(
            &[
                "import",
                "--panel",
                "https://panel.example.com",
                "--user",
                "alice",
            ],
            &["https://panel.example.com/api/nodes/alice"],
        );
        assert!(r.is_err(), "{t}");
        assert!(t.lines().any(|l| l == RELINK_CLI), "\n{t}");
    }

    /// 接缝 1 第 3 条：什么来源都没给时，先教粘贴整条链接；`--user` 不再只认用户名。
    /// 一个来源都没给是用法错误，退出码 2（spec §0.2 R15，与「--json 没加 -y」同类）。
    #[test]
    fn cli_import_without_a_source_teaches_pasting_the_whole_link_first() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        let n = FakeNet::new();
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        let err = dispatch(&parse(&["import"]), &mut ctx).unwrap_err();
        assert_eq!(err.exit_code(), 2, "用法错误（R15）：{err}");
        let msg = err.to_string();
        let first = msg.lines().next().unwrap_or_default();
        assert!(
            first.contains("bui-c import --sub -"),
            "先教整条链接：{msg}"
        );
        assert!(msg.contains("--user <用户名或 token>"), "{msg}");
        assert!(!msg.contains("<用户名>"), "{msg}");
        assert!(n.log().is_empty(), "{:?}", n.log());
    }

    /// Ttoken 接缝 2：`--user -` 从标准输入读 token，走面板路径；token 只惰性存进
    /// `Panel.username`，不打出来。
    #[test]
    fn cli_import_user_dash_reads_the_token_from_standard_input() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        let n = FakeNet::new();
        n.route(
            &format!("https://panel.example.com/api/nodes/{TOKEN}"),
            nodes_payload("alice", vec![reality_direct_node(), hy2_direct_node()]),
        );
        let padded = format!("  {TOKEN}  ");
        let mut p = Scripted::from([padded.as_str()]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(
            &parse(&[
                "import",
                "--panel",
                "https://panel.example.com",
                "--user",
                "-",
            ]),
            &mut ctx,
        )
        .unwrap();
        let t = ctx.transcript.clone();
        assert_eq!(p.asked.len(), 1, "读一行：{:?}", p.asked);
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(
            names(&s, &pp),
            vec!["alice-reality-direct", "alice-hy2-direct"],
            "{t}"
        );
        assert_eq!(saved.profiles[0].source, crate::profiles::Source::ApiNodes);
        assert_eq!(
            saved.panel.as_ref().map(|x| x.username.as_str()),
            Some(TOKEN),
            "去掉首尾空白后照原样存下"
        );
        assert!(!t.contains(TOKEN), "token 不打出来：\n{t}");
    }

    /// 接缝 2：`--sub -` 从标准输入读链接，走订阅路径。
    #[test]
    fn cli_import_sub_dash_reads_the_link_from_standard_input() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        let n = FakeNet::new();
        let url = format!("https://sub.example.com/link/{TOKEN}");
        n.route(&url, b64(BOB_REALITY));
        let mut p = Scripted::from([url.as_str()]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["import", "--sub", "-"]), &mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert_eq!(p.asked.len(), 1, "{:?}", p.asked);
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(saved.profiles.len(), 1, "{t}");
        assert_eq!(
            saved.profiles[0].source,
            crate::profiles::Source::Subscription
        );
        assert_eq!(saved.panel, None, "{t}");
        assert!(!t.contains(TOKEN), "\n{t}");
    }

    /// 接缝 2：标准输入只有一份，`-` 写在两处是用法错误（退出码 2），先判、一行都不读。
    #[test]
    fn cli_import_reads_standard_input_in_one_place_only() {
        let pp = paths();
        for args in [
            vec![
                "import",
                "-",
                "--panel",
                "https://panel.example.com",
                "--user",
                "-",
            ],
            vec!["import", "-", "--sub", "-"],
            vec![
                "import",
                "--panel",
                "https://panel.example.com",
                "--user",
                "-",
                "--sub",
                "-",
            ],
        ] {
            let s = FakeSys::new();
            ready(&s);
            let n = FakeNet::new();
            let mut p = Scripted::from([TOKEN, TOKEN]);
            let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
            let e = dispatch(&parse(&args), &mut ctx).unwrap_err();
            assert_eq!(e.exit_code(), 2, "{args:?}: {e}");
            assert!(e.to_string().contains("只能有一处写 -"), "{args:?}: {e}");
            assert!(p.asked.is_empty(), "{args:?}: {:?}", p.asked);
            assert!(n.log().is_empty(), "{args:?}: {:?}", n.log());
        }
    }

    /// 接缝 2：`--user -` / `--sub -` 读到 EOF 或空行就报错，不拿空值去联网。
    #[test]
    fn cli_import_dash_with_nothing_on_standard_input_is_an_error() {
        let pp = paths();
        let panel = [
            "import",
            "--panel",
            "https://panel.example.com",
            "--user",
            "-",
        ];
        let sub = ["import", "--sub", "-"];
        let cases: [(&[&str], Vec<&str>); 4] = [
            (&panel, vec![]),
            (&panel, vec![""]),
            (&sub, vec!["   "]),
            (&sub, vec![]),
        ];
        for (args, input) in cases {
            let s = FakeSys::new();
            ready(&s);
            let n = FakeNet::new();
            let mut p = Scripted {
                queue: input.iter().map(|x| x.to_string()).collect(),
                asked: Vec::new(),
                tty: false,
            };
            let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
            let e = dispatch(&parse(args), &mut ctx).unwrap_err();
            assert!(e.to_string().contains("粘贴后回车"), "{args:?}: {e}");
            assert!(n.log().is_empty(), "{args:?}: {:?}", n.log());
            assert!(names(&s, &pp).is_empty());
        }
    }

    /// 接缝 2：`bui-c import --help` 写明 `--user` / `--sub` 可以写 `-`。
    #[test]
    fn import_help_says_user_and_sub_can_read_standard_input() {
        use clap::CommandFactory as _;
        let cmd = Cli::command();
        let import = cmd.find_subcommand("import").unwrap();
        for id in ["user", "sub"] {
            let help = import
                .get_arguments()
                .find(|a| a.get_id() == id)
                .and_then(|a| a.get_help())
                .map(|h| h.to_string())
                .unwrap_or_default();
            assert!(help.contains("写 - 从标准输入读"), "--{id}：{help}");
            assert!(help.contains("shell 历史"), "--{id}：{help}");
        }
    }

    /// Ttoken 接缝 3：订阅地址末段是 token 时，profile 名里不能有它（列表截图就把订阅凭据
    /// 带出去了）。面板认这条链接就用载荷里的人名；拿不到人名退回「主机 + kind」。
    #[test]
    fn importing_a_token_subscription_never_puts_the_token_in_a_profile_name() {
        let pp = paths();
        let url = format!("https://panel.example.com/api/sub/{TOKEN}");
        let api_nodes = format!("https://panel.example.com/api/nodes/{TOKEN}");
        let no_token = |s: &FakeSys, t: &str| {
            let saved = Profiles::load(s, &pp).unwrap();
            assert!(!saved.profiles.is_empty(), "{t}");
            for p in &saved.profiles {
                assert!(!p.name.contains(TOKEN), "{}：\n{t}", p.name);
            }
            assert!(!t.contains(TOKEN), "\n{t}");
            saved
        };

        // ① 命令行 --sub，面板认 token：名字用载荷里的人名
        let s = FakeSys::new();
        ready(&s);
        let n = FakeNet::new();
        n.route(
            &api_nodes,
            nodes_payload("alice", vec![reality_direct_node()]),
        );
        n.route(&url, b64(BOB_REALITY));
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["import", "--sub", &url]), &mut ctx).unwrap();
        let saved = no_token(&s, &ctx.transcript);
        assert_eq!(names(&s, &pp), vec!["alice-reality-direct"]);
        assert_eq!(saved.profiles[0].source, crate::profiles::Source::ApiNodes);
        // 面板认这条链接（/api/nodes 合法载荷 + https）：与菜单 [3]、--panel/--user 同一判据，
        // 记成 root 自更新来源。只钉 base_url，不打印 username（它就是 token）
        assert_eq!(
            saved.panel.as_ref().map(|x| x.base_url.as_str()),
            Some("https://panel.example.com"),
            "--sub 经 /api/nodes 取到节点时记 panel"
        );

        // ② 命令行 --sub，面板接口 404、退回订阅：主机 + kind
        let s = FakeSys::new();
        ready(&s);
        let n = FakeNet::new();
        n.route(&api_nodes, FakeReply::Status(404));
        n.route(&url, b64(BOB_REALITY));
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["import", "--sub", &url]), &mut ctx).unwrap();
        let saved = no_token(&s, &ctx.transcript);
        assert_eq!(names(&s, &pp), vec!["panel.example.com-reality-direct"]);
        assert_eq!(
            saved.profiles[0].source,
            crate::profiles::Source::Subscription
        );
        // 退回订阅：订阅主机不成为 root 自更新来源
        assert!(saved.panel.is_none(), "退回订阅时不记 panel");

        // ③ 菜单 [3] 粘贴同一条链接、退回订阅：同样不露 token
        let s = FakeSys::new();
        ready(&s);
        let n = FakeNet::new();
        n.route(&api_nodes, FakeReply::Status(404));
        n.route(&url, b64(BOB_REALITY));
        let r = run_menu(&s, &n, &pp, &["3", &url, "", "", "0"], true);
        no_token(&s, &r.t);
        assert_eq!(names(&s, &pp), vec!["panel.example.com-reality-direct"]);
    }

    /// Ttoken 新文案：固定文案按容量口径 ≤ 59 列；菜单里经 `say` 原样打的两句加 2 列缩进
    /// 在 40 列也放得下（宽度守门表 `menu::tests::screens` 另按 40–100 列量一遍）。
    #[test]
    fn token_import_wording_fits_the_column_budget() {
        let fixed: Vec<String> = [RELINK_CLI, STDIN_ONCE, PANEL_REJECTED, RELINK_MENU]
            .iter()
            .map(|x| x.to_string())
            .chain([stdin_empty("--user"), stdin_empty("--sub")])
            .chain(IMPORT_NO_SOURCE.lines().map(String::from))
            .collect();
        for l in &fixed {
            let w = menu::budget_width(l);
            assert!(w <= 59, "{l} = {w}");
        }
        for l in [PANEL_REJECTED, RELINK_MENU] {
            let said = format!("  {l}");
            let w = menu::budget_width(&said);
            assert!(w <= menu::line_limit(40), "{said} = {w}");
        }
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
                "  2026-09-13T10:15:30+08:00 FATAL[0000] start service: open tun: operation no…",
            ],
            "空行 + 标题行，日志缩进 2 列、去掉主机名与 ident、去掉颜色，按行宽尾截：\n{t}"
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
        let said = |t: &str, key: &str| t.lines().any(|l| l.contains(key));
        assert!(
            said(&t, "  上次：") && said(&t, "先用 [3] 导入节点"),
            "单元不存在时应引导先导入节点：\n{t}"
        );
        assert!(!t.contains("bui-c update"), "{t}");
        assert!(
            !said(&t, "v3 客户端") && !said(&t, "→ [3]"),
            "没有 v3 目录就不提 [7] → [3]：\n{t}"
        );
        assert!(!t.contains("最近 50 行日志"), "没有单元就不进子菜单：\n{t}");
        assert!(!s.called("systemctl restart bui-c.service"));
        assert!(p.asked.iter().all(|q| q != "回车返回菜单"), "一行不停");
        // 40 列：这句只活在「上次：」行里，尾截之后可操作的那半还在
        s.set_term_size(Some((40, 24)));
        let r = run_menu(&s, &n, &pp, &["4", "0"], true);
        let last = r.t.lines().find(|l| l.starts_with("  上次：")).unwrap();
        assert!(last.contains("[3] 导入节点"), "{last}");

        // 机器上有 v3 客户端：另起一行说 [7] 更新与维护 → [3]，两行要看完，停一下
        s.set_term_size(None);
        s.put(
            "/opt/hysteria-client/configs/hysteria2-1/uri.txt",
            "hysteria2://alice:hy2-pw@panel.example.com:10000/?sni=panel.example.com#a",
        );
        let r = run_menu(&s, &n, &pp, &["4", "", "0"], true);
        assert!(said(&r.t, "先用 [3] 导入节点"), "{}", r.t);
        assert!(said(&r.t, "[7] 更新与维护 → [3] 从 v3 导入"), "\n{}", r.t);
        assert!(!r.t.contains("[7] 从 v3 导入"), "{}", r.t);
        assert_eq!(pauses(&r.asked), 1, "{:?}", r.asked);
        assert!(
            r.t.lines()
                .any(|l| l == format!("  上次：{}", menu::NO_UNITS)),
            "{}",
            r.t
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

    // ---- 菜单 [5] 连接检查（spec §6） ----

    #[test]
    fn the_timer_check_only_touches_the_probe_and_update_urls() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        n.route(crate::check::PROBE_URL, FakeReply::Status(204));
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["check"]), &mut ctx).unwrap();
        for l in n.log() {
            for u in crate::nettest::URLS {
                assert!(!l.contains(u), "timer 访问了检测站 {u}：{l}");
            }
        }
    }

    #[test]
    fn the_timer_check_never_resolves_names_even_when_it_restarts() {
        // 巡检失败、要重启时也一样：只有 204 探测与更新源，不解析域名（spec §6.7、§0.2 R13）
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        s.reply("systemctl is-active --quiet bui-c.service", 3, "");
        profiles_socks().save(&s, &pp).unwrap();
        let n = FakeNet::new();
        n.route(crate::check::PROBE_URL, FakeReply::Timeout);
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["check"]), &mut ctx).unwrap();
        assert!(s.called("systemctl restart bui-c.service"));
        assert!(
            !s.calls().iter().any(|c| c.starts_with("resolve ")),
            "{:?}",
            s.calls()
        );
        for l in n.log() {
            for u in crate::nettest::URLS {
                assert!(!l.contains(u), "timer 访问了检测站 {u}：{l}");
            }
        }
    }

    /// [5] 用的一台机器：各项都通（`nettest::sample::healthy`），自动更新关着。
    fn check_ready(s: &FakeSys, n: &FakeNet, pp: &Paths, mode: Mode) {
        ready(s);
        crate::nettest::sample::healthy(s, n, mode);
        let mut prof = match mode {
            Mode::Tun => crate::testutil::profiles_tun(),
            Mode::Socks => profiles_socks(),
        };
        prof.auto_update = false;
        prof.save(s, pp).unwrap();
    }

    #[test]
    fn menu_check_clears_first_prints_the_report_and_pauses_with_the_summary() {
        let pp = paths();
        let (s, n) = (FakeSys::new(), FakeNet::new());
        check_ready(&s, &n, &pp, Mode::Tun);
        s.set_term_size(Some((60, 30)));
        let mut p = Scripted::from(["5", "", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert_eq!(ctx.clears, 3, "进菜单、进报告、回主菜单");
        assert!(!t.contains('\u{1b}'));
        for want in [
            "  ── 连接检查（TUN 模式） ──",
            "  节点  alice-hy2-direct",
            "        HY2直连  panel.example.com:10000",
            "  服务       ✓ bui-c.service 在运行",
            "  本地端口   ✓ SOCKS5 :1080   ✓ HTTP :8080",
            "  隧道       ✓ 通  356ms",
            "  下载       ✓ 1 MB 用时 0.9 秒，约 1.1 MB/s（良好）",
            "  IPv4 出口  ✓ 203.0.113.7",
            "             IDC 机房  风险分 22（ippure）",
            "  IPv6       ✓ 已被隧道拦截，没有泄漏",
            "  全部通过（7 项），用时 0 秒",
            "  上次：连接检查：全部通过（7 项）",
        ] {
            assert!(t.lines().any(|l| l == want), "缺 {want:?}：\n{t}");
        }
        assert!(!t.contains("下一步"), "全通过不给小菜单：\n{t}");
        assert_eq!(
            p.asked,
            vec!["选择 [0-9]", "回车返回菜单", "选择 [0-9]"],
            "看完报告回车再回主菜单"
        );
    }

    #[test]
    fn menu_check_without_nodes_is_a_note_and_never_touches_the_network() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        s.set_term_size(Some((60, 30)));
        let n = FakeNet::new();
        let mut p = Scripted::from(["5", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert_eq!(ctx.clears, 2, "不进报告页：只有进菜单、回主菜单两次");
        assert!(
            t.lines()
                .any(|l| l == "  上次：没有节点可检查：先用 [3] 导入节点"),
            "{t}"
        );
        assert!(!t.contains("连接检查（"), "{t}");
        assert!(n.log().is_empty());
        assert!(!p.asked.iter().any(|q| q == "回车返回菜单"));
    }

    #[test]
    fn next_step_menu_goes_back_to_itself_after_logs() {
        let pp = paths();
        let (s, n) = (FakeSys::new(), FakeNet::new());
        check_ready(&s, &n, &pp, Mode::Socks);
        n.route(crate::check::PROBE_URL, FakeReply::Timeout);
        s.reply(
            JOURNAL_50,
            0,
            "2026-09-13T10:15:30+08:00 baiyi sing-box[4242]: ERROR[0010] connection timeout\n",
        );
        // 5 检查 → 3 看日志 → 回到小菜单 → 0 回主菜单 → 0 退出
        let mut p = Scripted::from(["5", "3", "0", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert!(s.called(JOURNAL_50), "{:?}", s.calls());
        let lines: Vec<&str> = t.lines().collect();
        let log = lines
            .iter()
            .position(|l| *l == "  ── bui-c.service 最近 50 行日志 ──")
            .unwrap_or_else(|| panic!("{t}"));
        assert_eq!(
            lines[log + 1..log + 8],
            [
                "  2026-09-13T10:15:30+08:00 ERROR[0010] connection timeout",
                "  本机能上网，是当前节点不通。",
                "  下一步：",
                "     [1] 再查一次",
                "     [2] 换个节点",
                "     [3] 看最近 50 行日志",
                "     [0] 返回菜单",
            ],
            "日志下面再给一遍判断与选项：\n{t}"
        );
        assert_eq!(t.matches("  下一步：").count(), 2, "{t}");
        assert!(
            t.lines()
                .any(|l| l == "  上次：连接检查：失败 1 项（隧道）"),
            "{t}"
        );
        assert_eq!(t.matches("B-UI 客户端").count(), 2, "{t}");
        assert_eq!(
            p.asked,
            vec!["选择 [0-9]", "选择 [0-3]", "选择 [0-3]", "选择 [0-9]"],
            "看完日志回到小菜单，不回主菜单"
        );
    }

    #[test]
    fn next_step_one_rechecks_and_two_goes_to_the_node_list() {
        let pp = paths();
        let (s, n) = (FakeSys::new(), FakeNet::new());
        check_ready(&s, &n, &pp, Mode::Socks);
        two_nodes(&s, &pp);
        n.route(crate::check::PROBE_URL, FakeReply::Timeout);
        s.set_term_size(Some((60, 30)));
        // 5 → 1 再查一次（清屏重跑）→ x 输错原地重问 → 2 换个节点 → 选 2 → 回主菜单 → 0
        let mut p = Scripted::from(["5", "1", "x", "2", "2", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert_eq!(ctx.clears, 5, "进菜单、报告、再查一次、节点列表、回主菜单");
        assert_eq!(
            t.matches("  ── 连接检查（SOCKS 模式） ──").count(),
            2,
            "{t}"
        );
        assert!(
            t.lines().any(|l| l == "  无效选项：x（请输入 0-3 的数字）"),
            "{t}"
        );
        assert!(t.lines().any(|l| l == "  切换节点"), "{t}");
        assert_eq!(
            Profiles::load(&s, &pp).unwrap().active.as_deref(),
            Some("alice-reality-direct")
        );
        assert!(
            t.lines()
                .any(|l| l == "  上次：已切到 alice-reality-direct"),
            "{t}"
        );
        assert_eq!(
            p.asked,
            vec![
                "选择 [0-9]",
                "选择 [0-3]",
                "选择 [0-3]",
                "选择 [0-3]",
                "选择节点编号",
                "选择 [0-9]"
            ]
        );
    }

    #[test]
    fn the_menu_check_no_longer_runs_the_daily_self_update() {
        let pp = paths();
        let (s, n) = (FakeSys::new(), FakeNet::new());
        check_ready(&s, &n, &pp, Mode::Socks);
        // 自动更新开着、从没更新过：timer 这一轮会去取 manifest，[5] 不该
        let mut prof = Profiles::load(&s, &pp).unwrap();
        prof.auto_update = true;
        prof.panel = Some(crate::profiles::Panel {
            base_url: "https://panel.example.com".into(),
            username: "alice".into(),
        });
        prof.save(&s, &pp).unwrap();
        assert!(crate::check::update_due(&s, &Runtime::load(&s, &pp), &prof));
        let mut p = Scripted::from(["5", "", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        assert!(
            ctx.transcript.contains("  全部通过（5 项），用时 0 秒"),
            "{}",
            ctx.transcript
        );
        assert!(
            !n.log().iter().any(|l| l.contains("manifest.json")),
            "{:?}",
            n.log()
        );
        let rt = Runtime::load(&s, &pp);
        assert_eq!(
            (rt.last_update_attempt_at, rt.last_update_at),
            (None, None),
            "连尝试都没有"
        );
    }

    #[test]
    fn menu_check_applies_instead_of_restarting_when_the_unit_file_is_missing() {
        // 有活动节点、主单元文件却不在（删光没做完）：restart 只会报 Unit not found，
        // 要 apply 把单元与配置写回来（spec §0.2 R13）
        let pp = paths();
        let (s, n) = (FakeSys::new(), FakeNet::new());
        check_ready(&s, &n, &pp, Mode::Socks);
        s.remove_file(&pp.unit(UNIT_MAIN)).unwrap();
        s.reply("systemctl is-active --quiet bui-c.service", 3, "");
        let mut p = Scripted::from(["5", "0", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        assert!(
            !s.called("systemctl restart bui-c.service"),
            "{:?}",
            s.calls()
        );
        assert!(s.exists(&pp.unit(UNIT_MAIN)), "apply 把单元写回来了");
        assert!(s.called("systemctl start bui-c.service"), "{:?}", s.calls());
    }

    #[test]
    fn the_log_page_fits_forty_columns() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        with_unit(&s);
        s.reply(JOURNAL_50, 0, crate::nettest::sample::LONG_JOURNAL);
        profiles_socks().save(&s, &pp).unwrap();
        s.set_term_size(Some((40, 30)));
        let n = FakeNet::new();
        let mut p = Scripted::from(["4", "2", "", "0"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        let lines: Vec<&str> = t.lines().collect();
        let at = lines
            .iter()
            .position(|l| *l == "  ── bui-c 最近 50 行日志 ──")
            .unwrap_or_else(|| panic!("40 列要用短标题：\n{t}"));
        for l in &lines[at..at + 4] {
            assert!(menu::budget_width(l) <= menu::line_limit(40), "{l:?}");
        }
        assert!(
            lines[at + 1].starts_with("  10:15:30 FATAL") && lines[at + 1].ends_with('…'),
            "40 列压时间戳 + 长行尾截：{}",
            lines[at + 1]
        );
    }

    // ───────────── T7b：墓碑名单（spec §5.7） ─────────────

    /// 面板导入过两个节点、删掉其中一个的机器：返回 `(FakeSys, FakeNet)`，面板还挂着那两个。
    fn one_buried(pp: &Paths, url: &str) -> (FakeSys, FakeNet) {
        let s = FakeSys::new();
        ready(&s);
        let n = FakeNet::new();
        n.route(
            url,
            nodes_payload("alice", vec![reality_direct_node(), hy2_direct_node()]),
        );
        let mut prof = Profiles::new_default();
        prof.upsert(crate::testutil::named(
            "alice-reality-direct",
            reality_direct_node(),
        ));
        prof.upsert(crate::testutil::named(
            "alice-hy2-direct",
            hy2_direct_node(),
        ));
        prof.active = Some("alice-hy2-direct".into());
        prof.save(&s, pp).unwrap();
        Engine::new(&s, pp).apply(&prof).unwrap();
        // 删掉 Reality 那个（活动节点是 hy2，所以是 Passive：不动数据面）
        let seen = delete::snapshot(&prof);
        let (r, t) = del(&s, &n, pp, &["alice-reality-direct"], None, &seen);
        r.expect(&t);
        assert_eq!(names(&s, pp), vec!["alice-hy2-direct".to_string()], "{t}");
        assert_eq!(Profiles::load(&s, pp).unwrap().deleted.len(), 1, "{t}");
        (s, n)
    }

    /// 删除成功落盘时每个被删节点记一条墓碑，**和 profiles、active 同一次 save**。
    #[test]
    fn deleting_buries_the_nodes_in_the_same_save() {
        let pp = paths();
        let s = FakeSys::new();
        let n = FakeNet::new();
        let prof = nine_nodes(&s, &pp, Mode::Socks);
        let seen = delete::snapshot(&prof);
        let writes = s.writes("/opt/bui-c/profiles.json");
        let (r, t) = del(&s, &n, &pp, &["HY2", "reality-Reality"], None, &seen);
        r.expect(&t);
        assert_eq!(
            s.writes("/opt/bui-c/profiles.json") - writes,
            1,
            "墓碑与 profiles、active 是同一次原子写：{t}"
        );
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(saved.profiles.len(), 7);
        let names: Vec<&str> = saved.deleted.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, vec!["HY2", "reality-Reality"], "{t}");
        let node_of = |name: &str| {
            prof.profiles
                .iter()
                .find(|p| p.name == name)
                .map(|p| p.node.clone())
                .unwrap()
        };
        for name in ["HY2", "reality-Reality"] {
            assert!(saved.is_deleted(&node_of(name)), "{name} 该被认出来");
            let t = saved.tombstone_of(&node_of(name)).unwrap();
            assert!(t.key.contains('|') && t.at > 0, "{t:?}");
        }
        assert!(
            !saved.is_deleted(&node_of("hysteria2-1778329470")),
            "没删的那些不许留墓碑（同账号、同主机，只有 kind 不同）"
        );
        // 落盘的字节里没有明文凭据、没有端口
        let raw = s.get("/opt/bui-c/profiles.json").unwrap();
        let doc: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let block = doc["deleted"].to_string();
        assert!(
            !block.contains("hy2-pw") && !block.contains("40000") && !block.contains("10001"),
            "墓碑里不存明文凭据、不存端口：{block}"
        );
    }

    /// 面板重新导入（刷新节点的日常操作）：删过的先跳过，菜单在**放锁之后**问一句，
    /// 答 y 只把这几个加回来、墓碑清掉（spec §5.7、§0.2 R11）。
    #[test]
    fn a_panel_reimport_skips_buried_nodes_and_the_menu_asks_to_restore() {
        let pp = paths();
        let url = "https://panel.example.com/api/nodes/alice";

        // ① 答 n：说清跳过的是哪个，节点不回来，墓碑还在
        let (s, n) = one_buried(&pp, url);
        let r = run_menu(&s, &n, &pp, &["3", url, "", "n", "0"], true);
        assert!(
            r.asked.iter().any(|q| q == menu::BURIED_ASK),
            "要问一句：{:?}",
            r.asked
        );
        assert!(
            r.t.lines()
                .any(|l| l.trim() == "这次导入里有 1 个你删过的节点：alice-reality-direct"),
            "{}",
            r.t
        );
        assert!(
            !r.t.contains("--with-deleted"),
            "菜单里不提命令行开关：{}",
            r.t
        );
        assert_eq!(
            names(&s, &pp),
            vec!["alice-hy2-direct".to_string()],
            "{}",
            r.t
        );
        assert_eq!(Profiles::load(&s, &pp).unwrap().deleted.len(), 1);
        // 「上次：」行是导入结果本身，不是它前面那几句附加提示（审查第 1 条）
        assert!(
            r.t.lines().any(|l| l == "  上次：导入 0 个新节点，共 1 个"),
            "{}",
            r.t
        );

        // ② 答 y：只把这一个加回来、墓碑清掉，而且不再联网取第二趟
        let (s, n) = one_buried(&pp, url);
        let before = n.log().len();
        let r = run_menu(&s, &n, &pp, &["3", url, "", "y", "n", "0"], true);
        let mut got = names(&s, &pp);
        got.sort();
        assert_eq!(
            got,
            vec![
                "alice-hy2-direct".to_string(),
                "alice-reality-direct".to_string()
            ],
            "{}",
            r.t
        );
        assert!(
            Profiles::load(&s, &pp).unwrap().deleted.is_empty(),
            "加回来了就清墓碑：{}",
            r.t
        );
        assert_eq!(
            n.log().len() - before,
            1,
            "第二趟拿的是同一批节点，不再联网：{:?}",
            n.log()
        );
        assert!(
            r.t.lines().any(|l| l == "  上次：导入 1 个新节点，共 2 个"),
            "{}",
            r.t
        );
    }

    /// 命令行不提问：打一行说跳了哪几个、怎么加回来；`--with-deleted` 照常导入并清墓碑。
    #[test]
    fn cli_import_prints_the_skipped_nodes_and_with_deleted_restores_them() {
        let pp = paths();
        let url = "https://panel.example.com/api/nodes/alice";
        let args = [
            "import",
            "--panel",
            "https://panel.example.com",
            "--user",
            "alice",
        ];

        // ① 默认：跳过并说清楚，一句提问都没有
        let (s, n) = one_buried(&pp, url);
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&args), &mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert!(
            t.lines().any(
                |l| l == "跳过 1 个删过的节点：alice-reality-direct（要加回用 --with-deleted）"
            ),
            "{t}"
        );
        assert!(p.asked.is_empty(), "命令行不提问：{:?}", p.asked);
        assert_eq!(names(&s, &pp), vec!["alice-hy2-direct".to_string()], "{t}");

        // ② --with-deleted：照常导入并清墓碑
        let (s, n) = one_buried(&pp, url);
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        let mut with = args.to_vec();
        with.push("--with-deleted");
        dispatch(&parse(&with), &mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert!(!t.contains("跳过 1 个删过的节点"), "{t}");
        let mut got = names(&s, &pp);
        got.sort();
        assert_eq!(
            got,
            vec![
                "alice-hy2-direct".to_string(),
                "alice-reality-direct".to_string()
            ],
            "{t}"
        );
        assert!(Profiles::load(&s, &pp).unwrap().deleted.is_empty(), "{t}");
    }

    /// 单独粘贴**一条**节点链接就是明确要它：直接加回并清墓碑，不问（spec §5.7 表第 2 行）。
    #[test]
    fn pasting_a_single_uri_restores_a_buried_node() {
        let pp = paths();
        let (s, n) = one_buried(&pp, "https://panel.example.com/api/nodes/alice");
        // BOB_REALITY 与被删的那个是同一个账号位（同 uuid、同 host、同 kind）
        let r = run_menu(&s, &n, &pp, &["3", BOB_REALITY, "", "n", "0"], true);
        assert!(
            !r.asked.iter().any(|q| q == menu::BURIED_ASK),
            "粘一条不问：{:?}",
            r.asked
        );
        assert!(
            r.t.lines().any(|l| l.trim() == menu::BURIED_RESTORED),
            "{}",
            r.t
        );
        assert_eq!(names(&s, &pp).len(), 2, "{}", r.t);
        assert!(
            Profiles::load(&s, &pp).unwrap().deleted.is_empty(),
            "加回来了就清墓碑：{}",
            r.t
        );
        assert!(!n.log().iter().any(|l| l.contains("api/nodes")), "没联网");
    }

    /// 迁移过、又删掉一个节点的机器：v3 目录按约定留着（回滚素材），`[7]` 随时会被再按一次。
    /// 返回 `(FakeSys, FakeNet, 被删节点名)`。
    fn v3_with_one_buried(pp: &Paths) -> (FakeSys, FakeNet, String) {
        let s = FakeSys::new();
        ready(&s);
        s.put(
            "/opt/hysteria-client/configs/hysteria2-1/uri.txt",
            "hysteria2://alice:hy2-pw@panel.example.com:10000/?sni=panel.example.com#alice-HY2%E7%9B%B4%E8%BF%9E",
        );
        s.put(
            "/opt/hysteria-client/configs/HY2/uri.txt",
            "hysteria2://alice:hy2-pw@panel.example.com:40000/?sni=panel.example.com#alice-HY2%E4%BD%8F%E5%AE%85",
        );
        let n = FakeNet::new();
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, pp, &mut p, false, false);
        dispatch(&parse(&["import-v3"]), &mut ctx).unwrap();
        let mut got = names(&s, pp);
        got.sort();
        assert_eq!(got, vec!["HY2".to_string(), "hysteria2-1".to_string()]);
        // 删掉非活动的那个（Passive：不动数据面）
        let prof = Profiles::load(&s, pp).unwrap();
        let target = if prof.active.as_deref() == Some("HY2") {
            "hysteria2-1"
        } else {
            "HY2"
        };
        let seen = delete::snapshot(&prof);
        let (r, t) = del(&s, &n, pp, &[target], None, &seen);
        r.expect(&t);
        assert_eq!(Profiles::load(&s, pp).unwrap().deleted.len(), 1, "{t}");
        (s, n, target.to_string())
    }

    /// `[7]` 从 v3 导入也认墓碑：默认跳过、菜单问一句，命令行 `--with-deleted` 照常导入。
    #[test]
    fn import_v3_honors_tombstones_unless_with_deleted() {
        let pp = paths();

        // ① 命令行默认：不导回来，打一行说怎么加回
        let (s, n, target) = v3_with_one_buried(&pp);
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["import-v3"]), &mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert_eq!(names(&s, &pp).len(), 1, "{t}");
        assert!(
            t.lines()
                .any(|l| l == format!("跳过 1 个删过的节点：{target}（要加回用 --with-deleted）")),
            "{t}"
        );
        assert!(p.asked.is_empty(), "命令行不提问：{:?}", p.asked);

        // ② 命令行 --with-deleted：导回来，墓碑清掉
        let (s, n, _) = v3_with_one_buried(&pp);
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["import-v3", "--with-deleted"]), &mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert_eq!(names(&s, &pp).len(), 2, "{t}");
        assert!(Profiles::load(&s, &pp).unwrap().deleted.is_empty(), "{t}");

        // ③ 菜单 [7] → [3]：问一句，答 y 才加回来
        let (s, n, target) = v3_with_one_buried(&pp);
        let r = run_menu(&s, &n, &pp, &["7", "3", "y", "", "0"], true);
        assert!(
            r.asked.iter().any(|q| q == menu::BURIED_ASK),
            "{:?}",
            r.asked
        );
        assert!(
            r.t.lines()
                .any(|l| l.trim() == format!("这次导入里有 1 个你删过的节点：{target}")),
            "{}",
            r.t
        );
        assert!(
            !r.t.contains("--with-deleted"),
            "菜单里不提命令行开关：{}",
            r.t
        );
        assert_eq!(names(&s, &pp).len(), 2, "{}", r.t);
        assert!(
            Profiles::load(&s, &pp).unwrap().deleted.is_empty(),
            "{}",
            r.t
        );

        // ④ 菜单 [7] → [3] 答 n：不加回来
        let (s, n, _) = v3_with_one_buried(&pp);
        let r = run_menu(&s, &n, &pp, &["7", "3", "n", "0"], true);
        assert_eq!(names(&s, &pp).len(), 1, "{}", r.t);
        assert_eq!(Profiles::load(&s, &pp).unwrap().deleted.len(), 1, "{}", r.t);
    }

    /// 审查第 2 条：附加行算不算、摘要取哪一句，都由打印方标出来，不在 [`outcome_since`] 里
    /// 按文案前缀猜——「当前节点：」这五个字本身不再有特权。
    #[test]
    fn outcome_since_counts_lines_by_mark_not_by_wording() {
        let pp = paths();
        let s = FakeSys::new();
        let n = FakeNet::new();
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);

        // ① 普通 say 打的「当前节点：X」照样算一行：两行 → 停
        let a = ctx.transcript.len();
        ctx.say(format!("{CURRENT_NODE_HEAD}alice-hy2-direct"));
        ctx.say("另一行");
        assert_eq!(
            outcome_since(&ctx, a),
            Outcome::Pause("当前节点：alice-hy2-direct".to_string())
        );

        // ② say_aside 标过的不算行：只剩一行 → 不停
        let b = ctx.transcript.len();
        ctx.say_aside(format!("{CURRENT_NODE_HEAD}alice-hy2-direct"));
        ctx.say("另一行");
        assert_eq!(outcome_since(&ctx, b), Outcome::Note("另一行".to_string()));

        // ③ say_result 标过的当摘要，哪怕它不是第一行
        let c = ctx.transcript.len();
        ctx.say("先打的附加提示");
        ctx.say_result("导入 2 个新节点，共 5 个");
        assert_eq!(
            outcome_since(&ctx, c),
            Outcome::Pause("导入 2 个新节点，共 5 个".to_string())
        );

        // ④ 标了不止一句结果就以最后一句为准；只剩它一行时不停
        let d = ctx.transcript.len();
        ctx.say_result("第一趟的结果");
        ctx.say_aside(format!("{CURRENT_NODE_HEAD}x"));
        assert_eq!(
            outcome_since(&ctx, d),
            Outcome::Note("第一趟的结果".to_string())
        );
        ctx.say_result("第二趟的结果");
        assert_eq!(
            outcome_since(&ctx, d),
            Outcome::Pause("第二趟的结果".to_string())
        );

        // ⑤ 「失败：」仍然最优先，哪怕标过结果行
        let e = ctx.transcript.len();
        ctx.say_result("导入 1 个新节点，共 6 个");
        ctx.say("失败：切换没做成");
        assert_eq!(
            outcome_since(&ctx, e),
            Outcome::Pause("失败：切换没做成".to_string())
        );

        // ⑥ 上一次动作标过的结果行不许漏进这一次
        let f = ctx.transcript.len();
        ctx.say("这一次只打了这一行");
        assert_eq!(
            outcome_since(&ctx, f),
            Outcome::Note("这一次只打了这一行".to_string())
        );
    }

    // ───────────── T7b 审查修复（第 1 轮） ─────────────

    /// 面板导入过两个节点、**全部删掉**的机器（spec §9：`profiles = []`、`deleted` 里有全部节点）。
    /// 面板还挂着那两个。返回 `(FakeSys, FakeNet)`。
    fn all_buried(pp: &Paths, url: &str) -> (FakeSys, FakeNet) {
        let s = FakeSys::new();
        ready(&s);
        let n = FakeNet::new();
        n.route(
            url,
            nodes_payload("alice", vec![reality_direct_node(), hy2_direct_node()]),
        );
        let mut prof = Profiles::new_default();
        prof.upsert(crate::testutil::named(
            "alice-reality-direct",
            reality_direct_node(),
        ));
        prof.upsert(crate::testutil::named(
            "alice-hy2-direct",
            hy2_direct_node(),
        ));
        prof.active = Some("alice-hy2-direct".into());
        prof.save(&s, pp).unwrap();
        Engine::new(&s, pp).apply(&prof).unwrap();
        // 删光（Empty）：stop 之后复查不再 active 才进不可回头段
        s.reply("systemctl is-active --quiet bui-c.service", 3, "");
        let seen = delete::snapshot(&prof);
        let (r, t) = del(
            &s,
            &n,
            pp,
            &["alice-reality-direct", "alice-hy2-direct"],
            None,
            &seen,
        );
        let r = r.unwrap_or_else(|e| panic!("删光应当成功：{e}\n{t}"));
        assert_eq!(r.remaining, 0, "{t}");
        let saved = Profiles::load(&s, pp).unwrap();
        assert!(saved.profiles.is_empty() && saved.active.is_none(), "{t}");
        assert_eq!(saved.deleted.len(), 2, "{t}");
        assert!(saved.panel.is_none(), "{t}");
        // 之后 start 了就是在跑
        s.reply("systemctl is-active --quiet bui-c.service", 0, "");
        (s, n)
    }

    /// 审查 C1（命令行）：删光之后从面板重新导入，全部命中墓碑——这是 spec §5.8 的恢复路。
    /// 默认跳过并说清怎么加回，退出码 0；带 `--with-deleted` 两个都回来、第一个被激活。
    #[test]
    fn reimporting_after_deleting_everything_skips_then_restores_from_the_cli() {
        let pp = paths();
        let url = "https://panel.example.com/api/nodes/alice";
        let args = [
            "import",
            "--panel",
            "https://panel.example.com",
            "--user",
            "alice",
        ];

        // ① 默认：成功返回，打出被跳过的名字；什么都没存就不改自动更新来源、不 apply
        let (s, n) = all_buried(&pp, url);
        let writes = s.writes("/opt/bui-c/profiles.json");
        let restarts_before = restarts(&s);
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        let r = dispatch(&parse(&args), &mut ctx);
        let t = ctx.transcript.clone();
        assert!(r.is_ok(), "命令行该成功（退出码 0）：{r:?}\n{t}");
        let skipped = menu::buried_skipped(&[
            "alice-reality-direct".to_string(),
            "alice-hy2-direct".to_string(),
        ]);
        assert!(skipped.contains("--with-deleted"), "前提：命令行那句带开关");
        assert!(t.lines().any(|l| l == skipped), "{t}");
        assert!(p.asked.is_empty(), "命令行不提问：{:?}", p.asked);
        assert!(!t.contains("自动更新来源改为"), "什么都没存，不换来源：{t}");
        assert!(!t.contains(CURRENT_NODE_HEAD), "没有活动节点可说：{t}");
        let saved = Profiles::load(&s, &pp).unwrap();
        assert!(saved.profiles.is_empty() && saved.active.is_none(), "{t}");
        assert_eq!(saved.deleted.len(), 2, "墓碑还在：{t}");
        assert!(saved.panel.is_none(), "什么都没存，不写 panel：{t}");
        assert_eq!(
            s.writes("/opt/bui-c/profiles.json"),
            writes,
            "什么都没变，不重写 profiles.json：{t}"
        );
        assert_eq!(restarts(&s), restarts_before, "不 apply：{t}");
        assert!(!s.exists(&pp.unit(UNIT_MAIN)), "主单元照旧不在：{t}");

        // ② --with-deleted：两个都回来，第一个被激活并 apply，墓碑清空
        let (s, n) = all_buried(&pp, url);
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        let mut with = args.to_vec();
        with.push("--with-deleted");
        let r = dispatch(&parse(&with), &mut ctx);
        let t = ctx.transcript.clone();
        assert!(r.is_ok(), "{r:?}\n{t}");
        assert!(!t.contains("跳过 "), "{t}");
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(
            names(&s, &pp),
            vec![
                "alice-reality-direct".to_string(),
                "alice-hy2-direct".to_string()
            ],
            "{t}"
        );
        assert_eq!(saved.active.as_deref(), Some("alice-reality-direct"), "{t}");
        assert!(saved.deleted.is_empty(), "{t}");
        assert_eq!(
            saved.panel.as_ref().map(|p| p.base_url.as_str()),
            Some("https://panel.example.com"),
            "{t}"
        );
        assert!(
            t.lines()
                .any(|l| l == format!("{CURRENT_NODE_HEAD}alice-reality-direct")),
            "{t}"
        );
        assert!(s.exists(&pp.unit(UNIT_MAIN)), "apply 把主单元建回来：{t}");
    }

    /// 审查 C1（菜单）：同一状态下菜单 `[3]` 贴面板地址，要问那一句；答 y 两个都回来、
    /// 一个被激活，「上次：」行是第二趟的导入结果。
    #[test]
    fn reimporting_after_deleting_everything_still_asks_in_the_menu() {
        let pp = paths();
        let url = "https://panel.example.com/api/nodes/alice";
        let (s, n) = all_buried(&pp, url);
        let r = run_menu(&s, &n, &pp, &["3", url, "", "y", "0"], true);
        assert!(
            r.asked.iter().any(|q| q == menu::BURIED_ASK),
            "要问一句：{:?}\n{}",
            r.asked,
            r.t
        );
        assert!(!r.t.contains("失败："), "{}", r.t);
        assert!(!r.t.contains("--with-deleted"), "{}", r.t);
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(saved.profiles.len(), 2, "{}", r.t);
        assert!(
            saved
                .active
                .as_deref()
                .is_some_and(|a| saved.profiles.iter().any(|p| p.name == a)),
            "活动节点是加回来的其中一个：{:?}\n{}",
            saved.active,
            r.t
        );
        assert!(saved.deleted.is_empty(), "{}", r.t);
        assert!(
            r.t.lines().any(|l| l == "  上次：导入 2 个新节点，共 2 个"),
            "{}",
            r.t
        );
    }

    /// 审查 I1：住宅换槽位后旧槽 :40000 与新槽 :40001 并存（同账号、同 host、同 kind，只差端口），
    /// 删掉旧的之后面板发来新的——它已经在列表里，照常刷新，不算「删过的」，也不问。
    #[test]
    fn a_live_profile_sharing_the_key_is_refreshed_not_reported_as_deleted() {
        let pp = paths();
        let url = "https://panel.example.com/api/nodes/alice";
        let new_slot = bui_schema::nodes::Node {
            port: 40001,
            ..crate::testutil::hy2_resi_node()
        };
        let setup = || {
            let s = FakeSys::new();
            ready(&s);
            let n = FakeNet::new();
            let relabeled = bui_schema::nodes::Node {
                label: "HY2住宅-新槽".into(),
                ..new_slot.clone()
            };
            n.route(
                url,
                nodes_payload("alice", vec![hy2_direct_node(), relabeled]),
            );
            let mut prof = Profiles::new_default();
            prof.upsert(crate::testutil::named(
                "alice-hy2-direct",
                hy2_direct_node(),
            ));
            prof.upsert(crate::testutil::named(
                "alice-hy2-resi",
                crate::testutil::hy2_resi_node(),
            ));
            prof.upsert(crate::testutil::named("alice-hy2-resi-2", new_slot.clone()));
            prof.active = Some("alice-hy2-direct".into());
            prof.save(&s, &pp).unwrap();
            Engine::new(&s, &pp).apply(&prof).unwrap();
            let seen = delete::snapshot(&prof);
            let (r, t) = del(&s, &n, &pp, &["alice-hy2-resi"], None, &seen);
            r.expect(&t);
            let saved = Profiles::load(&s, &pp).unwrap();
            assert_eq!(saved.deleted.len(), 1, "{t}");
            assert!(saved.is_deleted(&new_slot), "前提：新槽与墓碑同 key");
            (s, n)
        };

        // 命令行
        let (s, n) = setup();
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        let r = dispatch(
            &parse(&[
                "import",
                "--panel",
                "https://panel.example.com",
                "--user",
                "alice",
            ]),
            &mut ctx,
        );
        let t = ctx.transcript.clone();
        assert!(r.is_ok(), "{r:?}\n{t}");
        assert!(!t.contains("跳过 "), "活着的节点不许当成删过的：{t}");
        assert!(
            t.lines().any(|l| l == "更新节点 alice-hy2-resi-2"),
            "活着的节点照常刷新：{t}"
        );
        let saved = Profiles::load(&s, &pp).unwrap();
        let live = saved
            .profiles
            .iter()
            .find(|p| p.name == "alice-hy2-resi-2")
            .unwrap_or_else(|| panic!("{t}"));
        assert_eq!(live.node.label, "HY2住宅-新槽", "{t}");
        assert!(
            !saved.profiles.iter().any(|p| p.name == "alice-hy2-resi"),
            "删掉的旧槽不回来：{t}"
        );
        assert!(
            !saved.is_deleted(&new_slot),
            "过期墓碑顺手清掉：{:?}\n{t}",
            saved.deleted
        );

        // 菜单：不问那一句，再刷新一次也不问
        let (s, n) = setup();
        let r = run_menu(
            &s,
            &n,
            &pp,
            &["3", url, "", "", "3", url, "", "", "0"],
            true,
        );
        assert_eq!(
            r.asked.iter().filter(|q| *q == PASTE_PROMPT).count(),
            2,
            "前提：真的导入了两趟：{:?}\n{}",
            r.asked,
            r.t
        );
        assert_eq!(
            n.log().iter().filter(|l| l.contains("api/nodes")).count(),
            2,
            "{:?}",
            n.log()
        );
        assert!(
            !r.asked.iter().any(|q| q == menu::BURIED_ASK),
            "活着的节点不问：{:?}\n{}",
            r.asked,
            r.t
        );
        assert!(!r.t.contains("你删过的节点"), "{}", r.t);
    }

    /// 审查 M1：节点存下了、apply 才失败时，被跳过的名字与 `--with-deleted` 那句照样要打出来；
    /// 终态：返回错误，墓碑还在，旧配置不动。
    #[test]
    fn cli_import_still_lists_skipped_nodes_when_apply_fails() {
        let pp = paths();
        let url = "https://panel.example.com/api/nodes/alice";
        let (s, n) = one_buried(&pp, url);
        let config = s.get("/opt/bui-c/config.json").unwrap();
        s.reply(
            "/opt/bui-c/bin/sing-box check -c /opt/bui-c/.config.json.new",
            1,
            "outbounds[0]: 解析失败",
        );
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        let r = dispatch(
            &parse(&[
                "import",
                "--panel",
                "https://panel.example.com",
                "--user",
                "alice",
                "--activate",
            ]),
            &mut ctx,
        );
        let t = ctx.transcript.clone();
        assert!(r.is_err(), "apply 失败要报错（退出码非 0）：{t}");
        let skipped = menu::buried_skipped(&["alice-reality-direct".to_string()]);
        assert!(
            t.lines().any(|l| l == skipped),
            "失败时也要说清跳了哪几个：{t}"
        );
        assert!(p.asked.is_empty(), "{:?}", p.asked);
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(names(&s, &pp), vec!["alice-hy2-direct".to_string()], "{t}");
        assert_eq!(saved.deleted.len(), 1, "墓碑还在：{t}");
        assert_eq!(
            s.get("/opt/bui-c/config.json").unwrap(),
            config,
            "校验没过，旧配置不动：{t}"
        );
    }

    // ───────────── T7b 审查修复（第 2 轮） ─────────────

    /// 没迁移的 v3 客户端、所有 v3 单元文件的完整路径（三个主单元 + 两个 health 残留）。
    fn v3_unit_files(pp: &Paths) -> Vec<PathBuf> {
        import_v3::V3_UNITS
            .iter()
            .chain(import_v3::V3_AUX_UNITS.iter())
            .map(|u| pp.unit(u))
            .collect()
    }

    /// 审查 r2 I1 的机器：[`all_buried`]（面板导入过两个节点、全部删掉），外加一个**没迁移**的
    /// v3 客户端——目录里是同一个账号的两个节点（与两条墓碑同 key），五个 v3 单元文件都在。
    /// 到这个状态只要：首次邀请答 n、改走面板导入、后来删光（spec §9）。
    fn all_buried_with_v3(pp: &Paths, url: &str) -> (FakeSys, FakeNet) {
        let (s, n) = all_buried(pp, url);
        s.put(
            "/opt/hysteria-client/configs/hysteria2-1/uri.txt",
            "hysteria2://alice:hy2-pw@panel.example.com:10000/?sni=panel.example.com#alice-HY2%E7%9B%B4%E8%BF%9E",
        );
        s.put(
            "/opt/hysteria-client/configs/reality-1/uri.txt",
            BOB_REALITY,
        );
        s.put("/opt/hysteria-client/active", "hysteria2-1");
        for f in v3_unit_files(pp) {
            s.put(f.to_str().unwrap(), "[Unit]");
        }
        // 前提：v3 的两个节点都与墓碑同 key（面板节点与 v3 节点同账号、同 host、同 kind）
        let prof = Profiles::load(&s, pp).unwrap();
        for dir in ["hysteria2-1", "reality-1"] {
            let uri = s
                .get(&format!("/opt/hysteria-client/configs/{dir}/uri.txt"))
                .unwrap();
            let node = bui_schema::parse::node_uri(&uri).unwrap();
            assert!(prof.is_deleted(&node), "前提：v3 的 {dir} 命中墓碑");
        }
        (s, n)
    }

    /// 墓碑里记的名字，按 v3 目录名排序后的顺序（`hysteria2-1` 在 `reality-1` 前）。
    fn v3_buried_names() -> Vec<String> {
        vec![
            "alice-hy2-direct".to_string(),
            "alice-reality-direct".to_string(),
        ]
    }

    /// 最后一条「上次：」行（主菜单每重画一次打一条）。
    fn last_summary(t: &str) -> Option<&str> {
        t.lines().rev().find(|l| l.starts_with("  上次："))
    }

    /// 审查 r2 I1（命令行）：删光之后 v3 的节点全部命中墓碑、v3 单元还在——默认成功返回并打出
    /// 跳过行，v3 一字不动；`--with-deleted` 两个都回来、第一个被激活、墓碑清空、v3 单元卸掉。
    #[test]
    fn import_v3_after_deleting_everything_with_v3_units_left_skips_then_restores() {
        let pp = paths();
        let url = "https://panel.example.com/api/nodes/alice";

        // ① 默认：退出码 0，说清跳了哪几个、怎么加回；什么都不存、不 apply、不动 v3
        let (s, n) = all_buried_with_v3(&pp, url);
        let writes = s.writes("/opt/bui-c/profiles.json");
        let restarts_before = restarts(&s);
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        let r = dispatch(&parse(&["import-v3"]), &mut ctx);
        let t = ctx.transcript.clone();
        assert!(r.is_ok(), "命令行该成功（退出码 0）：{r:?}\n{t}");
        let skipped = menu::buried_skipped(&v3_buried_names());
        assert!(skipped.contains("--with-deleted"), "前提：命令行那句带开关");
        assert!(t.lines().any(|l| l == skipped), "{t}");
        assert!(p.asked.is_empty(), "命令行不提问：{:?}", p.asked);
        assert!(!t.contains(CURRENT_NODE_HEAD), "没有活动节点可说：{t}");
        let saved = Profiles::load(&s, &pp).unwrap();
        assert!(saved.profiles.is_empty() && saved.active.is_none(), "{t}");
        assert_eq!(saved.deleted.len(), 2, "墓碑还在：{t}");
        assert_eq!(
            s.writes("/opt/bui-c/profiles.json"),
            writes,
            "什么都没存，不重写 profiles.json：{t}"
        );
        assert_eq!(restarts(&s), restarts_before, "不 apply：{t}");
        for f in v3_unit_files(&pp) {
            assert!(s.exists(&f), "v3 单元原样保留：{}\n{t}", f.display());
        }
        assert!(
            !s.called("systemctl stop hysteria-client.service"),
            "不卸 v3：{t}"
        );

        // ② --with-deleted：两个都回来，v3 的活动节点被激活并 apply，墓碑清空，v3 单元卸掉
        let (s, n) = all_buried_with_v3(&pp, url);
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        let r = dispatch(&parse(&["import-v3", "--with-deleted"]), &mut ctx);
        let t = ctx.transcript.clone();
        assert!(r.is_ok(), "{r:?}\n{t}");
        assert!(!t.contains("--with-deleted"), "{t}");
        assert!(p.asked.is_empty(), "{:?}", p.asked);
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(
            names(&s, &pp),
            vec!["hysteria2-1".to_string(), "reality-1".to_string()],
            "{t}"
        );
        assert_eq!(saved.active.as_deref(), Some("hysteria2-1"), "{t}");
        assert!(saved.deleted.is_empty(), "{t}");
        assert!(
            t.lines()
                .any(|l| l == format!("{CURRENT_NODE_HEAD}hysteria2-1")),
            "{t}"
        );
        for f in v3_unit_files(&pp) {
            assert!(!s.exists(&f), "v3 单元该卸掉：{}\n{t}", f.display());
        }
        assert!(s.exists(&pp.unit(UNIT_MAIN)), "apply 把主单元建回来：{t}");
    }

    /// 审查 r2 I1（菜单）：同一状态下，先拒邀请再按 `[7] 更新与维护 → [3]`、或邀请直接答 y，
    /// 都要问墓碑那一句；答 y 两个都回来、不报失败、不提命令行开关，「上次：」行是导入结果。
    #[test]
    fn the_menu_asks_about_buried_v3_nodes_after_deleting_everything() {
        let pp = paths();
        let url = "https://panel.example.com/api/nodes/alice";
        for inputs in [&["n", "7", "3", "y", "", "0"][..], &["y", "y", "", "0"][..]] {
            let (s, n) = all_buried_with_v3(&pp, url);
            let r = run_menu(&s, &n, &pp, inputs, true);
            let ctx_msg = format!("{inputs:?}\n{:?}\n{}", r.asked, r.t);
            assert!(
                r.asked.iter().any(|q| q.contains("导入 v3 的节点")),
                "前提：进菜单前邀请过：{ctx_msg}"
            );
            assert!(
                r.asked.iter().any(|q| q == menu::BURIED_ASK),
                "要问一句：{ctx_msg}"
            );
            assert!(
                r.t.lines()
                    .any(|l| l.trim() == menu::buried_head(&v3_buried_names())),
                "{ctx_msg}"
            );
            assert!(!r.t.contains("失败："), "{ctx_msg}");
            assert!(
                !r.t.contains("--with-deleted"),
                "菜单里不提命令行开关：{ctx_msg}"
            );
            let saved = Profiles::load(&s, &pp).unwrap();
            assert_eq!(
                names(&s, &pp),
                vec!["hysteria2-1".to_string(), "reality-1".to_string()],
                "{ctx_msg}"
            );
            assert_eq!(saved.active.as_deref(), Some("hysteria2-1"), "{ctx_msg}");
            assert!(saved.deleted.is_empty(), "{ctx_msg}");
            for f in v3_unit_files(&pp) {
                assert!(!s.exists(&f), "v3 单元该卸掉：{}\n{ctx_msg}", f.display());
            }
            assert!(
                last_summary(&r.t).is_some_and(|l| l.starts_with("  上次：导入 2 个节点")),
                "「上次：」行是导入结果：{:?}\n{ctx_msg}",
                last_summary(&r.t)
            );
        }
    }

    /// 审查 r2 M6：菜单 `[3]` 一次粘多条、其中一条删过，答 y 之后第二趟只剩那一条——它不是
    /// 「单独粘贴一条」（spec §5.7 表第 1 行），「上次：」行是导入计数。
    #[test]
    fn restoring_one_of_several_pasted_links_summarizes_with_the_count() {
        let pp = paths();
        let (s, n) = one_buried(&pp, "https://panel.example.com/api/nodes/alice");
        // BOB_REALITY 与被删的那个同一个账号位；bob 的 HY2 是全新的
        let bob_hy2 =
            "hysteria2://bob:bob-pw@panel.example.com:10000/?sni=panel.example.com#bob-HY2";
        let r = run_menu(
            &s,
            &n,
            &pp,
            &["3", BOB_REALITY, bob_hy2, "", "y", "n", "0"],
            true,
        );
        assert!(
            r.asked.iter().any(|q| q == menu::BURIED_ASK),
            "前提：多条粘贴要问：{:?}\n{}",
            r.asked,
            r.t
        );
        let saved = Profiles::load(&s, &pp).unwrap();
        assert_eq!(saved.profiles.len(), 3, "{}", r.t);
        assert!(saved.deleted.is_empty(), "加回来了就清墓碑：{}", r.t);
        assert_eq!(
            last_summary(&r.t),
            Some("  上次：导入 1 个新节点，共 3 个"),
            "{}",
            r.t
        );
    }

    // ───────────── T12a：进程锁（spec §8.3、§5.9、§0.2 R11） ─────────────

    /// 把每次提问也记进 FakeSys 的调用流水（`ask <提示>`），好和 lock / unlock 排先后。
    struct LoggingPrompt<'a> {
        inner: Scripted,
        sys: &'a FakeSys,
    }

    impl<'a> LoggingPrompt<'a> {
        fn new(sys: &'a FakeSys, inputs: &[&str]) -> Self {
            Self {
                inner: Scripted {
                    queue: inputs.iter().map(|x| x.to_string()).collect(),
                    asked: Vec::new(),
                    tty: true,
                },
                sys,
            }
        }
    }

    impl Prompt for LoggingPrompt<'_> {
        fn interactive(&self) -> bool {
            self.inner.interactive()
        }
        fn read(&mut self, prompt: &str) -> Result<Option<String>> {
            self.sys.mark(format!("ask {prompt}"));
            self.inner.read(prompt)
        }
        fn lines_until_blank(&mut self, prompt: &str) -> Result<Vec<String>> {
            self.sys.mark(format!("ask {prompt}"));
            self.inner.lines_until_blank(prompt)
        }
        fn confirm(&mut self, prompt: &str) -> Result<bool> {
            self.sys.mark(format!("ask {prompt}"));
            self.inner.confirm(prompt)
        }
    }

    /// 核对调用流水：lock 与 unlock 一一配对、不嵌套、最后放掉，两者之间没有任何提问。
    /// 返回 `from` 之后拿过几次锁。
    fn no_prompt_under_lock(s: &FakeSys, from: usize, what: &str) -> usize {
        let calls = s.calls();
        let mut held = false;
        let mut taken = 0;
        for c in &calls[from..] {
            match c.as_str() {
                "lock" => {
                    assert!(!held, "{what}：嵌套拿锁：{calls:?}");
                    held = true;
                    taken += 1;
                }
                "unlock" => {
                    assert!(held, "{what}：没拿锁就放锁：{calls:?}");
                    held = false;
                }
                ask if ask.starts_with("ask ") => {
                    assert!(!held, "{what}：持锁期间提问了「{ask}」：{calls:?}")
                }
                _ => {}
            }
        }
        assert!(!held, "{what}：锁没放：{calls:?}");
        taken
    }

    fn logged_menu(s: &FakeSys, n: &FakeNet, pp: &Paths, inputs: &[&str]) -> (String, Vec<String>) {
        let mut p = LoggingPrompt::new(s, inputs);
        let mut ctx = Ctx::new(s, n, pp, &mut p, false, false);
        menu_loop(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        (t, p.inner.asked)
    }

    #[test]
    fn no_prompt_is_ever_asked_while_holding_the_lock() {
        let pp = paths();

        // [1] 切换：选完编号后拿锁、apply、放锁
        let (s, n) = (FakeSys::new(), FakeNet::new());
        ready(&s);
        two_nodes(&s, &pp);
        let (t, _) = logged_menu(&s, &n, &pp, &["1", "2", "0"]);
        assert_eq!(no_prompt_under_lock(&s, 0, "[1] 切换"), 1, "{t}");
        assert_eq!(
            Profiles::load(&s, &pp).unwrap().active.as_deref(),
            Some("alice-reality-direct"),
            "{t}"
        );

        // [2] 切模式：答 y 之后才拿锁
        let (s, n) = (FakeSys::new(), FakeNet::new());
        ready(&s);
        two_nodes(&s, &pp);
        let (t, _) = logged_menu(&s, &n, &pp, &["2", "y", "", "0"]);
        assert_eq!(no_prompt_under_lock(&s, 0, "[2] 切模式"), 1, "{t}");
        assert_eq!(Profiles::load(&s, &pp).unwrap().mode, Mode::Tun, "{t}");

        // [3] 导入：锁内 save_import + apply；放锁之后才问墓碑那一问与「切换到新导入的 X？」，
        // 答 y 另拿一次锁
        let url = "https://panel.example.com/api/nodes/alice";
        let (s, n) = one_buried(&pp, url);
        let from = s.calls().len();
        let (t, asked) = logged_menu(&s, &n, &pp, &["3", url, "", "y", "n", "0"]);
        assert!(asked.iter().any(|q| q == menu::BURIED_ASK), "{asked:?}");
        assert!(
            asked.iter().any(|q| q.starts_with("切换到新导入的")),
            "{asked:?}"
        );
        assert_eq!(no_prompt_under_lock(&s, from, "[3] 导入"), 2, "{t}");
        assert_eq!(names(&s, &pp).len(), 2, "{t}");

        // [4] 重启
        let (s, n) = (FakeSys::new(), FakeNet::new());
        ready(&s);
        with_unit(&s);
        two_nodes(&s, &pp);
        let (t, _) = logged_menu(&s, &n, &pp, &["4", "1", "0"]);
        assert_eq!(no_prompt_under_lock(&s, 0, "[4] 重启"), 1, "{t}");
        assert!(s.called("systemctl restart bui-c.service"), "{t}");

        // [5] 只在修复（重启、等就绪）那一段拿锁，小菜单的提问在锁外
        let (s, n) = (FakeSys::new(), FakeNet::new());
        check_ready(&s, &n, &pp, Mode::Socks);
        s.reply("systemctl is-active --quiet bui-c.service", 3, "");
        let (t, asked) = logged_menu(&s, &n, &pp, &["5", "0", "0"]);
        assert!(asked.iter().any(|q| q == "选择 [0-3]"), "{asked:?}");
        assert_eq!(no_prompt_under_lock(&s, 0, "[5] 修复"), 1, "{t}");

        // [6] 删除：确认之后才拿锁（Switch 形态，要输入 yes）
        let (s, n) = (FakeSys::new(), FakeNet::new());
        let prof = nine_nodes(&s, &pp, Mode::Socks);
        let idx = prof.profiles.iter().position(|p| p.name == ACTIVE).unwrap() + 1;
        let idx = idx.to_string();
        let from = s.calls().len();
        let mut p = LoggingPrompt::new(&s, &[idx.as_str(), "yes"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        delete_menu(&mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert_eq!(no_prompt_under_lock(&s, from, "[6] 删除"), 1, "{t}");
        assert_eq!(names(&s, &pp).len(), 8, "{t}");

        // 命令行 delete：终端里的确认也在拿锁之前
        let (s, n) = (FakeSys::new(), FakeNet::new());
        nine_nodes(&s, &pp, Mode::Socks);
        let from = s.calls().len();
        let mut p = LoggingPrompt::new(&s, &["yes"]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["delete", ACTIVE]), &mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert_eq!(no_prompt_under_lock(&s, from, "bui-c delete"), 1, "{t}");
        assert_eq!(names(&s, &pp).len(), 8, "{t}");

        // [7] → [1] 检查更新：检查与提问在锁外，答 y 之后才拿锁装；结果的停顿在放锁之后
        let (s, n) = update_machine("9.9.9", Some("bui-c-new"), Some("1.14.5"));
        let (t, asked) = logged_menu(&s, &n, &pp, &["7", "1", "y", "", "0"]);
        assert!(
            asked.iter().any(|q| q.starts_with("现在更新到")),
            "{asked:?}"
        );
        assert_eq!(no_prompt_under_lock(&s, 0, "[7] → [1] 检查更新"), 1, "{t}");
        assert_eq!(s.get(crate::paths::SELF_BIN).unwrap(), "bui-c-new", "{t}");

        // [8] 卸载：确认在菜单层做完，只在 uninstall::run 外面拿锁
        let (s, n) = (FakeSys::new(), FakeNet::new());
        ready(&s);
        with_unit(&s);
        two_nodes(&s, &pp);
        let (t, _) = logged_menu(&s, &n, &pp, &["8", "y", "0"]);
        assert_eq!(no_prompt_under_lock(&s, 0, "[8] 卸载"), 1, "{t}");
        assert!(!s.exists(&pp.profiles()), "{t}");
    }

    #[test]
    fn with_lock_waits_then_gives_up_after_15_seconds() {
        let pp = paths();
        let n = FakeNet::new();

        // 一直被占：打一行「在等」并冲出去，等满 15 秒，什么都不做
        let s = FakeSys::new();
        s.lock_busy(u32::MAX);
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        let mut ran = false;
        let e = with_lock(&mut ctx, |_, _| {
            ran = true;
            Ok(())
        })
        .unwrap_err();
        assert!(!ran, "没拿到锁就不能动手");
        let msg = e.to_string();
        assert!(
            msg.contains("什么都没改") && msg.contains("稍后再试"),
            "{msg}"
        );
        assert_eq!(e.exit_code(), 1, "锁等不到是执行失败（R15）");
        let t = ctx.transcript.clone();
        assert_eq!(
            t.lines().filter(|l| l.contains("等它结束")).count(),
            1,
            "只在第一次没拿到时说一次：\n{t}"
        );
        assert!(ctx.out.is_empty(), "等之前就冲出去了，人看得到");
        let sleeps = s.sleeps();
        assert!(sleeps.iter().all(|&ms| ms == 250), "{sleeps:?}");
        assert_eq!(sleeps.iter().sum::<u64>(), 15_000);
        assert!(!s.called("lock"));

        // 等了两拍拿到：还是只说一次，`f` 在 lock 与 unlock 之间跑
        let s = FakeSys::new();
        s.lock_busy(2);
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        with_lock(&mut ctx, |ctx, _| {
            ctx.sys.mark("inside".to_string());
            Ok(())
        })
        .unwrap();
        assert_eq!(s.calls(), vec!["lock", "inside", "unlock"]);
        assert_eq!(
            ctx.transcript
                .lines()
                .filter(|l| l.contains("等它结束"))
                .count(),
            1
        );

        // 一上来就拿到：一个字都不打
        let s = FakeSys::new();
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        with_lock(&mut ctx, |_, _| Ok(())).unwrap();
        assert!(ctx.transcript.is_empty(), "{}", ctx.transcript);
        assert!(s.sleeps().is_empty());
    }

    #[test]
    fn the_lock_lines_fit_forty_columns() {
        let pp = paths();
        let n = FakeNet::new();
        let s = FakeSys::new();
        s.set_term_size(Some((40, 24)));
        s.lock_busy(u32::MAX);
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        let e = with_lock(&mut ctx, |_, _| Ok(())).unwrap_err();
        assert!(ctx.transcript.contains("等它结束"), "{}", ctx.transcript);
        for l in ctx.transcript.lines() {
            assert!(menu::budget_width(l) <= menu::line_limit(40), "{l:?}");
        }
        assert!(menu::budget_width(&e.to_string()) <= 59, "{e}");
        // `--json` 下 stdout 只放一个对象：在等的那一行不打
        let s = FakeSys::new();
        s.lock_busy(1);
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, true, true);
        with_lock(&mut ctx, |_, _| Ok(())).unwrap();
        assert!(ctx.transcript.is_empty(), "{}", ctx.transcript);
    }

    #[test]
    fn delete_holds_the_lock_around_the_restart() {
        let pp = paths();
        let (s, n) = (FakeSys::new(), FakeNet::new());
        let prof = nine_nodes(&s, &pp, Mode::Tun);
        let from = s.calls().len();
        let seen = delete::snapshot(&prof);
        let (r, t) = del(&s, &n, &pp, &[ACTIVE], None, &seen);
        r.expect(&t);
        let calls = s.calls()[from..].to_vec();
        let at = |c: &str| {
            calls
                .iter()
                .position(|x| x == c)
                .unwrap_or_else(|| panic!("没有 {c}：{calls:?}"))
        };
        assert!(at("lock") < at("ip link delete bui-tun"), "{calls:?}");
        assert!(
            at("ip link delete bui-tun") < at("systemctl restart bui-c.service"),
            "{calls:?}"
        );
        assert!(
            at("systemctl restart bui-c.service") < at("unlock"),
            "{calls:?}"
        );
        assert_eq!(no_prompt_under_lock(&s, from, "删除"), 1);
    }

    #[test]
    fn a_busy_lock_leaves_every_changing_command_untouched_and_exits_1() {
        let pp = paths();
        let n = FakeNet::new();
        let busy = |s: &FakeSys, args: &[&str], answers: &[&str]| {
            s.lock_busy(u32::MAX);
            let mut p = Scripted {
                queue: answers.iter().map(|x| x.to_string()).collect(),
                asked: Vec::new(),
                tty: true,
            };
            let mut ctx = Ctx::new(s, &n, &pp, &mut p, false, args.contains(&"-y"));
            let r = dispatch(&parse(args), &mut ctx);
            let mut err = Vec::new();
            let rc = finish(&mut ctx, r, &mut err);
            s.lock_busy(0);
            (rc, String::from_utf8(err).unwrap())
        };

        // switch：active 不变、没重启、profiles.json 一次都没写
        let s = FakeSys::new();
        ready(&s);
        two_nodes(&s, &pp);
        let writes = s.writes("/opt/bui-c/profiles.json");
        let (rc, err) = busy(&s, &["switch", "alice-reality-direct"], &[]);
        assert_eq!(rc, std::process::ExitCode::FAILURE);
        assert!(err.contains("稍后再试"), "{err}");
        assert_eq!(s.writes("/opt/bui-c/profiles.json"), writes);
        assert_eq!(
            Profiles::load(&s, &pp).unwrap().active.as_deref(),
            Some("alice-hy2-direct")
        );
        assert!(!s.called("systemctl restart bui-c.service"));

        // mode：模式不变
        let (rc, err) = busy(&s, &["mode", "tun"], &[]);
        assert_eq!(rc, std::process::ExitCode::FAILURE, "{err}");
        assert_eq!(Profiles::load(&s, &pp).unwrap().mode, Mode::Socks);
        assert_eq!(s.writes("/opt/bui-c/profiles.json"), writes);

        // import：节点一个没多
        let (rc, err) = busy(&s, &["import", "-"], &[BOB_REALITY, ""]);
        assert_eq!(rc, std::process::ExitCode::FAILURE, "{err}");
        assert_eq!(names(&s, &pp).len(), 2);

        // update --auto：开关不变
        let (rc, err) = busy(&s, &["update", "--auto", "off"], &[]);
        assert_eq!(rc, std::process::ExitCode::FAILURE, "{err}");
        assert!(Profiles::load(&s, &pp).unwrap().auto_update);

        // uninstall -y：什么都没删
        with_unit(&s);
        let (rc, err) = busy(&s, &["uninstall", "-y"], &[]);
        assert_eq!(rc, std::process::ExitCode::FAILURE, "{err}");
        assert!(s.exists(&pp.profiles()) && s.exists(&pp.unit(UNIT_MAIN)));

        // delete -y：9 个节点都还在，数据面没动
        let s = FakeSys::new();
        nine_nodes(&s, &pp, Mode::Socks);
        let before = restarts(&s);
        let (rc, err) = busy(&s, &["delete", ACTIVE, "-y"], &[]);
        assert_eq!(rc, std::process::ExitCode::FAILURE);
        assert!(err.contains("什么都没改"), "{err}");
        assert_eq!(names(&s, &pp).len(), 9);
        assert_eq!(restarts(&s), before);
    }

    #[test]
    fn a_busy_lock_in_the_menu_reports_on_the_last_line_and_changes_nothing() {
        let pp = paths();
        let (s, n) = (FakeSys::new(), FakeNet::new());
        ready(&s);
        two_nodes(&s, &pp);
        s.lock_busy(u32::MAX);
        let r = run_menu(&s, &n, &pp, &["1", "2", "", "0"], true);
        assert_eq!(
            Profiles::load(&s, &pp).unwrap().active.as_deref(),
            Some("alice-hy2-direct"),
            "{}",
            r.t
        );
        assert!(!s.called("systemctl restart bui-c.service"));
        assert!(
            r.t.lines()
                .any(|l| l.starts_with("  上次：失败：") && l.contains("稍后再试")),
            "「上次：」行要写明没做成、下一步是什么：\n{}",
            r.t
        );
    }

    #[test]
    fn changing_commands_read_profiles_inside_the_lock() {
        // 另一个会话在我们拿锁之前刚加了一个节点：锁内重读才不会把它写丢（spec §8.3「用户确认之后、
        // 读 profiles.json 之前」、R16）
        let pp = paths();
        let n = FakeNet::new();
        let mut theirs = profiles_socks();
        theirs.upsert(crate::testutil::named(
            "alice-reality-direct",
            reality_direct_node(),
        ));
        let theirs = String::from_utf8(serde_json::to_vec_pretty(&theirs).unwrap()).unwrap();

        for (args, answers) in [
            (vec!["import", "-"], vec![BOB_REALITY, ""]),
            (vec!["mode", "tun"], vec![]),
            // 切到的正是别人刚加的那个：锁外读的那份里根本没有它
            (vec!["switch", "alice-reality-direct"], vec![]),
            (vec!["update", "--auto", "off"], vec![]),
        ] {
            let s = FakeSys::new();
            ready(&s);
            profiles_socks().save(&s, &pp).unwrap();
            s.stage_on_lock("/opt/bui-c/profiles.json", &theirs);
            let mut p = Scripted {
                queue: answers.iter().map(|x| x.to_string()).collect(),
                asked: Vec::new(),
                tty: true,
            };
            let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
            dispatch(&parse(&args), &mut ctx).unwrap();
            let t = ctx.transcript.clone();
            assert!(
                names(&s, &pp).contains(&"alice-reality-direct".to_string()),
                "{args:?} 把拿锁前别人加的节点写丢了：\n{t}"
            );
        }
    }

    #[test]
    fn the_timer_check_busy_skips_the_round_and_the_self_update_and_writes_nothing() {
        let pp = paths();
        let s = FakeSys::new();
        ready(&s);
        s.reply("systemctl is-active --quiet bui-c.service", 3, "");
        let mut prof = profiles_socks();
        prof.panel = Some(crate::profiles::Panel {
            base_url: "https://panel.example.com".into(),
            username: "alice".into(),
        });
        prof.save(&s, &pp).unwrap();
        let n = FakeNet::new();
        n.route(crate::check::PROBE_URL, FakeReply::Timeout);
        s.lock_busy(u32::MAX);
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["check"]), &mut ctx).unwrap();
        let t = ctx.transcript.clone();
        assert!(t.contains("本轮巡检跳过"), "{t}");
        assert!(!s.called("systemctl restart bui-c.service"), "{t}");
        assert_eq!(s.writes("/opt/bui-c/runtime.json"), 0, "{t}");
        assert!(
            !n.log().iter().any(|l| l.contains("manifest.json")),
            "锁被占着的这一轮不自更新：{:?}",
            n.log()
        );
    }

    #[test]
    fn a_busy_lock_skips_the_daily_self_update_without_recording_an_attempt() {
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
        s.lock_busy(u32::MAX);
        let mut p = Scripted::from([]);
        let mut ctx = Ctx::new(&s, &n, &pp, &mut p, false, false);
        dispatch(&parse(&["check"]), &mut ctx).unwrap();
        assert!(ctx.transcript.contains("正常"), "{}", ctx.transcript);
        assert!(!n.log().iter().any(|l| l.contains("manifest.json")));
        let rt = Runtime::load(&s, &pp);
        assert_eq!(
            rt.last_update_attempt_at, None,
            "没真的去试：下一分钟还要能试，不能落进 1 小时的失败退避"
        );
    }

    /// [5] 的修复不再重复探测（T11 审查 I2）：隧道那一次 8 秒的探测只在首查与重查各做一次。
    #[test]
    fn menu_repair_restarts_without_probing_the_tunnel_a_second_time() {
        let pp = paths();
        let (s, n) = (FakeSys::new(), FakeNet::new());
        check_ready(&s, &n, &pp, Mode::Tun);
        n.route(crate::check::PROBE_URL, FakeReply::Timeout);
        let r = run_menu(&s, &n, &pp, &["5", "0", "0"], true);
        assert!(s.called("systemctl restart bui-c.service"), "{}", r.t);
        let probes = n
            .log()
            .iter()
            .filter(|l| l.contains(crate::check::PROBE_URL))
            .count();
        assert_eq!(probes, 2, "首查一次、重查一次：{:?}", n.log());
    }

    /// 只有本地端口没在听（服务在跑、TUN 在、隧道通）：nettest 判了端口失败，修复就要真的重启，
    /// 不能再让巡检那套只看服务与隧道的探测把它否掉（T11 审查 I2 的同源洞）。
    #[test]
    fn a_local_port_failure_really_restarts_even_with_the_tunnel_up() {
        let pp = paths();
        let (s, n) = (FakeSys::new(), FakeNet::new());
        ready(&s);
        crate::nettest::sample::machine(&s, &n, Mode::Tun); // 端口没登记在听
        let mut prof = crate::testutil::profiles_tun();
        prof.auto_update = false;
        prof.save(&s, &pp).unwrap();
        let r = run_menu(&s, &n, &pp, &["5", "0", "0"], true);
        assert!(
            s.called("systemctl restart bui-c.service"),
            "报告说了「再试一次」就得真的重启：\n{}",
            r.t
        );
        assert!(r.t.contains("已重启 bui-c.service"), "{}", r.t);
        assert_eq!(
            Runtime::load(&s, &pp).fail_streak,
            1,
            "照样记进 runtime.json"
        );
    }

    /// 重启与等就绪在同一把锁里（T11 审查 M5），而且只等一次。
    #[test]
    fn menu_repair_waits_for_readiness_inside_the_lock_and_only_once() {
        let pp = paths();

        // TUN：接口马上就在，只睡一拍；等接口的查询落在 restart 与 unlock 之间
        let (s, n) = (FakeSys::new(), FakeNet::new());
        check_ready(&s, &n, &pp, Mode::Tun);
        n.route(crate::check::PROBE_URL, FakeReply::Timeout);
        let r = run_menu(&s, &n, &pp, &["5", "0", "0"], true);
        let calls = s.calls();
        let restart = calls
            .iter()
            .position(|c| c == "systemctl restart bui-c.service")
            .unwrap_or_else(|| panic!("{}", r.t));
        let unlock = calls.iter().position(|c| c == "unlock").expect("放了锁");
        assert!(calls[..restart].iter().any(|c| c == "lock"), "{calls:?}");
        assert!(
            calls[restart..unlock]
                .iter()
                .any(|c| c == "ip link show bui-tun"),
            "等 TUN 就绪在锁里：{calls:?}"
        );
        assert_eq!(s.sleeps(), vec![500], "只等一次：{}", r.t);

        // SOCKS：服务一直起不来，等满 3 秒（12 拍）就停，不再在锁外重等一遍
        let (s, n) = (FakeSys::new(), FakeNet::new());
        check_ready(&s, &n, &pp, Mode::Socks);
        s.reply("systemctl is-active --quiet bui-c.service", 3, "");
        let r = run_menu(&s, &n, &pp, &["5", "0", "0"], true);
        assert_eq!(s.sleeps(), vec![250; 12], "{}", r.t);
        let calls = s.calls();
        let restart = calls
            .iter()
            .position(|c| c == "systemctl restart bui-c.service")
            .unwrap_or_else(|| panic!("{}", r.t));
        let unlock = calls.iter().position(|c| c == "unlock").expect("放了锁");
        assert!(restart < unlock, "{calls:?}");
        assert!(
            r.t.lines().any(|l| l.trim() == "已重启，3 秒内还是没起来"),
            "{}",
            r.t
        );
    }

    #[test]
    fn menu_repair_with_the_lock_busy_restarts_nothing_and_says_so() {
        let pp = paths();
        let (s, n) = (FakeSys::new(), FakeNet::new());
        check_ready(&s, &n, &pp, Mode::Socks);
        s.reply("systemctl is-active --quiet bui-c.service", 3, "");
        s.lock_busy(u32::MAX);
        let r = run_menu(&s, &n, &pp, &["5", "0", "0"], true);
        assert!(!s.called("systemctl restart bui-c.service"), "{}", r.t);
        assert_eq!(Runtime::load(&s, &pp).fail_streak, 0, "没重启就不记");
        assert!(
            r.t.lines()
                .any(|l| l.contains("修复失败：") && l.contains("稍后再试")),
            "{}",
            r.t
        );
        assert!(
            r.t.lines()
                .any(|l| l == "  上次：连接检查：失败 1 项（服务）"),
            "{}",
            r.t
        );

        // 40 列：「在等」那一句与「修复失败：…稍后再试」（61 列）都折得下，「稍后再试」不被截掉
        let (s, n) = (FakeSys::new(), FakeNet::new());
        check_ready(&s, &n, &pp, Mode::Socks);
        s.reply("systemctl is-active --quiet bui-c.service", 3, "");
        s.set_term_size(Some((40, 24)));
        s.lock_busy(u32::MAX);
        let r = run_menu(&s, &n, &pp, &["5", "0", "0"], true);
        for l in r.t.lines() {
            assert!(
                menu::budget_width(l) <= menu::line_limit(40),
                "{l:?}\n{}",
                r.t
            );
        }
        assert!(r.t.contains("稍后再试"), "{}", r.t);
        assert!(!s.called("systemctl restart bui-c.service"));
    }

    /// [5] 探测之后、修复拿到锁之前，节点设置被别处改了：不按旧设置重启或 apply（spec §8.3
    /// j-risk 那一条：删光节点的会话刚拆完数据面，按旧设置 apply 会把删掉的节点拉起来）。
    #[test]
    fn menu_repair_does_not_act_on_settings_that_changed_under_it() {
        let pp = paths();
        let json = |p: &Profiles| String::from_utf8(serde_json::to_vec_pretty(p).unwrap()).unwrap();
        // 另一个会话刚切到了 TUN：apply 得起来的设置，没有快照比对就会被照做
        let mut theirs = profiles_socks();
        theirs.auto_update = false;
        theirs.mode = Mode::Tun;

        // ① 单元文件在：不重启、不记连击，报告里说清修复没做
        let (s, n) = (FakeSys::new(), FakeNet::new());
        check_ready(&s, &n, &pp, Mode::Socks);
        s.reply("systemctl is-active --quiet bui-c.service", 3, "");
        s.stage_on_lock("/opt/bui-c/profiles.json", &json(&theirs));
        let r = run_menu(&s, &n, &pp, &["5", "0", "0"], true);
        assert!(!s.called("systemctl restart bui-c.service"), "{}", r.t);
        assert_eq!(Runtime::load(&s, &pp).fail_streak, 0, "{}", r.t);
        assert!(r.t.lines().any(|l| l.contains("修复失败：")), "{}", r.t);

        // ② 单元文件不在（删光没做完）：不按变了的设置 apply，也不按删光之后的空列表 apply
        for staged in [theirs, Profiles::new_default()] {
            let (s, n) = (FakeSys::new(), FakeNet::new());
            check_ready(&s, &n, &pp, Mode::Socks);
            s.remove_file(&pp.unit(UNIT_MAIN)).unwrap();
            s.reply("systemctl is-active --quiet bui-c.service", 3, "");
            s.stage_on_lock("/opt/bui-c/profiles.json", &json(&staged));
            let r = run_menu(&s, &n, &pp, &["5", "0", "0"], true);
            assert!(!s.exists(&pp.unit(UNIT_MAIN)), "没按旧设置 apply：{}", r.t);
            assert!(!s.called("systemctl start bui-c.service"), "{}", r.t);
            assert!(r.t.contains("修复失败："), "{}", r.t);
        }
    }
}
