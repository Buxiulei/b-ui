# 服务端重启硬化与订阅端口修正 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修三个体检 P0：v2rayN 订阅的 HY2直连 `mport` 按实际监听输出；面板用户变更只在配置真变化时非阻塞重启对应服务；cron 自更新按变更文件门控重启并加抖动。附带安装期依赖复查。

**Architecture:** `web/server.js` 的 `getConfig()` 以 `config.yaml` `listen` 行为端口跳跃真源，`/api/sub/` 条件拼 `mport`；三个 `update*Config()` 生成新内容与磁盘比较后再写再用 `spawn(...).unref()` 重启；`server/update.sh` `auto_update()` 用下载前后 md5 得到变更集决定重启、非交互时随机延迟；`install.sh` 依赖安装后复查。

**Tech Stack:** Node.js ESM（无框架）；bash + md5sum + jq。

**Spec:** `docs/superpowers/specs/2026-09-10-restart-hardening-design.md`

## Global Constraints

- 只动 spec 列出的文件：`web/server.js`、`server/update.sh`、`install.sh`、`server/core.sh`（仅 `apply_hy2_userpass_auth` 一行）。不改 `version.json`（随 IPv6 计划最后 bump）。
- Surgical：不重构邻近代码；`saveUsers()` 的调用结构不变，只改三个 `update*Config()` 内部与新增 `restartServiceAsync()`。
- 端口跳跃真源规则固定：`listen: :PORT,START-END` 有区间 → `{enabled:true,start,end}`；无区间 → 沿用 `port-hopping.json`。住宅 HY2 固定 `mport=41000-50000`。
- 重启一律 `spawn("systemctl",["restart",unit],{detached:true,stdio:"ignore"}).unref()`，不再 `execSync`。
- `auto_update()` 里 `hysteria-server`/`hysteria-residential`/`xray` 不再无条件重启；只有 `web/` 文件变更才重启 `b-ui-admin`。
- 每个 Task 一个提交，提交信息 `fix(<scope>): …`，结尾附：
  Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_01GqNZ1FqiwWYBXC3CGmBqsU
- 只提交不 push；显式 `git add <file>`，禁止 `git commit -a`。
- 测试放 `/tmp/claude-1000/-home-roots-b-ui/71917ef4-b1f0-4466-927f-5df467756568/scratchpad/restart-tests/`；本地起 server.js 用 `scratchpad/resi-tests/run_server.sh` 的已修正版（住宅 Task 1 修过 PID bug）为蓝本。不碰 `/opt/b-ui`。

---

### Task 1: P0-1 `/api/sub/` 的 `mport` 按实际监听输出

**Files:**
- Modify: `web/server.js:341-345`（`getConfig()` 内 `pm = hc.match(...)` 附近）、`:380-390`（`portHopping` 读取）、`:1636-1662`（`buildHy2Url` 与两处直连调用）
- Create: `scratchpad/restart-tests/run_server.sh`、`scratchpad/restart-tests/test_mport.sh`

**Interfaces:**
- Produces: `cfg.portHopping` 语义变更（listen 区间优先），被 `/api/sub/`、`generateSingboxConfig`、`generateClashConfig`、`/api/config` 共同消费。

- [ ] **Step 1: 写测试（预期失败）**

`run_server.sh`（在住宅计划的 `resi-tests/run_server.sh` 基础上加 `listen`/json 两个参数）：

