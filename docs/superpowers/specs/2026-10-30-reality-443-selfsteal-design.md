# REALITY 443 + self-steal 改造设计

- 日期：2026-10-30
- 状态：已定稿（主理人「先出 spec，然后开工」）；实现待派
- 范围：REALITY 从 `10001/10002 + www.bing.com` 改为 **客户端侧 443、单 inbound、self-steal 本地 Caddy**；保留「Reality直连 / Reality住宅」两个节点
- 不在本 spec：面板/订阅拆 `panel.` / `sub.` 子域、真实伪装站、Cloudflare Tunnel、HTTP/3 面板、mldsa65、spx、换 IP 选段、Xray 版本升级

## 0. 一句话

把 REALITY 的对外端口收敛到 **443**，伪装目标从 `www.bing.com` 换成 **本机 Caddy（self-steal）**；两个 REALITY 节点用 VLESS 的 `vlessRoute`（UUID 第 7/8 字节，客户端无感知）区分，仍然一个 inbound、一个 email、一套计量。Caddy 退到本地 `:9443`，面板/订阅继续走 `https://<域名>` 的 REALITY 回落。两台生产机的 443 被前置 haproxy 占着，走 `front=l4` 模式接入，NBDpsy 加速线原样保留。

## 1. 背景与证据

### 1.1 官方/维护者口径（A 级）

- Xray v26.3.27 release notes（2026-03-27）：「非 443 端口、偷苹果极易导致服务器 IP 被封锁」。
- Xray commit `157e65b`（2026-03-21）：`apple` / `icloud` target 警告。
- RPRX（XTLS/BBS #21）：「REALITY 推荐用法只有直接监听 443」「使用高位端口更是送人头给 GFW」。
- Xray 文档：target 要 TLS 1.3 + H2 + 不重定向；`serverNames` 与 target 同源；`limitFallback` 本身是指纹、不建议；`fallbacks` 是 VLESS 入站功能，REALITY 的回落是 `dest`。
- 社区最佳实践（B 级）：self-steal——自有域名 + 同机真实站点；REALITY 监听 443，`dest` 指本地 Caddy。

### 1.2 2026-10-26 的事实

IP 级 TCP 阻断，443 也中招；同 IP 的 UDP / ICMP / IPv6 正常（V2EX 1251991 / 1252881、ZibVPN 复盘）。⇒ 本次改造降低「被协议/端口/目标画像」的概率，**不保证**新 IP 不被同类事件打掉；UDP（HY2）、CDN、IP/段轮换仍是那类事件的对策。

### 1.3 b-ui 现状（代码事实）

- `crates/bui-schema/src/render/xray.rs`：两个 REALITY inbound `vless-direct`（`ports.reality_direct`）/ `vless-residential`（`ports.reality_resi`），共用一份 `clients`；住宅 inbound 按 `email`（= `user_id`）路由到槽 relay。
- `crates/bui-schema/src/model.rs`：`Reality { dest, server_names, … }`，`sni()` = `dest` 去端口；`crates/bui-schema/src/nodes.rs` 的 `server_name`、`render/hysteria.rs:90` 的 masquerade URL、`render/hy2_singbox.rs:149` 都从 `sni()` 推导。
- `crates/bui/src/modules/core_files.rs:143`：Caddyfile 是 `{domain} { reverse_proxy 127.0.0.1:8080 }`，Caddy 占 443。
- 生产机（bwg-rick / bwg-tizi）的 443 由 haproxy（L4，NBDpsy 海外加速）占用；tizi 还有一份 `bwg.baiyibaiyi.cn:8443` 的面板镜像站点。
- NBDpsy 生产服务器的 `GEMINI_PROXY` 用 tizi 的「服务器-reality」节点出海，配置文件是静态的（v3 xray-client），改端口会断，必须同步更新。

### 1.5 不可动的外部约束：443 上的反代线

主理人 2026-10-30 明示：**这两台机同时承担另一个项目的反向代理**，国外用户经这两台机的 443 访问国内源站。现有实现是 haproxy L4：

