//! 进程内 watchdog：60 秒一轮，探四个内核的「单元存活 + 监听端口」，只治「进程还在、
//! 端口却不 listen」这一类僵死，重启带 1/2/4 分钟退避（spec §3.4）。
//!
//! 4.1 起同一轮里还做两件与住宅 HY2 跳跃有关的事（[`check_nft`]）：**校验并重放
//! `table inet bui`**（三处幂等重放的第三处），以及在每次重放**之前**把兼容段的活 counter
//! 增量累加进 `runtime.json`（[`CompatHits`]，spec §2.4 的裁决——活 counter 会被
//! `flush table` 清零，下线判据只能读持久值）。
//!
//! 移植参照 `server/core.sh:1050-1130`（`setup_hy2_watchdog`）。v4 的变化（审计 §3.1/§3.2
//! 与 web-C12）：不再生成脚本、不再建 timer、不再用 `/tmp/hy2-watchdog-*` 计数文件（面板还
//! 在读那个文件），改成守护进程里的内存状态机 + `runtime.json` 持久化；覆盖面从两个
//! hysteria 扩到四个内核；阈值 2 次（60s×2）+ 退避 1/2/4 分钟。
//!
//! 本模块不产出任何 `Artifact`（`render` 返回空 `Vec`），只有后台任务。

use crate::reconcile::{Artifact, DaemonCtx, Module, RenderCtx};
use crate::state::runtime::WatchdogRecord;
use crate::sys::{Host, Proto};
use crate::util::{fmt_rfc3339, parse_rfc3339};
use bui_schema::model::State;
use bui_schema::paths::Paths;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// 一轮检查的间隔。
pub const INTERVAL_SECS: u64 = 60;
/// 连续多少轮「进程在、端口不 listen」才重启。
pub const FAIL_THRESHOLD: u32 = 2;
/// 退避 1/2/4 分钟（spec §3.4），第四次及以后停在 4 分钟。
pub const BACKOFF_MINUTES: [i64; 3] = [1, 2, 4];
/// 每轮跑完把「这一轮的时刻 / 下一轮的时刻」记到 `runtime.extra` 的这个键下
/// （`RuntimeData::extra` 是 flatten 的扩展位，不必为两个字段改 `runtime.rs`）。
/// 面板 `GET /api/hy2/watchdog/status` 原样透出这两个字段。
pub const RUN_KEY: &str = "watchdog_run";

/// 会自己建端口跳跃 nat 链的 hysteria 实例与它的配置文件名。链按**实例**定位（base 端口 +
/// 跳跃区间都在这份配置的 `listen:` 行里），所以自愈时要把对应的那一份传给
/// [`crate::modules::portjump::cleanup`]。
///
/// **4.1 起只剩直连一项**：住宅换成 sing-box（一个 `:40000` 入站 + `inet bui` 表 REDIRECT
/// 整段跳跃），它不建任何 NAT 规则、也就没有孤儿链可清，而 4.0 那份
/// `config-residential.yaml` 已被对账删掉。住宅那一侧的等价自愈是 [`check_nft`]
/// （表被人删了 / 规则不符就整表重放）。4.0 遗留的按槽孤儿规则由
/// [`crate::modules::portjump::cleanup_legacy_residential`] 在对账里清。
pub const HY2_CONFIGS: [(&str, &str); 1] = [("hysteria-server", "config.yaml")];

/// 崩溃循环的判据：日志里的这句话（真机实录「ip6tables: Chain already exists」）。
pub const CHAIN_MARKER: &str = "Chain already exists";

/// 每轮看多少行日志。
pub const JOURNAL_LINES: &str = "50";

/// 自愈事件落 `runtime.extra` 的键：`{ "<单元>": ChainHeal }`。
/// 面板与 `bui status` 从 `runtime.json` 原样读得到。
pub const HEAL_KEY: &str = "hy2_chain_heal";

/// 同一个单元两次自愈之间的最短间隔：清完链还起不来说明另有原因，
/// 不能每 60 秒无脑 restart 一次（那就是自己制造崩溃循环）。
pub const HEAL_COOLDOWN_MINUTES: i64 = 10;

/// nft 表校验的结果落 `runtime.extra` 的键：`{ "<桶>": ChainHeal }`（与 [`HEAL_KEY`] 同款的
/// 累计次数 + 最近一次的时刻与做过的事）。住宅 HY2 的跳跃只有**一张**表，但冷却按
/// [裁决分桶][nft_bucket] —— 否则「已重放」那条 Warn 会把紧接着的「重放失败」Error 吞掉
/// 10 分钟。
pub const NFT_HEAL_KEY: &str = "nft_table_heal";

/// 表不在 / 规则不符（已重放，或重放失败）的事件签名。
pub const NFT_TABLE_MISSING_SIG: &str = "nft_table_missing";

/// PATH 上没有 `nft` 的事件签名（**Error 级**）。
pub const NFT_MISSING_SIG: &str = "nft_missing";

/// `nft list table` 失败、但失败原因**不是**「表不在」（权限、并发事务、包装脚本）的事件签名。
/// 这一轮既不重放也不采样：表很可能好着，冒充「表被删了」只会每 60 秒无谓重放一次，
/// 还会把兼容段的活计数按 `None` 吞掉、把 30 天门禁推向「零命中」的误判。
pub const NFT_UNREADABLE_SIG: &str = "nft_list_failed";

/// 整表重放这个动作的 id（事件的 `action` 字段）。
const NFT_REPLAY_ACTION: &str = "replay_nft_table";

/// 只能告警、无法处置时的动作 id（与哨兵的 `Action::Alert` 同字面）。
const NFT_ALERT_ACTION: &str = "alert";

/// 兼容段累计命中数落 `runtime.extra` 的键（[`CompatHits`]）。
pub const COMPAT_HITS_KEY: &str = "hy2_resi_compat_hits";

/// 一轮 nft 表校验的裁决。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NftVerdict {
    /// 表在位、规则与期望等价 —— 什么都不做
    Ok,
    /// 表不在或规则不符 ⇒ 已整表重放（一个 `nft -f -` 事务）
    Replayed,
    /// 该重放却重放失败（`nft -f` 非零，例如内核 < 5.2）：跳跃仍然不通
    Missing,
    /// PATH 上没有 `nft`：一步都做不了，只能告 Error
    NoBinary,
    /// `nft list table` 失败，但原因**不是**「表不在」（权限 / 并发事务 / 包装脚本）：
    /// 表可能好着，这一轮既不重放也不采样，只照抄原文告 Warn
    Unreadable,
}

/// 一轮 nft 表校验：裁决 + **重放之前**采到的兼容段活计数 + 重放失败时的原文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NftRound {
    pub verdict: NftVerdict,
    /// 兼容段两条规则的 `counter packets` 之和。表不在、兼容段关着、或回显里没有 counter
    /// ⇒ `None`（这一轮不动累计值）。**必须在重放之前采**：`nft -f` 的第一句是
    /// `flush table`，counter 随之归零（spec §2.4，2026-09-16 裁决）。
    pub compat_live: Option<u64>,
    /// 重放失败时 `nft` 的第一行错误（进事件正文，运维照它查内核版本）
    pub error: Option<String>,
    /// 回显里数出来的 redirect 条数（表不在 ⇒ 0）。进事件正文：判成「规则不符」却又
    /// 恰好是期望条数时，故障就不是「表被删了」而是回显解析对不上
    /// ——那会每 60 秒重放一次，得让 `bui incidents` 一眼看出来。
    pub seen_rules: usize,
}

/// `nft list table` 的回显（或渲染器的规则集）里每条 redirect 的 `(链, dport, 目标端口)`。
///
/// **不逐字比 `nft` 的回显**：`nft list` 会重排空白、把 `priority -100` 打成
/// `priority dstnat`、给每条规则插一段 `counter packets N bytes N`，逐字比必然假 FAIL。
/// 链名进 key 是为了守住「prerouting + output 双 hook」那条硬要求（少一条链时条数就不对，
/// 而只挂 prerouting 时本机发往自身公网 IP 的包不过 prerouting、跳跃对本机自测直接失效）。
pub fn redirect_rules(text: &str) -> Vec<(String, String, String)> {
    let mut chain = String::new();
    let mut out = Vec::new();
    for line in text.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("chain ") {
            chain = rest.trim_end_matches('{').trim().to_string();
            continue;
        }
        let Some((head, tail)) = line.split_once("redirect to :") else {
            continue;
        };
        let w: Vec<&str> = head.split_whitespace().collect();
        if w.first() != Some(&"udp") {
            continue;
        }
        let Some(dport) = w
            .iter()
            .position(|x| *x == "dport")
            .and_then(|i| w.get(i + 1))
        else {
            continue;
        };
        let Some(target) = tail.split_whitespace().next() else {
            continue;
        };
        out.push((chain.clone(), (*dport).to_string(), target.to_string()));
    }
    out.sort();
    out
}

/// 兼容段那两条规则的 `counter packets N` 之和（`nft list table` 的回显）。
/// 兼容段关着、表不在、或回显里找不到那两条 ⇒ `None`。
///
/// 量纲是**流数**而不是包数：nat 链的 counter 只计每条 conntrack 流的首包
/// （`man nft`：「Only the first packet of a connection …」）。
pub fn compat_counter(text: &str, p: &bui_schema::model::Ports) -> Option<u64> {
    let (a, b) = bui_schema::render::nft::compat_range(p);
    let want = format!("{a}-{b}");
    let mut total: Option<u64> = None;
    for line in text.lines().map(str::trim) {
        let w: Vec<&str> = line.split_whitespace().collect();
        if w.first() != Some(&"udp") || !line.contains("redirect to :") {
            continue;
        }
        let dport = w
            .iter()
            .position(|x| *x == "dport")
            .and_then(|i| w.get(i + 1))
            .copied();
        if dport != Some(want.as_str()) {
            continue;
        }
        let n = w
            .iter()
            .position(|x| *x == "packets")
            .and_then(|i| w.get(i + 1))
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        total = Some(total.unwrap_or(0) + n);
    }
    total
}

/// 住宅 HY2 端口跳跃那张 `inet bui` 表的每轮（60 秒）校验与重放（spec §2.4）。
///
/// 三处幂等重放之一（另两处：住宅单元的 `ExecStartPre=-{bin}/bui nft apply` 与每轮对账）。
/// 判据是[规范化后的规则集][redirect_rules]，**不是** `nft` 的回显文本。
///
/// 重放**不带冷却**（整表替换是幂等的、不重启任何单元），冷却只管事件与记录
/// ——表被人删掉时每 60 秒补回来，比「10 分钟内不再管」正确得多：这张表不在就等于
/// 住宅 HY2 的整段跳跃不通。
pub fn check_nft(host: &dyn Host, s: &State) -> NftRound {
    use bui_schema::render::nft;
    if !host.which("nft") {
        return NftRound {
            verdict: NftVerdict::NoBinary,
            compat_live: None,
            error: None,
            seen_rules: 0,
        };
    }
    let want = nft::ruleset(&s.node.ports, s.system.hy2_resi_compat_ports);
    // `nft list table` 失败分两种：**表不在**（重放它）与**读不到**（权限 / 并发事务 /
    // 包装脚本 —— 表可能好着，这一轮什么都不做）。后者冒充前者会每 60 秒无谓重放一次，
    // 还会让 `compat_live` 恒为 `None` ⇒ 累计命中永不涨 ⇒ 30 天门禁误判成「零命中」；
    // 前者冒充后者更贵（整表自愈永久失效），所以 [`table_missing`] 的默认方向是「表不在」。
    let listed = match host.run("nft", &["list", "table", nft::FAMILY, nft::NAME]) {
        Ok(o) if o.ok() => Some(o.stdout),
        Ok(o) if table_missing(&o) => None,
        Ok(o) => {
            return NftRound {
                verdict: NftVerdict::Unreadable,
                compat_live: None,
                error: Some(first_line(&o)),
                seen_rules: 0,
            }
        }
        Err(e) => {
            return NftRound {
                verdict: NftVerdict::Unreadable,
                compat_live: None,
                error: Some(e.to_string()),
                seen_rules: 0,
            }
        }
    };
    // 采样排在重放之前（`nft -f` 先 flush table，counter 归零）
    let compat_live = listed
        .as_deref()
        .and_then(|t| compat_counter(t, &s.node.ports));
    let got = listed.as_deref().map(redirect_rules).unwrap_or_default();
    let seen_rules = got.len();
    // `want` 恒有 2 或 4 条 redirect，所以「表不在」（`got` 为空）永远落不进这一支
    if got == redirect_rules(&want) {
        return NftRound {
            verdict: NftVerdict::Ok,
            compat_live,
            error: None,
            seen_rules,
        };
    }
    let (verdict, error) = match host.run_stdin("nft", &["-f", "-"], &want) {
        Ok(o) if o.ok() => (NftVerdict::Replayed, None),
        Ok(o) => (NftVerdict::Missing, Some(first_line(&o))),
        Err(e) => (NftVerdict::Missing, Some(e.to_string())),
    };
    NftRound {
        verdict,
        compat_live,
        error,
        seen_rules,
    }
}

