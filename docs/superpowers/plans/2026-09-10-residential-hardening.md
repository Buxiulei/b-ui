# 住宅代理硬化 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修三个住宅代理链路问题：订阅域名回退与服务端一致、relay 配置加锁/原子写/巡检重启冷却、SOCKS5 凭据不再出现在 curl 命令行。

**Architecture:** 默认域名列表只保留在 `server/residential-helper.sh`，其它读者通过新子命令 `domains` 取值；两脚本对 `singbox-relay.json` 的写路径共享一把 `flock`；三处 curl 改成 `-K -` 从 stdin 读 `proxy`/`proxy-user`。

**Tech Stack:** bash + jq + flock（util-linux）+ curl；Node.js ESM（`web/server.js`，无框架）。

**Spec:** `docs/superpowers/specs/2026-09-10-residential-hardening-design.md`

## Global Constraints

- 只动 spec 列出的文件：`server/residential-helper.sh`、`server/resi-health.sh`、`web/server.js`。不改 `version.json`（版本随 IPv6 计划最后统一 bump）。
- 遵守 CLAUDE.md「Surgical Changes」：不顺手重构、不删既有死代码（`LEGACY_DEFAULT_DOMAINS_V3_4_17` 留着）。
- 每个 Task 一个独立提交，提交信息格式 `fix(residential): <描述>`，结尾附本会话的 Co-Authored-By / Claude-Session 两行（见 system-reminder）。只提交，不 push。
- 所有 bash 改动通过 `bash -n`；`web/server.js` 通过 `node --check`。
- 测试在 scratchpad `/tmp/claude-1000/-home-roots-b-ui/71917ef4-b1f0-4466-927f-5df467756568/scratchpad/` 下建临时目录，不碰 `/opt/b-ui`。脚本通过 `BASE_DIR` 环境变量重定向（helper 支持 `BASE_DIR="${BASE_DIR:-/opt/b-ui}"`；`resi-health.sh` 的路径是硬编码 `/opt/b-ui/...`，Task 2 会把它们改成可用环境变量覆盖）。

---

### Task 1: R1 订阅域名回退与服务端一致

**Files:**
- Modify: `server/residential-helper.sh`（`usage` 行约 566；`case "$cmd"` 分发约 431-570）
- Modify: `web/server.js:435-450`（`getResidentialConfig`）、`web/server.js:1899-1927`（`/api/residential` GET 的 `DEFAULT_DOMAINS`）

**Interfaces:**
- Produces: `residential-helper.sh domains` → stdout 一行 JSON 数组（生效域名关键字），退出 0。
- Produces: `getEffectiveResidentialDomains(): string[]`（server.js 内部函数）。

- [ ] **Step 1: 写 helper 子命令测试脚本（先跑，预期失败）**

创建 `scratchpad/resi-tests/test_domains.sh`：

```bash
#!/bin/bash
set -u
REPO=/home/roots/b-ui
T=$(mktemp -d)
HELPER="$REPO/server/residential-helper.sh"
fail=0
run() { BASE_DIR="$T" bash "$HELPER" domains 2>/dev/null; }

# case 1: 无配置文件 → 默认列表(31 条)
rm -f "$T/residential-proxy.json"
n=$(run | jq 'length'); [[ "$n" == "31" ]] || { echo "FAIL case1 got $n"; fail=1; }
# case 2: domains: null → 默认
echo '{"enabled":true,"global":false,"domains":null,"urls":[]}' > "$T/residential-proxy.json"
n=$(run | jq 'length'); [[ "$n" == "31" ]] || { echo "FAIL case2 got $n"; fail=1; }
# case 3: domains: [] → 默认
echo '{"enabled":true,"domains":[]}' > "$T/residential-proxy.json"
n=$(run | jq 'length'); [[ "$n" == "31" ]] || { echo "FAIL case3 got $n"; fail=1; }
# case 4: 自定义
echo '{"enabled":true,"domains":["foo","bar"]}' > "$T/residential-proxy.json"
out=$(run); [[ "$out" == '["bar","foo"]' || "$out" == '["foo","bar"]' ]] || { echo "FAIL case4 got $out"; fail=1; }
rm -rf "$T"
[[ $fail == 0 ]] && echo "PASS domains" || exit 1
```

- [ ] **Step 2: 运行，确认失败**

Run: `bash scratchpad/resi-tests/test_domains.sh`
Expected: 全部 FAIL（`domains` 未知子命令，退出 1，输出空）。

- [ ] **Step 3: 在 helper 加 `domains` 子命令**

在 `case "$cmd" in` 的 `status)` 分支后加：

```bash
    domains)
        # v3.6.0 R1: 输出生效域名关键字（自定义优先，否则 DEFAULT_DOMAINS）
        # server.js 订阅生成器与面板显示都从这里取，默认列表只此一处
        get_domains
        printf '%s\n' "${DOMAINS[@]}" | jq -R . | jq -sc 'unique'
        ;;
```

`usage` 行改为：`{setup|enable <url>|disable|status|domains|reapply|set-domains <json>|global on|off}`。

- [ ] **Step 4: 运行测试，确认通过**