- tizi：haproxy `bind *:443`，按 `req.ssl_sni` 把 `www.nbdpsy.com` / `database.nbdpsy.com` 转到国内源站的 `:8443`，`send-proxy-v2`（源站靠它拿真实客户端 IP）；其余 SNI → 本地面板。
- rick：`table ip nbdpsy_l4` 把 IPv4 `443 → 8443`，haproxy 在 8443 做同样的 SNI 分流；**这张表不是孤儿，是反代线的入口，不能删**。
- 端口 80 上 tizi 还有一条 HTTP 分流（NBDpsy 域名 → 源站 `:80`，其余 → 本地面板 :80）。

⇒ 本次改造的硬边界：

1. `front=l4` 时 **haproxy 继续独占 443**，Xray 只监听 `127.0.0.1:10443`，只改 haproxy 的 `default_backend` 指向；`use_backend nbdpsy_origin` 的 SNI 规则、`send-proxy-v2`、超时、源站地址、80 端口 HTTP 分流**一个都不动**。
2. rick 的 `table ip nbdpsy_l4` 原样保留；不得以「清理遗留」为由删除。
3. Caddy 退到 9443 只影响面板/订阅这条回落路径；反代流量在 haproxy 就分流了，不经过 Xray/Caddy。
4. 验收必须包含反代线：从境外视角（或至少带 SNI 的 `curl --resolve`）确认 NBDpsy 域名仍到源站、PROXY v2 仍生效。
5. 新 IP 更换时，国外用户侧的入口地址/域名要同步更新——这是运维动作，与本次代码改造分开。

### 1.4 已验证的技术事实

- **`vlessRoute`**：VLESS 入站用 UUID 的第 7/8 字节（第三组 4 位 hex）作为路由数据；`validator` 用 `ProcessUUID` 把这两字节清零后查用户，所以**同一个 UUID 的任意第 7/8 字节都映射到同一个用户**，客户端只需发送改过这两字节的 UUID 字符串，sing-box / mihomo / v2rayN 都无需特殊支持。Xray 26.3.27 源码 `proxy/vless/validator.go`、`proxy/vless/inbound/inbound.go:536`、`app/router/config.proto` 的 `vless_route_list = 20` 均已确认。
- **REALITY 回落透明**：本地起 Caddy 在 `127.0.0.1:9443`，Xray REALITY `dest` 指它，未认证 TLS 客户端（openssl s_client）经 REALITY 回落拿到 Caddy 的页面 ⇒ self-steal 与 ACME TLS-ALPN 走 443 回落可行。
- **Caddy `proxy_protocol` + `tls` 监听包装器**：`listener_wrappers { proxy_protocol tls }`（顺序不能反）在 Caddy 2.11.7 实测能配合 Xray `xver: 1` 透传客户端 IP。
- Caddy 2.11.7 含 `caddy.listeners.proxy_protocol`。

## 2. 目标与非目标

### 2.1 目标

1. 客户端连接的 REALITY 端口恒为 **443**；服务端只有一个 REALITY inbound。
2. `dest` 指向本机 Caddy（self-steal），`serverNames = [node.domain]`；`sni()` 与 `dest` 解耦。
3. 保留两个节点（直连 / 住宅），不新增 email、不改计量与用户同步。
4. `sniffing` 加 `routeOnly: true`。
5. Caddy 退到本地 `:9443`，面板/订阅仍走 `https://<域名>`（443 的 REALITY 回落）；外部 9443 由 nft 封住。
6. 支持 `front=public`（Xray 直接 443，新装机默认）与 `front=l4`（Xray 监听 `127.0.0.1:10443`，前置 haproxy 把 443 转进来；两台生产机用）。
7. 旧 `ports.reality_direct/reality_resi` 归一为 443；所有端口/防火墙/自检/面板显示同步。
8. 全套测试 + 两台生产机的可执行迁移步骤。

### 2.2 非目标

