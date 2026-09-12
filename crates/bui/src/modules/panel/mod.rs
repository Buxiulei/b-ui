//! P2 面板与用户域：管理员 API、订阅与节点端点、嵌入前端、`/packages`、
//! 采样与限额、`auth-snapshot.json`、Xray gRPC、`bui auth-hook`。
//!
//! 对 P1 只暴露一个 [`PanelModule`]（`reconcile::Module` 的实现）。
//!
//! ⚠️ 本子树整体带一条 `allow(dead_code)`：Task 1 只交付「契约面」——`Shared` 的各把锁、
//! [`Applied`] / [`SampleCache`] 的字段、`XRAY_INBOUND_TAGS` 等常量、`testsupport` 的几个
//! 挂载 helper，调用方都在 Task 2…Task 13。P1 的 crate 级 allow 只作用于 `not(test)`
//! 构建（见 `main.rs` 文首），而本子树的绝大多数消费方是单元测试，所以那条盖不住这里。
//! **Task 13 收口时整条删掉**：届时每一项都该有真实调用方，删不掉就说明有真正的死代码。
#![allow(dead_code)]

pub mod api_admin;
pub mod api_me;
pub mod api_public;
pub mod assets;
pub mod auth_hook;
pub mod hy2;
pub mod packages;
pub mod snapshot;
pub mod traffic;
pub mod users;
pub mod xray;

#[cfg(test)]
pub mod fakes;
#[cfg(test)]
pub mod testsupport;

use crate::api::AppState;
use crate::reconcile::{Artifact, DaemonCtx, Module, RenderCtx};
use bui_schema::model::State;
use bui_schema::paths::Paths;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use time::OffsetDateTime;
use uuid::Uuid;

pub const MODULE_NAME: &str = "panel";
/// 两个 hysteria 的 trafficStats 监听端口（由 `bui_schema::render::hysteria` 写死）。
pub const HY2_STATS_PORT_DIRECT: u16 = 9999;
pub const HY2_STATS_PORT_RESI: u16 = 9998;
/// `bui_schema::render::hysteria` 渲染的 `trafficStats.secret` 是空串 ⇒ 不发 Authorization 头。
/// 若将来改成非空，`hy2::Hy2Client` 按调研 H13 发 `Authorization: <secret>`（**无** `Bearer ` 前缀）。
pub const HY2_STATS_SECRET: &str = "";
/// Xray 的 api inbound（`bui_schema::render::xray` 的 `API_PORT` = 10085）。
pub const XRAY_API_ADDR: &str = "127.0.0.1:10085";
/// 两个 REALITY inbound 的 tag（`bui_schema::render::xray` 的 `vless_inbound` 调用处）。
pub const XRAY_INBOUND_TAGS: [&str; 2] = ["vless-direct", "vless-residential"];
/// VLESS `Account.flow`（`bui_schema::render::xray::clients` 里同值）。
pub const VLESS_FLOW: &str = "xtls-rprx-vision";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxRx {
    pub tx: u64,
    pub rx: u64,
}

impl TxRx {
    pub fn total(&self) -> u64 {
        self.tx.saturating_add(self.rx)
    }

    pub fn add(&mut self, other: TxRx) {
        self.tx = self.tx.saturating_add(other.tx);
        self.rx = self.rx.saturating_add(other.rx);
    }

    pub fn is_zero(&self) -> bool {
        self.tx == 0 && self.rx == 0
    }
}

/// `/api/stats` 与 `/api/online` 读的同一份缓存（spec §4.2「读同一份缓存；面板开着不增加采样」）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SampleCache {
    /// 用户名 → 守护进程启动以来的累计增量
    pub stats: BTreeMap<String, TxRx>,
    /// 用户名 → 当前连接数
    pub online: BTreeMap<String, u32>,
    pub last_sample_at: Option<String>,
    /// 上次把内存增量并进 `state.json` 的时刻（Task 7 的 `tick` 用它判「到点该落盘了没」）
    pub last_flush_at: Option<OffsetDateTime>,
    /// 本轮采样错误（已脱敏）
    pub errors: Vec<String>,
}

