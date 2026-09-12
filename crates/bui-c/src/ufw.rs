//! UFW 行为迁移（裁决记录「P4：UFW 改为放行 `bui-tun` 接口」）：
//! v3.6 在 TUN 期间 `ufw disable` 把整墙关掉；v4 只加两条接口规则，墙全程开着。
//!
//! 编排在 T12 的 CLI：TUN 模式 `apply` 前 [`allow_tun`]，切 socks 与 `uninstall` 后
//! [`revoke_tun`]。`engine` 不认识 ufw，保持单一职责。

use crate::paths::TUN_IFACE;
use crate::sys::Sys;
use crate::{Error, Result};

/// `ufw status` 的标准输出；命令跑不通或非零退出都算「没装」。
fn status<S: Sys>(sys: &S) -> Option<String> {
    sys.run("ufw", &["status"])
        .ok()
        .filter(|o| o.ok())
        .map(|o| o.stdout)
}

pub fn installed<S: Sys>(sys: &S) -> bool {
    status(sys).is_some()
}

pub fn active<S: Sys>(sys: &S) -> bool {
    status(sys).is_some_and(|s| s.contains("Status: active"))
}

fn must<S: Sys>(sys: &S, args: &[&str]) -> Result<()> {
    let o = sys.run("ufw", args)?;
    if o.ok() {
        return Ok(());
    }
    // ufw 的报错有时走 stdout（`ERROR: Bad interface name`），两路都收进错误里，
    // 否则「规则被拒」在日志里只剩一个退出码
    let detail = match (o.stderr.trim(), o.stdout.trim()) {
        (err, "") => err.to_string(),
        ("", out) => out.to_string(),
        (err, out) => format!("{err}\n{out}"),
    };
    Err(Error::Command {
        prog: format!("ufw {}", args.join(" ")),
        code: o.code,
        stderr: detail,
    })
}

/// TUN 开启时放行 `bui-tun` 接口。`route allow` 是关键：
/// UFW 默认 FORWARD DROP 会掐掉 sing-box 转发的 TCP——v3 为此把整墙关了。
///
/// 未装或未启用 → `Ok(false)`（无事可做，不是错）。
pub fn allow_tun<S: Sys>(sys: &S) -> Result<bool> {
    if !active(sys) {
        return Ok(false);
    }
    must(sys, &["allow", "in", "on", TUN_IFACE])?;
    must(sys, &["route", "allow", "in", "on", TUN_IFACE])?;
    Ok(true)
}

/// 撤回两条规则。未装 → `Ok(false)`；墙当前关着也照样撤（规则还在配置里）。
pub fn revoke_tun<S: Sys>(sys: &S) -> Result<bool> {
    if !installed(sys) {
        return Ok(false);
    }
    // 规则不存在时 ufw 也返回 0，失败只可能是参数问题，照样报出来
    must(sys, &["delete", "allow", "in", "on", TUN_IFACE])?;
    must(sys, &["route", "delete", "allow", "in", "on", TUN_IFACE])?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeSys;

    #[test]
    fn no_ufw_installed_is_a_quiet_noop() {
        let s = FakeSys::new();
        s.reply("ufw status", 127, "");
        assert!(!installed(&s));
        assert!(!allow_tun(&s).unwrap());
        assert!(!revoke_tun(&s).unwrap());
        assert!(!s.called("ufw allow in on bui-tun"));
    }

    #[test]
    fn inactive_ufw_needs_no_rules() {
        let s = FakeSys::new();
        s.reply("ufw status", 0, "Status: inactive\n");
        assert!(installed(&s) && !active(&s));
        assert!(!allow_tun(&s).unwrap());
    }

    #[test]
    fn active_ufw_gets_interface_and_forward_rules_not_a_disable() {
        let s = FakeSys::new();
        s.reply("ufw status", 0, "Status: active\n\nTo    Action  From\n");
        assert!(allow_tun(&s).unwrap());
        assert!(s.called("ufw allow in on bui-tun"));
        assert!(s.called("ufw route allow in on bui-tun"));
        assert!(
            !s.calls().iter().any(|c| c == "ufw disable"),
            "v4 不再关整墙（v3 的做法）"
        );
    }

    #[test]
    fn revoke_deletes_both_rules() {
        let s = FakeSys::new();
        s.reply("ufw status", 0, "Status: active\n");
        assert!(revoke_tun(&s).unwrap());
        assert!(s.called("ufw delete allow in on bui-tun"));
        assert!(s.called("ufw route delete allow in on bui-tun"));
    }

    #[test]
    fn allow_reports_error_when_ufw_rejects_the_rule() {
        let s = FakeSys::new();
        s.reply("ufw status", 0, "Status: active\n");
        s.reply("ufw allow in on bui-tun", 1, "ERROR: Bad interface name");
        let e = allow_tun(&s).unwrap_err();
        assert!(e.to_string().contains("Bad interface name"), "{e}");
    }
}
