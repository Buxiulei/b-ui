//! 住宅预案（spec §5.7 × §5.6）：带外探测 → 立即判不健康 → 按槽借用 → 告警。
//! 边界（设计裁决 D15）：**不增删池成员、不做切回**（切回归巡检的 `drive_slots`）；
//! 长时间不可达只建议管理员替换。

use super::engine::SentinelRuntime;
use super::incidents::{Level, Outcome};
use super::{LONG_UNREACHABLE_MINS, PROBE_TCP_TIMEOUT_SECS};
use crate::modules::residential::clash::Clash;
use crate::modules::residential::proxy::{self, Prober};
use crate::modules::residential::slots::{self, SlotOutcome};
use crate::modules::residential::{health, state};
use crate::reconcile::DaemonCtx;
use crate::util::{fmt_rfc3339, parse_rfc3339};
use bui_schema::model::{ResidentialGroup, Upstream};
use std::sync::Arc;
use time::OffsetDateTime;
use uuid::Uuid;

/// 事件与告警里的上游对象名：`host:port`（**绝不带凭据**），演练脚本按它认事件
pub fn subject_of(up: &Upstream) -> String {
    format!("{}:{}", up.host, up.port)
}

/// 告警里的「IP X」：体检学到的出口 IP，没有就退回 `host:port`
pub fn ip_of(up: &Upstream) -> String {
    up.verified
        .as_ref()
        .map(|v| v.ip.clone())
        .unwrap_or_else(|| subject_of(up))
}

/// `borrow_now` 的结果 → 「槽 1 已临时切到 Y；槽 2：没有可借用的健康 IP，保持现状；槽 3 已手动锁定，未动」
fn borrow_text(g: &ResidentialGroup, moved: &[SlotOutcome]) -> String {
    if moved.is_empty() {
        return "当前没有槽经它出网".into();
    }
    moved
        .iter()
        .map(|o| {
            if o.switched {
                let to = g
                    .upstreams
                    .iter()
                    .find(|u| u.id == o.target)
                    .map(ip_of)
                    .unwrap_or_else(|| o.target.to_string());
                format!("槽 {} 已临时切到 {to}", o.index)
            } else if o.note.as_deref() == Some(slots::PINNED_UNTOUCHED_NOTE) {
                format!("槽 {} {}", o.index, slots::PINNED_UNTOUCHED_NOTE)
            } else {
                format!("槽 {}：{}", o.index, o.note.clone().unwrap_or_default())
            }
        })
        .collect::<Vec<_>>()
        .join("；")
}

fn gone(id: Uuid) -> Outcome {
    Outcome {
        subject: id.to_string(),
        result: "上游已不在池里（刚被删），忽略".into(),
        level: Level::Info,
    }
}

/// relay 日志里同一上游 60 秒 ≥2 条连接错误（凭据失效 ≥3 条）之后：带外快探
/// （[`health::probe_quick`]：网关 TCP [`PROBE_TCP_TIMEOUT_SECS`] 秒内连不上即判不可达）；
/// 不可用 ⇒ 立即判不健康 + [`slots::borrow_now`] + 上游级告警「IP X 不可达，槽 i 已临时切到 Y」。
pub async fn on_upstream_error(
    ctx: &DaemonCtx,
    prober: Arc<dyn Prober>,
    clash: Arc<dyn Clash>,
    id: Uuid,
    now: OffsetDateTime,
) -> Outcome {
    let g = state::group_of(&*ctx.store.read().await);
    let Some(up) = g.upstreams.iter().find(|u| u.id == id).cloned() else {
        return gone(id);
    };
    let u2 = up.clone();
    let tcp_within = std::time::Duration::from_secs(PROBE_TCP_TIMEOUT_SECS);
    let probe = match tokio::task::spawn_blocking(move || {
        health::probe_quick(prober.as_ref(), &u2, tcp_within)
    })
    .await
    {
        Ok(p) => p,
        Err(e) => {
            return Outcome {
                subject: subject_of(&up),
                result: format!("带外探测任务异常（{e}），本次不动作"),
                level: Level::Warn,
            }
        }
    };
    if probe.ok {
        return Outcome {
            subject: subject_of(&up),
            result: "带外探测通过（日志里的错误来自目标侧或已自愈），不动作".into(),
            level: Level::Info,
        };
    }
    let why = if probe.auth_failed {
        "凭据失效（407 / SOCKS5 认证被拒）"
    } else {
        "不可达"
    };
    state::update(&ctx.runtime, move |r| {
        state::mark_unhealthy(r.health.entry(id.to_string()).or_default(), now);
    })
    .await;
    let moved = slots::borrow_now(ctx, clash, id, now).await;
    let msg = format!("IP {} {why}，{}", ip_of(&up), borrow_text(&g, &moved));
    let alert = msg.clone();
    state::update(&ctx.runtime, move |r| {
        state::set_upstream_alert(r, id, alert)
    })
    .await;
    Outcome {
        subject: subject_of(&up),
        result: msg,
        level: Level::Error,
    }
}

