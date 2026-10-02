//! Traffic and mutation evidence for the residential endpoint's required-exit contract.
//!
//! Run explicitly in the network-none Linux fixture container with
//! `BUI_SING_BOX=/new-kernel/sing-box cargo test -p bui-schema --test
//! kernel_relay_required -- --ignored --nocapture`. An explicit run never skips
//! a missing binary or a different kernel version.
use bui_schema::model::{AutoEntry, Pin, ResidentialGroup, Rule, Slot};
use bui_schema::relay_generation::Bank;
use bui_schema::render::relay::generation::{backend_config, front_config, BackendOpts, FrontOpts};
use bui_schema::render::relay::{config, RelayOpts};
use serde_json::{json, Map};
use std::io::Write;
use std::net::TcpListener;
use std::process::{Command, Stdio};

#[test]
#[ignore = "requires upstream sing-box 1.14.2 and the isolated network-none traffic fixture"]
fn stock_residential_routes_reject_fallback_and_preserve_payloads() {
    let binary = std::env::var("BUI_SING_BOX")
        .expect("explicit stock traffic fixture requires BUI_SING_BOX");
    let version = Command::new(&binary)
        .arg("version")
        .output()
        .expect("the explicitly selected stock kernel must be executable");
    assert!(
        version.status.success(),
        "stock kernel version command failed"
    );
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).lines().next(),
        Some("sing-box version 1.14.2"),
        "this fixture only validates the selected upstream 1.14.2"
    );
    // Reserve an arbitrary disallowed business port. No synthetic destination
    // is ever dialed: the Python receiver preserves public target metadata.
    let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let target_port = reservation.local_addr().unwrap().port();
    let mut configs = Map::new();
    let mut check_configs = Map::new();
    for case in [
        "plain",
        "split",
        "disabled",
        "empty",
        "pins_domain",
        "pins_port",
        "auto",
        "ports",
        "mixed",
    ] {
        let mut group: ResidentialGroup = serde_json::from_value(json!({
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
        match case {
            "split" => {
                group.mode = bui_schema::model::ResiMode::Split;
                group.keywords = Some(vec!["inside-split".into()]);
            }
            "disabled" => group.enabled = false,
            "empty" => group.upstreams.clear(),
            "pins_domain" => group.blacklist.pins.push(Pin {
                rule: Rule::Domain("manual-block.invalid".into()),
                note: "fixture domain guard".into(),
                created_at: "2026-10-02T00:00:00Z".into(),
            }),
            "pins_port" => group.blacklist.pins.push(Pin {
                rule: Rule::Port(target_port),
                note: "fixture port guard".into(),
                created_at: "2026-10-02T00:00:00Z".into(),
            }),
            "auto" => {
                for (upstream, domain) in group
                    .upstreams
                    .iter()
                    .zip(["a-block.invalid", "b-block.invalid"])
                {
                    group.blacklist.auto.push(AutoEntry {
                        upstream_id: upstream.id,
                        rule: Rule::Domain(domain.into()),
                        hits: 3,
                        confirmed_at: "2026-10-02T00:00:00Z".into(),
                        last_verified_at: "2026-10-02T00:00:00Z".into(),
                        passes: 0,
                    });
                }
            }
            "ports" => {
                group.upstreams[0].ports_allowed = Some(vec![80, 443]);
                group.upstreams[1].ports_allowed = Some(vec![target_port]);
            }
            "mixed" => group.upstreams[0].kind = bui_schema::model::UpstreamKind::Http,
            "plain" => {}
            _ => unreachable!(),
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
        let opts = RelayOpts {
            listen_port: 2080,
            api: "127.0.0.1:9091".into(),
            cache_path: "/unused/required-cache.db".into(),
            server_ip: Some("203.0.113.50".into()),
        };
        let rendered = config(&group, &slots, &opts);
        check_configs.insert(format!("monolithic-{case}"), rendered.clone());
        configs.insert(case.into(), rendered);
        if ["plain", "mixed", "disabled", "empty"].contains(&case) {
            check_configs.insert(
                format!("front-{case}"),
                front_config(
                    &group,
                    &slots,
                    &FrontOpts {
                        relay: opts,
                        bank: Bank::A,
                    },
                )
                .expect("generation front config"),
            );
            check_configs.insert(
                format!("backend-{case}"),
                backend_config(
                    &group,
                    &BackendOpts {
                        bank: Bank::B,
                        generation: 27,
                        server_ip: Some("203.0.113.50".into()),
                    },
                )
                .expect("generation backend config"),
            );
        }
    }
    let input = json!({"target_port":target_port,"configs":configs,"check_configs":check_configs});
    drop(reservation);
    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/relay/required.py"
    );
    let mut child = Command::new("python3")
        .args([script, &binary])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start required residential traffic fixture");
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
