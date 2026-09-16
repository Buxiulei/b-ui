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
//!    受支持目标（Ubuntu 22.04+ / Debian 12 / CentOS Stream 9）内核都 ≥ 5.15，低于 5.2 的只有
//!    已 EOL 的 CentOS 7（3.10）与 Debian 10（4.19），所以 4.1 直接钉这道下限，**不**退回
//!    `ip` + `ip6` 双表（双表会让规则数翻倍，`rule_count` 与自检 / watchdog 的判据随之分叉）；
//!    落地路径负责 `nft -c -f` 预检与内核版本核对，失败时显式报错而不是让跳跃静默消失
//!    （2026-09-16 裁决）。
//! 2. **每条规则都带 `counter`，但它是瞬时流计数，不是兼容段的下线判据本体。**
//!    `flush table` 连具名 counter 对象一起清零，所以开机、`bui nft apply`、watchdog 自愈重放、
//!    改端口——每次重放都让计数从 0 重新开始；nat 链的 counter 又只计每条 conntrack 流的首包
//!    （`man nft`：「Only the first packet of a connection …」），量纲是流数而非包数。判据因此
//!    归持久层：守护进程在每次重放**之前**先读一次活计数、增量累加进 `runtime.json`
//!    （累计命中数与 `last_hit_at` 时间戳），`bui status` 与 `bui set hy2-resi-compat off` 的
//!    「连续 30 天零命中」门禁一律读那份持久值，**绝不读活 counter**（spec §2.4，2026-09-16 裁决）。
//! 3. **与生产上现存的 9 张 `hysteria_*` nft 表跨族共存，已在 tizi 上实测无冲突**：
//!    apernet 那些表在 ip / ip6（第 1 条的 `NFT_FAMILIES`），本表在 inet，天然不共享名字空间；
//!    直连 HY2 仍是 apernet 的内置跳跃，它那些表照旧存在，本表只认住宅那两段端口。
//!
//! 双 hook 是硬要求：只挂 `prerouting` 时，**本机发往自身公网 IP 的包不过 prerouting**，
//! 跳跃对本机自测直接失效（spec §2.4，tizi PoC 实测；apernet 的规则同样是
//! PREROUTING + OUTPUT 两条）。
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

/// 表里应有的规则条数（自检与 watchdog 的判据与渲染同源）。
pub fn rule_count(compat: bool) -> usize {
    if compat {
        4
    } else {
        2
    }
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
            "    udp dport {hop_start}-{hop_end} counter redirect to :{base} comment \"hy2 residential hop{suffix}\"\n"
        ));
        if compat {
            out.push_str(&format!(
                "    udp dport {compat_start}-{compat_end} counter redirect to :{base} comment \"hy2 residential 4.0 compat{suffix}\"\n"
            ));
        }
        out.push_str("  }\n");
    }
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
    udp dport 41000-50000 counter redirect to :40000 comment \"hy2 residential hop\"
    udp dport 40001-40007 counter redirect to :40000 comment \"hy2 residential 4.0 compat\"
  }
  chain output {
    type nat hook output priority -100; policy accept;
    udp dport 41000-50000 counter redirect to :40000 comment \"hy2 residential hop (local)\"
    udp dport 40001-40007 counter redirect to :40000 comment \"hy2 residential 4.0 compat (local)\"
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
        assert!(got.contains("udp dport 41000-50000 counter redirect to :40000"));
        assert!(!got.contains("40001-40007"), "兼容段的两条规则一起消失");
        assert_eq!(got.matches("counter redirect").count(), 2);
        assert_eq!(rule_count(false), 2);
        assert_eq!(compat_range(&ports()), (40001, 40007));
    }

    /// 端口改过的机器：规则跟着期望态走，函数里不许有第二份端口口径
    #[test]
    fn the_rules_follow_the_desired_state_ports() {
        let mut p = ports();
        p.hy2_resi = 45000;
        p.hy2_resi_hop = (46000, 47000);
        let got = ruleset(&p, true);
        assert!(got.contains("udp dport 46000-47000 counter redirect to :45000"));
        assert!(got.contains("udp dport 45001-45007 counter redirect to :45000"));
    }
}
