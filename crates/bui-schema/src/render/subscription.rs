//! 三种订阅渲染器：v2rayN base64 URI 列表、sing-box 完整配置、mihomo YAML。
//!
//! 移植来源（逐字段对照 v3）：
//! - URI 列表 `web/server.js:1806-1913`（`buildVlessUrl` / `buildHy2Url`）
//! - sing-box `web/server.js:647-849 generateSingboxConfig`
//! - mihomo `web/server.js:852-1034 generateClashConfig`
//!
//! 三者的节点集合都来自 [`nodes_for`](crate::nodes::nodes_for)，顺序即 v3 的
//! Reality直连 / Reality住宅 / HY2直连 / HY2住宅。

use base64::{engine::general_purpose::STANDARD, Engine as _};
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use serde_json::{json, Value};
use serde_yaml::{Mapping, Value as Yaml};

use super::SplitRules;
use crate::nodes::{Node, NodeKind, Transport};

/// 与 JS `encodeURIComponent` 相同的保留集：不编码 `A-Za-z0-9-_.!~*'()`。
const COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')');

fn enc(s: &str) -> String {
    utf8_percent_encode(s, COMPONENT).to_string()
}

/// sing-box 出站 tag（也是 clash 之外所有内部引用的名字）。
fn tag(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::RealityDirect => "vless-direct",
        NodeKind::RealityResidential => "vless-residential",
        NodeKind::Hy2Direct => "hy2-direct",
        NodeKind::Hy2Residential => "hy2-residential",
    }
}

fn is_resi(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::RealityResidential | NodeKind::Hy2Residential
    )
}

/// 生效的分流关键字：未启用的池不发布住宅分流目标；
/// 排序去重与 v3 的真源 `residential-helper.sh domains`（`jq -sc unique`）一致。
fn effective_keywords(split: &SplitRules) -> Vec<String> {
    if !split.enabled {
        return vec![];
    }
    let mut k = split.keywords.clone();
    k.sort();
    k.dedup();
    k
}

/// 一条已授权节点的规范 URI（v2rayN / bui-c 都吃这个格式）。
/// 调用方先由 [`crate::nodes::nodes_for`] 得到授权节点，面板与订阅共用此格式器。
pub fn node_uri(node: &Node, username: &str) -> String {
    let name = enc(&node.name(username));
    match &node.transport {
        Transport::Reality {
            uuid,
            public_key,
            short_id,
            server_name,
            fingerprint,
            flow,
        } => format!(
            "vless://{uuid}@{host}:{port}?security=reality&encryption=none&pbk={public_key}\
             &headerType=&fp={fingerprint}&spx=%2F&type=tcp&flow={flow}&sni={server_name}\
             &sid={short_id}#{name}",
            host = node.host,
            port = node.port,
        ),
        Transport::Hysteria2 {
            username: hy2_user,
            password,
            sni,
            obfs_password,
        } => {
            let mut q = format!("sni={sni}&insecure=0");
            if let Some((start, end)) = node.hop {
                q.push_str(&format!("&mport={start}-{end}"));
            }
            if let Some(pw) = obfs_password {
                q.push_str(&format!("&obfs=salamander&obfs-password={}", enc(pw)));
            }
            format!(
                "hysteria2://{}:{}@{}:{}?{q}#{name}",
                enc(hy2_user),
                enc(password),
                node.host,
                node.port,
            )
        }
    }
}

/// base64 的 URI 列表订阅（`/api/sub/<token>`，末段是 [`crate::sub`] 的随机订阅 token）。
pub fn uri_list(nodes: &[Node], username: &str) -> String {
    let lines: Vec<String> = nodes.iter().map(|n| node_uri(n, username)).collect();
    STANDARD.encode(lines.join("\n"))
}

/// 出站按解析出的公网 IPv4 连接、TLS SNI 仍用域名（`host` 是 IP 时不做替换）。
fn dial_host<'a>(host: &'a str, dial_ip: &'a str) -> &'a str {
    if host_is_domain(host) && !dial_ip.is_empty() {
        dial_ip
    } else {
        host
    }
}

/// 空串既不是域名也不是 IP：**零节点用户**（见 [`singbox`]）的 `host` 就是空串，当成域名
/// 会渲出 `dns.rules[0].domain: [""]`，而 sing-box 拒绝空 item ⇒ 整份配置加载失败。
fn host_is_domain(host: &str) -> bool {
    !host.is_empty() && host.parse::<std::net::Ipv4Addr>().is_err()
}

