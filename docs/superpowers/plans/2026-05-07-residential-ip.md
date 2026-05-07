# Residential IP Outbound Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Route OpenAI/ChatGPT/Google/Gemini/Claude/Anthropic traffic through an upstream SOCKS5 residential IP proxy, while keeping all other traffic on the VPS's direct connection.

**Architecture:** A shared bash helper script (`server/residential-helper.sh`, deploys to `/opt/b-ui/residential-helper.sh`) is the single point of control. It parses credentials, verifies connectivity, writes Xray's JSON config via `jq`, injects Hysteria2's YAML via `awk` marker regions, and reloads services. Three entry points (installer, CLI, web API) all call this helper. The web API uses `spawnSync` with argument arrays (not string interpolation) to prevent command injection. Config state persists in `/opt/b-ui/residential-proxy.json`.

**Tech Stack:** bash, jq (already installed by install.sh), curl, awk, systemd; Node.js ESM (web/server.js); vanilla JS (web/app.js); HTML (web/index.html)

---

## File Map

| File | Action | What changes |
|---|---|---|
| `server/residential-helper.sh` | **Create** | Core logic: parse/verify/apply-xray/apply-hysteria/reload/status |
| `install.sh` | **Modify** | Add helper to download list; add residential prompt in fresh-install path |
| `server/core.sh` | **Modify** | Add `# B-UI:RESIDENTIAL-START/END` anchors to Hysteria2 config template |
| `server/b-ui-cli.sh` | **Modify** | Add menu item 12 + submenu function + dispatcher case |
| `server/update.sh` | **Modify** | Call `residential-helper.sh reapply` after upgrade to re-apply existing config |
| `web/server.js` | **Modify** | Import `spawnSync`; add GET/POST/DELETE `/api/residential` endpoints |
| `web/index.html` | **Modify** | Add 🏠 toolbar button + `m-resi` modal |
| `web/app.js` | **Modify** | Add `openResi()`, `saveResi()`, `disableResi()` functions |
| `version.json` | **Modify** | Bump to v3.3.0 + changelog entry |
| `CLAUDE.md` | **Modify** | Document helper script and sed-anchor convention |

---

## Task 1: Add residential-helper.sh to install.sh download list

**Files:**
- Modify: `install.sh` (around line 234, inside `download_all_files` file array)

- [ ] **Step 1: Add the helper to the files array**

In `install.sh`, find the `local files=(` array inside `download_all_files` (around line 232). Add one line after `"server/update.sh:${BASE_DIR}/update.sh"`:

```bash
        "server/residential-helper.sh:${BASE_DIR}/residential-helper.sh"
```

The block after the edit should look like:
```bash
    local files=(
        "version.json:${BASE_DIR}/version.json"
        "server/core.sh:${BASE_DIR}/core.sh"
        "server/b-ui-cli.sh:${BASE_DIR}/b-ui-cli.sh"
        "server/update.sh:${BASE_DIR}/update.sh"
        "server/residential-helper.sh:${BASE_DIR}/residential-helper.sh"
        "web/server.js:${ADMIN_DIR}/server.js"
        ...
    )
```

- [ ] **Step 2: Verify syntax**

```bash
bash -n install.sh
```
Expected: no output (success).

- [ ] **Step 3: Commit**

```bash
git add install.sh
git commit -m "feat(install): add residential-helper.sh to download list"
```

---

## Task 2: Create server/residential-helper.sh

**Files:**
- Create: `server/residential-helper.sh`

- [ ] **Step 1: Create the file**

Write the following complete content to `server/residential-helper.sh`:

