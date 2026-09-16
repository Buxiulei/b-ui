//! `bui` 侧的槽位入口（spec §5.6）：把 `bui_schema::slots` 的纯函数包进一次
//! `Store::update_as`，顺带同步槽位表、重分配孤儿用户，再发一次
//! `Event::StateChanged("residential")` 交给 P1 的去抖对账重渲染。
//!
//! 本模块**不渲染任何内核配置**（与 `residential/mod.rs` 同一条边界）：
//! `config-residential[-<i>].yaml`、`singbox-relay.json`、`xray-config.json` 都由
//! `crate::modules::core_files` / `units` 从期望态渲染。
use crate::api::{Event, EventBus};
use crate::modules::panel::XrayApi;
use crate::modules::residential::clash::{self, Clash};
use crate::modules::residential::proxy::Prober;
use crate::modules::residential::state;
use crate::modules::residential::{health, SLOT_BACK_ROUNDS};
use crate::modules::sentinel::incidents::{self, Incident, Level};
use crate::reconcile::DaemonCtx;
use crate::state::runtime::Runtime;
use crate::state::store::Store;
use crate::sys::Host;
use crate::util::{fmt_rfc3339, parse_rfc3339};
use bui_schema::model::{ResidentialGroup, Slot, State, Upstream, DEFAULT_GROUP};
use bui_schema::render::xray as xray_render;
use bui_schema::render::xray::SlotRule;
use bui_schema::slots;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use time::OffsetDateTime;
use uuid::Uuid;

/// 一次带槽位同步的写入之后，槽位表与用户分配发生了什么。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SlotSync {
    /// 本次被释放的槽位（它们的上游已不在池里）
    pub released: Vec<Slot>,
    /// 因此被重新分配的用户数
    pub reassigned: usize,
    /// 手里那份订阅已经不能用、**必须重新拉订阅**的用户，按原因分三组（见
    /// [`slots::resubscribe_impact`]）：HY2 住宅节点的端口或跳跃段被这次写入改了。
    /// 在同一个临界区里按写入前后两份期望态算出 —— 调用方自己先 `store.read()` 再算
    /// 会拿到过期名单（两步之间可能插进另一次 add / assign / rebalance / remove）。
    pub impact: slots::ResubscribeImpact,
}

/// **改住宅池并同步槽位的唯一入口**：在同一次 `Store::update_as` 里改组、同步槽位表、
/// 重分配孤儿用户，写成功后发 `Event::StateChanged("residential")`。
///
/// `caller` 透传给 `Store::update_as` 的拒写防线（只有
/// [`crate::state::store::CALLER_RESI_REMOVE`] 能让上游池变短，R3 ①）。
/// 只改模式 / 关键字 / 优先级这类**不动池成员**的写入继续用
/// `state::update_group`，不必经过这里。
///
/// 返回的 [`SlotSync::impact`] 是这次写入之后必须重新拉订阅的用户（按原因分三组），
/// 执行路径（`upstream::add` 与 `upstream::remove`）都要把它逐组打给操作者 ——
/// **加上游也要**：跳跃段按当下槽数等分，多一个槽就把每个存活槽的段重切一遍。
pub async fn update_group_slots_as(
    store: &Store,
    bus: &EventBus,
    caller: &'static str,
    f: impl FnOnce(&mut ResidentialGroup),
) -> anyhow::Result<SlotSync> {
    let mut sync = SlotSync::default();
    let out = &mut sync;
    store
        .update_as(caller, |s| {
            let before = s.clone();
            let g = s
                .residential
                .groups
                .entry(DEFAULT_GROUP.to_string())
                .or_default();
            f(g);
            out.released = slots::sync_slots(&mut s.residential);
            out.reassigned = slots::migrate_unassigned(s);
            out.impact = slots::resubscribe_impact(&before, s);
        })
        .await?;
    bus.send(Event::StateChanged("residential"));
    Ok(sync)
}

/// 新建用户时分槽（spec §5.6 规则 1）**并给他一条住宅 HY2 凭据**（spec §3.1）。
/// **在 `Store::update` 的闭包里调**，与 `users.push(...)` 同一次写盘 —— 否则会出现
/// 「用户已存在但没有槽位」的中间态，那一瞬间他的 Reality 住宅会落到兜底槽；
/// 凭据同理：没有凭据 ⇒ `nodes_for` 压根不发住宅 HY2 节点，他刷出来的订阅里少一个节点。
///
/// 返回值仍只报**分槽**结果（调用方据此判要不要重算 Xray 槽规则）；凭据分没分到由调用方
/// 事后读 `hy2pool::cred_of` 判，池的 id 域用尽时要打 Error 级事件（spec §3.1：建用户
/// 不拒绝）。
pub fn assign_new_user(s: &mut State, user_id: Uuid, now: OffsetDateTime) -> bool {
    let slotted = slots::assign_least_loaded(s, user_id);
    assign_hy2_cred(s, user_id, now);
    slotted
}

/// 给用户分一条住宅 HY2 凭据（spec §3.1）。返回凭据 `id`；`None` = 没有住宅 hysteria2
/// 权益（不该占凭据），或池的 id 域（[`POOL_MAX`](bui_schema::hy2pool::POOL_MAX) = 256 条）
/// 用尽 —— 后者调用方要记 Error 级事件。
///
/// **权益判据不能省**（口径只有 [`gates::has_resi_hy2`](crate::modules::panel::gates) 一处）：
/// 纯直连用户白占一条凭据会把空闲吃掉，进而触发「当场扩容」= 重写 `hy2-residential.json`
/// + 重启 `hysteria-residential` ⇒ 全体住宅 HY2 会话重连一次。
///
/// **幂等**：已持凭据的用户原样拿回那一条（换凭据必须先 `hy2pool::release`）。
///
/// `now` 由调用方从 `Host::now()` 取：24 小时冷却期是安全判据，判定时钟必须与盖
/// `released_at` 的那个时钟同源（墙钟会让它测不到、也会被任何时钟偏移静默作废）。
///
/// 正常路径：池里恒有空闲（容量 = 2 × 用户数，下限 32）⇒ 一次 `assign_at` 就够，
/// `hy2-residential.json` 一个字节不动、内核不重启（spec §3.5）。分不出来（空闲全在
/// 24 小时冷却期内、或池还没建）才落到 `migrate`：它先补到 `size_for`、必要时当场应急
/// 扩容 16 条再分一轮 —— **建用户不因为池满被拒**。
pub fn assign_hy2_cred(s: &mut State, user_id: Uuid, now: OffsetDateTime) -> Option<String> {
    let entitled = s
        .users
        .iter()
        .find(|u| u.user_id == user_id)
        .is_some_and(|u| crate::modules::panel::gates::has_resi_hy2(u, &s.residential));
    if !entitled {
        return None;
    }
    if let Some(id) = bui_schema::hy2pool::assign_at(s, user_id, now) {
        return Some(id);
    }
    bui_schema::hy2pool::migrate(s, now);
    s.users
        .iter()
        .find(|u| u.user_id == user_id)
        .and_then(|u| u.credentials.hy2_resi_cred.clone())
}

/// 守护进程启动时跑一次槽位迁移（spec §5.6 规则 4）：
/// 旧 `state.json` 没有 `slots` 字段就补齐，既有住宅用户按创建时间轮流落槽。
/// 返回被分配的用户数；**幂等**，所以每次启动都可以无条件调用（零变更不写盘）。
pub async fn migrate_on_start(store: &Store, bus: &EventBus) -> anyhow::Result<usize> {
    let mut assigned = 0usize;
    let n = &mut assigned;
    store
        .update(|s| {
            slots::sync_slots(&mut s.residential);
            *n = slots::migrate_unassigned(s);
        })
        .await?;
    if assigned > 0 {
        tracing::info!(
            users = assigned,
            "按创建时间把既有住宅用户轮流落槽（spec §5.6 规则 4）"
        );
        bus.send(Event::StateChanged("residential"));
    }
    Ok(assigned)
}

/// 悬空凭据指针事件的签名（非日志签名，由本模块直接记事件）。
pub const CRED_DANGLING_SIG: &str = "hy2_resi_cred_dangling";
/// 凭据 id 域用尽事件的签名（spec §3.1：建用户不拒绝、当场扩容并记 Error 级事件）。
pub const POOL_EXHAUSTED_SIG: &str = "hy2_resi_pool_exhausted";

/// [`migrate_hy2_pool_on_start`] 这一轮做了什么。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PoolMigration {
    /// 指针悬空、被清成 `None` 的用户数（他们随后由 `migrate` 补发）
    pub healed: usize,
    /// 这一轮拿到（或新建）凭据的用户数
    pub changed: usize,
    /// 扩到 `POOL_MAX` 仍分不到凭据的住宅 hysteria2 用户数
    pub unassigned: usize,
}

/// 守护进程启动时把住宅 HY2 凭据池拉到期望态（spec §4.3 / §3.1）。**幂等**，零变更不写盘，
/// 所以每次启动无条件调一次；**必须在启动那一轮对账之前**跑完 —— `hy2-residential.json`
/// 的凭据、门与 `auth_user` 规则都从池里渲染。
///
/// 两步，顺序即语义：
///
/// 1. **先治「凭据指针悬空」**：`credentials.hy2_resi_cred` 指向池里**已不存在**的 id 时
///    （人工编辑 `state.json`、回滚到 4.0.x 再升上来、池被缩过），`hy2pool::assign` 会把那个
///    id 原样还给你、`cred_of` 却是 `None`，而 `migrate` 判 pending 用的是
///    `hy2_resi_cred.is_none()` ⇒ 这类用户**既不会被治愈、也不进 `unassigned`**，表现为
///    「有住宅权益但订阅里渲染不出住宅 HY2 节点」，而且零告警。所以先把这些指针清成
///    `None`（记一条 Warn 级事件，只写用户名与个数、**不写凭据**），让第 2 步正常补发。
/// 2. `hy2pool::migrate`：池空则按 `created_at` 升序给存量用户建
///    `{ name: 用户名, secret: 当时的 hy2_password }` 的凭据（于是升级**零刷新订阅**），
///    再补到 `size_for`、给没有凭据的人分配。`unassigned > 0` ⇒ 记一条 Error 级事件。
pub async fn migrate_hy2_pool_on_start(ctx: &DaemonCtx) -> anyhow::Result<PoolMigration> {
    let mut out = PoolMigration::default();
    let mut healed_names: Vec<String> = Vec::new();
    let (o, names) = (&mut out, &mut healed_names);
    // 冷却期判定与 `released_at` 的盖章走同一个注入时钟（墙钟会让这条安全判据测不到）
    let clock = ctx.host.now();
    ctx.store
        .update(|s| {
            let live: BTreeSet<String> = s
                .residential
                .hy2_pool
                .creds
                .iter()
                .map(|c| c.id.clone())
                .collect();
            for u in s.users.iter_mut() {
                let dangling = u
                    .credentials
                    .hy2_resi_cred
                    .as_deref()
                    .is_some_and(|id| !live.contains(id));
                if dangling {
                    u.credentials.hy2_resi_cred = None;
                    names.push(u.username.clone());
                }
            }
            let r = bui_schema::hy2pool::migrate(s, clock);
            *o = PoolMigration {
                healed: names.len(),
                changed: r.changed,
                unassigned: r.unassigned,
            };
        })
        .await?;
    let now = fmt_rfc3339(ctx.host.now());
    if out.healed > 0 {
        // 用户名可以进日志与事件（面板本来就在列它们），凭据一个字节都不许。
        tracing::warn!(
            users = out.healed,
            "住宅 HY2 凭据指针悬空，已清空并重新分配"
        );
        let inc = Incident {
            at: now.clone(),
            unit: "b-ui".into(),
            signature: CRED_DANGLING_SIG.into(),
            subject: healed_names.join("、"),
            action: "清空悬空指针并重新分配".into(),
            result: format!("{} 个用户的住宅 HY2 凭据指向池外，已重新分配", out.healed),
            level: Level::Warn,
            sample: None,
        };
        ctx.runtime.update(move |rt| incidents::push(rt, inc)).await;
    }
    if out.unassigned > 0 {
        tracing::error!(
            users = out.unassigned,
            "住宅 HY2 凭据池 id 域用尽，这些用户渲染不出住宅 HY2 节点"
        );
        let inc = Incident {
            at: now,
            unit: "b-ui".into(),
            signature: POOL_EXHAUSTED_SIG.into(),
            subject: "hy2_pool".into(),
            action: "扩容凭据池".into(),
            result: format!(
                "扩到上限 {} 条仍有 {} 个用户分不到凭据",
                bui_schema::hy2pool::POOL_MAX,
                out.unassigned
            ),
            level: Level::Error,
            sample: None,
        };
        ctx.runtime.update(move |rt| incidents::push(rt, inc)).await;
    }
    if out.changed > 0 || out.healed > 0 {
        tracing::info!(
            changed = out.changed,
            healed = out.healed,
            "住宅 HY2 凭据池已迁移（spec §4.3）"
        );
        ctx.bus.send(Event::StateChanged("residential"));
    }
    Ok(out)
}

/// 对账前的期望态整备（spec §3.1 第三道防线）：`hy2-residential.json` 这一轮**本来就要
/// 重写**（§3.5 的四件事之一 ⇒ 住宅内核要重启）时，顺带把**空闲**凭据的 `secret` 重随机。
/// 返回换掉的条数。
///
/// 为什么非做不可：被 `release` 掉的凭据只有 24 小时冷却期这一道屏障，冷却期一过它就
/// 原样（同 id、同 secret）发给下一个人，前任持有人手里的旧订阅直接连上新人的门。
/// 空闲凭据的旧密码只有前任知道，而它写在内核配置里 —— **重启是唯一能换掉它的时机**。
///
/// 判据是「不算这次轮换、这一轮也会重写」：按当前期望态渲染一次，与盘上的字节比
/// （`reconcile::diff` 也是逐字节比）。一致 ⇒ 一个字节都不动 —— 否则每轮对账都会因为
/// 自己刚换的 secret 重写文件 + 重启住宅内核，全体在线连接跟着断。
pub async fn reroll_idle_hy2_secrets(ctx: &DaemonCtx) -> usize {
    let path = crate::modules::core_files::hy2_resi_config_path(&ctx.paths);
    let want = {
        let s = ctx.store.read().await;
        if s.residential.hy2_pool.creds.is_empty() {
            return 0; // 池还没建（首装、迁移之前）：没有空闲凭据可换
        }
        serde_json::to_vec_pretty(&bui_schema::render::hy2_singbox::config(
            &s.node,
            &ctx.paths,
            &s.residential.hy2_pool,
        ))
        .ok()
    };
    let host = ctx.host.clone();
    let landed = tokio::task::spawn_blocking(move || host.read_file(&path))
        .await
        .ok()
        .and_then(Result::ok)
        .flatten();
    if want.is_none() || landed == want {
        return 0; // 这一轮不会重写它 ⇒ 不许趁机换 secret（换了就是白重启一次内核）
    }
    let mut rerolled = 0usize;
    let n = &mut rerolled;
    let r = ctx
        .store
        .update(|s| {
            let used: BTreeSet<String> = s
                .users
                .iter()
                .filter_map(|u| u.credentials.hy2_resi_cred.clone())
                .collect();
            *n = bui_schema::hy2pool::regenerate_idle_secrets(&mut s.residential.hy2_pool, &used);
        })
        .await;
    if let Err(e) = r {
        tracing::warn!(error = %e, "重随机空闲住宅 HY2 凭据写盘失败，下一轮对账重试");
        return 0;
    }
    if rerolled > 0 {
        // 只写条数，凭据一个字节都不许进日志
        tracing::info!(
            creds = rerolled,
            "住宅 HY2 配置这一轮要重写，顺带换掉空闲凭据的 secret（spec §3.1）"
        );
    }
    rerolled
}

/// [`converge_xray`] 这一轮做了什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvergeOutcome {
    /// 没脏、或脏但哈希已一致（清脏、不动 xray）
    Clean,
    /// 走 gRPC 增删收敛成功，带发出的 `AddRule`/`RemoveRule` 条数
    Applied(usize),
    /// gRPC 失败、新配置已落盘 ⇒ 退回了一次 `systemctl restart xray`
    Restarted,
    /// xray 本次启动晚于磁盘上那份（已是期望槽规则的）`xray-config.json` 的写入 ⇒
    /// 进程已从磁盘加载了完整槽规则，直接记账，不调 gRPC、不重启
    LoadedFromDisk,
    /// 什么都没做，脏标记留着等下一轮
    Deferred,
}

/// 连接类 gRPC 错误的退避重试总预算（从第一次尝试算起，含每次尝试本身的耗时）
pub const GRPC_RETRY_BUDGET: Duration = Duration::from_secs(15);
/// 退避的首个间隔，此后每次翻倍（0.5/1/2/4 秒…），最后一段截到预算用完为止
const GRPC_RETRY_FIRST: Duration = Duration::from_millis(500);
/// [`restart_fallback`] 那条告警的固定前缀：收敛成功时按它认领、清掉
const GRPC_FAIL_ALERT: &str = "Xray 槽路由 gRPC 失败";

/// 标记「渲染出的 Xray 槽路由与 xray 进程里跑的那一份可能已经不一致」（D7）。
/// 用户增删、改分槽、`rebalance`、删上游后的重分配之后都要置位。
pub async fn mark_xray_rules_dirty(runtime: &Runtime) {
    state::update(runtime, |r| r.xray_slot_rules_dirty = true).await;
}

/// 让 Xray 的住宅槽路由与期望态一致（D7）。**正常路径不重启 xray**。
///
/// 五道门（顺序即语义，别调整）：
/// 1. `runtime.residential.xray_slot_rules_dirty` 没置位 ⇒ 什么都不做；
/// 2. 置位但 `xray_slot_rules_hash` 已等于当前渲染的 `slot_rules_hash` ⇒ 清脏、不动 xray
///    （这一轮的变化与槽路由无关，例如加了个没有住宅权益的用户）；
/// 3. 磁盘上那份 `xray-config.json` 已是期望槽规则，**且** xray 本次启动晚于它的写入
///    （[`loaded_from_disk`]）⇒ 进程就是从这份文件起来的：记哈希、清脏，不调 gRPC、
///    不重启（对账刚为别的原因重启过 xray 的那一轮就是这样）；
/// 4. 否则 `ListRule()` 读回**进程里正在跑的**那张表，与期望态求差，只对差集调
///    `RemoveRule` / `AddRule`（每个用户一条规则，`ruleTag` = `resi-u-<user_id>`），
///    最后把兜底规则挪回表尾。差分连**表序**一起比：内容一致但兜底不在表尾也算差异，
///    所以哈希只在顺序与内容都收敛之后才写；全成功 ⇒ 记哈希、清脏。连接类错误
///    （xray 刚重启、还没起监听）先按退避重试（[`apply_with_retry`]）；
/// 5. 重试用完或非连接类错误 ⇒ [`restart_fallback`]：只有磁盘上那份 `xray-config.json`
///    已经是新规则时才重启一次并记事件，否则什么都不做、脏标记留着。
///
/// 第 3、4 道门成功时顺带清掉第 5 道门以前写下的「gRPC 失败」告警（[`mark_converged`]）。
///
/// 调用点只有对账 consumer 的末尾（`serve.rs` 启动那一轮 + 去抖那一轮）：那时新的
/// `xray-config.json` 刚落盘，第 4 步的判据才可能成立。干净时是个零成本 no-op，
/// 可以无条件调。**不在这里发 `ReconcileRequested`**：那会与「对账末尾调本函数」
/// 组成自触发环；脏标记留着等 10 分钟巡检 tick / 下一次 `StateChanged` / 每日自检更安全。
pub async fn converge_xray(ctx: &DaemonCtx, xray: &dyn XrayApi) -> ConvergeOutcome {
    if !state::read(&ctx.runtime).await.xray_slot_rules_dirty {
        return ConvergeOutcome::Clean;
    }
    let (want, want_hash) = {
        let s = ctx.store.read().await;
        let cfg = xray_render::config(&s.node, &s.users, &s.residential, &ctx.paths);
        (
            xray_render::slot_rules(&s.users, &s.residential),
            xray_render::slot_rules_hash(&cfg),
        )
    };
    if state::read(&ctx.runtime)
        .await
        .xray_slot_rules_hash
        .as_deref()
        == Some(want_hash.as_str())
    {
        state::update(&ctx.runtime, |r| r.xray_slot_rules_dirty = false).await;
        return ConvergeOutcome::Clean;
    }
    if loaded_from_disk(ctx, &want_hash).await {
        mark_converged(ctx, want_hash).await;
        tracing::info!(
            "xray 本次启动晚于 xray-config.json 落盘，已从磁盘加载完整槽规则：直接记账（不调 gRPC、不重启）"
        );
        return ConvergeOutcome::LoadedFromDisk;
    }
    match apply_with_retry(xray, &want).await {
        Ok(calls) => {
            mark_converged(ctx, want_hash).await;
            if calls > 0 {
                tracing::info!(
                    calls,
                    "Xray 住宅槽路由已按用户增删收敛（未重启，spec §5.6）"
                );
            }
            ConvergeOutcome::Applied(calls)
        }
        Err(e) => {
            tracing::warn!(error = %e, "Xray RoutingService 收敛失败，看磁盘配置是否已落盘");
            restart_fallback(ctx, &want_hash, &e.to_string()).await
        }
    }
}

