# bui-c（Linux 客户端）交接手册

> 2026-09-13。给接手 **v4 Linux 客户端 `bui-c`** 的 agent / 工程师。服务端（`crates/bui`）与发布链路由原会话继续负责，两边并行时的文件边界见 §8。
> 生产主机一律用别名：`bwg-rick`（已切 v4）、`bwg-tizi`（仍是 v3.6.3）、`baiyi`（国内 Linux 客户端机，本手册的真机）。公开文档不写真实 IP / 域名 / 凭据。

## 1. 现状一句话

- 代码：`crates/bui-c`（约 11k 行 Rust 含测试，19 个文件，244 个单元测试），随 workspace 一起发版；GitHub 上最新预发布 `v4.0.0-rc7`。本手册对应的修复在本地分支 `Baiyi/bui-c-completion-aa7d30`（从 `6318b77` 起 50 个修复 commit + 文档，未 push），等服务端负责人审查合入 `v4` 后随下一个 rc 发布。与 `origin/v4`（多出 Hysteria2 鉴权改 http 的 3 个 commit）合并无冲突，合并树在 baiyi 上跑过全量门禁。
- **下一步看 [HANDOVER-bui-c-menu-v2.md](HANDOVER-bui-c-menu-v2.md)**：用户 2026-09-13 要求菜单加删除节点、[5] 连接检查补齐 v3.6.2、清屏与窄屏。2026-09-15 起 P0（T1–T12c、T14、Ttoken 等）已全部进分支 `Baiyi/bui-c-menu-node-deletion-e6aee1` 并审查收口，baiyi 第一轮真机验收通过删除流程与 [5]；剩 T13、T15–T18 与最终审查，这批进 4.0.1，客户端现状见文末「菜单 v2 现状」一节。本段上面提到的 completion 分支后来已由服务端会话合进 `v4`，下文的 commit 哈希以那边为准。
- **跨 crate 的 3 个 commit 需要服务端负责人单独过目**：`20758eb`（`bui-schema` 的 `node_uri` 接受 `user%3Apass@`；服务端没有调用方，只影响客户端）、`ef163f1`（`panel/packages.rs` 下发 `bui-c-install.sh` 时替换面板源占位符）、`85e8f9c`（`safe_host`：`Host` 头只认 `主机名[:端口]`，否则回落期望态域名，防 `curl | sudo bash` 注入）。后两个还没部署到 rick。
- 服务端已能分发客户端：`https://<面板域名>/packages/bui-c-install.sh`、`/packages/bui-c-linux-{amd64,arm64}`、`/packages/manifest.json`（总纲 C4 形状），GitHub Releases 的预发布也带同样的三件。
- **baiyi 已于 2026-09-13 完成 v4 切换**：`bui-c 4.0.0`（当前是本分支 `56219f9` 的代码构建（历史脱敏重写前的同一份源码，sha256 `74555d02…`））跑在 TUN 模式，活动节点仍是 v3 时的 `hysteria2-1778329470`；v3 五个单元已卸，`/opt/hysteria-client/` 与 `/root/bui-c-v3-backup/` 按约定保留作回滚素材（30 秒回滚命令见 §4）。
- 数字菜单做过三轮真机逐键测试（§4「菜单真机测试」），发现的问题分四批修完。
- M4 里程碑（spec §9）**已验收**（2026-09-13，baiyi，结果表见 §4；④ 在最终二进制上用沙箱重跑过）：四节点 SOCKS/TUN 均通；裸 IPv6 回落符合 spec；`kill -9` 6 秒内由 systemd 拉起、`systemctl stop` 后 40 秒内由 `bui-c.timer` 拉起；从 v3 原地升级 5 个节点一个不丢、活动节点保持。

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
| 节点来源 | `import <uri>|-`（stdin 读，凭据不进 argv）、`import --panel <url> --user <名>`（面板 `/api/nodes/<末段>`，末段 2026-09-14 起是订阅 token，见 §6；载荷 `NodesPayload{user,split,nodes}`）、`import --sub <url>`、`import-v3`（读 `/opt/hysteria-client/configs/*`，导入后卸载 v3 五个单元与残留）。 |
| 可测性 | 所有系统交互经 `Sys`（命令/文件）与 `Net`（HTTP）两个 trait；单元测试只用 `fake.rs` 的内存实现，不 `systemctl`、不写 `/etc`、不出网。真机行为留给 M4。 |
| CLI | `status / list / switch / mode / import / check / update / import-v3 / uninstall / menu`；全局 `--json`（status/list）、`-y/--yes`。 |

设计与计划：spec `docs/superpowers/specs/2026-09-11-v4-architecture-design.md` §6（客户端）、§3.4（单元资源）、§7（多源下载）；计划 `docs/superpowers/plans/2026-09-11-v4-p4-client.md`（13 任务，含 D1–D11 决策与 M4 清单）；裁决 `docs/superpowers/plans/2026-09-11-v4-master.md`「裁决记录」P4 各条。

