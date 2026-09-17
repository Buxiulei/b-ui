//! 住宅 HY2 的**门位**（每凭据一个 `gate-<id>` selector）收敛（spec §3.3 / §3.4）。
//!
//! 门位就是授权本身：`gate-<id>` 指向 `slot-<i>-out` = 这条凭据现在能出网、走第 `i` 槽的
//! 住宅 IP；指向 `deny`（那条打 `127.0.0.1:1` 的 socks 出站）= 握手照旧成功、每个请求被拒
//! （spec §6 的到期 / 封禁语义）。于是用户的**全部**生命周期动作都只是一次
//! `PUT /proxies/gate-<id>`：`hy2-residential.json` 一个字节都不动、内核不重启
//! （spec §3.5）。
//!
//! 三件事在这个文件里：
//!
//! 1. [`expected`] —— 期望门位的**唯一**口径（纯函数）。面板投影（T14 的 `hy2ResiGate`）
//!    与 CLI 都读它，免得「谁该放行」在三处各写一遍。「谁算住宅 HY2 用户」这半条判据
//!    在 [`bui_schema::hy2pool::is_resi_hy2`]：T15 把 4.0.x 那两套（本文件不看分组存在性
//!    的旧 `has_resi_hy2` / schema 里不看分组的 `is_resi_hy2`）合成了一处，取更严的那个，
//!    池容量、分凭据、门位、面板与踢人从此同源。
//! 2. [`converge`] —— 挂在 [`users::sync_users`](super::users::sync_users) 末尾，与它旁边
//!    那段 xray 收敛**同构**：读一次内核真源（`GET /proxies`）、求差集、只 PUT 不一致的那几个。
//!    触发路径沿用现成的两条（`StateChanged` + 60 秒安全网），不新增定时器。
//! 3. [`replay_after_restart`] —— **不开 `cache_file`** 的代价与地基（spec §14 裁决 1）：
//!    sing-box 重启后每个 selector 回到 `default = "deny"`，全员当场 fail-closed，由 b-ui
//!    按 [`REPLAY_RETRY_BUDGET`] 的退避重放真实门位；预算用完仍有差集 ⇒ 没重放到的门**留在
//!    `deny`**（绝不为了可用性开成放行）+ 记事件与告警，60 秒安全网继续收敛。
//!
//! **只对内核报回来的门下手**：[`converge`] 遍历的是 `GET /proxies` 读回来的门位表，而不是
//! 期望态里的凭据池。池扩容那一瞬（state 已有 48 条凭据、sing-box 还跑着 32 个门）多出来的
//! 那 16 条没有门可 PUT，遍历期望态会每轮刷 16 条假告警，而它们本来就连不上（没有凭据就没有
//! `auth_user` 规则）—— 等对账重写配置、重启单元，[`replay_after_restart`] 一次带齐。
use super::users;
use super::Shared;
use crate::modules::residential::health::REPLAY_RETRY_BUDGET;
use crate::modules::residential::state as resi_state;
use crate::modules::sentinel::incidents::{self, Incident, Level};
use crate::reconcile::DaemonCtx;
use bui_schema::hy2pool::is_resi_hy2;
use bui_schema::model::{State, User};
use bui_schema::render::hy2_singbox::{gate_tag, slot_out_tag, DENY_TAG};
use bui_schema::slots::index_of_user;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;
use uuid::Uuid;

/// 重放退避的首个间隔，此后每次翻倍（0.3/0.6/1.2/2.4/4.8…），最后一段截到
/// [`REPLAY_RETRY_BUDGET`] 用完为止。与 relay 那条重放同值（`residential::health` 里
/// 那个常量是私有的，值在这里重写一份；两边都是「内核刚重启、还没起监听」这同一个场景）。
const REPLAY_RETRY_FIRST: Duration = Duration::from_millis(300);

