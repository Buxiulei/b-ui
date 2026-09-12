//! 「本实例」的端口跳跃孤儿 NAT 链清理：`bui hy2-prestart <config>` 的全部逻辑，
//! 同时被两个 hysteria 单元的 `ExecStartPre=-` 与 watchdog 的自愈分支调用。
//!
//! ## 为什么需要它
//!
//! Hysteria2 的**内置**端口跳跃（`listen: :40000,41000-50000`）在启动时自己往
//! `iptables`/`ip6tables` 的 `nat` 表里建一条 `HYSTERIA-PR-<hash>` 链，并在
//! PREROUTING 与 OUTPUT 上各加一条按跳跃区间的跳转规则（`--dport 41000:50000 -j
//! HYSTERIA-PR-<hash>`），链内是 `REDIRECT --to-ports <base>`。正常 SIGTERM 退出时它自己清掉；
//! **被 SIGKILL / OOM / 超时杀掉则链残留**，下一次启动 `-N` 同名链直接
//! `exit status 1: ip6tables: Chain already exists` → Hysteria2 判为 `invalid config: listen:`
//! 而 FATAL 退出 → `Restart=always` 把它变成崩溃循环。
//!
//! 真机事故（bwg-rick，2026-09-12 20:33 UTC，M5 回滚演练 `bui upgrade --rollback` 之后）：
//! `hysteria-residential` 连续崩 52 次，日志正是
//! 「invalid config: listen: ip6tables [-w -t nat -N HYSTERIA-PR-c66a02d9]: exit status 1:
//! ip6tables: Chain already exists」。这是 v3.5.14 那个老坑在 v4 复活——v4 只在 `import-v3`
//! 的卸载末尾清过一次（[`crate::commands::import_v3::flush_v3_portjump_rules`]），
//! 之后每次非正常终止都会再踩一遍。
//!
//! 实录的两条关键细节，决定了这里的判定方式：
//! ① **孤儿链本身可能是空的**，只剩 OUTPUT 里一条跳转规则（进程崩在 `-N` 之后、加 REDIRECT
//!    之前），所以不能只靠「链内有 REDIRECT 到本实例 base 端口」来认领；
//! ② `iptables` 与 `ip6tables` 的链名（hash 不同）与残留情况**各自独立**，两张表必须分别处理。
//!
//! ## 怎么判定「这条链是本实例的」
//!
//! 上游 `apernet/hysteria` 的源码在本机与本任务的离线环境里都取不到（Go 项目，不在 cargo
//! registry），`HYSTERIA-PR-<hash>` 里 hash 的算法因此**未经证实**（事故现场的
//! `c66a02d9` 是 8 个 hex 字符，形态上像 listen 字符串的截断哈希）。所以判定**不依赖**
//! hash，照 v3 `hy2-portjump-cleanup.sh`（`server/core.sh:139-190`，v3.5.14 引入、v3.5.16
//! 补上空链）的行为学判据，双重定位、按实例唯一：
//!
//! - **完整孤儿**：链内规则 `-A HYSTERIA-PR-x … -j REDIRECT --to-ports <base>`，base 端口按实例唯一；
//! - **空链**：任意链（实录是 OUTPUT，PREROUTING 同理）上 `-A … --dport <start>:<end> -j
//!   HYSTERIA-PR-x`，跳跃区间按实例唯一（直连 20000-30000 / 住宅 41000-50000）。
//!
//! 两条判据都只认本实例的端口，**绝不碰另一实例的链**（v3.5.1 的「共享 cleanup 跨实例误删」
//! 就是反例）。找不到 `iptables`/`ip6tables` 命令则跳过；任何一步失败只记一行说明，
//! 绝不阻塞启动（单元里的 `ExecStartPre=-` 前缀同样保证这一点）。

use crate::sys::Host;
use std::collections::BTreeSet;
use std::path::Path;

/// Hysteria2 内置端口跳跃建的链名前缀。
pub const CHAIN_PREFIX: &str = "HYSTERIA-PR-";

/// 两张表分别处理：链名与残留情况互不相干（事故实录）。
pub const TABLES: [&str; 2] = ["iptables", "ip6tables"];

/// 一份 `listen:` 行解析出的本实例端口。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Listen {
    /// `listen: :40000,41000-50000` 里的 `40000`（REDIRECT 的目标端口）
    pub base: u16,
    /// 跳跃区间 `41000-50000`；没有区间就是 `None`
    pub hop: Option<(u16, u16)>,
}