## 3. baiyi 首次真机安装暴露的缺陷与修复（2026-09-13 全部修复）

按 2026-09-12 首次真机 `import-v3` 失败时记下的优先级，每条一个 commit（分支 `Baiyi/bui-c-completion-aa7d30`）：

| # | 缺陷 | 根因 | 修复（commit 主题） |
|---|---|---|---|
| 1 | `import-v3` 先卸旧单元、后拿引擎，manifest 拿不到时机器直接没代理 | `import_v3::run` 顺序：导入 → 落盘 → 卸 v3，内核要到 cli 的 `apply_with_ufw` 才装 | `import-v3 先备好引擎并自检，最后才卸 v3`：导入 → `ensure_kernel` → 渲染 + `sing-box check` → 落盘 → `teardown`；前三步任一失败即 `Err`，**不落盘、不动 v3**。新增 `--panel <url>`、`--mode socks\|tun`，`BUI_C_PANEL=<url>` 可一次性覆盖 manifest 来源（`update`/`check` 只在内存里覆盖；`import-v3` 会连同 `--panel` 一起落盘） |
| 2 | manifest 来源在预发布期不可用：v3 面板没有 `/packages`，GitHub `latest` 404 | GitHub 的 `releases/latest` 不含预发布 | `manifest 来源在 GitHub latest 404 时回退到最新预发布`：查 releases 列表取第一个 `prerelease=true` 且形如 `v<x.y.z>-rcN` 的 tag，口径同服务端 `kernels::fetch_manifest_with`；断网/502 不回退 |
| 3 | `import-v3` 只导入了 1 个节点，三个 `hysteria2-*` 目录「跳过」 | v3 ≤3.4 把整段 `user:pass` 做 encodeURIComponent 放进 userinfo（`user%3Apass@host`），url crate 看不到冒号、`password()` 为 None，`node_uri` 报「缺少用户名或密码」 | `node_uri 接受 v3 早期 user%3Apass@ 形式`（`crates/bui-schema/src/parse/node_uri.rs`，唯一一处跨 crate 改动）：解码后按第一个冒号拆一次 |
| 4 | `scripts/bui-c-install.sh` 默认源是 GitHub latest | 脚本没有「面板下发时替换占位符」的机制 | `安装脚本默认用下发它的面板 /packages，GitHub latest 404 回退预发布`（脚本 + 7 个回环用例）与 `下发 bui-c-install.sh 时把面板 /packages 写进占位符`（`panel/packages.rs::get_package`，`Host` 头只认 `主机名[:端口]` 形状，否则回落期望态域名）；真机上又发现 awk 提前 `exit` 在 251KB 的 releases JSON 上触发 SIGPIPE、被 `pipefail` 杀掉脚本（rc=141），已修并把夹具做大到能复现 |
| 5 | 菜单「服务控制」在单元不存在时报 `Unit bui-c.service not found` | 直接 `systemctl restart` | `菜单「服务控制」在单元不存在时引导先装引擎` |
| 6 | （新发现）所有面板用户名都是中文，`sanitize` 后为空，profile 名塌成 `hy2-direct`/`-2`/`-3`，不同服务器的同类节点互相覆盖 | `profile_name` 只留 ASCII 且是 upsert 主键 | `import-v3 沿用 v3 目录名做 profile 名，中文用户名回落到主机名`：v3 用户在 v3 里就是拿目录名 `switch` 的；面板/订阅导入的名字形如 `<rick 域名>-hy2-resi` |
| 7 | （新发现）在已迁移的机器上重按 [7]：多出 5 个 `-2` 重复节点，并删掉 v4 正在用的 `bui-tun`（断网一分钟） | v3 目录按约定保留，`detect` 恒为真；`teardown` 无条件 `ip link delete bui-tun` | `import-v3 重跑幂等`：同一连接的节点计入「已导入过」不新建；v4 在跑时不删接口；无事可做时不 apply |
| 8 | （新发现）同一台服务器两个中文账号互相覆盖凭据；v3 迁来的节点经面板再导入出现两份 | 名字是主键，`find_by_node` 比整个 `Node`（含 label） | `导入按连接身份去重`：kind + host + port + 凭据相同即原地更新（不看 label/hop/sni）；同名不同账号另起 `-2` 并提示；同一 ASCII 用户名在两台服务器上是两个账号 |
| 9 | （新发现，高危）导入任意订阅会把订阅主机记成 `panel`，而 `panel` 是 root 每日自更新 manifest 与二进制的首选来源；无变化的导入也会悄悄换掉它 | `fetch_sub` 与回退路径无条件写 `prof.panel` | `只信任被证明是 v4 面板的 https 源`：只有 `/api/nodes` 成功且 https 才记；换来源要提示；update 读到非 https 的旧 panel 也跳过 |
| 10 | （新发现）网络错误把完整 URL（含用户名）带进屏幕与 journal | reqwest 错误文本自带 `for url (…)`，绕过 `redact_url` | `网络错误不再带出完整 URL`：`without_url()` |

