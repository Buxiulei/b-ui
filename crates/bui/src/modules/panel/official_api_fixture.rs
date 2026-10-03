//! Explicit, isolated official-release API experiment; no production accounting adapter.
//! SOCKS transport probes API identity only, not residential HY2 readiness.

// Pure ledger intentionally has no runtime dependencies, so its failure tests can run offline.
mod ledger {
    use std::collections::BTreeMap;
    #[derive(Default, Debug)]
    pub struct Ledger {
        pub totals: BTreeMap<String, (u64, u64)>,
        pub seen: BTreeMap<(i64, String), (String, u64, u64, bool)>,
        pub gap: bool,
        // Raw API reset diagnostics: epoch, event count, closed count, upload sum, download sum.
        // These non-atomic snapshot sums are diagnostic only, never ledger input or a fence.
        pub reset_snapshots: Vec<(i64, usize, usize, i64, i64)>,
    }
    impl Ledger {
        pub fn absolute(
            &mut self,
            epoch: i64,
            id: &str,
            user: &str,
            up: i64,
            down: i64,
            closed: bool,
        ) {
            if epoch <= 0 || id.is_empty() || !matches!(user, "alice" | "bob") || up < 0 || down < 0
            {
                self.gap = true;
                return;
            }
            let row = self
                .seen
                .entry((epoch, id.into()))
                .or_insert((user.into(), 0, 0, !closed));
            if row.0 != user {
                self.gap = true;
                return;
            }
            let total = self.totals.entry(user.into()).or_default();
            total.0 += (up as u64).saturating_sub(row.1);
            total.1 += (down as u64).saturating_sub(row.2);
            row.1 = row.1.max(up as u64);
            row.2 = row.2.max(down as u64);
            row.3 = !closed;
        }
        pub fn delta(&mut self, epoch: i64, id: &str, up: i64, down: i64) {
            let Some((user, old_up, old_down, online)) =
                self.seen.get(&(epoch, id.into())).cloned()
            else {
                self.gap = true;
                return;
            };
            if up < 0 || down < 0 || !online {
                self.gap = true;
                return;
            }
            self.absolute(
                epoch,
                id,
                &user,
                old_up as i64 + up,
                old_down as i64 + down,
                false,
            );
        }
    }
    #[test]
    fn resets_deduplicate_and_closed_tail_is_counted_once() {
        let mut l = Ledger::default();
        l.absolute(1000, "id", "alice", 10, 20, false);
        l.absolute(1000, "id", "alice", 10, 20, false);
        l.delta(1000, "id", 2, 3);
        l.absolute(1000, "id", "alice", 15, 25, true);
        l.absolute(1000, "id", "alice", 15, 25, true);
        assert_eq!(l.totals.get("alice"), Some(&(15, 25)));
        assert!(!l.seen[&(1000, "id".into())].3);
    }
    #[test]
    fn unknown_delta_never_invents_identity() {
        let mut l = Ledger::default();
        l.delta(1000, "unknown", 20, 30);
        assert!(l.gap);
        assert!(l.totals.is_empty());
    }
    #[test]
    fn restart_keeps_ledger_but_isolates_connection_keys() {
        let mut l = Ledger::default();
        l.absolute(1000, "id", "alice", 7, 9, true);
        l.absolute(2000, "id", "alice", 3, 5, true);
        assert_eq!(l.totals.get("alice"), Some(&(10, 14)));
        assert_eq!(l.seen.len(), 2);
    }
    #[test]
    fn unknown_user_or_identity_change_denies_instead_of_reassigning() {
        let mut l = Ledger::default();
        l.absolute(1000, "id", "alice", 7, 9, false);
        l.absolute(1000, "id", "bob", 8, 10, false);
        l.absolute(1000, "empty", "", 30, 40, true);
        assert!(l.gap);
        assert_eq!(l.totals.get("alice"), Some(&(7, 9)));
        assert!(!l.totals.contains_key("bob"));
    }
}

use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::process::{Child, Command};
use tokio::task::{JoinHandle, JoinSet};
use tonic::{transport::Channel, Request, Streaming};
use uuid::Uuid;