Run: `bash scratchpad/resi-tests/test_domains.sh && bash -n server/residential-helper.sh`
Expected: `PASS domains`。

- [ ] **Step 5: server.js 加 `getEffectiveResidentialDomains()` 并接入两处**

在 `getResidentialConfig()` 前加：

```js
// v3.6.0 R1: 生效域名关键字以 residential-helper.sh 为唯一真源（含默认回退）
function getEffectiveResidentialDomains() {
    try {
        if (!fs.existsSync(CONFIG.residentialHelper)) return [];
        const r = spawnSync(CONFIG.residentialHelper, ["domains"], { encoding: "utf8", timeout: 3000 });
        if (r.status !== 0 || !r.stdout) return [];
        const arr = JSON.parse(r.stdout.trim());
        return Array.isArray(arr) ? arr.filter(x => typeof x === "string" && x) : [];
    } catch { return []; }
}
```

`getResidentialConfig()` 的 `domains` 行改为：

```js
                domains: (Array.isArray(r.domains) && r.domains.length > 0) ? r.domains : getEffectiveResidentialDomains()
```

且末尾 `return { enabled: false, global: false, domains: [] };` 保持（文件不存在视为未启用，不需要默认列表）。

`/api/residential` GET 处理：删除 `const DEFAULT_DOMAINS = [...]` 一行；`display.domains = DEFAULT_DOMAINS` → `display.domains = getEffectiveResidentialDomains()`；catch 分支 `{ enabled: false, domains: DEFAULT_DOMAINS }` → `{ enabled: false, domains: getEffectiveResidentialDomains() }`。确认文件内不再有 `DEFAULT_DOMAINS` 引用（`grep -n DEFAULT_DOMAINS web/server.js` 为空）。

- [ ] **Step 6: 本地起 server.js 验证订阅出现默认关键字规则**

创建 `scratchpad/resi-tests/run_server.sh`（Task 3 也复用）：

```bash
#!/bin/bash
# 用法: run_server.sh <workdir>  → 起 server.js 于 127.0.0.1:18080，PID 写 <workdir>/pid
set -u
REPO=/home/roots/b-ui
W="$1"; mkdir -p "$W"
cp "$REPO/server/residential-helper.sh" "$W/residential-helper.sh"; chmod +x "$W/residential-helper.sh"
cat > "$W/users.json" <<'EOF'
[{"username":"alice","password":"pw","uuid":"11111111-2222-3333-4444-555555555555","sni":"www.bing.com","protocol":"fusion","residential":true,"createdAt":"2026-09-10T00:00:00Z","limits":{}}]
EOF
cat > "$W/config.yaml" <<'EOF'
listen: :10000,20000-30000
tls:
  cert: /x/fullchain.pem
  key: /x/privkey.pem
EOF
cat > "$W/reality-keys.json" <<'EOF'
{"privateKey":"priv","publicKey":"pubkeyXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX","shortId":"abcd1234"}
EOF
cat > "$W/xray-config.json" <<'EOF'
{"inbounds":[{"tag":"vless-direct","port":10001,"protocol":"vless","streamSettings":{"realitySettings":{"serverNames":["www.bing.com"],"shortIds":["abcd1234"]}}}]}
EOF
echo '{"enabled":true,"global":false,"domains":null,"urls":[{"host":"1.2.3.4","port":1080,"username":"u","password":"p","name":"url-1"}]}' > "$W/residential-proxy.json"
cd "$REPO/web" && BASE_DIR="$W" ADMIN_DIR="$REPO/web" ADMIN_PORT=18080 SERVER_IP=203.0.113.10 \
  node server.js > "$W/server.log" 2>&1 &
echo $! > "$W/pid"; sleep 1.5
```

注意：`server.js` 读取 `config.yaml`/`reality-keys.json`/`xray-config.json` 的具体字段以 `getConfig()`（`web/server.js:341-433`）为准，若上面样例缺字段导致 `cfg.pubKey`/`cfg.shortId` 为空，按 `getConfig()` 的读取路径补齐样例文件，目标是 `/api/subscription/alice` 含 4 个节点。

验证：

```bash
W=scratchpad/resi-tests/w1; bash scratchpad/resi-tests/run_server.sh "$W"
curl -s http://127.0.0.1:18080/api/subscription/alice | jq '[.route.rules[] | select(.domain_keyword) | .domain_keyword | length]'
curl -s http://127.0.0.1:18080/api/clash/alice | grep -c 'DOMAIN-KEYWORD'
kill "$(cat $W/pid)"
```

Expected: 第一条输出 `[31]`；第二条 ≥ 1（改前两者都是 0/空）。

- [ ] **Step 7: 静态检查并提交**

```bash
bash -n server/residential-helper.sh && node --check web/server.js
git add server/residential-helper.sh web/server.js
git commit -m "fix(residential): 订阅域名回退与服务端中继一致(helper 新增 domains 子命令)"
```

---

### Task 2: R2 加锁 + 原子写 + 巡检重启冷却

**Files:**
- Modify: `server/residential-helper.sh`（顶部常量区约 25-31；`save_config` 325-349；`set-domains`/`global` 分支的直写路径；主入口 `case` 前加锁）
- Modify: `server/resi-health.sh`（路径常量 12-14；写段 64-79）

