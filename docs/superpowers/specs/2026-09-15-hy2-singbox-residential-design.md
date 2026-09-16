# b-ui 4.1 住宅 HY2 改 sing-box 单入站设计（凭据池 + 门控 + b-ui 自管 nft 跳跃）

日期：2026-09-15。状态：**主理人已拍板方向，主会话已终审**——八条终审裁决见 **§14**，正文（§2.2 / §2.4 / §2.5 / §3.1 / §3.4 / §4.2 / §7 / §11 / §12）已按裁决改写，实施者按正文做即可。

上位文档：`docs/superpowers/specs/2026-09-11-v4-architecture-design.md`（下称 **spec**，§3.1 / §3.2 / §3.4 / §4.2 / §5.6 / §5.7 / §9 受本文修订）、`docs/superpowers/plans/2026-09-11-v4-master.md`（下称 **总纲**，C1–C5 影响见 §13）。

输入与引用约定：

- **评估** = 4.1 评估工作流输出（`sb` = sing-box 1.14 上游事实 27 条、`coupling` = b-ui 耦合盘点 50 条、`ev` = 三方案矩阵 / `key_facts` / `plan_if_B` / `risks` / `unknowns`）。
- **tizi PoC** = 2026-09-15 06:4xZ 在 `bwg-tizi` 上以隔离端口跑的 sidecar 真机验证（入站 `:40100`、跳跃 `50001-53000`、独立 nft 表、已自清理；生产单元全程 active）。结果文件与硬件实测在本次会话的 scratchpad（`scratchpad/poc/RESULTS-tizi-sidecar.md`、`scratchpad/v41-hw-facts.md`），**不入库**——所以本文把每条结论逐字抄下来，读本文不需要那两个文件。
- **本机 PoC** = 开发机上的配置形状验证（`scratchpad/poc/sb1.json` 门控、`sb128.json` 128 凭据、`hyc_hop.yaml` 换源端口）。
- 源码引用一律 `路径:行`（按 2026-09-15 的 `v4-portjump-nft` 分支）；上游行为引用官方文档 URL 或评估里的事实条目。拿不准的一律进 §12，不写成事实。

生产主机只用别名：`bwg-rick`（测试机，先上）、`bwg-tizi`（生产）、`baiyi`（Linux 客户端）。示例一律 `example.com` / `203.0.113.10` / `alice`。

**主理人 2026-09-15 裁决**：住宅 HY2 从「每住宅 IP 一个 apernet hysteria 进程」改为「**一个 sing-box hysteria2 入站 + `auth_user` 路由到各槽出站**」；已接受三个代价——① 自建 sing-box（`with_v2ray_api`）进发布链；② 到期 / 封禁用户「握手成功但全部流被拒」；③ 住宅 HY2 收成单进程。**直连 HY2 仍用 apernet，不动**。staging 方式已定为 tizi sidecar（不另开 VPS、不等低峰）。追问「自建以后每次 sing-box 更新是不是还要重建」——答案在 §5.4：轨道 → 锁 → CI 构建，**不需要人工重建**；构建或校验失败则锁不动、发布保持上一版并告警。

---

## 1. 背景、目标与非目标

### 1.1 2026-09-15 回归事故：切片跨进程 + 旧整段订阅 = 2/3 静默丢包 → 30 秒 idle 超时

v4.0.0 的 IP 池（spec §5.6 落地细节 D3/D4）把住宅跳跃段 `41000-50000` 按**槽位空间等分成不相交切片**，每槽一个 apernet 进程：

- 端口换算是槽序号的纯函数：`hy2_port = ports.hy2_resi + i`、`hop = hop_slice(ports.hy2_resi_hop, i, slot_span)`（`crates/bui-schema/src/slots.rs` 的 `hop_slice` / `resources`，常量 `MAX_SLOTS = 8` 在 `slots.rs:21`）。
- 每槽渲染一份配置、`listen: :{hy2_port},{hop.0}-{hop.1}`（`crates/bui-schema/src/render/hysteria.rs:42-78` `residential_slot_yaml`），单元名 `hysteria-residential[-<i>]`（`crates/bui/src/reconcile/mod.rs:42-48` `resi_unit`）。

而用户手里那份订阅写死的是**整段** `mport=41000-50000`（v3 与单槽时代发出的全部订阅；`render/subscription.rs:137` 写 `&mport={start}-{end}`）。客户端每 30 秒换一次端口（`render/subscription.rs:213`、`render/client.rs:338` 都渲染 `hop_interval: "30s"`），换到的端口有 2/3 的概率落在**别的槽的进程**上——生产两台各跑 4 个 hysteria 进程（1 直连 + 3 住宅，`v41-hw-facts.md`），整段被切成 3 片，只有 1 片是自己的。

后果不是「重握手一次」，而是**静默丢包直到超时**：内核只按目的端口分流，包进了另一个进程；那个进程既没有这条 QUIC 连接的 CID、收到的也不是握手包，于是不应答（现场表现为静默丢包）。客户端在下一次跳跃之前收不到任何回应，直到 **30 秒 idle 超时**才重建连接——我们渲染给客户端的 `idle_timeout` 是 `30m`（`render/subscription.rs:233`，且注释说明它必须 ≥ `hop_interval`），所以掐会话的是客户端 QUIC 栈自己的空闲判据与用户重连，而不是这个值。这条「为什么是静默丢包而不是 stateless reset / 立刻重握手」的内核细节属于现场观测，未逐包验证，列入 §12 第 13 项。

根因是**结构性**的：**共享一段跳跃 ⇔ 单进程**。任何「多进程各监听一片」的方案都不可能让整段订阅稳定（评估 `ev.key_facts[0]`）。4.0.1 曾设想的「登记分配」（给每槽登记持久切片）救不了手里仍是整段的旧订阅、只能防今后再切，且在本方案下 100% 沉没（评估 `ev.recommendation` / `ev.rationale`）。而 apernet hysteria **没有**「按认证用户选出口」的能力（评估 `sb.facts[1-apernet对照]`：`fillAuthenticator` 只有 password/userpass/http/command 四种鉴权后端，无路由概念），单进程里按用户钉住住宅 IP 只有 sing-box 的 `auth_user` 路由能做（https://sing-box.sagernet.org/configuration/route/rule/ ；hysteria2 入站把认证用户名写进 `metadata.User`，评估 `sb.facts[2-auth_user适用hysteria2]`）。

### 1.2 目标

1. **共享一段跳跃**：住宅 HY2 只剩一个监听端口 `:40000`，整段 `41000-50000` 由 b-ui 自管的 nft 表 REDIRECT 到它。**tizi PoC 已在生产内核上验证**：`table inet bui_sidecar` 同时挂 prerouting 与 output 两个 hook（priority -100）后，客户端用公网域名 + `50123` / `52777` 两个随机端口、`insecure: false`（真证书）连接，两个端口都握手成功并返回 HTTP 204。
2. **增删住宅 IP 不牵连任何用户**：住宅 HY2 节点不再含任何槽位信息（端口、区间、凭据都与槽无关）；删上游只改受影响用户的「门位」（一次 Clash API PUT），不重写配置、不重启、不改任何人的订阅；`resubscribe_impact` 三组通知机制整体退役。
3. **用户全生命周期零重载**：建 / 到期 / 封禁 / 解封 / 换槽 / rotate / kick 只经 Clash API 切门。**tizi PoC 已验证零重连**：`PUT /proxies/gate-r000 {"name":"deny"}` → 请求 HTTP 000；切回 → 204；客户端日志 `connected to server` 次数**全程 = 1**。
4. **消灭一类崩溃循环**：apernet 内置跳跃自建规则、被 SIGKILL / OOM 杀掉后残留导致下次启动 FATAL、`Restart=always` 变崩溃循环（`crates/bui/src/modules/portjump.rs:1-40` 记录 bwg-rick 2026-09-12 连崩 52 次）。4.1 的住宅路径上不再有这类残留：nft 表由 b-ui 幂等持有、与进程生命周期无关。
5. **计量、在线、踢人、哨兵、watchdog、自检、验收脚本**全部跟上新拓扑，判据可机器验证（§5、§8、§10）。

### 1.3 非目标

- **不动直连 HY2**（`hysteria-server`，`:10000` + `20000-30000`）：apernet、`auth.type: http`、trafficStats 计量、`hy2-prestart` 孤儿链清理、`Hy2AuthHttpFailed` 哨兵签名在它上面原样保留。`bui set hy2-auth http|command` 的作用域缩成「仅直连」（§6）。
- **不做 sing-box 配置热重载**：官方 sing-box 没有运行期加删用户 / 改路由的接口，SIGHUP = 整 box 关闭重建、hysteria2 入站 `Close` 关掉 UDP 监听（评估 `sb.facts[3-SIGHUP重载机制]`、`ev.key_facts[2]`）。本设计**不需要**热重载，靠静态凭据池 + 门控绕开。
- **不合并 `b-ui-relay` 与住宅 HY2 入站进同一个 sing-box 进程**：relay 因池增删 / 黑名单 / pin 会重启，合并会把这些重启传导给 HY2 的 QUIC 会话。两个进程各自的重启面保持今天的形状。
- 不做多住宅分组（spec §5.5 预留不变）、不做按用户限速、不换 Xray、不改三种订阅的形状与 `bui-c` 的渲染器接口（只改节点集合的值，§7）。

---

## 2. 架构

### 2.1 进程拓扑前后对比

| | 4.0.x（今天） | 4.1（本文） |
|---|---|---|
| 住宅 HY2 进程 | 每槽一个 apernet 进程，N ≤ 8（`reconcile::managed_units`，`reconcile/mod.rs:73-81`） | **一个** sing-box 进程 |
| 单元 | `hysteria-residential`（槽 0）+ `hysteria-residential-<i>` | **只剩 `hysteria-residential`**（名字沿用，见 §2.5）；`ExecStart={bin}/sing-box run -c {base}/hy2-residential.json` |
| 监听 | `:(40000+i)` + 第 i 片切片 | `:40000` 单端口；整段 `41000-50000` 与兼容段 `40001-40007` 由 nft REDIRECT 到 `:40000`（§2.4） |
| 鉴权 | 外置 `auth.type: http` → 守护进程 `127.0.0.1:18789`（`render/hysteria.rs:13`、`modules/panel/auth_http.rs`） | 内置 `users[]` 静态凭据池（§3），握手在 sing-box 内完成；守护进程不在鉴权路径上 |
| 按用户选出口 | 每槽一进程，进程即出口 | 每凭据一条 `auth_user` 规则 → selector 门 `gate-<id>` → `slot-<i>-out`（socks `127.0.0.1:2080+i`）或 `deny` |
| 用户变更 | 零重启（配置不含用户，`core_files.rs:199-221`） | 零重启（配置只含凭据池；门位经 Clash API `127.0.0.1:9092`） |
| 计量 / 在线 / 踢人 | trafficStats `9998-i` 的 `/traffic?clear=1`、`/online`、`/kick`（`panel/hy2.rs`） | v2ray_api gRPC `StatsService` `127.0.0.1:10086` + Clash API `/connections`（§5） |
| 端口跳跃的所有权 | apernet 自己建（`portjump.rs` 按 `HYSTERIA-PR-*` 链清理孤儿） | b-ui 自管 nft 表 `inet bui`，三处幂等重放（§2.4） |
| 受管单元数 | 6 + (槽数 − 1) | **固定 6** |
| 内存 | apernet 每实例 **28.1 MB**（tizi 实测；生产 4 个合计 108.4 MB，`v41-hw-facts.md`） | 单进程 **61.8 → 65.2 MB**（tizi 实测，括号内为传输中）；本机 128 凭据 + 128 门 + 128 规则仍 ≈ 57 MB、`check` 0.04 s（`sb128.json`）⇒ **3 槽起更省**（65 vs 84 MB） |

直连 `hysteria-server`、`xray`、`b-ui-relay`、`caddy`、`b-ui` 五个单元的拓扑不变。硬件不是判据：两台都是 2 vCPU / 1020 MB，各内核 CPU < 1%，B 净省 ≤ 60 MB（`v41-hw-facts.md`）。

### 2.2 数据路径

```
v2rayN / bui-c / apernet client
   │ UDP → example.com:{40000 | 41000-50000 | 40001-40007(兼容段)}
   ▼
[nft inet bui]  prerouting nat 与 output nat 各一条：udp dport 41000-50000 → redirect to :40000
   ▼
sing-box hysteria2 入站 hy2-resi :40000     users[] = 凭据池，password = "<name>:<secret>"
   │ 握手：auth 串命中 users[] ⇒ metadata.User = name；不命中 ⇒ 走 masquerade（伪装站点），无日志
   ▼
route.rules   { "auth_user": ["alice"], "outbound": "gate-r000" }
   ▼
selector gate-r000 { outbounds: ["deny","slot-0-out"…"slot-7-out"], default: "deny", interrupt_exist_connections: true }
   ├─ slot-3-out : socks → 127.0.0.1:2083 ──▶ b-ui-relay 的 slot-3 入站 ──▶ slot-3-pool ──▶ 住宅上游 ──▶ Internet
   └─ deny       : socks → 127.0.0.1:1（永不监听）⇒ 每条流 connection refused
```

- 目标域名不在本机解析、原样经 socks 交给 relay，与今天 apernet `acl: relay(all)` → socks5 同语义（`render/hysteria.rs:50-74`）。
- relay 侧**完全不变**：8 个槽出站 `slot-<i>-out` 指向 relay 的 `slot-<i>` 入站（端口 `2080+i`，`slots.rs:16` `RELAY_SOCKS_BASE`；relay 渲染在 `render/relay.rs:110-177`）。**8 个出站在配置里预声明**，槽不存在时它只是拨不通的回环端口 ⇒ 池增删不改本文件（评估 `ev.key_facts[11]`）。
- 「门」是 sing-box `selector` 出站（https://sing-box.sagernet.org/configuration/outbound/selector/ ：`outbounds` 必填、`default` 缺省取第一个、`interrupt_exist_connections` = "Interrupt existing connections when the selected outbound has changed"）。
- `deny` 用指向 `127.0.0.1:1` 的 socks 出站而不是 `block`：`block` 出站 1.11 弃用、1.13 删除（https://sing-box.sagernet.org/deprecated/ ，评估 `ev.key_facts[4]`），而门的成员必须是**出站**。端口 1 本机永不监听，`bui selfcheck` 加一行断言（§8.3）。
- **`deny` 的日志噪音本期不预先改**（§14 裁决 4）：被封用户每个请求打一行 `outbound/socks[deny]` 的拒绝行，本期只靠单元的 `LogRateLimitIntervalSec=10s` + `LogRateLimitBurst=200`（§2.5）与哨兵忽略这类行（§8.1）压噪音。速率**实测后裁**（§12 第 11 项量出行数/分钟）；退路两条、都不进本期：把 `deny` 换成本机 blackhole 端口，或把 `log.level` 降到 `warn`（降 `warn` 会一并吃掉 §6 那条带用户名的 `inbound connection` 排查线索，所以不先付这个代价）。

