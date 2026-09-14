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
    POOL, SWITCH_IMPROVE_ROUNDS, SWITCH_LATENCY_GAIN, SWITCH_MIN_INTERVAL_SECS, SWITCH_SPEED_GAIN,
};
use crate::api::Event;
use crate::reconcile::DaemonCtx;
use crate::util::{fmt_rfc3339, parse_rfc3339};
use bui_schema::model::{ResidentialGroup, Slot, Upstream};
use clash::Clash;
use proxy::{ProbeError, Prober};
use state::ResiRuntime;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;
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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemberProbe {
    pub ok: bool,
    pub auth_failed: bool,
    /// 经该上游还能不能正常用 Google 搜索（`None` = 本轮没探到结论）。
    /// 主理人硬要求「住宅上游不封 Google」，所以它参与选路（R2 ②）
    pub google_ok: Option<bool>,
    /// 本轮**经上游的完整 HTTP 往返**耗时（毫秒）；`None` = 没测到。
    /// 与连通性判定复用同一次请求（[`probe_reachable`] 的 try #1）
    pub http_ms: Option<u64>,
    /// 本轮**到上游网关**的 TCP 建连耗时（毫秒）
    pub tcp_ms: Option<u64>,
    /// 本轮的 UDP 探测（socks5 走 STUN，http 恒不通 + 标注「HTTP 上游无 UDP」）
    pub udp: proxy::UdpProbe,
}

/// 单成员一轮探测：可达性（含延迟样本）+ Google 可达性 + UDP 各一轮。
pub fn probe_member(p: &dyn Prober, up: &Upstream) -> MemberProbe {
    // 到网关的 TCP 建连耗时：不经隧道、不发请求。它只是**指标**，连不上也照样往下探
    let tcp_ms = p.gateway_tcp_ms(up);
    let mut probe = probe_reachable(p, up);
    probe.tcp_ms = tcp_ms;
    // R2 ②：每轮额外经该上游打一次真实 Google 搜索。凭据失效时不打——每条连接都会
    // 被拒，探不出任何关于 Google 的结论，只会白等 GOOGLE_PROBE_TIMEOUT_SECS。
    // UDP 同理（SOCKS5 认证会被直接拒）
    if !probe.auth_failed {
        probe.google_ok = proxy::google_ok_of(&p.google_search(up));
        probe.udp = p.stun_binding(up);
    }
    probe
}

/// 哨兵（spec §5.7，设计裁决 D5）的带外快探：先在 `tcp_within` 内对上游网关建 TCP
/// （[`Prober::gateway_tcp_within`]：解析出的各地址并发拨、整体限时），**连不上直接判不可达**
/// ——网关都连不上，隧道不可能通，再等一次 HTTP 超时只会拖慢预案（上游被丢包时巡检那套
/// [`probe_member`] 要走 ~40 秒）；连得上再走一轮与巡检同口径的 [`probe_reachable`]（含 407 补判）。
/// **不测 Google / UDP / 测速**：那些是巡检的指标，预案只关心「这条上游此刻还能不能用」。
///
/// **它本身没有上界**：网关连得上之后那段完整探测的超时是各请求自己的
/// （`timed_get` 吃 [`super::LATENCY_PROBE_TIMEOUT_SECS`] = 8 秒 —— **不是**调用方
/// `Prober` 的那个超时；`get` 与 407 补判的 CONNECT 各吃 `Prober` 的超时，`ReqwestProber::dial`
/// 还按解析出的地址逐个串行各等一次），叠起来远超预案的延迟预算。所以它**私有**：唯一的出口是
/// [`probe_quick_within`]（带预算、结论三值），这个不变式交给编译器而不是注释 —— 上一轮的教训
/// 正是「注释挡不住后来人按旧口径算预算」（2026-09-14 审查第 5 条）。
fn probe_quick(p: &dyn Prober, up: &Upstream, tcp_within: Duration) -> MemberProbe {
    let Some(tcp_ms) = p.gateway_tcp_within(up, tcp_within) else {
        return MemberProbe::default();
    };
    let mut probe = probe_reachable(p, up);
    probe.tcp_ms = Some(tcp_ms);
    probe
}

/// 一次带预算的快探（[`probe_quick_within`]）的**整体**时限：整个 `probe_quick` 被
/// `tokio::time::timeout` 包住，超时即结论「未确认」（不是「不可用」）。
///
/// **为什么非得有这个上界**：`probe_quick` 只在网关 TCP 连不上时才是「≤ `tcp_within` 返回」；
/// 一旦 TCP 通过，它会继续走 [`probe_reachable`]（`timed_get` 8 秒 + `get` 5 秒 + 407 补判的
/// CONNECT 5 秒，多地址网关还要按 `dial` 逐地址串行各 5 秒），而「网关活着、某个静态端口背后
/// 的出口 IP 死了」恰恰是住宅预案主打的故障形态。用生产参数
/// （`ReqwestProber::with_timeout(PROBE_TIMEOUT_SECS = 5)`、`tcp_within = 3`）对着一个
/// 「accept 后永不应答」的本地监听实测单次 `probe_quick` = **28.4 秒**（2026-09-14 审查实测），
/// 多地址网关更高。没有这个上界，spec §5.7 的两条处置延迟 SLA（15 / 25 秒）在生产最常见的
/// 故障形态上必然破。
///
/// 4 秒的来历：① 网关 TCP 连不上那条路 ≤ `tcp_within`（哨兵传
/// `sentinel::PROBE_TCP_TIMEOUT_SECS` = 3 秒）就返回；② 网关活着、隧道正常那条路经隧道一次
/// GET 远小于 1 秒。**必须 > `tcp_within`**，否则 TCP 一步就把预算吃光，每条「网关连不上」的
/// 上游都从「确认不可用」退化成「未确认」⇒ 借用侧在第一个候选上就停（既不记不健康也不试下一
/// 个），候选轮换静默失效，判原上游那侧也丢掉「凭据失效」与「不可达」的区分
/// （`sentinel::run` 里有一条用例钉住这个不等式）。
///
/// **这个预算之内「隧道挂死」拿不到 [`Verdict::Dead`]**：`probe_reachable` 的第一次请求是
/// `timed_get`，超时 [`super::LATENCY_PROBE_TIMEOUT_SECS`] = 8 秒 > 4 秒，隧道不应答就必然先
/// 吃光预算 ⇒ 结论只能是「未确认」。所以判原上游那一侧把「未确认」按不可用处置（那边有 relay
/// 的连报错误作佐证），否则哨兵在**主打的故障形态**上就是永久空操作 —— 见 [`Verdict`] 的文档
/// 与 `sentinel::resi::on_upstream_error`（2026-09-14 审查第 2 条）。
///
/// **已知取舍（2026-09-14 审查）**：4 秒对真实住宅出口偏紧 —— 经隧道的首包 1–3 秒并不罕见，
/// 加上最坏 3 秒的网关 TCP 就可能越过 4 秒，于是**健康**的上游也会常态报「未确认」。代价按侧
/// 不同：借用侧 fail-safe（保留候选、不记不健康），文案从「槽 i 已临时切到 Y」退化成「已切到
/// Y，未能在 4 秒内确认可用」；判原上游那侧会多借一次兄弟 IP（吵闹、巡检连续 3 轮自己切回），
/// 换来「隧道挂死」这条路上仍然 15 秒自愈。要不要放宽，等 bwg-tizi 上实测一次真实借用验证的
/// 耗时再定（计划 D18 记了这一项）。
pub const QUICK_PROBE_BUDGET_SECS: u64 = 4;

/// 一次带预算的快探的三种结论。「确认不可用」与「没能确认」**必须分开**：前者是这条上游
/// 自己给出的明确失败，后者只是「预算内没应答」—— 两者该怎么处置，由调用方按**手上还有没有
/// 别的佐证**决定，本枚举只如实报观测：
/// - `sentinel::resi::on_upstream_error`（判原上游）：relay 刚在 60 秒内独立报了 ≥2 条连接
///   错误，带外也过不去是**佐证** ⇒ `Unconfirmed` 与 `Dead` 同样按不可用处置（文案区分）；
/// - `slots::borrow_now`（借用后验证）：那条候选没人报过错，没有这层佐证 ⇒ `Unconfirmed`
///   只能**保留候选**、不记不健康。
///
/// 两处处置不同不是含糊，是证据量不同（2026-09-14 审查第 2 条的裁决）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// 预算内确认可用
    Alive,
    /// 预算内拿到明确的失败：网关 TCP 不通，或经隧道的 HTTP 明确失败 / 凭据被拒
    /// （`auth_failed` = 407 / SOCKS5 认证被拒，告警要据此说「凭据失效」而不是「不可达」）
    Dead { auth_failed: bool },
    /// 预算内没能确认（超时、或探测任务本身异常）⇒ 结论未知
    Unconfirmed,
}

/// 私有的 `probe_quick` 加 [`QUICK_PROBE_BUDGET_SECS`] 的整体预算，结论三值（[`Verdict`]）。
/// **哨兵两侧都只能经这里**（`probe_quick` 不对外）。
///
/// 哨兵判「原上游坏了」（`sentinel::resi::on_upstream_error`）与借用后验证刚切过去那条
/// （`slots::borrow_now`）**共用这一份**：同一个探测、同一个上界、同一套三值口径。**观测**
/// 必须对称 —— 只给其中一处加时限，处置延迟算式里的上界就只对那一处成立（2026-09-14 审查
/// 第 1 条）。**处置**不对称：两处对 [`Verdict::Unconfirmed`] 的处理按各自手上的佐证分道，
/// 见 [`Verdict`] 的文档（2026-09-14 审查第 2 条）。
///
/// `Prober` 是同步的（`reqwest::blocking`），照例进 `spawn_blocking`；超时后那个任务会自己
/// 跑完（`reqwest::blocking` 没有取消点），结果丢弃，不阻塞调用方。
///
/// 探测任务本身异常（`JoinError`：探测器 panic / runtime 正在关）也并进 [`Verdict::Unconfirmed`]
/// —— 结论一样是「不知道」。但**原因必须落日志**：调用方的事件文案只说「网关通但隧道无响应，
/// 或探测任务异常」，探测器 panic 会每 60 秒（`sentinel::DEBOUNCE_SECS`）静静复发一次，
/// 日志里不留原因就无从下手（2026-09-14 审查第 4 条）。
pub async fn probe_quick_within(
    prober: &Arc<dyn Prober>,
    up: Upstream,
    tcp_within: Duration,
) -> Verdict {
    let p = prober.clone();
    // 日志里指称这条上游只用 `host:port`（同 `sentinel::resi::subject_of`），绝不带凭据
    let subject = format!("{}:{}", up.host, up.port);
    let probe = tokio::task::spawn_blocking(move || probe_quick(p.as_ref(), &up, tcp_within));
    let budget = Duration::from_secs(QUICK_PROBE_BUDGET_SECS);
    match tokio::time::timeout(budget, probe).await {
        Ok(Ok(probe)) if probe.ok => Verdict::Alive,
        Ok(Ok(probe)) => Verdict::Dead {
            auth_failed: probe.auth_failed,
        },
        Ok(Err(e)) => {
            tracing::warn!(upstream = %subject, error = %e, "带外快探的探测任务异常，结论按「未确认」");
            Verdict::Unconfirmed
        }
        Err(_) => Verdict::Unconfirmed,
    }
}

