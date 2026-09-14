# b-ui 架构审计汇总报告（v3.6.3 基线，面向 v4 重设计）

日期：2026-09-11。作者：Claude Fable 5.1（汇总），依据 `2026-09-11-verdicts.json` 的 106 个反驳 agent 判决（78 条结论：54 CONFIRMED / 22 PARTIALLY / 2 REFUTED）。本文只做汇总与归纳，**不重新验证**；每条事实后标注来源判决 id，可回查 `verdicts.json` 的 `evidence` 字段。读取阶段（`readers-raw-UNVERIFIED.json`、`web-panel-findings-UNVERIFIED.md`）与判决冲突之处，一律以判决为准，并在 §0 单独列出。

生产主机一律用别名（`bwg-rick` 测试机、`bwg-tizi` 生产机）。

---

## 0. 读取阶段错误的纠正（已吸收，勿再引用旧说法）

| 旧说法（读取阶段） | 判决 | 真实情况 |
|---|---|---|
| `core.sh` 装机时已写静态 `resolv.conf`，update.sh 块 D 不可达 | **REFUTED**（ctrl-C6） | 反了：`setup_static_dns()`（core.sh:1289-1319）是**死代码**，从未被调用；update.sh 块 D（1738-1752）是**唯一**会把 DNS 锁静态的代码，只在下一次 cron 才生效。bwg-tizi 的 `chattr +i` resolv.conf 就是块 D 写的；bwg-rick 没有 systemd-resolved，两条路都没跑，至今用的是机房 DNS、未加锁 |
| `core.sh` 装机时已开 v3.5 住宅端口，update.sh 块 E 不可达 | **REFUTED**（ctrl-C7） | 反了：`configure_firewall()`（core.sh:1237-1283）是**死代码**，唯一调用者是未部署的 legacy `b-ui-server.sh`。新装机只有 `configure_port_hopping()` 开直连跳跃段；10002/tcp、40000/udp、41000-50000/udp 只有 update.sh 块 E 会开。两台生产机没装 ufw、firewalld 未激活，所以没露馅 |
| `npm install` 每 6 小时 cron 都跑 | PARTIALLY（ctrl-C18） | 只在「有新版本」分支跑（bwg-rick 日志：362 次空转 vs 2 次真更新）。真实缺陷是版本更新内部没按 `web/package.json` 是否变化门控 |
| bwg-tizi 多占的磁盘来自 `.bak` 堆积 | PARTIALLY（prod-C7） | ~3.6G 是 `/root/.vscode-server`（5 份缓存），1.2G 是第二个 `/swap.img`，日志多 0.24G；`.bak` 全部加起来 0.2MB。两台机 `/opt/b-ui` 都是 205M |
| update.sh 有 11 处 `.bak` 写入点 | PARTIALLY（prod-C4） | 8 处（682/703/734/774/858/1660/1675/1709）；719 是还原不是写入。bwg-tizi 21 个 config `.bak` 里 16 个不是 update.sh 写的（14 个来自 v3.5 前的 helper，2 个来自 obfs 开关） |
| 10 秒流量循环是第三个冗余触发点 | PARTIALLY（eff-C4） | 它是**唯一**持久化 `usage.total/monthly` 的地方，是流量限额的根基，不能作为冗余删除；可删的只是它与 `/stats`、`/online` 之间缺共享缓存 |
| `install_chinese_fonts()` 全仓库只有定义一处 | PARTIALLY（data-C3） | `b-ui-server.sh:849/1252` 有独立副本并自调用，但那是 legacy 单文件，不在 `version.json`；core.sh 副本在所有交付路径确实死，两台机都没装 fonts-noto-cjk |
| b-ui-relay 没有 LimitNOFILE，与 xray 等其它三个服务全都设了内存限制形成对比 | PARTIALLY（eff-C5） | relay 确实什么都没设（实测 fd 上限 524288 = systemd 默认，Nice 0，无内存上限）；但 xray 也**没有**内存限制，只有两个 hysteria 和 b-ui-admin 设了 |
| `syncKernels` 与 shell 侧 sing-box 版本上限不同 | PARTIALLY（web-C14） | 四处副本都是 `SINGBOX_MAX_MINOR="1.14"`，上限一致；真问题是 GitHub release 轮询逻辑重复了 4 次（server.js / core.sh / update.sh / b-ui-client.sh） |
| 客户端死菜单簇 ~1190 行 | PARTIALLY（client-C5） | 13 个函数共 **958 行**；读取阶段给的行号区间吞掉了活代码（`_switch_to_profile`、`cmd_list/switch/import` 等），删除时必须按函数逐个删 |

