//! 日志哨兵的签名表（spec §5.7）：一行日志 → 哪一类故障、落在哪个对象上、触发门槛多少。
//! **纯函数**，不碰机器；fixtures 用真机原文。
//!
//! 边界（设计裁决 D4）：relay 里「上游拒绝了**这个目标**」（`unexpected status: 4xx/5xx`、SOCKS5
//! 的 REP 拒绝）归黑名单（spec §5.4，`residential::journal::parse_line`），哨兵不碰；哨兵只认
//! 「**上游本身**不能用了」（连不上 / 超时 / 凭据失效）与「上游对 Google 搜索整域拒绝」。

use crate::modules::residential::MEMBER_PREFIX;

/// 签名。`id()` 是事件、面板与演练脚本共用的稳定字符串。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Sig {
    /// relay → 某上游：connection refused / i/o timeout / deadline exceeded / 407 / SOCKS5 认证被拒
    RelayUpstreamError,
    /// relay → 某上游：`unexpected status: 403 … serp …`，或对 Google 搜索域名的 403
    RelayGoogleBlocked,
    /// hysteria 连不上 http 鉴权端口（spec §3.2）
    Hy2AuthHttpFailed,
    /// hysteria / xray：`bind: address already in use`
    KernelBindInUse,
    /// systemd：`Start request repeated too quickly` / `restart counter is at N`（N ≥ [`CRASH_LOOP_RESTARTS`]）
    KernelCrashLoop,
    /// 守护进程自己的日志：用户同步的 xray gRPC 调用 Unavailable（设计裁决 D10）
    XrayGrpcUnavailable,
    /// caddy：签证书失败
    CaddyCertFailed,
}

/// 签名对应的预案（冷却按「动作 + 对象」计，设计裁决 D12）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    ProbeAndBorrow,
    VerifyGoogleAndBorrow,
    Alert,
    DelegateWatchdog,
    RetryUserSync,
}

/// 触发门槛：`window_secs` 秒内同签名同对象累计 `threshold` 条。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rule {
    pub threshold: usize,
    pub window_secs: i64,
}

impl Sig {
    pub fn id(self) -> &'static str {
        match self {
            Sig::RelayUpstreamError => "relay_upstream_error",
            Sig::RelayGoogleBlocked => "relay_google_blocked",
            Sig::Hy2AuthHttpFailed => "hy2_auth_http_failed",
            Sig::KernelBindInUse => "kernel_bind_in_use",
            Sig::KernelCrashLoop => "kernel_crash_loop",
            Sig::XrayGrpcUnavailable => "xray_grpc_unavailable",
            Sig::CaddyCertFailed => "caddy_cert_failed",
        }
    }

    pub fn action(self) -> Action {
        match self {
            Sig::RelayUpstreamError => Action::ProbeAndBorrow,
            Sig::RelayGoogleBlocked => Action::VerifyGoogleAndBorrow,
            Sig::Hy2AuthHttpFailed | Sig::CaddyCertFailed => Action::Alert,
            Sig::KernelBindInUse | Sig::KernelCrashLoop => Action::DelegateWatchdog,
            Sig::XrayGrpcUnavailable => Action::RetryUserSync,
        }
    }

    pub fn rule(self) -> Rule {
        match self {
            Sig::RelayUpstreamError => Rule {
                threshold: 3,
                window_secs: 60,
            },
            Sig::Hy2AuthHttpFailed => Rule {
                threshold: crate::modules::watchdog::AUTH_HTTP_FAIL_THRESHOLD as usize,
                window_secs: 60,
            },
            // 用户同步的安全网 60 秒一轮：「连续两轮失败」落在 150 秒窗口里（设计裁决 D10）
            Sig::XrayGrpcUnavailable => Rule {
                threshold: 2,
                window_secs: 150,
            },
            // 一条就说明问题：403 serp 是明确的上游策略，bind / 崩溃循环 / 证书失败都不会「偶发」
            Sig::RelayGoogleBlocked
            | Sig::KernelBindInUse
            | Sig::KernelCrashLoop
            | Sig::CaddyCertFailed => Rule {
                threshold: 1,
                window_secs: 60,
            },
        }
    }
}

impl Action {
    pub fn id(self) -> &'static str {
        match self {
            Action::ProbeAndBorrow => "probe_and_borrow",
            Action::VerifyGoogleAndBorrow => "verify_google_and_borrow",
            Action::Alert => "alert",
            Action::DelegateWatchdog => "delegate_watchdog",
            Action::RetryUserSync => "retry_user_sync",
        }
    }
}

