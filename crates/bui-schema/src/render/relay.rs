//! 本地出站中继 `b-ui-relay`（sing-box）的配置渲染。
//!
//! 住宅 HY2 凭据门与 xray 的 `vless-residential` 都把流量交给每槽的 `127.0.0.1:2080+i`，
//! 所以这份配置是住宅出口的唯一决策点。它不携带账户身份，不能授权 VPS 直连：
//!
//! - 池无效（`enabled=false` 或没有上游）→ 所有住宅业务明确拒绝；
//! - 池有效 → selector → 每上游回环策略端点 → 稳定 UUID 的真实出口。
//!   巡检经 Clash API 热切换时，新连接的规则与实际出口一起切换，已有连接保持。
use crate::model::{ResidentialGroup, Rule, Slot, Upstream, UpstreamKind};
use crate::relay_policy;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

pub mod generation;

/// 渲染 relay 配置需要的本机参数。
#[derive(Debug, Clone, PartialEq)]
pub struct RelayOpts {
    /// slot socks 入站基准端口（生产固定 2080）
    pub listen_port: u16,
    /// Clash API 监听地址（巡检热切换用）
    pub api: String,
    /// sing-box cache_file 路径
    pub cache_path: String,
    /// 本机公网 IP（拒绝住宅业务访问本机，探测不到时为 `None`）
    pub server_ip: Option<String>,
}

/// 私网与本机回环：拒绝住宅业务访问；基础设施拨号不经过这些入站规则。
/// 与 `server/residential-helper.sh` 顶部的 `PRIVATE_CIDRS` 同值。
const PRIVATE_CIDRS: &[&str] = &[
    "127.0.0.0/8",
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "169.254.0.0/16",
    "::1/128",
    "fc00::/7",
    "fe80::/10",
];

/// 全局 selector 的 tag：`dns_resi` 的 detour 与 global 模式的 `route.final` 用它，
/// 巡检既有的「全局最优」逻辑（手动锁定 / 更优候选防抖）也驱动它（D8）。
pub const POOL: &str = "resi-pool";

/// relay 的结构哈希：只排除全局 `resi-pool` selector 的 `default`。
///
/// 当前选择经 Clash API 生效；保存它只更新下次启动的落点，不应中断现有流。
/// 槽 selector 的 default、池成员、端点凭据、出口策略、嗅探与 DNS 都属于结构，
/// 任何变化仍需激活。调用方只在结构成功激活后确认这个 key，不能以写盘替代激活。
pub fn structural_hash(cfg: &Value) -> String {
    let mut structure = cfg.clone();
    if let Some(outbounds) = structure.get_mut("outbounds").and_then(Value::as_array_mut) {
        for outbound in outbounds {
            if outbound["type"] == "selector" && outbound["tag"] == POOL {
                if let Some(fields) = outbound.as_object_mut() {
                    fields.remove("default");
                }
            }
        }
    }
    let bytes = serde_json::to_vec(&structure).expect("Value 序列化不会失败");
    hex::encode(Sha256::digest(bytes))
}

/// 槽 `i` 的 socks 入站 tag。
fn inbound_tag(i: u16) -> String {
    format!("slot-{i}")
}

/// 槽 `i` 的 selector tag。
fn selector_tag(i: u16) -> String {
    format!("slot-{i}-pool")
}

/// 成员出站 tag：`resi-{下标+1}`，**与 v3 逐字相同**（`clash::tag_of` 依赖这条规则）。
fn member_tag(i: usize) -> String {
    format!("resi-{}", i + 1)
}

/// 仅使用精确的有效槽位；缺失或无效的绑定不能获得默认住宅出口。
fn slot_view(g: &ResidentialGroup, slots: &[Slot]) -> Vec<Slot> {
    let mut view: Vec<Slot> = slots
        .iter()
        .copied()
        .filter(|slot| slot.index < crate::slots::MAX_SLOTS)
        .filter(|slot| g.upstreams.iter().any(|u| u.id == slot.upstream_id))
        .filter(|slot| {
            slots
                .iter()
                .filter(|other| other.index == slot.index)
                .count()
                == 1
        })
        .collect();
    view.sort_by_key(|slot| slot.index);
    view
}

/// 每槽一个 socks 入站；视图为空时保留 2080 监听，以明确拒绝未绑定业务。
fn inbounds(view: &[Slot], base: u16) -> Vec<Value> {
    let idx: Vec<u16> = if view.is_empty() {
        vec![0]
    } else {
        view.iter().map(|s| s.index).collect()
    };
    idx.into_iter()
        .map(|i| {
            json!({
                "type": "socks",
                "tag": inbound_tag(i),
                "listen": "127.0.0.1",
                "listen_port": base + i
            })
        })
        .collect()
}

