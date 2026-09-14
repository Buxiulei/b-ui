//! `bui residential <子命令>` 与住宅子菜单（spec §2.4、§5）。
//!
//! **全部**子命令经 `/run/b-ui.sock` 调本模块自己的 HTTP 端点（spec §2.4「所有菜单项
//! 通过 unix socket 调守护进程 API」），不在 CLI 进程里直接碰 `state` —— 否则会与守护
//! 进程的 `Store` 并发写同一个 `state.json`。
//!
//! 子命令 → 端点的翻译是**纯函数**（[`to_request`] / [`blacklist_request`]），因此
//! 全部路由与载荷都能在不起守护进程的情况下单测。

use crate::commands::menu::{MenuAction, MenuItem};
use crate::modules::residential::upstream;
use std::path::PathBuf;

/// `bui residential` 的子命令。
#[derive(Debug, Clone, clap::Subcommand, PartialEq, Eq)]
pub enum ResidentialCmd {
    /// 查看住宅池状态
    Status {
        #[arg(long)]
        json: bool,
    },
    /// 加一条上游；`-` 表示从 stdin 读一行（凭据不进 argv，`ps` 看不到）
    Add { url: String },
    /// 删一条上游（`<uuid>`、`resi-N` / `url-N` 或 `<host:port>`）
    Remove { target: String },
    /// 总开关：开
    Enable,
    /// 总开关：关
    Disable,
    /// 分流模式（on = 全量走住宅 / off = 按关键字分流）
    Global {
        #[arg(value_parser = ["on", "off"])]
        on_off: String,
    },
    /// 打印生效的分流关键字（JSON 数组，供脚本消费；等价于 v3 `residential-helper.sh domains`）
    Domains,
    /// 分流关键字回到跟随默认表
    RestoreDefault,
    /// 体检（不带 `--id` 则体检当前选中的上游）。`--id` 收 `<uuid>`、`resi-N` / `url-N`
    /// 或 `<host:port>`
    Check {
        #[arg(long)]
        id: Option<String>,
    },
    /// 手动切换当前出口（`<uuid>`、`resi-N` / `url-N` 或 `<host:port>`，锁定到它
    /// 不健康为止）；`--auto` 解除锁定回到自动选路
    Select {
        target: Option<String>,
        #[arg(long)]
        auto: bool,
    },
    /// 巡检与出口画像
    Health {
        #[arg(long)]
        json: bool,
    },
    /// 按槽列出 IP、当前实际出口、用户数与指标
    Slots {
        #[arg(long)]
        json: bool,
    },
    /// 把某一槽的出口钉在指定上游上（`<uuid>`、`resi-N` / `url-N` 或 `<host:port>`）；
    /// `--auto` 解除，回到「本槽优先 / 借用」的自动驱动。
    /// **与 `blacklist pin`（钉域名走直连）无关**，钉的是槽位的出口 IP
    SlotPin {
        index: u16,
        target: Option<String>,
        #[arg(long)]
        auto: bool,
    },
    /// 把住宅用户在各槽间均匀重排（按创建时间稳定排序）
    Rebalance,
    /// 把某个用户钉到某一槽（`<槽序号>`、`<uuid>`、`resi-N` / `url-N` 或 `<host:port>`）
    Assign { user: String, target: String },
    /// 黑名单（查看 / 钉住 / 立即应用）
    Blacklist {
        #[command(subcommand)]
        cmd: BlacklistCmd,
    },
}

/// `bui residential blacklist` 的子命令。
#[derive(Debug, Clone, clap::Subcommand, PartialEq, Eq)]
pub enum BlacklistCmd {
    /// 列出 pins / auto / 待生效 / 候选
    List {
        #[arg(long)]
        json: bool,
    },
    /// 钉住（强制直连）
    Pin {
        value: String,
        #[arg(long, default_value = "domain_suffix")]
        kind: String,
        #[arg(long, default_value = "")]
        note: String,
    },
    /// 取消钉住
    Unpin {
        value: String,
        #[arg(long, default_value = "domain_suffix")]
        kind: String,
    },
    /// 把已确认的待生效条目立刻写进黑名单（会重启 b-ui-relay）
    Apply,
}

/// 把一个子命令翻译成一次 socket 请求：`(method, path, body)`。
pub fn to_request(
    cmd: &ResidentialCmd,
) -> anyhow::Result<(&'static str, String, Option<serde_json::Value>)> {
    use ResidentialCmd as C;
    Ok(match cmd {
        // domains 只是 status 的一个投影（服务端不必为它单开端点）
        C::Status { .. } | C::Domains => ("GET", "/api/residential/status".into(), None),
        C::Health { .. } => ("GET", "/api/residential/health".into(), None),
        C::Add { url } => (
            "POST",
            "/api/residential/add".into(),
            Some(serde_json::json!({"url": url})),
        ),
        // 定位字符串原样送到端点：CLI 不读 state（spec §2.4），uuid / resi-N / url-N /
        // host:port 四种写法统一由服务端的 `upstream::resolve_upstream` 解析
        C::Remove { target } => (
            "POST",
            "/api/residential/remove".into(),
            Some(serde_json::json!({"id": target})),
        ),
        C::Enable => (
            "POST",
            "/api/residential/enable".into(),
            Some(serde_json::json!({"enabled": true})),
        ),
        C::Disable => (
            "POST",
            "/api/residential/enable".into(),
            Some(serde_json::json!({"enabled": false})),
        ),
        C::Global { on_off } => (
            "POST",
            "/api/residential/global".into(),
            Some(serde_json::json!({"global": on_off.as_str() == "on"})),
        ),
        C::RestoreDefault => ("POST", "/api/residential/restore-default".into(), None),
        C::Check { id } => (
            "POST",
            "/api/residential/check".into(),
            Some(match id {
                Some(id) => serde_json::json!({"id": id}),
                None => serde_json::json!({}),
            }),
        ),
        C::Select { target, auto } => {
            // 两种形态互斥：`select <target>` 锁定，`select --auto` 解锁
            anyhow::ensure!(
                !(*auto && target.is_some()),
                "select 只能二选一：<target>（手动锁定）或 --auto（解除锁定）"
            );
            let body = match (auto, target) {
                (true, _) => serde_json::json!({"auto": true}),
                (false, Some(t)) => serde_json::json!({"id": t}),
                (false, None) => anyhow::bail!("select 要么给 <target>，要么给 --auto"),
            };
            ("POST", "/api/residential/select".into(), Some(body))
        }
        C::Slots { .. } => ("GET", "/api/residential/slots".into(), None),
        C::SlotPin {
            index,
            target,
            auto,
        } => {
            anyhow::ensure!(
                !(*auto && target.is_some()),
                "slot-pin 只能二选一：<target>（钉住）或 --auto（解除）"
            );
            let body = match (auto, target) {
                (true, _) => serde_json::json!({"index": index, "auto": true}),
                (false, Some(t)) => serde_json::json!({"index": index, "id": t}),
                (false, None) => anyhow::bail!("slot-pin 要么给 <target>，要么给 --auto"),
            };
            ("POST", "/api/residential/slots/pin".into(), Some(body))
        }
        C::Rebalance => ("POST", "/api/residential/rebalance".into(), None),
        C::Assign { user, target } => (
            "POST",
            "/api/residential/assign".into(),
            Some(serde_json::json!({"user": user, "target": target})),
        ),
        C::Blacklist { cmd } => return blacklist_request(cmd),
    })
}

