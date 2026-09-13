# bui-c 菜单 v2（删除节点、连接检查补齐、清屏与窄屏）交接手册

> 2026-09-13 晚，上一会话额度用尽时写。先读 [HANDOVER-bui-c.md](HANDOVER-bui-c.md) 了解客户端全貌，再读这份。
> 公开文档只用别名与示例值：`bwg-rick`、`bwg-tizi`（服务器）、`baiyi`（国内 Linux 客户端真机，SSH 别名）；示例域名 `rick-node.example-a.net`、`tizi.example.test`。真实值只在本机 Claude 记忆里。

**给下一个 agent 的一句话：** 先读本手册第 3、4 节和计划 `docs/superpowers/plans/2026-09-13-bui-c-menu-v2.md`，从 T7（删除节点）按 brief 开工、T11 分支先送审再合并，每个任务都走「实现 → 审查 → 修复」闭环，推送前用敏感值清单 grep 一遍，永远不要 push `v4`、`main` 以及 `opus/*`、`worktree-agent-*` 分支。

## 1. 用户要什么

- 原话：「bui-c 数字菜单要能删除节点，要考虑更充分，还要优化显示界面和操作，包括 60 列终端下状态行折行的问题」。
- 追加：[5] 连接检查要对照 v3.6.2 补齐（原来只有一句话）；回菜单要清屏、操作要连贯。
- 已拍板：删掉的节点记进墓碑，重新导入时先跳过再问要不要加回；主菜单 `[1] 切换 [2] 模式 [3] 导入 [4] 服务控制 [5] 连接检查 [6] 删除节点 [7] 更新与维护 [8] 卸载 [9] 节点测速 [0] 退出`。
- 硬约束：
  - 菜单只数字直选，不要箭头、gum、fzf；文案全中文；家人用手机 SSH 登录，40 列左右也要能用。
  - 凭据不进命令行参数和日志；不改根 `Cargo.toml`（服务端共用）。
  - 不用 emoji（回复、文档、提交、注释、测试数据都不写 emoji 字面量，要测宽度就写 `\u{…}`）；需要图标时只用 SVG。菜单现有的 ★ ☆ ● ○ ✓ ✗ ▸ 是文本字形，用户确认保留。
  - 仓库公开，`/api/sub/<用户名>` 免鉴权：代码、文档、commit 只用示例值。

## 2. 设计与计划（已定稿，都在仓库里）

- spec：`docs/superpowers/specs/2026-09-13-bui-c-menu-v2-design.md`。§0.2 的 R1–R16 是规范性覆盖，和正文冲突时以 R 条为准。
- 计划：`docs/superpowers/plans/2026-09-13-bui-c-menu-v2.md`，任务表在 spec §13.0。顺序：T1→T2→T3→T4→T5∥T6→T7→T7b→T8→T9→T10→T11→T12a→T12b→T12c →**真机验收第一轮**→ T13→T15→T16→T17→T18→T19；T14 随时可做。
- 关键技术决定（都已写进 spec）：进程锁用 `nix::fcntl::Flock`（现有 features 就够，不改 Cargo）；终端尺寸用 stdout 上一次 `ioctl(TIOCGWINSZ)`（全 crate 唯一的 `unsafe`）；宽度按「容量口径」算（歧义字符算 2 列），固定文案 ≤ 59 列、40 列可辨认；测速走「临时 sing-box + 带随机认证的 socks 入站」，必须 `route.auto_detect_interface: true`。

## 3. 分支与进度

### 3.1 分支

| 分支（都已推到 origin） | 内容 | 状态 |
|---|---|---|
| `Baiyi/bui-c-menu-node-deletion-e6aee1` | 主线，基于 `38e5d79`（当时的 `origin/v4`） | T1–T6、T8、T14 已完成并审查通过；门禁全绿（macOS 与 baiyi Linux，`bui-c` 327 个测试） |
| `Baiyi/bui-c-menu-v2-t11-nettest` | T11 连接检查，一个提交 `f417b59`，基于主线的 `4f3a854` | 已实现、门禁全绿（317），**未审查**；合进主线时 `cli.rs` / `menu.rs` 会有冲突要手工合 |

之前的 `Baiyi/bui-c-completion-aa7d30`、`Baiyi/bui-c-update-sha256` 已由服务端会话合进 `v4`。本机若还有 `opus/*`、`worktree-agent-*` 旧分支，它们历史里有真实值，**绝不能 push**。

