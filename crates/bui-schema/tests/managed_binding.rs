use bui_schema::managed::{EgressIdentity, Evidence, EvidenceStatus};
use bui_schema::managed_binding::{path_fingerprint, reconcile_revisions};
use bui_schema::model::State;
use serde_json::json;

fn state() -> State {
    serde_json::from_value(json!({
        "schema_version":1,
        "node":{"id":"00000000-0000-0000-0000-000000000001", "name":"fixture", "domain":"example.test", "public_ip":"203.0.113.1",
        "ports":{"hy2":10000,"hy2_resi":40000,"hy2_resi_hop":[41000,50000],"reality_direct":10001,"reality_resi":10002,"admin":8080},
        "reality":{"private_key":"fixture-private","public_key":"fixture-public","short_ids":["abcd"],"dest":"example.test:443","server_names":["example.test"]}},
        "admin":{"password_hash":"unused","jwt_secret":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"},
        "users":[{"user_id":"00000000-0000-0000-0000-000000000002","username":"alice","created_at":"2026-10-03T00:00:00Z", "credentials":{"hy2_password":"fixture-user-secret","vless_uuid":"00000000-0000-0000-0000-000000000003"},"entitlements":{"protocols":["hysteria2","reality"],"direct":true}}]
    })).unwrap()
}
fn selected() -> State {
    let old = state();
    let mut next = old.clone();
    next.users[0].managed_egress = Some(EgressIdentity::Vps);
    reconcile_revisions(&old, &mut next).unwrap();
    next
}
#[test]
fn explicit_selection_initializes_once_and_ignores_forged_revision() {
    let old = state();
    let mut next = old.clone();
    next.users[0].managed_profile_revision = Some(999);
    reconcile_revisions(&old, &mut next).unwrap();
    assert_eq!(next, old);
    let old = selected();
    assert_eq!(old.users[0].managed_profile_revision, Some(1));
    let mut next = old.clone();
    next.users[0].managed_profile_revision = Some(999);
    reconcile_revisions(&old, &mut next).unwrap();
    assert_eq!(next, old);
}
#[test]
fn credential_node_and_key_changes_bump_but_accounting_and_notes_do_not() {
    let old = selected();
    let path = path_fingerprint(&old, &old.users[0]).unwrap();
    let mut next = old.clone();
    next.users[0].usage.total_bytes = 123;
    next.users[0].note = "operator note".into();
    reconcile_revisions(&old, &mut next).unwrap();
    assert_eq!(next.users[0].managed_profile_revision, Some(1));
    next.users[0].credentials.hy2_password = "replacement-secret".into();
    assert_eq!(path_fingerprint(&next, &next.users[0]).unwrap(), path);
    reconcile_revisions(&old, &mut next).unwrap();
    assert_eq!(next.users[0].managed_profile_revision, Some(2));
    for field in ["public_ip", "key", "port"] {
        let mut next = old.clone();
        match field {
            "public_ip" => next.node.public_ip = "203.0.113.2".into(),
            "key" => next.admin.jwt_secret = "ff".repeat(32),
            _ => next.node.ports.hy2 += 1,
        }
        assert_ne!(path_fingerprint(&next, &next.users[0]).unwrap(), path);
        reconcile_revisions(&old, &mut next).unwrap();
        assert_eq!(next.users[0].managed_profile_revision, Some(2));
    }
}
#[test]
fn capability_refresh_timestamps_do_not_bump_but_policy_changes_do() {
    let mut old = selected();
    let path = path_fingerprint(&old, &old.users[0]).unwrap();
    old.managed_egress_capabilities
        .entry(path.clone())
        .or_default()
        .v4_tcp = EvidenceStatus::Verified(Evidence {
        path_fingerprint: path.clone(),
        observed_at: 10,
        expires_at: 20,
        observed_identity: "fixture-identity".into(),
    });
    let mut next = old.clone();
    if let EvidenceStatus::Verified(e) = &mut next
        .managed_egress_capabilities
        .get_mut(&path)
        .unwrap()
        .v4_tcp
    {
        e.observed_at = 15;
        e.expires_at = 25;
    }
    reconcile_revisions(&old, &mut next).unwrap();
    assert_eq!(next.users[0].managed_profile_revision, Some(1));
    next.managed_egress_capabilities
        .get_mut(&path)
        .unwrap()
        .v4_tcp = EvidenceStatus::Unsupported;
    reconcile_revisions(&old, &mut next).unwrap();
    assert_eq!(next.users[0].managed_profile_revision, Some(2));
}
#[test]
fn overflow_and_duplicate_ids_are_transactional() {
    let mut old = selected();
    old.users[0].managed_profile_revision = Some(u64::MAX);
    let mut next = old.clone();
    next.node.public_ip = "203.0.113.2".into();
    let untouched = next.clone();
    assert!(reconcile_revisions(&old, &mut next).is_err());
    assert_eq!(next, untouched);
    let old = selected();
    let mut next = old.clone();
    next.users.push(next.users[0].clone());
    let untouched = next.clone();
    assert!(reconcile_revisions(&old, &mut next).is_err());
    assert_eq!(next, untouched);
}
#[test]
fn malformed_key_denies_managed_but_allows_legacy_and_key_repair() {
    let mut old = selected();
    old.admin.jwt_secret = "weak".into();
    assert!(path_fingerprint(&old, &old.users[0]).is_err());
    let mut next = old.clone();
    next.users[0].note = "repair in progress".into();
    reconcile_revisions(&old, &mut next).unwrap();
    assert_eq!(next.users[0].managed_profile_revision, Some(1));
    next.admin.jwt_secret = state().admin.jwt_secret;
    reconcile_revisions(&old, &mut next).unwrap();
    assert_eq!(next.users[0].managed_profile_revision, Some(2));
}

fn residential() -> State {
    let mut state = selected();
    let id = uuid::Uuid::from_u128(7);
    state.users[0].managed_egress = Some(EgressIdentity::Residential);
    state.users[0].entitlements.residential =
        Some(serde_json::from_value(json!({"group_id":"default","slot_id":id})).unwrap());
    state.users[0].credentials.hy2_resi_cred = Some("r000".into());
    state.residential.slots.push(bui_schema::model::Slot {
        index: 0,
        upstream_id: id,
    });
    state.residential.hy2_pool.creds.push(
        serde_json::from_value(
            json!({"id":"r000","name":"fixture-account","secret":"fixture-subscriber-secret"}),
        )
        .unwrap(),
    );
    let group = state.residential.groups.get_mut("default").unwrap();
    group.enabled = true;
    group.upstreams.push(serde_json::from_value(json!({"id":id,"name":"fixture-provider","kind":"socks5","host":"provider.example.test","port":10007,"username":"fixture-provider-account","password":"fixture-provider-secret","ports_allowed":[443,80]})).unwrap());
    state
}
#[test]
fn exact_supplier_slot_and_protocol_scope_invalidate_proof_without_subscriber_leakage() {
    let old = residential();
    let path = path_fingerprint(&old, &old.users[0]).unwrap();
    assert_eq!(path.len(), 64);
    for change in 0..7 {
        let mut next = old.clone();
        match change {
            0 => next
                .residential
                .groups
                .get_mut("default")
                .unwrap()
                .upstreams[0]
                .password
                .push('x'),
            1 => next
                .residential
                .groups
                .get_mut("default")
                .unwrap()
                .upstreams[0]
                .username
                .push('x'),
            2 => next.residential.slots[0].index = 1,
            3 => next.users[0]
                .entitlements
                .protocols
                .retain(|p| *p != bui_schema::model::Protocol::Hysteria2),
            4 => {
                next.node.obfs = bui_schema::model::Obfs {
                    enabled: true,
                    password: "fixture-obfs".into(),
                }
            }
            5 => {
                next.residential
                    .groups
                    .get_mut("default")
                    .unwrap()
                    .upstreams[0]
                    .ports_allowed = Some(vec![443])
            }
            _ => next.node.ports.hy2_resi += 1,
        }
        assert_ne!(
            path_fingerprint(&next, &next.users[0]).unwrap(),
            path,
            "change {change}"
        );
        reconcile_revisions(&old, &mut next).unwrap();
        assert_eq!(next.users[0].managed_profile_revision, Some(2));
    }
    let mut next = old.clone();
    next.residential.hy2_pool.creds[0].secret.push('x');
    assert_eq!(path_fingerprint(&next, &next.users[0]).unwrap(), path);
    reconcile_revisions(&old, &mut next).unwrap();
    assert_eq!(next.users[0].managed_profile_revision, Some(2));
}
#[test]
fn supplier_probe_metadata_and_canonical_port_order_are_not_profile_changes() {
    let old = residential();
    let mut next = old.clone();
    let upstream = &mut next
        .residential
        .groups
        .get_mut("default")
        .unwrap()
        .upstreams[0];
    upstream.verified = Some(
        serde_json::from_value(json!({"ip":"203.0.113.8","at":"2026-10-03T01:00:00Z"})).unwrap(),
    );
    upstream.ports_allowed = Some(vec![80, 443, 80]);
    upstream.priority = 42;
    reconcile_revisions(&old, &mut next).unwrap();
    assert_eq!(next.users[0].managed_profile_revision, Some(1));
    assert_eq!(
        path_fingerprint(&next, &next.users[0]).unwrap(),
        path_fingerprint(&old, &old.users[0]).unwrap()
    );
}
#[test]
fn duplicate_upstream_or_slot_never_produces_a_proof_key() {
    let old = residential();
    let mut next = old.clone();
    next.residential.slots.push(next.residential.slots[0]);
    assert!(path_fingerprint(&next, &next.users[0]).is_err());
    let mut next = old.clone();
    let group = next.residential.groups.get_mut("default").unwrap();
    group.upstreams.push(group.upstreams[0].clone());
    assert!(path_fingerprint(&next, &next.users[0]).is_err());
}
#[test]
fn grant_validation_preserves_unavailable_intent_without_manufacturing_rights() {
    use bui_schema::managed_binding::selection_granted;
    let mut state = residential();
    state.residential.groups.get_mut("default").unwrap().enabled = false;
    assert!(selection_granted(
        &state.users[0],
        &state,
        EgressIdentity::Residential
    ));
    assert!(path_fingerprint(&state, &state.users[0]).is_err());
    state.users[0].entitlements.residential = None;
    assert!(!selection_granted(
        &state.users[0],
        &state,
        EgressIdentity::Residential
    ));
    state.users[0].entitlements.protocols.clear();
    assert!(!selection_granted(
        &state.users[0],
        &state,
        EgressIdentity::Vps
    ));
}

#[test]
fn all_revisions_are_computed_before_any_user_is_mutated_and_ids_survive_reorder() {
    let mut old = selected();
    let mut second = old.users[0].clone();
    second.user_id = uuid::Uuid::from_u128(44);
    second.username = "bob".into();
    second.managed_profile_revision = Some(u64::MAX);
    old.users.push(second);
    let mut next = old.clone();
    next.node.public_ip = "203.0.113.2".into();
    let unchanged = next.clone();
    assert!(reconcile_revisions(&old, &mut next).is_err());
    assert_eq!(next, unchanged);
    let mut next = old.clone();
    next.users.reverse();
    next.users[1].credentials.hy2_password.push('x');
    reconcile_revisions(&old, &mut next).unwrap();
    assert_eq!(next.users[0].managed_profile_revision, Some(u64::MAX));
    assert_eq!(next.users[1].managed_profile_revision, Some(2));
}
#[test]
fn already_selected_import_normalizes_only_at_publication_and_unset_serde_stays_absent() {
    let old = state();
    let value = serde_json::to_value(&old).unwrap();
    assert!(value.get("managed_egress_capabilities").is_none());
    assert!(value["users"][0].get("managed_egress").is_none());
    assert!(value["users"][0].get("managed_profile_revision").is_none());
    let mut imported = old;
    imported.users[0].managed_egress = Some(EgressIdentity::Vps);
    let mut next = imported.clone();
    reconcile_revisions(&imported, &mut next).unwrap();
    assert_eq!(next.users[0].managed_profile_revision, Some(1));
    assert_eq!(imported.users[0].managed_profile_revision, None);
}

fn change_unavailable_desired(state: &mut State, change: &str) {
    match change {
        "selected_uuid" => {
            state.users[0]
                .entitlements
                .residential
                .as_mut()
                .unwrap()
                .slot_id = Some(uuid::Uuid::from_u128(91))
        }
        "slot_index" => state.residential.slots[0].index += 1,
        "supplier_password" => state
            .residential
            .groups
            .get_mut("default")
            .unwrap()
            .upstreams[0]
            .password
            .push('x'),
        "supplier_endpoint" => {
            state
                .residential
                .groups
                .get_mut("default")
                .unwrap()
                .upstreams[0]
                .host = "replacement.example.test".into()
        }
        "subscriber_password" => state.users[0].credentials.hy2_password.push('x'),
        "subscriber_reference" => state.users[0].credentials.hy2_resi_cred = Some("r009".into()),
        "reserved_secret" => state.residential.hy2_pool.creds[0].secret.push('x'),
        "reality_uuid" => state.users[0].credentials.vless_uuid = uuid::Uuid::from_u128(93),
        "protocol_scope" => {
            state.users[0].entitlements.protocols = vec![bui_schema::model::Protocol::Hysteria2]
        }
        "service_port" => state.node.ports.hy2_resi += 1,
        "tls" => state.node.domain = "new.example.test".into(),
        "obfs" => {
            state.node.obfs.enabled = true;
            state.node.obfs.password = "fixture-new-obfs".into();
        }
        _ => unreachable!(),
    }
}
fn assert_unavailable_changes_increment_once(old: &State, changes: &[&str]) {
    assert!(path_fingerprint(old, &old.users[0]).is_err());
    let mut missed = Vec::new();
    for change in changes {
        let mut next = old.clone();
        change_unavailable_desired(&mut next, change);
        assert!(
            path_fingerprint(&next, &next.users[0]).is_err(),
            "path must remain unavailable: {change}"
        );
        reconcile_revisions(old, &mut next).unwrap();
        if next.users[0].managed_profile_revision != Some(2) {
            missed.push(*change);
        }
        let published = next.clone();
        reconcile_revisions(&published, &mut next).unwrap();
        assert_eq!(
            next.users[0].managed_profile_revision, published.users[0].managed_profile_revision,
            "repeat {change}"
        );
    }
    assert!(
        missed.is_empty(),
        "desired semantic changes lost during outage: {missed:?}"
    );
}
#[test]
fn disabled_group_desired_binding_supplier_subscriber_protocol_and_service_changes_increment_once()
{
    let mut old = residential();
    old.residential.groups.get_mut("default").unwrap().enabled = false;
    assert_unavailable_changes_increment_once(
        &old,
        &[
            "selected_uuid",
            "slot_index",
            "supplier_password",
            "supplier_endpoint",
            "subscriber_password",
            "subscriber_reference",
            "reserved_secret",
            "reality_uuid",
            "protocol_scope",
            "service_port",
            "tls",
            "obfs",
        ],
    );
}
#[test]
fn missing_credential_desired_supplier_subscriber_and_service_changes_increment_once() {
    let mut old = residential();
    old.users[0].entitlements.protocols = vec![bui_schema::model::Protocol::Hysteria2];
    old.residential.hy2_pool.creds.clear();
    assert_unavailable_changes_increment_once(
        &old,
        &[
            "selected_uuid",
            "slot_index",
            "supplier_password",
            "supplier_endpoint",
            "subscriber_password",
            "subscriber_reference",
            "service_port",
            "tls",
            "obfs",
        ],
    );
}
#[test]
fn unavailable_probe_usage_timestamps_and_unselected_bindings_remain_nonsemantic() {
    for missing_credential in [false, true] {
        let mut old = residential();
        if missing_credential {
            old.users[0].entitlements.protocols = vec![bui_schema::model::Protocol::Hysteria2];
            old.users[0].credentials.hy2_resi_cred = Some("missing".into());
        } else {
            old.residential.groups.get_mut("default").unwrap().enabled = false;
        }
        let mut next = old.clone();
        next.users[0].entitlements.protocols.reverse();
        let protocol = next.users[0].entitlements.protocols[0];
        next.users[0].entitlements.protocols.push(protocol);
        next.users[0].usage.total_bytes = 123;
        next.users[0].usage.last_seen_at = Some("2026-10-03T01:00:00Z".into());
        next.users[0].note = "fixture note".into();
        next.residential.hy2_pool.generation += 1;
        next.residential.hy2_pool.creds[0].released_at = Some("2026-10-03T01:00:00Z".into());
        let group = next.residential.groups.get_mut("default").unwrap();
        group.upstreams[0].verified = Some(
            serde_json::from_value(json!({"ip":"203.0.113.8","at":"2026-10-03T01:00:00Z"}))
                .unwrap(),
        );
        group.upstreams[0].priority = 3;
        group.upstreams[0].ports_allowed = Some(vec![80, 443, 80]);
        let mut unrelated = group.upstreams[0].clone();
        unrelated.id = uuid::Uuid::from_u128(100);
        unrelated.password = "unselected-fixture-secret".into();
        group.upstreams.push(unrelated);
        group.selected_upstream_id = Some(uuid::Uuid::from_u128(100));
        next.node.ports.hy2 += 1;
        reconcile_revisions(&old, &mut next).unwrap();
        assert_eq!(next.users[0].managed_profile_revision, Some(1));
        assert!(path_fingerprint(&next, &next.users[0]).is_err());
    }
}
