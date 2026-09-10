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

## 4. 提交拆分

1. `fix(residential): 订阅域名回退与服务端中继一致`（R1）
2. `fix(residential): singbox-relay.json 加锁 + 原子写 + 巡检重启冷却`（R2）
3. `fix(residential): SOCKS5 凭据不再出现在 curl 命令行`（R3）
4. `fix(residential): 中继显式处理 UDP(53 直连/443 拒绝/其余直连)`（R4）
5. `fix(residential): 三层探测降频(中继 3m/500ms, 订阅 60s 不打断连接, 巡检 2 次+抖动)`（R5）

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