**Interfaces:**
- Produces: 锁文件 `${BASE_DIR}/.relay.lock`；`resi-health.sh` 新环境变量 `RESI_HEALTH_RESTART_COOLDOWN`（秒，默认 600）、`RESI_HEALTH_BASE_DIR`（默认 `/opt/b-ui`，测试用）；状态文件新键 `_last_restart`。

- [ ] **Step 1: 写测试脚本（预期失败）**

创建 `scratchpad/resi-tests/test_lock_cooldown.sh`：

```bash
#!/bin/bash
set -u
REPO=/home/roots/b-ui
T=$(mktemp -d); mkdir -p "$T/bin"
HELPER="$REPO/server/residential-helper.sh"; HEALTH="$REPO/server/resi-health.sh"
fail=0
# systemctl stub：记录调用
cat > "$T/bin/systemctl" <<'EOF'
#!/bin/bash
echo "$@" >> "${STUB_LOG}"; exit 0
EOF
chmod +x "$T/bin/systemctl"
export STUB_LOG="$T/systemctl.log"; export PATH="$T/bin:$PATH"

# --- A. save_config 原子写：执行后无 .tmp 残留、权限 600
echo '{"enabled":false,"global":false,"urls":[]}' > "$T/residential-proxy.json"
BASE_DIR="$T" bash "$HELPER" global on >/dev/null 2>&1
ls "$T"/*.tmp 2>/dev/null && { echo "FAIL A tmp 残留"; fail=1; }
[[ "$(stat -c %a "$T/residential-proxy.json")" == "600" ]] || { echo "FAIL A perm"; fail=1; }

# --- B. 互斥：持锁时第二个 set-domains 必须等待
exec 9>"$T/.relay.lock"; flock 9
( BASE_DIR="$T" timeout 5 bash "$HELPER" set-domains '["a"]' >/dev/null 2>&1; echo $? > "$T/rc" ) &
sleep 1.5
[[ -f "$T/rc" ]] && { echo "FAIL B 没等锁就执行完了"; fail=1; }
flock -u 9; wait
[[ "$(cat "$T/rc")" == "0" ]] || { echo "FAIL B rc=$(cat "$T/rc")"; fail=1; }
[[ "$(jq -c .domains "$T/residential-proxy.json")" == '["a"]' ]] || { echo "FAIL B 内容"; fail=1; }

# --- C. resi-health 冷却：池需变化但 _last_restart 很近 → 不重启不改文件
cat > "$T/singbox-relay.json" <<'EOF'
{"outbounds":[
 {"type":"socks","tag":"resi-1","server":"127.0.0.1","server_port":1,"username":"u","password":"p"},
 {"type":"socks","tag":"resi-2","server":"127.0.0.1","server_port":2,"username":"u","password":"p"},
 {"type":"urltest","tag":"resi-pool","outbounds":["resi-1","resi-2"]},
 {"type":"direct","tag":"direct"}]}
EOF
now=$(date +%s)
# resi-1 failstreak 1→2 被剔除，resi-2 0→1 保留 → 期望池 [resi-2] ≠ 当前 [resi-1,resi-2] → 触发变更判断 → 冷却
echo "{\"resi-1\":{\"active\":true,\"failstreak\":1,\"okstreak\":0},\"resi-2\":{\"active\":true,\"failstreak\":0,\"okstreak\":0},\"_last_restart\":$now}" > "$T/.resi-health-state.json"
: > "$STUB_LOG"
RESI_HEALTH_BASE_DIR="$T" RESI_HEALTH_TRIES=1 RESI_HEALTH_OK_NEED=1 RESI_HEALTH_TIMEOUT=1 \
  RESI_HEALTH_FAIL_TO_REMOVE=2 RESI_HEALTH_LOG="$T/health.log" bash "$HEALTH"
grep -q 'restart' "$STUB_LOG" && { echo "FAIL C 冷却期内重启了"; fail=1; }
grep -q '冷却' "$T/health.log" || { echo "FAIL C 无冷却日志"; fail=1; }
[[ "$(jq -c '.outbounds[2].outbounds' "$T/singbox-relay.json")" == '["resi-1","resi-2"]' ]] || { echo "FAIL C 文件被改"; fail=1; }

# --- D. 冷却过期(_last_restart=0) → 同样的状态：resi-1 被剔除、resi-2 保留 → 改文件 + 重启 + 写 _last_restart
echo '{"resi-1":{"active":true,"failstreak":1,"okstreak":0},"resi-2":{"active":true,"failstreak":0,"okstreak":0},"_last_restart":0}' > "$T/.resi-health-state.json"
: > "$STUB_LOG"
RESI_HEALTH_BASE_DIR="$T" RESI_HEALTH_TRIES=1 RESI_HEALTH_OK_NEED=1 RESI_HEALTH_TIMEOUT=1 \
  RESI_HEALTH_FAIL_TO_REMOVE=2 RESI_HEALTH_LOG="$T/health.log" bash "$HEALTH"
grep -q 'restart b-ui-relay' "$STUB_LOG" || { echo "FAIL D 未重启"; fail=1; }
[[ "$(jq -c '.outbounds[2].outbounds' "$T/singbox-relay.json")" == '["resi-2"]' ]] || { echo "FAIL D 池未更新: $(jq -c '.outbounds[2].outbounds' "$T/singbox-relay.json")"; fail=1; }
lr=$(jq -r '._last_restart' "$T/.resi-health-state.json"); [[ "$lr" -gt 1000000 ]] || { echo "FAIL D _last_restart=$lr"; fail=1; }

rm -rf "$T"
[[ $fail == 0 ]] && echo "PASS lock/cooldown" || exit 1
```