```bash
#!/bin/bash
# 用法: run_server.sh <workdir> <listen行> <json:none|enabled> ; 端口 18082
set -u
REPO=/home/roots/b-ui
W="$1"; LISTEN="$2"; JSON="${3:-none}"; mkdir -p "$W/certs"
cp "$REPO/server/residential-helper.sh" "$W/"; chmod +x "$W/residential-helper.sh"
cat > "$W/users.json" <<'EOF'
[{"username":"alice","password":"pw","uuid":"11111111-2222-3333-4444-555555555555","sni":"www.bing.com","protocol":"fusion","residential":true,"createdAt":"2026-09-10T00:00:00Z","limits":{}}]
EOF
printf 'listen: %s\ntls:\n  cert: /etc/letsencrypt/live/proxy.example.com/fullchain.pem\n  key: /x/privkey.pem\nauth:\n  type: userpass\n  userpass:\n    alice: pw\n\ntrafficStats:\n  listen: 127.0.0.1:9999\n' "$LISTEN" > "$W/config.yaml"
echo "proxy.example.com" > "$W/certs/.domain"
echo '{"privateKey":"priv","publicKey":"pubkeyXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX","shortId":"abcd1234"}' > "$W/reality-keys.json"
echo '{"inbounds":[{"tag":"vless-direct","port":10001,"protocol":"vless","settings":{"clients":[]},"streamSettings":{"realitySettings":{"dest":"www.bing.com:443","serverNames":["www.bing.com"],"shortIds":["abcd1234"]}}},{"tag":"vless-residential","port":10002,"protocol":"vless","settings":{"clients":[]},"streamSettings":{"realitySettings":{"dest":"www.bing.com:443","serverNames":["www.bing.com"],"shortIds":["abcd1234"]}}}],"outbounds":[{"tag":"direct","protocol":"freedom"}]}' > "$W/xray-config.json"
[[ "$JSON" == "enabled" ]] && echo '{"enabled":true,"startPort":20000,"endPort":30000}' > "$W/port-hopping.json" || rm -f "$W/port-hopping.json"
echo '{"enabled":false,"global":false,"urls":[]}' > "$W/residential-proxy.json"
( cd "$REPO/web" && BASE_DIR="$W" ADMIN_DIR="$REPO/web" ADMIN_PORT=18082 ADMIN_PASSWORD=test123 SERVER_IP=203.0.113.10 exec node server.js > "$W/server.log" 2>&1 ) &
echo $! > "$W/pid"; sleep 1.5
```

`test_mport.sh`：

```bash
#!/bin/bash
set -u
S=/tmp/claude-1000/-home-roots-b-ui/71917ef4-b1f0-4466-927f-5df467756568/scratchpad/restart-tests
fail=0
check() { # <listen> <json> <期望直连 mport 片段或 NONE>
  local W="$S/w_$RANDOM"; bash "$S/run_server.sh" "$W" "$1" "$2"
  local links; links=$(curl -s http://127.0.0.1:18082/api/sub/alice | base64 -d)
  local direct; direct=$(grep -F 'HY2%E7%9B%B4%E8%BF%9E' <<<"$links" || grep -F '直连' <<<"$links" | grep '^hysteria2://' | head -1)
  local resi; resi=$(grep '^hysteria2://' <<<"$links" | grep -v "$direct" | head -1)
  if [[ "$3" == NONE ]]; then grep -q 'mport=' <<<"$direct" && { echo "FAIL [$1|$2] 直连不该有 mport: $direct"; fail=1; }
  else grep -q "mport=$3" <<<"$direct" || { echo "FAIL [$1|$2] 直连期望 mport=$3: $direct"; fail=1; }; fi
  grep -q 'mport=41000-50000' <<<"$resi" || { echo "FAIL [$1|$2] 住宅 mport: $resi"; fail=1; }
  # sing-box / clash 一致性
  local sb; sb=$(curl -s http://127.0.0.1:18082/api/subscription/alice | jq -r '.outbounds[]|select(.tag=="hy2-direct")|(.server_ports // .hop_ports // ["NONE"])|.[0]')
  if [[ "$3" == NONE ]]; then [[ "$sb" == "NONE" ]] || { echo "FAIL [$1|$2] sing-box 直连不该跳跃: $sb"; fail=1; }
  else [[ "$sb" == "${3/-/:}" || "$sb" == "$3" ]] || { echo "FAIL [$1|$2] sing-box 直连区间: $sb"; fail=1; }; fi
  kill "$(cat "$W/pid")" 2>/dev/null; sleep 0.3
}
check ':10000'                 none    NONE
check ':10000,25000-26000'     none    25000-26000
check ':10000'                 enabled 20000-30000
check ':10000,25000-26000'     enabled 25000-26000
[[ $fail == 0 ]] && echo "PASS mport" || exit 1
```

