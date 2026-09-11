//! 三个 systemd 单元的文本（决策 3）：
//! `bui-c.service`（sing-box 常驻数据面）+ `bui-c-check.service`（oneshot 巡检）
//! + `bui-c.timer`（每分钟激活前者）。
//!
//! timer 必须显式写 `Unit=bui-c-check.service`：systemd 的默认行为是激活同名
//! `.service`，只写两个单元会让 `bui-c.timer` 每分钟去重启 sing-box 本身。

use crate::paths::{Paths, SELF_BIN, UNIT_CHECK};

/// sing-box 常驻单元。`WorkingDirectory` 固定在 `base`，免得内核的相对路径产物落到 `/`。
pub fn main_service(paths: &Paths) -> String {
    format!(
        "[Unit]\n\
         Description=B-UI Client (sing-box)\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         StartLimitIntervalSec=0\n\
         \n\
         [Service]\n\
         Type=simple\n\
         WorkingDirectory={base}\n\
         ExecStart={singbox} run -c {config}\n\
         Restart=always\n\
         RestartSec=3\n\
         LimitNOFILE=1048576\n\
         TimeoutStopSec=10\n\
         \n\
         [Install]\n\
         WantedBy=multi-user.target\n",
        base = paths.base.display(),
        singbox = paths.singbox().display(),
        config = paths.config().display(),
    )
}

/// 巡检单元：oneshot 调自己的 `check`，由 timer 激活，不需要 `[Install]`。
pub fn check_service() -> String {
    format!(
        "[Unit]\n\
         Description=B-UI Client watchdog\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         ExecStart={SELF_BIN} check\n"
    )
}

/// 每分钟的巡检定时器。
pub fn timer() -> String {
    format!(
        "[Unit]\n\
         Description=B-UI Client watchdog timer\n\
         \n\
         [Timer]\n\
         Unit={UNIT_CHECK}\n\
         OnBootSec=1min\n\
         OnUnitActiveSec=1min\n\
         AccuracySec=15s\n\
         \n\
         [Install]\n\
         WantedBy=timers.target\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::{Paths, UNIT_CHECK};

    #[test]
    fn main_service_runs_bundled_singbox_with_bundled_config() {
        let u = main_service(&Paths::new("/opt/bui-c", "/etc/systemd/system"));
        assert!(u.contains("ExecStart=/opt/bui-c/bin/sing-box run -c /opt/bui-c/config.json"));
        assert!(u.contains("Restart=always"));
        assert!(u.contains("LimitNOFILE=1048576"));
        assert!(u.contains("WorkingDirectory=/opt/bui-c"));
        assert!(u.contains("After=network-online.target"));
        assert!(u.contains("WantedBy=multi-user.target"));
        assert!(!u.contains("Conflicts="));
    }

    #[test]
    fn timer_targets_the_oneshot_check_unit_not_the_daemon() {
        let t = timer();
        assert!(
            t.contains(&format!("Unit={UNIT_CHECK}")),
            "否则 timer 默认去激活 bui-c.service（sing-box 本身）"
        );
        assert!(t.contains("OnUnitActiveSec=1min"));
        assert!(t.contains("OnBootSec=1min"));
        assert!(t.contains("WantedBy=timers.target"));
    }

    #[test]
    fn check_service_is_oneshot_calling_self() {
        let c = check_service();
        assert!(c.contains("Type=oneshot"));
        assert!(c.contains("ExecStart=/usr/local/bin/bui-c check"));
        assert!(!c.contains("[Install]"), "由 timer 激活，不需要 enable");
    }
}
