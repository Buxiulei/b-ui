//! `b-ui-relay`（sing-box）日志里「上游拒绝了这个目标」的行解析，以及按 journald
//! 游标的增量收集（契约决策 §E：用 `--after-cursor` 轮询，不用 `-f`）。

use super::{JOURNAL_UNIT, MEMBER_PREFIX};
use crate::sys::Host;

/// relay 日志里一条「上游拒绝了这个目标」的记录
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectLine {
    /// 成员 tag（`resi-1`…），来自 `outbound/http[resi-1]`
    pub tag: String,
    pub host: String,
    pub port: u16,
    /// HTTP 上游的 CONNECT 状态码；SOCKS 拒绝行没有状态码 → `None`
    pub status: Option<u16>,
}

/// 一轮增量收集的结果：本轮解析出的拒绝行 + `--show-cursor` 给出的新游标
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JournalBatch {
    pub lines: Vec<RejectLine>,
    pub cursor: Option<String>,
}

/// 去掉 ANSI 色码（sing-box 往 journald 写带色输出，R13 §6.2 踩过）
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        // CSI 序列：ESC [ 参数… 终止字母
        if chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        }
    }
    out
}

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
    // 只学住宅出站的拒绝；direct 出站与住宅无关
    if !matches!(kind, "http" | "socks") || !tag.starts_with(MEMBER_PREFIX) {
        return None;
    }
    if let Some(s) = reason.strip_prefix("unexpected status: ") {
        let code: u16 = s.split_whitespace().next()?.parse().ok()?;
        // 只有 4xx/5xx 才是拒绝（2xx 出现在这条 error 行里只可能是上游的怪行为）
        if !(400..600).contains(&code) {
            return None;
        }
        return Some(RejectLine {
            tag: tag.to_string(),
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
            tag: tag.to_string(),
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
        tag: tag.to_string(),
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
    use crate::sys::{fake::FakeHost, CmdOut};
    use pretty_assertions::assert_eq;

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
                tag: "resi-1".into(),
                host: "gateway.icloud.com".into(),
                port: 443,
                status: Some(403)
            })
        );
        assert_eq!(
            parse_line(HTTP_403_PORT),
            Some(RejectLine {
                tag: "resi-1".into(),
                host: "198.51.100.9".into(),
                port: 5228,
                status: Some(403)
            })
        );
        assert_eq!(
            parse_line(SOCKS_DENY),
            Some(RejectLine {
                tag: "resi-2".into(),
                host: "x.com".into(),
                port: 443,
                status: None
            })
        );
        // REP=2（connection not allowed by ruleset）才是策略拒绝
        assert_eq!(
            parse_line(SOCKS_CODE2),
            Some(RejectLine {
                tag: "resi-1".into(),
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