/// `nft list table inet bui` 这次非零到底是不是「表不在」（⇒ 重放）。
///
/// **判据不许拿 glibc 的 strerror 文案当唯一凭据**：真机原文是 nft 自己的英文
/// `Error: ` 加 strerror(errno)，后半句跟着 locale 翻译（`LANG=zh_CN.UTF-8` 的机器上
/// ENOENT 是「没有那个文件或目录」），而守护进程的 locale 不由我们定——systemd manager
/// 继承 `/etc/locale.conf`，`b-ui.service` 只设 `RUST_LOG`。
/// 所以默认方向是**当表不在**：认错了只多重放一次（整表替换幂等、不重启任何单元），
/// 认反了则整表自愈永久失效 —— 住宅 HY2 的整段跳跃一直不通，只剩每 10 分钟一条 Warn。
/// 只把 [`UNREADABLE_MARKERS`] 认得出的「表可能好着、只是这一轮读不到」挑走；真机上那些
/// 文案也是英文，因为 [`crate::sys::real`] 给每个子进程钉了 `LC_ALL=C`。
fn table_missing(out: &crate::sys::CmdOut) -> bool {
    let text = if out.stderr.trim().is_empty() {
        out.stdout.trim()
    } else {
        out.stderr.trim()
    };
    !UNREADABLE_MARKERS.iter().any(|m| text.contains(m))
}

/// 「表可能好着，只是这一轮读不到」的回显标记：权限不足、内核对象被别的事务占着、
/// nft 自己的缓存初始化失败。认不出来的一律走 [`table_missing`] 的默认方向。
const UNREADABLE_MARKERS: [&str; 4] = [
    "Operation not permitted",
    "Permission denied",
    "Device or resource busy",
    "cache initialization failed",
];

/// 命令输出的第一行（stderr 优先）：进事件正文的那一句。
fn first_line(out: &crate::sys::CmdOut) -> String {
    let s = if out.stderr.trim().is_empty() {
        &out.stdout
    } else {
        &out.stderr
    };
    s.trim()
        .lines()
        .next()
        .unwrap_or("（没有输出）")
        .to_string()
}

/// 一种裁决一个冷却桶（[`NFT_HEAL_KEY`] 里的键，与 [`HEAL_KEY`] 按单元分桶同款）。
/// [`NftVerdict::Ok`] 没有桶。
///
/// **必须分桶**：四种裁决共用一条记录时，10 分钟窗口内的等级升级会被吞掉 —— 先记了一条
/// 「已整表重放」（Warn），紧接着 `nft -f` 开始失败（Error，正文是「跳跃段不通」）时那条
/// Error 会被冷却挡掉，`bui incidents` 在这 10 分钟里只看得到那句让人放心的 Warn。
pub fn nft_bucket(v: NftVerdict) -> Option<&'static str> {
    match v {
        NftVerdict::Ok => None,
        NftVerdict::Replayed => Some("replayed"),
        NftVerdict::Missing => Some("replay_failed"),
        NftVerdict::NoBinary => Some("no_binary"),
        NftVerdict::Unreadable => Some("unreadable"),
    }
}

/// nft 校验要记的记录与事件（纯函数）：[`NftVerdict::Ok`] 什么都不记，其余四种各记一条，
/// 各按自己那个[桶][nft_bucket]吃 [`HEAL_COOLDOWN_MINUTES`] 的冷却（同一件事 10 分钟内
/// 不重复刷，换一件事立刻出）。返回值第一项就是桶名（调用方据它写回 [`NFT_HEAL_KEY`]）。
///
/// 量级按真实后果写：`nft` 不在 ⇒ 带 `mport` 的客户端**只往跳跃段发、从不发 `:40000`**，
/// 所以那不是「跳跃失效」而是住宅全断。
pub fn nft_event(
    round: &NftRound,
    s: &State,
    heals: &std::collections::BTreeMap<String, ChainHeal>,
    now: OffsetDateTime,
) -> Option<(
    &'static str,
    ChainHeal,
    crate::modules::sentinel::incidents::Incident,
)> {
    use crate::modules::sentinel::incidents::{Incident, Level};
    let table = bui_schema::render::nft::TABLE;
    let p = &s.node.ports;
    let (hop_a, hop_b) = p.hy2_resi_hop;
    let want_rules = bui_schema::render::nft::rule_count(s.system.hy2_resi_compat_ports);
    let (signature, action, level, result) = match round.verdict {
        NftVerdict::Ok => return None,
        NftVerdict::NoBinary => (
            NFT_MISSING_SIG,
            NFT_ALERT_ACTION,
            Level::Error,
            format!(
                "PATH 上没有 nft：住宅 HY2 跳跃段全部失效 —— 带 mport 的客户端只往 \
                 {hop_a}-{hop_b} 发、从不发 :{}，等于住宅全断。请装 nftables 包（bui 不装系统包）",
                p.hy2_resi
            ),
        ),
        NftVerdict::Replayed => (
            NFT_TABLE_MISSING_SIG,
            NFT_REPLAY_ACTION,
            Level::Warn,
            format!(
                "table {table} 不在或规则不符（期望 {want_rules} 条 redirect，实到 {}），\
                 已整表重放（{hop_a}-{hop_b} → :{}）",
                round.seen_rules, p.hy2_resi
            ),
        ),
        NftVerdict::Missing => (
            NFT_TABLE_MISSING_SIG,
            NFT_REPLAY_ACTION,
            Level::Error,
            format!(
                "table {table} 重放失败：{} —— 住宅 HY2 跳跃段不通（{hop_a}-{hop_b} 没人接，\
                 盘上实到 {} 条 redirect）",
                round.error.as_deref().unwrap_or("（没有输出）"),
                round.seen_rules
            ),
        ),
        NftVerdict::Unreadable => (
            NFT_UNREADABLE_SIG,
            NFT_ALERT_ACTION,
            Level::Warn,
            format!(
                "读不到 table {table}：{} —— 这一轮不重放也不采样（表可能好着，\
                 照原文查权限 / 并发事务；真是表不在的话下一轮就会重放）",
                round.error.as_deref().unwrap_or("（没有输出）")
            ),
        ),
    };
    let bucket = nft_bucket(round.verdict)?;
    let last = heals.get(bucket);
    if !should_heal(last.and_then(|h| parse_rfc3339(&h.at)), now) {
        return None;
    }
    let rec = ChainHeal {
        at: fmt_rfc3339(now),
        count: last.map(|h| h.count).unwrap_or(0) + 1,
        done: vec![result.clone()],
    };
    let inc = Incident {
        at: fmt_rfc3339(now),
        // 这是守护进程自己做的事，不是从某个内核单元的日志里读出来的
        unit: "b-ui".into(),
        signature: signature.into(),
        subject: table.into(),
        action: action.into(),
        result,
        level,
        sample: None,
    };
    Some((bucket, rec, inc))
}

/// 兼容段（`40001-40007`）的**累计**命中数，落 `runtime.json` 的 [`COMPAT_HITS_KEY`]。
///
/// 存在的理由（spec §2.4，2026-09-16 裁决）：nft 的 `counter` 是瞬时流计数，
/// `render::nft` 每次重放都先 `flush table`，开机 / `bui nft apply` / watchdog 自愈 /
/// 改端口都把它清回 0。所以守护进程在每次重放**之前**采一次活计数、把增量累加到这里；
/// `bui status` 的「最近命中 N 次」与 `bui set hy2-resi-compat off` 的下线门禁
/// （[`CompatHits::idle_for_takedown`]）一律读这份持久值，**绝不读活 counter**。
/// 误判方向是危险的那一侧：把仍在用的兼容段判成闲置并关掉，全部还没刷订阅的 4.0 住宅
/// 用户当场断联。
///
/// **已知误差（2026-09-17 裁决：接受，不加跨进程协调）**：采样点只有 watchdog 自己这一处
/// （60 秒一轮，自己重放过的那一轮把比较基准归零，见 [`accumulate`]）。另两处幂等重放
/// ——住宅单元的 `ExecStartPre=-{bin}/bui nft apply` 与每轮对账的 `ApplyNftTable`——
/// 没有采样点（`bui nft apply` 连守护进程都不经，读不到 `runtime.json`），它们在两次采样
/// 之间 flush 过表时那一段增量（最多 60 秒的流数）会丢。有了
/// [`idle_for_takedown`](CompatHits::idle_for_takedown) 的「`total > 0` 永不自动判闲置」
/// 之后，漏计不再能把在用的兼容段判成闲置，所以不为它加协调。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompatHits {
    /// 累计命中（量纲是 conntrack 流数，见 [`compat_counter`]）
    pub total: u64,
    /// 上一轮采到的活计数：算增量用。表被 flush 过 ⇒ 活计数变小 ⇒ 按「从 0 重新计」处理
    pub seen: u64,
    /// 最近一次 `total` 涨过的时刻（`None` = 从没命中过）
    pub last_hit_at: Option<String>,
    /// 开始统计的时刻（第一次采样）——「连续 30 天为 0」的起点
    pub since: String,
}

impl CompatHits {
    /// 「从这一刻起没再命中过」：有过命中就是最近那一次，否则是开始统计的时刻。
    /// 30 天门禁从它算起（读取侧在 `bui status` / `bui set hy2-resi-compat off`）。
    pub fn quiet_since(&self) -> &str {
        self.last_hit_at.as_deref().unwrap_or(&self.since)
    }

    /// **兼容段可以自动判闲置了吗**（2026-09-17 裁决，T14 的 30 天门禁与
    /// `bui set hy2-resi-compat off` 的 `--force` 判据都读它）。
    ///
    /// 判据是「**一次都没命中过** 且静默满 [`COMPAT_IDLE_DAYS`] 天」——
    /// `total > 0` 就**永不**自动判闲置。
    ///
    /// 为什么不能只看 [`quiet_since`](Self::quiet_since)：兼容段的 counter 是 **nat 链**
    /// 计数，只计每条 conntrack 流的**首包**（`man nft`：「Only the first packet of a
    /// connection …」）。一个 24×7 不断线的 4.0 客户端（订阅里是裸 `40000+i`、没有
    /// `mport`）只在建连那一刻记 1 次，之后几十天一动不动 ⇒ `last_hit_at` 一路变旧 ⇒
    /// 只看静默时长就会把**正在用**的兼容段判成闲置并关掉，那批还没刷订阅的 4.0 住宅
    /// 用户当场断联。所以取安全方向：命中过就只能人工 `--force` 下线（T14 打印
    /// `total` / [`last_hit_at`](Self::last_hit_at) / [`since`](Self::since) 三个值，
    /// 由人判断）。
    pub fn idle_for_takedown(&self, now: OffsetDateTime) -> bool {
        if self.total > 0 {
            return false;
        }
        match parse_rfc3339(self.quiet_since()) {
            // 起算时刻读不出来（字段被人改坏）⇒ 不判闲置：误判方向是危险的那一侧
            None => false,
            Some(t) => now - t >= time::Duration::days(COMPAT_IDLE_DAYS),
        }
    }
}

