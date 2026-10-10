//! 住宅 HY2 的字节账本（独立计量模块，计划 §3）。**这是唯一一处把内核报来的字节变成
//! 用户账目的地方**：采样器只管按 10 秒一轮来 [`Ledger::drain`] 取走已确证字节，不关心
//! 字节是怎么来的。
//!
//! 为什么不能再用 `QueryStats`：官方 sing-box 发行物**永不含** `with_v2ray_api`
//! （17 个 tag 抽样 0 次），住宅计量的唯一官方面是 `daemon.StartedService` 的
//! `SubscribeConnections` 流。这条流的形状与坑都由 G0 决定性实验钉死（642 条连接逐连接
//! 对账、3 个会话、四寿命档 × 两档 tick），下面每条规则后面括号里是它的实测依据。
//!
//! ## 账本规则（计划 §3.2，逐条对应到实现）
//!
//! 1. **入账只用 `CLOSED` 事件的 `connection.uplinkTotal/downlinkTotal`**（642 条连接
//!    逐连接差额 **0**）。[`Ledger::apply`] 的 `Closed { conn: Some(c) }` 分支是唯一的
//!    `pending` 写入点。
//! 2. **绝不用 Σdelta 入账**（实测丢 6.8%~100%：UPDATE 不带 `user` 也不带总量，亚 tick
//!    连接根本不发 UPDATE）。`Update` 分支只累加进 `known`，那是给指标 A 的
//!    「未结算存量」用的，**不碰 `pending`、不碰 `booked_total`**。
//! 3. **按 `(实例代际, 连接 id)` 幂等 upsert**（`reset=true` 的首帧/重连快照带订阅**之前**
//!    就存在的连接及其累计量；同一连接两帧 CLOSED 实测相隔 114µs、字段全同）。
//!    代际来自 `GetStartedAt`，换代即清空 id 空间（[`Ledger::note_session_start`]）。
//!    **幂等只在窗口内承诺**：`DEDUP_TTL_MS`（120 s = 实测重投窗口 14–30 s 的 4 倍余量）
//!    之内同一 id 的 CLOSED 只记一次，窗口外再投递按「新连接」入账 —— 有界的近似幂等
//!    换掉无界的去重表（生产洪泛下内存无上界）。边界由
//!    `dedup_entries_expire_after_the_ttl` 钉死；容量挤掉**年轻**（仍在重投窗口内）条目
//!    的风险以「有损」进日志，挤掉年老条目只记 [`MeterStat::dedup_evicted`]。
//! 4. **绝不用 Σdelta 与 Σtotal 相加**（朴素相加过账 **1.96–1.97×**）：两者活在两个不同的
//!    账里，`known` 与 `booked_total` 从不相交。
//! 5. **亚 tick 连接必须入账**（40/40 sub-ms、60/60 ~1–2ms 的终值完整）：CLOSED 一律入账，
//!    不分寿命；`stat.sub_tick_conns` 只是让这件事在账面上看得见。
//! 6. **`UPDATE` 按 id 关联**，不得按 `source`/`destination`（UPDATE 不含 connection 对象，
//!    按五元组关联得 0）。
//! 7. **`conn=nil` 的 CLOSED 必须显式处理**（实测 14 条、39,168/41,472 字节无处登记）：
//!    它的字节进 `booked_total`（否则会被指标 A 当成丢失）但**不进任何人的账**，
//!    走 `stat.unnamed_closed` / `stat.unnamed_bytes` 可见。
//! 8. **不依赖 closed 环 / 快照做审计或补账**（GC 终结器随机清空，寿命 14–30s）：账本只认
//!    送到手上的 CLOSED 事件；快照只用来 upsert `open`（规则 3）。
//! 9. **实例代际用 `GetStartedAt`**：换代即封账（`mark = None`）+ 记一条「可疑损失」观察。
//! 10. **UDP 计费口径 = 账本净负载**（不可配置的产品决策，见下）。
//! 11. **归属键 = `connection.user`**（服务端 `users[].name`），出账时由采样器经
//!     `traffic::resi_name_to_user` 映射到 `user_id`；本模块不认识 `Uuid`。
//!
//! ## 三指标
//!
//! **A（主指标）**：`R = ΔG − ΔB − ΔS`，其中 ΔG 是 `SubscribeStatus` 全局总量的区间差分、
//! ΔB 是本区间入账的终值、ΔS 是在册未关闭连接的 `known` 之和的区间差分。
//!
//! R 的身份是**跟踪滞后之差**：把每条连接的真实总量记作 `true`、账本记作 `known`，
//! 滞后 `L = Σ_open (true − known)`，那么 `R = L2 − L1`。健康（快消费）时 L 恒定 ⇒ **R = 0**
//! （实测快消费者两轮 byte-exact：全局 325,888/1,188,352 == 账本值）；发生满丢后缀时
//! L 突增 ⇒ R ≈ 丢失的字节（实测缺口 273,920/1,095,680 = **84–92%**）。
//! 计划 §3.3 的公式（2026-10-10 修订版）与本实现逐项同形：ΔG/ΔB/ΔS 三个差分缺一不可
//! —— 初版 `ΔG − Σ(区间内 CLOSED 终值)` 漏掉 ΔS，会把「区间内仍在传输的连接」整体算成
//! 缺口，健康区间也报巨额缺口。
//!
//! 阈值：`R > 64 KiB` 记观察；**`R ≥ 1 MiB` 或 `R ≥ 区间字节（ΔG）的 5%`（先触发者）**
//! 判「后缀丢失」⇒ 告警 + 该区间**标损** + 暂停该窗口的自动裁决（见下）+ 请求重订阅一次。
//!
//! **B（世代边界）**：流终止（非 OK gRPC 码 / 流结束）与代际变化都封账，并把
//! `[最后一 tick, 流终止]` 标「可疑损失」（预期 **≤384 B @1s tick**：250ms tick 丢 0 B）。
//!
//! **C（配对完整性）**：`stat.new` / `stat.closed` / `stat.orphan_closed`（没配到 NEW 的
//! CLOSED）/ `stat.dup_closed`（重复 CLOSED，幂等吸收）。已交付基线是 0 条半对。
//!
//! ## 拒绝语义（计划 §5，分层）
//!
//! - **用户层 fail-open**：只按确证字节计费，**不因账目不确定封人**。落到实现是
//!   [`super::users::blocked_set`] 在有损窗口里**不拿未落盘的 `pending` 参与判定**
//!   （那些字节是本区间里「可能不完整」的部分），已经确证并落盘的用量照常判定 ——
//!   否则一个已经超额的住宅用户会因为一次计量故障被**解封**，那比少封更糟。
//! - **系统层 fail-closed**：指标 A/B/C 越线即 [`Ledger::lossy_at`] 为真 + 一条带
//!   [`METERING_MARKER`] 与 [`METERING_LOSS_TAG`] 的 `warn` 日志（`sentinel` 的
//!   `hy2_resi_metering_lost` 签名按这两个词认领）。**不许静默**。
//!
//! ## 两条口径（产品决策，不做可配置）
//!
//! - **UDP**：账本记的是**净负载**（`connection` 的 `uplinkTotal/downlinkTotal`），与客户端
//!   封装口径恒差 **10 B/报文/方向（IPv4）/ 22 B（IPv6）**，按包数线性放大。混用即系统性
//!   偏差，所以**只认这一个口径**，报文头/开销不计。`stat.udp_bytes` 让这条口径在账面上可见。
//! - **版本探针**：hysteria 客户端自带的 `api.hy2.io:443` 探针**真实过代理并归到用户头上**
//!   （实测）。本模块**计入账单**（下游无法区分它与真实流量，按目标排除等于替客户端做
//!   策略），但单独计数（`stat.probe_conns` / `probe_bytes`），账单口径里写明它包含在内。
//!
//! ## 盲区（写下来，不装作没有）
//!
//! - R 只在**缺口发生的那一个区间**报警。若丢失的尾巴恰好只含某条连接的 CLOSED、且其后
//!   再无流量，那条连接会永远停在 `open` 里（字节既不入账也不报警）。它由
//!   [`MeterView::open_conns`] 与 `stat.daemon_connections_in` 的对照可见（在册连接数
//!   长期高于内核报的活动连接数 = 有已关闭未结算的连接），**本模块不对这个比较报警** ——
//!   `connectionsIn` 的语义没有实测背书，宁可只展示。
//! - 账本在内存里：`b-ui` 自己重启会丢掉在册未结算的连接（与 sing-box 重启同样的窗口，
//!   这里不做补偿，与 `traffic.rs` 对直连 hysteria 计数器重启的口径一致）。

use super::TxRx;
use serde::Serialize;
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;
use time::OffsetDateTime;

/// 日志标记：计量相关的行都带它（`sentinel::signature` 按它 + [`METERING_LOSS_TAG`] 认领）。
pub const METERING_MARKER: &str = "hy2-metering";
/// 「这一条是有损告警」的标记词。观察性的行（换代、恢复、阈值以下）**不带**它，
/// 否则哨兵会把良性的观察行也报成事件。
pub const METERING_LOSS_TAG: &str = "有损";