**额外两条观察（HANDOVER §1.2，未进入判决，提方案时要用）**：
- **生成器改了、既有机器不重生成**：`resi-pool` 的 `selector` 是 v3.6.0 引入的，bwg-tizi 的 `singbox-relay.json` 直到 2026-09-11 06:56 UTC 的 v3.6.3 更新才换上，滞后 1–2 个版本。同类实例：hy2-watchdog 脚本只在 timer 缺失时才写（prod-C3），bwg-tizi 至今跑的是 v3.5.14 之前的版本；反面极端是 cert-sync 重写块每次 cron 都无条件重写并打日志（ctrl-C12，rick/tizi 各 362/469 次）。
- **微信不走住宅**：两台机 24h 内 relay 几乎没见微信流量（rick 0 条、tizi 1 条）；被上游拒的全是 Apple push、Google、FCM:5228。v2rayN 微信图片问题与住宅上游无关（见 HANDOVER §1.3）。

---

## 1. 网络拓扑图（v3.6.3 实况，两台机一致）

```
                                   ┌───────────────────────────── VPS（2 vCPU / 1 GB，仅 IPv4 出口）─────────────────────────────┐
  v2rayN / bui-c                   │                                                                                                │
  ┌──────────────┐  UDP :10000     │  hysteria-server ──────────────── direct(mode:4, IPv4) ──────────────────────────────▶ Internet │
  │  节点①HY2直连 ├────(+20000-30000)─▶  config.yaml  · trafficStats 127.0.0.1:9999                                                    │
  │              │                 │                                                                                                │
  │  节点②Reality├── TCP :10001 ───▶  xray ┬ inbound vless-direct ───── freedom(ForceIPv4) ────────────────────────────────▶ Internet │
  │      直连    │                 │       │ (REALITY dest=masq:443, 同一 UUID/shortId)                                              │
  │              │                 │       │ api 127.0.0.1:10085                                                                     │
  │  节点④Reality├── TCP :10002 ───▶       └ inbound vless-residential ── socks ──┐                                                  │
  │      住宅    │                 │                                              │                                                  │
  │              │  UDP :40000     │  hysteria-residential ── acl relay(all) ─────┤ loopback                                          │
  │  节点③HY2住宅├────(+41000-50000)▶  config-residential.yaml · stats :9998        │                                                  │
  └──────────────┘                 │                                              ▼                                                  │
                                   │                          b-ui-relay (sing-box)  socks 127.0.0.1:2080  · Clash API 127.0.0.1:9091 │
                                   │                          singbox-relay.json（helper 唯一改写的数据面文件）                         │
                                   │                             ├─ global=true（两台生产机）: route.final = resi-pool                  │
                                   │                             ├─ global=false: domain_keyword(67 条) → resi-pool，其余 direct       │
                                   │                             ├─ 池为空: 全部 direct（fail-open，设计意图 data-C6）                  │
                                   │                             ├─ UDP/443 reject、UDP/53 direct、其余 UDP direct ⇒ 住宅路径实际只有 TCP │
                                   │                             └─ resi-pool = selector（不是 urltest）→ resi-1..N (socks5 / http)     │
                                   │                                          └──────────────▶ 住宅上游（现为 Bright Data HTTP 44445）──▶ Internet
                                   │                                                                                                │
  管理员浏览器 ── HTTPS :443 ──────▶  Caddy（ACME 签证书）── 127.0.0.1:8080 ──▶ b-ui-admin (Node, 单进程)                                │
                                   │     证书经 cert-sync.sh 同步到 /opt/b-ui/certs 供两个 hysteria 使用                                 │
                                   │     b-ui-admin ──shell-out──▶ residential-helper.sh ──写──▶ singbox-relay.json ──restart──▶ relay │
                                   │     b-ui-admin ──HTTP──▶ 127.0.0.1:9999（只读直连实例，9998 从未被读，web-C3）                       │
                                   │     b-ui-admin ──execSync `xray api stats`──▶ 10085（每 vless 用户 2 次，fusion 用户不统计，web-C4） │
                                   │                                                                                                │
  定时器/cron：b-ui-resi-health(2min→Clash API 9091 切 selector) · hy2-watchdog(5min) · b-ui-cert-sync(6h) · cron cert-check(12h)      │
               cron update.sh auto(6h) / kernel(12h) → apply_systemd_configs() 25 个迁移块每次全跑                                     │
  └────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘

  Linux 客户端 bui-c（/opt/hysteria-client）：三选一（systemd Conflicts=）
    SOCKS 模式  app → 127.0.0.1:1080/8080 → hysteria-client 或 xray-client → 节点
    TUN 模式    app → tun bui-tun → sing-box(singbox-tun.json, schema 8) → 节点   （TUN 模板与服务端订阅模板是两份独立模板）
```

