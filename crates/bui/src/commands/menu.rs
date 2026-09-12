//! `sudo b-ui` 的数字菜单（spec §1、§2.4）。
//!
//! 菜单自己**不动手**：状态 / 对账 / 重启数据面都经 unix socket 调守护进程的 API
//! （`/api/health`、`/api/reconcile`、`/api/services/{unit}/{action}`），于是三条对账路径
//! 仍然只有守护进程里那一个 consumer 在跑（S6）。
//!
//! 守护进程没跑时（首装、或 `b-ui.service` 挂了）仍保留六项：状态 / 对账 / 对账并清理漂移 /
//! 升级 / SSH 硬化 / 退出——对账两项退化为进程内跑（与 `serve::reconcile_cli` 同一条路径），
//! `harden-ssh` 放行的理由见 Task 7。只有「重启数据面」「查看日志」与「住宅出口」（P3）
//! 标「(需守护进程)」。

use crate::sys::Host;
use anyhow::Result;
use bui_schema::paths::Paths;
use std::path::PathBuf;
use std::sync::Arc;

/// 数据面单元：菜单第 4 项按这个顺序逐个 restart（`b-ui` 与 `caddy` 不在内）。
pub const DATA_PLANE_UNITS: [&str; 4] = [
    "hysteria-server",
    "hysteria-residential",
    "xray",
    "b-ui-relay",
];

/// 「查看日志」默认看守护进程自己的 journal。
pub const LOG_UNIT: &str = "b-ui";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuItem {
    pub key: &'static str,
    pub title: &'static str,
    pub action: MenuAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuAction {
    Status,
    Reconcile,
    ReconcileForce,
    /// 对数据面的 systemd 动作（`restart` / `stop` / `start`）
    Service(&'static str),
    Logs,
    Upgrade,
    HardenSsh,
    /// 住宅出口子菜单（P3；整项需守护进程）
    Residential,
    Quit,
}

pub fn items() -> Vec<MenuItem> {
    vec![
        MenuItem {
            key: "1",
            title: "状态与体检",
            action: MenuAction::Status,
        },
        MenuItem {
            key: "2",
            title: "立即对账",
            action: MenuAction::Reconcile,
        },
        MenuItem {
            key: "3",
            title: "对账并清理漂移",
            action: MenuAction::ReconcileForce,
        },
        MenuItem {
            key: "4",
            title: "重启数据面（两个 hysteria + xray + relay）",
            action: MenuAction::Service("restart"),
        },
        MenuItem {
            key: "5",
            title: "查看日志",
            action: MenuAction::Logs,
        },
        MenuItem {
            key: "6",
            title: "升级 / 回滚",
            action: MenuAction::Upgrade,
        },
        MenuItem {
            key: "7",
            title: "SSH 硬化",
            action: MenuAction::HardenSsh,
        },
        MenuItem {
            key: "8",
            title: "住宅出口（池 / 体检 / 黑名单）",
            action: MenuAction::Residential,
        },
        MenuItem {
            key: "0",
            title: "退出",
            action: MenuAction::Quit,
        },
    ]
}

/// 只认列出的 key（去掉首尾空白）；其余一律 `None`，由调用方提示重输。
pub fn parse_choice(input: &str, items: &[MenuItem]) -> Option<MenuAction> {
    let key = input.trim();
    items
        .iter()
        .find(|i| i.key == key)
        .map(|i| i.action.clone())
}

/// 两列数字菜单（守护进程在跑的版本）。
pub fn render(items: &[MenuItem]) -> String {
    render_with(items, true)
}

/// 终端显示宽度：CJK / 全角标点占两格，其余按一格。左列补空格要按这个数，
/// 否则中文标题按 `chars().count()` 算出来的左列宽度会短一半，右列参差不齐。
fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| match c as u32 {
            // CJK 统一汉字、中日韩符号与标点、全角 ASCII、汉字扩展区
            0x1100..=0x115F
            | 0x2E80..=0x303E
            | 0x3041..=0x33FF
            | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xA000..=0xA4CF
            | 0xAC00..=0xD7A3
            | 0xF900..=0xFAFF
            | 0xFE30..=0xFE6F
            | 0xFF00..=0xFF60
            | 0xFFE0..=0xFFE6
            | 0x20000..=0x3FFFD => 2,
            _ => 1,
        })
        .sum()
}