```bash
#!/bin/bash
# residential-helper.sh — 住宅 IP SOCKS5 出站代理助手
# Usage:
#   residential-helper.sh enable <url>   → prints exit_ip (line1) isp_info (line2)
#   residential-helper.sh disable        → removes residential config
#   residential-helper.sh status         → prints residential-proxy.json
#   residential-helper.sh reapply        → re-applies existing enabled config (used by update.sh)

set -euo pipefail

BASE_DIR="${BASE_DIR:-/opt/b-ui}"
RESIDENTIAL_CONFIG="${BASE_DIR}/residential-proxy.json"
XRAY_CONFIG="${BASE_DIR}/xray-config.json"
HYSTERIA_CONFIG="${BASE_DIR}/config.yaml"

RED='\033[0;31m'; YELLOW='\033[1;33m'; NC='\033[0m'
err()  { echo -e "${RED}ERROR: $*${NC}" >&2; }
info() { echo -e "${YELLOW}$*${NC}" >&2; }

# ---------------------------------------------------------------------------
# URL 解析 → 导出 RESI_HOST RESI_PORT RESI_USER RESI_PASS
# 支持: socks5://user:pass@host:port | user:pass@host:port | host:port:user:pass
# ---------------------------------------------------------------------------
parse_url() {
    local url="$1"
    local s="${url#socks5://}"

    if [[ "$s" == *"@"* ]]; then
        local userpass="${s%%@*}"
        local hostport="${s##*@}"
        RESI_USER="${userpass%%:*}"
        RESI_PASS="${userpass#*:}"
        RESI_HOST="${hostport%%:*}"
        RESI_PORT="${hostport##*:}"
    elif [[ "$url" =~ ^([^:@]+):([0-9]+):([^:]+):(.+)$ ]]; then
        RESI_HOST="${BASH_REMATCH[1]}"
        RESI_PORT="${BASH_REMATCH[2]}"
        RESI_USER="${BASH_REMATCH[3]}"
        RESI_PASS="${BASH_REMATCH[4]}"
    else
        err "无法解析凭据格式。支持: socks5://user:pass@host:port 或 host:port:user:pass"
        return 1
    fi

    [[ -n "${RESI_HOST:-}" && -n "${RESI_PORT:-}" && -n "${RESI_USER:-}" && -n "${RESI_PASS:-}" ]] \
        || { err "解析结果包含空字段"; return 1; }
    [[ "$RESI_PORT" =~ ^[0-9]+$ ]] \
        || { err "端口必须是数字，实际: ${RESI_PORT}"; return 1; }
}

# ---------------------------------------------------------------------------
# 连通性校验 → 导出 RESI_EXIT_IP RESI_ISP_INFO
# ---------------------------------------------------------------------------
verify() {
    local host="$1" port="$2" user="$3" pass="$4"

    info "获取 VPS 公网 IP..."
    local vps_ip
    vps_ip=$(curl -sS --max-time 5 https://api.ipify.org 2>/dev/null) \
        || { err "无法获取 VPS 公网 IP，请检查网络连接"; return 1; }

    info "通过 SOCKS5 测试出口..."
    local exit_ip
    exit_ip=$(curl -sS --max-time 10 \
        --socks5-hostname "${user}:${pass}@${host}:${port}" \
        https://api.ipify.org 2>/dev/null) \
        || { err "连接住宅代理失败 (${host}:${port})，请检查凭据"; return 1; }

    [[ "$exit_ip" == "$vps_ip" ]] \
        && { err "出口 IP 与 VPS 相同 (${vps_ip})，代理未生效，请检查凭据是否正确"; return 1; }

    RESI_EXIT_IP="$exit_ip"
    RESI_ISP_INFO=$(curl -sS --max-time 5 "https://ipinfo.io/${exit_ip}/json" 2>/dev/null \
        | jq -r '((.org // "") + ", " + (.city // "") + ", " + (.country // "")) | gsub("null"; "")' \
        2>/dev/null || echo "")
}

# ---------------------------------------------------------------------------
# Xray 配置写入 (jq 原子替换)
# ---------------------------------------------------------------------------
apply_xray() {
    local host="$1" port="$2" user="$3" pass="$4"

    local outbounds
    outbounds=$(jq -n \
        --arg  host "$host" \
        --argjson port "$port" \
        --arg  user "$user" \
        --arg  pass "$pass" \
        '[{"tag":"residential","protocol":"socks","settings":{"servers":[{
            "address":$host,"port":$port,
            "users":[{"user":$user,"pass":$pass,"level":0}]
          }]}},
          {"tag":"direct","protocol":"freedom"}]')

    local rules='[
      {"type":"field","inboundTag":["api"],"outboundTag":"api"},
      {"type":"field","ip":["geoip:private"],"outboundTag":"direct"},
      {"type":"field","domain":["geosite:openai","geosite:google","domain:anthropic.com","domain:claude.ai"],"outboundTag":"residential"},
      {"type":"field","outboundTag":"direct","network":"tcp,udp"}
    ]'

    local backup="${XRAY_CONFIG}.bak.$(date +%Y%m%d%H%M%S)"
    cp "${XRAY_CONFIG}" "$backup"
    jq --argjson outbounds "$outbounds" --argjson rules "$rules" \
       '.outbounds = $outbounds | .routing.rules = $rules' \
       "${XRAY_CONFIG}" > "${XRAY_CONFIG}.tmp" \
    && mv "${XRAY_CONFIG}.tmp" "${XRAY_CONFIG}" \
    || { cp "$backup" "${XRAY_CONFIG}"; err "Xray 配置写入失败"; return 1; }
}

# ---------------------------------------------------------------------------
# Hysteria2 配置写入 (awk 锚点注入)
# ---------------------------------------------------------------------------
apply_hysteria() {
    local host="$1" port="$2" user="$3" pass="$4"
    local config="${HYSTERIA_CONFIG}"

    if ! grep -q "# B-UI:RESIDENTIAL-START" "$config"; then
        printf '\n# B-UI:RESIDENTIAL-START\n# B-UI:RESIDENTIAL-END\n' >> "$config"
    fi

    local backup="${config}.bak.$(date +%Y%m%d%H%M%S)"
    cp "$config" "$backup"

    local block_file
    block_file=$(mktemp)
    cat > "$block_file" << EOYAML
outbounds:
  - name: residential
    type: socks5
    socks5:
      addr: ${host}:${port}
      username: ${user}
      password: ${pass}
  - name: direct
    type: direct
acl:
  inline:
    - direct(127.0.0.0/8)
    - direct(10.0.0.0/8)
    - direct(172.16.0.0/12)
    - direct(192.168.0.0/16)
    - direct(169.254.0.0/16)
    - residential(*.openai.com)
    - residential(openai.com)
    - residential(*.chatgpt.com)
    - residential(chatgpt.com)
    - residential(*.google.com)
    - residential(google.com)
    - residential(*.googleapis.com)
    - residential(*.gstatic.com)
    - residential(*.anthropic.com)
    - residential(anthropic.com)
    - residential(*.claude.ai)
    - residential(claude.ai)
    - direct(all)
EOYAML

    local tmp
    tmp=$(mktemp)
    awk -v blockfile="$block_file" '
        /# B-UI:RESIDENTIAL-START/ { print; while((getline ln < blockfile) > 0) print ln; close(blockfile); skip=1; next }
        /# B-UI:RESIDENTIAL-END/   { skip=0 }
        !skip { print }
    ' "$config" > "$tmp" \
    && mv "$tmp" "$config" \
    || { cp "$backup" "$config"; rm -f "$block_file" "$tmp"; err "Hysteria2 配置写入失败"; return 1; }

    rm -f "$block_file"
}

# ---------------------------------------------------------------------------
# 清除配置 (还原 Xray/Hysteria 到默认)
# ---------------------------------------------------------------------------
clear_xray() {
    local backup="${XRAY_CONFIG}.bak.$(date +%Y%m%d%H%M%S)"
    cp "${XRAY_CONFIG}" "$backup"
    jq '.outbounds = [{"protocol":"freedom","tag":"direct"}] |
        .routing.rules = [{"type":"field","inboundTag":["api"],"outboundTag":"api"}]' \
       "${XRAY_CONFIG}" > "${XRAY_CONFIG}.tmp" \
    && mv "${XRAY_CONFIG}.tmp" "${XRAY_CONFIG}" \
    || { cp "$backup" "${XRAY_CONFIG}"; err "Xray 配置清除失败"; return 1; }
}

clear_hysteria() {
    local config="${HYSTERIA_CONFIG}"
    grep -q "# B-UI:RESIDENTIAL-START" "$config" || return 0

    local backup="${config}.bak.$(date +%Y%m%d%H%M%S)"
    cp "$config" "$backup"

    local tmp
    tmp=$(mktemp)
    awk '
        /# B-UI:RESIDENTIAL-START/ { print; skip=1; next }
        /# B-UI:RESIDENTIAL-END/   { skip=0 }
        !skip { print }
    ' "$config" > "$tmp" \
    && mv "$tmp" "$config" \
    || { cp "$backup" "$config"; rm -f "$tmp"; err "Hysteria2 配置清除失败"; return 1; }
}

reload_services() {
    systemctl restart xray 2>/dev/null || true
    systemctl restart hysteria-server 2>/dev/null || true
}

save_config() {
    local enabled="$1"
    jq -n \
        --argjson enabled "$enabled" \
        --arg  host      "${RESI_HOST:-}" \
        --argjson port   "${RESI_PORT:-0}" \
        --arg  username  "${RESI_USER:-}" \
        --arg  password  "${RESI_PASS:-}" \
        --arg  lastVerifiedIp      "${RESI_EXIT_IP:-}" \
        --arg  lastVerifiedIspInfo "${RESI_ISP_INFO:-}" \
        --arg  lastVerifiedAt "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        '{enabled:$enabled,host:$host,port:$port,username:$username,
          password:$password,lastVerifiedIp:$lastVerifiedIp,
          lastVerifiedIspInfo:$lastVerifiedIspInfo,lastVerifiedAt:$lastVerifiedAt}' \
        > "${RESIDENTIAL_CONFIG}"
    chmod 600 "${RESIDENTIAL_CONFIG}"
}

# ---------------------------------------------------------------------------
# 主入口
# ---------------------------------------------------------------------------
cmd="${1:-}"
case "$cmd" in
    enable)
        [[ -z "${2:-}" ]] && { err "用法: $0 enable <socks5_url>"; exit 1; }
        parse_url "$2"
        verify "$RESI_HOST" "$RESI_PORT" "$RESI_USER" "$RESI_PASS"
        apply_xray "$RESI_HOST" "$RESI_PORT" "$RESI_USER" "$RESI_PASS"
        apply_hysteria "$RESI_HOST" "$RESI_PORT" "$RESI_USER" "$RESI_PASS"
        save_config true
        reload_services
        echo "$RESI_EXIT_IP"
        echo "${RESI_ISP_INFO:-}"
        ;;

    disable)
        RESI_HOST="" RESI_PORT=0 RESI_USER="" RESI_PASS="" RESI_EXIT_IP="" RESI_ISP_INFO=""
        clear_xray
        clear_hysteria
        if [[ -f "${RESIDENTIAL_CONFIG}" ]]; then
            jq '.enabled = false' "${RESIDENTIAL_CONFIG}" > "${RESIDENTIAL_CONFIG}.tmp" \
                && mv "${RESIDENTIAL_CONFIG}.tmp" "${RESIDENTIAL_CONFIG}"
        fi
        reload_services
        ;;

    status)
        if [[ -f "${RESIDENTIAL_CONFIG}" ]]; then
            cat "${RESIDENTIAL_CONFIG}"
        else
            echo '{"enabled":false}'
        fi
        ;;

    reapply)
        [[ -f "${RESIDENTIAL_CONFIG}" ]] || exit 0
        enabled=$(jq -r '.enabled' "${RESIDENTIAL_CONFIG}")
        [[ "$enabled" == "true" ]] || exit 0
        RESI_HOST=$(jq -r '.host'     "${RESIDENTIAL_CONFIG}")
        RESI_PORT=$(jq -r '.port'     "${RESIDENTIAL_CONFIG}")
        RESI_USER=$(jq -r '.username' "${RESIDENTIAL_CONFIG}")
        RESI_PASS=$(jq -r '.password' "${RESIDENTIAL_CONFIG}")
        apply_xray "$RESI_HOST" "$RESI_PORT" "$RESI_USER" "$RESI_PASS"
        apply_hysteria "$RESI_HOST" "$RESI_PORT" "$RESI_USER" "$RESI_PASS"
        reload_services
        ;;

    *)
        echo "Usage: $0 {enable <url>|disable|status|reapply}" >&2
        exit 1
        ;;
esac
```

