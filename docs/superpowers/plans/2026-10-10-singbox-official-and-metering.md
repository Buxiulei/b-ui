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

### 2.4 ✅ 缺口形状已钉死（决定性实验结论）

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
| 3 | 可见性：**整轮对账中止**要可见（**当前核实仍缺**——迁移期任何失败都是「机器不报错但状态是错的」）。⚠️ 见 §4.3 的**设计修订**：不新增 `last_reconcile_error` 字段，改为「包装 `reconcile_from_ctx` + 复用 `last_reconcile`」**加**哨兵 `Sig::ReconcileFailed` | ⬜ 施工图已就绪 |
| 4 | **原子核心**：删 `render/hy2_singbox.rs:185-191` 整个 `experimental.v2ray_api` 键（保留 `clash_api`；**必须删键，只删 stats 子对象无效**） | ⬜ 与内核换装同一发布 |
| 5 | 断言同步（同提交）：`hy2_singbox.rs:362-368` 删三条+补反向守卫；`core_files.rs:1156-1160` 同步；`hysteria.rs:283-286` 端口表登记新口。**端口号裁决：保留 `10086`**，只改常量名（`HY2_RESI_V2RAY_API` → `HY2_RESI_API`）与全部标签（不留「v2ray_api」幽灵）。理由：回环内部 API，号无对外语义，保留号让端口表/夹具/测试只改文案不改值；说谎的责任落在名字上。**常量改名只许与渲染器同一次提交**（`hy2resi.rs:26-31` 已把这条写死） | ⬜ |
| 6 | **测试改真跑**：`kernel_hy2_singbox.rs` 删 skip 门，让两用例对不含 `v2ray_api` 的产物跑官方 `sing-box check`（**CI 抓 skip 的 grep 已删，此用例现在是静默——整个切换期住宅配置形状无人验**） | ⬜ blocker |
| 7 | 计量路径收口：`panel/hy2resi.rs:275,357` 删除或改接；`traffic.rs:240-248` Err 分支必须可见或整体删除；面板/文档承诺同步 | ⬜ 等 §3 |
| 8 | 自检改写：`selfcheck.rs` 拆成「计量面／门位·在线面」两条判据，去掉 10086 白等 120s 与必然 FAIL | ⬜ |
| 9 | **版本钉子改能力/sha 绑定**（§1.4 的核心）：`serve.rs`（否则整轮对账中止）、`upgrade.rs`（否则升级双通道自锁）、`install.rs`（否则新装中止）、`gates.rs`（否则门停 deny 住全体住宅用户）；rollback 守卫从版本白名单改为 `RESIDENTIAL_LAYOUT_SINCE` + sha | ✅ **完成**（`fd55363`） |
| 10 | 夹具与守卫：`relay_generation_kernel.rs`/`generation.py`/`policy.py` 版本串；`install/upgrade/serve` 夹具 1.14.2 字面量；stock 三处换官方真实 sha；`fakes.rs` 补守卫测试 | 🔶 部分（stock sha 已换 `299f9f60…`） |
| 11 | 清扫：`serve.rs` 的 `bin/.prepared-<uuid>` 失败与成功路径统一清理（否则每 tick 残留 ~81MB 且不被体检暴露） | ✅ **完成**（`fd55363`） |
| 12 | 真机兜底：官方 1.14.3 + 生产形状配置实测 `auth_user=<name> => gate` 路由串（含多用户 `[a b]` → None 分支）；演练 `/api/health` 对计量失败的 degraded 表现 | ⬜ |
| 13 | **发布纪律**：2–12 同一 release，不许拆包 | ⬜ |
| 14 | **原子性自检（机制化闸门）**：内核来源与渲染器是否产出 `experimental.v2ray_api` 必须自洽 | ✅ **完成**（`51024f5`；`check-kernel-consistency.sh`，ci.yml 只告警 / release.yml 硬闸门。**真实 CI 里验证过会发告警**） |
| 15 | **lock 检查的 fail-open 修复**：`pin-kernels.sh` 新增 `--warn`——只降级「漂移」，`lock 少行`／`核实不了` 仍旧退 1（原 `\|\| echo ::warning::` 把三类一起吞掉） | ✅ **完成**（`fd55363`；安全审查与对抗复审各自独立指出） |
| 16 | **测试层的版本守卫要读 lock，不能写死**（🔴 **不做则「一直更新最新版」在实践上失效**，见 §4.2） | ✅ **完成**（`ddd928c`）。四个文件改完；**换版证伪实验**（私有 mount namespace bind 换入版本列=1.14.99 的锁，真文件零改动）四守卫全部改口变红；坏锁四类（缺失/空/无行/缺列）全硬失败，无一处降级为 skip。遗留：`relay_generation.rs:527`（在 `src/`、自洽故不卡 bot）与 stock 那族（opt-in 手动夹具）并入第 4/5 批 |

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