/// 已经同步到内核的状态（只在进程内，用来做差分）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Applied {
    pub snapshot_sha: Option<String>,
    /// 已 AddUser 的 `user_id` → `vless_uuid`
    pub xray_users: BTreeMap<Uuid, Uuid>,
    /// 上次同步时 xray 的 `NRestarts`
    pub xray_restarts: Option<String>,
    /// 已经 RemoveUser 成功的用户（被封 / 到期 / 禁用）。被封用户要**无条件**删（决策 D6），
    /// 不能靠 `xray_users` 判「该不该删」——守护进程重启后 `xray_users` 是空的，
    /// 而 `xray-config.json` 里还带着他们的凭据。`xray_restarts` 变化时与 `xray_users` 一起清空。
    pub xray_removed: BTreeSet<Uuid>,
    /// 上次判定为拒绝的用户
    pub blocked: BTreeSet<Uuid>,
}

#[async_trait::async_trait]
pub trait XrayApi: Send + Sync {
    async fn add_user(&self, tag: &str, user_id: Uuid, vless_uuid: Uuid) -> anyhow::Result<()>;
    async fn remove_user(&self, tag: &str, user_id: Uuid) -> anyhow::Result<()>;
    /// `QueryStats(pattern="user>>>", reset=true)`：email（= `user_id` 的字符串）→ 本轮增量
    async fn query_user_deltas(&self) -> anyhow::Result<BTreeMap<String, TxRx>>;
}

#[async_trait::async_trait]
pub trait Hy2Api: Send + Sync {
    /// `GET /traffic?clear=1`：`user_id` → 本轮增量（`clear` 与读取在同一把锁里，H10）
    async fn traffic_clear(&self, port: u16) -> anyhow::Result<BTreeMap<String, TxRx>>;
    /// `GET /online`：`user_id` → 当前 QUIC 连接数（H11）
    async fn online(&self, port: u16) -> anyhow::Result<BTreeMap<String, u32>>;
    /// `POST /kick`，体是 JSON 字符串数组（H12）
    async fn kick(&self, port: u16, ids: &[String]) -> anyhow::Result<()>;
}

pub struct Shared {
    paths: OnceLock<Paths>,
    cache: tokio::sync::RwLock<SampleCache>,
    applied: tokio::sync::Mutex<Applied>,
    /// 同步反应器的轮次锁（见 [`Shared::sync_guard`]）
    sync: tokio::sync::Mutex<()>,
    pending: tokio::sync::Mutex<BTreeMap<Uuid, TxRx>>,
    xray_seen: tokio::sync::Mutex<BTreeMap<Uuid, OffsetDateTime>>,
    xray: Box<dyn XrayApi>,
    hy2: Box<dyn Hy2Api>,
}

impl Shared {
    pub fn new(xray: Box<dyn XrayApi>, hy2: Box<dyn Hy2Api>) -> Self {
        Self {
            paths: OnceLock::new(),
            cache: tokio::sync::RwLock::new(SampleCache::default()),
            applied: tokio::sync::Mutex::new(Applied::default()),
            sync: tokio::sync::Mutex::new(()),
            pending: tokio::sync::Mutex::new(BTreeMap::new()),
            xray_seen: tokio::sync::Mutex::new(BTreeMap::new()),
            xray,
            hy2,
        }
    }

    /// `AppState` 里没有 `paths`，而 `/packages/*` 与快照重写都要它；`render()` 与 `spawn()`
    /// 各调一次 `set_paths`，两者都跑在处理第一个请求之前（P1 的 `serve::run` 先对账、
    /// 再起后台任务、最后才 listen）。
    pub fn paths(&self) -> Paths {
        self.paths
            .get()
            .cloned()
            .unwrap_or_else(Paths::default_server)
    }

    pub fn set_paths(&self, p: &Paths) {
        let _ = self.paths.set(p.clone());
    }

    pub fn snapshot_path(&self) -> PathBuf {
        crate::paths::auth_snapshot_file(&self.paths())
    }

    pub fn packages_dir(&self) -> PathBuf {
        self.paths().base_dir.join("packages")
    }

    pub fn xray(&self) -> &dyn XrayApi {
        self.xray.as_ref()
    }

    pub fn hy2(&self) -> &dyn Hy2Api {
        self.hy2.as_ref()
    }

