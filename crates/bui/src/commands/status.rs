//! `bui status`：经 unix socket 读 `/api/health` 并打印人读文本（`--json` 直出 JSON）。
//!
//! 守护进程没跑时（spec §2.4 的白名单里 `status` 是放行项）退化为本地拼一个
//! [`HealthResponse`]：单元状态现场问 systemd，对账 / 漂移 / watchdog / 可升级版本读
//! `runtime.json`，节点名读 `state.json`。

use crate::api::health::{HealthResponse, ServiceStatus};
use crate::state::runtime::Runtime;
use crate::state::store::Store;
use crate::sys::Host;
use anyhow::Result;
use bui_schema::model::Hy2Auth;
use bui_schema::paths::Paths;
use std::path::PathBuf;
use std::sync::Arc;
use time::OffsetDateTime;

/// `bui status` 那几行 `/api/health` 里没有的东西（P2 的 `HealthResponse` 是回归锁死的
/// 形状，不许往里加字段），全部从期望态 + `runtime.json` 读出来。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatusExtra<'a> {
    /// 住宅 HY2 凭据池（spec §3.1）：`(已用, 容量, 第几代)`
    pub pool: (usize, usize, u64),
    /// nft 端口跳跃表期望的规则条数（`render::nft::rule_count`）
    pub nft_rules: usize,
    /// 兼容段的闭区间；`None` = `bui set hy2-resi-compat off` 之后已收口
    pub compat_range: Option<(u16, u16)>,
    /// 兼容段的**累计**命中（T12 落在 `runtime.json` 的那份，**绝不是活 counter**）；
    /// `None` = 守护进程还没采过一轮
    pub compat_hits: Option<&'a crate::modules::watchdog::CompatHits>,
}

/// 把 `/api/health` 渲染成人读文本。
///
/// `hy2_auth` / `legacy_sub_until` / `extra` 单独传：`/api/health` 里没有它们（P2 的
/// `HealthResponse` 是回归锁死的形状），而运维在排「谁都登不上」时第一件要看的就是当前
/// 走的是 http 还是 command，排「订阅取不到」时要看旧用户名链接还认不认，排「住宅 HY2
/// 全员被拒」时要看凭据池、nft 表与门位重放这三行（4.1，spec §2.4 / §3.1 / §3.4）。
pub fn format_status(
    h: &HealthResponse,
    hy2_auth: Hy2Auth,
    legacy_sub_until: Option<&str>,
    extra: &StatusExtra<'_>,
    now: OffsetDateTime,
) -> String {
    let mut out = vec![format!(
        "b-ui v{} @ {}     状态: {}     运行: {}",
        h.version,
        h.node,
        h.status,
        crate::util::human_duration(h.uptime_secs)
    )];
    // 单元名按最长的对齐（按字符数），第一行带「单元」标签，其余行缩进对齐
    let width = h
        .services
        .iter()
        .map(|s| s.unit.chars().count())
        .max()
        .unwrap_or(0);
    for (n, s) in h.services.iter().enumerate() {
        let label = if n == 0 {
            "单元        "
        } else {
            "            "
        };
        out.push(format!(
            "{label}{:<width$} {}({}, 重启 {} 次)",
            s.unit,
            if s.active { "running" } else { "stopped" },
            if s.enabled { "enabled" } else { "disabled" },
            s.n_restarts,
            width = width
        ));
    }
    // 「仅直连」不是修饰语而是作用域（spec §6、C5）：4.1 的住宅 HY2 是 sing-box 的静态
    // 凭据池 + 门，配置里没有 `auth` 段 —— 拿这一行去判断住宅侧鉴权会判错。
    out.push(format!(
        "鉴权模式    Hysteria2 {}（仅直连；住宅 HY2 走凭据池 + 门）",
        match hy2_auth {
            Hy2Auth::Http => "auth.type=http（守护进程进程内应答）",
            Hy2Auth::Command => "auth.type=command（钩子 bin/bui-auth-hook，退路）",
        }
    ));
    out.push(format_legacy_sub(legacy_sub_until, now));
    out.push(format_hy2_pool(extra.pool));
    out.push(format_nft_table(
        extra.nft_rules,
        extra.compat_range,
        extra.compat_hits,
    ));
    out.push(format_gate_replay(h.residential.as_ref()));
    if let Some(r) = &h.reconcile {
        out.push(format!(
            "上次对账    {}  变更 {} 项{}",
            r.at,
            r.changed.len(),
            if r.dry_run { "（dry-run）" } else { "" }
        ));
        for e in r.errors.iter().chain(r.verify_failures.iter()) {
            out.push(format!("错误        {e}"));
        }
        for n in &r.notes {
            out.push(format!("提示        {n}"));
        }
    }
    if h.drift.is_empty() {
        out.push("漂移        无漂移".to_string());
    } else {
        for d in &h.drift {
            out.push(format!("漂移        {} {} —— {}", d.kind, d.path, d.detail));
        }
    }
    if let Some(v) = &h.upgrade_available {
        // 版本号与正在跑的一样 ⇒ rc 通道的同版本重建（rc1 / rc2 / 正式版共用一个版本号），
        // 报「已是最新」会让人白等（真机 bwg-rick 就这么卡住过）
        if *v == h.version {
            out.push("新构建      有同版本的新构建（rc 通道），可执行 bui upgrade".to_string());
        } else {
            out.push(format!("新版本      {v} 可用（运行 b-ui upgrade）"));
        }
    }
    out.join("\n")
}