- 面板/订阅拆 `panel.` / `sub.` 子域（Phase 2；见 §10）。
- 真实伪装站内容、Cloudflare Tunnel、面板 IP allowlist。
- 面板 HTTP/3（改造后 Caddy 不再占公网 443/UDP，HTTP/3 直连丢失；这是已知取舍，见 §9）。
- mldsa65、spx、shortId/密钥轮换、Xray 26.10 升级。
- 换 IP 的选段与观察（运维动作，不写进代码）。

## 3. 架构

### 3.1 数据面

```
client --TCP 443--> [haproxy L4 (front=l4)] --TCP 127.0.0.1:10443--> Xray REALITY (单 inbound)
                         |                                                  |-- 认证 + route=0 --> freedom (IPv4 only)
                         |                                                  |-- 认证 + route=1 --> socks 127.0.0.1:2080+i (槽 relay)
                         |                                                  |-- 未认证 ---------> dest 127.0.0.1:9443
                         |-- SNI = NBDpsy 域名 --> 外部源站（原样保留）          |
                                                                              +--> Caddy :9443 (面板/订阅 + sites/*.caddy)
client --TCP 443--> Xray REALITY (front=public，新装机)
```

- `front=public`：Xray 监听 `0.0.0.0:443`。
- `front=l4`：Xray 监听 `127.0.0.1:10443`；haproxy 监听公网 443（或 8443 + nft 重定向），`default_backend` 指向 `127.0.0.1:10443`；NBDpsy SNI 规则不动。
- Caddy 监听 `:9443`（所有地址），`inet bui` 的 `input` 链只放行 loopback 访问 9443；Xray `dest = 127.0.0.1:9443`，`xver: 1`。
- Caddy 仍在 `:80` 提供 ACME HTTP-01 与 `http → https://<域名>` 跳转。

### 3.2 两个节点怎么区分

| 节点 | UUID | 服务端 `vlessRoute` | 路由 |
|---|---|---|---|
| Reality直连 | `base` 的第 7/8 字节 = `0000` | 0 | 规则不匹配 → 兜底 `direct` |
| Reality住宅 | `base` 的第 7/8 字节 = `0001` | 1 | 先匹配「本用户 + route 1」→ 自己的槽；没规则时落到通用 route 1 → 兜底槽 |

- `base` 是 `User.credentials.vless_uuid`（state 里的原值）。Xray 的 `ProcessUUID` 清零第 7/8 字节后查用户，所以两个 UUID 映射到同一个 email。
- 新增 `bui_schema::sub::routed_uuid(base, route)`（或 `nodes::routed_uuid`）：解析失败时原样返回，不 panic；路由常量 `DIRECT_ROUTE = 0`、`RESIDENTIAL_ROUTE = 1`。
- 住宅节点 UUID = `routed_uuid(base, 1)`；直连节点 = `routed_uuid(base, 0)`。
- 住宅用户如果只有住宅权益（`entitlements.direct == false`），仍然只发住宅节点；反之只发直连节点。节点生成逻辑不变，只改 UUID 与端口。

### 3.3 路由规则（顺序即语义）

```
1. {"type":"field","ip":["::/0"],"outboundTag":"block"}                       # 服务端 IPv4-only，保留
2. {"type":"field","inboundTag":["api"],"outboundTag":"api"}
3. 每个住宅用户一条：
   {"type":"field","ruleTag":"resi-u-<user_id>","inboundTag":["vless"],
    "user":["<user_id>"],"vlessRoute":"1","outboundTag":"relay-slot-N"}
4. 通用住宅兜底：
   {"type":"field","ruleTag":"resi-route","inboundTag":["vless"],
    "vlessRoute":"1","outboundTag":"relay-slot-<fallback>"}
5. 最终兜底：
   {"type":"field","ruleTag":"resi-fallback","inboundTag":["vless"],"outboundTag":"direct"}
```