/// 一行日志的匹配结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub sig: Sig,
    /// 对象在日志里的名字：relay 是成员 tag（`resi-2`，**位置键**，调用方当场经
    /// `clash::id_of_tag` 换成 uuid 再计数）、caddy 是域名、xray gRPC 是 `xray`、其余是单元名
    pub subject: String,
    /// 先脱敏再截断的原文，进事件的 `sample`
    pub detail: String,
}

/// `sample` 的长度上限（按字符）
pub const DETAIL_MAX: usize = 240;
/// `restart counter is at N` 里 N 到多少算崩溃循环
pub const CRASH_LOOP_RESTARTS: u32 = 5;
pub const BIND_IN_USE: &str = "bind: address already in use";
/// 「凭据失效」（reason 小写）。`407` 只认状态行开头，免得把端口 4070 当成 407
const AUTH_MARKERS: [&str; 3] = [
    "proxy authentication",
    "incorrect user name or password",
    "username/password authentication failed",
];
/// 「到上游本身连不上 / 超时」（reason 小写）
const UNREACHABLE_MARKERS: [&str; 5] = [
    "connection refused",
    "i/o timeout",
    "deadline exceeded",
    "no route to host",
    "network is unreachable",
];

/// 一行日志（`unit` 是裸单元名，`message` 已剥色码）→ 签名。不认识的一律 `None`。
pub fn classify(unit: &str, message: &str) -> Option<Match> {
    let kernel = unit == "xray" || unit == "b-ui-relay" || unit.starts_with("hysteria-");
    if kernel && is_crash_loop(message) {
        return Some(hit(Sig::KernelCrashLoop, unit, message));
    }
    match unit {
        "b-ui-relay" => relay(message),
        "caddy" => caddy(message),
        "b-ui" => xray_grpc(message),
        _ if kernel => {
            if unit.starts_with("hysteria-")
                && crate::modules::watchdog::is_auth_http_failure(message)
            {
                return Some(hit(Sig::Hy2AuthHttpFailed, unit, message));
            }
            message
                .contains(BIND_IN_USE)
                .then(|| hit(Sig::KernelBindInUse, unit, message))
        }
        _ => None,
    }
}

fn hit(sig: Sig, subject: &str, message: &str) -> Match {
    Match {
        sig,
        subject: subject.to_string(),
        detail: detail_of(message),
    }
}

/// 进事件的原文：**先脱敏再截断**（先截断可能把 `socks5://user1:pw1@…` 截成不带 `@` 的半截，
/// 脱敏就认不出来了）。
pub fn detail_of(message: &str) -> String {
    crate::redact::line(message)
        .chars()
        .take(DETAIL_MAX)
        .collect()
}

/// systemd 的崩溃循环判据
pub fn is_crash_loop(message: &str) -> bool {
    if message.contains("Start request repeated too quickly") {
        return true;
    }
    message
        .split_once("restart counter is at ")
        .and_then(|(_, n)| n.trim().trim_end_matches('.').parse::<u32>().ok())
        .is_some_and(|n| n >= CRASH_LOOP_RESTARTS)
}

/// `open connection to <host>:<port> using outbound/<http|socks>[resi-N]: <reason>` →
/// `(tag, host, port, reason)`；不是住宅出站的行一律 `None`（与 `residential::journal::parse_line`
/// 同一形态，但那边只收「上游拒绝了目标」）。
pub fn parse_relay(message: &str) -> Option<(String, String, u16, String)> {
    let rest = message.split_once("open connection to ")?.1;
    let (target, rest) = rest.split_once(" using outbound/")?;
    let (host, port) = target.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    let (kind_tag, reason) = rest.split_once("]: ")?;
    let (kind, tag) = kind_tag.split_once('[')?;
    if !matches!(kind, "http" | "socks") || !tag.starts_with(MEMBER_PREFIX) || host.is_empty() {
        return None;
    }
    Some((tag.to_string(), host.to_string(), port, reason.to_string()))
}

fn is_google_search_host(host: &str) -> bool {
    let h = host.to_ascii_lowercase();
    h == "google.com" || h.starts_with("www.google.")
}