    pub async fn cache(&self) -> tokio::sync::RwLockReadGuard<'_, SampleCache> {
        self.cache.read().await
    }

    pub async fn cache_mut(&self) -> tokio::sync::RwLockWriteGuard<'_, SampleCache> {
        self.cache.write().await
    }

    pub async fn applied(&self) -> tokio::sync::MutexGuard<'_, Applied> {
        self.applied.lock().await
    }

    /// 同步反应器的轮次锁：`users::sync_users` 全程持有它（采样任务与事件反应器都会调它，
    /// 同一时刻只许一轮），但**不**全程持有 `applied`——`applied` 只在算差分与写回记账时
    /// 短暂加锁。否则 2×N 次 gRPC（每次最长 5 秒）都抱着 `applied`，xray 掉线时
    /// `traffic::write_health_summary` 会跟着卡住，`/api/users/health` 一起卡。
    pub async fn sync_guard(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.sync.lock().await
    }

    pub async fn pending(&self) -> tokio::sync::MutexGuard<'_, BTreeMap<Uuid, TxRx>> {
        self.pending.lock().await
    }

    pub async fn xray_seen(&self) -> tokio::sync::MutexGuard<'_, BTreeMap<Uuid, OffsetDateTime>> {
        self.xray_seen.lock().await
    }
}

pub struct PanelModule {
    shared: Arc<Shared>,
}

impl PanelModule {
    pub fn new() -> Self {
        Self::with_shared(Arc::new(Shared::new(
            Box::new(xray::XrayClient::new()),
            Box::new(hy2::Hy2Client::new()),
        )))
    }

    pub fn with_shared(shared: Arc<Shared>) -> Self {
        Self { shared }
    }

    pub fn shared(&self) -> Arc<Shared> {
        self.shared.clone()
    }
}

