//! Offline compiler contracts. Actual upstream-core validation is a separate root-run gate.
use bui_schema::managed::{
    EgressCapabilities, EgressIdentity, Evidence, EvidenceStatus, ManagedPolicy,
};
use bui_schema::nodes::{Node, NodeKind, Transport};
use bui_schema::render::managed::{compile, ManagedRuntime};
use serde_json::{json, Value};

fn verified() -> EvidenceStatus {
    EvidenceStatus::Verified(Evidence {
        path_fingerprint: "fixture-path".into(),
        observed_at: 100,
        expires_at: 200,
        observed_identity: "fixture-identity".into(),
    })
}
fn policy(flags: [bool; 4]) -> ManagedPolicy {
    let grant = |yes| {
        if yes {
            verified()
        } else {
            EvidenceStatus::Unknown
        }
    };
    ManagedPolicy {
        revision: 1,
        selected_identity: EgressIdentity::Residential,
        path_fingerprint: "fixture-path".into(),
        capabilities: EgressCapabilities {
            v4_tcp: grant(flags[0]),
            v4_udp: grant(flags[1]),
            v6_tcp: grant(flags[2]),
            v6_udp: grant(flags[3]),
        },
    }
}
fn node() -> Node {
    Node {
        kind: NodeKind::Hy2Residential,
        label: "fixture".into(),
        host: "fixture.example.com".into(),
        port: 443,
        hop: None,
        transport: Transport::Hysteria2 {
            username: "fixture-user".into(),
            password: "fixture-only".into(),
            sni: "fixture.example.com".into(),
            obfs_password: None,
        },
    }
}
fn runtime(tun: bool) -> ManagedRuntime {
    ManagedRuntime {
        interface_name: String::new(),
        mixed_port: 10808,
        enable_tun: tun,
    }
}
fn config(flags: [bool; 4]) -> Value {
    compile(
        &policy(flags),
        &[node()],
        "203.0.113.10",
        &runtime(true),
        150,
    )
}
fn rules(cfg: &Value) -> &[Value] {
    cfg["route"]["rules"].as_array().unwrap()
}
fn outbound<'a>(cfg: &'a Value, tag: &str) -> &'a Value {
    cfg["outbounds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["tag"] == tag)
        .unwrap()
}
fn assert_closed(cfg: &Value) {
    assert_eq!(cfg["route"]["final"], "managed-proxy");
    assert_eq!(outbound(cfg, "managed-proxy")["type"], "block");
    assert_eq!(cfg["outbounds"].as_array().unwrap().len(), 1);
    assert_eq!(cfg["dns"]["servers"][0]["detour"], "managed-proxy");
    assert_eq!(
        cfg["dns"]["rules"],
        json!([{"action":"predefined","rcode":"REFUSED"}])
    );
    assert!(rules(cfg).contains(&json!({"action":"reject"})));
    assert_eq!(cfg["inbounds"][1]["address"].as_array().unwrap().len(), 2);
}

#[test]
fn full_capture_keeps_both_families_even_when_only_v4_is_authorized() {
    let cfg = config([true, true, false, false]);
    assert_eq!(cfg["inbounds"][0]["listen"], "127.0.0.1");
    assert_eq!(cfg["inbounds"][0]["listen_port"], 10808);
    let tun = &cfg["inbounds"][1];
    assert_eq!(
        tun["address"],
        json!(["172.19.0.1/30", "fdfe:dcba:9876::1/126"])
    );
    assert_eq!(tun["auto_route"], true);
    assert_eq!(tun["strict_route"], true);
    assert_eq!(tun["dns_mode"], "hijack");
    assert!(tun.get("route_exclude_address").is_none());
    assert_eq!(cfg["route"]["auto_detect_interface"], true);
    assert_eq!(cfg["route"]["final"], "managed-proxy");
}