- [ ] **Step 2: Make executable and syntax check**

```bash
chmod +x server/residential-helper.sh
bash -n server/residential-helper.sh
```
Expected: no output.

- [ ] **Step 3: Commit**

```bash
git add server/residential-helper.sh
git commit -m "feat: add residential-helper.sh — SOCKS5 outbound core logic"
```

---

## Task 3: Add B-UI:RESIDENTIAL anchor markers to Hysteria2 config template

**Files:**
- Modify: `server/core.sh` (~line 418, end of Hysteria2 config heredoc)

The heredoc currently ends with:
```bash
masquerade:
  type: proxy
  proxy:
    url: ${MASQUERADE_URL}
    rewriteHost: true
EOF
```

- [ ] **Step 1: Add anchor lines before the closing EOF**

Edit the heredoc to add two anchor lines, leaving the final EOF unchanged:

```bash
masquerade:
  type: proxy
  proxy:
    url: ${MASQUERADE_URL}
    rewriteHost: true

# B-UI:RESIDENTIAL-START
# B-UI:RESIDENTIAL-END
EOF
```

- [ ] **Step 2: Syntax check**

```bash
bash -n server/core.sh
```
Expected: no output.

- [ ] **Step 3: Commit**

```bash
git add server/core.sh
git commit -m "feat(core): add residential IP anchor markers to Hysteria2 config template"
```