- [ ] **Step 2: 运行，确认失败**

Run: `bash scratchpad/resi-tests/test_lock_cooldown.sh`
Expected: B、C、D 失败（无锁、无冷却、`RESI_HEALTH_BASE_DIR` 不生效）。

- [ ] **Step 3: helper 加锁与原子写**

常量区（`RELAY_SERVICE=` 之后）加：

```bash
RELAY_LOCK="${BASE_DIR}/.relay.lock"

# v3.6.0 R2: 写路径互斥（与 resi-health.sh 共用同一把锁）；持锁到进程退出
acquire_relay_lock() {
    exec 9>"${RELAY_LOCK}"
    chmod 600 "${RELAY_LOCK}" 2>/dev/null || true
    flock -w 30 9 || { err "获取 relay 锁超时(30s)，可能有另一个 residential-helper/resi-health 在运行"; exit 1; }
}
```

主入口 `cmd="${1:-}"` 之后、`case` 之前加：

```bash
case "$cmd" in
    setup|enable|disable|reapply|set-domains|global) acquire_relay_lock ;;
esac
```

`save_config()`：`> "${RESIDENTIAL_CONFIG}"` 改为 `> "${RESIDENTIAL_CONFIG}.tmp" && chmod 600 "${RESIDENTIAL_CONFIG}.tmp" && mv "${RESIDENTIAL_CONFIG}.tmp" "${RESIDENTIAL_CONFIG}"`，原 `chmod 600 "${RESIDENTIAL_CONFIG}"` 保留无害。

`set-domains` 与 `global` 分支里"文件不存在"的 `jq -n ... > "${RESIDENTIAL_CONFIG}"` 同样改成写 `.tmp` + `chmod 600` + `mv`。`add_url_to_config` 里 `echo '{...}' > "${RESIDENTIAL_CONFIG}"` 同样处理。

- [ ] **Step 4: resi-health.sh 路径可覆盖 + 持锁写 + 冷却**

顶部常量改为：

```bash
BASE="${RESI_HEALTH_BASE_DIR:-/opt/b-ui}"
RELAY="$BASE/singbox-relay.json"
STATE="$BASE/.resi-health-state.json"
LOCK="$BASE/.relay.lock"
LOG="${RESI_HEALTH_LOG:-/var/log/b-ui-resi-health.log}"
RESTART_COOLDOWN="${RESI_HEALTH_RESTART_COOLDOWN:-600}"
```

把从 `des=$(printf ...)` 到 `fi`（含 restart）的整段改为：

```bash
des=$(printf '%s\n' "${desired[@]}" | sort -u | jq -R . | jq -cs .)

# v3.6.0 R2: 只在读-改-写-重启这一段持锁（探测循环不持锁），持锁后重读当前池，避免用陈旧快照覆盖管理员刚做的修改
exec 9>"$LOCK"
if ! flock -w 30 9; then log "WARN 获取 relay 锁超时，本轮跳过"; exit 0; fi
cur=$(jq -c '[.outbounds[]|select(.type=="urltest" and .tag=="resi-pool").outbounds[]]|sort' "$RELAY" 2>/dev/null)
if [ "$des" != "$cur" ]; then
    now=$(date +%s)
    last=$(jq -r '._last_restart // 0' "$STATE" 2>/dev/null); last=${last:-0}
    if [ $((now - last)) -lt "$RESTART_COOLDOWN" ]; then
        log "住宅池需变更 ${cur} → ${des}，重启冷却中（剩余 $((RESTART_COOLDOWN - now + last))s），延后"
    else
        log "住宅池变化: ${cur} → ${des}"
        if [ "$DRY_RUN" != "1" ]; then
            jq --argjson d "$des" '.outbounds |= map(if (.type=="urltest" and .tag=="resi-pool") then (.outbounds=$d) else . end)' \
               "$RELAY" > "${RELAY}.tmp" 2>/dev/null && mv "${RELAY}.tmp" "$RELAY" \
               && systemctl restart b-ui-relay 2>/dev/null \
               && jq --argjson t "$now" '._last_restart=$t' "$STATE" > "${STATE}.tmp" 2>/dev/null && mv "${STATE}.tmp" "$STATE" \
               && log "已更新 singbox-relay.json + 重启 b-ui-relay（住宅池=$(IFS=,; echo "${desired[*]}")）"
        fi
    fi
fi
flock -u 9
```

`jq --arg n "$tag" ... '.[$n]={...}'` 的按 tag 写法不受 `_last_restart` 影响，无需改。头部注释补一句"重启有冷却（默认 10 分钟，`RESI_HEALTH_RESTART_COOLDOWN` 可调）"。

