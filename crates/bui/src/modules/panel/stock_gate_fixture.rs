//! Explicit opt-in integration against the pinned stock kernel; never runs on a host by default.
use super::gates::{self, GateManifest};
use super::hy2resi::Hy2ResiClient;
use super::testsupport::harness;
use super::{Hy2ResiApi, Shared};
use crate::reconcile::DaemonCtx;
use anyhow::{ensure, Context, Result};
use bui_schema::model::{ReservedCred, Slot, State, Upstream, UpstreamKind};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use uuid::Uuid;

const STOCK_SHA256: &str = "1a60ac17d93042c5a12410cfe83472ddee5084131dfae7a9b9806926ffb84447";

struct Kernel(Child);
impl Kernel {
    fn spawn(binary: &Path, config: &Path, log: &Path) -> Result<Self> {
        let output = std::fs::File::create(log)?;
        Ok(Self(
            Command::new(binary)
                .args(["run", "-c"])
                .arg(config)
                .stdout(output.try_clone()?)
                .stderr(output)
                .kill_on_drop(true)
                .spawn()?,
        ))
    }
    async fn stop(&mut self) -> Result<()> {
        self.0.start_kill()?;
        tokio::time::timeout(Duration::from_secs(3), self.0.wait()).await??;
        Ok(())
    }
}
impl Drop for Kernel {
    fn drop(&mut self) {
        let _ = self.0.start_kill();
    }
}

struct Supplier {
    port: u16,
    task: JoinHandle<()>,
}
impl Drop for Supplier {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn supplier(marker: &'static str) -> Result<Supplier> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let task = tokio::spawn(async move {
        // One request at a time keeps ownership/cleanup bounded and sufficient for this fixture.
        while let Ok((stream, _)) = listener.accept().await {
            let _ = tokio::time::timeout(Duration::from_secs(4), supply(stream, marker)).await;
        }
    });
    Ok(Supplier { port, task })
}
async fn supply(mut stream: TcpStream, marker: &str) -> Result<()> {
    let mut header = [0u8; 2];
    stream.read_exact(&mut header).await?;
    ensure!(header[0] == 5, "supplier requires SOCKS5");
    let mut methods = vec![0; usize::from(header[1])];
    stream.read_exact(&mut methods).await?;
    ensure!(methods.contains(&0), "supplier requires no-auth method");
    stream.write_all(&[5, 0]).await?;
    let mut request = [0u8; 4];
    stream.read_exact(&mut request).await?;
    ensure!(request[..3] == [5, 1, 0], "supplier requires CONNECT");
    let size = match request[3] {
        1 => 4,
        4 => 16,
        3 => usize::from(stream.read_u8().await?),
        _ => anyhow::bail!("invalid SOCKS address"),
    };
    let mut target = vec![0u8; size + 2];
    stream.read_exact(&mut target).await?;
    stream.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0]).await?;
    let mut request = Vec::new();
    while !request.ends_with(b"\r\n\r\n") {
        ensure!(request.len() < 4096, "oversize fixture HTTP request");
        request.push(stream.read_u8().await?);
    }
    ensure!(
        request.starts_with(b"GET /supplier-oracle "),
        "unexpected fixture HTTP target"
    );
    stream
        .write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{marker}",
                marker.len()
            )
            .as_bytes(),
        )
        .await?;
    stream.shutdown().await?;
    Ok(())
}