### 2.3 `hy2-residential.json` 形状（新渲染器 `bui_schema::render::hy2_singbox`）

```jsonc
{
  "log": { "level": "info", "timestamp": true },
  "inbounds": [{
    "type": "hysteria2", "tag": "hy2-resi", "listen": "::", "listen_port": 40000,
    "ignore_client_bandwidth": true,
    "users": [ { "name": "alice", "password": "alice:<hy2_password>" },
               { "name": "r017",  "password": "r017:<22 字符随机>" } ],
    "obfs": { "type": "salamander", "password": "<node.obfs.password>" },
    "masquerade": { "type": "proxy", "url": "https://www.bing.com", "rewrite_host": true },
    "tls": { "enabled": true,
             "certificate_path": "/opt/b-ui/certs/fullchain.pem",
             "key_path": "/opt/b-ui/certs/privkey.pem" }
  }],
  "outbounds": [
    { "type": "socks", "tag": "deny", "server": "127.0.0.1", "server_port": 1 },
    { "type": "socks", "tag": "slot-0-out", "server": "127.0.0.1", "server_port": 2080 },
    { "type": "selector", "tag": "gate-r000",
      "outbounds": ["deny","slot-0-out","slot-1-out","slot-2-out","slot-3-out",
                    "slot-4-out","slot-5-out","slot-6-out","slot-7-out"],
      "default": "deny", "interrupt_exist_connections": true }
  ],
  "route": {
    "rules": [ { "action": "sniff" },
               { "auth_user": ["alice"], "outbound": "gate-r000" } ],
    "final": "deny"
  },
  "experimental": {
    "clash_api": { "external_controller": "127.0.0.1:9092" },
    "v2ray_api": { "listen": "127.0.0.1:10086",
                   "stats": { "enabled": true, "users": ["alice", "r017"] } }
  }
}
```

**哪些已在 1.14.0 真机上过 `check`**（tizi PoC，逐条）：hysteria2 入站 + `users[{name,password:"r000:<pw>"}]` + salamander `obfs` + `masquerade proxy{url,rewrite_host}` + 复用生产证书 + `ignore_client_bandwidth` + 三个 selector 门 + 两条 `auth_user` 规则 + `clash_api`；**route 规则用传统 `"outbound"` 字段即可（无需 action 语法）**——所以渲染器就产出这个已验证的写法。

**哪些还没验**（PoC 配置里没有，进 §12）：`experimental.v2ray_api` 段（tizi 上跑的是官方 1.14.0 二进制，不带 `with_v2ray_api`，这段配置在它上面必然 FATAL，只能等自建二进制）、`{"action":"sniff"}` 规则、QUIC 空闲超时字段名。

字段出处：`users[].name/password`、`obfs`、`masquerade`、`ignore_client_bandwidth`、`bbr_profile`（1.14 起，缺省 `standard`，不写）、`tls` 必填——https://sing-box.sagernet.org/configuration/inbound/hysteria2/ ；`ListenOptions` 只有单个 `listen_port`、**没有端口范围字段**——https://sing-box.sagernet.org/configuration/shared/listen/ （评估 `sb.facts[5-入站无端口范围字段]`，这正是必须自管 nft 的原因）；`v2ray_api.stats.users` = "User list to count traffic" 且 "V2Ray API is not included by default"——https://sing-box.sagernet.org/configuration/experimental/v2ray-api/ 。

与今天 apernet 配置（`render/hysteria.rs:86-152` `common_doc`）的逐项对应：`sniGuard: disable` → sing-box 无此概念，不需要字段；`resolver`（DoH）→ 本实例不解析目标域名，不要 `dns` 段；`trafficStats` → `v2ray_api`；`auth` → `users[]`；`sniff` → route 的 `sniff` action；`quic.maxIdleTimeout: 60s` → 字段名待核（§12 第 7 项）。这四个 apernet 专属字段在 sing-box 的入站 schema 里根本不存在，是配置模型的结构性差异而不是改名（评估 `sb.facts[1-apernet专属字段无对应]`）。

**新端口常量**：`HY2_RESI_CLASH_API = "127.0.0.1:9092"`（relay 是 9091，`residential/mod.rs:42` + 测试 `:388`）、`HY2_RESI_V2RAY_API = "127.0.0.1:10086"`（Xray api 是 10085）。两个面都**只监听回环、不设 secret**，与 relay 的 Clash API 同口径。`render/hysteria.rs` 里那条「鉴权端口避开全部监听端口」的守门测试扩成一张端口占用表，把这两个也钉进去。

**兼容矩阵不适用于本文件**：它只在我们自建的 sing-box（≥ 1.14，带 `with_v2ray_api`）上跑，CI 只对它跑自建 1.14 的 `check`（§5.4 第 9 条）。三种订阅 / relay / `bui-c` 必须继续过 1.12–1.14 三版 `check` 的约束不变。

### 2.4 端口与 nft 表

**期望态端口**：住宅 HY2 固定 `ports.hy2_resi = 40000` + 整段 `ports.hy2_resi_hop = (41000, 50000)`；兼容段 = `40001 .. 40000 + MAX_SLOTS - 1` = `40001-40007`（4.0 按槽发出的那些端口），由新字段 `system.hy2_resi_compat_ports: bool`（serde default **true**）控制。

防火墙（`modules/system.rs:84-108` `firewall_ports`）签名从 `(p: &Ports, slots: u16)` 改为 `(p: &Ports, compat: bool)`，放行 `40000/udp` + `41000-50000/udp` + （compat 时）`40001-40007/udp`；`slot_span` 不再参与端口计算。

**表形状**（`bui_schema::render::nft::ruleset(&Ports, compat: bool) -> String`，唯一实现）：

```
table inet bui {
  chain prerouting {
    type nat hook prerouting priority -100; policy accept;
    udp dport 41000-50000 counter redirect to :40000 comment "hy2 residential hop"
    udp dport 40001-40007 counter redirect to :40000 comment "hy2 residential 4.0 compat"
  }
  chain output {
    type nat hook output priority -100; policy accept;
    udp dport 41000-50000 counter redirect to :40000 comment "hy2 residential hop (local)"
    udp dport 40001-40007 counter redirect to :40000 comment "hy2 residential 4.0 compat (local)"
  }
}
```

- **双 hook 是 tizi PoC 实测得出的硬要求**：只挂 prerouting 时，本机发往自身公网 IP 的包不过 prerouting，跳跃失效；apernet 的规则同样是 PREROUTING + OUTPUT 两条（`portjump.rs:6-9` 的实录）。`bui selfcheck` 与 m1/m3 都用 bundled 客户端在本机带 `mport` 打自己，没有 output 链就会假 FAIL。族用 `inet`（一张表覆盖 v4/v6），对应 apernet 分别在 v4 / v6 建规则的事实（`portjump.rs:50` `TABLES`）。
- **`counter` 是兼容段命中的采集手段，判据读持久值**（2026-09-16 裁决）：nft 的 `counter` 是瞬时流计数，`render::nft` 每次重放先 `flush table` 会把它清零（开机 / `bui nft apply` / watchdog 自愈 / 改端口都算），所以每次重放前先读一次活计数、增量累加进 `runtime.json`（累计命中 + `last_hit_at`）；下面这条判据与 `bui status` **一律读持久值，绝不读活 counter**（误判方向危险：把仍在用的兼容段判成闲置关掉 ⇒ 未刷订阅的 4.0 住宅用户当场断联）。`bui status` 打印「兼容段 40001-40007 最近命中 N 次（自 <时刻>）」；**连续 30 天为 0** 才 `bui set hy2-resi-compat off`（删那两条规则 + 防火墙收口），下线前在 CHANGELOG 与面板事件提前 30 天通知。这条判据**不收紧**（§14 裁决 3）。4.0 发出的按槽订阅只有 `40000+i` 这一个端口会失效（切片本来就在整段里），所以兼容段只为它存在。
- **下线前的通知必须点名 Linux 客户端重新导入**（bui-c 会话 2026-09-15 提出，已采纳；与其客户端定稿口径一致）。原因：`bui-c` **不会自动刷新节点**——每分钟的 `check` 与每日自动更新都不取节点，取节点只有 `bui-c import` 与菜单 [3] 两条路；**从不重新导入的机器会在兼容段下线当天直接连不上**。所以下线流程必须三件齐备：
  1. 下线公告里**单列一段**「Linux 客户端（bui-c）必须重新导入一次」，给出 `bui-c import --sub -` 与菜单 [3] 两条路径；
  2. 下线前用上面那个 `counter` 判定兼容段是否仍有流量命中——**有命中说明还有机器没导入，窗口顺延**；
  3. 公告与下线动作之间**不少于 30 天**。
- **落地**：新 `Artifact::NftTable { family, name, ruleset }`（C2 的**第 10 个**变体；今天 9 个，`reconcile/mod.rs:228-270`）。`apply` 把 ruleset 哈希与 `runtime.restart_keys["nft:inet:bui"]` 比对，不同或 `nft list table inet bui` 不存在时跑**一个** `nft -f` 事务（`table inet bui` + `flush table inet bui` + 完整定义，同一事务原子替换）。三处调同一个函数：`hysteria-residential.service` 的 `ExecStartPre=-{bin}/bui nft apply`（`-` 前缀：nft 失败不阻塞内核启动，`:40000` 照常可用，只是跳跃失效并被自检 / watchdog 点名）、每轮对账、watchdog 每 60 秒发现表缺失即重放。删除 = `nft delete table inet bui`（§9 回滚必须做）。
- **与 apernet 的共存：已在生产内核上验证**。tizi PoC 的 `inet bui_sidecar` 与**生产现有 9 张 `hysteria_*` nft 表**同族共存、无冲突（区间 `50001-53000` 与 `41000-50000` / `20000-30000` 不重叠），期间生产五个单元全程 active。评估里这一条原是 `confidence=unsure`（`sb.facts[5-REDIRECT方案的可行性]`、`ev.risks[4]`），现已落地为事实。
- **与 apernet 孤儿清理的关系：rc2 已修，直连与住宅同一判据**（§14 裁决 7）。spec 早先那段「生产上 apernet 建原生 nft 表 `hysteria_*`、而 `portjump.rs` 只扫 `iptables -t nat -S` 的 `HYSTERIA-PR-*` 链 ⇒ 直连实例的孤儿清理可能一直是 no-op」是按**rc2 之前**的代码写的，**已不成立**：4.0.1 rc2 给 `crates/bui/src/modules/portjump.rs` 加了 nft 后端扫描（`nft list tables` 枚举 `hysteria_*` → 逐张读正文认领 → 整表删），判据是「表正文 `redirect to :<base>` 或 `dnat to [<ip>]:<base>` 指向**本实例 base 端口**」，所以**直连实例的 base `10000` 同样覆盖**（绝不按表名前缀整表删，免得清掉同机别的 hysteria）。**2026-09-15 在 `bwg-tizi` 实测**：重启 `hysteria-residential-1` 后 prestart 日志打出「已删除 nft 表 ip6 hysteria_\<hash\>（本实例端口跳跃孤儿）」，孤儿命中 **2 → 0**，现役表 `44000-46999 → :40001` 正常重建。⇒ §12 第 14 项**已作废**（标注保留、序号不重排）。
- **`table inet` + `type nat` 要求 Linux ≥ 5.2**（2026-09-16 裁决：钉下限，不出 `ip` + `ip6` 双表）：低于该内核整份规则集被整事务拒绝、住宅段与兼容段双双不通，而住宅入站只监听 `:40000` ⇒ 该机住宅用户当场断联。受支持目标（Ubuntu 22.04+ / Debian 12 / CentOS Stream 9）内核均 ≥ 5.15；低于 5.2 的只有已 EOL 的 CentOS 7 / Debian 10。`bui nft apply` 先 `nft -c -f` 预检，失败以可操作的中文错误显式失败。
- **`nft` 不保证预装**（2026-09-16 实测：`bwg-rick` Ubuntu 26.04 **无** `nft`、`bwg-tizi` 有 1.1.6；Ubuntu 云镜像不预装 nftables 包）：`bui install` 与 `bui upgrade` 缺 `nft` 时**硬性拒绝**（退出码 2、不落盘、按包管理器打印安装命令），**不自动装包**；`bui upgrade` 由旧二进制执行，4.0.1→4.1 那一跳进程内拦不住，由演练脚本 preflight 与 T19 闸门守。`which nft` 为假时自检 FAIL、watchdog 告 Error 级 `nft_missing`，**此时住宅 HY2 对所有带 `mport` 的客户端等于全断**（客户端只往 `41000-50000` 发、从不发 `40000`），不是「只是跳跃失效」。退路（不进本期）是改用 iptables-nft 自建链。
- **`ip_local_reserved_ports` 同步收口**：`modules/system.rs` 的 `reserved_ports(p, slots)`（`:37-71`）今天把「住宅基础端口段（每槽一个）+ 住宅跳跃段」都写进 sysctl，签名同样从 `slots: u16` 改成 `compat: bool` ⇒ 单端口 `40000` +（兼容段开着时）`40001-40007` + 整段 `41000-50000`。它与 `firewall_ports` 是 `slots::slot_span` 的**唯一**两个消费者（调用点 `system.rs:119`、`system.rs:186`、`serve.rs:136`），两者收口后 `slot_span` 无人使用 ⇒ 随 `hop_slice` 一起删（§4.2）。
- **为什么不让 sing-box 自己多端口监听**：它没有这个能力，`server_ports` / `hop_interval` 都长在**出站**（客户端）上（https://sing-box.sagernet.org/configuration/outbound/hysteria2/ ，评估 `sb.facts[5-hop字段只在出站(客户端)]`）。服务端对「客户端换源端口」的容忍由 sagernet/quic-go 的 `DisablePathManager` 保证（注释原文 "for hysteria2 port hopping, direct change remote address without connection migration logic"，v0.61.0-sing-box-mod.7 `interface.go`）；本机 PoC 里客户端 `hopInterval: 5s` 18 秒换了 4 个源端口、服务端同一会话 18/18 成功，tizi PoC 又在真 nft + 真证书 + 公网域名下复验了两个随机端口。