---

## Task 4: Add residential IP interactive prompt to install.sh (fresh install)

**Files:**
- Modify: `install.sh`

- [ ] **Step 1: Add the function definition**

In `install.sh`, after the `create_global_command` function definition (~line 559), add:

```bash
#===============================================================================
# 住宅 IP 交互配置（全新安装时询问）
#===============================================================================

configure_residential_interactive() {
    echo ""
    print_info "========================================================"
    print_info "可选：配置住宅 IP 出站"
    print_info "启用后 OpenAI / Google / Claude 流量将走住宅 IP，其余直出"
    print_info "========================================================"
    read -rp "$(echo -e "${YELLOW}是否配置住宅 IP？(y/N): ${NC}")" ans
    [[ "${ans,,}" != "y" ]] && return 0

    read -rp "$(echo -e "${YELLOW}请粘贴凭据 (socks5://user:pass@host:port): ${NC}")" resi_url
    [[ -z "$resi_url" ]] && { print_warning "已跳过住宅 IP 配置"; return 0; }

    print_info "正在校验连通性..."
    local output exit_code
    output=$("${BASE_DIR}/residential-helper.sh" enable "$resi_url" 2>&1) && exit_code=0 || exit_code=$?

    if [[ $exit_code -eq 0 ]]; then
        local exit_ip isp_info
        exit_ip=$(echo "$output" | head -1)
        isp_info=$(echo "$output" | sed -n '2p')
        print_success "住宅 IP 已启用，出口 IP: ${exit_ip}"
        [[ -n "$isp_info" ]] && print_info "ISP: ${isp_info}"
    else
        print_error "住宅 IP 校验失败（配置未保存）："
        echo "$output" | grep "ERROR:" | sed 's/.*ERROR: //' >&2
        print_warning "可稍后通过 'sudo b-ui' 菜单或 Web 看板重新配置"
    fi
}
```