#[test]
fn every_capability_grid_cell_is_enforced_for_literals_and_resolved_domains() {
    for mask in 0..16 {
        let flags = [mask & 1 != 0, mask & 2 != 0, mask & 4 != 0, mask & 8 != 0];
        let cfg = config(flags);
        if mask == 0 {
            assert_closed(&cfg);
            continue;
        }
        for (i, family, network, cidr) in [
            (0, 4, "tcp", "0.0.0.0/0"),
            (1, 4, "udp", "0.0.0.0/0"),
            (2, 6, "tcp", "::/0"),
            (3, 6, "udp", "::/0"),
        ] {
            assert_eq!(
                rules(&cfg)
                    .contains(&json!({"ip_version":family,"network":network,"action":"reject"})),
                !flags[i],
                "literal mask={mask}, cell={i}"
            );
            assert_eq!(
                rules(&cfg)
                    .contains(&json!({"ip_cidr":[cidr],"network":network,"action":"reject"})),
                !flags[i],
                "resolved mask={mask}, cell={i}"
            );
        }
    }
}

#[test]
fn resolving_domains_uses_each_networks_granted_families_then_private_guard() {
    let cfg = config([true, false, false, true]);
    let rs = rules(&cfg);
    for (network, strategy, forbidden) in [
        ("tcp", "ipv4_only", "::/0"),
        ("udp", "ipv6_only", "0.0.0.0/0"),
    ] {
        let resolve = rs.iter().position(|r| r == &json!({"network":network,"action":"resolve","server":"business-dns","strategy":strategy})).unwrap();
        let guard = rs
            .iter()
            .position(|r| r == &json!({"network":network,"ip_cidr":[forbidden],"action":"reject"}))
            .unwrap();
        assert!(resolve < guard);
        assert!(rs[guard + 1..].contains(&json!({"ip_is_private":true,"action":"reject"})));
    }
    let both = config([true, true, true, true]);
    for network in ["tcp", "udp"] {
        assert!(rules(&both).contains(&json!({"network":network,"action":"resolve","server":"business-dns","strategy":"prefer_ipv4"})));
    }
}

#[test]
fn dns_transport_requires_its_own_tcp_evidence_and_never_local_fallback() {
    for (flags, server, detour) in [
        (
            [true, false, false, false],
            "1.1.1.1",
            "managed-dns-tcp-adapter",
        ),
        (
            [false, false, true, false],
            "2606:4700:4700::1111",
            "managed-dns-tcp-adapter",
        ),
        ([false, true, false, true], "1.1.1.1", "managed-dns-block"),
    ] {
        let cfg = config(flags);
        assert_eq!(cfg["dns"]["servers"].as_array().unwrap().len(), 1);
        let dns = &cfg["dns"]["servers"][0];
        assert_eq!(dns["type"], "https");
        assert_eq!(dns["server"], server);
        assert_eq!(dns["detour"], detour);
        assert_eq!(dns["server_port"], 443);
        assert_eq!(dns["path"], "/dns-query");
        assert_eq!(
            dns["tls"],
            json!({"enabled":true,"server_name":"cloudflare-dns.com","insecure":false})
        );
        assert_eq!(cfg["dns"]["final"], "business-dns");
        assert_eq!(cfg["route"]["default_domain_resolver"], "business-dns");
        if detour == "managed-dns-block" {
            assert_eq!(outbound(&cfg, detour)["type"], "block");
            assert_eq!(
                cfg["dns"]["rules"],
                json!([{"action":"predefined","rcode":"REFUSED"}])
            );
        } else {
            assert_eq!(cfg["dns"]["rules"], json!([]));
        }
    }
}

#[test]
fn dns_interception_precedes_private_and_udp_denial_without_http_tls_rewriting() {
    let cfg = config([true, false, false, false]);
    let rs = rules(&cfg);
    assert!(rs.contains(&json!({"action":"sniff","sniffer":["dns"]})));
    let hijack = rs
        .iter()
        .position(|r| r == &json!({"network":["tcp","udp"],"port":53,"action":"hijack-dns"}))
        .unwrap();
    let private = rs.iter().position(|r| r["ip_is_private"] == true).unwrap();
    let denied = rs
        .iter()
        .position(|r| r == &json!({"network":"udp","action":"reject"}))
        .unwrap();
    assert!(hijack < private && hijack < denied);
    assert!(rs.contains(&json!({"protocol":"dns","action":"hijack-dns"})));
    assert!(rs.contains(&json!({"network":"icmp","action":"reject"})));
    assert!(cfg.get("experimental").is_none());
    for o in cfg["outbounds"].as_array().unwrap() {
        assert!(!["direct", "urltest", "dns"].contains(&o["type"].as_str().unwrap()));
    }
}

