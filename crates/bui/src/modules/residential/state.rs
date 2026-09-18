//! 住宅模块的运行时数据（`runtime.json` 的 `extra["residential"]`）与状态读写助手。
use super::{
    FAIL_TO_UNHEALTHY, GROUP_DEFAULT, LATENCY_SAMPLES_MAX, OK_TO_HEALTHY, RATE_SAMPLES_MAX,
    RATE_WINDOW_SECS, RUNTIME_KEY, SPEEDTEST_SAMPLES_MAX,
};
use crate::api::{Event, EventBus};
use crate::state::runtime::{Runtime, RuntimeData};
use crate::state::store::Store;
use crate::util::{fmt_rfc3339, parse_rfc3339};
use bui_schema::model::{ResidentialGroup, State};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use time::OffsetDateTime;
use uuid::Uuid;

/// `alerts` 的长度上限：面板只展示最近几条，无上限会把 `runtime.json` 撑爆
pub const ALERTS_MAX: usize = 20;

/// 「这个池反复有没记账的切换」告警的固定前缀（[`note_unstable_pools`] 写、
/// 面板与用例按它认领）。文案里点出是哪个池 —— 不然运维不知道该查哪一条落账路径。
pub const UNSTABLE_ALERT: &str = "住宅池 selector 反复与运行时记录不一致";

/// `runtime.json` 的 `extra["residential"]`（spec §2.1：健康 streak、黑名单候选计数、
/// 上次选中的上游、采样游标都放 runtime.json，丢了也能从零重建）
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ResiRuntime {
    /// key = **上游 uuid 的字符串形式**（`Uuid::to_string()`），不是 `resi-N`：tag 是位置键，
    /// 增删上游会 renumber，用它当主键会让 streak 与成功率错位（契约决策 §C）
    pub health: BTreeMap<String, HealthState>,
    /// 当前实际生效的上游（Clash API 真源快照；tag 由 `clash::tag_of` 现算，契约决策 §C）
    pub selected_upstream_id: Option<Uuid>,
    /// 管理员**手动锁定**的上游（`POST /api/residential/select`）：只要它还健康，巡检就
    /// 不按 priority / Google 排序把它切走（R2 ①）。它变不健康时由巡检清空并自动切换；
    /// `POST /api/residential/select {"auto":true}` 也能主动解锁
    pub manual_selected_id: Option<Uuid>,
    /// 每个槽位的运行时状态，键 = 槽序号的十进制串（spec §5.6）
    pub slots: BTreeMap<String, SlotRuntime>,
    /// 渲染出的 Xray 槽路由与 xray 进程里正在跑的那一份**可能**已经不一致（D7）。
    /// 由用户增删 / 改分槽 / `rebalance` / 删上游后的重分配置位，由
    /// `slots::converge_xray`（T4）收敛成功（gRPC 增删、确认 xray 已从磁盘加载，
    /// 或退回一次重启）后清掉。
    pub xray_slot_rules_dirty: bool,
    /// 最近一次**成功收敛**时那份槽路由的 `slot_rules_hash`（D7 第 2–4 步）。
    /// `converge_xray` 拿它做快速判等：脏了但哈希没变 ⇒ 清脏、不动 xray。
    /// 真正的差分真源是 `ListRule()` 读回来的那张表，不是这个字段。
    pub xray_slot_rules_hash: Option<String>,
    pub last_switch_at: Option<String>,
    /// 每个**池 selector** 最近一次成功切换的时刻（RFC3339，键 = 池 tag：[`super::POOL`] 或
    /// `slot-<i>-pool`）。relay 错误行归因的**时间戳门**（[`super::SWITCH_ATTRIB_GRACE_SECS`]）
    /// 就读它：写入口只有 [`note_pool_switch`] 与它的落盘壳 [`mark_pool_switch`]（以及批量版
    /// [`note_pools_switch`] / [`mark_pools_switch`]，供「relay 重启 = 全池切换」那几条来路用），
    /// 调用点是全部 `Clash::select` 成功返回处、以及检测到没记账的切换时的补记处。
    /// 与上面那个全局的 `last_switch_at` 不是一回事——那个是巡检切换的 60 秒限速游标，
    /// 只记全局池、重放时还故意不写。键数上限 = `MAX_SLOTS + 1`。
    pub pool_switch_at: BTreeMap<String, String>,
    /// 每个池 selector 连续被 [`super::clash::unstable_pools`] 判成「有过没记账的切换」的
    /// **批次数**（键 = 池 tag）。稳定一批即清零，攒满
    /// [`super::UNSTABLE_BATCHES_TO_ALERT`] 就告警 + 收敛一次（2026-09-18 第四次裁决 ③：
    /// 兜底不许变哑巴）。写入口只有 [`note_unstable_pools`]。键数上限 = `MAX_SLOTS + 1`。
    pub unstable_streak: BTreeMap<String, u32>,
    /// 上一次为「这个池反复不稳定」告警的时刻（RFC3339，键 = 池 tag）：告警与收敛共用
    /// 这一个冷却游标（[`super::UNSTABLE_ALERT_COOLDOWN_SECS`]），别刷屏、也别反复重放
    pub unstable_alert_at: BTreeMap<String, String>,
    /// `state.selected_upstream_id` 与 `selected_upstream_id` 已漂移，等下一个 04:00 窗口写回
    pub selected_pending_persist: bool,
    /// 面板「立即巡检一轮」按钮的限速游标（T10 的 `POST /api/residential/health/check`）
    pub last_manual_round_at: Option<String>,
    /// 黑名单候选，key = `candidate_key(upstream_id, host, port)`
    pub candidates: BTreeMap<String, Candidate>,
    /// **已确认完毕**（`confirms >= CONFIRM_NEEDED`）、等 04:00 批量写进
    /// `state.blacklist.auto` 的条目。进了这里就不再需要判 `confirms`
    pub pending: Vec<PendingEntry>,
    pub journal_cursor: Option<String>,
    /// key = upstream_id 的字符串形式，值 = `check::CheckReport` 的 JSON
    /// （避免 state.rs 反向依赖 check.rs）
    pub checks: BTreeMap<String, serde_json::Value>,
    /// 正在跑的体检（面板据此显示「检测中…」，对应 R13 §4 的 `checking`）
    pub checking: Option<Checking>,
    /// 最近的**全局**告警（全员不达标、切换失败、journalctl 缺失…），上限
    /// [`ALERTS_MAX`]，新的在前。上游级告警不进这里，见 [`ResiRuntime::upstream_alerts`]
    pub alerts: Vec<String>,
    /// 上游级告警（407 凭据失效…），**以 uuid 为键**（契约决策 §C）。绝不能按 `url-N`
    /// 存：位置名会被 `renumber` 复用，删掉一条后新加的上游会顶着上一个账号的告警
    /// （2026-09-12 bwg-rick 的真机事故）。文案里也只写 `host:port`，不写 `url-N`。
    /// 同一 uuid 只留最新一条；条目被删（[`super::upstream::remove`]）或探测/体检
    /// 成功一次即清。输出侧一律经 [`visible_alerts`]，池里没有的 uuid 不显示。
    pub upstream_alerts: BTreeMap<Uuid, String>,
    pub last_daily_at: Option<String>,
    /// 上一次**全量测速**的时间（每小时一轮的判据，主理人 2026-09-12）。
    /// 放 runtime 而不是另起一个后台任务：巡检本来就每 2 分钟醒一次，到点顺手跑一轮，
    /// 少一个调度器、也不会与巡检抢同一批上游
    pub last_speedtest_at: Option<String>,
    /// 「更优候选」防抖：当前正在攒轮数的候选与已连续满足的轮数。
    /// 候选一换就归零 —— 三条上游轮流各赢一轮不该凑成 3 轮
    pub improve_candidate_id: Option<Uuid>,
    pub improve_rounds: u32,
}