- [ ] **Step 2: Call the function in case 2 (fresh install only)**

Find the `case $install_type` block. In case 2, add the call after `run_core_install`:

```bash
        2)  # 全新安装
            print_info "开始全新安装..."
            download_all_files
            run_core_install
            configure_residential_interactive
            ;;
```

- [ ] **Step 3: Syntax check**

```bash
bash -n install.sh
```
Expected: no output.

- [ ] **Step 4: Commit**

```bash
git add install.sh
git commit -m "feat(install): add residential IP prompt to fresh install flow"
```

---

## Task 5: Add CLI submenu to b-ui-cli.sh

**Files:**
- Modify: `server/b-ui-cli.sh`

- [ ] **Step 1: Add menu item 12 to show_menu**

In `show_menu`, find the line showing `11. VPS 质量测试` and add a new line after it:

```bash
    echo -e "${CYAN}║${NC}  ${YELLOW}11.${NC} ${BLUE}VPS 质量测试${NC}                                            ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}12.${NC} ${YELLOW}配置住宅 IP 出站${NC}                                        ${CYAN}║${NC}"
```

Also update the read prompt from `[0-11]` to `[0-12]`:
```bash
        read -p "请选择 [0-12]: " choice
```

- [ ] **Step 2: Add the configure_residential_menu function**

Add this function before `main()`:

```bash
#===============================================================================
# 住宅 IP 配置菜单
#===============================================================================

configure_residential_menu() {
    local helper="${BASE_DIR}/residential-helper.sh"

    while true; do
        echo ""
        echo -e "${CYAN}════════════ 配置住宅 IP 出站 ════════════${NC}"

        local status enabled exit_ip isp_info verified_at
        status=$("$helper" status 2>/dev/null || echo '{"enabled":false}')
        enabled=$(echo "$status" | jq -r '.enabled' 2>/dev/null || echo "false")
        exit_ip=$(echo "$status" | jq -r '.lastVerifiedIp // ""' 2>/dev/null || echo "")
        isp_info=$(echo "$status" | jq -r '.lastVerifiedIspInfo // ""' 2>/dev/null || echo "")
        verified_at=$(echo "$status" | jq -r '.lastVerifiedAt // ""' 2>/dev/null || echo "")

        if [[ "$enabled" == "true" ]]; then
            echo -e "  状态: ${GREEN}已启用 ✓${NC}"
            [[ -n "$exit_ip" ]]     && echo -e "  出口 IP: ${YELLOW}${exit_ip}${NC}"
            [[ -n "$isp_info" ]]    && echo -e "  ISP: ${isp_info}"
            [[ -n "$verified_at" ]] && echo -e "  最后校验: ${verified_at}"
        else
            echo -e "  状态: ${RED}未启用${NC}"
        fi

        echo ""
        echo -e "  ${YELLOW}1.${NC} 启用 / 修改凭据"
        echo -e "  ${YELLOW}2.${NC} 禁用住宅 IP"
        echo -e "  ${YELLOW}3.${NC} 重新校验连通性"
        echo -e "  ${YELLOW}0.${NC} 返回主菜单"
        echo ""
        read -rp "请选择: " sub

        case "$sub" in
            1)
                read -rp "$(echo -e "${YELLOW}请粘贴凭据 (socks5://user:pass@host:port): ${NC}")" resi_url
                [[ -z "$resi_url" ]] && continue
                print_info "正在校验连通性..."
                local out ec
                out=$("$helper" enable "$resi_url" 2>&1) && ec=0 || ec=$?
                if [[ $ec -eq 0 ]]; then
                    print_success "住宅 IP 已启用，出口 IP: $(echo "$out" | head -1)"
                    local isp
                    isp=$(echo "$out" | sed -n '2p')
                    [[ -n "$isp" ]] && print_info "ISP: $isp"
                else
                    print_error "校验失败（配置未保存）:"
                    echo "$out" | grep "ERROR:" | sed 's/.*ERROR: //'
                fi
                ;;
            2)
                "$helper" disable && print_success "住宅 IP 已禁用，流量恢复直出" \
                    || print_error "禁用失败"
                ;;
            3)
                if [[ "$enabled" != "true" ]]; then
                    print_warning "住宅 IP 未启用，无法重新校验"
                    continue
                fi
                local host port user pass
                host=$(echo "$status" | jq -r '.host')
                port=$(echo "$status" | jq -r '.port')
                user=$(echo "$status" | jq -r '.username')
                pass=$(echo "$status" | jq -r '.password')
                print_info "正在重新校验..."
                local out ec
                out=$("$helper" enable "socks5://${user}:${pass}@${host}:${port}" 2>&1) && ec=0 || ec=$?
                if [[ $ec -eq 0 ]]; then
                    print_success "校验成功，出口 IP: $(echo "$out" | head -1)"
                else
                    print_error "校验失败: $(echo "$out" | grep "ERROR:" | sed 's/.*ERROR: //')"
                fi
                ;;
            0) return ;;
            *) print_error "无效选项" ;;
        esac
    done
}
```

