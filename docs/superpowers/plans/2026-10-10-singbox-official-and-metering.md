# 官方 sing-box 发行物 + 独立计量模块：实施计划

记录日期：2026-10-10。主理人本轮裁决：**不再自建 sing-box，改用官方发行物；住宅按用户计量由独立模块承担**；
并追加要求 **「支持一直更新最新版的 sing-box 核心」**。

> 状态：**第 2、3 节已由 G0 实测回填**（`e9b6133`）。第 4 节第 1/4/5/6/7/9/11/14/15/16 步已落地；
> 第 3 步见 §4.3。**发布前的未决项见 §8.1 与 §7.3**（真机兜底、发布纪律、UDP 计费口径）。

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

## 2. G0：官方控制面的计量能力

### 2.1 接口事实（已实测，可直接依赖）

- 服务形状 `services: [{"type":"api","listen":"127.0.0.1","listen_port":P}]`，**支持 gRPC 反射**
  （`grpcurl -plaintext 127.0.0.1:P list` 即可驱动全套 API，**无需 proto 文件**）。它是 gRPC 不是 REST——
  对裸 HTTP 路径全部 404。
- 事件形状：
  ```
  ConnectionEvent { type(NEW=0/UPDATE=1/CLOSED=2), id, connection, uplinkDelta, downlinkDelta, closedAt }
  Connection { …, user, ipVersion, network, …, uplink, downlink, uplinkTotal, downlinkTotal, … }
  SubscribeConnectionsRequest { int64 interval = 1; }   // 纳秒；0 = 默认 1 秒
  ```
  ⚠️ proto3 零值在 JSON 里被省略——**`type` 字段缺失就是 `NEW`**，别解析成 NULL。

### 2.2 五维实测结论

| 维度 | 结论 | 关键数字 |
|---|---|---|
| hy2 逐用户归户 | **成立** | `user` = 服务端 `users[].name` 标签（**TCP 与 UDP 都给**）；`api connection show` 有显式 `User:` 字段；用 B-UI 的 `{name}:{secret}` 凭据形状复测仍报服务端 name |
| 关闭历史读路径 | **存在但脆弱** | `SubscribeConnections` 实时推出 CLOSED（12ms 短连接也能抓到）；**但初始快照是 Go GC 易失缓存**——实测 10 条新关闭 → 快照 11 → 4 秒后 GC → **0** |
| 轮询快照当账本 | **不可用** | 1–12ms 连接在 0.1s 轮询下字节捕获率 **1.081% / 2.744%**，≥0.5s 基本为 0 |
| nft 按端口计数 | **逐字节精确** | 1KiB–1MiB、1000+2000 条短连接、5000 条 UDP 全部与内核计数器**误差 0**；匿名 per-rule 计数器**不可 reset**，表重建即清零 |
| clash_api `/connections` | **无 user 字段** | 4/4 条 metadata 里 0 命中；连接一关即清 |
| 官方二进制 | **永远不含 `with_v2ray_api`** | 17 个 tag 抽样 0 次；1.14.2/1.14.3 对含该段的配置均 exit=1 |

### 2.3 🔴 两条必须记住的硬事实

1. **全局 `downloadTotal`/`uploadTotal` 逐字节精确**（14/14 次与客户端实测字节完全相等，**与轮询间隔无关**）。
   ⇒ 它不能归户，但可以当**独立总账**，用来给逐用户账本做**缺口检测**。
2. **nft 只能当总账交叉校验，不能做逐用户归户。** 住宅 HY2 只有**一个**监听口 `:40000`
   （整段由 `table inet bui` REDIRECT 进来），用户靠**凭据门**区分、多用户共享一个槽/IP，
   **不存在每用户端口、也不存在每用户 cgroup**。「每用户一条本地端口 + nft 计数器」与冻结架构相反。

### 2.4 ✅ 缺口形状已钉死（决定性实验结论；重启截断与阈值口径亦已钉死，仅 UDP/IPv6 **计费**口径仍挂账，见 §5.3/§7.3）

三处矛盾已由常开流的逐字节对账裁定（642 条连接 / 3 个会话 / 逐连接 1:1 配对）：

> **`CLOSED` 事件的 `connection.downlinkTotal`/`uplinkTotal` 是精确账本；`Σdelta` 不是。**
> 「亚 tick 连接永久不可归户」的说法**是错的**（早期最小冒烟测试的伪影）——
> 亚 tick 连接没有 *delta*，但它的 CLOSED **带全量总量**。

| 口径 | 实测 |
|---|---|
| **CLOSED 累计总量**（按 `connection.user` 归户） | **差额全部为 0**（S2 alice 1,216,159=1,216,159 / bob 487,159=487,159；S3 400 条 Σdl=3,316,800 精确） |
| `Σdelta` | ❌ S1 alice −13.9% / bob −6.8%；**上行 −99.93%**；S2 **−100%**；长连接 12.6% 只在 CLOSED 里 |
| 轮询 `/connections` | ❌ 1–12ms 连接捕获率 1.081% / 2.744% |

> ⚠️ **本表与 §3.3 表的数字来自两次不同实验、不要混读。**
> 本表是**第一轮会话**（S1/S2/S3，tick 与流量形状不统一，只用于定性「哪种口径可用」）；
> §3.3 是**决定性实验**（run1/run2，四寿命档 × 两档 tick，逐连接 1:1 对账），那才是可引用的量级。
> 两轮的定性结论一致：**CLOSED 终值精确，Σdelta 有确定性缺口，轮询不可用**。

**`Σdelta` 为什么不可用**：`UPDATE` 事件**只有** `{id, uplinkDelta, downlinkDelta}`——没有 `user`、
没有总量、没有时间戳（29 条 UPDATE 里带 `user` 的 **0 条**）；且**亚 tick 连接根本不发 UPDATE**。

**另外三条必须写进实现的语义**：

1. **断开竞态**：CLOSED 总量 = **客户端实际收到的字节**（客户端落地文件公证，精确相等）；
   与源站已写字节差**恰好一个 64 KiB chunk**（客户端死后源站又写的那块，从未送达）。
   ⇒ **按「送达计费」CLOSED 是对的**；拿源站计数器比会把它误判成丢失。
2. **`reset=true` 首帧是陷阱**：它带订阅**之前**就存在连接及其累计量（连 hysteria 客户端自己的
   `api.hy2.io:443` 探针流量都在里面）⇒ **新计量器不按 id 去重就会重报一遍**。
3. **归户副作用**：hysteria 客户端的版本探针流量**真实过代理并归到用户头上**，生产要排除或计入基准。

**仍未证**（决定性实验的其余三维正在跑）：64/256 队列阈值下的丢事件（实测只在 400 并发 / ~800 事件
下零丢失，阈值没被压到）、≥30min 长稳、UDP/IPv6 口径、SIGHUP/重启的重复计。

### 2.5 一条迁移地雷（与计量无关但同批要处理）

官方 hysteria 客户端 `auth: alice:pass-alice` **会 404**，只有 `auth: pass-alice` 成功 ⇒
服务端 `users[].password` 必须写**整串** `{name}:{secret}` 才能互通。
B-UI 现有池凭据正是 `{name}:{secret}`，但**渲染器现在写进 `password` 的是哪个字段、订阅下发的又是什么**，
需与 §4 step 4 同批核对——写错则**所有现有客户端的住宅 HY2 全部连不上**。

---

## 3. 独立计量模块设计（依据 §2 实测，非推测）

### 3.1 采集面

常开 **`daemon.StartedService/SubscribeConnections`**（gRPC server-streaming），
对每个住宅 sing-box 实例一条订阅。`interval` **不影响账本正确性**（账本靠 CLOSED 总量，不靠 delta），
所以取默认 1s 即可——省 CPU 与事件带宽，不必为了精度去调小。

### 3.2 账本规则（每条都能追溯到实测数字）

| # | 规则 | 依据 |
|---|---|---|
| 1 | **入账只用 `CLOSED` 事件的 `connection.downlinkTotal`/`uplinkTotal`** | 642 条连接逐连接差额 **0** |
| 2 | **绝不用 `Σdelta`** | 实测丢 6.8%~100%（UPDATE 无 user/无总量；亚 tick 连接无 UPDATE） |
| 3 | **按 `(实例代际, 连接 id)` 去重** | `reset=true` 首帧带订阅前的存量连接及其累计量 |
| 4 | **未关闭连接不计入已结算账本** | 未关闭连接的终值未定 |
| 5 | **归户键 = `connection.user`**（服务端 `users[].name` 标签） | TCP/UDP 都给；不是客户端自报 |
| 6 | 内存只需保存「未关闭连接的 id → user」映射 | 账本在 CLOSED 时一次性入账 |
| 7 | **实例代际用 `GetStartedAt`** 切分（待决定性实验确认其重启后行为） | 重启前后不得双计/漏计 |

