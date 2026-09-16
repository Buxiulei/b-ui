# bui-c 导入按账号匹配：端口变了也原地替换（4.0.2（暂定），先于服务端 4.1）

- 日期：2026-09-15
- 状态：定稿修订第 3 轮（r3），另有核查后手工追加的三轮：第一轮三项（A1 定序改判、M1、M2），第二轮按对抗核查清单修的六项（I1-I6，只换论证与夹具，不动结论），第三轮收尾三项（J1-J3，测试夹具的可达性与逐字节基准）；见文末修订记录最后一节「r3 → 定稿（核查后手工追加）」。方案层面的结论都已由主理人与服务端定下（§14.1），剩余问题只有 §14.2 的几项纯技术细节，不阻塞开工
- 基线：`crates/bui-c` 在 `v4.0.1-rc1`、`v4.0.1-rc2` 与 `origin/v4`（`5b22bdf`）之间零改动（`git diff --stat v4.0.1-rc1 v4.0.1-rc2 -- crates/bui-c`、`git diff --stat v4.0.1-rc2 origin/v4 -- crates/bui-c` 都为空）。下文客户端行号取自 `git show v4.0.1-rc2:<路径>`（与 rc1、origin/v4 相同），`crates/bui-c/src/` 前缀省略；服务端自检、包缓存、发版流程以 rc2 为准，路径写全；服务端 4.1 的设计与计划以 origin/v4 上的 `docs/superpowers/specs/2026-09-15-hy2-singbox-residential-design.md`（下称「服务端 4.1 spec」）与 `docs/superpowers/plans/2026-09-15-v41-hy2-singbox.md`（下称「服务端 4.1 plan」）为准
- 落点：**4.0.2（暂定）**。4.0.1 在浸泡，正式版等 09-18 判定，4.0.1 不再加代码；`v4.0.1-rc2` 已打在 `ad1abb5`，不含本改动。本改动必须先于服务端 4.1 改端口上线（§12）
- 文件：改 `crates/bui-c/src/{profiles.rs,cli.rs,import_v3.rs,menu.rs,delete.rs}`、根 `Cargo.toml` 的 workspace version（4.0.2）、`CHANGELOG.md`（新开 4.0.2 段）、`docs/HANDOVER-bui-c.md`；不改 `bui-schema`；`profiles.json` 不加任何持久化字段，只在代码里给三个结构加 catch-all（§6.3）。发版门禁以服务端 4.1 plan 的 T20 为准、由服务端实现；本线交付的是填进门禁清单的提交 SHA（§12.4）
- 来源：研究员 A/B、三份候选设计、两位评审、合成稿 v1、两份对抗审查（correctness、migration-rollout）、主理人与服务端对审查意见的裁定、对 r1 与 r2 的各两份核查，以及已合并进 origin/v4 的服务端 4.1 spec 与 plan。各版之间的变化见文末「修订记录」

## 0. 结论先行

1. **匹配顺序**：导入时先按账号找列表里的同一账号，再看墓碑，最后才按名字起新名。账号 = `kind` + `host`（**不区分大小写**，与墓碑 key 同口径）+ 凭据主体（HY2 `username`、Reality `uuid`）。账号身份在导入时从 `node` 字段现算，**不持久化**。
2. **kind 门槛（按现状）**：两边的 kind 都「可信」（面板 `/api/nodes` 来源，或备注含「直连」「住宅」）才允许端口不同也认作同一账号；任一边是按备注猜的，只认同端口。门槛实际只作用于 Direct kind，4.1 的住宅换端口不受它影响（§5.2）。
3. **只升不降扩到节点本身**：来件不是面板来源、而账号里对应的条目是面板来源或是活动节点时，只有**同参数**（除 `label`、`hop` 以外 `node` 全等，必然也是同一连接）才原地替换；否则按新节点起名，并打一行说明（说明按挡下的原因给不同出路，不会叫人重复同一个无效操作）；挡下的原因**先判是否受保护（来源是面板，或是活动节点）、门槛次之，两条都报**，受保护条目同时落在门槛外时，说明句在「…不变」之后再补「同时认不准是直连还是住宅」（A1，§5.1、§5.3）。账号组非空、同时有条目被挡下时，也说明被挡下的是哪一条（§5.4 ①）。面板来件可替换组内任何条目；Subscription 来源的条目之间照旧互相替换（§5.3）。
4. **来源等级只升不降，命中即升**：来源等级 ApiNodes > Subscription > Paste = V3。条目被导入命中时，不管节点有没有变化（`Replaced` 还是 `Unchanged`），来源都升到本次来件、条目自身、账号组内面板成员三者中最高的那一个，从不降低（§5.5）。
5. **命中**：沿用留存者的名字原地替换，端口变了打 `更新节点 X：端口 A → B`。留存者次序：活动节点 > 导入前已存在 > 同一连接 > 名字等于 `profile_name` > 列表位置（§5.1）。
6. **同账号存量重复不自动合并**：命令行只提示、不问；菜单 `[3]` 提示并 `y/N` 确认合并，默认 N。只有本批导入刚写进去、又落进同一账号组的副本当场并掉（它不是存量）（§5.8）。
7. **token 名**：导入时把 4.0.0 留下的 token 名改成 `<主机>-<kind>`，输出 `节点 <旧名（打码）> 已改名为 <新名>`；端口没变、命中同一连接时也改。**人读输出一律把 token 打码成前 4 位加省略号**；`--json` 与 `profiles.json` 原样不打码（§8）。
8. **分流只升不降**：非面板来件命中账号时，在合并前的整个账号组里找面板来源成员，沿用它的 `split`（§5.5）。
9. **apply 判定**改为比较导入前后活动节点的内容，不再看名字（§5.7）。
10. **import-v3** 只认本次运行开始前就在列表里的同一账号，认到就跳过，永不回写（§5.9）。
11. **新增的输出行一律经 `tell` 折行**，包括端口变化行；`tell` 打的行计入菜单的「附加行」（§5.4）。
12. **兼容**：`profiles.json` 只做加法；`Profiles`、`Profile`、`Tombstone` 加 `#[serde(flatten)]` catch-all 保住未知字段，`node` 不加；`SCHEMA_VERSION` 不动。本改动不满足「4.0.0 读不了，或 4.0.0 写回会丢不可重建数据」，**不触发**关闭 rc 测试机自动更新（§6）。
13. **上线**：客户端拿到哪个 bui-c 由**打了哪个 tag** 决定，与服务端升没升、谁先升无关（§12.2）。tag 门禁**以服务端 4.1 plan T20 为准**：打 `v4.1.x`（含 rc）之前，`scripts/release/check-release-gate.sh` 断言清单 `scripts/release/required-commits.env` 里 `GATE_v4_1` 的提交是 HEAD 的祖先，清单还是 `pending` 就判不过；本线在「不自动合并同账号重复节点」的实现提交落地后把 SHA 交服务端填进去（§12.4）。baiyi 在打 tag **之前**验收本地构建、打 rc 之后再验收发布件（§12.7）。4.1 放行前逐台核对已知客户端 `status --json` 的 `version`，并以服务端 4.1 spec §7.5「旧订阅兼容不变量」验收兜底（§12.8、§12.10）。兼容段 `40001-40007` 的 REDIRECT **不永久保留**：按服务端 4.1 spec §2.4，counter 连续 30 天为 0 才下线、有命中就顺延，公告与下线间隔不少于 30 天，公告单列一段点名 bui-c 重新导入。

## 1. 背景与问题

### 1.1 服务端 4.1 要改什么

今天（v4.0.x）住宅 HY2 每槽一个实例：槽 `i` 监听 `40000+i`（`crates/bui-schema/src/slots.rs:103` `hy2_port: ports.hy2_resi + index`），跳跃段把 41000-50000 按槽空间等分（`slots.rs:75` `hop_slice`）。用户粘在哪个槽，他的 HY2 住宅节点就是哪个端口（`crates/bui-schema/src/nodes.rs:137-148`）。

服务端 4.1 改成所有住宅 HY2 共用 `40000` + 整段 `41000-50000`；4.0 按槽发出的旧端口是兼容段 `40001-40007`，与整段一起由 nft `redirect to :40000` 转到 40000（服务端 4.1 spec §2.4「端口与 nft 表」）。住宅 HY2 的 label「HY2住宅」在 4.1 不改（服务端 4.1 spec §7.4 第 1 条），所以 `profile_name` 的算法不会漂移。对客户端的影响：

| 用户所在槽 | 4.1 前 | 4.1 后 | rc 客户端重新导入时 |
|---|---|---|---|
| 槽 0 | `40000`，跳跃段是切片 | `40000`，整段 | 端口没变，`same_endpoint` 不比 `hop`（`profiles.rs:197-214`），原地更新，没问题 |
| 槽 ≥ 1 | `40000+i` | `40000` | 端口变了，**可能另起新节点，旧的那条留下**（本文要修的） |

`bui residential rebalance` / `assign` 在 4.1 之前就会让用户换槽、换端口，所以这个改动今天就有收益，4.1 只是让全部槽 ≥ 1 的用户同时换一次端口。

### 1.2 rc 客户端为什么会另起新节点

`store_fetched`（`cli.rs:499-578`）的判定顺序：

1. `find_same_endpoint`（`cli.rs:515`；定义 `profiles.rs:294-297`、`203-214`）：要求 host、port 相同，HY2 还要密码相同。端口一变必然不中。
2. 墓碑（`cli.rs:520-526`）：key 不含端口（`profiles.rs:183-195`），账号删过就跳过。
3. 名字（`cli.rs:527-537`）：只看名字恰好等于 `wanted = profile_name(user, node)` 的那**一条**。它是同一账号就覆盖（`_ => wanted`，`cli.rs:536`），是别的账号就 `free_name` 起 `-2`。

所以端口变了之后能不能原地替换，全看旧 profile 的名字是否**碰巧**等于 `profile_name(user, node)`。`same_account` 从来没被用来「不看名字、直接找同账号」：**在命名路径上它只在 `cli.rs:529` 被读**（另一处调用在 `same_endpoint` 内部，`profiles.rs:204`）。`import_v3::import` 连第 3 步的名字兜底都没有：不中同一连接就直接 `free_name`（`import_v3.rs:149-174`）。

另一个相关的 rc 行为：`upsert`（`profiles.rs:310-325`）在节点与分流都没变时返回 `Unchanged`，**不写 `source`**（`profiles.rs:313-315`）。一条 V3 或粘贴来源的节点被订阅或面板同参数命中过多少次，来源都还记作 V3 / Paste。本版在 §5.5 处理它。

### 1.3 四类存量机器（名字不等于 `profile_name` 的来源）

| 类 | 名字长相 | 怎么来的 | rc 客户端端口变了之后 |
|---|---|---|---|
| A. v3 迁来 | `hysteria2-1785892136`、`HY2`、`reality-Reality` | `import_v3` 沿用 v3 目录名（`import_v3.rs:167-174`） | 另起 `alice-hy2-resi`，旧的留下 |
| B. 4.0.0 token 名 | `<32 位十六进制>-hy2-resi` | 4.0.0 没有 token 过滤，`import --sub <token 链接>` 把 token 当用户名（rc 的过滤在 `source.rs:91-107`；`git grep looks_like_token v4.0.0 -- crates/bui-c/src/source.rs` 无命中） | 另起 `<主机>-hy2-resi`，token 名那条留下；**token 名出现在 list / 菜单 / status 里，截图即泄露** |
| C. 带后缀 | `alice-hy2-direct-2` | 两台服务器同一个 ASCII 用户名（`cli.rs:3696` 的测试），或 rc 早先已留下的换槽副本 | `wanted` 被第一台占着且不是同账号 → 再 `free_name` → `-3`，`-2` 留下，每换一次涨一个后缀 |
| D. 先粘贴后面板 | 先 `panel.example.com-hy2-resi`，后从面板导入 `alice` | 粘贴来源 `user` 恒为空（`source.rs:263`），回落成主机名 | 另起 `alice-hy2-resi` |

**覆盖范围的边界**：`node_uri` 按备注含「住宅」判住宅（`crates/bui-schema/src/parse/node_uri.rs:25`、`57-62`）。备注不含「住宅」的 v3 住宅节点当初被解析成 `Hy2Direct`，与面板下发的 `Hy2Residential` kind 不同，永远不是同一账号：4.1 之后导入会另起 `*-hy2-resi`，旧的那条留下（兼容段下线前照样能用）。这一小类不在本改动覆盖范围内（与 F2「更早的 v3 标签未核实」一起说明）。

**测试夹具**：`baiyi_like`（`testutil.rs:121-185`，字段长度照抄真机、凭据合成）的 9 条**全部**经 `named()` 写成 `Source::ApiNodes`（`testutil.rs:110-119`、`181`），所以按 `kind_trusted(&p.node, p.source)` 全部算 kind 可信，门槛在这个夹具上**从不触发**。它能触发的是存量重复：`HY2`（`Hy2Residential` 40000，`testutil.rs:139-142`）与 `tizi.example.test-hy2-resi`（`Hy2Residential` 40002，`176-179`）同 host、同 username（都派生自 `hy2_direct_node`，`testutil.rs:125-134`）。门槛相关的用例一律自建 `Source::V3` / `Source::Paste` 的 profile（§11）。

### 1.4 核对时确认的事实

- **F1 客户端没有定时刷新节点。** 联网取节点只有**两个入口**：`bui-c import` 命令与菜单 `[3]`；`fetch_panel` / `fetch_http` / `fetch_sub` 一共 **6 个调用点**（`cli.rs:878`、`884`、`2881`、`3005`、`3010`、`3014`），全部只可达自这两个入口（`fetch_panel` 本身只在 `878` 与 `3005` 被调，`source::from_panel` 只经它，`cli.rs:600`）。`check.rs`、`update.rs` 一处不取（`git grep -n -e from_panel -e from_subscription -e fetch_panel -e fetch_sub -e fetch_http v4.0.1-rc2 -- crates/bui-c/src/check.rs crates/bui-c/src/update.rs` 无命中）；每日自更新 `daily_self_update`（`cli.rs:1295-1349`）只换二进制与内核。实现与核对按「入口」做，不按行号做（与服务端 4.1 spec §7.5 首段同一口径）。所以「零手动」分两半：从不重新导入的机器靠服务端兼容段 REDIRECT；重新导入的机器靠本改动不留多余条目。
- **F2 URI 来源的 kind 是按备注猜的。** `node_uri.rs:25` `let resi = label.contains("住宅")`，不含「住宅」一律当直连。面板 `/api/nodes` 直接给 kind；b-ui 的订阅备注是 `<用户名>-HY2直连` / `<用户名>-HY2住宅`（`nodes.rs:52-56`、`127-148`），v3 侧据评审核对自 `1196885` 起如此，更早的 v3 标签未核实。只有用户改过备注或第三方来源才会猜错，但猜错的后果是活动节点被静默改成另一种出口。
- **F3 profile 名不进内核配置。** `engine.rs` 里没有任何 `.name` 引用；`pending.json` 的收敛只看文件在不在。改名不会让 `config.json` 字节变化，也不会重启（`engine.rs:355` `write_if_changed`）。
- **F4 导入后 apply 失败的终态已经是钉住的预期。** `cli_import_still_lists_skipped_nodes_when_apply_fails`（`cli.rs:10249`）：节点存下、apply 失败、返回错误、旧配置不动。本改动不加回滚。
- **F5 HY2 鉴权不看槽（已核实）。** 所有 hysteria 实例的 `auth.http.url` 是同一个地址（`crates/bui-schema/src/render/hysteria.rs:17-19`、`159-166`）；鉴权快照每个用户只有 `user_id`、`hy2_password`、`expires_at`、`blocked`，没有槽位（`crates/bui/src/commands/install.rs:262-275`）；`git grep -n -e slot -e 槽 v4.0.1-rc2 -- crates/bui/src/modules/panel/auth_hook.rs` 无命中。所以 **4.1 之前，同一账号落在旧槽端口上的副本照样能连，只是从另一个住宅 IP 出去**，不是死节点；rc 客户端上活动节点停在旧端口时，一直在悄悄用错的住宅 IP。
- **F6 客户端版本跟着 tag 走。** 面板 `/packages/manifest.json` 发的是 `<base>/manifest.json`（`crates/bui/src/modules/panel/packages.rs:241-256`、`295-306`），这份缓存由服务端每日自检从 GitHub 拉回（`crates/bui/src/serve.rs:286-385`、`crates/bui/src/kernels/mod.rs:446-491`），不是服务端自己跑的版本。详见 §12.2。
- **F7 `tell` 打的行计入「附加行」。** `tell`（`cli.rs:1564-1573`）→ `Ctx::show`（`cli.rs:280-284`）→ `emit` 写进 transcript（`cli.rs:291-294`）；`outcome_since`（`cli.rs:1428-1439`）只跳过空行与 `say_aside` 标过的行。`tell` 按宽度折行、每行缩进两列（`delete::page` → `menu::wrap(l, 2, width)`，`delete.rs:304-308`、`menu.rs:1607-1614`），命令行（`Ctx.indent` 为空，`cli.rs:182`）与菜单（`indent` 为两个空格，`cli.rs:2303`）下都是两列缩进。

### 1.5 现状（2026-09-15）

- `v4.0.1-rc2` 已发布（`ad1abb5`，含 `e72eac1`「每日自检不再把预发布服务器降回稳定版内核」），不含 `crates/bui-c` 的改动。
- bwg-tizi 与 bwg-rick 已升到 rc2，两台面板 `/packages/manifest.json` 的 `version` 为 4.0.1（rc 通道）。两台都是 v4 面板，`/api/nodes` 可用。
- baiyi 已由每日自动更新升到 rc1 的 bui-c 二进制与 sing-box 1.14.1，`auto_update` 开着；sing-box 1.14.1 的 TUN 回归验收已通过。
- 已知 Linux 客户端只有两台：baiyi，与另一台 socks 模式的生产机。
- **现在还走「订阅退回」路径的**，是 `/api/sub/` 链接在 `/api/nodes` 取失败的客户端：v3 面板，或 `/api/nodes` 回 401 / 404 的链接（停用的用户名链接、面板不认的 token），`cli.rs:3004-3011`、`3018-3020`。rc2 `cli.rs:2990-2993` 的注释仍写「v3 面板（bwg-tizi）没有 `/api/nodes`」，是历史描述，已与现状不符（§14.3 另立小项更正）。

## 2. 目标与非目标

**目标**

1. 同一账号端口变了（含同时换密码），重新导入时原地替换，A-D 四类名字全部覆盖（§1.3 的边界除外），名字不变。
2. 存量 token 名导入一次就改掉；存量重复每次导入都说清楚，用户在菜单里一键合并。
3. 绝不把一种出口的节点改写成另一种出口（直连 / 住宅），绝不跨服务器、跨家人账号覆盖；来源更弱的来件（非面板）只有在与活动节点或面板节点**同参数**（除 `label`、`hop` 外 `node` 全等）时才原地替换它们，端口、HY2 密码、obfs 密码、SNI、Reality 公钥或 short_id 任一不同都不覆盖。
4. 删过的账号，端口变了之后仍然挡得住。
5. 客户端先于服务端 4.1 铺开，且在 4.1 生效窗口里不丢掉按账号匹配（§12）。
6. 人读输出不露完整 token。

**非目标**

- 不改删除流程本身（墓碑怎么记、确认块结构、R15 宽度表的行结构都不动；确认块里的名字只换成打码后的显示名，§8.3）。
- 不在 `list` / `status` / 进菜单时改名或写盘：它们是只读路径，不拿锁（menu-v2 spec §0.2 R11），非主动路径不改用户配置（`import_v3.rs:3-4` 决策 10）。只读入口只做**显示层**打码。
- 不自动合并存量重复（§5.8，主理人拍板）。
- 不处理 Reality uuid 轮换（面板「重置订阅链接与凭据」会同时换订阅 token、HY2 密码与 VLESS UUID，`docs/HANDOVER-bui-c.md:120`）留下的旧 Reality 节点：新 uuid 就是新账号。同一次轮换里 HY2 只换密码、username 不变，仍按账号原地替换。
- 不给导入加回滚（F4），不加定期刷新节点（F1、决策 10），不给 `import` / `import-v3` 加 `--json`。
- 不回收后缀名，唯一例外是 §5.8 合并时留存者取回规范名。
- 不改 kind 门槛口径，不改成「只对面板来源开放跨端口匹配」（主理人拍板，§5.2）。
- **客户端「只升不降」自更新**（不按版本号排序、降级也换，`update.rs:414-417`）是另外的 4.0.1 backlog 项，不在本设计。

## 3. 方案来源与取舍

两位评审意见曾不一致：评审一选 minimal（只替换、不删、孤儿与 token 名只提示），评审二选 migration（导入一次就合并重复、改掉 token 名）。v1 以 migration 的结构为主干、嫁接 identity 的双向 kind 门槛。两份对抗审查之后，主理人与服务端把分歧点逐条定下；r1 的两份核查又补了来源等级与上线门禁的缺口。本版的形态是：

| 维度 | 取自 | 本版 |
|---|---|---|
| 按账号匹配、不看名字 | migration / identity | 保留 |
| 双向 kind 门槛 | identity | 保留，写明只作用于 Direct kind |
| 只升不降 | identity（分流）+ 审查 migration-rollout C2（节点）+ r1 核查（来源命中即升） | 扩到节点本身；来源等级命中即升；账号组内找面板成员取分流 |
| 存量重复 | minimal（提示）/ migration（合并） | 命令行提示；菜单确认合并，默认 N |
| token 名 | migration（导入时改名）+ 审查（展示层打码） | 两者都做 |
| apply 判定 | migration / identity | 按内容比较 |
| import-v3 | 评审一 | 只匹配 `[..known]`，只跳过 |
| 上线 | 审查 migration-rollout C1 + r1 核查 | 按 tag 推导；tag 门禁按「首个含本改动的版本」起算；兼容不变量兜底 |

仍然保留 v1 评审一坚持的保守约束：不碰删除流程、不给只读入口加写路径、门槛外的条目绝不移除。

## 4. 术语

| 术语 | 定义 | 实现 |
|---|---|---|
| 连接 | 账号相同，且 port 相同；HY2 还要密码相同。不比 label / hop / sni / public_key / short_id / obfs_password | `same_endpoint`（`profiles.rs:197-214`，本版删掉第 205 行多余的 `a.host == b.host`，host 口径只由 `same_account` 决定） |
| 账号 | `kind` 相同、`host` 相同（**本版起不区分大小写**，按 `to_lowercase` 比，与 `tombstone_key` 的 `profiles.rs:192` 一致）、凭据主体相同。不比端口、不比密码。导入时现算，不落盘 | `same_account`（`profiles.rs:150-168`） |
| 墓碑 key | `"{kind_slug}\|{host 小写}\|{sha256(凭据主体)[:16]}"`，不含端口、不含明文凭据；与「账号」同一层级 | `tombstone_key`（`profiles.rs:170-195`） |
| 来源等级 | `ApiNodes` 3 > `Subscription` 2 > `Paste` 1 = `V3` 1；同级不互换 | 新增 `Source::rank` |
| kind 可信 | 来源是 `ApiNodes`，或备注含「直连」或「住宅」 | 新增 `kind_trusted` |
| 同参数 | 除 `label`、`hop` 以外 `node` 全等（host 按 `same_account` 的口径不区分大小写）；比「连接」严：obfs 密码、SNI、Reality 公钥 / short_id / flow 也要相同 | 新增 `same_params` |
| 门槛内 | 同账号，且（端口相同，或两边 kind 都可信） | 新增 `gate_ok` |
| 受保护条目 | 相对一条非面板来件：条目来源是 `ApiNodes`；或条目是活动节点，且不属于「Subscription 来件遇 Subscription 条目」 | 新增 `protected` |
| 账号组 | 门槛内、且（不受保护，或与来件同参数）的 profile 下标，按列表顺序 | 新增 `account_group` |
| 被挡下的同账号条目 | 同账号、不在账号组里的条目；原因**先判是否受保护，门槛次之，两条都报**（A1）：受保护（来源 `ApiNodes`，或非面板来源的活动节点）且与来件不同参数 → 来源 `ApiNodes` 为 `PanelEntry`，否则 `ActiveEntry`，**不论门槛内外**；同时在门槛外时说明句再补一句「同时认不准是直连还是住宅」。其余被挡下的（此时必然在门槛外、且不受保护）→ `KindUnsure` | 新增 `blocked_same_account` |
| 留存者 | 账号组里留下来接收新数据的那一条 | 新增 `pick_keeper` |
| 存量 / 本批副本 | 下标 `< known`（`store_fetched` 开始时的条数）的是存量；`≥ known` 的是本批刚写入的 | `known` |
| token 名 | 按 `-` 切开后有一段恰好是 32 位十六进制的 profile 名或墓碑名（判据同 `source::looks_like_token`，`source.rs:91-97`） | 新增 `name_has_token` |
| 显示名 | 人读输出里的名字：先 `menu::sanitize`，再把 token 段打码成前 4 位加 `…` | 新增 `menu::display_name` |