- [ ] **Step 3: Add dispatcher case**

In the `case $choice in` block, add after `11)`:

```bash
            12) configure_residential_menu ;;
```

- [ ] **Step 4: Syntax check**

```bash
bash -n server/b-ui-cli.sh
```
Expected: no output.

- [ ] **Step 5: Commit**

```bash
git add server/b-ui-cli.sh
git commit -m "feat(cli): add residential IP configuration submenu (option 12)"
```

---

## Task 6: Add API endpoints to web/server.js

**Files:**
- Modify: `web/server.js`

- [ ] **Step 1: Add spawnSync to the import on line 4**

Current line 4:
```javascript
import { execSync, exec } from "child_process";
```

New line 4:
```javascript
import { execSync, exec, spawnSync } from "child_process";
```

- [ ] **Step 2: Add residentialConfig and residentialHelper to CONFIG**

Find the `CONFIG` object (around line 33). Add two lines after the `xrayKeysFile` entry:

```javascript
    residentialConfig: process.env.RESIDENTIAL_CONFIG || `${BASE_DIR}/residential-proxy.json`,
    residentialHelper: `${BASE_DIR}/residential-helper.sh`,
```

- [ ] **Step 3: Add the three API endpoints**

Find `if (r === "masquerade")` (around line 1604). Add the residential block immediately before it:

```javascript
            if (r === "residential") {
                const helperPath = CONFIG.residentialHelper;

                if (req.method === "GET") {
                    try {
                        const raw = fs.existsSync(CONFIG.residentialConfig)
                            ? JSON.parse(fs.readFileSync(CONFIG.residentialConfig, "utf8"))
                            : { enabled: false };
                        const display = { ...raw };
                        if (display.password) display.password = display.password.slice(0, 2) + "***";
                        if (display.username && display.host) {
                            display.displayUrl = `socks5://${display.username.slice(0, 2)}***@${display.host}:${display.port}`;
                        }
                        return sendJSON(res, display);
                    } catch {
                        return sendJSON(res, { enabled: false });
                    }
                }

                if (req.method === "POST") {
                    const b = await parseBody(req);
                    if (!b.url) return sendJSON(res, { error: "url 字段必填" }, 400);
                    try {
                        const result = spawnSync(helperPath, ["enable", b.url], {
                            env: { ...process.env, BASE_DIR },
                            encoding: "utf8",
                            timeout: 30000,
                        });
                        if (result.status !== 0) {
                            const errMsg = (result.stderr || "").replace(/\x1b\[[0-9;]*m/g, "").trim();
                            return sendJSON(res, { error: errMsg || "住宅 IP 启用失败" }, 400);
                        }
                        const lines = result.stdout.trim().split("\n");
                        return sendJSON(res, { success: true, exitIp: lines[0] || "", ispInfo: lines[1] || "" });
                    } catch (e) {
                        return sendJSON(res, { error: e.message }, 500);
                    }
                }

                if (req.method === "DELETE") {
                    try {
                        const result = spawnSync(helperPath, ["disable"], {
                            env: { ...process.env, BASE_DIR },
                            encoding: "utf8",
                            timeout: 15000,
                        });
                        if (result.status !== 0) {
                            return sendJSON(res, { error: (result.stderr || "禁用失败").trim() }, 500);
                        }
                        return sendJSON(res, { success: true });
                    } catch (e) {
                        return sendJSON(res, { error: e.message }, 500);
                    }
                }
            }
