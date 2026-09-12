//! 期望态控制器：组装 `ClientOpts` → `bui_schema::render::client` 渲染 →
//! 随包 sing-box `check` 校验 → 原子落盘 → 写三个单元 → `systemctl`。
//!
//! 本 crate 不生成任何 sing-box JSON 字段，渲染全在 `bui-schema`（P0）。
//! `apply` 幂等：配置与单元逐字节比对，没变就不重启；TUN 的内核前置（清残留接口、
//! `rp_filter=2`、`ip_forward=1`）只在真要（重）启动时做——对一个活着的 TUN 跑
//! `ip link delete` 只会拿到 EOPNOTSUPP 并白刷日志。

use crate::paths::{Paths, TUN_IFACE, UNIT_CHECK, UNIT_MAIN, UNIT_TIMER};
use crate::profiles::{Mode, Profile, Profiles};
use crate::sys::{systemd, Sys};
use crate::units;
use crate::{Error, Result};
use bui_schema::render::client::{mixed_config, tun_config, ClientMode, ClientOpts};
use std::time::Duration;

/// TUN 就绪轮询次数：10 × 500ms = 5s。
pub const APPLY_POLL_STEPS: u32 = 10;
const POLL_INTERVAL_MS: u64 = 500;

/// 一次 `apply` 实际改了什么，菜单与 `check` 据此决定提示语与后续动作。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Applied {
    pub config_changed: bool,
    pub units_changed: bool,
    pub restarted: bool,
    /// TUN 模式下接口是否就绪；socks 模式为 `None`
    pub tun_ready: Option<bool>,
}

pub struct Engine<'a, S: Sys> {
    pub sys: &'a S,
    pub paths: &'a Paths,
}

impl<'a, S: Sys> Engine<'a, S> {
    pub fn new(sys: &'a S, paths: &'a Paths) -> Self {
        Self { sys, paths }
    }

    /// ipv6-takeover spec §3.2：四条全满足才认为主机有真 IPv6，
    /// 否则给 TUN 加 v6 地址会让 sing-tun 放弃整个 TUN。
    /// `BUI_FORCE_IPV6=1|0` 强制覆盖（经 `Sys::env` 注入，决策 11）。
    pub fn host_has_ipv6(&self) -> bool {
        match self.sys.env("BUI_FORCE_IPV6").as_deref() {
            Some("1") => return true,
            Some("0") => return false,
            _ => {}
        }
        if !self.sys.exists(std::path::Path::new("/proc/net/if_inet6")) {
            return false;
        }
        for key in [
            "net.ipv6.conf.all.disable_ipv6",
            "net.ipv6.conf.default.disable_ipv6",
        ] {
            let v = self
                .sys
                .run("sysctl", &["-n", key])
                .map(|o| o.stdout.trim().to_string())
                .unwrap_or_default();
            if v == "1" {
                return false;
            }
        }
        let route = match self.sys.run("ip", &["-6", "route", "show", "default"]) {
            Ok(o) if o.ok() => o.stdout,
            _ => return false,
        };
        let dev = match route.split_whitespace().skip_while(|w| *w != "dev").nth(1) {
            Some(d) => d.to_string(),
            None => return false,
        };
        let addrs = match self.sys.run(
            "ip",
            &["-6", "addr", "show", "dev", &dev, "scope", "global"],
        ) {
            Ok(o) if o.ok() => o.stdout,
            _ => return false,
        };
        // 只认 2000::/3 的全局地址（首字符 2 或 3）。逐行看**任一**地址：
        // ULA（fd00::/8，Docker 网桥那种）排在 GUA 前面时，只看第一个 inet6 会误判成没有 v6
        // （ipv6-takeover spec §3.2 第四条是「有任一 2000::/3 地址」）。
        addrs.lines().any(|l| {
            l.split_whitespace()
                .skip_while(|w| *w != "inet6")
                .nth(1)
                .is_some_and(|a| matches!(a.chars().next(), Some('2') | Some('3')))
        })
    }

