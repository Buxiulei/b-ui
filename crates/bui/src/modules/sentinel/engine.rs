//! 去抖与冷却（spec §5.7，设计裁决 D12）+ 哨兵自己的运行时段（`runtime.extra["sentinel"]`）。
//! [`Engine`] 是纯状态机（时钟由调用方传入），只活在内存里；冷却表与游标在 [`SentinelRuntime`]，落盘。

use super::signature::Sig;
use super::{ACTION_COOLDOWN_SECS, DEBOUNCE_SECS};
use crate::state::runtime::RuntimeData;
use crate::util::parse_rfc3339;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

/// `RuntimeData.extra` 里哨兵独占的键
pub const SENTINEL_KEY: &str = "sentinel";
/// [`Engine::prune`] 丢掉「最后一条命中早于这么久」的计数：比最长的签名窗口（150 秒）宽
const PRUNE_AFTER_SECS: i64 = 300;

/// 哨兵的持久化运行时段。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SentinelRuntime {
    /// 上一次落盘时读到的最后一条 `__CURSOR`（内存里的更新，落盘有节流，设计裁决 D1）
    pub cursor: Option<String>,
    /// 与 `cursor` 成对：读到它时的本轮时刻（到这一刻为止的日志都已读过）。重启时它早于最长签名窗口
    /// ⇒ 丢掉游标、从现在读起（`run::tick`）
    pub cursor_at: Option<String>,
    /// 没有游标时从这一刻读起（首次启动 / 游标失效时写成「当时的现在」，不回放历史）
    pub since: Option<String>,
    /// 冷却表：`<动作 id>|<对象>` → 上次执行时刻（RFC3339）
    pub acted: BTreeMap<String, String>,
    /// 住宅上游被判不健康的起点（30 分钟建议替换，设计裁决 D15）
    pub down_since: BTreeMap<Uuid, String>,
    /// 这一段不可达已经建议过替换的上游（恢复即清）
    pub suggested: BTreeSet<Uuid>,
}

