//! 三种订阅渲染器与 v3 golden 样本逐项等价（base64 按行、JSON/YAML 按语义）。
mod common;

use base64::Engine;
use bui_schema::model::State;
use bui_schema::nodes::{nodes_for, Node};
use bui_schema::render::{subscription, SplitRules};
use pretty_assertions::assert_eq;

const USERS: [&str; 4] = ["alice", "bob", "carol", "dave"];
const MODES: [&str; 3] = ["global", "split", "obfs"];

/// 某用户的完整节点集合，不做任何过滤就与 v3 golden 逐项相等：单协议 + 住宅的 bob / carol
/// 导入后 `direct=false`，与 v3 一样只有住宅版（2026-09-13 裁决撤销了此前的 `v3_nodes_only` 例外）。
fn nodes_of(s: &State, username: &str) -> Vec<Node> {
    let user = s.users.iter().find(|u| u.username == username).unwrap();
    nodes_for(user, &s.node, &s.residential)
}

fn split_of(s: &State) -> SplitRules {
    SplitRules::from_group(s.residential.default_group().unwrap())
}

fn b64(s: &str) -> String {
    String::from_utf8(
        base64::engine::general_purpose::STANDARD
            .decode(s.trim())
            .unwrap(),
    )
    .unwrap()
}

#[test]
fn uri_list_matches_v3() {
    for mode in MODES {
        let s = common::state(mode);
        for u in USERS {
            let nodes = nodes_of(&s, u);
            let got = b64(&subscription::uri_list(&nodes, u));
            let want = b64(&common::expected(mode, u, "sub.txt"));
            assert_eq!(got, want, "mode={mode} user={u}");
        }
    }
}

#[test]
fn singbox_matches_v3() {
    for mode in MODES {
        let s = common::state(mode);
        let split = split_of(&s);
        for u in USERS {
            let nodes = nodes_of(&s, u);
            let got = subscription::singbox(&nodes, &split, &s.node.public_ip);
            let want: serde_json::Value =
                serde_json::from_str(&common::expected(mode, u, "singbox.json")).unwrap();
            assert_eq!(got, want, "mode={mode} user={u}");
            common::check_singbox(&got);
        }
    }
}

#[test]
fn clash_matches_v3() {
    for mode in MODES {
        let s = common::state(mode);
        let split = split_of(&s);
        for u in USERS {
            let nodes = nodes_of(&s, u);
            let got: serde_yaml::Value =
                serde_yaml::from_str(&subscription::clash(&nodes, u, &split)).unwrap();
            let want: serde_yaml::Value =
                serde_yaml::from_str(&common::expected(mode, u, "clash.yaml")).unwrap();
            assert_eq!(got, want, "mode={mode} user={u}");
        }
    }
}

