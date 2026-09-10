# IPv6 接管 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 客户端 TUN 接管 IPv6 流量且不泄漏、DNS 只返回 A 记录、裸 IPv6 目标就地 reject；服务端所有出站 IPv4-only；`/api/subscription/` 的 sing-box 配置在 1.12 到 1.14 可用并内置上述策略；给 v2rayN 用户文档。

**Architecture:** 三层各改一处生成器：`web/server.js` `generateSingboxConfig()` 重写成现代 sing-box 语法；`b-ui-client.sh` `generate_singbox_tun_config()` 加 v6 地址与 reject 规则；`server/core.sh` + `server/update.sh` 给 Hysteria2/Xray/中继出站加 IPv4-only。验证全部靠真实二进制（`sing-box check`、`xray run -test`）。

**Tech Stack:** bash + jq；Node.js ESM；sing-box 1.13.19（本机 `/usr/bin/sing-box`）与 1.14.0（下载到 scratchpad）；Xray 26.x（下载到 scratchpad）。

**Spec:** `docs/superpowers/specs/2026-09-10-ipv6-takeover-design.md`

## Global Constraints

- 目标 sing-box 版本区间：1.12.0 到 1.14.0（`packages/versions.json` 分发 1.13.11；v2rayN 7.25 锁 ≤1.14）。生成的每份 sing-box 配置都必须在 **1.13.19、1.14.0、1.15.0-alpha.2** 三个二进制上 `sing-box check` 退出 0 且无 `deprecated` 输出（1.15 把 1.14 的弃用项变成致命错误，提前挡住）。
- 不使用 remote rule_set / `download_detour` / `http_client`（1.13 与 1.15 无法同时兼容）；cn 直连用域名后缀列表。
- TUN 地址固定 `["172.19.0.1/30", "fdfe:dcba:9876::1/126"]`；DNS `strategy` 固定 `ipv4_only`；IPv6 reject 规则固定 `{"ip_version": 6, "action": "reject"}` 且位于 `ip_is_private` 之后、所有域名规则之前。
- Xray freedom 用 `settings.domainStrategy: "ForceIPv4"`；Hysteria2 用 `direct: {mode: 4}`。
- 遵守 CLAUDE.md「Surgical Changes」：节点集合/urltest 逻辑不动；不删无关死代码（`singbox-converter` 死 import 留着，提交信息里提一句）。
- 每个 Task 一个提交；Task 1 到 5 用 `feat(ipv6): …` / `fix(sub): …`，最后一个 Task `bump: v3.6.0 …`。提交信息结尾附本会话 Co-Authored-By / Claude-Session 两行。只提交，不 push。
- 测试文件放 scratchpad `/tmp/claude-1000/-home-roots-b-ui/71917ef4-b1f0-4466-927f-5df467756568/scratchpad/ipv6-tests/`，不碰 `/opt/b-ui`，不 `sing-box run`（需要 root/TUN）。
- 在 Task 0 完成前不要开始其它 Task。

---

### Task 0: 测试工具就位（二进制 + 校验脚本）

**Files:**
- Create: `scratchpad/ipv6-tests/bin/`（sing-box 1.14.0、xray 26.x 二进制）
- Create: `scratchpad/ipv6-tests/check_singbox.sh`

**Interfaces:**
- Produces: `check_singbox.sh <config.json>` → 用 1.13.19 与 1.14.0 各跑一次 `sing-box check`，任一失败或 stderr 含 `deprecated` 则退出 1 并打印输出。

- [ ] **Step 1: 下载二进制**

```bash
S=/tmp/claude-1000/-home-roots-b-ui/71917ef4-b1f0-4466-927f-5df467756568/scratchpad/ipv6-tests; mkdir -p "$S/bin" && cd "$S/bin"
# 调研代理已在 scratchpad 下载过 1.14.0 与 1.15.0-alpha.2（find $S/.. -name 'sing-box*' -type f -perm -u+x），有则直接复制为 sing-box-1.14.0 / sing-box-1.15.0-alpha.2，否则下载：
curl -fsSL -o sb.tgz https://github.com/SagerNet/sing-box/releases/download/v1.14.0/sing-box-1.14.0-linux-amd64.tar.gz && tar xzf sb.tgz && mv sing-box-1.14.0-linux-amd64/sing-box ./sing-box-1.14.0 && ./sing-box-1.14.0 version
curl -fsSL -o sb15.tgz https://github.com/SagerNet/sing-box/releases/download/v1.15.0-alpha.2/sing-box-1.15.0-alpha.2-linux-amd64.tar.gz && tar xzf sb15.tgz && mv sing-box-1.15.0-alpha.2-linux-amd64/sing-box ./sing-box-1.15.0-alpha.2 && ./sing-box-1.15.0-alpha.2 version
XV=$(curl -fsSL https://api.github.com/repos/XTLS/Xray-core/releases/latest | jq -r .tag_name)
curl -fsSL -o xray.zip "https://github.com/XTLS/Xray-core/releases/download/${XV}/Xray-linux-64.zip" && unzip -o -q xray.zip xray && ./xray version | head -1
```

若 GitHub 不可达，改用 `packages/` 说明的 CDN 或已缓存文件；实在拿不到 1.14.0 则记录在 Task 报告里，用 1.13.19 单独校验并标注。

- [ ] **Step 2: 写校验脚本**

```bash
cat > "$S/check_singbox.sh" <<'EOF'
#!/bin/bash
# 用法: check_singbox.sh <config.json> ; 两版 sing-box 都过才算过
set -u
S=/tmp/claude-1000/-home-roots-b-ui/71917ef4-b1f0-4466-927f-5df467756568/scratchpad/ipv6-tests
cfg="$1"; rc=0
for bin in /usr/bin/sing-box "$S/bin/sing-box-1.14.0" "$S/bin/sing-box-1.15.0-alpha.2"; do
    [[ -x "$bin" ]] || { echo "SKIP $bin (缺失)"; continue; }
    out=$("$bin" check -c "$cfg" 2>&1); r=$?
    if [[ $r -ne 0 ]] || grep -qi 'deprecated' <<<"$out"; then
        echo "FAIL [$($bin version | head -1)] $cfg"; echo "$out"; rc=1
    else
        echo "OK   [$($bin version | head -1)] $cfg"
    fi
done
exit $rc
EOF
chmod +x "$S/check_singbox.sh"
```

