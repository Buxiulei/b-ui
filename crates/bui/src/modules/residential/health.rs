//! 住宅出口的健康巡检、切换决策与 relay 重启后的选择重放（spec §5.3）。
//!
//! 契约决策 §C：**当前实际生效的出口**的真源是 `runtime.selected_upstream_id`（uuid），
//! 自动切换与手动 `select` 都只经 Clash API + runtime，**不写 state**，因此不重启 relay；
//! `state.selected_upstream_id` 只是「配置里的落点」（selector 的 `default`、
//! `ports_allowed` 取反依据、`auto` 过滤依据），由增删上游 / 每日 04:00 窗口改写。
//! `resi-N` 是**位置键**（增删上游会 renumber），只在调 Clash API 的那一瞬由
//! `clash::tag_of` 现算，绝不当运行时主键。

use super::{
    clash, proxy, state, HEALTH_INTERVAL_SECS, HEALTH_PROBE_HOST, HEALTH_PROBE_URL, HEALTH_TRIES,
    SWITCH_MIN_INTERVAL_SECS,
};
use crate::api::Event;
use crate::reconcile::DaemonCtx;
use crate::util::{fmt_rfc3339, parse_rfc3339};
use bui_schema::model::{ResidentialGroup, Upstream};
use clash::Clash;
use proxy::{ProbeError, Prober};
use state::ResiRuntime;
use std::sync::Arc;
use time::OffsetDateTime;
use uuid::Uuid;

/// 一轮巡检的结论（返回给测试与 `/api/residential/health`）
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RoundOutcome {
    /// 本轮判定健康的成员 tag，顺序与池一致
    pub healthy: Vec<String>,
    /// 本轮探测结果，`(tag, 本轮是否达标)`
    pub probed: Vec<(String, bool)>,
    /// 因健康决策**切换**到的 tag（`None` = 没切）
    pub switched_to: Option<String>,
    /// 因 relay 重启后 selector 回落、把 `runtime.selected_upstream_id` **重放**回去的
    /// tag（`None` = 没重放）。与 `switched_to` 分开记：重放不是一次新的选择，它不算
    /// 切换、不吃 60 秒限速、不动 `last_switch_at`，测试也必须能把两者区分开
    pub replayed_to: Option<String>,
    pub notes: Vec<String>,
}

/// 一个成员一轮探测的结果。`auth_failed` 单独带出来，是因为凭据失效要**单独告警**
/// （管理员必须换凭据，不是等它自愈），而 `check_once` 是 async、不能为了补判一次
/// 就再同步调一次 `Prober`（`reqwest::blocking` 在 async 上下文会 panic）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemberProbe {
    pub ok: bool,
    pub auth_failed: bool,
    /// 经该上游还能不能正常用 Google 搜索（`None` = 本轮没探到结论）。
    /// 主理人硬要求「住宅上游不封 Google」，所以它参与选路（R2 ②）
    pub google_ok: Option<bool>,
}

/// 单成员一轮探测：可达性（[`HEALTH_PROBE_URL`]）+ Google 可达性各一轮。
pub fn probe_member(p: &dyn Prober, up: &Upstream) -> MemberProbe {
    let mut probe = probe_reachable(p, up);
    // R2 ②：每轮额外经该上游打一次真实 Google 搜索。凭据失效时不打——每条连接都会
    // 被拒，探不出任何关于 Google 的结论，只会白等 GOOGLE_PROBE_TIMEOUT_SECS。
    if !probe.auth_failed {
        probe.google_ok = proxy::google_ok_of(&p.google_search(up));
    }
    probe
}

/// 可达性那一半：对 [`HEALTH_PROBE_URL`] 最多 [`HEALTH_TRIES`] 次，任一成功即本轮健康。
/// **407 / SOCKS5 认证被拒一律算不健康**（调研 §D：凭据失效的上游会把每条连接都拒掉，
/// 它「可达」但不可用），并且立刻停止重试。
fn probe_reachable(p: &dyn Prober, up: &Upstream) -> MemberProbe {
    for _ in 0..HEALTH_TRIES {
        match p.get(up, HEALTH_PROBE_URL) {
            // generate_204 正常回 204；任何 2xx/3xx 都说明隧道通了
            Ok(hp) if hp.status < 400 => {
                return MemberProbe {
                    ok: true,
                    auth_failed: false,
                    google_ok: None,
                }
            }
            // 调研 §D：407 的上游「可达」但每条连接都被拒 —— 一律不健康，且不必再试
            Err(ProbeError::AuthFailed) => {
                return MemberProbe {
                    ok: false,
                    auth_failed: true,
                    google_ok: None,
                }
            }
            _ => {}
        }
    }
    // 全失败：`get` 在 https 目标上分不出「凭据失效」与「连不上」（407 发生在 CONNECT
    // 隧道建立阶段，reqwest 只给一个 Err，见 T3 `confirm_auth_failure` 的注释），
    // 所以这里经上游自写一次 CONNECT 把两者分开。不补判的后果是生产上凭据失效
    // 只会报「上游挂了」，spec §5.2 要求的「凭据失效」告警永远不出现。
    let auth_failed = proxy::confirm_auth_failure(p, up, HEALTH_PROBE_HOST);
    MemberProbe {
        ok: false,
        auth_failed,
        google_ok: None,
    }
}

/// 当前池的成员 id，顺序与 `g.upstreams` 一致（成员集比较用它，不用位置键 tag）
pub fn ids_of(g: &ResidentialGroup) -> Vec<Uuid> {
    g.upstreams.iter().map(|u| u.id).collect()
}

/// 切换目标。排序键依次是：**「Google 通」优先**（R2 ②，封 Google 的上游对主理人等于
/// 不可用）→ `priority` 最小 → 近 24h 成功率降序 → 池内下标升序（稳定）。
/// 管理员手动锁定的上游（`r.manual_selected_id`）只要还在池里且健康就**直接返回**，
/// 不吃任何排序键（R2 ①）。进出都是 **uuid**（契约决策 §C：运行时主键不用位置键 tag）
pub fn pick_target(
    g: &ResidentialGroup,
    r: &ResiRuntime,
    healthy: &[Uuid],
    now: OffsetDateTime,
) -> Option<Uuid> {
    // 手动锁定压过一切：管理员按了「切到这条」，巡检不许因为别人 priority 更好就抢走。
    // 不在池里（删上游的残留）或已不健康时才落回自动排序，由调用方顺带清空锁定。
    if let Some(m) = r.manual_selected_id {
        if healthy.contains(&m) && g.upstreams.iter().any(|u| u.id == m) {
            return Some(m);
        }
    }
    let mut cands: Vec<(u8, u32, i64, usize, Uuid)> = healthy
        .iter()
        .filter_map(|id| {
            // 不在当前池里的 uuid 直接忽略（删上游与巡检并发时会出现）
            let idx = g.upstreams.iter().position(|u| u.id == *id)?;
            let h = r.health.get(&id.to_string());
            let rate = h.map(|h| state::success_rate_24h(h, now)).unwrap_or(0.0);
            // 「未知」与「封了」一起排在「通」之后：没探到结论不该凭空赢过探过的成员
            let google_rank = u8::from(h.and_then(|h| h.google_ok) != Some(true));
            // 成功率降序 = 放大后取负（f64 不能直接排序，也不想引 total_cmp 的歧义）；
            // 第四项 idx 让前面全同时按池内下标稳定排序
            Some((
                google_rank,
                g.upstreams[idx].priority,
                -((rate * 1_000_000.0) as i64),
                idx,
                *id,
            ))
        })
        .collect();
    cands.sort();
    cands.into_iter().next().map(|(_, _, _, _, id)| id)
}