/// 旧「用户名链接」宽限期（`system.legacy_sub_until`）的三种态（2026-09-14 裁决）。
///
/// `bui status` 的那一行与装机摘要（[`crate::commands::install::summary`]）都按这一处判定：
/// 两边各写一遍「过没过期」，迟早会一处说「还认」另一处说「已过期」。
///
/// `None`、以及解析不出来的时刻都是 [`LegacySub::Off`] —— 端点侧要的是「当前时间早于它」，
/// 判不出来就一律不认（fail-closed），所以这两种情况的实际效果就是停用。
pub enum LegacySub<'a> {
    /// 没有宽限期（全新装机、或运维 `bui set legacy-sub off`）⇒ 只认随机 token 链接
    Off,
    /// 宽限期内：`raw` 是期望态里那个时刻的原文，`secs_left` 是到它还剩多少秒
    Active { raw: &'a str, secs_left: u64 },
    /// 宽限期已过：用户名链接已经不通了，`raw` 同上
    Expired { raw: &'a str },
}

/// 判定 [`LegacySub`]（口径见它的文档）。
pub fn legacy_sub(until: Option<&str>, now: OffsetDateTime) -> LegacySub<'_> {
    match until.and_then(|t| crate::util::parse_rfc3339(t).map(|d| (t, d))) {
        None => LegacySub::Off,
        Some((raw, deadline)) if deadline > now => LegacySub::Active {
            raw,
            secs_left: (deadline - now).whole_seconds().max(0) as u64,
        },
        Some((raw, _)) => LegacySub::Expired { raw },
    }
}

/// `bui status` 的「旧订阅链接」一行（2026-09-14 裁决）：四个免鉴权订阅端点按每用户随机
/// token 取，旧的「用户名链接」只在全局宽限期（`system.legacy_sub_until`）内还认。
pub fn format_legacy_sub(until: Option<&str>, now: OffsetDateTime) -> String {
    match legacy_sub(until, now) {
        LegacySub::Off => "旧订阅链接  已停用（只认随机 token 链接）".to_string(),
        LegacySub::Active { raw, secs_left } => format!(
            "旧订阅链接  用户名链接还剩 {} 到期（{raw}）",
            crate::util::human_duration(secs_left)
        ),
        LegacySub::Expired { raw } => {
            format!("旧订阅链接  已过期（{raw}），只认随机 token 链接")
        }
    }
}

/// `bui status` 的「住宅 HY2 凭据池」一行（4.1，spec §3.1）。
///
/// 空闲耗尽会把建用户 / 轮换推到「当场扩容 ⇒ 重写 `hy2-residential.json` ⇒ 重启
/// `hysteria-residential`」那条路上（全体住宅 HY2 会话重连一次），所以已用 / 容量是运维
/// 要盯的第一个数。「第 N 代」= 池被重写过几次，日志与事件按它指称。
///
/// `used` / `size` **只能来自 [`hy2pool::usage`](bui_schema::hy2pool::usage)**
/// （`GET /api/residential/pool` 同源）：自己另算一套会把悬空指针也计进已用，于是这一行的
/// 「空闲不足 20%」比 `bui residential pool status` 更早喊，同一台机器上两处互相打脸。
pub fn format_hy2_pool((used, size, generation): (usize, usize, u64)) -> String {
    let low =
        size > 0 && (used as f64) > (1.0 - bui_schema::hy2pool::LOW_FREE_RATIO) * (size as f64);
    format!(
        "住宅 HY2 凭据池：已用 {used} / {size}，第 {generation} 代{}",
        if low {
            format!(
                "（空闲不足 {}%，下次建用户会触发扩容 + 住宅内核重启）",
                (bui_schema::hy2pool::LOW_FREE_RATIO * 100.0) as u32
            )
        } else {
            String::new()
        }
    )
}