要点：
- 四个节点 = 四个独立可拨的 `host:port`，客户端选节点即选端口；直连/住宅的区分**只编码在端口上**，两个 Reality inbound 共用 UUID/邮箱（data-C5），所以不能用 Xray 的 user 路由把它们并到一个端口。
- 直连两条路径零依赖 sing-box；住宅两条路径**无条件**经 relay，池为空时也过一次 relay 再 direct（fail-open，是 v3.5 之后的设计意图，不是事故）。
- Caddy 只服务面板，不终结任何代理流量；REALITY 伪装域名与 Caddy 无关。
- 住宅路径 TCP-only：relay 拒 UDP/443 迫使 QUIC 回落 TCP，DNS 与其它 UDP 直连。

---

## 2. 每节点跳数

| 节点 / 路径 | VPS 内进程数 | loopback 跳 | 外部跳 | 出口 IP | 备注 |
|---|---|---|---|---|---|
| ① HY2 直连 :10000 | 1（hysteria-server） | 0 | 0 | VPS IPv4 | 内置 direct，`mode: 4` |
| ② Reality 直连 :10001 | 1（xray） | 0 | 0 | VPS IPv4 | `freedom` + `ForceIPv4` |
| ③ HY2 住宅 :40000 | 2（hysteria-residential + relay） | 1（socks5 :2080） | 1（上游代理） | 住宅上游 IP | 池空时 loopback 跳仍在，出口回到 VPS IP |
| ④ Reality 住宅 :10002 | 2（xray + relay） | 1 | 1 | 住宅上游 IP | 同上 |
| DNS（global 模式） | relay 内 dns_resi 经 resi-pool 解析 | — | 1 | — | 数据流的目标域名**不解析**，原样交给上游 CONNECT |
| 面板 → 数据面 | Caddy → Node → helper(shell) → relay 重启 | 2 | 0 | — | 每次改住宅配置都是「写 JSON + restart relay」 |

loopback 一跳的实测量级是微秒级（调研报告 §1.9），**不是延迟问题**；它带来的真实成本是：多一个常驻进程、多一份 systemd 单元（且是唯一没设资源限制的那份，eff-C5）、多一处配置生成器、以及「池空也过 relay」在千用户下多出成千上万条本地 socket 对（readers 数据面 scaling_concerns）。

---

## 3. 组件判定表

判定词：**保留**＝能力与实现都留；**重写**＝能力留、实现换（v4 决策：控制面 + 客户端 Rust）；**合并**＝多份实现收敛成一份；**删除**＝零行为变化即可删；**重新落位**＝代码活着但放错了地方，删块前必须先在装机路径重建。行数为 v3.6.3 `wc -l`。

### 3.1 数据面（保留协议内核，不自实现）

| 组件 | 行数/位置 | 判定 | 依据 |
|---|---|---|---|
| hysteria-server（直连）+ Salamander + 端口跳跃 | core.sh 生成 | **保留** | 调研：GFW QUIC SNI 解密是 measured 能力，混淆是承重件 |
| hysteria-residential | core.sh 生成 | **保留**（能力） | 四节点 = 四端口的结构性需要 |
| xray 双 REALITY inbound | core.sh:775-841 | **保留** | 大陆无一手指纹化证据；客户端覆盖最广 |
| b-ui-relay（sing-box 本地 relay，selector 池） | residential-helper.sh 生成 | **保留能力，实现纳入 v4 住宅模块** | 出口 IP 声誉问题只能靠它解决；fail-open 语义要保留 |
| relay 的 systemd 单元 | helper:507-522 | **修**：补 LimitNOFILE / MemoryMax / Nice | eff-C5 |
| `type: http` 鉴权中间态 + `apply_hy2_userpass_auth` 二次改写 | core.sh:560-564/618-622/662-692 | **删除**（直接写 userpass） | web-C2、eff-C7 |
| `configure_firewall()` | core.sh:1237-1283（47 行） | **删除**；但 v4 装机路径要**新增**真正的开端口调用 | data-C1（REFUTED ctrl-C7） |
| `setup_static_dns()` | core.sh:1289-1319（31 行） | **删除**；静态 DNS 由 update.sh 块 D **重新落位**到装机路径 | data-C2（REFUTED ctrl-C6） |
| `install_chinese_fonts()` | core.sh:394-410（17 行） | **删除** | data-C3 |
| `install_nginx`/`configure_nginx_proxy` 别名 | core.sh:363-366/913-916 | **删除**（install.sh 两处调用改名） | data-C4 |
| 证书同步三重触发（timer 6h + cron 12h + 装机一次） | core.sh:923-1040、1193 | **合并**为一个 timer | readers 数据面；cert-check.sh 在 bwg-tizi 从第一天起就不存在（prod-C2） |
| systemd 模板单元 hysteria-server@/xray@ | 上游安装器副产物 | **删除**（装后 `rm`），不在仓库 | prod-C1 |