/// 已入账 id 的去重窗口（实测重复 CLOSED 两帧相隔 114µs；120 秒是三个数量级的余量）
pub const DEDUP_TTL_MS: i64 = 120_000;
/// 「还在可能被重投的窗口里」的判据：实测 closed 环/退避补投最远 14–30 s。
/// 容量淘汰只对**比它年轻**的条目才判有损（那些 id 的重复 CLOSED 还能再投回来）。
pub const REFILL_WINDOW_MS: i64 = 30_000;
/// 去重表容量上限（≈ 273 条/秒持续 120 s；1000 条/秒的洪泛也能盖住 30 s 补投窗口）。
/// 超了按入账时间淘汰最旧的：淘汰**年轻**条目（年龄 < [`REFILL_WINDOW_MS`]）意味着
/// 同一 id 的 CLOSED 若再投递会重复记账 ⇒ 有损告警；淘汰年老条目只是内存保护。
pub const DEDUP_CAP: usize = 32_768;
/// 指标 A 的观察门槛
pub const OBSERVE_BYTES: i64 = 64 * 1024;
/// 指标 A 的告警门槛（绝对量）
pub const ALERT_BYTES: i64 = 1024 * 1024;
/// 指标 A 的告警门槛（区间字节占比，%）。两个门槛**先触发者**生效。
pub const ALERT_PCT: i64 = 5;
/// `lossy` 单次触发后的抑制时长（每次再触发就再延长）
pub const LOSSY_HOLD_MS: i64 = 30 * 60 * 1000;
/// `lossy` 的绝对上限：`since` 之后最多抑制这么久，到期必须吐一条恢复告警
pub const LOSSY_MAX_MS: i64 = 6 * 60 * 60 * 1000;
/// 「流还在」的判据：这么久没有收到任何一帧就当成卡死（订阅间隔默认 1 秒）
pub const STREAM_STALE_MS: i64 = 15_000;
/// 中断计数的滚动窗口与门槛（10 分钟内 ≥5 次中断 ⇒ 有损）
pub const INTERRUPT_WINDOW_MS: i64 = 600_000;
pub const INTERRUPT_LIMIT: usize = 5;
/// 亚 tick 的判据（连接寿命 < 1 秒）。**只用来说明**：亚 tick 连接照样入账。
pub const SUB_TICK_MS: i64 = 1_000;
/// 会话持续超过这么久 ⇒ 退避重置回最小值
pub const SESSION_RESET_AFTER: Duration = Duration::from_secs(30);
/// 重连退避
pub const BACKOFF_MIN: Duration = Duration::from_secs(1);
pub const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// 保留多少条最近的告警给面板/`bui status` 看
pub const ALERTS_KEPT: usize = 16;
/// hysteria 客户端自带的版本探针目标（计划 §3.4）。**逐字节精确**匹配 host 与 port。
pub const PROBE_HOST: &str = "api.hy2.io";
pub const PROBE_PORT: u16 = 443;

/// 现在（Unix 毫秒）。账本的所有时间参数都由调用方传入，只有后台循环用它。
pub fn now_ms() -> i64 {
    ms_of(OffsetDateTime::now_utc())
}

/// `OffsetDateTime` → Unix 毫秒（采样器手里只有 `OffsetDateTime`，别再抄一遍换算）。
pub fn ms_of(t: OffsetDateTime) -> i64 {
    (t.unix_timestamp_nanos() / 1_000_000) as i64
}

fn rfc3339(ms: i64) -> Option<String> {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .ok()
        .map(crate::util::fmt_rfc3339)
}

/// 一条要落日志的告警。`lossy = true` 的行带 [`METERING_LOSS_TAG`]（哨兵认领），
/// 其余的只是观察。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Alert {
    pub lossy: bool,
    pub text: String,
}

/// 一条已关闭的连接（`CLOSED` 事件里 `connection` 非空的那一支）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosedConn {
    /// `connection.user`（服务端 `users[].name`）。空串按「没有」处理。
    pub user: Option<String>,
    pub destination: Option<String>,
    pub network: Option<String>,
    /// **终值**：入账的唯一来源。
    pub total: TxRx,
    pub created_at_ms: i64,
    pub closed_at_ms: i64,
}

impl ClosedConn {
    fn lifetime_ms(&self) -> i64 {
        if self.created_at_ms > 0 && self.closed_at_ms >= self.created_at_ms {
            self.closed_at_ms - self.created_at_ms
        } else {
            i64::MAX
        }
    }

    fn udp(&self) -> bool {
        self.network.as_deref() == Some("udp")
    }
}

/// 事件。`Update` 只有增量（实测它不带 `connection` 对象），`Closed` 可能没有连接对象。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventKind {
    /// `NEW`：带创建时的快照总量（通常 0）。`reset` 快照也走这里 —— 一律 **upsert**。
    /// **不带 user**：归属只在 CLOSED 从 `conn.user` 取（规则 11），NEW 的 user 无人读
    /// （带它会让 `Open` 不是 `Copy`、也让「归属键只有一处」这条线多一个岔口）。
    New { total: TxRx },
    /// `UPDATE`：只有增量。**只累加进 `known`**（服务指标 A 的 ΔS），绝不入账。
    Update { delta: TxRv },
    /// `CLOSED`：`conn = None` 是实测 14 条的那一类（字节无处登记，必须显式处理）。
    Closed { conn: Option<ClosedConn> },
}

/// `Update` 的增量对（与 [`TxRx`] 同构；单独起名是为了让「这里只可能是增量」写进类型里）
pub type TxRv = TxRx;

/// 一帧（`SubscribeConnections` 的一次投递，或一次 `SubscribeStatus` 的全局量）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frame {
    /// `ConnectionEvents.reset`：首帧/重连快照。**按 id upsert，快照 totals 绝不累加。**
    pub reset: bool,
    /// 最近一次 `SubscribeStatus` 的 `uplinkTotal + downlinkTotal`（指标 A 的 G）。
    /// `None` = 这一帧不含全局量；`trafficAvailable = false` 时也是 `None`。
    pub global: Option<TxRx>,
    /// 内核报的**当前活动连接数**（`Status.connectionsIn`）。只展示，不参与判定。
    pub connections_in: Option<u64>,
    pub events: Vec<Event>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub id: String,
    pub kind: EventKind,
}

/// 账本的可见统计（`/api/users/health` 的 `metering` 块、`bui incidents`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct MeterStat {
    pub frames: u64,
    pub resets: u64,
    pub events: u64,
    pub new: u64,
    pub update: u64,
    pub closed: u64,
    /// 重复 CLOSED（幂等吸收。实测两帧相隔 114µs、字段全同）
    pub dup_closed: u64,
    /// 没配到 NEW 的 CLOSED（成对率 = 1 − orphan/closed）
    pub orphan_closed: u64,
    /// `conn = nil` 的 CLOSED（实测 14 条）
    pub unnamed_closed: u64,
    pub unnamed_bytes: u64,
    /// 有 `user` 但归不出去... 不：这里是**没有 user** 的 CLOSED 字节（与 unnamed 分立）
    pub unattributed_bytes: u64,
    /// 已结账的连接数 / 字节（含无名与探针）
    pub booked_conns: u64,
    pub booked_bytes: u64,
    pub sub_tick_conns: u64,
    pub udp_bytes: u64,
    pub probe_conns: u64,
    pub probe_bytes: u64,
    pub interrupted: u64,
    pub drains: u64,
    /// 最近一个区间的三个差分与缺口（i64 饱和）
    pub last_global_delta: i64,
    pub last_booked_delta: i64,
    pub last_span_delta: i64,
    pub last_gap: i64,
    pub last_gap_pct: i64,
    /// 区间缺口 > 观察门槛的次数 / 判成「后缀丢失」的次数
    pub observed_intervals: u64,
    pub lossy_intervals: u64,
    pub dedup_evicted: u64,
}

/// 面板/`bui status` 看的账本视图。
#[derive(Debug, Clone, Serialize)]
pub struct MeterView {
    pub generation: Option<i64>,
    pub available: bool,
    pub lossy: bool,
    pub lossy_since: Option<String>,
    pub lossy_until: Option<String>,
    pub last_frame_at: Option<String>,
    pub open_conns: u64,
    pub daemon_connections_in: Option<u64>,
    pub pending_creds: u64,
    pub booked: TxRx,
    pub stat: MeterStat,
    pub alerts: Vec<Alert>,
}

#[derive(Debug, Clone, Copy, Default)]
struct Open {
    /// 账本跟踪到的总量：NEW 快照赋值、UPDATE 增量累加。**只服务指标 A 的 ΔS。**
    known: TxRx,
}

