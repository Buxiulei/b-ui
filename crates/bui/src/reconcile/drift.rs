//! 非受管项的漂移：**只报不改**（spec §2.2/§2.3）。派生物由 [`super::apply`] 直接覆盖，
//! 而「不是对账器产的东西」——受管单元下的陌生 drop-in、v3 遗留单元、受管目录里的陌生文件、
//! 多出来的 cron 行、`resolv.conf` 的 immutable 位不符——一律只列进体检，
//! `bui reconcile --force` 才调 [`clean`] 清理。这样 bwg-tizi 那类手工 drop-in 会被暴露出来，
//! 而不是被静默盖掉。
//!
//! [`scan`] 全程只读：唯一产生 `ops` 流水的调用是 `crontab -l`。

use super::{Artifact, LEGACY_UNITS, MANAGED_UNITS};
use crate::state::runtime::DriftItem;
use crate::sys::Host;
use bui_schema::paths::Paths;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// `<base>` 顶层允许存在、但不是 artifact 的条目（少一项就会每轮报永久漂移）
pub const BASE_WHITELIST: [&str; 14] = [
    "bin",
    "certs",
    "caddy",
    "packages",
    "state.backups",
    ".verify",
    "v3-backup",
    "state.json",
    "runtime.json",
    "manifest.json",
    // `bui upgrade` 留下的回滚快照。同一批的 `bin/bui.prev` 与 `bin/<kernel>.prev` 不用单独
    // 列：`stray_file` 只看顶层直接子项，`bin` 本身已在白名单里且不递归（见 `scan` 的说明）。
    // 同理，units 模块渲染的钩子入口链接 `bin/bui-auth-hook`（事故 2026-09-12）也不用列——
    // 它在 `bin/` 里，而这份白名单比的是 `file_name()`，写成 `bin/bui-auth-hook` 也比不到。
    "manifest.prev.json",
    "relay-cache.db",
    "auth-snapshot.json",
    "auth-hook.log",
];

/// 只在这四个 `/etc` 目录里找「b-ui 前缀的陌生配置」（`stray_conf`）；其余 `/etc` 一概不扫
pub const MANAGED_CONF_DIRS: [&str; 4] = [
    "/etc/sysctl.d",
    "/etc/modprobe.d",
    "/etc/modules-load.d",
    "/etc/ssh/sshd_config.d",
];

/// 受管配置的文件名前缀（`99-b-ui-network.conf` 一类）；别人的 `60-cloudimg.conf` 不看。
const CONF_PREFIXES: [&str; 3] = ["b-ui", "00-b-ui", "99-b-ui"];

/// 写盘期间可能瞬时存在的中间文件后缀，不算漂移。
const TRANSIENT_SUFFIXES: [&str; 2] = [".tmp", ".new"];

const SYSTEMD_DIR: &str = "/etc/systemd/system";