fn singbox_outbound(node: &Node, dial: &str) -> Value {
    match &node.transport {
        Transport::Reality {
            uuid,
            public_key,
            short_id,
            server_name,
            fingerprint,
            flow,
        } => json!({
            "type": "vless",
            "tag": tag(node.kind),
            "server": dial,
            "server_port": node.port,
            "connect_timeout": "2s",
            "uuid": uuid,
            "flow": flow,
            "tls": {
                "enabled": true,
                "server_name": server_name,
                "utls": { "enabled": true, "fingerprint": fingerprint },
                "reality": { "enabled": true, "public_key": public_key, "short_id": short_id }
            }
        }),
        Transport::Hysteria2 {
            username,
            password,
            sni,
            obfs_password,
        } => {
            let mut o = json!({
                "type": "hysteria2",
                "tag": tag(node.kind),
                "server": dial,
                "server_port": node.port,
                "connect_timeout": "2s",
                "password": format!("{username}:{password}"),
                "tls": { "enabled": true, "server_name": sni, "insecure": false }
            });
            if let Some((start, end)) = node.hop {
                o["server_ports"] = json!([format!("{start}:{end}")]);
                o["hop_interval"] = json!("30s");
            }
            if let Some(pw) = obfs_password {
                o["obfs"] = json!({ "type": "salamander", "password": pw });
            }
            o
        }
    }
}

/// v3.6.0 R5: interval 60s（默认 3m 过慢、10s 会让住宅 IP 每分钟被打 6 次）；
/// `idle_timeout` 必须 >= interval，否则 sing-box 启动即 FATAL（`check` 查不出来）。
fn urltest(tag: &str, outbounds: &[&str]) -> Value {
    json!({
        "type": "urltest",
        "tag": tag,
        "outbounds": outbounds,
        "url": "https://www.gstatic.com/generate_204",
        "interval": "60s",
        "tolerance": 100,
        "idle_timeout": "30m",
        "interrupt_exist_connections": false
    })
}

fn client_inbounds() -> Value {
    json!([
        { "type": "mixed", "tag": "mixed-in", "listen": "127.0.0.1", "listen_port": 7890 },
        {
            "type": "tun", "tag": "tun-in", "interface_name": "bui-tun",
            "address": ["172.19.0.1/30", "fdfe:dcba:9876::1/126"],
            "auto_route": true, "strict_route": true
        }
    ])
}

/// Complete configuration for upstream sing-box 1.14.2.
/// Business traffic uses the delivered authorized proxy paths; there is no local
/// native public DIRECT alternative. Bootstrap DNS is reserved for proxy dialing.
/// Zero available nodes produce a loadable refusal configuration.
pub fn singbox(nodes: &[Node], split: &SplitRules, dial_ip: &str) -> Value {
    if nodes.is_empty() {
        return json!({
            "log": { "level": "info", "timestamp": true },
            "inbounds": client_inbounds(),
            "outbounds": [],
            "route": { "rules": [{ "action": "reject" }], "auto_detect_interface": true }
        });
    }
    let host = nodes[0].host.as_str();
    let dial = dial_host(host, dial_ip);
    let mut outbounds = Vec::with_capacity(nodes.len() + 3);
    let mut direct_tags = Vec::new();
    let mut resi_tags = Vec::new();
    for n in nodes {
        outbounds.push(singbox_outbound(n, dial));
        if is_resi(n.kind) {
            resi_tags.push(tag(n.kind));
        } else {
            direct_tags.push(tag(n.kind));
        }
    }
    let mut rules = vec![
        json!({ "action": "sniff" }),
        json!({ "protocol": "dns", "action": "hijack-dns" }),
        json!({ "ip_is_private": true, "action": "reject" }),
        json!({ "ip_version": 6, "action": "reject" }),
    ];
    let primary;
    let route_final;
    if !direct_tags.is_empty() && !resi_tags.is_empty() {
        outbounds.push(urltest("direct-pool", &direct_tags));
        outbounds.push(urltest("residential-pool", &resi_tags));
        // A residential identity also applies to business DNS. Split routing can
        // still select the independently authorized direct proxy for other traffic.
        primary = "residential-pool";
        if split.global {
            route_final = "residential-pool";
        } else {
            let keywords = effective_keywords(split);
            if !keywords.is_empty() {
                rules.push(json!({ "domain_keyword": keywords, "outbound": "residential-pool" }));
            }
            route_final = "direct-pool";
        }
    } else {
        let only = if direct_tags.is_empty() {
            &resi_tags
        } else {
            &direct_tags
        };
        outbounds.push(urltest("proxy", only));
        primary = "proxy";
        route_final = "proxy";
    }
    outbounds.push(json!({ "type": "direct", "tag": "bootstrap-direct" }));
    let mut dns_rules = Vec::new();
    if host_is_domain(host) && !dial_ip.is_empty() {
        dns_rules.push(json!({
            "domain": [host], "action": "predefined",
            "answer": [format!("{host}. IN A {dial_ip}")]
        }));
    }
    json!({
        "log": { "level": "info", "timestamp": true },
        "experimental": {
            "clash_api": {
                "external_controller": "127.0.0.1:9090", "external_ui": "", "secret": "", "default_mode": "rule"
            },
            "cache_file": { "enabled": true, "path": "cache.db" }
        },
        "dns": {
            "servers": [
                { "tag": "remote", "type": "https", "server": "8.8.8.8", "detour": primary },
                { "tag": "bootstrap", "type": "udp", "server": "223.5.5.5", "detour": "bootstrap-direct" }
            ],
            "rules": dns_rules, "final": "remote", "strategy": "ipv4_only"
        },
        "inbounds": client_inbounds(),
        "outbounds": outbounds,
        "route": {
            "rules": rules, "final": route_final,
            "auto_detect_interface": true, "default_domain_resolver": "bootstrap"
        }
    })
}

