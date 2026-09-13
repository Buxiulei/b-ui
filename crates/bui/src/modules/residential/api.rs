//! 住宅模块的面板端点（spec §4.3 + 契约决策 §A）：规范路径 + v3 路径别名，
//! 共 18 条路由。`status` / `health` 的字段名逐字照 v3（`web/app.js` 直接读），
//! v4 的新字段一律**追加**。
//!
//! 鉴权由 P1 的 `api::router()` 统一 `layer(require_admin)`，本模块**不加**中间件。

use super::clash::Clash;
use super::proxy::Prober;
use super::{blacklist, check, clash, health, state, upstream, MANUAL_ROUND_MIN_GAP_SECS};
use crate::api::AppState;
use crate::reconcile::DaemonCtx;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::{Extension, Json};
// `State` 这个名字被 axum 的提取器占了，模型态一律用 `SchemaState`
use bui_schema::model::{
    ResiMode, ResidentialGroup, Rule, State as SchemaState, Upstream, UpstreamKind,
};
use bui_schema::paths::Paths;
use bui_schema::render::SplitRules;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

// ── 请求体 ───────────────────────────────────────────────────────────────
#[derive(Debug, Deserialize)]
pub struct AddRequest {
    pub url: String,
}

#[derive(Debug, Deserialize)]
pub struct RemoveRequest {
    /// uuid / `resi-N` / `url-N` / `host:port`（见 [`upstream::resolve_upstream`]）
    #[serde(default)]
    pub id: Option<String>,
    /// v3 的定位方式 `"host:port"`
    #[serde(default)]
    pub host_port: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GlobalRequest {
    pub global: bool,
}

/// v3 的 `POST /api/residential/enable` 是**空体、且不带 `Content-Type`**，所以 handler 的
/// 提取器必须是 `Option<Json<EnableRequest>>`（`None` ⇒ `enabled = true`）；`#[serde(default)]`
/// 只救得了 `{}` 这种「有 JSON 但缺字段」的请求，救不了「没有 body」的 415
#[derive(Debug, Deserialize)]
pub struct EnableRequest {
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct DomainsRequest {
    /// `null` 或缺省 = 回到跟随默认表
    #[serde(default)]
    pub domains: Option<Vec<String>>,
    #[serde(default)]
    pub reset: bool,
}

/// `id` 缺省 = 体检当前落点。取值见 [`upstream::resolve_upstream`]
#[derive(Debug, Deserialize)]
pub struct CheckRequest {
    #[serde(default)]
    pub id: Option<String>,
}

/// `id` 取值见 [`upstream::resolve_upstream`]。**不是 `Uuid`**：面板传 uuid，运维照着
/// `status` 里的 `resi-2` 敲也得认，否则只能拿到 serde 的「invalid character」
#[derive(Debug, Deserialize)]
pub struct SelectRequest {
    /// 切到这条上游并**锁定**到它不健康为止（R2 ①）
    #[serde(default)]
    pub id: Option<String>,
    /// `true` = 解除手动锁定，回到自动选路（与 `bui residential select --auto` 同义）
    #[serde(default)]
    pub auto: bool,
}

#[derive(Debug, Deserialize)]
pub struct PinRequest {
    /// `domain_suffix`（缺省）| `domain` | `port`
    #[serde(default)]
    pub kind: Option<String>,
    pub value: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Deserialize)]
pub struct ForgetAutoRequest {
    pub upstream_id: Uuid,
    #[serde(default)]
    pub kind: Option<String>,
    pub value: String,
}

#[derive(Debug, Deserialize)]
pub struct PriorityRequest {
    pub id: Uuid,
    pub priority: u32,
}

/// v3 的 `POST /api/residential`（一条路径三种语义：加上游 / 设关键字 / 恢复默认）
#[derive(Debug, Deserialize)]
pub struct V3PostRequest {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub domains: Option<Vec<String>>,
    #[serde(default)]
    pub reset: bool,
}

// ── 响应体 ───────────────────────────────────────────────────────────────
/// `GET /api/residential/status`（= v3 `GET /api/residential`）。
/// **前 7 个字段名逐字照 v3**（`web/app.js:827/875/964` 直接读），其余是 v4 追加项。
#[derive(Debug, Serialize, PartialEq)]
pub struct StatusResponse {
    pub enabled: bool,
    pub global: bool,
    pub urls: Vec<UrlRow>,
    pub domains: Vec<String>,
    #[serde(rename = "domainsFollowDefault")]
    pub domains_follow_default: bool,
    #[serde(rename = "lastVerifiedIp")]
    pub last_verified_ip: String,
    #[serde(rename = "lastVerifiedIspInfo")]
    pub last_verified_isp_info: String,
    // v4 追加（只追加，不改不删任何 v3 字段）
    pub mode: ResiMode,
    /// **state 里的配置落点**（selector 的 default、`ports_allowed` 取反与 `auto` 过滤的依据）
    pub selected_upstream_id: Option<Uuid>,
    /// **当前实际生效的上游**（`runtime.selected_upstream_id`，契约决策 §C）
    pub active_upstream_id: Option<Uuid>,
    /// 上一行对应的成员 tag，由 `clash::tag_of` 现算（面板显示用，不做主键）
    pub active_tag: Option<String>,
    pub selected_pending_persist: bool,
    /// UDP 走不走住宅出口（全 socks5 池才走，见 [`ResidentialGroup::udp_via_pool`]）
    pub udp_via_residential: bool,
    pub checking: Option<state::Checking>,
    pub blacklist: BlacklistCounts,
    pub upstreams: Vec<UpstreamRow>,
    pub notes: Vec<String>,
    pub alerts: Vec<String>,
    /// 一行「当前这条出口为什么是它」（[`health::selection_reason`]，主理人 2026-09-12）。
    /// 与 [`HealthResponse::selected_reason`] 同一个函数
    pub selected_reason: Option<String>,
    /// 「更优候选」防抖进度与上次全量测速时间，与 [`HealthResponse`] 的同名字段同源
    pub switch_improve_rounds: u32,
    pub switch_improve_needed: u32,
    pub switch_improve_candidate: Option<String>,
    pub last_speedtest_at: Option<String>,
    /// 槽位表（spec §5.6）。与 [`HealthResponse::slots`] 同一个投影函数（[`slot_rows`]）
    pub slots: Vec<SlotRow>,
}

/// v3 的 `urls[]` 行（`web/app.js:665-699 renderResidentialUrls` 逐字段读）
#[derive(Debug, Serialize, PartialEq)]
pub struct UrlRow {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub name: String,
    /// `"socks5"` | `"http"`（v3 的取值，不是 `UpstreamKind` 的 serde 名 —— 它俩恰好一致，
    /// 但这里显式转，免得以后改了 model 打死前端）
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(rename = "lastVerifiedIp")]
    pub last_verified_ip: String,
    #[serde(rename = "displayUrl")]
    pub display_url: String,
    // v4 追加
    pub id: Uuid,
    pub priority: u32,
}

