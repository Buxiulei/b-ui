# 住宅代理自动黑名单（v3.7.0，R13）

- 日期：2026-09-11
- 状态：已批准（主理人三项决策：候选 = 内置表 + relay 日志学习；面板 = 展示 + 手动钉住；重启 = 添加/切换时立即应用、每日刷新只在有变化时重启）
- 文件：新增 `server/resi-blacklist.sh`；改 `server/residential-helper.sh`、`server/resi-health.sh`、`server/update.sh`、`web/server.js`、`web/app.js`、`web/index.html`、`web/style.css`、`version.json`、`install.sh`、`docs/residential-proxy-guide.md`

## 1. 背景与目标

现状：relay（`b-ui-relay`，sing-box）只有两种路由模式。`global=true` 时 `route.final=resi-pool`，全部流量走住宅上游；`global=false` 时只有 `DEFAULT_DOMAINS` 关键字命中的走住宅，其余直出。两台生产服务器都是 global。

问题：住宅供应商按策略拒绝一部分目标（Bright Data：搜索 policy_20110、支付处理商和 TikTok policy_20050、非白名单端口），global 模式下这些连接直接失败。2026-09-11 实测 bwg-tizi relay 日志每 30 分钟约 256 条 `unexpected status: 403 Forbidden`，目标是 `<ip>:5228`（FCM 推送）、`<ip>:5223`（APNs）、`gateway.icloud.com`、`query.ess.apple.com`、`courier.push.apple.com`、`www.google.com`；从 baiyi 客户端经住宅节点测 `www.google.com` 也被拒。反过来用关键字白名单又太脆：AI 相关域名漏一个就出问题。

目标：**默认全部走住宅，只把「这个上游代理不了的目标」送去机房直出**。黑名单按上游分别维护，添加上游时立刻探测一遍，健康切换上游时跟着换成对应黑名单，每天后台刷新一次，面板可看、可手动重测、可钉住覆盖。

## 2. 决策摘要

| 议题 | 决定 |
|---|---|
| 候选来源 | 内置候选表（按类别列具体主机名）+ 从 relay 日志学习被拒目标 |
| 初始黑名单 | **预置为空**：不存在任何写死的黑名单条目，内置表只是「值得探测的候选」；**录入上游时立即探测一次**，探测结果就是该上游的初始黑名单；录入时探测失败则保持为空，由每日刷新或手动重测补上 |
| 面板 | 按上游展示条数与明细、「立即重新检测」、钉住（强制机房 / 强制住宅） |
| 重启 | 添加上游、健康切换、手动操作：有变化立即重写并重启；每日刷新北京时间 04:00 起 30 分钟内随机，只有黑名单真变化才重启 |
| 实现路线 | 全部在服务端 shell：`resi-blacklist.sh` 探测/学习/状态，`residential-helper.sh` 仍是 relay 配置唯一写入者，`resi-health.sh` 切换后调 helper 应用，面板经 server.js 调脚本；不做免重启的多 selector 编码（同一供应商多个上游黑名单几乎相同，切换时通常无变化不重启，复杂度不值） |

## 3. 路由语义

黑名单优先级最高，两种模式都生效。`write_singbox_config_residential_multi` 生成的 `route.rules` 顺序变为：

```
sniff → udp:53 direct → udp:443 reject → udp direct → 私网/本机 IP direct
→ [钉住:强制住宅]  {"domain_suffix": [...], "outbound": "resi-pool"}
→ [黑名单+钉住:强制机房] {"domain_suffix": [...], "outbound": "direct"}
→ [黑名单端口]      {"port": [5228, 5223, ...], "outbound": "direct"}
→ （关键字模式）     {"domain_keyword": $kw, "outbound": "resi-pool"}
final: global ? resi-pool : direct
```

