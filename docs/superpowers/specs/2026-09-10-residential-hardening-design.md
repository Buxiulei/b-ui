# 住宅代理硬化设计（v3.6.0 三项修复）

- 日期：2026-09-10
- 目标版本：v3.6.0（与 IPv6 接管同版本，独立提交）
- 状态：已批准，待实施
- 关联：`docs/superpowers/specs/2026-09-10-ipv6-takeover-design.md`

## 1. 背景

2026-09-10 对住宅代理链路（`server/residential-helper.sh`、`server/resi-health.sh`、`web/server.js` 住宅相关接口）做了审计。架构本身成立：一个常驻本地 sing-box 中继（`127.0.0.1:2080`）做统一出站路由，Hysteria2 住宅实例与 Xray 住宅 inbound 在安装时写死指向中继，之后只改中继自身配置；目标域名原样透传给住宅 SOCKS5 由上游解析。

审计发现并经复核确认的问题中，本次修三项（用户 2026-09-10 选定）：

| # | 问题 | 证据 |
|---|---|---|
| R1 | `web/server.js` `getResidentialConfig()` 在 `residential-proxy.json` 的 `.domains` 为 null 时回退成 `[]`；`residential-helper.sh` `get_domains()` 回退成 31 条 `DEFAULT_DOMAINS`。结果 sing-box / Clash 订阅在"启用住宅但从未自定义域名"这一默认状态下不产生任何 `domain_keyword` 规则，`final` 永远是直连池，住宅线路对这两类客户端形同虚设。 | `web/server.js:437-450, 577, 755`；`server/residential-helper.sh:38-66, 325-349`（`save_config` 把 `domains` 持久化为 null） |
| R2 | 中继 unit 无 `ExecReload`，任何池变动（管理员操作或 2 分钟一次的健康巡检自动切换）都 `systemctl restart b-ui-relay`，掐断所有用户的全部住宅路径连接。两脚本对 `singbox-relay.json` 无锁读改写；`save_config()` 直接重定向写 `residential-proxy.json`，非原子。 | `residential-helper.sh:289-316, 347`；`resi-health.sh:64-79` |
| R3 | 住宅 SOCKS5 密码以 curl 命令行参数明文出现在三处（`ps` / `/proc/<pid>/cmdline` 可见），其中巡检每 2 分钟一次。 | `residential-helper.sh:106-109`；`resi-health.sh:45-46`；`web/server.js:2152-2161` |

本次不做（记录待办）：4 份 AI 域名列表合并、`LEGACY_DEFAULT_DOMAINS_V3_4_17` 死代码、`ensure_singbox()` 版本下限、`.resi-health-state.json` 权限、旧 spec 更新。

## 2. 设计

### R1 域名回退统一

**原则**：默认域名列表只存在于 `residential-helper.sh`，其它读者通过它取"生效列表"。

- `residential-helper.sh` 新增子命令 `domains`：调用 `get_domains` 后输出 JSON 数组（`printf '%s\n' "${DOMAINS[@]}" | jq -R . | jq -sc 'unique'`），退出 0。不需要 root（只读 `residential-proxy.json`；文件 600 属 root，server.js 本就以 root 运行）。`usage` 里补一行。
- `web/server.js`：
  - 新增 `getEffectiveResidentialDomains()`：`spawnSync(CONFIG.residentialHelper, ["domains"], { encoding: "utf8", timeout: 3000 })`，stdout 解析为数组；任一失败（文件不存在、非 0 退出、JSON 解析失败）返回 `[]`。
  - `getResidentialConfig()`：`domains` 字段改为：文件里 `.domains` 是非空数组则用它，否则用 `getEffectiveResidentialDomains()`。
  - `GET /api/residential` 的显示回退（约 `web/server.js:1899-1927`）删除内联 `DEFAULT_DOMAINS` 常量，改用 `getEffectiveResidentialDomains()`。
  - `residential/health` 处理器已优先读 `singbox-relay.json` 的实际规则，保持不变。
- 效果：默认状态下 sing-box/Clash 订阅出现 `domain_keyword` 规则，与服务端中继一致；管理员自定义域名后两边同样一致。

### R2 锁、原子写、重启冷却

**锁**：

