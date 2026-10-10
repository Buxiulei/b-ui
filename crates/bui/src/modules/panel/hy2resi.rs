//! 住宅 HY2（官方 sing-box）的两个控制面客户端（spec §2.3、§5.1、§5.2）。
//!
//! 住宅入站从 apernet hysteria 换成 sing-box 之后，「计量」与「在线 / 踢人」分别落在
//! 两个只监听回环、都不设 secret 的面上（[`super::HY2_RESI_API`] /
//! [`super::HY2_RESI_CLASH_API`]）：
//!
//! - **计量**走官方内核自带的 `daemon.StartedService`：[`Hy2ResiClient::started_at`]（实例
//!   代际）、[`Hy2ResiClient::subscribe_status`]（全局总量与活动连接数）、
//!   [`Hy2ResiClient::subscribe_connections`]（逐连接事件流）。三条 RPC 的返回值由
//!   [`frame_of_events`] / [`frame_of_status`] 翻译成 [`super::meter::Frame`]，
//!   **语义全在账本里**（`panel::meter` 的规则 1–11，每条都有 G0 决定性实验背书）。
//!   **不再走 v2ray_api 的 `StatsService.QueryStats`**：官方发行物永不含 `with_v2ray_api`
//!   编译标签（2026-10-10 抽样 17 个 tag、0 次命中），那条路只在自建内核上存在；
//!   [`Hy2ResiApi::query_user_deltas`] 因此改成「排空流驱动账本」（[`Self::meter_session`]
//!   喂帧、采样器 `drain`），账本不在时是一条响亮的 `Err`（[`STREAM_DOWN_MSG`]）。
//!   归属键在 `connection.user`（服务端 `users[].name`，**不是凭据 `id`**）：新发凭据两者
//!   相同，迁移用户的 `name` 是他的用户名（本部署即中文，T3 2026-09-18 真机实测中文与含
//!   空格的 name 在 `auth_user`、`users[].name` 三处都一字不改地工作）；`name → user_id`
//!   的映射在采样器（`traffic.rs`），本模块只把 `user` 原样交给账本。
//! - **在线数与踢人**走 Clash API：`GET /connections` 里**没有** user 字段，唯一能把连接
//!   归到用户的线索是 `rule` 字符串里的 `auth_user=<name>`（[`user_of_rule`]）；踢用户 =
//!   门（selector）切 `deny` + 逐条 `DELETE /connections/{id}` 兜底（spec §5.2）。
//!
//! 内核重启会清空计量计数器：账本按 `GetStartedAt` 的代际封账（`meter` 规则 9），本模块
//! 只把 `started_at` 原样交给它，**不做**降级或补偿。
//!
//! 计量面的地址是 [`super::HY2_RESI_API`]（渲染器产的顶层 `services` 里的 api 服务，
//! 官方内核 `daemon.StartedService` 的 gRPC）；4.x 那个 `experimental.v2ray_api` 段已随
//! 自建二进制一起退役。**常量只许与渲染器同一次提交改**：分叉 = 客户端打没人听的端口而
//! 测试全绿，属静默生产故障。

// 生成代码归 prost/tonic 管，clippy 的意见对它没有意义；理由同 `panel::xray::pb`。
#[allow(clippy::all, dead_code)]
pub mod pb {
    // prost 生成的嵌套模块树；包文件是 `daemon.rs`、模块根是 `singbox_daemon.rs`
    // （两个都出自 build.rs 的第二次 `configure()`，取名理由见 build.rs 的注释）。
    include!(concat!(env!("OUT_DIR"), "/singbox_daemon.rs"));
}

use super::meter::{ClosedConn, Event, EventKind, Frame, Ledger};
use super::xray::{CALL_TIMEOUT, CONNECT_TIMEOUT};
use super::{Hy2ResiApi, TxRx};
use crate::modules::residential::clash::ClashError;
use bui_schema::render::hy2_singbox::gate_tag;
use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};
use tonic::transport::{Channel, Endpoint};

use pb::daemon::{
    started_service_client::StartedServiceClient, Connection, ConnectionEventType,
    ConnectionEvents, Status, SubscribeConnectionsRequest, SubscribeStatusRequest,
};

/// Clash API 的超时：与直连那条 trafficStats HTTP 路同值（[`super::hy2::REQ_TIMEOUT`]）。
/// 采样与生命周期动作都**同步等**它，所以宁短不长 —— 住宅面挂了要的是快速报错，
/// 不是把整轮采样拖住（gRPC 那条路另有 `CONNECT_TIMEOUT` / `CALL_TIMEOUT`）。
pub const REQ_TIMEOUT: std::time::Duration = super::hy2::REQ_TIMEOUT;

/// 两条订阅 RPC 的 `interval`。
///
/// **单位是纳秒**（Go 的 `time.Duration`：上游 `SubscribeStatus` / `SubscribeConnections`
/// 直接 `time.Duration(request.Interval)` 交给 `time.NewTicker`，`started_service.go:466`
/// 与 `:821`，v1.14.3）：`<= 0` ⇒ 上游默认 **1 秒**。填成毫秒不会报错，只会让流按微秒级
/// 抖动（那会让 G0 的「tick 粒度」实测结论全部失效），所以这里恒填 0，把节奏留给内核。
pub const SUBSCRIBE_INTERVAL_NS: i64 = 0;

/// 账本不在收帧（未建立 / 已中断）时 `query_user_deltas` 的措辞。
///
/// 它经采样器（`traffic.rs`）带着 `METERING_MARKER` + `METERING_LOSS_TAG` 落成 `warn`，
/// 并被哨兵 `hy2_resi_metering_lost` 认领 —— 所以**不许**写成「没有流量」这类可被读成正常的话。
pub(super) const STREAM_DOWN_MSG: &str =
    "住宅计量流不在（未建立或已中断）：没有任何确证字节，本轮不产生增量";

/// `GET /connections` 里的一条连接。`metadata` 里**没有** user 字段，所以只留
/// 归组要用的三项（`rule` 归人、`id` 踢人、`chains` 供自检看门位是否真生效）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hy2ResiConn {
    pub id: String,
    pub rule: String,
    pub chains: Vec<String>,
}

