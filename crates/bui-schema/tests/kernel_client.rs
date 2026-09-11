//! 客户端 sing-box 配置渲染（TUN schema 8 / mixed）+ 真实内核校验。
mod common;

use bui_schema::render::client::{self, ClientMode, ClientOpts};
use bui_schema::render::SplitRules;

fn copts(mode: ClientMode, v6: bool, split: SplitRules) -> ClientOpts {
    ClientOpts {
        mode,
        socks_port: 1080,
        http_port: 8080,
        host_has_ipv6: v6,
        split,
    }
}

#[test]
fn tun_config_matches_schema8_and_checks() {
    let s = common::state("global");
    let u = &s.users[0];
    let nodes = bui_schema::nodes::nodes_for(u, &s.node, &s.residential);
    let split = SplitRules::from_group(s.residential.default_group().unwrap());
    assert_eq!(nodes.len(), 4, "alice 是 fusion + 住宅");
    for n in &nodes {
        let cfg = client::tun_config(n, &copts(ClientMode::Tun, true, split.clone()));
        let tun = &cfg["inbounds"][0];
        assert_eq!(tun["type"], "tun");
        assert_eq!(tun["interface_name"], "bui-tun");
        assert_eq!(tun["stack"], "mixed");
        assert_eq!(tun["auto_route"], true);
        assert_eq!(tun["strict_route"], true);
        assert!(
            tun["address"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a.as_str().unwrap().contains(':')),
            "v6 主机接管 ::/0"
        );
        let rules = cfg["route"]["rules"].as_array().unwrap();
        assert!(rules.iter().any(|r| r["action"] == "hijack-dns"));
        assert!(rules
            .iter()
            .any(|r| r["ip_version"] == 6 && r["action"] == "reject"));
        assert_eq!(cfg["route"]["final"], "proxy-out");
        assert!(cfg
            .get("route")
            .unwrap()
            .get("default_domain_resolver")
            .is_some());
        assert_eq!(cfg["dns"]["strategy"], "ipv4_only");
        assert!(!serde_json::to_string(&cfg).unwrap().contains("rule_set"));
        common::check_singbox(&cfg);

        let cfg4 = client::tun_config(n, &copts(ClientMode::Tun, false, split.clone()));
        assert!(cfg4["inbounds"][0]["address"]
            .as_array()
            .unwrap()
            .iter()
            .all(|a| !a.as_str().unwrap().contains(':')));
        common::check_singbox(&cfg4);
    }
}

#[test]
fn tun_outbound_follows_node_transport() {
    let s = common::state("obfs");
    let u = &s.users[0];
    let nodes = bui_schema::nodes::nodes_for(u, &s.node, &s.residential);
    let split = SplitRules::from_group(s.residential.default_group().unwrap());
    let cfg = client::tun_config(&nodes[0], &copts(ClientMode::Tun, false, split.clone()));
    let o = &cfg["outbounds"][0];
    assert_eq!(o["type"], "vless");
    assert_eq!(o["tag"], "proxy-out");
    assert_eq!(o["server"], "example.com");
    assert_eq!(o["server_port"], 10001);
    assert_eq!(o["flow"], "xtls-rprx-vision");
    assert_eq!(o["tls"]["server_name"], "www.bing.com");
    assert_eq!(o["tls"]["utls"]["fingerprint"], "chrome");
    assert_eq!(o["tls"]["reality"]["short_id"], "0123456789abcdef");
    assert_eq!(o["domain_resolver"], "local-dns");

    // HY2 直连：密码 = username:password，端口跳跃 + salamander obfs
    let cfg = client::tun_config(&nodes[2], &copts(ClientMode::Tun, false, split.clone()));
    let o = &cfg["outbounds"][0];
    assert_eq!(o["type"], "hysteria2");
    assert_eq!(o["server_port"], 10000);
    assert_eq!(o["password"], "alice:pw-alice-01");
    assert_eq!(o["server_ports"], serde_json::json!(["20000:30000"]));
    assert_eq!(o["hop_interval"], "30s");
    assert_eq!(o["obfs"]["type"], "salamander");
    assert_eq!(o["obfs"]["password"], "obfs-pw-test");
    assert_eq!(o["tls"]["server_name"], "example.com");
    assert_eq!(o["tls"]["insecure"], false);
    common::check_singbox(&cfg);

    // HY2 住宅：v3 语义不带 obfs
    let cfg = client::tun_config(&nodes[3], &copts(ClientMode::Tun, false, split));
    assert!(cfg["outbounds"][0].get("obfs").is_none());
    assert_eq!(
        cfg["outbounds"][0]["server_ports"],
        serde_json::json!(["41000:50000"])
    );
    common::check_singbox(&cfg);
}

#[test]
fn residential_node_gets_keyword_split_before_cn_direct() {
    let s = common::state("split");
    let u = &s.users[0];
    let nodes = bui_schema::nodes::nodes_for(u, &s.node, &s.residential);
    let split = SplitRules::from_group(s.residential.default_group().unwrap());
    let kw = serde_json::json!({
        "domain_keyword": split.keywords.clone(),
        "outbound": "proxy-out",
    });

    // 住宅节点：关键字规则存在且排在国内域名直连之前
    let cfg = client::tun_config(&nodes[1], &copts(ClientMode::Tun, false, split.clone()));
    let rules = cfg["route"]["rules"].as_array().unwrap();
    let at = rules
        .iter()
        .position(|r| *r == kw)
        .expect("住宅关键字分流规则");
    let cn = rules
        .iter()
        .position(|r| r["outbound"] == "direct-out" && r.get("domain_suffix").is_some())
        .expect("国内域名直连规则");
    assert!(at < cn, "住宅关键字优先于 cn 直连");
    common::check_singbox(&cfg);

    // 直连节点：不加该规则
    let cfg = client::tun_config(&nodes[0], &copts(ClientMode::Tun, false, split));
    assert!(!cfg["route"]["rules"].as_array().unwrap().contains(&kw));
}

#[test]
fn mixed_config_has_two_mixed_inbounds() {
    let s = common::state("global");
    let u = &s.users[0];
    let n = &bui_schema::nodes::nodes_for(u, &s.node, &s.residential)[2];
    let cfg = client::mixed_config(
        n,
        &copts(
            ClientMode::Mixed,
            true,
            SplitRules::from_group(s.residential.default_group().unwrap()),
        ),
    );
    let inb = cfg["inbounds"].as_array().unwrap();
    assert_eq!(inb.len(), 2);
    assert_eq!(inb[0]["type"], "mixed");
    assert_eq!(inb[0]["listen_port"], 1080);
    assert_eq!(inb[1]["listen_port"], 8080);
    assert!(inb.iter().all(|i| i["type"] != "tun"));
    // 无 DNS 劫持
    assert!(!cfg["route"]["rules"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["action"] == "hijack-dns"));
    assert_eq!(cfg["route"]["final"], "proxy-out");
    common::check_singbox(&cfg);
}