（`grep -F 'HY2%E7%9B%B4%E8%BF%9E'` 匹配 URL 编码的 `HY2直连` 标签；若实际编码不同，按 `links` 输出调整匹配方式，目标是准确取到直连那条链接。）

- [ ] **Step 2: 运行，确认失败**

Run: `bash $S/test_mport.sh`
Expected: 第 1、2、4 组 FAIL（直连总是 `mport=20000-30000`）。

- [ ] **Step 3: 改 `getConfig()`**

在 `portHopping` 的 `try{...}catch{}` 之后加：

```js
        // v3.6.0: hysteria 实际监听 (listen: :PORT[,START-END]) 是端口跳跃的真源；
        // 有区间即启用且用该区间；无区间沿用 port-hopping.json（老式 iptables 路径兜底）
        const lm = hc.match(/^listen:\s*:?(\d+)(?:,(\d+)-(\d+))?\s*$/m);
        if (lm && lm[2]) portHopping = { enabled: true, start: parseInt(lm[2]), end: parseInt(lm[3]) };
```

- [ ] **Step 4: 改 `/api/sub/`**

```js
                const buildHy2Url = (port, hopRange, label, includeObfs) => {
                    if (!user.password) return null;
                    const auth = `${encodeURIComponent(user.username)}:${encodeURIComponent(user.password)}`;
                    // v3.6.0: mport 只在实际启用端口跳跃时输出（写死 20000-30000 曾让未开跳跃/自定义区间的服务器必现连不上）
                    let qp = `sni=${serverHost}&insecure=0`;
                    if (hopRange) qp += `&mport=${hopRange}`;
                    …（obfs 段与 return 不变）
                };
                const directHop = cfg.portHopping?.enabled ? `${cfg.portHopping.start}-${cfg.portHopping.end}` : null;
```

两处 `buildHy2Url(cfg.port || 10000, "20000-30000", "HY2直连", true)` → `buildHy2Url(cfg.port || 10000, directHop, "HY2直连", true)`。住宅两处不变。

- [ ] **Step 5: 运行测试**

Run: `node --check web/server.js && bash $S/test_mport.sh`
Expected: `PASS mport`。

- [ ] **Step 6: 提交**

```bash
git add web/server.js
git commit -m "fix(sub): v2rayN 订阅 HY2直连 mport 按 config.yaml 实际监听输出,未开跳跃不再写死 20000-30000"
```

---

### Task 2: P0-2 用户变更只在配置真变化时非阻塞重启

**Files:**
- Modify: `web/server.js:255-338`（`updateHysteriaResidentialConfig`、`updateHysteriaConfig`、`updateXrayConfig`），新增 `restartServiceAsync()` 放在这三者之前
- Create: `scratchpad/restart-tests/test_restart_gate.sh`

**Interfaces:**
- Produces: `restartServiceAsync(unit: string): void`。

- [ ] **Step 1: 写测试（预期失败）**

