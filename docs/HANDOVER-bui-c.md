# bui-c（Linux 客户端）交接手册

> 2026-09-13。给接手 **v4 Linux 客户端 `bui-c`** 的 agent / 工程师。服务端（`crates/bui`）与发布链路由原会话继续负责，两边并行时的文件边界见 §8。
> 生产主机一律用别名：`bwg-rick`（已切 v4）、`bwg-tizi`（仍是 v3.6.3）、`baiyi`（国内 Linux 客户端机，本手册的真机）。公开文档不写真实 IP / 域名 / 凭据。

## 1. 现状一句话

- 代码：`crates/bui-c`（约 11k 行 Rust 含测试，19 个文件，244 个单元测试），随 workspace 一起发版；GitHub 上最新预发布 `v4.0.0-rc7`。本手册对应的修复在本地分支 `Baiyi/bui-c-completion-aa7d30`（从 `6318b77` 起 50 个修复 commit + 文档，未 push），等服务端负责人审查合入 `v4` 后随下一个 rc 发布。与 `origin/v4`（多出 Hysteria2 鉴权改 http 的 3 个 commit）合并无冲突，合并树在 baiyi 上跑过全量门禁。
- **下一步看 [HANDOVER-bui-c-menu-v2.md](HANDOVER-bui-c-menu-v2.md)**：用户 2026-09-13 要求菜单加删除节点、[5] 连接检查补齐 v3.6.2、清屏与窄屏。设计与计划已定稿，实现做到一半（T1–T6、T8、T14 完成并审查；T11 在单独分支待审；T7 起未做），分支 `Baiyi/bui-c-menu-node-deletion-e6aee1` 已推到 origin。本段上面提到的 completion 分支后来已由服务端会话合进 `v4`，下文的 commit 哈希以那边为准。
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
| 节点来源 | `import <uri>|-`（stdin 读，凭据不进 argv）、`import --panel <url> --user <名>`（面板 `/api/nodes/<user>`，载荷 `NodesPayload{user,split,nodes}`）、`import --sub <url>`、`import-v3`（读 `/opt/hysteria-client/configs/*`，导入后卸载 v3 五个单元与残留）。 |
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
- 面板侧已就绪，可直接取制品：`/packages/manifest.json`、`/packages/bui-c-linux-amd64`、`/api/nodes/<用户名>`（需 URL 编码中文）。

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

- `/api/nodes/<user>`（公开，按用户）：节点集合 + 分流规则，`bui-schema::nodes::nodes_for` 与 `SplitRules` 直接序列化；住宅 HY2 节点端口按用户所在槽位（IP 池，spec §5.6）——`40000 + 槽号`，跳跃区间等分。客户端不需要知道槽位概念，只按载荷连。
- `/packages/*`：由守护进程按 manifest 缓存分发（`client_sing_box` 是客户端目标版本）。
- 订阅（`/api/subscription/<user>`）与 `bui-c` 渲染共用 `render::client`，改一处两边生效；改 TUN 模板要考虑 sing-box 1.12–1.14 三版兼容（CI 有三版 `sing-box check` 矩阵）。
- 客户端只把 **https 且 `/api/nodes/<user>` 成功返回合法载荷** 的面板记为自更新来源；v3 面板的 `/api/nodes` 回 401/404 时客户端回退订阅导入，不记面板。改 `/api/nodes` 的鉴权或状态码要考虑这条回退。
- `/packages/bui-c-install.sh` 下发时把 `PANEL_SOURCE="__BUI_C_PANEL_SOURCE__"` 替换成 `https://<Host>/packages`，`Host` 不合 `主机名[:端口]` 形状时用期望态域名。

## 7. 文件边界（并行开发时）

客户端负责人可改：`crates/bui-c/**`、`scripts/bui-c-install.sh`、`scripts/tests/test-bui-c-install.sh`、`crates/bui/src/modules/panel/` 中**分发 `/packages/bui-c-install.sh` 的处理器**（做占位符替换）、本手册、spec §6 与 P4 计划。其它服务端文件先与原会话确认。

## 8. 相关记忆与记录

- 会话记忆 `project-baiyi-bui-c-handover`（主理人 2026-09-12 指定客户端由另一 agent 负责；baiyi 现场状态）。
- 总纲 `docs/superpowers/plans/2026-09-11-v4-master.md`「裁决记录」：P4 相关 11 条 + 「bui-c 首次安装」。
- 项目总交接 `HANDOVER.md`。