/// 可达性那一半：最多 [`HEALTH_TRIES`] 次，任一成功即本轮健康。
/// **第 1 次打 [`super::LATENCY_PROBE_URL`] 并计时**——延迟样本与连通性判定复用同一次
/// 请求，所以加了指标之后健康成员每轮的 HTTP 请求数没变（主理人口径「避免多发」）；
/// 第 2 次打 [`HEALTH_PROBE_URL`]，两个互不相干的目标反而比连打同一个更能分辨
/// 「上游挂了」与「这个站点挂了」。
/// **407 / SOCKS5 认证被拒一律算不健康**（调研 §D：凭据失效的上游会把每条连接都拒掉，
/// 它「可达」但不可用），并且立刻停止重试。
fn probe_reachable(p: &dyn Prober, up: &Upstream) -> MemberProbe {
    let mut http_ms = None;
    for i in 0..HEALTH_TRIES {
        let r = if i == 0 {
            let (ms, r) = p.timed_get(up, super::LATENCY_PROBE_URL);
            http_ms = ms;
            r
        } else {
            p.get(up, HEALTH_PROBE_URL)
        };
        match r {
            // generate_204 正常回 204；任何 2xx/3xx 都说明隧道通了
            Ok(hp) if hp.status < 400 => {
                return MemberProbe {
                    ok: true,
                    http_ms,
                    ..Default::default()
                }
            }
            // 调研 §D：407 的上游「可达」但每条连接都被拒 —— 一律不健康，且不必再试
            Err(ProbeError::AuthFailed) => {
                return MemberProbe {
                    auth_failed: true,
                    http_ms,
                    ..Default::default()
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
        auth_failed,
        http_ms,
        ..Default::default()
    }
}

/// 当前池的成员 id，顺序与 `g.upstreams` 一致（成员集比较用它，不用位置键 tag）
pub fn ids_of(g: &ResidentialGroup) -> Vec<Uuid> {
    g.upstreams.iter().map(|u| u.id).collect()
}

/// [`pick_target`] 的排序键，按元组的字典序比较：`(Google 不通, priority, UDP 不通,
/// 延迟 p50, -下行×1000, -近 24h 成功率×1e6, 池内下标, uuid)`。
/// 布尔项用 `0 = 好`，降序项取负 —— f64 不能直接排序，也不想引 `total_cmp` 的歧义。
type SortKey = (u8, u32, u8, u64, i64, i64, usize, Uuid);

/// 切换目标。排序键依次是：
/// ① **「Google 通」优先**（R2 ②，封 Google 的上游对主理人等于不可用）。
///    主理人 2026-09-12 的口径写的是「候选 = 健康且 google_ok」，这里落成**第一排序键**
///    而不是硬过滤：全池都探不到 Google 结论（首轮、或 Google 整域不可达）时硬过滤会让
///    候选集为空、整池选不出出口，fail-open 比 fail-closed 安全。有一条 Google 通时，
///    它与过滤等价。
/// ② `priority` 最小（管理员显式给的偏好，压过下面的实测指标）
/// ③ **「UDP 通」优先**（主理人 2026-09-12 追加：http 上游没有 UDP，socks5 上游也可能被中
///    间设备吞 UDP，QUIC / HTTP3 全靠它）
/// ④ 延迟 p50 最低 → ⑤ 下行中位数最快
///    （主理人 2026-09-12：「选最健康、最低延迟、速度最快的」）
/// ⑥ 近 24h 成功率降序 → ⑦ 池内下标升序（稳定）。
///
/// ④⑤ 的「未知」一律排在「有数据」之后（延迟按 `u64::MAX`、速度按 0）：没测过不等于
/// 0 毫秒，否则一条刚加进来、什么都没测的上游会拿到全池最低延迟、直接抢走出口。
/// ⑥ 留在 ⑤ 之后：主理人给的键到 ⑤ 为止，但两条上游连延迟与速度都齐平时，
/// 近 24h 成功率仍是比池内下标更有意义的判据。
///
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
    rank_healthy(g, r, healthy, now).into_iter().next()
}

/// 把健康成员按 [`pick_target`] 的排序键**全部**排好（不只取首位）。
/// 按槽驱动要「借用排名最高的其他健康 IP」，需要的是整张排名而不是冠军。
///
/// **不含手动锁定的短路**：那是全局 selector（`resi-pool`）的语义，
/// 每槽的手动 pin 在 `slots::drive_slots` 里单独处理。
pub fn rank_healthy(
    g: &ResidentialGroup,
    r: &ResiRuntime,
    healthy: &[Uuid],
    now: OffsetDateTime,
) -> Vec<Uuid> {
    let mut cands: Vec<SortKey> = healthy
        .iter()
        .filter_map(|id| {
            // 不在当前池里的 uuid 直接忽略（删上游与巡检并发时会出现）
            let idx = g.upstreams.iter().position(|u| u.id == *id)?;
            let h = r.health.get(&id.to_string());
            let rate = h.map(|h| state::success_rate_24h(h, now)).unwrap_or(0.0);
            // 「未知」与「封了 / 不通」一起排在「通」之后：没探到结论不该凭空赢过探过的
            let google_rank = u8::from(h.and_then(|h| h.google_ok) != Some(true));
            let udp_rank = u8::from(h.and_then(|h| h.udp_ok) != Some(true));
            // 延迟升序，未知排最后；速度与成功率降序 = 放大后取负
            // （f64 不能直接排序，也不想引 total_cmp 的歧义）
            let latency = h.and_then(state::latency_p50).unwrap_or(u64::MAX);
            let down = h
                .and_then(|h| state::median(&h.down_mbps))
                .unwrap_or_default();
            Some((
                google_rank,
                g.upstreams[idx].priority,
                udp_rank,
                latency,
                -((down * 1_000.0) as i64),
                -((rate * 1_000_000.0) as i64),
                idx,
                *id,
            ))
        })
        .collect();
    cands.sort();
    cands.into_iter().map(|c| c.7).collect()
}

/// 「候选比当前出口**明显**更优」（主理人 2026-09-12 的防抖判据）：延迟 p50 低
/// ≥ [`SWITCH_LATENCY_GAIN`]，**或**下行中位数快 ≥ [`SWITCH_SPEED_GAIN`]。
/// 任一侧没数据就不算更优 —— 未知不许赢，否则一条没测过的上游能靠「无数据」抢出口。
pub fn is_improvement(r: &ResiRuntime, cand: Uuid, cur: Uuid) -> bool {
    let h = |id: Uuid| r.health.get(&id.to_string());
    let faster = match (
        h(cand).and_then(state::latency_p50),
        h(cur).and_then(state::latency_p50),
    ) {
        (Some(a), Some(b)) if b > 0 => (b as f64 - a as f64) / b as f64 >= SWITCH_LATENCY_GAIN,
        _ => false,
    };
    let fatter = match (
        h(cand).and_then(|x| state::median(&x.down_mbps)),
        h(cur).and_then(|x| state::median(&x.down_mbps)),
    ) {
        (Some(a), Some(b)) if b > 0.0 => (a - b) / b >= SWITCH_SPEED_GAIN,
        _ => false,
    };
    faster || fatter
}

/// 一行「当前选中 resi-N 的原因」（主理人 2026-09-12）：按 [`pick_target`] 的排序键
/// 从前往后找第一条**真正起作用**的理由，说人话。纯函数，`health` / `status` / CLI 共用。
pub fn selection_reason(g: &ResidentialGroup, r: &ResiRuntime, id: Uuid) -> String {
    let Some(up) = g.upstreams.iter().find(|u| u.id == id) else {
        return "当前出口已不在池里（成员集刚变过）".into();
    };
    if r.manual_selected_id == Some(id) {
        return "手动锁定（巡检不会按优先级 / 延迟 / 速度把它切走）".into();
    }
    let h = |u: &Upstream| r.health.get(&u.id.to_string()).cloned().unwrap_or_default();
    let healthy: Vec<&Upstream> = g.upstreams.iter().filter(|u| h(u).active).collect();
    if healthy.len() <= 1 {
        return "唯一健康的成员".into();
    }
    let me = h(up);
    let others = || healthy.iter().filter(|u| u.id != id);
    // 排序键 ①：只在「别人确实不行」时才说得上是理由
    if me.google_ok == Some(true) && others().all(|u| h(u).google_ok != Some(true)) {
        return "其余健康成员的 Google 不可用（Google 可用是第一排序键）".into();
    }
    // 排序键 ②
    if others().all(|u| u.priority > up.priority) {
        return format!("优先级最优（{}）", up.priority);
    }
    // 排序键 ③④⑤ 都是同优先级内部的比较（priority 压过实测指标）
    let peers: Vec<&Upstream> = healthy
        .iter()
        .copied()
        .filter(|u| u.priority == up.priority && u.id != id)
        .collect();
    if me.udp_ok == Some(true) && peers.iter().all(|u| h(u).udp_ok != Some(true)) {
        return "同优先级里只有它的 UDP 通".into();
    }
    if let Some(ms) = state::latency_p50(&me) {
        if peers
            .iter()
            .all(|u| state::latency_p50(&h(u)).is_none_or(|x| x > ms))
        {
            return format!("同优先级里延迟最低（p50 {ms} ms）");
        }
    }
    if let Some(v) = state::median(&me.down_mbps) {
        if peers
            .iter()
            .all(|u| state::median(&h(u).down_mbps).is_none_or(|x| x < v))
        {
            return format!("同优先级里下行最快（{v:.1} Mbps）");
        }
    }
    "按 Google / 优先级 / UDP / 延迟 / 速度排序后的首位".into()
}

/// 测速的用量与周期：常量为默认，`state.residential` 的三个可选字段可覆盖。
/// 0 与荒唐的大小一律回退 / 夹住 —— 打错一个 0 不该把上游流量打爆，也不该把测速关死。
pub fn speedtest_cfg(s: &bui_schema::model::State) -> (u64, u64, i64) {
    let bytes = |v: Option<u64>, dflt: u64| match v {
        Some(x) if x > 0 => x.min(super::SPEEDTEST_MAX_BYTES),
        _ => dflt,
    };
    (
        bytes(
            s.residential.speedtest_down_bytes,
            super::SPEEDTEST_DOWN_BYTES,
        ),
        bytes(s.residential.speedtest_up_bytes, super::SPEEDTEST_UP_BYTES),
        match s.residential.speedtest_interval_mins {
            Some(m) if m > 0 => m,
            _ => super::SPEEDTEST_INTERVAL_MINS,
        },
    )
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
    // R1 之前按位置名写进 alerts 的上游告警、以及已被删出池的 uuid 留下的孤儿告警：
    // 每轮清一次（只在有变化时写盘）。放在 pool_active 之前 —— 池被清空时
    // 那些条目同样该消失。
    state::purge_stale_alerts(&ctx.runtime, &g).await;
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
    // 本轮每个成员的完整探测结果（Google 判定、延迟样本、UDP），写回时按 uuid 落账
    let mut metrics: Vec<(Uuid, MemberProbe)> = Vec::new();
    for (i, up) in ups.iter().enumerate() {
        let probe = results.get(i).cloned().flatten();
        let ok = probe.as_ref().map(|x| x.ok).unwrap_or(false);
        if probe.is_none() {
            out.notes
                .push(format!("{} 的探测任务异常结束，本轮按不达标处理", up.name));
        }
        // 凭据失效要单独告警：不是网络抖动，管理员必须换凭据（探测结果里直接带出来，
        // 不再为了补判而在 async 上下文里同步调一次 Prober）
        if probe.as_ref().map(|x| x.auth_failed).unwrap_or(false) {
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
        // 任务异常结束的成员只记「本轮不达标」（上面的 `probed`），指标一个都不落账：
        // 缺省的 `MemberProbe` 会把 UDP 记成「不通」，那是「没探」不是「不通」
        if let Some(pr) = probe {
            metrics.push((up.id, pr));
        }
    }

    // 规则 3：迟滞 + 24h 样本 + Google 判定 + 延迟 / UDP 样本。**一轮只写一次 runtime**：
    // 每次 update 都是 tmp + fsync + rename，按成员各写一次等于一轮 N 次落盘。
    // 凭据失效告警的写入与「成功即清」也并进这一次写（下面各分支不再重复落它）。
    let samples = probed.clone();
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
        for (id, m) in metrics {
            let h = r.health.entry(id.to_string()).or_default();
            state::record_google(h, m.google_ok, now);
            state::record_latency(h, m.http_ms, m.tcp_ms, None);
            // 凭据失效那一轮没探 UDP（`probe_member` 跳过了），别把「没探」记成「不通」
            if !m.auth_failed {
                state::record_udp(h, &m.udp, now);
            }
        }
    })
    .await;

    // 每小时一轮**全量测速**（主理人 2026-09-12）：与巡检同一个 tick，不另起调度器；
    // 到点与否看 runtime 的游标，所以守护进程重启既不会漏测也不会连着测两次。
    // 放在健康判定之后：测速失败只记 note，一个字节都不影响上面那份判定。
    let (down_bytes, up_bytes, interval) = speedtest_cfg(&ctx.store.read().await.clone());
    let due = match rt.last_speedtest_at.as_deref().and_then(parse_rfc3339) {
        // 时钟回跳（NTP 校时）不该把测速永久锁死
        Some(t) => {
            let mins = (now - t).whole_minutes();
            mins >= interval || mins < 0
        }
        None => true,
    };
    if due {
        run_speedtest(ctx, p.clone(), &ups, down_bytes, up_bytes, now).await;
    }
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

    // spec §5.6：按槽驱动各自的 selector。放在 healthy 算完、全局切换判定之前 ——
    // 它与全局 selector 的决策彼此独立（D8），而且不管全局这一轮切不切，
    // 每个槽都得按「本槽优先 / 借用 / 3 轮切回」收敛到位。
    for o in
        crate::modules::residential::slots::drive_slots(ctx, c.clone(), &g, &healthy, now).await
    {
        if o.switched {
            out.notes.push(format!(
                "槽 {} 切到 {}",
                o.index,
                clash::tag_of(&g, o.target).unwrap_or_else(|| o.target.to_string())
            ));
        } else if let Some(n) = &o.note {
            out.notes.push(format!("槽 {}：{n}", o.index));
        }
    }

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
    let sel_tag = tokio::task::spawn_blocking(move || cc.selected(POOL)).await?;
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
    //
    // 规则 6b：runtime 没有选择（守护进程首次启动、从没切过）或它已被删出池 ——
    // 此时才拿 Clash 的 now 初始化 runtime，并粘在它上面。
    let ids = ids_of(&g);
    let want = rt.selected_upstream_id.filter(|id| ids.contains(id));
    // 「当前该生效的出口」：6a 的 runtime 记录（真源）优先，6b 的 Clash now 兜底
    let current = match want {
        Some(w) => healthy.contains(&w).then_some(w),
        None => sel_id.filter(|s| healthy.contains(s)),
    };
    // 规则 6d（主理人 2026-09-12：「当前不健康 / Google 封立即切」）：不健康由上面的
    // `healthy` 挡掉，Google 被封的当前出口在这里放掉「它就是当前出口」这个结论，直接
    // 落到规则 8 的立即切换（不吃 6c 的防抖轮数）。两个前提：手动锁定的那条不挪
    // （R2 ①），且池里确实还有别的健康成员 Google 通 —— 否则全池都封 Google 时
    // 每轮都会「切」到自己身上，白掐一次住宅连接。
    let google_ok = |id: Uuid| rt.health.get(&id.to_string()).and_then(|h| h.google_ok);
    let current = match current {
        Some(cur) if google_ok(cur) == Some(false) && rt.manual_selected_id != Some(cur) => {
            // tag 现算；cur 取自当前池，tag_of 必有值
            let tag = clash::tag_of(&g, cur).unwrap_or_else(|| sel_tag.clone());
            if healthy
                .iter()
                .any(|id| *id != cur && google_ok(*id) == Some(true))
            {
                out.notes
                    .push(format!("当前 {tag} 的 Google 已被封，立即切换（不吃防抖）"));
                None
            } else {
                out.notes.push(format!(
                    "当前 {tag} 的 Google 已被封，但池里没有别的 Google 可用的健康成员，本轮不切"
                ));
                Some(cur)
            }
        }
        other => other,
    };
    if let Some(cur) = current {
        // 规则 6c（主理人 2026-09-12）：当前出口健康时也要看有没有**明显**更优的候选。
        // 先算判定，好把防抖计数与下面的选择落账并成同一次写盘
        let d = improve_decision(&g, &rt, &healthy, cur, now);
        let mut alerts: Vec<String> = Vec::new();
        if want == Some(cur) && Some(cur) != sel_id {
            // tag 现算（位置键，不做主键）；cur 来自当前池，tag_of 必有值
            let tag = clash::tag_of(&g, cur).expect("cur 取自当前池");
            let (cc, t2) = (c.clone(), tag.clone());
            match tokio::task::spawn_blocking(move || cc.select(POOL, &t2)).await? {
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
        let pending = Some(cur) != g.selected_upstream_id;
        let (cand, rounds) = (d.cand, d.rounds);
        state::update(&ctx.runtime, move |r| {
            r.selected_upstream_id = Some(cur);
            r.selected_pending_persist = pending;
            r.improve_candidate_id = cand;
            r.improve_rounds = rounds;
            for a in alerts.drain(..) {
                state::push_alert(r, a);
            }
        })
        .await;
        if let Some(target_id) = d.go {
            // tag 现算；target_id 来自 pick_target，必在当前池里
            let tag = clash::tag_of(&g, target_id).expect("target 取自当前池");
            // 这是一次真切换，同吃规则 9 的 60 秒限速
            if let Some(rest) = switch_cooldown(&rt, now) {
                out.notes
                    .push(format!("{tag} 明显更优，但切换限速中（剩余 {rest}s）"));
            } else if switch_to(ctx, c, &g, target_id, &tag, now, None, &mut out).await? {
                tracing::info!(from = %sel_tag, to = %tag, rounds, "住宅出口按延迟 / 速度切换");
                out.notes.push(format!(
                    "{tag} 连续 {} 轮延迟 / 速度明显更优，已切换",
                    rounds.max(1)
                ));
            }
        } else if let Some(c) = d.cand {
            if let Some(tag) = clash::tag_of(&g, c) {
                out.notes.push(format!(
                    "{tag} 延迟 / 速度更优，已连续 {rounds}/{SWITCH_IMPROVE_ROUNDS} 轮，未到防抖门槛，本轮不切"
                ));
            }
        }
        return Ok(out);
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
    if let Some(rest) = switch_cooldown(&rt, now) {
        out.notes.push(format!(
            "当前 {sel_tag} 需切到 {target_tag}，但切换限速中（剩余 {rest}s）"
        ));
        return Ok(out);
    }

    // 规则 10/11：切换
    // 切到的不是手动锁定的那条 ⇒ 锁定目标已不健康（否则 pick_target 会直接返回它），
    // 解除锁定，别锁着一条坏上游不放（R2 ①）。切到的**就是**锁定目标时（规则 6a 的
    // runtime 选择另有其人、已不健康）这一轮正是在把锁定目标放回去，锁必须留着。
    let manual_dropped = rt.manual_selected_id.filter(|m| *m != target_id);
    if switch_to(
        ctx,
        c,
        &g,
        target_id,
        &target_tag,
        now,
        manual_dropped,
        &mut out,
    )
    .await?
    {
        tracing::info!(from = %sel_tag, to = %target_tag, "住宅出口切换");
        if let Some(m) = manual_dropped {
            tracing::info!(manual = %m, to = %target_tag, "手动锁定的出口已不健康，自动切换并解除锁定");
            out.notes.push(format!(
                "手动锁定的出口已不健康，已自动切到 {target_tag} 并解除手动锁定"
            ));
        }
    }
    Ok(out)
}

/// 一轮「更优候选」的判定结果
struct Improve {
    /// 本轮的更优候选（`None` = 没有，计数要归零）
    cand: Option<Uuid>,
    /// 连续满足改善阈值的轮数
    rounds: u32,
    /// 本轮就该切到它（防抖攒满，或手动锁定压过防抖）
    go: Option<Uuid>,
}

/// 纯判定：本轮有没有明显更优的候选、攒到第几轮、该不该切。
/// **候选一换就从 1 数起**——三条上游轮流各赢一轮，不该凑成「连续 3 轮」。
fn improve_decision(
    g: &ResidentialGroup,
    r: &ResiRuntime,
    healthy: &[Uuid],
    cur: Uuid,
    now: OffsetDateTime,
) -> Improve {
    let none = Improve {
        cand: None,
        rounds: 0,
        go: None,
    };
    // 目标本来就是当前出口 ⇒ 没有候选。手动锁定时 `pick_target` 直接返回锁定目标，
    // 所以「锁定的那条不会被更快的候选抢走」自动成立（R2 ① 的优先级不变）；反过来
    // 锁定目标还不是当前出口时也**不**给它开后门立即切 —— 那会改掉 R2 ① 的既有语义
    // （当前出口健康就不动，等它不健康时由规则 8 把锁定目标放回去）
    let Some(cand) = pick_target(g, r, healthy, now).filter(|t| *t != cur) else {
        return none;
    };
    if !is_improvement(r, cand, cur) {
        return none;
    }
    let rounds = if r.improve_candidate_id == Some(cand) {
        r.improve_rounds + 1
    } else {
        1
    };
    Improve {
        cand: Some(cand),
        rounds,
        go: (rounds >= SWITCH_IMPROVE_ROUNDS).then_some(cand),
    }
}

/// 距上次切换还差多少秒才够 [`SWITCH_MIN_INTERVAL_SECS`]（`None` = 不限速）。
/// 规则 9 与规则 6c 的「更优候选」共用：两者都是真切换，都会掐住宅连接。
fn switch_cooldown(r: &ResiRuntime, now: OffsetDateTime) -> Option<i64> {
    let since = r
        .last_switch_at
        .as_deref()
        .and_then(parse_rfc3339)
        .map(|t| (now - t).whole_seconds())
        // 时钟回跳（NTP 校时）不该把限速锁死
        .map(|s| if s < 0 { i64::MAX } else { s })
        .unwrap_or(i64::MAX);
    (since < SWITCH_MIN_INTERVAL_SECS).then_some(SWITCH_MIN_INTERVAL_SECS - since)
}

/// 共用的「切换落账」：Clash PUT 成功 → 写 runtime（当前出口 / 限速游标 / 解除已失效的
/// 手动锁定 / 归零防抖计数）并返回 `true`；失败 → 记全局告警、**不改 runtime** 并返回
/// `false`（别把没生效的选择记成生效，下一轮重新评估）。
/// 规则 10/11 与规则 6c 两条切换路径共用它，免得落账口径有两份。
#[allow(clippy::too_many_arguments)]
async fn switch_to(
    ctx: &DaemonCtx,
    c: Arc<dyn Clash>,
    g: &ResidentialGroup,
    target_id: Uuid,
    target_tag: &str,
    now: OffsetDateTime,
    manual_dropped: Option<Uuid>,
    out: &mut RoundOutcome,
) -> anyhow::Result<bool> {
    let (cc, t2) = (c, target_tag.to_string());
    match tokio::task::spawn_blocking(move || cc.select(POOL, &t2)).await? {
        Ok(()) => {
            let pending = Some(target_id) != g.selected_upstream_id;
            state::update(&ctx.runtime, move |r| {
                r.selected_upstream_id = Some(target_id);
                r.last_switch_at = Some(fmt_rfc3339(now));
                r.selected_pending_persist = pending;
                if manual_dropped.is_some() {
                    r.manual_selected_id = None;
                }
                // 切完归零：下一次「更优」要重新攒满 SWITCH_IMPROVE_ROUNDS 轮
                r.improve_candidate_id = None;
                r.improve_rounds = 0;
            })
            .await;
            out.switched_to = Some(target_tag.to_string());
            Ok(true)
        }
        Err(e) => {
            persist_alerts(ctx, &[format!("切换住宅出口到 {target_tag} 失败：{e}")]).await;
            out.notes.push(format!("切换到 {target_tag} 失败：{e}"));
            Ok(false)
        }
    }
}

/// 一轮全量测速：并发跑完（上限 [`super::PROBE_CONCURRENCY`]）后**一次**写盘。
/// 失败只写 `note`，既不入样本也不碰健康位（主理人口径）。
async fn run_speedtest(
    ctx: &DaemonCtx,
    p: Arc<dyn Prober>,
    ups: &[Upstream],
    down: u64,
    up: u64,
    now: OffsetDateTime,
) {
    let pp = p.clone();
    let results = super::fanout(ups.to_vec(), move |u| pp.speedtest(&u, down, up)).await;
    let samples: Vec<(Uuid, proxy::SpeedSample)> = ups
        .iter()
        .zip(results)
        .map(|(u, r)| {
            (
                u.id,
                r.unwrap_or_else(|| proxy::SpeedSample {
                    note: Some("测速任务异常结束".into()),
                    ..Default::default()
                }),
            )
        })
        .collect();
    state::update(&ctx.runtime, move |r| {
        for (id, s) in samples {
            let h = r.health.entry(id.to_string()).or_default();
            state::record_speed(h, s.down_mbps, s.up_mbps, s.note, now);
        }
        r.last_speedtest_at = Some(fmt_rfc3339(now));
    })
    .await;
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

/// relay 重启后重放的退避：首个间隔 0.3 秒、此后翻倍（0.3/0.6/1.2/2.4/4.8…），
/// 最后一段截到 [`REPLAY_RETRY_BUDGET`] 用完为止
const REPLAY_RETRY_FIRST: Duration = Duration::from_millis(300);
/// 重放的退避总预算（从第一次尝试算起）。Clash API 单次调用另有
/// [`super::CLASH_TIMEOUT_SECS`] 的超时，最后一次尝试本身的耗时不计在内
pub const REPLAY_RETRY_BUDGET: Duration = Duration::from_secs(15);
/// 重放最终失败那条告警的固定前缀：下一次重放全部成功时按它认领、清掉
const REPLAY_FAIL_ALERT: &str = "relay 重启后重放住宅出口选择失败";

/// [`replay_after_restart`] 的结论（给测试与日志）
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReplayOutcome {
    /// 重放成功的 `(selector, tag)`
    pub replayed: Vec<(String, String)>,
    /// 最终仍失败的 `(selector, tag, 原因)`
    pub failed: Vec<(String, String, String)>,
}

/// 一条要重放的 selector 选择
#[derive(Debug, Clone)]
struct ReplayItem {
    selector: String,
    tag: String,
    /// 槽 selector 才有：`(槽序号, 事件时 runtime 记的 current_upstream_id, 重放目标)`
    slot: Option<(u16, Option<Uuid>, Uuid)>,
}

/// relay 重启后该重放哪些 selector（纯函数；tag 现算，runtime 只存 uuid，§C）：
/// - 全局 [`POOL`] → `runtime.selected_upstream_id`（还在池里就重放，与以前一样不比 default）；
/// - 每槽 `slot-<i>-pool` → 手动 pin（还在池里）优先，否则 `current_upstream_id`；
///   与本槽自己的 IP（selector 的 `default`）相同的不必重放。
fn replay_plan(g: &ResidentialGroup, view: &[Slot], r: &ResiRuntime) -> Vec<ReplayItem> {
    let in_pool = |id: Uuid| g.upstreams.iter().any(|u| u.id == id);
    let mut plan = Vec::new();
    if let Some(id) = r.selected_upstream_id {
        match clash::tag_of(g, id) {
            Some(tag) => plan.push(ReplayItem {
                selector: POOL.to_string(),
                tag,
                slot: None,
            }),
            None => tracing::warn!(%id, "运行时选中的上游已不在池里，跳过重放"),
        }
    }
    for s in view {
        // 本槽 IP 不在池里 ⇒ relay 没渲染这个 selector（slot_view 已过滤掉）
        if !in_pool(s.upstream_id) {
            continue;
        }
        let Some(sr) = r.slots.get(&s.index.to_string()) else {
            continue;
        };
        let want = sr
            .pinned_upstream_id
            .filter(|p| in_pool(*p))
            .or(sr.current_upstream_id);
        let Some(want) = want.filter(|w| *w != s.upstream_id) else {
            continue;
        };
        let Some(tag) = clash::tag_of(g, want) else {
            continue;
        };
        plan.push(ReplayItem {
            selector: super::slot_selector(s.index),
            tag,
            slot: Some((s.index, sr.current_upstream_id, want)),
        });
    }
    plan
}

/// 逐轮重放：每轮先探 Clash API 可用（[`Clash::ready`]）再逐条 select。连接类失败
/// （探不通、[`clash::ClashError::Unreachable`]）按 0.3/0.6/1.2/… 秒退避重试，总时长不超过
/// [`REPLAY_RETRY_BUDGET`]；Clash 明确拒绝（`Rejected`：selector / 成员不存在）重试也没用，
/// 当轮就记失败。返回 `(成功, 失败 + 原因, 尝试次数)`。
async fn select_with_retry(
    c: Arc<dyn Clash>,
    plan: Vec<ReplayItem>,
) -> (Vec<ReplayItem>, Vec<(ReplayItem, String)>, u32) {
    let deadline = tokio::time::Instant::now() + REPLAY_RETRY_BUDGET;
    let mut delay = REPLAY_RETRY_FIRST;
    let (mut pending, mut done, mut failed) = (plan, Vec::new(), Vec::new());
    let mut attempts = 0u32;
    loop {
        attempts += 1;
        let (cc, batch) = (c.clone(), pending.clone());
        let res = tokio::task::spawn_blocking(move || {
            cc.ready().then(|| {
                batch
                    .iter()
                    .map(|i| cc.select(&i.selector, &i.tag))
                    .collect::<Vec<_>>()
            })
        })
        .await;
        let mut why = "Clash API 未就绪（relay 多半刚重启、还没起监听）".to_string();
        match res {
            Ok(Some(results)) => {
                let mut again = Vec::new();
                for (item, r) in pending.drain(..).zip(results) {
                    match r {
                        Ok(()) => done.push(item),
                        Err(e @ clash::ClashError::Unreachable(_)) => {
                            why = e.to_string();
                            again.push(item);
                        }
                        Err(e) => failed.push((item, e.to_string())),
                    }
                }
                pending = again;
            }
            Ok(None) => {}
            // 阻塞任务 panic：重试也只会再炸一次
            Err(e) => failed.extend(pending.drain(..).map(|i| (i, format!("重放任务异常：{e}")))),
        }
        if pending.is_empty() {
            return (done, failed, attempts);
        }
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            failed.extend(pending.into_iter().map(|i| (i, why.clone())));
            return (done, failed, attempts);
        }
        let wait = delay.min(left);
        tracing::debug!(wait = ?wait, pending = pending.len(), reason = %why, "Clash API 暂时连不上，退避后重放");
        tokio::time::sleep(wait).await;
        delay *= 2;
    }
}

/// relay 重启后把选择重放回去（spec §5.3 最后一句 + §5.6）：全局 [`POOL`] 与每个借用 /
/// pin 中的槽 selector。
///
/// 2026-09-13 04:05:47 UTC bwg-rick 实录：每日黑名单批量重启 b-ui-relay 后紧接着重放，
/// 那一刻 Clash API 还没起监听，一次失败就放弃了；槽 selector 也从没重放过（借用 / pin
/// 中的槽被 relay 打回默认）。所以每轮先探 Clash API 可用再 select，连接类失败按退避
/// 重试（[`select_with_retry`]）。
///
/// 落账：
/// - 不需要重放的槽，`current_upstream_id` 照旧清零 ⇒ 下一轮 `drive_slots` 无条件按本槽
///   优先重 PUT 一次，不去赌 sing-box 的 `cache_file` 有没有把选择持久化下来；
/// - 重放成功的槽保留 current（借用中的切回防抖不因 relay 重启被绕过）；
/// - 最终失败的槽清零、全局选择不动，并记一条告警 —— 下一轮巡检由现有逻辑兜底
///   （`drive_slots` 重 PUT；规则 6a 发现 now 与运行时不一致就重放）；
/// - `back_rounds` 一律不清：它记的是「本槽自己健康了几轮」，与 relay 重启无关。
pub async fn replay_after_restart(ctx: &DaemonCtx, c: Arc<dyn Clash>) -> ReplayOutcome {
    let (g, view) = {
        let s = ctx.store.read().await;
        (
            state::group_of(&s),
            bui_schema::slots::sorted(&s.residential),
        )
    };
    // 池无效时 relay 里一个 selector 都没有（fail-open 直连），没什么可重放
    let plan = if g.pool_active() {
        replay_plan(&g, &view, &state::read(&ctx.runtime).await)
    } else {
        Vec::new()
    };
    let kept: BTreeSet<String> = plan
        .iter()
        .filter_map(|i| i.slot)
        .map(|(idx, _, _)| idx.to_string())
        .collect();
    state::update(&ctx.runtime, move |r| {
        for (k, s) in r.slots.iter_mut() {
            if !kept.contains(k) {
                s.current_upstream_id = None;
            }
        }
    })
    .await;
    if plan.is_empty() {
        return ReplayOutcome::default();
    }

    let (done, failed, attempts) = select_with_retry(c, plan).await;
    let out = ReplayOutcome {
        replayed: done
            .iter()
            .map(|i| (i.selector.clone(), i.tag.clone()))
            .collect(),
        failed: failed
            .iter()
            .map(|(i, e)| (i.selector.clone(), i.tag.clone(), e.clone()))
            .collect(),
    };
    // 槽的落账与告警并成一次写。重放期间巡检也可能动过某一槽（drive_slots 同样会 PUT）：
    // 只在 current 还是事件时那个值时才改，别覆盖它
    let slot_writes: Vec<(u16, Option<Uuid>, Option<Uuid>)> = done
        .iter()
        .filter_map(|i| i.slot)
        .map(|(idx, seen, want)| (idx, seen, Some(want)))
        .chain(
            failed
                .iter()
                .filter_map(|(i, _)| i.slot)
                .map(|(idx, seen, _)| (idx, seen, None)),
        )
        .collect();
    let failed_list = out
        .failed
        .iter()
        .map(|(s, t, e)| format!("{s} → {t}：{e}"))
        .collect::<Vec<_>>()
        .join("；");
    let alert = (!out.failed.is_empty())
        .then(|| format!("{REPLAY_FAIL_ALERT}（{failed_list}），下一轮巡检兜底"));
    state::update(&ctx.runtime, move |r| {
        for (idx, seen, landed) in slot_writes {
            if let Some(e) = r.slots.get_mut(&idx.to_string()) {
                if e.current_upstream_id == seen {
                    e.current_upstream_id = landed;
                }
            }
        }
        // 只留最新一次的结论：全部成功 ⇒ 认领以前的失败告警
        state::remove_alerts_with_prefix(r, REPLAY_FAIL_ALERT);
        if let Some(a) = alert {
            state::push_alert(r, a);
        }
    })
    .await;

    let replayed = out
        .replayed
        .iter()
        .map(|(s, t)| format!("{s} → {t}"))
        .collect::<Vec<_>>()
        .join("；");
    if out.failed.is_empty() {
        tracing::info!(%replayed, attempts, "relay 重启后已重放住宅出口选择");
    } else {
        tracing::warn!(%replayed, failed = %failed_list, attempts, "relay 重启后重放住宅出口选择失败，下一轮巡检兜底");
    }
    out
}

/// relay 重启后重放选择（spec §5.3 最后一句），做法见 [`replay_after_restart`]。
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
                replay_after_restart(&ctx, c.clone()).await;
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
    tokio::task::spawn_blocking(move || cc.select(POOL, &t2)).await??;
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
    #[derive(Default)]
    struct ByHost {
        bad: std::collections::BTreeSet<String>,
        auth_failed: std::collections::BTreeSet<String>,
        /// 经这些上游打 Google 搜索回 403（调研 §D 的 Bright Data 形态）
        google_blocked: std::collections::BTreeSet<String>,
        /// host → (HTTP 往返 ms, TCP 建连 ms)；不在表里 = 没测到
        latency: std::collections::BTreeMap<String, (u64, u64)>,
        /// host → (下行 Mbps, 上行 Mbps)
        speed: std::collections::BTreeMap<String, (f64, f64)>,
        /// host → UDP 探测结果（http 上游由 `proxy::http_no_udp` 兜底，不看这张表）
        udp: std::collections::BTreeMap<String, proxy::UdpProbe>,
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
        fn gateway_tcp_ms(&self, up: &Upstream) -> Option<u64> {
            self.latency.get(&up.host).map(|(_, tcp)| *tcp)
        }
        fn timed_get(
            &self,
            up: &Upstream,
            url: &str,
        ) -> (Option<u64>, Result<HttpProbe, ProbeError>) {
            let r = self.get(up, url);
            // 失败不给耗时（超时值不是延迟），与真实 Prober 同口径
            let ms = r
                .is_ok()
                .then(|| self.latency.get(&up.host).map(|(http, _)| *http))
                .flatten();
            (ms, r)
        }
        fn stun_binding(&self, up: &Upstream) -> proxy::UdpProbe {
            // 「HTTP 上游没有 UDP」是协议事实，测试替身也不许伪造成通
            proxy::http_no_udp(up)
                .or_else(|| self.udp.get(&up.host).cloned())
                .unwrap_or_default()
        }
        fn speedtest(&self, up: &Upstream, _d: u64, _u: u64) -> proxy::SpeedSample {
            match self.speed.get(&up.host) {
                Some((d, u)) => proxy::SpeedSample {
                    down_mbps: Some(*d),
                    up_mbps: Some(*u),
                    note: None,
                },
                None => proxy::SpeedSample {
                    note: Some("测速目标不可达".into()),
                    ..Default::default()
                },
            }
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
            ..Default::default()
        })
    }
    /// 按 `isp{i}.example.net` 编好延迟与速度的 prober：`metrics[i] = (http_ms, tcp_ms, down, up)`
    fn by_host_with_metrics(metrics: &[(u64, u64, f64, f64)]) -> Arc<dyn Prober> {
        let mut p = ByHost::default();
        for (i, (http, tcp, d, u)) in metrics.iter().enumerate() {
            let host = format!("isp{}.example.net", i + 1);
            p.latency.insert(host.clone(), (*http, *tcp));
            p.speed.insert(host, (*d, *u));
        }
        Arc::new(p)
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
        assert_eq!(clash.calls(), vec!["get:resi-pool"], "不发 PUT");
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
        assert_eq!(clash.selected(POOL).as_deref(), Some("resi-2"));
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
        assert_eq!(clash.calls().last().unwrap(), "put:resi-pool:resi-3");
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
    async fn a_round_purges_legacy_and_orphan_alerts_and_clears_the_live_ones_on_success() {
        // 真机（bwg-rick 升级到含 R1 的 v4 后）：三条 Decodo 连续成功 109 次、体检全通，
        // status 仍挂着 5 条 R1 之前按位置名写进 alerts 的旧告警。一轮巡检就该清干净。
        let d = tempfile::tempdir().unwrap();
        let (c, _host) = ctx(&d, &[10, 20]).await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        let live = Uuid::from_u128(1);
        let gone = Uuid::from_u128(0xdead);
        rstate::update(&c.runtime, |r| {
            rstate::push_alert(r, "上游 url-3 凭据失效（407）");
            rstate::push_alert(
                r,
                "上游 url-6 凭据失效（407 / SOCKS5 认证被拒），请更新凭据",
            );
            rstate::set_upstream_alert(r, gone, "上游 old.example.net:10007 凭据失效（407）");
            rstate::set_upstream_alert(r, live, "上游 isp1.example.net:10007 凭据失效（407）");
        })
        .await;

        check_once(&c, by_host(&[], &[]), clash.clone())
            .await
            .unwrap();
        let g = rstate::group_of(&*c.store.read().await);
        let r = rstate::read(&c.runtime).await;
        assert!(
            r.alerts.is_empty() && r.upstream_alerts.is_empty(),
            "遗留/孤儿告警清掉，探通的那条也消警：{:?} / {:?}",
            r.alerts,
            r.upstream_alerts
        );
        assert!(rstate::visible_alerts(&g, &r).is_empty());
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
            clash.selected(POOL).as_deref(),
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
            clash.selected(POOL).as_deref(),
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
    async fn a_round_records_latency_and_runs_the_first_speedtest() {
        // 主理人 2026-09-12：「巡检除了连通健康度，还要给出延迟、上下行速度」
        let d = tempfile::tempdir().unwrap();
        let (c, _h) = ctx(&d, &[10, 20]).await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        let p = by_host_with_metrics(&[(90, 30, 50.0, 10.0), (120, 40, 20.0, 5.0)]);
        check_once(&c, p, clash.clone()).await.unwrap();
        let r = rstate::read(&c.runtime).await;
        let h1 = &r.health[&Uuid::from_u128(1).to_string()];
        assert_eq!(h1.http_ms, vec![90], "经上游的完整往返耗时");
        assert_eq!(h1.tcp_ms, vec![30], "到上游网关的 TCP 建连耗时，单独存");
        assert_eq!(rstate::latency_p50(h1), Some(90));
        // 第一轮（`last_speedtest_at` 还是 None）必须测一次速，否则面板上永远是「未测」
        assert_eq!(h1.down_mbps, vec![50.0]);
        assert_eq!(h1.up_mbps, vec![10.0]);
        assert!(h1.speed_at.is_some());
        assert!(r.last_speedtest_at.is_some(), "测速游标要落账");
        let h2 = &r.health[&Uuid::from_u128(2).to_string()];
        assert_eq!(h2.http_ms, vec![120], "每个成员各自记");
        assert_eq!(h2.down_mbps, vec![20.0]);
    }

    #[tokio::test]
    async fn the_speedtest_runs_once_an_hour_not_once_a_round() {
        // 每 2 分钟测一次 4MB 就是 每月 86GB，只能每小时一次（spec §5.3 的流量账）
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d, &[10]).await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        let p = by_host_with_metrics(&[(90, 30, 50.0, 10.0)]);
        check_once(&c, p.clone(), clash.clone()).await.unwrap();
        let key = Uuid::from_u128(1).to_string();
        assert_eq!(
            rstate::read(&c.runtime).await.health[&key].down_mbps.len(),
            1
        );
        host.advance(120); // 下一轮巡检
        check_once(&c, p.clone(), clash.clone()).await.unwrap();
        let r = rstate::read(&c.runtime).await;
        assert_eq!(r.health[&key].down_mbps.len(), 1, "2 分钟后不重复测速");
        assert_eq!(r.health[&key].http_ms.len(), 2, "延迟仍是每轮都记");
        host.advance(60 * 60); // 满一小时
        check_once(&c, p, clash).await.unwrap();
        assert_eq!(
            rstate::read(&c.runtime).await.health[&key].down_mbps.len(),
            2,
            "满 60 分钟再测一次"
        );
    }

    #[tokio::test]
    async fn a_failed_speedtest_only_leaves_a_note_and_never_touches_health() {
        let d = tempfile::tempdir().unwrap();
        let (c, _h) = ctx(&d, &[10]).await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        // `by_host` 没编 speed 表 ⇒ 测速回 note，但连通性与 Google 都是通的
        let out = check_once(&c, by_host(&[], &[]), clash).await.unwrap();
        assert_eq!(out.healthy, vec!["resi-1"], "测速失败不影响健康判定");
        let h = &rstate::read(&c.runtime).await.health[&Uuid::from_u128(1).to_string()];
        assert!(h.active);
        assert!(h.down_mbps.is_empty());
        assert_eq!(h.speed_note.as_deref(), Some("测速目标不可达"));
    }

    #[tokio::test]
    async fn a_socks5_member_records_the_udp_exit_ip_while_an_http_member_is_marked_no_udp() {
        // 主理人 2026-09-12 追加：「巡检也要测 UDP」。http 上游恒不通 + 标注
        let d = tempfile::tempdir().unwrap();
        let (c, _h) = ctx(&d, &[10, 20]).await;
        // resi-1 换成 socks5（ctx() 建的是 http），resi-2 保持 http 做对照
        rstate::update_group(&c.store, &c.bus, |g| {
            g.upstreams[0].kind = UpstreamKind::Socks5;
        })
        .await
        .unwrap();
        let mut byh = ByHost::default();
        byh.udp.insert(
            "isp1.example.net".into(),
            proxy::UdpProbe {
                ok: true,
                exit_ip: Some("198.51.100.7".into()),
                ms: Some(42),
                note: None,
            },
        );
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        check_once(&c, Arc::new(byh), clash).await.unwrap();
        let r = rstate::read(&c.runtime).await;
        let h1 = &r.health[&Uuid::from_u128(1).to_string()];
        assert_eq!(h1.udp_ok, Some(true));
        assert_eq!(h1.udp_exit_ip.as_deref(), Some("198.51.100.7"));
        assert_eq!(h1.udp_ms, vec![42], "UDP 耗时与 TCP/HTTP 分开报");
        assert!(h1.udp_at.is_some());
        let h2 = &r.health[&Uuid::from_u128(2).to_string()];
        assert_eq!(h2.udp_ok, Some(false));
        assert_eq!(h2.udp_note.as_deref(), Some("HTTP 上游无 UDP"));
        assert!(h2.udp_ms.is_empty());
    }

    #[tokio::test]
    async fn a_udp_timeout_is_recorded_as_not_ok_with_the_reason() {
        let d = tempfile::tempdir().unwrap();
        let (c, _h) = ctx(&d, &[10]).await;
        rstate::update_group(&c.store, &c.bus, |g| {
            g.upstreams[0].kind = UpstreamKind::Socks5;
        })
        .await
        .unwrap();
        let mut byh = ByHost::default();
        byh.udp.insert(
            "isp1.example.net".into(),
            proxy::UdpProbe {
                ok: false,
                exit_ip: None,
                ms: None,
                note: Some("STUN 无回复（5s 内）：timed out".into()),
            },
        );
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        let out = check_once(&c, Arc::new(byh), clash).await.unwrap();
        assert_eq!(out.healthy, vec!["resi-1"], "UDP 不通不影响 TCP 健康判定");
        let h = &rstate::read(&c.runtime).await.health[&Uuid::from_u128(1).to_string()];
        assert_eq!(h.udp_ok, Some(false));
        assert!(h.udp_note.as_deref().unwrap().contains("无回复"));
        assert!(h.udp_ms.is_empty(), "超时的 5 秒不是延迟");
    }

    #[test]
    fn pick_target_orders_by_latency_then_download_speed() {
        // 主理人 2026-09-12：「选择最健康、最低延迟、速度最快的住宅代理」
        let mut g = rstate::group_of(&crate::modules::residential::sample_state_with_pool());
        g.upstreams = vec![upstream(1, 10), upstream(2, 10), upstream(3, 10)];
        let now = time::macros::datetime!(2026-09-12 00:00:00 UTC);
        let healthy = vec![Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3)];
        // 同优先级：u2 延迟最低 ⇒ 它赢
        let mut r = ResiRuntime::default();
        for (i, ms) in [(1u128, 200u64), (2, 80), (3, 150)] {
            let h = r.health.entry(Uuid::from_u128(i).to_string()).or_default();
            rstate::record_latency(h, Some(ms), None, None);
        }
        assert_eq!(pick_target(&g, &r, &healthy, now), Some(Uuid::from_u128(2)));
        // 延迟齐平后比下行：u3 最快
        for (i, ms) in [(1u128, 80u64), (3, 80)] {
            let h = r.health.entry(Uuid::from_u128(i).to_string()).or_default();
            h.http_ms = vec![ms];
        }
        for (i, d) in [(1u128, 10.0), (2, 20.0), (3, 90.0)] {
            let h = r.health.entry(Uuid::from_u128(i).to_string()).or_default();
            rstate::record_speed(h, Some(d), Some(d / 2.0), None, now);
        }
        assert_eq!(
            pick_target(&g, &r, &healthy, now),
            Some(Uuid::from_u128(3)),
            "延迟相同 ⇒ 下行最快的赢"
        );
        // priority 仍压过延迟与速度（它是更靠前的排序键）
        g.upstreams[0].priority = 1;
        assert_eq!(pick_target(&g, &r, &healthy, now), Some(Uuid::from_u128(1)));
        // 没测过延迟的成员不许凭空赢过测过的（未知 ≠ 0 毫秒）
        g.upstreams[0].priority = 10;
        let mut unknown = ResiRuntime::default();
        let h = unknown
            .health
            .entry(Uuid::from_u128(2).to_string())
            .or_default();
        rstate::record_latency(h, Some(500), None, None);
        assert_eq!(
            pick_target(&g, &unknown, &healthy, now),
            Some(Uuid::from_u128(2)),
            "只有 u2 有延迟数据 ⇒ 选它，不选「未知」"
        );
    }

    #[test]
    fn pick_target_puts_a_working_udp_upstream_ahead_of_one_without() {
        // 排序键：Google → priority → UDP → 延迟 → 下行（主理人 2026-09-12 的口径）。
        // 所以 UDP 只在**同优先级**里说话，跨优先级由管理员给的 priority 说话
        let mut g = rstate::group_of(&crate::modules::residential::sample_state_with_pool());
        g.upstreams = vec![upstream(1, 5), upstream(2, 20)];
        let now = time::macros::datetime!(2026-09-12 00:00:00 UTC);
        let healthy = vec![Uuid::from_u128(1), Uuid::from_u128(2)];
        let mk = |udp: [Option<bool>; 2], google: [Option<bool>; 2]| {
            let mut r = ResiRuntime::default();
            for i in 0..2 {
                let h = r
                    .health
                    .entry(Uuid::from_u128(i as u128 + 1).to_string())
                    .or_default();
                h.udp_ok = udp[i];
                rstate::record_google(h, google[i], now);
            }
            r
        };
        // 同优先级：u1 的 UDP 不通 ⇒ 让给 UDP 通的 u2
        g.upstreams[1].priority = 5;
        assert_eq!(
            pick_target(
                &g,
                &mk([Some(false), Some(true)], [Some(true), Some(true)]),
                &healthy,
                now
            ),
            Some(Uuid::from_u128(2)),
            "同优先级里 UDP 通的优先"
        );
        // 两条都通 ⇒ 回到池内下标（都同优先级、都没测过延迟与速度）
        assert_eq!(
            pick_target(
                &g,
                &mk([Some(true), Some(true)], [Some(true), Some(true)]),
                &healthy,
                now
            ),
            Some(Uuid::from_u128(1))
        );
        // priority 压过 UDP：管理员把 u1 排在前面，它 UDP 不通也照样是它
        g.upstreams[1].priority = 20;
        assert_eq!(
            pick_target(
                &g,
                &mk([Some(false), Some(true)], [Some(true), Some(true)]),
                &healthy,
                now
            ),
            Some(Uuid::from_u128(1)),
            "priority 是比 UDP 更靠前的排序键"
        );
        // Google 仍压过一切（priority 也压不过它）：u2 的 priority 更好、UDP 也通，
        // 但 Google 被封 ⇒ 选 Google 通的 u1
        g.upstreams[0].priority = 20;
        g.upstreams[1].priority = 5;
        assert_eq!(
            pick_target(
                &g,
                &mk([Some(false), Some(true)], [Some(true), Some(false)]),
                &healthy,
                now
            ),
            Some(Uuid::from_u128(1)),
            "Google 可用是第一排序键"
        );
    }

    #[test]
    fn the_improvement_threshold_is_twenty_percent_latency_or_thirty_percent_download() {
        let mut r = ResiRuntime::default();
        let (cand, cur) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let set = |r: &mut ResiRuntime, id: Uuid, ms: u64, down: f64| {
            let h = r.health.entry(id.to_string()).or_default();
            h.http_ms = vec![ms];
            h.down_mbps = vec![down];
        };
        // 两项都没数据 ⇒ 不算更优（未知不许赢）
        assert!(!is_improvement(&r, cand, cur));
        set(&mut r, cur, 100, 10.0);
        set(&mut r, cand, 81, 10.9);
        assert!(!is_improvement(&r, cand, cur), "19% / 9% 都不够");
        set(&mut r, cand, 80, 10.0);
        assert!(is_improvement(&r, cand, cur), "延迟正好低 20%");
        set(&mut r, cand, 100, 13.0);
        assert!(is_improvement(&r, cand, cur), "下行正好快 30%");
        set(&mut r, cand, 200, 5.0);
        assert!(!is_improvement(&r, cand, cur), "更差当然不算更优");
    }

    #[tokio::test]
    async fn a_better_candidate_only_wins_after_three_consecutive_rounds() {
        // 防抖（主理人 2026-09-12）：当前出口**健康**时也可以按延迟/速度切，但两条
        // 上游的延迟会互相交替领先，不防抖就会每 2 分钟掐一次住宅连接
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d, &[10, 10]).await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        // resi-2 延迟只有 resi-1 的一半 ⇒ 每轮都满足「低 ≥20%」
        let p = by_host_with_metrics(&[(200, 50, 10.0, 5.0), (80, 20, 12.0, 6.0)]);
        for round in 1..=2 {
            let out = check_once(&c, p.clone(), clash.clone()).await.unwrap();
            assert_eq!(out.switched_to, None, "第 {round} 轮还在防抖里");
            assert!(
                out.notes.iter().any(|n| n.contains(&format!("{round}/3"))),
                "要说清攒到第几轮了：{:?}",
                out.notes
            );
            assert_eq!(
                rstate::read(&c.runtime).await.improve_rounds,
                round,
                "轮数记在 runtime 里"
            );
            host.advance(120);
        }
        let out = check_once(&c, p, clash.clone()).await.unwrap();
        assert_eq!(
            out.switched_to.as_deref(),
            Some("resi-2"),
            "连续 3 轮更优 ⇒ 切"
        );
        assert_eq!(clash.calls().last().unwrap(), "put:resi-pool:resi-2");
        let r = rstate::read(&c.runtime).await;
        assert_eq!(r.selected_upstream_id, Some(Uuid::from_u128(2)));
        assert_eq!(r.improve_rounds, 0, "切完归零，下一次要重新攒");
        assert!(r.last_switch_at.is_some(), "这是一次真切换，吃 60s 限速");
    }

    #[tokio::test]
    async fn a_candidate_that_stops_being_better_resets_the_streak() {
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d, &[10, 10]).await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        let fast = by_host_with_metrics(&[(200, 50, 10.0, 5.0), (80, 20, 12.0, 6.0)]);
        check_once(&c, fast.clone(), clash.clone()).await.unwrap();
        assert_eq!(rstate::read(&c.runtime).await.improve_rounds, 1);
        host.advance(120);
        // 这一轮两条延迟拉平 ⇒ 不再更优，计数归零
        let even = by_host_with_metrics(&[(90, 30, 10.0, 5.0), (90, 30, 10.0, 5.0)]);
        let out = check_once(&c, even, clash.clone()).await.unwrap();
        assert_eq!(out.switched_to, None);
        let r = rstate::read(&c.runtime).await;
        assert_eq!(r.improve_rounds, 0, "断了就归零");
        assert_eq!(r.improve_candidate_id, None);
        host.advance(120);
        // 再更优也得从 1 数起（不是接着 2）
        let out = check_once(&c, fast, clash).await.unwrap();
        assert_eq!(out.switched_to, None);
        assert_eq!(rstate::read(&c.runtime).await.improve_rounds, 1);
    }

    #[tokio::test]
    async fn a_manually_locked_upstream_is_not_stolen_by_a_faster_candidate() {
        // R2 ① 的优先级不变：手动锁定的出口再慢也不许被延迟/速度抢走
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d, &[10, 10]).await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        select_manual(&c, clash.clone(), Uuid::from_u128(1))
            .await
            .unwrap();
        let p = by_host_with_metrics(&[(200, 50, 10.0, 5.0), (80, 20, 99.0, 9.0)]);
        for _ in 0..4 {
            host.advance(120);
            let out = check_once(&c, p.clone(), clash.clone()).await.unwrap();
            assert_eq!(out.switched_to, None, "手动锁定压过延迟/速度");
        }
        assert_eq!(clash.selected(POOL).as_deref(), Some("resi-1"));
        assert_eq!(rstate::read(&c.runtime).await.improve_rounds, 0);
    }

    #[tokio::test]
    async fn an_unhealthy_current_exit_switches_immediately_without_the_debounce() {
        // 「当前出口不健康或 Google 封时立即切，沿用既有规则」：防抖只管「更优」这条新路径
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d, &[10, 10]).await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        let mut byh = ByHost {
            bad: ["isp1.example.net".to_string()].into_iter().collect(),
            ..Default::default()
        };
        // resi-2 延迟更差：不是「更优候选」，但当前出口坏了照样要切
        byh.latency.insert("isp2.example.net".into(), (900, 300));
        let p: Arc<dyn Prober> = Arc::new(byh);
        check_once(&c, p.clone(), clash.clone()).await.unwrap();
        host.advance(120);
        let out = check_once(&c, p, clash).await.unwrap();
        assert_eq!(
            out.switched_to.as_deref(),
            Some("resi-2"),
            "当前不健康 ⇒ 不吃防抖，立刻切"
        );
    }

    #[tokio::test]
    async fn a_google_blocked_current_exit_switches_immediately_without_the_debounce() {
        // 主理人 2026-09-12：「当前不健康 / Google 封立即切」。resi-1 连通性没问题、
        // 延迟也更好（不是「更优候选」那条路径），但它封 Google ⇒ 本轮就得让位
        let d = tempfile::tempdir().unwrap();
        let (c, _h) = ctx(&d, &[10, 10]).await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        let out = check_once(
            &c,
            by_host_with_google(&[], &[], &["isp1.example.net"]),
            clash.clone(),
        )
        .await
        .unwrap();
        assert_eq!(
            out.switched_to.as_deref(),
            Some("resi-2"),
            "Google 被封 ⇒ 不吃防抖，立刻切：{:?}",
            out.notes
        );
        assert_eq!(rstate::read(&c.runtime).await.improve_rounds, 0);
    }

    #[tokio::test]
    async fn a_pool_where_every_member_blocks_google_keeps_the_current_exit() {
        // 全池都封 Google 时切不出更好的，切到自己身上只会白掐一次住宅连接
        let d = tempfile::tempdir().unwrap();
        let (c, _h) = ctx(&d, &[10, 10]).await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        let p = by_host_with_google(&[], &[], &["isp1.example.net", "isp2.example.net"]);
        let out = check_once(&c, p, clash.clone()).await.unwrap();
        assert_eq!(out.switched_to, None);
        assert_eq!(clash.selected(POOL).as_deref(), Some("resi-1"));
        assert!(
            out.notes.iter().any(|n| n.contains("没有别的 Google 可用")),
            "{:?}",
            out.notes
        );
    }

    #[tokio::test]
    async fn a_manual_lock_keeps_a_google_blocked_exit() {
        // R2 ①：手动锁定压过一切，包括「Google 封立即切」
        let d = tempfile::tempdir().unwrap();
        let (c, host) = ctx(&d, &[10, 10]).await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        select_manual(&c, clash.clone(), Uuid::from_u128(1))
            .await
            .unwrap();
        host.advance(120);
        let out = check_once(
            &c,
            by_host_with_google(&[], &[], &["isp1.example.net"]),
            clash.clone(),
        )
        .await
        .unwrap();
        assert_eq!(out.switched_to, None, "锁定的出口不因 Google 被封而挪走");
        assert_eq!(clash.selected(POOL).as_deref(), Some("resi-1"));
    }

    #[test]
    fn the_selection_reason_names_why_this_exit_is_the_one_in_use() {
        // 主理人 2026-09-12：「输出一行『当前选中 resi-N 的原因』」
        let mut g = rstate::group_of(&crate::modules::residential::sample_state_with_pool());
        g.upstreams = vec![upstream(1, 5), upstream(2, 20)];
        let now = time::macros::datetime!(2026-09-12 00:00:00 UTC);
        let (u1, u2) = (Uuid::from_u128(1), Uuid::from_u128(2));

        // 只有一个成员 ⇒ 唯一健康
        let mut one = g.clone();
        one.upstreams.truncate(1);
        assert!(selection_reason(&one, &ResiRuntime::default(), u1).contains("唯一"));
        // 手动锁定压过一切
        let locked = ResiRuntime {
            manual_selected_id: Some(u2),
            ..Default::default()
        };
        assert!(selection_reason(&g, &locked, u2).contains("手动锁定"));
        // 优先级最优
        assert!(selection_reason(&g, &ResiRuntime::default(), u1).contains("优先级"));
        // 同优先级 ⇒ 说延迟
        g.upstreams[1].priority = 5;
        let mut r = ResiRuntime::default();
        for (id, ms) in [(u1, 80u64), (u2, 200)] {
            let h = r.health.entry(id.to_string()).or_default();
            rstate::record_latency(h, Some(ms), None, None);
        }
        let why = selection_reason(&g, &r, u1);
        assert!(why.contains("延迟") && why.contains("80"), "{why}");
        // 延迟齐平 ⇒ 说下行
        r.health.get_mut(&u2.to_string()).unwrap().http_ms = vec![80];
        for (id, d) in [(u1, 90.0), (u2, 10.0)] {
            let h = r.health.entry(id.to_string()).or_default();
            rstate::record_speed(h, Some(d), Some(1.0), None, now);
        }
        assert!(selection_reason(&g, &r, u1).contains("下行"));
        // Google 被封的那条在场 ⇒ 先说 Google
        let mut blocked = r.clone();
        rstate::record_google(
            blocked.health.get_mut(&u2.to_string()).unwrap(),
            Some(false),
            now,
        );
        rstate::record_google(
            blocked.health.get_mut(&u1.to_string()).unwrap(),
            Some(true),
            now,
        );
        assert!(selection_reason(&g, &blocked, u1).contains("Google"));
        // 已被删出池的 uuid 不许 panic
        assert!(selection_reason(&g, &r, Uuid::from_u128(99)).contains("不在池里"));
    }

    #[test]
    fn the_speedtest_budget_comes_from_constants_and_can_be_overridden_by_state() {
        let mut s = crate::modules::residential::sample_state_with_pool();
        assert_eq!(
            speedtest_cfg(&s),
            (
                crate::modules::residential::SPEEDTEST_DOWN_BYTES,
                crate::modules::residential::SPEEDTEST_UP_BYTES,
                crate::modules::residential::SPEEDTEST_INTERVAL_MINS
            ),
            "默认 4MB / 1MB / 60min"
        );
        s.residential.speedtest_down_bytes = Some(1024);
        s.residential.speedtest_up_bytes = Some(512);
        s.residential.speedtest_interval_mins = Some(180);
        assert_eq!(speedtest_cfg(&s), (1024, 512, 180));
        // 0 与荒唐的大小不采信（打错一个 0 不该把上游流量打爆 / 把测速关死）
        s.residential.speedtest_down_bytes = Some(0);
        s.residential.speedtest_up_bytes = Some(1 << 40);
        s.residential.speedtest_interval_mins = Some(0);
        let (d, u, m) = speedtest_cfg(&s);
        assert_eq!(d, crate::modules::residential::SPEEDTEST_DOWN_BYTES);
        assert_eq!(u, crate::modules::residential::SPEEDTEST_MAX_BYTES);
        assert_eq!(m, crate::modules::residential::SPEEDTEST_INTERVAL_MINS);
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
            if clash.selected(POOL).as_deref() == Some("resi-2") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        task.abort();
        assert_eq!(
            clash.selected(POOL).as_deref(),
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
        assert_eq!(clash.selected(POOL).as_deref(), Some("resi-2"));
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

    /// 用本文件 `mod tests` 里**已有**的 `upstream(i, priority)` 直接拼一个组
    /// （`health.rs` 没有 `group_of_n` 这种夹具，只有 `upstream` 与 `ctx(&dir, &prios)`；
    /// 本测试是纯函数测试，不需要 `ctx` 那套 Store/Runtime）。
    #[test]
    fn rank_healthy_is_pick_target_extended_to_the_whole_list() {
        // resi-3 的 priority 最好（10）但 Google 被封；resi-1 / resi-2 同 priority，
        // resi-2 的 HTTP 往返更低
        let g = ResidentialGroup {
            enabled: true,
            upstreams: vec![upstream(1, 100), upstream(2, 100), upstream(3, 10)],
            ..Default::default()
        };
        let ids = ids_of(&g);
        let h = |google: Option<bool>, ms: u64| rstate::HealthState {
            google_ok: google,
            http_ms: vec![ms],
            ..Default::default()
        };
        let mut r = ResiRuntime::default();
        let now = OffsetDateTime::UNIX_EPOCH;
        r.health.insert(ids[0].to_string(), h(Some(true), 300));
        r.health.insert(ids[1].to_string(), h(Some(true), 100));
        r.health.insert(ids[2].to_string(), h(Some(false), 10));
        let ranked = rank_healthy(&g, &r, &ids, now);
        assert_eq!(
            ranked,
            vec![ids[1], ids[0], ids[2]],
            "Google 通排在 priority 之前，同 priority 再比延迟"
        );
        assert_eq!(
            pick_target(&g, &r, &ids, now),
            ranked.first().copied(),
            "pick_target 必须与 rank_healthy 的首位一致（同一套排序键）"
        );
        assert!(rank_healthy(&g, &r, &[], now).is_empty());
    }

    /// spec §5.6：relay 一重启，每槽的 selector 都回落到配置里的 `default`（本槽 IP）。
    /// **没被重放的**槽（这里 state 里没有槽位，无从重放）必须把运行时选择清零 —— 下一轮
    /// `drive_slots` 就会无条件重 PUT，而不是去赌 sing-box 的 `cache_file` 有没有把
    /// selector 的选择持久化下来。借用 / pin 中的槽会被重放回去，见下面的重放用例。
    #[tokio::test]
    async fn a_relay_restart_clears_every_slot_selection() {
        let d = tempfile::tempdir().unwrap();
        let (c, _h) = ctx(&d, &[10, 20]).await;
        rstate::update(&c.runtime, |r| {
            r.slots.insert(
                "1".into(),
                rstate::SlotRuntime {
                    current_upstream_id: Some(Uuid::from_u128(2)),
                    pinned_upstream_id: None,
                    back_rounds: 2,
                },
            );
        })
        .await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        // 先 subscribe 再 spawn（broadcast 丢弃「发送时还没有订阅者」的事件）
        let rx = c.bus.subscribe();
        let task = tokio::spawn(replay_loop(c.clone(), clash.clone(), rx));
        c.bus.send(Event::RelayRestarted);
        for _ in 0..50 {
            if rstate::read(&c.runtime).await.slots["1"]
                .current_upstream_id
                .is_none()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        task.abort();
        let r = rstate::read(&c.runtime).await;
        assert!(
            r.slots["1"].current_upstream_id.is_none(),
            "relay 重启后各槽的运行时选择必须清零"
        );
        assert_eq!(
            r.slots["1"].back_rounds, 2,
            "切回轮数不清：它记的是「本槽自己健康了几轮」，与 relay 重启无关"
        );
    }

    // ── relay 重启后的重放：退避重试 + 覆盖各槽 selector ─────────────────────────

    /// 带槽位的 ctx：`n` 条上游（priority 全 10），槽 i ↔ 上游 `i+1`（`resi-(i+1)`）
    async fn slot_ctx(d: &tempfile::TempDir, n: usize) -> DaemonCtx {
        let (c, _h) = ctx(d, &vec![10; n]).await;
        crate::modules::residential::slots::migrate_on_start(&c.store, &c.bus)
            .await
            .unwrap();
        c
    }

    fn slot_rt(
        current: Option<u128>,
        pinned: Option<u128>,
        back_rounds: u32,
    ) -> rstate::SlotRuntime {
        rstate::SlotRuntime {
            current_upstream_id: current.map(Uuid::from_u128),
            pinned_upstream_id: pinned.map(Uuid::from_u128),
            back_rounds,
        }
    }

    /// 2026-09-13 04:05:47 UTC bwg-rick 实录：每日黑名单批量重启 b-ui-relay，replay_loop
    /// 收到事件的那一刻 Clash API 还没起监听，重放一次失败就放弃了；而且只重放全局
    /// resi-pool，借用 / pin 中的槽 selector 被 relay 打回默认。现在：每轮先探 API 可用
    /// 再 select，连接类失败按退避重试；全局与各槽的选择都要恢复。
    #[tokio::test(start_paused = true)]
    async fn a_relay_restart_retries_until_the_clash_api_is_up_and_replays_every_selector() {
        let d = tempfile::tempdir().unwrap();
        let c = slot_ctx(&d, 3).await;
        rstate::update(&c.runtime, |r| {
            r.selected_upstream_id = Some(Uuid::from_u128(2));
            // 槽 0 用的就是本槽 IP（= selector 的 default），不必重放
            r.slots.insert("0".into(), slot_rt(Some(1), None, 0));
            // 槽 1 正借用 resi-3，本槽已恢复 1 轮
            r.slots.insert("1".into(), slot_rt(Some(3), None, 1));
            // 槽 2 被管理员 pin 到 resi-1
            r.slots.insert("2".into(), slot_rt(Some(1), Some(1), 0));
            rstate::push_alert(r, "机器上没有 journalctl，黑名单候选只能靠每日探针集");
            rstate::push_alert(
                r,
                format!(
                    "{REPLAY_FAIL_ALERT}（resi-pool → resi-2：Clash API 不可达），下一轮巡检兜底"
                ),
            );
        })
        .await;
        // relay 刚重启：前两次连 Clash API 都被拒
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        clash.with(|i| i.refuse = 2);

        let out = replay_after_restart(&c, clash.clone()).await;

        assert!(out.failed.is_empty(), "{out:?}");
        assert_eq!(
            clash.calls(),
            vec![
                "ready",
                "ready",
                "ready",
                "put:resi-pool:resi-2",
                "put:slot-1-pool:resi-3",
                "put:slot-2-pool:resi-1",
            ],
            "每轮先探 Clash API 可用再 select；探不通的那两轮一个 PUT 都不发"
        );
        assert_eq!(clash.peek(POOL).as_deref(), Some("resi-2"));
        assert_eq!(
            clash.peek("slot-1-pool").as_deref(),
            Some("resi-3"),
            "借用中的槽重放回借用目标"
        );
        assert_eq!(
            clash.peek("slot-2-pool").as_deref(),
            Some("resi-1"),
            "pin 住的槽重放回 pin"
        );
        assert_eq!(clash.peek("slot-0-pool"), None, "用本槽 IP 的槽不发 PUT");
        let r = rstate::read(&c.runtime).await;
        assert_eq!(r.slots["1"].current_upstream_id, Some(Uuid::from_u128(3)));
        assert_eq!(r.slots["1"].back_rounds, 1);
        assert_eq!(r.slots["2"].current_upstream_id, Some(Uuid::from_u128(1)));
        assert_eq!(
            r.slots["0"].current_upstream_id, None,
            "不重放的槽照旧清零 ⇒ 下一轮 drive_slots 无条件重 PUT 本槽"
        );
        assert_eq!(
            r.alerts,
            vec!["机器上没有 journalctl，黑名单候选只能靠每日探针集".to_string()],
            "全部成功 ⇒ 认领上一次重放失败留下的告警，别的告警不动"
        );
    }

    /// 槽 1 借用中遇上 relay 重启：selector 被重放回借用目标，而且切回防抖不被重启清掉 ——
    /// 以前 current 被清零，下一轮 drive_slots 会跳过「攒满 3 轮」直接切回本槽。
    /// 运行时还没选过全局出口：旧实现在这里直接 continue，一个槽都不重放。
    #[tokio::test]
    async fn a_borrowing_slot_is_replayed_to_its_borrow_target_and_keeps_its_debounce() {
        let d = tempfile::tempdir().unwrap();
        let c = slot_ctx(&d, 3).await;
        rstate::update(&c.runtime, |r| {
            r.slots.insert("1".into(), slot_rt(Some(3), None, 1));
        })
        .await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));

        let out = replay_after_restart(&c, clash.clone()).await;

        assert_eq!(
            out.replayed,
            vec![("slot-1-pool".to_string(), "resi-3".to_string())]
        );
        assert!(out.failed.is_empty(), "{out:?}");
        assert_eq!(clash.peek("slot-1-pool").as_deref(), Some("resi-3"));
        // 下一轮巡检：本槽已健康，但借用中要攒满 SLOT_BACK_ROUNDS 轮才切回
        let g = rstate::group_of(&*c.store.read().await);
        let all = ids_of(&g);
        let o = crate::modules::residential::slots::drive_slots(
            &c,
            clash.clone(),
            &g,
            &all,
            OffsetDateTime::UNIX_EPOCH,
        )
        .await;
        let s1 = o.iter().find(|o| o.index == 1).unwrap();
        assert!(s1.borrowed && !s1.switched, "{s1:?}");
        assert_eq!(clash.peek("slot-1-pool").as_deref(), Some("resi-3"));
        assert_eq!(rstate::read(&c.runtime).await.slots["1"].back_rounds, 2);
    }

    /// Clash API 一直起不来：退避总时长封顶 [`REPLAY_RETRY_BUDGET`]，之后写一条告警、
    /// 把重放失败的槽交还给下一轮 drive_slots（current 清零 ⇒ 无条件重 PUT），全局选择
    /// 由下一轮规则 6a 兜底；replay_loop 本身不 panic、继续收事件。
    #[tokio::test(start_paused = true)]
    async fn a_clash_api_that_never_comes_up_leaves_an_alert_and_hands_over_to_the_next_round() {
        let d = tempfile::tempdir().unwrap();
        let c = slot_ctx(&d, 3).await;
        rstate::update(&c.runtime, |r| {
            r.selected_upstream_id = Some(Uuid::from_u128(2));
            r.slots.insert("1".into(), slot_rt(Some(3), None, 1));
        })
        .await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        clash.with(|i| i.refuse = u32::MAX);

        let t0 = tokio::time::Instant::now();
        let out = replay_after_restart(&c, clash.clone()).await;
        let waited = t0.elapsed();

        assert!(
            waited <= REPLAY_RETRY_BUDGET,
            "退避总时长超预算：{waited:?}"
        );
        assert!(
            waited > REPLAY_RETRY_BUDGET / 2,
            "预算没用满就放弃了：{waited:?}"
        );
        assert!(out.replayed.is_empty(), "{out:?}");
        assert_eq!(
            out.failed
                .iter()
                .map(|(s, t, _)| (s.as_str(), t.as_str()))
                .collect::<Vec<_>>(),
            vec![("resi-pool", "resi-2"), ("slot-1-pool", "resi-3")]
        );
        let calls = clash.calls();
        assert!(
            calls.iter().filter(|c| *c == "ready").count() >= 5,
            "{calls:?}"
        );
        assert!(
            calls.iter().all(|c| !c.starts_with("put:")),
            "探不通就不发 PUT：{calls:?}"
        );
        let r = rstate::read(&c.runtime).await;
        assert_eq!(r.alerts.len(), 1, "{:?}", r.alerts);
        assert!(r.alerts[0].starts_with(REPLAY_FAIL_ALERT), "{:?}", r.alerts);
        assert!(
            r.alerts[0].contains("resi-pool") && r.alerts[0].contains("slot-1-pool"),
            "{:?}",
            r.alerts
        );
        assert_eq!(
            r.slots["1"].current_upstream_id, None,
            "重放失败的槽交还给下一轮 drive_slots 无条件重 PUT"
        );
        assert_eq!(
            r.selected_upstream_id,
            Some(Uuid::from_u128(2)),
            "运行时选择是真源，不因重放失败改动（规则 6a 下一轮会把它重放回去）"
        );

        // 同样的失败发生在 replay_loop 里：只写告警，循环还活着
        let rx = c.bus.subscribe();
        let task = tokio::spawn(replay_loop(c.clone(), clash.clone(), rx));
        c.bus.send(Event::RelayRestarted);
        tokio::time::sleep(REPLAY_RETRY_BUDGET * 2).await;
        assert!(!task.is_finished(), "重放失败不许把 replay_loop 带崩");
        task.abort();
    }

    /// Clash 明确拒绝（selector / 成员不存在，HTTP 4xx）不是「还没起来」，重试也没用：
    /// 当轮就记失败，不退避。
    #[tokio::test(start_paused = true)]
    async fn a_rejected_replay_is_not_retried() {
        let d = tempfile::tempdir().unwrap();
        let c = slot_ctx(&d, 2).await;
        rstate::update(&c.runtime, |r| {
            r.selected_upstream_id = Some(Uuid::from_u128(2))
        })
        .await;
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        clash.with(|i| i.reject = true);

        let t0 = tokio::time::Instant::now();
        let out = replay_after_restart(&c, clash.clone()).await;

        assert!(t0.elapsed() < REPLAY_RETRY_FIRST, "{:?}", t0.elapsed());
        assert_eq!(clash.calls(), vec!["ready", "put:resi-pool:resi-2"]);
        assert_eq!(out.failed.len(), 1, "{out:?}");
        assert!(rstate::read(&c.runtime)
            .await
            .alerts
            .iter()
            .any(|a| a.starts_with(REPLAY_FAIL_ALERT)));
    }

    /// 池未启用 ⇒ relay 里一个 selector 都没有（fail-open 直连），没什么可重放，
    /// 更不能退避 15 秒后写一条假告警。
    #[tokio::test]
    async fn a_disabled_pool_has_nothing_to_replay() {
        let d = tempfile::tempdir().unwrap();
        let c = slot_ctx(&d, 2).await;
        rstate::update(&c.runtime, |r| {
            r.selected_upstream_id = Some(Uuid::from_u128(2))
        })
        .await;
        rstate::update_group(&c.store, &c.bus, |g| g.enabled = false)
            .await
            .unwrap();
        let clash = Arc::new(FakeClash::new(Some("resi-1")));
        clash.with(|i| i.refuse = u32::MAX);

        let out = replay_after_restart(&c, clash.clone()).await;

        assert_eq!(out, ReplayOutcome::default());
        assert!(clash.calls().is_empty(), "{:?}", clash.calls());
        assert!(rstate::read(&c.runtime).await.alerts.is_empty());
    }

    // ── 哨兵的带外快探（spec §5.7，设计裁决 D5）──

    #[test]
    fn the_quick_probe_gives_up_at_once_when_the_gateway_is_unreachable() {
        let p = crate::modules::residential::proxy::FakeProber::new(); // tcp_ms 缺省 None = 连不上
        let seen = Arc::new(std::sync::Mutex::new(None));
        let s2 = seen.clone();
        p.with(|i| i.on_tcp_fail = Some(Box::new(move |d| *s2.lock().unwrap() = Some(d))));
        let r = probe_quick(&p, &upstream(2, 10), Duration::from_secs(3));
        assert!(!r.ok && !r.auth_failed);
        assert_eq!(
            p.calls(),
            vec!["tcp"],
            "网关都连不上就不再发 HTTP（丢包时每次都要等满超时）"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            Some(Duration::from_secs(3)),
            "TCP 的总时限原样交给 prober"
        );
    }

    #[test]
    fn the_quick_probe_tunnels_once_when_the_gateway_answers() {
        let p = crate::modules::residential::proxy::FakeProber::new();
        p.with(|i| {
            i.tcp_ms = Some(30);
            i.gets.insert(
                crate::modules::residential::LATENCY_PROBE_URL.into(),
                Ok(HttpProbe {
                    status: 204,
                    body: String::new(),
                }),
            );
        });
        let r = probe_quick(&p, &upstream(2, 10), Duration::from_secs(3));
        assert!(r.ok);
        assert_eq!(r.tcp_ms, Some(30));
        assert_eq!(
            p.calls(),
            vec![
                "tcp".to_string(),
                format!("timed:{}", crate::modules::residential::LATENCY_PROBE_URL)
            ],
            "快探不测 Google / UDP / 测速"
        );
    }

    #[test]
    fn the_quick_probe_still_tells_a_credential_failure_apart() {
        let p = crate::modules::residential::proxy::FakeProber::new();
        p.with(|i| {
            i.tcp_ms = Some(30);
            i.gets.insert(
                crate::modules::residential::LATENCY_PROBE_URL.into(),
                Err("__auth_failed__".into()),
            );
        });
        let r = probe_quick(&p, &upstream(2, 10), Duration::from_secs(3));
        assert!(!r.ok && r.auth_failed);
    }

    /// 抓 `tracing` 输出的极小写入器：[`probe_quick_within`] 对 `JoinError` 只落日志
    /// （结论、级别都不动），所以「原因有没有被丢掉」只能从日志里断言
    #[derive(Clone, Default)]
    struct LogCapture(Arc<std::sync::Mutex<Vec<u8>>>);

    impl LogCapture {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().expect("日志缓冲锁")).into_owned()
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

    /// 探测任务本身异常（`spawn_blocking` 的 `JoinError`：探测器 panic / runtime 正在关）：
    /// 结论只能是 [`Verdict::Unconfirmed`]（确实不知道），但**原因必须落日志**——两侧调用方的
    /// 事件文案都只说「网关通但隧道无响应，或探测任务异常」，而 panic 会每
    /// `sentinel::DEBOUNCE_SECS` 秒复发一次，日志里不留原因就无从下手（2026-09-14 审查第 4 条）。
    /// 日志里指称上游只许用 `host:port`，不许带凭据。
    #[tokio::test]
    async fn a_panicking_probe_task_is_unconfirmed_and_logs_the_reason() {
        let p = crate::modules::residential::proxy::FakeProber::new();
        // 网关连不上那一步会调 on_tcp_fail：借它让阻塞任务炸在里面
        p.with(|i| i.on_tcp_fail = Some(Box::new(|_| panic!("探测器炸了"))));
        let prober: Arc<dyn Prober> = Arc::new(p);
        let log = LogCapture::default();
        let verdict = {
            let _g = tracing::subscriber::set_default(
                tracing_subscriber::fmt()
                    .with_writer(log.clone())
                    .with_ansi(false)
                    .finish(),
            );
            probe_quick_within(&prober, upstream(2, 10), Duration::from_secs(1)).await
        };
        assert_eq!(verdict, Verdict::Unconfirmed, "不知道就是不知道");
        let text = log.text();
        assert!(text.contains("带外快探的探测任务异常"), "{text}");
        assert!(text.contains("探测器炸了"), "原因被丢掉了：{text}");
        assert!(
            text.contains("isp2.example.net:10007"),
            "要点名上游：{text}"
        );
        assert!(!text.contains("pw1"), "日志不许带凭据：{text}");
    }
}