## 4. M4 验收清单与安全做法

清单（spec §9）：① 四节点（直连 REALITY / 住宅 REALITY / 直连 HY2 / 住宅 HY2）SOCKS 与 TUN 都通；② 裸 IPv6 回落：`curl -6 https://api64.ipify.org` 应失败或超时，`curl https://api64.ipify.org` 返回节点 IP，`ip -6 route` 默认路由在 TUN 上；③ `kill -9 sing-box` 后一分钟内 `bui-c.timer` 自愈；④ 从 v3 原地升级后 `bui-c list` 与 v3 `/opt/hysteria-client/configs/*` 数量一致，活动节点保持。

安全做法（血泪教训）：
- **baiyi 上跑着的其它会话（包括 Claude Code 自己）对 API 的访问依赖这条隧道**。切 TUN 前先确认本地恢复手段：`sudo tar xzf /root/bui-c-v3-backup/hysteria-client-*.tgz -C / && sudo systemctl daemon-reload && sudo systemctl enable --now bui-tun.service` 能在 30 秒内把 v3 拉回来。
- 先用 `socks` 模式验通四个节点，再切 `tun`。
- 长命令用 `nohup`/`systemd-run` 脱管，别在会因断网而中断的会话里执行切换。
- 面板侧已就绪，可直接取制品：`/packages/manifest.json`、`/packages/bui-c-linux-amd64`、`/api/nodes/<订阅token>`（2026-09-14 前是 `<用户名>`，中文需 URL 编码）。

### 菜单真机测试（2026-09-13，baiyi）

方法：在 baiyi 上用独立的 `tmux -L buitest` 起 100×40 与 60×30 伪终端跑 `sudo bui-c`，逐字符 `send-keys` 模拟人手输入，每步 `capture-pane` 截屏；截屏交给多视角审阅（文案、渲染对齐、危险操作、错误反馈、spec 与用户习惯），每条发现三方反驳，修完再由「截屏判定 + 代码判定」两人独立判定、分歧时第三人裁决。改 `profiles.json` 的步骤先备份、测完按 sha256 核对恢复；切 SOCKS/TUN 前挂 `systemd-run --on-active` 保险丝。

| 轮次 | 覆盖 | 结果 |
|---|---|---|
| 第一轮（23 步） | 首屏、无效输入、节点列表、切节点/模式、导入坏链接、服务控制、检查更新、自动更新、卸载取消、Ctrl-C、空行、一次性命令、窄终端 | 17 条问题成立、13 条被反驳；另真机复现非 UTF-8 输入崩出菜单、重按 [7] 删接口 |
| 第二轮（18 步） | 逐条复测第一轮问题 + 新功能 | 21 条里 19 条修好（剩 60 列状态行折行、SOCKS 下分隔线差 2 列）；新找出 9 条回归（列表 113–119 列、输错踢回主菜单、日志被顶出屏幕等） |
| 第三轮（15 步） | 第三批 9 项 + 导入去重在真实 v3 订阅上的表现 + timer 日志 + 沙箱新机器 | 13 项全部修好；又找出 10 条（其中 2 条高危：订阅主机成为自更新源、无变化导入换掉更新源） |
| 收尾（针对性） | 第四批与三处遗留 | v3 面板回退提示、无变化导入 `profiles.json` 字节不变且更新源仍是 rick、切到当前节点不重启、网络错误不带用户名、没节点切模式不下内核；最终二进制上沙箱重跑 M4 ④ 通过 |

修完后的菜单行为（给接手的人对照）：
- 主菜单：直接回车只重画，`0` 或 EOF 才退出；认全角数字；非 UTF-8 输入按无效选项处理；[2] 写出目标模式「切到 SOCKS / 切到 TUN」；结果行缩进两列；分隔线与最宽选项行等宽。
- [1] 切换节点：空行 + 标题，每个节点两行（`[n] ★ 名字` / `label  kind  host:port`），真机数据每行 ≤ 80 列；输错原地重问，空行或 `0` 返回。
- [3] 导入节点：空行即取消；单独一行的面板地址（`/api/sub|subscription|clash|nodes/<用户>`）走 `/api/nodes`，v3 面板 401/404 时回退订阅；有新节点且活动节点不在其中时追问是否切换；原地更新了活动节点会 apply。
- [4] 服务控制：二级菜单「重启 / 最近 50 行日志 / 返回」；重启在 TUN 下等接口就绪；日志带标题、每行「时间 消息」、去 ANSI，看完回车返回。没有单元时引导「先用 [3] 导入节点」。
- [5] 连接检查：手动检查不受退避约束；异常逐条中文说明；TUN 下重启后等接口。timer 路径保持退避，journal 里同样是中文。
- [7] 从 v3 导入：没有 v3 目录时如实说明；已导入过时单行说明、不改任何东西。
- 命令行：`status`/`list` 不带菜单块与编号；`--json` 以换行结尾；错误走 stderr；非终端 stdin 不打提示符；`| head` 不 panic。