/// 检查顺序固定：`unit_dropin` → `legacy_unit` → `stray_file` → `stray_conf` → `cron`
/// → `resolv_immutable`（测试按这个顺序断言）。
///
/// 扫描范围是**有意收窄**的：`stray_file` 只看 `<base>` 顶层的直接子项，不递归进
/// `bin/` `certs/` `caddy/` `packages/`（那些目录的内容由 Caddy、客户端缓存与内核自己生成）；
/// `/etc` 只多扫 [`MANAGED_CONF_DIRS`] 四个目录里 b-ui 前缀的文件。`/tmp` 一概不扫：
/// v3 的 `/tmp/hy2-watchdog-*` 只在 `import-v3` 里删，`/tmp` 是 tmpfs 重启即清，
/// 纳入扫描只会造出一条重启前无法修复的永久 degraded。
pub fn scan(host: &dyn Host, artifacts: &[Artifact], paths: &Paths) -> Vec<DriftItem> {
    let mut out = Vec::new();
    let managed_paths = artifact_paths(artifacts);

    // 1) 受管单元下的陌生 drop-in。**按「所有可能的槽」枚举而不是按期望态**：
    // 某个槽此刻不存在，它留下的 drop-in 照样是我们该报出来的东西。
    let units: Vec<String> = MANAGED_UNITS
        .iter()
        .map(|u| u.to_string())
        .chain((1..super::MAX_RESI_SLOTS).map(super::resi_unit))
        .collect();
    for unit in &units {
        let dir = PathBuf::from(SYSTEMD_DIR).join(format!("{unit}.service.d"));
        for entry in host.list_dir(&dir).unwrap_or_default() {
            if managed_paths.contains(&entry) {
                continue;
            }
            out.push(DriftItem {
                kind: "unit_dropin".into(),
                path: entry.display().to_string(),
                detail: format!("受管单元 {unit} 下的非受管 drop-in"),
            });
        }
    }

    // 2) v3 遗留单元（正常路径上 units 模块已产出 `Absent` 删掉它们；这里兜手工放回来的）
    for unit in LEGACY_UNITS {
        let path = PathBuf::from(SYSTEMD_DIR).join(unit);
        if host.read_file(&path).unwrap_or_default().is_some() {
            out.push(DriftItem {
                kind: "legacy_unit".into(),
                path: path.display().to_string(),
                detail: "v3 遗留单元仍在".into(),
            });
        }
    }

    // 3) `<base>` 顶层的陌生文件/目录
    for entry in host.list_dir(&paths.base_dir).unwrap_or_default() {
        let Some(name) = entry.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if managed_paths.contains(&entry)
            || BASE_WHITELIST.contains(&name)
            || TRANSIENT_SUFFIXES.iter().any(|s| name.ends_with(s))
        {
            continue;
        }
        let detail = if host.is_dir(&entry).unwrap_or(false) {
            "受管目录里的陌生目录"
        } else {
            "受管目录里的陌生文件"
        };
        out.push(DriftItem {
            kind: "stray_file".into(),
            path: entry.display().to_string(),
            detail: detail.into(),
        });
    }

    // 4) 四个 /etc 目录里 b-ui 前缀的陌生配置（典型是手工复制的 `99-b-ui-network.conf.bak`）
    for dir in MANAGED_CONF_DIRS {
        for entry in host.list_dir(Path::new(dir)).unwrap_or_default() {
            let Some(name) = entry.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if managed_paths.contains(&entry) || !CONF_PREFIXES.iter().any(|p| name.starts_with(p))
            {
                continue;
            }
            out.push(DriftItem {
                kind: "stray_conf".into(),
                path: entry.display().to_string(),
                detail: "受管目录里的陌生 b-ui 配置".into(),
            });
        }
    }

    // 5) cron 里指向 `<base>` 的行（v3 的 update.sh 定时任务）
    if let Ok(o) = host.run("crontab", &["-l"]) {
        if o.ok() {
            let needle = paths.base_dir.display().to_string();
            for line in o.stdout.lines().filter(|l| l.contains(&needle)) {
                out.push(DriftItem {
                    kind: "cron".into(),
                    path: "crontab".into(),
                    detail: line.trim().to_string(),
                });
            }
        }
    }

    // 6) `/etc/resolv.conf` 的 immutable 位与期望不符。
    // **只在 chattr 与 lsattr 都存在时才报**：容器/精简镜像上没有这两个命令，而
    // `RealHost::set_immutable` 失败只 warn 返回 Ok，报了就是一条永远修不掉的漂移
    // （drift 非空 → `/api/health` degraded → M1 的「体检无漂移」不可达）。
    // 有 chattr 但文件系统不支持的机器（overlayfs、部分 VPS 模板）照报，
    // 逃生口是把 `state.system.static_dns` 置 false。
    if host.which("chattr") && host.which("lsattr") {
        for art in artifacts {
            let Artifact::File {
                path, immutable, ..
            } = art
            else {
                continue;
            };
            if path != Path::new("/etc/resolv.conf") {
                continue;
            }
            if host.is_immutable(path).unwrap_or(false) != *immutable {
                out.push(DriftItem {
                    kind: "resolv_immutable".into(),
                    path: path.display().to_string(),
                    detail: format!("immutable 位与期望不符（期望 {immutable}）"),
                });
            }
        }
    }

    out
}