/// Clash 订阅的 IPv6 接管字段——v3 没有，v4 在 v3 golden 之上**新增**（2026-09-12 裁决）。
///
/// 字段名与语法以 mihomo 文档为准：`tun.stack` / `auto-route` / `strict-route` /
/// `auto-detect-interface` / `inet6-address` / `dns-hijack`（wiki.metacubex.one/config/inbound/tun/）、
/// 顶层与 `dns` 下的 `ipv6`（/config/general/、/config/dns/）、`IP-CIDR6` 与 `no-resolve`
/// （/config/rules/）。目标客户端是 v2rayN 7.x 内置 mihomo 与 Clash Verge。
///
/// 语义对应 sing-box 订阅（`docs/superpowers/specs/2026-09-10-ipv6-takeover-design.md` §2/§3.1）：
/// `ip_is_private ⇒ direct` ↔ `IP-CIDR6,fc00::/7|fe80::/10,DIRECT`，
/// `ip_version 6 ⇒ reject` ↔ `IP-CIDR6,::/0,REJECT`；TUN v6 地址与 sing-box 侧同源。
/// `tun.enable` 故意不下发：TUN 开关归客户端（v2rayN / Clash Verge）自己管。
///
/// 顶层 `ipv6` 必须为 `true`：mihomo `config/config.go parseIPV6()` 在
/// `!rawCfg.IPv6` 时把 `Tun.Inet6Address` 置 nil，`ipv6: false` 会让下面的
/// `inet6-address` 失效、`auto-route` 不装 `::/0`，裸 v6 进不了隧道。AAAA 由
/// `dns.ipv6: false` 关闭（`hub/executor updateDNS()`：`ipv6 = dns.ipv6 && general.ipv6`）。
#[test]
fn clash_declares_ipv6_takeover() {
    for mode in MODES {
        let s = common::state(mode);
        let split = split_of(&s);
        for u in USERS {
            let nodes = nodes_of(&s, u);
            let doc: serde_yaml::Value =
                serde_yaml::from_str(&subscription::clash(&nodes, u, &split)).unwrap();
            let ctx = format!("mode={mode} user={u}");

            assert_eq!(doc["ipv6"], serde_yaml::Value::Bool(true), "{ctx}");
            assert_eq!(doc["dns"]["ipv6"], serde_yaml::Value::Bool(false), "{ctx}");

            let tun = doc["tun"].as_mapping().expect("tun 段");
            // enable 归客户端自己开关，订阅不写
            assert!(
                !tun.contains_key(serde_yaml::Value::from("enable")),
                "{ctx}"
            );
            // inet4-address 是 mihomo 忽略的死配置（RawTun 里已注释掉，parseTun 由
            // dns.fake-ip-range 推 /30），不写
            assert!(
                !tun.contains_key(serde_yaml::Value::from("inet4-address")),
                "{ctx}"
            );
            assert_eq!(
                doc["tun"]["stack"],
                serde_yaml::Value::from("mixed"),
                "{ctx}"
            );
            for k in ["auto-route", "strict-route", "auto-detect-interface"] {
                assert_eq!(
                    doc["tun"][k],
                    serde_yaml::Value::Bool(true),
                    "{ctx} key={k}"
                );
            }
            assert_eq!(
                doc["tun"]["inet6-address"]
                    .as_sequence()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap())
                    .collect::<Vec<_>>(),
                vec!["fdfe:dcba:9876::1/126"],
                "{ctx}"
            );
            assert_eq!(
                doc["tun"]["dns-hijack"]
                    .as_sequence()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap())
                    .collect::<Vec<_>>(),
                vec!["any:53"],
                "{ctx}"
            );

            // 三条 IPv6 规则紧贴最终 MATCH 之前、既有直连/分流规则之后
            let rules: Vec<&str> = doc["rules"]
                .as_sequence()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            let tail = &rules[rules.len() - 4..];
            assert_eq!(
                tail[..3],
                [
                    "IP-CIDR6,fc00::/7,DIRECT,no-resolve",
                    "IP-CIDR6,fe80::/10,DIRECT,no-resolve",
                    "IP-CIDR6,::/0,REJECT,no-resolve",
                ],
                "{ctx}"
            );
            assert!(tail[3].starts_with("MATCH,"), "{ctx} tail={tail:?}");
            assert!(
                rules[..rules.len() - 4]
                    .iter()
                    .all(|r| !r.starts_with("IP-CIDR6") && !r.starts_with("MATCH")),
                "{ctx}"
            );
        }
    }
}

/// golden 里每一行 URI 都能被 `parse::node_uri` 解回同一个节点（label 带 `{user}-` 前缀）。
#[test]
fn uri_list_round_trips_through_node_uri_parser() {
    let s = common::state("obfs");
    let nodes = nodes_of(&s, "alice");
    let decoded = b64(&common::expected("obfs", "alice", "sub.txt"));
    let lines: Vec<&str> = decoded.lines().collect();
    assert_eq!(lines.len(), nodes.len());
    for (line, want) in lines.iter().zip(&nodes) {
        let got = bui_schema::parse::node_uri(line).unwrap();
        let mut want = want.clone();
        want.label = want.name("alice");
        assert_eq!(got, want, "line={line}");
    }
}