/// v4 追加的上游明细（面板新卡片用；**永不含 password**）
#[derive(Debug, Serialize, PartialEq)]
pub struct UpstreamRow {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub host: String,
    pub port: u16,
    pub username_masked: String,
    pub priority: u32,
    pub provider: Option<String>,
    pub region: Option<String>,
    pub ports_allowed: Option<Vec<u16>>,
    pub verified: Option<bui_schema::model::Verified>,
    /// 经这条上游还能不能正常用 Google 搜索（最近一轮巡检的结论，`None` = 未知）。
    /// 主理人硬要求「住宅上游不封 Google」，面板要能一眼看出是哪条封了（R2 ②）
    pub google_ok: Option<bool>,
    pub google_at: Option<String>,
    /// **只数这一条上游自己的 `auto` 条目**（`auto[].upstream_id == id`），一律经
    /// [`auto_count`]。**不含 `pins`**：pins 是全局强制直连规则，不属于任何上游，
    /// 计进去会让面板上每个上游都凭空多出 `pins.len()` 条。全局计数看
    /// [`StatusResponse::blacklist`]（`BlacklistCounts{pins,auto,pending,candidates}`）
    pub blacklist_count: usize,
    /// 最近一次体检报告（`check::CheckReport` 的 JSON）
    pub check: Option<serde_json::Value>,
    /// 与 [`MemberRow::metrics`] 同源同函数（[`metrics_of`]）
    #[serde(flatten)]
    pub metrics: Metrics,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct BlacklistCounts {
    pub pins: usize,
    pub auto: usize,
    pub pending: usize,
    pub candidates: usize,
}

/// `GET /api/residential/health`（= v3 同名端点）。前 9 个字段名逐字照 v3。
#[derive(Debug, Serialize, PartialEq)]
pub struct HealthResponse {
    pub enabled: bool,
    pub urls: Vec<HealthUrlRow>,
    pub domains_count: usize,
    /// `"selector"`（池有效）| `"none"`
    pub mode: String,
    pub selected: Option<String>,
    pub members: Vec<MemberRow>,
    pub current_egress_ip_test: Option<String>,
    pub egress_ip_type: String,
    pub via_proxy_isp: Option<String>,
    // v4 追加
    /// 同 [`StatusResponse::udp_via_residential`]
    pub udp_via_residential: bool,
    pub alerts: Vec<String>,
    pub last_daily_at: Option<String>,
    pub notes: Vec<String>,
    /// 一行「当前选中 resi-N 的原因」（主理人 2026-09-12）
    pub selected_reason: Option<String>,
    /// 「更优候选」防抖的进度：已连续满足 `switch_improve_rounds` 轮，
    /// 攒到 `switch_improve_needed` 轮才切
    pub switch_improve_rounds: u32,
    pub switch_improve_needed: u32,
    /// 上一行那个候选的 tag（`None` = 本轮没有更优候选）
    pub switch_improve_candidate: Option<String>,
    /// 上一次全量测速的时间（面板据此知道速度数字有多新）
    pub last_speedtest_at: Option<String>,
    /// 槽位表（spec §5.6）。与 [`StatusResponse::slots`] 同一个投影函数（[`slot_rows`]）
    pub slots: Vec<SlotRow>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct HealthUrlRow {
    pub host: String,
    pub port: u16,
    pub last_verified_at: Option<String>,
    pub last_verified_ip: Option<String>,
    pub last_verified_isp: Option<String>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct MemberRow {
    pub tag: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub host: String,
    pub port: u16,
    pub active: bool,
    pub failstreak: u32,
    pub okstreak: u32,
    pub egress: Option<Egress>,
    // v4 追加
    pub priority: u32,
    pub success_rate_24h: f64,
    /// 与 [`UpstreamRow::blacklist_count`] **同一语义、同一函数**（[`auto_count`]）：
    /// 只数 `auto[].upstream_id == upstream_id` 的条目，不含 `pins`。`status` 与
    /// `health` 的同名字段必须永远相等，所以两处都不许自己写过滤式
    pub blacklist_count: usize,
    /// 最近一次巡检样本的结果（`None` = 还没探过）
    pub probe_ok: Option<bool>,
    /// 管理员手动锁定在这条上游（`runtime.manual_selected_id`，R2 ①）：
    /// 巡检不会因为别的成员 priority 更好就把它切走
    pub manual_locked: bool,
    /// 与 [`UpstreamRow::google_ok`] 同源（都读 `runtime.health[<id>]`，R2 ②）
    pub google_ok: Option<bool>,
    pub google_at: Option<String>,
    pub upstream_id: Uuid,
    /// 延迟 / 速度 / UDP 指标（主理人 2026-09-12）。与 [`UpstreamRow`] 的同名字段
    /// **同源同函数**（[`metrics_of`]），面板两处显示的必须是同一个数
    #[serde(flatten)]
    pub metrics: Metrics,
}

/// 一个成员的延迟 / 速度 / UDP 指标。`status` 的 [`UpstreamRow`] 与 `health` 的
/// [`MemberRow`] 都 flatten 它 —— 同一份数字不许有两套算法。
/// 每一项都是 `Option`：**「没测过」必须与「0」区分开**，否则面板会把一条什么都没测
/// 的上游显示成延迟最低、也会让运维以为它最快。
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Metrics {
    /// 经上游的完整 HTTP 往返延迟（毫秒），选路用的就是 p50
    pub latency_p50_ms: Option<u64>,
    pub latency_p95_ms: Option<u64>,
    /// 到上游网关的 TCP 建连耗时（毫秒）
    pub tcp_p50_ms: Option<u64>,
    pub tcp_p95_ms: Option<u64>,
    /// 最近若干次测速的中位数（Mbps）与测速时间
    pub down_mbps: Option<f64>,
    pub up_mbps: Option<f64>,
    pub speed_at: Option<String>,
    pub speed_note: Option<String>,
    /// 经该上游 SOCKS5 UDP ASSOCIATE + STUN 的结论、UDP 出口 IP 与往返 p50
    pub udp_ok: Option<bool>,
    pub udp_exit_ip: Option<String>,
    pub udp_p50_ms: Option<u64>,
    pub udp_note: Option<String>,
    pub udp_at: Option<String>,
}

/// `state` + `runtime` → 槽位表。**唯一**一份投影：`status` / `health` /
/// `GET /slots` / CLI 都调它，三处显示的数字永远一致。
pub fn slot_rows(s: &SchemaState, r: &state::ResiRuntime) -> Vec<SlotRow> {
    let g = state::group_of(s);
    let h = |id: Uuid| r.health.get(&id.to_string()).cloned().unwrap_or_default();
    bui_schema::slots::sorted(&s.residential)
        .into_iter()
        .filter_map(|sl| {
            let own = g.upstreams.iter().find(|u| u.id == sl.upstream_id)?;
            let res = bui_schema::slots::resources_of(&s.node.ports, &s.residential, sl.index);
            let sr = r
                .slots
                .get(&sl.index.to_string())
                .cloned()
                .unwrap_or_default();
            let active = sr.current_upstream_id;
            let borrowed = active.is_some_and(|a| a != sl.upstream_id);
            let mut users: Vec<String> = bui_schema::slots::users_of_slot(s, sl.upstream_id)
                .into_iter()
                .map(|u| u.username.clone())
                .collect();
            users.sort();
            Some(SlotRow {
                index: sl.index,
                selector: crate::modules::residential::slot_selector(sl.index),
                upstream_id: sl.upstream_id,
                upstream_tag: clash::tag_of(&g, sl.upstream_id).unwrap_or_default(),
                host: own.host.clone(),
                port: own.port,
                ip: own.verified.as_ref().map(|v| v.ip.clone()),
                active_upstream_id: active,
                active_tag: active.and_then(|a| clash::tag_of(&g, a)),
                borrowed,
                borrowed_from: active
                    .filter(|_| borrowed)
                    .and_then(|a| g.upstreams.iter().find(|u| u.id == a))
                    .map(|u| format!("{}:{}", u.host, u.port)),
                pinned: sr.pinned_upstream_id.is_some(),
                back_rounds: sr.back_rounds,
                back_rounds_needed: crate::modules::residential::SLOT_BACK_ROUNDS,
                relay_port: res.relay_port,
                hy2_port: res.hy2_port,
                hop: res.hop,
                user_count: users.len(),
                users,
                metrics: metrics_of(&h(sl.upstream_id)),
            })
        })
        .collect()
}

/// `runtime.health[<id>]` → [`Metrics`]。**唯一**一份投影
pub fn metrics_of(h: &state::HealthState) -> Metrics {
    Metrics {
        latency_p50_ms: state::percentile(&h.http_ms, 50.0),
        latency_p95_ms: state::percentile(&h.http_ms, 95.0),
        tcp_p50_ms: state::percentile(&h.tcp_ms, 50.0),
        tcp_p95_ms: state::percentile(&h.tcp_ms, 95.0),
        down_mbps: state::median(&h.down_mbps),
        up_mbps: state::median(&h.up_mbps),
        speed_at: h.speed_at.clone(),
        speed_note: h.speed_note.clone(),
        udp_ok: h.udp_ok,
        udp_exit_ip: h.udp_exit_ip.clone(),
        udp_p50_ms: state::percentile(&h.udp_ms, 50.0),
        udp_note: h.udp_note.clone(),
        udp_at: h.udp_at.clone(),
    }
}

/// 一个槽位在面板上的一行（spec §5.6「按槽列出 IP、当前实际出口、用户数与用户名、指标」）。
#[derive(Debug, Serialize, PartialEq)]
pub struct SlotRow {
    pub index: u16,
    /// relay 里这一槽的 selector tag
    pub selector: String,
    /// 本槽自己的 IP（上游 uuid 与它的成员 tag）
    pub upstream_id: Uuid,
    pub upstream_tag: String,
    pub host: String,
    pub port: u16,
    /// 本槽 IP 的出口地址（`verified.ip`，未体检过为 `None`）
    pub ip: Option<String>,
    /// **当前实际出口**：本槽的 IP，或借用来的那条
    pub active_upstream_id: Option<Uuid>,
    pub active_tag: Option<String>,
    pub borrowed: bool,
    /// 借用来源（`borrowed` 为真时是那条上游的 `host:port`）
    pub borrowed_from: Option<String>,
    pub pinned: bool,
    /// 借用中「本槽已连续恢复几轮」与门槛
    pub back_rounds: u32,
    pub back_rounds_needed: u32,
    /// 这一槽的端口资源（面板据此告诉运维该放行什么、订阅里是哪个端口）
    pub relay_port: u16,
    pub hy2_port: u16,
    pub hop: (u16, u16),
    /// 落在这一槽上的用户（用户名，按名字升序）与数量
    pub users: Vec<String>,
    pub user_count: usize,
    /// 与 [`MemberRow::metrics`] 同源同函数（[`metrics_of`]）
    #[serde(flatten)]
    pub metrics: Metrics,
}

#[derive(Debug, Deserialize)]
pub struct PinSlotRequest {
    pub index: u16,
    /// `<uuid>` / `resi-N` / `url-N` / `<host:port>`，由 `upstream::resolve_upstream` 解析
    #[serde(default)]
    pub id: Option<String>,
    /// 解除 pin
    #[serde(default)]
    pub auto: bool,
}

#[derive(Debug, Deserialize)]
pub struct AssignRequest {
    /// 用户名（面板与 CLI 都按用户名说话）
    pub user: String,
    /// 槽序号（`"0"`）或上游定位串（`<uuid>` / `resi-N` / `url-N` / `<host:port>`）
    pub target: String,
}

/// v3 `members[].egress`：`type` 是中文串（`web/app.js:1095` 用 `/IDC|机房/i` 判色）
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Egress {
    pub ip: Option<String>,
    #[serde(rename = "type")]
    pub kind: String,
    pub isp: Option<String>,
    pub country: Option<String>,
    pub city: Option<String>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct BlacklistResponse {
    pub pins: Vec<PinRow>,
    pub auto: Vec<AutoRow>,
    pub pending: Vec<PendingRow>,
    pub candidates: Vec<CandidateRow>,
    pub checking: Option<state::Checking>,
    pub last_daily_at: Option<String>,
    /// 面板说明文案（spec §5.4「局限」）
    pub notes: Vec<String>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct PinRow {
    pub kind: String,
    pub value: String,
    pub note: String,
    pub created_at: String,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct AutoRow {
    pub upstream_id: Uuid,
    pub upstream_name: String,
    /// 该上游的槽序号（已移除的上游为 `null`）；CLI 按上游分组时写进组标题
    pub upstream_slot: Option<u16>,
    /// 该上游的 `host:port`（已移除的上游为空串）
    pub upstream_addr: String,
    pub kind: String,
    pub value: String,
    pub hits: u64,
    pub confirmed_at: String,
    pub last_verified_at: String,
    pub passes: u32,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct PendingRow {
    pub upstream_id: Uuid,
    pub upstream_name: String,
    pub upstream_slot: Option<u16>,
    pub upstream_addr: String,
    pub host: String,
    pub port: u16,
    pub confirms: u32,
    pub last_confirm_at: String,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct CandidateRow {
    pub upstream_id: Uuid,
    pub upstream_name: String,
    pub upstream_slot: Option<u16>,
    pub upstream_addr: String,
    pub host: String,
    pub port: u16,
    pub hits: u64,
    pub last_seen: String,
}

/// 出错时的统一形状，与 v3 一致：`{"error":"…"}`（`web/app.js` 各处读 `r.error`）
#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub error: String,
}

/// 四个句柄经请求扩展下发给全部 handler（用 `Extension` 而不是闭包捕获：闭包把
/// `Deps` move 进被调函数后只能调一次，推出来是 `FnOnce`，而 axum 0.8 的 `Handler`
/// 要求 `Fn + Clone`；写成「闭包内再克隆」则要 40 多处样板）。
#[derive(Clone)]
struct Deps {
    prober: Arc<dyn Prober>,
    clash: Arc<dyn Clash>,
    paths: Paths,
    background: bool,
}

/// 把 `AppState` 补齐成后台任务用的 `DaemonCtx`（两者字段同源，`paths` 由模块持有）
pub fn ctx_of(app: &AppState, paths: &Paths) -> DaemonCtx {
    DaemonCtx {
        store: app.store.clone(),
        runtime: app.runtime.clone(),
        bus: app.bus.clone(),
        host: app.host.clone(),
        paths: paths.clone(),
    }
}

/// 本模块的全部路由。P1 的 `api::router` 会把它 merge 进 protected 分支并统一
/// `layer(require_admin)`，所以这里**不加**任何鉴权中间件。
pub fn routes(
    prober: Arc<dyn Prober>,
    clash: Arc<dyn Clash>,
    paths: Paths,
) -> axum::Router<AppState> {
    routes_with(prober, clash, paths, true)
}

/// 同上，但可关掉 handler 里的后台任务（`post_add` / `post_check` 的 `tokio::spawn`）。
/// **测试一律用 `background = false`**：FakeProber 秒回，后台体检会与下一个请求抢
/// `runtime.checking` 与 `state`，断言随调度时序飘。生产即 `routes(..)`。
pub fn routes_with(
    prober: Arc<dyn Prober>,
    clash: Arc<dyn Clash>,
    paths: Paths,
    background: bool,
) -> axum::Router<AppState> {
    let d = Deps {
        prober,
        clash,
        paths,
        background,
    };
    axum::Router::new()
        // ── 规范路径（外加 v3 的 enable：spec §4.3 没有它，原样保留，见 §A）──
        .route("/api/residential/status", get(get_status))
        .route("/api/residential/add", post(post_add))
        .route("/api/residential/remove", post(post_remove))
        .route("/api/residential/enable", post(post_enable))
        .route("/api/residential/global", post(post_global))
        .route("/api/residential/domains", post(post_domains))
        .route(
            "/api/residential/restore-default",
            post(post_restore_default),
        )
        .route("/api/residential/health", get(get_health))
        .route("/api/residential/health/check", post(post_health_check))
        .route("/api/residential/check", post(post_check))
        .route("/api/residential/select", post(post_select))
        .route("/api/residential/priority", post(post_priority))
        .route(
            "/api/residential/blacklist",
            get(get_blacklist).delete(delete_auto),
        )
        .route(
            "/api/residential/blacklist/pins",
            post(post_pin).delete(delete_pin),
        )
        .route("/api/residential/blacklist/apply", post(post_apply))
        // ── 槽位（spec §5.6）──
        .route("/api/residential/slots", get(get_slots))
        .route("/api/residential/slots/pin", post(post_pin_slot))
        .route("/api/residential/rebalance", post(post_rebalance))
        .route("/api/residential/assign", post(post_assign))
        // ── v3 路径别名（契约决策 §A；前端零改动，P5 删兼容层时一并删掉本段）──
        .route(
            "/api/residential",
            get(get_status).post(v3_post).delete(post_enable_off),
        )
        .route("/api/residential/urls", post(post_add))
        .route("/api/residential/urls/{host_port}", delete(delete_url))
        // `Router::layer` 只包住**此刻已注册**的这些路由，`api::router` 后面的 `merge`
        // 与统一 `layer(require_admin)` 都不影响它。
        .layer(axum::Extension(d))
}

type ApiResult = Result<axum::response::Response, (StatusCode, Json<ErrorBody>)>;

fn err(code: StatusCode, msg: impl Into<String>) -> (StatusCode, Json<ErrorBody>) {
    (code, Json(ErrorBody { error: msg.into() }))
}

/// `UpstreamError` → 状态码（与 v3 一致：校验类 400、找不到 404、其余 500）
fn map_upstream_err(e: upstream::UpstreamError) -> (StatusCode, Json<ErrorBody>) {
    use upstream::UpstreamError as E;
    let code = match &e {
        E::Parse(_)
        | E::Unverifiable { .. }
        | E::AuthFailed
        | E::NotProxied(_)
        | E::PoolFull
        | E::PoolEmpty => StatusCode::BAD_REQUEST,
        E::NotFound | E::Unresolvable(_) => StatusCode::NOT_FOUND,
        E::Other(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    err(code, e.to_string())
}

pub fn rule_of(kind: Option<&str>, value: &str) -> Option<Rule> {
    let ok_host = |v: &str| {
        !v.is_empty()
            && v.len() <= 253
            && v.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-'))
    };
    match kind.unwrap_or("domain_suffix") {
        "domain_suffix" => ok_host(value).then(|| Rule::DomainSuffix(value.to_string())),
        "domain" => ok_host(value).then(|| Rule::Domain(value.to_string())),
        "port" => value.parse().ok().map(Rule::Port),
        _ => None,
    }
}

/// `Rule` → `(kind, value)`（响应用）
pub fn rule_parts(r: &Rule) -> (String, String) {
    match r {
        Rule::DomainSuffix(v) => ("domain_suffix".into(), v.clone()),
        Rule::Domain(v) => ("domain".into(), v.clone()),
        Rule::Port(p) => ("port".into(), p.to_string()),
    }
}

/// 用户名打码：前两字符 + `***`（v3 `resiRow` 的 `slice(0,2) + "***"`）
fn mask(u: &str) -> String {
    format!("{}***", u.chars().take(2).collect::<String>())
}

fn kind_str(u: &Upstream) -> String {
    match u.kind {
        UpstreamKind::Http => "http".to_string(),
        UpstreamKind::Socks5 => "socks5".to_string(),
    }
}

/// 一条上游自己的 `auto` 条目数。**`pins` 不计入**：pins 是管理员钉的全局强制直连
/// 规则，跟哪条上游都没关系；把 `pins.len()` 加进来会让面板上每个上游都凭空多出
/// 同样多的条数，运维会以为是这条上游自己学到的。全局计数走 `BlacklistCounts`。
/// `UpstreamRow`（status）与 `MemberRow`（health）都只调这一个函数。
pub fn auto_count(g: &ResidentialGroup, id: Uuid) -> usize {
    g.blacklist
        .auto
        .iter()
        .filter(|a| a.upstream_id == id)
        .count()
}

/// 面板说明文案（spec §5.4「局限」；`blacklist` 与 `status` 共用）
fn notes() -> Vec<String> {
    vec![
        "软封锁（上游返回 200 拦截页）无法自动识别，请手动钉住（pins）".into(),
        "端口类拒绝不进黑名单，由该上游体检学到的端口白名单（ports_allowed）表达".into(),
        "自动条目每日 04:00 批量生效；钉住与「立即应用」即时生效，会重启 b-ui-relay 并掐断住宅连接"
            .into(),
    ]
}

/// `GET /api/residential/health` 的面板说明：把「这些数字是什么时候的」讲清楚，
/// 免得运维以为刷新一次就重新探过了（裁决 D6）
fn health_notes() -> Vec<String> {
    vec![
        "成员状态来自最近一轮健康巡检（后台每 2 分钟一轮）；要立刻探一轮请点「立即巡检」（POST /api/residential/health/check，60 秒内限一次）".into(),
        "出口 IP 与归属来自最近一次体检（添加上游或点「体检」时刷新），不是本次请求现拨".into(),
    ]
}

pub fn status_of(s: &SchemaState, r: &state::ResiRuntime) -> StatusResponse {
    let g = state::group_of(s);
    // keywords = null/空 ⇒ 回生效默认表，并把 domainsFollowDefault 置 true。
    // 面板要能区分「跟随默认」与「自定义」，否则用户一保存就把当时的默认表固化了（R12）。
    let split = SplitRules::from_group(&g);
    let follow_default = g.keywords.as_ref().map(|k| k.is_empty()).unwrap_or(true);
    let selected = g
        .selected_upstream_id
        .and_then(|id| g.upstreams.iter().find(|u| u.id == id))
        .or_else(|| g.upstreams.first());
    let urls = g
        .upstreams
        .iter()
        .map(|u| UrlRow {
            host: u.host.clone(),
            port: u.port,
            username: u.username.clone(),
            name: u.name.clone(),
            kind: kind_str(u),
            last_verified_ip: u
                .verified
                .as_ref()
                .map(|v| v.ip.clone())
                .unwrap_or_default(),
            display_url: format!(
                "{}://{}@{}:{}",
                kind_str(u),
                mask(&u.username),
                u.host,
                u.port
            ),
            id: u.id,
            priority: u.priority,
        })
        .collect();
    let upstreams = g
        .upstreams
        .iter()
        .map(|u| UpstreamRow {
            id: u.id,
            name: u.name.clone(),
            kind: kind_str(u),
            host: u.host.clone(),
            port: u.port,
            username_masked: mask(&u.username),
            priority: u.priority,
            provider: u.provider.clone(),
            region: u.region.clone(),
            ports_allowed: u.ports_allowed.clone(),
            verified: u.verified.clone(),
            // health 与 status 读同一处（runtime.health 以 uuid 为键，契约决策 §C）
            google_ok: r.health.get(&u.id.to_string()).and_then(|h| h.google_ok),
            google_at: r
                .health
                .get(&u.id.to_string())
                .and_then(|h| h.google_at.clone()),
            blacklist_count: auto_count(&g, u.id),
            check: r.checks.get(&u.id.to_string()).cloned(),
            metrics: r
                .health
                .get(&u.id.to_string())
                .map(metrics_of)
                .unwrap_or_default(),
        })
        .collect();
    let v = selected.and_then(|u| u.verified.clone());
    StatusResponse {
        enabled: g.pool_active(),
        global: matches!(g.mode, ResiMode::Global),
        urls,
        domains: split.keywords,
        domains_follow_default: follow_default,
        last_verified_ip: v.as_ref().map(|v| v.ip.clone()).unwrap_or_default(),
        last_verified_isp_info: v
            .as_ref()
            .map(|v| {
                [
                    v.asn.map(|a| format!("AS{a}")),
                    v.org.clone(),
                    v.country.clone(),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(", ")
            })
            .unwrap_or_default(),
        mode: g.mode,
        selected_upstream_id: g.selected_upstream_id,
        active_upstream_id: r.selected_upstream_id,
        // tag 现算：runtime 存的是 uuid，池增删后同一个 resi-N 可能已指向别人（§C）
        active_tag: r.selected_upstream_id.and_then(|id| clash::tag_of(&g, id)),
        selected_pending_persist: r.selected_pending_persist,
        udp_via_residential: g.udp_via_pool(),
        checking: r.checking.clone(),
        blacklist: BlacklistCounts {
            pins: g.blacklist.pins.len(),
            auto: g.blacklist.auto.len(),
            pending: r.pending.len(),
            candidates: r.candidates.len(),
        },
        upstreams,
        notes: notes(),
        alerts: state::visible_alerts(&g, r),
        // 真源是 `runtime.selected_upstream_id`（§C），所以理由也按它算
        selected_reason: r
            .selected_upstream_id
            .map(|id| health::selection_reason(&g, r, id)),
        switch_improve_rounds: r.improve_rounds,
        switch_improve_needed: crate::modules::residential::SWITCH_IMPROVE_ROUNDS,
        switch_improve_candidate: r.improve_candidate_id.and_then(|id| clash::tag_of(&g, id)),
        last_speedtest_at: r.last_speedtest_at.clone(),
        slots: slot_rows(s, r),
    }
}

pub fn blacklist_of(s: &SchemaState, r: &state::ResiRuntime) -> BlacklistResponse {
    let g = state::group_of(s);
    // 名字 / 槽位 / host:port 同一处解析，auto / pending / 候选三段口径一致
    let who = |id: Uuid| {
        let slot = s
            .residential
            .slots
            .iter()
            .find(|sl| sl.upstream_id == id)
            .map(|sl| sl.index);
        match g.upstreams.iter().find(|u| u.id == id) {
            Some(u) => (u.name.clone(), slot, format!("{}:{}", u.host, u.port)),
            None => ("(已移除)".into(), slot, String::new()),
        }
    };
    BlacklistResponse {
        pins: g
            .blacklist
            .pins
            .iter()
            .map(|p| {
                let (kind, value) = rule_parts(&p.rule);
                PinRow {
                    kind,
                    value,
                    note: p.note.clone(),
                    created_at: p.created_at.clone(),
                }
            })
            .collect(),
        auto: g
            .blacklist
            .auto
            .iter()
            .map(|a| {
                let (kind, value) = rule_parts(&a.rule);
                let (upstream_name, upstream_slot, upstream_addr) = who(a.upstream_id);
                AutoRow {
                    upstream_id: a.upstream_id,
                    upstream_name,
                    upstream_slot,
                    upstream_addr,
                    kind,
                    value,
                    hits: a.hits,
                    confirmed_at: a.confirmed_at.clone(),
                    last_verified_at: a.last_verified_at.clone(),
                    passes: a.passes,
                }
            })
            .collect(),
        pending: r
            .pending
            .iter()
            .map(|e| {
                let (upstream_name, upstream_slot, upstream_addr) = who(e.upstream_id);
                PendingRow {
                    upstream_id: e.upstream_id,
                    upstream_name,
                    upstream_slot,
                    upstream_addr,
                    host: e.host.clone(),
                    port: e.port,
                    confirms: e.confirms,
                    last_confirm_at: e.last_confirm_at.clone(),
                }
            })
            .collect(),
        candidates: r
            .candidates
            .values()
            .map(|c| {
                let (upstream_name, upstream_slot, upstream_addr) = who(c.upstream_id);
                CandidateRow {
                    upstream_id: c.upstream_id,
                    upstream_name,
                    upstream_slot,
                    upstream_addr,
                    host: c.host.clone(),
                    port: c.port,
                    hits: c.hits,
                    last_seen: c.last_seen.clone(),
                }
            })
            .collect(),
        checking: r.checking.clone(),
        last_daily_at: r.last_daily_at.clone(),
        notes: notes(),
    }
}

/// 出口画像来自缓存：优先上一次体检报告，退回 `verified`（裁决 D6）
fn egress_of(u: &Upstream, r: &state::ResiRuntime) -> Option<Egress> {
    if let Some(rep) = r.checks.get(&u.id.to_string()) {
        let ip = rep
            .pointer("/exit/ip")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        if ip.is_some() {
            return Some(Egress {
                ip,
                kind: rep
                    .get("class_label")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
                isp: rep
                    .pointer("/exit/org")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                country: rep
                    .pointer("/exit/country")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                city: rep
                    .pointer("/exit/city")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            });
        }
    }
    let v = u.verified.as_ref()?;
    Some(Egress {
        ip: Some(v.ip.clone()),
        // verified 只记归属，没有分类结论 —— 明确写 unknown，不猜
        kind: check::ExitClass::Unknown.label().to_string(),
        isp: v.org.clone(),
        country: v.country.clone(),
        city: None,
    })
}

/// 分流关键字的公共响应体（`domains` / `restore-default` / v3 的 `{reset:true}` 共用）
async fn domains_body(app: &AppState) -> serde_json::Value {
    let g = state::group_of(&*app.store.read().await);
    let split = SplitRules::from_group(&g);
    let follow = g.keywords.as_ref().map(|k| k.is_empty()).unwrap_or(true);
    serde_json::json!({
        "success": true,
        "domains": split.keywords,
        "domainsFollowDefault": follow,
    })
}

async fn get_status(State(app): State<AppState>) -> ApiResult {
    // 纯读 state + runtime，不需要 Deps ⇒ 不写那个提取器
    let s = app.store.read().await;
    // 顺手把 R1 之前的遗留告警与已删上游的孤儿告警清掉（只在有变化时写盘）：
    // 面板一刷新就干净，不必等下一轮巡检
    let r = state::purge_stale_alerts(&app.runtime, &state::group_of(&s)).await;
    Ok(Json(status_of(&s, &r)).into_response())
}

/// `GET /api/residential/slots`（spec §5.6）：按槽列出 IP、当前实际出口、用户与指标。
async fn get_slots(State(app): State<AppState>) -> ApiResult {
    // 纯读 state + runtime，不需要 Deps ⇒ 不写那个提取器（照 `get_status` 的写法）
    let s = app.store.read().await;
    let r = state::read(&app.runtime).await;
    Ok(Json(serde_json::json!({ "slots": slot_rows(&s, &r) })).into_response())
}

/// `POST /api/residential/slots/pin`：把一槽的出口钉在指定上游上（`auto=true` 解除）。
async fn post_pin_slot(
    State(app): State<AppState>,
    Extension(d): Extension<Deps>,
    Json(req): Json<PinSlotRequest>,
) -> ApiResult {
    let ctx = ctx_of(&app, &d.paths);
    let target = match (&req.id, req.auto) {
        (Some(raw), _) => {
            let g = state::group_of(&*app.store.read().await);
            Some(upstream::resolve_upstream(&g, raw).map_err(map_upstream_err)?)
        }
        (None, true) => None,
        (None, false) => {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "要么给 id（钉住），要么给 auto=true（解除）",
            ))
        }
    };
    crate::modules::residential::slots::pin_slot(&ctx, d.clash.clone(), req.index, target)
        .await
        .map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(Json(serde_json::json!({ "success": true })).into_response())
}

/// `POST /api/residential/rebalance`（spec §5.6 规则 3）：把住宅用户在各槽间均匀重排。
async fn post_rebalance(State(app): State<AppState>, Extension(d): Extension<Deps>) -> ApiResult {
    let ctx = ctx_of(&app, &d.paths);
    let moved = crate::modules::residential::slots::rebalance_users(&ctx)
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    // `xray_rules_pending` 提醒前端：槽路由要等下一轮对账（≈1 秒）走 gRPC 收口才生效（D7），
    // 那一下不重启 xray、不掐连接
    Ok(Json(serde_json::json!({
        "success": true,
        "moved": moved,
        "xray_rules_pending": moved > 0
    }))
    .into_response())
}

/// `POST /api/residential/assign`（spec §5.6 规则 3）：把某个用户钉到某一槽。
async fn post_assign(
    State(app): State<AppState>,
    Extension(d): Extension<Deps>,
    Json(req): Json<AssignRequest>,
) -> ApiResult {
    let s = app.store.read().await;
    let user_id = s
        .users
        .iter()
        .find(|u| u.username == req.user)
        .map(|u| u.user_id)
        .ok_or_else(|| err(StatusCode::NOT_FOUND, format!("没有用户 {}", req.user)))?;
    let g = state::group_of(&s);
    // `target` 先按纯槽序号解释（面板传的是 SlotRow.index），再按上游定位串解析
    let slot_id = match req.target.parse::<u16>() {
        Ok(i) => s
            .residential
            .slots
            .iter()
            .find(|x| x.index == i)
            .map(|x| x.upstream_id)
            .ok_or_else(|| err(StatusCode::NOT_FOUND, format!("没有槽 {i}")))?,
        Err(_) => upstream::resolve_upstream(&g, &req.target).map_err(map_upstream_err)?,
    };
    drop(s);
    let ctx = ctx_of(&app, &d.paths);
    if !crate::modules::residential::slots::assign_user(&ctx, user_id, slot_id)
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "该用户没有住宅权益，无法分配槽位",
        ));
    }
    // **不在这里调 converge_xray**：守护进程里只许对账 consumer 那一处调它（D7）。
    // `assign_user` 已经发了 `StateChanged("residential")`，约 1 秒后对账末尾那次调用
    // 会走 gRPC 把这个用户的规则改掉，xray 不重启。
    Ok(Json(serde_json::json!({ "success": true, "xray_rules_pending": true })).into_response())
}

async fn post_add(
    State(app): State<AppState>,
    Extension(d): Extension<Deps>,
    Json(b): Json<AddRequest>,
) -> ApiResult {
    let raw = b.url.trim().to_string();
    if raw.is_empty() {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "请粘贴供应商给的代理，如 socks5://user:pass@host:port 或 host:port:user:pass",
        ));
    }
    let ctx = ctx_of(&app, &d.paths);
    let out = upstream::add(&ctx, d.prober.clone(), &raw)
        .await
        .map_err(map_upstream_err)?;
    // 录入即探测（R13 §7）：端口白名单与支付/AI 可达性在后台补，面板轮询 checking 消失。
    // `background` 为假时不起（测试里 FakeProber 秒回，后台体检会与下一个请求抢 runtime/state）
    if d.background {
        let (c2, p2, id) = (ctx.clone(), d.prober.clone(), out.id);
        tokio::spawn(async move {
            if let Err(e) = check::run_and_store(&c2, p2, id).await {
                tracing::warn!(error = %e, "新增上游后的体检失败");
            }
        });
    }
    // 响应字段照 v3 的 add：success / exitIp / ispInfo / type
    Ok(Json(serde_json::json!({
        "success": true,
        "exitIp": out.exit_ip,
        "ispInfo": out.isp,
        "type": match out.kind { UpstreamKind::Http => "http", UpstreamKind::Socks5 => "socks5" },
        "id": out.id,
        "name": out.name,
        "class": out.class_label,
    }))
    .into_response())
}

async fn post_remove(
    State(app): State<AppState>,
    Extension(d): Extension<Deps>,
    Json(b): Json<RemoveRequest>,
) -> ApiResult {
    let g = state::group_of(&*app.store.read().await);
    let sel = match (b.id.as_deref(), b.host_port.as_deref()) {
        // `id` 认 uuid / resi-N / url-N / host:port（CLI 的 remove 只送这一个字段）
        (Some(raw), _) => upstream::UpstreamSel::Id(
            upstream::resolve_upstream(&g, raw).map_err(map_upstream_err)?,
        ),
        (None, Some(hp)) => upstream::UpstreamSel::parse_host_port(hp)
            .ok_or_else(|| err(StatusCode::BAD_REQUEST, "host_port 必须是 host:port"))?,
        (None, None) => return Err(err(StatusCode::BAD_REQUEST, "id 或 host_port 字段必填")),
    };
    let ctx = ctx_of(&app, &d.paths);
    upstream::remove(&ctx, &sel)
        .await
        .map_err(map_upstream_err)?;
    Ok(Json(serde_json::json!({ "success": true })).into_response())
}

/// v3 别名 `DELETE /api/residential/urls/<host:port>`（路径段是 URL 编码的 `host:port`）
async fn delete_url(
    State(app): State<AppState>,
    Extension(d): Extension<Deps>,
    Path(host_port): Path<String>,
) -> ApiResult {
    let sel = upstream::UpstreamSel::parse_host_port(&host_port)
        .ok_or_else(|| err(StatusCode::BAD_REQUEST, "路径必须是 host:port"))?;
    let ctx = ctx_of(&app, &d.paths);
    upstream::remove(&ctx, &sel)
        .await
        .map_err(map_upstream_err)?;
    Ok(Json(serde_json::json!({ "success": true })).into_response())
}

/// v3 的空体启用请求（`web/app.js:37-50` 不带 `Content-Type`）：提取器必须是
/// `Option<Json<..>>`，否则 axum 0.8 直接回 415
async fn post_enable(
    State(app): State<AppState>,
    Extension(d): Extension<Deps>,
    body: Option<Json<EnableRequest>>,
) -> ApiResult {
    let enabled = body.map(|Json(b)| b.enabled).unwrap_or(true);
    let ctx = ctx_of(&app, &d.paths);
    upstream::set_enabled(&ctx, enabled)
        .await
        .map_err(map_upstream_err)?;
    Ok(Json(serde_json::json!({ "success": true, "enabled": enabled })).into_response())
}

async fn post_enable_off(State(app): State<AppState>, Extension(d): Extension<Deps>) -> ApiResult {
    // v3 的「禁用」是 DELETE /api/residential（无 body）
    let ctx = ctx_of(&app, &d.paths);
    upstream::set_enabled(&ctx, false)
        .await
        .map_err(map_upstream_err)?;
    Ok(Json(serde_json::json!({ "success": true, "enabled": false })).into_response())
}

async fn post_global(
    State(app): State<AppState>,
    Extension(d): Extension<Deps>,
    Json(b): Json<GlobalRequest>,
) -> ApiResult {
    let ctx = ctx_of(&app, &d.paths);
    upstream::set_mode(&ctx, b.global)
        .await
        .map_err(map_upstream_err)?;
    Ok(Json(serde_json::json!({ "success": true, "global": b.global })).into_response())
}

async fn post_domains(
    State(app): State<AppState>,
    Extension(d): Extension<Deps>,
    Json(b): Json<DomainsRequest>,
) -> ApiResult {
    let ctx = ctx_of(&app, &d.paths);
    // reset 或 domains = null/空 ⇒ 回到跟随默认表（`set_keywords` 自己也做这层归一）
    let kw = if b.reset { None } else { b.domains };
    upstream::set_keywords(&ctx, kw)
        .await
        .map_err(map_upstream_err)?;
    Ok(Json(domains_body(&app).await).into_response())
}

async fn post_restore_default(
    State(app): State<AppState>,
    Extension(d): Extension<Deps>,
) -> ApiResult {
    let ctx = ctx_of(&app, &d.paths);
    upstream::set_keywords(&ctx, None)
        .await
        .map_err(map_upstream_err)?;
    Ok(Json(domains_body(&app).await).into_response())
}

async fn get_health(State(app): State<AppState>) -> ApiResult {
    // **只读**：不碰 Prober / Clash，不推进迟滞，不切换（裁决 D6/D10）
    let s = app.store.read().await;
    let g = state::group_of(&s);
    let r = state::purge_stale_alerts(&app.runtime, &g).await;
    // 槽位表与 `status` 同一个投影函数：两处显示的数字永远一致（spec §5.6）
    let slots = slot_rows(&s, &r);
    drop(s);
    let split = SplitRules::from_group(&g);
    let now = app.host.now();
    // active tag 现算：runtime 存 uuid（§C）
    let active_tag = r.selected_upstream_id.and_then(|id| clash::tag_of(&g, id));
    let mut resp = HealthResponse {
        enabled: g.pool_active(),
        urls: vec![],
        domains_count: split.keywords.len(),
        mode: if g.pool_active() {
            "selector".into()
        } else {
            "none".into()
        },
        selected: active_tag.clone(),
        members: vec![],
        current_egress_ip_test: None,
        egress_ip_type: "unknown".into(),
        via_proxy_isp: None,
        udp_via_residential: g.udp_via_pool(),
        alerts: state::visible_alerts(&g, &r),
        last_daily_at: r.last_daily_at.clone(),
        notes: health_notes(),
        // 理由按 runtime 记的真源算（§C）；tag 现算
        selected_reason: r
            .selected_upstream_id
            .map(|id| health::selection_reason(&g, &r, id)),
        switch_improve_rounds: r.improve_rounds,
        switch_improve_needed: crate::modules::residential::SWITCH_IMPROVE_ROUNDS,
        switch_improve_candidate: r.improve_candidate_id.and_then(|id| clash::tag_of(&g, id)),
        last_speedtest_at: r.last_speedtest_at.clone(),
        slots,
    };
    if !g.pool_active() {
        return Ok(Json(resp).into_response());
    }
    resp.selected = active_tag.or_else(|| clash::tags(&g).first().cloned());
    for (i, tag) in clash::tags(&g).iter().enumerate() {
        let u = &g.upstreams[i];
        // runtime.health 以 uuid 的字符串形式为键（契约决策 §C），不是 tag
        let h = r.health.get(&u.id.to_string()).cloned().unwrap_or_default();
        resp.members.push(MemberRow {
            tag: tag.clone(),
            kind: kind_str(u),
            host: u.host.clone(),
            port: u.port,
            active: h.active,
            failstreak: h.failstreak,
            okstreak: h.okstreak,
            egress: egress_of(u, &r),
            priority: u.priority,
            success_rate_24h: state::success_rate_24h(&h, now),
            // 与 status_of 的 UpstreamRow 同一个函数：面板上是同一个数字
            blacklist_count: auto_count(&g, u.id),
            probe_ok: h.samples.last().map(|x| x.ok),
            manual_locked: r.manual_selected_id == Some(u.id),
            google_ok: h.google_ok,
            google_at: h.google_at.clone(),
            upstream_id: u.id,
            // 与 status_of 的 UpstreamRow 同一个函数：面板两处是同一个数
            metrics: metrics_of(&h),
        });
        resp.urls.push(HealthUrlRow {
            host: u.host.clone(),
            port: u.port,
            last_verified_at: u.verified.as_ref().map(|v| v.at.clone()),
            last_verified_ip: u.verified.as_ref().map(|v| v.ip.clone()),
            last_verified_isp: u.verified.as_ref().and_then(|v| v.org.clone()),
        });
    }
    // 顶层旧字段由「当前生效成员」派生（v3 同语义，面板旧渲染继续可用）
    let cur = resp
        .members
        .iter()
        .find(|m| Some(&m.tag) == resp.selected.as_ref())
        .or_else(|| resp.members.first())
        .and_then(|m| m.egress.clone());
    if let Some(e) = cur {
        resp.current_egress_ip_test = e.ip.clone();
        resp.egress_ip_type = e.kind.clone();
        let place = match (&e.city, &e.country) {
            (Some(c), Some(n)) => format!(" ({c}, {n})"),
            (None, Some(n)) => format!(" ({n})"),
            _ => String::new(),
        };
        let isp = format!("{}{}", e.isp.clone().unwrap_or_default(), place)
            .trim()
            .to_string();
        resp.via_proxy_isp = (!isp.is_empty()).then_some(isp);
    }
    Ok(Json(resp).into_response())
}

/// 面板「立即巡检一轮」（裁决 D10）：跑一次 `health::check_once`（会推进迟滞、可能切换），
/// 所以按 `MANUAL_ROUND_MIN_GAP_SECS` 限速，超频回 429。
async fn post_health_check(
    State(app): State<AppState>,
    Extension(d): Extension<Deps>,
) -> ApiResult {
    let now = app.host.now();
    let r = state::read(&app.runtime).await;
    if let Some(t) = r
        .last_manual_round_at
        .as_deref()
        .and_then(crate::util::parse_rfc3339)
    {
        let gap = (now - t).whole_seconds();
        // 时钟回跳（NTP 校时）不该把按钮永久锁死，所以只在 [0, 上限) 区间内拦
        if (0..MANUAL_ROUND_MIN_GAP_SECS).contains(&gap) {
            return Err(err(
                StatusCode::TOO_MANY_REQUESTS,
                format!(
                    "巡检限速中，请 {} 秒后再试",
                    MANUAL_ROUND_MIN_GAP_SECS - gap
                ),
            ));
        }
    }
    let stamp = crate::util::fmt_rfc3339(now);
    state::update(&app.runtime, move |r| r.last_manual_round_at = Some(stamp)).await;
    let ctx = ctx_of(&app, &d.paths);
    let out = health::check_once(&ctx, d.prober.clone(), d.clash.clone())
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({
        "healthy": out.healthy,
        "probed": out.probed,
        "switched_to": out.switched_to,
        // 重放与切换是两件事：面板要能区分「relay 刚重启过，已把你选的出口放回去」
        // 与「你选的出口坏了，已自动切走」
        "replayed_to": out.replayed_to,
        "notes": out.notes,
    }))
    .into_response())
}

async fn post_check(
    State(app): State<AppState>,
    Extension(d): Extension<Deps>,
    Json(b): Json<CheckRequest>,
) -> ApiResult {
    let g = state::group_of(&*app.store.read().await);
    let id = match b.id.as_deref() {
        // resolve_upstream 已经保证 id 在池里（定位不到就是 404），不必再查一遍
        Some(raw) => upstream::resolve_upstream(&g, raw).map_err(map_upstream_err)?,
        None => g
            .selected_upstream_id
            .or_else(|| g.upstreams.first().map(|u| u.id))
            .ok_or_else(|| err(StatusCode::BAD_REQUEST, "代理节点池为空"))?,
    };
    let r = state::read(&app.runtime).await;
    if let Some(c) = r.checking {
        // 超过 10 分钟视为过期（R13 §4），否则一次卡住的体检会永久挡住按钮
        let stale = crate::util::parse_rfc3339(&c.started_at)
            .map(|t| (app.host.now() - t).whole_seconds() > 600)
            .unwrap_or(true);
        if !stale {
            return Err(err(StatusCode::CONFLICT, "已有体检在进行中，请稍候"));
        }
    }
    if d.background {
        let ctx = ctx_of(&app, &d.paths);
        let p = d.prober.clone();
        tokio::spawn(async move {
            if let Err(e) = check::run_and_store(&ctx, p, id).await {
                tracing::warn!(error = %e, "体检失败");
            }
        });
    }
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "started": true, "upstream_id": id })),
    )
        .into_response())
}