- [ ] **Step 3: 自检脚本本身**

```bash
printf '{"inbounds":[{"type":"tun","inet4_address":"172.19.0.1/30"}],"outbounds":[{"type":"direct"}]}' > "$S/legacy.json"
"$S/check_singbox.sh" "$S/legacy.json"; echo "rc=$?"
```

Expected: 两版都 FAIL（legacy tun address），`rc=1`。这一步确认脚本能抓住旧字段。无需提交。

---

### Task 1: 重写 `web/server.js` `generateSingboxConfig()`

**Files:**
- Modify: `web/server.js:452-637`（`generateSingboxConfig`）
- Create: `scratchpad/ipv6-tests/run_server.sh`、`scratchpad/ipv6-tests/test_subscription.sh`

**Interfaces:**
- Consumes: `getResidentialConfig()`（住宅计划 Task 1 后 `domains` 已含默认回退；若该计划尚未执行，本测试用显式 `domains` 数组）。
- Produces: `/api/subscription/<user>` JSON，结构见 spec §3.1。

- [ ] **Step 1: 写本地起服务脚本与测试（预期失败）**

`run_server.sh`（与住宅计划的同名脚本等价，可复用）：

```bash
#!/bin/bash
# 用法: run_server.sh <workdir> [hop:on|off] [resi:null|list|global]
set -u
REPO=/home/roots/b-ui
W="$1"; HOP="${2:-hop:on}"; RESI="${3:-resi:list}"; mkdir -p "$W"
cp "$REPO/server/residential-helper.sh" "$W/"; chmod +x "$W/residential-helper.sh"
cat > "$W/users.json" <<'EOF'
[{"username":"alice","password":"pw","uuid":"11111111-2222-3333-4444-555555555555","sni":"www.bing.com","protocol":"fusion","residential":true,"createdAt":"2026-09-10T00:00:00Z","limits":{}},
 {"username":"bob","password":"pw2","protocol":"hysteria2","residential":false,"createdAt":"2026-09-10T00:00:00Z","limits":{}}]
EOF
if [[ "$HOP" == "hop:on" ]]; then printf 'listen: :10000,20000-30000\n' > "$W/config.yaml"; echo '{"enabled":true,"start":20000,"end":30000}' > "$W/port-hopping.json"
else printf 'listen: :10000\n' > "$W/config.yaml"; fi
echo '{"privateKey":"priv","publicKey":"pubkeyXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX","shortId":"abcd1234"}' > "$W/reality-keys.json"
echo '{"inbounds":[{"tag":"vless-direct","port":10001,"protocol":"vless","streamSettings":{"realitySettings":{"serverNames":["www.bing.com"],"shortIds":["abcd1234"]}}}]}' > "$W/xray-config.json"
case "$RESI" in
  resi:null)   echo '{"enabled":true,"global":false,"domains":null,"urls":[{"host":"1.2.3.4","port":1080,"username":"u","password":"p","name":"url-1"}]}' ;;
  resi:global) echo '{"enabled":true,"global":true,"domains":["openai"],"urls":[{"host":"1.2.3.4","port":1080,"username":"u","password":"p","name":"url-1"}]}' ;;
  *)           echo '{"enabled":true,"global":false,"domains":["openai","anthropic"],"urls":[{"host":"1.2.3.4","port":1080,"username":"u","password":"p","name":"url-1"}]}' ;;
esac > "$W/residential-proxy.json"
echo '{"domain":"proxy.example.com","port":10000}' > "$W/config.json" 2>/dev/null || true
cd "$REPO/web" && BASE_DIR="$W" ADMIN_DIR="$REPO/web" ADMIN_PORT=18081 SERVER_IP=203.0.113.10 node server.js > "$W/server.log" 2>&1 &
echo $! > "$W/pid"; sleep 1.5
```

`getConfig()`（`web/server.js:341-433`）决定 `cfg.domain/port/pubKey/shortId/portHopping/obfs` 从哪些文件读；先读一遍它，把上面样例文件的字段名/路径对齐，目标是 alice 得到 4 个节点且 hy2-direct 带端口跳跃。

`test_subscription.sh`：

```bash
#!/bin/bash
set -u
S=/tmp/claude-1000/-home-roots-b-ui/71917ef4-b1f0-4466-927f-5df467756568/scratchpad/ipv6-tests
fail=0
for combo in "hop:on resi:list" "hop:off resi:null" "hop:on resi:global"; do
  set -- $combo; W="$S/w_${1#hop:}_${2#resi:}"; rm -rf "$W"
  bash "$S/run_server.sh" "$W" "$1" "$2"
  for u in alice bob; do
    curl -s "http://127.0.0.1:18081/api/subscription/$u" > "$W/$u.json"
    jq -e . "$W/$u.json" >/dev/null || { echo "FAIL $combo $u 非 JSON"; fail=1; continue; }
    "$S/check_singbox.sh" "$W/$u.json" || fail=1
    # 结构断言
    jq -e '.dns.strategy=="ipv4_only"' "$W/$u.json" >/dev/null || { echo "FAIL $combo $u strategy"; fail=1; }
    jq -e '.inbounds[]|select(.type=="tun")|.address==["172.19.0.1/30","fdfe:dcba:9876::1/126"]' "$W/$u.json" >/dev/null || { echo "FAIL $combo $u tun address"; fail=1; }
    jq -e '[.route.rules[]|select(.ip_version==6 and .action=="reject")]|length==1' "$W/$u.json" >/dev/null || { echo "FAIL $combo $u v6 reject"; fail=1; }
    jq -e '(.route.rules|map(.ip_is_private==true)|index(true)) < (.route.rules|map(.ip_version==6)|index(true))' "$W/$u.json" >/dev/null || { echo "FAIL $combo $u 规则顺序"; fail=1; }
    jq -e '.route.default_domain_resolver=="local"' "$W/$u.json" >/dev/null || { echo "FAIL $combo $u resolver"; fail=1; }
    jq -e '[.outbounds[]|select(.type=="block" or .type=="dns")]|length==0' "$W/$u.json" >/dev/null || { echo "FAIL $combo $u 特殊出站残留"; fail=1; }
    jq -e '[.. | objects | select(has("inet4_address") or has("geoip") or has("geosite") or has("hop_ports") or has("rule_set") or has("download_detour") or has("http_client"))]|length==0' "$W/$u.json" >/dev/null || { echo "FAIL $combo $u 旧字段/规则集残留"; fail=1; }
    jq -e '[.route.rules[]|select(.domain_suffix and .outbound=="direct")]|length==1' "$W/$u.json" >/dev/null || { echo "FAIL $combo $u cn 直连后缀规则"; fail=1; }
  done
  # alice fusion: 4 节点 + 两个 urltest 池；hop:on 时 hy2-direct 有 server_ports
  jq -e '[.outbounds[]|select(.type=="hysteria2" or .type=="vless")]|length==4' "$W/alice.json" >/dev/null || { echo "FAIL $combo alice 节点数"; fail=1; }
  [[ "$1" == "hop:on" ]] && { jq -e '.outbounds[]|select(.tag=="hy2-direct")|.server_ports==["20000:30000"]' "$W/alice.json" >/dev/null || { echo "FAIL $combo server_ports"; fail=1; }; }
  [[ "$2" == "resi:list" ]] && { jq -e '.route.rules[]|select(.domain_keyword)|.outbound=="residential-pool"' "$W/alice.json" >/dev/null || { echo "FAIL $combo 住宅规则"; fail=1; }; }
  [[ "$2" == "resi:global" ]] && { jq -e '.route.final=="residential-pool"' "$W/alice.json" >/dev/null || { echo "FAIL $combo global final"; fail=1; }; }
  kill "$(cat "$W/pid")" 2>/dev/null
done
[[ $fail == 0 ]] && echo "PASS subscription" || exit 1
```

