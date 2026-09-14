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

/// 主菜单的一次选择（键位见 spec §1.1、§1.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    SwitchNode,
    ToggleMode,
    ImportNode,
    Service,
    Check,
    /// `[6] 删除节点`（v4 的 [6] 检查更新挪进了 [7] 子页）。
    DeleteNode,
    /// `[7] 更新与维护`：检查更新、自动更新开关、从 v3 导入的子页。
    Maintenance,
    Uninstall,
    /// `[9] 节点测速`。T16 之前主菜单不显示这一行，按 9 只给一句 [`AUTO_UPDATE_MOVED`]。
    SpeedTest,
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
    /// 上一次检查更新的结论：有新版时主菜单在 `[7] 更新与维护` 后面挂 ★（spec §0.2 R6）。
    pub update_available: bool,
}

// ───────────── [7] 更新与维护子页与旧键过渡（spec §1.2、§8.4、§0.2 R1 / R6 / R15） ─────────────

/// `[7] 更新与维护` 子页里的一次选择。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaintAction {
    CheckUpdate,
    ToggleAuto,
    ImportV3,
    Back,
}

/// `[7]` 子页顶部三行只读的本地事实（spec §8.1 第一条）：不为画这一页去联网。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaintStatus {
    /// 本机 bui-c 的版本。
    pub version: String,
    /// 「上次检查」那一行的内容：`有新版（2 小时前）`、`已是最新（…）`、`还没检查过`。
    pub update_line: String,
    /// `profiles.auto_update`。
    pub auto_update: bool,
}

/// [6] 删除页顶的旧键过渡提示（spec §0.2 R1，D2）：给按 v3 的「6 高级设置」、v4 的「6 检查更新」
/// 进来的人。常量本身 ≤ 37 列（加 2 列缩进 ≤ 39），40 列终端一行放得下。下一个次版本删掉。
pub const MOVED_HINT_DELETE: &str = "（高级设置已取消，检查更新在 [7]）";
/// [9] 测速页顶的旧键过渡提示（spec §0.2 R1，T16 用）：给按 v4 的「9 自动更新」进来的人。
/// 口径同 [`MOVED_HINT_DELETE`]。
pub const MOVED_HINT_SPEEDTEST: &str = "（自动更新开关挪到了 [7]）";
/// [`Ctx`](crate::cli::Ctx) 的 `hints_shown` 按位记：[6] 删除页的过渡提示显示过。
pub const HINT_DELETE: u8 = 1 << 0;
/// 同上：[9] 测速页的过渡提示显示过（T16）。
pub const HINT_SPEEDTEST: u8 = 1 << 1;
/// T9 到 T16 之间按 9 给的那句 Note（spec §0.2 R15 逐字）。
pub const AUTO_UPDATE_MOVED: &str = "自动更新开关在 [7] 更新与维护 → [2]";
/// 进菜单前的 v3 导入邀请答了否（spec §11.1）。交互终端里它接着就被清屏抹掉、只活在「上次：」行
/// 里，所以菜单走法写在前头：40 列那一行只有 31 列，尾截之后 `[7] 更新与维护 → [3]` 还在；
/// 命令放后面，宽屏才看得到。命令不加反引号，60 列整句放得下。
pub const V3_SKIPPED: &str = "已跳过：[7] 更新与维护 → [3]，或跑 bui-c import-v3";
/// [4] 服务控制时还没有主单元（新机器）：装引擎与单元的路是导入节点。整句 31 列，40 列的
/// 「上次：」行正好放下（以前「还没有安装引擎与单元：…」39 列，尾截会砍掉「[3] 导入节点」）。
pub const NO_UNITS: &str = "还没装好引擎：先用 [3] 导入节点";
/// 同上、机器上还有 v3 客户端时另起一行（spec §11.1：拼成一句超 59 列）。停顿页上按宽度折行：
/// 60 列一行；40 列折在「→」后面，`[7] 更新与维护` 与 `[3] 从 v3 导入` 各自不被拆开。
pub const NO_UNITS_V3: &str = "有 v3 客户端：用 [7] 更新与维护 → [3] 从 v3 导入";

/// 「上次检查」后面的「多久以前」：不到 1 分钟（含时钟往回拨）写「刚刚」，其余只到分钟、小时、天。
pub fn ago(secs: i64) -> String {
    match secs {
        i64::MIN..=59 => "刚刚".to_string(),
        60..=3_599 => format!("{} 分钟前", secs / 60),
        3_600..=86_399 => format!("{} 小时前", secs / 3_600),
        _ => format!("{} 天前", secs / 86_400),
    }
}

/// `[7] 更新与维护` 子页（spec §3e-60-5、§8.4）：样式与 [4] 服务控制一致——前空一行、两列缩进的
/// 标题、5 列缩进的内容。顶部三行是本地事实，下面三个动作与返回；「上次检查」那一行放不下就尾截
/// （T10 会写成带版本号、来源的长句），编号与动作名都是固定短文案，40 列放得下。提示符
/// `选择 [0-3]` 由调用方问。
pub fn render_maint(m: &MaintStatus, width: usize) -> String {
    const FACT: &str = "     ";
    let fact = |head: &str, value: &str| {
        let lead = format!("{FACT}{head}   ");
        let room = line_limit(width).saturating_sub(budget_width(&lead));
        format!("{lead}{}\n", truncate_end(&sanitize(value), room))
    };
    let (auto, toggle) = if m.auto_update {
        ("开，每天一次", "关闭自动更新")
    } else {
        ("关", "开启自动更新")
    };
    let mut out = String::from("\n  更新与维护\n");
    out.push_str(&fact("当前版本", &m.version));
    out.push_str(&fact("上次检查", &m.update_line));
    out.push_str(&fact("自动更新", auto));
    out.push_str(&format!(
        "     [1] 检查更新\n     [2] {toggle}\n     [3] 从 v3 导入\n     [0] 返回\n"
    ));
    out
}

/// `[7]` 子页：空行与 `0` 返回，`1`–`3` 是动作（全角数字折半角），别的一律 `None`（调用方打一行
/// 错误、原地重问）。
pub fn parse_maint_choice(input: &str) -> Option<MaintAction> {
    match normalize_digits(input).as_str() {
        "" | "0" => Some(MaintAction::Back),
        "1" => Some(MaintAction::CheckUpdate),
        "2" => Some(MaintAction::ToggleAuto),
        "3" => Some(MaintAction::ImportV3),
        _ => None,
    }
}

