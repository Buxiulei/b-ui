# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

B-UI is a Hysteria2 + Xray (VLESS-REALITY) proxy server with a built-in Web admin panel, plus a Linux client. Since 4.2.0 REALITY is a **single 443 inbound** (one `vless` tag) with self-steal fallback to a local Caddy (`127.0.0.1:9443`); the direct/residential nodes are distinguished by VLESS `vlessRoute` (UUID bytes 6/7, `0000`/`0001`), not by two ports. Since v4.0.0 the control plane and the client are **Rust single binaries** (`bui` / `bui-c`); the protocol kernels (Xray / hysteria / sing-box / Caddy) are all upstream builds — sing-box included: since 2026-10-10 it ships as the official v1.14.3 release archive (the CI self-build with the `with_v2ray_api` tag that 4.1.x used is retired), and residential HY2 per-user metering now runs on the official built-in service (top-level `services` `api` → `daemon.StartedService` connection events) plus a local ledger in `modules/panel/meter.rs`. It targets Ubuntu/Debian/CentOS/RHEL VPS hosts, amd64 or arm64, systemd. Users are in mainland China; Windows users consume subscriptions with v2rayN, Linux users run `bui-c`. The project, its UI and its commit messages are in Chinese.

v4.0.0 删掉了 v3 的 shell / Node 实现（`server/` 下的五个脚本、Node 面板、根目录那两个 v3 安装与客户端脚本、旧的版本清单 JSON）。它们还在 git 历史里，`crates/` 里那些「移植参照 …:<行号>」注释指的就是历史里的那份代码；要看原文用 `git log --diff-filter=D -1 -- <路径>` 找到删除提交，再 `git show <sha>^:<路径>`。

## Architecture

Cargo workspace，三个 crate + 保留的前端三文件：

1. **`crates/bui-schema`** — 唯一知道端口、标签、规则与格式的地方：期望态模型（`State`）、权益 → 节点集合（`nodes::nodes_for`）、四种内核配置渲染（hysteria ×2 / xray / sing-box relay）、三种订阅渲染、客户端配置渲染、上游 URL 与节点 URI 解析、v3 状态导入。改端口/标签/参数只改这里。公共 API 是总纲 C1 契约，`src/lib.rs` 的模块文档是它的清单。
2. **`crates/bui`** — 服务端单二进制：`install / upgrade / serve / reconcile / status / incidents / import-v3 / set / auth-hook / residential / menu / harden-ssh`。期望态存 `/opt/b-ui/state.json`，对账器把文件/单元/sysctl/防火墙/二进制拉到期望态（非受管项只报不改）；面板 API + 内嵌前端 + 住宅巡检 + 黑名单 + 证书监听 + watchdog + 日志哨兵（spec §5.7，`modules/sentinel`：5 秒增量读受管单元的 journald，按签名表探测 → 按槽借用 / 重试用户同步 / 告警，事件落 `runtime.json` 的 `incidents`，`bui incidents` 查看）+ 升级都在这一个进程里，**没有 cron、没有独立 timer、没有改配置的 shell 脚本**。
3. **`crates/bui-c`** — Linux 客户端：单引擎（sing-box ≤ 1.14）的 SOCKS/TUN，`bui-c.service` + `bui-c.timer`（每分钟 `bui-c check`）。
4. **`web/{index.html,app.js,style.css}`** — 保留的 vanilla SPA，由 `bui` 以 `rust-embed` 内嵌（`modules/panel/assets.rs`）。

引导：`install.sh`（有效代码 ≤150 行，`scripts/tests/test-install.sh` 守门）识别架构 → 从 GitHub Releases（失败则 `BUI_MIRRORS` 的镜像前缀）下载 `manifest.json` 与 `bui` → 校验 sha256 → `exec bui install [--import-v3]`。
发布：`scripts/release/`（`kernel-versions.env` 版本轨道 → `kernels.lock` → `manifest.json`）+ `.github/workflows/{ci,release}.yml`；运维脚本在 `scripts/ops/`，纯 bash 测试在 `scripts/tests/`。详见 `docs/superpowers/plans/2026-09-11-v4-p5-release.md`。