```

- [ ] **Step 4: Syntax check**

```bash
node --input-type=module -e "import('./web/server.js')" 2>&1 | grep "SyntaxError"
```
Expected: no output.

- [ ] **Step 5: Commit**

```bash
git add web/server.js
git commit -m "feat(server): add GET/POST/DELETE /api/residential endpoints"
```

---

## Task 7: Add residential IP UI to web/index.html and web/app.js

**Files:**
- Modify: `web/index.html`
- Modify: `web/app.js`

### index.html

- [ ] **Step 1: Add toolbar button**

Find the masquerade button (around line 38):
```html
<button class="ibtn" onclick="openMasq()" title="伪装网站设置">🎭</button>
```

Add the residential button immediately after it:
```html
<button class="ibtn" onclick="openResi()" title="住宅 IP 出站">🏠</button>
```

- [ ] **Step 2: Add modal**

Find `<div id="m-masq"` (around line 222). Add the residential modal immediately before it:

```html
    <!-- Residential IP Modal -->
    <div id="m-resi" class="modal">
        <div class="modal-card">
            <h3>🏠 住宅 IP 出站</h3>
            <div class="info-box" style="margin-bottom:16px; font-size:13px; color:var(--text-dim)">
                OpenAI / Google / Claude 流量走住宅 IP，其余流量直出 VPS。
            </div>
            <div id="resi-status" style="margin-bottom:16px; font-size:13px; padding:10px; border-radius:8px; background:var(--bg-card)">
                加载中...
            </div>
            <input type="password" id="resi-url" placeholder="socks5://user:pass@host:port"
                   style="font-family:monospace; margin-bottom:8px">
            <div style="font-size:12px; color:var(--text-dim); margin-bottom:16px">
                支持格式: socks5://user:pass@host:port 或 host:port:user:pass
            </div>
            <div id="resi-error" style="display:none; color:var(--red,#e74c3c); font-size:13px; margin-bottom:12px"></div>
            <div style="display:grid; grid-template-columns:1fr 1fr 1fr; gap:10px">
                <button class="btn btn-secondary" onclick="closeM()">取消</button>
                <button class="btn btn-secondary" id="resi-disable-btn" onclick="disableResi()">禁用</button>
                <button class="btn" onclick="saveResi()">保存启用</button>
            </div>
        </div>
    </div>
```

### app.js

- [ ] **Step 3: Add functions at end of app.js**

```javascript
// ─── 住宅 IP 出站 ─────────────────────────────────────────────────────────────

function openResi() {
    const statusEl = $("#resi-status");
    const urlEl    = $("#resi-url");
    const errEl    = $("#resi-error");
    const disBtn   = $("#resi-disable-btn");
    statusEl.textContent = "加载中...";
    urlEl.value = "";
    errEl.style.display = "none";
    openM("m-resi");
    api("/residential").then(r => {
        statusEl.textContent = "";
        if (r.enabled) {
            const dot = document.createElement("span");
            dot.style.color = "var(--green,#2ecc71)";
            dot.textContent = "● 已启用";
            const sep = document.createTextNode("　出口 IP: ");
            const ipEl = document.createElement("b");
            ipEl.textContent = r.lastVerifiedIp || "-";
            const br = document.createElement("br");
            const ispEl = document.createElement("span");
            ispEl.style.cssText = "font-size:12px;color:var(--text-dim)";
            ispEl.textContent = r.lastVerifiedIspInfo || "";
            statusEl.append(dot, sep, ipEl, br, ispEl);
            urlEl.placeholder = r.displayUrl || "socks5://user:pass@host:port";
            disBtn.style.display = "";
        } else {
            const dot = document.createElement("span");
            dot.style.color = "var(--text-dim)";
            dot.textContent = "● 未启用";
            statusEl.append(dot);
            disBtn.style.display = "none";
        }
    }).catch(() => {
        statusEl.textContent = "状态获取失败";
    });
}

function saveResi() {
    const url    = $("#resi-url").value.trim();
    const errEl  = $("#resi-error");
    errEl.style.display = "none";
    if (!url) { errEl.textContent = "请填写凭据"; errEl.style.display = ""; return; }
    api("/residential", { method: "POST", body: JSON.stringify({ url }) }).then(r => {
        if (r.success) {
            closeM();
            toast("住宅 IP 已启用，出口 IP: " + (r.exitIp || ""));
        } else {
            errEl.textContent = r.error || "保存失败";
            errEl.style.display = "";
        }
    }).catch(e => {
        errEl.textContent = e.message || "请求失败";
        errEl.style.display = "";
    });
}

