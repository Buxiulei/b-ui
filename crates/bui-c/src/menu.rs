//! 数字两列菜单：渲染与输入解析（无箭头、无 ANSI、CJK 按 2 列对齐）。
//!
//! 渲染输出不含 ANSI 颜色：颜色会让快照测试变脆，可读性靠对齐与 `●`/`○`/`★` 够用。

use crate::profiles::{kind_slug, Mode, Profiles};
use crate::{Error, Result};
use std::collections::VecDeque;

/// 两列菜单左栏的列宽（按 [`display_width`] 计）。
pub const LEFT_WIDTH: usize = 14;

/// 交互输入：真实终端用 [`Stdin`]，测试与 `--yes` 路径用 [`Scripted`]。
pub trait Prompt {
    /// 读一行（去首尾空白）；EOF 返回 `None`。主菜单靠它区分「回车」（重画）与「EOF」（退出）。
    fn read(&mut self, prompt: &str) -> Result<Option<String>>;
    /// 读一行，EOF 当空串：子提示里两者都是「取消 / 返回」。
    fn line(&mut self, prompt: &str) -> Result<String> {
        Ok(self.read(prompt)?.unwrap_or_default())
    }
    /// 批量粘贴：空行结束。
    fn lines_until_blank(&mut self, prompt: &str) -> Result<Vec<String>>;
    /// 只有 `y` / `yes`（忽略大小写）算是。
    fn confirm(&mut self, prompt: &str) -> Result<bool>;
    /// 停下来等一个回车（EOF 也算）：长输出打完别让菜单重画把它顶出屏幕。
    fn pause(&mut self, prompt: &str) -> Result<()> {
        self.read(prompt).map(|_| ())
    }
    /// 有人在终端前看提示吗。`Stdin` 在 stdin 不是终端（管道 / 重定向）时为假：
    /// 不打「▸」提示，菜单因 EOF 退出时也不必补换行。
    fn interactive(&self) -> bool {
        true
    }
}

/// 把 `s` 整段写进 `w` 并 flush。stdout 的写出都经它：`print!` 遇到被关掉的管道
/// （`bui-c list | head -1`）会 panic，这里把 `BrokenPipe` 交还给调用方处理。
pub fn write_out<W: std::io::Write>(w: &mut W, s: &str) -> std::io::Result<()> {
    w.write_all(s.as_bytes())?;
    w.flush()
}

/// 打一段交互提示。stdin 不是终端时一个字都不写（没人在看，管道输出里只该有结果）；
/// stdout 被关返回 `Ok(false)`，调用方当 EOF 收场。
fn prompt_out<W: std::io::Write>(interactive: bool, w: &mut W, text: &str) -> Result<bool> {
    if !interactive {
        return Ok(true);
    }
    match write_out(w, text) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(false),
        Err(e) => Err(Error::io(std::path::Path::new("<stdout>"), e)),
    }
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

/// [`Prompt::pause`] 的提示：不是在问问题，不挂冒号。
fn pause_text(prompt: &str) -> String {
    format!("  ▸ {prompt}")
}