### 3.2 主线提交（按顺序）

| 提交 | 内容 | 审查 |
|---|---|---|
| `4c7be25` | fix(schema)：`node_uri` 无冒号分支只解码一次用户名（**跨 crate**） | 已审 |
| `7014682` `088182b` `11e7167` `487debd` `48fff10` `70f6600` | spec 与计划，以及执行中的对齐修订 | — |
| `a66d2ec` + `dadf300` | T1 宽度工具（`display_width` / `budget_width` / `sanitize` / 截断） | 通过 |
| `1bca2a9` | T2 `term_size`，非终端回落 80×24 | 通过 |
| `e079950` | T3 按宽度排版，宽度守门表 `screens()` | 通过 |
| `c84f5a6` | T14 测速探测配置 `node_outbound` / `probe_config`（**跨 crate**，标签按下标生成） | 通过 |
| `3067043` + `e4ab2f0` | T4 清屏重画、「上次」行、输错原地重问、菜单不认 `-y`；修复：v3 邀请结果不被清屏抹掉等 | 通过 |
| `4f3a854` | T5 `Net` 的 `text_via` / `probe` / `download_via`，`Net: Sync` | 通过 |
| `e561f50` | T1/T3 审查遗留：窄屏列表只出 label 等 | **未单独审**，最终审查点名看 |
| `63b184e` | T8 v3 导入邀请只在还没迁移时出现 | 通过 |
| `57b3d91` | T4 遗留：TUN 没起来时「上次」行不再说切换成功 | **未单独审**，最终审查点名看 |
| `58ae637` | T6 批量编号解析、确认输入解析、删除确认块（含命令行变体 `delete_confirm_cli`） | 通过 |

### 3.3 还没做的

T7（删除节点与 `bui-c delete`，核心）、T7b（墓碑）、T9（菜单重排与 [7] 子页）、T10（检查更新先确认）、T11 的审查与合并、T12a/b/c（进程锁、更新拆锁、pending.json 收敛）、真机验收第一轮、T13、T15–T18（P1：卸载清单、Sys 扩展、[9] 测速、巡检连续失败行、`bui-c test`）、T19（文档与 CHANGELOG）、最终整分支审查。

T7 在额度用尽前刚派出就停了，没有留下任何代码改动。

## 4. 计划之外、执行中追加的要求（开工前必读）

计划文件里的 brief 是开工前写的，下面这些是后来的审查与裁定追加的，派任务时要一起交代：

- **T7**
  - 用 T6 的接口，确切签名看 `menu.rs` 的 `delete_confirm` / `delete_confirm_cli` / `render_delete_picker` / `parse_selection` / `parse_confirm` / `wrap`。
  - `delete_confirm` 的 `rows` 传 `ctx.rows()` 原值（函数内部已减 2）。
  - 断言 `needs_word == !matches!(plan.kind, PlanKind::Passive)`。
  - 补测试：没有测速结果时，「剩下的节点都在…」这一行出现当且仅当 `default_to` 走了回落分支。
  - Empty 形态要输 yes，不能用「这一步只认 y」的说法。
  - 越界编号 `Pick(len)` 报错时回显用户原始输入（净化、截短），不打 `i+1`。
  - `bui-c delete` 用 `delete_confirm_cli`，替换目标用 `--switch-to`。
  - `SelError::message` 在 40 列用 `menu::wrap(&e.message(len), 2, width)` 折行。
  - 删除后的「上次」行来自 `summary(r, width)`，名字按 R6 中间截断（与 `fit_name_in_last` 同规则）。
  - `delete_menu` 本任务不接进主菜单（T9 接）。
  - `lock::acquire` 先做桩，T12a 换真的。
  - 墓碑留给 T7b。
  - 新出现的整屏加进 `screens()`。
- **T7b**
  - 菜单 [3] 的「上次」行必须是导入结果本身（`导入 N 个新节点`、`失败：…`），不是结果前面的附加提示。
  - 顺手把 T4 在 `outcome_since` 里按行首 `CURRENT_NODE_HEAD` 豁免的写法，改成由打印方直接报「有没有附加行」。