/// 收敛成功（gRPC 增删，或第 3 道门确认进程已从磁盘加载）的记账：记哈希、清脏，并认领
/// [`restart_fallback`] 以前写下的「gRPC 失败」告警 —— 它描述的是一次已经过去的退回，
/// 收敛成功后还挂着只会让面板与 `status` 一直报警（2026-09-13 bwg-rick）。
async fn mark_converged(ctx: &DaemonCtx, hash: String) {
    let mut cleared = 0usize;
    let n = &mut cleared;
    state::update(&ctx.runtime, move |r| {
        r.xray_slot_rules_dirty = false;
        r.xray_slot_rules_hash = Some(hash);
        *n = state::remove_alerts_with_prefix(r, GRPC_FAIL_ALERT);
    })
    .await;
    if cleared > 0 {
        tracing::info!(cleared, "Xray 槽路由已收敛，清除旧的「gRPC 失败」告警");
    }
}

/// 磁盘上那份 `xray-config.json` 的槽路由哈希（读不到 / 解析不了 ⇒ `None`）。
async fn landed_hash(ctx: &DaemonCtx) -> Option<String> {
    let path = ctx.paths.base_dir.join("xray-config.json");
    let host = ctx.host.clone();
    tokio::task::spawn_blocking(move || host.read_file(&path))
        .await
        .ok()
        .and_then(Result::ok)
        .flatten()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .map(|v| xray_render::slot_rules_hash(&v))
}

/// 第 3 道门：磁盘上已是期望槽规则，**且** xray 本次启动严格晚于该文件的最后一次写入 ⇒
/// 进程就是从这份文件起来的，里面已是完整槽规则。改分槽只重写文件、不重启 xray
/// （`restart_key` 是结构哈希），那时启动早于写入，这道门自然不成立。
async fn loaded_from_disk(ctx: &DaemonCtx, want_hash: &str) -> bool {
    if landed_hash(ctx).await.as_deref() != Some(want_hash) {
        return false;
    }
    let path = ctx.paths.base_dir.join("xray-config.json");
    let host = ctx.host.clone();
    tokio::task::spawn_blocking(move || xray_started_after(host.as_ref(), &path))
        .await
        .unwrap_or(false)
}

/// xray **本次启动**（`ActiveEnterTimestamp`）是否严格晚于 `path` 的 mtime。任一时间取不到
/// 或解析不了 ⇒ `false`（不走捷径，照常调 gRPC）。
///
/// 不用 [`Host::unit_property`]：它的 `systemctl show --value` 按本机时区输出
/// （`Sun 2026-09-13 08:05:30 CST`），时区缩写有歧义；这里要 `--timestamp=us+utc` 拿微秒 +
/// UTC（不认这个选项的老 systemd ⇒ 退出码非 0 ⇒ 不走捷径）。精度要到亚秒：真机实录里
/// 写配置与重启 xray 就在同一秒。
fn xray_started_after(host: &dyn Host, path: &Path) -> bool {
    fn stdout_of(host: &dyn Host, program: &str, args: &[&str]) -> Option<String> {
        host.run(program, args)
            .ok()
            .filter(|o| o.ok())
            .map(|o| o.stdout)
    }
    let started = stdout_of(
        host,
        "systemctl",
        &[
            "show",
            "-p",
            "ActiveEnterTimestamp",
            "--value",
            "--timestamp=us+utc",
            "xray.service",
        ],
    )
    .and_then(|s| parse_systemd_utc(&s));
    let mtime = stdout_of(host, "stat", &["-c", "%y", &path.to_string_lossy()])
        .and_then(|s| parse_stat_mtime(&s));
    matches!((started, mtime), (Some(s), Some(m)) if s > m)
}

/// `systemctl show --timestamp=us+utc` 的值：`Sun 2026-09-13 00:05:30.512345 UTC`
/// （星期几随 locale 变，只取后三段）。
fn parse_systemd_utc(s: &str) -> Option<OffsetDateTime> {
    match s.split_whitespace().collect::<Vec<_>>().as_slice() {
        [.., date, time, "UTC"] => parse_rfc3339(&format!("{date}T{time}Z")),
        _ => None,
    }
}

/// `stat -c %y` 的值：`2026-09-13 08:05:30.101234567 +0800`。
fn parse_stat_mtime(s: &str) -> Option<OffsetDateTime> {
    match s.split_whitespace().collect::<Vec<_>>().as_slice() {
        [date, time, off] => {
            let (h, m) = (off.get(..3)?, off.get(3..)?);
            parse_rfc3339(&format!("{date}T{time}{h}:{m}"))
        }
        _ => None,
    }
}

/// [`apply_slot_rules`] 外面包一层退避：**连接类**错误（[`is_connect_error`]；2026-09-13
/// bwg-rick 实录就是对账重启 xray 的同一秒撞上 10085 Connection refused）按 0.5/1/2/4…
/// 秒退避重试，总时长不超过 [`GRPC_RETRY_BUDGET`]；预算用完、或非连接类错误（规则非法、
/// 重名 ruleTag…）立刻把错误交回，由调用方走 [`restart_fallback`]。每次重试都重新
/// `ListRule`，中途失败留下的半截增删会被下一次差分补齐。
async fn apply_with_retry(xray: &dyn XrayApi, want: &[SlotRule]) -> anyhow::Result<usize> {
    let deadline = tokio::time::Instant::now() + GRPC_RETRY_BUDGET;
    let mut delay = GRPC_RETRY_FIRST;
    loop {
        let err = match apply_slot_rules(xray, want).await {
            Err(e) if is_connect_error(&e) => e,
            other => return other,
        };
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return Err(err);
        }
        let wait = delay.min(left);
        tracing::info!(
            error = %err,
            wait = ?wait,
            "xray gRPC 暂时连不上（多半刚重启、还没起监听），退避后重试"
        );
        tokio::time::sleep(wait).await;
        delay *= 2;
    }
}

/// 连接类错误：连不上（`Unavailable`，tonic 把 connect error 映射到它）、超时
/// （`DeadlineExceeded`，或 tonic 客户端超时给的 `Cancelled("Timeout expired")`）。
/// 其余错误码与非 `tonic::Status` 的错误一律不算。
fn is_connect_error(e: &anyhow::Error) -> bool {
    use tonic::Code;
    e.downcast_ref::<tonic::Status>()
        .is_some_and(|s| match s.code() {
            Code::Unavailable | Code::DeadlineExceeded => true,
            Code::Cancelled => s.message() == tonic::TimeoutExpired(()).to_string(),
            _ => false,
        })
}

/// 用 `ListRule` 读回进程里真正在跑的那张表，只对差集调 `RemoveRule` / `AddRule`。
/// 返回发出的增删条数（0 = 本来就一致）。任一调用失败立刻返回 `Err`，**不回滚** ——
/// 下一轮 `ListRule` 会看到真实状态再补差分（幂等）。
///
/// 差分要**连表序一起比**：Xray 按表序首条匹配，兜底规则（没有 `user` 条件，吃掉
/// 整个住宅入站）一旦排在某条用户规则之前，那个用户就会被路由到兜底槽而不是自己的槽。
/// 所以「内容一致但兜底不在表尾」也算差异 —— 否则上一轮把兜底挪回表尾那步失败之后，
/// 下一轮会误判为一致、记哈希清脏，把错序永久固化下来。
async fn apply_slot_rules(xray: &dyn XrayApi, want: &[SlotRule]) -> anyhow::Result<usize> {
    // 保留 `ListRule` 的原始表序：顺序本身就是要收敛的一部分
    let live: Vec<(String, String)> = xray
        .list_rules()
        .await?
        .into_iter()
        .filter(|(tag, _)| {
            tag.starts_with(xray_render::USER_RULE_PREFIX) || tag == xray_render::FALLBACK_RULE_TAG
        })
        .collect();
    let by_tag: BTreeMap<&str, &str> = live
        .iter()
        .map(|(tag, out)| (tag.as_str(), out.as_str()))
        .collect();
    let wanted: BTreeSet<&str> = want.iter().map(|r| r.rule_tag.as_str()).collect();
    let mut calls = 0usize;
    // ① 多出来的（用户删了 / 没了住宅权益 / 换了 tag 写法）
    for (tag, _) in &live {
        if !wanted.contains(tag.as_str()) {
            xray.remove_rule(tag).await?;
            calls += 1;
        }
    }
    // ② 缺的或指错槽的：**先删再加** —— 重名 ruleTag 会让整条 AddRule 报错（D7 事实①）
    let fallback = want.last().expect("slot_rules 末尾一定是兜底规则").clone();
    let mut appended = false;
    for r in want.iter().filter(|r| r.rule_tag != fallback.rule_tag) {
        if by_tag.get(r.rule_tag.as_str()).copied() == Some(r.outbound_tag.as_str()) {
            continue;
        }
        if by_tag.contains_key(r.rule_tag.as_str()) {
            xray.remove_rule(&r.rule_tag).await?;
            calls += 1;
        }
        xray.add_rule(r).await?;
        calls += 1;
        appended = true;
    }
    // ③ `AddRule` 只能追加到表尾 ⇒ 这一轮追加过、或兜底本身缺了 / 指错了 / 排在别的
    //    槽规则前面，就把兜底删掉再追加一次，让它回到全部用户规则之后。两次调用之间
    //    有个亚毫秒窗口，期间「一条规则都没有的 email」落到首个出站 direct（D7 已接受）。
    //    ①删掉的那些不算数：判位置只看留下来的（`wanted` 里的）那些规则的相对次序。
    let last_kept = live
        .iter()
        .rev()
        .find(|(tag, _)| wanted.contains(tag.as_str()))
        .map(|(tag, _)| tag.as_str());
    let fallback_ok = by_tag.get(fallback.rule_tag.as_str()).copied()
        == Some(fallback.outbound_tag.as_str())
        && last_kept == Some(fallback.rule_tag.as_str());
    if appended || !fallback_ok {
        if by_tag.contains_key(fallback.rule_tag.as_str()) {
            xray.remove_rule(&fallback.rule_tag).await?;
            calls += 1;
        }
        xray.add_rule(&fallback).await?;
        calls += 1;
    }
    Ok(calls)
}

/// gRPC 收敛失败时的唯一退路：重启一次 xray，让它把磁盘上那份**完整**规则表读回来。
/// 只有磁盘哈希已等于期望哈希才动手 —— 否则重启只是把旧规则重新加载一遍。
async fn restart_fallback(ctx: &DaemonCtx, want_hash: &str, err: &str) -> ConvergeOutcome {
    let landed = landed_hash(ctx).await;
    if landed.as_deref() != Some(want_hash) {
        tracing::debug!(
            landed = ?landed,
            want = %want_hash,
            "xray-config.json 还没落到新的槽路由，本轮不重启（脏标记留着）"
        );
        return ConvergeOutcome::Deferred;
    }
    let host = ctx.host.clone();
    let out = tokio::task::spawn_blocking(move || host.systemd("restart", "xray.service")).await;
    if !matches!(out, Ok(Ok(ref o)) if o.status == 0) {
        tracing::warn!("重启 xray 失败，槽路由等下一轮对账再收敛");
        return ConvergeOutcome::Deferred;
    }
    // 清脏 ⇒ 这次脏事件最多退回一次重启，不会每 10 分钟掐一遍连接
    let hash = want_hash.to_string();
    // 错误串是 gRPC 状态文本，不含凭据；仍然只取首行，避免把多行 tonic 报文塞进面板
    let msg = format!(
        "{GRPC_FAIL_ALERT}（{}），已重启 xray 收敛一次",
        err.lines().next().unwrap_or("")
    );
    state::update(&ctx.runtime, move |r| {
        r.xray_slot_rules_dirty = false;
        r.xray_slot_rules_hash = Some(hash);
        state::push_alert(r, msg);
    })
    .await;
    tracing::info!("已重启 xray 使住宅槽路由生效（gRPC 退路，spec §5.6）");
    ConvergeOutcome::Restarted
}

/// 手动把一个用户钉到某个槽（spec §5.6 规则 3 的 `assign`）。
/// 成功返回 `true`、发 `StateChanged("residential")` 并置脏标记 —— 收尾（gRPC 增删）由
/// 那次 `StateChanged` 触发的对账末尾的 [`converge_xray`] 负责，**调用方不要自己去调 gRPC**。
///
/// 第二个返回值是**必须重新拉订阅**的用户（[`slots::resubscribe_impact`]）：换槽把他的
/// HY2 住宅端口与跳跃段一起改了，而那两样写死在已下发的订阅里。名单在**这次写入的临界区
/// 里**算出，调用方自己前后 `read()` 会拿到过期名单。
pub async fn assign_user(
    ctx: &DaemonCtx,
    user_id: Uuid,
    slot_upstream_id: Uuid,
) -> anyhow::Result<(bool, slots::ResubscribeImpact)> {
    let mut out = (false, slots::ResubscribeImpact::default());
    let o = &mut out;
    ctx.store
        .update(|s| {
            let before = s.clone();
            o.0 = slots::assign(s, user_id, slot_upstream_id);
            o.1 = slots::resubscribe_impact(&before, s);
        })
        .await?;
    if out.0 {
        ctx.bus.send(Event::StateChanged("residential"));
        mark_xray_rules_dirty(&ctx.runtime).await;
    }
    Ok(out)
}

/// spec §5.6 规则 3：把住宅用户在各槽间均匀重排。返回被改动的用户数，以及**必须重新拉
/// 订阅**的用户（口径同 [`assign_user`]）。
///
/// **只写 state + 置脏 + 发事件，不自己碰 xray**（D7）：让那次 `StateChanged` 触发的对账
/// 把新的 `xray-config.json` 写下去（给下次启动用），对账末尾的 [`converge_xray`] 再走
/// `RoutingService` gRPC 把**被挪动的那些用户**的规则增删掉 —— 从按下按钮到槽路由生效
/// 约 1 秒（500ms 去抖 + 一轮对账），**xray 不重启、在线连接不断**。
pub async fn rebalance_users(ctx: &DaemonCtx) -> anyhow::Result<(usize, slots::ResubscribeImpact)> {
    let mut out = (0usize, slots::ResubscribeImpact::default());
    let o = &mut out;
    ctx.store
        .update(|s| {
            let before = s.clone();
            o.0 = slots::rebalance(s);
            o.1 = slots::resubscribe_impact(&before, s);
        })
        .await?;
    let moved = out.0;
    if moved > 0 {
        ctx.bus.send(Event::StateChanged("residential"));
        mark_xray_rules_dirty(&ctx.runtime).await;
        tracing::info!(moved, "住宅用户按槽重排（spec §5.6 规则 3），等对账后收口");
    }
    Ok(out)
}

// ── 按槽驱动 selector（spec §5.6，裁决 D8）────────────────────────────────────

/// 一个槽在这一轮的驱动结果。
#[derive(Debug, Clone, PartialEq)]
pub struct SlotOutcome {
    pub index: u16,
    /// 本槽自己的 IP
    pub own: Uuid,
    /// **终态**：这一槽的 selector 现在指着哪条上游
    pub target: Uuid,
    /// `target != own`
    pub borrowed: bool,
    /// 切成了：`target` 已生效（[`borrow_now`] 里还要求它当下带外验证**没被证死**，
    /// 是否确认可用看 `unconfirmed`）
    pub switched: bool,
    /// [`borrow_now`]：切过去了，但**没能在预算内确认它可用**（[`health::QUICK_PROBE_BUDGET_SECS`]
    /// 超时 / 探测任务异常 / 本次调用的验证预算已用尽）。此时仍然保留这个候选 ——
    /// 本槽原来的出口是**已知死的**，未知优于已知死 —— 但告警绝不许说「已临时切到」。
    /// [`drive_slots`] 永远 false
    pub unconfirmed: bool,
    /// [`borrow_now`]：候选**全部试完且逐条被证死**（`dead` 就是全部候选）⇒ 这一槽此刻
    /// 真的没有可用出口。告警里「当前无可用出口」那句话的唯一依据（演练
    /// `sentinel-drill.sh --all-ports` 判据②' 认这个关键词）。[`drive_slots`] 永远 false
    pub no_exit: bool,
    /// 没切 / 切失败的原因，或 `switched && unconfirmed` 时「没能确认」的原因
    /// （面板与 CLI 直接显示；`sentinel::resi` 的告警文案在它前后补上终态）
    pub note: Option<String>,
    /// [`borrow_now`] 本次调用里**已被证死**的候选，每条都已 `mark_unhealthy`：先是前面的槽
    /// 探死、本槽据此跳过的（按候选顺序），再是本槽亲自探死的（按尝试顺序）。
    /// [`drive_slots`] 永远留空
    pub dead: Vec<Uuid>,
}

/// 按槽切换的互斥（设计裁决 D7）：[`drive_slots`] 整轮「读快照 → 逐槽 PUT → 按快照写回
/// `current_upstream_id`」，[`borrow_now`] 若落在中间，它写的记录会被那轮写回盖成旧值（relay 的
/// selector 已经切走，runtime 还记着旧值）。两者各自整段持锁。
///
/// **锁内不只有毫秒级的 Clash PUT**：[`borrow_now`] 在锁内最多做 [`BORROW_PROBES_PER_CALL`] 次
/// 带外验证，每次上限 [`health::QUICK_PROBE_BUDGET_SECS`] 秒，所以一次持锁最坏是「每个受影响的
/// 槽几次 PUT + 3 × 4 秒」。算预算时按这个上界算，别按「毫秒级」算。这段时间里巡检那一轮只是排队等
/// （`sentinel_loop` 与巡检都用 `MissedTickBehavior::Delay`，不补跑），单一互斥、无嵌套，
/// store 的读锁在探测前已经 drop。
///
/// **每个守护进程一把**（按 `ctx.host` 这个 `Arc` 的地址区分，`DaemonCtx` 的克隆共用它）：生产上
/// 一个进程只有一个 `DaemonCtx`，等于进程级一把。不写成进程级 `static`：同一个测试二进制里的多个
/// `#[tokio::test]` 会共用这把锁，`start_paused` 的测试在等别的测试放锁时假时钟自动推进，
/// 「等到出现」的循环瞬间耗尽（同 `api::auth::LoginLimiter` 不用进程级 `OnceLock` 的理由）。
fn slot_switch(ctx: &DaemonCtx) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: std::sync::Mutex<BTreeMap<usize, Arc<tokio::sync::Mutex<()>>>> =
        std::sync::Mutex::new(BTreeMap::new());
    let key = Arc::as_ptr(&ctx.host).cast::<()>() as usize;
    LOCKS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(key)
        .or_default()
        .clone()
}

