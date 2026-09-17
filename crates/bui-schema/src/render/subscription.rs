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

/// 国内域名直连后缀，与 `b-ui-client.sh` 的 TUN 模板同源。
///
/// 不用远程 `rule_set`：sing-box 1.13 / 1.15 的字段不兼容。
pub const CN_DIRECT_SUFFIXES: &[&str] = &[
    ".qq.com",
    ".qpic.cn",
    ".qlogo.cn",
    ".wechat.com",
    ".weixin.qq.com",
    ".wx.qq.com",
    ".tencent.com",
    ".tencent-cloud.net",
    ".myqcloud.com",
    ".gtimg.com",
    ".xiaohongshu.com",
    ".xhscdn.com",
    ".douyin.com",
    ".douyincdn.com",
    ".amemv.com",
    ".bytedance.com",
    ".bytecdntp.com",
    ".toutiao.com",
    ".iesdouyin.com",
    ".kuaishou.com",
    ".ksapisrv.com",
    ".bilibili.com",
    ".bilivideo.com",
    ".hdslb.com",
    ".taobao.com",
    ".tbcdn.cn",
    ".alicdn.com",
    ".aliyuncs.com",
    ".alipay.com",
    ".alipayobjects.com",
    ".alibaba.com",
    ".alibabacloud.com",
    ".tmall.com",
    ".tmall.hk",
    ".jd.com",
    ".360buyimg.com",
    ".jdcdn.com",
    ".baidu.com",
    ".bdstatic.com",
    ".bdimg.com",
    ".cn",
];

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

/// 生效的分流关键字：池失效时为空（中继此时 fail-open 直连，发规则只是噪声）；
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