- [ ] **Step 5: 运行测试，确认通过**

Run: `bash scratchpad/resi-tests/test_lock_cooldown.sh && bash -n server/residential-helper.sh server/resi-health.sh && bash scratchpad/resi-tests/test_domains.sh`
Expected: `PASS lock/cooldown`、`PASS domains`。

- [ ] **Step 6: 提交**

```bash
git add server/residential-helper.sh server/resi-health.sh
git commit -m "fix(residential): singbox-relay.json 加锁 + 原子写 + 巡检重启冷却(默认10min)"
```

---

### Task 3: R3 SOCKS5 凭据不再出现在 curl 命令行

**Files:**
- Modify: `server/residential-helper.sh:95-118`（`verify()`）
- Modify: `server/resi-health.sh`（探测循环 `curl --socks5-hostname` 处）
- Modify: `web/server.js:2150-2165`（`residential/health` 的 `execFile("curl", …)`）

**Interfaces:**
- Produces: bash 函数 `curl_cfg_escape()`（两脚本各自内联一份）；JS 函数 `curlCfgEscape(s)`；curl 统一以 `-K -` 读取 `proxy = "socks5h://HOST:PORT"` 与 `proxy-user = "USER:PASS"`。

- [ ] **Step 1: 写测试（预期失败）**

创建 `scratchpad/resi-tests/test_creds.sh`：

```bash
#!/bin/bash
set -u
REPO=/home/roots/b-ui
T=$(mktemp -d); mkdir -p "$T/bin"
fail=0
# curl stub：追加记录每次调用的 argv 与 stdin（verify() 会连调三次 curl：ipify 直连、ipify 经 socks、ipinfo）
# 经 socks(-K -) 的调用返回不同 IP，让 verify() 的"出口 IP ≠ VPS IP"检查通过
cat > "$T/bin/curl" <<'EOF'
#!/bin/bash
{ echo "--- call"; printf '%s\n' "$@"; } >> "${STUB_DIR}/argv"
if printf '%s\n' "$@" | grep -qx -- '-K'; then
    { echo "--- call"; cat; } >> "${STUB_DIR}/stdin"
    echo "198.51.100.7"
else
    echo "203.0.113.1"
fi
EOF
chmod +x "$T/bin/curl"
cat > "$T/bin/systemctl" <<'EOF'
#!/bin/bash
exit 0
EOF
chmod +x "$T/bin/systemctl"
export STUB_DIR="$T"; export PATH="$T/bin:$PATH"

# A. resi-health.sh 探测：argv 无密码，stdin 有 proxy-user
cat > "$T/singbox-relay.json" <<'EOF'
{"outbounds":[
 {"type":"socks","tag":"resi-1","server":"h1","server_port":1080,"username":"user1","password":"se\"cr\\et"},
 {"type":"socks","tag":"resi-2","server":"h2","server_port":1081,"username":"user2","password":"p2"},
 {"type":"urltest","tag":"resi-pool","outbounds":["resi-1","resi-2"]},{"type":"direct","tag":"direct"}]}
EOF
echo '{}' > "$T/.resi-health-state.json"
RESI_HEALTH_BASE_DIR="$T" RESI_HEALTH_TRIES=1 RESI_HEALTH_LOG="$T/h.log" bash "$REPO/server/resi-health.sh"
grep -q 'p2' "$T/argv" && { echo "FAIL A 密码在 argv"; fail=1; }
grep -q 'cr' "$T/argv" && { echo "FAIL A 密码1在 argv"; fail=1; }
grep -q '^-K$' "$T/argv" || { echo "FAIL A 未用 -K"; fail=1; }
grep -q 'proxy-user = "user2:p2"' "$T/stdin" || { echo "FAIL A stdin: $(cat "$T/stdin")"; fail=1; }
grep -q 'proxy = "socks5h://h2:1081"' "$T/stdin" || { echo "FAIL A proxy 行"; fail=1; }

# B. helper verify()：通过 enable --add 触发；ensure_singbox 会下载 → 用 BASE_DIR/sing-box 预置跳过
#    （若 helper 顶部有 set -e，verify 失败会直接退出；stub 已保证 socks 调用返回不同 IP 使 verify 通过）
: > "$T/argv"; : > "$T/stdin"
printf '#!/bin/bash\necho sing-box version 1.13.19\n' > "$T/sing-box"; chmod +x "$T/sing-box"
echo '{"enabled":false,"global":false,"urls":[]}' > "$T/residential-proxy.json"
BASE_DIR="$T" bash "$REPO/server/residential-helper.sh" enable --add 'socks5://us%40er:se%22cr@h3:1082' >/dev/null 2>&1
grep -q 'se"cr' "$T/argv" && { echo "FAIL B 密码在 argv"; fail=1; }
grep -q 'proxy-user = "us@er:se\\"cr"' "$T/stdin" || { echo "FAIL B stdin: $(cat "$T/stdin")"; fail=1; }

rm -rf "$T"
[[ $fail == 0 ]] && echo "PASS creds(bash)" || exit 1
```