- **T9**
  - 把「有新版」形态的主菜单（左栏 `[7] 更新与维护 ★`）加进 `screens()`，40 列要放得下。
  - 跳过 v3 导入后的提示现在只活在「上次」行里：换 §11.1 新文案时把菜单入口放前面，40 列放得下。
  - 删除页的过渡提示实际上限是 37 列（加 2 列缩进 ≤ 39），R1 写的「≤ 39」要改。
  - brief 里的行号已过时，四处文案按内容找：`offer_v3_import` 的跳过提示及其断言；服务控制没有单元时的引导及测试 `menu_service_control_without_units_points_to_install`。
- **T10**：[7]→[1] 检查更新成功时，「上次」行现在是「manifest …（来源 …）」，要改成人话。
- **T11**（分支已实现，审查时逐条判断）
  - GitHub 探测地址改成了 `https://github.com/robots.txt`：spec 写的是根路径，但根路径是每日自更新 manifest 地址的前缀，会让「timer 不访问检测站」的守门测试必红。建议接受并改 spec §6.2。
  - SOCKS 的 DNS 行失败用 ○ 不用 ✗（不计分项）；汇总写「YouTube 没通，不计分」。
  - **Google 那句判断带缩进 60 列，60 列终端会折行，要改到 ≤ 59**。
  - 修复路径会多探测一次（最坏多等约 8 秒），T12a 拆段后去掉。
  - 检查失败时，下一步小菜单会读走管道输入的一行。
  - 40 列日志页时间戳占 25 列，消息只剩不到 12 列，建议窄屏缩短时间戳。
  - `RealSys` 的 `tcp_listening` / `resolve` 只能真机验。
- **T12a**：依赖 T7 的删除流程和锁桩，不能提前。
- **T13**：卸载成功要退出菜单（`Outcome::Exit`）。
- **T16**：给端口分配补「互不相同」的单元测试（`sing-box check` 不绑端口，查不出重复）；更多连接失败归 `Other("connect")`，结果页显示「✗ 失败」。

## 5. 留给最终审查的小问题

- `FakeSys::term_size` 按契约归一（列 0 → None），删掉 `width` 里的过滤与 `(0,30)` 断言。
- `char_width` 是手挑的宽字子集（文档已写明没收的段），带 U+FE0F 的 emoji 按 1+1 算。
- `wrap` 的行首禁则只守了硬折，没守折点路径：`wrap_pieces("备注（家里人用的，）别删", 18, 18)` 会得到以「）」开头的行；`fits` 要再检查下一行开头不在 `NO_LINE_START` 里。
- spec 残留：§5.3 输入表与 §11.3 的「`y`、`yes` → 执行」「这一步只认 y」在 Empty 形态下与 R1 冲突；§10.3 的 `ConfirmInput` / `parse_confirm` / `render_delete_picker` / `render_delete_confirm` 签名是旧的。
- TUN 没起来时「上次」行的文案裁定：后缀缩短为「，但 TUN 没起来」；名字可用不到 8 列时不写名字，改「已切换节点，但 TUN 没起来」；「已是当前节点」加 TUN 没起来同样处理。
- 40 列下切换结果行本身 47 列（一闪即被清屏）。
- T14 的 golden 是四份单行常量，难审（已有 `#[ignore]` 重录用例）。
- `e561f50`、`57b3d91` 两个小修没单独审过。

## 6. 真机验收要核对的（第一轮在 T12c 之后）

- sing-box 对坏域名、坏端口实际回的 SOCKS 应答码；如果 sing-box 放弃拨号比 bui-c 的计时器早并回 0x01，超时会被判成「解析失败」（[5] 第 4、5 行，8 秒 / 6 秒，风险最大）。
- 探测不跟重定向：YouTube 拿到 3xx 算通。
- 下载在 5 秒到点时显示结果，不是报错。
- 管道输入时停顿不读：`printf '5\n0\n' | sudo bui-c`。
- `tcp_listening` / `resolve` 的真实行为。
- 40 / 60 / 80 / 100 列 tmux 逐键操作截屏；手机 SSH 客户端与歧义宽度终端的截屏要用户提供（spec R9 ②）。

## 7. 怎么接着干

- **流程**
  - 用 superpowers 的 subagent-driven-development：每个任务派一个执行者，完成后派审查员，拿到 spec 符合度和代码质量两个结论。Critical / Important 必须修完再复审；Minor 记账，最后统一处理。
  - brief 用插件脚本 `task-brief <计划文件> <任务号>` 从计划里抽；审查包用 `review-package <基> <头>` 生成。
  - 执行者和审查员都用 Opus 档。