#[derive(Debug, Clone, Copy)]
struct Mark {
    generation: Option<i64>,
    /// 这个基准里的 `global` 是否**真的被喂过**（收到过至少一帧带 global 的帧）。
    /// 没喂过时它还是初值 0，拿它当基准会把内核的历史总量整体算成本区间的 ΔG。
    primed: bool,
    global: TxRx,
    booked: TxRx,
    span: TxRx,
}

#[derive(Debug, Default)]
struct Inner {
    generation: Option<i64>,
    open: BTreeMap<String, Open>,
    /// 已入账 id → 入账时刻（去重，规则 3）
    dedup: BTreeMap<String, i64>,
    pending: BTreeMap<String, TxRx>,
    booked_total: TxRx,
    global: TxRx,
    /// `global` 是否被某一帧真的赋过值（指标 A 的基准两端都必须为真才比较）
    global_primed: bool,
    connections_in: Option<u64>,
    mark: Option<Mark>,
    stat: MeterStat,
    lossy_since_ms: Option<i64>,
    lossy_until_ms: i64,
    interrupts: Vec<i64>,
    interrupted_at_ms: Option<i64>,
    last_frame_ms: Option<i64>,
    alerts: VecDeque<Alert>,
    /// 本有损期是否**已经宣告过**（`enter_lossy` 的去重判据，[`Inner::expire_lossy`] 复位）。
    /// 不能用「环里最后一条是不是有损」判：告警统一出口之后，观察行（非有损）也会入环，
    /// 那条判据会因为中间夹了一条观察行而误判成「还没宣告」，把同一次有损重报一遍。
    lossy_announced: bool,
    resubscribe: bool,
}

impl Inner {
    /// 在册未关闭连接的 `known` 之和（指标 A 的 ΔS 用）
    fn span(&self) -> TxRx {
        let mut out = TxRx::default();
        for c in self.open.values() {
            out.add(c.known);
        }
        out
    }

    fn push_alert(&mut self, lossy: bool, text: String) {
        self.alerts.push_back(Alert { lossy, text });
        while self.alerts.len() > ALERTS_KEPT {
            self.alerts.pop_front();
        }
    }

    fn lossy_alive(&self, now_ms: i64) -> bool {
        self.lossy_since_ms.is_some() && now_ms < self.lossy_until_ms
    }

    /// 进入有损状态。**返回真的新推了这一条（不是被抑制）的告警**；抑制期内返回 `None`
    /// （同一次有损只宣告一次，不刷屏）。返回的告警由调用方并入本轮告警、
    /// 经 [`Ledger::publish_alerts`] 统一入环 + 落日志 —— 这里**不写环**，写了就会重复入环。
    fn enter_lossy(&mut self, now_ms: i64, text: String) -> Option<Alert> {
        let expired = now_ms >= self.lossy_until_ms;
        self.lossy_since_ms = self.lossy_since_ms.or(Some(now_ms));
        let since = self.lossy_since_ms.unwrap_or(now_ms);
        let until = (now_ms + LOSSY_HOLD_MS).min(since + LOSSY_MAX_MS);
        self.lossy_until_ms = self.lossy_until_ms.max(until);
        if expired || !self.lossy_announced {
            self.lossy_announced = true;
            return Some(Alert { lossy: true, text });
        }
        None
    }

    /// 抑制到期就清掉并吐一条恢复告警（只在会写状态的入口里跑，别放读路径）。
    /// 恢复行同样经 [`Ledger::publish_alerts`] 出入境，与其它告警一条出口。
    fn expire_lossy(&mut self, now_ms: i64, alerts: &mut Vec<Alert>) {
        if self.lossy_since_ms.is_some() && now_ms >= self.lossy_until_ms {
            let ms = self.lossy_until_ms - self.lossy_since_ms.unwrap_or(0);
            self.lossy_since_ms = None;
            self.lossy_until_ms = 0;
            self.lossy_announced = false;
            alerts.push(Alert {
                lossy: false,
                text: format!(
                    "计量已恢复：{} 分钟的抑制窗口走完，自动封禁/计费裁决恢复（用户层全程只按确证字节计费）",
                    ms / 60_000
                ),
            });
        }
    }

    /// 去重表 TTL + 容量。**逐帧调用一次**（不是逐事件：一帧可带成千上万事件，逐事件调用
    /// 会把 TTL 扫描做成 N²）。TTL 过期是正常老化，不提；容量淘汰**年轻**条目（年龄 <
    /// [`REFILL_WINDOW_MS`]、重投还可能回来）才判有损 —— 那会让同一 CLOSED 重复入账。
    fn prune_dedup(&mut self, now_ms: i64, alerts: &mut Vec<Alert>) {
        self.dedup
            .retain(|_, at| now_ms.saturating_sub(*at) <= DEDUP_TTL_MS);
        if self.dedup.len() > DEDUP_CAP {
            let mut by_age: Vec<(i64, String)> = self
                .dedup
                .iter()
                .map(|(id, at)| (*at, id.clone()))
                .collect();
            by_age.sort_unstable();
            let drop_n = by_age.len() - DEDUP_CAP;
            let mut young = 0u64;
            for (at, id) in by_age.into_iter().take(drop_n) {
                self.dedup.remove(&id);
                self.stat.dedup_evicted += 1;
                if now_ms.saturating_sub(at) < REFILL_WINDOW_MS {
                    young += 1;
                }
            }
            if young > 0 {
                let text = format!(
                    "去重表容量（{DEDUP_CAP}）挤掉了 {young} 条仍在 {REFILL_WINDOW_MS} ms 重投窗口内的 id：\
                     这些 CLOSED 再被投递会重复入账，标损并只按确证字节裁决"
                );
                if let Some(a) = self.enter_lossy(now_ms, text) {
                    alerts.push(a);
                }
            }
        }
    }

    fn book(&mut self, id: &str, now_ms: i64) {
        self.dedup.insert(id.to_string(), now_ms);
    }
}

/// 字节账本。**std 锁、绝不跨 await 持有**（`apply`/`drain` 都是同步的）。
#[derive(Debug, Default)]
pub struct Ledger {
    inner: std::sync::Mutex<Inner>,
}

impl Ledger {
    pub fn new() -> Self {
        Self::default()
    }

    /// 按值转 `Arc`。名字用 `into_` 而非 `as_`：它**消费** self，
    /// clippy 的 `wrong_self_convention` 对此有硬要求（`as_*` 只能借 self）。
    pub fn into_arc(self) -> Arc<Self> {
        Arc::new(self)
    }

    /// 告警的**唯一出口**：入环 + 落日志一并做，两个动作永远同进同出。
    ///
    /// 此前是三条并行写法（`enter_lossy` 自己写环、`note_session_start` 在锁里补写、
    /// 其余入口只 `log_alerts`），于是 `drain` 产出的观察行只落日志、不进环 ——
    /// 「最近的告警」按入口不同给两份答案（`the_observe_line_alerts_without_marking_lossy` 抓到）。
    /// 现在所有产出告警的入口都攒进 `alerts` 再调这里，环与日志逐条对齐、且各一条。
    pub fn publish_alerts(&self, alerts: &[Alert]) {
        if alerts.is_empty() {
            return;
        }
        {
            let mut g = self.inner.lock().unwrap();
            for a in alerts {
                g.push_alert(a.lossy, a.text.clone());
            }
        }
        log_alerts(alerts);
    }

    /// 吸收一帧。规则 1/2/3/6/7 全在这里。
    pub fn apply(&self, frame: Frame, now_ms: i64) {
        let mut alerts: Vec<Alert> = Vec::new();
        {
            let mut g = self.inner.lock().unwrap();
            g.expire_lossy(now_ms, &mut alerts);
            g.stat.frames += 1;
            if frame.reset {
                g.stat.resets += 1;
            }
            if let Some(global) = frame.global {
                g.global = global;
                // 指标 A 的基准两端都必须是「真的被喂过」的值：没喂过时 `global` 还是
                // 初值 0，拿它当基准会把内核的历史总量整体算成这一区间的 ΔG（缺陷①）。
                g.global_primed = true;
            }
            if let Some(n) = frame.connections_in {
                g.connections_in = Some(n);
            }
            g.last_frame_ms = Some(now_ms);
            g.interrupted_at_ms = None;
            for ev in frame.events {
                g.stat.events += 1;
                let booked = g.dedup.contains_key(&ev.id);
                match ev.kind {
                    EventKind::New { total } => {
                        g.stat.new += 1;
                        // 已结账的 id 再出现（重连快照/环回放）：不许重开，否则它的 CLOSED
                        // 会被去重吃掉、而 open 里留一条永不结算的幽灵
                        if booked {
                            continue;
                        }
                        // upsert：快照总量**赋值**，绝不累加（规则 3）
                        g.open.insert(ev.id, Open { known: total });
                    }
                    EventKind::Update { delta } => {
                        g.stat.update += 1;
                        if booked {
                            continue;
                        }
                        // 按 id 关联（规则 6）。没见过的 id 也建册：它的总量只服务 ΔS
                        let entry = g.open.entry(ev.id).or_default();
                        entry.known.add(delta);
                    }
                    EventKind::Closed { conn } => g.close(&ev.id, conn, now_ms, &mut alerts),
                }
            }
            // 去重表逐帧维护一次（逐事件调用是 N²）；容量淘汰的告警也走 `alerts`
            g.prune_dedup(now_ms, &mut alerts);
        }
        self.publish_alerts(&alerts);
    }