### Server topology

一台直连 Hysteria2 + 每个住宅槽位一台 Hysteria2 + **一个** Xray REALITY 入站（2026-10-30 起），全部由 `bui-schema` 的渲染器产出、由对账器写盘（`i` = 槽序号）：

| Path | Listener | Egress |
|---|---|---|
| `hysteria-server` (`config.yaml`) | `:PORT` (+ built-in port hopping range in the same `listen:` line) | built-in direct, **恒 `mode: 4`**（服务端出站一律 IPv4） |
| `hysteria-residential` (`hy2-residential.json`，**sing-box**，官方发行物、无 `with_v2ray_api`) | `:40000` 单端口；整段 `41000-50000`（+ 兼容段 `40001-40007`）由 `table inet bui` REDIRECT 进来 | 每凭据一个 `gate-<id>` selector → `slot-<i>-out` = `socks 127.0.0.1:(2080+i)`，或 `deny`（到期/封禁） |
| xray `vless` (`xray-config.json`) | `:443` REALITY；`front=l4` 时改为 `127.0.0.1:10443`，公网 443 由前置 haproxy 转进来 | `vlessRoute` 0 → `freedom`（`ForceIPv4` + `AsIs`，首条 `::/0 → block`）；`vlessRoute` 1 → `socks 127.0.0.1:(2080+i)`（每人一条 `user + vlessRoute` 规则，增删走 `RoutingService` gRPC，不重启 xray）；**无住宅权益的用户 route 1 → `block`，通用 route 1 → `block`（4.2.1）**；未认证 → `dest 127.0.0.1:9443` |
| caddy (`Caddyfile`) | `127.0.0.1:9443`（面板/订阅/`sites/*.caddy`；公网 443 的未认证 REALITY 回落经 `xver:1` PROXY 进来）；`table inet bui` 的 `input` 链把非回环访问 9443 drop | 面板 `127.0.0.1:8080`；外部站点由用户目录配置 |

REALITY 自 2026-10-30（4.2.0）起是 **443 单 inbound + self-steal**（spec `docs/superpowers/specs/2026-10-30-reality-443-selfsteal-design.md`）：`reality.dest = 127.0.0.1:9443`、`serverNames = [node.domain]`、`xver: 1`；两个节点（Reality直连 / Reality住宅）共用 host/port，用 VLESS `vlessRoute`（UUID 第 6/7 字节，直连 `0000` / 住宅 `0001`）区分；`email` 仍是 `user_id`，用户同步与计量不变。`node.reality.front` 只有 `public`（Xray 直接听 443，新装机默认）与 `l4`（前置 L4 占 443，Xray 听 `127.0.0.1:10443`；两台生产机用）。**`front=l4` 的机器上 443 还承载另一个项目的国外反代（haproxy L4 → 国内源站，`send-proxy-v2`；rick 的 `table ip nbdpsy_l4` 是入口不是孤儿）——只允许改 haproxy 的 `default_backend` 指向 `127.0.0.1:10443`，NBDpsy 的 SNI 规则、80 端口分流与那张 nft 表一律不动。** `reality.dest` / `server_names` 由 `State::normalize` 固定（`Store::open` 读盘归一并 best-effort 写回），面板 `/api/masquerade` 写入口径已退役（GET 只读、POST 400）；`ports.reality_direct/resi` 只为旧 state 反序列化保留，恒 443。**4.2.1 起每个未停用 Reality 用户都有 per-user route-1 规则：有住宅权益 → 自己的槽，无住宅权益 → `block`；通用 `resi-route`（`vlessRoute:1`）也是 `block`** —— 非住宅用户构造 route 1 UUID 不会借到住宅出口，per-user 规则未收敛的窗口同样被拒绝（fail-closed）。

