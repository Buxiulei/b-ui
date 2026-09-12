//! `bui import-v3`（只生成 `state.json`）与 `bui install --import-v3` 用的 v3 卸载流程。
//!
//! 卸载的顺序是硬要求（2026-09-12 裁决）：先把发行版 Caddy 的 ACME 账号与证书搬进 v4 的数据
//! 目录，再停它；整个 `uninstall_v3` 又必须排在**对账之前**（v3 的 `b-ui-admin` 还占着 `:8080`
//! 时，对账 start 的 `b-ui.service` 首启 bind 失败会进 `Restart=always` 循环）。

use crate::sys::Host;
use bui_schema::paths::Paths;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 要停用并删除的 v3 单元。**不含 `b-ui-relay.service`**：v4 原地重写同名单元，
/// 由内容 diff 决定是否重启；把它列进来会在装好 v4 之后又把 relay 卸掉，住宅两个节点一起断。
pub const V3_UNITS: [&str; 7] = [
    "b-ui-admin.service",
    "b-ui-cert-sync.timer",
    "b-ui-cert-sync.service",
    "hy2-watchdog.timer",
    "hy2-watchdog.service",
    "b-ui-resi-health.timer",
    "b-ui-resi-health.service",
];

/// v3 的 shell / Node / 引导文件与它自带的 sing-box 二进制：直接删。
/// 每一项都是从仓库里的 v3 脚本核实过的，不是凭记忆列的：
/// `sing-box` —— v3 的 relay 二进制在 **`${BASE_DIR}/sing-box`**（顶层，不在 `bin/`；
///   `server/residential-helper.sh:35`），v4 的那份在 `<base>/bin/sing-box`，所以删顶层这个不影响 v4；
/// `cert-check.sh` —— `server/core.sh:1170-1217` 写出并 `chmod +x`，`update.sh:2044-2066` 还给它挂了
///   一条 12 小时 cron（cron 行由 [`filter_cron`] 一并清掉）。
/// 少一项就会在 `<base>` 顶层留一个永久 `stray_file`：每 10 分钟一条漂移 → `/api/health` 恒
/// `degraded` → M1 的「体检无漂移」不可达（spec §2.3 + §9）。
pub const V3_FILES: [&str; 13] = [
    "core.sh",
    "update.sh",
    "b-ui-cli.sh",
    "residential-helper.sh",
    "resi-health.sh",
    "hy2-watchdog.sh",
    "cert-sync.sh",
    "cert-check.sh",
    "hy2-portjump-cleanup.sh",
    "install-key.txt",
    "b-ui-client.sh",
    "version.json",
    "sing-box",
];

/// v3 的状态文件：移进 `<base>/v3-backup/`（0700）而不是删——出问题要能回查，
/// 留在原地则被 §2.2 的漂移扫描永久报告。同样逐条核实过：
/// `port-hopping.json`（`core.sh:1422` 写，`update.sh:801-808/1315-1321`、`web/server.js:434` 读）、
/// `masquerade.json`（`core.sh:773` 写，`web/server.js:2567` 读）、
/// `server_ip.txt`（`web/server.js:73` 读，P0 的 v3 fixture 就带一个）。
/// 前两个不含秘密但含节点配置，第三个是公网 IP：一起归档，既不丢线索也不留漂移。
pub const V3_STATE_FILES: [&str; 9] = [
    "users.json",
    "reality-keys.json",
    "residential-proxy.json",
    "admin.env",
    ".resi-health-state.json",
    ".relay.lock",
    "port-hopping.json",
    "masquerade.json",
    "server_ip.txt",
];

/// v3 的迁移块与 CLI 留在 `<base>` 顶层的**备份 / 临时文件**的基名。只认这六个基名，
/// 别人的文件一概不碰。核实来源：
/// `server/update.sh:682/703/734`（`*.bak.v360.<ts>`，三个 config）、`:774`（`*.bak.<ts>`）、
/// `:858`（`*.bak.broken-<ts>`）、`:1660`（`config.yaml.bak.v357.<ts>`）、
/// `:1674`（`xray-config.json.bak.<ts>`）、`:1709`（`xray-config.json.bak.v359.<ts>`）、
/// `server/b-ui-cli.sh:939/963`（`config.yaml.bak.obfs.<ts>`）；
/// `.tmp` 来自 `core.sh`/`update.sh` 的 `config.yaml.tmp` / `config.yaml.v357.tmp` /
/// `xray-config.json.tmp` 与 `residential-helper.sh:455/500/565` 的原子写中断残留。
/// `*.bak.*` 里有 HY2 明文密码与 UUID → **归档**进 `v3-backup/`；`.tmp` 是半成品 → **删除**。
pub const V3_LEFTOVER_PREFIXES: [&str; 6] = [
    "config.yaml",
    "config-residential.yaml",
    "xray-config.json",
    "singbox-relay.json",
    "residential-proxy.json",
    ".resi-health-state.json",
];

