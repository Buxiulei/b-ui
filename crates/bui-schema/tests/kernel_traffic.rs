//! 真实数据面回归；无需外网，使用回环上的合成 SOCKS5 上游。
use std::io::Write;
use std::process::{Command, Stdio};

#[test]
#[ignore = "requires the pinned sing-box target and python3"]
fn residential_selector_round_trips_udp_ips_and_domains() {
    let group = serde_json::from_str(
        r#"{"enabled":true,"mode":"global","upstreams":[{
          "id":"00000000-0000-0000-0000-000000000001","name":"synthetic",
          "kind":"socks5","host":"127.0.0.1","port":1080}]}"#,
    )
    .unwrap();
    let cfg = bui_schema::render::relay::config(
        &group,
        &[bui_schema::model::Slot {
            index: 0,
            upstream_id: uuid::Uuid::from_u128(1),
        }],
        &bui_schema::render::relay::RelayOpts {
            listen_port: 2080,
            api: "127.0.0.1:9091".into(),
            cache_path: "/unused/cache.db".into(),
            server_ip: None,
        },
    );
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/relay/udp.py");
    let mut child = Command::new("python3")
        .args([script, "sing-box"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start loopback relay test");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(cfg.to_string().as_bytes())
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