`127.0.0.1:2080` 是常驻的本地 sing-box 中继（`b-ui-relay.service`，配置 `/opt/b-ui/singbox-relay.json`，由 `render::relay::config` 渲染）：住宅上游池非空时把 AI 域名关键字（global 模式则全部）送进 `resi-pool`，池空则全部直连（fail-open）。目标域名不在本机解析，原样交给上游。上游可以是 `socks5` 或 `http`（`model::UpstreamKind`，粘贴 `socks5://u:p@h:port`、`http://…`、`h:port:u:p`、`u:p@h:port` 四种写法都由 `parse::upstream_url` 归一）；Bright Data 用 HTTP 端口（44445），它的 SOCKS5 端口拒绝明文 HTTP 目标（2026-09-10 实测）。池状态、凭据、黑名单与选中的上游都在 `state.json` 的 `residential` 里（600），没有独立的 `residential-proxy.json`；体检与自动黑名单是守护进程里的任务，切换上游经中继的 Clash API 而不重启内核。

池里每个 IP 是一个槽位（`state.residential.slots`，spec §5.6）：每槽一个中继入站 `2080+i` 与一个住宅出站 `slot-<i>-out`，REALITY 入站按 `user + vlessRoute:1` 路由到槽（无住宅权益 → `block`）、住宅 HY2 按用户的凭据门（`gate-<id>`）路由到槽；用户经 `entitlements.residential.slot_id` 粘在一个 IP 上（多个用户可以共用一个 IP）。**槽位与对外端口无关**（4.1）：住宅 HY2 只有 `40000` 这一个监听端口、整段 `41000-50000` 由 `table inet bui` 送进去，所以**增删槽 / `assign` / `rebalance` 只换出口 IP，不动任何人手里那份订阅**——4.0.x 那套按槽切跳跃段（`slots::slot_span` / `hop_slice`）与「必须重新获取订阅」的名单机制已连签名一起删除，`bui_schema::slots::resources(index)` 现在只算 relay 入站那一个端口。新建用户分到负载最少的槽，`bui residential rebalance` / `assign` 手动调整，`bui residential slots` 与面板按槽展示。

住宅出口策略在 selector 之后执行（`docs/superpowers/specs/2026-09-29-residential-egress-policy-design.md`）：`resi-N` 是回环 SOCKS wrapper，连接 `127.0.0.1:2180..2187` 的 `resi-policy-<UUID>`；自动黑名单与端口能力只作用于这个上游，终点为 `resi-egress-<UUID>` 真实供应商。端口/标签唯一来源为 `bui-schema::relay_policy`。人工 pin 在槽入口全局生效，UDP 先匹配实际出口策略再解析 IPv4。日志以 UUID 归因，新拓扑外层派生错误不参与重复学习/切换；旧拓扑兼容必须以实际 Clash 清单确认。Clash 热切保留旧流；配置内容变更仍会重启 relay，尚无代际排空。

住宅 HY2 的授权落点是**门**而不是配置（spec §3.3）：一池静态凭据（`state.residential.hy2_pool`，`{name}:{secret}`，容量 = `clamp(ceil16(2×住宅 HY2 用户数), 32, 256)`），每条凭据一个 `gate-<id>` selector，用户的全部生命周期动作（建号 / 到期 / 封禁 / 停用 / rotate）都只是一次 `PUT /proxies/gate-<id>`，`hy2-residential.json` 一个字节都不动、内核不重启。到期 / 封禁的语义因此变了：**住宅 HY2 客户端仍显示已连接，但所有请求被拒**（门切到 `deny`，握手照旧成功），直连则在连接时即被拒。不开 sing-box 的 `cache_file` ⇒ 重启后每个门回到 `deny`，由 b-ui 重放真实门位（fail-closed）。「谁算住宅 HY2 用户」的判据只有 `bui_schema::hy2pool::is_resi_hy2` 一处（住宅权益 + 分组真实存在 + 开了 hysteria2）。