### M4 验收结果（2026-09-13，baiyi，x86_64，Ubuntu 24.04，有 IPv6，ufw inactive）

| 判据 | 结果 |
|---|---|
| ① 四节点 SOCKS/TUN 均通 | 从 rick 面板 `import --panel … --user <四节点用户>` 得到四种节点；SOCKS 与 TUN 下逐个 `switch` + `check` 均「正常：单元在跑、204 探测通过」；直连出口 <rick 直连出口 IP>（rick），住宅出口 <住宅出口 IP>（Bright Data，槽 0） |
| ② 裸 IPv6 回落 | TUN 下 `curl -6 https://api64.ipify.org` 失败（rc=6）；`curl https://api64.ipify.org` 返回节点 IPv4；`ip -6 route show table all` 里 `default … dev bui-tun table 2022`；`bui-tun` 有 `fdfe:dcba:9876::1/126` |
| ③ 杀 sing-box 一分钟内自愈 | `pkill -9 -x sing-box` → 6 秒内 `active`（`Restart=always`）；更强的 `systemctl stop bui-c` → 40 秒后由 `bui-c.timer` 拉起，`journalctl -u bui-c-check` 记录 `UnitDown / TunMissing / TunNoDefaultRoute` 三项异常并重启 |
| ④ 原地升级不丢节点 | `import-v3 --panel https://<rick> --mode socks -y`：导入 5 个（= v3 `configs/` 目录数），名字即目录名，活动节点保持 `hysteria2-1778329470`；卸载 5 个旧单元；`profiles.json`/`config.json`/`runtime.json` 均 0600 |
| 其它 | `update --check-only` → `manifest 4.0.0（来源 面板）已是最新`；`--auto off/on` 只翻开关；ufw inactive 时全程无报错、`runtime.json` 的 `ufw_rules=false`；v3 节点里 `<临时域名>`（已下线的临时面板）探测失败并按 1 分钟退避重启，属预期 |

已知差异与遗留（不在 M4 范围）：
- v3 的 `bui-tun.service.d/10-camoufox-direct.conf`（`ExecStartPre` 注入 camoufox 直连规则）v4 **不继承**，目录还留在 `/etc/systemd/system/` 下无主；若 baiyi 上的 camoufox 依赖它直连，需另行处理。
- `/usr/local/bin/bui-c.v3`、`bui-c.bak.*`、`bui-c.bak-3.6.2` 是 v3 脚本备份；**v3 脚本的 `--version` 会触发它自带的「更新全局命令」逻辑去覆盖 `/usr/local/bin/bui-c`**，辨认它只用 `file`/`sha256sum`，别跑它的任何子命令。
- `xray.service`（`/usr/local/etc/xray`，failed）不属于 v3 客户端清单，`import-v3` 不碰它。
- baiyi 上 `~/b-ui-build/` 是本分支的构建目录（依赖缓存 ~1GB），`~/b-ui-merge/` 是与 `origin/v4` 的临时合并树，`/tmp/bui-c.<commit>` 是各轮装上去的二进制。
- 沙箱 `BUI_C_BASE` / `BUI_C_UNIT_DIR` **只隔离文件，不隔离 systemd**：状态点读的是真实 `bui-c.service`，会 apply 的路径（导入首个节点、[2] 有节点时、[4] 重启、[8] 卸载）会作用到真实单元。只在沙箱里测不会 apply 的路径，或预期真实服务会重启一次。
- 服务端 `install.sh` 的 `latest_v4_tag` 与本次修掉的脚本是同一个 `tr | awk … exit` 写法，releases JSON 超过管道缓冲后会 SIGPIPE，属服务端负责人的文件，已在 PR 里提醒。
- rick `/packages/sing-box-linux-amd64` 是 81MB（似未 strip），客户端照 sha256 装了，不影响功能。
- **已装客户端收不到这些修复**：各 rc 的 manifest `version` 都是 `4.0.0`，`update::run` 只在版本字符串不同时替换自身，所以已装 rc6/rc7 的客户端不会自更新到新构建。baiyi 是手动装的。发版时需要服务端负责人决定怎么让修复到达（例如版本号前进，或 manifest 带构建标识）。