/// 兼容段自动判闲置要静默多少天（spec §2.4：「连续 30 天为 0」）。
/// 与 [`CompatHits::idle_for_takedown`] 一起被 T14 的门禁消费。
pub const COMPAT_IDLE_DAYS: i64 = 30;

/// 取持久化的兼容段命中数（`None` = 还没采过一轮，或字段坏了）。
pub fn compat_hits(rt: &crate::state::runtime::RuntimeData) -> Option<CompatHits> {
    rt.extra
        .get(COMPAT_HITS_KEY)
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
}

/// 把这一轮采到的活计数并进累计值（纯函数）。
///
/// 活计数比上一轮**小**就是「中间被 flush 过」（别的路径重放、改端口、开机），此时增量按活
/// 计数本身算——把它当成「从 0 重新计到 live」，既不重复计也不漏计那一段。
///
/// `flushed_after` = **这一轮自己重放过**（采样之后 `nft -f` 的第一句 `flush table` 把
/// counter 清回 0）：此时把 `seen` 记成 0，下一轮的增量就是精确值。少了这一项，
/// 「flush 之后的新计数在一轮内恰好越过旧值」会漏计（正好等于旧值时连 `last_hit_at`
/// 都不动），方向正是危险那一侧。
pub fn accumulate(
    prev: Option<CompatHits>,
    live: u64,
    now: OffsetDateTime,
    flushed_after: bool,
) -> CompatHits {
    let mut h = prev.unwrap_or_else(|| CompatHits {
        since: fmt_rfc3339(now),
        ..Default::default()
    });
    let delta = if live >= h.seen { live - h.seen } else { live };
    h.seen = if flushed_after { 0 } else { live };
    if delta > 0 {
        h.total += delta;
        h.last_hit_at = Some(fmt_rfc3339(now));
    }
    h
}

/// http 鉴权连不上的判据（spec §3.2）：`auth.type: http` 下内核每条
/// 连接都要打一次 `127.0.0.1:AUTH_HTTP_PORT`，守护进程没在听（或应答超时）就是全员登录失败。
/// **检测与告警在日志哨兵**（`modules::sentinel`，5 秒增量读 journald）；这里只留判据与门槛，
/// 哨兵的签名表引用它们。
///
/// **4.1 起只喂直连**（`hysteria-server`）：住宅那一路是 sing-box 的静态凭据池，没有 auth 段，
/// 鉴权失败也不打任何日志（auth 不命中走 masquerade）⇒ 判据在它上面失去对象
/// （spec §6、§8.1；作用域在 `sentinel::signature::classify` 里收）。
/// 多少条鉴权连接失败（60 秒内）才算一次事件。
pub const AUTH_HTTP_FAIL_THRESHOLD: u32 = 3;
/// 「这一行说的是鉴权请求」的判据：内核把整个 URL 打进错误里，所以路径或端口任一命中即可。
pub const AUTH_HTTP_PATH_MARKER: &str = "/auth";
/// 「这一行说的是连不上 / 超时」的判据。
pub const AUTH_HTTP_FAIL_MARKERS: [&str; 4] = [
    "connection refused",
    "deadline exceeded",
    "timeout",
    "no route to host",
];

/// 一行 journal 是不是「鉴权请求连不上 / 超时」（纯函数）。日志哨兵（`modules::sentinel`）逐行用它。
///
/// 两类标记都要命中才算：只匹配 "connection refused" 会把住宅上游、relay 的连接错误
/// 一起数进来；只匹配 "/auth" 会把正常的鉴权日志数进来。
pub fn is_auth_http_failure(line: &str) -> bool {
    let port = format!(":{}", bui_schema::render::hysteria::AUTH_HTTP_PORT);
    let lower = line.to_ascii_lowercase();
    (lower.contains(AUTH_HTTP_PATH_MARKER) || lower.contains(&port))
        && AUTH_HTTP_FAIL_MARKERS.iter().any(|m| lower.contains(m))
}

/// 数一段 journal 里「鉴权请求连不上 / 超时」的行数（纯函数，便于单测）。
pub fn count_auth_http_failures(log: &str) -> u32 {
    log.lines().filter(|l| is_auth_http_failure(l)).count() as u32
}

/// 一个单元的孤儿链自愈记录（累计次数 + 最近一次的时刻与清掉的链）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ChainHeal {
    pub at: String,
    pub count: u32,
    /// 最近一次清理做过的事（`modules::portjump::cleanup` 的返回值，每行一条）
    pub done: Vec<String>,
}

/// 冷却判据：`last` 是上次自愈时刻（`None` = 没治过）。
pub fn should_heal(last: Option<OffsetDateTime>, now: OffsetDateTime) -> bool {
    match last {
        None => true,
        Some(t) => now - t >= time::Duration::minutes(HEAL_COOLDOWN_MINUTES),
    }
}

/// 「这个单元正卡在崩溃循环里」：`ActiveState` 是 `failed`（已撞上 systemd 的 start limit）
/// 或 `activating`（`auto-restart` 间隙）。
///
/// **不看 `inactive`**：运维 `systemctl stop` 停掉的单元就是 inactive，自愈不该把它拉起来。
pub fn is_crash_looping(state: Option<&str>) -> bool {
    matches!(state, Some("failed") | Some("activating"))
}

/// 一个 hysteria 单元的自愈：`ActiveState` 判崩溃循环 → 日志里找 [`CHAIN_MARKER`] →
/// 清本实例的孤儿链 → `reset-failed` + `restart`。返回 `Some(清理做过的事)` 表示治过一次。
///
/// `reset-failed` 是必须的：52 次崩溃早已撞上 `StartLimitBurst`，不清计数直接 `restart`
/// 会被 systemd 以 "start request repeated too quickly" 挡掉。
fn heal_chain_conflict(
    host: &dyn Host,
    paths: &Paths,
    unit: &str,
    conf: &str,
) -> Option<Vec<String>> {
    let active_state = host.unit_property(unit, "ActiveState").ok().flatten();
    if !is_crash_looping(active_state.as_deref()) {
        return None;
    }
    let log = host
        .run(
            "journalctl",
            &["-u", unit, "-n", JOURNAL_LINES, "--no-pager"],
        )
        .ok()?;
    if !log.ok() || !log.stdout.contains(CHAIN_MARKER) {
        return None;
    }
    tracing::warn!(
        unit = %unit,
        "日志含「{CHAIN_MARKER}」：清本实例的端口跳跃孤儿链后重启"
    );
    let done = crate::modules::portjump::cleanup(host, &paths.base_dir.join(conf));
    let _ = host.systemd("reset-failed", unit);
    let _ = host.systemd("restart", unit);
    Some(done)
}

/// 一个探测目标：单元名 + 协议 + 监听端口。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub unit: String,
    pub proto: Proto,
    pub port: u16,
}

/// 状态机对一轮探测的裁决。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Healthy,
    Failing { fails: u32 },
    Restart,
    Backoff,
}

pub struct WatchdogModule;

impl Module for WatchdogModule {
    fn name(&self) -> &'static str {
        "watchdog"
    }

    /// watchdog 没有期望项，只有后台任务。
    fn render(&self, _s: &State, _ctx: &RenderCtx) -> Vec<Artifact> {
        Vec::new()
    }

    fn spawn(&self, ctx: DaemonCtx) -> Vec<tokio::task::JoinHandle<()>> {
        vec![tokio::spawn(watch_loop(ctx))]
    }
}

/// 四类内核 + 每个住宅实例，各自的监听端口：relay 与 xray 是 TCP，hysteria 是 UDP。
/// 端口取自期望态（`state.node.ports` + 槽位表），relay 的入站端口是基准常量。
pub fn targets(state: &State) -> Vec<Target> {
    let mut v = vec![Target {
        unit: "hysteria-server".into(),
        proto: Proto::Udp,
        port: state.node.ports.hy2,
    }];
    // 4.1：住宅只有一个实例，监听期望态里那个单端口（整段跳跃由 nft 表 REDIRECT 过来）
    v.push(Target {
        unit: "hysteria-residential".into(),
        proto: Proto::Udp,
        port: state.node.ports.hy2_resi,
    });
    v.push(Target {
        unit: "xray".into(),
        proto: Proto::Tcp,
        port: state.node.ports.reality_direct,
    });
    v.push(Target {
        unit: "b-ui-relay".into(),
        proto: Proto::Tcp,
        port: crate::modules::core_files::RELAY_LISTEN_PORT,
    });
    v
}

/// 纯状态机：返回裁决并就地更新记录（不碰机器、不落盘，便于单测）。
pub fn decide(
    rec: &mut WatchdogRecord,
    alive: bool,
    listening: bool,
    now: OffsetDateTime,
) -> Decision {
    // 进程不在 → systemd 的 Restart=always 负责，watchdog 不插手（core.sh:1065-1068）。
    // **这与 spec §3.4「检查四个内核进程存活 + 监听端口」的字面读法不同，是有意为之**（v3 同语义）：
    // watchdog 只解决「进程还在、端口却不 listen」这一类僵死；`!alive` 交给 systemd，重复插手会和
    // `Restart=always` 抢着重启。单元不 active 这件事本身由 `/api/health` 的 services 报 degraded，
    // 不会被吞掉。M5 验收时按这一段口径核对，不要按 spec 字面要求 watchdog 去 start 单元。
    if !alive || listening {
        rec.fails = 0;
        return Decision::Healthy;
    }
    rec.fails += 1;
    if rec.fails < FAIL_THRESHOLD {
        return Decision::Failing { fails: rec.fails };
    }
    if let Some(until) = rec.backoff_until.as_deref().and_then(parse_rfc3339) {
        if now < until {
            return Decision::Backoff;
        }
    }
    rec.fails = 0;
    rec.restarts += 1;
    // restarts 是累计值（不清零），所以退避取 1/2/4 后停在 4。
    let minutes = BACKOFF_MINUTES[(rec.restarts as usize - 1).min(BACKOFF_MINUTES.len() - 1)];
    rec.last_restart_at = Some(fmt_rfc3339(now));
    rec.backoff_until = Some(fmt_rfc3339(now + time::Duration::minutes(minutes)));
    Decision::Restart
}

/// 一轮跑完的时间戳：`last_run_at` = 这一轮的时刻，`next_run_at` = 再加一个间隔。
pub fn run_stamp(now: OffsetDateTime) -> serde_json::Value {
    serde_json::json!({
        "last_run_at": fmt_rfc3339(now),
        "next_run_at": fmt_rfc3339(now + time::Duration::seconds(INTERVAL_SECS as i64)),
    })
}

