//! 自动黑名单（spec §5.4）：journald 候选学习 → 硬拒确认 → 每日 04:00 批量生效 →
//! 每日复核移除，以及管理员手动的 `pins` / `apply_now` / `remove_auto`。
//!
//! 三段式的存放位置与生效方式见计划「契约决策 §D」：候选与待生效条目都在
//! `runtime.extra["residential"]`（不掐连接），只有写 `state.blacklist` 才会经 P1 的
//! 对账器重渲染 `singbox-relay.json` 并重启 `b-ui-relay`。

use super::proxy::{ConnectVerdict, Prober};
use super::{
    check, clash, journal, port_probe_host, state, BASE_PORTS, CANDIDATE_THRESHOLD,
    CONFIRM_MIN_GAP_SECS, CONFIRM_NEEDED, DAILY_HOUR, DAILY_TICK_SECS, JOURNAL_POLL_SECS,
    PROBE_PORTS, REVIEW_PASSES_TO_REMOVE,
};
use crate::reconcile::DaemonCtx;
use crate::util::parse_rfc3339;
use bui_schema::model::{AutoEntry, Pin, Rule, Upstream};
use clash::Clash;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use uuid::Uuid;

/// 每日固定探针集（spec §5.4 (b)）：支付域名集 + R13 §6.1 内置候选里 2026-09-11 实测被拒的那些。
/// **只是「值得一探」的候选**，进不进黑名单完全由探测结果决定（R13 §6.1 的硬要求：
/// 新上游的黑名单从空开始，没有任何预置条目）。
pub const PROBE_SET: [&str; 12] = [
    "checkout.stripe.com",
    "pay.google.com",
    "www.paypal.com",
    "api.stripe.com",
    "gateway.icloud.com",
    "query.ess.apple.com",
    "courier.push.apple.com",
    "x.com",
    "api.x.com",
    "www.google.com",
    "www.tiktok.com",
    "www.instagram.com",
];

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DailyReport {
    /// 本轮从日志新增/累加的候选数
    pub learned: usize,
    /// 本轮被探针集推成候选的条目数（确认交给 `journal_loop` 的确认轮）
    pub probed: usize,
    /// 写进 `state.blacklist.auto` 的条目数
    pub applied: usize,
    /// 复核通过被移除的条目数
    pub removed: usize,
    /// 本轮端口集重探后更新了 `ports_allowed` 的上游数（spec §5.4 (b)）
    pub ports_learned: usize,
    /// 是否把运行时落点的漂移写回了 state（契约决策 §C 的 D4）
    pub persisted_selection: bool,
    pub notes: Vec<String>,
}

/// 每日端口集重探对一个上游的结论
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortLearn {
    /// 本轮干净：把该上游的 `ports_allowed` 写成这个值（`None` = 不限）
    Learned(Option<Vec<u16>>),
    /// 本轮不可信（407 / 有探测抖动）：保留上一次学到的白名单，一个字节都不动
    Keep,
}

/// 裸 IP 目标不进黑名单（spec §5.4：规则是 `domain_suffix` = 完整主机名；端口类由
/// `ports_allowed` 表达）
pub fn is_bare_ip(host: &str) -> bool {
    host.parse::<std::net::IpAddr>().is_ok()
}

pub fn candidate_key(upstream_id: Uuid, host: &str, port: u16) -> String {
    format!("{upstream_id}|{host}|{port}")
}

/// **端口类拒绝不进域名黑名单**（裁决）：只有落在该上游 `ports_allowed` 白名单里的端口
/// 被拒，才算「这个上游代理不了这个域名」；白名单外端口的硬拒是**端口策略**，已由
/// `ports_allowed` 表达（T6 体检学得），再生成一条 `domain_suffix` 规则就是把整个域名
/// 误判成不可代理。`None` = 还没学到白名单 ⇒ 按基准端口 [`BASE_PORTS`]（`[80, 443]`）看待。
pub fn port_allowed(up: &Upstream, port: u16) -> bool {
    // 把 courier.push.apple.com:5228 的 403 变成一条 domain_suffix 规则，会把该上游本来
    // 能代理的 443 流量也推回直连。
    match up.ports_allowed.as_deref() {
        Some(list) => list.contains(&port),
        // 还没体检过 ⇒ 只认基准端口（spec §5.2 的 80/443）
        None => BASE_PORTS.contains(&port),
    }
}

/// 从 journald 增量学候选（每 [`JOURNAL_POLL_SECS`] 秒一次）。返回新增/累加的条目数。
/// 只计 [`port_allowed`] 为真的拒绝。
///
/// **归因**（2026-09-18 裁决）：生产的 relay 日志只打池 tag（`selector[slot-<i>-pool]` /
/// `urltest[resi-pool]`），成员 tag 一个字都不出现（[`journal::RelaySubject`]），所以
/// 「哪个上游」要在这里换：路径 A 用 reason 里的 `dial tcp` 地址（零 I/O），路径 B 问一次
/// relay 的 Clash API 拿池的 `now`（**每池只查一次**）。归不了因的行一律丢弃，**绝不猜**。
/// 路径 B 的行还要过一道**时间戳门**（[`clash::within_switch_grace`]，2026-09-18 第二次
/// 裁决）：这条链的轮询间隔是 [`JOURNAL_POLL_SECS`] 秒，「日志行产生 → 读 `now`」之间只要
/// b-ui 切过一次池，老成员的失败就记到新成员头上；候选表按 `(upstream_id, host, port)`
/// 累计、误记会一直攒着，所以本批证据宁可放弃、等下次复发。
pub async fn learn_from_journal(ctx: &DaemonCtx, clash: Arc<dyn Clash>) -> anyhow::Result<usize> {
    let (cursor, switch_at) = {
        let r = state::read(&ctx.runtime).await;
        (r.journal_cursor, r.pool_switch_at)
    };
    let host = ctx.host.clone();
    // **碰 `Host` 一律 spawn_blocking**（与 P1 的 `Fetcher`/`Facts::probe` 同一条铁律）：
    // `RealHost::which` 会同步扫 PATH、`collect` 会 exec journalctl，两件事放进同一次
    // 阻塞任务里，async 上下文里一次 Host 调用都不留。
    let probed = tokio::task::spawn_blocking(move || {
        if !host.which("journalctl") {
            return None;
        }
        Some(journal::collect(host.as_ref(), cursor.as_deref()))
    })
    .await?;
    let Some(collected) = probed else {
        state::update(&ctx.runtime, |r| {
            state::push_alert(r, "机器上没有 journalctl，黑名单候选只能靠每日探针集");
        })
        .await;
        return Ok(0);
    };
    let batch = match collected {
        Ok(b) => b,
        Err(e) => {
            // 游标失效（日志轮转/重启）→ 丢掉游标，下一轮用 --since -25h 重来
            tracing::warn!(error = %e, "journalctl 读取失败，重置游标");
            state::update(&ctx.runtime, |r| r.journal_cursor = None).await;
            return Ok(0);
        }
    };
    let g = state::group_of(&*ctx.store.read().await);
    let now = crate::util::fmt_rfc3339(ctx.host.now());
    let mut learned = 0usize;
    // ── 归因层（有 I/O）────────────────────────────────────────────────────────
    let pools: Vec<String> = batch
        .lines
        .iter()
        .filter_map(|l| clash::pool_of(&l.subject).map(str::to_string))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let pool_now = clash::pool_now(&clash, &pools).await;
    let mut lines: Vec<(Uuid, journal::RejectLine)> = Vec::new();
    for l in &batch.lines {
        match clash::attribute(&g, &l.subject, &pool_now, &switch_at, l.ts) {
            clash::Attrib::Upstream(id) => lines.push((id, l.clone())),
            clash::Attrib::Switched => {
                tracing::debug!(host = %l.host, port = l.port, "这条行产生时该池刚被切过，本批证据放弃、等下次复发")
            }
            clash::Attrib::Unknown => {
                tracing::debug!(host = %l.host, port = l.port, "relay 拒绝行归不了因，丢弃（绝不猜）")
            }
        }
    }
    // ──────────────────────────────────────────────────────────────────────────
    state::update(&ctx.runtime, |r| {
        for (id, l) in &lines {
            let id = *id;
            // 裸 IP 目标（推送、非白名单端口）由 ports_allowed 表达，不进黑名单（spec §5.4）
            if is_bare_ip(&l.host) {
                continue;
            }
            // 端口类拒绝不进域名黑名单（裁决）：白名单外端口一律不计候选
            let Some(up) = g.upstreams.iter().find(|u| u.id == id) else {
                continue;
            };
            if !port_allowed(up, l.port) {
                continue;
            }
            let key = candidate_key(id, &l.host, l.port);
            let e = r.candidates.entry(key).or_insert_with(|| state::Candidate {
                upstream_id: id,
                host: l.host.clone(),
                port: l.port,
                hits: 0,
                first_seen: now.clone(),
                last_seen: now.clone(),
                confirms: 0,
                last_confirm_at: None,
            });
            e.hits += 1;
            e.last_seen = now.clone();
            learned += 1;
        }
        if batch.cursor.is_some() {
            r.journal_cursor = batch.cursor.clone();
        }
    })
    .await;
    Ok(learned)
}