/// 同 [`to_request`]，但管 `blacklist` 那一支。非法 `--kind` 在 CLI 侧就挡掉。
pub fn blacklist_request(
    cmd: &BlacklistCmd,
) -> anyhow::Result<(&'static str, String, Option<serde_json::Value>)> {
    let check_kind = |k: &str| -> anyhow::Result<()> {
        anyhow::ensure!(
            matches!(k, "domain_suffix" | "domain" | "port"),
            "--kind 只能是 domain_suffix / domain / port，实际 {k}"
        );
        Ok(())
    };
    Ok(match cmd {
        BlacklistCmd::List { .. } => ("GET", "/api/residential/blacklist".into(), None),
        BlacklistCmd::Apply => ("POST", "/api/residential/blacklist/apply".into(), None),
        BlacklistCmd::Pin { value, kind, note } => {
            check_kind(kind)?;
            (
                "POST",
                "/api/residential/blacklist/pins".into(),
                Some(serde_json::json!({"kind": kind, "value": value, "note": note})),
            )
        }
        BlacklistCmd::Unpin { value, kind } => {
            check_kind(kind)?;
            (
                "DELETE",
                "/api/residential/blacklist/pins".into(),
                Some(serde_json::json!({"kind": kind, "value": value})),
            )
        }
    })
}

/// `add -` 时从 stdin 读一行（凭据不进 argv，`ps` 看得到 argv）。
pub fn resolve_url(raw: &str, stdin: &mut impl std::io::BufRead) -> anyhow::Result<String> {
    if raw != "-" {
        return Ok(raw.trim().to_string());
    }
    let mut line = String::new();
    stdin.read_line(&mut line)?;
    let line = line.trim().to_string();
    anyhow::ensure!(!line.is_empty(), "stdin 未读到代理 URL");
    Ok(line)
}

fn as_str<'a>(v: &'a serde_json::Value, k: &str) -> &'a str {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("-")
}

fn as_bool(v: &serde_json::Value, k: &str) -> bool {
    v.get(k).and_then(|x| x.as_bool()).unwrap_or(false)
}

fn as_u64(v: &serde_json::Value, k: &str) -> u64 {
    v.get(k).and_then(|x| x.as_u64()).unwrap_or(0)
}

fn as_arr<'a>(v: &'a serde_json::Value, k: &str) -> &'a [serde_json::Value] {
    v.get(k)
        .and_then(|x| x.as_array())
        .map(|a| a.as_slice())
        .unwrap_or(&[])
}

/// `google_ok` 的人类可读形态（`null` = 还没探到结论，R2 ②）
fn google_label(v: &serde_json::Value) -> &'static str {
    match v.get("google_ok").and_then(|x| x.as_bool()) {
        Some(true) => "Google 通",
        Some(false) => "Google 封",
        None => "Google 未知",
    }
}

/// 百分比（`success_rate_24h` 是 0.0–1.0）
fn pct(v: &serde_json::Value, k: &str) -> i64 {
    (v.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0) * 100.0).round() as i64
}

/// 一个数字字段的人读形态；**没测过打 `-`**，绝不打 0 —— 面板与终端上「未知」和
/// 「0 毫秒 / 0 Mbps」是两回事（后者会被误读成最优）
fn num(v: &serde_json::Value, k: &str) -> String {
    match v.get(k) {
        Some(serde_json::Value::Number(n)) => match n.as_u64() {
            Some(u) => u.to_string(),
            // 速度是浮点，一位小数就够看
            None => format!("{:.1}", n.as_f64().unwrap_or_default()),
        },
        _ => "-".into(),
    }
}

/// 延迟一段：`延迟 p50 100 / p95 300 ms（TCP p50 33 ms）`
fn latency_text(v: &serde_json::Value) -> String {
    format!(
        "延迟 p50 {} / p95 {} ms（TCP p50 {} ms）",
        num(v, "latency_p50_ms"),
        num(v, "latency_p95_ms"),
        num(v, "tcp_p50_ms"),
    )
}

/// 速度一段：`↓88.5 / ↑12.3 Mbps（测于 …）`
fn speed_text(v: &serde_json::Value) -> String {
    format!(
        "↓{} / ↑{} Mbps（测于 {}）",
        num(v, "down_mbps"),
        num(v, "up_mbps"),
        as_str(v, "speed_at"),
    )
}

/// UDP 一段：`UDP 通（198.51.100.9，p50 50 ms）` / `UDP 不通（HTTP 上游无 UDP）`
fn udp_text(v: &serde_json::Value) -> String {
    match v.get("udp_ok").and_then(|x| x.as_bool()) {
        Some(true) => format!(
            "UDP 通（{}，p50 {} ms）",
            as_str(v, "udp_exit_ip"),
            num(v, "udp_p50_ms")
        ),
        Some(false) => format!("UDP 不通（{}）", as_str(v, "udp_note")),
        None => "UDP 未知".into(),
    }
}

/// 选路那几行：`选路原因` + 「更优候选」防抖进度 + 上次全量测速时间。
/// **status / health 共用**（两处的字段同源，渲染也不许有两份）。
fn selection_lines(v: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(why) = v.get("selected_reason").and_then(|x| x.as_str()) {
        out.push(format!("选路原因：{why}"));
    }
    // 防抖进度：面板与终端都要看得出「还差几轮才会切」
    let (rounds, needed) = (
        as_u64(v, "switch_improve_rounds"),
        as_u64(v, "switch_improve_needed"),
    );
    if needed > 0 {
        out.push(
            match v.get("switch_improve_candidate").and_then(|x| x.as_str()) {
                Some(tag) => {
                    format!(
                        "切换条件：{tag} 已连续 {rounds}/{needed} 轮延迟 / 速度更优（攒满才切）"
                    )
                }
                None => format!("切换条件：当前没有明显更优的候选（0/{needed} 轮）"),
            },
        );
        out.push(format!("上次全量测速：{}", as_str(v, "last_speedtest_at")));
    }
    out
}

/// `status` 的人类可读渲染。**只读**传入的 JSON 字段，绝不打印 `password`（服务端也不回它）。
/// status / health 共用的一行：sing-box 的 http 出站没有 UDP 能力，池里混一条就整池不走 UDP。
fn udp_line(v: &serde_json::Value) -> String {
    format!(
        "UDP 经住宅：{}（池内含 http 上游时为否）",
        if as_bool(v, "udp_via_residential") {
            "是"
        } else {
            "否"
        }
    )
}