### 3.3 缺口检测配方（决定性实验给出，含实测阈值）

**先认清缺口形状**：不是「谁对谁错」，而是**一条权威账 + 三种赤字机制 + 两类伪缺口**。

| 机制 | 成因 | 实测量级 | 检测方式 |
|---|---|---|---|
| **(a) 尾窗赤字** | 正常路径：最后一次 tick→关闭之间到达的字节只进终值、不发 delta | 20ms tick 缺 4.03%；1s tick 缺 18.97% | **不需检测**——用终值结算即自动消除 |
| **(b) 满丢后缀** | 慢消费者：只拿到严格连续前缀，切点 ≈128 KiB gRPC 流控边界，**流内零信号** | 缺口达区间字节 **84–92%**（实测 273,920/1,095,680） | **只能靠流外全局账**；字节下界可得，**条数与身份不可恢复** |
| **(c) 重启截断** | HUP/TERM/-9 必以非 OK gRPC 码断流 | **有界**：250ms tick 丢 0 B、1s tick 丢 384 B | `GetStartedAt` 变化即封账 |
| 伪缺口 ① | 重连/快照重报已关闭连接 | — | **按 id 去重**即零重复零少计 |
| 伪缺口 ② | `conn=nil` 的 CLOSED（实测 14 条、39,168/41,472 字节无处登记） | — | 必须显式处理，否则丢 |

**指标 A（主指标，唯一能发现静默丢流）**

```
R = ΔG − ΔB − ΔS
    ΔG = SubscribeStatus 总量差分（区间首尾采样）
    ΔB = 区间内**已入账**的字节（CLOSED 终值之和）
    ΔS = 在册未关闭连接的 known 之和在该区间的变化
```

> ⚠️ **本式修订过（2026-10-10，由实现批的对抗复审证伪了初版）**：初版写的是
> `Δ_glob = ΔG − Σ(区间内 CLOSED 终值)`，**漏掉了 ΔS**——那会把「区间内仍在传输的连接」
> 的字节整体算成缺口，使健康区间也报巨额缺口，与本节「健康基线实测 = 0」自相矛盾。
> **必须减掉 ΔS 才能让基线落在 0。** 复现方式：任一健康区间代入初版公式，R ≠ 0。

- **健康基线实测 = 0**（快消费者两轮 byte-exact；全局 325,888/1,188,352 == 账本值）
- **故障实测 = 273,920/1,095,680**（占 84–92%）
- **阈值**：>64 KiB 记账观察；**≥1 MB 或 ≥区间字节 5%（先触发者）判「后缀丢失」**
- **超了做什么**：告警；该区间**标损（不折算到人）**；**暂停该区间的自动封禁/计费裁决**；
  重订阅一次（快照可补 14–30s 内的已关闭连接，按 id 去重）；排查消费方读取延迟

**指标 B（世代边界）**：`GetStartedAt` 变化即封账（30 分钟实测恒定；重启序列每次必变）。
把 `[最后一 tick, 流终止]` 标「可疑损失」（预期 ≤384 B @1s tick）。非 OK gRPC 码辅助定位重启类型
（Internal=HUP / Canceled=TERM / Unavailable EOF=-9）。

**指标 C（配对完整性）**：NEW/CLOSED 成对率与重复率。基线：已交付 0 条半对；
重复 CLOSED **幂等吸收**即可（实测同一连接两帧相隔 114µs、字段全同）。

### 3.3.1 消费方速度就是账本上限

**实测**：慢消费者（SIGSTOP）在 ~420–426 事件 ≈128 KiB 未读缓冲处被截断，
**4,492 条连接只余 212 条**；而快消费者两轮均 byte-exact，且 **`interval` 调大调小截断点都一样**（1s 与 10s 相同）。
⇒ 必须**单点快速消费 + 指标 A 兜底**，不能指望「慢点读也没关系」。

### 3.3.2 其余经实测钉死的硬约束

- 禁止 `Σdelta` 与 `Σtotal` **相加**（朴素相加过账 1.96–1.97×）
- **亚 tick 连接必须入账**（终值完整：40/40 sub-ms、60/60 ~1–2ms）——短连接不是「无字节」
- `UPDATE` 必须**按 id** 关联，不得按 `source`/`destination`（UPDATE 不含 `connection` 对象，按五元组关联得 0）
- `reset=true` 首帧 / 重连快照按 id upsert，**快照 totals 绝不累加进账**
- **不得依赖 closed 环/快照做审计或补账**（GC 终结器随机清空，寿命 14–30s；重启后新实例 `snap_entries=0`）
- **UDP 计费口径必须单一定义并写死**：与客户端封装口径差**恒定 10 B/报文/方向（v4）/ 22 B（v6）**，
  按包数线性放大，混用即系统性偏差 ⇒ **这是产品决策，不是测量问题**（见 §5）
- **鉴权串必须整串 `{name}:{secret}` 逐字符等值**，归因用 `users[].name`。
  2×2×2 实测矩阵：整串×整串成功 / 整串×裸 404 / 裸×裸成功 / 裸×整串 404（hysteria 2.12.2 与 sing-box 1.14.3 一致）；
  **v2rayN 把 userinfo 原样透传成 password** ⇒ 服务端写整串即可兼容全部客户端。

### 3.4 必须排除的流量

hysteria 客户端自带的 `api.hy2.io:443` 版本探针**真实过代理并归到用户头上**（实测）。
生产计量要么按目标排除，要么显式计入基准并在账单口径里说明。

### 3.5 实现落点（对应 §4 的 step 7）

- **proto**：需要 vendored 上游 `daemon/*.proto`（对照现有 `crates/bui/proto/`(Xray) 与
  `proto-v2ray/` 的做法，带 `SHA256SUMS`）。
  ⚠️ **新增一条 CI 门**：vendored proto 的 sha 必须与**当次 lock 里那个 sing-box tag** 的上游文件一致——
  否则内核一升级、API 悄悄变了，我们的客户端会静默解析错。这条是「一直更新最新版」的必要配套。
- `modules/panel/hy2resi.rs`：`StatsService.QueryStats` 客户端 → 换成流客户端 + 去重/入账逻辑。
- `modules/panel/traffic.rs`：采样从「每轮 QueryStats」改成「流驱动增量入账」；
  **Err 分支必须可见**（tracing::warn + 哨兵签名 + 推到有人看的出口），不许再只 `s.errors.push(...)`。
- `crates/bui/build.rs` 的第二次 prost/tonic 编译、`proto-v2ray/` 的取舍随调用删除同批处理
  （**注意**：`proto-v2ray/` 与 Xray 的 `proto/` 是两套，别一并误删）。

### 3.6 计量不可用时的语义（与 §5 配套）

现状是**三条静默失效链**（审计核实）：Err 只进 `s.errors`（前端不请求那个端点）、
住宅字节永不进 pending（配额判据缺一半 ⇒ 超量永不触发）、`blocked_set` 缺住宅（明确拒绝点不落地）。
新模块必须显式选择 **fail-open（少计）** 或 **fail-closed（拒绝）**，并把选择的结果推到可见出口。
**不允许继续静默放行。**

---

## 4. 最小改动集（来自只读影响面审计，基于冻结 `654415d`）

按依赖顺序。标 ✅ 的已完成。

