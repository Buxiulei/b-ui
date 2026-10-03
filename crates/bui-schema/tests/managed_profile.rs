use bui_schema::managed::{
    allows, select_nodes, ClientTarget, EgressCapabilities, EgressIdentity, Evidence,
    EvidenceStatus, ManagedPolicy,
};
use bui_schema::nodes::{Node, NodeKind, Transport};
use serde_json::json;
use uuid::Uuid;

fn evidence(observed_at: i64, expires_at: i64) -> EvidenceStatus {
    EvidenceStatus::Verified(Evidence {
        path_fingerprint: "resi-binding-a".into(),
        observed_at,
        expires_at,
        observed_identity: "fixture-residential".into(),
    })
}

#[test]
fn evidence_expires_without_granting_another_path() {
    let status = evidence(100, 200);
    for (now, expected) in [(99, false), (100, true), (199, true), (200, false)] {
        assert_eq!(
            allows(&status, "resi-binding-a", now),
            expected,
            "now={now}"
        );
    }
    assert!(!allows(&status, "vps-binding-b", 199));
    assert!(!allows(&status, "resi-binding-a-changed", 199));
}

#[test]
fn future_observations_and_invalid_intervals_never_allow() {
    assert!(!allows(&evidence(300, 400), "resi-binding-a", 199));
    for (observed_at, expires_at) in [(200, 200), (300, 200)] {
        for now in [199, 200, 250, 300, 400] {
            assert!(!allows(
                &evidence(observed_at, expires_at),
                "resi-binding-a",
                now
            ));
        }
    }
}

#[test]
fn unknown_and_unsupported_never_allow() {
    for status in [EvidenceStatus::Unknown, EvidenceStatus::Unsupported] {
        for now in [i64::MIN, 100, i64::MAX] {
            assert!(!allows(&status, "resi-binding-a", now));
        }
    }
}

fn node(kind: NodeKind, label: &str, port: u16, transport: Transport) -> Node {
    Node {
        kind,
        label: label.into(),
        host: "example.com".into(),
        port,
        hop: matches!(kind, NodeKind::Hy2Direct | NodeKind::Hy2Residential)
            .then_some((41000, 50000)),
        transport,
    }
}

fn hy2(username: &str, password: &str) -> Transport {
    Transport::Hysteria2 {
        username: username.into(),
        password: password.into(),
        sni: "example.com".into(),
        obfs_password: Some("test-only-obfs-pw".into()),
    }
}

fn reality() -> Transport {
    Transport::Reality {
        uuid: Uuid::from_u128(1),
        public_key: "test-only-public-key".into(),
        short_id: "0123456789abcdef".into(),
        server_name: "example.com".into(),
        fingerprint: "chrome".into(),
        flow: "xtls-rprx-vision".into(),
    }
}

#[test]
fn selection_preserves_authorized_nodes_order_and_credentials() {
    let residential_hy2 = node(
        NodeKind::Hy2Residential,
        "alice-HY2住宅",
        40000,
        hy2("alice", "test-only-residential-pw"),
    );
    let direct_reality = node(
        NodeKind::RealityDirect,
        "alice-Reality直连",
        10001,
        reality(),
    );
    let direct_hy2 = node(
        NodeKind::Hy2Direct,
        "alice-HY2直连",
        10000,
        hy2("alice", "test-only-direct-pw"),
    );
    let residential_reality = node(
        NodeKind::RealityResidential,
        "alice-Reality住宅",
        10002,
        reality(),
    );
    let authorized = vec![
        residential_hy2.clone(),
        direct_reality.clone(),
        direct_hy2.clone(),
        residential_reality.clone(),
    ];
    let before = authorized.clone();
    assert_eq!(
        select_nodes(&authorized, EgressIdentity::Residential),
        vec![residential_hy2, residential_reality]
    );
    assert_eq!(
        select_nodes(&authorized, EgressIdentity::Vps),
        vec![direct_reality, direct_hy2]
    );
    assert_eq!(
        authorized, before,
        "selection must leave the input untouched"
    );
}

#[test]
fn residential_selection_from_vps_only_authorization_is_empty() {
    let authorized = vec![node(
        NodeKind::Hy2Direct,
        "alice-HY2直连",
        10000,
        hy2("alice", "test-only-direct-pw"),
    )];
    assert!(select_nodes(&authorized, EgressIdentity::Residential).is_empty());
    assert_eq!(select_nodes(&authorized, EgressIdentity::Vps), authorized);
}