### 2.5 单元名沿用 `hysteria-residential` 的理由

进程换了内核，**单元名不换**——名字是八处硬编码的锚点，换名字等于同时改动这八处的行为与验收：

| 依赖点 | 位置 | 沿用后的效果 |
|---|---|---|
| 哨兵把它当内核单元 | `sentinel/signature.rs:160` `unit.starts_with("hysteria-")` | `kernel_crash_loop` / `kernel_bind_in_use` 判据继续覆盖它 |
| 证书轮换错峰重启 | `modules/certs.rs:133-138`（固定数组 + `RESTART_GAP_SECS`，`certs.rs:14`） | 代码一行不改 |
| watchdog 目标 | `modules/watchdog.rs:180-205` | 只需把「按槽枚举」改成一条（§8.2） |
| 受管单元集合 | `reconcile/mod.rs:13-20` `MANAGED_UNITS` 六个固定名字 | 数量回到 6，`managed_units` 退化为常量表 |
| m3 验收 | `scripts/m3-acceptance.sh:54,73-76` `KERNEL_UNITS` | 回到固定三项，删按槽枚举（`:67`、`:1306-1310`） |
| 升级演练 | `scripts/ops/upgrade-drill.sh:23` `UNITS` | 不改 |
| m1 验收 | `scripts/m1-acceptance.sh:417`（按槽拼单元名） | 删按槽分支 |
| 面板 / API 白名单 | `reconcile::is_managed_unit`（`reconcile/mod.rs:57-68`） | 继续认它 |

`hysteria-residential-1..7` 进 `LEGACY_UNITS`（`reconcile/mod.rs:24-34`，9 项 → **16 项**）：对账把它们 stop + disable + 删单元文件 + 删配置（`config-residential-<i>.yaml` 产 `Absent`），漂移扫描对「删不掉还在的」再报一次。`is_managed_unit` 从此对带后缀的名字返回 false，`resi_unit()` 删除。守门测试「`MANAGED_UNITS` 与 `LEGACY_UNITS` 不相交」（`reconcile/mod.rs:531-537`）继续成立——进遗留列表的是带 `-<i>` 后缀的名字，不是 `hysteria-residential` 本身。

**新单元正文**（`units.rs` 的 `resi_unit_text(index)` 改为无参 `resi_unit_text()`）：照今天 relay 单元的形状（`units.rs:244-265`，同款 sing-box 进程），`ExecStart={bin}/sing-box run -c {base}/hy2-residential.json`、`ExecStartPre=-{bin}/bui nft apply`、`After=network-online.target b-ui-relay.service`（保留：出站指向 relay）、`Restart=always` / `RestartSec=3`、`LimitNOFILE=1048576`、`Nice=-5`、`LogRateLimitIntervalSec=10s` + `LogRateLimitBurst=200`（relay 已有，对 §11 #10 的 `deny` 噪音正好有用）。内存沿用住宅档 `MemoryHigh=300M` / `MemoryMax=500M`（`units.rs:339-340`，实测 65 MB 有充足余量）；删掉 apernet 专属的 `Environment=HYSTERIA_LOG_LEVEL=warn` 与 `GOMEMLIMIT=200MiB`（**裁决：sing-box 不设 `GOMEMLIMIT`**，与 relay 今天一致，§14 裁决 5——sidecar 实测 65 MB，单元已有 `MemoryHigh=300M` / `MemoryMax=500M` 兜底；apernet 那条 200MiB 是它自己的 GC 口径，照抄没有依据），删掉 `LimitNPROC=512`（sing-box 线程模型与 apernet 不同，按 relay 档走）。

---

## 3. 凭据池与门控

### 3.1 凭据命名与池大小

`state.residential.hy2_pool.creds[]`，每项 `ReservedCred { id, name, secret, released_at }`：

- **`id`** = `r` + 三位十进制（`r000`…`r255`，id 域随 `POOL_MAX = 256` 收，§14 裁决 2）。**ASCII、稳定、只在本机内部用**：门的 tag `gate-<id>`、Clash API 路径、日志匹配都按它——生产用户名是中文，不能进 URL 路径。
- **`name`** = sing-box `users[].name`，也是 `auth_user` 匹配值与 `stats.users` 的计数键。**迁移用户 `name = username`**（保住订阅逐字不变，§4.3），**新发凭据 `name = id`**。生成时跳过与现有用户名 / 凭据名冲突的值（`validate_username` 允许 `r017` 这种名字，`panel/users.rs:168`）。
- **`secret`**：迁移用户 = 当时的 `credentials.hy2_password`（复制一份，之后各走各的）；新发凭据 = 16 字节随机 base64url 无填充（22 字符，**不含 `:`**）。
- **`password`（写进 sing-box）= `"{name}:{secret}"`**。这个形态是实测出来的：sing-box 的 `users[].password` 就是客户端发送的**整个 auth 串**，写 `p1` 握手 404、写 `r00:p1` 才通（本机 PoC `sb1.json` + `hyc.yaml`；官方文档明写不提供 `userpass` 别名；tizi PoC 的配置同样是 `password:"r000:<pw>"` 并握手成功）。于是 `hysteria2://name:secret@host:40000?…` 的 URI 形态、以及三处 `"password": format!("{username}:{password}")`（`render/subscription.rs:208` sing-box 订阅、`:411` clash YAML、`render/client.rs:331` 客户端）**一个字都不用改**。
- 用户侧指针：`User.credentials.hy2_resi_cred: Option<String>` = 凭据 `id`。一个凭据最多属于一个用户；`hy2_password` **保留给直连**（§4）。

**池大小 = 2 × 用户上限**：`size_for(n) = clamp(ceil16(2 × n), POOL_MIN = 32, POOL_MAX = 256)`，`n` = 有住宅权益且开 hysteria2 的用户数（仓库里没有独立的用户上限常量，用户数就是基数；容量始终 ≥ 2 倍，留一倍给增长）。**上限 256 是裁决值**（§14 裁决 2）：当前住宅 HY2 用户个位数，2× 上限远够；128 凭据 + 128 门 + 128 规则的形状本机实测内存不涨、`check` 0.04 s，256 离这个已实测点只差一倍，而早先写的 512 属于**未实测的推断上限**。空载 RSS 与 `check` 耗时按 256 复核余量（§12 第 8 项）。

- 空闲 < 20% ⇒ 哨兵一次性告警 `hy2_resi_pool_low`（恢复即清）。
- 空闲耗尽时建用户**不拒绝**：当场扩容（重写 + 重启，全体住宅 HY2 会话重连一次）并记 Error 级事件；正常路径是运维在告警阶段用 `bui residential pool grow [--to N]` 择时扩容。
- **每次因 §3.5 四件事重写配置时，顺带把全部空闲凭据的 `secret` 重新随机**（`released_at` 清空）：空闲凭据的旧密码只有前任持有人知道，重启是唯一能换掉它的时机。
- **回收**：删用户 / rotate 释放的凭据先切门到 `deny`、记 `released_at`；再分配时优先「从未用过」的，其次 `released_at` 最早且 ≥ 24 小时的；池里全是 24 小时内释放的 ⇒ 视同耗尽 → 扩容。

### 3.2 门（selector）与规则的契约

- 每个凭据一个 selector `gate-<id>`：成员 `["deny", "slot-0-out", …, "slot-7-out"]`（8 个槽预声明，与 `slots::MAX_SLOTS = 8` 同源）、**`default: "deny"`**、**`interrupt_exist_connections: true`**。
- 每个凭据一条路由规则 `{"auth_user":["<name>"],"outbound":"gate-<id>"}`——**每凭据一条**是刻意的：`/connections` 的 `rule` 字段是唯一能把连接归到用户的线索（§5.2），合并规则就丢了这个线索。
- `route.final = "deny"`（理论上不可达：auth 串不命中时 hysteria2 入站走 masquerade，不进路由）。
- `default: "deny"` 是**到期语义的实现**，也是 fail-closed 的地基：tizi PoC 验证过「默认门 deny 的用户握手成功、请求全 000」。
- `interrupt_exist_connections: true` 与 relay 的 `slot-<i>-pool` 固定 `false`（`render/relay.rs:283,297`）**刻意相反**：relay 那边切换是巡检行为（不能掐 AI 登录会话），门这边切换只发生在**用户自己的**生命周期动作上（到期 / 封禁 / 换槽 / 踢），掐断正是目的。

### 3.3 用户生命周期 → Clash API 对照表

Clash API 客户端复用 `modules/residential/clash.rs` 的 `Clash` trait（`ready` / `selected` / `select`，`clash.rs:19-28`），实例地址换成 `HY2_RESI_CLASH_API`；新增 `selected_all() -> BTreeMap<selector, now>`（`GET /proxies` 一次读全部门位，避免每凭据一次 GET）。

「期望门位」是纯函数 `expected(&State, &BTreeSet<Uuid> blocked) -> BTreeMap<cred_id, tag>`，放在新文件 `crates/bui/src/modules/panel/gates.rs`（只是 panel 下的一个文件，**不是**新的 `Module` 实现）：用户存在、不在 `blocked_set`（`panel/users.rs:399-412`：disabled / `expires_at` / `traffic_limit`）、有住宅权益且开 hysteria2 ⇒ `slot-<index_of_user>-out`；其余凭据（含全部空闲）⇒ `deny`。

| 动作 | 期望态改动 | Clash API | 客户端感知 |
|---|---|---|---|
| 建用户（住宅权益 + hysteria2） | 分配凭据（§3.1 顺序）+ `assign_least_loaded` | `PUT gate-<id> → slot-<i>-out` | 立即可连，配置不动、不重启 |
| 到期 / 禁用 / 超限 | `blocked_set` 判拒 | `PUT gate-<id> → deny` | 握手仍成功、流即刻被拒（§6） |
| 续期 / 解禁 / 额度重置 | 反向 | `PUT gate-<id> → slot-<i>-out` | 不必重连，下一个请求即通 |
| 换槽（`assign` / `rebalance` / 删上游后的 `migrate_unassigned`） | `slot_id` 变 | `PUT gate-<id> → slot-<j>-out` | 既有流被掐（换出口 IP 本就该断），QUIC 会话保持 |
| rotate（`POST /api/users/{u}/rotate`，`api_admin.rs:239-300`） | 换 `hy2_password`（直连）/ `vless_uuid` / `sub_token`；住宅：释放旧凭据、分配新凭据 | `PUT gate-<旧> → deny`、`PUT gate-<新> → slot-<i>-out` | 旧订阅的住宅 HY2 握手成功但全拒；直连按今天（握手即拒）；刷新订阅后拿到新凭据 |
| kick（`POST /api/kick`、限额 `newly_blocked`，`panel/traffic.rs:204-213`） | — | 已封者已在 `deny`；未封者手动踢 = `PUT deny` 再 `PUT slot-<i>-out`（各掐一次既有流）+ 对按 `rule` 筛出的每条连接 `DELETE /connections/{id}` | 既有流断、QUIC 会话不断、未封者下一请求即通 |
| 删用户 | 释放凭据 | `PUT gate-<id> → deny` | 同到期 |
| 删上游（槽消失） | `sync_slots` 释放槽 + 用户重分配 | 受影响用户各一次 `PUT` | relay 照今天重启一次（`core_files.rs:250-256` 无 `restart_key`）；hysteria 配置不变、**订阅不变** |
| 加上游 | 新槽 | 无 | 无 |
| 凭据池扩容 / obfs 开关 / 证书轮换 / 端口改动 | 重写配置 | 重启单元 → §3.4 重放 | 全体住宅 HY2 会话重连一次（与今天 obfs 开关 / 证书轮换的影响面相同） |

**收敛入口只有一个**：`panel::users::sync_users`（`users.rs:465`）增加「门位收敛」段——算期望门位、`selected_all()` 读真源、只对差集 `select`，与它旁边那段 xray 收敛（读内核 → 差集 AddUser/RemoveUser）同构；`StateChanged` 与 60 秒安全网（`SYNC_INTERVAL_SECS`，`users.rs:25`）两条触发路径不变。任一 PUT 失败进 `SyncOutcome.errors` → 打 `USER_SYNC_FAILED_LOG`（`users.rs:29`）→ 哨兵新签名 `hy2_resi_gate_sync_failed`（§8.1）。`/api/users` 的投影增加 `hy2ResiGate`（`slot-3-out` / `deny` / `未分配`），面板与 `bui residential slots` 按槽列用户时读它。

### 3.4 重启后的门位重放与 fail-closed

**裁决：不开 sing-box 的 `cache_file`，保持 fail-closed**（§14 裁决 1）。sing-box 重启后每个 selector 回到 `default` = `deny`，由 b-ui 重放真实门位。**理由**：可用性包线与今天 `auth_http` 相同（b-ui 不在时 HTTP 钩子同样全拒），而「陈旧放行」会让到期 / 封禁用户在 b-ui 掉线期间照常上网，比短暂拒绝更糟。