注意 B 的 URL 解码规则以 `parse_url()`（`residential-helper.sh:68-93`）实际实现为准；若它不做 `%40` 解码，改用不含特殊字符的凭据 `user3:p3` 并相应改断言，转义逻辑由 A 的 `se"cr\et` 覆盖。

- [ ] **Step 2: 运行，确认失败**

Run: `bash scratchpad/resi-tests/test_creds.sh`
Expected: FAIL A（密码在 argv / 未用 -K）。

- [ ] **Step 3: 两个 bash 脚本改 `-K -`**

在两脚本各加（helper 放 `err()/info()` 之后；resi-health 放 `log()` 之后）：

```bash
# v3.6.0 R3: curl 配置文件双引号内转义 \ 与 "
curl_cfg_escape() { printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g'; }
# 生成 curl -K - 的配置：proxy 行 + 可选 proxy-user 行（凭据不出现在 argv）
curl_socks_cfg() {
    local host="$1" port="$2" user="$3" pass="$4"
    printf 'proxy = "socks5h://%s:%s"\n' "$host" "$port"
    [ -n "$user" ] && printf 'proxy-user = "%s:%s"\n' "$(curl_cfg_escape "$user")" "$(curl_cfg_escape "$pass")"
    return 0
}
```

`verify()` 里：

```bash
    exit_ip=$(curl_socks_cfg "$host" "$port" "$user" "$pass" \
        | curl -sS --max-time 10 -K - https://api.ipify.org 2>/dev/null) \
        || { err "连接住宅代理失败 (${host}:${port})"; return 1; }
```

`resi-health.sh` 探测循环：

```bash
    ok=0
    for _ in $(seq 1 "$TRIES"); do
        curl_socks_cfg "$host" "$port" "$user" "$pass" \
            | curl -s -o /dev/null --max-time "$TIMEOUT" -K - "$PROBE_URL" 2>/dev/null && ok=$((ok+1))
    done
```

删除不再使用的 `creds` 变量。用户名或密码含换行时 `curl_socks_cfg` 前加检查：`case "$user$pass" in *$'\n'*) log/err "凭据含换行，跳过"; continue/return 1;; esac`。

- [ ] **Step 4: 运行 bash 测试，确认通过**

Run: `bash scratchpad/resi-tests/test_creds.sh && bash -n server/residential-helper.sh server/resi-health.sh && bash scratchpad/resi-tests/test_lock_cooldown.sh`
Expected: 全 PASS。

- [ ] **Step 5: server.js 改 stdin 配置**

`residential/health` 处理器里，`const socksProxy = ...` 与 `execFile("curl", [...])` 替换为：

```js
                // v3.6.0 R3: 凭据经 stdin 配置文件传给 curl，不出现在进程参数里
                const curlCfgEscape = (s) => String(s).replace(/\\/g, "\\\\").replace(/"/g, '\\"');
                let curlCfg = `proxy = "socks5h://${raw.host}:${raw.port}"\n`;
                if (raw.username && raw.password) {
                    if (/[\r\n]/.test(raw.username + raw.password)) return sendJSON(res, baseResp);
                    curlCfg += `proxy-user = "${curlCfgEscape(raw.username)}:${curlCfgEscape(raw.password)}"\n`;
                }
                const child = execFile("curl", [
                    "-K", "-",
                    "-m", "8",
                    "-sS",
                    "http://ip-api.com/json/?fields=status,country,city,isp,org,as,mobile,proxy,hosting,query"
                ], { timeout: 9000, maxBuffer: 1 * 1024 * 1024 }, (err, stdout) => {
                    // …回调体原样保留…
                });
                child.stdin.on("error", () => { });
                child.stdin.end(curlCfg);
```

回调体不动。`socks5://` 用 `socks5h://` 让远端解析（与 bash 侧一致；原 `--proxy socks5://` 是本机解析，此处为健康探测无差别）。

- [ ] **Step 6: 验证 server.js**

```bash
node --check web/server.js
W=scratchpad/resi-tests/w3; bash scratchpad/resi-tests/run_server.sh "$W"
# 用 PATH 前置的 curl stub 起 server 才能断言；简化：直接读源码断言 + 功能冒烟
grep -n '"-K", "-"' web/server.js && ! grep -n 'socks5://\${encodeURIComponent' web/server.js
kill "$(cat $W/pid)"
```

再做一次真实调用冒烟（需登录 token，参考 `/api/login`；若不便，用 `node -e` 直接复现 execFile+stdin 片段调用本机不存在的代理，确认回调走 `err` 分支不崩溃）。

- [ ] **Step 7: 提交**

```bash
git add server/residential-helper.sh server/resi-health.sh web/server.js
git commit -m "fix(residential): SOCKS5 凭据改经 stdin 传给 curl,不再出现在命令行"
```


---

### Task 4: R4 中继显式处理 UDP

**Files:**
- Modify: `server/residential-helper.sh`（`write_singbox_config_residential_multi` 与 `write_singbox_config_direct` 的 `route.rules`）
- Test: `scratchpad/resi-tests/test_udp_rules.sh`

- [ ] **Step 1: 写测试（预期失败）**

