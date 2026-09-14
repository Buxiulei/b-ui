//! 菜单 `[5]` 连接检查（spec §6）：11 行逐项检查，每做完一项就经 [`Hooks`] 回调一次，
//! 调用方用 [`Painter`] 按终端宽度排版、边做边打（spec §6.4）。
//!
//! 只被菜单 `[5]`（以及之后的 `bui-c test`）调用。timer 的 `bui-c check` 走 [`check::run`]，
//! 一个检测站都不碰（spec §6.7；守门测试 `the_timer_check_only_touches_the_probe_and_update_urls`）。
//! [`run`] 只出事件与结论，不知道终端多宽；排版全在 [`Painter`] 与 `render_*`。

use crate::check::{self, Verdict, PROBE_TIMEOUT, PROBE_URL};
use crate::engine::{Engine, TUN_READY_WAIT_S};
use crate::menu::{
    budget_width, compact_journal_line, line_limit, pad, rule, sanitize, strip_ansi, title_bar,
    truncate_end, truncate_middle, STANDARD_WIDTH,
};
use crate::net::{Net, Probe, ProbeError, Via};
use crate::paths::{Paths, TUN_IFACE, UNIT_MAIN};
use crate::profiles::{Mode, Profiles};
use crate::sys::{systemd, Sys};
use crate::Result;
use std::collections::BTreeMap;
use std::time::Duration;

/// 隧道通了之后测的网站，与隧道同一条腿（TUN 直连、SOCKS 经本地入站）。
pub const GOOGLE_URL: &str = "https://www.google.com/generate_204";
pub const YOUTUBE_URL: &str = "https://www.youtube.com/";
/// 不用根路径 `https://github.com/`：每日自更新的 manifest 也在 github.com 上，守门测试按
/// 「请求日志里含不含这个地址」判，根路径会把 `https://github.com/<仓库>/releases/…` 也算进来。
/// 探测只等响应头，robots.txt 是 GitHub 上最小、最稳的静态地址，一样看得出通不通、快不快。
pub const GITHUB_URL: &str = "https://github.com/robots.txt";
/// 永远直连：TUN 下按路由规则走 direct-out（验证国内分流），SOCKS 下绕过 sing-box（验证本机网络）。
pub const BAIDU_URL: &str = "https://www.baidu.com/";
pub const DOWNLOAD_URL: &str = "https://speed.cloudflare.com/__down?bytes=1000000";
pub const IPPURE_URL: &str = "https://my.ippure.com/v1/info";
/// ip-api 的地址都以它开头：按出口查（[`IPAPI_URL`]）与按地址查 IPv6（[`ipapi_url_for`]）。
pub const IPAPI_BASE: &str = "http://ip-api.com/json/";
const IPAPI_FIELDS: &str =
    "?fields=status,country,regionName,city,isp,org,as,mobile,proxy,hosting,query";
/// IPv4 出口的回退：ippure 缺字段或不可达时用（spec §6.2 第 10 行）。
pub const IPAPI_URL: &str =
    "http://ip-api.com/json/?fields=status,country,regionName,city,isp,org,as,mobile,proxy,hosting,query";
/// IPv6 出口地址的两个来源，一律直连（spec §6.2 第 11 行）。
pub const IPIFY6_URL: &str = "https://api6.ipify.org";
pub const ICANHAZIP6_URL: &str = "https://ipv6.icanhazip.com";
/// SOCKS 模式「DNS」一行用本机解析器解析的域名。
pub const DNS_HOST: &str = "www.baidu.com";

/// 检测站的全部具体地址：timer 的巡检一个都不许碰（守门测试逐个比对请求日志）。
/// 按地址而不是按主机名判：每日自更新本来就会访问 github.com（spec §0.2 R3）。
pub const URLS: &[&str] = &[
    GOOGLE_URL,
    YOUTUBE_URL,
    GITHUB_URL,
    BAIDU_URL,
    DOWNLOAD_URL,
    IPPURE_URL,
    IPAPI_BASE,
    IPIFY6_URL,
    ICANHAZIP6_URL,
];

const SITE_TIMEOUT: Duration = Duration::from_secs(6);
const BAIDU_TIMEOUT: Duration = Duration::from_secs(5);
const DOWNLOAD_BYTES: u64 = 1_000_000;
const DOWNLOAD_CAP: Duration = Duration::from_secs(5);
const IPPURE_TIMEOUT: Duration = Duration::from_secs(8);
const IPAPI_TIMEOUT: Duration = Duration::from_secs(6);
const V6_TIMEOUT: Duration = Duration::from_secs(5);
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(3);
/// SOCKS 模式重启后等本地端口：12 × 250ms = 3 秒（TUN 模式等接口，见 [`Engine::wait_tun_ready`]）。
const SOCKS_WAIT_STEPS: u32 = 12;
const SOCKS_WAIT_STEP: Duration = Duration::from_millis(250);
const SOCKS_WAIT_S: u64 = 3;
/// 服务起不来时报告里带出的日志行数；全文走「下一步」的 [3]。
pub const JOURNAL_LINES: u32 = 5;
/// 延迟不短于它就在后面加「（慢）」。
const SLOW_MS: u128 = 1000;
/// 汇总上面那条分隔线本来的长度（spec §2.2：60 列 28 个、40 列 18 个）。
const REPORT_RULE: usize = 28;

/// 一行结果前面的标记（spec §6.2）：计分项通了 `✓`、没通 `✗`；不计分项没通或只作提示 `○`；跳过 `-`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Pass,
    Fail,
    Info,
    Skip,
}

impl Mark {
    pub fn glyph(self) -> &'static str {
        match self {
            Mark::Pass => "✓",
            Mark::Fail => "✗",
            Mark::Info => "○",
            Mark::Skip => "-",
        }
    }
}

/// 检查项，按报告里的先后排：`Ord` 就是这个顺序，「第一个失败项」按它取。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Item {
    Service,
    Ports,
    Tun,
    Tunnel,
    Google,
    YouTube,
    GitHub,
    Dns,
    Baidu,
    Download,
    V4,
    V6,
}

impl Item {
    /// 报告左栏的标签，也是汇总与「上次：」行里的叫法。
    pub fn name(self) -> &'static str {
        match self {
            Item::Service => "服务",
            Item::Ports => "本地端口",
            Item::Tun => "TUN",
            Item::Tunnel => "隧道",
            Item::Google => "Google",
            Item::YouTube => "YouTube",
            Item::GitHub => "GitHub",
            Item::Dns => "DNS",
            Item::Baidu => "百度直连",
            Item::Download => "下载",
            Item::V4 => "IPv4 出口",
            Item::V6 => "IPv6",
        }
    }
}

/// [`run`] 边做边报的事件。排版（左栏宽、折行、截断、净化）归 [`Painter`]，这里只有内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// 一行开始：先打出左栏标签（不换行），人看得到正在查哪一项。
    Begin(&'static str),
    /// 这一行的结果，接在标签后面。
    Line(Mark, String),
    /// 续行，对齐到内容列（出口的归属、重启的说明、重查的结果）。
    Cont(String),
    /// 服务起不来时带出的最近几行日志。
    Journal(Vec<String>),
}

/// 一次检查的结论。计分项（spec §6.2）：TUN 模式 7 项，SOCKS 模式 5 项。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Summary {
    pub passed: u32,
    pub failed: u32,
    pub skipped: u32,
    /// 没通的不计分项（YouTube、GitHub、DNS、百度直连、下载），全部通过时汇总行里点名。
    pub info_failed: Vec<Item>,
    pub first_failure: Option<Item>,
    /// 百度直连拿到了响应（任何状态码）：本机自己能上网。
    pub baidu_ok: bool,
    /// 本机 DNS 解析失败：SOCKS 模式看「DNS」那一行，TUN 模式看百度直连的错误类别（spec §0.2 R13）。
    pub dns_failed: bool,
    pub elapsed_s: u64,
}

/// [`run`] 的回调。事件交给调用方打出来；修复（重启）也由调用方做：它管锁与收敛
/// （spec §0.2 R2、R13）。
pub trait Hooks {
    fn event(&mut self, e: Event);
    fn repair(&mut self) -> Result<Verdict>;
}

/// 出口类型（spec §6.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressKind {
    Residential,
    Datacenter,
    Proxy,
    Mobile,
}

impl EgressKind {
    pub fn name(self) -> &'static str {
        match self {
            EgressKind::Residential => "住宅宽带",
            EgressKind::Datacenter => "IDC 机房",
            EgressKind::Proxy => "代理 IP",
            EgressKind::Mobile => "移动网络",
        }
    }
}

/// 一个出口的检测结果。字段是检测站返回的原文，渲染时才净化、截断。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Egress {
    pub ip: String,
    pub country: String,
    pub city: String,
    pub org: String,
    pub kind: EgressKind,
    /// 只有 ippure 给风险分（0–100）。
    pub score: Option<u8>,
    pub source: &'static str,
}

/// ip-api 按地址查（IPv6 出口的归属），字段与 [`IPAPI_URL`] 相同。
pub fn ipapi_url_for(addr: &str) -> String {
    format!("{IPAPI_BASE}{addr}{IPAPI_FIELDS}")
}

/// 跑一遍连接检查（spec §6.2 的表，§0.2 R3、R13 的修订），每一项先报 [`Event::Begin`]、做完报结果。
///
/// 服务 → 本地端口 → TUN（只在 TUN 模式）→ 隧道；1–4 有失败就经 [`Hooks::repair`] 修一次、等就绪、
/// 重查，一轮检查只修一次。服务修过还没起来，其余全部跳过并带出日志；只有隧道不通时先测百度直连，
/// 直连也不通就不修（R13）。隧道通了再测 Google / YouTube / GitHub、（SOCKS）DNS、百度、下载、IPv4
/// 出口；隧道不通时网站、下载、出口合成跳过行，百度与 IPv6 照做（它们本来就直连）。
pub fn run<S: Sys, N: Net>(
    sys: &S,
    net: &N,
    paths: &Paths,
    prof: &Profiles,
    hooks: &mut dyn Hooks,
) -> Result<Summary> {
    let started = sys.now();
    let mut r = Runner {
        sys,
        net,
        paths,
        prof,
        hooks,
        via: check::via_for(prof),
        tun: prof.mode == Mode::Tun,
        scored: BTreeMap::new(),
        info_failed: Vec::new(),
        baidu: None,
        dns_failed: false,
    };
    r.all();
    let mut sum = r.summary();
    sum.elapsed_s = u64::try_from((sys.now() - started).whole_seconds()).unwrap_or(0);
    Ok(sum)
}

type ProbeResult = std::result::Result<Probe, ProbeError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TunState {
    Up,
    NoIface,
    NoRoute,
}

/// 第 2–4 项一次的结论：修复前后各查一次，比对着报。
#[derive(Debug, Clone)]
struct Core {
    socks: bool,
    http: bool,
    /// SOCKS 模式没有这一项。
    tun: Option<TunState>,
    tunnel: ProbeResult,
}

impl Core {
    fn ports_ok(&self) -> bool {
        self.socks && self.http
    }
    fn tun_ok(&self) -> bool {
        matches!(self.tun, None | Some(TunState::Up))
    }
    fn tunnel_ok(&self) -> bool {
        tunnel_passed(&self.tunnel)
    }
    fn ok(&self) -> bool {
        self.ports_ok() && self.tun_ok() && self.tunnel_ok()
    }
}

/// 隧道只认 204：强制门户（captive portal）会回 200 / 302。
fn tunnel_passed(r: &ProbeResult) -> bool {
    matches!(r, Ok(p) if p.code == 204)
}

struct Runner<'a, 'h, S: Sys, N: Net> {
    sys: &'a S,
    net: &'a N,
    paths: &'a Paths,
    prof: &'a Profiles,
    hooks: &'h mut dyn Hooks,
    via: Via,
    tun: bool,
    /// 计分项的结论：`Some(true)` 通过、`Some(false)` 失败、`None` 跳过；按 [`Item`] 的顺序排。
    scored: BTreeMap<Item, Option<bool>>,
    info_failed: Vec<Item>,
    /// 百度直连的结果：R13 下隧道不通时会提前测，行还是打在原位，不测第二次。
    baidu: Option<ProbeResult>,
    dns_failed: bool,
}