- 锁文件 `${BASE_DIR}/.relay.lock`（root 600）。
- `residential-helper.sh`：在 `main`/子命令分发处，对所有会写 `residential-proxy.json` 或 `singbox-relay.json` 的子命令（`enable`、`disable`、`reapply`、`global`、`set-domains`、`--add`、`--remove` 等写路径）用 `exec 9>"$LOCK"; flock -w 30 9 || { err "获取锁超时"; exit 1; }` 持锁到进程结束。`status`、`domains` 等只读子命令不加锁。
- `resi-health.sh`：探测循环**不持锁**（可长达数十秒）；仅在读取当前池 `cur`、改写 `singbox-relay.json`、restart 这一段用 `flock -w 30`（同一锁文件）。持锁后重新读取 `cur`，与探测得出的 `des` 比较后再写，避免用陈旧快照覆盖管理员刚做的修改。

**原子写**：

- `save_config()`：`jq ... > "${RESIDENTIAL_CONFIG}.tmp" && mv "${RESIDENTIAL_CONFIG}.tmp" "${RESIDENTIAL_CONFIG}"`，`chmod 600` 放在 mv 之后（先 chmod tmp 再 mv 亦可，保证任一时刻文件不为 644）。
- `set-domains` / `global` 等分支里存在"文件不存在时直接写"的非原子路径，统一改为 tmp+mv。

**重启冷却（仅自动巡检）**：

- `resi-health.sh` 新增 `RESTART_COOLDOWN="${RESI_HEALTH_RESTART_COOLDOWN:-600}"`（秒）。
- 状态文件 `.resi-health-state.json` 增加顶层键 `_last_restart`（epoch 秒）。
- 当 `des != cur` 且 `now - _last_restart < RESTART_COOLDOWN`：只 `log "住宅池需变更 ${cur} → ${des}，重启冷却中（剩余 Ns），延后"`，不改文件、不重启；下一轮重新评估。
- 否则照常改写 + restart，并写 `_last_restart=now`。
- 理由：sing-box urltest 本身会跳过探测失败的出站，池成员延后剪枝不影响可用性；冷却把最坏情况下的全体掉线频率从 4 到 10 分钟一次降到 10 分钟一次以上。管理员手动操作不受冷却限制。
- 状态文件里既有按 tag 的对象也有 `_last_restart`，探测循环里 `jq '.[$n]=…'` 的写法不受影响；读取 tag 时 `_last_restart` 不是 tag，循环只遍历 relay 里的 socks 出站，不会误读。

### R3 凭据不上命令行

三处 curl 改为 `-K -`（从 stdin 读配置），凭据以 `proxy-user` 行传入，避免 URL 编码问题：

```
proxy = "socks5h://HOST:PORT"
proxy-user = "USER:PASS"
```

- curl 配置文件双引号内需转义 `\` 与 `"`；用户名/密码含换行的直接报错退出（视为非法）。
- `residential-helper.sh` `verify()`：
  ```bash
  exit_ip=$(printf 'proxy = "socks5h://%s:%s"\nproxy-user = "%s:%s"\n' "$host" "$port" "$(cfg_escape "$user")" "$(cfg_escape "$pass")" \
      | curl -sS --max-time 10 -K - https://api.ipify.org 2>/dev/null)
  ```
  新增 `cfg_escape()`（sed 转义 `\` 与 `"`）。无凭据时只输出 `proxy` 行。
- `resi-health.sh` 探测循环同样改写（同一 `cfg_escape` 内联实现，脚本独立不 source helper）。
- `web/server.js` `residential/health`：`execFile("curl", ["-K", "-", "-m", "8", "-sS", "<url>"], …)` 得到 `ChildProcess`，向 `child.stdin` 写入上述两行后 `end()`；转义函数 `curlCfgEscape(s)`。回调逻辑不变。
- 验证：`ps -o args` 在探测期间不含密码；行为与改前一致（用一个本地 socks5 stub 或对比 `curl` dry-run 输出）。

### R4 中继显式处理 UDP（住宅 SOCKS5 基本不支持 UDP ASSOCIATE）

来源：住宅最佳实践调研 P0-1。现状：中继 route 无任何 `network: udp` 规则；`global=true` 时 `final: resi-pool`，客户端的 QUIC(443/udp) 与 DNS(53/udp) 全被塞进住宅 SOCKS5；RFC1928 允许服务器不支持 UDP ASSOCIATE，主流住宅供应商未文档化 UDP 能力。

- `residential-helper.sh` 两个 `write_singbox_config_*` 的 `route.rules` 在 `sniff` 之后、私网规则之前插入（顺序固定）：
  1. `{"network": "udp", "port": 53, "outbound": "direct"}` — DNS 走 VPS 直连；
  2. `{"network": "udp", "port": 443, "action": "reject"}` — QUIC 立即拒绝，浏览器回退 TCP，TCP 仍按后续规则走住宅；
  3. `{"network": "udp", "outbound": "direct"}` — 其余 UDP（游戏/STUN 等）直连 fail-open。
