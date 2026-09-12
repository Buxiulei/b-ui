//! `bui upgrade` / `bui upgrade --rollback`（spec §7）。
//!
//! manifest（总纲 C4）是版本的唯一来源：`m.version` 决定要不要换 `bui` 自己，`m.kernels`
//! 决定四个内核；资产一律按架构从 `m.artifacts` 查，没有 Rust target 三元组那一层。
//!
//! 下载全部经 [`Fetcher`]，而 [`crate::kernels::HttpFetcher`] 用的是 `reqwest::blocking`，
//! 在 async 上下文里会 panic——所以本文件的每个下载调用点都在 `tokio::task::spawn_blocking` 里。

use crate::kernels::{Asset, Fetcher, HttpFetcher, Manifest};
use crate::sys::Host;
use bui_schema::paths::Paths;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 每日自检的抖动实现在 `serve.rs`（那边不能反向依赖本模块），这里再导出一层，
/// 让 CLI 侧也有一处稳定入口。
///
/// 计划里写的是 `pub use crate::serve::jitter_secs;`，但 `bui` 是 bin-only crate：`pub use` 出不了
/// crate，非 test 构建里没人用它就是 `unused_imports`（与 dead_code 分属两条 lint，`-D warnings`
/// 下直接失败）。改成等价的转调函数，测试断言的「两条路径行为一致」照旧成立。
pub fn jitter_secs(node_id: uuid::Uuid) -> u64 {
    crate::serve::jitter_secs(node_id)
}

/// 一次升级要做的事：换不换 `bui` 自己 + 哪些内核要换版本。
#[derive(Debug, PartialEq)]
pub struct UpgradePlan {
    pub self_from: String,
    pub self_to: Option<String>,
    /// (二进制名, 现装版本, 目标版本)
    pub kernels: Vec<(String, String, String)>,
}

/// 点分版本号比较（只比数字段；段数不等时缺位当 0，非数字段当 0）。不引新依赖。
pub fn version_lt(a: &str, b: &str) -> bool {
    let seg = |s: &str| -> Vec<u64> {
        s.trim_start_matches('v')
            .split('.')
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    };
    let (x, y) = (seg(a), seg(b));
    for i in 0..x.len().max(y.len()) {
        let (l, r) = (
            x.get(i).copied().unwrap_or(0),
            y.get(i).copied().unwrap_or(0),
        );
        if l != r {
            return l < r;
        }
    }
    false
}

/// 比对 manifest 与现装版本，产出升级计划。
///
/// 顺带消费总纲 C4 的可选字段 `min_upgrade_from`：当前版本低于它就直接报错，
/// 提示先升到那个中间版本（消费方规则在 P1，不在 P5）。
pub fn plan_upgrade(
    m: &Manifest,
    current_bui: &str,
    installed: &BTreeMap<String, String>,
    arch: &str,
) -> anyhow::Result<UpgradePlan> {
    if let Some(min) = m.min_upgrade_from.as_deref() {
        if version_lt(current_bui, min) {
            anyhow::bail!(
                "当前 {current_bui} 低于 manifest 要求的 min_upgrade_from {min}：请先 `bui upgrade --version {min}`，再升到 {}",
                m.version
            );
        }
    }
    // 资产缺失就当场报错，别等下载才发现
    let _ = m.bui_asset(arch)?;
    let self_to = (m.version != current_bui).then(|| m.version.clone());
    let mut kernels = Vec::new();
    for name in crate::kernels::KERNELS {
        // manifest 的 kernels 表用下划线键（sing_box），装在盘上的二进制名用连字符
        if let Some(want) = m.kernels.get(&crate::kernels::kernels_key(name)) {
            let from = installed.get(name).cloned().unwrap_or_default();
            if &from != want {
                kernels.push((name.to_string(), from, want.clone()));
            }
        }
    }
    Ok(UpgradePlan {
        self_from: current_bui.to_string(),
        self_to,
        kernels,
    })
}

/// 下载 → sha256 → 旧版另存 `bin/bui.prev` → 写 `bin/bui`。校验失败不动现装二进制。
pub fn apply_self(
    host: &dyn Host,
    fetcher: &dyn Fetcher,
    asset: &Asset,
    bin_dir: &Path,
) -> anyhow::Result<()> {
    let bytes = fetcher.get_bytes(&asset.url)?;
    let got = crate::kernels::sha256_hex(&bytes);
    if !got.eq_ignore_ascii_case(&asset.sha256) {
        anyhow::bail!(
            "bui 二进制 sha256 不匹配：期望 {}，实际 {got}",
            asset.sha256
        );
    }
    let current = bin_dir.join("bui");
    if let Some(old) = host.read_file(&current)? {
        host.write_file(&bin_dir.join("bui.prev"), &old, 0o755)?;
    }
    host.write_file(&current, &bytes, 0o755)?;
    Ok(())
}

