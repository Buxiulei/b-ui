//! `bui-c uninstall`：停用并删除三个单元、清 `/opt/bui-c`、删残留 TUN 接口、
//! 撤 ufw 规则；`purge_bin` 才连自身二进制一起删。
//!
//! 全程幂等：干净机器上跑一遍返回全 false 的 [`Report`]，不报错。

use crate::paths::{Paths, SELF_BIN, TUN_IFACE, UNIT_CHECK, UNIT_MAIN, UNIT_TIMER};
use crate::sys::{systemd, Sys};
use crate::{ufw, Result};
use std::path::Path;

/// 实际做掉的事，供 CLI 打印与 `check::Runtime` 记账。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Report {
    pub removed_units: Vec<String>,
    pub purged_base: bool,
    pub purged_bin: bool,
    pub ufw_revoked: bool,
}

pub fn run<S: Sys>(sys: &S, paths: &Paths, purge_bin: bool) -> Result<Report> {
    let mut r = Report::default();
    // 顺序固定：timer → check → 主单元。反过来会出现「刚停完 sing-box 又被巡检拉起」
    for u in [UNIT_TIMER, UNIT_CHECK, UNIT_MAIN] {
        systemd::stop_quiet(sys, u);
        systemd::disable_quiet(sys, u);
        systemd::reset_failed_quiet(sys, u);
    }
    for u in [UNIT_MAIN, UNIT_CHECK, UNIT_TIMER] {
        let f = paths.unit(u);
        if sys.exists(&f) {
            sys.remove_file(&f)?;
            r.removed_units.push(u.to_string());
        }
    }
    systemd::daemon_reload(sys)?;
    // 单元停了 TUN 接口通常自己就没了；留下的残骸删掉，没有也不算错
    let _ = sys.run("ip", &["link", "delete", TUN_IFACE]);
    r.ufw_revoked = ufw::revoke_tun(sys)?;

    if sys.exists(&paths.base) {
        sys.remove_dir_all(&paths.base)?;
        r.purged_base = true;
    }
    if purge_bin && sys.exists(Path::new(SELF_BIN)) {
        sys.remove_file(Path::new(SELF_BIN))?;
        r.purged_bin = true;
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeSys;
    use pretty_assertions::assert_eq;

    fn paths() -> Paths {
        Paths::new("/opt/bui-c", "/etc/systemd/system")
    }

    fn installed(s: &FakeSys) {
        for u in [UNIT_MAIN, UNIT_CHECK, UNIT_TIMER] {
            s.put(&format!("/etc/systemd/system/{u}"), "[Unit]");
        }
        s.put("/opt/bui-c/profiles.json", "{}");
        s.put("/opt/bui-c/config.json", "{}");
        s.put("/opt/bui-c/bin/sing-box", "ELF");
        s.put("/usr/local/bin/bui-c", "ELF");
    }

    #[test]
    fn removes_units_dir_and_interface() {
        let s = FakeSys::new();
        installed(&s);
        s.reply("ufw status", 0, "Status: active\n");
        let r = run(&s, &paths(), false).unwrap();
        assert_eq!(
            r.removed_units,
            vec![
                UNIT_MAIN.to_string(),
                UNIT_CHECK.to_string(),
                UNIT_TIMER.to_string()
            ]
        );
        assert!(r.purged_base && !r.purged_bin && r.ufw_revoked);
        // timer 先停再停主单元，避免刚停完 sing-box 又被巡检拉起来
        let calls = s.calls();
        let i_timer = calls
            .iter()
            .position(|c| c == &format!("systemctl stop {UNIT_TIMER}"))
            .unwrap();
        let i_main = calls
            .iter()
            .position(|c| c == &format!("systemctl stop {UNIT_MAIN}"))
            .unwrap();
        assert!(i_timer < i_main, "先停 timer 再停 sing-box");
        assert!(s.called("systemctl daemon-reload"));
        assert!(s.called("ip link delete bui-tun"));
        assert!(s.called("ufw delete allow in on bui-tun"));
        assert!(!s.exists(std::path::Path::new("/opt/bui-c/profiles.json")));
        assert!(!s.exists(std::path::Path::new("/opt/bui-c/bin/sing-box")));
        assert!(
            s.exists(std::path::Path::new("/usr/local/bin/bui-c")),
            "默认保留自身二进制"
        );
    }

    #[test]
    fn purge_bin_removes_the_binary_too() {
        let s = FakeSys::new();
        installed(&s);
        let r = run(&s, &paths(), true).unwrap();
        assert!(r.purged_bin);
        assert!(!s.exists(std::path::Path::new("/usr/local/bin/bui-c")));
    }

    #[test]
    fn is_idempotent_on_a_clean_machine() {
        let s = FakeSys::new();
        let r = run(&s, &paths(), true).unwrap();
        assert_eq!(
            r,
            Report {
                removed_units: vec![],
                purged_base: false,
                purged_bin: false,
                ufw_revoked: false
            }
        );
        // `ufw status` 未登记 → FakeSys 的查询默认值 127（没装 ufw，T3 `default_code`）
        // → `installed()` 为假 → 不去撤规则，`ufw_revoked` 才可能是 false
        assert!(!s.called("ufw delete allow in on bui-tun"));
    }
}
