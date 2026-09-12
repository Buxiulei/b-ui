//! 出口体检（spec §5.2）：三方出口画像交叉分类、Google `/sorry/`、AI / 支付可达性、
//! 固定端口集与 SOCKS5 UDP ASSOCIATE，最后学出上游的 `ports_allowed`。
//!
//! 交叉判定逻辑只有 [`fold_sources`] **一份**：顺序版 [`probe_exit`]（给 T7 的「录入即探测」
//! 用）与并发版 [`run`] 都调它，避免两条路径给出不同的分类。

use super::proxy::{ConnectVerdict, HttpProbe, ProbeError, Prober};
use super::{port_probe_host, state, AI_HOSTS, BASE_PORTS, PAY_HOSTS, PROBE_PORTS};
use crate::reconcile::DaemonCtx;
use crate::util::fmt_rfc3339;
use bui_schema::model::{Upstream, Verified};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
use time::OffsetDateTime;
use uuid::Uuid;

/// 出口类型。`label()` 返回的**中文串**是面板契约（`web/app.js:1095` 用 `/IDC|机房/i`
/// 判色），不得改成英文枚举，否则「非住宅」告警永久失效。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitClass {
    Residential,
    Datacenter,
    Proxy,
    Mobile,
    Unknown,
}

impl ExitClass {
    pub fn label(&self) -> &'static str {
        // 这些中文串是面板契约（v3 `egress_ip_type`），不要改
        match self {
            ExitClass::Residential => "家庭宽带 IP",
            ExitClass::Datacenter => "IDC机房 IP",
            ExitClass::Proxy => "代理 IP",
            ExitClass::Mobile => "移动网络 IP",
            ExitClass::Unknown => "unknown",
        }
    }
}

/// 出口归属（IP 与 ASN / 组织 / 国家 / 城市）
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ExitInfo {
    pub ip: Option<String>,
    pub asn: Option<u32>,
    pub org: Option<String>,
    pub country: Option<String>,
    pub city: Option<String>,
}

/// 一次体检的完整结果（存 `runtime.checks[<upstream_id>]`，面板整体回显）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckReport {
    pub at: String,
    pub upstream_id: Uuid,
    pub class: ExitClass,
    /// 面板直接显示这一串（v3 `egress_ip_type` 的取值集合）
    pub class_label: String,
    pub exit: ExitInfo,
    /// 参与交叉的数据源与各自的判定（`ippure` / `ipquery` / `ipinfo`）
    pub sources: BTreeMap<String, String>,
    /// Google 搜索页是否落到 `/sorry/`；`None` = 没探到结论
    pub google_sorry: Option<bool>,
    /// 经该上游还能不能正常用 Google 搜索（浏览器 UA 的真实搜索请求，R2 ②）；
    /// `None` = 没探到结论。与 `google_sorry` 分开：那一项走普通 `get`（无 UA），
    /// 看不出 Bright Data 这类上游对 serp 域名的整域硬拒
    pub google_ok: Option<bool>,
    pub ai: BTreeMap<String, bool>,
    pub payments: BTreeMap<String, bool>,
    /// 端口 → `ConnectVerdict::label()`
    pub ports: BTreeMap<u16, String>,
    pub udp_associate: Option<bool>,
    /// 学到的端口白名单（`None` = 不限或学不到）
    pub ports_allowed: Option<Vec<u16>>,
    /// 本轮遇到 407 / SOCKS5 认证被拒 —— 结果不可信，不写 `ports_allowed`
    pub auth_failed: bool,
    pub notes: Vec<String>,
}

/// 三方出口画像的数据源（v3 `web/server.js:546-629` 同源、同顺序）
pub const EXIT_SOURCES: [(&str, &str); 3] = [
    ("ippure", "https://my.ippure.com/v1/info"),
    ("ipquery", "https://api.ipquery.io/?format=json"),
    ("ipinfo", "https://ipinfo.io/json"),
];
/// Google 搜索探测目标：命中 `/sorry/` 即被判为机器人
pub const GOOGLE_SEARCH_URL: &str = "https://www.google.com/search?q=hello";

/// Cloudflare 挑战页识别（spec §5.2：识别为「未知」而非失败）
pub fn is_cloudflare_challenge(p: &HttpProbe) -> bool {
    if p.status == 200 {
        return false;
    }
    let b = p.body.to_ascii_lowercase();
    [
        "just a moment",
        "cf-challenge",
        "attention required! | cloudflare",
        "cf-browser-verification",
    ]
    .iter()
    .any(|k| b.contains(k))
}

pub fn classify_ippure(v: &serde_json::Value) -> Option<(ExitClass, ExitInfo)> {
    let ip = v.get("ip")?.as_str()?.to_string();
    let class = match v.get("isResidential").and_then(serde_json::Value::as_bool) {
        Some(true) => ExitClass::Residential,
        Some(false) => ExitClass::Datacenter,
        None => ExitClass::Unknown, // 只剩归属字段时不猜类型
    };
    Some((
        class,
        ExitInfo {
            ip: Some(ip),
            asn: v.get("asn").and_then(as_asn),
            org: v
                .get("asOrganization")
                .and_then(|x| x.as_str())
                .map(str::to_string),
            country: v
                .get("country")
                .and_then(|x| x.as_str())
                .map(str::to_string),
            city: v.get("city").and_then(|x| x.as_str()).map(str::to_string),
        },
    ))
}