/// 发行版 Caddy（v3 用的那个）的数据目录：ACME 账号与已签证书都在它下面。
pub const V3_CADDY_DATA: &str = "/var/lib/caddy/.local/share/caddy";

/// 只删 b-ui 自己写的 cron 行。
pub fn filter_cron(text: &str) -> String {
    text.lines()
        .filter(|l| !l.contains("/opt/b-ui/"))
        .map(|l| format!("{l}\n"))
        .collect()
}

/// 递归复制目录：只用 [`Host`] 的原语，所以测试里注入 `FakeHost` 就能全程不碰真实系统。
/// 目标已存在同名文件 → 跳过（幂等，且绝不用旧数据盖掉新的）。
fn copy_tree(host: &dyn Host, src: &Path, dest: &Path, done: &mut Vec<String>) {
    let Ok(entries) = host.list_dir(src) else {
        return;
    };
    for e in entries {
        let Some(name) = e.file_name() else { continue };
        let target = dest.join(name);
        if host.is_dir(&e).unwrap_or(false) {
            copy_tree(host, &e, &target, done);
        } else if host.read_file(&target).ok().flatten().is_some() {
            continue;
        } else if let Ok(Some(bytes)) = host.read_file(&e) {
            // 里面有 ACME 账号私钥与证书私钥，一律 0600（v4 的 caddy 以 root 运行）
            if host.write_file(&target, &bytes, 0o600).is_ok() {
                done.push(format!("已复制 {} → {}", e.display(), target.display()));
            }
        }
    }
}

/// 把 [`V3_CADDY_DATA`] 整棵复制到 `<base>/caddy/caddy/`（目标已有同名文件则跳过，不覆盖），
/// **复制完才** `systemctl stop caddy`；[`uninstall_v3`] 的第一步。
pub fn migrate_caddy_data(host: &dyn Host, paths: &Paths) -> Vec<String> {
    let src = Path::new(V3_CADDY_DATA);
    if !host.is_dir(src).unwrap_or(false) {
        return Vec::new(); // 没装过发行版 caddy（全新机器）：什么都不做
    }
    let dest = crate::paths::caddy_data(paths);
    let mut done = Vec::new();
    copy_tree(host, src, &dest, &mut done);
    // 复制完才停发行版 caddy：它还活着就会继续往旧目录写，停早了 443 上会出现无证书窗口。
    // 对账（install 第 9 步）随后写 v4 的 caddy.service 并把它拉起来，届时用的是新的 XDG 目录。
    let _ = host.systemd("stop", "caddy");
    done.push(format!(
        "已迁移 v3 Caddy 数据目录（ACME 账号与证书）到 {} 并停用发行版 caddy",
        dest.display()
    ));
    done
}

/// 扫 `<base>` **顶层**（`list_dir` 只给直接子项），把 [`V3_LEFTOVER_PREFIXES`] 里某个基名后面
/// 跟着 `.bak.…` 的文件归档进 `v3-backup/`（0600）、跟着 `.tmp` 结尾的删掉；文件名恰好等于基名
/// 本身的（就是 v4 自己在管的那四个配置）绝不碰。
///
/// 漂移扫描只跳过 `.tmp` / `.new` 后缀，**不跳过 `*.bak.*`**，所以不清掉这些备份，
/// 导入后的机器每 10 分钟就会报一串 `stray_file`（spec §2.3 + §9）。
pub fn sweep_v3_leftovers(host: &dyn Host, paths: &Paths) -> Vec<String> {
    let mut done = Vec::new();
    let Ok(entries) = host.list_dir(&paths.base_dir) else {
        return done;
    };
    for e in entries {
        let Some(name) = e.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        // 只认「某个受管基名 + 后缀」，且不能等于基名本身（那是 v4 正在管的配置）
        let Some(rest) = V3_LEFTOVER_PREFIXES
            .iter()
            .find_map(|pre| name.strip_prefix(pre).filter(|r| !r.is_empty()))
        else {
            continue;
        };
        if host.is_dir(&e).unwrap_or(false) {
            continue; // 只处理文件
        }
        if rest.starts_with(".bak.") {
            // 里面有 HY2 明文密码与 UUID：归档而不是删，0600
            if let Ok(Some(bytes)) = host.read_file(&e) {
                let dest = crate::paths::v3_backup_dir(paths).join(name);
                if host.write_file(&dest, &bytes, 0o600).is_ok() {
                    let _ = host.remove_file(&e);
                    done.push(format!(
                        "已归档 v3 备份 {} → {}",
                        e.display(),
                        dest.display()
                    ));
                }
            }
        } else if rest.ends_with(".tmp") {
            let _ = host.remove_file(&e);
            done.push(format!("已删除 v3 临时文件 {}", e.display()));
        }
    }
    done
}