### 3.2 服务端控制面（v4 重写为 Rust，此表指出现有实现里什么是纯冗余）

| 组件 | 行数/位置 | 判定 | 依据 |
|---|---|---|---|
| `apply_systemd_configs()` 25 个迁移块 | update.sh:756-1789（1034 行） | **删除 ≈ 680 行**；块 D（15 行）、块 E（14 行）、D8 resi-health timer（53 行）**重新落位** | ctrl-C1~C5、C8~C13、C15、C17、prod-C3、resi-C2/C3；ctrl-C6/C7 REFUTED；resi-C4 |
| `migrate_admin_env`、`migrate_ssh_hardening`、`update_service_paths` | update.sh:575-659、368-406 | **删除** | ctrl-C11、C14、C15 |
| cert-sync 重写块 | update.sh:1433-1521（87 行） | **删除**（与 core.sh 仅差 1 行注释，每次 cron 空转重写） | ctrl-C12 |
| hy2-portjump-cleanup 第二份 | update.sh:932-978（48 行） | **合并**到一份 | ctrl-C13（0ce9df2 证明每次修都要改两处） |
| SSH 硬化 ×3 | core.sh:1867-1935 / update.sh:614-659 / b-ui-cli.sh:1157-1208 | **合并**为一份（3.4.37 已因分叉出过 bug） | ctrl-C14 |
| cron 写入 ×3 | core.sh:1162-1230 / update.sh:2030-2071 / install.sh:848-853 | **合并** | readers 控制面 |
| GitHub release 轮询 ×4 | server.js:1284-1315 / core.sh:1541-1562 / update.sh:1808-1829 / b-ui-client.sh:1128-1149 | **合并** | web-C14 |
| 远程文件清单 ×3（install.sh / do_update / auto_update） | — | **合并**（version.json 本应是唯一真源） | readers 控制面 |
| `install_tui_tools` ×2 + gum/fzf 下载 | install.sh:688-758 / update.sh:2233-2297 | **删除**（TUI_AVAILABLE 永久 false） | hist-C2/C3、ctrl-C17 |
| install.sh 旧版迁移（install_type==0、check_old_version、migrate_old_path、update_service_paths、create_global_command 残留） | install.sh:343-463、621-631、784-790 | **删除** | ctrl-C16 |
| b-ui-cli.sh 内联内核更新回退 | b-ui-cli.sh:523-541 | **删除** | ctrl-C19 |
| b-ui-cli.sh gum/fzf 包装 | b-ui-cli.sh:115-158、307 | **删除**（保留 fallback 分支） | hist-C2 |
| 交互菜单住宅项 1/3 走单 URL 覆盖路径 | b-ui-cli.sh:822、855 → helper:730 | **修**：在两台机上执行会清空 `urls[]` 毁掉池 | resi-C8 |
| `.bak.*` 无保留策略 | update.sh 8 处写入、helper、cli | **删除**机制，v4 用单一备份目录 + 保留 N 份 | prod-C4/C5 |
| `b-ui-server.sh`（legacy v3.1 单文件） | 1332 行 | **删除** | hist-C1 |

### 3.3 Web 面板（v4 重写为 Rust）