/// 按插入顺序构造 YAML 映射：`serde_json::Value` 的键会被字典序重排，
/// 而订阅文件是给人看的，键序照 v3 模板。
fn ymap(pairs: Vec<(&str, Yaml)>) -> Yaml {
    let mut m = Mapping::with_capacity(pairs.len());
    for (k, v) in pairs {
        m.insert(Yaml::from(k), v);
    }
    Yaml::Mapping(m)
}

fn y<T: serde::Serialize>(v: T) -> Yaml {
    serde_yaml::to_value(v).expect("YAML 值序列化")
}

fn clash_proxy(node: &Node, name: &str) -> Yaml {
    match &node.transport {
        Transport::Reality {
            uuid,
            public_key,
            short_id,
            server_name,
            fingerprint,
            flow,
        } => ymap(vec![
            ("name", y(name)),
            ("type", y("vless")),
            ("server", y(&node.host)),
            ("port", y(node.port)),
            ("uuid", y(uuid)),
            ("flow", y(flow)),
            ("tls", y(true)),
            ("udp", y(true)),
            ("servername", y(server_name)),
            ("client-fingerprint", y(fingerprint)),
            (
                "reality-opts",
                ymap(vec![
                    ("public-key", y(public_key)),
                    ("short-id", y(short_id)),
                ]),
            ),
            ("network", y("tcp")),
        ]),
        Transport::Hysteria2 {
            username,
            password,
            sni,
            obfs_password,
        } => {
            let mut pairs = vec![
                ("name", y(name)),
                ("type", y("hysteria2")),
                ("server", y(&node.host)),
                ("port", y(node.port)),
            ];
            if let Some((start, end)) = node.hop {
                pairs.push(("ports", y(format!("{start}-{end}"))));
                pairs.push(("hop-interval", y(30)));
            }
            pairs.extend([
                ("password", y(format!("{username}:{password}"))),
                ("sni", y(sni)),
                ("skip-cert-verify", y(false)),
                ("alpn", y(["h3"])),
            ]);
            if let Some(pw) = obfs_password {
                pairs.push(("obfs", y("salamander")));
                pairs.push(("obfs-password", y(pw)));
            }
            ymap(pairs)
        }
    }
}