同一台服务器上 Reality 直连（:10001）与 Reality 住宅（:10002）共用 uuid，HY2 直连（:10000）与 HY2 住宅（:40000）共用 username，只靠 kind 区分（`profiles.rs:177-179`）；家人账号凭据主体不同，天然分开。

## 5. 匹配规则

### 5.1 `profiles.rs` 新增与改动

```rust
impl Source {
    /// 来源等级：ApiNodes 3 > Subscription 2 > Paste 1 = V3 1。只用于「只升不降」（§5.5）。
    pub fn rank(self) -> u8;
}

/// 改：host 不区分大小写（与 tombstone_key 的 host.to_lowercase() 同口径，D6）。
pub fn same_account(a: &Node, b: &Node) -> bool {
    a.kind == b.kind
        && a.host.to_lowercase() == b.host.to_lowercase()
        && /* 凭据主体比较：profiles.rs:160-167 原样 */
}

/// 改：删掉 profiles.rs:205 的 `a.host == b.host`，否则 same_endpoint 仍区分大小写，
/// 两个函数的 host 口径不一致。
pub fn same_endpoint(a: &Node, b: &Node) -> bool {
    same_account(a, b) && a.port == b.port && /* HY2 密码：profiles.rs:207-213 原样 */
}

/// kind 是来源明说的，还是 node_uri 按备注猜的（bui-schema parse/node_uri.rs:25）。
pub fn kind_trusted(node: &Node, src: Source) -> bool {
    src == Source::ApiNodes || node.label.contains("直连") || node.label.contains("住宅")
}

/// 门槛内：同一账号，且跨端口时两边 kind 都可信。
pub fn gate_ok(p: &Profile, node: &Node, src: Source) -> bool {
    same_account(&p.node, node)
        && (p.node.port == node.port
            || (kind_trusted(&p.node, p.source) && kind_trusted(node, src)))
}

/// 只升不降（D1）：非面板来件不许覆盖面板来源的条目或活动节点，
/// 例外是 Subscription 来件遇 Subscription 条目（现拉的服务端数据，照常受益）。
pub fn protected(p: &Profile, is_active: bool, src: Source) -> bool {
    src != Source::ApiNodes
        && (p.source == Source::ApiNodes
            || (is_active && !(src == Source::Subscription && p.source == Source::Subscription)))
}

/// 同参数（r3，核查 C2）：除 label、hop 以外 node 全等，host 按 same_account 口径。
/// same_endpoint 只比端口与 HY2 密码，obfs_password、sni、public_key、short_id 不比
/// （profiles.rs:197-214 的注释有意如此）；受保护条目要的是更严的这一个：
/// 面板开了 obfs 之后粘贴一条旧链接，端口与密码都相同，但覆盖上去活动节点就连不上了。
pub fn same_params(a: &Node, b: &Node) -> bool {
    same_account(a, b) && {
        let mut x = a.clone();
        x.label = b.label.clone();
        x.hop = b.hop;
        x.host = b.host.clone(); // same_account 已按小写比过
        x == *b
    }
}

/// 这条 profile 能不能被来件原地替换。受保护条目只接受同参数的来件（same_params 蕴含 same_endpoint）。
pub fn movable(p: &Profile, is_active: bool, node: &Node, src: Source) -> bool {
    gate_ok(p, node, src) && (!protected(p, is_active, src) || same_params(&p.node, node))
}

/// 名字里带 4.0.0 的订阅 token。
pub fn name_has_token(name: &str) -> bool {
    name.split('-').any(crate::source::looks_like_token)
}

/// 从墓碑 key 推出可以展示的名字 `<sanitize(host)>-<kind_slug>`；host 清洗后为空退回 kind_slug。
fn key_display(key: &str) -> String;

/// 同账号条目为什么没进账号组（给 ① 与 ③ 选说明句用，§5.3、§5.4、§10）。
/// 每一条的原因**先判是否受保护，门槛次之，两条都报**，结果唯一
/// （A1，服务端 bui 会话裁定，取代 r2 核查定下的「逐条先判门槛」）：
///   protected(p) 且 !same_params(&p.node, node)
///       → p.source == ApiNodes ? PanelEntry : ActiveEntry，
///         两者都带 kind_unsure: !gate_ok(p, node, src)，**不论门槛内外都报这一条**
///   否则 → KindUnsure
/// 第二支必然 !gate_ok：same_params 蕴含 same_endpoint、进而蕴含 gate_ok，所以「被挡下」
/// 之后剩下的两种情形里，「不受保护」等价于 !gate_ok，「受保护且同参数」根本到不了这里
/// （它 movable，已经进了账号组）。
/// derive 是必需的：测试 5a 要用 `==` 断言 `PanelEntry { kind_unsure: true }`（PartialEq/Eq + Debug），
/// §5.4 ③ 的 `match why { … => menu::protected_new(&other, &fresh, why) }` 在 arm 里再按值用一次
/// `why`（Copy）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blocked {
    /// 受保护、且条目来源是 ApiNodes（不管是不是活动节点）：出路是从面板重新导入。
    /// kind_unsure 为真表示它同时在门槛外（端口不同、且来件的 kind 是按备注猜的——
    /// 条目自己来源是 ApiNodes，kind 恒可信），说明句按 §5.3 补那半句。
    PanelEntry { kind_unsure: bool },
    /// 受保护、且条目是非面板来源的活动节点：出路是确认新节点能用后切换过去。
    /// kind_unsure 同上（端口不同，且两边 kind 至少一边是按备注猜的）。
    ActiveEntry { kind_unsure: bool },
    /// 不受保护、且在门槛外：kind 至少一边是按备注猜的、端口不同。
    KindUnsure,
}

#[derive(Debug, Default, PartialEq)]
pub struct Healed {
    /// (旧名, 新名)。旧名只用于打码后输出（C7），不进任何未打码的输出。
    pub renamed: Vec<(String, String)>,
    pub tombstones: usize,
}

#[derive(Debug, Default, PartialEq)]
pub struct Merged {
    pub keeper: String,         // 合并之后留存者的名字（可能已取回规范名）
    pub removed: Vec<String>,
    pub renamed_from: Option<String>,
}

impl Profiles {
    /// 账号组：movable 命中的 profile 下标，按列表顺序。扫全部 profile，不只看某个名字；
    /// 同一批里先写入的节点也在其中。
    pub fn account_group(&self, node: &Node, src: Source) -> Vec<usize>;

    /// 没进账号组的同账号条目（不看名字；账号组成员按定义不在其中）。每条先按 Blocked 的文档
    /// 定出唯一原因，多条之间按 PanelEntry > ActiveEntry > KindUnsure 取，同级取列表最前，
    /// 取舍与 kind_unsure 无关。PanelEntry 排最前的理由（A1 改判后重新论证，展开见 §9 表后说明）：
    /// 面板来源条目的 kind 恒可信、面板来件的 kind 也恒可信，且面板来件不触发 protected，
    /// 所以**任何一次面板重新导入它都必然进账号组**；进组不等于被替换（§5.4 ① 只 upsert
    /// pick_keeper 选中的那一条），但两种落法都把账号更新到了面板参数——它是留存者就被原地替换，
    /// 不是留存者（例如同组的活动节点按第 1 级胜出）就落进 stale、作存量重复提示、菜单答 y 并入
    /// 留存者（§5.8），所以这一句的出路照做一定有效。被同一来件一并挡下的活动节点是不是也一起
    /// 更新，要看它自身 kind 可不可信——可信则它也进组、按第 1 级被原地替换；若它自身 kind
    /// 也是按备注猜的，只有面板当时的端口恰好等于它现在的端口才进得了组（已知缺口，§13 R18）。
    pub fn blocked_same_account(&self, node: &Node, src: Source) -> Option<(usize, Blocked)>;

    /// 账号组里留哪一条（group 非空）。依次比较：
    ///   1. 是当前活动节点——不打扰正在用的那一条，active 不用改，不弹切换提问；
    ///   2. 导入前已存在（下标 < known）——老名字不被本批新条目或 rc 残留的 -2 顶掉；
    ///   3. 与来件 same_endpoint；
    ///   4. 名字 == wanted；
    ///   5. 列表位置靠前。
    pub fn pick_keeper(&self, group: &[usize], node: &Node, wanted: &str, known: usize) -> usize;

    /// import-v3 用：只在 `self.profiles[..known]` 里找 gate_ok 的（**不看** protected：
    /// import-v3 只跳过、不写，挡住冻结快照的回写正是要的）。优先 active，其次列表最前。
    pub fn find_account_before(&self, known: usize, node: &Node, src: Source) -> Option<&Profile>;

    /// 把名为 name 的条目的来源升到 src；src 等级不高于现有来源时什么都不动。改了返回 true。
    pub fn raise_source(&mut self, name: &str, src: Source) -> bool;

    /// 改名，并在同一个结构里同步 active。new 已被占用或 old 不存在时返回 false、什么都不动。
    pub fn rename(&mut self, old: &str, new: &str) -> bool;

    /// 改掉 token 名：
    ///   profile：新名 = free_name(profile_name("", &node))，即 `<主机>-<kind>`（被占则 -2、-3）；经 rename 同步 active。
    ///   墓碑：   只改显示名，新名 = key_display(key)，key 与 at 不变（墓碑 key 本来就是账号维度，C7）。
    /// 没有 token 名时一个字段都不动（保证 prof == loaded，store_import 不重写文件）。
    pub fn heal_token_names(&mut self) -> Healed;

    /// 把 others 并入 keeper：逐条核对「还在、与 keeper 同账号、不是 active」才 remove（不 bury）。
    /// 规范名规则（D9）：c = profile_name("", &keeper.node)；若被并掉的条目里有一条恰好叫 c，
    /// 且 keeper 的名字是 `c-<数字>`，keeper 经 rename 取回 c（active 跟着改）。
    pub fn merge_into(&mut self, keeper: &str, others: &[String]) -> Merged;
}
```

改动三处既有函数：

- `bury`（`profiles.rs:339-353`）两处改：① 记名前先洗 token：`name: if name_has_token(&p.name) { key_display(&key) } else { p.name.clone() }`；② rc 对同 key 旧墓碑是 `retain` 掉再 `push` 一条新的（`profiles.rs:344-349`），加了 catch-all 之后这会丢掉旧墓碑上以后版本写进来的未知字段。改为先按 key 找到旧墓碑、`remove` 出来取走它的 `extra`，再 `push` 带着这份 `extra` 的新墓碑（名字与 `at` 跟着这次更新）。仍然移到末尾，`TOMBSTONE_CAP` 丢最旧的语义（`profiles.rs:350-352`）不变。删除流程的调用点（`cli.rs:2044-2049`）不动。
- `upsert`（`profiles.rs:310-325`）的 `Replaced` 分支：`*existing = p` 之前把 `existing.extra` 搬到 `p.extra`，否则整条替换会丢掉新版本写进这条 profile 的未知字段（§6.3）。`Unchanged` 分支不改：来源的升级由调用方经 `raise_source` 显式做（§5.4），`upsert` 的「节点与分流都没变就不动」语义保持原样。生产代码里 `upsert` 只有 `cli.rs:541` 与 `import_v3.rs:183` 两个调用点，后者只写 `free_name` 起的新名字，走不到 `Unchanged`。
- `same_account` / `same_endpoint` 的 host 口径（D6），受影响的调用点见 §5.2 末尾。

不变的：`tombstone_key`、`free_name`、`find_same_endpoint`（`import-v3` 仍在用，自动吃到 D6 的新口径）、`remove`（`profiles.rs:363-373`）、`heal_active`（`profiles.rs:277-288`，生产代码里没有调用方，本改动不引入调用）。

### 5.2 kind 门槛（按现状，D8）

| 场景 | 只挡来件会怎样 | 双向门槛 |
|---|---|---|
| 列表：`alice-hy2-direct`（面板，:10000）；粘贴一条备注被改成 `custom` 的住宅 HY2（:40003，同 username，被猜成 Hy2Direct） | 若对粘贴开放：直连节点被改写成住宅端口 | 来件是猜的、端口不同 → 不进组；按新节点导入。列表那条是面板来源、受保护，所以原因报 `PanelEntry`（A1：保护优先），打 `protected_new` 并在「…不变」之后补「同时认不准是直连还是住宅」 |
| 列表：v3 迁来的活动节点，备注 `alice-HY2`、:40003（真实是住宅，被猜成 Hy2Direct）；面板发来 `HY2直连 :10000` | 活动节点被改写成直连，出口从住宅 IP 静默换成 VPS 的 IP | 列表那条是猜的、端口不同 → 不进组；面板直连节点另起一条 |
| 列表：V3 来源、备注不含直连/住宅的 `hysteria2-1778329470`（:10000）；面板发来 `HY2直连 :10000` | 放行，原地替换 | 端口相同 → 放行，原地替换，名字保留 |

表里只有第一行的**报什么原因**受 A1 影响（列表那条是面板来源，对粘贴来件受保护）；谁能进组三行都不变。第二、三行的来件都是面板（`ApiNodes`），`protected` 的第一个判据 `src != Source::ApiNodes` 恒假，条目永远判不到「受保护」，所以第二行仍报 `KindUnsure`、第三行本来就进组。

门槛的实际作用面（写明，免得高估）：

- URI 解析出的 `Residential` kind 必然来自含「住宅」的备注，所以一定可信；门槛**只可能**拦下备注里没有「直连」的 `Direct` kind。直连端口在 4.1 不变，所以 4.1 的住宅换端口不受门槛影响，门槛也不挡路。
- 备注不含「住宅」的 v3 住宅节点被解析成 `Hy2Direct`，与面板 `Hy2Residential` 永不是同一账号，不在覆盖范围（§1.3）。
- 主理人定：门槛按现状，不改成「只对面板来源开放跨端口匹配」。后者会让**面板取失败、退回订阅路径的机器**吃不到本改动：v3 面板，或 `/api/nodes` 回 401 / 404 的链接（停用的用户名链接、面板不认的 token），`cli.rs:3004-3011`、`3018-3020`（§1.5）。

门槛同时修掉 rc 的一个潜伏问题：rc 在「同名且同账号」时无条件覆盖（`cli.rs:536`），面板用户名全是中文、名字回落成 `<主机>-<kind>` 时，一条猜错 kind 的 profile 恰好同名就会被覆盖。

**D6 host 大小写口径是跨调用点的判定变化。** 受影响的调用点：

| 调用点 | 变化 |
|---|---|
| `same_endpoint`（`profiles.rs:203-214`） | host 只差大小写的两条算同一连接 |
| `find_same_endpoint`（`profiles.rs:294-297`）→ `import_v3::import`（`import_v3.rs:149`） | v3 目录里的 host 只差大小写时认作已有、跳过，不再多出 `-2` |
| rc 的命名路径 `cli.rs:529` | 被 §5.4 的新流程取代 |
| 新增 `same_params` / `gate_ok` / `movable` / `account_group` / `blocked_same_account` / `find_account_before` / `merge_into` | 一律经 `same_account`，同一口径（`same_params` 先过 `same_account`，再把 host 抹平后比其余字段） |
| `tombstone_key` | 本来就小写，不变；两者从此一致：「墓碑挡得住」与「账号组命中」不会因大小写分叉 |

`profile_name` 用 `sanitize(host)` 保留原大小写，名字不受影响。面板下发的域名都是小写，实际触发面很小；测试见 §11。

### 5.3 只升不降扩到节点（D1）

| 来件 \ 组内条目 | ApiNodes 条目 | 活动节点（非 ApiNodes） | 其余条目 |
|---|---|---|---|
| ApiNodes | 可替换 | 可替换 | 可替换 |
| Subscription | 仅同参数 | Subscription 条目：可替换；V3 / Paste 条目：仅同参数 | 可替换 |
| Paste / V3 | 仅同参数 | 仅同参数 | 可替换 |

表里的「条目来源」是**命中时已升过级的来源**（§5.5）：一条 V3 活动节点只要被订阅命中过一次（哪怕当时参数完全相同、`upsert` 返回 `Unchanged`），它就已记作 Subscription，之后订阅刷新遇上换端口照常原地替换。

「仅同参数」时除 `label`、`hop` 外 `node` 全等才替换（此时新数据只可能是备注或跳跃段不同：面板改了备注、槽 0 在 4.1 从切片变整段）。r2 这里写的是「同一连接」（端口与 HY2 密码），但 `same_endpoint` 有意不比 `obfs_password`、`sni`、`public_key`、`short_id`（`profiles.rs:200-202`）：面板用 `bui set obfs on` 开了混淆、用户从面板导入过之后，再粘一条开混淆前的旧链接，端口与密码都相同，按「同一连接」会把活动节点的 obfs 密码抹掉并立即 apply，照样断网。所以受保护条目收紧到同参数（r3，核查 C2）。不满足时，账号组为空就走 ③ 起新名，账号组非空就在 ① 里替换留存者；两处都按挡下的原因打一行说明（③ 用 `menu::protected_new`，① 用 `menu::protected_kept`，文案见 §10）。四句基础文案（`Blocked` 的 `kind_unsure` 为假时逐字如下）：

- ③，挡下的是面板来源条目（`Blocked::PanelEntry`）：`与 {旧名} 同一账号但连接参数不同，已按新节点导入为 {新名}，{旧名} 不变；要更新它请从面板重新导入`
- ③，挡下的是非面板来源的活动节点（`Blocked::ActiveEntry`）：`与当前节点 {旧名} 同一账号但连接参数不同，已按新节点导入为 {新名}，当前节点不变；确认新节点能用后可以切换过去`
- ①（组非空，留存者 `{keep}` 已更新），`PanelEntry`：`{旧名} 与 {keep} 同一账号但连接参数不同，{旧名} 不变；要更新它请从面板重新导入`
- ①，`ActiveEntry`：`当前节点 {旧名} 与 {keep} 同一账号但连接参数不同，当前节点不变；确认 {keep} 能用后可以切换过去`；菜单把 `{keep}` 加进切换追问的候选（§5.10）

**受保护条目同时在门槛外时的追加半句（A1）。** `Blocked` 的 `kind_unsure` 为真时，上面四句都在「…不变」之后、分号之前插入 `，同时认不准是直连还是住宅`，其余逐字不变，所以一共八种输出。例如第一句的追加版是：

- ③、`PanelEntry { kind_unsure: true }`：`与 {旧名} 同一账号但连接参数不同，已按新节点导入为 {新名}，{旧名} 不变，同时认不准是直连还是住宅；要更新它请从面板重新导入`

四句的插入点一致（都是「…不变」紧跟分号），所以实现上是 `protected_new` / `protected_kept` 内部按 `kind_unsure` 决定插不插这半句，不必为八种输出各写一份模板。三个文案函数的签名（在 `menu.rs`，`Blocked` 整枚传进去，调用点见 §5.4 ① 与 ③）：

```rust
pub fn protected_new(old: &str, new: &str, why: Blocked) -> String;   // ③：另起了新节点
pub fn protected_kept(old: &str, keep: &str, why: Blocked) -> String; // ①：组里的留存者已更新
pub fn kind_unsure_new(old: &str, new: &str) -> String;               // ③ 且不受保护，没有 kind_unsure 之分
```

前两者只处理 `PanelEntry` / `ActiveEntry`；收到 `KindUnsure` 是调用方写错了，`debug_assert!` 挡住（release 下回落到 `kind_unsure_new` 的措辞，不 panic）。

「连接参数」指端口、HY2 密码、obfs 密码、SNI、Reality 公钥与 short_id；说明句不细分是哪一项，免得每种组合一句、宽度表翻倍。

两句的出路都是照做就有效的：面板来源条目当初能从面板导入，面板重新导入必然把它带进账号组，从而把这个账号更新到面板参数（它是留存者就被原地替换，不是留存者就转成存量重复等合并；展开见 §9 表后说明）；活动节点那一句不叫人「从订阅重新导入」（订阅来件替换不了 V3 / Paste 活动节点，照做只会重复同样的结果），而是给切换这条一定能走通的路。**门槛外也一样**：面板来源条目的 `kind_trusted` 恒真、面板来件的 `kind_trusted` 也恒真，所以「从面板重新导入」这条出路的门槛必过（`gate_ok` 与端口无关地成立），而面板来件不触发 `protected`；活动节点那一句的出路是切换，与门槛无关。所以追加半句只补原因，不改出路。

理由：一条凭据轮换前留下的旧链接（`docs/HANDOVER-bui-c.md:120`：面板轮换同时换 HY2 密码），粘进来就会把活动节点原地改成旧密码并立即 apply，TUN 下整机断网。rc 只在名字恰好等于 `profile_name` 时才会这样，按账号匹配会把这条路径扩到所有名字，所以必须同步收紧。订阅是现拉的服务端数据，不是用户手里的过期副本，所以 Subscription 条目之间照旧互相替换，面板取失败、退回订阅路径的机器（§1.5）上的 4.1 换端口照常原地完成。

代价：活动节点来源是 V3 / Paste、且**从未被订阅或面板导入命中过**（命中过一次来源就已升级，§5.5）的机器，第一次经订阅刷新遇上端口变化时另起一条新节点，要用户切换过去（§13 R11）。

### 5.4 `store_fetched` 新流程（`cli.rs:499-578`）

