# bui-c 菜单 v2（删除节点、连接检查补齐、清屏与窄屏）交接手册

> 2026-09-13 晚写，上一会话额度用尽时；2026-09-15 按主线 `632b475` 与第一轮真机验收更新。先读 [HANDOVER-bui-c.md](HANDOVER-bui-c.md) 了解客户端全貌，再读这份。
> 公开文档只用别名与示例值：`bwg-rick`、`bwg-tizi`（服务器）、`baiyi`（国内 Linux 客户端真机，SSH 别名）；示例域名 `rick-node.example-a.net`、`tizi.example.test`、`temp.example.test`（已下线的临时服务器）、`panel.example.com`（面板）；示例账号 `alice` / `bob`；示例 IP `203.0.113.x`。真实值只在本机 Claude 记忆与台账里。

**给下一个 agent 的一句话：** P0 已全部进主线并审查收口——T7–T12c、Ttoken、T5fix、T14fu / T14fu2 都走完「实现 → 审查 → 修复」闭环，T12b 第 3 轮的 Important 由 lead 裁定再修最后一次（`632b475`），范围钉死的第 4 轮复审 **Approved**；baiyi 第一轮真机验收**删除流程与 [5] 连接检查通过**，侵入性的几项还没做（§6）。下一步：T13 → T15–T18 → 最终整分支审查；这批进 **4.0.1**，等 `v4.0.0` 打 tag 之后再合进 `v4`（§9）。旧手册里「派到别的 worktree 的 agent 先 `EnterWorktree`」这条是错的，实际跑通的并行做法见 §7。推送前照旧用敏感值清单 grep，永远不要 push `v4`、`main` 以及 `opus/*`、`worktree-agent-*` 分支。

## 1. 用户要什么

- 原话：「bui-c 数字菜单要能删除节点，要考虑更充分，还要优化显示界面和操作，包括 60 列终端下状态行折行的问题」。
- 追加：[5] 连接检查要对照 v3.6.2 补齐（原来只有一句话）；回菜单要清屏、操作要连贯。
- 已拍板：删掉的节点记进墓碑，重新导入时先跳过再问要不要加回；主菜单 `[1] 切换 [2] 模式 [3] 导入 [4] 服务控制 [5] 连接检查 [6] 删除节点 [7] 更新与维护 [8] 卸载 [9] 节点测速 [0] 退出`（`[9]` 在 T16 落地前不显示，按 9 只给一句「自动更新开关在 [7] 更新与维护 → [2]」）。
- 硬约束：
  - 菜单只数字直选，不要箭头、gum、fzf；文案全中文；家人用手机 SSH 登录，40 列左右也要能用。
  - 凭据不进命令行参数和日志；不改根 `Cargo.toml`（服务端共用）。
  - 不用 emoji（回复、文档、提交、注释、测试数据都不写 emoji 字面量，要测宽度就写 `\u{…}`）；需要图标时只用 SVG。菜单现有的 ★ ☆ ● ○ ✓ ✗ ▸ 是文本字形，用户确认保留。
  - 仓库公开，`/api/sub/<token>`（rc12 之前是 `/api/sub/<用户名>`）免鉴权：代码、文档、commit 只用示例值。

## 2. 设计与计划（已定稿，都在仓库里）

- spec：`docs/superpowers/specs/2026-09-13-bui-c-menu-v2-design.md`。§0.2 的 R1–R16 是规范性覆盖，和正文冲突时以 R 条为准。
- 计划：`docs/superpowers/plans/2026-09-13-bui-c-menu-v2.md`，任务表在 spec §13.0。顺序：T1→T2→T3→T4→T5∥T6→T7→T7b→T8→T9→T10→T11→T12a→T12b→T12c →**真机验收第一轮**→ T13→T15→T16→T17→T18→T19；T14 随时可做。执行中插进来的任务：T11fix、T14fu / T14fu2（跨 crate）、T5fix（SOCKS 应答码）、Ttoken（服务端 rc12 订阅 token 化的三处接缝）。
- 关键技术决定（都已写进 spec）：
  - 进程锁用 `nix::fcntl::Flock`。`Flock` 在 nix 的 `fs` 特性后面，`crates/bui-c/Cargo.toml` 已把 `nix.workspace = true` 改成 `nix = { workspace = true, features = ["fs"] }`（`d69eb49`），根 `Cargo.toml` 与 `Cargo.lock` 都没动；spec §0.1 / §8.3 / §10 末已随 `722e303` 改过。原先写的「现有 features 就够，不改 Cargo」是错的：整个 workspace 一起构建靠 `crates/bui` 开的 `fs` 被 feature 统一带过来才「看起来能编过」，`cargo build -p bui-c` 单独构建（baiyi 上的 musl 发布构建正是这样）不加就是 E0432。
  - 终端尺寸用 stdout 上一次 `ioctl(TIOCGWINSZ)`（全 crate 唯一的 `unsafe`）；宽度按「容量口径」算（歧义字符算 2 列），固定文案 ≤ 59 列、40 列可辨认；测速走「临时 sing-box + 带随机认证的 socks 入站」，必须 `route.auto_detect_interface: true`。

## 3. 分支与进度

### 3.1 分支

| 分支 | 内容 | 状态 |
|---|---|---|
| `Baiyi/bui-c-menu-node-deletion-e6aee1` | 主线，基于 `38e5d79`（当时的 `origin/v4`） | T1–T12c、T14、Ttoken、T5fix、T14fu / T14fu2 已完成并审查收口；origin 上停在 `632b475`（推前 `742865d..632b475` 敏感值预检零命中，推的是 `rev-parse` 取出的精确 SHA） |
| `Baiyi/bui-c-menu-v2-t11-nettest` | T11 连接检查（`f417b59`） | 已由 `bcbf133` 合进主线，分支与本机 worktree 可以删 |

- 之前的 `Baiyi/bui-c-completion-aa7d30`、`Baiyi/bui-c-update-sha256` 已由服务端会话合进 `v4`。本机若还有 `opus/*`、`worktree-agent-*` 旧分支，它们历史里有真实值，**绝不能 push**。
- 本机 `.claude/worktrees/` 下还有约 25 个流水线 worktree（`wf_dc30752a-599-*`、`wf_978e3279-25d-*`、`agent-*`、`sdd-t6-delete-confirm` 等），全部已集成，可以清；里面没有真实值（都从主线 reset 出来），但照旧不 push。
- `origin/v4 = origin/main = f5571a9`（tag `v4.0.0-rc12`）。`git merge-tree --write-tree 632b475 f5571a9` **干净**（含 T12b / T12c 全部、`update.rs` 两次改动）。
- 门禁：
  - macOS 主线 @`632b475`：`cargo test -p bui-c` 524 passed，`cargo test -p bui-schema` 125 passed / 1 ignored；fmt / clippy 零告警。
  - baiyi Linux @`632b475`（`git archive` 导出到 `~/b-ui-build-menu`）：musl `cargo build --release --locked -p bui-c` **单独构建成功**（实证 `fs` 特性修好了单独构建）；fmt 0、clippy 零告警、`cargo test -p bui-c` 524 passed、`cargo test -p bui-schema` 全过；PATH 上有 sing-box 1.14，内核用例（含 `probe_config` 的真实 `sing-box check`）真跑了，没有 skip。