/// `daemon_up = false` 时给需要守护进程的项加「(需守护进程)」后缀。
///
/// 后缀**紧跟标题、不带空格**：两列排版下一行放两项，标注若写在行尾就分不清是左项还是右项的。
pub fn render_with(items: &[MenuItem], daemon_up: bool) -> String {
    let cell = |i: &MenuItem| -> String {
        let needs = matches!(
            i.action,
            MenuAction::Service(_) | MenuAction::Logs | MenuAction::Residential
        );
        let suffix = if !daemon_up && needs {
            "(需守护进程)"
        } else {
            ""
        };
        format!("{}) {}{}", i.key, i.title, suffix)
    };
    let cells: Vec<String> = items.iter().map(cell).collect();
    // 左列宽度按加了后缀的单元格算（按终端显示宽度），保证右列对齐且不换行
    let left_width = cells
        .iter()
        .step_by(2)
        .map(|c| display_width(c))
        .max()
        .unwrap_or(0);
    let mut out = vec!["b-ui v4 控制台".to_string()];
    if !daemon_up {
        out.push("守护进程未运行，仅可用 1/2/3/6/7/0".to_string());
    }
    for pair in cells.chunks(2) {
        match pair {
            [l, r] => {
                let pad = left_width.saturating_sub(display_width(l));
                out.push(format!("  {l}{}   {r}", " ".repeat(pad)));
            }
            [l] => out.push(format!("  {l}")),
            _ => {}
        }
    }
    out.join("\n")
}

pub async fn run(paths: Paths, host: Arc<dyn Host>) -> Result<()> {
    run_with(paths, host, PathBuf::from(crate::paths::SOCKET_PATH)).await
}

/// socket 路径由调用方传入（与 install / reconcile 同一口径：单元测试不碰真实 socket）。
pub async fn run_with(paths: Paths, host: Arc<dyn Host>, socket: PathBuf) -> Result<()> {
    let items = items();
    loop {
        let client = crate::ipc::Client::new(&socket);
        let up = client.available().await;
        // 守护进程在跑就是 `render`（等于 `render_with(.., true)`），没跑才显式传 false
        let text = if up {
            render(&items)
        } else {
            render_with(&items, false)
        };
        println!("\n{text}\n");
        print!("请选择: ");
        use std::io::Write;
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line)? == 0 {
            return Ok(()); // stdin 关闭（管道里跑）：当退出
        }
        let Some(action) = parse_choice(&line, &items) else {
            println!("无效选择：{}", line.trim());
            continue;
        };
        if action == MenuAction::Quit {
            return Ok(());
        }
        if let Err(e) = dispatch(&action, &paths, &host, &socket, up).await {
            println!("执行失败：{e:#}");
        }
    }
}