- **并行**
  - 改的文件不相交的任务可以各开一个 worktree 并行做（本会话 T5、T6、T8、T11、T14 都这样做过），完成后 cherry-pick 回主线，多个提交先压成一个。
  - 派到别的 worktree 的 agent，Edit 工具可能拒写，要先 `EnterWorktree(path=…)` 切进去。
  - 审查员一律用 `git show <SHA>:<路径>` 读代码，免得读到正在变的工作区。
- **门禁**（每个提交）：`cargo fmt --all -- --check`、`cargo clippy -p bui-c -p bui-schema --all-targets -- -D warnings`、`cargo test -p bui-c`（碰了 bui-schema 再加 `cargo test -p bui-schema`）。这三条在 macOS 能直接跑；`crates/bui` 在 macOS 编不过，不要跑 `--workspace`。
- **Linux 门禁**（每合进几个任务跑一次）：
  - `git archive <SHA> | tar -x -C <临时目录>`，再 `rsync -az --delete --exclude target --exclude .git --exclude .claude <临时目录>/ baiyi:~/b-ui-build-menu/`。用单独的目录，不碰别人可能在用的 `~/b-ui-build`。
  - 在 baiyi 上跑 `CC_x86_64_unknown_linux_musl=gcc AR_x86_64_unknown_linux_musl=ar cargo check --locked --target x86_64-unknown-linux-musl -p bui-c`，再跑 clippy 和 test。
- **推送前**
  - 用本机敏感值清单（真实域名、面板用户名、IP，路径记在 Claude 记忆 `bui-c-menu-v2-pending` 里，不进仓库）对 `git log -p <基>..<分支>` 做 `grep -c -i -F -f`，结果必须是 0。
  - 清单文件里的空行要先去掉，否则空模式会匹配一切。
- **收尾**
  - 更新本手册、[HANDOVER-bui-c.md](HANDOVER-bui-c.md) 和 `CHANGELOG.md`，推分支。
  - 把跨 crate 的 `4c7be25`、`c84f5a6` 连同分支交给服务端会话（名字 `bui`，用 SendMessage 按名字发）审查。
  - 合入 `v4` 前先与当时的 `origin/v4` 合并或变基。

## 8. baiyi 真机要点

- **tmux**
  - 真人式测试一律用独立 socket：`tmux -L buitest new-session -d -s t -x 100 -y 40`。
  - 用 `send-keys -l` 逐字符打，`capture-pane -p` 截屏，收尾 `tmux -L buitest kill-server`。
  - **主人自己的 tmux 会话绝对不能碰。**
- **动配置前**
  - 改 `profiles.json` 之前先备份：`sudo -n cp -a /opt/bui-c/profiles.json /root/bui-c-profiles.bak.json`，测完按 sha256 核对恢复。
  - 切 SOCKS / TUN 之前先挂保险丝：`sudo -n systemd-run --quiet --on-active=300 --unit=buitest-revert /usr/local/bin/bui-c mode tun`，验完 `systemctl stop buitest-revert.timer`。
- **沙箱**：`BUI_C_BASE` / `BUI_C_UNIT_DIR` 只隔离文件，不隔离 systemd，会 apply 的操作仍会重启真实服务。
- **v3 备份**：`/usr/local/bin/bui-c.v3`（及其它 v3 备份）的任何子命令都不要跑，它会覆盖 `/usr/local/bin/bui-c`；辨认只用 `file` / `sha256sum`。
- **pkill**：ssh 命令串里不要出现能被 `pkill -f` 模式匹配到自己的写法。
- **删除功能的验收对象**：9 个节点里有 1 个指向已下线的临时服务器，探测必失败，正好用来验收删除；验收前先备份 `profiles.json`。

## 9. 需要用户或服务端负责人决定的

- 跨 crate 提交 `4c7be25`（`node_uri`）与 `c84f5a6`（`probe_config` / `node_outbound` / `ProbeTarget`，C1 契约新增）要服务端负责人审查。
- 公开仓库历史里曾出现过真实值：凭据轮换和历史重写，要服务端负责人与用户一起定。
- 本地分流规则功能没做。
- T11 的 GitHub 探测地址改为 `robots.txt`，建议接受。
- 手机与歧义宽度终端的实机截屏，要用户提供。