### 3.2 主线提交（按顺序）

| 提交 | 内容 | 审查 |
|---|---|---|
| `4c7be25` | fix(schema)：`node_uri` 无冒号分支只解码一次用户名（**跨 crate**） | 已审 |
| `7014682` `088182b` `11e7167` `487debd` `48fff10` `70f6600` | spec 与计划，以及执行中的对齐修订 | — |
| `a66d2ec` + `dadf300` | T1 宽度工具（`display_width` / `budget_width` / `sanitize` / 截断） | 通过 |
| `1bca2a9` | T2 `term_size`，非终端回落 80×24 | 通过 |
| `e079950` | T3 按宽度排版，宽度守门表 `screens()` | 通过 |
| `c84f5a6` | T14 测速探测配置 `node_outbound` / `probe_config`（**跨 crate**，标签按下标生成；`0b85284` 已把 `node_outbound` 收回私有，C1 只剩 `probe_config` / `ProbeTarget`） | 通过 |
| `3067043` + `e4ab2f0` | T4 清屏重画、「上次」行、输错原地重问、菜单不认 `-y`；修复：v3 邀请结果不被清屏抹掉等 | 通过 |
| `4f3a854` | T5 `Net` 的 `text_via` / `probe` / `download_via`，`Net: Sync` | 通过 |
| `e561f50` | T1/T3 审查遗留：窄屏列表只出 label 等 | **未单独审**，最终审查点名看 |
| `63b184e` | T8 v3 导入邀请只在还没迁移时出现 | 通过 |
| `57b3d91` | T4 遗留：TUN 没起来时「上次」行不再说切换成功 | **未单独审**，最终审查点名看 |
| `58ae637` | T6 批量编号解析、确认输入解析、删除确认块（含命令行变体 `delete_confirm_cli`） | 通过 |
| `6437894` | 交接文档（上一会话末） | — |
| `e875cec` | T7 删除节点与 `bui-c delete`：快照比对、预检、先数据面后落盘、失败回滚、删光拆数据面（`delete.rs` / `lock.rs` 新建） | r1 NeedsFixes（4 Important：I1 UFW / runtime 挪到 save 之后、I2 命令行文案按 §5.4、I3 回滚后 TUN 未起要说实话、I4 Empty save 失败裸 `?`） |
| `004b61d` | T7 审查修复（I1–I4 + M1–M8） | 按审查清单落地；**没有单独复审**，最终审查点名看 `roll_back` / `delete_nodes` Empty 分支 |
| `e80b7bd` | T7 收尾（lead 改：拆数据面短摘要在所有分支下为真；命令行删光句留三位数余量） | 同上 |
| `f417b59` → `bcbf133` | T11 连接检查补齐 v3.6.2（分支实现 + merge commit；9 处冲突全「两边都留」，4 处手工） | 分支审查 NeedsFixes（3 Important）→ 修复见下 |
| `c44ce8c` | T11 修复：Google 判断缩到 59 列、窄屏日志压时间戳、裸 ESC 转义、spec 对齐裁决（robots.txt、○） | r1 Approved |
| `3ecad85` `02cf87e` | T11 修复第 2、3 轮（纯 spec 措辞） | r2 / r3 Approved；r3 M1'' fix_now 未做（§2.5 把 [4]→[1] 的 10 行日志标成「重启失败后」，实际是「重启成功但 bui-tun 没起来」） |
| `0b85284` | T14fu（跨 crate）：`node_outbound` 收回私有、`ProbeTarget` 手写脱敏 Debug、probe 配置钉住 `ipv4_only`、`node_uri` 变异测试 | r1 Approved；**服务端审过**：1 Important（同源断言自指）→ T14fu2 |
| `b1d99ae` | T14fu 修复（脱敏 Debug 钉住 host 不露、变异断言钉住拒绝原因） | 并入 T14fu2 审查 |
| `742865d` | T7b 墓碑：删掉的节点记进墓碑，重新导入先跳过再问 | r1 NeedsFixes（**Critical** 删光后重新导入走不到墓碑那一问；Important 活节点与墓碑同 key 被当删过） |
| `5bb9ee7` | T7b 修复 r1（`save_import` 拆成 `store_import` + `apply_import`，墓碑只挡新节点） | r2 NeedsFixes（Important：v3 路径全部命中墓碑 + 残留 v3 单元 → 报「没有可用的活动节点」） |
| `50b1106` | T7b 修复 r2（`import_v3::run` 早退条件、邀请答 y 走 `menu_import_v3`、多条粘贴答 y 摘要照计数；集成时按键改 `[7]→[3]`） | r3 Approved（M7–M10 记账） |
| `9e0b179` | T5fix（跨调研）：SOCKS 应答 0x01 归「连不上」而非「域名解析失败」，失败文案带耗时 | r1 NeedsFixes（spec §11.5 文案表没跟上；耗时接线无 run 级测试） |
| `3b7d70e` | T5fix 修复 | r2 Approved（spec 测速章节仍承诺「✗ 解析失败」的矛盾 → T16） |
| `7665f19` | T14fu2（跨 crate）：同源断言改成与 tun 主配置逐字段比对、`probe_config` 文档写明返回值含明文凭据、Debug 字段改名 `label`、HANDOVER 两处 | r1 Approved（2 Minor） |
| `594c0c3` | T14fu2 修复（Reality fixture 对齐 golden、同源测试逐字节挂到 golden 常量） | r2 Approved |
| `8531dce` | **T9 主菜单重排：[6] 删除节点接进主菜单、[7] 更新与维护子页、旧键过渡提示**（用户可见的那一步） | r1 Approved（F1 [7]→[2] 锁外读写 → T12a 已做；F2 `AUTO_UPDATE_MOVED` 40 列上次行丢「→ [2]」；F3 spec 与 R1/R6 脱节；F5 逐字节钉整页；F6 [7]→[1] 标「检查更新」却直接更新 → T10 已改） |
| `102d22a` | T9 修复（`Runtime.update_available` 文档注释的旧菜单编号） | 纯注释，不复审 |
| `d69eb49` | **T12a 进程锁 `/run/bui-c.lock`**：改配置的路径互斥、巡检遇锁跳过、`RealSys::write` 临时文件带 pid、T11 压过来的 A–D（修复不重复探测、端口失败真重启、预算改回 79 秒、等就绪在锁内）；改 `crates/bui-c/Cargo.toml` nix 加 `fs` | r1 NeedsFixes（Important：[6] 删除锁等不到时停顿页不说原因） |
| `722e303` | T12a 修复（`wait_for_lock`、`LOCK_BUSY_SHORT`、巡检 Busy 文案两种情形、spec 三处） | r2 Approved（M8 fix_now 未做：spec R2 / §8.3 三处仍叫 `Lock`；M9 `Err(e)` 分支无测试；M10 61 列） |
| `b6e4e08` | **Ttoken**：404 不再说「面板没有节点接口」+ 出路、`--user -` / `--sub -` 读 stdin、profile 名不露 token | r1 Approved（fix_now ×2） |
| `549f796` | Ttoken 修复（缺来源改用法错误退出码 2；钉住 `--sub` 经 `/api/nodes` 时记 panel） | r2 Approved（M5 / M6 fix_now 未做：失败路径没断言 transcript 不含 token；命令行「面板不认链接」退出码没钉 1） |
| `96a2324` | **T10 检查更新先显示版本再确认**：`SelfReason` 五种原因、`build_differs` 参数化、`Report` 加 4 字段、`maint_check_update` / `maint_install_update`、`Runtime.update_version` | r1 NeedsFixes（Important：检查成功但 runtime 写不进时 `?` 冲出菜单） |
| `6fabbfd` | T10 修复（兜住 CheckUpdate 的 Err；装好后写 runtime 失败只记 warn） | r2 Approved（#B fix_now 未做：`check_only_verdict` 内核行与 `kernel_local` None 无断言；#C / #D 留 T12b；#E 子页在 MissingAsset / Unreadable 后显示「已是最新」要先定 spec） |
| `0dd9af5` | **T12c pending.json 与中断收敛**：`Paths::pending()`、`needs_converge` / `converge`、进菜单与巡检两条收敛路、`Runtime.converge_failed` / `last_converge` | r1 NeedsFixes（3 Important：`converge_failed` 原因消除后不重试；删光拆到一半的停顿页说「节点都还在」；teardown 按 active 判与 R2 不符） |
| `5ad6475` | T12c 修复（`converge_key` 纳入内核 / is-active / 单元 / config；`TEARDOWN_HALFWAY`；`tidy_for` 按 profiles 为空判 teardown） | r2 Approved（M1 / M2 fix_now 未做：`nodes_without_an_active_one_are_not_torn_down` 没断言 pending 删掉；halfway 段夹具 is-active 翻回 0 跑的不是真实恢复路；M4 收敛放弃后巡检每分钟 restart 不存在的单元） |
| `a64da6c` | **T12b update 拆成锁外 fetch 与锁内 install，只在有活动节点时重启（D16）**；删掉 T12a / T10 的三处外层锁；[7]→[1]「变了再问」 | r1 NeedsFixes（Important：`self_sha` 在下载之后取样，闸是空的） |
| `a246f39` | T12b 修复 r1（取样点挪到 fetch 开头；`Staged` Debug 补 `self_sha`；`LandsDuringDownload` 包装 Sys） | r2 NeedsFixes（Important：跳过后 ★ 重亮、`last_update_at` 记成刚更新过） |
| `d5511ec` | T12b 修复 r2（跳过时按盘上现状重算 `self_reason` / `kernel_outdated`；`counts_as_update`；三个入口各一条用例） | r3 NeedsFixes（达流水线上限 → lead 裁定再修最后一次、范围钉死）：I1 别处装的**不是** manifest 那份时，菜单结果行仍「没有需要更新的」而 ★ 挂着，`bui-c update` 退出 0 且输出与已是最新相同，且有一条测试把这个矛盾钉成了期望；M1–M8 记账 |
| `632b475` | T12b 修复 r3（lead 自己 cherry-pick）：菜单走现成的 `UPDATE_CHANGED`「重新显示再问」；`bui-c update` 同情形报「下载期间已被别的操作换过，这次没装，再跑一次 bui-c update」（57 列）退出 1；改写那条钉矛盾的用例；spec §8.1 加一条 | **r4 Approved**（范围钉死，只核 I1 与回归；0 Critical / 0 Important / 3 Minor，见 §5） |