/// mihomo 的 TUN 接管参数。**不下发 `enable`**：TUN 开关归客户端（v2rayN / Clash Verge）自己管。
///
/// v6 地址与 sing-box 订阅的 TUN `address` 同源（spec 2026-09-10-ipv6-takeover-design §3.1），
/// 有 v6 地址 `auto-route` 才会把 `::/0` 装进 TUN 路由表，裸 v6 才进得了隧道被下面的
/// `IP-CIDR6,::/0,REJECT` 打回、让应用回退 v4。
///
/// 不写 `inet4-address`：mihomo `config/config.go` 的 `RawTun` 里该字段是注释掉的，
/// `parseTun()` 固定用 `dns.fake-ip-range` 取 /30（本配置即 `198.18.0.1/30`），写了也被忽略。
fn clash_tun() -> Yaml {
    ymap(vec![
        ("stack", y("mixed")),
        ("auto-route", y(true)),
        ("strict-route", y(true)),
        ("auto-detect-interface", y(true)),
        ("inet6-address", y(["fdfe:dcba:9876::1/126"])),
        ("dns-hijack", y(["any:53"])),
    ])
}

fn clash_group(name: &str, members: &[String]) -> Yaml {
    ymap(vec![
        ("name", y(name)),
        ("type", y("url-test")),
        ("proxies", y(members)),
        ("url", y("https://www.gstatic.com/generate_204")),
        ("interval", y(300)),
        ("tolerance", y(50)),
    ])
}

