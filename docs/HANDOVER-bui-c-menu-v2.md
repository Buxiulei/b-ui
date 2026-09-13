# bui-c 菜单 v2（节点管理与界面优化）交接手册

> 2026-09-13。给接手 bui-c 下一阶段的 agent / 工程师。先读完 [HANDOVER-bui-c.md](HANDOVER-bui-c.md)（客户端现状、M4 验收、四批菜单修复、约定与边界），再读这份。
> 公开文档不写真实域名、IP、用户名与凭据。本文用别名：`bwg-rick`（v4 面板）、`bwg-tizi`（仍是 v3 面板）、`baiyi`（国内 Linux 客户端真机，SSH 别名）。真实值在本机 Claude 记忆 `baiyi-linux-client` 里。

## 1. 用户要什么

用户看完上一轮报告后的原话：

- 「需要有删除选项」——针对「菜单里没有删除节点的入口」。
- 「考虑更充分一些」。
- 「优化这个体验，还需要优化显示界面，优化操作」——针对「60 列终端下状态行会折行」和整体菜单体验。

硬约束（用户持久偏好）：菜单**数字直选**，不要箭头、不要 gum/fzf；全中文；家人会从手机 SSH 客户端登录，窄屏要能用。

## 2. 进度

| 阶段 | 状态 |
|---|---|
| 客户端缺陷修复、M4 真机验收、菜单三轮真机测试与四批修复 | 完成，分支 `Baiyi/bui-c-completion-aa7d30` 已推到 origin，**未合入 `v4`**，等服务端负责人审查 |
| 菜单 v2 设计 | **刚开始就中断**：设计 workflow 在「调研」阶段被停止（会话额度用尽），没有产出设计文档 |
| 菜单 v2 实现、真机验收 | 未开始 |

分支与合并：
- 分支从 `6318b77` 起，51 个 commit。与 `origin/v4`（多出 Hysteria2 鉴权改 http 的 3 个 commit）`git merge-tree` 无冲突；合并树在 baiyi 上跑过 fmt / clippy / `cargo test --workspace` / `scripts/tests/run-all.sh`，全绿（`bui` 700 个、`bui-c` 244 个测试）。
- 推送前把整段历史做了一次脱敏重写：测试与文档里原先出现的真实域名、面板用户名、出口 IP 换成了等长的示例值（`rick-node.example-a.net`、`tizi.example.test`、`示例用户甲`、`示例专用名` 等），列宽断言不受影响。**所以 commit 哈希与会话记录里的不一致**，按 commit 主题找。
- 本地还留着若干子 agent 的临时分支（`opus/*`、`worktree-agent-*`），它们的历史里有真实值，**不要 push**，确认不用后删掉。

baiyi 现场：
- `/usr/local/bin/bui-c` 是本分支最终源码构建（sha256 前缀 `74555d02`），TUN 模式，9 个节点，活动节点是 v3 迁来的 bwg-tizi 直连 HY2。
- 其中 1 个节点指向已下线的临时服务器，探测必失败——正是用户想删却删不掉的那个，适合做删除功能的真机验收对象（验收前先备份 `profiles.json`）。

## 3. 菜单 v2 的设计输入（已收集到的事实）

**v3 客户端有、v4 还没有的**（`git show fc3e757^:b-ui-client.sh`，5810 行）：
- `delete_config()`（约 2995 行）：删除节点。
- `test_proxy()`（约 4240 行）：连接测试。
- 主菜单 `[6] 高级设置`（`show_menu()` 约 5193 行）。
- `import_from_subscription()`（约 3286 行）、`_get_node_display_name()`（约 2382 行）。
- v3 主菜单：`[1] 切换节点 [2] 开启/停止 TUN [3] 导入节点 [4] 服务控制 [5] 连接测试 [6] 高级设置 [7] 一键更新 ★ [8] 卸载 [0] 退出`，两栏数字直选。