| 组件 | 位置 | 判定 | 依据 |
|---|---|---|---|
| 节点集合逻辑 ×4（/api/sub、/api/subscription、/api/clash、app.js genUri） | server.js:706-760、909-934、1850-1907；app.js:292-351 | **合并**为一份 schema 多格式渲染 | web-C15；调研 §1.10 |
| 流量统计：`execSync` 每 vless 用户 2 次、3 个触发源无缓存 | server.js:1071-1144、1423 | **重写**：单次 `statsquery`、异步、共享缓存；**且要把 fusion 用户纳入**（现在 Reality 流量完全不计） | eff-C1~C4、web-C4 |
| 住宅 HY2 流量从不读取（9998 常量无读者） | server.js:51、1034-1066 | **修**：对 9999+9998 双端汇总 | web-C3 |
| 重启后首个 tick 把累计值当增量重复计入 | server.js:1422-1441 | **修**（`clear=1` / `-reset` 或持久化 lastTraffic） | web-C5 |
| 限额检查 `checkUserLimits` + `/auth/hysteria` | server.js:1149-1155、2690-2712 | **删除**；但注意：**限额目前哪里都没执行** | web-C2 |
| 用户 CRUD → 整进程重启 hysteria/xray | server.js:302-389 | **重写**：按用户增量（Xray API / Hysteria userpass 热加载方案待定） | eff-C7 |
| `handleManage` delete/update/list、`POST /api/users` | server.js:1203-1226、1960-1975 | **删除** | web-C7 |
| 创建用户走 `GET /api/manage?key=<管理员密码>`、密码存 localStorage | app.js:61、190；server.js:1157-1201 | **重写**为 JWT 保护的 POST | web-C6 |
| `/api/install-command` 在 Bearer 检查之前返回 install key；`/packages/b-ui-client.sh` 无 key | server.js:1770-1780、1633-1691 | **重写**（install-key 机制目前安全上为零） | web-C8 |
| JWT secret 每次进程启动随机 | server.js:41 | **修**：持久化 | web-C17 |
| `generateBootstrapScript` + GitHub 回退 | server.js:132-192、1541-1570 | **删除** | web-C9 |
| `GET /api/kernel-downloads` | server.js:1739-1767 | **删除** | web-C11 |
| ws-tls inbound 构建（证书路径错、域名正则永不匹配） | server.js:349-384、353-356 | **删除**；若 8 项能力含 WS-TLS 则按 `/opt/b-ui/certs` 重做并进三个生成器 | web-C16 |
| 端口跳跃 POST 写 iptables 而非 `listen:` 行 | server.js:2093-2152；core.sh:1387-1400；update.sh:1313-1334 | **重写**为改 `listen:` + restart；iptables 清理块随之删 | web-C13 |
| `sing-box-tun`/`sing-box-client` 停用行、`/tmp/hy2-watchdog-fail-count` 读取 | server.js:1591-1594、2546 | **删除** | web-C10、web-C12 |
| `singbox-converter` 导入 + core.sh 硬编码 package.json + 重试块 | server.js:8；core.sh:1678、1701-1712 | **删除**（面板层不再有 npm 网络出口） | web-C1 |
| 每个订阅请求同步 spawn helper `domains`（~26ms） | server.js:492-506 | **合并**进同一份 schema（顺带消失） | web-C18 |
| `syncKernels` 6h 轮询 + `update.sh kernel` 12h + core.sh 装机下载 | server.js:1242-1419 等 | **合并**为一处 | web-C14 |

### 3.4 住宅出口模块（v4 内含自动黑名单 R13）

| 组件 | 位置 | 判定 | 依据 |
|---|---|---|---|
| residential-helper.sh + resi-health.sh | 858 + 146 行 | **重写**为一个 Rust 常驻 agent（配置生成 + 健康探测 + selector 切换 + 黑名单） | 用户决策；resi-C4（timer 只有 update.sh 才建） |
| curl 代理配置 `-K -` 构造 ×3（helper / resi-health / server.js） | helper:67-76、resi-health:34-43、server.js:546-573 | **合并** | resi-C5/C6 |
| `LEGACY_DEFAULT_DOMAINS_V3_4_17`、`migrate_residential_keywords`、A.fix2 | helper:113-116；update.sh:510-568、1653-1668 | **删除** | resi-C1/C2/C3 |
| 三种探测（add 时 verify / 周期健康 / 面板体检分类） | — | **保留**三种目的，合并底层 | resi-C6 |
| 单 URL 旧字段与 `urls[]` 双表示 | helper:578-597 | **删除**旧表示 | readers 住宅 |
| global / 关键字分流两种模式 | helper:365-417 | **保留**两种（新装默认非 global；删关键字模式等于改默认行为，且 R13 黑名单按关键字模式建模） | resi-C7 |

### 3.5 Linux 客户端（v4 重写为 Rust）