function disableResi() {
    const errEl = $("#resi-error");
    errEl.style.display = "none";
    api("/residential", { method: "DELETE" }).then(r => {
        if (r.success) { closeM(); toast("住宅 IP 已禁用"); }
        else { errEl.textContent = r.error || "禁用失败"; errEl.style.display = ""; }
    }).catch(e => {
        errEl.textContent = e.message || "请求失败";
        errEl.style.display = "";
    });
}
```

- [ ] **Step 4: Syntax check**

```bash
node --check web/app.js
```
Expected: no output.

- [ ] **Step 5: Commit**

```bash
git add web/index.html web/app.js
git commit -m "feat(web): add residential IP UI — toolbar button and modal"
```

---

## Task 8: Add update.sh re-apply migration

**Files:**
- Modify: `server/update.sh`

After an upgrade, Xray/Hysteria configs may be regenerated. The `reapply` subcommand re-injects the residential config if it was enabled.

- [ ] **Step 1: Find the post-update service restart section**

```bash
grep -n "systemctl restart\|重启.*服务\|reload" server/update.sh | tail -10
```

- [ ] **Step 2: Add reapply call after the service restarts**

After the existing `systemctl restart hysteria-server` / `systemctl restart xray` lines (in the update completion section), add:

```bash
    # 重新应用住宅 IP 配置（如已启用，防止升级重写 outbound 配置丢失）
    if [[ -f "${BASE_DIR}/residential-helper.sh" ]]; then
        "${BASE_DIR}/residential-helper.sh" reapply 2>/dev/null || true
    fi
```

- [ ] **Step 3: Syntax check**

```bash
bash -n server/update.sh
```
Expected: no output.

- [ ] **Step 4: Commit**

```bash
git add server/update.sh
git commit -m "feat(update): re-apply residential IP config after upgrade"
```

---

## Task 9: Bump version.json and update CLAUDE.md

**Files:**
- Modify: `version.json`
- Modify: `CLAUDE.md`

- [ ] **Step 1: Bump version.json**

Update `version` field to `"3.3.0"` and prepend a new changelog entry:

```json
"3.3.0": "新增住宅 IP 出站功能：OpenAI/Google/Claude 流量可选走住宅 SOCKS5 代理，三入口配置（一键安装/CLI/Web看板）"
```

- [ ] **Step 2: Add to CLAUDE.md Key Design Patterns**

In the `## Key Design Patterns` section, add:

```markdown
- **Residential IP helper**: `server/residential-helper.sh` (deploys to `/opt/b-ui/residential-helper.sh`) is the single control point for residential SOCKS5 outbound. Call with `enable <url>`, `disable`, `status`, or `reapply`. State in `/opt/b-ui/residential-proxy.json` (chmod 600). Hysteria2 config uses `# B-UI:RESIDENTIAL-START/END` awk anchor markers; Xray config uses `jq` atomic replace on `outbounds` + `routing.rules`.
```

- [ ] **Step 3: Commit**

```bash
git add version.json CLAUDE.md
git commit -m "bump: v3.3.0 新增住宅 IP 出站 (OpenAI/Google/Claude 分流)"
```

---

## Acceptance Test Checklist

```bash
# Syntax checks — all must produce no output
bash -n server/residential-helper.sh
bash -n install.sh
bash -n server/core.sh
bash -n server/b-ui-cli.sh
bash -n server/update.sh
node --check web/app.js
node --input-type=module -e "import('./web/server.js')" 2>&1 | grep "SyntaxError"

# Helper rejects bad URL format
./server/residential-helper.sh enable "badformat" 2>&1 | grep -i "error"
# Expected: error message about format

# Helper rejects wrong credentials (fake host — will timeout after ~10s)
BASE_DIR=/tmp timeout 15 ./server/residential-helper.sh enable "socks5://fake:fake@1.2.3.4:1080" 2>&1
# Expected: "连接住宅代理失败" error, exit code 1
```

On live server with valid credentials:
- `sudo b-ui` → option 12 → enable with valid URL → shows exit IP
- `sudo b-ui` → option 12 → status shows ISP info
- Web panel → 🏠 button → modal opens with status → save valid URL → toast shows exit IP
- Access `openai.com` through proxy → exit IP is residential
- Access `baidu.com` through proxy → exit IP is VPS (direct)
- Run `update.sh` → residential config survives upgrade