/// `POST /api/residential/select`（手动切当前出口，只走 Clash API，不写 state）。
/// `{"auto": true}` = 解除手动锁定，回到自动选路（R2 ①）。
async fn post_select(
    State(app): State<AppState>,
    Extension(d): Extension<Deps>,
    Json(b): Json<SelectRequest>,
) -> ApiResult {
    let ctx = ctx_of(&app, &d.paths);
    if b.auto {
        health::select_auto(&ctx).await;
        return Ok(Json(serde_json::json!({ "success": true, "auto": true })).into_response());
    }
    // 空载荷不当成解锁：那是笔误，解锁得明确写 `{"auto": true}`
    let raw =
        b.id.ok_or_else(|| err(StatusCode::BAD_REQUEST, "id 或 auto 字段必填"))?;
    // 定位与 404 都归 resolve_upstream：它认 uuid / resi-N / url-N / host:port，
    // 并且只返回池内的 id（select_manual 只会返回 anyhow::Error，没有类型可匹配）
    let g = state::group_of(&*app.store.read().await);
    let id = upstream::resolve_upstream(&g, &raw).map_err(map_upstream_err)?;
    let tag = health::select_manual(&ctx, d.clash.clone(), id)
        .await
        // 走到这里只剩「Clash API 调用失败」一种可能（池内判据已预检过）
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({ "success": true, "tag": tag, "manual": true })).into_response())
}