- 直连模式（空池）也加同样三条（行为一致，便于测试）。
- 不改 Hysteria2 ACL 与 Xray 配置：hy2 住宅实例的 socks5 出站与 xray relay 出站的 UDP 都进中继，由中继统一裁决。
- 已知取舍：global 模式下非 QUIC 的 UDP 会用 VPS IP 出去（与 TCP 的住宅身份不一致）；对 AI 服务无影响（只用 TCP/QUIC）。

### R5 三层探测降频

来源：住宅最佳实践调研 P0-2。现状：中继 urltest `interval: 10s` / `tolerance: 50`；客户端 sing-box 订阅 urltest `interval: 10s` 且 `interrupt_exist_connections: true`；巡检每 2 分钟 × 3 次 curl。sing-box 官方默认 `3m / 50 / idle 30m`；Bright Data 按每 IP 请求速率限流（429）。

- 中继（`residential-helper.sh` urltest）：`interval: "3m"`，`tolerance: 500`（毫秒；避免延迟抖动引起出口漂移），`idle_timeout: "30m"`（显式写出，默认值）。
- 客户端 sing-box 订阅（`web/server.js` `generateSingboxConfig` 的 `urltest()`）：`interval: "60s"`，`interrupt_exist_connections: false`，`tolerance: 100`。Clash 订阅已是 300s，不动。
- 巡检（`resi-health.sh`）：默认 `TRIES=2`、`OK_NEED=1`（一轮内任一成功即健康，迟滞仍是 2 轮）；`update.sh` 的 `b-ui-resi-health.timer` 加 `RandomizedDelaySec=30s`（新装与既有 timer 文件都要有：D8 块生成的 unit 文本加此行，并对已存在但缺该行的 timer 做一次幂等补丁 + `daemon-reload`）。
- 验收：生成的中继配置三版 `sing-box check` 通过；订阅 JSON 的 urltest 字段断言；resi-health 用 stub 跑一轮，日志/状态符合新默认；timer 文本含 `RandomizedDelaySec`。


### R6 selector + Clash API 热切换（替代"改配置 + 重启 + 冷却"）

来源：住宅最佳实践调研 P0-3。sing-box 重启/reload 会重置实例上所有连接（官方 issue #3731 closed as not planned）；urltest 按延迟择优 + 50ms 容忍会在会话内换出口 IP，对 AI 登录账号是高危动作。行业做法是"按健康度粘住"。

- 中继配置（`write_singbox_config_residential_multi`）：
  - `resi-pool` 由 `urltest` 改为 `{"type":"selector","tag":"resi-pool","outbounds":[resi-1..N],"default":"resi-1","interrupt_exist_connections":false}`。
  - 新增 `"experimental": {"clash_api": {"external_controller": "127.0.0.1:9091"}, "cache_file": {"enabled": true, "path": "<BASE_DIR>/relay-cache.db"}}`（selector 的选择默认写入 cache_file、跨重启保留——注意 `store_selected` 不是合法字段，三版 sing-box `check` 会 FATAL；仅回环监听，不设 secret；`SINGBOX_RELAY_API="127.0.0.1:9091"` 作为脚本常量）。
  - 直连模式写函数不加 selector/clash_api（无可选）。
- `resi-health.sh` 决策段重写：
  - 探测循环与迟滞状态（`active/failstreak/okstreak`）保持。
  - 不再改写 `singbox-relay.json`、不再 `systemctl restart`、删除重启冷却逻辑（`_last_restart` 不再写）。
  - 取当前选择：`GET http://127.0.0.1:9091/proxies/resi-pool` 的 `.now`；取不到（旧配置无 API / relay 未起）→ 记一行日志并 `exit 0`（`update.sh` 每次升级都 `reapply`，配置会自动换成 selector）。
  - 决策：健康集 = 迟滞后 `active=true` 的成员。当前选择在健康集内 → 不动（粘住）。否则切到健康集中按 `alltags` 顺序的第一个：`PUT /proxies/resi-pool` body `{"name":"resi-N"}`。健康集为空 → 不切、WARN。
  - 切换限速：状态文件 `_last_switch`（epoch），两次切换间隔 ≥ `RESI_HEALTH_SWITCH_MIN_INTERVAL`（默认 60s）。
  - 探测期间成员集变化的复核保留（比较 relay 的 socks tag 集），不再需要 `flock`（不写 relay 文件）；`acquire_relay_lock` 仅 helper 使用。
  - 单成员（<2）仍直接退出。
