//! `bui` 侧的路径 helper：全部从 [`Paths::base_dir`] 派生，没有第二处硬编码。
use bui_schema::paths::Paths;
use std::path::PathBuf;

/// CLI 与守护进程之间的 unix socket（0600，root）。
pub const SOCKET_PATH: &str = "/run/b-ui.sock";

/// CLI 入口符号链接（spec §2.4）：两个都指向 <base>/bin/bui。
pub const CLI_LINKS: [&str; 2] = ["/usr/local/bin/bui", "/usr/local/bin/b-ui"];

/// 期望态（`state.json`，0600）。
pub fn state_file(p: &Paths) -> PathBuf {
    p.base_dir.join("state.json")
}

/// 运行时数据（`runtime.json`，0600）。
pub fn runtime_file(p: &Paths) -> PathBuf {
    p.base_dir.join("runtime.json")
}

/// install / upgrade / 每日自检写下的 manifest 缓存，对账据它决定内核版本。
pub fn manifest_file(p: &Paths) -> PathBuf {
    p.base_dir.join("manifest.json")
}

/// state 的历史备份目录（保留 10 份）。
pub fn backups_dir(p: &Paths) -> PathBuf {
    p.base_dir.join("state.backups")
}

/// 渲染结果过内核校验时用的落地目录（FakeHost 下可断言）。
pub fn verify_dir(p: &Paths) -> PathBuf {
    p.base_dir.join(".verify")
}

/// v3 状态文件（含秘密）归档目录，0700；`uninstall_v3` 写，漂移扫描白名单里有它。
pub fn v3_backup_dir(p: &Paths) -> PathBuf {
    p.base_dir.join("v3-backup")
}

/// 传给 caddy 的 XDG_DATA_HOME 与 XDG_CONFIG_HOME（同一个目录）。
pub fn caddy_xdg(p: &Paths) -> PathBuf {
    p.base_dir.join("caddy")
}

/// Caddy 实际的数据目录（`$XDG_DATA_HOME/caddy`），ACME 账号与证书都在它下面。
pub fn caddy_data(p: &Paths) -> PathBuf {
    caddy_xdg(p).join("caddy")
}

/// Caddyfile：按总纲 C3 与 2026-09-12 裁决放 `<base>`，`caddy.service` 以 `--config` 指它。
pub fn caddyfile(p: &Paths) -> PathBuf {
    p.base_dir.join("Caddyfile")
}

/// 外部站点通道（2026-09-13 裁决）：Caddyfile 末尾 `import <这个目录>/*.caddy`。
/// 目录由对账器建出来（[`caddy_sites_readme`] 那个占位文件），里面的 `*.caddy` **归用户管**：
/// bui 不渲染、不覆盖、不删除，漂移扫描也不看（`caddy` 在 `BASE_WHITELIST` 里且不递归）。
pub fn caddy_sites(p: &Paths) -> PathBuf {
    caddy_xdg(p).join("sites")
}

/// 站点目录的占位说明文件：它同时承担「把目录建出来」的职责（写文件会建父目录），
/// 后缀不是 `.caddy` 所以不会被 import 进配置。
pub fn caddy_sites_readme(p: &Paths) -> PathBuf {
    caddy_sites(p).join("README.txt")
}

/// `import-v3` 把 `/etc/caddy/Caddyfile` 里非 b-ui 的站点块原样导到这里（0644）。
/// 写完就归用户管：之后的对账绝不再碰它。
pub fn caddy_sites_imported(p: &Paths) -> PathBuf {
    caddy_sites(p).join("imported-from-v3.caddy")
}

/// hysteria 鉴权钩子读的快照（`bui install` 写初版，P2 的用户模块接手后原子重写）。
pub fn auth_snapshot_file(p: &Paths) -> PathBuf {
    p.base_dir.join("auth-snapshot.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn derives_every_bui_path_from_base_dir() {
        let p = Paths::default_server();
        assert_eq!(state_file(&p), PathBuf::from("/opt/b-ui/state.json"));
        assert_eq!(runtime_file(&p), PathBuf::from("/opt/b-ui/runtime.json"));
        assert_eq!(manifest_file(&p), PathBuf::from("/opt/b-ui/manifest.json"));
        assert_eq!(backups_dir(&p), PathBuf::from("/opt/b-ui/state.backups"));
        assert_eq!(verify_dir(&p), PathBuf::from("/opt/b-ui/.verify"));
        assert_eq!(v3_backup_dir(&p), PathBuf::from("/opt/b-ui/v3-backup"));
        assert_eq!(caddy_xdg(&p), PathBuf::from("/opt/b-ui/caddy"));
        assert_eq!(caddy_data(&p), PathBuf::from("/opt/b-ui/caddy/caddy"));
        assert_eq!(caddyfile(&p), PathBuf::from("/opt/b-ui/Caddyfile"));
        assert_eq!(caddy_sites(&p), PathBuf::from("/opt/b-ui/caddy/sites"));
        assert_eq!(
            caddy_sites_readme(&p),
            PathBuf::from("/opt/b-ui/caddy/sites/README.txt")
        );
        assert_eq!(
            caddy_sites_imported(&p),
            PathBuf::from("/opt/b-ui/caddy/sites/imported-from-v3.caddy")
        );
        assert_eq!(
            auth_snapshot_file(&p),
            PathBuf::from("/opt/b-ui/auth-snapshot.json")
        );
        assert_eq!(SOCKET_PATH, "/run/b-ui.sock");
        assert_eq!(CLI_LINKS, ["/usr/local/bin/bui", "/usr/local/bin/b-ui"]);
    }
}