/// relay 日志里某上游对 Google 搜索 403（serp）之后：带外 `google_search` 复核（判据是
/// `proxy::google_ok_of` 那唯一一份，含 sorry 页）。复核通 ⇒ 不动作；封 / 没结论 ⇒
/// `google_ok = false` + [`slots::borrow_now`] + 全局告警（设计裁决 D8）。
pub async fn on_google_blocked(
    ctx: &DaemonCtx,
    prober: Arc<dyn Prober>,
    clash: Arc<dyn Clash>,
    id: Uuid,
    now: OffsetDateTime,
) -> Outcome {
    let g = state::group_of(&*ctx.store.read().await);
    let Some(up) = g.upstreams.iter().find(|u| u.id == id).cloned() else {
        return gone(id);
    };
    let u2 = up.clone();
    let verdict =
        tokio::task::spawn_blocking(move || proxy::google_ok_of(&prober.google_search(&u2)))
            .await
            .ok()
            .flatten();
    if verdict == Some(true) {
        return Outcome {
            subject: subject_of(&up),
            result: "带外复核 Google 可用（日志可能来自已解封之前），不动作".into(),
            level: Level::Info,
        };
    }
    state::update(&ctx.runtime, move |r| {
        state::record_google(
            r.health.entry(id.to_string()).or_default(),
            Some(false),
            now,
        );
    })
    .await;
    let moved = slots::borrow_now(ctx, clash, id, now).await;
    let msg = format!(
        "IP {} 的 Google 被封（serp 403 / sorry 页），{}",
        ip_of(&up),
        borrow_text(&g, &moved)
    );
    let alert = msg.clone();
    state::update(&ctx.runtime, move |r| state::push_alert(r, alert)).await;
    Outcome {
        subject: subject_of(&up),
        result: msg,
        level: Level::Warn,
    }
}

