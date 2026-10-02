//! Literal contract assertions for the residence-only relay. Runtime payload tests
//! separately verify that these rejection decisions cannot reach a VPS receiver.
use bui_schema::model::{ResidentialGroup, Slot};
use bui_schema::relay_generation::Bank;
use bui_schema::render::relay::{config, generation, RelayOpts};
use serde_json::{json, Value};

fn group() -> ResidentialGroup {
    serde_json::from_value(json!({
        "enabled": true, "mode": "split", "keywords": ["only-selected"],
        "upstreams": [{"id":"00000000-0000-0000-0000-000000000001", "name":"A",
            "kind":"socks5", "host":"provider.invalid", "port":1080}],
        "blacklist": {"pins": [{"rule":{"kind":"domain","value":"pinned.invalid"},"note":"","created_at":""}]}
    })).unwrap()
}
fn opts() -> RelayOpts {
    RelayOpts {
        listen_port: 2080,
        api: "127.0.0.1:9091".into(),
        cache_path: "/tmp/contract.db".into(),
        server_ip: Some("203.0.113.9".into()),
    }
}
fn slots(g: &ResidentialGroup) -> Vec<Slot> {
    vec![Slot {
        index: 0,
        upstream_id: g.upstreams[0].id,
    }]
}
fn rules(cfg: &Value) -> &[Value] {
    cfg["route"]["rules"].as_array().unwrap()
}

