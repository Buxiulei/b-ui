//! 哨兵各任务共用的测试支架（`#[cfg(test)]`）：一台装了三条上游、三个槽的假机器。
use crate::api::EventBus;
use crate::reconcile::DaemonCtx;
use crate::state::runtime::Runtime;
use crate::state::store::Store;
use crate::sys::fake::FakeHost;
use bui_schema::model::{ResiMode, Upstream, UpstreamKind, Verified, DEFAULT_GROUP};
use bui_schema::paths::Paths;
use std::sync::Arc;
use uuid::Uuid;

/// 上游 N：`ispN.example.net:10007`，体检学到的出口 `198.51.100.(6+N)`（N = 1..=3 ⇒ .7/.8/.9）
pub fn upstream(n: u128) -> Upstream {
    Upstream {
        id: Uuid::from_u128(n),
        name: format!("url-{n}"),
        kind: UpstreamKind::Socks5,
        host: format!("isp{n}.example.net"),
        port: 10007,
        username: "user1".into(),
        password: "pw1".into(),
        priority: 100,
        provider: None,
        region: None,
        ports_allowed: None,
        verified: Some(Verified {
            ip: format!("198.51.100.{}", 6 + n),
            asn: None,
            org: None,
            country: None,
            at: "2026-09-11T00:00:00Z".into(),
        }),
    }
}

/// 三条上游 + 三个槽（`migrate_on_start` 按池序落槽：上游 N 在槽 N-1）；state / runtime /
/// 快照都在 `dir` 里，时钟是 FakeHost 的 2026-09-11T00:00:00Z。
pub async fn pool_ctx(dir: &std::path::Path) -> (DaemonCtx, Arc<FakeHost>) {
    let mut st = crate::testutil::sample_state();
    let g = st
        .residential
        .groups
        .get_mut(DEFAULT_GROUP)
        .expect("sample_state 自带 default 组");
    g.enabled = true;
    g.mode = ResiMode::Global;
    g.upstreams = (1..=3).map(upstream).collect();
    g.selected_upstream_id = Some(Uuid::from_u128(1));
    let paths = Paths {
        base_dir: dir.to_path_buf(),
        certs_dir: dir.join("certs"),
        bin_dir: dir.join("bin"),
    };
    let store = Store::create(crate::paths::state_file(&paths), st)
        .await
        .unwrap();
    let bus = EventBus::new();
    crate::modules::residential::slots::migrate_on_start(&store, &bus)
        .await
        .unwrap();
    let host = Arc::new(FakeHost::new());
    (
        DaemonCtx {
            store,
            runtime: Runtime::load(crate::paths::runtime_file(&paths)),
            bus,
            host: host.clone(),
            paths,
        },
        host,
    )
}

/// 面板的共享句柄（gRPC / hysteria HTTP 全是内存 fake），快照写进 `ctx.paths` 的临时目录
pub fn panel_shared(
    ctx: &DaemonCtx,
) -> (
    Arc<crate::modules::panel::Shared>,
    crate::modules::panel::fakes::FakeXray,
) {
    let xray = crate::modules::panel::fakes::FakeXray::new();
    let shared = Arc::new(crate::modules::panel::Shared::new(
        Box::new(xray.clone()),
        Box::new(crate::modules::panel::fakes::FakeHy2::new()),
    ));
    shared.set_paths(&ctx.paths);
    (shared, xray)
}