/// 一个槽位的运行时状态（spec §5.6「巡检按槽驱动 selector」）。
/// 键是**槽序号的十进制串**（与 `health` 用 uuid 串同风格：`runtime.json` 的 map 键只能是串）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SlotRuntime {
    /// 该槽的 selector 当前指向哪条上游（`None` = 还没驱动过 / 刚被清零）。
    /// 与本槽自己的 IP 不同 ⇒ 正在**借用**别人的出口。
    ///
    /// 两处会把它清成 `None`：relay 重启（`health::replay_loop`，T5）、解除 pin
    /// （`slots::pin_slot(.., None)`，T5）。清零的语义都是「下一轮 `drive_slots`
    /// 无条件按本槽优先重 PUT 一次」，而不是「正在借用」——所以解除 pin 不会被防抖
    /// 卡住 3 轮。
    pub current_upstream_id: Option<Uuid>,
    /// 管理员把这一槽**钉**在某条上游上（`POST /api/residential/slots/pin`）：
    /// 驱动器不再按「本槽优先 / 借用」把它挪走。
    pub pinned_upstream_id: Option<Uuid>,
    /// 借用期间「本槽自己已经连续健康了几轮」，攒满 `SLOT_BACK_ROUNDS` 才切回。
    pub back_rounds: u32,
}

/// 单个上游的健康状态。**容器级** `#[serde(default)]`：缺字段时走 [`HealthState::default`]，
/// 这样从旧版 `runtime.json` 读回来的成员也是 `active = true`（字段级 default 会给 `false`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct HealthState {
    pub active: bool,
    pub okstreak: u32,
    pub failstreak: u32,
    pub samples: Vec<ProbeSample>,
    /// 最近一次**探到结论**的 Google 可达性（`None` = 还没探到）。参与选路：
    /// [`super::health::pick_target`] 把「Google 通」排在 `priority` 之前（R2 ②）
    pub google_ok: Option<bool>,
    /// 上一行那个结论是什么时候的（面板显示用）
    pub google_at: Option<String>,
    /// **到上游网关**的 TCP 建连耗时样本（毫秒，环形，上限 [`LATENCY_SAMPLES_MAX`]）
    pub tcp_ms: Vec<u64>,
    /// **经上游的完整 HTTP 往返**耗时样本（毫秒，环形）。选路用的就是这一项的 p50
    pub http_ms: Vec<u64>,
    /// 经上游 SOCKS5 UDP ASSOCIATE 的 **STUN 往返**耗时样本（毫秒，环形）。
    /// 与上面两项分开：UDP 走的是另一条路径，混进 TCP 样本就看不出是谁慢
    pub udp_ms: Vec<u64>,
    /// 最近一次**探到结论**的 UDP 可用性（`None` = 还没探过）。参与选路（排在 `priority` 之前）
    pub udp_ok: Option<bool>,
    /// STUN 的 XOR-MAPPED-ADDRESS：**UDP 出口 IP**（与 TCP 出口 IP 可能不是同一个）
    pub udp_exit_ip: Option<String>,
    /// UDP 不通的原因（`HTTP 上游无 UDP` / 超时…）
    pub udp_note: Option<String>,
    pub udp_at: Option<String>,
    /// 最近 [`SPEEDTEST_SAMPLES_MAX`] 次测速（Mbps，环形），报中位数
    pub down_mbps: Vec<f64>,
    pub up_mbps: Vec<f64>,
    pub speed_at: Option<String>,
    /// 测速失败的原因。**测速失败不影响健康判定**（主理人口径），只留这一条说明
    pub speed_note: Option<String>,
}

impl Default for HealthState {
    fn default() -> Self {
        // 新成员默认健康：没探过 ≠ 坏。与 v3 `resi-health.sh` 的 `.active // true` 同义。
        Self {
            active: true,
            okstreak: 0,
            failstreak: 0,
            samples: Vec::new(),
            // Google 可达性没有「默认通」：没探过就是未知，别让它凭空赢过探过的成员
            google_ok: None,
            google_at: None,
            // 延迟 / 速度 / UDP 同理：没测过就是空，不是 0（0 毫秒会赢过所有人）
            tcp_ms: Vec::new(),
            http_ms: Vec::new(),
            udp_ms: Vec::new(),
            udp_ok: None,
            udp_exit_ip: None,
            udp_note: None,
            udp_at: None,
            down_mbps: Vec::new(),
            up_mbps: Vec::new(),
            speed_at: None,
            speed_note: None,
        }
    }
}

/// 一次巡检探测的样本（24h 成功率的原料）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProbeSample {
    #[serde(default)]
    pub at: String,
    #[serde(default)]
    pub ok: bool,
}

/// 黑名单候选：还没确认，只计数
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Candidate {
    #[serde(default)]
    pub upstream_id: Uuid,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub hits: u64,
    #[serde(default)]
    pub first_seen: String,
    #[serde(default)]
    pub last_seen: String,
    /// 确认进度记在候选上（spec §5.4「间隔 ≥10 分钟连续 2 次」）：
    /// 只有攒到 `CONFIRM_NEEDED` 才升进 `pending`
    #[serde(default)]
    pub confirms: u32,
    #[serde(default)]
    pub last_confirm_at: Option<String>,
}

/// 已确认、等 04:00 窗口批量写进 `state.blacklist.auto` 的条目
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingEntry {
    #[serde(default)]
    pub upstream_id: Uuid,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub hits: u64,
    #[serde(default)]
    pub confirms: u32,
    #[serde(default)]
    pub last_confirm_at: String,
}

/// 正在跑的体检
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Checking {
    #[serde(default)]
    pub upstream_id: Uuid,
    #[serde(default)]
    pub started_at: String,
}

/// 从 `RuntimeData.extra` 里取住宅段；解析失败按空值重建（`runtime.json` 可丢可重建）
pub fn from_runtime(rt: &RuntimeData) -> ResiRuntime {
    match rt.extra.get(RUNTIME_KEY) {
        None => ResiRuntime::default(),
        Some(v) => serde_json::from_value(v.clone()).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "runtime.json 的 residential 段解析失败，按空值重建");
            ResiRuntime::default()
        }),
    }
}