#[test]
fn unknown_unsupported_expired_future_and_different_path_are_closed() {
    let statuses = [
        EvidenceStatus::Unknown,
        EvidenceStatus::Unsupported,
        verified(),
        verified(),
        verified(),
    ];
    for (i, mut status) in statuses.into_iter().enumerate() {
        if let EvidenceStatus::Verified(e) = &mut status {
            match i {
                2 => e.expires_at = 150,
                3 => e.observed_at = 151,
                _ => e.path_fingerprint = "changed".into(),
            }
        }
        let mut p = policy([false; 4]);
        p.capabilities = EgressCapabilities {
            v4_tcp: status.clone(),
            v4_udp: status.clone(),
            v6_tcp: status.clone(),
            v6_udp: status,
        };
        assert_closed(&compile(&p, &[node()], "203.0.113.10", &runtime(true), 150));
    }
}

#[test]
fn empty_or_wrong_identity_nodes_never_fall_back_to_another_identity() {
    let p = policy([true; 4]);
    assert_closed(&compile(&p, &[], "203.0.113.10", &runtime(true), 150));
    let mut other = node();
    other.kind = NodeKind::Hy2Direct;
    assert_closed(&compile(
        &p,
        &[other.clone()],
        "203.0.113.10",
        &runtime(true),
        150,
    ));
    let cfg = compile(&p, &[other, node()], "203.0.113.10", &runtime(true), 150);
    assert_eq!(
        outbound(&cfg, "managed-proxy")["outbounds"],
        json!(["managed-node-0"])
    );
}

#[test]
fn literal_outer_endpoints_preserve_sni_and_domain_bootstrap_is_narrow() {
    for literal in ["203.0.113.10", "2001:db8:3::10"] {
        let mut n = node();
        n.host = literal.into();
        let cfg = compile(&policy([true; 4]), &[n], "", &runtime(true), 150);
        assert_eq!(outbound(&cfg, "managed-node-0")["server"], literal);
        assert_eq!(
            outbound(&cfg, "managed-node-0")["tls"]["server_name"],
            "fixture.example.com"
        );
    }
    let cfg = config([true; 4]);
    assert_eq!(outbound(&cfg, "managed-node-0")["server"], "203.0.113.10");
    for invalid in [
        "",
        "not-an-ip",
        "999.999.999.999",
        "[2001:db8::10]",
        "203.0.113.10:443",
    ] {
        assert_closed(&compile(
            &policy([true; 4]),
            &[node()],
            invalid,
            &runtime(true),
            150,
        ));
    }
    let mut other = node();
    other.host = "other.example.com".into();
    assert_closed(&compile(
        &policy([true; 4]),
        &[node(), other],
        "203.0.113.10",
        &runtime(true),
        150,
    ));
    for invalid_host in ["", "https://example.com", "999.999.999.999", "bad host"] {
        let mut n = node();
        n.host = invalid_host.into();
        assert_closed(&compile(
            &policy([true; 4]),
            &[n],
            "203.0.113.10",
            &runtime(true),
            150,
        ));
    }
}

#[test]
fn selector_members_have_unique_tags_and_stable_default_without_recovery() {
    let mut second = node();
    second.port = 444;
    let cfg = compile(
        &policy([true; 4]),
        &[node(), second],
        "203.0.113.10",
        &runtime(true),
        150,
    );
    let selector = outbound(&cfg, "managed-proxy");
    assert_eq!(selector["type"], "selector");
    assert_eq!(
        selector["outbounds"],
        json!(["managed-node-0", "managed-node-1"])
    );
    assert_eq!(selector["default"], "managed-node-0");
    assert_eq!(outbound(&cfg, "managed-node-1")["server_port"], 444);
}

