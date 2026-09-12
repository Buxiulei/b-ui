//! 数字两列菜单：渲染与输入解析（无箭头、无 ANSI、CJK 按 2 列对齐）。
//!
//! 渲染输出不含 ANSI 颜色：颜色会让快照测试变脆，可读性靠对齐与 `●`/`○`/`★` 够用。

use crate::profiles::{Mode, Profiles};
use crate::{Error, Result};
use std::collections::VecDeque;

/// 两列菜单左栏的列宽（按 [`display_width`] 计）。
pub const LEFT_WIDTH: usize = 14;

/// 交互输入：真实终端用 [`Stdin`]，测试与 `--yes` 路径用 [`Scripted`]。
pub trait Prompt {
    fn line(&mut self, prompt: &str) -> Result<String>;
    /// 批量粘贴：空行结束。
    fn lines_until_blank(&mut self, prompt: &str) -> Result<Vec<String>>;
    /// 只有 `y` / `yes`（忽略大小写）算是。
    fn confirm(&mut self, prompt: &str) -> Result<bool>;
}

pub struct Stdin;

impl Prompt for Stdin {
    fn line(&mut self, prompt: &str) -> Result<String> {
        use std::io::Write as _;
        print!("  ▸ {prompt}: ");
        std::io::stdout()
            .flush()
            .map_err(|e| Error::io(std::path::Path::new("<stdout>"), e))?;
        let mut buf = String::new();
        if std::io::stdin()
            .read_line(&mut buf)
            .map_err(|e| Error::io(std::path::Path::new("<stdin>"), e))?
            == 0
        {
            // EOF：返回空串，调用方按「取消」处理
            return Ok(String::new());
        }
        Ok(buf.trim().to_string())
    }

    fn lines_until_blank(&mut self, prompt: &str) -> Result<Vec<String>> {
        println!("  {prompt}（每行一个，空行结束）");
        let mut out = Vec::new();
        loop {
            let l = self.line("")?;
            if l.is_empty() {
                return Ok(out);
            }
            out.push(l);
        }
    }

    fn confirm(&mut self, prompt: &str) -> Result<bool> {
        let a = self.line(&format!("{prompt} [y/N]"))?;
        Ok(matches!(a.to_ascii_lowercase().as_str(), "y" | "yes"))
    }
}

/// 脚本化输入：测试用，也给 `--yes` 路径喂固定答案。队列空了当 EOF（空串 / 否 / 空列表）。
pub struct Scripted {
    pub queue: VecDeque<String>,
}

impl<'a, const N: usize> From<[&'a str; N]> for Scripted {
    fn from(v: [&'a str; N]) -> Self {
        Self {
            queue: v.iter().map(|s| s.to_string()).collect(),
        }
    }
}

impl Prompt for Scripted {
    fn line(&mut self, _prompt: &str) -> Result<String> {
        Ok(self.queue.pop_front().unwrap_or_default())
    }

    fn lines_until_blank(&mut self, _prompt: &str) -> Result<Vec<String>> {
        let mut out = Vec::new();
        while let Some(l) = self.queue.pop_front() {
            if l.is_empty() {
                break;
            }
            out.push(l);
        }
        Ok(out)
    }

    fn confirm(&mut self, _prompt: &str) -> Result<bool> {
        Ok(matches!(
            self.line("")?.to_ascii_lowercase().as_str(),
            "y" | "yes"
        ))
    }
}

/// 主菜单的一次选择。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    SwitchNode,
    ToggleMode,
    ImportNode,
    Service,
    Check,
    Update,
    ImportV3,
    Uninstall,
    AutoUpdate,
    Quit,
}

/// 渲染主菜单要的全部事实，由调用方（T12）从 `profiles.json` 与 systemd 探测组装。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub node: String,
    pub label: String,
    pub mode: Mode,
    pub service_running: bool,
    pub tun_up: bool,
    pub socks_port: u16,
    pub http_port: u16,
    pub update_available: bool,
    /// `[9]` 每日自动更新的开关状态（spec §6「可关」）。
    pub auto_update: bool,
}

/// 终端列宽：CJK 与全角标点按 2 列（菜单对齐只需要这个精度，不引 unicode-width）。
pub fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| {
            let cp = u32::from(c);
            let wide = (0x1100..=0x115F).contains(&cp)
                || (0x2E80..=0xA4CF).contains(&cp)
                || (0xAC00..=0xD7A3).contains(&cp)
                || (0xF900..=0xFAFF).contains(&cp)
                || (0xFE30..=0xFE6F).contains(&cp)
                || (0xFF00..=0xFF60).contains(&cp)
                || (0xFFE0..=0xFFE6).contains(&cp)
                || (0x1F300..=0x1FAFF).contains(&cp)
                || cp == 0x2605
                || cp == 0x2606;
            if wide {
                2
            } else {
                1
            }
        })
        .sum()
}

/// 右补空格到 `width` 列；超宽不截断（宁可破格，也不切半个字）。
pub fn pad(s: &str, width: usize) -> String {
    let w = display_width(s);
    if w >= width {
        return s.to_string();
    }
    format!("{s}{}", " ".repeat(width - w))
}