/// 跑一轮：先治 hysteria 的端口跳跃孤儿链崩溃循环（[`heal_chain_conflict`]），再校验住宅
/// 跳跃那张 nft 表（[`check_nft`]，顺带采一次兼容段命中数），再读单元状态与监听端口
/// （都经 `Host`，时钟也取 `host.now()`），按裁决重启，落 `runtime.json`。
///
/// 重启过 `hysteria-residential` 就广播 [`Event::Hy2ResiRestarted`]：不开 `cache_file`
/// （spec §14 裁决 1）⇒ 重启把每个 `gate-<id>` 打回 `default = deny`，不重放就是全体住宅
/// HY2 用户被拒到下一轮 60 秒安全网，而且没有任何告警说明原因。relay 早就为完全一样的
/// 来路补过重放（`residential::health` 规则 6a），住宅 HY2 这条在这里补。
///
/// **4.1 起这条广播只有一条来路**：「进程在、端口不 listen」连续两轮那条。孤儿链自愈随
/// [`HY2_CONFIGS`] 收成直连一项后只会重启 `hysteria-server`（住宅换 sing-box、不建 NAT
/// 规则），而 [`check_nft`] 的整表重放不重启任何单元、门位不会回落 ⇒ 都不该广播。
pub async fn check_once(ctx: &DaemonCtx) -> anyhow::Result<Vec<(String, Decision)>> {
    let state = ctx.store.read().await;
    let targets = targets(&state);
    let rt = ctx.runtime.read().await;
    let mut heals: std::collections::BTreeMap<String, ChainHeal> = rt
        .extra
        .get(HEAL_KEY)
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    let mut nft_heals: std::collections::BTreeMap<String, ChainHeal> = rt
        .extra
        .get(NFT_HEAL_KEY)
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    let hits_last = compat_hits(&rt);
    let mut records = rt.watchdog;
    let host = ctx.host.clone();
    let paths = ctx.paths.clone();
    let st = state.clone();
    let (decisions, records, heals, nft, now, restarted) =
        tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            let now = host.now();
            // 本轮真的被 `systemctl restart` 过的单元（自愈 + 监听失活两条来路都记）
            let mut restarted: std::collections::BTreeSet<String> = Default::default();
            // 孤儿链自愈排在监听探测之前：崩溃循环里的实例根本没进程，按端口判只会得出
            // `Healthy`（`!alive` 交给 systemd），而 systemd 的 `Restart=always` 在这个错误上
            // 永远治不好——链不清掉，下一次 `-N` 还是 "Chain already exists"。
            for (unit, conf) in HY2_CONFIGS {
                let last = heals.get(unit).and_then(|h| parse_rfc3339(&h.at));
                if !should_heal(last, now) {
                    continue;
                }
                if let Some(done) = heal_chain_conflict(&*host, &paths, unit, conf) {
                    let rec = heals.entry(unit.to_string()).or_default();
                    rec.at = fmt_rfc3339(now);
                    rec.count += 1;
                    rec.done = done;
                    restarted.insert(unit.to_string());
                }
            }
            // 住宅那一侧的自愈：表被人删了 / 规则不符就整表重放（采样在重放之前）
            let nft = check_nft(&*host, &st);
            let udp = host.listening_ports(Proto::Udp).unwrap_or_default();
            let tcp = host.listening_ports(Proto::Tcp).unwrap_or_default();
            let mut out = Vec::with_capacity(targets.len());
            for t in &targets {
                let rec = records.entry(t.unit.clone()).or_default();
                let alive = host.unit_is_active(&t.unit).unwrap_or(false);
                let listening = match t.proto {
                    Proto::Udp => udp.contains(&t.port),
                    Proto::Tcp => tcp.contains(&t.port),
                };
                let d = decide(rec, alive, listening, now);
                if d == Decision::Restart {
                    tracing::warn!(unit = %t.unit, port = t.port, "监听失活连续 2 轮，重启");
                    let _ = host.systemd("restart", &t.unit);
                    restarted.insert(t.unit.clone());
                }
                out.push((t.unit.clone(), d));
            }
            Ok((out, records, heals, nft, now, restarted))
        })
        .await??;
    // 门位在重启时全部回落 `deny` ⇒ 通知 `gates::replay_loop` 立刻重放（见本函数文档）
    if restarted.contains("hysteria-residential") {
        ctx.bus.send(crate::api::Event::Hy2ResiRestarted);
    }
    // relay 被重启过 = 一次**全池**切换：它的每个池 selector 的 `now` 都回落到配置里的
    // default，归因的时间戳门要当场对全池记一次（2026-09-18 第三次裁决 ③），否则这一刻
    // 之前产生的 relay 错误行会被路径 B 记到新成员头上。选择本身的重放由
    // `residential::health` 的规则 6a 与 `drive_slots` 下一轮兜底 —— 这条来路按既有口径
    // **不发** `Event::RelayRestarted`（§C 末段）。
    if restarted.contains("b-ui-relay") {
        let pools = crate::modules::residential::state::all_pool_selectors(&state);
        crate::modules::residential::state::mark_pools_switch(&ctx.runtime, &pools, now).await;
    }
    let nft_record = nft_event(&nft, &state, &nft_heals, now);
    // 命中数每轮都并一次（不只在重放那一轮）：采样点就是重放之前的那一刻，
    // 这一轮自己重放过就把 `seen` 归零（`nft -f` 先 flush table），中途被别的路径
    // flush 过时按「从 0 重新计」处理，见 [`accumulate`]。
    let hits = nft.compat_live.map(|live| {
        accumulate(
            hits_last,
            live,
            now,
            nft.verdict == NftVerdict::Replayed, // 重放失败是原子回滚，表没被 flush
        )
    });
    ctx.runtime
        .update(|r| {
            r.watchdog = records;
            r.extra.insert(RUN_KEY.into(), run_stamp(now));
            // 治过才写这个键：没治过的机器上 `runtime.json` 里连它都不该出现
            if !heals.is_empty() {
                if let Ok(v) = serde_json::to_value(&heals) {
                    r.extra.insert(HEAL_KEY.into(), v);
                }
            }
            if let Some((bucket, rec, inc)) = nft_record {
                nft_heals.insert(bucket.to_string(), rec);
                if let Ok(v) = serde_json::to_value(&nft_heals) {
                    r.extra.insert(NFT_HEAL_KEY.into(), v);
                }
                crate::modules::sentinel::incidents::push(r, inc);
            }
            if let Some(h) = hits {
                if let Ok(v) = serde_json::to_value(&h) {
                    r.extra.insert(COMPAT_HITS_KEY.into(), v);
                }
            }
        })
        .await;
    Ok(decisions)
}

