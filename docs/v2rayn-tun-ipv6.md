# v2rayN TUN 模式 IPv6 设置（服务端出站仅 IPv4）

## 为什么

B-UI 服务端的四条代理出站一律 IPv4（2026-10-27 裁决，没有双栈开关），主机新 IPv6 出站也由 `table inet bui` 的 `ipv6_egress` 链封死，只保留 IPv6 入站。TUN 模式下若本机 IPv6 没被接管，访问支持 IPv6 的网站仍会直接从本地网络走 v6，泄漏真实地址；若被接管但隧道出口是 IPv4，IPv6-only 目标会连接失败。

目标：IPv6 全部进隧道、DNS 只要 A 记录、**全局** IPv6 字面量在客户端直接拒绝（私网 `::1` / `fc00::/7` / `fe80::/10` 仍直连），让应用回退 IPv4。IPv6-only 目标从此不可达，这是封堵的代价，不是配置错误。

## 方案 A：改 v2rayN 设置（推荐）

1. 升级 v2rayN 到 **7.24.9** 或更新（7.24.7 起「无论是否启用 IPv6 地址，始终将 IPv6 路由到 TUN」）。
2. **设置 → Tun 模式设置**：`严格路由` 开；`协议栈` 选 `mixed`。
3. **设置 → DNS 设置**：`直连目标解析策略` 与 `代理目标解析策略` 都设为 `UseIPv4`。

   注意：sing-box 内核下 `UseIPv4` 对应 `prefer_ipv4`，是软偏好——只有 AAAA 记录时仍会返回 IPv6。Hysteria2 节点走 sing-box 内核，若测试站仍显示 IPv6，把 sing-box DNS 模板里的 `"strategy"` 改成 `"ipv4_only"`（硬 IPv4-only）。

   下面是这份模板的形状，实际操作只需改 `strategy` 一项，其余规则按你界面里的原样保留：

   ```json
   {
     "servers": [
       { "tag": "remote", "type": "tcp", "server": "8.8.8.8", "detour": "proxy" },
       { "tag": "local", "type": "udp", "server": "223.5.5.5" }
     ],
     "rules": [
       { "rule_set": ["geosite-cn"], "server": "local" }
     ],
     "final": "remote",
     "strategy": "ipv4_only"
   }
   ```

4. 验证：
   - <https://test-ipv6.com> 应显示「无 IPv6」；
   - <https://ipleak.net> 无 v6 地址，DNS 只出现代理出口。

## 方案 B：导入完整 sing-box 配置

在面板里复制该用户的订阅链接（token 链接，形如 `https://example.com/api/sub/0123456789abcdef0123456789abcdef#alice`），把路径里的 `/api/sub/` 换成 `/api/subscription/` 并去掉末尾的 `#备注`，得到 `https://example.com/api/subscription/0123456789abcdef0123456789abcdef`，作为 sing-box 自定义配置订阅导入 v2rayN。示例里的域名与 token 都是占位符，以面板里复制到的为准；末段是该用户的随机订阅 token，不是用户名。

这份配置已内置 TUN（v4+v6 地址）、`ipv4_only`、IPv6 reject、国内直连规则，也可以直接给独立 sing-box 使用。

## 已知限制

- 只有 IPv6 地址的网站在隧道开启时无法访问（服务端出站一律 IPv4）。
- 浏览器自带 DoH 仍可能解析到 IPv6，连接会被本机立即拒绝并自动回退 IPv4，属预期行为。
- **SOCKS 模式**下不走本地代理的应用流量不经过本客户端的拒绝规则，不在保证内；完整封堵只在 TUN 模式。
- 住宅节点经供应商时，目标域名原样交给供应商解析；供应商自己的出口地址族不在本机控制内。
- 服务端管理面（升级下载、公网 IP 探测、住宅探针）也只走 IPv4，但这不是客户端防泄漏的证明。

## 说明

以上 v2rayN 设置项名称来自 v2rayN wiki 与源码模板（2026-09 抓取），本项目**未在 Windows 上实测**，请按实际界面核对；有出入请提 issue。
