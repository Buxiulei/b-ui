//! P2 面板与用户域：管理员 API、订阅与节点端点、嵌入前端、`/packages`、
//! 采样与限额、`auth-snapshot.json`、Xray gRPC、两条 Hysteria2 鉴权路径
//! （默认 `auth_http` 的进程内应答，退路 `auth_hook` 的 `bui auth-hook`）。
//!
//! 对 P1 只暴露一个 [`PanelModule`]（`reconcile::Module` 的实现）。
//!
//! Task 1 留的子树级 `#![allow(dead_code)]` 已在 Task 13 收口时删掉：契约面的每一项
//! （`Shared` 的各把锁、[`Applied`] / [`SampleCache`] 的字段、常量、`testsupport` 的挂载
//! helper）都有了真实调用方。以后这里再出 dead_code 告警就是真正的死代码，直接删符号。

pub mod api_admin;
pub mod api_me;
pub mod api_public;
pub mod assets;
pub mod auth_hook;
pub mod auth_http;
pub mod hy2;
pub mod hy2resi;
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
/// 住宅实例的 `trafficStats` 端口基准（槽 i = `9998 - i`，见 `bui_schema::slots`）。
/// 单槽时就是今天的 9998。
///
/// **4.1 起没有任何生产调用点**：住宅是一个 sing-box 入站，计量走 v2ray_api、在线与踢人
/// 走 Clash API（[`traffic::stats_ports`] 因此只剩直连那一个端口）。符号本身留给 T15
/// 的「删旧 API」一起清，`allow` 就是它已经是死代码的记号 —— 别再给它加调用点。
#[allow(dead_code)]
pub const HY2_STATS_PORT_RESI: u16 = bui_schema::slots::HY2_STATS_RESI_BASE;
/// 住宅 HY2（sing-box）的两个回环控制面（spec §2.3）：`HY2_RESI_CLASH_API` 上跑在线数、
/// 踢连接与门位（selector），`HY2_RESI_V2RAY_API` 上跑 `StatsService.QueryStats` 的计量。
/// 两个面都**只监听回环、不设 secret** ⇒ 这条路从不发 Authorization 头。
///
/// **端口只有一处来源：写出 `hy2-residential.json` 的那个渲染器。** 这里必须是 `pub use`
/// 而不是另定字面量——一旦两边分叉，客户端会去打没人听的端口，而计量（v2ray_api）与
/// 门位（Clash API）双双打空、测试却全绿，属静默生产故障。守门断言见
/// `panel::hy2resi::tests::the_endpoints_come_from_the_renderer`。
pub use bui_schema::render::hy2_singbox::{HY2_RESI_CLASH_API, HY2_RESI_V2RAY_API};
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
    /// 读回这个 email 在 `tag` 里挂着的 vless uuid（`HandlerService.GetInboundUsers`）；
    /// 位置空着返回 `Ok(None)`。`users::sync_users` 换 uuid 时先读后写，靠它避免
    /// 「uuid 没变也摘挂一遍」（2026-09-14 审查意见①②）。
    async fn inbound_user_uuid(&self, tag: &str, user_id: Uuid) -> anyhow::Result<Option<Uuid>>;
    /// `QueryStats(pattern="user>>>", reset=true)`：email（= `user_id` 的字符串）→ 本轮增量
    async fn query_user_deltas(&self) -> anyhow::Result<BTreeMap<String, TxRx>>;
    /// 追加一条住宅槽路由规则（`RoutingService.AddRule`，`shouldAppend=true` ⇒ 落在表尾）。
    /// `rule_tag` 与表内已有的重名会让**整条请求失败**，调用方必须先 `remove_rule`（D7）。
    async fn add_rule(&self, rule: &bui_schema::render::xray::SlotRule) -> anyhow::Result<()>;
    /// 按 `ruleTag` 删规则（`RoutingService.RemoveRule`）。tag 不存在也返回 `Ok`（内核语义，幂等）。
    async fn remove_rule(&self, rule_tag: &str) -> anyhow::Result<()>;
    /// 读回进程里**正在跑**的规则表：`(ruleTag, outboundTag)`，按表序；没有 `ruleTag` 的规则不列。
    async fn list_rules(&self) -> anyhow::Result<Vec<(String, String)>>;
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