/// `rule` 字符串 → 这条连接属于哪个凭据 name。
///
/// 形态是 sing-box 自己拼的：`F.ToString(c.Rule, " => ", c.Rule.Action())`
/// （`experimental/clashapi/connections.go`），`auth_user` 那一项的 `String()` 是
/// `"auth_user=" + users[0]`，`route` 动作的是 `"route(" + 出站 + ")"`
/// ⇒ `auth_user=<凭据 name> => route(gate-<id>)`。没有规则命中时 sing-box 写的是 `"final"`。
///
/// **`auth_user` 位置是凭据 `name`，不是 `id`**（T3 2026-09-18 真机复核）：新发凭据的
/// `name` 恰好等于 `id`（于是形如 `auth_user=r000 => route(gate-r000)`），而**迁移用户的
/// `name` 是他的用户名**（本部署即中文，spec §3.1）—— 真机实测中文 / 含空格的 name 在
/// `auth_user`、`users[].name`、`stats.users` 三处都一字不改地工作。所以这个函数返回的是
/// 凭据 `name`，调用方要经 `hy2_pool` 才换得到 `user_id`，别把它当 `id` 用。
///
/// 多用户形态（`auth_user=[a b]`，规则里写了多个 user）归不到单个用户，返回 `None`
/// —— 我们每条规则只写一个用户（spec §3.2），出现它就是渲染出了错，宁可不计。
pub fn user_of_rule(rule: &str) -> Option<&str> {
    let cond = rule.split(" => ").next()?;
    let name = cond
        .split_whitespace()
        .find_map(|t| t.strip_prefix("auth_user="))?;
    (!name.is_empty() && !name.starts_with('[')).then_some(name)
}

/// 凭据 name → 当前连接条数（一条连接算一次，spec §5.2）
pub fn online_of(conns: &[Hy2ResiConn]) -> BTreeMap<String, u32> {
    let mut out: BTreeMap<String, u32> = BTreeMap::new();
    for c in conns {
        if let Some(name) = user_of_rule(&c.rule) {
            *out.entry(name.to_string()).or_default() += 1;
        }
    }
    out
}

/// `GET /connections` 的响应体 → 连接表；坏条目跳过，整轮不失败（口径同 `hy2::parse_traffic`）。
pub fn parse_connections(body: &str) -> Vec<Hy2ResiConn> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let Some(list) = v.get("connections").and_then(|c| c.as_array()) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|c| {
            Some(Hy2ResiConn {
                id: c.get("id")?.as_str()?.to_string(),
                rule: c.get("rule")?.as_str()?.to_string(),
                chains: c
                    .get("chains")
                    .and_then(|a| a.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|t| t.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default(),
            })
        })
        .collect()
}

/// Typed stock Clash selector inventory; invalid gates never become an empty success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateSelector {
    pub kind: String,
    pub now: String,
    pub all: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GateInventory {
    pub gates: BTreeMap<String, GateSelector>,
}

impl GateInventory {
    pub fn selected(&self) -> BTreeMap<String, String> {
        self.gates
            .iter()
            .map(|(tag, gate)| (tag.clone(), gate.now.clone()))
            .collect()
    }
}

pub fn parse_gate_inventory(body: &str) -> anyhow::Result<GateInventory> {
    let value: serde_json::Value = serde_json::from_str(body)?;
    let proxies = value
        .get("proxies")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| anyhow::anyhow!("Clash inventory missing proxies object"))?;
    let mut gates = BTreeMap::new();
    for (tag, value) in proxies
        .iter()
        .filter(|(tag, _)| tag.starts_with(&gate_tag("")))
    {
        let kind = value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("gate {tag} missing type"))?;
        anyhow::ensure!(kind == "Selector", "gate {tag} is not a Selector");
        let now = value
            .get("now")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("gate {tag} missing now"))?;
        let all = value
            .get("all")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("gate {tag} missing all"))?
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| anyhow::anyhow!("gate {tag} invalid member"))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        anyhow::ensure!(all.iter().any(|s| s == "deny"), "gate {tag} missing deny");
        anyhow::ensure!(
            all.iter().any(|s| s == now),
            "gate {tag} selection is not a member"
        );
        anyhow::ensure!(all.iter().all(|s| !s.is_empty()), "gate {tag} empty member");
        gates.insert(
            tag.clone(),
            GateSelector {
                kind: kind.into(),
                now: now.into(),
                all,
            },
        );
    }
    Ok(GateInventory { gates })
}

/// Strict projection for callers needing only current selections.
pub fn parse_proxies_now(body: &str) -> anyhow::Result<BTreeMap<String, String>> {
    Ok(parse_gate_inventory(body)?.selected())
}