| # | 步骤 | 状态 |
|---|---|---|
| 1 | 发布侧换官方：`kernels.lock` 两行改官方 1.14.3 tar.gz + 官方 sha；`kernel-versions.env` 轨道改 1.14.3、`SINGBOX_TAGS` 删；`build-singbox.sh` 与 `test-build-singbox.sh` 删；CI job 名与三处 skip-grep 自建支改 | ✅ **完成**（`51024f5`） |
| 2 | **裁决计量替代来源与迁移期语义**（fail-open 少计 / fail-closed 拒连），据此拍板 `query_user_deltas`、`10086`、`HY2_RESI_V2RAY_API`、`proto-v2ray` 的去留 | ✅ **裁决已定**（§2/§3/§5，全部实测支撑）；**实现待第 7 步** |
| 3 | 可见性：**整轮对账中止**要可见（迁移期任何失败都是「机器不报错但状态是错的」）。做法见 §4.3：薄包装 + 哨兵 `Sig::ReconcileFailed`（**不新增字段**，复用 `last_reconcile`） | ✅ **完成**（`e90ad21`；对抗复审做了两次反向实验证明测试非永绿） |
| 4 | **原子核心**：删 `render/hy2_singbox.rs` 的 `experimental.v2ray_api` 键，换成官方顶层 `services:[{"type":"api",…}]`（`clash_api` 保留；常量改名 `HY2_RESI_API`，端口单一真源） | ✅ **完成**（切换批；门 `EXIT 3 → 0`） |
| 5 | 断言同步（同提交）：`hy2_singbox.rs`（反向守卫 + `services[0]` 正断言）、`core_files.rs`、`hysteria.rs` 端口表标签、常量全部消费点（`lib.rs`/`panel/mod.rs`/`hy2resi.rs`/`selfcheck.rs`/`install.rs`/`stock_gate_fixture.rs`） | ✅ **完成**（切换批） |
| 6 | **测试改真跑**：`kernel_hy2_singbox.rs` 删 skip 门 —— 冷/热两套均 `2 passed / 0 skipped`，并用 PATH shim 证明真调用官方 `/usr/bin/sing-box` | ✅ **完成**（切换批，blocker 解除） |
| 7 | 计量路径收口：`hy2resi.rs` 重写（Clash HTTP + daemon gRPC 双面）、`traffic.rs` Err 分支落双标记 warn、`query_user_deltas` 签名冻结、lossy 贯通 `users.rs`/`gates.rs`/`api_admin.rs`/`auth_http.rs` | ✅ **完成**（`9f9b1fb`） |
| 8 | ~~自检改写：拆成「计量面／门位·在线面」两条判据~~ → **降级为可选（Fable 2026-10-10 裁决）**：本步的两条动机**已随第 4 步消失**——「白等 120s 与必然 FAIL」的成因是官方内核不认 `v2ray_api` ⇒ 10086 永不监听；切换后 10086 由新 `services.api` **真绑**（本机实测），`wait_ready` 天然满足。且对手复审确认 `selfcheck.rs:1557` 的「等待有界 ≤120s」假时钟断言**仍在验一条真性质**，不必重写。剩下的只是「把一行拆成两行以改善诊断」，属外观改进 | ⏸ **可选，不阻塞发布** |
| 9 | **版本钉子改能力/sha 绑定**（§1.4 的核心）：`serve.rs`（否则整轮对账中止）、`upgrade.rs`（否则升级双通道自锁）、`install.rs`（否则新装中止）、`gates.rs`（否则门停 deny 住全体住宅用户）；rollback 守卫从版本白名单改为 `RESIDENTIAL_LAYOUT_SINCE` + sha | ✅ **完成**（`fd55363`） |
| 10 | 夹具与守卫：`relay_generation_kernel.rs`/`generation.py`/`policy.py` 版本串 ✅；`install/upgrade/serve` 夹具 1.14.2 字面量（自洽、不卡 bot）✅；stock sha 已换官方 `299f9f60…` ✅；`stock_gate_fixture.rs:449` 与 `tests/test-residential-stock-gates.sh:30` 的 `1.14.3` 硬字面量**留作升轨维护点**（opt-in 手动夹具） | 🔶 残部见 §8 |
| 11 | 清扫：`serve.rs` 的 `bin/.prepared-<uuid>` 失败与成功路径统一清理（否则每 tick 残留 ~81MB 且不被体检暴露） | ✅ **完成**（`fd55363`） |
| 12 | 真机兜底：官方 1.14.3 + 生产形状配置实测 `auth_user=<name> => gate` 路由串（含多用户 `[a b]` → None 分支）；演练 `/api/health` 对计量失败的 degraded 表现 | 🔶 **前半完成**（路由串已在真机实测，见 §9）；**后半未做**（需跑整套 `bui serve`，动 systemd 单元与 `/opt`，超出实验沙箱的安全边界，留真机环境） |
| 13 | **发布纪律**：2–12 同一 release，不许拆包 | ⬜ |
| 14 | **原子性自检（机制化闸门）**：内核来源与渲染器是否产出 `experimental.v2ray_api` 必须自洽 | ✅ **完成**（`51024f5`；`check-kernel-consistency.sh`，ci.yml 只告警 / release.yml 硬闸门。**真实 CI 里验证过会发告警**） |
| 15 | **lock 检查的 fail-open 修复**：`pin-kernels.sh` 新增 `--warn`——只降级「漂移」，`lock 少行`／`核实不了` 仍旧退 1（原 `\|\| echo ::warning::` 把三类一起吞掉） | ✅ **完成**（`fd55363`；安全审查与对抗复审各自独立指出） |
| 16 | **测试层的版本守卫要读 lock，不能写死**（🔴 **不做则「一直更新最新版」在实践上失效**，见 §4.2） | ✅ **完成**（`ddd928c`）。四个文件改完；**换版证伪实验**（私有 mount namespace bind 换入版本列=1.14.99 的锁，真文件零改动）四守卫全部改口变红；坏锁四类（缺失/空/无行/缺列）全硬失败，无一处降级为 skip。遗留**已消解**：`relay_generation.rs` 的 `1.14.3` 字面量已在切换批（`79753b2`）改为 `include_str!` 编译期读 `kernels.lock`。stock 那族见 §8 #7 |

### 4.2 为什么第 16 步是「一直更新最新版」的最后一道坎

第 9 步清掉的是**生产代码**里的版本字面量。但**测试层**还钉着一批，而它们恰好是**周更 bot 自动顶版的闸门**：

```
crates/bui-schema/tests/fixtures/relay/policy.py:263      assert version == "sing-box version 1.14.3"
crates/bui-schema/tests/fixtures/relay/generation.py:359  同上
crates/bui-schema/tests/relay_generation_kernel.rs:129    Some("sing-box version 1.14.3")
crates/bui/src/modules/panel/stock_gate_fixture.rs:424    version=1.14.3（+ STOCK_SHA256）
scripts/tests/test-residential-stock-gates.sh:19          sing-box version 1.14.3
```

而这些是**硬失败、不是 skip**（设计如此：找到了内核但版本不对就该红）。于是：

> `kernels-bump.yml` 第 101 行用 `cargo test --workspace` 当**自己开 PR 的闸门**（`set -euo pipefail`，
> 测试红 = 步骤失败 = **不开 PR**）⇒ 轨道一浮到 1.14.4，bot 的自测就红 ⇒ **锁永远推不动**。

**修法与第 9 步同源**：守卫要证的是「PATH 上这个内核 == `kernels.lock` 声明的那个」，
不是「它是 1.14.3」。所以**期望值从 lock 读**（单一真源），版本号与 sha 都不写死。

> 注意区分：`test-manifest.sh` / `test-fetch-kernels.sh` 里的 `1.14.3` 是**合成夹具数据**
> （它们喂的是自己造的假 lock），不依赖真 lock，**不属于本条**，不要一并改。

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

### 4.3 第 3 步的设计修订（Fable 2026-10-10 裁决，附只读侦察证据）

**原文**写的是「写 `RuntimeData.last_reconcile_error` 并映射 `/api/health` 与 `bui status` 的 degraded」。
侦察推翻了这条的两个前提：

1. **前端不消费 `/api/health`**（确证：`web/app.js` 的 `api()` 会自拼 `/api` 前缀，全文只有
   `/api/residential/health`；`web/index.html` 对 `degraded` 与 `/api/health` 零命中）。
   于是新字段 = **准确但没人看**，正是本计划自己在计量审计里批过的那种病
   （「唯一能读到它的端点前端从不请求」）。按 §5 立的「不得静默」标准，光加字段**不达标**。
2. **真正静默的是「整轮中止」，不是 apply 逐项失败**。`reconcile_from_ctx`（`serve.rs:203-394`）
   有 **5 条 Err 出口**（`:319` 的 `??`＝Facts/plan/内核下载；`:338` 发布被拒；`:351` `reconcile_once`；
   `:371`/`:375` 两条回执缺失），**没有一条能走到 `:387`**（唯一的 runtime 写入点）。
   ⇒ `last_reconcile` 停在**上一次成功的、几小时前的、干净的报告**上，`/api/health` 照报 `ok`。
   （apply 内部的 `out.errors`/`verify_failures` 本来就进报告也参与 degraded，**不是盲区**。）