- [ ] **Step 2: 运行，确认失败**

Run: `bash $S/test_subscription.sh`
Expected: `check_singbox.sh` 对现有输出 FAIL（legacy 字段），结构断言全 FAIL。

- [ ] **Step 3: 重写函数输出骨架**

保留 `resi/proto/includeResi/userSni/hasVless/hasHy2`、`mkHy2/mkVless`（`mkHy2` 里 `hop_ports: \`${hopStart}-${hopEnd}\`` 改为 `server_ports: [\`${hopStart}:${hopEnd}\`]`，`hop_interval` 保留）、节点集合分支、`urltest`、`hasSplit/primaryTag/routeFinal` 逻辑。把 `routeRules` 初始化与 `return` 改为：

```js
    const serverIp = getServerIP();
    const hostIsDomain = !/^\d+\.\d+\.\d+\.\d+$/.test(host);
    const predefined = (hostIsDomain && /^\d+\.\d+\.\d+\.\d+$/.test(serverIp) && serverIp !== "127.0.0.1")
        ? [{ domain: [host], action: "predefined", answer: [`${host}. IN A ${serverIp}`] }] : [];

    // 顺序：sniff → DNS 劫持 → 私网直连 → IPv6 reject（服务端无 v6 出口，RST 让应用回退 v4）→ 住宅关键字 → cn 直连
    const routeRules = [
        { action: "sniff" },
        { protocol: "dns", action: "hijack-dns" },
        { ip_is_private: true, outbound: "direct" },
        { ip_version: 6, action: "reject" }
    ];
    // …hasSplit 分支里原来的 routeRules.push({ domain_keyword: resi.domains, outbound: "residential-pool" }) 位置不变…
    routeRules.push({ domain_suffix: CN_DIRECT_SUFFIXES, outbound: "direct" });

    outbounds.push({ type: "direct", tag: "direct" });

    return {
        log: { level: "info", timestamp: true },
        experimental: {
            clash_api: { external_controller: "127.0.0.1:9090", external_ui: "", secret: "", default_mode: "rule" },
            cache_file: { enabled: true, path: "cache.db" }
        },
        dns: {
            servers: [
                { tag: "remote", type: "https", server: "8.8.8.8", detour: primaryTag },
                { tag: "local", type: "udp", server: "223.5.5.5" }
            ],
            rules: [
                ...predefined,
                { domain_suffix: CN_DIRECT_SUFFIXES, server: "local" }
            ],
            final: "remote",
            strategy: "ipv4_only"
        },
        inbounds: [
            { type: "mixed", tag: "mixed-in", listen: "127.0.0.1", listen_port: 7890 },
            { type: "tun", tag: "tun-in", interface_name: "bui-tun",
              address: ["172.19.0.1/30", "fdfe:dcba:9876::1/126"], auto_route: true, strict_route: true }
        ],
        outbounds,
        route: {
            rules: routeRules,
            final: routeFinal,
            auto_detect_interface: true,
            default_domain_resolver: "local"
        }
    };
```

在 `web/server.js` 顶部（`CONFIG` 之后）加常量 `CN_DIRECT_SUFFIXES`，内容逐字复制 `b-ui-client.sh` 约 1508 行 route 规则里的 `domain_suffix` 数组（`.qq.com` … `.cn`，约 40 项），注释 `// v3.6.0: 国内域名直连后缀，与 b-ui-client.sh TUN 模板同源；不用 remote rule_set（1.13/1.15 字段不兼容）`。

注意 `routeRules.push(domain_keyword…)` 必须在 `domain_suffix cn 直连` 之前（住宅关键字优先级高于 cn 直连，与旧版 geoip 在前的顺序不同——旧版 cn 规则在住宅规则前，但 cn 域名不会命中 AI 关键字，两种顺序等价；新顺序让关键字命中更早短路）。把函数顶部注释更新为 `v3.6.0: sing-box 1.12-1.14 语法 + IPv6 接管(ipv4_only + v6 reject)`。删除 `outbounds.push({ type: "block"…})`、`{ type: "dns"…}` 两行。

- [ ] **Step 4: 运行测试，确认通过**

Run: `node --check web/server.js && bash $S/test_subscription.sh`
Expected: 三组合 × 两用户全部 `OK [1.13.19]`、`OK [1.14.0]`，`PASS subscription`。若 1.14.0 报 `deprecated`，按提示修改直到无警告。

