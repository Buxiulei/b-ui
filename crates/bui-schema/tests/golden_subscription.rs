//! 三种订阅渲染器与 v3 golden 样本逐项等价（base64 按行、JSON/YAML 按语义）。
mod common;

use base64::Engine;
use bui_schema::model::State;
use bui_schema::nodes::{nodes_for, Node};
use bui_schema::render::{subscription, SplitRules};
use pretty_assertions::assert_eq;

const USERS: [&str; 4] = ["alice", "bob", "carol", "dave"];
const MODES: [&str; 3] = ["global", "split", "obfs"];

/// 某用户在 golden 里应有的节点集合（v3 单协议只发住宅版，见 `common::v3_nodes_only`）。
fn nodes_of(s: &State, username: &str) -> Vec<Node> {
    let user = s.users.iter().find(|u| u.username == username).unwrap();
    common::v3_nodes_only(nodes_for(user, &s.node, &s.residential), user)
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