pub async fn read(runtime: &Runtime) -> ResiRuntime {
    from_runtime(&runtime.read().await)
}

/// 改运行时数据的唯一入口：读出住宅段 → 交给 `f` 改 → 写回 `extra[RUNTIME_KEY]` 并落盘
pub async fn update(runtime: &Runtime, f: impl FnOnce(&mut ResiRuntime)) -> ResiRuntime {
    let mut out = ResiRuntime::default();
    runtime
        .update(|rt| {
            let mut r = from_runtime(rt);
            f(&mut r);
            // to_value 只会在自定义 Serialize 里失败，这里全是 derive
            if let Ok(v) = serde_json::to_value(&r) {
                rt.extra.insert(RUNTIME_KEY.to_string(), v);
            }
            out = r;
        })
        .await;
    out
}

/// 记一次池 selector 的切换时刻（[`ResiRuntime::pool_switch_at`] 的**唯一**写入口）。
///
/// **必须在 `Clash::select` 成功返回之后立刻调**：门比较的就是「日志行时间戳 vs. `select`
/// 返回时刻」。切失败的那次不写——什么都没变，写了只会白丢一段本该学的行。
/// relay 重启后的**重放**也算切换：它同样让 selector 的 `now` 变了值（它不算「切换」的只是
/// 巡检那条 60 秒限速，那是另一回事）。
///
/// **`slots::drive_slots` 只给自己真 PUT 过的那几槽记，不对全池盖章** —— 追认为设计
/// （2026-09-18 第四次裁决 ⑤）：它是逐槽驱动，没动过的 selector 的 `now` 一个字都没变，
/// 给它们盖章只会白丢一段本该学的行。「全池都变了」是 relay 重启那件事的性质，由那几条
/// 来路（看门狗 / 服务端点 / 规则 6a / [`mark_unstable_pools`] 的兜底）走
/// [`note_pools_switch`]。
pub fn note_pool_switch(r: &mut ResiRuntime, pool: &str, now: OffsetDateTime) {
    r.pool_switch_at.insert(pool.to_string(), fmt_rfc3339(now));
}

/// [`note_pool_switch`] 的独立落盘版：调用点手头没有别的 runtime 改动时用它
pub async fn mark_pool_switch(runtime: &Runtime, pool: &str, now: OffsetDateTime) {
    update(runtime, |r| note_pool_switch(r, pool, now)).await;
}

/// 对一批池各记一次切换时刻。**relay 重启 = 一次全池切换**（2026-09-18 第三次裁决 ③）：
/// 不开 `cache_file`，一重启每个 selector 的 `now` 都回落到配置里的 default，
/// 所以凡是检测 / 处理 relay 重启的来路都拿 [`all_pool_selectors`] 走这个批量版。
pub fn note_pools_switch(r: &mut ResiRuntime, pools: &[String], now: OffsetDateTime) {
    for p in pools {
        note_pool_switch(r, p, now);
    }
}

/// [`note_pools_switch`] 的独立落盘版；空表直接返回，不白写一次盘
pub async fn mark_pools_switch(runtime: &Runtime, pools: &[String], now: OffsetDateTime) {
    if pools.is_empty() {
        return;
    }
    update(runtime, |r| note_pools_switch(r, pools, now)).await;
}

/// 「没记账的切换」兜底的**连续批次**账（2026-09-18 第四次裁决 ③：兜底不许变哑巴）。
///
/// `seen` = 本批读过 `now` 的全部池，`unstable` = 其中被
/// [`super::clash::unstable_pools`] 判成不稳定的那些。不稳定的池 streak +1，其余清零
/// （`seen` 之外的池本批没有证据，账不动）。某池连续
/// [`super::UNSTABLE_BATCHES_TO_ALERT`] 批不稳定 ⇒ **告警一次**并要求收敛一次，之后
/// [`super::UNSTABLE_ALERT_COOLDOWN_SECS`] 秒内同一池不再重复（否则每批一条告警 +
/// 每批一次重放）。
///
/// 返回本批**刚触发**的池（调用方据此发一次 [`crate::api::Event::RelayRestarted`] 让
/// `health::replay_after_restart` 把 runtime 与 relay 收敛回来）。告警已在函数内 push。
pub fn note_unstable_pools(
    r: &mut ResiRuntime,
    seen: &[String],
    unstable: &std::collections::BTreeSet<String>,
    now: OffsetDateTime,
) -> Vec<String> {
    let mut fired = Vec::new();
    for p in seen {
        if !unstable.contains(p) {
            // 稳定一批就清零：下次要重新攒满才再喊
            r.unstable_streak.remove(p);
            r.unstable_alert_at.remove(p);
            continue;
        }
        let streak = r.unstable_streak.entry(p.clone()).or_insert(0);
        *streak += 1;
        if *streak < super::UNSTABLE_BATCHES_TO_ALERT {
            continue;
        }
        let streak = *streak;
        // 冷却：同一池的告警与收敛共用这一个游标，别每批喊一次、每批重放一次
        let cooling = r
            .unstable_alert_at
            .get(p)
            .and_then(|s| parse_rfc3339(s))
            .is_some_and(|t| {
                now < t + time::Duration::seconds(super::UNSTABLE_ALERT_COOLDOWN_SECS)
            });
        if cooling {
            continue;
        }
        r.unstable_alert_at.insert(p.clone(), fmt_rfc3339(now));
        push_alert(
            r,
            format!(
                "{UNSTABLE_ALERT}：{p} 连续 {streak} 批被判有没记账的切换，\
                 这些批的归因证据已放弃；正在重放收敛，请查该池的落账路径"
            ),
        );
        fired.push(p.clone());
    }
    fired
}

/// [`note_unstable_pools`] 的落盘版：兜底的补记切换时刻与连续批次账**并成一次写**
/// （两件事读的是同一批证据，分两次写会在中间留一个不一致的窗口）。
/// 返回值同 [`note_unstable_pools`]。
pub async fn mark_unstable_pools(
    runtime: &Runtime,
    seen: &[String],
    unstable: &std::collections::BTreeSet<String>,
    now: OffsetDateTime,
) -> Vec<String> {
    if seen.is_empty() {
        return Vec::new();
    }
    let pools: Vec<String> = unstable.iter().cloned().collect();
    let mut fired = Vec::new();
    update(runtime, |r| {
        note_pools_switch(r, &pools, now);
        fired = note_unstable_pools(r, seen, unstable, now);
    })
    .await;
    fired
}