/// `[7]` 子页输错：`无效选项：{x}（请输入 0-3 的数字）`，回显先净化再截到行宽。
pub fn invalid_maint_choice(input: &str, width: usize) -> String {
    bad_input_line("无效选项：", input, "（请输入 0-3 的数字）", width)
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

// ───────────── 墓碑（spec §5.7）：导入时被挡下的节点怎么说 ─────────────

/// 墓碑那一句里最多列几个名字（spec §5.7）：多出来的写成「等 N 个」。
pub const BURIED_LIST_MAX: usize = 3;

/// 被墓碑挡下的节点名列成一串：`a、b、c`；多于 [`BURIED_LIST_MAX`] 个时
/// `a、b、c 等 N 个`。名字先过 [`sanitize`]，不截断——折行交给调用方（菜单经
/// [`wrap`]，命令行交给终端）。
pub fn buried_list(names: &[String]) -> String {
    let head: Vec<String> = names
        .iter()
        .take(BURIED_LIST_MAX)
        .map(|n| sanitize(n).into_owned())
        .collect();
    let listed = head.join("、");
    if names.len() > BURIED_LIST_MAX {
        format!("{listed} 等 {} 个", names.len())
    } else {
        listed
    }
}

/// 导入之后、墓碑那一问上面的那一句（spec §5.7）：
/// `这次导入里有 N 个你删过的节点：a、b`。
///
/// 名字与问句分两行：连在一句里问，`  ▸ `、冒号与 `[y/N]` 加起来在 60 列只剩两列给名字，
/// 名字就永远露不出来了。这一句按当前宽度折行打，紧接着问 [`BURIED_ASK`]。
pub fn buried_head(names: &[String]) -> String {
    format!(
        "这次导入里有 {} 个你删过的节点：{}",
        names.len(),
        buried_list(names)
    )
}

/// 紧跟 [`buried_head`] 的那一问（spec §5.7；`[y/N]` 由 `Prompt::confirm` 自己补）。
/// 菜单在**放锁之后**问它（spec §0.2 R11），答 y 才另拿一次锁把这几个导回来。
pub const BURIED_ASK: &str = "要加回来吗？";

/// 命令行 `bui-c import` / `bui-c import-v3` 跳过墓碑时打的那一行（spec §5.7）：命令行不提问，
/// 说清跳了几个、是哪几个、怎么加回来。由终端自己折行，不截断。
pub fn buried_skipped(names: &[String]) -> String {
    format!(
        "跳过 {} 个删过的节点：{}（要加回用 --with-deleted）",
        names.len(),
        buried_list(names)
    )
}

/// 单独粘贴**一条**节点链接、而它之前被删过时的结果行（spec §5.7 表第 2 行）：
/// 粘一条就是明确要它，直接加回并清掉墓碑，不再问。
pub const BURIED_RESTORED: &str = "它之前被删过，已恢复";

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

/// 两列数字菜单块：`[1]`~`[8]` + 分隔线 + `[0] 退出`。两列排布任何宽度都不转单列（spec §2.2），
/// 跟着宽度变的只有分隔线。`[9] 节点测速` 这一行 T16 才加（spec §0.2 R15）。
pub fn render_options(st: &Status, width: usize) -> String {
    let mut out = String::new();
    // 有新版时 ★ 挂在左栏名字后面、不改名（R6）：「更新与维护 ★」显示 12 列、容量 13 列，
    // 放得进 14 列的左栏，右栏 [8] 不错位，40 列也放得下
    let maint = if st.update_available {
        "更新与维护 ★"
    } else {
        "更新与维护"
    };
    // [2] 直接写目标模式：「切换模式」不说切到哪边，用户得自己跟状态行对
    let to_mode = match st.mode {
        Mode::Tun => "切到 SOCKS",
        Mode::Socks => "切到 TUN",
    };
    let rows = [
        row("1", "切换节点", "2", to_mode),
        row("3", "导入节点", "4", "服务控制"),
        row("5", "连接检查", "6", "删除节点"),
        row("7", maint, "8", "卸载"),
    ];
    // 分隔线跟实际最宽的那行选项等宽：[2] 的目标模式会改变行宽，写死的线要么比选项长、
    // 要么短一截。再按容量口径封顶：`─` 在有的手机客户端上画成 2 列
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

/// 带编号的节点列表的左缩进：标准版式 5 列（与主菜单选项同列），窄版式 2 列。
fn list_margin(width: usize) -> &'static str {
    if is_narrow(width) {
        "  "
    } else {
        "     "
    }
}

/// 带编号的节点行开头 `{缩进}[n] `：两位数编号时 `[n]` 右补空格，名字仍对齐在同一列。全是 ASCII，
/// 字节数就是列数。节点列表（[1] 切换、[6] 删除页）与删除确认块的被删清单、可换节点都用它：
/// 编号的排法只有这一处，眼睛能直接对上。
fn number_lead(prof: &Profiles, i: usize, width: usize) -> String {
    let num_w = format!("[{}]", prof.profiles.len()).len();
    format!(
        "{}{} ",
        list_margin(width),
        pad(&format!("[{}]", i + 1), num_w)
    )
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
    let lead = |i: usize| {
        if with_back {
            number_lead(prof, i, width)
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
        out.push_str(&format!("{}[0] 返回\n", list_margin(width)));
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

/// 去首尾空白，再把全角 ASCII 折成半角（[`fold_fullwidth`]：数字、字母、`，`、`－`、`～` 等）。
/// 主菜单与子菜单的数字、删除页的编号、确认的回答都经它：全角折叠只有 `fold_fullwidth` 一处。
///
/// 中文输入法下 `１` 是常见误触：真机截屏里就被判成了「无效选项」。
fn normalize_digits(input: &str) -> String {
    fold_fullwidth(input.trim())
}

/// 主菜单只认数字，别的一律 `None`（调用方重画菜单）。
pub fn parse_choice(input: &str) -> Option<Action> {
    match normalize_digits(input).as_str() {
        "1" => Some(Action::SwitchNode),
        "2" => Some(Action::ToggleMode),
        "3" => Some(Action::ImportNode),
        "4" => Some(Action::Service),
        "5" => Some(Action::Check),
        "6" => Some(Action::DeleteNode),
        "7" => Some(Action::Maintenance),
        "8" => Some(Action::Uninstall),
        "9" => Some(Action::SpeedTest),
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

// [6] 删除节点：选择语法、确认输入与确认块（spec §5.2、§5.3、§0.2 R1）。
// 菜单与 `bui-c delete` 在终端里的确认块都从这里出；执行编排在 cli。

/// 全角 ASCII（U+FF01..=U+FF5E）折成半角：中文输入法下打出来的 `１２`、`ｙｅｓ`、`－`、`～`、`，`。
fn fold_fullwidth(s: &str) -> String {
    s.chars()
        .map(|c| match u32::from(c) {
            cp @ 0xFF01..=0xFF5E => char::from_u32(cp - 0xFEE0).unwrap_or(c),
            _ => c,
        })
        .collect()
}

/// 去掉数字串的前导零；全是零时留一个 `0`。调用方保证非空。
fn strip_zeros(digits: &str) -> &str {
    let t = digits.trim_start_matches('0');
    if t.is_empty() {
        "0"
    } else {
        t
    }
}

/// [`parse_selection`] 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// 空行、全是分隔符、单独的 0（含 `０`、`00`）：回主菜单，什么都不打。
    Back,
    /// 选中的节点下标（0-based），按列表顺序、去重。
    Picks(Vec<usize>),
}

/// 选择写错了：整行作废、原地重问，只报优先级最高的一类（spec §5.2）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelError {
    /// 看不懂的片段：只记第一个，原样（不折全角）。
    Junk(String),
    /// 0（返回）和编号写在了一起。
    ZeroMixed,
    /// 范围写反了（第一个；两头都是有的编号才算），规整成半角 `5-3`。
    Reversed(String),
    /// 没有这些编号（含解析溢出、范围里带 0），规整成半角 `10-12`、`99`，按输入顺序去重。
    OutOfRange(Vec<String>),
}

/// 回显的片段最多几列（spec §5.2：超过 12 列就尾截）。
const SEL_FRAG_MAX: usize = 12;

/// 选择报错文案的容量上限：固定文案不超过 59 列（spec §11 的 T6 测试）。
const SEL_MSG_MAX: usize = 59;

impl SelError {
    /// spec §5.2 表里的文案（不含缩进），接在输入下面一行、原地重问；`len` 拼进「可选 1-N」。
    /// 回显的片段先净化、超过 12 列尾截；越界的片段多到放不下时只列前几个，后面写「等 N 个」。
    /// 整句按容量口径不超过 59 列；40 列终端上由调用方用 [`wrap`] 折行。
    pub fn message(&self, len: usize) -> String {
        match self {
            SelError::Junk(x) => format!(
                "看不懂「{}」：只能写数字、逗号和 -",
                truncate_end(&sanitize(x), SEL_FRAG_MAX)
            ),
            SelError::ZeroMixed => "0 是返回，不能和编号写在一起".to_string(),
            SelError::Reversed(x) => {
                let x = sanitize(x);
                match x.split_once('-') {
                    Some((a, b)) => format!("范围写反了：{a}-{b}，要写成 {b}-{a}"),
                    None => format!("范围写反了：{x}"),
                }
            }
            SelError::OutOfRange(v) => {
                let head = "没有编号 ";
                let tail = match len {
                    0 => "（没有可选的编号）".to_string(),
                    1 => "（可选 1）".to_string(),
                    n => format!("（可选 1-{n}）"),
                };
                let room = SEL_MSG_MAX.saturating_sub(budget_width(head) + budget_width(&tail));
                let pieces: Vec<String> = v
                    .iter()
                    .map(|f| truncate_end(&sanitize(f), SEL_FRAG_MAX))
                    .collect();
                let mut list = pieces.join("、");
                if budget_width(&list) > room {
                    let more = format!(" 等 {} 个", pieces.len());
                    let mut shown = String::new();
                    for p in &pieces {
                        let next = if shown.is_empty() {
                            p.clone()
                        } else {
                            format!("{shown}、{p}")
                        };
                        if budget_width(&next) + budget_width(&more) > room {
                            break;
                        }
                        shown = next;
                    }
                    list = format!("{shown}{more}");
                }
                format!("{head}{list}{tail}")
            }
        }
    }
}

/// 选择里片段之间的分隔符：空白（含全角空格）、`,`、`，`、`、`。
fn is_sel_sep(c: char) -> bool {
    c.is_whitespace() || matches!(c, ',' | '，' | '、')
}

/// 范围的连接号（全角已折成半角）：`-`、`~`。
fn is_range_dash(c: char) -> bool {
    matches!(c, '-' | '~')
}

/// 一个片段的归类；看不懂的不在这里（[`sel_frag`] 返回 `None`）。
enum SelFrag {
    Zero,
    /// 0-based 闭区间，两头都是有的编号、不写反
    Range(usize, usize),
    Reversed(String),
    Out(String),
}

/// `N` 或 `A-B`（全角已折）；数字以外的字、多余或落单的连接号 → `None`（看不懂）。
fn sel_frag(raw: &str, len: usize) -> Option<SelFrag> {
    let s = normalize_digits(raw);
    let (a, b) = match s.split_once(is_range_dash) {
        Some((a, b)) => (a, Some(b)),
        None => (s.as_str(), None),
    };
    let digits = |t: &str| !t.is_empty() && t.bytes().all(|c| c.is_ascii_digit());
    if !digits(a) || b.is_some_and(|b| !digits(b)) {
        return None;
    }
    // 编号 1..=len 才算有；位数多到 usize 放不下也是没有
    let have = |t: &str| t.parse::<usize>().ok().filter(|n| (1..=len).contains(n));
    let a = strip_zeros(a);
    Some(match b.map(strip_zeros) {
        None if a == "0" => SelFrag::Zero,
        None => match have(a) {
            Some(n) => SelFrag::Range(n - 1, n - 1),
            None => SelFrag::Out(a.to_string()),
        },
        // 两头都是有的编号才叫写反（教人改成 3-5 才有意义）；否则先说没有这个编号
        Some(b) => match (have(a), have(b)) {
            (Some(x), Some(y)) if x > y => SelFrag::Reversed(format!("{a}-{b}")),
            (Some(x), Some(y)) => SelFrag::Range(x - 1, y - 1),
            _ => SelFrag::Out(format!("{a}-{b}")),
        },
    })
}

/// [6] 删除页的选择（spec §5.2）。片段之间用空白、`,`、`，`、`、` 隔开；片段是 `N` 或 `A-B`，
/// 连接号认 `-`、`－`、`~`、`～`；全角数字折半角，允许前导零（`03` 就是 3）。
///
/// - 空行、全是分隔符、单独的 0（含 `０`、`00`）→ [`Selection::Back`]；
/// - 重复与重叠静默去重（确认块会列出最终集合），结果按列表顺序；
/// - 只要有一个片段不对就整行作废，按优先级只报一类：看不懂（第一个）> 0 与编号混写 >
///   范围写反（第一个）> 越界（全部）。不做「只删合法的那几个」：破坏性操作不能部分执行。
pub fn parse_selection(input: &str, len: usize) -> std::result::Result<Selection, SelError> {
    let mut frags = Vec::new();
    for raw in input.split(is_sel_sep).filter(|f| !f.is_empty()) {
        // 看不懂的优先级最高：遇到第一个就报
        frags.push(sel_frag(raw, len).ok_or_else(|| SelError::Junk(raw.to_string()))?);
    }
    let zeros = frags.iter().filter(|f| matches!(f, SelFrag::Zero)).count();
    if zeros == frags.len() {
        return Ok(Selection::Back);
    }
    if zeros > 0 {
        return Err(SelError::ZeroMixed);
    }
    if let Some(r) = frags.iter().find_map(|f| match f {
        SelFrag::Reversed(r) => Some(r.clone()),
        _ => None,
    }) {
        return Err(SelError::Reversed(r));
    }
    let mut missing: Vec<String> = Vec::new();
    for s in frags.iter().filter_map(|f| match f {
        SelFrag::Out(s) => Some(s),
        _ => None,
    }) {
        if !missing.contains(s) {
            missing.push(s.clone());
        }
    }
    if !missing.is_empty() {
        return Err(SelError::OutOfRange(missing));
    }
    let mut chosen = vec![false; len];
    for f in &frags {
        if let SelFrag::Range(a, b) = f {
            chosen[*a..=*b].fill(true);
        }
    }
    Ok(Selection::Picks((0..len).filter(|&i| chosen[i]).collect()))
}

/// 删除确认那一问的回答（spec §5.3 的输入表、§0.2 R1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmInput {
    /// 确认删除。
    Yes,
    /// 打了一个编号（0-based，没核对范围）：「删完切到」形态下改替换目标；越界、在删除之列、
    /// 不在这个形态，由调用方分别报。越界时回显用户的原始输入，不要打 `i + 1`（见 [`parse_confirm`]）。
    Pick(usize),
    /// 空行、`n`、`0` 与其它一切：取消，默认否。
    Cancel,
    /// 会断网的删除只输了 `y`：按取消处理，调用方提示「会断网的删除要输入 yes」。
    NeedWord,
}

/// 解析删除确认的回答（先去首尾空白、全角折半角、不分大小写）：
///
/// - `yes`、「是」→ `Yes`；`y` 在 `needs_word`（会断网：删到当前节点、或删光）时是 `NeedWord`，否则 `Yes`；
/// - 一个编号 → `Pick(编号 − 1)`，前导零可以；越界（`i >= len`）照样交回，由调用方报「没有编号」。
///   位数多到 usize 放不下时交回 `Pick(len)`：这时 `i` 不是用户打的那个数，所以报越界时要回显用户的
///   原始输入（净化、截短后，例如 `SelError::OutOfRange(vec![输入.trim().into()]).message(len)`），
///   不要拿 `i + 1` 拼文案；
/// - `0`（菜单里的「返回」）、空行、`n`、几个编号、别的字 → `Cancel`。
///
/// 这一问用 `Prompt::line` 读、不用 `confirm`（要认编号）；菜单里不认全局 `-y`（D11）。
pub fn parse_confirm(input: &str, len: usize, needs_word: bool) -> ConfirmInput {
    let s = normalize_digits(input).to_ascii_lowercase();
    match s.as_str() {
        "yes" | "是" => ConfirmInput::Yes,
        "y" if needs_word => ConfirmInput::NeedWord,
        "y" => ConfirmInput::Yes,
        d if !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()) => match strip_zeros(d) {
            "0" => ConfirmInput::Cancel,
            n => ConfirmInput::Pick(n.parse::<usize>().map_or(len, |n| n - 1)),
        },
        _ => ConfirmInput::Cancel,
    }
}

/// `[6] 删除节点` 的一屏（spec §5.1）：前空一行、两列缩进的标题（带数量与 ★ 图例）；`hint` 是
/// 过渡提示（调用方只在每个菜单会话第一次进这一页时给），放在标题下一行、放不下就折行；再接与 [1]
/// 同一个带编号的 [`render_nodes`]，最下面一行是写法。「删除哪几个」由调用方问。
pub fn render_delete_picker(prof: &Profiles, width: usize, hint: Option<&str>) -> String {
    let mut out = format!("\n  删除节点（共 {} 个，★ 为当前）\n", prof.profiles.len());
    if let Some(h) = hint.filter(|h| !h.is_empty()) {
        out.push_str(&wrap(h, 2, width));
    }
    out.push_str(&render_nodes(prof, true, width));
    if !prof.profiles.is_empty() {
        out.push_str("  可多选：1 3 5 或 2-4，空行返回\n");
    }
    out
}

/// 折行时可以在它后面断开的中文标点。
const BREAK_AFTER: &str = "，：、；。";

/// 不能落在行首的标点（行首禁则）：硬折时把前一个字一起挪到下一行。
const NO_LINE_START: &str = "，：、；。？！）」";

/// 把 `text` 排成若干行（不含缩进与换行）：第一行最多 `first` 列、续行最多 `rest` 列（容量口径）。
///
/// 折点有两种：中文标点（`，：、；。`）之后；后面紧跟 ASCII 字的空格处（空格丢掉）。空格算不算折点
/// 要等下一个字到了才定，下一个字不是 ASCII 就不算：所以 `12 个`、`TUN 模式` 这种数字、ASCII 词与
/// 后面的中文不会拆开，要断就断在 `12` 前面。放不下时断在最后一个放得下的折点；最新的折点本身放不下
/// （比如溢出的正是那个「，」），就退回前一个折点，不硬折。一个折点都用不上时才按字硬折（[`hard_cut`]），
/// 拼回去一字不差：确认块里的名字、label、主机「完整不截」靠的就是它。
fn wrap_pieces(text: &str, first: usize, rest: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    // line 里的折点（字节位置，递增）：断在这里，前半收成一行，后半去掉开头的空格留给下一行
    let mut brks: Vec<usize> = Vec::new();
    // 最近一个还没定的空格：等下一个字到了再看它算不算折点
    let mut space: Option<usize> = None;
    for c in text.chars() {
        if c == ' ' {
            space = Some(line.len());
        } else if let Some(s) = space.take() {
            if c.is_ascii() {
                brks.push(s);
            }
        }
        line.push(c);
        if BREAK_AFTER.contains(c) {
            brks.push(line.len());
        }
        loop {
            let room = if lines.is_empty() { first } else { rest };
            if budget_width(line.trim_end()) <= room {
                break;
            }
            let fits = |b: usize| {
                let head = line[..b].trim_end();
                !head.is_empty() && budget_width(head) <= room
            };
            let cut = brks
                .iter()
                .rev()
                .copied()
                .find(|&b| fits(b))
                .unwrap_or_else(|| hard_cut(&line, room));
            let tail = line.split_off(cut);
            lines.push(line.trim_end().to_string());
            let kept = tail.trim_start();
            // 折点与待定的空格挪进新的一行：落在切掉的部分（含行首空格）里的丢掉
            let shift = cut + (tail.len() - kept.len());
            brks = brks
                .iter()
                .filter_map(|&b| b.checked_sub(shift))
                .filter(|&b| b > 0)
                .collect();
            space = space.and_then(|s| s.checked_sub(shift));
            line = kept.to_string();
        }
    }
    if !line.trim().is_empty() {
        lines.push(line.trim_end().to_string());
    }
    lines
}

/// 没有折点可用时的硬折位置：放得下的最长前缀。下一行会以 `，）」` 这类标点开头时（[`NO_LINE_START`]），
/// 把前一个字一起挪下去；至少留一个字，免得原地打转。
fn hard_cut(line: &str, room: usize) -> usize {
    let first = line.chars().next().map_or(0, char::len_utf8);
    let mut cut = prefix_within(line, room).len().max(first);
    while cut > first && line[cut..].starts_with(|c: char| NO_LINE_START.contains(c)) {
        cut -= line[..cut].chars().next_back().map_or(0, char::len_utf8);
    }
    cut
}

/// 一段文字按行宽上限折成几行：每行缩进 `indent` 列、以换行结尾，折法见 [`wrap_pieces`]。
/// 给放不下一行的固定文案与报错用（例如 40 列终端上的 [`SelError::message`]）；空文本返回空串。
pub fn wrap(text: &str, indent: usize, width: usize) -> String {
    let room = line_limit(width).saturating_sub(indent);
    let pad = " ".repeat(indent);
    wrap_pieces(text, room, room)
        .into_iter()
        .map(|l| format!("{pad}{l}\n"))
        .collect()
}

/// 确认块的三种形态（与删除计划的 Passive / Switch / Empty 同义，这里只按编号推断，给文案用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeleteForm {
    /// 活动节点不在删除之列（或本来就没有），还剩节点：不断网。
    Passive,
    /// 删到活动节点、还剩节点：切到替换节点。
    Switch,
    /// 一个不剩：代理停止。
    Empty,
}

/// 活动节点在列表里的下标；没有活动节点、或名字对不上时为 `None`。
fn active_index(prof: &Profiles) -> Option<usize> {
    let name = prof.active.as_deref()?;
    prof.profiles.iter().position(|p| p.name == name)
}

/// `picks` 已按列表顺序去重、都在范围里。
fn delete_form(prof: &Profiles, picks: &[usize]) -> DeleteForm {
    if picks.len() >= prof.profiles.len() {
        DeleteForm::Empty
    } else if active_index(prof).is_some_and(|a| picks.contains(&a)) {
        DeleteForm::Switch
    } else {
        DeleteForm::Passive
    }
}

/// 名字至少要留几列才带进提问：再少只剩「hy…70」这种认不出来的样子，不如退回「这 N 个节点」。
const QUESTION_NAME_MIN: usize = 8;

/// 确认的提问（交给 `Prompt::line`，不含 `  ▸ ` 与冒号）。R1：带上第一个名字，
/// `确认删除 {名字}？` / `确认删除 {名字} 等 N 个节点？`；会断网的写 `[yes/N]`，否则 `[y/N]`。
/// 连 `  ▸ ` 与冒号整行放不下时名字中间截断；可用不到 [`QUESTION_NAME_MIN`] 列时退回
/// `确认删除这 N 个节点？`（R1 的窄屏例外：40–44 列删几个长名字的节点时），被删清单就在上面几行。
/// 删单个节点时 40 列也有 15 列给名字。
fn delete_question(prof: &Profiles, picks: &[usize], form: DeleteForm, width: usize) -> String {
    let n = picks.len();
    let tag = if form == DeleteForm::Passive {
        "[y/N]"
    } else {
        "[yes/N]"
    };
    let plain = format!("确认删除这 {n} 个节点？{tag}");
    let Some(&first) = picks.first() else {
        return plain;
    };
    let name = sanitize(&prof.profiles[first].name);
    let after = if n == 1 {
        format!("？{tag}")
    } else {
        format!(" 等 {n} 个节点？{tag}")
    };
    let used = budget_width(&prompt_text(&format!("确认删除 {after}")));
    let room = line_limit(width).saturating_sub(used);
    if budget_width(&name) <= room {
        format!("确认删除 {name}{after}")
    } else if room >= QUESTION_NAME_MIN {
        format!("确认删除 {}{after}", truncate_middle(&name, room))
    } else {
        plain
    }
}

/// 确认块里被删节点的行（spec §5.3 第 2 条、§2.4）：`[编号] ★ 名字`，下面是 label 与服务器:端口。
///
/// 编号、缩进与 [`render_nodes`] 相同，眼睛能直接对上。名字中间截断；两个被删节点截断后一样
/// （全名其实不同）时改成全名、按行宽折行（§2.3 撞名保护）。label 与服务器:端口完整不截：
/// `label  kind  服务器:端口` 有一个放不下就整块去掉 kind（窄版式本来就不显示），还放不下就
/// 各占一行，一行仍放不下就折行。外来文字先过 [`sanitize`]。
fn victim_lines(prof: &Profiles, picks: &[usize], width: usize) -> Vec<String> {
    struct Victim {
        head: String,
        name: String,
        short: String,
        name_room: usize,
        label: String,
        hp: String,
        full: String,
    }
    let limit = line_limit(width);
    let narrow = is_narrow(width);
    // 详情行与折下来的名字缩进到名字起始列：`★ ` 在终端里是 2 列，与两个空格同宽
    let indent_w = number_lead(prof, 0, width).len() + 2;
    let indent = " ".repeat(indent_w);
    let room = limit.saturating_sub(indent_w);
    let active = active_index(prof);
    let victims: Vec<Victim> = picks
        .iter()
        .map(|&i| {
            let p = &prof.profiles[i];
            let mark = if active == Some(i) { "★ " } else { "  " };
            let head = format!("{}{mark}", number_lead(prof, i, width));
            let name_room = limit.saturating_sub(budget_width(&head));
            let name = sanitize(&p.name).into_owned();
            let short = truncate_middle(&name, name_room);
            let label = sanitize(&p.node.label).into_owned();
            let hp = sanitize(&format!("{}:{}", p.node.host, p.node.port)).into_owned();
            let full = join2(&[&label, kind_slug(p.node.kind), &hp]);
            Victim {
                head,
                name,
                short,
                name_room,
                label,
                hp,
                full,
            }
        })
        .collect();
    let show_kind = !narrow && victims.iter().all(|v| budget_width(&v.full) <= room);
    let mut out = Vec::new();
    for (k, v) in victims.iter().enumerate() {
        let clash = victims
            .iter()
            .enumerate()
            .any(|(j, w)| j != k && w.short == v.short && w.name != v.name);
        if clash {
            for (n, piece) in wrap_pieces(&v.name, v.name_room, room).iter().enumerate() {
                let lead = if n == 0 { v.head.as_str() } else { &indent };
                out.push(format!("{lead}{piece}"));
            }
        } else {
            out.push(format!("{}{}", v.head, v.short));
        }
        let both = join2(&[&v.label, &v.hp]);
        let detail: Vec<String> = if show_kind {
            vec![v.full.clone()]
        } else if budget_width(&both) <= room {
            vec![both]
        } else {
            [&v.label, &v.hp]
                .into_iter()
                .flat_map(|s| wrap_pieces(s, room, room))
                .collect()
        };
        out.extend(
            detail
                .into_iter()
                .filter(|l| !l.is_empty())
                .map(|l| format!("{indent}{l}")),
        );
    }
    out
}

/// 可换节点的 label 至少要剩几列才显示（spec §5.3：少于 5 列就不显示）。
const ALT_LABEL_MIN: usize = 5;

/// 删到当前节点时逐行列出的可换节点（spec §5.3 第 3 条）：`[编号] 名字  label`，编号保留原来的，
/// 列表滚出屏幕也能照着打。名字中间截断；label 尾截，剩不到 5 列就不显示；窄版式只显示名字。
fn alternative_lines(prof: &Profiles, remaining: &[usize], width: usize) -> Vec<String> {
    let limit = line_limit(width);
    let narrow = is_narrow(width);
    remaining
        .iter()
        .map(|&k| {
            let p = &prof.profiles[k];
            let lead = number_lead(prof, k, width);
            let room = limit.saturating_sub(budget_width(&lead));
            let name = truncate_middle(&sanitize(&p.name), room);
            let label = sanitize(&p.node.label);
            let label_room = room.saturating_sub(budget_width(&name) + 2);
            if narrow || label.is_empty() || label_room < ALT_LABEL_MIN {
                format!("{lead}{name}")
            } else {
                format!("{lead}{name}  {}", truncate_end(&label, label_room))
            }
        })
        .collect()
}

/// 删除确认块拆成两段给菜单用：`body` 原样打出，`question` 交给 `Prompt::line`（它自己加
/// `  ▸ ` 与冒号），`needs_word` 交给 [`parse_confirm`]。提示里的 `[yes/N]` 与行为出自同一处，
/// 对得上。确认时改了替换目标、输错了编号，重问用的还是同一个 `question`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteConfirm {
    /// 以空行开头、每行以换行结尾，不含提问。
    pub body: String,
    /// `确认删除 {名字}？[y/N]` 这类提问，不含 `  ▸ ` 与冒号。
    pub question: String,
    /// 会断网（删到当前节点且还剩节点，或删光）：要输入 `yes` 或「是」才算确认。
    pub needs_word: bool,
}