| 组件 | 位置 | 判定 | 依据 |
|---|---|---|---|
| 死菜单簇 13 函数 | 958 行（import_node 4680-4795、service_control_menu 4801-4931、config_management 3256-3280、import_from_subscription 3286-~3480、list_configs/switch_config/delete_config/import_batch/activate_imported_config/_configure_and_save/quick_install/configure_client/import_from_uri） | **删除** | client-C5/C6/C7 |
| `check_dependencies` 簇 + `detect_package_manager` | 784-958（~175 行） | **删除**（依赖自动安装从未真正跑过） | client-C3 |
| `print_banner`、`tui_write`、`get_server_ip`、`get_current_ssh_ip` | 44-57、300-314、1053-1067 | **删除** | client-C1/C2/C4 |
| gum/fzf 分支（8 个 tui_* 包装 + 2 处内联门） | 204-303、5346、5413 | **删除**（保留 fallback） | hist-C2 |
| `tui_import_node` 拒绝 https:// 并指向已死的订阅导入 | ~5443 | **修**：订阅导入要么复活要么改提示 | client-C7 |
| 健康检查只盯 hysteria-client，VLESS 活跃时每分钟重启 hysteria-client | 3912-3981 | **修**：协议感知 | client-C8 |
| 三服务 stop 序列手写 ×3（cmd_stop / cmd_restart / uninstall）+ 各处两服务 stop | 2940-2959、4555-4587 | **合并** | client-C9 |
| TUN 模板（`generate_singbox_tun_config`，schema 8）与服务端订阅模板两份 | 1377-1651 | **合并**进同一份节点 schema（客户端渲染） | CLAUDE.md、调研 §1.10 |
| TUN 就绪/互斥/UFW 切换逻辑 | 1906-2217 | **重写**（Rust 客户端基于 tun2proxy 或 sing-box 子进程，方案阶段定） | readers 客户端 |

---

## 4. 反复横跳（历史上改了又改回的决定）

| 主题 | 轨迹 | 教训 |
|---|---|---|
| 住宅出口拓扑（6 种线上形态） | v3.3.0 各自域名匹配拨 socks5 → v3.3.5 本地 relay → v3.3.8 relay 永久常驻 → v3.4.0 sing-box 接管全部路由/DNS → **v3.5.0 全面反转**为两套独立实例，直连零依赖 sing-box → v3.5.7/v3.5.9 两个补丁清理迁移残留 | 「所有流量过一跳」被生产复盘认定为 v3.4.x broken-pipe/DNS 噪音根因；v4 不要再回到单路由器形态（hist 读取 + 调研 §4） |
| Hysteria2 鉴权 | `type: http` 回调面板 → v3.5.15 改本地 userpass（面板是 SPOF）| 但 core.sh 至今仍先写 http 再改写成 userpass；`/auth/hysteria` 端点留着（web-C2） |
| 端口跳跃 NAT 清理 | hy2-nft-cleanup.sh（v3.4.42）→ 误删另一实例规则（v3.5.1）→ 去掉 ExecStartPre → hy2-portjump-cleanup.sh（v3.5.14）→ 空链修补（v3.5.16）| 两个 hysteria 实例共享 nft 表是根因；v4 若保留双实例要在设计上隔离 |
| 订阅主机 | v3.5.0 改 IP 字面量（防 DNS 污染）→ v3.5.8 改回域名（客户端 DoH 预解析） | — |
| gum/fzf 箭头菜单 | 2026-05-07 当天设计+上线 → 05-08 `TUI_AVAILABLE=false` 关掉，代码全留 | 用户偏好数字菜单（记忆 cli-menu-preference）；v4 不要箭头 TUI |
| TUN 模板 schema | v1→v8，至少 3 次生产回滚；`interface_name` 03-20 被删、v3.6.3 补回 | 模板要有回归测试，不是靠 schema 号 |
| 住宅关键字表 | 10 → 25 → 31 → 64 → 67 条，每次都是被 403 打了才补 | 这就是 R13 自动黑名单要替代的手工打地鼠 |
| 2026-05-07 住宅 spec 的「非目标」 | 明确排除多上游池、HTTP 上游 → 两者都在 v3.4.19/v3.5.0 上线 | 46% 的 spec/plan 行数已与运行代码不符（hist 读取）；v4 spec 要标「可废弃条件」 |
| README | 版本停在 v3.5.18，端口表把 :10002 写成 WS-TLS | hist-C7 |

---

## 5. 生产实况（2026-09-11，只读 SSH）