/// 一轮完整巡检（spec §5.3 的全部规则）：自己取「本轮开始时的成员集快照」再转调 [`check_round`]
pub async fn check_once(
    ctx: &DaemonCtx,
    p: Arc<dyn Prober>,
    c: Arc<dyn Clash>,
) -> anyhow::Result<RoundOutcome> {
    // 规则 4 的「本轮开始时的成员集」快照在这里取；check_round 只负责比较
    let before = ids_of(&state::group_of(&*ctx.store.read().await));
    check_round(ctx, p, c, before).await
}

/// 同上，但成员集快照由调用方给。**测试用它注入一份「本轮开始时成员集不同」的快照**来
/// 触发规则 4（探测中途改池的真实时序无法在单元测试里可靠复现）。生产只走 [`check_once`]。
pub async fn check_round(
    ctx: &DaemonCtx,
    p: Arc<dyn Prober>,
    c: Arc<dyn Clash>,
    before: Vec<Uuid>,
) -> anyhow::Result<RoundOutcome> {
    let mut out = RoundOutcome::default();
    let g = state::group_of(&*ctx.store.read().await);
    if !g.pool_active() {
        out.notes
            .push("住宅池未启用或为空，跳过巡检（relay 已 fail-open 直连）".into());
        return Ok(out);
    }
    let tags = clash::tags(&g); // 只用于展示与 Clash API，绝不当运行时主键（§C）
    let now = ctx.host.now();

    // 规则 2：成员并行探测（并发上限 4）
    let ups = g.upstreams.clone();
    let pp = p.clone();
    let results = super::fanout(ups.clone(), move |up| probe_member(pp.as_ref(), &up)).await;

    // 凭据失效的告警以 **uuid** 为键（契约决策 §C），文案用 `host:port`：按 `url-N` 存的
    // 话，删掉一条上游后位置名会被新条目复用，新上游就顶着上一个账号的 407 告警
    let mut auth_alerts: Vec<(Uuid, String)> = Vec::new();
    let mut probed: Vec<(Uuid, bool)> = Vec::new();
    // 本轮探到的 Google 结论（`None` = 没探到，写回时不动上次的结论）
    let mut googles: Vec<(Uuid, Option<bool>)> = Vec::new();
    for (i, up) in ups.iter().enumerate() {
        let probe = results.get(i).copied().flatten();
        let ok = probe.map(|x| x.ok).unwrap_or(false);
        if probe.is_none() {
            out.notes
                .push(format!("{} 的探测任务异常结束，本轮按不达标处理", up.name));
        }
        // 凭据失效要单独告警：不是网络抖动，管理员必须换凭据（探测结果里直接带出来，
        // 不再为了补判而在 async 上下文里同步调一次 Prober）
        if probe.map(|x| x.auth_failed).unwrap_or(false) {
            auth_alerts.push((
                up.id,
                format!(
                    "上游 {}:{} 凭据失效（407 / SOCKS5 认证被拒），请更新凭据",
                    up.host, up.port
                ),
            ));
        }
        out.probed.push((tags[i].clone(), ok));
        probed.push((up.id, ok));
        googles.push((up.id, probe.and_then(|x| x.google_ok)));
    }

    // 规则 3：迟滞 + 24h 样本 + Google 判定。**一轮只写一次 runtime**：每次 update 都是
    // tmp + fsync + rename，按成员各写一次等于一轮 N 次落盘。凭据失效告警的写入与
    // 「成功即清」也并进这一次写（下面各分支不再重复落它）。
    let samples = probed.clone();
    let verdicts = googles.clone();
    let rt = state::update(&ctx.runtime, move |r| {
        for (id, ok) in samples {
            let h = r.health.entry(id.to_string()).or_default();
            state::record_probe(h, ok, now);
            let _ = state::apply_hysteresis(h, ok);
            // 探通一次就把这条上游的 407 告警清掉：换完凭据不该还要人手动消警
            if ok {
                state::clear_upstream_alert(r, id);
            }
        }
        for (id, msg) in auth_alerts {
            state::set_upstream_alert(r, id, msg);
        }
        for (id, google) in verdicts {
            let h = r.health.entry(id.to_string()).or_default();
            state::record_google(h, google, now);
        }
    })
    .await;
    let healthy: Vec<Uuid> = probed
        .iter()
        .map(|(id, _)| *id)
        // 没探过的成员默认健康（HealthState::default().active = true）
        .filter(|id| {
            rt.health
                .get(&id.to_string())
                .map(|h| h.active)
                .unwrap_or(true)
        })
        .collect();
    out.healthy = healthy
        .iter()
        .filter_map(|id| clash::tag_of(&g, *id))
        .collect();

    // 规则 4：成员集在本轮内变化 → 只写 runtime，不切
    let after = ids_of(&state::group_of(&*ctx.store.read().await));
    if after != before {
        out.notes.push(format!(
            "住宅池成员集在本轮探测期间变化（{} → {} 个成员），本轮不切换",
            before.len(),
            after.len()
        ));
        return Ok(out);
    }

    // 规则 5：读不到当前选择（relay 没起来或旧配置）→ 不切
    let cc = c.clone();
    let sel_tag = tokio::task::spawn_blocking(move || cc.selected()).await?;
    let Some(sel_tag) = sel_tag else {
        out.notes
            .push("relay 的 Clash API 读不到当前选择（未运行或旧配置），本轮不切换".into());
        return Ok(out);
    };
    // Clash 的 now 只说明「relay 此刻在用哪条」，**不是**「该用哪条」的真源（§C）
    let sel_id = clash::id_of_tag(&g, &sel_tag);

    // 规则 6a：runtime 记的选择还在池里且健康 → 它就是该生效的出口。
    // 与 Clash 的 now 不一致 = relay 刚被看门狗（watchdog.rs 的重启分支）或
    // `/api/services/b-ui-relay/restart` 重启过、selector 回落到配置里的 default，
    // 这两条来路都不发 Event::RelayRestarted（§C 末段），所以在这里重放。
    // **方向只能是 runtime → Clash**：反过来把 now 写进 runtime 会让一次重启静默
    // 撤销管理员的手动切换与上一轮的自动避障。
    let ids = ids_of(&g);
    let want = rt.selected_upstream_id.filter(|id| ids.contains(id));
    if let Some(want) = want {
        if healthy.contains(&want) {
            let mut alerts: Vec<String> = Vec::new();
            if Some(want) != sel_id {
                // tag 现算（位置键，不做主键）；want 来自当前池，tag_of 必有值
                let tag = clash::tag_of(&g, want).expect("want 取自当前池");
                let (cc, t2) = (c.clone(), tag.clone());
                match tokio::task::spawn_blocking(move || cc.select(&t2)).await? {
                    Ok(()) => {
                        tracing::info!(from = %sel_tag, to = %tag, "relay 重启后重放住宅出口选择");
                        out.notes.push(format!(
                            "relay 的当前选择 {sel_tag} 与运行时记录的 {tag} 不一致（relay 刚重启过），已重放运行时的选择"
                        ));
                        // 重放不是切换：不写 last_switch_at、不吃 60s 限速
                        out.replayed_to = Some(tag);
                    }
                    Err(e) => {
                        alerts.push(format!("重放住宅出口选择到 {tag} 失败：{e}"));
                        out.notes.push(format!("重放到 {tag} 失败：{e}"));
                    }
                }
            }
            let pending = Some(want) != g.selected_upstream_id;
            state::update(&ctx.runtime, move |r| {
                r.selected_upstream_id = Some(want);
                r.selected_pending_persist = pending;
                for a in alerts.drain(..) {
                    state::push_alert(r, a);
                }
            })
            .await;
            return Ok(out);
        }
    } else if let Some(sel_id) = sel_id {
        // 规则 6b：runtime 没有选择（守护进程首次启动、从没切过）或它已被删出池 ——
        // 此时才拿 Clash 的 now 初始化 runtime，并粘在它上面
        if healthy.contains(&sel_id) {
            let pending = Some(sel_id) != g.selected_upstream_id;
            state::update(&ctx.runtime, move |r| {
                r.selected_upstream_id = Some(sel_id);
                r.selected_pending_persist = pending;
            })
            .await;
            return Ok(out);
        }
    }

    // 规则 7：全不健康 → 保持并告警
    if healthy.is_empty() {
        out.notes.push(format!(
            "全部上游探测不达标，保持当前出口 {sel_tag}（降级总比乱切好）"
        ));
        persist_alerts(
            ctx,
            &["全部住宅上游探测不达标，出口已降级但未切换".to_string()],
        )
        .await;
        return Ok(out);
    }

    // 规则 8：选目标（uuid 进、uuid 出；tag 只在调 Clash API 时现算）
    let Some(target_id) = pick_target(&g, &rt, &healthy, now) else {
        return Ok(out);
    };
    let Some(target_tag) = clash::tag_of(&g, target_id) else {
        // healthy 全部来自当前池，走不到这里；真走到了说明池刚变过，按规则 4 处理
        out.notes
            .push("切换目标已不在池里（成员集刚变过），本轮不切换".into());
        return Ok(out);
    };

    // 规则 9：≥ 60 秒
    let since = rt
        .last_switch_at
        .as_deref()
        .and_then(parse_rfc3339)
        .map(|t| (now - t).whole_seconds())
        // 时钟回跳（NTP 校时）不该把限速锁死
        .map(|s| if s < 0 { i64::MAX } else { s })
        .unwrap_or(i64::MAX);
    if since < SWITCH_MIN_INTERVAL_SECS {
        out.notes.push(format!(
            "当前 {sel_tag} 不健康，需切到 {target_tag}，但切换限速中（剩余 {}s）",
            SWITCH_MIN_INTERVAL_SECS - since
        ));
        return Ok(out);
    }

    // 规则 10/11：切换
    // 切到的不是手动锁定的那条 ⇒ 锁定目标已不健康（否则 pick_target 会直接返回它），
    // 解除锁定，别锁着一条坏上游不放（R2 ①）。切到的**就是**锁定目标时（规则 6a 的
    // runtime 选择另有其人、已不健康）这一轮正是在把锁定目标放回去，锁必须留着。
    let manual_dropped = rt.manual_selected_id.filter(|m| *m != target_id);
    let (cc, t2) = (c.clone(), target_tag.clone());
    match tokio::task::spawn_blocking(move || cc.select(&t2)).await? {
        Ok(()) => {
            let pending = Some(target_id) != g.selected_upstream_id;
            state::update(&ctx.runtime, move |r| {
                r.selected_upstream_id = Some(target_id);
                r.last_switch_at = Some(fmt_rfc3339(now));
                r.selected_pending_persist = pending;
                if manual_dropped.is_some() {
                    r.manual_selected_id = None;
                }
            })
            .await;
            tracing::info!(from = %sel_tag, to = %target_tag, "住宅出口切换");
            if let Some(m) = manual_dropped {
                tracing::info!(manual = %m, to = %target_tag, "手动锁定的出口已不健康，自动切换并解除锁定");
                out.notes.push(format!(
                    "手动锁定的出口已不健康，已自动切到 {target_tag} 并解除手动锁定"
                ));
            }
            out.switched_to = Some(target_tag);
        }
        Err(e) => {
            // 切换失败不改 runtime：下一轮重新评估（别把没生效的选择记成生效）
            persist_alerts(ctx, &[format!("切换住宅出口到 {target_tag} 失败：{e}")]).await;
            out.notes.push(format!("切换到 {target_tag} 失败：{e}"));
        }
    }
    Ok(out)
}

