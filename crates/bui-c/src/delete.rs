//! 删除节点的纯函数：期望态（[`plan`]）、并发比对用的快照（[`snapshot`]）、默认替换目标
//! （[`default_to`]）、结果摘要（[`summary`]）与失败停顿页的文案（[`page`] 与本模块的常量）。
//!
//! 会改机器的编排在 `cli::delete_nodes`（拿锁、预检、先数据面后落盘、失败回滚），
//! 屏幕上的确认块在 [`crate::menu::delete_confirm`]。菜单 `[6]`、`bui-c delete`
//! 与 `[9]` 测速结果页三个入口共用这一份实现（spec §5.1）。

use crate::menu;
use crate::profiles::{Mode, Profiles};
use serde::Serialize;

/// 一次删除的形态（spec §5.5 的三行表）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanKind {
    /// 活动节点不在删除之列（或本来就没有活动节点），且还剩节点：不动数据面。
    Passive,
    /// 删到活动节点、还剩节点：切到 `to`。
    Switch { to: String },
    /// 删完一个不剩：拆掉数据面。
    Empty,
}

/// 删除的期望态：只算，不动机器。
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    /// 要删的节点名，按给进来的顺序去重。
    pub targets: Vec<String>,
    /// 删完之后的 `profiles.json`（Switch 形态里 `active` 已经是替换节点）。
    pub next: Profiles,
    pub kind: PlanKind,
}

/// 算不出期望态：一个节点都不删（spec §5.2「破坏性操作不能部分执行」）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// 这些名字在 `profiles.json` 里找不到。
    NotFound(Vec<String>),
    /// `--switch-to` 指的节点不存在。
    BadSwitchTo(String),
    /// `--switch-to` 指的节点也在删除之列。
    SwitchToIsTarget(String),
}

impl std::fmt::Display for PlanError {
    /// 节点名一律先过 [`menu::sanitize`]（spec §5.10 末条）。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let clean = |n: &String| menu::sanitize(n).into_owned();
        match self {
            PlanError::NotFound(v) => write!(
                f,
                "没有叫 {} 的节点，用 `bui-c list` 看名字；什么都没删",
                v.iter().map(clean).collect::<Vec<_>>().join("、")
            ),
            PlanError::BadSwitchTo(n) => write!(
                f,
                "--switch-to {} 不存在，用 `bui-c list` 看名字；什么都没删",
                clean(n)
            ),
            PlanError::SwitchToIsTarget(n) => {
                write!(f, "--switch-to {} 也在要删的节点里；什么都没删", clean(n))
            }
        }
    }
}

impl From<PlanError> for crate::Error {
    /// 全是「执行失败」，退出码 1（spec §0.2 R15）。
    fn from(e: PlanError) -> Self {
        crate::Error::msg(e.to_string())
    }
}