/// 从 hysteria 配置正文里取 `listen:` 行。
///
/// 手写的 YAML 可能是 `listen: "0.0.0.0:40000,41000-50000"`，所以按「最后一个 `:` 之后是
/// base 端口」解析，并去掉两侧引号。解析不出端口 → `None`（调用方据此什么都不做）。
pub fn parse_listen(text: &str) -> Option<Listen> {
    let raw = text.lines().find_map(|l| l.strip_prefix("listen:"))?;
    let raw = raw.trim().trim_matches('"').trim_matches('\'');
    let (addr, hop) = match raw.split_once(',') {
        Some((a, h)) => (a, parse_range(h)),
        None => (raw, None),
    };
    let base = addr.rsplit(':').next()?.trim().parse().ok()?;
    Some(Listen { base, hop })
}

/// `41000-50000` / `41000:50000` → `(41000, 50000)`。
fn parse_range(s: &str) -> Option<(u16, u16)> {
    let s = s.trim();
    let (a, b) = s.split_once('-').or_else(|| s.split_once(':'))?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

/// 从 `iptables -t nat -S` 的整份输出里挑出**本实例**的 `HYSTERIA-PR-*` 链名。
///
/// 两条判据见模块文档：链内 `--to-ports <base>`，或任意链（PREROUTING / OUTPUT）上
/// 按本实例跳跃区间的 `-j HYSTERIA-PR-*` 跳转（覆盖「空链只剩一条跳转」的实录形态）。
pub fn instance_chains(dump: &str, listen: &Listen) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in dump.lines() {
        let Some(rest) = line.strip_prefix("-A ") else {
            continue;
        };
        let w: Vec<&str> = rest.split_whitespace().collect();
        // ① 完整孤儿：链自己的 REDIRECT 指向本实例 base 端口
        if let Some(chain) = w.first().filter(|c| c.starts_with(CHAIN_PREFIX)) {
            if flag_value(&w, "--to-ports").and_then(|v| v.split('-').next()?.parse().ok())
                == Some(listen.base)
            {
                out.insert((*chain).to_string());
            }
        }
        // ② 空链：只剩一条按本实例区间的跳转（实录是 OUTPUT，PREROUTING 同理）
        if let (Some(hop), Some(target)) = (listen.hop, jump_target(&w)) {
            let dport = flag_value(&w, "--dport")
                .or_else(|| flag_value(&w, "--dports"))
                .and_then(parse_range);
            if dport == Some(hop) {
                out.insert(target);
            }
        }
    }
    out
}

/// `-j HYSTERIA-PR-x` 结尾的跳转目标（`-A PREROUTING -p udp -m udp --dport … -j <chain>`）。
/// 只认**行尾**的 `-j <chain>`，链内的 `-j REDIRECT --to-ports …` 因此不会被误判成跳转。
fn jump_target(words: &[&str]) -> Option<String> {
    let [.., "-j", chain] = words else {
        return None;
    };
    chain
        .starts_with(CHAIN_PREFIX)
        .then(|| (*chain).to_string())
}

/// 取 `--flag <value>` 的值。
fn flag_value<'a>(words: &[&'a str], flag: &str) -> Option<&'a str> {
    words
        .iter()
        .position(|w| *w == flag)
        .and_then(|i| words.get(i + 1))
        .copied()
}

/// `-A PREROUTING …` → `-D PREROUTING …`：跳到 `chain` 的那些跳转规则，行尾必须是 `-j <chain>`。
pub fn jump_rules<'a>(dump: &'a str, chain: &str) -> Vec<&'a str> {
    let suffix = format!(" -j {chain}");
    dump.lines()
        .filter(|l| l.starts_with("-A ") && l.ends_with(&suffix))
        .collect()
}

