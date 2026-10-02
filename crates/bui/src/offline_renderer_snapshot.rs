//! Test-only pure producer/Plan adapter. Private input values never reach stdout or panic text.
use crate::kernels::{sha256_hex, Manifest};
use crate::modules::{
    core_files::CoreFilesModule, ssh::SshModule, system::SystemModule, units::UnitsModule,
};
use crate::reconcile::{diff, Artifact, Facts, Module, RenderCtx, Unit};
use crate::sys::{CmdOut, Host, JournalFrom, JournalRecord, Proto, StagedWrite};
use anyhow::{ensure, Context, Result};
use bui_schema::{model::State, paths::Paths};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

struct ReadOnlySnapshotHost {
    input: PathBuf,
    observed: Value,
    unknown: Mutex<BTreeSet<String>>,
}
impl ReadOnlySnapshotHost {
    fn reject<T>(&self, label: &str) -> Result<T> {
        self.unknown.lock().unwrap().insert(label.into());
        anyhow::bail!("unsampled or forbidden Host operation: {label}")
    }
    fn known(&self) -> Result<()> {
        ensure!(
            self.unknown.lock().unwrap().is_empty(),
            "snapshot accessed unknown observations"
        );
        Ok(())
    }
    fn observation(&self, path: &Path) -> Result<&Value> {
        let key = path.to_str().context("invalid observation path")?;
        self.observed["files_before"]
            .get(key)
            .or_else(|| self.observed["dynamic_files_before"].get(key))
            .ok_or_else(|| {
                self.unknown.lock().unwrap().insert(format!("file:{key}"));
                anyhow::anyhow!("unsampled file observation")
            })
    }
    fn command(&self, key: &str) -> Result<&Value> {
        let value = self.observed["commands"]
            .get(key)
            .context("unsampled command observation")?;
        ensure!(
            value["rc"].is_i64() && value["stdout"].is_string() && value["stderr"].is_string(),
            "unknown command result"
        );
        Ok(value)
    }
    fn payload(&self, logical: &Path) -> Result<Vec<u8>> {
        let observation = self.observation(logical)?;
        ensure!(
            observation["exists"].as_bool() == Some(true),
            "required input file is absent"
        );
        let relative = self.observed["payload_file_map"]
            .get(logical.to_str().context("invalid input path")?)
            .and_then(Value::as_str)
            .context("missing input payload")?;
        let path = Path::new(relative);
        ensure!(
            path.components()
                .all(|part| matches!(part, Component::Normal(_)))
                && path.starts_with("payload"),
            "unsafe input payload path"
        );
        let bytes = std::fs::read(self.input.join(path))?;
        ensure!(
            Some(bytes.len() as u64) == observation["size"].as_u64(),
            "input payload size mismatch"
        );
        ensure!(
            Some(sha256_hex(&bytes).as_str()) == observation["sha256"].as_str(),
            "input payload SHA mismatch"
        );
        Ok(bytes)
    }
    fn unit_bool(&self, unit: &str, field: &str) -> Result<bool> {
        let full = if unit.contains('.') {
            unit.to_owned()
        } else {
            format!("{unit}.service")
        };
        let prefix = if field == "active_rc" {
            "active"
        } else {
            "enabled"
        };
        let command = self.command(&format!("before:{prefix}:{full}"))?;
        Ok(command["rc"].as_i64().context("unknown unit result")? == 0)
    }
}