/// 渲染一份 relay 配置。
///
/// 调用者须先经 [`relay_policy::validate_group`] 校验池容量；生产由 Store 的读写边界
/// 保证此契约。超过支持范围应明确报错，不截断上游或静默 fail-open。
///
/// `slots` 是 `state.residential.slots`（spec §5.6）：每个槽在这里落成
/// 「一个 socks 入站 `slot-<i>`（端口 `opts.listen_port + i`）+ 一个 selector
/// `slot-<i>-pool`（成员本槽优先）+ 一条 `inbound → selector` 路由」。
/// 传空 slice ⇒ 只保留拒绝请求的 2080 监听，不合成授权槽位。
/// 无效绑定或重复序号的槽位不获得路由，避免默认住宅出口取代精确绑定。
pub fn config(g: &ResidentialGroup, slots: &[Slot], opts: &RelayOpts) -> Value {
    let active = g.pool_active();
    let selected = selected_upstream(g);
    let view = slot_view(g, slots);
    let pins = Blacklist::collect(g.blacklist.pins.iter().map(|pin| &pin.rule));
    let slot_inbounds: Vec<_> = if view.is_empty() {
        vec![inbound_tag(0)]
    } else {
        view.iter().map(|slot| inbound_tag(slot.index)).collect()
    };
    let mut ip_cidr: Vec<String> = PRIVATE_CIDRS.iter().map(|cidr| cidr.to_string()).collect();
    if let Some(ip) = opts
        .server_ip
        .as_ref()
        .and_then(|ip| ip.parse::<std::net::IpAddr>().ok())
    {
        ip_cidr.push(format!("{ip}/{}", if ip.is_ipv4() { 32 } else { 128 }));
    }
    let mut route_rules = vec![json!({ "inbound": slot_inbounds, "action": "sniff" })];
    if let Some(mut rule) = pins.domain_rule("action", "reject") {
        rule["inbound"] = json!(slot_inbounds);
        route_rules.push(rule);
    }
    if !pins.ports.is_empty() {
        route_rules
            .push(json!({ "inbound": slot_inbounds, "port": pins.ports, "action": "reject" }));
    }
    // HTTP suppliers cannot relay UDP. A capability gap is a rejection, never a
    // change of egress identity; this guard also covers application DNS on UDP/53.
    if !g.udp_via_pool() {
        route_rules.push(json!({ "inbound": slot_inbounds, "network": "udp", "action": "reject" }));
    }
    route_rules.push(json!({ "inbound": slot_inbounds, "ip_cidr": ip_cidr, "action": "reject" }));

    let mut dns_servers = Vec::new();
    if active {
        dns_servers.push(json!({
            "tag": "dns_resi", "type": "tcp", "server": "8.8.8.8", "detour": POOL
        }));
        for upstream in &g.upstreams {
            let inbound = json!([relay_policy::inbound_tag(upstream.id)]);
            let auto = Blacklist::collect(
                g.blacklist
                    .auto
                    .iter()
                    .filter(|entry| entry.upstream_id == upstream.id)
                    .map(|entry| &entry.rule),
            );
            if auto.has_domains() {
                route_rules.push(json!({
                    "type": "logical", "mode": "and",
                    "rules": [ { "inbound": inbound }, { "domain_regex": [".+"], "invert": true } ],
                    "action": "sniff"
                }));
            }
            if let Some(mut rule) = auto.domain_rule("action", "reject") {
                rule["inbound"] = inbound.clone();
                route_rules.push(rule);
            }
            if !auto.ports.is_empty() {
                route_rules
                    .push(json!({ "inbound": inbound, "port": auto.ports, "action": "reject" }));
            }
            if !g.udp_via_pool() {
                route_rules
                    .push(json!({ "inbound": inbound, "network": "udp", "action": "reject" }));
            } else {
                // Resolve business UDP through the same supplier selected for the
                // payload. Bootstrap resolution of the supplier itself is separate.
                let server = supplier_dns_tag(upstream.id);
                dns_servers.push(json!({ "tag": server, "type": "tcp", "server": "8.8.8.8",
                    "detour": relay_policy::egress_tag(upstream.id) }));
                route_rules.push(
                    json!({ "inbound": inbound, "network": "udp", "action": "resolve",
                    "server": server, "strategy": "ipv4_only" }),
                );
            }
            route_rules.push(json!({ "inbound": inbound, "ip_cidr": ip_cidr, "action": "reject" }));
            if let Some(allowed) = &upstream.ports_allowed {
                let ranges = inverted_port_ranges(allowed);
                if !ranges.is_empty() {
                    route_rules.push(
                        json!({ "inbound": inbound, "port_range": ranges, "action": "reject" }),
                    );
                }
            }
            route_rules.push(
                json!({ "inbound": inbound, "outbound": relay_policy::egress_tag(upstream.id) }),
            );
        }
        // The shared relay has no account identity. Every authorized residence
        // slot remains residence-only, independent of legacy client split keywords.
        for slot in &view {
            route_rules.push(json!({ "inbound": [inbound_tag(slot.index)], "outbound": selector_tag(slot.index) }));
        }
    }
    // This terminal decision also owns unknown/missing slots and unavailable pools.
    // Keeping an explicit bootstrap direct outbound never authorizes application use.
    route_rules.push(json!({ "action": "reject" }));
    dns_servers.push(json!({ "tag": "dns_direct", "type": "udp", "server": "1.1.1.1" }));
    let mut listeners = inbounds(&view, opts.listen_port);
    if active {
        listeners.extend(g.upstreams.iter().enumerate().map(|(index, upstream)| json!({
            "type": "socks", "tag": relay_policy::inbound_tag(upstream.id), "listen": "127.0.0.1",
            "listen_port": relay_policy::policy_port(index).expect("住宅上游池已校验不超过 MAX_SLOTS")
        })));
    }
    json!({
        "log": { "level": "error" },
        "dns": { "servers": dns_servers, "rules": [],
            "final": if active { "dns_resi" } else { "dns_direct" }, "strategy": "ipv4_only" },
        "inbounds": listeners,
        "outbounds": outbounds(g, active, selected, &view),
        "experimental": {
            "clash_api": { "external_controller": opts.api },
            "cache_file": { "enabled": true, "path": opts.cache_path }
        },
        "route": { "rules": route_rules, "final": if active { POOL } else { "direct" },
            "default_domain_resolver": if active { "dns_resi" } else { "dns_direct" } }
    })
}

