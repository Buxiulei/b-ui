#!/bin/bash
# residential-helper.sh — 住宅 IP SOCKS5 出站代理助手
# Usage:
#   residential-helper.sh enable <url>          → prints exit_ip (line1) isp_info (line2)
#   residential-helper.sh disable               → removes residential config
#   residential-helper.sh status                → prints residential-proxy.json
#   residential-helper.sh reapply               → re-applies existing enabled config (used by update.sh)
#   residential-helper.sh set-domains <json>    → update domain list and reapply if enabled

set -euo pipefail

BASE_DIR="${BASE_DIR:-/opt/b-ui}"
RESIDENTIAL_CONFIG="${BASE_DIR}/residential-proxy.json"
XRAY_CONFIG="${BASE_DIR}/xray-config.json"
HYSTERIA_CONFIG="${BASE_DIR}/config.yaml"

RED='\033[0;31m'; YELLOW='\033[1;33m'; BLUE='\033[0;34m'; NC='\033[0m'
err()  { echo -e "${RED}ERROR: $*${NC}" >&2; }
info() { echo -e "${BLUE}$*${NC}" >&2; }

DEFAULT_DOMAINS=("openai" "chatgpt" "google" "googleapis" "gstatic" "anthropic" "claude" "ping0" "ip.sb")

# ---------------------------------------------------------------------------
# 读取分流域名列表 → 导出 DOMAINS 数组
# 优先从 residential-proxy.json 读取，否则使用默认列表
# ---------------------------------------------------------------------------
get_domains() {
    if [[ -f "${RESIDENTIAL_CONFIG}" ]]; then
        local d
        d=$(jq '.domains // empty' "${RESIDENTIAL_CONFIG}" 2>/dev/null || echo "")
        if [[ -n "$d" && "$d" != "null" && "$d" != "[]" ]]; then
            mapfile -t DOMAINS < <(jq -r '.[]' <<< "$d")
            return
        fi
    fi
    DOMAINS=("${DEFAULT_DOMAINS[@]}")
}

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
# Xray 配置写入 (jq 原子替换，域名动态生成)
# ---------------------------------------------------------------------------
apply_xray() {
    local host="$1" port="$2" user="$3" pass="$4"

    get_domains
    local domain_json
    domain_json=$(printf '%s\n' "${DOMAINS[@]}" | jq -R . | jq -s 'map("keyword:" + .)')

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

    local rules
    rules=$(jq -n \
        --argjson domains "$domain_json" \
        '[
          {"type":"field","inboundTag":["api"],"outboundTag":"api"},
          {"type":"field","ip":["geoip:private"],"outboundTag":"direct"},
          {"type":"field","domain":$domains,"outboundTag":"residential"},
          {"type":"field","outboundTag":"direct","network":"tcp,udp"}
        ]')

    local backup="${XRAY_CONFIG}.bak.$(date +%Y%m%d%H%M%S)"
    cp "${XRAY_CONFIG}" "$backup"
    jq --argjson outbounds "$outbounds" --argjson rules "$rules" \
       '.outbounds = $outbounds | .routing.rules = $rules' \
       "${XRAY_CONFIG}" > "${XRAY_CONFIG}.tmp" \
    && mv "${XRAY_CONFIG}.tmp" "${XRAY_CONFIG}" \
    || { cp "$backup" "${XRAY_CONFIG}"; err "Xray 配置写入失败"; return 1; }
}