```bash
#!/bin/bash
set -u
S=/tmp/claude-1000/-home-roots-b-ui/71917ef4-b1f0-4466-927f-5df467756568/scratchpad/restart-tests
fail=0; T=$(mktemp -d); mkdir -p "$T/bin"
cat > "$T/bin/systemctl" <<'EOF'
#!/bin/bash
echo "$@" >> "${STUB_LOG}"; exit 0
EOF
chmod +x "$T/bin/systemctl"; export STUB_LOG="$T/systemctl.log"; export PATH="$T/bin:$PATH"; : > "$STUB_LOG"
W="$S/w_gate"; rm -rf "$W"; bash "$S/run_server.sh" "$W" ':10000' none
# hysteria-residential 配置也放一份，验证它也受门控
cp "$W/config.yaml" "$W/config-residential.yaml"
API=http://127.0.0.1:18082/api
TOKEN=$(curl -s -X POST "$API/login" -H 'Content-Type: application/json' -d '{"password":"test123"}' | jq -r '.token')
[[ -n "$TOKEN" && "$TOKEN" != null ]] || { echo "FAIL login: $(curl -s -X POST "$API/login" -H 'Content-Type: application/json' -d '{"password":"test123"}')"; exit 1; }
H=(-H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json')
m0=$(stat -c %Y "$W/config.yaml" "$W/config-residential.yaml" "$W/xray-config.json" | tr '\n' ' ')
# A. 只改限额 → 无重启、文件不动
sleep 1.1; : > "$STUB_LOG"
curl -s -X PUT "$API/users/alice" "${H[@]}" -d '{"trafficLimit":123456789}' >/dev/null; sleep 0.8
[[ -s "$STUB_LOG" ]] && { echo "FAIL A 改限额触发重启: $(cat "$STUB_LOG")"; fail=1; }
m1=$(stat -c %Y "$W/config.yaml" "$W/config-residential.yaml" "$W/xray-config.json" | tr '\n' ' ')
[[ "$m0" == "$m1" ]] || { echo "FAIL A 文件被改写"; fail=1; }
# B. 改密码 → hysteria 两实例重启，xray 不重启
: > "$STUB_LOG"
curl -s -X PUT "$API/users/alice" "${H[@]}" -d '{"password":"newpw"}' >/dev/null; sleep 0.8
grep -q 'restart hysteria-server' "$STUB_LOG" || { echo "FAIL B 未重启 hysteria-server: $(cat "$STUB_LOG")"; fail=1; }
grep -q 'restart hysteria-residential' "$STUB_LOG" || { echo "FAIL B 未重启 hysteria-residential"; fail=1; }
grep -q 'restart xray' "$STUB_LOG" && { echo "FAIL B 不该重启 xray"; fail=1; }
grep -q 'newpw' "$W/config.yaml" || { echo "FAIL B config.yaml 未更新"; fail=1; }
# C. 新增带 uuid 用户 → xray 重启
: > "$STUB_LOG"
curl -s -X POST "$API/users" "${H[@]}" -d '{"username":"bob","protocol":"vless-reality"}' >/dev/null; sleep 0.8
grep -q 'restart xray' "$STUB_LOG" || { echo "FAIL C 新增 vless 用户未重启 xray: $(cat "$STUB_LOG")"; fail=1; }
kill "$(cat "$W/pid")" 2>/dev/null; rm -rf "$T"
[[ $fail == 0 ]] && echo "PASS restart gate" || exit 1
```

登录接口的请求体字段、`PUT /api/users/<name>` 与 `POST /api/users` 的 body 字段以 `web/server.js:1457`、`:1705-1822` 的实现为准，先读再写测试。

- [ ] **Step 2: 运行，确认失败**

Run: `bash $S/test_restart_gate.sh`
Expected: A FAIL（三份都重启），B 可能 FAIL（xray 也重启）。

- [ ] **Step 3: 实现**

在 `updateHysteriaResidentialConfig` 之前加：

```js
// v3.6.0: 非阻塞重启（范式同 :2373 的 b-ui-admin 自重启）；失败只记日志
function restartServiceAsync(unit) {
    try {
        spawn("systemctl", ["restart", unit], { detached: true, stdio: "ignore" }).unref();
    } catch (e) { log("ERROR", `restart ${unit}: ${e.message}`); }
}
```

三个函数改为"先算后比再写"：

```js
function updateHysteriaResidentialConfig(users) {
    try {
        if (!fs.existsSync(CONFIG.hysteriaResidentialConfig)) return;
        const c = fs.readFileSync(CONFIG.hysteriaResidentialConfig, "utf8");
        const up = users.reduce((a, u) => { a[u.username] = u.password; return a; }, {});
        const auth = "auth:\n  type: userpass\n  userpass:\n" + Object.entries(up).map(([u, p]) => "    " + u + ": " + p).join("\n");
        const next = c.replace(/auth:[\s\S]*?(?=\n[a-zA-Z]|$)/, auth + "\n\n");
        if (next === c) return; // v3.6.0: 内容未变不写不重启（改限额/到期日不再踢掉所有在线用户）
        fs.writeFileSync(CONFIG.hysteriaResidentialConfig, next);
        restartServiceAsync("hysteria-residential");
    } catch (e) { log("ERROR", "HysteriaResidential: " + e.message); }
}
```

