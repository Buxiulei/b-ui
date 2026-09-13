# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

B-UI is a Hysteria2 + Xray (VLESS-REALITY) proxy server with a built-in Web admin panel, plus a Linux client. Since v4.0.0 the control plane and the client are **Rust single binaries** (`bui` / `bui-c`); the protocol kernels (Xray / hysteria / sing-box / Caddy) are unchanged upstream builds. It targets Ubuntu/Debian/CentOS/RHEL VPS hosts, amd64 or arm64, systemd. Users are in mainland China; Windows users consume subscriptions with v2rayN, Linux users run `bui-c`. The project, its UI and its commit messages are in Chinese.

v4.0.0 删掉了 v3 的 shell / Node 实现（`server/` 下的五个脚本、Node 面板、根目录那两个 v3 安装与客户端脚本、旧的版本清单 JSON）。它们还在 git 历史里，`crates/` 里那些「移植参照 …:<行号>」注释指的就是历史里的那份代码；要看原文用 `git log --diff-filter=D -1 -- <路径>` 找到删除提交，再 `git show <sha>^:<路径>`。

## Architecture

Cargo workspace，三个 crate + 保留的前端三文件：

1. **`crates/bui-schema`** — 唯一知道端口、标签、规则与格式的地方：期望态模型（`State`）、权益 → 节点集合（`nodes::nodes_for`）、四种内核配置渲染（hysteria ×2 / xray / sing-box relay）、三种订阅渲染、客户端配置渲染、上游 URL 与节点 URI 解析、v3 状态导入。改端口/标签/参数只改这里。公共 API 是总纲 C1 契约，`src/lib.rs` 的模块文档是它的清单。
2. **`crates/bui`** — 服务端单二进制：`install / upgrade / serve / reconcile / status / import-v3 / set / auth-hook / residential / menu / harden-ssh`。期望态存 `/opt/b-ui/state.json`，对账器把文件/单元/sysctl/防火墙/二进制拉到期望态（非受管项只报不改）；面板 API + 内嵌前端 + 住宅巡检 + 黑名单 + 证书监听 + watchdog + 升级都在这一个进程里，**没有 cron、没有独立 timer、没有改配置的 shell 脚本**。
3. **`crates/bui-c`** — Linux 客户端：单引擎（sing-box ≤ 1.14）的 SOCKS/TUN，`bui-c.service` + `bui-c.timer`（每分钟 `bui-c check`）。
4. **`web/{index.html,app.js,style.css}`** — 保留的 vanilla SPA，由 `bui` 以 `rust-embed` 内嵌（`modules/panel/assets.rs`）。

引导：`install.sh`（有效代码 ≤150 行，`scripts/tests/test-install.sh` 守门）识别架构 → 从 GitHub Releases（失败则 `BUI_MIRRORS` 的镜像前缀）下载 `manifest.json` 与 `bui` → 校验 sha256 → `exec bui install [--import-v3]`。
发布：`scripts/release/`（`kernel-versions.env` 版本轨道 → `kernels.lock` → `manifest.json`）+ `.github/workflows/{ci,release}.yml`；运维脚本在 `scripts/ops/`，纯 bash 测试在 `scripts/tests/`。详见 `docs/superpowers/plans/2026-09-11-v4-p5-release.md`。

### Server topology

一台直连 Hysteria2 + 每个住宅槽位一台 Hysteria2 + 两个 Xray 入站，全部由 `bui-schema` 的渲染器产出、由对账器写盘（`i` = 槽序号）：

| Path | Listener | Egress |
|---|---|---|
| `hysteria-server` (`config.yaml`) | `:PORT` (+ built-in port hopping range in the same `listen:` line) | built-in direct, `mode: 4` (IPv4-only) |
| `hysteria-residential[-<i>]` (`config-residential[-<i>].yaml`) | `:(40000+i),<41000-50000 按槽位空间等分的第 i 片>` | `outbounds: relay` → `socks5 127.0.0.1:(2080+i)` via `acl: relay(all)` |
| xray `vless-direct` (`xray-config.json`) | `:10001` REALITY | `freedom` with `domainStrategy: ForceIPv4` |
| xray `vless-residential` | `:10002` REALITY | `socks 127.0.0.1:(2080+i)`，按用户 email 路由到槽（每人一条 `ruleTag` 规则，增删走 `RoutingService` gRPC，不重启 xray） |