fn relay(message: &str) -> Option<Match> {
    let (tag, host, _port, reason) = parse_relay(message)?;
    let lower = reason.to_ascii_lowercase();
    let m = |sig: Sig| Some(hit(sig, &tag, message));
    // ① 凭据失效：整条上游都不能用（调研 §D），与连不上同一个预案
    if lower.starts_with("unexpected status: 407") || AUTH_MARKERS.iter().any(|k| lower.contains(k))
    {
        return m(Sig::RelayUpstreamError);
    }
    if let Some(s) = lower.strip_prefix("unexpected status: ") {
        // ② Google 搜索被上游整域拒绝（Bright Data 的 `403 Forbidden serp domain`）
        if s.starts_with("403") && (s.contains("serp") || is_google_search_host(&host)) {
            return m(Sig::RelayGoogleBlocked);
        }
        // 其余状态码：上游拒绝了这个目标，黑名单的地盘
        return None;
    }
    // ③ SOCKS5 的 REP 拒绝同样是目标级（凭据失效已在 ① 截走）
    if lower.starts_with("socks5:") {
        return None;
    }
    // ④ 到上游本身连不上 / 超时
    if UNREACHABLE_MARKERS.iter().any(|k| lower.contains(k)) {
        return m(Sig::RelayUpstreamError);
    }
    None
}

fn caddy(message: &str) -> Option<Match> {
    let lower = message.to_ascii_lowercase();
    let about_cert =
        lower.contains("obtaining certificate") || lower.contains("could not get certificate");
    if !about_cert || !lower.contains("\"level\":\"error\"") {
        return None;
    }
    let v: Option<serde_json::Value> = serde_json::from_str(message).ok();
    let field = |k: &str| {
        v.as_ref()
            .and_then(|v| v.get(k))
            .and_then(|x| x.as_str())
            .map(str::to_string)
    };
    let domain = field("identifier")
        .or_else(|| {
            field("error").and_then(|e| {
                e.split_once(": obtaining certificate")
                    .map(|(d, _)| d.trim().to_string())
            })
        })
        .unwrap_or_else(|| "caddy".into());
    Some(Match {
        sig: Sig::CaddyCertFailed,
        subject: domain,
        detail: detail_of(message),
    })
}