pub fn classify_ipquery(v: &serde_json::Value) -> Option<(ExitClass, ExitInfo)> {
    let ip = v.get("ip")?.as_str()?.to_string();
    let risk = v.get("risk").cloned().unwrap_or(serde_json::Value::Null);
    let b = |k: &str| risk.get(k).and_then(serde_json::Value::as_bool);
    let any_known = ["is_datacenter", "is_proxy", "is_vpn", "is_tor", "is_mobile"]
        .iter()
        .any(|k| b(k).is_some());
    // 判定顺序照 v3 `ipqueryEgress`：机房 > 代理/VPN/Tor > 移动 > 家宽
    let class = if !any_known {
        ExitClass::Unknown
    } else if b("is_datacenter") == Some(true) {
        ExitClass::Datacenter
    } else if [b("is_proxy"), b("is_vpn"), b("is_tor")].contains(&Some(true)) {
        ExitClass::Proxy
    } else if b("is_mobile") == Some(true) {
        ExitClass::Mobile
    } else {
        ExitClass::Residential
    };
    let isp = v.get("isp").cloned().unwrap_or(serde_json::Value::Null);
    let loc = v
        .get("location")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    Some((
        class,
        ExitInfo {
            ip: Some(ip),
            asn: isp.get("asn").and_then(as_asn),
            org: isp
                .get("org")
                .or_else(|| isp.get("isp"))
                .and_then(|x| x.as_str())
                .map(str::to_string),
            country: loc
                .get("country")
                .and_then(|x| x.as_str())
                .map(str::to_string),
            city: loc.get("city").and_then(|x| x.as_str()).map(str::to_string),
        },
    ))
}

pub fn classify_ipinfo(v: &serde_json::Value) -> Option<(ExitClass, ExitInfo)> {
    let ip = v.get("ip")?.as_str()?.to_string();
    let org = v.get("org").and_then(|x| x.as_str()).map(str::to_string);
    Some((
        ExitClass::Unknown, // 匿名层没有类型字段
        ExitInfo {
            ip: Some(ip),
            asn: org
                .as_deref()
                .and_then(|o| o.strip_prefix("AS"))
                .and_then(|o| o.split_whitespace().next().and_then(|n| n.parse().ok())),
            org,
            country: v
                .get("country")
                .and_then(|x| x.as_str())
                .map(str::to_string),
            city: v.get("city").and_then(|x| x.as_str()).map(str::to_string),
        },
    ))
}

/// `asn` 在 ippure 是数字、在 ipquery 是 `"AS33667"` 字符串
fn as_asn(v: &serde_json::Value) -> Option<u32> {
    v.as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .or_else(|| v.as_str()?.trim_start_matches("AS").parse().ok())
}

/// 交叉：多数票；平票取先出现的（数据源顺序 = 可信度顺序）；无票 `Unknown`
pub fn cross_class(votes: &[ExitClass]) -> ExitClass {
    let mut best = (ExitClass::Unknown, 0usize);
    for (i, v) in votes.iter().enumerate() {
        if *v == ExitClass::Unknown {
            continue; // Unknown 不投票
        }
        let n = votes.iter().skip(i).filter(|x| *x == v).count();
        if n > best.1 {
            best = (*v, n); // 平票时先出现的胜出（数据源顺序即可信度顺序）
        }
    }
    best.0
}

/// `ports` 表 →（`ports_allowed`, 是否可学）：全通 → `None`（不限）；
/// 有通有拒 → `Some(通的那些)`；遇到 407 / 探测抖动 → 不学（`None`）
pub fn derive_ports_allowed(ports: &BTreeMap<u16, String>, auth_failed: bool) -> Option<Vec<u16>> {
    if auth_failed || ports.is_empty() {
        return None;
    }
    if ports.values().any(|v| v == "unreachable") {
        return None; // 抖动不该学成白名单
    }
    let open: Vec<u16> = ports
        .iter()
        .filter(|(_, v)| *v == "open")
        .map(|(k, _)| *k)
        .collect();
    if open.len() == ports.len() {
        return None; // 全通 = 不限
    }
    // 连基准端口都不通 → 不是端口策略问题，别写空白名单（会把所有端口判成直连）
    if BASE_PORTS.iter().any(|p| !open.contains(p)) {
        return None;
    }
    Some(open)
}

/// **唯一**一份三源交叉逻辑：三个源各自的 GET 结果（`None` = 该项探测任务 panic 了）
/// → (分类, 归属, `sources` 表, 本轮是否遇到凭据失效, notes)。
/// [`probe_exit`]（顺序版）与 [`run`]（`fanout` 并发版）都调它，判定逻辑没有第二份。
pub fn fold_sources(
    results: &[Option<Result<HttpProbe, ProbeError>>],
) -> (
    ExitClass,
    ExitInfo,
    BTreeMap<String, String>,
    bool,
    Vec<String>,
) {
    let mut sources = BTreeMap::new();
    let mut votes = Vec::new();
    let mut exit = ExitInfo::default();
    let mut auth_failed = false;
    let mut notes = Vec::new();
    for (i, (name, _)) in EXIT_SOURCES.iter().enumerate() {
        let (class, detail) = match results.get(i).and_then(|x| x.as_ref()) {
            // fanout 的 None = 该项的阻塞任务 panic 了
            None => (ExitClass::Unknown, "probe_panicked".to_string()),
            Some(Err(ProbeError::AuthFailed)) => {
                auth_failed = true;
                (ExitClass::Unknown, "auth_failed".to_string())
            }
            Some(Err(e)) => (ExitClass::Unknown, format!("error:{e}")),
            Some(Ok(hp)) if is_cloudflare_challenge(hp) => {
                // spec §5.2：挑战页是「未知」，不是失败
                (ExitClass::Unknown, "cloudflare_challenge".to_string())
            }
            Some(Ok(hp)) => match serde_json::from_str::<serde_json::Value>(&hp.body) {
                Err(_) => (ExitClass::Unknown, format!("non_json:{}", hp.status)),
                Ok(v) => {
                    let parsed = match *name {
                        "ippure" => classify_ippure(&v),
                        "ipquery" => classify_ipquery(&v),
                        _ => classify_ipinfo(&v),
                    };
                    match parsed {
                        None => (ExitClass::Unknown, "no_ip".to_string()),
                        Some((c, e)) => {
                            // 第一个给出 IP 的源定归属（EXIT_SOURCES 顺序 = 可信度顺序）
                            if exit.ip.is_none() {
                                exit = e;
                            }
                            (c, c.label().to_string())
                        }
                    }
                }
            },
        };
        sources.insert((*name).to_string(), detail);
        votes.push(class);
    }
    if exit.ip.is_none() {
        notes.push("三个出口画像数据源都没给出 IP，出口分类未知".into());
    }
    (cross_class(&votes), exit, sources, auth_failed, notes)
}

