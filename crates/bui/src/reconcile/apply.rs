//! 把 [`Plan`] 落到机器上。[`apply`] 是**唯一真正改机器**的函数，步骤顺序写死（见下），
//! 测试按 [`crate::sys::fake::FakeHost`] 的 `ops` 流水逐条断言。
//!
//! 四条不变量：
//! 1. **校验再写盘**：`verify` 为 `Some` 时先把候选内容写进 `.verify/` 跑内核校验，失败不写目标文件（spec §2.2）。
//! 2. **配置没落地就搁置该单元**：带 `restart` 的 `WriteFile` 校验失败或写失败 ⇒ 该单元本轮的
//!    单元文件与重启一并搁置（held，第 1 / 3 / 12 步），绝不让「单元已换、配置未落」同时发生
//!    （2026-09-16 裁决 P-C）。
//! 3. **重启失败回滚**：重启失败就把该单元相关文件恢复成上一版内容再启一次，仍失败才记 `errors`。
//!    「起来了没有」一律以 `is-active` 为准而不是 `systemctl restart` 的退出码，且每次 restart
//!    之前先 `reset-failed` 清 start-limit（见 [`activate`]）。
//! 4. **绝不同步重启 `b-ui` 自己**：apply 跑在守护进程自己的进程里，只置
//!    [`ApplyOutcome::self_restart_required`]，由调用方在报告落盘之后处理（第 12 步）。

use super::diff::{Change, Plan};
use super::{Facts, PortSpec, Unit, UnitAction, Verify};
use crate::sys::Host;
use bui_schema::paths::Paths;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// 下载 → sha256 校验 → 原子替换一个内核二进制（实现在 `kernels`，Task 9）。
pub trait BinaryInstaller: Send + Sync {
    fn install(
        &self,
        name: &str,
        version: &str,
        sha256: &str,
        url: &str,
        dest: &Path,
    ) -> anyhow::Result<()>;
}

#[derive(Debug, Default, PartialEq)]
pub struct ApplyOutcome {
    pub changed: Vec<String>,
    pub restarted: Vec<String>,
    pub notes: Vec<String>,
    pub verify_failures: Vec<String>,
    pub errors: Vec<String>,
    /// 本轮**真正落地**的 `restart_key`：从 `plan.keys` 搬过来（第 1 步与第 10 步写明了搬的条件）。
    /// Task 15 的 `reconcile_once` 把它 `extend` 进 `runtime.restart_keys`；不搬 = 下一轮 `keys[id] != k`
    /// 永远成立 = 每次改 `clients` 都重启 xray（spec §3.3 失效），乱搬 = 该重启时不重启。
    pub keys: BTreeMap<String, String>,
    pub relay_restarted: bool,
    /// `b-ui.service` 自身需要重启；apply **绝不**自己动它（第 12 步），由调用方处理
    pub self_restart_required: bool,
}

pub struct ApplyInput<'a> {
    pub plan: Plan,
    pub paths: &'a Paths,
    pub facts: &'a Facts,
    pub installer: &'a dyn BinaryInstaller,
    pub dry_run: bool,
}

/// 守护进程自己的单元名：第 12 步把它从同步重启循环里摘出去。
const SELF_UNIT: &str = "b-ui";

/// systemd 单元文件与 drop-in 的权限。
const UNIT_MODE: u32 = 0o644;

/// 一个可回滚的写入点（重启失败时用它恢复上一版）。
struct Restore {
    path: PathBuf,
    /// `None` = 这个文件本轮才新建，回滚就是删掉它
    old: Option<Vec<u8>>,
    mode: u32,
}