impl Host for ReadOnlySnapshotHost {
    fn read_file(&self, path: &Path) -> Result<Option<Vec<u8>>> {
        let value = self.observation(path)?;
        match value["exists"].as_bool() {
            Some(false) => Ok(None),
            Some(true) => self.payload(path).map(Some),
            None => self.reject("read_file unknown"),
        }
    }
    fn file_sha256(&self, path: &Path) -> Result<Option<String>> {
        for value in self.observed["kernels"]
            .as_object()
            .context("missing kernel observations")?
            .values()
        {
            if value["path"].as_str() == path.to_str() {
                return value["sha256"]
                    .as_str()
                    .map(|s| Some(s.into()))
                    .context("unknown kernel SHA");
            }
        }
        self.reject("file_sha256")
    }
    fn read_link(&self, path: &Path) -> Result<Option<PathBuf>> {
        let value = self.observation(path)?;
        match value["exists"].as_bool() {
            Some(false) => Ok(None),
            Some(true) if value["kind"].as_str() == Some("symlink") => Ok(Some(
                value["link_target"]
                    .as_str()
                    .context("missing link target")?
                    .into(),
            )),
            Some(true) => Ok(None),
            None => self.reject("read_link unknown"),
        }
    }
    fn is_symlink(&self, path: &Path) -> Result<bool> {
        Ok(self.observation(path)?["kind"].as_str() == Some("symlink"))
    }
    fn is_immutable(&self, path: &Path) -> Result<bool> {
        let value = self.observed["immutable"]
            .get(path.to_str().context("invalid immutable path")?)
            .context("unsampled immutable observation")?;
        ensure!(
            value["rc"].as_i64() == Some(0),
            "immutable observation failed"
        );
        value["is_immutable"]
            .as_bool()
            .context("unknown immutable result")
    }
    fn is_dir(&self, path: &Path) -> Result<bool> {
        if path == Path::new("/sys/module/nf_conntrack") {
            return self.observed["presence"]["nf_conntrack_is_dir"]
                .as_bool()
                .context("unknown module observation");
        }
        self.reject("is_dir")
    }
    fn unit_is_active(&self, unit: &str) -> Result<bool> {
        self.unit_bool(unit, "active_rc")
    }
    fn unit_is_enabled(&self, unit: &str) -> Result<bool> {
        self.unit_bool(unit, "enabled_rc")
    }
    fn sysctl_get(&self, key: &str) -> Result<Option<String>> {
        let value = self.observed["sysctls"]
            .get(key)
            .context("unsampled sysctl")?;
        ensure!(value["rc"].as_i64() == Some(0), "sysctl observation failed");
        Ok(Some(
            value["stdout"]
                .as_str()
                .context("unknown sysctl")?
                .trim()
                .into(),
        ))
    }
    fn run(&self, program: &str, args: &[&str]) -> Result<CmdOut> {
        // Only recorded nft reads are allowed. diff currently uses Facts/key evidence instead.
        let key = match (program, args) {
            ("nft", ["list", "tables"]) => "facts:nft-tables",
            ("nft", ["-j", "list", "table", "inet", "bui"]) => "baseline:nft-bui-json",
            _ => return self.reject("run"),
        };
        let value = self.command(key)?;
        Ok(CmdOut {
            status: value["rc"].as_i64().unwrap() as i32,
            stdout: value["stdout"].as_str().unwrap().into(),
            stderr: value["stderr"].as_str().unwrap().into(),
        })
    }
    // BEGIN_CANDIDATE_HOST_ADAPTER: baseline lacks these three Host methods.
    fn rename_file(&self, _from: &Path, _to: &Path) -> Result<()> {
        self.reject("rename_file")
    }
    fn sync_parent(&self, _path: &Path) -> Result<()> {
        self.reject("sync_parent")
    }
    fn set_file_mode(&self, _path: &Path, _mode: u32) -> Result<()> {
        self.reject("set_file_mode")
    }
    // END_CANDIDATE_HOST_ADAPTER
    fn write_file(&self, _path: &Path, _content: &[u8], _mode: u32) -> Result<()> {
        self.reject("write_file")
    }
    fn stage_file<'a>(&'a self, _dest: &Path, _mode: u32) -> Result<Box<dyn StagedWrite + 'a>> {
        self.reject("stage_file")
    }
    fn remove_file(&self, _path: &Path) -> Result<()> {
        self.reject("remove_file")
    }
    fn list_dir(&self, _path: &Path) -> Result<Vec<PathBuf>> {
        self.reject("list_dir")
    }
    fn remove_dir_all(&self, _path: &Path) -> Result<()> {
        self.reject("remove_dir_all")
    }
    fn symlink(&self, _target: &Path, _link: &Path) -> Result<()> {
        self.reject("symlink")
    }
    fn set_immutable(&self, _path: &Path, _on: bool) -> Result<()> {
        self.reject("set_immutable")
    }
    fn run_stdin(&self, _program: &str, _args: &[&str], _stdin: &str) -> Result<CmdOut> {
        self.reject("run_stdin")
    }
    fn run_journalctl(&self, _args: &[&str]) -> Result<CmdOut> {
        self.reject("run_journalctl")
    }
    fn which(&self, _program: &str) -> bool {
        let _ = self.reject::<()>("which");
        false
    }
    fn systemd_daemon_reload(&self) -> Result<()> {
        self.reject("systemd_daemon_reload")
    }
    fn systemd(&self, _verb: &str, _unit: &str) -> Result<CmdOut> {
        self.reject("systemd")
    }
    fn unit_exists(&self, _unit: &str) -> Result<bool> {
        self.reject("unit_exists")
    }
    fn unit_property(&self, _unit: &str, _prop: &str) -> Result<Option<String>> {
        self.reject("unit_property")
    }
    fn sysctl_set(&self, _key: &str, _value: &str) -> Result<()> {
        self.reject("sysctl_set")
    }
    fn modprobe(&self, _module: &str) -> Result<()> {
        self.reject("modprobe")
    }
    fn mem_mb(&self) -> Result<u64> {
        self.reject("mem_mb")
    }
    fn arch(&self) -> Result<String> {
        self.reject("arch")
    }
    fn hostname(&self) -> Result<String> {
        self.reject("hostname")
    }
    fn listening_ports(&self, _proto: Proto) -> Result<BTreeSet<u16>> {
        self.reject("listening_ports")
    }
    fn now(&self) -> time::OffsetDateTime {
        panic!("snapshot Host clock is not a Plan input")
    }
    fn journal_read(&self, _units: &[String], _from: &JournalFrom) -> Result<Vec<JournalRecord>> {
        self.reject("journal_read")
    }
}