/// `bin/bui.prev` → `bin/bui`，并把最近一份 state 备份恢复成 `state.json`，最后重启 `b-ui`。
pub fn rollback(host: &dyn Host, paths: &Paths) -> anyhow::Result<Vec<String>> {
    let prev = paths.bin_dir.join("bui.prev");
    let bytes = host
        .read_file(&prev)?
        .ok_or_else(|| anyhow::anyhow!("没有 {}，无法回滚二进制", prev.display()))?;
    host.write_file(&paths.bin_dir.join("bui"), &bytes, 0o755)?;
    let mut done = vec![format!("已恢复上一版 bui（{}）", prev.display())];
    let backups = crate::paths::backups_dir(paths);
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&backups)
        .map(|it| it.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default();
    // 备份名是 `state-<stamp>-<nnn>.json`（零填充，Task 2）⇒ 字典序 == 时间序
    entries.sort();
    if let Some(newest) = entries.last() {
        std::fs::copy(newest, crate::paths::state_file(paths))?;
        done.push(format!("已恢复期望态备份 {}", newest.display()));
    }
    let _ = host.systemd("restart", "b-ui");
    done.push("已重启 b-ui".into());
    Ok(done)
}

/// 总纲 C5：`bui upgrade [--version <x.y.z>] [--manifest-url <url|file>] [--rollback]`。
pub async fn run(
    rollback_flag: bool,
    version: Option<String>,
    manifest_url: Option<String>,
    paths: Paths,
    host: Arc<dyn Host>,
) -> anyhow::Result<()> {
    run_with(
        rollback_flag,
        version,
        manifest_url,
        paths,
        host,
        Arc::new(HttpFetcher::new()),
    )
    .await
}

/// 注入 `fetcher` 的版本；每个下载调用点都在 `spawn_blocking` 里。
pub async fn run_with(
    rollback_flag: bool,
    version: Option<String>,
    manifest_url: Option<String>,
    paths: Paths,
    host: Arc<dyn Host>,
    fetcher: Arc<dyn Fetcher>,
) -> anyhow::Result<()> {
    if rollback_flag {
        let (h, p) = (host.clone(), paths.clone());
        let done = tokio::task::spawn_blocking(move || rollback(h.as_ref(), &p)).await??;
        for line in done {
            println!("{line}");
        }
        return Ok(());
    }
    let (h, f, p) = (host.clone(), fetcher.clone(), paths.clone());
    // 总纲 C4 的解析顺序在 kernels 里一处实现：--manifest-url > --version（模板）> $BUI_MANIFEST_URL > latest
    let url = crate::kernels::manifest_url(manifest_url.as_deref(), version.as_deref());
    let want = version.clone();
    let (plan, asset) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let m = Manifest::from_url(f.as_ref(), &url)?;
        if let Some(v) = want.as_deref() {
            // 指定了版本就必须拿到那一版（本地文件或 $BUI_MANIFEST_URL 里可能是别的版本）
            anyhow::ensure!(
                m.version == v.trim_start_matches('v'),
                "manifest 里是 {}，不是请求的 {v}",
                m.version
            );
        }
        let bytes = serde_json::to_vec_pretty(&m)?;
        // 缓存给对账用：内核版本随 manifest 落地
        h.write_file(&crate::paths::manifest_file(&p), &bytes, 0o644)?;
        let installed = crate::kernels::installed_versions(h.as_ref(), &p.bin_dir);
        let arch = h.arch()?;
        let plan = plan_upgrade(&m, env!("CARGO_PKG_VERSION"), &installed, &arch)?;
        let asset = m.bui_asset(&arch)?.clone();
        Ok((plan, asset))
    })
    .await??;
    println!("{}", format_plan(&plan));
    if plan.self_to.is_none() && plan.kernels.is_empty() {
        return Ok(());
    }
    if plan.self_to.is_some() {
        let (h, f, bin) = (host.clone(), fetcher.clone(), paths.bin_dir.clone());
        tokio::task::spawn_blocking(move || apply_self(h.as_ref(), f.as_ref(), &asset, &bin))
            .await??;
    }
    // 守护进程重启后自己会对账（内核随 manifest 落地）；它没在跑就本进程内跑一次
    let socket = PathBuf::from(crate::paths::SOCKET_PATH);
    if crate::ipc::Client::new(&socket).available().await {
        let h = host.clone();
        let _ = tokio::task::spawn_blocking(move || h.systemd("restart", "b-ui")).await;
        println!("已重启 b-ui，升级后的对账由守护进程完成");
        return Ok(());
    }
    println!("守护进程未运行，改为本进程内对账一次");
    crate::serve::reconcile_cli(paths, host, socket, false, false).await
}

