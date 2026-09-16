//! 系统预案（spec §5.7）：hysteria 鉴权失败告警、bind 冲突 / 崩溃循环只记事件、caddy 证书告警、
//! xray gRPC 不可用时立即跑一轮用户同步。**不重启任何单元**：重启归 systemd 与看门狗
//! （设计裁决 D9 / D10 / D11）。

use super::incidents::{Level, Outcome};
use super::signature::Sig;
use crate::modules::panel::{users, Shared, XRAY_API_ADDR};
use crate::reconcile::DaemonCtx;
use crate::sys::Proto;
use bui_schema::render::hy2_singbox::HY2_RESI_CLASH_API;
use bui_schema::render::hysteria::AUTH_HTTP_PORT;
use bui_schema::slots::RELAY_SOCKS_BASE;

/// `XRAY_API_ADDR`（`127.0.0.1:10085`）的端口。常量写死在面板模块里，这里只解析、不另立一份
fn xray_api_port() -> u16 {
    XRAY_API_ADDR
        .rsplit_once(':')
        .and_then(|(_, p)| p.parse().ok())
        .unwrap_or_default()
}

/// `HY2_RESI_CLASH_API`（`127.0.0.1:9092`）的端口。同上：只解析 `bui-schema` 那一份
fn hy2_resi_clash_port() -> u16 {
    HY2_RESI_CLASH_API
        .rsplit_once(':')
        .and_then(|(_, p)| p.parse().ok())
        .unwrap_or_default()
}

async fn tcp_listening(ctx: &DaemonCtx, port: u16) -> bool {
    let host = ctx.host.clone();
    tokio::task::spawn_blocking(move || {
        host.listening_ports(Proto::Tcp)
            .unwrap_or_default()
            .contains(&port)
    })
    .await
    .unwrap_or(false)
}

/// hysteria 连不上 http 鉴权端口（60 秒内 ≥3 条）：记事件 + 告警，看一眼守护进程还在不在听。
/// **不重启 b-ui**：它由 systemd `Restart=always` 拉起，自己 restart 自己只会把正在收敛的对账拦腰砍断。
pub async fn on_hy2_auth(ctx: &DaemonCtx, unit: &str) -> Outcome {
    let tail = if tcp_listening(ctx, AUTH_HTTP_PORT).await {
        "守护进程在听该端口，多半是应答超时（看 b-ui 日志）"
    } else {
        "守护进程没在听该端口；b-ui 由 systemd 拉起，哨兵不重启它"
    };
    Outcome {
        subject: unit.to_string(),
        result: format!(
            "{unit} 连不上 http 鉴权端口 127.0.0.1:{AUTH_HTTP_PORT}，本实例正在全员拒绝登录；{tail}"
        ),
        level: Level::Error,
    }
}

/// bind 冲突 / 崩溃循环：交给看门狗（退避重启、孤儿链自愈）与 systemd，哨兵只记事件
pub fn on_kernel(unit: &str, sig: Sig) -> Outcome {
    let what = if sig == Sig::KernelBindInUse {
        "端口被占（bind: address already in use）"
    } else {
        "崩溃循环"
    };
    Outcome {
        subject: unit.to_string(),
        result: format!(
            "{unit} {what}：交给看门狗（退避重启、孤儿链自愈）与 systemd，哨兵只记事件"
        ),
        level: Level::Warn,
    }
}

/// caddy 签证书失败：只告警（caddy 自己会重试）
pub fn on_caddy_cert(domain: &str) -> Outcome {
    Outcome {
        subject: domain.to_string(),
        result: format!(
            "caddy 签 {domain} 的证书失败：caddy 会自行重试，哨兵不动作；持续失败请查 DNS 解析与 80/443 放行"
        ),
        level: Level::Error,
    }
}

/// xray gRPC 连续不可用：API 端口在听 ⇒ 立即跑一轮用户同步安全网（不等 60 秒）；不在听 ⇒ 只记事件，
/// xray 被拉起后 `users::sync_users` 的 `NRestarts` 侦测与 60 秒安全网会自己补齐。
/// 这里的文案**不许**包含 `users::USER_SYNC_FAILED_LOG`：哨兵读自己的日志，那样会自触发。
pub async fn on_xray_grpc(ctx: &DaemonCtx, panel: &Shared) -> Outcome {
    let subject = "xray".to_string();
    if !tcp_listening(ctx, xray_api_port()).await {
        return Outcome {
            subject,
            result: format!(
                "xray 的 gRPC 端口 {XRAY_API_ADDR} 没在听：等 xray 被 systemd / 看门狗拉起后由 60 秒安全网补齐，本次不重试"
            ),
            level: Level::Warn,
        };
    }
    retried(subject, users::sync_now(ctx, panel).await)
}