- **为什么不用 `cache_file`**（relay 今天靠它跨重启保留选择）：门是**授权决定**，「陈旧但放行」比「短暂拒绝」更糟——b-ui 停机期间被封的用户，重启后会被缓存恢复成放行。不开缓存 ⇒ 重启即 fail-closed，方向与今天一致：`auth_http` 的快照读不动时「退化成空快照 ⇒ 谁都进不来（fail-closed）」（`panel/auth_http.rs:88-94`）。
- **代价**（明确记下）：b-ui 不在时 sing-box 重启一次，住宅 HY2 就全员被拒，直到 b-ui 回来重放。这与今天的可用性包线**相同**——今天 b-ui 不在时 `127.0.0.1:18789` 没人应答，住宅 HY2 握手同样全失败。
- **重放**：`apply` 重启 `hysteria-residential` 后发新事件 `Event::Hy2ResiRestarted`（对照 `Event::RelayRestarted`，`api/state.rs:10-14`），订阅者立即跑一轮门位收敛；Clash API 未就绪时按 relay 现成的退避重试（`select_with_retry`，`residential/health.rs:993`，预算 `REPLAY_RETRY_BUDGET = 15 s`，`health.rs:923`）。
- **失败 = fail-closed + 告警**：预算用完仍有差集 ⇒ 没重放到的门停在 `deny`，记事件 + 告警 `hy2_resi_gate_replay_failed`（`bui status` 一行 + 面板），60 秒安全网继续收敛，成功即清（对照 `REPLAY_FAIL_ALERT` 的清理，`health.rs:1125-1135`）。

### 3.5 配置重写与重启的契约

`hy2-residential.json` 是 `Artifact::File` + `verify(Verify::SingBox)` + `restart(Unit::restart("hysteria-residential"))`，**不用 `restart_key`**——文件内容本身就与用户无关：只含凭据池、8 个槽出站、门、规则、两个回环端点、obfs、证书路径、监听端口。它只会因**四件事**变化：

1. **凭据池扩容**（含 §3.1 的空闲密码重随机，只在重写时顺带做）；
2. **`bui set obfs on|off`**（`node.obfs`）；
3. **证书轮换**（`certs.rs:131-151` 那次重启，单元名没变所以代码不动；sing-box 是否会热加载证书从而免掉这次重启 → §12 第 9 项）；
4. **`ports.hy2_resi` / `hy2_resi_hop` 改动**（`POST /api/config/port-hopping`，同时重写 nft 表）。

用户的任何生命周期动作**不许**触碰本文件。守门测试写进 `core_files.rs`：对同一 state 增删用户、换槽、置到期，渲染出的 `hy2-residential.json` 字节**逐字相等**（形状照 `core_files.rs:517-535` 那条「加用户改内容但不改 restart_key」的现有测试）。

---

## 4. 状态模型与迁移

### 4.1 新字段（全部 `#[serde(default, skip_serializing_if = …)]`，旧 state 直接可读，与总纲 D10 同风格）

```jsonc
// state.residential
"hy2_pool": {
  "creds": [
    { "id": "r000", "name": "alice", "secret": "<hy2_password 的副本>" },
    { "id": "r001", "name": "r001",  "secret": "<22 字符随机>", "released_at": "2026-09-20T00:00:00Z" }
  ],
  "generation": 3          // 每次重写 hy2-residential.json 递增，日志/事件用它指称「第几代池」
}
// state.users[].credentials
{ "hy2_password": "…", "vless_uuid": "…", "hy2_resi_cred": "r000" }
// state.system
{ "hy2_resi_compat_ports": true }   // 兼容段 40001-40007 是否 REDIRECT + 放行；缺省 true
```

类型：`Hy2Pool { creds: Vec<ReservedCred>, generation: u64 }`、`ReservedCred { id: String, name: String, secret: String, released_at: Option<String> }`（`SystemSettings` 加 `hy2_resi_compat_ports: bool`，`#[serde(default = "default_true")]`，`model.rs:493-530` 那一族）。纯函数集中在新模块 `bui_schema::hy2pool`：`size_for` / `grow` / `assign` / `release` / `cred_of` / `regenerate_idle_secrets`。

### 4.2 与 slots / entitlements 的关系

- `entitlements.residential.slot_id`（用户粘的那个 IP）语义**不变**；`Slot { index, upstream_id }`、`sync_slots` 三条不变量、`least_loaded` / `assign` / `rebalance` / `migrate_unassigned` 全部保留——它们现在只驱动 relay 入站、xray 规则与门位。
- `SlotRes` 收缩为 `{ index, relay_port }`：`hy2_port` / `stats_port` / `hop` 三个字段，连同 `hop_slice`、`slot_span`、`HY2_STATS_RESI_BASE`（`slots.rs:19`）与 `resources_of` **一起彻底删**（连签名一起，不留 `#[deprecated]`、不留转发壳、不留兼容层，§14 裁决 6：留一个还能编译的旧签名，下一个人就会继续按槽算端口——而 4.1 之后端口与槽无关，那正是回归事故的入口），只留 `resources(index) -> SlotRes`。`indices` / `sorted` / `fallback_index` / `index_of_user` / `sync_slots` / `least_loaded` / `assign*` / `migrate_unassigned` / `users_of_slot` **保留原签名**（relay、xray 与对账枚举仍按槽走）。**全部调用点**（漏一个就编译不过或显示错端口）：

| 调用点 | 今天用它做什么 | 4.1 |
|---|---|---|
| `render/xray.rs:62-63` | 每槽一个 socks 出站，取 `relay_port` | 改调 `resources(i)`，行为不变 |
| `nodes.rs:140` | 住宅 HY2 节点的端口与区间 | 删（端口与区间直接取 `ports`，§7.1） |
| `panel/users.rs:99-102` | 面板显示该用户的住宅 HY2 端口（注释说「端口口径与 `nodes_for` 完全一致」） | 端口固定显示 `ports.hy2_resi`；槽位 / IP 投影不变，另加 `hy2ResiGate`（§3.3） |
| `residential/api.rs:334` `slot_rows` | `status` / `health` / `GET /slots` / CLI 共用的**唯一**槽位投影 | 行里不再有 HY2 端口；`relay_port` 改调 `resources(i)` |
| `panel/traffic.rs:39-41` `resi_stats_ports` | 住宅 trafficStats 端口表 | 整个函数删（§5.1） |
| `watchdog.rs:186-190` | 逐槽 UDP 探测口 | 收成一条（§8.2） |
| `core_files.rs:207-221` | 逐槽渲染 apernet 配置 + 空槽产 `Absent` | 换成一份 `hy2-residential.json`；`Absent` 覆盖全部 `config-residential*.yaml` |
| `units.rs:82` | 逐槽渲染住宅单元 | 只渲染一个单元 |
| `panel/mod.rs:45` `HY2_STATS_PORT_RESI` | 住宅 trafficStats 基准端口 | 删 |
| `golden_subscription.rs:298,306`、`core_files.rs:858` | 按槽断言端口与区间 | 改成「8 槽下每人端口与区间完全相同」 |
| `lib.rs:33,36-37` 模块文档 | 描述端口换算与 `resubscribe_impact` | 按本文改写 |

  **`resources_of` 到收口（plan T15）时只剩四处活调用点**：`render/xray.rs:62-63`、`panel/users.rs:99-102`（面板显示端口）、`residential/api.rs:334`（`slot_rows`），以及 `slots.rs` 自己的两个单测（`a_single_slot_keeps_the_v3_ports_exactly` 与 `three_slots_split_the_hop_range_into_contiguous_slices`，改写成只断言 `relay_port`；第三个用例 `a_hole_in_the_index_space_leaves_its_slice_idle` 随 `hop_slice` / `slot_span` 一起删）。上表其余调用点在 T7 / T8 / T10 / T12 就已经不再调它——收口时 `cargo check` 仍报到它们，说明前序任务漏了。

- **「必须重新获取订阅」这一整套机制退役**（订阅不再含槽位信息，这个问题不存在了）：`slots::{resubscribe_impact, ResubscribeImpact}`、`residential/slots.rs:38-73` 的 `impact` 字段与它的计算、`upstream.rs` 的删上游返回值与 `impact_title` / `impact_groups` / `format_impact_line`（`:316-462`）、事件签名常量 `PORT_CHANGED_SIG = "resi_slot_port_changed"` 与动作 `NOTIFY_RESUBSCRIBE_ACTION = "notify_resubscribe"`（`upstream.rs:20-22`，事件在 `:405-415` 推入 `incidents`）、两个删上游端点回包里的 `port_changed`（`residential/api.rs:1105-1121`）、CLI 的读取与渲染（`residential/cli.rs:680-686`）、前端 `web/app.js:896-932` 的三组渲染（记入 CHANGELOG）。
  **注意**：`resi_slot_port_changed` **不是** `sentinel::signature::Sig` 的成员（那个枚举的 8 个成员里没有它），所以哨兵的签名表不因它而改——删的是 `upstream.rs` 里那两个常量与推事件的那一段。
- 不变量 3「池非空 ⇒ 槽 0 存在」继续成立（relay 的 `2080` 与 xray 的 `relay-slot-0` 仍是兼容面），但它保护的四个名字里 `40000` / `9998` / `hysteria-residential.service` 三个**不再与槽 0 绑定**：`40000` 归整个入站、`9998` 消失、单元名归入站。`slots.rs` 的模块文档与 `m1-acceptance.sh:328` 的措辞同步改。

### 4.3 v4 → 4.1 迁移：一次性分配，无迁移块

守护进程启动时跑 `hy2pool::migrate_on_start`（形状照 `residential/slots.rs:90-106` 的同名函数；幂等、零变更不写盘、日志只写个数不写凭据）：

1. `hy2_pool.creds` 为空 ⇒ 给每个「有住宅权益且开 hysteria2」的用户建凭据 `{ id: r%03d（按 created_at 顺序）, name: username, secret: hy2_password }`，写回 `hy2_resi_cred`；再补新凭据到 `size_for`。
2. 已有池但某用户没有 `hy2_resi_cred`（升级后新建、或 4.0.x 回滚再升）⇒ 从空闲里分配。
3. **直连仍用 `hy2_password`**：它留在 `credentials` 里，继续喂 `auth-snapshot.json` 与直连 apernet 的 http 鉴权，迁移**不动它**；住宅侧用的是它在 `hy2_pool` 里的那份副本，两者此后各自独立（rotate 换直连密码不会顺带改住宅凭据，反之亦然——这一点写进 CLI 与面板文案）。
4. `bui import-v3` 在导入完成后调同一个函数，所以 v3 → 4.1 直装的用户凭据也是 `username:hy2_password`。

于是**迁移用户的住宅 HY2 节点与今天逐字相同**（`alice:<hy2_password>@example.com:40000?…&mport=41000-50000`），P0 的 v3 golden（`crates/bui-schema/tests/fixtures/v3/expected/` 三种订阅）继续逐字节通过——这也是升级零刷新（§7.3）的凭据侧前提。

---

## 5. 计量与发布链

### 5.1 按用户流量：v2ray_api `StatsService.QueryStats(reset=true)`

为什么必须自建：官方二进制不含 `with_v2ray_api`（评估 `sb.facts[4-官方release二进制默认不带v2ray_api]`），而 Clash API 的 `/connections` 只给**未关闭**的连接、`closedConnections` 只被 libbox/daemon 消费、不经 HTTP 暴露（评估 `ev.key_facts[9]`）⇒ 轮询会漏掉两次采样之间开闭的短连接、限额执行偏松。主理人已接受自建，所以计量走 gRPC：

- 配置 `experimental.v2ray_api.stats.users` = **全部凭据的 `name`**（这是白名单，不在表里的用户不计——评估 `sb.facts[4-v2ray_api用户计量覆盖hysteria2]`）；池静态 ⇒ 这张表也静态，不因用户变更改文件（与 §3.5 一致）。
- `panel::traffic::sample_once`（`traffic.rs:55-90`）：**直连不变**，仍打 `hy2.rs` 的 `/traffic?clear=1` + `/online`（`stats_ports` 收成 `[9999]`、`resi_stats_ports` 删除，`traffic.rs:38-51`）；**住宅改为** `shared.hy2resi().query_user_deltas()`，即 `QueryStats(patterns=["user>>>"], reset=true)`（**2026-09-16 订正**：上游用重复字段 `patterns`，单数 `pattern` 留空；写成 `pattern=` 会让语义变成每轮清零全部计数器），计数器名 `user>>>{name}>>>traffic>>>uplink|downlink`，`reset=true` 原子交换清零——与 Xray 那条路完全同构（`panel/xray.rs:59` `STATS_PATTERN`、`:172` `parse_user_counter`、`:301-303` 的 `reset: true`，uplink 计 tx / downlink 计 rx）。返回键是凭据 `name` ⇒ 经 `hy2_pool` 换成 `user_id` 再进 `Sample.deltas`。
- **proto 要另 vendor**：sing-box 的 v2ray_api 实现的是 `v2ray.core.app.stats.command.StatsService`，与我们已 vendor 的 Xray `xray.app.stats.command` **包名与 gRPC 方法路径都不同**，得另生成一份 tonic 客户端；**包名以源码为准**（§12 第 3 项）。
- 计数器不跨重启存活（评估 `sb.facts[4-计数器不跨重载存活]`）：10 秒采样 + 30 秒落盘的现套路下最多丢一个采样周期，与 apernet trafficStats 内存计数器的丢失窗口相同，不新增降级。

### 5.2 在线与踢人：`/connections` 的 `rule` 字段 + `DELETE`

- `GET /connections` 每条含 `rule` 字段（值是 `F.ToString(c.Rule, " => ", c.Rule.Action())`），PoC 实测形如 `auth_user=r000 => route(gate-r000)`，`chains` 含门 tag（评估 `ev.key_facts[7]`）。**在线数** = 按 `rule` 里的 `auth_user=<name>` 归组的连接条数（`> 0` 即在线），并入 `Sample.online`。`metadata` 里**没有** user 字段（评估 `sb.facts[4-clash_api连接列表不带用户名]`），只能靠这个串——这正是「每凭据一条规则」的理由（§3.2）。
- **踢单条**：`DELETE /connections/{id}`。**踢用户** = 门切 `deny`（`interrupt_exist_connections` 掐既有流）+ 逐条 DELETE 兜底。`GET /connections` 是现取快照，10 秒采样够用，不上 websocket。
- 新 trait `Hy2ResiApi`（`panel/mod.rs` 里与 `XrayApi` / `Hy2Api` 并列）：`query_user_deltas` / `connections` / `close_connection` / `selected_all` / `select`；`fakes.rs` 补 `FakeHy2Resi`。`panel/traffic.rs:592-593` 那条「两个实例都要 kick（`kick:9999` + `kick:9998`）」的断言改成「直连 kick:9999 + 住宅 gate→deny」。
- 直连的 `Hy2Client`（`panel/hy2.rs`）保留，只服务 `9999`。