`updateHysteriaConfig` 同形（`hysteria-server`）。`updateXrayConfig`：开头 `const raw = fs.readFileSync(CONFIG.xrayConfig, "utf8"); let c = JSON.parse(raw);`，末尾：

```js
        const next = JSON.stringify(c, null, 2);
        if (next === raw) return;
        fs.writeFileSync(CONFIG.xrayConfig, next);
        restartServiceAsync("xray");
```

注意：磁盘上的 `xray-config.json` 若原本不是 `JSON.stringify(c,null,2)` 的格式（如 core.sh 手写缩进），第一次比较必然不同并重启一次，之后稳定；这是可接受的一次性开销，在提交信息里说明。

- [ ] **Step 4: 运行测试**

Run: `node --check web/server.js && bash $S/test_restart_gate.sh && bash $S/test_mport.sh`
Expected: 全 PASS。

- [ ] **Step 5: 提交**

```bash
git add web/server.js
git commit -m "fix(panel): 用户变更只在配置真变化时非阻塞重启对应服务(改限额不再踢掉所有在线用户)"
```

---

### Task 3: P0-3 自更新按变更文件门控重启 + 抖动 + 内核更新按版本变化重启

**Files:**
- Modify: `server/update.sh:1940-2035`（`auto_update`）、`:1786-1840`（`auto_update_kernel`）、`:2105-2112`（主入口 `auto)`/`kernel)`）
- Create: `scratchpad/restart-tests/test_auto_update.sh`

- [ ] **Step 1: 写测试（预期失败）**