/// 按名字算期望态（纯函数）。`names` 重复的去重；有一个找不到就整条作废
/// （[`PlanError::NotFound`]）。`switch_to` 只要给了就校验，哪种形态都一样。
///
/// 形态：删完一个不剩 → [`PlanKind::Empty`]（`active = None`，模式、端口、panel、
/// auto_update 都留着）；删到活动节点、还剩节点 → [`PlanKind::Switch`]（`to` 用
/// `switch_to`，没给就按 [`default_to`] 挑）；其余 → [`PlanKind::Passive`]。
pub fn plan(
    prof: &Profiles,
    names: &[String],
    switch_to: Option<&str>,
) -> std::result::Result<Plan, PlanError> {
    let mut targets: Vec<String> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    for n in names {
        let known = prof.profiles.iter().any(|p| &p.name == n);
        let bucket = if known { &mut targets } else { &mut missing };
        if !bucket.contains(n) {
            bucket.push(n.clone());
        }
    }
    if !missing.is_empty() {
        return Err(PlanError::NotFound(missing));
    }
    if let Some(t) = switch_to {
        if !prof.profiles.iter().any(|p| p.name == t) {
            return Err(PlanError::BadSwitchTo(t.to_string()));
        }
        if targets.iter().any(|n| n == t) {
            return Err(PlanError::SwitchToIsTarget(t.to_string()));
        }
    }
    let mut next = prof.clone();
    next.profiles.retain(|p| !targets.contains(&p.name));
    let kind = if next.profiles.is_empty() && !targets.is_empty() {
        next.active = None;
        PlanKind::Empty
    } else if prof
        .active_profile()
        .is_some_and(|a| targets.contains(&a.name))
    {
        // 这一支里 next.profiles 非空（空了就是 Empty），所以 default_to 的 remaining 非空、
        // 它的 `.or_else(remaining.first())` 必然给得出一个下标。以前这里还垫了两级兜底，
        // 末级 `unwrap_or_default()` 会造出 `Switch { to: "" }` 与 `active = Some("")`，
        // 之后 preflight 报「没有激活的节点」，排障要绕一圈；不可达就让它明着炸。
        let to = switch_to.map(str::to_string).unwrap_or_else(|| {
            let i = default_to(prof, &targets, None).expect("剩下的节点非空时 default_to 必有值");
            prof.profiles[i].name.clone()
        });
        next.active = Some(to.clone());
        PlanKind::Switch { to }
    } else {
        PlanKind::Passive
    };
    Ok(Plan {
        targets,
        next,
        kind,
    })
}

/// 确认那一刻的节点列表，用来在锁里比对「有没有被别处改过」（spec §5.5 第 2 步、R11）。
/// 只比会影响这次删除的东西：全部节点名（按顺序）、活动节点、模式与两个本地端口；
/// label、凭据这类改动不算。
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    names: Vec<String>,
    active: Option<String>,
    mode: Mode,
    socks_port: u16,
    http_port: u16,
}

pub fn snapshot(prof: &Profiles) -> Snapshot {
    Snapshot {
        names: prof.profiles.iter().map(|p| p.name.clone()).collect(),
        active: prof.active.clone(),
        mode: prof.mode,
        socks_port: prof.socks_port,
        http_port: prof.http_port,
    }
}

/// 删到活动节点时默认切到哪个（spec §5.4）：`prof.profiles` 的下标。
///
/// 1. 有测速结果（`fastest`，从 `[9]` 进来）就用它——刚测过，知道它能用；
/// 2. 否则取剩下的节点里第一个 **host（转小写）不在被删节点的服务器之列**的：删的往往是
///    整台下线服务器上的节点，「剩下的第一个」很可能是同一台上的另一个死节点；
/// 3. 都在这些服务器上，才取剩下的第一个。
///
/// 一个都不剩（删光）时为 `None`。
pub fn default_to(prof: &Profiles, targets: &[String], fastest: Option<usize>) -> Option<usize> {
    let remaining: Vec<usize> = (0..prof.profiles.len())
        .filter(|&i| !targets.contains(&prof.profiles[i].name))
        .collect();
    if let Some(i) = fastest.filter(|i| remaining.contains(i)) {
        return Some(i);
    }
    let gone: std::collections::HashSet<String> = prof
        .profiles
        .iter()
        .filter(|p| targets.contains(&p.name))
        .map(|p| p.node.host.to_lowercase())
        .collect();
    remaining
        .iter()
        .find(|&&i| !gone.contains(&prof.profiles[i].node.host.to_lowercase()))
        .or_else(|| remaining.first())
        .copied()
}

/// 一次删除的结果。`bui-c delete --json` 直接序列化它（spec §5.10 的键就是这几个）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Report {
    pub deleted: Vec<String>,
    pub active: Option<String>,
    pub switched: bool,
    pub stopped: bool,
    pub remaining: usize,
}