> 🔁 **本节 2026-10-10 晚被三轮独立对抗验证 + 一次综合裁决推翻过一次，下面是修正后的版本。**
> 初版有三处错：① 说 `reconcile_from_ctx` 有「5 条 Err 出口」——实为 **≥7 条**（`residential_lifecycle.rs` 另有 2）；
> ② 说那两行 `tracing::error` 「本来就在 journald 里，只差签名」——对**启动轮**只在同秒竞态下成立、对 **Ok-带错误类**完全不成立；
> ③ 把自触发描述成「死循环、永不停止」——**机制上不成立**（见硬约束 2，必做的理由不是这个）。
> 照初版实现仍会得到一个**永不命中**的 matcher 与一个**被饿死的计量告警**。

**修订后的做法（两件，缺一不可）：**

| 件 | 落点 | 为什么 |
|---|---|---|
| **① 包装 `reconcile_from_ctx`** | `serve.rs:203`。**实现方式是「原函数原地改名 `reconcile_from_ctx_inner` + 在它上方加 ~12 行薄包装」**，不是把 190 行整体缩进重排（那是最容易在这个文件上制造 review 噪声与冲突的做法，零收益；文档注释留在新的 pub 包装上） | 一处覆盖**全部 ≥7 条** Err 出口与全部 4 个生产调用点（`serve.rs:745` 启动轮、`:775` 唯一 mpsc consumer、`:902` `reconcile_offline`（`bui reconcile` / 菜单 / `upgrade.rs:455`）、`install.rs:1224` 装机）。**不新增字段**：Err（`!dry_run`）时把一份只填 `at` + `errors` + 一条 `notes` 的合成 `ReconcileReport`（`..Default::default()`）写进 `last_reconcile`——`api/health.rs:64-69` 的 degraded 第三项**自动点亮**，`status.rs:388-393` 同源判据同步点亮，`format_status:92-105` 自动打出错误行，`HealthResponse` 形状（三处注释锁死的回归面）**一字不改** |
| **② 哨兵 `Sig::ReconcileFailed`** | `signature.rs`（`Sig` 枚举+文档、`ALL_SIGS` 13→14〔`[Sig; 13]` 是编译期约束〕、`id()`、`action()`=`Alert`、`rule()`、`classify` 的 `b-ui` 分支 `:249` 追加 `.or_else(reconcile_failed)`）、`run.rs:342-370` dispatch 加一条（穷尽 match，编译器兜底）、`system.rs` 纯处理函数 | 让**整轮中止**主动出现在有人看的地方：`bui incidents` + 面板系统状态卡（`web/app.js:1485` 拉 `/incidents?limit=20`）。`classify` 用源码子串 **`对账失败`**（同时覆盖 `:755` 与 `:777`）+ `unit == "b-ui"` |

**硬约束（对抗验证逐条给出，实现前必须落死）：**

1. **matcher 与 producer 共用同一个常量**，常量**取自 `serve.rs:755`/`:777` 的源码原文**——`"启动对账失败：{e}"` 与 `"对账失败：{e}"`，**没有方括号**。
   ⚠️ 计划初版写成 `"[启动]对账失败：{e}"`，**逐字照抄 = matcher 永不命中，而照抄写的测试会一起绿**。形状照 `users::USER_SYNC_FAILED_LOG`（`users.rs:26-29`）。
2. **handler 的 result 文案禁止包含该常量**，并**补一条同款断言测试**。理由要说准（初版说成「死循环、永不停止」，**综合裁决复核后判定机制上不成立**）：
   `run.rs:320/323` 会把事件文案以 `"哨兵事件：{result}"` 写回 b-ui 自己的 journald，而哨兵读的 `units()` 含 `b-ui`。但回响行在事件落盘后 ~2 秒就被读走，
   而同签名去抖 60 秒（`sentinel/mod.rs:23`，`engine.rs` 的 observe 在 `now-fired<60` 时丢弃）、命中窗口也是 60 秒且过期即 `pop_front`，冷却又把事件率钉死在 ≤1/10 分钟
   ⇒ **链条会自灭，不会无限循环**。**必做的真实理由是**：(a) 它**违反成文不变量**——`system.rs:87`（xray 文案不许含 `USER_SYNC_FAILED_LOG`）、
   `system.rs:161-163` + 测试 `system.rs:411-416`（计量文案不许同带双标记）；(b) 在 `threshold≥2` 的签名上它是**拿自己的回响给自己的门槛凑数**。
   另注：`sys/mod.rs:293-297` 会把 tracing 的 `error=` 字段追加进 MESSAGE，**所以带错误原文的那条行也在匹配面内**——这正是「不把错误原文塞回 result」的原因。
   可选加固：`classify`（或 tick 匹配处）跳过 MESSAGE 以 `哨兵事件：` 开头的记录（`run.rs:320/323` 是唯一写者）。
3. **用独立 subject（建议 `reconcile`），不要用 `b-ui`**——`engine.rs:59-61` 的 `action_key = "<action.id()>|<subject>"` **不含签名**，
   而 `alert|b-ui` 已被 `Hy2ResiMeteringLost` 占用（`signature.rs:124-127` → `Alert`，`system.rs:181` subject `"b-ui"`）。
   两者在 600 秒内相遇时**后者连事件都不记**（`run.rs:258-262` 只留一行 debug），能把计量告警饿死——而它文档里明写「必须有人看见，不许静默」。
4. **`Rule { threshold: 1, window_secs: 60 }`**——tick 节律是 `DRIFT_INTERVAL_SECS=600`，抄 relay 那族的 `2/60` **永不达标**（60 秒外的命中会被逐条弹出）。
   同族先例见 `signature.rs:152-160`（`KernelBindInUse`/`CaddyCertFailed`）。**window 也不许 >150**：`longest_window_secs` 从 `ALL_SIGS` 推导（`run.rs:27-33`，现为 150），抬高它会改变 `drop_stale_start`（`run.rs:37-58`）的游标丢弃阈值。
   ⚠️ 攻击者曾提议用 `threshold≥2 / window_secs≥1500` 来压事件量，**综合裁决否掉**：1500 会让 `longest_window_secs` 从 150 跳到 1500，把**所有**签名的回放窗口一起放大；而它要解决的「事件量」本来就由 600 秒动作冷却兜着（见下面的「事件节律」条）。
5. **wrapper 必须把 Err 原样重抛**（不包装、不吞）。下游全依赖 Err 传播：`serve.rs:755`/`:777` 两个 `tracing::error!`、`reconcile_offline` 的 `?`（`:902` 后打印返回值）、`install.rs:1223-1236` 的 `?` + 依 `report.errors` bail，
   以及两条硬断言 `serve.rs:1730`（`expect_err`）与 `:2321`（`unwrap_err`）——**吞掉 Err 会让它们 panic**。落笔时别只写「写报告」，要写「写完 `return result`」。
6. **只写 `last_reconcile` 一项。** `serve.rs:387` 一次写 `restart_keys` / `drift` / `last_reconcile` 三个字段；失败路径若照抄整行会把真实漂移与重启键擦成空
   （而漂移非空**本身也是独立 degraded 判据**，`api/health.rs:60-66` 与 `status.rs:388-393` 都读顶层 `rt.drift`，不读 `report.drift`）。
7. **`errors` 落盘前必须脱敏。** `logging.rs` 只是 subscriber 初始化，**没有脱敏层**，脱敏全靠调用点。同一函数里正反例并存：`:363` 对 relay 确认错误用了 `redact::url_credentials`，`:379` 的 `residential activation: {error}` 是裸写。
   合成报告建议统一走 `crate::redact::line(&format!("{e:#}"))`（带错误链更可诊断），**顺手把 `:379` 那处也统一到同一口径**。否则错误链里的 `scheme://user:pass@…` 会经 `runtime.json`（0600）、`/api/health`、`bui status`、`POST /api/reconcile` 回包与 incident 回读。