/// v3 早期版本用 iptables/nft 的 REDIRECT 链做端口跳跃（`hy2-portjump-cleanup.sh` 负责清理，
/// v4 把这个脚本删了，spec §3.1 也要求 v4「不再有任何 iptables/nft 规则」由 b-ui 自己写）。
/// 只删两类**孤儿**：`iptables`/`ip6tables` 的 `nat` 表里 `HYSTERIA-PR-*` 链（移植
/// `server/core.sh:159-178`：跳转规则先 `-D`，再 `-F` + `-X`），以及 `nft` 里
/// `hysteria_*` 表（`server/core.sh:180-186`）。命令不存在 / 删不掉 → 只记一行说明，不算错误。
///
/// **注意**：hysteria 2.12 自己会为内置端口跳跃创建同名链并在 shutdown 时清理，所以
/// ① 这一步必须排在 [`uninstall_v3`] 的**最后**（紧接着的对账会重写两个 hysteria 单元并重启，
/// 启动时 hysteria 自建所需的链）；② 漂移扫描**不**看这些链——v4 运行中的 hysteria 正当持有它们。
pub fn flush_v3_portjump_rules(host: &dyn Host) -> Vec<String> {
    let mut done = Vec::new();
    for ipt in ["iptables", "ip6tables"] {
        if !host.which(ipt) {
            continue;
        }
        let Ok(out) = host.run(ipt, &["-t", "nat", "-S"]) else {
            continue;
        };
        if !out.ok() {
            continue;
        }
        let chains: std::collections::BTreeSet<String> = out
            .stdout
            .lines()
            .filter_map(|l| l.split_whitespace().find(|w| w.starts_with("HYSTERIA-PR-")))
            .map(str::to_string)
            .collect();
        for ch in chains {
            // 先删所有跳转到该链的规则（`-A … -j <ch>` → `-D … -j <ch>`），再清空并删链
            for line in out
                .stdout
                .lines()
                .filter(|l| l.starts_with("-A ") && l.ends_with(&format!("-j {ch}")))
            {
                let rule = line.replacen("-A ", "-D ", 1);
                let mut args = vec!["-t", "nat"];
                args.extend(rule.split_whitespace());
                let _ = host.run(ipt, &args);
            }
            let _ = host.run(ipt, &["-t", "nat", "-F", &ch]);
            let _ = host.run(ipt, &["-t", "nat", "-X", &ch]);
            done.push(format!("已清理 {ipt} nat 链 {ch}（v3 端口跳跃遗留）"));
        }
    }
    if host.which("nft") {
        if let Ok(out) = host.run("nft", &["list", "tables"]) {
            for (family, table) in out.stdout.lines().filter_map(|l| {
                let mut w = l.split_whitespace();
                let (_, f, t) = (w.next()?, w.next()?, w.next()?);
                t.starts_with("hysteria_")
                    .then(|| (f.to_string(), t.to_string()))
            }) {
                let _ = host.run("nft", &["delete", "table", &family, &table]);
                done.push(format!("已删除 nft 表 {family} {table}（v3 端口跳跃遗留）"));
            }
        }
    }
    done
}