```rust
struct Stored {
    added: Vec<String>,     // 原 usize（cli.rs:469）；结果行用 len()，菜单追问用名字
    names: Vec<String>,
    replaced: Vec<String>,
    buried: Vec<String>,
    restored: Vec<String>,
    dups: Vec<DupGroup>,    // 新：存量重复，命令行提示、菜单问（§5.8）
    switch_to: Vec<String>, // 新：① 里活动节点被挡下（ActiveEntry）时的留存者名，菜单切换追问的候选（§5.10）
}

struct DupGroup { keeper: String, others: Vec<String> }  // 按 keeper 去重，others 合并去重

fn store_fetched<S: Sys, N: Net, P: Prompt>(
    ctx: &mut Ctx<'_, S, N, P>, prof: &mut Profiles, f: &Fetched,
    src: Source, panel: Option<Panel>, restore: bool,
) -> Stored {
    let mut out = Stored::default();
    let known = prof.profiles.len();
    for node in &f.nodes {
        let wanted = profile_name(&f.user, node);
        let group = prof.account_group(node, src);

        let (name, is_new, old_port, keep_src, panel_split) = if !group.is_empty() {
            // ① 账号已在列表里：原地替换。不看名字，也不看墓碑（它本来就在，不算「回来」，
            //    与 rc cli.rs:493-496 的立场一致，从「同一连接」扩大到「账号组」）
            let k = prof.pick_keeper(&group, node, &wanted, known);
            let keep = prof.profiles[k].name.clone();
            let old_port = prof.profiles[k].node.port;
            let keep_src = prof.profiles[k].source;
            // D7：合并前的整个账号组里找面板来源成员，取它的分流
            let panel_split = group.iter().map(|&i| &prof.profiles[i])
                .find(|p| p.source == Source::ApiNodes)
                .map(|p| p.split.clone());
            let (batch, stale): (Vec<usize>, Vec<usize>) =
                group.iter().copied().filter(|&i| i != k).partition(|&i| i >= known);
            let stale: Vec<String> = stale.iter().map(|&i| prof.profiles[i].name.clone()).collect();
            let batch: Vec<String> = batch.iter().map(|&i| prof.profiles[i].name.clone()).collect();
            // 本批副本不是存量：当场并掉，不提示、不 bury；只删下标 ≥ known 的，前 known 条下标不变
            for d in &batch {
                prof.remove(d);
                for list in [&mut out.added, &mut out.names, &mut out.replaced, &mut out.restored] {
                    list.retain(|n| n != d);
                }
            }
            if !stale.is_empty() { out.push_dups(&keep, stale); }
            // A2（r3）：组非空时，被 §5.3 挡下的同账号条目也要说明（组成员按定义不在 blocked 里）。
            // 不说明的话，活动节点停在旧端口而另一条副本被悄悄换到新端口，用户看不出当前节点没动。
            // KindUnsure 在 ① 不说：门槛外又不受保护的条目本来就当作另一种出口，第一次另起时 ③ 已说过，
            // 每次导入重复是噪音。
            // A1：PanelEntry / ActiveEntry 门槛内外都报，kind_unsure 只决定说明句要不要补那半句（§5.3）；
            // switch_to 记不记只看报出来的是不是 ActiveEntry，与 kind_unsure 无关（门槛外的活动节点照样记，
            // 切换这条出路与门槛无关）。面板条目与活动节点同时被挡下时只报 PanelEntry（优先级不变），
            // 被压下去的那条活动节点不进 switch_to——理由与已知缺口见 §9 表后说明与 §13 R18。
            // b 整枚传给 menu::protected_kept，由它按 kind_unsure 决定措辞；这里不要写成裸变体字面量。
            match prof.blocked_same_account(node, src) {
                Some((i, b @ Blocked::PanelEntry { .. })) =>
                    tell(ctx, menu::protected_kept(&prof.profiles[i].name, &keep, b)),
                Some((i, b @ Blocked::ActiveEntry { .. })) => {
                    tell(ctx, menu::protected_kept(&prof.profiles[i].name, &keep, b));
                    out.switch_to.push(keep.clone());
                }
                Some((_, Blocked::KindUnsure)) | None => {}
            }
            (keep, false, Some(old_port), Some(keep_src), panel_split)
        } else {
            // ② 账号不在组里：先看墓碑（rc cli.rs:520-526 原样）
            if !restore {
                if let Some(t) = prof.tombstone_of(node) {
                    out.buried.push(t.name.clone());
                    continue;
                }
            }
            // ③ 起名。名字只用来起名，绝不据此覆盖。先查没进组的同账号条目（不看名字）
            let name = match prof.blocked_same_account(node, src) {
                Some((i, why)) => {
                    let other = prof.profiles[i].name.clone();
                    let fresh = prof.free_name(&wanted);
                    // why 整枚传下去，protected_new 内部按 kind_unsure 决定要不要补
                    // 「同时认不准是直连还是住宅」那半句（§5.3、A1）
                    tell(ctx, match why {
                        Blocked::PanelEntry { .. } | Blocked::ActiveEntry { .. } =>
                            menu::protected_new(&other, &fresh, why),
                        Blocked::KindUnsure => menu::kind_unsure_new(&other, &fresh),
                    });
                    fresh
                }
                None => match prof.profiles.iter().find(|p| p.name == wanted) {
                    None => wanted,
                    Some(_) => {
                        let fresh = prof.free_name(&wanted);
                        ctx.say(format!("节点名 {wanted} 已被另一个账号占用，新节点命名为 {fresh}")); // rc 原文
                        fresh
                    }
                },
            };
            (name, true, None, None, None)
        };

        // §5.5：分流——非面板来件在组里找到面板成员就沿用它的；来源——三者取等级最高，同级留条目原来的
        let split = panel_split.clone().filter(|_| src != Source::ApiNodes)
            .unwrap_or_else(|| f.split.clone());
        let source = best_source(src, keep_src, panel_split.is_some());
        let r = prof.upsert(Profile { name: name.clone(), node: node.clone(), split, source,
                                      imported_at: rfc3339(ctx.sys), extra: Default::default() });
        if prof.forget(node) && is_new {                                // rc cli.rs:550-552 原样
            out.restored.push(name.clone());
        }
        match r {
            Upsert::Added => out.added.push(name.clone()),
            Upsert::Replaced => {
                let shown = menu::display_name(&name);
                match old_port.filter(|p| *p != node.port) {
                    Some(a) => tell(ctx, format!("更新节点 {shown}：端口 {a} → {}", node.port)),
                    None => ctx.say(format!("更新节点 {shown}")),        // rc 原文，只换显示名
                }
                out.replaced.push(name.clone());
            }
            Upsert::Unchanged => {
                // 节点与分流都没变，来源仍要升（§5.5）：否则 V3 / Paste 条目被订阅或面板命中多少次
                // 都还记作 V3 / Paste，下一次换端口时被 §5.3 误当成受保护条目
                prof.raise_source(&name, source);
                ctx.say(format!("节点 {} 无变化", menu::display_name(&name)));  // rc 原文，只换显示名
            }
        }
        out.names.push(name);
    }
    // panel 与 skipped 两段：rc cli.rs:563-576 原样
    out
}

/// 来件、条目、组内面板成员三者里等级最高的来源；同级时留条目原来的（Paste 与 V3 不互换）。
fn best_source(src: Source, keep: Option<Source>, has_panel_member: bool) -> Source;
```

逐条说明：

- **范围只限这批节点涉及的账号。** 这批没带到的账号，列表里有重复也不动、不提示。
- **同一批里同一账号出现两次（门槛内）：后一条生效。** 第二条命中第一条刚写入（或刚替换）的留存者，走 ①。面板来源不会出现这种批次（`nodes_for` 每种 kind 最多一个节点，`nodes.rs:110-150`）。
- **同一批里同一账号出现两次（kind 是猜的、端口不同）：两条都保留。** 第二条不在门槛内，走 ③，打 `kind_unsure_new`。rc 在这里会覆盖成一条、丢掉一个端口，新行为更安全。
- **本批副本当场并掉的可达场景**：列表里有存量可信条目 X（跨端口，且**非活动、非面板来源**，所以不受保护——第一条猜 kind 的来件对它报的是 `KindUnsure`，A1），同一次粘贴先来一条猜 kind 的（③ 新建 Y），再来一条同端口可信的 → 组 = [X, Y] → 留存者 X（导入前已存在）→ Y 被当场并掉。Y 那条 `kind_unsure_new` 说明已经打出去了，留在输出里（第三方批次才会这样，接受）。
- **① 的 upsert 结果**：端口或密码变了必然 `Replaced`；已经一致则 `Unchanged`，但来源照样经 `raise_source` 升级（`profiles.json` 因此可能重写一次，`prof != loaded`，`cli.rs:694`）。结果行「导入 N 个新节点」只数 `added`，端口变化不算新节点。
- **墓碑与 ①**：`forget` 清掉同 key 的过期墓碑，`is_new = false` 不算恢复，与 rc 已钉住的 `a_live_profile_sharing_the_key_is_refreshed_not_reported_as_deleted`（`cli.rs:10136`）同一语义。
- **① 里的挡下说明（A2）**：组非空而 `blocked_same_account` 报 `PanelEntry` / `ActiveEntry` 时打 `protected_kept`（文案见 §5.3、§10）；`ActiveEntry` 还把留存者名记进 `switch_to`，菜单据此追问切换（§5.10），命令行只打这一句。active 不动、不 apply（§5.7 按内容比较，活动节点内容没变）。**A1 的两点**：这两种原因门槛内外都会报，`kind_unsure` 为真时 `protected_kept` 在「…不变」之后补「同时认不准是直连还是住宅」；`switch_to` 记不记只看报出来的是不是 `ActiveEntry`，与 `kind_unsure` 无关，被 `PanelEntry` 压下去的活动节点不进 `switch_to`（§9 表后说明、§13 R18）。
- **输出行一律经 `tell`（T1 已定）**：`protected_new`、`protected_kept`、`kind_unsure_new`、存量重复提示、改名行、合并行，以及**端口变化行**，都经 `tell`（`cli.rs:1564-1573`）按宽度折行、缩进两列打。依据 F7：`tell` → `show` → `emit` 进 transcript，`outcome_since` 只跳过 `say_aside`，所以这些行计入附加行、菜单会停。两行 rc 原文（不带端口的「更新节点 X」、「节点 X 无变化」）仍走 `say`，只把名字换成显示名，与 rc 一样交给终端折行、不进宽度表。代价：命令行下经 `tell` 的行带两列缩进，与 `say` 的行（命令行 `indent` 为空，`cli.rs:182`）差两列；测试按 `trim()` 比对，接受。

### 5.5 分流与来源只升不降（D7，随本版上）

rc 无条件用这一趟的 `f.split` 与 `src` 覆盖（`cli.rs:541-547`），而且 `Unchanged` 时连来源都不写（`profiles.rs:313-315`）。订阅和粘贴拿不到服务端分流，只有 `default_split()`（`profiles.rs:216-227`）。

本版两件事分开定：

- **分流**：非面板来件时，在**合并前的整个账号组**里找 `ApiNodes` 成员（§5.4 的 `panel_split`），沿用它的 `split`；面板来件用这一趟的。只看留存者会漏掉这种组合：`panel.example.com-hy2-resi`（Subscription，默认分流，active）+ `alice-hy2-resi`（ApiNodes，关键字分流，与来件同一连接）；面板 404 退回订阅（`cli.rs:3007-3010`）→ 留存者是 active 那条 → 若只看留存者，面板分流就随后续合并一起没了。
- **来源**：`best_source` 取来件、留存者、组内面板成员三者中等级最高的（ApiNodes 3 > Subscription 2 > Paste 1 = V3 1），同级留条目原来的。`Replaced` 时它直接写进新 `Profile`；`Unchanged` 时经 `raise_source` 写。结果是**命中即升，永不降低**：
  - V3 活动节点被同参数的订阅刷新一次 → 记作 Subscription → 之后订阅遇上换端口原地替换，不打 `protected_new`；
  - V3 条目被同参数的面板导入一次 → 记作 ApiNodes → 之后过期粘贴按 §5.3 挡下，覆盖不了它；
  - Subscription 条目被粘贴替换（非活动、非面板条目本来就可替换）→ 仍记 Subscription。

判定用 `source` 而不是 `split != default_split()`，避免「`source=Paste` 却带着面板分流」的不一致。保留 `ApiNodes` 来源不会让门槛失真：非面板来件能替换这条，要么两边 kind 都可信，要么端口相同；两种情况下 `same_account` 已保证 kind 相等。保留 `ApiNodes` 来源也意味着这条从此受 §5.3 保护，与「它的权威数据来自面板」一致。升到 `Subscription` 不影响 `kind_trusted`（它只认 `ApiNodes`），只影响 §5.3 表里「Subscription 来件遇 Subscription 条目」那一格，而订阅本来就是现拉的服务端数据。

`Source` 字段在 rc 生产代码里只被 `list --json` 输出（`cli.rs:807`），本版新增的读取点是 `kind_trusted`、`protected`、`best_source` 与 `raise_source`。

### 5.6 `store_import`：先改 token 名

```rust
struct Imported {
    stored: Stored,
    prof: Profiles,
    activated: bool,
    before_active: Option<(Node, SplitRules)>,   // 新：导入前活动节点的内容
}

fn store_import(...) -> Result<Imported> {
    let mut prof = Profiles::load(ctx.sys, ctx.paths)?;          // cli.rs:673，锁内重读
    let loaded = prof.clone();
    let had_active = prof.active_profile().is_some();
    // 新，必须在 store_fetched 之前：之后的「更新节点 X」、说明行、墓碑名单里都不再有 token
    let healed = prof.heal_token_names();
    for (old, new) in &healed.renamed {
        tell(ctx, format!("节点 {} 已改名为 {new}", menu::display_name(old)));   // C7
    }
    let single = inc.src == Source::Paste && inc.fetched.nodes.len() == 1;   // cli.rs:677 原样
    let stored = store_fetched(ctx, &mut prof, &inc.fetched, inc.src, inc.panel, with_deleted || single);
    /* 激活（cli.rs:686-692）、落盘（694-696）、restored 行（704-707）原样；结果行用 stored.added.len() */
    /* 新：stored.dups 非空时打存量重复提示（§5.8），命令行与菜单同一句，经 tell */
    Ok(Imported {
        stored, prof, activated,
        before_active: loaded.active_profile().map(|p| (p.node.clone(), p.split.clone())),
    })
}
```

- 改名、`active`、墓碑显示名、来源升级都在同一个 `&mut Profiles` 里改，由 `store_import` 在同一把锁（`save_import` 的 `take_lock`，`cli.rs:646-652`）内**一次**写盘（`cli.rs:694-696`，C7）。
- C6：端口没变、命中同一连接时 token 名也改——`heal_token_names` 在匹配之前、对全部 profile 做，不依赖这批命中了什么。
- 全部节点都被墓碑挡下时，改名结果照样落盘：`prof != loaded` 自然覆盖。

### 5.7 `apply_import` 按内容判断

`apply_import`（`cli.rs:717-739`）把 `replaced.contains(active)`（`cli.rs:729-733`）换成：

```rust
} else if let Some(now) = prof.active_profile() {
    let same = imported.before_active.as_ref()
        .is_some_and(|(n, s)| *n == now.node && *s == now.split);
    if !same {
        apply_with_ufw(ctx, prof, g)?;
    }
}
```

- 活动节点端口变了 → 内容变 → apply。
- 活动节点只被改名（token 名）或只升了来源 → 内容不变 → 不 apply；就算 apply 了，`write_if_changed` 也不会重启（F3）。
- 只改 label → `Node` 含 label，仍 apply、但配置字节不变不重启，与 rc 注释一致（`cli.rs:734-735`）。
- 过期粘贴被 §5.3 挡下 → 活动节点内容不变 → 不 apply。

### 5.8 存量重复：提示与确认合并（D2）

事实（F5）：4.1 之前旧槽端口的副本能用、从另一个住宅 IP 出去。所以不自动合并，文案也不暗示「死节点」。

- **提示句**（命令行与菜单同一句，`menu::dups_head`，经 `tell` 折行；名单复用 `buried_list` 的个数上限 `BURIED_LIST_MAX`（`menu.rs:774`）与净化，并过 `display_name`，不截断）：
  `同一账号还有 {N} 个节点与服务端这次给的端口或凭据不一致：{a、b}（本次已更新 {X}）`
- **命令行**：只打这一句，外加 `要合并请在菜单 [3] 里导入并答 y`；不问、不改，退出码不变。
- **菜单 `[3]`**：导入之后、**放锁之后**（与墓碑那一问同一位置，spec §0.2 R11），墓碑那一问（含答 y 的第二趟）结束之后问 `要合并吗？`（名单已在 `dups_head` 里，问句本身短，40 列放得下；`[y/N]` 由 `Prompt::confirm` 补，默认 N，EOF 按 N）。答 y：另拿一次锁、重读 `profiles.json`，对每个 `DupGroup` 调 `merge_into`，一次写盘；经 `tell` 打 `已把 {a、b} 并入 {X}`，取回规范名时再打 `节点 {旧名} 已改名为 {新名}`。
- **合并不碰活动节点**：留存者第 1 级是 active，active 只要在组里就是留存者；不在组里（§5.3 挡下）就也不在 `others` 里。`merge_into` 再核对一次 `others` 不含 active，所以合并永不需要 apply（仍按 §5.7 的内容比较兜底）。
- **重读后状态变了**（别的会话删了、改了）：`merge_into` 逐条核对，不满足的跳过；一个都没并成就打 `节点列表已经变了，没有合并`。
- **合并不记墓碑**：墓碑是账号级的，记了等于把留存者也判了删除。
- 被并掉的名字从菜单「切换到新导入的 X？」的候选里去掉（§5.10 的 `after` 过滤自然覆盖）。
- **规范名**（D9）：token 名活动节点改名时 `<主机>-<kind>` 已被同账号占着，它只能先叫 `<主机>-<kind>-2`；答 y 合并掉占着规范名的那条之后，留存者取回规范名。

代价：用户不答 y，每次导入涉及该账号都会提示一次、菜单停一次（§13 R1）。

### 5.9 `import_v3::import`（`import_v3.rs:99-222`）

```rust
let known = prof.profiles.len();   // 本次运行开始前的条数；循环里只会 upsert 新名字（free_name 保证），前 known 条不变
for dir in ... {
    // 解析、面板候选：import_v3.rs:111-144 原样
    if let Some(existing) = prof.find_same_endpoint(&node) { /* import_v3.rs:149-156 原样 */ }
    // 新：同一账号已在导入前的列表里（面板换过端口 / 轮换过密码）→ 认作已有、跳过，绝不拿 v3 的冻结快照回写
    if let Some(existing) = prof.find_account_before(known, &node, Source::V3) {
        let name = existing.name.clone();
        if dir_name == v3_active { r.active = Some(name.clone()); }
        r.existing.push(name);
        continue;
    }
    // 墓碑 → forget → 目录名 + free_name → upsert：import_v3.rs:157-190 原样
}
```

- **只跳过、不替换、不合并、不升来源**：`<base>/configs` 是 v3 时代冻结下来的快照（`import_v3.rs:145-148`），列表里同一账号只可能比它新；V3 是最低一级来源，也没有可升的。
- **只匹配 `[..known]`**：第一次跑时，v3 目录里同一账号两个端口的快照都会导入（与 rc 相同），不会因为后读到的目录命中本批刚写入的那条而被跳过。
- **不改 token 名、不改字段**：`import` 保持 `import_v3.rs:198-200`「没有新节点就一个字段都不动」。注意这不等于「不写盘」：`run` 在一个新节点都没有时，只有**没有残留 v3 单元**（或无事可做）才提前返回（`import_v3.rs:300-313`）；残留单元还在时它照常往下走、在 `import_v3.rs:350` 把 `prof` 原样 `save` 一次（字段未变，只有 `--panel` / `--mode` 覆盖会改，`315-327`，与本改动无关）。本改动不改这一行为。墓碑名单经 `buried_list` → `display_name` 打码（§8.3），命令行 `buried_skipped`（`cli.rs:1105`）与菜单 `buried_head`（`cli.rs:2979`）都覆盖到。
- `Report::existing` 的文档（`import_v3.rs:38-40`）改成「同一个连接，或导入前就在列表里的同一账号」。
- 命令行结果行 `v3 的 N 个节点都已导入过`（`cli.rs:1165-1171`）不改文案。

### 5.10 菜单 `[3]` 的追问（`cli.rs:2862-2959`）

rc 用「导入前后名字求差集」判断新节点（`cli.rs:2874-2878`、`2935-2941`）。改名与合并会让差集里冒出「新名字」，误问「切换到新导入的 X？」。改为：

- 删掉 `before`；
- 候选 = 第一趟 `save_import` 的 `stored.switch_to`（① 里活动节点被挡下时的留存者，A2），接着是 `stored.added`，再加上墓碑第二趟的 `stored.added`（`cli.rs:2929` 目前把第二趟的返回值丢了，要接住）；`switch_to` 排最前，因为它关系到当前节点正停在旧参数上；
- 过滤掉 `after` 里已经不存在的名字（合并掉的自然出局），其余判断（活动节点已是候选就不问，`cli.rs:2950-2952`）原样；仍然只问第一个候选，与 rc 相同；
- 问句：候选来自 `switch_to` 时是 `切换到 {keep}？`（它不是新导入的），来自 `added` 时是 rc 原文 `切换到新导入的 {X}？`；
- 提问顺序：墓碑 → 存量重复 → 切换；每一问都算停顿（`asked_is_a_pause`，`cli.rs:1459`）；
- 追问句里的名字过 `display_name`（`cli.rs:2954`）。

## 6. 数据模型与兼容

### 6.1 结论

| 项 | 结论 | 依据 |
|---|---|---|
| 持久化字段 | **不加**。账号身份（kind + host + 凭据主体）与改名都在导入时从 `node` 字段现算（C1） | 它是 `node` 的纯函数，多存一份就多一个会过期的副本；墓碑 key 已是它的落盘形式 |
| `source` 取值 | 仍是既有四个变体，只是写入时机多了「`Unchanged` 时升级」 | 不加变体（C4 第 3 条） |
| `Profile` / `Profiles` / `Tombstone` | 只加代码侧 `#[serde(flatten)] extra`（§6.3），没有未知字段时不产生任何 JSON 键 | `profiles.rs:58-97` |
| `SCHEMA_VERSION` | 仍是 1，数值随意 | `load` 只做 `from_slice`，从不读版本号（`profiles.rs:253-261`） |
| 迁移代码 | 不写 | 改名、合并、升来源都是导入时对值的正常改写，与 CLAUDE.md「没有迁移块」一致 |
| 新版读旧文件 | 照常；token 名第一次导入时改掉，存量重复第一次导入时提示 | — |
| 没有 token 名、没有重复、来源已是最高的机器 | 重复导入同一面板不重写文件，与 rc 逐字节一致 | `prof != loaded`（`cli.rs:694`） |
| `runtime.json` | 不存 profile 名 | — |

### 6.2 `profiles.json` 只做加法（C4）

以后每个版本（含本版）改 `profiles.json` 都守这几条：

1. 不改已有字段的类型或形状；
2. 不删、不改名必需字段。基准是 4.0.0：`Profile` 五个字段（`name`、`node`、`split`、`source`、`imported_at`，`git show v4.0.0:crates/bui-c/src/profiles.rs` 第 57-65 行），`Profiles` **八个**字段（`schema_version`、`active`、`mode`、`socks_port`、`http_port`、`auto_update`、`panel`、`profiles`，第 67-77 行）。第九个字段 `deleted` 是 4.0.1 才加的（rc2 `profiles.rs:92-96`），带 `serde(default)`、可缺省；
3. 不给已有枚举加变体：`mode`（`profiles.rs:17-22`）、`source`（`25-32`）、`node.kind`（`bui-schema nodes.rs`）——旧版本读到不认识的变体会整份解析失败（`load` 报错，`profiles.rs:260`），等于「读不了」；
4. `schema_version` 的数值随意，不能拿它做兼容判断；
5. 每个新增字段要么能重建（下次导入会再算出来），要么丢了无妨。

本版对照：没有新增持久化字段、没有新变体，第 1-3 条自然满足。

### 6.3 catch-all（C3）

```rust
pub struct Profiles {
    /* rc2 既有九个字段原样（4.0.0 的八个 + deleted）；deleted 仍在 profiles 之后、skip_serializing_if 原样 */
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}
pub struct Profile  { /* 既有五个字段 */ #[serde(flatten)] pub extra: serde_json::Map<String, serde_json::Value> }
pub struct Tombstone { /* key, name, at */ #[serde(flatten)] pub extra: serde_json::Map<String, serde_json::Value> }
```

- 作用：以后的版本给这三个结构加字段，降回**已经带 catch-all 的版本**（4.0.2 起）时，读进 `extra`、写回原样带出，不丢。
- `Profile` 被整条替换时要搬 `extra`（`upsert` 的 `Replaced` 分支，§5.1）；`Tombstone` 被同 key 重复 `bury` 时同样搬 `extra`（§5.1，测试 14a）；改名、合并、升来源都是在原结构上改，不丢。
- **`node` 不加**：`node` 由服务端下发、每次导入整条覆盖，未知字段下次导入就回来了（可重建）；给 `bui-schema` 的 `Node` 加 catch-all 会让「未知字段也是 `Node` 的一部分」，削弱总纲 C1 契约（`bui-schema` 是端口、标签、格式的唯一真源）。
- **救不了降到 4.0.0**：4.0.0 没有 catch-all，它写回时照样丢掉自己不认识的字段（含 `deleted`）。catch-all 只保护「降到已带它的版本」。
- 没有未知字段时 `extra` 为空，flatten 不产生任何键；`a_file_without_deleted_loads_and_saving_without_tombstones_is_byte_identical`（`profiles.rs:781`）必须继续逐字节通过（§11 测试 21）。

### 6.4 降级

| 降到 | 读 | 写回 | 行为 |
|---|---|---|---|
| rc1 / rc2（4.0.1） | 照常（结构相同，不加 `deny_unknown_fields`，`profiles.rs:94`） | 本版没有新增持久化字段，不丢东西 | 已改名、已合并、已升级来源的结果留在文件里；之后导入恢复「端口变了另起新节点」 |
| 4.0.0 | 照常（`git grep deny_unknown_fields v4.0.0 -- crates/bui-c/src/profiles.rs` 无命中） | **① 丢墓碑**：4.0.0 的 `Profiles` 没有 `deleted` 字段（第 67-77 行），写回即丢，删过的节点下次导入会回来（menu-v2 spec R16 ②，`docs/superpowers/specs/2026-09-13-bui-c-menu-v2-design.md:170`）；**② token 名复发**：4.0.0 没有 token 过滤，用 token 链接导入会重新生成 token 名 | 节点本身不丢 |

现实中的降级路径（与 §12.3 互相引用）：

- 没记面板（粘贴、订阅、v3 面板、http 面板导入，`cli.rs:601-610`、`618-626`）或跟随的面板缓存还是 4.0.0 的客户端，走 GitHub `releases/latest`（`update.rs:155-165`），4.0.1 正式版前那就是 4.0.0，版本号不同就换（`update.rs:369-371`）。
- rc1 代码的面板每日自检曾把缓存改写成 4.0.0（`e72eac1` 修掉的事故），跟随它的客户端被降到 4.0.0；bwg-tizi 与 bwg-rick 已升 rc2，这条路径对它们已关闭（§12.3）。

**C5 判断**：触发条件是「某版本 4.0.0 读不了，或 4.0.0 写回会丢不可重建数据」，满足时进 rc 前关掉 rc 测试机 baiyi 的自动更新，直到正式版成为 GitHub latest。本改动不加持久化字段（C1）、不改已有字段、不加枚举变体（C4），4.0.0 读得了，写回也不会丢任何**本改动**引入的数据（改名、合并、升来源都落在 4.0.0 认识的字段里），所以**不触发**。表里 4.0.0 丢墓碑是 4.0.1 引入 `deleted` 时就存在的既有情形，处置不在本设计。

