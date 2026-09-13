# bui-c（Linux 客户端）交接手册

> 2026-09-13。给接手 **v4 Linux 客户端 `bui-c`** 的 agent / 工程师。服务端（`crates/bui`）与发布链路由原会话继续负责，两边并行时的文件边界见 §8。
> 生产主机一律用别名：`bwg-rick`（已切 v4）、`bwg-tizi`（仍是 v3.6.3）、`baiyi`（国内 Linux 客户端机，本手册的真机）。公开文档不写真实 IP / 域名 / 凭据。

## 1. 现状一句话

- 代码：`crates/bui-c`（约 6.3k 行 Rust，19 个文件），随 workspace 一起发版；当前 tag `v4.0.0-rc6`（`main` 与 `v4` 同步）。
- 服务端已能分发客户端：`https://<面板域名>/packages/bui-c-install.sh`、`/packages/bui-c-linux-{amd64,arm64}`、`/packages/manifest.json`（总纲 C4 形状），GitHub Releases 的预发布也带同样的三件。
- **baiyi 上还没有完成 v4 切换**：2026-09-12 首次真机 `import-v3` 失败后已把 v3 客户端原样恢复（`bui-tun.service` 在跑、出口正常、IPv6 被拦截）。v4 二进制 `bui-c 4.0.0` 留在 `/usr/local/bin/bui-c`；v3 脚本备份在 `/usr/local/bin/bui-c.v3` 与 `/root/bui-c-v3-backup/`（含 `/opt/hysteria-client` 与四个旧单元的 tar）；`/opt/bui-c/` 可能残留一份只含 1 个节点的 `profiles.json`。
- M4 里程碑（spec §9）**未验收**：四节点 SOCKS/TUN 均通；裸 IPv6 回落符合 spec；杀 sing-box 一分钟内自愈；从 v3 客户端原地升级不丢节点。

## 2. 架构速览

| 项 | 事实 |
|---|---|
| 单引擎 | 只跑 sing-box（≤ 1.14）；不再有 hysteria/xray 客户端进程。sing-box 配置**不由本 crate 生成**，全部来自 `bui-schema::render::client::{mixed_config, tun_config}`（与服务端订阅同一套渲染），`engine.rs` 只负责组装 `ClientOpts`、`sing-box check` 校验、原子落盘、拉起进程。 |
| 布局 | `/usr/local/bin/bui-c`（静态二进制）；`/opt/bui-c/profiles.json`（0600，用户意图：多节点、活动节点、模式、SOCKS/HTTP 端口、auto_update、面板信息）；`/opt/bui-c/runtime.json`（0600，运行状态：失败连击、上次重启/自更新、ufw 标记；丢了可重建）；`/opt/bui-c/bin/sing-box`；sing-box 配置落在 `/opt/bui-c/`。 |
| systemd | `bui-c.service`（sing-box 常驻）+ `bui-c-check.service`（oneshot 巡检）+ `bui-c.timer`（每分钟激活 check）。三个都要写，只写两个会让 timer 去重启 sing-box 本身（`units.rs` 注释有说明）。 |
| 模式 | `socks`（mixed 入站：SOCKS5 :1080 + HTTP :8080）/ `tun`（TUN 接管 IPv4+IPv6，`strict_route`，裸 IPv6 拒绝回落；主机没有 IPv6 时自动不配 v6，见 `engine.rs` 对 `/proc/net/if_inet6` 与 `inet6` 地址的判定）。 |
| UFW | 不再整墙关闭：TUN 期间 `ufw allow in on bui-tun` + `ufw route allow in on bui-tun`，停 TUN/卸载时撤回（`ufw.rs`，裁决记录 P4 第 4 条）。 |
| 巡检 `check` | 每分钟：探 `https://www.gstatic.com/generate_204`（8s 超时）；失败按连击退避重启 sing-box；每 23h 自更新一次（失败 1h 后重试）；状态写 `runtime.json`。 |
| 更新 `update` | 读 manifest（顺序：面板 `<base_url>/packages/manifest.json` → GitHub `releases/latest/download/manifest.json`），比 sha256 后替换自身与 sing-box。 |
| 节点来源 | `import <uri>|-`（stdin 读，凭据不进 argv）、`import --panel <url> --user <名>`（面板 `/api/nodes/<user>`，载荷 `NodesPayload{user,split,nodes}`）、`import --sub <url>`、`import-v3`（读 `/opt/hysteria-client/configs/*`，导入后卸载 v3 五个单元与残留）。 |
| 可测性 | 所有系统交互经 `Sys`（命令/文件）与 `Net`（HTTP）两个 trait；单元测试只用 `fake.rs` 的内存实现，不 `systemctl`、不写 `/etc`、不出网。真机行为留给 M4。 |
| CLI | `status / list / switch / mode / import / check / update / import-v3 / uninstall / menu`；全局 `--json`（status/list）、`-y/--yes`。 |

设计与计划：spec `docs/superpowers/specs/2026-09-11-v4-architecture-design.md` §6（客户端）、§3.4（单元资源）、§7（多源下载）；计划 `docs/superpowers/plans/2026-09-11-v4-p4-client.md`（13 任务，含 D1–D11 决策与 M4 清单）；裁决 `docs/superpowers/plans/2026-09-11-v4-master.md`「裁决记录」P4 各条。

## 3. baiyi 首次真机安装暴露的缺陷（按优先级）

