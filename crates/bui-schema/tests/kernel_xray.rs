//! Xray 配置渲染：结构、用户过滤与真实内核校验。
mod common;

use bui_schema::model::{RealityFront, L4_XRAY_PORT, REALITY_PORT};
use bui_schema::paths::Paths;
use bui_schema::render::xray;

/// 2026-10-30（REALITY 443 + self-steal）：**一个** REALITY inbound `vless`，`public` 时
/// 直接监听 `0.0.0.0:443`，`dest` 指本机 Caddy `127.0.0.1:9443`、`serverNames = [domain]`、
/// `xver: 1`、`sniffing.routeOnly`。
#[test]
fn xray_config_has_one_self_steal_reality_inbound_and_passes_xray_test() {
    let s = common::state("global");
    let cfg = xray::config(&s.node, &s.users, &s.residential, &Paths::default_server());
    let inb = cfg["inbounds"].as_array().unwrap();
    assert_eq!(
        inb.iter()
            .map(|i| i["tag"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["api", "vless"],
        "只剩一个 REALITY inbound"
    );
    let vless = &inb[1];
    assert_eq!(vless["listen"], "0.0.0.0");
    assert_eq!(vless["port"], REALITY_PORT);
    let clients = vless["settings"]["clients"].as_array().unwrap();
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
    // clients 的 id 是 state 里的 base UUID（不是 routed UUID）：
    // Xray 的 ProcessUUID 清零第 6/7 字节后查用户，两个 routed UUID 都映射到它。
    assert!(clients.iter().all(|c| c["id"]
        .as_str()
        .unwrap()
        .split('-')
        .nth(2)
        .is_some_and(|g| g != "0000" && g != "0001")));
    assert!(!clients
        .iter()
        .any(|c| c["id"] == "11111111-1111-0000-8111-111111111111"));
    let reality = &vless["streamSettings"]["realitySettings"];
    assert_eq!(
        reality["dest"], "127.0.0.1:9443",
        "self-steal：未认证连接回落到本机 Caddy"
    );
    assert_eq!(reality["serverNames"], serde_json::json!(["example.com"]));
    assert_eq!(reality["xver"], 1, "给 Caddy 发 PROXY protocol");
    assert_eq!(vless["streamSettings"]["security"], "reality");
    assert_eq!(vless["sniffing"]["routeOnly"], true);
    assert_eq!(
        vless["sniffing"]["destOverride"],
        serde_json::json!(["http", "tls"])
    );
    // 首条是字面 IPv6 的 blackhole（服务端出站一律 IPv4，2026-10-27）
    let rs = cfg["routing"]["rules"].as_array().unwrap();
    assert_eq!(rs[0]["ip"], serde_json::json!(["::/0"]));
    assert_eq!(rs[0]["outboundTag"], "block");
    assert_eq!(cfg["routing"]["domainStrategy"], "AsIs");
    assert!(cfg["outbounds"]
        .as_array()
        .unwrap()
        .iter()
        .any(|o| o["tag"] == "block" && o["protocol"] == "blackhole"));
    // 表尾：resi-route（route 1 → 兜底槽）→ resi-fallback（→ direct）
    let route = &rs[rs.len() - 2];
    assert_eq!(route["ruleTag"].as_str().unwrap(), "resi-route");
    assert_eq!(route["vlessRoute"], "1");
    assert_eq!(route["outboundTag"].as_str().unwrap(), "relay-slot-0");
    let last = rs.last().unwrap();
    assert_eq!(last["ruleTag"].as_str().unwrap(), "resi-fallback");
    assert_eq!(last["outboundTag"].as_str().unwrap(), "direct");
    assert!(last.get("vlessRoute").is_none());
    // 每个住宅用户一条 user + vlessRoute 1
    assert!(rs.iter().filter(|r| r.get("user").is_some()).all(|r| {
        r["vlessRoute"] == "1"
            && r["inboundTag"] == serde_json::json!(["vless"])
            && r["ruleTag"]
                .as_str()
                .is_some_and(|t| t.starts_with("resi-u-"))
    }));
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

/// `front = l4`：前置 L4 占着公网 443，Xray 只监听回环 10443；客户端订阅里的端口仍是 443。
#[test]
fn the_l4_front_listens_on_loopback_only() {
    let mut s = common::state("global");
    s.node.reality.front = RealityFront::L4;
    let cfg = xray::config(&s.node, &s.users, &s.residential, &Paths::default_server());
    let vless = &cfg["inbounds"][1];
    assert_eq!(vless["listen"], "127.0.0.1");
    assert_eq!(vless["port"], L4_XRAY_PORT);
    assert_eq!(
        vless["streamSettings"]["realitySettings"]["dest"],
        "127.0.0.1:9443"
    );
    if !common::have("xray") {
        eprintln!("skipped: xray not found");
        return;
    }
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
    // 结构性的变化（前置形态）必须让哈希变 —— dest/serverNames 由 normalize 固定，
    // 面板不再能改，所以拿 front 当判据。
    let mut s2 = s.clone();
    s2.node.reality.front = RealityFront::L4;
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

/// 加用户 / 改槽只动带 `user` 的规则（gRPC 收敛，不重启）；兜底槽变化才动结构哈希。
#[test]
fn user_churn_does_not_change_the_structural_hash() {
    let mut s = common::state("global");
    bui_schema::slots::sync_slots(&mut s.residential);
    let a = xray::config(&s.node, &s.users, &s.residential, &Paths::default_server());
    // 把每个住宅用户钉到另一条上游：只有 `user` 规则的 outboundTag 变
    let ids: Vec<_> = s
        .residential
        .default_group()
        .unwrap()
        .upstreams
        .iter()
        .map(|u| u.id)
        .collect();
    assert!(ids.len() >= 2);
    for u in s.users.iter_mut() {
        if let Some(r) = u.entitlements.residential.as_mut() {
            r.slot_id = Some(ids[1]);
        }
    }
    let b = xray::config(&s.node, &s.users, &s.residential, &Paths::default_server());
    assert_ne!(a, b, "用户规则变了");
    assert_eq!(
        xray::structural_hash(&a),
        xray::structural_hash(&b),
        "加用户 / 改槽不该重启 xray"
    );
}

/// 三槽 + 每人一条 `user` 规则 + `vlessRoute` 的配置必须过真实 `xray run -test`：
/// `vlessRoute` 与「`user` + `inboundTag` + `vlessRoute` 同一条 field 规则」是本任务
/// 唯一没被内核校验过的写法，必须实测（26.3.27 的 `RoutingRule.vless_route_list`）。
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
    assert_eq!(cfg["outbounds"].as_array().unwrap().len(), 5);
    let rs = cfg["routing"]["rules"].as_array().unwrap();
    assert!(rs.iter().any(|r| r.get("user").is_some()));
    assert!(rs
        .iter()
        .filter(|r| r.get("user").is_some())
        .all(|r| r["ruleTag"].as_str().unwrap().starts_with("resi-u-")));
    assert_eq!(rs[rs.len() - 2]["ruleTag"], "resi-route");
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
