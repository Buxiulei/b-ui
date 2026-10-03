//! 固定官方 sing-box 1.14.2 的受管配置编译器。
//!
//! 只消费同一授权快照，不探测网络、不迁移旧配置，也不产生能力证据。
//! `now` 只在编译时检查证据；已交付配置的到期撤权由服务端准入负责。
//! mixed 的 SOCKS UDP 会话不能逐目标重跑路由，故全部拒绝；UDP 业务通过 TUN。

use crate::managed::{allows, select_nodes, ManagedPolicy};
use crate::nodes::{Node, Transport};
use serde_json::{json, Value};
use std::net::IpAddr;

/// 调用方的运行参数；关闭 TUN 仅适用于已有捕获层或受控测试，不能宣称全接管。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedRuntime {
    /// macOS 传空串，让官方内核分配合法 utun 名称。
    pub interface_name: String,
    pub mixed_port: u16,
    pub enable_tun: bool,
}

/// 编译完整原生配置。节点必须已获授权，证据必须覆盖整个可选节点集合。
///
/// 域名节点仅使用调用方同一快照的 literal `dial_ip`，保留原 TLS SNI；
/// 单个 dial_ip 无法表示多个域名时整份配置拒绝。无节点或无有效能力时输出
/// 显式 block 默认与拒绝 DNS，防止内核补出隐式 DIRECT/local DNS。
pub fn compile(
    policy: &ManagedPolicy,
    nodes: &[Node],
    dial_ip: &str,
    runtime: &ManagedRuntime,
    now: i64,
) -> Value {
    let caps = &policy.capabilities;
    let permitted = [&caps.v4_tcp, &caps.v4_udp, &caps.v6_tcp, &caps.v6_udp]
        .map(|e| allows(e, &policy.path_fingerprint, now));
    let selected = select_nodes(nodes, policy.selected_identity);
    let endpoints = literal_endpoints(&selected, dial_ip);
    let open = !selected.is_empty() && endpoints.is_some() && permitted.iter().any(|v| *v);

    let mut outbounds = Vec::new();
    if open {
        let mut tags = Vec::new();
        for (i, (node, endpoint)) in selected.iter().zip(endpoints.unwrap()).enumerate() {
            let tag = format!("managed-node-{i}");
            outbounds.push(proxy_outbound(node, &endpoint, &tag));
            tags.push(tag);
        }
        outbounds.push(json!({
            "type":"selector", "tag":"managed-proxy", "outbounds":tags, "default":tags[0]
        }));
    } else {
        outbounds.push(json!({"type":"block", "tag":"managed-proxy"}));
    }

    // DNS detour 直接调用 outbound，先独立检查 resolver 的 TCP 族。
    let dns_allowed = open && (permitted[0] || permitted[2]);
    let dns_detour = if dns_allowed {
        // 同核心 SOCKS TCP 适配保留 DoH 响应的关闭边界；literal resolver 重新进入
        // managed-mixed 后仍走同一能力/身份路由，不增加 DIRECT 或业务放行规则。
        outbounds.push(json!({
            "type":"socks", "tag":"managed-dns-tcp-adapter", "server":"127.0.0.1",
            "server_port":runtime.mixed_port, "version":"5"
        }));
        "managed-dns-tcp-adapter"
    } else if !open {
        "managed-proxy"
    } else {
        outbounds.push(json!({"type":"block", "tag":"managed-dns-block"}));
        "managed-dns-block"
    };
    let dns_server = if dns_allowed && !permitted[0] {
        "2606:4700:4700::1111"
    } else {
        "1.1.1.1"
    };
    let dns_rules = if dns_allowed {
        json!([])
    } else {
        json!([{"action":"predefined", "rcode":"REFUSED"}])
    };

    let mut rules = vec![
        // 必须先于 DNS/sniff：mixed UDP 首包路由不能约束同会话后续目标。
        json!({"inbound":["managed-mixed"], "network":"udp", "action":"reject"}),
        json!({"network":["tcp","udp"], "port":53, "action":"hijack-dns"}),
        json!({"action":"sniff", "sniffer":["dns"]}),
        json!({"protocol":"dns", "action":"hijack-dns"}),
        json!({"ip_is_private":true, "action":"reject"}),
        json!({"network":"icmp", "action":"reject"}),
    ];
    if !open {
        rules.push(json!({"action":"reject"}));
    } else {
        for (i, family, network) in [(0, 4, "tcp"), (1, 4, "udp"), (2, 6, "tcp"), (3, 6, "udp")] {
            if !permitted[i] {
                rules.push(json!({"ip_version":family, "network":network, "action":"reject"}));
            }
        }
        for (network, v4, v6) in [
            ("tcp", permitted[0], permitted[2]),
            ("udp", permitted[1], permitted[3]),
        ] {
            let strategy = match (v4, v6) {
                (true, true) => "prefer_ipv4",
                (true, false) => "ipv4_only",
                (false, true) => "ipv6_only",
                (false, false) => {
                    rules.push(json!({"network":network, "action":"reject"}));
                    continue;
                }
            };
            rules.push(json!({"network":network, "action":"resolve", "server":"business-dns", "strategy":strategy}));
        }
        // resolve 只填 DestinationAddresses，不填 IPVersion；还须拦截异常混族 DNS 答复。
        for (i, network, cidr) in [
            (0, "tcp", "0.0.0.0/0"),
            (1, "udp", "0.0.0.0/0"),
            (2, "tcp", "::/0"),
            (3, "udp", "::/0"),
        ] {
            if !permitted[i] {
                rules.push(json!({"network":network, "ip_cidr":[cidr], "action":"reject"}));
            }
        }
        rules.push(json!({"ip_is_private":true, "action":"reject"}));
    }

    let mut inbounds = vec![json!({
        "type":"mixed", "tag":"managed-mixed", "listen":"127.0.0.1", "listen_port":runtime.mixed_port
    })];
    if runtime.enable_tun {
        inbounds.push(json!({
            "type":"tun", "tag":"managed-tun", "interface_name":runtime.interface_name,
            "address":["172.19.0.1/30", "fdfe:dcba:9876::1/126"],
            "auto_route":true, "strict_route":true, "dns_mode":"hijack",
            "udp_mapping":"address_and_port_dependent"
        }));
    }
    json!({
        "log":{"level":"info", "timestamp":true},
        "inbounds":inbounds,
        "outbounds":outbounds,
        "dns":{
            "servers":[{
                "type":"https", "tag":"business-dns", "server":dns_server,
                "server_port":443, "path":"/dns-query", "detour":dns_detour,
                "tls":{"enabled":true, "server_name":"cloudflare-dns.com", "insecure":false}
            }],
            "rules":dns_rules, "final":"business-dns"
        },
        "route":{
            "rules":rules, "final":"managed-proxy", "auto_detect_interface":true,
            "default_domain_resolver":"business-dns"
        }
    })
}