8. **合成报告加一条 `notes`**，例如「本轮在中止前可能已应用部分改动，changed/restarted 清单不可信」——
   `:371`/`:375` 两条出口发生在 `reconcile_once` 已经落盘并重启过单元**之后**，报告里 `changed`/`restarted`/`notes` 都是真实值却被 `?` 整份丢弃；
   不写这条，`status.rs:91-97` 会打出「变更 **0** 项」而事实相反（**不是误判**——errors 行照常打印、degraded 照常点亮——但会让人低估已发生的改动）。
9. **`!dry_run` 守卫**：`reconcile_offline --dry-run`（`serve.rs:859-869`）与各测试都传 `dry_run=true`；dry-run 成败都不写 runtime。
   守护进程永远传 `false`（`api/system.rs:25` 忽略请求里的 dry_run），所以这个守卫只影响离线 CLI。
10. **`residential_lifecycle.rs:367` 的关闭场景**：`ensure!(!jobs.closing, "residential owner is shutting down")` 会让 wrapper 在守护进程 drain 时写一份合成失败报告。噪声可接受，但**文案要能看懂**（别让它读起来像生产事故）。

**覆盖边界（写文档时不许含糊）：**

- ① 覆盖 **≥7 条** Err 出口：`serve.rs:319`（`??`，内含 `:220`/`:230`/`:285`/`:291` 等内层出口）、`:329`（候选 sing-box 存在时的 `Facts::probe`）、`:338`（发布被拒）、`:351`（`reconcile_once` 失败）、`:371`（证书待定回执缺失）、`:374`（`maintenance`）、`:375`（维护回执缺失）；`residential_lifecycle.rs` 另有 2 条（`:367` 关闭中、`:381-382` owner 无结果）。最现实的一类是 `:220`/`:230`/`:285` 的 preflight——**GitHub 或镜像不可达时每 10 分钟必失败**。
  另注：`:371` 只在回执**整体缺失**时才 Err（`residential_lifecycle.rs:749-752` 会先 persist `awaiting_certificate`）⇒ 正常首装等证书是 **Ok + notes**，不会被 ① 误记成错误。
- **启动那一轮（`serve.rs:755`）只是同秒竞态，不是「一定看不到」**（初版说反了）：哨兵模块 `:762` 才 spawn，晚于启动对账（`:745-757`）；哨兵首 tick 用 `--since @unix秒`（`sys/mod.rs:179-197`）⇒ **与启动错误行落在同一秒才读得到，常态同秒即可见，跨秒才漏**；后续轮与 10 分钟巡检走游标，可靠。
  所以口径写成「启动轮**可能**漏首发，后续轮会补」，**不写成保证、也不为此加通道**。
- **不许宣称「再无静默对账失败」。** 以下都不在 ① ② 覆盖内：
  - **Ok-带错误类（apply 逐项失败）本次不做成事件**（综合裁决：另开一条，别搭车）。它今天在 `bui status` 与 `/api/health` 上**可见**（`reconcile_once` 把 `ApplyOutcome.errors`/`verify_failures` 拷进报告后返回 **Ok**，`serve.rs:142-163`），
    只是**面板不读 `/api/health`** ⇒ 真正的缺口在「面板不消费 health」这个 UI 问题上，**要覆盖得先改 UI 口径，而不是在这里加第二个发射点**。
  - `apply.rs:517-543`：`let _ = host.run("ufw"/"firewall-cmd", …)` 之后**无条件** `out.changed.push(describe(c))` 与 `out.keys.insert("firewall", k)`
    ⇒ **端口放行失败被记账成成功**，diff 里 firewall key 已提交、不再重发 `OpenPorts`，**永不重试、不进 errors/verify_failures**。**独立缺陷，单独提。**
  - `apply.rs:473-505`、`:633` 的 `let _ = host.systemd(…)` / `let _ = host.systemd_daemon_reload()` 静默。
  - `drift::scan` 是不可失败的 `Vec`（host 查询错误 `unwrap_or_default` 吞掉），也不报。
- **事件节律是有意为之、要写进注释**：`DRIFT_INTERVAL_SECS=600` 与 `ACTION_COOLDOWN_SECS=600` 相等 ⇒ 稳定态的持续故障约 **20 分钟一条**同签名 Alert（约 72 条/天），
  而 `INCIDENTS_MAX=200` ⇒ 单一故障约 **2.8 天占满事件环**；面板卡读 `limit=20` 只覆盖约 6.7 小时。这与既有 Alert 级签名（`Hy2AuthHttpFailed`/`CaddyCertFailed`）**行为一致**，是平台语义不是新缺陷——
  但要在 `signature.rs` 的注释里写明「持续故障每冷却周期提醒一次」是**选择**。

**已核过的兼容性**：`last_reconcile` 的活消费者确为 4 类（`api/health.rs:67` degraded ✅、`api/health.rs:80` / `status.rs:411` 响应体 ✅、`api/system.rs:28` `POST /api/reconcile` 回包 ✅、`status.rs:391` degraded ✅），
`web/*`、`.github/*`、`modules/panel/*`（含 `residential::state::health_summary`）**零读取**；`is_clean()` 只在测试里。
合成报告在每处都**更真**（`api/system.rs:28` 现在会回一份失败报告，比今天的陈旧成功报告准确）。
⚠️ 补一处初版漏掉的消费方：**`scripts/m1-acceptance.sh:30-50`** 读 `bui status --json` 的 `.reconcile.errors`/`.verify_failures`——
合成报告让它从「停留在几小时前的干净报告 ⇒ 假通过」变成「真失败」，**方向正确，验收时不要当回归**（其 stub 形状 `:866-869` 与新形状逐字段一致）。
另：`:387` 与 `:393` 之间若发生 panic（如 `c.bus.send(Event::RelayRestarted)`），wrapper 会用合成报告覆盖本轮刚落的真实报告——**影响面小（本轮确实失败且仍 degraded），接受并在注释里写明这是有意的**。

**顺带纠正第 16 步的遗留条目**：`relay_generation.rs:527` 的 `"1.14.3"` 字面量**不在活的生产路径上**——
`relay_generation` 只被 `render/relay/generation.rs` 引用，而后者自己在文件头写明「当前生产 renderer 尚未调用本模块」。
所以它不阻塞周更 bot，也**不紧急**；但一旦 generation 式 relay 启用（内核已经是 1.14.4 时）它会让 lease 证据校验
`EvidenceMismatch` ⇒ **并进第 4/5 批顺手绑 lock**，不单独开批。（stock 那族同理：sha 已换官方；`stock_gate_fixture.rs:426`
的 `version=1.14.3` 只是 opt-in 手动夹具的一行打印。）

### 4.4 新 API 面的三段决定性实测（Fable 2026-10-10，本机官方 1.14.3）

派第 4/5 步之前先把形状钉死，避免子代理在「`type` 到底写什么」上猜：

| 试验 | 命令 | 结果 |
|---|---|---|
| 形状正确性 | `sing-box check -c` 含 `services:[{"type":"api","listen":"127.0.0.1","listen_port":10086}]` | **rc=0** ✅ |
| `type` 字面量 | 同上但 `"type":"daemon"`（proto 包名是 `daemon`，**类型串不是**） | `FATAL … unknown inbound type: daemon` ❌ |
| 反向守卫有意义 | 把 `services` 改成未知顶层键 | `FATAL … unknown field "services_typo"` ✅ ⇒ 官方内核**不认** `experimental.v2ray_api`，第 6 步的豁免门一旦拆掉就是硬红，不是形式主义 |
| 运行时真的监听 | `sing-box run -c`（回环口，按 PID 清理） | 日志 `service/api[0]: tcp server started at 127.0.0.1:10086`；`ss` 确认 LISTEN ✅ |

**两条推论**（改变了第 8 步的规模）：

1. **保留 10086 的裁决有了实测支撑**——新 `services` 段确实会绑这个口。
2. 切换之后 `wait_ready`（`selfcheck.rs:187`）要求的 10086 **照样在听** ⇒ 原本「官方内核不认 v2ray_api ⇒ 10086 永不监听 ⇒ 白等 120s 再 FAIL」那条路**自行消失**。
   第 8 步因此从「拆判据 + 消白等 + 改断言」缩成「拆两行 + 改文案 + 重写 `:1555` 那条已失效的『最多睡 120s』断言」。

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

⇒ 新模块必须显式选择并实现。**决定性实验已给出裁定：分层。**

### 5.1 裁定：用户层 fail-open，系统层 fail-closed