/// 升级计划的人读文本。
pub fn format_plan(p: &UpgradePlan) -> String {
    let mut out = Vec::new();
    match &p.self_to {
        Some(to) => out.push(format!("bui          {} → {to}", p.self_from)),
        None => out.push(format!("bui          {}（已最新）", p.self_from)),
    }
    for (name, from, to) in &p.kernels {
        let from = if from.is_empty() {
            "（未装）"
        } else {
            from
        };
        out.push(format!("{name:<12} {from} → {to}"));
    }
    if p.self_to.is_none() && p.kernels.is_empty() {
        out.push("无需升级".into());
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernels::{Asset, Manifest};
    use crate::sys::{fake::FakeHost, Host};
    use pretty_assertions::assert_eq;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    struct F(Mutex<Vec<(String, Vec<u8>)>>);
    impl crate::kernels::Fetcher for F {
        fn get_bytes(&self, url: &str) -> anyhow::Result<Vec<u8>> {
            self.0
                .lock()
                .unwrap()
                .iter()
                .find(|(u, _)| u == url)
                .map(|(_, b)| b.clone())
                .ok_or_else(|| anyhow::anyhow!("404 {url}"))
        }
    }

    /// 总纲 C4 形状；只放 amd64 资产，用来顺带测「缺架构资产直接报错」
    fn manifest(bui: &str, sb: &str, sum: &str) -> Manifest {
        let a = |u: &str| Asset {
            url: u.into(),
            sha256: sum.into(),
        };
        Manifest {
            version: bui.into(),
            kernels: BTreeMap::from([("sing_box".to_string(), sb.to_string())]),
            artifacts: BTreeMap::from([
                ("bui-linux-amd64".to_string(), a("https://x/bui")),
                ("sing-box-linux-amd64".to_string(), a("https://x/sb")),
            ]),
            min_upgrade_from: None,
        }
    }

    #[test]
    fn bui_asset_is_keyed_by_arch_not_by_target_triple() {
        let m = manifest("4.0.1", "1.13.19", "00");
        assert_eq!(m.bui_asset("x86_64").unwrap().url, "https://x/bui");
        assert!(
            m.bui_asset("aarch64").is_err(),
            "manifest 没放 arm64 资产就该当场报错"
        );
        assert!(m.bui_asset("armv7l").is_err(), "架构本身不支持");
        assert!(
            plan_upgrade(&m, "4.0.0", &BTreeMap::new(), "aarch64").is_err(),
            "缺资产不许出计划"
        );
    }

    #[test]
    fn version_compare_handles_uneven_segments() {
        assert!(version_lt("4.0.0", "4.0.1"));
        assert!(version_lt("4.0", "4.0.1"));
        assert!(version_lt("3.9.9", "4.0.0"));
        assert!(!version_lt("4.0.1", "4.0.1"));
        assert!(!version_lt("4.1.0", "4.0.9"));
        assert!(!version_lt("4.0.10", "4.0.9"), "按数字比，不是字典序");
    }

    #[test]
    fn min_upgrade_from_refuses_to_skip_the_required_intermediate_version() {
        // 总纲 C4 的可选字段：低于此版本必须先升到它。消费方规则在 P1。
        let mut m = manifest("4.2.0", "1.13.19", "00");
        m.min_upgrade_from = Some("4.1.0".into());
        let err = plan_upgrade(&m, "4.0.0", &BTreeMap::new(), "x86_64")
            .unwrap_err()
            .to_string();
        assert!(err.contains("4.1.0"), "{err}");
        // 已经到了门槛版本就放行
        assert!(plan_upgrade(&m, "4.1.0", &BTreeMap::new(), "x86_64").is_ok());
        // 没有这个字段时一切照旧
        let m2 = manifest("4.2.0", "1.13.19", "00");
        assert!(plan_upgrade(&m2, "4.0.0", &BTreeMap::new(), "x86_64").is_ok());
    }

    #[test]
    fn nothing_to_do_when_versions_match() {
        let m = manifest("4.0.0", "1.13.19", "00");
        let installed = BTreeMap::from([("sing-box".to_string(), "1.13.19".to_string())]);
        let p = plan_upgrade(&m, "4.0.0", &installed, "x86_64").unwrap();
        assert_eq!(
            p,
            UpgradePlan {
                self_from: "4.0.0".into(),
                self_to: None,
                kernels: vec![]
            }
        );
    }

    #[test]
    fn plan_lists_self_and_kernel_upgrades() {
        let m = manifest("4.0.1", "1.14.2", "00");
        let installed = BTreeMap::from([("sing-box".to_string(), "1.13.19".to_string())]);
        let p = plan_upgrade(&m, "4.0.0", &installed, "x86_64").unwrap();
        assert_eq!(p.self_to.as_deref(), Some("4.0.1"));
        assert_eq!(
            p.kernels,
            vec![(
                "sing-box".to_string(),
                "1.13.19".to_string(),
                "1.14.2".to_string()
            )]
        );
    }

    #[test]
    fn apply_self_keeps_the_previous_binary() {
        let payload = b"NEWBUI".to_vec();
        let sum = crate::kernels::sha256_hex(&payload);
        let m = manifest("4.0.1", "1.13.19", &sum);
        let f = F(Mutex::new(vec![("https://x/bui".to_string(), payload)]));
        let h = FakeHost::new();
        h.write_file(std::path::Path::new("/opt/b-ui/bin/bui"), b"OLDBUI", 0o755)
            .unwrap();
        h.clear_ops();
        apply_self(
            &h,
            &f,
            m.bui_asset("x86_64").unwrap(),
            std::path::Path::new("/opt/b-ui/bin"),
        )
        .unwrap();
        assert_eq!(h.text("/opt/b-ui/bin/bui").as_deref(), Some("NEWBUI"));
        assert_eq!(h.text("/opt/b-ui/bin/bui.prev").as_deref(), Some("OLDBUI"));
        assert_eq!(h.mode("/opt/b-ui/bin/bui"), Some(0o755));
    }

    #[test]
    fn apply_self_refuses_a_bad_checksum() {
        let f = F(Mutex::new(vec![(
            "https://x/bui".to_string(),
            b"NEWBUI".to_vec(),
        )]));
        let m = manifest("4.0.1", "1.13.19", "deadbeef");
        let h = FakeHost::new();
        h.write_file(std::path::Path::new("/opt/b-ui/bin/bui"), b"OLDBUI", 0o755)
            .unwrap();
        assert!(apply_self(
            &h,
            &f,
            m.bui_asset("x86_64").unwrap(),
            std::path::Path::new("/opt/b-ui/bin")
        )
        .is_err());
        assert_eq!(
            h.text("/opt/b-ui/bin/bui").as_deref(),
            Some("OLDBUI"),
            "校验失败不替换"
        );
    }

    #[test]
    fn rollback_restores_binary_and_the_newest_state_backup() {
        let d = tempfile::tempdir().unwrap();
        let paths = bui_schema::paths::Paths {
            base_dir: d.path().into(),
            certs_dir: d.path().join("certs"),
            bin_dir: d.path().join("bin"),
        };
        std::fs::create_dir_all(crate::paths::backups_dir(&paths)).unwrap();
        std::fs::write(
            crate::paths::backups_dir(&paths).join("state-20260910T000000Z.json"),
            b"{\"old\":1}",
        )
        .unwrap();
        std::fs::write(
            crate::paths::backups_dir(&paths).join("state-20260911T000000Z.json"),
            b"{\"new\":1}",
        )
        .unwrap();
        std::fs::write(crate::paths::state_file(&paths), b"{\"current\":1}").unwrap();
        let h = FakeHost::new();
        h.write_file(&paths.bin_dir.join("bui.prev"), b"OLDBUI", 0o755)
            .unwrap();
        h.write_file(&paths.bin_dir.join("bui"), b"NEWBUI", 0o755)
            .unwrap();
        let done = rollback(&h, &paths).unwrap();
        assert_eq!(
            h.text(paths.bin_dir.join("bui").to_str().unwrap())
                .as_deref(),
            Some("OLDBUI")
        );
        assert_eq!(
            std::fs::read_to_string(crate::paths::state_file(&paths)).unwrap(),
            "{\"new\":1}",
            "恢复最近一份备份"
        );
        assert!(done
            .iter()
            .any(|l| l.contains("state-20260911T000000Z.json")));
        assert!(h.ops().contains(&"systemd:restart:b-ui".to_string()));
    }

    #[test]
    fn rollback_without_a_previous_binary_is_an_explicit_error() {
        let d = tempfile::tempdir().unwrap();
        let paths = bui_schema::paths::Paths {
            base_dir: d.path().into(),
            certs_dir: d.path().join("certs"),
            bin_dir: d.path().join("bin"),
        };
        let h = FakeHost::new();
        let err = rollback(&h, &paths).unwrap_err().to_string();
        assert!(err.contains("bui.prev"), "{err}");
    }

    #[test]
    fn jitter_is_reexported_from_serve() {
        // 实现与单元测试在 Task 15；这里只保证 CLI 侧的路径可用且行为一致
        let a = uuid::Uuid::parse_str("8d5a1a1e-3b2c-4d1e-9f00-000000000001").unwrap();
        assert_eq!(jitter_secs(a), crate::serve::jitter_secs(a));
        assert!(jitter_secs(a) < 3600);
    }
}