/// 一帧 `SubscribeConnections` 投递 → 账本帧。
///
/// 三个只在线上才看得见、又最容易读错的形状（上游 `daemon.StartedService` v1.14.3，
/// `started_service.go`；G0 实测）：
/// ① **首帧/重连快照**：`reset = true`，把订阅之前就存在的连接以 `NEW` + 完整快照发出来，
///    closed 环里的条目也以 `NEW` 出现（`buildInitialConnectionState`，`:878`）⇒ 账本按
///    id **upsert**，快照总量绝不累加。
/// ② **`UPDATE` 只带增量**、不带 `connection` 对象（`buildTrafficUpdates`，`:958`）⇒ 只能
///    按 id 关联；`uplink_delta` / `downlink_delta` 由账本累进 `known`（只服务指标 A）。
/// ③ **`CLOSED` 可能没有 `connection`**：事件带了 id，但元数据已被 GC 时 `connection` 为
///    nil（`applyConnectionEvent` 的 `event.Metadata.ID != uuid.Nil` 判定，`:936`；实测
///    14 条）⇒ [`ClosedConn`] 为 `None`，账本走规则 7（字节进总账、不进任何人的账）。
///
/// 包成独立函数是为了让这三条**可以被测试钉住**：字段读错（把 `uplink_delta` 当总量、
/// 把 `r#type` 的 0/1/2 认反）编译一律过，只会静默把账算错。
pub fn frame_of_events(msg: &ConnectionEvents) -> Frame {
    let mut events = Vec::with_capacity(msg.events.len());
    for e in &msg.events {
        if e.id.is_empty() {
            // 上游恒发 uuid；空 id 是坏帧。收下它只会给 `open` / `dedup` 造一个会互相
            // 碰撞的空键，不如丢掉（丢掉最坏也是把它当「没有收到」）。
            continue;
        }
        let kind = match ConnectionEventType::try_from(e.r#type) {
            Ok(ConnectionEventType::ConnectionEventNew) => match e.connection.as_ref() {
                Some(c) => EventKind::New { total: totals(c) },
                // 没有快照的 NEW 语义未知（上游不这么发）⇒ 不给它在册，别猜
                None => continue,
            },
            Ok(ConnectionEventType::ConnectionEventUpdate) => EventKind::Update {
                delta: TxRx {
                    tx: non_negative(e.uplink_delta),
                    rx: non_negative(e.downlink_delta),
                },
            },
            Ok(ConnectionEventType::ConnectionEventClosed) => EventKind::Closed {
                conn: e.connection.as_ref().map(closed_conn),
            },
            // 认不出的类型（更新的内核加了新枚举值）：跳过。别当 NEW —— 拿未知语义的快照
            // 去 upsert `open`，会让指标 A 的 ΔS 从此带上一个说不清的偏差。
            Err(_) => continue,
        };
        events.push(Event {
            id: e.id.clone(),
            kind,
        });
    }
    Frame {
        reset: msg.reset,
        global: None,
        connections_in: None,
        events,
    }
}

/// 一帧 `SubscribeStatus` 投递 → 账本帧（只有全局总量与内核报的活动连接数）。
///
/// `traffic_available = false`（内核还没装上流量计数器）⇒ `global = None`：这一帧**不更新**
/// 指标 A 的 G。绝不用 0 顶替 —— 顶替会让下一个区间的 ΔG 假涨成整段总量。
pub fn frame_of_status(msg: &Status) -> Frame {
    Frame {
        reset: false,
        global: msg.traffic_available.then(|| TxRx {
            tx: non_negative(msg.uplink_total),
            rx: non_negative(msg.downlink_total),
        }),
        connections_in: u64::try_from(msg.connections_in).ok(),
        events: Vec::new(),
    }
}

/// `CLOSED` 事件里的连接元数据 → 账本要的终值（规则 1：只有这里的 `total` 入账）。
fn closed_conn(c: &Connection) -> ClosedConn {
    ClosedConn {
        user: non_empty_string(&c.user),
        destination: non_empty_string(&c.destination),
        network: non_empty_string(&c.network),
        total: totals(c),
        created_at_ms: c.created_at,
        closed_at_ms: c.closed_at,
    }
}

fn totals(c: &Connection) -> TxRx {
    TxRx {
        tx: non_negative(c.uplink_total),
        rx: non_negative(c.downlink_total),
    }
}