/// 每轮巡查：住宅上游被判不健康连续 [`LONG_UNREACHABLE_MINS`] 分钟 ⇒ 一次性建议替换
/// （上游级告警 + 事件）。`sr.down_since` / `sr.suggested` 随哨兵段落盘；恢复即清。
/// **只建议、不动池**：替换由管理员做，§5.6 的重分配随之自动完成。
pub async fn sweep_long_unreachable(
    ctx: &DaemonCtx,
    sr: &mut SentinelRuntime,
    now: OffsetDateTime,
) -> Vec<Outcome> {
    let g = state::group_of(&*ctx.store.read().await);
    let rt = state::read(&ctx.runtime).await;
    let in_pool = |id: &Uuid| g.upstreams.iter().any(|u| u.id == *id);
    sr.down_since.retain(|id, _| in_pool(id));
    sr.suggested.retain(|id| in_pool(id));
    let mut out = Vec::new();
    let mut alerts: Vec<(Uuid, String)> = Vec::new();
    for up in &g.upstreams {
        let active = rt
            .health
            .get(&up.id.to_string())
            .map(|h| h.active)
            .unwrap_or(true);
        if active {
            sr.down_since.remove(&up.id);
            sr.suggested.remove(&up.id);
            continue;
        }
        let since = sr
            .down_since
            .entry(up.id)
            .or_insert_with(|| fmt_rfc3339(now))
            .clone();
        let Some(t) = parse_rfc3339(&since) else {
            continue;
        };
        let mins = (now - t).whole_minutes();
        if mins < LONG_UNREACHABLE_MINS || sr.suggested.contains(&up.id) {
            continue;
        }
        let msg = format!(
            "IP {} 已连续不可达 {mins} 分钟，建议替换：面板删掉这条上游后添加新 IP，或 \
             `bui residential remove {}` 再 `bui residential add -`（替换后该槽用户自动重分配）",
            ip_of(up),
            subject_of(up)
        );
        sr.suggested.insert(up.id);
        alerts.push((up.id, msg.clone()));
        out.push(Outcome {
            subject: subject_of(up),
            result: msg,
            level: Level::Error,
        });
    }
    if !alerts.is_empty() {
        state::update(&ctx.runtime, move |r| {
            for (id, m) in alerts {
                state::set_upstream_alert(r, id, m);
            }
        })
        .await;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::residential::clash::{Clash, FakeClash};
    use crate::modules::residential::proxy::{FakeProber, HttpProbe};
    use crate::modules::sentinel::testkit::pool_ctx;
    use pretty_assertions::assert_eq;
    use time::macros::datetime;

    fn t0() -> OffsetDateTime {
        datetime!(2026-09-11 00:00:00 UTC)
    }

    fn u(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn fakes() -> (Arc<FakeProber>, Arc<FakeClash>) {
        (
            Arc::new(FakeProber::new()),
            Arc::new(FakeClash::new(Some("resi-1"))),
        )
    }

    #[tokio::test]
    async fn an_unreachable_upstream_is_marked_down_and_its_slot_borrows_at_once() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, _host) = pool_ctx(d.path()).await;
        let (p, c) = fakes(); // FakeProber 缺省：网关连不上
        let o = on_upstream_error(&ctx, p.clone(), c.clone(), u(2), t0()).await;
        assert_eq!(
            o.subject, "isp2.example.net:10007",
            "对象只写 host:port，不带凭据"
        );
        assert_eq!(
            o.result,
            "IP 198.51.100.8 不可达，槽 1 已临时切到 198.51.100.7"
        );
        assert_eq!(o.level, Level::Error);
        assert_eq!(p.calls(), vec!["tcp"], "快探：网关连不上就不再发 HTTP");
        assert_eq!(c.selected("slot-1-pool").as_deref(), Some("resi-1"));
        let r = state::read(&ctx.runtime).await;
        assert!(!r.health[&u(2).to_string()].active, "带外确认 ⇒ 立即不健康");
        assert_eq!(r.slots["1"].current_upstream_id, Some(u(1)));
        assert_eq!(
            r.upstream_alerts.get(&u(2)),
            Some(&o.result),
            "上游级告警：面板住宅卡看得到，巡检探通即清"
        );
    }

    /// 唯一经这条 IP 出网的槽被管理员 pin 住：不动它，但告警要如实说，不能说「当前没有槽经它出网」
    #[tokio::test]
    async fn a_pinned_slot_on_the_failed_ip_is_left_alone_and_said_so() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, _host) = pool_ctx(d.path()).await;
        state::update(&ctx.runtime, |r| {
            let e = r.slots.entry("1".into()).or_default();
            e.pinned_upstream_id = Some(u(2));
            e.current_upstream_id = Some(u(2));
        })
        .await;
        let (p, c) = fakes(); // 网关连不上
        let o = on_upstream_error(&ctx, p, c.clone(), u(2), t0()).await;
        assert_eq!(o.result, "IP 198.51.100.8 不可达，槽 1 已手动锁定，未动");
        assert!(c.calls().is_empty(), "管理员的 pin 压过哨兵");
        assert_eq!(
            state::read(&ctx.runtime).await.slots["1"].current_upstream_id,
            Some(u(2))
        );
    }

    #[tokio::test]
    async fn a_passing_out_of_band_probe_changes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, _host) = pool_ctx(d.path()).await;
        let (p, c) = fakes();
        p.with(|i| {
            i.tcp_ms = Some(20);
            i.gets.insert(
                crate::modules::residential::LATENCY_PROBE_URL.into(),
                Ok(HttpProbe {
                    status: 204,
                    body: String::new(),
                }),
            );
        });
        let o = on_upstream_error(&ctx, p.clone(), c.clone(), u(2), t0()).await;
        assert_eq!(o.level, Level::Info);
        assert!(o.result.contains("带外探测通过"), "{}", o.result);
        assert!(c.calls().is_empty(), "不许借用");
        let r = state::read(&ctx.runtime).await;
        assert!(r.health.get(&u(2).to_string()).is_none_or(|h| h.active));
        assert!(r.upstream_alerts.is_empty());
    }

    #[tokio::test]
    async fn an_auth_failure_is_reported_as_credentials_not_reachability() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, _host) = pool_ctx(d.path()).await;
        let (p, c) = fakes();
        p.with(|i| {
            i.tcp_ms = Some(20);
            i.gets.insert(
                crate::modules::residential::LATENCY_PROBE_URL.into(),
                Err("__auth_failed__".into()),
            );
        });
        let o = on_upstream_error(&ctx, p, c, u(2), t0()).await;
        assert_eq!(
            o.result,
            "IP 198.51.100.8 凭据失效（407 / SOCKS5 认证被拒），槽 1 已临时切到 198.51.100.7"
        );
    }

    #[tokio::test]
    async fn a_sorry_page_on_recheck_marks_google_blocked_and_borrows() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, _host) = pool_ctx(d.path()).await;
        let (p, c) = fakes();
        p.with(|i| {
            i.google = Some(Ok(HttpProbe {
                status: 200,
                body: "<a href=\"https://www.google.com/sorry/index?continue=x\">".into(),
            }))
        });
        let o = on_google_blocked(&ctx, p, c.clone(), u(2), t0()).await;
        assert_eq!(
            o.result,
            "IP 198.51.100.8 的 Google 被封（serp 403 / sorry 页），槽 1 已临时切到 198.51.100.7"
        );
        assert_eq!(o.level, Level::Warn);
        assert_eq!(c.selected("slot-1-pool").as_deref(), Some("resi-1"));
        let r = state::read(&ctx.runtime).await;
        assert_eq!(r.health[&u(2).to_string()].google_ok, Some(false));
        assert!(
            r.health[&u(2).to_string()].active,
            "Google 被封不等于不可达"
        );
        assert_eq!(
            r.alerts.first(),
            Some(&o.result),
            "全局告警（上游级的会被下一轮探通清掉）"
        );
    }

    #[tokio::test]
    async fn an_inconclusive_recheck_trusts_the_explicit_serp_403() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, _host) = pool_ctx(d.path()).await;
        let (p, c) = fakes(); // google 缺省：连不上 ⇒ google_ok_of = None
        let o = on_google_blocked(&ctx, p, c.clone(), u(2), t0()).await;
        assert_eq!(o.level, Level::Warn);
        assert!(
            !c.calls().is_empty(),
            "日志里的 403 serp 是明确策略，复核没结论也要借"
        );
    }

    #[tokio::test]
    async fn google_fine_on_recheck_means_no_action() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, _host) = pool_ctx(d.path()).await;
        let (p, c) = fakes();
        p.with(|i| {
            i.google = Some(Ok(HttpProbe {
                status: 200,
                body: "<html>results</html>".into(),
            }))
        });
        let o = on_google_blocked(&ctx, p, c.clone(), u(2), t0()).await;
        assert_eq!(o.level, Level::Info);
        assert!(c.calls().is_empty());
        let r = state::read(&ctx.runtime).await;
        assert_eq!(
            r.health.get(&u(2).to_string()).and_then(|h| h.google_ok),
            None
        );
    }

    #[tokio::test]
    async fn an_upstream_down_for_thirty_minutes_gets_exactly_one_replace_suggestion() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, _host) = pool_ctx(d.path()).await;
        state::update(&ctx.runtime, |r| {
            r.health.entry(u(2).to_string()).or_default().active = false;
        })
        .await;
        let min = |m: i64| t0() + time::Duration::minutes(m);
        let mut sr = SentinelRuntime::default();
        assert!(sweep_long_unreachable(&ctx, &mut sr, min(0))
            .await
            .is_empty());
        assert_eq!(sr.down_since.get(&u(2)), Some(&fmt_rfc3339(min(0))));
        assert!(sweep_long_unreachable(&ctx, &mut sr, min(29))
            .await
            .is_empty());
        let out = sweep_long_unreachable(&ctx, &mut sr, min(30)).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].subject, "isp2.example.net:10007");
        assert!(
            out[0]
                .result
                .starts_with("IP 198.51.100.8 已连续不可达 30 分钟，建议替换"),
            "{}",
            out[0].result
        );
        assert!(out[0]
            .result
            .contains("bui residential remove isp2.example.net:10007"));
        assert_eq!(
            state::read(&ctx.runtime).await.upstream_alerts.get(&u(2)),
            Some(&out[0].result)
        );
        assert!(
            sweep_long_unreachable(&ctx, &mut sr, min(31))
                .await
                .is_empty(),
            "同一段不可达只建议一次"
        );
        // 恢复 ⇒ 两张表都清掉，下一段不可达重新计时
        state::update(&ctx.runtime, |r| {
            r.health.get_mut(&u(2).to_string()).unwrap().active = true;
        })
        .await;
        assert!(sweep_long_unreachable(&ctx, &mut sr, min(32))
            .await
            .is_empty());
        assert!(sr.down_since.is_empty() && sr.suggested.is_empty());
    }
}
