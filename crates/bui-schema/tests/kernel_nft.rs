//! 真 Linux 内核回归：所有接口、路由、nft 表都在临时网络命名空间里。
//! 运行：sudo -E cargo test -p bui-schema --test kernel_nft -- --ignored --nocapture
//! 需要 unshare/nsenter/ip/nft/python3 与 CAP_SYS_ADMIN/CAP_NET_ADMIN。
use std::io::Write;
use std::process::{Command, Stdio};

#[test]
#[ignore = "requires Linux network namespace capabilities and nft/ip/python3"]
fn port_hopping_preserves_external_udp_and_accepts_local_ipv4_and_ipv6() {
    let ports = serde_json::from_str(
        r#"{"hy2":10000,"hy2_hop":[20000,30000],"hy2_resi":40000,
        "hy2_resi_hop":[41000,50000],"reality_direct":10001,"reality_resi":10002,"admin":8080}"#,
    )
    .unwrap();
    let rules = bui_schema::render::nft::ruleset(&ports, true);
    let script = format!(
        "{}/tests/fixtures/nft/network.py",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut child = Command::new("unshare")
        .args(["--mount", "--net", "--mount-proc", "python3", &script])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start isolated nft regression");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(rules.as_bytes())
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
