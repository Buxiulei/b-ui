# b-ui v4 架构设计（方案一：单二进制「期望态」控制器）

日期：2026-09-11。状态：**已与主理人逐节确认（①–⑦）**，待 spec 审阅后进入 `superpowers:writing-plans`。
输入：`docs/superpowers/audits/2026-09-11-architecture-audit.md`（审计汇总）、`docs/superpowers/research/2026-09-11-research-review.md`（调研审查 + Decodo 实测）、`docs/superpowers/specs/2026-09-11-residential-auto-blacklist-design.md`（R13）。
生产主机只用别名：`bwg-rick`（测试机，先上）、`bwg-tizi`（生产，验收后上）、`baiyi`（Linux 客户端主机）。

---

## 0. 目标与硬约束

**目标**：在保留全部 8 项能力的前提下，把 v3 里「装机与升级两条代码路、25 个迁移块、节点集合写 5 份、三处 cron、四个定时器、三份 SSH 硬化」这类实现冗余从结构上消灭；控制面与 Linux 客户端用 Rust 重写，追求可预测的内存与单二进制运维；千用户量级下面板不再是瓶颈，流量计费可信，用户变更不掉线。

**8 项能力（保留）**：① Hysteria2 直连节点（端口跳跃、Salamander 混淆开关）② VLESS-REALITY 直连节点 ③ 住宅出口（HY2 住宅 + Reality 住宅、上游池、健康切换、global/分流、R13 黑名单）④ Web 管理面板 ⑤ 三种订阅 ⑥ Linux 客户端 `bui-c`（SOCKS + TUN）⑦ 一键安装 + 自动更新 ⑧ 系统硬化（SSH、sysctl、证书、watchdog）。

**硬约束（已决策，不再讨论）**：
- 协议集不变：TCP/443 车道 = VLESS + REALITY + Vision（Xray-core）；UDP 车道 = Hysteria2 + Salamander + 端口跳跃（apernet/hysteria）。不新增第三条 wire 协议，不做 CDN 前置，不加国内中转。
- 协议内核保留成熟实现（Xray / hysteria / sing-box / Caddy），不自实现协议，不换 shoes。
- Rust 只覆盖控制面、住宅模块、Linux 客户端。
- 两台服务器允许重装；v3 的迁移块整体删除；现有订阅者零重导入。
- 前端 `web/{index.html,app.js,style.css}` 保留，只改必要的 4 处。
- 单机先做扎实，为多机与自助门户预留接口，不实现。
- 发版顺序：bwg-rick → 主理人验收 → bwg-tizi。

**明确不做（v4 范围外）**：VLESS-WS-TLS「免流」节点（删除）；按用户限速（内核不支持，`speedLimit` 字段删除）；自助门户前端与支付；多机中心面板；Windows/macOS 客户端（继续用 v2rayN / Shadowrocket 等消费订阅）；箭头菜单 TUI。

---

## 1. 总体结构

```
                       ┌──────────────── VPS ────────────────────────────────────────────┐
  v2rayN / bui-c ──────▶ hysteria-server      :10000 + 20000-30000  ── direct(IPv4) ───▶ Internet
                       ▶ xray  vless-direct   :10001 REALITY        ── freedom ─────────▶ Internet
                       ▶ xray  vless-resi     :10002 REALITY ──┐
                       ▶ hysteria-residential :40000 + 41000-50000 ─┤ socks 127.0.0.1:2080
                       │                                            ▼
                       │                       b-ui-relay (sing-box) ── resi-pool(selector) ──▶ 住宅上游 ──▶ Internet
                       │                                                └─ 黑名单 / 分流 / fail-open → direct
  管理员 ── :443 ──────▶ Caddy ── 127.0.0.1:8080 ──▶ bui serve（面板 API + 嵌入前端 + 对账器 + 巡检 + 黑名单 + 证书监听 + watchdog + 升级）
                       │                                   ▲ unix socket /run/b-ui.sock ◀── sudo b-ui（CLI/数字菜单）、bui auth-hook 读快照
                       └────────────────────────────────────────────────────────────────────┘
```

进程：4 个数据面内核 + Caddy + `bui`。**没有 cron、没有独立 timer、没有 shell 脚本改配置。** 端口、标签、UUID、密码与 v3 完全一致，订阅零变化。

Cargo workspace：
- `crates/bui-schema`：节点 schema、用户权益 → 节点集合、四种内核配置渲染（hysteria ×2 / xray / sing-box relay）、三种订阅渲染、客户端 sing-box 配置渲染（TUN / mixed）。**唯一**知道端口、标签、规则的地方。
- `crates/bui`：服务端守护进程 + CLI（`install / upgrade / serve / reconcile / status / import-v3 / auth-hook / menu`）。
- `crates/bui-c`：Linux 客户端。
- `web/`：前端三文件，`rust-embed` 嵌入 `bui`。

---

