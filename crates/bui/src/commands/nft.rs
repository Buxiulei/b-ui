//! `bui nft apply|status|delete`：住宅 HY2 端口跳跃那张 `inet bui` 表的三个运维入口
//! （4.1，spec §2.4）。
//!
//! 规则集只有 [`bui_schema::render::nft::ruleset`] 一处实现，这里只负责把它喂给 `nft`。
//!
//! **`apply` 必须能在守护进程没跑的时候工作**：它是住宅单元的
//! `ExecStartPre=-{bin}/bui nft apply`（今天两处幂等重放之一，另一处是每轮对账；
//! watchdog 每 60 秒那第三处 **T12 起**才有，现在还没有那段代码），
//! 而单元启动时 `b-ui.service` 可能还没起来。所以它**直接读
//! `state.json` + 自己跑 `nft`，不经 `/run/b-ui.sock`**。
//!
//! 落地前先 `nft -c -f -` 预检，失败就以可操作的中文错误显式失败 —— 绝不让跳跃静默消失
//! （2026-09-16 裁决）：`inet` 族的 `type nat` 链要内核 ≥ 5.2，更低的内核会把整份事务拒掉
//! （`Error: Chain of type "nat" is not supported…`），而 4.1 的住宅 sing-box 只监听单端口，
//! 于是该机住宅用户升级后当场断联。装机与升级路径上的硬闸门在
//! [`crate::sys::env_probe::nft_blocking`]。

use crate::sys::Host;
use anyhow::{Context, Result};
use bui_schema::model::State;
use bui_schema::paths::Paths;
use bui_schema::render::nft;

/// 幂等重放 `inet bui`：读期望态 → 预检 → 一个 `nft -f -` 事务整表替换。
pub fn apply(host: &dyn Host, paths: &Paths) -> Result<String> {
    let s = load_state(host, paths)?;
    let ruleset = nft::ruleset(&s.node.ports, s.system.hy2_resi_compat_ports);
    require_nft(host)?;
    // 预检与落地喂同一份规则集：`-c` 只解析 + 问内核要不要，不改任何东西。
    let check = host
        .run_stdin("nft", &["-c", "-f", "-"], &ruleset)
        .context("nft -c -f 无法执行")?;
    if !check.ok() {
        anyhow::bail!(
            "nft 预检拒绝了 {} 的规则集：{}\n\
             `inet` 族的 `type nat` 链要求 Linux 内核 ≥ 5.2（本机 {}）。\
             内核过低请先升内核；其它原因请把上面那行原文贴进 issue —— \
             这张表不落地就等于住宅 HY2 的端口跳跃整段不通，\
             带 mport 的现役订阅全部连不上（4.1 的住宅只监听单端口 {}）。",
            nft::TABLE,
            detail(&check),
            kernel_release(host).unwrap_or_else(|| "未知".into()),
            s.node.ports.hy2_resi
        );
    }
    let out = host
        .run_stdin("nft", &["-f", "-"], &ruleset)
        .context("nft -f 无法执行")?;
    if !out.ok() {
        anyhow::bail!("nft -f 落地 {} 失败：{}", nft::TABLE, detail(&out));
    }
    let (hop_a, hop_b) = s.node.ports.hy2_resi_hop;
    let compat = if s.system.hy2_resi_compat_ports {
        let (a, b) = nft::compat_range(&s.node.ports);
        format!("、兼容段 {a}-{b}")
    } else {
        String::new()
    };
    Ok(format!(
        "已落地 table {}（{} 条规则）：{hop_a}-{hop_b}{compat} → :{}",
        nft::TABLE,
        nft::rule_count(s.system.hy2_resi_compat_ports),
        s.node.ports.hy2_resi
    ))
}

/// 表在不在 + 四条规则的**瞬时**流计数。
///
/// 这里打出来的 counter **不是**兼容段的下线判据（2026-09-16 裁决）：`flush table` 每次
/// 重放都把它清回 0，nat 链的 counter 又只计每条 conntrack 流的首包。累计命中数与
/// `last_hit_at` 由守护进程在每次重放之前采样、累加进 `runtime.json`，
/// `bui status` 与 `bui set hy2-resi-compat off` 的 30 天门禁读的是那份持久值。
pub fn status(host: &dyn Host, paths: &Paths) -> Result<String> {
    let s = load_state(host, paths)?;
    require_nft(host)?;
    let out = host
        .run("nft", &["list", "table", nft::FAMILY, nft::NAME])
        .context("nft list table 无法执行")?;
    if !out.ok() {
        return Ok(format!(
            "table {} 不存在（住宅 HY2 的端口跳跃整段不通）：执行 `bui nft apply` 或等下一轮对账重放",
            nft::TABLE
        ));
    }
    let mut lines = vec![format!(
        "table {} 在位，期望 {} 条规则",
        nft::TABLE,
        nft::rule_count(s.system.hy2_resi_compat_ports)
    )];
    for line in out.stdout.lines().map(str::trim) {
        if line.starts_with("udp dport") {
            lines.push(format!("  {line}"));
        }
    }
    lines.push(
        "（counter 是瞬时流计数：每次重放都从 0 开始，兼容段的下线判据看 `bui status` 里的累计值）"
            .into(),
    );
    Ok(lines.join("\n"))
}

