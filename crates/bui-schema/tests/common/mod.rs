//! fixture 加载与内核校验辅助（Task 6–10 的测试共用，按需取用）。
#![allow(dead_code)]

use bui_schema::model::*;
use std::path::PathBuf;

/// v3 fixture 根目录（`tests/fixtures/v3`）。
pub fn fx() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/v3"))
}

/// 导入 fixture 里的 v3 状态，并按 golden 模式（global / split / obfs）调整。
pub fn state(mode: &str) -> State {
    let mut s = bui_schema::v3::import(&fx().join("src")).unwrap().state;
    let g = s.residential.groups.get_mut("default").unwrap();
    match mode {
        "global" | "obfs" => g.mode = ResiMode::Global,
        "split" => g.mode = ResiMode::Split,
        _ => unreachable!(),
    }
    if mode == "obfs" {
        s.node.obfs = Obfs {
            enabled: true,
            password: "obfs-pw-test".into(),
        };
    }
    s
}

/// 读取 v3 生成的 golden 输出（`kind` 形如 `sub.txt` / `singbox.json` / `clash.yaml`）。
pub fn expected(mode: &str, user: &str, kind: &str) -> String {
    std::fs::read_to_string(
        fx().join("expected")
            .join(mode)
            .join(format!("{user}.{kind}")),
    )
    .unwrap()
}

/// 内核二进制是否可用（不可用时测试打印 skipped 而非失败；**CI 里硬失败**）。
///
/// 这是「内核不在了就静默变绿」的唯一收口点：`kernel_xray` / `kernel_hysteria` 的直接
/// 调用、以及 `check_singbox`（`kernel_client` / `kernel_relay` / `golden_subscription` /
/// `kernel_hy2_singbox`）都经它。三个发布内核由 `.github/actions/setup-kernels` 放进 PATH
/// （`ci.yml` 的 `test (sing-box 1.14.3)`、`release.yml`、`kernels-bump.yml` 三处都装），
/// 所以在 CI 里 `have()` 为假只可能是内核没到位这类环境故障，静默 skip 会让全部「渲染
/// 结果过真实内核」的用例假绿。断言写在这里而不是各调用点：外部 grep 黑名单那套转录会
/// 漂移（`kernel_hysteria` 的 openssl skip 就漏在三个 workflow 的黑名单之外），把口子
/// 收在生产者侧；范式照 `kernel_hy2_singbox.rs` / `relay_generation_kernel.rs` 的
/// `assert!(CI.is_none())`。**要新增一个「CI 里也接受缺内核」的用例，请另写显式的
/// opt-in 函数，别绕过这里。**
pub fn have(bin: &str) -> bool {
    let ok = std::process::Command::new(bin)
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        assert!(
            std::env::var_os("CI").is_none(),
            "kernel validation must not skip in CI: `{bin}` is not on PATH"
        );
    }
    ok
}

/// 用真实 sing-box 校验一份配置；二进制缺失时跳过。
pub fn check_singbox(cfg: &serde_json::Value) {
    check_singbox_with("sing-box", cfg);
}

fn check_singbox_with(bin: &str, cfg: &serde_json::Value) {
    if !have(bin) {
        eprintln!("skipped: {bin} not found");
        return;
    }
    let f = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(f.path(), serde_json::to_vec_pretty(cfg).unwrap()).unwrap();
    let out = std::process::Command::new(bin)
        .args(["check", "-c"])
        .arg(f.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{bin}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
