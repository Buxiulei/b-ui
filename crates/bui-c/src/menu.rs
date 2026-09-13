//! 数字两列菜单：渲染与输入解析（无箭头、无 ANSI、CJK 按 2 列对齐）。
//!
//! 渲染输出不含 ANSI 颜色：颜色会让快照测试变脆，可读性靠对齐与 `●`/`○`/`★` 够用。

use crate::profiles::{kind_slug, Mode, Profiles};
use crate::{Error, Result};
use std::borrow::Cow;
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
    /// 停下来等一个回车（EOF 也算）：长输出打完，别让清屏重画把它抹掉。
    /// `Stdin` 在 stdin 不是终端时直接返回、不读（spec §4.4）。
    fn pause(&mut self, prompt: &str) -> Result<()> {
        self.read(prompt).map(|_| ())
    }
    /// 有人在终端前看提示吗。`Stdin` 在 stdin 不是终端（管道 / 重定向）时为假：
    /// 不打「▸」提示、不清屏、不停顿，菜单因 EOF 退出时也不必补换行。
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

/// [`Stdin::pause`] 读的那一段。stdin 不是终端时没人会按回车：直接返回、一个字节都不读——
/// 照读的话，`printf '5\n0\n' | sudo bui-c` 里的 `0` 会被停顿吃掉，菜单就不照脚本走了
/// （spec §4.4）。终端里读掉一行，EOF 也算回车。
fn pause_from<R: std::io::BufRead>(interactive: bool, r: &mut R) -> Result<()> {
    if !interactive {
        return Ok(());
    }
    read_line_from(r).map(|_| ())
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
        let interactive = self.interactive();
        // stdout 已关（没人看得到提示）就当已经按过回车；非终端时两步都什么也不做
        if !prompt_out(
            interactive,
            &mut std::io::stdout().lock(),
            &pause_text(prompt),
        )? {
            return Ok(());
        }
        pause_from(interactive, &mut std::io::stdin().lock())
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
    /// 按顺序记下被问过的每条提示（`read` / `line` / `confirm` / `lines_until_blank` / `pause`），
    /// 测试靠它断言提示文案、以及「某一问根本没出现」。
    pub asked: Vec<String>,
    /// [`Prompt::interactive`] 的回答，默认 `true`（有人在终端前）。设成 `false` 模拟管道：
    /// 不清屏、EOF 退出不补换行。[`Prompt::pause`] 照样消费一行，测试里写 `""` 代表回车
    /// （spec §4.4）。
    pub tty: bool,
}

impl<'a, const N: usize> From<[&'a str; N]> for Scripted {
    fn from(v: [&'a str; N]) -> Self {
        Self {
            queue: v.iter().map(|s| s.to_string()).collect(),
            asked: Vec::new(),
            tty: true,
        }
    }
}

impl Prompt for Scripted {
    fn interactive(&self) -> bool {
        self.tty
    }

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
    /// 活动节点的 kind（[`kind_slug`]），状态区详情行用；没有节点时为空。
    pub kind: String,
    /// 活动节点的 `服务器:端口`；没有节点时为空。
    pub host_port: String,
    pub mode: Mode,
    pub service_running: bool,
    pub tun_up: bool,
    pub socks_port: u16,
    pub http_port: u16,
    pub update_available: bool,
    /// `[9]` 每日自动更新的开关状态（spec §6「可关」）。
    pub auto_update: bool,
}

/// 歧义宽度字符：真机（tmux）里是 1 列，手机客户端上可能是 2 列（spec §2.2 容量口径）。
/// 对齐按 [`display_width`] 的 1 列算，判断一行放不放得下按 [`budget_width`] 的 2 列算，
/// 两种终端都不折行，最坏只是带这些符号的那一行错开 1 列。
pub const AMBIGUOUS: &str = "★☆●○─…·—–“”‘’→←";

/// 确定只占 1 列的符号白名单，与 [`AMBIGUOUS`] 不相交。[`budget_width`] 不查这张表：
/// 它们不在 AMBIGUOUS 里，容量口径自然按 1 列算。
/// 守门测试靠这两个常量判断渲染输出里的新符号有没有归类。
pub const NARROW: &str = "✓✗▸";

/// 截断补的省略号，在 [`AMBIGUOUS`] 里，容量口径按 2 列。
const ELLIPSIS: char = '…';

/// 一个字符的终端列宽：东亚宽字符 2 列，其余 1 列。宽字大致按 Unicode EastAsianWidth 的 W / F 收，
/// 宁多勿少（多算只会让截断偏短，不会折行）；收的是外部节点名里常见的：谚文、CJK 与全角、
/// CJK 扩展 B–G、宽 emoji。已知没收、按 1 列算的 W（节点名里基本见不到）：谚文字母扩展 A
/// （U+A960–A97C）、竖排标点（U+FE10–FE19）、U+16FE0–1B2FB 里的西夏文、契丹小字、女书、
/// 假名补充等，以及 Unicode 16 才改成 W 的八卦与两仪、四象符号（U+2630–2637、U+268A–268F）、
/// 太玄经符号与算筹（U+1D300–1D376）。
/// 歧义宽度的 ★☆●○ 与 [`NARROW`] 的 ✓✗▸ 在真机上是 1 列，不在这里。
fn char_width(c: char) -> usize {
    let wide = matches!(
        u32::from(c),
        0x1100..=0x115F
            | 0x2E80..=0xA4CF
            | 0xAC00..=0xD7A3
            | 0xF900..=0xFAFF
            | 0xFE30..=0xFE6F
            | 0xFF00..=0xFF60
            | 0xFFE0..=0xFFE6
            | 0x1F300..=0x1FAFF
            // CJK 扩展 B–G
            | 0x20000..=0x3FFFD
            // BMP 里的宽 emoji（U+231A、U+23F0、U+2615、U+26A1、U+2705、U+274C、U+2B50 等）
            | 0x231A..=0x231B | 0x2329..=0x232A | 0x23E9..=0x23EC | 0x23F0 | 0x23F3
            | 0x25FD..=0x25FE | 0x2614..=0x2615 | 0x2648..=0x2653 | 0x267F | 0x2693
            | 0x26A1 | 0x26AA..=0x26AB | 0x26BD..=0x26BE | 0x26C4..=0x26C5 | 0x26CE
            | 0x26D4 | 0x26EA | 0x26F2..=0x26F3 | 0x26F5 | 0x26FA | 0x26FD
            | 0x2705 | 0x270A..=0x270B | 0x2728 | 0x274C | 0x274E | 0x2753..=0x2755
            | 0x2757 | 0x2795..=0x2797 | 0x27B0 | 0x27BF | 0x2B1B..=0x2B1C | 0x2B50
            | 0x2B55
            // U+1F300 以下的宽 emoji（U+1F004、U+1F0CF、U+1F18E、U+1F191–U+1F19A 与带框的汉字）
            | 0x1F004 | 0x1F0CF | 0x1F18E | 0x1F191..=0x1F19A | 0x1F200..=0x1F202
            | 0x1F210..=0x1F23B | 0x1F240..=0x1F248 | 0x1F250..=0x1F251
            | 0x1F260..=0x1F265
    );
    if wide {
        2
    } else {
        1
    }
}

/// 一个字符的容量口径列宽：[`AMBIGUOUS`] 里的多算 1 列。
fn char_budget(c: char) -> usize {
    char_width(c) + usize::from(AMBIGUOUS.contains(c))
}

/// 终端列宽：CJK、全角标点与宽 emoji 按 2 列，其余（含 ★☆●○─…·✓✗▸）按 1 列，与真机（tmux）实测一致。
/// 只用来对齐；判断放不放得下用 [`budget_width`]。菜单只需要这个精度，不引 unicode-width。
pub fn display_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

/// 容量口径列宽：[`display_width`] 加上其中 [`AMBIGUOUS`] 字符的个数。
/// 每一行都要满足 `budget_width ≤ line_limit(终端宽度)`。
pub fn budget_width(s: &str) -> usize {
    s.chars().map(char_budget).sum()
}

/// 一行容量口径的上限：终端宽度减 1，留出最后一列不写（写满整行在有的终端上会多折一行）。
/// 窄于 40 列按 40 排：40 是「不能崩」的下限，再窄由终端自己折行（spec §2.2）。
pub fn line_limit(width: usize) -> usize {
    width.max(40) - 1
}

/// 显示时要换成 `?` 的字符：C0 控制符、DEL、C1（含 ESC 与 U+0085），
/// 双向格式符（U+061C 阿拉伯字母标记、U+202A–202E、U+2066–2069），
/// 零宽与不可见字符（U+200B–200F、U+2060–2064、U+FEFF），
/// 行 / 段分隔符（U+2028–2029，有的终端当换行处理）。
fn unsafe_for_display(c: char) -> bool {
    matches!(
        c,
        '\u{0}'..='\u{1f}'
            | '\u{7f}'..='\u{9f}'
            | '\u{61c}'
            | '\u{200b}'..='\u{200f}'
            | '\u{2028}'..='\u{2029}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{feff}'
    )
}

/// 外部来的文字（节点名、label、主机、日志、检测站返回的字段）渲染前先过这里，
/// 控制字符、双向格式符、零宽与不可见字符、行段分隔符一律换成 `?`：`render_*` 零 ANSI 对订阅来的数据也成立，
/// 名字也没法伪装成别的节点诱导误删。先净化再截断；`profiles.json` 里的原值不动。
/// 没有要换的字符时原样借出。
pub fn sanitize(s: &str) -> Cow<'_, str> {
    if !s.chars().any(unsafe_for_display) {
        return Cow::Borrowed(s);
    }
    Cow::Owned(
        s.chars()
            .map(|c| if unsafe_for_display(c) { '?' } else { c })
            .collect(),
    )
}