客户端侧仍未做的（有意留下，各自需要决定）：
- 进程锁：timer 的 `check` 与菜单操作、两个会话同时改 `profiles.json` 之间没有互斥。标准做法是 `flock`，要给根 `Cargo.toml` 的 `nix` 加 `fs` 特性（与服务端共用），或把 MSRV 从 1.85 提到 1.89 用 `File::try_lock`，按 §7 先与服务端负责人确认。
- 60 列终端下状态行的节点名 + label 会折行；失败行有 3 种以上 scheme 时略超 80 列。
- [6]「检查更新」直接执行更新（替换自身 + 内核 + 重启），没有先显示版本再确认。
- 菜单里没有删除节点的入口（`Profiles::remove` 已有但无人调用），baiyi 上已下线的 `<临时域名>` 节点删不掉。
- 旧版本经 `--sub` 记下的第三方 **https** panel 无法从文件本身分辨，仍会被当作更新源；只影响在旧构建上用过 `--sub` 的机器（baiyi 的 panel 是 rick）。
- 「安装脚本 → 导入 → 首次 apply 建单元 → 切模式 → 卸载」这条全新主机路径没有在干净的 systemd 主机上真机走过（单元测试覆盖，M4 验的是从 v3 原地升级）。

## 5. 约定（与服务端一致）

- 凭据不进 argv：URI 用 `import -` 从 stdin 读；测试与日志只出现合成值。
- 测试不碰真实系统：`Sys`/`Net` 用 fake；`scripts/tests/test-bui-c-install.sh` 只用 `127.0.0.1` 的 `python3 -m http.server`。
- 完成标准：`cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`；脚本过 `bash -n` + `shellcheck -S error`；`bash scripts/tests/run-all.sh` 全 PASS。
- 提交：中文 commit，`fix(bui-c): …` / `feat(bui-c): …`，一个任务一个 commit；分支从 `v4` 派生（`main` 与 `v4` 同步，发版走 tag `v4.0.0-rcN`，release.yml 自动构建 amd64/arm64 并发预发布）；**不要直接 push `v4`/`main`**，合并请求由原会话（服务端负责人）审查后进 `v4`。
- musl 交叉编译：`CC_x86_64_unknown_linux_musl=gcc AR_x86_64_unknown_linux_musl=ar cargo build --release --locked --target x86_64-unknown-linux-musl -p bui-c`；`.cargo/config.toml` 已设非 PIE 静态。macOS 本机没有 musl 工具链且 `crates/bui` 因 inotify 编不过，实际做法是 `rsync` 到 baiyi 的 `~/b-ui-build/` 编（32 核冷编 25 秒，增量 17 秒），`cargo clippy --workspace` / `cargo test --workspace` / `scripts/tests/run-all.sh` 也在那里跑。

## 6. 服务端侧与客户端相关的接口（改动需同步）

- `/api/nodes/<token>`（公开）：节点集合 + 分流规则，`bui-schema::nodes::nodes_for` 与 `SplitRules` 直接序列化；住宅 HY2 节点端口按用户所在槽位（IP 池，spec §5.6）——`40000 + 槽号`，跳跃区间等分。客户端不需要知道槽位概念，只按载荷连。
- **路径末段 2026-09-14 起是随机订阅 token，不再是用户名**（spec §4.5）：32 位小写十六进制，面板里点开该用户的配置弹窗复制，`/api/sub|subscription|clash|nodes` 四条同一口径。旧的用户名链接只在服务端的全局宽限期内还认（全新装机没有宽限期；v3 导入给 7 天），过后一律 404 `{"error":"User not found"}` —— 与「查无此人」同一个回应，客户端**分辨不出**是链接过期还是用户被删，错误文案不要写成「用户不存在」。所以面板给出的导入链接、`import --panel` 记下的 `base_url` + 末段都应按 token 存。
- **轮换凭据（面板「重置订阅链接与凭据」/ `POST /api/users/<用户名>/rotate`）会同时换订阅 token、HY2 密码与 VLESS UUID**：已部署的 `bui-c` 手里的节点凭据当场失效，`bui-c.timer` 每分钟一次 `check` 会一直判失败、按退避不停重启 sing-box，而更新源（面板 `/api/nodes/<旧 token>`）也 404 ⇒ **无法自愈，必须人工重新导入一次**（面板复制新链接 → `bui-c import --panel … ` 或菜单 [3]）。运维在轮换某个用户前要先知道他有没有 Linux 客户端。
- `/packages/*`：由守护进程按 manifest 缓存分发（`client_sing_box` 是客户端目标版本）。
- 订阅（`/api/subscription/<token>`）与 `bui-c` 渲染共用 `render::client`，改一处两边生效；改 TUN 模板要考虑 sing-box 1.12–1.14 三版兼容（CI 有三版 `sing-box check` 矩阵）。
- 客户端只把 **https 且 `/api/nodes/<末段>` 成功返回合法载荷** 的面板记为自更新来源；v3 面板的 `/api/nodes` 回 401/404 时客户端回退订阅导入，不记面板。改 `/api/nodes` 的鉴权或状态码要考虑这条回退。
- `/packages/bui-c-install.sh` 下发时把 `PANEL_SOURCE="__BUI_C_PANEL_SOURCE__"` 替换成 `https://<Host>/packages`，`Host` 不合 `主机名[:端口]` 形状时用期望态域名。

