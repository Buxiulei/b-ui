//! 上游池的增删改（spec §5.1）：四种粘贴格式、类型自动探测、出口画像写回 `verified`，
//! 以及总开关 / 分流模式 / 分流关键字 / 优先级。
//!
//! 写 state 一律经 [`super::state::update_group`]（契约决策 §B）：本模块**不渲染**
//! `singbox-relay.json`，改完 state 由 P1 的对账器重渲染并重启 `b-ui-relay`。

use super::proxy::{ProbeError, Prober};
use super::{check, proxy, state, EXIT_IP_HOST, EXIT_IP_URL, MAX_UPSTREAMS, MEMBER_PREFIX};
use crate::reconcile::DaemonCtx;
use crate::util::fmt_rfc3339;
use bui_schema::model::{ResiMode, ResidentialGroup, Upstream, UpstreamKind};
use bui_schema::parse::{upstream_url, ParseError, UpstreamInput};
use serde::Serialize;
use std::sync::Arc;
use uuid::Uuid;

/// 一次「加上游」的结果（面板与 CLI 直接回显）。**不含凭据**。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AddOutcome {
    pub id: Uuid,
    pub name: String,
    pub kind: UpstreamKind,
    pub exit_ip: Option<String>,
    pub isp: Option<String>,
    pub class_label: String,
}

