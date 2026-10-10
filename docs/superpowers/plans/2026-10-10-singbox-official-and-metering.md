# 官方 sing-box 发行物 + 独立计量模块：实施计划（草稿）

记录日期：2026-10-10。主理人本轮裁决：**不再自建 sing-box，改用官方发行物；住宅按用户计量由独立模块承担**；
并追加要求 **「支持一直更新最新版的 sing-box 核心」**。

> 状态：**本文件为草稿**。第 2、3 节等待 G0 实测结论回填。其余已可执行。

---

## 0. 两条硬约束（先读）

1. **原子性**：本计划第 4 节的所有改动**必须落在同一个 release**（同一版本号、单一 tag）。
   任何拆分发布 = 三选一 brick 生产：
   - 先换内核不摘渲染器 → 住宅单元 crash-loop，配置永不落盘，住宅 HY2 全体断；
   - 先摘渲染器不接计量 → 住宅超量静默放行（少计）或 fail-closed 误封；
   - 先改版本判据不改其余 → 对账整轮中止 / 升级双通道自锁。
2. **闸门**：在第 4 节全部完成并通过验收前，**不得发版**。
   机制上已确认：`kernels.lock` 只在发版生成 `manifest.json` 时生效，在跑的服务要等升级才拿到新内核；
   但 **CI 那道抓 skip 的 grep 已被删除**（见 §4 step 6），所以「CI 会拦住」这条兜底**当前不成立**，
   不能再依赖它，只能靠纪律。

---

## 1. 「一直更新最新版内核」的设计

主理人要求内核可持续跟到最新版。这**不是**只改一个轨道字符串就能成立的，它需要四件东西同时到位：

| # | 要件 | 现状 | 要做什么 |
|---|---|---|---|
| 1.1 | **浮动版本轨道** | `SINGBOX_TRACK="pin:v1.14.3"`（钉死） | 改成浮动轨道。`pin-kernels.sh` 已支持 `major:<x>` / `minor:<x.y>`；见 §1.1 的取舍 |
| 1.2 | **自动顶版** | `kernels-bump.yml` 每周一 03:00 跑 | 保留；确认它在浮动轨道下会真的提 PR 顶 lock |
| 1.3 | **兼容性闸门** | CI 的 `test (sing-box <ver>)` job 会用 lock 里的内核跑 `kernel_*` 用例 | **这是「跟到最新」的安全网**：新内核若加载不了我们渲染的配置，CI 红，改动进不了发布。要确认它对**四份**产物都有效 |
| 1.4 | **代码版本无关** | ❌ **当前在 5 处用版本字面量等值**（§4 step 9） | 全部改成能力/sha 判据。**这是「一直更新」能否成立的真正瓶颈**——不修，每次内核往前走都会自锁一轮 |

### 1.1 轨道：`minor:1.14`（主理人 2026-10-10 裁决）

```diff
- SINGBOX_TRACK="pin:v1.14.3"
+ SINGBOX_TRACK="minor:1.14"
```

语义：永远吃到 1.14.x 的最新补丁（1.14.4 / 1.14.5 …），**但不自动跨 minor**。
跨 minor（如 1.15）那一步由一次显式评审决定——而那次评审正好由 §1.3 的兼容性闸门来判定。

**待办**：改 `scripts/release/kernel-versions.env` 这一行，并在同一批把 §1.3 的闸门确认到位。
（当前该文件正被 ①②③④ 的实现批次占用，改动排在它之后。）

> 未选的选项与理由留档：`major:1` 最贴合字面，但 sing-box 各 minor 之间有破坏性配置变更，
> 1.15 若改 schema，渲染器必须跟着改——而这依赖 §1.4（代码版本无关）先完成。
> 等 §1.4 落地、§1.3 的住宅配置闸门补上之后，可再议放宽到 `major:1`。

### 1.2 兼容性闸门要覆盖的四份产物

当前 CI 的内核通用例覆盖 `kernel_client` / `kernel_relay` / `kernel_relay_policy` / `kernel_traffic` /
`kernel_xray` / `kernel_nft` / `relay_generation_kernel`。切官方后**新增必需的一条**：
住宅配置（`hy2-residential.json`）必须在**当次 lock 里的内核**上 `sing-box check` 通过——
见 §4 step 6，它是被删掉的 skip 门留下的覆盖空洞。

---

## 2. G0：官方控制面的计量能力（**待回填**）

G0 的三项前置事实已实测：

- 官方 sing-box 1.14.3 的服务形状是 `services: [{"type":"api","listen":"127.0.0.1","listen_port":P}]`，
  **支持 gRPC 反射**（`grpcurl -plaintext 127.0.0.1:P list` 即可驱动全套 API，无需 proto 文件）。