fn facts(host: &ReadOnlySnapshotHost) -> Result<Facts> {
    let value = &host.observed["facts"];
    let text = |key: &str| {
        value[key]
            .as_str()
            .map(str::to_owned)
            .context("unknown Facts string")
    };
    let boolean = |key: &str| value[key].as_bool().context("unknown Facts boolean");
    // Raw commands must be known, even when their observed RC denotes inactive/absent.
    host.command("facts:ssh-exists")?;
    host.command("facts:resolved-exists")?;
    let nft = host.command("facts:nft-tables")?;
    ensure!(
        nft["rc"].as_i64() == Some(0),
        "nft table observation failed"
    );
    let tables: BTreeSet<_> = nft["stdout"]
        .as_str()
        .unwrap()
        .lines()
        .filter_map(crate::reconcile::parse_nft_table_line)
        .collect();
    let recorded: BTreeSet<String> = serde_json::from_value(value["nft_tables"].clone())?;
    ensure!(tables == recorded, "nft Facts differ from recorded command");
    let raw = host.command("baseline:nft-bui-json")?;
    ensure!(raw["rc"].as_i64() == Some(0), "nft JSON observation failed");
    let parsed: Value = serde_json::from_str(raw["stdout"].as_str().unwrap())?;
    ensure!(
        parsed["nftables"].is_array(),
        "invalid nft JSON observation"
    );
    Ok(Facts {
        mem_mb: value["mem_mb"].as_u64().context("unknown memory")?,
        arch: text("arch")?,
        hostname: text("hostname")?,
        has_ufw: boolean("has_ufw")?,
        ufw_active: boolean("ufw_active")?,
        has_firewalld: boolean("has_firewalld")?,
        firewalld_active: boolean("firewalld_active")?,
        ssh_unit: text("ssh_unit")?,
        ssh_pubkeys: u32::try_from(value["ssh_pubkeys"].as_u64().context("unknown key count")?)?,
        systemd_resolved: boolean("systemd_resolved")?,
        nft_tables: tables,
    })
}
fn kernel_versions(
    host: &ReadOnlySnapshotHost,
    manifest: &Manifest,
    arch: &str,
) -> Result<BTreeMap<String, String>> {
    let mut versions = BTreeMap::new();
    for name in ["hysteria", "xray", "sing-box", "caddy"] {
        let (expected, asset) = manifest.kernel_asset(name, arch)?;
        let value = &host.observed["kernels"][name];
        let path = format!("/opt/b-ui/bin/{name}");
        ensure!(
            value["path"].as_str() == Some(&path),
            "unexpected kernel observation path"
        );
        let sha = value["sha256"].as_str().context("unknown kernel SHA")?;
        ensure!(
            sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid kernel SHA"
        );
        ensure!(
            host.observed["kernel_after_sha256"][name].as_str() == Some(sha),
            "kernel changed during snapshot"
        );
        let version = &value["version_observation"];
        ensure!(
            version["rc"].as_i64() == Some(0),
            "kernel version observation failed"
        );
        let actual = crate::kernels::parse_version(
            name,
            version["stdout"]
                .as_str()
                .context("unknown kernel version")?,
        )
        .context("invalid kernel version output")?;
        ensure!(
            actual == expected && sha == asset.sha256,
            "kernel differs from full manifest"
        );
        versions.insert(name.into(), actual);
    }
    Ok(versions)
}
fn render(state: &State, manifest: &Manifest, ctx: &RenderCtx) -> Vec<Artifact> {
    let producers: Vec<Box<dyn Module>> = vec![
        Box::new(CoreFilesModule::new(Some(manifest.clone()))),
        Box::new(UnitsModule),
        Box::new(SystemModule),
        Box::new(SshModule),
    ];
    producers
        .into_iter()
        .flat_map(|producer| producer.render(state, ctx))
        .collect()
}
fn unit(unit: &Unit) -> Value {
    json!({"name":unit.name,"action":format!("{:?}",unit.action)})
}
fn private_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    std::io::Write::write_all(&mut file, bytes)?;
    Ok(())
}
fn inventory(
    artifacts: &[Artifact],
    output: &Path,
    host: &ReadOnlySnapshotHost,
) -> Result<Vec<Value>> {
    let mut ids = BTreeSet::new();
    let mut entries = Vec::new();
    for (index, artifact) in artifacts.iter().enumerate() {
        ensure!(ids.insert(artifact.id()), "duplicate artifact identity");
        let mut entry = match artifact {
            Artifact::File {
                path,
                content,
                mode,
                immutable,
                restart,
                restart_key,
                verify,
            } => {
                ensure!(
                    host.read_file(path)?.as_deref() == Some(content.as_slice()),
                    "renderer differs from current managed file"
                );
                json!({"type":"File","target":path,"mode":mode,"immutable":immutable,"verify":verify.map(|v|format!("{v:?}")),"restart":restart.as_ref().map(unit),"restart_key_sha256":restart_key.as_ref().map(|k|sha256_hex(k.as_bytes()))})
            }
            Artifact::Unit {
                name,
                dropin,
                content,
            } => {
                let path = Artifact::unit_path(&name.name, dropin.as_deref());
                ensure!(
                    host.read_file(&path)?.as_deref() == Some(content.as_bytes()),
                    "renderer differs from current managed unit"
                );
                json!({"type":"Unit","target":path,"unit":unit(name),"dropin":dropin})
            }
            Artifact::UnitState {
                name,
                enabled,
                active,
            } => json!({"type":"UnitState","target":name,"enabled":enabled,"active":active}),
            Artifact::Sysctl { key, value } => json!({"type":"Sysctl","target":key,"value":value}),
            Artifact::Modprobe { module } => json!({"type":"Modprobe","target":module}),
            Artifact::Binary {
                name,
                version,
                sha256,
                url,
            } => {
                json!({"type":"Binary","target":name,"version":version,"sha256":sha256,"url_sha256":sha256_hex(url.as_bytes())})
            }
            Artifact::Symlink { path, target } => {
                json!({"type":"Symlink","target":path,"link_target":target})
            }
            Artifact::FirewallPorts { ports } => {
                json!({"type":"FirewallPorts","ports":ports.iter().map(|p|json!({"proto":format!("{:?}",p.proto),"from":p.from,"to":p.to})).collect::<Vec<_>>()})
            }
            Artifact::NftTable { family, name, .. } => {
                json!({"type":"NftTable","family":family,"target":name})
            }
            Artifact::Absent { path } => json!({"type":"Absent","target":path}),
        };
        entry["id"] = json!(artifact.id());
        let payload = match artifact {
            Artifact::File { content, .. } => Some(content.as_slice()),
            Artifact::Unit { content, .. } => Some(content.as_bytes()),
            Artifact::NftTable { ruleset, .. } => Some(ruleset.as_bytes()),
            _ => None,
        };
        if let Some(bytes) = payload {
            let relative = format!("payload/{index:03}.bin");
            private_write(&output.join(&relative), bytes)?;
            entry["payload"] = json!(relative);
            entry["size"] = json!(bytes.len());
            entry["payload_sha256"] = json!(sha256_hex(bytes));
        }
        entries.push(entry);
    }
    ensure!(
        entries.iter().filter(|v| v["type"] == "Binary").count() == 4,
        "renderer omitted required kernel assets"
    );
    ensure!(
        entries.iter().filter(|v| v["type"] == "Unit").count() == 6,
        "unexpected unit producers"
    );
    ensure!(
        entries.iter().filter(|v| v["type"] == "Symlink").count() == 3,
        "unexpected link producers"
    );
    Ok(entries)
}
fn startup_noop(state: &State, ctx: &RenderCtx, host: &ReadOnlySnapshotHost) -> Result<()> {
    ensure!(
        !state.node.public_ip.is_empty(),
        "startup needs public IP backfill"
    );
    ensure!(
        state.users.iter().all(|user| user.sub_token.is_some()),
        "startup needs subscription token backfill"
    );
    ensure!(
        state
            .users
            .iter()
            .filter_map(|u| u.credentials.hy2_resi_cred.as_ref())
            .all(|id| state
                .residential
                .hy2_pool
                .creds
                .iter()
                .any(|cred| &cred.id == id)),
        "startup needs dangling credential healing"
    );
    let before = serde_json::to_vec(state)?;
    let mut migrated = state.clone();
    bui_schema::slots::sync_slots(&mut migrated.residential);
    ensure!(
        bui_schema::slots::migrate_unassigned(&mut migrated) == 0,
        "startup reassigns slots"
    );
    let now = time::OffsetDateTime::parse(
        host.observed["started_utc"]
            .as_str()
            .context("missing snapshot timestamp")?,
        &time::format_description::well_known::Rfc3339,
    )?;
    let report = bui_schema::hy2pool::migrate(&mut migrated, now);
    ensure!(
        report.changed == 0 && report.unassigned == 0 && before == serde_json::to_vec(&migrated)?,
        "startup changes credential pool"
    );
    let rendered = serde_json::to_vec_pretty(&bui_schema::render::hy2_singbox::config(
        &state.node,
        &ctx.paths,
        &state.residential.hy2_pool,
    ))?;
    ensure!(
        host.read_file(&ctx.paths.base_dir.join("hy2-residential.json"))?
            .as_deref()
            == Some(rendered.as_slice()),
        "idle secret reroll guard would open"
    );
    Ok(())
}
fn metadata_manifest(manifest: &Manifest) -> Result<Manifest> {
    let mut next = manifest.clone();
    next.version = "4.1.5".into();
    next.tag = Some("v4.1.5".into());
    let names = [
        "bui-linux-amd64",
        "bui-linux-arm64",
        "bui-c-linux-amd64",
        "bui-c-linux-arm64",
    ];
    for name in names {
        let asset = next
            .artifacts
            .get_mut(name)
            .context("missing complete BUI asset metadata")?;
        asset.url = format!("https://fixture.invalid/v4.1.5/{name}");
        asset.sha256 = sha256_hex(name.as_bytes());
    }
    Ok(next)
}
fn exercise_snapshot(input: PathBuf, output: PathBuf) -> Result<()> {
    ensure!(
        input.is_dir() && output.is_dir(),
        "snapshot input/output must be existing directories"
    );
    std::fs::set_permissions(&output, std::fs::Permissions::from_mode(0o700))?;
    ensure!(
        std::fs::read_dir(&output)?.next().is_none(),
        "snapshot output must be empty"
    );
    std::fs::create_dir(output.join("payload"))?;
    std::fs::set_permissions(
        output.join("payload"),
        std::fs::Permissions::from_mode(0o700),
    )?;
    let raw = std::fs::read(input.join("inputs.private.json"))?;
    let observed: Value = serde_json::from_slice(&raw)?;
    ensure!(
        observed["format"].as_str() == Some("bui-renderer-input-v1")
            && observed["consistent_observed_inputs"].as_bool() == Some(true),
        "inconsistent snapshot"
    );
    ensure!(
        observed["errors"].as_array().is_some_and(Vec::is_empty),
        "snapshot collection had errors"
    );
    ensure!(
        observed["files_before"] == observed["files_after"]
            && observed["dynamic_files_before"] == observed["dynamic_files_after"]
            && observed["units_before"] == observed["units_after"]
            && observed["runtime_projection_before_sha256"]
                == observed["runtime_projection_after_sha256"],
        "snapshot consistency proof incomplete"
    );
    let host = ReadOnlySnapshotHost {
        input,
        observed,
        unknown: Mutex::new(BTreeSet::new()),
    };
    let state: State = serde_json::from_slice(&host.payload(Path::new("/opt/b-ui/state.json"))?)?;
    let manifest: Manifest =
        serde_json::from_slice(&host.payload(Path::new("/opt/b-ui/manifest.json"))?)?;
    let paths = Paths::default_server();
    ensure!(
        host.observed["paths"]
            == json!({"base_dir":paths.base_dir,"bin_dir":paths.bin_dir,"certs_dir":paths.certs_dir}),
        "snapshot production Paths mismatch"
    );
    let ctx = RenderCtx {
        account_blocked: crate::modules::panel::users::blocked_set(
            &state,
            &BTreeMap::new(),
            time::OffsetDateTime::parse(
                host.observed["started_utc"]
                    .as_str()
                    .context("snapshot authorization time missing")?,
                &time::format_description::well_known::Rfc3339,
            )?,
        ),
        paths,
        facts: facts(&host)?,
    };
    let versions = kernel_versions(&host, &manifest, &ctx.facts.arch)?;
    let keys: BTreeMap<String, String> =
        serde_json::from_value(host.observed["restart_projection"]["restart_keys"].clone())?;
    let artifacts = render(&state, &manifest, &ctx);
    private_write(
        &output.join("artifacts.private.txt"),
        format!("{artifacts:#?}").as_bytes(),
    )?;
    let entries = inventory(&artifacts, &output, &host)?;
    private_write(
        &output.join("inventory.safe.json"),
        &serde_json::to_vec_pretty(&entries)?,
    )?;
    let plan = diff::plan(
        diff::PlanInput {
            artifacts: &artifacts,
            paths: &ctx.paths,
            keys: &keys,
            installed_versions: &versions,
            facts: &ctx.facts,
        },
        &host,
    )?;
    private_write(
        &output.join("plan.private.txt"),
        format!("{plan:#?}").as_bytes(),
    )?;
    host.known()?;
    private_write(
        &output.join("plan.safe.json"),
        &serde_json::to_vec_pretty(
            &json!({"changes":plan.changes.len(),"keys":plan.keys.len(),"unchanged":plan.unchanged,"unknown_accesses":0}),
        )?,
    )?;
    ensure!(
        plan.changes.is_empty(),
        "raw snapshot Plan is not empty; deployment blocked"
    );
    startup_noop(&state, &ctx, &host)?;
    let mut next_state = state.clone();
    next_state.versions.bui = "4.1.5".into();
    let next_manifest = metadata_manifest(&manifest)?;
    let next_artifacts = render(&next_state, &next_manifest, &ctx);
    ensure!(
        next_artifacts == artifacts,
        "BUI-only metadata changes data-plane artifacts"
    );
    let next_versions = kernel_versions(&host, &next_manifest, &ctx.facts.arch)?;
    let next_plan = diff::plan(
        diff::PlanInput {
            artifacts: &next_artifacts,
            paths: &ctx.paths,
            keys: &keys,
            installed_versions: &next_versions,
            facts: &ctx.facts,
        },
        &host,
    )?;
    ensure!(
        next_plan.changes.is_empty(),
        "BUI-only metadata changes data-plane Plan"
    );
    host.known()?;
    private_write(
        &output.join("proof.safe.json"),
        &serde_json::to_vec_pretty(
            &json!({"input_sha256":sha256_hex(&raw),"inventory_sha256":sha256_hex(&serde_json::to_vec(&entries)?),"raw_plan_empty":true,"metadata_plan_empty":true,"startup_noop":true,"current_managed_bytes_equal":true,"kernel_count":4,"unknown_accesses":0,"nft_proof":"same rendered payload plus original runtime key and sampled table existence; no complete live semantic health claim"}),
        )?,
    )?;
    println!("offline_renderer_snapshot artifacts={} kernels=4 raw_plan_empty=true metadata_plan_empty=true startup_noop=true unknown_accesses=0",artifacts.len());
    Ok(())
}