/// 住宅 HY2 的门位收敛失败（`Sig::Hy2ResiGateSyncFailed`）：与 [`on_xray_grpc`] 同预案 ——
/// Clash API 在听 ⇒ 立即重跑一轮收敛；不在听 ⇒ 只记事件，等 sing-box 被拉起后由重放与
/// 60 秒安全网补齐（spec §3.4 的重放是 fail-closed：门停在 `deny`，不会误放行）。
pub async fn on_gate_sync(ctx: &DaemonCtx, panel: &Shared) -> Outcome {
    let subject = "b-ui".to_string();
    if !tcp_listening(ctx, hy2_resi_clash_port()).await {
        return Outcome {
            subject,
            result: format!(
                "住宅 HY2 的 Clash API {HY2_RESI_CLASH_API} 没在听：门位停在原处（重放是 fail-closed，不会误放行），等 sing-box 被 systemd / 看门狗拉起后由 60 秒安全网补齐，本次不重试"
            ),
            level: Level::Warn,
        };
    }
    retried(subject, users::sync_now(ctx, panel).await)
}

/// 「立即重试一轮用户同步」的结论（`RetryUserSync` 两个签名共用）
fn retried(subject: String, out: users::SyncOutcome) -> Outcome {
    if out.errors.is_empty() {
        Outcome {
            subject,
            result: format!(
                "已立即重试一轮用户同步：新增 {}、移除 {}",
                out.added.len(),
                out.removed.len()
            ),
            level: Level::Info,
        }
    } else {
        Outcome {
            subject,
            result: format!(
                "立即重试仍有 {} 项失败（{}），交给 60 秒安全网",
                out.errors.len(),
                out.errors[0]
            ),
            level: Level::Warn,
        }
    }
}

/// 住宅 HY2 入站拨不通 relay 的槽入站：交给看门狗（它每 60 秒探 `b-ui-relay` 并重启），
/// 哨兵只记事件。影响面是**全部**住宅用户的出海，所以是告警级。
pub fn on_hy2_resi_relay(unit: &str) -> Outcome {
    Outcome {
        subject: unit.to_string(),
        result: format!(
            "{unit} 拨不通 relay 的槽入站（127.0.0.1:{RELAY_SOCKS_BASE} 起）：住宅用户此刻全部出不去；交给看门狗（探测 + 退避重启 b-ui-relay）与 systemd，哨兵只记事件"
        ),
        level: Level::Error,
    }
}