```bash
#!/bin/bash
set -u
REPO=/home/roots/b-ui; fail=0; T=$(mktemp -d); mkdir -p "$T/bin" "$T/base/packages" "$T/admin"
cat > "$T/bin/systemctl" <<'EOF'
#!/bin/bash
echo "$@" >> "${STUB_LOG}"; [[ "$1" == is-active ]] && exit 0; exit 0
EOF
chmod +x "$T/bin/systemctl"; export STUB_LOG="$T/systemctl.log"; export PATH="$T/bin:$PATH"
extract() { sed -n "/^$1() {/,/^}/p" "$REPO/server/update.sh"; }
# 壳：真实 auto_update + version_compare；其余 stub
mk_shell() { # $1 = 远程要变更的文件列表(空格分隔, remote 名)
  {
    echo "BASE_DIR='$T/base'; ADMIN_DIR='$T/admin'; DOWNLOAD_URL='http://stub'; GITHUB_RAW='http://stub'"
    grep -E '^(RED|GREEN|YELLOW|BLUE|NC)=' "$REPO/server/update.sh" | head -5
    for f in print_info print_success print_warning print_error version_compare auto_update; do extract "$f"; done
    echo "get_local_version(){ echo 3.5.23; }; get_remote_version(){ echo 3.6.0; }; select_download_source(){ :; }"
    echo "apply_systemd_configs(){ :; }; ensure_cron_jobs(){ :; }; auto_update_kernel(){ :; }; npm(){ :; }"
    echo "CHANGED='$1'"
    echo 'download_and_validate(){ local url="$1" p="$2"; local rel="${url#http://stub/}"; if [[ " $CHANGED " == *" $rel "* ]]; then echo "changed-$RANDOM" > "$p"; else [[ -f "$p" ]] || echo "same" > "$p"; fi; return 0; }'
  } > "$T/shell.sh"
}
seed() { for p in version.json core.sh b-ui-cli.sh update.sh residential-helper.sh resi-health.sh b-ui-client.sh; do echo same > "$T/base/$p"; done; for p in server.js package.json index.html style.css app.js qrcode.min.js logo.jpg; do echo same > "$T/admin/$p"; done; }
# A. 只有 web/app.js 变 → 只重启 b-ui-admin
seed; mk_shell "web/app.js version.json"; : > "$STUB_LOG"
( source "$T/shell.sh"; auto_update ) >/dev/null 2>&1
grep -q 'restart b-ui-admin' "$STUB_LOG" || { echo "FAIL A 未重启 b-ui-admin: $(cat "$STUB_LOG")"; fail=1; }
grep -qE 'restart (hysteria|xray)' "$STUB_LOG" && { echo "FAIL A 不该重启代理核心: $(cat "$STUB_LOG")"; fail=1; }
# B. 只有 b-ui-client.sh 变 → 无任何 restart
seed; mk_shell "b-ui-client.sh version.json"; : > "$STUB_LOG"
( source "$T/shell.sh"; auto_update ) >/dev/null 2>&1
grep -q 'restart' "$STUB_LOG" && { echo "FAIL B 不该重启: $(cat "$STUB_LOG")"; fail=1; }
grep -q '变更文件' /var/log/b-ui-update.log 2>/dev/null || true   # 日志路径为 /var/log，若无权限忽略
# C. 内核：hysteria version 前后相同 → 不重启
{ echo "BASE_DIR='$T/base'"; grep -E '^(RED|GREEN|YELLOW|BLUE|NC)=' "$REPO/server/update.sh" | head -5
  for f in print_info print_success print_warning print_error _is_newer auto_update_kernel; do extract "$f"; done
  echo 'hysteria(){ echo "Version: v2.9.1"; }; xray(){ echo "Xray 26.3.27"; }'
  echo 'curl(){ if [[ "$*" == *hysteria* ]]; then echo "\"tag_name\": \"app/v2.9.9\""; elif [[ "$*" == *Xray* ]]; then echo "\"tag_name\": \"v26.3.27\""; else echo ""; fi; }'
  echo 'bash(){ :; }; command(){ if [[ "$2" == sing-box ]]; then return 1; fi; builtin command "$@"; }'
} > "$T/kshell.sh"
: > "$STUB_LOG"
( source "$T/kshell.sh"; auto_update_kernel ) >/dev/null 2>&1
grep -q 'restart hysteria-server' "$STUB_LOG" && { echo "FAIL C 安装未生效仍重启: $(cat "$STUB_LOG")"; fail=1; }
rm -rf "$T"
[[ $fail == 0 ]] && echo "PASS auto_update" || exit 1
```

`auto_update()` 写日志到 `/var/log/b-ui-update.log`；测试壳里若不可写，`echo >>` 失败会因无 `set -e` 被忽略。若函数体内有其它未 stub 的外部命令（如 `cp -f`、`mkdir`、`chmod`）都是真实可用的。`auto_update_kernel` 的 `bash <(curl ...)` 在壳里被 `bash(){ :; }` 拦下；`curl` 函数按参数返回假 JSON。

- [ ] **Step 2: 运行，确认失败**

Run: `bash $S/test_auto_update.sh`
Expected: A FAIL（重启了 hysteria/xray）、B FAIL、C FAIL。

- [ ] **Step 3: 改 `auto_update()`**

下载循环前：

```bash
        # v3.6.0 P0-3: 记录下载前哈希，只重启真正受影响的服务
        declare -A before_md5=()
        for remote in "${!file_map[@]}"; do
            local lp="${file_map[$remote]}"
            before_md5["$remote"]=$( [[ -f "$lp" ]] && md5sum "$lp" 2>/dev/null | cut -d' ' -f1 || echo "" )
        done
```

下载循环后、`packages` 同步前：

```bash
        local changed_files=() web_changed=0
        for remote in "${!file_map[@]}"; do
            local lp="${file_map[$remote]}" after
            after=$( [[ -f "$lp" ]] && md5sum "$lp" 2>/dev/null | cut -d' ' -f1 || echo "" )
            if [[ "$after" != "${before_md5[$remote]}" ]]; then
                changed_files+=("$remote"); [[ "$remote" == web/* ]] && web_changed=1
            fi
        done
        echo "[$(date '+%Y-%m-%d %H:%M:%S')] 变更文件: ${changed_files[*]:-无}" >> "$LOG_FILE"
```

