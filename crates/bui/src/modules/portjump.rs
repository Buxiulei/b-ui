//! 「本实例」的端口跳跃孤儿 NAT 规则清理（iptables 的链 + nft 的表）：
//! `bui hy2-prestart <config>` 的全部逻辑，同时被**直连** hysteria 单元
//! （`hysteria-server`）的 `ExecStartPre=-` 与 watchdog 的自愈分支调用（4.1 起住宅换成
//! sing-box，它不建任何 NAT 规则、也就没有孤儿链可清，那一侧改挂 `bui nft apply`）。
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
//! ip6tables: Chain already exists」。这是 v3.5.14 那个老坑在 v4 复活，之后每次非正常终止
//! 都会再踩一遍。
//!
//! ## 两种后端
//!
//! hysteria 2.12 的后端选择在 `app/internal/firewall/firewall_linux.go`
//! （`setupUDPPortRedirectWithRunner`）：`HYSTERIA_FIREWALL_BACKEND` 认
//! `nftables`/`nft` 与 `iptables`/`ipt`，其余一律自动探测——**PATH 上有 `nft` 就走 nft**。
//! nft 后端建的是表而不是链：`hysteria_<sha256 前 8 位>`，ip 与 ip6 两族各一张
//! （`listen:` 钉了 IP 时只有对应那一族），表内两条链 `prerouting` / `output`，规则形如
//! `udp dport 41000-43999 redirect to :40000`（`listen:` 钉了 IPv6 地址时是
//! `dnat to [<ip>]:40000`）。
//!
//! 4.0.1 之前这里只清 iptables 两族，nft 一族不管（v3 的 `hy2-portjump-cleanup.sh` 两种
//! 后端都清，这是 v3→v4 回归）：tizi 走 nft 后端，实测残留一张 2 槽时期的
//! `ip6 hysteria_<hash> { udp dport 45500-50000 redirect to :40001 }`。nft 的 `add table`
//! 幂等，残留表不会像 iptables 的 `-N` 那样直接把内核打进崩溃循环，但 `add rule` 不幂等：
//! 崩溃重启会往老表里一轮一轮堆重复规则，槽位重切之后老规则还把新区间送去旧 base 端口。
//! 两种后端都清不冲突（各自幂等），所以 iptables 那两族照旧无条件清，nft 按后端探测加清。
//!
//! 实录的两条关键细节，决定了这里的判定方式：
//! ① **孤儿链本身可能是空的**，只剩 OUTPUT 里一条跳转规则（进程崩在 `-N` 之后、加 REDIRECT
//!    之前），所以不能只靠「链内有 REDIRECT 到本实例 base 端口」来认领；
//! ② `iptables` 与 `ip6tables` 的链名（hash 不同）与残留情况**各自独立**，两张表必须分别处理。
//!
//! ## 怎么判定「这条链是本实例的」
//!
//! hash 是 `sha256(<后端标识>|<listen IP>|<base 端口>|<跳跃区间>)` 的前 8 个 hex 字符
//! （上游 `shortHash`/`hashInput`，2026-09-15 对着 2.12.2 源码核实）：**同一份 listen 配置
//! 才得到同一个 hash**，槽位重切、端口改动都会换一个名字，所以孤儿的名字无从预测。判定因此
//! **不依赖** hash，照 v3 `hy2-portjump-cleanup.sh`（`server/core.sh:139-190`，v3.5.14 引入、
//! v3.5.16 补上空链）的行为学判据，双重定位、按实例唯一：
//!
//! - **完整孤儿**：链内规则 `-A HYSTERIA-PR-x … -j REDIRECT --to-ports <base>`，base 端口按实例唯一；
//! - **空链**：任意链（实录是 OUTPUT，PREROUTING 同理）上 `-A … --dport <start>:<end> -j
//!   HYSTERIA-PR-x`，跳跃区间按实例唯一（直连 20000-30000 / 住宅 41000-50000）。
//!
//! nft 一族同理、只是判据只剩第一条：表名里的 hash 不参与判定，`nft list table <族> <表名>`
//! 的正文里有 `redirect to :<base>` 或 `dnat to [<ip>]:<base>` 才算本实例的
//! （[`nft_redirects_to`]）。**绝不按 `hysteria_` 前缀整表删**——那会连同机别的 hysteria
//! 实例一起清掉。
//!
//! 判据都只认本实例的端口，**绝不碰另一实例的链 / 表**（v3.5.1 的「共享 cleanup 跨实例误删」
//! 就是反例）。找不到 `iptables`/`ip6tables`/`nft` 命令则跳过；任何一步失败只记一行说明，
//! 绝不阻塞启动（单元里的 `ExecStartPre=-` 前缀同样保证这一点）。
//!
//! 启动前本实例的链 / 表本来就不该存在（上一次正常退出时 hysteria 自己清掉了），所以
//! **命中即孤儿**，不必再问「这是不是我这次要建的那一张」。
//!
//! ## 运行中的实例：CLI 层的护栏（4.0.1）
//!
//! 「命中即孤儿」只在**启动前**成立。对一个**正在服役**的实例跑清理，判据照样命中——命中的
//! 却是它现役的那张表 / 那条链：清掉之后端口跳跃当即失效，且要等该实例下次重启才恢复
//! （hysteria 只在启动时建规则）。2026-09-15 主会话本想「手跑一次 prestart」做零影响验证，
//! 查过实现才发现这一点，改用重启实例。
//!
//! 所以 [`run`]（**只有** `bui hy2-prestart` 这一个 CLI 入口）在**手动**调用时先按配置路径
//! 推出单元名（[`unit_for_config`]），`ActiveState` 恰为 [`ACTIVE_STATE`] 就什么都不做、报
//! [`InstanceRunning`]（`main` 打印一行并退 2）；`--force` 照原样执行。
//!
//! ## 为什么要短路（这段不是多余代码，别删）
//!
//! 护栏防的是「人或 agent 手动在活实例上跑」，**不是**拦 systemd 自己的启动。而
//! `ExecStartPre=` 跑在 HY2 单元的**启动关键路径**上，在那里多问 systemd 一句是要付代价的：
//! [`crate::sys::real::RealHost::run`] 用的是 `Command::output()`、**没有超时**；单元里
//! `ExecStartPre=-` 那个 `-` 只忽略退出码、**救不了挂起**（真卡住的上界是 systemd 的
//! `DefaultTimeoutStartSec`，本机 90s）。护栏之前这里只碰 iptables / nft、从不与 systemd
//! 通信，加一次 `systemctl show` 等于给每次 HY2 启动新增一个依赖。
//!
//! 所以 [`run`] 收一个 `spawned_by_systemd`：为真（被 systemd 拉起）时**直接清理，一次
//! [`Host::unit_property`] 都不发**，只有手动调用才查 `ActiveState`。判据是环境里有没有
//! [`SYSTEMD_INVOCATION_ENV`]——2026-09-15 在 systemd 259 上实测 `ExecStartPre=` 进程确实有它
//! ——由 `main` 读出来**按参数传进来**（[`Host`] 没有环境变量入口、[`crate::sys::fake::FakeHost`]
//! 也注入不了 env，就地 `std::env::var` 会让这段逻辑不可测）。
//!
//! 两条**不受**护栏影响的路径（改动的全部风险都在这里，各有用例守住）：
//! - **systemd 的 `ExecStartPre=`**：`INVOCATION_ID` 把整个护栏短路掉，正常启动路径一步不变、
//!   也不多一次 systemd 往返。万一短路没生效（环境被清过），判定也只认**字面** `active`——
//!   systemd 跑 `ExecStartPre=` 时单元是 `activating`，于是 `activating` / `inactive` /
//!   `failed` / 查不到 / 查询失败一律放行（把 `activating` 也算进去就等于每次启动都清不了
//!   孤儿链，护栏自己制造出它要防的那个崩溃循环）；推不出单元名同样放行。这道第二保险照旧留着。
//! - **watchdog 自愈与对账**：它们直接调 [`cleanup`]，护栏只在 [`run`] 里，所以进程内调用点
//!   行为一字不变（崩溃循环中的实例在 systemd 眼里可能正是 `active`，自愈要的就是先清再重启）。

