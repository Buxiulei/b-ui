//! 守护进程的共享状态与事件总线；HTTP 实现见 api/auth.rs / health.rs / system.rs（Task 13）。
use crate::state::runtime::Runtime;
use crate::state::store::Store;
use crate::sys::Host;
use std::sync::Arc;
use time::OffsetDateTime;

/// 进程内事件：state 变更、relay 重启（P3 据此重放上游）、住宅 HY2 入站重启
/// （4.1 据此重放门位）、显式请求对账。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    StateChanged(&'static str),
    RelayRestarted,
    /// `hysteria-residential`（住宅 HY2 的 sing-box 入站）刚被重启过。
    ///
    /// 不开 `cache_file`（spec §14 裁决 1）⇒ 重启后每个 `gate-<id>` selector 回到
    /// `default = "deny"`、住宅 HY2 全员 fail-closed，由
    /// [`panel::gates::replay_after_restart`](crate::modules::panel::gates::replay_after_restart)
    /// 重放真实门位。
    Hy2ResiRestarted,
    ReconcileRequested {
        force: bool,
    },
}

#[derive(Clone)]
pub struct EventBus(tokio::sync::broadcast::Sender<Event>);

impl EventBus {
    pub fn new() -> Self {
        Self(tokio::sync::broadcast::channel(64).0)
    }

    /// 没有订阅者时静默丢弃（装机阶段还没起后台任务）。
    pub fn send(&self, e: Event) {
        let _ = self.0.send(e);
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Event> {
        self.0.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

/// axum handler 的共享状态（总纲 C2 的 `AppState`，字段细化）。
#[derive(Clone)]
pub struct AppState {
    pub store: Store,
    pub bus: EventBus,
    pub runtime: Runtime,
    pub host: Arc<dyn Host>,
    pub started_at: OffsetDateTime,
    pub version: &'static str,
    /// 登录限速表：**每个 router 实例一份**（随 `AppState` 克隆），理由见 `auth::LoginLimiter`。
    pub login: crate::api::auth::LoginLimiter,
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn bus_broadcasts_to_every_subscriber_and_tolerates_none() {
        let bus = EventBus::new();
        bus.send(Event::StateChanged("nobody-listening")); // 不能 panic
        let mut a = bus.subscribe();
        let mut b = bus.subscribe();
        bus.send(Event::ReconcileRequested { force: true });
        assert_eq!(
            a.recv().await.unwrap(),
            Event::ReconcileRequested { force: true }
        );
        assert_eq!(
            b.recv().await.unwrap(),
            Event::ReconcileRequested { force: true }
        );
    }
}