fn row(n1: &str, l1: &str, n2: &str, l2: &str) -> String {
    format!("     [{n1}] {}  [{n2}] {l2}\n", pad(l1, LEFT_WIDTH))
}

/// 标题条 + 三行状态 + 两列数字菜单。
pub fn render(st: &Status) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "\n  ─────  B-UI 客户端 · v{}  ──────────────────────\n\n",
        crate::VERSION
    ));
    let node = if st.node.is_empty() {
        "(未设置)".to_string()
    } else {
        format!("{}  {}", st.node, st.label)
    };
    let svc = if st.service_running {
        "●  运行中"
    } else {
        "○  已停止"
    };
    out.push_str(&format!("   节点   {svc}  {node}\n"));
    out.push_str(&format!(
        "   代理      SOCKS5 :{}   HTTP :{}\n",
        st.socks_port, st.http_port
    ));
    let mode = match st.mode {
        Mode::Tun => {
            if st.tun_up {
                "TUN   ●  运行中"
            } else {
                "TUN   ○  未就绪"
            }
        }
        Mode::Socks => "SOCKS ●  本地入口",
    };
    out.push_str(&format!("   模式   {mode}\n\n"));

    let update = if st.update_available {
        "检查更新 ★ 有新版"
    } else {
        "检查更新"
    };
    out.push_str(&row("1", "切换节点", "2", "切换模式"));
    out.push_str(&row("3", "导入节点", "4", "服务控制"));
    out.push_str(&row("5", "连接检查", "6", update));
    out.push_str(&row("7", "从 v3 导入", "8", "卸载"));
    // 第 9 项单独一行：spec §6 的「每日自动更新，可关」需要一个用户能点的开关
    out.push_str(&format!(
        "     [9] 自动更新 {}\n",
        if st.auto_update { "开" } else { "关" }
    ));
    out.push_str("     ─────────\n");
    out.push_str("     [0] 退出\n");
    out
}

/// 编号节点列表，当前节点带 `★`。
pub fn render_nodes(prof: &Profiles) -> String {
    if prof.profiles.is_empty() {
        return "  没有节点，先导入（主菜单 3）\n".to_string();
    }
    let mut out = String::new();
    for (i, p) in prof.profiles.iter().enumerate() {
        let mark = if prof.active.as_deref() == Some(p.name.as_str()) {
            " ★"
        } else {
            ""
        };
        out.push_str(&format!(
            "  [{}] {}  {}{}\n",
            i + 1,
            pad(&p.name, 26),
            p.node.label,
            mark
        ));
    }
    out.push_str("  [0] 返回\n");
    out
}

/// 主菜单只认数字，别的一律 `None`（调用方重画菜单）。
pub fn parse_choice(input: &str) -> Option<Action> {
    match input.trim() {
        "1" => Some(Action::SwitchNode),
        "2" => Some(Action::ToggleMode),
        "3" => Some(Action::ImportNode),
        "4" => Some(Action::Service),
        "5" => Some(Action::Check),
        "6" => Some(Action::Update),
        "7" => Some(Action::ImportV3),
        "8" => Some(Action::Uninstall),
        "9" => Some(Action::AutoUpdate),
        "0" => Some(Action::Quit),
        _ => None,
    }
}