/// `bui reconcile --force` 的清理：返回真正处置掉的路径。`cron` 与 `resolv_immutable` 不动
/// （删别人的 cron 行太危险；immutable 位由下一轮 diff 的 `SetImmutable` 修）。
pub fn clean(host: &dyn Host, items: &[DriftItem]) -> Vec<String> {
    let mut done = Vec::new();
    let mut need_daemon_reload = false;
    for item in items {
        let path = PathBuf::from(&item.path);
        let ok = match item.kind.as_str() {
            "unit_dropin" | "stray_conf" => host.remove_file(&path).is_ok(),
            "stray_file" => {
                if item.detail.contains("目录") || host.is_dir(&path).unwrap_or(false) {
                    host.remove_dir_all(&path).is_ok()
                } else {
                    host.remove_file(&path).is_ok()
                }
            }
            "legacy_unit" => {
                if let Some(unit) = path.file_name().and_then(|s| s.to_str()) {
                    let _ = host.systemd("disable", unit);
                    let _ = host.systemd("stop", unit);
                }
                host.remove_file(&path).is_ok()
            }
            _ => continue,
        };
        if ok {
            done.push(item.path.clone());
            if path.starts_with(SYSTEMD_DIR) {
                need_daemon_reload = true;
            }
        }
    }
    if need_daemon_reload {
        let _ = host.systemd_daemon_reload();
    }
    done
}