/// 住宅 HY2（sing-box）的控制面：计量走 v2ray_api 的 gRPC，在线 / 踢人 / 门位走 Clash API
/// （spec §5.1、§5.2）。生产实现是 [`hy2resi::Hy2ResiClient`]，测试注入
/// `fakes::FakeHy2Resi`（见 [`Shared::with_hy2resi`]）。
#[async_trait::async_trait]
pub trait Hy2ResiApi: Send + Sync {
    /// `QueryStats(patterns=["user>>>"], reset=true)`：凭据 name → 本轮增量
    async fn query_user_deltas(&self) -> anyhow::Result<BTreeMap<String, TxRx>>;
    /// `GET /connections` → 每条的 (id, rule)；在线数按 rule 里的 `auth_user=<name>` 归组
    /// （`metadata` 里**没有** user 字段）
    async fn connections(&self) -> anyhow::Result<Vec<hy2resi::Hy2ResiConn>>;
    /// `DELETE /connections/{id}`：踢用户时「门切 `deny`」之后的逐条兜底
    async fn close_connection(&self, id: &str) -> anyhow::Result<()>;
    /// `GET /proxies` 一次读全部门位：selector tag → 当前成员
    async fn selected_all(&self) -> anyhow::Result<BTreeMap<String, String>>;
    /// `PUT /proxies/<selector>` `{"name":"<tag>"}`
    async fn select(&self, selector: &str, tag: &str) -> anyhow::Result<()>;
    /// `GET /version` 是否 2xx（住宅 sing-box 刚重启时还没起监听，重放门位前先探这一下）
    async fn ready(&self) -> bool;
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
    hy2resi: Box<dyn Hy2ResiApi>,
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
            // 「测试不碰真实系统」在这里是**结构保证**，不是「每个人记得注入」：
            // `Shared::new` 的八个调用点里七个是测试专用，所以测试构型下默认就给 fake。
            // 真按调用点数少数派（生产只有 `PanelModule::new` 一处）去改签名，代价是
            // 动 `users.rs` / `sentinel/*` 五处（T15 的签名收缩才碰它们）。
            #[cfg(test)]
            hy2resi: Box::new(fakes::FakeHy2Resi::new()),
            #[cfg(not(test))]
            hy2resi: Box::new(hy2resi::Hy2ResiClient::new()),
        }
    }

    /// 换掉住宅 HY2 的控制面客户端。
    ///
    /// 默认值按构型分流（见 [`Shared::new`]）：生产是 [`hy2resi::Hy2ResiClient`]，测试是
    /// `fakes::FakeHy2Resi`。所以**这个 builder 只在测试要拿句柄记账 / 注入失败时才用**
    /// （T11 的 fail-closed 用例），漏用它也不会让测试去打 `127.0.0.1` 的两个回环面。
    pub fn with_hy2resi(mut self, hy2resi: Box<dyn Hy2ResiApi>) -> Self {
        self.hy2resi = hy2resi;
        self
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

    pub fn hy2resi(&self) -> &dyn Hy2ResiApi {
        self.hy2resi.as_ref()
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
    /// 包缓存任务（`packages::cache_loop`）用的下载器。生产是 [`crate::kernels::HttpFetcher`]，
    /// 测试用 [`PanelModule::with_fetcher`] 注入内存 fake，于是 `spawn` 出来的任务不可能出网。
    fetcher: Arc<dyn crate::kernels::Fetcher>,
}

impl PanelModule {
    pub fn new() -> Self {
        Self::with_shared(Arc::new(Shared::new(
            Box::new(xray::XrayClient::new()),
            Box::new(hy2::Hy2Client::new()),
        )))
    }

    pub fn with_shared(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            fetcher: Arc::new(crate::kernels::HttpFetcher::new()),
        }
    }

    /// 换掉包缓存用的 `Fetcher`（只有测试需要；生产走 [`PanelModule::new`] 的 `HttpFetcher`）。
    #[cfg(test)]
    pub fn with_fetcher(mut self, fetcher: Arc<dyn crate::kernels::Fetcher>) -> Self {
        self.fetcher = fetcher;
        self
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
        api_admin::routes(self.shared.clone())
    }

    fn public_routes(&self) -> axum::Router<AppState> {
        api_public::public_routes(self.shared.clone())
            .merge(api_me::public_routes(self.shared.clone()))
            .merge(assets::public_routes(self.shared.clone()))
            .merge(packages::public_routes(self.shared.clone()))
    }

    /// 三个后台任务，顺序固定：①10 秒采样 ②用户同步反应器（事件驱动 + 60 秒）③每日包缓存。
    fn spawn(&self, ctx: DaemonCtx) -> Vec<tokio::task::JoinHandle<()>> {
        self.shared.set_paths(&ctx.paths);
        vec![
            tokio::spawn(traffic::sampling_loop(ctx.clone(), self.shared.clone())),
            tokio::spawn(users::sync_loop(ctx.clone(), self.shared.clone())),
            tokio::spawn(packages::cache_loop(
                ctx,
                self.shared.clone(),
                self.fetcher.clone(),
            )),
        ]
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
                nft_tables: Default::default(),
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
        // 铁律锁：整套支架只活在 tempfile 的临时目录里，`Harness::dir` 就是它的存活守卫
        assert_eq!(h.paths.base_dir.as_path(), h.dir.path());
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

    /// `with_hy2resi` 是给「要拿句柄记账 / 注入失败」的用例用的通路
    /// （T10 的采样、T11 的门位收敛都靠它）。
    #[tokio::test]
    async fn the_hy2resi_client_is_injectable_so_tests_can_account_for_its_calls() {
        let fake = super::fakes::FakeHy2Resi::new();
        let s = Shared::new(
            Box::new(super::fakes::FakeXray::new()),
            Box::new(super::fakes::FakeHy2::new()),
        )
        .with_hy2resi(Box::new(fake.clone()));
        assert!(s.hy2resi().ready().await, "注入的 fake 默认可用");
        assert_eq!(fake.calls(), vec!["ready".to_string()]);
        fake.clear_calls();
        fake.set_selected(BTreeMap::from([(
            "gate-r000".to_string(),
            "deny".to_string(),
        )]));
        assert_eq!(
            s.hy2resi().selected_all().await.unwrap()["gate-r000"],
            "deny"
        );
        assert_eq!(fake.calls(), vec!["selected_all".to_string()]);
    }

    /// **漏注入也不许碰真实系统**：测试构型下 `Shared::new` 的默认住宅控制面客户端是
    /// fake，不是只连 `127.0.0.1:9092` / `:10086` 的生产客户端（`new` 的八个调用点里
    /// 七个是测试专用，所以这件事必须由构型兜住，不能靠「每个人记得 `with_hy2resi`」）。
    ///
    /// 判据就是「不出网也能成」：fake 立刻回 `ready = true` 且 `Ok(空表)`；换成生产客户端，
    /// 本机没人听那两个回环端口 ⇒ `ready()` 为假、`query_user_deltas()` 是 `Err`。
    #[tokio::test]
    async fn the_default_hy2resi_client_in_test_builds_is_the_fake() {
        let s = Shared::new(
            Box::new(super::fakes::FakeXray::new()),
            Box::new(super::fakes::FakeHy2::new()),
        );
        assert!(s.hy2resi().ready().await, "默认客户端必须是 fake");
        assert_eq!(
            s.hy2resi().query_user_deltas().await.unwrap(),
            BTreeMap::new()
        );
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

    #[tokio::test]
    async fn routes_cover_every_admin_endpoint_and_nothing_public() {
        let h = testsupport::harness().await;
        // 给 alice 一个订阅 token：下面探「订阅不在 routes() 里」时必须用**能取到东西**的
        // 末段，否则 handler 自己也会回 404，真挂错了也看不出来
        let tok = "0123456789abcdef0123456789abcdef";
        h.store
            .update(|s| s.users[0].sub_token = Some(tok.into()))
            .await
            .unwrap();
        let router =
            testsupport::mount(&h.app, PanelModule::with_shared(h.shared.clone()).routes());
        // 受保护路由在这里是裸挂的（没套 require_admin），只验「路由存在」
        for (m, p) in [
            ("GET", "/api/users"),
            ("GET", "/api/stats"),
            ("GET", "/api/online"),
            ("GET", "/api/config"),
            ("GET", "/api/masquerade"),
            ("GET", "/api/bandwidth"),
            ("GET", "/api/port-hopping"),
            ("GET", "/api/hy2/watchdog/status"),
            ("GET", "/api/users/health"),
        ] {
            let (s, _) = testsupport::send(&router, m, p, None, None).await;
            assert_ne!(s, axum::http::StatusCode::NOT_FOUND, "{m} {p} 没挂上");
        }
        // 公开端点不该出现在 routes() 里
        for p in [
            &format!("/api/sub/{tok}"),
            &format!("/api/nodes/{tok}"),
            "/",
            "/packages/manifest.json",
            "/api/me",
        ] {
            let (s, _) = testsupport::send(&router, "GET", p, None, None).await;
            assert_eq!(
                s,
                axum::http::StatusCode::NOT_FOUND,
                "{p} 应该在 public_routes 里"
            );
        }
    }

    #[tokio::test]
    async fn public_routes_cover_subscriptions_front_end_packages_and_the_user_domain() {
        let h = testsupport::harness().await;
        // 订阅末段自 2026-09-14 裁决起是随机 token；用户名链接只在宽限期内还认，
        // 而 `sample_state` 不开宽限期 ⇒ 这里必须拿 token 去探路由挂没挂上
        let tok = "0123456789abcdef0123456789abcdef";
        h.store
            .update(|s| s.users[0].sub_token = Some(tok.into()))
            .await
            .unwrap();
        let router = testsupport::mount(
            &h.app,
            PanelModule::with_shared(h.shared.clone()).public_routes(),
        );
        for p in [
            "/",
            "/index.html",
            "/app.js",
            "/style.css",
            "/qrcode.min.js",
            "/logo.jpg",
            &format!("/api/sub/{tok}"),
            &format!("/api/subscription/{tok}"),
            &format!("/api/clash/{tok}"),
            &format!("/api/nodes/{tok}"),
            "/api/install-command",
            "/api/me",
            "/api/me/billing",
        ] {
            let (s, _) = testsupport::send(&router, "GET", p, None, None).await;
            assert_ne!(s, axum::http::StatusCode::NOT_FOUND, "{p} 没挂上");
        }
    }

    /// 只记 URL、一律失败的 `Fetcher`：注入它之后，`spawn` 出来的包缓存任务
    /// **不可能**碰到 `HttpFetcher`（铁律：测试不出网）。
    #[derive(Default)]
    struct FakeFetcher(std::sync::Mutex<Vec<String>>);

    impl FakeFetcher {
        fn urls(&self) -> Vec<String> {
            self.0.lock().unwrap().clone()
        }
    }

    impl crate::kernels::Fetcher for FakeFetcher {
        fn get_bytes(&self, url: &str) -> anyhow::Result<Vec<u8>> {
            self.0.lock().unwrap().push(url.to_string());
            anyhow::bail!("fake fetcher 不出网")
        }
    }

    #[tokio::test]
    async fn spawn_starts_three_tasks_and_hands_the_paths_to_shared() {
        let h = testsupport::harness().await;
        let s = Shared::new(
            Box::new(super::fakes::FakeXray::new()),
            Box::new(super::fakes::FakeHy2::new()),
        );
        // 播一份 manifest 缓存，让包缓存任务真的走到取二进制那一步（否则它直接跳过本轮，
        // 「注入生效」就没被证明过）
        let fetcher = std::sync::Arc::new(FakeFetcher::default());
        let mut artifacts = serde_json::Map::new();
        let mut want: Vec<String> = Vec::new();
        for name in packages::CLIENT_BINARIES {
            for arch in packages::CLIENT_ARCHES {
                let url = format!("https://fake.invalid/{name}-linux-{arch}");
                artifacts.insert(
                    format!("{name}-linux-{arch}"),
                    serde_json::json!({"url": url, "sha256": "00"}),
                );
                want.push(url);
            }
        }
        let manifest =
            serde_json::json!({"version": "4.0.0", "kernels": {}, "artifacts": artifacts});
        h.host.with(|i| {
            i.files.insert(
                crate::paths::manifest_file(&h.paths),
                (serde_json::to_vec(&manifest).unwrap(), 0o600),
            );
        });
        let m = PanelModule::with_shared(std::sync::Arc::new(s))
            .with_fetcher(fetcher.clone() as Arc<dyn crate::kernels::Fetcher>);
        let ctx = crate::reconcile::DaemonCtx {
            store: h.store.clone(),
            runtime: h.runtime.clone(),
            bus: h.app.bus.clone(),
            host: h.host.clone(),
            paths: h.paths.clone(),
        };
        let handles = m.spawn(ctx);
        assert_eq!(handles.len(), 3, "采样 / 用户同步 / 包缓存");
        assert_eq!(m.shared().paths().base_dir, h.paths.base_dir);
        // 让第一轮跑完再收摊，确认三个任务都没有立刻 panic
        for _ in 0..200 {
            if fetcher.urls().len() == want.len() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let mut got = fetcher.urls();
        got.sort();
        assert_eq!(got, want, "包缓存任务取的是注入进来的 Fetcher");
        for hd in &handles {
            assert!(!hd.is_finished(), "后台任务不该自己结束");
        }
        for hd in handles {
            hd.abort();
        }
    }

    #[tokio::test]
    async fn the_panel_module_is_registered_in_serve_modules() {
        let reg = crate::serve::modules(None);
        assert!(
            reg.modules.iter().any(|m| m.name() == MODULE_NAME),
            "serve::modules() 里没有 panel：{:?}",
            reg.modules.iter().map(|m| m.name()).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn the_user_health_endpoint_reports_the_summary_after_one_sampling_tick() {
        let h = testsupport::harness().await;
        let ctx = crate::reconcile::DaemonCtx {
            store: h.store.clone(),
            runtime: h.runtime.clone(),
            bus: h.app.bus.clone(),
            host: h.host.clone(),
            paths: h.paths.clone(),
        };
        traffic::tick(&ctx, &h.shared).await.unwrap();
        traffic::write_health_summary(&ctx, &h.shared).await;
        // 裁决 D2：摘要从 T8 的 `GET /api/users/health` 读，`/api/health` 不加用户段
        let router = h.router();
        let tok = testsupport::token(&h).await;
        let (s, users) =
            testsupport::send(&router, "GET", "/api/users/health", Some(&tok), None).await;
        assert_eq!(s, axum::http::StatusCode::OK);
        assert_eq!(users["total"], 1);
        assert_eq!(users["blocked"], 0);
        assert_eq!(users["month_key"], "2026-09");
        // 回归锁：P1 的 `HealthResponse` 一个字都没改，`/api/health` 里不该出现用户段
        let out = crate::api::health::get(axum::extract::State(h.app.clone())).await;
        let raw = serde_json::to_value(&out.0).unwrap();
        assert!(
            raw.get("users").is_none(),
            "裁决 D2 不批准给 /api/health 加用户段：{raw}"
        );
    }

    #[tokio::test]
    async fn the_full_router_serves_the_panel_login_and_a_subscription() {
        let h = testsupport::harness().await;
        let router = h.router();
        // 前端无鉴权
        let (s_index, _) = testsupport::text(&router, "/").await;
        assert_eq!(s_index, axum::http::StatusCode::OK);
        // 订阅无鉴权（末段是随机 token，见 `api_public::resolve`）
        let tok = "0123456789abcdef0123456789abcdef";
        h.store
            .update(|s| s.users[0].sub_token = Some(tok.into()))
            .await
            .unwrap();
        let (s_sub, _) = testsupport::text(&router, &format!("/api/sub/{tok}")).await;
        assert_eq!(s_sub, axum::http::StatusCode::OK);
        // 管理员端点要 token
        let (s_401, _) = testsupport::send(&router, "GET", "/api/users", None, None).await;
        assert_eq!(s_401, axum::http::StatusCode::UNAUTHORIZED);
        let tok = testsupport::token(&h).await;
        let (s_ok, v) = testsupport::send(&router, "GET", "/api/users", Some(&tok), None).await;
        assert_eq!(s_ok, axum::http::StatusCode::OK);
        assert_eq!(v[0]["username"], "alice");
    }
}