/// 「上次：」行的短摘要（spec §0.2 R6）：`已删 N 个` / `已删 N 个，切到 {名字}` /
/// `已删光节点，按 [3] 导入`。名字按 R6 单独中间截断，与 `menu::fit_name_in_last`
/// （切换摘要）同一规则。
pub fn summary(r: &Report, width: usize) -> String {
    if r.stopped {
        return "已删光节点，按 [3] 导入".to_string();
    }
    let n = r.deleted.len();
    match r.active.as_deref().filter(|_| r.switched) {
        Some(name) => menu::fit_name_in_last(&format!("已删 {n} 个，切到 {name}"), name, width),
        None => format!("已删 {n} 个"),
    }
}

/// `bui-c delete` 成功时打的那一句（spec §5.4 命令行那一条）。命令行没有「上次：」行、宽度
/// 也不紧张，所以说全，不用 [`summary`] 的短式（R6 的短式只管菜单那一行），也不把菜单键
/// 漏进命令行。名字照 §5.10 末条过 [`menu::sanitize`]。
///
/// - Switch：`当前节点已删除，切到 X`（§5.4 逐字）
/// - Passive：`已删除 N 个节点，还剩 M 个`
/// - Empty：「已删除全部 N 个节点，代理已停止；用 bui-c import 重新导入」
pub fn cli_summary(r: &Report) -> String {
    let n = r.deleted.len();
    if r.stopped {
        // 不给 `bui-c import` 加反引号：整句要容得下三位数的节点数。不带反引号是
        // 57 列（三位数 59），带上就是 59（三位数 61），越过固定文案 ≤ 59 列那条线
        return format!("已删除全部 {n} 个节点，代理已停止；用 bui-c import 重新导入");
    }
    match r.active.as_deref().filter(|_| r.switched) {
        Some(to) => format!("当前节点已删除，切到 {}", menu::sanitize(to)),
        None => format!("已删除 {n} 个节点，还剩 {} 个", r.remaining),
    }
}

