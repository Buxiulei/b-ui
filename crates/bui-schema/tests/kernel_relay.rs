//! relay（sing-box 本地中继）配置渲染：规则顺序、黑名单、端口白名单、fail-open，并用真实内核校验。
mod common;

use bui_schema::model::{Pin, Rule, UpstreamKind};
use bui_schema::render::relay::{self, RelayOpts};

fn opts() -> RelayOpts {
    RelayOpts {
        listen_port: 2080,
        api: "127.0.0.1:9091".into(),
        cache_path: "/opt/b-ui/relay-cache.db".into(),
        server_ip: Some("203.0.113.10".into()),
    }
}

#[test]
fn relay_global_with_blacklist_and_ports_allowed() {
    let s = common::state("global");
    let mut g = s.residential.default_group().unwrap().clone();
    g.upstreams[0].ports_allowed = Some(vec![80, 443]);
    g.blacklist.pins.push(Pin {
        rule: Rule::DomainSuffix("pay.google.com".into()),
        note: "".into(),
        created_at: "2026-09-11T00:00:00Z".into(),
    });
    let cfg = relay::config(&g, &opts());

    let tags: Vec<_> = cfg["outbounds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["tag"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(tags, vec!["resi-1", "resi-2", "resi-pool", "direct"]);
    assert_eq!(cfg["outbounds"][0]["type"], "http");
    assert_eq!(cfg["outbounds"][1]["type"], "socks");
    assert_eq!(cfg["outbounds"][1]["version"], "5");
    assert_eq!(cfg["route"]["final"], "resi-pool");

    let rules = cfg["route"]["rules"].as_array().unwrap();
    assert_eq!(rules[0]["action"], "sniff");
    assert_eq!(rules[1]["domain_suffix"][0], "pay.google.com");
    assert_eq!(rules[1]["outbound"], "direct");
    assert_eq!(
        rules[2]["port_range"],
        serde_json::json!(["1:79", "81:442", "444:65535"])
    );
    assert_eq!(rules[2]["outbound"], "direct");
    assert!(rules
        .iter()
        .any(|r| r["network"] == "udp" && r["port"] == 443 && r["action"] == "reject"));
    // 本机公网 IP 与私网一律直连
    assert!(rules.iter().any(|r| r["ip_cidr"]
        .as_array()
        .map(|a| a.iter().any(|c| c == "203.0.113.10/32"))
        .unwrap_or(false)));
    // global 模式没有 domain_keyword 分流（全部走住宅）
    assert!(rules.iter().all(|r| r.get("domain_keyword").is_none()));

    assert_eq!(cfg["dns"]["rules"][0]["domain_suffix"][0], "pay.google.com");
    assert_eq!(cfg["dns"]["rules"][0]["server"], "dns_direct");
    assert_eq!(cfg["dns"]["final"], "dns_resi");

    common::check_singbox(&cfg);
}

#[test]
fn relay_split_uses_keywords_and_direct_final() {
    let s = common::state("split");
    let g = s.residential.default_group().unwrap().clone();
    let cfg = relay::config(&g, &opts());
    assert_eq!(cfg["route"]["final"], "direct");
    let last = cfg["route"]["rules"].as_array().unwrap().last().unwrap();
    assert_eq!(last["outbound"], "resi-pool");
    assert_eq!(
        last["domain_keyword"].as_array().unwrap().len(),
        bui_schema::keywords::DEFAULT_KEYWORDS.len()
    );
    // DNS 镜像：关键字走住宅 DNS，其余直连
    assert_eq!(cfg["dns"]["rules"][0]["server"], "dns_resi");
    assert_eq!(cfg["dns"]["final"], "dns_direct");
    common::check_singbox(&cfg);
}

#[test]
fn relay_blacklist_takes_pins_and_selected_upstream_auto_only() {
    use bui_schema::model::AutoEntry;
    let s = common::state("split");
    let mut g = s.residential.default_group().unwrap().clone();
    let selected = g.selected_upstream_id.unwrap();
    let other = g.upstreams[1].id;
    assert_ne!(selected, other);
    g.blacklist.pins.push(Pin {
        rule: Rule::Domain("www.paypal.com".into()),
        note: "".into(),
        created_at: "2026-09-11T00:00:00Z".into(),
    });
    let entry = |upstream_id, rule| AutoEntry {
        upstream_id,
        rule,
        hits: 3,
        confirmed_at: "2026-09-11T00:00:00Z".into(),
        last_verified_at: "2026-09-11T00:00:00Z".into(),
        passes: 0,
    };
    g.blacklist.auto.push(entry(
        selected,
        Rule::DomainSuffix("gateway.icloud.com".into()),
    ));
    g.blacklist.auto.push(entry(selected, Rule::Port(5228)));
    // 别的上游学到的黑名单不影响当前选中上游
    g.blacklist
        .auto
        .push(entry(other, Rule::DomainSuffix("other.example.net".into())));

    let cfg = relay::config(&g, &opts());
    let rules = cfg["route"]["rules"].as_array().unwrap();
    assert_eq!(rules[1]["domain"], serde_json::json!(["www.paypal.com"]));
    assert_eq!(
        rules[1]["domain_suffix"],
        serde_json::json!(["gateway.icloud.com"])
    );
    assert_eq!(rules[1]["outbound"], "direct");
    assert_eq!(rules[2]["port"], serde_json::json!([5228]));
    assert_eq!(rules[2]["outbound"], "direct");
    assert!(!serde_json::to_string(&cfg)
        .unwrap()
        .contains("other.example.net"));
    common::check_singbox(&cfg);
}

/// 全 socks5 池：UDP ASSOCIATE 可用（2026-09-12 实测 Decodo ISP，QUIC 握手双向 1200 字节通），
/// 所以只保留 UDP/53 直连，其余 UDP 交给 `route.final`（global 时 = resi-pool）。
#[test]
fn relay_all_socks5_pool_lets_udp_reach_the_pool() {
    let s = common::state("global");
    let mut g = s.residential.default_group().unwrap().clone();
    for u in &mut g.upstreams {
        u.kind = UpstreamKind::Socks5;
    }
    let cfg = relay::config(&g, &opts());
    let rules = cfg["route"]["rules"].as_array().unwrap();
    assert!(
        udp_dns_direct(rules),
        "客户端明文 DNS 仍由本机解析：{rules:#?}"
    );
    assert!(
        !rules
            .iter()
            .any(|r| r["network"] == "udp" && r["port"] == 443),
        "UDP/443 不再 reject：{rules:#?}"
    );
    assert!(
        !udp_catch_all_direct(rules),
        "「其余 UDP 直连」那条要删掉（否则 UDP 永远暴露 VPS 自身 IP）：{rules:#?}"
    );
    assert_eq!(cfg["route"]["final"], "resi-pool");
    common::check_singbox_all(&cfg);
}

/// 混合池（fixture 自带 http + socks5）：http 出站没有 UDP 能力，规则保持 v3 三条。
#[test]
fn relay_mixed_pool_keeps_the_three_udp_rules() {
    let s = common::state("global");
    let g = s.residential.default_group().unwrap().clone();
    assert!(g.upstreams.iter().any(|u| u.kind == UpstreamKind::Http));
    assert!(g.upstreams.iter().any(|u| u.kind == UpstreamKind::Socks5));
    let cfg = relay::config(&g, &opts());
    let rules = cfg["route"]["rules"].as_array().unwrap();
    assert!(udp_dns_direct(rules));
    assert!(rules
        .iter()
        .any(|r| r["network"] == "udp" && r["port"] == 443 && r["action"] == "reject"));
    assert!(udp_catch_all_direct(rules));
    common::check_singbox_all(&cfg);
}

/// 全 http 池：同上，三条规则一条不少。
#[test]
fn relay_all_http_pool_keeps_the_three_udp_rules() {
    let s = common::state("split");
    let mut g = s.residential.default_group().unwrap().clone();
    for u in &mut g.upstreams {
        u.kind = UpstreamKind::Http;
    }
    let cfg = relay::config(&g, &opts());
    let rules = cfg["route"]["rules"].as_array().unwrap();
    assert!(udp_dns_direct(rules));
    assert!(rules
        .iter()
        .any(|r| r["network"] == "udp" && r["port"] == 443 && r["action"] == "reject"));
    assert!(udp_catch_all_direct(rules));
    common::check_singbox_all(&cfg);
}

fn udp_dns_direct(rules: &[serde_json::Value]) -> bool {
    rules
        .iter()
        .any(|r| r["network"] == "udp" && r["port"] == 53 && r["outbound"] == "direct")
}

/// 「其余 UDP 一律直连」= 只带 `network: udp` 的那条兜底规则
fn udp_catch_all_direct(rules: &[serde_json::Value]) -> bool {
    rules.iter().any(|r| {
        r["network"] == "udp"
            && r["outbound"] == "direct"
            && r.get("port").is_none()
            && r.get("port_range").is_none()
    })
}

#[test]
fn relay_fail_open_when_pool_empty() {
    let g = bui_schema::model::ResidentialGroup {
        enabled: true,
        ..Default::default()
    };
    let cfg = relay::config(&g, &opts());
    assert_eq!(cfg["route"]["final"], "direct");
    assert!(cfg["outbounds"]
        .as_array()
        .unwrap()
        .iter()
        .all(|o| o["tag"] != "resi-pool"));
    assert_eq!(cfg["dns"]["final"], "dns_direct");
    common::check_singbox(&cfg);
}

#[test]
fn relay_disabled_group_is_direct_only() {
    let s = common::state("global");
    let mut g = s.residential.default_group().unwrap().clone();
    g.enabled = false;
    let cfg = relay::config(&g, &opts());
    assert_eq!(cfg["route"]["final"], "direct");
    assert_eq!(cfg["dns"]["final"], "dns_direct");
    assert!(!serde_json::to_string(&cfg)
        .unwrap()
        .contains("isp.example.net"));
    common::check_singbox(&cfg);
}
