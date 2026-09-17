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

/// 混淆开启时三种订阅里**住宅** HY2 也带 obfs 参数（2026-09-15 裁决：混淆覆盖全部 HY2 实例），
/// 形状与直连逐项相同；关闭时三种订阅里一个 obfs 字都没有。
#[test]
fn obfs_reaches_residential_hy2_in_all_three_subscriptions() {
    let s = common::state("obfs");
    let split = split_of(&s);
    let nodes = nodes_of(&s, "alice");

    let uris = b64(&subscription::uri_list(&nodes, "alice"));
    let resi_line = uris
        .lines()
        .find(|l| l.ends_with("#alice-HY2%E4%BD%8F%E5%AE%85"))
        .expect("alice 的 HY2 住宅 URI");
    assert!(
        resi_line.contains("&mport=41000-50000&obfs=salamander&obfs-password=obfs-pw-test#"),
        "{resi_line}"
    );

    let sb = subscription::singbox(&nodes, &split, &s.node.public_ip);
    let out = |tag: &str| {
        sb["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["tag"] == tag)
            .unwrap()
            .clone()
    };
    let want = serde_json::json!({"type": "salamander", "password": "obfs-pw-test"});
    assert_eq!(out("hy2-residential")["obfs"], want);
    assert_eq!(out("hy2-direct")["obfs"], want);
    common::check_singbox_all(&sb);

    let clash: serde_yaml::Value =
        serde_yaml::from_str(&subscription::clash(&nodes, "alice", &split)).unwrap();
    let resi = clash["proxies"]
        .as_sequence()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "alice-HY2住宅")
        .unwrap();
    assert_eq!(resi["obfs"], serde_yaml::Value::from("salamander"));
    assert_eq!(
        resi["obfs-password"],
        serde_yaml::Value::from("obfs-pw-test")
    );

    let off = common::state("global");
    let nodes = nodes_of(&off, "alice");
    let split = split_of(&off);
    assert!(!b64(&subscription::uri_list(&nodes, "alice")).contains("obfs"));
    assert!(!subscription::singbox(&nodes, &split, &off.node.public_ip)
        .to_string()
        .contains("obfs"));
    assert!(!subscription::clash(&nodes, "alice", &split).contains("obfs"));
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

/// 4.1：多槽也不再动三种订阅里的 HY2 住宅端点 —— 每个用户都是期望态里那一对
/// `hy2_resi` / `hy2_resi_hop`（整段），其余节点与 golden 逐字相同。
#[test]
fn multi_slot_does_not_move_the_residential_hy2_endpoint() {
    let mut s = common::state("global");
    // fixture 自带 2 条上游，补到 3 条并按创建时间轮流落槽
    let g = s.residential.groups.get_mut("default").unwrap();
    let mut third = g.upstreams[0].clone();
    third.id = uuid::Uuid::from_u128(0xdead);
    third.host = "isp3.example.net".into();
    g.upstreams.push(third);
    // 4.1：住宅 HY2 不再含槽位信息 —— 三条上游、三个槽，**每个用户端口与区间都一样**
    bui_schema::slots::sync_slots(&mut s.residential);
    bui_schema::slots::migrate_unassigned(&mut s);
    bui_schema::hy2pool::migrate(&mut s, time::OffsetDateTime::now_utc());

    let (port, hop) = (s.node.ports.hy2_resi, s.node.ports.hy2_resi_hop);
    let split = split_of(&s);
    let mut seen = std::collections::BTreeSet::new();
    for u in USERS {
        if !s.users.iter().any(|x| x.username == u) {
            continue;
        }
        let nodes = nodes_of(&s, u);
        let Some(resi) = nodes
            .iter()
            .find(|n| n.kind == bui_schema::nodes::NodeKind::Hy2Residential)
        else {
            continue; // 没有住宅权益（或没开 hysteria2）的用户
        };
        assert_eq!(resi.port, port, "user={u}");
        assert_eq!(resi.hop, Some(hop), "user={u}");
        seen.insert((resi.port, resi.hop));

        // URI 列表里出现的就是这一对固定值
        let text = String::from_utf8(
            base64::engine::general_purpose::STANDARD
                .decode(subscription::uri_list(&nodes, u).trim())
                .unwrap(),
        )
        .unwrap();
        assert!(
            text.contains(&format!(":{port}?")),
            "user={u} 的 URI 里没有住宅端口 {port}：\n{text}"
        );
        assert!(
            text.contains(&format!("mport={}-{}", hop.0, hop.1)),
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
        assert_eq!(out["server_port"], port, "user={u}");
        common::check_singbox_all(&sb);
        let yaml = subscription::clash(&nodes, u, &split);
        assert!(
            yaml.contains(&format!("port: {port}")),
            "user={u} 的 clash YAML 里没有住宅端口"
        );
    }
    assert_eq!(seen.len(), 1, "多槽下的端口/区间必须是同一对，不该有第二种");
}

/// 4.1（T14 收口）：**零节点用户的订阅必须整份仍然可用**。
///
/// 谁会零节点：只有住宅权益、只开 hysteria2 的用户（fixture 里的 bob），在凭据池耗尽
/// （`hy2pool::migrate` 报 `unassigned > 0`）或建完用户到下一轮收敛之间那一瞬，
/// `hy2pool::cred_of` 为 `None` ⇒ `nodes_for` 一个节点都不发（spec §3.1）。
///
/// 为什么不是「少一个节点」这么轻：空节点集会渲出 `urltest` 的 `outbounds: []`，而
/// `dns.servers[remote].detour` 与 `route.final` 都指着它，sing-box 直接拒绝加载
/// **整份**配置（实测 1.14.0 先在空 `domain` 项上 FATAL，改掉后仍在空 urltest 上失败）；
/// mihomo 那侧同理（空 `select` 组 + 指向不存在组的 `MATCH`）。于是这个用户连订阅都用
/// 不了、客户端起不来 —— 比「没有住宅节点」严重得多。
#[test]
fn a_user_with_no_nodes_still_gets_a_loadable_subscription() {
    let mut s = common::state("split");
    // bob：单 hysteria2 + 只有住宅（`direct=false`），摘掉他的凭据指针就是「池耗尽」那一瞬
    let bob = s
        .users
        .iter_mut()
        .find(|u| u.username == "bob")
        .expect("fixture 里没有 bob");
    bob.credentials.hy2_resi_cred = None;
    let nodes = nodes_of(&s, "bob");
    assert!(
        nodes.is_empty(),
        "这条用例的前提是零节点，实际 {:?}",
        nodes.iter().map(|n| n.kind).collect::<Vec<_>>()
    );

    let split = split_of(&s);
    let sb = subscription::singbox(&nodes, &split, &s.node.public_ip);
    // 不许渲出 urltest（它的 outbounds 只能是空的），DNS / route 的落点必须真实存在
    let tags: Vec<&str> = sb["outbounds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["tag"].as_str().unwrap())
        .collect();
    for o in sb["outbounds"].as_array().unwrap() {
        assert!(o["type"] != "urltest", "零节点渲出了 urltest：{o}");
    }
    let route_final = sb["route"]["final"].as_str().unwrap();
    assert!(
        tags.contains(&route_final),
        "route.final={route_final} 不存在"
    );
    let detour = sb["dns"]["servers"][0]["detour"].as_str().unwrap();
    assert!(tags.contains(&detour), "dns detour={detour} 不存在");
    for r in sb["dns"]["rules"].as_array().unwrap() {
        if let Some(d) = r["domain"].as_array() {
            assert!(
                !d.iter().any(|x| x.as_str() == Some("")),
                "DNS 规则里有空 domain 项，sing-box 会拒绝加载：{r}"
            );
        }
    }
    // 判据：真内核跑一遍 check（缺内核则 skip）
    common::check_singbox_all(&sb);

    // mihomo 那侧：组不许是空的，`MATCH` 的落点要么是内置出站、要么是真实存在的组
    let yaml = subscription::clash(&nodes, "bob", &split);
    let doc: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
    let groups: Vec<String> = doc["proxy-groups"]
        .as_sequence()
        .unwrap()
        .iter()
        .map(|g| g["name"].as_str().unwrap().to_string())
        .collect();
    for g in doc["proxy-groups"].as_sequence().unwrap() {
        assert!(
            !g["proxies"].as_sequence().unwrap().is_empty(),
            "空的 proxy-group，mihomo 会拒绝加载：{g:?}"
        );
    }
    let m = doc["rules"]
        .as_sequence()
        .unwrap()
        .last()
        .unwrap()
        .as_str()
        .unwrap()
        .strip_prefix("MATCH,")
        .expect("最后一条规则不是 MATCH")
        .to_string();
    assert!(
        ["DIRECT", "REJECT"].contains(&m.as_str()) || groups.contains(&m),
        "MATCH,{m} 指向不存在的组，groups={groups:?}"
    );
}