/// [`put_slot`] 没切成的原因。文案与抽出前的 `drive_slots` 逐字相同（面板与 CLI 直接显示）。
enum PutFail {
    /// 目标已不在池里
    Gone,
    /// Clash API 拒绝 / 不可达
    Rejected(String),
    /// `spawn_blocking` 任务异常
    Task(String),
}

impl PutFail {
    fn note(&self) -> String {
        match self {
            PutFail::Gone => "目标已不在池里，本轮不切".into(),
            PutFail::Rejected(s) | PutFail::Task(s) => s.clone(),
        }
    }
}

/// 把槽 `index` 的 selector 切到 `target`：成功返回目标 tag。[`drive_slots`] 与 [`borrow_now`]
/// 共用，「切一槽」只有这一份实现。
async fn put_slot(
    c: Arc<dyn Clash>,
    g: &ResidentialGroup,
    index: u16,
    target: Uuid,
) -> Result<String, PutFail> {
    let Some(tag) = clash::tag_of(g, target) else {
        return Err(PutFail::Gone);
    };
    let (sel, t) = (super::slot_selector(index), tag.clone());
    match tokio::task::spawn_blocking(move || c.select(&sel, &t)).await {
        Ok(Ok(())) => Ok(tag),
        Ok(Err(e)) => Err(PutFail::Rejected(format!("切到 {tag} 失败：{e}"))),
        Err(e) => Err(PutFail::Task(format!("切换任务异常：{e}"))),
    }
}

/// 按槽驱动各自的 selector（spec §5.6）。每轮巡检的**最后一步**，由
/// `health::check_round` 调用；`healthy` 就是那一轮算出来的健康成员集。
///
/// 每槽的规则（比全局 selector 的规则简单得多，D8）：
/// 1. 手动 pin 且目标还在池里 ⇒ 用 pin，不看健康度（管理员的判断压过巡检）；
/// 2. 本槽自己的 IP 健康**且** Google 没被封 ⇒ 用本槽；正在借用时要先攒满
///    [`SLOT_BACK_ROUNDS`] 轮才切回（防抖）；
/// 3. 否则借用 [`health::rank_healthy`] 里排名最高的**其他**健康 IP；
/// 4. 一个健康的都没有 ⇒ **本轮什么都不做**：不发 Clash PUT、不改 runtime 里的
///    `current_upstream_id`，只记一条 note（fail-open，降级总比乱切好）。首轮就全池不健康
///    时 `current` 还是 `None`，这条规则同样必须**保持沉默** —— 此时随便 PUT 一条不健康的
///    IP 只是把故障固化下来，relay 的 selector 本来就停在配置里的 `default`（本槽 IP）上。
///
/// 规则 2 的「攒满轮数才切回」与 [`borrow_now`] 的「当下探一次就借」故意不对称，理由见
/// [`borrow_now`] 的文档：**失败会静默的方向要即时证据，失败会吵闹的方向要持续证据。**
///
/// 每槽独立：一个槽切换不影响别的槽的连接（各自一个 selector，
/// `interrupt_exist_connections: false`）。整轮只写一次 runtime。
pub async fn drive_slots(
    ctx: &DaemonCtx,
    c: Arc<dyn Clash>,
    g: &ResidentialGroup,
    healthy: &[Uuid],
    now: OffsetDateTime,
) -> Vec<SlotOutcome> {
    let _switch = slot_switch(ctx).lock_owned().await;
    let view = slots::sorted(&ctx.store.read().await.residential);
    if view.is_empty() {
        return Vec::new();
    }
    let rt = state::read(&ctx.runtime).await;
    let ranked = health::rank_healthy(g, &rt, healthy, now);
    let google_ok =
        |id: Uuid| rt.health.get(&id.to_string()).and_then(|h| h.google_ok) != Some(false);
    let is_healthy = |id: Uuid| healthy.contains(&id);

    let mut out: Vec<SlotOutcome> = Vec::with_capacity(view.len());
    // (槽序号, 本轮该用的 IP, 新的 back_rounds)
    let mut writes: Vec<(u16, Option<Uuid>, u32)> = Vec::new();
    for s in &view {
        if !g.upstreams.iter().any(|u| u.id == s.upstream_id) {
            continue; // 槽位与池并发变化，本轮跳过（下一轮 sync_slots 会收拾）
        }
        let key = s.index.to_string();
        let sr = rt.slots.get(&key).cloned().unwrap_or_default();
        let current = sr.current_upstream_id;
        let own_good = is_healthy(s.upstream_id) && google_ok(s.upstream_id);

        // `hold` = 规则 4：本轮不许发 PUT（全池不健康，保持现状）
        let (target, mut rounds, mut note, hold) = match sr.pinned_upstream_id {
            // 规则 1
            Some(p) if g.upstreams.iter().any(|u| u.id == p) => {
                (p, 0, Some("手动 pin".to_string()), false)
            }
            _ if own_good => {
                // 规则 2：正在借用时要攒轮数
                if current.is_some_and(|cur| cur != s.upstream_id) {
                    let r = sr.back_rounds + 1;
                    if r >= SLOT_BACK_ROUNDS {
                        (s.upstream_id, 0, None, false)
                    } else {
                        (
                            current.expect("上一行判过 is_some"),
                            r,
                            Some(format!("本槽已恢复 {r}/{SLOT_BACK_ROUNDS} 轮，攒满才切回")),
                            false,
                        )
                    }
                } else {
                    (s.upstream_id, 0, None, false)
                }
            }
            // 规则 3
            _ => match ranked.iter().copied().find(|id| *id != s.upstream_id) {
                Some(borrow) => (
                    borrow,
                    0,
                    Some("本槽 IP 不可用，临时借用".to_string()),
                    false,
                ),
                // 规则 4：`hold = true` ⇒ 下面那段 PUT 整段跳过
                None => (
                    current.unwrap_or(s.upstream_id),
                    sr.back_rounds,
                    Some("没有可借用的健康 IP，保持现状".to_string()),
                    true,
                ),
            },
        };

        let mut switched = false;
        if !hold && current != Some(target) {
            match put_slot(c.clone(), g, s.index, target).await {
                Ok(tag) => {
                    switched = true;
                    tracing::info!(slot = s.index, to = %tag, "按槽切换住宅出口");
                }
                Err(f) => {
                    if matches!(f, PutFail::Rejected(_)) {
                        rounds = sr.back_rounds; // 没切成就别把轮数清掉
                    }
                    note = Some(f.note());
                }
            }
        }
        let landed = if switched || current == Some(target) {
            Some(target)
        } else {
            current
        };
        writes.push((s.index, landed, rounds));
        out.push(SlotOutcome {
            index: s.index,
            own: s.upstream_id,
            target: landed.unwrap_or(target),
            borrowed: landed.is_some_and(|l| l != s.upstream_id),
            switched,
            unconfirmed: false,
            no_exit: false,
            note,
            dead: Vec::new(),
        });
    }

    // 整轮一次写盘（每次 update 都是 tmp + rename，按槽各写一次等于一轮 N 次落盘）
    if !writes.is_empty() {
        state::update(&ctx.runtime, move |r| {
            for (index, landed, rounds) in writes {
                let e = r.slots.entry(index.to_string()).or_default();
                e.current_upstream_id = landed;
                e.back_rounds = rounds;
            }
            // 池缩小后留下的槽位运行时条目
            r.slots
                .retain(|k, _| k.parse::<u16>().is_ok_and(|i| i < slots::MAX_SLOTS));
        })
        .await;
    }
    out
}

/// [`borrow_now`] 遇到压在故障 IP 上、但被手动 pin 的槽时记的 note
pub const PINNED_UNTOUCHED_NOTE: &str = "已手动锁定，未动";

/// [`borrow_now`] **单次调用**（跨槽共享，不是每槽）最多做几次带外验证。
///
/// 必须是调用级的：本函数按槽循环，每个「当前出口 == 故障 IP」的槽都会挨个试自己的候选，
/// 槽级上限 N 会让单次调用最坏做「池大小 − 1」次验证（`bui_schema::slots::MAX_SLOTS` = 8）。
///
/// 3 是「无可用出口 ≤25 秒」那条算式里的项。spec §5.7 两条处置延迟 SLA 的算式（首条 relay
/// 错误 → 事件，按真实数字算，2026-09-14 主会话裁决）：
/// - **有可用出口 ≤15 秒** = 等第 2 条错误 ≤5（串行最坏一次中继拨号超时）+ 哨兵轮询 ≤2 +
///   判原上游那次快探 ≤ [`health::QUICK_PROBE_BUDGET_SECS`] 4（整体预算，与这里的验证共用，
///   见 `health::probe_quick_within`）+ PUT + 首候选验证（健康候选经隧道一次 GET < 1）
///   ⇒ **≈ ≤12 秒**。首候选自己也「网关活着、隧道挂死」时那次验证吃满 4 秒、结论「未确认」，
///   落地是 15 秒整、事件说「已切到 Y，未能在 4 秒内确认可用」—— 没确认出口可用，那条路不是
///   「有可用出口」，不进这条算式（`sentinel::resi` 有一条用例走的就是它）；
/// - **无可用出口 ≤25 秒** = ≤5 + ≤2 + ≤4 + 3 ×（PUT + 验证 ≤4）+ 收尾 PUT（放回本槽）
///   ⇒ **≈ ≤24 秒**。「3 × 验证 ≤4」要取到三个**满额**预算，必须落在**三个不同的槽**上：同一个
///   槽只要有一次撞满预算（⇒ `Unconfirmed`）就 `break` 保留那个候选，往下试候选的 `Dead` 必然在
///   4 秒内返回，所以单槽只能逼近、取不到 3 × 4。方向是安全的（高估），这里写明以免后来人按它
///   反推单槽行为（2026-09-14 审查第 3 条）。
///
/// **PUT 按正常毫秒级计**：本机 Clash API（单次上限 `CLASH_TIMEOUT_SECS` = 2 秒），**Clash
/// 自己挂起不在这两条预算内**，那时算式不成立。次数也不是常数，它按**受影响的槽数**累加：8 个槽
/// 全压在故障 IP 上时，槽 0 最坏 4 次（3 次探死 + 预算耗尽那次）加 1 次放回，其余 7 槽各 1 次
/// ⇒ 约 12 次，毫秒级下合计仍 < 1 秒（2026-09-14 审查第 2 条）。
///
/// 预算用尽之后**不退回「保持现状」**（那会把槽留在已证死的上游上）：剩下的槽照样 PUT 到
/// 各自的最优候选，只是按 `unconfirmed` 口径报，由下一轮巡检用完整预算复核。
pub const BORROW_PROBES_PER_CALL: usize = 3;

/// 哨兵的立即借用（spec §5.7，设计裁决 D7）：`failed` 已被带外探测确认不可用（不可达 /
/// Google 被封），把**此刻正压在它身上**的槽立刻挪走，不等下一轮巡检。
///
/// 与 [`drive_slots`] 同一口径、只做「借出」这一半：
/// - 只动「当前出口 == `failed`」的槽：本槽 IP 就是它（`current_upstream_id = None` 时 selector
///   停在配置里的 default = 本槽 IP），或正借用它；
/// - 手动 pin 的槽不动（管理员的判断压过哨兵，同 [`drive_slots`] 规则 1），但压在 `failed` 上的
///   照样出现在结果里，note = [`PINNED_UNTOUCHED_NOTE`]（告警据此如实说明，不说成「没有槽经它出网」）；
/// - 候选顺序：本槽 IP 不是 `failed` 且健康、Google 未被封 ⇒ 先回本槽；其余按
///   [`health::rank_healthy`] 的排名；一个候选都没有 ⇒ 保持现状（fail-open）；
/// - **每切一条都当下验证**：`put_slot` 成功后对刚切过去的那条跑一次
///   [`health::probe_quick_within`]（与哨兵判「原上游坏了」**同一个**函数、同一个 `prober`、
///   同一个网关 TCP 时限 `tcp_within`、同一个整体预算
///   [`health::QUICK_PROBE_BUDGET_SECS`] 秒）。结论**三值**（[`health::Verdict`]）：
///   **确认可用** ⇒ 借到了（`switched`，行为与没有这次验证时逐字一致）；
///   **确认不可用** ⇒ 把**这一条**记 `mark_unhealthy` 并试下一个候选；
///   **预算内未能确认**（超时 / 探测任务异常）⇒ **保留这个候选**、不记不健康，`unconfirmed = true`
///   让告警如实说「未能确认」—— 本槽原来的出口是已知死的，**未知优于已知死**，而巡检下一轮会用
///   完整预算复核。**这一处的「未能确认」与判原上游那一处处置不同**：那边 relay 刚连报过错误、
///   可以拿「带外也过不去」当佐证按不可用处置，这条候选没人报过错，没有那层佐证（理由见
///   [`health::Verdict`]）。验证**逐条按 `host:port` 探，绝不按网关主机归组**：池里多条上游常常是同一个
///   网关的不同静态端口、每个端口一个出口 IP，「单个出口 IP 挂掉」是最常见的故障形态，此时借
///   兄弟端口正是「住宅 IP 连不通立刻分配新 IP」唯一可行的做法；归组会把整组判死、直接砸掉这个
///   功能。整网关挂掉时逐条探也只是每条在 TCP 超时（≤ `tcp_within`）返回，代价是多探几次而不是判错；
/// - 验证次数的上限是**单次调用**的、跨槽共享的（[`BORROW_PROBES_PER_CALL`]）；调用内缓存
///   「已证死」与「已确认可用」，同一条上游一次调用里只探一遍。预算用尽后剩下的槽照样 PUT 到
///   各自的最优候选，按 `unconfirmed` 报；
/// - 候选全不通（逐条都被证死）⇒ 把 selector **PUT 回本槽自己的上游**（`own`），恢复到事件前的
///   指向，`switched = false`、`no_exit = true`、`dead` 记下被证死的候选，调用方据此如实报
///   「当前无可用出口」。不补探 `own`（哨兵刚探过、巡检每轮还会探，补探换不到新信息只吃预算）；
///   连这次 PUT 都失败时 note 说出这一层，不吞掉；
/// - 候选被「本次调用已证死」剪空（前面的槽已经把它们逐条探死）⇒ 同样走上面那条「全不通」口径：
///   证据已经拿到了，不能让这个槽停在刚被证死的 `failed` 上。**区别于一开始就没有候选**
///   （runtime 里一个健康的都没有）：那时本次调用对这个槽的候选一无所知，维持 [`drive_slots`]
///   规则 4 的 fail-open「保持现状」，不拿没有证据的 PUT 去动路由；
/// - 被挪动的槽 `back_rounds` 归零；**不推进任何槽的 `back_rounds`，切回永远只由巡检的
///   [`drive_slots`] 负责**（连续 [`SLOT_BACK_ROUNDS`] 轮）。
///
/// 与 [`drive_slots`] 的判据**故意不对称**（切回要连续几轮确认、借用只要当下一次），不是一边严
/// 一边松，而是判错的代价形状不同：借用判错是**静默失败**（用户没网，事件 / `bui status` /
/// 面板三处都说已自愈，没人会去看），切回判错是**吵闹失败**（来回抖动，抖动本身就是信号）。
/// 一句话：**失败会静默的方向要即时证据，失败会吵闹的方向要持续证据。**
///
/// 「健康」= runtime 里 `active` 的成员（调用方先 `mark_unhealthy` 再调本函数），它只用来排候选
/// 顺序 —— 这张表来自上一轮巡检、最坏情况已经过期，所以「真的能用」一律由上面那次验证说话。
/// 与 [`drive_slots`] 共用 [`slot_switch`] 那把锁：不会落在巡检那一轮的读快照与写回之间。
pub async fn borrow_now(
    ctx: &DaemonCtx,
    prober: Arc<dyn Prober>,
    c: Arc<dyn Clash>,
    failed: Uuid,
    now: OffsetDateTime,
    tcp_within: Duration,
) -> Vec<SlotOutcome> {
    let _switch = slot_switch(ctx).lock_owned().await;
    let s = ctx.store.read().await;
    let view = slots::sorted(&s.residential);
    let g = state::group_of(&s);
    drop(s);
    let rt = state::read(&ctx.runtime).await;
    let healthy: Vec<Uuid> = g
        .upstreams
        .iter()
        .map(|u| u.id)
        .filter(|id| {
            *id != failed
                && rt
                    .health
                    .get(&id.to_string())
                    .map(|h| h.active)
                    .unwrap_or(true)
        })
        .collect();
    let ranked = health::rank_healthy(&g, &rt, &healthy, now);
    let google_ok =
        |id: Uuid| rt.health.get(&id.to_string()).and_then(|h| h.google_ok) != Some(false);
    let in_pool = |id: Uuid| g.upstreams.iter().any(|u| u.id == id);

    let mut out = Vec::new();
    let mut writes: Vec<(u16, Uuid)> = Vec::new();
    // 本次调用里已经当下验证过的候选：确认不通的记 `proven_dead`（后面的槽不必再白探一遍，
    // 收尾时一起 `mark_unhealthy`），确认可用的记 `proven_alive`（后面的槽直接用，不重复探）
    let mut proven_dead: BTreeSet<Uuid> = BTreeSet::new();
    let mut proven_alive: BTreeSet<Uuid> = BTreeSet::new();
    // 单次调用共享的验证预算（见 `BORROW_PROBES_PER_CALL`）
    let mut probes_left = BORROW_PROBES_PER_CALL;
    for sl in &view {
        if !in_pool(sl.upstream_id) {
            continue;
        }
        let sr = rt
            .slots
            .get(&sl.index.to_string())
            .cloned()
            .unwrap_or_default();
        let own = sl.upstream_id;
        let current = sr.current_upstream_id.unwrap_or(own);
        if current != failed {
            continue;
        }
        let stay = |note: String| SlotOutcome {
            index: sl.index,
            own,
            target: current,
            borrowed: current != own,
            switched: false,
            unconfirmed: false,
            no_exit: false,
            note: Some(note),
            dead: Vec::new(),
        };
        if sr.pinned_upstream_id.is_some_and(in_pool) {
            out.push(stay(PINNED_UNTOUCHED_NOTE.into()));
            continue;
        }
        // 本槽 IP 够好就排在最前，其余按排名；本次调用里已证死的剪掉（记进 `pruned`：
        // 剪空时按「全不通」口径收尾，不能把这个槽留在刚被证死的 `failed` 上）
        let mut cands: Vec<Upstream> = Vec::new();
        let mut pruned: Vec<Uuid> = Vec::new();
        let order = std::iter::once(own)
            .filter(|_| own != failed && healthy.contains(&own) && google_ok(own))
            .chain(ranked.iter().copied());
        for id in order {
            if cands.iter().any(|u| u.id == id) || pruned.contains(&id) {
                continue;
            }
            let Some(up) = g.upstreams.iter().find(|u| u.id == id) else {
                continue; // 槽位/排名与池并发变化，本轮跳过这条
            };
            if proven_dead.contains(&id) {
                pruned.push(id);
                continue;
            }
            cands.push(up.clone());
        }
        if cands.is_empty() && pruned.is_empty() {
            // 本次调用对这个槽的候选一无所知（runtime 里一个健康的都没有）⇒ fail-open
            out.push(stay("没有可借用的健康 IP，保持现状".into()));
            continue;
        }

        // 本次调用里已证死的候选：本槽亲自探死的，加上前面的槽探死、本槽据此跳过的
        let mut dead: Vec<Uuid> = pruned;
        let mut landed: Option<Uuid> = None; // 最后一次成功的 PUT：selector 此刻指着它
        let mut put_err: Option<String> = None;
        // 借到了：(目标, 没能确认可用的原因)
        let mut borrowed_to: Option<(Uuid, Option<String>)> = None;
        for up in &cands {
            match put_slot(c.clone(), &g, sl.index, up.id).await {
                Ok(tag) => {
                    landed = Some(up.id);
                    if proven_alive.contains(&up.id) {
                        tracing::warn!(slot = sl.index, to = %tag, "哨兵：本槽当前出口不可用，立即借用（本次调用已验证过这条）");
                        borrowed_to = Some((up.id, None));
                        break;
                    }
                    if probes_left == 0 {
                        // 预算用尽：仍然把槽切到最优候选（未知优于已知死），按「未确认」报
                        tracing::warn!(slot = sl.index, to = %tag, "哨兵：本次调用的验证预算已用尽，切过去但未确认");
                        borrowed_to = Some((
                            up.id,
                            Some(format!(
                                "本次调用的 {BORROW_PROBES_PER_CALL} 次验证预算已用尽，未确认可用"
                            )),
                        ));
                        break;
                    }
                    probes_left -= 1;
                    match health::probe_quick_within(&prober, up.clone(), tcp_within).await {
                        health::Verdict::Alive => {
                            tracing::warn!(slot = sl.index, to = %tag, "哨兵：本槽当前出口不可用，立即借用");
                            proven_alive.insert(up.id);
                            borrowed_to = Some((up.id, None));
                            break;
                        }
                        health::Verdict::Unconfirmed => {
                            tracing::warn!(slot = sl.index, to = %tag, "哨兵：借到的这条没能在预算内确认，保留它并如实报告");
                            borrowed_to = Some((
                                up.id,
                                Some(format!(
                                    "未能在 {} 秒内确认可用",
                                    health::QUICK_PROBE_BUDGET_SECS
                                )),
                            ));
                            break;
                        }
                        health::Verdict::Dead { .. } => {
                            tracing::warn!(slot = sl.index, cand = %tag, "哨兵：候选切过去也探不通，记不健康后试下一个");
                            dead.push(up.id);
                            proven_dead.insert(up.id);
                        }
                    }
                }
                Err(f) => {
                    put_err = Some(f.note());
                    break;
                }
            }
        }
        if let Some((target, unconfirmed)) = borrowed_to {
            writes.push((sl.index, target));
            out.push(SlotOutcome {
                index: sl.index,
                own,
                target,
                borrowed: target != own,
                switched: true,
                unconfirmed: unconfirmed.is_some(),
                no_exit: false,
                note: unconfirmed,
                dead,
            });
            continue;
        }
        if let (None, Some(e)) = (landed, &put_err) {
            // 第一个候选的 PUT 就没成：与验证无关，selector 没动过，按原样记一笔原因，runtime 不动
            let mut o = stay(e.clone());
            o.dead = dead;
            out.push(o);
            continue;
        }
        // 候选逐条被证死（或全被「本次已证死」剪掉）：selector 可能正停在一条死候选上。
        // 这个终态不可接受（路由指着一条我们自己挑的死路），放回本槽自己的上游；
        // `no_exit` 只在候选**试完了**的时候才置 —— 中途 PUT 失败停下来的那条路没有资格说
        // 「无可用出口」（剩下的候选压根没探过，很可能是好的）
        let no_exit = put_err.is_none();
        let mut note = put_err;
        let mut target = landed.unwrap_or(current);
        if target != own {
            match put_slot(c.clone(), &g, sl.index, own).await {
                Ok(_) => target = own,
                Err(f) => {
                    let back = format!("放回本槽 IP 也失败：{}", f.note());
                    note = Some(match note {
                        Some(e) => format!("{e}；{back}"),
                        None => back,
                    });
                }
            }
        }
        writes.push((sl.index, target));
        out.push(SlotOutcome {
            index: sl.index,
            own,
            target,
            borrowed: target != own,
            switched: false,
            unconfirmed: false,
            no_exit,
            note,
            dead,
        });
    }
    if !writes.is_empty() || !proven_dead.is_empty() {
        let unhealthy: Vec<Uuid> = proven_dead.into_iter().collect();
        state::update(&ctx.runtime, move |r| {
            for (index, target) in writes {
                let e = r.slots.entry(index.to_string()).or_default();
                e.current_upstream_id = Some(target);
                e.back_rounds = 0;
            }
            for id in unhealthy {
                state::mark_unhealthy(r.health.entry(id.to_string()).or_default(), now);
            }
        })
        .await;
    }
    out
}