/// Business resolver tied to the actual supplier rather than the global selector.
fn supplier_dns_tag(id: uuid::Uuid) -> String {
    format!("dns-resi-{id}")
}

/// 当前生效的上游：`selected_upstream_id` 指向的那个，指不到则退回第一个（与 selector 的 `default` 一致）。
fn selected_upstream(g: &ResidentialGroup) -> Option<&Upstream> {
    g.selected_upstream_id
        .and_then(|id| g.upstreams.iter().find(|u| u.id == id))
        .or_else(|| g.upstreams.first())
}

/// 出站表：回环成员、UUID 真实出口、槽/全局 selector 与基础设施 direct dialer。
/// `direct` 不在任何业务路由中；终止拒绝规则覆盖空池、失效池及未知入站。
fn outbounds(
    g: &ResidentialGroup,
    active: bool,
    selected: Option<&Upstream>,
    view: &[Slot],
) -> Vec<Value> {
    if !active {
        return vec![json!({ "type": "direct", "tag": "direct" })];
    }
    let tag = member_tag;
    let mut out: Vec<Value> = g.upstreams.iter().enumerate().map(|(i, _)| json!({
        "type": "socks",
        "tag": tag(i),
        "server": "127.0.0.1",
        "server_port": relay_policy::policy_port(i).expect("住宅上游池已校验不超过 MAX_SLOTS"),
        "version": "5"
    })).collect();
    out.extend(g.upstreams.iter().map(|u| {
        let mut o = json!({
            "tag": relay_policy::egress_tag(u.id),
            "server": u.host,
            "server_port": u.port,
            "username": u.username,
            "password": u.password,
            "domain_resolver": "dns_direct"
        });
        let m = o.as_object_mut().expect("json object");
        match u.kind {
            UpstreamKind::Http => {
                m.insert("type".into(), json!("http"));
            }
            UpstreamKind::Socks5 => {
                m.insert("type".into(), json!("socks"));
                m.insert("version".into(), json!("5"));
            }
        }
        o
    }));
    let tags: Vec<String> = (0..g.upstreams.len()).map(tag).collect();

    // 每槽一个 selector：成员顺序 = [本槽 IP, 其余按池内顺序]，default = 本槽
    // （spec §5.6）。巡检按槽驱动它（T5）：本槽健康就用本槽，否则借用排名最高的健康 IP。
    for s in view {
        let own = g
            .upstreams
            .iter()
            .position(|u| u.id == s.upstream_id)
            .map(member_tag)
            .expect("slot_view 已过滤掉不在池里的槽");
        let mut members = vec![own.clone()];
        members.extend(tags.iter().filter(|t| **t != own).cloned());
        out.push(json!({
            "type": "selector",
            "tag": selector_tag(s.index),
            "outbounds": members,
            "default": own,
            "interrupt_exist_connections": false
        }));
    }

    let default = selected
        .and_then(|s| g.upstreams.iter().position(|u| u.id == s.id))
        .map(tag)
        .unwrap_or_else(|| tags[0].clone());
    out.push(json!({
        "type": "selector",
        "tag": POOL,
        "outbounds": tags,
        "default": default,
        // 切换不掐已有连接（住宅抖动大，AI 登录场景高危）
        "interrupt_exist_connections": false
    }));
    out.push(json!({ "type": "direct", "tag": "direct" }));
    out
}