- 域名条目用 `domain_suffix`，条目就是探测过的那个主机名本身：`www.google.com` 只影响它和它的子域，不自动扩到 `google.com`；`accounts.google.com`、`gemini.google.com` 继续走住宅。自动逻辑永远不写 apex（如 `google.com`）。
- 端口条目 `port:N` 处理日志里的裸 IP 目标（推送、非白名单端口），按目标端口直出。80/443/8080/8443 永远不会成为端口条目。
- DNS 段不改：relay 热路径把域名原样交上游，走 `direct` 时按 `route.default_domain_resolver=dns_direct` 解析（v3.6.0 R10 结论）。
- 三个订阅生成器不改：客户端照旧把全部流量发给住宅节点，由服务端 relay 决定谁直出。
- 空黑名单、空钉住时不生成对应规则，配置与 v3.6.x 逐字节一致（升级零变化不重启）。

## 4. 状态文件 `/opt/b-ui/residential-blacklist.json`（chmod 600，`.tmp` + `mv` 原子写）

```json
{
  "version": 1,
  "upstreams": {
    "brd.superproxy.io:44445": {
      "checkedAt": "2026-09-11T20:05:12Z",
      "entries": {
        "www.google.com": {"kind": "domain", "source": "builtin:search", "reason": "403 Forbidden serp domain",
                            "fails": 2, "okStreak": 0, "since": "2026-09-11T20:05:12Z", "lastCheck": "..."},
        "port:5228":      {"kind": "port", "source": "learned", "reason": "403 Forbidden", "fails": 2, "okStreak": 0, "since": "...", "lastCheck": "..."}
      }
    }
  },
  "learned": {
    "brd.superproxy.io:44445": {
      "gateway.icloud.com": {"count": 68, "firstSeen": "...", "lastSeen": "..."},
      "port:13861":         {"count": 5,  "firstSeen": "...", "lastSeen": "...", "sampleIp": "1.2.3.4"}
    }
  },
  "pins": {"www.google.com": "direct", "example.com": "resi"},
  "checking": {"upstream": "brd.superproxy.io:44445", "startedAt": "..."},
  "applied": {"upstream": "brd.superproxy.io:44445", "digest": "sha256:…", "at": "..."}
}
```

- 上游键是 `host:port`，与 `enable --remove` 的定位方式一致（`urls[].name` 会重编号，不能当键）。同一 host:port 不同账号视为同一上游。
- `entries` 只放当前判定为「拒绝」的条目；`learned` 是候选池；`pins` 全局，不分上游。
- `checking` 表示后台探测进行中，超过 10 分钟视为过期。`applied` 记录最近一次写进 relay 的有效黑名单摘要。
- 上限：每上游 `entries` 500 条、`learned` 200 条、端口条目 20 条，超出按 `lastSeen` 淘汰最旧。

## 5. 探测与判定

每个候选目标做「上游探测 + 直连对照」，两次通过上游（间隔 3 秒），并发 8，单次 `--max-time 12`。凭据经 `curl -K -` 从 stdin 传入（沿用 `curl_proxy_cfg`），不上命令行。

### 5.1 域名目标（`https://<host>/`，`-sS -o /dev/null -I -v`）

| 上游探测结果 | 判定 |
|---|---|
| curl 退出 56 且 `-v` 里 CONNECT 响应为 4xx/5xx（HTTP 上游）；退出 97（SOCKS5 拒绝） | 代理拒绝 |
| 退出 35（CONNECT 成功后 TLS 被掐）且直连 TLS 正常 | 代理拒绝 |
| 退出 0，站点返回任意 HTTP 状态（含 401/403，如 Cloudflare 对 curl 的挑战） | 能通 |
| 退出 28（超时） | 未知，不改状态 |
| 退出 7（连不上代理本身）；CONNECT 响应 407（代理鉴权失败） | 上游不可用，本轮整体跳过 |

拒绝原因：从 `-v` 输出取 CONNECT 响应状态行（如 `HTTP/1.1 403 Forbidden serp domain`）及 `x-brd-*` / `x-luminati-*` 头，存入 `reason`。

### 5.2 端口目标（`host:port`，port ≠ 443）

上游：`curl -x <proxy> https://host:port/` — 退出 56/97 为代理拒绝；退出 0/35/52 说明 CONNECT 已建立，算能通。直连对照：`timeout 8 bash -c 'exec 3<>/dev/tcp/host/port'`。

### 5.3 进出规则