/// 手动把一槽钉在某条上游上（`target = None` 解除）。钉住后立刻生效一次，
/// 之后 [`drive_slots`] 不再按「本槽优先 / 借用」把它挪走。
///
/// **解除时连 `current_upstream_id` 一起清零**：不清的话它还记着刚才钉住的那条
/// （≠ 本槽 IP），下一轮 [`drive_slots`] 会把这当成「正在借用」走规则 2 的防抖分支，
/// 要再攒满 [`SLOT_BACK_ROUNDS`] 轮才回本槽 —— 而管理员按下「解除」的语义是
/// 「马上交还给驱动器」。清零后下一轮 `current == None != Some(own)` ⇒ 无条件重 PUT 本槽。
pub async fn pin_slot(
    ctx: &DaemonCtx,
    c: Arc<dyn Clash>,
    index: u16,
    target: Option<Uuid>,
) -> anyhow::Result<()> {
    let s = ctx.store.read().await;
    let g = state::group_of(&s);
    anyhow::ensure!(
        s.residential.slots.iter().any(|x| x.index == index),
        "槽 {index} 不存在"
    );
    if let Some(t) = target {
        anyhow::ensure!(
            g.upstreams.iter().any(|u| u.id == t),
            "目标上游不在住宅池里"
        );
    }
    drop(s);
    if let Some(t) = target {
        let tag = clash::tag_of(&g, t).ok_or_else(|| anyhow::anyhow!("目标上游不在住宅池里"))?;
        let (cc, sel) = (c, super::slot_selector(index));
        tokio::task::spawn_blocking(move || cc.select(&sel, &tag)).await??;
    }
    state::update(&ctx.runtime, move |r| {
        let e = r.slots.entry(index.to_string()).or_default();
        e.pinned_upstream_id = target;
        // 钉住 ⇒ 当前选择就是它；解除 ⇒ 一并清零（见上面的文档注释）
        e.current_upstream_id = target;
        e.back_rounds = 0;
    })
    .await;
    tracing::info!(slot = index, pinned = ?target, "手动 pin 住宅槽位");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::EventBus;
    use crate::modules::panel::fakes::FakeXray;
    use crate::modules::residential::{state, MAX_UPSTREAMS};
    use crate::reconcile::DaemonCtx;
    use crate::state::store::Store;
    use crate::sys::fake::FakeHost;
    use bui_schema::model::{ResiMode, Upstream, UpstreamKind};
    use bui_schema::slots;
    use pretty_assertions::assert_eq;
    use uuid::Uuid;

    /// D5 的对偶断言：`bui` 侧的池上限必须等于 `bui-schema` 的槽位上限。
    #[test]
    fn the_pool_cap_and_the_slot_cap_are_the_same_number() {
        assert_eq!(MAX_UPSTREAMS, slots::MAX_SLOTS as usize);
    }

    fn upstream(i: u16) -> Upstream {
        Upstream {
            id: Uuid::from_u128(u128::from(i) + 1),
            name: format!("url-{}", i + 1),
            kind: UpstreamKind::Socks5,
            host: format!("isp{}.example.net", i + 1),
            port: 10007,
            username: "user1".into(),
            password: "pw1".into(),
            priority: 100,
            provider: None,
            region: None,
            ports_allowed: None,
            verified: None,
        }
    }

    /// 一个装了 n 条上游、m 个住宅用户（created_at 递增）的 Store。
    async fn store_with(dir: &std::path::Path, n: u16, m: u32) -> (Store, EventBus) {
        let mut st = crate::testutil::sample_state();
        let proto = st.users[0].clone();
        st.users = (0..m)
            .map(|i| {
                let mut u = proto.clone();
                u.user_id = Uuid::from_u128(0x1000 + u128::from(i));
                u.username = format!("u{}", i + 1);
                u.created_at = format!("2026-09-11T00:0{}:00Z", i);
                u
            })
            .collect();
        let g = st
            .residential
            .groups
            .get_mut(bui_schema::model::DEFAULT_GROUP)
            .unwrap();
        g.enabled = n > 0;
        g.mode = ResiMode::Global;
        g.upstreams = (0..n).map(upstream).collect();
        g.selected_upstream_id = g.upstreams.first().map(|u| u.id);
        (
            Store::create(dir.join("state.json"), st).await.unwrap(),
            EventBus::new(),
        )
    }

    /// `assign_user` / `rebalance_users` 都要在**自己那次写入的临界区里**算出必须重新拉
    /// 订阅的人（换槽 = 端口与跳跃段一起变），否则面板与 CLI 只会说「已重排 N 个用户」，
    /// 那 N 个人的住宅 HY2 会一直反复断联到他们各自刷新订阅为止。
    #[tokio::test]
    async fn assign_and_rebalance_report_who_must_refetch_the_subscription() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 2, 2).await;
        let (ctx, _host) = ctx_of(d.path(), store.clone(), bus.clone()).await;
        migrate_on_start(&store, &bus).await.unwrap(); // u1 → 槽 0，u2 → 槽 1
        let slot0 = slots::sorted(&store.read().await.residential)[0].upstream_id;
        let u2 = store.read().await.users[1].user_id;

        // assign：u2 槽 1 → 槽 0（40001 → 40000，跳跃段一起下移）
        let (ok, impact) = assign_user(&ctx, u2, slot0).await.unwrap();
        assert!(ok);
        assert_eq!(
            impact,
            slots::ResubscribeImpact {
                slot_removed: vec![],
                slot_moved: vec!["u2".to_string()],
                hop_resliced: vec![],
            }
        );

        // rebalance：把 u2 摊回槽 1，同一份口径
        let (moved, impact) = rebalance_users(&ctx).await.unwrap();
        assert_eq!(moved, 1);
        assert_eq!(impact.slot_moved, vec!["u2".to_string()]);
        assert_eq!(impact.total(), 1);

        // 零改动 ⇒ 空名单
        let (moved, impact) = rebalance_users(&ctx).await.unwrap();
        assert_eq!(moved, 0);
        assert!(impact.is_empty());
    }

    #[tokio::test]
    async fn migrate_on_start_fills_in_slots_and_assignments_once() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 3, 5).await;
        // 旧 state：既没有 slots，也没有任何 slot_id
        assert!(store.read().await.residential.slots.is_empty());

        assert_eq!(migrate_on_start(&store, &bus).await.unwrap(), 5);
        let s = store.read().await;
        assert_eq!(slots::indices(&s.residential), vec![0, 1, 2]);
        let mut load = vec![0usize; 3];
        for u in &s.users {
            load[slots::index_of_user(u, &s.residential) as usize] += 1;
        }
        assert_eq!(load, vec![2, 2, 1], "spec §5.6 规则 4");

        // 幂等：第二次启动不再改动任何东西（也就不会多写一次 state.json）
        assert_eq!(migrate_on_start(&store, &bus).await.unwrap(), 0);
    }

    /// 启动迁移给每个住宅 hysteria2 用户发凭据（迁移口径：`name = 用户名`、
    /// `secret = 当时的 hy2_password` ⇒ 升级零刷新订阅），并把池补到 `size_for`；幂等
    #[tokio::test]
    async fn the_pool_migration_mints_a_cred_per_residential_user_and_is_idempotent() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 1, 3).await;
        let (ctx, _host) = ctx_of(d.path(), store.clone(), bus.clone()).await;
        let r = migrate_hy2_pool_on_start(&ctx).await.unwrap();
        assert_eq!(
            r,
            PoolMigration {
                healed: 0,
                changed: 3,
                unassigned: 0
            }
        );
        let s = store.read().await;
        assert_eq!(
            s.residential.hy2_pool.creds.len(),
            bui_schema::hy2pool::POOL_MIN
        );
        for u in &s.users {
            let c = bui_schema::hy2pool::cred_of(u, &s.residential)
                .unwrap_or_else(|| panic!("{} 没拿到凭据", u.username));
            assert_eq!(c.name, u.username, "迁移用户的 name 就是用户名");
            assert_eq!(c.secret, u.credentials.hy2_password);
            // 有凭据才渲染得出住宅 HY2 节点（T7 的 `nodes_for`）
            assert!(
                bui_schema::nodes::nodes_for(u, &s.node, &s.residential)
                    .iter()
                    .any(|n| n.kind == bui_schema::nodes::NodeKind::Hy2Residential),
                "{} 的订阅里没有住宅 HY2 节点",
                u.username
            );
        }
        drop(s);
        assert_eq!(
            migrate_hy2_pool_on_start(&ctx).await.unwrap(),
            PoolMigration::default(),
            "幂等：零变更"
        );
    }

    /// 「凭据指针悬空」必须被治（第一波复核发现）：`hy2_resi_cred` 指向池里已不存在的 id
    /// 时，`hy2pool::assign` 原样还回那个 id、`cred_of` 却是 `None`，而 `migrate` 判 pending
    /// 用 `is_none()` ⇒ 这类用户既不被治愈也不进 `unassigned`，表现为「有权益但订阅里渲染
    /// 不出住宅 HY2 节点」且零告警。
    #[tokio::test]
    async fn a_dangling_cred_pointer_is_healed_and_reported() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 1, 2).await;
        let (ctx, _host) = ctx_of(d.path(), store.clone(), bus.clone()).await;
        migrate_hy2_pool_on_start(&ctx).await.unwrap();
        // 人工把 u1 的指针指到池外（人工改 state / 回滚后再升级 / 池被缩过）
        store
            .update(|s| {
                s.users[0].credentials.hy2_resi_cred = Some("r999".into());
            })
            .await
            .unwrap();
        {
            let s = store.read().await;
            assert!(
                bui_schema::hy2pool::cred_of(&s.users[0], &s.residential).is_none(),
                "前提：悬空指针拿不到凭据"
            );
        }

        let r = migrate_hy2_pool_on_start(&ctx).await.unwrap();
        assert_eq!((r.healed, r.changed, r.unassigned), (1, 1, 0));
        let s = store.read().await;
        let c = bui_schema::hy2pool::cred_of(&s.users[0], &s.residential)
            .expect("治愈后必须拿到池里真实存在的凭据");
        assert_ne!(c.id, "r999");
        assert!(
            bui_schema::nodes::nodes_for(&s.users[0], &s.node, &s.residential)
                .iter()
                .any(|n| n.kind == bui_schema::nodes::NodeKind::Hy2Residential),
            "治愈后订阅里要有住宅 HY2 节点"
        );
        let inc = crate::modules::sentinel::incidents::from_runtime(&ctx.runtime.read().await);
        let warn = inc
            .iter()
            .find(|i| i.signature == CRED_DANGLING_SIG)
            .expect("悬空指针必须留一条 Warn 级事件");
        assert_eq!(warn.level, Level::Warn);
        assert_eq!(warn.subject, "u1", "事件只写用户名与个数");
        assert!(
            !warn.result.contains(&c.secret) && !warn.subject.contains(&c.secret),
            "事件里一个凭据字节都不许有"
        );
    }

    /// 空闲凭据全在 24 小时冷却期内 ⇒ 建用户**不被拒**：当场扩容再分（spec §3.1）。
    /// 少了这条兜底，池的常规目标容量早已达标（`size_for` 不补一条），新用户会静默地
    /// 一直没有凭据、订阅里永远少一个节点。
    #[tokio::test]
    async fn a_new_user_still_gets_a_cred_when_every_idle_cred_is_cooling_down() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 1, 1).await;
        let (ctx, host) = ctx_of(d.path(), store.clone(), bus.clone()).await;
        migrate_hy2_pool_on_start(&ctx).await.unwrap();
        let before = store.read().await.residential.hy2_pool.creds.len();
        let newbie = Uuid::from_u128(0x2000);
        // 盖章与判定用**同一个注入时钟**：墙钟会让这一格测不到（假时钟 2026-09-11 配真实
        // now 之差早就超过 24 小时，冷却期直接被绕开）
        let stamp = fmt_rfc3339(host.now());
        host.advance(3600);
        let now = host.now();
        store
            .update(|s| {
                // 全部空闲凭据都「1 小时前释放」⇒ 一条都不许发出去
                let used: BTreeSet<String> = s
                    .users
                    .iter()
                    .filter_map(|u| u.credentials.hy2_resi_cred.clone())
                    .collect();
                for c in s
                    .residential
                    .hy2_pool
                    .creds
                    .iter_mut()
                    .filter(|c| !used.contains(&c.id))
                {
                    c.released_at = Some(stamp.clone());
                }
                let mut u = s.users[0].clone();
                u.user_id = newbie;
                u.username = "newbie".into();
                u.credentials.hy2_resi_cred = None;
                s.users.push(u);
                assign_new_user(s, newbie, now);
            })
            .await
            .unwrap();
        let s = store.read().await;
        let u = s.users.iter().find(|u| u.user_id == newbie).unwrap();
        assert!(
            bui_schema::hy2pool::cred_of(u, &s.residential).is_some(),
            "冷却期内耗尽也不许让新用户没有凭据"
        );
        assert!(
            s.residential.hy2_pool.creds.len() > before,
            "当场扩容过：{} → {}",
            before,
            s.residential.hy2_pool.creds.len()
        );
    }

    /// id 域用尽 ⇒ Error 级事件（spec §3.1：不静默吞掉「没拿到凭据」）
    #[tokio::test]
    async fn an_exhausted_id_space_is_reported_as_an_error() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 1, 1).await;
        let (ctx, _host) = ctx_of(d.path(), store.clone(), bus.clone()).await;
        // 池已满到上限、全部空闲凭据都在 24 小时冷却期内 ⇒ 谁都分不到
        store
            .update(|s| {
                s.residential.hy2_pool.creds = (0..bui_schema::hy2pool::POOL_MAX)
                    .map(|i| bui_schema::model::ReservedCred {
                        id: format!("r{i:03}"),
                        name: format!("r{i:03}"),
                        secret: "x".repeat(22),
                        released_at: Some("2099-01-01T00:00:00Z".into()),
                    })
                    .collect();
            })
            .await
            .unwrap();

        let r = migrate_hy2_pool_on_start(&ctx).await.unwrap();
        assert_eq!((r.healed, r.changed, r.unassigned), (0, 0, 1));
        let inc = crate::modules::sentinel::incidents::from_runtime(&ctx.runtime.read().await);
        let err = inc
            .iter()
            .find(|i| i.signature == POOL_EXHAUSTED_SIG)
            .expect("池耗尽必须留一条 Error 级事件");
        assert_eq!(err.level, Level::Error);
    }

    /// spec §3.1 的第三道防线：`hy2-residential.json` 这一轮本来就要重写（§3.5 四件事之一，
    /// 这里用池扩容）⇒ 落盘前把**空闲**凭据的 secret 重随机、`released_at` 清掉；
    /// **在用的那一条一个字节都不许动**（动了就是把在线用户踢下线）。
    ///
    /// 不接上这道防线的话，被 `release` 掉的凭据 secret 终生不变：24 小时冷却期一过，
    /// 同 id、同 secret 原样发给下一个人，前任持有人手里的旧订阅直接连上新人的门。
    /// 反过来，**文件不变的那一轮一个字节都不许动**：否则每轮对账都会因为自己刚换的
    /// secret 重写文件 + 重启住宅内核，全体在线连接跟着断。
    #[tokio::test]
    async fn a_rewrite_of_the_residential_config_rerolls_only_the_idle_secrets() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 1, 1).await;
        let (ctx, host) = ctx_of(d.path(), store.clone(), bus.clone()).await;
        migrate_hy2_pool_on_start(&ctx).await.unwrap();
        let path = crate::modules::core_files::hy2_resi_config_path(&ctx.paths);
        let secrets = |s: &State| -> Vec<(String, String, Option<String>)> {
            s.residential
                .hy2_pool
                .creds
                .iter()
                .map(|c| (c.id.clone(), c.secret.clone(), c.released_at.clone()))
                .collect()
        };
        // 一条空闲凭据「已经被释放过」——它正是这道防线要保护的那一类
        let cooled = store.read().await.residential.hy2_pool.creds[1].id.clone();
        let stamp = fmt_rfc3339(host.now());
        store
            .update(|s| {
                if let Some(c) = s
                    .residential
                    .hy2_pool
                    .creds
                    .iter_mut()
                    .find(|c| c.id == cooled)
                {
                    c.released_at = Some(stamp.clone());
                }
            })
            .await
            .unwrap();
        // 盘上那份 = 当前期望态 ⇒ 这一轮不会重写它
        {
            let s = store.read().await;
            let bytes = serde_json::to_vec_pretty(&bui_schema::render::hy2_singbox::config(
                &s.node,
                &ctx.paths,
                &s.residential.hy2_pool,
            ))
            .unwrap();
            host.write_file(&path, &bytes, 0o600).unwrap();
        }
        let before = secrets(&store.read().await.clone());
        assert_eq!(
            reroll_idle_hy2_secrets(&ctx).await,
            0,
            "文件不变的那一轮一个字节都不许动，否则每轮对账都重写 + 重启住宅内核"
        );
        assert_eq!(secrets(&store.read().await.clone()), before, "零变更");

        // 池扩容（spec §3.5 四件事之一）⇒ 渲染结果与盘上不一致，这一轮本来就要重写
        store
            .update(|s| {
                s.residential
                    .hy2_pool
                    .creds
                    .push(bui_schema::model::ReservedCred {
                        id: "r099".into(),
                        name: "r099".into(),
                        secret: "x".repeat(22),
                        released_at: None,
                    });
            })
            .await
            .unwrap();
        let used = store.read().await.users[0]
            .credentials
            .hy2_resi_cred
            .clone()
            .expect("前提：u1 占着一条");
        let n = reroll_idle_hy2_secrets(&ctx).await;
        let after = secrets(&store.read().await.clone());
        assert_eq!(n, after.len() - 1, "除了在用那一条，全部空闲凭据都要换");
        for (id, secret, _) in &before {
            let now = after
                .iter()
                .find(|(i, _, _)| i == id)
                .expect("凭据不会消失");
            if *id == used {
                assert_eq!(&now.1, secret, "在用的凭据 secret 一个字节都不许动");
            } else {
                assert_ne!(&now.1, secret, "空闲凭据 {id} 的 secret 没换");
            }
        }
        let cooled_after = after.iter().find(|(i, _, _)| *i == cooled).unwrap();
        assert!(
            cooled_after.2.is_none(),
            "换了 secret 就顺带清 released_at（旧密码已经无效，不必再压着冷却期）"
        );
    }

    /// 24 小时冷却期的判定时钟必须是**注入的** `Host::now()` —— 与盖 `released_at` 的那个
    /// 同源。用墙钟的话：假时钟盖的 `released_at` 配真实 `now_utc()` 之差早就超过 24 小时，
    /// 刚释放的凭据当场就能再分配出去（前任的旧订阅直接连上新人的门），而且测不到。
    #[tokio::test]
    async fn the_reclaim_cooldown_is_judged_by_the_injected_clock() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 1, 1).await;
        let (ctx, host) = ctx_of(d.path(), store.clone(), bus.clone()).await;
        let _ = &ctx;
        let stamp = fmt_rfc3339(host.now());
        let newbie = Uuid::from_u128(0x2000);
        // 池满到 id 域上限、除 u1 那条之外全部「此刻刚释放」⇒ 扩容也补不出新的，
        // 能不能分出去完全由冷却期这一条判据决定
        store
            .update(|s| {
                s.residential.hy2_pool.creds = (0..bui_schema::hy2pool::POOL_MAX)
                    .map(|i| bui_schema::model::ReservedCred {
                        id: format!("r{i:03}"),
                        name: format!("r{i:03}"),
                        secret: "x".repeat(22),
                        released_at: (i > 0).then(|| stamp.clone()),
                    })
                    .collect();
                s.users[0].credentials.hy2_resi_cred = Some("r000".into());
                let mut u = s.users[0].clone();
                u.user_id = newbie;
                u.username = "newbie".into();
                u.credentials.hy2_resi_cred = None;
                s.users.push(u);
            })
            .await
            .unwrap();

        host.advance(23 * 3600);
        let at23 = host.now();
        store
            .update(|s| {
                assign_hy2_cred(s, newbie, at23);
            })
            .await
            .unwrap();
        assert_eq!(
            store.read().await.users[1].credentials.hy2_resi_cred,
            None,
            "23 小时 < 24 小时冷却期 ⇒ 一条都不许发（墙钟判定会在这里放行）"
        );

        host.advance(2 * 3600);
        let at25 = host.now();
        store
            .update(|s| {
                assign_hy2_cred(s, newbie, at25);
            })
            .await
            .unwrap();
        assert_eq!(
            store.read().await.users[1]
                .credentials
                .hy2_resi_cred
                .as_deref(),
            Some("r001"),
            "过了 24 小时才可再分配，取 released_at 最早的那条"
        );
    }

    /// 新建用户同一次写盘里既分槽、也分凭据（否则他的订阅里从一开始就少一个节点）
    #[tokio::test]
    async fn a_new_user_gets_a_slot_and_a_cred_in_the_same_write() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 2, 1).await;
        let (ctx, host) = ctx_of(d.path(), store.clone(), bus.clone()).await;
        migrate_on_start(&store, &bus).await.unwrap();
        migrate_hy2_pool_on_start(&ctx).await.unwrap();
        let newbie = Uuid::from_u128(0x2000);
        let now = host.now();
        store
            .update(|s| {
                let mut u = s.users[0].clone();
                u.user_id = newbie;
                u.username = "newbie".into();
                u.credentials.hy2_resi_cred = None;
                u.entitlements.residential.as_mut().unwrap().slot_id = None;
                s.users.push(u);
                assign_new_user(s, newbie, now);
            })
            .await
            .unwrap();
        let s = store.read().await;
        let u = s.users.iter().find(|u| u.user_id == newbie).unwrap();
        assert!(slots::slot_id_of_user(u).is_some(), "分到了槽");
        let c = bui_schema::hy2pool::cred_of(u, &s.residential).expect("分到了凭据");
        assert_eq!(c.name, c.id, "新发凭据的 name = id（spec §3.1）");
        assert_ne!(
            c.secret, u.credentials.hy2_password,
            "住宅凭据与直连密码各走各的"
        );
        assert!(
            bui_schema::nodes::nodes_for(u, &s.node, &s.residential)
                .iter()
                .any(|n| n.kind == bui_schema::nodes::NodeKind::Hy2Residential),
            "新用户的订阅里要有住宅 HY2 节点"
        );
    }

    #[tokio::test]
    async fn adding_an_upstream_creates_its_slot_without_moving_anyone() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 2, 4).await;
        migrate_on_start(&store, &bus).await.unwrap();
        let before: Vec<Option<Uuid>> = store
            .read()
            .await
            .users
            .iter()
            .map(slots::slot_id_of_user)
            .collect();
        let sync =
            update_group_slots_as(&store, &bus, crate::state::store::CALLER_UNLABELED, |g| {
                g.upstreams.push(upstream(2))
            })
            .await
            .unwrap();
        assert!(sync.released.is_empty());
        assert_eq!(sync.reassigned, 0, "规则 2：新增上游不搬既有用户");
        let s = store.read().await;
        assert_eq!(slots::indices(&s.residential), vec![0, 1, 2]);
        assert_eq!(
            s.users
                .iter()
                .map(slots::slot_id_of_user)
                .collect::<Vec<_>>(),
            before
        );
    }

    #[tokio::test]
    async fn removing_an_upstream_releases_its_slot_and_reassigns_its_users() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 3, 5).await;
        migrate_on_start(&store, &bus).await.unwrap();
        let gone = Uuid::from_u128(3); // 槽 2，上面是 u3
        let sync = update_group_slots_as(
            &store,
            &bus,
            crate::state::store::CALLER_RESI_REMOVE,
            move |g| g.upstreams.retain(|u| u.id != gone),
        )
        .await
        .unwrap();
        assert_eq!(
            sync.released.iter().map(|s| s.index).collect::<Vec<_>>(),
            vec![2]
        );
        assert_eq!(sync.reassigned, 1, "只有那个槽的用户被重分配");
        assert_eq!(
            sync.impact,
            slots::ResubscribeImpact {
                slot_removed: vec!["u3".to_string()],
                slot_moved: vec![],
                hop_resliced: vec!["u2".to_string(), "u5".to_string()],
            },
            "名单与这次写入出自同一个临界区，而且按原因分组：u3 的槽被删（40002 → 40000）\
             = 组一，u2 / u5 的跳跃区间被重切（44000-46999 → 45500-50000）= 组三；\
             槽 0 的 u1 / u4 旧区间仍是新区间的前缀，一组都不进"
        );
        let s = store.read().await;
        assert_eq!(slots::indices(&s.residential), vec![0, 1]);
        assert!(
            s.users
                .iter()
                .all(|u| slots::slot_id_of_user(u).is_some_and(|id| id != gone)),
            "没人还指着被删掉的槽"
        );
    }

    #[tokio::test]
    async fn a_slot_runtime_entry_survives_a_runtime_round_trip() {
        let d = tempfile::tempdir().unwrap();
        let rt = crate::state::runtime::Runtime::load(d.path().join("runtime.json"));
        state::update(&rt, |r| {
            r.slots.insert(
                "1".into(),
                state::SlotRuntime {
                    current_upstream_id: Some(Uuid::from_u128(2)),
                    pinned_upstream_id: None,
                    back_rounds: 2,
                },
            );
            r.xray_slot_rules_dirty = true;
            r.xray_slot_rules_hash = Some("deadbeef".into());
        })
        .await;
        let back = crate::state::runtime::Runtime::load(d.path().join("runtime.json"));
        let r = state::read(&back).await;
        assert_eq!(r.slots["1"].back_rounds, 2);
        assert_eq!(r.slots["1"].current_upstream_id, Some(Uuid::from_u128(2)));
        assert!(r.xray_slot_rules_dirty);
        assert_eq!(r.xray_slot_rules_hash.as_deref(), Some("deadbeef"));
    }

    // ── D7：Xray 槽路由的 gRPC 收敛 ────────────────────────────────────────────

    async fn ctx_of(
        dir: &std::path::Path,
        store: Store,
        bus: EventBus,
    ) -> (DaemonCtx, std::sync::Arc<FakeHost>) {
        let host = std::sync::Arc::new(FakeHost::new());
        (
            DaemonCtx {
                store,
                runtime: crate::state::runtime::Runtime::load(dir.join("runtime.json")),
                bus,
                host: host.clone(),
                paths: bui_schema::paths::Paths::default_server(),
            },
            host,
        )
    }

    /// 期望态的 `(ruleTag, 出站)` 表，末尾一定是兜底那条。
    async fn want_rules(ctx: &DaemonCtx) -> Vec<(String, String)> {
        let s = ctx.store.read().await;
        bui_schema::render::xray::slot_rules(&s.users, &s.residential)
            .into_iter()
            .map(|r| (r.rule_tag, r.outbound_tag))
            .collect()
    }

    /// 把**当前期望态**渲染出的 xray 配置写进 FakeHost 的假文件系统，
    /// 模拟「对账已经把新的 `xray-config.json` 落盘了」（只有退回重启那条路要它）。
    async fn land_xray_config(ctx: &DaemonCtx, host: &FakeHost) {
        let s = ctx.store.read().await;
        let cfg = bui_schema::render::xray::config(&s.node, &s.users, &s.residential, &ctx.paths);
        let bytes = serde_json::to_vec_pretty(&cfg).unwrap();
        let path = ctx.paths.base_dir.join("xray-config.json");
        host.with(|i| {
            i.files.insert(path, (bytes, 0o600));
        });
    }

    fn restarts_of_xray(host: &FakeHost) -> usize {
        host.ops()
            .iter()
            .filter(|o| *o == "systemd:restart:xray.service")
            .count()
    }

    /// 「表内容一致 + 兜底在表尾」。用户规则**彼此之间的先后顺序无所谓** —— 一个 email 只
    /// 出现在一条规则里，谁先谁后都匹配同一结果；而删+加会把被改的那条挪到表尾，所以
    /// 这里比集合、只对兜底那条断言位置（它没有 `user`，排在谁前面就会把谁抢掉）。
    fn assert_same_rules(live: &[(String, String)], want: &[(String, String)]) {
        use std::collections::BTreeSet;
        assert_eq!(
            live.iter().cloned().collect::<BTreeSet<_>>(),
            want.iter().cloned().collect::<BTreeSet<_>>(),
            "规则表内容与期望态不一致"
        );
        assert_eq!(
            live.last().map(|(tag, _)| tag.as_str()),
            Some("resi-fallback"),
            "兜底必须在表尾：{live:?}"
        );
    }

    /// 第二个槽的槽位键（上游 uuid）。
    async fn second_slot_id(ctx: &DaemonCtx) -> Uuid {
        let s = ctx.store.read().await;
        slots::sorted(&s.residential)[1].upstream_id
    }

    #[tokio::test]
    async fn converge_xray_is_a_no_op_when_nothing_is_dirty() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 2, 2).await;
        let (ctx, host) = ctx_of(d.path(), store, bus).await;
        migrate_on_start(&ctx.store, &ctx.bus).await.unwrap();
        state::update(&ctx.runtime, |r| r.xray_slot_rules_dirty = false).await;
        let x = FakeXray::new();
        assert_eq!(converge_xray(&ctx, &x).await, ConvergeOutcome::Clean);
        assert!(
            x.calls().is_empty(),
            "干净时连 ListRule 都不该发：{:?}",
            x.calls()
        );
        assert_eq!(restarts_of_xray(&host), 0);
    }

    #[tokio::test]
    async fn converge_xray_installs_one_rule_per_user_without_restarting() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 2, 2).await;
        let (ctx, host) = ctx_of(d.path(), store, bus).await;
        migrate_on_start(&ctx.store, &ctx.bus).await.unwrap();
        mark_xray_rules_dirty(&ctx.runtime).await;
        let x = FakeXray::new();

        let n = match converge_xray(&ctx, &x).await {
            ConvergeOutcome::Applied(n) => n,
            other => panic!("期望 Applied，实际 {other:?}"),
        };
        assert!(n > 0);
        assert_same_rules(&x.rules(), &want_rules(&ctx).await);
        assert_eq!(
            restarts_of_xray(&host),
            0,
            "槽路由靠 gRPC 生效，**不许**重启 xray（M3 判据①）：{:?}",
            host.ops()
        );
        let r = state::read(&ctx.runtime).await;
        assert!(!r.xray_slot_rules_dirty, "收敛成功后清脏");
        assert!(r.xray_slot_rules_hash.is_some(), "记下已收敛的槽路由哈希");
        // 幂等：再置一次脏也只会 ListRule 一次、什么都不改
        x.clear_calls();
        mark_xray_rules_dirty(&ctx.runtime).await;
        assert_eq!(converge_xray(&ctx, &x).await, ConvergeOutcome::Clean);
        assert_eq!(
            x.calls(),
            Vec::<String>::new(),
            "哈希没变 ⇒ 清脏不动（D7 第 2 步）"
        );
    }

    /// **裁决的核心**：改一个人的槽位只碰他那一条规则（外加把兜底挪回表尾），
    /// 邻居的规则一个字都不动，xray 不重启。
    #[tokio::test]
    async fn assigning_a_user_touches_only_his_own_rule() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 2, 2).await;
        let (ctx, host) = ctx_of(d.path(), store, bus).await;
        migrate_on_start(&ctx.store, &ctx.bus).await.unwrap();
        mark_xray_rules_dirty(&ctx.runtime).await;
        let x = FakeXray::new();
        converge_xray(&ctx, &x).await;

        let (moved, other) = {
            let s = ctx.store.read().await;
            (s.users[0].user_id, s.users[1].user_id)
        };
        let slot1 = second_slot_id(&ctx).await;
        assert!(assign_user(&ctx, moved, slot1).await.unwrap().0);
        x.clear_calls();
        assert!(matches!(
            converge_xray(&ctx, &x).await,
            ConvergeOutcome::Applied(_)
        ));
        assert_eq!(
            x.calls(),
            vec![
                "list-rules".to_string(),
                format!("remove-rule:resi-u-{moved}"),
                format!("add-rule:resi-u-{moved}:relay-slot-1"),
                "remove-rule:resi-fallback".to_string(),
                "add-rule:resi-fallback:relay-slot-0".to_string(),
            ],
            "先删再加（重名会让整条 AddRule 报错），最后把兜底挪回表尾"
        );
        assert!(
            !x.calls().iter().any(|c| c.contains(&other.to_string())),
            "邻居的规则不许动：{:?}",
            x.calls()
        );
        assert_same_rules(&x.rules(), &want_rules(&ctx).await);
        assert_eq!(restarts_of_xray(&host), 0, "{:?}", host.ops());
    }

    /// 上一轮「把兜底挪回表尾」那步失败留下的错序（兜底排在用户规则中间）：
    /// 内容一模一样，但 Xray 首条匹配会把后面那些用户吃到兜底槽。差分必须看见这个差异，
    /// 把兜底挪回表尾，而且**不重启** xray。
    #[tokio::test]
    async fn a_fallback_stuck_in_the_middle_is_moved_back_to_the_tail() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 2, 2).await;
        let (ctx, host) = ctx_of(d.path(), store, bus).await;
        migrate_on_start(&ctx.store, &ctx.bus).await.unwrap();
        mark_xray_rules_dirty(&ctx.runtime).await;
        let x = FakeXray::new();
        // 期望态的全部规则都在表里，只是兜底被塞在了第一条（内容一致、顺序错）
        let want = want_rules(&ctx).await;
        let mut live = vec![want.last().unwrap().clone()];
        live.extend(want.iter().take(want.len() - 1).cloned());
        x.with(|i| i.rules = live);

        assert!(matches!(
            converge_xray(&ctx, &x).await,
            ConvergeOutcome::Applied(_)
        ));
        assert_eq!(
            x.calls(),
            vec![
                "list-rules".to_string(),
                "remove-rule:resi-fallback".to_string(),
                "add-rule:resi-fallback:relay-slot-0".to_string(),
            ],
            "只挪兜底，用户规则一条都不碰"
        );
        assert_same_rules(&x.rules(), &want);
        assert_eq!(restarts_of_xray(&host), 0, "{:?}", host.ops());
        // 收敛之后才记哈希：再置脏一轮就什么都不发了
        x.clear_calls();
        mark_xray_rules_dirty(&ctx.runtime).await;
        assert_eq!(converge_xray(&ctx, &x).await, ConvergeOutcome::Clean);
        assert_eq!(x.calls(), Vec::<String>::new());
    }

    /// 删用户只发一次 `RemoveRule`，连兜底都不用动（删规则不会打乱顺序）。
    #[tokio::test]
    async fn deleting_a_user_only_removes_his_rule() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 2, 2).await;
        let (ctx, host) = ctx_of(d.path(), store, bus).await;
        migrate_on_start(&ctx.store, &ctx.bus).await.unwrap();
        mark_xray_rules_dirty(&ctx.runtime).await;
        let x = FakeXray::new();
        converge_xray(&ctx, &x).await;

        let gone = ctx.store.read().await.users[0].user_id;
        ctx.store
            .update(|s| s.users.retain(|u| u.user_id != gone))
            .await
            .unwrap();
        mark_xray_rules_dirty(&ctx.runtime).await;
        x.clear_calls();
        assert!(matches!(
            converge_xray(&ctx, &x).await,
            ConvergeOutcome::Applied(_)
        ));
        assert_eq!(
            x.calls(),
            vec![
                "list-rules".to_string(),
                format!("remove-rule:resi-u-{gone}")
            ]
        );
        assert_same_rules(&x.rules(), &want_rules(&ctx).await);
        assert_eq!(restarts_of_xray(&host), 0);
    }

    /// gRPC 挂了（xray 没在听 / RoutingService 没开）且**新配置已落盘** ⇒
    /// 退回一次 restart（重启会把磁盘上那份完整规则表读回来），记一条面板可见的事件，
    /// 并且**只退一次**：清脏之后不会每 10 分钟再重启一遍。
    #[tokio::test]
    async fn a_grpc_failure_falls_back_to_exactly_one_restart_and_records_an_event() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 2, 2).await;
        let (ctx, host) = ctx_of(d.path(), store, bus).await;
        migrate_on_start(&ctx.store, &ctx.bus).await.unwrap();
        land_xray_config(&ctx, &host).await;
        mark_xray_rules_dirty(&ctx.runtime).await;
        let x = FakeXray::new();
        x.with(|i| {
            i.fail_on.insert("list-rules".into());
        });

        assert_eq!(converge_xray(&ctx, &x).await, ConvergeOutcome::Restarted);
        assert_eq!(
            x.calls(),
            vec!["list-rules".to_string()],
            "非连接类错误（规则非法等）不重试，直接走退路"
        );
        assert_eq!(restarts_of_xray(&host), 1, "{:?}", host.ops());
        let r = state::read(&ctx.runtime).await;
        assert!(!r.xray_slot_rules_dirty);
        assert!(r.xray_slot_rules_hash.is_some());
        assert!(
            r.alerts.iter().any(|a| a.contains("槽路由")),
            "面板要看得见这次退回：{:?}",
            r.alerts
        );
        // 再来一轮：已经不脏了 ⇒ 不再重启（「退回一次」的含义）
        assert_eq!(converge_xray(&ctx, &x).await, ConvergeOutcome::Clean);
        assert_eq!(restarts_of_xray(&host), 1);
    }

    /// gRPC 挂了、而且磁盘上还是旧配置（对账在 500ms 之后，或全新装机还没跑过第一轮）：
    /// **不重启、不清脏**。这时重启只会把旧规则重新加载一遍，脏标记一旦被清掉，
    /// 新槽路由就要等下一次别的脏事件才生效。
    #[tokio::test]
    async fn a_grpc_failure_defers_when_the_config_file_has_not_landed() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 2, 2).await;
        let (ctx, host) = ctx_of(d.path(), store, bus).await;
        migrate_on_start(&ctx.store, &ctx.bus).await.unwrap();
        // 先落一份「分槽变更之前」的配置，再改分槽 ⇒ 磁盘哈希 != 期望哈希
        land_xray_config(&ctx, &host).await;
        let uid = ctx.store.read().await.users[0].user_id;
        let slot1 = second_slot_id(&ctx).await;
        assert!(assign_user(&ctx, uid, slot1).await.unwrap().0);
        let x = FakeXray::new();
        x.with(|i| {
            i.fail_on.insert("list-rules".into());
        });

        assert_eq!(converge_xray(&ctx, &x).await, ConvergeOutcome::Deferred);
        assert_eq!(restarts_of_xray(&host), 0, "{:?}", host.ops());
        assert!(
            state::read(&ctx.runtime).await.xray_slot_rules_dirty,
            "文件没落地时**绝不**清脏标记，否则这台机器的槽路由永远不收敛"
        );
        // 对账把新文件写下去之后（serve.rs 的每轮收口），同一条路才走到重启
        land_xray_config(&ctx, &host).await;
        assert_eq!(converge_xray(&ctx, &x).await, ConvergeOutcome::Restarted);
        assert_eq!(restarts_of_xray(&host), 1);
    }

    #[tokio::test]
    async fn a_failed_restart_keeps_the_dirty_flag_for_the_next_round() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 2, 1).await;
        let (ctx, host) = ctx_of(d.path(), store, bus).await;
        migrate_on_start(&ctx.store, &ctx.bus).await.unwrap();
        land_xray_config(&ctx, &host).await;
        // FakeHost 的 systemd 失败注入走 `fail_units`（裸名或全名都认，见 sys/fake.rs）
        host.with(|i| {
            i.fail_units.insert("xray".into());
        });
        mark_xray_rules_dirty(&ctx.runtime).await;
        let x = FakeXray::new();
        x.with(|i| {
            i.fail_on.insert("list-rules".into());
        });
        assert_eq!(converge_xray(&ctx, &x).await, ConvergeOutcome::Deferred);
        assert_eq!(restarts_of_xray(&host), 1, "重启确实发了一次，只是失败了");
        assert!(
            state::read(&ctx.runtime).await.xray_slot_rules_dirty,
            "重启失败不许清标记，否则这台机器的槽路由永远不收敛"
        );
    }

    /// 一条 `AddRule` 失败（比如内核报 duplicate ruleTag）⇒ 整轮算失败，走同一条退路；
    /// 已经发出去的那几条不回滚（下一轮 `ListRule` 会看到真实状态再补差分）。
    #[tokio::test]
    async fn a_failing_add_rule_also_falls_back() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 2, 2).await;
        let (ctx, host) = ctx_of(d.path(), store, bus).await;
        migrate_on_start(&ctx.store, &ctx.bus).await.unwrap();
        land_xray_config(&ctx, &host).await;
        mark_xray_rules_dirty(&ctx.runtime).await;
        let x = FakeXray::new();
        x.with(|i| {
            i.fail_on.insert("add-rule".into());
        });
        assert_eq!(converge_xray(&ctx, &x).await, ConvergeOutcome::Restarted);
        assert_eq!(restarts_of_xray(&host), 1);
    }

    // ── xray 刚重启时 gRPC 连不上（2026-09-13 bwg-rick rc6 升级实录）──────────────

    fn list_rule_calls(x: &FakeXray) -> usize {
        x.calls().iter().filter(|c| *c == "list-rules").count()
    }

    /// 脚本化「xray 本次启动时刻」与「xray-config.json 的 mtime」（`FakeHost::scripted`
    /// 前缀匹配），格式与真机 `systemctl show --timestamp=us+utc` / `stat -c %y` 一致。
    fn script_xray_times(host: &FakeHost, active_enter: &str, mtime: &str) {
        use crate::sys::CmdOut;
        host.with(|i| {
            i.scripted.push((
                "systemctl show -p ActiveEnterTimestamp".into(),
                CmdOut::success(active_enter),
            ));
            i.scripted
                .push(("stat -c %y".into(), CmdOut::success(mtime)));
        });
    }

    /// 真机那一秒：对账写完新配置、刚重启了 xray，紧跟着的 `ListRule` 撞上
    /// Connection refused（10085 还没起监听）。连接类错误先退避重试，**不许**立刻重启，
    /// 也不许留告警。
    #[tokio::test(start_paused = true)]
    async fn connection_refused_twice_then_ok_retries_without_restart_or_alert() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 2, 2).await;
        let (ctx, host) = ctx_of(d.path(), store, bus).await;
        migrate_on_start(&ctx.store, &ctx.bus).await.unwrap();
        // 退路的前提成立（新配置已落盘）：真走到退路就会重启，这条测试才有意义
        land_xray_config(&ctx, &host).await;
        mark_xray_rules_dirty(&ctx.runtime).await;
        let x = FakeXray::new();
        x.with(|i| i.list_rules_unavailable = 2);

        assert!(
            matches!(converge_xray(&ctx, &x).await, ConvergeOutcome::Applied(_)),
            "{:?}",
            x.calls()
        );
        assert_eq!(
            list_rule_calls(&x),
            3,
            "两次连不上 + 一次成功：{:?}",
            x.calls()
        );
        assert_same_rules(&x.rules(), &want_rules(&ctx).await);
        assert_eq!(restarts_of_xray(&host), 0, "{:?}", host.ops());
        let r = state::read(&ctx.runtime).await;
        assert!(!r.xray_slot_rules_dirty);
        assert!(r.alerts.is_empty(), "{:?}", r.alerts);
    }

    /// 一直连不上：退避总时长封顶 [`GRPC_RETRY_BUDGET`]，之后仍走原来那条「重启一次」退路。
    #[tokio::test(start_paused = true)]
    async fn a_connection_that_never_comes_back_still_falls_back_to_one_restart() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 2, 2).await;
        let (ctx, host) = ctx_of(d.path(), store, bus).await;
        migrate_on_start(&ctx.store, &ctx.bus).await.unwrap();
        land_xray_config(&ctx, &host).await;
        mark_xray_rules_dirty(&ctx.runtime).await;
        let x = FakeXray::new();
        x.with(|i| i.list_rules_unavailable = u32::MAX);

        let t0 = tokio::time::Instant::now();
        assert_eq!(converge_xray(&ctx, &x).await, ConvergeOutcome::Restarted);
        let waited = t0.elapsed();
        assert!(waited <= GRPC_RETRY_BUDGET, "退避总时长超预算：{waited:?}");
        assert!(
            waited > GRPC_RETRY_BUDGET / 2,
            "预算没用满就放弃了：{waited:?}"
        );
        assert!(list_rule_calls(&x) >= 3, "{:?}", x.calls());
        assert_eq!(restarts_of_xray(&host), 1, "{:?}", host.ops());
        assert!(state::read(&ctx.runtime)
            .await
            .alerts
            .iter()
            .any(|a| a.starts_with("Xray 槽路由 gRPC 失败")));
    }

    /// xray 本次启动晚于配置落盘（且磁盘上就是期望的槽规则）⇒ 进程里已是完整规则：
    /// 直接记哈希、清脏，**不调 gRPC、不重启**，顺带认领以前的「gRPC 失败」告警。
    #[tokio::test]
    async fn xray_started_after_the_config_landed_is_booked_without_grpc() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 2, 2).await;
        let (ctx, host) = ctx_of(d.path(), store, bus).await;
        migrate_on_start(&ctx.store, &ctx.bus).await.unwrap();
        land_xray_config(&ctx, &host).await;
        state::update(&ctx.runtime, |r| {
            state::push_alert(r, "机器上没有 journalctl，黑名单候选只能靠每日探针集");
            state::push_alert(
                r,
                "Xray 槽路由 gRPC 失败（code: 'The service is currently unavailable'），已重启 xray 收敛一次",
            );
        })
        .await;
        mark_xray_rules_dirty(&ctx.runtime).await;
        // 真机形态：mtime 按本地时区（+0800）给、启动时刻按 UTC 给；换算后启动晚 0.4 秒
        script_xray_times(
            &host,
            "Sun 2026-09-13 00:05:30.512345 UTC\n",
            "2026-09-13 08:05:30.101234567 +0800\n",
        );
        let x = FakeXray::new();

        assert_eq!(
            converge_xray(&ctx, &x).await,
            ConvergeOutcome::LoadedFromDisk
        );
        assert!(x.calls().is_empty(), "不许调 gRPC：{:?}", x.calls());
        assert_eq!(restarts_of_xray(&host), 0, "{:?}", host.ops());
        let r = state::read(&ctx.runtime).await;
        assert!(!r.xray_slot_rules_dirty);
        assert_eq!(
            r.alerts,
            vec!["机器上没有 journalctl，黑名单候选只能靠每日探针集".to_string()],
            "只清本路径的旧告警"
        );
        // 记下的就是期望哈希：再置脏一轮走第 2 道门，什么都不发
        mark_xray_rules_dirty(&ctx.runtime).await;
        assert_eq!(converge_xray(&ctx, &x).await, ConvergeOutcome::Clean);
        assert!(x.calls().is_empty(), "{:?}", x.calls());
    }

    /// 反方向：配置是在 xray 这次启动**之后**才写的（改分槽只重写文件、不重启 xray），
    /// 进程里还是旧规则 ⇒ 必须照常走 gRPC，不许偷懒记账。
    #[tokio::test]
    async fn xray_started_before_the_config_landed_still_goes_through_grpc() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 2, 2).await;
        let (ctx, host) = ctx_of(d.path(), store, bus).await;
        migrate_on_start(&ctx.store, &ctx.bus).await.unwrap();
        land_xray_config(&ctx, &host).await;
        mark_xray_rules_dirty(&ctx.runtime).await;
        script_xray_times(
            &host,
            "Sun 2026-09-13 00:05:30.000000 UTC\n",
            "2026-09-13 08:05:30.101234567 +0800\n",
        );
        let x = FakeXray::new();

        assert!(matches!(
            converge_xray(&ctx, &x).await,
            ConvergeOutcome::Applied(_)
        ));
        assert_eq!(x.calls().first().map(String::as_str), Some("list-rules"));
        assert_same_rules(&x.rules(), &want_rules(&ctx).await);
        assert_eq!(restarts_of_xray(&host), 0, "{:?}", host.ops());
    }

    /// 真机的完整经过：一次 gRPC 失败退回重启、写下告警；此后某一轮 gRPC 收敛成功
    /// ⇒ 那条告警必须消失（以前它会一直挂在面板与 status 上）。
    #[tokio::test]
    async fn a_stale_grpc_failure_alert_is_cleared_by_the_next_successful_converge() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 2, 2).await;
        let (ctx, host) = ctx_of(d.path(), store, bus).await;
        migrate_on_start(&ctx.store, &ctx.bus).await.unwrap();
        land_xray_config(&ctx, &host).await;
        state::update(&ctx.runtime, |r| {
            state::push_alert(r, "机器上没有 journalctl，黑名单候选只能靠每日探针集")
        })
        .await;
        mark_xray_rules_dirty(&ctx.runtime).await;
        let x = FakeXray::new();
        x.with(|i| {
            i.fail_on.insert("list-rules".into());
        });
        assert_eq!(converge_xray(&ctx, &x).await, ConvergeOutcome::Restarted);
        assert_eq!(state::read(&ctx.runtime).await.alerts.len(), 2);

        // gRPC 恢复；改一个人的分槽 ⇒ 下一轮走 gRPC 增删
        x.with(|i| i.fail_on.clear());
        let uid = ctx.store.read().await.users[0].user_id;
        let slot1 = second_slot_id(&ctx).await;
        assert!(assign_user(&ctx, uid, slot1).await.unwrap().0);
        assert!(matches!(
            converge_xray(&ctx, &x).await,
            ConvergeOutcome::Applied(_)
        ));
        assert_eq!(
            state::read(&ctx.runtime).await.alerts,
            vec!["机器上没有 journalctl，黑名单候选只能靠每日探针集".to_string()]
        );
    }

    #[tokio::test]
    async fn assigning_a_user_marks_the_slot_rules_dirty() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 2, 2).await;
        let (ctx, _host) = ctx_of(d.path(), store, bus).await;
        migrate_on_start(&ctx.store, &ctx.bus).await.unwrap();
        state::update(&ctx.runtime, |r| r.xray_slot_rules_dirty = false).await;
        let uid = ctx.store.read().await.users[0].user_id;
        let target = second_slot_id(&ctx).await;
        assert!(assign_user(&ctx, uid, target).await.unwrap().0);
        assert!(state::read(&ctx.runtime).await.xray_slot_rules_dirty);
    }

    // ── T5：巡检按槽驱动 selector + 手动 pin ─────────────────────────────────

    use crate::modules::residential::clash::{Clash, FakeClash};
    use crate::modules::residential::proxy::FakeProber;
    use crate::modules::residential::state::HealthState;
    use crate::modules::residential::{LATENCY_PROBE_URL, SLOT_BACK_ROUNDS};
    use std::sync::Arc;
    use time::OffsetDateTime;

    /// `n` 槽 + `n` 个上游（`upstream(i)` = `isp(i+1).example.net:10007`），健康状态可注入。
    async fn slot_ctx(dir: &std::path::Path, n: u16) -> (DaemonCtx, Arc<FakeClash>) {
        let (store, bus) = store_with(dir, n, 3).await;
        let (ctx, _host) = ctx_of(dir, store, bus).await;
        migrate_on_start(&ctx.store, &ctx.bus).await.unwrap();
        (ctx, Arc::new(FakeClash::new(Some("resi-1"))))
    }

    /// 三槽 + 三个上游，健康状态可注入。
    async fn three_slot_ctx(dir: &std::path::Path) -> (DaemonCtx, Arc<FakeClash>) {
        slot_ctx(dir, 3).await
    }

    /// 借用后带外验证（spec §5.7）的网关 TCP 时限。假件不真的等，只是同口径传参
    const QUICK: Duration = Duration::from_secs(3);

    /// 只有这些 `"<host>:<port>"` 探得通的假探测器（其余缺省「网关连不上」）
    fn prober_up(endpoints: &[&str]) -> Arc<FakeProber> {
        let p = Arc::new(FakeProber::new());
        p.with_gateways_up(endpoints);
        p
    }

    /// 一次成功的带外验证记下的两条调用（网关 TCP + 经隧道一次 GET）
    fn probe_ok_calls() -> Vec<String> {
        vec!["tcp".into(), format!("timed:{LATENCY_PROBE_URL}")]
    }

    fn healthy_state(google: Option<bool>, ms: u64) -> HealthState {
        HealthState {
            active: true,
            google_ok: google,
            http_ms: vec![ms],
            ..Default::default()
        }
    }

    async fn seed_health(ctx: &DaemonCtx, rows: &[(u128, Option<bool>, u64, bool)]) {
        let rows = rows.to_vec();
        state::update(&ctx.runtime, move |r| {
            for (n, google, ms, active) in rows {
                let mut h = healthy_state(google, ms);
                h.active = active;
                r.health.insert(Uuid::from_u128(n).to_string(), h);
            }
        })
        .await;
    }

    #[tokio::test]
    async fn a_healthy_slot_uses_its_own_ip() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, clash) = three_slot_ctx(d.path()).await;
        seed_health(
            &ctx,
            &[
                (1, Some(true), 100, true),
                (2, Some(true), 100, true),
                (3, Some(true), 100, true),
            ],
        )
        .await;
        let g = state::group_of(&*ctx.store.read().await);
        let healthy = vec![Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3)];
        let out = drive_slots(
            &ctx,
            clash.clone(),
            &g,
            &healthy,
            OffsetDateTime::UNIX_EPOCH,
        )
        .await;
        assert_eq!(out.len(), 3);
        for (i, o) in out.iter().enumerate() {
            assert_eq!(o.index as usize, i);
            assert_eq!(o.target, o.own, "本槽健康 ⇒ 用本槽");
            assert!(!o.borrowed);
        }
        // 每槽的 selector 各自被设成本槽的成员 tag
        assert_eq!(clash.selected("slot-1-pool").as_deref(), Some("resi-2"));
        assert_eq!(clash.selected("slot-2-pool").as_deref(), Some("resi-3"));
    }

    #[tokio::test]
    async fn an_unhealthy_slot_borrows_the_top_ranked_healthy_ip_immediately() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, clash) = three_slot_ctx(d.path()).await;
        // 槽 1 的 IP 不健康；槽 2 的 IP 延迟更低 ⇒ 借用 resi-3
        seed_health(
            &ctx,
            &[
                (1, Some(true), 300, true),
                (2, Some(true), 100, false),
                (3, Some(true), 50, true),
            ],
        )
        .await;
        let g = state::group_of(&*ctx.store.read().await);
        let healthy = vec![Uuid::from_u128(1), Uuid::from_u128(3)];
        let out = drive_slots(
            &ctx,
            clash.clone(),
            &g,
            &healthy,
            OffsetDateTime::UNIX_EPOCH,
        )
        .await;
        let s1 = out.iter().find(|o| o.index == 1).unwrap();
        assert!(s1.borrowed);
        assert_eq!(s1.target, Uuid::from_u128(3));
        assert_eq!(clash.selected("slot-1-pool").as_deref(), Some("resi-3"));
        // 其余槽不受影响
        assert_eq!(clash.selected("slot-0-pool").as_deref(), Some("resi-1"));
    }

    /// 「Google 被封」与「不健康」同等对待（spec §5.6「本槽 IP 健康**且** Google 通」）。
    #[tokio::test]
    async fn a_slot_whose_own_ip_lost_google_also_borrows() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, clash) = three_slot_ctx(d.path()).await;
        seed_health(
            &ctx,
            &[
                (1, Some(true), 100, true),
                (2, Some(false), 100, true),
                (3, Some(true), 100, true),
            ],
        )
        .await;
        let g = state::group_of(&*ctx.store.read().await);
        let healthy = vec![Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3)];
        let out = drive_slots(
            &ctx,
            clash.clone(),
            &g,
            &healthy,
            OffsetDateTime::UNIX_EPOCH,
        )
        .await;
        let s1 = out.iter().find(|o| o.index == 1).unwrap();
        assert!(s1.borrowed);
        assert_eq!(s1.target, Uuid::from_u128(1), "借最高排名的 Google 可用 IP");
    }

    /// 切回要连续 3 轮（spec §5.6）；中途断一轮就重新数。
    #[tokio::test]
    async fn switching_back_needs_three_consecutive_good_rounds() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, clash) = three_slot_ctx(d.path()).await;
        // 先让槽 1 借用出去
        seed_health(
            &ctx,
            &[
                (1, Some(true), 100, true),
                (2, Some(true), 100, false),
                (3, Some(true), 100, true),
            ],
        )
        .await;
        let grp = state::group_of(&*ctx.store.read().await);
        let healthy = vec![Uuid::from_u128(1), Uuid::from_u128(3)];
        drive_slots(
            &ctx,
            clash.clone(),
            &grp,
            &healthy,
            OffsetDateTime::UNIX_EPOCH,
        )
        .await;
        assert_ne!(clash.selected("slot-1-pool").as_deref(), Some("resi-2"));

        // 本槽恢复：第 1、2 轮仍然借用，第 3 轮才切回
        seed_health(&ctx, &[(2, Some(true), 100, true)]).await;
        let healthy = vec![Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3)];
        for round in 1..SLOT_BACK_ROUNDS {
            let out = drive_slots(
                &ctx,
                clash.clone(),
                &grp,
                &healthy,
                OffsetDateTime::UNIX_EPOCH,
            )
            .await;
            let s1 = out.iter().find(|o| o.index == 1).unwrap();
            assert!(s1.borrowed, "第 {round} 轮还不该切回");
            assert_eq!(
                state::read(&ctx.runtime).await.slots["1"].back_rounds,
                round
            );
        }
        let out = drive_slots(
            &ctx,
            clash.clone(),
            &grp,
            &healthy,
            OffsetDateTime::UNIX_EPOCH,
        )
        .await;
        let s1 = out.iter().find(|o| o.index == 1).unwrap();
        assert!(!s1.borrowed);
        assert!(s1.switched);
        assert_eq!(clash.selected("slot-1-pool").as_deref(), Some("resi-2"));
        assert_eq!(state::read(&ctx.runtime).await.slots["1"].back_rounds, 0);
    }

    #[tokio::test]
    async fn a_bad_round_resets_the_switch_back_counter() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, clash) = three_slot_ctx(d.path()).await;
        seed_health(
            &ctx,
            &[
                (1, Some(true), 100, true),
                (2, Some(true), 100, false),
                (3, Some(true), 100, true),
            ],
        )
        .await;
        let grp = state::group_of(&*ctx.store.read().await);
        let all = vec![Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3)];
        let some = vec![Uuid::from_u128(1), Uuid::from_u128(3)];
        drive_slots(&ctx, clash.clone(), &grp, &some, OffsetDateTime::UNIX_EPOCH).await;
        seed_health(&ctx, &[(2, Some(true), 100, true)]).await;
        drive_slots(&ctx, clash.clone(), &grp, &all, OffsetDateTime::UNIX_EPOCH).await;
        assert_eq!(state::read(&ctx.runtime).await.slots["1"].back_rounds, 1);
        // 又坏一轮 ⇒ 归零
        seed_health(&ctx, &[(2, Some(true), 100, false)]).await;
        drive_slots(&ctx, clash.clone(), &grp, &some, OffsetDateTime::UNIX_EPOCH).await;
        assert_eq!(state::read(&ctx.runtime).await.slots["1"].back_rounds, 0);
    }

    /// 全池都不健康：保持当前选择、只记 note，绝不乱切（与全局逻辑的规则 7 同口径）。
    /// 首轮 `current` 还是 `None`，规则 4 也必须**一次 PUT 都不发**（实现里的 `hold`）；
    /// 断言按「没有 put: 开头的调用」写，不依赖 `FakeClash` 有没有记 `get:`。
    #[tokio::test]
    async fn with_nothing_healthy_a_slot_keeps_what_it_has() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, clash) = three_slot_ctx(d.path()).await;
        let grp = state::group_of(&*ctx.store.read().await);
        let out = drive_slots(&ctx, clash.clone(), &grp, &[], OffsetDateTime::UNIX_EPOCH).await;
        assert!(out.iter().all(|o| !o.switched));
        assert!(out.iter().all(|o| o.note.is_some()));
        assert!(
            clash.calls().iter().all(|c| !c.starts_with("put:")),
            "一次 PUT 都不该发，实际 {:?}",
            clash.calls()
        );
        // runtime 里也不该被写进任何「当前选择」
        assert!(state::read(&ctx.runtime)
            .await
            .slots
            .values()
            .all(|s| s.current_upstream_id.is_none()));
    }

    #[tokio::test]
    async fn pinning_a_slot_overrides_the_driver_until_it_is_released() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, clash) = three_slot_ctx(d.path()).await;
        seed_health(
            &ctx,
            &[
                (1, Some(true), 100, true),
                (2, Some(true), 100, true),
                (3, Some(true), 100, true),
            ],
        )
        .await;
        pin_slot(&ctx, clash.clone(), 1, Some(Uuid::from_u128(3)))
            .await
            .unwrap();
        assert_eq!(clash.selected("slot-1-pool").as_deref(), Some("resi-3"));
        let grp = state::group_of(&*ctx.store.read().await);
        let all = vec![Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3)];
        let out = drive_slots(&ctx, clash.clone(), &grp, &all, OffsetDateTime::UNIX_EPOCH).await;
        let s1 = out.iter().find(|o| o.index == 1).unwrap();
        assert_eq!(s1.target, Uuid::from_u128(3), "钉住的不许被驱动器挪回本槽");
        assert!(!s1.switched);
        // 解除 ⇒ **下一轮**（不是 3 轮后）回到本槽：pin_slot(None) 把 current 一起清零，
        // 所以驱动器看到的是「没选过」而不是「正在借用」，不进防抖。
        pin_slot(&ctx, clash.clone(), 1, None).await.unwrap();
        let rt = state::read(&ctx.runtime).await;
        assert!(rt.slots["1"].pinned_upstream_id.is_none());
        assert!(
            rt.slots["1"].current_upstream_id.is_none(),
            "解除 pin 必须连当前选择一起清零，否则下一轮会被当成借用、要攒 3 轮才回本槽"
        );
        let out = drive_slots(&ctx, clash.clone(), &grp, &all, OffsetDateTime::UNIX_EPOCH).await;
        let s1 = out.iter().find(|o| o.index == 1).unwrap();
        assert_eq!(s1.target, s1.own);
        assert!(s1.switched, "重 PUT 回本槽");
        assert_eq!(clash.selected("slot-1-pool").as_deref(), Some("resi-2"));
    }

    // ── T7：`rebalance` ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn rebalance_users_evens_out_the_slots_and_marks_xray_dirty() {
        let d = tempfile::tempdir().unwrap();
        let (store, bus) = store_with(d.path(), 3, 5).await;
        let (ctx, host) = ctx_of(d.path(), store, bus).await;
        migrate_on_start(&ctx.store, &ctx.bus).await.unwrap();
        // 人为压到槽 0
        let slot0 = slots::sorted(&ctx.store.read().await.residential)[0].upstream_id;
        ctx.store
            .update(|s| {
                for u in s.users.iter_mut() {
                    if let Some(e) = u.entitlements.residential.as_mut() {
                        e.slot_id = Some(slot0);
                    }
                }
            })
            .await
            .unwrap();
        state::update(&ctx.runtime, |r| r.xray_slot_rules_dirty = false).await;

        assert_eq!(rebalance_users(&ctx).await.unwrap().0, 3);
        let s = ctx.store.read().await;
        let mut load = vec![0usize; 3];
        for u in &s.users {
            load[slots::index_of_user(u, &s.residential) as usize] += 1;
        }
        assert_eq!(load, vec![2, 2, 1]);
        drop(s);
        assert!(state::read(&ctx.runtime).await.xray_slot_rules_dirty);
        assert_eq!(
            restarts_of_xray(&host),
            0,
            "rebalance 自己不碰 xray：收口交给对账末尾的 converge_xray（走 gRPC 增删，D7）"
        );
        // 幂等
        assert_eq!(rebalance_users(&ctx).await.unwrap().0, 0);
    }

    #[tokio::test]
    async fn pinning_rejects_an_upstream_that_is_not_in_the_pool() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, clash) = three_slot_ctx(d.path()).await;
        assert!(pin_slot(&ctx, clash.clone(), 1, Some(Uuid::from_u128(99)))
            .await
            .is_err());
        assert!(pin_slot(&ctx, clash, 9, Some(Uuid::from_u128(1)))
            .await
            .is_err());
    }

    // ── 哨兵的立即借用（spec §5.7，设计裁决 D7）──────────────────────────────

    #[tokio::test]
    async fn borrow_now_moves_only_the_slots_sitting_on_the_failed_ip() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, clash) = three_slot_ctx(d.path()).await;
        seed_health(
            &ctx,
            &[
                (1, Some(true), 300, true),
                (2, Some(true), 100, false),
                (3, Some(true), 50, true),
            ],
        )
        .await;
        state::update(&ctx.runtime, |r| {
            r.slots.entry("1".into()).or_default().back_rounds = 2;
        })
        .await;
        let p = prober_up(&["isp3.example.net:10007"]);
        let out = borrow_now(
            &ctx,
            p.clone(),
            clash.clone(),
            Uuid::from_u128(2),
            OffsetDateTime::UNIX_EPOCH,
            QUICK,
        )
        .await;
        assert_eq!(out.len(), 1, "只有槽 1 压在故障 IP 上：{out:?}");
        let o = &out[0];
        assert_eq!(
            (o.index, o.target, o.borrowed, o.switched),
            (1, Uuid::from_u128(3), true, true),
            "借排名最高的健康 IP（延迟最低的 resi-3）"
        );
        assert!(o.dead.is_empty() && o.note.is_none(), "{o:?}");
        assert_eq!(
            clash.calls(),
            vec!["put:slot-1-pool:resi-3"],
            "槽 0 / 槽 2 一次 PUT 都不许有"
        );
        assert_eq!(
            p.calls(),
            probe_ok_calls(),
            "探通就到此为止：只验证刚切过去的这一条"
        );
        let r = state::read(&ctx.runtime).await;
        assert_eq!(r.slots["1"].current_upstream_id, Some(Uuid::from_u128(3)));
        assert_eq!(r.slots["1"].back_rounds, 0, "被挪走的槽从头数切回轮数");
    }

    #[tokio::test]
    async fn a_slot_borrowing_the_failed_ip_goes_home_when_its_own_ip_is_fine() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, clash) = three_slot_ctx(d.path()).await;
        seed_health(
            &ctx,
            &[
                (1, Some(true), 300, true),
                (2, Some(true), 100, true),
                (3, Some(true), 50, true),
            ],
        )
        .await;
        // 槽 2 此刻借用着 IP 1
        state::update(&ctx.runtime, |r| {
            r.slots.entry("2".into()).or_default().current_upstream_id = Some(Uuid::from_u128(1));
        })
        .await;
        let p = prober_up(&["isp3.example.net:10007"]);
        let out = borrow_now(
            &ctx,
            p.clone(),
            clash.clone(),
            Uuid::from_u128(1),
            OffsetDateTime::UNIX_EPOCH,
            QUICK,
        )
        .await;
        let s0 = out.iter().find(|o| o.index == 0).unwrap();
        assert_eq!(
            s0.target,
            Uuid::from_u128(3),
            "槽 0 的本槽 IP 就是故障 IP ⇒ 借排名最高的"
        );
        let s2 = out.iter().find(|o| o.index == 2).unwrap();
        assert_eq!(
            (s2.target, s2.borrowed),
            (Uuid::from_u128(3), false),
            "槽 2 自己的 IP 好好的 ⇒ 回本槽"
        );
        assert!(out.iter().all(|o| o.index != 1), "槽 1 没压在 IP 1 上");
        assert_eq!(
            p.calls(),
            probe_ok_calls(),
            "两个槽的目标是同一条 resi-3：第一个槽验证过就缓存住，第二个槽不重复探：{:?}",
            p.calls()
        );
    }

    #[tokio::test]
    async fn borrow_now_leaves_a_pinned_slot_alone() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, clash) = three_slot_ctx(d.path()).await;
        state::update(&ctx.runtime, |r| {
            let e = r.slots.entry("1".into()).or_default();
            e.pinned_upstream_id = Some(Uuid::from_u128(2));
            e.current_upstream_id = Some(Uuid::from_u128(2));
            // 另一个 pin 住、但不压在故障 IP 上的槽：与本次无关，不出现在结果里
            let e = r.slots.entry("0".into()).or_default();
            e.pinned_upstream_id = Some(Uuid::from_u128(3));
            e.current_upstream_id = Some(Uuid::from_u128(3));
        })
        .await;
        let p = prober_up(&["isp3.example.net:10007"]);
        let out = borrow_now(
            &ctx,
            p.clone(),
            clash.clone(),
            Uuid::from_u128(2),
            OffsetDateTime::UNIX_EPOCH,
            QUICK,
        )
        .await;
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(
            (
                out[0].index,
                out[0].target,
                out[0].switched,
                out[0].note.as_deref()
            ),
            (1, Uuid::from_u128(2), false, Some("已手动锁定，未动")),
            "管理员的 pin 压过哨兵，但结果里要记一笔（告警不能说成没有槽经它出网）"
        );
        assert!(clash.calls().is_empty() && p.calls().is_empty());
    }

    #[tokio::test]
    async fn with_nothing_healthy_to_borrow_the_slot_holds_still() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, clash) = three_slot_ctx(d.path()).await;
        seed_health(
            &ctx,
            &[
                (1, Some(true), 100, false),
                (2, Some(true), 100, true),
                (3, Some(true), 100, false),
            ],
        )
        .await;
        let p = prober_up(&["isp3.example.net:10007"]);
        let out = borrow_now(
            &ctx,
            p.clone(),
            clash.clone(),
            Uuid::from_u128(2),
            OffsetDateTime::UNIX_EPOCH,
            QUICK,
        )
        .await;
        assert_eq!(out.len(), 1);
        assert!(!out[0].switched && out[0].dead.is_empty());
        assert_eq!(
            out[0].note.as_deref(),
            Some("没有可借用的健康 IP，保持现状")
        );
        assert!(
            clash.calls().is_empty(),
            "fail-open：不许 PUT 一条不健康的 IP"
        );
        assert!(p.calls().is_empty(), "没有候选可切 ⇒ 一次带外探测都不发");
        assert_eq!(
            state::read(&ctx.runtime)
                .await
                .slots
                .get("1")
                .and_then(|s| s.current_upstream_id),
            None
        );
    }

    #[tokio::test]
    async fn a_rejected_put_leaves_the_runtime_untouched() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, clash) = three_slot_ctx(d.path()).await;
        seed_health(
            &ctx,
            &[
                (1, Some(true), 300, true),
                (2, Some(true), 100, true),
                (3, Some(true), 50, true),
            ],
        )
        .await;
        clash.with(|i| i.reject = true);
        let p = prober_up(&["isp3.example.net:10007"]);
        let out = borrow_now(
            &ctx,
            p.clone(),
            clash.clone(),
            Uuid::from_u128(2),
            OffsetDateTime::UNIX_EPOCH,
            QUICK,
        )
        .await;
        assert!(!out[0].switched && out[0].dead.is_empty());
        assert!(
            p.calls().is_empty(),
            "PUT 都没成，没有「刚切过去的目标」可验证"
        );
        assert!(
            out[0]
                .note
                .as_deref()
                .unwrap()
                .starts_with("切到 resi-3 失败"),
            "{:?}",
            out[0].note
        );
        assert_eq!(
            state::read(&ctx.runtime)
                .await
                .slots
                .get("1")
                .and_then(|s| s.current_upstream_id),
            None,
            "没切成就别记成切了"
        );
    }

    /// 设计裁决 D7：`borrow_now` 与 `drive_slots` 共用 [`slot_switch`] 那把锁。锁被占着（另一方正在
    /// 「读快照 → PUT → 写回」）时两者都得等，一次 PUT 都不发；放开后各自照常做完。
    #[tokio::test]
    async fn borrow_now_and_drive_slots_wait_for_each_other_on_the_slot_switch_lock() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, clash) = three_slot_ctx(d.path()).await;
        seed_health(
            &ctx,
            &[
                (1, Some(true), 300, true),
                (2, Some(true), 100, false),
                (3, Some(true), 50, true),
            ],
        )
        .await;
        let grp = state::group_of(&*ctx.store.read().await);
        let healthy = vec![Uuid::from_u128(1), Uuid::from_u128(3)];
        let wait = std::time::Duration::from_millis(50);

        let held = slot_switch(&ctx).lock_owned().await;
        let borrow = borrow_now(
            &ctx,
            prober_up(&["isp3.example.net:10007"]),
            clash.clone(),
            Uuid::from_u128(2),
            OffsetDateTime::UNIX_EPOCH,
            QUICK,
        );
        tokio::pin!(borrow);
        assert!(
            tokio::time::timeout(wait, &mut borrow).await.is_err(),
            "锁被占着，借用必须等"
        );
        let drive = drive_slots(
            &ctx,
            clash.clone(),
            &grp,
            &healthy,
            OffsetDateTime::UNIX_EPOCH,
        );
        tokio::pin!(drive);
        assert!(
            tokio::time::timeout(wait, &mut drive).await.is_err(),
            "锁被占着，巡检也得等"
        );
        assert!(clash.calls().is_empty(), "等锁期间一次 PUT 都不许发");
        drop(held);
        assert_eq!(
            borrow.await.len(),
            1,
            "放开后借用照常做完（只有槽 1 压在 IP 2 上）"
        );
        assert_eq!(drive.await.len(), 3, "放开后巡检照常做完三个槽");
    }

    // ── 借用后的带外验证（spec §5.7）────────────────────────────────────────

    /// 把池改成「同一个网关主机、不同静态端口」——生产拓扑就是这样，每个端口一个出口 IP，
    /// 「一个出口 IP 挂了」时同主机的兄弟端口照常可用。
    async fn same_gateway(ctx: &DaemonCtx) {
        ctx.store
            .update(|s| {
                let g = s
                    .residential
                    .groups
                    .get_mut(DEFAULT_GROUP)
                    .expect("store_with 自带 default 组");
                for (i, u) in g.upstreams.iter_mut().enumerate() {
                    u.host = "gw.example.net".into();
                    u.port = 10001 + i as u16;
                }
            })
            .await
            .unwrap();
    }

    /// 上一轮巡检留下的 `runtime.health` 可能已过期：第一候选切过去才发现也不通 ⇒
    /// 只把**这一条**记为不健康，换下一个候选。
    #[tokio::test]
    async fn a_candidate_that_fails_its_own_probe_is_marked_and_the_next_one_is_tried() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, clash) = three_slot_ctx(d.path()).await;
        seed_health(
            &ctx,
            &[
                (1, Some(true), 300, true),
                (2, Some(true), 100, false),
                (3, Some(true), 50, true),
            ],
        )
        .await;
        let p = prober_up(&["isp1.example.net:10007"]); // 只有 resi-1 真的通
        let out = borrow_now(
            &ctx,
            p.clone(),
            clash.clone(),
            Uuid::from_u128(2),
            OffsetDateTime::UNIX_EPOCH,
            QUICK,
        )
        .await;
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(
            (
                out[0].target,
                out[0].borrowed,
                out[0].switched,
                out[0].dead.clone(),
                out[0].note.clone()
            ),
            (
                Uuid::from_u128(1),
                true,
                true,
                vec![Uuid::from_u128(3)],
                None
            ),
            "排名最高的 resi-3 探不通 ⇒ 换 resi-1"
        );
        assert_eq!(
            clash.calls(),
            vec!["put:slot-1-pool:resi-3", "put:slot-1-pool:resi-1"]
        );
        assert_eq!(
            p.calls(),
            ["tcp".to_string()]
                .into_iter()
                .chain(probe_ok_calls())
                .collect::<Vec<_>>(),
            "逐条探：死的那条在网关 TCP 就返回，活的那条走完快探"
        );
        let r = state::read(&ctx.runtime).await;
        assert!(
            !r.health[&Uuid::from_u128(3).to_string()].active,
            "探不通的候选立刻记不健康，巡检与下一次借用都不再选它"
        );
        assert!(
            r.health[&Uuid::from_u128(1).to_string()].active,
            "别的上游不许被连坐"
        );
        assert_eq!(r.slots["1"].current_upstream_id, Some(Uuid::from_u128(1)));
        assert_eq!(r.slots["1"].back_rounds, 0);
    }

    /// **不按网关主机归组**：池里多条上游常常是同一个网关的不同静态端口（每个端口一个
    /// 出口 IP），「单个出口 IP 挂掉」时借兄弟端口是唯一可行的处置。
    #[tokio::test]
    async fn candidates_are_probed_per_endpoint_so_a_sibling_port_can_still_be_borrowed() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, clash) = three_slot_ctx(d.path()).await;
        same_gateway(&ctx).await;
        seed_health(
            &ctx,
            &[
                (1, Some(true), 300, true),
                (2, Some(true), 100, true),
                (3, Some(true), 50, true),
            ],
        )
        .await;
        // 同一网关的三个端口，只有 10002（resi-2）这一个出口 IP 还活着
        let p = prober_up(&["gw.example.net:10002"]);
        let out = borrow_now(
            &ctx,
            p.clone(),
            clash.clone(),
            Uuid::from_u128(1),
            OffsetDateTime::UNIX_EPOCH,
            QUICK,
        )
        .await;
        assert_eq!(out.len(), 1, "只有槽 0 压在 gw:10001 上：{out:?}");
        assert_eq!(
            (out[0].index, out[0].target, out[0].switched),
            (0, Uuid::from_u128(2), true),
            "gw:10003 探不通 ⇒ 借同一网关的 gw:10002"
        );
        assert_eq!(out[0].dead, vec![Uuid::from_u128(3)]);
        assert_eq!(
            clash.calls(),
            vec!["put:slot-0-pool:resi-3", "put:slot-0-pool:resi-2"]
        );
        assert_eq!(
            p.calls().len(),
            3,
            "按端点各探一次，不按网关归组（归组会把整组判死、直接砸掉借用）：{:?}",
            p.calls()
        );
        let r = state::read(&ctx.runtime).await;
        assert!(
            r.health[&Uuid::from_u128(2).to_string()].active,
            "同主机的兄弟端口不许被连坐"
        );
    }

    /// 候选全不通：每个候选都发过 PUT，所以终态必须明确——把 selector 放回本槽自己的
    /// 上游，`SlotOutcome` 如实反映，绝不出现「已临时切到」一条死路。
    #[tokio::test]
    async fn with_every_candidate_dead_the_slot_goes_back_to_its_own_ip() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, clash) = three_slot_ctx(d.path()).await;
        seed_health(
            &ctx,
            &[
                (1, Some(true), 300, true),
                (2, Some(true), 100, true),
                (3, Some(true), 50, true),
            ],
        )
        .await;
        let p = Arc::new(FakeProber::new()); // 整个网关都连不上
        let out = borrow_now(
            &ctx,
            p.clone(),
            clash.clone(),
            Uuid::from_u128(2),
            OffsetDateTime::UNIX_EPOCH,
            QUICK,
        )
        .await;
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(
            (
                out[0].target,
                out[0].borrowed,
                out[0].switched,
                out[0].unconfirmed,
                out[0].no_exit,
                out[0].dead.clone(),
                out[0].note.clone()
            ),
            (
                Uuid::from_u128(2),
                false,
                false,
                false,
                true,
                vec![Uuid::from_u128(3), Uuid::from_u128(1)],
                None
            ),
            "终态 = 本槽自己的上游；没借到就不许说 switched；候选试完了才许说 no_exit"
        );
        assert_eq!(
            clash.calls(),
            vec![
                "put:slot-1-pool:resi-3",
                "put:slot-1-pool:resi-1",
                "put:slot-1-pool:resi-2"
            ],
            "收尾那次 PUT 把 selector 从最后一个死候选上放回本槽"
        );
        assert_eq!(
            p.calls(),
            vec!["tcp", "tcp"],
            "两个候选各探一次；本槽自己的上游不补探（哨兵刚探过）"
        );
        let r = state::read(&ctx.runtime).await;
        assert_eq!(
            r.slots["1"].current_upstream_id,
            Some(Uuid::from_u128(2)),
            "runtime 必须和 selector 一致，面板与演练脚本才查得出现在指着谁"
        );
        assert_eq!(r.slots["1"].back_rounds, 0);
        for n in [1u128, 3] {
            assert!(
                !r.health[&Uuid::from_u128(n).to_string()].active,
                "候选 {n}"
            );
        }
    }

    /// 连「放回本槽」的 PUT 都失败：终态是未知（可能仍停在最后一个候选上），
    /// 结果里必须说出来，不许吞掉。
    #[tokio::test]
    async fn a_failed_put_back_to_the_own_ip_is_reported_not_swallowed() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, clash) = three_slot_ctx(d.path()).await;
        seed_health(
            &ctx,
            &[
                (1, Some(true), 300, true),
                (2, Some(true), 100, true),
                (3, Some(true), 50, true),
            ],
        )
        .await;
        clash.with(|i| {
            i.reject_tags.insert("resi-2".into()); // 本槽自己的上游切不回去
        });
        let p = Arc::new(FakeProber::new());
        let out = borrow_now(
            &ctx,
            p,
            clash.clone(),
            Uuid::from_u128(2),
            OffsetDateTime::UNIX_EPOCH,
            QUICK,
        )
        .await;
        assert_eq!(
            (
                out[0].target,
                out[0].borrowed,
                out[0].switched,
                out[0].no_exit
            ),
            (Uuid::from_u128(1), true, false, true),
            "selector 还停在最后一个候选上，如实记"
        );
        assert_eq!(out[0].dead, vec![Uuid::from_u128(3), Uuid::from_u128(1)]);
        let note = out[0].note.clone().unwrap_or_default();
        assert!(
            note.starts_with("放回本槽 IP 也失败：切到 resi-2 失败"),
            "终态由调用方按 `target` 渲染（`sentinel::resi` 那句「当前指向 …」）：{note}"
        );
        assert_eq!(
            state::read(&ctx.runtime).await.slots["1"].current_upstream_id,
            Some(Uuid::from_u128(1))
        );
    }

    /// 中途 Clash PUT 失败停下来：剩下的候选**压根没探过**，所以这一槽没有资格报
    /// 「无可用出口」（`no_exit = false`）——但终态照样要收拾干净：selector 从探死的那个
    /// 候选上放回本槽自己的 IP，note 说清卡在哪一步。
    #[tokio::test]
    async fn a_put_failure_midway_stops_the_loop_without_claiming_there_is_no_exit() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, clash) = three_slot_ctx(d.path()).await;
        seed_health(
            &ctx,
            &[
                (1, Some(true), 300, true),
                (2, Some(true), 100, true),
                (3, Some(true), 50, true),
            ],
        )
        .await;
        clash.with(|i| {
            i.reject_tags.insert("resi-1".into()); // 第二个候选切不过去
        });
        let p = Arc::new(FakeProber::new()); // 网关全连不上：第一个候选探死
        let out = borrow_now(
            &ctx,
            p.clone(),
            clash.clone(),
            Uuid::from_u128(2),
            OffsetDateTime::UNIX_EPOCH,
            QUICK,
        )
        .await;
        assert_eq!(
            (
                out[0].target,
                out[0].switched,
                out[0].no_exit,
                out[0].dead.clone()
            ),
            (Uuid::from_u128(2), false, false, vec![Uuid::from_u128(3)]),
            "候选没试完 ⇒ 不许说无可用出口；终态仍要放回本槽"
        );
        let note = out[0].note.clone().unwrap_or_default();
        assert!(note.starts_with("切到 resi-1 失败"), "{note}");
        assert_eq!(
            p.calls(),
            vec!["tcp"],
            "PUT 失败就停，不再探：{:?}",
            p.calls()
        );
        assert_eq!(
            clash.calls(),
            vec![
                "put:slot-1-pool:resi-3",
                "put:slot-1-pool:resi-1",
                "put:slot-1-pool:resi-2"
            ]
        );
        assert!(
            state::read(&ctx.runtime).await.health[&Uuid::from_u128(1).to_string()].active,
            "PUT 失败不是「它不通」的证据，不许记不健康"
        );
    }

    /// 验证撞上**整体**预算（[`health::QUICK_PROBE_BUDGET_SECS`]）：网关活着、隧道不响应时
    /// `probe_quick` 会一路走到 HTTP 那一段（真实实现实测 ≈28 秒），所以上界只能靠
    /// `timeout` 在代码里保证 —— 演练的丢包只造得出「TCP 连不上」那条快路，量不到这一段。
    /// 结论必须落在「未确认」：保留这个候选（本槽原来那条是已知死的）、不记不健康、不再试下一个。
    ///
    /// **这条用例真的要等满 4 秒**（另一条是 `sentinel::resi` 里判原上游那侧的同形态用例）：
    /// 假时钟在这里用不了 —— 只要有 `spawn_blocking` 任务在飞，tokio 就会抑制 `start_paused`
    /// 的自动推进（`runtime/blocking/schedule.rs` 调 `Clock::inhibit_auto_advance`），而这次
    /// 验证正是一个 `spawn_blocking`。闸门在断言之前放行，阻塞线程不会拖住 runtime 关闭；
    /// `open_after` 是兜底，让「timeout 被删掉」这个变异红在耗时断言上而不是挂死。
    #[tokio::test]
    async fn a_verification_that_outruns_its_budget_keeps_the_candidate_and_says_so() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, clash) = three_slot_ctx(d.path()).await;
        seed_health(
            &ctx,
            &[
                (1, Some(true), 300, true),
                (2, Some(true), 100, true),
                (3, Some(true), 50, true),
            ],
        )
        .await;
        let p = Arc::new(FakeProber::new());
        let gate = Arc::new(crate::modules::residential::proxy::Gate::default());
        p.with(|i| {
            // 排名最高的 resi-3 网关连得上，但经它的 GET 卡住（真实世界里要等满 5 + 5 + N × 5 秒）
            i.tcp_by_endpoint
                .insert("isp3.example.net:10007".into(), Some(20));
            i.gate = Some(gate.clone());
        });
        gate.open_after(Duration::from_secs(15));
        let t0 = std::time::Instant::now();
        let out = borrow_now(
            &ctx,
            p.clone(),
            clash.clone(),
            Uuid::from_u128(2),
            OffsetDateTime::UNIX_EPOCH,
            QUICK,
        )
        .await;
        let took = t0.elapsed();
        gate.open(); // 先放行，再断言（失败也不许把阻塞线程留给 runtime 关闭）
        let budget = Duration::from_secs(health::QUICK_PROBE_BUDGET_SECS);
        assert!(
            took >= budget && took < budget + Duration::from_secs(2),
            "整体耗时被预算截断：{took:?}"
        );
        assert_eq!(
            (
                out[0].target,
                out[0].switched,
                out[0].unconfirmed,
                out[0].no_exit,
                out[0].dead.clone()
            ),
            (Uuid::from_u128(3), true, true, false, Vec::new()),
            "保留这个候选，但不许说「已借到」"
        );
        assert_eq!(
            out[0].note.as_deref(),
            Some("未能在 4 秒内确认可用"),
            "{out:?}"
        );
        assert_eq!(
            clash.calls(),
            vec!["put:slot-1-pool:resi-3"],
            "不再试下一个、也不放回本槽"
        );
        let r = state::read(&ctx.runtime).await;
        assert!(
            r.health[&Uuid::from_u128(3).to_string()].active,
            "没拿到「它坏了」的证据 ⇒ 不许记不健康（下一轮巡检用完整预算复核）"
        );
        assert_eq!(r.slots["1"].current_upstream_id, Some(Uuid::from_u128(3)));
    }

    /// n 槽 / n 条上游，**每个槽都压在 IP 2 上**（`borrow_now` 按槽循环那条路）。
    /// 健康表按「uuid 越大延迟越低」播种 ⇒ `rank_healthy` 的排名就是 uuid 降序，候选顺序可预期。
    async fn all_slots_on_the_failed_ip(
        dir: &std::path::Path,
        n: u16,
    ) -> (DaemonCtx, Arc<FakeClash>) {
        let (ctx, clash) = slot_ctx(dir, n).await;
        let rows: Vec<(u128, Option<bool>, u64, bool)> = (1..=u128::from(n))
            .map(|k| (k, Some(true), 10 * (u64::from(n) + 1 - k as u64), true))
            .collect();
        seed_health(&ctx, &rows).await;
        state::update(&ctx.runtime, move |r| {
            for i in 0..n {
                r.slots
                    .entry(i.to_string())
                    .or_default()
                    .current_upstream_id = Some(Uuid::from_u128(2));
            }
        })
        .await;
        (ctx, clash)
    }

    /// 四个槽全压在故障 IP 上：第一个槽把候选逐条探死之后，**后面的槽的候选被「本次已证死」
    /// 剪空** —— 这一支绝不许退回旧的 fail-open「保持现状」（那会把槽留在刚被证死的故障 IP
    /// 上、事件也不说终态），而是走同一条「全不通」口径：放回本槽自己的 IP、`no_exit`、
    /// 不再白探一遍。
    #[tokio::test]
    async fn slots_whose_candidates_were_already_proven_dead_go_home_without_reprobing() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, clash) = all_slots_on_the_failed_ip(d.path(), 4).await;
        let p = Arc::new(FakeProber::new()); // 整个网关都连不上：每次验证都判死
        let out = borrow_now(
            &ctx,
            p.clone(),
            clash.clone(),
            Uuid::from_u128(2),
            OffsetDateTime::UNIX_EPOCH,
            QUICK,
        )
        .await;
        assert_eq!(out.len(), 4, "四个槽都压在故障 IP 上：{out:?}");
        assert_eq!(
            p.calls().len(),
            BORROW_PROBES_PER_CALL,
            "单次调用的总验证数上限（每次验证在网关 TCP 就返回 = 一条 tcp）：{:?}",
            p.calls()
        );
        let r = state::read(&ctx.runtime).await;
        for o in &out {
            assert_eq!(
                (o.index, o.target, o.borrowed, o.switched, o.no_exit),
                (o.index, o.own, false, false, true),
                "终态一律是本槽自己的 IP：{o:?}"
            );
            assert!(!o.dead.is_empty() && o.note.is_none(), "{o:?}");
            assert_eq!(
                r.slots[&o.index.to_string()].current_upstream_id,
                Some(o.own),
                "runtime 与 selector 一致：{o:?}"
            );
        }
        for n in [1u128, 3, 4] {
            assert!(
                !r.health[&Uuid::from_u128(n).to_string()].active,
                "被证死的候选 {n} 记不健康"
            );
        }
    }

    /// 本次调用里已经**确认可用**的那条，后面的槽直接用，不重复探（预算就三次）。
    #[tokio::test]
    async fn a_candidate_confirmed_alive_once_is_not_probed_again() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, clash) = all_slots_on_the_failed_ip(d.path(), 3).await;
        let p = prober_up(&["isp3.example.net:10007"]); // 只有 resi-3 通
        let out = borrow_now(
            &ctx,
            p.clone(),
            clash.clone(),
            Uuid::from_u128(2),
            OffsetDateTime::UNIX_EPOCH,
            QUICK,
        )
        .await;
        assert!(
            out.iter()
                .all(|o| o.switched && !o.unconfirmed && o.target == Uuid::from_u128(3)),
            "三个槽各自的 selector 都落到那条唯一能用的上游：{out:?}"
        );
        assert_eq!(
            p.calls(),
            ["tcp".to_string()]
                .into_iter()
                .chain(probe_ok_calls())
                .collect::<Vec<_>>(),
            "槽 0 先探死自己的 IP、再探出 resi-3 可用；后两个槽直接用缓存：{:?}",
            p.calls()
        );
    }

    /// 验证预算是**单次调用**的、跨槽共享的，不是每槽一份（槽级上限会让单次调用最坏做
    /// 「池大小 − 1」= 7 次验证，把两条处置延迟 SLA 全破）：六条上游全不通时，预算只够
    /// 探三条，剩下**没探过**的候选照样 PUT 过去（本槽原来的出口是**已知死的**，未知优于
    /// 已知死），按「未确认」报，绝不退回「保持现状」把槽留在故障 IP 上。
    #[tokio::test]
    async fn the_verify_budget_is_per_call_so_a_spent_budget_reports_unconfirmed() {
        let d = tempfile::tempdir().unwrap();
        let (ctx, clash) = all_slots_on_the_failed_ip(d.path(), 6).await;
        let p = Arc::new(FakeProber::new()); // 网关全连不上
        let out = borrow_now(
            &ctx,
            p.clone(),
            clash.clone(),
            Uuid::from_u128(2),
            OffsetDateTime::UNIX_EPOCH,
            QUICK,
        )
        .await;
        assert_eq!(p.calls().len(), BORROW_PROBES_PER_CALL, "{:?}", p.calls());
        let r = state::read(&ctx.runtime).await;
        for o in &out {
            assert_eq!(
                (o.index, o.switched, o.unconfirmed, o.no_exit),
                (o.index, true, true, false),
                "{o:?}"
            );
            assert_eq!(
                o.note.as_deref(),
                Some("本次调用的 3 次验证预算已用尽，未确认可用"),
                "{o:?}"
            );
            assert_ne!(o.target, Uuid::from_u128(2), "不许留在故障 IP 上：{o:?}");
            assert!(!o.dead.contains(&o.target), "落点不是已证死的那几条：{o:?}");
            assert_eq!(
                r.slots[&o.index.to_string()].current_upstream_id,
                Some(o.target)
            );
            assert!(
                r.health[&o.target.to_string()].active,
                "没探过的那条不许被记成不健康：{o:?}"
            );
        }
    }
}
