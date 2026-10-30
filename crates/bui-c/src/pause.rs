//! `bui-c stop` / `bui-c start`：用户意图「现在不要代理」，不是期望配置。
//!
//! 暂停只落一个标记文件 `<base>/paused`（0600，一行 `RFC3339 来源`），`profiles.json`
//! 一个字节都不动（旧版本读到它仍逐字节兼容）。判据只有一处（[`is_paused`]）：
//! 巡检看到标记就返回 `Verdict::Paused`，既不探测也不重启，`bui-c.timer` 因此拉不回来。
//!
//! `stop` 的命令序列与 `start` 的前半段都在这里（只经 [`Sys`] / [`systemd`]），CLI 与菜单共用；
//! `start` 的后半段（`apply_with_ufw` + `enable`）在 `cli`，因为那条路径要装内核、动 UFW。

use crate::paths::{Paths, UNIT_MAIN};
use crate::profiles::{rfc3339, Profiles};
use crate::sys::{systemd, Sys};
use crate::{Error, Result};

/// 标记里记的来源：命令行 `bui-c stop`。
pub const SOURCE_CLI: &str = "cli";
/// 标记里记的来源：菜单 `[4] 服务控制` → `[3] 停止代理`。
pub const SOURCE_MENU: &str = "menu";

/// 状态区里的说法（`bui-c status` 与菜单共用）。
pub const PAUSED_WORD: &str = "已停止（直连）";
/// `stop` 的结果行。
pub const STOPPED: &str = "已停止代理，流量恢复直连；bui-c start 恢复";
/// `start` 的结果行。
pub const RESUMED: &str = "已恢复代理";
/// `start` 时代理本来就在跑、也没有暂停标记。
pub const ALREADY_RUNNING: &str = "代理已在运行";
/// `stop` 时既没有单元文件、也没有节点：没有可停的东西，**不写标记**。
pub const NO_PROXY: &str = "本机没有安装代理（没有 bui-c.service 单元，也没有节点），不需要停止";
/// `start` 时没有活动节点：报错，**不清标记**。
pub const NO_NODE: &str = "没有可用的节点，先导入或切换节点（bui-c import）再恢复代理";
/// 暂停期间 `bui-c update` 的拒绝理由。
pub const UPDATE_REFUSED: &str = "代理已停止，先 bui-c start";
/// `bui-c check` 在暂停期间那一行（每分钟一次，journald 可见）。
pub const CHECK_SKIPPED: &str = "已停止（直连），巡检跳过";
/// 暂停期间任何非显式激活的 apply 被跳过时那一行（4.3.0）。
pub const APPLY_SKIPPED: &str = "代理已停止，配置未应用；bui-c start 后生效";
/// 菜单 `[5] 连接检查` 在暂停期间的那一行。
pub const CHECK_MENU_SKIPPED: &str = "已停止（直连），先 [4] 服务控制 → [3] 恢复代理再检查";
/// 菜单 `[4] 服务控制` → `[1] 重启` 在暂停期间的那一行（不静默清标记，指到 [3]）。
pub const RESTART_SKIPPED: &str = "代理已停止，未重启；用 [3] 恢复代理";
/// 删除切节点失败后回滚时的那一行（4.3.0）：暂停期间不启动服务，旧配置也不换回，
/// `pending.json` 留给 `bui-c start` 之后的下一次收敛。
pub const ROLLBACK_SKIPPED: &str = "代理已停止，没有换回旧配置；bui-c start 后按节点列表收敛";

/// 暂停中？标记存在即暂停；内容坏了也算（只认存在性，读不到内容不影响判据）。
pub fn is_paused<S: Sys>(sys: &S, paths: &Paths) -> bool {
    sys.exists(&paths.paused())
}

/// 写标记：一行 `<RFC3339> <来源>`，0600 原子写。
pub fn mark<S: Sys>(sys: &S, paths: &Paths, source: &str) -> Result<()> {
    let line = format!("{} {source}\n", rfc3339(sys));
    sys.mkdir_p(&paths.base)?;
    sys.write(&paths.paused(), line.as_bytes(), 0o600)
}

/// 清标记；本来就没有也不算错。
pub fn clear<S: Sys>(sys: &S, paths: &Paths) -> Result<()> {
    sys.remove_file(&paths.paused())
}

/// `bui-c stop` 的机械动作：写标记 → `systemctl disable` → `systemctl stop`。
///
/// `bui-c.timer` **不动**：巡检仍每分钟跑一次（只记一行「已停止」），`start` 之后无需再
/// enable timer。`installed` 为假（没有单元文件、也没有节点）时返回 `Ok(false)`，
/// 不写标记、不碰 systemd。重复执行幂等。
pub fn stop<S: Sys>(sys: &S, paths: &Paths, installed: bool, source: &str) -> Result<bool> {
    if !installed {
        return Ok(false);
    }
    mark(sys, paths, source)?;
    systemd::disable_quiet(sys, UNIT_MAIN);
    systemd::stop_quiet(sys, UNIT_MAIN);
    Ok(true)
}