/// 删表（今天只有手工排障用；`bui upgrade --rollback` **T15 起**才会调它，spec §9.1）。
/// 表不存在只记一行，不算失败。
pub fn delete(host: &dyn Host, _paths: &Paths) -> Result<String> {
    if !host.which("nft") {
        return Ok("PATH 上没有 nft，没有表可删".into());
    }
    let out = host
        .run("nft", &["delete", "table", nft::FAMILY, nft::NAME])
        .context("nft delete table 无法执行")?;
    if out.ok() {
        return Ok(format!("已删除 table {}", nft::TABLE));
    }
    // `No such file or directory` = 表本来就不在：回滚路径按 note 处理，不算失败
    Ok(format!(
        "table {} 未删除（可能本来就不存在）：{}",
        nft::TABLE,
        detail(&out)
    ))
}

/// 期望态直接从盘上读：`apply` 是 `ExecStartPre`，守护进程可能还没起来。
fn load_state(host: &dyn Host, paths: &Paths) -> Result<State> {
    let path = crate::paths::state_file(paths);
    let bytes = host
        .read_file(&path)?
        .with_context(|| format!("读不到期望态 {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("{} 解析失败", path.display()))
}

fn require_nft(host: &dyn Host) -> Result<()> {
    anyhow::ensure!(
        host.which("nft"),
        "PATH 上没有 nft：4.1 的住宅 HY2 端口跳跃靠它维护的 {} 表实现，\
         请先安装 nftables 包（bui 不装系统包）",
        nft::TABLE
    );
    Ok(())
}

fn kernel_release(host: &dyn Host) -> Option<String> {
    let out = host.run("uname", &["-r"]).ok()?;
    let v = out.stdout.trim();
    (out.ok() && !v.is_empty()).then(|| v.to_string())
}

fn detail(out: &crate::sys::CmdOut) -> String {
    let s = if out.stderr.is_empty() {
        &out.stdout
    } else {
        &out.stderr
    };
    s.trim()
        .lines()
        .next()
        .unwrap_or("（没有输出）")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::{fake::FakeHost, CmdOut};
    use pretty_assertions::assert_eq;

    fn seeded() -> (tempfile::TempDir, Paths, FakeHost) {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths {
            base_dir: d.path().into(),
            certs_dir: d.path().join("certs"),
            bin_dir: d.path().join("bin"),
        };
        let h = FakeHost::new();
        let s = crate::testutil::sample_state();
        h.write_file(
            &crate::paths::state_file(&paths),
            &serde_json::to_vec(&s).unwrap(),
            0o600,
        )
        .unwrap();
        h.with(|i| {
            i.which.insert("nft".into());
        });
        // 播种那次写盘不算被测代码的流水
        h.clear_ops();
        (d, paths, h)
    }

    /// `apply` 直接读 `state.json`、自己跑 `nft`：它是 `ExecStartPre`，守护进程可能没起来。
    /// 预检必须在落地**之前**（裁决 2026-09-16），两次都喂同一份规则集。
    #[test]
    fn apply_prechecks_then_lands_the_ruleset_from_the_state_file() {
        let (_d, paths, h) = seeded();
        let msg = apply(&h, &paths).unwrap();
        assert_eq!(
            h.ops(),
            vec!["run:nft -c -f -".to_string(), "run:nft -f -".to_string()],
            "先预检再落地，一条多余的命令都没有"
        );
        let want =
            bui_schema::render::nft::ruleset(&crate::testutil::sample_state().node.ports, true);
        assert_eq!(
            h.stdins(),
            vec![
                ("nft -c -f -".to_string(), want.clone()),
                ("nft -f -".to_string(), want),
            ],
            "规则集经 stdin 喂进去（不进 argv），两次一模一样"
        );
        assert!(msg.contains("41000-50000"), "{msg}");
        assert!(msg.contains("40001-40007"), "{msg}");
        assert!(msg.contains("4 条规则"), "{msg}");
    }

    /// 预检不过就**显式失败**，一次 `nft -f` 都不发；错误里要有内核下限与可操作的下一步。
    #[test]
    fn a_failed_precheck_refuses_to_land_anything() {
        let (_d, paths, h) = seeded();
        h.with(|i| {
            i.scripted.push((
                "nft -c -f -".into(),
                CmdOut::failure(
                    1,
                    "Error: Chain of type \"nat\" is not supported, perhaps kernel support is missing?\n",
                ),
            ));
            i.scripted
                .push(("uname -r".into(), CmdOut::success("4.19.0-21-amd64\n")));
        });
        let err = apply(&h, &paths).unwrap_err().to_string();
        assert!(err.contains("Chain of type"), "{err}");
        assert!(err.contains("5.2"), "要给出内核下限：{err}");
        assert!(err.contains("4.19.0-21-amd64"), "要报本机内核：{err}");
        assert!(err.contains("现役订阅"), "要报真实量级：{err}");
        assert!(
            !h.ops().contains(&"run:nft -f -".to_string()),
            "预检失败还落地就等于把整段跳跃悄悄换成一张空表：{:?}",
            h.ops()
        );
    }

    /// 兼容段关掉之后规则从 4 条变 2 条，文案也跟着变。
    #[test]
    fn turning_the_compat_range_off_lands_two_rules() {
        let (_d, paths, h) = seeded();
        let mut s = crate::testutil::sample_state();
        s.system.hy2_resi_compat_ports = false;
        h.write_file(
            &crate::paths::state_file(&paths),
            &serde_json::to_vec(&s).unwrap(),
            0o600,
        )
        .unwrap();
        let msg = apply(&h, &paths).unwrap();
        assert!(msg.contains("2 条规则"), "{msg}");
        assert!(!msg.contains("40001-40007"), "{msg}");
        assert!(!h.stdins()[0].1.contains("40001-40007"));
    }

    /// 缺 nft ⇒ 显式失败、一条命令都不发（装机与升级的硬闸门在 `env_probe::nft_blocking`，
    /// 这里是活机器上被人卸了包的路径）。
    #[test]
    fn without_nft_apply_fails_with_the_install_hint() {
        let (_d, paths, h) = seeded();
        h.with(|i| {
            i.which.remove("nft");
        });
        let err = apply(&h, &paths).unwrap_err().to_string();
        assert!(err.contains("nftables"), "{err}");
        assert!(err.contains("inet bui"), "{err}");
        assert!(h.ops().is_empty(), "{:?}", h.ops());
    }

    #[test]
    fn status_says_so_when_the_table_is_missing() {
        let (_d, paths, h) = seeded();
        h.with(|i| {
            i.scripted.push((
                "nft list table inet bui".into(),
                CmdOut::failure(1, "Error: No such file or directory\n"),
            ));
        });
        let msg = status(&h, &paths).unwrap();
        assert!(msg.contains("不存在"), "{msg}");
        assert!(msg.contains("bui nft apply"), "{msg}");
    }

    #[test]
    fn status_prints_the_rule_counters_but_calls_them_instantaneous() {
        let (_d, paths, h) = seeded();
        h.with(|i| {
            i.scripted.push((
                "nft list table inet bui".into(),
                CmdOut::success(
                    "table inet bui {\n\tchain prerouting {\n\t\ttype nat hook prerouting priority dstnat; policy accept;\n\t\tudp dport 41000-50000 counter packets 12 bytes 480 redirect to :40000 comment \"hy2 residential hop\"\n\t\tudp dport 40001-40007 counter packets 0 bytes 0 redirect to :40000 comment \"hy2 residential 4.0 compat\"\n\t}\n}\n",
                ),
            ));
        });
        let msg = status(&h, &paths).unwrap();
        assert!(msg.contains("在位，期望 4 条规则"), "{msg}");
        assert!(msg.contains("packets 12"), "{msg}");
        assert!(
            msg.contains("瞬时流计数"),
            "counter 不是兼容段的下线判据，必须说清：{msg}"
        );
    }

    /// 回滚路径要用它：表不存在只记一行，不算失败（spec §9.1）。
    /// 回滚路径要用它：表不存在只记一行，不算失败（spec §9.1）。
    #[test]
    fn delete_is_fine_when_the_table_is_already_gone() {
        let (_d, paths, h) = seeded();
        assert!(delete(&h, &paths).unwrap().contains("已删除"));
        assert_eq!(h.ops(), vec!["run:nft delete table inet bui".to_string()]);
        let (_d2, paths2, h2) = seeded();
        h2.with(|i| {
            i.scripted.push((
                "nft delete table inet bui".into(),
                CmdOut::failure(1, "Error: No such file or directory\n"),
            ));
        });
        let msg = delete(&h2, &paths2).unwrap();
        assert!(msg.contains("未删除"), "{msg}");
        // 没有 nft 的机器上也不算失败
        let msg = delete(&FakeHost::new(), &paths).unwrap();
        assert!(msg.contains("没有表可删"), "{msg}");
    }

    #[test]
    fn a_missing_state_file_is_a_readable_error() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths {
            base_dir: d.path().into(),
            certs_dir: d.path().join("certs"),
            bin_dir: d.path().join("bin"),
        };
        let h = FakeHost::new();
        h.with(|i| {
            i.which.insert("nft".into());
        });
        let err = apply(&h, &paths).unwrap_err().to_string();
        assert!(err.contains("读不到期望态"), "{err}");
    }
}