## 7. 文件边界（并行开发时）

客户端负责人可改：`crates/bui-c/**`、`scripts/bui-c-install.sh`、`scripts/tests/test-bui-c-install.sh`、`crates/bui/src/modules/panel/` 中**分发 `/packages/bui-c-install.sh` 的处理器**（做占位符替换）、本手册、spec §6 与 P4 计划。其它服务端文件先与原会话确认。

账号匹配（4.0.2）那一批判定函数**全在 `crates/bui-c/src/profiles.rs`，服务端没有调用方**：`same_account` / `same_endpoint` / `same_params` / `kind_trusted` / `gate_ok` / `protected` / `movable` / `best_source` / `tombstone_key` 这些自由函数，以及 `Profiles` 上的 `account_group` / `blocked_same_account` / `pick_keeper` / `find_account_before` / `raise_source` / `rename` / `heal_token_names` / `merge_into`；输出文案在 `menu.rs`（`protected_new` / `protected_kept` / `kind_unsure_new` / `dups_head` / `display_name`），编排在 `cli.rs` 的 `store_fetched` / `store_import` / `apply_import` / `menu_import` 与 `import_v3.rs::import`。判定的输入是 `bui_schema::nodes::Node` 的字段（`kind`、`host`、HY2 `username`、Reality `uuid`、端口、`obfs_password`、`sni`、`public_key`、`short_id`）——**改 `Node` 的字段或 kind 口径是服务端的事，会同时改变客户端的账号判定**，两边要一起看。住宅 HY2 的 label「HY2住宅」被这套判定依赖（`profile_name` 与 kind 判定都从它来），服务端 4.1 spec §7.4 已写死不改。

## 8. 相关记忆与记录

- 会话记忆 `project-baiyi-bui-c-handover`（主理人 2026-09-12 指定客户端由另一 agent 负责；baiyi 现场状态）。
- 总纲 `docs/superpowers/plans/2026-09-11-v4-master.md`「裁决记录」：P4 相关 11 条 + 「bui-c 首次安装」。
- 项目总交接 `HANDOVER.md`。

## 菜单 v2 现状（2026-09-15）

分支 `Baiyi/bui-c-menu-node-deletion-e6aee1`（`632b475`，`bui-c` 524 个测试），进 4.0.1，等 `v4.0.0` 打 tag 后再合进 `v4`。提交表、审查记账、真机验收与接手办法全在 [HANDOVER-bui-c-menu-v2.md](HANDOVER-bui-c-menu-v2.md)；这里只列与上文 §2–§4 不同的地方。