pub fn format_status(v: &serde_json::Value) -> String {
    let mut out = vec![format!(
        "总开关：{}",
        if as_bool(v, "enabled") {
            "已启用"
        } else {
            "已关闭"
        }
    )];
    out.push(format!(
        "分流模式：{}",
        if as_bool(v, "global") {
            "global（全量走住宅）"
        } else {
            "split（按关键字分流）"
        }
    ));
    out.push(format!(
        "分流关键字：{} 条（{}）",
        as_arr(v, "domains").len(),
        if as_bool(v, "domainsFollowDefault") {
            "跟随默认表"
        } else {
            "自定义"
        }
    ));
    out.push(format!(
        "当前生效：{} / {}",
        as_str(v, "active_tag"),
        as_str(v, "active_upstream_id")
    ));
    out.extend(selection_lines(v));
    out.push(udp_line(v));
    if as_bool(v, "selected_pending_persist") {
        out.push("  自动切换后的落点尚未持久化，将在每日 04:00 写回".to_string());
    }
    let urls = as_arr(v, "urls");
    // Google 判定在 v4 追加的 `upstreams[]` 里（`urls[]` 是 v3 契约），按 id 对上
    let ups = as_arr(v, "upstreams");
    out.push(format!("上游 {} 条：", urls.len()));
    for u in urls {
        let row = ups
            .iter()
            .find(|x| x.get("id") == u.get("id"))
            .cloned()
            .unwrap_or(serde_json::json!({}));
        out.push(format!(
            "  - {} [{}] {}:{} 出口 {} 优先级 {} {} {}",
            as_str(u, "name"),
            as_str(u, "type"),
            as_str(u, "host"),
            as_u64(u, "port"),
            as_str(u, "lastVerifiedIp"),
            as_u64(u, "priority"),
            google_label(&row),
            as_str(u, "displayUrl"),
        ));
        // 延迟 / 速度 / UDP 另起一行：上一行已经满了，挤在一起没人读得下去
        out.push(format!(
            "      {} {} {}",
            latency_text(&row),
            speed_text(&row),
            udp_text(&row)
        ));
    }
    let b = v.get("blacklist").cloned().unwrap_or(serde_json::json!({}));
    out.push(format!(
        "黑名单：钉住 {} / 自动 {} / 待生效 {} / 候选 {}",
        as_u64(&b, "pins"),
        as_u64(&b, "auto"),
        as_u64(&b, "pending"),
        as_u64(&b, "candidates"),
    ));
    for n in as_arr(v, "notes") {
        out.push(format!("说明：{}", n.as_str().unwrap_or_default()));
    }
    for a in as_arr(v, "alerts") {
        out.push(format!("告警：{}", a.as_str().unwrap_or_default()));
    }
    out.extend(slot_lines(v));
    out.join("\n")
}

/// `health` 的人类可读渲染（当前选中的成员行以 `*` 标出）。
pub fn format_health(v: &serde_json::Value) -> String {
    let selected = as_str(v, "selected");
    let mut out = vec![format!(
        "巡检：{}，模式 {}，当前 {}",
        if as_bool(v, "enabled") {
            "已启用"
        } else {
            "已关闭"
        },
        as_str(v, "mode"),
        selected,
    )];
    out.push(format!("分流关键字 {} 条", as_u64(v, "domains_count")));
    out.extend(selection_lines(v));
    out.push(udp_line(v));
    out.push(format!(
        "当前出口 IP：{}（{}）",
        v.get("current_egress_ip_test")
            .and_then(|x| x.as_str())
            .unwrap_or("-"),
        as_str(v, "egress_ip_type"),
    ));
    let members = as_arr(v, "members");
    out.push(format!("池内成员 {} 条：", members.len()));
    for m in members {
        let tag = as_str(m, "tag");
        let e = m.get("egress").cloned().unwrap_or(serde_json::json!({}));
        out.push(format!(
            "{} {} [{}] {}:{} {}{} 连续成功 {} / 连续失败 {} 24h 成功率 {}% 优先级 {} {} 出口 {}（{}，{}）黑名单 {} 条",
            if tag == selected { "*" } else { " " },
            tag,
            as_str(m, "type"),
            as_str(m, "host"),
            as_u64(m, "port"),
            if as_bool(m, "active") { "健康" } else { "不健康" },
            // 手动锁定：巡检不会按 priority 把它切走（R2 ①）
            if as_bool(m, "manual_locked") { "（手动锁定）" } else { "" },
            as_u64(m, "okstreak"),
            as_u64(m, "failstreak"),
            pct(m, "success_rate_24h"),
            as_u64(m, "priority"),
            google_label(m),
            as_str(&e, "ip"),
            as_str(&e, "type"),
            as_str(&e, "isp"),
            as_u64(m, "blacklist_count"),
        ));
        // 指标另起一行：主理人要的「延迟、上下行速度、UDP」三项，挤进上一行读不了
        out.push(format!(
            "    {} {} {}",
            latency_text(m),
            speed_text(m),
            udp_text(m)
        ));
    }
    for n in as_arr(v, "notes") {
        out.push(format!("说明：{}", n.as_str().unwrap_or_default()));
    }
    for a in as_arr(v, "alerts") {
        out.push(format!("告警：{}", a.as_str().unwrap_or_default()));
    }
    out.extend(slot_lines(v));
    out.join("\n")
}

/// `bui residential slots` 的人类可读输出。
pub fn format_slots(v: &serde_json::Value) -> String {
    let mut out = vec![
        "槽  IP                 当前出口    用户  延迟     HY2 端口        用户名".to_string(),
    ];
    for s in as_arr(v, "slots") {
        let users = as_arr(s, "users")
            .iter()
            .filter_map(|x| x.as_str())
            .collect::<Vec<_>>()
            .join(",");
        out.push(format!(
            "{:<3} {:<18} {:<11} {:<5} {:<8} {:<15} {}",
            as_u64(s, "index"),
            s.get("ip").and_then(|x| x.as_str()).unwrap_or("—"),
            format!(
                "{}{}",
                s.get("active_tag").and_then(|x| x.as_str()).unwrap_or("—"),
                if s.get("pinned").and_then(|x| x.as_bool()) == Some(true) {
                    "(钉)"
                } else if s.get("borrowed").and_then(|x| x.as_bool()) == Some(true) {
                    "(借)"
                } else {
                    ""
                }
            ),
            as_u64(s, "user_count"),
            latency_text(s),
            format!(
                "{}+{}-{}",
                as_u64(s, "hy2_port"),
                as_arr(s, "hop")
                    .first()
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0),
                as_arr(s, "hop")
                    .get(1)
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0)
            ),
            users
        ));
    }
    out.join("\n")
}

/// `status` / `health` 末尾的槽位段（spec §5.6 要求两处都按槽列出）。
/// 空池 / 没有 `slots` 字段时返回空 —— `format_slots` 那时只会打个表头。
fn slot_lines(v: &serde_json::Value) -> Vec<String> {
    if as_arr(v, "slots").is_empty() {
        return Vec::new();
    }
    vec!["槽位：".to_string(), format_slots(v)]
}

