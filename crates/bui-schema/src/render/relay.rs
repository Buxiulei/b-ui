//! 本地出站中继 `b-ui-relay`（sing-box）的配置渲染。
//!
//! 移植来源：`server/residential-helper.sh` 的 `write_singbox_config_residential_multi`
//! （池模式）与 `write_singbox_config_direct`（直连模式），外加 spec §5.4 的黑名单与
//! 端口白名单两段规则。
//!
//! 两个 hysteria 住宅实例与 xray 的 `vless-residential` 都把流量交给 `127.0.0.1:2080`，
//! 所以这份配置是住宅分流的唯一决策点：
//!
//! - 池无效（`enabled=false` 或没有上游）→ 只有 `direct` 出站，全部直连（fail-open）；
//! - 池有效 → 每个上游一个出站 + `resi-pool` selector（巡检经 Clash API 热切换，不重启）。
use crate::model::{ResidentialGroup, Rule, Upstream, UpstreamKind};
use crate::render::SplitRules;
use serde_json::{json, Map, Value};

/// 渲染 relay 配置需要的本机参数。
#[derive(Debug, Clone, PartialEq)]
pub struct RelayOpts {
    /// socks 入站端口（固定 2080，两个数据面内核都指向它）
    pub listen_port: u16,
    /// Clash API 监听地址（巡检热切换用）
    pub api: String,
    /// sing-box cache_file 路径
    pub cache_path: String,
    /// 本机公网 IP（直连例外，探测不到时为 `None`）
    pub server_ip: Option<String>,
}

/// 私网与本机回环：一律直连，绝不经住宅上游。
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

/// 渲染一份 relay 配置。
pub fn config(g: &ResidentialGroup, opts: &RelayOpts) -> Value {
    let split = SplitRules::from_group(g);
    let active = split.enabled;
    let selected = selected_upstream(g);
    // 池无效时全部直连，黑名单与端口白名单都无意义
    let bl = if active {
        Blacklist::collect(g)
    } else {
        Blacklist::default()
    };

    let mut route_rules = vec![json!({ "action": "sniff" })];
    if let Some(r) = bl.domain_rule("outbound", "direct") {
        route_rules.push(r);
    }
    if !bl.ports.is_empty() {
        route_rules.push(json!({ "port": bl.ports, "outbound": "direct" }));
    }
    // 上游只放行部分端口时，其余端口直连（否则会在上游侧被硬拒，表现为连不上）
    if let Some(allowed) = selected
        .filter(|_| active)
        .and_then(|u| u.ports_allowed.as_deref())
    {
        let ranges = inverted_port_ranges(allowed);
        if !ranges.is_empty() {
            route_rules.push(json!({ "port_range": ranges, "outbound": "direct" }));
        }
    }
    // 住宅上游基本不支持 UDP ASSOCIATE：DNS 直连、QUIC 拒绝（浏览器回退 TCP 走住宅）、其余 UDP 直连
    route_rules.push(json!({ "network": "udp", "port": 53, "outbound": "direct" }));
    route_rules.push(json!({ "network": "udp", "port": 443, "action": "reject" }));
    route_rules.push(json!({ "network": "udp", "outbound": "direct" }));
    let mut ip_cidr: Vec<String> = PRIVATE_CIDRS.iter().map(|c| c.to_string()).collect();
    if let Some(ip) = &opts.server_ip {
        ip_cidr.push(format!("{ip}/32"));
    }
    route_rules.push(json!({ "ip_cidr": ip_cidr, "outbound": "direct" }));
    if active && !split.global {
        route_rules.push(json!({ "domain_keyword": split.keywords, "outbound": "resi-pool" }));
    }

    let mut dns_rules = vec![];
    if let Some(r) = bl.domain_rule("server", "dns_direct") {
        dns_rules.push(r);
    }
    if active && !split.global {
        dns_rules.push(json!({ "domain_keyword": split.keywords, "server": "dns_resi" }));
    }

    let mut dns_servers = vec![];
    if active {
        dns_servers.push(json!({
            "tag": "dns_resi", "type": "udp", "server": "8.8.8.8", "detour": "resi-pool"
        }));
    }
    dns_servers.push(json!({ "tag": "dns_direct", "type": "udp", "server": "1.1.1.1" }));

    json!({
        "log": { "level": "error" },
        "dns": {
            "servers": dns_servers,
            "rules": dns_rules,
            "final": if active && split.global { "dns_resi" } else { "dns_direct" },
            "strategy": "ipv4_only"
        },
        "inbounds": [{
            "type": "socks",
            "tag": "socks-in",
            "listen": "127.0.0.1",
            "listen_port": opts.listen_port
        }],
        "outbounds": outbounds(g, active, selected),
        "experimental": {
            "clash_api": { "external_controller": opts.api },
            "cache_file": { "enabled": true, "path": opts.cache_path }
        },
        "route": {
            "rules": route_rules,
            "final": if active && split.global { "resi-pool" } else { "direct" },
            "default_domain_resolver": "dns_direct"
        }
    })
}