- [ ] **Step 5: 提交**

```bash
git add web/server.js
git commit -m "fix(sub): sing-box 订阅生成器升级 1.12-1.14 语法并内置 IPv6 接管(ipv4_only + v6 reject + cn 后缀直连)"
```

---

### Task 2: `b-ui-client.sh` TUN 模板 IPv6 接管

**Files:**
- Modify: `b-ui-client.sh:1345`（`TUN_SCHEMA_VERSION`）、`1347-1530`（`generate_singbox_tun_config`）、`3070-3080`（`import_from_subscription` sing-box 分支）
- Create: `scratchpad/ipv6-tests/gen_client_tun.sh`、`scratchpad/ipv6-tests/test_client_tun.sh`

**Interfaces:**
- Produces: bash 函数 `host_ipv6_enabled()`（返回 0 表示主机 IPv6 可用），环境变量 `BUI_FORCE_IPV6=0|1` 覆盖检测（测试与排障用）。

- [ ] **Step 1: 写隔离生成器与测试（预期失败）**

`gen_client_tun.sh`：从脚本里抽出需要的函数在临时 `BASE_DIR` 下生成配置，不执行主流程。

```bash
#!/bin/bash
# 用法: gen_client_tun.sh <protocol hysteria2|vless-reality> <out.json> <BUI_FORCE_IPV6 0|1>
set -u
REPO=/home/roots/b-ui
proto="$1"; out="$2"; export BUI_FORCE_IPV6="$3"
T=$(mktemp -d); export BASE_DIR="$T"
# 抽取函数：颜色/print_*/json_escape/host_ipv6_enabled/generate_singbox_tun_config
extract() { sed -n "/^$1() {/,/^}/p" "$REPO/b-ui-client.sh"; }
{
  sed -n '1,120p' "$REPO/b-ui-client.sh" | grep -E '^(RED|GREEN|YELLOW|BLUE|CYAN|NC|BASE_DIR)=' ; echo 'BASE_DIR="'"$T"'"'
  for f in print_info print_success print_warning print_error json_escape host_ipv6_enabled generate_singbox_tun_config; do extract "$f"; done
  grep -m1 '^readonly TUN_SCHEMA_VERSION' "$REPO/b-ui-client.sh"
} > "$T/lib.sh"
export SERVER_ADDR="proxy.example.com:443" AUTH_PASSWORD="alice:pw" SNI="proxy.example.com" MPORT="20000-30000" INSECURE=false
export UUID="11111111-2222-3333-4444-555555555555" PUBLIC_KEY="pubkeyXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX" SHORT_ID="abcd1234" FLOW="xtls-rprx-vision"
export SOCKS_PORT=1080 HTTP_PORT=8080
# 不联网：dig/getent 预解析失败时函数只是不写 predefined 规则
( source "$T/lib.sh"; generate_singbox_tun_config "$proto" >/dev/null 2>&1 )
cp "$T/singbox-tun.json" "$out" 2>/dev/null || { echo "生成失败"; cat "$T"/*.tmp* 2>/dev/null; exit 1; }
rm -rf "$T"
```

若 `generate_singbox_tun_config` 依赖别的函数（如 `print_*` 之外的辅助），按报错把函数名加进 `for f in …` 列表。

`test_client_tun.sh`：

```bash
#!/bin/bash
set -u
S=/tmp/claude-1000/-home-roots-b-ui/71917ef4-b1f0-4466-927f-5df467756568/scratchpad/ipv6-tests
fail=0
for proto in hysteria2 vless-reality; do
  for v6 in 1 0; do
    out="$S/client_${proto}_v6${v6}.json"
    bash "$S/gen_client_tun.sh" "$proto" "$out" "$v6" || { echo "FAIL gen $proto v6=$v6"; fail=1; continue; }
    "$S/check_singbox.sh" "$out" || fail=1
    jq -e '.dns.strategy=="ipv4_only"' "$out" >/dev/null || { echo "FAIL $proto strategy"; fail=1; }
    jq -e '[.route.rules[]|select(.ip_version==6 and .action=="reject")]|length==1' "$out" >/dev/null || { echo "FAIL $proto v6 reject"; fail=1; }
    jq -e '(.route.rules|map(.ip_is_private==true)|index(true)) < (.route.rules|map(.ip_version==6)|index(true))' "$out" >/dev/null || { echo "FAIL $proto 顺序"; fail=1; }
    jq -e '(.route.rules|map(.ip_version==6)|index(true)) < (.route.rules|map(has("domain_suffix"))|index(true))' "$out" >/dev/null || { echo "FAIL $proto v6 reject 需在域名规则前"; fail=1; }
    if [[ $v6 == 1 ]]; then
      jq -e '.inbounds[]|select(.type=="tun")|.address==["172.19.0.1/30","fdfe:dcba:9876::1/126"]' "$out" >/dev/null || { echo "FAIL $proto v6 地址"; fail=1; }
    else
      jq -e '.inbounds[]|select(.type=="tun")|.address==["172.19.0.1/30"]' "$out" >/dev/null || { echo "FAIL $proto v4-only 地址"; fail=1; }
    fi
  done
done
grep -q '^readonly TUN_SCHEMA_VERSION="7"' /home/roots/b-ui/b-ui-client.sh || { echo "FAIL schema version"; fail=1; }
[[ $fail == 0 ]] && echo "PASS client tun" || exit 1
```

- [ ] **Step 2: 运行，确认失败**

Run: `bash $S/test_client_tun.sh`
Expected: `host_ipv6_enabled` 不存在 → gen 失败；或生成但 v6 地址/reject/schema 断言 FAIL。

- [ ] **Step 3: 改脚本**

`TUN_SCHEMA_VERSION="6"` → `"7"`；其上方注释块（约 1330-1344）追加一行 `# v7 (v3.6.0): TUN 加 IPv6 地址接管 ::/0 + ip_version 6 reject，服务端无 v6 出口`。

在 `generate_singbox_tun_config()` 之前加：

