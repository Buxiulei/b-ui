//! v3 状态导入的验收测试（fixture 为合成的 v3 安装目录）。
use bui_schema::model::*;
use bui_schema::nodes::nodes_for;
use bui_schema::render::subscription;
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

/// 把 `src/` 复制到临时目录，再用给定的 users.json 覆盖（`src/` 与 golden 不动）。
fn fixture_with_users(users_json: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    copy_tree(fixture(), tmp.path());
    std::fs::write(tmp.path().join("users.json"), users_json).unwrap();
    tmp
}

fn legacy_users_json() -> String {
    std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/v3/legacy-user/users.json"
    ))
    .unwrap()
}

fn decoded_uri_list(s: &State, username: &str) -> String {
    use base64::Engine;
    let user = s.users.iter().find(|u| u.username == username).unwrap();
    let nodes = nodes_for(user, &s.node, &s.residential);
    let b64 = subscription::uri_list(&nodes, username);
    String::from_utf8(
        base64::engine::general_purpose::STANDARD
            .decode(b64.trim())
            .unwrap(),
    )
    .unwrap()
}

/// v3 最早期的用户记录（只有 username / password / createdAt / limits）不能再让导入中止：
/// v3 把缺 protocol 渲染成 fusion（`web/server.js:1848`），但缺 uuid 时 VLESS 一个不出
/// （`web/server.js:1826`），实际只发 HY2直连 + HY2住宅；v4 以 `[Hysteria2]` 复现，生成的
/// UUID 不进订阅。
#[test]
fn earliest_v3_user_record_imports_with_v3_semantics() {
    let tmp = fixture_with_users(&legacy_users_json());
    let r = bui_schema::v3::import(tmp.path()).unwrap();
    let s = &r.state;
    assert_eq!(s.users.len(), 2);

    let frank = s.users.iter().find(|u| u.username == "frank").unwrap();
    assert_eq!(frank.entitlements.protocols, vec![Protocol::Hysteria2]);
    assert!(frank.entitlements.direct);
    assert_eq!(
        frank.entitlements.residential.as_ref().unwrap().group_id,
        "default"
    );
    assert_eq!(frank.credentials.hy2_password, "pw-frank-06");
    assert_eq!(frank.created_at, "2026-01-06T00:00:00+00:00");
    // 缺 usage 按 0
    assert_eq!(frank.usage.total_bytes, 0);
    assert_eq!(frank.usage.monthly_bytes, 0);
    // 生成的是合法的随机（v4）UUID，且能原样往返
    let id = frank.credentials.vless_uuid;
    assert_eq!(id.get_version(), Some(uuid::Version::Random));
    assert_eq!(uuid::Uuid::parse_str(&id.to_string()).unwrap(), id);

    assert_eq!(
        r.warnings,
        vec!["用户 frank 在 v3 里没有 VLESS UUID，已生成（不影响现有订阅）".to_string()]
    );

    // 字段齐全的对照组照旧
    let grace = s.users.iter().find(|u| u.username == "grace").unwrap();
    assert_eq!(
        grace.entitlements.protocols,
        vec![Protocol::Hysteria2, Protocol::Reality]
    );
    assert_eq!(
        grace.credentials.vless_uuid.to_string(),
        "77777777-7777-4777-8777-777777777777"
    );
    assert_eq!(grace.usage.total_bytes, 4096);

    // 订阅：与 v3 对这条记录实际渲染的链接逐字相同（`web/server.js:1872-1907` 的 buildHy2Url 与 fusion HY2 分支，
    // 格式同 golden `expected/global/alice.sub.txt` 的后两行），不含 vless://
    let got = decoded_uri_list(s, "frank");
    assert!(!got.contains("vless://"), "{got}");
    let want = [
        "hysteria2://frank:pw-frank-06@example.com:10000?sni=example.com&insecure=0&mport=20000-30000#frank-HY2%E7%9B%B4%E8%BF%9E",
        "hysteria2://frank:pw-frank-06@example.com:40000?sni=example.com&insecure=0&mport=41000-50000#frank-HY2%E4%BD%8F%E5%AE%85",
    ]
    .join("\n");
    assert_eq!(got, want);
}

/// protocol 明确含 REALITY 却没有 uuid：v3 本来就渲染不出可用 VLESS；v4 保留 REALITY 权益、
/// 生成 UUID，并提示该用户刷新订阅。缺 protocol 但有 uuid 的记录仍按 v3 的 fusion 导入。
#[test]
fn reality_user_without_uuid_gets_generated_uuid_and_refresh_warning() {
    let tmp = fixture_with_users(
        r#"[
 {"username":"heidi","password":"pw-heidi-08","uuid":"","protocol":"fusion","createdAt":"2026-05-08T00:00:00.000Z","limits":{}},
 {"username":"ivan","password":"pw-ivan-09","uuid":"99999999-9999-4999-8999-999999999999","createdAt":"2026-05-09T00:00:00.000Z","limits":{}}
]"#,
    );
    let r = bui_schema::v3::import(tmp.path()).unwrap();
    let s = &r.state;

    let heidi = s.users.iter().find(|u| u.username == "heidi").unwrap();
    assert_eq!(
        heidi.entitlements.protocols,
        vec![Protocol::Hysteria2, Protocol::Reality]
    );
    assert_eq!(
        heidi.credentials.vless_uuid.get_version(),
        Some(uuid::Version::Random)
    );
    assert_eq!(
        r.warnings,
        vec![
            "用户 heidi 在 v3 里没有 VLESS UUID，已生成，需该用户刷新订阅以获得 REALITY 节点"
                .to_string()
        ]
    );
    let heidi_sub = decoded_uri_list(s, "heidi");
    assert!(
        heidi_sub.contains(&format!("vless://{}@", heidi.credentials.vless_uuid)),
        "{heidi_sub}"
    );

    let ivan = s.users.iter().find(|u| u.username == "ivan").unwrap();
    assert_eq!(
        ivan.entitlements.protocols,
        vec![Protocol::Hysteria2, Protocol::Reality]
    );
    assert_eq!(
        ivan.credentials.vless_uuid.to_string(),
        "99999999-9999-4999-8999-999999999999"
    );
}

/// 真正无法导入的记录（缺 password）仍然报错。
#[test]
fn user_without_password_still_fails() {
    let tmp = fixture_with_users(
        r#"[{"username":"judy","createdAt":"2026-01-10T00:00:00+00:00","limits":{}}]"#,
    );
    let err = bui_schema::v3::import(tmp.path()).unwrap_err().to_string();
    assert!(err.contains("用户 judy 缺少 password"), "{err}");
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
