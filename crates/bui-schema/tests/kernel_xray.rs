//! Xray 配置渲染：结构、用户过滤与真实内核校验。
mod common;

use bui_schema::paths::Paths;
use bui_schema::render::xray;

#[test]
fn xray_config_has_two_reality_inbounds_and_passes_xray_test() {
    let s = common::state("global");
    let cfg = xray::config(&s.node, &s.users, &s.residential, &Paths::default_server());
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
    // 住宅兜底是**最后一条**（前面每个住宅用户各有自己的 `resi-u-<user_id>` 规则）
    let rs = cfg["routing"]["rules"].as_array().unwrap();
    let last = rs.last().unwrap();
    assert_eq!(last["ruleTag"].as_str().unwrap(), "resi-fallback");
    assert_eq!(last["outboundTag"].as_str().unwrap(), "relay-slot-0");
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
    let a = xray::config(&s.node, &s.users, &s.residential, &Paths::default_server());
    let b = xray::config(
        &s.node,
        &s.users[..1],
        &s.residential,
        &Paths::default_server(),
    );
    assert_eq!(xray::structural_hash(&a), xray::structural_hash(&b));
    let mut s2 = s.clone();
    s2.node.reality.dest = "www.apple.com:443".into();
    assert_ne!(
        xray::structural_hash(&a),
        xray::structural_hash(&xray::config(
            &s2.node,
            &s2.users,
            &s2.residential,
            &Paths::default_server()
        ))
    );
}

/// 三槽 + 每人一条 `user` 规则 + `ruleTag` 的配置必须过真实 `xray run -test`：
/// `ruleTag` 与「`user` 和 `inboundTag` 同时出现在一条 field 规则里」是本任务
/// 唯一没被内核校验过的写法，必须实测。
#[test]
fn a_three_slot_routing_table_passes_xray_test() {
    let mut s = common::state("global");
    let g = s.residential.groups.get_mut("default").unwrap();
    let mut third = g.upstreams[0].clone();
    third.id = uuid::Uuid::from_u128(0xdead);
    third.host = "isp3.example.net".into();
    g.upstreams.push(third);
    s.residential.slots = s.residential.groups["default"]
        .upstreams
        .iter()
        .enumerate()
        .map(|(i, u)| bui_schema::model::Slot {
            index: i as u16,
            upstream_id: u.id,
        })
        .collect();
    // 把每个住宅用户按创建顺序轮流落槽，制造出「每槽都有 user 规则」的形态
    bui_schema::slots::migrate_unassigned(&mut s);
    let cfg = xray::config(&s.node, &s.users, &s.residential, &Paths::default_server());
    assert_eq!(cfg["outbounds"].as_array().unwrap().len(), 4);
    let rs = cfg["routing"]["rules"].as_array().unwrap();
    assert!(rs.iter().any(|r| r.get("user").is_some()));
    assert!(rs
        .iter()
        .filter(|r| r.get("user").is_some())
        .all(|r| r["ruleTag"].as_str().unwrap().starts_with("resi-u-")));
    assert_eq!(rs.last().unwrap()["ruleTag"], "resi-fallback");
    if !common::have("xray") {
        eprintln!("skipped: xray not found");
        return;
    }
    // 临时文件必须带 `.json` 后缀：xray 26.x 按扩展名判定配置格式，无后缀直接报错
    // （本文件既有测试已经踩过一次并留了注释，照同一写法）。
    let f = tempfile::Builder::new().suffix(".json").tempfile().unwrap();
    std::fs::write(f.path(), serde_json::to_vec_pretty(&cfg).unwrap()).unwrap();
    let out = std::process::Command::new("xray")
        .args(["run", "-test", "-c"])
        .arg(f.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "xray run -test 失败：{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