use crate::sys::Host;
use bui_schema::paths::Paths;
use std::collections::BTreeSet;
use std::path::Path;

/// Hysteria2 内置端口跳跃建的链名前缀。
pub const CHAIN_PREFIX: &str = "HYSTERIA-PR-";

/// 两张表分别处理：链名与残留情况互不相干（事故实录）。
pub const TABLES: [&str; 2] = ["iptables", "ip6tables"];

/// hysteria 选防火墙后端的环境变量（上游 `firewallBackendEnv`）。单元里的
/// `ExecStartPre=` 与内核本体同一份 `Environment=`，所以 `bui hy2-prestart` 读到的就是
/// 内核会用的那个后端。
pub const FIREWALL_BACKEND_ENV: &str = "HYSTERIA_FIREWALL_BACKEND";

/// systemd 给一次单元启动里的**每个**进程设的调用 ID（`ExecStartPre=` 进程与同一次启动的
/// `ExecStart` 进程同值）。2026-09-15 在两台生产机（systemd 259）上用 `systemd-run --wait
/// --collect` 起瞬态单元实测：`ExecStartPre=` 进程的环境里确实有它，值长 32。
/// `main` 读它、按参数传进 [`run`]（见模块文档「为什么要短路」）。
pub const SYSTEMD_INVOCATION_ENV: &str = "INVOCATION_ID";

/// nft 后端的表名前缀（上游 `"hysteria_" + shortHash(...)`）。
pub const NFT_TABLE_PREFIX: &str = "hysteria_";

/// hysteria 只在这两族建表（上游 `nftFamiliesForAddr`）：`inet` / `arp` / `bridge` 里
/// 同名的表不是它建的，一律不碰。
pub const NFT_FAMILIES: [&str; 2] = ["ip", "ip6"];

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

/// 该不该清 nft 表：照 hysteria 自己的后端选择（上游 `setupUDPPortRedirectWithRunner`）。
/// `HYSTERIA_FIREWALL_BACKEND` 钉死 iptables 时不碰 nft，其余（`nftables`/`nft`、认不出的值、
/// 没设）都落到「PATH 上有 `nft` 就走 nft」——机器上没有 `nft` 自然也没有表可清。
pub fn wants_nft(backend: Option<&str>, has_nft: bool) -> bool {
    match backend.unwrap_or_default().to_ascii_lowercase().as_str() {
        "iptables" | "ipt" => false,
        _ => has_nft,
    }
}

/// 从 `nft list tables` 的输出里挑出 hysteria 的表：每行 `table <族> <表名>`，
/// 只认 [`NFT_FAMILIES`] 两族里 [`NFT_TABLE_PREFIX`] 开头的表名，保持输出顺序。
pub fn nft_hysteria_tables(dump: &str) -> Vec<(String, String)> {
    dump.lines()
        .filter_map(|line| {
            let w: Vec<&str> = line.split_whitespace().collect();
            let ["table", family, name, ..] = w[..] else {
                return None;
            };
            (NFT_FAMILIES.contains(&family) && name.starts_with(NFT_TABLE_PREFIX))
                .then(|| (family.to_string(), name.to_string()))
        })
        .collect()
}

/// `nft list table <族> <表名>` 的正文里有没有「重定向到本实例 base 端口」的规则。
///
/// 两种形态（上游 `setupNFTablesRedirect`）：`udp dport <区间> redirect to :<base>`，
/// 以及 `listen:` 钉了 IPv6 地址时的 `… dnat to [<ip>]:<base>`。按**词**比对而不是子串：
/// `:4000` 是 `:40000` 的子串，子串匹配会把别的实例的表也删掉。
pub fn nft_redirects_to(dump: &str, base: u16) -> bool {
    let dest = format!(":{base}");
    let w: Vec<&str> = dump.split_whitespace().collect();
    w.windows(3).any(|t| match t {
        ["redirect", "to", d] => *d == dest,
        ["dnat", "to", d] => d.ends_with(&dest),
        _ => false,
    })
}

/// 清 nft 里属于本实例的孤儿表：`nft list tables` 枚举 → 逐张读正文按 base 端口认领 →
/// 整表 `nft delete table`。`nft` 不在则什么都不做；任何一步失败只记一行说明。
fn cleanup_nft(host: &dyn Host, listen: &Listen) -> Vec<String> {
    let mut done = Vec::new();
    if !host.which("nft") {
        return done;
    }
    let Ok(list) = host.run("nft", &["list", "tables"]) else {
        return done;
    };
    if !list.ok() {
        done.push(format!(
            "nft list tables 失败（跳过）：{}",
            list.stderr.trim()
        ));
        return done;
    }
    for (family, name) in nft_hysteria_tables(&list.stdout) {
        let Ok(table) = host.run("nft", &["list", "table", &family, &name]) else {
            continue;
        };
        if !table.ok() || !nft_redirects_to(&table.stdout, listen.base) {
            continue;
        }
        match host.run("nft", &["delete", "table", &family, &name]) {
            Ok(out) if out.ok() => {
                done.push(format!(
                    "已删除 nft 表 {family} {name}（本实例端口跳跃孤儿）"
                ));
            }
            Ok(out) => done.push(format!(
                "nft delete table {family} {name} 失败（跳过）：{}",
                out.stderr.trim()
            )),
            Err(e) => done.push(format!(
                "nft delete table {family} {name} 失败（跳过）：{e}"
            )),
        }
    }
    done
}