## 2. 期望态与对账器（§①）

### 2.1 期望态 `state.json`

路径 `/opt/b-ui/state.json`，权限 600，写入 = 临时文件 + `rename`；每次写前把旧版复制到 `/opt/b-ui/state.backups/`（保留最近 10 份）。顶层：

```jsonc
{
  "schema_version": 1,
  "node": { "id": "<uuid>", "name": "bwg-rick", "domain": "…", "public_ip": "…",
            "ports": { "hy2": 10000, "hy2_hop": [20000, 30000], "hy2_resi": 40000, "hy2_resi_hop": [41000, 50000],
                       "reality_direct": 10001, "reality_resi": 10002, "admin": 8080 },
            "reality": { "private_key": "…", "public_key": "…", "short_ids": ["…"], "dest": "www.bing.com:443", "server_names": ["www.bing.com"] },
            "obfs": { "enabled": false, "password": "…" } },
  "admin": { "password_hash": "<argon2id>", "jwt_secret": "<hex>" },
  "users": [ /* §4.1 */ ],
  "residential": { "groups": { "default": { /* §5 */ } } },
  "system": { "ssh_hardening": true, "static_dns": true, "sysctl_profile": "auto", "firewall": "auto" },
  "versions": { "bui": "4.0.0", "hysteria": "…", "xray": "…", "sing_box": "1.14.x", "caddy": "…", "client_sing_box": "1.14.x" },
  "catalog": [ /* §4.4 SKU 桩 */ ]
}
```

运行时数据（健康 streak、黑名单候选计数、上次选中的上游、采样游标）放 `/opt/b-ui/runtime.json`，丢了也能从零重建。

### 2.2 对账器

`reconcile()` 是唯一改机器的函数。输入期望态，输出「机器应有的样子」：

| 类别 | 项 | 差异动作 |
|---|---|---|
| 文件 | `config.yaml`、`config-residential.yaml`、`xray-config.json`、`singbox-relay.json`、`Caddyfile`、`auth-snapshot.json`、6 个 systemd 单元、`/etc/sysctl.d/99-b-ui-*.conf`、`/etc/modprobe.d`、`/etc/resolv.conf`、`sshd_config.d/00-b-ui-hardening.conf` | 按内容哈希比对，只写有差异的 |
| 二进制 | `/opt/b-ui/bin/{hysteria,xray,sing-box,caddy}` | 版本不符则下载校验替换 |
| 服务 | 6 个单元的 enabled/active | `daemon-reload` + enable/start |
| 内核参数 | sysctl 值、`nf_conntrack` 模块 | `sysctl -w` |
| 防火墙 | ufw / firewalld 规则 | 装了就开端口集；没装就在体检里提示云侧放行 |

**重启映射**：`config.yaml` 变 → 重启 hysteria-server；`config-residential.yaml` 变 → 重启 hysteria-residential；`xray-config.json` 的**结构哈希**（排除 `inbounds[].settings.clients`）变 → 重启 xray；`singbox-relay.json` 变 → 重启 b-ui-relay 并随后重放上次选中的上游；`Caddyfile` 变 → `caddy reload`；单元变 → `daemon-reload` + 重启该单元。

**触发**：启动时；每次 API 变更后（500ms 去抖）；每 10 分钟漂移检查；`bui reconcile` 手动。

**漂移策略**：由对账器生成的文件与现状不同 → 直接覆盖（它们是派生物）；**非**对账器所有的东西（受管单元下的 `.d/` drop-in、多出来的 cron 行、`resolv.conf` 的 immutable 位与期望不符、受管目录里的陌生文件）→ 只在体检里列为「漂移」，不动，`bui reconcile --force` 才清理。这是为了把 bwg-tizi 那类手工 drop-in 暴露出来而不是静默盖掉。

**验证再重启**：渲染结果先过内核校验（`xray run -test`、`sing-box check`；hysteria 无校验命令，靠 schema golden 测试兜底），校验失败不写盘、体检报错；重启失败自动恢复上一版文件并再启一次，健康状态降级。

### 2.3 v3 导入

`bui import-v3` 读 `users.json`、`reality-keys.json`、`residential-proxy.json`、`config.yaml` 的 `listen:` 行（端口与跳跃段）、`config-residential.yaml` 的 `listen:`、`certs/.domain`、`admin.env`（管理员密码 → argon2id 哈希）、`xray-config.json` 的 `dest/serverNames/shortIds`、obfs 状态；生成 `state.json`。用户的 `protocol` 字段映射为权益（§4.1）。导入后卸载 v3：停用并删除 v3 单元、timer、cron 行（只删 b-ui 自己写的行）、`/opt/b-ui` 下的 shell 与 Node 文件；保留 `certs/` 与 Caddy 数据目录。

### 2.4 CLI 与守护进程

