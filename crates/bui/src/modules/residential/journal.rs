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

/// 解析一行；不是拒绝行（或不含状态/拒绝语义）→ `None`
pub fn parse_line(line: &str) -> Option<RejectLine> {
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
            host: host.to_string(),
            port,
            status: None,
        });
    }
    // 其余 SOCKS5 拒绝形态（REP ≠ 0 的文字化）。超时/EOF 一类不是拒绝，不学。
    let lower = reason.to_ascii_lowercase();
    let denied = ["not allowed", "refused", "rejected", "forbidden", "denied"]
        .iter()
        .any(|k| lower.contains(k));
    denied.then(|| RejectLine {
        subject,
        host: host.to_string(),
        port,
        status: None,
    })
}

/// 增量读日志：有游标用 `--after-cursor`，否则 `--since -25h`（与 R13 §6.2 同窗口）。
/// `journalctl` 不存在 → `Ok(JournalBatch::default())`（调用方据此记一条 alert）。
pub fn collect(host: &dyn Host, cursor: Option<&str>) -> anyhow::Result<JournalBatch> {
    if !host.which("journalctl") {
        return Ok(JournalBatch::default());
    }
    let mut args: Vec<&str> = vec![
        "-u",
        JOURNAL_UNIT,
        "--no-pager",
        "-o",
        "cat",
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
            continue;
        }
        if let Some(r) = parse_line(line) {
            batch.lines.push(r);
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
            parse_line(fx::POOL_URLTEST_SOCKS_CODE2),
            Some(RejectLine {
                subject: pool("resi-pool"),
                host: "www.example.com".into(),
                port: 443,
                status: None
            })
        );
        assert_eq!(
            parse_line(fx::POOL_SELECTOR_SOCKS_CODE2),
            Some(RejectLine {
                subject: pool("slot-3-pool"),
                host: "www.example.com".into(),
                port: 443,
                status: None
            })
        );
        assert_eq!(
            parse_line(fx::POOL_SELECTOR_403),
            Some(RejectLine {
                subject: pool("slot-1-pool"),
                host: "www.example.com".into(),
                port: 443,
                status: Some(403)
            })
        );
        assert_eq!(
            parse_line(fx::POOL_SELECTOR_502),
            Some(RejectLine {
                subject: pool("slot-1-pool"),
                host: "www.example.com".into(),
                port: 443,
                status: Some(502)
            })
        );
        // 拨号失败的行带着上游自己的地址：解析层原样带出来，归因（路径 A）归调用方
        assert_eq!(
            parse_line(fx::POOL_SELECTOR_DIAL_REFUSED).map(|l| l.subject),
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
            parse_line(fx::MEMBER_SOCKS_CODE2),
            Some(RejectLine {
                subject: member("resi-1"),
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
            assert_eq!(parse_line(l), None, "{l}");
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
        assert_eq!(
            parse_line(fx::POOL_SELECTOR_DIAL_NO_ROUTE).map(|l| l.subject),
            None
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
            parse_line(HTTP_403),
            Some(RejectLine {
                subject: member("resi-1"),
                host: "gateway.icloud.com".into(),
                port: 443,
                status: Some(403)
            })
        );
        assert_eq!(
            parse_line(HTTP_403_PORT),
            Some(RejectLine {
                subject: member("resi-1"),
                host: "198.51.100.9".into(),
                port: 5228,
                status: Some(403)
            })
        );
        assert_eq!(
            parse_line(SOCKS_DENY),
            Some(RejectLine {
                subject: member("resi-2"),
                host: "x.com".into(),
                port: 443,
                status: None
            })
        );
        // REP=2（connection not allowed by ruleset）才是策略拒绝
        assert_eq!(
            parse_line(SOCKS_CODE2),
            Some(RejectLine {
                subject: member("resi-1"),
                host: "smtp.gmail.com".into(),
                port: 465,
                status: None
            })
        );
    }

    #[test]
    fn ignores_lines_that_are_not_upstream_rejections() {
        assert_eq!(parse_line(UNRELATED), None);
        // 超时不是拒绝：算进候选会把网络抖动学成黑名单
        assert_eq!(parse_line(TIMEOUT), None);
        // SOCKS5 REP=4（主机不可达）/ 1（通用失败）是目标或网络的问题，不是策略拒绝
        assert_eq!(parse_line(SOCKS_CODE4), None);
        assert_eq!(parse_line(SOCKS_CODE1), None);
        // 2xx 不是拒绝
        assert_eq!(
            parse_line(
                "open connection to a.com:443 using outbound/http[resi-1]: unexpected status: 200 OK"
            ),
            None
        );
        // direct 出站的行与住宅无关
        assert_eq!(
            parse_line(
                "open connection to a.com:443 using outbound/direct[direct]: unexpected status: 403 Forbidden"
            ),
            None
        );
        assert_eq!(parse_line(""), None);
        // 端口不是数字
        assert_eq!(
            parse_line(
                "open connection to a.com:https using outbound/http[resi-1]: unexpected status: 403 x"
            ),
            None
        );
    }

    #[test]
    fn strip_ansi_removes_color_codes_only() {
        assert_eq!(strip_ansi("\x1b[31mERROR\x1b[0m x"), "ERROR x");
        assert_eq!(strip_ansi("plain"), "plain");
    }

    #[test]
    fn collect_uses_since_on_the_first_run_and_after_cursor_afterwards() {
        let h = FakeHost::new();
        h.with(|i| {
            i.which.insert("journalctl".into());
            i.scripted.push((
                format!("journalctl -u {JOURNAL_UNIT} --no-pager -o cat --show-cursor --since -25h"),
                CmdOut::success(&format!(
                    "{HTTP_403}\n{SOCKS_DENY}\n{UNRELATED}\n-- cursor: s=abc;i=1;b=2\n"
                )),
            ));
            i.scripted.push((
                format!(
                    "journalctl -u {JOURNAL_UNIT} --no-pager -o cat --show-cursor --after-cursor s=abc;i=1;b=2"
                ),
                CmdOut::success(&format!("{HTTP_403_PORT}\n-- cursor: s=abc;i=9;b=2\n")),
            ));
        });
        let first = collect(&h, None).unwrap();
        assert_eq!(first.lines.len(), 2, "两条拒绝行，无关行被丢掉");
        assert_eq!(first.cursor.as_deref(), Some("s=abc;i=1;b=2"));
        let second = collect(&h, first.cursor.as_deref()).unwrap();
        assert_eq!(second.lines.len(), 1);
        assert_eq!(second.lines[0].port, 5228);
        assert_eq!(second.cursor.as_deref(), Some("s=abc;i=9;b=2"));
    }

    #[test]
    fn collect_is_a_no_op_when_journalctl_is_missing() {
        let h = FakeHost::new(); // which 里没有 journalctl
        assert_eq!(collect(&h, None).unwrap(), JournalBatch::default());
        assert!(h.ops().is_empty(), "不该尝试执行不存在的命令");
    }
}