/// 本轮 artifact 占用的路径集合（`Absent` 不算：它期望的就是「不存在」）。
fn artifact_paths(artifacts: &[Artifact]) -> BTreeSet<PathBuf> {
    let mut out = BTreeSet::new();
    for art in artifacts {
        match art {
            Artifact::File { path, .. } | Artifact::Symlink { path, .. } => {
                out.insert(path.clone());
            }
            Artifact::Unit { name, dropin, .. } => {
                out.insert(Artifact::unit_path(&name.name, dropin.as_deref()));
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reconcile::Artifact;
    // `Host` 由上面的 `use super::*` 带进来（本文件顶部已 `use crate::sys::Host`），
    // 再显式导入一次会被 `-D warnings` 判成 unused_imports。
    use crate::sys::{fake::FakeHost, CmdOut};
    use bui_schema::paths::Paths;
    use pretty_assertions::assert_eq;

    fn managed() -> Vec<Artifact> {
        vec![
            Artifact::file("/opt/b-ui/config.yaml", "a"),
            // Caddyfile 按 C3 就在 <base> 顶层：它是 artifact，所以不该被报成陌生文件
            Artifact::file("/opt/b-ui/Caddyfile", "example.com {}\n").mode(0o644),
            Artifact::file("/etc/resolv.conf", "b")
                .mode(0o644)
                .immutable(),
            Artifact::Unit {
                name: crate::reconcile::Unit::restart("xray"),
                dropin: None,
                content: "x".into(),
            },
        ]
    }

    #[test]
    fn reports_foreign_dropins_legacy_units_stray_files_and_cron() {
        let h = FakeHost::new();
        h.with(|i| {
            i.files
                .insert("/opt/b-ui/config.yaml".into(), (b"a".to_vec(), 0o600));
            i.files
                .insert("/etc/resolv.conf".into(), (b"b".to_vec(), 0o644));
            i.files.insert(
                "/etc/systemd/system/xray.service".into(),
                (b"x".to_vec(), 0o644),
            );
            i.files.insert(
                "/etc/systemd/system/xray.service.d/50-manual.conf".into(),
                (b"y".to_vec(), 0o644),
            );
            i.files.insert(
                "/etc/systemd/system/hy2-watchdog.timer".into(),
                (b"z".to_vec(), 0o644),
            );
            i.files
                .insert("/opt/b-ui/stray-note.txt".into(), (b"?".to_vec(), 0o644));
            // chattr/lsattr 都在 → 才有资格报 resolv_immutable（见 Step 5 的降级口径）
            i.which.insert("chattr".into());
            i.which.insert("lsattr".into());
            i.scripted.push((
                "crontab -l".into(),
                CmdOut::success("0 */6 * * * /opt/b-ui/update.sh auto\n"),
            ));
        });
        let items = scan(&h, &managed(), &Paths::default_server());
        let kinds: Vec<&str> = items.iter().map(|i| i.kind.as_str()).collect();
        assert_eq!(
            kinds,
            vec![
                "unit_dropin",
                "legacy_unit",
                "stray_file",
                "cron",
                "resolv_immutable"
            ]
        );
        assert_eq!(
            items[0].path,
            "/etc/systemd/system/xray.service.d/50-manual.conf"
        );
        assert_eq!(items[1].path, "/etc/systemd/system/hy2-watchdog.timer");
        assert_eq!(items[2].path, "/opt/b-ui/stray-note.txt");
        assert!(items[3].detail.contains("update.sh"));
        assert!(items[4].detail.contains("immutable"));
        assert!(
            h.ops().iter().all(|o| o.starts_with("run:")),
            "scan 只读，不改任何东西"
        );
    }

    #[test]
    fn a_clean_host_has_no_drift() {
        let h = FakeHost::new();
        h.with(|i| {
            i.files
                .insert("/opt/b-ui/config.yaml".into(), (b"a".to_vec(), 0o600));
            i.files
                .insert("/etc/resolv.conf".into(), (b"b".to_vec(), 0o644));
            i.immutable.insert("/etc/resolv.conf".into());
            i.files.insert(
                "/etc/systemd/system/xray.service".into(),
                (b"x".to_vec(), 0o644),
            );
            i.scripted.push((
                "crontab -l".into(),
                CmdOut::failure(1, "no crontab for root"),
            ));
        });
        assert_eq!(scan(&h, &managed(), &Paths::default_server()), vec![]);
    }

    #[test]
    fn every_file_a_healthy_v4_install_has_is_whitelisted() {
        // 少一项白名单就会每 10 分钟报一条永久漂移、M1 的「体检无漂移」不可达
        let h = FakeHost::new();
        h.with(|i| {
            i.files
                .insert("/opt/b-ui/config.yaml".into(), (b"a".to_vec(), 0o600));
            i.files.insert(
                "/opt/b-ui/Caddyfile".into(),
                (b"example.com {}\n".to_vec(), 0o644),
            );
            i.files
                .insert("/etc/resolv.conf".into(), (b"b".to_vec(), 0o644));
            i.immutable.insert("/etc/resolv.conf".into());
            i.files.insert(
                "/etc/systemd/system/xray.service".into(),
                (b"x".to_vec(), 0o644),
            );
            i.files.insert(
                "/opt/b-ui/auth-snapshot.json".into(),
                (br#"{"schema":1,"users":{}}"#.to_vec(), 0o600),
            );
            // P2 的钩子自己写的日志（spec §3.2）：白名单里没有它就会每 10 分钟报一条永久漂移
            i.files
                .insert("/opt/b-ui/auth-hook.log".into(), (b"".to_vec(), 0o600));
            // 对账器与运行时自己造的东西
            i.files
                .insert("/opt/b-ui/state.json".into(), (b"{}".to_vec(), 0o600));
            i.files
                .insert("/opt/b-ui/runtime.json".into(), (b"{}".to_vec(), 0o600));
            i.files
                .insert("/opt/b-ui/manifest.json".into(), (b"{}".to_vec(), 0o644));
            // `bui upgrade` 留下的回滚快照（`--rollback` 靠它回退内核与内核版本表）
            i.files.insert(
                "/opt/b-ui/manifest.prev.json".into(),
                (b"{}".to_vec(), 0o644),
            );
            i.files.insert(
                "/opt/b-ui/relay-cache.db".into(),
                (b"sqlite".to_vec(), 0o644),
            );
            i.files.insert(
                "/opt/b-ui/state.backups/state-20260911T000000Z.json".into(),
                (b"{}".to_vec(), 0o600),
            );
            i.files
                .insert("/opt/b-ui/bin/xray".into(), (b"ELF".to_vec(), 0o755));
            i.files
                .insert("/opt/b-ui/bin/bui.prev".into(), (b"ELF".to_vec(), 0o755));
            for k in crate::kernels::KERNELS {
                i.files.insert(
                    format!("/opt/b-ui/bin/{k}.prev").into(),
                    (b"ELF".to_vec(), 0o755),
                );
            }
            i.files.insert(
                "/opt/b-ui/certs/fullchain.pem".into(),
                (b"CERT".to_vec(), 0o644),
            );
            i.files.insert(
                "/opt/b-ui/caddy/caddy/certificates/x/y.crt".into(),
                (b"CERT".to_vec(), 0o600),
            );
            i.files.insert(
                "/opt/b-ui/packages/versions.json".into(),
                (b"{}".to_vec(), 0o644),
            );
            i.files.insert(
                "/opt/b-ui/.verify/xray-config.json".into(),
                (b"{}".to_vec(), 0o600),
            );
            // import-v3 归档的 v3 秘密文件
            i.files.insert(
                "/opt/b-ui/v3-backup/users.json".into(),
                (b"[]".to_vec(), 0o600),
            );
            i.scripted.push((
                "crontab -l".into(),
                CmdOut::failure(1, "no crontab for root"),
            ));
        });
        assert_eq!(
            scan(&h, &managed(), &Paths::default_server()),
            vec![],
            "健康装机必须零漂移"
        );
    }

    /// 事故回归（2026-09-12）配套：`<base>/bin/bui-auth-hook` 这条钩子入口链接永远不许被
    /// 报成漂移——否则 `/api/health` 每 10 分钟 degraded，`bui reconcile --force` 还会把它
    /// 删掉，Hysteria2 的 `auth.command` 当场指向一个不存在的文件（= 事故本身）。
    ///
    /// 它**不需要**往 [`BASE_WHITELIST`] 加条目：`stray_file` 只看 `<base>` 顶层直接子项，
    /// 而 `bin` 已在白名单里且不递归；白名单比的也是 `file_name()`，加一条 `bin/bui-auth-hook`
    /// 根本比不到。这条测试把这个推理钉死，免得后人「补白名单」时把 scan 改成递归。
    #[test]
    fn the_auth_hook_symlink_is_never_reported_as_drift() {
        let paths = Paths::default_server();
        let hook = paths.auth_hook_bin();
        assert_eq!(
            hook.parent(),
            Some(paths.bin_dir.as_path()),
            "钩子入口必须在 bin/ 里，顶层扫不到"
        );
        assert!(BASE_WHITELIST.contains(&"bin"));

        let h = FakeHost::new();
        h.with(|i| {
            i.files
                .insert("/opt/b-ui/bin/bui".into(), (b"ELF".to_vec(), 0o755));
            i.scripted.push((
                "crontab -l".into(),
                CmdOut::failure(1, "no crontab for root"),
            ));
        });
        h.symlink(Path::new("bui"), &hook).unwrap();
        let mut arts = managed();
        arts.push(Artifact::Symlink {
            path: hook.clone(),
            target: "bui".into(),
        });
        assert_eq!(scan(&h, &arts, &paths), vec![], "bin/ 下的钩子入口不算漂移");
        assert!(
            artifact_paths(&arts).contains(&hook),
            "它是受管 artifact：将来就算 scan 递归进 bin/，也在受管路径集合里"
        );
    }

    /// 2026-09-13 裁决「P1：Caddy 外部站点通道」：`<base>/caddy/sites/` 里的文件归用户管，
    /// 既不是 artifact 也永远不算漂移（报了就是每 10 分钟一条永久 degraded）。
    #[test]
    fn user_site_files_under_caddy_sites_are_never_drift() {
        let h = FakeHost::new();
        h.with(|i| {
            i.files
                .insert("/opt/b-ui/config.yaml".into(), (b"a".to_vec(), 0o600));
            i.files.insert(
                "/opt/b-ui/Caddyfile".into(),
                (b"example.com {}\n".to_vec(), 0o644),
            );
            i.files
                .insert("/etc/resolv.conf".into(), (b"b".to_vec(), 0o644));
            i.immutable.insert("/etc/resolv.conf".into());
            i.files.insert(
                "/etc/systemd/system/xray.service".into(),
                (b"x".to_vec(), 0o644),
            );
            // 用户自己丢进来的站点块 + import-v3 导出的那份 + 对账器的占位 README
            i.files.insert(
                "/opt/b-ui/caddy/sites/my-blog.caddy".into(),
                (
                    b"blog.example.com {\n\troot * /srv/blog\n}\n".to_vec(),
                    0o644,
                ),
            );
            i.files.insert(
                "/opt/b-ui/caddy/sites/imported-from-v3.caddy".into(),
                (b"shop.example.com {\n}\n".to_vec(), 0o644),
            );
            i.files.insert(
                "/opt/b-ui/caddy/sites/README.txt".into(),
                (b"put your *.caddy here".to_vec(), 0o644),
            );
            i.scripted.push((
                "crontab -l".into(),
                CmdOut::failure(1, "no crontab for root"),
            ));
        });
        assert!(
            BASE_WHITELIST.contains(&"caddy"),
            "caddy/ 在白名单里，sites/ 才连带不被扫"
        );
        assert_eq!(
            scan(&h, &managed(), &Paths::default_server()),
            vec![],
            "用户的站点文件不许被报成漂移"
        );
    }

    #[test]
    fn a_b_ui_prefixed_backup_under_etc_is_reported_as_stray_conf() {
        // spec §2.2「受管目录里的陌生文件」：四个 /etc 目录只看 b-ui 前缀的文件名
        let h = FakeHost::new();
        h.with(|i| {
            i.files
                .insert("/opt/b-ui/config.yaml".into(), (b"a".to_vec(), 0o600));
            i.files
                .insert("/etc/resolv.conf".into(), (b"b".to_vec(), 0o644));
            i.immutable.insert("/etc/resolv.conf".into());
            i.files.insert(
                "/etc/systemd/system/xray.service".into(),
                (b"x".to_vec(), 0o644),
            );
            // 手工复制出来的备份：不是 artifact，但前缀属于我们
            i.files.insert(
                "/etc/sysctl.d/99-b-ui-network.conf.bak".into(),
                (b"x".to_vec(), 0o644),
            );
            // 别人的文件：一律不管
            i.files.insert(
                "/etc/sysctl.d/60-cloudimg.conf".into(),
                (b"x".to_vec(), 0o644),
            );
            i.which.insert("chattr".into());
            i.which.insert("lsattr".into());
            i.scripted.push((
                "crontab -l".into(),
                CmdOut::failure(1, "no crontab for root"),
            ));
        });
        let items = scan(&h, &managed(), &Paths::default_server());
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0].kind, "stray_conf");
        assert_eq!(items[0].path, "/etc/sysctl.d/99-b-ui-network.conf.bak");
    }

    #[test]
    fn a_stray_directory_is_reported_and_cleaned_recursively() {
        let h = FakeHost::new();
        h.with(|i| {
            i.files
                .insert("/opt/b-ui/config.yaml".into(), (b"a".to_vec(), 0o600));
            i.files
                .insert("/etc/resolv.conf".into(), (b"b".to_vec(), 0o644));
            i.immutable.insert("/etc/resolv.conf".into());
            i.files.insert(
                "/etc/systemd/system/xray.service".into(),
                (b"x".to_vec(), 0o644),
            );
            i.files.insert(
                "/opt/b-ui/admin/node_modules/x/index.js".into(),
                (b"x".to_vec(), 0o644),
            );
            i.scripted.push((
                "crontab -l".into(),
                CmdOut::failure(1, "no crontab for root"),
            ));
        });
        let items = scan(&h, &managed(), &Paths::default_server());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, "stray_file");
        assert_eq!(items[0].path, "/opt/b-ui/admin");
        assert!(items[0].detail.contains("目录"));
        h.clear_ops();
        clean(&h, &items);
        assert_eq!(h.ops(), vec!["rmdir:/opt/b-ui/admin"]);
        assert!(h.text("/opt/b-ui/admin/node_modules/x/index.js").is_none());
    }

    #[test]
    fn clean_removes_everything_but_cron() {
        let h = FakeHost::new();
        let items = vec![
            DriftItem {
                kind: "unit_dropin".into(),
                path: "/etc/systemd/system/xray.service.d/50-manual.conf".into(),
                detail: String::new(),
            },
            DriftItem {
                kind: "legacy_unit".into(),
                path: "/etc/systemd/system/hy2-watchdog.timer".into(),
                detail: String::new(),
            },
            DriftItem {
                kind: "stray_file".into(),
                path: "/opt/b-ui/stray-note.txt".into(),
                detail: String::new(),
            },
            DriftItem {
                kind: "cron".into(),
                path: "crontab".into(),
                detail: "0 */6 * * * /opt/b-ui/update.sh auto".into(),
            },
        ];
        let done = clean(&h, &items);
        assert_eq!(done.len(), 3, "cron 行只报不删（删别人的 cron 太危险）");
        assert_eq!(
            h.ops(),
            vec![
                "remove:/etc/systemd/system/xray.service.d/50-manual.conf",
                "systemd:disable:hy2-watchdog.timer",
                "systemd:stop:hy2-watchdog.timer",
                "remove:/etc/systemd/system/hy2-watchdog.timer",
                "remove:/opt/b-ui/stray-note.txt",
                "daemon-reload",
            ]
        );
    }
}