```bash
#!/bin/bash
set -u
S=/tmp/claude-1000/-home-roots-b-ui/71917ef4-b1f0-4466-927f-5df467756568/scratchpad
REPO=/home/roots/b-ui; fail=0; T=$(mktemp -d); mkdir -p "$T/bin"
printf '#!/bin/bash\nexit 0\n' > "$T/bin/systemctl"; chmod +x "$T/bin/systemctl"; export PATH="$T/bin:$PATH"
printf '#!/bin/bash\necho sing-box version 1.13.19\n' > "$T/sing-box"; chmod +x "$T/sing-box"
check_rules() { # $1 文件
  jq -e '(.route.rules|map(select(.network=="udp"))) as $u
     | ($u|length)==3
     and $u[0]=={"network":"udp","port":53,"outbound":"direct"}
     and $u[1]=={"network":"udp","port":443,"action":"reject"}
     and $u[2]=={"network":"udp","outbound":"direct"}' "$1" >/dev/null || { echo "FAIL $1 udp 规则"; fail=1; }
  # 顺序：sniff 之后、第一条 ip_cidr 之前
  jq -e '(.route.rules|map(has("network"))|index(true)) > (.route.rules|map(.action=="sniff")|index(true))
     and (.route.rules|map(has("network"))|index(true)) < (.route.rules|map(has("ip_cidr"))|index(true))' "$1" >/dev/null || { echo "FAIL $1 顺序"; fail=1; }
}
for mode in multi direct; do
  if [[ $mode == multi ]]; then echo '{"enabled":true,"global":true,"domains":null,"urls":[{"host":"1.2.3.4","port":1080,"username":"u","password":"p","name":"url-1"},{"host":"1.2.3.5","port":1080,"username":"u","password":"p","name":"url-2"}]}' > "$T/residential-proxy.json"
  else echo '{"enabled":false,"global":false,"urls":[]}' > "$T/residential-proxy.json"; fi
  BASE_DIR="$T" bash "$REPO/server/residential-helper.sh" reapply >/dev/null 2>&1
  check_rules "$T/singbox-relay.json"
  for bin in /usr/bin/sing-box "$S/singbox-bins/v1.14.0/sing-box" "$S/singbox-bins/v1.15.0-alpha.2/sing-box"; do
    out=$("$bin" check -c "$T/singbox-relay.json" 2>&1) && ! grep -qi deprecated <<<"$out" || { echo "FAIL $mode check $bin: $out"; fail=1; }
  done
done
rm -rf "$T"; [[ $fail == 0 ]] && echo "PASS udp rules" || exit 1
```

- [ ] **Step 2: 运行确认失败** → Run: `bash $S/resi-tests/test_udp_rules.sh` → Expected: udp 规则 FAIL。

- [ ] **Step 3: 实现**

两个写函数的 jq 模板里，`[{"action": "sniff"}, {"ip_cidr": …}]` 之间插入：

```json
{"network": "udp", "port": 53, "outbound": "direct"},
{"network": "udp", "port": 443, "action": "reject"},
{"network": "udp", "outbound": "direct"}
```

并在模板上方加注释：`# v3.6.0 R4: 住宅 SOCKS5 基本不支持 UDP ASSOCIATE —— DNS 直连、QUIC 拒绝(浏览器回退 TCP 走住宅)、其余 UDP 直连`。

- [ ] **Step 4: 验证并提交**

```bash
bash -n server/residential-helper.sh && bash $S/resi-tests/test_udp_rules.sh && bash $S/resi-tests/test_domains.sh && bash $S/resi-tests/test_lock_cooldown.sh
git add server/residential-helper.sh
git commit -m "fix(residential): 中继显式处理 UDP(53 直连/443 拒绝/其余直连),global 模式下 DNS/QUIC 不再塞进住宅 SOCKS5"
```

---

### Task 5: R5 三层探测降频

**Files:**
- Modify: `server/residential-helper.sh`（urltest 字段）、`server/resi-health.sh`（默认值）、`server/update.sh`（D8 timer 文本 + 既有 timer 幂等补丁）、`web/server.js`（`generateSingboxConfig` 的 `urltest()`）
- Test: `scratchpad/resi-tests/test_probe_rate.sh`

- [ ] **Step 1: 写测试（预期失败）**