### 6.5 往返测试（C2）

测试里定义一份只含 4.0.0 字段的结构：`Profile400`（五个字段，照抄 `v4.0.0` 的 `profiles.rs:57-65`）与 `Profiles400`（**八个字段，不含 `deleted`**，照抄第 67-77 行），`node` 用 `bui_schema::nodes::Node`。步骤：

1. 本版构造一份带 token 名已改、有墓碑、有未知字段的 `Profiles` 并保存；
2. 以 `Profiles400` 读入再写回（模拟 4.0.0）。**断言**：写回的 JSON 里没有 `deleted` 键，也没有第 1 步的未知键（墓碑与未知字段确实丢了，模拟才算数）；
3. 本版再 `Profiles::load`：`deleted` 为空；对同一个面板节点（端口已变）求 `account_group`，仍命中第 1 步里的同一条 profile，名字不变。

另有一条「未知字段写回不丢」：本版读入一份在三个结构上都带未知键的文件，保存后三处未知键逐字保留（§11 测试 18）。

## 7. 墓碑交互

| 情形 | 规则 | 依据 |
|---|---|---|
| 账号组非空 | 走 ①：原地替换，不看墓碑；`forget` 清同 key 过期墓碑，不算恢复 | `cli.rs:493-496`、`550-552` 的立场，从「同一连接」扩大到「账号组」 |
| 账号组为空、有墓碑、没给 `--with-deleted` | 跳过，记进 `buried`；命令行打 `buried_skipped`（`menu.rs:810-818`），菜单问 `BURIED_ASK`（`menu.rs:808`） | key 不含端口，4.1 改端口后删过的账号仍然挡得住 |
| 账号组为空、有墓碑、`--with-deleted` / 菜单答 y / 单条粘贴 | 新建并清墓碑，记进 `restored` | `cli.rs:677`、`684`、`704-707` 不变 |
| 存量重复合并（菜单答 y）、本批副本当场并掉 | **绝不 `bury`**：墓碑是账号级的，记了等于把留存者也判了删除 | `profiles.rs:339-353` |
| 账号只有门槛外或受保护的 profile，来件有墓碑 | 组为空 → 墓碑挡下。不会复活删掉的节点 | 本设计不引入第二套「账号还活着吗」口径 |
| 菜单答 y 的第二趟 | `live.is_deleted(n)` 过滤（`cli.rs:2928`）仍然正确：第一趟被 ① 清掉墓碑的账号不会进第二趟 | — |
| 删除 | 不改：每个被删条目一条墓碑（`cli.rs:2044-2049`） | — |
| 删掉与来件同一连接的副本、留着另一端口的副本 | 下次导入账号还有活着的条目 → 走 ①，刷新它、清墓碑，活着的赢 | `cli.rs:10136` 钉住的场景 |
| **删掉新端口副本、留着旧端口副本，再导入** | **有意的新行为**：rc 下新端口节点命中墓碑被跳过（`cli.rs:515-526` 先查同一连接、再查墓碑）；本版旧端口副本在组里 → 被原地换到新端口、墓碑清掉，不提问。理由：墓碑是账号维度，用户留着这个账号的一条节点，就是还要这个账号；删掉的只是一个端点。例外：留着的副本受 §5.3 保护而来件不是面板、又不同参数时，组为空 → 墓碑照常挡下 | `cli.rs:10136` 只覆盖「留下的与来件同一连接」；新测试见 §11 测试 54 |
| 墓碑里的 token 名 | 只改显示名：导入时 `heal_token_names` 改成 `key_display(key)`；新删除时 `bury` 先洗。key 与 `at` 不变（C7）。这是「墓碑名记录删除当时的名字」的唯一例外 | `profiles.rs:68-71` |
| import-v3 打出的墓碑名 | import-v3 不写盘、不洗 token，但名单经 `display_name` 打码 | §5.9、§8.3 |

## 8. 命名、改名与打码

### 8.1 规则

| 存量名字 | 处理 | 理由 |
|---|---|---|
| v3 目录名（A 类） | 沿用 | `import_v3.rs:167-169` 有意保留，用户拿它 `switch` |
| `-2` / `-3` 后缀（C 类） | 沿用，不回收；唯一例外是菜单合并时留存者叫 `<主机>-<kind>-N` 而被并掉的恰好叫 `<主机>-<kind>`（§5.8） | 回收会连带 active、脚本、切换提问一起变，不增加安全性 |
| `<主机>-<kind>`（D 类）、`alice-<kind>` | 沿用 | 同上 |
| **token 名（B 类）** | **导入时改成 `<主机>-<kind>`**（被占则 `-2`），端口没变、命中同一连接时也改（C6） | 安全：token 等价订阅凭据（spec `2026-09-14-subscription-token-design.md` §1），会出现在 list、菜单、status、删除确认块、「上次：」行（`source.rs:91-94` 的注释） |

token 名统一改成 `profile_name("", node)` 而不是用这一趟的用户名：改名要覆盖这批没带到的节点，只有主机和 kind 永远拿得到；也与 rc 退回订阅时的命名一致（`source.rs:99-107`）。

### 8.2 改名的连带影响

| 受影响处 | 处理 |
|---|---|
| `active` | `rename` 在同一个 `&mut Profiles` 里同步，与墓碑显示名一起由 `store_import` 在同一把锁内一次写盘（`cli.rs:646-652`、`694-696`，C7） |
| apply | 按内容比较（§5.7），改名不 apply、不重启（F3） |
| 菜单「切换到新导入的 X？」 | 改用 `Stored.added`（§5.10），改名与合并都不触发 |
| 墓碑 `name` | 非 token 名不改；token 名改成 `<主机>-<kind>` |
| 输出 | 每条改名一行 `节点 <旧名（打码）> 已改名为 <新名>`，经 `tell` 折行（C7） |
| `pending.json` | 只有名字，收敛只看文件在不在，不受影响（F3） |
| 删除确认期间被导入改名或合并 | `delete::snapshot`（`delete.rs:145`）比对（`cli.rs:2027-2037`）→ `SNAPSHOT_CHANGED`，让用户重选，安全 |
| `list --json` / `status --json` | `name`、`active` 会变（只在有 token 名的机器上）。**CHANGELOG 注明：导入时改名会让按旧 token 名写的脚本（`bui-c switch <旧名>` 等）失效** |

### 8.3 人读输出打码（D3）

```rust
/// 人读输出里的节点名：先 sanitize（menu.rs:601-614），再把每个 token 段打码成前 4 位加「…」。
/// `<合成 token>-hy2-resi` → `0123…-hy2-resi`（合成 token 见 §11 门禁段）。没有 token 段时原样借出。
pub fn display_name(name: &str) -> Cow<'_, str>;
```

打码只改显示，不写盘，只读入口也能用，与 R11、决策 10 不冲突；从不导入的机器也覆盖到。宽度只会变窄，截断（`truncate_middle`，`menu.rs:665`）在打码之后做。

| 打码的地方 | 位置 |
|---|---|
| `list` 表格、菜单节点列表 | `menu::render_nodes`（`menu.rs:1066`），名字在 `menu.rs:1111` |
| `status` 人读部分、主菜单头 | `menu::render_status`（`menu.rs:859`），名字在 `menu.rs:885` |
| 删除确认块 | `menu.rs:1663`、`1710`、`1776`；`render_switch_to` 的名字在 `menu.rs:1983` |
| 删除的错误与摘要 | `delete.rs:44-64`（`PlanError` 的 `Display`）、`delete.rs:214-222`（`cli_summary`）、`cli.rs:2077-2117` |
| 墓碑名单（import 与 **import-v3** 两条路） | `menu::buried_list`，名字在 `menu.rs:783`；经 `buried_head`（`cli.rs:2923`、`2979`）与 `buried_skipped`（`cli.rs:918`、`1105`） |
| 「上次：」行 | 拼摘要的调用方用显示名（`cli.rs:1510-1519` 等），`fit_name_in_last`（`menu.rs:755-757`）收到的也是显示名 |
| 导入输出 | `cli.rs:556`、`559`（更新 / 无变化）、`726` 与 `1196`（`CURRENT_NODE_HEAD`）、`2954`（切换追问）；本版新增的改名、合并、说明、端口变化行 |
| 切换 | `cli.rs:826`、`832`、`838` |

**不打码**：`status --json`（`cli.rs:777-790`）、`list --json`（`cli.rs:800-813`）、`delete --json`（`cli.rs:1005-1009`）与 `profiles.json`。理由：它们是机器接口；能跑它们的人有 root，能直接读 `profiles.json`，打码挡不住，还会让字段有损、按名字写的脚本坏掉。

后果：打码后的名字不能直接敲进 `bui-c switch` / `bui-c delete`。出路：菜单里按编号选；从 `list --json` 取原名；或者导入一次让它改名。发版说明写这一句。

### 8.4 安全边界

改名只在导入时发生：命令行 `bui-c import`、菜单 `[3]`，两条路都持锁、都是用户主动发起。从不导入的机器文件里一直留着 token 名，但人读输出已经打码。发版说明写明「4.0.0 时用 token 订阅链接导入过的机器，升级后随便导入一次，节点名里的 token 会自动去掉；导入前屏幕上只显示前 4 位」。

## 9. 边界情况

| 场景 | 处理 | 落点 |
|---|---|---|
| 同账号已有两条（`alice-hy2-resi` 40003、`alice-hy2-resi-2` 40007），面板推 40009 | 组两条 → 留存者按 active > 导入前已存在 > 同一连接 > wanted > 列表 → 替换一条；另一条是存量重复：命令行提示，菜单问、默认 N | §5.4、§5.8 |
| 两条都是遗留名字（`hysteria2-<ts>`、`bob-hy2-resi-2`） | 同上，不会多出第三条 | §5.4 |
| 活动节点是旧端口副本，另一条已在新端口（rc 留下的 `-2`） | 留存者 = 活动节点，替换到新端口并 apply；`-2` 提示 / 菜单确认合并 | §5.4、§5.7、§5.8 |
| 非活动的 v3 名 + rc 残留的新端口 `-2`（两条都是存量），面板推新端口 | 第 2 级打平，第 3 级同一连接选中 `-2` 作留存者（`Unchanged`，来源照样升）；v3 名成为存量重复，只提示、菜单问，不会被静默删掉 | §5.1、§5.8 |
| 非活动的 v3 名（存量）+ 本批刚写入的同端口条目 | 第 2 级选中 v3 名作留存者，本批条目当场并掉，老名字保住 | §5.1、§5.4 |
| 第二台服务器上的 `-2` 换端口（rc 会涨成 `-3`） | 账号看 host，直接命中 `-2`，后缀不涨 | §5.4 ① |
| token 名 + 端口变 | 先改名为 `<主机>-<kind>`，再按账号原地替换 | §5.6 |
| token 名 + 端口没变（命中同一连接） | 照样改名（C6） | §5.6 |
| token 名改名时 `<主机>-<kind>` 被家人账号占着 | 改成 `-2` | §8.1 |
| token 名活动节点改名时 `<主机>-<kind>` 被同一账号占着 | 先叫 `-2`；两条成存量重复；菜单答 y 合并后留存者取回规范名 | §5.8 |
| HY2 换密码（同端口或连端口一起），面板来件 | 组命中，不再依赖名字碰巧相等 | §5.4 ① |
| **过期密码的粘贴遇上活动节点或面板节点** | 受保护且不同参数 → 不进组，按新节点导入，打 `protected_new`（按原因两种说法，门槛外再补半句）；active 不变、不 apply | §5.3 |
| **面板开了 obfs 之后粘贴开混淆前的旧链接**（端口与密码都相同） | 同一连接但不同参数 → 受保护条目不被覆盖，按新节点导入并说明；active 不变、不 apply | §5.3（C2） |
| **订阅刷新：活动的 V3 节点被挡下，另一条 Subscription 副本在组里** | ① 替换那条副本；打「当前节点 X 与 {keep} 同一账号但连接参数不同，当前节点不变；确认 {keep} 能用后可以切换过去」；active 不变；菜单问 `切换到 {keep}？` | §5.4 ①、§5.10（A2） |
| 同一条既受保护（面板来源或活动节点）又在门槛外 | 原因报 `PanelEntry` / `ActiveEntry`（A1：保护优先，门槛次之，两条都报），`kind_unsure` 为真，说明句在「…不变」之后补「同时认不准是直连还是住宅」；**不**打 `kind_unsure_new` | §5.1、§5.3（A1） |
| 同时有面板条目与活动节点被挡下 | 仍只报 `PanelEntry` 那一句（优先级不受 A1 影响）；被压下去的活动节点不进 `switch_to`。理由与已知缺口见本表之后的说明 | §5.1、§9 表后说明、§13 R18 |
| 订阅刷新遇上 Subscription 来源的活动节点，端口变了 | 允许替换（面板取失败、退回订阅路径的机器上的 4.1 换端口） | §5.3 |
| V3 / Paste 活动节点先被同参数的订阅刷新过一次（`Unchanged`），之后订阅换端口 | 第一次命中已把来源升成 Subscription → 第二次原地替换，不打 `protected_new` | §5.5 |
| V3 / Paste 活动节点从未被订阅或面板命中过，订阅刷新时端口已变 | 仅同参数才替换 → 另起一条（组为空时），打「当前节点不变；确认新节点能用后可以切换过去」；组里另有可替换的副本时按 §5.4 ① 替换那一条并说明 | §5.3、§13 R11 |
| V3 条目被同参数的面板导入命中过，之后粘贴过期链接 | 已升成 ApiNodes → 受保护，粘贴另起一条 | §5.5 |
| Reality uuid 轮换 | 不在范围：新 uuid 就是新账号 → ③ 起 `-2`，旧的留下 | §2 非目标 |
| host 变了（换域名 / IP） | 不算同一账号，另起新节点（`profiles.rs:153-156` 的理由） | 不改 |
| host 只差大小写 | 本版起算同一账号，与墓碑 key 一致（D6） | §5.2 |
| Reality 直连与住宅共用 uuid；HY2 直连与住宅共用 username | kind 不同，永不匹配、永不合并 | §4 |
| 同 host 家人账号 | 凭据主体不同，永不匹配；撞名走 `-2` | §5.4 ③ |
| 粘贴一条备注被改过的住宅 HY2（猜成直连，端口不同） | 门槛挡下，新增一条；列表那条**不受保护**（非活动、非面板来源）时打 `kind_unsure_new`（名字不同也打），**受保护**（面板来源或活动节点）时按 A1 打的是 `protected_new` 并补那半句 | §5.2、§5.4 ③ |
| 猜错 kind 的活动 profile 遇上面板直连节点 | 门槛挡下，活动节点原样不动 | §5.2 |
| 猜 kind 的 V3 / Paste profile 与面板节点同端口 | 同端口放行（面板来件不受 §5.3 限制），原地替换，名字保留，来源升成 ApiNodes | §5.2、§5.5 |
| 备注不含「住宅」的 v3 住宅节点，4.1 后导入 | kind 不同，永不同账号 → 另起 `*-hy2-resi`，旧的留下（兼容段下线前可用） | §1.3 |
| 同一批同账号两次，门槛内 | 后一条生效，只留一条 | §5.4 |
| 同一批同账号两次，kind 是猜的、端口不同 | 两条都保留（rc 会覆盖成一条） | §5.4 |
| 本批刚写入的副本落进后一条的账号组 | 当场并掉，不提示、不 bury，不算新节点 | §5.4 |
| 墓碑与活着的同账号 profile 并存 | 活着的赢，刷新并清墓碑（含「删新端口留旧端口」这一有意变化） | §7 |
| 同账号所有 profile 都删了，端口又变了 | 墓碑挡下；`--with-deleted` 以 `wanted` 名字加回 | §7 |
| 订阅 / 粘贴刷新面板来的节点（同一连接） | 分流与来源保留面板那份 | §5.5 |
| 订阅刷新时面板来源成员不是留存者 | 仍从组里取面板分流，来源升成 ApiNodes | §5.5 |
| 被替换的是活动节点 | 内容比较 → apply；TUN 短暂重连是既有代价 | §5.7 |
| 替换后 apply 失败 | 不回滚；`profiles.json` 已是新端口、跑着旧配置；兼容段 REDIRECT 在时旧端口仍可用，下一次 apply 用上新数据 | F4 |
| 菜单问合并期间别的会话改了列表 | `merge_into` 重读后逐条核对，不满足的跳过 | §5.8 |
| import-v3 重跑，列表里同账号已被面板换过端口 / 密码 | 认作 `existing`，不回写 | §5.9 |
| import-v3 首次跑，v3 目录里同账号两个端口 | 两条都导入（只匹配 `[..known]`） | §5.9 |
| import-v3 命中墓碑，墓碑名是 token 名 | 名单打码 | §8.3 |
| 服务端 4.1 后槽 0 用户 | 端口仍是 40000、只变 hop → 同一连接 | §1.1 |
| 删除中断留下 `pending.json`，同时导入改名 | 收敛只看文件在不在，不冲突 | F3 |
| 并发导入 | 改名、替换、升来源在锁内、基于重读的 profiles 做（`cli.rs:652`、`673`，R16）；合并另拿锁重读 | 不改 |
| 这批全被墓碑挡下 | 不动 panel（`cli.rs:565`）、不激活；token 改名照样落盘 | §5.6 |
| 从来没导入过的 token 名机器 | 文件不动；人读输出打码；`--json` 原名 | §8.3 |

**「同时有面板条目与活动节点被挡下」为什么仍只报 `PanelEntry`（A1 改判后重新论证）。** r3 给的理由是「从面板重新导入会把两条一起更新」，它依赖旧定序下「`ActiveEntry` 蕴含门槛内」这个隐含前提。A1 改成保护优先之后前提失效：门槛是双向的（`gate_ok` 要求端口相同，或**两边**的 kind 都可信），一条 kind 按备注猜、端口又不同的活动节点，面板来件对它同样过不了门槛。按新定序重新论证：

- **面板来源条目自己必然救得回。** 它的来源是 `ApiNodes`，`kind_trusted` 恒真；面板来件的 `kind_trusted` 也恒真，所以 `gate_ok` 与端口无关地成立。而 `protected` 的第一个判据是 `src != Source::ApiNodes`，面板来件下恒假，于是 `movable` 恒真——**任何一次面板重新导入它都必然进账号组**。进组不等于被替换：§5.4 ① 里只有 `pick_keeper` 选中的那一条被 `upsert` 原地替换，组内其余下标 < `known` 的成员进 `stale` → `push_dups`，只作存量重复提示（命令行只提示、一条都不动；菜单答 y 才并掉，§5.8）。但两种落法都把这个账号更新到了面板参数：**它是留存者** → 它自己被原地替换；**它不是留存者**（例如同组还有一条自身 kind 也可信的活动节点，按 `pick_keeper` 第 1 级「是当前活动节点」胜出）→ 留存者已带着面板参数，它被点名成存量重复、答 y 即并入留存者，不会作为一条没人管的旧条目继续漂着。所以「要更新它请从面板重新导入」这条出路照做仍然有效，排最前是对的，与门槛内外无关。
- **被压下去的活动节点是不是也一起更新，看它自身 kind 可不可信。** 可信（来源 `ApiNodes`，或备注含「直连」「住宅」）→ `gate_ok` 同样与端口无关地成立，它也必然进组；它是当前活动节点，`pick_keeper` 第 1 级就选它，所以它正是被原地替换的那一条（这时轮到面板条目落进 `stale` 作存量重复提示，见上一条）。不可信 → 只有面板当时的端口恰好等于它现在的端口，它才进得了组、才救得回；否则这次面板重新导入既不替换它、也不把它列进存量重复（它根本不在组里），更新不了它（§12.10 已写过「门槛外的猜 kind 条目连面板导入也不会更新」，A1 之后这句从边缘情形变成本行的核心场景）。
- **口径（本轮定）**：优先级不变，仍只报 `PanelEntry` 一句，不新增消息类型；`switch_to` 的收集也不变，只看 `blocked_same_account` 实际报出来的那一条是不是 `ActiveEntry`，被压下去的活动节点不进候选（K7 因此不用改）。代价是上面第三种情形里那条活动节点会继续卡在旧参数上，而**组非空、走 §5.4 ① 的这一轮**又不再单独提示指向它——**已知缺口，本设计不解决**，记在 §13 R18。（组为空走 ③ 时不落进这个缺口：那里另起了一条新节点，菜单仍会按 `added` 问「切换到新导入的 X？」，§5.10，用户并非全无提示。）

## 10. 用户可见变化

### 命令行 `bui-c import`（`--panel/--user`、`--sub`、`-`）

| 场景 | 输出 |
|---|---|
| 端口变了 | `更新节点 alice-hy2-resi：端口 40003 → 40000`（经 `tell`，两列缩进、按宽度折行；rc 是「导入 1 个新节点」加一条多余条目） |
| 只换密码或参数 | `更新节点 alice-hy2-resi`（rc 原文，`say`） |
| 参数全同 | `节点 alice-hy2-resi 无变化`（rc 原文；来源可能在背后升了一级，不另打一行） |
| 存量重复 | `同一账号还有 1 个节点与服务端这次给的端口或凭据不一致：alice-hy2-resi-2（本次已更新 alice-hy2-resi）` + `要合并请在菜单 [3] 里导入并答 y` |
| token 改名 | 每条一行 `节点 0123…-hy2-resi 已改名为 panel.example.com-hy2-resi` |
| 过期链接被面板来源条目挡下（组为空） | `与 alice-hy2-direct 同一账号但连接参数不同，已按新节点导入为 panel.example.com-hy2-direct，alice-hy2-direct 不变；要更新它请从面板重新导入` |
| 过期链接或订阅被非面板来源的活动节点挡下（组为空） | `与当前节点 hysteria2-1785892136 同一账号但连接参数不同，已按新节点导入为 panel.example.com-hy2-resi，当前节点不变；确认新节点能用后可以切换过去` |
| 组非空、面板来源条目被挡下 | 端口变化行或「更新节点 X」之外再打 `alice-hy2-resi 与 panel.example.com-hy2-resi 同一账号但连接参数不同，alice-hy2-resi 不变；要更新它请从面板重新导入` |
| 组非空、活动节点被挡下 | 同上再打 `当前节点 hysteria2-1785892136 与 panel.example.com-hy2-resi 同一账号但连接参数不同，当前节点不变；确认 panel.example.com-hy2-resi 能用后可以切换过去` |
| 上面四种，被挡下的条目同时在门槛外（A1） | 在「…不变」之后插入 `，同时认不准是直连还是住宅`，其余逐字不变。以「过期链接被面板来源条目挡下（组为空）」那一行为例：`与 alice-hy2-direct 同一账号但连接参数不同，已按新节点导入为 panel.example.com-hy2-direct，alice-hy2-direct 不变，同时认不准是直连还是住宅；要更新它请从面板重新导入` |
| 门槛挡下、**不受保护**的同账号（非活动、非面板来源的条目） | `与 hysteria2-1785892136 同一账号但端口不同，认不准是直连还是住宅，按新节点导入为 panel.example.com-hy2-direct` |
| 结果行 | `导入 {added.len()} 个新节点，共 {N} 个`（`cli.rs:697-701`）；合并、改名不塞进结果行：「上次：」行在 40 列终端只有 31 列（`cli.rs:2034` 的注释） |
| 墓碑 | 文案不变，名单打码 |
| 退出码 | 不变 |

### 菜单 `[3]`

- 改名行、说明行、端口变化行、存量重复提示都经 `tell`，按 `outcome_since`（`cli.rs:1428`）算附加行，会停下来让人看到一次（F7）。
- 存量重复时多一问 `要合并吗？[y/N]`，默认 N。
- 端口变化、改名、合并都不再弹「切换到新导入的 X？」；只有真正新增的节点才问（活动节点被挡下另起的那一条也算新增，会问，与说明句的「切换过去」对得上）。组非空而活动节点被挡下时问 `切换到 {keep}？`（§5.10）。
- 活动节点被替换时静默重新 apply，与 rc 相同；过期粘贴被挡时不 apply。

### 菜单 `[7]`→`[3]` 与 `bui-c import-v3`

同账号的节点记进 `existing`，不再多出 `-2`；结果行文案不变；墓碑名单打码。

### `list` / `status` / `delete` / 主菜单

人读输出里 token 段显示成前 4 位加 `…`；`--json` 原样。

### `--json`

- `import` / `import-v3` 仍然没有 `--json`（rc 只有 status / list / delete 有，`cli.rs:777-813`、`1005-1009`），本次不加。
- `list --json` 的 `name`、`port`、`source` 自然反映改名、合并、新端口与来源升级；键不变；名字不打码。