- `residential-helper.sh`：`enable --add/--remove` 等改成员集的操作仍需重写配置并 `restart`（出站列表变化无法热加载）；这是管理员显式操作，可接受。
- 已知取舍：selector 不再自动按延迟择优，只在当前线路探测失败时切换；这是刻意的。

### R7 体检端点以中继为真源 + 暴露巡检状态

来源：调研 P0-5。现状 `/api/residential/health` 只拨 `residential-proxy.json` 顶层 `host`（= 最后一次 `--add` 的那条，可能已不在池里）。

- 读 `singbox-relay.json` 的 socks 出站（tag/server/server_port/username/password）作为成员列表；读 `.resi-health-state.json` 取每成员 `active/failstreak/okstreak`；`GET 127.0.0.1:9091/proxies/resi-pool` 取 `now`（1s 超时，失败为 null）。
- 对每个成员（最多 8 个）并行做出口探测（复用 R3 的 `-K -` 凭据方式，改用 R9 的 HTTPS 源），单个 8s 超时。
- 响应：
  ```json
  { "enabled": true, "domains_count": 31, "mode": "selector|urltest|none", "selected": "resi-2",
    "members": [ { "tag": "resi-1", "host": "…", "port": 1080, "active": true, "failstreak": 0, "okstreak": 5,
                   "egress": { "ip": "…", "type": "家庭宽带 IP", "isp": "…", "country": "…" } } ],
    "current_egress_ip_test": "…", "egress_ip_type": "…", "via_proxy_isp": "…", "urls": [ … ] }
  ```
  顶层旧字段由"当前选择的成员"（无则第一个）派生，保持面板旧渲染可用。密码不出现在响应里。
- `web/app.js` `loadResiHealth()`：状态行用当前选择成员的出口类型定色，下方列出成员表（tag、host:port、健康/剔除、出口 IP、类型）；旧的 ISP/IP 类型/分流关键词行保留。

### R8 供应商粘性参数引导 + 文档

来源：调研 P0-4。各厂商默认"出口不可用就静默换 IP"，只有在**用户名参数**里加锁定/失效参数才会显式报错，我们的健康探测语义才成立；参数不含冒号，`parse_url` 无需改。

- 新文档 `docs/residential-proxy-guide.md`：供应商用户名参数表（Bright Data `-session-<id>` + `-const`，SOCKS5 端口 22228 / HTTP 44445，住宅 SOCKS5 只开放 8080/8443 等目标端口且只允许 HTTPS 目标；Oxylabs `sessid-<id>` + `sesstime-<min>` + `sessid_oneip`；Decodo `session-<id>-sessionduration-<min>`；IPRoyal `_session-<8位>_lifetime-<t>_killswitch-1`；SOAX `sessionid-<id>-sessionlength-<s>`）、为什么 AI 登录必须粘住、"不限量"实为每 IP 配额、转售条款风险、UDP/QUIC 处理（R4）、探测频率（R5）、体检读数含义（R7）。
- 面板：住宅弹窗 URL 输入区加一行提示（"建议在用户名里加供应商的粘性/失效参数，见文档"）并链接到该文档；`residential-helper.sh verify()` 失败文案补一句"若供应商限制目标端口（如 Bright Data 住宅仅开放 8080/8443 等）请改用其 HTTP 代理端口"。

### R9 体检数据源改 HTTPS

来源：调研 P0-6。`http://ip-api.com` 免费端点无 HTTPS、禁商用、45 次/分；明文 HTTP 经住宅腿可被篡改。实施时发现 `api.ipapi.is` 免费层自 2026-09-01 起不再返回 `is_datacenter` 等分类字段，弃用。

- 主源：`https://my.ippure.com/v1/info`（公开、无 key、仅 IPv4——住宅 SOCKS5 本就是 IPv4；字段 `ip / asOrganization / country / region / city / fraudScore / isResidential`）。类型映射：`isResidential:true` → 家庭宽带 IP；false → IDC机房 IP；ISP 标签附 `风险分 N`。与客户端「连接测试」同源，判定口径一致。
- 备源：`https://api.ipquery.io/`（免费、无 key；`risk.is_datacenter/is_vpn/is_proxy/is_mobile`）：`is_datacenter` → IDC机房 IP；`is_vpn||is_proxy` → 代理 IP；`is_mobile` → 移动网络 IP；否则 家庭宽带 IP。
- 兜底：`https://ipinfo.io/json`（`ip/org/country/city`），类型记 unknown、ISP 取 `org`。
- 三者都经成员的 SOCKS5（socks5h，`-K -` 凭据）拨出，按顺序失败即降级。
## 3. 文件改动清单