/// 两个**非日志**签名的告警文案：它们由门位重放（spec §3.4）与凭据池巡查（§3.1）
/// 直接记事件，`classify` 不会产出它们；这里只兜住「万一从日志那条路进来了」。
pub fn on_resi_alert(sig: Sig) -> Outcome {
    let result = if sig == Sig::Hy2ResiGateReplayFailed {
        "住宅 HY2 的门位重放没能在预算内收敛：受影响的门停在 deny（fail-closed），60 秒安全网继续收敛"
    } else {
        "住宅 HY2 的空闲凭据不足：下一次建用户会当场扩容并重写配置（一次重启）"
    };
    Outcome {
        subject: "b-ui".to_string(),
        result: result.to_string(),
        level: Level::Error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::sentinel::testkit::{panel_shared, pool_ctx};
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn hy2_auth_alert_says_whether_the_daemon_listens_and_restarts_nothing() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, host) = pool_ctx(d.path()).await;
        let o = on_hy2_auth(&ctx, "hysteria-server").await;
        assert_eq!(
            (o.subject.as_str(), o.level),
            ("hysteria-server", Level::Error)
        );
        assert!(
            o.result.contains("127.0.0.1:18789") && o.result.contains("没在听"),
            "{}",
            o.result
        );
        host.with(|i| {
            i.listening
                .insert(Proto::Tcp, [AUTH_HTTP_PORT].into_iter().collect());
        });
        assert!(on_hy2_auth(&ctx, "hysteria-server")
            .await
            .result
            .contains("应答超时"));
        assert!(
            host.ops().iter().all(|o| !o.starts_with("systemd:")),
            "守护进程由 systemd 拉起，哨兵不重启任何东西：{:?}",
            host.ops()
        );
    }

    #[test]
    fn kernel_and_certificate_signatures_only_record() {
        let o = on_kernel("xray", Sig::KernelBindInUse);
        assert_eq!(o.level, Level::Warn);
        assert!(
            o.result.contains("端口被占") && o.result.contains("交给看门狗"),
            "{}",
            o.result
        );
        assert!(on_kernel("hysteria-server", Sig::KernelCrashLoop)
            .result
            .contains("崩溃循环"));
        let c = on_caddy_cert("panel.example.com");
        assert_eq!(
            (c.subject.as_str(), c.level),
            ("panel.example.com", Level::Error)
        );
        assert!(c.result.contains("不动作"));
    }

    #[tokio::test]
    async fn xray_grpc_trouble_runs_one_user_sync_when_the_api_port_is_up() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, host) = pool_ctx(d.path()).await;
        let (shared, _xray) = panel_shared(&ctx);
        host.with(|i| {
            i.listening
                .insert(Proto::Tcp, [10085].into_iter().collect());
        });
        let o = on_xray_grpc(&ctx, &shared).await;
        assert!(
            o.result.starts_with("已立即重试一轮用户同步"),
            "{}",
            o.result
        );
        assert_eq!(o.level, Level::Info);
        assert!(
            shared.snapshot_path().exists(),
            "sync_now 真跑了一轮（快照已重写）"
        );
    }

    #[tokio::test]
    async fn xray_grpc_trouble_waits_when_the_api_port_is_down() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, _host) = pool_ctx(d.path()).await;
        let (shared, xray) = panel_shared(&ctx);
        let o = on_xray_grpc(&ctx, &shared).await;
        assert_eq!(o.level, Level::Warn);
        assert!(o.result.contains("没在听"), "{}", o.result);
        assert!(xray.calls().is_empty(), "端口都不在听，gRPC 一次都不许打");
        assert!(!shared.snapshot_path().exists());
    }

    #[test]
    fn the_xray_api_port_is_parsed_from_the_shared_constant() {
        assert_eq!(xray_api_port(), 10085);
        assert_eq!(hy2_resi_clash_port(), 9092, "住宅 HY2 的 Clash API 端口");
    }

    /// 门位收敛失败与 xray gRPC 同预案，只是探的是住宅 HY2 的 Clash API（9092）
    #[tokio::test]
    async fn a_gate_sync_failure_retries_the_sync_only_when_the_clash_api_listens() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, host) = pool_ctx(d.path()).await;
        let (shared, _xray) = panel_shared(&ctx);
        let o = on_gate_sync(&ctx, &shared).await;
        assert_eq!((o.subject.as_str(), o.level), ("b-ui", Level::Warn));
        assert!(
            o.result.contains("127.0.0.1:9092") && o.result.contains("没在听"),
            "{}",
            o.result
        );
        assert!(!shared.snapshot_path().exists(), "端口不在听就不跑同步");
        host.with(|i| {
            i.listening.insert(Proto::Tcp, [9092].into_iter().collect());
        });
        let o = on_gate_sync(&ctx, &shared).await;
        assert_eq!(o.level, Level::Info);
        assert!(
            o.result.starts_with("已立即重试一轮用户同步"),
            "{}",
            o.result
        );
        assert!(shared.snapshot_path().exists(), "真跑了一轮");
        assert!(
            host.ops().iter().all(|o| !o.starts_with("systemd:")),
            "哨兵不重启任何单元：{:?}",
            host.ops()
        );
    }

    /// 拨不通 relay 的槽入站：告警级、点名影响面、交看门狗
    #[test]
    fn the_relay_slot_alert_names_the_blast_radius_and_delegates() {
        let o = on_hy2_resi_relay("hysteria-residential");
        assert_eq!(
            (o.subject.as_str(), o.level),
            ("hysteria-residential", Level::Error)
        );
        assert!(
            o.result.contains("127.0.0.1:2080")
                && o.result.contains("住宅用户此刻全部出不去")
                && o.result.contains("看门狗"),
            "{}",
            o.result
        );
    }

    /// 两个非日志签名的文案：重放那条必须写明 fail-closed（停在 deny，不误放行）
    #[test]
    fn the_non_log_residential_signatures_have_their_own_wording() {
        let replay = on_resi_alert(Sig::Hy2ResiGateReplayFailed);
        assert_eq!(
            (replay.subject.as_str(), replay.level),
            ("b-ui", Level::Error)
        );
        assert!(
            replay.result.contains("deny") && replay.result.contains("安全网"),
            "{}",
            replay.result
        );
        let low = on_resi_alert(Sig::Hy2ResiPoolLow);
        assert!(low.result.contains("空闲凭据不足"), "{}", low.result);
        assert_ne!(replay.result, low.result);
    }
}