impl<S: Sys, N: Net> Runner<'_, '_, S, N> {
    fn begin(&mut self, label: &'static str) {
        self.hooks.event(Event::Begin(label));
    }
    fn line(&mut self, mark: Mark, text: impl Into<String>) {
        self.hooks.event(Event::Line(mark, text.into()));
    }
    fn cont(&mut self, text: impl Into<String>) {
        self.hooks.event(Event::Cont(text.into()));
    }
    fn score(&mut self, item: Item, ok: Option<bool>) {
        self.scored.insert(item, ok);
    }

    /// 这一模式下计分的项：TUN 7 项，SOCKS 5 项（没有 TUN，IPv6 只作提示）。
    fn scored_items(&self) -> Vec<Item> {
        let mut v = vec![Item::Service, Item::Ports];
        if self.tun {
            v.push(Item::Tun);
        }
        v.extend([Item::Tunnel, Item::Google, Item::V4]);
        if self.tun {
            v.push(Item::V6);
        }
        v
    }

    fn all(&mut self) {
        // 1 服务：没在运行就当场修一次，修完还没起来就不往下查
        self.begin(Item::Service.name());
        let mut repaired = false;
        if systemd::is_active(self.sys, UNIT_MAIN) {
            self.line(Mark::Pass, format!("{UNIT_MAIN} 在运行"));
        } else {
            self.line(Mark::Fail, format!("{UNIT_MAIN} 没在运行"));
            repaired = true;
            let restarted = self.repair();
            if restarted == Some(true) {
                self.wait_ready();
            }
            let up = systemd::is_active(self.sys, UNIT_MAIN);
            match (restarted, up) {
                (Some(true), true) => self.cont("已重启，现在在运行"),
                (Some(true), false) => {
                    let s = self.wait_s();
                    self.cont(format!("已重启，{s} 秒内还是没起来"));
                }
                (Some(false), true) => self.cont("再看一次，已经在运行"),
                (Some(false), false) => self.cont("再看一次，还是没在运行"),
                (None, _) => {} // 修复失败，原因已经打在续行里
            }
            if !up {
                self.service_down();
                return;
            }
        }
        self.score(Item::Service, Some(true));

        // 2–4 本地端口、TUN、隧道；有失败就修一次、重查（服务刚修过就不再修）
        let mut core = self.core_rows();
        if !core.ok() && !repaired && self.worth_repairing(&core) {
            if let Some(restarted) = self.repair() {
                if restarted {
                    self.cont(format!("已重启 {UNIT_MAIN}，再试一次…"));
                    self.wait_ready();
                } else {
                    self.cont("再试一次…");
                }
                if !systemd::is_active(self.sys, UNIT_MAIN) {
                    self.cont(format!("{} {UNIT_MAIN} 没在运行", Mark::Fail.glyph()));
                    self.service_down();
                    return;
                }
                let again = self.core_check();
                self.report_recheck(&core, &again);
                core = again;
            }
        }
        self.score(Item::Ports, Some(core.ports_ok()));
        if let Some(t) = core.tun {
            self.score(Item::Tun, Some(t == TunState::Up));
        }
        self.score(Item::Tunnel, Some(core.tunnel_ok()));

        // 5–10：隧道不通时走代理的项注定失败，合成跳过行，不白等 6+6+6+5+8+6 秒
        if core.tunnel_ok() {
            self.site(Item::Google, GOOGLE_URL, true);
            self.site(Item::YouTube, YOUTUBE_URL, false);
            self.site(Item::GitHub, GITHUB_URL, false);
            if !self.tun {
                self.dns_row();
            }
            self.baidu_row();
            self.download_row();
            self.v4_row();
        } else {
            self.begin("网站/下载");
            self.line(Mark::Skip, "跳过（隧道不通）");
            self.score(Item::Google, None);
            if !self.tun {
                self.dns_row();
            }
            self.baidu_row();
            self.begin(Item::V4.name());
            self.line(Mark::Skip, "跳过（隧道不通）");
            self.score(Item::V4, None);
        }
        // 11 IPv6：直连，隧道通不通都做
        self.v6_row();
    }

    /// 修一次：`Some(true)` 重启过，`Some(false)` 没重启（再探测时已经好了），`None` 修复失败
    /// （原因打成续行，不重查）。
    fn repair(&mut self) -> Option<bool> {
        match self.hooks.repair() {
            Ok(Verdict::Restarted { .. }) => Some(true),
            Ok(_) => Some(false),
            Err(e) => {
                self.cont(format!("修复失败：{e}"));
                None
            }
        }
    }

    /// 只有隧道不通、端口与 TUN 都正常时，先测百度直连：本机自己也上不了网，重启 bui-c 没用，
    /// 不修（spec §0.2 R13）。
    fn worth_repairing(&mut self, core: &Core) -> bool {
        let only_tunnel = core.ports_ok() && core.tun_ok() && !core.tunnel_ok();
        if only_tunnel && self.baidu_probe().is_err() {
            self.cont("本机也连不上百度，先不重启");
            return false;
        }
        true
    }

    /// 重启后等就绪，先睡再查：TUN 等接口（最多 5 秒），SOCKS 等本地端口（最多 3 秒）。
    fn wait_ready(&self) {
        if self.tun {
            Engine::new(self.sys, self.paths).wait_tun_ready();
            return;
        }
        for _ in 0..SOCKS_WAIT_STEPS {
            self.sys.sleep(SOCKS_WAIT_STEP);
            if systemd::is_active(self.sys, UNIT_MAIN)
                && self.sys.tcp_listening(self.prof.socks_port)
            {
                return;
            }
        }
    }

    fn wait_s(&self) -> u64 {
        if self.tun {
            TUN_READY_WAIT_S
        } else {
            SOCKS_WAIT_S
        }
    }

    /// 服务修过还是没起来：其余各项合成一行跳过，带出最近几行日志（spec §6.2 跳过规则）。
    /// TUN 下服务没起来时直连走的是本机出口：出口会显示成本机，IPv6 会被误判成泄漏，跳过比误报强。
    fn service_down(&mut self) {
        self.begin("其余各项");
        self.line(Mark::Skip, "跳过（服务没起）");
        for item in self.scored_items() {
            self.score(item, (item == Item::Service).then_some(false));
        }
        let lines = journal_tail(self.sys, JOURNAL_LINES).unwrap_or_else(|why| vec![why]);
        self.hooks.event(Event::Journal(lines));
    }

    fn check_tun(&self) -> TunState {
        let e = Engine::new(self.sys, self.paths);
        if !e.tun_up() {
            TunState::NoIface
        } else if !e.tun_default_route() {
            TunState::NoRoute
        } else {
            TunState::Up
        }
    }

    fn check_tunnel(&self) -> ProbeResult {
        self.net.probe(PROBE_URL, self.via, PROBE_TIMEOUT)
    }

    /// 第 2–4 项，不出声（修复之后重查用）。
    fn core_check(&self) -> Core {
        Core {
            socks: self.sys.tcp_listening(self.prof.socks_port),
            http: self.sys.tcp_listening(self.prof.http_port),
            tun: self.tun.then(|| self.check_tun()),
            tunnel: self.check_tunnel(),
        }
    }

    /// 第 2–4 项，每项先出标签再查（边做边打）。
    fn core_rows(&mut self) -> Core {
        self.begin(Item::Ports.name());
        let socks = self.sys.tcp_listening(self.prof.socks_port);
        let http = self.sys.tcp_listening(self.prof.http_port);
        self.line(pass_or(socks, Mark::Fail), self.ports_text(http));
        let tun = if self.tun {
            self.begin(Item::Tun.name());
            let t = self.check_tun();
            self.line(pass_or(t == TunState::Up, Mark::Fail), tun_text(t));
            Some(t)
        } else {
            None
        };
        self.begin(Item::Tunnel.name());
        let tunnel = self.check_tunnel();
        self.line(
            pass_or(tunnel_passed(&tunnel), Mark::Fail),
            self.tunnel_text(&tunnel),
        );
        Core {
            socks,
            http,
            tun,
            tunnel,
        }
    }

    /// `SOCKS5 :1080   ✓ HTTP :8080`：SOCKS 的标记是这一行的 [`Mark`]，HTTP 的写在中间。
    fn ports_text(&self, http: bool) -> String {
        format!(
            "SOCKS5 :{}   {} HTTP :{}",
            self.prof.socks_port,
            pass_or(http, Mark::Fail).glyph(),
            self.prof.http_port
        )
    }

    fn tunnel_text(&self, r: &ProbeResult) -> String {
        match r {
            Ok(p) if p.code == 204 => format!("通  {}", latency(p.elapsed)),
            Ok(p) => format!("返回 HTTP {}", p.code),
            Err(e) => {
                let why = failure_text(e, PROBE_TIMEOUT);
                match self.via {
                    Via::Socks5 { port } => format!("经 SOCKS5 :{port} {why}"),
                    Via::Direct => why,
                }
            }
        }
    }

    /// 重查的结果写成续行：之前没通、或者这次没通的项各一行；两次都通的不说。
    fn report_recheck(&mut self, before: &Core, after: &Core) {
        if !(before.ports_ok() && after.ports_ok()) {
            let mark = pass_or(after.socks, Mark::Fail).glyph();
            let text = format!(
                "{}：{mark} {}",
                Item::Ports.name(),
                self.ports_text(after.http)
            );
            self.cont(text);
        }
        if let (Some(b), Some(a)) = (before.tun, after.tun) {
            if b != TunState::Up || a != TunState::Up {
                let mark = pass_or(a == TunState::Up, Mark::Fail).glyph();
                self.cont(format!("{}：{mark} {}", Item::Tun.name(), tun_text(a)));
            }
        }
        if !(before.tunnel_ok() && after.tunnel_ok()) {
            let fail = Mark::Fail.glyph();
            self.cont(match &after.tunnel {
                Ok(p) if p.code == 204 => {
                    format!("{} 通了  {}", Mark::Pass.glyph(), latency(p.elapsed))
                }
                _ if before.tunnel_ok() => format!("{fail} 重启后不通了"),
                _ => format!("{fail} 还是不通"),
            });
        }
    }

    /// Google（计分）/ YouTube / GitHub（不计分）：拿到 4xx 以下的响应就算通，不跟重定向。
    fn site(&mut self, item: Item, url: &str, scored: bool) {
        self.begin(item.name());
        let r = self.net.probe(url, self.via, SITE_TIMEOUT);
        let (ok, text) = match &r {
            Ok(p) if p.code < 400 => (true, latency(p.elapsed)),
            Ok(p) => (false, format!("返回 HTTP {}", p.code)),
            Err(e) => (false, failure_text(e, SITE_TIMEOUT)),
        };
        let mark = match (ok, scored) {
            (true, _) => Mark::Pass,
            (false, true) => Mark::Fail,
            (false, false) => Mark::Info,
        };
        self.line(mark, text);
        if scored {
            self.score(item, Some(ok));
        } else if !ok {
            self.info_failed.push(item);
        }
    }

    /// SOCKS 模式的「DNS」一行（spec §0.2 R3）：本机解析器解析 [`DNS_HOST`]，不计分。
    /// TUN 模式不单测（`hijack-dns` 截走了），并进百度那一行报。
    fn dns_row(&mut self) {
        self.begin(Item::Dns.name());
        match self.sys.resolve(DNS_HOST, RESOLVE_TIMEOUT) {
            Ok(d) => self.line(Mark::Pass, format!("本机解析正常 {}ms", d.as_millis())),
            Err(_) => {
                self.line(Mark::Info, "本机 DNS 解析失败");
                self.dns_failed = true;
                self.info_failed.push(Item::Dns);
            }
        }
    }

    fn baidu_probe(&mut self) -> ProbeResult {
        if let Some(r) = &self.baidu {
            return r.clone();
        }
        let r = self.net.probe(BAIDU_URL, Via::Direct, BAIDU_TIMEOUT);
        self.baidu = Some(r.clone());
        r
    }

    /// 百度直连（不计分）：解析失败时写「本机 DNS 解析失败」，TUN 模式下判断就看它。
    fn baidu_row(&mut self) {
        self.begin(Item::Baidu.name());
        let text = match self.baidu_probe() {
            Ok(p) if p.code < 400 => {
                self.line(Mark::Pass, latency(p.elapsed));
                return;
            }
            Ok(p) => format!("返回 HTTP {}", p.code),
            Err(ProbeError::Dns) => {
                if self.tun {
                    self.dns_failed = true;
                }
                "本机 DNS 解析失败".to_string()
            }
            Err(e) => failure_text(&e, BAIDU_TIMEOUT),
        };
        self.line(Mark::Info, text);
        self.info_failed.push(Item::Baidu);
    }