/// 同一作用域的黑名单；人工 pins 为公共作用域，auto 各自绑定上游 UUID。
#[derive(Debug, Default)]
struct Blacklist {
    suffixes: Vec<String>,
    domains: Vec<String>,
    ports: Vec<u16>,
}

impl Blacklist {
    fn collect<'a>(rules: impl IntoIterator<Item = &'a Rule>) -> Self {
        let mut bl = Self::default();
        for rule in rules {
            match rule {
                Rule::DomainSuffix(v) => bl.suffixes.push(v.clone()),
                Rule::Domain(v) => bl.domains.push(v.clone()),
                Rule::Port(v) => bl.ports.push(*v),
            }
        }
        bl
    }

    fn has_domains(&self) -> bool {
        !self.suffixes.is_empty() || !self.domains.is_empty()
    }

    /// 黑名单域名规则（`domain` / `domain_suffix` 在同一条规则里是 OR）。
    /// `key`/`value` 是终止字段；住宅限制一律使用 `action: reject`。
    fn domain_rule(&self, key: &str, value: &str) -> Option<Value> {
        if self.suffixes.is_empty() && self.domains.is_empty() {
            return None;
        }
        let mut m = Map::new();
        if !self.domains.is_empty() {
            m.insert("domain".into(), json!(self.domains));
        }
        if !self.suffixes.is_empty() {
            m.insert("domain_suffix".into(), json!(self.suffixes));
        }
        m.insert(key.into(), json!(value));
        Some(Value::Object(m))
    }
}