/// 锁里重读发现节点列表变了（spec §5.5 第 2 步）。
pub const SNAPSHOT_CHANGED: &str = "节点列表刚被别处改过，这次什么都没删，请重新选";
/// 同一件事进「上次：」行时的短式：40 列那一行只有 31 列可用，长句会被尾截、正好砍掉
/// 「请重新选」这半句可操作的话（spec §0.2 R6）。停顿页上仍打 [`SNAPSHOT_CHANGED`]。
pub const SNAPSHOT_CHANGED_SHORT: &str = "节点列表刚被别处改过，没有删除";
/// `profiles.json` 写不进去：Switch（数据面已经切过去，紧接着回滚）与 Passive（数据面
/// 一点没动）共用的第一行与短摘要。删光那一种另有说法，见 [`STOPPED_NOT_SAVED_HEAD`]。
pub const SAVE_FAILED: &str = "删除没做：写 profiles.json 失败";
/// 删光时拆数据面失败的短摘要（页上打的是 `删除没做：{engine 的原因}`，40 列放不下）。
///
/// 故意不说是哪一步失败：`Engine::teardown_main` 除了「停不下来」，在 is-active 复查通过之后
/// 还可能因为删单元文件、`daemon-reload`、删 `config.json`、清临时文件而失败。要在这一行里
/// 分辨就得拿 `engine` 的错误文本去匹配（像 `KERNEL_MISSING` 那样），而那是把两个模块的文案
/// 绑在一起的写法，不值得为一行摘要再加一处。页上紧接着就是真正的原因，所以这一行只要
/// **在所有分支下都为真**：「代理停不下来」在删单元文件失败时是假的，「拆数据面失败」不是。
pub const TEARDOWN_FAILED_SHORT: &str = "删除没做：拆数据面失败";
/// 删光时数据面拆完了、`profiles.json` 却写不进去：这是唯一一种「代理已经不在、节点条目
/// 还列着」的状态，所以不能沿用 [`STILL_THERE`]（「节点都还在」在这里是错的）。
pub const STOPPED_NOT_SAVED_HEAD: &str = "代理已经停了，但写 profiles.json 失败";
/// 承接 [`STOPPED_NOT_SAVED_HEAD`] 的下一步：T12c 的收敛条件「有活动节点但主单元文件不在
/// → apply」「profiles 为空但单元还在 → teardown」正好覆盖这一状态。
pub const STOPPED_NOT_SAVED: &str = "节点条目还在，下次进菜单或巡检会按节点列表收拾";
/// 同一件事进「上次：」行时的短式（40 列只有 31 列可用）。
pub const STOPPED_NOT_SAVED_SHORT: &str = "删除没做完：代理已停，节点还在";
/// 失败页的安抚行：数据面根本没动。
pub const STILL_THERE: &str = "节点都还在";
/// 失败页的安抚行：动过数据面，已经换回去了。
pub const ROLLED_BACK: &str = "已换回原来的配置，节点都还在";
/// 回滚写回了旧配置，但 TUN 还是没起来：R10 对 `apply` 的判据（Err 或 `tun_ready == Some(false)`
/// 都算失败）同样适用于回滚方向。不说「换回也失败」——配置确实换回去了，只是接口没起来。
pub const ROLLED_BACK_TUN_DOWN: &str = "已换回原来的配置，但 bui-tun 还是没起来";
/// 回滚本身也失败（spec §5.5 Switch 那一行）。
pub const ROLLBACK_FAILED: &str = "换回也失败";
/// 回滚也失败之后的出路。
pub const ROLLBACK_NEXT: &str = "用 [4] 服务控制 → 重启，或 [1] 选一个节点";
/// 预检里 `sing-box check` 不通过时的下一步（spec §0.2 R1）。
pub const NEXT_PICK_ANOTHER: &str = "换一个节点再删：确认时输入别的编号";
/// 预检里内核还缺时的下一步（spec §0.2 R1）。
pub const NEXT_KERNEL: &str = "先检查更新，装好内核再删：[7] 更新与维护 → [1] 检查更新";
/// 删光之后撤 UFW 失败：尽力而为，只说一句（spec §0.2 R10）。
pub const UFW_LEFT: &str = "UFW 规则没撤掉，不影响上网";
/// 确认那一问答了否（spec §5.3 输入表）。
pub const CANCELLED: &str = "已取消，没有删除任何节点";
/// 会断网的删除只输了 `y`（spec §0.2 R1）。
pub const NEEDS_YES: &str = "会断网的删除要输入 yes";
/// 一个节点都没有时不进删除页（spec §5.1）。
pub const NO_NODES: &str = "没有节点可删";
/// 命令行的确认块里打了编号（spec §5.10「命令行的确认不认编号，换目标用 `--switch-to`」）：
/// 按取消处理，但要说清命令行换目标的办法——从菜单养成的习惯是打编号。
pub const CLI_NO_NUMBER: &str = "命令行不认编号，换目标请加 --switch-to <名字>";
/// Passive 形态（没删到当前节点）下给了 `--switch-to`：校验照做（名字打错要早报），但它没有
/// 作用对象。不报用法错误——R15 把退出码 2 严格限定在三种，多一种会让脚本分不清。
pub const SWITCH_TO_UNUSED: &str = "没有删到当前节点，--switch-to 用不上：当前节点不变";

/// 不在「删完切到」形态时打了编号（spec §5.3 输入表）。会断网的形态（Switch、Empty）要
/// 输入 `yes`，文案跟着换说法（`needs_word`，spec §0.2 R1）。
pub fn only_y(needs_word: bool) -> String {
    let word = if needs_word { "yes" } else { "y" };
    format!("这一步只认 {word}，别的都算取消")
}

/// 改替换目标时打了一个正在删的编号（spec §5.3 输入表）。`i` 是 0-based 下标。
pub fn also_a_target(i: usize) -> String {
    format!("[{}] 也在要删的节点里，换一个", i + 1)
}