/// `bui status` 的「nft 表」一行（4.1，spec §2.4）。
///
/// 这张 `inet bui` 表就是住宅 HY2 的端口跳跃：整段 `41000-50000` REDIRECT 到单一监听
/// 端口。表没了 ⇒ 带 `mport` 的现役订阅全部连不上，所以规则条数要能一眼对上
/// [`rule_count`](bui_schema::render::nft::rule_count)。
///
/// **兼容段那一句读的是持久化的累计命中，绝不是活 counter**（2026-09-16 裁决）：
/// `ruleset` 每次重放都先 `flush table`，活计数被清回 0；拿它当下线判据会把仍在用的
/// 兼容段判成闲置并关掉，全部还没刷订阅的 4.0 住宅用户当场断联。累计值由守护进程在每次
/// 重放**之前**采样累加（`modules::watchdog`，T12），`None` = 还没采过一轮。
pub fn format_nft_table(
    rules: usize,
    compat: Option<(u16, u16)>,
    hits: Option<&crate::modules::watchdog::CompatHits>,
) -> String {
    let table = bui_schema::render::nft::TABLE;
    let Some((a, b)) = compat else {
        return format!("nft 表 {table}：{rules} 条规则；兼容段已下线");
    };
    let tail = match hits {
        Some(hits) => format!("最近命中 {} 次（自 {}）", hits.total, hits.quiet_since()),
        // T12 的采样还没落地（守护进程刚起、或那段代码还没合进来）：**明说不知道**，
        // 别打一个 0 出来让人当成「30 天没人用了，可以关」
        None => "命中统计未就绪（守护进程还没采过一轮）".to_string(),
    };
    format!("nft 表 {table}：{rules} 条规则；兼容段 {a}-{b} {tail}")
}

/// `bui status` 的「门位重放」一行（4.1，spec §3.4）。
///
/// 不开 `cache_file`（spec §14 裁决 1）⇒ 住宅 sing-box 每次重启都把每个 `gate-<id>`
/// selector 打回 `default = deny`、住宅 HY2 全员 fail-closed，由 b-ui 按退避重放真实门位。
/// 重放最终失败时那条告警留在住宅告警表里（`gates::GATE_REPLAY_FAIL_ALERT` 前缀），
/// 下一次全部成功时被认领清掉 —— 所以「有没有那条告警」就是这一行的判据。
///
/// 读的是 `/api/health` 里的住宅摘要（`residential.alerts`）：守护进程没跑时本地探测
/// 那份摘要是 `None`，此时如实说「未知」而不是「正常」。
pub fn format_gate_replay(residential: Option<&serde_json::Value>) -> String {
    let Some(v) = residential else {
        return "门位重放：未知（守护进程未运行）".to_string();
    };
    let fail = v
        .get("alerts")
        .and_then(|a| a.as_array())
        .into_iter()
        .flatten()
        .filter_map(|x| x.as_str())
        // **剥掉告警自己的前缀**：告警原文是「住宅 HY2 门位重放失败（<原因>），门留在
        // deny，60 秒安全网继续收敛」，整条塞进括号会套两层前缀（「门位重放：失败（住宅
        // HY2 门位重放失败（…）…）」）。
        .find_map(|s| s.strip_prefix(crate::modules::panel::gates::GATE_REPLAY_FAIL_ALERT));
    match fail {
        None => "门位重放：正常".to_string(),
        // 剩下的部分正好接在「失败」后面 ⇒「失败（<原因>），门留在 deny，…」，
        // 与计划 / CHANGELOG 的写法一致，那句下一步也不丢。
        Some(rest) => format!("门位重放：失败{rest}"),
    }
}

/// `bui status` 末尾显示几条事件（spec §5.7；全量看 `bui incidents`）
pub const STATUS_INCIDENTS: usize = 5;