- 新住宅用户规则还没经 gRPC 落地的窗口里，route 1 会落到兜底槽（仍是住宅出口，fail-closed），不会走直连。
- 直连节点 route 0 不匹配 3/4，落到 5 → `direct`。
- `SlotRule` 增加 `vless_route: Option<u16>`；`rule_json` 写 `"vlessRoute": "<n>"`；`add_rule_request` 填 `vless_route_list`（proto 字段 20）。
- 通用规则 tag 用常量 `RESI_ROUTE_RULE_TAG = "resi-route"`，不能以 `USER_RULE_PREFIX` 开头（否则收敛会把线上规则当孤儿删掉）。
- `structural_hash` 仍剥离带 `user` 的规则；`resi-route` / `resi-fallback` 留在结构哈希里（兜底槽变化 = 重启）。

## 4. 配置渲染

### 4.1 Xray（`crates/bui-schema/src/render/xray.rs`）

- 删除 `vless-direct` / `vless-residential` 两个 inbound，改成单个 `tag: "vless"`。
- inbound：
  - `front=public`：`listen: "0.0.0.0"`, `port: 443`。
  - `front=l4`：`listen: "127.0.0.1"`, `port: 10443`（`L4_XRAY_PORT`）。
  - `settings.clients`：与现在一样，一人一条，`email = user_id`，`id = state 里的 base UUID`。
  - `realitySettings`：`dest: "127.0.0.1:9443"`（`CADDY_LOCAL_PORT`）、`serverNames: [node.domain]`、`privateKey`/`shortIds` 不变、`xver: 1`。
  - `sniffing`：`{"enabled": true, "destOverride": ["http","tls"], "routeOnly": true}`。
- 常量：`REALITY_PORT = 443`、`CADDY_LOCAL_PORT = 9443`、`L4_XRAY_PORT = 10443`，放在 `bui-schema`（建议 `render/mod.rs` 或 `slots.rs`）供 `crates/bui` 复用。
- `config()` 的 `paths` 参数不变；`api` inbound / outbounds / DNS / IPv4 出站策略不变。

### 4.2 Caddyfile（`crates/bui/src/modules/core_files.rs`）

```caddy
{
	auto_https disable_redirects
	log default { … 保持现有 … }
	servers :9443 {
		listener_wrappers {
			proxy_protocol
			tls
		}
	}
}

{domain}:80 {
	redir https://{domain}{uri} permanent
}

https://{domain}:9443 {
	@sub path /api/sub/* /api/subscription/* /api/clash/* /api/nodes/*
	log_skip @sub
	reverse_proxy 127.0.0.1:{admin_port}
	log { … 保持现有 … }
}

import {sites_glob}
```

- 不写 site 级 `bind`：9443 由 nft `input` 链挡外部；`bind 127.0.0.1` 会把 ACME HTTP-01 的 `bind_host` 也锁到 loopback，导致公网 80 的挑战失败。
- `listener_wrappers` 的顺序必须是 `proxy_protocol` 再 `tls`（实测顺序反了会把 TLS 连接包在代理层里）。
- `auto_https disable_redirects` + 显式 `:80` 站点：跳转目标是 443，不是 9443。
- ACME 不写死挑战类型：HTTP-01（80）为主，TLS-ALPN（443 → REALITY 回落 → 9443）为备；两条路都不禁用。
- Rust `format!` 模板里 `{uri}` 要转义成 `{{uri}}`；`{domain}`/`{admin_port}`/`{sites_glob}` 保持现有。
- 外部站点 `import` 位置不变；tizi 的 `nwg...:8443` 镜像文件属于用户目录，本 spec 只要求迁移时删除（§7）。

### 4.3 nft（`crates/bui-schema/src/render/nft.rs`）

`table inet bui` 增加一个 input 基础链：

```
chain input {
	type filter hook input priority filter; policy accept;
	iifname != "lo" tcp dport 9443 counter drop
	iifname != "lo" udp dport 9443 counter drop
}
```