`sudo b-ui` 是 `bui menu` 的符号链接，数字两列菜单沿用；所有菜单项通过 `/run/b-ui.sock`（0600，root）调守护进程 API。守护进程未运行时（装机、升级中）CLI 只允许 `install / upgrade / status / reconcile`。

---

## 3. 数据面与配置渲染（§②）

### 3.1 内核与端口

| 内核 | 单元 | 监听 | 出口 | 变化 |
|---|---|---|---|---|
| hysteria-server | `hysteria-server.service` | `:10000,20000-30000`（`listen:` 一行） | 内置 direct，`mode: 4` | 鉴权改 `command`；**不再有任何 iptables/nft 规则**，`hy2-portjump-cleanup.sh` 与面板的 iptables REDIRECT 路径删除 |
| hysteria-residential | `hysteria-residential.service` | `:40000,41000-50000` | `acl: relay(all)` → socks5 127.0.0.1:2080 | 同上 |
| xray | `xray.service` | `:10001` vless-direct、`:10002` vless-residential（同一 REALITY 密钥、同一 UUID 集）、`127.0.0.1:10085` api | freedom ForceIPv4 / socks 127.0.0.1:2080 | 用户增删走 gRPC |
| sing-box relay | `b-ui-relay.service` | `127.0.0.1:2080` socks、`127.0.0.1:9091` Clash API | resi-pool selector / direct | 规则见 §5 |
| Caddy | `caddy.service` | 80/443 | reverse_proxy 127.0.0.1:8080 | 不变 |

四个内核都是从各自 GitHub Releases 下载的静态二进制（sha256 校验），放 `/opt/b-ui/bin/`；不再调用 `get.hy2.sh` / Xray-install / 发行版包，不再装 Node.js。sing-box 上限 1.14（v2rayN 7.25 封顶）。

### 3.2 Hysteria2 鉴权：`auth.type: command`

`config.yaml`：`auth: { type: command, command: /opt/b-ui/bin/bui auth-hook }`。内核对**每条新 QUIC 连接调用一次** `bui auth-hook <addr> <auth> <tx>`（无环境变量、stdin 为空；`auth` 是客户端 `hysteria2://user:pass@` 里的 `user:pass` 原串，钩子按第一个 `:` 拆分用户名与密码；调研 H1–H7）。钩子读 `/opt/b-ui/auth-snapshot.json`（600，守护进程在用户变更时原子重写；内容 = 用户名 → HY2 密码、`user_id`、到期时间、是否超限），常量时间比对，放行则 stdout 打印 `user_id` 并退出 0——**该字符串就是 `/traffic`、`/online`、`/kick` 的键**。不放行：到期、超限、禁用、不存在、快照读取失败或超时（fail-closed）。内核对钩子**不设超时、不限流**（H8/H9）：钩子自设 ≤ 2 秒硬超时；`auth-hook` 子命令走极简路径（不初始化 tokio/tracing/不加载 state，只读快照一个小文件）；stderr 不会进 `hysteria-server` 的 journal（H5），诊断日志由钩子自写 `/opt/b-ui/auth-hook.log`（按大小轮转，只记用户名与结果，不记密码）。
- 效果：增删用户、到期、超限在建连时生效，内核不重启；无面板 SPOF。
- 代价：每次建连 fork 一个静态二进制（毫秒级）。M5 压测 200 建连/秒 p99 < 20ms，同时记录 PID/fd 峰值；不达标的退路是改回 `userpass` + 内容比对重启（v3.6.0 行为；Hysteria2 无 SIGHUP/热加载，用户表变更只能重启，H14），此退路必须在 M3 就以配置开关实现好。
- 密码以明文存 state 与快照（v3 的 `config.yaml` 本来就是明文，订阅也需要明文）；文件 600，日志脱敏。

### 3.3 Xray 用户与统计走 gRPC

tonic 客户端，proto 从 Xray-core `v26.3.27` vendor 进仓（以仓库根为 include path，共 10 个文件：`app/proxyman/command/command.proto`、`app/stats/command/command.proto`、`common/protocol/user.proto`、`common/serial/typed_message.proto`、`core/config.proto`、`proxy/vless/account.proto`、`app/proxyman/config.proto`、`transport/internet/config.proto`、`common/net/address.proto`、`common/net/port.proto`；标准 proto3，无 well-known types）。增删用户：`HandlerService.AlterInbound{tag, operation}`，`operation` 是两层 `TypedMessage`——外层 `type="xray.app.proxyman.command.AddUserOperation"` 包 `User{level:0, email:<user_id>, account}`，`account` 的 `type="xray.proxy.vless.Account"` 包 `Account{id:<uuid>, flow:"xtls-rprx-vision"}`；删除用 `RemoveUserOperation{email}`；对两个 inbound 各一次。**`email` 是 gRPC 侧唯一键**（stats 计数器名也用它），因此 `xray-config.json` 的 `clients[].email` 与 state 的 `user_id` 一一对应且非空。统计：`StatsService.QueryStats(pattern="user>>>", reset=true)` 一次拉全量（`pattern` 是子串匹配；`reset` 对每个计数器原子交换清零，不丢不重，但不同计数器不是同一时刻快照，累加语义正确，X6/X7）。`RemoveUser` **只阻止新握手，已建立的 REALITY 连接会活到客户端自己断开**（Xray #5844 未合并，X10）——v4 接受该窗口，不做进程重启硬切，面板说明写明「超限/到期用户的既有连接可能延续到其断开」。`xray-config.json` 里的 `clients` 列表同步维护，只为重启后持久化；它的变化不触发重启；REALITY 参数在 transport 层，增删用户无额外动作（X11）。CLI 退路：`xray api rmu -tag=<tag> <email>`；`xray api adu` 需要完整 inbound JSON 片段。