/// [`Prompt::lines_until_blank`] 在终端里打的那一行说明（含 2 列缩进，不含换行）。
pub fn paste_head(prompt: &str) -> String {
    format!("  {prompt}（每行一个，空行结束）")
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

impl Stdin {
    /// 打出提示（不换行）再读一行；stdout 已关就当 EOF。
    fn ask(&mut self, text: &str) -> Result<Option<String>> {
        if !prompt_out(self.interactive(), &mut std::io::stdout().lock(), text)? {
            return Ok(None);
        }
        read_line_from(&mut std::io::stdin().lock())
    }
}

impl Prompt for Stdin {
    fn read(&mut self, prompt: &str) -> Result<Option<String>> {
        self.ask(&prompt_text(prompt))
    }

    fn pause(&mut self, prompt: &str) -> Result<()> {
        self.ask(&pause_text(prompt)).map(|_| ())
    }

    fn lines_until_blank(&mut self, prompt: &str) -> Result<Vec<String>> {
        let head = format!("{}\n", paste_head(prompt));
        if !prompt_out(self.interactive(), &mut std::io::stdout().lock(), &head)? {
            return Ok(Vec::new());
        }
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

    fn interactive(&self) -> bool {
        use std::io::IsTerminal as _;
        std::io::stdin().is_terminal()
    }
}

/// 脚本化输入：测试用，也给 `--yes` 路径喂固定答案。队列空了当 EOF（`read` 为 `None`，
/// `line` 为空串 / 否 / 空列表）。
pub struct Scripted {
    pub queue: VecDeque<String>,
    /// 按顺序记下被问过的每条提示（`read` / `line` / `confirm` / `lines_until_blank`），
    /// 测试靠它断言提示文案、以及「某一问根本没出现」。
    pub asked: Vec<String>,
}

impl<'a, const N: usize> From<[&'a str; N]> for Scripted {
    fn from(v: [&'a str; N]) -> Self {
        Self {
            queue: v.iter().map(|s| s.to_string()).collect(),
            asked: Vec::new(),
        }
    }
}

impl Prompt for Scripted {
    fn read(&mut self, prompt: &str) -> Result<Option<String>> {
        self.asked.push(prompt.to_string());
        Ok(self.queue.pop_front())
    }

    fn lines_until_blank(&mut self, prompt: &str) -> Result<Vec<String>> {
        self.asked.push(prompt.to_string());
        let mut out = Vec::new();
        while let Some(l) = self.queue.pop_front() {
            if l.is_empty() {
                break;
            }
            out.push(l);
        }
        Ok(out)
    }

    fn confirm(&mut self, prompt: &str) -> Result<bool> {
        Ok(matches!(
            self.line(prompt)?.to_ascii_lowercase().as_str(),
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
        // 端口不在这里重复：「代理」那一行已经写了（TUN 模式下 1080/8080 也在监听）
        Mode::Socks => {
            if st.service_running {
                "SOCKS ●  运行中".to_string()
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
    let rows = [
        row("1", "切换节点", "2", to_mode),
        row("3", "导入节点", "4", "服务控制"),
        row("5", "连接检查", "6", update),
        row("7", "从 v3 导入", "8", "卸载"),
        // 第 9 项单独一行：spec §6 的「每日自动更新，可关」需要一个用户能点的开关
        format!(
            "     [9] 自动更新 {}\n",
            if st.auto_update { "开" } else { "关" }
        ),
    ];
    // 分隔线跟实际最宽的那行选项等宽：[2] 的目标模式与 [6] 的「★ 有新版」都会改变行宽，
    // 写死的线要么比选项长、要么短一截
    let widest = rows
        .iter()
        .map(|r| display_width(r.trim_end()))
        .max()
        .unwrap_or(0);
    for r in &rows {
        out.push_str(r);
    }
    out.push_str(&format!(
        "     {}\n",
        "\u{2500}".repeat(widest.saturating_sub(5))
    ));
    out.push_str("     [0] 退出\n");
    out
}

/// 节点列表：每个节点两行，当前节点名字前带 `★`。
///
/// ```text
///      [1]   HY2
///            示例专用名-HY2住宅  hy2-resi  tizi.example.test:40000
///      [2] ★ hysteria2-1778329470
///            示例专用名  hy2-direct  tizi.example.test:10000
/// ```
///
/// 以前一个节点一行、四列对齐，真机上每行 113–119 列，100 列终端整屏折行、★ 被折到下一行。
/// 拆成两行后第二行不做列对齐——对齐就会被最长的 label / host 撑宽。
/// 编号行与主菜单选项同为 5 列缩进；第二行缩进到名字起始列（`★ ` 按终端里的 2 列算，
/// 与两个空格同宽）。
///
/// `with_back`：菜单里选节点要打编号与 `[0] 返回`；一次性 `bui-c list` 不打编号
/// （`bui-c switch` 只认名字），第一行形如 `  ★ name` / `    name`。
pub fn render_nodes(prof: &Profiles, with_back: bool) -> String {
    if prof.profiles.is_empty() {
        return if with_back {
            "  没有节点，先用 [3] 导入节点\n".to_string()
        } else {
            "  没有节点，先 `bui-c import …`\n".to_string()
        };
    }
    // 两位数编号时补齐 `[n]`，名字仍然对齐在同一列
    let num_w = format!("[{}]", prof.profiles.len()).len();
    let mut out = String::new();
    for (i, p) in prof.profiles.iter().enumerate() {
        let mark = if prof.active.as_deref() == Some(p.name.as_str()) {
            "★ "
        } else {
            "  "
        };
        let lead = if with_back {
            format!("     {} ", pad(&format!("[{}]", i + 1), num_w))
        } else {
            "  ".to_string()
        };
        let indent = " ".repeat(lead.len() + 2);
        out.push_str(&format!("{lead}{mark}{}\n", p.name));
        out.push_str(&format!(
            "{indent}{}  {}  {}:{}\n",
            p.node.label,
            kind_slug(p.node.kind),
            p.node.host,
            p.node.port
        ));
    }
    if with_back {
        out.push_str("     [0] 返回\n");
    }
    out
}

/// 菜单 `[1] 切换节点` 的一屏：前空一行、两列缩进的标题，再接带编号的 [`render_nodes`]——
/// 与 [`render_service_options`] 同一个样式。一次性 `bui-c list` 不用它，不加标题。
pub fn render_node_picker(prof: &Profiles) -> String {
    format!("\n  切换节点\n{}", render_nodes(prof, true))
}

/// `[4] 服务控制` 的二级菜单里的一次选择。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceAction {
    Restart,
    Logs,
    Back,
}

/// `[4] 服务控制` 二级菜单的日志行数，与选项文案同源。
pub const SERVICE_LOG_LINES: u32 = 50;

/// `[4] 服务控制` 的二级数字菜单：样式与主菜单一致——前空一行、两列缩进的标题、
/// 选项 5 列缩进。以前两列缩进直接接在主菜单下面，看着像主菜单多出来的几行。
pub fn render_service_options() -> String {
    format!(
        "\n  服务控制\n     [1] 重启 bui-c.service\n     [2] 最近 {SERVICE_LOG_LINES} 行日志\n     [0] 返回\n"
    )
}

/// 二级菜单：空行与 `0` 返回，别的无法识别 → `None`（调用方打「无效选项」后返回）。
pub fn parse_service_choice(input: &str) -> Option<ServiceAction> {
    match normalize_digits(input).as_str() {
        "" | "0" => Some(ServiceAction::Back),
        "1" => Some(ServiceAction::Restart),
        "2" => Some(ServiceAction::Logs),
        _ => None,
    }
}

/// 去掉 ANSI 转义：CSI（`ESC [ … 终止字节`，颜色就是它）、OSC（`ESC ] … BEL|ESC \`）
/// 与其余两字节转义。sing-box 往 journald 里写带颜色的级别（`\x1b[36mINFO\x1b[0m`），
/// 菜单约定无 ANSI，原样打出来在不认颜色的终端里是一串乱码。
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match it.next() {
            // CSI：参数与中间字节之后，以 0x40..=0x7E 的终止字节收尾
            Some('[') => {
                for c in it.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            // OSC：以 BEL 或 ST（ESC \）收尾
            Some(']') => {
                while let Some(c) = it.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' {
                        if it.peek() == Some(&'\\') {
                            it.next();
                        }
                        break;
                    }
                }
            }
            // 其余两字节转义（`ESC c` 之类）连同那个字节一起丢；末尾孤零零的 ESC 也丢
            _ => {}
        }
    }
    out
}

/// 把一行 `journalctl -o short-iso` 压成 `<时间> <消息>`：去掉主机名与 `ident[pid]:` 两段。
///
/// 80 列的终端里 `baiyi sing-box[4242]: ` 这 22 列全是噪音：单元已经在标题里，主机就是本机。
/// 解析不了的行（`-- Boot … --`、多行消息的续行）原样保留。
pub fn compact_journal_line(line: &str) -> String {
    fn parse(line: &str) -> Option<String> {
        let (time, rest) = line.split_once(' ')?;
        let b = time.as_bytes();
        let iso =
            b.len() >= 19 && b[..4].iter().all(u8::is_ascii_digit) && b[4] == b'-' && b[10] == b'T';
        if !iso {
            return None;
        }
        let (_host, rest) = rest.split_once(' ')?;
        let (ident, msg) = match rest.split_once(": ") {
            Some(x) => x,
            None => (rest.strip_suffix(':')?, ""),
        };
        if ident.is_empty() || ident.contains(' ') {
            return None;
        }
        Some(if msg.is_empty() {
            time.to_string()
        } else {
            format!("{time} {msg}")
        })
    }
    parse(line).unwrap_or_else(|| line.to_string())
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

/// 子提示里的「返回」：空行或 `0`（含全角 `０`）。
pub fn is_back(input: &str) -> bool {
    matches!(normalize_digits(input).as_str(), "" | "0")
}

/// `"2"` → `Some(1)`；`0` / 空 / 非数字 / 越界 → `None`（`0` 是「返回」，先用 [`is_back`] 分开）。
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
    fn options_rule_ends_where_the_widest_option_row_ends() {
        // 写死 34 列：SOCKS 模式（[2] 切到 TUN）比选项长 2 列，有新版时又比 [6] 那行短 8 列
        let tun = st();
        let socks = Status {
            mode: Mode::Socks,
            ..st()
        };
        let update = Status {
            update_available: true,
            ..st()
        };
        for (case, s) in [("TUN", tun), ("SOCKS", socks), ("有新版", update)] {
            let out = render_options(&s);
            let rule = out.lines().find(|l| l.contains('─')).unwrap();
            let widest = out
                .lines()
                .filter(|l| (1..=9).any(|n| l.contains(&format!("[{n}]"))))
                .map(display_width)
                .max()
                .unwrap();
            assert_eq!(
                display_width(rule),
                widest,
                "{case}：分隔线结束列要等于最宽选项行的结束列\n{out}"
            );
            assert_eq!(
                rule.len() - rule.trim_start().len(),
                5,
                "与选项同为 5 列缩进"
            );
        }
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
        assert!(up.ends_with("SOCKS ●  运行中"), "{up}");
        assert!(
            !up.contains(":1081") && !up.contains(":8081"),
            "端口「代理」那一行已经写了，模式行不再重复：{up}"
        );
        let proxy = render_status(&s)
            .lines()
            .find(|l| l.contains("代理"))
            .unwrap()
            .to_string();
        assert!(
            proxy.contains("SOCKS5 :1081") && proxy.contains("HTTP :8081"),
            "端口从 Status 取：{proxy}"
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
        assert!(out.contains("[1]   alice-hy2-direct"), "{out}");
        assert!(out.contains("[2] ★ alice-reality-direct"), "{out}");
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
        // 空列表的引导按场景给：菜单里指菜单项（统一写「[3] 导入节点」），命令行里指命令
        assert_eq!(
            render_nodes(&Profiles::new_default(), true),
            "  没有节点，先用 [3] 导入节点\n"
        );
        let empty = render_nodes(&Profiles::new_default(), false);
        assert!(empty.contains("bui-c import"), "{empty}");
        assert!(!empty.contains("[3]"), "{empty}");
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

    /// 真机（baiyi）形态的 9 个节点：名字 3–38 列、label 最长 26 列、host:port 最长 29 列。
    /// 端点与凭据是合成的，只有各字段的长度照抄真机。
    fn baiyi_like() -> Profiles {
        use bui_schema::nodes::{Node, NodeKind};
        let node = |kind: NodeKind, label: &str, host: &str, port: u16| Node {
            kind,
            label: label.into(),
            host: host.into(),
            port,
            ..match kind {
                NodeKind::RealityDirect | NodeKind::RealityResidential => reality_direct_node(),
                NodeKind::Hy2Direct | NodeKind::Hy2Residential => hy2_direct_node(),
            }
        };
        let bwg = "tizi.example.test";
        let cl = "rick-node.example-a.net";
        let mut p = Profiles::new_default();
        for (name, n) in [
            (
                "HY2",
                node(NodeKind::Hy2Residential, "示例专用名-HY2住宅", bwg, 40000),
            ),
            (
                "hysteria2-1778329470",
                node(NodeKind::Hy2Direct, "示例专用名", bwg, 10000),
            ),
            (
                "reality-Reality",
                node(
                    NodeKind::RealityDirect,
                    "示例名-reality-Reality直连",
                    bwg,
                    10001,
                ),
            ),
            (
                "rick-node.example-a.net-reality-direct",
                node(NodeKind::RealityDirect, "Reality直连", cl, 10001),
            ),
            (
                "rick-node.example-a.net-reality-resi",
                node(NodeKind::RealityResidential, "Reality住宅", cl, 10002),
            ),
            (
                "rick-node.example-a.net-hy2-direct",
                node(NodeKind::Hy2Direct, "HY2直连", cl, 10000),
            ),
            (
                "rick-node.example-a.net-hy2-resi",
                node(NodeKind::Hy2Residential, "HY2住宅", cl, 40001),
            ),
            (
                "tizi.example.test-reality-resi",
                node(NodeKind::RealityResidential, "Reality住宅", bwg, 10002),
            ),
            (
                "tizi.example.test-hy2-resi",
                node(NodeKind::Hy2Residential, "HY2住宅", bwg, 40002),
            ),
        ] {
            p.profiles.push(named(name, n));
        }
        p.active = Some("hysteria2-1778329470".into());
        p
    }

    #[test]
    fn node_list_fits_in_80_columns_with_real_world_names() {
        let p = baiyi_like();
        assert_eq!(p.profiles.len(), 9);
        let widest = |f: fn(&Profile) -> String| {
            p.profiles
                .iter()
                .map(|x| display_width(&f(x)))
                .max()
                .unwrap()
        };
        assert_eq!(widest(|x| x.name.clone()), 38, "样例名字最长 38 列");
        assert_eq!(
            widest(|x| x.node.label.clone()),
            26,
            "样例 label 最长 26 列"
        );
        assert_eq!(
            widest(|x| format!("{}:{}", x.node.host, x.node.port)),
            29,
            "样例 host:port 最长 29 列"
        );

        for with_back in [true, false] {
            let out = render_nodes(&p, with_back);
            for l in out.lines() {
                assert!(
                    display_width(l) <= 80,
                    "{} 列超过 80（with_back={with_back}）：{l:?}\n{out}",
                    display_width(l)
                );
            }
        }
    }

    #[test]
    fn node_list_is_two_lines_per_node_indented_to_the_name() {
        let out = render_nodes(&baiyi_like(), true);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 9 * 2 + 1, "每个节点两行 + [0] 返回：\n{out}");
        assert_eq!(
            lines[..4].join("\n"),
            "     [1]   HY2\n           示例专用名-HY2住宅  hy2-resi  tizi.example.test:40000\n     [2] ★ hysteria2-1778329470\n           示例专用名  hy2-direct  tizi.example.test:10000",
            "\n{out}"
        );
        assert_eq!(
            lines[6], "     [4]   rick-node.example-a.net-reality-direct",
            "长名字不影响别的行：\n{out}"
        );
        assert_eq!(
            lines[7], "           Reality直连  reality-direct  rick-node.example-a.net:10001",
            "不做列对齐：label 不被最长的那个撑宽\n{out}"
        );
        assert_eq!(lines[18], "     [0] 返回", "与编号行同为 5 列缩进");
        // 编号行与主菜单选项同为 5 列缩进
        let menu_indent = render_options(&st())
            .lines()
            .find(|l| l.contains("[1]"))
            .map(|l| l.len() - l.trim_start().len())
            .unwrap();
        assert_eq!(menu_indent, 5);
        for l in lines.iter().step_by(2).take(9) {
            assert!(l.starts_with("     ["), "{l:?}");
        }
    }

    #[test]
    fn two_digit_numbers_keep_names_in_one_column() {
        let mut p = baiyi_like();
        p.profiles.push(named("n10", hy2_direct_node()));
        let out = render_nodes(&p, true);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "     [1]    HY2", "{out}");
        assert_eq!(lines[18], "     [10]   n10", "{out}");
        assert!(
            lines[19].starts_with(&format!("{}HY2直连", " ".repeat(12))),
            "第二行仍缩进到名字起始列：{out}"
        );
    }

    #[test]
    fn one_shot_node_list_has_no_numbers() {
        // `bui-c switch` 只认名字：一次性 list 不打 [n]
        let out = render_nodes(&baiyi_like(), false);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 9 * 2, "{out}");
        assert_eq!(
            lines[..4].join("\n"),
            "    HY2\n    示例专用名-HY2住宅  hy2-resi  tizi.example.test:40000\n  ★ hysteria2-1778329470\n    示例专用名  hy2-direct  tizi.example.test:10000",
            "\n{out}"
        );
        assert!(!out.contains('['), "不打编号：{out}");
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
    fn back_is_blank_or_zero() {
        assert!(is_back(""));
        assert!(is_back("  "));
        assert!(is_back("0"));
        assert!(is_back("０"));
        assert!(!is_back("00"));
        assert!(!is_back("1"));
        assert!(!is_back("x"));
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

    /// 已经被关掉的 stdout（`bui-c list | head -1`）：每次写都 BrokenPipe。
    struct ClosedPipe;
    impl std::io::Write for ClosedPipe {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn write_out_writes_everything_and_surfaces_broken_pipe() {
        let mut v = Vec::new();
        write_out(&mut v, "  ▸ 选择 [0-9]：").unwrap();
        assert_eq!(String::from_utf8(v).unwrap(), "  ▸ 选择 [0-9]：");
        assert_eq!(
            write_out(&mut ClosedPipe, "x").unwrap_err().kind(),
            std::io::ErrorKind::BrokenPipe
        );
    }

    #[test]
    fn prompts_are_silent_without_a_terminal_and_a_closed_stdout_reads_as_eof() {
        let mut v = Vec::new();
        assert!(prompt_out(true, &mut v, "  ▸ 选择 [0-9]：").unwrap());
        assert_eq!(String::from_utf8(v).unwrap(), "  ▸ 选择 [0-9]：");

        let mut v = Vec::new();
        assert!(
            prompt_out(false, &mut v, "  粘贴节点链接（每行一个，空行结束）\n").unwrap(),
            "照常读 stdin"
        );
        assert!(
            v.is_empty(),
            "stdin 不是终端（管道 / 重定向）：不打提示与说明"
        );

        assert!(
            !prompt_out(true, &mut ClosedPipe, "  ▸ 选择 [0-9]：").unwrap(),
            "stdout 被关：当 EOF 收场，不 panic、不报错"
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
    fn compact_journal_line_keeps_time_and_message_only() {
        // systemd ≥ 249 的 short-iso 时区带冒号
        assert_eq!(
            compact_journal_line(
                "2026-09-13T10:15:30+08:00 baiyi sing-box[4242]: INFO[0000] inbound/mixed[mixed-in]: 127.0.0.1:1080"
            ),
            "2026-09-13T10:15:30+08:00 INFO[0000] inbound/mixed[mixed-in]: 127.0.0.1:1080",
            "只切掉第一个「: 」之前的主机名与 ident，消息里的冒号原样保留"
        );
        // 老 systemd（Ubuntu 20.04 的 245）时区不带冒号
        assert_eq!(
            compact_journal_line(
                "2026-09-13T10:15:30+0800 baiyi systemd[1]: Started bui-c.service - B-UI client."
            ),
            "2026-09-13T10:15:30+0800 Started bui-c.service - B-UI client."
        );
        assert_eq!(
            compact_journal_line("2026-09-13T10:15:30+08:00 baiyi kernel: tun: Universal TUN/TAP"),
            "2026-09-13T10:15:30+08:00 tun: Universal TUN/TAP",
            "ident 可以不带 [pid]"
        );
        assert_eq!(
            compact_journal_line("2026-09-13T10:15:30+08:00 baiyi sing-box[4242]:"),
            "2026-09-13T10:15:30+08:00",
            "空消息"
        );
        for raw in [
            "-- Boot 0123456789abcdef --",
            "-- No entries --",
            "INFO[0000] 没有时间戳（--output cat 的形态）",
            "2026-09-13T10:15:30+08:00 baiyi",
            "",
        ] {
            assert_eq!(compact_journal_line(raw), raw, "解析不了的行原样保留");
        }
    }

    #[test]
    fn pause_prompt_has_no_trailing_colon() {
        assert_eq!(pause_text("回车返回菜单"), "  ▸ 回车返回菜单");
    }

    #[test]
    fn strip_ansi_removes_colors_and_other_escapes() {
        assert_eq!(
            strip_ansi("\u{1b}[36mINFO\u{1b}[0m[0000] started"),
            "INFO[0000] started"
        );
        assert_eq!(strip_ansi("\u{1b}[1;31mFATAL\u{1b}[m x"), "FATAL x");
        assert_eq!(
            strip_ansi("\u{1b}]0;title\u{7}正文"),
            "正文",
            "OSC 以 BEL 收尾"
        );
        assert_eq!(
            strip_ansi("\u{1b}]8;;u\u{1b}\\链接"),
            "链接",
            "OSC 以 ST 收尾"
        );
        assert_eq!(strip_ansi("a\u{1b}cb"), "ab", "两字节转义");
        assert_eq!(strip_ansi("尾巴\u{1b}"), "尾巴", "末尾孤立的 ESC");
        assert_eq!(
            strip_ansi("没有转义 [0000] 中文\n第二行"),
            "没有转义 [0000] 中文\n第二行",
            "普通文字与换行原样保留"
        );
    }

    #[test]
    fn service_submenu_is_numbered_and_parses_back_on_blank_or_zero() {
        assert_eq!(
            render_service_options(),
            "\n  服务控制\n     [1] 重启 bui-c.service\n     [2] 最近 50 行日志\n     [0] 返回\n",
            "与主菜单一致：前空一行、标题、选项 5 列缩进"
        );
        assert_eq!(parse_service_choice("1"), Some(ServiceAction::Restart));
        assert_eq!(parse_service_choice(" ２ "), Some(ServiceAction::Logs));
        assert_eq!(parse_service_choice("0"), Some(ServiceAction::Back));
        assert_eq!(parse_service_choice(""), Some(ServiceAction::Back));
        assert_eq!(parse_service_choice("3"), None);
        assert_eq!(parse_service_choice("x"), None);
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

    #[test]
    fn scripted_records_every_prompt_it_was_asked() {
        let mut p = Scripted::from(["1", "y"]);
        p.line("选择 [0-9]").unwrap();
        p.confirm("切换？").unwrap();
        p.lines_until_blank("粘贴").unwrap();
        assert_eq!(p.asked, vec!["选择 [0-9]", "切换？", "粘贴"]);
    }

    #[test]
    fn scripted_read_tells_a_blank_line_from_eof() {
        let mut p = Scripted::from(["", "1"]);
        assert_eq!(p.read("选择").unwrap().as_deref(), Some(""), "回车");
        assert_eq!(p.read("选择").unwrap().as_deref(), Some("1"));
        assert_eq!(p.read("选择").unwrap(), None, "队列空 = EOF");
        assert_eq!(p.line("选择").unwrap(), "", "line 把 EOF 折成空串");
    }
}
