//! 日志哨兵与预案（spec §5.7，2026-09-13 主理人：「bui 需要监听后台日志，如果出现报错，要立马处理，
//! 比如某个住宅代理 ip 连不通了，就要启用预案，立马分配新的 ip」）。
//!
//! 结构：`signature`（一行日志 → 签名，纯函数）→ `engine`（去抖 / 冷却）→ 预案（`resi` 住宅、
//! `system` 系统）→ `incidents`（事件落 `runtime.extra["incidents"]`）；`run` 是主循环。
//! 边界：只做「探测 → 借用 / 重试 / 告警」，**不增删池成员、不做切回**（切回归巡检的 `drive_slots`）。

pub mod api;
pub mod engine;
#[cfg(test)]
pub mod fixtures_hy2_resi;
#[cfg(test)]
pub mod fixtures_relay;
pub mod incidents;
pub mod resi;
pub mod run;
pub mod signature;
pub mod system;
#[cfg(test)]
pub mod testkit;

/// 同签名同对象 60 秒内只触发一次（spec §5.7）
pub const DEBOUNCE_SECS: i64 = 60;
/// 同动作同对象 10 分钟内不重复执行（spec §5.7）；冷却表持久化在 `runtime.extra["sentinel"]`
pub const ACTION_COOLDOWN_SECS: i64 = 600;
/// `runtime.incidents` 的环形上限（新的在前）
pub const INCIDENTS_MAX: usize = 200;
/// 住宅 IP 被判不健康连续这么久 ⇒ 告警里建议管理员替换（设计裁决 D15）
pub const LONG_UNREACHABLE_MINS: i64 = 30;
/// 每轮增量读 journald 的间隔（设计裁决 D1）。每轮一个短命的 `journalctl --after-cursor`，
/// 不常驻进程；2 秒是「首条错误 → 事件」预算里的轮询那一项（演练 SLA 15 秒）
pub const POLL_SECS: u64 = 2;
/// 只有游标前进时，至少隔这么久才把游标落盘一次（hysteria 在 info 级每条连接都写日志）
pub const CURSOR_PERSIST_SECS: i64 = 60;
/// 带外快探第一步：到上游网关的 TCP 建连的**总**时限（解析出的各地址并发拨，设计裁决 D5）。
/// 连不上 ⇒ 直接判不可达并借用，不再跑完整探测
pub const PROBE_TCP_TIMEOUT_SECS: u64 = 3;
/// 网关连得上之后那段完整探测（经隧道 GET、407 补判的 CONNECT）的单次超时
pub const PROBE_TIMEOUT_SECS: u64 = 5;

use crate::modules::panel::Shared;
use crate::modules::residential::clash::{Clash, HttpClash};
use crate::modules::residential::proxy::{Prober, ReqwestProber};
use crate::reconcile::{Artifact, DaemonCtx, Module, RenderCtx};
use bui_schema::model::State;
use std::sync::Arc;

/// 外部通知（Telegram / Webhook）的预留口（spec §5.7：本期不做）。每条事件落盘后调一次。
pub trait Notifier: Send + Sync + 'static {
    fn notify(&self, incident: &incidents::Incident);
}

/// 本期的实现：什么都不做
pub struct NoopNotifier;

impl Notifier for NoopNotifier {
    fn notify(&self, _incident: &incidents::Incident) {}
}

/// 哨兵的外部依赖（测试注入 fake）
#[derive(Clone)]
pub struct Deps {
    /// 带外快探用（生产：`ReqwestProber::with_timeout(PROBE_TIMEOUT_SECS)`）
    pub prober: Arc<dyn Prober>,
    /// 按槽借用用（relay 的 Clash API）
    pub clash: Arc<dyn Clash>,
    /// 用户同步重试用（面板模块的共享句柄）
    pub panel: Arc<Shared>,
    pub notifier: Arc<dyn Notifier>,
}

pub struct SentinelModule {
    deps: Deps,
}

impl SentinelModule {
    /// 生产构造：`panel` 是 `PanelModule::shared()`（`serve::modules` 里传）
    pub fn new(panel: Arc<Shared>) -> Self {
        Self::with(Deps {
            prober: Arc::new(ReqwestProber::with_timeout(PROBE_TIMEOUT_SECS)),
            clash: Arc::new(HttpClash::new()),
            panel,
            notifier: Arc::new(NoopNotifier),
        })
    }

    pub fn with(deps: Deps) -> Self {
        Self { deps }
    }
}

impl Module for SentinelModule {
    fn name(&self) -> &'static str {
        "sentinel"
    }

    /// 没有期望项，只有后台任务（与 watchdog 同）
    fn render(&self, _s: &State, _ctx: &RenderCtx) -> Vec<Artifact> {
        Vec::new()
    }

    fn routes(&self) -> axum::Router<crate::api::AppState> {
        api::routes()
    }

    fn spawn(&self, ctx: DaemonCtx) -> Vec<tokio::task::JoinHandle<()>> {
        vec![tokio::spawn(run::sentinel_loop(ctx, self.deps.clone()))]
    }
}