### 3.4 证书、系统状态、watchdog

- 证书：Caddy 继续签；守护进程 inotify 监听 Caddy 证书目录，内容变化才复制到 `/opt/b-ui/certs/`，然后**间隔 10 秒**依次重启两个 hysteria。
- 静态 DNS：`system.static_dns=true` 时写 `/etc/resolv.conf`（1.1.1.1 / 8.8.8.8）并 `chattr +i`，systemd-resolved 存在则先禁用；两台机统一行为；可在 state 关闭。
- sysctl：BBR、somaxconn、按内存分档的 `nf_conntrack_max`（131072 / 262144 / 524288），与 v3 同值。
- 防火墙：装了 ufw/firewalld → 开 22/`PORT`/80/443/10001/10002/40000 + 两个跳跃段；否则体检提示。
- SSH 硬化：一份实现（禁密码登录，仅当存在公钥；`sshd -t` 失败则不写并记录），装机与 `b-ui harden-ssh` 同一函数。
- systemd 单元全部带 `LimitNOFILE=1048576`；两个 hysteria 与 relay、xray 带 `MemoryHigh/MemoryMax`（relay 与 xray 是 v3 缺的）；`Nice=-5`；`b-ui.service` 自身 `MemoryMax=200M`、`Restart=always`。
- watchdog：守护进程每 60 秒检查四个内核进程存活 + 监听端口存在（读 `/proc/net/{udp,tcp}`），连续 2 次失败 `systemctl restart`，退避 1/2/4 分钟，体检显示最近重启记录。`/tmp/hy2-watchdog-*` 文件与 timer 删除。
- 日志：`tracing` → journald；默认 INFO；凭据、密钥、密码一律脱敏。

---

## 4. 用户、流量、面板 API（§③）

### 4.1 用户模型

```jsonc
{
  "user_id": "<uuid>", "username": "alice", "note": "", "created_at": "…", "disabled": false,
  "credentials": { "hy2_password": "…", "vless_uuid": "…" },
  "entitlements": { "protocols": ["hysteria2", "reality"], "direct": true,
                    "residential": { "group_id": "default" },        // 或 null
                    "expires_at": null, "traffic_limit": { "total_bytes": null, "monthly_bytes": null } },
  "usage": { "total_bytes": 0, "monthly_bytes": 0, "month_key": "2026-09", "last_seen_at": null },
  "portal_auth": { "password_hash": null, "tokens": [] },
  "billing": { "currency": "CNY", "balance_minor": 0, "orders": [] }
}
```

- v3 `protocol` 映射：`fusion` → 两协议 + direct + residential(default)；`hysteria2` → `["hysteria2"]` + direct + residential(default)；`vless-reality` → `["reality"]` + 同上。
- **节点集合 = f(权益)**：`bui-schema::nodes_for(user, node)` 返回该用户可用的节点列表（最多 4 个：HY2 直连 / Reality 直连 / HY2 住宅 / Reality 住宅），三种订阅、`/api/nodes`、面板显示都只调用它。
- 订单记录：`{ "order_id", "sku", "amount_minor", "status": "pending|paid|fulfilled|cancelled", "external_ref", "created_at" }`，追加式；`catalog[]`：`{ "sku", "title", "kind": "residential_ip|plan", "region", "price_minor", "period_days" }`。v4 只定义结构与读写，不做支付与履约。

### 4.2 流量采样与限额

- 守护进程内 10 秒任务：hysteria 直连 `GET /traffic?clear=1`（9999）+ 住宅（9998）增量相加（`clear` 在同一把锁内序列化并清零，原子，H10；trafficStats 若设 `secret`，请求头 `Authorization: <secret>`，无 `Bearer` 前缀，H13）；Xray `QueryStats(reset=true)`；累加到内存中的 `usage`，最多每 30 秒合并落盘一次。
- 在线：hysteria 两个 `/online` 的并集（值是该用户当前 QUIC 连接数，`>0` 即在线，H11）∪ 最近 30 秒有 Xray 增量的用户。
- 月度重置：服务器本地时间每月 1 日 00:00，`month_key` 变化时清零 `monthly_bytes`。
- 执行：到期 / 超限 / 禁用 → 快照标记拒绝（主保障：新连接一律被钩子拒绝）+ 对两个 hysteria `POST /kick`（请求体 `["<user_id>"]`；kick 只是标记，要等该用户下次有流量才断连，空闲连接可能长期不断，被踢后客户端会重连并再次过钩子，H12）+ Xray `RemoveUser`（只阻新握手，见 §3.3）；恢复条件满足时反向操作（快照放行 + Xray `AddUser`）。限额判断每次采样后执行。
- `/api/stats`、`/api/online` 读同一份缓存；面板开着不增加采样。