    /// 下载 1 MB、5 秒封顶（不计分）：按实际读到的字节 ÷ 实际用时算，没下完就说是估算。
    fn download_row(&mut self) {
        self.begin(Item::Download.name());
        let d = self
            .net
            .download_via(DOWNLOAD_URL, self.via, DOWNLOAD_BYTES, DOWNLOAD_CAP);
        let Some(d) = d.ok().filter(|d| d.bytes > 0) else {
            self.line(Mark::Info, "下载失败");
            self.info_failed.push(Item::Download);
            return;
        };
        // 用时按 1ms 兜底，免得除以 0
        let secs = d.elapsed.max(Duration::from_millis(1)).as_secs_f64();
        let mb = d.bytes as f64 / 1e6;
        let speed = mb / secs;
        if d.complete {
            self.line(
                Mark::Pass,
                format!(
                    "1 MB 用时 {secs:.1} 秒，约 {} MB/s（{}）",
                    num(speed),
                    rating(speed)
                ),
            );
        } else {
            let head = if d.elapsed >= DOWNLOAD_CAP {
                format!("{} 秒没下完", DOWNLOAD_CAP.as_secs())
            } else {
                "没下完".to_string()
            };
            self.line(
                Mark::Info,
                format!(
                    "{head}，按已下载的 {} MB 估算约 {} MB/s（{}）",
                    num(mb),
                    num(speed),
                    rating(speed)
                ),
            );
        }
    }

    /// IPv4 出口（计分）：ippure 缺字段或不可达就回退 ip-api，同一条腿。成功时四行：
    /// IP / 国家  城市 / 运营商 / 类型  风险分 N（来源）。
    fn v4_row(&mut self) {
        self.begin(Item::V4.name());
        let e = self
            .net
            .text_via(IPPURE_URL, self.via, IPPURE_TIMEOUT)
            .ok()
            .and_then(|b| parse_ippure(&b))
            .or_else(|| {
                self.net
                    .text_via(IPAPI_URL, self.via, IPAPI_TIMEOUT)
                    .ok()
                    .and_then(|b| parse_ipapi(&b))
            });
        let Some(e) = e else {
            self.line(Mark::Fail, "查不到（ippure 与 ip-api 都不可达）");
            self.score(Item::V4, Some(false));
            return;
        };
        self.line(Mark::Pass, e.ip.clone());
        let geo = join2(&[&e.country, &e.city]);
        self.cont(if geo.is_empty() {
            "归属未知".to_string()
        } else {
            geo
        });
        self.cont(if e.org.is_empty() {
            "运营商未知".to_string()
        } else {
            e.org.clone()
        });
        self.cont(match e.score {
            Some(n) => format!("{}  风险分 {n}（{}）", e.kind.name(), e.source),
            None => format!("{}（{}）", e.kind.name(), e.source),
        });
        self.score(Item::V4, Some(true));
    }

    /// IPv6（一律直连）：TUN 下拿到地址就是泄漏（计分），SOCKS 下只作提示。拿到地址再用 ip-api
    /// 按地址查归属，查到了续一行。
    fn v6_row(&mut self) {
        self.begin(Item::V6.name());
        let addr = [IPIFY6_URL, ICANHAZIP6_URL].into_iter().find_map(|u| {
            self.net
                .text_via(u, Via::Direct, V6_TIMEOUT)
                .ok()
                .map(|t| t.trim().to_string())
                .filter(|a| valid_v6(a))
        });
        let geo = addr
            .as_deref()
            .and_then(|a| {
                self.net
                    .text_via(&ipapi_url_for(a), Via::Direct, IPAPI_TIMEOUT)
                    .ok()
            })
            .and_then(|b| parse_ipapi(&b))
            .map(|e| join2(&[&e.country, &e.city, &e.org]))
            .filter(|g| !g.is_empty());
        match (self.tun, addr) {
            (true, None) => {
                self.line(Mark::Pass, "已被隧道拦截，没有泄漏");
                self.score(Item::V6, Some(true));
            }
            (true, Some(a)) => {
                self.line(Mark::Fail, format!("IPv6 泄漏（没进隧道）：{a}"));
                self.score(Item::V6, Some(false));
            }
            (false, Some(a)) => self.line(
                Mark::Info,
                format!("本机 IPv6 {a}（SOCKS 模式下没走代理的流量用它）"),
            ),
            (false, None) => self.line(Mark::Info, "本机没有 IPv6（SOCKS 模式只作提示）"),
        }
        if let Some(g) = geo {
            self.cont(g);
        }
    }

    fn summary(self) -> Summary {
        let mut s = Summary {
            info_failed: self.info_failed,
            baidu_ok: matches!(self.baidu, Some(Ok(_))),
            dns_failed: self.dns_failed,
            ..Summary::default()
        };
        for (item, v) in self.scored {
            match v {
                Some(true) => s.passed += 1,
                Some(false) => {
                    s.failed += 1;
                    s.first_failure.get_or_insert(item);
                }
                None => s.skipped += 1,
            }
        }
        s
    }
}

fn pass_or(ok: bool, fail: Mark) -> Mark {
    if ok {
        Mark::Pass
    } else {
        fail
    }
}

fn tun_text(t: TunState) -> String {
    match t {
        TunState::Up => format!("{TUN_IFACE} 已建立，默认路由已接管"),
        TunState::NoIface => format!("{TUN_IFACE} 接口不存在"),
        TunState::NoRoute => format!("默认路由没有指向 {TUN_IFACE}"),
    }
}

/// 探测失败的说法（spec §11.5）。拒绝与其它连接错误都叫「连不上」：人分不清，也不必分。
fn failure_text(e: &ProbeError, timeout: Duration) -> String {
    match e {
        ProbeError::Timeout => format!("超时（{} 秒）", timeout.as_secs()),
        ProbeError::Dns => "域名解析失败".to_string(),
        ProbeError::Refused | ProbeError::Other(_) => "连不上".to_string(),
    }
}

/// 整数毫秒；不短于 1 秒的加「（慢）」（不上色，零 ANSI）。
fn latency(d: Duration) -> String {
    let ms = d.as_millis();
    if ms >= SLOW_MS {
        format!("{ms}ms（慢）")
    } else {
        format!("{ms}ms")
    }
}

/// 一位小数；不到 0.1 时两位，免得慢速被写成 0.0。
fn num(x: f64) -> String {
    if x >= 0.1 {
        format!("{x:.1}")
    } else {
        format!("{x:.2}")
    }
}

/// 严格校验 IPv6 地址（spec §6.2）：只含十六进制字符与 `:`、至少一个 `:`、不超过 39 个字符。
/// 错误页、JSON 报错、IPv4 都可能带冒号，拿它们当地址会误报泄漏。
fn valid_v6(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 39
        && s.contains(':')
        && s.chars().all(|c| c.is_ascii_hexdigit() || c == ':')
}

/// 用两个空格把非空的几段接起来（不用 `·`：它是歧义字符）。
fn join2(parts: &[&str]) -> String {
    parts
        .iter()
        .filter(|p| !p.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("  ")
}

/// ippure 的回答 → 出口。先解析再判空：`ip` 或 `isResidential` 缺一个（含 null、空串、
/// 挪进 `data.*`）都不算数，调用方回退 ip-api（spec §6.2）。只认顶层字段。
pub fn parse_ippure(body: &str) -> Option<Egress> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let resi = v.get("isResidential")?.as_bool()?;
    let ip = field(&v, "ip");
    if ip.is_empty() {
        return None;
    }
    Some(Egress {
        ip,
        country: field(&v, "country"),
        city: field(&v, "city"),
        org: field(&v, "asOrganization"),
        kind: if resi {
            EgressKind::Residential
        } else {
            EgressKind::Datacenter
        },
        score: v
            .get("fraudScore")
            .and_then(serde_json::Value::as_f64)
            .filter(|n| (0.0..=100.0).contains(n))
            .map(|n| n.round() as u8),
        source: "ippure",
    })
}

/// ip-api 的回答 → 出口。`status == "success"` 且有 `query` 才算数；运营商取 `isp`，空了取 `org`；
/// 类型：`hosting` → 机房，否则 `proxy` → 代理 IP，否则 `mobile` → 移动网络，都不是 → 住宅宽带。
pub fn parse_ipapi(body: &str) -> Option<Egress> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    if v.get("status")?.as_str()? != "success" {
        return None;
    }
    let ip = field(&v, "query");
    if ip.is_empty() {
        return None;
    }
    let flag = |k: &str| {
        v.get(k)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    };
    let kind = if flag("hosting") {
        EgressKind::Datacenter
    } else if flag("proxy") {
        EgressKind::Proxy
    } else if flag("mobile") {
        EgressKind::Mobile
    } else {
        EgressKind::Residential
    };
    let isp = field(&v, "isp");
    Some(Egress {
        ip,
        country: field(&v, "country"),
        city: field(&v, "city"),
        org: if isp.is_empty() {
            field(&v, "org")
        } else {
            isp
        },
        kind,
        score: None,
        source: "ip-api",
    })
}

/// 顶层的字符串字段，去首尾空白；没有、不是字符串都当空串。
fn field(v: &serde_json::Value, key: &str) -> String {
    v.get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_string()
}

/// 下载速度的评语，沿用 v3.6.2 的阈值：≥ 1.25 MB/s（10 Mbps）优秀，≥ 0.625 MB/s（5 Mbps）良好。
pub fn rating(mb_s: f64) -> &'static str {
    if mb_s >= 1.25 {
        "优秀"
    } else if mb_s >= 0.625 {
        "良好"
    } else {
        "较慢"
    }
}

/// 按第一个失败的计分项给一句人话判断，外加要不要给「下一步」小菜单（spec §6.5 的表；
/// DNS 分支 SOCKS 模式看「DNS」那一行，§0.2 R13）。全部通过（不计分项没通也算）时是 `None`。
pub fn diagnose(s: &Summary) -> Option<(&'static str, bool)> {
    Some(match s.first_failure? {
        Item::Service => ("服务没起来，网络检查都跳过了。", true),
        Item::Ports => ("本地端口没在监听。", true),
        Item::Tun => ("bui-tun 没接管路由；还不行就先切到 SOCKS 用。", true),
        Item::Tunnel if s.baidu_ok => ("本机能上网，是当前节点不通。", true),
        Item::Tunnel if s.dns_failed => ("本机 DNS 解析失败：先检查网络设置。", true),
        Item::Tunnel => ("这台机器自己也上不了网：先检查网线 / Wi-Fi。", true),
        Item::Google => (
            "隧道通但 Google 打不开：多半是出口受限，换个节点试试。",
            true,
        ),
        Item::V4 => ("出口检测站暂时连不上，网站能打开就不影响上网。", false),
        Item::V6 => ("IPv6 没进隧道：把这一屏截图发给管理员。", false),
        // 不计分项进不了 first_failure
        Item::YouTube | Item::GitHub | Item::Dns | Item::Baidu | Item::Download => return None,
    })
}

/// 「上次：」行的摘要（spec §6.5）。
pub fn last_summary(s: &Summary) -> String {
    match s.first_failure {
        None => format!("连接检查：全部通过（{} 项）", s.passed),
        Some(item) => format!("连接检查：失败 {} 项（{}）", s.failed, item.name()),
    }
}

/// label 至少留几列，才值得跟服务器:端口挤在一行（与节点列表同一口径，spec §2.4）。
const LABEL_MIN: usize = 8;

/// 报告页头：空行、标题条、节点名（中间截断）与详情，再空一行（spec §3d）。
/// 详情标准版式是 `label  服务器:端口`，放不下就尾截 label、保住服务器；窄屏（< 50 列）只留 label。
pub fn header(prof: &Profiles, width: usize) -> String {
    const HEAD: &str = "  节点  ";
    let limit = line_limit(width);
    let mode = match prof.mode {
        Mode::Tun => "TUN",
        Mode::Socks => "SOCKS",
    };
    let mut out = format!("\n{}\n", title_bar(&format!("连接检查（{mode} 模式）")));
    if let Some(p) = prof.active_profile() {
        let room = limit - budget_width(HEAD);
        let indent = " ".repeat(budget_width(HEAD));
        out.push_str(&format!(
            "{HEAD}{}\n",
            truncate_middle(&sanitize(&p.name), room)
        ));
        let label = sanitize(&p.node.label);
        let hp = sanitize(&format!("{}:{}", p.node.host, p.node.port)).into_owned();
        let detail = if width < STANDARD_WIDTH {
            truncate_end(&label, room)
        } else {
            let both = join2(&[&label, &hp]);
            let label_room = room.saturating_sub(budget_width(&hp) + 2);
            if budget_width(&both) <= room {
                both
            } else if !label.is_empty() && label_room >= LABEL_MIN {
                format!("{}  {hp}", truncate_end(&label, label_room))
            } else {
                truncate_end(&hp, room)
            }
        };
        if !detail.is_empty() {
            out.push_str(&format!("{indent}{detail}\n"));
        }
    }
    out.push('\n');
    out
}