pub async fn watch_loop(ctx: DaemonCtx) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(INTERVAL_SECS));
    loop {
        tick.tick().await;
        if let Err(e) = check_once(&ctx).await {
            tracing::warn!(error = %e, "watchdog 一轮检查失败");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::EventBus;
    use crate::state::runtime::{Runtime, WatchdogRecord};
    use crate::state::store::Store;
    use crate::sys::fake::FakeHost;
    use crate::testutil::sample_state;
    use bui_schema::paths::Paths;
    use pretty_assertions::assert_eq;
    use std::sync::Arc;
    use time::macros::datetime;

    fn t0() -> time::OffsetDateTime {
        datetime!(2026-09-11 00:00:00 UTC)
    }

    #[test]
    fn targets_cover_four_kernels_with_the_right_protocol() {
        assert_eq!(
            targets(&sample_state()),
            vec![
                Target {
                    unit: "hysteria-server".into(),
                    proto: Proto::Udp,
                    port: 10000
                },
                Target {
                    unit: "hysteria-residential".into(),
                    proto: Proto::Udp,
                    port: 40000
                },
                Target {
                    unit: "xray".into(),
                    proto: Proto::Tcp,
                    port: 10001
                },
                Target {
                    unit: "b-ui-relay".into(),
                    proto: Proto::Tcp,
                    port: 2080
                },
            ]
        );
    }

    #[test]
    fn healthy_resets_the_counter() {
        let mut rec = WatchdogRecord {
            fails: 1,
            restarts: 3,
            last_restart_at: None,
            backoff_until: None,
        };
        assert_eq!(decide(&mut rec, true, true, t0()), Decision::Healthy);
        assert_eq!(rec.fails, 0);
        assert_eq!(rec.restarts, 3, "重启次数是累计值，不清零");
    }

    #[test]
    fn dead_process_is_left_to_systemd() {
        let mut rec = WatchdogRecord::default();
        // 进程不在 → systemd Restart=always 会管，watchdog 只清计数（移植 core.sh:1065-1068）
        assert_eq!(decide(&mut rec, false, false, t0()), Decision::Healthy);
        assert_eq!(rec.fails, 0);
    }

    #[test]
    fn two_consecutive_half_dead_rounds_trigger_a_restart_then_backoff() {
        let mut rec = WatchdogRecord::default();
        assert_eq!(
            decide(&mut rec, true, false, t0()),
            Decision::Failing { fails: 1 }
        );
        let r = decide(&mut rec, true, false, t0() + time::Duration::seconds(60));
        assert_eq!(r, Decision::Restart);
        assert_eq!(rec.restarts, 1);
        assert_eq!(rec.fails, 0);
        assert_eq!(rec.last_restart_at.as_deref(), Some("2026-09-11T00:01:00Z"));
        assert_eq!(
            rec.backoff_until.as_deref(),
            Some("2026-09-11T00:02:00Z"),
            "第一次退避 1 分钟"
        );
        // 退避窗口内即使再连续两轮失败也不重启
        assert_eq!(
            decide(&mut rec, true, false, t0() + time::Duration::seconds(70)),
            Decision::Failing { fails: 1 }
        );
        assert_eq!(
            decide(&mut rec, true, false, t0() + time::Duration::seconds(80)),
            Decision::Backoff
        );
        assert_eq!(rec.restarts, 1);
    }

    #[test]
    fn backoff_grows_one_two_four_then_stays_at_four() {
        let mut rec = WatchdogRecord::default();
        let mut now = t0();
        let mut seen = Vec::new();
        for _ in 0..4 {
            // 每轮：两次失败触发一次重启，然后跳过退避窗口
            decide(&mut rec, true, false, now);
            now += time::Duration::seconds(60);
            assert_eq!(decide(&mut rec, true, false, now), Decision::Restart);
            let until = time::OffsetDateTime::parse(
                rec.backoff_until.as_deref().unwrap(),
                &time::format_description::well_known::Rfc3339,
            )
            .unwrap();
            seen.push((until - now).whole_minutes());
            now = until + time::Duration::seconds(1);
        }
        assert_eq!(seen, vec![1, 2, 4, 4]);
        assert_eq!(rec.restarts, 4);
    }

    /// 只看真正动了机器的操作：孤儿链自愈会先读一次崩溃单元的 journal，这些只读调用不属于任何一条动作断言
    fn acting_ops(host: &FakeHost) -> Vec<String> {
        host.ops()
            .into_iter()
            .filter(|o| !o.starts_with("run:journalctl"))
            .collect()
    }

    async fn ctx(host: Arc<FakeHost>) -> (crate::reconcile::DaemonCtx, tempfile::TempDir) {
        ctx_with_state(host, sample_state()).await
    }

    async fn ctx_with_state(
        host: Arc<FakeHost>,
        state: State,
    ) -> (crate::reconcile::DaemonCtx, tempfile::TempDir) {
        let d = tempfile::tempdir().unwrap();
        let store = Store::create(d.path().join("state.json"), state)
            .await
            .unwrap();
        let runtime = Runtime::load(d.path().join("runtime.json"));
        (
            crate::reconcile::DaemonCtx {
                store,
                runtime,
                bus: EventBus::new(),
                host,
                paths: Paths::default_server(),
            },
            d,
        )
    }

    #[tokio::test]
    async fn check_once_restarts_only_the_half_dead_unit() {
        let host = Arc::new(FakeHost::new());
        host.with(|i| {
            for u in [
                "hysteria-server",
                "hysteria-residential",
                "xray",
                "b-ui-relay",
            ] {
                i.units_active.insert(format!("{u}.service"));
            }
            i.listening
                .insert(Proto::Udp, [40000].into_iter().collect()); // 10000 失活
            i.listening
                .insert(Proto::Tcp, [10001, 2080].into_iter().collect());
        });
        let (c, _d) = ctx(host.clone()).await;
        let first = check_once(&c).await.unwrap();
        assert_eq!(
            first[0],
            (
                "hysteria-server".to_string(),
                Decision::Failing { fails: 1 }
            )
        );
        assert_eq!(
            first[1],
            ("hysteria-residential".to_string(), Decision::Healthy)
        );
        assert!(!host.ops().iter().any(|o| o.starts_with("systemd:restart")));
        host.advance(60);
        let second = check_once(&c).await.unwrap();
        assert_eq!(
            second[0],
            ("hysteria-server".to_string(), Decision::Restart)
        );
        assert_eq!(acting_ops(&host), vec!["systemd:restart:hysteria-server"]);
        let rt = c.runtime.read().await;
        assert_eq!(rt.watchdog["hysteria-server"].restarts, 1);
        assert_eq!(rt.watchdog["xray"].fails, 0);
    }

    /// 看门狗重启住宅入站之后必须广播 `Event::Hy2ResiRestarted`（第七波复核）：
    /// 不开 `cache_file`（spec §14 裁决 1）⇒ 重启把每个 `gate-<id>` selector 打回
    /// `default = deny`，没人重放就是全体住宅 HY2 用户被拒到下一轮 60 秒安全网，
    /// 而且没有任何告警说明原因。relay 早就为完全一样的两条来路补过重放。
    #[tokio::test]
    async fn restarting_the_residential_inbound_is_announced_so_the_gates_get_replayed() {
        let host = Arc::new(FakeHost::new());
        host.with(|i| {
            for u in [
                "hysteria-server",
                "hysteria-residential",
                "xray",
                "b-ui-relay",
            ] {
                i.units_active.insert(format!("{u}.service"));
            }
            // 40000 失活（进程在、端口不 listen）⇒ 连续两轮后重启住宅入站
            i.listening
                .insert(Proto::Udp, [10000].into_iter().collect());
            i.listening
                .insert(Proto::Tcp, [10001, 2080].into_iter().collect());
        });
        let (c, _d) = ctx(host.clone()).await;
        let mut rx = c.bus.subscribe();
        check_once(&c).await.unwrap();
        assert!(
            rx.try_recv().is_err(),
            "第一轮只是 Failing，没重启 ⇒ 不许广播"
        );
        host.advance(60);
        let second = check_once(&c).await.unwrap();
        assert_eq!(
            second[1],
            ("hysteria-residential".to_string(), Decision::Restart)
        );
        assert_eq!(
            rx.try_recv().ok(),
            Some(crate::api::Event::Hy2ResiRestarted),
            "重启了住宅入站却不广播 ⇒ 门全停在 deny、没人重放"
        );
    }

    /// 2026-09-18 第三次裁决 ③：看门狗重启 `b-ui-relay` = 一次**全池**切换。不开
    /// `cache_file` ⇒ 重启把**每个**池 selector 的 `now` 都打回配置里的 default，
    /// 而这条来路按既有口径**不发** `Event::RelayRestarted`（重放交给规则 6a /
    /// `drive_slots` 下一轮兜底）—— 那就更得当场把归因的时间戳门记上，否则这一刻
    /// 之前产生的 relay 错误行会被路径 B 整批记到重启后 default 的那条上游头上。
    #[tokio::test]
    async fn restarting_the_relay_stamps_every_pool_for_the_attribution_gate() {
        let host = Arc::new(FakeHost::new());
        host.with(|i| {
            for u in [
                "hysteria-server",
                "hysteria-residential",
                "xray",
                "b-ui-relay",
            ] {
                i.units_active.insert(format!("{u}.service"));
            }
            // relay 进程在、2080 不 listen ⇒ 连续两轮后重启它
            i.listening
                .insert(Proto::Udp, [10000, 40000].into_iter().collect());
            i.listening
                .insert(Proto::Tcp, [10001].into_iter().collect());
        });
        // 池有效 + 两个槽 ⇒ 全池 = 全局池 + slot-0-pool + slot-1-pool
        let mut state = crate::modules::residential::sample_state_with_pool();
        let up = state.residential.groups["default"].upstreams[0].id;
        state.residential.slots = (0..2)
            .map(|index| bui_schema::model::Slot {
                index,
                upstream_id: up,
            })
            .collect();
        let (c, _d) = ctx_with_state(host.clone(), state).await;
        let mut rx = c.bus.subscribe();
        check_once(&c).await.unwrap();
        assert!(
            crate::modules::residential::state::read(&c.runtime)
                .await
                .pool_switch_at
                .is_empty(),
            "第一轮只是 Failing、没重启 ⇒ 一笔都不许记"
        );
        host.advance(60);
        let second = check_once(&c).await.unwrap();
        assert!(
            second.contains(&("b-ui-relay".to_string(), Decision::Restart)),
            "{second:?}"
        );
        assert!(
            rx.try_recv().is_err(),
            "这条来路不发 Event::RelayRestarted（口径没变）"
        );
        assert_eq!(
            crate::modules::residential::state::read(&c.runtime)
                .await
                .pool_switch_at
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            vec![
                crate::modules::residential::POOL.to_string(),
                crate::modules::residential::slot_selector(0),
                crate::modules::residential::slot_selector(1),
            ],
            "relay 重启 = 全池切换：每个池 selector 都要记一笔"
        );
    }

    /// 合并裁决（T11 ↔ T12，2026-09-17）：孤儿链自愈这条来路在 4.1 **不再**广播。
    /// [`HY2_CONFIGS`] 收成直连一项后它只会重启 `hysteria-server`，住宅换 sing-box
    /// 不建 NAT 规则、没有孤儿链可清；4.0 起那条「自愈完也广播」的用例（断言这一路
    /// 重启了 `hysteria-residential`）按 T12 的口径已不可满足，改成正面钉住新口径：
    /// 自愈这一路**只**碰直连实例、住宅一个命令都收不到、门位也不白重放一轮。
    #[tokio::test]
    async fn healing_a_crash_loop_restarts_only_the_direct_inbound_and_announces_nothing() {
        let host = crash_looping_host();
        let (c, _d) = ctx(host.clone()).await;
        let mut rx = c.bus.subscribe();
        check_once(&c).await.unwrap();
        assert!(
            host.ops()
                .iter()
                .any(|o| o == "systemd:restart:hysteria-server"),
            "前提：自愈这一路重启了直连实例"
        );
        assert!(
            !host
                .ops()
                .iter()
                .any(|o| o == "systemd:restart:hysteria-residential"),
            "4.1 的孤儿链自愈不许碰住宅入站（它不建 NAT 规则、没有孤儿链可清）"
        );
        assert!(
            rx.try_recv().is_err(),
            "住宅入站没重启 ⇒ 不许广播（白重放一轮门位 = 多打一次 GET /proxies 与若干 PUT）"
        );
    }

    /// 只重启直连实例时不许广播（白重放一轮门位 = 多打一次 `GET /proxies` 与若干 PUT）
    #[tokio::test]
    async fn restarting_only_the_direct_inbound_announces_nothing() {
        let host = Arc::new(FakeHost::new());
        host.with(|i| {
            for u in [
                "hysteria-server",
                "hysteria-residential",
                "xray",
                "b-ui-relay",
            ] {
                i.units_active.insert(format!("{u}.service"));
            }
            i.listening
                .insert(Proto::Udp, [40000].into_iter().collect()); // 10000 失活
            i.listening
                .insert(Proto::Tcp, [10001, 2080].into_iter().collect());
        });
        let (c, _d) = ctx(host.clone()).await;
        let mut rx = c.bus.subscribe();
        check_once(&c).await.unwrap();
        host.advance(60);
        check_once(&c).await.unwrap();
        assert!(
            host.ops()
                .iter()
                .any(|o| o == "systemd:restart:hysteria-server"),
            "前提：重启了直连实例"
        );
        assert!(rx.try_recv().is_err(), "住宅入站没重启 ⇒ 不许广播");
    }

    #[test]
    fn only_a_failed_or_auto_restarting_unit_counts_as_a_crash_loop() {
        assert!(is_crash_looping(Some("failed")));
        assert!(is_crash_looping(Some("activating")));
        // 运维手动 stop 的单元不许被自愈拉起来
        assert!(!is_crash_looping(Some("inactive")));
        assert!(!is_crash_looping(Some("active")));
        assert!(!is_crash_looping(Some("deactivating")));
        assert!(!is_crash_looping(None));
    }

    #[test]
    fn healing_the_same_unit_again_waits_out_the_cooldown() {
        assert!(should_heal(None, t0()), "没治过就治");
        assert!(!should_heal(Some(t0()), t0() + time::Duration::minutes(9)));
        assert!(should_heal(
            Some(t0()),
            t0() + time::Duration::minutes(HEAL_COOLDOWN_MINUTES)
        ));
    }

    /// 播种真机事故现场（bwg-rick 2026-09-12 20:33 UTC）：一个 hysteria 实例在崩溃循环，
    /// journal 里是「ip6tables: Chain already exists」，`ip6tables` 的 nat 表里只剩 OUTPUT
    /// 一条跳转（链本身是空的）。
    ///
    /// **4.1 把现场换到直连实例**（`hysteria-server` + `config.yaml`）：住宅换成 sing-box 之后
    /// 不再自建 NAT 规则、也就没有孤儿链，[`HY2_CONFIGS`] 只剩直连这一项。事故的形状一字不变，
    /// 只是主角换了 —— 直连的 base 端口是 10000、跳跃段 20000-30000。
    fn crash_looping_host() -> Arc<FakeHost> {
        let host = Arc::new(FakeHost::new());
        host.with(|i| {
            i.units_active.insert("hysteria-residential.service".into());
            i.units_active.insert("xray.service".into());
            i.units_active.insert("b-ui-relay.service".into());
            i.unit_props.insert(
                ("hysteria-residential.service".into(), "ActiveState".into()),
                "active".into(),
            );
            i.unit_props.insert(
                ("hysteria-server.service".into(), "ActiveState".into()),
                "failed".into(),
            );
            i.scripted.push((
                "journalctl -u hysteria-server".into(),
                crate::sys::CmdOut::success(
                    "hysteria[1234]: invalid config: listen: ip6tables [-w -t nat -N \
                     HYSTERIA-PR-c66a02d9]: exit status 1: ip6tables: Chain already exists\n",
                ),
            ));
            i.which.insert("ip6tables".into());
            i.scripted.push((
                "ip6tables -t nat -S".into(),
                crate::sys::CmdOut::success(
                    "-N HYSTERIA-PR-c66a02d9\n-A OUTPUT -p udp -m udp --dport 20000:30000 \
                     -j HYSTERIA-PR-c66a02d9\n",
                ),
            ));
            i.files.insert(
                "/opt/b-ui/config.yaml".into(),
                (b"listen: :10000,20000-30000\n".to_vec(), 0o600),
            );
            i.listening
                .insert(Proto::Udp, [40000].into_iter().collect());
            i.listening
                .insert(Proto::Tcp, [10001, 2080].into_iter().collect());
        });
        host
    }

    /// 事故回归：崩溃循环 + 日志含「Chain already exists」→ 先清本实例的孤儿链，
    /// 再 `reset-failed`（52 次重启早撞上 start limit，不清计数 restart 会被挡）+ `restart`，
    /// 并把事件记进 `runtime.json`。住宅实例（active，且 4.1 根本不建 NAT 规则）
    /// 一个命令都不许收到。
    #[tokio::test]
    async fn a_chain_conflict_crash_loop_is_healed_before_the_port_probe() {
        let host = crash_looping_host();
        let (c, _d) = ctx(host.clone()).await;
        check_once(&c).await.unwrap();
        let ops = acting_ops(&host);
        let i = |needle: &str| {
            ops.iter()
                .position(|o| o.contains(needle))
                .unwrap_or_else(|| panic!("没有 {needle}：{ops:?}"))
        };
        assert!(i("-D OUTPUT") < i("-X HYSTERIA-PR-c66a02d9"), "{ops:?}");
        assert!(
            i("-X HYSTERIA-PR-c66a02d9") < i("systemd:reset-failed:hysteria-server"),
            "清链必须在重启之前，否则起来照样撞同名链：{ops:?}"
        );
        assert!(
            i("systemd:reset-failed:hysteria-server") < i("systemd:restart:hysteria-server"),
            "{ops:?}"
        );
        assert!(
            !ops.iter().any(|o| o.contains("hysteria-residential")),
            "住宅实例是 active（4.1 也不建 NAT 规则），不该被碰：{ops:?}"
        );
        // 事件落盘
        let heals: std::collections::BTreeMap<String, ChainHeal> =
            serde_json::from_value(c.runtime.read().await.extra[HEAL_KEY].clone()).unwrap();
        let rec = &heals["hysteria-server"];
        assert_eq!(rec.count, 1);
        assert_eq!(rec.at, "2026-09-11T00:00:00Z");
        assert_eq!(
            rec.done,
            vec!["已清理 ip6tables nat 链 HYSTERIA-PR-c66a02d9（本实例端口跳跃孤儿）"]
        );
    }

    /// 自愈分支与 `bui hy2-prestart` 是同一个 [`crate::modules::portjump::cleanup`]：
    /// nft 后端的机器（tizi）上，崩溃循环的自愈同样要把本实例的 `hysteria_*` 表删掉，
    /// 而不是只清 iptables 链。
    #[tokio::test]
    async fn the_heal_branch_drops_this_instances_nft_table_too() {
        let host = crash_looping_host();
        host.with(|i| {
            i.which.insert("nft".into());
            i.scripted.push((
                "nft list tables".into(),
                crate::sys::CmdOut::success(
                    "table ip6 hysteria_390d4d8b\ntable ip hysteria_7c1e0f2a\n",
                ),
            ));
            i.scripted.push((
                "nft list table ip6 hysteria_390d4d8b".into(),
                crate::sys::CmdOut::success(
                    "table ip6 hysteria_390d4d8b {\n\tchain output {\n\t\tudp dport 20000-30000 \
                     redirect to :10000\n\t}\n}\n",
                ),
            ));
            i.scripted.push((
                "nft list table ip hysteria_7c1e0f2a".into(),
                crate::sys::CmdOut::success(
                    "table ip hysteria_7c1e0f2a {\n\tchain output {\n\t\tudp dport 45500-50000 \
                     redirect to :40001\n\t}\n}\n",
                ),
            ));
        });
        let (c, _d) = ctx(host.clone()).await;
        check_once(&c).await.unwrap();
        let ops = acting_ops(&host);
        assert!(
            ops.iter()
                .any(|o| o == "run:nft delete table ip6 hysteria_390d4d8b"),
            "{ops:?}"
        );
        assert!(
            !ops.iter()
                .any(|o| o.contains("delete table ip hysteria_7c1e0f2a")),
            "别的实例（4.0 遗留的 base 40001）的表不许碰：{ops:?}"
        );
        let heals: std::collections::BTreeMap<String, ChainHeal> =
            serde_json::from_value(c.runtime.read().await.extra[HEAL_KEY].clone()).unwrap();
        assert_eq!(
            heals["hysteria-server"].done,
            vec![
                "已清理 ip6tables nat 链 HYSTERIA-PR-c66a02d9（本实例端口跳跃孤儿）",
                "已删除 nft 表 ip6 hysteria_390d4d8b（本实例端口跳跃孤儿）",
            ]
        );
    }

    /// 冷却窗口内不再治第二次（清完链还起不来说明另有原因，每 60 秒 restart 一次就是
    /// 自己造崩溃循环）；过了窗口再治，计数累加。
    #[tokio::test]
    async fn the_second_round_waits_for_the_cooldown_then_heals_again() {
        let host = crash_looping_host();
        let (c, _d) = ctx(host.clone()).await;
        check_once(&c).await.unwrap();
        host.clear_ops();

        host.advance(60);
        check_once(&c).await.unwrap();
        assert!(
            acting_ops(&host)
                .iter()
                .all(|o| !o.contains("tables") && !o.contains("reset-failed")),
            "冷却期内不该再清链、也不该再走自愈的重启：{:?}",
            host.ops()
        );

        host.advance(HEAL_COOLDOWN_MINUTES * 60);
        check_once(&c).await.unwrap();
        assert!(host
            .ops()
            .iter()
            .any(|o| o == "systemd:restart:hysteria-server"));
        let heals: std::collections::BTreeMap<String, ChainHeal> =
            serde_json::from_value(c.runtime.read().await.extra[HEAL_KEY].clone()).unwrap();
        assert_eq!(heals["hysteria-server"].count, 2);
    }

    /// 崩溃循环但日志里**不是**这个错误（证书过期、端口被占…）→ 不清链、不重启：
    /// 那些错误清 nat 链治不好，`Restart=always` 与体检各归各管。
    #[tokio::test]
    async fn a_crash_loop_with_another_error_is_left_alone() {
        let host = crash_looping_host();
        host.with(|i| {
            i.scripted.clear();
            i.scripted.push((
                "journalctl -u hysteria-server".into(),
                crate::sys::CmdOut::success("hysteria: failed to load cert: no such file\n"),
            ));
        });
        let (c, _d) = ctx(host.clone()).await;
        check_once(&c).await.unwrap();
        assert!(
            host.ops()
                .iter()
                .all(|o| !o.starts_with("systemd:restart") && !o.contains("ip6tables")),
            "{:?}",
            host.ops()
        );
        assert!(!c.runtime.read().await.extra.contains_key(HEAL_KEY));
    }

    /// 运维 `systemctl stop` 停掉的实例（`ActiveState=inactive`）即使日志里还留着那句错误，
    /// 也不许被自愈拉起来。
    #[tokio::test]
    async fn a_deliberately_stopped_unit_is_not_restarted() {
        let host = crash_looping_host();
        host.with(|i| {
            i.unit_props.insert(
                ("hysteria-server.service".into(), "ActiveState".into()),
                "inactive".into(),
            );
        });
        let (c, _d) = ctx(host.clone()).await;
        check_once(&c).await.unwrap();
        assert!(
            acting_ops(&host)
                .iter()
                .all(|o| !o.contains("hysteria-server")),
            "{:?}",
            host.ops()
        );
        assert!(!c.runtime.read().await.extra.contains_key(HEAL_KEY));
    }

    #[tokio::test]
    async fn each_round_stamps_last_run_at_and_next_run_at() {
        let host = Arc::new(FakeHost::new());
        let (c, _d) = ctx(host.clone()).await;
        assert!(
            !c.runtime.read().await.extra.contains_key(RUN_KEY),
            "没跑过一轮时没有时间戳（接口那头就是 null）"
        );

        check_once(&c).await.unwrap();
        let stamp = c.runtime.read().await.extra[RUN_KEY].clone();
        assert_eq!(stamp["last_run_at"], "2026-09-11T00:00:00Z");
        assert_eq!(
            stamp["next_run_at"], "2026-09-11T00:01:00Z",
            "next = last + INTERVAL_SECS"
        );

        // 时钟推进一轮：两个字段跟着走
        host.advance(INTERVAL_SECS as i64);
        check_once(&c).await.unwrap();
        let stamp = c.runtime.read().await.extra[RUN_KEY].clone();
        assert_eq!(stamp["last_run_at"], "2026-09-11T00:01:00Z");
        assert_eq!(stamp["next_run_at"], "2026-09-11T00:02:00Z");
    }

    #[test]
    fn targets_cover_every_residential_slot_instance() {
        let mut s = crate::testutil::sample_state();
        assert_eq!(
            targets(&s)
                .iter()
                .map(|t| (t.unit.clone(), t.port))
                .collect::<Vec<_>>(),
            vec![
                ("hysteria-server".to_string(), 10000),
                ("hysteria-residential".to_string(), 40000),
                ("xray".to_string(), 10001),
                ("b-ui-relay".to_string(), 2080),
            ]
        );
        // 4.1：增槽不再多出监控目标（住宅只有一个 sing-box 实例，只听 ports.hy2_resi）
        let one = targets(&s);
        s.residential.slots = (0..3)
            .map(|i| bui_schema::model::Slot {
                index: i,
                upstream_id: uuid::Uuid::from_u128(u128::from(i) + 1),
            })
            .collect();
        let t = targets(&s);
        assert_eq!(t, one, "槽位表不许影响监控目标");
        assert_eq!(t.len(), 4);
        assert!(!t
            .iter()
            .any(|x| x.unit.starts_with("hysteria-residential-")));
        assert!(t.iter().all(|x| match x.unit.as_str() {
            "xray" | "b-ui-relay" => x.proto == Proto::Tcp,
            _ => x.proto == Proto::Udp,
        }));
    }

    /// 4.1：住宅只剩一个探测目标，孤儿链自愈只对 apernet 直连那一支有意义
    /// （sing-box 不建 NAT 规则、没有孤儿链；住宅那侧的等价自愈是 `check_nft`）。
    #[test]
    fn the_watchdog_probes_one_residential_listener() {
        let s = sample_state();
        let t = targets(&s);
        let resi: Vec<&Target> = t
            .iter()
            .filter(|x| x.unit.starts_with("hysteria-residential"))
            .collect();
        assert_eq!(resi.len(), 1);
        assert_eq!(
            (resi[0].unit.as_str(), resi[0].port, resi[0].proto),
            ("hysteria-residential", 40000, Proto::Udp)
        );
        assert!(!t.iter().any(|x| x.unit.contains("hysteria-residential-")));
        assert_eq!(HY2_CONFIGS.len(), 1);
        assert_eq!(HY2_CONFIGS[0], ("hysteria-server", "config.yaml"));
    }

    /// 真机的 `nft list table` 回显与渲染器的规则集**等价但不逐字相同**：空白被重排、
    /// `priority -100` 打成 `priority dstnat`、每条规则多一段 `counter packets N bytes N`。
    /// 逐字比会每 60 秒重放一次（顺带把兼容段的 counter 清零），所以判据是规范化后的规则。
    #[test]
    fn the_listed_table_is_compared_normalized_not_verbatim() {
        let s = sample_state();
        let want = bui_schema::render::nft::ruleset(&s.node.ports, true);
        assert_eq!(
            redirect_rules(LISTED),
            redirect_rules(&want),
            "回显格式不同不算不符"
        );
        assert_eq!(redirect_rules(&want).len(), 4);
        // 只挂 prerouting（output 链被人删了）⇒ 必须判不符：本机发往自身公网 IP 的包
        // 不过 prerouting，跳跃对本机自测直接失效
        let half = LISTED.split("\tchain output {").next().unwrap().to_string() + "}\n";
        assert_ne!(redirect_rules(&half), redirect_rules(&want));
        // 兼容段关掉时规则从 4 条变 2 条，同样判不符
        assert_ne!(
            redirect_rules(LISTED),
            redirect_rules(&bui_schema::render::nft::ruleset(&s.node.ports, false))
        );
        // **四条都挂在 prerouting 上**（有人把 output 那两条粘错了链）：条数对、端口对、
        // 目标对，只有链不对 —— 双 hook 这条硬要求只有链名进 key 才守得住
        let both_in_prerouting = LISTED.replace("chain output {", "chain prerouting {");
        assert_eq!(redirect_rules(&both_in_prerouting).len(), 4);
        assert_ne!(
            redirect_rules(&both_in_prerouting),
            redirect_rules(&want),
            "跳跃对本机自测失效（output 链没了），不许判成等价"
        );
    }

    /// 一台装着 nft、表也在位的真机回显（`priority dstnat` + counter + 制表符缩进）。
    const LISTED: &str = "\
table inet bui {
\tchain prerouting {
\t\ttype nat hook prerouting priority dstnat; policy accept;
\t\tudp dport 41000-50000 counter packets 12 bytes 480 redirect to :40000 comment \"hy2 residential hop\"
\t\tudp dport 40001-40007 counter packets 3 bytes 120 redirect to :40000 comment \"hy2 residential 4.0 compat\"
\t}
\tchain output {
\t\ttype nat hook output priority dstnat; policy accept;
\t\tudp dport 41000-50000 counter packets 0 bytes 0 redirect to :40000 comment \"hy2 residential hop (local)\"
\t\tudp dport 40001-40007 counter packets 1 bytes 40 redirect to :40000 comment \"hy2 residential 4.0 compat (local)\"
\t}
}
";

    /// 表被人删掉 ⇒ 整表重放；`nft` 不存在 ⇒ 只告警、一条命令都不发；规则一致 ⇒ 什么都不做。
    #[test]
    fn a_missing_nft_table_is_replayed_and_a_missing_nft_binary_only_alerts() {
        let s = sample_state();
        let want = bui_schema::render::nft::ruleset(&s.node.ports, true);

        let h = FakeHost::new();
        h.with(|i| {
            i.which.insert("nft".into());
            i.scripted.push((
                "nft list table inet bui".into(),
                crate::sys::CmdOut::failure(1, "Error: No such file or directory\n"),
            ));
        });
        assert_eq!(check_nft(&h, &s).verdict, NftVerdict::Replayed);
        assert!(
            h.ops().contains(&"run:nft -f -".to_string()),
            "{:?}",
            h.ops()
        );
        assert_eq!(h.stdins(), vec![("nft -f -".to_string(), want.clone())]);

        // 缺 nft：一步都做不了，只能告 Error（不许把它当成「重放失败」去跑 nft）
        let h2 = FakeHost::new();
        assert_eq!(check_nft(&h2, &s).verdict, NftVerdict::NoBinary);
        assert!(h2.ops().is_empty(), "{:?}", h2.ops());

        // 规则集一致（真机回显格式）⇒ 什么都不做，一次 `nft -f` 都没有
        let h3 = FakeHost::new();
        h3.with(|i| {
            i.which.insert("nft".into());
            i.scripted.push((
                "nft list table inet bui".into(),
                crate::sys::CmdOut::success(LISTED),
            ));
        });
        let r = check_nft(&h3, &s);
        assert_eq!(r.verdict, NftVerdict::Ok);
        assert_eq!(r.compat_live, Some(4), "兼容段两条规则的 packets 之和");
        assert!(
            !h3.ops().iter().any(|o| o.starts_with("run:nft -f")),
            "{:?}",
            h3.ops()
        );

        // 重放本身失败（内核 < 5.2 的形态）⇒ Missing，原文进事件
        let h4 = FakeHost::new();
        h4.with(|i| {
            i.which.insert("nft".into());
            i.scripted.push((
                "nft -f -".into(),
                crate::sys::CmdOut::failure(
                    1,
                    "Error: Chain of type \"nat\" is not supported, perhaps kernel support is missing?\n",
                ),
            ));
        });
        let r = check_nft(&h4, &s);
        assert_eq!(r.verdict, NftVerdict::Missing);
        assert!(r.error.unwrap().contains("Chain of type"));
    }

    /// 缺 `nft` 的事件是 **Error 级**，正文按真实量级写（带 mport 的客户端只往跳跃段发，
    /// 所以那不是「只是跳跃失效」，而是住宅全断）；同一件事 10 分钟内不重复刷。
    #[test]
    fn the_missing_nft_binary_alert_states_the_real_blast_radius() {
        let s = sample_state();
        let round = NftRound {
            verdict: NftVerdict::NoBinary,
            compat_live: None,
            error: None,
            seen_rules: 0,
        };
        let none = std::collections::BTreeMap::new();
        let (bucket, rec, inc) = nft_event(&round, &s, &none, t0()).unwrap();
        assert_eq!(inc.signature, NFT_MISSING_SIG);
        assert_eq!(inc.level, crate::modules::sentinel::incidents::Level::Error);
        assert_eq!(inc.subject, "inet bui");
        assert!(inc.result.contains("41000-50000"), "{}", inc.result);
        assert!(inc.result.contains("住宅全断"), "{}", inc.result);
        assert_eq!(rec.count, 1);
        // 冷却：同一个桶 10 分钟内不再记第二条
        let mut heals = std::collections::BTreeMap::new();
        heals.insert(bucket.to_string(), rec);
        assert!(nft_event(&round, &s, &heals, t0() + time::Duration::minutes(9)).is_none());
        let (_, rec2, _) = nft_event(
            &round,
            &s,
            &heals,
            t0() + time::Duration::minutes(HEAL_COOLDOWN_MINUTES),
        )
        .unwrap();
        assert_eq!(rec2.count, 2, "累计次数接着涨");
        // 表不在（已重放）是 Warn；重放失败是 Error
        let replayed = NftRound {
            verdict: NftVerdict::Replayed,
            compat_live: None,
            error: None,
            seen_rules: 2,
        };
        let (_, _, inc) = nft_event(&replayed, &s, &none, t0()).unwrap();
        assert_eq!(inc.signature, NFT_TABLE_MISSING_SIG);
        assert_eq!(inc.level, crate::modules::sentinel::incidents::Level::Warn);
        assert!(
            inc.result.contains("期望 4 条 redirect，实到 2"),
            "条数进正文：判成不符却恰好是期望条数时，故障是回显解析对不上而不是表被删了：{}",
            inc.result
        );
        let failed = NftRound {
            verdict: NftVerdict::Missing,
            compat_live: None,
            error: Some("Error: Chain of type \"nat\" is not supported".into()),
            seen_rules: 0,
        };
        let (_, _, inc) = nft_event(&failed, &s, &none, t0()).unwrap();
        assert_eq!(inc.level, crate::modules::sentinel::incidents::Level::Error);
        assert!(inc.result.contains("Chain of type"), "{}", inc.result);
        // 一切正常时什么都不记
        assert!(nft_event(
            &NftRound {
                verdict: NftVerdict::Ok,
                compat_live: Some(0),
                error: None,
                seen_rules: 4,
            },
            &s,
            &none,
            t0()
        )
        .is_none());
    }

    /// 冷却**按裁决分桶**：刚记过一条「已整表重放」（Warn）之后 `nft -f` 开始失败
    /// （Error，正文是「跳跃段不通」），这条 Error 必须立刻出 —— 四种裁决共用一条记录时
    /// 它会被那句让人放心的 Warn 吞掉 10 分钟，而现场是每 60 秒重放失败一次。
    #[test]
    fn an_escalation_is_never_swallowed_by_the_previous_verdicts_cooldown() {
        let s = sample_state();
        let replayed = NftRound {
            verdict: NftVerdict::Replayed,
            compat_live: Some(0),
            error: None,
            seen_rules: 0,
        };
        let (bucket, rec, inc) = nft_event(&replayed, &s, &Default::default(), t0()).unwrap();
        assert_eq!(bucket, "replayed");
        assert_eq!(inc.level, crate::modules::sentinel::incidents::Level::Warn);
        let mut heals = std::collections::BTreeMap::new();
        heals.insert(bucket.to_string(), rec);

        // 1 分钟后重放开始失败：另一个桶 ⇒ 立刻出一条 Error
        let failed = NftRound {
            verdict: NftVerdict::Missing,
            compat_live: Some(0),
            error: Some("Error: Chain of type \"nat\" is not supported".into()),
            seen_rules: 0,
        };
        let (bucket2, rec2, inc2) =
            nft_event(&failed, &s, &heals, t0() + time::Duration::minutes(1)).unwrap();
        assert_eq!(bucket2, "replay_failed");
        assert_ne!(bucket2, bucket, "两种裁决不许共用一个冷却桶");
        assert_eq!(
            inc2.level,
            crate::modules::sentinel::incidents::Level::Error
        );
        assert!(inc2.result.contains("跳跃段不通"), "{}", inc2.result);
        heals.insert(bucket2.to_string(), rec2);
        // 同一个桶接着吃冷却（每 60 秒失败一次不许刷屏）
        assert!(nft_event(&failed, &s, &heals, t0() + time::Duration::minutes(9)).is_none());
        // 「已重放」那个桶也还在冷却里（它的 10 分钟从 t0 算）
        assert!(nft_event(&replayed, &s, &heals, t0() + time::Duration::minutes(9)).is_none());
    }

    /// `nft list table` 非零但**不是**「表不在」（权限 / 并发事务）：不许冒充表被删了
    /// —— 那会每 60 秒无谓重放一次，还让 `compat_live` 恒为 `None` ⇒ 累计命中永不涨
    /// ⇒ 30 天门禁被推向「零命中」的误判。
    #[test]
    fn a_list_failure_that_is_not_a_missing_table_neither_replays_nor_samples() {
        let s = sample_state();
        let h = FakeHost::new();
        h.with(|i| {
            i.which.insert("nft".into());
            i.scripted.push((
                "nft list table inet bui".into(),
                crate::sys::CmdOut::failure(1, "Error: Operation not permitted\n"),
            ));
        });
        let r = check_nft(&h, &s);
        assert_eq!(r.verdict, NftVerdict::Unreadable);
        assert_eq!(r.compat_live, None, "读不到就不采样（也就不写累计值）");
        assert!(
            !h.ops().iter().any(|o| o.starts_with("run:nft -f")),
            "表可能好着，不许重放：{:?}",
            h.ops()
        );
        let (bucket, _, inc) = nft_event(&r, &s, &Default::default(), t0()).unwrap();
        assert_eq!(bucket, "unreadable");
        assert_eq!(inc.signature, NFT_UNREADABLE_SIG);
        assert_eq!(inc.level, crate::modules::sentinel::incidents::Level::Warn);
        assert!(
            inc.result.contains("Operation not permitted"),
            "原文照抄进正文：{}",
            inc.result
        );

        // 另外三种「读不到」的标记同样不重放（少一个标记就掉回默认方向）
        for stderr in [
            "Error: Permission denied\n",
            "Error: Could not process rule: Device or resource busy\n",
            "Error: cache initialization failed\n",
        ] {
            let h = FakeHost::new();
            h.with(|i| {
                i.which.insert("nft".into());
                i.scripted.push((
                    "nft list table inet bui".into(),
                    crate::sys::CmdOut::failure(1, stderr),
                ));
            });
            let r = check_nft(&h, &s);
            assert_eq!(r.verdict, NftVerdict::Unreadable, "{stderr}");
            assert!(
                !h.ops().iter().any(|o| o.starts_with("run:nft -f")),
                "{stderr} ⇒ {:?}",
                h.ops()
            );
        }

        // 真是「表不在」的那几种回显仍然判重放（收窄不许把它一起收掉）。第三种是
        // **locale 翻译过**的 ENOENT：守护进程的 locale 不由我们定（中文 VPS 上
        // `localectl set-locale LANG=zh_CN.UTF-8` 很常见），判据一旦沾上 glibc 的
        // strerror 文案，表被人删掉后就永远自愈不回来了。
        for out in [
            crate::sys::CmdOut::failure(1, "Error: No such file or directory\n"),
            crate::sys::CmdOut::failure(1, ""),
            crate::sys::CmdOut::failure(1, "Error: 没有那个文件或目录\n"),
        ] {
            let h = FakeHost::new();
            h.with(|i| {
                i.which.insert("nft".into());
                i.scripted
                    .push(("nft list table inet bui".into(), out.clone()));
            });
            assert_eq!(
                check_nft(&h, &s).verdict,
                NftVerdict::Replayed,
                "{:?}",
                out.stderr
            );
        }
    }

    /// 兼容段关掉之后（`system.hy2_resi_compat_ports = false`）盘上还是 4 条：判不符，
    /// 重放的 stdin 必须是**2 条**那份规则集 —— 期望态写死成 `true` 就会每 60 秒把兼容段
    /// 重放回来，与收紧后的 `firewall_ports(p, false)` 长期打架。
    #[test]
    fn the_expected_ruleset_follows_the_compat_switch() {
        let mut s = sample_state();
        s.system.hy2_resi_compat_ports = false;
        let h = FakeHost::new();
        h.with(|i| {
            i.which.insert("nft".into());
            i.scripted.push((
                "nft list table inet bui".into(),
                crate::sys::CmdOut::success(LISTED),
            ));
        });
        let r = check_nft(&h, &s);
        assert_eq!(r.verdict, NftVerdict::Replayed);
        assert_eq!(r.seen_rules, 4, "盘上实到 4 条");
        let two = bui_schema::render::nft::ruleset(&s.node.ports, false);
        assert_eq!(h.stdins(), vec![("nft -f -".to_string(), two)]);
        assert_eq!(bui_schema::render::nft::rule_count(false), 2);
        // 事件正文报的是「期望 2 条」，不是恒 4 条
        let (_, _, inc) = nft_event(&r, &s, &Default::default(), t0()).unwrap();
        assert!(
            inc.result.contains("期望 2 条 redirect，实到 4"),
            "{}",
            inc.result
        );
    }

    /// 30 天门禁的判据（2026-09-17 裁决）：**命中过就永不自动判闲置**。
    /// 兼容段的 counter 是 nat 链计数、只计每条流的首包，24×7 不断线的 4.0 客户端
    /// 只在建连那一刻记 1 次 ⇒ 只看静默时长会把在用的兼容段判成闲置并关掉。
    #[test]
    fn a_compat_segment_that_was_ever_hit_is_never_auto_idle() {
        let hit = CompatHits {
            total: 1,
            seen: 1,
            last_hit_at: Some(fmt_rfc3339(t0())),
            since: fmt_rfc3339(t0()),
        };
        assert!(
            !hit.idle_for_takedown(t0() + time::Duration::days(365)),
            "命中过一次就只能人工 --force 下线"
        );
        // 一次都没命中过：静默满 30 天才判闲置
        let quiet = CompatHits {
            total: 0,
            seen: 0,
            last_hit_at: None,
            since: fmt_rfc3339(t0()),
        };
        assert_eq!(COMPAT_IDLE_DAYS, 30);
        assert!(!quiet.idle_for_takedown(t0() + time::Duration::days(29)));
        assert!(quiet.idle_for_takedown(t0() + time::Duration::days(30)));
        assert_eq!(quiet.quiet_since(), fmt_rfc3339(t0()));
        // 起算时刻被改坏 ⇒ 不判闲置（误判方向是危险那一侧）
        let broken = CompatHits {
            since: "不是时刻".into(),
            ..quiet
        };
        assert!(!broken.idle_for_takedown(t0() + time::Duration::days(365)));
    }

    /// 兼容段命中数的累加（纯函数）：活 counter 会被 `flush table` 清零，所以判据是这份
    /// 持久值。变小 ⇒ 中途被 flush 过 ⇒ 增量按活计数本身算，既不重复也不漏。
    #[test]
    fn compat_hits_accumulate_across_every_flush() {
        let h = accumulate(None, 0, t0(), false);
        assert_eq!((h.total, h.seen), (0, 0));
        assert_eq!(h.last_hit_at, None);
        assert_eq!(h.since, "2026-09-11T00:00:00Z");
        assert_eq!(
            h.quiet_since(),
            "2026-09-11T00:00:00Z",
            "没命中过就从起点算"
        );

        // 涨了 5 ⇒ 累计 5，记下命中时刻
        let h = accumulate(Some(h), 5, t0() + time::Duration::minutes(1), false);
        assert_eq!((h.total, h.seen), (5, 5));
        assert_eq!(h.last_hit_at.as_deref(), Some("2026-09-11T00:01:00Z"));
        assert_eq!(h.quiet_since(), "2026-09-11T00:01:00Z");

        // 重放（或开机）把 counter 清零：活计数变小 ⇒ 当成从 0 重新计，累计不许倒退、也不许翻倍
        let h = accumulate(Some(h), 2, t0() + time::Duration::minutes(2), false);
        assert_eq!((h.total, h.seen), (7, 2));
        // 一轮没有新流量：累计不动、命中时刻不动（30 天门禁靠它）
        let h = accumulate(Some(h), 2, t0() + time::Duration::minutes(3), false);
        assert_eq!((h.total, h.seen), (7, 2));
        assert_eq!(h.last_hit_at.as_deref(), Some("2026-09-11T00:02:00Z"));
    }

    /// 自己重放过那一轮要把 `seen` 归零：flush 之后的新计数在一轮内**恰好越过旧值**时
    /// 不许漏计（正好等于旧值时连 `last_hit_at` 都不动 —— 方向正是危险那一侧：
    /// 在用的兼容段被判成闲置）。
    #[test]
    fn a_round_that_replayed_the_table_restarts_the_counter_from_zero() {
        // 采到 9 之后这一轮自己重放（`nft -f` 先 flush table ⇒ counter 归零）
        let h = accumulate(None, 9, t0(), true);
        assert_eq!((h.total, h.seen), (9, 0), "重放过 ⇒ 下一轮从 0 比");
        // 下一轮又来了 9 条流：这 9 条必须计进去
        let h = accumulate(Some(h), 9, t0() + time::Duration::minutes(1), false);
        assert_eq!((h.total, h.seen), (18, 9));
        assert_eq!(
            h.last_hit_at.as_deref(),
            Some("2026-09-11T00:01:00Z"),
            "命中时刻要前移"
        );
    }

    /// 一整轮：表不在 ⇒ 重放 + 记录 + 事件；兼容段的活计数在重放**之前**被采走并累加。
    #[tokio::test]
    async fn check_once_replays_the_table_and_persists_the_compat_hits() {
        let host = Arc::new(FakeHost::new());
        host.with(|i| {
            i.which.insert("nft".into());
            // 表在位但兼容段少了一条（有人手工删过）⇒ 判不符、重放
            i.scripted.push((
                "nft list table inet bui".into(),
                crate::sys::CmdOut::success(
                    "table inet bui {\n\tchain prerouting {\n\t\tudp dport 41000-50000 counter \
                     packets 1 bytes 40 redirect to :40000\n\t\tudp dport 40001-40007 counter \
                     packets 9 bytes 360 redirect to :40000\n\t}\n}\n",
                ),
            ));
        });
        let (c, _d) = ctx(host.clone()).await;
        check_once(&c).await.unwrap();
        // **采样必须排在重放之前**（spec §2.4 的裁决）：`nft -f` 的第一句是 `flush table`，
        // counter 随之归零，采在后面就等于漏计 ⇒ 兼容段被判闲置 ⇒ 关掉它让全部没刷订阅的
        // 4.0 住宅用户当场断联。假机器的回显是无状态的（flush 表达不出来），所以这条裁决
        // 只能按**顺序**钉：这一轮 `nft list` 恰好一次，且在 `nft -f -` 之前。
        let ops = host.ops();
        let list = ops
            .iter()
            .position(|o| o == "run:nft list table inet bui")
            .unwrap_or_else(|| panic!("这一轮没列过表：{ops:?}"));
        assert_eq!(
            ops.iter()
                .filter(|o| *o == "run:nft list table inet bui")
                .count(),
            1,
            "只许列一次（列第二次就说明采样可能挪到了重放之后）：{ops:?}"
        );
        let replay = ops
            .iter()
            .position(|o| o == "run:nft -f -")
            .unwrap_or_else(|| panic!("表不符却没重放：{ops:?}"));
        assert!(list < replay, "采样必须在重放之前：{ops:?}");
        let rt = c.runtime.read().await;
        let heals: std::collections::BTreeMap<String, ChainHeal> =
            serde_json::from_value(rt.extra[NFT_HEAL_KEY].clone()).unwrap();
        let rec = &heals["replayed"];
        assert_eq!(rec.count, 1);
        assert_eq!(rec.at, "2026-09-11T00:00:00Z");
        let incs = crate::modules::sentinel::incidents::from_runtime(&rt);
        assert_eq!(incs.len(), 1);
        assert_eq!(incs[0].signature, NFT_TABLE_MISSING_SIG);
        assert!(
            incs[0].result.contains("期望 4 条 redirect，实到 2"),
            "盘上到底有几条要如实报（夹具那张表只剩 prerouting 的两条）：{}",
            incs[0].result
        );
        let hits = compat_hits(&rt).unwrap();
        assert_eq!(hits.total, 9, "重放前采到的 9 条流要进累计值");
        assert_eq!(hits.last_hit_at.as_deref(), Some("2026-09-11T00:00:00Z"));
        assert_eq!(
            hits.seen, 0,
            "这一轮自己重放过（`nft -f` 先 flush table）⇒ 下一轮的增量从 0 比，否则 \
             flush 后的新计数恰好越过旧值时会漏计"
        );
        drop(rt);

        // 下一轮：表已经对了（重放之后 counter 归零）⇒ 不再重放、不再刷事件，累计值不动
        host.with(|i| {
            i.scripted.clear();
            i.scripted.push((
                "nft list table inet bui".into(),
                crate::sys::CmdOut::success(&bui_schema::render::nft::ruleset(
                    &sample_state().node.ports,
                    true,
                )),
            ));
        });
        host.clear_ops();
        host.advance(60);
        check_once(&c).await.unwrap();
        assert!(
            !host.ops().iter().any(|o| o == "run:nft -f -"),
            "{:?}",
            host.ops()
        );
        let rt = c.runtime.read().await;
        assert_eq!(
            crate::modules::sentinel::incidents::from_runtime(&rt).len(),
            1,
            "冷却期内不重复刷事件"
        );
        let hits = compat_hits(&rt).unwrap();
        assert_eq!((hits.total, hits.seen), (9, 0), "flush 过之后从 0 重新计");
    }

    /// 没装 nft 的机器：每轮都告 Error（住宅全断），但**不许**去跑 `nft`，也不许把它
    /// 误记成「表不在」那条签名。
    #[tokio::test]
    async fn a_machine_without_nft_gets_an_error_level_incident_each_cooldown() {
        let host = Arc::new(FakeHost::new());
        let (c, _d) = ctx(host.clone()).await;
        check_once(&c).await.unwrap();
        assert!(
            !host.ops().iter().any(|o| o.starts_with("run:nft")),
            "{:?}",
            host.ops()
        );
        let rt = c.runtime.read().await;
        let incs = crate::modules::sentinel::incidents::from_runtime(&rt);
        assert_eq!(incs.len(), 1);
        assert_eq!(incs[0].signature, NFT_MISSING_SIG);
        assert_eq!(
            incs[0].level,
            crate::modules::sentinel::incidents::Level::Error
        );
        assert!(compat_hits(&rt).is_none(), "没表可采就不写这个键");
        drop(rt);
        host.advance(60);
        check_once(&c).await.unwrap();
        assert_eq!(
            crate::modules::sentinel::incidents::from_runtime(&c.runtime.read().await).len(),
            1,
            "10 分钟冷却内只有一条"
        );
    }

    /// 只数「鉴权请求 + 连不上/超时」两类标记同时命中的行。住宅上游、relay 的连接错误
    /// 与正常的鉴权日志都不能被数进来，否则哨兵天天误报。
    #[test]
    fn only_lines_about_the_auth_endpoint_failing_are_counted() {
        let log = "\
hysteria[1]: authentication error {\"error\": \"Post \\\"http://127.0.0.1:18789/auth\\\": dial tcp 127.0.0.1:18789: connect: connection refused\"}
hysteria[1]: authentication error {\"error\": \"Post \\\"http://127.0.0.1:18789/auth\\\": context deadline exceeded\"}
hysteria[1]: authentication error {\"error\": \"Post \\\"http://127.0.0.1:18789/auth\\\": dial tcp 127.0.0.1:18789: i/o timeout\"}
hysteria[1]: client connected {\"addr\": \"203.0.113.9:1\", \"id\": \"u-1\"}
hysteria[1]: outbound error {\"error\": \"dial tcp 198.51.100.7:10007: connect: connection refused\"}
";
        assert_eq!(count_auth_http_failures(log), 3);
        assert_eq!(count_auth_http_failures(""), 0);
        assert_eq!(
            count_auth_http_failures("hysteria[1]: client connected /auth ok\n"),
            0,
            "只有 /auth 没有失败标记 ⇒ 不算"
        );
    }

    /// 鉴权日志由哨兵负责（5 秒增量读，设计裁决 D9）：watchdog 不再每 60 秒自己翻 hysteria 的日志，
    /// 也不再写 `hy2_auth_http` 键——同一故障不许两处各报一次。
    #[tokio::test]
    async fn the_watchdog_leaves_the_auth_log_to_the_sentinel() {
        let host = Arc::new(FakeHost::new());
        host.with(|i| {
            for u in [
                "hysteria-server",
                "hysteria-residential",
                "xray",
                "b-ui-relay",
            ] {
                i.units_active.insert(format!("{u}.service"));
            }
            i.listening
                .insert(Proto::Udp, [10000, 40000].into_iter().collect());
            i.listening
                .insert(Proto::Tcp, [10001, 2080].into_iter().collect());
        });
        let (c, _d) = ctx(host.clone()).await;
        check_once(&c).await.unwrap();
        assert!(
            !host.ops().iter().any(|o| o.contains("journalctl")),
            "{:?}",
            host.ops()
        );
        assert!(!c.runtime.read().await.extra.contains_key("hy2_auth_http"));
    }
}