    /// 排空自上次调用以来已确证的每用户字节 —— **采样器唯一的入口**（键 = 凭据 name）。
    ///
    /// 同时是**区间边界**：指标 A 在这里算（`R = ΔG − ΔB − ΔS`）。中断/换代之后
    /// 不再硬算（`mark = None`），符合指标 B「跨边界只标可疑损失、不假装精确」。
    pub fn drain(&self, now_ms: i64) -> BTreeMap<String, TxRx> {
        let mut alerts: Vec<Alert> = Vec::new();
        let pending = {
            let mut g = self.inner.lock().unwrap();
            g.expire_lossy(now_ms, &mut alerts);
            g.stat.drains += 1;
            let taken = std::mem::take(&mut g.pending);
            let span = g.span();
            let new_mark = Mark {
                generation: g.generation,
                primed: g.global_primed,
                global: g.global,
                booked: g.booked_total,
                span,
            };
            // 基准两端都必须真的被喂过全局量才比较（缺陷①：首个区间没喂过时，
            // 内核的历史总量会被整段算成 ΔG）
            match g.mark.take() {
                Some(prev) if prev.generation == g.generation && prev.primed && g.global_primed => {
                    let dg = diff(new_mark.global, prev.global);
                    let db = diff(new_mark.booked, prev.booked);
                    let ds = diff(new_mark.span, prev.span);
                    let gap = dg - db - ds;
                    let pct = if dg > 0 { gap * 100 / dg } else { 0 };
                    g.stat.last_global_delta = dg;
                    g.stat.last_booked_delta = db;
                    g.stat.last_span_delta = ds;
                    g.stat.last_gap = gap;
                    g.stat.last_gap_pct = pct;
                    if gap > OBSERVE_BYTES {
                        g.stat.observed_intervals += 1;
                        alerts.push(Alert {
                            lossy: false,
                            text: format!(
                                "区间缺口 {gap} B（全局 {dg} B 的 {pct}%）：超过 {OBSERVE_BYTES} B 观察线，继续盯下一区间"
                            ),
                        });
                    }
                    if gap >= ALERT_BYTES || (dg > 0 && pct >= ALERT_PCT) {
                        g.stat.lossy_intervals += 1;
                        // 重订阅一次：快照能补 14–30s 内的已关闭连接（按 id 去重，重收不重计）
                        g.resubscribe = true;
                        let text = format!(
                            "后缀丢失：本区间 {gap} B 未确证（全局 {dg} B 的 {pct}%，阈值 {ALERT_BYTES} B 或 {ALERT_PCT}%），\
                             已标损且不折算到人；本窗口自动封禁/计费裁决只用确证字节，并重订阅一次兜底"
                        );
                        // 真的新触发的有损必须经 `publish_alerts` 出境（§5「不得静默」）
                        if let Some(a) = g.enter_lossy(now_ms, text) {
                            alerts.push(a);
                        }
                    }
                }
                _ => {}
            }
            if g.interrupted_at_ms.is_none() {
                g.mark = Some(new_mark);
            } else {
                // 中断期间不建立比较基准（那段区间没法解释成「滞后」）
                g.mark = None;
            }
            taken
        };
        self.publish_alerts(&alerts);
        pending
    }

    /// 一条新会话开始了（`meter_session` 每次建流时调）。
    pub fn note_session_start(&self, generation: i64, now_ms: i64) {
        let mut alerts: Vec<Alert> = Vec::new();
        {
            let mut g = self.inner.lock().unwrap();
            let previous = g.generation;
            if previous != Some(generation) {
                if let Some(old) = previous {
                    alerts.push(Alert {
                        lossy: false,
                        text: format!(
                            "实例代际变化（GetStartedAt {old} → {generation}）：封账；上一代最后一 tick 到流终止之间的字节按「可疑损失」记（预期 ≤384 B @1s tick）"
                        ),
                    });
                }
                g.generation = Some(generation);
                // 代际变了 ⇒ id 空间重置（规则 3 的键是 (代际, id)），在册与去重都作废
                g.open.clear();
                g.dedup.clear();
                // 新实例的全局量从头开始：旧实例的 `global` 还在手里，绝不能再当基准
                g.global_primed = false;
            }
            // 新流的第一帧是 reset 快照 ⇒ 在册总量会被重新赋值，跨它算 A 没有意义
            g.mark = None;
            g.interrupted_at_ms = None;
            g.last_frame_ms = Some(now_ms);
        }
        self.publish_alerts(&alerts);
    }

    /// 流断了（非 OK gRPC 码 / 流结束 / 请求重订阅之外的任何收场）。
    pub fn note_interruption(&self, now_ms: i64, reason: &str) {
        let mut alerts: Vec<Alert> = Vec::new();
        {
            let mut g = self.inner.lock().unwrap();
            g.expire_lossy(now_ms, &mut alerts);
            g.interrupted_at_ms = Some(now_ms);
            g.mark = None;
            g.stat.interrupted += 1;
            g.interrupts.push(now_ms);
            g.interrupts
                .retain(|at| now_ms.saturating_sub(*at) <= INTERRUPT_WINDOW_MS);
            let open = g.open.len();
            alerts.push(Alert {
                lossy: false,
                text: format!(
                    "计量流中断（{reason}）：封账并标「可疑损失」（[最后一 tick, 流终止]，预期 ≤384 B @1s tick）；\
                     中断时在册 {open} 条连接的终值要等重连快照或下一次 CLOSED"
                ),
            });
            let recent = g.interrupts.len();
            if recent >= INTERRUPT_LIMIT {
                let text = format!(
                    "{INTERRUPT_WINDOW_MS} ms 内已中断 {recent} 次（线是 {INTERRUPT_LIMIT} 次）：\
                     判「后缀丢失」，标损并只按确证字节裁决"
                );
                if let Some(a) = g.enter_lossy(now_ms, text) {
                    alerts.push(a);
                }
            }
        }
        self.publish_alerts(&alerts);
    }

    /// 账本要不要请上层重订阅一次（指标 A 越线时置位，取走即清）。
    pub fn take_resubscribe(&self) -> bool {
        std::mem::take(&mut self.inner.lock().unwrap().resubscribe)
    }

    /// 计量此刻**可用**吗：既没在中断里，也没有超过 [`STREAM_STALE_MS`] 没有收到帧。
    ///
    /// `query_user_deltas` 用它区分「这轮真的没有字节」与「流没了」——后者必须是 `Err`，
    /// 否则采样器看不到、面板看不到、哨兵也看不到（审计核实的三条静默失效链之一）。
    pub fn available(&self, now_ms: i64) -> bool {
        let g = self.inner.lock().unwrap();
        g.interrupted_at_ms.is_none()
            && g.last_frame_ms
                .is_some_and(|at| now_ms.saturating_sub(at) <= STREAM_STALE_MS)
    }

    /// 这一区间要不要暂停自动裁决（`blocked_set` 的口径见模块文档）。
    pub fn lossy_at(&self, now_ms: i64) -> bool {
        self.inner.lock().unwrap().lossy_alive(now_ms)
    }

    pub fn view(&self, now_ms: i64) -> MeterView {
        let g = self.inner.lock().unwrap();
        MeterView {
            generation: g.generation,
            available: g.interrupted_at_ms.is_none()
                && g.last_frame_ms
                    .is_some_and(|at| now_ms.saturating_sub(at) <= STREAM_STALE_MS),
            lossy: g.lossy_alive(now_ms),
            lossy_since: g.lossy_since_ms.and_then(rfc3339),
            lossy_until: (g.lossy_until_ms > 0)
                .then(|| rfc3339(g.lossy_until_ms))
                .flatten(),
            last_frame_at: g.last_frame_ms.and_then(rfc3339),
            open_conns: g.open.len() as u64,
            daemon_connections_in: g.connections_in,
            pending_creds: g.pending.len() as u64,
            booked: g.booked_total,
            stat: g.stat,
            alerts: g.alerts.iter().cloned().collect(),
        }
    }

    pub fn recent_alerts(&self) -> Vec<Alert> {
        self.inner.lock().unwrap().alerts.iter().cloned().collect()
    }
}

