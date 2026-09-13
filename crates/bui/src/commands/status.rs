//! `bui status`：经 unix socket 读 `/api/health` 并打印人读文本（`--json` 直出 JSON）。
//!
//! 守护进程没跑时（spec §2.4 的白名单里 `status` 是放行项）退化为本地拼一个
//! [`HealthResponse`]：单元状态现场问 systemd，对账 / 漂移 / watchdog / 可升级版本读
//! `runtime.json`，节点名读 `state.json`。

use crate::api::health::{HealthResponse, ServiceStatus};
use crate::state::runtime::Runtime;
use crate::state::store::Store;
use crate::sys::Host;
use anyhow::Result;
use bui_schema::model::Hy2Auth;
use bui_schema::paths::Paths;
use std::path::PathBuf;
use std::sync::Arc;

/// 把 `/api/health` 渲染成人读文本。
///
/// `hy2_auth` 单独传：`/api/health` 里没有这一位（P2 的 `HealthResponse` 是回归锁死的形状），
/// 而运维在排「谁都登不上」时第一件要看的就是当前走的是 http 还是 command。
pub fn format_status(h: &HealthResponse, hy2_auth: Hy2Auth) -> String {
    let mut out = vec![format!(
        "b-ui v{} @ {}     状态: {}     运行: {}",
        h.version,
        h.node,
        h.status,
        crate::util::human_duration(h.uptime_secs)
    )];
    // 单元名按最长的对齐（按字符数），第一行带「单元」标签，其余行缩进对齐
    let width = h
        .services
        .iter()
        .map(|s| s.unit.chars().count())
        .max()
        .unwrap_or(0);
    for (n, s) in h.services.iter().enumerate() {
        let label = if n == 0 {
            "单元        "
        } else {
            "            "
        };
        out.push(format!(
            "{label}{:<width$} {}({}, 重启 {} 次)",
            s.unit,
            if s.active { "running" } else { "stopped" },
            if s.enabled { "enabled" } else { "disabled" },
            s.n_restarts,
            width = width
        ));
    }
    out.push(format!(
        "鉴权模式    Hysteria2 {}",
        match hy2_auth {
            Hy2Auth::Http => "auth.type=http（守护进程进程内应答）",
            Hy2Auth::Command => "auth.type=command（钩子 bin/bui-auth-hook，退路）",
        }
    ));
    if let Some(r) = &h.reconcile {
        out.push(format!(
            "上次对账    {}  变更 {} 项{}",
            r.at,
            r.changed.len(),
            if r.dry_run { "（dry-run）" } else { "" }
        ));
        for e in r.errors.iter().chain(r.verify_failures.iter()) {
            out.push(format!("错误        {e}"));
        }
        for n in &r.notes {
            out.push(format!("提示        {n}"));
        }
    }
    if h.drift.is_empty() {
        out.push("漂移        无漂移".to_string());
    } else {
        for d in &h.drift {
            out.push(format!("漂移        {} {} —— {}", d.kind, d.path, d.detail));
        }
    }
    if let Some(v) = &h.upgrade_available {
        // 版本号与正在跑的一样 ⇒ rc 通道的同版本重建（rc1 / rc2 / 正式版共用一个版本号），
        // 报「已是最新」会让人白等（真机 bwg-rick 就这么卡住过）
        if *v == h.version {
            out.push("新构建      有同版本的新构建（rc 通道），可执行 bui upgrade".to_string());
        } else {
            out.push(format!("新版本      {v} 可用（运行 b-ui upgrade）"));
        }
    }
    out.join("\n")
}

/// `bui status` 末尾显示几条事件（spec §5.7；全量看 `bui incidents`）
pub const STATUS_INCIDENTS: usize = 5;

/// `bui status` 的「最近事件」段；`--json` 不带它（`/api/health` 的形状是 P2 锁死的回归面）
pub fn format_recent_incidents(v: &[crate::modules::sentinel::incidents::Incident]) -> String {
    if v.is_empty() {
        return "最近事件    无（`bui incidents` 查看全量）".into();
    }
    let mut out = vec!["最近事件    （`bui incidents` 查看全量）".to_string()];
    out.extend(v.iter().map(|i| {
        format!(
            "            {}",
            crate::modules::sentinel::incidents::format_line(i)
        )
    }));
    out.join("\n")
}

pub async fn run(json: bool, paths: Paths, host: Arc<dyn Host>) -> Result<()> {
    run_with(json, paths, host, PathBuf::from(crate::paths::SOCKET_PATH)).await
}