### 5.3 自建 sing-box 的**范围**：只有随发布分发的那一个

| 用途 | 来源 | 是否自建 |
|---|---|---|
| 随发布分发的 `sing-box`（relay + 住宅 HY2 入站 + `bui-c` 客户端，C4 只有一组 `sing-box-linux-<arch>`） | `kernels.lock` 的 `sing-box target` 两行 | **自建**（为了 `with_v2ray_api`） |
| CI 里跑 `sing-box check` 的 1.12 / 1.13（`SINGBOX_CHECK_MINORS="1.12 1.13"`，`kernel-versions.env:10`） | 上游归档 | **不自建**，继续用官方二进制 |

一份二进制三个用途：自建版是官方标签的**超集**，客户端行为不变，只是体积略增（§12 第 6 项记差值）。

### 5.4 CI 改动：轨道 → 锁 → 构建，失败则保持上一版并告警

**直接回答主理人的追问：上游出新 patch 不需要人工重建。** 现有机制一个不改：

1. **轨道不变**：`kernel-versions.env` 的 `SINGBOX_TRACK="minor:1.14"`（`:9`）；新增 `SINGBOX_TAGS`（构建标签集）。
2. **锁多一种来源**：`kernels.lock` 的 `sing-box target` 两行 URL 列改成 `build:SagerNet/sing-box@v1.14.1;go=go1.25.4;tags=<逗号分隔>`，sha256 列是**我们构建出来的裸二进制**的 sha256（今天是上游归档的 sha）。`sing-box check` 那两行不动。
3. **构建脚本** `scripts/release/build-singbox.sh --version <v> --arch amd64|arm64 --out <path>`：`git clone --depth 1 -b v<v>` → `GOTOOLCHAIN=auto`（按上游 `go.mod` 取精确 Go 版本，记进锁的 `go=`）→ `CGO_ENABLED=0 GOOS=linux GOARCH=<arch> go build -trimpath -tags "$SINGBOX_TAGS" -ldflags "-s -w -buildid= -checklinkname=0 -X …constant.Version=<v>" ./cmd/sing-box`（写法与 `-checklinkname=0` 的要求见 https://sing-box.sagernet.org/installation/build-from-source/ ）。Go 原生交叉编译，arm64 不需要容器。CI 要新增 `actions/setup-go`（今天没有）并钉死 Go 版本。
4. **标签集不凭记忆写死**：构建 job 顺手下载同版本官方归档，`sing-box version` 打印的 `Tags:` 必须 ⊆ 我们二进制的 `Tags:`，且后者含 `with_v2ray_api`，不满足即红（§12 第 4 项：首次构建时把实际标签集写进 env）。
5. **可重现**：`-trimpath` + `-buildid=` + 精确 Go 版本 + 无 cgo ⇒ 同输入两次构建 sha256 相同（§12 第 5 项：首次验一次，之后靠锁校验）。`scripts/ci/fetch-kernels.sh` 认 `build:` 前缀：缓存里 sha 相符就用，否则调 `build-singbox.sh` 后比对锁的 sha，不符退 4（与今天下载 sha 不符同码，`fetch-kernels.sh:104-109`）。`setup-kernels` 的 `actions/cache` 键已含 `hashFiles('scripts/release/kernels.lock')`（`action.yml:26`），命中即零构建。
6. **`pin-kernels.sh --write`（自建模式）**：解析轨道 → 对两个架构各跑一次构建算 sha → 写锁；`--check` 不变（只比版本号）。
7. **自动跟进** 新工作流 `.github/workflows/kernels-bump.yml`（每周 cron + `workflow_dispatch`）：`pin-kernels.sh --write` → 锁有变化才继续 → 用新内核跑 `cargo test`（含渲染结果的 `sing-box check` 矩阵）→ 开 PR `chore(kernels): sing-box 1.14.1 → 1.14.2`。
8. **失败语义（硬要求）**：上游 clone 失败、构建失败、`Tags` 不达标、sha 不可重现、`check` 不过——**任何一步失败 ⇒ 工作流红、锁不动、不开 PR**；`release.yml` 永远只用锁里的版本，所以**发布保持上一版**，GitHub 的失败通知就是告警。**绝不因上游改动阻塞发版**。正式发版仍由主理人打 tag。
9. **`release.yml` / `ci.yml` 改动点**：两个 job 的 `setup-kernels` 自动走 `build:`；`release` job 里「把上游内核以裸二进制归集进 dist」那步对 sing-box 改取构建产物（12 个资产的清点不变，`release.yml:211-217`）；加断言 `dist/sing-box-linux-amd64 version` 的 `Tags` 含 `with_v2ray_api`（arm64 不能在 x86 runner 执行，靠锁 sha）。`ci.yml` 的 `test (1.12/1.13/1.14)` 三格保留（订阅 / relay / 客户端的兼容门槛）；`hy2-residential.json` 的 `check` 只在自建 1.14 上跑，二进制不带 `with_v2ray_api` 时该用例 `skip`，并把这种 skip 加进 `ci.yml:130-131` 的白名单（「内核校验漏跑判红」的口径不变）。
10. **manifest（C4）形状不变**，`kernels.sing_box` 仍是纯版本号；可选附加字段 `builds: { "sing-box": { "source": …, "go": …, "tags": … } }`（消费方忽略未知字段；**2026-09-16 裁决：本期不做这个字段**，没有消费方）。**同版本不同构建靠 `artifacts.sing-box-linux-<arch>.sha256` 识别**：对账、`bui upgrade`、`bui install` 三处按「版本不同**或** sha 不同即重装」判定，否则自建二进制在已装官方同版本的机器上永远装不上，住宅 HY2 会陷入永久崩溃循环（每个内核 × 每个架构的 sha 都在 manifest 里，`Manifest::kernel_asset` 按 `<name>-linux-<arch>` 查得到）。`scripts/tests/test-pin-kernels.sh` / `test-fetch-kernels.sh` / `test-manifest.sh` 按 `build:` 行更新，**测试禁网**：`build-singbox.sh` 用 PATH 前置 stub。

---

## 6. 鉴权语义变化与面板文案

- **今天**：到期 / 封禁 / 超限 / rotate 后的旧密码在**握手**即被拒（`auth_hook::decide`：常量时间比密码 + `blocked` + `expires_at`，fail-closed），客户端显示连不上。
- **4.1 住宅**：密码静态在 `users[]` 里，**握手成功**，`metadata.User` 落到门 `deny` ⇒ 每条流 `connection refused`。客户端显示「已连接」但所有请求失败。**tizi PoC 已验证**：默认门 deny 的用户握手成功、请求全部 HTTP 000。直连 HY2 与 Reality 不变。
- **面板与 CLI 文案**（用户列表状态 tooltip + `bui status` 用户段）：「已停用 / 已到期：**住宅 HY2 客户端仍会显示已连接，但所有请求会被拒绝**；直连节点在连接时即被拒绝。」`docs/residential-proxy-guide.md` 同步一段；rotate 的回包提示加一句「住宅 HY2 的旧凭据会在刷新订阅前一直显示已连接但不通」。
- **`auth-hook.log`**：住宅路径**不再有任何记录**——sing-box 对 hysteria2 鉴权失败不打任何日志（评估 `sb.facts[7-鉴权失败无日志]`，tizi PoC 复验「鉴权失败仍无日志」）。文件保留给直连，格式不变。住宅侧排查改看 `journalctl -u hysteria-residential`：tizi PoC 实测日志**带用户名**——`inbound/hysteria2[hy2-resi]: inbound connection from <ip>:<port>` 与 `[r000] inbound connection to <host>:<port>`，比 apernet 更好定位。
- **`bui set hy2-auth http|command`**：只重渲染直连 `config.yaml`、只重启 `hysteria-server`；CLI 帮助与 `bui status` 的「HY2 鉴权」行注明「仅直连」。
- **m1 判据变化**（`scripts/m1-acceptance.sh`：`hy2_auth_probe` 在 `:219`、`check_hy2_auth` 在 `:278`、住宅探测调用在 `:305`）：直连探测不变（bundled hysteria 客户端 → https 200 → `auth-hook.log` 末行 `allow`，`:215`）；**住宅探测改为**用该用户的住宅凭据（`name:secret`）连 `127.0.0.1:40000` → https 200，且 `journalctl -u hysteria-residential --since <探测起点>` 出现 `[<name>] inbound connection`；再加一步带 `mport=41000-50000` 打回环，验 nft 的 **output** 链。
- **m3 判据变化**（`scripts/m3-acceptance.sh`）：① 加用户 → 三个内核单元 `NRestarts` / `MainPID` 不变（`KERNEL_UNITS` 回固定三项，删 `:67`、`:73-76`、`:1306-1310` 的按槽枚举）且邻居会话不断；② 100 MB 计数误差 ≤ 1%（住宅经 v2ray_api）；③ 到期：**新握手仍成功**但 10 秒内经它的请求全部失败、同槽其他用户客户端 `connected` 计数不变、`/connections` 里该用户条数归零（`:828-833` 那两条读 `auth-hook.log` 的断言只对直连保留）；④ 删一个上游 → 任一用户三种订阅 sha 不变、门位重放正确（`bui residential slots --json` 里每个用户的 `gate` 与期望一致）。

---

## 7. 订阅与客户端

### 7.1 节点集合

`nodes::nodes_for` 的 `Hy2Residential` 分支（`nodes.rs:137-148`）改为：`port = node.ports.hy2_resi`（40000）、`hop = Some(node.ports.hy2_resi_hop)`（**整段**）、`Transport::Hysteria2 { username: cred.name, password: cred.secret, sni, obfs_password }`，凭据取 `hy2pool::cred_of(user, resi)`；用户尚无凭据（只可能在迁移前的一瞬）⇒ 不发该节点。`slots::resources_of` 从 `nodes.rs` 消失。函数签名 `nodes_for(&User, &NodeParams, &Residential)` **不变**（C1）。

### 7.2 三种订阅与 `bui-c`：零改动

`render/subscription.rs` 的三个渲染器与 `render/client.rs` 只消费 `Node`，**一行都不用改**：

- URI：`hysteria2://{name}:{secret}@example.com:40000?sni=…&insecure=0&mport=41000-50000[&obfs=salamander&obfs-password=…]`（`subscription.rs:137` 的 `mport`）。
- sing-box 订阅与 `bui-c`：出站 `password: "{name}:{secret}"`（`subscription.rs:208`、`client.rs:331`）、`server_ports: ["41000:50000"]` + `hop_interval: "30s"`（`subscription.rs:212-213`、`client.rs:337-338`）、`idle_timeout: "30m"`（`subscription.rs:233`）。
- Clash：`password`（`subscription.rs:411`）与 `ports` 字段同理。obfs 密码与服务端同源（`node.obfs.password`，`nodes.rs:73-77`）。

### 7.3 升级零刷新的论证

升级那一刻用户手里可能有三代住宅 HY2 节点：

| 手里那份 | 端口 | `mport` | 凭据 | 4.1 下的路径 |
|---|---|---|---|---|
| v3 / 4.0 单槽时代 | 40000 | 41000-50000 | `alice:pw` | 直达入站；整段 REDIRECT → 40000 ⇒ ✔（**这正是回归事故要修的那批**） |
| 4.0 多槽、槽 0 | 40000 | 第 0 片 | `alice:pw` | 片 ⊂ 整段 ⇒ ✔ |
| 4.0 多槽、槽 i ≥ 1 | 40000+i | 第 i 片 | `alice:pw` | `40000+i` 走**兼容段** REDIRECT → 40000；片 ⊂ 整段 ⇒ ✔ |
| 4.1 之后取的 | 40000 | 41000-50000 | `alice:pw`（迁移）或 `r017:…`（新建 / rotate 后） | ✔ |

三代都能连，出口按 `auth_user` 落到该用户此刻的槽（迁移不改 `slot_id`，4.0 里在槽 i 的人还在槽 i）⇒ **升级不需要任何人刷新订阅**，也没有「走错出口 IP」的情形（评估 `ev.key_facts[14]`）。唯一要通知的时刻是**兼容段下线**（§2.4 的 30 天零命中判据），只影响第三行那一代。这撤销了 4.0.0 的升级口径「非槽 0 用户需刷新一次订阅」（spec §5.6 规则 4）。

### 7.4 客户端侧三条硬约束（bui-c 会话按 rc1 源码 `cli.rs:515-560` 核过，原文引用）

1. > 住宅 HY2 节点的 label「HY2住宅」**不能改**：profile 名由 用户名 + label 算出，改了会新增节点、旧节点变死节点。

   ⇒ `nodes.rs:145` 的 `"HY2住宅"` 字面量**不许动**（连空格、连全半角都不许），阶段 1 加一条守门测试钉住四个 label 的字面量。
2. > 导入路径要前后一致：`--sub -` 优先走 `/api/nodes`（载荷带真实用户名）；只有回退到 `/api/sub` 时算出的名字才可能不同。端口 4000x→40000、hop 变整段时 `same_endpoint` 不命中但 `same_account` 同名沿用 → `upsert` 原地覆盖，不会出重复节点，活动节点不变。

   ⇒ 4.1 的端口与区间变化对 `bui-c` 是**原地覆盖**，不产生重复节点、不改活动节点。`/api/nodes` 的载荷仍带真实用户名（凭据换成住宅 `name:secret`，用户名字段本身不变）。
3. > `bui-c check` **不会**自动重拉订阅（check.rs/update.rs 不取节点、不存 token）⇒ 服务端换槽/改段/开混淆后 bui-c 用户要手动 `import --sub -`。B 的目标之一就是让「换槽不再需要刷订阅」。

   ⇒ 这正是 §1.2 目标 2 的价值所在：4.1 之后换槽 / 增删 IP **不再产生任何需要重导的理由**。`slots.rs` 里那句「要等下一次订阅更新（`bui-c` 的每日 timer…）」的注释**与事实不符**（`bui-c` 的 `check` 与每日自动更新都不取节点），已在 4.0.1 按事实改写成「Linux 客户端不会自动刷新节点，必须由用户手动 `bui-c import --sub -` 或菜单 [3] 重新导入」；这套机制本身随 §4.2 退役时连注释一起删。仍需人工重导的只剩两种：rotate（今天已是如此）与**兼容段下线时**那一代「4.0 多槽、槽 ≥ 1」的导入。