/// 读哨兵段；缺失或解析失败按空值重建（`runtime.json` 可丢可重建）
pub fn sentinel_of(rt: &RuntimeData) -> SentinelRuntime {
    rt.extra
        .get(SENTINEL_KEY)
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

pub fn put_sentinel(rt: &mut RuntimeData, s: &SentinelRuntime) {
    // to_value 只会在自定义 Serialize 里失败，这里全是 derive
    if let Ok(v) = serde_json::to_value(s) {
        rt.extra.insert(SENTINEL_KEY.to_string(), v);
    }
}

/// 同签名同对象的计数键
pub fn sig_key(sig: Sig, subject: &str) -> String {
    format!("{}|{subject}", sig.id())
}

/// 冷却键：同一个**动作**落在同一个对象上（两种签名共用一个动作时共享冷却）
pub fn action_key(sig: Sig, subject: &str) -> String {
    format!("{}|{subject}", sig.action().id())
}

/// 去抖计数器：每个 `sig|subject` 一串命中时刻 + 上次触发时刻。
#[derive(Debug, Default)]
pub struct Engine {
    hits: BTreeMap<String, VecDeque<OffsetDateTime>>,
    fired: BTreeMap<String, OffsetDateTime>,
}

impl Engine {
    /// 喂一条命中（`ts` = 日志时刻，`now` = 本轮时刻）。返回 `true` = 它让该签名在窗口内
    /// 达到门槛、且距上次触发已过 [`DEBOUNCE_SECS`] ⇒ **本轮触发一次**（触发后计数清零）。
    /// 早于窗口的日志直接不计（重启后续读的积压描述的是过去）。
    pub fn observe(
        &mut self,
        sig: Sig,
        subject: &str,
        ts: OffsetDateTime,
        now: OffsetDateTime,
    ) -> bool {
        let rule = sig.rule();
        let window = Duration::seconds(rule.window_secs);
        if now - ts > window {
            return false;
        }
        let key = sig_key(sig, subject);
        let q = self.hits.entry(key.clone()).or_default();
        q.push_back(ts);
        while q.front().is_some_and(|t| now - *t > window) {
            q.pop_front();
        }
        if q.len() < rule.threshold {
            return false;
        }
        if self
            .fired
            .get(&key)
            .is_some_and(|t| now - *t < Duration::seconds(DEBOUNCE_SECS))
        {
            return false;
        }
        self.fired.insert(key.clone(), now);
        self.hits.remove(&key);
        true
    }

    /// 丢掉陈旧的计数与已过期的去抖记录（每轮调一次）
    pub fn prune(&mut self, now: OffsetDateTime) {
        let keep = Duration::seconds(PRUNE_AFTER_SECS);
        self.hits
            .retain(|_, q| q.back().is_some_and(|t| now - *t <= keep));
        self.fired
            .retain(|_, t| now - *t < Duration::seconds(DEBOUNCE_SECS));
    }
}

/// 冷却表里这一条是否还「新鲜」。时钟回跳（记录时刻晚于现在）按不在冷却处理，不能把动作锁死
fn fresh(at: &str, now: OffsetDateTime) -> bool {
    parse_rfc3339(at).is_some_and(|t| t <= now && now - t < Duration::seconds(ACTION_COOLDOWN_SECS))
}

/// 同动作同对象 [`ACTION_COOLDOWN_SECS`] 内是否执行过
pub fn in_cooldown(acted: &BTreeMap<String, String>, key: &str, now: OffsetDateTime) -> bool {
    acted.get(key).is_some_and(|at| fresh(at, now))
}

/// 冷却表只留还在冷却期里的条目（它随 `runtime.json` 落盘，不能只进不出）
pub fn prune_acted(acted: &mut BTreeMap<String, String>, now: OffsetDateTime) {
    acted.retain(|_, at| fresh(at, now));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::fmt_rfc3339;
    use pretty_assertions::assert_eq;
    use time::macros::datetime;

    fn at(s: i64) -> OffsetDateTime {
        datetime!(2026-09-11 00:00:00 UTC) + Duration::seconds(s)
    }

    /// 喂一条、当下时刻就是日志时刻（实时流的常态）
    fn feed(e: &mut Engine, sig: Sig, subject: &str, s: i64) -> bool {
        e.observe(sig, subject, at(s), at(s))
    }

    #[test]
    fn three_hits_inside_the_window_fire_exactly_once() {
        let mut e = Engine::default();
        assert!(!feed(&mut e, Sig::RelayUpstreamError, "u2", 0));
        assert!(!feed(&mut e, Sig::RelayUpstreamError, "u2", 1));
        assert!(
            feed(&mut e, Sig::RelayUpstreamError, "u2", 2),
            "第 3 条触发"
        );
        assert!(
            !feed(&mut e, Sig::RelayUpstreamError, "u2", 3),
            "触发后计数清零"
        );
    }

    #[test]
    fn hits_spread_wider_than_the_window_never_add_up() {
        let mut e = Engine::default();
        for s in [0, 40, 80, 120, 160] {
            assert!(!feed(&mut e, Sig::RelayUpstreamError, "u2", s), "t={s}");
        }
    }

    #[test]
    fn a_fired_signature_is_debounced_for_sixty_seconds() {
        let mut e = Engine::default();
        for s in 0..3 {
            feed(&mut e, Sig::RelayUpstreamError, "u2", s);
        }
        for s in 10..13 {
            assert!(
                !feed(&mut e, Sig::RelayUpstreamError, "u2", s),
                "去抖窗口内 t={s}"
            );
        }
        assert!(
            feed(&mut e, Sig::RelayUpstreamError, "u2", 63),
            "距上次触发 ≥ 60 秒、窗口内仍有 ≥3 条 ⇒ 再触发"
        );
    }

    /// 重启后续读到的积压（设计裁决 D12）：描述的是过去，不许在此刻触发动作
    #[test]
    fn stale_backlog_older_than_the_window_is_ignored() {
        let mut e = Engine::default();
        for s in [-300, -299, -298] {
            assert!(!e.observe(Sig::RelayUpstreamError, "u2", at(s), at(0)));
        }
        assert!(
            !e.observe(Sig::KernelBindInUse, "xray", at(-61), at(0)),
            "门槛 1 的签名也一样"
        );
        assert!(e.observe(Sig::KernelBindInUse, "xray", at(-5), at(0)));
    }

    #[test]
    fn subjects_and_signatures_count_separately() {
        let mut e = Engine::default();
        feed(&mut e, Sig::RelayUpstreamError, "u2", 0);
        feed(&mut e, Sig::RelayUpstreamError, "u2", 1);
        assert!(
            !feed(&mut e, Sig::RelayUpstreamError, "u3", 2),
            "别的上游不帮 u2 凑数"
        );
        assert!(
            !feed(&mut e, Sig::Hy2AuthHttpFailed, "u2", 2),
            "别的签名也不凑数"
        );
        assert!(
            feed(&mut e, Sig::CaddyCertFailed, "panel.example.com", 3),
            "门槛 1：一条即触发"
        );
    }

    #[test]
    fn xray_grpc_needs_two_within_one_hundred_fifty_seconds() {
        let mut e = Engine::default();
        assert!(!feed(&mut e, Sig::XrayGrpcUnavailable, "xray", 0));
        assert!(
            feed(&mut e, Sig::XrayGrpcUnavailable, "xray", 60),
            "安全网连续两轮失败"
        );
        let mut e = Engine::default();
        feed(&mut e, Sig::XrayGrpcUnavailable, "xray", 0);
        assert!(!feed(&mut e, Sig::XrayGrpcUnavailable, "xray", 200));
    }

    #[test]
    fn prune_forgets_old_counts_and_expired_debounces() {
        let mut e = Engine::default();
        feed(&mut e, Sig::RelayUpstreamError, "u2", 0);
        feed(&mut e, Sig::CaddyCertFailed, "panel.example.com", 0);
        e.prune(at(1000));
        assert!(
            e.hits.is_empty() && e.fired.is_empty(),
            "常驻进程里不许越攒越多"
        );
    }

    #[test]
    fn cooldown_lasts_ten_minutes_and_survives_a_clock_step_back() {
        let key = action_key(Sig::RelayUpstreamError, "u2");
        assert_eq!(key, "probe_and_borrow|u2");
        let mut acted = BTreeMap::new();
        acted.insert(key.clone(), fmt_rfc3339(at(0)));
        assert!(in_cooldown(&acted, &key, at(599)));
        assert!(!in_cooldown(&acted, &key, at(600)));
        assert!(
            !in_cooldown(&acted, &key, at(-30)),
            "时钟回跳不能把冷却锁死"
        );
        assert!(!in_cooldown(&acted, "alert|xray", at(1)));
        prune_acted(&mut acted, at(700));
        assert!(acted.is_empty());
    }

    #[tokio::test]
    async fn the_sentinel_section_round_trips_through_runtime_json() {
        let d = tempfile::tempdir().unwrap();
        let rt = crate::state::runtime::Runtime::load(d.path().join("runtime.json"));
        let mut s = SentinelRuntime {
            cursor: Some("s=abc;i=9".into()),
            since: Some(fmt_rfc3339(at(0))),
            ..Default::default()
        };
        s.acted.insert("alert|xray".into(), fmt_rfc3339(at(1)));
        s.down_since.insert(Uuid::from_u128(2), fmt_rfc3339(at(2)));
        s.suggested.insert(Uuid::from_u128(2));
        let snap = s.clone();
        rt.update(move |r| put_sentinel(r, &snap)).await;
        let back = crate::state::runtime::Runtime::load(d.path().join("runtime.json"));
        assert_eq!(sentinel_of(&back.read().await), s);
        assert_eq!(
            sentinel_of(&crate::state::runtime::RuntimeData::default()),
            SentinelRuntime::default()
        );
    }
}
