//! 住宅 HY2 端口跳跃的 nft 规则集（`table inet bui`）的**唯一**实现。
//!
//! 4.1 起住宅 HY2 只剩一个监听端口 `ports.hy2_resi`（`:40000`），整段
//! `ports.hy2_resi_hop`（`41000-50000`）与 4.0 的兼容段（`40001-40007`）由 b-ui 自己管的
//! 这张表 REDIRECT 到它——跳跃不再由内核自己建 NAT 规则，所以也不再有
//! 「每槽一个 hysteria 进程、跳跃段按槽位空间等分」带来的跨进程静默丢包。
//! 产出可以直接喂 `nft -f -`：先 `table` 声明（表不存在时 `flush` 会让整份事务失败）、
//! 再 `flush table`、最后一份完整定义，一次事务原子替换，重放幂等。
//!
//! 三件事值得写在这里：
//!
//! 1. **族用 `inet`，一张表同时覆盖 v4 与 v6；代价是把内核下限钉在 Linux ≥ 5.2。**
//!    apernet hysteria 的内置跳跃在 ip 与 ip6 两族各建一张 `hysteria_<hash>` 表
//!    （`crates/bui/src/modules/portjump.rs:128` 的 `NFT_FAMILIES`），两张表的 hash 与残留
//!    情况互不相干；`inet` 省掉这份对称的重复。但 `inet` 族的 `type nat` 链是内核 5.2 才有的
//!    （`man nft` NAT STATEMENTS：「When used in the inet family (available with kernel 5.2),
//!    the dnat and snat statements …」），更低的内核会把整份事务拒掉（`Error: Chain of type
//!    "nat" is not supported, perhaps kernel support is missing?`），跳跃段与兼容段双双不通。
//!    硬下限是 Linux ≥ 5.2；受支持目标（Ubuntu 22.04+ / Debian 12 / CentOS Stream 9）默认内核
//!    最低是 CentOS Stream 9 的 5.14（nft-distros 实测表：5.15 / 6.8 / 6.1 / 5.14，均 ≥ 5.2），
//!    低于 5.2 的只有已 EOL 的 CentOS 7（3.10）与 Debian 10（4.19），所以 4.1 直接钉这道下限，**不**退回
//!    `ip` + `ip6` 双表（双表会让规则数翻倍，`rule_count` 与自检 / watchdog 的判据随之分叉）；
//!    落地路径负责 `nft -c -f` 预检与内核版本核对，失败时显式报错而不是让跳跃静默消失
//!    （2026-09-16 裁决）。
//! 2. **每条规则都带 `counter`，但它是瞬时流计数，不是兼容段的下线判据本体。**
//!    `flush table` 连具名 counter 对象一起清零，所以开机、`bui nft apply`、每轮对账、
//!    watchdog 每 60 秒那处自愈重放、改端口——每次重放都让计数从 0 重新开始；
//!    nat 链的 counter 又只计每条 conntrack 流的首包
//!    （`man nft`：「Only the first packet of a connection …」），量纲是流数而非包数。判据因此
//!    归持久层：守护进程在每次重放**之前**先读一次活计数、增量累加进 `runtime.json`
//!    （累计命中数与 `last_hit_at` 时间戳），`bui status` 与 `bui set hy2-resi-compat off` 的
//!    「连续 30 天零命中」门禁一律读那份持久值，**绝不读活 counter**（spec §2.4，2026-09-16 裁决）。
//! 3. **与生产上现存的 9 张 `hysteria_*` nft 表跨族共存，已在 tizi 上实测无冲突**：
//!    apernet 那些表在 ip / ip6（第 1 条的 `NFT_FAMILIES`），本表在 inet，天然不共享名字空间；
//!    直连 HY2 仍是 apernet 的内置跳跃，它那些表照旧存在，本表只认住宅那两段端口。
//!
//! 每条规则先确认目标地址属于本机（`fib daddr type local`），再 REDIRECT。
//! 只按端口匹配会把服务器发往外部同号端口的 UDP（如 SOCKS5 UDP relay）送回自己的
//! HY2 入站；prerouting 也必须排除经本机转发的外部流量。fib 同时覆盖 IPv4/IPv6、
//! 回环和本机公网地址，不把安装时探到的公网 IP 固化进规则。
//!
//! 双 hook 是硬要求：只挂 `prerouting` 时，**本机发往自身公网 IP 的包不过 prerouting**，
//! 跳跃对本机自测直接失效（spec §2.4，tizi PoC 实测；apernet 的规则同样是
//! PREROUTING + OUTPUT 两条）。
//!
//! 2026-10-27 起这张表里还有一条主机级封禁链 [`EGRESS_CHAIN`]（filter / output）：
//! 新建的 IPv6 出站一律 reject，只放行本机地址、链路本地/组播、已建立连接的回包与
//! 入站连接需要的 ICMPv6 错误。目的是让升级下载、ACME、住宅探针、伪装反代与内核自身
//! 都不再新建 IPv6 出站，同时保留 IPv6 **入站**。它随整表 `flush` + 重放，watchdog
//! 每 60 秒用 [`egress_signature`] 校验（[`rule_count`] 仍只数 REDIRECT）。
use crate::model::Ports;
use crate::slots::MAX_SLOTS;