/// 候选学习 + 确认的同一个循环（裁决「黑名单确认节奏」）：每 [`JOURNAL_POLL_SECS`] 秒
/// **先** [`learn_from_journal`] **再** [`confirm_round`]，所以一条候选攒够「间隔 ≥ 10
/// 分钟、连续 [`CONFIRM_NEEDED`] 次」就能进 `pending`（≥10 分钟即可），不必等 04:00。
pub async fn journal_loop(ctx: DaemonCtx, p: Arc<dyn Prober>, c: Arc<dyn Clash>) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(JOURNAL_POLL_SECS));
    loop {
        tick.tick().await;
        // 顺序是裁决的一部分：**先 learn 再 confirm**。本轮新攒到阈值的候选立刻就能
        // 做第 1 次确认，于是「间隔 ≥ CONFIRM_MIN_GAP_SECS、连续 CONFIRM_NEEDED 次」
        // 最快 10 分钟走完（第 1 轮 + t=600 秒那轮），而不是一天一次、要两天。
        if let Err(e) = learn_from_journal(&ctx, c.clone()).await {
            tracing::warn!(error = %e, "黑名单候选学习失败");
        }
        match confirm_round(&ctx, p.clone()).await {
            Ok(n) if n > 0 => {
                tracing::info!(promoted = n, "黑名单候选确认完毕，等 04:00 批量生效")
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "黑名单候选确认失败"),
        }
    }
}

/// 单条确认：经该上游**硬拒** 且 直连同目标 TCP 可达 → `Some(true)`；上游说通 →
/// `Some(false)`（复核路径据此累计 `passes`）；其余一律 `None`（未知，不改状态）——
/// 包括「硬拒但直连也不通」：那既不能算代理的错，也绝不能当成 pass，否则 3 天后会
/// 误删本该保留的 `auto` 条目。
pub fn confirm_once(p: &dyn Prober, up: &Upstream, host: &str, port: u16) -> Option<bool> {
    match p.connect(up, host, port) {
        // 硬拒 + 直连可达 ⇒ 确认是「这个上游代理不了它」（R13 §5.3）
        ConnectVerdict::Refused { .. } if p.direct_tcp(host, port) => Some(true),
        // 硬拒但直连也不通 ⇒ 未知。**不能返回 Some(false)**：复核路径会把它当 pass 累计，
        // 连续 3 天目标自己挂着就会误删一条本该保留的 auto 条目
        ConnectVerdict::Refused { .. } => None,
        ConnectVerdict::Open => Some(false),
        // 凭据失效 / 连不上 ⇒ 未知，不改状态（绝不因为一轮失败清空或误加）
        ConnectVerdict::AuthFailed | ConnectVerdict::Unreachable { .. } => None,
    }
}

/// 把达到阈值的候选推进一步：**确认进度记在候选上**（`Candidate.confirms` /
/// `last_confirm_at`），只有攒到「间隔 ≥ 10 分钟、连续 `CONFIRM_NEEDED` 次」才升进
/// `runtime.pending`（并出候选池）。返回本轮新升进 `pending` 的条目数。
pub async fn confirm_round(ctx: &DaemonCtx, p: Arc<dyn Prober>) -> anyhow::Result<usize> {
    let g = state::group_of(&*ctx.store.read().await);
    let r = state::read(&ctx.runtime).await;
    let now = ctx.host.now();
    // 只探「达到阈值」且「距上次确认 ≥ 10 分钟」的候选：间隔是 spec §5.4 的硬要求，
    // 没有它同一波抖动会在一轮里连中两次直接进黑名单
    let due: Vec<(String, state::Candidate, Upstream)> = r
        .candidates
        .iter()
        .filter(|(_, c)| c.hits >= CANDIDATE_THRESHOLD)
        .filter(
            |(_, c)| match c.last_confirm_at.as_deref().and_then(parse_rfc3339) {
                Some(t) => (now - t).whole_seconds() >= CONFIRM_MIN_GAP_SECS,
                None => true,
            },
        )
        .filter_map(|(k, c)| {
            let up = g.upstreams.iter().find(|u| u.id == c.upstream_id)?.clone();
            Some((k.clone(), c.clone(), up))
        })
        .collect();
    if due.is_empty() {
        return Ok(0);
    }
    let pp = p.clone();
    let verdicts = super::fanout(due, move |(k, c, up)| {
        (k, confirm_once(pp.as_ref(), &up, &c.host, c.port))
    })
    .await;

    let mut promoted = 0usize;
    let now_s = crate::util::fmt_rfc3339(now);
    state::update(&ctx.runtime, |r| {
        for (key, verdict) in verdicts.into_iter().flatten() {
            let Some(c) = r.candidates.get_mut(&key) else {
                continue;
            };
            match verdict {
                // 未知（凭据失效 / 连不上 / 硬拒但直连也不通）：一个字段都不动
                None => {}
                Some(false) => {
                    // 现在通了：确认进度与计数一起归零，别把一次抖动攒成黑名单
                    c.confirms = 0;
                    c.hits = 0;
                    c.last_confirm_at = Some(now_s.clone());
                }
                Some(true) => {
                    c.confirms += 1;
                    c.last_confirm_at = Some(now_s.clone());
                }
            }
        }
        // 攒满 CONFIRM_NEEDED 次的候选升进 pending 并出候选池，等 04:00 批量生效。
        // 进了 pending 就等于「确认完毕」，所以 flush_pending / apply_now 不再判 confirms。
        let done: Vec<String> = r
            .candidates
            .iter()
            .filter(|(_, c)| c.confirms >= CONFIRM_NEEDED)
            .map(|(k, _)| k.clone())
            .collect();
        for key in done {
            let Some(c) = r.candidates.remove(&key) else {
                continue;
            };
            r.pending.retain(|e| {
                !(e.upstream_id == c.upstream_id && e.host == c.host && e.port == c.port)
            });
            r.pending.push(state::PendingEntry {
                upstream_id: c.upstream_id,
                host: c.host,
                port: c.port,
                hits: c.hits,
                confirms: c.confirms,
                last_confirm_at: now_s.clone(),
            });
            promoted += 1;
        }
    })
    .await;
    Ok(promoted)
}

/// `ports` 表 →「本轮该不该改 `ports_allowed`」（纯函数）
pub fn port_learn(ports: &BTreeMap<u16, String>, auth_failed: bool) -> PortLearn {
    // 凭据失效、任一项探测失败、或一个端口都没探到 ⇒ 本轮结论不可信，保留上次的白名单。
    // 这里若改成「写 None（不限）」，一次网络抖动就会静默放开端口策略。
    if auth_failed
        || ports.is_empty()
        || ports
            .values()
            .any(|v| v == "unreachable" || v == "auth_failed")
    {
        return PortLearn::Keep;
    }
    PortLearn::Learned(check::derive_ports_allowed(ports, false))
}

/// spec §5.4 (b) 的**端口集**部分：每个上游对 `BASE_PORTS ∪ PROBE_PORTS` 重探一遍。
/// 结论在每日那一次 `Store::update` 里一起写回，不额外多一次 relay 重启。
pub async fn probe_ports_daily(p: Arc<dyn Prober>, ups: &[Upstream]) -> Vec<(Uuid, PortLearn)> {
    let plist: Vec<u16> = BASE_PORTS
        .iter()
        .chain(PROBE_PORTS.iter())
        .copied()
        .collect();
    let jobs: Vec<(Upstream, u16)> = ups
        .iter()
        .flat_map(|u| plist.iter().map(move |port| (u.clone(), *port)))
        .collect();
    let pp = p.clone();
    // 目标主机由 `port_probe_host` 给（与 T6 体检第 ④ 步同一份表）：基准 80/443 打中性
    // 主机 www.gstatic.com——用搜索域名会被 Bright Data 整域硬拒，基准端口全判不通，
    // `port_learn` 于是永远学不出白名单；固定端口集打各自真在该端口监听的主机
    let probed = super::fanout(jobs.clone(), move |(up, port)| {
        pp.connect(&up, port_probe_host(port), port)
    })
    .await;
    let mut per_up: BTreeMap<Uuid, (BTreeMap<u16, String>, bool)> = BTreeMap::new();
    for (i, (up, port)) in jobs.iter().enumerate() {
        let e = per_up.entry(up.id).or_default();
        match probed.get(i).and_then(|x| x.as_ref()) {
            // fanout 的 None = 该项的阻塞任务 panic 了
            None => {
                e.0.insert(*port, "unreachable".into());
            }
            Some(v) => {
                if *v == ConnectVerdict::AuthFailed {
                    e.1 = true;
                }
                e.0.insert(*port, v.label());
            }
        }
    }
    per_up
        .into_iter()
        .map(|(id, (ports, af))| (id, port_learn(&ports, af)))
        .collect()
}