---

### 7.5 旧订阅兼容不变量（4.1 的硬约束，逐条可核对）

**客户端不会自动刷新节点。** `bui-c` 取节点只有两处、都在用户显式导入的路径上
（`crates/bui-c/src/cli.rs:878` 的 `import --panel` / `--sub`，与 `:3005` 的 `fetch_http`）；（**点位更精确的说法**：`fetch_panel` / `fetch_sub` 的调用点实为 6 处 —— `cli.rs:878`、`:884`、`:2881`、`:3005`、`:3010`、`:3014` —— 但**全部只可达自显式 `bui-c import` 命令与菜单 [3]**；`check.rs` 与 `update.rs` 一处不取。实施时按「入口」而非「行号」核，别只 grep 两个点位。）
每分钟的 `bui-c check` 与每日自动更新**都不取节点**，v2rayN 也只在用户点「更新订阅」时才拉。
所以**从不重新导入的机器会一直用手里那份旧参数**——§7.3 的 REDIRECT 只兜住「目的端口」这一项，
其余参数错一个，那批机器就是断网。下面四条是 4.1 的硬约束：

| # | 不变量 | 破了会怎样 | 怎么核对 |
|---|---|---|---|
| 1 | HY2 认证串仍是 **userpass 形式的 `username:password`** | 客户端就是这么发的（`crates/bui-c/src/parse/node_uri.rs:41-53`），换成别的形态 ⇒ 全部旧节点握手失败 | **已验证天然满足**：sidecar PoC 实测 sing-box 的 `users[].password` 必须写成 `名:密码` 才能与 `hysteria2://user:pass@` 互通（§3.1），4.1 的凭据池只能是这个形态；阶段 1 的 v3 golden 逐字节不变再钉一道 |
| 2 | **obfs 密码、SNI 与证书不变** | obfs 密码两侧不一致 ⇒ 握手前就被丢（`bui set obfs` 那条 4.0.1 CHANGELOG 已有先例）；SNI / 证书换了 ⇒ TLS 失败 | obfs 密码与 `nodes_for` 同源（`nodes.rs:73-77`）且有测试钉住；`hy2-residential.json` 复用生产证书路径（§2.3）；m1 住宅探测用真证书跑 |
| 3 | **`41000-50000` 与 `4000x` 的 UDP 全部转发** | 客户端每 30 秒换一个目的端口，漏一段就是周期性静默丢包 —— §1.1 回归事故的形状 | nft 四条规则（prerouting + output × 整段 + 兼容段，§2.4）、`firewall_ports`、`reserved_ports` 三处同一口径；`bui selfcheck` 带 `mport` 打回环验 output 链（§8.3） |
| 4 | **按用户粘槽的出口路由语义不变** | 同一个人换 IP 出去 ⇒ AI 站点风控当场翻脸（这正是粘槽存在的理由） | 迁移不改 `slot_id`（§4.3）；门位只由 `gates::expected` 一处算（§3.3）；m3 判据 ④ |

**验收（进 plan 阶段 4 = T19）**：用 **rc 客户端渲染出的 `config.json`**（升级前就在盘上、没有
重新导入过的那一份）去连 4.1 服务端，验通——光靠「REDIRECT 覆盖了旧端口」这一条论证不够，
四条不变量要有一次端到端的机器化证据。

### 7.6 客户端二进制通道与「不自动合并」的共同结论

三条按 rc1 / rc2 源码核过的事实（bui-c 会话核、主会话复核），实施与发版都按它们走：

1. **`bui-c` 的替换判据是「版本号或 sha256 任一不同就换」，不只是版本号**：`crates/bui-c/src/update.rs:359`
   的 `build_differs` —— manifest 里 `bui-c-linux-<arch>` 的 sha256 与盘上二进制不同就换
   （`SelfReason::Rebuild`），所以**同版本不同构建也会互换**（rc2 与 rc3 都是 `4.0.1`）。
   排 rc 顺序要把这一点算进去：两个 rc 之间来回升降会互相覆盖。
2. **决定客户端拿到哪个 `bui-c` 的是「打了哪个 tag」，不是「服务端升没升」**：面板
   `/packages/manifest.json` 发的是服务端每日自检从 GitHub 拉回的那份 manifest
   （`crates/bui/src/kernels/mod.rs:446` `pick_selfcheck_manifest`：缓存是 rc 就跟最新 rc tag，
   否则跟 `latest`）。所以**一打 4.1.x 预发布，还挂在 rc 通道的面板一天之内就会把那一版 `bui-c`
   发给客户端**。⇒ 打 4.1.x（含 rc）之前必须过发布门禁，见 plan **T20**。
3. **HY2 鉴权不看槽**：所有 hysteria 实例共用同一个 auth URL（`render/hysteria.rs:13`），
   鉴权快照里没有槽位。所以 **4.1 之前，用户手里旧槽端口的节点照样能连**，只是从**另一个住宅
   IP** 出去。据此裁定：**客户端那一版不自动合并同账号的重复节点**，只提示、或经菜单确认——
   自动合并等于替用户做了一个他看不见的出口变更（`profiles::same_account` 认得出同账号，但认不出
   「这两条出口 IP 不同」）；等 4.1 把旧端口汇到同一出口之后再考虑自动合并。这条是主会话与
   bui-c 会话的**共同结论**。

## 8. 哨兵 / watchdog / 自检改动清单

### 8.1 签名表（`sentinel/signature.rs`）

| 签名 | 4.1 处置 |
|---|---|
| `hy2_auth_http_failed`（`signature.rs:56`） | **作用域缩为直连**：`classify` 里那个分支的判据从 `unit.starts_with("hysteria-")`（`signature.rs:169`）改成 `unit == "hysteria-server"`——住宅单元不再有 `/auth` 回调，签名在它上面**失去对象**（评估 `sb.facts[7-b-ui现有哨兵签名无法直接复用]`，tizi PoC 复验鉴权失败无日志）。夹具里那条 `hysteria-residential-2` 的断言删除。`watchdog::is_auth_http_failure`（`watchdog.rs:73`）与阈值 `AUTH_HTTP_FAIL_THRESHOLD`（`watchdog.rs:58`）保留，只喂直连的日志 |
| `kernel_bind_in_use` / `kernel_crash_loop` | 保留（`starts_with("hysteria-")` 的内核判定因为单元名沿用而继续生效，`signature.rs:160`）；对 `hysteria-residential` 的夹具按 sing-box 真机日志重采（下表）。`is_crash_loop` 只看 systemd 文案，不变 |
| **新** `hy2_resi_relay_unreachable` | `hysteria-residential` 的 `open connection to … using outbound/socks[slot-<i>-out]: … connection refused / i/o timeout` ⇒ relay 或它的 `slot-<i>` 入站不在 ⇒ 60 秒 ≥ 3 条 → `DelegateWatchdog` + 事件。**`outbound/socks[deny]` 的拒绝行一律忽略**（那是被封用户的正常噪音）。relay 侧的 `parse_relay` 只认 `resi-` 前缀的 tag，`slot-<i>-out` / `deny` 天然不会被误认成上游成员 |
| **新** `hy2_resi_gate_sync_failed` | `b-ui` 自己的 `USER_SYNC_FAILED_LOG` 行且错误含 `Clash API 不可达` / `拒绝切换`（`ClashError` 两种文案，`clash.rs:30-36`）⇒ 150 秒 ≥ 2 条 → `RetryUserSync`（与 `xray_grpc_unavailable` 同预案：9092 在听就立刻重跑一轮收敛） |
| **新** `hy2_resi_gate_replay_failed` | 非日志签名，由 §3.4 的重放直接记事件 + 告警，恢复即清 |
| **新** `hy2_resi_pool_low` | 巡查类（同 `upstream_long_unreachable`）：空闲凭据 < 20% 一次性告警 |
| **新** `nft_table_missing` / `nft_missing` | watchdog 发现 `inet bui` 不存在或规则不符 ⇒ 重放 + 记事件；`which nft` 为假 ⇒ 告警（10 分钟冷却） |
| ~~`resi_slot_port_changed`~~ | **本表不动它**：它不是 `Sig` 成员，而是 `residential/upstream.rs:20` 的事件签名常量，随 §4.2 的「必须重新获取订阅」机制一起退役 |

**要按 sing-box 真机日志重采的夹具**（阶段 0 在 tizi sidecar 上采 `journalctl -u <单元> -o cat`，每条一个单测常量）：① 启动行 + `inbound/hysteria2[hy2-resi]: udp server started at [::]:40000`；② 成功连接两行（tizi 实测形态：`inbound connection from <ip>:<port>` 与 `[r000] inbound connection to <host>:<port>`）；③ `bind: address already in use` 的 FATAL 行（sing-box 措辞待采）；④ systemd 崩溃循环两行（与今天相同）；⑤ 到 relay 的拨号失败行；⑥ `deny` 噪音行（必须判 `None`）；⑦ `clash-api: restful api listening at 127.0.0.1:9092`；⑧ v2ray_api 起监听行（要自建二进制才有）。

### 8.2 watchdog（`modules/watchdog.rs`）

- `targets()`（`:180-205`）：住宅只剩一条 `{ unit: "hysteria-residential", proto: Udp, port: ports.hy2_resi }`，按槽枚举删除。
- `HY2_CONFIGS`（`:34-37`）只留 `("hysteria-server", "config.yaml")`：孤儿链自愈只对 apernet 直连有意义。`coupling.facts` 里「端口跳跃自愈只覆盖 2 个实例（已知缺口）」那条缺口**随住宅实例一起消失**（直连那一支的 nft 孤儿清理**已在 4.0.1 rc2 修好**并在 `bwg-tizi` 实测过，不再是待核项：§2.4、§14 裁决 7）。
- 新增每轮检查：`nft list table inet bui` 存在且规则集哈希 = 期望，否则重放并记 `nft_table_missing`。
- Clash API `9092` 与 v2ray_api `10086` **不进** watchdog 目标（回环面，进程在就有；进程僵死靠 `40000/udp` 判）。

### 8.3 自检（`commands/selfcheck.rs`）

- `listen_ports`（`:188-200`）**已经**只查 `p.hy2_resi` 一个住宅端口，注释也已写「端口跳跃区间不查（redirect 过来的，本身不监听）」⇒ **本函数不用改**，槽 1..7 的端口本来就没进去过。
- `hy2_loopback`（`:248`）保留直连探测；新增住宅回环探测：取首个有 `hy2_resi_cred` 的用户的 `name:secret`（配置文件 0600、用完即删，同 `hy2_client_yaml` 套路，`:201`）连 `127.0.0.1:40000` 且带 `mport=41000-50000`（验 output 链）→ https 200。证书未就绪时 SKIP 而不是 FAIL（沿用 `:150` 那套口径）。
- 新增四行：「nft 表 inet bui」（存在 + 规则数 = 4，兼容段关掉后是 2）、「端口 1 未被监听」（`deny` 出站的前提）、「住宅 HY2 凭据池」（已用 / 空闲 / 代数）、「Clash API 9092 与 v2ray_api 10086 在听」。

---

## 9. 回滚

### 9.1 步骤：必须先删 nft 表

`bui upgrade --rollback`（`commands/upgrade.rs`：恢复 `bin/bui.prev`、四个内核 `.prev`、`manifest.prev.json`、最近一份 state 备份，重启 `b-ui`）在 4.1 上**由 4.1 的二进制执行**，所以它知道 nft 表的存在。`rollback()` 增加两步：先 `systemctl stop hysteria-residential`，再 **`nft delete table inet bui`**（表不存在或没有 `nft` 只记 note，不算失败）。之后 4.0.1 的对账重新渲染每槽 apernet 配置与单元、把 `hysteria-residential` 单元文件写回 apernet 形态、起 `hysteria-residential-<i>`。

**为什么表必须删**：4.0.1 的槽 0 只监听第 0 片，而 `inet bui` 若还在，会把整段 `41000-50000` 与 `40001-40007` 全部 REDIRECT 到 `40000` ⇒ 所有槽的跳跃包与基础端口都被吸进槽 0 的进程，**比回归事故更糟**（全体用户从槽 0 的 IP 出去，且 `40000+i` 无人应答）。

**`scripts/ops/upgrade-drill.sh` 增补**（回滚相位的断言）：`nft list tables` 不含 `inet bui`；`ss -lnu` 上 `40000..40000+span-1` 逐个在听；`hysteria-residential-<i>` 单元 active；`UNITS`（`:23`）按相位切换（升级后相位是固定六个，回滚相位加回带后缀的名字）。升级相位的断言：`nft list table inet bui` 有 4 条规则、三种订阅 sha 对**每个**用户不变。

### 9.2 state 与订阅

- 4.0.1 的 `State` 反序列化忽略未知字段（`model.rs` 无 `deny_unknown_fields`），所以即使恢复的备份是 4.1 写过的（含 `hy2_pool` / `hy2_resi_cred` / `hy2_resi_compat_ports`），4.0.1 也能读，下次写盘时丢掉这些字段；再升回 4.1 时 §4.3 第 1 步重新建池，**迁移用户仍拿到 `username:hy2_password`**。
- 回滚后的订阅：迁移用户（凭据 `alice:pw`）回到 4.0 行为（按槽端口 + 切片，整段订阅重新遭遇跨进程静默丢包）；**4.1 期间新建或 rotate 过的用户**手里是 `r017:secret`，4.0.1 的 `auth_hook::decide` 查不到用户名 `r017` ⇒ **握手被拒，必须刷新订阅**。CHANGELOG 4.1.0 的「回滚提示」写清这两类，`bui residential pool status` 能列出「持有新发凭据的用户」名单供回滚前核对。
- obfs 跨版本提示沿用 4.0.1 CHANGELOG 那条（开着混淆回滚要先 `bui set obfs off`）。

---

## 10. 分阶段计划与验收判据

