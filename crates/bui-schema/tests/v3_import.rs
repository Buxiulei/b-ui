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

/// v3 面板允许小数 GB 限额（`web/index.html` 的 `step="0.1"`，`web/server.js` 落盘
/// `parseFloat(x) * 1073741824`），users.json 里会出现浮点字节数；导入必须取整，
/// 不能让一个用户的一个字段把整份导入打挂。
#[test]
fn fractional_byte_limits_are_rounded() {
    let tmp = tempfile::tempdir().unwrap();
    copy_tree(fixture(), tmp.path());
    std::fs::write(
        tmp.path().join("users.json"),
        r#"[
 {"username":"erin","password":"pw-erin-05","uuid":"55555555-5555-4555-8555-555555555555","protocol":"fusion","residential":true,"createdAt":"2026-05-05T00:00:00.000Z","limits":{"trafficLimit":107374182.4,"monthlyLimit":536870912.0},"usage":{"total":1024.7,"monthly":{"2026-09":2048.5}}}
]"#,
    )
    .unwrap();

    let s = bui_schema::v3::import(tmp.path()).unwrap().state;
    let erin = s.users.iter().find(|u| u.username == "erin").unwrap();
    assert_eq!(
        erin.entitlements.traffic_limit.total_bytes,
        Some(107_374_182)
    );
    assert_eq!(
        erin.entitlements.traffic_limit.monthly_bytes,
        Some(536_870_912)
    );
    assert_eq!(erin.usage.total_bytes, 1025);
    assert_eq!(erin.usage.monthly_bytes, 2049);
}

fn copy_tree(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dst = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            std::fs::create_dir_all(&dst).unwrap();
            copy_tree(&entry.path(), &dst);
        } else {
            std::fs::copy(entry.path(), &dst).unwrap();
        }
    }
}