```bash
# v3.6.0: 主机 IPv6 是否可用。可用 → TUN 加 v6 地址接管 ::/0；不可用 → 保持 IPv4-only（无 v6 则无泄漏，
# 且 disable_ipv6=1 的主机给 TUN 配 v6 地址会失败）。BUI_FORCE_IPV6=0|1 可强制（排障/测试）。
host_ipv6_enabled() {
    case "${BUI_FORCE_IPV6:-}" in 1) return 0 ;; 0) return 1 ;; esac
    # 1. 内核 IPv6 栈存在（ipv6.disable=1 启动参数会让它消失）
    [[ -f /proc/net/if_inet6 ]] || return 1
    # 2. sysctl 未禁用：all 管现有接口，default 决定新建的 TUN 接口能否配 v6 地址
    [[ "$(sysctl -n net.ipv6.conf.all.disable_ipv6 2>/dev/null)" == "1" ]] && return 1
    [[ "$(sysctl -n net.ipv6.conf.default.disable_ipv6 2>/dev/null)" == "1" ]] && return 1
    # 3. 有 v6 默认路由
    local dev
    dev=$(ip -6 route show default 2>/dev/null | awk '{for(i=1;i<=NF;i++) if ($i=="dev"){print $(i+1); exit}}')
    [[ -n "$dev" ]] || return 1
    # 4. 该接口有 2000::/3 全球单播地址（排除只有 ULA 的 Docker 网桥等）
    ip -6 addr show dev "$dev" scope global 2>/dev/null | grep -qE 'inet6 [23]'
}
```

依据 spec §3.2：sing-tun 加 v6 地址失败即整个 TUN 起不来，所以四条全满足才加。

函数内、`cat > "$tmp_config" <<EOF` 之前加：

```bash
    local tun_address='["172.19.0.1/30"]'
    if host_ipv6_enabled; then
        tun_address='["172.19.0.1/30", "fdfe:dcba:9876::1/126"]'
        print_info "主机 IPv6 可用：TUN 同时接管 IPv6（服务端 IPv4 出站，裸 v6 目标就地 reject）"
    fi
```

heredoc 里 `"address": ["172.19.0.1/30"],` → `"address": ${tun_address},`。

路由规则：`{ "ip_is_private": true, "outbound": "direct-out" },` 之后插入一行 `{ "ip_version": 6, "action": "reject" },`。

`import_from_subscription()` 的 `cp "$sub_file" "${BASE_DIR}/singbox-tun.json"` 改为：

```bash
                # v3.6.0: 服务端订阅默认带 IPv6 TUN 地址；本机 IPv6 不可用时剔除，避免 sing-box 起不来
                if ! host_ipv6_enabled && command -v jq >/dev/null 2>&1; then
                    jq '(.inbounds[] | select(.type=="tun") | .address) |= map(select(contains(":") | not))' \
                        "$sub_file" > "${BASE_DIR}/singbox-tun.json.tmp" && mv "${BASE_DIR}/singbox-tun.json.tmp" "${BASE_DIR}/singbox-tun.json" \
                        || cp "$sub_file" "${BASE_DIR}/singbox-tun.json"
                elif ! host_ipv6_enabled; then
                    print_warning "本机 IPv6 不可用且缺 jq，订阅里的 IPv6 TUN 地址未剔除；若 TUN 起不来请安装 jq 后重导"
                    cp "$sub_file" "${BASE_DIR}/singbox-tun.json"
                else
                    cp "$sub_file" "${BASE_DIR}/singbox-tun.json"
                fi
```

- [ ] **Step 4: 运行测试与静态检查**

Run: `bash -n b-ui-client.sh && bash $S/test_client_tun.sh && (command -v shellcheck && shellcheck -S error b-ui-client.sh || true)`
Expected: `PASS client tun`，4 份配置两版 sing-box 全 OK。

- [ ] **Step 5: 提交**

```bash
git add b-ui-client.sh
git commit -m "feat(ipv6): 客户端 TUN 接管 IPv6(v6 地址 + ip_version 6 reject),schema v7,订阅导入按主机 v6 可用性剔除"
```

---

### Task 3: 服务端出站 IPv4-only（core.sh 新装 + update.sh 迁移 + 中继）

**Files:**
- Modify: `server/core.sh:536-583`（`config.yaml` heredoc）、`590-645`（`config-residential.yaml` 的 direct 出站）、`818`（xray freedom）
- Modify: `server/update.sh`（D8 块之后、`# v3.4.19 D2` 之前插入 D9）
- Modify: `server/residential-helper.sh:32`（`PRIVATE_CIDRS`）、两处 `"strategy": "prefer_ipv4"`
- Create: `scratchpad/ipv6-tests/test_server_egress.sh`

**Interfaces:**
- Produces: `config.yaml` 末尾 `outbounds` 块；`config-residential.yaml` direct 出站含 `direct.mode: 4`；`xray-config.json` direct 出站 `settings.domainStrategy=ForceIPv4`；`update.sh` D9 幂等。

- [ ] **Step 1: 写测试（预期失败）**