/// socket 路径由调用方传入（与 install / reconcile 同一口径：单元测试不碰真实 socket）。
pub async fn run_with(
    json: bool,
    paths: Paths,
    host: Arc<dyn Host>,
    socket: PathBuf,
) -> Result<()> {
    let health = match fetch_over_socket(&socket).await {
        Some(h) => h,
        None => {
            eprintln!("守护进程未运行，以下为本地探测结果");
            local_health(&paths, host).await?
        }
    };
    // 鉴权模式读期望态：守护进程在不在跑都读得到，`--json` 那一支不动
    // （`HealthResponse` 的形状是 P2 锁死的回归面）。
    let hy2_auth = match Store::open(crate::paths::state_file(&paths)).await {
        Ok(store) => store.read().await.system.hy2_auth,
        Err(_) => Hy2Auth::default(),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&health)?);
    } else {
        println!("{}", format_status(&health, hy2_auth));
        let (recent, _) =
            crate::modules::sentinel::incidents::load_recent(&socket, &paths, STATUS_INCIDENTS)
                .await;
        println!("{}", format_recent_incidents(&recent));
    }
    Ok(())
}

/// socket 可用且 `/api/health` 解析成功才返回 `Some`，否则退化到本地探测。
async fn fetch_over_socket(socket: &std::path::Path) -> Option<HealthResponse> {
    let client = crate::ipc::Client::new(socket);
    if !client.available().await {
        return None;
    }
    let (status, body) = client.request("GET", "/api/health", None).await.ok()?;
    if status != 200 {
        return None;
    }
    serde_json::from_value(body).ok()
}