- 目的：Caddy 的 9443 只服务本机 Xray 回落，不暴露公网。
- `nft status` / selfcheck / watchdog 增加 `input` 链的检查；`egress_signature` 扩成 `guard_signatures`（或新增 `input_signature`），把这两条规则纳入比对。
- `b-ui.service` 的 `ExecStartPre=-{bin}/bui nft apply` 与 `caddy After=b-ui.service` 保证开机先落表；无需新 unit。

### 4.4 状态模型（`crates/bui-schema/src/model.rs`）

- `Reality` 不变字段，新增：
  ```rust
  #[serde(default)]
  pub front: RealityFront,
  ```
  `enum RealityFront { #[default] Public, L4 }`，serde 小写 `"public"` / `"l4"`。
- `State::normalize()`（新方法，加载 state 后调用）：
  - `reality.dest = "127.0.0.1:9443"`
  - `reality.server_names = vec![node.domain.clone()]`
  - `node.ports.reality_direct = REALITY_PORT`、`reality_resi = REALITY_PORT`
  - 缺省 `front = Public`（serde default 已覆盖）
  - 归一后写回 state 文件（守护进程启动那一轮；日志一行 `REALITY 已归一为 self-steal`）。
- `Reality::sni()` 改为 `server_names.first()`，缺失时回退 `dest` 去端口（与现在兼容）；所有调用方语义变成「客户端该用的 SNI」。
- `Ports.reality_direct/reality_resi` 保留字段（v3 反序列化），文档标「已废弃，恒 443」。
- `bui set reality-front public|l4`（CLI + API 如已有 set 通道）：写 `front`，触发对账。安装默认 `public`。

### 4.5 展示与自检

- `nodes_for`：两个 REALITY 节点的 `host = node.domain`、`port = REALITY_PORT`；UUID 分别 `routed_uuid(...,0)` / `routed_uuid(...,1)`；label / kind 不变。
- `api_admin` 的 `xrayPort` 显示 443；`sni` 用新的 `Reality::sni()`。
- `selfcheck` / `env_probe` / `watchdog` 的端口检查用「实际监听」：`public` → 443，`l4` → `127.0.0.1:10443`；防火墙放行 443（TCP），不再放行 10001/10002。
- `modules/system.rs::firewall_ports`：现实端口用 443（去重），其余不变。
- `modules/panel/mod.rs::XRAY_INBOUND_TAGS = ["vless"]`；`users.rs` 的同步对单 tag 工作（逻辑不变，少一个 tag）。
- `modules/panel/xray.rs::add_rule_request`：`RoutingRule.vless_route_list = Some(PortList { range: vec![PortRange { from: 1, to: 1 }] })`（仅当 `SlotRule.vless_route` 有值）。

## 5. 兼容与迁移语义

- 旧订阅（10001/10002）在部署后不可用；客户端刷新订阅即拿到新节点（同域名、443、两个不同 UUID）。`bui-c` 的节点集合会替换，tombstone 逻辑照常。
- 旧 state 的 `reality.dest=www.bing.com:443`、`server_names=[www.bing.com]` 由 `normalize()` 覆盖；面板/订阅/ HY2 masquerade 全部改用 `node.domain`。
- `v3 import` 读出的 dest/server_names 同样被归一；import 测试的期望值改为 self-steal。
- `ports.reality_direct/reality_resi` 写回 443；v3 导入仍能解析旧 JSON。
- 安装交互：去掉「REALITY 伪装站」提问；`Answers.masquerade` 保留字段但忽略/写日志（非交互 JSON 兼容），`build_state` 直接写 self-steal 的 dest/server_names。非交互 `--masquerade` 参数保留但提示已废弃（不报错）。
- 版本：行为/端口变更，按 minor 发布（建议 4.2.0），CHANGELOG 单列「REALITY 443 + self-steal」。

## 6. 测试清单

### 6.1 单元/渲染测试