/// `pending` → `g.blacklist.auto`（同一条重复出现时只累加 hits）。**纯函数**，
/// [`flush_pending`] 与 [`daily_round`] 那一次 `update_group` 共用同一份合并逻辑。
fn merge_pending(
    g: &mut bui_schema::model::ResidentialGroup,
    pending: &[state::PendingEntry],
    at: &str,
) {
    for e in pending {
        // 规则值就是被拒的完整主机名；domain_suffix 已覆盖其子域，不泛化到注册域名
        let rule = Rule::DomainSuffix(e.host.clone());
        match g
            .blacklist
            .auto
            .iter_mut()
            .find(|a| a.upstream_id == e.upstream_id && a.rule == rule)
        {
            Some(a) => {
                a.hits += e.hits;
                a.last_verified_at = at.to_string();
                a.passes = 0;
            }
            None => g.blacklist.auto.push(AutoEntry {
                upstream_id: e.upstream_id,
                rule,
                hits: e.hits,
                confirmed_at: at.to_string(),
                last_verified_at: at.to_string(),
                passes: 0,
            }),
        }
    }
}

/// `pending` → `state.blacklist.auto`，返回写入条数。
/// **`pending` 为空时直接返回**：不写 state、不发 `StateChanged`——面板「立即应用」
/// 连点两下（或 pending 早就空了）不该白走一次「重渲染 relay 配置 + 重启 b-ui-relay」的
/// 对账（spec §5.4：relay 重启是唯一掐连接的动作）。
async fn flush_pending(ctx: &DaemonCtx) -> anyhow::Result<usize> {
    let pending = state::read(&ctx.runtime).await.pending;
    if pending.is_empty() {
        return Ok(0);
    }
    let at = crate::util::fmt_rfc3339(ctx.host.now());
    let n = pending.len();
    state::update_group(&ctx.store, &ctx.bus, move |g| {
        merge_pending(g, &pending, &at);
    })
    .await?;
    state::update(&ctx.runtime, |r| r.pending.clear()).await;
    Ok(n)
}

/// 管理员「立即应用」：把 `runtime.pending` 全部写进 `state.blacklist.auto`（立刻重启 relay）
pub async fn apply_now(ctx: &DaemonCtx) -> anyhow::Result<usize> {
    flush_pending(ctx).await
}

/// 每日窗口（服务器本地 04:00）：learn → 探针集（只探 `port_allowed` 为真的上游）→
/// **端口集重探** → 批量写 state（含 `ports_allowed` 与落点漂移）→ 复核移除。
/// **确认不在这里**：确认节奏归 [`journal_loop`] 的每 5 分钟一轮（裁决「黑名单确认节奏」），
/// 04:00 只做批量生效 + spec §5.4 (b) 的每日探针/端口集 + 每日复核。
/// 这是**唯一**会因 `auto` 变化而重启 relay 的时机（spec §5.4「relay 重启是唯一掐连接的动作」）。
pub async fn daily_round(
    ctx: &DaemonCtx,
    p: Arc<dyn Prober>,
    c: Arc<dyn Clash>,
) -> anyhow::Result<DailyReport> {
    let mut rep = DailyReport::default();
    let g = state::group_of(&*ctx.store.read().await);
    if !g.pool_active() {
        rep.notes.push("住宅池未启用，跳过每日黑名单轮次".into());
        return Ok(rep);
    }
    // ① 从日志学
    rep.learned = learn_from_journal(ctx, c).await?;
    // ② 固定探针集：每个上游 × PROBE_SET，把被拒的推成候选（≥ 阈值，等 journal_loop
    // 的确认轮接手）。探针集固定打 443，所以 **443 不在该上游 ports_allowed 白名单里
    // 的上游整条跳过**（裁决「端口类拒绝不进域名黑名单」：那种上游的 443 全被拒是端口
    // 策略，不能变成一堆域名规则）
    let now_s = crate::util::fmt_rfc3339(ctx.host.now());
    let jobs: Vec<(Upstream, String)> = g
        .upstreams
        .iter()
        .filter(|u| port_allowed(u, 443))
        .flat_map(|u| PROBE_SET.iter().map(move |h| (u.clone(), (*h).to_string())))
        .collect();
    let pp = p.clone();
    let probed = super::fanout(jobs, move |(up, h)| {
        (up.id, h.clone(), confirm_once(pp.as_ref(), &up, &h, 443))
    })
    .await;
    let mut seeded = 0usize;
    state::update(&ctx.runtime, |r| {
        for (id, host, verdict) in probed.into_iter().flatten() {
            if verdict != Some(true) {
                continue;
            }
            seeded += 1;
            let key = candidate_key(id, &host, 443);
            let e = r.candidates.entry(key).or_insert_with(|| state::Candidate {
                upstream_id: id,
                host: host.clone(),
                port: 443,
                hits: 0,
                first_seen: now_s.clone(),
                last_seen: now_s.clone(),
                confirms: 0,
                last_confirm_at: None,
            });
            // 探针集是主动证据，一次就够阈值（阈值只用来过滤日志噪声）
            e.hits = e.hits.max(CANDIDATE_THRESHOLD);
            e.last_seen = now_s.clone();
        }
    })
    .await;
    rep.probed = seeded;
    // ②b spec §5.4 (b) 的端口集部分：每日对 BASE_PORTS ∪ PROBE_PORTS 重探一遍。
    // 手动「体检」按钮也会学 ports_allowed，但没人保证运维每天点它，所以每日轮次必须自己探。
    let ports_learned = probe_ports_daily(p.clone(), &g.upstreams).await;
    rep.ports_learned = ports_learned
        .iter()
        .filter(|(_, l)| matches!(l, PortLearn::Learned(_)))
        .count();
    // ③ **没有确认这一步**：确认归 `journal_loop` 的每 JOURNAL_POLL_SECS 一轮（裁决
    // 「黑名单确认节奏」）。04:00 只做批量生效 + 每日探针/端口集 + 复核；②
    // 刚推成的候选会在接下来的两轮确认（≥10 分钟）里升进 pending，等下一个窗口生效。
    // ④ 复核已生效的 auto：连续 3 次不再被拒 → 移除
    let autos: Vec<(AutoEntry, Upstream)> = g
        .blacklist
        .auto
        .iter()
        .filter_map(|a| {
            // Port 规则是端口策略，不走域名复探
            if !matches!(a.rule, Rule::DomainSuffix(_) | Rule::Domain(_)) {
                return None;
            }
            let up = g.upstreams.iter().find(|u| u.id == a.upstream_id)?.clone();
            Some((a.clone(), up))
        })
        .collect();
    let pp = p.clone();
    let reviewed = super::fanout(autos, move |(a, up)| {
        let host = match &a.rule {
            Rule::DomainSuffix(h) | Rule::Domain(h) => h.clone(),
            Rule::Port(_) => return (a, None),
        };
        (a.clone(), confirm_once(pp.as_ref(), &up, &host, 443))
    })
    .await;
    let at = crate::util::fmt_rfc3339(ctx.host.now());
    let mut removed = 0usize;
    // ⑤ 一次 Store::update 里完成：批量写入 + 复核更新/移除 + 落点漂移写回（只重启一次 relay）
    let r = state::read(&ctx.runtime).await;
    // 契约决策 §C 的 D4：runtime 存的就是 uuid，不需要任何 tag 换算；上游已被删掉就别写回
    let persist_id = r
        .selected_pending_persist
        .then_some(r.selected_upstream_id)
        .flatten()
        .filter(|id| g.upstreams.iter().any(|u| u.id == *id));
    // 待生效条目在**这一次**写盘里合并（不经 flush_pending：复核移除、端口白名单与落点
    // 写回都得和它同一次 update_group，否则一轮 04:00 会重启 relay 两次）
    let pending = r.pending.clone();
    rep.applied = pending.len();
    state::update_group(&ctx.store, &ctx.bus, |g| {
        merge_pending(g, &pending, &at);
        for (a, verdict) in reviewed.into_iter().flatten() {
            let Some(cur) = g
                .blacklist
                .auto
                .iter_mut()
                .find(|x| x.upstream_id == a.upstream_id && x.rule == a.rule)
            else {
                continue;
            };
            match verdict {
                Some(false) => {
                    cur.passes += 1;
                    cur.last_verified_at = at.clone();
                }
                Some(true) => {
                    cur.passes = 0;
                    cur.last_verified_at = at.clone();
                }
                None => {}
            }
        }
        let before = g.blacklist.auto.len();
        g.blacklist
            .auto
            .retain(|a| a.passes < REVIEW_PASSES_TO_REMOVE);
        removed = before - g.blacklist.auto.len();
        // ②b 学到的端口白名单在同一次写盘里落地（PortLearn::Keep 的一律不动）
        for (id, learn) in &ports_learned {
            if let PortLearn::Learned(allowed) = learn {
                if let Some(u) = g.upstreams.iter_mut().find(|u| u.id == *id) {
                    u.ports_allowed = allowed.clone();
                }
            }
        }
        // 契约决策 §C 的 D4：自动切换造成的落点漂移在这个窗口写回，不额外多一次重启
        if let Some(id) = persist_id {
            g.selected_upstream_id = Some(id);
        }
    })
    .await?;
    rep.removed = removed;
    rep.persisted_selection = persist_id.is_some();
    let done_at = at.clone();
    let flushed = rep.applied > 0;
    state::update(&ctx.runtime, move |r| {
        r.last_daily_at = Some(done_at);
        if flushed {
            r.pending.clear();
        }
        if persist_id.is_some() {
            r.selected_pending_persist = false;
        }
    })
    .await;
    Ok(rep)
}

