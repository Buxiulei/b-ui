//! Offline, real-kernel acceptance for the generated two-bank relay topology.
use bui_schema::model::{AutoEntry, ResidentialGroup, Rule, Slot};
use bui_schema::relay_generation::Bank;
use bui_schema::render::relay::generation::{self, BackendOpts, FrontOpts};
use bui_schema::render::relay::RelayOpts;
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn isolated(command: &mut Command) -> &mut Command {
    #[cfg(unix)]
    command.process_group(0);
    command.stdout(Stdio::piped()).stderr(Stdio::piped())
}

#[cfg(unix)]
fn kill_owned_groups(pid: u32) {
    // Python isolates each supervised kernel/check in its own process group.
    // Snapshot the descendants before killing the supervisor so the last Rust
    // deadline also reaches those groups if Python cannot run its finally.
    let mut descendants = std::collections::BTreeSet::from([pid]);
    if let Ok(snapshot) = Command::new("/bin/ps")
        .args(["-A", "-o", "pid=,ppid="])
        .output()
    {
        let pairs: Vec<(u32, u32)> = String::from_utf8_lossy(&snapshot.stdout)
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
            })
            .collect();
        loop {
            let before = descendants.len();
            for (child, parent) in &pairs {
                if descendants.contains(parent) {
                    descendants.insert(*child);
                }
            }
            if descendants.len() == before {
                break;
            }
        }
    }
    for group in descendants.iter().rev() {
        let _ = Command::new("/bin/kill")
            .args(["-KILL", "--", &format!("-{group}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn wait_bounded(mut child: Child, budget: Duration) -> io::Result<Output> {
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let err = thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).map(|_| bytes)
    });
    let started = Instant::now();
    let mut status = None;
    while started.elapsed() < budget {
        if status.is_none() {
            status = child.try_wait()?;
        }
        if let Some(completed) = status {
            if out.is_finished() && err.is_finished() {
                return Ok(Output {
                    status: completed,
                    stdout: out.join().unwrap()?,
                    stderr: err.join().unwrap()?,
                });
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
    // The Python supervisor normally cleans up every kernel. This last fence
    // also kills descendants if a broken wrapper prevents its cleanup running.
    #[cfg(unix)]
    kill_owned_groups(child.id());
    let _ = child.kill();
    let _ = child.wait();
    out.join().unwrap()?;
    err.join().unwrap()?;
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("kernel regression command exceeded {budget:?}"),
    ))
}

fn bounded_output(command: &mut Command, budget: Duration) -> io::Result<Output> {
    wait_bounded(isolated(command).spawn()?, budget)
}

fn pinned_binary() -> Option<String> {
    let binary = std::env::var("BUI_SING_BOX").unwrap_or_else(|_| "sing-box".into());
    let version = bounded_output(Command::new(&binary).arg("version"), Duration::from_secs(5));
    if let Err(error) = &version {
        assert_ne!(
            error.kind(),
            io::ErrorKind::TimedOut,
            "kernel version probe exceeded its five-second deadline"
        );
    }
    if !version
        .as_ref()
        .map(|output| output.status.success())
        .unwrap_or(false)
    {
        assert!(
            std::env::var_os("CI").is_none(),
            "generation traffic regression requires the pinned sing-box 1.14.2 in CI"
        );
        eprintln!("skipped: sing-box not found; generation traffic regression requires sing-box 1.14.2 and python3");
        return None;
    }
    assert_eq!(
        String::from_utf8_lossy(&version.unwrap().stdout)
            .lines()
            .next(),
        Some("sing-box version 1.14.2"),
        "generation regression requires the pinned sing-box 1.14.2"
    );
    Some(binary)
}

#[test]
fn generations_preserve_tcp_udp_and_half_close_across_partial_publication() {
    let Some(binary) = pinned_binary() else {
        return;
    };
    let mut group: ResidentialGroup = serde_json::from_value(serde_json::json!({
        "enabled": true, "mode": "global",
        "selected_upstream_id": "00000000-0000-0000-0000-000000000001",
        "upstreams": [
            {"id":"00000000-0000-0000-0000-000000000001", "name":"first",
             "kind":"socks5", "host":"127.0.0.1", "port":1080,
             "ports_allowed":[443,12345,12346]},
            {"id":"00000000-0000-0000-0000-000000000002", "name":"second",
             "kind":"socks5", "host":"127.0.0.1", "port":1081,
             "ports_allowed":[443,12345,12346]}
        ]
    }))
    .unwrap();
    for (upstream, domain) in group
        .upstreams
        .iter()
        .zip(["a-block.invalid", "b-block.invalid"])
    {
        group.blacklist.auto.push(AutoEntry {
            upstream_id: upstream.id,
            rule: Rule::Domain(domain.into()),
            hits: 3,
            confirmed_at: "2026-09-30T00:00:00Z".into(),
            last_verified_at: "2026-09-30T00:00:00Z".into(),
            passes: 0,
        });
    }
    let slots: Vec<_> = group
        .upstreams
        .iter()
        .enumerate()
        .map(|(index, upstream)| Slot {
            index: index as u16,
            upstream_id: upstream.id,
        })
        .collect();
    let front = generation::front_config(
        &group,
        &slots,
        &FrontOpts {
            relay: RelayOpts {
                listen_port: 2080,
                api: "127.0.0.1:9091".into(),
                cache_path: "/unused/generation-cache.db".into(),
                server_ip: None,
            },
            bank: Bank::A,
        },
    )
    .unwrap();
    let a = generation::backend_config(
        &group,
        &BackendOpts {
            bank: Bank::A,
            generation: 17,
            server_ip: None,
        },
    )
    .unwrap();
    // Same UUIDs and front topology, different immutable endpoint credentials
    // and policy. The Python supplier labels expose the bank actually used.
    let mut next = group.clone();
    next.upstreams[0].username = "generation-18".into();
    next.upstreams[0].password = "synthetic-only".into();
    next.upstreams[0].ports_allowed = Some(vec![443, 2222, 12345, 12346]);
    next.blacklist.auto[0].rule = Rule::Domain("b-block.invalid".into());
    let b = generation::backend_config(
        &next,
        &BackendOpts {
            bank: Bank::B,
            generation: 18,
            server_ip: None,
        },
    )
    .unwrap();
    let input = serde_json::json!({"front":front,"backends":{"a":a,"b":b}});
    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/relay/generation.py"
    );
    let mut command = Command::new("python3");
    command
        .args([script, &binary])
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .stdin(Stdio::piped());
    let mut child = isolated(&mut command)
        .spawn()
        .expect("start offline generation traffic regression");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    let output = wait_bounded(child, Duration::from_secs(180)).unwrap();
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    print!("{}", String::from_utf8_lossy(&output.stdout));
}