- `routed_uuid`：route 0/1、第三组 hex 正确、非法 UUID 原样返回、base 的 6/7 字节被替换。
- `nodes_for`：双权益用户得到两个节点、host/port 相同、UUID 第三组 `0000`/`0001`；只有直连/只有住宅的用户各得一个；住宅凭据逻辑不变。
- `xray::config`：单 inbound tag `vless`；`public` port 443 / `l4` listen 127.0.0.1:10443；dest/serverNames/xver；`sniffing.routeOnly`；规则顺序与 `vlessRoute`；`structural_hash`/`slot_rules_hash` 在加用户/改槽时不重启、兜底槽变化时重启。
- `slot_rules`：per-user + `resi-route` + `resi-fallback`；`rule_json` 的 `vlessRoute` 字符串；`add_rule_request` 的 `vless_route_list`。
- Caddyfile：`https://{domain}:9443`、`servers :9443`、`proxy_protocol` 在 `tls` 前、`:80` 跳转、无 site 级 `bind`、`auto_https disable_redirects`。
- nft：input 链两条 drop；`input_signature` 稳定；`nft status`/selfcheck 用例。
- 面板：`XRAY_INBOUND_TAGS` 单 tag；sync 测试的 tag 字符串更新；`api_admin` 的端口/SNI。
- `kernel_xray`：真实 26.3.27 `xray run -test` 通过渲染配置（含 `vlessRoute`）。
- `golden_subscription`：base64 / sing-box / mihomo / client 四份 golden 更新（端口 443、两个 UUID）。
- `kernel_client`、`bui-c` 的 fixture 端口可保持测试值（客户端不依赖服务端端口）；只改服务端节点 goldens。

### 6.2 生产验收

- `bui status` 六单元 active、无漂移；`xray run -test`。
- `openssl s_client -connect <域名>:443 -servername <域名>` 未认证回落拿到面板 200（self-steal）。
- `curl -4 https://<域名>/` 200；面板登录；订阅四个端点 200。
- v2rayN / `bui-c` 各拉一次订阅：两个 Reality 节点同 host/port，UUID 不同；直连节点出口 = VPS IPv4，住宅节点出口 = 槽 IP。
- 住宅用户在**通用规则尚未收敛**的窗口内（新建用户立即连）走兜底槽，不走直连（可人工制造规则缺失验证）。
- `ss -ltnp`：公网只有 443（或 haproxy 的 443）与 80；9443 只被 loopback 访问；`nft list table inet bui` 有 input 链。
- 证书：Caddy 日志无 ACME 错误；强制续期一次（Caddy admin `/certificates/.../renew`，以实际 API 为准）确认 HTTP-01/TLS-ALPN 至少一条成功。
- **反代线（不可回归项）**：带 SNI `www.nbdpsy.com` / `database.nbdpsy.com` 的 TLS 请求仍到国内源站；源站看到的客户端 IP 仍来自 `send-proxy-v2`（不是 VPS 的 127.0.0.1）；tizi 的 80 端口 HTTP 分流仍把 NBDpsy 域名送到源站、其余送到本地面板；rick 的 `table ip nbdpsy_l4` 仍在。至少从境外视角（或用户侧现网）实测一次。
- NBDpsy 服务器的 GEMINI_PROXY 更新后用新节点出海正常（与上面的反代线是两条独立链路）。

## 7. 两台生产机的迁移（bwg-rick / bwg-tizi）

> 生产机只写 SSH 别名；真实域名/IP 不落文档。

### 7.1 前置

1. 深快照（`/opt/b-ui-backups/pre-4.2.0-<ts>`，含 `bin/`、`state.json`、`Caddyfile`、`xray-config.json`、`nft-before.conf`）。
2. 备份 `/etc/haproxy/haproxy.cfg` 与 `/opt/b-ui/caddy/sites/`；**逐行抄下**当前反代线的 `use_backend ... req.ssl_sni`、`server ... send-proxy-v2`、`timeout`、80 端口 HTTP 分流，作为改后对照。
3. 确认 rick 的 `table ip nbdpsy_l4` 仍在（`nft list table ip nbdpsy_l4`）；它和反代线同生共死，**不动**。
4. 确认 NBDpsy 服务器的 `GEMINI_PROXY` 当前节点可回滚（旧 xray-client 配置另存）。
5. 新 IP 已确定（本次不换 IP 也可先上代码；换 IP 时按旧 runbook 改 `state.node.public_ip`，并同步国外用户侧的反代入口地址）。