### 3.3 还没做的

- T13（卸载清单，成功要 `Outcome::Exit`）、T15–T18（P1：Sys 扩展、[9] 节点测速、巡检连续失败行、`bui-c test`）、最终整分支审查。
- 第一轮真机验收没做的几项（§6 末「未做」）；P1 一轮真机验收（T18 之后）。
- 第一轮真机截屏已由独立判定员复核：**通过**（0 严重 / 0 一般 / 4 轻微，见 §6 末与 §5 末）。下一轮真机验收要补上 §5 末「验收方法缺口」那一条。
- 合进 `v4`：等 `v4.0.0` 打 tag（§9）。

## 4. 计划之外、执行中追加的要求（逐条标状态）

计划文件里的 brief 是开工前写的，下面这些是后来的审查与裁定追加的。截至 `632b475` 的状态标在每条后面。

- **T7**（13 条**全部落地并有测试**，T7 r1 §C 组逐条核过）
  - 用 T6 的接口，确切签名看 `menu.rs` 的 `delete_confirm` / `delete_confirm_cli` / `render_delete_picker` / `parse_selection` / `parse_confirm` / `wrap`。
  - `delete_confirm` 的 `rows` 传 `ctx.rows()` 原值（函数内部已减 2）。
  - 断言 `needs_word == !matches!(plan.kind, PlanKind::Passive)`。
  - 补测试：没有测速结果时，「剩下的节点都在…」这一行出现当且仅当 `default_to` 走了回落分支。
  - Empty 形态要输 yes，不能用「这一步只认 y」的说法。
  - 越界编号 `Pick(len)` 报错时回显用户原始输入（净化、截短），不打 `i+1`。
  - `bui-c delete` 用 `delete_confirm_cli`，替换目标用 `--switch-to`。
  - `SelError::message` 在 40 列用 `menu::wrap(&e.message(len), 2, width)` 折行。
  - 删除后的「上次」行来自 `summary(r, width)`，名字按 R6 中间截断（与 `fit_name_in_last` 同规则）。
  - `delete_menu` 本任务不接进主菜单（T9 接）——**T9 已接**。
  - `lock::acquire` 先做桩，T12a 换真的——**T12a 已换成真锁**。
  - 墓碑留给 T7b——**T7b 已做**。
  - 新出现的整屏加进 `screens()`。
- **T7b**
  - 菜单 [3] 的「上次」行必须是导入结果本身（`导入 N 个新节点`、`失败：…`），不是结果前面的附加提示——**已落地**（r3 断言 `上次：导入 2 个新节点，共 2 个`）。
  - 顺手把 T4 在 `outcome_since` 里按行首 `CURRENT_NODE_HEAD` 豁免的写法，改成由打印方直接报「有没有附加行」——**T7b 报告未点名，最终审查核一下**。
