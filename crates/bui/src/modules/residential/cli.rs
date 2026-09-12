//! `bui residential <子命令>` 与住宅子菜单（spec §2.4、§5）。
//!
//! **全部**子命令经 `/run/b-ui.sock` 调本模块自己的 HTTP 端点（spec §2.4「所有菜单项
//! 通过 unix socket 调守护进程 API」），不在 CLI 进程里直接碰 `state` —— 否则会与守护
//! 进程的 `Store` 并发写同一个 `state.json`。
//!
//! 子命令 → 端点的翻译是**纯函数**（[`to_request`] / [`blacklist_request`]），因此
//! 全部路由与载荷都能在不起守护进程的情况下单测。

use crate::commands::menu::{MenuAction, MenuItem};
use std::path::PathBuf;
use uuid::Uuid;

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
    /// 删一条上游（`<id>` 或 `<host:port>`）
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
    /// 体检（不带 `--id` 则体检当前选中的上游）
    Check {
        #[arg(long)]
        id: Option<Uuid>,
    },
    /// 手动切换当前出口
    Select { id: Uuid },
    /// 巡检与出口画像
    Health {
        #[arg(long)]
        json: bool,
    },
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
        C::Remove { target } => {
            // 先按 uuid 解，不是 uuid 就当 host:port（与面板两种定位方式一致）
            let body = match Uuid::parse_str(target) {
                Ok(id) => serde_json::json!({"id": id}),
                Err(_) => serde_json::json!({"host_port": target}),
            };
            ("POST", "/api/residential/remove".into(), Some(body))
        }
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
        C::Select { id } => (
            "POST",
            "/api/residential/select".into(),
            Some(serde_json::json!({"id": id})),
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

/// 百分比（`success_rate_24h` 是 0.0–1.0）
fn pct(v: &serde_json::Value, k: &str) -> i64 {
    (v.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0) * 100.0).round() as i64
}

/// `status` 的人类可读渲染。**只读**传入的 JSON 字段，绝不打印 `password`（服务端也不回它）。
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
    if as_bool(v, "selected_pending_persist") {
        out.push("  自动切换后的落点尚未持久化，将在每日 04:00 写回".to_string());
    }
    let urls = as_arr(v, "urls");
    out.push(format!("上游 {} 条：", urls.len()));
    for u in urls {
        out.push(format!(
            "  - {} [{}] {}:{} 出口 {} 优先级 {} {}",
            as_str(u, "name"),
            as_str(u, "type"),
            as_str(u, "host"),
            as_u64(u, "port"),
            as_str(u, "lastVerifiedIp"),
            as_u64(u, "priority"),
            as_str(u, "displayUrl"),
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
            "{} {} [{}] {}:{} {} 连续成功 {} / 连续失败 {} 24h 成功率 {}% 优先级 {} 出口 {}（{}，{}）黑名单 {} 条",
            if tag == selected { "*" } else { " " },
            tag,
            as_str(m, "type"),
            as_str(m, "host"),
            as_u64(m, "port"),
            if as_bool(m, "active") { "健康" } else { "不健康" },
            as_u64(m, "okstreak"),
            as_u64(m, "failstreak"),
            pct(m, "success_rate_24h"),
            as_u64(m, "priority"),
            as_str(&e, "ip"),
            as_str(&e, "type"),
            as_str(&e, "isp"),
            as_u64(m, "blacklist_count"),
        ));
    }
    for n in as_arr(v, "notes") {
        out.push(format!("说明：{}", n.as_str().unwrap_or_default()));
    }
    for a in as_arr(v, "alerts") {
        out.push(format!("告警：{}", a.as_str().unwrap_or_default()));
    }
    out.join("\n")
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
    for a in auto {
        out.push(format!(
            "  - [{}] {} 上游 {} 命中 {} 复核通过 {} 确认于 {}",
            as_str(a, "kind"),
            as_str(a, "value"),
            as_str(a, "upstream_name"),
            as_u64(a, "hits"),
            as_u64(a, "passes"),
            as_str(a, "confirmed_at"),
        ));
    }
    let pending = as_arr(v, "pending");
    out.push(format!(
        "待生效 {} 条（下一个 04:00 窗口批量写入）：",
        pending.len()
    ));
    for p in pending {
        out.push(format!(
            "  - {}:{} 已确认 {} 次，最近 {}",
            as_str(p, "host"),
            as_u64(p, "port"),
            as_u64(p, "confirms"),
            as_str(p, "last_confirm_at"),
        ));
    }
    let cands = as_arr(v, "candidates");
    out.push(format!("候选 {} 条：", cands.len()));
    for c in cands {
        out.push(format!(
            "  - {}:{} 被拒 {} 次，最近 {}",
            as_str(c, "host"),
            as_u64(c, "port"),
            as_u64(c, "hits"),
            as_str(c, "last_seen"),
        ));
    }
    for n in as_arr(v, "notes") {
        out.push(format!("说明：{}", n.as_str().unwrap_or_default()));
    }
    out.join("\n")
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
        // 供脚本消费：只打印生效关键字数组（等价于 v3 `residential-helper.sh domains`）
        ResidentialCmd::Domains => println!(
            "{}",
            v.get("domains").cloned().unwrap_or(serde_json::json!([]))
        ),
        _ => println!("{v}"),
    }
    Ok(())
}