/// relay 里**全部**池 selector：全局池 [`super::POOL`] + 每槽一个（[`super::slot_selector`]）。
/// 池未启用时 relay 是 fail-open 直连、一个 selector 都没有 ⇒ 空表。
pub fn all_pool_selectors(s: &State) -> Vec<String> {
    if !group_of(s).pool_active() {
        return Vec::new();
    }
    std::iter::once(super::POOL.to_string())
        .chain(
            bui_schema::slots::sorted(&s.residential)
                .iter()
                .map(|sl| super::slot_selector(sl.index)),
        )
        .collect()
}

/// 记一条告警：同文案去重、最新的排在最前、截断到 [`ALERTS_MAX`]
pub fn push_alert(r: &mut ResiRuntime, msg: impl Into<String>) {
    let msg = msg.into();
    r.alerts.retain(|a| a != &msg);
    r.alerts.insert(0, msg);
    r.alerts.truncate(ALERTS_MAX);
}

/// 按前缀移除全局告警：某条路径**恢复了**时认领它自己以前写的那类告警。返回移除条数
pub fn remove_alerts_with_prefix(r: &mut ResiRuntime, prefix: &str) -> usize {
    let before = r.alerts.len();
    r.alerts.retain(|a| !a.starts_with(prefix));
    before - r.alerts.len()
}

/// 记一条上游级告警（同一 uuid 只留最新一条）。`msg` 里请写 `host:port`，别写 `url-N`
pub fn set_upstream_alert(r: &mut ResiRuntime, id: Uuid, msg: impl Into<String>) {
    r.upstream_alerts.insert(id, msg.into());
}

/// 清掉该上游的告警：巡检/体检成功一次，或条目被删
pub fn clear_upstream_alert(r: &mut ResiRuntime, id: Uuid) {
    r.upstream_alerts.remove(&id);
}

/// 面板与 CLI 看到的告警：全局告警（新的在前）+ **仍在池里**的上游的告警（按池内顺序）。
/// 池里已经没有的 uuid 一律不显示 —— 删掉的上游不该继续刷屏。
pub fn visible_alerts(g: &ResidentialGroup, r: &ResiRuntime) -> Vec<String> {
    let mut out = r.alerts.clone();
    out.extend(
        g.upstreams
            .iter()
            .filter_map(|u| r.upstream_alerts.get(&u.id).cloned()),
    );
    out
}

/// R1 之前的上游级告警是按**位置名**拼成字符串写进 [`ResiRuntime::alerts`] 的
/// （`上游 url-3 凭据失效（407）`）。这些条目既不以 uuid 为键、也不会被「探通一次即清」
/// 认领，会在 status / 面板上永久刷屏：2026-09-12 bwg-rick 升级到含 R1 的 v4 后，池里
/// 三条 Decodo 连续成功 109 次、体检全通，`bui residential status` 仍挂着 5 条旧告警。
/// R1 之后的代码只把上游级告警写进 [`ResiRuntime::upstream_alerts`]，所以这条判据纯粹
/// 是一次性的迁移过滤，不会误伤新写入的全局告警（它们都不是这个形状）。
fn is_legacy_upstream_alert(msg: &str) -> bool {
    msg.starts_with("上游 ") && msg.contains("凭据失效")
}

/// 丢掉所有**不以池内现存 uuid 为键**的上游级告警：R1 之前的纯字符串条目，以及键是
/// uuid 但该 uuid 已不在池里的条目。全局告警（全员不达标、切换失败、journalctl 缺失）
/// 留着。返回是否真的改了，调用方据此决定要不要写盘。
fn retain_live_alerts(g: &ResidentialGroup, r: &mut ResiRuntime) -> bool {
    let before = (r.alerts.len(), r.upstream_alerts.len());
    r.alerts.retain(|a| !is_legacy_upstream_alert(a));
    let live: std::collections::BTreeSet<Uuid> = g.upstreams.iter().map(|u| u.id).collect();
    r.upstream_alerts.retain(|id, _| live.contains(id));
    before != (r.alerts.len(), r.upstream_alerts.len())
}

/// 加载 runtime（status / health 每次读）与每轮巡检时清一次遗留/孤儿告警，并把结果写回
/// runtime —— **只在真有变化时写**：本方法每 2 分钟一轮、每次看板刷新都会被调，
/// 而每次 `update` 都是 tmp + fsync + rename。
pub async fn purge_stale_alerts(runtime: &Runtime, g: &ResidentialGroup) -> ResiRuntime {
    let mut r = read(runtime).await;
    if !retain_live_alerts(g, &mut r) {
        return r;
    }
    update(runtime, |r| {
        retain_live_alerts(g, r);
    })
    .await
}

/// 读当前住宅分组（不存在时给 `Default`，等价于「池未启用」→ relay fail-open 直连）
pub fn group_of(s: &State) -> ResidentialGroup {
    s.residential
        .groups
        .get(GROUP_DEFAULT)
        .cloned()
        .unwrap_or_default()
}

/// **改住宅状态的唯一入口**：写 state → 发 `Event::StateChanged("residential")`
/// → P1 的 500ms 去抖对账重渲染 relay（契约决策 §B）
pub async fn update_group(
    store: &Store,
    bus: &EventBus,
    f: impl FnOnce(&mut ResidentialGroup),
) -> anyhow::Result<()> {
    update_group_as(store, bus, crate::state::store::CALLER_UNLABELED, f).await
}

/// 同 [`update_group`]，但带调用点标签。**只有 `upstream::remove` 需要它**：
/// `Store::update_as` 的防线只让 [`crate::state::store::CALLER_RESI_REMOVE`]
/// 缩短上游池（R3 ①）。
pub async fn update_group_as(
    store: &Store,
    bus: &EventBus,
    caller: &'static str,
    f: impl FnOnce(&mut ResidentialGroup),
) -> anyhow::Result<()> {
    store
        .update_as(caller, |s| {
            let g = s
                .residential
                .groups
                .entry(GROUP_DEFAULT.to_string())
                .or_default();
            f(g);
        })
        .await?;
    // 对账由 P1 的去抖消费者跑：重渲染 singbox-relay.json → sing-box check → 重启 b-ui-relay
    bus.send(Event::StateChanged("residential"));
    Ok(())
}

/// 迟滞状态机（spec §5.3）：返回本轮之后该成员是否算健康
pub fn apply_hysteresis(h: &mut HealthState, ok: bool) -> bool {
    if ok {
        h.okstreak += 1;
        h.failstreak = 0;
        if !h.active && h.okstreak >= OK_TO_HEALTHY {
            h.active = true;
        }
    } else {
        h.failstreak += 1;
        h.okstreak = 0;
        if h.active && h.failstreak >= FAIL_TO_UNHEALTHY {
            h.active = false;
        }
    }
    h.active
}

