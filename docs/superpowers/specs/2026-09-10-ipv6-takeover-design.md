# IPv6 接管设计（v3.6.0）

- 日期：2026-09-10
- 目标版本：v3.6.0
- 状态：已批准，待实施
- 关联：`docs/superpowers/specs/2026-09-10-residential-hardening-design.md`（同版本独立提交）

## 1. 背景与目标

### 问题

用户在 v2rayN（Windows）TUN 模式下观察到 IPv6 泄漏：本机真实 IPv6 地址暴露在 test-ipv6.com 一类测试站。服务器（VPS）没有 IPv6 出口，只能用 IPv4 访问外网。

调研（2026-09-10，见 §7 事实依据）确认的现状：

| 位置 | 现状 |
|---|---|
| v2rayN 默认 sing-box TUN 模板 | 带 IPv6 地址、`strict_route: false`、DNS `strategy: prefer_ipv4`（软偏好）。旧版或未勾"启用 IPv6"时 IPv6 直接走物理网卡。 |
| `web/server.js` `generateSingboxConfig()`（`/api/subscription/<user>`） | 使用 `inet4_address`、字符串式 DNS server、inbound 级 `sniff`、`geoip/geosite`、`block/dns` 特殊出站。这些字段在 sing-box 1.12 到 1.14 之间已全部移除，`sing-box check` 直接 FATAL。Linux 客户端"从服务端导入订阅 → sing-box"把这份 JSON 原样落盘为 `singbox-tun.json`（`b-ui-client.sh:3076`），所以该路径当前完全不可用。 |
| `b-ui-client.sh` `generate_singbox_tun_config()` | 语法现代，但 TUN 只有 IPv4 地址 + `strict_route: true`。sing-tun 在这种组合下装一条全局 `ip -6 rule unreachable`，IPv6 在客户端整体黑洞（不泄漏、也不可用）。 |
| 服务端 Xray | `dns.queryStrategy: UseIPv4`，但 freedom 出站无 `domainStrategy`（AsIs 走 Go 系统解析器，与 queryStrategy 无关）。 |
| 服务端 Hysteria2 | 直连实例无 `outbounds` 块 → 内置 direct `mode: auto`（双栈）；住宅实例 direct 出站同样未限 IPv4。resolver 总是同时查 A 和 AAAA，mode 只决定拨哪个族。 |
| 服务端中继 sing-box（`residential-helper.sh`） | `dns.strategy: prefer_ipv4`；防回环 CIDR 仅 IPv4。 |

### 目标

1. TUN 开启时，客户端所有 IPv6 流量进入隧道，不再从物理网卡泄漏。
2. 客户端 DNS 只返回 A 记录；极少数裸 IPv6 目标（应用自带 DoH、硬编码地址）在客户端就地 reject，让应用的 Happy Eyeballs 立即回退 IPv4。
3. 服务端所有出站显式 IPv4-only。
4. `/api/subscription/<user>` 的 sing-box 配置在 sing-box 1.12 到 1.14 全部可用，且内置上述 IPv6 策略；v2rayN 可把它当自定义配置导入。
5. 给 v2rayN 用户一份设置文档。

### 非目标

- 不做 NAT64/DNS64，不让服务器代理 IPv6 出站。IPv6-only 站点在隧道开启时不可达，是服务器无 v6 出口的固有限制，文档写明。
- 不引入 fake-ip。客户端 DNS 已受控只返回 A 记录，fake-ip 只是多一套缓存与副作用（`ping` 显示假 IP、应用缓存过期等）。
- 不改 `/api/sub/` 明文 URI 与 `/api/clash/` YAML 的节点集合逻辑。
- 不改 Linux 客户端 SOCKS/HTTP 本地代理模式（VLESS/Hysteria2 协议原样携带域名，服务端解析，已满足 A-only）。
- 不合并 `web/server.js` 里三份重复的节点集合构造逻辑（另立任务）。

## 2. 关键决策

