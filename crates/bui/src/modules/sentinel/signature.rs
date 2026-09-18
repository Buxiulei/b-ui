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
    /// relay → 某上游连不上 / 超时：connection refused / i/o timeout / deadline exceeded /
    /// no route to host / network is unreachable
    RelayUpstreamError,
    /// relay → 某上游凭据失效：407 / SOCKS5 认证被拒。与 [`Sig::RelayUpstreamError`] 同一个预案
    /// （共享动作冷却），只是门槛不同
    RelayUpstreamAuthFailed,
    /// relay → 某上游：`unexpected status: 403 … serp …`，或对 Google 搜索域名的 403
    RelayGoogleBlocked,
    /// hysteria 连不上 http 鉴权端口（spec §3.2）。**只认直连单元** `hysteria-server`：
    /// 4.1 起住宅那一路是 sing-box 的静态凭据池，没有 auth 段、鉴权失败也不打日志
    /// （auth 不命中走 masquerade），签名在它上面失去对象（spec §6、§8.1）
    Hy2AuthHttpFailed,
    /// 住宅 HY2 入站拨不通 relay 的槽入站（`outbound/socks[slot-<i>-out]` 连不上 /
    /// 超时）⇒ `b-ui-relay` 或它的 `slot-<i>` 入站不在（spec §8.1）
    Hy2ResiRelayUnreachable,
    /// 守护进程自己的日志：用户同步里切门（住宅 HY2 的 Clash API）失败
    Hy2ResiGateSyncFailed,
    /// **非日志签名**：sing-box 重启后的门位重放用完预算仍有差集（spec §3.4），
    /// 由重放那一方直接记事件
    Hy2ResiGateReplayFailed,
    /// **非日志签名**：空闲凭据 < 20%（spec §3.1），由池巡查直接记事件
    Hy2ResiPoolLow,
    /// hysteria / xray：`bind: address already in use`
    KernelBindInUse,
    /// systemd：`Start request repeated too quickly` / `restart counter is at N`（N ≥ [`CRASH_LOOP_RESTARTS`]）
    KernelCrashLoop,
    /// 守护进程自己的日志：用户同步的 xray gRPC 调用 Unavailable（设计裁决 D10）
    XrayGrpcUnavailable,
    /// caddy：签证书失败
    CaddyCertFailed,
}