/// `blacklist` 的人类可读渲染：pins / auto / 待生效 + 候选，末尾附局限说明。
pub fn format_blacklist(v: &serde_json::Value) -> String {
    let pins = as_arr(v, "pins");
    let mut out = vec![format!("钉住（强制直连）{} 条：", pins.len())];
    for p in pins {
        out.push(format!(
            "  - [{}] {} {} 建于 {}",
            as_str(p, "kind"),
            as_str(p, "value"),
            as_str(p, "note"),
            as_str(p, "created_at"),
        ));
    }
    let auto = as_arr(v, "auto");
    out.push(format!("自动 {} 条：", auto.len()));
    out.extend(upstream_groups(
        auto,
        |a| format!("[{}] {}", as_str(a, "kind"), as_str(a, "value")),
        |a| {
            format!(
                "命中 {} 复核通过 {} 确认于 {}",
                as_u64(a, "hits"),
                as_u64(a, "passes"),
                as_str(a, "confirmed_at"),
            )
        },
    ));
    let pending = as_arr(v, "pending");
    out.push(format!(
        "待生效 {} 条（下一个 04:00 窗口批量写入）：",
        pending.len()
    ));
    out.extend(upstream_groups(pending, host_port, |p| {
        format!(
            "已确认 {} 次，最近 {}",
            as_u64(p, "confirms"),
            as_str(p, "last_confirm_at"),
        )
    }));
    let cands = as_arr(v, "candidates");
    out.push(format!("候选 {} 条：", cands.len()));
    out.extend(upstream_groups(cands, host_port, |c| {
        format!(
            "被拒 {} 次，最近 {}",
            as_u64(c, "hits"),
            as_str(c, "last_seen"),
        )
    }));
    for n in as_arr(v, "notes") {
        out.push(format!("说明：{}", n.as_str().unwrap_or_default()));
    }
    out.join("\n")
}

fn host_port(r: &serde_json::Value) -> String {
    format!("{}:{}", as_str(r, "host"), as_u64(r, "port"))
}

/// 黑名单条目按上游分组（按首次出现的顺序）：组标题写上游名、槽位与 host:port，
/// 组内按 `key` 去重，重复的只列首条并标 `×N`。三个上游学到同一批域名时不再平铺成
/// 「看着重复三遍」的一长串。
fn upstream_groups(
    rows: &[serde_json::Value],
    key: impl Fn(&serde_json::Value) -> String,
    detail: impl Fn(&serde_json::Value) -> String,
) -> Vec<String> {
    // (upstream_id, 组内首条, [(key, 首条, 次数)])
    type Group<'a> = (
        &'a str,
        &'a serde_json::Value,
        Vec<(String, &'a serde_json::Value, usize)>,
    );
    let mut groups: Vec<Group> = Vec::new();
    for r in rows {
        let id = as_str(r, "upstream_id");
        let gi = match groups.iter().position(|g| g.0 == id) {
            Some(i) => i,
            None => {
                groups.push((id, r, Vec::new()));
                groups.len() - 1
            }
        };
        let k = key(r);
        let items = &mut groups[gi].2;
        match items.iter_mut().find(|it| it.0 == k) {
            Some(it) => it.2 += 1,
            None => items.push((k, r, 1)),
        }
    }
    let mut out = Vec::new();
    for (_, head, items) in groups {
        let slot = head
            .get("upstream_slot")
            .and_then(|x| x.as_u64())
            .map_or_else(|| "-".to_string(), |i| i.to_string());
        let addr = match as_str(head, "upstream_addr") {
            "" => "-",
            a => a,
        };
        out.push(format!(
            "  上游 {}（槽 {slot}，{addr}）{} 条：",
            as_str(head, "upstream_name"),
            items.len(),
        ));
        for (k, r, n) in items {
            let times = if n > 1 {
                format!(" ×{n}")
            } else {
                String::new()
            };
            out.push(format!("    - {k}{times} {}", detail(r)));
        }
    }
    out
}

/// `remove` 的人读渲染：把「必须重新获取订阅」的用户**按后果逐组**打给操作者。
///
/// 名单来自回包的 `port_changed`（服务端已按「手里那份订阅还能不能用」算过，并分成三组）：
/// HY2 住宅节点的端口或端口跳跃区间写死在已下发的订阅里，客户端要等下一次订阅更新才会知道
/// 它变了。组名与后果文案直接取 [`upstream::impact_title`] / [`upstream::impact_groups`]，
/// 与哨兵事件、面板同一份口径。**没有人受影响时不打空名单。**
pub fn format_remove(v: &serde_json::Value) -> String {
    let impact: bui_schema::slots::ResubscribeImpact = v
        .get("port_changed")
        .cloned()
        .and_then(|x| serde_json::from_value(x).ok())
        .unwrap_or_default();
    if impact.is_empty() {
        return "上游已移除（没有用户需要重新获取订阅）".into();
    }
    let mut out = format!("上游已移除。{}：", upstream::impact_title(&impact));
    for line in upstream::impact_groups(&impact) {
        out.push_str("\n  ");
        out.push_str(&line);
    }
    out
}

fn print_or(json: bool, v: &serde_json::Value, f: impl Fn(&serde_json::Value) -> String) {
    if json {
        println!("{v}");
    } else {
        println!("{}", f(v));
    }
}

/// 派发一条子命令：`Client::available()` 为假时报「守护进程未运行」。
pub async fn run(cmd: ResidentialCmd, socket: PathBuf) -> anyhow::Result<()> {
    let client = crate::ipc::Client::new(&socket);
    anyhow::ensure!(
        client.available().await,
        "守护进程未运行（{}），请先 `systemctl start b-ui`",
        socket.display()
    );
    // `add -` 的 stdin 读取放在请求组装之前，凭据不进 argv
    let cmd = match cmd {
        ResidentialCmd::Add { url } => {
            let mut stdin = std::io::BufReader::new(std::io::stdin());
            ResidentialCmd::Add {
                url: resolve_url(&url, &mut stdin)?,
            }
        }
        other => other,
    };
    let (method, path, body) = to_request(&cmd)?;
    let (status, v) = client.request(method, &path, body).await?;
    if !(200..300).contains(&status) {
        anyhow::bail!(
            "{}",
            v.get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("请求失败")
        );
    }
    match &cmd {
        ResidentialCmd::Status { json } => print_or(*json, &v, format_status),
        ResidentialCmd::Health { json } => print_or(*json, &v, format_health),
        ResidentialCmd::Blacklist {
            cmd: BlacklistCmd::List { json },
        } => print_or(*json, &v, format_blacklist),
        ResidentialCmd::Slots { json } => print_or(*json, &v, format_slots),
        ResidentialCmd::Remove { .. } => println!("{}", format_remove(&v)),
        ResidentialCmd::SlotPin { index, auto, .. } => println!(
            "槽 {index} {}",
            if *auto {
                "已解除 pin，回到自动驱动"
            } else {
                "已钉住"
            }
        ),
        ResidentialCmd::Rebalance => println!(
            "已重排 {} 个用户{}",
            v.get("moved").and_then(|m| m.as_u64()).unwrap_or(0),
            if v.get("xray_rules_pending") == Some(&serde_json::json!(true)) {
                "，约 1 秒后槽路由生效（不重启 xray）"
            } else {
                ""
            }
        ),
        ResidentialCmd::Assign { user, target } => {
            println!("已把用户 {user} 分到 {target}，约 1 秒后槽路由生效（不重启 xray）")
        }
        // 供脚本消费：只打印生效关键字数组（等价于 v3 `residential-helper.sh domains`）
        ResidentialCmd::Domains => println!(
            "{}",
            v.get("domains").cloned().unwrap_or(serde_json::json!([]))
        ),
        _ => println!("{v}"),
    }
    Ok(())
}