| 决策 | 选择 | 理由 |
|---|---|---|
| 裸 IPv6 目标处理 | 路由规则 `{"ip_version": 6, "action": "reject"}` | sing-box 1.13 起嗅探域名不再替换连接目标（`sniff` action 无 `override_destination`，源码 `route/route.go` 的 `OverrideDestination` 无任何配置路径可置位），服务器收到的必然是 IPv6 字面量而无法处理；reject 默认方法对 TCP 回 RST、UDP 回 ICMP 端口不可达，应用能立刻回退 IPv4。 |
| 客户端 DNS 策略 | 保持/设为 `ipv4_only` | sing-box 对 AAAA 返回 NOERROR 空答案（`dns/client.go` 本地短路），应用拿不到 v6 地址。`prefer_ipv4` 在只有 AAAA 时仍会返回 v6。 |
| TUN 地址 | `["172.19.0.1/30", "fdfe:dcba:9876::1/126"]` | sing-box 官方手册示例；有 v6 地址后 `auto_route` 才会把 `::/0` 装进 TUN 路由表。 |
| 客户端主机 IPv6 被禁用时 | 仅 IPv4 地址（保持现状） | 无 IPv6 则无泄漏；避免给 TUN 加 v6 地址在 `disable_ipv6=1` 主机上失败。检测规则见 §3.2。 |
| Xray freedom | `settings.domainStrategy: "ForceIPv4"` | `UseIPv4` 在解析不到 A 时静默回退 AsIs；`ForceIPv4` 直接失败。Xray 26.9.9 起该字段自动迁移到 `sockopt.domainStrategy` 并记警告，新旧版本都能用。 |
| Hysteria2 | 显式 `outbounds: - name: direct, type: direct, direct: {mode: 4}` | 官方文档：`mode: 4` = 只拨 IPv4，无 IPv4 地址则失败。用户自定义 `direct` 出站会覆盖内置 direct。 |
| `geoip/geosite` 替代 | `route.rule_set` remote（SagerNet 官方 `.srs`），`download_detour` 指向主代理池 | 旧字段 1.12 已移除；rule_set 是官方替代且保持"cn 直连"语义不变。 |
| `stack` | 不指定 | sing-box 默认（有 gVisor 时 `mixed`，否则 `system`）跨平台最稳；v2rayN wiki 推荐 mixed。 |
| 版本号 | 3.6.0 | 行为变更（IPv6 接管 + 服务端出站策略），非 patch。 |

## 3. 组件设计

### 3.1 `web/server.js` — 重写 `generateSingboxConfig(user, cfg, host)`

节点集合、`mkHy2`/`mkVless`、urltest 分组、`hasSplit`/`resi.global`/`resi.domains` 逻辑**保持不变**。只改输出骨架：

```jsonc
{
  "log": { "level": "info", "timestamp": true },
  "experimental": {
    "clash_api": { "external_controller": "127.0.0.1:9090", "external_ui": "", "secret": "", "default_mode": "rule" },
    "cache_file": { "enabled": true, "path": "cache.db" }
  },
  "dns": {
    "servers": [
      { "tag": "remote", "type": "https", "server": "8.8.8.8", "detour": "<primaryTag>" },
      { "tag": "local",  "type": "udp",   "server": "223.5.5.5" }
    ],
    "rules": [
      // 仅当 host 是域名且 getServerIP() 返回公网 IPv4（非 127.0.0.1 兜底）时输出，防 GFW 投毒服务器域名
      { "domain": ["<host>"], "action": "predefined", "answer": ["<host>. IN A <serverIp>"] },
      { "rule_set": ["geosite-cn"], "server": "local" },
      { "domain_suffix": [".cn"], "server": "local" }
    ],
    "final": "remote",
    "strategy": "ipv4_only"
  },
  "inbounds": [
    { "type": "mixed", "tag": "mixed-in", "listen": "127.0.0.1", "listen_port": 7890 },
    { "type": "tun", "tag": "tun-in", "interface_name": "bui-tun",
      "address": ["172.19.0.1/30", "fdfe:dcba:9876::1/126"],
      "auto_route": true, "strict_route": true }
  ],
  "outbounds": [ /* 节点 + urltest 池 + */ { "type": "direct", "tag": "direct" } ],
  "route": {
    "rule_set": [
      { "tag": "geosite-cn", "type": "remote", "format": "binary",
        "url": "https://raw.githubusercontent.com/SagerNet/sing-geosite/rule-set/geosite-cn.srs",
        "download_detour": "<primaryTag>" },
      { "tag": "geoip-cn", "type": "remote", "format": "binary",
        "url": "https://raw.githubusercontent.com/SagerNet/sing-geoip/rule-set/geoip-cn.srs",
        "download_detour": "<primaryTag>" }
    ],
    "rules": [
      { "action": "sniff" },
      { "protocol": "dns", "action": "hijack-dns" },
      { "ip_is_private": true, "outbound": "direct" },
      { "ip_version": 6, "action": "reject" },
      // hasSplit && !global && resi.domains.length>0 时：
      { "domain_keyword": [/* resi.domains */], "outbound": "residential-pool" },
      { "rule_set": ["geosite-cn", "geoip-cn"], "outbound": "direct" }
    ],
    "final": "<routeFinal>",
    "auto_detect_interface": true,
    "default_domain_resolver": "local"
  }
}
```