`127.0.0.1:2080` 是常驻的本地 sing-box 中继（`b-ui-relay.service`，配置 `/opt/b-ui/singbox-relay.json`，由 `render::relay::config` 渲染）：住宅上游池非空时把 AI 域名关键字（global 模式则全部）送进 `resi-pool`，池空则全部直连（fail-open）。目标域名不在本机解析，原样交给上游。上游可以是 `socks5` 或 `http`（`model::UpstreamKind`，粘贴 `socks5://u:p@h:port`、`http://…`、`h:port:u:p`、`u:p@h:port` 四种写法都由 `parse::upstream_url` 归一）；Bright Data 用 HTTP 端口（44445），它的 SOCKS5 端口拒绝明文 HTTP 目标（2026-09-10 实测）。池状态、凭据、黑名单与选中的上游都在 `state.json` 的 `residential` 里（600），没有独立的 `residential-proxy.json`；体检与自动黑名单是守护进程里的任务，切换上游经中继的 Clash API 而不重启内核。

池里每个 IP 是一个槽位（`state.residential.slots`，spec §5.6）：每槽一个中继入站 `2080+i`、一个 `hysteria-residential[-<i>]` 实例 `40000+i`，Xray 住宅入站按用户 email 路由到槽；用户经 `entitlements.residential.slot_id` 粘在一个 IP 上（多个用户可以共用一个 IP），端口换算只在 `bui_schema::slots` 一处。新建用户分到负载最少的槽，`bui residential rebalance` / `assign` 手动调整，`bui residential slots` 与面板按槽展示。

受管单元 = `reconcile::MANAGED_UNITS` 六个固定名字（`b-ui`、`hysteria-server`、`hysteria-residential`、`xray`、`b-ui-relay`、`caddy`）+ 每个住宅槽位一个 `hysteria-residential-<i>`；枚举一律经 `reconcile::managed_units(&state)`，词法判定用 `reconcile::is_managed_unit`。v3 的九个遗留单元与定时器列在 `reconcile::LEGACY_UNITS`，对账器只负责把它们停掉、删掉。

VPS 没有 IPv6 出口：服务端每条出口都钉在 IPv4，客户端配置接管 IPv6 并拒绝裸 IPv6 目标，让应用回落 IPv4（spec：`docs/superpowers/specs/2026-09-10-ipv6-takeover-design.md`）。

### Subscriptions (`crates/bui-schema/src/render/subscription.rs`)

三个免鉴权端点，节点集合都来自 `nodes::nodes_for`（fusion = Reality直连 :10001 / Reality住宅 :10002 / HY2直连 / HY2住宅 `:(40000+用户槽位)`）：
- `/api/sub/<user>` — base64 `vless://`/`hysteria2://` URIs (what v2rayN uses)。端口跳跃（`mport=`）来自期望态的 `ports.hy2_hop` / `hy2_resi_hop`。
- `/api/subscription/<user>` — a complete sing-box config (TUN + DNS + route). Must stay valid for **sing-box 1.12 through 1.14**: typed DNS servers, TUN `address` array, rule actions (`sniff`/`hijack-dns`/`reject`), `route.default_domain_resolver`, no `rule_set`/`download_detour` (1.13 and 1.15 disagree on those fields).
- `/api/clash/<user>` — mihomo YAML.

节点集合只有 `nodes::nodes_for` 一处实现，三种订阅与客户端渲染都从它取；改端口/标签/obfs 只需改 `bui-schema`。

### 没有迁移块

v4 用对账器取代 v3 的 25 个迁移块：新行为写进渲染器与对账模块，升级时 `reconcile()` 自然把现存安装拉到期望态。给现有安装打补丁不需要写迁移代码。

## Development Commands

