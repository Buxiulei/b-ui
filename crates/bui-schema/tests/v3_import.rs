//! v3 状态导入的验收测试（fixture 为合成的 v3 安装目录）。
use bui_schema::model::*;
use std::path::Path;

fn fixture() -> &'static Path {
    Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/v3/src"
    ))
}

#[test]
fn imports_node_params() {
    let s = bui_schema::v3::import(fixture()).unwrap().state;
    assert_eq!(s.schema_version, SCHEMA_VERSION);
    assert_eq!(s.node.domain, "example.com");
    assert_eq!(s.node.ports.hy2, 10000);
    assert_eq!(s.node.ports.hy2_hop, Some((20000, 30000)));
    assert_eq!(s.node.ports.hy2_resi, 40000);
    assert_eq!(s.node.ports.hy2_resi_hop, (41000, 50000));
    assert_eq!(s.node.reality.dest, "www.bing.com:443");
    assert_eq!(
        s.node.reality.short_ids,
        vec!["0123456789abcdef".to_string()]
    );
    assert!(!s.node.obfs.enabled);
}

#[test]
fn maps_users_and_limits() {
    let s = bui_schema::v3::import(fixture()).unwrap().state;
    let alice = s.users.iter().find(|u| u.username == "alice").unwrap();
    assert_eq!(
        alice.entitlements.protocols,
        vec![Protocol::Hysteria2, Protocol::Reality]
    );
    assert!(alice.entitlements.direct);
    assert_eq!(
        alice.entitlements.residential.as_ref().unwrap().group_id,
        "default"
    );
    assert_eq!(alice.usage.total_bytes, 123456);
    assert_eq!(alice.usage.month_key, "2026-09");
    assert_eq!(alice.usage.monthly_bytes, 2345);
    let bob = s.users.iter().find(|u| u.username == "bob").unwrap();
    assert_eq!(bob.entitlements.protocols, vec![Protocol::Hysteria2]);
    assert_eq!(
        bob.entitlements.expires_at.as_deref(),
        Some("2027-01-01T00:00:00.000Z")
    );
    assert_eq!(
        bob.entitlements.traffic_limit.total_bytes,
        Some(107374182400)
    );
    let carol = s.users.iter().find(|u| u.username == "carol").unwrap();
    assert_eq!(carol.entitlements.protocols, vec![Protocol::Reality]);
    assert_eq!(
        carol.entitlements.traffic_limit.monthly_bytes,
        Some(53687091200)
    );
    let dave = s.users.iter().find(|u| u.username == "dave").unwrap();
    assert!(dave.entitlements.residential.is_none());
    assert_eq!(s.users.len(), 4);
}

#[test]
fn maps_residential_pool() {
    let s = bui_schema::v3::import(fixture()).unwrap().state;
    let g = s.residential.default_group().unwrap();
    assert!(g.enabled);
    assert_eq!(g.mode, ResiMode::Global);
    assert_eq!(g.keywords, None);
    assert_eq!(g.upstreams.len(), 2);
    assert_eq!(g.upstreams[0].kind, UpstreamKind::Http);
    assert_eq!(g.upstreams[1].kind, UpstreamKind::Socks5);
    assert_eq!(g.upstreams[0].verified.as_ref().unwrap().ip, "198.51.100.7");
    assert_eq!(g.selected_upstream_id, Some(g.upstreams[0].id));
}

#[test]
fn admin_password_is_hashed_not_stored() {
    let r = bui_schema::v3::import(fixture()).unwrap();
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    let s = r.state;
    assert!(s.admin.password_hash.starts_with("$argon2id$"));
    assert_ne!(s.admin.jwt_secret, "");
}