    pub fn render(&self, prof: &Profiles, p: &Profile) -> Result<serde_json::Value> {
        let opts = ClientOpts {
            mode: match prof.mode {
                Mode::Tun => ClientMode::Tun,
                Mode::Socks => ClientMode::Mixed,
            },
            socks_port: prof.socks_port,
            http_port: prof.http_port,
            host_has_ipv6: self.host_has_ipv6(),
            split: p.split.clone(),
        };
        Ok(match prof.mode {
            Mode::Tun => tun_config(&p.node, &opts),
            Mode::Socks => mixed_config(&p.node, &opts),
        })
    }

    /// 用随包的 sing-box 校验；失败不写 `config.json`（spec §2.2「验证再重启」的客户端版）。
    pub fn verify(&self, cfg: &serde_json::Value) -> Result<()> {
        let tmp = self.paths.base.join(".config.json.new");
        let mut data = serde_json::to_vec_pretty(cfg)
            .map_err(|e| Error::parse("config.json", e.to_string()))?;
        data.push(b'\n');
        self.sys.mkdir_p(&self.paths.base)?;
        self.sys.write(&tmp, &data, 0o600)?;
        let sb = self.paths.singbox().display().to_string();
        let out = self
            .sys
            .run(&sb, &["check", "-c", &tmp.display().to_string()]);
        self.sys.remove_file(&tmp)?;
        match out {
            Ok(o) if o.ok() => Ok(()),
            Ok(o) => Err(Error::Verify(format!(
                "sing-box check 不通过：{}",
                o.stderr.trim()
            ))),
            Err(e) => Err(e),
        }
    }

    pub fn tun_up(&self) -> bool {
        self.sys
            .run("ip", &["link", "show", TUN_IFACE])
            .map(|o| o.ok())
            .unwrap_or(false)
    }

    pub fn tun_default_route(&self) -> bool {
        self.sys
            .run("ip", &["-4", "route", "show", "table", "all"])
            .map(|o| {
                o.stdout
                    .lines()
                    .any(|l| l.starts_with("default") && l.contains(TUN_IFACE))
            })
            .unwrap_or(false)
    }