/// 分隔线、汇总行、图例（打过 `○` 才有，spec §0.2 R3）。汇总行放不下就在中文标点处折。
pub fn render_summary(s: &Summary, saw_info: bool, width: usize) -> String {
    let mut out = format!("  {}\n", rule(width, 2, REPORT_RULE));
    out.push_str(&indented(&summary_text(s), width));
    if saw_info {
        out.push_str("  ○ 仅供参考，不计入通过/失败\n");
    }
    out
}

/// `全部通过（N 项），用时 T 秒[；YouTube 没通，不计分]`，或 `通过 a 项，失败 b 项[，跳过 c 项]，用时 T 秒`
/// （一项都没通过时不写「通过 0 项」，spec §3d-60-3）。
fn summary_text(s: &Summary) -> String {
    if s.failed == 0 && s.skipped == 0 {
        let mut t = format!("全部通过（{} 项），用时 {} 秒", s.passed, s.elapsed_s);
        if !s.info_failed.is_empty() {
            let names: Vec<&str> = s.info_failed.iter().map(|i| i.name()).collect();
            t.push_str(&format!("；{} 没通，不计分", names.join("、")));
        }
        return t;
    }
    let mut parts = Vec::new();
    if s.passed > 0 {
        parts.push(format!("通过 {} 项", s.passed));
    }
    if s.failed > 0 {
        parts.push(format!("失败 {} 项", s.failed));
    }
    if s.skipped > 0 {
        parts.push(format!("跳过 {} 项", s.skipped));
    }
    format!("{}，用时 {} 秒", parts.join("，"), s.elapsed_s)
}

/// 一句人话判断：缩进 2 列，放不下就在中文标点处折。
pub fn render_sentence(sentence: &str, width: usize) -> String {
    indented(sentence, width)
}

/// 缩进 2 列、按行宽折行的一段固定文案，每行以换行结尾。
fn indented(text: &str, width: usize) -> String {
    wrap(text, line_limit(width) - 2)
        .into_iter()
        .map(|l| format!("  {l}\n"))
        .collect()
}