/// 族，`nft` 命令行里表标识符的前半段。
pub const FAMILY: &str = "inet";

/// 表名，`nft` 命令行里表标识符的后半段。
pub const NAME: &str = "bui";

/// 完整表标识符：`nft list table inet bui` / `nft delete table inet bui` 直接拼这个串。
pub const TABLE: &str = "inet bui";

/// 4.0 遗留的「每槽一个监听端口」兼容段 = `(hy2_resi + 1, hy2_resi + MAX_SLOTS - 1)`。
///
/// 4.0 的客户端订阅里，住宅 HY2 的端口是 `40000 + 槽序号`；4.1 全员收到 `:40000`，
/// 但**客户端不会自动刷新节点**，所以旧订阅指的那些端口必须继续通。
pub fn compat_range(p: &Ports) -> (u16, u16) {
    (p.hy2_resi + 1, p.hy2_resi + MAX_SLOTS - 1)
}

/// 表里应有的 **REDIRECT** 规则条数（自检与 watchdog 的判据与渲染同源）。
///
/// 语义**不是**整表规则数：watchdog 拿它比跳转/兼容段（2 或 4），
/// IPv6 出站封禁那六条由 [`EGRESS_RULES`] + [`egress_signature`] 单独校验。
pub fn rule_count(compat: bool) -> usize {
    if compat {
        4
    } else {
        2
    }
}

/// 主机级 IPv6 出站封禁链（filter / output）的链名。
pub const EGRESS_CHAIN: &str = "ipv6_egress";

/// [`EGRESS_CHAIN`] 里的规则条数（自检与 watchdog 的判据与渲染同源）。
pub const EGRESS_RULES: usize = 6;

/// 规范化 [`EGRESS_CHAIN`] 的规则，用于比较「盘上那一份」与「期望的那一份」。
///
/// 不逐字比 `nft list` 的回显。真实回显（2026-10-27 netns 实测）会：
/// - 把 `priority 0` 打成 `priority filter`；
/// - 给每条规则插 `counter packets N bytes N`；
/// - 在能由 `ip6` / `icmpv6` 隐含地址族时**省略** `meta nfproto ipv6` 前缀；
/// - 把 `reject with icmpx type admin-prohibited` 打成 `reject with icmpx admin-prohibited`；
/// - 某些版本把 `fib daddr type local` 打成 `fib daddr type == local`。
///
/// 这里按原顺序收集链内每条规则（跳过链头的 `type … hook …` 行），去掉上述可逆差异后返回；
/// 链缺失、条数不符或任何语义差异都会让结果与期望不等，watchdog 据此重放。
pub fn egress_signature(text: &str) -> Vec<String> {
    let mut in_chain = false;
    let mut out = Vec::new();
    for line in text.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("chain ") {
            in_chain = rest.trim_end_matches('{').trim() == EGRESS_CHAIN;
            continue;
        }
        if !in_chain {
            continue;
        }
        // 本链结束：后面的表级 `}` 或别的链不再属于这里
        if line == "}" {
            in_chain = false;
            continue;
        }
        if line.is_empty() || line.starts_with("type ") {
            continue;
        }
        let mut words = Vec::new();
        let mut it = line.split_whitespace();
        while let Some(w) = it.next() {
            match w {
                "counter" => {}
                "packets" | "bytes" => {
                    // `counter packets N bytes N`：数字是瞬时量，不参与比较
                    let _ = it.next();
                }
                _ => words.push(w),
            }
        }
        // 回显里可省略、期望里显式写着的地址族限定：去掉后两边同形
        if words.starts_with(&["meta", "nfproto", "ipv6"]) {
            words.drain(0..3);
        }
        // `fib daddr type == local` 与 `fib daddr type local` 同义
        let mut i = 0;
        while i + 1 < words.len() {
            if words[i] == "type" && words[i + 1] == "==" {
                words.remove(i + 1);
            } else {
                i += 1;
            }
        }
        // `icmpx type admin-prohibited` 的回显不带 `type`
        if let Some(i) = words.iter().position(|w| *w == "icmpx") {
            if words.get(i + 1) == Some(&"type") {
                words.remove(i + 1);
            }
        }
        out.push(words.join(" "));
    }
    out
}