async fn persist_alerts(ctx: &DaemonCtx, alerts: &[String]) {
    if alerts.is_empty() {
        return;
    }
    let alerts = alerts.to_vec();
    state::update(&ctx.runtime, move |r| {
        for a in alerts {
            state::push_alert(r, a);
        }
    })
    .await;
}

/// 每 2 分钟一轮
pub async fn health_loop(ctx: DaemonCtx, p: Arc<dyn Prober>, c: Arc<dyn Clash>) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(HEALTH_INTERVAL_SECS));
    loop {
        tick.tick().await;
        if let Err(e) = check_once(&ctx, p.clone(), c.clone()).await {
            tracing::warn!(error = %e, "住宅巡检一轮失败");
        }
    }
}

/// 重放 `runtime.selected_upstream_id`（spec §5.3 最后一句）。
/// **`rx` 由调用方先 `bus.subscribe()` 拿到再传进来**：broadcast 会丢弃「发送时还没有
/// 订阅者」的事件，若在本函数里 subscribe，调用方 spawn 之后立刻 send 的那条必丢
/// （current_thread 运行时下 100% 丢）。T11 的 `spawn` 与 T8 的测试都按这个顺序写。
pub async fn replay_loop(
    ctx: DaemonCtx,
    c: Arc<dyn Clash>,
    mut rx: tokio::sync::broadcast::Receiver<Event>,
) {
    loop {
        match rx.recv().await {
            Ok(Event::RelayRestarted) => {
                // relay 重启后 selector 回到配置里的 default（池首），把运行时选择重放回去。
                // 没有这一步，每次黑名单批量或 pin 都会把出口悄悄换回池首。
                // tag 现算：runtime 存的是 uuid，池增删后同一个 resi-N 可能已指向别人（§C）
                let Some(id) = state::read(&ctx.runtime).await.selected_upstream_id else {
                    continue;
                };
                let g = state::group_of(&*ctx.store.read().await);
                let Some(tag) = clash::tag_of(&g, id) else {
                    tracing::warn!(%id, "运行时选中的上游已不在池里，跳过重放");
                    continue;
                };
                let (cc, t2) = (c.clone(), tag.clone());
                match tokio::task::spawn_blocking(move || cc.select(&t2)).await {
                    Ok(Ok(())) => tracing::info!(tag = %tag, "relay 重启后已重放住宅出口选择"),
                    other => {
                        tracing::warn!(tag = %tag, result = ?other, "重放住宅出口选择失败")
                    }
                }
            }
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
        }
    }
}