    pub fn apply(&self, prof: &Profiles) -> Result<Applied> {
        if !self.sys.exists(&self.paths.singbox()) {
            return Err(Error::msg(format!(
                "内核缺失：{}，先跑 `bui-c update` 安装 sing-box",
                self.paths.singbox().display()
            )));
        }
        let p = prof
            .active_profile()
            .ok_or_else(|| Error::msg("没有激活的节点，先 `bui-c import …` 或在菜单里导入"))?;

        let cfg = self.render(prof, p)?;
        self.verify(&cfg)?;
        let mut data = serde_json::to_vec_pretty(&cfg)
            .map_err(|e| Error::parse("config.json", e.to_string()))?;
        data.push(b'\n');

        // 先算两个「变了吗」，再一次性建 Applied：
        // 先 `Applied::default()` 再逐个赋值会撞 clippy::field_reassign_with_default（-D warnings 下直接红）
        let config_changed = self.write_if_changed(&self.paths.config(), &data, 0o600)?;
        let mut units_changed = false;
        for (name, content) in [
            (UNIT_MAIN, units::main_service(self.paths)),
            (UNIT_CHECK, units::check_service()),
            (UNIT_TIMER, units::timer()),
        ] {
            if self.write_if_changed(&self.paths.unit(name), content.as_bytes(), 0o644)? {
                units_changed = true;
            }
        }
        let mut out = Applied {
            config_changed,
            units_changed,
            restarted: false,
            tun_ready: None,
        };
        if out.units_changed {
            systemd::daemon_reload(self.sys)?;
        }

        let running = systemd::is_active(self.sys, UNIT_MAIN);
        let enabled = systemd::is_enabled(self.sys, UNIT_MAIN);
        // 重启语义（P4 终审）：没在跑 → start；在跑且配置/单元变了 → restart。
        // 「在跑但被 disable」不能拿 `enable --now` 顶替 restart：--now 对已 active 的单元
        // 是空操作，新配置根本不会被加载。enabled 状态单独用 `enable` 补齐。
        let will_start = !running;
        let will_restart = running && (out.config_changed || out.units_changed);

        // 内核前置只在真要（重）启动时做：残留接口会让 sing-box 起不来，
        // 但 sing-box 已经在跑且配置没变时去 `ip link delete` 一个活着的 TUN
        // 只会拿到 EOPNOTSUPP 并刷日志。rp_filter=2 与 ip_forward=1 是 Linux TUN 的前置条件。
        if prof.mode == Mode::Tun && (will_start || will_restart) {
            let _ = self.sys.run("ip", &["link", "delete", TUN_IFACE]);
            for kv in [
                "net.ipv4.conf.all.rp_filter=2",
                "net.ipv4.conf.default.rp_filter=2",
                "net.ipv4.ip_forward=1",
            ] {
                let _ = self.sys.run("sysctl", &["-w", kv]);
            }
        }

        if will_start {
            // 一次调用同时把 enable 补上，少一次 systemctl
            if enabled {
                systemd::start(self.sys, UNIT_MAIN)?;
            } else {
                systemd::enable_now(self.sys, UNIT_MAIN)?;
            }
            out.restarted = true;
        } else {
            if will_restart {
                systemd::restart(self.sys, UNIT_MAIN)?;
                out.restarted = true;
            }
            if !enabled {
                systemd::enable(self.sys, UNIT_MAIN)?;
            }
        }
        if !systemd::is_active(self.sys, UNIT_TIMER) || !systemd::is_enabled(self.sys, UNIT_TIMER) {
            systemd::enable_now(self.sys, UNIT_TIMER)?;
        }

        if prof.mode == Mode::Tun {
            out.tun_ready = Some(if out.restarted {
                // Type=simple 的 is-active 在 exec 后立刻为真，必须连接口一起等（v3.6.0 的教训）
                let mut ready = false;
                for _ in 0..APPLY_POLL_STEPS {
                    self.sys.sleep(Duration::from_millis(POLL_INTERVAL_MS));
                    if systemd::is_active(self.sys, UNIT_MAIN) && self.tun_up() {
                        ready = true;
                        break;
                    }
                }
                ready
            } else {
                // 什么都没改：不睡，直接报接口现状
                systemd::is_active(self.sys, UNIT_MAIN) && self.tun_up()
            });
        }
        Ok(out)
    }

    pub fn stop(&self) -> Result<()> {
        systemd::stop_quiet(self.sys, UNIT_MAIN);
        let _ = self.sys.run("ip", &["link", "delete", TUN_IFACE]);
        Ok(())
    }