/// 守护进程没跑时的本地版本：单元状态现场问 systemd，其余读 `runtime.json` / `state.json`。
async fn local_health(paths: &Paths, host: Arc<dyn Host>) -> Result<HealthResponse> {
    // 读不到期望态就只报那六个固定单元（槽 1.. 的住宅实例无从得知）
    let (node, units) = match Store::open(crate::paths::state_file(paths)).await {
        Ok(store) => {
            let s = store.read().await;
            (s.node.name.clone(), crate::reconcile::managed_units(&s))
        }
        Err(_) => (
            host.hostname().unwrap_or_default(),
            crate::reconcile::MANAGED_UNITS
                .iter()
                .map(|u| u.to_string())
                .collect(),
        ),
    };
    let rt = Runtime::load(crate::paths::runtime_file(paths))
        .read()
        .await;
    let h = host.clone();
    let services = tokio::task::spawn_blocking(move || {
        units
            .iter()
            .map(|u| ServiceStatus {
                unit: u.to_string(),
                active: h.unit_is_active(u).unwrap_or(false),
                enabled: h.unit_is_enabled(u).unwrap_or(false),
                n_restarts: h
                    .unit_property(u, "NRestarts")
                    .ok()
                    .flatten()
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(0),
            })
            .collect::<Vec<_>>()
    })
    .await?;
    // 守护进程没跑 ⇒ 至少 `b-ui` 不 active，状态必然 degraded；判据与 `/api/health` 同一套
    let degraded = services.iter().any(|s| !s.active)
        || !rt.drift.is_empty()
        || rt
            .last_reconcile
            .as_ref()
            .is_some_and(|r| !r.errors.is_empty() || !r.verify_failures.is_empty());
    // 守护进程没跑，「运行时长」无从得知，按 0 报
    let uptime_secs = rt
        .started_at
        .as_deref()
        .and_then(crate::util::parse_rfc3339)
        .map(|t| (host.now() - t).whole_seconds().max(0) as u64)
        .unwrap_or(0);
    Ok(HealthResponse {
        status: if degraded {
            "degraded".into()
        } else {
            "ok".into()
        },
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_secs,
        node,
        services,
        reconcile: rt.last_reconcile.clone(),
        drift: rt.drift.clone(),
        watchdog: rt.watchdog.clone(),
        upgrade_available: rt.upgrade_available.clone(),
        residential: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::health::{HealthResponse, ServiceStatus};
    use crate::state::runtime::{DriftItem, ReconcileReport};

    fn sample() -> HealthResponse {
        HealthResponse {
            status: "degraded".into(),
            version: "4.0.0".into(),
            uptime_secs: 3725,
            node: "node-a".into(),
            services: vec![
                ServiceStatus {
                    unit: "hysteria-server".into(),
                    active: true,
                    enabled: true,
                    n_restarts: 2,
                },
                ServiceStatus {
                    unit: "xray".into(),
                    active: false,
                    enabled: true,
                    n_restarts: 7,
                },
            ],
            reconcile: Some(ReconcileReport {
                at: "2026-09-11T00:00:00Z".into(),
                changed: vec!["/opt/b-ui/config.yaml".into()],
                errors: vec!["xray 重启失败".into()],
                ..Default::default()
            }),
            drift: vec![DriftItem {
                kind: "cron".into(),
                path: "crontab".into(),
                detail: "0 */6 * * * /opt/b-ui/update.sh".into(),
            }],
            watchdog: Default::default(),
            upgrade_available: Some("4.0.1".into()),
            residential: None,
        }
    }

    #[test]
    fn status_text_shows_units_uptime_errors_and_drift() {
        let t = format_status(&sample(), Hy2Auth::Http);
        assert!(t.contains("node-a"));
        assert!(t.contains("4.0.0"));
        assert!(t.contains("1h 2m"), "uptime 要人读得懂：{t}");
        assert!(t.contains("hysteria-server") && t.contains("running"));
        assert!(t.contains("xray") && t.contains("stopped"));
        assert!(t.contains("xray 重启失败"));
        assert!(t.contains("漂移") && t.contains("crontab"));
        assert!(
            t.contains("4.0.1"),
            "有新版本要提示（spec §7 的每日自检结果）：{t}"
        );
    }

    /// rc 通道（2026-09-12 bwg-rick）：rc1 / rc2 / 正式版的版本号都是同一个，所以每日自检报上来
    /// 的「可升级版本」== 正在跑的版本时，说的是**同版本的新构建**，不是新版本号。
    #[test]
    fn a_same_version_rebuild_is_reported_as_a_new_build() {
        let mut h = sample();
        h.upgrade_available = Some(h.version.clone());
        let t = format_status(&h, Hy2Auth::Http);
        assert!(t.contains("同版本的新构建"), "{t}");
        assert!(t.contains("bui upgrade"), "要说清下一步怎么做：{t}");
        assert!(
            !t.contains("4.0.0 可用"),
            "别报成「新版本 4.0.0 可用」：{t}"
        );
    }

    /// 排「谁都登不上」时第一眼要看的就是这一行（2026-09-13 裁决：默认 http，command 是退路）。
    #[test]
    fn status_text_names_the_current_hysteria_auth_mode() {
        let t = format_status(&sample(), Hy2Auth::Http);
        assert!(
            t.contains("鉴权模式") && t.contains("auth.type=http"),
            "{t}"
        );
        assert!(!t.contains("auth.type=command"), "{t}");
        let t = format_status(&sample(), Hy2Auth::Command);
        assert!(t.contains("auth.type=command") && t.contains("退路"), "{t}");
        assert!(!t.contains("auth.type=http"), "{t}");
    }

    #[test]
    fn status_text_is_clean_when_everything_is_ok() {
        let mut h = sample();
        h.status = "ok".into();
        h.services.iter_mut().for_each(|s| s.active = true);
        h.reconcile = None;
        h.drift.clear();
        h.upgrade_available = None;
        let t = format_status(&h, Hy2Auth::Http);
        assert!(t.contains("无漂移"));
        assert!(!t.contains("重启失败"));
        assert!(!t.contains("4.0.1"));
    }

    #[test]
    fn status_ends_with_the_latest_incidents() {
        use crate::modules::sentinel::incidents::{Incident, Level};
        assert_eq!(
            format_recent_incidents(&[]),
            "最近事件    无（`bui incidents` 查看全量）"
        );
        let i = Incident {
            at: "2026-09-11T00:00:03Z".into(),
            unit: "b-ui-relay".into(),
            signature: "relay_upstream_error".into(),
            subject: "isp2.example.net:10007".into(),
            action: "probe_and_borrow".into(),
            result: "IP 198.51.100.8 不可达，槽 1 已临时切到 198.51.100.7".into(),
            level: Level::Error,
            sample: None,
        };
        let t = format_recent_incidents(&[i.clone(), i]);
        assert!(
            t.starts_with("最近事件    （`bui incidents` 查看全量）"),
            "{t}"
        );
        assert_eq!(t.lines().count(), 3);
        assert!(t.contains("槽 1 已临时切到 198.51.100.7"));
        assert_eq!(STATUS_INCIDENTS, 5, "spec §5.7：status 只显示最近 5 条");
    }
}