/// 删除确认块（spec §5.3，按 §0.2 R1 排），菜单用。`picks` 是列表下标（0-based；顺序与重复不要紧，
/// 这里按列表顺序去重，越界的忽略）。
///
/// 顺序：① 删到当前节点时，`删完切到 [n] 名字`；剩下的节点都在被删节点所在的服务器上时，下面多一行
/// 说明（剩下的只在 1 台上：`剩下的节点都在同一台服务器上`；分在多台上：`剩下的节点都在被删节点所在的
/// 服务器上`）；再是可换节点（逐行 `[k] 名字  label`，只剩替换目标一个时不列）；② `将删除 N 个节点…`
/// 与被删清单；③ 影响说明（不含当前节点：删完还剩 M 个、不会断网；删到当前节点：TUN 下断网几秒；
/// 删光：代理会停止，TUN 下本机直连）与复活说明（§5.3 第 4 条）；④ 提问。被删清单永远紧挨着提问。
///
/// `rows` 传 `ctx.rows()` 的原值，函数里自己减 2（留给提示符与最后一行），调用方不要再减。整块（连开头的
/// 空行与提问）超过 `rows − 2` 行时，可换节点压成一行 `可换：1 4 5（编号同上面列表）`：手机横屏只有约 17 行。
///
/// 前提：删到当前节点、还剩节点时，`replacement` 必须是剩下的节点之一（调用方按 `delete::default_to`
/// 或用户打的编号算好）。给 `None`、或给了被删的节点，是调用方的错：debug 构建里 panic，发布构建里
/// 只是少打「删完切到」那一行。别的形态不看 `replacement`。
pub fn delete_confirm(
    prof: &Profiles,
    picks: &[usize],
    replacement: Option<usize>,
    width: usize,
    rows: usize,
) -> DeleteConfirm {
    confirm_block(prof, picks, replacement, width, Some(rows))
}

/// `bui-c delete` 在终端里的确认块（spec §5.10）。命令行不认编号（换目标用 `--switch-to`），所以不列
/// 可换节点：没有「想换就输入下面的编号：」，也不压成「可换：…」。其余与 [`delete_confirm`] 逐字相同：
/// 「删完切到」与同服务器那一行都在，`question` 与 `needs_word` 一样。没有行数参数：命令行一次打完。
/// `to` 的前提同 [`delete_confirm`] 的 `replacement`。
pub fn delete_confirm_cli(
    prof: &Profiles,
    picks: &[usize],
    to: Option<usize>,
    width: usize,
) -> DeleteConfirm {
    confirm_block(prof, picks, to, width, None)
}

/// [`delete_confirm`] 与 [`delete_confirm_cli`] 共用的实现；`rows` 为 `None` 是命令行版（不列可换节点）。
fn confirm_block(
    prof: &Profiles,
    picks: &[usize],
    replacement: Option<usize>,
    width: usize,
    rows: Option<usize>,
) -> DeleteConfirm {
    let len = prof.profiles.len();
    let mut picks: Vec<usize> = picks.iter().copied().filter(|&i| i < len).collect();
    picks.sort_unstable();
    picks.dedup();
    let remaining: Vec<usize> = (0..len)
        .filter(|i| picks.binary_search(i).is_err())
        .collect();
    let form = delete_form(prof, &picks);
    debug_assert!(
        form != DeleteForm::Switch || replacement.is_some_and(|r| remaining.contains(&r)),
        "删到当前节点时要给一个剩下的节点作替换目标，给的是 {replacement:?}"
    );
    let limit = line_limit(width);
    let n = picks.len();
    let tun = prof.mode == Mode::Tun;
    // 缩进 2 列的一句，放不下就折行
    let say = |text: &str| -> Vec<String> {
        let room = limit.saturating_sub(2);
        wrap_pieces(text, room, room)
            .into_iter()
            .map(|l| format!("  {l}"))
            .collect()
    };

    // ① 删完切到哪个、还能换哪个
    let mut switch_to = Vec::new();
    let (mut alts, mut alts_short) = (Vec::new(), Vec::new());
    if form == DeleteForm::Switch {
        if let Some(r) = replacement.filter(|r| remaining.contains(r)) {
            switch_to.push(render_switch_to(prof, r, width).trim_end().to_string());
        }
        // R1：剩下的节点都在被删节点所在的服务器上（host 转小写后比较）时多一行说明。条件与默认替换
        // 目标「都没有就取剩下第一个」那一分支相同；说的是剩下的节点本身，替换目标从哪儿来都成立。
        // 剩下的只在 1 台上才说「同一台」，分在多台上说「被删节点所在的服务器」
        let host = |i: &usize| prof.profiles[*i].node.host.to_lowercase();
        let gone: std::collections::HashSet<String> = picks.iter().map(host).collect();
        let left: std::collections::HashSet<String> = remaining.iter().map(host).collect();
        if left.is_subset(&gone) {
            switch_to.extend(say(if left.len() == 1 {
                "剩下的节点都在同一台服务器上"
            } else {
                "剩下的节点都在被删节点所在的服务器上"
            }));
        }
        // 命令行版不认编号，不列可换节点
        if rows.is_some() && remaining.len() > 1 {
            alts = say("想换就输入下面的编号：");
            alts.extend(alternative_lines(prof, &remaining, width));
            let nums: Vec<String> = remaining.iter().map(|k| (k + 1).to_string()).collect();
            let text = format!("可换：{}（编号同上面列表）", nums.join(" "));
            // 续行对齐到「可换：」后面
            let hang = 2 + budget_width("可换：");
            alts_short = wrap_pieces(&text, limit.saturating_sub(2), limit.saturating_sub(hang))
                .into_iter()
                .enumerate()
                .map(|(i, l)| {
                    let lead = if i == 0 { 2 } else { hang };
                    format!("{}{l}", " ".repeat(lead))
                })
                .collect();
        }
    }

    // ② 被删清单
    let mut tail = say(&match form {
        DeleteForm::Empty => format!("将删除全部 {n} 个节点："),
        DeleteForm::Switch => format!("将删除 {n} 个节点，含当前节点："),
        DeleteForm::Passive => format!("将删除 {n} 个节点："),
    });
    tail.extend(victim_lines(prof, &picks, width));

    // ③ 影响说明与复活说明
    match form {
        DeleteForm::Passive => {
            let m = remaining.len();
            tail.extend(say(&if active_index(prof).is_some() {
                format!("删完还剩 {m} 个，当前节点不变，不会断网")
            } else {
                format!("删完还剩 {m} 个，不会断网")
            }));
        }
        DeleteForm::Switch => {
            if tun {
                tail.extend(say("TUN 模式：切换时会断网几秒"));
            }
        }
        DeleteForm::Empty => {
            tail.extend(say("删完就没有节点了，代理会停止"));
            if tun {
                tail.extend(say("TUN 撤掉后本机直连，国外网站打不开"));
            }
        }
    }
    let them = if n == 1 { "它" } else { "它们" };
    tail.extend(say(&format!("以后导入时会先跳过{them}，再问你要不要加回")));

    // ④ 提问；整块超过 rows − 2 行就把可换节点压成一行
    let question = delete_question(prof, &picks, form, width);
    let total = 1 + switch_to.len() + alts.len() + tail.len() + 1;
    let alts = match rows {
        Some(rows) if total > rows.saturating_sub(2) => alts_short,
        _ => alts,
    };
    let mut body = String::from("\n");
    for l in switch_to.iter().chain(&alts).chain(&tail) {
        body.push_str(l);
        body.push('\n');
    }
    DeleteConfirm {
        body,
        question,
        needs_word: form != DeleteForm::Passive,
    }
}