| 项 | bwg-rick（测试机，2026-06-03 重装） | bwg-tizi（生产，2026-05-09 起未重装） |
|---|---|---|
| 版本 / 拓扑 | v3.6.3，6 服务 + 2 timer 全 active | 同 |
| 硬件 | 2 vCPU / 1020 MB | 同 |
| 内存 | 用 231 MB，swap 0 | 用 324 MB，**swap 86 MB 在用** |
| 磁盘 | 2.1G / 19G（13%） | 7.5G / 19G（43%）：3.6G vscode-server + 1.2G 第二个 swap.img，与 b-ui 无关 |
| 住宅池 | Bright Data HTTP 44445 ×1，global=true | 同 |
| relay 日志 | ~31.8k 行/24h，几乎全是上游 403/rejected（Apple、Google、FCM:5228） | ~31.6k 行/24h，同 |
| hysteria-residential 日志 | 3.8k 行/24h | **72k 行/24h**：手工 drop-in `99-debug.conf`（2026-05-14）开了 `--log-level debug`（prod-C6） |
| 静态 DNS | **未锁**，机房 DNS 203.0.113.x（块 D 守卫不触发） | 已锁，内容 = update.sh 块 D heredoc |
| 防火墙 | 无 ufw，firewalld 未激活 → 端口全靠云侧 | 同 |
| hy2-watchdog.sh | v3.5.14+ 版本 | **v3.5.14 之前的版本**（写 `/tmp/hy2-watchdog-fail-count`），面板 fail_count 恰好在此机是活的 |
| cert-check.sh cron | 文件存在 | **文件从未存在过**，每 12h 报 not found（241/241 次） |
| b-ui-admin 内存上限 | 来自 update.sh D5 drop-in（主单元早于 v3.5.14） | 同 |
| `.bak` | 8 个（本周期） | 21 个 config `.bak` + 手工残留（`.bak.null/.rmdc/.pre-null/.namebak/.bak.ippure`，无任何代码会产生） |
| 模板单元 hysteria-server@/xray@ | 存在、从未激活 | 同 |
| relay 配置 | selector | selector（**直到 2026-09-11 06:56 UTC 才换上**） |
| sshd | :22 | 非标准高位端口，直连被掐，需 `-J bwg-rick` |
| 无关负载 | cron 每分钟一个其它项目的 keepalive 脚本 | — |

结论：两台机数据面一致且健康；差异全部来自「增量迁移随机器寿命退化」——bwg-tizi 的每一处漂移（watchdog 版本、debug drop-in、残留文件、cron 指向不存在的脚本）都是 update.sh 只补不收的结果。这直接支持已定的「v4 两台机重装」。

---

## 6. 千人评估（现有实现在 ~1000 用户下会先撞到什么）

按会先撞到的顺序：

1. **硬件**：2 vCPU / 1 GB 是 QUIC + REALITY 加解密的硬顶，项目自己的 3.5.15 changelog 已写明「几十个用户同时打满带宽即触顶」。这不是代码问题；千人必须多机（已定为二期）。
2. **面板统计的 O(N) 阻塞**（eff-C1~C4）：目前是**空转**（两台机所有用户都是 fusion，过滤器把他们排除在外），代价是 fusion 用户的 Reality 流量根本没计入（web-C4）。一旦 v4 把 fusion 纳入，如果沿用「每用户两次 execSync、5s+5s+10s 三个触发源」的模式，千用户每 tick 会阻塞事件循环数秒，拖垮所有订阅端点。v4 必须用一次 `statsquery` + 共享缓存。
3. **住宅 HY2 流量从未计入**（web-C3）+ **重启后重复计入**（web-C5）+ **限额哪里都不执行**（web-C2）：流量计费在 v3.6.3 是不可信的。
4. **用户增删 = 整进程重启**（eff-C7）：千人规模下每次开号都是全体掉线。
5. **relay 单进程无资源限制**（eff-C5）：全部住宅流量串行过一个 sing-box，fd 上限靠系统默认，无内存上限；同时 global 模式下每天 3 万条被拒连接是白白消耗的 CPU/IO（R13 黑名单要解决）。
6. **resi-health timer 只由 update.sh 创建**（resi-C4）：新装机或改池后最长 6 小时没有故障切换。
7. **切换策略是「第一个健康的」**（readers 住宅）：所有用户同时压到同一个上游 IP；上游并发承受力比 UDP 支持更该优先核实（供应商报告 §7）。
8. **安全面**：JWT 每次重启失效（web-C17）；创建用户走 GET 明文管理员密码（web-C6）；install-key 形同虚设（web-C8）；`/api/manage` 无 JWT。这些在千人对外服务时是必修。
9. **运维面**：修复只能等 6h cron（无 push）；三份 cron 写入不原子；GitHub 匿名 60 次/小时在多机共享出口时会撞限。

---

## 7. 简化路线图（v4 重装前提下）

### 7.1 可删行数（零行为变化，按判决行号累计，估算）