/// 池失效（未启用 / 空池）时不发分流关键字：中继此时 fail-open 直连，
/// 再发规则只是把流量导向一个直连池。住宅节点本身仍在（与 v3 的 4 节点拓扑一致）。
#[test]
fn disabled_pool_emits_no_keyword_rules() {
    let mut s = common::state("split");
    s.residential
        .groups
        .get_mut("default")
        .unwrap()
        .upstreams
        .clear();
    let split = split_of(&s);
    assert!(!split.enabled);
    let nodes = nodes_of(&s, "alice");

    let sb = subscription::singbox(&nodes, &split, &s.node.public_ip);
    assert!(sb["route"]["rules"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r.get("domain_keyword").is_none()));
    assert!(sb["outbounds"]
        .as_array()
        .unwrap()
        .iter()
        .any(|o| o["tag"] == "residential-pool"));

    let yaml = subscription::clash(&nodes, "alice", &split);
    assert!(!yaml.contains("DOMAIN-KEYWORD"));
}

/// 多槽时三种订阅里的 HY2 住宅端口/跳跃区间都跟着用户的槽位走，
/// 其余节点与 golden 逐字相同。
#[test]
fn multi_slot_moves_only_the_residential_hy2_endpoint() {
    let mut s = common::state("global");
    // fixture 自带 2 条上游，补到 3 条并按创建时间轮流落槽
    let g = s.residential.groups.get_mut("default").unwrap();
    let mut third = g.upstreams[0].clone();
    third.id = uuid::Uuid::from_u128(0xdead);
    third.host = "isp3.example.net".into();
    g.upstreams.push(third);
    bui_schema::slots::sync_slots(&mut s.residential);
    bui_schema::slots::migrate_unassigned(&mut s);
    assert_eq!(bui_schema::slots::slot_span(&s.residential), 3);

    let split = split_of(&s);
    for u in USERS {
        let Some(user) = s.users.iter().find(|x| x.username == u) else {
            continue;
        };
        let idx = bui_schema::slots::index_of_user(user, &s.residential);
        let res = bui_schema::slots::resources_of(&s.node.ports, &s.residential, idx);
        let nodes = nodes_of(&s, u);
        let Some(resi) = nodes
            .iter()
            .find(|n| n.kind == bui_schema::nodes::NodeKind::Hy2Residential)
        else {
            continue; // 没有住宅权益的用户
        };
        assert_eq!(resi.port, res.hy2_port, "user={u}");
        assert_eq!(resi.hop, Some(res.hop), "user={u}");

        // URI 列表里出现的就是这一槽的端口与 mport
        let text = String::from_utf8(
            base64::engine::general_purpose::STANDARD
                .decode(subscription::uri_list(&nodes, u).trim())
                .unwrap(),
        )
        .unwrap();
        assert!(
            text.contains(&format!(":{}?", res.hy2_port)),
            "user={u} 的 URI 里没有槽位端口 {}：\n{text}",
            res.hy2_port
        );
        assert!(
            text.contains(&format!("mport={}-{}", res.hop.0, res.hop.1)),
            "user={u}"
        );

        // sing-box / clash 同源，且必须仍然过 check
        let sb = subscription::singbox(&nodes, &split, &s.node.public_ip);
        let out = sb["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["tag"] == "hy2-residential")
            .unwrap()
            .clone();
        assert_eq!(out["server_port"], res.hy2_port, "user={u}");
        common::check_singbox_all(&sb);
        let yaml = subscription::clash(&nodes, u, &split);
        assert!(
            yaml.contains(&format!("port: {}", res.hy2_port)),
            "user={u} 的 clash YAML 里没有槽位端口"
        );
    }
}