```bash
cat > "$S/test_server_egress.sh" <<'EOF'
#!/bin/bash
set -u
S=/tmp/claude-1000/-home-roots-b-ui/71917ef4-b1f0-4466-927f-5df467756568/scratchpad/ipv6-tests
REPO=/home/roots/b-ui; fail=0; T=$(mktemp -d)
# --- A. core.sh 生成：抽 configure_hysteria / configure_xray 在临时目录跑
mkdir -p "$T/bin" "$T/base/certs"
for c in systemctl openssl; do printf '#!/bin/bash\nexit 0\n' > "$T/bin/$c"; chmod +x "$T/bin/$c"; done
export PATH="$T/bin:$PATH"
extract() { sed -n "/^$1() {/,/^}/p" "$REPO/server/core.sh"; }
{ grep -E '^(RED|GREEN|YELLOW|BLUE|NC)=' "$REPO/server/core.sh" | head -5
  echo "BASE_DIR='$T/base'; CERTS_DIR='$T/base/certs'; CONFIG_FILE='$T/base/config.yaml'; USERS_FILE='$T/base/users.json'"
  echo "PORT=10000; PORT_HOPPING_ENABLED=y; PORT_HOPPING_START=20000; PORT_HOPPING_END=30000; MASQUERADE_URL=https://www.bing.com; FIRST_USER=a; FIRST_USER_PASS=p; FIRST_USER_UUID=11111111-2222-3333-4444-555555555555; FIRST_USER_SNI=www.bing.com"
  for f in print_info print_success print_warning print_error configure_hysteria configure_xray; do extract "$f"; done
} > "$T/lib.sh"
echo '{"privateKey":"priv","publicKey":"pub","shortId":"abcd1234"}' > "$T/base/reality-keys.json"
( source "$T/lib.sh"; configure_hysteria >/dev/null 2>&1; configure_xray >/dev/null 2>&1 )
python3 - "$T/base" <<'PY' || fail=1
import sys, yaml, json
b=sys.argv[1]
d=yaml.safe_load(open(f"{b}/config.yaml")); assert d["outbounds"][0]["name"]=="direct" and d["outbounds"][0]["direct"]["mode"]==4, "config.yaml mode"
r=yaml.safe_load(open(f"{b}/config-residential.yaml")); ob={o["name"]:o for o in r["outbounds"]}; assert ob["direct"]["direct"]["mode"]==4, "resi mode"; assert ob["relay"]["type"]=="socks5"
x=json.load(open(f"{b}/xray-config.json")); assert [o for o in x["outbounds"] if o["tag"]=="direct"][0]["settings"]["domainStrategy"]=="ForceIPv4", "xray"
print("core.sh OK")
PY
"$S/bin/xray" run -test -c "$T/base/xray-config.json" >/dev/null 2>&1 || { echo "FAIL xray -test"; "$S/bin/xray" run -test -c "$T/base/xray-config.json" 2>&1 | tail -3; fail=1; }

# --- B. update.sh D9 幂等：用 v3.5.23 风格样例（无 outbounds / 无 mode / 无 domainStrategy）
mkdir -p "$T/up"; cd "$T/up"
printf 'listen: :10000\nquic:\n  maxIdleTimeout: 60s\nresolver:\n  type: https\n' > config.yaml
printf 'listen: :40000,41000-50000\noutbounds:\n  - name: relay\n    type: socks5\n    socks5:\n      addr: "127.0.0.1:2080"\n  - name: direct\n    type: direct\n\nacl:\n  inline:\n    - relay(all)\n' > config-residential.yaml
echo '{"outbounds":[{"tag":"direct","protocol":"freedom"},{"tag":"relay","protocol":"socks","settings":{"servers":[{"address":"127.0.0.1","port":2080}]}}],"routing":{"rules":[]}}' > xray-config.json
extract_u() { sed -n "/^$1() {/,/^}/p" "$REPO/server/update.sh"; }
{ grep -E '^(RED|GREEN|YELLOW|BLUE|NC)=' "$REPO/server/update.sh" | head -5; echo "BASE_DIR='$T/up'"
  for f in print_info print_success print_warning print_error migrate_ipv4_only_egress; do extract_u "$f"; done; } > "$T/ulib.sh"
( source "$T/ulib.sh"; migrate_ipv4_only_egress ) > "$T/run1.log" 2>&1
cp config.yaml c1.yaml; cp config-residential.yaml r1.yaml; cp xray-config.json x1.json
( source "$T/ulib.sh"; migrate_ipv4_only_egress ) > "$T/run2.log" 2>&1
cmp -s config.yaml c1.yaml && cmp -s config-residential.yaml r1.yaml && cmp -s xray-config.json x1.json || { echo "FAIL D9 不幂等"; fail=1; }
python3 - "$T/up" <<'PY' || fail=1
import sys, yaml, json
b=sys.argv[1]
d=yaml.safe_load(open(f"{b}/config.yaml")); assert d["outbounds"][0]["direct"]["mode"]==4, "D9 config.yaml"
r=yaml.safe_load(open(f"{b}/config-residential.yaml")); assert [o for o in r["outbounds"] if o["name"]=="direct"][0]["direct"]["mode"]==4, "D9 resi"; assert r["acl"]["inline"]==["relay(all)"]
x=json.load(open(f"{b}/xray-config.json")); assert x["outbounds"][0]["settings"]["domainStrategy"]=="ForceIPv4", "D9 xray"
print("D9 OK")
PY
grep -q 'restart hysteria-server' "$T/run1.log" && ! grep -q 'restart' "$T/run2.log" || echo "注意：检查 run1/run2 日志里的重启记录: $(cat "$T/run1.log" "$T/run2.log")"

# --- C. residential-helper 中继：ipv4_only + v6 私网
mkdir -p "$T/rh"; echo '{"enabled":true,"global":false,"domains":null,"urls":[{"host":"1.2.3.4","port":1080,"username":"u","password":"p","name":"url-1"},{"host":"1.2.3.5","port":1080,"username":"u","password":"p","name":"url-2"}]}' > "$T/rh/residential-proxy.json"
printf '#!/bin/bash\necho sing-box version 1.13.19\n' > "$T/rh/sing-box"; chmod +x "$T/rh/sing-box"
BASE_DIR="$T/rh" bash "$REPO/server/residential-helper.sh" reapply >/dev/null 2>&1
jq -e '.dns.strategy=="ipv4_only"' "$T/rh/singbox-relay.json" >/dev/null || { echo "FAIL relay strategy"; fail=1; }
jq -e '.route.rules[]|select(.ip_cidr)|.ip_cidr|index("fc00::/7") and index("::1/128") and index("fe80::/10")' "$T/rh/singbox-relay.json" >/dev/null || { echo "FAIL relay v6 cidr"; fail=1; }
"$S/check_singbox.sh" "$T/rh/singbox-relay.json" || fail=1
rm -rf "$T"
[[ $fail == 0 ]] && echo "PASS server egress" || exit 1
EOF
chmod +x "$S/check_singbox.sh" "$S/test_server_egress.sh"
```

`reapply` 会调 `get_server_ip`（curl 外网）与 `start_relay_service`（systemctl）；测试里 `PATH` 前置 stub 的 systemctl，`get_server_ip` 失败时 `_SERVER_IP` 为空只是不加 `/32` 规则。python3 需要 `yaml` 模块（`python3 -c 'import yaml'`，缺失则 `pip install --user pyyaml` 或改用 `yq`）。`configure_hysteria` 后半段（auth 转 userpass 等）依赖的其它函数按报错补进 extract 列表，或只测 heredoc 段：目标是断言两份 YAML 的 outbounds。