    fn write_if_changed(&self, path: &std::path::Path, data: &[u8], mode: u32) -> Result<bool> {
        if self.sys.exists(path) && self.sys.read(path)? == data {
            return Ok(false);
        }
        self.sys.mkdir_p(&self.paths.base)?;
        self.sys.write(path, data, mode)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeSys;
    use crate::testutil::{profiles_socks, profiles_tun};
    use pretty_assertions::assert_eq;

    fn paths() -> Paths {
        Paths::new("/opt/bui-c", "/etc/systemd/system")
    }

    /// 有内核二进制、单元未起的干净机器。
    /// `bui-c.timer` 的 is-active / is-enabled 故意不登记：FakeSys 对查询类命令的默认值
    /// 就是「没起 / 没 enable」（T3 `default_code`），所以 apply 必须去 `enable --now` 它。
    fn clean(sys: &FakeSys) {
        sys.put("/opt/bui-c/bin/sing-box", "ELF");
        sys.reply("systemctl is-active --quiet bui-c.service", 3, "");
        sys.reply("systemctl is-enabled --quiet bui-c.service", 1, "");
    }

    /// 干净机器 + 「单元一拉起来就 active」：`Type=simple` 的 is-active 在 exec 后立刻为真，
    /// FakeSys 不会因为跑过 `enable --now` 自己改状态，所以要显式登记成 0，
    /// 否则 TUN 就绪轮询里的 `is_active && tun_up` 永假。
    fn clean_and_starts(sys: &FakeSys) {
        clean(sys);
        sys.reply("systemctl is-active --quiet bui-c.service", 0, "");
    }

    #[test]
    fn apply_socks_writes_config_units_and_enables() {
        let s = FakeSys::new();
        clean(&s);
        let prof = profiles_socks();
        let a = Engine::new(&s, &paths()).apply(&prof).unwrap();
        assert_eq!(
            a,
            Applied {
                config_changed: true,
                units_changed: true,
                restarted: true,
                tun_ready: None
            }
        );
        assert_eq!(s.mode("/opt/bui-c/config.json"), Some(0o600));
        let cfg: serde_json::Value =
            serde_json::from_str(&s.get("/opt/bui-c/config.json").unwrap()).unwrap();
        assert_eq!(cfg["inbounds"][0]["type"], "mixed");
        assert!(s
            .get("/etc/systemd/system/bui-c.service")
            .unwrap()
            .contains("sing-box run"));
        assert!(s.get("/etc/systemd/system/bui-c-check.service").is_some());
        assert!(s
            .get("/etc/systemd/system/bui-c.timer")
            .unwrap()
            .contains("Unit=bui-c-check.service"));
        assert!(s.called("systemctl daemon-reload"));
        assert!(s.called("systemctl enable --now bui-c.service"));
        assert!(s.called("systemctl enable --now bui-c.timer"));
        // socks 模式不碰 TUN：不删接口、不写 rp_filter
        assert!(!s.calls().iter().any(|c| c.contains("ip link delete")));
        assert!(!s.calls().iter().any(|c| c.contains("rp_filter")));
    }

    #[test]
    fn second_apply_is_a_no_op() {
        let s = FakeSys::new();
        clean(&s);
        let prof = profiles_socks();
        let pt = paths();
        let e = Engine::new(&s, &pt);
        e.apply(&prof).unwrap();
        s.reply("systemctl is-active --quiet bui-c.service", 0, "");
        s.reply("systemctl is-enabled --quiet bui-c.service", 0, "");
        s.reply("systemctl is-active --quiet bui-c.timer", 0, "");
        s.reply("systemctl is-enabled --quiet bui-c.timer", 0, "");
        let a = e.apply(&prof).unwrap();
        assert_eq!(
            a,
            Applied {
                config_changed: false,
                units_changed: false,
                restarted: false,
                tun_ready: None
            }
        );
        assert!(!s.called("systemctl restart bui-c.service"));
    }

    #[test]
    fn running_but_disabled_unit_with_a_config_change_is_restarted_and_enabled() {
        // 在跑的单元光 `enable --now` 不会重载配置（systemd 对已 active 的单元 --now 是空操作），
        // 所以「在跑 + 配置变了」一律 restart，enabled 状态另外用 `enable` 补。
        let s = FakeSys::new();
        s.put("/opt/bui-c/bin/sing-box", "ELF");
        s.reply("systemctl is-active --quiet bui-c.service", 0, "");
        s.reply("systemctl is-enabled --quiet bui-c.service", 1, "");
        let a = Engine::new(&s, &paths()).apply(&profiles_socks()).unwrap();
        assert!(a.config_changed && a.restarted);
        assert!(
            s.called("systemctl restart bui-c.service"),
            "{:?}",
            s.calls()
        );
        assert!(
            s.called("systemctl enable bui-c.service"),
            "{:?}",
            s.calls()
        );
        assert!(
            !s.called("systemctl enable --now bui-c.service"),
            "在跑的单元不能用 enable --now 顶替 restart"
        );
    }

    #[test]
    fn running_and_disabled_without_changes_is_only_enabled_not_restarted() {
        let s = FakeSys::new();
        let prof = profiles_socks();
        let pt = paths();
        let e = Engine::new(&s, &pt);
        clean(&s);
        e.apply(&prof).unwrap(); // 第一次：写盘 + enable --now
        s.reply("systemctl is-active --quiet bui-c.service", 0, "");
        s.reply("systemctl is-active --quiet bui-c.timer", 0, "");
        s.reply("systemctl is-enabled --quiet bui-c.timer", 0, "");
        let a = e.apply(&prof).unwrap(); // 配置没变，但单元被 disable 了
        assert!(!a.restarted, "没有变更就不要打断在跑的隧道");
        assert!(s.called("systemctl enable bui-c.service"));
        assert!(!s.called("systemctl restart bui-c.service"));
    }

    #[test]
    fn stopped_but_enabled_unit_is_started_not_enabled_again() {
        let s = FakeSys::new();
        s.put("/opt/bui-c/bin/sing-box", "ELF");
        s.reply("systemctl is-active --quiet bui-c.service", 3, "");
        s.reply("systemctl is-enabled --quiet bui-c.service", 0, "");
        let a = Engine::new(&s, &paths()).apply(&profiles_socks()).unwrap();
        assert!(a.restarted);
        assert!(s.called("systemctl start bui-c.service"), "{:?}", s.calls());
        assert!(!s.called("systemctl enable --now bui-c.service"));
        assert!(!s.called("systemctl enable bui-c.service"));
    }

    #[test]
    fn apply_tun_preps_kernel_and_waits_for_interface() {
        let s = FakeSys::new();
        clean_and_starts(&s);
        s.reply("ip link show bui-tun", 0, "5: bui-tun: <POINTOPOINT,UP>");
        let a = Engine::new(&s, &paths()).apply(&profiles_tun()).unwrap();
        assert_eq!(a.tun_ready, Some(true));
        let cfg: serde_json::Value =
            serde_json::from_str(&s.get("/opt/bui-c/config.json").unwrap()).unwrap();
        assert_eq!(cfg["inbounds"][0]["type"], "tun");
        assert_eq!(cfg["inbounds"][0]["interface_name"], "bui-tun");
        assert!(s.called("ip link delete bui-tun"), "先清残留接口");
        assert!(s.called("sysctl -w net.ipv4.conf.all.rp_filter=2"));
        assert!(s.called("sysctl -w net.ipv4.conf.default.rp_filter=2"));
        assert!(s.called("sysctl -w net.ipv4.ip_forward=1"));
    }

    #[test]
    fn apply_tun_reports_not_ready_when_interface_never_appears() {
        let s = FakeSys::new();
        clean(&s);
        s.reply("ip link show bui-tun", 1, "");
        let a = Engine::new(&s, &paths()).apply(&profiles_tun()).unwrap();
        assert_eq!(a.tun_ready, Some(false));
        assert_eq!(
            s.sleeps().len(),
            APPLY_POLL_STEPS as usize,
            "轮询 10 × 500ms，不用固定 sleep 2"
        );
    }

    #[test]
    fn tun_apply_second_time_skips_prep_and_reports_ready() {
        let s = FakeSys::new();
        s.put("/opt/bui-c/bin/sing-box", "ELF");
        s.reply("systemctl is-active --quiet bui-c.service", 0, "");
        s.reply("systemctl is-enabled --quiet bui-c.service", 0, "");
        s.reply("systemctl is-active --quiet bui-c.timer", 0, "");
        s.reply("systemctl is-enabled --quiet bui-c.timer", 0, "");
        s.reply("ip link show bui-tun", 0, "5: bui-tun");
        let prof = profiles_tun();
        let pt = paths();
        let e = Engine::new(&s, &pt);
        e.apply(&prof).unwrap(); // 第一次：写盘 + 重启 + 轮询
        let sleeps_after_first = s.sleeps().len();
        let a = e.apply(&prof).unwrap(); // 第二次：什么都没变
        assert_eq!(
            a,
            Applied {
                config_changed: false,
                units_changed: false,
                restarted: false,
                tun_ready: Some(true)
            }
        );
        // 什么都不改时不去动活着的 TUN：`ip link delete` 对活的 TUN fd 是 EOPNOTSUPP，白刷日志
        let deletes = s
            .calls()
            .iter()
            .filter(|c| *c == "ip link delete bui-tun")
            .count();
        assert_eq!(deletes, 1, "只有第一次（要重启单元）才清残留接口");
        assert_eq!(
            s.sleeps().len(),
            sleeps_after_first,
            "没重启就不轮询，直接读接口现状"
        );
    }

    #[test]
    fn verify_failure_leaves_config_untouched() {
        let s = FakeSys::new();
        clean(&s);
        s.put("/opt/bui-c/config.json", "{\"old\":true}");
        s.reply(
            "/opt/bui-c/bin/sing-box check -c /opt/bui-c/.config.json.new",
            1,
            "",
        );
        let e = Engine::new(&s, &paths())
            .apply(&profiles_socks())
            .unwrap_err();
        assert!(matches!(e, crate::Error::Verify(_)), "{e}");
        assert_eq!(s.get("/opt/bui-c/config.json").unwrap(), "{\"old\":true}");
        assert!(
            !s.exists(std::path::Path::new("/opt/bui-c/.config.json.new")),
            "校验用的临时文件要删掉"
        );
        assert!(!s.called("systemctl restart bui-c.service"));
    }

    #[test]
    fn apply_without_kernel_is_an_actionable_error() {
        let s = FakeSys::new();
        let e = Engine::new(&s, &paths())
            .apply(&profiles_socks())
            .unwrap_err();
        assert!(
            e.to_string().contains("bui-c update"),
            "要告诉用户怎么装内核：{e}"
        );
    }

    #[test]
    fn apply_without_active_profile_errors() {
        let s = FakeSys::new();
        clean(&s);
        let mut prof = profiles_socks();
        prof.active = None;
        let e = Engine::new(&s, &paths()).apply(&prof).unwrap_err();
        assert!(e.to_string().contains("没有激活的节点"), "{e}");
    }

    #[test]
    fn host_has_ipv6_needs_all_four_conditions() {
        let s = FakeSys::new();
        s.put(
            "/proc/net/if_inet6",
            "00000000000000000000000000000001 01 80 10 80 lo\n",
        );
        s.reply("sysctl -n net.ipv6.conf.all.disable_ipv6", 0, "0\n");
        s.reply("sysctl -n net.ipv6.conf.default.disable_ipv6", 0, "0\n");
        s.reply(
            "ip -6 route show default",
            0,
            "default via fe80::1 dev eth0 metric 1024\n",
        );
        s.reply(
            "ip -6 addr show dev eth0 scope global",
            0,
            "    inet6 2001:db8::1/64 scope global\n",
        );
        let pt = paths();
        let e = Engine::new(&s, &pt);
        assert!(e.host_has_ipv6());
        // default.disable_ipv6=1 → 新建 TUN 接口配不上 v6 地址（sing-tun 直接放弃整个 TUN）
        s.reply("sysctl -n net.ipv6.conf.default.disable_ipv6", 0, "1\n");
        assert!(!e.host_has_ipv6());
        s.reply("sysctl -n net.ipv6.conf.default.disable_ipv6", 0, "0\n");
        // 只有 ULA（Docker 网桥那种）不算
        s.reply(
            "ip -6 addr show dev eth0 scope global",
            0,
            "    inet6 fd00::1/64 scope global\n",
        );
        assert!(!e.host_has_ipv6());
        // 没有 v6 默认路由不算
        s.reply(
            "ip -6 addr show dev eth0 scope global",
            0,
            "    inet6 2001:db8::1/64 scope global\n",
        );
        s.reply("ip -6 route show default", 0, "\n");
        assert!(!e.host_has_ipv6());
    }

    #[test]
    fn host_has_ipv6_finds_a_gua_listed_after_a_ula() {
        // ipv6 spec §3.2 第四条是「有任一 2000::/3 地址」，不是「第一个地址是 GUA」。
        // 只看第一个 inet6 的写法在 ULA 排前面时会误判成「没有 v6」。
        let s = FakeSys::new();
        s.put(
            "/proc/net/if_inet6",
            "00000000000000000000000000000001 01 80 10 80 lo\n",
        );
        s.reply("sysctl -n net.ipv6.conf.all.disable_ipv6", 0, "0\n");
        s.reply("sysctl -n net.ipv6.conf.default.disable_ipv6", 0, "0\n");
        s.reply(
            "ip -6 route show default",
            0,
            "default via fe80::1 dev eth0 metric 1024\n",
        );
        s.reply(
            "ip -6 addr show dev eth0 scope global",
            0,
            "    inet6 fd00::1/64 scope global\n       valid_lft forever\n    inet6 2001:db8::1/64 scope global\n",
        );
        assert!(Engine::new(&s, &paths()).host_has_ipv6());
        // 两个都是 ULA 才算没有
        s.reply(
            "ip -6 addr show dev eth0 scope global",
            0,
            "    inet6 fd00::1/64 scope global\n    inet6 fc00::2/64 scope global\n",
        );
        assert!(!Engine::new(&s, &paths()).host_has_ipv6());
    }

    #[test]
    fn host_has_ipv6_without_kernel_stack_is_false() {
        let s = FakeSys::new();
        assert!(
            !Engine::new(&s, &paths()).host_has_ipv6(),
            "/proc/net/if_inet6 不存在 = ipv6.disable=1"
        );
    }

    #[test]
    fn tun_default_route_reads_all_tables() {
        let s = FakeSys::new();
        s.reply(
            "ip -4 route show table all",
            0,
            "default dev bui-tun table 2022 scope link\nlocal 127.0.0.1 dev lo\n",
        );
        assert!(Engine::new(&s, &paths()).tun_default_route());
        s.reply(
            "ip -4 route show table all",
            0,
            "default via 203.0.113.1 dev eth0\n",
        );
        assert!(!Engine::new(&s, &paths()).tun_default_route());
    }

    #[test]
    fn render_maps_mode_and_ports() {
        let s = FakeSys::new();
        let pt = paths();
        let e = Engine::new(&s, &pt);
        let prof = profiles_tun();
        let cfg = e.render(&prof, prof.active_profile().unwrap()).unwrap();
        // FakeSys 没有 /proc/net/if_inet6 → host_has_ipv6 = false → TUN 只有 v4 地址
        let addrs = cfg["inbounds"][0]["address"].as_array().unwrap();
        assert!(addrs.iter().all(|a| !a.as_str().unwrap().contains(':')));
        let mut socks = profiles_socks();
        socks.socks_port = 11080;
        socks.http_port = 18080;
        let cfg = e.render(&socks, socks.active_profile().unwrap()).unwrap();
        assert_eq!(cfg["inbounds"][0]["listen_port"], 11080);
        assert_eq!(cfg["inbounds"][1]["listen_port"], 18080);
    }

    #[test]
    fn stop_stops_unit_and_drops_interface() {
        let s = FakeSys::new();
        Engine::new(&s, &paths()).stop().unwrap();
        assert!(s.called("systemctl stop bui-c.service"));
        assert!(s.called("ip link delete bui-tun"));
    }

    #[test]
    fn force_ipv6_env_overrides_probe() {
        // 环境变量走 Sys::env 注入：不碰进程环境，本文件里「TUN 无 v6 地址」那条测试
        // 与这条可以安全并发（决策 11）
        let s = FakeSys::new();
        s.set_env("BUI_FORCE_IPV6", "1");
        assert!(Engine::new(&s, &paths()).host_has_ipv6());
        s.set_env("BUI_FORCE_IPV6", "0");
        assert!(!Engine::new(&s, &paths()).host_has_ipv6());
        // 其它取值当没设，走真实探测（FakeSys 没有 /proc/net/if_inet6 → false）
        s.set_env("BUI_FORCE_IPV6", "yes");
        assert!(!Engine::new(&s, &paths()).host_has_ipv6());
    }
}