受管单元 = `reconcile::MANAGED_UNITS` 那六个固定名字（`b-ui`、`hysteria-server`、`hysteria-residential`、`xray`、`b-ui-relay`、`caddy`），**就是全部**（4.1 起住宅只有一个实例）；枚举一律经 `reconcile::managed_units(&state)`，词法判定用 `reconcile::is_managed_unit`（带后缀的 `hysteria-residential-<i>` 一律 false）。`reconcile::LEGACY_UNITS` 是 16 项：v3 的九个遗留单元与定时器 + 4.0 的 `hysteria-residential-1..7.service`，对账器只负责把它们停掉、删掉；它们留下的端口跳跃 NAT 孤儿由 `portjump::cleanup_legacy_residential` 在落 `inet bui` 之前清（那条路径的下线判据写在它的文档注释里，与那七个名字同生共死）。`Artifact::NftTable` 是第 10 个期望项，`bui nft apply|status|delete` 是它的运维入口。

**服务端出站恒 IPv4，没有开关**（2026-10-27 裁决）：`node.direct_ip_policy` 只为旧 `state.json` 反序列化保留，渲染器不再读它；`bui set direct-ip-policy` 只接受 `ipv4-only`（把旧双栈值写回默认），`dual-stack` 在 CLI 与 API 都当场拒绝。四条代理通路（HY2 直连 / Xray 的直连与住宅两条路由 / relay 直连 / 住宅上游的 VPS→供应商连接与 UDP resolve）一律 IPv4：HY2 `direct.mode: 4`；Xray `ForceIPv4`/`UseIPv4` + `routing.domainStrategy: AsIs`，首条路由把字面 `::/0` 送进 `block` blackhole；relay 路由首条用 logical + invert 只拒绝**全局** IPv6 字面量（`::1`/`fc00::/7`/`fe80::/10` 仍走私网直连），**不要写 `domain_strategy`**（sing-box 1.14.3 判 legacy、`check`/`run` 退出 1），也**不要用 `ip_is_private: false`**（1.14.3 只在 true 时添加匹配项，false 是 no-op）。

主机层由 `table inet bui` 的 `ipv6_egress` 链（filter/output）封死新 IPv6 出站：放行本机地址、链路本地/组播、`ct state established,related` 回包与入站连接需要的 ICMPv6 错误，其余立即 reject；同一张表的 `input` 链把非回环访问 `9443` 的 TCP/UDP drop（Caddy 只服务本机 REALITY 回落，自检/watchdog/`bui nft status` 都校验它）。`b-ui.service` 带 `ExecStartPre=-{bin}/bui nft apply`、五个数据面单元 `After=b-ui.service`，保证开机先落表。管理面同样只走 A：`probe_public_ip` 的 curl `-4`、安装脚本 `curl/wget -4`、`bui` 的 `HttpFetcher` 与 `bui-c` 的 `ReqwestNet` 各带一个只返回 A 记录的 `reqwest::dns::Resolve`、住宅探针 `to_socket_addrs` 过滤 IPv4。

**客户端**接管 IPv6、拒绝全局 IPv6 字面量（`logical ip_version 6 && !ip_is_private ⇒ reject`，私网 v6 仍直连），DNS `ipv4_only`；SOCKS 模式下不走本地代理的应用流量不在保证内，住宅供应商自己的出口地址族也不在本机控制内。设计见 `docs/superpowers/specs/2026-10-27-ipv4-only-egress-design.md`（取代 `2026-09-10-ipv6-takeover-design.md` 里服务端双栈/「VPS 没有 IPv6 出口」的旧前提）。

### Subscriptions (`crates/bui-schema/src/render/subscription.rs`)