#[test]
#[ignore = "requires explicit private snapshot paths; use scripts/tests/test-renderer-snapshot.sh"]
fn offline_renderer_snapshot() {
    let input = std::env::var_os("BUI_RENDER_INPUT_DIR").expect("missing BUI_RENDER_INPUT_DIR");
    let output = std::env::var_os("BUI_RENDER_OUTPUT_DIR").expect("missing BUI_RENDER_OUTPUT_DIR");
    if let Err(error) = exercise_snapshot(input.into(), PathBuf::from(&output)) {
        let _ = private_write(
            &PathBuf::from(output).join("failure.private.txt"),
            format!("{error:#}").as_bytes(),
        );
        panic!("offline renderer snapshot rejected; inspect private evidence");
    }
}

#[test]
fn strict_snapshot_unknown_file_is_rejected_instead_of_becoming_absent() {
    let host = ReadOnlySnapshotHost {
        input: PathBuf::new(),
        observed: json!({"files_before":{},"dynamic_files_before":{}}),
        unknown: Mutex::new(BTreeSet::new()),
    };
    assert!(host
        .read_file(Path::new("/opt/b-ui/unobserved.json"))
        .is_err());
    assert!(host.known().is_err());
}
#[test]
fn strict_snapshot_remembers_hash_errors_swallowed_by_diff() {
    let host = ReadOnlySnapshotHost {
        input: PathBuf::new(),
        observed: json!({"kernels":{}}),
        unknown: Mutex::new(BTreeSet::new()),
    };
    assert!(host.known().is_ok());
    let installed = BTreeMap::from([("sing-box".into(), "1.14.2".into())]);
    assert!(crate::kernels::kernel_build_differs(
        &host,
        Path::new("/opt/b-ui/bin"),
        &installed,
        "sing-box",
        "1.14.2",
        "expected"
    ));
    assert!(host.known().is_err());
}

#[test]
fn strict_snapshot_forbids_runtime_mutation() {
    let host = ReadOnlySnapshotHost {
        input: PathBuf::new(),
        observed: json!({}),
        unknown: Mutex::new(BTreeSet::new()),
    };
    assert!(host
        .write_file(Path::new("/etc/forbidden"), b"private", 0o600)
        .is_err());
    assert!(host.systemd("restart", "hysteria-residential").is_err());
    assert!(host.run_stdin("nft", &["-f", "-"], "private").is_err());
    assert!(host.known().is_err());
}