// Upstream enum names are fixed by the pinned proto; do not rename generated schema.
#[allow(dead_code, clippy::enum_variant_names)]
mod wire {
    include!(concat!(env!("OUT_DIR"), "/official_api.rs"));
}
use wire::daemon::{self as api, started_service_client::StartedServiceClient};
const BINARY_SHA: &str = "b8610f45abb7e967e195264383f5cbd20fba7821a3c37e3a8c4c5ab6cad28eac";
const INTERVAL_NS: i64 = 1_000_000_000;
const REQUIRED: &[&str] = &[
    "two_users_tcp_udp_ipv4_ipv6",
    "sustained_active",
    "short_connections",
    "repeated_reset",
    "reconnect_dedup",
    "closed_tail",
    "pressure_64",
    "pressure_256",
    "closed_history_over_1000",
    "consumer_task_exit",
    "core_restart_unsettled",
    "gap_gate_and_existing_flow_termination",
];
fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn request<T>(value: T) -> Request<T> {
    let mut r = Request::new(value);
    r.metadata_mut()
        .insert("authorization", "Bearer fixture-g0-only".parse().unwrap());
    r
}
fn port() -> Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port())
}
fn payload(user: &str, index: usize) -> Vec<u8> {
    let nonce = Uuid::new_v4();
    let mut result = format!("{user}:{nonce}:").into_bytes();
    result.extend(std::iter::repeat_n((index % 251) as u8, 73 + index % 193));
    result
}
type Receipts = Arc<Mutex<BTreeMap<String, Value>>>;
struct Targets {
    endpoints: BTreeMap<String, SocketAddr>,
    receipts: Receipts,
    tasks: Vec<JoinHandle<()>>,
}
impl Targets {
    async fn start() -> Result<Self> {
        let receipts: Receipts = Arc::default();
        let mut result = Self {
            endpoints: BTreeMap::new(),
            receipts,
            tasks: Vec::new(),
        };
        for (family, ip) in [("4", "127.0.0.1"), ("6", "::1")] {
            let tcp = TcpListener::bind((ip, 0)).await?;
            result
                .endpoints
                .insert(format!("tcp{family}"), tcp.local_addr()?);
            let receipts = result.receipts.clone();
            result.tasks.push(tokio::spawn(async move {
                let mut children = JoinSet::new();
                loop {
                    tokio::select! {
                        stream = tcp.accept() => {
                            let Ok((mut stream, _)) = stream else { break };
                            let receipts = receipts.clone();
                            children.spawn(async move {
                                while let Ok(length) = stream.read_u32().await {
                                    if length > 65536 { break; }
                                    let mut data = vec![0; length as usize];
                                    if stream.read_exact(&mut data).await.is_err() { break; }
                                    record(&receipts, &data, "tcp", family, length as u64 + 4);
                                    if stream.write_all(&data).await.is_err() { break; }
                                }
                            });
                        }
                        _ = children.join_next(), if !children.is_empty() => {}
                    }
                }
            }));
            let udp = UdpSocket::bind((ip, 0)).await?;
            result
                .endpoints
                .insert(format!("udp{family}"), udp.local_addr()?);
            let receipts = result.receipts.clone();
            result.tasks.push(tokio::spawn(async move {
                let mut data = vec![0; 65536];
                while let Ok((n, peer)) = udp.recv_from(&mut data).await {
                    record(&receipts, &data[..n], "udp", family, n as u64);
                    let _ = udp.send_to(&data[..n], peer).await;
                }
            }));
        }
        Ok(result)
    }
    async fn stop(&mut self) {
        for task in self.tasks.drain(..) {
            task.abort();
            let _ = task.await;
        }
    }
    fn totals(&self) -> BTreeMap<String, (u64, u64)> {
        let mut result: BTreeMap<String, (u64, u64)> = BTreeMap::new();
        for receipt in self.receipts.lock().unwrap().values() {
            let row = result
                .entry(receipt["user"].as_str().unwrap().into())
                .or_default();
            row.0 += receipt["tracked_upload_expected"].as_u64().unwrap();
            row.1 += receipt["length"].as_u64().unwrap();
        }
        result
    }
}
impl Drop for Targets {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}
fn record(receipts: &Receipts, data: &[u8], network: &str, family: &str, up: u64) {
    let user = String::from_utf8_lossy(data)
        .split(':')
        .next()
        .unwrap_or("")
        .to_owned();
    let nonce = String::from_utf8_lossy(data)
        .split(':')
        .nth(1)
        .unwrap_or("")
        .to_owned();
    let hash = digest(data);
    let previous = receipts.lock().unwrap().insert(
        hash.clone(),
        json!({
            "user":user,"nonce":nonce,"network":network,"family":family,"length":data.len(),
            "sha256":hash,"tracked_upload_expected":up
        }),
    );
    // Duplicate receipt is retained as a visible defect, never silently accepted as deduped delivery.
    if previous.is_some() {
        receipts.lock().unwrap().insert(format!("duplicate-{hash}"), json!({"user":user,"nonce":nonce,"network":network,"family":family,"length":data.len(),"sha256":hash,"tracked_upload_expected":up}));
    }
}
fn socks_address(target: SocketAddr) -> Vec<u8> {
    let mut out = Vec::new();
    match target.ip() {
        IpAddr::V4(ip) => {
            out.push(1);
            out.extend(ip.octets());
        }
        IpAddr::V6(ip) => {
            out.push(4);
            out.extend(ip.octets());
        }
    }
    out.extend(target.port().to_be_bytes());
    out
}
async fn socks(
    proxy: u16,
    user: &str,
    target: SocketAddr,
    udp: bool,
) -> Result<(TcpStream, SocketAddr)> {
    let mut stream = TcpStream::connect(("127.0.0.1", proxy)).await?;
    stream.write_all(&[5, 1, 2]).await?;
    let mut auth = [0; 2];
    stream.read_exact(&mut auth).await?;
    ensure!(auth == [5, 2], "SOCKS authentication unavailable");
    let mut credentials = vec![1, user.len() as u8];
    credentials.extend(user.as_bytes());
    credentials.extend([7]);
    credentials.extend(b"g0-pass");
    stream.write_all(&credentials).await?;
    stream.read_exact(&mut auth).await?;
    ensure!(auth == [1, 0], "SOCKS credentials denied");
    let mut command = vec![5, if udp { 3 } else { 1 }, 0];
    command.extend(socks_address(target));
    stream.write_all(&command).await?;
    let mut header = [0; 4];
    stream.read_exact(&mut header).await?;
    ensure!(header[..3] == [5, 0, 0], "SOCKS target rejected");
    let ip = match header[3] {
        1 => {
            let mut b = [0; 4];
            stream.read_exact(&mut b).await?;
            IpAddr::from(b)
        }
        4 => {
            let mut b = [0; 16];
            stream.read_exact(&mut b).await?;
            IpAddr::from(b)
        }
        _ => bail!("unexpected SOCKS relay address"),
    };
    let mut relay = SocketAddr::new(ip, stream.read_u16().await?);
    if relay.ip().is_unspecified() {
        relay.set_ip("127.0.0.1".parse()?);
    }
    Ok((stream, relay))
}
async fn tcp_exchange(stream: &mut TcpStream, data: &[u8]) -> Result<()> {
    // One write avoids a four-byte segment waiting on Nagle/delayed ACK during churn.
    // The on-wire bytes and independent target accounting remain length-prefix + body.
    let mut framed = Vec::with_capacity(data.len() + 4);
    framed.extend_from_slice(&(data.len() as u32).to_be_bytes());
    framed.extend_from_slice(data);
    stream.write_all(&framed).await?;
    let mut response = vec![0; data.len()];
    stream.read_exact(&mut response).await?;
    ensure!(
        digest(&response) == digest(data),
        "TCP target byte/hash mismatch"
    );
    Ok(())
}
async fn transfer(
    proxy: u16,
    user: &str,
    target: SocketAddr,
    network: &str,
    data: &[u8],
) -> Result<()> {
    if network == "tcp" {
        let (mut stream, _) = socks(proxy, user, target, false).await?;
        tcp_exchange(&mut stream, data).await?;
        stream.shutdown().await?;
    } else {
        let (_control, relay) = socks(proxy, user, "127.0.0.1:0".parse()?, true).await?;
        let socket = UdpSocket::bind("127.0.0.1:0").await?;
        let mut packet = vec![0, 0, 0];
        packet.extend(socks_address(target));
        packet.extend(data);
        socket.send_to(&packet, relay).await?;
        let mut response = vec![0; 65536];
        let (n, _) = socket.recv_from(&mut response).await?;
        ensure!(
            n >= 4 && response[..3] == [0, 0, 0],
            "invalid UDP relay response"
        );
        let offset = match response[3] {
            1 => 10,
            4 => 22,
            _ => bail!("unsupported relay response"),
        };
        ensure!(
            n >= offset && digest(&response[offset..n]) == digest(data),
            "UDP target byte/hash mismatch"
        );
    }
    Ok(())
}
async fn udp_exchange(
    socket: &UdpSocket,
    relay: SocketAddr,
    target: SocketAddr,
    data: &[u8],
) -> Result<()> {
    let mut packet = vec![0, 0, 0];
    packet.extend(socks_address(target));
    packet.extend(data);
    socket.send_to(&packet, relay).await?;
    let mut response = vec![0; 65536];
    let (n, _) = socket.recv_from(&mut response).await?;
    ensure!(
        n >= 4 && response[..3] == [0, 0, 0],
        "invalid UDP relay response"
    );
    let offset = match response[3] {
        1 => 10,
        4 => 22,
        _ => bail!("unsupported relay response"),
    };
    ensure!(
        n >= offset && digest(&response[offset..n]) == digest(data),
        "UDP target byte/hash mismatch"
    );
    Ok(())
}
async fn bounded_transfer(
    proxy: u16,
    user: &str,
    target: SocketAddr,
    network: &str,
    data: &[u8],
) -> Result<()> {
    tokio::time::timeout(
        Duration::from_secs(3),
        transfer(proxy, user, target, network, data),
    )
    .await?
}
struct Kernel {
    child: Child,
    pid: u32,
}
impl Kernel {
    async fn start(binary: &Path, config: &Path, log: &Path) -> Result<Self> {
        let check = Command::new(binary)
            .args(["check", "-c"])
            .arg(config)
            .output()
            .await?;
        ensure!(
            check.status.success(),
            "official check failed: {}",
            String::from_utf8_lossy(&check.stderr)
        );
        let out = std::fs::File::create(log)?;
        let child = Command::new(binary)
            .args(["run", "-c"])
            .arg(config)
            .stdout(out.try_clone()?)
            .stderr(out)
            .kill_on_drop(true)
            .spawn()?;
        let pid = child.id().context("missing child PID")?;
        Ok(Self { child, pid })
    }
    async fn stop(&mut self) -> Result<()> {
        if self.child.try_wait()?.is_none() {
            self.child.start_kill()?;
        }
        tokio::time::timeout(Duration::from_secs(3), self.child.wait()).await??;
        Ok(())
    }
}
impl Drop for Kernel {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}
type Client = StartedServiceClient<Channel>;
async fn connect(port: u16) -> Result<Client> {
    for _ in 0..100 {
        if let Ok(client) = StartedServiceClient::connect(format!("http://127.0.0.1:{port}")).await
        {
            return Ok(client);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    bail!("official API not ready")
}
fn consume(ledger: &mut ledger::Ledger, epoch: i64, batch: api::ConnectionEvents) {
    // reset reconstructs the observation; cumulative credited bytes are never erased.
    if batch.reset {
        let metadata = batch
            .events
            .iter()
            .filter_map(|event| event.connection.as_ref());
        let (closed, up, down) = metadata.fold((0, 0, 0), |a, c| {
            (
                a.0 + usize::from(c.closed_at > 0),
                a.1 + c.uplink_total,
                a.2 + c.downlink_total,
            )
        });
        ledger
            .reset_snapshots
            .push((epoch, batch.events.len(), closed, up, down));
    }
    for event in batch.events {
        if let Some(c) = event.connection {
            if c.id != event.id {
                ledger.gap = true;
                continue;
            }
            ledger.absolute(
                epoch,
                &c.id,
                &c.user,
                c.uplink_total,
                c.downlink_total,
                c.closed_at > 0 || event.r#type == 2,
            );
        } else if event.r#type == 1 {
            ledger.delta(epoch, &event.id, event.uplink_delta, event.downlink_delta);
        } else {
            ledger.gap = true;
        } // CLOSED without metadata cannot prove the final tail.
    }
}
struct Observer {
    task: JoinHandle<()>,
    paused: Arc<AtomicBool>,
}
impl Observer {
    async fn start(
        client: &mut Client,
        epoch: i64,
        ledger: Arc<Mutex<ledger::Ledger>>,
    ) -> Result<Self> {
        let mut stream = client
            .subscribe_connections(request(api::SubscribeConnectionsRequest {
                interval: INTERVAL_NS,
            }))
            .await?
            .into_inner();
        let initial = stream
            .message()
            .await?
            .context("empty initial observation")?;
        ensure!(initial.reset, "initial snapshot must reset view");
        consume(&mut ledger.lock().unwrap(), epoch, initial);
        let paused = Arc::new(AtomicBool::new(false));
        let pause = paused.clone();
        let task = tokio::spawn(async move {
            loop {
                while pause.load(Ordering::SeqCst) {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                match stream.message().await {
                    Ok(Some(batch)) => consume(&mut ledger.lock().unwrap(), epoch, batch),
                    _ => {
                        ledger.lock().unwrap().gap = true;
                        break;
                    }
                }
            }
        });
        Ok(Self { task, paused })
    }
    async fn stop(mut self) -> bool {
        self.task.abort();
        (&mut self.task).await.is_err_and(|e| e.is_cancelled())
    }
}
impl Drop for Observer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn mode(client: &mut Client, value: &str) -> Result<()> {
    client
        .set_clash_mode(request(api::ClashMode { mode: value.into() }))
        .await?;
    ensure!(
        client
            .get_clash_mode_status(request(()))
            .await?
            .into_inner()
            .current_mode
            == value,
        "mode readback differs"
    );
    Ok(())
}
struct StableStatus {
    totals: (u64, u64),
    quiet_ms: u64,
}
async fn stable_status(client: &mut Client) -> Result<StableStatus> {
    let mut stream: Streaming<api::Status> = client
        .subscribe_status(request(api::SubscribeStatusRequest {
            interval: INTERVAL_NS,
        }))
        .await?
        .into_inner();
    let mut last = None;
    let mut equal = 0;
    let mut quiet_since = tokio::time::Instant::now();
    for _ in 0..12 {
        let s = tokio::time::timeout(Duration::from_secs(2), stream.message())
            .await??
            .context("status ended")?;
        ensure!(
            s.traffic_available && s.uplink_total >= 0 && s.downlink_total >= 0,
            "traffic status unavailable"
        );
        let current = (s.uplink_total as u64, s.downlink_total as u64);
        if s.connections_in == 0 && s.connections_out == 0 && last == Some(current) {
            equal += 1;
        } else {
            equal = 0;
            quiet_since = tokio::time::Instant::now();
        }
        last = Some(current);
        let quiet = quiet_since.elapsed();
        if equal >= 3 && quiet >= Duration::from_secs(3) {
            return Ok(StableStatus {
                totals: current,
                quiet_ms: quiet.as_millis() as u64,
            });
        }
    }
    bail!("no stable zero-active status fence")
}
fn config(api_port: u16, proxy: u16) -> Value {
    json!({
        "log":{"level":"error"},
        "services":[{"type":"api","tag":"bui-control","listen":"127.0.0.1","listen_port":api_port,"secret":"fixture-g0-only"}],
        "inbounds":[{"type":"socks","tag":"fixture-socks","listen":"127.0.0.1","listen_port":proxy,"users":[{"username":"alice","password":"g0-pass"},{"username":"bob","password":"g0-pass"}]}],
        "outbounds":[{"type":"direct","tag":"direct"}],
        "route":{"rules":[{"clash_mode":"g0-deny","action":"reject"},{"clash_mode":"g0-allow","action":"route","outbound":"direct"}],"final":"direct"},
        "experimental":{"clash_api":{"default_mode":"g0-deny"}}
    })
}

struct DenialProof {
    denied: bool,
    old_tcp_terminated: bool,
    old_udp_terminated: bool,
    probes: usize,
}
async fn probe_denial(
    proxy: u16,
    targets: &Targets,
    held: &mut [(&str, &str, TcpStream)],
    held_udp: &[(&str, TcpStream, UdpSocket, SocketAddr, SocketAddr)],
    nonce_base: usize,
) -> DenialProof {
    let before = targets.receipts.lock().unwrap().len();
    let mut denied = true;
    let mut denial_probes = 0;
    for user in ["alice", "bob"] {
        for family in ["4", "6"] {
            for network in ["tcp", "udp"] {
                let data = payload(user, nonce_base + denial_probes);
                denial_probes += 1;
                if tokio::time::timeout(
                    Duration::from_millis(250),
                    transfer(
                        proxy,
                        user,
                        targets.endpoints[&format!("{network}{family}")],
                        network,
                        &data,
                    ),
                )
                .await
                .is_ok_and(|r| r.is_ok())
                {
                    denied = false;
                }
            }
        }
    }
    let mut old_terminated = true;
    for (user, _, stream) in held {
        let data = payload(user, nonce_base + denial_probes);
        denial_probes += 1;
        // EOF/reset/error is a termination witness; a timeout alone is not.
        if !matches!(
            tokio::time::timeout(Duration::from_millis(250), tcp_exchange(stream, &data)).await,
            Ok(Err(_))
        ) {
            old_terminated = false;
        }
    }
    let mut old_udp_terminated = true;
    for (user, _control, socket, relay, target) in held_udp {
        let data = payload(user, nonce_base + denial_probes);
        denial_probes += 1;
        if tokio::time::timeout(
            Duration::from_millis(250),
            udp_exchange(socket, *relay, *target, &data),
        )
        .await
        .is_ok_and(|r| r.is_ok())
        {
            old_udp_terminated = false;
        }
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    denied &=
        old_udp_terminated && old_terminated && targets.receipts.lock().unwrap().len() == before;
    DenialProof {
        denied,
        old_tcp_terminated: old_terminated,
        old_udp_terminated,
        probes: denial_probes,
    }
}

struct CaseRuntime<'a> {
    binary: &'a Path,
    dir: &'a Path,
    name: &'a str,
}
impl CaseRuntime<'_> {
    async fn run(&self) -> Value {
        let mut targets = match Targets::start().await {
            Ok(t) => t,
            Err(e) => return json!({"name":self.name,"error":e.to_string(),"executed":false}),
        };
        let mut kernels = Vec::new();
        let mut phase = "setup";
        let result = match tokio::time::timeout(
            Duration::from_secs(45),
            self.exercise(&targets, &mut kernels, &mut phase),
        )
        .await
        {
            Ok(value) => value,
            Err(error) => Err(anyhow::anyhow!("case deadline elapsed: {error}")),
        };
        let mut reaped = true;
        let mut pids = Vec::new();
        for kernel in &mut kernels {
            pids.push(kernel.pid);
            if kernel.stop().await.is_err() {
                reaped = false;
            }
        }
        targets.stop().await;
        let mut value = match result {
            Ok(v) => v,
            Err(e) => {
                json!({"name":self.name,"executed":false,"branch_entered":phase!="setup","phase":phase,"error":format!("{e:#}"),"delivery_verified":false,"accounting_complete":false,"gap_detected":false,"actual_gate_denied":false})
            }
        };
        value["children_reaped"] = json!(reaped);
        value["pids"] = json!(pids);
        value["receipts"] = json!(targets
            .receipts
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect::<Vec<_>>());
        value["delivered_bytes_by_user"] = json!(targets.totals());
        value
    }
    async fn exercise(
        &self,
        targets: &Targets,
        kernels: &mut Vec<Kernel>,
        phase: &mut &'static str,
    ) -> Result<Value> {
        let api_port = port()?;
        let proxy = port()?;
        let config_path = self.dir.join(format!("{}.json", self.name));
        std::fs::write(
            &config_path,
            serde_json::to_vec_pretty(&config(api_port, proxy))?,
        )?;
        kernels.push(
            Kernel::start(
                self.binary,
                &config_path,
                &self.dir.join(format!("{}.log", self.name)),
            )
            .await?,
        );
        let mut client = connect(api_port).await?;
        let epoch = client
            .get_started_at(request(()))
            .await?
            .into_inner()
            .started_at;
        ensure!(
            epoch > 1_000_000_000_000,
            "startedAt must be Unix milliseconds"
        );
        let ledger = Arc::new(Mutex::new(ledger::Ledger::default()));
        let mut observer = Some(Observer::start(&mut client, epoch, ledger.clone()).await?);
        mode(&mut client, "g0-allow").await?;
        let mut expected = BTreeMap::new();
        let mut delivered = 0usize;
        let mut held = Vec::new();
        let mut held_udp = Vec::new();
        // Eight established paths survive until the gate probe, including UDP associations.
        for user in ["alice", "bob"] {
            for family in ["4", "6"] {
                let data = payload(user, delivered);
                delivered += 1;
                let (mut stream, _) = socks(
                    proxy,
                    user,
                    targets.endpoints[&format!("tcp{family}")],
                    false,
                )
                .await?;
                *phase = "authenticated_target_transfer";
                tcp_exchange(&mut stream, &data).await?;
                expected.insert(digest(&data), data.len());
                held.push((user, family, stream));
                let (control, relay) = socks(proxy, user, "127.0.0.1:0".parse()?, true).await?;
                let socket = UdpSocket::bind("127.0.0.1:0").await?;
                let data = payload(user, delivered);
                delivered += 1;
                let target = targets.endpoints[&format!("udp{family}")];
                udp_exchange(&socket, relay, target, &data).await?;
                expected.insert(digest(&data), data.len());
                held_udp.push((user, control, socket, relay, target));
            }
        }
        *phase = "required_branch";
        let mut gap_reason = String::new();
        let mut discontinuity = false;
        let pressure = matches!(self.name, "pressure_64" | "pressure_256");
        let eviction = matches!(
            self.name,
            "closed_history_over_1000" | "gap_gate_and_existing_flow_termination"
        );
        if pressure {
            observer
                .as_ref()
                .unwrap()
                .paused
                .store(true, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        if eviction {
            ensure!(
                observer.take().unwrap().stop().await,
                "observer did not stop"
            );
        }
        let count = match self.name {
            "pressure_64" => 192,
            "pressure_256" => 768,
            "closed_history_over_1000" | "gap_gate_and_existing_flow_termination" => 1100,
            "short_connections" => 32,
            _ => 8,
        };
        for i in 0..count {
            let user = if i % 2 == 0 { "alice" } else { "bob" };
            let family = if i % 4 < 2 { "4" } else { "6" };
            let network = if self.name == "two_users_tcp_udp_ipv4_ipv6" && i % 8 >= 4 {
                "udp"
            } else {
                "tcp"
            };
            let data = payload(user, delivered);
            delivered += 1;
            bounded_transfer(
                proxy,
                user,
                targets.endpoints[&format!("{network}{family}")],
                network,
                &data,
            )
            .await?;
            expected.insert(digest(&data), data.len());
            if self.name == "sustained_active" {
                let data = payload("alice", delivered);
                delivered += 1;
                tcp_exchange(&mut held[0].2, &data).await?;
                expected.insert(digest(&data), data.len());
                tokio::time::sleep(Duration::from_millis(120)).await;
            }
            if self.name == "repeated_reset" || (self.name == "reconnect_dedup" && i == 4) {
                ensure!(
                    observer.take().unwrap().stop().await,
                    "observer not stopped"
                );
                observer = Some(Observer::start(&mut client, epoch, ledger.clone()).await?);
            }
        }
        if pressure {
            observer
                .as_ref()
                .unwrap()
                .paused
                .store(false, Ordering::SeqCst);
        }
        if eviction {
            observer = Some(Observer::start(&mut client, epoch, ledger.clone()).await?);
        }
        if self.name == "closed_tail" {
            let data = payload("alice", delivered);
            delivered += 1;
            tcp_exchange(&mut held[0].2, &data).await?;
            expected.insert(digest(&data), data.len());
            held[0].2.shutdown().await?;
        }
        if self.name == "consumer_task_exit" {
            // Parent task owns the child independently of the observer task. Abort+join is the
            // actual consumer death signal; this does not claim OS-process watchdog coverage.
            ensure!(
                observer.take().unwrap().stop().await,
                "consumer task death not witnessed"
            );
            discontinuity = true;
            gap_reason = "supervisor observed consumer task termination".into();
            kernels.last_mut().unwrap().stop().await?;
        } else if self.name == "core_restart_unsettled" {
            ensure!(
                observer.take().unwrap().stop().await,
                "observer not stopped"
            );
            let data = payload("bob", delivered);
            delivered += 1;
            bounded_transfer(proxy, "bob", targets.endpoints["tcp4"], "tcp", &data).await?;
            expected.insert(digest(&data), data.len());
            kernels.last_mut().unwrap().stop().await?;
            kernels.push(
                Kernel::start(self.binary, &config_path, &self.dir.join("restarted.log")).await?,
            );
            client = connect(api_port).await?;
            let restarted = client
                .get_started_at(request(()))
                .await?
                .into_inner()
                .started_at;
            ensure!(restarted != epoch, "restart did not change Unix-ms epoch");
            // New generation starts deny by config; do not allow traffic after lost old counters.
            observer = Some(Observer::start(&mut client, restarted, ledger.clone()).await?);
            discontinuity = true;
            gap_reason = "API epoch changed; unsettled old-generation bytes unrecoverable".into();
        } else {
            mode(&mut client, "g0-deny").await?;
            client.close_all_connections(request(())).await?;
        }
        *phase = "gate_probes";
        let initial_denial = probe_denial(proxy, targets, &mut held, &held_udp, delivered).await;
        let denied = initial_denial.denied;
        let old_terminated = initial_denial.old_tcp_terminated;
        let old_udp_terminated = initial_denial.old_udp_terminated;
        let denial_probes = initial_denial.probes;
        let mut aggregate = None;
        let mut fence = false;
        let mut fence_quiet_ms = Vec::new();
        if self.name != "consumer_task_exit" {
            let receipt_count = targets.receipts.lock().unwrap().len();
            let totals = stable_status(&mut client).await?;
            // Snapshot only after gate probes, closed active paths, and repeated stable totals.
            if let Some(o) = observer.take() {
                ensure!(o.stop().await, "observer not stopped");
            }
            let observed_epoch = if self.name == "core_restart_unsettled" {
                client_epoch(&mut client).await?
            } else {
                epoch
            };
            observer = Some(Observer::start(&mut client, observed_epoch, ledger.clone()).await?);
            let after = stable_status(&mut client).await?;
            fence = denied
                && totals.totals == after.totals
                && targets.receipts.lock().unwrap().len() == receipt_count;
            fence_quiet_ms = vec![totals.quiet_ms, after.quiet_ms];
            aggregate = Some(after.totals);
        }
        if let Some(o) = observer.take() {
            ensure!(o.stop().await, "observer not stopped");
        }
        let (ledger_totals, ledger_gap, reset_snapshots, known_connections) = {
            let l = ledger.lock().unwrap();
            (
                l.totals.clone(),
                l.gap,
                l.reset_snapshots.clone(),
                l.seen.len(),
            )
        };
        let ledger_sum = ledger_totals
            .values()
            .fold((0u64, 0u64), |a, b| (a.0 + b.0, a.1 + b.1));
        let aggregate_gap = fence && !discontinuity && aggregate != Some(ledger_sum);
        if aggregate_gap {
            gap_reason="stable same-generation API total differs from attributed ledger; no redistribution".into();
        }
        let mut delivery = {
            let actual = targets.receipts.lock().unwrap();
            actual.len() == expected.len()
                && expected.iter().all(|(hash, len)| {
                    actual
                        .get(hash)
                        .is_some_and(|v| v["length"].as_u64() == Some(*len as u64))
                })
        };
        let expected_totals = targets.totals();
        let accounting = delivery
            && fence
            && !discontinuity
            && !ledger_gap
            && !aggregate_gap
            && ledger_totals == expected_totals;
        let gap = ledger_gap || aggregate_gap || discontinuity;
        // Keep the existing deny state. These probes occur after the observer has evidence of
        // uncertainty, not merely after an RPC acknowledgement or before gap discovery.
        let post_gap = if gap {
            Some(
                probe_denial(
                    proxy,
                    targets,
                    &mut held,
                    &held_udp,
                    delivered + denial_probes,
                )
                .await,
            )
        } else {
            None
        };
        delivery &= targets.receipts.lock().unwrap().len() == expected.len();
        Ok(
            json!({"name":self.name,"executed":true,"branch_entered":true,"phase":"complete","delivery_verified":delivery,"accounting_complete":accounting,
                "gap_detected":gap,"gap_reason":gap_reason,"actual_gate_denied":denied,"existing_tcp_flows_terminated":old_terminated,"existing_udp_sessions_terminated":old_udp_terminated,
                "denial_probes":denial_probes,
                "post_gap_denial_verified":post_gap.as_ref().map(|proof|proof.denied),
                "post_gap_denial_probes":post_gap.as_ref().map(|proof|proof.probes),
                "gate_trigger":if self.name=="consumer_task_exit"{"supervisor observed task exit then killed owned core"}else if self.name=="core_restart_unsettled"{"restart default deny; changed epoch retains uncertainty"}else{"audit closes admission before stable reconciliation; remains denied after detected gap"},
                "gate_scope":"owned SOCKS fixture only; no atomic admission guarantee or production watchdog claim",
                "stable_fence_verified":fence,"stable_fence_quiet_ms":fence_quiet_ms,
                "reset_snapshot_diagnostics":reset_snapshots,"known_connection_count":known_connections,
                "reset_snapshot_diagnostic_fields":["epoch_ms","event_count","closed_count","upload_sum","download_sum"],
                "epoch_unix_ms":epoch,"ledger_deltas_by_user":ledger_totals,
                "api_instance_totals":aggregate,"oracle_expected_tracked_bytes":expected_totals,"delivered_payloads":delivered,
                "pressure_connections":if pressure {count}else{0},"closed_history_churn":if eviction {count}else{0},
                "pressure_deficit_observed":if pressure {aggregate_gap}else{false},
                "queue_overflow_proven":false,
                "queue_overflow_evidence":"StartedService has no loss sequence/counter; a deficit does not uniquely identify queue drop or GC cleanup",
                "byte_semantics":"SOCKS target TCP framed payload upload length+4, echo download length; UDP payload both directions; SOCKS handshake excluded",
                "conclusion":if accounting {"complete_under_fixture_fence"}else if gap && denied && post_gap.as_ref().is_some_and(|proof|proof.denied) {"controlled_gap_migration_blocked"}else{"FAIL"}
            }),
        )
    }
}
async fn client_epoch(client: &mut Client) -> Result<i64> {
    Ok(client
        .get_started_at(request(()))
        .await?
        .into_inner()
        .started_at)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "explicit isolated official-release fixture; scripts/tests/test-official-api-g0.sh"]
async fn official_api_g0() {
    let result = run_fixture().await;
    assert!(result.is_ok(), "G0 fixture failed: {result:?}");
}
async fn run_fixture() -> Result<()> {
    ensure!(
        std::env::consts::OS == "linux",
        "G0 requires isolated Linux lab"
    );
    ensure!(
        std::env::var("BUI_G0_NETWORK_NONE").as_deref() == Ok("verified-by-wrapper"),
        "use explicit wrapper isolation check"
    );
    let binary =
        PathBuf::from(std::env::var("BUI_G0_BINARY").context("BUI_G0_BINARY missing; never skip")?);
    ensure!(
        digest(&std::fs::read(&binary)?) == BINARY_SHA,
        "official binary digest mismatch"
    );
    let archive = PathBuf::from(
        std::env::var("BUI_G0_ARCHIVE").context("BUI_G0_ARCHIVE missing; never skip")?,
    );
    ensure!(
        digest(&std::fs::read(&archive)?)
            == "b43a1fb1bda131c6653576741ce527eb2bdeab7c9308ca90ee8b972abb7e4a7f",
        "official archive digest mismatch"
    );
    let version = Command::new(&binary).arg("version").output().await?;
    ensure!(
        version.status.success()
            && String::from_utf8_lossy(&version.stdout).lines().next()
                == Some("sing-box version 1.14.2"),
        "wrong official version"
    );
    let output = PathBuf::from(std::env::var("BUI_G0_RESULT").context("BUI_G0_RESULT required")?);
    let dir = tempfile::tempdir()?;
    let mut cases = Vec::new();
    for name in REQUIRED {
        eprintln!("G0_CASE_BEGIN {name}");
        let case = CaseRuntime {
            binary: &binary,
            dir: dir.path(),
            name,
        }
        .run()
        .await;
        eprintln!(
            "G0_CASE_END {name} delivery={} accounting={} gap={} denied={}",
            case["delivery_verified"],
            case["accounting_complete"],
            case["gap_detected"],
            case["actual_gate_denied"]
        );
        cases.push(case);
    }
    let all = |key: &str| cases.iter().all(|c| c[key] == true);
    let complete = all("accounting_complete");
    let children_reaped = all("children_reaped");
    let executed = cases
        .iter()
        .filter(|c| c["executed"] == true)
        .map(|c| c["name"].clone())
        .collect::<Vec<_>>();
    let report = json!({"required_cases":REQUIRED,"attempted_cases":cases.iter().map(|c|c["name"].clone()).collect::<Vec<_>>(),"executed_cases":executed,"zero_case_skips":REQUIRED.len()-executed.len(),
        "identity_attribution_complete":complete,"children_reaped":children_reaped,
        "gap_detected":cases.iter().any(|c|c["gap_detected"]==true),"actual_gate_denied":all("actual_gate_denied"),
        "delivered_bytes_by_user":cases.iter().map(|c|(c["name"].as_str().unwrap(),c["delivered_bytes_by_user"].clone())).collect::<BTreeMap<_,_>>(),
        "ledger_deltas_by_user":cases.iter().map(|c|(c["name"].as_str().unwrap(),c["ledger_deltas_by_user"].clone())).collect::<BTreeMap<_,_>>(),
        "provenance":{"official_archive_verified":true,"archive_sha256":"b43a1fb1bda131c6653576741ce527eb2bdeab7c9308ca90ee8b972abb7e4a7f","binary_sha256":BINARY_SHA,"version":"1.14.2","transport":"socks",
        "branch":std::env::var("BUI_G0_BRANCH").unwrap_or_default(),"fixture_pid":std::process::id(),"source_proto_git":"af6e64c3b69e6132ebaee0e1a3d24e93903f6709",
        "proto_sha256":"4feeac3166f38074888d9a5e04e69c5f92a178d2a2c3e94a963d2999ef4a3994","interval_ns":INTERVAL_NS,
        "consumer_exit_scope":"Tokio observer task abort/join; independent owning fixture task; not OS-process watchdog",
        "scope":"API feasibility only; no HY2 production accounting/admission readiness"},"cases":cases,
        "failure_kind":if executed.len()!=REQUIRED.len(){"fixture_infrastructure"}else if !complete{"accounting_incomplete"}else{"none"},
        "migration_gate":if complete && children_reaped {"PASS"}else{"FAIL"}});
    std::fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
    println!("G0_RESULT {}", output.display());
    ensure!(children_reaped, "G0 child cleanup failed");
    ensure!(
        executed.len() == REQUIRED.len(),
        "G0 fixture infrastructure/partial-case failure; no upstream accounting conclusion"
    );
    ensure!(
        complete,
        "official API cannot establish complete attribution for every required case; see G0 JSON"
    );
    Ok(())
}