#[test]
fn transports_keep_authorized_parameters_and_never_need_local_resolution() {
    let mut hy2 = node();
    hy2.hop = Some((41000, 50000));
    if let Transport::Hysteria2 { obfs_password, .. } = &mut hy2.transport {
        *obfs_password = Some("fixture-obfs".into());
    }
    let mut reality = node();
    reality.kind = NodeKind::RealityResidential;
    reality.transport = Transport::Reality {
        uuid: uuid::Uuid::from_u128(1),
        public_key: "fixture-public".into(),
        short_id: "0123456789abcdef".into(),
        server_name: "tls.example.com".into(),
        fingerprint: "chrome".into(),
        flow: "xtls-rprx-vision".into(),
    };
    let cfg = compile(
        &policy([true; 4]),
        &[hy2, reality],
        "203.0.113.10",
        &runtime(true),
        150,
    );
    let h = outbound(&cfg, "managed-node-0");
    assert_eq!(h["password"], "fixture-user:fixture-only");
    assert_eq!(h["server_ports"], json!(["41000:50000"]));
    assert_eq!(
        h["obfs"],
        json!({"type":"salamander","password":"fixture-obfs"})
    );
    let r = outbound(&cfg, "managed-node-1");
    assert_eq!(r["type"], "vless");
    assert_eq!(r["uuid"], uuid::Uuid::from_u128(1).to_string());
    assert_eq!(r["tls"]["server_name"], "tls.example.com");
    assert_eq!(r["tls"]["reality"]["public_key"], "fixture-public");
    assert_eq!(r["tls"]["utls"]["fingerprint"], "chrome");
    assert!(h.get("domain_resolver").is_none());
    assert!(r.get("domain_resolver").is_none());
}

#[test]
fn same_snapshot_is_byte_stable_and_mixed_only_requires_explicit_runtime() {
    let p = policy([true; 4]);
    let ns = [node()];
    let a = compile(&p, &ns, "203.0.113.10", &runtime(true), 150);
    let b = compile(&p, &ns, "203.0.113.10", &runtime(true), 151);
    assert_eq!(
        serde_json::to_vec(&a).unwrap(),
        serde_json::to_vec(&b).unwrap()
    );
    let mixed = compile(&p, &ns, "203.0.113.10", &runtime(false), 150);
    assert_eq!(mixed["inbounds"].as_array().unwrap().len(), 1);
    assert_eq!(a["route"], mixed["route"]);
    assert_eq!(a["dns"], mixed["dns"]);
    let mut rt = runtime(true);
    rt.interface_name = "fixture-tun".into();
    assert_eq!(
        compile(&p, &ns, "203.0.113.10", &rt, 150)["inbounds"][1]["interface_name"],
        "fixture-tun"
    );
}