/// `s` 最长的、容量口径不超过 `budget` 列的前缀（只在字符边界上切）。
fn prefix_within(s: &str, budget: usize) -> &str {
    let mut used = 0;
    for (i, c) in s.char_indices() {
        used += char_budget(c);
        if used > budget {
            return &s[..i];
        }
    }
    s
}

/// `s` 最长的、容量口径不超过 `budget` 列的后缀（只在字符边界上切）。
fn suffix_within(s: &str, budget: usize) -> &str {
    let mut used = 0;
    for (i, c) in s.char_indices().rev() {
        used += char_budget(c);
        if used > budget {
            return &s[i + c.len_utf8()..];
        }
    }
    s
}

/// 尾部截断到容量口径 `max_budget` 列以内（按 [`budget_width`] 量，`…` 算 2 列）。
///
/// 调用约定：`max_budget` 直接传这一段可用的容量预算（`line_limit` 减去同一行其它部分的 `budget_width`），`…` 的 2 列由函数自己扣。
///
/// 放得下就原样返回；放不下就从头逐字累加，给 `…` 留出 2 列，下一个字放不下就停：
/// 不切半个字，所以结果可能比上限窄 1 列。上限连 `…` 都放不下时只留放得下的前缀，不补 `…`。
/// 只管宽度，不净化，调用方先过 [`sanitize`]。
pub fn truncate_end(s: &str, max_budget: usize) -> String {
    if budget_width(s) <= max_budget {
        return s.to_string();
    }
    match max_budget.checked_sub(char_budget(ELLIPSIS)) {
        Some(room) => format!("{}{ELLIPSIS}", prefix_within(s, room)),
        None => prefix_within(s, max_budget).to_string(),
    }
}