- **T9**
  - 把「有新版」形态的主菜单（左栏 `[7] 更新与维护 ★`）加进 `screens()`，40 列要放得下——**已做**（主菜单 ★ 形态由 T9 加；T10 的 `every_line_fits_by_budget` 又加了 update-page / ask / last 三类）。
  - 跳过 v3 导入后的提示现在只活在「上次」行里：换 §11.1 新文案时把菜单入口放前面，40 列放得下——**已做**（`V3_SKIPPED`）；但 `AUTO_UPDATE_MOVED` 在 40 列上次行丢「→ [2]」（T9 r1 F2，记账）。
  - 删除页的过渡提示实际上限是 37 列（加 2 列缩进 ≤ 39），R1 写的「≤ 39」要改——**spec 未改**（T9 r1 F3，记账）。
  - brief 里的行号已过时，四处文案按内容找——**已做**。
- **T10**：[7]→[1] 检查更新成功时，「上次」行改成人话——**已做**（`maint_check_update_yes_installs_and_the_last_line_is_plain_words`）。
- **T11**（7 条）
  - GitHub 探测地址改成 `https://github.com/robots.txt` 并改 spec——**已做**（spec §6.2 / §6.1 / R3）。
  - SOCKS 的 DNS 行失败用 ○ 不用 ✗、汇总「不计分」——**已做**并改 R3。
  - Google 那句判断缩到 ≤ 59 列——**已做**（56 列）。
  - 修复路径多探测一次——**T12a 已去掉**（`PROBE_URL` 只打 2 次）。
  - 检查失败时，下一步小菜单会读走管道输入的一行——**写进 spec §4.4**。
  - 40 列日志页时间戳压成 `HH:MM:SS`——**已做**（阈值 50 列）。
  - `RealSys` 的 `tcp_listening` / `resolve` 只能真机验——**第一轮未验**（§6 E）。
- **T12a**：**已做**；追加的 A–D 都有测试；`Hooks::repair` 多收 `wait_ready` 闭包（并行分支实现 `Hooks` 要跟着改）；`Verdict::Restarted` 加 `tun_ready`、`check::run_manual` 已删。
- **T12b 衔接**：T12a 的两处 + T10 的第三处外层锁**都已删**（`the_update_command_downloads_outside_the_lock_and_takes_it_once` 断言锁 (1,1) 不睡）。
- **T13**：卸载成功要退出菜单（`Outcome::Exit`）——**未做**。
- **T16**（**未做**，累计追加）
  - 给端口分配补「互不相同」的单元测试（`sing-box check` 不绑端口，查不出重复）。
  - 更多连接失败归 `Other("connect")`，结果页显示「✗ 失败」。
  - `log.level = "error"` 会打印 socks 入站认证失败的凭据明文：读 NoStart 日志就降档，不读就 `log.disabled`。
  - [5] 的 `Item::Tunnel if baidu_ok` 文案改指令式「本机能上网，是当前节点不通；先试一个直连节点。」（`Summary` 加 `active_is_residential`，`Item::Google` 不动；台账 `followup-resi-advice.md`）。
  - spec 测速章节仍写「✗ 解析失败」，与 T5fix 的「连不上」矛盾。
  - [9] 结果页的删除复用 `delete_nodes`（T12a 的锁等不到修复会跟过去）。

## 5. 留给最终审查的小问题

原有 8 条：

- `FakeSys::term_size` 按契约归一（列 0 → None），删掉 `width` 里的过滤与 `(0,30)` 断言。
- `char_width` 是手挑的宽字子集（文档已写明没收的段），带 U+FE0F 的 emoji 按 1+1 算。
- `wrap` 的行首禁则只守了硬折，没守折点路径：`wrap_pieces("备注（家里人用的，）别删", 18, 18)` 会得到以「）」开头的行；`fits` 要再检查下一行开头不在 `NO_LINE_START` 里。
- spec 残留：§5.3 输入表与 §11.3 的「`y`、`yes` → 执行」「这一步只认 y」在 Empty 形态下与 R1 冲突；§10.3 的 `ConfirmInput` / `parse_confirm` / `render_delete_picker` / `render_delete_confirm` 签名是旧的。
- TUN 没起来时「上次」行的文案裁定：后缀缩短为「，但 TUN 没起来」；名字可用不到 8 列时不写名字，改「已切换节点，但 TUN 没起来」；「已是当前节点」加 TUN 没起来同样处理。
- 40 列下切换结果行本身 47 列（一闪即被清屏）。
- T14 的 golden 是四份单行常量，难审（已有 `#[ignore]` 重录用例）。
- `e561f50`、`57b3d91` 两个小修没单独审过。

新增：

- **两条来自服务端往来的检查点**：① 涉及切换 / 回滚 / 重试的改动，每条失败路径写出终态且终态有断言，落在用户或运维会看的出口；② 任何「修好了失败路径」的改动重新量最坏耗时。
- 未单独复审的提交：`004b61d`、`e80b7bd`（T7 修复与收尾）、`102d22a`（注释）。
- 各任务记账未修的 Minor（按任务）：
  - T11fix r3 M1''（spec §2.5 [4]→[1] 那格措辞）；T11 r1 M1 / M2 / M6（端口行行首标记只反映 SOCKS；`service_down` 覆盖已评分项；[1] 再查一次不重载 profiles）。
  - T7 M10（T7 与 T7b 必须同发）；T7b M2–M5、M7（`Cmd::ImportV3` apply 失败时丢「跳过 N 个…」）、M8–M10（邀请答 y 再答 n 的终态无测试）。
  - T9 F2 / F3 / F5。
  - T12a M1（[1]/[2]/[7]/[8] 上次行 40 列截掉「稍后再试」，可沿用 `wait_for_lock` 配短式）、M3（R11 守门没走 [7] 三条路径、`import-v3`）、M6（「在等」那句排版）、M7（巡检 UFW 重放与 Healthy 清零锁外读改写 runtime.json）、M8（spec 三处 `Lock`）、M9（`delete_nodes` ② `Err(e)` 分支无测试，需 `FakeSys::lock_fail`）、M10（「删除没做：」+ `LOCK_BUSY` 61 列）；顺带：40 列下 `正在切到 {name}…` 长名字硬折出孤行。
  - Ttoken M3（菜单 [3] 出路判断挂在「取 + 落盘」合并后的错误上）、M4（`RELINK_CLI` 在 stdout）、M5 / M6（fix_now 未做）、M7（`stdin_empty` 退出码 1）。
  - T10 #A（runtime 写不进时看不到查到的版本）、#B / #C / #D / #E；报告顾虑：`UPDATE_CHECKING`「最多 30 秒」偏小，rc 回退最坏 60 秒；新 manifest 缺本机 bui-c 资产时只升级内核，留下旧 bui-c + 新内核（spec §8.1「内核也要更新时只给内核」的直接后果）。
  - T12c r1 M1（`converge` 的 apply 支不经 `apply_with_ufw`）、M2（进菜单等锁失败上次行 40 列截「稍后再试」，与 `LOCK_BUSY_SHORT` 统一）、M3（R11 守门缺「收敛失败 → Pause」）、M4（巡检锁外读改写 runtime.json）；r2 M1 / M2（fix_now）、M4（收敛放弃后巡检每分钟 restart 不存在的单元，建议 `check::run` 在主单元文件不在时按 R13 跳过）、M5（halfway 判据时间窗）；顾虑：「pending 在、节点还在、没有活动节点」格只删 pending 不报。
  - T12b r3 M1–M8（内核闸取样在 assess 之后；`counts_as_update` 半换分支与 doc 相反；「变了再问」比进 `manifest_source`；锁长期被占每分钟重下；`daily_self_update` doc 与 `rt.save(..)?`；`LOCK_BUSY` 两份字面量与 spec §11.7 / §8.3；`stage_on_lock` 用例只有 timer 路径；写内核与 restart 之间被杀）。
  - T12b r4 三条：① `Cmd::Update` 的新闸排在 MissingAsset 之前，缺本机架构 bui-c + 内核被别处换成别的时要跑两次才看到真正的阻塞原因（改一行次序）；② 镜像「半换」（自身这次换上、内核被别处换成别的）这一轮不打「已更新 bui-c」，再答 y 会把同一份 bui-c 再下一次、原样重写；③ `Installed::Changed` 的 doc 说「什么都没装」，半换时其实装了一半。②③ 与 lead 记账的「半换」同源，修法是 `Changed` 之前按 `self_updated` / `kernel_updated` 把 `self_reason` / `kernel_outdated` 改成盘上现状再重新显示。不装错、不丢数据。
  - T14 golden 四份单行常量难审；`probe_config` 公开等于公开隐式前置条件 `domain_resolver: "local-dns"`。