四个免鉴权端点（下列三种订阅 + 面板/客户端取节点表的 `/api/nodes/<token>`），节点集合都来自 `nodes::nodes_for`（fusion = Reality直连 / Reality住宅（同 host、都 `:443`，UUID 第 6/7 字节分别为 `0000`/`0001`）/ HY2直连 / HY2住宅 `:40000` + 整段 `mport=41000-50000`，**与槽位无关、每个用户一样**；住宅 HY2 的密码是他那条池凭据 `{name}:{secret}`，不是 `hy2_password`）：
- `/api/sub/<token>` — base64 `vless://`/`hysteria2://` URIs (what v2rayN uses)。端口跳跃（`mport=`）来自期望态的 `ports.hy2_hop` / `hy2_resi_hop`。
- `/api/subscription/<token>` — a complete sing-box config (TUN + DNS + route). Must stay valid for **sing-box 1.12 through 1.14**: typed DNS servers, TUN `address` array, rule actions (`sniff`/`hijack-dns`/`reject`), `route.default_domain_resolver`, no `rule_set`/`download_detour` (1.13 and 1.15 disagree on those fields).
- `/api/clash/<token>` — mihomo YAML.

`<token>` 是每用户一个随机订阅 token（`User.sub_token`，32 位小写十六进制，`bui_schema::sub`），
四个端点同一口径。响应体里有 hy2 明文密码与 vless uuid，**路径末段本身就是凭据**：
公开仓库 + 证书透明日志让「域名 + 用户名」不再是秘密，所以末段改成不可猜、可轮换的随机值
（2026-09-14 裁决）。旧的「用户名链接」只在全局宽限期 `system.legacy_sub_until` 内还认，且该用户
没被轮换过（`User.legacy_sub_disabled`）；全新装机不设宽限期，v3 导入给 7 天
（`sub::LEGACY_SUB_GRACE_DAYS`），`bui set legacy-sub off` 立刻收口。任何认不出的末段一律
404 `{"error":"User not found"}`，不区分「查无此人」与「链接过期」。

节点集合只有 `nodes::nodes_for` 一处实现，三种订阅与客户端渲染都从它取；改端口/标签/obfs 只需改 `bui-schema`。

### 没有迁移块

v4 用对账器取代 v3 的 25 个迁移块：新行为写进渲染器与对账模块，升级时 `reconcile()` 自然把现存安装拉到期望态。给现有安装打补丁不需要写迁移代码。

## Development Commands

```bash
# Rust 侧
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                     # 内核校验用例在 PATH 上找 sing-box / xray / hysteria，缺则 skip

# Shell 侧（install.sh 与 scripts/；git 的 ** 要求中间至少一层目录，所以过滤后缀而不是写 pathspec 通配）
# bash -n 只检查第一个文件（其余都成了它的位置参数），所以逐文件跑；shellcheck 认多文件
rc=0; for f in install.sh $(git ls-files -- scripts | grep -E '\.sh$'); do bash -n "$f" || rc=1; done; [ "$rc" = 0 ]
shellcheck -S error install.sh $(git ls-files -- scripts | grep -E '\.sh$')
bash scripts/tests/run-all.sh

# 发布元数据
bash scripts/release/pin-kernels.sh --check          # 内核 lock 是否还贴着版本轨道
bash scripts/release/check-version.sh v4.0.0         # tag / workspace version / CHANGELOG 一致

# 用真实内核校验渲染结果（本机 1.14.0；CI 只跑发布锁里那一版，当前 sing-box 官方 1.14.3）
sing-box check -c <渲染出的 json>
xray run -test -c <渲染出的 json>
```

## Important Conventions