1. **`import-v3` 先卸旧单元、后拿引擎**（最高）：卸掉 v3 四个单元后才去拉 manifest 装 sing-box，manifest 拿不到时 v4 单元没建，机器直接没代理。必须与服务端 `bui install` 同一原则：先把引擎与单元准备好并自检，最后才卸 v3；拿不到内核就中止且 v3 一字不动（服务端的「缺内核即中止」守卫是 `crates/bui/src/commands/install.rs` 里现成的样板）。
2. **manifest 来源在预发布期不可用**：从 v3 `server_address` 推导的面板是 v3 面板（没有 `/packages/manifest.json`），回退 GitHub `releases/latest` 又 404（只有预发布）。需要：`--panel` / 环境变量显式指定面板源；GitHub latest 404 时回退最新预发布（服务端 `crates/bui/src/kernels/mod.rs` 已实现同样的回退，照抄口径）。
3. **`import-v3` 只导入了 1 个节点**：四个 v3 目录里三个 `hysteria2-*` 打印「跳过」。查 `import_v3.rs::import` 对 `uri.txt`/`meta.json` 的解析与同 host:port 去重逻辑，M4 要求「原地升级不丢节点」。
4. **`scripts/bui-c-install.sh` 默认源是 GitHub latest**：从面板 `/packages/bui-c-install.sh` 拿到的脚本应默认用面板自己的 `/packages`（面板下发时替换占位符，v3 的 server.js 就是这么做的），并加 GitHub 预发布回退。绕过办法：`BUI_C_SOURCE=https://<面板域名>/packages bash <(curl -fsSL https://<面板域名>/packages/bui-c-install.sh)`。
5. 菜单「服务控制 / 连接检查」在单元不存在时报 `Unit bui-c.service not found`，应引导先装引擎（`bui-c update`）。

## 4. M4 验收清单与安全做法

清单（spec §9）：① 四节点（直连 REALITY / 住宅 REALITY / 直连 HY2 / 住宅 HY2）SOCKS 与 TUN 都通；② 裸 IPv6 回落：`curl -6 https://api64.ipify.org` 应失败或超时，`curl https://api64.ipify.org` 返回节点 IP，`ip -6 route` 默认路由在 TUN 上；③ `kill -9 sing-box` 后一分钟内 `bui-c.timer` 自愈；④ 从 v3 原地升级后 `bui-c list` 与 v3 `/opt/hysteria-client/configs/*` 数量一致，活动节点保持。

安全做法（血泪教训）：
- **baiyi 上跑着的其它会话（包括 Claude Code 自己）对 API 的访问依赖这条隧道**。切 TUN 前先确认本地恢复手段：`sudo tar xzf /root/bui-c-v3-backup/hysteria-client-*.tgz -C / && sudo systemctl daemon-reload && sudo systemctl enable --now bui-tun.service` 能在 30 秒内把 v3 拉回来。
- 先用 `socks` 模式验通四个节点，再切 `tun`。
- 长命令用 `nohup`/`systemd-run` 脱管，别在会因断网而中断的会话里执行切换。
- 面板侧已就绪，可直接取制品：`/packages/manifest.json`、`/packages/bui-c-linux-amd64`、`/api/nodes/<用户名>`（需 URL 编码中文）。

## 5. 约定（与服务端一致）

- 凭据不进 argv：URI 用 `import -` 从 stdin 读；测试与日志只出现合成值。
- 测试不碰真实系统：`Sys`/`Net` 用 fake；`scripts/tests/test-bui-c-install.sh` 只用 `127.0.0.1` 的 `python3 -m http.server`。
- 完成标准：`cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`；脚本过 `bash -n` + `shellcheck -S error`；`bash scripts/tests/run-all.sh` 全 PASS。
- 提交：中文 commit，`fix(bui-c): …` / `feat(bui-c): …`，一个任务一个 commit；分支从 `v4` 派生（`main` 与 `v4` 同步，发版走 tag `v4.0.0-rcN`，release.yml 自动构建 amd64/arm64 并发预发布）；**不要直接 push `v4`/`main`**，合并请求由原会话（服务端负责人）审查后进 `v4`。
- musl 交叉编译：`CC_x86_64_unknown_linux_musl=gcc AR_x86_64_unknown_linux_musl=ar cargo build --release --target x86_64-unknown-linux-musl -p bui-c`；`.cargo/config.toml` 已设非 PIE 静态。

## 6. 服务端侧与客户端相关的接口（改动需同步）

- `/api/nodes/<user>`（公开，按用户）：节点集合 + 分流规则，`bui-schema::nodes::nodes_for` 与 `SplitRules` 直接序列化；住宅 HY2 节点端口按用户所在槽位（IP 池，spec §5.6）——`40000 + 槽号`，跳跃区间等分。客户端不需要知道槽位概念，只按载荷连。
- `/packages/*`：由守护进程按 manifest 缓存分发（`client_sing_box` 是客户端目标版本）。
- 订阅（`/api/subscription/<user>`）与 `bui-c` 渲染共用 `render::client`，改一处两边生效；改 TUN 模板要考虑 sing-box 1.12–1.14 三版兼容（CI 有三版 `sing-box check` 矩阵）。

## 7. 文件边界（并行开发时）

客户端负责人可改：`crates/bui-c/**`、`scripts/bui-c-install.sh`、`scripts/tests/test-bui-c-install.sh`、`crates/bui/src/modules/panel/` 中**分发 `/packages/bui-c-install.sh` 的处理器**（做占位符替换）、本手册、spec §6 与 P4 计划。其它服务端文件先与原会话确认。

## 8. 相关记忆与记录

- 会话记忆 `project-baiyi-bui-c-handover`（主理人 2026-09-12 指定客户端由另一 agent 负责；baiyi 现场状态）。
- 总纲 `docs/superpowers/plans/2026-09-11-v4-master.md`「裁决记录」：P4 相关 11 条 + 「bui-c 首次安装」。
- 项目总交接 `HANDOVER.md`。