/// `ports_allowed` 取反成 `port_range` 列表（放行 [80,443] → 1:79 / 81:442 / 444:65535）。
/// 空列表当「不限」处理，返回空（否则会把全部端口都判成直连）。
fn inverted_port_ranges(allowed: &[u16]) -> Vec<String> {
    let mut ports: Vec<u32> = allowed
        .iter()
        .copied()
        .filter(|&p| p != 0)
        .map(u32::from)
        .collect();
    ports.sort_unstable();
    ports.dedup();
    if ports.is_empty() {
        return vec![];
    }
    let mut out = vec![];
    let mut cur = 1u32;
    for p in ports {
        if p > cur {
            out.push(format!("{}:{}", cur, p - 1));
        }
        cur = p + 1;
    }
    if cur <= 65535 {
        out.push(format!("{cur}:65535"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ResiMode;
    use uuid::Uuid;

    #[test]
    fn port_inversion_covers_edges() {
        assert_eq!(inverted_port_ranges(&[]), Vec::<String>::new());
        assert_eq!(
            inverted_port_ranges(&[80, 443]),
            ["1:79", "81:442", "444:65535"]
        );
        // 相邻端口不该产生空区间；重复与乱序要归一
        assert_eq!(
            inverted_port_ranges(&[443, 80, 443, 81]),
            ["1:79", "82:442", "444:65535"]
        );
        assert_eq!(inverted_port_ranges(&[1]), ["2:65535"]);
        assert_eq!(inverted_port_ranges(&[65535]), ["1:65534"]);
    }

    fn opts() -> RelayOpts {
        RelayOpts {
            listen_port: 2080,
            api: "127.0.0.1:9091".into(),
            cache_path: "/opt/b-ui/relay-cache.db".into(),
            server_ip: Some("203.0.113.10".into()),
        }
    }

    fn up(i: u16) -> Upstream {
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

    fn group(n: u16, mode: ResiMode) -> ResidentialGroup {
        ResidentialGroup {
            enabled: n > 0,
            mode,
            keywords: Some(vec!["openai".into(), "anthropic".into()]),
            upstreams: (0..n).map(up).collect(),
            selected_upstream_id: (n > 0).then(|| Uuid::from_u128(1)),
            blacklist: Default::default(),
        }
    }

    fn slots(indices: &[u16]) -> Vec<Slot> {
        indices
            .iter()
            .map(|i| Slot {
                index: *i,
                upstream_id: Uuid::from_u128(u128::from(*i) + 1),
            })
            .collect()
    }

    #[test]
    fn structural_hash_excludes_the_saved_global_selection_only() {
        let mut g = group(2, ResiMode::Global);
        let before = config(&g, &slots(&[0, 1]), &opts());
        g.selected_upstream_id = Some(g.upstreams[1].id);
        let after = config(&g, &slots(&[0, 1]), &opts());
        assert_ne!(before, after, "启动默认出口仍必须持久化");
        assert_eq!(structural_hash(&before), structural_hash(&after));

        let mut slot_changed = after.clone();
        let slot = slot_changed["outbounds"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|o| o["tag"] == "slot-0-pool")
            .unwrap();
        slot["default"] = json!("resi-2");
        assert_ne!(structural_hash(&after), structural_hash(&slot_changed));

        // 不能把所有 selector.default 或任何叫 resi-pool 的字段都当成运行期数据。
        let unusual = json!({"outbounds": [
            {"type": "selector", "tag": "another-pool", "default": "resi-1"},
            {"type": "socks", "tag": POOL, "default": "resi-1"}
        ]});
        for i in 0..2 {
            let mut changed = unusual.clone();
            changed["outbounds"][i]["default"] = json!("resi-2");
            assert_ne!(structural_hash(&unusual), structural_hash(&changed));
        }
    }

    #[test]
    fn structural_hash_retains_every_other_network_field() {
        let g = group(2, ResiMode::Split);
        let cfg = config(&g, &slots(&[0, 1]), &opts());
        let key = structural_hash(&cfg);
        // 真实渲染输出的关键结构：wrapper 端口、raw 凭据、slot 入站、sniff、DNS。
        // 每个路径必须已存在，避免测成「随便新增字段当然改变 hash」。
        for (pointer, value) in [
            ("/outbounds/0/server_port", json!(2280)),
            ("/outbounds/2/password", json!("rotated-placeholder")),
            ("/inbounds/0/listen_port", json!(2088)),
            ("/route/rules/0/action", json!("reject")),
            ("/dns/servers/0/server", json!("9.9.9.9")),
            ("/dns/servers/0/detour", json!("direct")),
        ] {
            let mut changed = cfg.clone();
            *changed.pointer_mut(pointer).expect("渲染字段必须存在") = value;
            assert_ne!(key, structural_hash(&changed), "不能排除 {pointer}");
        }
        let mut members_changed = cfg.clone();
        let pool = members_changed["outbounds"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|o| o["tag"] == POOL)
            .unwrap();
        pool["outbounds"] = json!(["resi-1"]);
        assert_ne!(key, structural_hash(&members_changed), "池成员属于结构");

        let mut policy_changed = g.clone();
        policy_changed.upstreams[1].ports_allowed = Some(vec![443]);
        assert_ne!(
            key,
            structural_hash(&config(&policy_changed, &slots(&[0, 1]), &opts())),
            "端口能力属于实际出口策略"
        );
        policy_changed = g;
        policy_changed.blacklist.auto.push(crate::model::AutoEntry {
            upstream_id: policy_changed.upstreams[1].id,
            rule: Rule::Domain("auto.example.com".into()),
            hits: 3,
            confirmed_at: "2026-09-29T00:00:00Z".into(),
            last_verified_at: "2026-09-29T00:00:00Z".into(),
            passes: 0,
        });
        assert_ne!(
            key,
            structural_hash(&config(&policy_changed, &slots(&[0, 1]), &opts())),
            "自动策略与它需要的嗅探仍属于结构"
        );
    }

    // 内部 policy 入站 / raw 出站之外，旧的入口与 selector 标签保持兼容。
    fn legacy_tags_of(cfg: &Value, key: &str) -> Vec<String> {
        cfg[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["tag"].as_str().unwrap().to_string())
            .filter(|tag| !tag.starts_with("resi-policy-") && !tag.starts_with("resi-egress-"))
            .collect()
    }

    /// 策略只能挂到实际出口的 policy inbound；不能在槽选择之前先用全局选中上游决定。
    #[test]
    fn policy_rules_are_bound_to_the_actual_upstream_and_not_global_selection() {
        use crate::model::{AutoEntry, Pin};
        let mut g = group(2, ResiMode::Global);
        g.upstreams[0].ports_allowed = Some(vec![80, 443]);
        g.upstreams[1].ports_allowed = Some(vec![8443]);
        for (i, domain) in [(0, "a.example.com"), (1, "b.example.com")] {
            g.blacklist.auto.push(AutoEntry {
                upstream_id: g.upstreams[i].id,
                rule: Rule::Domain(domain.into()),
                hits: 3,
                confirmed_at: "2026-09-29T00:00:00Z".into(),
                last_verified_at: "2026-09-29T00:00:00Z".into(),
                passes: 0,
            });
        }
        g.blacklist.pins.push(Pin {
            rule: Rule::DomainSuffix("pinned.example.com".into()),
            note: String::new(),
            created_at: "2026-09-29T00:00:00Z".into(),
        });
        let first = config(&g, &slots(&[0, 1]), &opts());
        g.selected_upstream_id = Some(g.upstreams[1].id);
        let second = config(&g, &slots(&[0, 1]), &opts());
        assert_eq!(
            first["route"], second["route"],
            "全局默认选中不应改变任何出口的策略"
        );
        assert_eq!(
            first["dns"], second["dns"],
            "上游 auto 不得污染全局 DNS 规则"
        );
        let rules = first["route"]["rules"].as_array().unwrap();
        for (i, domain, allowed) in [
            (0, "a.example.com", vec![80, 443]),
            (1, "b.example.com", vec![8443]),
        ] {
            let policy = format!("resi-policy-{}", g.upstreams[i].id);
            let domain_rule = rules
                .iter()
                .find(|r| r["domain"] == json!([domain]))
                .expect("每个上游的 auto 都必须渲染");
            assert_eq!(domain_rule["inbound"], json!([policy]));
            let ranges = json!(inverted_port_ranges(&allowed));
            let port_rule = rules.iter().find(|r| r["port_range"] == ranges).unwrap();
            assert_eq!(port_rule["inbound"], json!([policy]));
            let end = rules
                .iter()
                .find(|r| {
                    r["inbound"] == json!([policy])
                        && r.get("network").is_none()
                        && r["outbound"] == format!("resi-egress-{}", g.upstreams[i].id)
                })
                .expect("policy 必须终止到对应真实出口，不能回到 selector");
            assert!(end.get("domain").is_none() && end.get("port_range").is_none());
        }
        let pin = rules
            .iter()
            .find(|r| r["domain_suffix"] == json!(["pinned.example.com"]))
            .unwrap();
        assert_eq!(
            pin["inbound"],
            json!(["slot-0", "slot-1"]),
            "人工 pins 对所有用户入口生效"
        );
    }

    #[test]
    fn selector_members_follow_policy_endpoints_without_embedding_provider_credentials() {
        let g = group(2, ResiMode::Global);
        let cfg = config(&g, &slots(&[0, 1]), &opts());
        for (i, u) in g.upstreams.iter().enumerate() {
            let endpoint = cfg["inbounds"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["tag"] == format!("resi-policy-{}", u.id))
                .expect("缺少 policy inbound");
            assert_eq!(endpoint["listen"], "127.0.0.1");
            assert_eq!(endpoint["listen_port"], 2180 + i);
            let member = cfg["outbounds"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["tag"] == member_tag(i))
                .unwrap();
            assert_eq!(member["type"], "socks");
            assert_eq!(member["server"], "127.0.0.1");
            assert_eq!(member["server_port"], endpoint["listen_port"]);
            assert!(member.get("password").is_none());
            let raw = cfg["outbounds"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["tag"] == format!("resi-egress-{}", u.id))
                .expect("真实出口 UUID 必须稳定可归因");
            assert_eq!(raw["server"], u.host);
            assert_eq!(raw["password"], u.password);
        }
    }

    #[test]
    fn policy_reuses_domain_sniff_only_where_auto_rules_need_it() {
        let mut g = group(3, ResiMode::Global);
        for (i, rule) in [
            (0, Rule::DomainSuffix("auto.example.com".into())),
            (1, Rule::Port(5228)),
        ] {
            g.blacklist.auto.push(crate::model::AutoEntry {
                upstream_id: g.upstreams[i].id,
                rule,
                hits: 3,
                confirmed_at: "2026-09-29T00:00:00Z".into(),
                last_verified_at: "2026-09-29T00:00:00Z".into(),
                passes: 0,
            });
        }
        let cfg = config(&g, &slots(&[0, 1, 2]), &opts());
        assert_eq!(
            cfg["route"]["rules"][0],
            json!({
                "action": "sniff", "inbound": ["slot-0", "slot-1", "slot-2"]
            })
        );
        let inner: Vec<_> = cfg["route"]["rules"]
            .as_array()
            .unwrap()
            .iter()
            .skip(1)
            .filter(|r| r["action"] == "sniff")
            .collect();
        assert_eq!(inner.len(), 1, "只有带域名 auto 的出口需要第二次嗅探");
        assert_eq!(
            *inner[0],
            json!({
                "type": "logical", "mode": "and",
                "rules": [
                    { "inbound": [relay_policy::inbound_tag(g.upstreams[0].id)] },
                    { "domain_regex": [".+"], "invert": true }
                ],
                "action": "sniff"
            }),
            "已有目标域名时不得再次等待 server-first 首包"
        );
    }

    #[test]
    fn removing_an_upstream_does_not_rename_the_remaining_real_egress() {
        let mut g = group(2, ResiMode::Global);
        let id = g.upstreams[1].id;
        let before = config(&g, &slots(&[0, 1]), &opts());
        g.upstreams.remove(0);
        let after = config(&g, &slots(&[1]), &opts());
        let raw = |cfg: &Value| {
            cfg["outbounds"]
                .as_array()
                .unwrap()
                .iter()
                .find(|o| o["tag"] == relay_policy::egress_tag(id))
                .cloned()
                .unwrap()
        };
        assert_eq!(
            raw(&before),
            raw(&after),
            "raw UUID 出口必须跨池下标重排保持身份"
        );
        let member = |cfg: &Value, tag: &str| {
            cfg["outbounds"]
                .as_array()
                .unwrap()
                .iter()
                .find(|o| o["tag"] == tag)
                .cloned()
                .unwrap()
        };
        assert_eq!(member(&before, "resi-2")["server_port"], 2181);
        assert_eq!(member(&after, "resi-1")["server_port"], 2180);
    }

    #[test]
    fn a_single_slot_keeps_the_legacy_ingress_and_selector_contract() {
        // 槽 0 对外仍是 2080 入站与 slot-0-pool；内部增加 policy 端点。
        let g = group(1, ResiMode::Global);
        let cfg = config(&g, &slots(&[0]), &opts());
        assert_eq!(legacy_tags_of(&cfg, "inbounds"), vec!["slot-0"]);
        assert_eq!(cfg["inbounds"][0]["listen_port"], 2080);
        assert_eq!(cfg["inbounds"][0]["listen"], "127.0.0.1");
        assert_eq!(cfg["inbounds"][0]["type"], "socks");
        assert_eq!(
            legacy_tags_of(&cfg, "outbounds"),
            vec!["resi-1", "slot-0-pool", "resi-pool", "direct"]
        );
        assert_eq!(cfg["dns"]["servers"][0]["detour"], "resi-pool");
    }

    #[test]
    fn every_slot_gets_its_own_inbound_selector_and_route() {
        let g = group(3, ResiMode::Global);
        let cfg = config(&g, &slots(&[0, 1, 2]), &opts());
        assert_eq!(
            legacy_tags_of(&cfg, "inbounds"),
            vec!["slot-0", "slot-1", "slot-2"]
        );
        assert_eq!(
            cfg["inbounds"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|i| i["tag"].as_str().unwrap().starts_with("slot-"))
                .map(|i| i["listen_port"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            vec![2080, 2081, 2082]
        );
        assert_eq!(
            legacy_tags_of(&cfg, "outbounds"),
            vec![
                "resi-1",
                "resi-2",
                "resi-3",
                "slot-0-pool",
                "slot-1-pool",
                "slot-2-pool",
                "resi-pool",
                "direct"
            ]
        );
        let sel = |tag: &str| -> Value {
            cfg["outbounds"]
                .as_array()
                .unwrap()
                .iter()
                .find(|o| o["tag"] == tag)
                .cloned()
                .unwrap()
        };
        // 成员顺序 = 本槽优先，其余按池内顺序（spec §5.6「成员顺序 = [本槽 IP, 其余 IP…]」）
        assert_eq!(
            sel("slot-1-pool")["outbounds"],
            serde_json::json!(["resi-2", "resi-1", "resi-3"])
        );
        assert_eq!(sel("slot-1-pool")["default"], "resi-2");
        assert_eq!(sel("slot-1-pool")["type"], "selector");
        assert_eq!(
            sel("slot-1-pool")["interrupt_exist_connections"],
            false,
            "住宅抖动大，切换绝不许掐既有连接（spec §5.3）"
        );
        assert_eq!(
            sel("slot-2-pool")["outbounds"],
            serde_json::json!(["resi-3", "resi-1", "resi-2"])
        );
        // resi-pool 仍是全池顺序、default 跟 state 的落点
        assert_eq!(
            sel("resi-pool")["outbounds"],
            serde_json::json!(["resi-1", "resi-2", "resi-3"])
        );
        assert_eq!(sel("resi-pool")["default"], "resi-1");
        // global 模式：每槽一条「本槽入站 → 本槽 selector」，不带 domain_keyword
        let rules = cfg["route"]["rules"].as_array().unwrap();
        for i in 0..3u16 {
            let r = rules
                .iter()
                .find(|r| {
                    r["inbound"] == serde_json::json!([format!("slot-{i}")])
                        && r["outbound"] == format!("slot-{i}-pool")
                })
                .unwrap_or_else(|| panic!("缺 slot-{i} 的路由"));
            assert_eq!(r["outbound"], format!("slot-{i}-pool"));
            assert!(r.get("domain_keyword").is_none());
        }
    }

    #[test]
    fn legacy_split_keywords_cannot_change_the_shared_residential_exit() {
        let g = group(2, ResiMode::Split);
        let cfg = config(&g, &slots(&[0, 1]), &opts());
        let rules = cfg["route"]["rules"].as_array().unwrap();
        let r0 = rules
            .iter()
            .find(|r| {
                r["inbound"] == serde_json::json!(["slot-0"]) && r["outbound"] == "slot-0-pool"
            })
            .unwrap();
        assert!(r0.get("domain_keyword").is_none());
        assert_eq!(r0["outbound"], "slot-0-pool");
        assert_eq!(rules.last(), Some(&json!({"action":"reject"})));
        assert!(rules.iter().all(|rule| rule["outbound"] != "direct"));
    }

    /// 序号有空洞（删掉中间那条上游、新条目还没进来）时照样能渲染出自洽的配置。
    #[test]
    fn a_hole_in_the_index_space_still_renders_a_consistent_config() {
        let mut g = group(3, ResiMode::Global);
        g.upstreams.retain(|u| u.id != Uuid::from_u128(2));
        let cfg = config(&g, &slots(&[0, 2]), &opts());
        assert_eq!(legacy_tags_of(&cfg, "inbounds"), vec!["slot-0", "slot-2"]);
        assert_eq!(
            cfg["inbounds"][1]["listen_port"], 2082,
            "端口跟序号走，不跟位置走"
        );
        assert_eq!(
            legacy_tags_of(&cfg, "outbounds"),
            vec![
                "resi-1",
                "resi-2",
                "slot-0-pool",
                "slot-2-pool",
                "resi-pool",
                "direct"
            ]
        );
        // 槽 2 的 IP 是池里剩下的第二条（原 url-3），成员顺序本槽优先
        assert_eq!(
            cfg["outbounds"]
                .as_array()
                .unwrap()
                .iter()
                .find(|o| o["tag"] == "slot-2-pool")
                .unwrap()["outbounds"],
            serde_json::json!(["resi-2", "resi-1"])
        );
    }

    /// 不可用住宅保留入站，所有业务在终止拒绝规则处结束。
    #[test]
    fn an_inactive_pool_keeps_slot_listeners_and_rejects_all_business() {
        for (g, sl, want_inbounds, want_ports) in [
            (
                group(0, ResiMode::Global),
                vec![],
                vec!["slot-0"],
                vec![2080u64],
            ),
            (
                ResidentialGroup {
                    enabled: false,
                    ..group(2, ResiMode::Global)
                },
                slots(&[0, 1]),
                vec!["slot-0", "slot-1"],
                vec![2080, 2081],
            ),
        ] {
            let cfg = config(&g, &sl, &opts());
            assert_eq!(legacy_tags_of(&cfg, "inbounds"), want_inbounds);
            assert_eq!(
                cfg["inbounds"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|i| i["tag"].as_str().unwrap().starts_with("slot-"))
                    .map(|i| i["listen_port"].as_u64().unwrap())
                    .collect::<Vec<_>>(),
                want_ports
            );
            assert_eq!(legacy_tags_of(&cfg, "outbounds"), vec!["direct"]);
            let rules = cfg["route"]["rules"].as_array().unwrap();
            assert_eq!(rules.last(), Some(&json!({"action":"reject"})));
            assert!(rules.iter().all(|rule| rule.get("outbound").is_none()));
        }
    }

    /// 传进来的槽位指向已不在池里的上游（删上游与对账并发）时一律忽略，
    /// 绝不渲染出指向不存在出站的路由 —— 那会让 `sing-box check` 整份配置失败。
    #[test]
    fn slots_pointing_at_a_vanished_upstream_are_dropped() {
        let g = group(1, ResiMode::Global);
        let cfg = config(&g, &slots(&[0, 1]), &opts());
        assert_eq!(legacy_tags_of(&cfg, "inbounds"), vec!["slot-0"]);
        assert_eq!(
            legacy_tags_of(&cfg, "outbounds"),
            vec!["resi-1", "slot-0-pool", "resi-pool", "direct"]
        );
    }
}