把四行无条件 `systemctl restart …` 替换为：

```bash
        # v3.6.0 P0-3: 只有面板文件变了才重启 b-ui-admin；hysteria/xray 的运行配置由 apply_systemd_configs
        # 与各迁移块按需重启，版本升级本身不再无条件踢掉所有在线用户
        if [[ $web_changed -eq 1 ]]; then
            systemctl restart b-ui-admin 2>/dev/null || true
            echo "[$(date '+%Y-%m-%d %H:%M:%S')] 重启: b-ui-admin（web 文件变更）" >> "$LOG_FILE"
        else
            echo "[$(date '+%Y-%m-%d %H:%M:%S')] 无需重启服务" >> "$LOG_FILE"
        fi
```

- [ ] **Step 4: 改 `auto_update_kernel()`**

Hysteria2 分支：

```bash
        bash <(curl -fsSL https://get.hy2.sh/) >> "$LOG_FILE" 2>&1 || true
        local new_hy; new_hy=$(hysteria version 2>/dev/null | grep "^Version:" | awk '{print $2}' | sed 's/^v//' || echo "")
        if [[ -n "$new_hy" && "$new_hy" != "$local_hy" ]]; then
            systemctl restart hysteria-server 2>/dev/null || true
            systemctl is-active --quiet hysteria-residential 2>/dev/null && systemctl restart hysteria-residential 2>/dev/null || true
            updated=true
        else
            echo "[$(date '+%Y-%m-%d %H:%M:%S')] Hysteria2 安装未生效（仍为 v${local_hy}），跳过重启" >> "$LOG_FILE"
        fi
```

Xray 分支同形（`xray version | head -n1 | awk '{print $2}' | sed 's/^v//'`，`systemctl restart xray`）。注意 hysteria 内核更新原来只重启 `hysteria-server`，住宅实例跑同一二进制，这里补上（它本就应随内核更新重启）。

- [ ] **Step 5: 抖动**

主入口 `auto)` 与 `kernel)` 分支内、调用函数前加：

```bash
            # v3.6.0: cron 触发时随机延迟 0-15 分钟，避免机队整点齐步重启；交互/手动执行不延迟
            [[ -t 0 || -n "${B_UI_NO_JITTER:-}" ]] || sleep $((RANDOM % 900))
```

- [ ] **Step 6: 运行测试**

Run: `bash -n server/update.sh && bash $S/test_auto_update.sh`
Expected: `PASS auto_update`。

- [ ] **Step 7: 提交**

```bash
git add server/update.sh
git commit -m "fix(update): 自更新按变更文件门控重启(不再无条件重启四服务) + cron 抖动 + 内核更新按版本变化重启"
```

---

### Task 4: 依赖复查（flock/jq 静默失效）

**Files:**
- Modify: `install.sh:94-132`（`check_dependencies`）
- Modify: `server/core.sh:662`（`apply_hy2_userpass_auth` 的 `command -v jq || return 0`）
- Modify: `server/update.sh:1001`（D6 的 `command -v jq` 条件）
- Create: `scratchpad/restart-tests/test_deps.sh`

- [ ] **Step 1: 写测试（预期失败）**