#[test]
fn mixed_http_socks_generation_configs_pass_the_pinned_kernel_check() {
    let Some(binary) = pinned_binary() else {
        return;
    };
    let group: ResidentialGroup = serde_json::from_value(serde_json::json!({
        "enabled": true, "mode": "global",
        "upstreams": [
            {"id":"00000000-0000-0000-0000-000000000001", "name":"http",
             "kind":"http", "host":"127.0.0.1", "port":1080},
            {"id":"00000000-0000-0000-0000-000000000002", "name":"socks",
             "kind":"socks5", "host":"127.0.0.1", "port":1081}
        ]
    }))
    .unwrap();
    let slots: Vec<_> = group
        .upstreams
        .iter()
        .enumerate()
        .map(|(index, upstream)| Slot {
            index: index as u16,
            upstream_id: upstream.id,
        })
        .collect();
    let front = generation::front_config(
        &group,
        &slots,
        &FrontOpts {
            relay: RelayOpts {
                listen_port: 2080,
                api: "127.0.0.1:9091".into(),
                cache_path: "/unused/mixed-generation-cache.db".into(),
                server_ip: None,
            },
            bank: Bank::A,
        },
    )
    .unwrap();
    for (name, config) in [
        ("front", front),
        (
            "bank-a",
            generation::backend_config(
                &group,
                &BackendOpts {
                    bank: Bank::A,
                    generation: 19,
                    server_ip: None,
                },
            )
            .unwrap(),
        ),
        (
            "bank-b",
            generation::backend_config(
                &group,
                &BackendOpts {
                    bank: Bank::B,
                    generation: 20,
                    server_ip: None,
                },
            )
            .unwrap(),
        ),
    ] {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), serde_json::to_vec(&config).unwrap()).unwrap();
        let output = bounded_output(
            Command::new(&binary).args(["check", "-c"]).arg(file.path()),
            Duration::from_secs(10),
        )
        .unwrap();
        assert!(
            output.status.success(),
            "{name} kernel check: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
