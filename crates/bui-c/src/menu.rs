//! 数字两列菜单：渲染与输入解析（无箭头、无 ANSI、CJK 按 2 列对齐）。
//!
//! 渲染输出不含 ANSI 颜色：颜色会让快照测试变脆，可读性靠对齐与 `●`/`○`/`★` 够用。

use crate::profiles::{kind_slug, Mode, Profile, Profiles};
use crate::{Error, Result};
use std::collections::VecDeque;

/// 两列菜单左栏的列宽（按 [`display_width`] 计）。
pub const LEFT_WIDTH: usize = 14;

/// 节点列表里名字列的封顶列宽：再长就破格，不拖着所有行一起变宽。
pub const NAME_CAP: usize = 40;

/// 选项块分隔线的列宽 = 两列选项的宽度：`[n] ` 4 + 左栏 LEFT_WIDTH + 栏距 2 +
/// 右栏 `[2] 切到 SOCKS` 14。写死 9 格的短线看着像断了。
const RULE_WIDTH: usize = 4 + LEFT_WIDTH + 2 + 14;

/// 交互输入：真实终端用 [`Stdin`]，测试与 `--yes` 路径用 [`Scripted`]。
pub trait Prompt {
    fn line(&mut self, prompt: &str) -> Result<String>;
    /// 批量粘贴：空行结束。
    fn lines_until_blank(&mut self, prompt: &str) -> Result<Vec<String>>;
    /// 只有 `y` / `yes`（忽略大小写）算是。
    fn confirm(&mut self, prompt: &str) -> Result<bool>;
}

pub struct Stdin;

/// 输入提示符。空提示只打 `▸`（粘贴导入逐行读时不能每行挂个孤零零的冒号），
/// 非空用全角冒号，与其余中文文案一致。
fn prompt_text(prompt: &str) -> String {
    if prompt.is_empty() {
        "  ▸ ".to_string()
    } else {
        format!("  ▸ {prompt}：")
    }
}

/// 读一行（去首尾空白）；EOF（0 字节）返回 `None`。
///
/// 按字节读到 `\n` 再有损转码：`read_line` 遇到非 UTF-8 字节（GBK 终端、误触的控制键）
/// 会整个报错，菜单就带着「stream did not contain valid UTF-8」崩出去了。坏字节换成
/// U+FFFD 后，主菜单把它当成一次「无效选项」。
fn read_line_from<R: std::io::BufRead>(r: &mut R) -> Result<Option<String>> {
    let mut buf = Vec::new();
    let n = r
        .read_until(b'\n', &mut buf)
        .map_err(|e| Error::io(std::path::Path::new("<stdin>"), e))?;
    if n == 0 {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&buf).trim().to_string()))
}