### 7.2 升级顺序（每台）

```bash
# 1) 停 b-ui，避免旧 daemon 用新 state 渲染
systemctl stop b-ui

# 2) 把 state 的 front 改成 l4（新二进制 normalize 会补 dest/server_names/ports）
#    用 python3 就地改，chmod 600；或新二进制的 `bui set reality-front l4`
python3 - <<'PY'
import json
p='/opt/b-ui/state.json'
s=json.load(open(p))
s['node'].setdefault('reality', {})['front']='l4'
json.dump(s, open(p,'w'), ensure_ascii=False, indent=2)
PY

# 3) 换二进制（新版），启动
install -m755 /tmp/bui-4.2.0 /opt/b-ui/bin/bui
systemctl start b-ui
sleep 8

# 4) haproxy 只改 default backend 的那一行：
#    rick: backend b_ui_local  server local 127.0.0.1:443  → 127.0.0.1:10443
#    tizi: backend b_ui_local  server local 127.0.0.1:8443 → 127.0.0.1:10443
#    ⛔ 不要动 use_backend nbdpsy_origin / req.ssl_sni / send-proxy-v2 / 80 前端 / rick 的 nft 表
haproxy -c -f /etc/haproxy/haproxy.cfg   # 先校验，失败就不要 reload
systemctl reload haproxy

# 5) tizi 额外：删除 bwg.baiyibaiyi.cn:8443 的面板镜像站点文件（用户目录），reload caddy
#    rick 的 imported-from-v3.caddy 只有注释，不动

# 6) 验收（§6.2）
```

- 顺序注意：第 4 步之前，面板的 443 仍由 haproxy → 旧 Caddy 443/8443 服务；Xray 新配置已起在 10443，但没有流量。第 4 步切流后，面板与 REALITY 都走 Xray；短暂断连预期内。
- `front=l4` 时 Xray 不再监听 443；haproxy 仍占 443，NBDpsy SNI 规则在 default 之前，原样生效。
- `tizi` 的 mirror 文件删除后，`/opt/b-ui/caddy/sites/` 只剩 README；Caddy reload 后 8443 释放。
- 两台都确认 `state.node.public_ip` 是新 IP。

### 7.3 NBDpsy 服务器（跨项目）

tizi 的 REALITY 节点从 `:10001` 改为 `:443`，SNI 从 `www.bing.com` 改为 tizi 面板域名。NBDpsy 服务器的 `/opt/hysteria-client/xray-config.json`（v3 xray-client）需要更新 VLESS outbound：

- `address` = tizi 域名（不变）
- `port` = 443
- `id` = 该用户「服务器-reality」的 **route 0 UUID**（base 第三组 `0000`）
- `serverName` = tizi 面板域名
- `publicKey` / `shortId` / `flow` / `fingerprint` 不变
- 验证：`curl --socks5 127.0.0.1:1080 https://www.google.com` 200，出口 IP 为 tizi。

### 7.4 回滚

1. `systemctl stop b-ui`
2. 恢复快照里的 `state.json`（旧 dest=bing、ports=10001/10002）与旧 `bin/bui`
3. 恢复 haproxy 配置（rick → 127.0.0.1:443；tizi → 127.0.0.1:8443）并 reload；恢复 tizi 的 mirror 文件
4. 启动 b-ui，`bui reconcile`/`bui status` 验收
5. 客户端刷新订阅回到旧节点

> 注意：新 state 的 self-steal 归一值不能直接喂给旧二进制（旧渲染器会尝试用 443 + 本地 dest），回滚必须恢复**快照里的 state.json**，不能只换二进制。

## 8. 风险