**v4 当前主菜单**（数字 0–9 已用满）：`[1] 切换节点 [2] 切到 SOCKS/TUN [3] 导入节点 [4] 服务控制（重启 / 最近 50 行日志） [5] 连接检查 [6] 检查更新 [7] 从 v3 导入 [8] 卸载 [9] 自动更新 开/关 [0] 退出`。要加节点管理，多半得重新编排（例如 [7] 只在有 v3 目录时出现、[9] 并入更新子页）。

**代码里现成的**：
- `Profiles::remove(name) -> bool` 已有，无人调用；`upsert` / `free_name` / `find_same_endpoint` / `active_profile` 可复用（`crates/bui-c/src/profiles.rs`）。
- 子页模板照抄 `cli.rs` 的 `service_menu` 与 `pick_node`：输错原地重问、空行或 `0` 返回、EOF 返回；测试用 `Scripted` + `menu_loop` + `ctx.transcript`。
- 渲染：`menu::display_width`（中文 2 列）、`render_status` / `render_options` / `render_nodes(prof, with_back)` / `render_node_picker`。节点列表已是每节点两行、≤ 80 列。
- 删除活动节点、删除最后一个节点时要停服务：参考 `engine.rs` 的 `stop` 与 `uninstall.rs`。

**设计时必须想清楚的边界**（「考虑更充分」）：
- 删除当前活动节点：先让用户选替换节点，还是切到剩下的第一个，还是停服务？只剩一个节点时怎么办？TUN 正在跑时删除会断网多久？
- 批量删除的输入格式（`1 3 5`、`1-3`）与输错（`1 1 3`、`3-1`、`99`、全角、`0`），确认文案里列出将删除的名字与服务器。
- 从面板再次导入会把删掉的节点加回来：要不要提示，要不要记「已忽略」？
- 撤销：删除前备份一份 `profiles.json`，提供一次撤销？
- 命令行对应：`bui-c delete <名字>...`（`-y` 跳过确认、`--json`），与菜单共用同一实现。
- 并发：`bui-c.timer` 每分钟跑的 `check` 也会写 `profiles.json`/`runtime.json`，目前**没有进程锁**（要加 `flock` 需给根 `Cargo.toml` 的 `nix` 加 `fs` 特性，属服务端共用文件，先与服务端负责人确认；或把 MSRV 1.85 提到 1.89 用 `File::try_lock`）。
- 宽度自适应：状态行在 60 列折行。真机上 `Command::new("stty").arg("size")` 继承终端 stdin 应能拿到列数（待验证；非终端时拿不到要有回落）。长名字按宽度截断加 `…`。
- 可选的体验项（需要做价值判断，标 P0/P1/P2）：节点测速/可用性（帮用户找出该删的死节点）、节点重命名（面板导入的名字形如 `<域名>-hy2-resi`，太长）、节点详情、从面板刷新节点、按服务器分组显示、[6] 更新前先显示版本再确认。

**测速的技术路线（未验证，设计 workflow 正准备在 baiyi 上验证）**：
- 方案 A：`sing-box tools fetch -c <只含出站的临时配置> <探测 URL>`，看它能否指定出站、在 TUN 运行中是否直连节点服务器而不被 TUN 绕一圈。
- 方案 B：临时 sing-box 起 `127.0.0.1` 随机端口 socks 入站、`route.final` 指向该节点出站，`timeout` 包住，经 socks 探测 `generate_204`。
- 两个方案都要：临时配置含凭据，只放 0700 临时目录、不打印、测完删除；确认不影响在跑的 `bui-c.service`；单节点配置应由 `bui-schema::render::client` 提供函数渲染，不在 bui-c 里手拼。

## 4. 建议的做法（照上一阶段跑通的流程）