/// `bui status` 的「最近事件」段；`--json` 不带它（`/api/health` 的形状是 P2 锁死的回归面）
pub fn format_recent_incidents(v: &[crate::modules::sentinel::incidents::Incident]) -> String {
    if v.is_empty() {
        return "最近事件    无（`bui incidents` 查看全量）".into();
    }
    let mut out = vec!["最近事件    （`bui incidents` 查看全量）".to_string()];
    out.extend(v.iter().map(|i| {
        format!(
            "            {}",
            crate::modules::sentinel::incidents::format_line(i)
        )
    }));
    out.join("\n")
}

pub async fn run(json: bool, paths: Paths, host: Arc<dyn Host>) -> Result<()> {
    run_with(json, paths, host, PathBuf::from(crate::paths::SOCKET_PATH)).await
}

/// socket 路径由调用方传入（与 install / reconcile 同一口径：单元测试不碰真实 socket）。
pub async fn run_with(
    json: bool,
    paths: Paths,
    host: Arc<dyn Host>,
    socket: PathBuf,
) -> Result<()> {
    let now = host.now();
    let health = match fetch_over_socket(&socket).await {
        Some(h) => h,
        None => {
            eprintln!("守护进程未运行，以下为本地探测结果");
            local_health(&paths, host).await?
        }
    };
    // 鉴权模式、旧链接宽限期、凭据池与 nft 表都读期望态：守护进程在不在跑都读得到，
    // `--json` 那一支不动（`HealthResponse` 的形状是 P2 锁死的回归面）。
    let (hy2_auth, legacy_sub_until, pool, nft_rules, compat_range) =
        match Store::open(crate::paths::state_file(&paths)).await {
            Ok(store) => {
                let s = store.read().await;
                // 已用 / 容量走 `hy2pool::usage` 这**一处**口径（`GET /api/residential/pool`
                // 同源）：按「有指针的用户数」另算一套会把悬空指针也计进已用，于是这一行的
                // 「空闲不足 20%」比 `bui residential pool status` 更早喊，两处互相打脸。
                let u = bui_schema::hy2pool::usage(s.as_ref());
                let compat = s.system.hy2_resi_compat_ports;
                (
                    s.system.hy2_auth,
                    s.system.legacy_sub_until.clone(),
                    (u.used, u.size, s.residential.hy2_pool.generation),
                    bui_schema::render::nft::rule_count(compat),
                    compat.then(|| bui_schema::render::nft::compat_range(&s.node.ports)),
                )
            }
            Err(_) => (Hy2Auth::default(), None, (0, 0, 0), 0, None),
        };
    // **兼容段的累计命中读 `runtime.json` 的持久值，绝不读活 counter**（2026-09-16 裁决）。
    let hits = crate::modules::watchdog::compat_hits(
        &crate::state::runtime::Runtime::load(crate::paths::runtime_file(&paths))
            .read()
            .await,
    );
    let extra = StatusExtra {
        pool,
        nft_rules,
        compat_range,
        compat_hits: hits.as_ref(),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&health)?);
    } else {
        println!(
            "{}",
            format_status(&health, hy2_auth, legacy_sub_until.as_deref(), &extra, now)
        );
        let (recent, _) =
            crate::modules::sentinel::incidents::load_recent(&socket, &paths, STATUS_INCIDENTS)
                .await;
        println!("{}", format_recent_incidents(&recent));
    }
    Ok(())
}

/// socket 可用且 `/api/health` 解析成功才返回 `Some`，否则退化到本地探测。
async fn fetch_over_socket(socket: &std::path::Path) -> Option<HealthResponse> {
    let client = crate::ipc::Client::new(socket);
    if !client.available().await {
        return None;
    }
    let (status, body) = client.request("GET", "/api/health", None).await.ok()?;
    if status != 200 {
        return None;
    }
    serde_json::from_value(body).ok()
}

