//! relay 配置渲染：精确槽位、住宅出口策略、拒绝退路，并用真实内核校验。
mod common;

use bui_schema::model::{Pin, Rule, UpstreamKind};
use bui_schema::relay_policy;
use bui_schema::render::relay::{self, RelayOpts};

fn opts() -> RelayOpts {
    RelayOpts {
        listen_port: 2080,
        api: "127.0.0.1:9091".into(),
        cache_path: "/opt/b-ui/relay-cache.db".into(),
        server_ip: Some("203.0.113.10".into()),
    }
}

fn slots(g: &bui_schema::model::ResidentialGroup) -> Vec<bui_schema::model::Slot> {
    g.upstreams
        .iter()
        .enumerate()
        .map(|(index, upstream)| bui_schema::model::Slot {
            index: index as u16,
            upstream_id: upstream.id,
        })
        .collect()
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
    let slots: Vec<bui_schema::model::Slot> = g
        .upstreams
        .iter()
        .enumerate()
        .map(|(i, u)| bui_schema::model::Slot {
            index: i as u16,
            upstream_id: u.id,
        })
        .collect();
    let cfg = relay::config(&g, &slots, &opts());

    let tags: Vec<_> = cfg["outbounds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["tag"].as_str().unwrap().to_string())
        .filter(|tag| !tag.starts_with("resi-egress-"))
        .collect();
    assert_eq!(
        tags,
        vec![
            "resi-1",
            "resi-2",
            "slot-0-pool",
            "slot-1-pool",
            "resi-pool",
            "direct"
        ]
    );
    let raw = |id| {
        cfg["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["tag"] == relay_policy::egress_tag(id))
            .unwrap()
    };
    assert_eq!(raw(g.upstreams[0].id)["type"], "http");
    assert_eq!(raw(g.upstreams[1].id)["type"], "socks");
    assert_eq!(raw(g.upstreams[1].id)["version"], "5");
    assert_eq!(cfg["route"]["final"], "resi-pool");

    let rules = cfg["route"]["rules"].as_array().unwrap();
    assert_eq!(rules[0]["action"], "sniff");
    assert_eq!(rules[1]["domain_suffix"][0], "pay.google.com");
    assert_eq!(rules[1]["action"], "reject");
    assert!(rules[1].get("outbound").is_none());
    let ports = rules
        .iter()
        .find(|r| r.get("port_range").is_some())
        .unwrap();
    assert_eq!(
        ports["inbound"],
        serde_json::json!([relay_policy::inbound_tag(g.upstreams[0].id)])
    );
    assert_eq!(
        ports["port_range"],
        serde_json::json!(["1:79", "81:442", "444:65535"])
    );
    assert_eq!(ports["action"], "reject");
    assert!(ports.get("outbound").is_none());
    assert!(rules
        .iter()
        .any(|r| r["network"] == "udp" && r.get("port").is_none() && r["action"] == "reject"));
    // 本机公网 IP 与私网业务不能变成 VPS 直连。
    assert!(rules.iter().any(|r| r["action"] == "reject"
        && r["ip_cidr"]
            .as_array()
            .map(|a| a.iter().any(|c| c == "203.0.113.10/32"))
            .unwrap_or(false)));
    // global 模式没有 domain_keyword 分流（全部走住宅）
    assert!(rules.iter().all(|r| r.get("domain_keyword").is_none()));

    assert_eq!(cfg["dns"]["rules"], serde_json::json!([]));
    assert_eq!(cfg["dns"]["final"], "dns_resi");
    assert_eq!(
        rules.last().unwrap(),
        &serde_json::json!({"action":"reject"})
    );

    common::check_singbox(&cfg);
}

#[test]
fn relay_split_keeps_every_authorized_slot_on_residential_egress() {
    let s = common::state("split");
    let g = s.residential.default_group().unwrap().clone();
    let cfg = relay::config(&g, &slots(&g), &opts());
    let rules = cfg["route"]["rules"].as_array().unwrap();
    // legacy split 关键字不再把住宅入口的其余业务改成 VPS 直连；嗅探仍用于策略匹配。
    assert_eq!(
        rules[0]["action"], "sniff",
        "rules[0] 必须是 sniff（入口策略匹配前提）"
    );
    assert_eq!(cfg["route"]["final"], "resi-pool");
    for index in [0, 1] {
        assert!(rules.iter().any(|rule| rule["inbound"]
            == serde_json::json!([format!("slot-{index}")])
            && rule["outbound"] == format!("slot-{index}-pool")
            && rule.get("domain_keyword").is_none()));
    }
    assert!(rules.iter().all(|r| r.get("domain_keyword").is_none()));
    assert_eq!(
        rules.last().unwrap(),
        &serde_json::json!({"action":"reject"})
    );
    assert_eq!(cfg["dns"]["rules"], serde_json::json!([]));
    assert_eq!(cfg["dns"]["final"], "dns_resi");
    common::check_singbox(&cfg);
}

#[test]
fn relay_blacklist_keeps_pins_global_and_auto_with_each_actual_upstream() {
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

    let cfg = relay::config(&g, &slots(&g), &opts());
    let rules = cfg["route"]["rules"].as_array().unwrap();
    assert_eq!(rules[1]["domain"], serde_json::json!(["www.paypal.com"]));
    assert!(rules[1].get("domain_suffix").is_none());
    assert_eq!(
        rules[1]["inbound"],
        serde_json::json!(["slot-0", "slot-1"]),
        "人工 pins 对用户入口生效"
    );
    let auto = rules
        .iter()
        .find(|r| r["domain_suffix"] == serde_json::json!(["gateway.icloud.com"]))
        .unwrap();
    assert_eq!(
        auto["inbound"],
        serde_json::json!([relay_policy::inbound_tag(selected)])
    );
    assert_eq!(auto["action"], "reject");
    let port = rules
        .iter()
        .find(|r| r["port"] == serde_json::json!([5228]))
        .unwrap();
    assert_eq!(
        port["inbound"],
        serde_json::json!([relay_policy::inbound_tag(selected)])
    );
    assert_eq!(port["action"], "reject");
    let other_auto = rules
        .iter()
        .find(|r| r["domain_suffix"] == serde_json::json!(["other.example.net"]))
        .unwrap();
    assert_eq!(
        other_auto["inbound"],
        serde_json::json!([relay_policy::inbound_tag(other)])
    );
    assert_eq!(other_auto["action"], "reject");
    assert!(!serde_json::to_string(&cfg["dns"])
        .unwrap()
        .contains("gateway.icloud.com"));
    assert!(!serde_json::to_string(&cfg["dns"])
        .unwrap()
        .contains("other.example.net"));
    common::check_singbox(&cfg);
}

/// 全 socks5 池具有 UDP 能力，业务 DNS 与其余 UDP 均保持住宅出口。
#[test]
fn relay_all_socks5_pool_lets_udp_reach_the_pool() {
    let s = common::state("global");
    let mut g = s.residential.default_group().unwrap().clone();
    for u in &mut g.upstreams {
        u.kind = UpstreamKind::Socks5;
    }
    let cfg = relay::config(&g, &slots(&g), &opts());
    let rules = cfg["route"]["rules"].as_array().unwrap();
    assert!(
        !udp_dns_direct(rules),
        "客户端明文 DNS 不允许改成 VPS 出口：{rules:#?}"
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
    common::check_singbox(&cfg);
}

/// 混合池有 HTTP 成员，能力不足的 UDP 明确拒绝。
#[test]
fn relay_mixed_pool_rejects_all_business_udp() {
    let s = common::state("global");
    let g = s.residential.default_group().unwrap().clone();
    assert!(g.upstreams.iter().any(|u| u.kind == UpstreamKind::Http));
    assert!(g.upstreams.iter().any(|u| u.kind == UpstreamKind::Socks5));
    let cfg = relay::config(&g, &slots(&g), &opts());
    let rules = cfg["route"]["rules"].as_array().unwrap();
    assert!(!udp_dns_direct(rules));
    assert!(rules
        .iter()
        .any(|r| r["network"] == "udp" && r.get("port").is_none() && r["action"] == "reject"));
    assert!(!udp_catch_all_direct(rules));
    assert!(
        udp_resolve_pos(rules).is_none(),
        "混合池的 UDP 不进 socks 出站，不需要先解析：{rules:#?}"
    );
    common::check_singbox(&cfg);
}

/// 全 HTTP 池同样拒绝 UDP，包含业务 DNS。
#[test]
fn relay_all_http_pool_rejects_all_business_udp() {
    let s = common::state("split");
    let mut g = s.residential.default_group().unwrap().clone();
    for u in &mut g.upstreams {
        u.kind = UpstreamKind::Http;
    }
    let cfg = relay::config(&g, &slots(&g), &opts());
    let rules = cfg["route"]["rules"].as_array().unwrap();
    assert!(!udp_dns_direct(rules));
    assert!(rules
        .iter()
        .any(|r| r["network"] == "udp" && r.get("port").is_none() && r["action"] == "reject"));
    assert!(!udp_catch_all_direct(rules));
    assert!(
        udp_resolve_pos(rules).is_none(),
        "全 http 池的 UDP 不进 socks 出站，不需要先解析：{rules:#?}"
    );
    common::check_singbox(&cfg);
}

/// UDP 域名要先经过实际出口 auto，再解析成 IPv4；因此 resolve 属于 policy 入站。
#[test]
fn relay_all_socks5_pool_resolves_udp_after_policy_matching_before_raw_egress() {
    for mode in ["global", "split"] {
        let s = common::state(mode);
        let mut g = s.residential.default_group().unwrap().clone();
        for u in &mut g.upstreams {
            u.kind = UpstreamKind::Socks5;
        }
        let slots: Vec<bui_schema::model::Slot> = g
            .upstreams
            .iter()
            .enumerate()
            .map(|(i, u)| bui_schema::model::Slot {
                index: i as u16,
                upstream_id: u.id,
            })
            .collect();
        let cfg = relay::config(&g, &slots, &opts());
        let rules = cfg["route"]["rules"].as_array().unwrap();
        let resolves: Vec<_> = rules.iter().filter(|r| r["action"] == "resolve").collect();
        assert_eq!(
            resolves.len(),
            g.upstreams.len(),
            "每个真实出口各自解析，外层保留域名"
        );
        for u in &g.upstreams {
            let inbound = serde_json::json!([relay_policy::inbound_tag(u.id)]);
            let resolve = rules
                .iter()
                .position(|r| r["action"] == "resolve" && r["inbound"] == inbound)
                .unwrap();
            assert_eq!(rules[resolve]["network"], "udp", "TCP 域名仍交给上游");
            let server = format!("dns-resi-{}", u.id);
            assert_eq!(rules[resolve]["server"], server);
            assert_eq!(rules[resolve]["strategy"], "ipv4_only");
            let dns = cfg["dns"]["servers"]
                .as_array()
                .unwrap()
                .iter()
                .find(|dns| dns["tag"] == server)
                .unwrap();
            assert_eq!(dns["type"], "tcp");
            assert_eq!(dns["detour"], relay_policy::egress_tag(u.id));
            let private = rules
                .iter()
                .position(|r| r.get("ip_cidr").is_some() && r["inbound"] == inbound)
                .unwrap();
            let raw = rules
                .iter()
                .position(|r| {
                    r["inbound"] == inbound
                        && r.get("network").is_none()
                        && r["outbound"] == relay_policy::egress_tag(u.id)
                })
                .unwrap();
            assert!(
                resolve < private && private < raw,
                "解析之后仍须挡住私网目标"
            );
        }
        common::check_singbox(&cfg);
    }
}

#[test]
fn business_dns_obeys_supplier_ports_and_internal_dns_uses_supplier_detour() {
    let mut g = common::state("global")
        .residential
        .default_group()
        .unwrap()
        .clone();
    for u in &mut g.upstreams {
        u.kind = UpstreamKind::Socks5;
        u.ports_allowed = Some(vec![80, 443]);
    }
    let cfg = relay::config(&g, &slots(&g), &opts());
    let rules = cfg["route"]["rules"].as_array().unwrap();
    assert!(!udp_dns_direct(rules));
    for u in &g.upstreams {
        let inbound = serde_json::json!([relay_policy::inbound_tag(u.id)]);
        let ports = rules
            .iter()
            .find(|r| r["inbound"] == inbound && r.get("port_range").is_some())
            .unwrap();
        assert_eq!(ports["action"], "reject");
        assert_eq!(
            ports["port_range"][0], "1:79",
            "业务 DNS/53 必须受端口策略约束"
        );
        let server = format!("dns-resi-{}", u.id);
        let dns = cfg["dns"]["servers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|dns| dns["tag"] == server)
            .unwrap();
        assert_eq!(dns["type"], "tcp");
        assert_eq!(dns["server"], "8.8.8.8");
        assert_eq!(dns["detour"], relay_policy::egress_tag(u.id));
    }
    common::check_singbox(&cfg);
}

fn udp_resolve_pos(rules: &[serde_json::Value]) -> Option<usize> {
    rules
        .iter()
        .position(|r| r["network"] == "udp" && r["action"] == "resolve")
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
fn relay_empty_pool_rejects_before_infrastructure_direct_final() {
    let g = bui_schema::model::ResidentialGroup {
        enabled: true,
        ..Default::default()
    };
    let cfg = relay::config(&g, &[], &opts());
    assert_eq!(cfg["route"]["final"], "direct");
    assert!(cfg["outbounds"]
        .as_array()
        .unwrap()
        .iter()
        .all(|o| o["tag"] != "resi-pool"));
    assert_eq!(cfg["dns"]["final"], "dns_direct");
    assert_eq!(
        cfg["route"]["rules"].as_array().unwrap().last().unwrap(),
        &serde_json::json!({"action":"reject"})
    );
    assert!(cfg["route"]["rules"]
        .as_array()
        .unwrap()
        .iter()
        .all(|rule| rule.get("outbound").is_none()));
    common::check_singbox(&cfg);
}

#[test]
fn relay_disabled_group_rejects_before_infrastructure_direct_final() {
    let s = common::state("global");
    let mut g = s.residential.default_group().unwrap().clone();
    g.enabled = false;
    let cfg = relay::config(&g, &[], &opts());
    assert_eq!(cfg["route"]["final"], "direct");
    assert_eq!(cfg["dns"]["final"], "dns_direct");
    assert_eq!(
        cfg["route"]["rules"].as_array().unwrap().last().unwrap(),
        &serde_json::json!({"action":"reject"})
    );
    assert!(cfg["route"]["rules"]
        .as_array()
        .unwrap()
        .iter()
        .all(|rule| rule.get("outbound").is_none()));
    assert!(!serde_json::to_string(&cfg)
        .unwrap()
        .contains("isp.example.net"));
    common::check_singbox(&cfg);
}

/// 多槽配置必须在发布内核上过 `sing-box check`：每槽一个 socks 入站、
/// 每槽一个 selector、按 `inbound` 分流 —— 这三样都是 `check` 会严格校验的字段。
#[test]
fn a_three_slot_relay_passes_singbox_check_on_the_target_kernel() {
    let s = common::state("global");
    let mut g = s.residential.default_group().unwrap().clone();
    // fixture 自带 2 条上游，补到 3 条
    let mut third = g.upstreams[0].clone();
    third.id = uuid::Uuid::from_u128(0xdead);
    third.host = "isp3.example.net".into();
    third.kind = bui_schema::model::UpstreamKind::Socks5;
    g.upstreams.push(third);
    let slots: Vec<bui_schema::model::Slot> = g
        .upstreams
        .iter()
        .enumerate()
        .map(|(i, u)| bui_schema::model::Slot {
            index: i as u16,
            upstream_id: u.id,
        })
        .collect();
    let cfg = relay::config(&g, &slots, &opts());
    assert_eq!(cfg["inbounds"].as_array().unwrap().len(), 6);
    common::check_singbox(&cfg);
}