| 层 | 语义 |
|---|---|
| **面向用户的计费/封禁** | **fail-open** —— 只按**确证字节**计费，**不因账目不确定封人** |
| **面向系统的账本** | **fail-closed** —— 指标 A 超阈值 → 告警 + 该窗口**标损** + **暂停该窗口的自动封禁/计费裁决** + 人工核账 |

**依据是量级不对称**（这是本条裁定最关键的地方）：

1. 真正的未知只有两种来源，**且全部由供给侧/运维侧造成、与用户行为无关**：
   (b) 满丢后缀、(c) 重启截断。
2. **重启截断每窗 ≤384 B，而重启是常规动作**（升级/重载都会走到）。
   若「未知即封」，**每次升级都会误伤当时在线的住宅用户**。
   后果对比：误封一个无辜住宅用户 = 用户可见的断网 + 投诉 + 退款/信任损失；放行 384 B ≈ 0。
3. (b) 虽可达窗口的 84–92%，但它是**系统故障信号**（消费者停顿数秒），
   且**身份不可恢复**（实测：字节下界可得，条数与身份不可得）。
   任何「把缺口折算到人」的做法**必然错杀**——**把系统故障转嫁给用户是最不可接受的失败模式**。
4. 即便业务确要保守（FUP 成本刚性），保守应体现为「**对已确证用量设更低阈值、更早提醒**」，
   **而不是「把未知算作用量或因未知封人」**。

### 5.2 一条不属于政策开关的事

**亚 tick 连接不属于「未知」**——实测其终值完整（40/40 sub-ms、60/60 ~1–2ms）。
只有「用 Σδ 当账」这种**实现错误**才会 100% 漏计，**那是修 bug，不是 fail-open/closed 的取舍**。

### 5.3 UDP 计费口径（Fable 2026-10-10 裁决：**按账本口径**）

原为「仍未裁决」的产品口径。现裁定：**计费一律以服务端账本的数为准**（即应用真实净负载，
账本记的那个数），不再换算成任何「客户端封装口径」。

**理由（按分量排序，第 1 条是决定性的）：**

1. **「客户端口径」在本拓扑里甚至不是良定义的。** 那 10 B/报文（v4）/ 22 B（v6）是 **SOCKS5 UDP
   请求头**（RSV+FRAG 3 + ATYP 1 + 地址 4|16 + 端口 2）的长度，来自 Xray 那条路。而住宅 HY2 的
   UDP 走的是 hysteria2 over QUIC——客户端的封装是 QUIC datagram/stream，**没有**一个「加 10 字节」
   的对应量。想按客户端口径计费，就得凭空重构一个对不同客户端都不成立的定义。
2. **账本是唯一可验证的真相源。** 它是服务端逐连接、字节精确的（G0 实测 642 条连接逐连接差额 0），
   而客户端自报既不可得也不可信。计费锚定在一个别人无法复核的数上，是更糟的选择。
3. **混用才是真危险。** 偏差按**报文数**线性放大，一旦同一个用户的不同流量走了不同口径，
   就是系统性错账，且随包大小反向放大（小包影响最大）。**单一口径**本身就是最重要的性质。
4. **方向上是安全的**：账本口径**小于** Xray 那条路会记的（少掉每报文的头），所以相对旧口径
   是**少收**而不是多收；用户不会因为这次裁决被多算。

**必须配套做的（否则口径只是纸面上的）：**

- **订阅/面板文案要写明计费单位**是「服务端观测到的净负载」。用户拿客户端流量统计对不上时
  （尤其是小包密集的 UDP：每报文差 10/22 B，按包数放大），先看得到这句话，而不是来问。
- **不要在任何地方再引入第二种口径**——包括将来加 TCP 以外的协议、加新的订阅渲染路径时。
  这条写在这里就是给那时的自己看的。
- ⚠️ 这是**产品语义裁决**，主理人可一票否决。若否决，需要同时决定「换算公式 + 在哪个环节换算 +
  怎么对不同客户端成立」，那是另一件工作，不是本计划的一步。


---

## 6. 不在本计划范围（另立）

- bui-c 生命周期/owner/guard 线（隔离监督器、warm、Start/Restart、units/hooks、双栈物理矩阵）。
  入口见 `docs/superpowers/plans/2026-10-08-bui-c-recovery-guard.md` 与交接材料里的
  `next-actual-entrypoint-map.md`。**注意**：当前持锁同步 `systemctl` 的旧路径与 locking hooks
  存在死锁（job 等 hook、hook 等锁），**不得先装部分 hooks**。
- 客户端完整受管配置交付（平台逐项验收）。

---

## 7. 安全审查的未决发现（Fable 2026-10-10 裁决）

后台提交安全审查（`~/.claude/security/security_warnings_state_<session>.json`）在 `9f9b1fb` 上留下 3 条未决。
其中**两条此前已修**（记录在 `previous_findings` 里，此处只作存档）：`api/system.rs` 的 `format!("{result:?}")`
已换成只回错误消息 + `redact::url_credentials`；`ci.yml` / `release.yml` 的
`pin-kernels.sh --check || echo "::warning::…"` 已改成 `--check --warn`（`||` 会把硬失败一起吞掉，注释里点名了这次审查）。

**未决三条的裁决：**

| # | 位置 | 审查类别 | 裁决 |
|---|---|---|---|
| A | `crates/bui/src/modules/panel/hy2resi.rs` | 部署期可用性（客户端与渲染器分叉） | ✅ **已消解**（`79753b2`；本机门实测「内核来源与渲染器自洽」退 0）。客户端已改说官方 `daemon.StartedService`，而渲染器仍写 `experimental.v2ray_api` ⇒ 官方内核拒绝启动。**这就是本计划存在的理由**，切换批落地即消解 |
| B | `crates/bui/src/modules/panel/meter.rs` `g.open` | 无上界内存（远端连接 churn 喂养） | **真缺陷，单独排期修**。见下 |
| C | `crates/bui/src/modules/panel/users.rs` `blocked_set(.., lossy)` | fail-open 授权（攻击者可影响的状态关闭强制） | **接受的已记录风险**，但计划必须把它写成**安全控制**而非运维提示。见下 |

### 7.1 B：`g.open` 无上界（真泄漏，非理论）

`meter.rs` 里 `alerts` 有 `ALERTS_KEPT`、`dedup` 有 `DEDUP_CAP` + `DEDUP_TTL_MS`，**唯独 `g.open` 没有任何上界**：
`:507` `g.open.insert(ev.id, Open { known: total })`（NEW 事件）、`:515` `g.open.entry(ev.id).or_default()`（UPDATE，**连没见过的 id 也建条目**）、
`:720` `self.open.remove(id)`（只在收到 CLOSED 时删）。

**泄漏路径已核实**：判损时 `:572` 会 `resubscribe = true`（测试 `:1097` 守着），但 `note_session_start` **只在实例代际变化时 `g.open.clear()`**（`:614`）——
重订阅同一实例代际不变 ⇒ **gap 期间关闭的连接，其条目永久留下**。而 G0 已实测流会在 ~128 KiB 流控边界被截断（丢 4%~100%）
⇒ 反复 gap 会持续累积。

**影响面（如实界定，不夸大）**：`Open.known` **不参与入账**——入账用的是 CLOSED 事件的终值 `c.total`（规则 1）。
所以它不污染账本，只泄内存 + 扰动指标 A 的 ΔS 项（`R = ΔG − ΔB − ΔS`）。稳定态尺寸 ≈ 并发连接数，只在反复截断时累积 —— **不是紧急缺陷，但是确定的缺陷。**
**修法方向**（不要与本次切换搭车）。⚠️ **2026-10-10 对抗复审把修法方向纠正了两次，照下面写**：
**(a) 我原来的告诫「别把仍在开的连接算漏」方向是错的**——reset 快照（重新订阅后的第一帧）恰恰把**在开连接全部以 NEW + totals 重投**，
所以「每次重订阅就清空 `open`」不会漏掉活跃连接。
**(b) 但清表/设上界都拦不住真正的源头**：快照里还带 **closed 环**，其中 `closed_at != 0` 的条目被 `hy2resi.rs:245` **无条件当 `New` 收下** ⇒
「gap 期间关闭、从未入账」的连接被**重新种回 `g.open`**（去重表只在入账 helper `book()` 里写（`meter.rs:434`），
所以 `apply` 的 `booked` 守卫挡不住这一类）⇒ 幻影永存、ΔS 基线持续污染。
⇒ **修法必须先在解析侧按 `closed_at` 判别**：`closed_at != 0` 是「已经关闭」的公告，不是新连接，不得 `insert` 进 `open`。
在此之上，「在重订阅边界用 reset 快照结算幽灵（计 eviction 计数 + 发不静默的事件行，**且绝不写去重表**）」才成立。
**(c) 关于 `ΔS` 的正面回答**（对抗复审给）：结算幽灵 K ⇒ `ΔS = −K` ⇒ `R = ΔG − ΔB − ΔS` **上跳 +K** ——
这是**正偏差**：把「暂挂未确证」正确暴露成「永久未确证」，**不掩盖任何真实缺口**；代价只是结算瞬间指标 A 的一次无流量解释的瞬态尖峰（「解释缺失」而非「账目错误」）。
**两条陷阱不许走**：引入「累计 write-off 项」压尖峰（迟到的真 CLOSED 会**双计**）、或去减 `mark.span`（把尖峰**永远藏掉**）。