沿用评估 `ev.plan_if_B` 的五阶段（阶段 0 的多数项已由 tizi PoC 提前完成）：

| 阶段 | 内容 | 验收判据 |
|---|---|---|
| **0 · staging 补验**（0.5–1 天，tizi sidecar 复用同一套隔离端口 `:40100` / `50001-53000` / `inet bui_sidecar`） | 跑 §12 剩下的项：三种客户端矩阵（第 1 项）、`nft` 跨发行版（2）、自建二进制的 v2ray_api 与标签 / 可重现 / 体积（3–6）、QUIC 空闲字段名（7）、256 凭据（8）、证书热加载（9）、`interrupt` 对 UDP（10）、`deny` 日志速率（11）、sniff 传域名（12）；采 §8.1 的八条日志夹具 | 三种客户端各 30 分钟 `connected` 计数 = 1；自建二进制 `sing-box version` 的 `Tags` ⊇ 官方 + `with_v2ray_api`，`grpcurl` 打通 `QueryStats`；夹具八条齐 |
| **1 · bui-schema** | 模型（§4.1）、`hy2pool` 纯函数、`render::hy2_singbox`、`render::nft`、`nodes_for` 改值、`SlotRes` 收缩、删 `hop_slice` / `resubscribe_impact` | `cargo test` 全绿；**v3 golden 三种订阅逐字节不变**；`hy2-residential.json` 过自建 1.14 `check`；守门测试：同一 state 增删用户 / 换槽 / 置到期 ⇒ 配置字节相等；8 槽下每个用户的住宅 HY2 端口与区间**完全相同**；四个 label 字面量钉死（§7.4 约束 1） |
| **2 · bui 守护进程** | 单元模板、`LEGACY_UNITS` 16 项、`Artifact::NftTable`、`Hy2ResiApi` + fakes、门位收敛、`Event::Hy2ResiRestarted` 重放、`traffic.rs` 接 v2ray_api + `/connections`、kick/rotate 改门、`migrate_on_start`、watchdog / 哨兵 / 自检 / `bui status` / 面板投影、CLI（`bui residential pool status|grow`、`bui nft apply|status|delete`、`bui set hy2-resi-compat on|off`）、`rollback()` 删表、前端删 `port_changed` | 单测 + `cargo clippy --workspace --all-targets -D warnings`；`FakeHost` 上一轮对账：停删 `hysteria-residential-1..7`、写 nft 表、`hysteria-residential` 单元正文是 sing-box；渲染器产出的配置在 tizi sidecar 上复跑门控与跳跃两项 |
| **3 · 发布链与脚本** | `build-singbox.sh`、`pin-kernels.sh` 自建模式、`fetch-kernels.sh` 认 `build:`、`kernels-bump.yml`、`release.yml` / `ci.yml` 改动、`scripts/tests` 三份；m1 / m3 / upgrade-drill 按 §6、§9 重写 | `bash scripts/tests/run-all.sh` 绿；fork 的 Actions 上 `release.yml` dispatch 出 12 个资产且 Tags 达标；`kernels-bump.yml` dispatch：无新版本零变更、人为把锁改旧一版能开出 PR；**故意让构建失败一次 ⇒ 锁不动、不开 PR、工作流红**（§5.4 第 8 条的机器化验收）；m3 ①–④ 在 staging 全过 |
| **4 · 生产升级** | 先 `bwg-rick`：`bui upgrade` 到 `v4.1.0-rc1`（对账停旧槽单元、起新单元、写含兼容段的 nft 表；**全员不刷新订阅**），观察 24 小时；再 `bwg-tizi`；稳定 30 天且兼容段计数为 0 后宣布下线日期 | 三种订阅 sha 对每个用户不变；`hysteria-residential` `NRestarts` 只涨升级那一次；主理人用**手里未刷新**的 v2rayN 订阅连住宅 HY2 30 分钟无重连；`bui upgrade --rollback` 演练一次并过 §9.1 全部断言；**用 rc 客户端渲染出的 `config.json`（升级前就在盘上、没有重新导入的那一份）连 4.1 服务端**验通（§7.5 四条不变量的机器化验收） |

**工期量级**：与 P3 IP 池里程碑同级（评估 `coupling.facts` 的估算：删改 ≈ 2000 行 + 新增 ≈ 800–1250 行 + 脚本重写）。阶段 0 半天到一天，阶段 1–3 约两周 agent 工作量，阶段 4 每台一天 + 24 小时观察。

---

## 11. 风险与缓解

| # | 风险 | 缓解 | 状态 |
|---|---|---|---|
| 1 | 计量要自建 sing-box，打破「内核用上游原样构建」原则，CI 多一条 Go 构建路径 | §5.4：轨道 → 锁（记 Go 版本 + 标签）→ 可重现 sha 校验 → 周更 bot PR；Tags 必须是官方超集；**失败保持上一版并告警** | 主理人已接受 |
| 2 | 到期 / 封禁后握手仍成功、只在流级拒绝 | §6 文案 + m3 判据③改写；直连仍握手即拒 | 主理人已接受；tizi PoC 已复现该语义 |
| 3 | 单点：住宅 HY2 一进程，崩溃或扩容重启全员重连一次 | `Restart=always` + 客户端自动重握手；扩容只在 20% 余量告警后择时；内存档 300M/500M 对实测 65 MB 有余量；崩溃循环 / bind 冲突有哨兵 | 主理人已接受 |
| 4 | 门位重放失败 ⇒ 全员被拒（fail-closed） | §3.4：不开 `cache_file`（陈旧放行比短暂拒绝更糟）、复用 relay 的 15 秒退避、失败告警 + 60 秒安全网收敛；可用性包线与今天 `auth_http` 相同 | **已裁决**（§14 裁决 1），代价已写明 |
| 5 | nft 表与 apernet 的规则同挂 nat 钩子会互相干扰 | 表名 / 族 / 区间三重隔离；watchdog 表缺失自愈 | **已消解**：tizi 上与生产 9 张 `hysteria_*` nft 表共存无冲突 |
| 6 | 哨兵夹具与 m1/m3 判据作废、可能漏报 | §8.1 八条夹具在阶段 0 采齐；m1/m3 在阶段 3 重写并在 staging 跑绿后才进生产 | 阶段 0/3 |
| 7 | 工期与沉没：`hop_slice` / `resubscribe_impact` 整体废弃 | 4.0.1 不做「登记分配」（评估结论）；退役代码在阶段 1 一次删干净，不留兼容层 | 已决 |
| 8 | 升级过渡：`40000+i` 靠兼容段兜底，下线仍需通知；obfs 开着时两侧密码必须一致 | 兼容段带 `counter`，30 天零命中才下线且提前 30 天通知；obfs 密码与 `nodes_for` 同源，阶段 1 有测试钉住 | 阶段 4 |
| 9 | 客户端兼容：只用 apernet 2.12.2 客户端验过 `name:secret` 与共享段 | §12 第 1 项：三种客户端各 30 分钟；`bui-c` 本来就是 sing-box ↔ sing-box | **未验，首要待验项** |
| 10 | `deny` 出站的拒绝行是噪音：被封用户每个请求打一行 | 哨兵忽略 `outbound/socks[deny]`；单元带 `LogRateLimitIntervalSec=10s` + `Burst=200`；§12 第 11 项量速率，超过 relay 今天量级就把 `deny` 换成本机 blackhole 端口或降 `log.level` 为 warn | **不预先改，实测后裁**（§14 裁决 4；实测项 §12 第 11 项） |
| 11 | `nft` 二进制在某些发行版缺失 | 安装 / 升级缺 `nft` 即拒绝（退 2、不落盘）并给出安装命令，不自动装包；4.0.1→4.1 那一跳由演练 preflight 守；缺失时住宅 HY2 对带 `mport` 的客户端**全断**并告 Error；退路 iptables 不进本期 | 已实测（rick 缺 / tizi 1.1.6） |
| 12 | v2ray_api 的 proto 包名与 Xray 不同 | §12 第 3 项：以源码为准 vendor，tonic 两份客户端互不影响 | 阶段 0 |
| 13 | 吞吐无数据 | 两者同属 quic-go 家族、服务端都走 BBR（`ignore_client_bandwidth`），评估也明确无公开基准 ⇒ **不作为 A/B 判据**；真要数字就在阶段 0 用临时机 iperf3 双向测 | **已裁决**（§14 裁决 8）：吞吐不作为 A/B 判据、不在生产机重试（§12 第 15 项） |

---

## 12. 待验证清单

已由 tizi PoC（2026-09-15，生产内核）验证的项**划掉**，只留真正未知的。

- ~~**门控零重连**：`PUT /proxies/gate-r000 {"name":"deny"}` → 请求 000；切回 → 204；客户端 `connected to server` 次数全程 = 1~~ ✅ 已验证
- ~~**真实 nft REDIRECT + 共享整段**：公网域名 + `50123` / `52777` 随机端口 + 真证书（`insecure:false`）均握手成功并 204；**双 hook（prerouting + output）是必需的**~~ ✅ 已验证
- ~~**与 apernet 共存**：与生产现有 9 张 `hysteria_*` nft 表同族共存无冲突，期间生产五单元全程 active~~ ✅ 已验证（24 小时长跑未做，由阶段 4 的 24 小时观察涵盖）
- ~~**配置形状过 `check`**：hysteria2 入站 + `users[{name,password:"r000:<pw>"}]` + salamander obfs + masquerade proxy + 生产证书 + `ignore_client_bandwidth` + 三个 selector 门 + 两条 `auth_user` 规则 + `clash_api`；route 用传统 `"outbound"` 字段即可~~ ✅ 已验证（1.14.0）
- ~~**到期语义**：默认门 deny 的用户握手成功、请求全 000~~ ✅ 已验证
- ~~**日志可用性**：日志带用户名（`[r000] inbound connection to …`）；鉴权失败仍无日志 ⇒ `Hy2AuthHttpFailed` 对住宅单元失效~~ ✅ 已验证
- ~~**内存**：sing-box 单进程 61.8 → 65.2 MB；apernet 每实例 28.1 MB ⇒ 3 槽起更省~~ ✅ 已验证

剩下要验的：

1. **三种客户端矩阵（最高优先）**：v2rayN（默认内核）、mihomo、`bui-c`（sing-box 客户端 `server_ports: ["50001:53000"]` + `hop_interval: "30s"`）对 `password="<名>:<密码>"` 与**共享整段**的互通。每种连 sidecar 跑 30 分钟持续请求。判据：客户端 `connected` 计数 = 1、服务端日志里源端口变化 ≥ 10 次且无错误行；同时验**中文用户名**凭据握手通过。
2. **`nft` 跨发行版**（**已部分实测，2026-09-16**：`bwg-rick` Ubuntu 26.04 LTS 无 `nft`、nftables 包未装、`nftables.service` 为 `not-found`；`bwg-tizi` 同版本有 nftables 1.1.6 且服务 `disabled`。结论：Ubuntu 云镜像不预装，缺失是常态而非边缘情形）：其余发行版仍待验——Ubuntu 22.04 / 24.04、Debian 12、CentOS Stream 9 上 `which nft` / `nft --version`、`inet` 族 nat 链对 `redirect` 的支持、`nft -f` 单事务「declare + flush + define」是否原子；记最低可用版本。
3. **v2ray_api gRPC**：sing-box `experimental/v2rayapi` 的 proto package 名与 `QueryStats(reset=true)` 语义（以源码为准）；用自建二进制 + `grpcurl` 打一次，确认键形如 `user>>>alice>>>traffic>>>uplink`。同时验 `experimental.v2ray_api` 段能过自建二进制的 `check`（官方 1.14.0 上必然 FATAL，无法在 tizi 现有二进制上验）。
4. **自建标签集**：官方同版本归档 `sing-box version` 的 `Tags:` 作为基线写进 `SINGBOX_TAGS`；自建 ⊇ 基线 + `with_v2ray_api`；`-checklinkname=0` 是否必需。
5. **可重现构建**：同 tag、同 Go 版本、两次构建 ⇒ amd64 与 arm64 的 sha256 各自一致。
6. **体积与客户端**：自建 vs 官方二进制体积差；`bui-c` 用自建二进制 `sing-box check` + 连四个节点。
7. **QUIC 空闲超时字段名**：hysteria2 入站是否接受 `idle_timeout`（或对应名）以对应今天的 `quic.maxIdleTimeout: 60s`；`check` 对未知字段 FATAL，一试便知。
8. **凭据池上限**：**256** 凭据 + 256 门 + 256 规则的空载 RSS 与 `check` 耗时（128 已实测不涨）。上限已由 §14 裁决 2 从 512 压到 256，这一项按 **256** 量，只是复核余量。
9. **证书热加载**：替换 `certs/` 下的文件后 sing-box 是否自动重载 ⇒ 决定 §3.5 第 3 条那次重启能否省掉。
10. **`interrupt_exist_connections` 对 UDP**：门切 `deny` 后既有 UDP（如到目标站的 QUIC）是否也被掐断；`DELETE /connections/{id}` 对 UDP 条目的行为。
11. **`deny` 日志速率**（§14 裁决 4 把这一项留作裁决依据：**实测后裁**）：一个被封用户持续请求 10 分钟，`journalctl` 行数/分钟（§11 #10 的判据）。超过 relay 今天的量级就动退路——把 `deny` 换成本机 blackhole 端口，或把 `log.level` 降 `warn`；本期一个字不预先改。
12. **`{"action":"sniff"}` 是否保住今天的语义**：apernet 侧 `sniff.rewriteDomain: true` 会把目标改写成嗅探到的域名再交给 relay；要确认 sing-box 的 sniff action 之后，socks 出站发给 relay 的目标**是域名而不是 IP**（否则 relay 的 split 关键字分流会失配，住宅流量悄悄走直连）。判据：relay 日志里看到域名 + split 模式下命中关键字。
13. **回归事故的丢包机制**（补证，不阻塞实施）：跨进程的跳跃包是被静默丢弃还是收到 stateless reset；决定「静默丢包 → 30 秒 idle 超时」这句描述的精确措辞。
14. ~~**既有缺陷核实（另案）**：生产上 apernet 建的是 `hysteria_*` **nft 表**，而 `portjump.rs` 只扫 iptables 的 `HYSTERIA-PR-*` 链 ⇒ 直连实例的孤儿清理可能是 no-op。~~ **本项已作废（4.0.1 rc2 已修，§14 裁决 7）**：rc2 已加 nft 后端扫描，判据是「表正文 `redirect` / `dnat` 到**本实例 base 端口**」，直连实例的 base `10000` 同样覆盖；2026-09-15 在 `bwg-tizi` 实测孤儿命中 2 → 0、现役表正常重建（§2.4）。**序号保留、不重排**，以免与别处引用错位。
15. **吞吐：不作为 A/B 判据、不在生产机上重试（已裁决，§14 裁决 8）**。tizi 三轮都没拿到可用数据，原因都不在被测对象上：① 源站（瑞典）下 sidecar 与 apernet **同为 0.5 MB/s** ⇒ 瓶颈在源站/链路；② 换 Cloudflare 源后**直连控制组也是 0.0 MB/s** ⇒ tizi 到该源不通，整轮无效；③ 第三轮脚本在聚合步骤挂死，已 kill 并清理（tizi 上曾遗留一个监听 `:40100` 的 sing-box 与临时目录，已删净，生产八个单元 `NRestarts` 全 0）。结论：吞吐不是 A/B 判据（同 quic-go 家族、服务端都 BBR、无公开基准），spec 如实写**未验**；真要数字就在阶段 0 用一台临时机 + iperf3 双向做，**不要拿 curl 打公共源站**。

