//! `b-ui-relay`（sing-box）日志里「上游拒绝了这个目标」的行解析，以及按 journald
//! 游标的增量收集（契约决策 §E：用 `--after-cursor` 轮询，不用 `-f`）。

use super::{JOURNAL_UNIT, MEMBER_PREFIX};
use crate::sys::Host;

/// relay 一行出站失败日志里的**出站主体**：成员出站，还是包了一层的池（group）出站。
///
/// sing-box 的 `route/conn.go` 只把「路由选中的那个出站」的 `Type()`/`Tag()` 写进
/// `open connection to … using outbound/<kind>[<tag>]`；而 4.1 的 relay 路由规则一律指向
/// group（每槽 `slot-<i>-pool`、DNS 与 global 模式 `resi-pool`），group 的 `NewConnection`
/// 又把**自己**（不是选中的成员）当 dialer 传下去 ⇒ **生产日志里成员 tag 一个字都不出现**
/// （2026-09-18 调研，两台真机 14 天窗口：2026-09-15 之后成员形状 0 条）。
///
/// 所以解析层只解到「哪个池」这一层，「池 → 哪个上游」的归因要么靠 `dial_addr`、要么要问
/// relay 的 Clash API（有 I/O），一律归调用方（[`crate::modules::sentinel::run`] 与
/// [`super::blacklist::learn_from_journal`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelaySubject {
    /// `http` / `socks` 成员出站，tag = `resi-<n>`。历史形状，当前渲染器不再产生，仍认
    Member(String),
    /// `selector` / `urltest` 池出站，tag = `resi-pool` 或 `slot-<i>-pool`
    Pool {
        pool: String,
        /// reason 里 `dial tcp <ip>:<port>: …` 的那个地址 —— 它是**上游自己**的
        /// host:port（Go `net.Dialer` 拨号失败的原文自带），调用方据此零 I/O 归因。
        /// 协议层失败（4xx / 407 / SOCKS5 REP / EOF）的 reason 不含上游地址 ⇒ `None`
        dial_addr: Option<(String, u16)>,
    },
}

/// relay 日志里一条「上游拒绝了这个目标」的记录
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectLine {
    /// 出站主体（成员 tag 或池 tag + 拨号地址），由调用方归因成上游 id
    pub subject: RelaySubject,
    /// journald 给这条行打的时刻（`__REALTIME_TIMESTAMP`）。归因的**时间戳门**
    /// （[`super::SWITCH_ATTRIB_GRACE_SECS`]）比的就是它与池切换时刻——所以本模块的
    /// [`collect`] 必须读带时间戳的输出（`-o json`），不能再用 `-o cat`
    pub ts: time::OffsetDateTime,
    pub host: String,
    pub port: u16,
    /// HTTP 上游的 CONNECT 状态码；SOCKS 拒绝行没有状态码 → `None`
    pub status: Option<u16>,
}

/// 这个 tag 是不是住宅池的 group tag：全局池 `resi-pool`，或 `slot-<i>-pool`
/// （`i < MAX_SLOTS`，与 [`super::slot_selector`] / `bui_schema::render::relay` 同规则）。
/// **收窄到已知池名**是判据的一半：否则 `selector[gate-…]`、外部站点的 group 都能冒充住宅池。
pub fn is_pool_tag(tag: &str) -> bool {
    if tag == bui_schema::render::relay::POOL {
        return true;
    }
    let Some(n) = tag
        .strip_prefix("slot-")
        .and_then(|r| r.strip_suffix("-pool"))
    else {
        return false;
    };
    // `parse::<u16>` 认 `+1`，槽序号不认：只收纯十进制
    n.bytes().all(|b| b.is_ascii_digit())
        && n.parse::<u16>()
            .is_ok_and(|i| i < bui_schema::slots::MAX_SLOTS)
}

/// reason 里 `dial tcp <ip>:<port>: …` 的地址。只有 **TCP 拨号本身**失败时才有
/// （connection refused / i/o timeout / no route to host / network is unreachable），
/// 且那时它就是上游自己的 host:port；`dial tcp: lookup <域名>: …`（解析失败）没有端口 ⇒ `None`。
pub fn dial_addr(reason: &str) -> Option<(String, u16)> {
    let rest = reason.split_once("dial tcp ")?.1;
    let addr = rest.split_once(": ")?.0;
    let (h, p) = addr.rsplit_once(':')?;
    let port: u16 = p.parse().ok()?;
    let h = h.trim_start_matches('[').trim_end_matches(']');
    (!h.is_empty()).then(|| (h.to_string(), port))
}