/// Explicit private export only. Does not execute or validate a kernel.
/// Root owns the separate actual-core gate; absence of export input is an error.
#[cfg(unix)]
#[test]
#[ignore = "private fixture export, not a kernel test; requires BUI_MANAGED_EXPORT_DIR"]
fn export_managed_core_fixtures() {
    use sha2::{Digest, Sha256};
    use std::fs::{DirBuilder, OpenOptions};
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    use std::path::PathBuf;
    let directory = PathBuf::from(
        std::env::var_os("BUI_MANAGED_EXPORT_DIR").expect("BUI_MANAGED_EXPORT_DIR is required"),
    );
    assert!(directory.is_absolute(), "export directory must be absolute");
    DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .expect("export requires a new private directory");
    let write = |name: &str, value: &Value| {
        let bytes = serde_json::to_vec_pretty(value).unwrap();
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(directory.join(name))
            .unwrap();
        file.write_all(&bytes).unwrap();
        hex::encode(Sha256::digest(bytes))
    };
    let mut manifest = Vec::new();
    for (name, flags, closed) in [
        ("all_verified", [true, true, true, true], false),
        ("v4_tcp_only", [true, false, false, false], false),
        ("v4_udp_only", [false, true, false, false], false),
        ("v6_tcp_only", [false, false, true, false], false),
        ("v6_udp_only", [false, false, false, true], false),
        ("v4tcp_v6udp", [true, false, false, true], false),
        ("v6tcp_v4udp", [false, true, true, false], false),
        ("unknown", [false; 4], true),
        ("unsupported", [false; 4], true),
        ("expired", [false; 4], true),
        ("pathchanged", [false; 4], true),
        ("future", [false; 4], true),
        ("empty", [true; 4], true),
        ("wrongidentity", [true; 4], true),
        ("invalid_dial", [true; 4], true),
        ("multiple_domains", [true; 4], true),
        ("selected_path_failure", [true; 4], false),
        ("outer_v6", [true; 4], false),
    ] {
        let mut p = policy(flags);
        if matches!(name, "expired" | "pathchanged" | "future" | "unsupported") {
            let mut evidence = verified();
            if let EvidenceStatus::Verified(e) = &mut evidence {
                match name {
                    "expired" => e.expires_at = 150,
                    "pathchanged" => e.path_fingerprint = "old-path".into(),
                    "future" => e.observed_at = 151,
                    _ => {}
                }
            }
            if name == "unsupported" {
                evidence = EvidenceStatus::Unsupported;
            }
            p.capabilities = EgressCapabilities {
                v4_tcp: evidence.clone(),
                v4_udp: evidence.clone(),
                v6_tcp: evidence.clone(),
                v6_udp: evidence,
            };
        }
        let mut n = node();
        n.host = "203.0.113.10".into();
        n.port = 18443;
        n.transport = Transport::Hysteria2 {
            username: "fixture".into(),
            password: "managed-only".into(),
            sni: "managed-fixture.test".into(),
            obfs_password: None,
        };
        let mut ns = vec![n.clone()];
        let mut dial = "203.0.113.10";
        match name {
            "empty" => ns.clear(),
            "wrongidentity" => ns[0].kind = NodeKind::Hy2Direct,
            "invalid_dial" => {
                ns[0].host = "managed-fixture.test".into();
                dial = "invalid";
            }
            "multiple_domains" => {
                ns[0].host = "managed-fixture.test".into();
                n.host = "other-fixture.test".into();
                ns.push(n);
            }
            "selected_path_failure" => {
                n.port = 18444;
                ns.push(n);
            }
            "outer_v6" => ns[0].host = "2001:db8:3::10".into(),
            _ => {}
        }
        let mut rt = ManagedRuntime {
            interface_name: "mfp-tun0".into(),
            mixed_port: 18080,
            enable_tun: false,
        };
        let mixed = compile(&p, &ns, dial, &rt, 150);
        rt.enable_tun = true;
        let tun = compile(&p, &ns, dial, &rt, 150);
        assert_eq!(
            outbound(&mixed, "managed-proxy")["type"] == "block",
            closed,
            "{name}"
        );
        let mixed_name = format!("{name}.mixed.json");
        let tun_name = format!("{name}.tun.json");
        let mixed_sha = write(&mixed_name, &mixed);
        let tun_sha = write(&tun_name, &tun);
        manifest.push(json!({"name":name,"mixed":mixed_name,"tun":tun_name,"capabilities":flags,"closed":closed,"identity":"residential","ingress_expectations":{
            "mixed":{"tcp":[!closed && flags[0],!closed && flags[2]],"udp":[false,false],"dns_udp":false},
            "tun":{"tcp":[!closed && flags[0],!closed && flags[2]],"udp":[!closed && flags[1],!closed && flags[3]],"dns_udp":!closed && (flags[0] || flags[2])}
        },"sha256":{"mixed":mixed_sha,"tun":tun_sha}}));
    }
    assert_eq!(manifest.len(), 18);
    // These transport branches receive syntax/start checks only, not delivery credit.
    let mut check_start = Vec::new();
    for name in ["reality_only", "hy2_hop_obfs", "multiple_literal_hosts"] {
        let mut n = node();
        n.host = "203.0.113.10".into();
        n.port = 18443;
        let mut ns = vec![n];
        match name {
            "reality_only" => {
                use base64::Engine as _;
                ns[0].kind = NodeKind::RealityResidential;
                // Public test vector from RFC 7748, never a production key.
                let key =
                    hex::decode("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a")
                        .unwrap();
                ns[0].transport = Transport::Reality {
                    uuid: uuid::Uuid::from_u128(1),
                    public_key: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key),
                    short_id: "0123456789abcdef".into(),
                    server_name: "managed-fixture.test".into(),
                    fingerprint: "chrome".into(),
                    flow: "xtls-rprx-vision".into(),
                };
            }
            "hy2_hop_obfs" => {
                ns[0].hop = Some((18443, 18444));
                if let Transport::Hysteria2 { obfs_password, .. } = &mut ns[0].transport {
                    *obfs_password = Some("fixture-obfs".into());
                }
            }
            _ => {
                let mut other = ns[0].clone();
                other.host = "2001:db8:3::10".into();
                ns.push(other);
            }
        }
        let mut rt = ManagedRuntime {
            interface_name: "mfp-tun0".into(),
            mixed_port: 18080,
            enable_tun: false,
        };
        let mixed_name = format!("{name}.mixed.json");
        let tun_name = format!("{name}.tun.json");
        let mixed_sha = write(&mixed_name, &compile(&policy([true; 4]), &ns, "", &rt, 150));
        rt.enable_tun = true;
        let tun_sha = write(&tun_name, &compile(&policy([true; 4]), &ns, "", &rt, 150));
        check_start.push(json!({"name":name,"mixed":mixed_name,"tun":tun_name,"sha256":{"mixed":mixed_sha,"tun":tun_sha},"validation":"check_start_only"}));
    }
    assert_eq!(check_start.len(), 3);
    write(
        "manifest.json",
        &json!({"schema":1,"synthetic_only":true,"cases":manifest,"check_start_cases":check_start}),
    );
}