1. **设计**（上一会话写好的 workflow 在额度用尽前被停止，思路可复用）：
   - 并行调研五路：代码现状、v3 旧菜单、真机截屏与终端宽度探测、测速可行性、同类数字菜单工具（ShellCrash、233boy、fscarmen、x-ui）的节点管理模式。
   - 从三个角度各出一套完整设计：高频操作优先、以节点为中心、安全与窄屏优先。每套含 80 列与 60 列屏幕稿、删除流程、文案表、数据改动、任务拆分。
   - 三名评审打分，合成一版，再从边界安全、真实体验、可实现性三个方向对抗审查后修订。
   - 设计文档放 `docs/superpowers/specs/2026-09-1x-bui-c-menu-v2-design.md`，计划放 `docs/superpowers/plans/`。
2. **实现**：一个任务一个 commit，TDD（先看到失败），每步 `cargo fmt --all -- --check`、`cargo clippy -p bui-c -p bui-schema --all-targets -- -D warnings`、`cargo test -p bui-c` 全绿。改同一批文件（`menu.rs`、`cli.rs`）的任务不要并行。
3. **真机验收**：在 baiyi 上用独立 tmux socket 逐键操作，截屏交给「截屏判定 + 代码判定」两人独立判定，再找回归。
4. **收尾**：更新两份交接手册与 CHANGELOG，推分支，等服务端负责人合入。

## 5. baiyi 上的操作要点

- 构建：本机 macOS 编不了 musl 且 `crates/bui` 因 inotify 编不过。`rsync -az --delete --exclude target --exclude .git --exclude .claude <仓库>/ baiyi:~/b-ui-build/`，然后在 baiyi 上：

```bash
CC_x86_64_unknown_linux_musl=gcc AR_x86_64_unknown_linux_musl=ar cargo build --release --locked --target x86_64-unknown-linux-musl -p bui-c
```

  增量约 17 秒。全量门禁脚本在 `~/gate2.sh`（fmt / clippy / test / bash -n / run-all）。
- 装新二进制：`sudo -n install -m 0755 <产物> /usr/local/bin/bui-c`。
- 真人式测试：`tmux -L buitest new-session -d -s t -x 100 -y 40`，`send-keys -l` 逐字符打，`capture-pane -p` 截屏，收尾 `tmux -L buitest kill-server`。**主人自己的 tmux 会话绝对不能碰。**
- 改 `profiles.json` 的测试前 `sudo -n cp -a /opt/bui-c/profiles.json /root/bui-c-profiles.bak.json`，测完按 sha256 核对恢复。切 SOCKS/TUN 前挂保险丝：`sudo -n systemd-run --quiet --on-active=300 --unit=buitest-revert /usr/local/bin/bui-c mode tun`，验完 `systemctl stop buitest-revert.timer`。
- 沙箱 `BUI_C_BASE=/tmp/x BUI_C_UNIT_DIR=/tmp/y` **只隔离文件、不隔离 systemd**，会 apply 的路径会重启真实服务；首跑的 v3 导入邀请只能答 n。
- 回滚到 v3 客户端（30 秒）：`sudo tar xzf /root/bui-c-v3-backup/hysteria-client-*.tgz -C / && sudo systemctl daemon-reload && sudo systemctl enable --now bui-tun.service`。
- v3 脚本备份 `/usr/local/bin/bui-c.v3` 的 `--version` 会覆盖 `/usr/local/bin/bui-c`，辨认只用 `file`/`sha256sum`。
- ssh 命令串里不要出现能被 `pkill -f` 模式匹配到自己的写法（会把远端 shell 杀掉）。

## 6. 仍未处理、与菜单 v2 可能相关的遗留

- 已装 rc6/rc7 的客户端收不到修复：各 rc 的 manifest `version` 都是 `4.0.0`，`update::run` 只比版本字符串。需要服务端负责人决定（版本号前进，或 manifest 带构建标识）。
- 服务端 `install.sh` 的 `latest_v4_tag` 与客户端安装脚本修掉的是同一个 `tr | awk … exit` SIGPIPE 写法，属服务端负责人的文件。
- 全新主机「安装脚本 → 导入 → 首次 apply 建单元 → 切模式 → 卸载」没在干净的 systemd 主机上真机走过。
- 旧版本经 `--sub` 记下的第三方 https panel 无法从文件本身分辨。