fn xray_grpc(message: &str) -> Option<Match> {
    if !message.contains(crate::modules::panel::users::USER_SYNC_FAILED_LOG) {
        return None;
    }
    // tonic 0.14：`code: 'The service is currently unavailable'`；旧格式是 `status: Unavailable`
    message
        .to_ascii_lowercase()
        .contains("unavailable")
        .then(|| hit(Sig::XrayGrpcUnavailable, "xray", message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// relay 行的真实前缀（R13 §6.2 夹具，bwg-tizi `journalctl -u b-ui-relay -o cat`，色码已剥）
    fn relay(target: &str, out: &str, reason: &str) -> String {
        format!(
            "ERROR[4006] [2302991392 6.42s] connection: open connection to {target} using \
             outbound/{out}: {reason}"
        )
    }

    fn sig_of(unit: &str, msg: &str) -> Option<(Sig, String)> {
        classify(unit, msg).map(|m| (m.sig, m.subject))
    }

    #[test]
    fn upstream_side_failures_are_relay_upstream_errors_keyed_by_member_tag() {
        for (out, reason) in [
            (
                "socks[resi-2]",
                "dial tcp 198.51.100.8:10007: connect: connection refused",
            ),
            ("socks[resi-2]", "dial tcp 198.51.100.8:10007: i/o timeout"),
            ("http[resi-3]", "context deadline exceeded"),
            (
                "http[resi-1]",
                "unexpected status: 407 Proxy Authentication Required",
            ),
            ("socks[resi-1]", "socks5: incorrect user name or password"),
            (
                "http[resi-1]",
                "dial tcp: lookup isp1.example.net: no route to host",
            ),
        ] {
            let msg = relay("www.gstatic.com:443", out, reason);
            let tag = out.split_once('[').unwrap().1.trim_end_matches(']');
            assert_eq!(
                sig_of("b-ui-relay", &msg),
                Some((Sig::RelayUpstreamError, tag.to_string())),
                "{msg}"
            );
        }
        // 端口号里含 407 的超时仍是连不上（不是凭据失效），但签名相同、对象相同
        let msg = relay(
            "a.example.com:443",
            "http[resi-2]",
            "dial tcp 198.51.100.8:4070: i/o timeout",
        );
        assert_eq!(
            sig_of("b-ui-relay", &msg),
            Some((Sig::RelayUpstreamError, "resi-2".into()))
        );
    }

    /// 「上游拒绝了**这个目标**」归黑名单（spec §5.4），哨兵不计：否则一个被拒的支付域名
    /// 就能把整条上游判死。
    #[test]
    fn target_side_rejections_belong_to_the_blacklist_not_the_sentinel() {
        for (target, out, reason) in [
            (
                "gateway.icloud.com:443",
                "http[resi-1]",
                "unexpected status: 403 Forbidden",
            ),
            (
                "198.51.100.9:5228",
                "http[resi-1]",
                "unexpected status: 403 Forbidden",
            ),
            (
                "a.example.com:443",
                "http[resi-1]",
                "unexpected status: 502 Bad Gateway",
            ),
            (
                "x.com:443",
                "socks[resi-2]",
                "socks5: connection not allowed by ruleset",
            ),
            ("x.com:443", "socks[resi-2]", "socks5: connection refused"),
            // direct 出站与住宅无关
            (
                "a.example.com:443",
                "direct[direct]",
                "dial tcp 198.51.100.9:443: i/o timeout",
            ),
        ] {
            let msg = relay(target, out, reason);
            assert_eq!(sig_of("b-ui-relay", &msg), None, "{msg}");
        }
        assert_eq!(
            sig_of(
                "b-ui-relay",
                "inbound/socks[slot-1]: inbound connection from 127.0.0.1:41234"
            ),
            None
        );
    }

    #[test]
    fn a_serp_403_or_a_403_on_google_search_is_google_blocked() {
        let serp = relay(
            "www.google.com:443",
            "http[resi-1]",
            "unexpected status: 403 Forbidden serp domain",
        );
        assert_eq!(
            sig_of("b-ui-relay", &serp),
            Some((Sig::RelayGoogleBlocked, "resi-1".into()))
        );
        let bare = relay(
            "www.google.com.hk:443",
            "socks[resi-3]",
            "unexpected status: 403 Forbidden",
        );
        assert_eq!(
            sig_of("b-ui-relay", &bare),
            Some((Sig::RelayGoogleBlocked, "resi-3".into()))
        );
        let gateway = relay(
            "www.google.com:443",
            "http[resi-1]",
            "unexpected status: 502 Bad Gateway",
        );
        assert_eq!(sig_of("b-ui-relay", &gateway), None, "5xx 不是封 Google");
    }

    #[test]
    fn hysteria_auth_endpoint_failures_are_their_own_signature() {
        let line = "hysteria[1]: authentication error {\"error\": \"Post \\\"http://127.0.0.1:18789/auth\\\": \
                    dial tcp 127.0.0.1:18789: connect: connection refused\"}";
        assert_eq!(
            sig_of("hysteria-server", line),
            Some((Sig::Hy2AuthHttpFailed, "hysteria-server".into()))
        );
        assert_eq!(
            sig_of("hysteria-residential-2", line),
            Some((Sig::Hy2AuthHttpFailed, "hysteria-residential-2".into()))
        );
        // hysteria 自己的出站错误（住宅实例连 relay）不是鉴权失败
        let outbound = "hysteria[1]: TCP error {\"error\": \"dial tcp 127.0.0.1:2081: connect: connection refused\"}";
        assert_eq!(sig_of("hysteria-residential-1", outbound), None);
        assert_eq!(sig_of("xray", line), None, "鉴权签名只认 hysteria 单元");
    }

    #[test]
    fn bind_conflicts_and_crash_loops_are_recognised_on_kernel_units_only() {
        let hy = "FATAL\tfailed to load server config\t{\"error\": \"invalid config: listen: \
                  listen udp :10000: bind: address already in use\"}";
        assert_eq!(
            sig_of("hysteria-server", hy),
            Some((Sig::KernelBindInUse, "hysteria-server".into()))
        );
        let xr = "Failed to start: main: failed to start server > app/proxyman/inbound: failed to \
                  listen TCP on 10001 > listen tcp 0.0.0.0:10001: bind: address already in use";
        assert_eq!(
            sig_of("xray", xr),
            Some((Sig::KernelBindInUse, "xray".into()))
        );
        assert_eq!(
            sig_of("xray", "xray.service: Start request repeated too quickly."),
            Some((Sig::KernelCrashLoop, "xray".into()))
        );
        assert_eq!(
            sig_of(
                "b-ui-relay",
                "b-ui-relay.service: Scheduled restart job, restart counter is at 5."
            ),
            Some((Sig::KernelCrashLoop, "b-ui-relay".into()))
        );
        assert_eq!(
            sig_of(
                "hysteria-server",
                "hysteria-server.service: Scheduled restart job, restart counter is at 2."
            ),
            None,
            "偶发一两次重启不算崩溃循环"
        );
        assert_eq!(sig_of("caddy", hy), None, "bind 签名只认内核单元");
    }

    #[test]
    fn a_failed_user_sync_with_unavailable_is_the_xray_grpc_signature() {
        use crate::modules::panel::users::USER_SYNC_FAILED_LOG;
        let hit = format!(
            "{USER_SYNC_FAILED_LOG} error=AddUser vless-direct 失败：code: 'The service is \
             currently unavailable', message: \"tcp connect error\""
        );
        assert_eq!(
            sig_of("b-ui", &hit),
            Some((Sig::XrayGrpcUnavailable, "xray".into()))
        );
        let other = format!("{USER_SYNC_FAILED_LOG} error=AddUser vless-direct 失败：fake");
        assert_eq!(
            sig_of("b-ui", &other),
            None,
            "不是 Unavailable 的同步失败不算"
        );
        assert_eq!(
            sig_of("b-ui", "Clash API unavailable, retry later"),
            None,
            "别的 b-ui 行不算"
        );
    }

    #[test]
    fn caddy_certificate_failures_name_the_domain() {
        let a = r#"{"level":"error","ts":1757548800.1,"logger":"tls.obtain","msg":"could not get certificate from issuer","identifier":"panel.example.com","issuer":"acme-v02.api.letsencrypt.org-directory","error":"HTTP 429 urn:ietf:params:acme:error:rateLimited"}"#;
        assert_eq!(
            sig_of("caddy", a),
            Some((Sig::CaddyCertFailed, "panel.example.com".into()))
        );
        let b = r#"{"level":"error","ts":1757548800.2,"logger":"tls","msg":"job failed","error":"panel.example.com: obtaining certificate: [panel.example.com] Obtain: solving challenge"}"#;
        assert_eq!(
            sig_of("caddy", b),
            Some((Sig::CaddyCertFailed, "panel.example.com".into()))
        );
        let info = r#"{"level":"info","ts":1757548800.3,"logger":"tls.obtain","msg":"obtaining certificate","identifier":"panel.example.com"}"#;
        assert_eq!(sig_of("caddy", info), None, "info 级的「开始签」不是失败");
    }

    #[test]
    fn rules_and_actions_match_the_spec() {
        assert_eq!(
            Sig::RelayUpstreamError.rule(),
            Rule {
                threshold: 3,
                window_secs: 60
            }
        );
        assert_eq!(
            Sig::Hy2AuthHttpFailed.rule(),
            Rule {
                threshold: 3,
                window_secs: 60
            }
        );
        assert_eq!(
            Sig::XrayGrpcUnavailable.rule(),
            Rule {
                threshold: 2,
                window_secs: 150
            }
        );
        for s in [
            Sig::RelayGoogleBlocked,
            Sig::KernelBindInUse,
            Sig::KernelCrashLoop,
            Sig::CaddyCertFailed,
        ] {
            assert_eq!(s.rule().threshold, 1, "{s:?}");
        }
        assert_eq!(Sig::RelayUpstreamError.action().id(), "probe_and_borrow");
        assert_eq!(
            Sig::RelayGoogleBlocked.action().id(),
            "verify_google_and_borrow"
        );
        assert_eq!(Sig::Hy2AuthHttpFailed.action().id(), "alert");
        assert_eq!(Sig::CaddyCertFailed.action().id(), "alert");
        assert_eq!(Sig::KernelBindInUse.action().id(), "delegate_watchdog");
        assert_eq!(Sig::KernelCrashLoop.action().id(), "delegate_watchdog");
        assert_eq!(Sig::XrayGrpcUnavailable.action().id(), "retry_user_sync");
        assert_eq!(
            Sig::RelayUpstreamError.id(),
            "relay_upstream_error",
            "演练脚本按这个串认事件"
        );
    }

    #[test]
    fn details_are_redacted_before_they_are_clipped() {
        let msg = format!(
            "{} socks5://user1:pw1@isp1.example.net:10007",
            relay(
                "a.example.com:443",
                "socks[resi-1]",
                "dial tcp 198.51.100.7:10007: i/o timeout"
            )
        );
        let m = classify("b-ui-relay", &msg).unwrap();
        assert!(!m.detail.contains("pw1"), "{}", m.detail);
        let long = relay(
            &format!("{}.example.com:443", "a".repeat(600)),
            "http[resi-1]",
            "context deadline exceeded",
        );
        assert!(
            classify("b-ui-relay", &long)
                .unwrap()
                .detail
                .chars()
                .count()
                <= DETAIL_MAX
        );
    }
}