/// 住宅子菜单的 8 项（两列渲染沿用 P1 的 [`crate::commands::menu::render_with`]）。
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
                let target = prompt("要移除的上游（<id> 或 <host:port>）: ")?;
                if target.is_empty() {
                    println!("未输入，已取消");
                    continue;
                }
                ResidentialCmd::Remove { target }
            }
            "4" => ResidentialCmd::Check { id: None },
            "5" => {
                let raw = prompt("切换到哪个上游（<id>，见「住宅池状态」）: ")?;
                match Uuid::parse_str(&raw) {
                    Ok(id) => ResidentialCmd::Select { id },
                    Err(e) => {
                        println!("不是合法的上游 id：{e}");
                        continue;
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
                    Some(serde_json::json!({"host_port": "isp.example.net:10007"})),
                ),
            ),
            (
                ResidentialCmd::Remove {
                    target: id.to_string(),
                },
                (
                    "POST",
                    "/api/residential/remove",
                    Some(serde_json::json!({"id": id})),
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
                ResidentialCmd::Check { id: Some(id) },
                (
                    "POST",
                    "/api/residential/check",
                    Some(serde_json::json!({"id": id})),
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
                ResidentialCmd::Select { id },
                (
                    "POST",
                    "/api/residential/select",
                    Some(serde_json::json!({"id": id})),
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
                      "username": "user1", "lastVerifiedIp": "198.51.100.7",
                      "displayUrl": "http://us***@isp.example.net:10007"}],
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
        assert!(!out.contains("pw1"), "凭据绝不进 CLI 输出");
        let h = serde_json::json!({
            "enabled": true, "mode": "selector", "selected": "resi-1", "domains_count": 67,
            "members": [{"tag": "resi-1", "type": "http", "host": "isp.example.net", "port": 10007,
                         "active": true, "failstreak": 0, "okstreak": 3, "priority": 10,
                         "success_rate_24h": 0.97, "blacklist_count": 2,
                         "egress": {"ip": "198.51.100.7", "type": "家庭宽带 IP", "isp": "AS33667 Comcast"}}],
            "current_egress_ip_test": "198.51.100.7", "egress_ip_type": "家庭宽带 IP", "alerts": []
        });
        let out = format_health(&h);
        assert!(out.contains("resi-1"));
        assert!(out.contains("家庭宽带 IP"));
        assert!(out.contains("97"), "成功率按百分比显示：{out}");
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

    #[test]
    fn the_menu_lists_eight_items_and_the_p1_menu_gains_one() {
        let items = menu_items();
        assert_eq!(items.len(), 8);
        assert_eq!(items.last().unwrap().action, MenuAction::Quit);
        assert!(items.iter().any(|i| i.title.contains("黑名单")));
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
    }
}