| 文件 | 改动 |
|---|---|
| `server/residential-helper.sh` | `domains` 子命令；写路径 flock；`save_config` 与其它直写点改 tmp+mv；`verify()` 改 `-K -`；`cfg_escape()` |
| `server/resi-health.sh` | 写段 flock + 持锁后重读；`_last_restart` 冷却；探测 curl 改 `-K -` |
| `web/server.js` | `getEffectiveResidentialDomains()`；`getResidentialConfig()` 回退；删内联 `DEFAULT_DOMAINS`；`residential/health` 的 curl 改 stdin 配置 |
| `version.json` | changelog 3.6.0 里三条（随 IPv6 提交一起 bump） |

## 3b. R4/R5 文件改动

| 文件 | 改动 |
|---|---|
| `server/residential-helper.sh` | 两个写函数：route.rules 插入三条 UDP 规则；urltest `interval 3m` / `tolerance 500` / `idle_timeout 30m` |
| `server/resi-health.sh` | `TRIES` 默认 2、`OK_NEED` 默认 1 |
| `server/update.sh` | D8 timer 文本加 `RandomizedDelaySec=30s`；既有 timer 缺该行则补（幂等） |
| `web/server.js` | `generateSingboxConfig` `urltest()`：`interval "60s"`、`interrupt_exist_connections false`、`tolerance 100` |

## 3c. R6–R9 文件改动

| 文件 | 改动 |
|---|---|
| `server/residential-helper.sh` | multi 写函数：selector + experimental(clash_api/cache_file)；`verify()` 失败文案 |
| `server/resi-health.sh` | 决策段：Clash API GET/PUT、粘住、切换限速；删除重写/重启/冷却 |
| `web/server.js` | `/api/residential/health` 重写为成员列表 + 状态 + 选择 + HTTPS 探测（主/备源） |
| `web/app.js`、`web/index.html` | 健康卡成员表；住宅弹窗提示与文档链接 |
| `docs/residential-proxy-guide.md` | 新增 |

## 4. 提交拆分

1. `fix(residential): 订阅域名回退与服务端中继一致`（R1）
2. `fix(residential): singbox-relay.json 加锁 + 原子写 + 巡检重启冷却`（R2）
3. `fix(residential): SOCKS5 凭据不再出现在 curl 命令行`（R3）
4. `fix(residential): 中继显式处理 UDP(53 直连/443 拒绝/其余直连)`（R4）
5. `fix(residential): 三层探测降频(中继 3m/500ms, 订阅 60s 不打断连接, 巡检 2 次+抖动)`（R5）
6. `feat(residential): 中继改 selector + Clash API 热切换,巡检按健康度粘住不再重启`（R6）
7. `feat(residential): 体检端点以中继成员为真源并暴露巡检状态,数据源改 HTTPS`（R7+R9）
8. `docs(residential): 供应商粘性参数指南 + 面板提示`（R8）

每个提交独立可回滚，均不 bump 版本；版本随 IPv6 提交之后的 `bump: v3.6.0` 一起。

## 5. 验收标准

1. `bash -n server/residential-helper.sh server/resi-health.sh`；`node --check web/server.js`；shellcheck 无新增 error。
2. R1：临时目录下 `residential-proxy.json` 分别为 `domains: null`、`domains: []`、`domains: ["foo"]` 三种，`residential-helper.sh domains` 输出分别为默认 31 条、默认 31 条、`["foo"]`；本地起 server.js，`/api/subscription/<user>` 与 `/api/clash/<user>` 在 null 情形下出现默认关键字规则。
3. R2：
   - 两个并发 `residential-helper.sh set-domains` 调用互斥（第二个等待或超时报错，最终文件是两者之一的完整内容，不为空/截断）。
   - `resi-health.sh` 用 `RESI_HEALTH_DRY_RUN=1` 与伪造 relay/state 文件验证冷却分支日志；冷却过期后走重启分支（`systemctl` 用 PATH 前置的 stub 替代）。
   - `save_config` 执行后目录里无残留 `.tmp`，文件 600。
4. R3：用 `strace -f -e execve` 或 `ps` 抓 curl 进程参数，确认不含密码；三处功能仍通（本地 socks5 stub 或 `curl --trace-ascii` 看 `proxy-user` 生效）。
5. 线上：用户升级后 `journalctl -u b-ui-resi-health` 与 `/var/log/b-ui-resi-health.log` 看到冷却日志或正常切换日志，管理面板住宅健康卡正常。