- **主菜单重排**：`[1] 切换 [2] 模式 [3] 导入 [4] 服务控制 [5] 连接检查 [6] 删除节点 [7] 更新与维护 [8] 卸载 [0] 退出`（`[9] 节点测速` 等 T16）。原来的检查更新、从 v3 导入、自动更新开关都收进 [7] 子页（`[1] 检查更新 [2] 自动更新开关 [3] 从 v3 导入`），按旧键给一行过渡提示；有新版时显示 `[7] 更新与维护 ★`。上文 §4「修完后的菜单行为」里的「[7] 从 v3 导入」现在是 [7]→[3]。
- **删除节点**：菜单 [6] 与 `bui-c delete <名字>... [--switch-to <名字>] [-y] [--json]`（`delete.rs`）。批量编号（`1-3`、`1,4`），有一个编号不对就整行作废、不部分执行。三种形态：删非当前节点（`[y/N]`，不重启不断网）、删到当前节点（要输入 `yes`，默认换到不在同一台服务器的节点，TUN 下断网几秒）、删光（要输入 `yes`，停掉代理）。顺序是快照比对 → 预检 → 先数据面后落盘 → 失败回滚，回滚的成败判据与前向 apply 相同。上文 §4「菜单里没有删除节点的入口」已做。
- **墓碑**：删掉的节点按账号级指纹记进 `profiles.json` 的 `deleted`（不含明文凭据），从面板 / 订阅 / v3 重新导入时先跳过、再问要不要加回；`import --with-deleted`、`import-v3 --with-deleted` 连删过的一起导；单独粘贴一条链接直接恢复。
- **[5] 连接检查**（`nettest.rs`）补齐 v3.6.2：服务、本地端口、TUN、隧道、Google、YouTube、GitHub、百度直连、1MB 下载与评语、IPv4 出口（地理与风险分）、IPv6 逐项边做边打；失败给判断与下一步小菜单（再查一次 / 换个节点 / 看日志）。timer 的 `check` 不碰任何检测站（有守门测试）。SOCKS 下拨节点超时不再误报「域名解析失败」，改说「连不上」并带耗时。
- **进程锁** `/run/bui-c.lock`（`lock.rs`，`nix::fcntl::Flock`）：改配置的路径互斥，等不到最多 15 秒后说「稍后再试」；巡检遇锁跳过这一轮。`crates/bui-c/Cargo.toml` 给 nix 加了 `fs` 特性（不加时 `cargo build -p bui-c` 单独构建编不过），根 `Cargo.toml` 没动。上文 §4「客户端侧仍未做的」第一条（进程锁）已做。
- **更新先确认**：[7]→[1] 先显示「最新 X（来源 …），本机 Y」和会替换什么，再 `[y/N]`，答 N 不下载。`update` 拆成锁外下载、锁内安装，只在有活动节点时重启；下载期间被别的会话装过时不拿旧下载去盖——装的是同一份就说没有需要更新的，装的是别的一份则菜单重新显示再问、`bui-c update` 退出 1。上文 §4「检查更新直接执行更新」已改。
- **中断收敛**：删除动数据面之前写 `/opt/bui-c/pending.json`；做到一半断线或被杀，下次进菜单或下一分钟巡检按节点列表收拾好，并在「上次」行说明。
- **token 链接导入**：服务端 rc12 把订阅路径末段换成随机 token 之后，菜单 [3] 或 `import --sub` 粘贴面板复制的整条链接即可；profile 名用载荷里的用户名，不露 token；`--user -` / `--sub -` 从标准输入读；面板回 404 时提示「从面板重新复制整条订阅链接」，不再说「面板没有节点接口」；缺导入来源退出码 2。
- **显示**：交互终端清屏重画并加「上次」行；按终端宽度排版，60 列状态行不再折行、40 列手机 SSH 可用（上文 §4 遗留「60 列折行」已解决）；菜单不认全局 `-y`；v3 导入邀请只在还没迁移时出现。
- **上文 §4 遗留更正**：「已装客户端收不到这些修复」已由同版本比 sha256 解决（见 CHANGELOG 4.0.0）。
- **仍未做**：卸载前先列清单（T13）、[9] 节点测速、巡检连续失败行、`bui-c test`；第一轮真机验收里侵入性的几项（SOCKS 黑洞验应答码、删活动节点、真 flock 并发）。

## 4.0.2 按账号匹配（2026-09-17）

定稿 `docs/superpowers/specs/2026-09-15-bui-c-account-match-design.md`（T1–T12）。**为服务端 4.1 把住宅 HY2 从「每槽一个端口」改成共享端口做准备**：4.0.x 的客户端遇上端口变化会另起一条新节点，旧节点还连得上、但从另一个住宅 IP 出去（定稿 F5），4.1 上线后每台客户端都会多出副本、当前节点可能一直停在旧端口。这里只列与上文 §2–§4、「菜单 v2 现状」不同的地方；用户可见文案逐条见 CHANGELOG 4.0.2 段与定稿 §10。