- **进黑名单**：两次上游探测都是「代理拒绝」**且**直连对照成功。直连也不通的目标一律不碰（不是代理的错）。
- **出黑名单**：`entries` 里的条目每日复测，连续两次每日刷新都「能通」才移除（`okStreak ≥ 2`），避免供应商偶发抖动导致来回翻。
- 「未知」不改变现状；上游整体不可用时本轮不改任何条目、不应用。
- 钉住优先于自动判定：`pins.resi` 的目标即使被判拒绝也不进 direct 规则；`pins.direct` 的目标无需探测。

## 6. 候选来源

### 6.1 内置候选表（`BLACKLIST_CANDIDATES`，`resi-blacklist.sh` 内，具体主机名而非关键字）

```
# 搜索（Bright Data policy_20110）
www.google.com www.bing.com duckduckgo.com search.yahoo.com www.baidu.com yandex.com
# 短视频 / 社交（policy_20050 等）
www.tiktok.com api.tiktokv.com www.instagram.com www.facebook.com x.com api.x.com www.reddit.com
# 支付处理商（policy_20050）
checkout.stripe.com js.stripe.com api.stripe.com m.stripe.com www.paypal.com api.paypal.com
# Apple 服务（2026-09-11 bwg-tizi 日志实测被拒）
gateway.icloud.com query.ess.apple.com courier.push.apple.com gdmf.apple.com mesu.apple.com xp.apple.com
# 推送端口（用真实推送主机探测，命中记为端口条目）
mtalk.google.com:5228 => port:5228
courier.push.apple.com:5223 => port:5223
```

政府、银行、流媒体等不内置，靠日志学习。表只是「值得一探」的候选，进不进黑名单完全由探测结果决定；**新上游的黑名单从空开始**，没有任何预置条目，录入时那一次探测的结果就是它的初始内容。

### 6.2 从 relay 日志学习

每日刷新开始时扫描 `journalctl -u b-ui-relay --since -25h -o cat`（先 `sed` 去掉 ANSI 颜色码），匹配：

```
open connection to <target>:<port> using outbound/(http|socks)\[(resi-[0-9]+)\]: unexpected status: (4[0-9]{2}|5[0-9]{2}) [^\n]*
```

以及 SOCKS5 上游的拒绝形态（`SOCKS5 … refused / not allowed`）。`resi-N` 经当前 `singbox-relay.json` 的池顺序映射回 `host:port`。

- 域名目标：同一（上游，主机名）24 小时内 ≥ 3 次 → 进 `learned`，本轮一起探测。
- 裸 IP 目标：按端口聚合，≥ 3 次且来自 ≥ 2 个不同 IP → `port:N` 候选，用最近一个 IP 做端口探测；80/443/8080/8443 不聚合。
- `learned` 条目 7 天没再出现且未进黑名单则清理。

## 7. 触发时机与应用

| 时机 | 动作 | 重启 |
|---|---|---|
| `enable <url>` / `enable --add` 校验通过后（录入即探测） | 面板立即返回并显示「黑名单检测中」；helper 用 `systemd-run --unit=b-ui-resi-blacklist-probe-<ts>` 异步启动 `resi-blacklist.sh probe <host:port>`（内置候选表 + 该上游已有的 learned），跑完调 `residential-helper.sh blacklist-apply`；结果即该上游的初始黑名单，面板轮询到 `checking` 消失后直接展示 | 有条目即重写重启 |
| `enable --remove` | 删除该上游的 `entries` / `learned`，`blacklist-apply` | 有变化才重启 |
| `resi-health.sh` 切换成功（PUT selector 后） | 调 `residential-helper.sh blacklist-apply` | 新上游有效黑名单与现生效不同才重启（同供应商通常无变化） |
| 每日刷新 `b-ui-resi-blacklist.timer` | `resi-blacklist.sh refresh` = learn + 全部上游 probe + apply | 有变化才重启 |
| 面板「立即重新检测」 | `POST /api/residential/blacklist/probe` → 异步 probe（单个或全部上游）| 有变化才重启 |
| 钉住 / 取消钉住 | `resi-blacklist.sh pin …` 写 `pins` → `blacklist-apply` | 立即 |