/// 住宅子菜单的 12 项（两列渲染沿用 P1 的 [`crate::commands::menu::render_with`]）。
pub fn menu_items() -> Vec<MenuItem> {
    vec![
        MenuItem {
            key: "1",
            title: "住宅池状态",
            action: MenuAction::Residential,
        },
        MenuItem {
            key: "2",
            title: "添加上游（粘贴供应商那一行）",
            action: MenuAction::Residential,
        },
        MenuItem {
            key: "3",
            title: "移除上游",
            action: MenuAction::Residential,
        },
        MenuItem {
            key: "4",
            title: "体检当前上游",
            action: MenuAction::Residential,
        },
        MenuItem {
            key: "5",
            title: "手动切换出口",
            action: MenuAction::Residential,
        },
        MenuItem {
            key: "6",
            title: "分流模式（global / 关键字）",
            action: MenuAction::Residential,
        },
        MenuItem {
            key: "7",
            title: "黑名单（查看 / 钉住 / 立即应用）",
            action: MenuAction::Residential,
        },
        // 文案要与「钉住域名」区分开：这两项钉的东西完全不同（spec §5.6）
        MenuItem {
            key: "8",
            title: "按槽查看住宅出口",
            action: MenuAction::Residential,
        },
        MenuItem {
            key: "9",
            title: "钉住某一槽的出口 IP",
            action: MenuAction::Residential,
        },
        MenuItem {
            key: "10",
            title: "按槽重排用户",
            action: MenuAction::Residential,
        },
        MenuItem {
            key: "11",
            title: "指定用户的槽位",
            action: MenuAction::Residential,
        },
        MenuItem {
            key: "0",
            title: "返回",
            action: MenuAction::Quit,
        },
    ]
}