```bash
#!/bin/bash
set -u
S=/tmp/claude-1000/-home-roots-b-ui/71917ef4-b1f0-4466-927f-5df467756568/scratchpad
REPO=/home/roots/b-ui; fail=0; T=$(mktemp -d); mkdir -p "$T/bin"
printf '#!/bin/bash\nexit 0\n' > "$T/bin/systemctl"; chmod +x "$T/bin/systemctl"; export PATH="$T/bin:$PATH"
printf '#!/bin/bash\necho sing-box version 1.13.19\n' > "$T/sing-box"; chmod +x "$T/sing-box"
echo '{"enabled":true,"global":false,"domains":null,"urls":[{"host":"1.2.3.4","port":1080,"username":"u","password":"p","name":"url-1"},{"host":"1.2.3.5","port":1080,"username":"u","password":"p","name":"url-2"}]}' > "$T/residential-proxy.json"
BASE_DIR="$T" bash "$REPO/server/residential-helper.sh" reapply >/dev/null 2>&1
jq -e '.outbounds[]|select(.tag=="resi-pool")|.interval=="3m" and .tolerance==500 and .idle_timeout=="30m"' "$T/singbox-relay.json" >/dev/null || { echo "FAIL relay urltest"; fail=1; }
# 订阅
W="$S/resi-tests/w_rate"; rm -rf "$W"; bash "$S/ipv6-tests/run_server.sh" "$W" hop:on resi:list
curl -s http://127.0.0.1:18081/api/subscription/alice > "$W/a.json"
jq -e '[.outbounds[]|select(.type=="urltest")]|all(.interval=="60s" and .interrupt_exist_connections==false and .tolerance==100)' "$W/a.json" >/dev/null || { echo "FAIL 订阅 urltest"; fail=1; }
kill "$(cat "$W/pid")" 2>/dev/null
# 巡检默认值
grep -qE '^TRIES="\$\{RESI_HEALTH_TRIES:-2\}"' "$REPO/server/resi-health.sh" && grep -qE '^OK_NEED="\$\{RESI_HEALTH_OK_NEED:-1\}"' "$REPO/server/resi-health.sh" || { echo "FAIL health 默认值"; fail=1; }
# timer 文本（新装）与既有 timer 补丁
grep -q 'RandomizedDelaySec=30s' "$REPO/server/update.sh" || { echo "FAIL timer 文本"; fail=1; }
mkdir -p "$T/sysd"; printf '[Unit]\nDescription=B-UI Residential Health Timer\n[Timer]\nOnBootSec=2min\nOnUnitActiveSec=2min\nAccuracySec=20s\nPersistent=true\n[Install]\nWantedBy=timers.target\n' > "$T/sysd/b-ui-resi-health.timer"
extract() { sed -n "/^$1() {/,/^}/p" "$REPO/server/update.sh"; }
{ grep -E '^(RED|GREEN|YELLOW|BLUE|NC)=' "$REPO/server/update.sh" | head -5; for f in print_info print_success print_warning patch_resi_health_timer; do extract "$f"; done; } > "$T/u.sh"
( source "$T/u.sh"; RESI_HEALTH_TIMER_FILE="$T/sysd/b-ui-resi-health.timer" patch_resi_health_timer ) >/dev/null 2>&1
grep -q 'RandomizedDelaySec=30s' "$T/sysd/b-ui-resi-health.timer" || { echo "FAIL 既有 timer 未补"; fail=1; }
cp "$T/sysd/b-ui-resi-health.timer" "$T/t1"; ( source "$T/u.sh"; RESI_HEALTH_TIMER_FILE="$T/sysd/b-ui-resi-health.timer" patch_resi_health_timer ) >/dev/null 2>&1
cmp -s "$T/t1" "$T/sysd/b-ui-resi-health.timer" || { echo "FAIL timer 补丁不幂等"; fail=1; }
rm -rf "$T"; [[ $fail == 0 ]] && echo "PASS probe rate" || exit 1
```

- [ ] **Step 2: 运行确认失败** → Run: `bash $S/resi-tests/test_probe_rate.sh` → Expected: 四类断言 FAIL。

- [ ] **Step 3: 实现**

- `residential-helper.sh` urltest：`"interval": "3m", "tolerance": 500, "idle_timeout": "30m"`，注释 `# v3.6.0 R5: sing-box 默认 3m；10s 会让每个住宅 IP 每分钟被打 6 次，且 50ms 容忍导致会话内换 IP`。
- `resi-health.sh`：`TRIES` 默认 2、`OK_NEED` 默认 1；头部注释同步。
- `update.sh`：D8 块的 timer heredoc 在 `AccuracySec=20s` 后加 `RandomizedDelaySec=30s`；新增函数
  ```bash
  # v3.6.0 R5: 既有 timer 补随机抖动（幂等）
  patch_resi_health_timer() {
      local f="${RESI_HEALTH_TIMER_FILE:-/etc/systemd/system/b-ui-resi-health.timer}"
      [[ -f "$f" ]] || return 0
      grep -q '^RandomizedDelaySec=' "$f" && return 0
      sed -i '/^AccuracySec=/a RandomizedDelaySec=30s' "$f" && systemctl daemon-reload 2>/dev/null || true
      print_success "  ✓ b-ui-resi-health.timer 加随机抖动 30s"
  }
  ```
  在 D8 块末尾（timer 存在分支）调用。
- `web/server.js` `urltest()`：`interval: "60s"`, `tolerance: 100`, `interrupt_exist_connections: false`（注释同上）。

- [ ] **Step 4: 验证并提交**

```bash
bash -n server/residential-helper.sh server/resi-health.sh server/update.sh && node --check web/server.js
bash $S/resi-tests/test_probe_rate.sh && bash $S/resi-tests/test_udp_rules.sh && bash $S/resi-tests/test_lock_cooldown.sh && bash $S/ipv6-tests/test_subscription.sh
git add server/residential-helper.sh server/resi-health.sh server/update.sh web/server.js
git commit -m "fix(residential): 三层探测降频(中继 3m/500ms, 订阅 60s 不打断连接, 巡检 2 次+timer 抖动)"
```