async fn post_priority(
    State(app): State<AppState>,
    Extension(d): Extension<Deps>,
    Json(b): Json<PriorityRequest>,
) -> ApiResult {
    let ctx = ctx_of(&app, &d.paths);
    upstream::set_priority(&ctx, b.id, b.priority)
        .await
        .map_err(map_upstream_err)?;
    Ok(
        Json(serde_json::json!({ "success": true, "id": b.id, "priority": b.priority }))
            .into_response(),
    )
}

async fn get_blacklist(State(app): State<AppState>) -> ApiResult {
    let s = app.store.read().await;
    let r = state::read(&app.runtime).await;
    Ok(Json(blacklist_of(&s, &r)).into_response())
}

/// `POST /api/residential/blacklist/pins`（钉住，立即生效 ⇒ 会重启 relay）
async fn post_pin(
    State(app): State<AppState>,
    Extension(d): Extension<Deps>,
    Json(b): Json<PinRequest>,
) -> ApiResult {
    let rule = rule_of(b.kind.as_deref(), &b.value)
        .ok_or_else(|| err(StatusCode::BAD_REQUEST, "规则类型或取值非法"))?;
    let ctx = ctx_of(&app, &d.paths);
    blacklist::add_pin(&ctx, rule, b.note)
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({ "success": true })).into_response())
}