/// 清一张表（`iptables` 或 `ip6tables`）里属于本实例的孤儿链。
/// 命令不存在 / dump 取不到 → 空 `Vec`（不算错误）。
fn cleanup_table(host: &dyn Host, ipt: &str, listen: &Listen) -> Vec<String> {
    let mut done = Vec::new();
    if !host.which(ipt) {
        return done;
    }
    let Ok(dump) = host.run(ipt, &["-t", "nat", "-S"]) else {
        return done;
    };
    if !dump.ok() {
        done.push(format!(
            "{ipt} -t nat -S 失败（跳过）：{}",
            dump.stderr.trim()
        ));
        return done;
    }
    for chain in instance_chains(&dump.stdout, listen) {
        // 先删跳转（引用还在时 `-X` 必定 EBUSY），再清空、删链
        for rule in jump_rules(&dump.stdout, &chain) {
            let rule = rule.replacen("-A ", "-D ", 1);
            let mut args = vec!["-t", "nat"];
            args.extend(rule.split_whitespace());
            let _ = host.run(ipt, &args);
        }
        let _ = host.run(ipt, &["-t", "nat", "-F", &chain]);
        let _ = host.run(ipt, &["-t", "nat", "-X", &chain]);
        done.push(format!("已清理 {ipt} nat 链 {chain}（本实例端口跳跃孤儿）"));
    }
    done
}

/// 按一份 hysteria 配置清掉**该实例**残留的端口跳跃孤儿链，两张表分别处理。
/// 返回做过的事（每行一条，进日志 / 自愈事件）；正常退出过的实例上是空 `Vec`（no-op）。
pub fn cleanup(host: &dyn Host, config: &Path) -> Vec<String> {
    let Ok(Some(bytes)) = host.read_file(config) else {
        return vec![format!("读不到 {}，跳过孤儿链清理", config.display())];
    };
    let Some(listen) = parse_listen(&String::from_utf8_lossy(&bytes)) else {
        return vec![format!(
            "{} 里没有可解析的 listen: 行，跳过孤儿链清理",
            config.display()
        )];
    };
    TABLES
        .iter()
        .flat_map(|ipt| cleanup_table(host, ipt, &listen))
        .collect()
}