/// 管理员手动切换（`POST /api/residential/select`）：只走 Clash API，不写 state（契约决策 §C），
/// 并**锁定**到这条上游，直到它不健康（R2 ①）——否则下一轮巡检按 priority 就把它换走了。
pub async fn select_manual(ctx: &DaemonCtx, c: Arc<dyn Clash>, id: Uuid) -> anyhow::Result<String> {
    let g = state::group_of(&*ctx.store.read().await);
    let tag = clash::tag_of(&g, id).ok_or_else(|| anyhow::anyhow!("上游不在当前池里"))?;
    let (cc, t2) = (c.clone(), tag.clone());
    tokio::task::spawn_blocking(move || cc.select(&t2)).await??;
    let now = ctx.host.now();
    let pending = Some(id) != g.selected_upstream_id;
    state::update(&ctx.runtime, move |r| {
        r.selected_upstream_id = Some(id);
        r.manual_selected_id = Some(id);
        r.last_switch_at = Some(fmt_rfc3339(now));
        r.selected_pending_persist = pending;
    })
    .await;
    Ok(tag)
}

/// 解除手动锁定（`POST /api/residential/select {"auto":true}` / `bui residential select --auto`）：
/// 只清锁定位，**不动** relay 的当前选择 —— 下一轮巡检自己按 Google / priority 重新挑，
/// 免得一按「自动」就无谓地掐一次连接。
pub async fn select_auto(ctx: &DaemonCtx) {
    state::update(&ctx.runtime, |r| r.manual_selected_id = None).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::EventBus;
    use crate::modules::residential::clash::FakeClash;
    use crate::modules::residential::proxy::HttpProbe;
    use crate::modules::residential::state as rstate;
    use crate::state::runtime::Runtime;
    use crate::state::store::Store;
    use crate::sys::fake::FakeHost;
    use crate::sys::Host;
    use crate::testutil::sample_state;
    use bui_schema::model::UpstreamKind;
    use pretty_assertions::assert_eq;

    fn upstream(i: u128, priority: u32) -> Upstream {
        Upstream {
            id: Uuid::from_u128(i),
            name: format!("url-{i}"),
            kind: UpstreamKind::Http,
            host: format!("isp{i}.example.net"),
            port: 10007,
            username: "user1".into(),
            password: "pw1".into(),
            priority,
            provider: None,
            region: None,
            ports_allowed: None,
            verified: None,
        }
    }

    async fn ctx(d: &tempfile::TempDir, prios: &[u32]) -> (DaemonCtx, Arc<FakeHost>) {
        let mut s = sample_state();
        let g = s.residential.groups.get_mut("default").unwrap();
        g.enabled = true;
        g.upstreams = prios
            .iter()
            .enumerate()
            .map(|(i, p)| upstream(i as u128 + 1, *p))
            .collect();
        g.selected_upstream_id = g.upstreams.first().map(|u| u.id);
        g.blacklist.auto.clear();
        let host = Arc::new(FakeHost::new());
        let c = DaemonCtx {
            store: Store::create(d.path().join("state.json"), s).await.unwrap(),
            runtime: Runtime::load(d.path().join("runtime.json")),
            bus: EventBus::new(),
            host: host.clone(),
            paths: bui_schema::paths::Paths::default_server(),
        };
        (c, host)
    }

    /// `FakeProber` 按 URL 查表，本任务要按**上游**区分好坏，所以用一个按 host 分派的 prober。
    /// `connect` 也按上游分派：`probe_member` 全失败后会用它做 CONNECT 补判，
    /// `auth_failed` 集合里的上游必须在补判里也回 `AuthFailed`（真机上 407 只有这条路能认出来）
    struct ByHost {
        bad: std::collections::BTreeSet<String>,
        auth_failed: std::collections::BTreeSet<String>,
        /// 经这些上游打 Google 搜索回 403（调研 §D 的 Bright Data 形态）
        google_blocked: std::collections::BTreeSet<String>,
    }
    impl Prober for ByHost {
        fn connect(
            &self,
            u: &Upstream,
            _h: &str,
            _p: u16,
        ) -> crate::modules::residential::proxy::ConnectVerdict {
            if self.auth_failed.contains(&u.host) {
                return crate::modules::residential::proxy::ConnectVerdict::AuthFailed;
            }
            crate::modules::residential::proxy::ConnectVerdict::Open
        }
        fn get(&self, up: &Upstream, _url: &str) -> Result<HttpProbe, ProbeError> {
            if self.auth_failed.contains(&up.host) {
                return Err(ProbeError::AuthFailed);
            }
            if self.bad.contains(&up.host) {
                return Err(ProbeError::Unreachable("refused".into()));
            }
            Ok(HttpProbe {
                status: 204,
                body: String::new(),
            })
        }
        fn google_search(&self, up: &Upstream) -> Result<HttpProbe, ProbeError> {
            if self.auth_failed.contains(&up.host) {
                return Err(ProbeError::AuthFailed);
            }
            if self.google_blocked.contains(&up.host) {
                return Ok(HttpProbe {
                    status: 403,
                    body: "403 Forbidden serp domain".into(),
                });
            }
            Ok(HttpProbe {
                status: 200,
                body: "<html>weather results".into(),
            })
        }
        fn udp_associate(&self, _u: &Upstream) -> Result<bool, ProbeError> {
            Ok(true)
        }
        fn direct_tcp(&self, _h: &str, _p: u16) -> bool {
            true
        }
    }
    fn by_host(bad: &[&str], auth_failed: &[&str]) -> Arc<dyn Prober> {
        by_host_with_google(bad, auth_failed, &[])
    }
    fn by_host_with_google(
        bad: &[&str],
        auth_failed: &[&str],
        google_blocked: &[&str],
    ) -> Arc<dyn Prober> {
        Arc::new(ByHost {
            bad: bad.iter().map(|s| s.to_string()).collect(),
            auth_failed: auth_failed.iter().map(|s| s.to_string()).collect(),
            google_blocked: google_blocked.iter().map(|s| s.to_string()).collect(),
        })
    }

    #[tokio::test]
    async fn a_healthy_selection_sticks() {
        let d = tempfile::tempdir().unwrap();
        let (c, _h) = ctx(&d, &[10, 20]).await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        let out = check_once(&c, by_host(&[], &[]), clash.clone())
            .await
            .unwrap();
        assert_eq!(out.healthy, vec!["resi-1", "resi-2"]);
        assert_eq!(out.switched_to, None, "当前健康就粘住");
        assert_eq!(out.replayed_to, None, "两边一致，没什么可重放");
        assert_eq!(clash.calls(), vec!["get"], "不发 PUT");
        assert_eq!(
            rstate::read(&c.runtime).await.selected_upstream_id,
            Some(Uuid::from_u128(1)),
            "规则 6b：runtime 还没有选择（守护进程首次启动），才拿 Clash 的 now 初始化它"
        );
    }

    #[tokio::test]
    async fn a_relay_restart_outside_reconcile_is_healed_by_replaying_the_runtime_choice() {
        // 看门狗与 POST /api/services/b-ui-relay/restart 都不发 Event::RelayRestarted
        // （§C 末段），于是 replay_loop 收不到通知；relay 重启后 selector 回落到配置里的
        // default（池首 resi-1），而运行时选的是 resi-2。规则 6a 必须把 runtime 的选择
        // **重放**回去，而不是采纳 Clash 的 now。
        let d = tempfile::tempdir().unwrap();
        let (c, _h) = ctx(&d, &[10, 20]).await;
        rstate::update(&c.runtime, |r| {
            r.selected_upstream_id = Some(Uuid::from_u128(2))
        })
        .await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        let out = check_once(&c, by_host(&[], &[]), clash.clone())
            .await
            .unwrap();
        assert_eq!(
            out.replayed_to.as_deref(),
            Some("resi-2"),
            "重放，不是采纳 now"
        );
        assert_eq!(out.switched_to, None, "重放不是切换");
        assert!(
            out.notes.iter().any(|n| n.contains("重放")),
            "{:?}",
            out.notes
        );
        assert_eq!(clash.selected().as_deref(), Some("resi-2"));
        let r = rstate::read(&c.runtime).await;
        assert_eq!(
            r.selected_upstream_id,
            Some(Uuid::from_u128(2)),
            "runtime 是真源：绝不能被 Clash 的 now 改回池首（否则一次看门狗重启就静默撤销了手动切换）"
        );
        assert_eq!(
            r.last_switch_at, None,
            "重放不写 last_switch_at，不占 60s 限速额度"
        );
    }

    #[tokio::test]
    async fn a_stale_runtime_selection_falls_back_to_the_clash_now() {
        // runtime 里记着一条**已不在池里**的 uuid（删上游与巡检竞态的残留）：规则 6a 的
        // `filter(|id| ids.contains(id))` 必须把它滤掉，走规则 6b 从 Clash 的 now 初始化，
        // 而不是拿一个 tag_of 为 None 的 uuid 去重放（那会 panic 或发一个空 PUT）。
        let d = tempfile::tempdir().unwrap();
        let (c, _h) = ctx(&d, &[10, 20]).await;
        rstate::update(&c.runtime, |r| {
            r.selected_upstream_id = Some(Uuid::from_u128(99))
        })
        .await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        let out = check_once(&c, by_host(&[], &[]), clash.clone())
            .await
            .unwrap();
        assert_eq!(out.replayed_to, None, "悬空 uuid 不重放");
        assert_eq!(out.switched_to, None);
        assert!(
            !clash.calls().iter().any(|x| x.starts_with("put:")),
            "一个 PUT 都不发"
        );
        assert_eq!(
            rstate::read(&c.runtime).await.selected_upstream_id,
            Some(Uuid::from_u128(1)),
            "规则 6b：runtime 的选择失效 ⇒ 用 Clash 的 now 重新初始化"
        );
    }

    #[tokio::test]
    async fn two_bad_rounds_switch_to_the_lowest_priority_healthy_member() {
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d, &[10, 20, 5]).await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        let p = by_host(&["isp1.example.net"], &[]);
        let first = check_once(&c, p.clone(), clash.clone()).await.unwrap();
        assert_eq!(first.switched_to, None, "第 1 轮失败还在迟滞里，不切");
        assert!(first.healthy.contains(&"resi-1".to_string()));
        host.advance(120);
        let second = check_once(&c, p, clash.clone()).await.unwrap();
        assert_eq!(
            second.switched_to.as_deref(),
            Some("resi-3"),
            "priority 5 < 10 < 20"
        );
        assert_eq!(clash.calls().last().unwrap(), "put:resi-3");
        let r = rstate::read(&c.runtime).await;
        assert_eq!(
            r.selected_upstream_id,
            Some(Uuid::from_u128(3)),
            "runtime 记 uuid，不记位置键 tag"
        );
        assert!(
            r.selected_pending_persist,
            "state 落点还是 resi-1，标记待持久化（契约决策 §C）"
        );
        // state 没被改 → relay 不重启（spec §5.3 的用意）
        assert_eq!(
            rstate::group_of(&*c.store.read().await).selected_upstream_id,
            Some(Uuid::from_u128(1))
        );
    }

    #[tokio::test]
    async fn a_407_upstream_counts_as_unhealthy() {
        // 调研 §D：凭据失效的上游「可达」但每条连接都被拒，必须判不健康。
        // `ByHost::get` 直接回 ProbeError::AuthFailed（模拟 `looks_like_proxy_auth` 命中）；
        // 下一条测试覆盖「文字里没线索、只能靠 CONNECT 补判」的真机形态。
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d, &[10, 20]).await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        let p = by_host(&[], &["isp1.example.net"]);
        check_once(&c, p.clone(), clash.clone()).await.unwrap();
        host.advance(120);
        let out = check_once(&c, p, clash.clone()).await.unwrap();
        assert_eq!(out.healthy, vec!["resi-2"]);
        assert_eq!(out.switched_to.as_deref(), Some("resi-2"));
        let r = rstate::read(&c.runtime).await;
        // 告警以 uuid 为键（url-N 是位置名，删条目后会被新条目复用），文案用 host:port
        let msg = r
            .upstream_alerts
            .get(&Uuid::from_u128(1))
            .cloned()
            .unwrap_or_else(|| panic!("要点名凭据失效：{:?}", r.upstream_alerts));
        assert!(msg.contains("凭据"), "{msg}");
        assert!(
            msg.contains("isp1.example.net:10007"),
            "文案要用 host:port：{msg}"
        );
        assert!(!msg.contains("url-"), "文案不许用位置名：{msg}");
        assert!(
            !r.upstream_alerts.contains_key(&Uuid::from_u128(2)),
            "健康的那条不该有告警"
        );
        assert!(
            !r.alerts.iter().any(|a| a.contains("凭据")),
            "上游级告警不进全局列表：{:?}",
            r.alerts
        );
    }

    #[tokio::test]
    async fn one_good_round_clears_that_upstreams_credential_alert() {
        // 换了凭据之后，巡检成功一次就该把 407 告警消掉，不必等人手动点。
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d, &[10, 20]).await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        let bad = by_host(&[], &["isp1.example.net"]);
        check_once(&c, bad, clash.clone()).await.unwrap();
        assert!(rstate::read(&c.runtime)
            .await
            .upstream_alerts
            .contains_key(&Uuid::from_u128(1)));
        host.advance(120);
        check_once(&c, by_host(&[], &[]), clash.clone())
            .await
            .unwrap();
        let r = rstate::read(&c.runtime).await;
        assert!(
            r.upstream_alerts.is_empty(),
            "成功一轮即清：{:?}",
            r.upstream_alerts
        );
    }

    #[tokio::test]
    async fn a_407_that_only_the_connect_probe_can_see_still_raises_the_credential_alert() {
        // 真机形态：https 目标经 HTTP 上游走 CONNECT 隧道，407 让 reqwest 直接 Err，
        // 文字里也可能没有 407 字样 ⇒ `get` 只能给 Unreachable。此时唯一的识别路径是
        // `probe_member` 末尾那次 CONNECT 补判（T3 `confirm_auth_failure`）。
        struct Opaque;
        impl Prober for Opaque {
            fn connect(
                &self,
                _u: &Upstream,
                _h: &str,
                _p: u16,
            ) -> crate::modules::residential::proxy::ConnectVerdict {
                crate::modules::residential::proxy::ConnectVerdict::AuthFailed
            }
            fn get(&self, _up: &Upstream, _url: &str) -> Result<HttpProbe, ProbeError> {
                // 隧道建立阶段就失败了，reqwest 给的就是这种不带线索的错误
                Err(ProbeError::Unreachable("error trying to connect".into()))
            }
            fn google_search(&self, _up: &Upstream) -> Result<HttpProbe, ProbeError> {
                Err(ProbeError::Unreachable("error trying to connect".into()))
            }
            fn udp_associate(&self, _u: &Upstream) -> Result<bool, ProbeError> {
                Ok(true)
            }
            fn direct_tcp(&self, _h: &str, _p: u16) -> bool {
                true
            }
        }
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d, &[10]).await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        let p: Arc<dyn Prober> = Arc::new(Opaque);
        check_once(&c, p.clone(), clash.clone()).await.unwrap();
        host.advance(120);
        let out = check_once(&c, p, clash.clone()).await.unwrap();
        assert!(out.healthy.is_empty());
        let r = rstate::read(&c.runtime).await;
        assert!(
            r.upstream_alerts
                .values()
                .any(|a| a.contains("凭据") && a.contains("isp1.example.net:10007")),
            "没有 CONNECT 补判，这条告警在真机上永远不会出现：{:?}",
            r.upstream_alerts
        );
    }

    #[tokio::test]
    async fn all_unhealthy_keeps_the_current_member_and_alerts() {
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d, &[10, 20]).await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        let p = by_host(&["isp1.example.net", "isp2.example.net"], &[]);
        check_once(&c, p.clone(), clash.clone()).await.unwrap();
        host.advance(120);
        let out = check_once(&c, p, clash.clone()).await.unwrap();
        assert!(out.healthy.is_empty());
        assert_eq!(out.switched_to, None, "全不健康时保持（降级总比乱切好）");
        assert!(!clash.calls().iter().any(|x| x.starts_with("put:")));
        assert!(rstate::read(&c.runtime)
            .await
            .alerts
            .iter()
            .any(|a| a.contains("全部")));
    }

    #[tokio::test]
    async fn a_switch_is_rate_limited_to_one_per_minute() {
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d, &[10, 20]).await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        rstate::update(&c.runtime, |r| {
            r.last_switch_at = Some(crate::util::fmt_rfc3339(host.now()));
        })
        .await;
        let p = by_host(&["isp1.example.net"], &[]);
        check_once(&c, p.clone(), clash.clone()).await.unwrap();
        host.advance(30);
        let out = check_once(&c, p.clone(), clash.clone()).await.unwrap();
        assert_eq!(out.switched_to, None);
        assert!(
            out.notes.iter().any(|n| n.contains("限速")),
            "{:?}",
            out.notes
        );
        host.advance(31); // 距上次切换 61s
        let out = check_once(&c, p, clash.clone()).await.unwrap();
        assert_eq!(out.switched_to.as_deref(), Some("resi-2"));
    }

    #[tokio::test]
    async fn a_membership_change_during_the_round_skips_switching() {
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d, &[10, 20]).await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        let p = by_host(&["isp1.example.net"], &[]);
        let ids = vec![Uuid::from_u128(1), Uuid::from_u128(2)];
        // 先攒满迟滞，让「本来该切」成立
        check_round(&c, p.clone(), clash.clone(), ids.clone())
            .await
            .unwrap();
        host.advance(120);
        // 注入一份「本轮开始时池里有 3 个成员」的快照 = 探测中途管理员删了一条上游。
        // 直接在 check_once 前后改池是测不到这条规则的（那样前后快照相同，会落进规则 7）。
        let mut stale = ids.clone();
        stale.push(Uuid::from_u128(3));
        let out = check_round(&c, p.clone(), clash.clone(), stale)
            .await
            .unwrap();
        assert_eq!(out.switched_to, None);
        assert!(
            out.notes.iter().any(|n| n.contains("成员集")),
            "{:?}",
            out.notes
        );
        assert!(
            !clash.calls().iter().any(|x| x.starts_with("put:")),
            "本轮一个 PUT 都不发"
        );
        // 对照组：成员集没变的同一轮是会切的
        host.advance(120);
        let out2 = check_round(&c, p, clash.clone(), ids).await.unwrap();
        assert_eq!(out2.switched_to.as_deref(), Some("resi-2"));
    }

    #[tokio::test]
    async fn a_manual_selection_stays_locked_until_it_goes_unhealthy() {
        // R2 ①：手动切到 priority 更差的 resi-2 后，下一轮巡检不许按 priority 把它抢回
        // resi-1（以前只有 Clash API 生效、runtime 没有锁定位，池一动就回到优先级排序）
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d, &[5, 20]).await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        select_manual(&c, clash.clone(), Uuid::from_u128(2))
            .await
            .unwrap();
        assert_eq!(
            rstate::read(&c.runtime).await.manual_selected_id,
            Some(Uuid::from_u128(2)),
            "手动切换要在 runtime 里留下锁定位"
        );
        host.advance(120);
        let out = check_once(&c, by_host(&[], &[]), clash.clone())
            .await
            .unwrap();
        assert_eq!(out.switched_to, None);
        assert_eq!(
            clash.selected().as_deref(),
            Some("resi-2"),
            "两条都健康时 priority 5 也抢不走手动锁定的 resi-2"
        );
        // 手动目标连续两轮不达标 ⇒ 自动切走并**解除**锁定（否则锁着一条坏上游不放）
        let p = by_host(&["isp2.example.net"], &[]);
        check_once(&c, p.clone(), clash.clone()).await.unwrap();
        host.advance(120);
        let out = check_once(&c, p, clash.clone()).await.unwrap();
        assert_eq!(out.switched_to.as_deref(), Some("resi-1"));
        assert!(
            out.notes.iter().any(|n| n.contains("手动")),
            "要说清是手动锁定失效后才切的：{:?}",
            out.notes
        );
        assert_eq!(
            rstate::read(&c.runtime).await.manual_selected_id,
            None,
            "手动目标不健康 ⇒ 清空锁定，下一轮回到自动选路"
        );
    }

    #[tokio::test]
    async fn switching_back_onto_the_locked_upstream_keeps_the_lock() {
        // runtime 记的当前出口（resi-1）不健康，而手动锁定的 resi-2 还健康：这一轮是
        // 把锁定目标放回去，锁**不能**跟着解除（解了下一轮 priority 5 就抢走了）
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d, &[5, 20]).await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        rstate::update(&c.runtime, |r| {
            r.selected_upstream_id = Some(Uuid::from_u128(1));
            r.manual_selected_id = Some(Uuid::from_u128(2));
        })
        .await;
        let p = by_host(&["isp1.example.net"], &[]);
        check_once(&c, p.clone(), clash.clone()).await.unwrap();
        host.advance(120);
        let out = check_once(&c, p, clash.clone()).await.unwrap();
        assert_eq!(out.switched_to.as_deref(), Some("resi-2"));
        assert!(
            !out.notes.iter().any(|n| n.contains("解除手动锁定")),
            "这不是「锁定目标失效」：{:?}",
            out.notes
        );
        assert_eq!(
            rstate::read(&c.runtime).await.manual_selected_id,
            Some(Uuid::from_u128(2)),
            "切到的就是锁定目标 ⇒ 锁留着"
        );
    }

    #[tokio::test]
    async fn select_auto_releases_the_manual_lock() {
        let d = tempfile::tempdir().unwrap();
        let (c, _h) = ctx(&d, &[5, 20]).await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        select_manual(&c, clash.clone(), Uuid::from_u128(2))
            .await
            .unwrap();
        select_auto(&c).await;
        assert_eq!(rstate::read(&c.runtime).await.manual_selected_id, None);
        assert_eq!(
            clash.selected().as_deref(),
            Some("resi-2"),
            "解锁只清锁定位，不动 relay 当前选择（下一轮自己按选路规则挑）"
        );
        // 解锁后 priority 重新说话
        let out = check_once(&c, by_host(&["isp2.example.net"], &[]), clash.clone())
            .await
            .unwrap();
        assert_eq!(out.switched_to, None, "第 1 轮失败还在迟滞里");
    }

    #[tokio::test]
    async fn the_round_records_google_reachability_and_routes_around_a_blocked_upstream() {
        // R2 ②：主理人硬要求「住宅上游不封 Google」。resi-2 的 priority 最好但对
        // www.google.com 回 403 `Forbidden serp domain`（Bright Data 形态），
        // 所以当前出口 resi-1 挂掉时必须切到 Google 能用的 resi-3。
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d, &[10, 5, 20]).await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        let p = by_host_with_google(&["isp1.example.net"], &[], &["isp2.example.net"]);
        check_once(&c, p.clone(), clash.clone()).await.unwrap();
        host.advance(120);
        let out = check_once(&c, p, clash.clone()).await.unwrap();
        assert_eq!(
            out.switched_to.as_deref(),
            Some("resi-3"),
            "封 Google 的 resi-2 即使 priority 5 也不选"
        );
        let r = rstate::read(&c.runtime).await;
        let h2 = &r.health[&Uuid::from_u128(2).to_string()];
        let h3 = &r.health[&Uuid::from_u128(3).to_string()];
        assert_eq!(h2.google_ok, Some(false));
        assert_eq!(h3.google_ok, Some(true));
        assert!(
            h2.google_at.is_some(),
            "要记下判定时间，面板才知道是什么时候的"
        );
    }

    #[test]
    fn pick_target_prefers_an_upstream_that_can_still_use_google_and_honours_the_manual_lock() {
        let mut g = rstate::group_of(&crate::modules::residential::sample_state_with_pool());
        g.upstreams = vec![upstream(1, 5), upstream(2, 20)];
        let now = time::macros::datetime!(2026-09-12 00:00:00 UTC);
        let healthy = vec![Uuid::from_u128(1), Uuid::from_u128(2)];
        // google[i] = 这条上游最近一轮的 Google 判定
        let runtime = |google: [Option<bool>; 2], manual: Option<Uuid>| {
            let mut r = ResiRuntime {
                manual_selected_id: manual,
                ..Default::default()
            };
            for (i, ok) in google.iter().enumerate() {
                let mut h = rstate::HealthState::default();
                rstate::record_google(&mut h, *ok, now);
                r.health
                    .insert(Uuid::from_u128(i as u128 + 1).to_string(), h);
            }
            r
        };
        // priority 5 的 u1 封了 Google ⇒ 让给 priority 20 但 Google 能用的 u2
        assert_eq!(
            pick_target(&g, &runtime([Some(false), Some(true)], None), &healthy, now),
            Some(Uuid::from_u128(2)),
            "「Google 通」排在 priority 之前"
        );
        // 都能用 Google ⇒ 回到 priority
        assert_eq!(
            pick_target(&g, &runtime([Some(true), Some(true)], None), &healthy, now),
            Some(Uuid::from_u128(1))
        );
        // 「未知」不凭空赢过「通」
        assert_eq!(
            pick_target(&g, &runtime([None, Some(true)], None), &healthy, now),
            Some(Uuid::from_u128(2))
        );
        // 手动锁定压过 priority 与 Google 两个排序键：这里锁的正是两项都更差的 u2
        let locked = runtime([Some(true), Some(true)], Some(Uuid::from_u128(2)));
        assert_eq!(
            pick_target(&g, &locked, &healthy, now),
            Some(Uuid::from_u128(2)),
            "手动锁定的出口不许被 priority 抢走"
        );
        // 手动目标不健康 ⇒ 锁定不生效，回到自动排序
        assert_eq!(
            pick_target(&g, &locked, &[Uuid::from_u128(1)], now),
            Some(Uuid::from_u128(1))
        );
        // 手动目标已不在池里（删上游的残留）⇒ 同样忽略
        let dangling = runtime([Some(true), Some(true)], Some(Uuid::from_u128(99)));
        assert_eq!(
            pick_target(&g, &dangling, &healthy, now),
            Some(Uuid::from_u128(1))
        );
    }

    #[test]
    fn pick_target_breaks_priority_ties_by_the_24h_success_rate() {
        let mut g = rstate::group_of(&crate::modules::residential::sample_state_with_pool());
        g.upstreams = vec![upstream(1, 10), upstream(2, 10), upstream(3, 10)];
        let now = time::macros::datetime!(2026-09-12 00:00:00 UTC);
        let mut r = ResiRuntime::default();
        // health 以 uuid 的字符串形式为键（契约决策 §C）
        for (i, oks) in [(1u128, 1u32), (2, 3), (3, 2)] {
            let mut h = rstate::HealthState::default();
            for k in 0..4 {
                rstate::record_probe(&mut h, k < oks, now - time::Duration::hours(1));
            }
            r.health.insert(Uuid::from_u128(i).to_string(), h);
        }
        let healthy = vec![Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3)];
        assert_eq!(
            pick_target(&g, &r, &healthy, now),
            Some(Uuid::from_u128(2)),
            "3/4 > 2/4 > 1/4"
        );
        // 优先级压过成功率
        g.upstreams[0].priority = 1;
        assert_eq!(pick_target(&g, &r, &healthy, now), Some(Uuid::from_u128(1)));
        assert_eq!(pick_target(&g, &r, &[], now), None);
        // 不在当前池里的 uuid 直接忽略（删上游与巡检并发时会出现）
        assert_eq!(pick_target(&g, &r, &[Uuid::from_u128(99)], now), None);
    }

    #[tokio::test]
    async fn a_relay_restart_replays_the_selected_upstream() {
        let d = tempfile::tempdir().unwrap();
        let (c, _h) = ctx(&d, &[10, 20]).await;
        rstate::update(&c.runtime, |r| {
            r.selected_upstream_id = Some(Uuid::from_u128(2))
        })
        .await;
        // relay 重启后 selector 回到配置里的 default（池首），必须把运行时选择重放回去
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        // **先 subscribe 再 spawn**：broadcast 丢弃「发送时还没有订阅者」的事件，
        // 若让 replay_loop 自己 subscribe，spawn 之后立刻 send 的这条必丢
        let rx = c.bus.subscribe();
        let task = tokio::spawn(replay_loop(c.clone(), clash.clone(), rx));
        c.bus.send(Event::RelayRestarted);
        for _ in 0..50 {
            if clash.selected().as_deref() == Some("resi-2") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        task.abort();
        assert_eq!(
            clash.selected().as_deref(),
            Some("resi-2"),
            "spec §5.3：relay 重启后重放选择"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_health_loop_runs_a_round_every_two_minutes() {
        let d = tempfile::tempdir().unwrap();
        let (c, _h) = ctx(&d, &[10, 20]).await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        let task = tokio::spawn(health_loop(c.clone(), by_host(&[], &[]), clash.clone()));
        // 用「等到出现」而不是精确计时：一轮里有 spawn_blocking，与假时钟没有确定的先后。
        // 等待用 `sleep` 而不是 `yield_now`：current_thread 调度器只在本地队列排空或每
        // 61 tick 才捞一次远端队列，而 spawn_blocking 的完成唤醒落在远端队列里 ——
        // 空转 yield 会把本地队列一直填满，一轮里那几次 spawn_blocking 攒不够捞取次数。
        // `sleep` 会让运行时真正 park（假时钟随之自动推进），唤醒必被取走。
        for _ in 0..500 {
            if !clash.calls().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        assert!(
            !clash.calls().is_empty(),
            "interval 第一 tick 立即完成 ⇒ 起来就跑一轮"
        );
        let first = clash.calls().len();
        tokio::time::advance(std::time::Duration::from_secs(HEALTH_INTERVAL_SECS + 1)).await;
        for _ in 0..500 {
            if clash.calls().len() > first {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        assert!(
            clash.calls().len() > first,
            "每 {HEALTH_INTERVAL_SECS} 秒再跑一轮"
        );
        task.abort();
    }

    #[tokio::test]
    async fn manual_select_uses_clash_only_and_rejects_unknown_ids() {
        let d = tempfile::tempdir().unwrap();
        let (c, _h) = ctx(&d, &[10, 20]).await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        let tag = select_manual(&c, clash.clone(), Uuid::from_u128(2))
            .await
            .unwrap();
        assert_eq!(tag, "resi-2");
        assert_eq!(clash.selected().as_deref(), Some("resi-2"));
        assert_eq!(
            rstate::read(&c.runtime).await.selected_upstream_id,
            Some(Uuid::from_u128(2))
        );
        // 不写 state ⇒ 不重启 relay
        assert_eq!(
            rstate::group_of(&*c.store.read().await).selected_upstream_id,
            Some(Uuid::from_u128(1))
        );
        assert!(select_manual(&c, clash, Uuid::from_u128(99)).await.is_err());
    }
}