/// 删除失败的停顿页（spec §5.5 的失败列、§3c-60-7，下一步照 R1）：每句按 `width` 折行、
/// 缩进两列，行尾带换行。整块交给 `Ctx::show`；宽度守门表也用它。
pub fn page(lines: &[String], width: usize) -> String {
    lines.iter().map(|l| menu::wrap(l, 2, width)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::baiyi_like;
    use pretty_assertions::assert_eq;

    const ACTIVE: &str = "hysteria2-1778329470";
    const RICK_REALITY: &str = "rick-node.example-a.net-reality-direct";

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn all_names(prof: &Profiles) -> Vec<String> {
        prof.profiles.iter().map(|p| p.name.clone()).collect()
    }

    /// 只留 tizi 上的 5 个节点：剩下的节点全在被删节点的服务器上那一支。
    fn only_tizi() -> Profiles {
        let mut p = baiyi_like();
        p.profiles.retain(|x| x.node.host == "tizi.example.test");
        p
    }

    #[test]
    fn plan_passive_when_the_active_node_survives() {
        let prof = baiyi_like();
        let p = plan(&prof, &names(&["HY2"]), None).unwrap();
        assert_eq!(p.kind, PlanKind::Passive);
        assert_eq!(p.targets, names(&["HY2"]));
        assert_eq!(p.next.profiles.len(), 8);
        assert!(!p.next.profiles.iter().any(|x| x.name == "HY2"));
        assert_eq!(p.next.active.as_deref(), Some(ACTIVE), "当前节点不变");
        // 重复的名字静默去重，顺序按给进来的顺序
        let p = plan(&prof, &names(&["HY2", "HY2"]), None).unwrap();
        assert_eq!(p.targets, names(&["HY2"]));
        // 本来就没有活动节点：删别的也是 Passive
        let mut none = baiyi_like();
        none.active = None;
        let p = plan(&none, &names(&["HY2"]), None).unwrap();
        assert_eq!(p.kind, PlanKind::Passive);
        assert_eq!(p.next.active, None);
    }

    #[test]
    fn plan_switch_uses_the_given_target_and_rejects_bad_ones() {
        let prof = baiyi_like();
        let active = names(&[ACTIVE]);
        let p = plan(&prof, &active, Some("reality-Reality")).unwrap();
        assert_eq!(
            p.kind,
            PlanKind::Switch {
                to: "reality-Reality".into()
            }
        );
        assert_eq!(p.next.active.as_deref(), Some("reality-Reality"));
        assert_eq!(p.next.profiles.len(), 8);
        // 不给 --switch-to 就按 default_to 挑：跳过被删节点所在的服务器
        let p = plan(&prof, &active, None).unwrap();
        assert_eq!(
            p.kind,
            PlanKind::Switch {
                to: RICK_REALITY.into()
            }
        );
        assert_eq!(p.next.active.as_deref(), Some(RICK_REALITY));
        // 不存在、或也在删除之列：什么都不删
        assert_eq!(
            plan(&prof, &active, Some("nope")).unwrap_err(),
            PlanError::BadSwitchTo("nope".into())
        );
        assert_eq!(
            plan(&prof, &names(&[ACTIVE, "HY2"]), Some("HY2")).unwrap_err(),
            PlanError::SwitchToIsTarget("HY2".into())
        );
        // 删非活动节点时给的 --switch-to 也照样校验
        assert_eq!(
            plan(&prof, &names(&["HY2"]), Some("nope")).unwrap_err(),
            PlanError::BadSwitchTo("nope".into())
        );
        for e in [
            PlanError::BadSwitchTo("nope".into()),
            PlanError::SwitchToIsTarget("HY2".into()),
        ] {
            assert!(e.to_string().contains("什么都没删"), "{e}");
        }
    }

    #[test]
    fn plan_empty_when_nothing_is_left() {
        let mut prof = baiyi_like();
        prof.mode = Mode::Tun;
        prof.socks_port = 11080;
        prof.http_port = 18080;
        let p = plan(&prof, &all_names(&prof), None).unwrap();
        assert_eq!(p.kind, PlanKind::Empty);
        assert!(p.next.profiles.is_empty());
        assert_eq!(p.next.active, None);
        // 模式、端口、panel、auto_update 都保留（spec §5.5 Empty 那一行）
        assert_eq!(p.next.mode, Mode::Tun);
        assert_eq!(p.next.socks_port, 11080);
        assert_eq!(p.next.http_port, 18080);
        assert_eq!(p.next.auto_update, prof.auto_update);
        assert_eq!(p.next.panel, prof.panel);
        assert_eq!(p.targets.len(), 9);
    }

    #[test]
    fn plan_not_found_deletes_nothing() {
        let prof = baiyi_like();
        let e = plan(&prof, &names(&["HY2", "nope", "nada", "nope"]), None).unwrap_err();
        assert_eq!(e, PlanError::NotFound(names(&["nope", "nada"])));
        let text = e.to_string();
        assert!(text.contains("没有叫 nope、nada 的节点"), "{text}");
        assert!(text.contains("bui-c list"), "{text}");
        assert!(text.contains("什么都没删"), "{text}");
        // 名字里的控制字符不原样回显
        let e = plan(&prof, &names(&["a\u{1b}b"]), None).unwrap_err();
        assert!(e.to_string().contains("a?b"), "{e}");
    }

    #[test]
    fn default_to_skips_the_hosts_of_deleted_nodes() {
        let prof = crate::testutil::baiyi_like();
        // 删 tizi.example.test:10000 那个活动节点：剩下的第一个（HY2，tizi:40000）也在 tizi 上，要跳过
        let t = vec!["hysteria2-1778329470".to_string()];
        let i = default_to(&prof, &t, None).unwrap();
        assert_ne!(prof.profiles[i].node.host, "tizi.example.test");
        assert_eq!(
            prof.profiles[i].name,
            "rick-node.example-a.net-reality-direct"
        );
        // 有测速结果（从 [9] 进来）就用它
        assert_eq!(default_to(&prof, &t, Some(8)), Some(8));
        // 测速结果自己也在删除之列：不能用，按规则重挑
        assert_eq!(default_to(&prof, &t, Some(1)), Some(3));
    }

    #[test]
    fn default_to_falls_back_to_the_first_remaining_when_all_share_the_host() {
        let mut prof = only_tizi();
        assert_eq!(prof.profiles.len(), 5);
        let t = names(&[ACTIVE]);
        assert_eq!(default_to(&prof, &t, None), Some(0));
        assert_eq!(prof.profiles[0].name, "HY2");
        // host 比较不分大小写
        prof.profiles[0].node.host = "TIZI.Example.Test".into();
        assert_eq!(default_to(&prof, &t, None), Some(0));
        // 一个都不剩：没有替换目标
        assert_eq!(default_to(&prof, &all_names(&prof), None), None);
    }

    #[test]
    fn summary_is_short_and_middle_truncates_the_name() {
        let passive = Report {
            deleted: names(&["HY2"]),
            active: Some(ACTIVE.into()),
            switched: false,
            stopped: false,
            remaining: 8,
        };
        assert_eq!(summary(&passive, 80), "已删 1 个");
        assert_eq!(summary(&passive, 40), "已删 1 个");
        let switched = Report {
            active: Some(RICK_REALITY.into()),
            switched: true,
            ..passive.clone()
        };
        assert_eq!(
            summary(&switched, 80),
            format!("已删 1 个，切到 {RICK_REALITY}")
        );
        let at40 = summary(&switched, 40);
        assert_eq!(
            at40,
            menu::fit_name_in_last(&format!("已删 1 个，切到 {RICK_REALITY}"), RICK_REALITY, 40),
            "名字单独中间截断，与切换摘要同一规则（R6 / 第 8 条）"
        );
        assert!(at40.contains('…') && at40.ends_with("direct"), "{at40}");
        assert!(
            menu::budget_width(&format!("  上次：{at40}")) <= menu::line_limit(40),
            "{at40}"
        );
        let many = Report {
            deleted: names(&["a", "b", "c"]),
            active: Some("HY2".into()),
            switched: true,
            stopped: false,
            remaining: 6,
        };
        assert_eq!(summary(&many, 80), "已删 3 个，切到 HY2");
        let stopped = Report {
            deleted: names(&["a", "b"]),
            active: None,
            switched: false,
            stopped: true,
            remaining: 0,
        };
        assert_eq!(summary(&stopped, 40), "已删光节点，按 [3] 导入");
    }

    /// 命令行说全（spec §5.4），不用「上次：」行的短式，也不把菜单键漏进命令行。
    #[test]
    fn the_command_line_summary_spells_it_out_in_all_three_forms() {
        let passive = Report {
            deleted: names(&["HY2"]),
            active: Some(ACTIVE.into()),
            switched: false,
            stopped: false,
            remaining: 8,
        };
        assert_eq!(cli_summary(&passive), "已删除 1 个节点，还剩 8 个");
        let switched = Report {
            active: Some(RICK_REALITY.into()),
            switched: true,
            ..passive.clone()
        };
        assert_eq!(
            cli_summary(&switched),
            format!("当前节点已删除，切到 {RICK_REALITY}"),
            "§5.4 逐字"
        );
        let stopped = Report {
            deleted: names(&["a", "b"]),
            active: None,
            switched: false,
            stopped: true,
            remaining: 0,
        };
        let empty = cli_summary(&stopped);
        assert_eq!(
            empty,
            "已删除全部 2 个节点，代理已停止；用 bui-c import 重新导入"
        );
        assert!(!empty.contains("[3]"), "命令行里没有菜单键：{empty}");
        // 三位数的节点数也要压在 59 列内（固定文案的上限）
        let many = Report {
            deleted: (0..100).map(|i| format!("n{i}")).collect(),
            ..stopped
        };
        let w = menu::budget_width(&cli_summary(&many));
        assert!(w <= 59, "三位数节点数时占 {w} 列：{}", cli_summary(&many));
        // 名字照 §5.10 末条净化
        let dirty = Report {
            active: Some("a\u{1b}[31mb".into()),
            switched: true,
            ..passive
        };
        assert!(!cli_summary(&dirty).contains('\u{1b}'));
    }

    /// 不带节点名的失败摘要要整句放进 40 列的「上次：」行（spec §0.2 R6）：那一行只有
    /// `line_limit(40) − 「  上次：」` = 31 列，尾截会正好砍掉可操作的那半句。带名字的三条
    /// 走 `menu::fit_name_in_last` 中间截断，不在这张表里。
    #[test]
    fn the_nameless_failure_summaries_fit_the_40_column_last_line() {
        let room = menu::line_limit(40) - menu::budget_width("  上次：");
        assert_eq!(room, 31);
        for s in [
            SNAPSHOT_CHANGED_SHORT,
            SAVE_FAILED,
            TEARDOWN_FAILED_SHORT,
            STOPPED_NOT_SAVED_SHORT,
        ] {
            let w = menu::budget_width(s);
            assert!(
                w <= room,
                "{s:?} 占 {w} 列，40 列的「上次：」行只有 {room} 列"
            );
            assert_eq!(menu::truncate_end(s, room), s, "一个字都不该被截掉");
        }
        // 长句自己会被砍掉句尾——这就是要另配短式的原因
        assert!(menu::truncate_end(SNAPSHOT_CHANGED, room).ends_with('…'));
    }

    #[test]
    fn snapshot_covers_the_names_active_mode_and_ports() {
        let prof = baiyi_like();
        assert_eq!(snapshot(&prof), snapshot(&prof.clone()));
        for i in 0..5 {
            let mut other = prof.clone();
            match i {
                0 => other.profiles.swap(0, 1),
                1 => other.active = Some("HY2".into()),
                2 => other.mode = Mode::Tun,
                3 => other.socks_port += 1,
                _ => other.http_port += 1,
            }
            assert_ne!(snapshot(&other), snapshot(&prof), "第 {i} 项变了要认出来");
        }
        // 别的字段变了不算（label 改了不该拦住删除）
        let mut same = prof.clone();
        same.profiles[0].node.label = "改过的备注".into();
        assert_eq!(snapshot(&same), snapshot(&prof));
    }

    /// 第 2 条：确认块的 `needs_word` 与 `plan` 判出的形态必须一致（两边判形态的算法同源）。
    #[test]
    fn needs_word_matches_the_plan_kind_in_all_three_forms() {
        let prof = baiyi_like();
        let cases: Vec<(Vec<usize>, Vec<String>, Option<usize>)> = vec![
            (vec![0], names(&["HY2"]), None),
            (vec![1], names(&[ACTIVE]), Some(3)),
            ((0..9).collect(), all_names(&prof), None),
        ];
        for (picks, targets, repl) in cases {
            let to = repl.map(|i| prof.profiles[i].name.clone());
            let kind = plan(&prof, &targets, to.as_deref()).unwrap().kind;
            let c = menu::delete_confirm(&prof, &picks, repl, 60, 24);
            assert_eq!(
                c.needs_word,
                !matches!(kind, PlanKind::Passive),
                "{kind:?} 的 needs_word 对不上"
            );
            let cli = menu::delete_confirm_cli(&prof, &picks, repl, 60);
            assert_eq!(cli.needs_word, c.needs_word, "命令行版一样");
            // 会断网的两种形态提示 [yes/N]，Passive 提示 [y/N]
            let tag = if c.needs_word { "[yes/N]" } else { "[y/N]" };
            assert!(c.question.ends_with(tag), "{}", c.question);
        }
    }

    /// 第 3 条：没有测速结果时，「剩下的节点都在…」那一行出现，当且仅当 `default_to`
    /// 走了「都在被删节点的服务器上、回落到剩下的第一个」那条分支。
    #[test]
    fn the_same_host_note_appears_exactly_when_default_to_falls_back() {
        const NOTE: &str = "剩下的节点都在";
        for base in [baiyi_like(), only_tizi()] {
            for pick in 0..base.profiles.len() {
                let mut prof = base.clone();
                prof.active = Some(prof.profiles[pick].name.clone());
                let targets = vec![prof.profiles[pick].name.clone()];
                let to = default_to(&prof, &targets, None).unwrap();
                let gone = prof.profiles[pick].node.host.to_lowercase();
                // 回落分支 = 剩下的每个节点都在被删节点的服务器上
                let fell_back = prof.profiles[to].node.host.to_lowercase() == gone;
                let all_left_on_gone = prof
                    .profiles
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != pick)
                    .all(|(_, p)| p.node.host.to_lowercase() == gone);
                assert_eq!(fell_back, all_left_on_gone, "第 {pick} 个：回落判据");
                let body = menu::delete_confirm(&prof, &[pick], Some(to), 60, 40).body;
                assert_eq!(
                    body.contains(NOTE),
                    all_left_on_gone,
                    "第 {pick} 个：\n{body}"
                );
                let cli = menu::delete_confirm_cli(&prof, &[pick], Some(to), 60).body;
                assert_eq!(
                    cli.contains(NOTE),
                    all_left_on_gone,
                    "命令行版一样：\n{cli}"
                );
            }
        }
    }

    #[test]
    fn the_failure_page_wraps_every_line_at_40_columns() {
        let lines = vec![
            format!("删除没做：切到 {RICK_REALITY} 的配置校验不通过"),
            "sing-box check 不通过：outbounds[0]: bad type".to_string(),
            STILL_THERE.to_string(),
            NEXT_PICK_ANOTHER.to_string(),
        ];
        for w in [40, 50, 60, 80, 100] {
            let text = page(&lines, w);
            for l in text.lines() {
                assert!(
                    menu::budget_width(l) <= menu::line_limit(w),
                    "@{w}: {l:?} = {}",
                    menu::budget_width(l)
                );
                assert!(l.starts_with("  "), "缩进两列：{l:?}");
            }
        }
        assert_eq!(only_y(true), "这一步只认 yes，别的都算取消");
        assert_eq!(only_y(false), "这一步只认 y，别的都算取消");
        assert_eq!(also_a_target(2), "[3] 也在要删的节点里，换一个");
    }
}