/// 菜单版删除确认块连同提问那一行，就是屏幕上看到的样子（最后一行 `  ▸ 确认删除 …？[yes/N]：`）。
/// 宽度守门表与测试用它；菜单要分开打正文与提问，用 [`delete_confirm`]；命令行用 [`delete_confirm_cli`]。
pub fn render_delete_confirm(
    prof: &Profiles,
    picks: &[usize],
    replacement: Option<usize>,
    width: usize,
    rows: usize,
) -> String {
    let c = delete_confirm(prof, picks, replacement, width, rows);
    format!("{}{}\n", c.body, prompt_text(&c.question))
}

/// `  删完切到 [n] 名字`（含换行）：确认块①的第一行；确认时打编号改了替换目标，菜单再打一行
/// 同样的。名字中间截断到行宽上限；编号不存在时返回空串。
pub fn render_switch_to(prof: &Profiles, to: usize, width: usize) -> String {
    let Some(p) = prof.profiles.get(to) else {
        return String::new();
    };
    let head = format!("  删完切到 [{}] ", to + 1);
    let room = line_limit(width).saturating_sub(budget_width(&head));
    format!("{head}{}\n", truncate_middle(&sanitize(&p.name), room))
}

/// 连接检查失败后「下一步」小菜单里的一次选择（spec §6.5、§0.2 R3）。键固定，不随状态漂移。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextStep {
    Recheck,
    SpeedTest,
    Journal,
    Back,
}

/// 小菜单的「下一步：」与四个选项（不含提示符 `选择 [0-3]`）。[2] 在节点测速（T16）落地之前是
/// 「换个节点」，进 [1] 的列表；T16 把它换成测速，编号不变（spec §6.5）。
pub fn render_next_step() -> String {
    format!(
        "  下一步：\n     [1] 再查一次\n     [2] 换个节点\n     [3] 看最近 {SERVICE_LOG_LINES} 行日志\n     [0] 返回菜单\n"
    )
}

/// `1` 再查一次、`2` 换个节点（T16 起是测速）、`3` 看日志；空行与 `0` 返回菜单（全角数字折半角）。
/// 别的一律 `None`：调用方打一行错误、原地重问，不重画报告。
pub fn parse_next_step(input: &str) -> Option<NextStep> {
    match normalize_digits(input).as_str() {
        "" | "0" => Some(NextStep::Back),
        "1" => Some(NextStep::Recheck),
        "2" => Some(NextStep::SpeedTest),
        "3" => Some(NextStep::Journal),
        _ => None,
    }
}