/// `journalctl -o short-iso` 取 bui-c.service 最近 `n` 行，每行压成 `<时间> <消息>` 并去掉 ANSI 颜色；
/// 取不到时 `Err` 是一句给人看的说明。菜单 [4] 的日志页、[5] 小菜单的 [3] 与报告里的 5 行块共用。
pub fn journal_tail<S: Sys>(sys: &S, n: u32) -> std::result::Result<Vec<String>, String> {
    let n = n.to_string();
    let args = ["-u", UNIT_MAIN, "-n", &n, "--no-pager", "-o", "short-iso"];
    match sys.run("journalctl", &args) {
        Ok(o) if o.ok() => {
            let lines: Vec<String> = o
                .stdout
                .trim_end()
                .lines()
                .map(|l| strip_ansi(&compact_journal_line(l)))
                .collect();
            if lines.iter().all(|l| l.trim().is_empty()) {
                Err(format!("journalctl 里还没有 {UNIT_MAIN} 的日志"))
            } else {
                Ok(lines)
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

/// 窄屏把行首的 ISO 时间戳压成 `HH:MM:SS`：`2026-09-13T10:15:30+08:00` 占 25 列，
/// 40 列下留给消息的只剩 11 列。形状判定与 [`crate::menu::compact_journal_line`] 同源；
/// 认不出时间戳的行（`-- Boot … --`、多行消息的续行）原样保留。
fn compact_timestamp(line: &str) -> String {
    let Some((time, msg)) = line.split_once(' ') else {
        return line.to_string();
    };
    let b = time.as_bytes();
    let iso =
        b.len() >= 19 && b[..4].iter().all(u8::is_ascii_digit) && b[4] == b'-' && b[10] == b'T';
    // `get` 而不是索引：认不出形状、或切点不在字符边界上就原样留着，不 panic。
    match (iso, time.get(11..19)) {
        (true, Some(hms)) => format!("{hms} {msg}"),
        _ => line.to_string(),
    }
}

/// 日志块：空一行、标题条、每行缩进 2 列。先净化再尾截到行宽，一行日志不折成两行。
/// 窄屏放不下单元全名时标题改叫 `bui-c`：40 列下 `── bui-c.service 最近 50 行日志 ──` 超 1 列；
/// 窄屏还把行首时间戳压成 `HH:MM:SS`（省 17 列），否则 40 列下只看得到时间戳。
pub fn journal_block(n: u32, lines: &[String], width: usize) -> String {
    let limit = line_limit(width);
    let full = title_bar(&format!("{UNIT_MAIN} 最近 {n} 行日志"));
    let title = if budget_width(&full) <= limit {
        full
    } else {
        title_bar(&format!("bui-c 最近 {n} 行日志"))
    };
    let mut out = format!("\n{title}\n");
    for l in lines {
        if !l.is_empty() {
            let l = if width < STANDARD_WIDTH {
                compact_timestamp(l)
            } else {
                l.clone()
            };
            out.push_str("  ");
            out.push_str(&truncate_end(&sanitize(&l), limit - 2));
        }
        out.push('\n');
    }
    out
}

/// 把 [`Event`] 排成终端里的字（spec §2.5、§6.4）：左栏标准 11 列、窄屏 10 列，内容列对齐；放不下
/// 的在中文标点处或空格间隔处折行，续行对齐内容列；一段本身放不下就尾截。外来文字（检测站的字段、
/// 日志、错误原因）先 [`sanitize`]。顺带记下打没打过 `○`：汇总下面的图例看它。
pub struct Painter {
    width: usize,
    saw_info: bool,
}

impl Painter {
    pub fn new(width: usize) -> Self {
        Self {
            width,
            saw_info: false,
        }
    }

    /// 打过 `○` 没有（spec §0.2 R3：出现过 ○ 才加图例）。
    pub fn saw_info(&self) -> bool {
        self.saw_info
    }

    /// 内容列：2 列缩进加左栏。
    fn content_col(&self) -> usize {
        2 + if self.width < STANDARD_WIDTH { 10 } else { 11 }
    }

    /// 一个事件对应的一段字，原样打出即可：`Begin` 不带换行（结果接在同一行），其余以换行结尾。
    pub fn paint(&mut self, e: &Event) -> String {
        let col = self.content_col();
        let room = line_limit(self.width) - col;
        let indent = " ".repeat(col);
        match e {
            Event::Begin(label) => format!("  {}", pad(label, col - 2)),
            Event::Line(mark, text) => {
                if *mark == Mark::Info {
                    self.saw_info = true;
                }
                let mut out = String::new();
                for (i, l) in wrap(&format!("{} {}", mark.glyph(), sanitize(text)), room)
                    .iter()
                    .enumerate()
                {
                    if i > 0 {
                        out.push_str(&indent);
                    }
                    out.push_str(l);
                    out.push('\n');
                }
                out
            }
            Event::Cont(text) => wrap(&sanitize(text), room)
                .iter()
                .map(|l| format!("{indent}{l}\n"))
                .collect(),
            Event::Journal(lines) => journal_block(JOURNAL_LINES, lines, self.width),
        }
    }
}

/// 行首不该出现的收尾标点：断在它们后面，连着的几个一起走。
const CLOSING: &str = "，。；：、）";

/// 把一段字排进 `room` 列（容量口径）。放得下就原样一行；放不下就在断点处折：收尾标点之后、
/// 「（」之前、两个以上空格处（空格不带到行首）。ASCII 词不拆；一段本身放不下就尾截。
fn wrap(text: &str, room: usize) -> Vec<String> {
    if budget_width(text) <= room {
        return vec![text.to_string()];
    }
    // 切段：每段带上它前面的空格间隔，只有和上一段拼在同一行时才用
    let mut segs: Vec<(String, String)> = Vec::new();
    let (mut gap, mut cur) = (String::new(), String::new());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == ' ' {
            let mut run = String::from(' ');
            while chars.peek() == Some(&' ') {
                run.push(' ');
                chars.next();
            }
            if run.len() >= 2 && !cur.is_empty() {
                segs.push((std::mem::take(&mut gap), std::mem::take(&mut cur)));
                gap = run;
            } else {
                cur.push_str(&run);
            }
            continue;
        }
        if c == '（' && !cur.is_empty() {
            segs.push((std::mem::take(&mut gap), std::mem::take(&mut cur)));
        }
        cur.push(c);
        if CLOSING.contains(c) && !chars.peek().is_some_and(|n| CLOSING.contains(*n)) {
            segs.push((std::mem::take(&mut gap), std::mem::take(&mut cur)));
        }
    }
    if !cur.is_empty() {
        segs.push((gap, cur));
    }
    // 贪心拼行
    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();
    for (gap, seg) in segs {
        if line.is_empty() {
            line = seg;
        } else if budget_width(&line) + budget_width(&gap) + budget_width(&seg) <= room {
            line.push_str(&gap);
            line.push_str(&seg);
        } else {
            out.push(std::mem::replace(&mut line, seg));
        }
    }
    if !line.is_empty() {
        out.push(line);
    }
    out.into_iter().map(|l| truncate_end(&l, room)).collect()
}

/// 测试样例：装好了的机器、检测站的回答、事件录音机、整屏拼装与宽度守门用的几种形态。
/// cli 与 menu 的测试也用它。
#[cfg(test)]
pub(crate) mod sample {
    use super::*;
    use crate::fake::{FakeNet, FakeReply, FakeSys};

    pub const IPPURE_DC: &str = r#"{"ip":"203.0.113.7","asn":64500,"asOrganization":"Example Networks","country":"美国","countryCode":"US","region":"California","city":"洛杉矶","timezone":"America/Los_Angeles","fraudScore":22,"isResidential":false,"isBroadcast":false}"#;
    pub const IPPURE_RESI: &str = r#"{"ip":"203.0.113.8","asOrganization":"Example Broadband","country":"美国","city":"旧金山","fraudScore":12,"isResidential":true}"#;
    pub const IPAPI_DC: &str = r#"{"status":"success","country":"United States","regionName":"California","city":"Los Angeles","isp":"Example Hosting","org":"Example Org","as":"AS64500 Example","mobile":false,"proxy":false,"hosting":true,"query":"203.0.113.9"}"#;
    pub const V6_ADDR: &str = "2001:db8::1";
    pub const IPAPI_V6: &str = r#"{"status":"success","country":"Germany","regionName":"Berlin","city":"Berlin","isp":"Example Telecom","org":"","as":"AS64501 Example","mobile":false,"proxy":false,"hosting":false,"query":"2001:db8::1"}"#;
    pub const JOURNAL_5: &str = "journalctl -u bui-c.service -n 5 --no-pager -o short-iso";
    /// 真机形态的日志：带颜色、带主机名与 ident，行很长；外加一行解析不了的分隔行。
    pub const LONG_JOURNAL: &str = "2026-09-13T10:15:30+08:00 baiyi sing-box[4242]: \u{1b}[31mFATAL\u{1b}[0m[0000] start service: initialize inbound/tun[tun-in]: configure tun interface: operation not permitted\n\
        2026-09-13T10:15:30+08:00 baiyi systemd[1]: bui-c.service: Main process exited, code=exited, status=1/FAILURE\n\
        -- Boot 0123456789abcdef0123456789abcdef --\n";

    pub fn restarted() -> Verdict {
        Verdict::Restarted {
            failures: Vec::new(),
            next_backoff_min: 1,
        }
    }

    /// 一台装好了的机器：内核与单元文件都在、单元在跑、（TUN）接口与默认路由都在，检测站都通，
    /// 出口走 ippure（机房）；IPv6 两个来源都不通（TUN 下就是「被隧道拦截」）。本地端口没登记在听。
    pub fn machine(s: &FakeSys, n: &FakeNet, mode: Mode) {
        s.put("/opt/bui-c/bin/sing-box", "ELF");
        s.put("/etc/systemd/system/bui-c.service", "[Unit]");
        s.reply("systemctl is-active --quiet bui-c.service", 0, "");
        s.set_resolve(DNS_HOST, Ok(12));
        if mode == Mode::Tun {
            s.reply("ip link show bui-tun", 0, "5: bui-tun");
            s.reply(
                "ip -4 route show table all",
                0,
                "default dev bui-tun table 2022\n",
            );
        }
        for (url, reply, ms) in [
            (PROBE_URL, FakeReply::Status(204), 356),
            (GOOGLE_URL, FakeReply::Status(204), 420),
            (YOUTUBE_URL, FakeReply::Status(200), 610),
            (GITHUB_URL, FakeReply::Status(200), 388),
            (BAIDU_URL, FakeReply::Status(200), 32),
            (DOWNLOAD_URL, FakeReply::Bytes(vec![0; 1_000_000]), 900),
            (IPPURE_URL, FakeReply::Text(IPPURE_DC.into()), 500),
        ] {
            n.route(url, reply);
            n.delay(url, ms);
        }
    }

    /// [`machine`] 再加上两个本地端口在听：各项都通。
    pub fn healthy(s: &FakeSys, n: &FakeNet, mode: Mode) {
        machine(s, n, mode);
        s.listen(1080);
        s.listen(8080);
    }

    /// 事件录音机：记下全部事件与修复次数。修复默认报「已重启」但什么都不改。
    pub struct Recorder<'a> {
        pub events: Vec<Event>,
        pub repairs: u32,
        on_repair: Box<dyn FnMut() -> Result<Verdict> + 'a>,
    }

    impl<'a> Recorder<'a> {
        /// 修复时先跑 `f`：测试在里面改 fake 的回答，模拟「重启之后好了」。
        pub fn with_repair(f: impl FnMut() -> Result<Verdict> + 'a) -> Self {
            Self {
                events: Vec::new(),
                repairs: 0,
                on_repair: Box::new(f),
            }
        }
        /// 每一行的左栏标签，按先后。
        pub fn labels(&self) -> Vec<&'static str> {
            self.events
                .iter()
                .filter_map(|e| match e {
                    Event::Begin(l) => Some(*l),
                    _ => None,
                })
                .collect()
        }
        /// 每一行的结果：`标记 文字`。
        pub fn lines(&self) -> Vec<String> {
            self.events
                .iter()
                .filter_map(|e| match e {
                    Event::Line(m, t) => Some(format!("{} {t}", m.glyph())),
                    _ => None,
                })
                .collect()
        }
        /// 全部续行。
        pub fn conts(&self) -> Vec<String> {
            self.events
                .iter()
                .filter_map(|e| match e {
                    Event::Cont(t) => Some(t.clone()),
                    _ => None,
                })
                .collect()
        }
    }

    impl Default for Recorder<'_> {
        fn default() -> Self {
            Self::with_repair(|| Ok(restarted()))
        }
    }

    impl Hooks for Recorder<'_> {
        fn event(&mut self, e: Event) {
            self.events.push(e);
        }
        fn repair(&mut self) -> Result<Verdict> {
            self.repairs += 1;
            (self.on_repair)()
        }
    }

    /// 菜单 [5] 打出来的整屏：标题与节点、逐行事件、汇总、判断与小菜单（与 `cli::check_menu` 同序）。
    pub fn screen(prof: &Profiles, events: &[Event], sum: &Summary, width: usize) -> String {
        let mut p = Painter::new(width);
        let mut out = header(prof, width);
        for e in events {
            out.push_str(&p.paint(e));
        }
        out.push_str(&render_summary(sum, p.saw_info(), width));
        if let Some((sentence, offer)) = diagnose(sum) {
            out.push_str(&render_sentence(sentence, width));
            if offer {
                out.push_str(&crate::menu::render_next_step());
            }
        }
        out
    }

    fn shot(prof: &Profiles, s: &FakeSys, n: &FakeNet, width: usize) -> String {
        let paths = Paths::new("/opt/bui-c", "/etc/systemd/system");
        let mut rec = Recorder::default();
        let sum = run(s, n, &paths, prof, &mut rec).expect("连接检查不该出错");
        screen(prof, &rec.events, &sum, width)
    }

    /// 宽度守门表（`menu::tests::screens`）里的报告整屏与日志页：真跑一遍 [`run`]，
    /// 用逐行事件拼出整屏（spec §0.2 R15）。节点名取真机最长的那种，检测站的字段故意很长。
    pub fn report_screens(width: usize) -> Vec<(&'static str, String)> {
        let long_v6 = "2001:db8:ffff:ffff:ffff:ffff:ffff:ffff";
        let v6_geo = format!(
            r#"{{"status":"success","country":"United Kingdom of Great Britain and Northern Ireland","city":"Example Town","isp":"Example Telecommunications Group PLC","query":"{long_v6}"}}"#
        );
        let mut prof = crate::testutil::baiyi_like();
        prof.active = Some("rick-node.example-a.net-reality-direct".into());
        let mut out = Vec::new();

        // TUN 全通过：住宅出口、字段很长还夹着控制字符、Google 慢、YouTube 超时（不计分，点名 + 图例）
        prof.mode = Mode::Tun;
        let (s, n) = (FakeSys::new(), FakeNet::new());
        healthy(&s, &n, Mode::Tun);
        // 出口字段故意很长，还夹着一个 ANSI 颜色序列（净化要把它换成 `?`）；
        // ESC 写成转义，免得裸 0x1B 过编辑器与 diff 工具时丢掉
        let esc = "\u{1b}";
        let resi_long = format!(
            r#"{{"ip":"203.0.113.8","asOrganization":"Example Very Long Residential Broadband Provider Incorporated{esc}[31m","country":"美利坚合众国示例联邦共和国","city":"旧金山湾区示例市某某区某某街道","fraudScore":100,"isResidential":true}}"#
        );
        n.route(IPPURE_URL, FakeReply::Text(resi_long));
        n.delay(GOOGLE_URL, 1_234);
        n.delay(YOUTUBE_URL, 6_000);
        out.push(("check-tun-pass", shot(&prof, &s, &n, width)));

        // SOCKS 隧道不通、重启也没用：判断 + 下一步小菜单；本机有 39 列的 IPv6，归属很长
        prof.mode = Mode::Socks;
        let (s, n) = (FakeSys::new(), FakeNet::new());
        healthy(&s, &n, Mode::Socks);
        n.route(PROBE_URL, FakeReply::Timeout);
        n.route(IPIFY6_URL, FakeReply::Text(format!("{long_v6}\n")));
        n.route(&ipapi_url_for(long_v6), FakeReply::Text(v6_geo.clone()));
        out.push(("check-socks-fail", shot(&prof, &s, &n, width)));

        // SOCKS 两个端口都没在听：重启后重查，续行写重查的结果
        let (s, n) = (FakeSys::new(), FakeNet::new());
        machine(&s, &n, Mode::Socks);
        out.push(("check-ports-down", shot(&prof, &s, &n, width)));

        // SOCKS 本机 DNS 坏了：只有隧道不通、百度也解析不到 → 不重启，DNS 分支的判断
        let (s, n) = (FakeSys::new(), FakeNet::new());
        healthy(&s, &n, Mode::Socks);
        n.route(PROBE_URL, FakeReply::Timeout);
        n.route(BAIDU_URL, FakeReply::Dns);
        s.set_resolve(DNS_HOST, Err(()));
        out.push(("check-dns", shot(&prof, &s, &n, width)));

        // TUN 服务起不来：其余跳过，带出最近 5 行日志（长行、带颜色）
        prof.mode = Mode::Tun;
        let (s, n) = (FakeSys::new(), FakeNet::new());
        healthy(&s, &n, Mode::Tun);
        s.reply("systemctl is-active --quiet bui-c.service", 3, "");
        s.reply(JOURNAL_5, 0, LONG_JOURNAL);
        out.push(("check-service-down", shot(&prof, &s, &n, width)));

        // TUN 默认路由没接管（重启重查）、IPv6 泄漏（39 列地址）、下载没下完、出口回退 ip-api
        let (s, n) = (FakeSys::new(), FakeNet::new());
        healthy(&s, &n, Mode::Tun);
        s.reply(
            "ip -4 route show table all",
            0,
            "default via 203.0.113.1 dev eth0\n",
        );
        n.route(IPPURE_URL, FakeReply::Fail("HTTP 502".into()));
        n.route(
            IPAPI_URL,
            FakeReply::Text(
                r#"{"status":"success","country":"Example Republic of Very Long Country Names","city":"Example City With A Long Name","isp":"Example Mobile Communications Company Limited","mobile":true,"query":"203.0.113.10"}"#
                    .into(),
            ),
        );
        n.route(DOWNLOAD_URL, FakeReply::Bytes(vec![0; 600_000]));
        n.delay(DOWNLOAD_URL, 9_000);
        n.route(IPIFY6_URL, FakeReply::Text(long_v6.into()));
        n.route(&ipapi_url_for(long_v6), FakeReply::Text(v6_geo));
        out.push(("check-leak", shot(&prof, &s, &n, width)));

        // 日志页：[4] → [2] 与小菜单 [3] 共用，最近 50 行里挑长的
        let lines: Vec<String> = LONG_JOURNAL
            .lines()
            .map(crate::menu::compact_journal_line)
            .collect();
        out.push(("log-page", journal_block(50, &lines, width)));
        // 小菜单里输错：误贴一长串
        out.push((
            "next-step-typo",
            format!(
                "  {}\n",
                crate::menu::invalid_next_step(&"9".repeat(200), width)
            ),
        ));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::sample::{self, Recorder, IPAPI_DC, IPAPI_V6, IPPURE_RESI, JOURNAL_5, V6_ADDR};
    use super::*;
    use crate::fake::{FakeNet, FakeReply, FakeSys};
    use crate::testutil::{profiles_socks, profiles_tun};
    use pretty_assertions::assert_eq;

    fn paths() -> Paths {
        Paths::new("/opt/bui-c", "/etc/systemd/system")
    }

    fn check(s: &FakeSys, n: &FakeNet, prof: &Profiles, rec: &mut Recorder<'_>) -> Summary {
        run(s, n, &paths(), prof, rec).unwrap()
    }

    fn hits(n: &FakeNet, url: &str) -> usize {
        n.log().iter().filter(|l| l.contains(url)).count()
    }

    #[test]
    fn tun_all_pass_report_has_eleven_lines_and_says_all_passed() {
        let (s, n) = (FakeSys::new(), FakeNet::new());
        sample::healthy(&s, &n, Mode::Tun);
        let mut rec = Recorder::default();
        let sum = check(&s, &n, &profiles_tun(), &mut rec);
        assert_eq!(
            rec.labels(),
            vec![
                "服务",
                "本地端口",
                "TUN",
                "隧道",
                "Google",
                "YouTube",
                "GitHub",
                "百度直连",
                "下载",
                "IPv4 出口",
                "IPv6"
            ]
        );
        assert_eq!(
            rec.lines(),
            vec![
                "✓ bui-c.service 在运行",
                "✓ SOCKS5 :1080   ✓ HTTP :8080",
                "✓ bui-tun 已建立，默认路由已接管",
                "✓ 通  356ms",
                "✓ 420ms",
                "✓ 610ms",
                "✓ 388ms",
                "✓ 32ms",
                "✓ 1 MB 用时 0.9 秒，约 1.1 MB/s（良好）",
                "✓ 203.0.113.7",
                "✓ 已被隧道拦截，没有泄漏",
            ],
            "11 行，每行一个结果"
        );
        assert_eq!(
            rec.conts(),
            vec![
                "美国  洛杉矶",
                "Example Networks",
                "IDC 机房  风险分 22（ippure）"
            ],
            "出口四行：IP / 归属 / 运营商 / 类型与风险分"
        );
        assert_eq!(
            (sum.passed, sum.failed, sum.skipped, sum.first_failure),
            (7, 0, 0, None)
        );
        assert!(sum.info_failed.is_empty() && sum.baidu_ok && !sum.dns_failed);
        assert_eq!(rec.repairs, 0, "都通就不修");
        assert_eq!(diagnose(&sum), None);
        assert_eq!(last_summary(&sum), "连接检查：全部通过（7 项）");
        assert_eq!(
            render_summary(&sum, false, 60),
            format!("  {}\n  全部通过（7 项），用时 0 秒\n", "─".repeat(28))
        );
    }

    #[test]
    fn socks_mode_has_a_dns_row_and_tun_mode_does_not() {
        let (s, n) = (FakeSys::new(), FakeNet::new());
        sample::healthy(&s, &n, Mode::Socks);
        let mut rec = Recorder::default();
        let sum = check(&s, &n, &profiles_socks(), &mut rec);
        assert_eq!(
            rec.labels(),
            vec![
                "服务",
                "本地端口",
                "隧道",
                "Google",
                "YouTube",
                "GitHub",
                "DNS",
                "百度直连",
                "下载",
                "IPv4 出口",
                "IPv6"
            ],
            "SOCKS 没有 TUN 行，百度前面多一行 DNS（spec §0.2 R3）"
        );
        let lines = rec.lines();
        assert_eq!(lines[6], "✓ 本机解析正常 12ms");
        assert_eq!(lines[10], "○ 本机没有 IPv6（SOCKS 模式只作提示）");
        assert_eq!(
            (sum.passed, sum.failed),
            (5, 0),
            "SOCKS 计 5 项，IPv6 只作提示"
        );
        // 走哪条腿：代理项经本地入站（socks5h），百度与 IPv6 永远直连
        let log = n.log();
        for url in [
            PROBE_URL,
            GOOGLE_URL,
            YOUTUBE_URL,
            GITHUB_URL,
            DOWNLOAD_URL,
            IPPURE_URL,
        ] {
            assert!(
                log.contains(&format!("GET {url} via socks5:1080")),
                "{url}: {log:?}"
            );
        }
        for url in [BAIDU_URL, IPIFY6_URL, ICANHAZIP6_URL] {
            assert!(
                log.contains(&format!("GET {url} via Direct")),
                "{url}: {log:?}"
            );
        }
        assert!(s.called("resolve www.baidu.com"));

        let (s, n) = (FakeSys::new(), FakeNet::new());
        sample::healthy(&s, &n, Mode::Tun);
        let mut rec = Recorder::default();
        check(&s, &n, &profiles_tun(), &mut rec);
        assert!(
            !rec.labels().contains(&"DNS"),
            "TUN 下 hijack-dns，单测本机 DNS 没有意义"
        );
        assert!(!s.called("resolve www.baidu.com"));
        assert!(
            n.log().iter().all(|l| l.ends_with("via Direct")),
            "TUN 已接管全局路由，一律直连：{:?}",
            n.log()
        );
    }

    #[test]
    fn ippure_missing_isresidential_falls_back_to_ipapi() {
        // ippure：先解析再判空，ip 与 isResidential 缺一个就不算数；只认顶层字段（spec §6.2）
        for body in [
            r#"{"ip":"203.0.113.7","country":"美国"}"#,
            r#"{"ip":"","isResidential":true}"#,
            r#"{"ip":null,"isResidential":true}"#,
            r#"{"ip":"203.0.113.7","isResidential":null}"#,
            r#"{"data":{"ip":"203.0.113.7","isResidential":true}}"#,
            "<html>502 Bad Gateway</html>",
            "",
        ] {
            assert_eq!(parse_ippure(body), None, "{body}");
        }
        assert_eq!(
            parse_ippure(IPPURE_RESI),
            Some(Egress {
                ip: "203.0.113.8".into(),
                country: "美国".into(),
                city: "旧金山".into(),
                org: "Example Broadband".into(),
                kind: EgressKind::Residential,
                score: Some(12),
                source: "ippure",
            })
        );
        // ip-api：status == success 才算数；hosting → 机房，否则 proxy → 代理，否则 mobile → 移动，都不是 → 住宅
        assert_eq!(
            parse_ipapi(IPAPI_DC),
            Some(Egress {
                ip: "203.0.113.9".into(),
                country: "United States".into(),
                city: "Los Angeles".into(),
                org: "Example Hosting".into(),
                kind: EgressKind::Datacenter,
                score: None,
                source: "ip-api",
            })
        );
        let kind = |flags: &str| {
            parse_ipapi(&format!(
                r#"{{"status":"success","query":"203.0.113.9",{flags}}}"#
            ))
            .map(|e| e.kind)
        };
        assert_eq!(
            kind(r#""hosting":true,"proxy":true"#),
            Some(EgressKind::Datacenter)
        );
        assert_eq!(
            kind(r#""hosting":false,"proxy":true,"mobile":true"#),
            Some(EgressKind::Proxy)
        );
        assert_eq!(kind(r#""mobile":true"#), Some(EgressKind::Mobile));
        assert_eq!(kind(r#""hosting":false"#), Some(EgressKind::Residential));
        assert_eq!(
            parse_ipapi(r#"{"status":"fail","message":"reserved range","query":"203.0.113.9"}"#),
            None
        );
        assert_eq!(
            parse_ipapi(r#"{"status":"success"}"#),
            None,
            "没有 query 就没有 IP"
        );

        // 整轮：ippure 缺 isResidential → 回退 ip-api，走同一条腿
        let (s, n) = (FakeSys::new(), FakeNet::new());
        sample::healthy(&s, &n, Mode::Socks);
        n.route(
            IPPURE_URL,
            FakeReply::Text(r#"{"ip":"203.0.113.7"}"#.into()),
        );
        n.route(IPAPI_URL, FakeReply::Text(IPAPI_DC.into()));
        let mut rec = Recorder::default();
        let sum = check(&s, &n, &profiles_socks(), &mut rec);
        assert!(
            rec.lines().contains(&"✓ 203.0.113.9".to_string()),
            "{:?}",
            rec.lines()
        );
        assert_eq!(
            rec.conts(),
            vec![
                "United States  Los Angeles",
                "Example Hosting",
                "IDC 机房（ip-api）"
            ],
            "ip-api 没有风险分"
        );
        assert!(n
            .log()
            .contains(&format!("GET {IPAPI_URL} via socks5:1080")));
        assert_eq!(sum.failed, 0);

        // 两个都不可达：计分项失败，但不给小菜单
        n.route(IPAPI_URL, FakeReply::Timeout);
        let mut rec = Recorder::default();
        let sum = check(&s, &n, &profiles_socks(), &mut rec);
        assert!(
            rec.lines()
                .contains(&"✗ 查不到（ippure 与 ip-api 都不可达）".to_string()),
            "{:?}",
            rec.lines()
        );
        assert_eq!(sum.first_failure, Some(Item::V4));
        assert_eq!(
            diagnose(&sum),
            Some(("出口检测站暂时连不上，网站能打开就不影响上网。", false))
        );
    }

    #[test]
    fn ipv6_in_tun_is_a_leak_and_absent_is_fine() {
        // 拿不到：被隧道拦截，没有泄漏
        let (s, n) = (FakeSys::new(), FakeNet::new());
        sample::healthy(&s, &n, Mode::Tun);
        let mut rec = Recorder::default();
        let sum = check(&s, &n, &profiles_tun(), &mut rec);
        assert_eq!(rec.lines().last().unwrap(), "✓ 已被隧道拦截，没有泄漏");
        assert_eq!(sum.failed, 0);

        // 拿到了：泄漏，计分项失败；续行给归属，按地址查归属也直连
        n.route(IPIFY6_URL, FakeReply::Text(format!("{V6_ADDR}\n")));
        n.route(&ipapi_url_for(V6_ADDR), FakeReply::Text(IPAPI_V6.into()));
        let mut rec = Recorder::default();
        let sum = check(&s, &n, &profiles_tun(), &mut rec);
        assert_eq!(
            rec.lines().last().unwrap(),
            "✗ IPv6 泄漏（没进隧道）：2001:db8::1"
        );
        assert_eq!(
            rec.conts().last().unwrap(),
            "Germany  Berlin  Example Telecom"
        );
        assert!(n
            .log()
            .contains(&format!("GET {} via Direct", ipapi_url_for(V6_ADDR))));
        assert_eq!((sum.failed, sum.first_failure), (1, Some(Item::V6)));
        assert_eq!(
            diagnose(&sum),
            Some(("IPv6 没进隧道：把这一屏截图发给管理员。", false))
        );

        // 地址严格校验：只认十六进制与冒号、至少一个冒号、不超过 39 个字符；
        // 第一个来源给的不像地址就换第二个
        n.route(
            IPIFY6_URL,
            FakeReply::Text("<html>2001:db8::1</html>".into()),
        );
        n.route(ICANHAZIP6_URL, FakeReply::Text("2001:db8::2".into()));
        let mut rec = Recorder::default();
        check(&s, &n, &profiles_tun(), &mut rec);
        assert_eq!(
            rec.lines().last().unwrap(),
            "✗ IPv6 泄漏（没进隧道）：2001:db8::2"
        );
        let too_long = "1:".repeat(20);
        n.route(ICANHAZIP6_URL, FakeReply::Timeout);
        for bad in [
            "203.0.113.7",
            "",
            "2001:db8::g",
            "fe80::1%eth0",
            too_long.as_str(),
            "error: rate limited",
        ] {
            n.route(IPIFY6_URL, FakeReply::Text(bad.into()));
            let mut rec = Recorder::default();
            check(&s, &n, &profiles_tun(), &mut rec);
            assert_eq!(
                rec.lines().last().unwrap(),
                "✓ 已被隧道拦截，没有泄漏",
                "{bad:?}"
            );
        }

        // SOCKS 下只作提示，不计分
        let (s, n) = (FakeSys::new(), FakeNet::new());
        sample::healthy(&s, &n, Mode::Socks);
        n.route(IPIFY6_URL, FakeReply::Text(V6_ADDR.into()));
        let mut rec = Recorder::default();
        let sum = check(&s, &n, &profiles_socks(), &mut rec);
        assert_eq!(
            rec.lines().last().unwrap(),
            "○ 本机 IPv6 2001:db8::1（SOCKS 模式下没走代理的流量用它）"
        );
        assert_eq!((sum.passed, sum.failed), (5, 0));
        assert!(sum.info_failed.is_empty(), "本机有 IPv6 不是毛病");
    }

    #[test]
    fn download_shows_a_rating_and_estimates_when_cut_at_five_seconds() {
        // v3 的阈值：≥ 1.25 MB/s（10 Mbps）优秀，≥ 0.625 MB/s（5 Mbps）良好
        assert_eq!(
            [
                rating(4.2),
                rating(1.25),
                rating(1.249),
                rating(0.625),
                rating(0.624),
                rating(0.0)
            ],
            ["优秀", "优秀", "良好", "良好", "较慢", "较慢"]
        );
        let row = |reply: FakeReply, ms: u64| {
            let (s, n) = (FakeSys::new(), FakeNet::new());
            sample::healthy(&s, &n, Mode::Tun);
            n.route(DOWNLOAD_URL, reply);
            n.delay(DOWNLOAD_URL, ms);
            let mut rec = Recorder::default();
            let sum = check(&s, &n, &profiles_tun(), &mut rec);
            (rec.lines()[8].clone(), sum)
        };
        let (l, _) = row(FakeReply::Bytes(vec![0; 1_000_000]), 900);
        assert_eq!(l, "✓ 1 MB 用时 0.9 秒，约 1.1 MB/s（良好）");
        let (l, _) = row(FakeReply::Bytes(vec![0; 1_000_000]), 240);
        assert_eq!(l, "✓ 1 MB 用时 0.2 秒，约 4.2 MB/s（优秀）");
        // 5 秒没下完：按实际读到的量与实际用时估算，不把截断的量当成完整速度
        let (l, sum) = row(FakeReply::Bytes(vec![0; 600_000]), 9_000);
        assert_eq!(l, "○ 5 秒没下完，按已下载的 0.6 MB 估算约 0.1 MB/s（较慢）");
        assert!(sum.info_failed.is_empty(), "慢不算没通");
        assert_eq!(sum.failed, 0, "下载不计分");
        let (l, sum) = row(FakeReply::Status(503), 100);
        assert_eq!(l, "○ 下载失败");
        assert_eq!(sum.info_failed, vec![Item::Download]);
    }

    #[test]
    fn unscored_failures_are_noted_in_the_summary_with_a_legend() {
        let (s, n) = (FakeSys::new(), FakeNet::new());
        sample::healthy(&s, &n, Mode::Tun);
        n.delay(GOOGLE_URL, 1_234);
        n.delay(YOUTUBE_URL, 6_000);
        n.route(GITHUB_URL, FakeReply::Refused);
        let mut rec = Recorder::default();
        let sum = check(&s, &n, &profiles_tun(), &mut rec);
        assert_eq!(
            rec.lines()[4..7],
            ["✓ 1234ms（慢）", "○ 超时（6 秒）", "○ 连不上"]
        );
        assert_eq!((sum.passed, sum.failed), (7, 0), "YouTube、GitHub 不计分");
        assert_eq!(sum.info_failed, vec![Item::YouTube, Item::GitHub]);
        let mut p = Painter::new(60);
        for e in &rec.events {
            p.paint(e);
        }
        assert!(p.saw_info(), "打过 ○ 就要图例");
        assert_eq!(
            render_summary(&sum, p.saw_info(), 60),
            format!(
                "  {}\n  全部通过（7 项），用时 0 秒；YouTube、GitHub 没通，不计分\n  ○ 仅供参考，不计入通过/失败\n",
                "─".repeat(28)
            )
        );
        // 40 列：在中文标点处折行，续行同样缩进 2 列
        assert_eq!(
            render_summary(&sum, true, 40),
            format!(
                "  {}\n  全部通过（7 项），用时 0 秒；\n  YouTube、GitHub 没通，不计分\n  ○ 仅供参考，不计入通过/失败\n",
                "─".repeat(18)
            )
        );
        // 全是 ✓：没有图例
        let (s, n) = (FakeSys::new(), FakeNet::new());
        sample::healthy(&s, &n, Mode::Tun);
        let mut rec = Recorder::default();
        check(&s, &n, &profiles_tun(), &mut rec);
        let mut p = Painter::new(60);
        for e in &rec.events {
            p.paint(e);
        }
        assert!(!p.saw_info());
    }

    #[test]
    fn tunnel_down_with_baidu_down_does_not_restart() {
        let (s, n) = (FakeSys::new(), FakeNet::new());
        sample::healthy(&s, &n, Mode::Tun);
        n.route(PROBE_URL, FakeReply::Timeout);
        n.route(BAIDU_URL, FakeReply::Timeout);
        let mut rec = Recorder::default();
        let sum = check(&s, &n, &profiles_tun(), &mut rec);
        assert_eq!(
            rec.repairs, 0,
            "本机自己上不了网，重启 bui-c 也没用（spec §0.2 R13）"
        );
        assert_eq!(
            rec.labels(),
            vec![
                "服务",
                "本地端口",
                "TUN",
                "隧道",
                "网站/下载",
                "百度直连",
                "IPv4 出口",
                "IPv6"
            ]
        );
        assert_eq!(rec.conts(), vec!["本机也连不上百度，先不重启"]);
        assert_eq!(rec.lines()[3], "✗ 超时（8 秒）");
        assert_eq!(rec.lines()[5], "○ 超时（5 秒）");
        assert_eq!(hits(&n, BAIDU_URL), 1, "提前测过的百度不测第二次");
        for url in [
            GOOGLE_URL,
            YOUTUBE_URL,
            GITHUB_URL,
            DOWNLOAD_URL,
            IPPURE_URL,
        ] {
            assert_eq!(hits(&n, url), 0, "隧道不通时不白等 {url}");
        }
        assert_eq!((sum.passed, sum.failed, sum.skipped), (4, 1, 2));
        assert!(!sum.baidu_ok);
        assert_eq!(
            diagnose(&sum),
            Some(("这台机器自己也上不了网：先检查网线 / Wi-Fi。", true))
        );
        assert_eq!(last_summary(&sum), "连接检查：失败 1 项（隧道）");

        // TUN：百度是解析失败 → 本机 DNS 的判断
        n.route(BAIDU_URL, FakeReply::Dns);
        let mut rec = Recorder::default();
        let sum = check(&s, &n, &profiles_tun(), &mut rec);
        assert_eq!(rec.lines()[5], "○ 本机 DNS 解析失败");
        assert!(sum.dns_failed);
        assert_eq!(
            diagnose(&sum),
            Some(("本机 DNS 解析失败：先检查网络设置。", true))
        );

        // SOCKS：看 DNS 那一行
        let (s, n) = (FakeSys::new(), FakeNet::new());
        sample::healthy(&s, &n, Mode::Socks);
        n.route(PROBE_URL, FakeReply::Timeout);
        n.route(BAIDU_URL, FakeReply::Timeout);
        s.set_resolve(DNS_HOST, Err(()));
        let mut rec = Recorder::default();
        let sum = check(&s, &n, &profiles_socks(), &mut rec);
        assert_eq!(rec.repairs, 0);
        assert_eq!(rec.lines()[2], "✗ 经 SOCKS5 :1080 超时（8 秒）");
        assert_eq!(rec.lines()[4], "○ 本机 DNS 解析失败");
        assert!(sum.dns_failed);
        assert_eq!(
            diagnose(&sum).unwrap().0,
            "本机 DNS 解析失败：先检查网络设置。"
        );
    }

    #[test]
    fn service_down_restarts_once_then_skips_the_rest() {
        let (s, n) = (FakeSys::new(), FakeNet::new());
        sample::healthy(&s, &n, Mode::Tun);
        s.reply("systemctl is-active --quiet bui-c.service", 3, "");
        s.reply(JOURNAL_5, 0, sample::LONG_JOURNAL);
        let mut rec = Recorder::default();
        let sum = check(&s, &n, &profiles_tun(), &mut rec);
        assert_eq!(rec.repairs, 1, "只重启一次");
        assert_eq!(rec.labels(), vec!["服务", "其余各项"]);
        assert_eq!(
            rec.lines(),
            vec!["✗ bui-c.service 没在运行", "- 跳过（服务没起）"]
        );
        assert_eq!(rec.conts(), vec!["已重启，5 秒内还是没起来"]);
        assert_eq!(
            s.sleeps().len(),
            crate::engine::APPLY_POLL_STEPS as usize,
            "TUN 等接口最多 5 秒"
        );
        let journal = rec
            .events
            .iter()
            .find_map(|e| match e {
                Event::Journal(l) => Some(l.clone()),
                _ => None,
            })
            .expect("带出日志");
        assert_eq!(
            journal[0],
            "2026-09-13T10:15:30+08:00 FATAL[0000] start service: initialize inbound/tun[tun-in]: configure tun interface: operation not permitted",
            "去掉主机名、ident 与颜色"
        );
        assert!(
            n.log().is_empty(),
            "服务没起来就一个检测站都不碰：TUN 下直连会走本机出口，出口与 IPv6 都会误报：{:?}",
            n.log()
        );
        assert_eq!(
            (sum.passed, sum.failed, sum.skipped, sum.first_failure),
            (0, 1, 6, Some(Item::Service))
        );
        assert_eq!(
            diagnose(&sum),
            Some(("服务没起来，网络检查都跳过了。", true))
        );
        assert!(render_summary(&sum, false, 60).contains("\n  失败 1 项，跳过 6 项，用时 0 秒\n"));

        // SOCKS：等端口最多 3 秒，跳过 4 项
        let (s, n) = (FakeSys::new(), FakeNet::new());
        sample::healthy(&s, &n, Mode::Socks);
        s.reply("systemctl is-active --quiet bui-c.service", 3, "");
        let mut rec = Recorder::default();
        let sum = check(&s, &n, &profiles_socks(), &mut rec);
        assert_eq!(rec.conts(), vec!["已重启，3 秒内还是没起来"]);
        assert_eq!((sum.failed, sum.skipped), (1, 4));
    }

    #[test]
    fn a_restart_that_brings_the_service_back_carries_on_with_every_row() {
        let (s, n) = (FakeSys::new(), FakeNet::new());
        sample::healthy(&s, &n, Mode::Socks);
        s.reply("systemctl is-active --quiet bui-c.service", 3, "");
        let mut rec = Recorder::with_repair(|| {
            s.reply("systemctl is-active --quiet bui-c.service", 0, "");
            Ok(sample::restarted())
        });
        let sum = check(&s, &n, &profiles_socks(), &mut rec);
        assert_eq!(rec.repairs, 1);
        assert_eq!(rec.conts()[0], "已重启，现在在运行");
        assert_eq!(rec.labels().len(), 11);
        assert_eq!(
            (sum.passed, sum.failed),
            (5, 0),
            "修好了就按修好之后的结论算"
        );
    }

    #[test]
    fn tunnel_down_restarts_once_then_skips_sites_but_not_baidu_or_ipv6() {
        let (s, n) = (FakeSys::new(), FakeNet::new());
        sample::healthy(&s, &n, Mode::Socks);
        n.route(PROBE_URL, FakeReply::Timeout);
        let mut rec = Recorder::default();
        let sum = check(&s, &n, &profiles_socks(), &mut rec);
        assert_eq!(rec.repairs, 1);
        assert_eq!(
            rec.lines(),
            vec![
                "✓ bui-c.service 在运行",
                "✓ SOCKS5 :1080   ✓ HTTP :8080",
                "✗ 经 SOCKS5 :1080 超时（8 秒）",
                "- 跳过（隧道不通）",
                "✓ 本机解析正常 12ms",
                "✓ 32ms",
                "- 跳过（隧道不通）",
                "○ 本机没有 IPv6（SOCKS 模式只作提示）",
            ]
        );
        assert_eq!(
            rec.labels(),
            vec![
                "服务",
                "本地端口",
                "隧道",
                "网站/下载",
                "DNS",
                "百度直连",
                "IPv4 出口",
                "IPv6"
            ]
        );
        assert_eq!(
            rec.conts(),
            vec!["已重启 bui-c.service，再试一次…", "✗ 还是不通"]
        );
        assert_eq!(
            hits(&n, PROBE_URL),
            2,
            "探测、重查各一次（重启本身归调用方）"
        );
        assert_eq!(hits(&n, BAIDU_URL), 1);
        for url in [
            GOOGLE_URL,
            YOUTUBE_URL,
            GITHUB_URL,
            DOWNLOAD_URL,
            IPPURE_URL,
            IPAPI_BASE,
        ] {
            assert_eq!(hits(&n, url), 0, "{url}");
        }
        assert_eq!(
            (sum.passed, sum.failed, sum.skipped),
            (2, 1, 2),
            "spec §3d-60-2"
        );
        assert!(sum.baidu_ok);
        assert_eq!(diagnose(&sum), Some(("本机能上网，是当前节点不通。", true)));
        assert!(render_summary(&sum, true, 60)
            .contains("\n  通过 2 项，失败 1 项，跳过 2 项，用时 0 秒\n"));

        // 重启之后通了：续行报「通了」，照常往下查
        let (s, n) = (FakeSys::new(), FakeNet::new());
        sample::healthy(&s, &n, Mode::Socks);
        n.route(PROBE_URL, FakeReply::Timeout);
        let mut rec = Recorder::with_repair(|| {
            n.route(PROBE_URL, FakeReply::Status(204));
            Ok(sample::restarted())
        });
        let sum = check(&s, &n, &profiles_socks(), &mut rec);
        assert_eq!(
            rec.conts(),
            vec![
                "已重启 bui-c.service，再试一次…",
                "✓ 通了  356ms",
                "美国  洛杉矶",
                "Example Networks",
                "IDC 机房  风险分 22（ippure）"
            ]
        );
        assert_eq!((sum.passed, sum.failed), (5, 0));
    }

    #[test]
    fn port_or_tun_failures_restart_right_away_and_report_the_recheck() {
        // 两个端口都没在听：不用先问百度，直接修；重查的结果写成续行
        let (s, n) = (FakeSys::new(), FakeNet::new());
        sample::machine(&s, &n, Mode::Socks);
        n.route(PROBE_URL, FakeReply::Refused);
        let mut rec = Recorder::default();
        let sum = check(&s, &n, &profiles_socks(), &mut rec);
        assert_eq!(rec.repairs, 1);
        assert_eq!(rec.lines()[1], "✗ SOCKS5 :1080   ✗ HTTP :8080");
        assert_eq!(rec.lines()[2], "✗ 经 SOCKS5 :1080 连不上");
        assert_eq!(
            rec.conts(),
            vec![
                "已重启 bui-c.service，再试一次…",
                "本地端口：✗ SOCKS5 :1080   ✗ HTTP :8080",
                "✗ 还是不通"
            ]
        );
        assert_eq!(hits(&n, BAIDU_URL), 1, "百度只在自己那一行测");
        assert_eq!(sum.first_failure, Some(Item::Ports));
        assert_eq!(diagnose(&sum), Some(("本地端口没在监听。", true)));

        // TUN 默认路由没接管
        let (s, n) = (FakeSys::new(), FakeNet::new());
        sample::healthy(&s, &n, Mode::Tun);
        s.reply(
            "ip -4 route show table all",
            0,
            "default via 203.0.113.1 dev eth0\n",
        );
        let mut rec = Recorder::default();
        let sum = check(&s, &n, &profiles_tun(), &mut rec);
        assert_eq!(rec.repairs, 1);
        assert_eq!(rec.lines()[2], "✗ 默认路由没有指向 bui-tun");
        assert_eq!(
            rec.conts()[..2],
            [
                "已重启 bui-c.service，再试一次…",
                "TUN：✗ 默认路由没有指向 bui-tun"
            ]
        );
        assert_eq!(sum.first_failure, Some(Item::Tun));
        assert_eq!(
            diagnose(&sum).unwrap().0,
            "bui-tun 没接管路由；还不行就先切到 SOCKS 用。"
        );
    }

    #[test]
    fn a_failed_repair_is_reported_once_and_not_rechecked() {
        let (s, n) = (FakeSys::new(), FakeNet::new());
        sample::healthy(&s, &n, Mode::Socks);
        n.route(PROBE_URL, FakeReply::Timeout);
        let mut rec = Recorder::with_repair(|| Err(crate::Error::msg("另一个 bui-c 操作正在进行")));
        let sum = check(&s, &n, &profiles_socks(), &mut rec);
        assert_eq!(rec.repairs, 1);
        assert_eq!(rec.conts(), vec!["修复失败：另一个 bui-c 操作正在进行"]);
        assert_eq!(hits(&n, PROBE_URL), 1, "没修成就不重查");
        assert_eq!(sum.first_failure, Some(Item::Tunnel));
    }

    #[test]
    fn diagnose_follows_the_table_and_the_last_line_names_the_first_failure() {
        let failed = |item: Item, baidu_ok: bool, dns_failed: bool| Summary {
            passed: 1,
            failed: 1,
            first_failure: Some(item),
            baidu_ok,
            dns_failed,
            ..Summary::default()
        };
        let cases = [
            (
                failed(Item::Service, true, false),
                ("服务没起来，网络检查都跳过了。", true),
            ),
            (
                failed(Item::Ports, true, false),
                ("本地端口没在监听。", true),
            ),
            (
                failed(Item::Tun, true, false),
                ("bui-tun 没接管路由；还不行就先切到 SOCKS 用。", true),
            ),
            (
                failed(Item::Tunnel, true, false),
                ("本机能上网，是当前节点不通。", true),
            ),
            (
                failed(Item::Tunnel, false, false),
                ("这台机器自己也上不了网：先检查网线 / Wi-Fi。", true),
            ),
            (
                failed(Item::Tunnel, false, true),
                ("本机 DNS 解析失败：先检查网络设置。", true),
            ),
            (
                failed(Item::Google, true, false),
                (
                    "隧道通但 Google 打不开：多半是出口受限，换个节点试试。",
                    true,
                ),
            ),
            (
                failed(Item::V4, true, false),
                ("出口检测站暂时连不上，网站能打开就不影响上网。", false),
            ),
            (
                failed(Item::V6, true, false),
                ("IPv6 没进隧道：把这一屏截图发给管理员。", false),
            ),
        ];
        for (s, want) in cases {
            let got = diagnose(&s).expect("有失败项就该有判断");
            // 折行之前就得放得下：判断是固定文案，带 2 列缩进按容量口径要 ≤ line_limit(60)。
            // 整屏宽度守门只看折行后的结果，超宽的句子靠 indented() 折成两行就能一直绿着混过去，
            // 所以这里直接钉折行前的宽度（spec §0.2 R3 的固定文案口径）。
            assert!(
                budget_width(got.0) + 2 <= line_limit(60),
                "{:?} 的判断 {} 列，超过 {} 列：{}",
                s.first_failure,
                budget_width(got.0) + 2,
                line_limit(60),
                got.0
            );
            assert_eq!(got, want, "{:?}", s.first_failure);
        }
        let all = Summary {
            passed: 7,
            ..Summary::default()
        };
        assert_eq!(diagnose(&all), None);
        assert_eq!(
            last_summary(&failed(Item::V4, true, false)),
            "连接检查：失败 1 项（IPv4 出口）"
        );
        // 固定文案带上 2 列缩进放不下时在中文标点处折行（Google 那句 40 列要折）
        assert_eq!(
            render_sentence("隧道通但 Google 打不开：多半是出口受限，换个节点试试。", 40),
            "  隧道通但 Google 打不开：\n  多半是出口受限，换个节点试试。\n"
        );
        assert_eq!(
            render_sentence("本机能上网，是当前节点不通。", 60),
            "  本机能上网，是当前节点不通。\n"
        );
    }

    #[test]
    fn narrow_rows_wrap_at_chinese_punctuation_and_align_to_the_content_column() {
        // 60 列：左栏 11 列，内容从第 13 列起；放得下就一行
        let mut p = Painter::new(60);
        assert_eq!(p.paint(&Event::Begin("IPv4 出口")), "  IPv4 出口  ");
        assert_eq!(p.paint(&Event::Begin("服务")), "  服务       ");
        assert_eq!(
            p.paint(&Event::Line(
                Mark::Pass,
                "bui-tun 已建立，默认路由已接管".into()
            )),
            "✓ bui-tun 已建立，默认路由已接管\n"
        );
        assert_eq!(
            p.paint(&Event::Cont("美国  洛杉矶".into())),
            format!("{}美国  洛杉矶\n", " ".repeat(13))
        );
        // 40 列：左栏 10 列；固定文案在中文标点处折，端口两段在空格间隔处折，续行对齐内容列
        let mut p = Painter::new(40);
        let pad12 = " ".repeat(12);
        assert_eq!(p.paint(&Event::Begin("IPv4 出口")), "  IPv4 出口 ");
        assert_eq!(
            p.paint(&Event::Line(
                Mark::Pass,
                "bui-tun 已建立，默认路由已接管".into()
            )),
            format!("✓ bui-tun 已建立，\n{pad12}默认路由已接管\n")
        );
        assert_eq!(
            p.paint(&Event::Line(
                Mark::Pass,
                "SOCKS5 :1080   ✓ HTTP :8080".into()
            )),
            format!("✓ SOCKS5 :1080\n{pad12}✓ HTTP :8080\n")
        );
        assert_eq!(
            p.paint(&Event::Cont("住宅宽带  风险分 12（ippure）".into())),
            format!("{pad12}住宅宽带  风险分 12\n{pad12}（ippure）\n")
        );
        // 外来文字先净化；一段本身放不下就尾截
        assert_eq!(
            p.paint(&Event::Cont("Evil\u{1b}[31mCorp".into())),
            format!("{pad12}Evil?[31mCorp\n")
        );
        let long = p.paint(&Event::Cont(
            "Example Very Long Residential Broadband Provider".into(),
        ));
        assert!(long.trim_end().ends_with('…'), "{long}");
        assert!(budget_width(long.trim_end()) <= line_limit(40), "{long}");
        // 收尾标点不落到行首；整段不超行宽
        let leak = p.paint(&Event::Line(
            Mark::Fail,
            "IPv6 泄漏（没进隧道）：2001:db8:ffff:ffff:ffff:ffff:ffff:ffff".into(),
        ));
        assert!(leak.starts_with("✗ IPv6 泄漏（没进隧道）：\n"), "{leak}");
        for (i, l) in leak.lines().enumerate() {
            let head = if i == 0 { "  IPv4 出口 " } else { "" };
            assert!(
                budget_width(&format!("{head}{l}")) <= line_limit(40),
                "{l:?}"
            );
            assert!(
                !l.trim_start().starts_with(['，', '：', '）', '；']),
                "{l:?}"
            );
        }
        assert!(!p.saw_info());
        p.paint(&Event::Line(Mark::Info, "超时（6 秒）".into()));
        assert!(p.saw_info());
    }

    #[test]
    fn journal_block_shortens_the_title_and_truncates_lines_to_fit() {
        let lines: Vec<String> = sample::LONG_JOURNAL
            .lines()
            .map(crate::menu::compact_journal_line)
            .collect();
        for w in [40, 50, 60, 80, 100] {
            let b = journal_block(50, &lines, w);
            assert!(b.starts_with('\n'), "标题前空一行");
            for l in b.lines() {
                assert!(budget_width(l) <= line_limit(w), "@{w}: {l:?}");
                assert!(!l.contains('\u{1b}'), "@{w}: {l:?}");
            }
        }
        let b = journal_block(50, &lines, 80);
        assert_eq!(
            b.lines().nth(1),
            Some("  ── bui-c.service 最近 50 行日志 ──")
        );
        let first = b.lines().nth(2).unwrap();
        assert!(
            first.starts_with("  2026-09-13T10:15:30+08:00 ?[31mFATAL") && first.ends_with('…'),
            "先净化再尾截：{first}"
        );
        assert_eq!(
            journal_block(50, &lines, 40).lines().nth(1),
            Some("  ── bui-c 最近 50 行日志 ──"),
            "40 列放不下单元全名"
        );
        // 40 列下完整 ISO 时间戳占掉 26 列、消息只剩 11 列，所以压成 HH:MM:SS
        let narrow = journal_block(50, &lines, 40);
        let first = narrow.lines().nth(2).unwrap();
        assert!(
            first.starts_with("  10:15:30 ?[31mFATAL") && first.ends_with('…'),
            "40 列把时间戳压成 HH:MM:SS：{first}"
        );
        assert!(
            narrow.lines().nth(4).unwrap().starts_with("  -- Boot "),
            "认不出时间戳的行原样保留"
        );
        assert_eq!(
            journal_block(JOURNAL_LINES, &lines, 40).lines().nth(1),
            Some("  ── bui-c.service 最近 5 行日志 ──"),
            "报告里的 5 行块 40 列放得下全名"
        );
        assert_eq!(
            journal_block(50, &["-- Boot 0123 --".to_string(), String::new()], 80),
            "\n  ── bui-c.service 最近 50 行日志 ──\n  -- Boot 0123 --\n\n"
        );
    }

    #[test]
    fn header_middle_truncates_the_name_and_drops_the_server_when_narrow() {
        let mut prof = crate::testutil::baiyi_like();
        prof.active = Some("rick-node.example-a.net-reality-direct".into());
        prof.mode = Mode::Socks;
        assert_eq!(
            header(&prof, 60),
            "\n  ── 连接检查（SOCKS 模式） ──\n  节点  rick-node.example-a.net-reality-direct\n        Reality直连  rick-node.example-a.net:10001\n\n"
        );
        prof.mode = Mode::Tun;
        let h = header(&prof, 40);
        let lines: Vec<&str> = h.lines().collect();
        assert_eq!(lines[1], "  ── 连接检查（TUN 模式） ──");
        assert!(
            lines[2].starts_with("  节点  rick-node")
                && lines[2].contains('…')
                && lines[2].ends_with("reality-direct"),
            "{}",
            lines[2]
        );
        assert_eq!(lines[3], "        Reality直连", "窄屏不显示服务器");
        for l in &lines {
            assert!(budget_width(l) <= line_limit(40), "{l}");
        }
        // 50 列放不下 label 与服务器：尾截 label、保住服务器
        prof.active = Some("reality-Reality".into());
        let h = header(&prof, 50);
        let detail = h.lines().nth(3).unwrap();
        assert!(
            detail.ends_with("  tizi.example.test:10001") && detail.contains('…'),
            "{detail}"
        );
        assert!(budget_width(detail) <= line_limit(50), "{detail}");
    }

    #[test]
    fn every_probe_address_is_listed_for_the_timer_guard() {
        assert_eq!(ipapi_url_for(""), IPAPI_URL, "按出口查与按地址查同一组字段");
        assert!(ipapi_url_for(V6_ADDR).starts_with(IPAPI_BASE));
        for u in [
            GOOGLE_URL,
            YOUTUBE_URL,
            GITHUB_URL,
            BAIDU_URL,
            DOWNLOAD_URL,
            IPPURE_URL,
            IPIFY6_URL,
            ICANHAZIP6_URL,
        ] {
            assert!(URLS.contains(&u), "{u}");
        }
        assert!(URLS.iter().any(|u| IPAPI_URL.starts_with(u)));
        // 每日自更新的地址不能被误算成检测站（守门测试按「日志行含不含」判）
        for update in [
            "https://github.com/x/b-ui/releases/latest/download/manifest.json",
            "https://github.com/x/b-ui/releases/download/v4.0.1/bui-c-linux-amd64",
            "https://panel.example.com/packages/manifest.json",
        ] {
            assert!(!URLS.iter().any(|u| update.contains(u)), "{update}");
        }
    }
}
