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

/// 内核二进制是否可用（不可用时测试打印 skipped 而非失败）。
pub fn have(bin: &str) -> bool {
    std::process::Command::new(bin)
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 用真实 sing-box 校验一份配置；二进制缺失时跳过。
pub fn check_singbox(cfg: &serde_json::Value) {
    if !have("sing-box") {
        eprintln!("skipped: sing-box not found");
        return;
    }
    let f = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(f.path(), serde_json::to_vec_pretty(cfg).unwrap()).unwrap();
    let out = std::process::Command::new("sing-box")
        .args(["check", "-c"])
        .arg(f.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// v3 单协议 + residential=true 只有住宅版；v4 多给直连版。golden 比对只看 v3 有的节点。
pub fn v3_nodes_only(
    nodes: Vec<bui_schema::nodes::Node>,
    user: &User,
) -> Vec<bui_schema::nodes::Node> {
    let single = user.entitlements.protocols.len() == 1 && user.entitlements.residential.is_some();
    if single {
        nodes
            .into_iter()
            .filter(|n| {
                matches!(
                    n.kind,
                    bui_schema::nodes::NodeKind::RealityResidential
                        | bui_schema::nodes::NodeKind::Hy2Residential
                )
            })
            .collect()
    } else {
        nodes
    }
}
