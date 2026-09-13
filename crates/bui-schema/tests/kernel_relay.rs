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
    let cfg = relay::config(&g, &[], &opts());
    assert_eq!(cfg["route"]["final"], "direct");
    let last = cfg["route"]["rules"].as_array().unwrap().last().unwrap();
    assert_eq!(last["outbound"], "slot-0-pool");
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

    let cfg = relay::config(&g, &[], &opts());
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
    let cfg = relay::config(&g, &[], &opts());
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
    let cfg = relay::config(&g, &[], &opts());
    let rules = cfg["route"]["rules"].as_array().unwrap();
    assert!(udp_dns_direct(rules));
    assert!(rules
        .iter()
        .any(|r| r["network"] == "udp" && r["port"] == 443 && r["action"] == "reject"));
    assert!(udp_catch_all_direct(rules));
    assert!(
        udp_resolve_pos(rules).is_none(),
        "混合池的 UDP 不进 socks 出站，不需要先解析：{rules:#?}"
    );
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
    let cfg = relay::config(&g, &[], &opts());
    let rules = cfg["route"]["rules"].as_array().unwrap();
    assert!(udp_dns_direct(rules));
    assert!(rules
        .iter()
        .any(|r| r["network"] == "udp" && r["port"] == 443 && r["action"] == "reject"));
    assert!(udp_catch_all_direct(rules));
    assert!(
        udp_resolve_pos(rules).is_none(),
        "全 http 池的 UDP 不进 socks 出站，不需要先解析：{rules:#?}"
    );
    common::check_singbox_all(&cfg);
}

/// 全 socks5 池：UDP 目标先在本机解析成 IPv4 再进 socks 出站。
///
/// 2026-09-13 实测（经 Decodo SOCKS5）：UDP ASSOCIATE 的请求地址只收 ATYP=1（IPv4），
/// ATYP=3（域名）与 ATYP=4（IPv6）都回 code=8；而 sing-box 的 socks 出站会把 sniff
/// 出的域名原样当 UDP 目标发出（relay 日志 24h 823 条 `request rejected, code=8`）。
/// 这条 resolve 在 sniff 与 UDP/53 直连之后、各槽 inbound 规则之前，全表只一条；
/// 只作用于 UDP，TCP 的域名照旧原样交给上游。
#[test]
fn relay_all_socks5_pool_resolves_udp_targets_to_ipv4_before_the_slot_rules() {
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
        let at = udp_resolve_pos(rules)
            .unwrap_or_else(|| panic!("{mode}：缺 UDP → IPv4 的 resolve：{rules:#?}"));
        let sniff = rules.iter().position(|r| r["action"] == "sniff").unwrap();
        let dns53 = rules
            .iter()
            .position(|r| r["network"] == "udp" && r["port"] == 53)
            .unwrap();
        let first_slot = rules
            .iter()
            .position(|r| r.get("inbound").is_some())
            .unwrap();
        assert!(sniff < at, "{mode}：resolve 要在 sniff 之后：{rules:#?}");
        assert!(dns53 < at, "{mode}：UDP/53 直连不必先解析：{rules:#?}");
        assert!(
            at < first_slot,
            "{mode}：resolve 要在各槽 inbound 规则之前：{rules:#?}"
        );
        let resolves: Vec<_> = rules.iter().filter(|r| r["action"] == "resolve").collect();
        assert_eq!(resolves.len(), 1, "{mode}：只一条，不按槽重复：{rules:#?}");
        assert_eq!(resolves[0]["network"], "udp", "{mode}：TCP 不解析");
        common::check_singbox_all(&cfg);
    }
}

/// 「UDP 目标先在本机解析成 IPv4」那条 resolve 的位置
fn udp_resolve_pos(rules: &[serde_json::Value]) -> Option<usize> {
    rules.iter().position(|r| {
        *r == serde_json::json!({
            "network": "udp",
            "action": "resolve",
            "server": "dns_direct",
            "strategy": "ipv4_only"
        })
    })
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
    let cfg = relay::config(&g, &[], &opts());
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
    let cfg = relay::config(&g, &[], &opts());
    assert_eq!(cfg["route"]["final"], "direct");
    assert_eq!(cfg["dns"]["final"], "dns_direct");
    assert!(!serde_json::to_string(&cfg)
        .unwrap()
        .contains("isp.example.net"));
    common::check_singbox(&cfg);
}

/// 多槽配置必须在 1.12 / 1.13 / 1.14 三版上都过 `sing-box check`：每槽一个 socks 入站、
/// 每槽一个 selector、按 `inbound` 分流 —— 这三样都是 `check` 会严格校验的字段。
#[test]
fn a_three_slot_relay_passes_singbox_check_on_all_versions() {
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
    assert_eq!(cfg["inbounds"].as_array().unwrap().len(), 3);
    common::check_singbox_all(&cfg);
}