### 文档

`CHANGELOG.md` 新开 `## [4.0.2] - 未发布`（根 `Cargo.toml` 同步 4.0.2，`scripts/release/check-version.sh` 卡一致性），与 `docs/HANDOVER-bui-c.md` 写：按账号匹配、端口变了原地替换；**活动节点若还停在旧槽端口，导入后会挪到服务端分配的端口，出口住宅 IP 换成服务端为该用户分配的那个**（4.1 之前旧槽端口的副本能连，但从另一个住宅 IP 出去，F5）；过期链接不覆盖活动节点与面板节点（除备注与跳跃段外参数不全等就不覆盖），被挡下时另起一条或只更新另一条副本，并说明出路；来源命中即升级；存量重复在菜单里确认合并；token 名导入一次自动改名、**按旧 token 名写的脚本会失效**；人读输出打码、`--json` 原名；kind 门槛（备注改过的链接不跨端口）；host 不区分大小写。HANDOVER 另写兼容段下线前的操作（§12.10）。

## 11. 测试计划

门禁：`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`。样例一律 `panel.example.com`、`tizi.example.test`、`alice`、合成 token（`0123456789abcdef` 连写两遍，32 位十六进制；文档里不写出整串，免得被公开仓库的 token 自检误报）。门槛相关用例自建 `Source::V3` / `Source::Paste`、备注不含「直连」「住宅」的 profile，不引用 `baiyi_like`（§1.3）。编号沿用 r1，本版新增的用 `a` / `b` 后缀插在相关条目之后。

**通则：经 URI 进来的用例，备注必须与被比条目的 kind 同向**（核查后第三轮补）。`node_uri` 只看备注含不含「住宅」：含则 `Hy2Residential`，否则一律 `Hy2Direct`（`node_uri.rs:25`、`58-62`，F2）；「直连」二字不影响 kind，只影响 `kind_trusted`。而账号 key 里有 kind（§4）：给一条 `Hy2Direct` 条目配一条备注写「住宅」的粘贴，两者**根本不是同一账号**，`blocked_same_account` 返回 `None`、走 wanted 起名，用例既不报 `PanelEntry` 也不报 `ActiveEntry`，会**静默空过**（断言的字符串压根不出现，测试照样绿）。所以：直连条目的粘贴备注不能含「住宅」，住宅条目的粘贴备注必须含「住宅」；凡断言 `Blocked` 变体或说明句的用例，都同时断言 `blocked_same_account` 返回 `Some`，把空过挡住。

由此还有一条推论，写用例时别踩：**住宅条目经粘贴到不了 `kind_unsure` 为真**——`gate_ok` 要两边都 `kind_trusted`，而两边在住宅这一侧都是恒真的：粘贴要与住宅条目同账号就得含「住宅」、含「住宅」即可信；条目自己也一样，面板来源恒可信，URI 解析出的住宅 kind 必然来自含「住宅」的备注（§5.2）。所以「受保护且门槛外」的追加版只能用直连条目来测（§5.2 已写明：门槛只可能拦下备注里没有「直连」的 `Direct` kind）；反过来说，门槛用例的默认夹具（备注不含直连 / 住宅的 V3 / Paste 条目）只能配 `Direct` kind，**不要拿 `hy2_resi_node()` 改个 label 造出一条「备注不含住宅的住宅条目」**——现实中造不出来，拿它测出来的追加版是假的。`profiles.rs` 的纯函数用例不受本通则约束：那里的 `Node` 由 `testutil` 直接构造、kind 与 label 独立，同向体现为「构造的 `Node.kind` 与条目相同」。

### 11.1 `profiles.rs`（纯函数与结构）

1. `kind_trusted_only_for_panel_nodes_or_labels_naming_direct_or_residential`
2. `account_group_crosses_ports_only_when_both_kinds_are_trusted`：来件可信 × 列表可信 / 猜的，端口同 / 不同，四格。
3. `account_group_never_crosses_kind_host_or_subject`：Reality 直连与住宅共用 uuid、HY2 直连与住宅共用 username、另一台主机上的 alice、同主机家人账号。
4. `same_account_and_same_endpoint_ignore_host_case_like_the_tombstone_key`（D6）：`Panel.Example.com` 与 `panel.example.com` 同账号、同连接，`tombstone_key` 相同；端口或 kind 不同照旧不同。
5. `protected_entries_need_the_same_endpoint_for_non_panel_sources`（D1）：来件 {ApiNodes, Subscription, Paste, V3} × 条目来源 {ApiNodes, Subscription, Paste, V3} × {active, 非 active} × {同一连接, 否}，按 §5.3 表逐格断言 `movable`。
5a. `blocked_same_account_reports_panel_entries_before_active_ones_before_kind_unsure`：三种原因各一格，其中 `KindUnsure` 那一格取**不受保护**的条目（非活动、非面板来源）；受保护、门槛内、不同参数 → `PanelEntry { kind_unsure: false }` / `ActiveEntry { kind_unsure: false }` 各一格；**同一条既受保护（`ApiNodes` 条目、或非面板来源的活动节点）又在门槛外 → `PanelEntry { kind_unsure: true }` / `ActiveEntry { kind_unsure: true }`**（两格：`ApiNodes` 条目遇猜 kind 的跨端口粘贴 → `PanelEntry`；猜 kind 的活动 V3 条目遇备注可信的跨端口粘贴 → `ActiveEntry`，**粘贴的 `Node` 与条目同 kind**——5a 是 `profiles.rs` 的纯函数用例，`Node` 由 `testutil` 直接构造、kind 与 label 独立，§11 开头的通则在这里体现为「构造的 kind 相同」，label 只决定 `kind_trusted`：条目 label 不含直连 / 住宅（门槛外），来件 label 含「直连」（可信）；A1，钉住「不再退化成 `KindUnsure`」）；门槛内、受保护、同参数 → 在组里，不报；多条同时命中时按 PanelEntry > ActiveEntry > KindUnsure 报，取舍不受 `kind_unsure` 影响（加一格：`ApiNodes` 条目与猜 kind 的活动 V3 条目同时被同一条粘贴挡下 → 只报 `PanelEntry`，§9 表后说明、R18）。
5b. `same_params_ignores_only_label_and_hop`（C2）：label、hop、host 大小写不同 → 真；端口、HY2 密码、obfs_password（`None` 对 `Some`）、sni、Reality public_key / short_id / server_name / flow 任一不同 → 假；并断言 `same_params` 为真时 `same_endpoint` 必为真。
6. `pick_keeper_prefers_active_then_preexisting_then_same_endpoint_then_wanted_then_list_order`：五组，各测一级；含「本批同端口新条目不压过导入前的老名字」。
7. `find_account_before_ignores_profiles_added_in_this_run`，并断言它不看 `protected`。
8. `name_has_token_matches_4_0_0_token_names_only`：`<token>-hy2-resi`、`<token>-hy2-resi-2`、大写 hex 为真；31 位 hex、`alice-hy2-resi`、`hysteria2-1785892136`、`panel.example.com-hy2-direct` 为假。
9. `rename_moves_active_and_refuses_a_taken_name`
10. `heal_token_names_renames_to_host_kind_and_moves_active`，`Healed.renamed` 带 (旧名, 新名)。
11. `heal_token_names_suffixes_when_host_kind_is_taken`
12. `heal_token_names_rewrites_tombstone_display_names_from_the_key`：key 与 at 不变。
13. `heal_token_names_is_a_noop_without_token_names`：前后 `==`，保存逐字节相同。
14. `bury_never_records_a_token_name`
14a. `burying_the_same_account_twice_keeps_unknown_tombstone_fields`（B1）：文件里同 key 墓碑带未知键 → 再 `bury` 同一账号 → 只剩一条该 key 的墓碑，名字与 `at` 是这次的，未知键原样保留，且它排在 `deleted` 末尾。
15. `merge_into_keeps_the_keeper_name_and_never_buries`：`deleted` 不变；active 不在 others 里；重读后已不存在或已非同账号的条目跳过。
16. `merge_into_gives_the_canonical_name_to_a_suffixed_keeper`（D9）：留存者 `panel.example.com-hy2-resi-2`（active）并掉 `panel.example.com-hy2-resi` → 留存者改叫规范名、active 跟着改；留存者叫 `alice-hy2-resi-2` 时不改。
17. `upsert_replace_keeps_unknown_profile_fields`
17a. `raise_source_only_goes_up_and_keeps_ties`：V3 → Subscription → ApiNodes 逐级升，返回 true；ApiNodes 遇 Subscription、Subscription 遇 Paste 不动，返回 false；Paste 与 V3 互遇不动；名字不存在返回 false。另断言 `best_source` 同一张表。
18. `unknown_fields_survive_a_round_trip_on_profiles_profile_and_tombstone`（C3）
19. `a_4_0_0_shaped_rewrite_still_matches_the_same_node`（C2，§6.5）：`Profiles400` 八个字段、不含 `deleted`；第 2 步后断言 JSON 里没有 `deleted` 与未知键；第 3 步断言 `deleted` 为空且仍命中同一条。
20. 回归不改：`same_endpoint_ignores_label_hop_and_sni_but_not_credentials`（`profiles.rs:471`，host 大小写那一条按 D6 补断言）、`the_tombstone_key_is_account_level_and_holds_no_credentials`（`670`）、`an_old_v1_reader_still_parses_a_file_with_tombstones`（`843`）。
21. 回归不改、加 catch-all 后必须仍逐字节通过：`a_file_without_deleted_loads_and_saving_without_tombstones_is_byte_identical`（`profiles.rs:781`）。

### 11.2 `cli.rs`：导入编排

22. `port_move_replaces_in_place_under_the_existing_name`：面板 `alice`，`hy2_resi_node`（`testutil.rs:39-47`）40003 → 40000、hop 切片 → 整段。名字不变、端口与 hop 是新的、transcript 里有一行 `trim()` 后等于 `更新节点 alice-hy2-resi：端口 40003 → 40000`、结果行 `导入 0 个新节点`、没有 `-2`。
23. `port_move_keeps_a_v3_dir_name`：照 `reimporting_a_v3_node_from_the_panel_updates_it_in_place`（`cli.rs:3724`）的骨架加上端口变化，名字仍是 `hysteria2-1785892136`，`source` 变成 `ApiNodes`。
24. `port_move_keeps_the_suffix_on_the_second_server`：照 `the_same_ascii_username_on_two_servers_keeps_both_nodes`（`cli.rs:3696`）的骨架，第二台的 `-2` 换端口仍是 `-2`、不出现 `-3`。
25. `port_move_keeps_a_host_kind_name_after_a_panel_import`：先粘贴得到 `panel.example.com-hy2-resi`（40003，非 active），再从面板导入 `alice`（40000）→ 只剩一条，名字不变。
26. `a_token_named_profile_is_renamed_even_when_the_endpoint_is_unchanged`（C6）
27. `token_named_profiles_are_renamed_on_import_and_the_full_token_is_never_printed`：存量 `<token>-hy2-resi` 为 active、端口变 → 名字 `panel.example.com-hy2-resi`、active 跟着改；transcript 有 `节点 0123…-hy2-resi 已改名为 panel.example.com-hy2-resi`、不含完整 token；配置因端口变 apply 一次；改名、active、墓碑显示名一次写盘（写 `profiles.json` 恰好一次）。
28. `renaming_a_token_active_profile_alone_does_not_restart`
29. `an_unrelated_import_still_renames_leftover_token_names`：粘贴另一台服务器的一条链接，token 名照样改掉。
30. `a_token_in_a_tombstone_name_is_masked_on_import`：命令行「跳过…」与菜单 `buried_head` 不含完整 token。
31. `import_v3_never_prints_a_full_token_from_a_tombstone_name`（D3）：rc 删过的 token 名墓碑 + v3 目录里同一账号 → `bui-c import-v3` 与菜单 `[7]→[3]` 的输出只有 `0123…`。夹具**不含 v3 单元**，`run` 在 `import_v3.rs:312-313` 提前返回，断言 `profiles.json` 不被写；另加一格带残留单元的夹具（`run` 会在 `import_v3.rs:350` 原样写一次），那一格断言 `profiles.json` 内容逐字节不变（B2）。**这一格的夹具有硬前提（M1、I3）**：光有残留单元不够，`import_v3.rs:311-313` 的 `nothing_to_apply`（`r.existing.is_empty() && prof.active_profile().is_none()`）是第二道早退闸，而墓碑命中的 v3 目录走的是 `r.buried`（`import_v3.rs:159-164`）、进不了 `r.existing`，所以夹具必须在调用 `run` **之前**就让 `prof.active` 指向列表里一条真实存在的条目；`import_v3.rs:341` 之前还有一次 `active_profile().ok_or_else`，没有活动节点会直接报错返回，同样到不了 350。三条硬前提要一起满足：
    - **active 必须落在盘上的 `profiles.json` 夹具里**，不能照 `import_reports_existing_nodes_without_duplicating`（`import_v3.rs:878`）那样只在内存里 `prof.active = Some(...)`。CLI 与菜单两条路径都从盘上加载 `Profiles`，逐字节比对的基准也是那个文件；只在内存里设 active，`import_v3.rs:350` 的 `save` 会把它原样写进去，盘上文件反而多出 `active`，「逐字节不变」必然失败。
    - **这条活动节点要与被墓碑挡下的那个账号是不同账号**（例如另一台主机，或同主机的另一个用户名）。同账号的话，`import` 在 `import_v3.rs:149` 的 `find_same_endpoint`（以及本版新增的 `find_account_before`）先命中、进 `r.existing` 并 `continue`，走不到 `159-164` 的墓碑分支，墓碑名根本不会被打出来，这一格的主断言（输出里只有打码后的 token）就失去了对象。
    - **备好内核与引擎桩**：走到 350 之前还要过 `ensure_kernel`（`import_v3.rs:330`）与 `render` / `verify`（`342-347`）。照 `run_imports_then_tears_down_and_persists`（`import_v3.rs:620`）准备——放一个 `/opt/bui-c/bin/sing-box`（`ensure_kernel` 因此是空操作，`kernel_installed` 为假），`sing-box check` 不另外 `reply` 就默认成功（对照 `run_aborts_when_sing_box_check_rejects_the_config`，`import_v3.rs:671`，要显式登记退出码 1 才失败）。
    - **夹具里的每个 v3 目录都要落进 `buried` 或 `existing`**，一个都不能被当成新节点。`v3_machine()`（`import_v3.rs:423-443`）有 hysteria2 与 vless 两个目录，`import` 对每个目录独立判 `find_same_endpoint` / 墓碑 / 新建（`149-190`）：只要有一个走了新建，`r.imported` 就非空，`import_v3.rs:350` 写出的不再是原样，逐字节断言与「`run` 原样写一次」的前提一起失效。做法：要么夹具只留命中墓碑的那一个目录，要么把另一个目录的账号正好做成盘上那条活动节点（进 `existing`，顺带让 `nothing_to_apply` 为假）。断言 `r.imported` 为空。
    - **盘上夹具要由 `Profiles::save` 自己写出来**，逐字节比对才有意义。`save` 是 `serde_json::to_vec_pretty` 加一个末尾换行（`profiles.rs:263-270`），手写的 JSON 只要键序、缩进、末尾换行有一处不同，`import_v3.rs:350` 原样重写后字节就变了，断言会因为格式而不是因为内容失败。做法：在内存里构造 `Profiles`（含 token 名墓碑、另一账号的 active 条目），`save` 一次，把写出的字节读回来作基准，再调 `run`——与 `profiles.rs` 里那条逐字节用例（`a_file_without_deleted_loads_and_saving_without_tombstones_is_byte_identical`，`profiles.rs:780`）同一套做法。
    这一格再断言 `Report::removed_units` 非空（残留单元确实被 `teardown` 删了，`import_v3.rs:227-238`、`351-353`），以此证明真的走过 `import_v3.rs:350` 的 `save`，逐字节不变的断言不是因为文件压根没被重写而空过。
32. `json_outputs_keep_the_raw_token_name`（D3）：从没导入过的 token 名机器，`list --json`、`status --json` 输出原名。
33. `human_outputs_mask_token_names`：同一台机器的 `list` 表格、`status`、删除确认块、删除 `cli_summary`、「上次：」行只有 `0123…`。
34. `a_stale_password_paste_never_replaces_an_active_v3_node`（D1）：V3 来源活动节点 :10000；粘贴同 username 旧密码，三格（②/②′ 与测试 35 对称；`gate_ok` 要求**两边**都 `kind_trusted`，所以要写明哪一边的备注可信）：① 同端口（端口相同即过门槛，两边备注按 §11 开头的默认夹具都不含直连 / 住宅也无妨）→ `kind_unsure` 为假；② 跨端口，**V3 条目的备注含「直连」、粘贴的备注也含「直连」**（两边都可信）→ `kind_unsure` 为假；两格的说明句都是基础版 `当前节点不变；确认新节点能用后可以切换过去`。②′ 跨端口，V3 条目备注按默认夹具不含直连 / 住宅（门槛外）→ `kind_unsure` 为真，说明句在「…不变」之后追加「同时认不准是直连还是住宅」。三格都断言：active 不变、节点数据不变、报的是 `ActiveEntry`、引擎不 apply。
35. `a_stale_password_paste_never_replaces_a_panel_node`（D1）：同上，条目为非活动的 `ApiNodes`。两格（A1 之后不再需要靠可信备注把条目留在门槛内，条目来源是 `ApiNodes` 就足以报 `PanelEntry`）：② 跨端口、粘贴备注含「直连」（**与条目 kind 同向**：34 与 35 的条目都是 :10000 的直连，备注写「住宅」会被解析成 `Hy2Residential`、不再是同一账号，用例静默空过，见 §11 开头的通则）→ `kind_unsure` 为假，说明句不追加；②′ 跨端口、粘贴备注不可信（门槛外）→ `kind_unsure` 为真，说明句在「…不变」之后追加「同时认不准是直连还是住宅」。两格报的都是 `PanelEntry`、都有「要更新它请从面板重新导入」那一句。
36. `a_subscription_refresh_still_replaces_an_active_subscription_node_across_ports`（D1 例外）
36a. `a_no_op_subscription_refresh_raises_a_v3_source_so_the_next_port_move_is_in_place`（r1 核查）：V3 来源活动节点；第一次订阅导入与它参数完全相同 → 打「无变化」、`source` 变成 `Subscription`、`profiles.json` 写一次、引擎不 apply；第二次订阅导入端口变了 → 原地替换、active 不变、没有 `protected_new` 的任何一句、apply 新端口。
36b. `a_no_op_panel_refresh_raises_the_source_and_then_blocks_a_stale_paste`（r1 核查）：非活动 V3 条目（**直连口 :10000，备注含「直连」**；用直连是因为住宅条目经粘贴到不了 `kind_unsure` 为真，见 §11 开头通则的推论）被同参数面板导入命中（split 恰好也相同，`Unchanged`）→ `source` 变成 `ApiNodes`；随后粘贴旧密码、跨端口，**粘贴备注可信与否都报 `PanelEntry`**（条目已被 `raise_source` 升成 `ApiNodes`、受保护，A1 下与门槛内外无关）：粘贴备注含「直连」→ `kind_unsure` 为假、说明句不追加；粘贴备注不含「直连」「住宅」（仍解析成 `Hy2Direct`，同账号，但 `kind_trusted` 为假）→ `kind_unsure` 为真、说明句追加「同时认不准是直连还是住宅」；两种备注各一格（与 35 的 ②/②′ 同构）→ 条目不变、另起一条、打「要更新它请从面板重新导入」。
37. `a_subscription_refresh_never_replaces_an_active_v3_node_across_ports`（D1）：从未被订阅命中过的 V3 活动节点 → 另起一条，说明句是「切换过去」那一句，不含「订阅」二字。
38. `a_panel_import_replaces_protected_members`（D1）：面板来件替换活动的 V3 条目与 ApiNodes 条目。
38a. `a_stale_paste_without_obfs_never_replaces_the_active_node`（C2）：活动节点为 ApiNodes 来源、**住宅口 :40000**、带 `obfs_password`（面板开了 obfs 后导入的）；粘贴开混淆前的旧链接（同端口、同密码、无 obfs，**备注含「住宅」以保证同账号**——同端口本就过门槛，备注可信与否不参与 `gate_ok`，这里只要求 kind 同向，§11 开头的通则）→ active 与节点数据不变（`obfs_password` 仍在）、另起一条、打「要更新它请从面板重新导入」、引擎不 apply。再加一格活动节点为 V3 来源 → 说明句是「切换过去」那一句。
38b. `a_subscription_refresh_explains_a_blocked_active_node_when_another_copy_moves`（A2）：`hysteria2-1785892136`（V3，备注 `alice-HY2住宅`，40003，active，从未被订阅命中）+ `panel.example.com-hy2-resi`（Subscription，40003，非 active）；订阅导入 40000 → 组 = [后者]，它被换到 40000；transcript 有一行 `trim()` 后等于 `当前节点 hysteria2-1785892136 与 panel.example.com-hy2-resi 同一账号但连接参数不同，当前节点不变；确认 panel.example.com-hy2-resi 能用后可以切换过去`；active 与活动节点数据不变、引擎不 apply；`Stored.switch_to == ["panel.example.com-hy2-resi"]`。菜单版：随后问 `切换到 panel.example.com-hy2-resi？`，答 y 切过去。
38c. `a_panel_import_with_a_blocked_panel_copy_is_explained`（A2）：组非空、另有一条住宅口的 ApiNodes 条目被过期粘贴挡下，**粘贴的备注含「住宅」（可信，与该条目 kind 同向，§11 开头的通则）**——被挡那条是 `ApiNodes`、kind 恒可信，两边都可信所以 `gate_ok` 过、`kind_unsure` 为假（不写明就会落进默认夹具、报成追加版，断言不命中）→ 打基础版「{旧名} 与 {keep} 同一账号但连接参数不同，{旧名} 不变；要更新它请从面板重新导入」，不进 `switch_to`。再补两格，钉住这句出路的两种落法都真的把账号更新到了面板参数（§9 表后说明、R18）。两格**各自另起夹具，不接着上面那一步**：上一步里被粘贴原地替换的那条组成员，备注已换成粘贴的「住宅」、kind 可信，面板来件对它 `gate_ok` 且 `protected` 恒假，所以它必然也进组，「组里只有被挡过那条」在延续的夹具上不成立。
  - (a) 该账号在列表里只有被挡过的那条 `ApiNodes` 条目 P（同账号没有别的条目）→ 从面板导入该账号的新参数，组 = [P]，P 就是 `pick_keeper` 的留存者，被原地替换成面板参数。
  - (b) 该账号里除 P 之外还有一条自身 kind 可信的活动节点 A（备注与 P 同向）→ 两条都进组，留存者按 `pick_keeper` 第 1 级是 A、被原地替换，P 落进 `stale` 转成存量重复（`Stored.dups` 里有它、条目仍在、`deleted` 为空），菜单答 y 才并掉。**A 与 P 都要停在旧参数**（例如都在 :40003、面板推 :40000）：A 若已经是面板新参数，`upsert` 返回 `Unchanged`（`profiles.rs:313-315`）、只升来源，断言的端口变化行与 `Replaced` 都不成立。
