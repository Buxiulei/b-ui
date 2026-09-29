//! Offline regression for policy isolation on the upstream actually selected at runtime.
use std::io::Write;
use std::net::TcpListener;
use std::process::{Command, Stdio};

#[test]
fn selected_upstream_policy_survives_hot_switches_and_preserves_tcp_and_udp() {
    let binary = std::env::var("BUI_SING_BOX").unwrap_or_else(|_| "sing-box".into());
    if !Command::new(&binary)
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        eprintln!(
            "skipped: {binary} not found; policy traffic regression requires sing-box and python3"
        );
        return;
    }
    // Reserve only the synthetic direct destination; the Python harness relocates
    // every kernel listener and its matching loopback outbound together.
    let target = TcpListener::bind("127.0.0.1:0").unwrap();
    let target_port = target.local_addr().unwrap().port();
    let mut configs = serde_json::Map::new();
    for case in ["ports", "auto", "mixed", "plain"] {
        let mut group: bui_schema::model::ResidentialGroup =
            serde_json::from_value(serde_json::json!({
                "enabled": true, "mode": "global",
                "selected_upstream_id": "00000000-0000-0000-0000-000000000001",
                "upstreams": [
                    {"id":"00000000-0000-0000-0000-000000000001", "name":"A",
                     "kind":"socks5", "host":"127.0.0.1", "port":1080},
                    {"id":"00000000-0000-0000-0000-000000000002", "name":"B",
                     "kind":"socks5", "host":"127.0.0.1", "port":1081}
                ]
            }))
            .unwrap();
        if case == "ports" {
            group.upstreams[0].ports_allowed = Some(vec![80, 443]);
            group.upstreams[1].ports_allowed = Some(vec![target_port]);
        } else if case == "auto" {
            for (upstream, domain) in group
                .upstreams
                .iter()
                .zip(["a-block.invalid", "b-block.invalid"])
            {
                group.blacklist.auto.push(bui_schema::model::AutoEntry {
                    upstream_id: upstream.id,
                    rule: bui_schema::model::Rule::Domain(domain.into()),
                    hits: 3,
                    confirmed_at: "2026-09-29T00:00:00Z".into(),
                    last_verified_at: "2026-09-29T00:00:00Z".into(),
                    passes: 0,
                });
            }
        } else if case == "mixed" {
            group.upstreams[0].kind = bui_schema::model::UpstreamKind::Http;
        }
        let slots: Vec<_> = group
            .upstreams
            .iter()
            .enumerate()
            .map(|(i, u)| bui_schema::model::Slot {
                index: i as u16,
                upstream_id: u.id,
            })
            .collect();
        configs.insert(
            case.into(),
            bui_schema::render::relay::config(
                &group,
                &slots,
                &bui_schema::render::relay::RelayOpts {
                    listen_port: 2080,
                    api: "127.0.0.1:9091".into(),
                    cache_path: "/unused/policy-cache.db".into(),
                    server_ip: None,
                },
            ),
        );
    }
    let input = serde_json::json!({"target_port":target_port,"configs":configs});
    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/relay/policy.py"
    );
    drop(target);
    let mut child = Command::new("python3")
        .args([script, &binary])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start offline policy regression");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    print!("{}", String::from_utf8_lossy(&out.stdout));
}
