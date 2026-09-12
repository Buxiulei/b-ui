//! 住宅模块的运行时数据（`runtime.json` 的 `extra["residential"]`）与状态读写助手。
use super::{
    FAIL_TO_UNHEALTHY, GROUP_DEFAULT, OK_TO_HEALTHY, RATE_SAMPLES_MAX, RATE_WINDOW_SECS,
    RUNTIME_KEY,
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
    pub last_switch_at: Option<String>,
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
    /// 最近告警（全不健康、凭据失效、journalctl 缺失…），上限 [`ALERTS_MAX`]，新的在前
    pub alerts: Vec<String>,
    pub last_daily_at: Option<String>,
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
}

impl Default for HealthState {
    fn default() -> Self {
        // 新成员默认健康：没探过 ≠ 坏。与 v3 `resi-health.sh` 的 `.active // true` 同义。
        Self {
            active: true,
            okstreak: 0,
            failstreak: 0,
            samples: Vec::new(),
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

/// 记一条告警：同文案去重、最新的排在最前、截断到 [`ALERTS_MAX`]
pub fn push_alert(r: &mut ResiRuntime, msg: impl Into<String>) {
    let msg = msg.into();
    r.alerts.retain(|a| a != &msg);
    r.alerts.insert(0, msg);
    r.alerts.truncate(ALERTS_MAX);
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
    store
        .update(|s| {
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
        "alerts": r.alerts,
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

    #[test]
    fn health_summary_is_json_the_panel_can_read() {
        // 用本模块自己的夹具：P1 的 sample_state() 住宅段是空池（upstreams/pins 都是 0）
        let s = crate::modules::residential::sample_state_with_pool();
        let g = group_of(&s);
        let id = g.upstreams[0].id;
        let r = ResiRuntime {
            selected_upstream_id: Some(id),
            alerts: vec!["x".into()],
            ..Default::default()
        };
        let v = health_summary(&g, &r);
        assert_eq!(v["enabled"], true);
        assert_eq!(v["upstreams"], 1);
        assert_eq!(v["selected_upstream_id"], id.to_string());
        assert_eq!(v["unhealthy"], 0, "没探过的成员默认健康");
        assert_eq!(v["blacklist"]["pins"], 1);
        assert_eq!(v["blacklist"]["auto"], 1);
        assert_eq!(v["alerts"][0], "x");
    }
}