- spec / 计划脱节的记账：§12.1 T7 行的两条测试属 T12c / T12b；§5.5 正文与 R2 在「节点非空、active 缺失」格冲突，按 R 条优先（T12c 已按 R2）；§4.3 表「更新做完 ≥ 2 行 → 停」与 §8.1「→ Pause」口径不一。
- 遗留项更正：[HANDOVER-bui-c.md](HANDOVER-bui-c.md) §4 的「已装 rc6 / rc7 的客户端收不到修复」**已解决**（同版本比 sha256 的修复在本分支历史里），不再算遗留。
- **第一轮真机截屏独立判定的 4 条轻微**：
  - 40 列确认提问的窄屏例外：名字可用 5 列、低于 `QUESTION_NAME_MIN`（8），按 R1 字面应退回「确认删除这 3 个节点？」；实际因第一个名字只有 3 列「放得下」就带上了名字。行为比 R1 字面更好，但 R1、`delete_question` 的注释与代码三处说法不一，守门测试也没覆盖「短名字 + 窄空间」。改 R1 与注释对齐代码，并补一条用例。
  - spec §5.10、D10、R7 仍写「非终端没加 `-y`」「`--json` 没带 `-y`」退出码 1，与规范性的 R15（2）矛盾；§12.2 V13 没注明要在终端里跑。实现按 R15，判定员也认可「用法错误先于名字检查」的先后（这类错误只看 argv 与 tty，与盘上状态无关，反过来会让退出码随盘上有没有这个名字在 1 / 2 之间跳）。改 spec 那几处。
  - **验收方法缺口**：驱动脚本压掉了连续空行，没做 §12.2 要求的 `capture-pane -p` 与 `-pJ` 行数比对，所以「不折行」这一条是按容量口径计算推断的，不是直接证据；删完没再进 [6] 截 6 个节点的列表页；40 列的过渡提示没截到（它只在会话第一次进删除页时出现，而那次是 80 列）。下一轮补上。
  - 主菜单提示符写「选择 [0-9]」而选项块里没有 [9]：R15 规定 T9 到 T16 之间按 9 给一条 Note，不算错，T16 加回 [9] 行后自然消失。

## 6. 真机验收（baiyi）

准备、保险丝与收尾恢复照 spec §12.2 与 §8。主表是 spec §12.2 的 V1–V14，下面是 V1–V14 没覆盖的补遗（本机台账 `realmachine-addendum.md` 是这一节的底稿）。

### A. SOCKS 应答 0x01 的归类（T5fix）

sing-box 的 `ReplyCodeForError` 只认四个 errno，DNS 失败与拨号超时都落进 0x01「general server failure」；应答反映的是「拨节点服务器」这一段。误判窗口只在 **vless 节点、拨节点服务器 TCP 超时**（sing-box 默认 5 秒放弃，早于 bui-c 隧道行 8 秒 / 网站行 6 秒）。`9e0b179` 已把 0x01 归「连不上」并带耗时，所以判定是：

1. 前提：SOCKS 模式，活动节点是 vless / REALITY（`config.json` 里 `proxy-out` 的 `type == "vless"`）。hysteria2 节点 QUIC 握手超时 15 秒，bui-c 计时器先到，验不出，跳过。
2. 制造「拨号超时」而不是「被拒」：`iptables -I OUTPUT -d <节点IP> -p tcp --dport <节点端口> -j DROP`（验完立刻 `-D`）。不要用「端口没人听」，那会立刻 RST → ECONNREFUSED，走的是已知正确的分支。
3. 跑 `[5]`（或 T18 之后的 `bui-c test`），看隧道 / Google / YouTube / GitHub 四行。
4. 判定：显示「连不上」并带耗时（约 5 秒）= 修复生效；仍出现「域名解析失败」= 修复没覆盖到这条路；显示「超时（8 秒 / 6 秒）」= sing-box 没在 bui-c 计时器之前吐出 0x01，先核 iptables 规则是否命中正确的 IP 与端口。
5. 撤回规则，正常跑一遍 `[5]` 确认恢复。

### B–H（照旧）

