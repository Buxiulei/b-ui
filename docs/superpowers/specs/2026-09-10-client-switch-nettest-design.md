# 客户端：TUN 模式切换节点稳定性 + 网络测试双栈出口检测（v3.6.0 追加）

- 日期：2026-09-10
- 目标版本：v3.6.0
- 状态：已批准（主理人口头需求），待实施
- 文件：`b-ui-client.sh`

## 1. 背景

主理人反馈：`bui-c` 开启 TUN 后切换节点"时灵时不灵"；菜单「连接测试」不显示当前出口 IP，需要同时显示 IPv4 与 IPv6 的出口 IP、归属地，以及是否机房 IP（用 ippure.com 判定）。

调研（2026-09-10，代码走读 + 复核）确认的成因与事实：

| # | 事实 | 位置 |
|---|---|---|
| S1 | `hysteria-health.timer` 每 1 分钟跑 `create_health_check` 生成的脚本，发现 `hysteria-client.service` 不活动就 `systemctl restart`。TUN 模式下该服务本该停着（避免与 sing-box 抢 `127.0.0.1:SOCKS_PORT/HTTP_PORT`），timer 却在切换后最多 1 分钟内把它拉起来，与 `bui-tun` 的 `socks-in/http-in` 抢端口。`create_service()` 在每次 hysteria2 切换时都重新 enable/start 该 timer。 | `create_health_check` ~3725-3800（脚本内 `SERVICE="hysteria-client.service"` 硬编码、无 bui-tun 判断）；`create_service` 3679+；`_switch_to_profile` 2358 |
| S2 | `ensure_tun_config_ready()` 的快速路径是死代码：`grep -m1 '"server"'` 命中的是生成 JSON 里排在前面的 `dns.servers[0]`（`1.1.1.1`），永远不等于节点 host → 每次 `start_tun_mode` 都重新生成（含一次同步 `dig`），切换时与 `_switch_to_profile` 的显式生成叠加成两次。v3.6.0 T6 之后出站 `server` 可能是 IP，按 host 比较也不再成立。 | 1740-1830 |
| S3 | `bui-tun.service` 没有 `TimeoutStopSec`；`stop_tun_mode` 的 `systemctl stop bui-tun` 同步阻塞，sing-box 关停慢时整个切换"卡住无反馈"。 | unit 1605-1618；`stop_tun_mode` 1904+ |
| S4 | 三个 unit（bui-tun / hysteria-client / xray-client）之间没有 `Conflicts=`，互斥全靠脚本顺序。 | 全文无 `Conflicts=` |
| S5 | `stop_tun_mode` 末尾无条件重启 `hysteria-client` 并用旧节点端口写 `/etc/profile.d/proxy.sh`；`_switch_to_profile` 紧接着又把它停掉（最多 5 秒端口释放轮询 + pkill）。 | 1946-1958；2323-2337 |
| S6 | `start_tun_mode` 用 `sleep 2` + 一次 `is-active` 判定，无轮询。 | 1888-1897 |
| N1 | 「连接测试」= `test_proxy()`（菜单 5），测试 6 用 `http://ip-api.com` 只查 IPv4；TUN 后自动跑的 `check_public_ip()` 同样只查 IPv4。 | 3998-4266；1655-1728 |
| N2 | ippure.com 站内接口是签名私有 API（`api.123169.xyz`）不可脚本化；**公开接口** `GET https://my.ippure.com/v1/info`（无需 key，实测 200）返回 `ip / asn / asOrganization / country / countryCode / region / city / timezone / fraudScore(0-100) / isResidential(bool) / isBroadcast`；**仅 IPv4**（无 AAAA，`curl -6` 直接失败）；自述测试阶段可能变动。 | https://ippure.com/MyIP-Info-API.html |
| N3 | 免费且无需 key 的机房判定只有 ip-api.com（`hosting/proxy/mobile`，HTTP，IPv4 传输；支持按 IPv6 地址查询 `http://ip-api.com/json/<v6>`）；`api.ipapi.is` 与 `ipinfo.io` 免费层无 datacenter 字段。IPv6 出口地址用 `curl -6 https://api6.ipify.org`（备 `https://ipv6.icanhazip.com`）。 | 实测 |

## 2. 设计

### 2.1 切换稳定性（Task 1）

1. **健康巡检脚本 TUN 感知**：`create_health_check` 生成的脚本在重启前先判断 `systemctl is-active --quiet bui-tun` 为真则记日志 `TUN 模式，跳过 hysteria-client 巡检` 并退出 0。这是首因修复。
2. **systemd 互斥与超时**：`bui-tun.service` 加 `TimeoutStopSec=10` 与 `Conflicts=hysteria-client.service xray-client.service`；`hysteria-client.service`、`xray-client.service` 反向加 `Conflicts=bui-tun.service`。已安装客户端在下次 `create_service`/`start_tun_mode` 重写 unit 时获得（这两处本就重写 unit 文件；若只 `cat >` 不 `daemon-reload`，补上）。
3. **`ensure_tun_config_ready` 快速路径修正**：生成配置时写侧车 `${tun_cfg}.node`（内容 = 当前 active 节点名）；判定改为：文件缺失 / `.schema` 不匹配 / `.node` 不等于 `$active` / 关键字段为空 → 重生成，否则跳过。删除对 JSON 里 `"server"` 的 grep 比较。`_switch_to_profile` 显式生成后 `start_tun_mode` 不再二次生成。
4. **`start_tun_mode` 就绪判定**：`systemctl start bui-tun` 后轮询 `is-active`（最多 10 × 0.5s），成功后再确认 `ip link show bui-tun` 存在；失败时打印 `journalctl -u bui-tun -n 20 --no-pager` 尾部与提示 `BUI_FORCE_IPV6=0` 逃生口（v6 地址配不上时）。
5. **`stop_tun_mode` 增加 `--no-restore` 参数**：`_switch_to_profile` 调用时传入，跳过"重启 hysteria-client + 写 proxy.sh"的收尾；菜单手动关闭 TUN 仍恢复。
6. 不改：`Restart=always`、路由规则、`hysteria-health.timer` 的存在（SOCKS 模式仍需要它）。