### 4.3 面板 API

管理员域（JWT，`Authorization: Bearer`，24h 过期，密钥持久化在 state；登录失败限速 5 次/分钟/IP）：路径与响应结构沿用 v3 的 `/api/login`、`/api/users`（GET/POST/PUT/DELETE）、`/api/stats`、`/api/online`、`/api/config`、`/api/residential/*`（status / add / remove / global / domains / health / restore-default）、`/api/health` 以及 v3 现有的其余只读端点（以 `web/app.js` 实际调用的集合为准，M1 前逐一列表）。**四处改动**：
1. 创建用户改为 `POST /api/users`（JWT）；删除 `GET /api/manage`；前端 `app.js` 对应改一处，删除 localStorage `ap`。
2. 删除 `/auth/hysteria`、`/api/kernel-downloads`、install-key（`/api/install-command` 返回不带 key 的命令；`/packages/*` 直接可下）。
3. `/api/health` 增加漂移列表、watchdog 记录、上游体检结果。
4. 新增 `/api/residential/check`（体检）、`/api/residential/select`（手动切上游）、`/api/residential/blacklist`（GET）、`/api/residential/blacklist/pins`（POST/DELETE）、`/api/residential/blacklist/apply`（立即应用）。

无鉴权（按用户名，沿用 v3）：`/api/sub/<user>`、`/api/subscription/<user>`、`/api/clash/<user>`、**新增** `/api/nodes/<user>`（节点 schema JSON，供 `bui-c` 渲染）。

用户域（预留，**v4 全部返回 501 `{"error":"not_implemented"}`**，请求/响应结构在附录 A 定义）：`POST /api/me/login`、`GET /api/me`、`GET /api/me/subscription-links`、`GET /api/me/entitlements`、`GET /api/me/billing`、`POST /api/me/orders`、`GET /api/me/orders/{id}`。

### 4.4 订阅

三种订阅与 v3 逐项等价（节点集、端口、UUID、密码、标签、`mport=`、obfs 参数、住宅分流规则），由 `bui-schema` 从同一节点列表渲染；sing-box JSON 保持 1.12–1.14 兼容子集（typed DNS、TUN `address` 数组、rule action、无 `rule_set`）。CI 用 v3 抓取的脱敏样本做 golden 比对（§8）。

---

## 5. 住宅模块与黑名单（§④）

### 5.1 状态（`residential.groups.default`）

```jsonc
{ "enabled": true, "mode": "global",              // "global" | "split"
  "keywords": null,                                 // null = 跟随默认表；[] 自定义
  "upstreams": [ { "id": "<uuid>", "name": "url-3", "type": "http", "host": "isp.decodo.com", "port": 10007,
                   "username": "…", "password": "…", "priority": 10, "provider": "decodo", "region": "US",
                   "ports_allowed": [80, 443],       // null = 未知/不限；由体检学得
                   "verified": { "ip": "…", "asn": 33667, "org": "Comcast", "country": "US", "at": "…" } } ],
  "selected_upstream_id": "<uuid>",
  "blacklist": { "pins": [ { "rule": { "kind": "domain_suffix", "value": "pay.google.com" }, "note": "", "created_at": "…" } ],
                 "auto":  [ { "upstream_id": "<uuid>", "rule": { "kind": "domain_suffix", "value": "gateway.icloud.com" },
                              "hits": 74, "confirmed_at": "…", "last_verified_at": "…", "passes": 0 } ] } }
```

规则 `kind`：`domain_suffix` | `domain` | `port`。默认关键字表（67 条）随 `bui-schema` 发布，`keywords=null` 时跟随。

### 5.2 上游管理与体检

- 添加：接受 `socks5://u:p@h:port`、`http://…`、`h:port:u:p`、`u:p@h:port` 四种，CLI 用 `-` 从 stdin 读；类型未指定时先 SOCKS5 后 HTTP 探测（整轮重试一次）；成功后记 `verified`。凭据只进 state，日志与错误信息脱敏。
- 体检（`bui residential check <id>` / 面板按钮）：出口分类（ippure / ipquery / ipinfo 交叉，识别 Cloudflare 挑战页为「未知」而非失败）；Google 搜索页有无 `/sorry/`；`gemini.google.com`、`api.openai.com`、`api.anthropic.com` 可达；`checkout.stripe.com`、`pay.google.com`、`www.paypal.com`；固定端口集 5228 / 5223 / 993 / 22 / 8080 / 853 的 CONNECT 状态；SOCKS5 UDP ASSOCIATE。结果写 `verified` 与 `ports_allowed`（端口集里只有 80/443 通 → `[80,443]`）。
- 探测实现：reqwest（HTTP 代理与 SOCKS5 代理均支持鉴权），超时 10 秒，并发 4。

