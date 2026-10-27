# 服务端出站强制 IPv4 / 封死 IPv6 通路（2026-10-27）

## 背景

4.1.8 起 `node.direct_ip_policy = dual-stack` 会把 Hysteria 直连改成 `mode: auto`、
Xray freedom 改成 `ForceIPv4v6`，两台生产机靠宿主机 `ipv6net` SIT 隧道走 IPv6 出站。
2026-10-27 主理人裁决：**IPv6 出站会泄露我们的信息，全部封死**。客户端请求无论以
IPv4 还是 IPv6 到服务端，服务端一律经 IPv4 出去。

## 目标

1. 服务端四条代理通路的出站一律 IPv4：HY2 直连、Xray 直连、relay 直连、住宅上游。
2. 服务端主机不再新建任何 IPv6 出站连接（升级下载、ACME、住宅探针、伪装反代、
   内核自身），同时保留 IPv6 **入站**（客户端仍可经 IPv6 连到服务端，回包走
   conntrack established）。
3. 不再提供双栈开关；旧 state 里的 `dual_stack` 仍可反序列化，但渲染一律 IPv4。
4. 客户端接管 IPv6，并拒绝**全局** IPv6 字面量；私网 IPv6（`::1` / `fc00::/7` /
   `fe80::/10`）仍按原设计直连，不算外泄。

## 非目标

- **IPv6-only 目标不可达**：经 IPv4 出站无法到达只有 AAAA 的域名或字面 IPv6 目标；
  这是封死的代价，不是缺陷。
- **住宅供应商自己的出口地址族**：TCP 经供应商时域名原样交给供应商解析，供应商
  可能用 IPv6 连目标；那是供应商的出口，不是 VPS 或客户端出口。relay 的 UDP
  `resolve` 仍强制 `ipv4_only`。要连供应商出口也强制 IPv4，只能本地解析后发
  IPv4 字面量，会破坏住宅地理位置，故不做。
- **SOCKS 模式下不走本地代理的应用流量**：bui-c 无法接管；完整封堵只在 TUN 模式。

## 已确认的漏点（2026-10-27 实测）

| # | 漏点 | 事实 |
|---|---|---|
| 1 | Hysteria 直连 | `mode: 4` 对域名只取 A，字面 IPv6 直接失败，本身已是硬钉子 |
| 2 | Xray 直连 | `ForceIPv4` 只约束域名解析；字面 `[::1]` 仍走 freedom 拨 v6（实测 200） |
| 3 | relay 直连 | `direct` 出站无任何地址族字段；split 的 `final: direct`、`udp/53 → direct`、pin 端口规则都会让字面 v6 从 VPS 出网 |
| 4 | 住宅 HY2 / Xray 住宅 | 目标原样交给 relay，再由 relay 的 direct 或供应商出站 |
| 5 | 客户端 | `bui-c` 的 cloudflared、UDP/7844、22/2222、源端口 22/2222 四类直连例外排在 `ip_version 6 reject` 之前，命中字面 v6 时物理直连 |
| 6 | 管理面 | 升级/内核/客户端包下载（reqwest）、公网 IP 探测（curl）、Caddy ACME、
住宅探针 `dial`/`direct_tcp`/`gateway_tcp_within`（`to_socket_addrs`）、两个
masquerade 反代（Go `http.DefaultTransport`）都可双栈 |
| 7 | OS 级 | 没有 ip6tables filter、没有 `net.ipv6.*` sysctl、unit 没有地址族限制 |

## 机制决策（全部经本地内核实测）

### 1. 不再有双栈开关

- `DirectIpPolicy::DualStack` 保留在 enum 里只为旧 `state.json` 能反序列化；
  `FromStr`（CLI / 控制 API 共用）对 `dual-stack` 返回明确错误。
- `hysteria::direct_outbound` 恒为 `mode: 4`；`xray::config` 恒为
  `ForceIPv4` / `UseIPv4`。渲染不再读 `node.direct_ip_policy`。
- `bui set direct-ip-policy ipv4-only` 保留（可把旧 state 写回默认值），
  `dual-stack` 被拒绝。