impl Prompt for Stdin {
    fn line(&mut self, prompt: &str) -> Result<String> {
        use std::io::Write as _;
        print!("{}", prompt_text(prompt));
        std::io::stdout()
            .flush()
            .map_err(|e| Error::io(std::path::Path::new("<stdout>"), e))?;
        // EOF：返回空串，调用方按「取消」处理
        Ok(read_line_from(&mut std::io::stdin().lock())?.unwrap_or_default())
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
    let mut out = render_status(st);
    out.push_str(&render_options(st));
    out
}

/// 只有标题条与节点 / 代理 / 模式三行：一次性 `bui-c status` 用它，不打菜单块
/// （命令行里 `[1] 切换节点` 无处可点）。
pub fn render_status(st: &Status) -> String {
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
    // 两种模式的状态平行给：SOCKS 以前恒亮「● 本地入口」，服务停了也照亮，是假绿灯
    let mode = match st.mode {
        Mode::Tun => {
            if st.tun_up {
                "TUN   ●  运行中".to_string()
            } else {
                "TUN   ○  未就绪".to_string()
            }
        }
        Mode::Socks => {
            if st.service_running {
                format!(
                    "SOCKS ●  运行中（本地 :{}/:{}）",
                    st.socks_port, st.http_port
                )
            } else {
                "SOCKS ○  已停止".to_string()
            }
        }
    };
    out.push_str(&format!("   模式   {mode}\n\n"));
    out
}

/// 两列数字菜单块：`[1]`~`[9]` + 分隔线 + `[0] 退出`。
pub fn render_options(st: &Status) -> String {
    let mut out = String::new();
    let update = if st.update_available {
        "检查更新 ★ 有新版"
    } else {
        "检查更新"
    };
    // [2] 直接写目标模式：「切换模式」不说切到哪边，用户得自己跟状态行对
    let to_mode = match st.mode {
        Mode::Tun => "切到 SOCKS",
        Mode::Socks => "切到 TUN",
    };
    out.push_str(&row("1", "切换节点", "2", to_mode));
    out.push_str(&row("3", "导入节点", "4", "服务控制"));
    out.push_str(&row("5", "连接检查", "6", update));
    out.push_str(&row("7", "从 v3 导入", "8", "卸载"));
    // 第 9 项单独一行：spec §6 的「每日自动更新，可关」需要一个用户能点的开关
    out.push_str(&format!(
        "     [9] 自动更新 {}\n",
        if st.auto_update { "开" } else { "关" }
    ));
    out.push_str(&format!("     {}\n", "\u{2500}".repeat(RULE_WIDTH)));
    out.push_str("     [0] 退出\n");
    out
}

/// 编号节点列表，当前节点带 `★`。
///
/// `with_back`：菜单里选节点要能 `[0] 返回`，一次性 `bui-c list` 没有可返回的地方。
pub fn render_nodes(prof: &Profiles, with_back: bool) -> String {
    if prof.profiles.is_empty() {
        return if with_back {
            "  没有节点，先导入（主菜单 3）\n".to_string()
        } else {
            "  没有节点，先 `bui-c import …`\n".to_string()
        };
    }
    // 列宽跟着本次列表最宽的那个走：真机上名字从 3 列（v3 目录名）到 38 列都有，
    // 写死 26 会让长名字挤掉后面所有列。超过 NAME_CAP 的名字原样输出（破格），
    // 后面只留两个空格——宁可一行歪，也不让所有行为它变宽。
    let width = |f: fn(&Profile) -> &str, cap: usize| {
        prof.profiles
            .iter()
            .map(|p| display_width(f(p)))
            .max()
            .unwrap_or(0)
            .min(cap)
    };
    let name_w = width(|p| p.name.as_str(), NAME_CAP);
    let label_w = width(|p| p.node.label.as_str(), usize::MAX);
    let kind_w = width(|p| kind_slug(p.node.kind), usize::MAX);
    let mut out = String::new();
    for (i, p) in prof.profiles.iter().enumerate() {
        let mark = if prof.active.as_deref() == Some(p.name.as_str()) {
            " ★"
        } else {
            ""
        };
        out.push_str(&format!(
            "  [{}] {}  {}  {}  {}:{}{}\n",
            i + 1,
            pad(&p.name, name_w),
            pad(&p.node.label, label_w),
            pad(kind_slug(p.node.kind), kind_w),
            p.node.host,
            p.node.port,
            mark
        ));
    }
    if with_back {
        out.push_str("  [0] 返回\n");
    }
    out
}

/// 去空白 + 把全角数字（U+FF10..=U+FF19）折成 ASCII。
///
/// 中文输入法下 `１` 是常见误触：真机截屏里就被判成了「无效选项」。
fn normalize_digits(input: &str) -> String {
    input
        .trim()
        .chars()
        .map(|c| match u32::from(c) {
            cp @ 0xFF10..=0xFF19 => char::from_digit(cp - 0xFF10, 10).unwrap_or(c),
            _ => c,
        })
        .collect()
}

/// 主菜单只认数字，别的一律 `None`（调用方重画菜单）。
pub fn parse_choice(input: &str) -> Option<Action> {
    match normalize_digits(input).as_str() {
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
    let n: usize = normalize_digits(input).parse().ok()?;
    if n == 0 || n > len {
        return None;
    }
    Some(n - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::{Mode, Profile, Profiles, Source};
    use crate::testutil::{hy2_direct_node, hy2_resi_node, reality_direct_node, split_global};
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
        assert!(out.contains("[2] 切到 SOCKS"));
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
    fn options_rule_is_as_wide_as_the_two_columns() {
        let out = render_options(&st());
        let rule = out.lines().find(|l| l.contains('─')).unwrap();
        let row1 = out.lines().find(|l| l.contains("[1]")).unwrap();
        assert_eq!(
            display_width(rule.trim_start()),
            display_width(row1.trim_start()),
            "分隔线要跟两列选项等宽\n{out}"
        );
        assert_eq!(display_width(rule.trim_start()), 34, "{rule:?}");
        assert_eq!(
            rule.len() - rule.trim_start().len(),
            row1.len() - row1.trim_start().len(),
            "缩进也要一致"
        );
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
    fn mode_option_names_the_target_mode() {
        let mut s = st(); // Tun
        let tun = render_options(&s);
        assert!(tun.contains("[2] 切到 SOCKS"), "{tun}");
        assert!(!tun.contains("切换模式"), "别让用户猜切到哪边：{tun}");
        s.mode = Mode::Socks;
        assert!(render_options(&s).contains("[2] 切到 TUN"));
    }

    /// 状态块里的「模式」那一行。
    fn mode_line(st: &Status) -> String {
        render_status(st)
            .lines()
            .find(|l| l.contains("模式"))
            .unwrap()
            .to_string()
    }

    #[test]
    fn socks_mode_line_follows_the_service_like_tun_does() {
        let mut s = st();
        s.mode = Mode::Socks;
        s.tun_up = false;
        s.socks_port = 1081;
        s.http_port = 8081;
        s.service_running = true;
        let up = mode_line(&s);
        assert!(
            up.contains("SOCKS ●  运行中（本地 :1081/:8081）"),
            "端口从 Status 取：{up}"
        );
        s.service_running = false;
        let down = mode_line(&s);
        assert!(down.contains("SOCKS ○  已停止"), "{down}");
        assert!(!down.contains('●'), "服务停了不能还亮着：{down}");

        // TUN 分支保持「运行中 / 未就绪」
        let mut t = st();
        assert!(mode_line(&t).contains("TUN   ●  运行中"));
        t.tun_up = false;
        assert!(mode_line(&t).contains("TUN   ○  未就绪"));
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
        let out = render_nodes(&p, true);
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
        assert!(render_nodes(&Profiles::new_default(), true).contains("没有节点"));
    }

    #[test]
    fn render_splits_into_status_and_options() {
        let s = st();
        let status = render_status(&s);
        let options = render_options(&s);
        assert_eq!(
            render(&s),
            format!("{status}{options}"),
            "render = 两者相接"
        );
        assert!(status.contains("alice-hy2-direct"), "状态块留标题条与三行");
        assert!(status.contains(crate::VERSION));
        assert!(!status.contains("[1] "), "状态块不带菜单：{status}");
        assert!(!status.contains("[0] 退出"));
        assert!(options.contains("[1] 切换节点"));
        assert!(options.contains("[0] 退出"));
    }

    #[test]
    fn node_list_back_row_is_optional() {
        let mut p = Profiles::new_default();
        p.upsert(Profile {
            name: "alice-hy2-direct".into(),
            node: hy2_direct_node(),
            split: split_global(),
            source: Source::ApiNodes,
            imported_at: "2026-09-11T00:00:00Z".into(),
        });
        assert!(
            render_nodes(&p, true).contains("[0] 返回"),
            "菜单里要能返回"
        );
        assert!(
            !render_nodes(&p, false).contains("[0] 返回"),
            "一次性 list 没有可返回的地方"
        );
        // 空列表的引导按场景给：菜单里指菜单项，命令行里指命令
        assert!(render_nodes(&Profiles::new_default(), true).contains("主菜单 3"));
        let empty = render_nodes(&Profiles::new_default(), false);
        assert!(empty.contains("bui-c import"), "{empty}");
        assert!(!empty.contains("主菜单"), "{empty}");
    }

    /// 行里 `label` 之前占了多少列——用来断言各行的 label 起始列一致。
    fn label_col(line: &str, label: &str) -> usize {
        let i = line
            .find(label)
            .unwrap_or_else(|| panic!("行里没有 {label}：{line}"));
        display_width(&line[..i])
    }

    fn named(name: &str, node: bui_schema::nodes::Node) -> Profile {
        Profile {
            name: name.into(),
            node,
            split: split_global(),
            source: Source::ApiNodes,
            imported_at: "2026-09-11T00:00:00Z".into(),
        }
    }

    fn reality_resi_node() -> bui_schema::nodes::Node {
        bui_schema::nodes::Node {
            kind: bui_schema::nodes::NodeKind::RealityResidential,
            label: "Reality住宅".into(),
            port: 10002,
            ..reality_direct_node()
        }
    }

    #[test]
    fn node_list_columns_align_and_show_kind_and_endpoint() {
        // 真机上的混排：38 列的长名字 + 3 列的 v3 目录名
        let long = "rick-node.example-a.net-reality-direct";
        assert_eq!(display_width(long), 38, "样例得是 38 列");
        let mut p = Profiles::new_default();
        for (n, node) in [
            (long, reality_direct_node()),
            ("HY2", hy2_direct_node()),
            ("reality-Reality", reality_resi_node()),
            ("hysteria2-1778329470", hy2_resi_node()),
        ] {
            p.upsert(named(n, node));
        }
        let out = render_nodes(&p, true);
        let rows: Vec<&str> = out.lines().filter(|l| !l.contains("[0]")).collect();
        assert_eq!(rows.len(), 4, "{out}");

        // label 列起始列必须一致（截屏里四行各自起始列不同就是这条）
        let cols: Vec<usize> = rows
            .iter()
            .zip(["Reality直连", "HY2直连", "Reality住宅", "HY2住宅"])
            .map(|(l, lb)| label_col(l, lb))
            .collect();
        assert!(
            cols.windows(2).all(|w| w[0] == w[1]),
            "label 起始列 {cols:?}\n{out}"
        );

        // 每行多一列 kind + host:port
        for (l, kind) in
            rows.iter()
                .zip(["reality-direct", "hy2-direct", "reality-resi", "hy2-resi"])
        {
            assert!(l.contains(kind), "{l}");
            assert!(l.contains("panel.example.com:"), "{l}");
        }
        assert!(rows[1].contains("panel.example.com:10000"), "{}", rows[1]);
        assert!(rows[3].contains("panel.example.com:40000"), "{}", rows[3]);
    }

    #[test]
    fn node_name_column_follows_the_widest_name_and_caps_at_40() {
        let mut narrow = Profiles::new_default();
        narrow.upsert(named("HY2", hy2_direct_node()));
        let line = render_nodes(&narrow, false);
        assert!(
            line.starts_with("  [1] HY2  HY2直连"),
            "短名字不再补到写死的 26 列：{line:?}"
        );

        // 超过 40 列的名字原样输出，后面只留两个空格
        let huge = "x".repeat(45);
        let mut wide = Profiles::new_default();
        wide.upsert(named(&huge, hy2_direct_node()));
        wide.upsert(named("HY2", reality_direct_node()));
        let out = render_nodes(&wide, false);
        let rows: Vec<&str> = out.lines().collect();
        assert!(rows[0].contains(&format!("{huge}  HY2直连")), "{}", rows[0]);
        assert_eq!(
            label_col(rows[1], "Reality直连"),
            6 + 40 + 2,
            "其余名字补到封顶的 40 列：{}",
            rows[1]
        );
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
    fn fullwidth_digits_are_folded_to_ascii() {
        // 中文输入法下 `１` 是常见误触，别让它掉进「无效选项」
        assert_eq!(parse_choice("１"), Some(Action::SwitchNode));
        assert_eq!(parse_choice(" ９ "), Some(Action::AutoUpdate));
        assert_eq!(parse_choice("０"), Some(Action::Quit));
        assert_eq!(parse_choice("１０"), None, "折完还是越界");
        assert_eq!(pick_index("２", 3), Some(1));
        assert_eq!(pick_index("３", 3), Some(2));
        assert_eq!(pick_index("４", 3), None);
        assert_eq!(pick_index("０", 3), None);
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
    fn stdin_prompt_marker_uses_fullwidth_colon_and_none_when_empty() {
        assert_eq!(prompt_text(""), "  ▸ ", "粘贴导入时每行不能显示成「▸ :」");
        assert_eq!(prompt_text("选择 [0-9]"), "  ▸ 选择 [0-9]：");
        assert!(
            !prompt_text("选择节点编号").contains(':'),
            "与其余文案一致用全角冒号"
        );
    }

    #[test]
    fn read_line_from_replaces_invalid_utf8_instead_of_failing() {
        // 真机：`printf '\xff\xfe\n0\n' | sudo bui-c` 以前直接崩出菜单
        // （「读写 <stdin> 失败：stream did not contain valid UTF-8」）
        let mut r = std::io::Cursor::new(b"\xff\xfe\n0\n".to_vec());
        let got = read_line_from(&mut r).unwrap();
        assert_eq!(
            got.as_deref(),
            Some("\u{FFFD}\u{FFFD}"),
            "坏字节换成 U+FFFD"
        );
        assert_eq!(
            parse_choice(got.as_deref().unwrap()),
            None,
            "主菜单按「无效选项」处理，而不是退出"
        );
        assert_eq!(
            read_line_from(&mut r).unwrap().as_deref(),
            Some("0"),
            "后面的行照常读"
        );
        assert_eq!(read_line_from(&mut r).unwrap(), None, "读完是 EOF");
    }

    #[test]
    fn read_line_from_trims_and_reports_eof_as_none() {
        let mut empty = std::io::Cursor::new(Vec::<u8>::new());
        assert_eq!(read_line_from(&mut empty).unwrap(), None);
        let mut r = std::io::Cursor::new("  １ \r\n\n最后一行没换行".as_bytes().to_vec());
        assert_eq!(read_line_from(&mut r).unwrap().as_deref(), Some("１"));
        assert_eq!(
            read_line_from(&mut r).unwrap().as_deref(),
            Some(""),
            "空行是 Some(\"\")，不是 EOF"
        );
        assert_eq!(
            read_line_from(&mut r).unwrap().as_deref(),
            Some("最后一行没换行")
        );
        assert_eq!(read_line_from(&mut r).unwrap(), None);
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
