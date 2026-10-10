//! `hy2-residential.json` 过**官方** sing-box（= `scripts/release/kernels.lock` 里
//! `sing-box target` 那一版，2026-10-10 起是官方发行归档）的 check。
//!
//! 4.x 这里按「自建 `with_v2ray_api` 二进制」判 skip：官方发行物不含那个编译标签，而渲染器
//! 当时还产 `experimental.v2ray_api`，对官方内核必然
//! `FATAL create v2ray-server: v2ray api is not included in this build, rebuild with
//! -tags with_v2ray_api`。渲染器已把那段换成官方内置的 `services: [{ "type": "api", … }]`
//! （见 `render::hy2_singbox` 模块文档），所以这两格对官方产物**必须真跑**——再退回
//! 「自建不可用 ⇒ 静默 skip」会让「住宅内核起不来」正好发生在绿 CI 底下。
mod common;

/// 自签一张证书：`check` 会真读 `tls.certificate_path`（缺文件即
/// `FATAL initialize inbound[0]: read certificate: open …: no such file or directory`），
/// 所以证书目录只能是临时目录 —— CI 与开发机上都没有 `/opt/b-ui/certs`。
/// 与 `kernel_hysteria.rs::self_signed` 同款；本机没有 `openssl` 就跳过。
///
/// **CI 里不许静默跳过**（2026-10-10 计划 §8 #4）：这两个用例是「渲染器产的住宅配置真过
/// 官方内核 `check`」的唯一自动化面，而 Linux runner 自带 openssl —— 它不可用意味着这两格
/// 会在绿 CI 底下假绿，所以照 `relay_generation_kernel.rs` 的 `assert!(CI.is_none())` 范式
/// 硬失败（本机仍只是 `skipped: openssl not usable`）。
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
        .output();
    let ok = matches!(&out, Ok(out) if out.status.success());
    if !ok {
        assert!(
            std::env::var_os("CI").is_none(),
            "kernel_hy2_singbox requires a usable openssl under CI"
        );
        eprintln!("skipped: openssl not usable");
    }
    ok.then_some(())
}

#[test]
fn residential_inbound_config_passes_check_on_the_stock_singbox() {
    let dir = tempfile::tempdir().unwrap();
    if self_signed(dir.path()).is_none() {
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
/// **空 `route.rules`（住宅 HY2 侧不 sniff，见 `render::hy2_singbox` 模块文档）
/// 也必须过 check**（2026-09-16 第二波复核：T5 只手工验过，没有自动化测试守着）。
/// 空 `users` 或空 `rules` 数组被 sing-box 判非法的话，首装当轮住宅内核就起不来。
#[test]
fn an_empty_credential_pool_also_passes_check() {
    let dir = tempfile::tempdir().unwrap();
    if self_signed(dir.path()).is_none() {
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