/// `<kind>[<tag>]` + reason → 出站主体。**唯一**一处 kind/tag 白名单：成员类 kind 的 tag 要
/// 以 [`MEMBER_PREFIX`] 打头（`resi-pool` 本身除外，它是池不是成员），group 类 kind 的 tag
/// 必须是已知池名。不在白名单里的（`direct`、`gate-<id>`、越界槽号…）一律 `None`。
pub fn subject_of(kind: &str, tag: &str, reason: &str) -> Option<RelaySubject> {
    match kind {
        "http" | "socks" if tag.starts_with(MEMBER_PREFIX) && !is_pool_tag(tag) => {
            Some(RelaySubject::Member(tag.to_string()))
        }
        "selector" | "urltest" if is_pool_tag(tag) => Some(RelaySubject::Pool {
            pool: tag.to_string(),
            dial_addr: dial_addr(reason),
        }),
        _ => None,
    }
}

/// 一轮增量收集的结果：本轮解析出的拒绝行 + `--show-cursor` 给出的新游标
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JournalBatch {
    pub lines: Vec<RejectLine>,
    pub cursor: Option<String>,
}

/// 去掉 ANSI 色码：实现挪到 `crate::util`（日志哨兵的 journald 解析也要用），这里转出来，
/// 既有调用点与测试（`strip_ansi_removes_color_codes_only`）不变。
pub use crate::util::strip_ansi;

/// 解析一行；不是拒绝行（或不含状态/拒绝语义）→ `None`。`ts` 是 journald 给这条行打的时刻
/// （[`RejectLine::ts`]，归因的时间戳门要用），解析本身一个字都不看它。
pub fn parse_line(line: &str, ts: time::OffsetDateTime) -> Option<RejectLine> {
    let line = strip_ansi(line);
    // open connection to <host>:<port> using outbound/<kind>[<tag>]: <reason>
    let rest = line.split_once("open connection to ")?.1;
    let (target, rest) = rest.split_once(" using outbound/")?;
    let (host, port) = target.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    if host.is_empty() {
        return None;
    }
    let (kind_tag, reason) = rest.split_once("]: ")?;
    let (kind, tag) = kind_tag.split_once('[')?;
    // 只学住宅出站的拒绝；direct 出站与住宅无关。成员形状与池形状都认（RESEARCH §3：
    // 生产只剩池形状），「哪个上游」由调用方归因
    let subject = subject_of(kind, tag, reason)?;
    if let Some(s) = reason.strip_prefix("unexpected status: ") {
        let code: u16 = s.split_whitespace().next()?.parse().ok()?;
        // 只有 4xx/5xx 才是拒绝（2xx 出现在这条 error 行里只可能是上游的怪行为）
        if !(400..600).contains(&code) {
            return None;
        }
        return Some(RejectLine {
            subject,
            ts,
            host: host.to_string(),
            port,
            status: Some(code),
        });
    }
    // sing-box 的 SOCKS5 客户端把 REP ≠ 0 写成 `socks5: request rejected, code=<REP>`。
    // 只有 REP=2（connection not allowed by ruleset）是上游的策略拒绝；1（通用失败）、
    // 4（主机不可达）等是目标或网络的问题，学进黑名单会把目标自己的故障记到上游头上
    if let Some(code) = reason.strip_prefix("socks5: request rejected, code=") {
        return (code.trim() == "2").then(|| RejectLine {
            subject,
            ts,
            host: host.to_string(),
            port,
            status: None,
        });
    }
    // **`dial tcp` 开头的 reason 是上游级，不是目标级**（2026-09-18 第二次裁决）：它说的是
    // 「连不上上游自己」（connection refused / i/o timeout / no route to host），不是「上游
    // 拒绝了这个域名」。下面兜底关键词表里的 `refused` 正好会命中
    // `dial tcp …: connect: connection refused`，把一次上游抖动学成一条域名黑名单规则。
    // 这类行是哨兵的地盘（`Sig::RelayUpstreamError` → 快探 + 借用），这里一概不收。
    if reason.starts_with("dial tcp") {
        return None;
    }
    // 其余 SOCKS5 拒绝形态（REP ≠ 0 的文字化）。超时/EOF 一类不是拒绝，不学。
    let lower = reason.to_ascii_lowercase();
    let denied = ["not allowed", "refused", "rejected", "forbidden", "denied"]
        .iter()
        .any(|k| lower.contains(k));
    denied.then(|| RejectLine {
        subject,
        ts,
        host: host.to_string(),
        port,
        status: None,
    })
}