### 2. Xray：字面 IPv6 走 blackhole

`routing.domainStrategy` 显式写 `AsIs`，路由首条：

```json
{"type":"field","ip":["::/0"],"outboundTag":"block"}
```

并增加 `{"tag":"block","protocol":"blackhole","settings":{}}`。实测：`AsIs` 下
域名不在路由阶段解析，`localhost` 的 A 落在被 block 的 127/8 里仍走 freedom；
字面 `[::1]` 被 `taking detour [block]`；AAAA-only 域名走 freedom 后因
`ForceIPv4` 无 A 而失败。

### 3. relay：首条「全局 IPv6 字面量」reject

`route.rules` 第一条（在 `sniff` 之前）：

```json
{"type":"logical","mode":"and","rules":[
  {"ip_version":6},
  {"ip_is_private":true,"invert":true}
],"action":"reject"}
```

用 logical + invert 而不是裸 `ip_version: 6`，是为了让 `::1` / `fc00::/7` /
`fe80::/10` 继续落到 relay 现有的私网 `ip_cidr → direct` 规则（保持原行为），
只拒绝全局 IPv6 字面量。实测：`::1` 走 private→direct，`2001:db8::1` 被
`match[0] ... => reject`；域名不在路由阶段解析，原样交给 `direct` 或
`resi-pool`。relay 现有 `dns.strategy: ipv4_only` +
`route.default_domain_resolver: dns_direct` 已让 direct 出站的域名只取 A。

**不要写 `domain_strategy`**：sing-box 1.14.3 把 `domain_strategy` 判为已弃用的
legacy 字段，`sing-box check` 与 `run` 都退出 1（`FATAL ... set environment
variable ENABLE_DEPRECATED_LEGACY_DOMAIN_STRATEGY_OPTIONS=true`）。socks/http
出站的同名字段也只管**供应商服务器地址**，不是用户目标。

**不要用 `ip_is_private: false`**：1.14.3 只在字段为 true 时添加匹配项，false
等于没写，规则会退化成「所有 v6 都拒绝」。

### 4. Hysteria

`direct.mode` 恒为 `4`。无其它改动；其 masquerade 反代由第 5 条的 nft 链封住。

### 5. `table inet bui` 增加主机级 IPv6 出站封禁链

在现有 nat 链之外增加 `ipv6_egress`（filter / output / priority 0）：

```
meta nfproto ipv6 fib daddr type local counter accept          # 本机地址
meta nfproto ipv6 ip6 daddr { fe80::/10, ff00::/8 } counter accept  # 链路本地/组播（NDP/MLD）
meta nfproto ipv6 ct state established,related counter accept  # 入站连接的回包
meta nfproto ipv6 icmpv6 type { destination-unreachable, packet-too-big,
  time-exceeded, parameter-problem } counter accept             # 入站连接需要的 ICMPv6 错误
meta nfproto ipv6 meta l4proto tcp counter reject with tcp reset
meta nfproto ipv6 counter reject with icmpx type admin-prohibited
```

要点：

- 新建的 IPv6 出站一律立即 reject（不是 drop），让 Go/reqwest/Caddy 快速回落 IPv4。
- `ct state established,related` 保住 IPv6 入站连接的回包；`fib daddr type local`
  与 link-local/组播放行保住 NDP、MLD 与本机自连。
- 该链随 `table inet bui` 一起 `flush` + 重放；watchdog 每 60 秒自愈。**自愈判据
  必须覆盖这条链**（现有 `rule_count` 只比 redirect 规则，不能只在链缺失时沉默）。
- `bui nft status` 要能看出封禁链在不在。
- 现有 netns 用例的 IPv6 探测会被这条链拒掉：先只应用 nat 部分跑完重定向用例，
  再应用完整规则集验证「新 IPv6 出站被拒 / IPv4 不受影响 / IPv6 入站回包放行」。

### 6. 管理面与安装期

- `probe_public_ip` 的 curl 加 `-4`；`install.sh`、`scripts/bui-c-install.sh`
  的 curl/wget 加 `-4`。