/// 一条节点 URI（v2rayN / bui-c 都吃这个格式）。
fn node_uri(node: &Node, username: &str) -> String {
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

/// 完整的 sing-box 客户端配置（`/api/subscription/<token>`）。
///
/// `dial_ip` 是服务器域名对应的公网 IPv4（`State.node.public_ip`）：出站按它直连、
/// DNS 段同时下发 predefined 应答防 GFW 投毒 bootstrap。为空则退回用域名拨号。
///
/// **零节点守卫**：`nodes` 可以是空的 —— 只有住宅 HY2 权益、而凭据池耗尽
/// （`hy2pool::MigrateReport::unassigned > 0`）或建完用户到下一轮收敛之间那一瞬，
/// [`nodes_for`](crate::nodes::nodes_for) 一个节点都不发。此时绝不能渲出空 `urltest`
/// （sing-box：`outbounds: []` ⇒ 加载失败），也不能让 `dns.servers[remote].detour` 与
/// `route.final` 指向它 —— 那样该用户的 `/api/subscription/<token>` **整份不可用**，
/// 而不只是少一个节点。零节点一律退成全部走 `direct`：配置仍然加载得起来，客户端起得来、
/// 只是没有代理出口，下一轮收敛补上凭据后重新拉一次订阅即可。
pub fn singbox(nodes: &[Node], split: &SplitRules, dial_ip: &str) -> Value {
    let host = nodes.first().map(|n| n.host.as_str()).unwrap_or("");
    let dial = dial_host(host, dial_ip);

    let mut outbounds: Vec<Value> = Vec::with_capacity(nodes.len() + 3);
    let mut direct_tags: Vec<&str> = Vec::new();
    let mut resi_tags: Vec<&str> = Vec::new();
    for n in nodes {
        outbounds.push(singbox_outbound(n, dial));
        if is_resi(n.kind) {
            resi_tags.push(tag(n.kind));
        } else {
            direct_tags.push(tag(n.kind));
        }
    }

    // 顺序：sniff → DNS 劫持 → 私网直连 → IPv6 reject（服务端无 v6 出口，RST 让应用回退 v4）
    //       → 住宅关键字 → cn 直连
    let mut rules = vec![
        json!({ "action": "sniff" }),
        json!({ "protocol": "dns", "action": "hijack-dns" }),
        json!({ "ip_is_private": true, "outbound": "direct" }),
        json!({ "ip_version": 6, "action": "reject" }),
    ];

    // 只有同时有直连池和住宅池才做 global / 域名分流；单池直接 final
    let primary;
    let route_final;
    if direct_tags.is_empty() && resi_tags.is_empty() {
        // 零节点守卫（见函数文档）：不产空 `urltest`，DNS 与 route 都指向恒存在的 `direct`
        primary = "direct";
        route_final = "direct";
    } else if !direct_tags.is_empty() && !resi_tags.is_empty() {
        outbounds.push(urltest("direct-pool", &direct_tags));
        outbounds.push(urltest("residential-pool", &resi_tags));
        primary = "direct-pool";
        if split.global {
            route_final = "residential-pool";
        } else {
            let keywords = effective_keywords(split);
            if !keywords.is_empty() {
                rules.push(json!({
                    "domain_keyword": keywords,
                    "outbound": "residential-pool"
                }));
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
    rules.push(json!({ "domain_suffix": CN_DIRECT_SUFFIXES, "outbound": "direct" }));
    outbounds.push(json!({ "type": "direct", "tag": "direct" }));

    // 服务器域名预解析：只服务隧道内的应用查询；出站自身已按 dial 直连，两者同一个 IP
    let mut dns_rules = Vec::with_capacity(2);
    if host_is_domain(host) && !dial_ip.is_empty() {
        dns_rules.push(json!({
            "domain": [host],
            "action": "predefined",
            "answer": [format!("{host}. IN A {dial_ip}")]
        }));
    }
    dns_rules.push(json!({ "domain_suffix": CN_DIRECT_SUFFIXES, "server": "local" }));

    json!({
        "log": { "level": "info", "timestamp": true },
        "experimental": {
            "clash_api": {
                "external_controller": "127.0.0.1:9090",
                "external_ui": "",
                "secret": "",
                "default_mode": "rule"
            },
            "cache_file": { "enabled": true, "path": "cache.db" }
        },
        "dns": {
            "servers": [
                { "tag": "remote", "type": "https", "server": "8.8.8.8", "detour": primary },
                { "tag": "local", "type": "udp", "server": "223.5.5.5" }
            ],
            "rules": dns_rules,
            "final": "remote",
            "strategy": "ipv4_only"
        },
        "inbounds": [
            { "type": "mixed", "tag": "mixed-in", "listen": "127.0.0.1", "listen_port": 7890 },
            {
                "type": "tun",
                "tag": "tun-in",
                "interface_name": "bui-tun",
                "address": ["172.19.0.1/30", "fdfe:dcba:9876::1/126"],
                "auto_route": true,
                "strict_route": true
            }
        ],
        "outbounds": outbounds,
        "route": {
            "rules": rules,
            "final": route_final,
            "auto_detect_interface": true,
            "default_domain_resolver": "local"
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
        select.push("DIRECT".into());
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
        "DIRECT" // 零节点：两个自动组都不存在，MATCH 只能指内置出站
    };

    let mut rules = vec![
        "GEOIP,private,DIRECT,no-resolve".to_string(),
        "GEOSITE,cn,DIRECT".to_string(),
        "GEOIP,cn,DIRECT,no-resolve".to_string(),
    ];
    if has_split && !split.global {
        rules.extend(
            effective_keywords(split)
                .iter()
                .map(|k| format!("DOMAIN-KEYWORD,{k},住宅自动")),
        );
    }
    // IPv6 接管，对应 sing-box 订阅的 `ip_is_private ⇒ direct` 与 `ip_version 6 ⇒ reject`：
    // mihomo 没有 `ip_version` 匹配器，用 IP-CIDR6 等价表达（`no-resolve` 防触发 DNS 解析）。
    // 顺序：ULA / link-local 先直连，剩下的 v6 一律 REJECT。
    rules.push("IP-CIDR6,fc00::/7,DIRECT,no-resolve".to_string());
    rules.push("IP-CIDR6,fe80::/10,DIRECT,no-resolve".to_string());
    rules.push("IP-CIDR6,::/0,REJECT,no-resolve".to_string());
    rules.push(format!("MATCH,{match_target}"));

    let dns = ymap(vec![
        ("enable", y(true)),
        // AAAA 全关：mihomo `hub/executor updateDNS()` 取 `dns.ipv6 && general.ipv6`，
        // 这里关 dns 侧即可，顶层 `ipv6` 必须留 true（见 doc 顶部与 clash_tun）。
        ("ipv6", y(false)),
        ("enhanced-mode", y("fake-ip")),
        ("fake-ip-range", y("198.18.0.1/16")),
        // 零节点用户的 `host` 是空串（见 [`singbox`]）：`fake-ip-filter` 与
        // `nameserver-policy` 都是编译进域名 trie 的匹配面，空串是它的非法输入
        // （同形状的空 domain 项在 sing-box 上是硬 FATAL），所以空 host 一项都不进。
        (
            "fake-ip-filter",
            if host.is_empty() {
                y(["*.lan", "*.local", "*.localhost"])
            } else {
                y(["*.lan", "*.local", "*.localhost", host])
            },
        ),
        ("default-nameserver", y(["223.5.5.5", "119.29.29.29"])),
        (
            "nameserver",
            y([
                "https://dns.alidns.com/dns-query",
                "https://doh.pub/dns-query",
            ]),
        ),
        (
            "nameserver-policy",
            ymap(if host.is_empty() {
                vec![]
            } else {
                vec![(host, y("223.5.5.5"))]
            }),
        ),
        (
            "fallback",
            y(["https://8.8.8.8/dns-query", "https://1.1.1.1/dns-query"]),
        ),
        (
            "fallback-filter",
            ymap(vec![("geoip", y(true)), ("geoip-code", y("CN"))]),
        ),
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