删除：`block`、`dns` 特殊出站；`{protocol:"dns", outbound:"dns-out"}`、`geoip`、`geosite` 规则；`inet4_address`、`sniff`、`stack`；`{query_type:["A","AAAA"]}` DNS 规则；`{tag:"local", detour:"direct"}` 的 `detour`（direct 是默认）。

规则顺序说明：`ip_is_private` 在 `ip_version: 6` 之前，保证 `::1`、ULA、link-local 仍直连；`ip_version: 6` 在所有域名规则之前，保证任何 IPv6 字面量（含嗅探到域名的）都被 reject 而不是送到服务器。

`Content-Disposition`、路由匹配、404 逻辑不变。

### 3.2 `b-ui-client.sh` — `generate_singbox_tun_config()`

1. `TUN_SCHEMA_VERSION` `"6"` → `"7"`（`ensure_tun_config_ready()` 据此强制重生成）。
2. TUN inbound `address`：
   - 主机 IPv6 可用 → `["172.19.0.1/30", "fdfe:dcba:9876::1/126"]`
   - 否则 → `["172.19.0.1/30"]`（现状）
   - "IPv6 可用"判定（新增函数 `host_ipv6_enabled()`，返回 0 表示可用）：`/proc/net/if_inet6` 存在，且 `sysctl -n net.ipv6.conf.all.disable_ipv6` 与 `net.ipv6.conf.default.disable_ipv6` 都为 `0`。判定细节以 §7 调研结论为准，实施时若调研给出更稳的规则以调研为准。
3. `route.rules`：在 `{ "ip_is_private": true, "outbound": "direct-out" }` 之后、国内域名直连规则之前，插入 `{ "ip_version": 6, "action": "reject" }`。无论是否加了 v6 地址都插入（IPv4-only 时该规则永不命中，无副作用）。
4. `dns.strategy` 保持 `ipv4_only`。其余规则、DNS、出站、校验与原子写逻辑不动。
5. `import_from_subscription()`：sing-box 分支在 `cp "$sub_file" singbox-tun.json` 之前，若 `host_ipv6_enabled` 为假且 `jq` 可用，用 `jq` 从 `.inbounds[] | select(.type=="tun") | .address` 中剔除含 `:` 的条目后再落盘；`jq` 不可用则原样落盘并 `print_warning` 提示。

### 3.3 服务端出站 IPv4-only

#### `server/core.sh`

- `configure_hysteria()` 的 `config.yaml`（直连实例）追加：
  ```yaml
  outbounds:
    - name: direct
      type: direct
      direct:
        mode: 4
  ```
  不加 `acl`（无 ACL 时使用第一个出站）。头部注释"无 outbounds 块"同步改掉。
- `config-residential.yaml` 的 `- name: direct` 出站加 `direct: {mode: 4}`（同样缩进风格写成多行）。`relay` 出站与 `acl: relay(all)` 不变。
- `configure_xray()`：`{"tag": "direct", "protocol": "freedom"}` → `{"tag": "direct", "protocol": "freedom", "settings": {"domainStrategy": "ForceIPv4"}}`。

#### `server/update.sh` — 新增 `v3.6.0 D9` 迁移块（放在 D8 之后，沿用 D 系列写法）