- [ ] **Step 2: 运行，确认失败**

Run: `bash $S/test_server_egress.sh`
Expected: A/B/C 全 FAIL（无 outbounds、无 D9 函数、relay 仍 prefer_ipv4）。

- [ ] **Step 3: core.sh**

`config.yaml` heredoc 的 `sniff:` 段之后（`EOF` 前）追加：

```yaml

# v3.6.0: 出站只走 IPv4（VPS 无 IPv6 出口；mode 4 = 只拨 IPv4，解析不到 A 记录即失败）
outbounds:
  - name: direct
    type: direct
    direct:
      mode: 4
```

注释 `# 配置 1：config.yaml — hysteria-direct (无 outbounds 块，hy2 内置 direct)` 改为 `(outbounds: direct mode 4，IPv4-only)`。同时检查 `update.sh:413-495` `repair_hysteria_config()` 与 `435` 附近注释"永远内置直连，不写 outbounds"——该函数若会**删除** `outbounds` 块或以缺失 `outbounds` 判定损坏，需同步调整为允许且仅允许上述 direct 块（读它的实现再决定；A.fix2 的 marker 清理只匹配 `# B-UI:RESIDENTIAL-START`，不受影响）。

`config-residential.yaml`：

```yaml
  - name: direct
    type: direct
    direct:
      mode: 4
```

`configure_xray()`：`{"tag": "direct", "protocol": "freedom"},` → `{"tag": "direct", "protocol": "freedom", "settings": {"domainStrategy": "ForceIPv4"}},`。

- [ ] **Step 4: update.sh D9**

新增函数（放在 `migrate_relay_log()` 之后），并在 D8 块之后调用 `migrate_ipv4_only_egress`：

```bash
# v3.6.0 D9: 服务端出站 IPv4-only（VPS 无 IPv6 出口，v6 目标拨号必失败）
#   config.yaml           缺 outbounds → 追加 direct(mode 4)
#   config-residential.yaml direct 出站缺 mode → 补 direct.mode: 4
#   xray-config.json      direct freedom 缺 domainStrategy → ForceIPv4
# 幂等：每项只在缺失时改；改前备份 .bak.v360.<ts>
migrate_ipv4_only_egress() {
    local ts; ts=$(date +%s)
    local cfg="${BASE_DIR}/config.yaml" rcfg="${BASE_DIR}/config-residential.yaml" xcfg="${BASE_DIR}/xray-config.json"

    if [[ -f "$cfg" ]] && ! grep -qE '^outbounds:' "$cfg"; then
        cp "$cfg" "${cfg}.bak.v360.${ts}"
        printf '\n# v3.6.0: 出站只走 IPv4（VPS 无 IPv6 出口）\noutbounds:\n  - name: direct\n    type: direct\n    direct:\n      mode: 4\n' >> "$cfg"
        systemctl restart hysteria-server 2>/dev/null || true
        print_success "  ✓ D9 config.yaml 出站 IPv4-only (direct mode 4)"
        updated=1
    fi

    if [[ -f "$rcfg" ]] && grep -qE '^\s+- name: direct$' "$rcfg" && ! grep -qE '^\s+mode: 4$' "$rcfg"; then
        cp "$rcfg" "${rcfg}.bak.v360.${ts}"
        # 在 "- name: direct" 后的 "type: direct" 行后插入两行
        awk '
          /^[[:space:]]+- name: direct$/ { in_direct=1 }
          in_direct && /^[[:space:]]+type: direct$/ { print; print "    direct:"; print "      mode: 4"; in_direct=0; next }
          { print }' "$rcfg" > "${rcfg}.tmp" && mv "${rcfg}.tmp" "$rcfg" && chmod 644 "$rcfg"
        systemctl restart hysteria-residential 2>/dev/null || true
        print_success "  ✓ D9 config-residential.yaml direct 出站 IPv4-only"
        updated=1
    fi

    if [[ -f "$xcfg" ]] && command -v jq >/dev/null 2>&1 && \
       [[ "$(jq -r '.outbounds[]?|select(.tag=="direct")|.settings.domainStrategy // ""' "$xcfg" 2>/dev/null)" != "ForceIPv4" ]]; then
        cp "$xcfg" "${xcfg}.bak.v360.${ts}"
        if jq '.outbounds |= map(if .tag=="direct" and .protocol=="freedom" then .settings = ((.settings // {}) + {domainStrategy:"ForceIPv4"}) else . end)' \
              "$xcfg" > "${xcfg}.tmp" 2>/dev/null && [[ -s "${xcfg}.tmp" ]]; then
            mv "${xcfg}.tmp" "$xcfg" && chmod 644 "$xcfg"
            systemctl restart xray 2>/dev/null || true
            print_success "  ✓ D9 xray direct 出站 ForceIPv4"
            updated=1
        else
            rm -f "${xcfg}.tmp"; print_warning "  D9 xray-config.json jq 改写失败，保留原文件"
        fi
    fi
}
```

`updated` 是 `apply_systemd_configs()` 里的局部变量；若 D9 以独立函数调用，改为函数内 `local`/返回码，并在调用处 `migrate_ipv4_only_egress && updated=1` 之类保持与既有 D 块一致（读 D8 的写法照抄其风格）。

- [ ] **Step 5: residential-helper.sh**

`PRIVATE_CIDRS='["127.0.0.0/8","10.0.0.0/8","172.16.0.0/12","192.168.0.0/16","169.254.0.0/16","::1/128","fc00::/7","fe80::/10"]'`；两处 `"strategy": "prefer_ipv4"` → `"strategy": "ipv4_only"`，旁注 `# v3.6.0: VPS 无 v6 出口`。

- [ ] **Step 6: 运行测试**

Run: `bash -n server/core.sh server/update.sh server/residential-helper.sh && bash $S/test_server_egress.sh && bash $S/test_subscription.sh`
Expected: `core.sh OK`、`D9 OK`、`PASS server egress`；订阅测试仍 PASS（relay 改动不影响）。

- [ ] **Step 7: 提交**