# ---------------------------------------------------------------------------
# Hysteria2 配置写入 (awk 锚点注入，域名动态生成)
# ---------------------------------------------------------------------------
apply_hysteria() {
    local host="$1" port="$2" user="$3" pass="$4"
    local config="${HYSTERIA_CONFIG}"
    local block_file tmp
    trap 'rm -f "${block_file:-}" "${tmp:-}"' RETURN

    if ! grep -q "# B-UI:RESIDENTIAL-START" "$config"; then
        printf '\n# B-UI:RESIDENTIAL-START\n# B-UI:RESIDENTIAL-END\n' >> "$config"
    fi

    local backup="${config}.bak.$(date +%Y%m%d%H%M%S)"
    cp "$config" "$backup"

    get_domains

    block_file=$(mktemp)
    {
        printf 'outbounds:\n'
        printf '  - name: residential\n'
        printf '    type: socks5\n'
        printf '    socks5:\n'
        printf '      addr: "%s:%s"\n' "$host" "$port"
        printf '      username: "%s"\n' "$user"
        printf '      password: "%s"\n' "$pass"
        printf '  - name: direct\n'
        printf '    type: direct\n'
        printf 'acl:\n'
        printf '  inline:\n'
        printf '    - direct(127.0.0.0/8)\n'
        printf '    - direct(10.0.0.0/8)\n'
        printf '    - direct(172.16.0.0/12)\n'
        printf '    - direct(192.168.0.0/16)\n'
        printf '    - direct(169.254.0.0/16)\n'
        for d in "${DOMAINS[@]}"; do
            printf '    - residential(*%s*)\n' "$d"
        done
        printf '    - direct(all)\n'
    } > "$block_file"

    tmp=$(mktemp)
    awk -v blockfile="$block_file" '
        /# B-UI:RESIDENTIAL-START/ { print; while((getline ln < blockfile) > 0) print ln; close(blockfile); skip=1; next }
        /# B-UI:RESIDENTIAL-END/   { skip=0 }
        !skip { print }
    ' "$config" > "$tmp" \
    && mv "$tmp" "$config" \
    || { cp "$backup" "$config"; err "Hysteria2 配置写入失败"; return 1; }
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
    local existing_domains
    existing_domains=$(jq '.domains // null' "${RESIDENTIAL_CONFIG}" 2>/dev/null || echo "null")

    jq -n \
        --argjson enabled "$enabled" \
        --argjson domains  "${existing_domains}" \
        --arg  host      "${RESI_HOST:-}" \
        --argjson port   "${RESI_PORT:-0}" \
        --arg  username  "${RESI_USER:-}" \
        --arg  password  "${RESI_PASS:-}" \
        --arg  lastVerifiedIp      "${RESI_EXIT_IP:-}" \
        --arg  lastVerifiedIspInfo "${RESI_ISP_INFO:-}" \
        --arg  lastVerifiedAt "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        '{enabled:$enabled,domains:$domains,host:$host,port:$port,username:$username,
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

    set-domains)
        [[ -z "${2:-}" ]] && { err "用法: $0 set-domains <json_array>"; exit 1; }
        echo "$2" | jq 'if type == "array" then . else error("not an array") end' >/dev/null 2>&1 \
            || { err "参数必须是 JSON 数组，如 [\"openai.com\",\"google.com\"]"; exit 1; }

        if [[ -f "${RESIDENTIAL_CONFIG}" ]]; then
            jq --argjson domains "$2" '.domains = $domains' \
               "${RESIDENTIAL_CONFIG}" > "${RESIDENTIAL_CONFIG}.tmp" \
            && mv "${RESIDENTIAL_CONFIG}.tmp" "${RESIDENTIAL_CONFIG}"
        else
            jq -n --argjson domains "$2" '{enabled:false,domains:$domains}' \
               > "${RESIDENTIAL_CONFIG}"
            chmod 600 "${RESIDENTIAL_CONFIG}"
        fi

        local_enabled=$(jq -r '.enabled // false' "${RESIDENTIAL_CONFIG}" 2>/dev/null || echo "false")
        if [[ "$local_enabled" == "true" ]]; then
            RESI_HOST=$(jq -r '.host'     "${RESIDENTIAL_CONFIG}")
            RESI_PORT=$(jq -r '.port'     "${RESIDENTIAL_CONFIG}")
            RESI_USER=$(jq -r '.username' "${RESIDENTIAL_CONFIG}")
            RESI_PASS=$(jq -r '.password' "${RESIDENTIAL_CONFIG}")
            apply_xray "$RESI_HOST" "$RESI_PORT" "$RESI_USER" "$RESI_PASS"
            apply_hysteria "$RESI_HOST" "$RESI_PORT" "$RESI_USER" "$RESI_PASS"
            reload_services
        fi
        ;;

    *)
        echo "Usage: $0 {enable <url>|disable|status|reapply|set-domains <json>}" >&2
        exit 1
        ;;
esac