/// `DELETE /api/residential/blacklist/pins`（取消钉住，立即生效 ⇒ 会重启 relay）
async fn delete_pin(
    State(app): State<AppState>,
    Extension(d): Extension<Deps>,
    Json(b): Json<PinRequest>,
) -> ApiResult {
    let rule = rule_of(b.kind.as_deref(), &b.value)
        .ok_or_else(|| err(StatusCode::BAD_REQUEST, "规则类型或取值非法"))?;
    let ctx = ctx_of(&app, &d.paths);
    let removed = blacklist::remove_pin(&ctx, &rule)
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    // `Ok(false)` = 本来就没这条 ⇒ 404，别回 200 让面板以为删掉了
    if !removed {
        return Err(err(StatusCode::NOT_FOUND, "未找到该钉住规则"));
    }
    Ok(Json(serde_json::json!({ "success": true })).into_response())
}

/// `DELETE /api/residential/blacklist`（删一条 auto，裁决 D8；立即生效 ⇒ 会重启 relay）
async fn delete_auto(
    State(app): State<AppState>,
    Extension(d): Extension<Deps>,
    Json(b): Json<ForgetAutoRequest>,
) -> ApiResult {
    let rule = rule_of(b.kind.as_deref(), &b.value)
        .ok_or_else(|| err(StatusCode::BAD_REQUEST, "规则类型或取值非法"))?;
    let ctx = ctx_of(&app, &d.paths);
    let removed = blacklist::remove_auto(&ctx, b.upstream_id, &rule)
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if !removed {
        return Err(err(StatusCode::NOT_FOUND, "未找到该自动黑名单条目"));
    }
    Ok(Json(serde_json::json!({ "success": true })).into_response())
}

/// `POST /api/residential/blacklist/apply`（把 `runtime.pending` 全量写进 state）
async fn post_apply(State(app): State<AppState>, Extension(d): Extension<Deps>) -> ApiResult {
    let ctx = ctx_of(&app, &d.paths);
    let n = blacklist::apply_now(&ctx)
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({ "success": true, "applied": n })).into_response())
}