39. `cli_import_reports_duplicates_without_merging`（D2）：`alice-hy2-resi`（40003，active）+ `alice-hy2-resi-2`（40007），面板推 40009 → active 那条 40009、`-2` 原样、有提示两行、`deleted` 为空、apply 新端口。
40. `duplicates_of_accounts_outside_the_batch_are_left_alone`：不替换、不提示。
41. `a_batch_written_copy_is_merged_on_the_spot_and_not_counted_as_new`：列表 `hysteria2-1785892136`（V3，备注 `alice-HY2直连`，:10000，非 active；另有 active 节点）；一次粘贴两行 `…@panel.example.com:10005#custom`、`…@panel.example.com:10005#alice-HY2直连` → 留下的是 `hysteria2-1785892136`（端口 10005），`panel.example.com-hy2-direct` 不在列表，结果行 `导入 0 个新节点`，菜单不问切换，没有存量重复提示。
42. `a_stale_active_with_an_up_to_date_duplicate_moves_the_active`：active 是旧端口 40003，`-2` 已是新端口 40000，面板推 40000 → 留存者是 active、被替换成 40000、`config.json` 里是 40000、`-2` 作为存量重复提示。
43. `an_unchanged_active_with_a_stale_duplicate_does_not_restart`：active 已是新端口，旧端口那条非 active → `Unchanged` + 提示，引擎不重启。
44. `a_guessed_kind_paste_is_explained_as_a_protected_panel_entry`（A1 改判，原名 `a_guessed_kind_paste_never_moves_across_ports`；断言随之反转）：已有 `alice-hy2-direct:10000`，**来源 `ApiNodes`、是活动节点**（既受保护又落在门槛外），粘贴 `hysteria2://alice:pw@panel.example.com:40003#custom` → 直连不动、active 不变、新增一条；有 `protected_new` 的 `PanelEntry` 那一句、且句中「…不变」之后补了「同时认不准是直连还是住宅」，**没有** `kind_unsure_new` 那一行（A1：保护优先，门槛次之，两条都报）。「不跨端口替换」这条不变量仍由「直连不动、active 不变」钉住。
45. `a_guessed_kind_active_profile_is_not_swallowed_by_a_panel_direct_node`：V3 profile 备注 `alice-HY2`、40003、active；面板发 `HY2直连 :10000` → V3 profile 与 active 原样不动。
46. `a_guessed_kind_profile_on_the_same_port_is_updated_in_place`：**自建** `Source::V3`、备注 `示例备注`（不含直连 / 住宅）的 `hysteria2-1778329470`（:10000）遇上面板 `HY2直连 :10000` → 原地替换、名字保留。
47. `a_blocked_same_account_entry_with_another_name_is_explained`（D9 ③）：门槛外的同账号条目名字 ≠ wanted 时也打说明行。该条目取**非活动、非 `ApiNodes`**（即不受保护），报的是 `KindUnsure`；「受保护 + 名字不同」的组合在 5a 与 44 里覆盖，不与本条混用。
48. `the_same_account_twice_in_a_trusted_batch_keeps_the_later_one`
49. `the_same_account_twice_in_a_guessed_paste_keeps_both`
50. `direct_and_residential_sharing_credentials_never_merge_on_port_move`：Reality 与 HY2 各一组。
51. `family_accounts_on_one_host_never_merge_on_port_move`：`hy2_account_node("bob")`（`testutil.rs:25-37`）。
52. `a_host_case_difference_is_the_same_account_on_import`（D6）：列表 host `Panel.Example.com`，面板发 `panel.example.com`、端口变 → 原地替换，没有 `-2`。
53. `a_deleted_account_stays_deleted_after_its_port_moves`：命令行打 `跳过 1 个删过的节点：alice-hy2-resi（要加回用 --with-deleted）`；菜单问一句。
54. `deleting_the_new_port_copy_then_reimporting_moves_the_old_copy`（D9，§7 有意的新行为）：`alice-hy2-resi`（40003）+ `alice-hy2-resi-2`（40000），删掉 `-2`（记墓碑），面板推 40000 → `alice-hy2-resi` 被换到 40000、墓碑清掉、不提问、没有「跳过」。
55. `with_deleted_restores_a_deleted_account_at_its_new_port`
56. `a_rotated_reality_uuid_still_gets_a_suffix`：钉住范围边界。
57. `a_subscription_refresh_keeps_the_panel_split_when_the_panel_copy_is_merged_away`（D7）：`panel.example.com-hy2-resi`（Subscription，默认分流，active，40003）+ `alice-hy2-resi`（ApiNodes，关键字分流，40000）；订阅退回路径导入 40000 → 留存者是 active、`split` 是关键字、`source` 是 `ApiNodes`；菜单答 y 合并掉 `alice-hy2-resi` 后仍是关键字分流。
58. `a_paste_refresh_keeps_the_panel_split_and_source` / `a_panel_refresh_upgrades_a_pasted_profile`
59. 既有用例，断言不改、预期原样通过：
    - `a_live_profile_sharing_the_key_is_refreshed_not_reported_as_deleted`（`cli.rs:10136`）：删掉旧槽后只剩 `-2`（与来件同端口）→ ① 刷新并清墓碑，打的仍是 `更新节点 alice-hy2-resi-2`。只改文档注释：setup 描述的是 4.0.2 之前留在盘上的存量 `-2`。
    - `password_rotation_for_the_same_account_replaces_in_place`（`cli.rs:3752`）、`updating_the_active_node_in_place_applies_it`（`3768`）——两条都是面板来件，不受 D1 影响；`two_accounts_in_one_pasted_batch_under_the_same_name_are_both_kept`（`3934`）、`importing_a_second_account_on_the_same_host_does_not_overwrite_the_first`（`3659`）、`importing_a_token_subscription_never_puts_the_token_in_a_profile_name`（`7715`）、`cli_import_still_lists_skipped_nodes_when_apply_fails`（`10249`）。
    - 实现时 `git grep` 测试区里 `"更新节点 ` 的整行断言（端口变化的那些改成 `trim()` 比对）、`-2"` 的名字断言、「粘贴覆盖面板条目后 split 回到默认值」的断言、断言 `source` 在 `Unchanged` 后仍是 V3 / Paste 的断言，以及**用粘贴或订阅更新活动节点**的用例，按 D1、§5.5 与新行为逐条核对。

### 11.3 `cli.rs`：菜单

60. `menu_import_port_move_does_not_offer_to_switch`
61. `menu_import_token_rename_does_not_offer_to_switch`：混入一条真正新增的仍然会问，且问的是那一条（显示名）。
62. `menu_import_second_pass_new_nodes_are_still_offered`：墓碑答 y 加回的节点仍会追问。
63. `menu_import_duplicates_answer_no_keeps_both`（D2）：默认 N / EOF → 两条都在，`profiles.json` 在提问之后没有再被写。
64. `menu_import_duplicates_answer_yes_merges_and_keeps_the_keeper_name`（D2）：答 y → 只剩 `pick_keeper` 选中的那条、名字不变、`deleted` 为空、引擎不 apply。
65. `menu_import_merge_gives_a_token_keeper_the_canonical_name`（D9）：token 名 active（40003）+ `panel.example.com-hy2-resi`（ApiNodes，40000）；面板推 40000 → 改名为 `-2`、被替换到 40000；答 y → 只剩 `panel.example.com-hy2-resi`、active 是它、有改名行。
66. `menu_import_extra_lines_pause_before_returning`：端口变化行（经 `tell`）单独出现时也算附加行、菜单停一次。
67. 回归：`menu_import_pasted_uris_offer_to_switch_to_the_first_new_node`（`cli.rs:7830`）。

### 11.4 `import_v3.rs`

68. `rerun_after_a_panel_port_move_skips_the_account`：列表里 `hysteria2-<ts>` 已是 40000，v3 目录是 40003 → `existing`，端口不回写，没有 `-2`。
69. `rerun_after_password_rotation_never_writes_back_the_old_password`
70. `rerun_maps_the_v3_active_dir_to_the_live_profile_of_that_account`
71. `first_run_imports_both_ports_of_one_account_from_v3_dirs`
72. `a_guessed_kind_v3_dir_on_another_port_is_imported_as_new`
73. `a_v3_dir_whose_host_differs_only_in_case_is_existing`（D6）
74. 回归不改：`dir_names_that_sanitize_to_nothing_fall_back_to_profile_name`（`import_v3.rs:856`）、`import_reports_existing_nodes_without_duplicating`（`870`）、`rerun_recognises_nodes_whose_label_was_updated_by_the_panel`（`908`）。

### 11.5 `menu.rs` / `delete.rs`

75. `every_line_fits_by_budget`（`menu.rs:3429`）加入 `protected_new` **四种**说法、`protected_kept` **四种**说法（`PanelEntry` / `ActiveEntry` 各配 `kind_unsure` 真 / 假两个长度；带「同时认不准是直连还是住宅」那半句的是最长的样本，最可能撞上限，必须进表，A1）、`切换到 {keep}？`、`kind_unsure_new`、`dups_head`、`要合并吗？`、合并结果行、改名行、**端口变化行**（全部按 `tell` 的折法，即 `delete::page`），以及打码后的 token 名节点列表与确认块；样本名字用 `baiyi_like` 里最长的 38 列名字；宽度与现有表一致：`{40, 50, 59, 60, 80, 100}`（`menu.rs:3432`）。
76. `display_name_masks_each_token_segment_to_four_chars_and_sanitizes_first`：`…` 已在宽度表的字符归类里。
77. `dups_head_caps_the_list_and_masks_names`：个数上限同 `BURIED_LIST_MAX`（`menu.rs:774`），名字经 `display_name`，不截断、折行交给 `tell`。
78. `plan_errors_and_cli_summary_mask_token_names`（`delete.rs:44-64`、`214-222`）。

### 11.6 真机（baiyi）

79. 打 4.0.2 的 tag 之前（§12.7 第 3 步，本地构建）与打 `v4.0.2-rc1` 之后（第 4 步，手工装的发布件）各跑一次：跑一次 `bui-c import`，核对端口变化行、存量重复提示、token 改名行符合预期；`bui-c list --json` 条数不增；`status --json` 的 `active` 不变或只因改名变；菜单 `[3]` 答 N 与答 y 各走一遍（答 y 前先 `list --json` 留底）。
80. 服务端 4.1 之后：从面板再导入一次，住宅 HY2 的 `port` 从 `4000x` 变 `40000`，名字不变，没有新增条目；**`bui-c status --json` 的 `active` 指向的那一条在 `list --json` 里 `port` 是 40000**（活动节点确实挪到了新端口，不是另起一条留在旧端口）；`/opt/bui-c/bin/sing-box check -c /opt/bui-c/config.json` 通过（本机 1.14.1）。

## 12. 上线时序与兜底

### 12.1 客户端从哪取 manifest

- 客户端先取它跟随的面板 `/packages/manifest.json`，再取 GitHub `releases/latest`（`update.rs:155-165`）；latest 404 时回退到 releases 列表里版本号最大的预发布（`update.rs:230-266`，列表取最大在 `208-222`）。
- 只有 https 的 v4 面板会被记成跟随对象（`cli.rs:595-616`）；订阅、粘贴、v3 面板、http 面板不记 panel（`cli.rs:601-610`、`618-626`）。**每次从另一台 https v4 面板导入都会换跟随对象**（`cli.rs:563-573`「自动更新来源改为 …」）。
- 二进制来源顺序：面板 `/packages/<文件>` → manifest 给的 GitHub 地址 → 镜像（`update.rs:121-137`），逐个下载并校验 sha256（`update.rs:268-289`）：面板上的文件还是旧的，sha 不符就换下一个源。
- 每日自更新：23 小时 + 机器码抖动（`JITTER_SPAN_S = 7200`，最多约 2 小时）+ 失败 1 小时退避（`check.rs:23-26`、`309-341`）；`auto_update` 关着不自更新（`check.rs:314-316`），下载完成后关掉也不装（`cli.rs:1322-1326`）。

### 12.2 面板发的是哪一版：由 tag 决定

- 面板 `/packages/manifest.json` 直接读 `<base>/manifest.json`（`crates/bui/src/modules/panel/packages.rs:241-256`、`295-306`）。这份缓存由 install、`bui upgrade`、每日自检写入（`crates/bui/src/serve.rs:387`）。
- 每日自检（`serve.rs:286-385`）：启动后按节点 id 抖动 ≤ 1 小时（`serve.rs:294`、`396-404`），之后每 24 小时一轮（`serve.rs:35`、`383`），调 `pick_selfcheck_manifest`（`crates/bui/src/kernels/mod.rs:446-491`）：
  - 候选一：latest（404 时无条件回退到最新 rc，`kernels/mod.rs:403-429`）；
  - 候选二：**缓存是 rc**，或运行中的 bui 比 latest 新时，再取最新 rc tag（`kernels/mod.rs:455-457`，`latest_rc_tag` 按 (x, y, z, rcN) 取最大，`290-307`）；
  - 两份取 rank 大的，**严格高于**本机当前 rank 才写缓存并请求对账（`kernels/mod.rs:482-484`；rank 定义 `309-345`，稳定版的 rc 位取 `u32::MAX`；写缓存与 `ReconcileRequested` 在 `serve.rs:315-367`）。
- 客户端二进制缓存（`packages.rs:151-184`）有自己的 24 小时 `tokio::time::interval`，**第一次 tick 立即执行**（`packages.rs:153-156`），随面板启动（`crates/bui/src/modules/panel/mod.rs:291`）。

推论：

1. 跟随某台面板的客户端最终拿到的 bui-c，是那台面板的自检或升级选中的 **tag**。服务端升级不是客户端拿到新 bui-c 的前提；打 tag 才是。
2. 一台缓存是 rc 的面板会自动跟上之后打的**任何**版本号更大的 rc tag；一台缓存是稳定版的面板会在正式 tag 成为 latest 后的下一轮自检（≤ 24 小时）选中它。**两台服务器谁先手动升级，管的只是服务端二进制，管不住客户端的铺开**。
3. ≥ rc2 的面板绝不写入更低 rank（`e72eac1`）：一个版本经面板发出去之后，**不能靠面板回退**，客户端出问题只能打更高的版本号向前修复。

### 12.3 替换判据与降级口径（C8）

- 客户端自身替换判据：**版本号或 sha256 任一不同就换**（`update.rs:359-377`：版本不同 → `NewVersion`；版本相同、sha256 不同 → `Rebuild`）。不做 semver 排序，降级也会换（`update.rs:414-417`）。所以同一 x.y.z 下不同 rc 的构建，照样按 sha256 被换来换去。
- 服务端 4.0.1-rc2 起，自检绝不写入更低 rank 的 manifest（`e72eac1`），**跟随已升到 ≥ rc2 面板的客户端不再被面板降级**。
- **没记面板**，或**跟随未升级面板**的客户端，仍会拿到 GitHub latest——4.0.1 正式版之前那是 4.0.0。
- 客户端改成只升不降是另外的 4.0.1 backlog 项，不在本设计。本文不写「rc 降级不再发生」。

### 12.4 tag 门禁：以服务端 4.1 plan T20 为准

- **门禁是什么**（服务端 4.1 plan「Task 20: 发布 tag 门禁」；origin/v4 `5b22bdf` 上尚未实现，`git ls-tree -r --name-only origin/v4 -- scripts/release/` 里还没有下面两个文件）：`scripts/release/check-release-gate.sh <tag> [--warn]` 读清单 `scripts/release/required-commits.env`（`KEY=<40 位 sha>`，`#` 注释）。键是受管 tag 前缀、点换下划线（`v4.1` → `GATE_v4_1`），值是必须已是 HEAD 祖先的提交（`git merge-base --is-ancestor <sha> HEAD`）。清单现为 `GATE_v4_1=pending`：pending 或空 → 退 3（fail-closed）；提交在本仓库找不到 → 退 3（文案提示 `fetch-depth: 0`）；不是祖先 → 退 3；tag 不在清单范围（例如 `v4.0.2`）→ 退 0。
- **本线交付**：「不自动合并同账号重复节点，只提示或经菜单确认」（§5.8；服务端 4.1 spec §7.6 事实 3 的共同结论）的实现提交在 v4 分支落地后，把它的完整 SHA 交给服务端填进 `GATE_v4_1`。本改动分多个提交时交最后一个实现提交（或合并提交）：祖先关系覆盖它之前的全部提交。之后若 cherry-pick 或 rebase 到别的分支，SHA 变了会关着失败，要换成新 SHA，这是想要的。
- **判据为什么是祖先关系**：「断言某个测试名存在」可以被改名、删测试绕过，而且证明不了行为还在；祖先关系只看提交图，分支不带本改动就过不去（与服务端 4.1 plan T20「Interfaces」同一理由）。
- **两道闸，哪一道算「拒绝打 tag」**：
  1. **真正拒绝打 tag 的是打 tag 之前在本地跑门禁**：`bash scripts/release/check-release-gate.sh v4.1.0-rc1` 退 0 才打（服务端 4.1 plan T19 的前置闸门 4）。
  2. **CI 是第二道**：tag 推上去触发 `release.yml`（`.github/workflows/release.yml:7` 对 `vX.Y.Z` 与 `vX.Y.Z-rcN` 触发）；T20 在 `verify` 作业里给 checkout 加 `fetch-depth: 0` 并加硬门禁一步。门禁不过则 `verify` 失败，`release` 作业 `needs: [verify, build]`（`release.yml:190`）不运行，`gh release create`（`release.yml:313`）不执行，**不创建 Release**。面板自检与客户端都只看 Release：自检取 `releases/latest/download/manifest.json` 与 releases 列表里的预发布（`crates/bui/src/kernels/mod.rs:55`、`79`、`292-307`），客户端同样（`update.rs:23`、`162`、`208-222`），没有 Release 的 tag 谁也选不中。但 **tag 已经推上去了**：它悬空在仓库里，要手动删掉（本地与远端）、修好后重打；`workflow_dispatch` 只允许演练、不创建 Release（`release.yml:34-39`、`298-299`），不能拿它补发。
  3. `ci.yml` 里 T20 另加一步 `--warn` 预检（按 workspace version 拼 tag），只告警不卡 CI。
- **范围**：T20 只管前缀 `v4.1` 的 tag。本线的 4.0.2 tag（含 rc）不在门禁范围（`check-release-gate.sh v4.0.2` 退 0，服务端 4.1 plan T20 Step 4 第 4 条），4.0.2 靠 §12.7 第 3-4 步的 baiyi 验收把关。把门禁扩到「版本号 ≥ V₀」只是 §14.2 T4 的可选建议。
- **不假定 GitHub latest 按版本号排序**：`release.yml:303-310` 只给 rc 加 `--prerelease`，`gh release create`（`313-317`）没有指定 latest 的参数，哪个正式 Release 成为 latest 由 GitHub 决定，本文不据此推论「低版本的正式 tag 成不了 latest」。这也是 §12.7 第 2 步要求 4.0.1 正式版先成为 latest、再打 4.0.2 的理由之一：顺序反过来时，后发布的 4.0.1 正式版可能成为 latest，把没记面板的客户端换回 4.0.1（版本号不同就换，§12.3）。

### 12.5 等待时长

从 tag 发布算起（A 行例外，见行内）：

| 路径 | 过程 | 最坏 |
|---|---|---|
| A. 服务端手动升级（**从服务端手动升级完成算起**） | `bui upgrade` 写缓存 → 守护进程重启 → 包缓存第一次 tick 立即同步二进制（`packages.rs:153-156`），`/packages/manifest.json` 立即是新的 → 客户端下一次每日自更新 | ≤ 约 26 小时（23h + <2h 抖动 + 1h 退避） |
| B. 只靠每日自检（从 tag 发布算起） | 自检下一轮选中（≤ 24h）→ manifest 立即可见，但包缓存要等自己的下一轮（≤ 24h）；客户端能直连 GitHub 时经 GitHub 地址拿到，只能连面板时要等包缓存 → 客户端 ≤ 26h | ≤ 约 74 小时（能连 GitHub 约 50 小时） |

所以 4.1 放行的等待窗口按路径 A 执行：两台服务器手动升级之后再开始计时。路径 A 只是让「所有已知客户端都拿到」这件事可预期；客户端开始换上新版本的时刻由正式 tag 决定（§12.2 推论 2）。

### 12.6 4.0.2 的预发布与正式 tag 会不会推给生产客户端

- **预发布，面板缓存还是 rc 时：会**。bwg-tizi 与 bwg-rick 现在缓存的是 `v4.0.1-rc2`，此时打 `v4.0.2-rc1`，rank `(4,0,2,1)` > `(4,0,1,2)`，两台面板一天内选中（§12.2），跟随它们、`auto_update` 开着的客户端再过 ≤ 26 小时就换上。
- **预发布，4.0.1 正式版已是 GitHub latest、两台面板缓存已换成稳定版（`(4,0,1,MAX)` > `(4,0,1,2)`）之后：不会**。缓存不是 rc、运行中的 bui 4.0.1 也不比 latest 新，`follow_rc` 为假（`kernels/mod.rs:455-457`）；GitHub latest 存在，客户端也不回退到预发布（`update.rs:241`）。
- **正式 tag `v4.0.2`：一定会，而且与服务端升级无关**。它成为 latest 后，缓存是稳定版 4.0.1 的面板下一轮自检（≤ 24 小时）就选中 `(4,0,2,MAX)`、写缓存、请求对账，包缓存随后按自己的节奏同步；没记面板的客户端直接从 GitHub latest 拿到。
- 核对方法：`curl -s https://panel.example.com/packages/manifest.json` 看 `tag`（两台都要看）。

### 12.7 执行顺序

1. 本改动在 v4 分支实现、合入；workspace version 4.0.2、`CHANGELOG.md` 新开 4.0.2 段。合入后把「不自动合并同账号重复节点」的实现提交 SHA 交服务端，填进 `scripts/release/required-commits.env` 的 `GATE_v4_1`（§12.4，服务端 4.1 plan T20）。
2. 等 4.0.1 正式版（09-18 判定）成为 GitHub latest，并确认两台面板 `/packages/manifest.json` 的 `tag` 已是 `v4.0.1`（§12.6）。如果 4.0.1 正式版推迟、必须先打 4.0.2 rc，就按「rc 即灰度」处理：第 3 步必须在打 tag **之前**完成。
3. **硬前提：baiyi 验收（§11.6 测试 79）在打任何 4.0.2 tag 之前完成。** 这是客户端铺开前唯一的闸门：tag 一打、成为 latest 或被 rc 通道面板选中，客户端就开始换，服务端的升级顺序拦不住（§12.2 推论 2），出了问题也不能经面板回退（推论 3）。做法：baiyi 是 musl 构建机，本地构建装上；验收期间临时 `bui-c update --auto off`（`cli.rs:1017-1029`），否则每日自更新会把本地构建换回面板那一版：本地是 4.0.2、面板 manifest 是 4.0.1，**版本号不同**直接判 `NewVersion`（`update.rs:369-371`），轮不到比 sha256。验收完 `--auto on` 之后，下一次每日自更新同样会把 baiyi **暂时换回面板版本**（4.0.1），直到面板选中 4.0.2 的 tag；这不影响验收结论。想让它一直跑 4.0.2，就把 `--auto off` 保持到面板 `/packages/manifest.json` 的 `tag` 变成 `v4.0.2` 再开。这是验收手段，与 C5 的触发条件无关（本改动不触发 C5，§6.4）。
4. 打 `v4.0.2-rc1`（按第 2 步的前提它不会经面板推给生产客户端），**baiyi 手工装 rc1 的发布件再验一遍**：它是 CI 构建的 musl 二进制，与之后客户端经面板拿到的是同一条流水线的产物，本地构建验不到它。做法：
   1. `bui-c update --auto off`（`cli.rs:1017-1029`），整个验收期间保持关着；
   2. 从 GitHub Release `v4.0.2-rc1` 的资产下载 `bui-c-linux-<arch>` 与 `sha256sums.txt`（资产清单见 `release.yml:247`、`317`；国内网络不通时加镜像前缀）；
   3. 对照 `sha256sums.txt` 里 `bui-c-linux-<arch>` 那一行核对 `sha256sum` 的输出，不符就停；
   4. 复制到 `/usr/local/bin/` 下的临时文件、`chmod 755`，再 `mv` 覆盖 `/usr/local/bin/bui-c`（`paths.rs:7` 的 `SELF_BIN`；同目录 rename 与自更新的 `replace_self` 同一做法，`update.rs:311-317`，不要 `cp` 直接覆盖正在用的二进制）；
   5. `bui-c status --json` 的 `version` 为 4.0.2，再跑一遍 §11.6 测试 79。

   通过后打 `v4.0.2` 正式 tag，`--auto` 按第 3 步末尾的办法恢复。4.0.2 的 tag 不在 T20 门禁范围（§12.4），闸门就是第 3、4 两步。
5. 服务端：bwg-rick 手动升到 4.0.2，核对面板 `tag` 与 `/packages/` 里的 bui-c；用户验收后 bwg-tizi 手动升级（bwg-rick 先测、用户验收后才动 bwg-tizi）。**注意：这一步的顺序只管服务端二进制。** 客户端二进制在正式 tag 发布后 ≤ 24 小时内就会经两台面板的自检发出（跟随 bwg-tizi 的客户端不必等 bwg-tizi 手动升级）；手动升级只是让包缓存立即同步、把等待窗口收紧到路径 A。
6. 两台都升完起算，等 ≥ 26 小时（§12.5 路径 A）。期间**不从还没发出 4.0.2 bui-c 的面板导入**（该面板的 manifest 会把客户端换回去，§12.3）。
7. 放行 4.1 的核对（§12.8）。
8. 服务端 4.1 先上 bwg-rick、验收后 bwg-tizi。打 4.1 的任何 tag（含 rc）之前，本地跑 T20 门禁且 `GATE_v4_1` 已填本线交付的 SHA（§12.4）。

4.0.2 客户端上线后若发现问题：打更高版本号（例如 4.0.3）向前修复，新 tag 仍受门禁；不要试图让面板发回 4.0.1（≥ rc2 面板不会写入更低 rank）。

### 12.8 放行 4.1 的条件（主理人拍板）