async fn dispatch(
    action: &MenuAction,
    paths: &Paths,
    host: &Arc<dyn Host>,
    socket: &PathBuf,
    daemon_up: bool,
) -> Result<()> {
    let client = crate::ipc::Client::new(socket);
    match action {
        MenuAction::Status => {
            crate::commands::status::run(false, paths.clone(), host.clone()).await
        }
        MenuAction::Reconcile | MenuAction::ReconcileForce => {
            let force = action == &MenuAction::ReconcileForce;
            // socket 可用走 API，否则进程内跑一次（两支都在 reconcile_cli 里）
            crate::serve::reconcile_cli(paths.clone(), host.clone(), socket.clone(), force, false)
                .await
        }
        MenuAction::Service(verb) => {
            if !daemon_up {
                anyhow::bail!("守护进程未运行，无法经 API 动单元；请先 `systemctl start b-ui`");
            }
            for unit in DATA_PLANE_UNITS {
                let (status, body) = client
                    .request("POST", &format!("/api/services/{unit}/{verb}"), None)
                    .await?;
                println!("{unit} {verb}: HTTP {status} {body}");
            }
            Ok(())
        }
        MenuAction::Logs => {
            if !daemon_up {
                anyhow::bail!("守护进程未运行；可直接跑 `journalctl -u b-ui -n 200 --no-pager`");
            }
            let h = host.clone();
            let out = tokio::task::spawn_blocking(move || {
                h.run("journalctl", &["-u", LOG_UNIT, "-n", "200", "--no-pager"])
            })
            .await??;
            println!("{}", out.stdout);
            if !out.stderr.is_empty() {
                println!("{}", out.stderr);
            }
            Ok(())
        }
        MenuAction::Upgrade => {
            crate::commands::upgrade::run(false, None, None, paths.clone(), host.clone()).await
        }
        MenuAction::HardenSsh => {
            crate::commands::harden_ssh::run(paths.clone(), host.clone()).await
        }
        MenuAction::Residential => {
            // 与 4/5 同处理：整项标「(需守护进程)」，socket 不可用时拒绝进入
            if !daemon_up {
                anyhow::bail!(
                    "守护进程未运行，住宅出口全部经 API 操作；请先 `systemctl start b-ui`"
                );
            }
            crate::modules::residential::cli::menu(socket.clone()).await
        }
        MenuAction::Quit => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn menu_keys_are_stable_and_unique() {
        let items = items();
        let keys: Vec<&str> = items.iter().map(|i| i.key).collect();
        assert_eq!(keys, vec!["1", "2", "3", "4", "5", "6", "7", "8", "0"]);
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), keys.len());
        assert_eq!(items.last().unwrap().action, MenuAction::Quit);
    }

    #[test]
    fn parse_choice_accepts_only_listed_keys() {
        let items = items();
        assert_eq!(parse_choice("1", &items), Some(MenuAction::Status));
        assert_eq!(parse_choice(" 2 \n", &items), Some(MenuAction::Reconcile));
        assert_eq!(parse_choice("0", &items), Some(MenuAction::Quit));
        assert_eq!(parse_choice("99", &items), None);
        assert_eq!(parse_choice("", &items), None);
    }

    #[test]
    fn render_is_two_columns_and_mentions_every_item() {
        let items = items();
        let text = render(&items);
        for i in &items {
            assert!(text.contains(i.title), "菜单缺 {}", i.title);
        }
        let body: Vec<&str> = text
            .lines()
            .filter(|l| l.trim_start().starts_with(|c: char| c.is_ascii_digit()))
            .collect();
        assert_eq!(body.len(), items.len().div_ceil(2), "两列排版");
    }

    #[test]
    fn items_needing_the_daemon_are_marked_when_it_is_down() {
        // spec §2.4 的白名单在本计划里扩了一项 harden-ssh（理由见 Task 7 的「为什么 harden-ssh 放行」）：
        // 守护进程没跑时可用 install / upgrade / status / reconcile / harden-ssh
        let items = items();
        let up = render_with(&items, true);
        let down = render_with(&items, false);
        assert_eq!(up, render(&items), "render 就是 render_with(.., true)");
        assert!(!up.contains("需守护进程"));
        for i in &items {
            let needs_daemon = matches!(
                i.action,
                MenuAction::Service(_) | MenuAction::Logs | MenuAction::Residential
            );
            let line = down.lines().find(|l| l.contains(i.title)).unwrap();
            // 标注**紧跟标题**（`标题(需守护进程)`），不是行尾：两列排版下一行有两项，
            // 断言「这一行里有没有标注」会把同行邻居的标注算到自己头上（4/5 同行时 3 必假阳）
            assert_eq!(
                line.contains(&format!("{}(需守护进程)", i.title)),
                needs_daemon,
                "{} 的标注不对：{line}",
                i.title
            );
        }
    }
}