fn tcp_port() -> Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port())
}
fn udp_port() -> Result<u16> {
    Ok(std::net::UdpSocket::bind("127.0.0.1:0")?
        .local_addr()?
        .port())
}
fn fixture_state() -> State {
    let mut s = crate::testutil::sample_state();
    let group = s.residential.groups.get_mut("default").unwrap();
    group.enabled = true;
    group.upstreams = (0..2u16)
        .map(|i| Upstream {
            id: Uuid::from_u128(u128::from(i) + 1),
            name: format!("fixture-supplier-{i}"),
            kind: UpstreamKind::Socks5,
            host: "127.0.0.1".into(),
            port: 1,
            username: String::new(),
            password: String::new(),
            priority: 100,
            provider: None,
            region: None,
            ports_allowed: None,
            verified: None,
        })
        .collect();
    s.residential.slots = (0..2u16)
        .map(|i| Slot {
            index: i,
            upstream_id: Uuid::from_u128(u128::from(i) + 1),
        })
        .collect();
    let template = s.users[0].clone();
    s.users = (0..4u16)
        .map(|i| {
            let mut u = template.clone();
            u.user_id = Uuid::from_u128(u128::from(i) + 100);
            u.username = format!("fixture-{i}");
            u.credentials.hy2_resi_cred = Some(format!("r{i:03}"));
            u.entitlements.residential.as_mut().unwrap().slot_id =
                Some(Uuid::from_u128(u128::from(i % 2) + 1));
            u.disabled = i == 2;
            if i == 3 {
                u.entitlements.expires_at = Some("2000-01-01T00:00:00Z".into());
            }
            u
        })
        .collect();
    s.residential.hy2_pool.creds = (0..5)
        .map(|i| ReservedCred {
            id: format!("r{i:03}"),
            name: format!("r{i:03}"),
            secret: format!("fixture-secret-{i}"),
            released_at: None,
        })
        .collect();
    s
}
fn wanted() -> BTreeMap<String, String> {
    [
        ("gate-r000", "slot-0-out"),
        ("gate-r001", "slot-1-out"),
        ("gate-r002", "deny"),
        ("gate-r003", "deny"),
        ("gate-r004", "deny"),
    ]
    .into_iter()
    .map(|(a, b)| (a.into(), b.into()))
    .collect()
}
async fn wait_api(api: &Hy2ResiClient) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(8), async {
        while !api.ready().await {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("stock API readiness deadline")?;
    ensure!(
        api.version().await? == "sing-box 1.14.2",
        "stock API version mismatch"
    );
    Ok(())
}
async fn readback(api: &Hy2ResiClient, want: &BTreeMap<String, String>) -> Result<()> {
    let inventory = api.inventory().await?;
    ensure!(
        inventory.selected() == *want,
        "stock exact gate projection mismatch"
    );
    for selector in inventory.gates.values() {
        ensure!(selector.kind == "Selector", "stock gate kind mismatch");
        ensure!(
            selector.all.len() == 9 && selector.all.contains(&"deny".into()),
            "stock gate members incomplete"
        );
    }
    Ok(())
}
async fn probe(
    binary: &Path,
    dir: &Path,
    hy2_port: u16,
    credential: usize,
    expected: Option<&str>,
) -> Result<()> {
    let socks_port = tcp_port()?;
    let config = json!({
        "inbounds":[{"type":"socks","listen":"127.0.0.1","listen_port":socks_port}],
        "outbounds":[{"type":"hysteria2","tag":"hy2","server":"127.0.0.1","server_port":hy2_port,
            "password":format!("r{credential:03}:fixture-secret-{credential}"),
            "tls":{"enabled":true,"server_name":"localhost","insecure":true}}],
        "route":{"final":"hy2"}
    });
    let path = dir.join(format!("client-{credential}.json"));
    std::fs::write(&path, serde_json::to_vec(&config)?)?;
    let mut client = Kernel::spawn(binary, &path, &dir.join(format!("client-{credential}.log")))?;
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            if TcpStream::connect(("127.0.0.1", socks_port)).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .context("fixture client listener deadline")?;
    let http = reqwest::Client::builder()
        .no_proxy()
        .proxy(reqwest::Proxy::all(format!(
            "socks5h://127.0.0.1:{socks_port}"
        ))?)
        .timeout(Duration::from_secs(3))
        .build()?;
    let result = http.get("http://127.0.0.1:9/supplier-oracle").send().await;
    match expected {
        Some(marker) => {
            let response = result.context("allowed HY2 request failed")?;
            ensure!(
                response.status().is_success(),
                "supplier response status mismatch"
            );
            ensure!(
                response.text().await? == marker,
                "request reached wrong supplier slot"
            );
        }
        None => ensure!(
            result.is_err(),
            "denied credential unexpectedly reached supplier"
        ),
    }
    client.stop().await?;
    Ok(())
}

// Catches omission of the Rust barrier, missing candidate gates, swapped active slots,
// revoked grants after restart, and a transport returning the wrong supplier payload.
#[tokio::test]
#[ignore = "requires pinned stock Linux kernel; run scripts/tests/test-residential-stock-gates.sh"]
async fn stock_residential_gate_restore_and_payloads() {
    let result = tokio::time::timeout(Duration::from_secs(75), exercise()).await;
    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => panic!("stock residential fixture failed: {e:#}"),
        Err(_) => panic!("stock residential fixture deadline exceeded"),
    }
}
async fn exercise() -> Result<()> {
    ensure!(cfg!(target_os = "linux"), "stock fixture requires Linux");
    let binary = PathBuf::from(
        std::env::var_os("BUI_TEST_STOCK_SINGBOX_PATH")
            .context("missing explicit stock kernel path")?,
    );
    ensure!(
        std::fs::canonicalize(&binary)? == Path::new("/new-kernel/sing-box"),
        "unapproved stock binary path"
    );
    ensure!(
        crate::kernels::sha256_hex(&std::fs::read(&binary)?) == STOCK_SHA256,
        "stock binary SHA mismatch"
    );
    let h = harness().await;
    let state = fixture_state();
    h.store.update(|s| *s = state.clone()).await?;
    let a = supplier("supplier-A-slot-0").await?;
    let b = supplier("supplier-B-slot-1").await?;
    let hy2_port = udp_port()?;
    let clash = tcp_port()?;
    let v2ray = tcp_port()?;
    std::fs::create_dir_all(&h.paths.certs_dir)?;
    let status = Command::new("openssl")
        .args([
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            "1",
            "-subj",
            "/CN=localhost",
            "-keyout",
        ])
        .arg(h.paths.certs_dir.join("privkey.pem"))
        .arg("-out")
        .arg(h.paths.certs_dir.join("fullchain.pem"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .status()
        .await?;
    ensure!(status.success(), "temporary certificate generation failed");
    let mut config =
        bui_schema::render::hy2_singbox::config(&state.node, &h.paths, &state.residential.hy2_pool);
    // Preserve the actual renderer's routing, logging, users, selectors and deny outbound.
    config["inbounds"][0]["listen"] = json!("127.0.0.1");
    config["inbounds"][0]["listen_port"] = json!(hy2_port);
    config["experimental"]["clash_api"]["external_controller"] =
        json!(format!("127.0.0.1:{clash}"));
    config["experimental"]["v2ray_api"]["listen"] = json!(format!("127.0.0.1:{v2ray}"));
    for outbound in config["outbounds"].as_array_mut().unwrap() {
        match outbound["tag"].as_str() {
            Some("slot-0-out") => outbound["server_port"] = json!(a.port),
            Some("slot-1-out") => outbound["server_port"] = json!(b.port),
            _ => {}
        }
    }
    let bytes = serde_json::to_vec_pretty(&config)?;
    let path = h.dir.path().join("server.json");
    std::fs::write(&path, &bytes)?;
    let manifest = GateManifest::from_config(&bytes)?;
    let checked = Command::new(&binary)
        .args(["check", "-c"])
        .arg(&path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .status()
        .await?;
    ensure!(checked.success(), "stock rejected renderer configuration");
    let api = Hy2ResiClient::with_apis(format!("127.0.0.1:{clash}"), format!("127.0.0.1:{v2ray}"));
    let shared = Shared::new(Box::new(h.xray.clone()), Box::new(h.hy2.clone())).with_hy2resi(
        Box::new(Hy2ResiClient::with_apis(api.clash_api(), api.v2ray_api())),
    );
    let ctx = DaemonCtx {
        store: h.store.clone(),
        runtime: h.runtime.clone(),
        bus: h.app.bus.clone(),
        host: h.host.clone(),
        paths: h.paths.clone(),
    };
    let mut server = Kernel::spawn(&binary, &path, &h.dir.path().join("server-before.log"))?;
    wait_api(&api).await?;
    let denied: BTreeMap<_, _> = wanted()
        .keys()
        .map(|key| (key.clone(), "deny".into()))
        .collect();
    readback(&api, &denied).await?;
    let permit = gates::restore_and_verify(
        &ctx,
        &shared,
        &manifest,
        tokio::time::Instant::now() + Duration::from_secs(8),
    )
    .await?;
    ensure!(
        permit.projection() == &wanted(),
        "Rust restored projection mismatch"
    );
    permit.validate_now(ctx.host.now())?;
    drop(permit);
    readback(&api, &wanted()).await?;
    for (credential, marker) in [
        (0, Some("supplier-A-slot-0")),
        (1, Some("supplier-B-slot-1")),
        (2, None),
        (3, None),
        (4, None),
    ] {
        probe(&binary, h.dir.path(), hy2_port, credential, marker).await?;
    }
    server.stop().await?;
    server = Kernel::spawn(&binary, &path, &h.dir.path().join("server-after.log"))?;
    wait_api(&api).await?;
    readback(&api, &denied).await?;
    let permit = gates::restore_and_verify(
        &ctx,
        &shared,
        &manifest,
        tokio::time::Instant::now() + Duration::from_secs(8),
    )
    .await?;
    ensure!(
        permit.projection() == &wanted(),
        "Rust restored projection mismatch"
    );
    permit.validate_now(ctx.host.now())?;
    drop(permit);
    readback(&api, &wanted()).await?;
    for (credential, marker) in [
        (0, Some("supplier-A-slot-0")),
        (1, Some("supplier-B-slot-1")),
        (2, None),
        (3, None),
        (4, None),
    ] {
        probe(&binary, h.dir.path(), hy2_port, credential, marker).await?;
    }
    server.stop().await?;
    println!("stock_gate_fixture version=1.14.2 sha256={STOCK_SHA256} gates=5 active=2 denied=3 restart_new_requests=verified");
    Ok(())
}