/// 小菜单里输错的那一行，不含缩进（调用方经 `say` 在菜单里加两列）：回显的输入先净化再尾截，
/// 整行按容量口径不超过行宽上限。
pub fn invalid_next_step(input: &str, width: usize) -> String {
    const HEAD: &str = "无效选项：";
    const TAIL: &str = "（请输入 0-3 的数字）";
    // 2 = 菜单里 `say` 加的缩进
    let room = line_limit(width).saturating_sub(2 + budget_width(HEAD) + budget_width(TAIL));
    format!("{HEAD}{}{TAIL}", truncate_end(&sanitize(input), room))
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
            assert!(out.contains("[6] 删除节点"));
            assert!(out.contains("[7] 更新与维护"));
            assert!(out.contains("[8] 卸载"));
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
        // 写死 34 列：SOCKS 模式（[2] 切到 TUN）比选项长 2 列；有新版的 ★ 挂在左栏 [7] 后面，
        // 不改最宽行，这一形态照样核对一遍
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

    /// 主菜单重排（spec §1.1、§1.2、§0.2 R6、R15）：[6] 删除节点、[7] 更新与维护；T16 之前
    /// 不显示 [9] 行，按 9 由调用方给一句 Note。v4 挪走的三件（检查更新、从 v3 导入、自动更新
    /// 开关）不再出现在主菜单上。
    #[test]
    fn menu_numbers_follow_the_new_layout() {
        assert_eq!(parse_choice("6"), Some(Action::DeleteNode));
        assert_eq!(parse_choice("7"), Some(Action::Maintenance));
        assert_eq!(parse_choice("9"), Some(Action::SpeedTest));
        assert_eq!(parse_choice("８"), Some(Action::Uninstall));
        for w in [40, 50, 59, 80] {
            let out = render(&st(), w, None);
            assert!(out.contains("[6] 删除节点"), "@{w}\n{out}");
            assert!(out.contains("[7] 更新与维护"), "@{w}\n{out}");
            assert!(!out.contains("[9]"), "T16 之前不显示 [9] 行 @{w}\n{out}");
            for gone in ["检查更新", "从 v3 导入", "自动更新"] {
                assert!(!out.contains(gone), "{gone} 挪进了 [7] 子页 @{w}\n{out}");
            }
        }
    }

    /// 有新版时 ★ 挂在左栏 `[7] 更新与维护` 后面（R6，不改名）：40 列放得下，右栏 [8] 不错位。
    #[test]
    fn update_marker_hangs_on_maintenance_and_fits_40_columns() {
        assert!(!render(&st(), 80, None).contains('★'));
        let up = Status {
            update_available: true,
            ..st()
        };
        for w in [40, 50, 59, 80] {
            let out = render(&up, w, None);
            let row = out.lines().find(|l| l.contains("[7]")).unwrap();
            assert!(row.contains("[7] 更新与维护 ★"), "@{w}：{row}");
            assert!(row.contains("[8] 卸载"), "@{w}：{row}");
            assert!(budget_width(row) <= line_limit(w), "@{w}：{row}");
            assert!(!out.contains("有新版"), "主菜单只挂 ★ @{w}\n{out}");
            // 右栏起点按显示列宽对齐（★ 真机上 1 列）
            let col = |l: &str, key: &str| display_width(&l[..l.find(key).unwrap()]);
            let six = out.lines().find(|l| l.contains("[6]")).unwrap();
            assert_eq!(col(row, "[8]"), col(six, "[6]"), "@{w}\n{out}");
        }
    }

    /// [7] 子页（spec §3e-60-5、§8.4）：顶部三行只读本地事实，下面三个动作 + 返回；[2] 的文案跟着
    /// 开关变。
    #[test]
    fn maint_page_shows_facts_then_three_actions() {
        let on = MaintStatus {
            version: "4.0.0".into(),
            update_line: "有新版（2 小时前）".into(),
            auto_update: true,
        };
        assert_eq!(
            render_maint(&on, 60),
            "\n  更新与维护\n     当前版本   4.0.0\n     上次检查   有新版（2 小时前）\n     自动更新   开，每天一次\n     [1] 检查更新\n     [2] 关闭自动更新\n     [3] 从 v3 导入\n     [0] 返回\n"
        );
        let off = MaintStatus {
            auto_update: false,
            ..on.clone()
        };
        let out = render_maint(&off, 60);
        assert!(out.contains("     自动更新   关\n"), "{out}");
        assert!(out.contains("     [2] 开启自动更新\n"), "{out}");
        // 40 列：上次检查那一行尾截，编号与动作名一个不少
        let long = MaintStatus {
            update_line: "有新版 4.0.1-rc12（来源 面板，365 天前）".into(),
            ..on
        };
        let out = render_maint(&long, 40);
        for l in out.lines() {
            assert!(budget_width(l) <= line_limit(40), "{l:?}\n{out}");
        }
        for key in [
            "[1] 检查更新",
            "[2] 关闭自动更新",
            "[3] 从 v3 导入",
            "[0] 返回",
        ] {
            assert!(out.contains(key), "{key}\n{out}");
        }
    }

    #[test]
    fn maint_choice_parses_numbers_and_back() {
        assert_eq!(parse_maint_choice("1"), Some(MaintAction::CheckUpdate));
        assert_eq!(parse_maint_choice(" ２ "), Some(MaintAction::ToggleAuto));
        assert_eq!(parse_maint_choice("3"), Some(MaintAction::ImportV3));
        assert_eq!(parse_maint_choice("0"), Some(MaintAction::Back));
        assert_eq!(parse_maint_choice(""), Some(MaintAction::Back));
        assert_eq!(parse_maint_choice("4"), None);
        assert_eq!(parse_maint_choice("x"), None);
        let bad = invalid_maint_choice("x", 80);
        assert!(bad.contains("0-3"), "{bad}");
    }

    /// 「上次检查」后面的「多久以前」：只到分钟、小时、天，时钟往回拨算刚刚。
    #[test]
    fn ago_is_coarse_and_never_negative() {
        assert_eq!(ago(-5), "刚刚");
        assert_eq!(ago(59), "刚刚");
        assert_eq!(ago(60), "1 分钟前");
        assert_eq!(ago(3599), "59 分钟前");
        assert_eq!(ago(3600), "1 小时前");
        assert_eq!(ago(86_399), "23 小时前");
        assert_eq!(ago(86_400), "1 天前");
    }

    /// 旧键过渡提示（spec §0.2 R1）：常量本身 ≤ 37 列，加 2 列缩进 ≤ 39，40 列终端一行放得下。
    #[test]
    fn moved_hints_fit_one_line_at_40_columns() {
        for hint in [MOVED_HINT_DELETE, MOVED_HINT_SPEEDTEST] {
            assert!(budget_width(hint) <= 37, "{hint} = {}", budget_width(hint));
            assert_eq!(wrap(hint, 2, 40), format!("  {hint}\n"), "不折行");
            assert!(hint.contains("[7]"), "{hint}");
        }
    }

    /// 挪了位置的入口只活在「上次：」行里（跳过 v3 导入、按 9、没有单元）：固定文案 ≤ 59 列，
    /// 菜单走法写在前头，40 列尾截之后可操作的那半还在。
    #[test]
    fn moved_entries_keep_the_menu_path_in_the_last_line_at_40_columns() {
        for text in [V3_SKIPPED, AUTO_UPDATE_MOVED, NO_UNITS, NO_UNITS_V3] {
            assert!(budget_width(text) <= 59, "{text} = {}", budget_width(text));
        }
        let last = |text: &str, w: usize| {
            render(&st(), w, Some(text))
                .lines()
                .find(|l| l.starts_with("  上次："))
                .unwrap()
                .to_string()
        };
        assert!(
            V3_SKIPPED.find("[7]") < V3_SKIPPED.find("bui-c import-v3"),
            "先说菜单怎么走，再说命令：{V3_SKIPPED}"
        );
        assert!(last(V3_SKIPPED, 40).contains("[7] 更新与维护 → [3]"));
        assert!(last(NO_UNITS, 40).contains("[3] 导入节点"));
        assert!(last(AUTO_UPDATE_MOVED, 40).contains("[7] 更新与维护"));
        assert!(last(AUTO_UPDATE_MOVED, 50).contains("[7] 更新与维护 → [2]"));
        // NO_UNITS_V3 在停顿页上按宽度折行打：60 列一行；40 列折开时编号与名字不拆开
        let page = wrap(NO_UNITS_V3, 2, 60);
        assert!(
            page.lines()
                .any(|l| l.contains("[7] 更新与维护 → [3] 从 v3 导入")),
            "{page}"
        );
        let page = wrap(NO_UNITS_V3, 2, 40);
        for key in ["[7] 更新与维护", "[3] 从 v3 导入"] {
            assert!(page.lines().any(|l| l.contains(key)), "{key}\n{page}");
        }
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
        // 没有节点、SOCKS 模式服务停了且端口是 5 位数。T9 起还有「有新版」形态：★ 挂在左栏
        // `[7] 更新与维护` 后面（以前右栏的「检查更新 ★ 有新版」在 40–47 列放不下）
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
        let update = Status {
            update_available: true,
            ..st()
        };
        out.push(("update", render(&update, width, None)));
        out.push((
            "update-socks",
            render(
                &Status {
                    update_available: true,
                    ..socks.clone()
                },
                width,
                Some(V3_SKIPPED),
            ),
        ));
        // T9：[7] 更新与维护子页（开关两种、上次检查的几种说法，含 T10 会写成的带版本号的长句）、
        // 子页输错、旧键过渡提示、只活在「上次：」行里的几句挪了位置的引导
        for auto_update in [true, false] {
            for update_line in [
                "还没检查过".to_string(),
                format!("已是最新（{}）", ago(59 * 60)),
                format!("有新版（{}）", ago(23 * 3600)),
                "有新版 4.0.1-rc12（来源 面板，365 天前）".to_string(),
            ] {
                let m = MaintStatus {
                    version: "4.0.0-rc12".into(),
                    update_line,
                    auto_update,
                };
                out.push(("maint", render_maint(&m, width)));
            }
        }
        for hint in [MOVED_HINT_DELETE, MOVED_HINT_SPEEDTEST] {
            out.push(("moved-hint", format!("  {hint}\n")));
        }
        for text in [V3_SKIPPED, AUTO_UPDATE_MOVED, NO_UNITS] {
            out.push(("moved-last", render(&st(), width, Some(text))));
        }
        out.push(("no-units-v3", wrap(NO_UNITS_V3, 2, width)));
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
            out.push((
                "invalid-maint",
                format!("  {}\n", invalid_maint_choice(input, width)),
            ));
        }
        // T6：删除页（有无过渡提示）与确认块（Passive / Switch / Empty，SOCKS 与 TUN，行数够与不够）
        let del = baiyi_like();
        out.push(("delete-picker", render_delete_picker(&del, width, None)));
        out.push((
            "delete-picker-hint",
            render_delete_picker(&del, width, Some(MOVED_HINT_DELETE)),
        ));
        let all: Vec<usize> = (0..del.profiles.len()).collect();
        for mode in [Mode::Socks, Mode::Tun] {
            let mut p = baiyi_like();
            p.mode = mode;
            for rows in [17, 40] {
                let passive = render_delete_confirm(&p, &[2], None, width, rows);
                out.push(("delete-passive", passive));
                let many = render_delete_confirm(&p, &[0, 2, 3, 4, 5, 6, 7, 8], None, width, rows);
                out.push(("delete-passive-many", many));
                let switch = render_delete_confirm(&p, &[1, 2], Some(3), width, rows);
                out.push(("delete-switch", switch));
                out.push((
                    "delete-empty",
                    render_delete_confirm(&p, &all, None, width, rows),
                ));
            }
            for i in 0..p.profiles.len() {
                p.active = Some(p.profiles[i].name.clone());
                let to = (i + 1) % p.profiles.len();
                let each = render_delete_confirm(&p, &[i], Some(to), width, 40);
                out.push(("delete-switch-each", each));
            }
        }
        // T6 补充：剩下的节点都在被删节点的服务器上，确认块多一行说明
        let mut same = baiyi_like();
        same.profiles.retain(|p| p.node.host == "tizi.example.test");
        for mode in [Mode::Socks, Mode::Tun] {
            same.mode = mode;
            for rows in [17, 40] {
                let note = render_delete_confirm(&same, &[1], Some(0), width, rows);
                out.push(("delete-switch-same-host", note));
            }
        }
        // 审查后补：命令行版确认块，两位数编号（编号列宽 4），撞名改全名后折行，长 label 折行
        let del = baiyi_like();
        for (p, picks, to) in [(&del, vec![1, 2], Some(3)), (&same, vec![1], Some(0))] {
            let cli = delete_confirm_cli(p, &picks, to, width);
            out.push((
                "delete-cli",
                format!("{}{}\n", cli.body, prompt_text(&cli.question)),
            ));
        }
        let mut many = baiyi_like();
        for k in 10..=12 {
            let name = format!("tizi.example.test-hy2-direct-{k}");
            many.profiles.push(named(&name, hy2_direct_node()));
        }
        let every: Vec<usize> = (0..many.profiles.len()).collect();
        out.push(("delete-picker-12", render_delete_picker(&many, width, None)));
        for rows in [17, 40] {
            let passive = render_delete_confirm(&many, &[9, 10, 11], None, width, rows);
            out.push(("delete-passive-12", passive));
            let switch = render_delete_confirm(&many, &[1, 11], Some(9), width, rows);
            out.push(("delete-switch-12", switch));
            let empty = render_delete_confirm(&many, &every, None, width, rows);
            out.push(("delete-empty-12", empty));
        }
        let mut clash = baiyi_like();
        for place in ["tokyo", "osaka"] {
            let name = format!("rick-node.example-a.net-{place}-hy2-residential-direct");
            clash.profiles.push(named(&name, hy2_direct_node()));
        }
        let passive = render_delete_confirm(&clash, &[9, 10], None, width, 40);
        out.push(("delete-clash", passive));
        clash.active = Some(clash.profiles[9].name.clone());
        let switch = render_delete_confirm(&clash, &[9, 10], Some(0), width, 40);
        out.push(("delete-clash-switch", switch));
        let mut long = baiyi_like();
        long.mode = Mode::Tun;
        let label = "示例专用名-一个特别特别长的备注-给家里人看的-不要删";
        long.profiles[1].node.label = label.into();
        long.profiles[2].node.label = label.into();
        let switch = render_delete_confirm(&long, &[1, 2], Some(3), width, 40);
        out.push(("delete-long-label", switch));
        let passive = render_delete_confirm(&long, &[2], None, width, 40);
        out.push(("delete-long-label-passive", passive));
        // T7：删除执行路径新出现的整屏与行——失败的停顿页（预检不过 / 内核缺失 / 切换失败并
        // 换回 / 换回也失败 / 停不下来 / 快照变了）、确认时的几句提示、删除后的「上次：」行。
        // 文案都从 crate::delete 取，屏上打的就是这几句（cli 经 delete::page 折行）
        use crate::delete;
        let victim = "rick-node.example-a.net-reality-direct";
        let pages: Vec<Vec<String>> = vec![
            vec![
                format!("删除没做：切到 {victim} 的配置校验不通过"),
                "sing-box check 不通过：outbounds[0]: 解析失败".to_string(),
                delete::STILL_THERE.to_string(),
                delete::NEXT_PICK_ANOTHER.to_string(),
            ],
            vec![
                format!("删除没做：内核缺失，切不到 {victim}"),
                "内核缺失：/opt/bui-c/bin/sing-box，先跑 `bui-c update` 安装 sing-box".to_string(),
                delete::STILL_THERE.to_string(),
                delete::NEXT_KERNEL.to_string(),
            ],
            vec![
                format!("删除没做：切到 {victim} 失败"),
                "bui-tun 接口 5 秒内没起来".to_string(),
                delete::ROLLED_BACK.to_string(),
            ],
            vec![
                "删除没做：写 profiles.json 失败".to_string(),
                "读写 /opt/bui-c/profiles.json 失败：permission denied".to_string(),
                format!(
                    "{}：命令 systemctl restart bui-c.service 执行失败（退出码 1）",
                    delete::ROLLBACK_FAILED
                ),
                delete::ROLLBACK_NEXT.to_string(),
            ],
            vec![
                "删除没做：停止代理失败：bui-c.service 还在跑".to_string(),
                delete::STILL_THERE.to_string(),
            ],
            vec![delete::SNAPSHOT_CHANGED.to_string()],
            // 回滚写回了旧配置、接口还是没起来（R10 的判据在回滚方向一样算数）
            vec![
                format!("删除没做：切到 {victim} 失败"),
                "bui-tun 接口 5 秒内没起来".to_string(),
                delete::ROLLED_BACK_TUN_DOWN.to_string(),
                delete::ROLLBACK_NEXT.to_string(),
            ],
            // 删光：数据面拆完了、profiles 写不进去（代理已停、条目还在）
            vec![
                delete::STOPPED_NOT_SAVED_HEAD.to_string(),
                "读写 /opt/bui-c/profiles.json 失败：permission denied".to_string(),
                delete::STOPPED_NOT_SAVED.to_string(),
            ],
            // Passive：数据面没动过，写盘失败也给页
            vec![
                delete::SAVE_FAILED.to_string(),
                "读写 /opt/bui-c/profiles.json 失败：permission denied".to_string(),
                delete::STILL_THERE.to_string(),
            ],
        ];
        for p in &pages {
            out.push(("delete-failed", delete::page(p, width)));
        }
        for line in [
            delete::CANCELLED.to_string(),
            delete::NEEDS_YES.to_string(),
            delete::UFW_LEFT.to_string(),
            delete::NO_NODES.to_string(),
            delete::only_y(true),
            delete::only_y(false),
            delete::also_a_target(11),
            format!("正在切到 {victim}…"),
            "正在停止代理…".to_string(),
            "已安装 sing-box 内核".to_string(),
            SelError::OutOfRange(vec!["99999999999999999999".to_string()]).message(9),
            SelError::Junk("a".to_string()).message(9),
            SelError::Reversed("5-3".to_string()).message(9),
            SelError::ZeroMixed.message(9),
            delete::CLI_NO_NUMBER.to_string(),
            delete::SWITCH_TO_UNUSED.to_string(),
            // `bui-c delete` 成功那一句（三形态）：命令行由终端自己折行，收进这张表只为守住
            // 字符归类与「折得开」，屏上排版以菜单那几行为准
            delete::cli_summary(&delete::Report {
                deleted: vec!["HY2".to_string()],
                active: Some(victim.to_string()),
                switched: true,
                stopped: false,
                remaining: 7,
            }),
            delete::cli_summary(&delete::Report {
                deleted: vec!["HY2".to_string()],
                active: Some("hysteria2-1778329470".to_string()),
                switched: false,
                stopped: false,
                remaining: 8,
            }),
            delete::cli_summary(&delete::Report {
                deleted: vec!["HY2".to_string()],
                active: None,
                switched: false,
                stopped: true,
                remaining: 0,
            }),
        ] {
            out.push(("delete-line", delete::page(&[line], width)));
        }
        // 失败时进「上次：」行的短摘要：不带节点名的几条要整句放下，尾截会砍掉可操作的那
        // 半句（spec §0.2 R6；`delete::tests` 里另有一条不许被截的断言）
        for last in [
            delete::SNAPSHOT_CHANGED_SHORT,
            delete::SAVE_FAILED,
            delete::TEARDOWN_FAILED_SHORT,
            delete::STOPPED_NOT_SAVED_SHORT,
        ] {
            out.push(("delete-last-failed", render(&st(), width, Some(last))));
        }
        // 删除后的「上次：」行：三种形态，名字单独中间截断（R6）
        for r in [
            delete::Report {
                deleted: vec!["HY2".to_string()],
                active: Some("hysteria2-1778329470".to_string()),
                switched: false,
                stopped: false,
                remaining: 8,
            },
            delete::Report {
                deleted: vec!["HY2".to_string(), "reality-Reality".to_string()],
                active: Some(victim.to_string()),
                switched: true,
                stopped: false,
                remaining: 7,
            },
            delete::Report {
                deleted: vec!["HY2".to_string()],
                active: None,
                switched: false,
                stopped: true,
                remaining: 0,
            },
        ] {
            let last = delete::summary(&r, width);
            out.push(("delete-last", render(&st(), width, Some(&last))));
        }
        // 确认时打编号改了替换目标，菜单补打的那一行
        for k in [0, 3, 8] {
            out.push(("delete-switch-to", render_switch_to(&del, k, width)));
        }
        // T7b：墓碑。菜单把名字那一句经 `delete::page` 折行打出来，再问 BURIED_ASK；
        // 命令行打 buried_skipped（终端自己折行，收进这张表只为守住字符归类与「折得开」）
        let buried: Vec<String> = prof.profiles.iter().map(|p| p.name.clone()).collect();
        for n in [1usize, 2, 3, 4, 9] {
            let names = &buried[..n];
            out.push(("buried-head", delete::page(&[buried_head(names)], width)));
            out.push((
                "buried-skipped",
                delete::page(&[buried_skipped(names)], width),
            ));
        }
        for line in [BURIED_ASK.to_string(), BURIED_RESTORED.to_string()] {
            out.push(("buried-line", delete::page(&[line], width)));
        }
        // T11：连接检查报告的整屏（真跑一遍 nettest::run，用逐行事件拼出来）与日志页
        out.extend(crate::nettest::sample::report_screens(width));
        out
    }

    /// 墓碑那几句（spec §5.7）：最多列 3 个名字、多的写成「等 N 个」；固定文案按容量口径
    /// 都在 59 列以内，名字那一句交给折行，问句短到 40 列也不折。
    #[test]
    fn the_buried_lines_list_at_most_three_names() {
        let n: Vec<String> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|x| x.to_string())
            .collect();
        assert_eq!(buried_list(&n[..1]), "a");
        assert_eq!(buried_list(&n[..3]), "a、b、c");
        assert_eq!(buried_list(&n[..4]), "a、b、c 等 4 个");
        assert_eq!(buried_list(&n), "a、b、c 等 5 个");
        assert_eq!(buried_head(&n[..2]), "这次导入里有 2 个你删过的节点：a、b");
        assert_eq!(
            buried_skipped(&n[..2]),
            "跳过 2 个删过的节点：a、b（要加回用 --with-deleted）"
        );
        // 名字先净化：方向键、颜色码里的 ESC 不能原样写回终端
        assert_eq!(
            buried_list(&["\u{1b}[A".to_string()]),
            "?[A",
            "外来名字先过 sanitize"
        );
        // 固定文案（不含名字）≤ 59 列
        for fixed in [
            BURIED_ASK.to_string(),
            BURIED_RESTORED.to_string(),
            buried_head(&[]),
            buried_skipped(&[]),
        ] {
            assert!(
                budget_width(&fixed) <= 59,
                "{}：{fixed}",
                budget_width(&fixed)
            );
        }
        // 问句连 `  ▸ `、冒号与 `[y/N]` 在 40 列里也放得下：不折、不截
        let asked = prompt_text(&format!("{BURIED_ASK} [y/N]"));
        assert!(
            budget_width(&asked) <= line_limit(40),
            "{}：{asked}",
            budget_width(&asked)
        );
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
        // 59：行宽上限 58。60 列刚好放满的行（例如 V3_SKIPPED 那一条「上次：」行）少一列时
        // 也得截得开、折得开，守住差一列的边界
        for w in [40, 50, 59, 60, 80, 100] {
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
        assert_eq!(parse_choice("6"), Some(Action::DeleteNode));
        assert_eq!(parse_choice("7"), Some(Action::Maintenance));
        assert_eq!(parse_choice("9"), Some(Action::SpeedTest));
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
        assert_eq!(parse_choice(" ９ "), Some(Action::SpeedTest));
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

/// T6：删除页的选择语法、确认输入与确认块（spec §5.2、§5.3、§0.2 R1）。
#[cfg(test)]
mod delete_tests {
    use super::*;
    use crate::testutil::{baiyi_like, hy2_direct_node, named};
    use pretty_assertions::assert_eq;

    fn tun(mut p: Profiles) -> Profiles {
        p.mode = Mode::Tun;
        p
    }

    /// 9 个节点时选中的下标；不是 `Picks` 就直接失败。
    fn picked(s: &str) -> Vec<usize> {
        match parse_selection(s, 9) {
            Ok(Selection::Picks(v)) => v,
            other => panic!("{s:?}: {other:?}"),
        }
    }

    #[test]
    fn selection_follows_the_spec_table() {
        use Selection::*;
        assert!(matches!(parse_selection("", 9), Ok(Back)));
        assert!(matches!(parse_selection("０", 9), Ok(Back)));
        assert!(matches!(parse_selection("1 1 3", 9), Ok(Picks(v)) if v == vec![0, 2]));
        assert!(matches!(parse_selection("1-3 2", 9), Ok(Picks(v)) if v == vec![0, 1, 2]));
        assert!(matches!(parse_selection("１，３", 9), Ok(Picks(v)) if v == vec![0, 2]));
        assert!(matches!(parse_selection("03", 9), Ok(Picks(v)) if v == vec![2]));
        assert!(matches!(parse_selection("1 a 3", 9), Err(SelError::Junk(x)) if x == "a"));
        assert!(matches!(
            parse_selection("0 3", 9),
            Err(SelError::ZeroMixed)
        ));
        assert!(matches!(parse_selection("5-3", 9), Err(SelError::Reversed(x)) if x == "5-3"));
        assert!(
            matches!(parse_selection("3 10-12 99", 9), Err(SelError::OutOfRange(v)) if v == vec!["10-12", "99"])
        );
        assert_eq!(
            parse_selection("99", 9).unwrap_err().message(9),
            "没有编号 99（可选 1-9）"
        );
    }

    #[test]
    fn disruptive_deletes_need_the_word_yes() {
        assert_eq!(parse_confirm("y", 9, false), ConfirmInput::Yes);
        assert_eq!(parse_confirm("y", 9, true), ConfirmInput::NeedWord);
        assert_eq!(parse_confirm("YES", 9, true), ConfirmInput::Yes);
        assert_eq!(parse_confirm("是", 9, true), ConfirmInput::Yes);
        assert_eq!(parse_confirm("4", 9, true), ConfirmInput::Pick(3));
        assert_eq!(parse_confirm("", 9, true), ConfirmInput::Cancel);
    }

    #[test]
    fn the_confirm_block_keeps_the_victims_next_to_the_prompt_and_fits_17_rows() {
        let mut prof = baiyi_like();
        prof.active = Some(prof.profiles[1].name.clone());
        let text = render_delete_confirm(&prof, &[1, 2], Some(3), 47, 17);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines.len() <= 15, "{text}");
        let prompt = lines.last().unwrap();
        assert!(
            prompt.contains("[yes/N]") && prompt.contains("等 2 个节点"),
            "{prompt}"
        );
        let victims = lines.iter().position(|l| l.contains("将删除")).unwrap();
        let alt = lines
            .iter()
            .position(|l| l.contains("可换") || l.contains("想换"))
            .unwrap();
        assert!(alt < victims, "可换节点在前，被删清单紧挨提问");
    }

    /// §5.2 表第 1 行：看不懂的片段优先级最高，只报第一个，原样回显（不折全角），超过 12 列尾截。
    #[test]
    fn junk_is_reported_first_and_verbatim() {
        for (input, frag) in [
            ("1 a 3", "a"),
            ("3-", "3-"),
            ("-3", "-3"),
            ("1.5", "1.5"),
            ("1--3", "1--3"),
            ("１ｂ", "１ｂ"),
            ("1 a b", "a"),
            ("0 5-3 99 x", "x"),
        ] {
            assert_eq!(
                parse_selection(input, 9),
                Err(SelError::Junk(frag.to_string())),
                "{input}"
            );
        }
        assert_eq!(
            SelError::Junk("a".into()).message(9),
            "看不懂「a」：只能写数字、逗号和 -"
        );
        assert_eq!(
            SelError::Junk("abcdefghijklmnopqrstuvwxyz".into()).message(9),
            "看不懂「abcdefghij…」：只能写数字、逗号和 -",
            "片段超过 12 列就尾截"
        );
        assert_eq!(
            SelError::Junk("a\u{1b}[2J".into()).message(9),
            "看不懂「a?[2J」：只能写数字、逗号和 -",
            "用户打的字也先净化再回显"
        );
    }

    /// spec §12.1（T6）与 §5.2 表第 1–4 行的优先级：看不懂 > 0 混写 > 范围写反 > 越界；
    /// 越界的片段一起列出、按输入顺序去重。
    #[test]
    fn selection_errors_by_priority() {
        use SelError::*;
        let err = |s: &str| parse_selection(s, 9).unwrap_err();
        assert_eq!(err("1 a 5-3"), Junk("a".into()), "看不懂排在最前");
        assert_eq!(err("3-"), Junk("3-".into()));
        assert_eq!(err("1--3"), Junk("1--3".into()));
        assert_eq!(err("0 3"), ZeroMixed);
        assert_eq!(err("3,0"), ZeroMixed);
        assert_eq!(err("０ 5-3 99"), ZeroMixed, "0 混写排在写反、越界前面");
        assert_eq!(err("5-3 99"), Reversed("5-3".into()), "写反排在越界前面");
        assert_eq!(
            err("２ ７～４ ５－３"),
            Reversed("7-4".into()),
            "只报第一个，写法规整成半角"
        );
        assert_eq!(
            err("3 10-12 99"),
            OutOfRange(vec!["10-12".into(), "99".into()])
        );
        assert_eq!(
            err("0-2"),
            OutOfRange(vec!["0-2".into()]),
            "范围里带 0 算越界"
        );
        assert_eq!(
            err("99999999999999999999"),
            OutOfRange(vec!["99999999999999999999".into()]),
            "解析溢出算越界"
        );
        assert_eq!(
            err("12-10"),
            OutOfRange(vec!["12-10".into()]),
            "两头都没有这个编号：先说没有，不教人改成 10-12"
        );
        assert_eq!(
            err("99 099 10"),
            OutOfRange(vec!["99".into(), "10".into()]),
            "去重，前导零折掉"
        );

        assert_eq!(ZeroMixed.message(9), "0 是返回，不能和编号写在一起");
        assert_eq!(
            Reversed("5-3".into()).message(9),
            "范围写反了：5-3，要写成 3-5"
        );
        assert_eq!(
            OutOfRange(vec!["10-12".into(), "99".into()]).message(9),
            "没有编号 10-12、99（可选 1-9）"
        );
        assert_eq!(
            OutOfRange(vec!["2".into()]).message(1),
            "没有编号 2（可选 1）"
        );
    }

    /// spec §12.1（T6）：分隔符（空白含全角空格、`,`、`，`、`、`）与连接号（`-`、`－`、`~`、`～`）
    /// 都认，全角数字折半角，前导零可以。
    #[test]
    fn selection_accepts_all_separators() {
        assert_eq!(picked("1 3 5"), vec![0, 2, 4]);
        assert_eq!(picked("2-4"), vec![1, 2, 3]);
        assert_eq!(picked("１，３"), vec![0, 2]);
        assert_eq!(picked("2-3，5"), vec![1, 2, 4]);
        assert_eq!(picked("03"), vec![2]);
        assert_eq!(picked("1、2"), vec![0, 1]);
        assert_eq!(picked("5、1，3,7\t9\u{3000}2"), vec![0, 1, 2, 4, 6, 8]);
        for r in ["2-4", "2－4", "2~4", "2～4", "２－４", "02-004"] {
            assert_eq!(picked(r), vec![1, 2, 3], "{r}");
        }
    }

    /// spec §12.1（T6）与 §5.2 表最后一行：重复与重叠静默去重，结果按列表顺序。
    #[test]
    fn selection_dedupes_and_sorts() {
        assert_eq!(picked("3 1 1 2-3"), vec![0, 1, 2]);
        assert_eq!(picked("１ １，３"), vec![0, 2]);
        assert_eq!(picked("4-4 3-5"), vec![2, 3, 4]);
        assert_eq!(picked("9 1-9"), (0..9).collect::<Vec<_>>());
    }

    /// spec §12.1（T6）：空行、全是分隔符、单独的 0（含全角、前导零）回主菜单。
    #[test]
    fn selection_back_inputs() {
        for back in ["", "  ", "0", "０", ",,", "00", "0 0", ",，、 "] {
            assert_eq!(parse_selection(back, 9), Ok(Selection::Back), "{back:?}");
        }
    }

    /// spec §11 测试表（T6）：所有 SelError 文案按容量口径 ≤ 59 列；越界的太多就写「等 N 个」。
    #[test]
    fn selection_messages_fit_59_columns() {
        let many = (10..=30)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        let inputs = [
            "1 a 3".to_string(),
            "0 3".into(),
            "5-3".into(),
            "99".into(),
            "3 10-12 99".into(),
            "abcdefghijklmnopqrstuvwxyz0123456789".into(),
            "示例专用名示例专用名示例专用名".into(),
            "9".repeat(40),
            format!("10-{} {}-3", "9".repeat(30), "1".repeat(30)),
            many.clone(),
            "\u{1b}[31m".into(),
        ];
        for s in &inputs {
            let m = parse_selection(s, 9).unwrap_err().message(9);
            assert!(budget_width(&m) <= 59, "{s}: {m} = {}", budget_width(&m));
            assert!(!m.contains('\u{1b}'), "{m:?}");
        }
        assert_eq!(
            parse_selection(&many, 9).unwrap_err().message(9),
            "没有编号 10、11、12、13、14、15、16 等 21 个（可选 1-9）"
        );
    }

    /// spec §11 测试表（T6）与 R1：会断网的删除要输入 yes 或「是」；0 与多个编号都算取消。
    #[test]
    fn confirm_input_parses() {
        use ConfirmInput::*;
        for y in ["y", "Y", "yes", "YES", "Yes", "ｙ", "ＹＥＳ", "是", " y "] {
            assert_eq!(parse_confirm(y, 9, false), Yes, "{y:?}");
        }
        for y in ["yes", "YES", "ｙｅｓ", "是", " yes\u{3000}"] {
            assert_eq!(parse_confirm(y, 9, true), Yes, "{y:?}");
        }
        for y in ["y", "Y", "ｙ"] {
            assert_eq!(parse_confirm(y, 9, true), NeedWord, "{y:?}");
        }
        assert_eq!(parse_confirm("8", 9, false), Pick(7));
        assert_eq!(parse_confirm("８", 9, true), Pick(7));
        assert_eq!(parse_confirm("04", 9, true), Pick(3));
        assert_eq!(
            parse_confirm("12", 9, true),
            Pick(11),
            "越界照样交回，由调用方报「没有编号 12」"
        );
        assert_eq!(
            parse_confirm(&"9".repeat(30), 9, true),
            Pick(9),
            "usize 放不下：按第一个越界的编号交回"
        );
        for no in [
            "",
            "n",
            "N",
            "no",
            "x",
            "0",
            "００",
            "4 5",
            "yes please",
            "是的",
            "ye",
        ] {
            assert_eq!(parse_confirm(no, 9, true), Cancel, "{no:?}");
            assert_eq!(parse_confirm(no, 9, false), Cancel, "{no:?}");
        }
    }

    /// spec §12.1（T6）：Passive 不含当前节点，照旧 [y/N]，写「删完还剩 M 个」；不提切换、不列可换节点。
    #[test]
    fn confirm_block_counts_and_remaining() {
        let prof = baiyi_like(); // 活动节点是 [2]
        let c = delete_confirm(&prof, &[2], None, 60, 24);
        assert!(!c.needs_word);
        assert_eq!(c.question, "确认删除 reality-Reality？[y/N]");
        for want in [
            "\n  将删除 1 个节点：\n     [3]   reality-Reality\n",
            "  删完还剩 8 个，当前节点不变，不会断网\n",
            "  以后导入时会先跳过它，再问你要不要加回\n",
        ] {
            assert!(c.body.contains(want), "{want}\n{}", c.body);
        }
        for no in ["切到", "可换", "想换", "断网几秒"] {
            assert!(!c.body.contains(no), "{no}\n{}", c.body);
        }
        assert_eq!(
            render_delete_confirm(&prof, &[2], None, 60, 24),
            format!("{}  ▸ {}：\n", c.body, c.question),
            "整块 = 正文 + 屏幕上的那一行提问"
        );
        let two = delete_confirm(&prof, &[2, 3], None, 60, 24);
        assert_eq!(two.question, "确认删除 reality-Reality 等 2 个节点？[y/N]");
        assert!(two.body.contains("  将删除 2 个节点：\n"), "{}", two.body);
        assert!(
            two.body
                .contains("  删完还剩 7 个，当前节点不变，不会断网\n"),
            "{}",
            two.body
        );
        // 本来就没有活动节点：不说「当前节点不变」
        let mut none = baiyi_like();
        none.active = None;
        let c = delete_confirm(&none, &[2, 3], None, 60, 24);
        assert!(c.body.contains("  删完还剩 7 个，不会断网\n"), "{}", c.body);
        assert!(c.body.contains("跳过它们"), "{}", c.body);
    }

    /// spec §12.1（T6）与 R1 的顺序：替换目标与可换节点在前（逐行列出，编号与列表一致），
    /// 被删清单紧挨提问；要输入 yes。
    #[test]
    fn switch_form_lists_alternatives() {
        let prof = tun(baiyi_like());
        let c = delete_confirm(&prof, &[1, 2], Some(3), 60, 40); // 行数够：逐行列出
        assert!(c.needs_word);
        assert_eq!(
            c.question,
            "确认删除 hysteria2-1778329470 等 2 个节点？[yes/N]"
        );
        let lines: Vec<&str> = c.body.lines().collect();
        let at = |needle: &str| {
            lines
                .iter()
                .position(|l| l.contains(needle))
                .unwrap_or_else(|| panic!("{needle}\n{}", c.body))
        };
        let order = [
            at("删完切到 [4] rick-node.example-a.net-reality-direct"),
            at("想换就输入下面的编号："),
            at("[1] HY2  示例专用名-HY2住宅"),
            at("[9] tizi.example.test-hy2-resi"),
            at("将删除 2 个节点，含当前节点："),
            at("[2] ★ hysteria2-1778329470"),
            at("[3]   reality-Reality"),
            at("TUN 模式：切换时会断网几秒"),
            at("以后导入时会先跳过它们，再问你要不要加回"),
        ];
        assert!(
            order.windows(2).all(|w| w[0] < w[1]),
            "{order:?}\n{}",
            c.body
        );
        // 可换的就是剩下的 7 个，编号保留原来的；被删的不在里面
        let nums: Vec<&str> = lines[order[1] + 1..order[4]]
            .iter()
            .map(|l| l.split_whitespace().next().unwrap())
            .collect();
        assert_eq!(
            nums,
            ["[1]", "[4]", "[5]", "[6]", "[7]", "[8]", "[9]"],
            "{}",
            c.body
        );
    }

    /// Empty：删光也会断网，要输入 yes；TUN 下说清撤掉后本机直连。
    #[test]
    fn deleting_everything_says_the_proxy_stops_and_needs_yes() {
        let all: Vec<usize> = (0..9).collect();
        let socks = delete_confirm(&baiyi_like(), &all, None, 60, 24);
        assert!(socks.needs_word);
        assert_eq!(socks.question, "确认删除 HY2 等 9 个节点？[yes/N]");
        assert!(
            socks.body.starts_with("\n  将删除全部 9 个节点：\n"),
            "{}",
            socks.body
        );
        assert!(
            socks.body.contains("  删完就没有节点了，代理会停止\n"),
            "{}",
            socks.body
        );
        for no in ["TUN", "切到", "可换", "想换", "还剩"] {
            assert!(!socks.body.contains(no), "{no}\n{}", socks.body);
        }
        let t = delete_confirm(&tun(baiyi_like()), &all, None, 60, 24);
        assert!(
            t.body
                .contains("  删完就没有节点了，代理会停止\n  TUN 撤掉后本机直连，国外网站打不开\n"),
            "{}",
            t.body
        );
    }

    /// R1：确认块超过 rows − 2 行时，可换节点压成一行，编号照旧；只剩替换目标一个时不列。
    #[test]
    fn alternatives_collapse_into_one_line_when_rows_run_out() {
        let prof = tun(baiyi_like());
        let tall = render_delete_confirm(&prof, &[1, 2], Some(3), 60, 40);
        let short = render_delete_confirm(&prof, &[1, 2], Some(3), 60, 17);
        assert!(tall.contains("  想换就输入下面的编号：\n"), "{tall}");
        assert!(tall.lines().count() > 15, "{tall}");
        assert!(!short.contains("想换"), "{short}");
        assert!(
            short.contains(
                "  删完切到 [4] rick-node.example-a.net-reality-direct\n  可换：1 4 5 6 7 8 9（编号同上面列表）\n  将删除 2 个节点，含当前节点：\n"
            ),
            "{short}"
        );
        assert!(short.lines().count() <= 15, "{short}");
        let mut two = baiyi_like();
        two.profiles.truncate(2);
        let c = delete_confirm(&two, &[1], Some(0), 60, 40);
        assert!(c.body.contains("  删完切到 [1] HY2\n"), "{}", c.body);
        assert!(
            !c.body.contains("可换") && !c.body.contains("想换"),
            "{}",
            c.body
        );
    }

    /// R1：提问带第一个名字；窄到连截断的名字都放不下时退回「这 N 个节点」。
    #[test]
    fn the_question_names_the_first_victim() {
        let prof = baiyi_like();
        let q = |picks: &[usize], to: Option<usize>, w: usize| {
            delete_confirm(&prof, picks, to, w, 24).question
        };
        assert_eq!(q(&[2], None, 80), "确认删除 reality-Reality？[y/N]");
        assert_eq!(
            q(&[5, 2], None, 80),
            "确认删除 reality-Reality 等 2 个节点？[y/N]",
            "第一个 = 列表顺序上的第一个"
        );
        assert_eq!(
            q(&[1], Some(0), 80),
            "确认删除 hysteria2-1778329470？[yes/N]"
        );
        assert_eq!(
            q(&[1, 2], Some(3), 47),
            "确认删除 hys…29470 等 2 个节点？[yes/N]"
        );
        assert_eq!(
            q(&[1, 2], Some(3), 40),
            "确认删除这 2 个节点？[yes/N]",
            "名字只剩 3 列：不带"
        );
        // R1 的窄屏例外：名字可用不到 8 列就不带；删 2 个、要输 yes 时门槛落在 44 / 45 列之间
        assert_eq!(q(&[1, 2], Some(3), 44), "确认删除这 2 个节点？[yes/N]");
        assert_eq!(
            q(&[1, 2], Some(3), 45),
            "确认删除 hy…9470 等 2 个节点？[yes/N]"
        );
        assert_eq!(
            q(&[0, 2], None, 40),
            "确认删除 HY2 等 2 个节点？[y/N]",
            "短名字照样带上"
        );
        for w in [40, 47, 50, 60, 80] {
            for picks in [&[1][..], &[1, 2], &[0, 2], &[3, 4, 5], &[5]] {
                let line = prompt_text(&q(picks, Some(8), w));
                assert!(budget_width(&line) <= line_limit(w), "@{w}: {line}");
            }
        }
    }

    /// spec §12.1（T6）与 §2.3 撞名保护：两个被删节点截断后一样，就显示全名（按行宽折行），
    /// 宁可难看也要分得清。
    #[test]
    fn colliding_truncations_fall_back_to_full_names() {
        let a = "rick-node.example-a.net-tokyo-hy2-residential-direct";
        let b = "rick-node.example-a.net-osaka-hy2-residential-direct";
        let mut prof = baiyi_like();
        prof.profiles.push(named(a, hy2_direct_node()));
        prof.profiles.push(named(b, hy2_direct_node()));
        // 前提：40 列窄版式、两位数编号时名字可用 30 列，中间截断后两者一样
        assert_eq!(truncate_middle(a, 30), truncate_middle(b, 30));
        let text = render_delete_confirm(&prof, &[9, 10], None, 40, 24);
        let flat: String = text.lines().map(str::trim_start).collect();
        assert!(flat.contains(a) && flat.contains(b), "{text}");
        for l in text.lines() {
            assert!(budget_width(l) <= line_limit(40), "{l:?}\n{text}");
        }
        // 不撞名时照旧中间截断
        let plain = render_delete_confirm(&prof, &[3, 9], None, 40, 24);
        assert!(plain.contains("rick-node.e…"), "{plain}");
        assert!(!plain.contains(a), "{plain}");
    }

    /// §5.3 第 2 条：确认块里 label 与服务器:端口完整不截，放不下就拆行、再放不下就折行。
    #[test]
    fn labels_and_hosts_in_the_block_are_wrapped_not_truncated() {
        let mut prof = baiyi_like();
        let label = "示例专用名-一个特别特别长的备注-给家里人看的-不要删";
        prof.profiles[2].node.label = label.into();
        for w in [40, 60] {
            let text = render_delete_confirm(&prof, &[2], None, w, 24);
            let flat: String = text.lines().map(str::trim_start).collect();
            assert!(
                flat.contains(label) && flat.contains("tizi.example.test:10001"),
                "@{w}\n{text}"
            );
            assert!(!text.contains('…'), "@{w}：不截断\n{text}");
            for l in text.lines() {
                assert!(budget_width(l) <= line_limit(w), "@{w}: {l:?}");
            }
        }
    }

    /// 提问里的 [yes/N] 与 parse_confirm 要不要 yes 永远一致：标签与行为对得上。
    #[test]
    fn needs_word_always_matches_the_prompt_tag() {
        let all: Vec<usize> = (0..9).collect();
        for active in [None, Some(1), Some(8)] {
            let mut prof = baiyi_like();
            prof.active = active.map(|i: usize| prof.profiles[i].name.clone());
            for picks in [
                vec![0],
                vec![1],
                vec![1, 2],
                vec![2, 3],
                vec![8],
                all.clone(),
            ] {
                let c = delete_confirm(&prof, &picks, Some(4), 60, 24);
                let disrupts = picks.len() == 9 || active.is_some_and(|a| picks.contains(&a));
                assert_eq!(c.needs_word, disrupts, "{active:?} {picks:?}");
                assert_eq!(c.question.ends_with("[yes/N]"), disrupts, "{c:?}");
                assert_eq!(c.question.ends_with("[y/N]"), !disrupts, "{c:?}");
                let screen = render_delete_confirm(&prof, &picks, Some(4), 60, 24);
                assert!(screen.ends_with(&format!("{}：\n", c.question)), "{screen}");
            }
        }
    }

    /// 确认时改了替换目标，菜单再打一行「删完切到」：与确认块里那一行同一个样子。
    #[test]
    fn the_switch_to_line_middle_truncates_the_replacement() {
        let prof = baiyi_like();
        assert_eq!(
            render_switch_to(&prof, 5, 60),
            "  删完切到 [6] rick-node.example-a.net-hy2-direct\n"
        );
        let at40 = render_switch_to(&prof, 5, 40);
        assert!(
            at40.starts_with("  删完切到 [6] rick-")
                && at40.ends_with("hy2-direct\n")
                && at40.contains('…'),
            "{at40}"
        );
        assert!(budget_width(at40.trim_end()) <= line_limit(40), "{at40}");
        assert_eq!(render_switch_to(&prof, 99, 60), "", "编号不存在就不打");
        let block = delete_confirm(&prof, &[1], Some(5), 40, 24).body;
        assert!(block.contains(&at40), "{block}");
    }

    /// [6] 删除页：标题带数量与图例，过渡提示（调用方只在第一次进时给）在标题下，列表下面是写法。
    #[test]
    fn the_delete_picker_shows_the_title_hint_list_and_syntax() {
        let prof = baiyi_like();
        let hint = MOVED_HINT_DELETE;
        assert_eq!(
            render_delete_picker(&prof, 60, None),
            format!(
                "\n  删除节点（共 9 个，★ 为当前）\n{}  可多选：1 3 5 或 2-4，空行返回\n",
                render_nodes(&prof, true, 60)
            ),
            "列表与 [1] 同一个 render_nodes"
        );
        let hinted = render_delete_picker(&prof, 60, Some(hint));
        assert!(
            hinted.starts_with(&format!(
                "\n  删除节点（共 9 个，★ 为当前）\n  {hint}\n     [1]   HY2\n"
            )),
            "{hinted}"
        );
        // 40 列也是一行（常量 ≤ 37 列 + 缩进 2，spec §0.2 R1），「[7]）」不会单占一行
        let narrow = render_delete_picker(&prof, 40, Some(hint));
        assert!(
            narrow.starts_with(&format!(
                "\n  删除节点（共 9 个，★ 为当前）\n  {hint}\n  [1]   HY2\n"
            )),
            "{narrow}"
        );
        // 更长的提示照旧折行：断在最后一个放得下的折点，这里是「[7]」前面的空格（后面是 ASCII）
        let long =
            render_delete_picker(&prof, 40, Some("（原来的高级设置已取消，检查更新在 [7]）"));
        assert!(
            long.starts_with(
                "\n  删除节点（共 9 个，★ 为当前）\n  （原来的高级设置已取消，检查更新在\n  [7]）\n  [1]   HY2\n"
            ),
            "{long}"
        );
    }

    /// 外来的名字、label、主机先净化再上屏（spec §2.3，D18）。
    #[test]
    fn external_text_in_the_block_is_sanitized() {
        let mut prof = tun(baiyi_like());
        prof.profiles[1].name = "evil\u{1b}[2Jname".into();
        prof.active = Some(prof.profiles[1].name.clone());
        prof.profiles[1].node.label = "lab\u{202e}el".into();
        prof.profiles[3].node.label = "x\u{7}y".into();
        for w in [40, 60] {
            let text = render_delete_confirm(&prof, &[1], Some(3), w, 40);
            assert!(
                !text
                    .chars()
                    .any(|c| matches!(c, '\u{1b}' | '\u{7}' | '\u{202e}')),
                "{text:?}"
            );
            assert!(
                text.contains("evil?[2Jname") && text.contains("lab?el"),
                "{text}"
            );
        }
    }

    /// 折行工具：断在最后一个放得下的折点（中文标点之后，或后面是 ASCII 的空格处），
    /// 没有折点就按字硬折，拼回去一字不差。
    #[test]
    fn wrap_breaks_at_the_last_fitting_point_and_hard_breaks_long_words() {
        assert_eq!(
            wrap("删完还剩 8 个，当前节点不变，不会断网", 2, 40),
            "  删完还剩 8 个，当前节点不变，不会断网\n"
        );
        assert_eq!(
            wrap("删完还剩 12 个，当前节点不变，不会断网", 2, 40),
            "  删完还剩 12 个，当前节点不变，\n  不会断网\n"
        );
        assert_eq!(
            wrap("（原来的高级设置已取消，检查更新在 [7]）", 2, 40),
            "  （原来的高级设置已取消，检查更新在\n  [7]）\n"
        );
        let long = "rick-node.example-a.net-reality-direct-and-then-some";
        let out = wrap(long, 8, 40);
        assert_eq!(out.lines().map(str::trim_start).collect::<String>(), long);
        assert!(
            out.lines().all(|l| budget_width(l) <= line_limit(40)),
            "{out}"
        );
        assert_eq!(wrap("", 2, 40), "");
    }

    /// 删到当前节点的两张定稿屏（spec §3c-S，按 R1 重排）：60 列行数够时逐行列出可换节点；
    /// 40 列、17 行（手机横屏）压成一行，提问里放不下长名字就不带。
    #[test]
    fn the_switch_block_is_pinned_at_60_and_40_columns() {
        let prof = tun(baiyi_like());
        assert_eq!(
            render_delete_confirm(&prof, &[1, 2], Some(3), 60, 24),
            "\n  删完切到 [4] rick-node.example-a.net-reality-direct\n  想换就输入下面的编号：\n     [1] HY2  示例专用名-HY2住宅\n     [4] rick-node.example-a.net-reality-direct  Reality…\n     [5] rick-node.example-a.net-reality-resi  Reality住宅\n     [6] rick-node.example-a.net-hy2-direct  HY2直连\n     [7] rick-node.example-a.net-hy2-resi  HY2住宅\n     [8] tizi.example.test-reality-resi  Reality住宅\n     [9] tizi.example.test-hy2-resi  HY2住宅\n  将删除 2 个节点，含当前节点：\n     [2] ★ hysteria2-1778329470\n           示例专用名  tizi.example.test:10000\n     [3]   reality-Reality\n           示例名-reality-Reality直连\n           tizi.example.test:10001\n  TUN 模式：切换时会断网几秒\n  以后导入时会先跳过它们，再问你要不要加回\n  ▸ 确认删除 hysteria2-1778329470 等 2 个节点？[yes/N]：\n"
        );
        assert_eq!(
            render_delete_confirm(&prof, &[1, 2], Some(3), 40, 17),
            "\n  删完切到 [4] rick-nod…reality-direct\n  可换：1 4 5 6 7 8 9（编号同上面列表）\n  将删除 2 个节点，含当前节点：\n  [2] ★ hysteria2-1778329470\n        示例专用名\n        tizi.example.test:10000\n  [3]   reality-Reality\n        示例名-reality-Reality直连\n        tizi.example.test:10001\n  TUN 模式：切换时会断网几秒\n  以后导入时会先跳过它们，\n  再问你要不要加回\n  ▸ 确认删除这 2 个节点？[yes/N]：\n"
        );
    }

    /// R1：删到当前节点、剩下的节点又都在被删节点所在的服务器上（host 不分大小写）时，确认块多一行
    /// 说明，紧跟「删完切到」、在可换节点前面。剩下的只在 1 台服务器上写「都在同一台服务器上」，
    /// 分在多台上写「都在被删节点所在的服务器上」；剩下的里有别的服务器，或不是删到当前节点，都不加。
    #[test]
    fn a_note_appears_when_every_remaining_node_is_on_a_deleted_server() {
        const NOTE: &str = "  剩下的节点都在同一台服务器上\n";
        // 只留 tizi 上的 5 个节点，删掉 tizi 上的活动节点 [2]：剩下 4 个也都在 tizi 上
        let mut same = baiyi_like();
        same.profiles.retain(|p| p.node.host == "tizi.example.test");
        assert_eq!(same.profiles.len(), 5);
        assert_eq!(same.active.as_deref(), Some(same.profiles[1].name.as_str()));
        same.profiles[3].node.host = "TIZI.Example.Test".into();
        for (w, rows) in [(40, 17), (40, 40), (60, 17), (60, 40)] {
            let c = delete_confirm(&same, &[1], Some(0), w, rows);
            assert!(c.body.contains(NOTE), "@{w}x{rows}\n{}", c.body);
            let lines: Vec<&str> = c.body.lines().collect();
            let at = |s: &str| lines.iter().position(|l| l.contains(s)).unwrap();
            assert_eq!(
                at("剩下的节点都在同一台服务器上"),
                at("删完切到 [1] HY2") + 1,
                "紧跟「删完切到」\n{}",
                c.body
            );
            assert!(
                at("剩下的节点都在同一台服务器上") < at("将删除"),
                "{}",
                c.body
            );
        }
        // 连同 rick-node 上的 4 个一起删：剩下的只在 tizi 一台上，还是「同一台」
        let prof = baiyi_like();
        let mixed = delete_confirm(&prof, &[1, 3, 4, 5, 6], Some(0), 60, 40);
        assert!(mixed.body.contains(NOTE), "{}", mixed.body);
        // 删 [2]（tizi）与 [4]（rick-node）：剩下的分在两台上、又都是被删节点所在的服务器，写长文案
        for w in [40, 60] {
            let split = delete_confirm(&prof, &[1, 3], Some(0), w, 40);
            assert!(
                split
                    .body
                    .contains("  删完切到 [1] HY2\n  剩下的节点都在被删节点所在的服务器上\n"),
                "@{w}\n{}",
                split.body
            );
            assert!(!split.body.contains("同一台"), "@{w}\n{}", split.body);
        }
        // 反例：剩下的里还有别的服务器
        for picks in [&[1][..], &[1, 2], &[0, 1, 2, 7, 8]] {
            let c = delete_confirm(&prof, picks, Some(3), 60, 40);
            assert!(!c.body.contains("剩下的节点都在"), "{picks:?}\n{}", c.body);
        }
        // 不是删到当前节点：Passive、Empty 都不加
        let passive = delete_confirm(&same, &[0], None, 60, 40);
        assert!(!passive.body.contains("剩下的节点都在"), "{}", passive.body);
        let all: Vec<usize> = (0..same.profiles.len()).collect();
        let empty = delete_confirm(&same, &all, None, 60, 40);
        assert!(!empty.body.contains("剩下的节点都在"), "{}", empty.body);
    }

    /// 审查意见：空格算不算折点要等下一个字到了再定，下一个字不是 ASCII 就不算，所以「12 个」不会
    /// 拆成「12」「个」（要断就断在数字前面）。选择报错文案在 40 列折行后，没有一行以「个」开头。
    #[test]
    fn wrap_keeps_a_number_with_the_word_after_it() {
        let got = wrap_pieces("将删除 12 个节点", 11, 11);
        assert_ne!(got, ["将删除 12", "个节点"]);
        assert_eq!(got, ["将删除", "12 个节点"]);
        for start in 10..=40usize {
            for end in start..=start + 40 {
                let v: Vec<String> = (start..=end).map(|n| n.to_string()).collect();
                let w = wrap(&SelError::OutOfRange(v).message(9), 2, 40);
                for l in w.lines().skip(1) {
                    assert!(!l.trim_start().starts_with('个'), "{start}..={end}\n{w}");
                }
            }
        }
    }

    /// 审查意见：最新的折点放不下（溢出的正是那个「，」）就退回前一个折点，不硬折；
    /// 实在要硬折，也不让「，」「）」这类标点落到行首（把前一个字一起挪下去）。
    #[test]
    fn wrap_never_starts_a_line_with_punctuation() {
        assert_eq!(
            wrap_pieces("甲乙丙丁，戊己庚辛壬，癸", 21, 21),
            ["甲乙丙丁，", "戊己庚辛壬，癸"]
        );
        for text in [
            "甲乙丙丁，戊己庚辛壬，癸",
            "甲乙丙丁戊己庚辛壬，癸",
            "以后导入时会先跳过它们，再问你要不要加回",
            "家里人用的，备注家里人用的备注，别删",
            "（原来的高级设置已取消，检查更新在 [7]）",
        ] {
            for room in 4..=40 {
                let lines = wrap_pieces(text, room, room);
                for l in &lines {
                    assert!(budget_width(l) <= room, "{text} @{room}: {lines:?}");
                    assert!(
                        !l.starts_with(|c: char| NO_LINE_START.contains(c)),
                        "{text} @{room}: {lines:?}"
                    );
                }
            }
        }
    }

    /// 审查意见：命令行不认编号（换目标用 --switch-to），确认块不列可换节点、也不压成「可换：…」，
    /// 其余与菜单版逐字相同：「删完切到」与同服务器那一行都保留，提问与 needs_word 也一样。
    #[test]
    fn the_cli_block_has_no_alternatives_but_keeps_the_switch_target() {
        let prof = tun(baiyi_like());
        let menu = delete_confirm(&prof, &[1, 2], Some(3), 60, 100);
        let cli = delete_confirm_cli(&prof, &[1, 2], Some(3), 60);
        assert_eq!(
            (cli.needs_word, cli.question.as_str()),
            (menu.needs_word, menu.question.as_str())
        );
        for no in ["想换", "可换"] {
            assert!(!cli.body.contains(no), "{no}\n{}", cli.body);
        }
        assert!(
            cli.body
                .contains("  删完切到 [4] rick-node.example-a.net-reality-direct\n"),
            "{}",
            cli.body
        );
        // 菜单版去掉「想换就输入下面的编号：」与下面 7 行可换节点，就是命令行版
        let mut want: Vec<&str> = menu.body.lines().collect();
        let k = want
            .iter()
            .position(|l| l.contains("想换就输入下面的编号："))
            .unwrap();
        want.drain(k..k + 1 + 7);
        assert_eq!(cli.body.lines().collect::<Vec<_>>(), want);
        // 窄屏也一样；同服务器那一行照样有
        let mut same = baiyi_like();
        same.profiles.retain(|p| p.node.host == "tizi.example.test");
        for w in [40, 60] {
            let c = delete_confirm_cli(&same, &[1], Some(0), w);
            assert!(
                c.body.contains(
                    "  删完切到 [1] HY2\n  剩下的节点都在同一台服务器上\n  将删除 1 个节点，含当前节点：\n"
                ),
                "@{w}\n{}",
                c.body
            );
        }
        // Passive、Empty 本来就没有可换节点：两版逐字相同
        let all: Vec<usize> = (0..9).collect();
        for picks in [vec![2], vec![0, 2], all] {
            assert_eq!(
                delete_confirm_cli(&prof, &picks, None, 60),
                delete_confirm(&prof, &picks, None, 60, 24)
            );
        }
    }

    /// 删到当前节点却没给替换目标，是调用方的错：debug 构建里直接 panic。
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "替换目标")]
    fn switch_without_a_replacement_is_a_caller_bug() {
        delete_confirm(&baiyi_like(), &[1], None, 60, 24);
    }

    /// 替换目标本身在删除之列，同样是调用方的错；命令行版也查。
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "替换目标")]
    fn switching_to_a_deleted_node_is_a_caller_bug() {
        delete_confirm_cli(&baiyi_like(), &[1, 2], Some(2), 60);
    }
}