**staging 方式**：已由主理人定为 **tizi sidecar**，且 tizi PoC 已证明这条路可行、对生产无感，后续阶段**复用同一套隔离端口**（`:40100`、`50001-53000`、Clash API `9192`、v2ray_api `10186`、表名 `inet bui_sidecar`；与生产的 `41000-50000` / `20000-30000` 不重叠）。纪律照 PoC 那次执行：文件放 `/opt/b-ui` 之外（否则漂移扫描报陌生文件）、`nft -f` 前先 `nft -c` 校验、上线前人工核对表内容、防火墙临时放行测完即关、结束自清理并核对生产单元 `NRestarts`。

---

## 13. 对 C1–C5 契约的影响与需更新文档清单

### C1 `bui-schema` 公共 API

- **删**：`slots::{HY2_STATS_RESI_BASE, hop_slice, slot_span, resources_of, resubscribe_impact, ResubscribeImpact}`、`SlotRes.{hy2_port, stats_port, hop}`、`render::hysteria::{residential_slot_yaml, residential_yaml}`。
- **改**：`SlotRes { index, relay_port }`；`slots::resources(&Ports, index, span)` → `resources(index)`。`slots` 其余导出（`indices` / `sorted` / `fallback_index` / `slot_id_of_user` / `index_of_user` / `free_index` / `sync_slots` / `users_of_slot` / `least_loaded` / `assign` / `assign_least_loaded` / `migrate_unassigned` / `rebalance`）与 `MAX_SLOTS` / `RELAY_SOCKS_BASE` **签名不变**。
- **增**：`model::{Hy2Pool, ReservedCred}`、`Credentials.hy2_resi_cred`、`SystemSettings.hy2_resi_compat_ports`；`hy2pool::{size_for, grow, assign, release, cred_of, regenerate_idle_secrets, POOL_MIN, POOL_MAX}`；`render::hy2_singbox::{config(&NodeParams, &Paths, &Hy2Pool) -> serde_json::Value, HY2_RESI_CLASH_API, HY2_RESI_V2RAY_API, INBOUND_TAG, DENY_TAG, gate_tag(&str), slot_out_tag(u16)}`；`render::nft::{ruleset(&Ports, compat: bool) -> String, TABLE}`。
- **不变**：`nodes_for` 签名、三种订阅与 `render::client` 的签名、`render::relay::config`、`render::xray::config`、`v3::import`（内部多调一次 `hy2pool`）。

### C2 `bui` 内部模块边界

- `LEGACY_UNITS`：9 → **16**（+ `hysteria-residential-1..7.service`）；`is_managed_unit` 对带后缀的名字返回 false；`managed_units(&State)` 退化为固定六个（签名保留，调用点不动）；`resi_unit(i)` 删除；`MAX_RESI_SLOTS` 只剩枚举遗留单元用。
- `Artifact` 增**第 10 个**变体 `NftTable { family: String, name: String, ruleset: String }`（今天 9 个，`reconcile/mod.rs:228` 起）；`Event` 增 `Hy2ResiRestarted`（`api/state.rs:10-14`）。
- `panel` 增 `Hy2ResiApi` trait 与 `Shared::hy2resi()`、新文件 `panel/gates.rs`（纯函数，非 `Module`）；删 `panel/mod.rs:45` 的 `HY2_STATS_PORT_RESI`；`Module` 列表不新增模块（门位收敛在 `panel::users`，nft 在 `system`）。
- `modules/system.rs` 的 `reserved_ports` 与 `firewall_ports` 签名从 `slots: u16` 改成 `compat: bool`（调用点 `system.rs:119,186`、`serve.rs:136`）。
- `residential` 模块退役 §4.2 列的那一套：`upstream.rs:20-22,316-462`、`slots.rs:38-73` 的 `impact`、`api.rs:1105-1121`、`cli.rs:680-686`。

### C3 文件与路径

- **新**：`<base>/hy2-residential.json`（对账产物，0600）。**不新增** `BASE_WHITELIST` 条目（不开 `cache_file`，§3.4）⇒ 白名单仍是 13 项。
- **退役**：`config-residential.yaml`、`config-residential-<i>.yaml`（对账产 `Absent`）。
- 单元仍是六个名字；`hysteria-residential.service` 的 `ExecStart` 换成 sing-box、`ExecStartPre=-{bin}/bui nft apply`。
- 回环端点新增 `127.0.0.1:9092`（Clash API）与 `127.0.0.1:10086`（v2ray_api）；`9991..9998` 释放。

### C4 `manifest.json`

形状不变。`sing-box-linux-<arch>` 的 sha256 变为自建产物的；可选新字段 `builds`（§5.4 第 10 条）。`kernels.lock` 的 `sing-box target` 两行 URL 列改成 `build:` URI——这是发布脚本的内部契约，不是 manifest 的。

### C5 CLI 与快照

- 新子命令：`bui residential pool status|grow [--to N]`、`bui nft apply|status|delete`、`bui set hy2-resi-compat on|off`。
- `bui status` 新增三行：「住宅 HY2 凭据池」「nft 表 / 兼容段命中」「门位重放」；`bui incidents` 新增 §8.1 的签名 id。
- `bui set hy2-auth` 语义缩为**仅直连**（帮助文本改）；`auth-snapshot.json` 形状不变（只服务直连 + `command` 退路）。
- `bui upgrade --rollback` 多做 `nft delete table inet bui`（§9.1）。

### 需要更新的文档

1. **`CLAUDE.md`**：Server topology 表的住宅行（`hysteria-residential[-<i>]` → 单实例 sing-box、`:40000` + 整段）、受管单元段（去掉「每个住宅槽位一个 `hysteria-residential-<i>`」）、Subscriptions 段的住宅 HY2 描述（`:(40000+用户槽位)` → `:40000`）、「服务器上的文件」列表（加 `hy2-residential.json`、去 `config-residential*.yaml`）。
2. **spec `2026-09-11-v4-architecture-design.md`**：§1 图、§3.1 表的住宅行、§3.2 标题与作用域「仅直连」、§3.4 防火墙段、§4.2 采样段、§5.6 的 HY2 住宅一条与「订阅」段与规则 4、§5.7 签名表、§9 的 M3 判据；文首「状态」处链接本文。
3. **总纲 `2026-09-11-v4-master.md`**：C1–C5 按上文回写；裁决记录追加「2026-09-15 住宅 HY2 改 sing-box 单入站（三个代价 + 本文路径）」。
4. **`README.md`** 端口表：`40000` + `41000-50000`，加一行兼容段说明。
5. **`CHANGELOG.md`** `## [4.1.0]`：升级零刷新、兼容段与下线判据、到期语义变化、自建 sing-box、回滚提示（§9.2）、`bui-c` 无需重导。
6. **`docs/HANDOVER-bui-c.md`** §6：补「服务端 4.1 后住宅 HY2 节点是 `40000` + 整段，旧导入无需重做；兼容段下线前需重导的只有 4.0 多槽时代导入的槽 ≥ 1 节点」。
7. **`docs/residential-proxy-guide.md`**：到期 / 封禁在住宅 HY2 上的表现（§6 文案）、`bui residential pool` 用法。
8. **`docs/superpowers/plans/2026-09-13-v4-p3-ip-pool.md`**：文首加注「D3/D4 的 HY2 部分与相关任务已被 2026-09-15 本 spec 取代」（不改正文）。
9. **新计划** `docs/superpowers/plans/2026-09-15-v41-hy2-singbox.md`（已产出，按 §10 五阶段拆成 T1–T20；T20 是发布 tag 门禁，依据见 §7.6 事实 2）。
10. **脚本头注释与判据**：`scripts/m1-acceptance.sh`、`scripts/m3-acceptance.sh`、`scripts/ops/upgrade-drill.sh`、`soak-sample.sh`、`v3-cutover.sh` 里凡提到 `hysteria-residential-<i>` / `9998` / 住宅 `auth-hook.log` 的地方。

---

## 14. 主会话终审裁决（2026-09-15）

本节是**终审**：八条已拍板，正文已按裁决改写，实施者按正文做，不必回来问。

1. **不开 sing-box 的 `cache_file`，保持 fail-closed。**
   **裁决**：`hy2-residential.json` 不写 `experimental.cache_file`；重启后每个门回到
   `default: "deny"`，由 b-ui 重放真实门位（§3.4；C3 的 `BASE_WHITELIST` 因此不新增条目）。
   **理由**：可用性包线与今天 `auth_http` 相同——b-ui 不在时 HTTP 钩子同样全拒；而「陈旧放行」
   会让到期 / 封禁用户在 b-ui 掉线期间照常上网，比短暂拒绝更糟。

2. **凭据池 `POOL_MAX` 从 512 压到 256。**
   **裁决**：`size_for(n) = clamp(ceil16(2 × n), 32, 256)`，凭据 id 域随之收成 `r000`…`r255`（§3.1）。
   **理由**：当前住宅 HY2 用户个位数，2× 上限远够；128 凭据已实测 RSS 与 `check` 耗时不涨，
   256 离已实测点更近，512 属于未实测的推断上限。

3. **兼容段 `40001-40007` 的下线判据保持原样、不收紧。**
   **裁决**：仍是「`counter` 连续 30 天为 0 + 提前 30 天通知」（§2.4）。
   **理由**：判据本身已经够严，收紧只会推迟下线、并不减少风险；真正的风险点是「从不重新导入」
   的客户端（§7.5），那由通知与 §7.6 的发布门禁覆盖，不靠加长零命中窗口。

4. **`deny` 成员的日志噪音：不预先改，待 §12 第 11 项实测后再裁。**
   **裁决**：本期照 §2.2 / §2.3 用指向 `127.0.0.1:1` 的 socks 出站当 `deny`，只靠单元的
   `LogRateLimitIntervalSec=10s` + `LogRateLimitBurst=200` 与哨兵忽略这类行压噪音；阶段 0 量出
   行数/分钟后再裁。退路（两条，都不进本期）：把 `deny` 换成本机 blackhole 端口，或把
   `log.level` 降到 `warn`。§2.2 与 §12 第 11 项都已写明这一条。
   **理由**：噪音量级未知，先改等于凭猜改判据；而降 `log.level` 会一并吃掉 §6 那条「带用户名的
   `inbound connection`」排查线索——住宅侧鉴权失败本来就没有日志，这条线索不能盲付。

5. **sing-box 不设 `GOMEMLIMIT`**（与 relay 今天一致）。
   **裁决**：住宅单元不写 `Environment=GOMEMLIMIT=…`（§2.5）。
   **理由**：sidecar 实测 65 MB，单元已有 `MemoryHigh=300M` / `MemoryMax=500M` 兜底；
   apernet 那条 `200MiB` 是它自己的 GC 口径，照抄到 sing-box 上没有依据。

6. **彻底删 `slots::resources_of`，不保留僵尸签名。**
   **裁决**：收口任务（plan T15）连签名一起删——不留 `#[deprecated]`、不留转发壳；四处活调用点
   （`render/xray.rs`、`panel/users.rs`、`residential/api.rs` 的 `slot_rows`、`slots.rs` 自己的
   两个单测）一并改（§4.2）。
   **理由**：留一个还能编译的旧签名，下一个人就会继续「按槽算端口」——而 4.1 之后端口与槽无关，
   那正是 §1.1 回归事故的入口。

7. **「§2.4 顺带发现的既有缺陷」作废（4.0.1 rc2 已修）。**
   **裁决**：spec 早先那段「apernet 建原生 nft 表、而 `portjump.rs` 只扫 iptables 的
   `HYSTERIA-PR-*` 链 ⇒ 直连实例孤儿清理可能一直是 no-op」是按 **rc2 之前**的代码写的，
   **已不成立**：rc2 已加 nft 后端扫描，判据是「表正文 `redirect` / `dnat` 到**本实例 base
   端口**」，直连实例的 base `10000` 同样覆盖。2026-09-15 在 `bwg-tizi` 实测：重启
   `hysteria-residential-1` 后 prestart 日志「已删除 nft 表 ip6 hysteria_\<hash\>（本实例端口
   跳跃孤儿）」，孤儿命中 **2 → 0**，现役表 `44000-46999 → :40001` 正常重建。§2.4 已改成
   「rc2 已修，直连与住宅同一判据」，**§12 第 14 项标注已作废**（不删行、不重排序号，避免与
   别处引用错位）。

8. **吞吐不作为 A/B 判据。**
   **裁决**：维持 2026-09-15 裁决——不在生产机上重试；需要数字就在阶段 0 用一台临时机 +
   `iperf3` 双向测（§11 #13、§12 第 15 项）。
   **理由**：apernet 与 sing-box 同属 quic-go 家族、服务端都走 BBR、无公开基准；tizi 三轮失败
   的原因都在源站 / 链路上，继续拿 curl 打公共源站只会再产出无效数据。
