//! `hy2-residential.json` 过**自建** sing-box（≥ 1.14，带 with_v2ray_api）的 check。
//! 官方二进制不含 with_v2ray_api，`experimental.v2ray_api` 段在它上面必然 FATAL ⇒ 那种情况 skip
//! 而不是失败（spec §2.3「哪些还没验」+ §5.4 第 9 条）。
//!
//! 本机 1.14.0（官方二进制）实测，逐字：
//! `FATAL create v2ray-server: v2ray api is not included in this build, rebuild with -tags with_v2ray_api`
//! ——把 `experimental.v2ray_api` 摘掉后同一份配置 `check` 退 0，所以除那一段之外的形状
//! （凭据池 / deny socks / 8 个槽出站 / 门 / `{"action":"sniff"}` + `auth_user` 规则 /
//! `route.final` / `clash_api` / salamander / masquerade proxy）已在真实 1.14 上验过。
mod common;

fn singbox_has_v2ray_api() -> bool {
    std::process::Command::new("sing-box")
        .arg("version")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("with_v2ray_api"))
        .unwrap_or(false)
}

/// 自签一张证书：`check` 会真读 `tls.certificate_path`（缺文件即
/// `FATAL initialize inbound[0]: read certificate: open …: no such file or directory`），
/// 所以证书目录只能是临时目录 —— CI 与开发机上都没有 `/opt/b-ui/certs`。
/// 与 `kernel_hysteria.rs::self_signed` 同款；没有 `openssl` 就跳过。
fn self_signed(dir: &std::path::Path) -> Option<()> {
    let out = std::process::Command::new("openssl")
        .args([
            "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
        ])
        .arg("-keyout")
        .arg(dir.join("privkey.pem"))
        .arg("-out")
        .arg(dir.join("fullchain.pem"))
        .args(["-subj", "/CN=test.invalid"])
        .output()
        .ok()?;
    out.status.success().then_some(())
}

#[test]
fn residential_inbound_config_passes_check_on_the_self_built_singbox() {
    if !singbox_has_v2ray_api() {
        eprintln!("skipped: 自建 sing-box（with_v2ray_api）不可用");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    if self_signed(dir.path()).is_none() {
        eprintln!("skipped: openssl not usable");
        return;
    }
    let mut s = common::state("obfs"); // 带 obfs 的那份 fixture，覆盖 salamander 分支
    bui_schema::hy2pool::migrate(&mut s, time::OffsetDateTime::now_utc());
    let cfg = bui_schema::render::hy2_singbox::config(
        &s.node,
        &bui_schema::paths::Paths {
            base_dir: dir.path().to_path_buf(),
            certs_dir: dir.path().to_path_buf(),
            bin_dir: dir.path().join("bin"),
        },
        &s.residential.hy2_pool,
    );
    common::check_singbox(&cfg);
}

/// 全新装机那一格：`hy2pool::migrate` 之前池是空的，此时渲染出的 `users: []`、9 个出站、
/// 1 条 sniff 规则**也必须过 check**（2026-09-16 第二波复核：T5 只手工验过，没有自动化
/// 测试守着）。空 `users` 数组被 sing-box 判非法的话，首装当轮住宅内核就起不来。
#[test]
fn an_empty_credential_pool_also_passes_check() {
    if !singbox_has_v2ray_api() {
        eprintln!("skipped: 自建 sing-box（with_v2ray_api）不可用");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    if self_signed(dir.path()).is_none() {
        eprintln!("skipped: openssl not usable");
        return;
    }
    let s = common::state("obfs");
    let pool = bui_schema::model::Hy2Pool::default();
    assert!(pool.creds.is_empty());
    let cfg = bui_schema::render::hy2_singbox::config(
        &s.node,
        &bui_schema::paths::Paths {
            base_dir: dir.path().to_path_buf(),
            certs_dir: dir.path().to_path_buf(),
            bin_dir: dir.path().join("bin"),
        },
        &pool,
    );
    common::check_singbox(&cfg);
}