```bash
git add server/core.sh server/update.sh server/residential-helper.sh
git commit -m "feat(ipv6): 服务端出站 IPv4-only(hy2 direct mode 4 / xray ForceIPv4 / 中继 ipv4_only+v6 私网),update.sh D9 幂等迁移"
```

---

### Task 4: v2rayN 文档 + README 链接

**Files:**
- Create: `docs/v2rayn-tun-ipv6.md`
- Modify: `README.md`（客户端段落，约 118-125 行附近加一行）

- [ ] **Step 1: 写文档**

内容按 spec §3.4 五节。要点必须包含（措辞可调）：

```markdown
# v2rayN TUN 模式 IPv6 设置（服务端仅 IPv4 出口）

## 为什么
B-UI 服务器没有 IPv6 出口。TUN 模式下若本机 IPv6 没被接管，访问支持 IPv6 的网站会直接从本地网络走 v6（泄漏真实地址）；若被接管但服务器拨 v6 失败，就会看到连接失败。目标：IPv6 全部进隧道、DNS 只要 A 记录、极少数裸 IPv6 目标在本机直接拒绝让应用回退 IPv4。

## 方案 A：改 v2rayN 设置（推荐）
1. 升级 v2rayN 到 7.24.9 或更新（7.24.7 起"无论是否启用 IPv6 地址，始终将 IPv6 路由到 TUN"）。
2. 设置 → Tun 模式设置：严格路由 开；协议栈 mixed。
3. 设置 → DNS 设置：直连目标解析策略 / 代理目标解析策略 = UseIPv4。
   - 注意：sing-box 内核下 UseIPv4 对应 `prefer_ipv4`（软偏好）。Hysteria2 节点走 sing-box 内核，若测试站仍显示 IPv6，把 sing-box DNS 模板里 `"strategy"` 改为 `"ipv4_only"`：
     ```json
     { "servers": [ { "tag": "remote", "type": "tcp", "server": "8.8.8.8", "detour": "proxy" },
                    { "tag": "local", "type": "udp", "server": "223.5.5.5" } ],
       "rules": [ { "rule_set": ["geosite-cn"], "server": "local" } ],
       "final": "remote", "strategy": "ipv4_only" }
     ```
4. 验证：https://test-ipv6.com 应显示"无 IPv6"；https://ipleak.net 无 v6 地址，DNS 只出现代理出口。

## 方案 B：导入完整 sing-box 配置
把 `https://<你的域名>/api/subscription/<用户名>` 作为 sing-box 自定义配置订阅导入 v2rayN。这份配置已内置 TUN（v4+v6 地址）、`ipv4_only`、IPv6 reject、cn 直连规则集，也可直接给独立 sing-box 使用。

## 已知限制
- 只有 IPv6 地址的网站在隧道开启时无法访问（服务器无 v6 出口）。
- 浏览器自带 DoH 仍可能解析到 IPv6，连接会被本机立即拒绝并自动回退 IPv4，属预期。

## 说明
以上 v2rayN 设置项名称来自 v2rayN wiki 与源码模板（2026-09 抓取），本项目未在 Windows 上实测，请按实际界面核对；有出入请提 issue。
```

- [ ] **Step 2: README 加链接**

在"客户端 (Client)"段（约 122 行）下加一行：`- 🪟 **v2rayN TUN + IPv6**：服务端无 IPv6 出口时的推荐设置见 [docs/v2rayn-tun-ipv6.md](docs/v2rayn-tun-ipv6.md)`。

- [ ] **Step 3: 检查并提交**

```bash
test -s docs/v2rayn-tun-ipv6.md && grep -q 'v2rayn-tun-ipv6.md' README.md
git add docs/v2rayn-tun-ipv6.md README.md
git commit -m "docs: v2rayN TUN 模式 IPv6 设置指南(服务端仅 IPv4 出口)"
```

---

### Task 5: 版本 bump v3.6.0 + changelog + 全量回归

**Files:**
- Modify: `version.json`（`version`、`updated`、`changelog["3.6.0"]`）

- [ ] **Step 1: 改 version.json**

`"version": "3.6.0"`，`"updated": "2026-09-10"`，`changelog` 最前面加 `"3.6.0"` 条目，格式照 `3.5.23` 条目（先读一遍它的字段形状），内容要点：

- IPv6 接管：客户端 TUN 加 v6 地址 + `ip_version 6` reject（schema v7）；`/api/subscription` sing-box 配置升级到 1.12-1.14 语法（去掉 inet4_address/legacy DNS/geoip/geosite/block/dns/hop_ports）并内置 `ipv4_only` + v6 reject + cn 域名后缀直连（不用 remote rule_set）；服务端出站 IPv4-only（hy2 direct mode 4、xray ForceIPv4、中继 ipv4_only + v6 私网 CIDR），update.sh D9 幂等迁移；v2rayN 文档。
- 住宅：订阅域名回退与中继一致（`residential-helper.sh domains`）；relay 配置 flock + 原子写 + 巡检重启冷却 10 分钟；SOCKS5 凭据不再出现在 curl 命令行。
- 备注：`singbox-converter` 为未使用依赖（本次未删）。

- [ ] **Step 2: 全量回归**

```bash
cd /home/roots/b-ui
bash -n install.sh server/core.sh server/b-ui-cli.sh server/update.sh server/residential-helper.sh server/resi-health.sh b-ui-client.sh
node --check web/server.js
node -e 'JSON.parse(require("fs").readFileSync("version.json","utf8"))'
grep -c '"3.6.0"' version.json
S=/tmp/claude-1000/-home-roots-b-ui/71917ef4-b1f0-4466-927f-5df467756568/scratchpad
bash $S/ipv6-tests/test_subscription.sh && bash $S/ipv6-tests/test_client_tun.sh && bash $S/ipv6-tests/test_server_egress.sh
bash $S/resi-tests/test_domains.sh && bash $S/resi-tests/test_lock_cooldown.sh && bash $S/resi-tests/test_creds.sh
git status --short   # 只应有 version.json
```

Expected: 全部 PASS。

- [ ] **Step 3: 提交**

```bash
git add version.json
git commit -m "bump: v3.6.0 IPv6 接管 + sing-box 订阅生成器 1.14 适配 + 服务端出站 IPv4-only + 住宅硬化"
git log --oneline -8
```
