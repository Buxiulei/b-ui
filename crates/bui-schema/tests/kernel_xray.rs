//! Xray 配置渲染：结构、用户过滤与真实内核校验。
mod common;

use bui_schema::paths::Paths;
use bui_schema::render::xray;

#[test]
fn xray_config_has_two_reality_inbounds_and_passes_xray_test() {
    let mut s = common::state("global");
    bui_schema::slots::sync_slots(&mut s.residential);
    bui_schema::slots::migrate_unassigned(&mut s);
    let cfg = xray::config(&s.node, &s.users, &s.residential, &Paths::default_server());
    let inb = cfg["inbounds"].as_array().unwrap();
    assert_eq!(
        inb.iter()
            .map(|i| i["tag"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["api", "vless-direct", "vless-residential"]
    );
    let clients = inb[1]["settings"]["clients"].as_array().unwrap();
    assert_eq!(
        clients.len(),
        2,
        "只有 alice 与 dave 同时拥有 Reality 与 direct 权益"
    );
    // spec §3.3：clients[].email 必须是 user_id（gRPC AddUser/RemoveUser/QueryStats 的唯一键）
    let emails: Vec<String> = clients
        .iter()
        .map(|c| c["email"].as_str().unwrap().to_string())
        .collect();
    let id_of = |name: &str| {
        s.users
            .iter()
            .find(|u| u.username == name)
            .unwrap()
            .user_id
            .to_string()
    };
    assert_eq!(
        emails,
        vec![id_of("alice"), id_of("dave")],
        "direct credentials must follow explicit grants"
    );
    let residential_emails: Vec<_> = inb[2]["settings"]["clients"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["email"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        residential_emails,
        vec![id_of("alice"), id_of("carol")],
        "residential credentials must follow exact bindings"
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
    assert_eq!(last["outboundTag"].as_str().unwrap(), "blocked");
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
    assert_eq!(cfg["outbounds"].as_array().unwrap().len(), 5);
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

/// This fixture validates Xray's routing default during a missing residential
/// fallback. Local SOCKS replaces REALITY transport; it does not prove REALITY
/// credential authentication or any production endpoint's behavior.
#[test]
fn missing_residential_fallback_rejects_payload_and_detects_direct_default_mutation() {
    if !common::have("xray") {
        assert!(
            std::env::var_os("BUI_TEST_REQUIRE_XRAY").is_none(),
            "required Xray routing fixture cannot skip a missing binary"
        );
        eprintln!("skipped: xray routing fixture binary not found");
        return;
    }
    assert!(
        !run_missing_fallback_case(false),
        "production default must reject residential payload while its fallback is missing"
    );
    assert!(
        run_missing_fallback_case(true),
        "the same independent target oracle must detect a direct-default mutation"
    );
}

/// Returns true only if the independent echo target actually received the
/// residential nonce. Direct checks before and after require complete echoes.
fn run_missing_fallback_case(direct_default_mutation: bool) -> bool {
    use std::io::Write;
    use std::net::{TcpListener, TcpStream};
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    let target = TcpListener::bind("127.0.0.1:0").unwrap();
    let target_addr = target.local_addr().unwrap();
    target.set_nonblocking(true).unwrap();
    let ledger = Arc::new(Mutex::new(Vec::<String>::new()));
    let stopping = Arc::new(AtomicBool::new(false));
    let target_ledger = ledger.clone();
    let target_stopping = stopping.clone();
    let target_thread = std::thread::spawn(move || {
        use std::io::{BufRead, BufReader};
        while !target_stopping.load(Ordering::SeqCst) {
            match target.accept() {
                Ok((mut stream, _)) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(1)))
                        .unwrap();
                    stream
                        .set_write_timeout(Some(Duration::from_secs(1)))
                        .unwrap();
                    let mut payload = String::new();
                    if BufReader::new(&mut stream).read_line(&mut payload).is_ok()
                        && payload.ends_with('\n')
                    {
                        target_ledger.lock().unwrap().push(payload.clone());
                        let _ = stream.write_all(payload.as_bytes());
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("echo target failed: {e}"),
            }
        }
    });
    struct EchoGuard(Arc<AtomicBool>, Option<std::thread::JoinHandle<()>>);
    impl Drop for EchoGuard {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
            self.1.take().unwrap().join().unwrap();
        }
    }
    let _echo = EchoGuard(stopping, Some(target_thread));

    let direct_reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let residential_reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let direct_port = direct_reservation.local_addr().unwrap().port();
    let residential_port = residential_reservation.local_addr().unwrap().port();
    let mut s = common::state("global");
    bui_schema::slots::sync_slots(&mut s.residential);
    bui_schema::slots::migrate_unassigned(&mut s);
    let mut cfg = xray::config(&s.node, &s.users, &s.residential, &Paths::default_server());
    let direct_rule = cfg["routing"]["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["inboundTag"] == serde_json::json!(["vless-direct"]))
        .unwrap()
        .clone();
    cfg["routing"]["rules"] = serde_json::json!([direct_rule]);
    cfg.as_object_mut().unwrap().remove("api");
    cfg["inbounds"] = serde_json::json!([
        {"tag":"vless-direct", "listen":"127.0.0.1", "port":direct_port, "protocol":"socks", "settings":{"auth":"noauth", "udp":false}},
        {"tag":"vless-residential", "listen":"127.0.0.1", "port":residential_port, "protocol":"socks", "settings":{"auth":"noauth", "udp":false}}
    ]);
    if direct_default_mutation {
        let outbounds = cfg["outbounds"].as_array_mut().unwrap();
        let direct = outbounds.iter().position(|o| o["tag"] == "direct").unwrap();
        outbounds.swap(0, direct);
    }
    let file = tempfile::Builder::new().suffix(".json").tempfile().unwrap();
    std::fs::write(file.path(), serde_json::to_vec_pretty(&cfg).unwrap()).unwrap();
    let check = Command::new("xray")
        .args(["run", "-test", "-c"])
        .arg(file.path())
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "routing fixture configuration rejected: {}{}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
    struct KernelGuard(std::process::Child);
    impl Drop for KernelGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    drop(direct_reservation);
    drop(residential_reservation);
    let mut kernel = KernelGuard(
        Command::new("xray")
            .args(["run", "-c"])
            .arg(file.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    while TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, direct_port)).is_err()
        || TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, residential_port)).is_err()
    {
        assert!(
            kernel.0.try_wait().unwrap().is_none(),
            "routing fixture kernel exited before ready"
        );
        assert!(
            Instant::now() < deadline,
            "routing fixture listeners never became ready"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let nonce = uuid::Uuid::new_v4();
    let before = format!("direct-before:{nonce}\n");
    let residential = format!("residential:{nonce}\n");
    let after = format!("direct-after:{nonce}\n");
    assert_eq!(
        xray_socks_echo(direct_port, target_addr, &before).unwrap(),
        before.as_bytes()
    );
    let residential_result = xray_socks_echo(residential_port, target_addr, &residential);
    if direct_default_mutation {
        assert_eq!(
            residential_result.unwrap(),
            residential.as_bytes(),
            "direct-default mutant must reach the same target"
        );
    } else {
        assert!(
            residential_result.is_err(),
            "missing-fallback request must explicitly fail within socket budget"
        );
    }
    assert_eq!(
        xray_socks_echo(direct_port, target_addr, &after).unwrap(),
        after.as_bytes()
    );
    let received = ledger.lock().unwrap();
    assert!(
        received.contains(&before) && received.contains(&after),
        "both independent direct reachability controls must actually reach the target"
    );
    let leaked = received.contains(&residential);
    eprintln!("XRAY_ROUTING_CANARY mutation={direct_default_mutation} direct_controls=2 target_payloads={} residential_received={leaked}", received.len());
    leaked
}

fn xray_socks_echo(
    port: u16,
    target: std::net::SocketAddr,
    payload: &str,
) -> std::io::Result<Vec<u8>> {
    use std::io::{Read, Write};
    use std::net::{Ipv4Addr, SocketAddr, TcpStream};
    use std::time::Duration;
    let mut stream = TcpStream::connect_timeout(
        &SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        Duration::from_secs(1),
    )
    .expect("fixture request must reach the intended Xray listener");
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    stream
        .write_all(&[5, 1, 0])
        .expect("fixture SOCKS greeting must be sent");
    let mut auth = [0; 2];
    stream
        .read_exact(&mut auth)
        .expect("fixture SOCKS greeting must be answered");
    assert_eq!(
        auth,
        [5, 0],
        "fixture no-auth must succeed before route rejection is observed"
    );
    let mut connect = vec![5, 1, 0, 1, 127, 0, 0, 1];
    connect.extend(target.port().to_be_bytes());
    stream
        .write_all(&connect)
        .expect("fixture CONNECT target must be sent");
    let mut reply = [0; 4];
    stream.read_exact(&mut reply)?;
    if reply[0] != 5 || reply[1] != 0 {
        return Err(std::io::Error::other("SOCKS target rejected"));
    }
    let address_len = match reply[3] {
        1 => 4,
        4 => 16,
        3 => {
            let mut len = [0];
            stream.read_exact(&mut len)?;
            usize::from(len[0])
        }
        _ => return Err(std::io::Error::other("invalid SOCKS reply address")),
    };
    let mut address = vec![0; address_len + 2];
    stream.read_exact(&mut address)?;
    stream.write_all(payload.as_bytes())?;
    let mut echo = vec![0; payload.len()];
    stream.read_exact(&mut echo)?;
    Ok(echo)
}