#[test]
fn disabled_and_empty_residence_endpoints_end_in_reject() {
    let mut disabled = group();
    disabled.enabled = false;
    let mut empty = group();
    empty.upstreams.clear();
    for g in [disabled, empty] {
        let cfg = config(&g, &[], &opts());
        assert_eq!(
            rules(&cfg).last(),
            Some(&json!({"action":"reject"})),
            "unavailable residence must reject every payload"
        );
        assert!(rules(&cfg).iter().all(|r| r["outbound"] != "direct"));
    }
}
#[test]
fn legacy_split_does_not_allow_public_payloads_to_escape_residence() {
    let g = group();
    let cfg = config(&g, &slots(&g), &opts());
    let slot = rules(&cfg)
        .iter()
        .find(|r| r["outbound"] == "slot-0-pool")
        .unwrap();
    assert_eq!(
        slot,
        &json!({"inbound":["slot-0"],"outbound":"slot-0-pool"})
    );
    assert_eq!(rules(&cfg).last(), Some(&json!({"action":"reject"})));
    assert!(rules(&cfg).iter().all(|r| r["outbound"] != "direct"));
}
#[test]
fn restrictions_reject_instead_of_switching_to_vps() {
    let mut g = group();
    g.upstreams[0].ports_allowed = Some(vec![443]);
    let cfg = config(&g, &slots(&g), &opts());
    assert!(rules(&cfg)
        .iter()
        .any(|r| r["domain"] == json!(["pinned.invalid"]) && r["action"] == "reject"));
    assert!(rules(&cfg)
        .iter()
        .any(|r| r["port_range"] == json!(["1:442", "444:65535"]) && r["action"] == "reject"));
    for private in rules(&cfg).iter().filter(|r| r.get("ip_cidr").is_some()) {
        assert_eq!(private["action"], "reject");
    }
    assert!(rules(&cfg).iter().all(|r| r["outbound"] != "direct"));
}
#[test]
fn mixed_http_pool_rejects_all_business_udp_including_dns() {
    let mut g = group();
    g.upstreams[0].kind = bui_schema::model::UpstreamKind::Http;
    let cfg = config(&g, &slots(&g), &opts());
    let udp = rules(&cfg)
        .iter()
        .find(|r| r["inbound"] == json!(["slot-0"]) && r["network"] == "udp")
        .unwrap();
    assert_eq!(
        udp,
        &json!({"inbound":["slot-0"],"network":"udp","action":"reject"})
    );
    assert!(rules(&cfg)
        .iter()
        .all(|r| !(r["network"] == "udp" && r["outbound"] == "direct")));
}
#[test]
fn business_dns_uses_the_selected_provider_and_bootstrap_is_separate() {
    let g = group();
    let cfg = config(&g, &slots(&g), &opts());
    assert_eq!(cfg["dns"]["final"], "dns_resi");
    let dns = cfg["dns"]["servers"].as_array().unwrap();
    assert!(dns
        .iter()
        .any(|s| s
            == &json!({"tag":"dns_resi","type":"tcp","server":"8.8.8.8","detour":"resi-pool"})));
    let resolve = rules(&cfg)
        .iter()
        .find(|r| r["action"] == "resolve")
        .unwrap();
    assert_eq!(
        resolve["server"],
        "dns-resi-00000000-0000-0000-0000-000000000001"
    );
    assert!(dns.iter().any(|s| s["tag"] == resolve["server"]
        && s["detour"] == "resi-egress-00000000-0000-0000-0000-000000000001"));
    let raw = cfg["outbounds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["tag"] == "resi-egress-00000000-0000-0000-0000-000000000001")
        .unwrap();
    assert_eq!(raw["domain_resolver"], "dns_direct");
}
#[test]
fn missing_slot_never_synthesizes_an_authorized_fallback_route() {
    let g = group();
    let cfg = config(&g, &[], &opts());
    assert!(rules(&cfg).iter().all(|r| r["outbound"] != "slot-0-pool"));
    assert_eq!(rules(&cfg).last(), Some(&json!({"action":"reject"})));
}

#[test]
fn duplicate_out_of_range_and_vanished_slot_bindings_have_no_authorized_route() {
    let g = group();
    let id = g.upstreams[0].id;
    for slots in [
        vec![
            Slot {
                index: 0,
                upstream_id: id,
            },
            Slot {
                index: 0,
                upstream_id: id,
            },
        ],
        vec![Slot {
            index: 8,
            upstream_id: id,
        }],
        vec![Slot {
            index: 0,
            upstream_id: uuid::Uuid::from_u128(99),
        }],
    ] {
        let cfg = config(&g, &slots, &opts());
        assert!(rules(&cfg)
            .iter()
            .all(|rule| rule["outbound"] != "slot-0-pool"));
        assert_eq!(rules(&cfg).last(), Some(&json!({"action":"reject"})));
    }
}

#[test]
fn the_servers_ipv6_address_is_a_private_payload_rejection() {
    let g = group();
    let mut options = opts();
    options.server_ip = Some("2001:db8::9".into());
    let cfg = config(&g, &slots(&g), &options);
    assert!(rules(&cfg).iter().any(|rule| {
        rule["action"] == "reject"
            && rule["ip_cidr"]
                .as_array()
                .is_some_and(|cidrs| cidrs.contains(&json!("2001:db8::9/128")))
    }));
}
#[test]
fn generation_front_and_backend_inherit_the_same_required_contract() {
    let g = group();
    let front = generation::front_config(
        &g,
        &slots(&g),
        &generation::FrontOpts {
            relay: opts(),
            bank: Bank::A,
        },
    )
    .unwrap();
    let back = generation::backend_config(
        &g,
        &generation::BackendOpts {
            bank: Bank::A,
            generation: 7,
            server_ip: None,
        },
    )
    .unwrap();
    for cfg in [front, back] {
        assert_eq!(rules(&cfg).last(), Some(&json!({"action":"reject"})));
        assert!(rules(&cfg).iter().all(|r| r["outbound"] != "direct"));
    }
}

#[test]
fn backend_resolver_does_not_persist_the_fronts_mutable_global_selection() {
    let mut g = group();
    let mut second = g.upstreams[0].clone();
    second.id = uuid::Uuid::from_u128(2);
    g.upstreams.push(second);
    let options = generation::BackendOpts {
        bank: Bank::A,
        generation: 7,
        server_ip: None,
    };
    g.selected_upstream_id = Some(g.upstreams[0].id);
    let before = generation::backend_config(&g, &options).unwrap();
    g.selected_upstream_id = Some(g.upstreams[1].id);
    let after = generation::backend_config(&g, &options).unwrap();
    assert_eq!(
        before, after,
        "global selection belongs to the front; provider-specific DNS stays with its supplier"
    );
}