/// 顺序：[`migrate_caddy_data`] → 停 v3 单元 → 删 v3 shell/Node 文件 → 归档 v3 状态文件 →
/// [`sweep_v3_leftovers`]（`*.bak.*` 归档、`*.tmp` 删） → 删 v3 `admin/` → 删
/// `/tmp/hy2-watchdog-*` → 清 cron 行 → [`flush_v3_portjump_rules`]；保留 `certs/` 与 `packages/`。
pub fn uninstall_v3(host: &dyn Host, paths: &Paths) -> Vec<String> {
    // 第一步（2026-09-12 裁决）：先把发行版 Caddy 的 ACME 账号与证书搬进 v4 的数据目录，
    // 再停它；顺序颠倒就会重新签发。必须排在所有删除动作之前。
    let mut done = migrate_caddy_data(host, paths);
    let mut touched_units = false;
    for u in V3_UNITS {
        let unit_file = PathBuf::from("/etc/systemd/system").join(u);
        if host.read_file(&unit_file).ok().flatten().is_none()
            && !host.unit_exists(u).unwrap_or(false)
        {
            continue;
        }
        let _ = host.systemd("disable", u);
        let _ = host.systemd("stop", u);
        let _ = host.remove_file(&unit_file);
        touched_units = true;
        done.push(format!("已移除 v3 单元 {u}"));
    }
    for f in V3_FILES {
        let p = paths.base_dir.join(f);
        if host.read_file(&p).ok().flatten().is_some() {
            let _ = host.remove_file(&p);
            done.push(format!("已删除 {}", p.display()));
        }
    }
    // v3 的状态文件（含秘密）归档到 v3-backup/，0600
    for f in V3_STATE_FILES {
        let src = paths.base_dir.join(f);
        if let Ok(Some(bytes)) = host.read_file(&src) {
            let dest = crate::paths::v3_backup_dir(paths).join(f);
            if host.write_file(&dest, &bytes, 0o600).is_ok() {
                let _ = host.remove_file(&src);
                done.push(format!("已归档 {} → {}", src.display(), dest.display()));
            }
        }
    }
    // v3 迁移块与 CLI 留下的备份 / 临时文件（`*.bak.v357.<ts>` 一类）。漂移扫描只跳过
    // `.tmp` / `.new`，不跳过 `*.bak.*`，不清就是一串永久 stray_file
    done.extend(sweep_v3_leftovers(host, paths));
    // v3 的 Node 面板整棵删（server.js + node_modules + 目录本身）
    let admin = paths.base_dir.join("admin");
    if host.is_dir(&admin).unwrap_or(false) {
        let _ = host.remove_dir_all(&admin);
        done.push(format!("已删除 v3 Node 面板目录 {}", admin.display()));
    }
    // spec §3.4：/tmp/hy2-watchdog-* 计数文件（v3 面板还在读）
    if let Ok(entries) = host.list_dir(Path::new("/tmp")) {
        for e in entries {
            let name = e.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name.starts_with("hy2-watchdog-") {
                let _ = host.remove_file(&e);
                done.push(format!("已删除 {}", e.display()));
            }
        }
    }
    if let Ok(out) = host.run("crontab", &["-l"]) {
        if out.ok() && out.stdout.contains("/opt/b-ui/") {
            let kept = filter_cron(&out.stdout);
            let tmp = paths.base_dir.join(".crontab.new");
            if host.write_file(&tmp, kept.as_bytes(), 0o600).is_ok() {
                let _ = host.run("crontab", &[&tmp.display().to_string()]);
                let _ = host.remove_file(&tmp);
                done.push("已清理 b-ui 的 cron 行（其它行保留）".into());
            }
        }
    }
    if touched_units {
        let _ = host.systemd_daemon_reload();
    }
    // 最后一步：清掉 v3 早期版本留下的端口跳跃 NAT 孤儿链（spec §2.3、§3.1）。
    // 必须最后做：紧接着 install 第 9 步的对账会重写两个 hysteria 单元并重启，
    // hysteria 2.12 启动时自建它需要的链。
    done.extend(flush_v3_portjump_rules(host));
    done
}