- `config.yaml`：若不含 `^outbounds:` → 追加上述 `outbounds` 块，`systemctl restart hysteria-server`，`updated=1`。
- `config-residential.yaml`：若含 `- name: direct` 且该出站之后没有 `mode: 4` → 在 `type: direct` 行后插入 `direct:` / `mode: 4`（缩进 4/6 空格），`systemctl restart hysteria-residential`。
- `xray-config.json`：若 `jq '.outbounds[] | select(.tag=="direct") | .settings.domainStrategy'` 非 `"ForceIPv4"` → jq 原子替换（tmp + mv）后 `systemctl restart xray`。
- 每步先备份（`.bak.v360.<ts>`），失败保留原文件，与 v3.5.7 A.fix2 风格一致。

#### `server/residential-helper.sh`

- 两个 `write_singbox_config_*` 的 `"strategy": "prefer_ipv4"` → `"ipv4_only"`。
- `PRIVATE_CIDRS` 追加 `"::1/128"`, `"fc00::/7"`, `"fe80::/10"`。
- `update.sh:347` 已在每次升级调用 `reapply`，中继配置自动重生成，无需额外迁移。

### 3.4 文档

#### `docs/v2rayn-tun-ipv6.md`（中文）

内容：
1. 现象与原理（服务器无 IPv6 出口，为什么要"接管并只走 IPv4"）。
2. 方案 A（推荐，改设置）：
   - 升级 v2rayN ≥ 7.24.9（7.24.7 起"无论是否启用 IPv6 地址，始终将 IPv6 路由到 TUN"）。
   - Tun 模式设置：严格路由 开；协议栈 mixed。
   - DNS 设置：直连/代理目标解析策略 = UseIPv4。注明 sing-box 内核下它映射为 `prefer_ipv4`（软），Hysteria2 节点若要硬 IPv4-only，把 v2rayN 的 sing-box DNS 模板里 `"strategy"` 改成 `"ipv4_only"`（给出完整可粘贴 JSON）。
   - 验证：test-ipv6.com 应显示无 IPv6；ipleak.net 无 v6 地址、DNS 只显示代理出口。
3. 方案 B（导入完整配置）：把 `https://<域名>/api/subscription/<用户名>` 作为 sing-box 自定义配置订阅导入 v2rayN；说明这份配置已内置 TUN、`ipv4_only`、IPv6 reject。
4. 已知限制：IPv6-only 站点不可达；自带 DoH 的浏览器仍可能解析到 v6，会被 reject 后自动回退 v4，属预期。
5. 标注：v2rayN 内的具体效果本项目未在 Windows 上实测，以上设置项名称来自 v2rayN wiki（2026-09 抓取），请按实际界面核对。

#### `README.md`

在客户端/订阅相关段落加一行链接指向上述文档。

### 3.5 `version.json`

- `version`: `3.6.0`
- `changelog["3.6.0"]`：IPv6 接管 + sing-box 订阅生成器现代化 + 服务端出站 IPv4-only + v2rayN 文档 + 住宅三项修复（各一条）。

## 4. 数据流

```
客户端 TUN (sing-box)
  app → DNS 查询 ──hijack-dns──► sing-box DNS (strategy ipv4_only)
        A → 正常答案；AAAA → NOERROR 空
  app → IPv4 目标 ──► 路由 ──► proxy-out (hy2/vless，携带 IP 或域名) ──► 服务器
  app → IPv6 目标 ──► ip_is_private? 直连 : reject(RST/ICMP) → app 回退 IPv4
  app → 自带 DoH 拿到 AAAA → 同上 reject → 回退

服务器
  Hysteria2 direct(mode 4)  ── 只拨 tcp4/udp4
  Xray freedom(ForceIPv4)   ── 内置 DNS(UseIPv4) 解析域名，只拨 IPv4
  中继 sing-box(ipv4_only)  ── 住宅 SOCKS5 远端解析；direct 只 IPv4
```

## 5. 错误处理与回滚

| 场景 | 处理 |
|---|---|
| 客户端主机 IPv6 禁用 | 不加 v6 地址，行为与 v3.5 相同 |
| 服务端订阅 JSON 被旧版 sing-box（<1.12）加载 | 不支持；`packages/versions.json` 分发的是 1.13.11，客户端安装流程优先服务端缓存 |
| rule_set 首次下载失败 | sing-box 照常启动并重试，cn 直连规则暂不生效，其余规则正常 |
| `update.sh` D9 任一步失败 | 保留备份，跳过该步，打印 warning，不阻塞其余升级 |
| Xray 26.9.9+ 对 `settings.domainStrategy` 打弃用警告 | 接受，自动迁移不影响行为；后续版本再切 `targetStrategy` |
| 回滚 | `git revert` 对应提交；已升级的服务器：删 `config.yaml` 的 `outbounds` 块、去掉 `mode: 4`、jq 删 `domainStrategy`，重启三服务；客户端：`TUN_SCHEMA_VERSION` 回退即重生成 |