/// 读一行（去首尾空白）；stdin 关闭时报错，由调用方当「返回」处理。
fn prompt(msg: &str) -> anyhow::Result<String> {
    use std::io::Write;
    print!("{msg}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    anyhow::ensure!(std::io::stdin().read_line(&mut line)? > 0, "stdin 已关闭");
    Ok(line.trim().to_string())
}

/// 住宅子菜单。整项需守护进程，进入前由 P1 的菜单派发已经挡过一次。
pub async fn menu(socket: PathBuf) -> anyhow::Result<()> {
    let items = menu_items();
    loop {
        println!("\n{}\n", crate::commands::menu::render_with(&items, true));
        let choice = match prompt("请选择: ") {
            Ok(c) => c,
            // stdin 关闭（管道里跑）：当返回
            Err(_) => return Ok(()),
        };
        let cmd = match choice.as_str() {
            "1" => ResidentialCmd::Status { json: false },
            "2" => {
                // 上游那一行含凭据：让运维粘贴进 stdin，不经 argv
                println!(
                    "粘贴供应商那一行（socks5://u:p@h:port、http://u:p@h:port、h:port:u:p 或 u:p@h:port），回车确认："
                );
                let url = prompt("")?;
                if url.is_empty() {
                    println!("未输入，已取消");
                    continue;
                }
                ResidentialCmd::Add { url }
            }
            "3" => {
                let target = prompt("要移除的上游（<uuid>、resi-N 或 <host:port>）: ")?;
                if target.is_empty() {
                    println!("未输入，已取消");
                    continue;
                }
                ResidentialCmd::Remove { target }
            }
            "4" => ResidentialCmd::Check { id: None },
            "5" => {
                // 本地不解析：resi-N / url-N / host:port / uuid 都交给服务端定位，
                // 定位不到时端点回的那条文案已经把可用写法列全了
                let target = prompt(
                    "切换到哪个上游（<uuid>、resi-N 或 <host:port>，见「住宅池状态」；auto = 解除手动锁定）: ",
                )?;
                if target.is_empty() {
                    println!("未输入，已取消");
                    continue;
                }
                if target == "auto" {
                    ResidentialCmd::Select {
                        target: None,
                        auto: true,
                    }
                } else {
                    ResidentialCmd::Select {
                        target: Some(target),
                        auto: false,
                    }
                }
            }
            "6" => {
                let on_off = prompt("分流模式（on = 全量走住宅 / off = 按关键字）: ")?;
                if on_off != "on" && on_off != "off" {
                    println!("只能填 on 或 off，实际 {on_off}");
                    continue;
                }
                ResidentialCmd::Global { on_off }
            }
            "7" => ResidentialCmd::Blacklist {
                cmd: BlacklistCmd::List { json: false },
            },
            "8" => ResidentialCmd::Slots { json: false },
            "9" => {
                let index = prompt("槽序号（见「按槽查看住宅出口」）: ")?;
                let Ok(index) = index.parse::<u16>() else {
                    println!("槽序号要填数字，实际 {index}");
                    continue;
                };
                // 本地不解析上游定位串：uuid / resi-N / url-N / host:port 都交给服务端
                let target = prompt("目标上游（<uuid>、resi-N 或 <host:port>，留空 = 解除）: ")?;
                if target.is_empty() {
                    ResidentialCmd::SlotPin {
                        index,
                        target: None,
                        auto: true,
                    }
                } else {
                    ResidentialCmd::SlotPin {
                        index,
                        target: Some(target),
                        auto: false,
                    }
                }
            }
            "10" => ResidentialCmd::Rebalance,
            "11" => {
                let user = prompt("用户名: ")?;
                if user.is_empty() {
                    println!("未输入，已取消");
                    continue;
                }
                let target = prompt("槽序号或上游（<槽序号>、<uuid>、resi-N 或 <host:port>）: ")?;
                if target.is_empty() {
                    println!("未输入，已取消");
                    continue;
                }
                ResidentialCmd::Assign { user, target }
            }
            "0" => return Ok(()),
            other => {
                println!("无效选择：{other}");
                continue;
            }
        };
        if let Err(e) = run(cmd, socket.clone()).await {
            println!("执行失败：{e:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use uuid::Uuid;

    /// 一条子命令期望翻译成的 `(method, path, body)`
    type Want = (&'static str, &'static str, Option<serde_json::Value>);

    #[test]
    fn every_subcommand_maps_to_one_endpoint_call() {
        let id = Uuid::from_u128(7);
        let cases: Vec<(ResidentialCmd, Want)> = vec![
            (
                ResidentialCmd::Status { json: false },
                ("GET", "/api/residential/status", None),
            ),
            (
                ResidentialCmd::Add {
                    url: "socks5://u:p@h:1".into(),
                },
                (
                    "POST",
                    "/api/residential/add",
                    Some(serde_json::json!({"url": "socks5://u:p@h:1"})),
                ),
            ),
            (
                ResidentialCmd::Remove {
                    target: "isp.example.net:10007".into(),
                },
                (
                    "POST",
                    "/api/residential/remove",
                    Some(serde_json::json!({"id": "isp.example.net:10007"})),
                ),
            ),
            (
                ResidentialCmd::Remove {
                    target: id.to_string(),
                },
                (
                    "POST",
                    "/api/residential/remove",
                    Some(serde_json::json!({"id": id.to_string()})),
                ),
            ),
            // 池内序号也照原样送上去，由服务端的 resolve_upstream 定位（CLI 不读 state）
            (
                ResidentialCmd::Remove {
                    target: "resi-3".into(),
                },
                (
                    "POST",
                    "/api/residential/remove",
                    Some(serde_json::json!({"id": "resi-3"})),
                ),
            ),
            (
                ResidentialCmd::Enable,
                (
                    "POST",
                    "/api/residential/enable",
                    Some(serde_json::json!({"enabled": true})),
                ),
            ),
            (
                ResidentialCmd::Disable,
                (
                    "POST",
                    "/api/residential/enable",
                    Some(serde_json::json!({"enabled": false})),
                ),
            ),
            (
                ResidentialCmd::Global {
                    on_off: "on".into(),
                },
                (
                    "POST",
                    "/api/residential/global",
                    Some(serde_json::json!({"global": true})),
                ),
            ),
            (
                ResidentialCmd::Global {
                    on_off: "off".into(),
                },
                (
                    "POST",
                    "/api/residential/global",
                    Some(serde_json::json!({"global": false})),
                ),
            ),
            (
                ResidentialCmd::Domains,
                ("GET", "/api/residential/status", None),
            ),
            (
                ResidentialCmd::RestoreDefault,
                ("POST", "/api/residential/restore-default", None),
            ),
            (
                ResidentialCmd::Check {
                    id: Some(id.to_string()),
                },
                (
                    "POST",
                    "/api/residential/check",
                    Some(serde_json::json!({"id": id.to_string()})),
                ),
            ),
            (
                ResidentialCmd::Check {
                    id: Some("url-2".into()),
                },
                (
                    "POST",
                    "/api/residential/check",
                    Some(serde_json::json!({"id": "url-2"})),
                ),
            ),
            (
                ResidentialCmd::Check { id: None },
                (
                    "POST",
                    "/api/residential/check",
                    Some(serde_json::json!({})),
                ),
            ),
            (
                ResidentialCmd::Select {
                    target: Some(id.to_string()),
                    auto: false,
                },
                (
                    "POST",
                    "/api/residential/select",
                    Some(serde_json::json!({"id": id.to_string()})),
                ),
            ),
            // `bui residential select resi-3`：以前在 clap 层就被 uuid 解析挡成
            // 「invalid character」，现在原样送去服务端定位
            (
                ResidentialCmd::Select {
                    target: Some("resi-3".into()),
                    auto: false,
                },
                (
                    "POST",
                    "/api/residential/select",
                    Some(serde_json::json!({"id": "resi-3"})),
                ),
            ),
            (
                // `--auto` = 解除手动锁定，回到自动选路（R2 ①）
                ResidentialCmd::Select {
                    target: None,
                    auto: true,
                },
                (
                    "POST",
                    "/api/residential/select",
                    Some(serde_json::json!({"auto": true})),
                ),
            ),
            (
                ResidentialCmd::Health { json: true },
                ("GET", "/api/residential/health", None),
            ),
        ];
        for (cmd, want) in cases {
            let (m, p, b) = to_request(&cmd).unwrap();
            assert_eq!(
                (m, p.as_str(), b),
                (want.0, want.1, want.2),
                "子命令 {cmd:?}"
            );
        }
    }

    #[test]
    fn blacklist_subcommands_map_too() {
        assert_eq!(
            blacklist_request(&BlacklistCmd::List { json: false }).unwrap(),
            ("GET", "/api/residential/blacklist".to_string(), None)
        );
        assert_eq!(
            blacklist_request(&BlacklistCmd::Pin {
                value: "pay.google.com".into(),
                kind: "domain_suffix".into(),
                note: "支付直连".into()
            })
            .unwrap(),
            (
                "POST",
                "/api/residential/blacklist/pins".to_string(),
                Some(
                    serde_json::json!({"kind": "domain_suffix", "value": "pay.google.com", "note": "支付直连"})
                )
            )
        );
        assert_eq!(
            blacklist_request(&BlacklistCmd::Unpin {
                value: "pay.google.com".into(),
                kind: "domain_suffix".into()
            })
            .unwrap()
            .0,
            "DELETE"
        );
        assert_eq!(
            blacklist_request(&BlacklistCmd::Apply).unwrap(),
            ("POST", "/api/residential/blacklist/apply".to_string(), None)
        );
        // select 既没 id 也没 --auto ⇒ CLI 侧就挡掉
        assert!(to_request(&ResidentialCmd::Select {
            target: None,
            auto: false
        })
        .is_err());
        // 非法 kind 在 CLI 侧就挡掉，不必往服务端跑一趟
        assert!(blacklist_request(&BlacklistCmd::Pin {
            value: ".*".into(),
            kind: "regex".into(),
            note: String::new()
        })
        .is_err());
        // `blacklist` 那一支必须整支转给 blacklist_request，不许自己另造路径
        assert_eq!(
            to_request(&ResidentialCmd::Blacklist {
                cmd: BlacklistCmd::Apply
            })
            .unwrap()
            .1,
            "/api/residential/blacklist/apply"
        );
    }

    /// `remove` 的回包渲染：三组各一行，每行一句后果，末了同一个下一步（重新获取订阅）。
    /// 空组不出现；三组全空时一个名字都不打。
    #[test]
    fn format_remove_tells_the_operator_who_must_refetch_the_subscription() {
        let v = |removed: &[&str], moved: &[&str], resliced: &[&str]| {
            serde_json::json!({"success": true, "port_changed": {
                "slot_removed": removed, "slot_moved": moved, "hop_resliced": resliced,
            }})
        };
        assert_eq!(
            format_remove(&v(&[], &[], &[])),
            "上游已移除（没有用户需要重新获取订阅）"
        );
        // 只有一组时也是「N 个用户……」+ 那一组一行
        assert_eq!(
            format_remove(&v(&[], &[], &["alice"])),
            "上游已移除。1 个用户手里那份订阅已不能照旧用，需要重新获取订阅：\n  \
             端口跳跃区间被重切（1 人，端口没变、连得上，但旧区间里划给别的槽的那一段会从\
             错误的出口 IP 出去）：alice"
        );
        // 三组齐全：顺序固定（组一 → 组二 → 组三），每组的后果都不一样
        assert_eq!(
            format_remove(&v(&["alice"], &["bob", "carol"], &["dave"])),
            "上游已移除。4 个用户手里那份订阅已不能照旧用，需要重新获取订阅：\n  \
             原槽位已删除、已换槽（1 人，旧端口不再通向他的槽：没人监听就连不上，被搬到 0 \
             号的那个槽顶替了就从别人的出口 IP 出去）：alice\n  \
             槽位序号被搬到 0 号（2 人，端口下移，旧端口无人监听，连不上）：bob、carol\n  \
             端口跳跃区间被重切（1 人，端口没变、连得上，但旧区间里划给别的槽的那一段会从\
             错误的出口 IP 出去）：dave"
        );
        // 没有 port_changed 字段的回包不该 panic
        assert_eq!(
            format_remove(&serde_json::json!({"success": true})),
            "上游已移除（没有用户需要重新获取订阅）"
        );
    }

    #[test]
    fn add_dash_reads_the_url_from_stdin_so_it_never_hits_argv() {
        let mut input = std::io::Cursor::new(b"socks5://user1:pw1@isp.example.net:1080\n".to_vec());
        assert_eq!(
            resolve_url("-", &mut input).unwrap(),
            "socks5://user1:pw1@isp.example.net:1080"
        );
        let mut empty = std::io::Cursor::new(Vec::new());
        assert!(resolve_url("-", &mut empty).is_err());
        let mut unused = std::io::Cursor::new(Vec::new());
        assert_eq!(
            resolve_url("http://u:p@h:1", &mut unused).unwrap(),
            "http://u:p@h:1"
        );
    }

    #[test]
    fn formatters_never_print_a_password_and_cover_the_key_fields() {
        let v = serde_json::json!({
            "enabled": true, "global": true,
            "urls": [{"host": "isp.example.net", "port": 10007, "name": "url-1", "type": "http",
                      "username": "user1", "lastVerifiedIp": "198.51.100.7", "id": "u-1",
                      "displayUrl": "http://us***@isp.example.net:10007"}],
            "upstreams": [{"id": "u-1", "google_ok": false}],
            "domains": ["openai.com"], "domainsFollowDefault": true,
            "active_tag": "resi-1", "active_upstream_id": "8d5a1a1e-3b2c-4d1e-9f00-0000000000bb",
            "selected_pending_persist": false,
            "blacklist": {"pins": 1, "auto": 2, "pending": 0, "candidates": 3},
            "alerts": ["全部住宅上游探测不达标"]
        });
        let out = format_status(&v);
        assert!(out.contains("已启用"));
        assert!(out.contains("global"));
        assert!(out.contains("url-1"));
        assert!(out.contains("198.51.100.7"));
        assert!(out.contains("跟随默认"));
        assert!(out.contains("resi-1"));
        assert!(out.contains("全部住宅上游探测不达标"));
        assert!(
            out.contains("Google 封"),
            "封 Google 的上游要在 status 里看得见：{out}"
        );
        assert!(!out.contains("pw1"), "凭据绝不进 CLI 输出");
        let h = serde_json::json!({
            "enabled": true, "mode": "selector", "selected": "resi-1", "domains_count": 67,
            "members": [{"tag": "resi-1", "type": "http", "host": "isp.example.net", "port": 10007,
                         "active": true, "failstreak": 0, "okstreak": 3, "priority": 10,
                         "success_rate_24h": 0.97, "blacklist_count": 2,
                         "manual_locked": true, "google_ok": true,
                         "egress": {"ip": "198.51.100.7", "type": "家庭宽带 IP", "isp": "AS33667 Comcast"}}],
            "current_egress_ip_test": "198.51.100.7", "egress_ip_type": "家庭宽带 IP", "alerts": []
        });
        let out = format_health(&h);
        assert!(out.contains("resi-1"));
        assert!(out.contains("家庭宽带 IP"));
        assert!(out.contains("97"), "成功率按百分比显示：{out}");
        assert!(out.contains("Google 通"), "{out}");
        assert!(out.contains("手动锁定"), "{out}");
        let b = serde_json::json!({
            "pins": [{"kind": "domain_suffix", "value": "pay.google.com", "note": "", "created_at": "2026-09-12T00:00:00Z"}],
            "auto": [{"upstream_id": "00000000-0000-0000-0000-000000000001", "upstream_name": "url-1",
                      "kind": "domain_suffix", "value": "gateway.icloud.com", "hits": 74,
                      "confirmed_at": "2026-09-12T00:00:00Z", "last_verified_at": "2026-09-12T00:00:00Z", "passes": 0}],
            "pending": [], "candidates": [], "notes": ["软封锁（上游返回 200 拦截页）无法自动识别，请手动钉住（pins）"]
        });
        let out = format_blacklist(&b);
        assert!(out.contains("pay.google.com"));
        assert!(out.contains("gateway.icloud.com"));
        assert!(out.contains("url-1"));
        assert!(
            out.contains("软封锁"),
            "局限说明要出现在 CLI 输出里（spec §5.4）"
        );
    }

    /// 真机：三个 Decodo 上游各自学到同一批支付域名，旧渲染逐条平铺、不写上游，
    /// 看着像重复三遍。按上游分组：组标题写名字 / 槽位 / host:port，组内按 host:port 去重计数。
    #[test]
    fn pending_and_candidates_are_grouped_by_upstream_and_deduped_within_one() {
        let ups = [
            (
                "00000000-0000-0000-0000-00000000000a",
                "url-1",
                0,
                "isp1.example.net:10001",
            ),
            (
                "00000000-0000-0000-0000-00000000000b",
                "url-2",
                1,
                "isp2.example.net:10002",
            ),
            (
                "00000000-0000-0000-0000-00000000000c",
                "url-3",
                2,
                "isp3.example.net:10003",
            ),
        ];
        let row = |u: &(&str, &str, u64, &str), host: &str| {
            serde_json::json!({"upstream_id": u.0, "upstream_name": u.1, "upstream_slot": u.2,
                "upstream_addr": u.3, "host": host, "port": 443, "confirms": 2, "hits": 5,
                "last_confirm_at": "2026-09-12T00:00:00Z", "last_seen": "2026-09-12T00:00:00Z"})
        };
        let mut pending = Vec::new();
        for u in &ups {
            pending.push(row(u, "api.stripe.com"));
        }
        // 同一上游内重复一条 ⇒ 只列一次、带计数
        pending.push(row(&ups[0], "api.stripe.com"));
        let cands: Vec<_> = ups.iter().map(|u| row(u, "pay.google.com")).collect();
        let out = format_blacklist(&serde_json::json!({
            "pins": [], "auto": [], "pending": pending, "candidates": cands, "notes": []
        }));
        for (_, name, slot, addr) in &ups {
            let title = format!("上游 {name}（槽 {slot}，{addr}）");
            assert_eq!(
                out.matches(&title).count(),
                2,
                "待生效与候选各一个组标题：{out}"
            );
        }
        assert_eq!(out.matches("api.stripe.com:443").count(), 3, "{out}");
        assert_eq!(out.matches("pay.google.com:443").count(), 3, "{out}");
        assert!(out.contains("×2"), "组内重复要计数：{out}");
        // 段标题仍报原始条数（与 status 摘要同一口径）
        assert!(out.contains("待生效 4 条"), "{out}");
        assert!(out.contains("候选 3 条"), "{out}");
        // status 摘要行不受影响
        let st = format_status(&serde_json::json!({
            "blacklist": {"pins": 0, "auto": 0, "pending": 4, "candidates": 3}
        }));
        assert!(
            st.contains("黑名单：钉住 0 / 自动 0 / 待生效 4 / 候选 3"),
            "{st}"
        );
    }

    /// status / health 都要有一行「UDP 经住宅」，运维一眼看出池里混了 http 上游的代价。
    #[test]
    fn the_formatters_spell_out_whether_udp_goes_through_the_pool() {
        let line = |yes: bool| {
            format!(
                "UDP 经住宅：{}（池内含 http 上游时为否）",
                if yes { "是" } else { "否" }
            )
        };
        let s = format_status(&serde_json::json!({"udp_via_residential": true}));
        assert!(s.contains(&line(true)), "{s}");
        let s = format_status(&serde_json::json!({"udp_via_residential": false}));
        assert!(s.contains(&line(false)), "{s}");
        let hh = format_health(&serde_json::json!({"udp_via_residential": true}));
        assert!(hh.contains(&line(true)), "{hh}");
        let hh = format_health(&serde_json::json!({"udp_via_residential": false}));
        assert!(hh.contains(&line(false)), "{hh}");
    }

    #[test]
    fn rebalance_and_assign_map_to_their_endpoints() {
        assert_eq!(
            to_request(&ResidentialCmd::Rebalance).unwrap(),
            ("POST", "/api/residential/rebalance".to_string(), None)
        );
        let (m, p, b) = to_request(&ResidentialCmd::Assign {
            user: "alice".into(),
            target: "resi-2".into(),
        })
        .unwrap();
        assert_eq!((m, p.as_str()), ("POST", "/api/residential/assign"));
        assert_eq!(
            b.unwrap(),
            serde_json::json!({"user": "alice", "target": "resi-2"})
        );
    }

    #[test]
    fn the_menu_lists_twelve_items_and_the_p1_menu_gains_one() {
        let items = menu_items();
        assert_eq!(items.len(), 12);
        assert_eq!(items.last().unwrap().action, MenuAction::Quit);
        assert!(items.iter().any(|i| i.title.contains("黑名单")));
        // 两处「钉住」的文案必须能区分开：一个钉域名，一个钉槽位的出口 IP（spec §5.6）
        assert!(items.iter().any(|i| i.title.contains("按槽查看住宅出口")));
        assert!(items.iter().any(|i| i.title == "钉住某一槽的出口 IP"));
        // P1 的主菜单必须有住宅入口
        let p1 = crate::commands::menu::items();
        assert!(
            p1.iter().any(|i| i.action == MenuAction::Residential),
            "P1 Task 17 的 items() 里要有住宅项：{p1:?}"
        );
    }

    #[tokio::test]
    async fn a_missing_daemon_gives_an_actionable_error_instead_of_touching_state() {
        let d = tempfile::tempdir().unwrap();
        let e = run(
            ResidentialCmd::Status { json: false },
            d.path().join("absent.sock"),
        )
        .await
        .unwrap_err();
        assert!(e.to_string().contains("systemctl start b-ui"), "实际 {e}");
    }

    #[test]
    fn the_cli_parses_the_residential_subcommands() {
        use clap::Parser;
        let cli = crate::cli::Cli::try_parse_from([
            "bui",
            "residential",
            "blacklist",
            "pin",
            "pay.google.com",
            "--note",
            "支付直连",
        ])
        .unwrap();
        assert_eq!(
            cli.command,
            Some(crate::cli::Command::Residential {
                cmd: ResidentialCmd::Blacklist {
                    cmd: BlacklistCmd::Pin {
                        value: "pay.google.com".into(),
                        kind: "domain_suffix".into(),
                        note: "支付直连".into(),
                    }
                }
            })
        );
        // 凭据经 stdin：`add -`
        assert_eq!(
            crate::cli::Cli::try_parse_from(["bui", "residential", "add", "-"])
                .unwrap()
                .command,
            Some(crate::cli::Command::Residential {
                cmd: ResidentialCmd::Add { url: "-".into() }
            })
        );
        // global 只认 on/off
        assert!(crate::cli::Cli::try_parse_from(["bui", "residential", "global", "yes"]).is_err());
        // `select resi-3` / `check --id url-3` 必须能过 clap（以前 Uuid 解析直接报
        // 「invalid character」，运维照着 status 里的名字敲就用不了）
        assert_eq!(
            crate::cli::Cli::try_parse_from(["bui", "residential", "select", "resi-3"])
                .unwrap()
                .command,
            Some(crate::cli::Command::Residential {
                cmd: ResidentialCmd::Select {
                    target: Some("resi-3".into()),
                    auto: false
                }
            })
        );
        assert_eq!(
            crate::cli::Cli::try_parse_from(["bui", "residential", "check", "--id", "url-3"])
                .unwrap()
                .command,
            Some(crate::cli::Command::Residential {
                cmd: ResidentialCmd::Check {
                    id: Some("url-3".into())
                }
            })
        );
        // select 的两种形态：<target> 与 --auto
        assert_eq!(
            crate::cli::Cli::try_parse_from(["bui", "residential", "select", "--auto"])
                .unwrap()
                .command,
            Some(crate::cli::Command::Residential {
                cmd: ResidentialCmd::Select {
                    target: None,
                    auto: true
                }
            })
        );
        let id = Uuid::from_u128(7);
        assert_eq!(
            crate::cli::Cli::try_parse_from(["bui", "residential", "select", &id.to_string()])
                .unwrap()
                .command,
            Some(crate::cli::Command::Residential {
                cmd: ResidentialCmd::Select {
                    target: Some(id.to_string()),
                    auto: false
                }
            })
        );
    }

    #[test]
    fn slots_and_slot_pin_map_to_their_endpoints() {
        assert_eq!(
            to_request(&ResidentialCmd::Slots { json: true }).unwrap(),
            ("GET", "/api/residential/slots".to_string(), None)
        );
        let (m, p, b) = to_request(&ResidentialCmd::SlotPin {
            index: 2,
            target: Some("resi-3".into()),
            auto: false,
        })
        .unwrap();
        assert_eq!((m, p.as_str()), ("POST", "/api/residential/slots/pin"));
        assert_eq!(b.unwrap(), serde_json::json!({"index": 2, "id": "resi-3"}));
        let (_, _, b) = to_request(&ResidentialCmd::SlotPin {
            index: 0,
            target: None,
            auto: true,
        })
        .unwrap();
        assert_eq!(b.unwrap(), serde_json::json!({"index": 0, "auto": true}));
        assert!(to_request(&ResidentialCmd::SlotPin {
            index: 0,
            target: Some("resi-1".into()),
            auto: true
        })
        .is_err());
        assert!(to_request(&ResidentialCmd::SlotPin {
            index: 0,
            target: None,
            auto: false
        })
        .is_err());
        // 黑名单的 `pin` 还在原来的位置，没被劫持
        assert_eq!(
            to_request(&ResidentialCmd::Blacklist {
                cmd: BlacklistCmd::Pin {
                    value: "pay.google.com".into(),
                    kind: "domain_suffix".into(),
                    note: String::new(),
                },
            })
            .unwrap()
            .1,
            "/api/residential/blacklist/pins"
        );
    }
}