/// 全部签名。**唯一一份名单**：`id()` 的重名不变量、最长签名窗口（`run::longest_window_secs`）
/// 都按它枚举，新增签名只改这里。
pub const ALL_SIGS: [Sig; 12] = [
    Sig::RelayUpstreamError,
    Sig::RelayUpstreamAuthFailed,
    Sig::RelayGoogleBlocked,
    Sig::Hy2AuthHttpFailed,
    Sig::KernelBindInUse,
    Sig::KernelCrashLoop,
    Sig::XrayGrpcUnavailable,
    Sig::CaddyCertFailed,
    Sig::Hy2ResiRelayUnreachable,
    Sig::Hy2ResiGateSyncFailed,
    Sig::Hy2ResiGateReplayFailed,
    Sig::Hy2ResiPoolLow,
];

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
            Sig::RelayUpstreamAuthFailed => "relay_upstream_auth_failed",
            Sig::RelayGoogleBlocked => "relay_google_blocked",
            Sig::Hy2AuthHttpFailed => "hy2_auth_http_failed",
            Sig::KernelBindInUse => "kernel_bind_in_use",
            Sig::KernelCrashLoop => "kernel_crash_loop",
            Sig::XrayGrpcUnavailable => "xray_grpc_unavailable",
            Sig::CaddyCertFailed => "caddy_cert_failed",
            Sig::Hy2ResiRelayUnreachable => "hy2_resi_relay_unreachable",
            Sig::Hy2ResiGateSyncFailed => "hy2_resi_gate_sync_failed",
            Sig::Hy2ResiGateReplayFailed => "hy2_resi_gate_replay_failed",
            Sig::Hy2ResiPoolLow => "hy2_resi_pool_low",
        }
    }

    /// 对象是住宅上游的 relay 签名：日志里是成员 tag，调用方当场换成 uuid
    pub fn on_upstream(self) -> bool {
        matches!(
            self,
            Sig::RelayUpstreamError | Sig::RelayUpstreamAuthFailed | Sig::RelayGoogleBlocked
        )
    }

    pub fn action(self) -> Action {
        match self {
            Sig::RelayUpstreamError | Sig::RelayUpstreamAuthFailed => Action::ProbeAndBorrow,
            Sig::RelayGoogleBlocked => Action::VerifyGoogleAndBorrow,
            Sig::Hy2AuthHttpFailed
            | Sig::CaddyCertFailed
            | Sig::Hy2ResiGateReplayFailed
            | Sig::Hy2ResiPoolLow => Action::Alert,
            Sig::KernelBindInUse | Sig::KernelCrashLoop | Sig::Hy2ResiRelayUnreachable => {
                Action::DelegateWatchdog
            }
            Sig::XrayGrpcUnavailable | Sig::Hy2ResiGateSyncFailed => Action::RetryUserSync,
        }
    }

    pub fn rule(self) -> Rule {
        match self {
            // 丢包时每条连接错误都要等一次 relay 拨号超时（sing-box 5 秒）才出现，第 3 条是白等的；
            // 2 条就去带外探测，误报由探测兜底（探测通过 = Info，不借用、不占动作冷却）
            Sig::RelayUpstreamError => Rule {
                threshold: 2,
                window_secs: 60,
            },
            Sig::RelayUpstreamAuthFailed => Rule {
                threshold: 3,
                window_secs: 60,
            },
            Sig::Hy2AuthHttpFailed => Rule {
                threshold: crate::modules::watchdog::AUTH_HTTP_FAIL_THRESHOLD as usize,
                window_secs: 60,
            },
            // 用户同步的安全网 60 秒一轮：「连续两轮失败」落在 150 秒窗口里（设计裁决 D10）。
            // 门位收敛挂在同一轮同步里（spec §3.3），所以门的失败用同一个门槛
            Sig::XrayGrpcUnavailable | Sig::Hy2ResiGateSyncFailed => Rule {
                threshold: 2,
                window_secs: 150,
            },
            // 一次拨号失败可能只是瞬时的（relay 正在重启）：60 秒 3 条才算 relay 或它的
            // 槽入站不在了（spec §8.1）
            Sig::Hy2ResiRelayUnreachable => Rule {
                threshold: 3,
                window_secs: 60,
            },
            // 一条就说明问题：403 serp 是明确的上游策略，bind / 崩溃循环 / 证书失败都不会
            //「偶发」；后两个是非日志签名，由重放与池巡查各自记一条
            Sig::RelayGoogleBlocked
            | Sig::KernelBindInUse
            | Sig::KernelCrashLoop
            | Sig::CaddyCertFailed
            | Sig::Hy2ResiGateReplayFailed
            | Sig::Hy2ResiPoolLow => Rule {
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
/// 「这条用户同步的失败项说的是切门」的判据：[`crate::modules::residential::clash::ClashError`]
/// 两种文案各取不带占位符的那一截（`Unreachable` / `Rejected`）
const CLASH_MARKERS: [&str; 2] = ["Clash API 不可达", "Clash API 拒绝切换到"];
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
            // 鉴权回调只有直连那一路有（`watchdog::is_auth_http_failure` 的门槛与判据仍在，
            // 只喂 `hysteria-server` 的日志）
            if unit == "hysteria-server" && crate::modules::watchdog::is_auth_http_failure(message)
            {
                return Some(hit(Sig::Hy2AuthHttpFailed, unit, message));
            }
            if unit == "hysteria-residential" {
                if let Some(m) = hy2_resi(unit, message) {
                    return Some(m);
                }
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

/// 住宅 HY2 入站（sing-box）那一路：拨不通 relay 的槽入站 ⇒ 签名；`deny` 的拒绝行 ⇒ 噪音。
///
/// **生产形态**（2026-09-18 tizi sidecar 自建 1.14.1 真机，RESULT-up.md）：每条凭据一个
/// `gate-<id>` selector 门，成员是 `deny` 与 8 个 `slot-<i>-out`。选中的成员拨号失败时，
/// refused 的 ERROR 行挂在最外层的**门 selector** 上（`... using outbound/selector[gate-<id>]:
/// dial tcp 127.0.0.1:<port>: <原因>`），成员 tag 只出现在它自己的 INFO 行里。所以判据认
/// `outbound/selector[gate-` 并靠 `dial tcp 127.0.0.1:<port>` 里的回环端口区分：端口 =
/// [`DENY_DIAL_PORT`]（1）⇒ deny 噪音；落在 `RELAY_SOCKS_BASE..+MAX_SLOTS`（2080..2088）⇒
/// 拨不通某个槽入站。
///
/// **1.14.0 无门配置的成员出站形态**（`outbound/socks[slot-<i>-out]` / `outbound/socks[deny]`）
/// 保留双认：那种配置生产不再出现，但两种形态都认。TCP 打 `open connection to <目标> using
/// outbound/<...>: <原因>`，UDP 打 `listen packet connection using  using outbound/<...>: <原因>`
/// （`route/conn.go`，`fixtures_hy2_resi` 里 TCP / UDP 各有），只认前者会漏掉 UDP 那一半。
/// tag / 端口的形状由 [`bui_schema::render::hy2_singbox`]（`slot_out_tag` / `DENY_TAG` /
/// [`DENY_DIAL_PORT`]）与 [`bui_schema::slots`]（`RELAY_SOCKS_BASE` / `MAX_SLOTS`）决定。
///
/// [`DENY_DIAL_PORT`]: bui_schema::render::hy2_singbox::DENY_DIAL_PORT
fn hy2_resi(unit: &str, message: &str) -> Option<Match> {
    use bui_schema::render::hy2_singbox::DENY_DIAL_PORT;
    use bui_schema::slots::{MAX_SLOTS, RELAY_SOCKS_BASE};

    let lower = message.to_ascii_lowercase();
    // ── 成员出站形态（1.14.0 无门配置，判据看 tag）──
    // 被封 / 到期用户持续请求时每条流都打一条 `socks[deny]` 的拒绝行（`dial tcp 127.0.0.1:1:
    // connect: connection refused`）——门在正常工作的证据，spec §8.1 要求一律忽略。
    if message.contains("outbound/socks[deny]") {
        return None;
    }
    if lower.contains("outbound/socks[slot-")
        && UNREACHABLE_MARKERS.iter().any(|k| lower.contains(k))
    {
        return Some(hit(Sig::Hy2ResiRelayUnreachable, unit, message));
    }
    // ── 生产形态（门 selector 上的拨号失败，靠回环端口区分 deny 与槽）──
    // 门 selector tag 是 `gate-<id>`，看不出选中的是 deny 还是哪个槽，只能看
    // `dial tcp 127.0.0.1:<port>` 里的端口：= DENY_DIAL_PORT ⇒ deny 噪音（None）；落在槽入站
    // 端口段 ⇒ 拨不通那个槽 ⇒ 签名。段外的端口不认（既不误报也不漏真的槽）。
    if lower.contains("using outbound/selector[gate-") {
        if let Some(port) = loopback_dial_port(&lower) {
            if port == DENY_DIAL_PORT {
                return None;
            }
            if (RELAY_SOCKS_BASE..RELAY_SOCKS_BASE + MAX_SLOTS).contains(&port) {
                return Some(hit(Sig::Hy2ResiRelayUnreachable, unit, message));
            }
        }
    }
    None
}

/// 从 `... dial tcp 127.0.0.1:<port>: <原因>` 里取回环端口。门 selector 的拨号失败只带最外层
/// selector tag，deny 与槽的区分全靠这个端口（见 [`hy2_resi`]）。
fn loopback_dial_port(lower: &str) -> Option<u16> {
    let rest = lower.split_once("dial tcp 127.0.0.1:")?.1;
    rest.split_once(':')?.0.trim().parse().ok()
}

fn is_google_search_host(host: &str) -> bool {
    let h = host.to_ascii_lowercase();
    h == "google.com" || h.starts_with("www.google.")
}

fn relay(message: &str) -> Option<Match> {
    let (tag, host, _port, reason) = parse_relay(message)?;
    let lower = reason.to_ascii_lowercase();
    let m = |sig: Sig| Some(hit(sig, &tag, message));
    // ① 凭据失效：整条上游都不能用（调研 §D），与连不上同一个预案、各自的门槛
    if lower.starts_with("unexpected status: 407") || AUTH_MARKERS.iter().any(|k| lower.contains(k))
    {
        return m(Sig::RelayUpstreamAuthFailed);
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
    // 同一轮用户同步既切 xray 的路由规则也切住宅 HY2 的门（spec §3.3），失败项的文案决定
    // 是哪一边：`ClashError` 的两种（`residential::clash::ClashError`）⇒ 门位收敛失败
    if CLASH_MARKERS.iter().any(|k| message.contains(k)) {
        return Some(hit(Sig::Hy2ResiGateSyncFailed, "b-ui", message));
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
    use crate::modules::sentinel::fixtures_hy2_resi as fx;
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
        for (out, reason, sig) in [
            (
                "socks[resi-2]",
                "dial tcp 198.51.100.8:10007: connect: connection refused",
                Sig::RelayUpstreamError,
            ),
            (
                "socks[resi-2]",
                "dial tcp 198.51.100.8:10007: i/o timeout",
                Sig::RelayUpstreamError,
            ),
            (
                "http[resi-3]",
                "context deadline exceeded",
                Sig::RelayUpstreamError,
            ),
            (
                "http[resi-1]",
                "dial tcp: lookup isp1.example.net: no route to host",
                Sig::RelayUpstreamError,
            ),
            // 凭据失效：同一个预案，但单独一个签名（门槛不同，见 `rules_and_actions_match_the_spec`）
            (
                "http[resi-1]",
                "unexpected status: 407 Proxy Authentication Required",
                Sig::RelayUpstreamAuthFailed,
            ),
            (
                "socks[resi-1]",
                "socks5: incorrect user name or password",
                Sig::RelayUpstreamAuthFailed,
            ),
        ] {
            let msg = relay("www.gstatic.com:443", out, reason);
            let tag = out.split_once('[').unwrap().1.trim_end_matches(']');
            assert_eq!(
                sig_of("b-ui-relay", &msg),
                Some((sig, tag.to_string())),
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

    /// 鉴权签名的作用域缩成**仅直连**：住宅那一路 4.1 起是 sing-box 的静态凭据池、
    /// 没有 auth 段，鉴权失败也不打任何日志（auth 不命中走 masquerade）⇒ 签名失去对象
    #[test]
    fn the_auth_endpoint_signature_is_now_direct_only() {
        let line = "hysteria[1]: authentication error {\"error\": \"Post \\\"http://127.0.0.1:18789/auth\\\": \
                    dial tcp 127.0.0.1:18789: connect: connection refused\"}";
        assert_eq!(
            sig_of("hysteria-server", line),
            Some((Sig::Hy2AuthHttpFailed, "hysteria-server".into()))
        );
        assert_eq!(
            sig_of("hysteria-residential", line),
            None,
            "住宅单元不该再命中这个签名"
        );
        assert_eq!(
            sig_of("hysteria-residential-2", line),
            None,
            "带后缀的槽位实例已退役（进了 LEGACY_UNITS）"
        );
        // hysteria 自己的出站错误（4.0 的住宅实例连 relay）不是鉴权失败
        let outbound = "hysteria[1]: TCP error {\"error\": \"dial tcp 127.0.0.1:2081: connect: connection refused\"}";
        assert_eq!(sig_of("hysteria-server", outbound), None);
        assert_eq!(sig_of("xray", line), None, "鉴权签名只认直连 hysteria 单元");
    }

    /// 内核判定（崩溃循环 / bind 冲突）因为单元名沿用而继续覆盖住宅单元
    #[test]
    fn crash_loops_and_bind_conflicts_still_cover_the_residential_unit() {
        assert_eq!(
            sig_of("hysteria-residential", fx::BIND_IN_USE),
            Some((Sig::KernelBindInUse, "hysteria-residential".into()))
        );
        for l in [fx::CRASH_LOOP_A, fx::CRASH_LOOP_B] {
            assert_eq!(
                sig_of("hysteria-residential", l),
                Some((Sig::KernelCrashLoop, "hysteria-residential".into())),
                "{l}"
            );
        }
    }

    /// 新签名：拨不通 relay 的槽入站 ⇒ 60 秒 3 条 → DelegateWatchdog；
    /// `outbound/socks[deny]` 的拒绝行是被封用户的正常噪音，**一律忽略**
    #[test]
    fn dialing_the_relay_slot_is_a_signature_but_the_deny_noise_is_not() {
        // 生产的门 selector 形态（`selector[gate-<id>]`，2026-09-18 真机）与 1.14.0 无门
        // 配置的成员出站形态（`socks[slot-<i>-out]`）都要认，TCP 与 UDP 各两条
        for l in [
            fx::RELAY_DIAL_FAIL,
            fx::RELAY_DIAL_FAIL_UDP,
            fx::RELAY_DIAL_FAIL_SOCKS_FORM,
            fx::RELAY_DIAL_FAIL_UDP_SOCKS_FORM,
        ] {
            assert_eq!(
                sig_of("hysteria-residential", l),
                Some((Sig::Hy2ResiRelayUnreachable, "hysteria-residential".into())),
                "门 selector 与成员出站、TCP 与 UDP 四种形态都要认：{l}"
            );
        }
        // deny 噪音（门 selector 拨 `127.0.0.1:1`、成员出站 `socks[deny]`、以及 INFO 前半条）
        // 一律 None：门在正常拒绝，不是故障
        for l in [
            fx::DENY_NOISE,
            fx::DENY_NOISE_SOCKS_FORM,
            fx::DENY_NOISE_CONN,
        ] {
            assert_eq!(
                sig_of("hysteria-residential", l),
                None,
                "deny 噪音不许进事件：{l}"
            );
        }
        assert_eq!(
            Sig::Hy2ResiRelayUnreachable.rule(),
            Rule {
                threshold: 3,
                window_secs: 60
            }
        );
        assert_eq!(
            Sig::Hy2ResiRelayUnreachable.action(),
            Action::DelegateWatchdog
        );
        // 直连单元不跑 sing-box，也就没有槽出站；relay 自己的行归 relay 那张表
        assert_eq!(sig_of("hysteria-server", fx::RELAY_DIAL_FAIL), None);
        assert_eq!(sig_of("b-ui-relay", fx::RELAY_DIAL_FAIL), None);
    }

    /// 判据里的两个出站 tag 就是渲染器产出的那两个：改了 `slot_out_tag` / `DENY_TAG`
    /// 而忘了改这里，本用例转红（哨兵对住宅那一路会整体失明）
    #[test]
    fn the_markers_track_the_tags_the_renderer_emits() {
        use bui_schema::render::hy2_singbox::{slot_out_tag, DENY_TAG};
        let line = |tag: &str| {
            format!(
                "+0800 2026-09-17 03:48:11 ERROR [3493080625 1ms] connection: open connection to \
                 www.example.com:443 using outbound/socks[{tag}]: dial tcp 127.0.0.1:2087: \
                 connect: connection refused"
            )
        };
        for i in 0..bui_schema::slots::MAX_SLOTS {
            assert_eq!(
                sig_of("hysteria-residential", &line(&slot_out_tag(i))),
                Some((Sig::Hy2ResiRelayUnreachable, "hysteria-residential".into())),
                "槽 {i}"
            );
        }
        assert_eq!(sig_of("hysteria-residential", &line(DENY_TAG)), None);
    }

    /// 生产的门 selector 形态：ERROR 行只带 `gate-<id>`，deny 与槽的区分全靠回环端口，
    /// 端口段就是渲染器产出的那一段。改了 [`bui_schema::slots::RELAY_SOCKS_BASE`] /
    /// `MAX_SLOTS` / [`bui_schema::render::hy2_singbox::DENY_DIAL_PORT`] 而忘了改判据，本用例
    /// 转红（哨兵对生产那一路会整体失明）
    #[test]
    fn the_selector_form_tracks_the_renderer_ports() {
        use bui_schema::render::hy2_singbox::DENY_DIAL_PORT;
        use bui_schema::slots::{MAX_SLOTS, RELAY_SOCKS_BASE};
        // gate-<id> selector 上的拨号失败（2026-09-18 真机形态）；端口在原文里
        let line = |port: u16| {
            format!(
                "+0000 2026-09-18 02:52:23 ERROR [1393264390 301ms] connection: open connection to \
                 www.example.com:443 using outbound/selector[gate-r000]: dial tcp 127.0.0.1:{port}: \
                 connect: connection refused"
            )
        };
        // 每个槽入站端口 ⇒ 拨不通那个槽
        for i in 0..MAX_SLOTS {
            assert_eq!(
                sig_of("hysteria-residential", &line(RELAY_SOCKS_BASE + i)),
                Some((Sig::Hy2ResiRelayUnreachable, "hysteria-residential".into())),
                "槽 {i}（端口 {}）",
                RELAY_SOCKS_BASE + i
            );
        }
        // deny 端口 ⇒ 门在正常拒绝的噪音
        assert_eq!(sig_of("hysteria-residential", &line(DENY_DIAL_PORT)), None);
        // 段外端口（紧邻上界）既不是 deny 也不是任何槽 ⇒ 不认，不误报
        assert_eq!(
            sig_of("hysteria-residential", &line(RELAY_SOCKS_BASE + MAX_SLOTS)),
            None,
            "端口段外不许误报"
        );
    }

    /// 门位收敛失败：b-ui 自己的日志行 + ClashError 两种文案
    #[test]
    fn a_failing_gate_sync_retries_the_user_sync() {
        for err in [
            "Clash API 不可达：connection refused",
            "Clash API 拒绝切换到 slot-1-out（HTTP 404）",
        ] {
            let line = format!(
                "{} {err}",
                crate::modules::panel::users::USER_SYNC_FAILED_LOG
            );
            assert_eq!(
                sig_of("b-ui", &line),
                Some((Sig::Hy2ResiGateSyncFailed, "b-ui".into())),
                "{err}"
            );
        }
        assert_eq!(
            Sig::Hy2ResiGateSyncFailed.rule(),
            Rule {
                threshold: 2,
                window_secs: 150
            }
        );
        assert_eq!(Sig::Hy2ResiGateSyncFailed.action(), Action::RetryUserSync);
        // 不带同步失败前缀的 Clash 文案不算（住宅体检自己也会打「Clash API 不可达」）
        assert_eq!(sig_of("b-ui", "Clash API 不可达：connection refused"), None);
    }

    /// 正常日志不许误报（启动行、成功连接三行、槽出站的正常拨号、两个管理面的监听行）
    #[test]
    fn healthy_residential_log_lines_classify_to_none() {
        for l in [
            fx::START_LINE,
            fx::CONN_FROM,
            fx::CONN_TO_USER,
            fx::CONN_TO_USER_ZH,
            fx::CONN_TO_USER_ZH_SPACE,
            fx::CONN_TO_USER_UDP,
            fx::SLOT_OUT_CONN,
            fx::CLASH_API_LISTEN,
            fx::V2RAY_API_LISTEN,
        ] {
            assert_eq!(sig_of("hysteria-residential", l), None, "误报：{l}");
        }
    }

    /// 证书热加载每次都必然打一对 `reload certificate: reload key pair: … private key does not match
    /// public key`（ERROR）+ `reloaded TLS certificate`（INFO）（§3.5 第 3 件、§12 第 9 项）。没人给它
    /// 加白 —— `classify` 是签名白名单、这两条匹配不到任何签名 ⇒ 默认忽略、判 `None`。这条守门把「默认
    /// 忽略」钉死：将来若有人加一条泛 ERROR 签名把轮换中间态当异常报出来，本用例转红。
    #[test]
    fn cert_reload_churn_is_default_ignored_not_a_signature() {
        for l in [fx::CERT_RELOAD_MISMATCH, fx::CERT_RELOADED] {
            assert_eq!(
                sig_of("hysteria-residential", l),
                None,
                "证书热加载的正常轮换行不许报成事件：{l}"
            );
        }
    }

    /// 签名表与遗留清单的不变量：四个新 id 都稳定、且没有重名
    #[test]
    fn the_new_signature_ids_are_stable_and_unique() {
        let ids: Vec<&str> = [
            Sig::Hy2ResiRelayUnreachable,
            Sig::Hy2ResiGateSyncFailed,
            Sig::Hy2ResiGateReplayFailed,
            Sig::Hy2ResiPoolLow,
        ]
        .iter()
        .map(|s| s.id())
        .collect();
        assert_eq!(
            ids,
            vec![
                "hy2_resi_relay_unreachable",
                "hy2_resi_gate_sync_failed",
                "hy2_resi_gate_replay_failed",
                "hy2_resi_pool_low"
            ]
        );
        let mut all: Vec<&str> = ALL_SIGS.iter().map(|s| s.id()).collect();
        all.sort_unstable();
        let n = all.len();
        all.dedup();
        assert_eq!(
            all.len(),
            n,
            "签名 id 不许重名（事件、面板、演练脚本都按它认领）"
        );
        for s in [
            Sig::Hy2ResiRelayUnreachable,
            Sig::Hy2ResiGateSyncFailed,
            Sig::Hy2ResiGateReplayFailed,
            Sig::Hy2ResiPoolLow,
        ] {
            assert!(ALL_SIGS.contains(&s), "{s:?} 不在 ALL_SIGS 里");
        }
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
        // 连接类 2 条就动：丢包时每条错误都要等一次 relay 拨号超时才出现，第 3 条是白等的；
        // 误报由带外探测兜底（探测通过 = Info，不借用、不占冷却）
        assert_eq!(
            Sig::RelayUpstreamError.rule(),
            Rule {
                threshold: 2,
                window_secs: 60
            }
        );
        assert_eq!(
            Sig::RelayUpstreamAuthFailed.rule(),
            Rule {
                threshold: 3,
                window_secs: 60
            },
            "凭据类门槛不变"
        );
        assert_eq!(
            Sig::RelayUpstreamAuthFailed.action(),
            Sig::RelayUpstreamError.action(),
            "同一个预案 ⇒ 共享动作冷却（冷却键按动作 + 对象）"
        );
        assert_eq!(
            Sig::RelayUpstreamAuthFailed.id(),
            "relay_upstream_auth_failed"
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
        // 拨不通 relay 的槽入站：单条可能只是一次瞬时拨号失败，60 秒 3 条才是「relay 或它的
        // 槽入站不在了」；预案与 bind / 崩溃循环同款（交看门狗）
        assert_eq!(
            Sig::Hy2ResiRelayUnreachable.rule(),
            Rule {
                threshold: 3,
                window_secs: 60
            }
        );
        // 门位收敛与 xray 用户同步同一条安全网（60 秒一轮）⇒ 窗口同为 150 秒
        assert_eq!(
            Sig::Hy2ResiGateSyncFailed.rule(),
            Sig::XrayGrpcUnavailable.rule(),
        );
        assert_eq!(
            Sig::Hy2ResiGateSyncFailed.action(),
            Sig::XrayGrpcUnavailable.action(),
            "同预案：9092 在听就立刻重跑一轮收敛"
        );
        for s in [
            Sig::RelayGoogleBlocked,
            Sig::KernelBindInUse,
            Sig::KernelCrashLoop,
            Sig::CaddyCertFailed,
            // 非日志签名：重放（spec §3.4）与池巡查（§3.1）各自一条就告警
            Sig::Hy2ResiGateReplayFailed,
            Sig::Hy2ResiPoolLow,
        ] {
            assert_eq!(s.rule().threshold, 1, "{s:?}");
        }
        assert_eq!(
            Sig::Hy2ResiRelayUnreachable.action().id(),
            "delegate_watchdog"
        );
        assert_eq!(Sig::Hy2ResiGateSyncFailed.action().id(), "retry_user_sync");
        assert_eq!(Sig::Hy2ResiGateReplayFailed.action().id(), "alert");
        assert_eq!(Sig::Hy2ResiPoolLow.action().id(), "alert");
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