### 7.2 C：lossy 的 fail-open 是**安全控制**，不是运维提示

`users.rs` 的 `blocked_set(state, pending, now, lossy)` 在有损窗口里把该用户的 `pending` 视为 0
⇒ **有损期间新增的字节不参与封禁判定**。§5.1 的裁定（用户层 fail-open / 系统层 fail-closed）成立于
「诚实故障」的框架下，**但没有考虑「攻击者自己诱发」**。

⚠️ **已核实：`lossy` 是账本上的单个全局布尔，不是按用户的**（`MeterView` 只有 `lossy` / `lossy_since` / `lossy_until` 三个标量字段，
`Inner` 侧是 `lossy_since_ms` / `lossy_until_ms` 一对 `i64`；全账本共用）。所以它不是「一个用户逃配额」，而是**全局性**的：

> **单个用户**制造**持续**的计量 gap（判据是连接 churn 撑爆 gRPC 流控，G0 已实测会在 ~128 KiB 边界截断）
> ⇒ 全局 `lossy` 为真 ⇒ **所有用户**的未落盘 pending 都不参与判定 ⇒ 同期落盘循环也拿不到增量（`query_user_deltas` 返回 Err）。

**但严重度是有界的（三个参数把口子收窄了，这部分我起初也写过头，改回来）：**

| 收窄项 | 实测值 | 效果 |
|---|---|---|
| `LOSSY_MAX_MS` | **6 小时**（`until = min(now + LOSSY_HOLD_MS, since + LOSSY_MAX_MS)`） | 有损状态**硬封顶**在「自首次判损起 6 小时」，到点 `expire_lossy` 复位、`since` 清空 ⇒ **不能永久逃避**，每次诱发最多换 6 小时 |
| `LOSSY_HOLD_MS` | 30 分钟 | 单次判损的保底窗口 |
| 已有 usage | `blocked_set` 只把 **pending** 视为 0，**已落盘的 `usage` 照常判定** | 能逃的只有**未落盘的那一段**，历史超量仍会封 |

⇒ 定性：**有界、自限、每次窗口起止各留一条 incident 的已记录风险**，不是「一个人永久关掉强制」。

**这不改变 §5.1 的取舍**（反向做会在任何计量打嗝时封住全体无辜用户，更糟），但要补三件事：

1. **把它写成安全控制**：计划与代码注释里，「incidents 事件 + 人工核账」是**这一风险的唯一缓解**，
   不是「顺带提醒运维」。`Sig::Hy2ResiMeteringLost` 因此在功能上**不是可选项**（已有实现，`9f9b1fb`）。
   **并且要把 6 小时这个上限写进注释**——它是这条风险「有界」的依据，将来谁改动 `LOSSY_MAX_MS` 就要重估这条。
2. **记一条后续**：评估「有损持续接近 6 小时上限 ⇒ 升级动作」的策略。
   ⚠️ 因为 `lossy` 是全局的，任何升级动作**必须是全局性的**——按用户动作会误伤（攻击者诱发 lossy，被处理的是所有人）。
   这本身也是 DoS 面：升级策略既是缓解也是武器，取舍要单独论证。
3. **记第二条后续**：评估把有损**归因**做细（例如按用户维度统计「哪些用户的 UPDATE 落在 gap 里」）。
   当前账本只知「全局丢了 N 字节」而不知「丢的是谁的」，这正是它无法按用户降级的原因。

### 7.3 UDP 计费口径

**已裁决：按账本口径（应用净负载）**，理由与配套要求见 §5.3。本条不再是发布阻塞项。

---

## 8. 切换批验证者留下的残部（Fable 2026-10-10 收口）

原子切换批的两个验证者（对抗复审 + 契约核验）都判**通过、无阻塞缺陷**，但留下下列残部。
**判据分级**：「真」= 事实错误或安全面；「维护点」= 现在不错但升轨会错；「可选」= 外观。

| # | 位置 | 性质 | 处置 |
|---|---|---|---|
| 1 | `docs/superpowers/plans/2026-09-11-v4-master.md:66` | **真**：C1 契约清单仍列 `HY2_RESI_V2RAY_API`，而该符号已不存在（本文件本次已被 diff 改过 `:229/:342`，唯独漏了这处契约行） | ✅ **已完成**（`e90ad21`） |
| 2 | `docs/superpowers/specs/2026-09-15-hy2-singbox-residential-design.md:146/:156/:542/:548/:676` | **真**：`v2ray_api` 残留未全部加订正注（该文件已有 `:18/:66/:71/:129/:135/:354/:408` 的 2026-10-10 订正，口径不齐） | ✅ **已完成**（`e90ad21`；实补 7 处并顺带查出 2 处同缺陷） |
| 3 | `crates/bui/src/modules/panel/stock_gate_fixture.rs` 的新守卫 | **真（不可达但守卫不严）**：只校验 `services` 是长度 1 的数组；若 `services[0]` 是非对象（如数字），随后的 `["listen"]` 赋值会 panic。当前渲染器恒产对象故不可达 | ✅ **已完成**（`e90ad21`；并普查同文件同类「先索引后写」：`inbounds[0]`/`clash_api`/`outbounds` 的 `.unwrap()` 一并收口） |
| 4 | `crates/bui-schema/tests/kernel_hy2_singbox.rs` 的 openssl 早退 | **真（静默面）**：PATH 缺 sing-box 或 openssl 不可用时 `eprintln` + `return` **静默通过**；而 CI 的 skip 黑名单只匹配 `<kernel> not found` 与 caddy，**不匹配 `skipped: openssl not usable`** | ✅ **已完成**（`e90ad21` 取**后一种**：CI 里硬失败。理由——黑名单是外部 workflow 里的转写匹配器，注定会生产侧漂移而测试全绿。并已推广到 `common/mod.rs` 的 `have()`、`kernel_hysteria.rs` 的 `self_signed()`、`kernel_relay_policy.rs`） |
| 5 | `crates/bui/src/modules/panel/traffic.rs` 测试串 `fail_next("v2ray_api 不可达")`（`:787`/`:1193`）与「住宅的 QueryStats」注释 | **真（文案说谎）** | ✅ **已完成**（`e90ad21`，6 处） |
| 6 | `CHANGELOG.md:20`（未发布的 4.1.9 段）仍写「sing-box 1.14.2」 | **真**：锁自 `51024f5` 起是官方 1.14.3，且 `install.rs:356` 的 `client_sing_box` 与 `sing_box` 同源 ⇒ 服务端与 bui-c 一并到 1.14.3，该句对两个角色都失真 | **随第 13 步的发布准备改**（现在改是写未发生的记录） |
| 7 | `stock_gate_fixture.rs:449` 与 `tests/test-residential-stock-gates.sh:30` 的 `1.14.3` 硬字面量 | **维护点**：opt-in 手动夹具，升轨时须同步 | 记在此，升轨时一并 |
| 8 | `docs/superpowers/plans/2026-09-15-v41-hy2-singbox.md:802/:907` 旧口径 | **可选**：历史计划，不改史实 | 不动 |
| 9 | `run-all.sh` 以退出码判过、不解析 TAP；两个手动 runner 跳过仍计入「all script tests passed」 | **可选**：跳行有输出可见，只是汇总不区分「跑过」与「跳过」 | 记此，需要时再改 |
| 10 | 计划状态表滞后 | — | ✅ 已回填（第 4/5/6/7 步） |