- `daemon.StartedService` 提供 `SubscribeConnections(SubscribeConnectionsRequest) returns (stream ConnectionEvents)`
  与 `SubscribeStatus`、`GetStartedAt`。事件形状：
  ```
  ConnectionEvent { type(NEW/UPDATE/CLOSED), id, connection, uplinkDelta, downlinkDelta, closedAt }
  Connection { id, inbound, inboundType, ipVersion, network, source, destination, domain, protocol,
               user, fromOutbound, createdAt, closedAt, uplink, downlink, uplinkTotal, downlinkTotal,
               rule, outbound, outboundType, chainList, processInfo }
  ```
  **`uplinkDelta`/`downlinkDelta` 是增量**，逐事件求和即账本，无需轮询。
- **已知缺口（本会话实测）**：存活期短于一个采样 tick 的连接，`closedAt == createdAt`，
  NEW 与 CLOSED **都不含任何字节字段**，delta 为 0 ⇒ **这些字节永久不可归户**。
  而轮询 `/connections` 连连接本身都看不见（3 条 <1ms 连接，`connections=0`）。

> 五维实测（hy2 逐用户归户 / 亚 tick 缺口量化 / 队列与历史上限 / CLOSED 尾部语义 / UDP+IPv6）
> 正在跑；交接文档给的待验约束：关闭历史上限 1000、入口队列 256、每订阅队列 64（满则静默丢）、
> CLOSED 在 underlying Close 之前发出、初始快照可能含已关闭记录（不得计在线）、总量与归户非原子快照。
> **结论回填于本节。**

---

## 3. 独立计量模块设计（**待回填**）

设计必须遵守的已知硬约束（G0 前即成立）：

- 采集面是 **server-streaming 订阅**，不是轮询；轮询会丢短连接（已实测）。
- 必须处理 **亚 tick 缺口**：要么有别的计数面兜底，要么把「不可归户的残量」显式化并进入 §5 的拒绝语义。
- 必须能**检测缺口**并从流本身给出信号（队列满时静默丢事件是已知风险）。
- **计量不可用时不得静默**：现 `traffic.rs:240-248` 的 Err 分支只 `s.errors.push(...)`，
  无 tracing、无哨兵、不返回 Err，而唯一能读到它的 `/api/users/health` **前端从不请求**
  ⇒ 换内核后每轮都走该分支，**系统表现与健康时一模一样**。这一条必须在新模块里修掉。

---

## 4. 最小改动集（来自只读影响面审计，基于冻结 `654415d`）

按依赖顺序。标 ✅ 的已完成。

| # | 步骤 | 状态 |
|---|---|---|
| 1 | 发布侧换官方：`kernels.lock` 两行改官方 1.14.3 tar.gz + 官方 sha；`kernel-versions.env` 轨道改 1.14.3、`SINGBOX_TAGS` 删；`build-singbox.sh` 与 `test-build-singbox.sh` 删；CI job 名与三处 skip-grep 自建支改 | ✅ 进行中（本轮 ①②③④） |
| 2 | **裁决计量替代来源与迁移期语义**（fail-open 少计 / fail-closed 拒连），据此拍板 `query_user_deltas`、`10086`、`HY2_RESI_V2RAY_API`、`proto-v2ray` 的去留 | ⏳ 等 G0 |
| 3 | 可见性：`serve.rs` 对账失败写 `RuntimeData.last_reconcile_error` 并映射 `/api/health` 与 `bui status` 的 degraded（**当前核实仍缺**——迁移期任何失败都是「机器不报错但状态是错的」） | ⬜ |
| 4 | **原子核心**：删 `render/hy2_singbox.rs:185-191` 整个 `experimental.v2ray_api` 键（保留 `clash_api`；**必须删键，只删 stats 子对象无效**） | ⬜ 与内核换装同一发布 |
| 5 | 断言同步（同提交）：`hy2_singbox.rs:362-368` 删三条+补反向守卫；`core_files.rs:1156-1160` 同步；`hysteria.rs:283-286` 端口表登记新口（不留 10086 幽灵） | ⬜ |
| 6 | **测试改真跑**：`kernel_hy2_singbox.rs` 删 skip 门，让两用例对不含 `v2ray_api` 的产物跑官方 `sing-box check`（**CI 抓 skip 的 grep 已删，此用例现在是静默——整个切换期住宅配置形状无人验**） | ⬜ blocker |
| 7 | 计量路径收口：`panel/hy2resi.rs:275,357` 删除或改接；`traffic.rs:240-248` Err 分支必须可见或整体删除；面板/文档承诺同步 | ⬜ 等 §3 |
| 8 | 自检改写：`selfcheck.rs` 拆成「计量面／门位·在线面」两条判据，去掉 10086 白等 120s 与必然 FAIL | ⬜ |
| 9 | **版本钉子改能力/sha 绑定**（§1.4 的核心）：`serve.rs:272-273,288-293`（否则整轮对账中止）、`upgrade.rs:475-477`（否则升级双通道自锁）、`install.rs:415-417,1107-1113`（否则新装中止）、`gates.rs:347`（否则门停 deny 住全体住宅用户）；**`upgrade.rs:269-272` rollback 守卫本版即断，已裁决改受支持集合** | ⬜ blocker |
| 10 | 夹具与守卫：`relay_generation_kernel.rs`/`generation.py`/`policy.py` 版本串；`install/upgrade/serve` 夹具 1.14.2 字面量；stock 三处换官方真实 sha；`fakes.rs` 补守卫测试 | 🔶 部分（stock sha 已换 `299f9f60…`） |
| 11 | 清扫：`serve.rs` 的 `bin/.prepared-<uuid>` 失败与成功路径统一清理（否则每 tick 残留 ~81MB 且不被体检暴露） | ⬜ |
| 12 | 真机兜底：官方 1.14.3 + 生产形状配置实测 `auth_user=<name> => gate` 路由串（含多用户 `[a b]` → None 分支）；演练 `/api/health` 对计量失败的 degraded 表现 | ⬜ |
| 13 | **发布纪律**：2–12 同一 release，不许拆包 | ⬜ |
| 14 | **原子性自检（机制化闸门）**：加一条 `scripts/tests/` 用例——**若 `kernels.lock` 里 sing-box 是官方归档（即产物不含 `with_v2ray_api`），则渲染出的 `hy2-residential.json` 必须不含 `experimental.v2ray_api` 键**；反之亦然。不一致即判红。 | ⬜ **新增，见下** |