- **逐台核对**已知客户端 `bui-c status --json` 的 `version`（`cli.rs:786`）≥ V₀。V₀ = 首个含本改动的正式版本，现定 4.0.2；版本号若先被别的发布占用，以实际首个含本改动的版本为准。已知机器只有 baiyi 与另一台 socks 模式的生产机；后者 `auto_update` 若关着，手工 `bui-c update` 并记台账。
- **兜底**：客户端核对只能覆盖已知机器，验收判据以服务端的旧订阅兼容不变量（§12.10）为准。
- 期间不从未升级的面板导入；或先把两台服务器都升到含本改动的版本再开始等（§12.7 第 5-6 步即如此安排）。

### 12.9 过渡期矩阵

| 客户端 \ 服务端 | 4.0.x（每槽一个端口） | 4.1（共享端口 + 兼容段 REDIRECT） |
|---|---|---|
| 4.0.0 / 4.0.1，从不重新导入 | 正常 | 旧端口经兼容段 REDIRECT 可用，直到兼容段下线 |
| 4.0.0 / 4.0.1，重新导入 | 正常；换槽后会多出同账号副本，**旧副本能用但从另一个住宅 IP 出去**（F5），活动节点若停在旧副本就一直走错的 IP | 能用，但会多出 `-2` 或主机名副本，活动节点可能仍停在旧端口 |
| ≥ V₀，从不重新导入 | 正常 | 靠兼容段（F1），兼容段下线前必须重新导入 |
| ≥ V₀，从面板重新导入 | 正常；换槽后原地替换；存量重复提示、token 名改掉 | 原地换成 40000，零新增条目 |
| ≥ V₀，经订阅重新导入，活动节点来源是从未被订阅或面板命中过的 V3 / Paste | 同上，但端口变了时另起一条（§5.3） | 另起一条 40000 的节点，活动节点留在 4000x，要切换过去 |

新客户端与旧服务端之间没有任何依赖。

### 12.10 旧订阅兼容不变量与兼容段下线（服务端裁定：服务端 4.1 spec §7.5、§2.4、§14 裁决 3）

客户端不会自动刷新节点（F1），从不重新导入的机器会一直拿着手里那份节点参数，所以 4.1 必须保证服务端 4.1 spec §7.5「旧订阅兼容不变量」的四条：

1. HY2 认证串仍是 userpass 的 `username:password`（客户端按这个形状发，`crates/bui-schema/src/parse/node_uri.rs:41-53`）；
2. obfs 密码、SNI 与证书不变；
3. `41000-50000` 与 `4000x` 的 UDP 全部转发（nft prerouting 与 output 两条链 × 整段与兼容段，服务端 4.1 spec §2.4）；
4. 按用户粘槽的出口路由语义不变（同一用户仍从他粘着的那个住宅 IP 出去）。

另有两条同样由服务端写死：住宅 HY2 的 label「HY2住宅」不改（服务端 4.1 spec §7.4 第 1 条；`profile_name` 与 kind 判定都不漂移）；验收用 rc 客户端渲染出的、升级前就在盘上且没有重新导入过的 `config.json` 连 4.1 服务端验通（服务端 4.1 spec §7.5 末段，进服务端 4.1 plan T19）。

**兼容段 REDIRECT 不永久保留**（服务端 4.1 spec §2.4；判据不收紧，§14 裁决 3）。兼容段 `40001-40007` 的两条规则带 `counter`，`bui status` 打印最近命中次数；**连续 30 天为 0** 才 `bui set hy2-resi-compat off`。服务端 4.1 spec §2.4 要求下线流程三件齐备：

1. 下线公告里**单列一段**「Linux 客户端（bui-c）必须重新导入一次」，给出 `bui-c import --sub -` 与菜单 `[3]` 两条路径；
2. 下线前用 `counter` 判定兼容段是否仍有命中：**有命中说明还有机器没导入，窗口顺延**；
3. 公告与下线动作之间**不少于 30 天**。

所以 §12.5 的等待窗口只约束「多久之内所有机器升上来」，旧节点在兼容段下线前一直可用。

本线给公告里 bui-c 那一段提供具体步骤（与 HANDOVER 同一段文字）。光说「重新导入」不够：客户端不会自己刷新节点（F1）；活动节点是 V3 / Paste 来源、或被 §5.3 挡下的机器，重新导入后当前节点可能仍停在旧端口（§5.3、§5.4 ①、§12.9 末行）；门槛外的猜 kind 条目连面板导入也不会更新（§5.2）。所以第 ③ 步以第 ② 步看到的结果为准，不以导入时打了哪句提示为准：

> Linux 客户端（bui-c）请在兼容段下线前操作一次：① 从面板复制链接重新导入（`bui-c import --sub -` 粘贴，或菜单 [3]）；面板导入会更新同一账号的旧条目。② 用 `bui-c status` 看当前节点，再用 `bui-c list` 看它的端口是不是新端口（住宅 HY2 为 40000）。③ 如果第 ② 步看到当前节点仍是旧端口：在 `bui-c list` 里找同类型（直连或住宅）、端口是新端口的那一条，确认它能用后 `bui-c switch <它的名字>`（或在菜单里切换）。

30 天零命中的判据能兜住还在用的机器（在用就有命中，窗口顺延），兜不住的是下线前 30 天一直关机或离线的机器；这类机器开机后若断网，按上面三步处理。

另记：rc2 `crates/bui-schema/src/slots.rs:220` 的注释写「`bui-c` 的每日 timer」会更新订阅，与 F1 不符；服务端已在 origin/v4 改正（`1fc85d0`，现为 `slots.rs:220-224`「Linux 客户端 `bui-c` 根本不会自动刷新节点」）。

### 12.11 兜底

- 客户端被降回 4.0.1 / 4.0.0：见 §6.4；再升级、再导入即可收拢。降到 4.0.0 丢墓碑、token 名可能复发。
- 4.1 之后 apply 失败：旧端口在兼容段下线前仍可用（F4）。
- 4.0.2 客户端本身有问题：只能向前修复（§12.7 末段）。

## 13. 风险

- **R1 存量重复不自动合并。** 用户不答 y，每次导入涉及该账号都提示一次、菜单停一次。换来的是不删一条能用、出口 IP 不同的节点（F5）。用户故意为同一账号保留端口变体（自建转发）也不受影响。
- **R2 kind 门槛依赖备注。** 用户把住宅链接的备注改成含「直连」，门槛会信它。概率低，且 rc 解析时就已按这个备注判 kind。
- **R3 零手动依赖兼容段 REDIRECT（F1）。** 从不重新导入的机器用旧端口，兼容段下线前必须重新导入；靠公告里单列的 bui-c 一段与 counter 有命中就顺延（§12.10，服务端 4.1 spec §2.4）。下线前 30 天一直离线的机器兜不住。
- **R4 导入后 apply 失败不回滚（F4）。** 候选做法：落盘前活动节点内容变了先调 `Engine::preflight`（`engine.rs:185`），另立项。
- **R5 token 只在文件里留到导入。** 人读输出已打码；`--json` 与 `profiles.json` 原样（有意）。
- **R6 token 名误判。** 用户名恰好是 32 位十六进制的账号会被改名、被打码，与 `source.rs:95-97` 同口径，接受。
- **R7 同一批同账号「后一条生效」。** 同一次粘贴里新旧链接的顺序决定结果；有测试钉住。
- **R8 分流只升不降可能留住过期分流。** 面板改了分流、而用户恰好经订阅退回路径刷新时，保留的是上一次面板给的分流；下一次面板导入即纠正。
- **R9 Reality uuid 轮换仍留旧节点。** 不在范围。
- **R10 改动面。** `Stored.added` 从 `usize` 改 `Vec`（`cli.rs:469`、`554`、`699`）、三个结构加 `extra`（所有结构字面量）、显示名替换约二十处，属机械修改；新文案要过宽度表（§11.5）。
- **R11 D1 偏保守。** 活动节点来源是 V3 / Paste、且**从未被订阅或面板导入命中过**（命中过一次，不论当时有没有变化，来源都已升级，§5.5）的机器，第一次经订阅刷新遇上端口变化时另起一条，用户要按说明句切换过去；过期粘贴另起的条目要用户自己删。
- **R12 客户端不是只升不降。** 客户端从另一台还没发出新 bui-c 的面板导入一次就会被换回；本设计只能靠操作约束（§12.7 第 6 步），根治是 4.0.1 backlog 项。
- **R13 catch-all 救不了 4.0.0。** 见 §6.3。
- **R14 host 大小写口径变化是跨调用点的。** 只会让「只差大小写」的两条从「不同」变「相同」，方向与墓碑一致；列在 §5.2 并有测试。
- **R15 客户端铺开不可回退。** 正式 tag 发布后 ≤ 24 小时内经面板发出，≥ rc2 面板不写更低 rank；出问题只能向前修复。4.0.2 的闸门是 baiyi 在打 tag 前验本地构建、打 rc1 后验发布件（§12.7 第 3-4 步）；4.1 的 tag 另有 T20 门禁（§12.4）。
- **R16 「无变化」也可能重写一次文件。** 来源升级发生在 `Unchanged` 时，`profiles.json` 会重写一次、输出仍是「节点 X 无变化」；每条最多升两级，之后恢复逐字节不变。
- **R17 门禁依赖清单里的 SHA，且只管 `v4.1` 前缀。** `GATE_v4_1` 填的若不是实现提交（例如只填了文档提交），门禁会放过不含实现的分支；留 `pending` 或填了仓库里没有的提交只会关着失败。交 SHA 时附 `git show --stat`，服务端核对后再填。`v4.1` 以外的 tag 不受门禁（§12.4），靠流程与 §14.2 T4 的可选扩展。
- **R18 面板条目与活动节点同时被挡下时只报 `PanelEntry`。** 被压下去的那条活动节点，若它自身的 kind 也是按备注猜的、且面板当时的端口与它现在的端口不同，之后任何一次面板重新导入它都进不了账号组（门槛是双向的），于是既不被原地替换、也不会作为存量重复被点名，更新不了；而**组非空、走 §5.4 ① 的这一轮**又只打 `PanelEntry` 那一句、不再单独提示指向它——用户不会知道还有一条活动节点卡在旧参数上（组为空走 ③ 时另起了新节点，菜单会按 `added` 问「切换到新导入的 X？」，不落进本缺口）。可达面窄（要同一账号里同时有一条受保护的面板条目、一条 kind 不可信的活动节点，再加一条能进组的条目），缓解：无，本设计不解决（论证见 §9 表后说明，定义见 §5.1 A1）。

## 14. 已定事项与剩余问题

### 14.1 已定事项

归属按收到的结论清单标注，若与主理人记录有出入，以主理人记录为准。

| # | 结论 | 定者 | 落点 |
|---|---|---|---|
| C1 | 账号身份与改名在导入时从 node 现算，不持久化，`profiles.json` 不加身份字段 | 服务端与本线约定 | §6.1 |
| C2 | 往返用例：本版写 → 模拟 4.0.0（八个字段、无 `deleted`）读写回 → 本版读，仍命中同一节点 | 服务端与本线约定 | §6.5、测试 19 |
| C3 | `Profiles` / `Profile` / `Tombstone` 加 flatten catch-all，node 不加；只保护降到已带它的版本 | 服务端与本线约定 | §6.3、测试 17-18 |
| C4 | `profiles.json` 只做加法 | 服务端与本线约定 | §6.2 |
| C5 | rc 测试机关自动更新的触发条件；本改动不触发 | 主理人拍板 | §6.4 |
| C6 | token 旧名在端口没变、命中同一连接时也规范化 | 主理人拍板 | §5.6、测试 26 |
| C7 | 墓碑 key 账号维度不变，改名只同步墓碑显示名；改名、active、墓碑显示名同锁一次写盘；输出「节点 <旧名打码> 已改名为 <新名>」 | 主理人拍板 | §5.6、§8.2、测试 27 |
| C8 | 降级口径：≥ rc2 面板不降级跟随者；没记面板或跟随未升级面板仍拿 GitHub latest；替换判据「版本号或 sha256 任一不同」；客户端只升不降另立 4.0.1 backlog | 服务端裁定 | §12.3 |
| C9 | 落点 4.0.2（暂定），先于 4.1；4.0.1 不加代码 | 主理人拍板 | 文首、§12.7 |
| C10 | 公开仓库规矩（中文、示例域名、无 emoji、不写那台 socks 生产机的主机名） | 项目规矩（CLAUDE.md）与主理人 | 全文 |
| C11 | 现状事实（rc2 已发布、两台服务器 rc2、baiyi rc1 + 1.14.1、TUN 回归已过） | 事实更新 | §1.5 |
| D1 | 只升不降扩到节点本身；r3 起受保护条目只接受同参数的非面板来件（比 same_endpoint 严，满足 D1「只有 same_endpoint 才替换」） | 主理人拍板（审查提出）；r3 收紧按 r2 核查 C2 | §5.3、测试 5b、34-38c |
| D2 | 存量重复：命令行只提示；菜单 y/N 确认合并，默认 N；不自动合并 | 主理人拍板 | §5.8、测试 39、63-64 |
| D3 | 人读输出打码前 4 位加省略号；`--json` 与 `profiles.json` 原样；CHANGELOG 注明脚本失效 | 主理人拍板 | §8.3、测试 30-33 |
| D4 | 上线按 tag 推导；tag 门禁以服务端 4.1 plan T20 为准（`GATE_v4_1`，本线交实现提交 SHA）；两条等待路径；4.1 放行前逐台核对 version 并以兼容不变量兜底 | 门禁：服务端与本线约定（服务端 4.1 spec §7.6、plan T20）；放行条件：主理人拍板 | §12.2-§12.8 |
| D5 | 旧订阅兼容不变量（服务端 4.1 spec §7.5）；兼容段 REDIRECT 不永久保留，下线三件齐备（公告单列 bui-c 一段并给 `bui-c import --sub -` 与菜单 [3]；counter 有命中就顺延；公告与下线间隔 ≥ 30 天，服务端 4.1 spec §2.4），判据不收紧（§14 裁决 3）；label 不改（§7.4 第 1 条）；slots.rs 错注释服务端已改（`1fc85d0`） | 服务端裁定 | §12.10 |
| D6 | `same_account` 的 host 不区分大小写 | 主理人拍板 | §5.2、测试 4、52、73 |
| D7 | 分流与来源只升不降随本版上，账号组内找面板成员 | 主理人拍板 | §5.5、测试 57 |
| D8 | kind 门槛按现状 | 主理人拍板 | §5.2 |
| D9 | 两份审查的其余 Important 与 Minor 全部采纳 | 主理人拍板 | 见修订记录 |
| K1 | **来源等级命中即升**：`Unchanged` 时也经 `raise_source` 升级来源，等级 ApiNodes > Subscription > Paste = V3，同级不换；说明句按挡下原因分两种出路 | r1 核查提出，主理人转达本轮必须解决；属 D1、D7 的补全 | §5.3、§5.5、测试 17a、35-37、36a-36b |
| K2 | **T1 定为 `tell`**：端口变化行与其余新增行一律经 `tell`，计入附加行，进宽度表；合并问句缩成「要合并吗？」（原 T6） | r1 核查当场核定（F7） | §5.4、§5.8、测试 66、75 |
| K3 | r2 的「版本号 ≥ V₀ 的 tag 都检」**在 r3 撤回**：tag 门禁以已合并的服务端 4.1 plan T20 为准，≥ V₀ 只作 §14.2 T4 的可选建议；§12.8 仍按 version ≥ V₀（首个含本改动的正式版本，现定 4.0.2）逐台核对 | r1 核查提出；r3 按 r2 核查 D-b 与服务端 plan 对齐 | §12.4、§12.8、§14.2 |
| K4 | **客户端铺开从正式 tag 起算，与服务端升级顺序无关；只能向前修复；baiyi 在打 tag 前验收是硬前提** | r1 核查按 rc2 代码核定 | §12.2、§12.7 |
| K5 | **兼容段下线公告里 bui-c 那一段写三步**（从面板重新导入；看当前节点端口；仍是旧端口就在 list 里找同类型新端口那条切过去），HANDOVER 同文；真机测试 80 断言活动节点端口 | r1 核查提出，补全 D5；r3 按 r2 核查 C1 改第 ①③ 步 | §12.10、测试 80 |
| K6 | **挡下原因保护优先、门槛次之、两条都报**：受保护（来源 `ApiNodes`，或非面板来源的活动节点）且与来件不同参数 → 来源 `ApiNodes` 为 `PanelEntry`、否则 `ActiveEntry`，**不论门槛内外**；同时在门槛外时 `Blocked` 的 `kind_unsure` 为真，说明句在「…不变」之后补「同时认不准是直连还是住宅」。其余被挡下的（必然门槛外、不受保护）→ `KindUnsure`。多条仍按 PanelEntry > ActiveEntry > KindUnsure，取舍与 `kind_unsure` 无关；被压下去的活动节点不进 `switch_to`（缺口见 R18） | 服务端 bui 会话裁定，不再上递主理人（**取代** r2 核查 A1 定下的「逐条先判门槛」，见修订记录最后一节） | §4、§5.1、§5.2、§5.3、§5.4、§9、§10、§13 R18、测试 5a、35、36b、44、47、75 |
| K7 | **组非空时也说明被挡下的条目**：① 调 `blocked_same_account`，`PanelEntry` / `ActiveEntry` 各一句，`ActiveEntry` 把留存者加进菜单切换候选（`Stored.switch_to`）；`KindUnsure` 在 ① 不说 | r2 核查 A2 | §5.3、§5.4、§5.10、§10、测试 38b-38c |
| K8 | **同参数**：受保护条目遇非面板来件，除 `label`、`hop` 外 `node` 全等才替换 | r2 核查 C2（选方案 a） | §4、§5.1、§5.3、§2 目标 3、测试 5b、38a |
| K9 | `bury` 保留同 key 旧墓碑的 `extra`；import-v3 口径改为「不改字段」（`run` 可能原样写一次）；§12.4 不以 latest 按版本排序为前提；baiyi 验收写明版本号不同与 rc1 发布件手工装法；§12.5 计时起点；CHANGELOG 写活动节点挪端口换出口 IP | r2 核查 B1-B3、C3-C6 | §5.1、§5.9、§12.4、§12.5、§12.7、§10 |
| K10 | **取节点口径**：两个入口（`bui-c import` 与菜单 [3]）、6 个调用点（`cli.rs:878`、`884`、`2881`、`3005`、`3010`、`3014`），按入口核对；`check.rs`、`update.rs` 一处不取 | 与服务端 4.1 spec §7.5 首段对齐（r2 核查 D-c） | F1 |
| K11 | **服务端已合并、与本设计无直接依赖**：`bui hy2-prestart` 拒绝清理正在运行的实例的护栏（`8c16ccf`）；`slots.rs` 错注释修正（`1fc85d0`）；兼容段下线通知三条已写进服务端 4.1 spec §2.4（`ffeb0f4`） | 服务端 | §12.10 |

### 14.2 剩余问题（纯技术细节，不阻塞开工）

- **T2 `extra` 字段带来的结构字面量改动。** `Profile { .. }` 在测试与 `testutil.rs:89-95`、`110-119` 里很多。建议：给 `Profile` 加一个构造函数，生产代码与 `named()` 走它，测试里的字面量补 `extra: Default::default()`。
- **T3 flatten 与既有字节兼容测试。** flatten 让序列化走 map 路径，`serde_json` 的 pretty 输出预期不变，但以 `profiles.rs:781` 与 `843` 两条测试为准；若出现差异（键序），把 `extra` 放在每个结构的最后一个字段。
- **T4（可选建议，给服务端）把门禁从「前缀 `v4.1`」扩到「版本号 ≥ V₀」。** 门禁本身以 T20 为准、由服务端实现（§12.4），本条不阻塞任何事。T20 的键是 minor 前缀（`v4.1` → `GATE_v4_1`），不在清单里的前缀一律放过。「版本号 ≥ V₀（4.0.2）的 tag 都要求本改动是祖先」相对 T20 多覆盖两类：① `v4.0.x`（x ≥ 2）里从不含本改动的分支打出的热修 tag，客户端版本号不同就换（§12.3），被面板或 latest 选中时会悄悄换掉按账号匹配；② `v4.2` 起的新 minor，T20 要每开一个 minor 手工加一个键，漏加就放过。它不能靠给 T20 加 `GATE_v4_0` 实现：前缀键会连 `v4.0.1-rcN` 这类低于 V₀ 的合法 tag 一起卡住。若采纳，建议在 `check-release-gate.sh` 里加一种「`MIN_<名>=<x.y.z> <sha>`」按 x.y.z 数值比较的条目，与前缀键并存，退出码与文案沿用 T20。
- **T5 `DupGroup` 去重。** 同一批里同账号出现两次（门槛内）会两次推同一个留存者。建议按留存者名字合并，`others` 去重，提示只打一次。
- **T7 `raise_source` 要不要顺手更新 `imported_at`。** 建议不更新：`imported_at` 在 rc 里只随内容替换变化（`upsert` 的 `Unchanged` 保住原值，`profiles.rs:310`），来源升级不是内容变化。
- **T8 命令行下 `tell` 的两列缩进。** 命令行 `indent` 为空（`cli.rs:182`），经 `tell` 的行比经 `say` 的行多两列缩进。建议接受（rc 的删除流程在命令行下已是如此）；若要对齐，可给 `tell` 加一个「命令行不缩进」的分支，但要同步改宽度表的折法，不建议在本改动里做。

### 14.3 另立项（不在本设计）

- 客户端自更新只升不降（4.0.1 backlog）。
- Reality uuid 轮换留下的旧节点清理。
- `check` 里定期刷新节点（与决策 10 冲突，需先改决策）。
- 导入后 apply 失败的回滚（R4）。
- 服务端 4.1 spec §7.4 第 2 条引用的 rc 行为（「`same_account` 同名沿用 → `upsert` 原地覆盖，不会出重复节点」）只对名字恰好等于 `profile_name` 的条目成立（§1.2），「4.1 端口变化对 bui-c 是原地覆盖」要本改动上线后才对所有名字成立；建议服务端在该条后注一句指向本设计，小项。
- rc2 `cli.rs:2990-2993` 注释「v3 面板（bwg-tizi）没有 `/api/nodes`」更正为当前事实（两台都是 v4 面板；退回订阅针对的是 v3 面板与 401 / 404 链接），小项。

## 修订记录

审查编号：**甲** = review:correctness（v1，fix-then-ready，7 Important + 4 Minor）；**乙** = review:migration-rollout（v1，fix-then-ready，2 Critical + 3 Important + 3 Minor）；**核一** / **核二** = 对 r1 的两份核查；r2 → r3 的对应项沿用对 r2 核查与服务端对齐的编号 A1-A2、B1-B3、C1-C6、D-a-D-d。

### v1 → r1