- **B.** 下载行在 5 秒到点时显示按已下载字节估算的结果（MB/s 与评语），不是「失败」。
- **C.** 探测不跟重定向：YouTube 拿到 3xx 算通。
- **D.** 管道输入时停顿不读：`printf '5\n0\n' | sudo -n bui-c` 不出现「回车返回菜单」，`0` 不被停顿吃掉；`sudo -n bui-c status | od -c | grep -c 033` 为 0。
- **E.** `RealSys::tcp_listening`（SOCKS 下端口有人听 ✓；TUN 下正确跳过或判失败）与 `RealSys::resolve`（SOCKS 下 DNS 行毫秒数像真的；`/etc/resolv.conf` 指黑洞时「✗ 本机 DNS 解析失败」3 秒封顶——动全机 DNS，要主人同意）。
- **F.** 窄屏与手机：40 / 60 / 80 / 100 列 tmux 逐键截屏；`tmux -y 17`（手机横屏行数）下 47 列与 60 列各走一遍删除 Switch 形态与 `[5]`；歧义宽度按 2 列画的终端与真实手机 SSH 客户端要主人提供（spec R9 ②）。
- **G.** 删除对象：9 个节点里有 1 个指向已下线的临时服务器（`temp.example.test`），另有 2 个 `bwg-tizi` 上的 HY2 节点，其面板账号 09-14 已被删除，也是死节点——都不是活动节点，正好做删除验收。「删光」不上真机（沙箱只隔离文件不隔离 systemd，真删光会停掉 baiyi 的生产隧道），靠 T7 / T12c 单元测试覆盖。
- **H.** 既有风险：V14（删活动节点）时 SSH 可能卡几秒，先挂保险丝；全新主机「安装脚本 → 导入 → 首次 apply → 切模式 → 卸载」没在干净 systemd 主机上走过；`bui-c.v3` / `bui-c.bak*` 的任何子命令都不要跑；ssh 命令串里不出现能被 `pkill -f` 匹配到自己的写法。

### I–N（本轮新增）

- **I. 真 flock（T12a，`RealSys::try_lock` 无单元测试）**：① 两个 root 会话同时进菜单做切换，第二个先打「等它结束（最多 15 秒）」，等到或 15 秒后「稍后再试」；② 删除进行中 timer 的 journal 出现「本轮巡检跳过」且没有 restart；③ `ln -sf` 把 `/run/bui-c.lock` 换成符号链接后菜单报错不跟随；④ `ls -l /run/bui-c.lock` 是 `-rw-------`；非 root 跑改配置命令先被 root 检查拦下；⑤ 一个会话卡在锁里（[4] 重启等 TUN），另一个按 [6] → 确认 → 15 秒后停顿页「删除没做：…稍后再试」「节点都还在」，回车后「上次：别的操作没结束，没删，稍后再试」，`bui-c list` 节点数不变。
- **J. token 链接导入（Ttoken）**：从 rc12 面板复制整条 `https://panel.example.com/api/sub/<token>` 链接：① 菜单 [3] 粘贴 → profile 名是 `alice-…` 形态（载荷回填人名），列表 / 确认块 / 上次行 / `bui-c list` 都不含 32 位十六进制；② `printf '%s\n' '<链接>' | sudo bui-c import --sub -` 同样；③ 旧用户名链接（宽限期后 404）→ 停顿页有「面板接口不认这个地址」与「从面板重新复制整条订阅链接」，不含「节点接口」；④ `bui-c import` 不带参数退出码 2。
- **K. 更新拆锁（T12b）**：两个 root 会话，A 在 [7]→[1] 答 y 开始下载（`BUI_C_PANEL` 指到慢面板拖长），B 在下载期间 `bui-c update` 装完同一份 → A 进锁后 `sha256sum /usr/local/bin/bui-c` 仍是 B 装的，结果行「没有需要更新的」，回主菜单无 ★、[7] 子页「已是最新」。再看：B 从另一个来源装一份不同构建 → A 应看到「更新内容刚刚变了，按下面的再确认一次」并重新提问；同情形命令行 `bui-c update` 两行 `manifest …` / `自身更新=false 内核更新=false 已重启=false`，stderr「下载期间已被别的操作换过，这次没装，再跑一次 bui-c update」，退出 1；再跑一次退出 0 且真换上。
- **L. 中断收敛（T12c）**：删到当前节点确认 yes 之后立刻断 SSH（或 `kill -9` 菜单进程），重连进菜单看「上次：删除没做完，已按节点列表收拾好」；删光时让 `profiles.json` 所在分区只读，看停顿页与下一分钟巡检日志（后者只在一次性主机上做）。
- **M. 检查更新（T10）**：[7]→[1] 先「正在检查更新…」再「最新 X（来源 …），本机 Y」再 `[y/N]`；答 N 不拿锁不下载；`check_only` 在真机会多跑一次只读 `sing-box version`。
- **N. 40 列逐键截屏要加的形态**：主菜单 ★ 形态、[7] 子页、删除锁等不到的停顿页、收敛结果的上次行、token 导入的两句出路。

### 第一轮结果（2026-09-15，`632b475` 的 musl 构建）

准备：用新文件名备份 `profiles.json`（连同 sha256）、`runtime.json` 与旧二进制，不覆盖 09-13 的旧备份；装新二进制后 `bui-c.service` 的 MainPID 不变。全程 `tmux -L buitest`，主人的 tmux 会话一概没碰。

**通过的：**

- 命令行 `bui-c delete`：名字不存在 + 非终端无 `-y` → 退出码 2（用法检查先于名字检查，实现有意为之）；加 `-y` → 退出码 1「没有叫 … 的节点…什么都没删」，`profiles.json` 未变；`--json` 无 `-y` → 2；`status` 输出无 ESC；非 root 跑 `status` → 2「需要 root」。
- 菜单逐键（80 / 60 / 40 列）：主菜单有 [6] 删除节点 / [7] 更新与维护；[6] 的过渡提示只第一次出现；输错 `0 3` / `5-3` / `99` / `1 a` 各一行原地重问、列表不重画、文案对；空行回主菜单。
- 删除（spec V9，Passive 形态）：选 `1-3`（三个非活动死节点：临时服务器那个 + 账号已删的两个 HY2）→ 60 / 40 列确认块清单紧挨提问、`[y/N]`、「删完还剩 6 个，当前节点不变，不会断网」、墓碑说明、40 列主机另起一行；答 n → 上次行「已取消，没有删除任何节点」、sha256 不变；答 y → 上次行「已删 3 个」。
- 删完核对：9 → 6 个节点、墓碑 3 条、MainPID 前后不变（Passive 不重启）、没有 `profiles.json.bak`、`/run/bui-c.lock` 存在、没有 `pending.json`、隧道 204。
- [5] 连接检查（60 列，TUN）：全部计分项通过，约 8 秒；隧道 / Google / YouTube / GitHub 各约 1.1 秒标「慢」，百度直连约 100ms，下载 1MB 约 2 秒标「较慢」，IPv4 出口含地理与风险分，IPv6 已被隧道拦截；MainPID 不变。
- 收尾：备份拷回 `profiles.json`，`sha256sum -c` 通过，9 个节点、无墓碑字段，服务与 timer active，隧道 204，无残留，删掉临时脚本。新二进制**保留在 baiyi**（见 §8 自动更新提醒）。

**未做（侵入性，要主人同意或另找时机）：**