## 6. 验收标准

1. `bash -n` 通过：`b-ui-client.sh`、`server/core.sh`、`server/update.sh`、`server/residential-helper.sh`；`node --check web/server.js` 通过；shellcheck 无新增 error 级告警。
2. 本地起 `web/server.js`（`BASE_DIR` 指向临时目录，含 fusion 用户、port-hopping、住宅 domains 三种组合），`curl /api/subscription/<user>` 的输出用 sing-box **1.13.19** 与 **1.14.0** 各跑 `sing-box check` 均退出 0、无 deprecated 警告。
3. 从 `b-ui-client.sh` 提取 `generate_singbox_tun_config` 在隔离环境生成 hysteria2 与 vless-reality 两份配置（分别模拟 IPv6 可用/不可用），`sing-box check` 同上通过；IPv6 可用时 `address` 含 v6 条目且 rules 含 `ip_version: 6` reject。
4. `xray run -test -c` 对 `core.sh` 生成的 `xray-config.json` 通过（下载 26.x 二进制到 scratchpad）。
5. `core.sh` 生成的两份 Hysteria2 YAML 通过 `python3 -c "import yaml"` 或 `yq` 解析，且 `outbounds[0].direct.mode == 4`。
6. `update.sh` D9 在一份 v3.5.23 风格的样例 `config.yaml` / `config-residential.yaml` / `xray-config.json` 上执行两次：第一次产生预期改动，第二次无改动（幂等）。
7. `docs/v2rayn-tun-ipv6.md` 存在，README 有链接。
8. `version.json` 版本 3.6.0，changelog 有 3.6.0 条目。
9. 用户线上验收（不在本任务内）：Linux 客户端开 TUN 后 test-ipv6.com 无 IPv6；v2rayN 按文档设置后同样。

## 7. 事实依据（2026-09-10 调研）

- sing-box v1.13.11 源码：`route/route.go` `actionSniff` 仅在 `OverrideDestination` 为真时改写目标，而 `option.RouteActionSniff` 只有 `sniffer`/`timeout`，无法置位 → 嗅探域名不影响出站目标。
- `dns/client.go` `Exchange()`：`ipv4_only` 时 AAAA 返回 `FixedResponseStatus(RcodeSuccess)` 空答案。
- `route/rule/rule_item_ipversion.go`：`ip_version` 按原始 `Destination` 判定；`invert`、logical 规则均可用。
- sing-tun v0.8.9 `tun_linux.go`：`auto_route` 装 `::/0`（需 v6 地址）；`strict_route` 对缺失地址族装 `unreachable` 规则。
- Hysteria2 `app/v2.9.3` `server.go` / `extras/outbounds/ob_direct.go`：`mode: 4` 只拨 IPv4；resolver 总查 A+AAAA；无 `outbounds` 时默认 direct auto；用户命名 `direct` 覆盖内置。
- Xray-core v26.9.9 `freedom`：AsIs 走 Go 系统解析器，`queryStrategy` 对其无效；`ForceIPv4` 无 A 记录则失败；`settings.domainStrategy` 于 26.9.9 弃用并自动迁移。
- sing-box 官方：`inet4_address/inet6_address` 1.12 移除；字符串式 DNS server 1.14 移除；inbound `sniff` 与 `block/dns` 出站 1.13 移除；`geoip/geosite` 1.12 移除；`domain_strategy` 拨号字段 1.14 移除。
- v2rayN：7.24.9 稳定版（2026-08-29）；7.25.x 预发布锁 sing-box ≤1.14；默认模板 `strategy: prefer_ipv4`、`strict_route: false`、带 v6 地址；wiki 明示 UseIPv4 在 sing-box 内核映射 `prefer_ipv4`。
- sing-box 1.14/1.15 弃用清单与本项目三处生成器的对照：见 §8（调研回填）。

## 8. sing-box 1.14 / 1.15 弃用对照

（待 `singbox-deprecation` 调研回填。）
