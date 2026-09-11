//! Xray 配置渲染：结构、用户过滤与真实内核校验。
mod common;

use bui_schema::paths::Paths;
use bui_schema::render::xray;

#[test]
fn xray_config_has_two_reality_inbounds_and_passes_xray_test() {
    let s = common::state("global");
    let cfg = xray::config(&s.node, &s.users, &Paths::default_server());
    let inb = cfg["inbounds"].as_array().unwrap();
    assert_eq!(
        inb.iter()
            .map(|i| i["tag"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["api", "vless-direct", "vless-residential"]
    );
    let clients = inb[1]["settings"]["clients"].as_array().unwrap();
    assert_eq!(clients.len(), 3, "bob 是 hysteria2-only，不进 xray");
    // spec §3.3：clients[].email 必须是 user_id（gRPC AddUser/RemoveUser/QueryStats 的唯一键）
    let emails: Vec<String> = clients
        .iter()
        .map(|c| c["email"].as_str().unwrap().to_string())
        .collect();
    let ids: Vec<String> = s
        .users
        .iter()
        .filter(|u| {
            u.entitlements
                .protocols
                .contains(&bui_schema::model::Protocol::Reality)
        })
        .map(|u| u.user_id.to_string())
        .collect();
    assert_eq!(emails, ids, "clients[].email 必须是 user_id");
    assert_eq!(
        inb[1]["settings"]["clients"], inb[2]["settings"]["clients"],
        "两个 inbound 必须共用同一份 clients"
    );
    assert_eq!(
        inb[1]["streamSettings"]["realitySettings"]["dest"]
            .as_str()
            .unwrap(),
        "www.bing.com:443"
    );
    assert_eq!(
        cfg["routing"]["rules"][2]["outboundTag"].as_str().unwrap(),
        "relay"
    );
    if !common::have("xray") {
        eprintln!("skipped: xray not found");
        return;
    }
    // 偏离计划：tempfile 必须带 .json 后缀——xray 26.x 按扩展名判定配置格式，
    // 无后缀会直接报错；assert 失败信息也带上 stderr，方便定位内核报的字段。
    let f = tempfile::Builder::new().suffix(".json").tempfile().unwrap();
    std::fs::write(f.path(), serde_json::to_vec_pretty(&cfg).unwrap()).unwrap();
    let out = std::process::Command::new("xray")
        .args(["run", "-test", "-c"])
        .arg(f.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn structural_hash_ignores_clients() {
    let s = common::state("global");
    let a = xray::config(&s.node, &s.users, &Paths::default_server());
    let b = xray::config(&s.node, &s.users[..1], &Paths::default_server());
    assert_eq!(xray::structural_hash(&a), xray::structural_hash(&b));
    let mut s2 = s.clone();
    s2.node.reality.dest = "www.apple.com:443".into();
    assert_ne!(
        xray::structural_hash(&a),
        xray::structural_hash(&xray::config(&s2.node, &s2.users, &Paths::default_server()))
    );
}