- A（SOCKS 模式 + vless 节点 + iptables DROP 验 0x01 归类）。
- E 的 `resolve` 失败（改 `/etc/resolv.conf` 验 3 秒封顶）；B、C、D 也没有专门跑（这次下载 2 秒内读满、没触发 5 秒截断）。
- V14 删活动节点（spec 写明主人同意后才做）。
- I 真 flock 并发（两会话、timer 遇锁跳过、锁文件被换成符号链接）。
- 墓碑重新导入：这三个节点来自 v3 且账号已删，从面板导入复现不了，要换对象。
- J–M 与 F 里的 100 列、47×17、手机与歧义宽度截屏。

P1 一轮（T18 之后）照计划：[9] 测速（结果、切到最快的确认、删除不通的会先重测）；测速后 `ps -o pid,args -C sing-box` 只剩生产那一个；卸载清单只看不执行（答否）；`bui-c test`。

## 7. 怎么接着干

- **流程**
  - 用 superpowers 的 subagent-driven-development：每个任务派一个执行者，完成后派审查员，拿到 spec 符合度和代码质量两个结论。Critical / Important 必须修完再复审；Minor 记账，最后统一处理。
  - brief 用插件脚本 `task-brief <计划文件> <任务号>` 从计划里抽；审查包用 `review-package <基> <头>` 生成。
  - 模型分工：Fable 审查 / 设计 / 总结 / 派单（429 时审查退 Opus），Opus 执行，Sonnet 调研，Haiku 监控。
  - 审查：Approved 时只在第 1 轮修 fix_now；同一条 Important 跨两轮没收口就 escalate 给 lead，lead 裁定时同时钉死修复范围与复审范围，不再自动派修。审查员一律 `git show <SHA>:<路径>` 读、在 `git archive` 导出副本上跑门禁与探针，探针不进仓库。
  - 报告模板要有：基线 SHA、每条要求落在哪个测试、新文案实测宽度、失败路径终态表（路径 → 终态 → 出口 → 断言在哪）、最坏耗时表、变异核对、顾虑。
  - 并发语义的任务（两个会话交错）在设计阶段先画时序表，把每格终态列出来；T12b 三轮审查每轮都在「并发更新 + 跳过安装」这一片逐层挖出真问题。
- **并行**（旧手册「派到别的 worktree 的 agent 先 `EnterWorktree(path=…)` 切进去」是**错的**，已实证）
  - 子 agent 的写入 / 执行强制层钉在**会话的启动 worktree**。`EnterWorktree` 只换 cwd，换不了强制层：Bash、Edit、ExitWorktree 全被拒，派孙 agent 也继承同一个 pin。要么让执行 agent 的启动目录就是目标 worktree（`isolation: 'worktree'`，启动即钉），要么由主会话自己在那个 worktree 里做。
  - **并行实现 + 串行集成**：每个任务一个 worktree，起手 `git reset --hard <主线HEAD>`，`cp -Rc` 克隆主线 `target` 省编译；做完由**唯一一个**集成 agent 在主线串行 cherry-pick、解冲突、跑三条门禁，把 rc 与测试数写进台账的 `integrate-<任务>-<sha>.log`；审查对集成后的 SHA 做；修复再开新 worktree。别的 agent 不得同时往主线 cherry-pick（集成员见到脏工作区会 BLOCKED，整条线跟着死）。
  - 本轮实测：20 次 cherry-pick 集成 + 1 次 lead 手工合并 T11 分支（9 块）。真文本冲突只有 T11 合并与 T12a 的 `save_import` 一处；语义冲突 4 次（`ClockSys` 缺 `try_lock`、`last_line` 重定义 + 夹具口径、T9 键位重排后墓碑测试按键、T4 的 `a_piped_script` 断言旧输出）；其余干净。cherry-pick 干净不等于能编、能编不等于语义对，集成后一律跑全量门禁并读失败用例。
  - 「两边都留」型冲突先看 HEAD 侧片段末行是不是 `}`：T11 合并有三处共用右花括号被 git 当公共行，直接并集会让 fn 吞掉整块；测试模块冲突整块搬到末尾，合完核「测试数 = 基线 + 两边净增」。
  - 依赖顺序固定（T7b 依赖 T7 的落盘顺序、T12c 依赖 `teardown_all → save → release_after_teardown`、T12b 依赖 T12a + T10），派发单点名依赖的函数名，实现者开工先 grep。
  - 临时脚本一律带任务名前缀或写进自己的 worktree：共享 scratchpad 里同名脚本被另一个 worktree 的 agent 覆盖过一次。
  - 监控：前台轮询、轮数有限（10 轮 × 5 分钟）；起了后台脚本就返回等于没监控。
  - resume：缓存按调用前缀算，改了提示词就起新 run、把已完成 SHA 当基线，别 resume。
- **门禁**（每个提交）：`cargo fmt --all -- --check`、`cargo clippy -p bui-c -p bui-schema --all-targets -- -D warnings`、`cargo test -p bui-c`（碰了 bui-schema 再加 `cargo test -p bui-schema`）。这三条在 macOS 能直接跑；`crates/bui` 在 macOS 编不过，不要跑 `--workspace`。另外单独验一次 `cargo build -p bui-c`（或 `cargo tree -p bui-c -e features -i nix` 看 `fs` 在列）：workspace 级构建会掩盖单 crate 的 feature 缺口。
- **Linux 门禁**（每合进几个任务跑一次）：
  - `git archive <SHA> | tar -x -C <临时目录>`，再 `rsync -az --delete --exclude target --exclude .git --exclude .claude <临时目录>/ baiyi:~/b-ui-build-menu/`。用单独的目录，不碰别人可能在用的 `~/b-ui-build`。
  - 在 baiyi 上跑 `CC_x86_64_unknown_linux_musl=gcc AR_x86_64_unknown_linux_musl=ar cargo build --release --locked --target x86_64-unknown-linux-musl -p bui-c`，再跑 clippy 和 test。
- **推送前**
  - 用本机敏感值清单（真实域名、面板用户名、IP，路径记在 Claude 记忆 `bui-c-menu-v2-pending` 里，不进仓库）对 `git log -p <上次推送>..<SHA>` 做 `grep -c -i -F -f`，结果必须是 0。清单文件里的空行要先去掉，否则空模式会匹配一切。
  - 推精确 SHA：`git push origin "$(git rev-parse <提交>)":refs/heads/<分支>`。SHA 永远 `rev-parse` 取，别手敲（本轮手敲错过一次，远端拒绝、什么都没推）。
- **收尾**
  - 更新本手册、[HANDOVER-bui-c.md](HANDOVER-bui-c.md) 和 `CHANGELOG.md` 的 `4.0.1` 段，推分支。
  - 跨 crate 部分（`bui-schema` C1 契约、`render::client`、`node_uri`）在合进 `v4` 前交服务端会话（名字 `bui`，用 SendMessage 按名字发）再做一次对抗性审查；客户端内部改动不重做。
  - 合入 `v4` 前以当时的 `origin/v4` 再试合一次。