`blacklist-apply`（在 `residential-helper.sh` 内，持 `.relay.lock`）：读取当前选中上游（`GET 127.0.0.1:9091/proxies/resi-pool` 的 `.now` 映射到 host:port，API 不通则取池首）→ 用 `write_singbox_config_from_state` 生成到 `.tmp` → 与现配置 `file_digest` 相同则丢弃 `.tmp` 不重启 → 不同则先 `sing-box check -c .tmp`，失败保留旧配置并记日志 → 通过则 `mv` + `systemctl restart b-ui-relay`，更新 `applied`。selector 的 `default` 保持池首不变（`cache_file` 已跨重启保留当前选中项），这样空黑名单时 `reapply` 生成的配置与 v3.6.x 逐字节一致。

定时器：`b-ui-resi-blacklist.timer`，`OnCalendar=*-*-* 04:00:00 Asia/Shanghai`、`RandomizedDelaySec=30min`、`AccuracySec=5min`、`Persistent=true`；service `ExecStart=/opt/b-ui/resi-blacklist.sh refresh`、`Nice=10`、`TimeoutStartSec=15min`。生产机时区为 UTC，日历项带时区后按北京时间执行。由 `residential-helper.sh ensure_blacklist_timer` 在 `enable` / `reapply` 里创建、在池清空或 `disable` 时移除（与 D8 对 `resi-health.timer` 的处理方式一致）。

## 8. 组件与接口

### 8.1 `server/resi-blacklist.sh`（新，`set -uo pipefail`，`.blacklist.lock` 保护状态文件；调 helper 前先释放锁，避免与 `.relay.lock` 互等）

```
resi-blacklist.sh probe [<host:port>|all]    探测内置表 + learned，更新 entries，然后 helper blacklist-apply
resi-blacklist.sh learn                      扫描日志更新 learned（refresh 内部调用，也可单独跑）
resi-blacklist.sh refresh                    learn → probe all → apply（timer 目标）
resi-blacklist.sh status [--json]            打印状态（面板不直接调它，server.js 直接读 JSON）
resi-blacklist.sh pin <target> direct|resi|clear
resi-blacklist.sh forget <host:port>         删除一个上游的数据（helper --remove 时调用）
```

日志 `/var/log/b-ui-resi-blacklist.log`，退出时 `tail -300` 轮转。

### 8.2 `server/residential-helper.sh`

- `write_singbox_config_residential_multi`：新增 `--argjson bl_resi/bl_direct/bl_ports`，按第 3 节顺序插入规则；三者为空时不生成规则，其余部分不动。
- 新子命令 `blacklist-apply`（第 7 节）；`enable` / `--add` 末尾异步触发 probe；`--remove` 调 `forget`；`disable` 移除 timer。
- `ensure_blacklist_timer`；`status` 输出附 `blacklist` 摘要（当前选中上游条数、`checkedAt`、`checking`）。

### 8.3 `server/resi-health.sh`

切换成功那一行之后加 `"${BASE_DIR}/residential-helper.sh" blacklist-apply >>"$LOG" 2>&1 || true`。这是巡检脚本唯一的写路径，且只在黑名单确有差异时重启，其余仍保持「只读、不重启」。

### 8.4 `web/server.js`（均在鉴权之后）

| 端点 | 行为 |
|---|---|
| `GET /api/residential/blacklist` | 直接读状态文件，返回 `{upstreams: {"host:port": {checkedAt, checking, entries: [{target, kind, source, reason, since}]}}, pins: [{target, mode}], learned: [{upstream, target, count, lastSeen}], applied}`；过期 `checking` 视为 false |
| `POST /api/residential/blacklist/probe` `{upstream?}` | `systemd-run` 异步 `resi-blacklist.sh probe <x|all>`，返回 202 `{started: true}`；已有 `checking` 时返回 409 |
| `POST /api/residential/blacklist/pins` `{target, mode}` | `resi-blacklist.sh pin` |
| `DELETE /api/residential/blacklist/pins/<target>` | `resi-blacklist.sh pin <target> clear` |
| `GET /api/residential` | 增加 `blacklist: {count, checkedAt, checking}`（当前选中上游） |
| `GET /api/residential/health` | `members[]` 增加 `blacklist_count` |

`target` 校验：域名 `^[a-z0-9.-]{1,253}$` 或 `^port:[0-9]{1,5}$`，其余 400。