- `crates/bui` 的 `HttpFetcher` 与 `crates/bui-c` 的 `ReqwestNet` 加一个
  「只返回 A 记录」的 `reqwest::dns::Resolve`。这样安装期/每日自更新在 nft 链
  生效前也不会走 IPv6；`bui-c` 的 SOCKS 模式自更新也不会物理直连 v6。
- 住宅探针的 `dial` / `direct_tcp` / `gateway_tcp_within` 过滤
  `to_socket_addrs`，只拨 IPv4。
- 单元顺序：`b-ui.service` 加
  `ExecStartPre=-{bin}/bui nft apply`，五个数据面单元加
  `After=b-ui.service`，保证开机时封禁链先于内核起来。`hysteria-residential`
  现有的 `ExecStartPre=-… bui nft apply` 保留。
- `env_probe` 的「IPv6 出口」行改为反映策略（已封禁），不再用「有默认 v6 路由」
  冒充可出站。

### 7. 客户端

`bui-c` 主配置与测速配置的 route 规则最前面加一条 logical 规则（只拒绝全局
IPv6 字面量，私网 v6 继续落到已有的 `ip_is_private → direct-out`）：

```json
{"type":"logical","mode":"and","rules":[
  {"ip_version":6},
  {"ip_is_private":true,"invert":true}
],"action":"reject"}
```

然后才是 cloudflared / UDP 7844 / 22 / 2222 的直连例外。现有规则里后面那条
`{"ip_version":6,"action":"reject"}` 删除（它会把私网 v6 也拒掉，与新规则重复）。
实测依据：`ip_is_private: false` 在 1.14.3 是 no-op，不能用它表达「排除私网」；
logical `ip_version 6 && !(ip_is_private)` 已验证：`::1` 直连、
`fe80::1`/`fc00::1` 直连尝试、`2001:db8::1` reject。

订阅：

- sing-box 订阅现有顺序（`ip_is_private → direct`，再 `ip_version 6 reject`）
  已经正确，不改；只更新注释里过期的 `node.direct_ip_policy` 说法。
- mihomo 订阅把 `IP-CIDR6,fc00::/7,DIRECT`、`IP-CIDR6,fe80::/10,DIRECT`、
  `IP-CIDR6,::/0,REJECT,no-resolve` 三条整体提到 `GEOIP` / `GEOSITE` 之前，
  避免 `GEOIP,cn,DIRECT` 先把全局 IPv6 字面量判成直连。`::1/128` 补一条 DIRECT。

客户端诊断：

- 菜单的 IPv6 观测行不再用 `Via::Direct` 物理直连；改用当前 profile 的
  `via`（SOCKS 模式经本地代理，TUN 模式受捕获），文案改为「IPv6 出站已封禁/未确认」。
- `bui-c check` 的 `capture_failures` 增加 IPv6 默认路由检查：存在 IPv6 默认路由
  时必须指向 `bui-tun`，否则报 `TunNoDefaultRouteV6`。这条只做观察，不自动改路由。

## 验证

- `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、
  `cargo test --workspace`。
- 内核校验用例：渲染出的 xray / relay / hy2 / 客户端配置过 `xray run -test`、
  `sing-box check`（1.14.3，不得引入 `domain_strategy`）。
- nft：`nft -c -f` 语法通过；netns 用例覆盖出站拒绝与入站回包放行。
- 实测：字面 `[::1]` 在 Xray / relay / HY2 三条通路均失败；A+AAAA 域名走 A；
  AAAA-only 域名失败；IPv4 目标不受影响。

## 风险与已知残留

- 主机级封禁会影响 Caddy 托管的外部站点：其 IPv6-only 上游将不可达。
- 系统 DNS 必须能经 IPv4 解析；若 resolv.conf 只有 IPv6 nameserver，DNS 会断。
- 住宅 TCP 经供应商时，供应商自身可能用 IPv6 连目标（非本机出口）。
- `host_has_ipv6` 的探测只看默认路由设备的全局地址，策略路由场景可能假阴性；
  新的 `TunNoDefaultRouteV6` 会把它报出来，但不会自动修。
- IPv6-only 目标从此不可达。