/// 重放最终失败那条告警的固定前缀：下一次重放全部成功时按它认领、清掉
/// （口径同 `residential::health` 的 `REPLAY_FAIL_ALERT`）。
pub const GATE_REPLAY_FAIL_ALERT: &str = "住宅 HY2 门位重放失败";

/// 重放失败事件的签名（spec §3.4 / §8.1 的 `hy2_resi_gate_replay_failed`）。
/// 这是**非日志**签名：由本模块直接记事件，不经哨兵的日志匹配。
pub const GATE_REPLAY_FAILED_SIG: &str = "hy2_resi_gate_replay_failed";

/// 收敛 / 重放这一轮做了什么。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GateOutcome {
    /// 真正 PUT 成功的门位数（已经对上的不算）
    pub switched: usize,
    /// 失败项的人读原因；进 `SyncOutcome.errors` ⇒ `USER_SYNC_FAILED_LOG`
    pub errors: Vec<String>,
}

/// 期望门位：凭据 `id` → 出站 tag（spec §3.3）。
///
/// 持有人存在、不在 `blocked`（`users::blocked_set`：disabled / 到期 / 超总量 / 超月量）、
/// 且有住宅 hysteria2 权益 ⇒ 他那一槽的出站；**其余一律 `deny`**，含全部空闲凭据。
///
/// `hy2_resi_cred` 指向池里已不存在的 id（人工改 state、回滚后再升级）时那个用户不进表
/// —— 他压根没有门；这种悬空指针由
/// [`slots::migrate_hy2_pool_on_start`](crate::modules::residential::slots::migrate_hy2_pool_on_start)
/// 在启动时清掉并补发。
pub fn expected(s: &State, blocked: &BTreeSet<Uuid>) -> BTreeMap<String, String> {
    let holder: BTreeMap<&str, &User> = s
        .users
        .iter()
        .filter_map(|u| {
            u.credentials
                .hy2_resi_cred
                .as_deref()
                .map(|id| (id, u as &User))
        })
        .collect();
    s.residential
        .hy2_pool
        .creds
        .iter()
        .map(|c| {
            let tag = holder
                .get(c.id.as_str())
                .filter(|u| {
                    !u.disabled && !blocked.contains(&u.user_id) && is_resi_hy2(u, &s.residential)
                })
                .map(|u| slot_out_tag(index_of_user(u, &s.residential)))
                .unwrap_or_else(|| DENY_TAG.to_string());
            (c.id.clone(), tag)
        })
        .collect()
}

/// 一轮门位收敛：读一次 `GET /proxies`，只 PUT 与期望不一致的门（spec §3.3）。
///
/// 遍历的是**内核报回来的**门位表（见模块文档末段）；期望态里没有的门一律按 `deny`
/// 收 —— 凭据被人工从池里删掉时，它的门不许留在某个槽上。
pub async fn converge(ctx: &DaemonCtx, shared: &Shared, blocked: &BTreeSet<Uuid>) -> GateOutcome {
    // **先读内核、后算期望**：两次读之间可能落进一次 rotate / kick（它们写盘之后自己
    // 当场 PUT 两下）。这个顺序下陈旧的那半只会是 `live`，配上更新的 `want` 得出的是
    // 「按新期望再收一次」= fail-closed；反过来（先算 want）会拿旧持有人的期望覆盖掉
    // rotate 刚切成 `deny` 的那扇门，泄露的旧凭据多活一轮（≤60 秒）——
    // 而 rotate 当场两次 PUT 的全部理由就是「不许多活一分钟」。
    let live = match shared.hy2resi().selected_all().await {
        Ok(m) => m,
        Err(e) => {
            return GateOutcome {
                switched: 0,
                errors: vec![format!("住宅 HY2 门位读不到（GET /proxies）：{e}")],
            }
        }
    };
    let want = {
        let s = ctx.store.read().await;
        expected(&s, blocked)
    };
    let mut out = GateOutcome::default();
    for (gate, now) in &live {
        let cred_id = match gate.strip_prefix(gate_tag("").as_str()) {
            Some(id) => id,
            // `parse_proxies_now` 已按 `gate-` 前缀筛过，理论上不可达
            None => continue,
        };
        let tag = want.get(cred_id).map_or(DENY_TAG, String::as_str);
        if now == tag {
            continue;
        }
        match shared.hy2resi().select(gate, tag).await {
            Ok(()) => out.switched += 1,
            Err(e) => out
                .errors
                .push(format!("住宅 HY2 门位 {gate} → {tag} 失败：{e}")),
        }
    }
    if out.switched > 0 {
        tracing::info!(switched = out.switched, "住宅 HY2 门位已收敛");
    }
    out
}