/// 哨兵带外探测确认不可达（spec §5.7，设计裁决 D6）：**立即**判不健康（不等巡检的 2 轮迟滞），
/// 并记一条失败样本。否则下一轮巡检里它仍是 `active`，按槽驱动会把「还坏着的一轮」当成
/// 「恢复第 1 轮」。恢复照旧走 [`apply_hysteresis`]（连续 [`OK_TO_HEALTHY`] 轮）。
pub fn mark_unhealthy(h: &mut HealthState, now: OffsetDateTime) {
    record_probe(h, false, now);
    h.active = false;
    h.okstreak = 0;
    h.failstreak = h.failstreak.max(FAIL_TO_UNHEALTHY);
}

/// 记一条样本并裁掉窗口外/超量的（`now` 由 `Host::now()` 给，测试可推进）
pub fn record_probe(h: &mut HealthState, ok: bool, now: OffsetDateTime) {
    h.samples.push(ProbeSample {
        at: fmt_rfc3339(now),
        ok,
    });
    let cutoff = now - time::Duration::seconds(RATE_WINDOW_SECS);
    // 解析不出时间戳的旧样本一并丢掉（换过格式或文件被手改）
    h.samples
        .retain(|s| parse_rfc3339(&s.at).map(|t| t >= cutoff).unwrap_or(false));
    if h.samples.len() > RATE_SAMPLES_MAX {
        let drop = h.samples.len() - RATE_SAMPLES_MAX;
        h.samples.drain(..drop);
    }
}

/// 记一次 Google 可达性判定（R2 ②）。`None` = 本轮没探到结论 ⇒ **不动**上次的结论：
/// 这一项参与选路，一次超时把「通」翻成「封」就会把好上游踢到队尾。
pub fn record_google(h: &mut HealthState, ok: Option<bool>, now: OffsetDateTime) {
    if let Some(ok) = ok {
        h.google_ok = Some(ok);
        h.google_at = Some(fmt_rfc3339(now));
    }
}

/// 环形推入：超过 `cap` 就从头丢（`runtime.json` 不会无限长）。
/// `Vec` 而不是 `VecDeque`：它要 serde 成 JSON 数组，面板也直接画这串数
fn push_ring<T>(v: &mut Vec<T>, x: T, cap: usize) {
    v.push(x);
    if v.len() > cap {
        let drop = v.len() - cap;
        v.drain(..drop);
    }
}

/// 记一轮的延迟样本（毫秒）。`None` 的那一项**不入样本** —— 没测到不等于 0 毫秒，
/// 写成 0 会让一条连不上的上游拿到全池最低延迟。
pub fn record_latency(h: &mut HealthState, http: Option<u64>, tcp: Option<u64>, udp: Option<u64>) {
    for (samples, ms) in [
        (&mut h.http_ms, http),
        (&mut h.tcp_ms, tcp),
        (&mut h.udp_ms, udp),
    ] {
        if let Some(ms) = ms {
            push_ring(samples, ms, LATENCY_SAMPLES_MAX);
        }
    }
}

/// 第 `p` 百分位（毫秒，`p` 取 0–100）。空样本 `None`（未知不是 0）。
/// 最近邻插值：排序后取第 `ceil(p/100 * n)` 个（1-based），30 个样本时 p50 = 第 15、
/// p95 = 第 29。样本只有 30 个，线性插值的精度没有意义，只会让口径难解释。
pub fn percentile(samples: &[u64], p: f64) -> Option<u64> {
    if samples.is_empty() {
        return None;
    }
    let mut s = samples.to_vec();
    s.sort_unstable();
    let rank = ((p / 100.0) * s.len() as f64).ceil().max(1.0) as usize;
    s.get(rank.min(s.len()) - 1).copied()
}

/// 选路用的「延迟」：**经上游的完整 HTTP 往返** p50（用户体感），还没攒到 HTTP
/// 样本时退回到 TCP 建连耗时 —— 否则刚加进来的成员永远没有延迟依据、排序全靠 idx。
pub fn latency_p50(h: &HealthState) -> Option<u64> {
    percentile(&h.http_ms, 50.0).or_else(|| percentile(&h.tcp_ms, 50.0))
}

/// 测速样本的中位数（Mbps）。空 `None`；取中位数而不是平均数：一次撞上上游限速的
/// 慢样本不该把结论拉走
pub fn median(samples: &[f64]) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }
    let mut s = samples.to_vec();
    // f64 没有 Ord；样本是速率，不会有 NaN（`mbps` 已挡掉除零）
    s.sort_by(f64::total_cmp);
    Some(s[(s.len() - 1) / 2])
}

/// 记一次测速。失败（`None`）**不写样本、不碰健康位**，只留 `note`（主理人口径：
/// 「测速失败不影响健康判定，只记 note」）；成功一次就把上次的 note 清掉。
pub fn record_speed(
    h: &mut HealthState,
    down: Option<f64>,
    up: Option<f64>,
    note: Option<String>,
    now: OffsetDateTime,
) {
    if let Some(d) = down {
        push_ring(&mut h.down_mbps, d, SPEEDTEST_SAMPLES_MAX);
    }
    if let Some(u) = up {
        push_ring(&mut h.up_mbps, u, SPEEDTEST_SAMPLES_MAX);
    }
    h.speed_note = note;
    h.speed_at = Some(fmt_rfc3339(now));
}

/// 记一次 UDP 探测（STUN）。这一项每轮都有确定结论（http 上游也有：恒不通 + 标注），
/// 所以与 [`record_google`] 不同，不做「没结论就保留上次」——直接覆盖。
/// 耗时只在通的时候入样本（超时的 5 秒不是延迟）。
pub fn record_udp(h: &mut HealthState, v: &super::proxy::UdpProbe, now: OffsetDateTime) {
    h.udp_ok = Some(v.ok);
    h.udp_exit_ip = v.exit_ip.clone();
    h.udp_note = v.note.clone();
    h.udp_at = Some(fmt_rfc3339(now));
    if v.ok {
        record_latency(h, None, None, v.ms);
    }
}

/// 近 24h 成功率；无样本返回 0.0（「没数据」不该赢过「有数据且全成功」）
pub fn success_rate_24h(h: &HealthState, now: OffsetDateTime) -> f64 {
    let cutoff = now - time::Duration::seconds(RATE_WINDOW_SECS);
    let (mut total, mut ok) = (0u32, 0u32);
    for s in h
        .samples
        .iter()
        .filter(|s| parse_rfc3339(&s.at).map(|t| t >= cutoff).unwrap_or(false))
    {
        total += 1;
        if s.ok {
            ok += 1;
        }
    }
    if total == 0 {
        return 0.0;
    }
    f64::from(ok) / f64::from(total)
}