/// 三源出口画像的**同步、顺序**版：给 T7 的 `add` 用（那条路径已经在 `spawn_blocking`
/// 里，且只需要这一项）。内部就是「三次 `p.get` → [`fold_sources`]」。
pub fn probe_exit(
    p: &dyn Prober,
    up: &Upstream,
) -> (ExitClass, ExitInfo, BTreeMap<String, String>, Vec<String>) {
    // 顺序跑（调用方已经在 spawn_blocking 里），结果交给与 `run` 同一个 fold_sources
    let results: Vec<Option<Result<HttpProbe, ProbeError>>> = EXIT_SOURCES
        .iter()
        .map(|(_, url)| Some(p.get(up, url)))
        .collect();
    let (class, exit, sources, _auth_failed, notes) = fold_sources(&results);
    (class, exit, sources, notes)
}

/// 搜索页 GET 结果 → 是否被判机器人（正文含 `/sorry/` 或状态码 429）；
/// `None` = 没探到结论（别把网络抖动显示成「被判机器人」）。
/// **唯一**一份 sorry 判定：[`run`] 用它读 `fanout` 里 [`GOOGLE_SEARCH_URL`] 那一项。
pub fn sorry_of(r: Option<&Result<HttpProbe, ProbeError>>) -> Option<bool> {
    match r {
        // 正文含 /sorry/ 或状态码 429 都算「被判机器人」（v3 同判据）
        Some(Ok(hp)) => Some(hp.body.contains("/sorry/") || hp.status == 429),
        // 探不到就不下结论：把抖动显示成「被判机器人」会误导运维换上游
        _ => None,
    }
}

/// [`ExitInfo`] → `Verified`（写回 `state` 的那个结构；`ip` 为 `None` 时返回 `None`）
pub fn to_verified(e: &ExitInfo, at: &str) -> Option<Verified> {
    // 没 IP 就不写：`verified` 的语义是「上次确认到的出口」，空 IP 毫无意义
    Some(Verified {
        ip: e.ip.clone()?,
        asn: e.asn,
        org: e.org.clone(),
        country: e.country.clone(),
        at: at.to_string(),
    })
}

/// 一次完整体检：三源出口 + Google sorry + AI + 支付 + 端口集 + UDP ASSOCIATE。
/// **全部**探测经 [`super::fanout`] 并发跑（上限 4），整个函数是 `async`（因为 `fanout` 是），
/// 每个探测项自身在 `spawn_blocking` 里。
pub async fn run(p: Arc<dyn Prober>, up: &Upstream, now: OffsetDateTime) -> CheckReport {
    let mut notes = Vec::new();
    let mut auth_failed = false;

    // ① 三源出口画像 + ② Google /sorry/：四个 GET，一起 fanout
    let mut urls: Vec<String> = EXIT_SOURCES.iter().map(|(_, u)| (*u).to_string()).collect();
    urls.push(GOOGLE_SEARCH_URL.to_string());
    let (pp, u2) = (p.clone(), up.clone());
    let gets = super::fanout(urls, move |url| pp.get(&u2, &url)).await;

    // 三源交叉：判定逻辑只有 `fold_sources` 一份（`probe_exit` 走的也是它）
    let (class, exit, sources, src_auth_failed, src_notes) = fold_sources(&gets[..3]);
    auth_failed |= src_auth_failed;
    notes.extend(src_notes);

    let google_sorry = sorry_of(gets[3].as_ref());
    if matches!(&gets[3], Some(Err(ProbeError::AuthFailed))) {
        auth_failed = true;
    }

    // ②b Google 可达性（R2 ②）：浏览器 UA 的真实搜索请求，判据与巡检同一份
    // （`proxy::google_ok_of`）。单独一次 `spawn_blocking`：它与上面四个 GET 的
    // 超时不同（GOOGLE_PROBE_TIMEOUT_SECS），塞进同一个 fanout 会混淆判据来源。
    let (pp, u2) = (p.clone(), up.clone());
    let google = tokio::task::spawn_blocking(move || pp.google_search(&u2)).await;
    let google_ok = match &google {
        Ok(r) => {
            if matches!(r, Err(ProbeError::AuthFailed)) {
                auth_failed = true;
            }
            super::proxy::google_ok_of(r)
        }
        // 探测任务 panic：没结论
        Err(_) => None,
    };

    // ③ AI 与支付可达性：CONNECT 到 443 即算可达（TLS 之后的业务码不是我们的判据）
    let hosts: Vec<String> = AI_HOSTS
        .iter()
        .chain(PAY_HOSTS.iter())
        .map(|h| (*h).to_string())
        .collect();
    let (pp, u2) = (p.clone(), up.clone());
    let hv = super::fanout(hosts.clone(), move |h| pp.connect(&u2, &h, 443)).await;
    let (mut ai, mut payments) = (BTreeMap::new(), BTreeMap::new());
    for (h, v) in hosts.iter().zip(&hv) {
        if *v == Some(ConnectVerdict::AuthFailed) {
            auth_failed = true;
        }
        let open = *v == Some(ConnectVerdict::Open);
        if AI_HOSTS.contains(&h.as_str()) {
            ai.insert(h.clone(), open);
        } else {
            payments.insert(h.clone(), open);
        }
    }

    // ④ 固定端口集：每个端口打 `super::port_probe_host` 给的目标——基准 80/443 打中性主机
    // `www.gstatic.com`（用搜索域名会被 Bright Data 整域硬拒，基准端口全判不通 ⇒
    // 白名单学不出来），固定端口集打各自真在该端口监听的主机
    let plist: Vec<u16> = BASE_PORTS
        .iter()
        .chain(PROBE_PORTS.iter())
        .copied()
        .collect();
    let (pp, u2) = (p.clone(), up.clone());
    let pv = super::fanout(plist.clone(), move |port| {
        pp.connect(&u2, port_probe_host(port), port)
    })
    .await;
    let mut ports = BTreeMap::new();
    for (port, v) in plist.iter().zip(&pv) {
        let label = match v {
            None => "unreachable".to_string(),
            Some(v) => {
                if *v == ConnectVerdict::AuthFailed {
                    auth_failed = true;
                }
                v.label()
            }
        };
        ports.insert(*port, label);
    }

    // ⑤ SOCKS5 UDP ASSOCIATE
    let (pp, u2) = (p.clone(), up.clone());
    let udp = tokio::task::spawn_blocking(move || pp.udp_associate(&u2)).await;
    let udp_associate = match udp {
        Ok(Ok(v)) => Some(v),
        Ok(Err(ProbeError::AuthFailed)) => {
            auth_failed = true;
            None
        }
        _ => None,
    };

    if auth_failed {
        notes.push("本轮出现 407 / SOCKS5 认证被拒：上游凭据可能已失效，端口白名单不予采信".into());
    }
    let ports_allowed = derive_ports_allowed(&ports, auth_failed);
    CheckReport {
        at: fmt_rfc3339(now),
        upstream_id: up.id,
        class,
        class_label: class.label().to_string(),
        exit,
        sources,
        google_sorry,
        google_ok,
        ai,
        payments,
        ports,
        udp_associate,
        ports_allowed,
        auth_failed,
        notes,
    }
}