/// 立刻把一条凭据的门 PUT 过去（spec §3.3 的生命周期动作）。
///
/// **best-effort**：失败只记 warn —— 主保障是鉴权快照与下一轮门位收敛。换凭据这类
/// 「立刻生效」的动作不许只靠 60 秒安全网：泄露的旧凭据在那一分钟里照旧能出网。
pub(super) async fn put_gate(shared: &Shared, cred_id: &str, tag: &str) {
    let gate = gate_tag(cred_id);
    if let Err(e) = shared.hy2resi().select(&gate, tag).await {
        tracing::warn!(gate = %gate, tag, error = %e, "住宅 HY2 门位切换失败；门位收敛会补上");
    }
}

/// 住宅入站重启后把门位重放回去（spec §3.4）。
///
/// 每轮先探 Clash API 可用（[`super::Hy2ResiApi::ready`]，sing-box 刚起来时还没监听），
/// 再跑一轮 [`converge`]；仍有差集就按 0.3/0.6/1.2/… 秒退避重试，总时长不超过
/// [`REPLAY_RETRY_BUDGET`]。
///
/// 预算用完仍没收敛 ⇒ **停在 `deny`**（fail-closed，spec §14 裁决 1）+ 记一条 Error 级事件
/// 与一条告警（[`GATE_REPLAY_FAIL_ALERT`]），交给 `sync_users` 的 60 秒安全网继续收敛；
/// 全部成功时认领并清掉以前那条告警。
pub async fn replay_after_restart(ctx: &DaemonCtx, shared: &Shared) -> GateOutcome {
    let now = ctx.host.now();
    let pending = shared.pending().await.clone();
    let blocked = {
        let s = ctx.store.read().await;
        users::blocked_set(&s, &pending, now)
    };
    let deadline = tokio::time::Instant::now() + REPLAY_RETRY_BUDGET;
    let mut delay = REPLAY_RETRY_FIRST;
    let mut out = GateOutcome::default();
    let mut attempts = 0u32;
    loop {
        attempts += 1;
        if shared.hy2resi().ready().await {
            let round = converge(ctx, shared, &blocked).await;
            out.switched += round.switched;
            out.errors = round.errors;
            if out.errors.is_empty() {
                break;
            }
        } else {
            out.errors =
                vec!["Clash API 未就绪（住宅 sing-box 多半刚重启、还没起监听）".to_string()];
        }
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            break;
        }
        let wait = delay.min(left);
        tracing::debug!(wait = ?wait, attempts, "住宅 HY2 门位重放未完成，退避后重试");
        tokio::time::sleep(wait).await;
        delay *= 2;
    }
    let why = out.errors.join("；");
    let alert = (!out.errors.is_empty())
        .then(|| format!("{GATE_REPLAY_FAIL_ALERT}（{why}），门留在 deny，60 秒安全网继续收敛"));
    let incident = alert.as_ref().map(|_| Incident {
        at: crate::util::fmt_rfc3339(now),
        // 动作是守护进程自己做的，不是从内核日志里读出来的
        unit: "b-ui".into(),
        signature: GATE_REPLAY_FAILED_SIG.into(),
        subject: "hysteria-residential".into(),
        action: "重放门位".into(),
        result: format!("{REPLAY_RETRY_BUDGET:?} 内未收敛，门留在 deny：{why}"),
        level: Level::Error,
        sample: None,
    });
    resi_state::update(&ctx.runtime, |r| {
        // 只留最新一次的结论：全部成功 ⇒ 认领以前写下的失败告警
        resi_state::remove_alerts_with_prefix(r, GATE_REPLAY_FAIL_ALERT);
        if let Some(a) = alert {
            resi_state::push_alert(r, a);
        }
    })
    .await;
    if let Some(inc) = incident {
        ctx.runtime.update(|rt| incidents::push(rt, inc)).await;
    }
    if out.errors.is_empty() {
        tracing::info!(
            switched = out.switched,
            attempts,
            "住宅入站重启后已重放门位"
        );
    } else {
        tracing::warn!(
            switched = out.switched,
            attempts,
            reason = %why,
            "住宅入站重启后门位重放失败，门留在 deny"
        );
    }
    out
}