### 8.1 发布前必须人工做的（不属代码改动）

- **私有渲染器快照需重采**：`offline_renderer_snapshot.rs:680` 已写明；本机无 docker 夹具故 `test-renderer-snapshot.sh` 跳过。切上线后用新渲染产物重采。
- **stock 门位夹具容器需用官方裸二进制重建**：sha `299f9f60…`（arm64）。旧自建 1.14.2 的 PASS **不继承**。
- **第 12 步真机兜底**：前半（`auth_user=<name> => gate` 路由串，含多用户 `[a b]` → None）**已于 2026-10-10 在隔离 worktree 里用官方内核实测完成**（见 §9）；**后半仍未做**——`/api/health` 对计量失败的 degraded 演练需要跑整套 `bui serve`（动 systemd 单元与 `/opt`），且**真实网络路径**同样未验（§9.2）。
- **存量安装换核依赖真实发布 + 真机升级**。

### 8.2 两条人工项的可执行规程（Fable 2026-10-10 补充，含一个新发现的阻断）

**共同前置（两条都要）**：`scripts/tests/` 的两个手动 runner 都依赖同一个 **network=none 的隔离夹具容器**
`bui-policy-tests-20260929`（可用 `BUI_TEST_CONTAINER` 覆盖名字）。**本机实测该容器不存在**（`docker inspect`
报 `No such object`；`docker ps -a` 里只有无关的 g0-meter / nextcloud / jimeng 等）。
两个 runner 在 run-all.sh 的无参调用下都按「缺前置 ⇒ skip」退 0（有意如此，否则 CI 的 lint job 恒红），所以**容器不在=它们静默不跑**——
⚠️ 但两者触发条件不同，别记混：stock runner 是**缺容器即 skip**（`:19-21`）；renderer runner 是**无参调用才 skip**（`:10-13`），
**带 2 个参数手动跑而容器缺失时它退 1 FAIL**（`:22-24` 报 `requires a network=none container`）——按下面规程 2 手动跑就会看到 FAIL 而不是 skip。
`run-all.sh` 仍会打印 `all script tests passed`。这正是 §8 #9 记的那条「汇总不区分跑过与跳过」。

#### ⚠️ 新发现的阻断：stock runner 的平台写死

`scripts/tests/test-residential-stock-gates.sh:11` 把 `expected_sha` 硬编成
**arm64** 裸二进制的 `299f9f60…`，并在容器里断言 `sing-box version 1.14.3`。
而**本开发机是 x86_64**（`/usr/bin/sing-box` 的 sha `768d31fa…` 对上 lock 的 amd64 条目）。
⇒ 在 amd64 机器上重建该容器时，`expected_sha` 必须同步换成 **amd64** 的 `768d31fa…`，
否则 `:29` 那条 `test "$(sha256sum …)" = "$1"` 必红。

**这不是「改一个字面量」那么简单**：它和 §8 #7 是同一类病的两种形态——**平台也写死了**。
建议随容器重建一并处理：把 `expected_sha` 按 `uname -m` 分支（或从 `kernels.lock` 取），
照第 16 步「期望值从单一真源读」的口径。**在改之前不要盲跑本脚本。**

#### 规程 1：重建 stock 夹具容器

1. 取 lock 里 amd64 的官方归档，解包取裸二进制：
   ```
   url=$(awk '$1=="sing-box" && $2=="target" && $3=="1.14.3" && $4=="amd64" {print $6}' scripts/release/kernels.lock)
   sha=$(awk '$1=="sing-box" && $2=="target" && $3=="1.14.3" && $4=="amd64" {print $5}' scripts/release/kernels.lock)
   ```
   解包后核裸二进制 sha（本机实测 `768d31fa7f68debc067bb4626539498a1e8751d36a7b99e1a54bee6a23815f93`）。
2. 建 `--network=none` 的容器，把裸二进制放到 `/new-kernel/sing-box` 并软链到 `/usr/local/bin/sing-box`
   （脚本 `:28` 断言的就是这两条路径）。
3. 按上面的平台阻断先修 `expected_sha`，再跑
   `bash scripts/tests/test-residential-stock-gates.sh`。
4. **旧自建 1.14.2 的 PASS 不继承**——那是另一份产物。

#### 规程 2：私有渲染器快照重采

1. 快照是**私有的**，不进公开仓库：输入目录需含 `inputs.private.json`，输出目录必须存在且为空。
2. 重采**必须在切换上线之后**（用新 `bui` 的渲染产物），否则采到的还是旧形状。
3. 跑：`bash scripts/tests/test-renderer-snapshot.sh INPUT_DIRECTORY EMPTY_OUTPUT_DIRECTORY`；
   或对 `cargo test -p bui --ignored offline_renderer_snapshot` 设 `BUI_RENDER_INPUT_DIR` /
   `BUI_RENDER_OUTPUT_DIR`。runner 的文档注释（`offline_renderer_snapshot.rs:675-685`）已写明
   「本 runner 的快照需在切换上线后用新渲染产物重采」。
4. 核对器：同目录的 `compare-renderer-snapshots.py`。

---

## 9. 第 12 步实测结果（Fable 2026-10-10，隔离 worktree + 官方内核）

在 `c9f0289` 的 detached worktree（`/tmp/wt-step12`）里，用**本机官方 `/usr/bin/sing-box` 1.14.3**、
**由渲染器逐字产出的**生产形状配置、真 hysteria2 客户端，对住宅门路由做了端到端实测。

**证据链（复核者另建 marker + 客户端独立复现，且 `cmp` 逐字节确认配置就是渲染器产的那份）**：

| 真机 `/connections` 的 `rule` 串（逐字） | 生产解析器 `user_of_rule` |
|---|---|
| `auth_user=alice => route(gate-r000)` | `Some("alice")` |
| `auth_user=bob => route(gate-r001)` | `Some("bob")` |
| `auth_user=[alice bob] => route(gate-r000)` | `None`（**多用户 `[a b]` 那一支确认**） |
| `final` | `None` |

- `chains` = `["slot-0-out","gate-r000"]`（**最内层在前**）；**`metadata` 无 `user` 字段** ——
  面板归因**只能**靠 `rule` 串，本次逐字确认（`spec §5.2` 的前提成立）
- 流量真的走到 `127.0.0.1:2080` 的 marker；`PUT /proxies/gate-r000` 返回 204 且**内核未重启**（PID 不变）
- **deny 是真拒绝点**：客户端 `curl rc=97`，服务端 `dial tcp 127.0.0.1:1: connect: connection refused`
- **认证语义**：客户端必须发**整串** `alice:pw-alice-01`；`alice:wrong-secret` 与裸 `pw-alice-01`
  都得到服务端 `authentication error, HTTP status code: 404`——复核者用**关闭端口对照**（`timeout: no recent network activity`）
  证明这是**真鉴权拒绝**，不是端口/证书/客户端配置的伪影
- **按用户隔离**：bob → `gate-r001` → 2081，alice 的 2080 不受影响
- **重启 fail-closed**：无 `cache_file`，重启后所有门回 `deny`

### 9.1 实测查出的三处「代码与真机不符」

1. **单测夹具的 rule 串是内核不会产出的形态**：生产单测里写 `"final => route(deny)"`，
   而 1.14.3 真机在 final 兜底时输出的是**裸 `"final"`**。两者对 `user_of_rule` 都 → `None`（**行为正确**），
   但夹具在教后人一个错误的形状 ⇒ **应改成真机原文**。
2. **夹具的 `chains` 顺序是反的**：真机最内层在前，夹具相反。生产代码**无 `.chains` 消费方**
   （`grep -rn "\.chains" crates/` 零命中，只有 `hy2resi.rs` 一处解析赋值）⇒ 无功能影响，但会误导未来的断言。
3. **`hy2resi.rs:75` 的注释陈旧**：称「chains 供自检看门位」，而 `selfcheck` 根本没有 chains 用法。

### 9.2 仍未覆盖

- **`/api/health` 对计量失败的 degraded 演练**：需要对 systemd 受管单元与 `/opt` 真实状态跑 `bui serve`，
  会改本机系统状态（本机同时是生产客户端的宿主）⇒ **超出实验沙箱的安全边界**，如实留白，交真机环境。
- 实验全程限于回环，**未验证真实网络路径下的行为**（那属第 12 步的「真机」本义，仍未做）。