/// 跑一次体检并把结果写进 state（`verified` / `ports_allowed`）与
/// `runtime.checks[<id>]`；期间 `runtime.checking` 为 `Some`（面板显示「检测中…」）
pub async fn run_and_store(
    ctx: &DaemonCtx,
    p: Arc<dyn Prober>,
    id: Uuid,
) -> anyhow::Result<CheckReport> {
    let s = ctx.store.read().await;
    let g = state::group_of(&s);
    let up = g
        .upstreams
        .iter()
        .find(|u| u.id == id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("上游不存在"))?;
    drop(s);
    let now = ctx.host.now();
    state::update(&ctx.runtime, |r| {
        r.checking = Some(state::Checking {
            upstream_id: id,
            started_at: fmt_rfc3339(now),
        });
    })
    .await;
    let report = run(p, &up, now).await;
    // 体检学到的两项写回 state（会触发一次对账；ports_allowed 变了 relay 规则就得改，本来就要重启）。
    // **写回口径与每日轮次完全同一份**：都走 `blacklist::port_learn`——`Learned` 才写
    // （含「全通 ⇒ None = 不限」这种真结论），`Keep` 一个字节都不动。手动体检以前是
    // 「只要没 407 就写 report.ports_allowed」，一次端口探测抖动（unreachable）就能把
    // 已学到的 `[80, 443]` 覆盖成 `None`，端口策略静默失效。
    let learn = super::blacklist::port_learn(&report.ports, report.auth_failed);
    let verified = to_verified(&report.exit, &report.at);
    state::update_group(&ctx.store, &ctx.bus, |g| {
        if let Some(u) = g.upstreams.iter_mut().find(|u| u.id == id) {
            if let super::blacklist::PortLearn::Learned(allowed) = learn {
                u.ports_allowed = allowed;
            }
            if let Some(v) = verified {
                u.verified = Some(v);
            }
        }
    })
    .await?;
    let json = serde_json::to_value(&report).unwrap_or(serde_json::Value::Null);
    state::update(&ctx.runtime, |r| {
        r.checks.insert(id.to_string(), json);
        r.checking = None;
        // 告警以 uuid 为键、文案用 host:port：`url-N` 是位置名，删掉一条上游后会被新
        // 条目复用，新上游就顶着上一个账号的 407 告警（2026-09-12 bwg-rick 的真机事故）
        if report.auth_failed {
            state::set_upstream_alert(
                r,
                id,
                format!("上游 {}:{} 凭据失效（407），请更新凭据", up.host, up.port),
            );
        } else {
            // 体检通过一次就消警：换完凭据不该还要人手动点
            state::clear_upstream_alert(r, id);
        }
    })
    .await;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::residential::proxy::{ConnectVerdict, FakeProber, HttpProbe};
    use bui_schema::model::UpstreamKind;
    use pretty_assertions::assert_eq;
    use time::macros::datetime;

    fn t0() -> OffsetDateTime {
        datetime!(2026-09-12 00:00:00 UTC)
    }

    fn up() -> Upstream {
        Upstream {
            id: Uuid::from_u128(1),
            name: "url-1".into(),
            kind: UpstreamKind::Http,
            host: "isp.example.net".into(),
            port: 10007,
            username: "user1".into(),
            password: "pw1".into(),
            priority: 10,
            provider: Some("decodo".into()),
            region: Some("US".into()),
            ports_allowed: None,
            verified: None,
        }
    }

    fn json(body: serde_json::Value) -> Result<HttpProbe, String> {
        Ok(HttpProbe {
            status: 200,
            body: body.to_string(),
        })
    }

    #[test]
    fn exit_class_labels_are_the_v3_chinese_strings() {
        // web/app.js:1095 用 /IDC|机房/i 判色，改成英文会让「非住宅」告警永久失效
        assert_eq!(ExitClass::Residential.label(), "家庭宽带 IP");
        assert_eq!(ExitClass::Datacenter.label(), "IDC机房 IP");
        assert_eq!(ExitClass::Proxy.label(), "代理 IP");
        assert_eq!(ExitClass::Mobile.label(), "移动网络 IP");
        assert_eq!(ExitClass::Unknown.label(), "unknown");
    }

    #[test]
    fn classifiers_read_each_providers_real_shape() {
        // ippure（2026-09-10 实测形状）
        let (c, e) = classify_ippure(&serde_json::json!({
            "ip": "198.51.100.7", "asn": 33667, "asOrganization": "Comcast",
            "country": "US", "city": "Denver", "fraudScore": 2, "isResidential": true
        }))
        .unwrap();
        assert_eq!(c, ExitClass::Residential);
        assert_eq!((e.asn, e.org.as_deref()), (Some(33667), Some("Comcast")));
        assert_eq!(
            classify_ippure(&serde_json::json!({"ip": "203.0.113.10", "isResidential": false}))
                .unwrap()
                .0,
            ExitClass::Datacenter
        );
        // isResidential 缺失 → 只有归属，类型未知（不能猜）
        assert_eq!(
            classify_ippure(&serde_json::json!({"ip": "203.0.113.10"}))
                .unwrap()
                .0,
            ExitClass::Unknown
        );
        // ipquery
        assert_eq!(
            classify_ipquery(&serde_json::json!({
                "ip": "198.51.100.7",
                "isp": {"asn": "AS33667", "org": "Comcast"},
                "location": {"country": "US", "city": "Denver"},
                "risk": {"is_datacenter": false, "is_vpn": false, "is_proxy": false, "is_mobile": false}
            }))
            .unwrap()
            .0,
            ExitClass::Residential
        );
        assert_eq!(
            classify_ipquery(&serde_json::json!({"ip": "1.2.3.4", "risk": {"is_mobile": true}}))
                .unwrap()
                .0,
            ExitClass::Mobile
        );
        assert_eq!(
            classify_ipquery(&serde_json::json!({"ip": "1.2.3.4", "risk": {"is_vpn": true}}))
                .unwrap()
                .0,
            ExitClass::Proxy
        );
        // ipinfo 只有归属
        let (c, e) = classify_ipinfo(&serde_json::json!({
            "ip": "198.51.100.7", "org": "AS33667 Comcast", "country": "US", "city": "Denver"
        }))
        .unwrap();
        assert_eq!(c, ExitClass::Unknown, "ipinfo 没有类型字段，只能记 unknown");
        assert_eq!(e.ip.as_deref(), Some("198.51.100.7"));
        assert_eq!(
            classify_ipinfo(&serde_json::json!({})),
            None,
            "没有 ip 就不算一票"
        );
    }

    #[test]
    fn cloudflare_challenge_is_unknown_not_failure() {
        // spec §5.2：识别 Cloudflare 挑战页为「未知」而非失败
        assert!(is_cloudflare_challenge(&HttpProbe {
            status: 403,
            body: "<title>Just a moment...</title><div id=\"cf-challenge-running\">".into()
        }));
        assert!(is_cloudflare_challenge(&HttpProbe {
            status: 503,
            body: "Attention Required! | Cloudflare".into()
        }));
        assert!(!is_cloudflare_challenge(&HttpProbe {
            status: 200,
            body: "{\"ip\":\"1.2.3.4\"}".into()
        }));
    }

    #[test]
    fn cross_class_takes_the_majority_then_source_order() {
        use ExitClass::*;
        assert_eq!(
            cross_class(&[Residential, Residential, Unknown]),
            Residential
        );
        assert_eq!(
            cross_class(&[Datacenter, Residential, Unknown]),
            Datacenter,
            "平票取先出现的（源顺序即可信度）"
        );
        assert_eq!(cross_class(&[Unknown, Unknown, Unknown]), Unknown);
        assert_eq!(cross_class(&[]), Unknown);
        assert_eq!(
            cross_class(&[Unknown, Residential]),
            Residential,
            "Unknown 不参与投票"
        );
    }

    #[test]
    fn ports_allowed_is_learned_only_from_a_clean_round() {
        let p = |pairs: &[(u16, &str)]| -> BTreeMap<u16, String> {
            pairs.iter().map(|(k, v)| (*k, v.to_string())).collect()
        };
        // 调研 §D 的 Decodo 实测：只放行 80/443，其余六个 403
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
        assert_eq!(derive_ports_allowed(&decodo, false), Some(vec![80, 443]));
        // 全通 → 不限
        let all_open: BTreeMap<u16, String> = [22u16, 80, 443, 853, 993, 5223, 5228, 8080]
            .iter()
            .map(|k| (*k, "open".into()))
            .collect();
        assert_eq!(derive_ports_allowed(&all_open, false), None);
        // 有一项探测失败 → 不学（宁可不限，别把抖动学成白名单）
        let mut flaky = decodo.clone();
        flaky.insert(993, "unreachable".into());
        assert_eq!(derive_ports_allowed(&flaky, false), None);
        // 凭据失效 → 不学
        assert_eq!(derive_ports_allowed(&decodo, true), None);
        // 连 80/443 都拒 → 不学（空白名单会让 relay 把所有端口判成直连，等于静默关住宅）
        let dead: BTreeMap<u16, String> = [22u16, 80, 443, 853, 993, 5223, 5228, 8080]
            .iter()
            .map(|k| (*k, "refused:403".into()))
            .collect();
        assert_eq!(derive_ports_allowed(&dead, false), None);
    }

    #[tokio::test]
    async fn full_check_reproduces_the_decodo_measurement() {
        let p = std::sync::Arc::new(FakeProber::new());
        p.with(|i| {
            i.gets.insert(
                EXIT_SOURCES[0].1.into(),
                json(serde_json::json!({
                    "ip": "198.51.100.7", "asn": 33667, "asOrganization": "Comcast",
                    "country": "US", "city": "Denver", "isResidential": true
                })),
            );
            i.gets.insert(
                EXIT_SOURCES[1].1.into(),
                json(serde_json::json!({
                    "ip": "198.51.100.7", "isp": {"asn": "AS33667", "org": "Comcast"},
                    "location": {"country": "US"}, "risk": {"is_datacenter": false}
                })),
            );
            i.gets.insert(
                GOOGLE_SEARCH_URL.into(),
                Ok(HttpProbe {
                    status: 200,
                    body: "<html>results".into(),
                }),
            );
            for h in AI_HOSTS.iter().chain(PAY_HOSTS.iter()) {
                i.connects.insert(format!("{h}:443"), ConnectVerdict::Open);
            }
            // 调研 §D：pay.google.com 与 www.paypal.com 在 Decodo 上 403
            i.connects.insert(
                "pay.google.com:443".into(),
                ConnectVerdict::Refused { code: 403 },
            );
            i.connects.insert(
                "www.paypal.com:443".into(),
                ConnectVerdict::Refused { code: 403 },
            );
            // 固定端口集打各自的真实主机（基准 80/443 打中性主机、缺省 Open）
            for port in PROBE_PORTS {
                i.connects.insert(
                    format!("{}:{port}", port_probe_host(port)),
                    ConnectVerdict::Refused { code: 403 },
                );
            }
            i.udp = true;
        });
        let r = run(p, &up(), t0()).await;
        assert_eq!(r.class, ExitClass::Residential);
        assert_eq!(r.class_label, "家庭宽带 IP");
        assert_eq!(r.exit.ip.as_deref(), Some("198.51.100.7"));
        assert_eq!(r.google_sorry, Some(false));
        assert!(r.ai["api.anthropic.com"], "AI 可达性");
        assert!(!r.payments["pay.google.com"], "调研 §D：Decodo 上 403");
        assert!(r.payments["checkout.stripe.com"]);
        assert_eq!(r.ports[&5228], "refused:403");
        assert_eq!(r.ports[&443], "open");
        assert_eq!(r.ports_allowed, Some(vec![80, 443]));
        assert_eq!(r.udp_associate, Some(true));
        assert!(!r.auth_failed);
        assert_eq!(r.at, "2026-09-12T00:00:00Z");
    }

    #[test]
    fn probe_exit_crosses_three_sources_in_one_pass_and_to_verified_needs_an_ip() {
        // T7 的 `add` 只吃这一条（60 秒预算装不下整轮体检），所以它必须独立可测
        let p = FakeProber::new();
        p.with(|i| {
            i.gets.insert(
                EXIT_SOURCES[0].1.into(),
                json(serde_json::json!({
                "ip": "198.51.100.7", "asn": 33667, "asOrganization": "Comcast",
                "country": "US", "city": "Denver", "isResidential": true})),
            );
            i.gets.insert(
                EXIT_SOURCES[1].1.into(),
                json(serde_json::json!({
                "ip": "198.51.100.7", "isp": {"asn": "AS33667", "org": "Comcast"},
                "location": {"country": "US"}, "risk": {"is_datacenter": false}})),
            );
        });
        let (class, exit, sources, notes) = probe_exit(&p, &up());
        assert_eq!(class, ExitClass::Residential);
        assert_eq!(exit.ip.as_deref(), Some("198.51.100.7"));
        assert_eq!(
            (exit.asn, exit.org.as_deref()),
            (Some(33667), Some("Comcast"))
        );
        assert!(
            sources["ipinfo"].starts_with("error:"),
            "第三个源没登记 ⇒ 记错误、不算票"
        );
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(
            p.calls(),
            vec![
                format!("get:{}", EXIT_SOURCES[0].1),
                format!("get:{}", EXIT_SOURCES[1].1),
                format!("get:{}", EXIT_SOURCES[2].1),
            ],
            "顺序版：三个源各一次，顺序照 EXIT_SOURCES（= 可信度顺序）"
        );
        let v = to_verified(&exit, "2026-09-12T00:00:00Z").unwrap();
        assert_eq!(
            (
                v.ip.as_str(),
                v.asn,
                v.org.as_deref(),
                v.country.as_deref(),
                v.at.as_str()
            ),
            (
                "198.51.100.7",
                Some(33667),
                Some("Comcast"),
                Some("US"),
                "2026-09-12T00:00:00Z"
            )
        );
        assert!(
            to_verified(&ExitInfo::default(), "2026-09-12T00:00:00Z").is_none(),
            "没 IP 就不写 verified（宁可空着等下次体检）"
        );
        // 三源全哑 ⇒ Unknown + 一条 note，不猜
        let (class2, exit2, _s2, notes2) = probe_exit(&FakeProber::new(), &up());
        assert_eq!(class2, ExitClass::Unknown);
        assert!(exit2.ip.is_none());
        assert_eq!(notes2.len(), 1, "{notes2:?}");
    }

    #[test]
    fn sorry_of_reads_the_search_page_and_stays_none_when_unknown() {
        // `run` 的第 ② 步就是把 fanout 里 GOOGLE_SEARCH_URL 那一项喂给它（唯一一份判定）
        assert_eq!(
            sorry_of(Some(&Ok(HttpProbe {
                status: 429,
                body: "<title>https://www.google.com/sorry/index".into(),
            }))),
            Some(true)
        );
        assert_eq!(
            sorry_of(Some(&Ok(HttpProbe {
                status: 200,
                body: "<html>results".into(),
            }))),
            Some(false)
        );
        assert_eq!(
            sorry_of(Some(&Err(ProbeError::Unreachable("no route".into())))),
            None,
            "探不到就不下结论"
        );
        assert_eq!(sorry_of(None), None, "探测任务 panic 也是「未知」");
    }

    #[tokio::test]
    async fn the_report_carries_the_google_verdict() {
        // R2 ②：体检要能回答「这条上游封不封 Google」——Bright Data 对 serp 域名
        // 整域 403，只看 google_sorry（走 `get`）看不出来
        let p = std::sync::Arc::new(FakeProber::new());
        p.with(|i| {
            i.google = Some(Ok(HttpProbe {
                status: 403,
                body: "403 Forbidden serp domain".into(),
            }))
        });
        let r = run(p.clone(), &up(), t0()).await;
        assert_eq!(r.google_ok, Some(false));
        assert!(
            p.calls().iter().any(|c| c == "google"),
            "体检必须真打一次搜索请求：{:?}",
            p.calls()
        );
        let p2 = std::sync::Arc::new(FakeProber::new());
        p2.with(|i| {
            i.google = Some(Ok(HttpProbe {
                status: 200,
                body: "<html>weather results".into(),
            }))
        });
        assert_eq!(run(p2, &up(), t0()).await.google_ok, Some(true));
        // 探不到就是未知（缺省的 FakeProber 不给 google）
        assert_eq!(
            run(std::sync::Arc::new(FakeProber::new()), &up(), t0())
                .await
                .google_ok,
            None
        );
    }

    #[tokio::test]
    async fn a_google_probe_that_fails_auth_marks_the_round_untrustworthy() {
        let p = std::sync::Arc::new(FakeProber::new());
        p.with(|i| i.google = Some(Err("__auth_failed__".into())));
        let r = run(p, &up(), t0()).await;
        assert!(r.auth_failed, "407 出现在哪一项都算整轮不可信");
        assert_eq!(r.google_ok, None);
        assert_eq!(r.ports_allowed, None);
    }

    #[tokio::test]
    async fn a_407_marks_the_whole_round_untrustworthy() {
        let p = std::sync::Arc::new(FakeProber::new());
        p.with(|i| {
            for port in PROBE_PORTS.iter().chain(BASE_PORTS.iter()) {
                i.connects.insert(
                    format!("{}:{port}", port_probe_host(*port)),
                    ConnectVerdict::AuthFailed,
                );
            }
        });
        let r = run(p, &up(), t0()).await;
        assert!(r.auth_failed, "调研 §D 的 407 场景");
        assert_eq!(r.ports_allowed, None, "凭据失效时绝不写端口白名单");
        assert!(
            r.notes.iter().any(|n| n.contains("凭据")),
            "notes 要指名凭据失效：{:?}",
            r.notes
        );
    }

    #[tokio::test]
    async fn google_sorry_is_detected_and_cloudflare_is_not_a_failure() {
        let p = std::sync::Arc::new(FakeProber::new());
        p.with(|i| {
            i.gets.insert(
                GOOGLE_SEARCH_URL.into(),
                Ok(HttpProbe {
                    status: 429,
                    body: "<title>https://www.google.com/sorry/index".into(),
                }),
            );
            i.gets.insert(
                EXIT_SOURCES[0].1.into(),
                Ok(HttpProbe {
                    status: 403,
                    body: "<title>Just a moment...</title>".into(),
                }),
            );
        });
        let r = run(p, &up(), t0()).await;
        assert_eq!(r.google_sorry, Some(true));
        assert_eq!(r.class, ExitClass::Unknown, "挑战页记未知，不是失败");
        assert_eq!(r.sources["ippure"], "cloudflare_challenge");
    }

    /// 调研 §D 的 Bright Data 形态：搜索域名**整域**硬拒（policy_20110，端口无关），
    /// 非白名单端口 403，白名单端口（80/443）放行
    struct SearchDomainBlocked;
    impl Prober for SearchDomainBlocked {
        fn connect(&self, _u: &Upstream, h: &str, port: u16) -> ConnectVerdict {
            if h == "www.google.com" {
                return ConnectVerdict::Refused { code: 403 };
            }
            if BASE_PORTS.contains(&port) {
                ConnectVerdict::Open
            } else {
                ConnectVerdict::Refused { code: 403 }
            }
        }
        fn get(&self, _u: &Upstream, _url: &str) -> Result<HttpProbe, ProbeError> {
            Err(ProbeError::Unreachable("no route".into()))
        }
        fn google_search(&self, _u: &Upstream) -> Result<HttpProbe, ProbeError> {
            // 整域硬拒：CONNECT 到 www.google.com 就 403，GET 自然也到不了
            Err(ProbeError::Unreachable("no route".into()))
        }
        fn udp_associate(&self, _u: &Upstream) -> Result<bool, ProbeError> {
            Ok(false)
        }
        fn direct_tcp(&self, _h: &str, _p: u16) -> bool {
            false
        }
    }

    #[tokio::test]
    async fn a_403_on_the_search_domain_no_longer_kills_the_port_whitelist() {
        // 端口集的基准端口打中性主机 www.gstatic.com，所以「整域拒搜索域名」的上游
        // 照样能学出 [80, 443]
        let r = run(std::sync::Arc::new(SearchDomainBlocked), &up(), t0()).await;
        assert_eq!(r.ports[&80], "open");
        assert_eq!(r.ports[&443], "open");
        assert_eq!(r.ports[&5228], "refused:403");
        assert_eq!(
            r.ports_allowed,
            Some(vec![80, 443]),
            "基准主机被 403 的上游不该把 ports_allowed 判成 None"
        );
        // 反证（回归前的行为）：若基准端口仍打 www.google.com，80/443 会跟着被判 403，
        // derive_ports_allowed 的「连基准端口都不通就别学」分支直接返回 None
        let all_refused: BTreeMap<u16, String> = BASE_PORTS
            .iter()
            .chain(PROBE_PORTS.iter())
            .map(|p| (*p, "refused:403".to_string()))
            .collect();
        assert_eq!(derive_ports_allowed(&all_refused, false), None);
    }

    async fn store_ctx(d: &tempfile::TempDir, allowed: Option<Vec<u16>>) -> DaemonCtx {
        use crate::api::EventBus;
        use crate::state::runtime::Runtime;
        use crate::state::store::Store;
        use crate::sys::fake::FakeHost;
        let mut s = crate::testutil::sample_state();
        let g = s
            .residential
            .groups
            .entry(super::super::GROUP_DEFAULT.to_string())
            .or_default();
        g.enabled = true;
        g.upstreams = vec![Upstream {
            ports_allowed: allowed,
            ..up()
        }];
        DaemonCtx {
            store: Store::create(d.path().join("state.json"), s).await.unwrap(),
            runtime: Runtime::load(d.path().join("runtime.json")),
            bus: EventBus::new(),
            host: std::sync::Arc::new(FakeHost::new()),
            paths: bui_schema::paths::Paths::default_server(),
        }
    }

    async fn stored_ports(ctx: &DaemonCtx) -> Option<Vec<u16>> {
        state::group_of(&*ctx.store.read().await).upstreams[0]
            .ports_allowed
            .clone()
    }

    #[tokio::test]
    async fn one_flaky_port_probe_never_overwrites_the_learned_whitelist() {
        // 写回口径与每日轮次同一份（blacklist::port_learn）：本轮有 unreachable ⇒ Keep
        let d = tempfile::tempdir().unwrap();
        let ctx = store_ctx(&d, Some(vec![80, 443])).await;
        let p = std::sync::Arc::new(FakeProber::new());
        p.with(|i| {
            i.connects.insert(
                format!("{}:993", port_probe_host(993)),
                ConnectVerdict::Unreachable {
                    detail: "timeout".into(),
                },
            );
        });
        let r = run_and_store(&ctx, p, up().id).await.unwrap();
        assert_eq!(r.ports_allowed, None, "本轮抖动 ⇒ 这一轮什么也没学到");
        assert_eq!(
            stored_ports(&ctx).await,
            Some(vec![80, 443]),
            "一次抖动不能把已学到的白名单覆盖成 None"
        );
    }

    #[tokio::test]
    async fn a_clean_manual_check_writes_the_whitelist_back() {
        let d = tempfile::tempdir().unwrap();
        let ctx = store_ctx(&d, None).await;
        // 干净一轮：固定端口集全 403、基准端口通 ⇒ 学成 [80, 443]
        let p = std::sync::Arc::new(FakeProber::new());
        p.with(|i| {
            for port in PROBE_PORTS {
                i.connects.insert(
                    format!("{}:{port}", port_probe_host(port)),
                    ConnectVerdict::Refused { code: 403 },
                );
            }
        });
        let r = run_and_store(&ctx, p, up().id).await.unwrap();
        assert_eq!(r.ports_allowed, Some(vec![80, 443]));
        assert_eq!(stored_ports(&ctx).await, Some(vec![80, 443]));
        // 下一轮全通 ⇒ 「不限」是**真结论**，照 PortLearn::Learned(None) 写回（清掉白名单），
        // 否则供应商放开端口策略后这条上游永远只走 80/443
        let r2 = run_and_store(&ctx, std::sync::Arc::new(FakeProber::new()), up().id)
            .await
            .unwrap();
        assert_eq!(r2.ports_allowed, None);
        assert_eq!(stored_ports(&ctx).await, None, "全通 ⇒ 不限");
    }

    #[tokio::test]
    async fn a_407_round_leaves_the_stored_whitelist_untouched() {
        let d = tempfile::tempdir().unwrap();
        let ctx = store_ctx(&d, Some(vec![80, 443])).await;
        let p = std::sync::Arc::new(FakeProber::new());
        p.with(|i| {
            for port in BASE_PORTS.iter().chain(PROBE_PORTS.iter()) {
                i.connects.insert(
                    format!("{}:{port}", port_probe_host(*port)),
                    ConnectVerdict::AuthFailed,
                );
            }
        });
        let r = run_and_store(&ctx, p, up().id).await.unwrap();
        assert!(r.auth_failed);
        assert_eq!(stored_ports(&ctx).await, Some(vec![80, 443]));
        let rt = state::read(&ctx.runtime).await;
        // 告警以 uuid 为键、文案用 host:port（url-N 是位置名，删条目后会被新条目复用）
        let msg = rt
            .upstream_alerts
            .get(&up().id)
            .cloned()
            .unwrap_or_else(|| panic!("{:?}", rt.upstream_alerts));
        assert!(msg.contains("凭据失效"), "{msg}");
        assert!(msg.contains("isp.example.net:10007"), "{msg}");
        assert!(!msg.contains("url-"), "{msg}");
        assert!(rt.checking.is_none());
        assert!(rt.checks.contains_key(&up().id.to_string()));

        // 换过凭据后体检通过一次 ⇒ 该上游的 407 告警自动消掉
        run_and_store(&ctx, std::sync::Arc::new(FakeProber::new()), up().id)
            .await
            .unwrap();
        assert!(
            state::read(&ctx.runtime).await.upstream_alerts.is_empty(),
            "体检成功一次即清"
        );
    }
}