## 8. baiyi 真机要点

- **tmux**
  - 真人式测试一律用独立 socket：`tmux -L buitest new-session -d -s t -x 100 -y 40`。
  - 用 `send-keys -l` 逐字符打，`capture-pane -p` 截屏，收尾 `tmux -L buitest kill-server`。
  - **主人自己的 tmux 会话绝对不能碰。**
- **动配置前**
  - 改 `profiles.json` 之前先备份：`sudo -n cp -a /opt/bui-c/profiles.json /root/bui-c-profiles.bak-<日期>.json` 并记 sha256（用新文件名，不覆盖旧备份），测完按 sha256 核对恢复。
  - 切 SOCKS / TUN 之前先挂保险丝：`sudo -n systemd-run --quiet --on-active=300 --unit=buitest-revert /usr/local/bin/bui-c mode tun`，验完 `systemctl stop buitest-revert.timer`。
- **沙箱**：`BUI_C_BASE` / `BUI_C_UNIT_DIR` 只隔离文件，不隔离 systemd，会 apply 的操作仍会重启真实服务。
- **v3 备份**：`/usr/local/bin/bui-c.v3`（及其它 v3 备份）的任何子命令都不要跑，它会覆盖 `/usr/local/bin/bui-c`；辨认只用 `file` / `sha256sum`。
- **pkill**：ssh 命令串里不要出现能被 `pkill -f` 模式匹配到自己的写法。
- **删除功能的验收对象**：9 个节点里有 1 个指向已下线的临时服务器，另有 2 个 `bwg-tizi` 上的 HY2 节点账号已被面板删除（no-such-user），三个都是非活动死节点；第一轮已用它们验过删除并恢复。
- **账号被删那件事**：09-14 服务端报 `bwg-tizi` 上某个已删账号一直带错密码重试，核实下来是别的设备，不是 baiyi——baiyi 的活动节点早已是一个直连 REALITY 节点，TUN 通、上网正常；那两个 HY2 节点只是留在列表里的死节点。
- **面板 token**：客户端连 rc12 面板后 `Panel.username` 存的是 token（惰性，只存不打印），`status` / `list` / 菜单状态区都不打印 panel 记录。
- **当前装的是分支构建**：第一轮之后 baiyi 保留 `632b475` 的构建（旧二进制另有备份）。`auto_update` 开着，下一次每日自更新会按 rc12 manifest 的 sha256 判「同版本新构建」把它换回发布版客户端；要长期留测试版得关自动更新——这是主人的设置，没动。

## 9. 需要用户或服务端负责人决定的

- **合并时机（服务端，主理人已授权）**：这批进 **4.0.1**，等 `v4.0.0` 打 tag 之后再合进 `v4`。rc12 在 `bwg-rick` 上 72 小时浸泡（09-17 判定），通过就直接打 `v4.0.0`，`v4.0.0` 必须是浸泡过的那份代码；本分支含 `bui-schema` 改动，按冻结规矩不在浸泡中途改共享 crate。之后开 PR 或给 SHA，服务端对跨 crate 部分再审一次；合并前以那一刻的 `origin/v4` 再试合（`632b475` 对 `f5571a9` 干净）。版本号与 CHANGELOG 日期由主理人发版时定。
- **T12b 收口**：r3 I1 由 lead 裁定「接受，再修最后一次，范围钉死」并落地为 `632b475`，r4 Approved。M1–M8 与 r4 的 3 条 Minor 全部记账（§5）。「半换」情形（别处只换 bui-c、这次换了内核并重启，或反过来）文案对一半不准、菜单多一轮再确认，不会装错；要不要处理由 lead 定。
- **nix `fs`**：`crates/bui-c/Cargo.toml` 与 spec 都已改；台账的全局约束里那句「现有 features 可用」还要改。「CI 对每个发布 crate 单独 check」服务端记进 4.0.1（`release.yml` 一次编 `-p bui -p bui-c`，resolver 2 下 feature 统一替这类缺口打了掩护）。
- **跨 crate 提交**：`4c7be25`、`c84f5a6`、`0b85284` 服务端已审通过；`7665f19` / `594c0c3`（T14fu2）是按服务端五条意见做的。**服务端在浸泡空档对 `c1bbae1` 的跨 crate 部分做了预审，通过**（变异验证上次 5 条都修好、`594c0c3` 只加严不放宽、`crates/bui` 零改动且四个 golden 逐字节不变、合并干跑干净连 CHANGELOG 也合上）。v4.0.0 打完即可合，前提是 `bui-schema` 之后不再改，改了服务端只审增量。C1 契约现在只多 `probe_config` / `ProbeTarget`。
- **`probe_config` / `ProbeTarget` 在 4.0.1 暂时没有调用方**：它们是 [9] 节点测速（P1 的 T16）要用的 C1 公开项，4.0.1 先随 bui-schema 发出去。**不是死代码，不要删**；T16 落地后才有调用方。
- **发 4.0.1 前要 bump 根 `Cargo.toml` 的 workspace version**（现在是 4.0.0，`check-version.sh v4.0.1` 会失败）。本分支不提前 bump（v4.0.0 还没发），由服务端在打 4.0.1 tag 前做。
- **Ttoken 记账**：`looks_like_token` 只认 32 位十六进制，服务端改 token 格式要同步；rc12 → rc11 → rc12 往返会重新生成全部 token（节点凭据不断连，存下的 token 链接失效，要重新导入）；`--sub` 经 `/api/nodes` 成功时会记 `Profiles.panel`（root 自更新来源），与菜单 [3] 同判据，已有断言钉住。
- **发版顺序**：T7 不能单独发版，必须与 T7b 一起（确认块文案已承诺墓碑行为）。
- **服务端侧（不在我们分支）**：HY2 住宅端口与槽号解耦记成 v4.1 设计题；`bui residential remove` 当场列出受影响用户（删 0 号槽时含被搬动那一槽）；被 rebalance 移动、尚未刷新订阅的用户是否单独提醒；CI skip 守门正则与 CLAUDE.md「本机 1.13.19」由服务端改。
- **spec 待定说法**：[7] 子页在 `MissingAsset` / `Unreadable` 之后显示什么（T10 #E）；「pending 在、节点还在、没有活动节点」格要不要在上次行说一句（T12c 顾虑）；`stdin_empty` 退出码 1 还是 2（Ttoken M7）。
- **真机侵入项**：§6 第一轮「未做」各项（iptables 黑洞、改 resolv.conf、删活动节点、真 flock 并发）要主人同意或另找一次性主机。
- 公开仓库历史里曾出现过真实值：凭据轮换和历史重写仍要服务端负责人与用户一起定。
- 本地分流规则功能没做。
- 手机与歧义宽度终端的实机截屏，要用户提供。