/// `"2"` → `Some(1)`；`0` / 空 / 非数字 / 越界 → `None`（`0` 是「返回」）。
pub fn pick_index(input: &str, len: usize) -> Option<usize> {
    let n: usize = input.trim().parse().ok()?;
    if n == 0 || n > len {
        return None;
    }
    Some(n - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::{Mode, Profile, Profiles, Source};
    use crate::testutil::{hy2_direct_node, reality_direct_node, split_global};
    use pretty_assertions::assert_eq;

    fn st() -> Status {
        Status {
            node: "alice-hy2-direct".into(),
            label: "HY2直连".into(),
            mode: Mode::Tun,
            service_running: true,
            tun_up: true,
            socks_port: 1080,
            http_port: 8080,
            update_available: false,
            auto_update: true,
        }
    }

    #[test]
    fn width_counts_cjk_as_two_columns() {
        assert_eq!(display_width("abc"), 3);
        assert_eq!(display_width("切换节点"), 8);
        // "HY2" 三个半角 + "直连" 两个全角 = 3 + 4
        assert_eq!(display_width("HY2直连"), 7);
        assert_eq!(display_width("★"), 2);
    }

    #[test]
    fn pad_aligns_by_display_width_and_never_truncates() {
        assert_eq!(pad("切换节点", 14), "切换节点      ");
        assert_eq!(display_width(&pad("切换节点", 14)), 14);
        assert_eq!(display_width(&pad("abc", 14)), 14);
        assert_eq!(
            pad("这一项特别长的标签文字", 4),
            "这一项特别长的标签文字",
            "不截断，宁可破格"
        );
    }

    #[test]
    fn menu_is_two_columns_of_numbers_with_zero_to_quit() {
        let out = render(&st());
        assert!(out.contains("[1] 切换节点"));
        assert!(out.contains("[2] 切换模式"));
        assert!(out.contains("[3] 导入节点"));
        assert!(out.contains("[4] 服务控制"));
        assert!(out.contains("[5] 连接检查"));
        assert!(out.contains("[6] 检查更新"));
        assert!(out.contains("[7] 从 v3 导入"));
        assert!(out.contains("[8] 卸载"));
        assert!(out.contains("[9] 自动更新"));
        assert!(out.contains("[0] 退出"));
        // 每个选项行恰好两栏
        let rows: Vec<&str> = out
            .lines()
            .filter(|l| {
                l.contains("[1]") || l.contains("[3]") || l.contains("[5]") || l.contains("[7]")
            })
            .collect();
        assert_eq!(rows.len(), 4);
        for r in rows {
            assert_eq!(r.matches('[').count(), 2, "两列：{r}");
        }
        assert!(!out.contains('\u{1b}'), "不含 ANSI 转义（快照稳定）");
        assert!(!out.contains("↑") && !out.contains("↓"), "不做箭头菜单");
    }

    #[test]
    fn status_lines_show_node_mode_and_ports() {
        let out = render(&st());
        assert!(out.contains("alice-hy2-direct"));
        assert!(out.contains("HY2直连"));
        assert!(out.contains("SOCKS5 :1080"));
        assert!(out.contains("HTTP :8080"));
        assert!(out.contains("TUN"));
        assert!(out.contains(crate::VERSION));
        let mut s2 = st();
        s2.node = String::new();
        s2.service_running = false;
        s2.tun_up = false;
        s2.mode = Mode::Socks;
        let out2 = render(&s2);
        assert!(out2.contains("(未设置)"));
        assert!(out2.contains("已停止"));
        assert!(out2.contains("SOCKS"));
    }

    #[test]
    fn auto_update_row_reflects_the_switch() {
        assert!(render(&st()).contains("[9] 自动更新 开"));
        let mut s = st();
        s.auto_update = false;
        assert!(render(&s).contains("[9] 自动更新 关"));
    }

    #[test]
    fn update_marker_shows_only_when_available() {
        assert!(!render(&st()).contains("★ 有新版"));
        let mut s = st();
        s.update_available = true;
        assert!(render(&s).contains("★ 有新版"));
    }

    #[test]
    fn node_list_is_numbered_and_marks_current() {
        let mut p = Profiles::new_default();
        for (n, node) in [
            ("alice-hy2-direct", hy2_direct_node()),
            ("alice-reality-direct", reality_direct_node()),
        ] {
            p.upsert(Profile {
                name: n.into(),
                node,
                split: split_global(),
                source: Source::ApiNodes,
                imported_at: "2026-09-11T00:00:00Z".into(),
            });
        }
        p.active = Some("alice-reality-direct".into());
        let out = render_nodes(&p);
        assert!(out.contains("[1] alice-hy2-direct"));
        assert!(out.contains("[2] alice-reality-direct"));
        assert!(out
            .lines()
            .find(|l| l.contains("[2]"))
            .unwrap()
            .contains('★'));
        assert!(
            out.contains("HY2直连") && out.contains("Reality直连"),
            "显示 label 便于辨认"
        );
        assert!(render_nodes(&Profiles::new_default()).contains("没有节点"));
    }

    #[test]
    fn choices_map_to_actions() {
        assert_eq!(parse_choice("1"), Some(Action::SwitchNode));
        assert_eq!(parse_choice(" 2 "), Some(Action::ToggleMode));
        assert_eq!(parse_choice("7"), Some(Action::ImportV3));
        assert_eq!(parse_choice("9"), Some(Action::AutoUpdate));
        assert_eq!(parse_choice("0"), Some(Action::Quit));
        assert_eq!(parse_choice("10"), None);
        assert_eq!(parse_choice(""), None);
        assert_eq!(
            parse_choice("q"),
            None,
            "只认数字（数字菜单，不认字母快捷键）"
        );
    }

    #[test]
    fn pick_index_is_one_based_and_bounded() {
        assert_eq!(pick_index("1", 3), Some(0));
        assert_eq!(pick_index("3", 3), Some(2));
        assert_eq!(pick_index("4", 3), None);
        assert_eq!(pick_index("0", 3), None);
        assert_eq!(pick_index("", 3), None);
        assert_eq!(pick_index("abc", 3), None);
    }

    #[test]
    fn scripted_prompt_feeds_lines_and_confirms() {
        let mut p = Scripted::from(["1", "y", "n", "uri-a", "uri-b", ""]);
        assert_eq!(p.line("选择").unwrap(), "1");
        assert!(p.confirm("确定？").unwrap());
        assert!(!p.confirm("确定？").unwrap());
        assert_eq!(
            p.lines_until_blank("粘贴").unwrap(),
            vec!["uri-a".to_string(), "uri-b".to_string()]
        );
        // 队列空了 → 当成 EOF：返回空串，调用方按「取消」处理
        assert_eq!(p.line("选择").unwrap(), "");
    }
}