impl Inner {
    /// `CLOSED` 的入账（规则 1/2/7/11 + 指标 C 的孤儿计数）。
    fn close(&mut self, id: &str, conn: Option<ClosedConn>, now_ms: i64, alerts: &mut Vec<Alert>) {
        self.stat.closed += 1;
        if self.dedup.contains_key(id) {
            self.stat.dup_closed += 1;
            return;
        }
        // 指标 C：没配到 NEW 的 CLOSED（在册里没有这个 id）—— 这里是唯一的自增点。
        // `conn = nil` 的那一类也走这条路，但两个统计各自成立、不混：orphan 记的是
        // 配对完整性，unnamed 记的是有没有 connection 对象。
        let known = match self.open.remove(id) {
            Some(c) => c.known,
            None => {
                self.stat.orphan_closed += 1;
                TxRx::default()
            }
        };
        let has_conn = conn.is_some();
        let (total, user, udp, probe, sub_tick) = match conn {
            Some(c) => {
                let probe = c.destination.as_deref().is_some_and(is_probe_destination);
                let user = non_empty(c.user.clone());
                (c.total, user, c.udp(), probe, c.lifetime_ms() < SUB_TICK_MS)
            }
            // 规则 7：conn=nil 的 CLOSED。字节进总账（否则指标 A 会把它算成丢失），
            // 但不进任何人的账 —— 归不了户的字节宁可标损，不许猜。
            None => {
                self.stat.unnamed_closed += 1;
                self.stat.unnamed_bytes = self.stat.unnamed_bytes.saturating_add(known.total());
                (known, None, false, false, false)
            }
        };
        if user.is_none() && !total.is_zero() {
            self.stat.unattributed_bytes =
                self.stat.unattributed_bytes.saturating_add(total.total());
            // §5「不得静默」：有字节、没归属 —— 字节进了总账（指标 A 不会把它算成丢失），
            // 但不折算到人；这件事必须在 tracing 里看得见（哨兵按 METERING_MARKER 认领）
            alerts.push(Alert {
                lossy: false,
                text: format!(
                    "有 {} B 的 CLOSED 没有归属用户（{}）：只进总账、不折算到人",
                    total.total(),
                    if has_conn {
                        "connection 在但 user 为空"
                    } else {
                        "connection 缺失（规则 7）"
                    }
                ),
            });
        }
        if let Some(u) = &user {
            self.pending.entry(u.clone()).or_default().add(total);
        }
        self.booked_total.add(total);
        self.stat.booked_conns += 1;
        self.stat.booked_bytes = self.stat.booked_bytes.saturating_add(total.total());
        if udp {
            self.stat.udp_bytes = self.stat.udp_bytes.saturating_add(total.total());
        }
        if probe {
            self.stat.probe_conns += 1;
            self.stat.probe_bytes = self.stat.probe_bytes.saturating_add(total.total());
        }
        if sub_tick {
            self.stat.sub_tick_conns += 1;
        }
        self.book(id, now_ms);
    }
}

fn non_empty(s: Option<String>) -> Option<String> {
    s.filter(|s| !s.is_empty())
}