/// 按一份 hysteria 配置清掉**该实例**残留的端口跳跃孤儿：iptables 两族的链 + nft 两族的表，
/// 各自独立处理。返回做过的事（每行一条，进日志 / 自愈事件）；正常退出过的实例上是空
/// `Vec`（no-op）。
pub fn cleanup(host: &dyn Host, config: &Path) -> Vec<String> {
    let backend = std::env::var(FIREWALL_BACKEND_ENV).ok();
    cleanup_with(host, config, backend.as_deref())
}

/// [`cleanup`] 的本体；`backend` 是 [`FIREWALL_BACKEND_ENV`] 的值（`None` = 没设）。
/// 单测走这里，不动进程环境（`set_var` 会影响并行跑的其它用例）。
fn cleanup_with(host: &dyn Host, config: &Path, backend: Option<&str>) -> Vec<String> {
    let Ok(Some(bytes)) = host.read_file(config) else {
        return vec![format!("读不到 {}，跳过孤儿链清理", config.display())];
    };
    let Some(listen) = parse_listen(&String::from_utf8_lossy(&bytes)) else {
        return vec![format!(
            "{} 里没有可解析的 listen: 行，跳过孤儿链清理",
            config.display()
        )];
    };
    let mut done: Vec<String> = TABLES
        .iter()
        .flat_map(|ipt| cleanup_table(host, ipt, &listen))
        .collect();
    if wants_nft(backend, host.which("nft")) {
        done.extend(cleanup_nft(host, &listen));
    }
    done
}

/// [`cleanup_legacy_residential`] 每轮探测多少份 4.0 的按槽配置：槽 0..7 共
/// [`crate::reconcile::MAX_RESI_SLOTS`] 份（`config-residential.yaml` 与
/// `config-residential-1..7.yaml`）。与那条清理路径同生共死，见它的「下线判据」一节。
const RESI_CONFIGS_PER_ROUND: u16 = crate::reconcile::MAX_RESI_SLOTS;

/// 4.0 的按槽住宅实例（`hysteria-residential[-<i>]`，apernet）留下的端口跳跃 NAT 规则：
/// 对**每份仍在盘上**的 `config-residential[-<i>].yaml` 各调一次 [`cleanup`]。
///
/// 4.1 之后**没有任何别的路径再清它们**（住宅 prestart 换成 `bui nft apply`、
/// `crate::modules::watchdog::HY2_CONFIGS` 只留直连、`import_v3` 的前缀清理已在
/// `6dc2f90` 删掉），而残留的
/// `4xxxx-4yyyy → :4000i`（rick 是 iptables 链、tizi 是 nft 表）与我们的 `inet bui`
/// 同挂 nat priority **-100**：先注册者先做 NAT ⇒ 那一片跳跃端口被送到**已经没人监听**的
/// `:4000i`，正是 spec §1.1 里「每 30 秒一次的周期性静默丢包」的形状。
///
/// 调用点只有一个：[`crate::reconcile::apply`] 的第 7.5 步，**排在 `nft -f` 之前**
/// （也在停旧槽实例与删那些配置文件之前，所以 `listen:` 行还读得到），且**每轮无条件跑**、
/// 不挂在 `Change::ApplyNftTable` 上 —— 「4.1 落过表 → `--rollback` 回 4.0（不删表）→
/// 再升 4.1」这条路上哈希与表都没变、不产变更，挂在变更上就一次清理都不跑。
/// 配置已经不在盘上 = 上一轮已经清过 = 什么都不做，于是它对「二次对账零变更」无影响。
///
/// # 下线判据（T15 定，别让它永久留在热路径上）
///
/// 这是一条**纯 4.0 遗留清理路径**：稳态下每轮对账白跑 8 次 `read_file` 探测（零命令、
/// 零 note、零变更）。它的寿命与那七个遗留单元绑在一起 —— **`hysteria-residential-<i>.service`
/// 从 [`crate::reconcile::LEGACY_UNITS`] 里摘掉的那一刻，本函数、[`RESI_CONFIGS_PER_ROUND`]、
/// 它在 `reconcile::apply` 第 7.5 步的调用点与
/// [`tests::the_legacy_residential_cleanup_retires_with_the_legacy_units`] 一起删**。
/// 那张名单本身的下线条件是「全部在役机器都升过 4.1、`config-residential*.yaml` 与那些单元
/// 在任一机器上都不再出现」，与兼容段 `40001-40007` 的下线（连续 30 天零命中 + 提前 30 天
/// 通知，spec §2.4）各算各的 —— 兼容段管的是**客户端**手里的旧订阅，这里管的是**服务端**
/// 盘上的旧 NAT 规则。那条断言用例就是这个判据的机器可验形式：谁摘名单，它当场红。
pub fn cleanup_legacy_residential(host: &dyn Host, paths: &Paths) -> Vec<String> {
    let mut done = Vec::new();
    for i in 0..RESI_CONFIGS_PER_ROUND {
        let cfg = crate::modules::core_files::resi_config_path(paths, i);
        if host.read_file(&cfg).unwrap_or_default().is_none() {
            continue;
        }
        for line in cleanup(host, &cfg) {
            done.push(format!("{}：{line}", cfg.display()));
        }
    }
    done
}

/// 「这份配置的实例正在服役」——`bui hy2-prestart` 据此拒绝清理时的专属错误：
/// `main` 打印一行并以**退出码 2** 结束（与 `bui upgrade` 的降级守卫同一口径）。
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct InstanceRunning(pub String);

/// systemd 里「正在服役」的那**一个**字面值。只有它拒绝：`ExecStartPre=` 跑在
/// `activating` 上，多认一个状态就会拦下正常启动（见模块文档）。
pub const ACTIVE_STATE: &str = "active";

/// 配置路径 → 它那个受管单元名，来源只有 [`crate::modules::watchdog::HY2_CONFIGS`]
/// 那份 (单元, 配置) 表。
///
/// **4.1 起那张表只剩直连一项**，于是全部 `config-residential*.yaml` 都推不出单元名：
/// 带序号的实例进了 `LEGACY_UNITS`（`is_managed_unit` 对它们恒为 false），不带序号的那份
/// 配置随住宅换 sing-box 一起被对账删掉。所以原先那条「`config-residential-<i>.yaml` →
/// `hysteria-residential-<i>`」的分支已经不可达，一并收掉 —— 那些实例都停了，
/// 它们的孤儿规则由 [`cleanup_legacy_residential`] 在对账里直接清，护栏不必再认它们。
///
/// 认不出来 → `None`，调用方**放行**：推导失败绝不能成为拒绝启动的理由。
pub fn unit_for_config(config: &Path) -> Option<String> {
    let file = config.file_name()?.to_str()?;
    crate::modules::watchdog::HY2_CONFIGS
        .iter()
        .find(|(_, cfg)| *cfg == file)
        .map(|(unit, _)| (*unit).to_string())
}