/// `/api/health` 的 `residential` 字段（P1 Task 13 的 `HealthResponse.residential`）
pub fn health_summary(g: &ResidentialGroup, r: &ResiRuntime) -> serde_json::Value {
    serde_json::json!({
        "enabled": g.pool_active(),
        "mode": g.mode,
        "upstreams": g.upstreams.len(),
        "selected_upstream_id": r.selected_upstream_id,
        "selected_pending_persist": r.selected_pending_persist,
        "unhealthy": r.health.values().filter(|h| !h.active).count(),
        "blacklist": { "pins": g.blacklist.pins.len(), "auto": g.blacklist.auto.len(),
                       "pending": r.pending.len(), "candidates": r.candidates.len() },
        "checking": r.checking,
        "last_daily_at": r.last_daily_at,
        "alerts": visible_alerts(g, r),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Event, EventBus};
    use crate::state::runtime::Runtime;
    use crate::state::store::Store;
    use crate::testutil::sample_state;
    use pretty_assertions::assert_eq;
    use time::macros::datetime;

    fn t0() -> OffsetDateTime {
        datetime!(2026-09-12 00:00:00 UTC)
    }

    #[test]
    fn hysteresis_needs_two_rounds_in_each_direction() {
        let mut h = HealthState::default();
        assert!(h.active, "新成员默认健康（没探过不等于坏）");
        assert!(apply_hysteresis(&mut h, false), "第 1 轮失败还不剔除");
        assert_eq!((h.failstreak, h.okstreak), (1, 0));
        assert!(!apply_hysteresis(&mut h, false), "第 2 轮失败才剔除");
        assert!(!h.active);
        assert!(!apply_hysteresis(&mut h, true), "第 1 轮成功还不恢复");
        assert!(apply_hysteresis(&mut h, true), "第 2 轮成功才恢复");
        assert_eq!((h.failstreak, h.okstreak), (0, 2));
    }

    #[test]
    fn success_rate_only_counts_the_last_24h_and_caps_samples() {
        let mut h = HealthState::default();
        record_probe(&mut h, false, t0());
        record_probe(&mut h, true, t0() + time::Duration::hours(1));
        assert_eq!(success_rate_24h(&h, t0() + time::Duration::hours(2)), 0.5);
        // 25 小时后那条失败样本已出窗，且在写入时就被裁掉
        record_probe(&mut h, true, t0() + time::Duration::hours(25));
        assert_eq!(h.samples.len(), 2, "窗口外的样本在写入时裁掉");
        assert_eq!(success_rate_24h(&h, t0() + time::Duration::hours(25)), 1.0);
        assert_eq!(
            success_rate_24h(&HealthState::default(), t0()),
            0.0,
            "无样本记 0：不能让没探过的成员赢过全成功的成员"
        );
        for i in 0..(RATE_SAMPLES_MAX + 50) {
            record_probe(
                &mut h,
                true,
                t0() + time::Duration::hours(25) + time::Duration::seconds(i as i64),
            );
        }
        assert_eq!(
            h.samples.len(),
            RATE_SAMPLES_MAX,
            "样本数有上限，runtime.json 不会无限长"
        );
    }

    #[tokio::test]
    async fn runtime_round_trips_through_the_extra_map() {
        let d = tempfile::tempdir().unwrap();
        let runtime = Runtime::load(d.path().join("runtime.json"));
        let id = Uuid::from_u128(2);
        update(&runtime, |r| {
            r.selected_upstream_id = Some(id);
            r.health.insert(id.to_string(), HealthState::default());
        })
        .await;
        // extra 是 flatten，所以落盘后顶层就有一个 "residential" 键
        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(d.path().join("runtime.json")).unwrap())
                .unwrap();
        assert_eq!(raw["residential"]["selected_upstream_id"], id.to_string());
        assert!(
            raw["residential"]["health"].get(id.to_string()).is_some(),
            "health 以 uuid 为键"
        );
        assert_eq!(read(&runtime).await.selected_upstream_id, Some(id));
        assert!(
            raw.get("restart_keys").is_some(),
            "P1 自己的字段不被本模块碰掉"
        );
    }

    #[test]
    fn a_google_verdict_is_only_overwritten_by_a_conclusive_round() {
        let mut h = HealthState::default();
        assert_eq!(h.google_ok, None, "没探过就是未知");
        record_google(&mut h, Some(true), t0());
        assert_eq!(h.google_ok, Some(true));
        assert_eq!(h.google_at.as_deref(), Some("2026-09-12T00:00:00Z"));
        // 一轮没探到结论（超时 / 407）不该把「通」翻成「封」：它参与选路
        record_google(&mut h, None, t0() + time::Duration::hours(1));
        assert_eq!(h.google_ok, Some(true));
        assert_eq!(h.google_at.as_deref(), Some("2026-09-12T00:00:00Z"));
        record_google(&mut h, Some(false), t0() + time::Duration::hours(2));
        assert_eq!(h.google_ok, Some(false));
        assert_eq!(h.google_at.as_deref(), Some("2026-09-12T02:00:00Z"));
    }

    #[test]
    fn latency_samples_are_a_ring_of_thirty_and_report_p50_p95() {
        let mut h = HealthState::default();
        assert_eq!(percentile(&h.http_ms, 50.0), None, "没样本就是未知");
        // 1..=30 毫秒：p50 = 第 15 个（1-based ceil(0.5*30)），p95 = 第 29 个
        for i in 1..=30u64 {
            record_latency(&mut h, Some(i), None, None);
        }
        assert_eq!(h.http_ms.len(), 30);
        assert_eq!(percentile(&h.http_ms, 50.0), Some(15));
        assert_eq!(percentile(&h.http_ms, 95.0), Some(29));
        // 第 31 个把最老的 1 挤出去（环形，runtime.json 不会无限长）
        record_latency(&mut h, Some(999), None, None);
        assert_eq!(h.http_ms.len(), LATENCY_SAMPLES_MAX);
        assert_eq!(h.http_ms[0], 2, "最老的样本被挤掉");
        assert_eq!(h.http_ms[29], 999);
        // 单样本：p50 与 p95 都是它自己
        let mut one = HealthState::default();
        record_latency(&mut one, None, Some(7), Some(8));
        assert_eq!(percentile(&one.tcp_ms, 50.0), Some(7));
        assert_eq!(percentile(&one.tcp_ms, 95.0), Some(7));
        assert_eq!(percentile(&one.udp_ms, 50.0), Some(8));
        assert!(one.http_ms.is_empty(), "None 不入样本（没测到 ≠ 0 毫秒）");
        // 三种延迟分开存（TCP / HTTP / UDP 量的是不同的东西）
        assert_eq!(one.tcp_ms, vec![7]);
        assert_eq!(one.udp_ms, vec![8]);
    }

    #[test]
    fn latency_for_routing_prefers_the_http_round_trip_and_falls_back_to_tcp() {
        // 选路用的「延迟」是**经上游的完整往返**：它才是用户体感。只有还没攒到
        // HTTP 样本时才退回 TCP 建连耗时（否则新成员永远没有延迟依据）
        let mut h = HealthState::default();
        assert_eq!(latency_p50(&h), None);
        record_latency(&mut h, None, Some(20), None);
        assert_eq!(latency_p50(&h), Some(20), "只有 TCP 样本 ⇒ 用 TCP");
        record_latency(&mut h, Some(120), Some(20), None);
        assert_eq!(latency_p50(&h), Some(120), "有 HTTP 样本就用 HTTP");
    }

    #[test]
    fn speed_samples_keep_six_rounds_and_report_the_median() {
        let mut h = HealthState::default();
        assert_eq!(median(&h.down_mbps), None);
        let now = t0();
        for (i, v) in [10.0, 30.0, 20.0].iter().enumerate() {
            record_speed(
                &mut h,
                Some(*v),
                Some(v / 2.0),
                None,
                now + time::Duration::hours(i as i64),
            );
        }
        assert_eq!(median(&h.down_mbps), Some(20.0), "10/20/30 的中位数");
        assert_eq!(median(&h.up_mbps), Some(10.0));
        assert_eq!(h.speed_at.as_deref(), Some("2026-09-12T02:00:00Z"));
        for i in 0..10 {
            record_speed(
                &mut h,
                Some(1.0),
                Some(1.0),
                None,
                now + time::Duration::hours(10 + i),
            );
        }
        assert_eq!(h.down_mbps.len(), SPEEDTEST_SAMPLES_MAX, "只留最近 6 次");
        // 测速失败只记 note，**不影响健康判定**、也不把样本写成 0
        record_speed(
            &mut h,
            None,
            None,
            Some("下载测速失败：timeout".into()),
            now,
        );
        assert_eq!(h.down_mbps.len(), SPEEDTEST_SAMPLES_MAX);
        assert_eq!(median(&h.down_mbps), Some(1.0));
        assert_eq!(h.speed_note.as_deref(), Some("下载测速失败：timeout"));
        assert!(h.active, "测速失败不碰健康位");
        // 成功一次就把上次的 note 清掉
        record_speed(&mut h, Some(2.0), Some(2.0), None, now);
        assert_eq!(h.speed_note, None);
    }

    #[test]
    fn a_udp_verdict_records_the_exit_ip_and_only_a_probed_round_overwrites_it() {
        let mut h = HealthState::default();
        assert_eq!(h.udp_ok, None, "没探过就是未知");
        record_udp(
            &mut h,
            &crate::modules::residential::proxy::UdpProbe {
                ok: true,
                exit_ip: Some("198.51.100.7".into()),
                ms: Some(42),
                note: None,
            },
            t0(),
        );
        assert_eq!(h.udp_ok, Some(true));
        assert_eq!(h.udp_exit_ip.as_deref(), Some("198.51.100.7"));
        assert_eq!(h.udp_at.as_deref(), Some("2026-09-12T00:00:00Z"));
        assert_eq!(h.udp_ms, vec![42], "UDP 耗时与 TCP/HTTP 分开存");
        // http 上游：恒不通 + 标注，且不留下上一次的出口 IP
        record_udp(
            &mut h,
            &crate::modules::residential::proxy::UdpProbe {
                ok: false,
                exit_ip: None,
                ms: None,
                note: Some("HTTP 上游无 UDP".into()),
            },
            t0() + time::Duration::hours(1),
        );
        assert_eq!(h.udp_ok, Some(false));
        assert_eq!(h.udp_exit_ip, None);
        assert_eq!(h.udp_note.as_deref(), Some("HTTP 上游无 UDP"));
        assert_eq!(h.udp_ms, vec![42], "失败不入耗时样本");
    }

    #[tokio::test]
    async fn the_metric_fields_round_trip_and_an_old_runtime_json_still_loads() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("runtime.json");
        // R1 之前的 runtime.json 没有这些字段：容器级 #[serde(default)] 必须让它照样读回
        std::fs::write(
            &p,
            br#"{"residential":{"health":{"u1":{"active":true,"okstreak":3}}}}"#,
        )
        .unwrap();
        let runtime = Runtime::load(&p);
        let r = read(&runtime).await;
        assert_eq!(r.health["u1"].okstreak, 3);
        assert!(r.health["u1"].http_ms.is_empty());
        assert_eq!(r.health["u1"].udp_ok, None);
        assert_eq!(r.last_speedtest_at, None);
        assert_eq!(r.improve_rounds, 0);
        let id = Uuid::from_u128(9);
        update(&runtime, |r| {
            let h = r.health.entry("u1".into()).or_default();
            record_latency(h, Some(90), Some(30), Some(40));
            record_speed(h, Some(88.5), Some(12.0), None, t0());
            r.last_speedtest_at = Some(fmt_rfc3339(t0()));
            r.improve_candidate_id = Some(id);
            r.improve_rounds = 2;
        })
        .await;
        let back = read(&Runtime::load(&p)).await;
        assert_eq!(back.health["u1"].http_ms, vec![90]);
        assert_eq!(back.health["u1"].down_mbps, vec![88.5]);
        assert_eq!(
            back.last_speedtest_at.as_deref(),
            Some("2026-09-12T00:00:00Z")
        );
        assert_eq!(back.improve_candidate_id, Some(id));
        assert_eq!(back.improve_rounds, 2);
    }

    #[tokio::test]
    async fn the_manual_lock_round_trips_through_the_extra_map() {
        let d = tempfile::tempdir().unwrap();
        let runtime = Runtime::load(d.path().join("runtime.json"));
        let id = Uuid::from_u128(3);
        update(&runtime, |r| r.manual_selected_id = Some(id)).await;
        assert_eq!(read(&runtime).await.manual_selected_id, Some(id));
        // 重启也要记得锁定（runtime.json 落盘）
        let again = Runtime::load(d.path().join("runtime.json"));
        assert_eq!(read(&again).await.manual_selected_id, Some(id));
    }

    #[tokio::test]
    async fn alerts_dedupe_and_cap() {
        let d = tempfile::tempdir().unwrap();
        let runtime = Runtime::load(d.path().join("runtime.json"));
        let r = update(&runtime, |r| {
            push_alert(r, "全部上游探测不达标，保持当前出口");
            push_alert(r, "全部上游探测不达标，保持当前出口");
            for i in 0..30 {
                push_alert(r, format!("告警 {i}"));
            }
        })
        .await;
        assert_eq!(r.alerts.len(), ALERTS_MAX);
        assert_eq!(r.alerts[0], "告警 29", "最新的在最前");
    }

    #[tokio::test]
    async fn update_group_writes_state_and_fires_state_changed() {
        let d = tempfile::tempdir().unwrap();
        // sample_state() 的住宅段是 mode=split，所以这里改成 Global 才真的产生一次写盘
        // （`Store::update` 零变更不写盘，改成同值等于什么都没测）
        let store = Store::create(d.path().join("state.json"), sample_state())
            .await
            .unwrap();
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        update_group(&store, &bus, |g| {
            g.mode = bui_schema::model::ResiMode::Global
        })
        .await
        .unwrap();
        assert_eq!(
            store.read().await.residential.groups[GROUP_DEFAULT].mode,
            bui_schema::model::ResiMode::Global
        );
        // 从磁盘读回，确认真的落盘了（不是只改了内存缓存）
        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(d.path().join("state.json")).unwrap())
                .unwrap();
        assert_eq!(raw["residential"]["groups"]["default"]["mode"], "global");
        assert_eq!(rx.try_recv().unwrap(), Event::StateChanged("residential"));
    }

    #[tokio::test]
    async fn upstream_alerts_are_keyed_by_uuid_and_only_shown_for_members_still_in_the_pool() {
        let d = tempfile::tempdir().unwrap();
        let runtime = Runtime::load(d.path().join("runtime.json"));
        let s = crate::modules::residential::sample_state_with_pool();
        let g = group_of(&s);
        let live = g.upstreams[0].id;
        let gone = Uuid::from_u128(0xdead);
        let r = update(&runtime, |r| {
            push_alert(r, "机器上没有 journalctl，黑名单候选只能靠每日探针集");
            set_upstream_alert(r, live, "上游 isp.example.net:10007 凭据失效（407）");
            set_upstream_alert(r, gone, "上游 old.example.net:10007 凭据失效（407）");
            // 同一 uuid 只留最新一条
            set_upstream_alert(
                r,
                live,
                "上游 isp.example.net:10007 凭据失效（407），请更新凭据",
            );
        })
        .await;
        assert_eq!(r.upstream_alerts.len(), 2);
        let visible = visible_alerts(&g, &r);
        assert_eq!(
            visible,
            vec![
                "机器上没有 journalctl，黑名单候选只能靠每日探针集".to_string(),
                "上游 isp.example.net:10007 凭据失效（407），请更新凭据".to_string(),
            ],
            "只显示池内现存 uuid 的告警，且文案用 host:port 而不是 url-N"
        );
        assert!(
            !visible.iter().any(|a| a.contains("url-")),
            "位置名会被新条目复用，告警文案里不许出现：{visible:?}"
        );
        // 恢复一次就清掉这条上游的告警
        let r = update(&runtime, |r| clear_upstream_alert(r, live)).await;
        assert!(!r.upstream_alerts.contains_key(&live));
        assert_eq!(visible_alerts(&g, &r).len(), 1, "全局告警不受影响");
    }

    #[tokio::test]
    async fn loading_drops_legacy_and_orphan_upstream_alerts() {
        // 真机形态（2026-09-12 bwg-rick 升级到含 R1 的 v4 后）：池里三条 Decodo 连续成功
        // 109 次、体检全通，`bui residential status` 仍挂着 R1 之前按位置名写进 alerts 的
        // 旧告警，以及删掉的上游留在 upstream_alerts 里的孤儿条目。
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("runtime.json");
        let runtime = Runtime::load(&p);
        let s = crate::modules::residential::sample_state_with_pool();
        let g = group_of(&s);
        let live = g.upstreams[0].id;
        let gone = Uuid::from_u128(0xdead);
        update(&runtime, |r| {
            push_alert(r, "上游 url-3 凭据失效（407）");
            push_alert(
                r,
                "上游 url-6 凭据失效（407 / SOCKS5 认证被拒），请更新凭据",
            );
            set_upstream_alert(r, gone, "上游 old.example.net:10007 凭据失效（407）");
            set_upstream_alert(r, live, "上游 isp.example.net:10007 凭据失效（407）");
        })
        .await;

        let r = purge_stale_alerts(&runtime, &g).await;
        assert_eq!(
            visible_alerts(&g, &r),
            vec!["上游 isp.example.net:10007 凭据失效（407）".to_string()],
            "只留按池内现存 uuid 存的那一条"
        );
        assert!(r.alerts.is_empty(), "旧格式的纯字符串上游告警一条不留");
        assert_eq!(r.upstream_alerts.len(), 1);
        // 清理结果必须落盘：不然下次加载又是 5 条
        assert_eq!(read(&Runtime::load(&p)).await, r, "清理结果写回 runtime");
    }

    #[tokio::test]
    async fn purging_keeps_global_alerts_and_does_not_write_when_nothing_changed() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("runtime.json");
        let g = group_of(&crate::modules::residential::sample_state_with_pool());

        // 干净的 runtime：一次写盘都不该有（本方法每轮巡检、每次 status 都会被调）
        let runtime = Runtime::load(&p);
        purge_stale_alerts(&runtime, &g).await;
        assert!(!p.exists(), "无变化不写盘");

        // 全局告警（全员不达标、切换失败、journalctl 缺失）不是上游级告警，不许被清掉
        update(&runtime, |r| {
            push_alert(r, "机器上没有 journalctl，黑名单候选只能靠每日探针集");
            push_alert(r, "全部住宅上游探测不达标，出口已降级但未切换");
            push_alert(r, "切换住宅出口到 resi-2 失败：connection refused");
        })
        .await;
        let r = purge_stale_alerts(&runtime, &g).await;
        assert_eq!(r.alerts.len(), 3, "全局告警不受影响：{:?}", r.alerts);
    }

    #[test]
    fn health_summary_is_json_the_panel_can_read() {
        // 用本模块自己的夹具：P1 的 sample_state() 住宅段是空池（upstreams/pins 都是 0）
        let s = crate::modules::residential::sample_state_with_pool();
        let g = group_of(&s);
        let id = g.upstreams[0].id;
        let r = ResiRuntime {
            selected_upstream_id: Some(id),
            alerts: vec!["x".into()],
            // 已被删掉的上游留下的告警：不该再出现在任何输出里
            upstream_alerts: [(
                Uuid::from_u128(0xdead),
                "上游 old.example.net:1 凭据失效".into(),
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let v = health_summary(&g, &r);
        assert_eq!(v["enabled"], true);
        assert_eq!(v["upstreams"], 1);
        assert_eq!(v["selected_upstream_id"], id.to_string());
        assert_eq!(v["unhealthy"], 0, "没探过的成员默认健康");
        assert_eq!(v["blacklist"]["pins"], 1);
        assert_eq!(v["blacklist"]["auto"], 1);
        assert_eq!(
            v["alerts"],
            serde_json::json!(["x"]),
            "/api/health 也只透出池内现存 uuid 的告警"
        );
    }

    /// 哨兵带外确认不可达 ⇒ 立即不健康；恢复仍走巡检的 2 轮迟滞（设计裁决 D6）
    #[test]
    fn an_out_of_band_failure_marks_a_member_down_at_once_but_recovery_still_needs_two_rounds() {
        let mut h = HealthState::default();
        mark_unhealthy(&mut h, t0());
        assert!(!h.active);
        assert_eq!((h.okstreak, h.failstreak), (0, FAIL_TO_UNHEALTHY));
        assert_eq!(h.samples.len(), 1);
        assert!(!h.samples[0].ok, "带外失败也进 24h 成功率");
        assert!(!apply_hysteresis(&mut h, true), "第 1 轮探通还不恢复");
        assert!(apply_hysteresis(&mut h, true), "第 2 轮才恢复");
    }
}