/// `bui hy2-prestart <config>`：两个 hysteria 单元的 `ExecStartPre=-`。
/// **永远退 0**——清理失败绝不能阻塞内核启动（单元里的 `-` 前缀是第二道保险）。
pub fn run(host: &dyn Host, config: &Path) -> anyhow::Result<()> {
    for line in cleanup(host, config) {
        tracing::info!("{line}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::fake::FakeHost;
    use pretty_assertions::assert_eq;

    /// 住宅实例的真机形态（事故现场）：`ip6tables` 里孤儿链**是空的**，只剩 OUTPUT
    /// 一条跳转；`iptables` 里是完整孤儿（链名与 v6 不同）。
    const V4_DUMP: &str = "\
-P PREROUTING ACCEPT
-N HYSTERIA-PR-1111aaaa
-N HYSTERIA-PR-9999zzzz
-A PREROUTING -p udp -m udp --dport 41000:50000 -j HYSTERIA-PR-1111aaaa
-A OUTPUT -p udp -m udp --dport 41000:50000 -j HYSTERIA-PR-1111aaaa
-A HYSTERIA-PR-1111aaaa -p udp -j REDIRECT --to-ports 40000
-A PREROUTING -p udp -m udp --dport 20000:30000 -j HYSTERIA-PR-9999zzzz
-A HYSTERIA-PR-9999zzzz -p udp -j REDIRECT --to-ports 10000
";

    const V6_DUMP: &str = "\
-P OUTPUT ACCEPT
-N HYSTERIA-PR-c66a02d9
-A OUTPUT -p udp -m udp --dport 41000:50000 -j HYSTERIA-PR-c66a02d9
";

    fn resi_config() -> &'static str {
        "listen: :40000,41000-50000\ntls:\n  cert: /opt/b-ui/certs/fullchain.pem\n"
    }

    #[test]
    fn parses_the_listen_line_in_its_three_shapes() {
        assert_eq!(
            parse_listen(resi_config()),
            Some(Listen {
                base: 40000,
                hop: Some((41000, 50000))
            })
        );
        assert_eq!(
            parse_listen("listen: \"0.0.0.0:10000,20000-30000\"\n"),
            Some(Listen {
                base: 10000,
                hop: Some((20000, 30000))
            })
        );
        // 关掉端口跳跃的直连实例：只有 base 端口
        assert_eq!(
            parse_listen("listen: :10000\n"),
            Some(Listen {
                base: 10000,
                hop: None
            })
        );
        // 没有 listen 行 / 值不是端口 → None（调用方什么都不做）
        assert_eq!(parse_listen("tls:\n  cert: x\n"), None);
        assert_eq!(parse_listen("listen: :abc\n"), None);
    }

    #[test]
    fn finds_the_full_orphan_and_the_empty_chain_of_this_instance_only() {
        let resi = Listen {
            base: 40000,
            hop: Some((41000, 50000)),
        };
        assert_eq!(
            instance_chains(V4_DUMP, &resi),
            ["HYSTERIA-PR-1111aaaa".to_string()].into_iter().collect(),
            "住宅实例只认自己那条，直连的 9999zzzz 不许碰"
        );
        // 事故形态：链是空的，只剩 OUTPUT 一条跳转 —— 仅靠 --to-ports 判定会漏掉它
        assert_eq!(
            instance_chains(V6_DUMP, &resi),
            ["HYSTERIA-PR-c66a02d9".to_string()].into_iter().collect()
        );
        let direct = Listen {
            base: 10000,
            hop: Some((20000, 30000)),
        };
        assert_eq!(
            instance_chains(V4_DUMP, &direct),
            ["HYSTERIA-PR-9999zzzz".to_string()].into_iter().collect()
        );
        // 直连关掉端口跳跃后只剩 base 判据，仍然只认自己那条
        assert_eq!(
            instance_chains(
                V4_DUMP,
                &Listen {
                    base: 10000,
                    hop: None
                }
            ),
            ["HYSTERIA-PR-9999zzzz".to_string()].into_iter().collect()
        );
    }

    /// 链内的 `-j REDIRECT --to-ports 40000` 不是「跳转到本链」，不能被 `jump_rules` 捞进去
    /// （捞进去就会 `-D HYSTERIA-PR-x -p udp -j REDIRECT`，等于白删一次）。
    #[test]
    fn jump_rules_only_matches_the_trailing_jump_to_that_chain() {
        assert_eq!(
            jump_rules(V4_DUMP, "HYSTERIA-PR-1111aaaa"),
            vec![
                "-A PREROUTING -p udp -m udp --dport 41000:50000 -j HYSTERIA-PR-1111aaaa",
                "-A OUTPUT -p udp -m udp --dport 41000:50000 -j HYSTERIA-PR-1111aaaa",
            ]
        );
        assert_eq!(
            jump_rules(V4_DUMP, "HYSTERIA-PR-9999zzzz"),
            vec!["-A PREROUTING -p udp -m udp --dport 20000:30000 -j HYSTERIA-PR-9999zzzz"]
        );
    }

    fn host_with_both_tables() -> FakeHost {
        let h = FakeHost::new();
        h.with(|i| {
            i.which.insert("iptables".into());
            i.which.insert("ip6tables".into());
            i.scripted.push((
                "iptables -t nat -S".into(),
                crate::sys::CmdOut::success(V4_DUMP),
            ));
            i.scripted.push((
                "ip6tables -t nat -S".into(),
                crate::sys::CmdOut::success(V6_DUMP),
            ));
            i.files.insert(
                "/opt/b-ui/config-residential.yaml".into(),
                (resi_config().as_bytes().to_vec(), 0o600),
            );
        });
        h
    }

    /// 两张表各自独立：v4 是完整孤儿（两条跳转 + 链内 REDIRECT），v6 是空链（一条跳转），
    /// 链名也不同。顺序必须是「先删跳转，再 -F，再 -X」。
    #[test]
    fn cleanup_handles_both_tables_and_deletes_jumps_before_flushing() {
        let h = host_with_both_tables();
        let done = cleanup(&h, Path::new("/opt/b-ui/config-residential.yaml"));
        assert_eq!(
            done,
            vec![
                "已清理 iptables nat 链 HYSTERIA-PR-1111aaaa（本实例端口跳跃孤儿）",
                "已清理 ip6tables nat 链 HYSTERIA-PR-c66a02d9（本实例端口跳跃孤儿）",
            ]
        );
        assert_eq!(
            h.ops(),
            vec![
                "run:iptables -t nat -S",
                "run:iptables -t nat -D PREROUTING -p udp -m udp --dport 41000:50000 -j HYSTERIA-PR-1111aaaa",
                "run:iptables -t nat -D OUTPUT -p udp -m udp --dport 41000:50000 -j HYSTERIA-PR-1111aaaa",
                "run:iptables -t nat -F HYSTERIA-PR-1111aaaa",
                "run:iptables -t nat -X HYSTERIA-PR-1111aaaa",
                "run:ip6tables -t nat -S",
                "run:ip6tables -t nat -D OUTPUT -p udp -m udp --dport 41000:50000 -j HYSTERIA-PR-c66a02d9",
                "run:ip6tables -t nat -F HYSTERIA-PR-c66a02d9",
                "run:ip6tables -t nat -X HYSTERIA-PR-c66a02d9",
            ]
        );
    }

    /// 直连实例跑清理时，住宅那条链（另一实例，可能正在服役）一个命令都不许碰。
    #[test]
    fn the_other_instances_chain_is_never_touched() {
        let h = host_with_both_tables();
        h.with(|i| {
            i.files.insert(
                "/opt/b-ui/config.yaml".into(),
                (b"listen: :10000,20000-30000\n".to_vec(), 0o600),
            );
        });
        cleanup(&h, Path::new("/opt/b-ui/config.yaml"));
        assert!(
            h.ops()
                .iter()
                .all(|o| !o.contains("HYSTERIA-PR-1111aaaa") && !o.contains("c66a02d9")),
            "{:?}",
            h.ops()
        );
        assert!(h
            .ops()
            .iter()
            .any(|o| o == "run:iptables -t nat -X HYSTERIA-PR-9999zzzz"));
    }

    #[test]
    fn missing_iptables_or_config_is_a_no_op_that_never_fails() {
        // 命令都不存在（容器 / 精简镜像）
        let h = FakeHost::new();
        h.with(|i| {
            i.files.insert(
                "/opt/b-ui/config-residential.yaml".into(),
                (resi_config().as_bytes().to_vec(), 0o600),
            );
        });
        assert_eq!(
            cleanup(&h, Path::new("/opt/b-ui/config-residential.yaml")),
            Vec::<String>::new()
        );
        assert_eq!(h.ops(), Vec::<String>::new());
        assert!(run(&h, Path::new("/opt/b-ui/config-residential.yaml")).is_ok());

        // 配置读不到 / 没有 listen 行：只记一行，不跑任何命令，退 0
        let h2 = FakeHost::new();
        h2.with(|i| {
            i.which.insert("iptables".into());
        });
        let done = cleanup(&h2, Path::new("/opt/b-ui/config.yaml"));
        assert_eq!(done.len(), 1);
        assert!(done[0].contains("读不到"));
        assert_eq!(h2.ops(), Vec::<String>::new());
        assert!(run(&h2, Path::new("/opt/b-ui/config.yaml")).is_ok());
    }

    /// 没有孤儿的常态（正常 SIGTERM 退出后 hysteria 自己清干净了）：只读一次 dump，
    /// 不发任何删除命令。
    #[test]
    fn a_clean_machine_only_reads_the_dump() {
        let h = FakeHost::new();
        h.with(|i| {
            i.which.insert("iptables".into());
            i.scripted.push((
                "iptables -t nat -S".into(),
                crate::sys::CmdOut::success("-P PREROUTING ACCEPT\n"),
            ));
            i.files.insert(
                "/opt/b-ui/config-residential.yaml".into(),
                (resi_config().as_bytes().to_vec(), 0o600),
            );
        });
        assert_eq!(
            cleanup(&h, Path::new("/opt/b-ui/config-residential.yaml")),
            Vec::<String>::new()
        );
        assert_eq!(h.ops(), vec!["run:iptables -t nat -S"]);
    }

    /// `iptables -S` 本身失败（内核没 nat 表 / 没权限）：记一行说明，不再发删除命令。
    #[test]
    fn a_failing_dump_is_reported_and_skipped() {
        let h = FakeHost::new();
        h.with(|i| {
            i.which.insert("iptables".into());
            i.scripted.push((
                "iptables -t nat -S".into(),
                crate::sys::CmdOut::failure(3, "can't initialize iptables table `nat'"),
            ));
            i.files.insert(
                "/opt/b-ui/config-residential.yaml".into(),
                (resi_config().as_bytes().to_vec(), 0o600),
            );
        });
        let done = cleanup(&h, Path::new("/opt/b-ui/config-residential.yaml"));
        assert_eq!(done.len(), 1);
        assert!(
            done[0].starts_with("iptables -t nat -S 失败（跳过）"),
            "{done:?}"
        );
        assert_eq!(h.ops(), vec!["run:iptables -t nat -S"]);
    }
}