/// 整份规则集，可直接 `nft -f -`。
pub fn ruleset(p: &Ports, compat: bool) -> String {
    let (hop_start, hop_end) = p.hy2_resi_hop;
    let (compat_start, compat_end) = compat_range(p);
    let base = p.hy2_resi;

    let mut out = String::new();
    // 先声明再 flush：表不存在时 `flush table` 会让整份事务失败。
    out.push_str(&format!("table {TABLE}\n"));
    out.push_str(&format!("flush table {TABLE}\n"));
    out.push_str(&format!("table {TABLE} {{\n"));
    // 链名与 hook 同名；`output` 那条的注释带 ` (local)` 后缀以便一眼分辨。
    for (hook, suffix) in [("prerouting", ""), ("output", " (local)")] {
        out.push_str(&format!("  chain {hook} {{\n"));
        out.push_str(&format!(
            "    type nat hook {hook} priority -100; policy accept;\n"
        ));
        out.push_str(&format!(
            "    udp dport {hop_start}-{hop_end} fib daddr type local counter redirect to :{base} comment \"hy2 residential hop{suffix}\"\n"
        ));
        if compat {
            out.push_str(&format!(
                "    udp dport {compat_start}-{compat_end} fib daddr type local counter redirect to :{base} comment \"hy2 residential 4.0 compat{suffix}\"\n"
            ));
        }
        out.push_str("  }\n");
    }
    // 主机级 IPv6 出站封禁（2026-10-27）：新建的 IPv6 出站一律立即 reject（不是 drop），
    // 让 Go / reqwest / Caddy 快速回落 IPv4。保住的四类：
    // 本机地址（自连）、链路本地与组播（NDP / MLD）、已建立连接的回包（IPv6 入站仍可用）、
    // 入站连接需要的 ICMPv6 错误。IPv4 完全不经这条链。
    out.push_str(&format!("  chain {EGRESS_CHAIN} {{\n"));
    out.push_str("    type filter hook output priority 0; policy accept;\n");
    out.push_str("    meta nfproto ipv6 fib daddr type local counter accept\n");
    out.push_str("    meta nfproto ipv6 ip6 daddr { fe80::/10, ff00::/8 } counter accept\n");
    out.push_str("    meta nfproto ipv6 ct state established,related counter accept\n");
    out.push_str(
        "    meta nfproto ipv6 icmpv6 type { destination-unreachable, packet-too-big, \
         time-exceeded, parameter-problem } counter accept\n",
    );
    out.push_str("    meta nfproto ipv6 meta l4proto tcp counter reject with tcp reset\n");
    out.push_str("    meta nfproto ipv6 counter reject with icmpx type admin-prohibited\n");
    out.push_str("  }\n");
    out.push_str("}\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Ports;
    use pretty_assertions::assert_eq;

    fn ports() -> Ports {
        serde_json::from_str(
            r#"{"hy2":10000,"hy2_hop":[20000,30000],"hy2_resi":40000,"hy2_resi_hop":[41000,50000],
                "reality_direct":10001,"reality_resi":10002,"admin":8080}"#,
        )
        .unwrap()
    }

    /// 开着兼容段时的完整规则集：**双 hook 是 tizi PoC 实测得出的硬要求**
    /// （只挂 prerouting 时本机发往自身公网 IP 的包不过 prerouting，跳跃失效）。
    #[test]
    fn the_ruleset_has_both_hooks_and_the_exact_comments() {
        let want = "\
table inet bui {
  chain prerouting {
    type nat hook prerouting priority -100; policy accept;
    udp dport 41000-50000 fib daddr type local counter redirect to :40000 comment \"hy2 residential hop\"
    udp dport 40001-40007 fib daddr type local counter redirect to :40000 comment \"hy2 residential 4.0 compat\"
  }
  chain output {
    type nat hook output priority -100; policy accept;
    udp dport 41000-50000 fib daddr type local counter redirect to :40000 comment \"hy2 residential hop (local)\"
    udp dport 40001-40007 fib daddr type local counter redirect to :40000 comment \"hy2 residential 4.0 compat (local)\"
  }
  chain ipv6_egress {
    type filter hook output priority 0; policy accept;
    meta nfproto ipv6 fib daddr type local counter accept
    meta nfproto ipv6 ip6 daddr { fe80::/10, ff00::/8 } counter accept
    meta nfproto ipv6 ct state established,related counter accept
    meta nfproto ipv6 icmpv6 type { destination-unreachable, packet-too-big, time-exceeded, parameter-problem } counter accept
    meta nfproto ipv6 meta l4proto tcp counter reject with tcp reset
    meta nfproto ipv6 counter reject with icmpx type admin-prohibited
  }
}
";
        let got = ruleset(&ports(), true);
        // 整串等值：顺序（declare → flush → 完整定义）本身就是载荷。只用 `contains` +
        // `starts_with` 时，「flush 挪到定义之后」的变体也判过，而它喂给真 nft 退 0、
        // 落地的却是一张零规则空表——住宅跳跃与兼容段静默失效，正是要防的那起回归。
        assert_eq!(got, format!("table inet bui\nflush table inet bui\n{want}"));
        assert_eq!(rule_count(true), 4);
    }

    /// 兼容段关掉之后只剩两条（`bui set hy2-resi-compat off` 之后的形态）
    #[test]
    fn turning_the_compat_range_off_leaves_two_rules() {
        let got = ruleset(&ports(), false);
        assert!(
            got.contains("udp dport 41000-50000 fib daddr type local counter redirect to :40000")
        );
        assert!(!got.contains("40001-40007"), "兼容段的两条规则一起消失");
        assert_eq!(got.matches("counter redirect").count(), 2);
        assert_eq!(rule_count(false), 2);
        assert_eq!(compat_range(&ports()), (40001, 40007));
    }

    /// 2026-10-27 sudo netns 实测的真实 `nft list table inet bui`（完整规则集落地后）。
    ///
    /// 与期望渲染值的差异全部必须被 [`egress_signature`] 吸收：priority 被打成 filter、
    /// 每条规则带 counter 计数、`ip6`/`icmpv6` 规则省略 `meta nfproto ipv6` 前缀、
    /// `icmpx type` 回显成 `icmpx`。
    const REAL_LISTING: &str = "\
table inet bui {
\tchain prerouting {
\t\ttype nat hook prerouting priority dstnat; policy accept;
\t\tudp dport 41000-50000 fib daddr type local counter packets 2 bytes 86 redirect to :40000 comment \"hy2 residential hop\"
\t\tudp dport 40001-40007 fib daddr type local counter packets 0 bytes 0 redirect to :40000 comment \"hy2 residential 4.0 compat\"
\t}
\tchain output {
\t\ttype nat hook output priority dstnat; policy accept;
\t\tudp dport 41000-50000 fib daddr type local counter packets 4 bytes 172 redirect to :40000 comment \"hy2 residential hop (local)\"
\t\tudp dport 40001-40007 fib daddr type local counter packets 0 bytes 0 redirect to :40000 comment \"hy2 residential 4.0 compat (local)\"
\t}
\tchain ipv6_egress {
\t\ttype filter hook output priority filter; policy accept;
\t\tmeta nfproto ipv6 fib daddr type local counter packets 6 bytes 414 accept
\t\tip6 daddr { fe80::/10, ff00::/8 } counter packets 0 bytes 0 accept
\t\tmeta nfproto ipv6 ct state established,related counter packets 1 bytes 53 accept
\t\ticmpv6 type { destination-unreachable, packet-too-big, time-exceeded, parameter-problem } counter packets 0 bytes 0 accept
\t\tmeta nfproto ipv6 meta l4proto tcp counter packets 0 bytes 0 reject with tcp reset
\t\tmeta nfproto ipv6 counter packets 2 bytes 106 reject with icmpx admin-prohibited
\t}
}
";

    /// 主理人 2026-10-27 在 root netns 里贴回的同一条链（零计数形态，逐字）。
    const REAL_LISTING_ZERO: &str = "\
table inet bui {
\tchain ipv6_egress {
\t\ttype filter hook output priority filter; policy accept;
\t\tmeta nfproto ipv6 fib daddr type local counter packets 0 bytes 0 accept
\t\tip6 daddr { fe80::/10, ff00::/8 } counter packets 0 bytes 0 accept
\t\tmeta nfproto ipv6 ct state established,related counter packets 0 bytes 0 accept
\t\ticmpv6 type { destination-unreachable, packet-too-big, time-exceeded, parameter-problem } counter packets 0 bytes 0 accept
\t\tmeta nfproto ipv6 meta l4proto tcp counter packets 0 bytes 0 reject with tcp reset
\t\tmeta nfproto ipv6 counter packets 0 bytes 0 reject with icmpx admin-prohibited
\t}
}
";

    /// IPv6 出站封禁链必须随整表落地，且 watchdog/selfcheck 能用规范化签名比较
    /// 「盘上」与「期望」。基线上面的 `REAL_LISTING` 是 2026-10-27 sudo netns 实测原文。
    #[test]
    fn the_ipv6_egress_chain_matches_the_real_nft_list_output() {
        let want = ruleset(&ports(), true);
        let sig = egress_signature(&want);
        assert_eq!(sig.len(), EGRESS_RULES);
        assert_eq!(
            egress_signature(REAL_LISTING),
            sig,
            "真实回显与期望必须同签名，否则 watchdog 每 60 秒误重放"
        );
        assert_eq!(
            egress_signature(REAL_LISTING_ZERO),
            sig,
            "主理人贴回的零计数回显必须同签名"
        );
        assert_eq!(sig[0], "fib daddr type local accept");
        assert_eq!(sig[1], "ip6 daddr { fe80::/10, ff00::/8 } accept");
        assert_eq!(sig[2], "ct state established,related accept");
        assert_eq!(
            sig[3],
            "icmpv6 type { destination-unreachable, packet-too-big, time-exceeded, parameter-problem } accept"
        );
        assert_eq!(sig[4], "meta l4proto tcp reject with tcp reset");
        assert_eq!(sig[5], "reject with icmpx admin-prohibited");
        // 某些 nft 版本会显式打印 `==`；不能因此判成规则不符。
        assert_eq!(
            egress_signature(
                &REAL_LISTING.replace("fib daddr type local", "fib daddr type == local")
            ),
            sig
        );
        // 改一条 reject 为 accept ⇒ 签名必须不等，watchdog 会重放
        let tampered = REAL_LISTING_ZERO.replace("reject with icmpx admin-prohibited", "accept");
        let tampered_sig = egress_signature(&tampered);
        assert_eq!(tampered_sig.len(), EGRESS_RULES, "条数仍对，内容已变");
        assert_ne!(tampered_sig, sig, "改一条 reject 必须让签名不等");
        // 链缺失（旧表 / 被人删掉）⇒ 空签名 ⇒ watchdog 必须重放
        assert!(
            egress_signature(&ruleset(&ports(), false).replace(EGRESS_CHAIN, "renamed")).is_empty()
        );
        assert!(egress_signature(&REAL_LISTING_ZERO.replace(EGRESS_CHAIN, "renamed")).is_empty());
        assert_eq!(rule_count(true), 4, "rule_count 仍只数 REDIRECT");
    }

    /// 端口改过的机器：规则跟着期望态走，函数里不许有第二份端口口径
    #[test]
    fn the_rules_follow_the_desired_state_ports() {
        let mut p = ports();
        p.hy2_resi = 45000;
        p.hy2_resi_hop = (46000, 47000);
        let got = ruleset(&p, true);
        assert!(
            got.contains("udp dport 46000-47000 fib daddr type local counter redirect to :45000")
        );
        assert!(
            got.contains("udp dport 45001-45007 fib daddr type local counter redirect to :45000")
        );
    }
}