#[test]
fn tun_udp_sessions_are_keyed_by_destination_and_port() {
    for flags in [[true; 4], [false, true, false, false], [false; 4]] {
        let cfg = config(flags);
        assert_eq!(
            cfg["inbounds"][1]["udp_mapping"],
            "address_and_port_dependent"
        );
    }
}

#[test]
fn mixed_udp_is_rejected_before_dns_or_sniff_can_establish_an_association() {
    for flags in [[true; 4], [false, true, false, true], [false; 4]] {
        let cfg = config(flags);
        assert_eq!(
            rules(&cfg)[0],
            json!({"inbound":["managed-mixed"],"network":"udp","action":"reject"})
        );
        assert!(!rules(&cfg).iter().any(|r| r.get("udp_connect").is_some()));
        let mixed = compile(
            &policy(flags),
            &[node()],
            "203.0.113.10",
            &runtime(false),
            150,
        );
        assert_eq!(rules(&mixed)[0], rules(&cfg)[0]);
    }
}

#[test]
fn authorized_dns_adapter_reenters_the_same_runtime_listener_and_selected_policy() {
    for (flags, port) in [
        ([true, false, false, false], 10808),
        ([false, false, true, false], 18080),
        ([true; 4], 20808),
    ] {
        let mut rt = runtime(true);
        rt.mixed_port = port;
        let cfg = compile(&policy(flags), &[node()], "203.0.113.10", &rt, 150);
        let tag = cfg["dns"]["servers"][0]["detour"].as_str().unwrap();
        assert_eq!(tag, "managed-dns-tcp-adapter");
        assert_eq!(
            outbound(&cfg, tag),
            &json!({"type":"socks","tag":"managed-dns-tcp-adapter","server":"127.0.0.1","server_port":port,"version":"5"})
        );
        assert_eq!(cfg["inbounds"][0]["listen"], "127.0.0.1");
        assert_eq!(cfg["inbounds"][0]["listen_port"], port);
        assert_eq!(cfg["route"]["final"], "managed-proxy");
        assert_eq!(
            outbound(&cfg, "managed-proxy")["outbounds"],
            json!(["managed-node-0"])
        );
        assert!(!rules(&cfg)
            .iter()
            .any(|rule| rule.get("outbound").is_some()));
        assert!(rules(&cfg).contains(&json!({"ip_is_private":true,"action":"reject"})));
    }
}

#[test]
fn dns_adapter_is_absent_without_authorized_tcp_or_renderable_selected_nodes() {
    let mut invalid = policy([true; 4]);
    invalid.path_fingerprint = "changed-path".into();
    let mut other = node();
    other.kind = NodeKind::Hy2Direct;
    for cfg in [
        config([false; 4]),
        config([false, true, false, false]),
        config([false, false, false, true]),
        compile(&invalid, &[node()], "203.0.113.10", &runtime(true), 150),
        compile(&policy([true; 4]), &[], "203.0.113.10", &runtime(true), 150),
        compile(
            &policy([true; 4]),
            &[other],
            "203.0.113.10",
            &runtime(true),
            150,
        ),
        compile(
            &policy([true; 4]),
            &[node()],
            "invalid",
            &runtime(true),
            150,
        ),
        compile(
            &policy([true; 4]),
            &[node()],
            "203.0.113.10",
            &runtime(true),
            200,
        ),
    ] {
        assert!(!cfg["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["type"] == "socks" || o["tag"] == "managed-dns-tcp-adapter"));
        assert_eq!(
            cfg["dns"]["rules"],
            json!([{"action":"predefined","rcode":"REFUSED"}])
        );
        let detour = cfg["dns"]["servers"][0]["detour"].as_str().unwrap();
        assert_eq!(outbound(&cfg, detour)["type"], "block");
    }
}