/// 守护进程没跑时的本地版本：单元状态现场问 systemd，其余读 `runtime.json` / `state.json`。
async fn local_health(paths: &Paths, host: Arc<dyn Host>) -> Result<HealthResponse> {
    // 读不到期望态就只报那六个固定单元（槽 1.. 的住宅实例无从得知）
    let (node, units) = match Store::open(crate::paths::state_file(paths)).await {
        Ok(store) => {
            let s = store.read().await;
            (s.node.name.clone(), crate::reconcile::managed_units(&s))
        }
        Err(_) => (
            host.hostname().unwrap_or_default(),
            crate::reconcile::MANAGED_UNITS
                .iter()
                .map(|u| u.to_string())
                .collect(),
        ),
    };
    let rt = Runtime::load(crate::paths::runtime_file(paths))
        .read()
        .await;
    let h = host.clone();
    let services = tokio::task::spawn_blocking(move || {
        units
            .iter()
            .map(|u| ServiceStatus {
                unit: u.to_string(),
                active: h.unit_is_active(u).unwrap_or(false),
                enabled: h.unit_is_enabled(u).unwrap_or(false),
                n_restarts: h
                    .unit_property(u, "NRestarts")
                    .ok()
                    .flatten()
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(0),
            })
            .collect::<Vec<_>>()
    })
    .await?;
    // 守护进程没跑 ⇒ 至少 `b-ui` 不 active，状态必然 degraded；判据与 `/api/health` 同一套
    let degraded = services.iter().any(|s| !s.active)
        || !rt.drift.is_empty()
        || rt
            .last_reconcile
            .as_ref()
            .is_some_and(|r| !r.errors.is_empty() || !r.verify_failures.is_empty());
    // 守护进程没跑，「运行时长」无从得知，按 0 报
    let uptime_secs = rt
        .started_at
        .as_deref()
        .and_then(crate::util::parse_rfc3339)
        .map(|t| (host.now() - t).whole_seconds().max(0) as u64)
        .unwrap_or(0);
    Ok(HealthResponse {
        status: if degraded {
            "degraded".into()
        } else {
            "ok".into()
        },
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_secs,
        node,
        services,
        reconcile: rt.last_reconcile.clone(),
        drift: rt.drift.clone(),
        watchdog: rt.watchdog.clone(),
        upgrade_available: rt.upgrade_available.clone(),
        residential: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::health::{HealthResponse, ServiceStatus};
    use crate::state::runtime::{DriftItem, ReconcileReport};
    use time::macros::datetime;

    fn t0() -> OffsetDateTime {
        datetime!(2026-09-14 00:00:00 UTC)
    }

    fn sample() -> HealthResponse {
        HealthResponse {
            status: "degraded".into(),
            version: "4.0.0".into(),
            uptime_secs: 3725,
            node: "node-a".into(),
            services: vec![
                ServiceStatus {
                    unit: "hysteria-server".into(),
                    active: true,
                    enabled: true,
                    n_restarts: 2,
                },
                ServiceStatus {
                    unit: "xray".into(),
                    active: false,
                    enabled: true,
                    n_restarts: 7,
                },
            ],
            reconcile: Some(ReconcileReport {
                at: "2026-09-11T00:00:00Z".into(),
                changed: vec!["/opt/b-ui/config.yaml".into()],
                errors: vec!["xray 重启失败".into()],
                ..Default::default()
            }),
            drift: vec![DriftItem {
                kind: "cron".into(),
                path: "crontab".into(),
                detail: "0 */6 * * * /opt/b-ui/update.sh".into(),
            }],
            watchdog: Default::default(),
            upgrade_available: Some("4.0.1".into()),
            // 住宅摘要（`modules::residential::state::health_summary` 的形状）：
            // 告警表为空 ⇒ 门位重放这一行报「正常」
            residential: Some(serde_json::json!({ "alerts": [] })),
        }
    }

    /// 「住宅 HY2 凭据池 / nft 表 / 门位重放」这三行的输入（4.1）
    fn extra_with_pool() -> StatusExtra<'static> {
        StatusExtra {
            pool: (1, 32, 3),
            nft_rules: 4,
            compat_range: Some((40001, 40007)),
            compat_hits: None,
        }
    }

    fn hits(total: u64, last: Option<&str>, since: &str) -> crate::modules::watchdog::CompatHits {
        crate::modules::watchdog::CompatHits {
            total,
            last_hit_at: last.map(str::to_string),
            since: since.to_string(),
            ..Default::default()
        }
    }

    /// 已用 / 容量的口径只有 [`bui_schema::hy2pool::usage`] **一处**，`run_with` 这一侧
    /// 也得锁住（第九波复核点名：端点那一侧有逐值相等的断言，这一侧没有 —— 把 `run_with`
    /// 里那句 `hy2pool::usage` 换回 T14 之前的「去重后的非空 `hy2_resi_cred` 个数」全绿）。
    ///
    /// 为什么值得一条源级契约：`run_with` 是 I/O 路径，没有单元测试能直接钉它，而两套算法
    /// 的差别只在**悬空指针**（用户指向上一代池里已不存在的 id）—— 按用户数算会让
    /// `used + free > size`，于是 `bui status` 判「`used > 80%·size`」与
    /// `bui residential pool status` 判「`free < 20%·size`」在同一台机器上一处喊空闲不足、
    /// 另一处说没事，正是这次要修的那个症状。
    #[test]
    fn the_status_side_takes_used_and_size_from_the_one_pool_usage_criterion() {
        // 只看出厂代码那一半：本用例自己也在 `include_str!` 进来的那份源里，
        // 连测试一起 grep 就会匹配到自己、无论 `run_with` 干净与否都红。
        let src = include_str!("status.rs");
        let prod = src
            .split_once("\n#[cfg(test)]\n")
            .expect("status.rs 的测试模块标记变了")
            .0;
        assert_eq!(
            prod.matches(&format!("hy2pool::{}(", "usage")).count(),
            1,
            "`run_with` 必须有且只有一处调 hy2pool::usage"
        );
        // 出厂代码没有任何理由自己去摸这个字段：摸它就是在重算一套已用数
        let field = format!("hy2_resi{}", "_cred");
        assert!(
            !prod.contains(&field),
            "status.rs 又自己按 `{field}` 算了一遍已用数：口径只许有 hy2pool::usage 一处"
        );
        // 顺带钉住两处门槛同源：这一行的「空闲不足」与端点判的 free 比例是同一个常量
        assert_eq!(bui_schema::hy2pool::LOW_FREE_RATIO, 0.2);
        let u = bui_schema::hy2pool::usage(&crate::testutil::sample_state());
        assert_eq!(u.used + u.free, u.size, "usage 的三个数必须自洽");
    }

    /// T14 的三行新输出（纯函数，不碰机器）。
    #[test]
    fn status_reports_the_pool_the_nft_table_and_the_replay() {
        let h = hits(0, None, "2026-09-15T00:00:00Z");
        let extra = StatusExtra {
            compat_hits: Some(&h),
            ..extra_with_pool()
        };
        let text = format_status(&sample(), Hy2Auth::Http, None, &extra, t0());
        assert!(
            text.contains("住宅 HY2 凭据池：已用 1 / 32，第 3 代"),
            "{text}"
        );
        assert!(text.contains("nft 表 inet bui：4 条规则"), "{text}");
        assert!(
            text.contains("兼容段 40001-40007 最近命中 0 次（自 2026-09-15T00:00:00Z）"),
            "{text}"
        );
        assert!(text.contains("门位重放：正常"), "{text}");
    }

    /// 兼容段的「最近命中」**读持久值，绝不读活 counter**（2026-09-16 裁决）：
    /// 命中过 ⇒ 起算时刻是最近那一次；还没采过一轮 ⇒ 明说「未就绪」而不是打个 0
    /// （打 0 会被当成「30 天没人用了，可以关」，关掉就是未刷订阅的 4.0 用户当场断联）。
    #[test]
    fn the_compat_line_never_pretends_an_unsampled_counter_is_zero() {
        let none = format_nft_table(4, Some((40001, 40007)), None);
        assert_eq!(
            none,
            "nft 表 inet bui：4 条规则；兼容段 40001-40007 命中统计未就绪（守护进程还没采过一轮）"
        );
        assert!(!none.contains("0 次"), "不许打成 0 次：{none}");
        // 命中过：起算时刻用 last_hit_at，不是 since
        let h = hits(7, Some("2026-09-16T12:00:00Z"), "2026-09-01T00:00:00Z");
        assert_eq!(
            format_nft_table(4, Some((40001, 40007)), Some(&h)),
            "nft 表 inet bui：4 条规则；兼容段 40001-40007 最近命中 7 次（自 2026-09-16T12:00:00Z）"
        );
        // 关掉之后只剩两条规则，兼容段那一句换成「已下线」
        assert_eq!(
            format_nft_table(2, None, None),
            "nft 表 inet bui：2 条规则；兼容段已下线"
        );
    }

    /// 门位重放失败要在 `bui status` 里说出原因（spec §3.4）：没重放到的门**留在 deny**，
    /// 那批住宅 HY2 用户握手成功但每个请求被拒，运维只有这一行能看出来为什么。
    #[test]
    fn a_failed_gate_replay_shows_up_with_its_reason() {
        let why = format!(
            "{}（Clash API 未就绪），门留在 deny，60 秒安全网继续收敛",
            crate::modules::panel::gates::GATE_REPLAY_FAIL_ALERT
        );
        let mut h = sample();
        h.residential = Some(serde_json::json!({ "alerts": [why.clone()] }));
        let t = format_status(&h, Hy2Auth::Http, None, &extra_with_pool(), t0());
        // 告警自己的前缀要被剥掉：整条塞进括号会变成「失败（住宅 HY2 门位重放失败（…）…）」
        assert!(
            t.contains("门位重放：失败（Clash API 未就绪），门留在 deny，60 秒安全网继续收敛"),
            "{t}"
        );
        assert_eq!(
            t.matches(crate::modules::panel::gates::GATE_REPLAY_FAIL_ALERT)
                .count(),
            0,
            "「住宅 HY2 门位重放失败」这个前缀不许再出现一次（套两层）：{t}"
        );
        assert!(!t.contains("门位重放：正常"), "{t}");
        // 别的住宅告警不算门位重放失败
        h.residential = Some(serde_json::json!({ "alerts": ["上游 407 凭据失效"] }));
        assert!(
            format_status(&h, Hy2Auth::Http, None, &extra_with_pool(), t0())
                .contains("门位重放：正常")
        );
        // 守护进程没跑（本地探测那条路）⇒ 如实说未知，不许报「正常」
        h.residential = None;
        let t = format_status(&h, Hy2Auth::Http, None, &extra_with_pool(), t0());
        assert!(t.contains("门位重放：未知（守护进程未运行）"), "{t}");
    }

    /// 空闲率跌破 20%（`hy2pool::LOW_FREE_RATIO`）要在这一行点名：下一次建用户 / 轮换
    /// 会触发当场扩容 ⇒ 重写 `hy2-residential.json` ⇒ 重启住宅内核 ⇒ 全体会话重连一次。
    #[test]
    fn a_nearly_full_pool_says_the_next_new_user_restarts_the_kernel() {
        assert!(format_hy2_pool((26, 32, 1)).contains("空闲不足 20%"));
        assert!(
            !format_hy2_pool((25, 32, 1)).contains("空闲不足"),
            "20% 空闲不算低"
        );
        assert_eq!(
            format_hy2_pool((3, 32, 3)),
            "住宅 HY2 凭据池：已用 3 / 32，第 3 代"
        );
        // 池还没建（旧 state 首次启动）不许除零
        assert_eq!(
            format_hy2_pool((0, 0, 0)),
            "住宅 HY2 凭据池：已用 0 / 0，第 0 代"
        );
    }

    #[test]
    fn status_text_shows_units_uptime_errors_and_drift() {
        let t = format_status(
            &sample(),
            Hy2Auth::Http,
            None,
            &StatusExtra::default(),
            t0(),
        );
        assert!(t.contains("node-a"));
        assert!(t.contains("4.0.0"));
        assert!(t.contains("1h 2m"), "uptime 要人读得懂：{t}");
        assert!(t.contains("hysteria-server") && t.contains("running"));
        assert!(t.contains("xray") && t.contains("stopped"));
        assert!(t.contains("xray 重启失败"));
        assert!(t.contains("漂移") && t.contains("crontab"));
        assert!(
            t.contains("4.0.1"),
            "有新版本要提示（spec §7 的每日自检结果）：{t}"
        );
    }

    /// rc 通道（2026-09-12 bwg-rick）：rc1 / rc2 / 正式版的版本号都是同一个，所以每日自检报上来
    /// 的「可升级版本」== 正在跑的版本时，说的是**同版本的新构建**，不是新版本号。
    #[test]
    fn a_same_version_rebuild_is_reported_as_a_new_build() {
        let mut h = sample();
        h.upgrade_available = Some(h.version.clone());
        let t = format_status(&h, Hy2Auth::Http, None, &StatusExtra::default(), t0());
        assert!(t.contains("同版本的新构建"), "{t}");
        assert!(t.contains("bui upgrade"), "要说清下一步怎么做：{t}");
        assert!(
            !t.contains("4.0.0 可用"),
            "别报成「新版本 4.0.0 可用」：{t}"
        );
    }

    /// 排「谁都登不上」时第一眼要看的就是这一行（2026-09-13 裁决：默认 http，command 是退路）。
    #[test]
    fn status_text_names_the_current_hysteria_auth_mode() {
        let t = format_status(
            &sample(),
            Hy2Auth::Http,
            None,
            &StatusExtra::default(),
            t0(),
        );
        assert!(
            t.contains("鉴权模式") && t.contains("auth.type=http"),
            "{t}"
        );
        // 4.1：这一行的作用域只有直连（C5：`bui set hy2-auth` 语义同步缩窄）
        assert!(t.contains("仅直连"), "{t}");
        assert!(!t.contains("auth.type=command"), "{t}");
        let t = format_status(
            &sample(),
            Hy2Auth::Command,
            None,
            &StatusExtra::default(),
            t0(),
        );
        assert!(t.contains("auth.type=command") && t.contains("退路"), "{t}");
        assert!(!t.contains("auth.type=http"), "{t}");
    }

    /// 2026-09-14 裁决：订阅按随机 token 取，旧用户名链接只在宽限期内还认。
    /// 运维排「订阅取不到」时要一眼看出现在是哪一种。
    #[test]
    fn status_text_says_whether_the_username_links_still_work() {
        // 没设宽限期（全新装机、或运维 `bui set legacy-sub off`）⇒ 已停用
        let t = format_status(
            &sample(),
            Hy2Auth::Http,
            None,
            &StatusExtra::default(),
            t0(),
        );
        assert!(t.contains("旧订阅链接  已停用"), "{t}");
        // 宽限期内 ⇒ 报还剩多久
        let t = format_status(
            &sample(),
            Hy2Auth::Http,
            Some("2026-09-21T00:00:00Z"),
            &StatusExtra::default(),
            t0(),
        );
        assert!(
            t.contains("旧订阅链接  用户名链接还剩 7d 0h 到期（2026-09-21T00:00:00Z）"),
            "{t}"
        );
        // 到期 ⇒ 已过期
        let t = format_status(
            &sample(),
            Hy2Auth::Http,
            Some("2026-09-13T23:59:59Z"),
            &StatusExtra::default(),
            t0(),
        );
        assert!(
            t.contains("旧订阅链接  已过期（2026-09-13T23:59:59Z）"),
            "{t}"
        );
        // 解析不出来的时刻按停用报（端点侧判不出「早于」，一律不认）
        assert!(
            format_legacy_sub(Some("下周"), t0()).contains("已停用"),
            "垃圾值按停用报"
        );
    }

    #[test]
    fn status_text_is_clean_when_everything_is_ok() {
        let mut h = sample();
        h.status = "ok".into();
        h.services.iter_mut().for_each(|s| s.active = true);
        h.reconcile = None;
        h.drift.clear();
        h.upgrade_available = None;
        let t = format_status(&h, Hy2Auth::Http, None, &StatusExtra::default(), t0());
        assert!(t.contains("无漂移"));
        assert!(!t.contains("重启失败"));
        assert!(!t.contains("4.0.1"));
    }

    #[test]
    fn status_ends_with_the_latest_incidents() {
        use crate::modules::sentinel::incidents::{Incident, Level};
        assert_eq!(
            format_recent_incidents(&[]),
            "最近事件    无（`bui incidents` 查看全量）"
        );
        let i = Incident {
            at: "2026-09-11T00:00:03Z".into(),
            unit: "b-ui-relay".into(),
            signature: "relay_upstream_error".into(),
            subject: "isp2.example.net:10007".into(),
            action: "probe_and_borrow".into(),
            result: "IP 198.51.100.8 不可达，槽 1 已临时切到 198.51.100.7".into(),
            level: Level::Error,
            sample: None,
        };
        let t = format_recent_incidents(&[i.clone(), i]);
        assert!(
            t.starts_with("最近事件    （`bui incidents` 查看全量）"),
            "{t}"
        );
        assert_eq!(t.lines().count(), 3);
        assert!(t.contains("槽 1 已临时切到 198.51.100.7"));
        assert_eq!(STATUS_INCIDENTS, 5, "spec §5.7：status 只显示最近 5 条");
    }
}