/// 这份配置的实例此刻是不是 [`ACTIVE_STATE`]：是则返回单元名（调用方据此拒绝）。
/// 推不出单元名、`ActiveState` 查不到或查询失败 → `None`（放行，不知道不拦）。
fn active_unit(host: &dyn Host, config: &Path) -> Option<String> {
    let unit = unit_for_config(config)?;
    let state = host.unit_property(&unit, "ActiveState").ok().flatten()?;
    (state.trim() == ACTIVE_STATE).then_some(unit)
}

/// `bui hy2-prestart <config> [--force]`：直连 hysteria 单元（`hysteria-server`）的
/// `ExecStartPre=-`。
/// 清理本身**永远退 0**——失败绝不能阻塞内核启动（单元里的 `-` 前缀是第二道保险）。
///
/// 唯一的例外是护栏（模块文档「运行中的实例」）：该实例的 `ActiveState` 恰为
/// [`ACTIVE_STATE`] 而 `force` 为假时**一步清理都不做**，报 [`InstanceRunning`]。
///
/// `spawned_by_systemd`（`main` 按 [`SYSTEMD_INVOCATION_ENV`] 算出）为真时**整个护栏短路、
/// 一次 systemd 查询都不发**：启动关键路径上不加这个依赖（模块文档「为什么要短路」）。
pub fn run(
    host: &dyn Host,
    config: &Path,
    force: bool,
    spawned_by_systemd: bool,
) -> anyhow::Result<()> {
    // 被 systemd 拉起（`ExecStartPre=`）就直接清理，一次 systemd 查询都不发。
    if !force && !spawned_by_systemd {
        if let Some(unit) = active_unit(host, config) {
            return Err(InstanceRunning(format!(
                "{unit} 正在运行（active），此时清理会删掉它现役的端口跳跃规则：\
                 端口跳跃当即失效，且要等该实例下次重启才恢复。本命令是给单元启动前的 \
                 ExecStartPre 用的；只想让规则恢复请 systemctl restart {unit}，\
                 明知后果、确实要现在清，请加 --force"
            ))
            .into());
        }
    }
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
        assert!(run(
            &h,
            Path::new("/opt/b-ui/config-residential.yaml"),
            false,
            false
        )
        .is_ok());

        // 配置读不到 / 没有 listen 行：只记一行，不跑任何命令，退 0
        let h2 = FakeHost::new();
        h2.with(|i| {
            i.which.insert("iptables".into());
        });
        let done = cleanup(&h2, Path::new("/opt/b-ui/config.yaml"));
        assert_eq!(done.len(), 1);
        assert!(done[0].contains("读不到"));
        assert_eq!(h2.ops(), Vec::<String>::new());
        assert!(run(&h2, Path::new("/opt/b-ui/config.yaml"), false, false).is_ok());
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

    // ── nft 后端（tizi 走的就是它）────────────────────────────────────────────

    /// `nft list tables` 的真机形态：本实例（base 40000）两族各一张、另一个实例
    /// （base 40001）一张，外加与端口跳跃无关的族与表名。
    const NFT_TABLES: &str = "\
table ip filter
table inet firewalld
table ip hysteria_390d4d8b
table ip6 hysteria_390d4d8b
table ip hysteria_7c1e0f2a
table inet hysteria_deadbeef
";

    /// 本实例（住宅槽 0，base 40000）在 ip 族的表正文：`listen:` 没钉 IP ⇒ `redirect to`。
    const NFT_RESI_V4: &str = "\
table ip hysteria_390d4d8b {
	chain prerouting {
		type nat hook prerouting priority -100; policy accept;
		udp dport 41000-45499 redirect to :40000
	}
	chain output {
		type nat hook output priority -100; policy accept;
		udp dport 41000-45499 redirect to :40000
	}
}
";

    /// 同一个实例在 ip6 族的表正文：`listen:` 钉了 IPv6 地址时 hysteria 改用
    /// `dnat to [ip]:base`（上游 `setupNFTablesRedirect`）。
    const NFT_RESI_V6: &str = "\
table ip6 hysteria_390d4d8b {
	chain prerouting {
		type nat hook prerouting priority -100; policy accept;
		ip6 daddr 2001:db8::1 udp dport 41000-45499 dnat to [2001:db8::1]:40000
	}
}
";

    /// 另一个住宅实例（槽 1，base 40001）的表：一个删除命令都不许收到。
    const NFT_OTHER: &str = "\
table ip hysteria_7c1e0f2a {
	chain prerouting {
		type nat hook prerouting priority -100; policy accept;
		udp dport 45500-50000 redirect to :40001
	}
}
";

    fn host_with_nft() -> FakeHost {
        let h = FakeHost::new();
        h.with(|i| {
            i.which.insert("nft".into());
            i.scripted.push((
                "nft list tables".into(),
                crate::sys::CmdOut::success(NFT_TABLES),
            ));
            i.scripted.push((
                "nft list table ip hysteria_390d4d8b".into(),
                crate::sys::CmdOut::success(NFT_RESI_V4),
            ));
            i.scripted.push((
                "nft list table ip6 hysteria_390d4d8b".into(),
                crate::sys::CmdOut::success(NFT_RESI_V6),
            ));
            i.scripted.push((
                "nft list table ip hysteria_7c1e0f2a".into(),
                crate::sys::CmdOut::success(NFT_OTHER),
            ));
            i.files.insert(
                "/opt/b-ui/config-residential.yaml".into(),
                (resi_config().as_bytes().to_vec(), 0o600),
            );
        });
        h
    }

    /// 后端探测照 hysteria 自己那一份（`setupUDPPortRedirectWithRunner`）：环境变量优先，
    /// 认不出的值与没设一律自动探测——PATH 上有 `nft` 就是 nft 后端。
    #[test]
    fn the_backend_probe_matches_hysterias_own() {
        assert!(wants_nft(None, true), "没设环境变量 + 有 nft ⇒ nft 后端");
        assert!(!wants_nft(None, false));
        assert!(wants_nft(Some("nftables"), true));
        assert!(wants_nft(Some("NFT"), true), "hysteria 自己也大小写不敏感");
        assert!(
            !wants_nft(Some("iptables"), true),
            "钉死 iptables 就不碰 nft"
        );
        assert!(!wants_nft(Some("ipt"), true));
        assert!(wants_nft(Some("好家伙"), true), "认不出的值走自动探测");
        assert!(
            !wants_nft(Some("nft"), false),
            "机器上没有 nft 就没有表可清"
        );
    }

    /// `nft list tables` 每行是 `table <族> <表名>`：只认 hysteria 会建表的两族
    /// （`nftFamiliesForAddr` 只返回 ip / ip6），`inet` 里同名的表不是它建的。
    #[test]
    fn the_table_listing_keeps_only_hysteria_tables_in_ip_and_ip6() {
        assert_eq!(
            nft_hysteria_tables(NFT_TABLES),
            vec![
                ("ip".to_string(), "hysteria_390d4d8b".to_string()),
                ("ip6".to_string(), "hysteria_390d4d8b".to_string()),
                ("ip".to_string(), "hysteria_7c1e0f2a".to_string()),
            ]
        );
        assert_eq!(nft_hysteria_tables(""), Vec::new());
        assert_eq!(
            nft_hysteria_tables("garbage\ntable\ntable ip\n"),
            Vec::new()
        );
    }

    /// 表名里的 hash 不参与判定，按**表正文里的 base 端口**认领；比对按词而不是子串。
    #[test]
    fn a_table_is_claimed_by_this_instances_base_port_only() {
        assert!(nft_redirects_to(NFT_RESI_V4, 40000));
        assert!(
            nft_redirects_to(NFT_RESI_V6, 40000),
            "listen 钉了 IPv6 地址时是 dnat 形态"
        );
        assert!(nft_redirects_to(NFT_OTHER, 40001));
        assert!(!nft_redirects_to(NFT_OTHER, 40000), "别的实例的表不许命中");
        // `:4000` 是 `:40000` 的子串：子串匹配会把别人的表一起删掉
        assert!(!nft_redirects_to(NFT_RESI_V4, 4000));
        // 建完表就崩了的空表：没有规则可认，不动它（nft 的 `add table` 幂等，空表不致崩溃循环）
        assert!(!nft_redirects_to(
            "table ip hysteria_390d4d8b {\n}\n",
            40000
        ));
    }

    /// tizi 的真机残留形态：两族各自处理，孤儿表按 base 端口认领后整表删；
    /// 另一个实例（base 40001）的表只被读了一次、一个删除命令都没收到。
    #[test]
    fn cleanup_deletes_this_instances_nft_tables_in_both_families() {
        let h = host_with_nft();
        let done = cleanup_with(&h, Path::new("/opt/b-ui/config-residential.yaml"), None);
        assert_eq!(
            done,
            vec![
                "已删除 nft 表 ip hysteria_390d4d8b（本实例端口跳跃孤儿）",
                "已删除 nft 表 ip6 hysteria_390d4d8b（本实例端口跳跃孤儿）",
            ]
        );
        assert_eq!(
            h.ops(),
            vec![
                "run:nft list tables",
                "run:nft list table ip hysteria_390d4d8b",
                "run:nft delete table ip hysteria_390d4d8b",
                "run:nft list table ip6 hysteria_390d4d8b",
                "run:nft delete table ip6 hysteria_390d4d8b",
                "run:nft list table ip hysteria_7c1e0f2a",
            ]
        );
    }

    /// 机器上没有 `nft`（只有 iptables 的老内核）→ 一个 nft 命令都不发；
    /// 被 `HYSTERIA_FIREWALL_BACKEND=iptables` 钉在 iptables 后端时同样不碰 nft。
    #[test]
    fn a_machine_without_nft_never_runs_it() {
        let h = host_with_both_tables();
        let done = cleanup_with(&h, Path::new("/opt/b-ui/config-residential.yaml"), None);
        assert!(h.ops().iter().all(|o| !o.contains("nft")), "{:?}", h.ops());
        assert!(done.iter().all(|l| !l.contains("nft")), "{done:?}");

        let h2 = host_with_nft();
        assert_eq!(
            cleanup_with(
                &h2,
                Path::new("/opt/b-ui/config-residential.yaml"),
                Some("iptables")
            ),
            Vec::<String>::new()
        );
        assert_eq!(h2.ops(), Vec::<String>::new());
    }

    /// `nft list tables` 失败（没权限 / 内核没 nftables）：记一行说明就收手，不再发命令。
    #[test]
    fn a_failing_table_listing_is_reported_and_skipped() {
        let h = host_with_nft();
        h.with(|i| {
            i.scripted.insert(
                0,
                (
                    "nft list tables".into(),
                    crate::sys::CmdOut::failure(1, "Error: Operation not permitted"),
                ),
            );
        });
        let done = cleanup_with(&h, Path::new("/opt/b-ui/config-residential.yaml"), None);
        assert_eq!(done.len(), 1, "{done:?}");
        assert!(
            done[0].starts_with("nft list tables 失败（跳过）"),
            "{done:?}"
        );
        assert_eq!(h.ops(), vec!["run:nft list tables"]);
    }

    /// `nft delete table` 失败（表刚被别的进程删掉 / 没权限）：记一行说明，下一张照样处理，
    /// 绝不 panic、绝不阻塞启动。
    #[test]
    fn a_failing_nft_delete_is_reported_and_the_next_table_still_runs() {
        let h = host_with_nft();
        h.with(|i| {
            i.scripted.insert(
                0,
                (
                    "nft delete table ip hysteria_390d4d8b".into(),
                    crate::sys::CmdOut::failure(1, "Error: No such file or directory"),
                ),
            );
        });
        let done = cleanup_with(&h, Path::new("/opt/b-ui/config-residential.yaml"), None);
        assert_eq!(
            done,
            vec![
                "nft delete table ip hysteria_390d4d8b 失败（跳过）：Error: No such file or directory",
                "已删除 nft 表 ip6 hysteria_390d4d8b（本实例端口跳跃孤儿）",
            ]
        );
        // `bui hy2-prestart` 永远退 0
        assert!(run(
            &h,
            Path::new("/opt/b-ui/config-residential.yaml"),
            false,
            false
        )
        .is_ok());
    }

    /// v3 的直连实例（`listen: :10000,20000-30000`）与住宅实例（`:40000,41000-50000`）
    /// 留下的 iptables 孤儿链，两族同形。
    const V3_IPT: &str = "\
-P PREROUTING ACCEPT
-N HYSTERIA-PR-d1d1d1d1
-N HYSTERIA-PR-40404040
-A PREROUTING -p udp -m udp --dport 20000:30000 -j HYSTERIA-PR-d1d1d1d1
-A HYSTERIA-PR-d1d1d1d1 -p udp -j REDIRECT --to-ports 10000
-A PREROUTING -p udp -m udp --dport 41000:50000 -j HYSTERIA-PR-40404040
-A HYSTERIA-PR-40404040 -p udp -j REDIRECT --to-ports 40000
";

    const V3_NFT_TABLES: &str = "\
table ip hysteria_1a1a1a1a
table ip6 hysteria_2b2b2b2b
table ip hysteria_3c3c3c3c
table ip6 hysteria_4d4d4d4d
";

    /// v3 留下的端口跳跃孤儿只有两种形状：直连 `20000-30000 → :10000`、住宅
    /// `41000-50000 → :40000`，两族、两种后端。`bui_schema::v3::import` 读的就是 v3 自己那两行
    /// `listen:`，两个 base 端口原样进期望态（直连 = `ports.hy2`，住宅槽 0 = `ports.hy2_resi`），
    /// 所以两个 v4 实例各自的 prestart 按 base 判定就能全部认领。
    ///
    /// 这是 4.0.1 删掉 `import_v3::flush_v3_portjump_rules` 的依据：那一份按 `HYSTERIA-PR-` /
    /// `hysteria_` **前缀整表删**，会连同机别的 hysteria 实例一起清（v3.5.1 的老反例），
    /// 而本实例 prestart 的按 base 清理不但覆盖同样的形状，还每次启动都跑。
    #[test]
    fn every_v3_leftover_shape_is_claimed_by_the_matching_instance_prestart() {
        let h = FakeHost::new();
        h.with(|i| {
            for c in ["iptables", "ip6tables", "nft"] {
                i.which.insert(c.into());
            }
            for c in ["iptables", "ip6tables"] {
                i.scripted.push((
                    format!("{c} -t nat -S"),
                    crate::sys::CmdOut::success(V3_IPT),
                ));
            }
            i.scripted.push((
                "nft list tables".into(),
                crate::sys::CmdOut::success(V3_NFT_TABLES),
            ));
            for (family, table, hop, base) in [
                ("ip", "hysteria_1a1a1a1a", "20000-30000", 10000),
                ("ip6", "hysteria_2b2b2b2b", "20000-30000", 10000),
                ("ip", "hysteria_3c3c3c3c", "41000-50000", 40000),
                ("ip6", "hysteria_4d4d4d4d", "41000-50000", 40000),
            ] {
                i.scripted.push((
                    format!("nft list table {family} {table}"),
                    crate::sys::CmdOut::success(&format!(
                        "table {family} {table} {{\n\tchain prerouting {{\n\t\tudp dport {hop} \
                         redirect to :{base}\n\t}}\n}}\n"
                    )),
                ));
            }
            i.files.insert(
                "/opt/b-ui/config.yaml".into(),
                (b"listen: :10000,20000-30000\n".to_vec(), 0o600),
            );
            i.files.insert(
                "/opt/b-ui/config-residential.yaml".into(),
                (b"listen: :40000,41000-50000\n".to_vec(), 0o600),
            );
        });
        // 两个 v4 实例各自的 ExecStartPre
        for cfg in ["/opt/b-ui/config.yaml", "/opt/b-ui/config-residential.yaml"] {
            cleanup_with(&h, Path::new(cfg), None);
        }
        let ops = h.ops();
        for gone in [
            "run:iptables -t nat -X HYSTERIA-PR-d1d1d1d1",
            "run:iptables -t nat -X HYSTERIA-PR-40404040",
            "run:ip6tables -t nat -X HYSTERIA-PR-d1d1d1d1",
            "run:ip6tables -t nat -X HYSTERIA-PR-40404040",
            "run:nft delete table ip hysteria_1a1a1a1a",
            "run:nft delete table ip6 hysteria_2b2b2b2b",
            "run:nft delete table ip hysteria_3c3c3c3c",
            "run:nft delete table ip6 hysteria_4d4d4d4d",
        ] {
            assert!(ops.iter().any(|o| o == gone), "没清掉 {gone}：{ops:?}");
        }
    }

    // ── 运行中的实例：CLI 层的护栏（只在 `run` 里）────────────────────────────────

    /// 播种「这个单元此刻的 `ActiveState`」。`unit_props` 只认单元**全名**。
    fn set_state(h: &FakeHost, unit: &str, state: &str) {
        h.with(|i| {
            i.unit_props.insert(
                (format!("{unit}.service"), "ActiveState".into()),
                state.into(),
            );
        });
    }

    /// 已清掉这份 `listen:` 那条链的标志（本实例 base 40000）。
    const RESI_CLEANED: &str = "run:iptables -t nat -X HYSTERIA-PR-1111aaaa";

    /// 同 [`host_with_both_tables`]，但那份 `listen:` 行落在**直连**的 `config.yaml` 上：
    /// 4.1 起护栏（[`run`]）只认得出直连这一份配置的单元名（[`unit_for_config`]），
    /// 住宅那份已随 sing-box 化退役。端口值沿用同一份夹具 —— 护栏与端口值无关。
    fn host_for_the_guardrail() -> FakeHost {
        let h = host_with_both_tables();
        h.with(|i| {
            i.files.insert(
                "/opt/b-ui/config.yaml".into(),
                (resi_config().as_bytes().to_vec(), 0o600),
            );
        });
        h
    }

    /// 配置路径 → **受管**单元名：只认 watchdog 那份 (单元, 配置) 表里的名字，4.1 起
    /// 那张表只剩直连一项。于是**全部** `config-residential*.yaml` 都推不出单元名、
    /// 一律 `None` = 放行 —— 正是想要的：带序号的实例进了 `LEGACY_UNITS`、不带序号那份
    /// 配置已被对账删掉，它们的孤儿规则由 `cleanup_legacy_residential` 直接清，
    /// 而 4.1 的住宅单元（sing-box）根本不建 NAT 规则，没有可保护的现役规则。
    #[test]
    fn the_unit_name_is_derived_from_the_config_file_name() {
        assert_eq!(
            unit_for_config(Path::new("/opt/b-ui/config.yaml")).as_deref(),
            Some("hysteria-server")
        );
        // 推不出来 → None（放行）：不是 hysteria 的配置、4.0 的住宅配置（单元已退役或换内核）、
        // 非规范序号与越界序号都不认
        for cfg in [
            "/opt/b-ui/xray-config.json",
            "/opt/b-ui/singbox-relay.json",
            "/opt/b-ui/hy2-residential.json",
            "/opt/b-ui/config-residential.yaml",
            "/opt/b-ui/config-residential-3.yaml",
            "/opt/b-ui/config-residential-0.yaml",
            "/opt/b-ui/config-residential-01.yaml",
            "/opt/b-ui/config-residential-8.yaml",
            "/opt/b-ui/config-residential-x.yaml",
            "/opt/b-ui/config.yml",
            "/opt/b-ui",
        ] {
            assert_eq!(unit_for_config(Path::new(cfg)), None, "{cfg}");
        }
    }

    /// 护栏：实例正在服役时**一步清理都不做**（一个命令都没发出去），报的是 `main` 用来
    /// 给退出码 2 的那个类型，文案里有可操作的那半句。
    ///
    /// 这就是 2026-09-15 差点发生的事：手跑一次 prestart 会把它现役的那张表 / 那条链删掉，
    /// 端口跳跃当即失效、直到该实例下次重启才恢复。
    #[test]
    fn a_running_instance_is_refused_and_nothing_is_cleaned() {
        let h = host_for_the_guardrail();
        set_state(&h, "hysteria-server", "active");
        let err = run(&h, Path::new("/opt/b-ui/config.yaml"), false, false).unwrap_err();
        assert!(err.is::<InstanceRunning>(), "main 靠这个类型给退出码 2");
        let msg = err.to_string();
        assert!(msg.contains("正在运行"), "{msg}");
        assert!(msg.contains("请加 --force"), "{msg}");
        assert_eq!(
            h.ops(),
            Vec::<String>::new(),
            "拒绝时一个 iptables / nft 命令都不许发"
        );
    }

    /// `--force`：运维明知后果（实例卡在 failed 又想立刻清）时照原样执行。
    #[test]
    fn force_cleans_even_while_the_instance_is_running() {
        let h = host_for_the_guardrail();
        set_state(&h, "hysteria-server", "active");
        assert!(run(&h, Path::new("/opt/b-ui/config.yaml"), true, false).is_ok());
        assert!(h.ops().iter().any(|o| o == RESI_CLEANED), "{:?}", h.ops());
    }

    /// **正常启动路径一步不变**（本改动最大的风险）：systemd 执行 `ExecStartPre=` 时单元是
    /// `activating`，不是 `active`。所以除了字面 `active` 之外的每一种状态——含查不到与
    /// 查询失败——都必须照常清理。护栏一旦把 `activating` 也算进去，每次启动都清不了孤儿链，
    /// 就又变回它要防的那个崩溃循环（事故实录见模块文档）。
    ///
    /// systemd 那条路现在还多一层短路（`spawned_by_systemd`），这里守的是短路没生效时的
    /// **第二道保险**，所以照旧按手动调用（`spawned_by_systemd = false`）逐个状态跑。
    #[test]
    fn every_state_other_than_active_still_cleans() {
        for state in [
            Some("activating"), // systemd 跑 ExecStartPre 时就是这个
            Some("deactivating"),
            Some("inactive"),
            Some("failed"),
            Some("unknown"),
            Some(""), // `systemctl show` 给空值
            None,     // 查不到这个单元 / 查询失败
        ] {
            let h = host_for_the_guardrail();
            if let Some(s) = state {
                set_state(&h, "hysteria-server", s);
            }
            assert!(
                run(&h, Path::new("/opt/b-ui/config.yaml"), false, false).is_ok(),
                "{state:?}"
            );
            assert!(
                h.ops().iter().any(|o| o == RESI_CLEANED),
                "{state:?} 被护栏拦下了，正常启动路径会清不了孤儿链：{:?}",
                h.ops()
            );
        }
    }

    /// 推不出单元名（陌生的配置文件名）⇒ 放行，哪怕直连单元此刻正是 `active`：
    /// 推导失败绝不能成为拒绝启动的理由。
    #[test]
    fn a_config_whose_unit_cannot_be_derived_is_let_through() {
        let h = host_for_the_guardrail();
        set_state(&h, "hysteria-server", "active");
        h.with(|i| {
            i.files.insert(
                "/opt/b-ui/hy2-extra.yaml".into(),
                (resi_config().as_bytes().to_vec(), 0o600),
            );
        });
        assert!(run(&h, Path::new("/opt/b-ui/hy2-extra.yaml"), false, false).is_ok());
        assert!(h.ops().iter().any(|o| o == RESI_CLEANED), "{:?}", h.ops());
    }

    /// 护栏只在 CLI 入口层：watchdog 的自愈与对账直接调 [`cleanup`]，单元状态一概不看。
    /// 崩溃循环里的实例在 systemd 眼里可能正是 `active`（`Restart=always` 刚把它拉起来），
    /// 自愈要的就是「清掉本实例的表 / 链再重启」——这条路径行为一字不变。
    #[test]
    fn the_in_process_cleanup_ignores_the_unit_state() {
        let h = host_with_both_tables();
        set_state(&h, "hysteria-residential", "active");
        assert_eq!(
            cleanup(&h, Path::new("/opt/b-ui/config-residential.yaml")),
            vec![
                "已清理 iptables nat 链 HYSTERIA-PR-1111aaaa（本实例端口跳跃孤儿）",
                "已清理 ip6tables nat 链 HYSTERIA-PR-c66a02d9（本实例端口跳跃孤儿）",
            ]
        );
        assert!(h.ops().iter().any(|o| o == RESI_CLEANED), "{:?}", h.ops());
    }

    /// **被 systemd 拉起时一次 systemd 查询都不发**：`ExecStartPre=` 在 HY2 单元的启动关键
    /// 路径上，而 `RealHost::run` 用的 `Command::output()` 没有超时、单元里 `ExecStartPre=-`
    /// 的 `-` 只忽略退出码、救不了挂起（模块文档「为什么要短路」）。
    ///
    /// 怎么证明「没查」：单元播成 `active`——**查了就必定拒绝**，所以「照样清理」本身就是一层
    /// 证据；再直接断言 `unit_prop_reads()` 是空的（`unit_property` 是纯查询，不进 `ops`）。
    /// 后半段用同一份播种手动跑一次作**阳性对照**：证明这份流水确实会记账、该状态确实会拒绝，
    /// 否则「空流水」可能只是假机器不记账、断言恒绿。
    #[test]
    fn a_systemd_spawned_run_cleans_without_asking_systemd() {
        let h = host_for_the_guardrail();
        set_state(&h, "hysteria-server", "active");
        assert!(run(&h, Path::new("/opt/b-ui/config.yaml"), false, true).is_ok());
        assert!(h.ops().iter().any(|o| o == RESI_CLEANED), "{:?}", h.ops());
        assert_eq!(
            h.unit_prop_reads(),
            Vec::new(),
            "启动关键路径上一次 systemd 查询都不许发"
        );

        // 阳性对照：同一份播种，手动调用（环境里没有 INVOCATION_ID）
        let m = host_for_the_guardrail();
        set_state(&m, "hysteria-server", "active");
        assert!(run(&m, Path::new("/opt/b-ui/config.yaml"), false, false).is_err());
        assert_eq!(
            m.unit_prop_reads(),
            vec![(
                "hysteria-server.service".to_string(),
                "ActiveState".to_string()
            )],
            "手动调用恰好查一次 ActiveState"
        );
    }

    /// 4.0 两个槽（槽 0 = `config-residential.yaml`，槽 2 = `config-residential-2.yaml`）
    /// 各留下一条孤儿链。
    const TWO_SLOT_DUMP: &str = "\
-P PREROUTING ACCEPT
-N HYSTERIA-PR-5e0b13c4
-N HYSTERIA-PR-7c1e0f2a
-A PREROUTING -p udp -m udp --dport 41000:50000 -j HYSTERIA-PR-5e0b13c4
-A HYSTERIA-PR-5e0b13c4 -p udp -j REDIRECT --to-ports 40000
-A PREROUTING -p udp -m udp --dport 45500:50000 -j HYSTERIA-PR-7c1e0f2a
-A HYSTERIA-PR-7c1e0f2a -p udp -j REDIRECT --to-ports 40002
";

    /// 下线判据的机器可验形式（T15）：[`cleanup_legacy_residential`] 是纯 4.0 遗留清理
    /// 路径，稳态每轮白跑 8 次 `read_file`。它的寿命与那七个遗留单元绑死 ——
    /// 谁把 `hysteria-residential-<i>.service` 从 [`crate::reconcile::LEGACY_UNITS`]
    /// 里摘掉，这条用例当场红，提醒他把那条清理路径（函数本体 + `RESI_CONFIGS_PER_ROUND`
    /// + `reconcile::apply` 第 7.5 步的调用点 + 本用例）一起删掉。
    ///
    /// 这里只钉「常量定义没被人改小」；**份数由
    /// [`tests::the_legacy_cleanup_probes_every_one_of_the_eight_slots`] 按实际发出的
    /// dump 次数钉**（本条的等式按定义恒真，捕获不到有人改消费它的那个循环）。
    #[test]
    fn the_legacy_residential_cleanup_retires_with_the_legacy_units() {
        let legacy = crate::reconcile::LEGACY_UNITS;
        for i in 1..crate::reconcile::MAX_RESI_SLOTS {
            assert!(
                legacy.contains(&format!("hysteria-residential-{i}.service").as_str()),
                "hysteria-residential-{i}.service 不在 LEGACY_UNITS 了 ⇒ \
                 portjump::cleanup_legacy_residential 与它的调用点、本用例一起删掉"
            );
        }
        assert_eq!(
            RESI_CONFIGS_PER_ROUND,
            crate::reconcile::MAX_RESI_SLOTS,
            "常量定义被改小了 ⇒ 探测份数覆盖不到槽 0..7 全部 4.0 配置"
        );
    }

    /// **探测的份数真的是 8 份**（第九波复核点名：旁边那条「探 8 份」的断言是同义反复
    /// —— `RESI_CONFIGS_PER_ROUND` 按定义就 `= MAX_RESI_SLOTS`，它只能捕获「有人改常量
    /// 定义」，捕获不到「有人改消费它的那个循环」）。
    ///
    /// 这条把 8 份 4.0 配置全放上盘，按**实际发出的 dump 次数**数份数：循环上界少 1，
    /// 顶槽 `config-residential-7.yaml` 就永远不被探测、它留下的孤儿 NAT 规则永远不清 ——
    /// 那一片跳跃端口与 `inet bui` 同挂 nat priority -100，先注册者先做 NAT ⇒ 静默丢包，
    /// 与 2026-09-15 那次事故同类。
    #[test]
    fn the_legacy_cleanup_probes_every_one_of_the_eight_slots() {
        let slots = crate::reconcile::MAX_RESI_SLOTS;
        let h = FakeHost::new();
        // 每槽一份配置 + dump 里一条指向它 base 端口的孤儿链（链名按槽序号编，便于点名）
        let mut dump = String::from("-P PREROUTING ACCEPT\n");
        for i in 0..slots {
            dump.push_str(&format!(
                "-N HYSTERIA-PR-0000000{i}\n-A HYSTERIA-PR-0000000{i} -p udp -j REDIRECT \
                 --to-ports {}\n",
                40000 + i
            ));
        }
        h.with(|inv| {
            inv.which.insert("iptables".into());
            for i in 0..slots {
                inv.files.insert(
                    crate::modules::core_files::resi_config_path(&Paths::default_server(), i),
                    (
                        format!("listen: :{},41000-50000\n", 40000 + i).into_bytes(),
                        0o600,
                    ),
                );
            }
            inv.scripted.push((
                "iptables -t nat -S".into(),
                crate::sys::CmdOut::success(&dump),
            ));
        });

        let done = cleanup_legacy_residential(&h, &Paths::default_server());
        let ops = h.ops();
        assert_eq!(
            ops.iter()
                .filter(|o| *o == "run:iptables -t nat -S")
                .count(),
            usize::from(slots),
            "盘上 {slots} 份配置就得发 {slots} 次 dump，少一次就有一槽的孤儿规则没人清：{ops:?}"
        );
        // 顶槽那一份必须真的被清（份数少 1 时它是第一个掉出去的）
        let top = slots - 1;
        assert!(
            done.iter().any(|l| l
                .starts_with(&format!("/opt/b-ui/config-residential-{top}.yaml："))
                && l.contains(&format!("HYSTERIA-PR-0000000{top}"))),
            "顶槽（{top}）的孤儿链没被清：{done:?}"
        );
        assert!(
            ops.iter()
                .any(|o| *o == format!("run:iptables -t nat -X HYSTERIA-PR-0000000{top}")),
            "{ops:?}"
        );
    }

    /// 枚举**从槽 0 开始**：`config-residential.yaml`（单槽 4.0 机器唯一有的那份）
    /// 与 `config-residential-<i>.yaml` 一视同仁，盘上没有的那几格一个命令都不发。
    #[test]
    fn the_legacy_cleanup_covers_slot_zero_as_well_as_the_numbered_slots() {
        let h = FakeHost::new();
        h.with(|i| {
            i.which.insert("iptables".into());
            i.files.insert(
                "/opt/b-ui/config-residential.yaml".into(),
                (b"listen: :40000,41000-50000\n".to_vec(), 0o600),
            );
            i.files.insert(
                "/opt/b-ui/config-residential-2.yaml".into(),
                (b"listen: :40002,45500-50000\n".to_vec(), 0o600),
            );
            i.scripted.push((
                "iptables -t nat -S".into(),
                crate::sys::CmdOut::success(TWO_SLOT_DUMP),
            ));
        });
        let done = cleanup_legacy_residential(&h, &Paths::default_server());
        assert!(
            done.iter()
                .any(|l| l.starts_with("/opt/b-ui/config-residential.yaml：")
                    && l.contains("HYSTERIA-PR-5e0b13c4")),
            "槽 0 那份必须也清：{done:?}"
        );
        assert!(
            done.iter()
                .any(|l| l.starts_with("/opt/b-ui/config-residential-2.yaml：")
                    && l.contains("HYSTERIA-PR-7c1e0f2a")),
            "槽 2 那份必须也清：{done:?}"
        );
        let ops = h.ops();
        assert!(
            ops.iter()
                .any(|o| o == "run:iptables -t nat -X HYSTERIA-PR-5e0b13c4"),
            "{ops:?}"
        );
        assert!(
            ops.iter()
                .any(|o| o == "run:iptables -t nat -X HYSTERIA-PR-7c1e0f2a"),
            "{ops:?}"
        );
        // 盘上只有两份配置 ⇒ 只跑两次 dump，其余六格一个命令都不发
        assert_eq!(
            ops.iter()
                .filter(|o| *o == "run:iptables -t nat -S")
                .count(),
            2,
            "{ops:?}"
        );
    }

    /// `--force` 无条件清理，与是不是 systemd 拉起的无关。
    #[test]
    fn force_cleans_under_both_spawn_paths() {
        for spawned in [false, true] {
            let h = host_for_the_guardrail();
            set_state(&h, "hysteria-server", "active");
            assert!(
                run(&h, Path::new("/opt/b-ui/config.yaml"), true, spawned).is_ok(),
                "{spawned}"
            );
            assert!(
                h.ops().iter().any(|o| o == RESI_CLEANED),
                "{spawned}: {:?}",
                h.ops()
            );
        }
    }
}