### 5.3 健康与切换

每 2 分钟一轮，成员并行：每成员对 `https://www.gstatic.com/generate_204` 探测 2 次，任一成功即本轮健康；连续 2 轮不达标 → 标记不健康，连续 2 轮达标 → 恢复。当前选中成员健康 → 不动；不健康 → 切到健康成员里 `priority` 最小者（同优先级按近 24h 成功率）；两次切换间隔 ≥ 60 秒；全部不健康 → 保持并告警；成员集在本轮内变化 → 本轮不切。切换通过 Clash API `PUT /proxies/resi-pool`（与配置切换走同一条 `SelectOutbound` 路径，S4）。**`resi-pool` 选择器的 `interrupt_exist_connections` 固定为 `false`**，否则每次健康切换都会掐断全部住宅连接。relay 任何重启后立即重放 `selected_upstream_id`。

### 5.4 黑名单（R13 落地）

- **候选**：(a) 跟随 `journalctl -u b-ui-relay -f -o json`，解析 `open connection to <host>:<port> using outbound/…[resi-N]: unexpected status: 403` 与 SOCKS 拒绝行，按 (upstream, host, port) 计数；(b) 每日对固定探针集（§5.2 端口集 + 支付域名集）主动探测。
- **确认**：经该上游探测为**硬拒**（HTTP 上游：CONNECT 状态码 4xx/5xx；SOCKS5 上游：回复码 ≠ 0）**且**直连同目标 TCP 可达，间隔 ≥ 10 分钟连续 2 次 → 生成 `domain_suffix` 规则，值就是被拒的完整主机名（如 `gateway.icloud.com`，它同时覆盖其子域），**不**泛化到注册域名，进入 `auto`。端口类拒绝不进黑名单，由上游的 `ports_allowed` 表达。
- **生效**：pins 与手动「立即应用」→ 立刻重渲染 relay 并重启；`auto` 新增 → 每日 04:00（服务器本地时间）批量；relay 重启是唯一掐连接的动作。
- **复核**：每条 `auto` 每日复探一次，连续 3 次不再被拒（`passes ≥ 3`）→ 移除。
- **渲染**：relay `route.rules` 顺序：① 黑名单 `domain_suffix/domain → direct`；② `ports_allowed` 非空时 `port_range` 取反 → direct（写法 `{"port_range":["1:79","81:442","444:65535"],"outbound":"direct"}`；`port_range` 必含冒号，单端口用 `port` 数组，S3）；③ `udp/443 → reject`、`udp/53 → direct`、其余 udp → direct；④ split 模式下 `domain_keyword → resi-pool`；`final` = global 时 `resi-pool`，split 时 `direct`。DNS 规则镜像：黑名单域名走 `dns_direct`。池空或 `enabled=false` → 全部 direct（fail-open）。
- **局限**（写进面板说明）：返回 200 拦截页的软封锁识别不了，靠 pin。

### 5.5 多组预留

以上全部按 `group_id` 作用域实现；v4 只有 `default`。扩展路径（不实现）：Reality 住宅按用户路由到组的 socks 出站，relay 的 `mixed`/`socks` inbound 配 `users[]`、规则用 `auth_user`（不是 `user`，后者匹配进程属主，S1/S2）分组；HY2 住宅每组一个 `hysteria-residential@<group>` 实例与端口。

---

## 6. Linux 客户端 `bui-c`（§⑤）