#[derive(Debug, thiserror::Error)]
pub enum UpstreamError {
    #[error("{0}")]
    Parse(#[from] ParseError),
    #[error("SOCKS5 与 HTTP 两种协议都连不上 ({host}:{port}) —— 请核对凭据与端口（供应商的 SOCKS5 与 HTTP 端口通常不同）")]
    Unverifiable { host: String, port: u16 },
    #[error("上游凭据失效（407 / SOCKS5 认证被拒）")]
    AuthFailed,
    #[error("出口 IP 与本机相同（{0}），代理未生效")]
    NotProxied(String),
    #[error("代理节点池已满（上限 {MAX_UPSTREAMS} 个）")]
    PoolFull,
    #[error("未找到匹配的上游")]
    NotFound,
    #[error("定位不到上游「{0}」：请用 uuid、resi-N / url-N（N 是池内序号，见 status / health）或 host:port")]
    Unresolvable(String),
    #[error("代理节点池为空，请先添加至少 1 个住宅上游")]
    PoolEmpty,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// 上游定位：面板用 `host:port`（v3 的 `DELETE /api/residential/urls/<host:port>`），
/// 规范端点与 CLI 用 id
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpstreamSel {
    Id(Uuid),
    HostPort { host: String, port: u16 },
}

impl UpstreamSel {
    pub fn parse_host_port(s: &str) -> Option<Self> {
        let (host, port) = s.rsplit_once(':')?;
        if host.is_empty() {
            return None;
        }
        Some(Self::HostPort {
            host: host.to_string(),
            port: port.parse().ok()?,
        })
    }
}

/// 上游显示名的前缀：`url-1..url-N`（面板与 CLI 的 `status` 列的就是它）
pub const NAME_PREFIX: &str = "url-";

/// `url-1..url-N` 稠密重排（v3 `add_url_to_config` 的 `to_entries|map(name:…)`）。
/// 名字**必须**跟着下标重排，否则删掉中间一条后 `url-3` 与 `resi-2` 对不上，面板表与
/// relay 成员就错位了。
pub fn renumber(ups: &mut [Upstream]) {
    for (i, u) in ups.iter_mut().enumerate() {
        u.name = format!("{NAME_PREFIX}{}", i + 1);
    }
}

/// `resi-3` / `url-3` → `3`（大小写不敏感）。`0`、负数与非数字返回 `None`。
/// 两个前缀同一口径：`resi-N` 是 relay 的成员 tag（`clash::tags`），`url-N` 是
/// [`renumber`] 排的显示名，二者都等于**池内 1 起下标**。
fn pool_index(s: &str) -> Option<usize> {
    let low = s.to_ascii_lowercase();
    let digits = low
        .strip_prefix(MEMBER_PREFIX)
        .or_else(|| low.strip_prefix(NAME_PREFIX))?;
    let n: usize = digits.parse().ok()?;
    (n >= 1).then_some(n)
}

/// 定位一条上游：接受 uuid、`resi-N` / `url-N`（N 是池内 1 起下标，与 `status` /
/// `health` 的成员序号、relay 的成员 tag 同一口径）与 `host:port`。
///
/// **CLI 的 `select` / `check --id` / `remove` 与对应端点共用它**（CLI 自己不读 state，
/// 原样把字符串送到端点）：只认 uuid 的话，运维照着 `status` 里的 `resi-3` 敲进去只会
/// 拿到 serde 的「invalid character」，没人知道该填什么。错误文案因此列出全部写法。
pub fn resolve_upstream(g: &ResidentialGroup, raw: &str) -> Result<Uuid, UpstreamError> {
    let s = raw.trim();
    let unresolvable = || UpstreamError::Unresolvable(s.to_string());
    if let Ok(id) = Uuid::parse_str(s) {
        return g
            .upstreams
            .iter()
            .find(|u| u.id == id)
            .map(|u| u.id)
            .ok_or_else(unresolvable);
    }
    if let Some(n) = pool_index(s) {
        return g
            .upstreams
            .get(n - 1)
            .map(|u| u.id)
            .ok_or_else(unresolvable);
    }
    if let Some(UpstreamSel::HostPort { host, port }) = UpstreamSel::parse_host_port(s) {
        return g
            .upstreams
            .iter()
            .find(|u| u.host == host && u.port == port)
            .map(|u| u.id)
            .ok_or_else(unresolvable);
    }
    Err(unresolvable())
}

/// 类型自动探测：`socks5` → `http`，**整轮重试一次**（v3.6.2 R12：实测有效的端口也会偶发
/// 抽风，同一行立刻重试就成）。`kind` 已指定时只试那一种、不重试。
/// 全都失败、且 `get` 没能识别出凭据失效时，再对每个候选类型用
/// [`proxy::confirm_auth_failure`] 补判一次 [`EXIT_IP_HOST`]`:443`——`get` 在 https
/// 目标上判不出 407（见 T3 `confirm_auth_failure` 的注释），不补判的话真机上凭据写错
/// 只会得到 `Unverifiable`（「协议都连不上」）这条误导性文案，运维会去改端口而不是换凭据。
pub fn detect_kind(
    p: &dyn Prober,
    probe: &Upstream,
    want: Option<UpstreamKind>,
) -> Result<(UpstreamKind, String), UpstreamError> {
    // v3.6.2 R12：auto 整轮重试一次（实测有效的端口也会偶发抽风，
    // 同一行立刻重试就成，不该让用户以为凭据写错了）
    let (candidates, rounds): (Vec<UpstreamKind>, usize) = match want {
        Some(k) => (vec![k], 1),
        None => (vec![UpstreamKind::Socks5, UpstreamKind::Http], 2),
    };
    let mut auth_failed = false;
    for _round in 0..rounds {
        for kind in &candidates {
            let mut probe = probe.clone();
            probe.kind = *kind;
            match p.get(&probe, EXIT_IP_URL) {
                Ok(hp) => {
                    let ip = hp.body.trim().to_string();
                    if !ip.is_empty() {
                        return Ok((*kind, ip));
                    }
                }
                Err(ProbeError::AuthFailed) => auth_failed = true,
                Err(_) => {}
            }
        }
    }
    if !auth_failed {
        // `get` 分不出「凭据失效」与「连不上」：https 目标经 HTTP 上游走 CONNECT 隧道，
        // 407 发生在隧道建立阶段，reqwest 只给一个 Err（T3 `confirm_auth_failure` 的注释）。
        // 逐个候选类型补判一次，否则真机上凭据写错报的是 Unverifiable（「端口/协议不对」），
        // 把运维引到错误的排查方向。
        for kind in &candidates {
            let mut probe = probe.clone();
            probe.kind = *kind;
            if proxy::confirm_auth_failure(p, &probe, EXIT_IP_HOST) {
                auth_failed = true;
                break;
            }
        }
    }
    if auth_failed {
        return Err(UpstreamError::AuthFailed);
    }
    Err(UpstreamError::Unverifiable {
        host: probe.host.clone(),
        port: probe.port,
    })
}

/// 加一条上游：解析 → 类型探测 → 出口画像 → 写 state。**凭据只进 state**，
/// 日志与错误信息里一律脱敏。同 `host:port` 是**覆盖**语义（v3 同），所以覆盖时
/// 不占新名额、池满也允许，并且**沿用既有条目的 `id`**（绝不换新 uuid）：`id` 是
/// 全模块的运行时主键（契约决策 §C），换掉它会让 `runtime.selected_upstream_id`、
/// `runtime.health[id]`、`runtime.checks[id]` 与 `state.blacklist.auto[].upstream_id`
/// 里的旧 uuid 一起悬空。
///
/// 只做出口画像、**不做端口集**：面板的添加请求预算是 60 秒（v3 `RESI_ADD_TIMEOUT_MS`），
/// 类型探测最坏 2 轮 × 2 协议 × 10 秒 = 40 秒，再加端口集就超了。端口集由调用方
/// （T10 的 handler）返回后异步起一次 `check::run_and_store` 补。
pub async fn add(
    ctx: &DaemonCtx,
    p: Arc<dyn Prober>,
    raw: &str,
) -> Result<AddOutcome, UpstreamError> {
    let input: UpstreamInput = upstream_url(raw)?;
    let s = ctx.store.read().await;
    let g = state::group_of(&s);
    // 同 host:port 是**覆盖**既有条目（下面 update_group 里先 retain 再 push），不占新名额：
    // 池满时改一条已有上游的凭据/类型不该被 400 拒
    let existing_id = g
        .upstreams
        .iter()
        .find(|u| u.host == input.host && u.port == input.port)
        .map(|u| u.id);
    if existing_id.is_none() && g.upstreams.len() >= MAX_UPSTREAMS {
        return Err(UpstreamError::PoolFull);
    }
    let vps_ip = s.node.public_ip.clone();
    drop(s);

    // **覆盖时沿用既有 id**：id 是全模块的运行时主键（契约决策 §C）。换新 uuid 会让
    // runtime.selected_upstream_id / runtime.health[id] / runtime.checks[id] 与
    // state.blacklist.auto[].upstream_id 里的旧 uuid 全部悬空：旧 auto 条目不再被渲染、
    // 永远不进复核、remove_auto 也删不到，replay_loop 因 tag_of 为 None 跳过重放。
    let id = existing_id.unwrap_or_else(Uuid::new_v4);
    let mut up = Upstream {
        id,
        name: String::new(), // renumber 里填
        kind: input.kind.unwrap_or(UpstreamKind::Socks5),
        host: input.host,
        port: input.port,
        username: input.username,
        password: input.password,
        priority: 100,
        provider: None,
        region: None,
        ports_allowed: None,
        verified: None,
    };

    let (kind, exit_ip) = {
        let (pp, probe, want) = (p.clone(), up.clone(), input.kind);
        tokio::task::spawn_blocking(move || detect_kind(pp.as_ref(), &probe, want))
            .await
            .map_err(|e| UpstreamError::Other(anyhow::anyhow!(e)))??
    };
    if exit_ip == vps_ip {
        // v3 verify 的那条判据：出口与本机相同说明流量根本没经代理
        return Err(UpstreamError::NotProxied(vps_ip));
    }
    up.kind = kind;

    // 出口画像（三源交叉）；失败不阻塞添加，verified 留空由后续体检补
    let now = ctx.host.now();
    let (class, exit, sources, _notes) = {
        let (pp, u2) = (p.clone(), up.clone());
        tokio::task::spawn_blocking(move || check::probe_exit(pp.as_ref(), &u2))
            .await
            .map_err(|e| UpstreamError::Other(anyhow::anyhow!(e)))?
    };
    let at = fmt_rfc3339(now);
    up.verified = check::to_verified(&exit, &at);
    up.region = exit.country.clone();
    tracing::info!(
        upstream = %proxy::proxy_url(&up),
        kind = ?kind, class = ?class, sources = ?sources, "新增住宅上游"
    );

    let isp = exit
        .asn
        .map(|a| format!("AS{a}"))
        .into_iter()
        .chain(exit.org.clone())
        .collect::<Vec<_>>()
        .join(" ");
    let up2 = up.clone();
    state::update_group(&ctx.store, &ctx.bus, move |g| {
        // 同 host:port 视为同一上游，覆盖（v3 add_url_to_config 的 map(select(…)) 同语义）
        g.upstreams
            .retain(|u| !(u.host == up2.host && u.port == up2.port));
        g.upstreams.push(up2);
        renumber(&mut g.upstreams);
        g.enabled = true;
        if g.selected_upstream_id.is_none() {
            g.selected_upstream_id = g.upstreams.first().map(|u| u.id);
        }
    })
    .await?;
    let added = state::group_of(&*ctx.store.read().await)
        .upstreams
        .iter()
        .find(|u| u.id == id)
        .cloned()
        .ok_or(UpstreamError::NotFound)?;
    Ok(AddOutcome {
        id: added.id,
        name: added.name,
        kind,
        exit_ip: Some(exit_ip),
        isp: (!isp.is_empty()).then_some(isp),
        class_label: class.label().to_string(),
    })
}

/// 删一条上游；删掉最后一条时顺带 `enabled = false`（v3 `enable --remove` 同语义）
pub async fn remove(ctx: &DaemonCtx, sel: &UpstreamSel) -> Result<(), UpstreamError> {
    let g = state::group_of(&*ctx.store.read().await);
    let target = g
        .upstreams
        .iter()
        .find(|u| match sel {
            UpstreamSel::Id(id) => u.id == *id,
            UpstreamSel::HostPort { host, port } => u.host == *host && u.port == *port,
        })
        .cloned()
        .ok_or(UpstreamError::NotFound)?;
    let id = target.id;
    // R3 ①：删上游是**唯一**允许缩短池子的路径，所以它自己必须留下日志——真机上
    // 那两条上游「无任何日志地消失」时，这里一个字都没记，事后连「是谁删的」都无从判断。
    // 只记 name/id/host:port，不记凭据。
    tracing::info!(
        upstream = %target.name, id = %id, endpoint = %format!("{}:{}", target.host, target.port),
        "删除住宅上游"
    );
    state::update_group_as(
        &ctx.store,
        &ctx.bus,
        crate::state::store::CALLER_RESI_REMOVE,
        move |g| {
            g.upstreams.retain(|u| u.id != id);
            renumber(&mut g.upstreams);
            // 该上游的 auto 条目一起删：留着没人复核，还会被 render/relay 的
            // `upstream_id == selected` 过滤悄悄忽略
            g.blacklist.auto.retain(|e| e.upstream_id != id);
            if g.selected_upstream_id == Some(id) {
                g.selected_upstream_id = g.upstreams.first().map(|u| u.id);
            }
            if g.upstreams.is_empty() {
                g.enabled = false; // relay 回落 fail-open 直连（v3 `enable --remove` 同语义）
                g.selected_upstream_id = None;
            }
        },
    )
    .await?;
    state::update(&ctx.runtime, |r| {
        r.checks.remove(&id.to_string());
        r.candidates.retain(|_, c| c.upstream_id != id);
        r.pending.retain(|e| e.upstream_id != id);
        // 运行时主键是 uuid（契约决策 §C）：健康 streak 与「当前生效」都得跟着清，
        // 否则 replay_loop 会去重放一条已经不存在的上游，24h 成功率也会留着僵尸样本
        r.health.remove(&id.to_string());
        // 告警也跟着 uuid 走：留着的话，下一条上游会复用它的 url-N 名字并顶着这条
        // 「凭据失效 407」（2026-09-12 bwg-rick 的真机事故）
        state::clear_upstream_alert(r, id);
        if r.selected_upstream_id == Some(id) {
            r.selected_upstream_id = None;
            r.selected_pending_persist = false;
        }
        // 手动锁定也跟着清：留着会让面板显示一条锁在「已移除」上游上的锁定（R2 ①）
        if r.manual_selected_id == Some(id) {
            r.manual_selected_id = None;
        }
    })
    .await;
    Ok(())
}

/// 总开关。开启要求池非空（v3 `POST /api/residential/enable` 的 400 分支）
pub async fn set_enabled(ctx: &DaemonCtx, on: bool) -> Result<(), UpstreamError> {
    if on
        && state::group_of(&*ctx.store.read().await)
            .upstreams
            .is_empty()
    {
        return Err(UpstreamError::PoolEmpty);
    }
    state::update_group(&ctx.store, &ctx.bus, |g| g.enabled = on).await?;
    Ok(())
}

/// global / split（v3 `POST /api/residential/global`）
pub async fn set_mode(ctx: &DaemonCtx, global: bool) -> Result<(), UpstreamError> {
    let mode = if global {
        ResiMode::Global
    } else {
        ResiMode::Split
    };
    state::update_group(&ctx.store, &ctx.bus, |g| g.mode = mode).await?;
    Ok(())
}

/// 分流关键字：`None` = 回到跟随默认表（v3 的 `set-domains null`，R12 的语义）
pub async fn set_keywords(ctx: &DaemonCtx, kw: Option<Vec<String>>) -> Result<(), UpstreamError> {
    // None / 空列表 = 回到跟随 DEFAULT_KEYWORDS（R12：不把当时的默认表固化成自定义，
    // 否则后续版本扩充默认表这台机器永远跟不上）
    let cleaned = kw.and_then(|list| {
        let mut v: Vec<String> = list
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let mut seen = std::collections::BTreeSet::new();
        v.retain(|s| seen.insert(s.clone()));
        (!v.is_empty()).then_some(v)
    });
    state::update_group(&ctx.store, &ctx.bus, |g| g.keywords = cleaned).await?;
    Ok(())
}

/// 调优先级（切换目标的第一排序键）
pub async fn set_priority(ctx: &DaemonCtx, id: Uuid, priority: u32) -> Result<(), UpstreamError> {
    let g = state::group_of(&*ctx.store.read().await);
    if !g.upstreams.iter().any(|u| u.id == id) {
        return Err(UpstreamError::NotFound);
    }
    state::update_group(&ctx.store, &ctx.bus, |g| {
        if let Some(u) = g.upstreams.iter_mut().find(|u| u.id == id) {
            u.priority = priority;
        }
    })
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::EventBus;
    use crate::modules::residential::proxy::{FakeProber, HttpProbe};
    use crate::modules::residential::state as rstate;
    use crate::state::runtime::Runtime;
    use crate::state::store::Store;
    use crate::sys::fake::FakeHost;
    use crate::testutil::sample_state;
    use pretty_assertions::assert_eq;

    async fn ctx(d: &tempfile::TempDir) -> DaemonCtx {
        let mut s = sample_state();
        s.node.public_ip = "203.0.113.10".into();
        // 从空池起步，便于断言添加路径
        let g = s
            .residential
            .groups
            .get_mut(super::super::GROUP_DEFAULT)
            .expect("sample_state 带 default 分组");
        g.upstreams.clear();
        g.selected_upstream_id = None;
        DaemonCtx {
            store: Store::create(d.path().join("state.json"), s).await.unwrap(),
            runtime: Runtime::load(d.path().join("runtime.json")),
            bus: EventBus::new(),
            host: std::sync::Arc::new(FakeHost::new()),
            paths: bui_schema::paths::Paths::default_server(),
        }
    }

    fn prober_ok(exit: &str) -> std::sync::Arc<FakeProber> {
        let p = std::sync::Arc::new(FakeProber::new());
        let exit = exit.to_string();
        p.with(|i| {
            i.gets.insert(
                EXIT_IP_URL.into(),
                Ok(HttpProbe {
                    status: 200,
                    body: exit.clone(),
                }),
            );
            i.gets.insert(
                crate::modules::residential::check::EXIT_SOURCES[0].1.into(),
                Ok(HttpProbe {
                    status: 200,
                    body: serde_json::json!({"ip": exit, "asn": 33667, "asOrganization": "Comcast",
                                             "country": "US", "isResidential": true})
                    .to_string(),
                }),
            );
        });
        p
    }

    #[tokio::test]
    async fn add_accepts_all_four_paste_formats_and_records_verified() {
        let d = tempfile::tempdir().unwrap();
        let c = ctx(&d).await;
        let p = prober_ok("198.51.100.7");
        for raw in [
            "socks5://user1:pw1@isp1.example.net:1080",
            "http://user1:pw1@isp2.example.net:10007",
            "isp3.example.net:1084:user1:pw1",
            "user1:pw1@isp4.example.net:1080",
        ] {
            add(&c, p.clone(), raw).await.unwrap();
        }
        let g = rstate::group_of(&*c.store.read().await);
        assert_eq!(g.upstreams.len(), 4);
        assert_eq!(
            g.upstreams[0].kind,
            UpstreamKind::Socks5,
            "scheme 指定了就不探测"
        );
        assert_eq!(g.upstreams[1].kind, UpstreamKind::Http);
        assert_eq!(g.upstreams[2].host, "isp3.example.net");
        assert_eq!(g.upstreams[2].password, "pw1");
        assert_eq!(
            g.upstreams
                .iter()
                .map(|u| u.name.clone())
                .collect::<Vec<_>>(),
            vec!["url-1", "url-2", "url-3", "url-4"]
        );
        let v = g.upstreams[0].verified.clone().unwrap();
        assert_eq!(
            (v.ip.as_str(), v.asn, v.org.as_deref()),
            ("198.51.100.7", Some(33667), Some("Comcast"))
        );
        assert!(g.enabled, "第一条添加成功即启用（v3 save_config true）");
        assert_eq!(
            g.selected_upstream_id,
            Some(g.upstreams[0].id),
            "首条成为落点"
        );
    }

    #[tokio::test]
    async fn auto_detection_falls_back_to_http_and_retries_the_whole_round_once() {
        let d = tempfile::tempdir().unwrap();
        let c = ctx(&d).await;
        // 没写 scheme → auto：先 SOCKS5 再 HTTP，整轮重试一次
        let p = std::sync::Arc::new(FakeProber::new());
        p.with(|i| {
            // FakeProber 按 URL 查表、不区分上游类型，所以用调用流水断言顺序与次数
            i.gets
                .insert(EXIT_IP_URL.into(), Err("connection refused".into()));
        });
        let e = add(&c, p.clone(), "user1:pw1@isp.example.net:1080")
            .await
            .unwrap_err();
        assert!(matches!(e, UpstreamError::Unverifiable { .. }), "实际 {e}");
        assert_eq!(
            p.calls().iter().filter(|c| c.starts_with("get:")).count(),
            4,
            "socks5 / http 各两轮：整轮重试一次（v3.6.2 R12）"
        );
        assert!(
            rstate::group_of(&*c.store.read().await)
                .upstreams
                .is_empty(),
            "失败不写 state"
        );
    }

    #[tokio::test]
    async fn add_rejects_a_407_and_an_exit_ip_equal_to_the_vps() {
        let d = tempfile::tempdir().unwrap();
        let c = ctx(&d).await;
        let p = std::sync::Arc::new(FakeProber::new());
        p.with(|i| {
            i.gets
                .insert(EXIT_IP_URL.into(), Err("__auth_failed__".into()));
        });
        assert!(matches!(
            add(&c, p, "http://user1:pw1@isp.example.net:10007").await,
            Err(UpstreamError::AuthFailed)
        ));
        // 出口 IP == 本机公网 IP ⇒ 代理没生效（v3 verify 的那条判据）
        let p2 = prober_ok("203.0.113.10");
        assert!(matches!(
            add(&c, p2, "http://user1:pw1@isp.example.net:10007").await,
            Err(UpstreamError::NotProxied(_))
        ));
    }

    #[tokio::test]
    async fn add_rejects_bad_pastes_with_the_parse_message_and_enforces_the_pool_cap() {
        let d = tempfile::tempdir().unwrap();
        let c = ctx(&d).await;
        let p = prober_ok("198.51.100.7");
        assert!(matches!(
            add(&c, p.clone(), "https://u:p@h:1").await,
            Err(UpstreamError::Parse(_))
        ));
        assert!(matches!(
            add(&c, p.clone(), "socks5://u@h:1").await,
            Err(UpstreamError::Parse(_))
        ));
        for i in 0..MAX_UPSTREAMS {
            add(
                &c,
                p.clone(),
                &format!("http://user1:pw1@isp{i}.example.net:10007"),
            )
            .await
            .unwrap();
        }
        assert!(matches!(
            add(&c, p.clone(), "http://user1:pw1@overflow.example.net:10007").await,
            Err(UpstreamError::PoolFull)
        ));
        // 池满时「更新一条已有上游」不该被 400 拒：同 host:port 是覆盖，不占新名额
        add(&c, p, "socks5://user1:pw1@isp0.example.net:10007")
            .await
            .unwrap();
        let g = rstate::group_of(&*c.store.read().await);
        assert_eq!(g.upstreams.len(), MAX_UPSTREAMS, "覆盖不增长");
        assert_eq!(
            g.upstreams
                .iter()
                .find(|u| u.host == "isp0.example.net")
                .unwrap()
                .kind,
            UpstreamKind::Socks5,
            "覆盖后 kind 跟着新粘贴的那一行走"
        );
    }

    #[tokio::test]
    async fn overwriting_the_same_host_port_keeps_the_id_so_runtime_and_blacklist_keys_survive() {
        // 覆盖必须沿用既有 id（契约决策 §C：id 是运行时主键）。换新 uuid 的话，
        // 下面这四处引用会一起悬空：state.selected_upstream_id / blacklist.auto[].upstream_id
        // / runtime.selected_upstream_id / runtime.health[id]。
        let d = tempfile::tempdir().unwrap();
        let c = ctx(&d).await;
        let p = prober_ok("198.51.100.7");
        let first = add(&c, p.clone(), "http://user1:pw1@isp1.example.net:10007")
            .await
            .unwrap();
        rstate::update_group(&c.store, &c.bus, |g| {
            g.blacklist.auto.push(bui_schema::model::AutoEntry {
                upstream_id: first.id,
                rule: bui_schema::model::Rule::DomainSuffix("gateway.icloud.com".into()),
                hits: 9,
                confirmed_at: "2026-09-12T00:00:00Z".into(),
                last_verified_at: "2026-09-12T00:00:00Z".into(),
                passes: 0,
            });
        })
        .await
        .unwrap();
        rstate::update(&c.runtime, |r| {
            r.selected_upstream_id = Some(first.id);
            r.health
                .insert(first.id.to_string(), rstate::HealthState::default());
        })
        .await;

        // 同 host:port、换协议与凭据重新粘贴一次
        let again = add(&c, p, "socks5://user2:pw2@isp1.example.net:10007")
            .await
            .unwrap();
        assert_eq!(again.id, first.id, "覆盖沿用既有 id，不换 uuid");
        let g = rstate::group_of(&*c.store.read().await);
        assert_eq!(g.upstreams.len(), 1, "覆盖不增长");
        assert_eq!(g.upstreams[0].id, first.id);
        assert_eq!(
            g.upstreams[0].kind,
            UpstreamKind::Socks5,
            "类型跟着新粘贴的那一行走"
        );
        assert_eq!(g.upstreams[0].password, "pw2", "凭据也跟着走");
        assert_eq!(g.upstreams[0].name, "url-1");
        assert_eq!(
            g.selected_upstream_id,
            Some(first.id),
            "state 落点没被改成悬空 uuid"
        );
        assert_eq!(
            g.blacklist
                .auto
                .iter()
                .map(|a| a.upstream_id)
                .collect::<Vec<_>>(),
            vec![first.id],
            "该上游学到的 auto 条目仍挂在同一个 id 上（换 uuid 会让它永远不进复核、也删不到）"
        );
        let r = rstate::read(&c.runtime).await;
        assert_eq!(
            r.selected_upstream_id,
            Some(first.id),
            "replay_loop 还能重放到它"
        );
        assert!(
            r.health.contains_key(&first.id.to_string()),
            "健康 streak 与 24h 成功率不清零"
        );
    }

    #[tokio::test]
    async fn remove_by_host_port_renumbers_and_disables_when_the_pool_empties() {
        let d = tempfile::tempdir().unwrap();
        let c = ctx(&d).await;
        let p = prober_ok("198.51.100.7");
        for i in 1..=3 {
            add(
                &c,
                p.clone(),
                &format!("http://user1:pw1@isp{i}.example.net:10007"),
            )
            .await
            .unwrap();
        }
        remove(
            &c,
            &UpstreamSel::parse_host_port("isp2.example.net:10007").unwrap(),
        )
        .await
        .unwrap();
        let g = rstate::group_of(&*c.store.read().await);
        assert_eq!(
            g.upstreams
                .iter()
                .map(|u| (u.host.clone(), u.name.clone()))
                .collect::<Vec<_>>(),
            vec![
                ("isp1.example.net".to_string(), "url-1".to_string()),
                ("isp3.example.net".to_string(), "url-2".to_string()),
            ],
            "名字跟着下标稠密重排，否则 url-N 与 resi-N 会错位"
        );
        assert!(matches!(
            remove(
                &c,
                &UpstreamSel::parse_host_port("nope.example.net:1").unwrap()
            )
            .await,
            Err(UpstreamError::NotFound)
        ));
        for host in ["isp1.example.net", "isp3.example.net"] {
            remove(
                &c,
                &UpstreamSel::parse_host_port(&format!("{host}:10007")).unwrap(),
            )
            .await
            .unwrap();
        }
        let g = rstate::group_of(&*c.store.read().await);
        assert!(g.upstreams.is_empty());
        assert!(
            !g.enabled,
            "最后一条被移除即关总开关（relay 回落 fail-open 直连）"
        );
        assert_eq!(g.selected_upstream_id, None);
    }

    #[tokio::test]
    async fn remove_also_drops_that_upstreams_auto_blacklist_and_runtime_traces() {
        let d = tempfile::tempdir().unwrap();
        let c = ctx(&d).await;
        let p = prober_ok("198.51.100.7");
        let a = add(&c, p.clone(), "http://user1:pw1@isp1.example.net:10007")
            .await
            .unwrap();
        add(&c, p, "http://user1:pw1@isp2.example.net:10007")
            .await
            .unwrap();
        rstate::update_group(&c.store, &c.bus, |g| {
            g.blacklist.auto.push(bui_schema::model::AutoEntry {
                upstream_id: a.id,
                rule: bui_schema::model::Rule::DomainSuffix("gateway.icloud.com".into()),
                hits: 9,
                confirmed_at: "2026-09-12T00:00:00Z".into(),
                last_verified_at: "2026-09-12T00:00:00Z".into(),
                passes: 0,
            });
        })
        .await
        .unwrap();
        rstate::update(&c.runtime, |r| {
            r.checks
                .insert(a.id.to_string(), serde_json::json!({"x": 1}));
            // 运行时主键是 uuid（契约决策 §C），这两处也必须跟着清
            r.health
                .insert(a.id.to_string(), rstate::HealthState::default());
            r.selected_upstream_id = Some(a.id);
            r.selected_pending_persist = true;
        })
        .await;
        remove(&c, &UpstreamSel::Id(a.id)).await.unwrap();
        let g = rstate::group_of(&*c.store.read().await);
        assert!(
            g.blacklist.auto.is_empty(),
            "该上游的 auto 条目一起删（不然永远没人复核它）"
        );
        let r = rstate::read(&c.runtime).await;
        assert!(!r.checks.contains_key(&a.id.to_string()));
        assert!(
            !r.health.contains_key(&a.id.to_string()),
            "健康 streak 跟着 uuid 一起清"
        );
        assert_eq!(
            r.selected_upstream_id, None,
            "别让 replay_loop 去重放一条已删的上游"
        );
        assert!(!r.selected_pending_persist);
    }

    #[tokio::test]
    async fn resolve_upstream_takes_a_uuid_a_pool_index_or_a_host_port() {
        // 真机上运维照着 status / health 里的 resi-3 / url-3 敲进 `bui residential select`，
        // 只认 uuid 的话得到的是「invalid character」——四种写法必须都能定位。
        let d = tempfile::tempdir().unwrap();
        let c = ctx(&d).await;
        let p = prober_ok("198.51.100.7");
        for i in 1..=3 {
            add(
                &c,
                p.clone(),
                &format!("http://user1:pw1@isp{i}.example.net:1000{i}"),
            )
            .await
            .unwrap();
        }
        let g = rstate::group_of(&*c.store.read().await);
        let third = g.upstreams[2].id;
        for raw in [
            third.to_string(),
            "resi-3".to_string(),
            "url-3".to_string(),
            "RESI-3".to_string(),
            "isp3.example.net:10003".to_string(),
            " resi-3 ".to_string(),
        ] {
            assert_eq!(
                resolve_upstream(&g, &raw).unwrap(),
                third,
                "写法 {raw:?} 必须能定位到池内第 3 条"
            );
        }
        // 池内序号与 relay 的成员 tag / status 的 url-N 同一口径
        assert_eq!(
            resolve_upstream(&g, "resi-1").unwrap(),
            g.upstreams[0].id,
            "resi-N 是 1 起下标"
        );
        // 定位不到时文案要把三种写法列出来，否则运维只能猜
        for raw in ["resi-9", "url-0", "resi-x", "nope.example.net:1", "", "abc"] {
            let e = resolve_upstream(&g, raw).unwrap_err();
            assert!(matches!(e, UpstreamError::Unresolvable(_)), "{raw}: {e}");
            let msg = e.to_string();
            for form in ["uuid", "resi-N", "url-N", "host:port"] {
                assert!(msg.contains(form), "{raw} 的错误文案缺 {form}：{msg}");
            }
        }
        // 池里没有的 uuid 同样报「定位不到」，不是 500
        assert!(matches!(
            resolve_upstream(&g, &Uuid::from_u128(999).to_string()),
            Err(UpstreamError::Unresolvable(_))
        ));
    }

    #[tokio::test]
    async fn remove_drops_the_upstreams_alert_so_a_new_entry_never_inherits_it() {
        // 真机事故（2026-09-12 bwg-rick）：告警按 url-N 存，删条目后名字被新条目复用，
        // 新 Decodo 顶着上一个账号的「凭据失效 407」告警。告警以 uuid 为键 + 删条目即清。
        let d = tempfile::tempdir().unwrap();
        let c = ctx(&d).await;
        let p = prober_ok("198.51.100.7");
        let a = add(&c, p.clone(), "http://user1:pw1@old.example.net:10007")
            .await
            .unwrap();
        rstate::update(&c.runtime, |r| {
            rstate::set_upstream_alert(r, a.id, "上游 old.example.net:10007 凭据失效（407）");
        })
        .await;
        remove(&c, &UpstreamSel::Id(a.id)).await.unwrap();
        assert!(
            rstate::read(&c.runtime).await.upstream_alerts.is_empty(),
            "删条目连告警一起清"
        );
        // 新条目占用同一个 url-1 名字，绝不能继承旧告警
        let b = add(&c, p, "http://user2:pw2@new.example.net:10007")
            .await
            .unwrap();
        assert_ne!(b.id, a.id);
        let g = rstate::group_of(&*c.store.read().await);
        assert_eq!(g.upstreams[0].name, "url-1", "名字确实被复用了");
        assert!(
            rstate::visible_alerts(&g, &rstate::read(&c.runtime).await).is_empty(),
            "新上游不背旧账号的告警"
        );
    }

    #[tokio::test]
    async fn toggles_follow_the_v3_semantics() {
        let d = tempfile::tempdir().unwrap();
        let c = ctx(&d).await;
        // 空池不许开总开关（v3 的 400 分支）
        assert!(matches!(
            set_enabled(&c, true).await,
            Err(UpstreamError::PoolEmpty)
        ));
        let p = prober_ok("198.51.100.7");
        let a = add(&c, p, "http://user1:pw1@isp1.example.net:10007")
            .await
            .unwrap();
        set_mode(&c, true).await.unwrap();
        assert_eq!(
            rstate::group_of(&*c.store.read().await).mode,
            ResiMode::Global
        );
        set_keywords(
            &c,
            Some(vec!["openai.com".into(), " ".into(), "openai.com".into()]),
        )
        .await
        .unwrap();
        assert_eq!(
            rstate::group_of(&*c.store.read().await).keywords,
            Some(vec!["openai.com".to_string()]),
            "去空白、去重"
        );
        // None = 回到跟随默认表（R12：不把当时的默认表固化成自定义）
        set_keywords(&c, None).await.unwrap();
        assert_eq!(rstate::group_of(&*c.store.read().await).keywords, None);
        set_priority(&c, a.id, 5).await.unwrap();
        assert_eq!(
            rstate::group_of(&*c.store.read().await).upstreams[0].priority,
            5
        );
        set_enabled(&c, false).await.unwrap();
        assert!(!rstate::group_of(&*c.store.read().await).enabled);
    }

    // ───────────────────────────────────────────────────────────────────────────────
    // R3 ①（2026-09-12 bwg-rick）：池里 5 条中的 2 条无任何日志地从 state.json 消失。
    // 下面这几条把当时真机上并发跑着的那些路径（异步体检收尾、手动 select、巡检轮、
    // relay 重启触发的重放、录入）与增删交错起来跑，锁住「池子只会因 remove 变短」。
    // ───────────────────────────────────────────────────────────────────────────────

    /// 池内成员数与 `host:port` 集合（断言用）
    async fn pool(c: &DaemonCtx) -> Vec<String> {
        let mut v: Vec<String> = rstate::group_of(&*c.store.read().await)
            .upstreams
            .iter()
            .map(|u| format!("{}:{}", u.host, u.port))
            .collect();
        v.sort();
        v
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_adds_never_lose_an_upstream() {
        let d = tempfile::tempdir().unwrap();
        let c = ctx(&d).await;
        let p = prober_ok("198.51.100.7");
        let mut hs = Vec::new();
        for i in 1..=5u16 {
            let (c2, p2) = (c.clone(), p.clone());
            hs.push(tokio::spawn(async move {
                add(
                    &c2,
                    p2,
                    &format!("socks5://u:pw@isp{i}.example.net:{}", 1080 + i),
                )
                .await
            }));
        }
        for h in hs {
            h.await.unwrap().unwrap();
        }
        let g = rstate::group_of(&*c.store.read().await);
        assert_eq!(
            g.upstreams.len(),
            5,
            "5 次并发录入不能互相覆盖：{:?}",
            pool(&c).await
        );
        assert_eq!(
            g.upstreams
                .iter()
                .map(|u| u.name.clone())
                .collect::<Vec<_>>(),
            vec!["url-1", "url-2", "url-3", "url-4", "url-5"],
            "名字必须稠密重排"
        );
        let ids: std::collections::BTreeSet<Uuid> = g.upstreams.iter().map(|u| u.id).collect();
        assert_eq!(ids.len(), 5, "id 不能重复");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn re_adding_the_same_endpoint_concurrently_keeps_one_entry_and_drops_no_other() {
        let d = tempfile::tempdir().unwrap();
        let c = ctx(&d).await;
        let p = prober_ok("198.51.100.7");
        add(&c, p.clone(), "socks5://u:pw@keep1.example.net:1081")
            .await
            .unwrap();
        add(&c, p.clone(), "socks5://u:pw@keep2.example.net:1082")
            .await
            .unwrap();
        // 同一个 host:port 连续三次录入（面板双击 / CLI 重跑）：覆盖语义 ⇒ 只留一条，
        // 而且**绝不能**顺带带走别的成员
        let mut hs = Vec::new();
        for _ in 0..3 {
            let (c2, p2) = (c.clone(), p.clone());
            hs.push(tokio::spawn(async move {
                add(&c2, p2, "socks5://u:pw2@dup.example.net:1083").await
            }));
        }
        for h in hs {
            h.await.unwrap().unwrap();
        }
        assert_eq!(
            pool(&c).await,
            vec![
                "dup.example.net:1083".to_string(),
                "keep1.example.net:1081".to_string(),
                "keep2.example.net:1082".to_string()
            ]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_add_a_remove_and_a_finishing_check_only_drop_the_removed_member() {
        let d = tempfile::tempdir().unwrap();
        let c = ctx(&d).await;
        let p = prober_ok("198.51.100.7");
        for i in 1..=3u16 {
            add(
                &c,
                p.clone(),
                &format!("socks5://u:pw@isp{i}.example.net:{}", 1080 + i),
            )
            .await
            .unwrap();
        }
        let g = rstate::group_of(&*c.store.read().await);
        let (first, second) = (g.upstreams[0].id, g.upstreams[1].id);
        // 三件事同时在跑：异步体检收尾（run_and_store 写回 verified/ports_allowed）、
        // 删一条、再录一条
        let (c1, c2, c3) = (c.clone(), c.clone(), c.clone());
        let (p1, p3) = (p.clone(), p.clone());
        let check = tokio::spawn(async move {
            crate::modules::residential::check::run_and_store(&c1, p1, second).await
        });
        let rm = tokio::spawn(async move { remove(&c2, &UpstreamSel::Id(first)).await });
        let added =
            tokio::spawn(async move { add(&c3, p3, "socks5://u:pw@isp4.example.net:1084").await });
        let _ = check.await.unwrap();
        rm.await.unwrap().unwrap();
        added.await.unwrap().unwrap();
        assert_eq!(
            pool(&c).await,
            vec![
                "isp2.example.net:1082".to_string(),
                "isp3.example.net:1083".to_string(),
                "isp4.example.net:1084".to_string()
            ],
            "只有被 remove 的那条可以消失"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_health_round_a_manual_select_and_a_replay_never_shrink_the_pool() {
        use crate::api::Event;
        use crate::modules::residential::clash::FakeClash;
        use crate::modules::residential::health;
        let d = tempfile::tempdir().unwrap();
        let c = ctx(&d).await;
        let p = prober_ok("198.51.100.7");
        for i in 1..=2u16 {
            add(
                &c,
                p.clone(),
                &format!("socks5://u:pw@isp{i}.example.net:{}", 1080 + i),
            )
            .await
            .unwrap();
        }
        let before = pool(&c).await;
        let clash = std::sync::Arc::new(FakeClash::new(Some("resi-1")));
        // replay_loop 必须先 subscribe 再 spawn（broadcast 丢弃无订阅者时的事件）
        let rx = c.bus.subscribe();
        let replay = tokio::spawn(health::replay_loop(c.clone(), clash.clone(), rx));
        let ids = health::ids_of(&rstate::group_of(&*c.store.read().await));
        let (c1, c2, c3) = (c.clone(), c.clone(), c.clone());
        let (p1, cl1, cl2) = (p.clone(), clash.clone(), clash.clone());
        let round = tokio::spawn(async move { health::check_round(&c1, p1, cl1, ids).await });
        let sel_id = rstate::group_of(&*c.store.read().await).upstreams[1].id;
        let select = tokio::spawn(async move { health::select_manual(&c2, cl2, sel_id).await });
        // relay 重启多次：每次都触发一轮重放
        for _ in 0..3 {
            c.bus.send(Event::RelayRestarted);
        }
        let add3 = tokio::spawn(async move {
            add(
                &c3,
                prober_ok("198.51.100.9"),
                "socks5://u:pw@isp3.example.net:1083",
            )
            .await
        });
        round.await.unwrap().unwrap();
        select.await.unwrap().unwrap();
        add3.await.unwrap().unwrap();
        // 给 replay_loop 一次调度机会后收工
        tokio::task::yield_now().await;
        replay.abort();
        let after = pool(&c).await;
        for e in &before {
            assert!(
                after.contains(e),
                "巡检/手动切换/重放把 {e} 弄丢了：{after:?}"
            );
        }
        assert_eq!(after.len(), 3, "只多了刚录入的那条：{after:?}");
    }

    /// 防线本身的回归：住宅段的任何**非 remove** 调用点都不许让池变短（R3 ①）。
    #[tokio::test]
    async fn an_unlabelled_group_update_cannot_shrink_the_pool() {
        let d = tempfile::tempdir().unwrap();
        let c = ctx(&d).await;
        let p = prober_ok("198.51.100.7");
        add(&c, p.clone(), "socks5://u:pw@isp1.example.net:1081")
            .await
            .unwrap();
        add(&c, p, "socks5://u:pw@isp2.example.net:1082")
            .await
            .unwrap();
        let e = rstate::update_group(&c.store, &c.bus, |g| {
            g.upstreams.truncate(1); // 任何「顺手改住宅段」的代码都长这样
        })
        .await
        .expect_err("非 remove 调用点缩短上游池必须被拒");
        assert!(e.to_string().contains("upstream::remove"), "{e}");
        assert_eq!(pool(&c).await.len(), 2, "拒写之后池子原样");
        // 真正的删除路径照旧可用
        let id = rstate::group_of(&*c.store.read().await).upstreams[0].id;
        remove(&c, &UpstreamSel::Id(id)).await.unwrap();
        assert_eq!(pool(&c).await, vec!["isp2.example.net:1082".to_string()]);
    }
}