/// v3 的 `POST /api/residential`：一条路径三种语义，按体分派（契约决策 §A）
async fn v3_post(
    State(app): State<AppState>,
    Extension(d): Extension<Deps>,
    Json(b): Json<V3PostRequest>,
) -> ApiResult {
    if let Some(url) = b.url {
        return post_add(State(app), Extension(d), Json(AddRequest { url })).await;
    }
    if b.reset || b.domains.as_ref().map(|x| x.is_empty()).unwrap_or(false) {
        return post_restore_default(State(app), Extension(d)).await;
    }
    match b.domains {
        Some(domains) => {
            post_domains(
                State(app),
                Extension(d),
                Json(DomainsRequest {
                    domains: Some(domains),
                    reset: false,
                }),
            )
            .await
        }
        None => Err(err(StatusCode::BAD_REQUEST, "url 或 domains 字段必填")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{AppState, EventBus};
    use crate::modules::residential::clash::FakeClash;
    use crate::modules::residential::proxy::{FakeProber, HttpProbe};
    use crate::modules::residential::state as rstate;
    use crate::modules::residential::POOL;
    use crate::state::runtime::Runtime;
    use crate::state::store::Store;
    use crate::sys::fake::FakeHost;
    use crate::sys::Host;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use pretty_assertions::assert_eq;
    use std::sync::Arc;
    use tower::ServiceExt;

    struct Harness {
        app: axum::Router,
        ctx: DaemonCtx,
        /// 假时钟的句柄（`ctx.host` 是 `Arc<dyn Host>`，没有 `advance`）
        host: Arc<FakeHost>,
        prober: Arc<FakeProber>,
        clash: Arc<FakeClash>,
    }

    async fn harness(d: &tempfile::TempDir) -> Harness {
        // **必须**用本模块的夹具：P1 的 crate::testutil::sample_state() 住宅段是空池
        // （enabled:false / upstreams:[] / pins:[] / auto:[]），下面每一条断言都会崩
        let s = crate::modules::residential::sample_state_with_pool();
        let host = Arc::new(FakeHost::new());
        let store = Store::create(d.path().join("state.json"), s).await.unwrap();
        let runtime = Runtime::load(d.path().join("runtime.json"));
        let bus = EventBus::new();
        let paths = bui_schema::paths::Paths::default_server();
        let app_state = AppState {
            store: store.clone(),
            bus: bus.clone(),
            runtime: runtime.clone(),
            host: host.clone(),
            started_at: host.now(),
            version: "4.0.0",
            login: Default::default(),
        };
        let prober = Arc::new(FakeProber::new());
        prober.with(|i| {
            i.gets.insert(
                crate::modules::residential::EXIT_IP_URL.into(),
                Ok(HttpProbe {
                    status: 200,
                    body: "198.51.100.7".into(),
                }),
            );
            i.gets.insert(
                crate::modules::residential::check::EXIT_SOURCES[0].1.into(),
                Ok(HttpProbe {
                    status: 200,
                    body: serde_json::json!({"ip":"198.51.100.7","asn":33667,
                        "asOrganization":"Comcast","country":"US","isResidential":true})
                    .to_string(),
                }),
            );
            // `POST /api/residential/health/check` 会真探一轮（裁决 D10），这条不给就探不通
            i.gets.insert(
                crate::modules::residential::HEALTH_PROBE_URL.into(),
                Ok(HttpProbe {
                    status: 204,
                    body: String::new(),
                }),
            );
        });
        // 出口画像读缓存（裁决 D6）：预置一份体检报告，否则 egress 只能退回 verified 的 unknown
        let up_id = rstate::group_of(&*store.read().await).upstreams[0].id;
        rstate::update(&runtime, |r| {
            // runtime 的「当前生效」记 uuid（契约决策 §C）
            r.selected_upstream_id = Some(up_id);
            r.checks.insert(
                up_id.to_string(),
                serde_json::json!({
                    "class_label": "家庭宽带 IP",
                    "exit": {"ip": "198.51.100.7", "org": "AS33667 Comcast", "country": "US", "city": "Denver"}
                }),
            );
        })
        .await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        // 直接用本模块的 routes_with()：P1 的 require_admin 在 api::router 里统一套，
        // 这里单测 handler 本身，不再套一层鉴权（鉴权由 P1 Task 13 的测试覆盖）。
        // `background = false`：FakeProber 秒回，后台体检会与下一个请求抢 runtime.checking，
        // 不关掉 `check_is_202_and_409…` 会随调度时序飘
        let app =
            routes_with(prober.clone(), clash.clone(), paths.clone(), false).with_state(app_state);
        Harness {
            app,
            ctx: DaemonCtx {
                store,
                runtime,
                bus,
                host: host.clone(),
                paths,
            },
            host,
            prober,
            clash,
        }
    }

    async fn call(
        app: &axum::Router,
        method: &str,
        uri: &str,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, serde_json::Value) {
        let req = Request::builder().method(method).uri(uri);
        let req = match body {
            Some(v) => req
                .header("content-type", "application/json")
                .body(Body::from(v.to_string())),
            None => req.body(Body::empty()),
        }
        .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let v = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, v)
    }

    #[tokio::test]
    async fn status_keeps_every_v3_field_name_the_panel_reads() {
        let d = tempfile::tempdir().unwrap();
        let h = harness(&d).await;
        let (st, v) = call(&h.app, "GET", "/api/residential/status", None).await;
        assert_eq!(st, StatusCode::OK);
        // web/app.js:827-930 读的全部字段
        assert_eq!(v["enabled"], true);
        assert_eq!(v["global"], true);
        assert_eq!(v["urls"][0]["host"], "isp.example.net");
        assert_eq!(v["urls"][0]["port"], 10007);
        assert_eq!(v["urls"][0]["name"], "url-1");
        assert_eq!(v["urls"][0]["type"], "http");
        assert_eq!(v["urls"][0]["username"], "u");
        assert_eq!(v["urls"][0]["lastVerifiedIp"], "198.51.100.7");
        assert_eq!(
            v["urls"][0]["displayUrl"],
            "http://u***@isp.example.net:10007"
        );
        assert!(
            v["domains"].as_array().unwrap().len() >= 67,
            "keywords=null ⇒ 回生效默认表"
        );
        assert_eq!(v["domainsFollowDefault"], true);
        assert_eq!(
            v["lastVerifiedIp"], "198.51.100.7",
            "顶层旧字段由当前落点派生"
        );
        // 密码绝不外泄
        assert!(
            !serde_json::to_string(&v).unwrap().contains("\"p\""),
            "响应里不能出现密码"
        );
        // v4 追加项
        assert_eq!(v["mode"], "global");
        let id = rstate::group_of(&*h.ctx.store.read().await).upstreams[0].id;
        assert_eq!(
            v["selected_upstream_id"],
            id.to_string(),
            "state 的配置落点"
        );
        assert_eq!(
            v["active_upstream_id"],
            id.to_string(),
            "runtime 的当前生效（uuid，§C）"
        );
        assert_eq!(
            v["active_tag"], "resi-1",
            "tag 是现算出来给面板看的，不是主键"
        );
        assert_eq!(v["selected_pending_persist"], false);
        assert_eq!(v["blacklist"]["pins"], 1);
        // blacklist_count 只数这条上游自己的 auto（夹具里恰好 1 条）。夹具同时有 1 条
        // 全局 pin，若实现里手滑加上 pins.len() 这里就会是 2 —— 这条断言专治那个手滑。
        assert_eq!(
            v["upstreams"][0]["blacklist_count"], 1,
            "pins 是全局规则，不计入单个上游：{v}"
        );
        assert_eq!(v["upstreams"][0]["username_masked"], "u***");
        assert!(v["upstreams"][0].get("password").is_none());
        // v3 别名同结果
        let (st2, v2) = call(&h.app, "GET", "/api/residential", None).await;
        assert_eq!((st2, v2), (StatusCode::OK, v));
    }

    #[tokio::test]
    async fn health_keeps_every_v3_field_and_the_chinese_egress_type() {
        let d = tempfile::tempdir().unwrap();
        let h = harness(&d).await;
        let (st, v) = call(&h.app, "GET", "/api/residential/health", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["enabled"], true);
        assert_eq!(v["mode"], "selector");
        assert_eq!(v["selected"], "resi-1");
        assert!(v["domains_count"].as_u64().unwrap() >= 67);
        assert_eq!(v["members"][0]["tag"], "resi-1");
        assert_eq!(v["members"][0]["type"], "http");
        assert_eq!(v["members"][0]["host"], "isp.example.net");
        assert_eq!(v["members"][0]["active"], true);
        assert_eq!(v["members"][0]["failstreak"], 0);
        // GET 只读（裁决 D6）：没探过就是 0，绝不因为「刷新了一下面板」而推进迟滞
        assert_eq!(v["members"][0]["okstreak"], 0);
        assert_eq!(
            v["members"][0]["probe_ok"],
            serde_json::Value::Null,
            "还没探过"
        );
        assert!(h.prober.calls().is_empty(), "GET /health 一次探测都不该发");
        assert!(
            v["notes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n.as_str().unwrap().contains("立即巡检")),
            "面板要知道这些数字是什么时候的：{v}"
        );
        // egress.type 必须是中文串：app.js:1095 用 /IDC|机房/i 判色
        assert_eq!(v["members"][0]["egress"]["ip"], "198.51.100.7");
        assert_eq!(v["egress_ip_type"], "家庭宽带 IP");
        assert_eq!(v["current_egress_ip_test"], "198.51.100.7");
        assert!(v["via_proxy_isp"].as_str().unwrap().contains("Comcast"));
        // v4 追加。与 status 的 upstreams[0].blacklist_count 同源（都走 auto_count）：
        // 夹具里这条上游有 1 条 auto、全局有 1 条 pin，pins 不计入 ⇒ 1 而不是 2
        assert_eq!(v["members"][0]["blacklist_count"], 1);
        let (_, sv) = call(&h.app, "GET", "/api/residential/status", None).await;
        assert_eq!(
            v["members"][0]["blacklist_count"], sv["upstreams"][0]["blacklist_count"],
            "status 与 health 的同名字段必须永远相等（同一个 auto_count）"
        );
        assert!(v["members"][0]["priority"].is_number());
    }

    #[tokio::test]
    async fn health_and_status_expose_latency_speed_udp_and_why_this_exit_is_selected() {
        // 主理人 2026-09-12：巡检要给出延迟 p50/p95、上下行 Mbps、最近测速时间、
        // 是否满足切换条件，并说清「当前选中 resi-N 的原因」
        let d = tempfile::tempdir().unwrap();
        let h = harness(&d).await;
        let id = rstate::group_of(&*h.ctx.store.read().await).upstreams[0].id;
        rstate::update(&h.ctx.runtime, |r| {
            let x = r.health.entry(id.to_string()).or_default();
            for ms in [80u64, 100, 300] {
                rstate::record_latency(x, Some(ms), Some(ms / 3), Some(ms / 2));
            }
            rstate::record_speed(
                x,
                Some(88.5),
                Some(12.25),
                None,
                time::macros::datetime!(2026-09-12 00:00:00 UTC),
            );
            rstate::record_udp(
                x,
                &crate::modules::residential::proxy::UdpProbe {
                    ok: true,
                    exit_ip: Some("198.51.100.9".into()),
                    ms: Some(40),
                    note: None,
                },
                time::macros::datetime!(2026-09-12 00:00:00 UTC),
            );
            r.improve_rounds = 2;
            r.improve_candidate_id = Some(id);
        })
        .await;
        let (_, v) = call(&h.app, "GET", "/api/residential/health", None).await;
        let m = &v["members"][0];
        assert_eq!(m["latency_p50_ms"], 100, "80/100/300 的 p50");
        assert_eq!(m["latency_p95_ms"], 300);
        assert_eq!(m["tcp_p50_ms"], 33);
        assert_eq!(m["down_mbps"], 88.5);
        assert_eq!(m["up_mbps"], 12.25);
        assert_eq!(m["speed_at"], "2026-09-12T00:00:00Z");
        assert_eq!(m["udp_ok"], true);
        assert_eq!(m["udp_exit_ip"], "198.51.100.9");
        // udp_ms 样本是 [40, 50, 150, 40]（三轮 record_latency + 一次 record_udp）⇒ p50 = 40
        assert_eq!(m["udp_p50_ms"], 40, "UDP 耗时来自 STUN 往返，单独一列");
        // 防抖进度：面板要能看出「还差几轮才会切」
        assert_eq!(v["switch_improve_rounds"], 2);
        assert_eq!(
            v["switch_improve_needed"],
            crate::modules::residential::SWITCH_IMPROVE_ROUNDS
        );
        assert!(
            v["selected_reason"].as_str().unwrap().contains("唯一"),
            "夹具只有一条上游：{v}"
        );
        // status 的同名字段读同一处 runtime.health，必须一字不差
        let (_, sv) = call(&h.app, "GET", "/api/residential/status", None).await;
        let u = &sv["upstreams"][0];
        for k in [
            "latency_p50_ms",
            "latency_p95_ms",
            "down_mbps",
            "up_mbps",
            "udp_ok",
            "udp_exit_ip",
        ] {
            assert_eq!(u[k], m[k], "status 与 health 的 {k} 必须相等");
        }
        assert_eq!(sv["selected_reason"], v["selected_reason"]);
        // 防抖进度与上次测速时间在 status 上也要有（口径：health / status / API 都给）
        assert_eq!(sv["switch_improve_rounds"], v["switch_improve_rounds"]);
        assert_eq!(sv["switch_improve_needed"], v["switch_improve_needed"]);
        assert_eq!(sv["last_speedtest_at"], v["last_speedtest_at"]);
        // CLI 的渲染吃同一份 JSON：延迟 / 速度 / UDP / 选路原因都要出现在人读的那几行里
        let text = crate::modules::residential::cli::format_health(&v);
        assert!(text.contains("延迟 p50 100"), "{text}");
        assert!(text.contains("p95 300"), "{text}");
        assert!(text.contains("88.5"), "{text}");
        assert!(text.contains("UDP 通"), "{text}");
        assert!(text.contains("198.51.100.9"), "{text}");
        assert!(text.contains("选路原因"), "{text}");
        assert!(text.contains("2/3"), "防抖进度也要看得到：{text}");
        let stext = crate::modules::residential::cli::format_status(&sv);
        assert!(stext.contains("选路原因"), "{stext}");
        assert!(stext.contains("2/3"), "{stext}");
        assert!(stext.contains("88.5"), "{stext}");
    }

    #[tokio::test]
    async fn an_unmeasured_member_renders_as_unknown_not_as_zero() {
        // 「没测过」与「0 毫秒 / 0 Mbps」必须区分开：前者不该在面板上显示成最优
        let d = tempfile::tempdir().unwrap();
        let h = harness(&d).await;
        let (_, v) = call(&h.app, "GET", "/api/residential/health", None).await;
        let m = &v["members"][0];
        for k in [
            "latency_p50_ms",
            "latency_p95_ms",
            "tcp_p50_ms",
            "udp_p50_ms",
            "down_mbps",
            "up_mbps",
            "speed_at",
            "udp_ok",
            "udp_exit_ip",
        ] {
            assert_eq!(m[k], serde_json::Value::Null, "{k} 没测过就该是 null");
        }
        let text = crate::modules::residential::cli::format_health(&v);
        assert!(text.contains("延迟 p50 - / p95 - ms"), "{text}");
        assert!(text.contains("↓- / ↑- Mbps"), "{text}");
        assert!(text.contains("UDP 未知"), "{text}");
        assert!(text.contains("没有明显更优的候选"), "{text}");
    }

    #[tokio::test]
    async fn the_manual_round_endpoint_probes_once_and_is_rate_limited() {
        let d = tempfile::tempdir().unwrap();
        let h = harness(&d).await;
        let (st, v) = call(&h.app, "POST", "/api/residential/health/check", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["healthy"][0], "resi-1");
        assert!(
            h.prober.calls().iter().any(|c| c.starts_with("get:")),
            "这条端点才真的探：{:?}",
            h.prober.calls()
        );
        // 迟滞被推进了（这正是它不能挂在 GET 上的原因）
        let (_, hv) = call(&h.app, "GET", "/api/residential/health", None).await;
        assert_eq!(hv["members"][0]["okstreak"], 1);
        // 60 秒内再点一次 → 429
        let (st2, v2) = call(&h.app, "POST", "/api/residential/health/check", None).await;
        assert_eq!(st2, StatusCode::TOO_MANY_REQUESTS);
        assert!(v2["error"].as_str().unwrap().contains("限速"), "{v2}");
        // 过了限速窗口就放行
        h.host.advance(61);
        let (st3, _) = call(&h.app, "POST", "/api/residential/health/check", None).await;
        assert_eq!(st3, StatusCode::OK);
    }

    /// UDP 能不能经住宅出口要在 status / health 上看得见：夹具是 http 上游 ⇒ 否；
    /// 全池换成 socks5 ⇒ 是（relay 渲染同一判据）。
    #[tokio::test]
    async fn status_and_health_report_whether_udp_goes_through_the_pool() {
        let d = tempfile::tempdir().unwrap();
        let h = harness(&d).await;
        let (_, v) = call(&h.app, "GET", "/api/residential/status", None).await;
        assert_eq!(v["udp_via_residential"], false, "池内有 http 上游 ⇒ 否");
        let (_, v) = call(&h.app, "GET", "/api/residential/health", None).await;
        assert_eq!(v["udp_via_residential"], false);

        rstate::update_group(&h.ctx.store, &h.ctx.bus, |g| {
            for u in &mut g.upstreams {
                u.kind = bui_schema::model::UpstreamKind::Socks5;
            }
        })
        .await
        .unwrap();
        let (_, v) = call(&h.app, "GET", "/api/residential/status", None).await;
        assert_eq!(v["udp_via_residential"], true, "全 socks5 池 ⇒ 是");
        let (_, v) = call(&h.app, "GET", "/api/residential/health", None).await;
        assert_eq!(v["udp_via_residential"], true);

        // 池关掉时 UDP 根本不经住宅
        rstate::update_group(&h.ctx.store, &h.ctx.bus, |g| g.enabled = false)
            .await
            .unwrap();
        let (_, v) = call(&h.app, "GET", "/api/residential/status", None).await;
        assert_eq!(v["udp_via_residential"], false);
        let (_, v) = call(&h.app, "GET", "/api/residential/health", None).await;
        assert_eq!(v["udp_via_residential"], false);
    }

    #[tokio::test]
    async fn health_on_a_disabled_pool_returns_the_v3_shape() {
        let d = tempfile::tempdir().unwrap();
        let h = harness(&d).await;
        rstate::update_group(&h.ctx.store, &h.ctx.bus, |g| g.enabled = false)
            .await
            .unwrap();
        let (st, v) = call(&h.app, "GET", "/api/residential/health", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["enabled"], false);
        assert_eq!(v["mode"], "none");
        assert_eq!(v["members"].as_array().unwrap().len(), 0);
        assert_eq!(v["egress_ip_type"], "unknown");
    }

    #[tokio::test]
    async fn add_and_remove_work_on_both_the_canonical_and_the_v3_paths() {
        let d = tempfile::tempdir().unwrap();
        let h = harness(&d).await;
        // 规范路径
        let (st, v) = call(
            &h.app,
            "POST",
            "/api/residential/add",
            Some(serde_json::json!({"url": "http://user1:pw1@isp2.example.net:10007"})),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["success"], true);
        assert_eq!(
            v["type"], "http",
            "v3 的 add 响应字段：success/exitIp/ispInfo/type"
        );
        assert_eq!(v["exitIp"], "198.51.100.7");
        // v3 别名
        let (st, _) = call(
            &h.app,
            "POST",
            "/api/residential/urls",
            Some(serde_json::json!({"url": "socks5://user1:pw1@isp3.example.net:1080"})),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(
            rstate::group_of(&*h.ctx.store.read().await).upstreams.len(),
            3
        );
        // v3 的删除定位方式（URL 编码的 host:port）
        let (st, v) = call(
            &h.app,
            "DELETE",
            "/api/residential/urls/isp3.example.net%3A1080",
            None,
        )
        .await;
        assert_eq!(
            (st, v["success"].clone()),
            (StatusCode::OK, serde_json::json!(true))
        );
        let (st, v) = call(
            &h.app,
            "DELETE",
            "/api/residential/urls/nope.example.net%3A1",
            None,
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        assert!(v["error"].is_string());
        // 规范路径按 id 删
        let id = rstate::group_of(&*h.ctx.store.read().await).upstreams[1].id;
        let (st, _) = call(
            &h.app,
            "POST",
            "/api/residential/remove",
            Some(serde_json::json!({"id": id})),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(
            rstate::group_of(&*h.ctx.store.read().await).upstreams.len(),
            1
        );
    }

    #[tokio::test]
    async fn a_bad_paste_returns_400_with_the_parser_message() {
        let d = tempfile::tempdir().unwrap();
        let h = harness(&d).await;
        let (st, v) = call(
            &h.app,
            "POST",
            "/api/residential/add",
            Some(serde_json::json!({"url": "https://u:p@h:1"})),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        assert!(
            v["error"].as_str().unwrap().contains("https"),
            "文案要点名问题：{v}"
        );
        let (st, _) = call(
            &h.app,
            "POST",
            "/api/residential/add",
            Some(serde_json::json!({"url": "   "})),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn the_v3_post_route_dispatches_by_body() {
        let d = tempfile::tempdir().unwrap();
        let h = harness(&d).await;
        // {domains:[…]} → 设关键字
        let (st, v) = call(
            &h.app,
            "POST",
            "/api/residential",
            Some(serde_json::json!({"domains": ["openai.com"]})),
        )
        .await;
        assert_eq!(
            (st, v["success"].clone()),
            (StatusCode::OK, serde_json::json!(true))
        );
        assert_eq!(
            rstate::group_of(&*h.ctx.store.read().await).keywords,
            Some(vec!["openai.com".to_string()])
        );
        // {reset:true} → 回到跟随默认
        let (st, v) = call(
            &h.app,
            "POST",
            "/api/residential",
            Some(serde_json::json!({"reset": true})),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["domainsFollowDefault"], true);
        assert!(v["domains"].as_array().unwrap().len() >= 67);
        assert_eq!(rstate::group_of(&*h.ctx.store.read().await).keywords, None);
        // {url:…} → 加上游
        let (st, v) = call(
            &h.app,
            "POST",
            "/api/residential",
            Some(serde_json::json!({"url": "http://user1:pw1@isp9.example.net:10007"})),
        )
        .await;
        assert_eq!(
            (st, v["success"].clone()),
            (StatusCode::OK, serde_json::json!(true))
        );
        // 空体 → 400（没有语义）
        let (st, _) = call(
            &h.app,
            "POST",
            "/api/residential",
            Some(serde_json::json!({})),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn global_enable_and_disable_match_v3() {
        let d = tempfile::tempdir().unwrap();
        let h = harness(&d).await;
        let (st, v) = call(
            &h.app,
            "POST",
            "/api/residential/global",
            Some(serde_json::json!({"global": false})),
        )
        .await;
        assert_eq!(
            (st, v["global"].clone()),
            (StatusCode::OK, serde_json::json!(false))
        );
        assert_eq!(
            rstate::group_of(&*h.ctx.store.read().await).mode,
            ResiMode::Split
        );
        let (st, _) = call(
            &h.app,
            "POST",
            "/api/residential/global",
            Some(serde_json::json!({})),
        )
        .await;
        assert_eq!(
            st,
            StatusCode::UNPROCESSABLE_ENTITY,
            "缺 global 字段由 axum 的 Json 提取器挡掉"
        );
        // v3 的禁用：DELETE /api/residential
        let (st, _) = call(&h.app, "DELETE", "/api/residential", None).await;
        assert_eq!(st, StatusCode::OK);
        assert!(!rstate::group_of(&*h.ctx.store.read().await).enabled);
        // v3 的启用：POST /api/residential/enable —— **既没有 body 也没有 Content-Type**
        // （web/app.js:37-50 只在有 string body 时才加），所以 handler 必须收
        // Option<Json<EnableRequest>>；用 Json<..> 这里会是 415
        let (st, v) = call(&h.app, "POST", "/api/residential/enable", None).await;
        assert_eq!(st, StatusCode::OK, "空体启用不能是 415：{v}");
        assert_eq!(v["enabled"], true);
        assert!(rstate::group_of(&*h.ctx.store.read().await).enabled);
        // 显式关
        let (st, v) = call(
            &h.app,
            "POST",
            "/api/residential/enable",
            Some(serde_json::json!({"enabled": false})),
        )
        .await;
        assert_eq!(
            (st, v["enabled"].clone()),
            (StatusCode::OK, serde_json::json!(false))
        );
        let (st, _) = call(&h.app, "POST", "/api/residential/enable", None).await;
        assert_eq!(st, StatusCode::OK);
        // 空池启用 → 400（v3 同）。清空只走真正的删除路径：`Store::update_as` 的防线
        // 只让 `upstream::remove` 缩短上游池（R3 ①），夹具不能绕过它。
        for id in rstate::group_of(&*h.ctx.store.read().await)
            .upstreams
            .iter()
            .map(|u| u.id)
            .collect::<Vec<_>>()
        {
            upstream::remove(&h.ctx, &upstream::UpstreamSel::Id(id))
                .await
                .unwrap();
        }
        let (st, v) = call(
            &h.app,
            "POST",
            "/api/residential/enable",
            Some(serde_json::json!({"enabled": true})),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        assert!(v["error"].as_str().unwrap().contains("为空"));
    }

    #[tokio::test]
    async fn check_is_202_and_409_while_running_then_select_switches_via_clash() {
        let d = tempfile::tempdir().unwrap();
        let h = harness(&d).await;
        let id = rstate::group_of(&*h.ctx.store.read().await).upstreams[0].id;
        let (st, v) = call(
            &h.app,
            "POST",
            "/api/residential/check",
            Some(serde_json::json!({"id": id})),
        )
        .await;
        assert_eq!(st, StatusCode::ACCEPTED);
        assert_eq!(v["started"], true);
        assert_eq!(v["upstream_id"], id.to_string());
        // harness 用 background = false，所以这里没有后台体检来抢 runtime.checking：
        // 409 分支靠下面手动置 checking 触发，结果与调度时序无关
        assert_eq!(rstate::read(&h.ctx.runtime).await.checking, None);
        // 人为把 checking 置上，第二次要 409（R13 §8.4）
        rstate::update(&h.ctx.runtime, |r| {
            r.checking = Some(rstate::Checking {
                upstream_id: id,
                started_at: crate::util::fmt_rfc3339(h.ctx.host.now()),
            });
        })
        .await;
        let (st, _) = call(
            &h.app,
            "POST",
            "/api/residential/check",
            Some(serde_json::json!({"id": id})),
        )
        .await;
        assert_eq!(st, StatusCode::CONFLICT);
        // select
        rstate::update_group(&h.ctx.store, &h.ctx.bus, |g| {
            let mut u = g.upstreams[0].clone();
            u.id = Uuid::from_u128(77);
            u.host = "isp2.example.net".into();
            g.upstreams.push(u);
            crate::modules::residential::upstream::renumber(&mut g.upstreams);
        })
        .await
        .unwrap();
        let (st, v) = call(
            &h.app,
            "POST",
            "/api/residential/select",
            Some(serde_json::json!({"id": Uuid::from_u128(77)})),
        )
        .await;
        assert_eq!(
            (st, v["tag"].clone()),
            (StatusCode::OK, serde_json::json!("resi-2"))
        );
        assert_eq!(h.clash.selected(POOL).as_deref(), Some("resi-2"));
        let (st, _) = call(
            &h.app,
            "POST",
            "/api/residential/select",
            Some(serde_json::json!({"id": Uuid::from_u128(999)})),
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn select_check_and_remove_take_resi_n_url_n_and_host_port_too() {
        // 真机上运维照着 status / health 里的 resi-2 敲，端点只认 uuid 的话连 400 的
        // 文案都是 serde 的「invalid character」。三条端点共用 resolve_upstream。
        let d = tempfile::tempdir().unwrap();
        let h = harness(&d).await;
        rstate::update_group(&h.ctx.store, &h.ctx.bus, |g| {
            let mut u = g.upstreams[0].clone();
            u.id = Uuid::from_u128(77);
            u.host = "isp2.example.net".into();
            g.upstreams.push(u);
            crate::modules::residential::upstream::renumber(&mut g.upstreams);
        })
        .await
        .unwrap();
        for raw in ["resi-2", "url-2", "isp2.example.net:10007"] {
            let (st, v) = call(
                &h.app,
                "POST",
                "/api/residential/select",
                Some(serde_json::json!({"id": raw})),
            )
            .await;
            assert_eq!(
                (st, v["tag"].clone()),
                (StatusCode::OK, serde_json::json!("resi-2")),
                "{raw}"
            );
            let (st, v) = call(
                &h.app,
                "POST",
                "/api/residential/check",
                Some(serde_json::json!({"id": raw})),
            )
            .await;
            assert_eq!(st, StatusCode::ACCEPTED, "{raw}");
            assert_eq!(v["upstream_id"], Uuid::from_u128(77).to_string(), "{raw}");
        }
        // 定位不到时 404 + 三种写法的提示
        let (st, v) = call(
            &h.app,
            "POST",
            "/api/residential/select",
            Some(serde_json::json!({"id": "resi-9"})),
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        let msg = v["error"].as_str().unwrap();
        for form in ["uuid", "resi-N", "url-N", "host:port"] {
            assert!(msg.contains(form), "缺 {form}：{msg}");
        }
        // remove 也吃 resi-N（v3 的 host_port 字段照旧）
        let (st, _) = call(
            &h.app,
            "POST",
            "/api/residential/remove",
            Some(serde_json::json!({"id": "resi-2"})),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(
            rstate::group_of(&*h.ctx.store.read().await).upstreams.len(),
            1
        );
    }

    #[tokio::test]
    async fn select_locks_manually_until_select_auto_releases_it_and_google_shows_up() {
        let d = tempfile::tempdir().unwrap();
        let h = harness(&d).await;
        let id = rstate::group_of(&*h.ctx.store.read().await).upstreams[0].id;
        let (st, v) = call(
            &h.app,
            "POST",
            "/api/residential/select",
            Some(serde_json::json!({"id": id})),
        )
        .await;
        assert_eq!((st, v["tag"].clone()), (StatusCode::OK, "resi-1".into()));
        assert_eq!(
            rstate::read(&h.ctx.runtime).await.manual_selected_id,
            Some(id),
            "手动切换要锁定到失效为止（R2 ①）"
        );
        // 巡检记下的 Google 判定要在 health / status 都看得见（R2 ②）
        let now = h.ctx.host.now();
        rstate::update(&h.ctx.runtime, |r| {
            let hs = r.health.entry(id.to_string()).or_default();
            rstate::record_google(hs, Some(false), now);
        })
        .await;
        let (_, hv) = call(&h.app, "GET", "/api/residential/health", None).await;
        assert_eq!(hv["members"][0]["manual_locked"], true);
        assert_eq!(hv["members"][0]["google_ok"], false);
        assert!(hv["members"][0]["google_at"].is_string());
        let (_, sv) = call(&h.app, "GET", "/api/residential/status", None).await;
        assert_eq!(sv["upstreams"][0]["google_ok"], false);
        // `{"auto": true}` = 解除锁定
        let (st, v) = call(
            &h.app,
            "POST",
            "/api/residential/select",
            Some(serde_json::json!({"auto": true})),
        )
        .await;
        assert_eq!((st, v["auto"].clone()), (StatusCode::OK, true.into()));
        assert_eq!(rstate::read(&h.ctx.runtime).await.manual_selected_id, None);
        let (_, hv) = call(&h.app, "GET", "/api/residential/health", None).await;
        assert_eq!(hv["members"][0]["manual_locked"], false);
        // 既没 id 也没 auto ⇒ 400（别静默当成解锁）
        let (st, _) = call(
            &h.app,
            "POST",
            "/api/residential/select",
            Some(serde_json::json!({})),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn blacklist_endpoints_read_pins_auto_pending_and_candidates() {
        let d = tempfile::tempdir().unwrap();
        let h = harness(&d).await;
        let (st, v) = call(&h.app, "GET", "/api/residential/blacklist", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["pins"][0]["kind"], "domain_suffix");
        assert_eq!(v["pins"][0]["value"], "pay.google.com");
        assert!(
            v["notes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n.as_str().unwrap().contains("软封锁")),
            "spec §5.4 的局限说明要出现在面板里：{v}"
        );
        // 加 pin
        let (st, _) = call(
            &h.app,
            "POST",
            "/api/residential/blacklist/pins",
            Some(serde_json::json!({"value": "www.paypal.com", "note": "支付直连"})),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(
            rstate::group_of(&*h.ctx.store.read().await)
                .blacklist
                .pins
                .len(),
            2
        );
        // 非法 kind → 400
        let (st, _) = call(
            &h.app,
            "POST",
            "/api/residential/blacklist/pins",
            Some(serde_json::json!({"kind": "regex", "value": ".*"})),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        // 端口类 pin
        let (st, _) = call(
            &h.app,
            "POST",
            "/api/residential/blacklist/pins",
            Some(serde_json::json!({"kind": "port", "value": "5228"})),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        // 删 pin
        let (st, _) = call(
            &h.app,
            "DELETE",
            "/api/residential/blacklist/pins",
            Some(serde_json::json!({"value": "www.paypal.com"})),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (st, _) = call(
            &h.app,
            "DELETE",
            "/api/residential/blacklist/pins",
            Some(serde_json::json!({"value": "www.paypal.com"})),
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        // apply：pending → auto
        let up_id = rstate::group_of(&*h.ctx.store.read().await).upstreams[0].id;
        rstate::update(&h.ctx.runtime, |r| {
            r.pending.push(rstate::PendingEntry {
                upstream_id: up_id,
                host: "gateway.icloud.com".into(),
                port: 443,
                hits: 5,
                confirms: 2,
                last_confirm_at: crate::util::fmt_rfc3339(h.ctx.host.now()),
            });
        })
        .await;
        let (st, v) = call(&h.app, "POST", "/api/residential/blacklist/apply", None).await;
        assert_eq!(
            (st, v["applied"].clone()),
            (StatusCode::OK, serde_json::json!(1))
        );
        let (_, v) = call(&h.app, "GET", "/api/residential/blacklist", None).await;
        assert_eq!(v["auto"].as_array().unwrap().len(), 2);
        assert_eq!(v["auto"][1]["value"], "gateway.icloud.com");
        assert_eq!(v["auto"][1]["upstream_name"], "url-1");
        // 删一条 auto（裁决 D8）
        let (st, _) = call(
            &h.app,
            "DELETE",
            "/api/residential/blacklist",
            Some(serde_json::json!({"upstream_id": up_id, "value": "gateway.icloud.com"})),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
    }

    /// 三个上游各自学到同一条 pending / 候选：每条都带上游 id / 名字 / 槽位 / host:port，
    /// 让 CLI 与面板能按上游分组；status 摘要的计数仍是原始条数。
    #[tokio::test]
    async fn blacklist_rows_name_their_upstream_and_status_counts_stay_raw() {
        let d = tempfile::tempdir().unwrap();
        let h = harness(&d).await;
        grow_pool(&h, 3).await;
        let s = h.ctx.store.read().await.clone();
        let g = rstate::group_of(&s);
        let ids: Vec<Uuid> = g.upstreams.iter().map(|u| u.id).collect();
        rstate::update(&h.ctx.runtime, |r| {
            for id in &ids {
                r.pending.push(rstate::PendingEntry {
                    upstream_id: *id,
                    host: "api.stripe.com".into(),
                    port: 443,
                    hits: 5,
                    confirms: 2,
                    last_confirm_at: "2026-09-12T00:00:00Z".into(),
                });
                r.candidates.insert(
                    format!("{id}|pay.google.com:443"),
                    rstate::Candidate {
                        upstream_id: *id,
                        host: "pay.google.com".into(),
                        port: 443,
                        hits: 1,
                        first_seen: "2026-09-12T00:00:00Z".into(),
                        last_seen: "2026-09-12T00:00:00Z".into(),
                        confirms: 0,
                        last_confirm_at: None,
                    },
                );
            }
        })
        .await;
        let (st, v) = call(&h.app, "GET", "/api/residential/blacklist", None).await;
        assert_eq!(st, StatusCode::OK);
        for key in ["pending", "candidates"] {
            let rows = v[key].as_array().unwrap();
            assert_eq!(rows.len(), 3, "{key}：{v}");
            for (i, u) in g.upstreams.iter().enumerate() {
                let row = rows
                    .iter()
                    .find(|r| r["upstream_id"] == serde_json::json!(u.id))
                    .unwrap_or_else(|| panic!("{key} 缺上游 {i}：{v}"));
                assert_eq!(row["upstream_name"], u.name.as_str(), "{key}");
                assert_eq!(
                    row["upstream_addr"],
                    format!("{}:{}", u.host, u.port).as_str(),
                    "{key}"
                );
                let slot = s
                    .residential
                    .slots
                    .iter()
                    .find(|sl| sl.upstream_id == u.id)
                    .unwrap()
                    .index;
                assert_eq!(row["upstream_slot"], slot, "{key}");
            }
        }
        // auto 行同样带槽位与 host:port
        assert!(v["auto"][0].get("upstream_slot").is_some(), "{v}");
        assert!(v["auto"][0].get("upstream_addr").is_some(), "{v}");
        let (_, sv) = call(&h.app, "GET", "/api/residential/status", None).await;
        assert_eq!(sv["blacklist"]["pending"], 3);
        assert_eq!(sv["blacklist"]["candidates"], 3);
    }

    #[tokio::test]
    async fn priority_endpoint_updates_the_switch_ordering_key() {
        let d = tempfile::tempdir().unwrap();
        let h = harness(&d).await;
        let id = rstate::group_of(&*h.ctx.store.read().await).upstreams[0].id;
        let (st, _) = call(
            &h.app,
            "POST",
            "/api/residential/priority",
            Some(serde_json::json!({"id": id, "priority": 3})),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(
            rstate::group_of(&*h.ctx.store.read().await).upstreams[0].priority,
            3
        );
    }

    #[test]
    fn rule_conversion_covers_the_three_kinds_and_rejects_the_rest() {
        assert_eq!(
            rule_of(None, "a.com"),
            Some(Rule::DomainSuffix("a.com".into())),
            "缺省是 domain_suffix"
        );
        assert_eq!(
            rule_of(Some("domain"), "a.com"),
            Some(Rule::Domain("a.com".into()))
        );
        assert_eq!(rule_of(Some("port"), "5228"), Some(Rule::Port(5228)));
        assert_eq!(rule_of(Some("port"), "abc"), None);
        assert_eq!(rule_of(Some("regex"), ".*"), None);
        // 主机名校验（v3 R13 §8.4 的 `^[a-z0-9.-]{1,253}$`）
        assert_eq!(rule_of(None, "a b.com"), None);
        assert_eq!(rule_of(None, ""), None);
        assert_eq!(
            rule_parts(&Rule::Port(853)),
            ("port".to_string(), "853".to_string())
        );
        assert_eq!(
            rule_parts(&Rule::DomainSuffix("a.com".into())),
            ("domain_suffix".to_string(), "a.com".to_string())
        );
    }

    // ── T5：槽位表与 pin 端点（spec §5.6）──────────────────────────────────

    /// 把 `harness` 的池扩到 `n` 条上游（`sample_group()` 自带 1 条），并同步槽位与分配。
    /// **不改 `harness` 本身**：它是十几条既有测试的夹具，加参数会牵动全部调用点。
    async fn grow_pool(h: &Harness, n: u16) {
        let proto = rstate::group_of(&*h.ctx.store.read().await).upstreams[0].clone();
        crate::modules::residential::slots::update_group_slots_as(
            &h.ctx.store,
            &h.ctx.bus,
            crate::state::store::CALLER_UNLABELED,
            move |g| {
                while (g.upstreams.len() as u16) < n {
                    let i = g.upstreams.len() as u16;
                    g.upstreams.push(Upstream {
                        id: Uuid::from_u128(0xa000 + u128::from(i)),
                        name: format!("url-{}", i + 1),
                        host: format!("isp{}.example.net", i + 1),
                        ..proto.clone()
                    });
                }
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn get_slots_lists_ip_egress_users_and_metrics() {
        let d = tempfile::tempdir().unwrap();
        let h = harness(&d).await;
        grow_pool(&h, 3).await;
        // 把 alice 挪到槽 1（`sample_state` 只有她一个用户，默认落在槽 0）
        let uid = h.ctx.store.read().await.users[0].user_id;
        let slot1 = bui_schema::slots::sorted(&h.ctx.store.read().await.residential)[1].upstream_id;
        assert!(
            crate::modules::residential::slots::assign_user(&h.ctx, uid, slot1)
                .await
                .unwrap()
        );

        let (code, v) = call(&h.app, "GET", "/api/residential/slots", None).await;
        assert_eq!(code, StatusCode::OK);
        let rows = v["slots"].as_array().unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["index"], 0);
        assert_eq!(rows[0]["selector"], "slot-0-pool");
        assert_eq!(rows[0]["upstream_tag"], "resi-1");
        assert_eq!(rows[1]["relay_port"], 2081);
        assert_eq!(rows[1]["hy2_port"], 40001);
        assert_eq!(rows[1]["hop"], serde_json::json!([44000, 46999]));
        assert_eq!(rows[1]["user_count"], 1);
        assert_eq!(
            rows[1]["users"],
            serde_json::json!(["alice"]),
            "按槽列出用户名（spec §5.6「用户数与用户名」）"
        );
        assert_eq!(rows[0]["user_count"], 0);
        // 指标与 health 的 members 同源（都经 metrics_of）
        assert!(rows[0].as_object().unwrap().contains_key("latency_p50_ms"));
        assert!(rows[0].as_object().unwrap().contains_key("down_mbps"));
        // 绝不回凭据（`sample_group()` 的上游密码是 "p"，用整串字段名判，别用单字母子串）
        let text = serde_json::to_string(&v).unwrap();
        assert!(!text.contains("password"), "响应里不许出现密码字段：{text}");
    }

    #[tokio::test]
    async fn status_and_health_both_carry_the_same_slot_rows() {
        let d = tempfile::tempdir().unwrap();
        let h = harness(&d).await;
        grow_pool(&h, 2).await;
        let (_, a) = call(&h.app, "GET", "/api/residential/status", None).await;
        let (_, b) = call(&h.app, "GET", "/api/residential/health", None).await;
        assert_eq!(a["slots"], b["slots"], "同一个投影函数，两处必须一致");
        assert_eq!(a["slots"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn post_slots_pin_sets_and_clears_the_pin() {
        let d = tempfile::tempdir().unwrap();
        let h = harness(&d).await;
        grow_pool(&h, 2).await;
        let target =
            bui_schema::slots::sorted(&h.ctx.store.read().await.residential)[1].upstream_id;
        let (code, _) = call(
            &h.app,
            "POST",
            "/api/residential/slots/pin",
            Some(serde_json::json!({"index": 0, "id": target.to_string()})),
        )
        .await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(h.clash.selected("slot-0-pool").as_deref(), Some("resi-2"));
        let (_, v) = call(&h.app, "GET", "/api/residential/slots", None).await;
        assert_eq!(v["slots"][0]["pinned"], true);
        assert_eq!(v["slots"][0]["active_tag"], "resi-2");
        assert_eq!(v["slots"][0]["borrowed"], true);
        assert_eq!(v["slots"][0]["borrowed_from"], "isp2.example.net:10007");

        let (code, _) = call(
            &h.app,
            "POST",
            "/api/residential/slots/pin",
            Some(serde_json::json!({"index": 0, "auto": true})),
        )
        .await;
        assert_eq!(code, StatusCode::OK);
        let (_, v) = call(&h.app, "GET", "/api/residential/slots", None).await;
        assert_eq!(v["slots"][0]["pinned"], false);
    }

    // ── T7：`rebalance` / `assign` ─────────────────────────────────────────

    #[tokio::test]
    async fn post_rebalance_moves_users_and_reports_the_count() {
        let d = tempfile::tempdir().unwrap();
        let h = harness(&d).await;
        grow_pool(&h, 2).await;
        // alice 在槽 0；rebalance 后（1 人 2 槽）她仍在槽 0 ⇒ 改动数 0
        let (code, v) = call(&h.app, "POST", "/api/residential/rebalance", None).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(v["moved"], 0);
        // 把她挪到槽 1，再 rebalance ⇒ 回到槽 0，改动数 1
        let slot1 = bui_schema::slots::sorted(&h.ctx.store.read().await.residential)[1].upstream_id;
        let uid = h.ctx.store.read().await.users[0].user_id;
        crate::modules::residential::slots::assign_user(&h.ctx, uid, slot1)
            .await
            .unwrap();
        let (_, v) = call(&h.app, "POST", "/api/residential/rebalance", None).await;
        assert_eq!(v["moved"], 1);
        let (_, v) = call(&h.app, "GET", "/api/residential/slots", None).await;
        assert_eq!(v["slots"][0]["user_count"], 1);
    }

    #[tokio::test]
    async fn post_assign_pins_one_user_to_one_slot() {
        let d = tempfile::tempdir().unwrap();
        let h = harness(&d).await;
        grow_pool(&h, 2).await;
        let (code, _) = call(
            &h.app,
            "POST",
            "/api/residential/assign",
            Some(serde_json::json!({"user": "alice", "target": "resi-2"})),
        )
        .await;
        assert_eq!(code, StatusCode::OK);
        let (_, v) = call(&h.app, "GET", "/api/residential/slots", None).await;
        assert_eq!(v["slots"][1]["users"], serde_json::json!(["alice"]));
        // 槽序号写法也认
        let (code, _) = call(
            &h.app,
            "POST",
            "/api/residential/assign",
            Some(serde_json::json!({"user": "alice", "target": "0"})),
        )
        .await;
        assert_eq!(code, StatusCode::OK);
        let (_, v) = call(&h.app, "GET", "/api/residential/slots", None).await;
        assert_eq!(v["slots"][0]["users"], serde_json::json!(["alice"]));
    }

    #[tokio::test]
    async fn post_assign_rejects_an_unknown_user_or_target() {
        let d = tempfile::tempdir().unwrap();
        let h = harness(&d).await;
        grow_pool(&h, 2).await;
        for (body, want) in [
            (
                serde_json::json!({"user": "nobody", "target": "resi-1"}),
                StatusCode::NOT_FOUND,
            ),
            (
                serde_json::json!({"user": "alice", "target": "resi-9"}),
                StatusCode::NOT_FOUND,
            ),
            (
                serde_json::json!({"user": "alice"}),
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
        ] {
            let (code, v) = call(&h.app, "POST", "/api/residential/assign", Some(body)).await;
            assert_eq!(code, want, "{v}");
        }
    }

    #[tokio::test]
    async fn post_slots_pin_rejects_a_bad_index_or_target() {
        let d = tempfile::tempdir().unwrap();
        let h = harness(&d).await;
        grow_pool(&h, 2).await;
        for (body, want) in [
            // 槽序号不存在 ⇒ `pin_slot` 自己报错 ⇒ 400
            (
                serde_json::json!({"index": 9, "auto": true}),
                StatusCode::BAD_REQUEST,
            ),
            // 上游定位串解析不出来 ⇒ 沿用 `map_upstream_err` 的 **404**（与 `/assign`、
            // `/select` 等既有端点同一口径，Fable 2026-09-13 裁决）
            (
                serde_json::json!({"index": 0, "id": "isp9.example.net:1"}),
                StatusCode::NOT_FOUND,
            ),
            // 既不给 id 又不给 auto ⇒ 请求本身没意义 ⇒ 400
            (serde_json::json!({"index": 0}), StatusCode::BAD_REQUEST),
        ] {
            let (code, v) = call(&h.app, "POST", "/api/residential/slots/pin", Some(body)).await;
            assert_eq!(code, want, "{v}");
        }
    }
}