- **导入按账号匹配**：账号 = `kind` + `host`（**不区分大小写**，与墓碑 key 同口径）+ 凭据主体（HY2 `username`、Reality `uuid`），从 `node` 现算、**不持久化**。同一账号端口变了就沿用留存者的名字原地替换，打 `更新节点 X：端口 A → B`；留存者次序：活动节点 > 导入前已存在 > 同一连接 > 名字等于 `profile_name` > 列表位置。
- **当前节点会被挪到服务端这次给的端口**：当前节点若还停在旧槽端口，面板导入后换到服务端为该用户分配的端口，**出口住宅 IP 随之换成服务端分配的那个**。这是有意的——旧端口的副本在兼容转发下线前还能连，但它走的是另一个住宅 IP。
- **只升不降扩到节点本身**：非面板来件（粘贴 / v3 / 订阅）遇上「面板来源的条目」或「活动节点」时，只有除 `label`、`hop` 以外 `node` 全等才原地替换；否则不覆盖，按挡下的原因打一行说明并给出路（面板那条 → 从面板重新导入；活动节点那条 → 确认新节点能用后切换过去）。挡下时账号组为空就另起新名，组非空就只替换留存者。理由：一条凭据轮换前留下的旧链接粘进来会把活动节点改成旧密码并立即 apply，TUN 下整机断网。现拉的订阅不算过期副本，Subscription 条目之间照旧互相替换。
- **kind 门槛**：两边 kind 都可信（面板 `/api/nodes` 来源，或备注含「直连」「住宅」）才允许端口不同也算同一账号；任一边是猜的就只认同端口。只可能拦下备注里没有「直连」的 `Direct`；直连端口在 4.1 不变，所以 4.1 的住宅换端口不受影响。受保护条目同时落在门槛外时，说明句在「…不变」之后补「同时认不准是直连还是住宅」。
- **来源命中即升级**：`ApiNodes > Subscription > Paste = V3`，条目被命中就升到「本次来件 / 条目自身 / 账号组内面板成员」三者中最高的那一级，`Replaced` 与 `Unchanged` 都升，从不降。分流规则同理：非面板来件命中账号时沿用组内面板成员的 `split`。
- **存量重复不自动合并**：命令行只打 `同一账号还有 N 个节点：…（本次已更新 X）` + `要合并请在菜单 [3] 里导入并答 y`；菜单 [3] 在放锁之后、墓碑那一问之后多问 `要合并吗？[y/N]`（默认 N），答 y 另拿一次锁、重读文件、`merge_into` 一次写盘。合并永不碰活动节点（活动节点只要在组里就是留存者），所以不需要 apply。
- **token 名改名**：导入时把 4.0.0 留下的 token 名改成 `profile_name("", node)`（`<主机>-<kind>`，被占则 `-2`），端口没变、命中同一连接时也改；每条打 `节点 <旧名（打码）> 已改名为 <新名>`。改名与 `active`、墓碑显示名在同一把锁内一次写盘，按内容比较所以不 apply、不重启。**按旧 token 名写的脚本（`bui-c switch <旧名>` 等）会失效**，这一条要写进发版说明。
- **人读输出打码**：`display_name` 把名字里的 token 段打成前 4 位加省略号，覆盖 `list`、`status`、菜单列表、删除确认块、墓碑名单、「上次：」行、导入与切换输出；`--json`（status / list / delete）与 `profiles.json` **不打码**（机器接口，能跑它们的人本来就能读文件）。后果：打码后的名字不能直接敲进 `switch` / `delete`，出路是菜单按编号选、`list --json` 取原名、或导入一次让它改名——同样写进发版说明。
- **`import-v3`**：v3 目录里的节点在**本次运行开始前**的列表里已有同一账号时记进 `existing` 跳过，不再多出 `-2`；只跳过、不替换、不合并、不升来源（`<base>/configs` 是 v3 时代的冻结快照）。结果行文案不变。
- **apply 判定**改成比较导入前后活动节点的内容，不再看名字；菜单「切换到新导入的 X？」改用 `Stored.added`，改名与合并都不再触发，活动节点被挡下时改问 `切换到 {keep}？`。
- **兼容**：`Profiles` / `Profile` / `Tombstone` 加 `#[serde(flatten)]` catch-all 保住未知字段（`node` 不加，它由服务端下发、每次导入整条覆盖，加了会削弱总纲 C1）；`SCHEMA_VERSION` 不动，没有新增持久化字段。catch-all **只保护降到 4.0.2 及以后的版本**；降到 4.0.0 照旧丢墓碑、token 名复发。定稿 §6.4 判定本改动**不触发** C5：不必因为降级丢字段的风险去关 baiyi 的自动更新。（下一条里验收期间的 `--auto off` 是另一回事，为的是别被面板那一版盖掉待验的构建。）
- **上线闸门**：客户端拿到哪个 bui-c 由**打了哪个 tag** 决定，与服务端升没升无关。baiyi 必须在打任何 4.0.2 tag **之前**验收本地构建（定稿 §11.6 测试 79），打 `v4.0.2-rc1` 之后再用 CI 的发布件验一遍，验收期间 `bui-c update --auto off`。合入后把「不自动合并同账号重复节点」的实现提交 SHA 交服务端填进 `scripts/release/required-commits.env` 的 `GATE_v4_1`。完整时序见定稿 §12.7。

### 兼容段下线前的操作（给公告和运维照抄）

服务端 4.1 的兼容段 `40001-40007` REDIRECT **不永久保留**：counter 连续 30 天为 0 才 `bui set hy2-resi-compat off`，有命中就顺延，公告与下线之间不少于 30 天，公告里要**单列一段**点名 bui-c。客户端不会自己刷新节点，光说「重新导入」不够——活动节点来源是 V3 / Paste、或被上面「只升不降」挡下的机器，重新导入后当前节点可能仍停在旧端口。所以第 ③ 步以第 ② 步**看到的结果**为准，不以导入时打了哪句提示为准：

> Linux 客户端（bui-c）请在兼容段下线前操作一次：① 从面板复制链接重新导入（`sudo bui-c import --sub -` 粘贴，或菜单 [3]）；面板导入会更新同一账号的旧条目。② 用 `sudo bui-c status` 看当前节点，再用 `sudo bui-c list` 看它的端口是不是新端口（住宅 HY2 为 40000）。③ 如果第 ② 步看到当前节点仍是旧端口：在 `sudo bui-c list` 里找同类型（直连或住宅）、端口是新端口的那一条，确认它能用后 `sudo bui-c switch <它的名字>`（或在菜单里切换）。

30 天零命中的判据兜不住下线前一直关机或离线的机器；这类机器开机后若断网，按上面三步处理。