- 静态二进制（x86_64 / aarch64），`/opt/bui-c/{bin/sing-box, profiles.json, config.json}`，单元 `bui-c.service`（`sing-box run -c /opt/bui-c/config.json`，`Restart=always`）与 `bui-c.timer`（每分钟 `bui-c check`）。
- 引擎只有 sing-box（≤ 1.14）；Hysteria2 与 VLESS-REALITY 均为 sing-box 出站（uTLS chrome）。
- 模式：`socks`（`mixed` inbound 127.0.0.1:1080 与 127.0.0.1:8080）/ `tun`（`tun` inbound，`interface_name: bui-tun`，`stack: mixed`，`auto_route`，IPv6 接管与裸 v6 拒绝按 `2026-09-10-ipv6-takeover-design.md`，CN 域名直连 DNS，`sniff` + `hijack-dns`，cloudflared QUIC 例外，住宅节点的分流关键字）。切模式 = 重渲染 + 重启单元。DNS 用 typed server，**凡需经代理解析的 server 必须显式 `detour`**（typed server 不设 `detour` 时是空 direct dialer，不是默认出站，S5）；生成器禁止出现 `rule_set`、`download_detour`、legacy 字符串式 DNS server、`inet4_address/inet6_address`（S8/S9）。
- 节点来源：`/api/nodes/<user>`（首选，schema 同源）；`/api/sub` base64；粘贴 `hysteria2://`、`vless://`。多 profile，`switch` 切换。
- `check`：经本地 inbound 请求 gstatic 204；TUN 下核对接口与默认路由；失败退避重启（1/2/4 分钟）。
- `update`：来源顺序 面板 `/packages/` → GitHub Releases → 镜像；每日 timer 自动，可关。服务端内核缓存继续维护 sing-box 与 `bui-c` 的 Linux 二进制。
- `import-v3`：首次运行从 `/opt/hysteria-client/` 导入 profile，停用并删除 `hysteria-client / xray-client / bui-tun` 单元与 `hysteria-health.timer`。
- UFW 行为按 v3.6 现状迁移（TUN 开启时放行 `bui-tun` 接口）。
- 删除：三引擎互斥、死菜单簇、依赖自动安装、gum/fzf、`/tmp` 标记。

---

## 7. 安装、升级、发布（§⑥）

- `install.sh`（路径不变）≈ 100 行：架构识别 → 多源下载 `bui` → sha256 → `bui install [--import-v3]`。
- `bui install`：交互（域名、管理员密码、端口、可选住宅 URL）→ state → `reconcile()`。幂等：第二次运行零变更。
- `bui upgrade`：下载 `manifest.json` 指定版本的 `bui` → 原子替换 → `systemctl restart b-ui` → `reconcile()`（内核随 manifest 升级）。守护进程每日带抖动自检；面板 / CLI 手动；`--rollback` 恢复上一版二进制 + 最近一份 state 备份。
- 发布：GitHub Actions：`cargo test` → 渲染结果用真实内核校验（sing-box 1.12 / 1.13 / 1.14 `check`、`xray run -test`）→ musl 静态构建 → Release 附 `manifest.json`（`bui`、内核目标版本、sha256）。`CHANGELOG.md` 记录；首版 `v4.0.0`。

---

## 8. 测试策略

| 层 | 内容 | 位置 |
|---|---|---|
| 单元 | 四种粘贴格式解析；权益 → 节点集合；限额判断；月度重置；黑名单确认状态机；健康迟滞状态机；对账 diff 计算 | `cargo test` |
| Golden | 以 bwg-rick **迁移前**抓取并脱敏的 v3 样本（M1 第一项任务就是抓样本）（users.json、config、三种订阅输出）为 fixture，v4 渲染必须逐项相等 | `cargo test`，M1 验收的机器化版本 |
| 集成 | 渲染出的 hysteria / xray / relay / 客户端配置过真实内核校验；`sing-box check` 对未知字段（含嵌套层）严格报错（S10），是生成器的可靠门槛，1.13.19 与 1.14.x 都跑 | CI |
| 端到端 | §9 里程碑验收 | bwg-rick / baiyi |

---

## 9. 验收与灰度（§⑦）

| 里程碑 | 交付 | 验收标准 |
|---|---|---|
| M1 控制面闭环（2 周） | `bui install --import-v3`、对账器、四内核渲染、面板（旧前端）、三种订阅、`/api/nodes` | 每个现有用户三种订阅与 v3 逐项相同；sing-box JSON 过 1.12/1.13/1.14 `check`；v2rayN 四节点可连；二次 `install` 零变更；体检无漂移 |
| M2 住宅模块（1.5 周） | 池导入、健康切换、黑名单、体检、面板体检卡 | relay 重启后选中不变；24h 内学到 Decodo 的 `ports_allowed` 与支付域名；relay 日拒绝数较 v3 基线降 ≥ 90%；pin 立即生效 |
| M3 用户与流量（1 周） | 采样合并、限额执行、`auth-hook`、gRPC 增删、用户域 API 桩 | 加用户时 `NRestarts` 不变且在线会话不断；每节点 100MB 已知流量计数误差 ±5%；到期用户被拒并被踢；重启守护进程计数不重复 |
| M4 客户端（2 周） | `bui-c` 全功能 | baiyi 上四节点 SOCKS/TUN 均通；裸 IPv6 回落符合 spec；杀 sing-box 一分钟内自愈；从 v3 客户端原地升级不丢节点 |
| M5 硬化与发布（1 周） | soak、压测、升级回滚演练、CHANGELOG、v4.0.0 | 72h soak Xray RSS 无单调增长、`bui` RSS < 50MB；`auth-hook` 200 建连/秒 p99 < 20ms；`upgrade` 与 `--rollback` 各演练成功 |