/// 不创建 bootstrap 网络路径；全部域名必须属于同一快照主机。
fn literal_endpoints(nodes: &[Node], dial_ip: &str) -> Option<Vec<String>> {
    let mut domain = None;
    nodes
        .iter()
        .map(|node| {
            if let Ok(ip) = node.host.parse::<IpAddr>() {
                return Some(ip.to_string());
            }
            match url::Host::parse(&node.host).ok()? {
                url::Host::Domain(host) if !host.is_empty() => {}
                _ => return None,
            }
            if domain.is_some_and(|host| host != node.host) {
                return None;
            }
            domain = Some(node.host.as_str());
            dial_ip.parse::<IpAddr>().ok().map(|ip| ip.to_string())
        })
        .collect()
}

fn proxy_outbound(node: &Node, endpoint: &str, tag: &str) -> Value {
    match &node.transport {
        Transport::Hysteria2 {
            username,
            password,
            sni,
            obfs_password,
        } => {
            let mut outbound = json!({
                "type":"hysteria2", "tag":tag, "server":endpoint, "server_port":node.port,
                "connect_timeout":"2s", "password":format!("{username}:{password}"),
                "tls":{"enabled":true, "server_name":sni, "insecure":false}
            });
            if let Some((start, end)) = node.hop {
                outbound["server_ports"] = json!([format!("{start}:{end}")]);
                outbound["hop_interval"] = json!("30s");
            }
            if let Some(password) = obfs_password {
                outbound["obfs"] = json!({"type":"salamander", "password":password});
            }
            outbound
        }
        Transport::Reality {
            uuid,
            public_key,
            short_id,
            server_name,
            fingerprint,
            flow,
        } => json!({
            "type":"vless", "tag":tag, "server":endpoint, "server_port":node.port,
            "connect_timeout":"2s", "uuid":uuid, "flow":flow,
            "tls":{
                "enabled":true, "server_name":server_name,
                "utls":{"enabled":true, "fingerprint":fingerprint},
                "reality":{"enabled":true, "public_key":public_key, "short_id":short_id}
            }
        }),
    }
}