/// 当前生效的上游：`selected_upstream_id` 指向的那个，指不到则退回第一个（与 selector 的 `default` 一致）。
fn selected_upstream(g: &ResidentialGroup) -> Option<&Upstream> {
    g.selected_upstream_id
        .and_then(|id| g.upstreams.iter().find(|u| u.id == id))
        .or_else(|| g.upstreams.first())
}

/// 出站表：`resi-1..N` + `resi-pool` selector + `direct`；池无效时只有 `direct`。
fn outbounds(g: &ResidentialGroup, active: bool, selected: Option<&Upstream>) -> Vec<Value> {
    if !active {
        return vec![json!({ "type": "direct", "tag": "direct" })];
    }
    let tag = |i: usize| format!("resi-{}", i + 1);
    let mut out: Vec<Value> = g
        .upstreams
        .iter()
        .enumerate()
        .map(|(i, u)| {
            let mut o = json!({
                "tag": tag(i),
                "server": u.host,
                "server_port": u.port,
                "username": u.username,
                "password": u.password
            });
            let m = o.as_object_mut().expect("json object");
            match u.kind {
                // http 上游是 sing-box 的 http 出站（无 version 字段、TCP only）
                UpstreamKind::Http => {
                    m.insert("type".into(), json!("http"));
                }
                UpstreamKind::Socks5 => {
                    m.insert("type".into(), json!("socks"));
                    m.insert("version".into(), json!("5"));
                }
            }
            o
        })
        .collect();
    let tags: Vec<String> = (0..g.upstreams.len()).map(tag).collect();
    let default = selected
        .and_then(|s| g.upstreams.iter().position(|u| u.id == s.id))
        .map(tag)
        .unwrap_or_else(|| tags[0].clone());
    out.push(json!({
        "type": "selector",
        "tag": "resi-pool",
        "outbounds": tags,
        "default": default,
        // 切换不掐已有连接（住宅抖动大，AI 登录场景高危）
        "interrupt_exist_connections": false
    }));
    out.push(json!({ "type": "direct", "tag": "direct" }));
    out
}

/// 生效的黑名单：`pins` ∪ `auto` 中属于当前选中上游的条目。
#[derive(Debug, Default)]
struct Blacklist {
    suffixes: Vec<String>,
    domains: Vec<String>,
    ports: Vec<u16>,
}

impl Blacklist {
    fn collect(g: &ResidentialGroup) -> Self {
        let selected = g.selected_upstream_id;
        let auto = g
            .blacklist
            .auto
            .iter()
            .filter(|e| Some(e.upstream_id) == selected)
            .map(|e| &e.rule);
        let mut bl = Self::default();
        for rule in g.blacklist.pins.iter().map(|p| &p.rule).chain(auto) {
            match rule {
                Rule::DomainSuffix(v) => bl.suffixes.push(v.clone()),
                Rule::Domain(v) => bl.domains.push(v.clone()),
                Rule::Port(v) => bl.ports.push(*v),
            }
        }
        bl
    }

    /// 黑名单域名规则（`domain` / `domain_suffix` 在同一条规则里是 OR）。
    /// `key`/`value` 是出口字段：路由规则用 `outbound: direct`，DNS 规则用 `server: dns_direct`。
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
}