```bash
#!/bin/bash
set -u
REPO=/home/roots/b-ui; fail=0; T=$(mktemp -d); mkdir -p "$T/bin"
# 壳：只保留 curl/jq/dig/openssl/sed/grep/…，故意不放 flock；apt-get stub 什么都不装
for c in bash sed grep awk cut tr head tail cat mkdir rm cp mv date printf echo command tee sort uniq wc curl jq dig openssl; do p=$(command -v $c 2>/dev/null) && ln -sf "$p" "$T/bin/$c"; done
printf '#!/bin/bash\nexit 0\n' > "$T/bin/apt-get"; chmod +x "$T/bin/apt-get"
extract() { sed -n "/^$1() {/,/^}/p" "$REPO/install.sh"; }
{ grep -E '^(RED|GREEN|YELLOW|BLUE|NC)=' "$REPO/install.sh" | head -5; for f in print_info print_success print_warning print_error check_dependencies; do extract "$f"; done; } > "$T/shell.sh"
out=$(PATH="$T/bin" bash -c "source '$T/shell.sh'; check_dependencies" 2>&1); rc=$?
[[ $rc -ne 0 ]] || { echo "FAIL A 缺 flock 应失败退出 (rc=$rc): $out"; fail=1; }
grep -q 'flock' <<<"$out" || { echo "FAIL A 未点名 flock: $out"; fail=1; }
# B. core.sh 缺 jq 时应有 warning
extract_c() { sed -n "/^$1() {/,/^}/p" "$REPO/server/core.sh"; }
{ grep -E '^(RED|GREEN|YELLOW|BLUE|NC)=' "$REPO/server/core.sh" | head -5; echo "BASE_DIR='$T'; USERS_FILE='$T/users.json'"; for f in print_info print_success print_warning print_error apply_hy2_userpass_auth; do extract_c "$f"; done; } > "$T/cshell.sh"
echo '[{"username":"a","password":"p"}]' > "$T/users.json"
mkdir -p "$T/nojq"; for c in bash sed grep awk cat mktemp rm mv chmod date; do p=$(command -v $c) && ln -sf "$p" "$T/nojq/$c"; done
out=$(PATH="$T/nojq" bash -c "source '$T/cshell.sh'; apply_hy2_userpass_auth" 2>&1)
grep -q 'jq' <<<"$out" || { echo "FAIL B core.sh 缺 jq 无提示: '$out'"; fail=1; }
rm -rf "$T"
[[ $fail == 0 ]] && echo "PASS deps" || exit 1
```

- [ ] **Step 2: 运行，确认失败**

Run: `bash $S/test_deps.sh`
Expected: A FAIL（rc=0 且不提 flock）、B FAIL（无输出）。

- [ ] **Step 3: 实现**

`install.sh` `deps_map` 加 `"flock:util-linux:util-linux"`；在 `print_success "依赖安装完成"` 之前改为复查：

```bash
        # v3.6.0: 安装命令不看退出码（apt 锁/源限速/包 hold 都会静默失败），逐个复查
        local still_missing=()
        for cmd in "${missing_cmds[@]}"; do command -v "$cmd" &>/dev/null || still_missing+=("$cmd"); done
        if [[ ${#still_missing[@]} -gt 0 ]]; then
            print_error "依赖安装失败: ${still_missing[*]}（缺 jq 会让 hy2 认证卡在 http 回调面板，缺 flock 会让住宅巡检失效）"
            print_error "请手动安装后重新运行安装脚本"
            exit 1
        fi
        print_success "依赖安装完成"
```

`core.sh:662`：`command -v jq >/dev/null 2>&1 || { print_warning "缺少 jq，hy2 认证仍走 http 回调面板（高并发时面板成单点），请安装 jq 后重跑更新"; return 0; }`。

`update.sh` D6：把 `if [[ -f "${BASE_DIR}/users.json" ]] && command -v jq >/dev/null 2>&1; then` 拆成：先 `if [[ -f users.json ]]; then if ! command -v jq; then print_warning "缺少 jq，跳过 hy2 userpass 迁移（认证仍走 http）"; else …原逻辑… fi; fi`，保持缩进风格。

- [ ] **Step 4: 运行测试**

Run: `bash -n install.sh server/core.sh server/update.sh && bash $S/test_deps.sh && bash $S/test_auto_update.sh`
Expected: 全 PASS。

- [ ] **Step 5: 提交**

```bash
git add install.sh server/core.sh server/update.sh
git commit -m "fix(install): 依赖安装后逐个复查并纳入 flock;缺 jq 时 userpass 迁移不再静默跳过"
```