pub async fn daily_loop(ctx: DaemonCtx, p: Arc<dyn Prober>, c: Arc<dyn Clash>) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(DAILY_TICK_SECS));
    loop {
        tick.tick().await;
        let now = ctx.host.now();
        // `OffsetDateTime::hour()` 已经是 u8，套 u8::from 会被 clippy::useless_conversion 拦下
        if now.hour() != DAILY_HOUR {
            continue;
        }
        // 同一小时内只跑一次
        let last = state::read(&ctx.runtime)
            .await
            .last_daily_at
            .as_deref()
            .and_then(parse_rfc3339);
        if last.is_some_and(|t| (now - t).whole_seconds() < 3_600) {
            continue;
        }
        match daily_round(&ctx, p.clone(), c.clone()).await {
            Ok(rep) => tracing::info!(
                learned = rep.learned,
                probed = rep.probed,
                applied = rep.applied,
                removed = rep.removed,
                ports_learned = rep.ports_learned,
                "住宅黑名单每日轮次完成"
            ),
            Err(e) => tracing::warn!(error = %e, "住宅黑名单每日轮次失败"),
        }
    }
}

/// pins 增删（立即生效）
pub async fn add_pin(ctx: &DaemonCtx, rule: Rule, note: String) -> anyhow::Result<()> {
    let at = crate::util::fmt_rfc3339(ctx.host.now());
    state::update_group(&ctx.store, &ctx.bus, move |g| {
        if g.blacklist.pins.iter().any(|p| p.rule == rule) {
            return; // 幂等
        }
        g.blacklist.pins.push(Pin {
            rule,
            note,
            created_at: at,
        });
    })
    .await
}

pub async fn remove_pin(ctx: &DaemonCtx, rule: &Rule) -> anyhow::Result<bool> {
    let g = state::group_of(&*ctx.store.read().await);
    if !g.blacklist.pins.iter().any(|p| p.rule == *rule) {
        return Ok(false);
    }
    let rule = rule.clone();
    state::update_group(&ctx.store, &ctx.bus, move |g| {
        g.blacklist.pins.retain(|p| p.rule != rule)
    })
    .await?;
    Ok(true)
}