/// `bui import-v3`：**只**从 v3 目录生成 `state.json`，不卸载 v3、不对账
/// （命令语义就是「先人工 diff」）。
pub async fn run(
    dir: PathBuf,
    out: Option<PathBuf>,
    paths: Paths,
    host: Arc<dyn Host>,
) -> anyhow::Result<()> {
    let report = bui_schema::v3::import(&dir)?;
    for w in &report.warnings {
        println!("导入提示：{w}");
        tracing::warn!("{w}");
    }
    let mut state = report.state;
    let (hostname, probe_ip, versions) = {
        let h = host.clone();
        let p = paths.clone();
        // 只在导入值为空时才兜底：探测失败绝不覆盖已导入的值
        let need_name = state.node.name.is_empty();
        let need_ip = state.node.public_ip.is_empty();
        tokio::task::spawn_blocking(move || {
            let hostname = if need_name {
                h.hostname().unwrap_or_default()
            } else {
                String::new()
            };
            let ip = if need_ip {
                h.run("curl", &["-sS", "--max-time", "5", "https://api.ipify.org"])
                    .ok()
                    .filter(|o| o.ok())
                    .map(|o| o.stdout.trim().to_string())
                    .unwrap_or_default()
            } else {
                String::new()
            };
            (
                hostname,
                ip,
                crate::kernels::installed_versions(h.as_ref(), &p.bin_dir),
            )
        })
        .await?
    };
    if state.node.name.is_empty() {
        state.node.name = hostname;
    }
    if state.node.public_ip.is_empty() {
        state.node.public_ip = probe_ip;
        if state.node.public_ip.is_empty() {
            println!("提示：v3 目录里没有公网 IP，探测也失败了，请稍后在面板里补填");
        }
    }
    let g = |k: &str| versions.get(k).cloned().unwrap_or_default();
    state.versions = bui_schema::model::Versions {
        bui: env!("CARGO_PKG_VERSION").to_string(),
        hysteria: g("hysteria"),
        xray: g("xray"),
        sing_box: g("sing-box"),
        caddy: g("caddy"),
        client_sing_box: g("sing-box"),
    };
    let target = out.unwrap_or_else(|| crate::paths::state_file(&paths));
    crate::state::store::Store::create(&target, state).await?;
    println!("已写出 {}", target.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::{fake::FakeHost, CmdOut};
    use pretty_assertions::assert_eq;

    const CRONTAB: &str = "\
# m h dom mon dow command
0 */6 * * * /opt/b-ui/update.sh auto >/dev/null 2>&1
0 */12 * * * /opt/b-ui/update.sh kernel >/dev/null 2>&1
30 3 * * * /usr/local/bin/backup-my-blog.sh
";

    fn scratch(d: &tempfile::TempDir) -> bui_schema::paths::Paths {
        bui_schema::paths::Paths {
            base_dir: d.path().into(),
            certs_dir: d.path().join("certs"),
            bin_dir: d.path().join("bin"),
        }
    }

    #[test]
    fn filter_cron_only_drops_b_ui_lines() {
        let out = filter_cron(CRONTAB);
        assert!(!out.contains("/opt/b-ui/update.sh"));
        assert!(out.contains("backup-my-blog.sh"), "别人的 cron 行必须留着");
        assert!(out.contains("# m h dom mon dow command"));
    }

    #[test]
    fn v3_units_never_touch_a_v4_managed_unit() {
        for u in V3_UNITS {
            let bare = u.trim_end_matches(".service").trim_end_matches(".timer");
            assert!(
                !crate::reconcile::MANAGED_UNITS.contains(&bare),
                "{u} 是 v4 受管单元，不能在卸载列表里"
            );
        }
        assert!(!V3_UNITS.contains(&"b-ui-relay.service"));
        // 两份 v3 单元表必须是包含关系：上一轮事故就是「units 与 drift 各有一份、内容还不一样」，
        // 结果 `hysteria-server@.service` / `xray@.service` 只有 --force 才清理。
        for u in V3_UNITS {
            assert!(
                crate::reconcile::LEGACY_UNITS.contains(&u),
                "{u} 不在 LEGACY_UNITS 里：uninstall 停了它、漂移扫描却不认它"
            );
        }
    }

    #[test]
    fn orphan_port_hopping_nat_rules_are_flushed() {
        // spec §2.3 + §3.1：v3 早期版本留下的 iptables/nft REDIRECT 规则没人清（v4 把
        // hy2-portjump-cleanup.sh 删了），这里在卸载的最后一步清掉孤儿链。
        let h = FakeHost::new();
        h.with(|i| {
            i.which.insert("iptables".into());
            i.which.insert("nft".into());
            i.scripted.push((
                "iptables -t nat -S".into(),
                CmdOut::success(concat!(
                    "-N HYSTERIA-PR-abc123\n",
                    "-A PREROUTING -p udp -m udp --dport 20000:30000 -j HYSTERIA-PR-abc123\n",
                    "-A HYSTERIA-PR-abc123 -p udp -j REDIRECT --to-ports 10000\n",
                )),
            ));
            i.scripted.push((
                "nft list tables".into(),
                CmdOut::success("table inet hysteria_abc123\n"),
            ));
        });
        let done = flush_v3_portjump_rules(&h);
        let ops = h.ops();
        assert!(
            ops.iter()
                .any(|o| o.contains("iptables") && o.contains("-D PREROUTING")),
            "先删跳转规则：{ops:?}"
        );
        assert!(ops.iter().any(|o| o.contains("-F HYSTERIA-PR-abc123")));
        assert!(ops.iter().any(|o| o.contains("-X HYSTERIA-PR-abc123")));
        assert!(ops
            .iter()
            .any(|o| o.contains("nft delete table inet hysteria_abc123")));
        assert!(done.iter().any(|l| l.contains("HYSTERIA-PR-abc123")));
    }

    #[test]
    fn flushing_nat_rules_on_a_clean_machine_reports_nothing() {
        let h = FakeHost::new(); // iptables / nft 都不存在
        assert_eq!(flush_v3_portjump_rules(&h), Vec::<String>::new());
    }

    #[test]
    fn uninstall_stops_v3_units_archives_state_deletes_shell_and_keeps_certs() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let h = FakeHost::new();
        h.with(|i| {
            for u in V3_UNITS {
                i.files.insert(
                    format!("/etc/systemd/system/{u}").into(),
                    (b"x".to_vec(), 0o644),
                );
                i.units_enabled.insert(u.to_string());
                i.units_active.insert(u.to_string());
            }
            for f in V3_FILES {
                i.files
                    .insert(d.path().join(f), (b"#!/bin/bash".to_vec(), 0o755));
            }
            for f in V3_STATE_FILES {
                i.files
                    .insert(d.path().join(f), (b"secret".to_vec(), 0o600));
            }
            // v3 迁移块与 CLI 留下的真实残留
            i.files.insert(
                d.path().join("config.yaml.bak.v357.1757000000"),
                (b"listen: :10000".to_vec(), 0o600),
            );
            i.files.insert(
                d.path().join("config.yaml.bak.obfs.20260901-120000"),
                (b"obfs".to_vec(), 0o600),
            );
            i.files.insert(
                d.path().join("xray-config.json.bak.v359.1757000001"),
                (b"{}".to_vec(), 0o600),
            );
            i.files.insert(
                d.path().join("config-residential.yaml.bak.v360.1757000002"),
                (b"resi".to_vec(), 0o600),
            );
            i.files.insert(
                d.path().join("xray-config.json.tmp"),
                (b"half".to_vec(), 0o600),
            );
            // v4 自己在管的四个配置：同名文件绝不能被这一步碰到
            i.files.insert(
                d.path().join("config.yaml"),
                (b"listen: :10000".to_vec(), 0o600),
            );
            i.files.insert(
                d.path().join("certs/fullchain.pem"),
                (b"CERT".to_vec(), 0o644),
            );
            i.files.insert(
                d.path().join("packages/versions.json"),
                (b"{}".to_vec(), 0o644),
            );
            i.files
                .insert(d.path().join("admin/server.js"), (b"node".to_vec(), 0o644));
            i.files.insert(
                d.path().join("admin/node_modules/y/index.js"),
                (b"y".to_vec(), 0o644),
            );
            i.files
                .insert("/tmp/hy2-watchdog-10000".into(), (b"2".to_vec(), 0o644));
            i.files
                .insert("/tmp/hy2-watchdog-40000".into(), (b"0".to_vec(), 0o644));
            i.files
                .insert("/tmp/unrelated.txt".into(), (b"keep".to_vec(), 0o644));
            i.scripted
                .push(("crontab -l".into(), CmdOut::success(CRONTAB)));
        });
        let done = uninstall_v3(&h, &paths);
        for u in V3_UNITS {
            assert!(
                h.ops().contains(&format!("systemd:disable:{u}")),
                "{u} 要停用"
            );
            assert!(
                h.text(&format!("/etc/systemd/system/{u}")).is_none(),
                "{u} 的单元文件要删"
            );
        }
        for f in V3_FILES {
            assert!(
                h.text(d.path().join(f).to_str().unwrap()).is_none(),
                "{f} 要删"
            );
        }
        for f in V3_STATE_FILES {
            assert!(
                h.text(d.path().join(f).to_str().unwrap()).is_none(),
                "{f} 要移走"
            );
            assert_eq!(
                h.text(
                    crate::paths::v3_backup_dir(&paths)
                        .join(f)
                        .to_str()
                        .unwrap()
                )
                .as_deref(),
                Some("secret"),
                "{f} 要进 v3-backup"
            );
            assert_eq!(
                h.mode(
                    crate::paths::v3_backup_dir(&paths)
                        .join(f)
                        .to_str()
                        .unwrap()
                ),
                Some(0o600)
            );
        }
        assert!(
            h.text(
                d.path()
                    .join("admin/node_modules/y/index.js")
                    .to_str()
                    .unwrap()
            )
            .is_none(),
            "Node 面板整棵删"
        );
        assert!(h
            .ops()
            .contains(&format!("rmdir:{}", d.path().join("admin").display())));
        assert_eq!(
            h.text(d.path().join("certs/fullchain.pem").to_str().unwrap())
                .as_deref(),
            Some("CERT"),
            "证书必须保留"
        );
        assert!(
            h.text(d.path().join("packages/versions.json").to_str().unwrap())
                .is_some(),
            "内核缓存保留"
        );
        assert!(
            h.text("/tmp/hy2-watchdog-10000").is_none()
                && h.text("/tmp/hy2-watchdog-40000").is_none()
        );
        assert_eq!(
            h.text("/tmp/unrelated.txt").as_deref(),
            Some("keep"),
            "只删自己的 /tmp 文件"
        );
        // v3 的迁移备份：归档（含明文密码）；临时文件：删掉；v4 在管的同名配置：不许碰
        for bak in [
            "config.yaml.bak.v357.1757000000",
            "config.yaml.bak.obfs.20260901-120000",
            "xray-config.json.bak.v359.1757000001",
            "config-residential.yaml.bak.v360.1757000002",
        ] {
            assert!(
                h.text(d.path().join(bak).to_str().unwrap()).is_none(),
                "{bak} 应移走"
            );
            assert!(
                h.text(
                    crate::paths::v3_backup_dir(&paths)
                        .join(bak)
                        .to_str()
                        .unwrap()
                )
                .is_some(),
                "{bak} 应进 v3-backup（漂移扫描不跳过 *.bak.*）"
            );
        }
        assert!(
            h.text(d.path().join("xray-config.json.tmp").to_str().unwrap())
                .is_none(),
            ".tmp 应删掉"
        );
        assert_eq!(
            h.text(d.path().join("config.yaml").to_str().unwrap())
                .as_deref(),
            Some("listen: :10000"),
            "v4 在管的配置本身不能被 sweep 碰到"
        );
        // cron 必须被**重写**：`crontab -l` 自己就会产生一条 `run:crontab -l`，所以断言
        // `starts_with("run:crontab")` 是恒真的空断言；要断言真正的写入命令。
        assert!(
            h.ops().contains(&format!(
                "run:crontab {}",
                d.path().join(".crontab.new").display()
            )),
            "cron 行要重写：{:?}",
            h.ops()
        );
        assert!(done.iter().any(|l| l.contains("已清理 b-ui 的 cron 行")));
        assert!(!done.is_empty());
    }

    #[test]
    fn caddy_acme_data_is_copied_before_the_distro_unit_is_stopped() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let h = FakeHost::new();
        let certs = format!(
            "{V3_CADDY_DATA}/certificates/acme-v02.api.letsencrypt.org-directory/example.com"
        );
        let acct =
            format!("{V3_CADDY_DATA}/acme/acme-v02.api.letsencrypt.org-directory/users/default");
        h.with(|i| {
            i.files.insert(
                format!("{certs}/example.com.crt").into(),
                (b"CERT".to_vec(), 0o644),
            );
            i.files.insert(
                format!("{certs}/example.com.key").into(),
                (b"KEY".to_vec(), 0o600),
            );
            i.files.insert(
                format!("{acct}/default.key").into(),
                (b"ACCT".to_vec(), 0o600),
            );
            i.scripted.push((
                "crontab -l".into(),
                CmdOut::failure(1, "no crontab for root"),
            ));
        });
        let done = uninstall_v3(&h, &paths);
        let dest = crate::paths::caddy_data(&paths);
        let key = dest.join(
            "certificates/acme-v02.api.letsencrypt.org-directory/example.com/example.com.key",
        );
        assert_eq!(
            h.text(key.to_str().unwrap()).as_deref(),
            Some("KEY"),
            "证书私钥要搬过来"
        );
        assert_eq!(h.mode(key.to_str().unwrap()), Some(0o600));
        assert_eq!(
            h.text(
                dest.join("acme/acme-v02.api.letsencrypt.org-directory/users/default/default.key")
                    .to_str()
                    .unwrap()
            )
            .as_deref(),
            Some("ACCT"),
            "ACME 账号私钥必须一起搬，否则 Caddy 重新注册账号并重签，撞 Let's Encrypt 速率限制"
        );
        let ops = h.ops();
        let stop = ops
            .iter()
            .position(|o| o == "systemd:stop:caddy")
            .expect("要停发行版 caddy");
        let last_copy = ops
            .iter()
            .rposition(|o| o.starts_with(&format!("write:{}", dest.display())))
            .expect("要复制文件");
        assert!(
            last_copy < stop,
            "必须先复制完再停 caddy，否则会出现无证书窗口：{ops:?}"
        );
        assert!(done.iter().any(|l| l.contains("Caddy 数据目录")));
    }

    #[test]
    fn caddy_migration_is_idempotent_and_never_overwrites() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let h = FakeHost::new();
        let dest = crate::paths::caddy_data(&paths).join("certificates/x/example.com.crt");
        h.with(|i| {
            i.files.insert(
                format!("{V3_CADDY_DATA}/certificates/x/example.com.crt").into(),
                (b"OLD".to_vec(), 0o600),
            );
            i.files.insert(dest.clone(), (b"NEW".to_vec(), 0o600));
        });
        let done = migrate_caddy_data(&h, &paths);
        assert_eq!(
            h.text(dest.to_str().unwrap()).as_deref(),
            Some("NEW"),
            "目标已有的文件不许被旧数据盖掉"
        );
        assert!(done.iter().all(|l| !l.contains("已复制")), "{done:?}");
    }

    #[test]
    fn uninstall_on_a_machine_without_v3_is_a_no_op() {
        let d = tempfile::tempdir().unwrap();
        let h = FakeHost::new();
        h.with(|i| {
            i.scripted.push((
                "crontab -l".into(),
                CmdOut::failure(1, "no crontab for root"),
            ))
        });
        let done = uninstall_v3(&h, &scratch(&d));
        assert_eq!(done, Vec::<String>::new());
        assert!(!h
            .ops()
            .iter()
            .any(|o| o.starts_with("remove:") || o.starts_with("rmdir:")));
    }

    #[tokio::test]
    async fn import_writes_state_from_a_v3_directory() {
        // 复用 P0 的合成 v3 fixture（crates/bui-schema/tests/fixtures/v3/src）
        let src = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../bui-schema/tests/fixtures/v3/src"
        ));
        if !src.exists() {
            eprintln!("skipped: 缺 P0 的 v3 fixture");
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let host = std::sync::Arc::new(FakeHost::new());
        host.with(|i| i.hostname = "node-b".into());
        run(src.to_path_buf(), None, paths.clone(), host)
            .await
            .unwrap();
        let state: bui_schema::model::State =
            serde_json::from_slice(&std::fs::read(crate::paths::state_file(&paths)).unwrap())
                .unwrap();
        assert_eq!(state.users.len(), 4);
        assert_eq!(state.node.domain, "example.com");
        assert_eq!(
            state.node.name, "example.com",
            "导入值优先，hostname 只在导入值为空时兜底"
        );
        assert_eq!(state.node.ports.hy2_hop, Some((20000, 30000)));
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(crate::paths::state_file(&paths))
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode, 0o600);
    }

    #[tokio::test]
    async fn import_probes_the_public_ip_only_when_it_is_missing() {
        let src = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../bui-schema/tests/fixtures/v3/src"
        ));
        if !src.exists() {
            eprintln!("skipped: 缺 P0 的 v3 fixture");
            return;
        }
        // fixture 里有 server_ip.txt → 导入值非空 → 不该调 curl 覆盖它
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let host = std::sync::Arc::new(FakeHost::new());
        host.with(|i| {
            i.scripted
                .push(("curl".into(), CmdOut::failure(7, "couldn't connect")))
        });
        run(src.to_path_buf(), None, paths.clone(), host.clone())
            .await
            .unwrap();
        let state: bui_schema::model::State =
            serde_json::from_slice(&std::fs::read(crate::paths::state_file(&paths)).unwrap())
                .unwrap();
        assert_eq!(
            state.node.public_ip, "203.0.113.10",
            "导入到的公网 IP 不能被失败的探测清空"
        );
        assert!(
            !host.ops().iter().any(|o| o.starts_with("run:curl")),
            "导入值非空就别探测"
        );
    }
}