/// 订阅 `EventBus`：`Event::Hy2ResiRestarted` 一到就重放门位。
///
/// **`rx` 由调用方先 `bus.subscribe()` 拿到再传进来**：broadcast 丢弃「发送时还没有订阅者」
/// 的事件，在本函数里 subscribe 会丢掉调用方 spawn 之后立刻 send 的那一条
/// （口径同 `residential::health::replay_loop`）。
pub async fn replay_loop(
    ctx: DaemonCtx,
    shared: std::sync::Arc<Shared>,
    mut rx: tokio::sync::broadcast::Receiver<crate::api::Event>,
) {
    loop {
        match rx.recv().await {
            Ok(crate::api::Event::Hy2ResiRestarted) => {
                replay_after_restart(&ctx, &shared).await;
            }
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::panel::testsupport::{harness, Harness};
    use crate::testutil::sample_state;
    use bui_schema::model::Protocol;
    use pretty_assertions::assert_eq;
    use time::macros::datetime;

    fn t0() -> time::OffsetDateTime {
        datetime!(2026-09-11 00:00:00 UTC)
    }

    fn ctx_of(h: &Harness) -> DaemonCtx {
        DaemonCtx {
            store: h.store.clone(),
            runtime: h.runtime.clone(),
            bus: h.app.bus.clone(),
            host: h.host.clone(),
            paths: h.paths.clone(),
        }
    }

    /// 3 槽 + 三个用户：alice 粘槽 1、bob 粘槽 2、carol 已到期（槽 0）。
    /// 凭据池按 `migrate` 的迁移口径生成 ⇒ alice=r000 / bob=r001 / carol=r002。
    fn three_slot_state_with_pool() -> State {
        let mut s = sample_state();
        let g = s
            .residential
            .groups
            .get_mut(bui_schema::model::DEFAULT_GROUP)
            .unwrap();
        g.enabled = true;
        g.upstreams = (0..3u16)
            .map(|i| bui_schema::model::Upstream {
                id: Uuid::from_u128(u128::from(i) + 1),
                name: format!("url-{}", i + 1),
                kind: bui_schema::model::UpstreamKind::Socks5,
                host: format!("isp{}.example.net", i + 1),
                port: 10007,
                username: "user1".into(),
                password: "pw1".into(),
                priority: 100,
                provider: None,
                region: None,
                ports_allowed: None,
                verified: None,
            })
            .collect();
        s.residential.slots = (0..3u16)
            .map(|i| bui_schema::model::Slot {
                index: i,
                upstream_id: Uuid::from_u128(u128::from(i) + 1),
            })
            .collect();
        // alice（样例态里就有）→ 槽 1
        s.users[0].created_at = "2026-09-11T00:00:00Z".into();
        s.users[0]
            .entitlements
            .residential
            .as_mut()
            .unwrap()
            .slot_id = Some(Uuid::from_u128(2));
        let mut bob = s.users[0].clone();
        bob.user_id = Uuid::from_u128(0xb0b);
        bob.username = "bob".into();
        bob.created_at = "2026-09-11T00:01:00Z".into();
        bob.credentials.hy2_password = "pw-bob-01".into();
        bob.entitlements.residential.as_mut().unwrap().slot_id = Some(Uuid::from_u128(3));
        let mut carol = s.users[0].clone();
        carol.user_id = Uuid::from_u128(0xca7);
        carol.username = "carol".into();
        carol.created_at = "2026-09-11T00:02:00Z".into();
        carol.credentials.hy2_password = "pw-carol-01".into();
        carol.entitlements.residential.as_mut().unwrap().slot_id = Some(Uuid::from_u128(1));
        carol.entitlements.expires_at = Some("2026-09-10T00:00:00Z".into()); // 已到期
        s.users.push(bob);
        s.users.push(carol);
        bui_schema::hy2pool::migrate(&mut s, t0());
        s
    }

    async fn harness_with_pool() -> Harness {
        let h = harness().await;
        let s = three_slot_state_with_pool();
        h.store.update(|st| *st = s.clone()).await.unwrap();
        h
    }

    fn cred_id_of(s: &State, username: &str) -> String {
        s.users
            .iter()
            .find(|u| u.username == username)
            .and_then(|u| u.credentials.hy2_resi_cred.clone())
            .expect("迁移后每个住宅 hysteria2 用户都有凭据")
    }

    /// 期望门位：有权益且未被封 ⇒ 本槽出站；其余（含全部空闲凭据）⇒ deny
    #[test]
    fn expected_gates_map_live_users_to_their_slot_and_everyone_else_to_deny() {
        let s = three_slot_state_with_pool();
        let blocked = users::blocked_set(&s, &Default::default(), t0());
        let e = expected(&s, &blocked);
        assert_eq!(e["r000"], "slot-1-out", "alice 粘在槽 1");
        assert_eq!(e["r001"], "slot-2-out", "bob 粘在槽 2");
        assert_eq!(
            e["r002"], "deny",
            "carol 已到期：握手仍成功、流全拒（spec §6）"
        );
        for c in &s.residential.hy2_pool.creds {
            if ["r000", "r001", "r002"].contains(&c.id.as_str()) {
                continue;
            }
            assert_eq!(e[&c.id], "deny", "空闲凭据一律停在 deny：{}", c.id);
        }
        assert_eq!(
            e.len(),
            s.residential.hy2_pool.creds.len(),
            "每个凭据都有期望门位"
        );
    }

    /// 撤掉住宅 hysteria2 权益、或人工禁用 ⇒ 门位回 deny（不看 blocked 也成立）
    #[test]
    fn dropping_the_entitlement_or_disabling_closes_the_gate() {
        let mut s = three_slot_state_with_pool();
        let alice = cred_id_of(&s, "alice");
        s.users[0].entitlements.protocols = vec![Protocol::Reality];
        assert_eq!(expected(&s, &Default::default())[&alice], "deny");
        s.users[0].entitlements.protocols = vec![Protocol::Hysteria2];
        s.users[0].disabled = true;
        assert_eq!(expected(&s, &Default::default())[&alice], "deny");
    }

    /// `group_id` 悬空（人工改 `state.json`、分组被删）⇒ 门必须 `deny`。
    /// `nodes_for` 的 `resi_ok` 多一条 `resi.groups.contains_key(&r.group_id)`，这时订阅里
    /// 已经不发住宅 HY2 节点了；门是授权落点，少了这条判据就会留在某一槽上，
    /// 拿着旧订阅的人继续从住宅 IP 出海。
    #[test]
    fn a_dangling_group_id_closes_the_gate() {
        let mut s = three_slot_state_with_pool();
        let alice = cred_id_of(&s, "alice");
        assert_eq!(
            expected(&s, &Default::default())[&alice],
            "slot-1-out",
            "前提：分组存在时门开在他自己那一槽"
        );
        s.users[0]
            .entitlements
            .residential
            .as_mut()
            .unwrap()
            .group_id = "gone".into();
        assert_eq!(
            expected(&s, &Default::default())[&alice],
            "deny",
            "分组不存在 ⇒ 门必须关"
        );
        assert!(
            !bui_schema::nodes::nodes_for(&s.users[0], &s.node, &s.residential)
                .iter()
                .any(|n| n.kind == bui_schema::nodes::NodeKind::Hy2Residential),
            "口径必须与 nodes_for 的 resi_ok 一致（取两者里更严的那个）"
        );
    }

    /// 启动那一轮对账广播的 `Event::Hy2ResiRestarted` **必然被丢弃**：`EventBus` 是裸
    /// `broadcast::Sender`（`send` 吞 Err），而 `gates::replay_loop` 要到 `Module::spawn`
    /// 才订阅、`spawn` 又晚于启动对账。所以 `serve::run` 在启动路径上自己调一次
    /// [`replay_after_restart`]（判据是 `serve::hy2_resi_restarted`）—— 少了它，升级 / 首装 /
    /// 任何改了住宅配置的重启之后，全体住宅 HY2 用户都停在 `deny`（握手成功、每个请求被拒），
    /// 只能等 `sync_loop` 起来时那一次没有 `ready()` 探测的 `sync_now` 去碰运气。
    #[tokio::test]
    async fn the_restart_event_is_lost_before_anyone_subscribes_so_startup_replays_directly() {
        let h = harness_with_pool().await;
        h.hy2resi.set_selected(BTreeMap::from([(
            "gate-r000".to_string(),
            "deny".to_string(),
        )]));
        let ctx = ctx_of(&h);
        // ① 启动对账重启住宅入站后广播 —— 此刻总线上一个订阅者都没有
        ctx.bus.send(crate::api::Event::Hy2ResiRestarted);
        // ② 后台任务这时才起来（口径同 `PanelModule::spawn`：先 subscribe 再 spawn）
        let rx = ctx.bus.subscribe();
        let task = tokio::spawn(replay_loop(ctx.clone(), h.shared.clone(), rx));
        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            !h.hy2resi.calls().iter().any(|c| c.starts_with("select:")),
            "前提：spawn 之前发出的那一条已经丢了，没有任何重放：{:?}",
            h.hy2resi.calls()
        );
        // ③ 所以启动路径必须自己补一次
        let out = replay_after_restart(&ctx, &h.shared).await;
        task.abort();
        assert_eq!(out.switched, 1);
        assert!(
            h.hy2resi
                .calls()
                .contains(&"select:gate-r000:slot-1-out".to_string()),
            "{:?}",
            h.hy2resi.calls()
        );
    }

    /// 收敛只对差集下手：读一次 /proxies，然后只 PUT 不一致的那几个
    #[tokio::test]
    async fn convergence_only_switches_the_difference() {
        let h = harness_with_pool().await;
        h.hy2resi.set_selected(BTreeMap::from([
            ("gate-r000".to_string(), "slot-1-out".to_string()), // 已对
            ("gate-r001".to_string(), "deny".to_string()),       // 要切到 slot-2-out
        ]));
        let out = converge(&ctx_of(&h), &h.shared, &Default::default()).await;
        assert_eq!(out.switched, 1);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let calls = h.hy2resi.calls();
        assert_eq!(
            calls.iter().filter(|c| c.starts_with("select:")).count(),
            1,
            "{calls:?}"
        );
        assert!(calls.contains(&"select:gate-r001:slot-2-out".to_string()));
        assert_eq!(
            calls.iter().filter(|c| *c == "selected_all").count(),
            1,
            "一次 GET /proxies 读全部"
        );
    }

    /// 期望态里没有的门按 deny 收（凭据被人工从池里删掉时，门不许留在某个槽上）
    #[tokio::test]
    async fn a_gate_with_no_cred_behind_it_is_closed() {
        let h = harness_with_pool().await;
        h.hy2resi.set_selected(BTreeMap::from([(
            "gate-r250".to_string(),
            "slot-1-out".to_string(),
        )]));
        let out = converge(&ctx_of(&h), &h.shared, &Default::default()).await;
        assert_eq!(out.switched, 1);
        assert!(h
            .hy2resi
            .calls()
            .contains(&"select:gate-r250:deny".to_string()));
    }

    /// `GET /proxies` 读不到 ⇒ 一条 error，不 PUT 任何门（宁可什么都不动）
    #[tokio::test]
    async fn an_unreadable_proxies_endpoint_switches_nothing() {
        let h = harness_with_pool().await;
        h.hy2resi.fail_next("Clash API 不可达：connection refused");
        let out = converge(&ctx_of(&h), &h.shared, &Default::default()).await;
        assert_eq!(out.switched, 0);
        assert_eq!(out.errors.len(), 1);
        assert!(
            out.errors[0].contains("Clash API 不可达"),
            "{:?}",
            out.errors
        );
        assert!(!h.hy2resi.calls().iter().any(|c| c.starts_with("select:")));
    }

    /// PUT 失败 ⇒ 进 `SyncOutcome.errors`（`report()` 据此打 `USER_SYNC_FAILED_LOG`，
    /// 哨兵按那行认 `hy2_resi_gate_sync_failed`）
    #[tokio::test]
    async fn a_failed_put_is_reported_so_the_sentinel_can_see_it() {
        let h = harness_with_pool().await;
        h.hy2resi.set_selected(BTreeMap::from([(
            "gate-r000".to_string(),
            "deny".to_string(),
        )]));
        h.hy2resi.with(|i| {
            i.fail_on.insert("select:gate-r000:slot-1-out".into());
        });
        let out = users::sync_users(&ctx_of(&h), &h.shared, &Default::default()).await;
        assert!(
            out.errors.iter().any(|e| e.contains("gate-r000")),
            "{:?}",
            out.errors
        );
    }

    /// 门位收敛跟着 `sync_users` 的两条现成触发路径走（不新增定时器）
    #[tokio::test]
    async fn sync_users_converges_the_gates() {
        let h = harness_with_pool().await;
        h.hy2resi.set_selected(BTreeMap::from([
            ("gate-r000".to_string(), "deny".to_string()),
            ("gate-r002".to_string(), "slot-0-out".to_string()),
        ]));
        let blocked = {
            let st = h.store.read().await;
            users::blocked_set(&st, &Default::default(), t0())
        };
        let out = users::sync_users(&ctx_of(&h), &h.shared, &blocked).await;
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let calls = h.hy2resi.calls();
        assert!(
            calls.contains(&"select:gate-r000:slot-1-out".to_string()),
            "{calls:?}"
        );
        assert!(
            calls.contains(&"select:gate-r002:deny".to_string()),
            "到期用户的门要被收回 deny：{calls:?}"
        );
    }

    /// 重启后 fail-closed：selector 回到 default=deny，由 b-ui 重放真实门位
    #[tokio::test(start_paused = true)]
    async fn gates_are_replayed_after_the_inbound_restarts() {
        let h = harness_with_pool().await;
        h.hy2resi.set_selected(BTreeMap::from([
            ("gate-r000".to_string(), "deny".to_string()),
            ("gate-r001".to_string(), "deny".to_string()),
        ]));
        let out = replay_after_restart(&ctx_of(&h), &h.shared).await;
        assert_eq!(out.switched, 2);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let calls = h.hy2resi.calls();
        assert!(
            calls.contains(&"select:gate-r000:slot-1-out".to_string()),
            "{calls:?}"
        );
        assert!(
            calls.contains(&"select:gate-r001:slot-2-out".to_string()),
            "{calls:?}"
        );
        assert!(
            resi_state::read(&h.runtime).await.alerts.is_empty(),
            "成功一次就不该留告警"
        );
    }

    /// Clash API 一直起不来：退避封顶 15 秒、门停在 deny、写告警 + Error 级事件，
    /// 60 秒安全网继续收敛
    #[tokio::test(start_paused = true)]
    async fn a_replay_that_never_succeeds_stays_closed_and_alerts() {
        let h = harness_with_pool().await;
        h.hy2resi.set_selected(BTreeMap::from([(
            "gate-r000".to_string(),
            "deny".to_string(),
        )]));
        h.hy2resi.set_never_ready();
        let t = tokio::time::Instant::now();
        let out = replay_after_restart(&ctx_of(&h), &h.shared).await;
        assert!(!out.errors.is_empty());
        assert_eq!(
            out.switched, 0,
            "一个门都没切 ⇒ 全员留在 deny（fail-closed）"
        );
        assert!(
            t.elapsed() <= REPLAY_RETRY_BUDGET + Duration::from_secs(1),
            "退避不许超过预算：{:?}",
            t.elapsed()
        );
        assert!(
            !h.hy2resi.calls().iter().any(|c| c.starts_with("select:")),
            "未就绪时一条 PUT 都不许发：{:?}",
            h.hy2resi.calls()
        );
        let alerts = resi_state::read(&h.runtime).await.alerts;
        assert!(
            alerts.iter().any(|a| a.contains("门位重放失败")),
            "{alerts:?}"
        );
        let inc = incidents::from_runtime(&h.runtime.read().await);
        assert_eq!(inc.len(), 1);
        assert_eq!(inc[0].signature, GATE_REPLAY_FAILED_SIG);
        assert_eq!(inc[0].level, Level::Error);
    }

    /// 订阅者接线：`Event::Hy2ResiRestarted` 一到就重放（**先 subscribe 再 spawn**，
    /// 否则 broadcast 会丢掉紧随 spawn 发出的那一条）
    #[tokio::test]
    async fn the_replay_loop_reacts_to_the_restart_event() {
        let h = harness_with_pool().await;
        h.hy2resi.set_selected(BTreeMap::from([(
            "gate-r000".to_string(),
            "deny".to_string(),
        )]));
        let ctx = ctx_of(&h);
        let rx = ctx.bus.subscribe();
        let task = tokio::spawn(replay_loop(ctx.clone(), h.shared.clone(), rx));
        ctx.bus.send(crate::api::Event::Hy2ResiRestarted);
        for _ in 0..200 {
            if h.hy2resi
                .calls()
                .contains(&"select:gate-r000:slot-1-out".to_string())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        task.abort();
        assert!(
            h.hy2resi
                .calls()
                .contains(&"select:gate-r000:slot-1-out".to_string()),
            "{:?}",
            h.hy2resi.calls()
        );
    }

    /// 上一次失败留下的告警在下一次成功时被认领清掉（口径同 relay 那条重放）
    #[tokio::test(start_paused = true)]
    async fn a_successful_replay_claims_the_previous_alert() {
        let h = harness_with_pool().await;
        h.hy2resi.set_selected(BTreeMap::from([(
            "gate-r000".to_string(),
            "deny".to_string(),
        )]));
        h.hy2resi.set_never_ready();
        replay_after_restart(&ctx_of(&h), &h.shared).await;
        assert!(!resi_state::read(&h.runtime).await.alerts.is_empty());
        h.hy2resi.set_ready(true);
        let out = replay_after_restart(&ctx_of(&h), &h.shared).await;
        assert_eq!(out.switched, 1);
        assert!(
            resi_state::read(&h.runtime).await.alerts.is_empty(),
            "恢复即清"
        );
    }
}