- 版本号唯一来源是根 `Cargo.toml` 的 `[workspace.package] version`；发布 tag = `v<version>`（预发布 `v<version>-rcN`），同时在 `CHANGELOG.md` 加一段，`scripts/release/check-version.sh` 在 CI 卡一致性。commit 前缀：`fix(scope):` / `feat(scope):` / `chore(scope):` / `docs(scope):` / `test(scope):`。
- 内核版本由 `scripts/release/kernel-versions.env`（轨道）+ `kernels.lock`（精确版本与 sha256）决定；`manifest.json`（总纲 C4：`kernels` 版本表 + 扁平 `artifacts`，每项是裸二进制的 `url` + `sha256`）把它们带给安装与升级，上游归档由 Actions 解包后以裸二进制资产重传。**`kernels.lock` 的每一行都是上游归档的 sha**（sing-box 那两行 2026-10-10 起回到官方发布归档 v1.14.3、不含 `with_v2ray_api`；4.1.x 时期的 `build:` URI 自建链已退役 —— 住宅 HY2 的按用户计量改走官方内核内置 `services` 的 `api` 服务、`daemon.StartedService` 连接事件流 + `modules/panel/meter.rs` 本地账本，不再依赖编译标签）。Xray 的 `/releases/latest` 不可用（全部 tag 标 prerelease），轨道解析走 `git ls-remote --tags`。
- 客户端配置模板改动后无需版本常量：`bui-c` 每次 apply 都重渲染 `config.json` 并按内容比对（`engine::write_if_changed`）决定是否重写 + 重启，不再有 `TUN_SCHEMA_VERSION`。
- Rust 输出与错误：面板/CLI 的用户可见文字是中文；`bui` 的日志经 `logging.rs`，凭据一律经 `redact.rs` 脱敏后才落日志。Shell 脚本的输出 helper 是 `print_info`/`print_success`/`print_warning`/`print_error`；`scripts/` 下的脚本都带 `#!/usr/bin/env bash` + `LC_ALL=C`，采样/压测循环用 `set -uo pipefail`（不用 `set -e`），其余用 `set -euo pipefail`。
- 服务器上的文件：`state.json`（期望态，600；`Store::open` 读盘即把 REALITY 归一为 self-steal 的 `dest`/`serverNames`/端口并 best-effort 写回）、`runtime.json` / `auth-snapshot.json`（运行期，600）、`manifest.json`（升级缓存）、渲染产物 `config.yaml` / `hy2-residential.json`（600）/ `xray-config.json` / `singbox-relay.json` / `Caddyfile`（面板站点在 `127.0.0.1:9443`；`sites/*.caddy` 的外部站点地址必须写 `:9443`，公网 443 由 Xray/haproxy 的未认证回落进来），二进制在 `bin/`，备份在 `state.backups/`；CLI ↔ 守护进程走 `/run/b-ui.sock`。4.0 的 `config-residential.yaml` / `config-residential-<i>.yaml` 已退役（对账产 `Absent`）。Systemd 单元见 `reconcile::MANAGED_UNITS`，防火墙之外还有一张 b-ui 自管的 `table inet bui`。
- Never put credentials on a command line (`ps` leaks them): 面板密码走 `--admin-password-stdin`，住宅上游走 `bui residential add -`（stdin），curl 代理凭据用 `-K -`。
- 公开仓库不写真实域名 / IP / 用户名 / 供应商账号 / 订阅 token（四个订阅端点免鉴权，路径末段就是凭据——2026-09-14 起是随机 `sub_token`，宽限期内也仍认用户名）：示例一律用 `example.com`、`203.0.113.0/24`、`alice` / `bob`，token 用 `0123456789abcdef0123456789abcdef` 这类合成值，供应商账号写 `<decodo-user>` 这类占位符；生产主机只用 SSH 别名（`bwg-tizi` / `bwg-rick`）指代。
- `scripts/tests/` 里的测试禁止访问网络：需要 curl / systemctl / bui 的地方一律用 PATH 前置的 stub。
- Design docs live in `docs/superpowers/specs/YYYY-MM-DD-<slug>-design.md` with matching plans in `docs/superpowers/plans/`；REALITY 443 + self-steal 的设计在 `docs/superpowers/specs/2026-10-30-reality-443-selfsteal-design.md`；v4 的总纲是 `docs/superpowers/plans/2026-09-11-v4-master.md`（C1–C5 契约），架构与验收在 `docs/superpowers/specs/2026-09-11-v4-architecture-design.md`，删除 v3 的依据在 `docs/superpowers/audits/2026-09-11-architecture-audit.md` §7.1。2026-09-10 那一套记录 v3 末期的 IPv6、住宅与重启硬化决定，读它们时注意描述的是 v3 实现。