/// 负数在协议上是「计数器被重置」，不是「负流量」：丢成 0，绝不 `as u64` 回绕
/// （回绕会瞬间把用户判成超限 —— 与 Xray 那条路 `deltas_from_stats` 的同一条口径）。
fn non_negative(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

fn non_empty_string(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

pub struct Hy2ResiClient {
    clash_api: String,
    grpc_api: String,
    http: reqwest::Client,
    channel: OnceLock<Channel>,
    /// 计量账本（`panel::meter`）。**与 `query_user_deltas` 读的是同一个**：
    /// [`Self::meter_session`] 往里喂帧，采样器从它 `drain`，哨兵 / 裁决读它的有损位。
    ledger: Arc<Ledger>,
}

impl Hy2ResiClient {
    pub fn new() -> Self {
        Self::with_endpoints(super::HY2_RESI_CLASH_API, super::HY2_RESI_API)
    }

    pub fn with_endpoints(clash_api: impl Into<String>, grpc_api: impl Into<String>) -> Self {
        Self {
            clash_api: clash_api.into(),
            grpc_api: grpc_api.into(),
            http: reqwest::Client::builder()
                // 回环地址，绝不能走系统代理（住宅上游本身就是代理，绕回去会死锁）
                .no_proxy()
                .timeout(REQ_TIMEOUT)
                .build()
                .expect("reqwest 客户端构造不会失败"),
            channel: OnceLock::new(),
            ledger: Arc::new(Ledger::new()),
        }
    }

    pub fn clash_api(&self) -> &str {
        &self.clash_api
    }

    pub fn grpc_api(&self) -> &str {
        &self.grpc_api
    }

    /// Clash API 的 URL。`selector` / 连接 id 只可能是渲染器产的
    /// `gate-r000` 一类 tag 或 sing-box 自己发的 UUID，不含要转义的字符。
    pub fn clash_url(&self, path: &str) -> String {
        format!("http://{}{path}", self.clash_api)
    }

    pub fn grpc_endpoint_url(&self) -> String {
        format!("http://{}", self.grpc_api)
    }

    /// `connect_lazy` 的通道：进程启动时住宅 sing-box 可能还没起来，所以不在构造时连；
    /// 通道自己会重连，掉线不需要我们重建（同 `panel::xray::XrayClient::channel`）。
    fn channel(&self) -> anyhow::Result<Channel> {
        if let Some(c) = self.channel.get() {
            return Ok(c.clone());
        }
        let ep = Endpoint::from_shared(self.grpc_endpoint_url())?
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(CALL_TIMEOUT);
        let c = ep.connect_lazy();
        let _ = self.channel.set(c.clone());
        Ok(c)
    }

    /// 实例代际（`GetStartedAt`，秒）。账本按它换代封账（`meter` 规则 9），
    /// 本方法只做透传：**不做**降级、不猜「没变就是没重启」。
    pub async fn started_at(&self) -> anyhow::Result<i64> {
        let mut c = StartedServiceClient::new(self.channel()?);
        Ok(c.get_started_at(()).await?.into_inner().started_at)
    }

    /// 全局状态流（`SubscribeStatus`）：指标 A 的 G 与指标 C 的活动连接数。
    /// 返回的流由调用方驱动（`meter::metering_loop`），这里只负责把错误响亮地抛出去。
    pub async fn subscribe_status(&self) -> anyhow::Result<tonic::Streaming<Status>> {
        let mut c = StartedServiceClient::new(self.channel()?);
        let stream = c
            .subscribe_status(SubscribeStatusRequest {
                interval: SUBSCRIBE_INTERVAL_NS,
            })
            .await?
            .into_inner();
        Ok(stream)
    }

    /// 逐连接事件流（`SubscribeConnections`）：唯一入账来源（`meter` 规则 1）。
    pub async fn subscribe_connections(
        &self,
    ) -> anyhow::Result<tonic::Streaming<ConnectionEvents>> {
        let mut c = StartedServiceClient::new(self.channel()?);
        let stream = c
            .subscribe_connections(SubscribeConnectionsRequest {
                interval: SUBSCRIBE_INTERVAL_NS,
            })
            .await?
            .into_inner();
        Ok(stream)
    }

    /// 非 2xx 的**读**接口失败：措辞不许冒用 `Rejected` 的「拒绝切换到 {tag}」——
    /// 当时并没有在切任何门，那句话是 [`Hy2ResiApi::select`] 的签名文案（T13 的哨兵
    /// 按它认 `hy2_resi_gate_sync_failed`）。仍走 `Unreachable`，所以「Clash API 不可达」
    /// 这半个签名照旧命中，运维文案也是事实。
    fn http_failed(method: &str, path: &str, status: u16) -> ClashError {
        ClashError::Unreachable(format!("{method} {path} 回 HTTP {status}"))
    }

    async fn clash_get(&self, path: &str) -> Result<String, ClashError> {
        let resp = self
            .http
            .get(self.clash_url(path))
            .send()
            .await
            .map_err(|e| ClashError::Unreachable(e.to_string()))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(Self::http_failed("GET", path, status));
        }
        resp.text()
            .await
            .map_err(|e| ClashError::Unreachable(e.to_string()))
    }
}

impl Default for Hy2ResiClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Hy2ResiApi for Hy2ResiClient {
    /// **签名冻结**（键 = 服务端 `users[].name`），实现换成「排空流驱动账本」：
    /// 帧由 [`Self::meter_session`] 喂进 [`super::meter::Ledger`]，这里只把已确证的
    /// 每用户字节取走。**流不在时必须是 `Err`**（[`Ledger::available`]）—— 静默回空表
    /// 会被采样器读成「这轮没有字节」，正是审计核实的三条静默失效链之一。
    async fn query_user_deltas(&self) -> anyhow::Result<BTreeMap<String, TxRx>> {
        let now = super::meter::now_ms();
        if !self.ledger.available(now) {
            anyhow::bail!(STREAM_DOWN_MSG)
        }
        Ok(self.ledger.drain(now))
    }

    fn meter_ledger(&self) -> Arc<Ledger> {
        self.ledger.clone()
    }

    /// 一段计量会话：`GetStartedAt` 定代际 → 建两条流 → `select!` 逐帧喂账本，
    /// 直到任一条结束（`Ok(())` = 内核正常关流）或出错（`Err` = `metering_loop` 记中断）。
    async fn meter_session(&self) -> anyhow::Result<()> {
        let generation = self.started_at().await?;
        self.ledger
            .note_session_start(generation, super::meter::now_ms());
        let mut status = self.subscribe_status().await?;
        let mut conns = self.subscribe_connections().await?;
        loop {
            // 两条流各喂各的帧；`Streaming::message` 是 cancel-safe 的，select 里丢掉
            // 未完成的那个分支不会丢消息（tonic 的文档保证）。
            let frame = tokio::select! {
                msg = status.message() => msg?.map(|s| frame_of_status(&s)),
                msg = conns.message() => msg?.map(|m| frame_of_events(&m)),
            };
            let Some(frame) = frame else {
                // 内核把流关了（正常收尾）：退避重连即可，不算异常
                return Ok(());
            };
            self.ledger.apply(frame, super::meter::now_ms());
            if self.ledger.take_resubscribe() {
                // 指标 A 越线时账本自己要求的：重订阅一次，快照能补 14–30s 内的已关闭
                // 连接（按 id 去重，重收不重计）。故意重连也要按中断记（封账 + 可疑损失）。
                anyhow::bail!("按账本请求重订阅一次（指标 A 越线）")
            }
        }
    }

    async fn connections(&self) -> anyhow::Result<Vec<Hy2ResiConn>> {
        Ok(parse_connections(&self.clash_get("/connections").await?))
    }

    async fn close_connection(&self, id: &str) -> anyhow::Result<()> {
        let path = format!("/connections/{id}");
        let resp = self
            .http
            .delete(self.clash_url(&path))
            .send()
            .await
            .map_err(|e| ClashError::Unreachable(e.to_string()))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            // 踢连接也不是切门 ⇒ 同样不用「拒绝切换」那句话
            return Err(Hy2ResiClient::http_failed("DELETE", &path, status).into());
        }
        Ok(())
    }

    async fn inventory(&self) -> anyhow::Result<GateInventory> {
        parse_gate_inventory(&self.clash_get("/proxies").await?)
    }

    async fn version(&self) -> anyhow::Result<String> {
        let value: serde_json::Value = serde_json::from_str(&self.clash_get("/version").await?)?;
        Ok(value
            .get("version")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("Clash version missing product version"))?
            .to_owned())
    }

    async fn select(&self, selector: &str, tag: &str) -> anyhow::Result<()> {
        let resp = self
            .http
            .put(self.clash_url(&format!("/proxies/{selector}")))
            .json(&serde_json::json!({ "name": tag }))
            .send()
            .await
            .map_err(|e| ClashError::Unreachable(e.to_string()))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(ClashError::Rejected {
                tag: tag.to_string(),
                status,
            }
            .into());
        }
        Ok(())
    }

    async fn ready(&self) -> bool {
        self.http
            .get(self.clash_url("/version"))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    // 单条事件类型只在测试里构造（生产代码经 `frame_of_events` 收的是 Plural 流）
    use super::pb::daemon::ConnectionEvent;

    // Catches accepting malformed inventories as an empty successful authorization table.
    #[test]
    fn inventory_rejects_unknown_shape_type_selection_and_members() {
        for body in [
            "not json",
            "{}",
            r#"{"proxies":[]}"#,
            r#"{"proxies":{"gate-a":{"type":"Selector","all":["deny","slot-0-out"]}}}"#,
            r#"{"proxies":{"gate-a":{"type":"Selector","now":"deny"}}}"#,
            r#"{"proxies":{"gate-a":{"type":"Fallback","now":"deny","all":["deny"]}}}"#,
            r#"{"proxies":{"gate-a":{"type":"Selector","now":"missing","all":["deny"]}}}"#,
            r#"{"proxies":{"gate-a":{"type":"Selector","now":"slot-0-out","all":["slot-0-out"]}}}"#,
        ] {
            assert!(parse_gate_inventory(body).is_err(), "{body}");
        }
        let inventory=parse_gate_inventory(r#"{"proxies":{"GLOBAL":{"type":"Fallback"},"gate-a":{"type":"Selector","now":"deny","all":["deny","slot-0-out"]}}}"#).unwrap();
        assert_eq!(inventory.gates["gate-a"].now, "deny");
        assert_eq!(inventory.gates.len(), 1);
    }

    // Catches treating a successful version HTTP status as valid product evidence.
    #[tokio::test]
    async fn version_uses_the_real_http_product_body_and_rejects_bad_json() {
        for mode in [ClashMode::Ok, ClashMode::BadVersion] {
            let (port, _rx) = fake_clash(mode).await;
            let c = Hy2ResiClient::with_endpoints(format!("127.0.0.1:{port}"), "unused");
            if mode == ClashMode::Ok {
                assert_eq!(c.version().await.unwrap(), "sing-box 1.14.3");
            } else {
                assert!(c.version().await.is_err());
            }
        }
    }

    // Catches the old empty-map / skipped-entry fallback accepting unknown authorization.
    #[tokio::test]
    async fn incomplete_gate_inventory_is_an_error() {
        let (port, _rx) = fake_clash(ClashMode::Incomplete).await;
        let c = Hy2ResiClient::with_endpoints(format!("127.0.0.1:{port}"), "unused");
        assert!(
            c.selected_all().await.is_err(),
            "a gate without all must fail closed"
        );
    }

    /// `/connections` 的 rule 字段是唯一能把连接归到用户的线索（metadata 里没有 user 字段）。
    /// 形态来自真机实测：`auth_user=<凭据 name> => route(gate-<id>)` —— 下面两组分别是
    /// 「新发凭据（name == id）」与「迁移用户（name = 用户名）」两种口径。
    #[test]
    fn connections_are_grouped_by_the_auth_user_in_the_rule_string() {
        assert_eq!(
            user_of_rule("auth_user=r000 => route(gate-r000)"),
            Some("r000")
        );
        assert_eq!(
            user_of_rule("auth_user=alice => route(gate-r000)"),
            Some("alice")
        );
        assert_eq!(user_of_rule("final => route(deny)"), None);
        assert_eq!(user_of_rule(""), None);
        // 中文用户名（生产上真实存在）
        assert_eq!(
            user_of_rule("auth_user=张三 => route(gate-r007)"),
            Some("张三")
        );
        // 没有规则命中时 sing-box 写的就是裸 "final"（connections.go 的 else 分支）
        assert_eq!(user_of_rule("final"), None);
        // 多用户形态归不到单个用户（rule_item_auth_user.go:36）
        assert_eq!(
            user_of_rule("auth_user=[alice bob] => route(gate-r000)"),
            None
        );
    }

    #[test]
    fn online_counts_are_one_per_connection_of_that_user() {
        let conns = vec![
            Hy2ResiConn {
                id: "a".into(),
                rule: "auth_user=alice => route(gate-r000)".into(),
                chains: vec!["gate-r000".into()],
            },
            Hy2ResiConn {
                id: "b".into(),
                rule: "auth_user=alice => route(gate-r000)".into(),
                chains: vec![],
            },
            Hy2ResiConn {
                id: "c".into(),
                rule: "auth_user=r001 => route(gate-r001)".into(),
                chains: vec![],
            },
        ];
        let m = online_of(&conns);
        assert_eq!(m.get("alice").copied(), Some(2));
        assert_eq!(m.get("r001").copied(), Some(1));
    }

    /// 两个回环端点的地址只有**一处**来源：写出 `hy2-residential.json` 的那个渲染器。
    ///
    /// 断言比的是「`panel` 侧的常量 == `bui_schema` 侧的常量」而不是字面量 ——
    /// 比字面量的话，`panel` 这边另定一份值、与渲染器分叉，测试照样全绿，而生产上
    /// 计量（api 服务的 gRPC 口）与门位（Clash API）会双双打到没人听的
    /// 端口上（静默故障）。
    #[test]
    fn the_endpoints_come_from_the_renderer() {
        use bui_schema::render::hy2_singbox as renderer;
        assert_eq!(
            super::super::HY2_RESI_CLASH_API,
            renderer::HY2_RESI_CLASH_API,
            "panel 侧的 Clash API 必须就是渲染器写进配置的那一个"
        );
        assert_eq!(
            super::super::HY2_RESI_API,
            renderer::HY2_RESI_API,
            "panel 侧的 gRPC API 必须就是渲染器写进配置的那一个"
        );
        // 客户端不许自己拼地址：默认构造的两个端点就是上面那两个常量
        assert_eq!(
            Hy2ResiClient::new().clash_api(),
            renderer::HY2_RESI_CLASH_API
        );
        assert_eq!(Hy2ResiClient::new().grpc_api(), renderer::HY2_RESI_API);
        assert_eq!(
            Hy2ResiClient::new().clash_url("/connections"),
            format!("http://{}/connections", renderer::HY2_RESI_CLASH_API)
        );
        assert_eq!(
            Hy2ResiClient::new().grpc_endpoint_url(),
            format!("http://{}", renderer::HY2_RESI_API)
        );
    }

    /// 帧转换器是「线上字段 → 账本语义」的唯一翻译层：四个最贵的读错都在这里钉住。
    ///
    /// 用生成的真实类型构造（不是字符串 JSON）：字段名改了这里就编不过，比运行时断言更早。
    #[test]
    fn connection_events_translate_to_ledger_events() {
        let conn = |up: i64, down: i64| Connection {
            id: "a".into(),
            user: "alice".into(),
            destination: "example.com:443".into(),
            network: "tcp".into(),
            uplink_total: up,
            downlink_total: down,
            created_at: 1_000,
            closed_at: 2_000,
            ..Default::default()
        };
        let msg = ConnectionEvents {
            reset: true,
            events: vec![
                // NEW：快照总量，账本按 id upsert（绝不累加）
                ConnectionEvent {
                    r#type: ConnectionEventType::ConnectionEventNew as i32,
                    id: "a".into(),
                    connection: Some(conn(100, 200)),
                    ..Default::default()
                },
                // UPDATE：只有增量、没有 connection 对象（上游 buildTrafficUpdates）
                ConnectionEvent {
                    r#type: ConnectionEventType::ConnectionEventUpdate as i32,
                    id: "a".into(),
                    uplink_delta: 7,
                    downlink_delta: 9,
                    ..Default::default()
                },
                // 增量是负数（计数器被重置）：钳 0，绝不 as u64 回绕
                ConnectionEvent {
                    r#type: ConnectionEventType::ConnectionEventUpdate as i32,
                    id: "a".into(),
                    uplink_delta: -5,
                    downlink_delta: -6,
                    ..Default::default()
                },
                // CLOSED 带元数据：终值 + 归属
                ConnectionEvent {
                    r#type: ConnectionEventType::ConnectionEventClosed as i32,
                    id: "a".into(),
                    connection: Some(conn(100, 200)),
                    closed_at: 2_000,
                    ..Default::default()
                },
                // CLOSED 没有元数据（被打到 GC 的那 14 条）：账本走 conn=None，字节进总账不归人
                ConnectionEvent {
                    r#type: ConnectionEventType::ConnectionEventClosed as i32,
                    id: "b".into(),
                    closed_at: 3_000,
                    ..Default::default()
                },
                // 认不出的枚举值（更新的内核加了新类型）：跳过，不猜
                ConnectionEvent {
                    r#type: 99,
                    id: "c".into(),
                    ..Default::default()
                },
                // 空 id 是坏帧：丢掉，别造一个会互相碰撞的空键
                ConnectionEvent {
                    r#type: ConnectionEventType::ConnectionEventNew as i32,
                    connection: Some(conn(1, 1)),
                    ..Default::default()
                },
            ],
        };
        let f = frame_of_events(&msg);
        assert!(f.reset, "reset 透传：账本靠它决定要不要按快照 upsert");
        assert!(f.global.is_none(), "连接事件流不带全局总量");
        assert_eq!(
            f.events,
            vec![
                Event {
                    id: "a".into(),
                    kind: EventKind::New {
                        total: TxRx { tx: 100, rx: 200 }
                    }
                },
                Event {
                    id: "a".into(),
                    kind: EventKind::Update {
                        delta: TxRx { tx: 7, rx: 9 }
                    }
                },
                Event {
                    id: "a".into(),
                    kind: EventKind::Update {
                        delta: TxRx { tx: 0, rx: 0 }
                    }
                },
                Event {
                    id: "a".into(),
                    kind: EventKind::Closed {
                        conn: Some(ClosedConn {
                            user: Some("alice".into()),
                            destination: Some("example.com:443".into()),
                            network: Some("tcp".into()),
                            total: TxRx { tx: 100, rx: 200 },
                            created_at_ms: 1_000,
                            closed_at_ms: 2_000,
                        }),
                    },
                },
                Event {
                    id: "b".into(),
                    kind: EventKind::Closed { conn: None },
                },
            ]
        );
    }

    /// `traffic_available = false` ⇒ `global = None`（这一帧**不更新**指标 A 的 G），
    /// 绝不用 0 顶替：顶替会让下一个区间的 ΔG 假涨成整段总量。
    #[test]
    fn status_frames_skip_an_unavailable_traffic_counter() {
        let f = frame_of_status(&Status {
            connections_in: 3,
            traffic_available: true,
            uplink_total: 500,
            downlink_total: 700,
            ..Default::default()
        });
        assert_eq!(f.global, Some(TxRx { tx: 500, rx: 700 }));
        assert_eq!(f.connections_in, Some(3));
        assert!(f.events.is_empty() && !f.reset);

        let f = frame_of_status(&Status {
            connections_in: -1,
            traffic_available: false,
            uplink_total: 500,
            downlink_total: 700,
            ..Default::default()
        });
        assert_eq!(f.global, None, "计数器没装好 ⇒ 这一帧不碰 G");
        assert_eq!(f.connections_in, None, "负数连接数不收");
    }

    #[test]
    fn connections_and_proxies_bodies_parse_and_tolerate_junk() {
        // 形状抄自 `experimental/clashapi/connections.go` 的 MarshalJSON
        let conns = parse_connections(
            r#"{"downloadTotal":1,"uploadTotal":2,"connections":[
                {"id":"c1","metadata":{"network":"udp","host":"example.com"},
                 "upload":1,"download":2,"chains":["gate-r000","slot-0-out"],
                 "rule":"auth_user=r000 => route(gate-r000)","rulePayload":""},
                {"id":"c2","rule":"final","chains":[]},
                {"id":"c3"},
                {"rule":"auth_user=r001 => route(gate-r001)"}
            ],"memory":3}"#,
        );
        assert_eq!(
            conns,
            vec![
                Hy2ResiConn {
                    id: "c1".into(),
                    rule: "auth_user=r000 => route(gate-r000)".into(),
                    chains: vec!["gate-r000".into(), "slot-0-out".into()],
                },
                Hy2ResiConn {
                    id: "c2".into(),
                    rule: "final".into(),
                    chains: vec![],
                },
            ],
            "缺 id 或缺 rule 的条目跳过，不让整轮采样失败"
        );
        assert!(parse_connections("not json").is_empty());
        assert!(parse_connections("{}").is_empty());

        // 形状抄自 `experimental/clashapi/proxies.go` 的 proxyInfo：只有出站组才有 `now`
        let now = parse_proxies_now(
            r#"{"proxies":{
                "GLOBAL":{"type":"Fallback","now":"deny","all":["gate-r000"]},
                "gate-r000":{"type":"Selector","now":"slot-0-out","all":["deny","slot-0-out"]},
                "gate-r001":{"type":"Selector","now":"deny","all":["deny"]},
                "deny":{"type":"Socks","udp":true}
            }}"#,
        )
        .unwrap();
        assert_eq!(now.get("gate-r000").map(String::as_str), Some("slot-0-out"));
        assert_eq!(now.get("gate-r001").map(String::as_str), Some("deny"));
        assert!(!now.contains_key("deny"), "非组出站没有 now ⇒ 不进表");
        // sing-box 无条件塞的伪组不许混进门位表（proxies.go 的 getProxies），否则
        // 「遍历读回来的键」的写法会在生产上对它发 PUT（它不是 Selector，回 400）
        assert!(!now.contains_key("GLOBAL"), "GLOBAL 不是门位：{now:?}");
        assert_eq!(now.len(), 2, "只有 gate- 前缀的才是门位：{now:?}");
        assert!(parse_proxies_now("not json").is_err());
    }

    /// 切门失败的两种文案与 relay 那条路**逐字一致**（T13 的哨兵签名表按这两句话认）。
    #[tokio::test]
    async fn select_errors_use_the_same_wording_as_the_relay_clash_client() {
        assert_eq!(
            ClashError::Rejected {
                tag: "deny".into(),
                status: 400
            }
            .to_string(),
            "Clash API 拒绝切换到 deny（HTTP 400）"
        );
        assert!(ClashError::Unreachable("x".into())
            .to_string()
            .starts_with("Clash API 不可达："));
        // 端口 1 本机永不监听（spec §2.2 的 deny 出站用的就是它）⇒ 必是「不可达」
        let c = Hy2ResiClient::with_endpoints("127.0.0.1:1", "127.0.0.1:1");
        let err = c.select("gate-r000", "deny").await.unwrap_err().to_string();
        assert!(err.starts_with("Clash API 不可达："), "{err}");
        assert!(!c.ready().await, "没人听 ⇒ ready() 为假，不是挂住");
        assert!(c.connections().await.is_err());
        assert!(c.selected_all().await.is_err());
        assert!(c.close_connection("c1").await.is_err());
        assert!(
            c.query_user_deltas().await.is_err(),
            "gRPC 也是 Err 不是 panic"
        );
        // 三条 StartedService RPC 同理：Err 不是 panic、也不是挂住 —— 住宅 sing-box 没起来时
        // 采样循环必须能退避重试（把 Err 换成 unwrap 就是整进程挂掉）
        assert!(c.started_at().await.is_err());
        assert!(c.subscribe_status().await.is_err());
        assert!(c.subscribe_connections().await.is_err());
    }

    /// 假 Clash API 的应答口径。
    ///
    /// `AllBad` 那一档是**非 2xx 的判据**专用：`select` / `clash_get` / `close_connection`
    /// 的 `if !(200..300)` 与 `ready()` 的 `status().is_success()` 四处，删掉任何一处
    /// 都会让「门根本没切动」被当成成功——到期 / 封禁用户静默 fail-open（spec §3.4 禁止），
    /// 而这正是 T11 fail-closed 收敛唯一的判据。只靠手工构造 `ClashError` 比字符串测不到
    /// 这件事：客户端得**真的**产出一次。
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum ClashMode {
        /// 正常回；约定的坏 tag（`gate-bad` / 连接 `bad`）回非 2xx
        Ok,
        Incomplete,
        BadVersion,
        /// 一律回 500，`GET /version` 回 404（sing-box 对未知 selector 也是 404）
        AllBad,
    }

    /// 起一个**进程内的回环** HTTP 服务当假 Clash API：把「方法 + 路径 + 请求体 + 状态码」
    /// 这几件只在 HTTP 线上才成立的事真的验一遍（纯函数测不到，又最容易写错 ——
    /// `PUT /proxies/<门>` 的体漏了 `name` 就是门永远切不动、全员断网）。
    async fn fake_clash(mode: ClashMode) -> (u16, tokio::sync::mpsc::Receiver<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                // 读满：hyper 常把请求头与请求体分两次写（口径同 `hy2::tests::fake_hysteria`）
                let mut raw: Vec<u8> = Vec::new();
                let mut chunk = vec![0u8; 4096];
                let head_end = loop {
                    match raw.windows(4).position(|w| w == b"\r\n\r\n") {
                        Some(i) => break i + 4,
                        None => {
                            let n = s.read(&mut chunk).await.unwrap_or(0);
                            if n == 0 {
                                break raw.len();
                            }
                            raw.extend_from_slice(&chunk[..n]);
                        }
                    }
                };
                let head = String::from_utf8_lossy(&raw[..head_end]).to_ascii_lowercase();
                let want = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                while raw.len() < head_end + want {
                    let n = s.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    raw.extend_from_slice(&chunk[..n]);
                }
                let req = String::from_utf8_lossy(&raw).to_string();
                let _ = tx.send(req.clone()).await;
                // 约定的坏 tag：真 sing-box 对「切到非成员 tag」回 400
                // `Selector update error: not found`（`experimental/clashapi/proxies.go:178-184`），
                // 对未知 selector / 未知连接 id 回 404（`:50-53`）。
                let bad = match mode {
                    ClashMode::AllBad => Some(if req.starts_with("GET /version") {
                        404
                    } else {
                        500
                    }),
                    ClashMode::Ok if req.starts_with("PUT /proxies/gate-bad") => Some(400),
                    ClashMode::Ok if req.starts_with("DELETE /connections/bad") => Some(404),
                    ClashMode::Ok | ClashMode::Incomplete | ClashMode::BadVersion => None,
                };
                // 只有 GET 才有响应体，PUT / DELETE 学 sing-box 回 204（render.NoContent）
                let body = if bad.is_some() {
                    ""
                } else if req.starts_with("GET /connections") {
                    r#"{"connections":[{"id":"c1","rule":"auth_user=r000 => route(gate-r000)","chains":[]}]}"#
                } else if req.starts_with("GET /proxies") {
                    if mode == ClashMode::Incomplete {
                        r#"{"proxies":{"gate-r000":{"type":"Selector","now":"deny"}}}"#
                    } else {
                        r#"{"proxies":{"gate-r000":{"type":"Selector","now":"deny","all":["deny","slot-0-out"]}}}"#
                    }
                } else if req.starts_with("GET /version") {
                    if mode == ClashMode::BadVersion {
                        "not JSON"
                    } else {
                        r#"{"version":"sing-box 1.14.3","premium":true,"meta":true}"#
                    }
                } else {
                    ""
                };
                let resp = if let Some(status) = bad {
                    format!("HTTP/1.1 {status} Nope\r\nconnection: close\r\n\r\n")
                } else if body.is_empty() {
                    "HTTP/1.1 204 No Content\r\nconnection: close\r\n\r\n".to_string()
                } else {
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                };
                let _ = s.write_all(resp.as_bytes()).await;
                let _ = s.shutdown().await;
            }
        });
        (port, rx)
    }

    #[tokio::test]
    async fn the_clash_calls_use_the_documented_methods_paths_and_bodies() {
        let (port, mut rx) = fake_clash(ClashMode::Ok).await;
        let c = Hy2ResiClient::with_endpoints(format!("127.0.0.1:{port}"), "127.0.0.1:1");

        assert!(c.ready().await);
        assert!(rx
            .recv()
            .await
            .unwrap()
            .starts_with("GET /version HTTP/1.1"));

        assert_eq!(
            c.connections().await.unwrap(),
            vec![Hy2ResiConn {
                id: "c1".into(),
                rule: "auth_user=r000 => route(gate-r000)".into(),
                chains: vec![],
            }]
        );
        assert!(rx
            .recv()
            .await
            .unwrap()
            .starts_with("GET /connections HTTP/1.1"));

        assert_eq!(
            c.selected_all().await.unwrap(),
            BTreeMap::from([("gate-r000".to_string(), "deny".to_string())])
        );
        assert!(rx
            .recv()
            .await
            .unwrap()
            .starts_with("GET /proxies HTTP/1.1"));

        c.select("gate-r000", "slot-0-out").await.unwrap();
        let req = rx.recv().await.unwrap();
        assert!(
            req.starts_with("PUT /proxies/gate-r000 HTTP/1.1"),
            "请求行不对：{req}"
        );
        assert!(
            req.ends_with(r#"{"name":"slot-0-out"}"#),
            "体必须是 {{\"name\":\"<成员 tag>\"}}：{req}"
        );

        c.close_connection("c1").await.unwrap();
        let req = rx.recv().await.unwrap();
        assert!(
            req.starts_with("DELETE /connections/c1 HTTP/1.1"),
            "请求行不对：{req}"
        );
        // 两个面都不设 secret（spec §2.3）⇒ 一条也不发 Authorization
        assert!(
            !req.to_ascii_lowercase().contains("authorization:"),
            "{req}"
        );
    }

    /// **非 2xx 一律是失败，绝不当成静默的成功。**
    ///
    /// 这是 T11 fail-closed 门位收敛唯一的判据：真 sing-box 在「切到非成员 tag」时回 400
    /// `Selector update error: not found`（`experimental/clashapi/proxies.go:178-184`）、
    /// 对未知 selector 回 404（`:50-53`）。把 `select` 的 `if !(200..300)` 删掉，就会把
    /// 「门根本没切动」当成切门成功 —— 到期 / 封禁用户静默 fail-open，正是 spec §3.4
    /// 禁止的那一种错。四处判据（`select` / `clash_get` / `close_connection` / `ready`）
    /// 都必须由客户端**真的**产出一次错误来证明，手工构造 `ClashError` 比字符串证明不了。
    #[tokio::test]
    async fn a_non_2xx_answer_is_always_a_failure_never_a_silent_ok() {
        // ① select 的 400：文案与 relay 那条路逐字一致（T13 的哨兵按它认）
        let (port, _rx) = fake_clash(ClashMode::Ok).await;
        let c = Hy2ResiClient::with_endpoints(format!("127.0.0.1:{port}"), "127.0.0.1:1");
        let err = c.select("gate-bad", "deny").await.unwrap_err().to_string();
        assert_eq!(err, "Clash API 拒绝切换到 deny（HTTP 400）");
        // ② DELETE 的 404 也是失败；措辞不冒用「拒绝切换」（当时没在切门）
        let err = c.close_connection("bad").await.unwrap_err().to_string();
        assert_eq!(err, "Clash API 不可达：DELETE /connections/bad 回 HTTP 404");
        // 正常路照旧成功，证明上面两条是状态码判出来的、不是路径写错
        c.select("gate-r000", "deny").await.unwrap();
        c.close_connection("c1").await.unwrap();

        // ③ 两个读接口（clash_get）的 500：整轮采样 / 收敛必须失败，不许回空表当真源
        let (port, _rx) = fake_clash(ClashMode::AllBad).await;
        let c = Hy2ResiClient::with_endpoints(format!("127.0.0.1:{port}"), "127.0.0.1:1");
        assert_eq!(
            c.connections().await.unwrap_err().to_string(),
            "Clash API 不可达：GET /connections 回 HTTP 500",
            "回空表就是「谁都不在线」，踢人与计量会一起打空"
        );
        assert_eq!(
            c.selected_all().await.unwrap_err().to_string(),
            "Clash API 不可达：GET /proxies 回 HTTP 500",
            "回空表会被收敛当成「全部门位都不对」而全员重切"
        );
        // ④ ready() 的 404：有人听但答非 2xx ⇒ 仍然不算就绪（重放要等真起来）
        assert!(
            !c.ready().await,
            "答了 404 也不算就绪：重放会在门位还没生效时以为自己成了"
        );
    }
}