### 8.5 面板（`web/app.js` / `web/index.html` / `web/style.css`）

住宅弹窗「代理节点池」下方新增 `<details id="resi-blacklist-details">`「机房直出黑名单（自动检测）」：summary 徽标 `N 条 · 最近检测 HH:MM` 或 `检测中…`；展开后按上游分组的表（目标 / 来源 / 拒绝原因 / 加入时间）、候选池折叠列表、按钮「立即重新检测」；钉住区：输入框 + 「强制机房 / 强制住宅」下拉 + 添加，下方已钉列表可移除。探测中每 5 秒轮询一次 `GET /api/residential/blacklist` 直到 `checking` 消失。首页「住宅 IP 健康」卡片线路表增加「黑名单」列。文案沿用现有中文风格。

## 9. 升级迁移

`server/update.sh` `apply_systemd_configs()` 新增块 **D10**（幂等）：
1. `/opt/b-ui/resi-blacklist.sh` 不存在或为空 → 从当前下载源自愈下载（同 D8 对 `resi-health.sh` 的做法），`chmod +x`。
2. `residential-proxy.json` 为 `enabled=true` 且 `urls` 非空、且 `residential-blacklist.json` 不存在 → 写入空状态文件，`residential-helper.sh reapply`（安装 timer；配置无变化不重启），再 `systemd-run` 一次 `resi-blacklist.sh probe all` 做初始探测，`updated=1`。
3. 池为空 → 确保 timer 不存在。

新文件登记：`version.json files[]`、`install.sh` 文件表、`update.sh` 交互与 `auto` 两份文件表（`file_map`）。`install.sh` 新装时由 `residential-helper.sh setup` 路径同样走 `ensure_blacklist_timer`（池空则不装）。

## 10. 错误处理与安全

- 探测网络异常（上游整体不可用、直连对照全部失败、journal 不可读）→ 本轮跳过，保留旧黑名单，记日志；绝不因为一轮失败清空黑名单。
- `sing-box check` 不过 → 保留旧配置、不重启、状态文件 `applied` 不更新、日志报错；面板在 `applied.at` 早于 `checkedAt` 时提示「上次应用失败」。
- 状态文件 600：`learned` 里是用户真实访问的目标，只在鉴权后的面板可见。
- 凭据只经 stdin（`curl -K -`），探测和日志里不出现密码；systemd-run 的 unit 名只含时间戳。
- 两把锁：`.blacklist.lock` 只保护状态文件，`.relay.lock` 只保护 relay 配置；`resi-blacklist.sh` 调 helper 前必须已释放前者。
- 并发探测限制 8，单轮上限约 30 内置 + 200 候选，最长约 10 分钟；`TimeoutStartSec=15min` 兜底。

## 11. 测试与验收

- 单元（本机）：抽出判定函数，用 stub `curl` 按退出码/`-v` 输出跑表 5.1/5.2 的每一行；抽出日志解析函数，用 bwg-tizi 的真实日志样本（含 ANSI）验证域名/端口聚合与阈值；空黑名单时生成的 relay 配置与 v3.6.3 逐字节一致。
- 配置：有黑名单/钉住/端口条目时的 `singbox-relay.json` 用 sing-box 1.13.x 和 1.14.x `check` 校验。
- 集成（bwg-tizi，经 `ssh -J bwg-rick bwg-tizi`）：`probe all` 后预期 `www.google.com` 进黑名单、`gemini.google.com` 不进、`port:5228` / `port:5223` 与 Apple 推送主机被学到；从 baiyi 经住宅节点访问 `www.google.com` 变为可用（出口为机房 IP），`ippure.com` 仍显示住宅 IP；`blacklist-apply` 在无变化时不重启（`ActiveEnterTimestamp` 不变）；钉住 `www.google.com` 为强制住宅后再次被拒且不进 direct 规则。
- 升级：3.6.3 → 3.7.0 后 D10 只跑一次，第二次 `update.sh` 不再变更；timer 按北京时间触发（`systemctl list-timers`）。

## 12. 非目标

- 不改客户端订阅与 bui-c；不做免重启的 selector 编码；不内置政府/银行/流媒体表；不按账号区分同一 host:port 的上游。