#[test]
fn selection_never_creates_missing_nodes() {
    for identity in [EgressIdentity::Vps, EgressIdentity::Residential] {
        assert!(select_nodes(&[], identity).is_empty());
    }
    let authorized = vec![node(
        NodeKind::RealityResidential,
        "bob-Reality住宅",
        10002,
        reality(),
    )];
    assert_eq!(
        select_nodes(&authorized, EgressIdentity::Residential),
        authorized
    );
    assert!(select_nodes(&authorized, EgressIdentity::Vps).is_empty());
}

#[test]
fn missing_capabilities_are_unknown_and_deny() {
    let policy: ManagedPolicy = serde_json::from_value(json!({
        "revision": 7,
        "selected_identity": "residential",
        "path_fingerprint": "resi-binding-a"
    }))
    .unwrap();
    assert_eq!(policy.capabilities, EgressCapabilities::default());
    let partial: EgressCapabilities =
        serde_json::from_value(json!({"v4_tcp": "unsupported"})).unwrap();
    assert_eq!(partial.v4_tcp, EvidenceStatus::Unsupported);
    for status in [
        &policy.capabilities.v4_tcp,
        &policy.capabilities.v4_udp,
        &policy.capabilities.v6_tcp,
        &policy.capabilities.v6_udp,
        &partial.v4_udp,
        &partial.v6_tcp,
        &partial.v6_udp,
    ] {
        assert_eq!(status, &EvidenceStatus::Unknown);
        assert!(!allows(status, &policy.path_fingerprint, 100));
    }
}

#[test]
fn policy_round_trips_and_expiry_does_not_change_revision() {
    let policy = ManagedPolicy {
        revision: 7,
        selected_identity: EgressIdentity::Residential,
        path_fingerprint: "resi-binding-a".into(),
        capabilities: EgressCapabilities {
            v4_tcp: evidence(100, 200),
            v4_udp: EvidenceStatus::Unsupported,
            ..EgressCapabilities::default()
        },
    };
    let before = policy.clone();
    assert!(allows(
        &policy.capabilities.v4_tcp,
        &policy.path_fingerprint,
        199
    ));
    assert!(!allows(
        &policy.capabilities.v4_tcp,
        &policy.path_fingerprint,
        200
    ));
    assert_eq!(policy.revision, 7);
    assert_eq!(policy, before);
    let back: ManagedPolicy =
        serde_json::from_str(&serde_json::to_string(&policy).unwrap()).unwrap();
    assert_eq!(back, policy);
    for identity in [EgressIdentity::Vps, EgressIdentity::Residential] {
        assert_eq!(
            serde_json::from_value::<EgressIdentity>(serde_json::to_value(identity).unwrap())
                .unwrap(),
            identity
        );
    }
    for target in [ClientTarget::V2raynMacOs, ClientTarget::BuiCLinux] {
        assert_eq!(
            serde_json::from_value::<ClientTarget>(serde_json::to_value(target).unwrap()).unwrap(),
            target
        );
    }
}

#[test]
fn debug_redacts_arbitrary_evidence_and_policy_strings() {
    let evidence = Evidence {
        path_fingerprint: "SENSITIVE_EVIDENCE_PATH_MARKER".into(),
        observed_at: 100,
        expires_at: 200,
        observed_identity: "SENSITIVE_OBSERVED_IDENTITY_MARKER".into(),
    };
    let status = EvidenceStatus::Verified(evidence.clone());
    let capabilities = EgressCapabilities {
        v4_tcp: status.clone(),
        ..EgressCapabilities::default()
    };
    let policy = ManagedPolicy {
        revision: 7,
        selected_identity: EgressIdentity::Residential,
        path_fingerprint: "SENSITIVE_POLICY_PATH_MARKER".into(),
        capabilities: capabilities.clone(),
    };
    for debug in [
        format!("{evidence:?}"),
        format!("{status:?}"),
        format!("{capabilities:?}"),
        format!("{policy:?}"),
        format!("{policy:#?}"),
    ] {
        for marker in [
            "SENSITIVE_EVIDENCE_PATH_MARKER",
            "SENSITIVE_OBSERVED_IDENTITY_MARKER",
            "SENSITIVE_POLICY_PATH_MARKER",
        ] {
            assert!(
                !debug.contains(marker),
                "Debug must redact arbitrary inputs"
            );
        }
    }
}