### 4.1 为什么第 14 步是必需的（本日暴露的洞）

审计核实：`.github/workflows/ci.yml` 里原先那道
`grep -Eq 'skipped: (…|自建 sing-box（with_v2ray_api）不可用)'` 随迁移被删除，
而 `crates/bui-schema/tests/kernel_hy2_singbox.rs` 的 skip 门**还在**（拆它属第 6 步，与第 4 步同批）。

后果：**当前这棵「锁已换官方 + 渲染器仍产出 `v2ray_api`」的树，CI 会跑成绿的**——
因为住宅用例静默 skip、其余用例的配置（relay/client/xray）对官方内核都安全。
于是「不许发版」这条**只剩纪律，没有机制**。而 CI 绿恰恰会让人放心地发版。

第 14 步把这条不一致变成**一条会红的用例**：它不依赖任何人的记性，
只要「锁说官方、渲染器却还吐 v2ray_api」就当场判红。拆掉第 6 步的 skip 门之后，
它退化为第 6 步的自然结论（住宅配置真跑 `sing-box check`），**不会变成多余的重复守卫**。

> **落地位置：分两处，不要放进 `run-all.sh`。**
> 判据本身很简单（读 `scripts/release/kernels.lock` 判 sing-box 行是不是 `https://github.com/` 归档，
> 再对渲染产物断言 `experimental.v2ray_api` 键的有无；不需要内核、不需要网络，纯文本与 JSON 判定）。
> 但**放哪**很关键：
>
> | 落点 | 形态 | 理由 |
> |---|---|---|
> | `.github/workflows/ci.yml` 的 lint job | **只告警，不卡 CI** | 与既有 `check-release-gate.sh --warn` 同款：迁移期本就不一致，让 CI 长期常红会训练人忽略红色 |
> | `.github/workflows/release.yml` | **硬闸门，判红即中止** | 风险真正落地的地方就是发布；这一道必须硬 |
> | `scripts/ops/` 下留同名脚本 | 手动/演练可跑 | 给真机兜底（第 12 步）用 |
>
> 这样：迁移期 CI 保持有意义（只多一行告警），而**发布路径物理上无法在不一致状态下出包**。

---

## 5. 计量不可用时的拒绝语义（与 §3 配套）

现存的静默失效链（审计核实）：

- `traffic.rs:292-306,331`：住宅字节永不进 pending ⇒ `usage.total_bytes`/`monthly_bytes` 永不增长、
  只走住宅的用户 `last_seen` 停更、30s 落盘与退款逻辑对住宅永不触发（**空转不报错**）。
- `users.rs:401,431`：配额判据 `usage+extra >= limit` 缺住宅那半 ⇒ **超量用户永不返回
  `TotalExceeded`/`MonthlyExceeded`**，全树无缺口检测或补偿。
- `gates.rs:73,95,395` + `traffic.rs:109,137`：门位/踢人本身不依赖 `v2ray_api`
  （Clash selector 在官方构建里都在，`auth_user` 串已实测可通过 check），
  但 `blocked_set` 输入缺住宅 ⇒ **住宅超量/封禁的「明确拒绝点」永不落地**。

⇒ 新模块必须显式选择并实现：**计量不可用时**要么保守拒绝（fail-closed），要么放行但把
「住宅计量不可用」推向可见出口 + 告警链路。**不允许继续静默放行。**

---

## 6. 不在本计划范围（另立）

- bui-c 生命周期/owner/guard 线（隔离监督器、warm、Start/Restart、units/hooks、双栈物理矩阵）。
  入口见 `docs/superpowers/plans/2026-10-08-bui-c-recovery-guard.md` 与交接材料里的
  `next-actual-entrypoint-map.md`。**注意**：当前持锁同步 `systemctl` 的旧路径与 locking hooks
  存在死锁（job 等 hook、hook 等锁），**不得先装部分 hooks**。
- 客户端完整受管配置交付（平台逐项验收）。