/// 手动删一条 `auto`（面板「误伤了，删掉」）
pub async fn remove_auto(ctx: &DaemonCtx, upstream_id: Uuid, rule: &Rule) -> anyhow::Result<bool> {
    let g = state::group_of(&*ctx.store.read().await);
    if !g
        .blacklist
        .auto
        .iter()
        .any(|a| a.upstream_id == upstream_id && a.rule == *rule)
    {
        return Ok(false);
    }
    let rule = rule.clone();
    state::update_group(&ctx.store, &ctx.bus, move |g| {
        g.blacklist
            .auto
            .retain(|a| !(a.upstream_id == upstream_id && a.rule == rule));
    })
    .await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::EventBus;
    use crate::modules::residential::clash::FakeClash;
    use crate::modules::residential::proxy::{ConnectVerdict, HttpProbe, ProbeError};
    use crate::modules::residential::state as rstate;
    use crate::modules::sentinel::fixtures_relay as fx;
    use crate::state::runtime::Runtime;
    use crate::state::store::Store;
    use crate::sys::{fake::FakeHost, CmdOut, Host};
    use crate::testutil::sample_state;
    use bui_schema::model::{Upstream, UpstreamKind};
    use pretty_assertions::assert_eq;
    use std::sync::Arc;

    const HTTP_403: &str = "open connection to gateway.icloud.com:443 using outbound/http[resi-1]: unexpected status: 403 Forbidden";
    const IP_403: &str = "open connection to 198.51.100.9:5228 using outbound/http[resi-1]: unexpected status: 403 Forbidden";

    async fn ctx(d: &tempfile::TempDir) -> (DaemonCtx, Arc<FakeHost>) {
        let mut s = sample_state();
        let g = s.residential.groups.get_mut("default").unwrap();
        g.enabled = true;
        g.blacklist.auto.clear();
        g.blacklist.pins.clear();
        g.upstreams = vec![Upstream {
            id: Uuid::from_u128(1),
            name: "url-1".into(),
            kind: UpstreamKind::Http,
            host: "isp1.example.net".into(),
            port: 10007,
            username: "user1".into(),
            password: "pw1".into(),
            priority: 10,
            provider: None,
            region: None,
            ports_allowed: None,
            verified: None,
        }];
        g.selected_upstream_id = Some(Uuid::from_u128(1));
        let host = Arc::new(FakeHost::new());
        host.with(|i| {
            i.which.insert("journalctl".into());
        });
        let c = DaemonCtx {
            store: Store::create(d.path().join("state.json"), s).await.unwrap(),
            runtime: Runtime::load(d.path().join("runtime.json")),
            bus: EventBus::new(),
            host: host.clone(),
            paths: bui_schema::paths::Paths::default_server(),
        };
        (c, host)
    }

    /// 指定哪些 `host:port` 被上游硬拒、哪些直连可达
    struct Rejector {
        refused: Vec<String>,
        direct: Vec<String>,
    }
    impl Prober for Rejector {
        fn connect(&self, _u: &Upstream, h: &str, p: u16) -> ConnectVerdict {
            if self.refused.contains(&format!("{h}:{p}")) {
                ConnectVerdict::Refused { code: 403 }
            } else {
                ConnectVerdict::Open
            }
        }
        fn get(&self, _u: &Upstream, _url: &str) -> Result<HttpProbe, ProbeError> {
            Ok(HttpProbe {
                status: 204,
                body: String::new(),
            })
        }
        fn google_search(&self, _u: &Upstream) -> Result<HttpProbe, ProbeError> {
            Ok(HttpProbe {
                status: 200,
                body: "<html>weather results".into(),
            })
        }
        fn udp_associate(&self, _u: &Upstream) -> Result<bool, ProbeError> {
            Ok(true)
        }
        fn direct_tcp(&self, h: &str, p: u16) -> bool {
            self.direct.contains(&format!("{h}:{p}"))
        }
    }
    /// 给 `spawn_blocking` 的线程池一点**真实**时间。`start_paused` 只冻结虚拟时钟，
    /// 而 `journal_loop` / `daily_loop` 每轮都要等若干个阻塞任务（journalctl、探测、
    /// 写盘）跑完；光 `yield_now` 不消耗真实时间，等不到它们。
    async fn breathe() {
        tokio::task::yield_now().await;
        std::thread::sleep(std::time::Duration::from_millis(1));
        tokio::task::yield_now().await;
    }

    /// 归因用的 relay Clash API：池 selector 的 `now` 播种在 `slot-0-pool` 上
    fn clash_at(tag: &str) -> Arc<dyn Clash> {
        let c = FakeClash::new(None);
        c.with(|i| {
            i.now
                .insert(crate::modules::residential::POOL.into(), tag.into());
            for idx in 0..bui_schema::slots::MAX_SLOTS {
                i.now
                    .insert(crate::modules::residential::slot_selector(idx), tag.into());
            }
        });
        Arc::new(c)
    }

    /// 成员形状的行用不到 Clash（路径 A/成员 tag 直接换算）
    fn no_clash() -> Arc<dyn Clash> {
        Arc::new(FakeClash::new(None))
    }

    fn rejector(refused: &[&str], direct: &[&str]) -> Arc<dyn Prober> {
        Arc::new(Rejector {
            refused: refused.iter().map(|s| s.to_string()).collect(),
            direct: direct.iter().map(|s| s.to_string()).collect(),
        })
    }

    #[test]
    fn bare_ip_targets_never_become_rules() {
        // spec §5.4：端口类拒绝由 ports_allowed 表达，不进黑名单
        assert!(is_bare_ip("198.51.100.9"));
        assert!(is_bare_ip("2001:db8::1"));
        assert!(!is_bare_ip("gateway.icloud.com"));
        assert_eq!(
            candidate_key(Uuid::nil(), "a.com", 443),
            format!("{}|a.com|443", Uuid::nil())
        );
    }

    /// 第二个上游：路径 A 的 `dial tcp` 地址要能唯一命中它（第一个上游是域名 host）
    async fn add_second_upstream(c: &DaemonCtx) {
        rstate::update_group(&c.store, &c.bus, |g| {
            g.upstreams.push(Upstream {
                id: Uuid::from_u128(2),
                name: "url-2".into(),
                kind: UpstreamKind::Socks5,
                host: "203.0.113.7".into(),
                port: 10007,
                username: "user2".into(),
                password: "pw2".into(),
                priority: 20,
                provider: None,
                region: None,
                ports_allowed: None,
                verified: None,
            });
        })
        .await
        .unwrap();
    }

    /// 一行 `journalctl -o json`（`collect` 现在读的就是这个格式：归因的时间戳门要每行
    /// 自己的时刻）
    fn json_line(at: time::OffsetDateTime, n: usize, message: &str) -> String {
        serde_json::json!({
            "__CURSOR": format!("c-{n}"),
            "__REALTIME_TIMESTAMP": (at.unix_timestamp() * 1_000_000).to_string(),
            "_SYSTEMD_UNIT": format!("{}.service", crate::modules::residential::JOURNAL_UNIT),
            "MESSAGE": message,
        })
        .to_string()
    }

    /// 把一段日志（一行一条原文）按 `at` 打上时刻喂给 `learn_from_journal`
    ///（游标每次都新，免得被 scripted 前缀匹配吃掉）
    fn feed_at(host: &FakeHost, at: time::OffsetDateTime, body: &str) {
        let out: String = body
            .lines()
            .enumerate()
            .map(|(n, l)| json_line(at, n, l) + "\n")
            .collect();
        host.with(|i| {
            // `scripted` 是前缀匹配、先到先得：换一批日志要先清掉上一批
            i.scripted.clear();
            i.scripted.push((
                format!(
                    "journalctl -u {} --no-pager",
                    crate::modules::residential::JOURNAL_UNIT
                ),
                CmdOut::success(&format!("{out}-- cursor: s=1\n")),
            ));
        });
    }

    /// 按假机器的**当前**时刻喂日志（没有切换、时间戳门不参与的用例都用它）
    fn feed(host: &FakeHost, body: &str) {
        feed_at(host, host.now(), body);
    }

    /// **本次修法的核心**：生产的 relay 日志只打池 tag，成员 tag 一个字都不出现
    /// （RESEARCH §2：两台真机 2026-09-15 起成员形状 0 条）。池形态的拒绝行要经
    /// Clash API 的 `now`（路径 B）归因到上游，否则黑名单一条都学不到。
    #[tokio::test]
    async fn pool_shaped_rejections_are_attributed_through_the_clash_now() {
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d).await;
        add_second_upstream(&c).await;
        feed(
            &host,
            &format!(
                "{}\n{}",
                fx::POOL_SELECTOR_SOCKS_CODE2,
                fx::POOL_URLTEST_SOCKS_CODE2
            ),
        );
        // 两个池此刻都选中 resi-2（= 第二个上游）
        assert_eq!(learn_from_journal(&c, clash_at("resi-2")).await.unwrap(), 2);
        let r = rstate::read(&c.runtime).await;
        let key = candidate_key(Uuid::from_u128(2), "www.example.com", 443);
        assert_eq!(r.candidates[&key].hits, 2, "两条都记到池此刻选中的那个上游");
        assert_eq!(r.candidates.len(), 1);
    }

    /// 归不了因（池的 `now` 读不到、成员 tag 越界、tag 不是已知池名）一律丢弃，**绝不猜**
    #[tokio::test]
    async fn unattributable_lines_are_dropped_never_guessed() {
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d).await;
        feed(
            &host,
            &format!(
                "{}\n{}\n{}",
                fx::POOL_SELECTOR_SOCKS_CODE2, // relay 没起 ⇒ now 读不到
                fx::POOL_TAG_OUT_OF_RANGE,     // 槽序号越界，不是已知池名
                fx::GATE_SELECTOR              // 住宅 HY2 的门，不是 relay 的池
            ),
        );
        assert_eq!(learn_from_journal(&c, no_clash()).await.unwrap(), 0);
        assert!(rstate::read(&c.runtime).await.candidates.is_empty());
    }

    /// **对抗**：高频借用 / 巡检切换期间灌大量日志。真实暴露窗口是「日志行产生 → 归因读
    /// `now`」整段（这条链的轮询间隔是 `JOURNAL_POLL_SECS` = 300 秒），切换落在其中就会把
    /// 老成员的失败记到新成员头上。b-ui 是两级池唯一的切换者，所以按**时间戳门**判：
    /// 切换时刻之前（含 `SWITCH_ATTRIB_GRACE_SECS` 宽限）产生的路径 B 行整批放弃，
    /// 门之后的行照常学。
    #[tokio::test]
    async fn lines_logged_before_a_pool_switch_are_dropped_and_later_ones_are_learned() {
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d).await;
        add_second_upstream(&c).await;
        // 池在 t0 被切到 resi-2（写入口 = 生产那条：`Clash::select` 成功后记时刻）
        let t0 = host.now() + time::Duration::seconds(100);
        rstate::mark_pool_switch(&c.runtime, "slot-3-pool", t0).await;
        let flood: String = std::iter::repeat_n(fx::POOL_SELECTOR_SOCKS_CODE2, 200)
            .collect::<Vec<_>>()
            .join("\n");
        feed_at(&host, t0 - time::Duration::seconds(5), &flood);
        assert_eq!(
            learn_from_journal(&c, clash_at("resi-2")).await.unwrap(),
            0,
            "这 200 条产生在切换之前，`now` 说的却是切换之后 ⇒ 整批放弃"
        );
        assert!(
            rstate::read(&c.runtime).await.candidates.is_empty(),
            "黑名单候选表零新增"
        );
        // 宽限窗（1 秒）之后的行照常归到池此刻选中的那个上游
        feed_at(
            &host,
            t0 + time::Duration::seconds(1),
            fx::POOL_SELECTOR_SOCKS_CODE2,
        );
        assert_eq!(learn_from_journal(&c, clash_at("resi-2")).await.unwrap(), 1);
        assert!(rstate::read(&c.runtime)
            .await
            .candidates
            .contains_key(&candidate_key(Uuid::from_u128(2), "www.example.com", 443)));
    }

    /// **`dial tcp` 开头的 reason 不进黑名单候选**：它说的是「连不上上游自己」（上游级，
    /// 哨兵的地盘），不是「上游拒绝了这个域名」。兜底关键词表里的 `refused` 会命中
    /// `connect: connection refused`，所以要按前缀整个排掉
    #[tokio::test]
    async fn a_failed_dial_to_the_upstream_never_becomes_a_candidate() {
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d).await;
        add_second_upstream(&c).await;
        feed(
            &host,
            &format!(
                "{}\n{}\n{}",
                fx::POOL_SELECTOR_DIAL_REFUSED,
                fx::MEMBER_DIAL_REFUSED,
                fx::POOL_URLTEST_DIAL_TIMEOUT
            ),
        );
        assert_eq!(learn_from_journal(&c, clash_at("resi-1")).await.unwrap(), 0);
        assert!(rstate::read(&c.runtime).await.candidates.is_empty());
        // 同一条池上的**目标级**拒绝照常学：排掉的只是 `dial tcp` 这个前缀
        feed(&host, fx::POOL_SELECTOR_SOCKS_CODE2);
        assert_eq!(learn_from_journal(&c, clash_at("resi-1")).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn journal_learning_counts_per_upstream_host_port_and_drops_bare_ips() {
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d).await;
        feed(&host, &format!("{HTTP_403}\n{HTTP_403}\n{IP_403}"));
        assert_eq!(
            learn_from_journal(&c, no_clash()).await.unwrap(),
            2,
            "两条域名拒绝（裸 IP 被丢）"
        );
        let r = rstate::read(&c.runtime).await;
        let key = candidate_key(Uuid::from_u128(1), "gateway.icloud.com", 443);
        assert_eq!(r.candidates[&key].hits, 2);
        assert_eq!(r.candidates.len(), 1, "裸 IP 目标不进候选");
        assert_eq!(r.journal_cursor.as_deref(), Some("s=1"));
    }

    #[tokio::test]
    async fn a_refused_non_whitelisted_port_never_becomes_a_domain_rule() {
        // 裁决「端口类拒绝不进域名黑名单」：只有 port ∈ ports_allowed（None ⇒ [80,443]）
        // 的拒绝才计候选。5228 上的 403 是端口策略（ports_allowed 已表达），把
        // courier.push.apple.com 整域拉黑等于把该上游能代理的流量也推回直连。
        const PUSH_403: &str = "open connection to courier.push.apple.com:5228 using outbound/http[resi-1]: unexpected status: 403 Forbidden";
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d).await;
        feed(
            &host,
            &format!("{PUSH_403}\n{PUSH_403}\n{PUSH_403}\n{HTTP_403}"),
        );
        // ports_allowed 还没学到（None）⇒ 按 BASE_PORTS = [80, 443] 看待
        assert!(rstate::group_of(&*c.store.read().await).upstreams[0]
            .ports_allowed
            .is_none());
        assert_eq!(
            learn_from_journal(&c, no_clash()).await.unwrap(),
            1,
            "只有 443 那条计候选"
        );
        let r = rstate::read(&c.runtime).await;
        assert_eq!(r.candidates.len(), 1);
        assert!(r.candidates.contains_key(&candidate_key(
            Uuid::from_u128(1),
            "gateway.icloud.com",
            443
        )));
        // 学到白名单以后同理：5228 不在 [80,443] 里 ⇒ 永远不计候选
        rstate::update_group(&c.store, &c.bus, |g| {
            g.upstreams[0].ports_allowed = Some(vec![80, 443]);
        })
        .await
        .unwrap();
        rstate::update(&c.runtime, |r| {
            r.candidates.clear();
            r.journal_cursor = None;
        })
        .await;
        assert_eq!(learn_from_journal(&c, no_clash()).await.unwrap(), 1);
        assert!(!rstate::read(&c.runtime)
            .await
            .candidates
            .contains_key(&candidate_key(
                Uuid::from_u128(1),
                "courier.push.apple.com",
                5228
            )));
        // 白名单里没有 443 的上游，每日探针集（固定打 443）整条跳过
        rstate::update_group(&c.store, &c.bus, |g| {
            g.upstreams[0].ports_allowed = Some(vec![80]);
        })
        .await
        .unwrap();
        rstate::update(&c.runtime, |r| {
            r.candidates.clear();
            r.journal_cursor = None;
        })
        .await;
        let p = rejector(&["pay.google.com:443"], &["pay.google.com:443"]);
        let rep = daily_round(&c, p, no_clash()).await.unwrap();
        assert_eq!(rep.probed, 0, "443 不在白名单里，探针集不推候选");
        assert!(rstate::read(&c.runtime).await.pending.is_empty());
        assert!(rstate::group_of(&*c.store.read().await)
            .blacklist
            .auto
            .is_empty());
    }

    #[tokio::test]
    async fn only_a_socks5_ruleset_rejection_becomes_a_candidate() {
        // 2026-09-13 真机原文（经 Decodo SOCKS5）：sing-box 把 SOCKS5 REP 写成
        // `socks5: request rejected, code=<REP>`。REP=2（connection not allowed by ruleset）
        // 才是上游的策略拒绝；REP=1（通用失败）/ 4（主机不可达）是目标或网络的问题，
        // 学进黑名单等于把目标自己的故障判成「这个上游代理不了它」
        const CODE2: &str = "connection: open connection to smtp.gmail.com:465 using outbound/socks[resi-1]: socks5: request rejected, code=2";
        const CODE4: &str = "connection: open connection to gateway.push.apple.com:5223 using outbound/socks[resi-1]: socks5: request rejected, code=4";
        const CODE1: &str = "connection: open connection to flaky.example.com:443 using outbound/socks[resi-1]: socks5: request rejected, code=1";
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d).await;
        // 端口都在白名单里：本测试只看 REP 码，不让端口裁决把结论掩盖掉
        rstate::update_group(&c.store, &c.bus, |g| {
            g.upstreams[0].kind = UpstreamKind::Socks5;
            g.upstreams[0].ports_allowed = Some(vec![80, 443, 465, 5223]);
        })
        .await
        .unwrap();
        feed(
            &host,
            &format!("{CODE2}\n{CODE4}\n{CODE4}\n{CODE4}\n{CODE1}\n{CODE1}\n{CODE1}"),
        );
        assert_eq!(
            learn_from_journal(&c, no_clash()).await.unwrap(),
            1,
            "只有 code=2 那条计候选"
        );
        let r = rstate::read(&c.runtime).await;
        assert_eq!(
            r.candidates.keys().cloned().collect::<Vec<_>>(),
            vec![candidate_key(Uuid::from_u128(1), "smtp.gmail.com", 465)],
            "code=4 / code=1 不进候选"
        );
    }

    #[tokio::test]
    async fn a_missing_journalctl_only_alerts() {
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d).await;
        host.with(|i| {
            i.which.remove("journalctl");
        });
        assert_eq!(learn_from_journal(&c, no_clash()).await.unwrap(), 0);
        assert!(rstate::read(&c.runtime)
            .await
            .alerts
            .iter()
            .any(|a| a.contains("journalctl")));
    }

    #[tokio::test]
    async fn confirmation_needs_two_hard_rejects_ten_minutes_apart_plus_a_direct_hit() {
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d).await;
        let key = candidate_key(Uuid::from_u128(1), "gateway.icloud.com", 443);
        rstate::update(&c.runtime, |r| {
            r.candidates.insert(
                key.clone(),
                rstate::Candidate {
                    upstream_id: Uuid::from_u128(1),
                    host: "gateway.icloud.com".into(),
                    port: 443,
                    hits: CANDIDATE_THRESHOLD,
                    first_seen: crate::util::fmt_rfc3339(host.now()),
                    last_seen: crate::util::fmt_rfc3339(host.now()),
                    confirms: 0,
                    last_confirm_at: None,
                },
            );
        })
        .await;
        let p = rejector(&["gateway.icloud.com:443"], &["gateway.icloud.com:443"]);
        assert_eq!(
            confirm_round(&c, p.clone()).await.unwrap(),
            0,
            "第 1 次确认还不进 pending"
        );
        let r1 = rstate::read(&c.runtime).await;
        assert_eq!(
            r1.pending.len(),
            0,
            "确认进度记在候选上，不是一确认就进 pending"
        );
        assert_eq!(r1.candidates[&key].confirms, 1);
        // 10 分钟内的第二次不算（防同一轮抖动连中两次）
        host.advance(300);
        assert_eq!(confirm_round(&c, p.clone()).await.unwrap(), 0);
        assert_eq!(
            rstate::read(&c.runtime).await.candidates[&key].confirms,
            1,
            "间隔不够就不该被算作一次确认"
        );
        host.advance(301);
        assert_eq!(
            confirm_round(&c, p.clone()).await.unwrap(),
            1,
            "间隔 ≥10 分钟的第 2 次才进 pending"
        );
        let r = rstate::read(&c.runtime).await;
        assert_eq!(r.pending[0].host, "gateway.icloud.com");
        assert_eq!(r.pending[0].confirms, CONFIRM_NEEDED);
        assert!(!r.candidates.contains_key(&key), "进了 pending 就出候选池");
        // pending 不生效：state 还没动 ⇒ relay 不重启
        assert!(rstate::group_of(&*c.store.read().await)
            .blacklist
            .auto
            .is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn the_journal_loop_learns_then_confirms_so_ten_minutes_are_enough() {
        // 裁决「黑名单确认节奏」：同一个 JOURNAL_POLL_SECS 轮次里先 learn 再 confirm，
        // 所以两次确认（≥ CONFIRM_MIN_GAP_SECS）之后候选就进 pending——不再是「每天
        // 04:00 才确认一次 ⇒ 一条规则要两天」。
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d).await;
        // scripted 是前缀匹配：--since -25h 与 --after-cursor 两种形态共用这一条
        feed(&host, &format!("{HTTP_403}\n{HTTP_403}\n{HTTP_403}"));
        let p = rejector(&["gateway.icloud.com:443"], &["gateway.icloud.com:443"]);
        let key = candidate_key(Uuid::from_u128(1), "gateway.icloud.com", 443);
        let task = tokio::spawn(journal_loop(c.clone(), p, no_clash()));
        // 第 1 轮（`interval` 首 tick 立即触发）：3 条日志攒满阈值，同一轮里确认第 1 次
        for _ in 0..500 {
            if rstate::read(&c.runtime)
                .await
                .candidates
                .get(&key)
                .map(|x| x.confirms)
                == Some(1)
            {
                break;
            }
            breathe().await;
        }
        let r1 = rstate::read(&c.runtime).await;
        // 本轮刚攒满阈值就在同一轮里被确认了一次 ⇒ learn 一定排在 confirm 之前
        // （`start_paused` 的自动推进可能又跑了几轮，所以只断言 ≥ 阈值）
        assert!(r1.candidates[&key].hits >= CANDIDATE_THRESHOLD);
        assert_eq!(
            r1.candidates[&key].confirms, 1,
            "间隔没到，后续轮次不加第 2 次"
        );
        assert!(r1.pending.is_empty(), "一轮不够");
        // 往前推两轮：t=300 那轮间隔不够（<10 分钟）不算，t=600 那轮才是第 2 次确认
        host.advance(JOURNAL_POLL_SECS as i64 * 2);
        tokio::time::advance(std::time::Duration::from_secs(JOURNAL_POLL_SECS * 2 + 1)).await;
        for _ in 0..500 {
            if !rstate::read(&c.runtime).await.pending.is_empty() {
                break;
            }
            breathe().await;
        }
        let r2 = rstate::read(&c.runtime).await;
        assert_eq!(r2.pending.len(), 1, "≥10 分钟的两次确认就够，不必等 04:00");
        assert_eq!(r2.pending[0].host, "gateway.icloud.com");
        assert_eq!(r2.pending[0].confirms, CONFIRM_NEEDED);
        // 「进了 pending 就出候选池」由 confirmation_needs_two_hard_rejects… 断言：
        // 这里 loop 还在跑，同一条会被后续轮次重新学成候选，不能在这里断言它不存在
        // pending 不生效：state 一个字没动 ⇒ relay 不重启，写入等 04:00 或「立即应用」
        assert!(rstate::group_of(&*c.store.read().await)
            .blacklist
            .auto
            .is_empty());
        task.abort();
    }

    #[tokio::test]
    async fn a_target_that_is_also_unreachable_directly_is_never_blacklisted() {
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d).await;
        rstate::update(&c.runtime, |r| {
            r.candidates.insert(
                candidate_key(Uuid::from_u128(1), "dead.example.com", 443),
                rstate::Candidate {
                    upstream_id: Uuid::from_u128(1),
                    host: "dead.example.com".into(),
                    port: 443,
                    hits: CANDIDATE_THRESHOLD,
                    first_seen: crate::util::fmt_rfc3339(host.now()),
                    last_seen: crate::util::fmt_rfc3339(host.now()),
                    confirms: 0,
                    last_confirm_at: None,
                },
            );
        })
        .await;
        // 上游拒 + 直连也不通 ⇒ 不是代理的错（R13 §5.3），confirm_once 返回 None：
        // 既不累计确认，也不当成「通了」去累计复核 pass
        let p = rejector(&["dead.example.com:443"], &[]);
        confirm_round(&c, p.clone()).await.unwrap();
        host.advance(601);
        assert_eq!(confirm_round(&c, p).await.unwrap(), 0);
        let r = rstate::read(&c.runtime).await;
        assert!(r.pending.is_empty());
        assert_eq!(
            r.candidates[&candidate_key(Uuid::from_u128(1), "dead.example.com", 443)].confirms,
            0,
            "未知不推进确认"
        );
    }

    #[test]
    fn port_learn_only_rewrites_the_whitelist_after_a_clean_round() {
        let p = |pairs: &[(u16, &str)]| -> BTreeMap<u16, String> {
            pairs.iter().map(|(k, v)| (*k, v.to_string())).collect()
        };
        // 调研 §D 的 Decodo 形态：只放行 80/443
        let decodo = p(&[
            (22, "refused:403"),
            (80, "open"),
            (443, "open"),
            (853, "refused:403"),
            (993, "refused:403"),
            (5223, "refused:403"),
            (5228, "refused:403"),
            (8080, "refused:403"),
        ]);
        assert_eq!(
            port_learn(&decodo, false),
            PortLearn::Learned(Some(vec![80, 443]))
        );
        // 407 / 抖动 / 空表 ⇒ 保留上次学到的白名单（改成「不限」会静默放开端口策略）
        assert_eq!(port_learn(&decodo, true), PortLearn::Keep);
        let mut flaky = decodo.clone();
        flaky.insert(993, "unreachable".into());
        assert_eq!(port_learn(&flaky, false), PortLearn::Keep);
        let mut bad_auth = decodo.clone();
        bad_auth.insert(993, "auth_failed".into());
        assert_eq!(port_learn(&bad_auth, false), PortLearn::Keep);
        assert_eq!(port_learn(&BTreeMap::new(), false), PortLearn::Keep);
        // 全通 ⇒ 不限
        let all_open: BTreeMap<u16, String> = [22u16, 80, 443, 853, 993, 5223, 5228, 8080]
            .iter()
            .map(|k| (*k, "open".into()))
            .collect();
        assert_eq!(port_learn(&all_open, false), PortLearn::Learned(None));
    }

    #[tokio::test]
    async fn a_failing_journalctl_resets_the_cursor_so_the_next_round_rescans() {
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d).await;
        rstate::update(&c.runtime, |r| r.journal_cursor = Some("s=stale".into())).await;
        host.with(|i| {
            i.scripted.push((
                format!(
                    "journalctl -u {} --no-pager -o json --show-cursor --after-cursor s=stale",
                    crate::modules::residential::JOURNAL_UNIT
                ),
                CmdOut::failure(1, "Failed to seek to cursor: Invalid argument"),
            ));
        });
        assert_eq!(learn_from_journal(&c, no_clash()).await.unwrap(), 0);
        assert_eq!(
            rstate::read(&c.runtime).await.journal_cursor,
            None,
            "游标失效就丢掉，下一轮用 --since -25h 重扫（T5 的 collect 对非零退出返回 Err）"
        );
    }

    #[tokio::test]
    async fn the_daily_round_applies_pending_reviews_autos_and_persists_the_selection() {
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d).await;
        // 一条待生效、一条已生效且已连过两次复核的 auto
        rstate::update(&c.runtime, |r| {
            r.pending.push(rstate::PendingEntry {
                upstream_id: Uuid::from_u128(1),
                host: "gateway.icloud.com".into(),
                port: 443,
                hits: 9,
                confirms: CONFIRM_NEEDED,
                last_confirm_at: crate::util::fmt_rfc3339(host.now()),
            });
            // runtime 记的是 uuid（契约决策 §C），04:00 写回不需要任何 tag 换算
            r.selected_upstream_id = Some(Uuid::from_u128(1));
            r.selected_pending_persist = true;
        })
        .await;
        rstate::update_group(&c.store, &c.bus, |g| {
            g.selected_upstream_id = None; // 与 runtime 的「当前生效」漂移
            g.blacklist.auto.push(AutoEntry {
                upstream_id: Uuid::from_u128(1),
                rule: Rule::DomainSuffix("x.com".into()),
                hits: 3,
                confirmed_at: "2026-09-10T00:00:00Z".into(),
                last_verified_at: "2026-09-11T00:00:00Z".into(),
                passes: REVIEW_PASSES_TO_REMOVE - 1,
            });
        })
        .await
        .unwrap();
        // x.com 现在通了（第 3 次 pass ⇒ 移除）；探针集全通
        let p = rejector(&[], &[]);
        let rep = daily_round(&c, p, no_clash()).await.unwrap();
        assert_eq!(rep.applied, 1);
        assert_eq!(rep.removed, 1);
        assert_eq!(rep.ports_learned, 1, "spec §5.4 (b)：端口集也每日重探一遍");
        assert!(rep.persisted_selection);
        let g = rstate::group_of(&*c.store.read().await);
        assert_eq!(
            g.blacklist
                .auto
                .iter()
                .map(|e| e.rule.clone())
                .collect::<Vec<_>>(),
            vec![Rule::DomainSuffix("gateway.icloud.com".into())],
            "规则值就是被拒的完整主机名，不泛化到注册域名"
        );
        assert_eq!(
            g.selected_upstream_id,
            Some(Uuid::from_u128(1)),
            "漂移在 04:00 窗口写回（D4）"
        );
        assert_eq!(
            g.upstreams[0].ports_allowed, None,
            "端口集全通 ⇒ 学成「不限」，同一次写盘落地"
        );
        let r = rstate::read(&c.runtime).await;
        assert!(r.pending.is_empty());
        assert!(!r.selected_pending_persist);
        assert_eq!(
            r.last_daily_at.as_deref(),
            Some(&*crate::util::fmt_rfc3339(host.now()))
        );
    }

    #[tokio::test]
    async fn the_daily_probe_set_seeds_candidates_without_any_log_evidence() {
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d).await;
        host.with(|i| {
            i.scripted.push((
                format!(
                    "journalctl -u {} --no-pager",
                    crate::modules::residential::JOURNAL_UNIT
                ),
                CmdOut::success("-- cursor: s=1\n"),
            ));
        });
        let p = rejector(&["pay.google.com:443"], &["pay.google.com:443"]);
        // 第一轮 04:00：探针集把它推成候选（日志里一条证据都没有也行，spec §5.4 (b)）
        let first = daily_round(&c, p.clone(), no_clash()).await.unwrap();
        assert_eq!(first.probed, 1);
        assert_eq!(
            first.applied, 0,
            "04:00 只批量生效已确认的；确认归 journal_loop"
        );
        let key = candidate_key(Uuid::from_u128(1), "pay.google.com", 443);
        assert_eq!(
            rstate::read(&c.runtime).await.candidates[&key].hits,
            CANDIDATE_THRESHOLD
        );
        assert!(rstate::read(&c.runtime).await.pending.is_empty());
        // journal_loop 的两轮确认（间隔 ≥ 10 分钟）把它升进 pending
        assert_eq!(confirm_round(&c, p.clone()).await.unwrap(), 0);
        host.advance(601);
        assert_eq!(confirm_round(&c, p.clone()).await.unwrap(), 1);
        // 下一个 04:00 窗口只做批量生效
        let second = daily_round(&c, p, no_clash()).await.unwrap();
        assert_eq!(second.applied, 1);
        assert_eq!(
            rstate::group_of(&*c.store.read().await).blacklist.auto[0].rule,
            Rule::DomainSuffix("pay.google.com".into())
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_daily_loop_fires_only_inside_the_window_hour_and_only_once_per_hour() {
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d).await;
        host.with(|i| {
            i.now = time::macros::datetime!(2026-09-12 03:50:00 UTC);
            // scripted 是前缀匹配，一条就够覆盖两种游标形态
            i.scripted.push((
                format!(
                    "journalctl -u {} --no-pager",
                    crate::modules::residential::JOURNAL_UNIT
                ),
                CmdOut::success("-- cursor: s=1\n"),
            ));
        });
        let task = tokio::spawn(daily_loop(c.clone(), rejector(&[], &[]), no_clash()));
        // 03:50 不在窗口里：推两个 tick 也不该跑
        tokio::time::advance(std::time::Duration::from_secs(DAILY_TICK_SECS * 2 + 1)).await;
        for _ in 0..200 {
            breathe().await;
        }
        assert_eq!(
            rstate::read(&c.runtime).await.last_daily_at,
            None,
            "不是 04 点就不跑"
        );
        // 进入窗口
        host.with(|i| i.now = time::macros::datetime!(2026-09-12 04:05:00 UTC));
        tokio::time::advance(std::time::Duration::from_secs(DAILY_TICK_SECS + 1)).await;
        for _ in 0..500 {
            if rstate::read(&c.runtime).await.last_daily_at.is_some() {
                break;
            }
            breathe().await;
        }
        let first = rstate::read(&c.runtime).await.last_daily_at;
        assert!(first.is_some(), "04:05 跑了一轮");
        // 同一小时内再到点也只跑这一次（否则一小时会重启 relay 六次）
        host.with(|i| i.now = time::macros::datetime!(2026-09-12 04:40:00 UTC));
        tokio::time::advance(std::time::Duration::from_secs(DAILY_TICK_SECS + 1)).await;
        for _ in 0..200 {
            breathe().await;
        }
        assert_eq!(
            rstate::read(&c.runtime).await.last_daily_at,
            first,
            "同一小时只跑一次"
        );
        task.abort();
    }

    #[tokio::test]
    async fn pins_and_apply_now_take_effect_immediately() {
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d).await;
        let mut rx = c.bus.subscribe();
        add_pin(
            &c,
            Rule::DomainSuffix("pay.google.com".into()),
            "支付必须直连".into(),
        )
        .await
        .unwrap();
        assert_eq!(
            rstate::group_of(&*c.store.read().await).blacklist.pins[0].rule,
            Rule::DomainSuffix("pay.google.com".into())
        );
        assert_eq!(
            rx.try_recv().unwrap(),
            crate::api::Event::StateChanged("residential"),
            "pin 立刻触发对账 → 重渲染 relay → 重启"
        );
        // 同一条 pin 再加一次是幂等的
        add_pin(&c, Rule::DomainSuffix("pay.google.com".into()), "".into())
            .await
            .unwrap();
        assert_eq!(
            rstate::group_of(&*c.store.read().await)
                .blacklist
                .pins
                .len(),
            1
        );
        assert!(remove_pin(&c, &Rule::DomainSuffix("pay.google.com".into()))
            .await
            .unwrap());
        assert!(
            !remove_pin(&c, &Rule::DomainSuffix("pay.google.com".into()))
                .await
                .unwrap()
        );
        // apply_now：pending → auto
        rstate::update(&c.runtime, |r| {
            r.pending.push(rstate::PendingEntry {
                upstream_id: Uuid::from_u128(1),
                host: "gateway.icloud.com".into(),
                port: 443,
                hits: 3,
                confirms: CONFIRM_NEEDED,
                last_confirm_at: crate::util::fmt_rfc3339(host.now()),
            });
        })
        .await;
        assert_eq!(apply_now(&c).await.unwrap(), 1);
        assert_eq!(
            rstate::group_of(&*c.store.read().await)
                .blacklist
                .auto
                .len(),
            1
        );
        assert!(rstate::read(&c.runtime).await.pending.is_empty());
        // 手动删一条 auto（面板「误伤了」）
        assert!(remove_auto(
            &c,
            Uuid::from_u128(1),
            &Rule::DomainSuffix("gateway.icloud.com".into())
        )
        .await
        .unwrap());
        assert!(rstate::group_of(&*c.store.read().await)
            .blacklist
            .auto
            .is_empty());
    }

    #[tokio::test]
    async fn the_daily_port_probe_uses_a_neutral_base_host_and_the_real_host_per_port() {
        // Bright Data 对 www.google.com 整域 403：基准端口若打它，`port_learn` 永远
        // 只会得到 Keep（一条白名单都学不出来）。基准端口必须打中性主机。
        let up = Upstream {
            id: Uuid::from_u128(1),
            name: "url-1".into(),
            kind: UpstreamKind::Http,
            host: "isp1.example.net".into(),
            port: 10007,
            username: "user1".into(),
            password: "pw1".into(),
            priority: 10,
            provider: None,
            region: None,
            ports_allowed: None,
            verified: None,
        };
        let p = rejector(
            &[
                // 搜索域名整域被拒（端口无关）
                "www.google.com:80",
                "www.google.com:443",
                // 端口策略：白名单外的端口全拒（打的是各自的真实主机）
                "mtalk.google.com:5228",
                "courier.push.apple.com:5223",
                "imap.gmail.com:993",
                "github.com:22",
                "dns.google:853",
                "portquiz.net:8080",
            ],
            &[],
        );
        assert_eq!(
            probe_ports_daily(p, std::slice::from_ref(&up)).await,
            vec![(up.id, PortLearn::Learned(Some(vec![80, 443])))]
        );
    }

    #[tokio::test]
    async fn apply_now_with_nothing_pending_writes_nothing_and_restarts_nothing() {
        // pending 空 ⇒ flush_pending 直接返回：不写 state、不发 StateChanged。
        // 否则面板「立即应用」连点两下就白重启一次 b-ui-relay（唯一掐连接的动作）
        let d = tempfile::tempdir().unwrap();
        let (c, _host) = ctx(&d).await;
        let before = serde_json::to_string(&*c.store.read().await).unwrap();
        let mut rx = c.bus.subscribe();
        assert_eq!(apply_now(&c).await.unwrap(), 0);
        assert!(
            rx.try_recv().is_err(),
            "空 pending 不该触发对账（重渲染 relay + 重启）"
        );
        assert_eq!(
            serde_json::to_string(&*c.store.read().await).unwrap(),
            before,
            "state 一个字节都不该动"
        );
    }
}