/// 中间截断到容量口径 `max_budget` 列以内：节点名的区别常在尾部（v3 迁来的名字只差时间戳，
/// 面板导入的名字开头都是同一个域名），两头都要留住。
///
/// 调用约定：`max_budget` 直接传这一段可用的容量预算（`line_limit` 减去同一行其它部分的 `budget_width`），`…` 的 2 列由函数自己扣。
///
/// 扣掉 `…` 的 2 列后，头部约占 40%，尾部拿剩下的；尾部遇到宽字停早了，省下的列再还给头部。
/// 放得下就原样返回；上限 < 5，或头部连一个字都放不下（开头是宽字、预算又小）时，
/// 退回 [`truncate_end`]，不输出只剩尾巴的「…名字」。
pub fn truncate_middle(s: &str, max_budget: usize) -> String {
    if budget_width(s) <= max_budget {
        return s.to_string();
    }
    if max_budget < 5 {
        return truncate_end(s, max_budget);
    }
    let room = max_budget - char_budget(ELLIPSIS);
    // 头尾两段的容量合计不超过 room < budget_width(s)，所以不会重叠
    let head = prefix_within(s, room * 2 / 5);
    let tail = suffix_within(s, room - budget_width(head));
    let head = prefix_within(s, room - budget_width(tail));
    if head.is_empty() {
        return truncate_end(s, max_budget);
    }
    format!("{head}{ELLIPSIS}{tail}")
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

/// 标题条与分隔线的横线字符。它在 [`AMBIGUOUS`] 里，容量口径按 2 列；哪天发现哪种客户端
/// 还是折行，只改这一处，换成 ASCII `-`（spec §2.2）。
pub const RULE: char = '─';

/// 终端宽度不小于它用标准版式；40 ≤ W < 它用窄版式：状态区的名字单独一行、节点列表缩进
/// 2 列、不显示 kind（spec §2.2）。50 列时带编号的列表里名字的可用宽度：非活动行 38 列，
/// 正好放下真机最长的名字；活动行 37 列（★ 占 2 列容量），同一个名字当活动节点时要中间截断。
pub const STANDARD_WIDTH: usize = 50;

fn is_narrow(width: usize) -> bool {
    width < STANDARD_WIDTH
}

/// 标题条 `  ── {text} ──`，所有宽度同一个样子。以前的 `─────  … · v…  ─────…` 在把 `─`
/// 画成 2 列的手机客户端上最坏占 81 列，80 列也折行。
pub fn title_bar(text: &str) -> String {
    format!("  {RULE}{RULE} {text} {RULE}{RULE}")
}

/// 分隔线：`min(max, (line_limit(width) − indent) / 2)` 个 [`RULE`]，不含缩进（调用方自己加）。
/// 除的是 `RULE` 的容量口径列宽（`─` 为 2），所以连同缩进不超过行宽上限；`max` 是它本来该有的长度。
pub fn rule(width: usize, indent: usize, max: usize) -> String {
    let fit = line_limit(width).saturating_sub(indent) / char_budget(RULE);
    RULE.to_string().repeat(max.min(fit))
}

/// 用两个空格把非空的几段接起来：某段为空（例如没有 label）时不留空洞。
fn join2(parts: &[&str]) -> String {
    parts
        .iter()
        .filter(|p| !p.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("  ")
}

/// 标题条 + 状态区 + 两列数字菜单；`last` 有内容时，底下再接一行 `  上次：…`。
pub fn render(st: &Status, width: usize, last: Option<&str>) -> String {
    let mut out = render_status(st, width);
    out.push_str(&render_options(st, width));
    if let Some(text) = last.filter(|t| !t.is_empty()) {
        out.push_str(&last_line(text, width));
    }
    out
}

/// 「上次：」行的前缀，含 2 列缩进。
const LAST_HEAD: &str = "  上次：";

/// 主菜单底部的「上次：」行（含换行）：先净化，再尾部截断到行宽上限。
/// 摘要里名字的中间截断由拼摘要的调用方做（spec §0.2 R6），这里只保证整行不折。
fn last_line(text: &str, width: usize) -> String {
    let room = line_limit(width) - budget_width(LAST_HEAD);
    format!("{LAST_HEAD}{}\n", truncate_end(&sanitize(text), room))
}

/// 「上次：」行的摘要里带节点名时，名字单独中间截断（spec §0.2 R6）：名字的预算是行宽上限
/// 减去「  上次：」与名字前后的文字，放得下就原样。摘要里找不到名字就原样返回，整行照旧由
/// [`render`] 尾截。先净化，免得控制字符把宽度算错。
pub fn fit_name_in_last(summary: &str, name: &str, width: usize) -> String {
    let summary = sanitize(summary);
    let name = sanitize(name);
    let Some(at) = summary.find(name.as_ref()) else {
        return summary.into_owned();
    };
    let (head, rest) = summary.split_at(at);
    let tail = &rest[name.len()..];
    let room = line_limit(width)
        .saturating_sub(budget_width(LAST_HEAD) + budget_width(head) + budget_width(tail));
    format!("{head}{}{tail}", truncate_middle(&name, room))
}

/// 菜单里节点列表为空时的引导：[1] 的列表页与回主菜单后的「上次：」行共用。
pub const NO_NODES: &str = "没有节点，先用 [3] 导入节点";

/// 输错时回显的那一行，不含缩进（调用方经 `say` 在菜单里加两列）：`{head}{输入}{tail}`。
/// 回显的输入先净化（方向键是 `ESC [ A`，不能原样写回终端、写进 transcript），再尾截，
/// 整行按容量口径不超过行宽上限——误把一整条链接贴进来也只占一行。
fn bad_input_line(head: &str, input: &str, tail: &str, width: usize) -> String {
    // 2 = 菜单里 `say` 加的缩进
    let room = line_limit(width).saturating_sub(2 + budget_width(head) + budget_width(tail));
    format!("{head}{}{tail}", truncate_end(&sanitize(input), room))
}

/// 主菜单输错：`无效选项：{x}（请输入 0-9 的数字）`，回显先净化再截到行宽。
pub fn invalid_choice(input: &str, width: usize) -> String {
    bad_input_line("无效选项：", input, "（请输入 0-9 的数字）", width)
}

/// [1] 节点列表输错：`无效编号：{x}（可选 1-{len}，0 返回）`，回显先净化再截到行宽。
pub fn invalid_pick(input: &str, len: usize, width: usize) -> String {
    bad_input_line(
        "无效编号：",
        input,
        &format!("（可选 1-{len}，0 返回）"),
        width,
    )
}

/// [4] 服务控制输错：`无效选项：{x}`，回显先净化再截到行宽。
pub fn invalid_service_choice(input: &str, width: usize) -> String {
    bad_input_line("无效选项：", input, "", width)
}

/// 标题条与状态区（节点 / 代理 / 模式）：一次性 `bui-c status` 用它，不打菜单块
/// （命令行里 `[1] 切换节点` 无处可点）。
///
/// 标准版式：`   节点   ●  运行中  {名字}`，连名字放不下时名字另起一行；下面是详情行
/// `label  kind  服务器:端口`，缩进到 `●` 那一列。窄版式：状态词、名字、详情各占一行，前缀收紧。
/// 名字只做中间截断；详情怎么降级见 [`status_detail`]。
pub fn render_status(st: &Status, width: usize) -> String {
    let limit = line_limit(width);
    let narrow = is_narrow(width);
    let mut out = format!(
        "\n{}\n\n",
        title_bar(&format!("B-UI 客户端 v{}", crate::VERSION))
    );
    // 没有节点时服务就算在跑也没东西可用：不亮绿灯，也不报「已停止」这种无关的状态
    let (dot, word) = if st.node.is_empty() {
        ("○", "(未设置)")
    } else if st.service_running {
        ("●", "运行中")
    } else {
        ("○", "已停止")
    };
    // 另起的名字行与详情行缩进到 `●` 那一列
    let (head, indent) = if narrow {
        (format!("   节点 {dot} {word}"), 8)
    } else {
        (format!("   节点   {dot}  {word}"), 10)
    };
    let room = limit - indent;
    let pad = " ".repeat(indent);
    if st.node.is_empty() {
        out.push_str(&format!("{head}\n"));
    } else {
        let name = sanitize(&st.node);
        let one = format!("{head}  {name}");
        if !narrow && budget_width(&one) <= limit {
            out.push_str(&format!("{one}\n"));
        } else {
            // 另起一行后 60 列放得下 38 列的长名字，40 列才要中间截断
            out.push_str(&format!("{head}\n{pad}{}\n", truncate_middle(&name, room)));
        }
        for l in status_detail(st, narrow, room) {
            out.push_str(&format!("{pad}{l}\n"));
        }
    }
    // 窄版式收紧对齐空格：两个端口都是 5 位数时，标准写法有 40 列
    if narrow {
        out.push_str(&format!(
            "   代理 SOCKS5 :{}  HTTP :{}\n",
            st.socks_port, st.http_port
        ));
    } else {
        out.push_str(&format!(
            "   代理      SOCKS5 :{}   HTTP :{}\n",
            st.socks_port, st.http_port
        ));
    }
    // 两种模式的状态平行给：SOCKS 以前恒亮「● 本地入口」，服务停了也照亮，是假绿灯。
    // 端口不在这里重复：「代理」那一行已经写了（TUN 模式下 1080/8080 也在监听）
    let (mode, up, down) = match st.mode {
        Mode::Tun => ("TUN", st.tun_up, "未就绪"),
        Mode::Socks => ("SOCKS", st.service_running, "已停止"),
    };
    let (dot, word) = if up {
        ("●", "运行中")
    } else {
        ("○", down)
    };
    if narrow {
        out.push_str(&format!("   模式 {mode} {dot} {word}\n\n"));
    } else {
        out.push_str(&format!("   模式   {mode:<5} {dot}  {word}\n\n"));
    }
    out
}

/// 状态区的详情行（spec §2.4）：`label  kind  服务器:端口` 放不下就去掉 kind（窄版式本来就不显示），
/// 还放不下就 label 一行、服务器:端口一行，各自尾部截断到 `room`。三段都空时一行也不出。
fn status_detail(st: &Status, narrow: bool, room: usize) -> Vec<String> {
    let label = sanitize(&st.label);
    let hp = sanitize(&st.host_port);
    let full = join2(&[&label, &sanitize(&st.kind), &hp]);
    let short = join2(&[&label, &hp]);
    let line = if !narrow && budget_width(&full) <= room {
        full
    } else if budget_width(&short) <= room {
        short
    } else {
        return [&label, &hp]
            .into_iter()
            .filter(|s| !s.is_empty())
            .map(|s| truncate_end(s, room))
            .collect();
    };
    if line.is_empty() {
        Vec::new()
    } else {
        vec![line]
    }
}

/// 两列数字菜单块：`[1]`~`[9]` + 分隔线 + `[0] 退出`。两列排布任何宽度都不转单列（spec §2.2），
/// 跟着宽度变的只有分隔线。
pub fn render_options(st: &Status, width: usize) -> String {
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
    // 写死的线要么比选项长、要么短一截。再按容量口径封顶：`─` 在有的手机客户端上画成 2 列
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
        rule(width, 5, widest.saturating_sub(5))
    ));
    out.push_str("     [0] 退出\n");
    out
}