**修订后的做法（两件，缺一不可）：**

| 件 | 落点 | 为什么 |
|---|---|---|
| **① 包装 `reconcile_from_ctx`** | `serve.rs:203-393`（原函数体整体下移成私有 `reconcile_inner`，外层只留 wrapper） | 一处覆盖**全部 5 条出口**与全部 4 个生产调用点（`serve.rs:745`/`:775`/`:902`、`install.rs:1224`）。**不新增字段**：Err（`!dry_run`）时把一份只填 `at` + `errors` 的合成 `ReconcileReport` 写进 `last_reconcile`——`api/health.rs:66-69` 的 degraded 第三项**自动点亮**，`status.rs:388-393` 同源判据同步点亮，`format_status:92-105` 自动打出错误行，`HealthResponse` 形状（`status.rs:18-19`/`:290`、`api/mod.rs:331` 三处注释锁死的回归面）**一字不改** |
| **② 哨兵 `Sig::ReconcileFailed`** | `signature.rs`（`Sig` + `ALL_SIGS` + `id()` + `action()`=`Alert` + `rule()` + 匹配器）、`run.rs` 分派、`system.rs` 处理 | 让它在**有人看的地方**主动出现：`bui incidents` + **面板系统状态卡**（`web/app.js:1485` 已在拉 `/incidents?limit=20`，`web/index.html:154` 有刷新按钮）。那两行 `tracing::error!("[启动]对账失败：{e}")`（`serve.rs:755`/`:777`）**本来就在 b-ui 自己的 journald 里**，哨兵也本来就盯着 `b-ui`（`sentinel/run.rs:21-25`），**只差签名**。冷却/去重/恢复语义全是现成的 `Rule` |

**两条硬约束（侦察点名、必须遵守）：**

- **只许写自己那一项。** `:387` 一次写 `restart_keys` / `drift` / `last_reconcile` 三个字段；失败路径
  若照抄整行会把真实漂移与重启键擦成空（而漂移非空**本身也是 degraded 判据**，`api/health.rs:65`）。
- **必须「下一轮成功即清空」。** `:387` 在成功轮无条件覆盖 `last_reconcile` ⇒ 清空是自然的；但**不许**
  另加一个只在失败时写的字段（那就要自己发明「什么时候算恢复」）。教训见 `reconcile/drift.rs:466`、`modules/system.rs:524`。

**已核过的兼容性**：`last_reconcile` 的活消费者只有 4 处，合成报告在每处都**更真**——`api/health.rs:67`（degraded ✅ 想要它亮）、
`api/health.rs:80` / `status.rs:411`（响应体 ✅）、`api/system.rs:28`（`POST /api/reconcile` 回最近一份报告 ✅）、
`status.rs:391`（degraded ✅）。其余是测试（`serve.rs:2364`、`api/mod.rs:331/335/383`、`runtime.rs:437/439`）。
`is_clean()` 的 7 个使用点**全在测试里**，不受影响。

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

### 5.3 仍未裁决的产品口径

**UDP 按哪个口径计费**：账本记的是**净负载**，与客户端封装口径差**恒定 10 B/报文/方向（v4）/
22 B（v6）**，按包数线性放大。混用即系统性偏差 ⇒ **产品决策，不是测量问题**，发版前必须定死一个。

---

## 6. 不在本计划范围（另立）

- bui-c 生命周期/owner/guard 线（隔离监督器、warm、Start/Restart、units/hooks、双栈物理矩阵）。
  入口见 `docs/superpowers/plans/2026-10-08-bui-c-recovery-guard.md` 与交接材料里的
  `next-actual-entrypoint-map.md`。**注意**：当前持锁同步 `systemctl` 的旧路径与 locking hooks
  存在死锁（job 等 hook、hook 等锁），**不得先装部分 hooks**。
- 客户端完整受管配置交付（平台逐项验收）。