**bwg-tizi 上线**：主理人在 bwg-rick 验收 M1–M5 后，低峰窗口执行 `bui install --import-v3`；执行前 `tar` 快照 `/opt/b-ui` + 单元文件，回滚 = 10 分钟内恢复 v3；快照保留 30 天。

**分工**：Opus 在 worktree 里按计划实现；Sonnet 完成 §10 的三项调研；Fable 审查每个里程碑并裁决验收；Haiku 一次性核对。

---

## 10. 实施前三项未知（已裁决）

调研与裁决见 `docs/superpowers/research/2026-09-11-v4-unknowns.md`（2026-09-11，两路 Sonnet + Fable 对账）；结论已并入 §3.2、§3.3、§4.2、§5.3、§5.4、§5.5、§6、§8。

| 项 | 裁决 |
|---|---|
| Hysteria2 `auth.type: command` | 可用：`<addr> <auth> <tx>`，stdout id，exit 0；无超时/无限流/无热加载 → 钩子自设超时 + 极简路径 + 自写日志；`userpass` 退路开关 M3 实现 |
| sing-box `auth_user` / `port_range` 取反 | 1.12–1.14 三版一致可用；`sing-box check` 严格校验可作生成器门槛；`resi-pool` 必须 `interrupt_exist_connections: false` |
| Xray gRPC proto 与 `QueryStats(reset)` | 可用：10 个 proto vendor；`reset` 单计数器原子；`RemoveUser` 不断既有连接是上游缺口，§4.2 已明写接受该窗口 |

---

## 11. 风险与对策

| 风险 | 对策 |
|---|---|
| `auth-hook` 在建连风暴下开销 | M5 压测门槛；`userpass` 退路作为配置开关在 M3 就实现 |
| sing-box 客户端 REALITY 指纹与 Xray 客户端不同 | M4 在 baiyi 实测；不通过则客户端保留 Xray 作为 VLESS 引擎（仅客户端，不影响服务端） |
| Xray 版本 REALITY 连接泄漏（调研 §1.7） | M5 72h soak；watchdog 兜底；内核版本受 manifest 控制可回退 |
| 黑名单误伤（把可用目标拉黑） | 需直连可达 + 两次确认；每日复核自动移除；面板可删条目 |
| relay 重启掐连接 | 只在 04:00 批量与 pin 时重启；重启后重放选择 |
| 对账器覆盖手工改动 | 非受管项只报不改；`--force` 才清理 |
| 单进程 `bui` 崩溃 | 数据面不依赖它（鉴权靠快照文件）；`Restart=always`；崩溃只影响面板与巡检 |
| 供应商软封锁识别不了 | 文档说明 + pin |

---

## 附录 A：用户域 API 结构（v4 返回 501）

- `POST /api/me/login` `{ "username", "password" }` → `{ "token", "expires_at" }`
- `GET /api/me` → `{ "user_id", "username", "entitlements", "usage", "expires_at" }`
- `GET /api/me/subscription-links` → `{ "sub", "singbox", "clash", "nodes" }`（四个 URL）
- `GET /api/me/entitlements` → 同 §4.1 `entitlements`
- `GET /api/me/billing` → `{ "currency", "balance_minor", "orders": [...] }`
- `POST /api/me/orders` `{ "sku", "quantity", "region" }` → `{ "order_id", "status": "pending", "amount_minor" }`
- `GET /api/me/orders/{id}` → 订单记录

## 附录 B：从审计到设计的对应

| 审计结论 | 本设计的处置 |
|---|---|
| 25 个迁移块 / 装机升级两条路（ctrl-*） | §2.2 对账器，单一代码路 |
| `setup_static_dns` / `configure_firewall` 死代码，块 D/E 才是活实现（ctrl-C6/C7 REFUTED） | §3.4 装机路径直接实现 |
| 节点集合 ×4 + 客户端模板（web-C15） | `bui-schema` 单一来源（§1、§4.4、§6） |
| 统计 execSync O(N) / fusion 不计 / 9998 不读 / 重启重复计 / 限额不执行（eff-C1~C4、web-C2~C5） | §3.3、§4.2 |
| 用户 CRUD 整进程重启（eff-C7） | §3.2 `command` 鉴权 + §3.3 gRPC |
| relay 无资源限制（eff-C5）、timer 只由 update.sh 创建（resi-C4） | §3.4 单元限制；巡检进程内（§5.3） |
| 三份 SSH、三份 cron、四份 GitHub 轮询、两份 cert-sync/watchdog | §3.4 一份实现；无 cron；升级在进程内（§7） |
| 创建用户 GET 明文密码 / install-key 无效 / JWT 每次重启失效（web-C6/C8/C17） | §4.3 |
| 客户端死代码 1290 行、三引擎互斥、健康检查只盯 HY2（client-*） | §6 单引擎 |
| `.bak` 无保留策略、手工漂移不可见（prod-*） | §2.1 备份保留 10 份；§2.2 漂移只报不改 |