/// 增量读日志：有游标用 `--after-cursor`，否则 `--since -25h`（与 R13 §6.2 同窗口）。
/// `journalctl` 不存在 → `Ok(JournalBatch::default())`（调用方据此记一条 alert）。
///
/// **输出格式是 `-o json` 而不是 `-o cat`**（2026-09-18 第二次裁决）：归因的时间戳门要每条行
/// 自己的时刻，`-o cat` 只有 `MESSAGE` 一段。解析直接复用哨兵那条路的
/// [`crate::sys::parse_journal_json`]（同一口径：按单元过滤、剥色码、字节数组形式的
/// `MESSAGE` 也认），游标仍取 `--show-cursor` 打在末尾的那一行 —— 它是**本轮读到的最后一条**
/// 的游标，与本轮解析出几条拒绝行无关，所以安静时段也照常前进。
pub fn collect(host: &dyn Host, cursor: Option<&str>) -> anyhow::Result<JournalBatch> {
    if !host.which("journalctl") {
        return Ok(JournalBatch::default());
    }
    let mut args: Vec<&str> = vec![
        "-u",
        JOURNAL_UNIT,
        "--no-pager",
        "-o",
        "json",
        "--show-cursor",
    ];
    match cursor {
        Some(c) => {
            args.push("--after-cursor");
            args.push(c);
        }
        // 首次或游标失效：与 R13 §6.2 同窗口
        None => {
            args.push("--since");
            args.push("-25h");
        }
    }
    let out = host.run("journalctl", &args)?;
    if !out.ok() {
        // 游标失效时 journalctl 会报错；调用方下一轮会用 None 重来（见 Task 9）
        anyhow::bail!("journalctl 退出码 {}", out.status);
    }
    let mut batch = JournalBatch::default();
    for line in out.stdout.lines() {
        if let Some(c) = line.trim().strip_prefix("-- cursor: ") {
            batch.cursor = Some(c.trim().to_string());
        }
    }
    let units = [JOURNAL_UNIT.to_string()];
    for r in crate::sys::parse_journal_json(&out.stdout, &units) {
        if let Some(l) = parse_line(&r.message, r.ts) {
            batch.lines.push(l);
        }
    }
    Ok(batch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::sentinel::fixtures_relay as fx;
    use crate::sys::{fake::FakeHost, CmdOut};
    use pretty_assertions::assert_eq;

    /// 夹具行的默认时刻：解析层不看它，用例只要一个稳定值好做全量相等断言
    const TS: time::OffsetDateTime = time::macros::datetime!(2026-09-18 12:00:00 UTC);

    /// 按 [`TS`] 解析一行（解析本身与时刻无关）
    fn p(line: &str) -> Option<RejectLine> {
        parse_line(line, TS)
    }

    fn member(tag: &str) -> RelaySubject {
        RelaySubject::Member(tag.into())
    }

    fn pool(tag: &str) -> RelaySubject {
        RelaySubject::Pool {
            pool: tag.into(),
            dial_addr: None,
        }
    }

    /// 生产（4.1 的 per-slot selector 拓扑）**只会**打池形状的行：路由规则指向 group，
    /// sing-box 的 ERROR 只挂 group 自己的 tag（RESEARCH §3）。认不出它 = 黑名单学不到
    /// 任何东西（两台真机 2026-09-15 起 0 条被学到）。
    #[test]
    fn learns_from_pool_shaped_lines_which_are_the_only_ones_production_emits() {
        assert_eq!(
            p(fx::POOL_URLTEST_SOCKS_CODE2),
            Some(RejectLine {
                subject: pool("resi-pool"),
                ts: TS,
                host: "www.example.com".into(),
                port: 443,
                status: None
            })
        );
        assert_eq!(
            p(fx::POOL_SELECTOR_SOCKS_CODE2),
            Some(RejectLine {
                subject: pool("slot-3-pool"),
                ts: TS,
                host: "www.example.com".into(),
                port: 443,
                status: None
            })
        );
        assert_eq!(
            p(fx::POOL_SELECTOR_403),
            Some(RejectLine {
                subject: pool("slot-1-pool"),
                ts: TS,
                host: "www.example.com".into(),
                port: 443,
                status: Some(403)
            })
        );
        assert_eq!(
            p(fx::POOL_SELECTOR_502),
            Some(RejectLine {
                subject: pool("slot-1-pool"),
                ts: TS,
                host: "www.example.com".into(),
                port: 443,
                status: Some(502)
            })
        );
        // 拨号失败的行带着上游自己的地址：`subject_of`（与哨兵的 `parse_relay` 共用）原样
        // 带出来，归因（路径 A）归调用方。黑名单侧本身**不收**这类行，见
        // [`a_failed_dial_to_the_upstream_is_never_a_blacklist_candidate`]
        assert_eq!(
            subject_of(
                "selector",
                "slot-2-pool",
                "dial tcp 203.0.113.7:10007: connect: connection refused"
            ),
            Some(RelaySubject::Pool {
                pool: "slot-2-pool".into(),
                dial_addr: Some(("203.0.113.7".into(), 10007)),
            })
        );
    }

    /// 成员形状（历史）仍双认
    #[test]
    fn member_shaped_lines_are_still_parsed() {
        assert_eq!(
            p(fx::MEMBER_SOCKS_CODE2),
            Some(RejectLine {
                subject: member("resi-1"),
                ts: TS,
                host: "www.example.com".into(),
                port: 443,
                status: None
            })
        );
    }

    /// group 类 kind 的 tag **必须**是已知池名，否则任何 group 都能冒充住宅池
    #[test]
    fn group_kinds_only_count_when_the_tag_is_a_known_pool() {
        for l in [
            fx::POOL_TAG_OUT_OF_RANGE,
            fx::GATE_SELECTOR,
            fx::DIRECT_DIAL_TIMEOUT,
        ] {
            assert_eq!(p(l), None, "{l}");
        }
        assert!(is_pool_tag("resi-pool"));
        assert!(is_pool_tag("slot-0-pool"));
        assert!(is_pool_tag("slot-7-pool"));
        assert!(!is_pool_tag("slot-8-pool"), "MAX_SLOTS = 8 ⇒ 序号上限是 7");
        assert!(!is_pool_tag("slot--1-pool"));
        assert!(!is_pool_tag("slot-+1-pool"));
        assert!(!is_pool_tag("gate-7f3a"));
        assert!(!is_pool_tag("resi-1"));
    }

    /// `dial tcp <ip>:<port>` 是路径 A（零 I/O 归因）唯一的线索；没有它就得问 Clash
    #[test]
    fn the_dial_address_is_extracted_only_when_the_reason_carries_one() {
        assert_eq!(
            dial_addr("dial tcp 203.0.113.7:10007: connect: connection refused"),
            Some(("203.0.113.7".into(), 10007))
        );
        assert_eq!(
            dial_addr("dial tcp [2001:db8::1]:10007: i/o timeout"),
            Some(("2001:db8::1".into(), 10007))
        );
        // 解析阶段就失败 ⇒ 没有 host:port
        assert_eq!(
            dial_addr("dial tcp: lookup isp.example.net: no route to host"),
            None
        );
        // 协议层失败的 reason 完全不含上游地址
        assert_eq!(dial_addr("socks5: request rejected, code=2"), None);
        assert_eq!(
            dial_addr("unexpected status: 407 Proxy Authentication Required"),
            None
        );
        // 解析不出来的 `dial tcp: lookup …` 在 subject 这一层就没有地址 ⇒ 路径 A 归不了因
        assert_eq!(
            subject_of(
                "selector",
                "slot-0-pool",
                "dial tcp: lookup isp.example.net: no route to host"
            ),
            Some(RelaySubject::Pool {
                pool: "slot-0-pool".into(),
                dial_addr: None,
            })
        );
    }

    // bwg-tizi 2026-09-11 的真实形态（R13 §1 与调研 §D）；第一行带 ANSI 色码
    const HTTP_403: &str = "\x1b[31mERROR\x1b[0m open connection to gateway.icloud.com:443 using outbound/http[resi-1]: unexpected status: 403 Forbidden";
    const HTTP_403_PORT: &str = "open connection to 198.51.100.9:5228 using outbound/http[resi-1]: unexpected status: 403 Forbidden serp domain";
    const SOCKS_DENY: &str = "open connection to x.com:443 using outbound/socks[resi-2]: socks5: connection not allowed by ruleset";
    // 2026-09-13 真机原文（sing-box 1.14 经 Decodo SOCKS5）：REP 以 `code=<REP>` 写出
    const SOCKS_CODE2: &str = "connection: open connection to smtp.gmail.com:465 using outbound/socks[resi-1]: socks5: request rejected, code=2";
    const SOCKS_CODE4: &str = "connection: open connection to gateway.push.apple.com:5223 using outbound/socks[resi-1]: socks5: request rejected, code=4";
    const SOCKS_CODE1: &str = "connection: open connection to a.example.com:443 using outbound/socks[resi-1]: socks5: request rejected, code=1";
    const UNRELATED: &str = "inbound/socks[socks-in]: inbound connection from 127.0.0.1:41234";
    const TIMEOUT: &str =
        "open connection to a.example.com:443 using outbound/http[resi-1]: context deadline exceeded";

    #[test]
    fn parses_the_three_real_rejection_shapes() {
        assert_eq!(
            p(HTTP_403),
            Some(RejectLine {
                subject: member("resi-1"),
                ts: TS,
                host: "gateway.icloud.com".into(),
                port: 443,
                status: Some(403)
            })
        );
        assert_eq!(
            p(HTTP_403_PORT),
            Some(RejectLine {
                subject: member("resi-1"),
                ts: TS,
                host: "198.51.100.9".into(),
                port: 5228,
                status: Some(403)
            })
        );
        assert_eq!(
            p(SOCKS_DENY),
            Some(RejectLine {
                subject: member("resi-2"),
                ts: TS,
                host: "x.com".into(),
                port: 443,
                status: None
            })
        );
        // REP=2（connection not allowed by ruleset）才是策略拒绝
        assert_eq!(
            p(SOCKS_CODE2),
            Some(RejectLine {
                subject: member("resi-1"),
                ts: TS,
                host: "smtp.gmail.com".into(),
                port: 465,
                status: None
            })
        );
    }

    #[test]
    fn ignores_lines_that_are_not_upstream_rejections() {
        assert_eq!(p(UNRELATED), None);
        // 超时不是拒绝：算进候选会把网络抖动学成黑名单
        assert_eq!(p(TIMEOUT), None);
        // 上游凭据被拒（rick 308 条）是「整条上游不能用」，哨兵的地盘；不合 HTTP 的响应头
        //（rick 15 条 / tizi 94 条）语义不明。两者都不是「拒绝了这个目标」⇒ 都不学
        assert_eq!(p(fx::MEMBER_SOCKS_AUTH), None);
        assert_eq!(p(fx::MEMBER_MALFORMED_MIME), None);
        // SOCKS5 REP=4（主机不可达）/ 1（通用失败）是目标或网络的问题，不是策略拒绝
        assert_eq!(p(SOCKS_CODE4), None);
        assert_eq!(p(SOCKS_CODE1), None);
        // 2xx 不是拒绝
        assert_eq!(
            p("open connection to a.com:443 using outbound/http[resi-1]: unexpected status: 200 OK"),
            None
        );
        // direct 出站的行与住宅无关
        assert_eq!(
            p("open connection to a.com:443 using outbound/direct[direct]: unexpected status: 403 Forbidden"),
            None
        );
        assert_eq!(p(""), None);
        // 端口不是数字
        assert_eq!(
            p("open connection to a.com:https using outbound/http[resi-1]: unexpected status: 403 x"),
            None
        );
        // `unexpected EOF`（rick 17217 条）与 `context canceled`（tizi 7 条）语义含糊 /
        // 是客户端自己断的，都不是「上游拒绝了这个目标」⇒ 不进候选（哨兵那边同样不归类）
        assert_eq!(p(fx::POOL_SELECTOR_UNEXPECTED_EOF), None);
        assert_eq!(p(fx::MEMBER_UNEXPECTED_EOF), None);
        assert_eq!(p(fx::POOL_SELECTOR_CONTEXT_CANCELED), None);
    }

    /// **`dial tcp` 开头的 reason 是上游级，不是目标级**（2026-09-18 第二次裁决）：
    /// 「连不上上游自己」与「上游拒绝了这个域名」是两回事，前者是哨兵的地盘
    /// （`RelayUpstreamError` → 快探 + 借用），学进黑名单等于把一次上游抖动写成
    /// 「这条上游代理不了这个域名」。兜底拒绝关键词表里的 `refused` 正好会命中
    /// `dial tcp …: connect: connection refused`，所以要把这个前缀整个排掉
    #[test]
    fn a_failed_dial_to_the_upstream_is_never_a_blacklist_candidate() {
        for l in [
            fx::POOL_SELECTOR_DIAL_REFUSED,
            fx::MEMBER_DIAL_REFUSED,
            fx::POOL_URLTEST_DIAL_TIMEOUT,
            fx::POOL_SELECTOR_DIAL_NO_ROUTE,
        ] {
            assert_eq!(p(l), None, "{l}");
        }
        // 目标级的拒绝仍然照学：排掉的只是 `dial tcp` 这个前缀，不是 `refused` 这个词
        assert!(p(fx::POOL_SELECTOR_403).is_some());
        assert!(p(SOCKS_DENY).is_some());
    }

    #[test]
    fn strip_ansi_removes_color_codes_only() {
        assert_eq!(strip_ansi("\x1b[31mERROR\x1b[0m x"), "ERROR x");
        assert_eq!(strip_ansi("plain"), "plain");
    }

    /// `journalctl -o json` 的一条记录（只放本模块要的四个字段，与
    /// [`crate::sys::parse_journal_json`] 的判据同源）。`secs` 是相对 [`TS`] 的偏移
    fn json_rec(cursor: &str, secs: i64, message: &str) -> String {
        let us = (TS + time::Duration::seconds(secs)).unix_timestamp() * 1_000_000;
        serde_json::json!({
            "__CURSOR": cursor,
            "__REALTIME_TIMESTAMP": us.to_string(),
            "_SYSTEMD_UNIT": format!("{JOURNAL_UNIT}.service"),
            "MESSAGE": message,
        })
        .to_string()
    }

    /// 每条拒绝行都带上 journald 的时刻（归因的时间戳门要用），游标照旧从
    /// `--show-cursor` 的末行取
    #[test]
    fn collect_uses_since_on_the_first_run_and_after_cursor_afterwards() {
        let h = FakeHost::new();
        h.with(|i| {
            i.which.insert("journalctl".into());
            i.scripted.push((
                format!(
                    "journalctl -u {JOURNAL_UNIT} --no-pager -o json --show-cursor --since -25h"
                ),
                CmdOut::success(&format!(
                    "{}\n{}\n{}\n-- cursor: s=abc;i=1;b=2\n",
                    json_rec("c1", 0, HTTP_403),
                    json_rec("c2", 7, SOCKS_DENY),
                    json_rec("c3", 9, UNRELATED)
                )),
            ));
            i.scripted.push((
                format!(
                    "journalctl -u {JOURNAL_UNIT} --no-pager -o json --show-cursor --after-cursor s=abc;i=1;b=2"
                ),
                CmdOut::success(&format!(
                    "{}\n-- cursor: s=abc;i=9;b=2\n",
                    json_rec("c4", 30, HTTP_403_PORT)
                )),
            ));
        });
        let first = collect(&h, None).unwrap();
        assert_eq!(first.lines.len(), 2, "两条拒绝行，无关行被丢掉");
        assert_eq!(first.lines[0].ts, TS, "时刻来自 `__REALTIME_TIMESTAMP`");
        assert_eq!(first.lines[1].ts, TS + time::Duration::seconds(7));
        assert_eq!(first.cursor.as_deref(), Some("s=abc;i=1;b=2"));
        let second = collect(&h, first.cursor.as_deref()).unwrap();
        assert_eq!(second.lines.len(), 1);
        assert_eq!(second.lines[0].port, 5228);
        assert_eq!(second.lines[0].ts, TS + time::Duration::seconds(30));
        assert_eq!(second.cursor.as_deref(), Some("s=abc;i=9;b=2"));
    }

    #[test]
    fn collect_is_a_no_op_when_journalctl_is_missing() {
        let h = FakeHost::new(); // which 里没有 journalctl
        assert_eq!(collect(&h, None).unwrap(), JournalBatch::default());
        assert!(h.ops().is_empty(), "不该尝试执行不存在的命令");
    }
}