| 文件 | 现有行数 | 可直接删 | 说明 |
|---|---|---|---|
| `b-ui-server.sh` | 1332 | **1332** | 整个 legacy 文件（hist-C1） |
| `b-ui-client.sh` | 5810 | **~1290** | 死菜单簇 958 + 依赖簇 ~175 + 孤儿 ~54 + gum/fzf 分支 ~100（client-C1~C7、hist-C2） |
| `server/update.sh` | 2327 | **~680** | 迁移块 ~709 减去必须重新落位的块 D/E 29 行；v4 整个更新器另写，此数只表示「现有代码里纯冗余」 |
| `web/server.js` | 2721 | **~230** | /auth/hysteria、handleManage 死分支、POST /api/users、kernel-downloads、bootstrap 生成器 + GitHub 回退、ws-tls 构建、fail-count 读取、sing-box-tun 停用行、singbox-converter、9998 常量（web-C1/C2/C3/C7/C9/C10/C11/C12/C16） |
| `install.sh` | 869 | **~210** | 旧版迁移 ~137 + install_tui_tools 71（ctrl-C15/C16/C17） |
| `server/core.sh` | 1995 | **~130** | 三个死函数 95 + nginx 别名 6 + http 鉴权中间态与二次改写 ~30（data-C1~C4、web-C2） |
| `server/b-ui-cli.sh` | 1381 | **~65** | gum/fzf 包装 ~45 + 内核更新回退 19（hist-C2、ctrl-C19） |
| `server/residential-helper.sh` | 858 | **~10** | LEGACY 表、旧单 URL 字段路径（resi-C1） |
| **合计** | **18,661**（含 legacy） | **≈ 3,950（≈ 21%）** | 另有 **≈ 500–600 行**属「多份实现合并成一份」的净减（SSH ×3、cron ×3、GitHub 轮询 ×4、节点集合 ×4、cert-sync ×2、portjump ×2、watchdog ×2、curl-cfg ×3、三服务 stop ×3） |

估算口径：以 `verdicts.json` 的 correction 行号为准，取整；未做逐行复核。

### 7.2 删块前必须重新落位的「活迁移块」

| 块 | 现在的唯一实现 | v4 落位 |
|---|---|---|
| 静态 DNS（update.sh 块 D） | 只在 cron 生效，且只对 systemd-resolved 主机 | 装机路径一份实现（bwg-rick 这种无 resolved 的主机也要处理） |
| 住宅端口开放（块 E） | 只在 cron 生效 | 装机路径开全部基础端口（22/PORT/80/443/10001/10002/40000/跳跃段） |
| resi-health timer（D8） | 只在 cron 生效 | 住宅 agent 自带 |
| b-ui-admin 内存上限（D5） | 两台机都靠 drop-in | 主单元内置 |
| hy2-watchdog（prod-C3） | 只在 timer 缺失时写 | 生成器输出要能与机器上现状对账（版本戳或内容比对） |

### 7.3 结构性收敛（与 v4 方案直接相关，供 brainstorming 用）

1. **一份节点 schema**：/api/sub、/api/subscription、/api/clash、app.js genUri、客户端 TUN 模板五处收敛成一份数据 + 多个渲染器；端口/标签/obfs 只改一处。
2. **一份系统状态生成器，能与现状对账**：装机与升级不再是两条代码路（core.sh vs update.sh），而是「期望状态 → diff → 应用」；这一条同时消灭 25 个迁移块、三份 cron 写入、两份 cert-sync、两份 watchdog，并解决 §0 的「生成器改了机器不跟」。
3. **住宅模块自包含**：relay 配置生成、健康探测、selector 切换、黑名单探测（R13）、面板体检分类，一个 agent，一份 curl-cfg。
4. **面板去 shell-out、去 execSync**：统计走 API 一次拉全量；用户变更走增量；鉴权与限额真正执行。
5. **客户端**：先删 1290 行，再决定 Rust 客户端里 TUN 用什么承载。

---

## 8. 待决问题（本报告不裁决，进入 brainstorming）

1. **住宅路径要不要保留本地 socks 那一跳**：保留 = 沿用 fail-open、selector 热切换、四端口结构不变；去掉 = hysteria-residential / xray 直接拨上游，失去热切换与黑名单分流的落点。判决只证明这一跳不是延迟问题，是运维复杂度问题。
2. **两个 Reality inbound 共用 UUID**：是否改为每用户两组凭据，以便未来在同一端口上按用户路由？会改变订阅内容。
3. **新装默认 global 还是关键字**：两台机都 global；新装默认非 global；R13 按关键字模式建模。
4. **端口跳跃写法**：`listen:` 行（v3.6.0 起的真源）还是 iptables REDIRECT（面板现在写的）。
5. **WS-TLS 是否在 8 项能力内**：现实现已坏（web-C16）。
6. **流量计费口径**：直连 + 住宅 + Reality 三路合并，重启不重复计，限额真正执行——需要 Hysteria2 / Xray 各自的热加载方案。
7. **Xray 26.3.27 REALITY 泄漏风险**：调研 §1.7 建议上线前 48–72h soak；是否列入 v4 验收。
8. **hysteria 双实例共享 nft 表**：v4 是否换成单实例多监听或明确隔离。