```bash
# Rust 侧
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                     # 内核校验用例在 PATH 上找 sing-box / xray / hysteria，缺则 skip

# Shell 侧（install.sh 与 scripts/；git 的 ** 要求中间至少一层目录，所以过滤后缀而不是写 pathspec 通配）
bash -n install.sh $(git ls-files -- scripts | grep -E '\.sh$')
shellcheck -S error install.sh $(git ls-files -- scripts | grep -E '\.sh$')
bash scripts/tests/run-all.sh

# 发布元数据
bash scripts/release/pin-kernels.sh --check          # 内核 lock 是否还贴着版本轨道
bash scripts/release/check-version.sh v4.0.0         # tag / workspace version / CHANGELOG 一致

# 用真实内核校验渲染结果（本机 1.13.19；CI 跑 1.12/1.13/1.14）
sing-box check -c <渲染出的 json>
xray run -test -c <渲染出的 json>
```

## Important Conventions

- 版本号唯一来源是根 `Cargo.toml` 的 `[workspace.package] version`；发布 tag = `v<version>`（预发布 `v<version>-rcN`），同时在 `CHANGELOG.md` 加一段，`scripts/release/check-version.sh` 在 CI 卡一致性。commit 前缀：`fix(scope):` / `feat(scope):` / `chore(scope):` / `docs(scope):` / `test(scope):`。
- 内核版本由 `scripts/release/kernel-versions.env`（轨道）+ `kernels.lock`（上游归档的精确版本与 sha256）决定；`manifest.json`（总纲 C4：`kernels` 版本表 + 扁平 `artifacts`，每项是裸二进制的 `url` + `sha256`）把它们带给安装与升级，上游归档由 Actions 解包后以裸二进制资产重传。Xray 的 `/releases/latest` 不可用（全部 tag 标 prerelease），轨道解析走 `git ls-remote --tags`。
- 客户端配置模板改动后无需版本常量：`bui-c` 每次 apply 都重渲染 `config.json` 并按内容比对（`engine::write_if_changed`）决定是否重写 + 重启，不再有 `TUN_SCHEMA_VERSION`。
- Rust 输出与错误：面板/CLI 的用户可见文字是中文；`bui` 的日志经 `logging.rs`，凭据一律经 `redact.rs` 脱敏后才落日志。Shell 脚本的输出 helper 是 `print_info`/`print_success`/`print_warning`/`print_error`；`scripts/` 下的脚本都带 `#!/usr/bin/env bash` + `LC_ALL=C`，采样/压测循环用 `set -uo pipefail`（不用 `set -e`），其余用 `set -euo pipefail`。
- 服务器上的文件：`state.json`（期望态，600）、`runtime.json` / `auth-snapshot.json`（运行期，600）、`manifest.json`（升级缓存）、渲染产物 `config.yaml` / `config-residential.yaml` / `xray-config.json` / `singbox-relay.json` / `Caddyfile`，二进制在 `bin/`，备份在 `state.backups/`；CLI ↔ 守护进程走 `/run/b-ui.sock`。Systemd 单元见 `reconcile::MANAGED_UNITS`。
- Never put credentials on a command line (`ps` leaks them): 面板密码走 `--admin-password-stdin`，住宅上游走 `bui residential add -`（stdin），curl 代理凭据用 `-K -`。
- `scripts/tests/` 里的测试禁止访问网络：需要 curl / systemctl / bui 的地方一律用 PATH 前置的 stub。
- Design docs live in `docs/superpowers/specs/YYYY-MM-DD-<slug>-design.md` with matching plans in `docs/superpowers/plans/`；v4 的总纲是 `docs/superpowers/plans/2026-09-11-v4-master.md`（C1–C5 契约），架构与验收在 `docs/superpowers/specs/2026-09-11-v4-architecture-design.md`，删除 v3 的依据在 `docs/superpowers/audits/2026-09-11-architecture-audit.md` §7.1。2026-09-10 那一套记录 v3 末期的 IPv6、住宅与重启硬化决定，读它们时注意描述的是 v3 实现。