impl Default for PanelModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for PanelModule {
    fn name(&self) -> &'static str {
        MODULE_NAME
    }

    /// **空**。见 P2 计划「渲染边界」一节：`xray-config.json` 的 `clients` 归 P1 Task 10 的
    /// `core_files`（`structural_hash` 排除 `clients` ⇒ 增删用户不重启 xray），
    /// `auth-snapshot.json` 不是 artifact（它在 P1 的 `BASE_WHITELIST` 里），由本模块原子重写。
    fn render(&self, _s: &State, ctx: &RenderCtx) -> Vec<Artifact> {
        self.shared.set_paths(&ctx.paths);
        Vec::new()
    }

    fn routes(&self) -> axum::Router<AppState> {
        // Task 13 接线
        axum::Router::new()
    }

    fn public_routes(&self) -> axum::Router<AppState> {
        // Task 13 接线
        axum::Router::new()
    }

    fn spawn(&self, _ctx: DaemonCtx) -> Vec<tokio::task::JoinHandle<()>> {
        // Task 13 接线
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::testsupport::{full, harness, send};
    use super::*;
    use crate::reconcile::Facts;
    use pretty_assertions::assert_eq;

    /// 一个只为测「公开路由不过鉴权、受保护路由过鉴权」而存在的模块（与 T0 的回归锁互补：
    /// T0 锁的是 P1 `api::router()` 的装配，这一条锁的是 `testsupport::full` 与生产装配一致）
    struct ProbeModule;

    impl Module for ProbeModule {
        fn name(&self) -> &'static str {
            "probe"
        }

        fn render(&self, _s: &State, _c: &RenderCtx) -> Vec<Artifact> {
            Vec::new()
        }

        fn routes(&self) -> axum::Router<AppState> {
            axum::Router::new().route("/probe/admin", axum::routing::get(|| async { "admin" }))
        }

        fn public_routes(&self) -> axum::Router<AppState> {
            axum::Router::new().route("/probe/open", axum::routing::get(|| async { "open" }))
        }
    }

    fn ctx(paths: &Paths) -> RenderCtx {
        RenderCtx {
            paths: paths.clone(),
            facts: Facts {
                mem_mb: 2048,
                arch: "x86_64".into(),
                hostname: "node-a".into(),
                has_ufw: false,
                ufw_active: false,
                has_firewalld: false,
                firewalld_active: false,
                ssh_unit: "sshd".into(),
                ssh_pubkeys: 1,
                systemd_resolved: false,
            },
        }
    }

    #[tokio::test]
    async fn module_is_named_panel() {
        let h = harness().await;
        assert_eq!(
            PanelModule::with_shared(h.shared.clone()).name(),
            MODULE_NAME
        );
        assert_eq!(MODULE_NAME, "panel");
    }

    #[tokio::test]
    async fn render_produces_nothing_so_core_files_stays_the_only_renderer() {
        // 渲染边界：xray-config.json 的 clients 由 P1 Task 10 的 core_files 渲染，
        // auth-snapshot.json 不是 artifact。这里多产出一个同路径 artifact 就会让
        // 「二次对账零变更」永久失效（见 P2 计划「渲染边界」一节）。
        let h = harness().await;
        let m = PanelModule::with_shared(h.shared.clone());
        // `Store::read` 给的是 `Arc<State>`（P1 的真实签名），这里显式解引用
        let state = h.store.read().await;
        let arts = m.render(&state, &ctx(&h.paths));
        assert_eq!(arts, Vec::new());
        // render 顺带把 paths 交给 Shared（handler 要用 <base>/packages）
        assert_eq!(h.shared.paths().base_dir, h.paths.base_dir);
    }

    #[tokio::test]
    async fn public_routes_skip_require_admin_and_protected_routes_do_not() {
        let h = harness().await;
        let router = full(&h.app, std::sync::Arc::new(ProbeModule));
        let (open, _) = send(&router, "GET", "/probe/open", None, None).await;
        assert_eq!(
            open,
            axum::http::StatusCode::OK,
            "公开路由必须无 token 可达"
        );
        let (denied, body) = send(&router, "GET", "/probe/admin", None, None).await;
        assert_eq!(denied, axum::http::StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"], "Unauthorized");
        let tok = super::testsupport::token(&h).await;
        let (allowed, _) = send(&router, "GET", "/probe/admin", Some(&tok), None).await;
        assert_eq!(allowed, axum::http::StatusCode::OK);
    }

    #[tokio::test]
    async fn shared_paths_default_to_the_server_layout_and_are_set_once() {
        let s = Shared::new(
            Box::new(super::fakes::FakeXray::new()),
            Box::new(super::fakes::FakeHy2::new()),
        );
        assert_eq!(s.paths().base_dir, PathBuf::from("/opt/b-ui"));
        let p = Paths {
            base_dir: "/tmp/x".into(),
            certs_dir: "/tmp/x/certs".into(),
            bin_dir: "/tmp/x/bin".into(),
        };
        s.set_paths(&p);
        assert_eq!(
            s.snapshot_path(),
            PathBuf::from("/tmp/x/auth-snapshot.json")
        );
        assert_eq!(s.packages_dir(), PathBuf::from("/tmp/x/packages"));
        // OnceLock：第二次 set 不生效，也不 panic
        s.set_paths(&Paths::default_server());
        assert_eq!(s.paths().base_dir, PathBuf::from("/tmp/x"));
    }

    #[test]
    fn txrx_saturates_and_reports_totals() {
        let mut a = TxRx { tx: 3, rx: 4 };
        assert_eq!(a.total(), 7);
        a.add(TxRx {
            tx: u64::MAX,
            rx: 1,
        });
        assert_eq!(a.tx, u64::MAX, "饱和加，不 panic 也不回绕");
        assert!(!a.is_zero());
        assert!(TxRx::default().is_zero());
    }

    /// 空壳只为让 `PanelModule::new()` 从 Task 1 起就能编译；它们必须报错而不是假装成功。
    ///
    /// ⚠️ **T4 / T5 把两个客户端填实之后，这条测试仍然要绿，所以它只允许拨死端口
    /// `127.0.0.1:1`**（本机必然连不上，符合 P2 计划铁律）：
    /// - `XrayApi::query_user_deltas` 的 `QueryStats` 带 `reset=true`，打到真实的
    ///   `127.0.0.1:10085` 会把线上 Xray 的用户计数器**原子清零** ——
    ///   在跑着内核的机器上执行 `cargo test` 就等于把流量账清零；
    /// - `Hy2Api::online` 打到真实的 9999 在有内核的机器上会**成功**，`is_err()` 当场失败。
    ///
    /// T4 / T5 的实现者：不要把下面两次真实调用改成 `XRAY_API_ADDR` /
    /// `HY2_STATS_PORT_DIRECT`，也不要因为「空壳已经填实了」就删掉这条测试。
    #[tokio::test]
    async fn the_clients_report_errors_instead_of_pretending_and_only_dial_a_dead_port() {
        // 默认地址只做纯字符串断言，不发起任何连接
        let c = super::xray::XrayClient::new();
        assert_eq!(c.addr(), XRAY_API_ADDR);
        let k = super::hy2::Hy2Client::new();
        assert_eq!(
            k.url(HY2_STATS_PORT_DIRECT, "/online"),
            "http://127.0.0.1:9999/online"
        );
        // 真正发起的两次调用都只拨 127.0.0.1:1
        let dead_xray = super::xray::XrayClient::with_addr("127.0.0.1:1");
        assert!(XrayApi::query_user_deltas(&dead_xray).await.is_err());
        assert!(Hy2Api::online(&k, 1).await.is_err(), "端口 1 上没人听");
    }
}