1. **ACME 续期**：证书同时服务面板与 REALITY；HTTP-01 与 TLS-ALPN 双路，任一成功即可。上线后强制续期一次验证；若失败，回退到「Caddy 0.0.0.0:9443 + 保留 HTTP-01」已在本设计内（我们本来就没 bind），只需确认 9443 的 nft 封堵不影响 80。
2. **vlessRoute 客户端兼容**：UUID 是普通字符串，sing-box/mihomo/v2rayN 不需要新字段；但仍需在 v2rayN 与 `bui-c` 上实测两个节点（§6.2）。26.9+ 的 Xray 客户端也支持。
3. **haproxy 是外部配置，且同时承载反代线**：一行改错会同时打掉面板、REALITY 和另一个项目的国外反代；必须先 `haproxy -c` 校验、先备份、失败立即回滚。NBDpsy 的 SNI 规则在 default 之前，正常只改 default 不会碰它，但 reload 失败会整体不生效，影响面仍是 443 全量。
4. **反代线的真实客户端 IP**：源站依赖 `send-proxy-v2`；改 haproxy 时不要顺手删掉它或改 backend 名称，否则源站会把所有国外用户看成 VPS 的 127.0.0.1/内网地址。
5. **rick 的 nft 表**：`table ip nbdpsy_l4` 是 443 入口，不是遗留；任何「清理孤儿 nft 表」的动作都要先确认这条。
6. **NBDpsy 静态客户端**：不同步更新会断 GEMINI_PROXY；迁移窗口内优先处理。
7. **HTTP/3 面板**：Caddy 不再占公网 443/UDP，浏览器 HTTP/3 直连面板丢失；TCP 恢复后不影响。若以后需要，走 Cloudflare Tunnel/CDN（Phase 2）。
8. **9443 暴露**：依赖 `inet bui` 的 input 链；b-ui 的 `ExecStartPre` 与 `caddy After=b-ui.service` 保证先落表。若手工 `nft flush table inet bui`，9443 会在下一次 watchdog/对账前暴露；运维不要 flush。
9. **旧客户端不断订阅**：旧端口停用后直连/住宅 REALITY 都不可用；HY2 不受影响。发通知让用户刷新订阅。

## 9. 明确不做/后续

- **Phase 2（面板/订阅拆子域）**：`panel.<domain>` / `sub.<domain>`，主域做真实站点；面板控制口 IP allowlist 或 Cloudflare Tunnel；订阅链接改用 `sub.<domain>`；Caddy 多站点块与证书；客户端 `.cn` 直连规则已覆盖。需要新增 DNS 记录与真实站点内容，另行设计。
- mldsa65（target 证书 >3500 才可开；sing-box 1.14 不校验）、spx 每客户端随机、shortId/privateKey 轮换。
- Xray 26.10 升级（26.9 强制 PQ ClientHello，需测旧客户端）。
- 换 IP 选段、敏感期观察、tizi 热备切换。

## 10. 关键文件索引

| 改动 | 文件 |
|---|---|
| 状态/归一/常量 | `crates/bui-schema/src/model.rs`、`render/mod.rs`（或 `slots.rs`） |
| UUID route | `crates/bui-schema/src/nodes.rs`（`routed_uuid` + 两个节点） |
| Xray 渲染 | `crates/bui-schema/src/render/xray.rs` |
| 订阅/客户端 goldens | `crates/bui-schema/src/render/{subscription,client}.rs`、`tests/golden_subscription.rs` |
| Caddyfile | `crates/bui/src/modules/core_files.rs` |
| nft / 守卫 | `crates/bui-schema/src/render/nft.rs`、`crates/bui/src/modules/watchdog.rs`、`commands/{nft,selfcheck}.rs` |
| 端口/防火墙 | `crates/bui/src/modules/system.rs`、`sys/env_probe.rs` |
| 面板/同步 | `crates/bui/src/modules/panel/{mod,xray,users,api_admin}.rs` |
| 安装/CLI | `crates/bui/src/commands/install.rs`、`cli.rs`、`api/mod.rs` |
| 测试 | `crates/bui-schema/tests/{kernel_xray,kernel_nft,kernel_client,golden_subscription}.rs`、各模块单测 |