| 变化 | 对应 |
|---|---|
| 标题与落点从 4.0.1 改为 4.0.2（暂定）；基线改为 rc2（bui-c 与 rc1 相同），服务端引用以 rc2 为准 | C9、C11 |
| §0 重写，反映全部已定结论 | 全部 |
| 新增 §1.5 现状；§12.7 的未来时步骤改成当前事实与新顺序；删掉生产机主机名 | C10、C11 |
| §1.2 改为「在命名路径上只在 `cli.rs:529` 被读」 | 甲 Minor（§1.2/§8.2） |
| §1.3 改正夹具论断（`baiyi_like` 全是 ApiNodes，门槛从不触发）；写明备注不含「住宅」的 v3 住宅节点不在范围 | 甲 Important（夹具）、乙 Minor（A-D 覆盖范围）、D8 |
| 新增 F5「鉴权不看槽」为已核实事实，撤销原 R1 的「未核实」；文案不再暗示死节点 | 甲 Important（R1）、乙 Important（R1）、D2 |
| 新增 F6「客户端版本跟着 tag 走」 | 乙 Critical（上线时序）、D4 |
| 新增 §5.3「只升不降扩到节点本身」，`movable` 加 `protected`；测试 34-38 | 乙 Critical（过期粘贴覆盖活动节点）、D1 |
| 存量重复从自动合并改为「命令行提示、菜单确认、默认 N」；新增 `merge_into`、`DupGroup`；测试 39、63-64 | 甲 Important（R1）、乙 Important（R1）、D2 |
| 本批副本当场并掉（不是存量），`retain` 一并清 `restored`；③ 先查没进组的同账号条目（不看名字）再打说明 | 甲 Minor（③ 与合并循环）、D9 |
| `pick_keeper` 次序改为 active > 导入前已存在 > same_endpoint > wanted > 列表位置；测试 27 重写为可达场景（现测试 41） | 甲 Important（留存者排序）、D9 |
| 分流取自合并前整个账号组里的面板成员；测试 57 | 甲 Important（只升不降漏洞）、D7 |
| token 改名输出「节点 <旧名打码> 已改名为 <新名>」，一次写盘；端口没变也改 | C6、C7 |
| 新增 §8.3 人读输出打码（含 import-v3 墓碑名单），`--json` 与 `profiles.json` 不打码；CHANGELOG 注明脚本失效；测试 30-33 | 甲 Important（import-v3 泄露）、乙 Important（token 安全项）、D3 |
| token 名活动节点而规范名被同账号占着：合并后留存者取回规范名；测试 16、65 | 乙 Minor（token 改名 -2）、D9 |
| `same_account` host 不区分大小写，`same_endpoint` 去掉冗余的 host 比较；列出受影响调用点；测试 4、52、73 | D6 |
| §5.2 写明门槛只作用于 Direct kind、不影响 4.1 住宅换端口，门槛按现状 | 甲 Important（夹具）、乙 Minor、D8 |
| §7 把「删新端口副本、留旧端口副本、再导入」写成有意的新行为；测试 54 | 甲 Important（§7 行为变化）、D9 |
| §8.2 删掉 `heal_active` 一项 | 甲 Minor、D9 |
| 新增输出行经 `tell` 折行；宽度测试对齐 `{40,50,59,60,80,100}`；「复用 buried_list 的截断」改为「复用个数上限与净化」 | 甲 Minor（输出行宽）、D9 |
| §6 重写：不加持久化字段；catch-all；只做加法；降级补 4.0.0 的两个后果；C5 判断；往返测试；`upsert` 替换时保留 `extra` | C1-C5、乙 Minor（§6 降级）、D9 |
| §12 重写：客户端取 manifest 的来源、面板 manifest 由自检按 tag 决定、替换判据「版本或 sha256」、降级口径、tag 门禁、两条等待路径、4.0.2 预发布是否推给生产客户端、baiyi 验收位置、放行 4.1 条件 | 乙 Critical（上线时序）、甲 Important（上线缺口）、甲 Minor（包缓存首 tick、404 回退预发布）、C8、D4 |
| 兜底改为「旧订阅兼容不变量」清单；REDIRECT 不永久保留；label 不改；slots.rs 注释由服务端改 | 乙 Important（REDIRECT 不充分）、D5 |
| §13 新增 R11-R14；§14 改名为「已定事项与剩余问题」 | D1、C3、C8、D6、本轮要求 |

### r1 → r2

| 变化 | 对应 |
|---|---|
| **来源命中即升**：新增 `Source::rank`、`raise_source`、`best_source`；§5.4 ① 在 `Unchanged` 时也升来源，`Replaced` 时写入三者最高；§5.5 拆成「分流」「来源」两条；§1.2 补 rc `upsert` 不写来源的事实（`profiles.rs:313-315`）；§5.3 表注明「条目来源是已升过级的」；R11 措辞改为「从未被订阅或面板命中过」；§9 加三行；测试 17a、36a、36b | 核一 Important（Unchanged 不改 source）、D1、D7 |
| **说明句按原因分两种出路**：`Blocked` 拆成 `PanelEntry` / `ActiveEntry` / `KindUnsure`；活动节点那一句改为「确认新节点能用后可以切换过去」，不再叫人从订阅重新导入（照做无效）；§10 两行；测试 5a、34、35、37 | 核一 Important 的连带问题（原提示在订阅路径上是死循环） |
| **端口变化行改走 `tell`**，T1 当场定为 `tell`（新增 F7：`tell` → `show` → `emit` 进 transcript，`outcome_since` 只跳过 asides）；两行 rc 原文仍走 `say`；测试 22 改 `trim()` 比对，测试 66、75 覆盖端口变化行；原 T6 的合并问句缩短一并定下 | 核一 unresolved（端口变化行宽度矛盾）、核一 Minor（§5.4/§11.5/T1）、D9 |
| §6.2 改正：4.0.0 的 `Profiles` 是**八个**字段（v4.0.0 `profiles.rs:67-77`，无 `deleted`），`Profile` 五个（`57-65`）；§6.5 `Profiles400` 不含 `deleted`，第 2 步断言墓碑与未知字段已丢；测试 19 同步 | 核一 Minor、核二 Minor（§6.2 字段数） |
| 删掉「bwg-tizi 是 v3 面板」的过期论据：§1.5 写明两台都是 v4 面板、现在走订阅退回的是 v3 面板与 `/api/nodes` 回 401 / 404 的链接（`cli.rs:3004-3011`、`3018-3020`）；§5.2、§5.3、§9 改写受益对象；rc2 `cli.rs:2990-2993` 注释更正列入 §14.3 | 核一 Minor、核二 Minor（§5.2/§5.3 与 §1.5 矛盾）、C11 |
| **tag 门禁阈值改为「版本号 ≥ V₀」**（V₀ 记在清单文件，正式版与 rc 都查），并写明为什么版本号 < V₀ 的 tag 伤不到已铺开的客户端；§12.8 改核对 version ≥ V₀；T4 补清单格式与多提交号；新增 R17 | 核二 Important（门禁只卡 4.1）、核一 Minor（§12.4 阈值） |
| §12.2 加三条推论：服务端升级顺序管不住客户端铺开；稳定版缓存的面板 ≤ 24 小时选中正式 tag；≥ rc2 面板不能回退。§12.6 补「正式 tag 一定会推」；§12.7 第 3 步写成硬前提、第 5 步加注、末段写只能向前修复；新增 R15 | 核一 Minor、核二 Minor（§12.7 第 5 步） |
| §12.9 矩阵加「经订阅重新导入、活动节点是未命中过的 V3 / Paste」一行；§12.10 下线通知写成三步操作（从面板重新导入、确认当前节点端口、另起了就切换），HANDOVER 同文；测试 80 断言活动节点端口是 40000 | 核二 Minor（REDIRECT 通知不足） |
| 行号按 rc2 重新核对并修正若干处（墓碑 `cli.rs:520-526`、`import_v3.rs:149-174`、`source.rs:263`、`delete.rs:44-64`、`buried_skipped` 补 `cli.rs:918`、`render_nodes` / `render_status` 函数起始行等）；新增 R16（「无变化」也可能重写文件） | 本轮要求（行号重新核对） |
| §14.1 增加 K1-K5（本轮定下的事项）；§14.2 删去已定的 T1、T6，新增 T7（`imported_at`）、T8（命令行缩进） | 本轮要求 |

### r2 → r3（本版）

| 变化 | 对应 |
|---|---|
| `blocked_same_account` 的原因逐条先判门槛：门槛外一律 `KindUnsure`，门槛内受保护且不同参数才报 `PanelEntry` / `ActiveEntry`，多条按 PanelEntry > ActiveEntry > KindUnsure；§4 加「被挡下的同账号条目」、§5.1 注释、§9 两行、§10「门槛挡下」行；测试 5a 加「既受保护又在门槛外」两格，测试 44 写明条目为 ApiNodes 且 active、断言没有 `protected_new`，测试 35、36b 写明粘贴备注可信 | A1 |
| 组非空时 ① 也调 `blocked_same_account`：`PanelEntry` / `ActiveEntry` 打 `protected_kept`，`ActiveEntry` 把留存者记进 `Stored.switch_to`；菜单候选 `switch_to` 排最前，问句 `切换到 {keep}？`；`KindUnsure` 在 ① 不说；§5.3 文案四句、§5.4、§5.10、§9、§10；新测试 38b（订阅刷新、活动 V3 被挡、Subscription 副本被换、菜单问切换到 keep）、38c；宽度表加两句与问句；§12.10 第 ③ 步同步 | A2 |
| `bury` 对同 key 旧墓碑取走 `extra` 再写新墓碑，仍移到末尾；§6.3 同步；新测试 14a | B1 |
| §5.9 改为「`import` 不改字段」，写明残留 v3 单元在时 `run` 会原样 `save` 一次（`import_v3.rs:300-313`、`350`）；测试 31 夹具不含 v3 单元，另加带残留单元一格断言内容逐字节不变 | B2 |
| §12.4 不再以「GitHub latest 按版本号排序、低版本成不了 latest」为前提（`release.yml:303-310` 只给 rc 加 `--prerelease`，`313-317` 不指定 latest）；改为据此解释 §12.7 第 2 步的顺序 | B3 |
| §12.10 公告第 ① 步去掉「任何」，改「面板导入会更新同一账号的旧条目」；第 ③ 步触发条件改为第 ② 步看到当前节点仍是旧端口，按「同类型、新端口」在 list 里找再 switch；HANDOVER 同文 | C1 |
| 受保护条目遇非面板来件改为要求「同参数」（除 `label`、`hop` 外 `node` 全等）才替换：新增 `same_params`，`movable` 改用它；§0 第 3 条、§2 目标 3、§4、§5.3 表与正文、§7、§9 收窄措辞，说明句改「连接参数不同」；新测试 5b、38a（面板开 obfs 后粘贴旧链接不覆盖活动节点） | C2 |
| §12.7 第 3 步更正为「版本号不同」（本地 4.0.2 对面板 4.0.1 走 `NewVersion`，`update.rs:369-371`），写明 `--auto on` 后会暂时回到面板版本及怎么避免；第 4 步 rc1 不再「可选」，写明 baiyi 手工装 rc1 发布件的五步（`--auto off`、下 Release 资产、对 `sha256sums.txt`、同目录 `mv` 替换 `/usr/local/bin/bui-c`、核 version 后跑测试 79）；测试 79 两个时点各跑一次；R15 同步 | C3 |
| §12.4 写清两道闸：真正拒绝打 tag 的是打 tag 前本地跑门禁；CI 是第二道，`verify` 失败则 `release` 作业不跑、不创建 Release，面板自检与客户端只看 Release（`kernels/mod.rs:55`、`79`、`292-307`，`update.rs:23`、`162`、`208-222`）；悬空 tag 要手动删后重打，`workflow_dispatch` 只演练（`release.yml:7`、`34-39`、`190`、`298-299`、`313`） | C4 |
| §12.5 表头改为「从 tag 发布算起」，A 行注明「从服务端手动升级完成算起」，B 行注明从 tag 发布算起 | C5 |
| §10 CHANGELOG 与 HANDOVER 加「活动节点若还停在旧槽端口，导入后会挪到服务端分配的端口，出口住宅 IP 换成服务端为该用户分配的那个」 | C6 |
| 引用服务端 4.1 spec：文首定义路径；§1.1 兼容段 `40001-40007` 与 nft（§2.4）、label（§7.4 第 1 条）；§12.10 改名「兼容段下线」，四条不变量按 §7.5、下线三件（公告单列 bui-c 并给两条路径、counter 有命中顺延、间隔 ≥ 30 天）按 §2.4、判据不收紧按 §14 裁决 3；全文「REDIRECT 下线」统一为「兼容段下线」（§1.3、§9、§12.9、§12.11、R3）；§14.1 D5 同步 | D-a |
| tag 门禁以服务端 4.1 plan T20 为准：`check-release-gate.sh` + `required-commits.env`，键 `GATE_v4_1`，现为 `pending` 即 fail-closed；本线交付改为「不自动合并同账号重复节点」的实现提交落地后把 SHA 交服务端填进去；删掉 r2 自创的「≥ V₀、所有 tag 都检」，改为 §14.2 T4 可选建议并说明它相对 T20 多覆盖 4.0.x 热修与新 minor；文首、§0 第 13 条、§12.4、§12.7 第 1/4/8 步、§12.8、R17、§14.1 D4/K3 同步 | D-b |
| F1 改为「两个入口、6 个调用点（`cli.rs:878`、`884`、`2881`、`3005`、`3010`、`3014`），按入口核对」，写明 `check.rs`、`update.rs` 一处不取；`fetch_panel` 本身的调用点 878、3005 保留并注明 | D-c |
| §14.1 新增 K6-K11：A1、A2、C2 与其余小项的落点，取节点口径，服务端已合并的 `hy2-prestart` 对活实例拒绝护栏（`8c16ccf`）、`slots.rs` 错注释修正（`1fc85d0`）、兼容段下线通知三条写进服务端 spec §2.4（`ffeb0f4`）；§14.3 去掉已完成的 slots.rs 注释项，新增服务端 spec §7.4 第 2 条的小注建议 | D-d |

### r3 → 定稿（核查后手工追加）

对 r3 的两份核查都判 ready；这一节记的是判 ready 之后手工追加的三轮改动。**第一轮**（A1、M1、M2 与连带的 R18）：一处定序改判（服务端 bui 会话裁定，不再上递主理人）与两条核查 Minor。**第二轮**：两名对抗核查员对第一轮判 fix-then-ready，按问题清单修 I1-I6（一个 Important、五个 Minor），只修清单、不重写全文、不动其他结论——优先级（PanelEntry > ActiveEntry > KindUnsure）、`switch_to` 口径与 K7 都原样不动，I1 只换论证。**第三轮（本轮，收尾）**：两名复核员对第二轮判 ready，但一致提出三条测试夹具的 Minor（J1-J3），其中两条会让用例静默空过，照修；对第三轮的复核判 fix-then-ready（一个 Important：J1 把 36b 的条目改成住宅口后，它的「追加版」那一格变得不可达；六条 Minor），已逐条改完，36b 改回直连。三轮都只动论证与夹具，方案结论一处未改。上面「r2 → r3」那一节按当时的结论如实保留，不回改。

| 变化 | 对应 |
|---|---|
| **挡下原因的定序由「门槛优先」改判为「保护优先，门槛次之，两条都报」**：受保护且与来件不同参数的条目，**不论门槛内外**都报 `PanelEntry` / `ActiveEntry`（来源 `ApiNodes` 为前者）；`Blocked` 的这两个变体加一位 `kind_unsure`，为真时说明句在「…不变」之后、分号之前插入 `，同时认不准是直连还是住宅`（四句基础文案各配一个追加版，共八种输出，插入点一致）；其余被挡下的（必然门槛外、不受保护）才是 `KindUnsure`；多条的取舍不变。落点：文首状态行、§4 术语表、§5.1 `Blocked` 与 `blocked_same_account` 注释、§5.2 表第一行与表后新增一句（第二三行来件是面板、`protected` 恒假，不受影响）、§5.3 前导与追加半句一段（含出路在门槛外照样有效的论证）、§5.4 ① 与 ③ 的 `match`（变体改 struct 形式、整枚 `Blocked` 传给文案函数）与注释、§5.4 A2 小结、§9 三行、§10 两行（新增追加版一行，「门槛挡下」一行改成不受保护的例子，旧例里的 `alice-hy2-direct` 是面板来源、A1 下报的是 `PanelEntry`）、§14.1 K6；测试 5a、35、36b、44（改名 `a_guessed_kind_paste_is_explained_as_a_protected_panel_entry` 并反转断言）、47（写明该条目不受保护）、75（四种说法，带追加半句的是最长样本） | A1（服务端 bui 会话裁定，取代 r2 核查 A1） |
| 测试 31 第二格（带残留 v3 单元、断言 `profiles.json` 逐字节不变）补上夹具的硬前提：光有残留单元不够，`import_v3.rs:311-313` 的 `nothing_to_apply` 是第二道早退闸，墓碑命中的 v3 目录走 `r.buried`、进不了 `r.existing`，所以要在 `run` 之前就让 `prof.active` 指向一条真实存在的条目（`import_v3.rs:341` 之前还有一次 `active_profile().ok_or_else`）；再加断 `Report::removed_units` 非空，证明确实走到 `import_v3.rs:350` 的 `save`，逐字节不变的断言不空过。§5.9 正文本身已准确，不动 | M1（r3 核查 Minor） |
| §9「同时有面板条目与活动节点被挡下时只报 `PanelEntry`」那一行的理由按新定序重新论证，并落成 §9 表后一整段：旧理由依赖「`ActiveEntry` 蕴含门槛内」，A1 之后失效（门槛双向）；新论证是「面板来源条目自己一定救得回（两边 kind 恒可信、面板来件不触发 `protected`），被压下去的活动节点只有自身 kind 可信、或面板当时端口恰好相同才一起更新」。口径维持不变：仍只报 `PanelEntry`，`switch_to` 只看实际报出来的那一条（K7 不改），缺口记为 R18；§5.1 `blocked_same_account` 注释与 §5.4 ① 注释同步。**这段论证当时把「救得回」写成了「必然被替换」，第二轮由 I1 改成「必然进组」（见下表），结论不变** | M2（r3 核查 Minor） |
| §13 新增 R18：面板条目与活动节点同时被挡下时，自身 kind 也是猜的那条活动节点可能继续卡在旧参数上，且本轮不再单独提示；缓解：无，本设计不解决 | M2 |

第二轮（核查后第二轮修订）：

| 变化 | 对应 |
|---|---|
| **M2 新写的论证里「`movable` 恒真 → 任何一次面板重新导入都必然替换它」这一步改掉**（与 §5.4 矛盾：进组不等于被替换，组内只有 `pick_keeper` 选中的那条被 `upsert`，其余下标 < `known` 的成员进 `stale` → `push_dups`，只作存量重复提示，命令行不动列表、菜单答 y 才并掉）。改成能由 §5.4 推出来的口径：面板重新导入时它必然**进组**——是留存者就被原地替换，不是留存者（同组的活动节点按 `pick_keeper` 第 1 级胜出）就作存量重复被点名、答 y 并入留存者，两种落法都把这个账号更新到了面板参数，所以「要更新它请从面板重新导入」照做仍然有效。**结论一个不动**：优先级仍是 PanelEntry > ActiveEntry > KindUnsure、`switch_to` 不变、K7 不改。落点：§9 表后说明三条（第二条的「可信则必然一起更新」也改成「进组后按第 1 级被原地替换」；不可信那支补「既不替换、也不列进存量重复」）、§5.1 `blocked_same_account` 注释、§13 R18；测试 38c 补两格钉住 (a) 留存者被原地替换 / (b) 面板条目转存量重复提示 | I1（Important，两名核查员一致） |
| 测试 34 ② 与 38c 写明**哪一边**的备注可信（`gate_ok` 要两边都 `kind_trusted`，A1 之后说明句带不带追加半句正取决于它；按 §11 开头「门槛相关用例默认自建备注不含直连 / 住宅的 V3 profile」读，原文会报成追加版、断言的子串不命中）：34 拆成 ①（同端口过门槛）②（两边备注都含「直连」）②′（条目备注按默认夹具不可信 → 追加版）三格，与测试 35 的 ②/②′ 对称；38c 写明粘贴备注含「住宅」，与被挡的 `ApiNodes` 条目两边都可信 | I2（Minor，两名核查员都提） |
| 测试 31 第二格（带残留 v3 单元、断言 `profiles.json` 逐字节不变）的夹具补齐三条硬前提：active 必须落在**盘上的 `profiles.json` 夹具**里（只在内存里照 `import_v3.rs:878` 赋值，`import_v3.rs:350` 的 `save` 会把它写进盘上文件，逐字节断言必然失败）；这条活动节点必须与被墓碑挡下的是**不同账号**（同账号会被 `import_v3.rs:149` 的 `find_same_endpoint` / 新增的 `find_account_before` 先命中、进 `r.existing` 并 `continue`，走不到 `159-164` 的墓碑分支，主断言失去对象）；照 `run_imports_then_tears_down_and_persists`（`import_v3.rs:620`）备好内核与引擎桩，才过得了 `ensure_kernel`（330）与 `render` / `verify`（342-347） | I3（Minor，两名核查员互补） |
| §9 表后说明第三点给「本轮不再单独提示指向它」加限定「**组非空、走 §5.4 ① 的这一轮**」，并注明组为空走 ③ 时另起的新节点仍按 `added` 进菜单切换追问（§5.10），与 R18 原有的「再加一条能进组的条目」口径对齐；R18 同步 | I4（Minor） |
| §0 结论先行第 3 条补上 A1 的定序（挡下原因先判是否受保护、门槛次之、两条都报；受保护且在门槛外时说明句再补「同时认不准是直连还是住宅」），并指向 §5.1、§5.3 | I5（Minor） |
| §5.1 `Blocked` 加 `#[derive(Debug, Clone, Copy, PartialEq, Eq)]`（测试 5a 要 `==` 断言 `PanelEntry { kind_unsure: true }`，§5.4 ③ 的 `match` 在 arm 里按值再用一次 `why`）；§5.3 追加半句那一段后补三个文案函数的签名 `protected_new` / `protected_kept` / `kind_unsure_new`（前两者收到 `KindUnsure` 时 `debug_assert`）；§5.3 前文括号里的「`menu::protected_new`」改成「③ 用 `menu::protected_new`，① 用 `menu::protected_kept`」 | I6（Minor） |

第三轮（核查后第三轮修订，本轮，收尾）：

| 变化 | 对应 |
|---|---|
| §11 开头补**通则：经 URI 进来的用例，备注必须与被比条目的 kind 同向**。`node_uri` 只看备注含不含「住宅」（`node_uri.rs:25`、`58-62`）：含则 `Hy2Residential`，否则一律 `Hy2Direct`；「直连」二字不影响 kind、只影响 `kind_trusted`。而账号 key 里有 kind：给一条直连条目配一条备注写「住宅」的粘贴，两者不是同一账号，`blocked_same_account` 返回 `None`、走 wanted 起名，断言的字符串压根不出现，用例**静默空过**（测试照样绿）。通则同时给出两条推论：住宅条目经粘贴到不了 `kind_unsure` 为真（含「住宅」即可信，`gate_ok` 必过），所以追加版只能用直连条目测；`profiles.rs` 的纯函数用例不受约束（`Node` 由 `testutil` 直接构造，kind 与 label 独立）。并要求凡断言 `Blocked` 变体或说明句的用例都同时断言 `blocked_same_account` 返回 `Some`，把空过挡住。据此改：35 ② 删掉「或「住宅」」、写明条目是 :10000 直连；5a 的 `ActiveEntry` 那格改成纯函数口径（构造的 kind 相同，label 只决定 `kind_trusted`）；36b 写明条目是直连 :10000、两格粘贴备注分别含「直连」与不含直连住宅（与 35 的 ②/②′ 同构；**第三轮复核时纠正：一度改成住宅口 :40003，会让「追加版」那一格不可达**）；38a 写明活动节点是住宅 :40000、粘贴备注含「住宅」（同端口本就过门槛，这里只要 kind 同向）；38c 写明被挡的 `ApiNodes` 条目是住宅口 | J1（Minor，两名复核员一致；静默空过 = 假绿灯） |
| 测试 38c 新补的 (a)(b) 两格改成**各自另起夹具，不接着上一步**，并写明为什么：上一步里被粘贴原地替换的那条组成员，备注已换成粘贴的「住宅」、kind 可信，面板来件对它 `gate_ok` 且 `protected` 恒假，必然也进组，所以「组里只有被挡过那条」在延续的夹具上不成立。(a) 改为「该账号在列表里只有 P 一条 → 组 = [P]，P 是留存者、被原地替换」；(b) 改为「该账号里除 P 外还有一条自身 kind 可信、备注与 P 同向的活动节点 A → 两条都进组，A 按第 1 级作留存者被替换，P 落进 `stale` 转存量重复」 | J2（Minor，两名复核员一致） |
| 测试 31 第二格补第四条硬前提：**盘上夹具要由 `Profiles::save` 自己写出来**。`save` 是 `to_vec_pretty` 加末尾换行（`profiles.rs:263-270`），手写 JSON 只要键序、缩进、末尾换行有一处不同，`import_v3.rs:350` 原样重写后字节就变了，断言会因格式而非内容失败。做法照 `profiles.rs:780` 那条逐字节用例：内存构造 `Profiles` → `save` 一次 → 读回字节作基准 → 再调 `run`。第三轮复核又补一条：**夹具里每个 v3 目录都要落进 `buried` 或 `existing`**，`v3_machine()`（`import_v3.rs:423-443`）有两个目录，只要有一个走了新建，`r.imported` 非空、写出的就不是原样，逐字节断言与「原样 save」的前提一起失效；断言 `r.imported` 为空 | J3（Minor，两名复核员一致） |
| 第三轮复核查出的五处（一并修，无新编号）：36b 因 J1 改坏（住宅条目的追加版格不可达）改回直连；§11 通则里 `node_uri` 的判据措辞按 `node_uri.rs:25` 更正（只看「住宅」）；38a 补 kind 与端口、去掉与 `gate_ok` 无关的「备注可信」；38c (b) 写明 A 与 P 都停在旧参数（A 已是新参数会走 `Unchanged`，`profiles.rs:313-315`）；文首状态行与本节引言的轮次由两轮改成三轮；另有「每个 v3 目录都要落进 `buried` 或 `existing`、断言 `r.imported` 为空」一条记在上面 J3 行里 | 第三轮复核 |
