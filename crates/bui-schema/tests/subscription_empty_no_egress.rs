//! An empty subscription must deny a real request, with reachable controls.
//! Run explicitly in the isolated Linux stock sing-box 1.14.2 fixture.

use bui_schema::render::{subscription, SplitRules};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const NONCE: &[u8] = b"bui-empty-feed-must-not-send-this-payload";
const ACK: &[u8] = b"target-confirmed-complete-nonce";

struct Core(Child);
impl Drop for Core {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Target {
    address: Ipv4Addr,
    port: u16,
    requests: Arc<AtomicUsize>,
    done: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Target {
    fn start(address: Ipv4Addr) -> Self {
        let listener = TcpListener::bind((address, 0))
            .expect("the isolated observer address must be configured and bindable");
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let seen = requests.clone();
        let done = Arc::new(AtomicBool::new(false));
        let stopped = done.clone();
        let worker = thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        seen.fetch_add(1, Ordering::SeqCst);
                        stream
                            .set_read_timeout(Some(Duration::from_secs(1)))
                            .unwrap();
                        let mut payload = vec![0; NONCE.len()];
                        if stream.read_exact(&mut payload).is_ok() && payload == NONCE {
                            stream.write_all(ACK).unwrap();
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("target listener failed: {error}"),
                }
            }
        });
        Self {
            address,
            port,
            requests,
            done,
            worker: Some(worker),
        }
    }

    fn reachable_control(&self) {
        let mut stream = TcpStream::connect((self.address, self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream.write_all(NONCE).unwrap();
        let mut reply = vec![0; ACK.len()];
        stream.read_exact(&mut reply).unwrap();
        assert_eq!(
            reply, ACK,
            "control target did not acknowledge the complete nonce"
        );
    }
}

impl Drop for Target {
    fn drop(&mut self) {
        self.done.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}

#[test]
#[ignore = "requires isolated Linux and upstream stock sing-box 1.14.2"]
fn stock_empty_subscription_rejects_reachable_target() {
    let binary = std::env::var("BUI_TEST_STOCK_SINGBOX_PATH")
        .expect("explicit stock sing-box path is required; absence is not a passing test");
    let version = Command::new(&binary).arg("version").output().unwrap();
    assert!(version.status.success());
    assert_eq!(
        String::from_utf8(version.stdout).unwrap().lines().next(),
        Some("sing-box version 1.14.2"),
    );

    let public: Ipv4Addr = std::env::var("BUI_TEST_PUBLIC_OBSERVER_IP")
        .expect("an isolated reachable public-shaped observer is required; private-only coverage is insufficient")
        .parse().unwrap();
    assert!(
        !public.is_private()
            && !public.is_loopback()
            && !public.is_link_local()
            && !public.is_unspecified()
            && !public.is_broadcast()
            && !public.is_multicast(),
        "observer must exercise the public address branch: {public}"
    );
    for address in [Ipv4Addr::LOCALHOST, public] {
        rejects_reachable_target(&binary, address);
    }
    eprintln!("empty-subscription runtime: private+public targets refused; four complete-nonce reachability controls passed");
}

fn rejects_reachable_target(binary: &str, address: Ipv4Addr) {
    let target = Target::start(address);
    target.reachable_control();
    assert_eq!(target.requests.load(Ordering::SeqCst), 1);
    let reserved = TcpListener::bind("127.0.0.1:0").unwrap();
    let mixed_port = reserved.local_addr().unwrap().port();
    let mut config = subscription::singbox(
        &[],
        &SplitRules {
            enabled: false,
            global: false,
            keywords: vec![],
        },
        "",
    );
    // Exercise the generated business rules without requiring a privileged TUN.
    config["inbounds"] = serde_json::json!([{
        "type":"mixed", "tag":"mixed-in", "listen":"127.0.0.1", "listen_port":mixed_port
    }]);
    config["route"]["auto_detect_interface"] = false.into();
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), serde_json::to_vec(&config).unwrap()).unwrap();
    let checked = Command::new(binary)
        .args(["check", "-c"])
        .arg(file.path())
        .output()
        .unwrap();
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    drop(reserved);
    let mut core = Core(
        Command::new(binary)
            .args(["run", "-c"])
            .arg(file.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut client = loop {
        if let Ok(stream) = TcpStream::connect(("127.0.0.1", mixed_port)) {
            break stream;
        }
        assert!(
            core.0.try_wait().unwrap().is_none(),
            "stock core exited before listening"
        );
        assert!(
            Instant::now() < deadline,
            "stock mixed inbound did not become reachable"
        );
        thread::sleep(Duration::from_millis(10));
    };
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    client.write_all(&[5, 1, 0]).unwrap();
    let mut negotiated = [0; 2];
    client.read_exact(&mut negotiated).unwrap();
    assert_eq!(negotiated, [5, 0], "SOCKS listener did not actually run");
    let mut request = vec![5, 1, 0, 1];
    request.extend_from_slice(&address.octets());
    request.extend_from_slice(&target.port.to_be_bytes());
    client.write_all(&request).unwrap();
    let mut reply = [0; 10];
    if client.read_exact(&mut reply).is_ok() && reply[1] == 0 {
        client.write_all(NONCE).unwrap();
        let mut acknowledgement = vec![0; ACK.len()];
        let delivered = client.read_exact(&mut acknowledgement).is_ok();
        assert!(
            !delivered,
            "empty subscription delivered payload to an unauthorized direct target"
        );
    }
    thread::sleep(Duration::from_millis(100));
    assert_eq!(
        target.requests.load(Ordering::SeqCst),
        1,
        "refused subscription opened a direct connection"
    );
    target.reachable_control();
    assert_eq!(
        target.requests.load(Ordering::SeqCst),
        2,
        "negative result lacks a reachable after-control"
    );
}