/// mihomo（Clash Meta / Clash Verge Rev）订阅 YAML（`/api/clash/<token>`）。
///
/// 不含 `mixed-port`（由客户端自身管理）。v3 的首部注释带生成时间，这里去掉：
/// 期望态渲染必须可重复，带时间戳会让每次比对都判"变了"。
///
/// v4 在 v3 输出之上新增 IPv6 接管（2026-09-12 裁决，golden 同步补齐）：顶层 `ipv6: true`、
/// `dns.ipv6: false`、`tun` 接管参数、三条 `IP-CIDR6` 规则，与 sing-box 订阅同构，
/// 依据 spec 2026-09-10-ipv6-takeover-design。其余字段与 v3 逐项相同。
pub fn clash(nodes: &[Node], username: &str, split: &SplitRules) -> String {
    let host = nodes.first().map(|n| n.host.as_str()).unwrap_or("");

    let mut proxies: Vec<Yaml> = Vec::with_capacity(nodes.len());
    let mut direct_names: Vec<String> = Vec::new();
    let mut resi_names: Vec<String> = Vec::new();
    for n in nodes {
        let name = n.name(username);
        proxies.push(clash_proxy(n, &name));
        if is_resi(n.kind) {
            resi_names.push(name);
        } else {
            direct_names.push(name);
        }
    }

    let mut groups: Vec<Yaml> = Vec::with_capacity(3);
    let mut members: Vec<String> = Vec::with_capacity(2);
    if !direct_names.is_empty() {
        groups.push(clash_group("直连自动", &direct_names));
        members.push("直连自动".into());
    }
    if !resi_names.is_empty() {
        groups.push(clash_group("住宅自动", &resi_names));
        members.push("住宅自动".into());
    }
    let mut select: Vec<String> = members.clone();
    select.extend(direct_names.iter().cloned());
    select.extend(resi_names.iter().cloned());
    // 零节点守卫，与 [`singbox`] 同一个理由：空的 `select` 组 + 指向不存在的组的 `MATCH`
    // 会让 mihomo 拒绝整份配置，而不只是少一个节点。
    if select.is_empty() {
        select.push("REJECT".into());
    }
    groups.push(ymap(vec![
        ("name", y("PROXY")),
        ("type", y("select")),
        ("proxies", y(&select)),
    ]));

    let has_split = !direct_names.is_empty() && !resi_names.is_empty();
    let match_target = if has_split {
        if split.global {
            "住宅自动"
        } else {
            "直连自动"
        }
    } else if !resi_names.is_empty() {
        "住宅自动"
    } else if !direct_names.is_empty() {
        "直连自动"
    } else {
        "REJECT" // No authorized proxy exists; never substitute native direct service.
    };

    let mut rules = if nodes.is_empty() {
        vec![]
    } else {
        vec!["GEOIP,private,REJECT,no-resolve".to_string()]
    };
    if has_split && !split.global {
        rules.extend(
            effective_keywords(split)
                .iter()
                .map(|k| format!("DOMAIN-KEYWORD,{k},住宅自动")),
        );
    }
    if !nodes.is_empty() {
        rules.push("IP-CIDR6,fc00::/7,REJECT,no-resolve".to_string());
        rules.push("IP-CIDR6,fe80::/10,REJECT,no-resolve".to_string());
        rules.push("IP-CIDR6,::/0,REJECT,no-resolve".to_string());
    }
    rules.push(format!("MATCH,{match_target}"));
    let business_dns = if !resi_names.is_empty() {
        "https://8.8.8.8/dns-query#住宅自动"
    } else if !direct_names.is_empty() {
        "https://8.8.8.8/dns-query#直连自动"
    } else {
        "udp://127.0.0.1:1"
    };
    let bootstrap_dns = if nodes.is_empty() {
        "127.0.0.1"
    } else {
        "223.5.5.5"
    };
    let dns = ymap(vec![
        ("enable", y(true)),
        ("ipv6", y(false)),
        ("enhanced-mode", y("fake-ip")),
        ("fake-ip-range", y("198.18.0.1/16")),
        (
            "fake-ip-filter",
            if host.is_empty() {
                y(["*.lan", "*.local", "*.localhost"])
            } else {
                y(["*.lan", "*.local", "*.localhost", host])
            },
        ),
        // These DNS servers resolve only resolver/proxy addresses, never business domains.
        ("default-nameserver", y([bootstrap_dns])),
        ("proxy-server-nameserver", y([bootstrap_dns])),
        ("nameserver", y([business_dns])),
    ]);
    let doc = ymap(vec![
        ("mode", y("rule")),
        ("log-level", y("warning")),
        ("unified-delay", y(true)),
        ("tcp-concurrent", y(true)),
        ("find-process-mode", y("strict")),
        ("global-client-fingerprint", y("chrome")),
        // 必须 true：mihomo `config/config.go parseIPV6()` 在 `!rawCfg.IPv6` 时把
        // `Tun.Inet6Address` 置 nil，`ipv6: false` 会让 `tun.inet6-address` 失效、
        // `auto-route` 不装 `::/0`，裸 v6 从物理网卡漏出去。AAAA 由 `dns.ipv6: false` 关。
        ("ipv6", y(true)),
        ("tun", clash_tun()),
        ("dns", dns),
        ("proxies", Yaml::Sequence(proxies)),
        ("proxy-groups", Yaml::Sequence(groups)),
        ("rules", y(&rules)),
    ]);

    format!(
        "# B-UI Clash Meta 订阅配置\n# 用户: {username}\n\n{}",
        serde_yaml::to_string(&doc).expect("clash 配置序列化")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_node_configs_refuse_traffic_without_implicit_direct_service() {
        let split = SplitRules {
            enabled: false,
            global: false,
            keywords: vec![],
        };
        let singbox = singbox(&[], &split, "");
        assert_eq!(singbox["route"]["rules"], json!([{ "action": "reject" }]));
        assert_eq!(singbox["outbounds"], json!([]));
        assert!(
            singbox.get("dns").is_none(),
            "refusal must not resolve business queries directly"
        );
        let clash: Yaml = serde_yaml::from_str(&clash(&[], "no-access", &split)).unwrap();
        assert_eq!(clash["rules"], y(["MATCH,REJECT"]));
        assert_eq!(clash["proxy-groups"][0]["proxies"], y(["REJECT"]));
        assert_eq!(clash["dns"]["nameserver"], y(["udp://127.0.0.1:1"]));
    }

    #[test]
    fn component_encoding_matches_encode_uri_component() {
        // JS encodeURIComponent 的不编码集合：A-Za-z0-9-_.!~*'()
        assert_eq!(enc("aZ0-_.!~*'()"), "aZ0-_.!~*'()");
        assert_eq!(enc("p@ss:w/rd +&#?"), "p%40ss%3Aw%2Frd%20%2B%26%23%3F");
        assert_eq!(enc("HY2直连"), "HY2%E7%9B%B4%E8%BF%9E");
    }

    #[test]
    fn keywords_are_sorted_and_deduped_only_when_pool_active() {
        let base = SplitRules {
            enabled: true,
            global: false,
            keywords: vec!["openai".into(), "claude".into(), "openai".into()],
        };
        assert_eq!(effective_keywords(&base), vec!["claude", "openai"]);
        let off = SplitRules {
            enabled: false,
            ..base
        };
        assert!(effective_keywords(&off).is_empty());
    }

    #[test]
    fn dial_host_keeps_ip_literals() {
        assert_eq!(dial_host("example.com", "203.0.113.10"), "203.0.113.10");
        assert_eq!(dial_host("example.com", ""), "example.com");
        assert_eq!(dial_host("203.0.113.10", "198.51.100.1"), "203.0.113.10");
    }
}
