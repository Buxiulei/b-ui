# AGENTS.md

本仓库的**权威**智能体指引是 [`CLAUDE.md`](CLAUDE.md)：架构、服务器拓扑、订阅渲染、
开发命令、提交与发布约定都在那一份里，**动手前先读它**。

本文件刻意**不复制**任何架构或命令细节，只做指针 —— 2026-09-08 那版复制了整套 v3 架构，
到 v4.0.0 删掉 `server/*.sh` 与 Node 面板之后就整份成了误导。指针不会走味。

**与 CLAUDE.md 冲突时，一律以 CLAUDE.md 为准。**

## 无论读没读 CLAUDE.md，都必须守住

- **中文**：项目、UI、commit message、文档一律中文（`docs/superpowers/` 里的设计与计划同理）。
- **凭据不上命令行**（`ps` 会泄漏）：面板密码走 `--admin-password-stdin`，住宅上游走
  `bui residential add -`（stdin），curl 带代理凭据用 `-K -`。
- **公开仓库不写真实域名 / IP / 用户名 / 供应商账号 / 订阅 token**（四个订阅端点免鉴权，
  路径末段本身就是凭据）：示例一律用 `example.com`、`203.0.113.0/24`、`alice` / `bob`，
  生产主机只用 SSH 别名（`bwg-tizi` / `bwg-rick`）指代。
- **版本号唯一来源**是根 `Cargo.toml` 的 `[workspace.package] version`；发布 tag = `v<version>`，
  且 `CHANGELOG.md` 必须有对应段落（`scripts/release/check-version.sh` 在 CI 里硬卡）。
- **提交前跑三闸**：
  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  ```
- **`scripts/tests/` 里的测试禁止访问网络**：需要 curl / systemctl / bui 的地方一律用 PATH 前置的 stub。