/// label 至少留几列，才值得跟服务器:端口挤在一行（spec §2.4）。
const LABEL_MIN: usize = 8;

/// 节点名里带不带它的服务器：面板导入的名字形如 `{服务器}-reality-direct`。
fn host_in_name(name: &str, host: &str) -> bool {
    !host.is_empty()
        && name
            .to_ascii_lowercase()
            .contains(&host.to_ascii_lowercase())
}

/// 标准版式的节点列表不显示 kind 时的一行详情（spec §2.4 第 2 条）：`label  服务器:端口` 放得下就原样；
/// 放不下时，`keep_label`（名字里已经带着服务器，上一行看得见）保住 label，尾部截断；
/// 否则保住服务器:端口，label 尾截、至少留 [`LABEL_MIN`] 列，不够就只剩服务器:端口。
/// 窄版式不走这里，见 [`render_nodes`]。
fn list_detail(label: &str, hp: &str, keep_label: bool, room: usize) -> String {
    let both = join2(&[label, hp]);
    if budget_width(&both) <= room {
        return both;
    }
    if keep_label && !label.is_empty() {
        return truncate_end(label, room);
    }
    let label_room = room.saturating_sub(budget_width(hp) + 2);
    if !label.is_empty() && label_room >= LABEL_MIN {
        return format!("{}  {hp}", truncate_end(label, label_room));
    }
    truncate_end(hp, room)
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
/// 按宽度排（spec §2.2、§2.4）：名字放不下就中间截断；只要有一行放不下完整的
/// `label  kind  服务器:端口`，整张表都不显示 kind（同一张表的列要一致），仍放不下的行按
/// [`list_detail`] 降级。窄版式（40–49 列）编号行缩进 2 列，不显示 kind，第二行一律只出 label
/// （没有 label 才出服务器:端口，放不下尾截；spec §2.5）。
/// 名字、label、服务器先过 [`sanitize`]。
///
/// `with_back`：菜单里选节点要打编号与 `[0] 返回`；一次性 `bui-c list` 不打编号
/// （`bui-c switch` 只认名字），第一行形如 `  ★ name` / `    name`。
pub fn render_nodes(prof: &Profiles, with_back: bool, width: usize) -> String {
    if prof.profiles.is_empty() {
        return if with_back {
            format!("  {NO_NODES}\n")
        } else {
            "  没有节点，先 `bui-c import …`\n".to_string()
        };
    }
    let limit = line_limit(width);
    let narrow = is_narrow(width);
    let margin = if narrow { "  " } else { "     " };
    // 两位数编号时补齐 `[n]`，名字仍然对齐在同一列
    let num_w = format!("[{}]", prof.profiles.len()).len();
    let lead = |i: usize| {
        if with_back {
            format!("{margin}{} ", pad(&format!("[{}]", i + 1), num_w))
        } else {
            "  ".to_string()
        }
    };
    // lead 全是 ASCII，字节数就是列数；再加名字前的 `★ ` 或两个空格
    let indent = " ".repeat(lead(0).len() + 2);
    let room = limit - indent.len();
    let details: Vec<(String, String, String)> = prof
        .profiles
        .iter()
        .map(|p| {
            let label = sanitize(&p.node.label).into_owned();
            let hp = sanitize(&format!("{}:{}", p.node.host, p.node.port)).into_owned();
            let full = join2(&[&label, kind_slug(p.node.kind), &hp]);
            (label, hp, full)
        })
        .collect();
    let show_kind = !narrow
        && details
            .iter()
            .all(|(_, _, full)| budget_width(full) <= room);
    let mut out = String::new();
    for (i, (p, (label, hp, full))) in prof.profiles.iter().zip(&details).enumerate() {
        let mark = if prof.active.as_deref() == Some(p.name.as_str()) {
            "★ "
        } else {
            "  "
        };
        let lead = lead(i);
        let name_room = limit - budget_width(&lead) - budget_width(mark);
        out.push_str(&format!(
            "{lead}{mark}{}\n",
            truncate_middle(&sanitize(&p.name), name_room)
        ));
        let detail = if show_kind {
            full.clone()
        } else if narrow {
            // 窄版式每行只出 label：那是家人自己起的备注，v3 迁来的节点只靠它辨认；
            // 放得下服务器的行也不带，免得有的行带有的行不带。没有 label 才出服务器:端口
            truncate_end(if label.is_empty() { hp } else { label }, room)
        } else {
            list_detail(label, hp, host_in_name(&p.name, &p.node.host), room)
        };
        out.push_str(&format!("{indent}{detail}\n"));
    }
    if with_back {
        out.push_str(&format!("{margin}[0] 返回\n"));
    }
    out
}

/// 菜单 `[1] 切换节点` 的一屏：前空一行、两列缩进的标题，再接带编号的 [`render_nodes`]——
/// 与 [`render_service_options`] 同一个样式。一次性 `bui-c list` 不用它，不加标题。
pub fn render_node_picker(prof: &Profiles, width: usize) -> String {
    format!("\n  切换节点\n{}", render_nodes(prof, true, width))
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
    use crate::testutil::{baiyi_like, hy2_direct_node, named, reality_direct_node, split_global};
    use pretty_assertions::assert_eq;

    fn st() -> Status {
        Status {
            node: "alice-hy2-direct".into(),
            label: "HY2直连".into(),
            kind: "hy2-direct".into(),
            host_port: "panel.example.com:10000".into(),
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
        // 真机（tmux）里 ★ 是 1 列；手机客户端可能是 2 列，那部分由 budget_width 兜
        assert_eq!(display_width("★"), 1);
    }

    #[test]
    fn star_is_one_column_and_ambiguous_counts_twice_in_the_budget() {
        assert_eq!(display_width("★"), 1);
        assert_eq!(display_width("中a★"), 4);
        assert_eq!(budget_width("中a★"), 5);
        assert_eq!(budget_width("→ [3]"), 6);
        assert_eq!(budget_width("✓ 通"), 4); // ✓ 不在 AMBIGUOUS 里，不多算（budget_width 不查 NARROW）
    }

    #[test]
    fn sanitize_replaces_controls_bidi_and_zero_width() {
        assert_eq!(sanitize("a\u{1b}[31mb"), "a?[31mb");
        assert_eq!(sanitize("x\u{202e}y\u{200b}z\u{85}"), "x?y?z?");
        assert!(matches!(
            sanitize("正常-名字"),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn truncation_fits_the_budget_and_never_splits_a_character() {
        for (s, max) in [
            ("rick-node.example-a.net-reality-direct", 20),
            ("示例专用名-HY2住宅", 9),
            ("abc", 3),
        ] {
            let e = truncate_end(s, max);
            let m = truncate_middle(s, max);
            assert!(budget_width(&e) <= max, "{e}");
            assert!(budget_width(&m) <= max, "{m}");
        }
        assert_eq!(truncate_end("abc", 3), "abc"); // 放得下就原样
        let m = truncate_middle("rick-node.example-a.net-reality-direct", 20);
        assert!(
            m.starts_with("rick-") && m.ends_with("direct") && m.contains('…'),
            "{m}"
        );
        // 「示」2 列 +「…」按 2 = 4 ≤ 5；再加「例」就是 6 > 5，所以只留一个字
        assert_eq!(truncate_end("示例专用名", 5), "示…");
    }

    #[test]
    fn truncation_outputs_are_pinned() {
        // room = 20 − 2 = 18：头取 18×2/5 = 7 列，尾取 18 − 7 = 11 列
        assert_eq!(
            truncate_middle("rick-node.example-a.net-reality-direct", 20),
            "rick-no…lity-direct"
        );
        // room = 7：头 7×2/5 = 2 列（示），尾 5 列（2住宅），合计 2 + 2 + 5 = 9
        assert_eq!(truncate_middle("示例专用名-HY2住宅", 9), "示…2住宅");
        // room = 8：头先取 8×2/5 = 3 列（abc）；尾上限 5，「例名」4 列，再加「示」就是 6，停下；
        // 尾部省下的 1 列让回头部，得 abcd（少了这一步就是 abc…例名）
        assert_eq!(truncate_middle("abcdefgh示例名", 10), "abcd…例名");
        // 上限连 … 都放不下：只留放得下的前缀，不补 …
        assert_eq!(truncate_middle("abcdef", 1), "a");
        // … 按 2 列刚好占满，前缀一个字都不剩
        assert_eq!(truncate_end("abc", 2), "…");
    }

    #[test]
    fn middle_truncation_falls_back_to_end_when_the_head_is_empty() {
        // room = 3 / 4 时头部分到 1 列，放不下「示」：退回尾截断，不能输出「…字」「…名字」
        assert_eq!(
            truncate_middle("示例专用名字", 5),
            truncate_end("示例专用名字", 5)
        );
        assert_eq!(
            truncate_middle("示例专用名字", 6),
            truncate_end("示例专用名字", 6)
        );
        assert_eq!(truncate_middle("示例专用名字", 6), "示例…");
        // 任何预算下：不超上限；以 … 开头（头部丢光）只允许出现在与尾截断同值时
        for s in [
            "示例专用名字",
            "示例专用名-HY2住宅",
            "rick-node.example-a.net-reality-direct",
            "★示例专用名★",
        ] {
            for max in 0..=24 {
                let m = truncate_middle(s, max);
                assert!(budget_width(&m) <= max, "{s} @ {max}: {m}");
                if m.starts_with('…') {
                    assert_eq!(m, truncate_end(s, max), "{s} @ {max}");
                }
            }
        }
    }

    #[test]
    fn sanitize_also_replaces_isolates_separators_and_invisible_marks() {
        for c in [
            '\u{7f}', '\u{61c}', '\u{2028}', '\u{2029}', '\u{2060}', '\u{2064}', '\u{2066}',
            '\u{2067}', '\u{2068}', '\u{2069}', '\u{feff}',
        ] {
            assert_eq!(sanitize(&format!("a{c}b")), "a?b", "U+{:04X}", u32::from(c));
        }
        // 反例：不间断空格、连字符、窄不间断空格是正常排版字符，原样借出
        let keep = "a\u{a0}b\u{2010}c\u{202f}d";
        assert_eq!(sanitize(keep), keep);
        assert!(matches!(sanitize(keep), std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn narrow_symbols_are_one_column_and_not_ambiguous() {
        for c in NARROW.chars() {
            assert!(!AMBIGUOUS.contains(c), "{c} 两张表都在");
            assert_eq!(display_width(&c.to_string()), 1, "{c}");
        }
        // 歧义字符在真机上是 1 列，容量口径按 2 列：宽字表不能把它们收进去
        for c in AMBIGUOUS.chars() {
            assert_eq!(budget_width(&c.to_string()), 2, "{c}");
        }
    }

    #[test]
    fn wide_table_covers_cjk_extensions_and_wide_emoji() {
        // U+2B50、U+2705 是 BMP 里的宽 emoji，U+20000 是 CJK 扩展 B；源码里只写转义，不写 emoji 字面量
        assert_eq!(display_width("\u{2B50}\u{2705}\u{20000}"), 6);
        assert_eq!(display_width("\u{1F004}\u{1F19A}\u{1F201}"), 6);
        // ★ ✓ 仍按 1 列
        assert_eq!(display_width("★✓"), 2);
    }

    #[test]
    fn line_limit_floors_at_forty() {
        assert_eq!(line_limit(80), 79);
        assert_eq!(line_limit(30), 39);
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
        // 40 列也不转单列：最宽的选项行 39 列，正好是 40 列的行宽上限（spec §2.2）
        for w in [40, 80] {
            let out = render(&st(), w, None);
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
                assert_eq!(r.matches('[').count(), 2, "两列 @{w}：{r}");
            }
            assert!(!out.contains('\u{1b}'), "不含 ANSI 转义（快照稳定）");
            assert!(!out.contains("↑") && !out.contains("↓"), "不做箭头菜单");
        }
    }

    #[test]
    fn options_rule_ends_where_the_widest_option_row_ends() {
        // 写死 34 列：SOCKS 模式（[2] 切到 TUN）比选项长 2 列，有新版时又比 [6] 那行短 7 列
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
            // 100 列放得下原长：分隔线结束列 = 最宽选项行的结束列
            let out = render_options(&s, 100);
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
            // 窄了就按容量口径封顶（`─` 按 2 列算）：整行不超上限，也不比最宽选项行长
            for w in [40, 50, 60, 80] {
                let out = render_options(&s, w);
                let rule = out.lines().find(|l| l.contains('─')).unwrap();
                assert!(budget_width(rule) <= line_limit(w), "{case} @{w}：{rule}");
                assert!(display_width(rule) <= widest, "{case} @{w}：{rule}");
            }
        }
        // spec §2.2：菜单分隔线 60 列 27 个、40 列 17 个；80 列放得下原长 34 个
        let count = |w: usize| {
            render_options(&st(), w)
                .lines()
                .find(|l| l.contains('─'))
                .unwrap()
                .matches(RULE)
                .count()
        };
        assert_eq!((count(40), count(60), count(80)), (17, 27, 34));
    }

    #[test]
    fn status_lines_show_node_mode_and_ports() {
        let out = render(&st(), 80, None);
        assert!(out.contains("alice-hy2-direct"));
        assert!(out.contains("HY2直连"));
        assert!(
            out.contains("HY2直连  hy2-direct  panel.example.com:10000"),
            "详情行：label、kind、服务器:端口\n{out}"
        );
        assert!(out.contains("SOCKS5 :1080"));
        assert!(out.contains("HTTP :8080"));
        assert!(out.contains("TUN"));
        assert!(out.contains(crate::VERSION));
        assert!(
            out.contains(&format!("\n  ── B-UI 客户端 v{} ──\n", crate::VERSION)),
            "短标题条，不带歧义字符 ·：\n{out}"
        );
        let mut s2 = st();
        s2.node = String::new();
        s2.service_running = false;
        s2.tun_up = false;
        s2.mode = Mode::Socks;
        let out2 = render(&s2, 80, None);
        assert!(out2.contains("(未设置)"));
        assert!(out2.contains("已停止"));
        assert!(out2.contains("SOCKS"));
    }

    /// 没有节点时 sing-box 就算在跑也没什么可用的：节点行不能亮绿灯。
    #[test]
    fn node_line_without_a_node_is_never_green() {
        for running in [true, false] {
            let s = Status {
                node: String::new(),
                label: String::new(),
                kind: String::new(),
                host_port: String::new(),
                service_running: running,
                ..st()
            };
            for (w, want) in [(80, "   节点   ○  (未设置)"), (40, "   节点 ○ (未设置)")]
            {
                let out = render_status(&s, w);
                let lines: Vec<&str> = out.lines().collect();
                let i = lines.iter().position(|l| l.contains("节点")).unwrap();
                assert_eq!(lines[i], want, "service_running={running} @{w}");
                assert!(
                    lines[i + 1].starts_with("   代理"),
                    "没有节点就没有详情行：\n{out}"
                );
            }
        }
        // 有节点时照旧跟着服务状态；详情行缩进到 ● 那一列
        let out = render_status(&st(), 80);
        let lines: Vec<&str> = out.lines().collect();
        let i = lines.iter().position(|l| l.contains("节点")).unwrap();
        assert_eq!(lines[i], "   节点   ●  运行中  alice-hy2-direct");
        assert_eq!(
            lines[i + 1],
            "          HY2直连  hy2-direct  panel.example.com:10000"
        );
    }

    #[test]
    fn mode_option_names_the_target_mode() {
        let mut s = st(); // Tun
        let tun = render_options(&s, 80);
        assert!(tun.contains("[2] 切到 SOCKS"), "{tun}");
        assert!(!tun.contains("切换模式"), "别让用户猜切到哪边：{tun}");
        s.mode = Mode::Socks;
        assert!(render_options(&s, 80).contains("[2] 切到 TUN"));
    }

    /// 状态块里的「模式」那一行（80 列，标准版式）。
    fn mode_line(st: &Status) -> String {
        render_status(st, 80)
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
        let proxy = render_status(&s, 80)
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
        assert!(render(&st(), 80, None).contains("[9] 自动更新 开"));
        let mut s = st();
        s.auto_update = false;
        assert!(render(&s, 80, None).contains("[9] 自动更新 关"));
    }

    #[test]
    fn update_marker_shows_only_when_available() {
        assert!(!render(&st(), 80, None).contains("★ 有新版"));
        let mut s = st();
        s.update_available = true;
        assert!(render(&s, 80, None).contains("★ 有新版"));
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
        let out = render_nodes(&p, true, 80);
        assert!(out.contains("[1]   alice-hy2-direct"), "{out}");
        assert!(out.contains("[2] ★ alice-reality-direct"), "{out}");
        assert!(
            out.contains("HY2直连") && out.contains("Reality直连"),
            "显示 label 便于辨认"
        );
        assert!(render_nodes(&Profiles::new_default(), true, 80).contains("没有节点"));
    }

    #[test]
    fn render_splits_into_status_and_options() {
        let s = st();
        let status = render_status(&s, 80);
        let options = render_options(&s, 80);
        assert_eq!(
            render(&s, 80, None),
            format!("{status}{options}"),
            "render = 两者相接"
        );
        assert_eq!(
            render(&s, 80, Some("已切到 alice-reality-direct")),
            format!("{status}{options}  上次：已切到 alice-reality-direct\n"),
            "「上次：」行接在 [0] 退出 下面"
        );
        assert_eq!(
            render(&s, 80, Some("")),
            render(&s, 80, None),
            "空摘要不出「上次：」行"
        );
        assert!(
            status.contains("alice-hy2-direct"),
            "状态块留标题条与节点行"
        );
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
            render_nodes(&p, true, 80).contains("[0] 返回"),
            "菜单里要能返回"
        );
        assert!(
            !render_nodes(&p, false, 80).contains("[0] 返回"),
            "一次性 list 没有可返回的地方"
        );
        // 空列表的引导按场景给：菜单里指菜单项（统一写「[3] 导入节点」），命令行里指命令
        assert_eq!(
            render_nodes(&Profiles::new_default(), true, 80),
            "  没有节点，先用 [3] 导入节点\n"
        );
        let empty = render_nodes(&Profiles::new_default(), false, 80);
        assert!(empty.contains("bui-c import"), "{empty}");
        assert!(!empty.contains("[3]"), "{empty}");
    }

    /// 宽度守门表：之后的任务把自己新增的渲染也加进 `screens()`。
    fn screens(width: usize) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        let mut prof = baiyi_like();
        for i in 0..prof.profiles.len() {
            prof.active = Some(prof.profiles[i].name.clone());
            let mut s = st();
            s.node = prof.profiles[i].name.clone();
            s.label = prof.profiles[i].node.label.clone();
            out.push((
                "render",
                render(
                    &s,
                    width,
                    Some("已删 1 个，切到 rick-node.example-a.net-reality-direct"),
                ),
            ));
        }
        out.push(("nodes", render_nodes(&prof, true, width)));
        out.push(("picker", render_node_picker(&prof, width)));
        out.push(("service", render_service_options()));
        // 下面是另补的形态：状态区带上各节点真实的 kind 与服务器:端口（上面只换了名字与 label，
        // 服务器:端口一直是 st() 的 23 列）、每个节点轮流当活动节点时的菜单列表与一次性 list、
        // 没有节点、SOCKS 模式服务停了且端口是 5 位数。有新版时 [6] 右栏的「检查更新 ★ 有新版」
        // 在 40–47 列放不下；主菜单重排把 ★ 挪到左栏之后，把 update_available 也加进来
        for p in &prof.profiles {
            let s = Status {
                node: p.name.clone(),
                label: p.node.label.clone(),
                kind: kind_slug(p.node.kind).to_string(),
                host_port: format!("{}:{}", p.node.host, p.node.port),
                ..st()
            };
            out.push(("status", render_status(&s, width)));
        }
        for i in 0..prof.profiles.len() {
            prof.active = Some(prof.profiles[i].name.clone());
            out.push(("nodes", render_nodes(&prof, true, width)));
            out.push(("list", render_nodes(&prof, false, width)));
        }
        let empty = Status {
            node: String::new(),
            label: String::new(),
            kind: String::new(),
            host_port: String::new(),
            ..st()
        };
        out.push((
            "empty",
            render(&empty, width, Some("已删光节点，按 [3] 导入")),
        ));
        out.push((
            "no-nodes",
            render_node_picker(&Profiles::new_default(), width),
        ));
        out.push((
            "no-nodes-list",
            render_nodes(&Profiles::new_default(), false, width),
        ));
        let socks = Status {
            mode: Mode::Socks,
            service_running: false,
            socks_port: 65535,
            http_port: 65535,
            ..st()
        };
        out.push(("socks", render(&socks, width, None)));
        // T4：「上次：」行（名字单独中间截断的切换摘要、空列表引导、长的失败行）与主菜单输错
        // 那一行（回显的输入先净化再尾截；菜单里 `say` 加 2 列缩进）
        for p in &prof.profiles {
            for summary in [
                format!("已切到 {}", p.name),
                format!("已是当前节点：{}", p.name),
                format!("已切到 {}，但 bui-tun 没起来", p.name),
            ] {
                let last = fit_name_in_last(&summary, &p.name, width);
                out.push(("last-name", render(&st(), width, Some(&last))));
            }
        }
        out.push(("last-no-nodes", render(&empty, width, Some(NO_NODES))));
        out.push((
            "last-fail",
            render(
                &st(),
                width,
                Some("失败：manifest 两个源都取不到：面板 HTTP 502，GitHub 连接超时（15 秒）"),
            ),
        ));
        let long = "9".repeat(200);
        for input in [
            "abc",
            "\u{1b}[A",
            "https://panel.example.com/api/sub/示例用户甲",
            long.as_str(),
        ] {
            out.push(("invalid", format!("  {}\n", invalid_choice(input, width))));
            out.push((
                "invalid-pick",
                format!("  {}\n", invalid_pick(input, 9, width)),
            ));
            out.push((
                "invalid-service",
                format!("  {}\n", invalid_service_choice(input, width)),
            ));
        }
        out
    }

    #[test]
    fn stdin_pause_consumes_nothing_without_a_terminal() {
        // `printf '5\n0\n' | sudo bui-c`：停顿不能把留给主菜单的 0 读走（spec §4.4）
        let mut r = std::io::Cursor::new(b"0\n".to_vec());
        pause_from(false, &mut r).unwrap();
        assert_eq!(r.position(), 0, "没人会按回车：一个字节都不读");
        pause_from(true, &mut r).unwrap();
        assert_eq!(r.position(), 2, "终端里读掉一行（回车）");
        pause_from(true, &mut r).unwrap(); // EOF 也算回车，不报错
        assert_eq!(r.position(), 2);
    }

    #[test]
    fn junk_echoes_in_sub_pages_fit_the_line() {
        let url = "https://panel.example.com/api/sub/示例用户甲";
        assert_eq!(invalid_pick("9", 2, 60), "无效编号：9（可选 1-2，0 返回）");
        assert_eq!(
            invalid_pick(url, 2, 40),
            "无效编号：https…（可选 1-2，0 返回）"
        );
        assert_eq!(invalid_service_choice("x", 60), "无效选项：x");
        assert_eq!(
            invalid_service_choice("\u{1b}[A", 60),
            "无效选项：?[A",
            "方向键的 ESC 不能回显"
        );
        // 算上菜单里 `say` 加的 2 列缩进，按容量口径都不超过行宽上限
        for w in [40, 50, 60, 80, 100] {
            for l in [invalid_pick(url, 9, w), invalid_service_choice(url, w)] {
                let l = format!("  {l}");
                assert!(budget_width(&l) <= line_limit(w), "@{w}: {l}");
            }
        }
    }

    #[test]
    fn the_last_line_middle_truncates_the_node_name_on_its_own() {
        let name = "rick-node.example-a.net-reality-direct";
        let summary = format!("已切到 {name}");
        // 40 列：名字的预算 = 39 −「  上次：」8 −「已切到 」7 = 24，头约 40%、尾拿剩下的
        let fitted = fit_name_in_last(&summary, name, 40);
        assert_eq!(fitted, "已切到 rick-nod…reality-direct");
        let line = last_line(&fitted, 40);
        assert_eq!(
            line, "  上次：已切到 rick-nod…reality-direct\n",
            "名字截过之后整行不再被尾截"
        );
        assert!(budget_width(line.trim_end()) <= line_limit(40));
        // 名字前面的字更长：预算跟着缩，尾巴仍然留住 direct 与 resi 的区别
        let fitted = fit_name_in_last(&format!("已是当前节点：{name}"), name, 40);
        assert!(
            fitted.contains('…') && fitted.ends_with("direct"),
            "{fitted}"
        );
        assert!(
            budget_width(&format!("{LAST_HEAD}{fitted}")) <= line_limit(40),
            "{fitted}"
        );
        // 放得下就原样；摘要里没有这个名字也原样
        assert_eq!(fit_name_in_last(&summary, name, 80), summary);
        assert_eq!(
            fit_name_in_last("已取消，模式未变", name, 40),
            "已取消，模式未变"
        );
        // 控制字符先换成 `?` 再算宽度
        assert_eq!(
            fit_name_in_last("已切到 a\u{1b}b", "a\u{1b}b", 80),
            "已切到 a?b"
        );
    }

    #[test]
    fn a_typo_line_echoes_sanitized_input_and_fits_the_line() {
        assert_eq!(
            invalid_choice("abc", 60),
            "无效选项：abc（请输入 0-9 的数字）"
        );
        assert_eq!(
            invalid_choice("\u{1b}[A", 60),
            "无效选项：?[A（请输入 0-9 的数字）",
            "方向键的 ESC 不能回显"
        );
        // 误把整条链接贴进主菜单：只占一行，算上 2 列缩进按容量口径不超过行宽上限
        let url = "https://panel.example.com/api/sub/示例用户甲";
        assert_eq!(
            invalid_choice(url, 40),
            "无效选项：http…（请输入 0-9 的数字）"
        );
        for w in [40, 50, 60, 80, 100] {
            let l = format!("  {}", invalid_choice(url, w));
            assert!(budget_width(&l) <= line_limit(w), "@{w}: {l}");
        }
    }

    #[test]
    fn scripted_is_a_terminal_by_default_and_its_pause_still_eats_a_line() {
        let mut p = Scripted::from(["", "0"]);
        assert!(p.interactive(), "默认当有人在终端前");
        p.tty = false;
        assert!(!p.interactive());
        // 测试里写 "" 代表回车：管道形态下也照样消费一行（真 Stdin 不读）
        p.pause("回车返回菜单").unwrap();
        assert_eq!(p.asked, vec!["回车返回菜单"]);
        assert_eq!(p.queue.len(), 1);
        assert_eq!(p.queue[0], "0");
    }

    #[test]
    fn every_line_fits_by_budget() {
        for w in [40, 50, 60, 80, 100] {
            for (name, text) in screens(w) {
                for l in text.lines() {
                    assert!(
                        budget_width(l) <= line_limit(w),
                        "{name} @{w}: {l:?} = {}",
                        budget_width(l)
                    );
                    for c in l.chars() {
                        let known = c.is_ascii()
                            || display_width(&c.to_string()) == 2
                            || AMBIGUOUS.contains(c)
                            || NARROW.contains(c);
                        assert!(known, "{name}: 未归类的字符 {c:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_long_active_name_splits_at_60_and_is_middle_truncated_at_40() {
        let mut s = st();
        s.node = "rick-node.example-a.net-reality-direct".into();
        s.label = "Reality直连".into();
        let at60 = render_status(&s, 60);
        assert!(at60
            .lines()
            .any(|l| l.contains("rick-node.example-a.net-reality-direct")));
        let at40 = render_status(&s, 40);
        assert!(at40.contains('…') && at40.contains("direct"), "{at40}");
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
            let out = render_nodes(&p, with_back, 80);
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
        // 80 列：与改版前逐字节相同（spec §2.1）
        let out = render_nodes(&baiyi_like(), true, 80);
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
        let menu_indent = render_options(&st(), 80)
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
        let out = render_nodes(&p, true, 80);
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
        let out = render_nodes(&baiyi_like(), false, 80);
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
    fn title_bar_and_rules_stay_within_the_budget() {
        let t = title_bar("B-UI 客户端 v4.0.0");
        assert_eq!(t, "  ── B-UI 客户端 v4.0.0 ──");
        assert_eq!(
            (display_width(&t), budget_width(&t)),
            (26, 30),
            "spec §2.2：26 列，歧义字符画成 2 列时 30 列"
        );
        // spec §2.2：缩进 5 的菜单分隔线 60 列 27 个、40 列 17 个；缩进 2 的报告分隔线 28 / 18 个
        for (w, indent, n) in [
            (60, 5, 27),
            (40, 5, 17),
            (60, 2, 28),
            (40, 2, 18),
            (30, 5, 17),
        ] {
            assert_eq!(
                rule(w, indent, 99),
                RULE.to_string().repeat(n),
                "{w} 列，缩进 {indent}"
            );
        }
        assert_eq!(
            rule(80, 5, 34),
            RULE.to_string().repeat(34),
            "放得下就取原长"
        );
        assert_eq!(rule(40, 60, 10), "", "缩进比上限还宽也不下溢");
    }

    #[test]
    fn status_area_follows_the_width() {
        let v = crate::VERSION;
        let long = Status {
            node: "rick-node.example-a.net-reality-direct".into(),
            label: "Reality直连".into(),
            kind: "reality-direct".into(),
            host_port: "rick-node.example-a.net:10001".into(),
            ..st()
        };
        // 80 列：名字跟在状态后面，详情行带 kind（spec §3a-80）
        assert_eq!(
            render_status(&long, 80),
            format!(
                "\n  ── B-UI 客户端 v{v} ──\n\n   节点   ●  运行中  rick-node.example-a.net-reality-direct\n          Reality直连  reality-direct  rick-node.example-a.net:10001\n   代理      SOCKS5 :1080   HTTP :8080\n   模式   TUN   ●  运行中\n\n"
            )
        );
        // 60 列：连状态带名字按容量口径是 60 列（● 算 2），名字另起一行；详情去掉 kind
        assert_eq!(
            render_status(&long, 60),
            format!(
                "\n  ── B-UI 客户端 v{v} ──\n\n   节点   ●  运行中\n          rick-node.example-a.net-reality-direct\n          Reality直连  rick-node.example-a.net:10001\n   代理      SOCKS5 :1080   HTTP :8080\n   模式   TUN   ●  运行中\n\n"
            )
        );
        // 50 列标准版式：名字另起一行；详情去掉 kind 还有 42 列，超过可用的 39 列，
        // 拆成 label 一行、服务器:端口一行（spec §2.4）
        assert_eq!(
            render_status(&long, 50),
            format!(
                "\n  ── B-UI 客户端 v{v} ──\n\n   节点   ●  运行中\n          rick-node.example-a.net-reality-direct\n          Reality直连\n          rick-node.example-a.net:10001\n   代理      SOCKS5 :1080   HTTP :8080\n   模式   TUN   ●  运行中\n\n"
            )
        );
        // 40 列窄版式：状态词、中间截断的名字、label、服务器各占一行，前缀收紧（spec §3a-40）
        assert_eq!(
            render_status(&long, 40),
            format!(
                "\n  ── B-UI 客户端 v{v} ──\n\n   节点 ● 运行中\n        rick-node.e…net-reality-direct\n        Reality直连\n        rick-node.example-a.net:10001\n   代理 SOCKS5 :1080  HTTP :8080\n   模式 TUN ● 运行中\n\n"
            )
        );
        // 短名字在窄版式里也单独一行
        let at40 = render_status(&st(), 40);
        assert!(
            at40.contains(
                "   节点 ● 运行中\n        alice-hy2-direct\n        HY2直连\n        panel.example.com:10000\n"
            ),
            "{at40}"
        );
    }

    #[test]
    fn node_list_drops_kind_for_the_whole_table_and_keeps_labels_when_narrow() {
        let p = baiyi_like();
        let details = |w: usize| -> Vec<String> {
            render_nodes(&p, true, w)
                .lines()
                .skip(1)
                .step_by(2)
                .take(9)
                .map(str::to_string)
                .collect()
        };
        let kinds = ["hy2-direct", "hy2-resi", "reality-direct", "reality-resi"];
        // 80 列：每行都放得下完整三段，都显示 kind
        for l in details(80) {
            assert!(kinds.iter().any(|k| l.contains(k)), "{l}");
        }
        // 60 列：[3] 那一行放不下完整三段，整张表都不显示 kind；只有它的 label 被尾截
        let at60 = details(60);
        for l in &at60 {
            assert!(!kinds.iter().any(|k| l.contains(k)), "{l}");
        }
        assert_eq!(
            at60[2],
            "           示例名-reality-Realit…  tizi.example.test:10001"
        );
        assert_eq!(
            at60[3],
            "           Reality直连  rick-node.example-a.net:10001"
        );
        // 50 列：名字里带着服务器的保住 label；别的保住服务器:端口、label 尾截
        let at50 = details(50);
        assert_eq!(at50[0], "           示例专用名-…  tizi.example.test:40000");
        assert_eq!(at50[3], "           Reality直连");
        assert_eq!(at50[5], "           HY2直连  rick-node.example-a.net:10000");
        // 40 列窄版式：缩进 2 / 8，第二行只剩 label；长名字中间截断，尾部的 kind 还分得清（spec §3b-40）
        let out = render_nodes(&p, true, 40);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines[..4].join("\n"),
            "  [1]   HY2\n        示例专用名-HY2住宅\n  [2] ★ hysteria2-1778329470\n        示例专用名"
        );
        assert_eq!(lines[6], "  [4]   rick-node.e…net-reality-direct");
        assert_eq!(lines[7], "        Reality直连");
        assert_eq!(lines[8], "  [5]   rick-node.e…a.net-reality-resi");
        assert_eq!(lines[18], "  [0] 返回");
        // 一次性 `bui-c list` 第二行缩进 4、可用 35 列：[2]、[9] 连服务器也放得下（35、32 列），
        // 照样只出 label——窄版式每行出同一种东西，不能有的行带服务器、有的不带（spec §2.5）
        assert_eq!(
            render_nodes(&p, false, 40),
            "    HY2\n    示例专用名-HY2住宅\n  ★ hysteria2-1778329470\n    示例专用名\n    reality-Reality\n    示例名-reality-Reality直连\n    rick-node.exa…a.net-reality-direct\n    Reality直连\n    rick-node.exa…e-a.net-reality-resi\n    Reality住宅\n    rick-node.example-a.net-hy2-direct\n    HY2直连\n    rick-node.example-a.net-hy2-resi\n    HY2住宅\n    tizi.example.test-reality-resi\n    Reality住宅\n    tizi.example.test-hy2-resi\n    HY2住宅\n"
        );
        // 没有 label 才出服务器:端口；label 放不下照样尾截
        let mut q = baiyi_like();
        q.profiles[2].node.label = String::new();
        q.profiles[3].node.label = "示例专用名-reality-Reality直连备用".into();
        let out = render_nodes(&q, true, 40);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[5], "        tizi.example.test:10001");
        assert_eq!(lines[7], "        示例专用名-reality-Reality直…");
    }

    #[test]
    fn external_text_is_sanitized_before_it_reaches_the_screen() {
        let bad = |c: char| matches!(c, '\u{1b}' | '\u{7}' | '\u{202e}' | '\u{200b}');
        let s = Status {
            node: "evil\u{1b}[2Jname".into(),
            label: "lab\u{202e}el".into(),
            host_port: "h\u{7}ost:1".into(),
            ..st()
        };
        let mut p = baiyi_like();
        p.profiles[0].name = "x\u{1b}]0;t\u{7}y".into();
        p.profiles[0].node.label = "\u{200b}标签".into();
        for w in [40, 80] {
            let screen = render(&s, w, Some("上次\u{1b}[31m"));
            assert!(!screen.chars().any(bad), "{screen:?}");
            assert!(screen.contains("evil?[2Jname"), "{screen}");
            let list = render_nodes(&p, true, w);
            assert!(!list.chars().any(bad), "{list:?}");
            assert!(
                list.contains("x?]0;t?y") && list.contains("?标签"),
                "{list}"
            );
        }
    }

    #[test]
    fn the_last_line_is_tail_truncated_to_the_line_limit() {
        let last = "已删 1 个，切到 rick-node.example-a.net-reality-direct";
        let at80 = render(&st(), 80, Some(last));
        assert_eq!(
            at80.lines().last(),
            Some(format!("  上次：{last}").as_str())
        );
        let at40 = render(&st(), 40, Some(last));
        let l = at40.lines().last().unwrap();
        assert_eq!(l, "  上次：已删 1 个，切到 rick-node.exa…");
        assert!(budget_width(l) <= line_limit(40));
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