### 2.2 网络测试双栈出口（Task 2）

新增函数 `probe_egress(family, socks_port_or_empty)`（family=4|6），输出 `key=value` 行：`ip / country / region / city / org / type / score / source`：
- IPv4：`curl -4 -sL --max-time 8 [--socks5-hostname 127.0.0.1:PORT] https://my.ippure.com/v1/info`；解析（无 jq 依赖，沿用文件里 grep/sed 风格；有 jq 优先用 jq）`ip`、`country`、`region`、`city`、`asOrganization`、`fraudScore`、`isResidential`；`type`：`isResidential=true` → `家庭宽带 IP（住宅）`，否则 `IDC 机房 IP（数据中心）`；`source=ippure`。失败或字段缺失 → 回退 `http://ip-api.com/json/?fields=status,country,regionName,city,isp,org,as,mobile,proxy,hosting,query`（`hosting→IDC 机房 IP`、`proxy→代理 IP`、`mobile→移动网络 IP`、否则 家庭宽带 IP；`source=ip-api`）。
- IPv6：`curl -6 -sS --max-time 6 [--socks5-hostname …] https://api6.ipify.org`（失败再试 `https://ipv6.icanhazip.com`）。拿到地址后归属与类型用 `curl -4 … "http://ip-api.com/json/<ip6>?fields=status,country,regionName,city,isp,org,as,mobile,proxy,hosting"`（IPv4 传输、按地址查）；`source=ip-api`。拿不到地址 → `ip=`（空）。
- 渲染（`test_proxy` 测试 6 与 `check_public_ip` 共用一个 `print_egress_rows` 函数）：
  - IPv4 行：`IPv4 出口: 1.2.3.4  美国·洛杉矶  Cluster Logic  [IDC 机房 IP] 风险分 12  (ippure)`
  - IPv6 行：
    - TUN 模式且拿不到 v6：`IPv6 出口: 已被隧道拦截（无泄漏）✓`
    - TUN 模式拿到 v6：`IPv6 出口: 2001:… 归属…  ⚠ IPv6 泄漏（未进隧道）`
    - SOCKS 模式拿到 v6：`IPv6 出口: 2001:…  归属…  （SOCKS 模式下未走代理的流量使用本机 IPv6）`
    - SOCKS 模式拿不到：`IPv6 出口: 本机无 IPv6`
- `test_proxy` 测试 6 的标题改为「出口 IP 检测 (IPv4 ippure.com / IPv6)」，原 ip-api 逻辑并入回退分支；`check_public_ip` 改为调用同一函数（TUN 分支、无 socks 参数）。
- 超时：每个探测 ≤ 8s，整体测试 6 ≤ 30s。
- 不做：WebRTC/DNS 泄漏检测（ippure 的出口检测页是浏览器专属签名 API，不可脚本化）。

## 3. 验收

1. `bash -n b-ui-client.sh`；抽取测试（沿用 `scratchpad/ipv6-client-tests/gen_client_tun.sh` 的 awk 抽取器）。
2. Task 1：
   - 健康脚本：抽出 `create_health_check` 写出的脚本内容，在 `systemctl` stub（`is-active bui-tun` 返回 0）下运行 → 无 `restart` 调用、日志含"跳过"；stub 返回 1 时行为同旧版。
   - unit 文本：生成的三个 unit 含预期 `Conflicts=`，bui-tun 含 `TimeoutStopSec=10`。
   - `ensure_tun_config_ready`：临时 BASE_DIR 下构造 active 节点 + `singbox-tun.json` + `.schema` + `.node`：三者一致 → 不重生成（用 stub 的 `generate_singbox_tun_config` 记录是否被调用）；`.node` 不同 → 重生成；缺 `.node` → 重生成。
   - `start_tun_mode` 轮询：stub `systemctl` 前 3 次 `is-active` 返回 1、第 4 次返回 0 → 成功；一直返回 1 → 失败并输出 journalctl 提示。
   - `stop_tun_mode --no-restore`：stub 记录无 `start hysteria-client`。
3. Task 2：`probe_egress` 在 PATH 前置 `curl` stub（按 URL/`-4`/`-6` 返回固定 JSON 或失败）下：ippure 成功 → 字段正确、type 住宅/机房正确；ippure 失败 → 回退 ip-api；`-6` 失败 → `ip=` 空；`-6` 成功 → 归属查询用 ip-api 按地址。`print_egress_rows` 四种 IPv6 文案分支各一用例。
4. 主理人线上验收：TUN 模式下连续切换 10 次节点，每次 `bui-c status`/连接测试正常；连接测试显示两行出口信息。