pub fn apply(input: ApplyInput<'_>, host: &dyn Host) -> ApplyOutcome {
    let ApplyInput {
        plan,
        paths,
        facts,
        installer,
        dry_run,
    } = input;
    let Plan {
        changes,
        keys: candidate_keys,
        ..
    } = plan;

    // 第 13 步：dry-run 只描述，不调任何写操作
    if dry_run {
        return ApplyOutcome {
            changed: changes.iter().map(describe).collect(),
            ..Default::default()
        };
    }

    let mut out = ApplyOutcome::default();
    // 待重启/重载的单元集合（按 (name, action) 去重、按 name 排序）
    let mut restart: BTreeSet<Unit> = BTreeSet::new();
    // 单元名 → 本轮为它写过的文件（重启失败时按这份回滚）
    let mut restores: BTreeMap<String, Vec<Restore>> = BTreeMap::new();
    let mut need_daemon_reload = false;
    // 「配置这轮没落地」的单元 → 原因。第 1 步记，第 3 步与第 12 步据此搁置单元文件与重启。
    let mut held: BTreeMap<String, &'static str> = BTreeMap::new();

    // ---- 第 1 步：写文件（校验 → 去 immutable → 写 → 记 key / 重启 / 回滚点）
    for c in &changes {
        let Change::WriteFile {
            path,
            content,
            mode,
            verify,
            restart: unit,
        } = c
        else {
            continue;
        };
        let old = host.read_file(path).unwrap_or_default();
        let mut already_written = false;
        match verify {
            // sshd 只能校验全局配置，无法校验孤立文件（与 v3 `core.sh:1929` 同语义）：
            // 先写目标文件再 `sshd -t`，失败就恢复原样。
            Some(Verify::Sshd) => {
                if !clear_immutable(host, path, &mut out) {
                    hold(&mut held, unit, "写入失败");
                    continue;
                }
                if let Err(e) = host.write_file(path, content, *mode) {
                    out.errors.push(format!("{} 写入失败：{e}", path.display()));
                    hold(&mut held, unit, "写入失败");
                    continue;
                }
                if let Err(detail) = sshd_test(host) {
                    restore_one(host, path, old.as_deref(), *mode);
                    out.verify_failures
                        .push(format!("{} 校验失败：{detail}", path.display()));
                    hold(&mut held, unit, "未通过校验");
                    continue;
                }
                already_written = true;
            }
            Some(v) => match verify_candidate(host, paths, path, content, *v) {
                Ok(None) => {}
                Ok(Some(note)) => out.notes.push(note),
                Err(msg) => {
                    out.verify_failures.push(msg);
                    hold(&mut held, unit, "未通过校验");
                    continue;
                }
            },
            None => {}
        }
        if !already_written {
            if !clear_immutable(host, path, &mut out) {
                hold(&mut held, unit, "写入失败");
                continue;
            }
            if let Err(e) = host.write_file(path, content, *mode) {
                out.errors.push(format!("{} 写入失败：{e}", path.display()));
                hold(&mut held, unit, "写入失败");
                continue;
            }
        }
        out.changed.push(path.display().to_string());
        // `Artifact::id()` 的格式；只有写盘真的落地了才搬 key
        let id = format!("file:{}", path.display());
        if let Some(k) = candidate_keys.get(&id) {
            out.keys.insert(id, k.clone());
        }
        if let Some(u) = unit {
            restart.insert(u.clone());
            restores.entry(u.name.clone()).or_default().push(Restore {
                path: path.clone(),
                old,
                mode: *mode,
            });
        }
    }

    // ---- 第 2 步：immutable 位
    for c in &changes {
        let Change::SetImmutable { path, on } = c else {
            continue;
        };
        if *on {
            if let Err(e) = host.set_immutable(path, true) {
                out.errors
                    .push(format!("{} 设 immutable 位失败：{e}", path.display()));
                continue;
            }
            // overlayfs / 部分 VPS 模板上 `chattr +i` 静默无效：只提示，不当错误
            if !host.is_immutable(path).unwrap_or(false) {
                out.notes.push(format!(
                    "{} 所在文件系统不支持 chattr +i，已跳过 immutable 位",
                    path.display()
                ));
            }
        } else if host.is_immutable(path).unwrap_or(false) {
            if let Err(e) = host.set_immutable(path, false) {
                out.errors
                    .push(format!("{} 清 immutable 位失败：{e}", path.display()));
                continue;
            }
        }
        out.changed.push(path.display().to_string());
    }

    // ---- 第 3 步：单元文件
    for c in &changes {
        let Change::WriteUnit {
            path,
            content,
            unit,
        } = c
        else {
            continue;
        };
        // 该单元的配置本轮没落地 ⇒ 单元文件也不换：换了就是「ExecStart 指向一份不存在/旧版的
        // 配置」，内核起不来、旧配置又已被别的 Change 删掉时就是永久崩溃循环。
        if let Some(reason) = held.get(unit.name.as_str()) {
            out.notes.push(format!(
                "{} 的配置{reason}，单元文件本轮一并搁置",
                unit.name
            ));
            continue;
        }
        let old = host.read_file(path).unwrap_or_default();
        if let Err(e) = host.write_file(path, content.as_bytes(), UNIT_MODE) {
            out.errors.push(format!("{} 写入失败：{e}", path.display()));
            continue;
        }
        out.changed.push(path.display().to_string());
        need_daemon_reload = true;
        restart.insert(unit.clone());
        restores
            .entry(unit.name.clone())
            .or_default()
            .push(Restore {
                path: path.clone(),
                old,
                mode: UNIT_MODE,
            });
    }

    // ---- 第 4 步：只 daemon-reload 一次
    if need_daemon_reload {
        if let Err(e) = host.systemd_daemon_reload() {
            out.errors.push(format!("daemon-reload 失败：{e}"));
        }
    }

    // ---- 第 5 步：内核模块。**必须在 sysctl 之前**：`net.netfilter.nf_conntrack_*` 在模块
    // 未加载时 `sysctl -w` 直接 ENOENT，新装机会每 10 分钟报三条 errors、健康永久 degraded。
    for c in &changes {
        let Change::LoadModule { module } = c else {
            continue;
        };
        match host.modprobe(module) {
            Ok(()) => out.changed.push(module.clone()),
            // 有的内核把 conntrack 编进内核、没有可加载模块：只提示
            Err(e) => out
                .notes
                .push(format!("内核模块 {module} 加载失败（可能已编入内核）：{e}")),
        }
    }

    // ---- 第 6 步：sysctl
    for c in &changes {
        let Change::SetSysctl { key, value } = c else {
            continue;
        };
        match host.sysctl_set(key, value) {
            Ok(()) => {
                out.changed.push(key.clone());
                // 立刻读回：内核可能钳制或重排多值键（如把 `tcp_rmem` 的最小值抬到一个页）。
                // 不把读回值记成已生效值的话，下一轮 diff 又报一条 changed，「二次对账零变更」
                // 永远不成立（bwg-rick 真机 M1 step2）。
                if let Ok(Some(actual)) = host.sysctl_get(key) {
                    if !super::sysctl_tokens_eq(&actual, value) {
                        tracing::warn!(
                            key = %key,
                            want = %value,
                            got = %actual,
                            "sysctl 值被内核钳制/重排，以读回值为准"
                        );
                        out.notes.push(format!(
                            "sysctl {key} 写入 {value}，内核实际生效 {}（已按实际值记账）",
                            actual
                                .split_ascii_whitespace()
                                .collect::<Vec<_>>()
                                .join(" ")
                        ));
                        out.keys.insert(
                            format!("sysctl:{key}"),
                            super::sysctl_clamped_record(value, &actual),
                        );
                    }
                }
            }
            Err(e) => out.errors.push(format!("sysctl {key}={value} 失败：{e}")),
        }
    }

    // ---- 第 7 步：内核二进制
    for c in &changes {
        let Change::InstallBinary {
            name,
            version,
            sha256,
            url,
            path,
        } = c
        else {
            continue;
        };
        match installer.install(name, version, sha256, url, path) {
            Ok(()) => {
                out.changed.push(format!("{name} {version}"));
                for u in units_for_binary(name) {
                    restart.insert(u);
                }
            }
            Err(e) => out.errors.push(format!("{name} {version} 安装失败：{e}")),
        }
    }

    // ---- 第 7.5 步：nft 表（4.1 的住宅 HY2 端口跳跃）。
    // 排在二进制之后、单元状态（第 9 步停旧槽实例）与删除项（第 11 步删
    // `config-residential-<i>.yaml`）之前，所以「先清旧槽实例的孤儿 NAT 规则、再落自己这张表」
    // 天然成立（裁决 2026-09-16）。
    // 4.0 每槽一个 apernet 实例留下的 `4xxxx→:4000i` 与本表同挂 nat priority -100，
    // 先注册者先做 NAT ⇒ 那一片跳跃端口会被送到已经没人监听的端口。4.1 之后没有任何
    // 别的路径再清它们（住宅 prestart 换成 `bui nft apply`），所以在这里清一次。
    // **无条件跑**，不挂在 `Change::ApplyNftTable` 上：4.1 落过表 → `--rollback` 回 4.0
    // （不删表）→ 再升 4.1 时哈希与表都没变 ⇒ 不产变更，挂在变更上就一次清理都不跑，
    // 被 SIGKILL 的 4.0 槽实例的孤儿规则会带进 4.1。配置不在盘上时它是 no-op，
    // 所以对「二次对账零变更」没有影响。
    out.notes
        .extend(crate::modules::portjump::cleanup_legacy_residential(
            host, paths,
        ));
    for c in &changes {
        let Change::ApplyNftTable {
            family,
            name,
            ruleset,
            key,
        } = c
        else {
            continue;
        };
        if !host.which("nft") {
            // **不算 apply 失败**（机器上装不装包不是对账能决定的），但要按真实量级报：
            // 带 `mport` 的客户端只往跳跃段发、从不发 `:40000`，所以缺 nft 等于住宅 HY2
            // 对全体现役订阅**全断**，不是「只是跳跃失效」。装机与升级的硬闸门在
            // `env_probe::nft_blocking`；这里是活机器上被人卸了包的兜底提示。
            out.notes.push(format!(
                "PATH 上没有 nft，{family} {name} 表无法落地：住宅 HY2 的端口跳跃整段不通，\
                 带 mport 的现役订阅全部连不上（客户端只往跳跃段发，从不发单端口）。\
                 请安装 nftables 包后执行 `bui nft apply`"
            ));
            continue;
        }
        match host.run_stdin("nft", &["-f", "-"], ruleset) {
            Ok(o) if o.ok() => {
                out.changed.push(format!("nft table {family} {name}"));
                out.keys.insert(format!("nft:{family}:{name}"), key.clone());
            }
            Ok(o) => out.errors.push(format!(
                "nft -f 落地 {family} {name} 失败：{}",
                trim_detail(
                    if o.stderr.is_empty() {
                        &o.stdout
                    } else {
                        &o.stderr
                    }
                    .trim()
                )
            )),
            Err(e) => out
                .errors
                .push(format!("nft -f 落地 {family} {name} 无法执行：{e}")),
        }
    }

    // ---- 第 8 步：符号链接
    for c in &changes {
        let Change::WriteSymlink { path, target } = c else {
            continue;
        };
        match host.symlink(target, path) {
            Ok(()) => out.changed.push(path.display().to_string()),
            Err(e) => out
                .errors
                .push(format!("{} 建链接失败：{e}", path.display())),
        }
    }

    // ---- 第 9 步：单元状态。先停用（v3 遗留单元），再启用。
    let unit_states: Vec<(&String, bool, bool)> = changes
        .iter()
        .filter_map(|c| match c {
            Change::SetUnitState {
                unit,
                enabled,
                active,
            } => Some((unit, *enabled, *active)),
            _ => None,
        })
        .collect();
    for (unit, _, _) in &unit_states {
        out.changed.push((*unit).clone());
    }
    for (unit, enabled, active) in &unit_states {
        if !*enabled {
            let _ = host.systemd("disable", unit);
        }
        if !*active {
            let _ = host.systemd("stop", unit);
        }
    }
    for (unit, enabled, active) in &unit_states {
        if *enabled {
            let _ = host.systemd("enable", unit);
        }
        // 本轮要 restart 的单元不再额外 start：首装时那会变成「start 紧跟 restart」的双启动
        if *active && !restart.iter().any(|u| u.name.as_str() == unit.as_str()) {
            let _ = host.systemd("start", unit);
        }
    }

    // ---- 第 10 步：防火墙
    for c in &changes {
        let Change::OpenPorts { ports } = c else {
            continue;
        };
        if facts.ufw_active {
            for p in ports {
                let _ = host.run("ufw", &["allow", &p.ufw()]);
            }
        } else if facts.firewalld_active {
            for p in ports {
                let _ = host.run(
                    "firewall-cmd",
                    &["--permanent", &format!("--add-port={}", p.firewalld())],
                );
            }
            let _ = host.run("firewall-cmd", &["--reload"]);
        } else {
            // 守卫分支：Task 6 的 `SystemModule::render` 只在有活防火墙时才产出 `FirewallPorts`，
            // 所以 P1 的正常路径不会命中这里（提示由 `serve::reconcile_once` 从 facts 生成）。
            // 机器上什么都没改，所以**不搬** key——记了就等于骗下一轮说已放行。
            out.notes.push(format!(
                "未检测到 ufw/firewalld，请在云厂商安全组放行：{}",
                ports
                    .iter()
                    .map(PortSpec::ufw)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            continue;
        }
        out.changed.push(describe(c));
        if let Some(k) = candidate_keys.get("firewall") {
            out.keys.insert("firewall".to_string(), k.clone());
        }
    }

    // ---- 第 11 步：删除项
    let mut need_daemon_reload2 = false;
    // 本轮删过文件的受管单元 drop-in 目录（删完若空则连目录一起清掉，见循环之后）
    let mut dropin_dirs: BTreeSet<PathBuf> = BTreeSet::new();
    for c in &changes {
        let Change::RemoveFile { path } = c else {
            continue;
        };
        // 只在当前确实是 immutable 时才 `chattr -i`（无条件调会多一条流水）
        if host.is_immutable(path).unwrap_or(false) {
            let _ = host.set_immutable(path, false);
        }
        match host.remove_file(path) {
            Ok(()) => {
                out.changed.push(path.display().to_string());
                if path.starts_with("/etc/systemd/system") {
                    need_daemon_reload2 = true;
                }
                if let Some(dir) = path.parent().filter(|d| is_managed_dropin_dir(d)) {
                    dropin_dirs.insert(dir.to_path_buf());
                }
            }
            Err(e) => out.errors.push(format!("{} 删除失败：{e}", path.display())),
        }
    }
    // 受管单元的 `<unit>.service.d/` 被清空了就把目录一并删掉（**只删空目录**）：
    // 留一个空 drop-in 目录在那儿，官方安装器或云厂商 agent 下次再往里塞一个片段就又能悄悄
    // 覆盖 ExecStart，而单元文件本体对账起来毫无差异（2026-09-12 bwg-rick 首切的 xray 就是这样）。
    // 目录里还有别人的 drop-in 时一定要留着，否则连带删掉运维手写的片段。
    for dir in dropin_dirs {
        if host.is_dir(&dir).unwrap_or(false)
            && host.list_dir(&dir).map(|v| v.is_empty()).unwrap_or(false)
        {
            match host.remove_dir_all(&dir) {
                Ok(()) => {
                    out.changed.push(dir.display().to_string());
                    need_daemon_reload2 = true;
                }
                Err(e) => out.errors.push(format!("{} 删除失败：{e}", dir.display())),
            }
        }
    }
    if need_daemon_reload2 {
        if let Err(e) = host.systemd_daemon_reload() {
            out.errors.push(format!("daemon-reload 失败：{e}"));
        }
    }

    // ---- 第 12 步：重启/重载。`b-ui` 不在这个循环里（排序后它排第一，restart 自己会把后面的
    // 重启、报告与 restart_keys 全丢掉），只置 `self_restart_required` 交给调用方。
    for unit in restart {
        // 配置没落地就别重启：内核照旧跑着盘上那份仍然合法的旧配置，等下一轮校验过了再换。
        if let Some(reason) = held.get(unit.name.as_str()) {
            out.notes
                .push(format!("{} 的配置{reason}，重启本轮一并搁置", unit.name));
            continue;
        }
        if unit.name == SELF_UNIT {
            out.self_restart_required = true;
            continue;
        }
        let verb = match unit.action {
            UnitAction::Restart => "restart",
            UnitAction::Reload => "reload",
        };
        if activate(host, &unit) {
            record_restarted(&mut out, &unit.name);
            continue;
        }
        // 回滚该单元本轮写过的文件，再启一次
        let mut restored_a_unit_file = false;
        if let Some(items) = restores.get(&unit.name) {
            for r in items {
                restore_one(host, &r.path, r.old.as_deref(), r.mode);
                if r.path.starts_with("/etc/systemd/system") {
                    restored_a_unit_file = true;
                }
            }
        }
        if restored_a_unit_file {
            let _ = host.systemd_daemon_reload();
        }
        if activate(host, &unit) {
            record_restarted(&mut out, &unit.name);
            out.notes.push(format!(
                "{} {verb} 失败，已回滚上一版配置并重启成功",
                unit.name
            ));
        } else {
            // 上一版配置也起不来：这一轮对账必须是**失败**（errors 非空 → `/api/health` degraded、
            // `bui status` 显示 degraded 原因），并且把原因抄进去——2026-09-12 bwg-rick 首切时
            // 这里只报「已回滚上一版配置并重启成功」，真相是 start-limit-hit、caddy 处于 failed，
            // 同机托管的外部生产站点断了几分钟没人看出来。
            out.errors.push(format!(
                "回滚后 {} 仍未运行：{}",
                unit.name,
                failure_detail(host, &unit)
            ));
        }
    }
    out
}

/// `/etc/systemd/system/<受管单元>.service.d` → true。只认这一层：别人的单元
/// （发行版的 `ssh.service.d`、云厂商 agent 的目录）不是我们的地盘，空了也不许删。
fn is_managed_dropin_dir(dir: &Path) -> bool {
    dir.parent() == Some(Path::new("/etc/systemd/system"))
        && dir
            .file_name()
            .and_then(|s| s.to_str())
            .and_then(|n| n.strip_suffix(".service.d"))
            .is_some_and(super::is_managed_unit)
}

/// 让一个单元「现在真的在跑」：restart 走 `reset-failed` → `restart` → `is-active` 三步。
///
/// - **先 reset-failed**：连着几次 203/EXEC 之后 systemd 记住了 start-limit，后面的 `restart`
///   一律被拒（`start request repeated too quickly`）。不清这个计数器，回滚了也起不来，
///   正常路径同样会被上一轮的失败挡住——所以两条路径都先清。
/// - **以 is-active 为准**：`systemctl restart` 的退出码不可靠（退 0 而单元随即 failed 是常态），
///   只有 `is-active` 能回答「现在到底活着没有」。
///
/// `reload` 这一支只看退出码：reload 不改变激活态，而对一个没在跑的单元 `systemctl reload`
/// 本来就退非 0，退出码已经是可靠信号（多跑一次 reset-failed 反而会清掉运维要看的失败记录）。
fn activate(host: &dyn Host, unit: &Unit) -> bool {
    match unit.action {
        UnitAction::Reload => host.systemd("reload", &unit.name).is_ok_and(|o| o.ok()),
        UnitAction::Restart => {
            let _ = host.systemd("reset-failed", &unit.name);
            host.systemd("restart", &unit.name).is_ok_and(|o| o.ok())
                && host.unit_is_active(&unit.name).unwrap_or(false)
        }
    }
}

/// 单元没起来时给运维一行能直接抓的线索：优先 journal 最后一行，拿不到就退回
/// `ActiveState/SubState/Result` 三个属性。relay 的日志里可能带住宅上游的凭据，所以过一遍脱敏；
/// 长行截断，免得一条报错把 `/api/health` 的 JSON 撑爆。
fn failure_detail(host: &dyn Host, unit: &Unit) -> String {
    let full = unit.service();
    if host.which("journalctl") {
        if let Ok(o) = host.run(
            "journalctl",
            &["-u", &full, "-n", "1", "--no-pager", "-o", "cat"],
        ) {
            if let Some(line) = o.stdout.lines().rev().find(|l| !l.trim().is_empty()) {
                return trim_detail(&crate::redact::url_credentials(line.trim()));
            }
        }
    }
    let props: Vec<String> = ["ActiveState", "SubState", "Result"]
        .iter()
        .filter_map(|p| {
            host.unit_property(&unit.name, p)
                .ok()
                .flatten()
                .filter(|v| !v.trim().is_empty())
                .map(|v| format!("{p}={}", v.trim()))
        })
        .collect();
    if props.is_empty() {
        "journalctl 与 systemctl show 都没有输出，请人工看 `systemctl status`".to_string()
    } else {
        props.join(" ")
    }
}

/// 报错里的细节最多 240 字符。
fn trim_detail(s: &str) -> String {
    if s.chars().count() <= 240 {
        return s.to_string();
    }
    format!("{}…", s.chars().take(240).collect::<String>())
}

fn record_restarted(out: &mut ApplyOutcome, name: &str) {
    out.restarted.push(name.to_string());
    // Task 15 据此发 `Event::RelayRestarted`，P3 重放选中的上游
    if name == "b-ui-relay" {
        out.relay_restarted = true;
    }
}

/// 每条 `Change` 在报告里的描述（dry-run 与真跑用同一份，Task 15 的断言依赖它）。
fn describe(c: &Change) -> String {
    match c {
        Change::WriteFile { path, .. }
        | Change::SetImmutable { path, .. }
        | Change::RemoveFile { path }
        | Change::WriteUnit { path, .. }
        | Change::WriteSymlink { path, .. } => path.display().to_string(),
        Change::SetUnitState { unit, .. } => unit.clone(),
        Change::SetSysctl { key, .. } => key.clone(),
        Change::LoadModule { module } => module.clone(),
        Change::InstallBinary { name, version, .. } => format!("{name} {version}"),
        Change::OpenPorts { ports } => format!(
            "firewall: {}",
            ports
                .iter()
                .map(PortSpec::ufw)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Change::ApplyNftTable { family, name, .. } => format!("nft table {family} {name}"),
    }
}

/// 记一笔「这个单元的配置本轮没落地」（第 1 步的每条失败出口都要调）。没有 `restart` 的文件
/// 不影响任何单元，不记。
fn hold(held: &mut BTreeMap<String, &'static str>, unit: &Option<Unit>, reason: &'static str) {
    if let Some(u) = unit {
        held.insert(u.name.clone(), reason);
    }
}

/// 写盘前清掉 immutable 位；返回 false 表示清不掉（已记 errors，调用方跳过这一项）。
fn clear_immutable(host: &dyn Host, path: &Path, out: &mut ApplyOutcome) -> bool {
    if host.is_immutable(path).unwrap_or(false) {
        if let Err(e) = host.set_immutable(path, false) {
            out.errors
                .push(format!("{} 无法清除 immutable 位：{e}", path.display()));
            return false;
        }
    }
    true
}

/// 回滚一个写入点：有旧内容就写回去，本轮才新建的就删掉。
fn restore_one(host: &dyn Host, path: &Path, old: Option<&[u8]>, mode: u32) {
    match old {
        Some(bytes) => {
            let _ = host.write_file(path, bytes, mode);
        }
        None => {
            let _ = host.remove_file(path);
        }
    }
}

/// `sshd -t`：`Ok(())` = 通过，`Err(detail)` = 拒绝。
fn sshd_test(host: &dyn Host) -> Result<(), String> {
    match host.run("sshd", &["-t"]) {
        Ok(o) if o.ok() => Ok(()),
        Ok(o) => Err(if o.stderr.is_empty() {
            o.stdout
        } else {
            o.stderr
        }
        .trim()
        .to_string()),
        Err(e) => Err(e.to_string()),
    }
}

/// 返回 Ok(None)=校验通过；Ok(Some(note))=校验器不存在已跳过；Err(msg)=校验失败（不写盘）
fn verify_candidate(
    host: &dyn Host,
    paths: &Paths,
    path: &Path,
    content: &[u8],
    v: Verify,
) -> Result<Option<String>, String> {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("candidate");
    let program = match v {
        Verify::Xray => "xray",
        Verify::SingBox => "sing-box",
        Verify::Caddy => "caddy",
        Verify::Sshd => return Ok(None), // sshd 只能校验全局配置，见 apply 第 1 步的特例分支
    };
    // 校验器不存在（离线装机、内核还没下载）→ 跳过校验直接写盘，只记提示。
    // 反过来「校验不了就不写」会让 manifest 拉取失败的机器永远写不出配置（死锁）。
    let bundled = paths.bin_dir.join(program);
    let bin = if host
        .read_file(&bundled)
        .map(|o| o.is_some())
        .unwrap_or(false)
    {
        bundled.display().to_string()
    } else if host.which(program) {
        program.to_string()
    } else {
        return Ok(Some(format!(
            "校验器 {program} 不存在，已跳过 {} 的校验",
            path.display()
        )));
    };
    let candidate = crate::paths::verify_dir(paths).join(name);
    host.write_file(&candidate, content, 0o600)
        .map_err(|e| e.to_string())?;
    let cand = candidate.display().to_string();
    let out = match v {
        Verify::Xray => host.run(&bin, &["run", "-test", "-c", &cand]),
        Verify::SingBox => host.run(&bin, &["check", "-c", &cand]),
        Verify::Caddy => host.run(
            &bin,
            &["validate", "--config", &cand, "--adapter", "caddyfile"],
        ),
        Verify::Sshd => unreachable!("Sshd 已在上面提前返回"),
    };
    let _ = host.remove_file(&candidate);
    match out {
        Ok(o) if o.ok() => Ok(None),
        Ok(o) => Err(format!(
            "{} 校验失败：{}",
            path.display(),
            if o.stderr.is_empty() {
                o.stdout
            } else {
                o.stderr
            }
            .trim()
        )),
        Err(e) => Err(format!("{} 校验无法执行：{e}", path.display())),
    }
}

/// 换了内核二进制要重启哪些单元。`bui upgrade --rollback` 把内核换回上一版后也用它。
pub(crate) fn units_for_binary(name: &str) -> Vec<Unit> {
    match name {
        // 4.1 起住宅 HY2 跑的是 sing-box（`hy2-residential.json`），不再是 apernet hysteria：
        // 换 `hysteria` 只影响直连，换 `sing-box` 同时影响中继与住宅。漏了住宅那一条就会让
        // 升级 / `--rollback` 换完 sing-box 不重启住宅单元（它继续跑被原子替换掉的旧 inode）。
        "hysteria" => vec![Unit::restart("hysteria-server")],
        "xray" => vec![Unit::restart("xray")],
        "sing-box" => vec![
            Unit::restart("b-ui-relay"),
            Unit::restart("hysteria-residential"),
        ],
        "caddy" => vec![Unit::restart("caddy")],
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reconcile::diff::{Change, Plan};
    use crate::reconcile::{Facts, PortSpec, Unit, Verify};
    // `Host` 由上面的 `use super::*` 带进来（本文件顶部已 `use crate::sys::Host`），
    // 再显式导入一次会被 `-D warnings` 判成 unused_imports。
    use crate::sys::{fake::FakeHost, CmdOut, Proto};
    use bui_schema::paths::Paths;
    use pretty_assertions::assert_eq;
    use std::path::Path;

    struct NoopInstaller;
    impl BinaryInstaller for NoopInstaller {
        fn install(&self, _n: &str, _v: &str, _s: &str, _u: &str, _d: &Path) -> anyhow::Result<()> {
            Ok(())
        }
    }
    struct FailingInstaller;
    impl BinaryInstaller for FailingInstaller {
        fn install(&self, n: &str, _v: &str, _s: &str, _u: &str, _d: &Path) -> anyhow::Result<()> {
            anyhow::bail!("{n}: sha256 不匹配")
        }
    }

    fn facts() -> Facts {
        Facts {
            mem_mb: 2048,
            arch: "x86_64".into(),
            hostname: "node-a".into(),
            has_ufw: true,
            ufw_active: true,
            has_firewalld: false,
            firewalld_active: false,
            ssh_unit: "sshd".into(),
            ssh_pubkeys: 1,
            systemd_resolved: false,
            nft_tables: Default::default(),
        }
    }

    fn run(plan: Plan, host: &FakeHost, installer: &dyn BinaryInstaller) -> ApplyOutcome {
        let paths = Paths::default_server();
        apply(
            ApplyInput {
                plan,
                paths: &paths,
                facts: &facts(),
                installer,
                dry_run: false,
            },
            host,
        )
    }

    /// 4.1：一个 `nft -f -` 事务把整份规则集喂进去（**不进 argv**），成功后才搬
    /// `nft:inet:bui` 这条记账 —— 不搬就等于每轮都重放，搬早了等于表没落地还骗下一轮说落地了。
    #[test]
    fn the_nft_table_is_landed_in_one_transaction_and_only_then_records_its_key() {
        let h = FakeHost::new();
        h.with(|i| {
            i.which.insert("nft".into());
        });
        let ruleset = "table inet bui\nflush table inet bui\ntable inet bui {}\n";
        let mut keys = std::collections::BTreeMap::new();
        keys.insert("nft:inet:bui".to_string(), "abc123".to_string());
        let out = run(
            Plan {
                changes: vec![Change::ApplyNftTable {
                    family: "inet".into(),
                    name: "bui".into(),
                    ruleset: ruleset.into(),
                    key: "abc123".into(),
                }],
                keys,
                unchanged: 0,
            },
            &h,
            &NoopInstaller,
        );
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(out.changed, vec!["nft table inet bui".to_string()]);
        assert_eq!(
            out.keys.get("nft:inet:bui").map(String::as_str),
            Some("abc123")
        );
        assert_eq!(
            h.stdins(),
            vec![("nft -f -".to_string(), ruleset.to_string())],
            "规则集经 stdin 喂进去，一个事务一次调用"
        );
        assert!(
            h.ops()
                .iter()
                .filter(|o| o.starts_with("run:nft -f"))
                .count()
                == 1,
            "{:?}",
            h.ops()
        );
    }

    /// `nft -f` 被内核拒掉（内核 < 5.2 的 `Chain of type "nat" is not supported`）⇒
    /// 记 `errors`（体检 degraded）且**不搬** key：下一轮还要重放。
    #[test]
    fn a_rejected_nft_transaction_is_an_error_and_records_nothing() {
        let h = FakeHost::new();
        h.with(|i| {
            i.which.insert("nft".into());
            i.scripted.push((
                "nft -f -".into(),
                CmdOut::failure(1, "Error: Chain of type \"nat\" is not supported\n"),
            ));
        });
        let mut keys = std::collections::BTreeMap::new();
        keys.insert("nft:inet:bui".to_string(), "abc123".to_string());
        let out = run(
            Plan {
                changes: vec![Change::ApplyNftTable {
                    family: "inet".into(),
                    name: "bui".into(),
                    ruleset: "table inet bui\n".into(),
                    key: "abc123".into(),
                }],
                keys,
                unchanged: 0,
            },
            &h,
            &NoopInstaller,
        );
        assert_eq!(out.changed, Vec::<String>::new());
        assert!(out.keys.is_empty(), "{:?}", out.keys);
        assert_eq!(out.errors.len(), 1, "{:?}", out.errors);
        assert!(out.errors[0].contains("Chain of type"), "{:?}", out.errors);
    }

    /// 缺 `nft`：**不算 apply 失败**（装包不是对账能决定的），但提示必须报出真实量级 ——
    /// 带 `mport` 的客户端只往跳跃段发、从不发单端口，所以这不是「跳跃失效」而是住宅 HY2
    /// 对全体现役订阅全断。key 同样不搬。
    #[test]
    fn a_host_without_nft_gets_a_note_sized_to_the_real_blast_radius() {
        let h = FakeHost::new();
        let out = run(
            Plan {
                changes: vec![Change::ApplyNftTable {
                    family: "inet".into(),
                    name: "bui".into(),
                    ruleset: "table inet bui\n".into(),
                    key: "abc123".into(),
                }],
                keys: Default::default(),
                unchanged: 0,
            },
            &h,
            &NoopInstaller,
        );
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(out.changed, Vec::<String>::new());
        assert!(out.keys.is_empty());
        assert_eq!(out.notes.len(), 1, "{:?}", out.notes);
        assert!(out.notes[0].contains("现役订阅"), "{:?}", out.notes);
        assert!(out.notes[0].contains("nftables"), "{:?}", out.notes);
        assert!(
            !h.ops().iter().any(|o| o.starts_with("run:nft")),
            "{:?}",
            h.ops()
        );
    }

    /// 槽 1 的 4.0 实例（base 40001、跳跃切片 45500-50000）被 SIGKILL 之后留下的孤儿：
    /// 一条完整链 + PREROUTING 上的跳转（形态照 `portjump` 的真机实录夹具）。
    const OLD_SLOT_DUMP: &str = "\
-P PREROUTING ACCEPT
-N HYSTERIA-PR-7c1e0f2a
-A PREROUTING -p udp -m udp --dport 45500:50000 -j HYSTERIA-PR-7c1e0f2a
-A HYSTERIA-PR-7c1e0f2a -p udp -j REDIRECT --to-ports 40001
";

    /// 裁决 2026-09-16（相邻缺口）：落自己这张表**之前**，先把 4.0 每槽实例留下的孤儿
    /// NAT 规则清掉 —— 它们与 `inet bui` 同挂 nat priority -100，先注册者先做 NAT，
    /// 残留就会把一片跳跃端口送到已经没人监听的 `:4000i`。
    #[test]
    fn the_orphan_rules_of_the_old_slot_instances_are_cleaned_before_the_table_lands() {
        let h = FakeHost::new();
        h.with(|i| {
            i.which.insert("nft".into());
            i.which.insert("iptables".into());
            // 槽 1 的 4.0 配置还在盘上（本轮第 11 步才删），它的孤儿链也还在
            i.files.insert(
                "/opt/b-ui/config-residential-1.yaml".into(),
                (b"listen: :40001,45500-50000\n".to_vec(), 0o600),
            );
            i.scripted
                .push(("iptables -t nat -S".into(), CmdOut::success(OLD_SLOT_DUMP)));
            i.scripted.push((
                "nft list tables".into(),
                CmdOut::success("table inet bui\n"),
            ));
        });
        let out = run(
            Plan {
                changes: vec![Change::ApplyNftTable {
                    family: "inet".into(),
                    name: "bui".into(),
                    ruleset: "table inet bui\n".into(),
                    key: "abc123".into(),
                }],
                keys: Default::default(),
                unchanged: 0,
            },
            &h,
            &NoopInstaller,
        );
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let ops = h.ops();
        let x = ops
            .iter()
            .position(|o| o == "run:iptables -t nat -X HYSTERIA-PR-7c1e0f2a")
            .unwrap_or_else(|| panic!("没清掉槽 1 的孤儿链：{ops:?}"));
        let land = ops
            .iter()
            .position(|o| o == "run:nft -f -")
            .unwrap_or_else(|| panic!("没落地 nft 表：{ops:?}"));
        assert!(x < land, "清理必须发生在 nft -f 之前：{ops:?}");
        assert!(
            out.notes
                .iter()
                .any(|n| n.contains("config-residential-1.yaml")
                    && n.contains("HYSTERIA-PR-7c1e0f2a")),
            "清理要留一行可追溯的说明：{:?}",
            out.notes
        );
        // 盘上没有的那些配置一个命令都不发（上一轮已经清过 ⇒ 二次对账零多余动作）
        assert!(
            !ops.iter().any(|o| o.contains("HYSTERIA-PR-1111aaaa")),
            "{ops:?}"
        );
    }

    /// 槽 0 的 4.0 实例（`config-residential.yaml`，base 40000 + 整段 41000-50000，
    /// 单槽机器唯一有的那份）被 SIGKILL 之后留下的孤儿。
    const SLOT0_DUMP: &str = "\
-P PREROUTING ACCEPT
-N HYSTERIA-PR-5e0b13c4
-A PREROUTING -p udp -m udp --dport 41000:50000 -j HYSTERIA-PR-5e0b13c4
-A OUTPUT -p udp -m udp --dport 41000:50000 -j HYSTERIA-PR-5e0b13c4
-A HYSTERIA-PR-5e0b13c4 -p udp -j REDIRECT --to-ports 40000
";

    /// 清理**不许**挂在 `Change::ApplyNftTable` 上：4.1 落过表 → `bui upgrade --rollback`
    /// 回 4.0（不删表）→ 再升 4.1 时哈希与表都没变 ⇒ 本轮零 nft 变更；挂在变更上就
    /// 一次清理都不跑，被 SIGKILL 的 4.0 槽实例的孤儿规则会带进 4.1。
    #[test]
    fn the_legacy_slot_cleanup_runs_even_when_the_table_needs_no_change() {
        let h = FakeHost::new();
        h.with(|i| {
            i.which.insert("nft".into());
            i.which.insert("iptables".into());
            i.files.insert(
                "/opt/b-ui/config-residential.yaml".into(),
                (b"listen: :40000,41000-50000\n".to_vec(), 0o600),
            );
            i.scripted
                .push(("iptables -t nat -S".into(), CmdOut::success(SLOT0_DUMP)));
        });
        // 一条变更都没有的计划（= 表在、哈希没变的那一轮）
        let out = run(
            Plan {
                changes: vec![],
                keys: Default::default(),
                unchanged: 12,
            },
            &h,
            &NoopInstaller,
        );
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let ops = h.ops();
        assert!(
            ops.iter()
                .any(|o| o == "run:iptables -t nat -X HYSTERIA-PR-5e0b13c4"),
            "零 nft 变更的那一轮也必须清槽 0 的孤儿链：{ops:?}"
        );
        assert!(
            out.notes.iter().any(
                |n| n.contains("config-residential.yaml") && n.contains("HYSTERIA-PR-5e0b13c4")
            ),
            "清理要留一行可追溯的说明：{:?}",
            out.notes
        );
        assert!(out.changed.is_empty(), "清理不产变更：{:?}", out.changed);
    }

    /// 4.1 起住宅 HY2 跑的是 sing-box：换 `sing-box` 必须连住宅单元一起重启，
    /// 换 `hysteria` 只影响直连。漏了住宅那一条 = `bui upgrade` 与 `--rollback` 换完
    /// sing-box 不重启它，住宅数据面永远停在换之前那一版内核（`bui status` 却报已升级）。
    #[test]
    fn swapping_singbox_restarts_both_the_relay_and_the_residential_unit() {
        assert_eq!(
            units_for_binary("sing-box"),
            vec![
                Unit::restart("b-ui-relay"),
                Unit::restart("hysteria-residential"),
            ]
        );
        assert_eq!(
            units_for_binary("hysteria"),
            vec![Unit::restart("hysteria-server")],
            "住宅已不用 apernet，换它不该白断一次住宅的在线连接"
        );
        assert_eq!(units_for_binary("xray"), vec![Unit::restart("xray")]);
        assert_eq!(units_for_binary("caddy"), vec![Unit::restart("caddy")]);
        assert!(units_for_binary("bui").is_empty());
    }

    /// 同一映射的端到端形态：一轮对账里换了 sing-box，住宅单元要真的被重启。
    #[test]
    fn installing_singbox_restarts_the_residential_unit_in_the_same_round() {
        let h = FakeHost::new();
        let out = run(
            Plan {
                changes: vec![Change::InstallBinary {
                    name: "sing-box".into(),
                    version: "1.14.0".into(),
                    sha256: "aa".into(),
                    url: "https://example.com/sing-box".into(),
                    path: "/opt/b-ui/bin/sing-box".into(),
                }],
                keys: Default::default(),
                unchanged: 0,
            },
            &h,
            &NoopInstaller,
        );
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let ops = h.ops();
        assert!(
            ops.iter()
                .any(|o| o == "systemd:restart:hysteria-residential"),
            "{ops:?}"
        );
        assert!(
            ops.iter().any(|o| o == "systemd:restart:b-ui-relay"),
            "{ops:?}"
        );
    }

    #[test]
    fn writes_file_then_reloads_and_restarts_in_fixed_order() {
        let h = FakeHost::new();
        let plan = Plan {
            changes: vec![
                Change::WriteFile {
                    path: "/opt/b-ui/config.yaml".into(),
                    content: b"listen: :10000".to_vec(),
                    mode: 0o600,
                    verify: None,
                    restart: Some(Unit::restart("hysteria-server")),
                },
                Change::WriteUnit {
                    path: "/etc/systemd/system/b-ui.service".into(),
                    content: "[Service]\n".into(),
                    unit: Unit::restart("b-ui"),
                },
            ],
            keys: Default::default(),
            unchanged: 0,
        };
        let out = run(plan, &h, &NoopInstaller);
        assert_eq!(
            h.ops(),
            vec![
                "write:/opt/b-ui/config.yaml:600",
                "write:/etc/systemd/system/b-ui.service:644",
                "daemon-reload",
                // restart 之前一定先 reset-failed：上一轮留下的 start-limit 会让这次直接被拒
                "systemd:reset-failed:hysteria-server",
                "systemd:restart:hysteria-server",
            ],
            "b-ui 不在同步重启序列里（第 12 步）：在守护进程里第一条就会把自己杀掉"
        );
        assert_eq!(out.restarted, vec!["hysteria-server".to_string()]);
        assert!(out.self_restart_required, "改由调用方在报告落盘后重启 b-ui");
        assert!(out.errors.is_empty());
    }

    #[test]
    fn the_daemons_own_unit_is_never_restarted_inside_apply() {
        // 排序后 `b-ui` 排第一；apply 跑在守护进程自己的进程里，restart 自己 = 后面的重启、
        // runtime.json 的报告与 restart_keys 全丢，下次启动时文件已一致 → 内核永远跑旧配置。
        let h = FakeHost::new();
        let plan = Plan {
            changes: vec![
                Change::WriteUnit {
                    path: "/etc/systemd/system/b-ui.service".into(),
                    content: "[Service]\n".into(),
                    unit: Unit::restart("b-ui"),
                },
                Change::WriteUnit {
                    path: "/etc/systemd/system/xray.service".into(),
                    content: "[Service]\n".into(),
                    unit: Unit::restart("xray"),
                },
            ],
            keys: Default::default(),
            unchanged: 0,
        };
        let out = run(plan, &h, &NoopInstaller);
        assert!(
            !h.ops().iter().any(|o| o == "systemd:restart:b-ui"),
            "apply 不许重启守护进程自己：{:?}",
            h.ops()
        );
        assert_eq!(out.restarted, vec!["xray".to_string()]);
        assert!(out.self_restart_required);
    }

    #[test]
    fn restart_keys_are_recorded_only_for_the_writes_that_landed() {
        // 校验失败的文件不记 key（否则下次真写成功时该重启的单元不重启）；
        // 写成功的文件与真正调过 ufw 的防火墙才记（否则每轮都重启 xray / 重复 ufw allow）。
        let h = FakeHost::new();
        h.with(|i| {
            i.files
                .insert("/opt/b-ui/bin/xray".into(), (b"ELF".to_vec(), 0o755));
            i.scripted.push((
                "/opt/b-ui/bin/xray run -test".into(),
                CmdOut::failure(1, "bad inbound"),
            ));
        });
        let plan = Plan {
            changes: vec![
                Change::WriteFile {
                    path: "/opt/b-ui/xray-config.json".into(),
                    content: b"{}".to_vec(),
                    mode: 0o600,
                    verify: Some(Verify::Xray),
                    restart: Some(Unit::restart("xray")),
                },
                Change::WriteFile {
                    path: "/opt/b-ui/config.yaml".into(),
                    content: b"listen: :10000".to_vec(),
                    mode: 0o600,
                    verify: None,
                    restart: Some(Unit::restart("hysteria-server")),
                },
                Change::OpenPorts {
                    ports: vec![PortSpec::one(Proto::Udp, 40000)],
                },
            ],
            keys: std::collections::BTreeMap::from([
                (
                    "file:/opt/b-ui/xray-config.json".to_string(),
                    "hash-A".to_string(),
                ),
                (
                    "file:/opt/b-ui/config.yaml".to_string(),
                    "hash-B".to_string(),
                ),
                ("firewall".to_string(), "fw-1".to_string()),
            ]),
            unchanged: 0,
        };
        let out = run(plan, &h, &NoopInstaller);
        assert_eq!(
            out.keys,
            std::collections::BTreeMap::from([
                (
                    "file:/opt/b-ui/config.yaml".to_string(),
                    "hash-B".to_string()
                ),
                ("firewall".to_string(), "fw-1".to_string()),
            ])
        );
    }

    #[test]
    fn verify_failure_blocks_the_write_and_is_reported() {
        let h = FakeHost::new();
        h.with(|i| {
            i.files
                .insert("/opt/b-ui/bin/xray".into(), (b"ELF".to_vec(), 0o755));
            i.scripted.push((
                "/opt/b-ui/bin/xray run -test".into(),
                CmdOut::failure(1, "bad inbound"),
            ));
        });
        let plan = Plan {
            changes: vec![Change::WriteFile {
                path: "/opt/b-ui/xray-config.json".into(),
                content: b"{}".to_vec(),
                mode: 0o600,
                verify: Some(Verify::Xray),
                restart: Some(Unit::restart("xray")),
            }],
            keys: Default::default(),
            unchanged: 0,
        };
        let out = run(plan, &h, &NoopInstaller);
        assert!(
            h.text("/opt/b-ui/xray-config.json").is_none(),
            "校验失败不写盘"
        );
        assert_eq!(out.changed, Vec::<String>::new());
        assert_eq!(out.restarted, Vec::<String>::new());
        assert_eq!(out.verify_failures.len(), 1);
        assert!(out.verify_failures[0].contains("bad inbound"));
        assert!(h
            .ops()
            .iter()
            .any(|o| o.starts_with("write:/opt/b-ui/.verify/xray-config.json")));
    }

    /// 2026-09-16 裁决 P-C：配置校验失败时，**同一单元**的单元文件与重启一并搁置。
    ///
    /// 没有这道门控时一次对账会同时做成「单元 ExecStart 已指向新配置」+「新配置因校验
    /// FATAL 从未落盘」，而同轮别的 Change 已把旧配置删掉 ⇒ 内核每次启动都找不到配置文件、
    /// `Restart=always` 变成永久崩溃循环，没有自愈点（4.1 住宅 HY2 换 sing-box 就是这个形状）。
    #[test]
    fn a_failed_config_verification_holds_the_unit_file_and_the_restart() {
        let h = FakeHost::new();
        h.with(|i| {
            i.files
                .insert("/opt/b-ui/bin/sing-box".into(), (b"ELF".to_vec(), 0o755));
            i.scripted.push((
                "/opt/b-ui/bin/sing-box check".into(),
                CmdOut::failure(1, "FATAL v2ray api is not included in this build"),
            ));
        });
        let unit_path = "/etc/systemd/system/hysteria-residential.service";
        let plan = Plan {
            changes: vec![
                Change::WriteFile {
                    path: "/opt/b-ui/hy2-residential.json".into(),
                    content: b"{}".to_vec(),
                    mode: 0o600,
                    verify: Some(Verify::SingBox),
                    restart: Some(Unit::restart("hysteria-residential")),
                },
                Change::WriteUnit {
                    path: unit_path.into(),
                    content: "[Service]\nExecStart=/opt/b-ui/bin/sing-box run\n".into(),
                    unit: Unit::restart("hysteria-residential"),
                },
            ],
            keys: Default::default(),
            unchanged: 0,
        };
        let out = run(plan, &h, &NoopInstaller);
        assert!(
            h.text("/opt/b-ui/hy2-residential.json").is_none(),
            "校验失败不写盘"
        );
        assert!(h.text(unit_path).is_none(), "配置没落地，单元文件一并搁置");
        assert_eq!(out.verify_failures.len(), 1, "{:?}", out.verify_failures);
        assert_eq!(out.restarted, Vec::<String>::new());
        assert_eq!(out.changed, Vec::<String>::new());
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        // 校验失败的文件本来就没进 `restart`（第 1 步只有写盘落地才收），所以这一轮只有
        // 单元文件那一条搁置 note；重启那一条由下一个用例（`InstallBinary` 塞进来的重启）覆盖。
        assert_eq!(
            out.notes,
            vec!["hysteria-residential 的配置未通过校验，单元文件本轮一并搁置".to_string()]
        );
        assert!(
            !h.ops()
                .iter()
                .any(|o| o.contains("hysteria-residential") || o == "daemon-reload"),
            "{:?}",
            h.ops()
        );
    }

    /// 同一单元的另一个文件写成功也不解除搁置：**部分落地**正是要防的那半个状态。
    /// 内核二进制换新（`InstallBinary` 把住宅单元塞进 restart）同样被搁置挡住。
    #[test]
    fn one_landed_file_does_not_release_a_held_unit() {
        let h = FakeHost::new();
        h.with(|i| {
            i.files
                .insert("/opt/b-ui/bin/sing-box".into(), (b"ELF".to_vec(), 0o755));
            i.scripted.push((
                "/opt/b-ui/bin/sing-box check".into(),
                CmdOut::failure(1, "FATAL bad inbound"),
            ));
        });
        let plan = Plan {
            changes: vec![
                Change::WriteFile {
                    path: "/opt/b-ui/hy2-residential.json".into(),
                    content: b"{}".to_vec(),
                    mode: 0o600,
                    verify: Some(Verify::SingBox),
                    restart: Some(Unit::restart("hysteria-residential")),
                },
                Change::WriteFile {
                    path: "/opt/b-ui/other.json".into(),
                    content: b"{}".to_vec(),
                    mode: 0o600,
                    verify: None,
                    restart: Some(Unit::restart("hysteria-residential")),
                },
                Change::InstallBinary {
                    name: "sing-box".into(),
                    version: "1.14.1".into(),
                    sha256: "aa".into(),
                    url: "https://x/sb".into(),
                    path: "/opt/b-ui/bin/sing-box".into(),
                },
            ],
            keys: Default::default(),
            unchanged: 0,
        };
        let out = run(plan, &h, &NoopInstaller);
        assert_eq!(
            h.text("/opt/b-ui/other.json").as_deref(),
            Some("{}"),
            "校验通过的那个文件照写"
        );
        assert_eq!(
            out.restarted,
            vec!["b-ui-relay".to_string()],
            "换了 sing-box 该重启的中继照重启，住宅单元被搁置"
        );
        assert!(out
            .notes
            .iter()
            .any(|n| n == "hysteria-residential 的配置未通过校验，重启本轮一并搁置"));
    }

    /// 搁置的另一半理由：**写盘失败**（盘满 / 只读挂载）。校验失败那一半有上面两个用例钉着，
    /// 写失败这一半在假机器造不出写失败之前是裸的 —— 把四处 `hold(.., "写入失败")` 全删掉，
    /// 整套用例仍会全绿。所以 `FakeHost` 有了 `fail_writes`，这个用例专钉这一半：
    /// 配置写不进去时，同一单元的单元文件绝不能换（换了就是「ExecStart 指向一份不存在的
    /// 配置」+ `Restart=always` = 永久崩溃循环）。
    #[test]
    fn a_failed_config_write_also_holds_the_unit_file_and_the_restart() {
        let cfg = "/opt/b-ui/hy2-residential.json";
        let unit_path = "/etc/systemd/system/hysteria-residential.service";
        let h = FakeHost::new();
        h.with(|i| {
            i.fail_writes.insert(cfg.into());
        });
        let plan = Plan {
            changes: vec![
                Change::WriteFile {
                    path: cfg.into(),
                    content: b"{}".to_vec(),
                    mode: 0o600,
                    // 校验不是这条路径的前提：`verify: None` 也照样会写失败
                    verify: None,
                    restart: Some(Unit::restart("hysteria-residential")),
                },
                Change::WriteUnit {
                    path: unit_path.into(),
                    content: "[Service]\nExecStart=/opt/b-ui/bin/sing-box run\n".into(),
                    unit: Unit::restart("hysteria-residential"),
                },
            ],
            keys: Default::default(),
            unchanged: 0,
        };
        let out = run(plan, &h, &NoopInstaller);
        assert!(h.text(cfg).is_none(), "写失败就是没落盘");
        assert!(h.text(unit_path).is_none(), "配置没落地，单元文件一并搁置");
        assert_eq!(out.errors.len(), 1, "{:?}", out.errors);
        assert!(out.errors[0].contains("写入失败"), "{:?}", out.errors);
        assert_eq!(out.restarted, Vec::<String>::new());
        assert_eq!(out.changed, Vec::<String>::new());
        assert_eq!(
            out.notes,
            vec!["hysteria-residential 的配置写入失败，单元文件本轮一并搁置".to_string()]
        );
        assert!(
            !h.ops()
                .iter()
                .any(|o| o.contains("hysteria-residential") || o == "daemon-reload"),
            "{:?}",
            h.ops()
        );
    }

    #[test]
    fn restart_failure_restores_the_previous_file_and_retries_once() {
        let h = FakeHost::new();
        h.with(|i| {
            i.files.insert(
                "/opt/b-ui/config.yaml".into(),
                (b"listen: :9999".to_vec(), 0o600),
            );
            i.fail_units.insert("hysteria-server".into());
        });
        let plan = Plan {
            changes: vec![Change::WriteFile {
                path: "/opt/b-ui/config.yaml".into(),
                content: b"listen: :10000".to_vec(),
                mode: 0o600,
                verify: None,
                restart: Some(Unit::restart("hysteria-server")),
            }],
            keys: Default::default(),
            unchanged: 0,
        };
        let out = run(plan, &h, &NoopInstaller);
        assert_eq!(
            h.text("/opt/b-ui/config.yaml").unwrap(),
            "listen: :9999",
            "回滚到上一版内容"
        );
        assert_eq!(
            h.ops(),
            vec![
                "write:/opt/b-ui/config.yaml:600",
                "systemd:reset-failed:hysteria-server",
                "systemd:restart:hysteria-server",
                "write:/opt/b-ui/config.yaml:600",
                "systemd:reset-failed:hysteria-server",
                "systemd:restart:hysteria-server",
            ]
        );
        assert_eq!(out.errors.len(), 1);
        assert!(
            out.errors[0].contains("回滚后 hysteria-server 仍未运行"),
            "{}",
            out.errors[0]
        );
    }

    /// 2026-09-12 bwg-rick 首切实录：caddy 单元 203/EXEC 之后报告写「已回滚上一版配置并重启成功」，
    /// 真相是 start-limit-hit、单元处于 failed，同机托管的外部生产站点断了几分钟没人看出来。
    /// 判据只能是 `is-active`：`systemctl restart` 的退出码退 0 不代表单元活着。
    #[test]
    fn a_restart_that_returns_zero_but_leaves_the_unit_dead_is_reported_as_still_down() {
        let h = FakeHost::new();
        h.with(|i| {
            // restart 退 0，单元却永远起不来（203/EXEC、start-limit-hit 都是这个形态）
            i.never_active.insert("caddy".into());
            i.which.insert("journalctl".into());
            i.scripted.push((
                "journalctl -u caddy.service".into(),
                CmdOut::success("caddy.service: Start request repeated too quickly.\n"),
            ));
            i.files
                .insert("/opt/b-ui/Caddyfile".into(), (b"old\n".to_vec(), 0o644));
        });
        let plan = Plan {
            changes: vec![Change::WriteFile {
                path: "/opt/b-ui/Caddyfile".into(),
                content: b"new\n".to_vec(),
                mode: 0o644,
                verify: None,
                restart: Some(Unit::restart("caddy")),
            }],
            keys: Default::default(),
            unchanged: 0,
        };
        let out = run(plan, &h, &NoopInstaller);
        assert_eq!(
            h.ops(),
            vec![
                "write:/opt/b-ui/Caddyfile:644",
                "systemd:reset-failed:caddy",
                "systemd:restart:caddy",
                "write:/opt/b-ui/Caddyfile:644",
                "systemd:reset-failed:caddy",
                "systemd:restart:caddy",
                "run:journalctl -u caddy.service -n 1 --no-pager -o cat",
            ],
            "reset-failed 必须排在每次 restart 之前（start-limit 是常态坑）"
        );
        assert_eq!(
            h.text("/opt/b-ui/Caddyfile").as_deref(),
            Some("old\n"),
            "回滚到上一版内容"
        );
        assert!(out.restarted.is_empty(), "没起来就不算重启成功");
        assert!(
            !out.notes.iter().any(|n| n.contains("重启成功")),
            "不许再报「回滚上一版配置并重启成功」：{:?}",
            out.notes
        );
        assert_eq!(out.errors.len(), 1, "{:?}", out.errors);
        assert!(
            out.errors[0].contains("回滚后 caddy 仍未运行")
                && out.errors[0].contains("Start request repeated too quickly"),
            "{}",
            out.errors[0]
        );
    }

    /// 拿不到 journal（机器上没有 journalctl / 输出为空）时退回单元属性，而不是给一句空原因。
    #[test]
    fn without_journalctl_the_failure_reason_falls_back_to_unit_properties() {
        let h = FakeHost::new();
        h.with(|i| {
            i.never_active.insert("xray".into());
            i.unit_props.insert(
                ("xray.service".into(), "ActiveState".into()),
                "failed".into(),
            );
            i.unit_props
                .insert(("xray.service".into(), "Result".into()), "exit-code".into());
        });
        let plan = Plan {
            changes: vec![Change::WriteUnit {
                path: "/etc/systemd/system/xray.service".into(),
                content: "[Service]\n".into(),
                unit: Unit::restart("xray"),
            }],
            keys: Default::default(),
            unchanged: 0,
        };
        let out = run(plan, &h, &NoopInstaller);
        assert_eq!(out.errors.len(), 1, "{:?}", out.errors);
        assert!(
            out.errors[0].contains("回滚后 xray 仍未运行")
                && out.errors[0].contains("ActiveState=failed")
                && out.errors[0].contains("Result=exit-code"),
            "{}",
            out.errors[0]
        );
    }

    #[test]
    fn a_missing_verifier_skips_verification_and_still_writes() {
        // 离线装机：bin/ 里还没有 xray，PATH 上也没有。派生文件照写，只记一条提示。
        let h = FakeHost::new();
        let plan = Plan {
            changes: vec![Change::WriteFile {
                path: "/opt/b-ui/xray-config.json".into(),
                content: b"{}".to_vec(),
                mode: 0o600,
                verify: Some(Verify::Xray),
                restart: Some(Unit::restart("xray")),
            }],
            keys: Default::default(),
            unchanged: 0,
        };
        let out = run(plan, &h, &NoopInstaller);
        assert_eq!(h.text("/opt/b-ui/xray-config.json").as_deref(), Some("{}"));
        assert!(out.verify_failures.is_empty());
        assert_eq!(out.notes.len(), 1);
        assert!(out.notes[0].contains("xray") && out.notes[0].contains("跳过"));
    }

    #[test]
    fn modules_are_loaded_before_sysctl_and_failures_are_only_notes() {
        let h = FakeHost::new();
        let plan = Plan {
            changes: vec![
                Change::LoadModule {
                    module: "nf_conntrack".into(),
                },
                Change::SetSysctl {
                    key: "net.netfilter.nf_conntrack_max".into(),
                    value: "131072".into(),
                },
            ],
            keys: Default::default(),
            unchanged: 0,
        };
        let out = run(plan, &h, &NoopInstaller);
        assert_eq!(
            h.ops(),
            vec![
                "modprobe:nf_conntrack",
                "sysctl:net.netfilter.nf_conntrack_max=131072"
            ],
            "先 modprobe 再 sysctl，否则 net.netfilter.* 全 ENOENT"
        );
        assert!(out.errors.is_empty());
        assert_eq!(
            out.changed,
            vec![
                "nf_conntrack".to_string(),
                "net.netfilter.nf_conntrack_max".to_string()
            ]
        );
    }

    #[test]
    fn a_clamped_sysctl_is_recorded_as_effective_with_one_note() {
        // 内核把写入值钳制/重排了（这里用假机器的 `sysctl_clamp` 模拟）：apply 立刻读回、
        // 把「写入值 → 读回值」记进 keys 并留一条提示，下一轮 diff 才不会再报 changed。
        let h = FakeHost::new();
        h.with(|i| {
            i.sysctl_clamp
                .insert("net.ipv4.udp_mem".into(), "8192\t524288\t1048576".into());
        });
        let plan = Plan {
            changes: vec![
                Change::SetSysctl {
                    key: "net.ipv4.udp_mem".into(),
                    value: "262144 524288 1048576".into(),
                },
                Change::SetSysctl {
                    key: "net.ipv4.tcp_rmem".into(),
                    value: "4096 262144 16777216".into(),
                },
            ],
            keys: Default::default(),
            unchanged: 0,
        };
        let out = run(plan, &h, &NoopInstaller);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(
            out.keys.get("sysctl:net.ipv4.udp_mem").map(String::as_str),
            Some(super::super::sysctl_clamped_record(
                "262144 524288 1048576",
                "8192\t524288\t1048576"
            ))
            .as_deref()
        );
        assert!(
            !out.keys.contains_key("sysctl:net.ipv4.tcp_rmem"),
            "没被钳制的键不记账（读回值与写入值只差制表符）：{:?}",
            out.keys
        );
        assert_eq!(out.notes.len(), 1, "{:?}", out.notes);
        assert!(
            out.notes[0].contains("net.ipv4.udp_mem")
                && out.notes[0].contains("8192 524288 1048576"),
            "{:?}",
            out.notes
        );
    }

    #[test]
    fn symlinks_are_written() {
        let h = FakeHost::new();
        let plan = Plan {
            changes: vec![Change::WriteSymlink {
                path: "/usr/local/bin/b-ui".into(),
                target: "/opt/b-ui/bin/bui".into(),
            }],
            keys: Default::default(),
            unchanged: 0,
        };
        let out = run(plan, &h, &NoopInstaller);
        assert_eq!(
            h.ops(),
            vec!["symlink:/usr/local/bin/b-ui->/opt/b-ui/bin/bui"]
        );
        assert_eq!(out.changed, vec!["/usr/local/bin/b-ui".to_string()]);
    }

    #[test]
    fn a_freshly_written_unit_is_enabled_then_restarted_once_not_started_twice() {
        let h = FakeHost::new();
        let plan = Plan {
            changes: vec![
                Change::WriteUnit {
                    path: "/etc/systemd/system/xray.service".into(),
                    content: "[Service]\n".into(),
                    unit: Unit::restart("xray"),
                },
                Change::SetUnitState {
                    unit: "xray".into(),
                    enabled: true,
                    active: true,
                },
            ],
            keys: Default::default(),
            unchanged: 0,
        };
        let out = run(plan, &h, &NoopInstaller);
        assert_eq!(
            h.ops(),
            vec![
                "write:/etc/systemd/system/xray.service:644",
                "daemon-reload",
                "systemd:enable:xray",
                "systemd:reset-failed:xray",
                "systemd:restart:xray",
            ],
            "本轮要 restart 的单元不再额外 start（首装时会变成 start 后紧跟 restart 的双启动）"
        );
        assert_eq!(out.restarted, vec!["xray".to_string()]);
    }

    #[test]
    fn an_already_written_unit_that_is_down_gets_started() {
        let h = FakeHost::new();
        h.with(|i| {
            i.files.insert(
                "/etc/systemd/system/xray.service".into(),
                (b"[Service]\n".to_vec(), 0o644),
            );
        });
        let plan = Plan {
            changes: vec![Change::SetUnitState {
                unit: "xray".into(),
                enabled: true,
                active: true,
            }],
            keys: Default::default(),
            unchanged: 1,
        };
        run(plan, &h, &NoopInstaller);
        assert_eq!(h.ops(), vec!["systemd:enable:xray", "systemd:start:xray"]);
    }

    #[test]
    fn relay_restart_is_flagged_for_upstream_replay() {
        let h = FakeHost::new();
        let plan = Plan {
            changes: vec![Change::WriteFile {
                path: "/opt/b-ui/singbox-relay.json".into(),
                content: b"{}".to_vec(),
                mode: 0o600,
                verify: None,
                restart: Some(Unit::restart("b-ui-relay")),
            }],
            keys: Default::default(),
            unchanged: 0,
        };
        let out = run(plan, &h, &NoopInstaller);
        assert!(
            out.relay_restarted,
            "relay 重启后要重放 selected_upstream_id"
        );
    }

    #[test]
    fn caddyfile_change_reloads_instead_of_restarting() {
        let h = FakeHost::new();
        let plan = Plan {
            changes: vec![Change::WriteFile {
                path: "/opt/b-ui/Caddyfile".into(),
                content: b"example.com {}\n".to_vec(),
                mode: 0o644,
                verify: None,
                restart: Some(Unit::reload("caddy")),
            }],
            keys: Default::default(),
            unchanged: 0,
        };
        run(plan, &h, &NoopInstaller);
        assert!(h.ops().contains(&"systemd:reload:caddy".to_string()));
        assert!(!h.ops().iter().any(|o| o == "systemd:restart:caddy"));
    }

    #[test]
    fn binary_install_failure_is_an_error_and_skips_the_restart() {
        let h = FakeHost::new();
        let plan = Plan {
            changes: vec![Change::InstallBinary {
                name: "sing-box".into(),
                version: "1.13.19".into(),
                sha256: "bb".into(),
                url: "https://x/z".into(),
                path: "/opt/b-ui/bin/sing-box".into(),
            }],
            keys: Default::default(),
            unchanged: 0,
        };
        let out = run(plan, &h, &FailingInstaller);
        assert_eq!(out.errors.len(), 1);
        assert!(out.errors[0].contains("sha256"));
        assert!(out.restarted.is_empty());
    }

    #[test]
    fn open_ports_uses_ufw_when_active() {
        let h = FakeHost::new();
        let plan = Plan {
            changes: vec![Change::OpenPorts {
                ports: vec![
                    PortSpec::one(Proto::Tcp, 22),
                    PortSpec::range(Proto::Udp, 41000, 50000),
                ],
            }],
            keys: Default::default(),
            unchanged: 0,
        };
        run(plan, &h, &NoopInstaller);
        assert_eq!(
            h.ops(),
            vec!["run:ufw allow 22/tcp", "run:ufw allow 41000:50000/udp"]
        );
    }

    /// 守卫分支（第 10 步）：`SystemModule` 在没有活防火墙时根本不产出 `FirewallPorts`，
    /// 所以这条测试直接构造 `Plan`。它保证 P2/P3 将来自己产出这个 artifact 时不会静默失败。
    #[test]
    fn open_ports_without_a_firewall_becomes_a_health_note() {
        let h = FakeHost::new();
        let paths = Paths::default_server();
        let mut f = facts();
        f.has_ufw = false;
        f.ufw_active = false;
        let plan = Plan {
            changes: vec![Change::OpenPorts {
                ports: vec![PortSpec::one(Proto::Udp, 40000)],
            }],
            keys: Default::default(),
            unchanged: 0,
        };
        let out = apply(
            ApplyInput {
                plan,
                paths: &paths,
                facts: &f,
                installer: &NoopInstaller,
                dry_run: false,
            },
            &h,
        );
        assert!(h.ops().is_empty());
        assert_eq!(out.notes.len(), 1);
        assert!(out.notes[0].contains("40000/udp"));
        assert!(
            out.keys.is_empty(),
            "什么都没改就不记 firewall key，下一轮还要再提醒一次"
        );
    }

    /// 顺序：先 disable+stop、再删文件、最后一次 `daemon-reload`（第 11 步的 `need_daemon_reload2`）。
    /// `chattr -i` 只在文件当前确实是 immutable 时才调，所以这里的 ops 里没有 `chattr:-i:` 一行。
    #[test]
    fn removals_disable_and_stop_first_then_delete_then_daemon_reload() {
        let h = FakeHost::new();
        h.with(|i| {
            i.files.insert(
                "/etc/systemd/system/hy2-watchdog.timer".into(),
                (b"x".to_vec(), 0o644),
            );
            i.units_enabled.insert("hy2-watchdog.timer".into());
            i.units_active.insert("hy2-watchdog.timer".into());
        });
        let plan = Plan {
            changes: vec![
                Change::SetUnitState {
                    unit: "hy2-watchdog.timer".into(),
                    enabled: false,
                    active: false,
                },
                Change::RemoveFile {
                    path: "/etc/systemd/system/hy2-watchdog.timer".into(),
                },
            ],
            keys: Default::default(),
            unchanged: 0,
        };
        run(plan, &h, &NoopInstaller);
        assert_eq!(
            h.ops(),
            vec![
                "systemd:disable:hy2-watchdog.timer",
                "systemd:stop:hy2-watchdog.timer",
                "remove:/etc/systemd/system/hy2-watchdog.timer",
                "daemon-reload",
            ]
        );
    }

    #[test]
    fn dry_run_touches_nothing() {
        let h = FakeHost::new();
        let paths = Paths::default_server();
        let plan = Plan {
            changes: vec![Change::WriteFile {
                path: "/opt/b-ui/config.yaml".into(),
                content: b"x".to_vec(),
                mode: 0o600,
                verify: None,
                restart: None,
            }],
            keys: Default::default(),
            unchanged: 0,
        };
        let out = apply(
            ApplyInput {
                plan,
                paths: &paths,
                facts: &facts(),
                installer: &NoopInstaller,
                dry_run: true,
            },
            &h,
        );
        assert!(h.ops().is_empty());
        assert_eq!(out.changed, vec!["/opt/b-ui/config.yaml".to_string()]);
    }
}