fn diff(now: TxRx, before: TxRx) -> i64 {
    let now = i128::from(now.total());
    let before = i128::from(before.total());
    (now - before).clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

/// 这个目的地址是不是 hysteria 客户端的版本探针（[`PROBE_HOST`] + [`PROBE_PORT`]，
/// host 与 port **逐字节精确**，不做后缀/包含匹配 —— 免得出一个 `xapi.hy2.io` 就被漏算）。
pub fn is_probe_destination(destination: &str) -> bool {
    match destination.rsplit_once(':') {
        Some((host, port)) => host == PROBE_HOST && port == PROBE_PORT.to_string(),
        None => false,
    }
}

/// 把告警推到日志（`tracing` 是**唯一**的出口：面板/哨兵/`bui incidents` 都从它派生）。
pub fn log_alerts(alerts: &[Alert]) {
    for a in alerts {
        if a.lossy {
            tracing::warn!("{} {}：{}", METERING_MARKER, METERING_LOSS_TAG, a.text);
        } else {
            tracing::warn!("{} 观察：{}", METERING_MARKER, a.text);
        }
    }
}

use std::future::Future;

/// 计量后台循环（`PanelModule::spawn` 的第 5 个任务）：一条长连接 + 退避重连。
///
/// 它只管**会话**（建流 / 断流 / 退避），一帧都不解释 —— 帧的语义全在 [`Ledger::apply`]。
/// 会话持续超过 [`SESSION_RESET_AFTER`] 就把退避重置回最小值（长期健康之后的一次重启不该
/// 从 30 秒开始等）。
///
/// **中断必须记进真账本**（就是采样器/面板读的那一个），否则指标 B「10 分钟 5 次中断 ⇒
/// 有损」永远不会在真实状态上发生。第二段接线时的形态：
/// `spawn(metering_loop(ledger.clone(), move || feed_session(ledger.clone(), &client)))`——
/// `feed_session` 负责 `note_session_start` + 逐帧 `apply` + `take_resubscribe`。
pub async fn metering_loop<F, Fut>(ledger: Arc<Ledger>, session: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    let mut backoff = BACKOFF_MIN;
    loop {
        let started = std::time::Instant::now();
        let outcome = session().await;
        let now = now_ms();
        let reason = match outcome {
            Ok(()) => "流正常收尾".to_string(),
            Err(e) => crate::redact::line(&format!("{e:#}")),
        };
        ledger.note_interruption(now, &reason);
        if started.elapsed() >= SESSION_RESET_AFTER {
            backoff = BACKOFF_MIN;
        } else {
            backoff = (backoff * 2).min(BACKOFF_MAX);
        }
        tokio::time::sleep(backoff).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// 帧构造器：只填事件
    fn ev(id: &str, kind: EventKind) -> Frame {
        Frame {
            reset: false,
            global: None,
            connections_in: None,
            events: vec![Event {
                id: id.into(),
                kind,
            }],
        }
    }

    fn txrx(tx: u64, rx: u64) -> TxRx {
        TxRx { tx, rx }
    }

    /// `NEW` 不带 user（归属只在 CLOSED 从 `conn.user` 取，规则 11）
    fn new(total: TxRx) -> EventKind {
        EventKind::New { total }
    }

    fn update(tx: u64, rx: u64) -> EventKind {
        EventKind::Update {
            delta: txrx(tx, rx),
        }
    }

    fn closed(user: &str, total: TxRx, created: i64, closed: i64) -> EventKind {
        EventKind::Closed {
            conn: Some(ClosedConn {
                user: Some(user.into()),
                destination: Some("example.com:443".into()),
                network: Some("tcp".into()),
                total,
                created_at_ms: created,
                closed_at_ms: closed,
            }),
        }
    }

    /// 全局量帧（`SubscribeStatus`）
    fn status(tx: u64, rx: u64) -> Frame {
        Frame {
            global: Some(txrx(tx, rx)),
            ..Frame::default()
        }
    }

    /// 规则 1 + 4：只有 CLOSED 终值入账；Σdelta 与 Σtotal **不相加**。
    /// 朴素相加在这条用例里会得 200（实测生产上过账 1.96–1.97×）。
    #[test]
    fn closed_totals_are_the_only_source_of_bookings_and_deltas_are_never_added() {
        let l = Ledger::new();
        l.apply(ev("c1", new(txrx(0, 0))), 1_000);
        l.apply(ev("c1", update(60, 40)), 1_100);
        l.apply(ev("c1", update(0, 0)), 1_200);
        // 还没关：一个字节都不许出账
        assert_eq!(l.drain(2_000), BTreeMap::new());
        l.apply(ev("c1", closed("alice", txrx(60, 40), 1_000, 1_500)), 1_500);
        let mut want = BTreeMap::new();
        want.insert("alice".to_string(), txrx(60, 40));
        assert_eq!(l.drain(3_000), want, "只按终值入账，100 而不是 200");
    }

    /// 规则 2：Σdelta 单独用会丢字节（实测丢 6.8%~100%），所以 UPDATE 永远不进账。
    #[test]
    fn update_deltas_alone_never_enter_the_book() {
        let l = Ledger::new();
        l.apply(ev("c1", new(txrx(0, 0))), 1_000);
        for i in 0..10 {
            l.apply(ev("c1", update(10, 10)), 1_100 + i);
        }
        assert_eq!(
            l.drain(2_000),
            BTreeMap::new(),
            "没关的连接不计入已结算账本"
        );
        let g = l.inner.lock().unwrap();
        assert_eq!(g.booked_total, TxRx::default());
        assert_eq!(g.span(), txrx(100, 100), "增量只累加进在册跟踪量（ΔS）");
    }

    /// 规则 3 + 指标 A：`reset` 快照按 id upsert，快照 totals 绝不累加进账。
    #[test]
    fn reset_snapshots_upsert_by_id_and_never_book() {
        let l = Ledger::new();
        // 订阅之前就存在的连接，快照带上它的累计量
        l.apply(
            Frame {
                reset: true,
                global: None,
                connections_in: None,
                events: vec![Event {
                    id: "c1".into(),
                    kind: new(txrx(500, 300)),
                }],
            },
            1_000,
        );
        // 基准两端都得有全局量：第一帧 SubscribeStatus 在第一个区间边界之前到
        l.apply(status(500, 300), 1_100);
        assert_eq!(l.drain(2_000), BTreeMap::new(), "快照本身不许出账");
        // 快照总量只体现在 ΔS 上：这一区间全局 +100、在册跟踪量 +100，缺口为 0
        l.apply(status(600, 300), 2_100);
        l.apply(ev("c1", update(100, 0)), 2_200);
        assert_eq!(l.drain(3_000), BTreeMap::new());
        let st = l.view(3_000).stat;
        assert_eq!(
            (st.last_global_delta, st.last_span_delta, st.last_gap),
            (100, 100, 0),
            "{st:?}"
        );
        // 关闭时按终值入账一次（快照的 800 一次都不参与累加）
        l.apply(ev("c1", closed("alice", txrx(500, 400), 0, 3_100)), 3_100);
        let mut want = BTreeMap::new();
        want.insert("alice".to_string(), txrx(500, 400));
        assert_eq!(l.drain(4_000), want, "快照的 800 没被累加成 1700");
    }

    /// 规则 2：同一个 CLOSED 重复投递（实测两帧相隔 114µs、字段全同）幂等吸收。
    #[test]
    fn duplicate_closed_is_absorbed_by_id() {
        let l = Ledger::new();
        l.apply(ev("c1", new(txrx(0, 0))), 1_000);
        l.apply(ev("c1", closed("alice", txrx(7, 3), 1_000, 1_100)), 1_100);
        l.apply(ev("c1", closed("alice", txrx(7, 3), 1_000, 1_100)), 1_100);
        let mut want = BTreeMap::new();
        want.insert("alice".to_string(), txrx(7, 3));
        assert_eq!(l.drain(2_000), want, "重复的 CLOSED 不许再记一次");
        let st = l.view(2_000).stat;
        assert_eq!(
            (st.closed, st.dup_closed, st.booked_conns),
            (2, 1, 1),
            "{st:?}"
        );
    }

    /// 规则 5：亚 tick 连接必须入账（实测 40/40 sub-ms、60/60 ~1–2ms）。
    #[test]
    fn sub_tick_connections_are_booked() {
        let l = Ledger::new();
        for i in 0..40 {
            let id = format!("s{i}");
            l.apply(ev(&id, new(txrx(0, 0))), 1_000);
            // 同一毫秒里创建并关闭：寿命 0ms
            l.apply(ev(&id, closed("alice", txrx(1, 1), 1_000, 1_000)), 1_000);
        }
        assert_eq!(
            l.drain(2_000)["alice"],
            txrx(40, 40),
            "40 条亚毫秒连接一条不许丢"
        );
        let st = l.view(2_000).stat;
        assert_eq!((st.booked_conns, st.sub_tick_conns), (40, 40), "{st:?}");
    }

    /// 规则 6：`UPDATE` 按 id 关联（按五元组关联得 0）。两条同源同宿的连接各归各的。
    #[test]
    fn updates_are_keyed_by_id_not_by_tuple() {
        let l = Ledger::new();
        for id in ["a", "b"] {
            l.apply(ev(id, new(txrx(0, 0))), 1_000);
        }
        l.apply(ev("b", update(90, 10)), 1_100);
        let st = l.view(1_200).stat;
        assert_eq!(st.last_span_delta, 0, "还没到区间边界：{st:?}");
        l.apply(ev("b", closed("alice", txrx(90, 10), 1_000, 1_100)), 1_100);
        assert_eq!(l.drain(2_000)["alice"], txrx(90, 10));
        // a 的增量谁也没给，它还在册且 known 为 0 ⇒ 没有错配到 b 头上
        assert_eq!(l.view(2_000).open_conns, 1);
    }

    /// 规则 7：`conn=nil` 的 CLOSED（实测 14 条、39,168/41,472 字节）必须处理：
    /// 字节进总账（不然指标 A 把它算成丢失），但不进任何人的账。
    #[test]
    fn unnamed_closed_bytes_are_counted_but_never_billed() {
        let l = Ledger::new();
        l.apply(status(40_000, 1_472), 1_000);
        l.drain(1_000);
        l.apply(ev("c1", new(txrx(0, 0))), 1_100);
        l.apply(ev("c1", update(39_000, 168)), 1_200);
        l.apply(ev("c1", EventKind::Closed { conn: None }), 1_200);
        // 这 39,168 B 在全局账上也是要出现的（内核没丢它们，只是没给出归属），
        // 所以第二帧全局量把它们带上 —— 缺口必须仍然是 0
        l.apply(status(79_000, 1_640), 1_300);
        assert_eq!(
            l.drain(2_000),
            BTreeMap::new(),
            "归不了户的字节不许记到任何人头上"
        );
        let st = l.view(2_000).stat;
        assert_eq!(
            (
                st.unnamed_closed,
                st.unnamed_bytes,
                st.booked_bytes,
                st.last_gap
            ),
            (1, 39_168, 39_168, 0),
            "39,168 B 要进总账、缺口为 0：{st:?}"
        );
    }

    /// 指标 A 的健康基线（实测 = 0）：字节经 CLOSED 一分不差地过账时，每个区间都算平。
    #[test]
    fn a_healthy_interval_has_zero_gap() {
        let l = Ledger::new();
        l.note_session_start(1, 0);
        // 区间 1：全局 1,188,352 B 全落在一条开了没关的大连接上（UPDATE 把 known 追到终值）
        l.apply(ev("big", new(txrx(0, 0))), 10);
        l.apply(ev("big", update(1_000_100, 188_252)), 900);
        l.apply(status(1_000_100, 188_252), 1_000);
        assert_eq!(l.drain(1_000), BTreeMap::new());
        // 区间 2：它关了（终值 1,000,100/188,252），同区间又走了 452 B 的短连接
        l.apply(ev("short", new(txrx(0, 0))), 1_100);
        l.apply(
            ev("short", closed("bob", txrx(400, 52), 1_100, 1_150)),
            1_150,
        );
        l.apply(
            ev("big", closed("alice", txrx(1_000_100, 188_252), 10, 1_500)),
            1_500,
        );
        l.apply(status(1_000_500, 188_304), 2_000);
        l.apply(status(1_000_500, 188_304), 3_000);
        let mut want = BTreeMap::new();
        want.insert("alice".to_string(), txrx(1_000_100, 188_252));
        want.insert("bob".to_string(), txrx(400, 52));
        assert_eq!(l.drain(3_000), want);
        let st = l.view(3_000).stat;
        assert_eq!(
            (
                st.last_global_delta,
                st.last_booked_delta,
                st.last_span_delta,
                st.last_gap
            ),
            (452, 1_188_804, -1_188_352, 0),
            "健康区间缺口必须是 0（ΔG = ΔB + ΔS）：{st:?}"
        );
        assert!(!l.lossy_at(3_000));
    }

    /// 指标 A 的故障路径（实测缺口 84–92%）：流内零信号，只有全局账看得出来。
    #[test]
    fn a_lost_tail_marks_lossy_and_asks_for_a_resubscribe() {
        let l = Ledger::new();
        l.note_session_start(1, 0);
        l.apply(status(0, 0), 1);
        l.drain(1);
        // 一个区间里全局涨了 1,095,680 B（快消费者的两轮实测值），
        // 但账本只收到 212/4492 条连接里的一小撮：273,920 B 的缺口
        l.apply(ev("c1", new(txrx(0, 0))), 100);
        l.apply(
            ev("c1", closed("alice", txrx(800_000, 21_760), 100, 500)),
            500,
        );
        l.apply(status(1_095_680, 0), 1_000);
        l.drain(1_000);
        let st = l.view(1_000).stat;
        assert_eq!(st.last_gap, 273_920, "缺口 = 全局 − 入账 − 在册：{st:?}");
        assert_eq!(
            st.last_gap_pct, 25,
            "5% 线先触发（1 MiB 在 1,095,680 B 的区间里够不着）"
        );
        assert!(l.lossy_at(1_000), "越线即标损");
        assert!(l.take_resubscribe(), "越线要求重订阅一次兜底");
        assert!(!l.take_resubscribe(), "取走即清");
        assert!(
            l.recent_alerts()
                .iter()
                .any(|a| a.lossy && a.text.contains("后缀丢失")),
            "{:?}",
            l.recent_alerts()
        );
    }

    /// 观察线（> 64 KiB）与告警线（≥ 1 MiB **或** ≥ 5%）是两个门槛，别混。
    #[test]
    fn the_observe_line_alerts_without_marking_lossy() {
        let l = Ledger::new();
        l.note_session_start(1, 0);
        l.apply(status(0, 0), 1);
        l.drain(1);
        // 100 MiB 的区间里丢了 73,216 B：超过 64 KiB 观察线，但远不到 1 MiB，占比也远不到 5%
        l.apply(ev("c1", new(txrx(0, 0))), 100);
        l.apply(
            ev("c1", closed("alice", txrx(104_784_384, 0), 100, 500)),
            500,
        );
        l.apply(status(104_857_600, 0), 1_000);
        l.drain(1_000);
        let st = l.view(1_000).stat;
        assert_eq!(st.last_gap, 73_216);
        assert_eq!(st.observed_intervals, 1);
        assert_eq!(st.lossy_intervals, 0, "0.07% 不到 5%，也没到 1 MiB：{st:?}");
        assert!(!l.lossy_at(1_000));
        assert!(
            l.recent_alerts().iter().all(|a| !a.lossy),
            "观察行不带「有损」标记，否则哨兵会把良性观察也报成事件"
        );
        assert!(l.recent_alerts().iter().any(|a| a.text.contains("观察线")));
    }

    /// 钉住告警的单一出口（[`Ledger::publish_alerts`]）：每条告警**只入环一次**，
    /// 一个有损期**只宣告一次**（哪怕中间夹了观察行），恢复行也只有一条。
    ///
    /// 曾经是三条并行写法：`drain` 产出的观察行只落日志、不进环（上面那条用例抓到），
    /// 而 `enter_lossy` 自己写环、调用方再走日志，又给重复入环留了口子。
    #[test]
    fn alerts_enter_the_ring_exactly_once_through_the_single_exit() {
        let l = Ledger::new();
        l.note_session_start(1, 0);
        l.apply(status(0, 0), 1);
        l.drain(1);
        // 区间一：观察线（100 MiB 里缺 73,216 B）—— 非有损，但必须入环
        l.apply(ev("c1", new(txrx(0, 0))), 100);
        l.apply(
            ev("c1", closed("alice", txrx(104_784_384, 0), 100, 500)),
            500,
        );
        l.apply(status(104_857_600, 0), 1_000);
        l.drain(1_000);
        // 区间二：恰 1 MiB 的后缀丢失 —— 有损，宣告一次
        l.apply(ev("c2", new(txrx(0, 0))), 1_100);
        l.apply(
            ev("c2", closed("bob", txrx(1_048_576, 0), 1_100, 1_500)),
            1_500,
        );
        l.apply(status(104_857_600 + 2_097_152, 0), 2_000);
        l.drain(2_000);
        // 区间三：抑制期内再丢一次（数字与区间二不同）—— 不许重报
        l.apply(ev("c3", new(txrx(0, 0))), 2_100);
        l.apply(
            ev("c3", closed("bob", txrx(1_048_576, 0), 2_100, 2_500)),
            2_500,
        );
        l.apply(status(104_857_600 + 6_291_456, 0), 3_000);
        l.drain(3_000);

        let ring = l.recent_alerts();
        let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
        for a in &ring {
            *seen.entry(a.text.as_str()).or_default() += 1;
        }
        for (text, n) in &seen {
            assert_eq!(*n, 1, "同一条告警入环 {n} 次：{text}");
        }
        assert_eq!(
            ring.iter().filter(|a| a.text.contains("观察线")).count(),
            3,
            "每个区间的观察行都必须入环：{ring:?}"
        );
        assert_eq!(
            ring.iter().filter(|a| a.text.contains("后缀丢失")).count(),
            1,
            "一个抑制期只宣告一次（区间三不许重报）：{ring:?}"
        );
        // 抑制走完：恢复行一条，状态回到可裁决
        let after = LOSSY_HOLD_MS + LOSSY_MAX_MS + 1;
        l.drain(after);
        let ring = l.recent_alerts();
        assert_eq!(
            ring.iter().filter(|a| a.text.contains("已恢复")).count(),
            1,
            "{ring:?}"
        );
        assert_eq!(ring.len(), 5, "三条观察 + 一条有损 + 一条恢复：{ring:?}");
        assert!(!l.lossy_at(after));
    }

    /// 规则 9 + 指标 B：代际变化封账、在册与去重清空、记一条可疑损失。
    #[test]
    fn a_new_generation_seals_the_book() {
        let l = Ledger::new();
        l.note_session_start(1_700_000_000_000, 0);
        l.apply(ev("c1", new(txrx(0, 0))), 10);
        l.apply(ev("c1", closed("alice", txrx(5, 5), 0, 10)), 10);
        assert_eq!(l.drain(20)["alice"], txrx(5, 5));
        // 重启：代际变了，同一个 id 是另一条连接（id 空间重置）
        l.note_session_start(1_700_000_100_000, 30);
        l.apply(ev("c1", new(txrx(0, 0))), 31);
        l.apply(ev("c1", closed("alice", txrx(9, 1), 31, 32)), 32);
        assert_eq!(l.drain(40)["alice"], txrx(9, 1), "换代后同 id 要能重新入账");
        assert!(
            l.recent_alerts()
                .iter()
                .any(|a| !a.lossy && a.text.contains("代际变化")),
            "{:?}",
            l.recent_alerts()
        );
        assert!(!l.lossy_at(40), "换代只标可疑损失，不判有损");
        assert_eq!(l.view(40).generation, Some(1_700_000_100_000));
    }

    /// 规则 10：UDP 记**账本净负载**，不按客户端封装口径加 10 B/报文/方向。
    #[test]
    fn udp_is_billed_as_the_net_payload() {
        let l = Ledger::new();
        l.apply(ev("u1", new(txrx(0, 0))), 1_000);
        l.apply(
            ev(
                "u1",
                EventKind::Closed {
                    conn: Some(ClosedConn {
                        user: Some("alice".into()),
                        destination: Some("1.1.1.1:443".into()),
                        network: Some("udp".into()),
                        total: txrx(1_200, 800),
                        created_at_ms: 1_000,
                        closed_at_ms: 2_000,
                    }),
                },
            ),
            2_000,
        );
        assert_eq!(
            l.drain(3_000)["alice"],
            txrx(1_200, 800),
            "净负载就是净负载"
        );
        let st = l.view(3_000).stat;
        assert_eq!(st.udp_bytes, 2_000, "{st:?}");
    }

    /// 规则 11 + §3.4：归属键是服务端 `users[].name`；探针流量入账但单独计数。
    #[test]
    fn the_billing_key_is_the_server_side_user_name_and_probes_are_counted() {
        let l = Ledger::new();
        l.apply(ev("p1", new(txrx(0, 0))), 1_000);
        l.apply(
            ev(
                "p1",
                EventKind::Closed {
                    conn: Some(ClosedConn {
                        user: Some("alice".into()),
                        destination: Some("api.hy2.io:443".into()),
                        network: Some("tcp".into()),
                        total: txrx(200, 500),
                        created_at_ms: 1_000,
                        closed_at_ms: 1_100,
                    }),
                },
            ),
            1_100,
        );
        let drained = l.drain(2_000);
        assert_eq!(
            drained.keys().collect::<Vec<_>>(),
            vec!["alice"],
            "键是凭据 name，不是 uuid"
        );
        let st = l.view(2_000).stat;
        assert_eq!((st.probe_conns, st.probe_bytes), (1, 700), "{st:?}");
        // 逐字节精确匹配：别把长得像的域名算成探针
        assert!(is_probe_destination("api.hy2.io:443"));
        assert!(!is_probe_destination("xapi.hy2.io:443"));
        assert!(!is_probe_destination("api.hy2.io:8443"));
        assert!(!is_probe_destination("api.hy2.io"));
    }

    /// 指标 B/C：中断封账、`available` 变 false；10 分钟 5 次中断判有损。
    #[test]
    fn interruptions_seal_the_book_mark_lossy_and_expire() {
        let l = Ledger::new();
        l.note_session_start(1, 0);
        assert!(l.available(0));
        l.note_interruption(1_000, "Unavailable: transport is closing");
        assert!(
            !l.available(1_000),
            "断了就是不可用：采样器那条 Err 分支的判据"
        );
        for i in 1..INTERRUPT_LIMIT {
            l.note_interruption(1_000 + i as i64 * 60_000, "Unavailable");
        }
        assert!(l.lossy_at(300_000), "10 分钟 5 次中断 ⇒ 有损");
        assert!(l
            .recent_alerts()
            .iter()
            .any(|a| a.lossy && a.text.contains("中断")));
        // 恢复：一帧到达就重新可用；抑制到期后自动裁决恢复
        l.note_session_start(1, 400_000);
        assert!(l.available(400_000));
        l.drain(LOSSY_HOLD_MS + LOSSY_MAX_MS);
        assert!(
            !l.lossy_at(LOSSY_HOLD_MS + LOSSY_MAX_MS + 1),
            "上限到了必须失效"
        );
        assert!(
            l.recent_alerts()
                .iter()
                .any(|a| !a.lossy && a.text.contains("已恢复")),
            "{:?}",
            l.recent_alerts()
        );
    }

    /// 「流卡死」也算不可用：15 秒没有任何一帧就不再假装账目可信。
    #[test]
    fn a_stale_stream_is_not_available() {
        let l = Ledger::new();
        l.note_session_start(1, 0);
        assert!(l.available(STREAM_STALE_MS));
        assert!(!l.available(STREAM_STALE_MS + 1));
        l.apply(status(0, 0), STREAM_STALE_MS + 1);
        assert!(l.available(STREAM_STALE_MS + 1), "来一帧就算活着");
    }

    /// 规则 3 的边界：幂等只在 `DEDUP_TTL_MS` 窗口内承诺。窗口内重投（实测相隔 114µs）
    /// 被吸收；窗口外再投递按「新连接」入账 —— 有界的近似幂等换掉无界去重表。这条把
    /// 窗口两侧的行为都钉死，改 TTL 就会红。
    #[test]
    fn dedup_entries_expire_after_the_ttl() {
        let l = Ledger::new();
        l.apply(ev("c1", closed("alice", txrx(1, 1), 0, 0)), 0);
        // 窗口内重投：幂等吸收，不重复入账、不出告警
        l.apply(
            ev("c1", closed("alice", txrx(1, 1), 0, 0)),
            DEDUP_TTL_MS - 1,
        );
        assert_eq!(l.view(DEDUP_TTL_MS - 1).stat.dup_closed, 1);
        assert_eq!(l.recent_alerts().len(), 0, "幂等吸收不是有损，不许出告警");
        // 已结账的 id 再出现也不许重开（否则留一条永不结算的幽灵）
        l.apply(ev("c1", new(txrx(0, 0))), DEDUP_TTL_MS - 1);
        assert_eq!(l.view(DEDUP_TTL_MS - 1).open_conns, 0);
        let mut want = BTreeMap::new();
        want.insert("alice".to_string(), txrx(1, 1));
        assert_eq!(l.drain(DEDUP_TTL_MS), want, "窗口内只记一次");
        // TTL 过期（一次空帧就触发老化），同 id 的 CLOSED 再投递按新连接入账
        l.apply(Frame::default(), DEDUP_TTL_MS + 1);
        l.apply(
            ev("c1", closed("alice", txrx(2, 0), 0, DEDUP_TTL_MS + 2)),
            DEDUP_TTL_MS + 2,
        );
        let mut want = BTreeMap::new();
        want.insert("alice".to_string(), txrx(2, 0));
        assert_eq!(l.drain(DEDUP_TTL_MS + 3), want, "窗口外按新连接入账");
    }

    /// 缺陷①回归：指标 A 的基准（mark）两端都必须真的被喂过全局量。首个区间还没喂过时
    /// `global` 还是初值 0，若照样比较，内核的**历史总量**会被整段算成这一区间的 ΔG
    /// （一个凭空的「后缀丢失」）。
    #[test]
    fn an_interval_without_any_global_frame_is_never_compared() {
        let l = Ledger::new();
        l.note_session_start(1, 0);
        // 区间 1：CLOSED 入账了，但一个 SubscribeStatus 帧都还没来过
        l.apply(ev("c1", new(txrx(0, 0))), 100);
        l.apply(ev("c1", closed("alice", txrx(1_234_567, 0), 100, 200)), 200);
        l.drain(300);
        let st = l.view(300).stat;
        assert_eq!(
            (
                st.last_global_delta,
                st.last_gap,
                st.observed_intervals,
                st.lossy_intervals
            ),
            (0, 0, 0, 0),
            "没喂过全局量的区间不许比较：{st:?}"
        );
        // 区间 2：第一帧全局量到了（内核历史总量 1,234,567 就是区间 1 的那条连接），
        // 仍然不许把整段历史当成本区间的 ΔG
        l.apply(status(1_234_567, 0), 400);
        l.drain(600);
        let st = l.view(600).stat;
        assert_eq!((st.last_global_delta, st.last_gap), (0, 0), "{st:?}");
        assert_eq!(
            (st.observed_intervals, st.lossy_intervals),
            (0, 0),
            "{st:?}"
        );
        assert!(!l.lossy_at(600), "历史总量不是缺口");
        assert!(!l.take_resubscribe(), "也不许要求重订阅");
        // 基准两端都喂过之后，差分恢复（别过度修正）：区间 3 的 +10 B 如实报出，缺口 0
        l.apply(ev("c2", new(txrx(0, 0))), 610);
        l.apply(ev("c2", update(10, 0)), 620);
        l.apply(ev("c2", closed("bob", txrx(10, 0), 610, 630)), 630);
        l.apply(status(1_234_577, 0), 640);
        l.drain(700);
        let st = l.view(700).stat;
        assert_eq!(
            (st.last_global_delta, st.last_booked_delta, st.last_gap),
            (10, 10, 0),
            "基准齐了差分必须恢复：{st:?}"
        );
    }

    /// 容量淘汰只在**年轻**条目上判有损：那些 id 的重复 CLOSED 还可能被补投回来
    /// （实测补投窗口 14–30 s），挤掉它们 = 同一连接会重复入账；挤掉年老条目只是
    /// 内存保护，只记 [`MeterStat::dedup_evicted`]。
    #[test]
    fn only_young_dedup_evictions_mark_lossy() {
        // 单帧塞满 DEDUP_CAP 条不同 id 的 CLOSED（正好挤掉表里多出来的那一条）
        fn flood() -> Frame {
            Frame {
                events: (0..DEDUP_CAP)
                    .map(|i| Event {
                        id: format!("f{i}"),
                        kind: closed("flood", txrx(1, 0), 1_000, 1_000),
                    })
                    .collect(),
                ..Frame::default()
            }
        }
        // 年轻案：一条 @0，洪泛 @1,000 —— 被挤掉的条目还在 30 s 重投窗口里
        let l = Ledger::new();
        l.apply(ev("old", closed("alice", txrx(1, 0), 0, 0)), 0);
        l.apply(flood(), 1_000);
        let st = l.view(1_000).stat;
        assert_eq!(st.dedup_evicted, 1, "只挤掉最旧的那一条：{st:?}");
        assert!(l.lossy_at(1_000), "挤掉年轻条目 = 可能重复入账，必须标损");
        assert!(
            l.recent_alerts()
                .iter()
                .any(|a| a.lossy && a.text.contains("去重表")),
            "{:?}",
            l.recent_alerts()
        );
        // 年老案：同样的洪泛发生在重投窗口之后 —— 只记数，不标损
        let l = Ledger::new();
        l.apply(ev("old", closed("alice", txrx(1, 0), 0, 0)), 0);
        l.apply(flood(), REFILL_WINDOW_MS + 1);
        let st = l.view(REFILL_WINDOW_MS + 1).stat;
        assert_eq!(st.dedup_evicted, 1, "{st:?}");
        assert!(
            !l.lossy_at(REFILL_WINDOW_MS + 1),
            "超出补投窗口的老化只是内存保护，不该惊动哨兵"
        );
    }

    /// §5「不得静默」回归：有损判定必须以 [`METERING_MARKER`] + [`METERING_LOSS_TAG`]
    /// 进 tracing（哨兵按这两个词认领）。旧实现里 `close` 丢弃告警、`metering_loop`
    /// 把中断记进私有假账本，日志里只有非有损的观察行，哨兵永远认领不到。
    #[test]
    fn lossy_alerts_reach_the_log() {
        #[derive(Clone, Default)]
        struct LogCapture(Arc<std::sync::Mutex<Vec<u8>>>);

        impl LogCapture {
            fn text(&self) -> String {
                String::from_utf8_lossy(&self.0.lock().expect("日志缓冲锁")).into_owned()
            }

            fn clear(&self) {
                self.0.lock().expect("日志缓冲锁").clear();
            }
        }

        impl std::io::Write for LogCapture {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().expect("日志缓冲锁").extend_from_slice(buf);
                Ok(buf.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogCapture {
            type Writer = Self;

            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let log = LogCapture::default();
        let _g = tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .with_writer(log.clone())
                .with_ansi(false)
                .finish(),
        );
        // A 场景（与 a_lost_tail 同一组数字）：区间后缀丢失 ⇒ 有损行必须出现
        {
            let l = Ledger::new();
            l.note_session_start(1, 0);
            l.apply(status(0, 0), 1);
            l.drain(1);
            l.apply(ev("c1", new(txrx(0, 0))), 100);
            l.apply(
                ev("c1", closed("alice", txrx(800_000, 21_760), 100, 500)),
                500,
            );
            l.apply(status(1_095_680, 0), 1_000);
            l.drain(1_000);
        }
        let text = log.text();
        assert!(text.contains(METERING_MARKER), "{text}");
        assert!(
            text.contains(METERING_LOSS_TAG),
            "有损行必须进 tracing：{text}"
        );
        assert!(text.contains("后缀丢失"), "{text}");
        assert!(text.contains("观察"), "非有损的观察行同时在场：{text}");
        // B 场景：10 分钟 5 次中断 —— 每条中断都有观察行，有损行只出一次（抑制不刷屏）
        log.clear();
        {
            let l = Ledger::new();
            for i in 0..INTERRUPT_LIMIT {
                l.note_interruption(1_000 + i as i64 * 60_000, "Unavailable");
            }
        }
        let text = log.text();
        assert_eq!(
            text.matches("计量流中断").count(),
            INTERRUPT_LIMIT,
            "每条中断都要留下观察行：{text}"
        );
        assert_eq!(
            text.matches(METERING_LOSS_TAG).count(),
            1,
            "五次中断只触发一条有损：{text}"
        );
    }
}