#[cfg(test)]
mod nextstep_tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn next_step_keys_are_fixed() {
        assert_eq!(parse_next_step("1"), Some(NextStep::Recheck));
        assert_eq!(parse_next_step("2"), Some(NextStep::SpeedTest));
        assert_eq!(parse_next_step("3"), Some(NextStep::Journal));
        assert_eq!(
            parse_next_step("３"),
            Some(NextStep::Journal),
            "全角数字折半角"
        );
        for back in ["0", "", "  ", "０"] {
            assert_eq!(parse_next_step(back), Some(NextStep::Back), "{back:?}");
        }
        for junk in ["4", "9", "x", "1 2", "\u{1b}[A"] {
            assert_eq!(parse_next_step(junk), None, "{junk:?}");
        }
    }

    #[test]
    fn next_step_block_has_four_fixed_rows_and_the_typo_line_fits() {
        assert_eq!(
            render_next_step(),
            "  下一步：\n     [1] 再查一次\n     [2] 换个节点\n     [3] 看最近 50 行日志\n     [0] 返回菜单\n"
        );
        assert_eq!(
            invalid_next_step("x", 60),
            "无效选项：x（请输入 0-3 的数字）"
        );
        assert_eq!(
            invalid_next_step("\u{1b}[A", 60),
            "无效选项：?[A（请输入 0-3 的数字）",
            "方向键的 ESC 不能回显"
        );
        for w in [40, 50, 60, 80, 100] {
            // 2 = 菜单里 `say` 加的缩进
            let l = format!("  {}", invalid_next_step(&"9".repeat(200), w));
            assert!(budget_width(&l) <= line_limit(w), "@{w}: {l}");
        }
    }
}