/// `bui-c start` 的前半段（持锁调用）：没有活动节点报错、**不清标记**；已经在跑且没有标记时
/// 返回 `Ok(false)`（不重启）；否则清标记并返回 `Ok(true)`，由调用方
/// `apply_with_ufw` → `systemctl enable bui-c.service` → timer 不 active 时 start 它。
pub fn begin_start<S: Sys>(sys: &S, paths: &Paths, prof: &Profiles) -> Result<bool> {
    if prof.active_profile().is_none() {
        return Err(Error::msg(NO_NODE));
    }
    if !is_paused(sys, paths) && systemd::is_active(sys, UNIT_MAIN) {
        return Ok(false);
    }
    clear(sys, paths)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeSys;
    use crate::testutil::profiles_socks;
    use pretty_assertions::assert_eq;

    fn paths() -> Paths {
        Paths::new("/opt/bui-c", "/etc/systemd/system")
    }

    #[test]
    fn the_marker_is_one_line_with_the_time_and_source_and_is_0600() {
        let s = FakeSys::new();
        assert!(!is_paused(&s, &paths()));
        mark(&s, &paths(), SOURCE_CLI).unwrap();
        assert!(is_paused(&s, &paths()));
        assert_eq!(
            s.get("/opt/bui-c/paused").unwrap(),
            "2026-09-11T00:00:00Z cli\n",
            "一行 RFC3339 + 来源"
        );
        assert_eq!(s.mode("/opt/bui-c/paused"), Some(0o600));
        clear(&s, &paths()).unwrap();
        assert!(!is_paused(&s, &paths()));
        clear(&s, &paths()).unwrap();
    }

    #[test]
    fn stop_marks_first_then_disables_and_stops_and_leaves_the_timer_alone() {
        let s = FakeSys::new();
        assert!(stop(&s, &paths(), true, SOURCE_CLI).unwrap());
        assert!(is_paused(&s, &paths()));
        assert_eq!(
            s.get("/opt/bui-c/paused").unwrap(),
            "2026-09-11T00:00:00Z cli\n"
        );
        assert_eq!(
            s.calls(),
            vec![
                "systemctl disable bui-c.service",
                "systemctl stop bui-c.service"
            ],
            "写标记在 systemctl 之前：标记一落，timer 拉起来的巡检就看不到未暂停状态"
        );
        assert!(
            !s.calls().iter().any(|c| c.contains("bui-c.timer")),
            "timer 不动"
        );

        // 重复执行幂等：照样 disable + stop，不报错
        assert!(stop(&s, &paths(), true, SOURCE_CLI).unwrap());
        assert_eq!(
            s.calls()
                .iter()
                .filter(|c| *c == "systemctl disable bui-c.service")
                .count(),
            2
        );
    }

    #[test]
    fn stop_without_a_unit_or_a_node_writes_nothing() {
        let s = FakeSys::new();
        assert!(!stop(&s, &paths(), false, SOURCE_CLI).unwrap());
        assert!(!is_paused(&s, &paths()));
        assert!(s.calls().is_empty(), "{:?}", s.calls());
    }

    #[test]
    fn start_refuses_without_an_active_node_and_keeps_the_marker() {
        let s = FakeSys::new();
        mark(&s, &paths(), SOURCE_CLI).unwrap();
        let e = begin_start(&s, &paths(), &Profiles::new_default()).unwrap_err();
        assert!(e.to_string().contains("没有可用的节点"), "{e}");
        assert!(is_paused(&s, &paths()), "报错不清标记");
    }

    #[test]
    fn start_on_a_running_machine_without_the_marker_does_not_restart() {
        let s = FakeSys::new();
        s.reply("systemctl is-active --quiet bui-c.service", 0, "");
        assert!(!begin_start(&s, &paths(), &profiles_socks()).unwrap());
        assert!(!s.called("systemctl restart bui-c.service"));
        assert!(!s.called("systemctl start bui-c.service"));
    }

    #[test]
    fn start_clears_the_marker_when_it_has_to_apply() {
        let s = FakeSys::new();
        s.reply("systemctl is-active --quiet bui-c.service", 3, "");
        mark(&s, &paths(), SOURCE_CLI).unwrap();
        assert!(begin_start(&s, &paths(), &profiles_socks()).unwrap());
        assert!(!is_paused(&s, &paths()));
    }
}
