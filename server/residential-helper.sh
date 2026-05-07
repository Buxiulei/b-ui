#!/bin/bash
# residential-helper.sh — 住宅 IP 出站代理助手
#
# 架构：sing-box 作为永久本地出站中继 (127.0.0.1:2080)
#   开启住宅代理 → sing-box 按域名关键词分流，匹配域名走住宅 SOCKS5，其余直连
#   关闭住宅代理 → sing-box 全部直连，开销可忽略（loopback 转发）
#   Xray / Hysteria2 永远指向 sing-box，配置不随住宅代理开关变化
#
# Usage:
#   residential-helper.sh setup                → 初始化：配置 Xray/Hy2 永久走 sing-box（只需跑一次）
#   residential-helper.sh enable <url>         → 开启住宅代理，更新 sing-box 路由规则
#   residential-helper.sh disable              → 关闭住宅代理，sing-box 改为全直连
#   residential-helper.sh status               → 输出 residential-proxy.json
#   residential-helper.sh reapply             → 重新应用当前配置（update.sh 调用）
#   residential-helper.sh set-domains <json>   → 更新分流域名，重载 sing-box

set -euo pipefail

BASE_DIR="${BASE_DIR:-/opt/b-ui}"
RESIDENTIAL_CONFIG="${BASE_DIR}/residential-proxy.json"
XRAY_CONFIG="${BASE_DIR}/xray-config.json"
HYSTERIA_CONFIG="${BASE_DIR}/config.yaml"
SINGBOX_BIN="${BASE_DIR}/sing-box"
SINGBOX_CONFIG="${BASE_DIR}/singbox-relay.json"
SINGBOX_RELAY_PORT=2080
RELAY_SERVICE="b-ui-relay"

RED='\033[0;31m'; YELLOW='\033[1;33m'; BLUE='\033[0;34m'; NC='\033[0m'
err()  { echo -e "${RED}ERROR: $*${NC}" >&2; }
info() { echo -e "${BLUE}$*${NC}" >&2; }

DEFAULT_DOMAINS=("openai" "chatgpt" "google" "googleapis" "gstatic" "anthropic" "claude" "ping0")

# ---------------------------------------------------------------------------
# 读取分流域名列表 → 导出 DOMAINS 数组
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
# 下载 sing-box 二进制
# ---------------------------------------------------------------------------
ensure_singbox() {
    [[ -x "${SINGBOX_BIN}" ]] && return 0

    info "下载 sing-box..."
    local arch
    arch=$(uname -m)
    local arch_str
    case "$arch" in
        x86_64)  arch_str="linux-amd64" ;;
        aarch64) arch_str="linux-arm64" ;;
        armv7l)  arch_str="linux-armv7" ;;
        *) err "不支持的架构: $arch"; return 1 ;;
    esac

    local ver
    ver=$(curl -sS --max-time 15 \
        https://api.github.com/repos/SagerNet/sing-box/releases/latest \
        | grep '"tag_name"' | cut -d'"' -f4)
    [[ -z "$ver" ]] && { err "无法获取 sing-box 最新版本"; return 1; }

    local tarname="sing-box-${ver#v}-${arch_str}.tar.gz"
    curl -sS -L --max-time 120 \
        "https://github.com/SagerNet/sing-box/releases/download/${ver}/${tarname}" \
        | tar -xz -C "${BASE_DIR}" --wildcards "*/sing-box" --strip-components=1
    chmod +x "${SINGBOX_BIN}"
    [[ -x "${SINGBOX_BIN}" ]] || { err "sing-box 下载失败"; return 1; }
    info "sing-box ${ver} 就绪"
}

# ---------------------------------------------------------------------------
# sing-box 配置：住宅代理模式（按域名关键词分流 + DNS 路由）
# ---------------------------------------------------------------------------
write_singbox_config_residential() {
    local host="$1" port="$2" user="$3" pass="$4"

    get_domains
    local kw_json
    kw_json=$(printf '%s\n' "${DOMAINS[@]}" | jq -R . | jq -s '.')

    jq -n \
        --arg  host      "$host" \
        --argjson port   "$port" \
        --arg  user      "$user" \
        --arg  pass      "$pass" \
        --argjson relay_port "$SINGBOX_RELAY_PORT" \
        --argjson kw     "$kw_json" \
        '{
          "log": {"level": "error"},
          "dns": {
            "servers": [
              {"tag": "dns_resi",   "address": "udp://8.8.8.8", "detour": "residential"},
              {"tag": "dns_direct", "address": "udp://1.1.1.1",  "detour": "direct"}
            ],
            "rules": [{"domain_keyword": $kw, "server": "dns_resi"}],
            "final": "dns_direct",
            "strategy": "prefer_ipv4"
          },
          "inbounds": [{
            "type": "socks",
            "tag": "socks-in",
            "listen": "127.0.0.1",
            "listen_port": $relay_port,
            "sniff": true,
            "sniff_override_destination": true
          }],
          "outbounds": [
            {
              "type": "socks",
              "tag": "residential",
              "server": $host,
              "server_port": $port,
              "username": $user,
              "password": $pass,
              "version": "5"
            },
            {"type": "direct", "tag": "direct"}
          ],
          "route": {
            "rules": [
              {
                "ip_cidr": ["127.0.0.0/8","10.0.0.0/8","172.16.0.0/12",
                            "192.168.0.0/16","169.254.0.0/16"],
                "outbound": "direct"
              },
              {"domain_keyword": $kw, "outbound": "residential"}
            ],
            "final": "direct"
          }
        }' > "${SINGBOX_CONFIG}"
    chmod 600 "${SINGBOX_CONFIG}"
}

# ---------------------------------------------------------------------------
# sing-box 配置：直连模式（全部直连，无住宅代理）
# ---------------------------------------------------------------------------
write_singbox_config_direct() {
    jq -n \
        --argjson relay_port "$SINGBOX_RELAY_PORT" \
        '{
          "log": {"level": "error"},
          "inbounds": [{
            "type": "socks",
            "tag": "socks-in",
            "listen": "127.0.0.1",
            "listen_port": $relay_port,
            "sniff": true,
            "sniff_override_destination": true
          }],
          "outbounds": [{"type": "direct", "tag": "direct"}],
          "route": {"final": "direct"}
        }' > "${SINGBOX_CONFIG}"
    chmod 600 "${SINGBOX_CONFIG}"
}

# ---------------------------------------------------------------------------
# sing-box 服务管理
# ---------------------------------------------------------------------------
start_relay_service() {
    cat > /etc/systemd/system/${RELAY_SERVICE}.service <<EOF
[Unit]
Description=B-UI Outbound Relay (sing-box)
After=network.target

[Service]
Type=simple
ExecStart=${SINGBOX_BIN} run -c ${SINGBOX_CONFIG}
Restart=always
RestartSec=3
StandardOutput=null
StandardError=null

[Install]
WantedBy=multi-user.target
EOF
    systemctl daemon-reload
    systemctl enable --now "${RELAY_SERVICE}" 2>/dev/null || true
}

reload_relay_service() {
    if systemctl is-active --quiet "${RELAY_SERVICE}" 2>/dev/null; then
        systemctl restart "${RELAY_SERVICE}"
    else
        start_relay_service
    fi
}

stop_relay_service() {
    systemctl stop "${RELAY_SERVICE}" 2>/dev/null || true
    systemctl disable "${RELAY_SERVICE}" 2>/dev/null || true
    rm -f /etc/systemd/system/${RELAY_SERVICE}.service
    systemctl daemon-reload 2>/dev/null || true
}

# ---------------------------------------------------------------------------
# 获取服务器公网 IP（缓存到 _SERVER_IP，供 apply_xray/apply_hysteria 使用）
# ---------------------------------------------------------------------------
get_server_ip() {
    [[ -n "${_SERVER_IP:-}" ]] && return 0
    _SERVER_IP=$(curl -sS --max-time 5 https://api.ipify.org 2>/dev/null || \
                 curl -sS --max-time 5 https://ifconfig.me 2>/dev/null || true)
}

# ---------------------------------------------------------------------------
# Xray 配置：永久指向 sing-box（一次写入，不随住宅代理开关变化）
# ---------------------------------------------------------------------------
apply_xray() {
    get_server_ip

    local ip_direct_json
    if [[ -n "${_SERVER_IP:-}" ]]; then
        ip_direct_json=$(jq -n --arg ip "${_SERVER_IP}" '["geoip:private", ($ip + "/32")]')
    else
        ip_direct_json='["geoip:private"]'
    fi

    local outbounds
    outbounds=$(jq -n \
        --argjson relay_port "$SINGBOX_RELAY_PORT" \
        '[{"tag":"relay","protocol":"socks","settings":{"servers":[{
            "address":"127.0.0.1","port":$relay_port
          }]}},
          {"tag":"direct","protocol":"freedom"}]')

    local rules
    rules=$(jq -n --argjson ip_direct "$ip_direct_json" \
        '[
          {"type":"field","inboundTag":["api"],"outboundTag":"api"},
          {"type":"field","ip":$ip_direct,"outboundTag":"direct"},
          {"type":"field","outboundTag":"relay","network":"tcp,udp"}
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
# Hysteria2 配置：永久指向 sing-box（一次写入，不随住宅代理开关变化）
# ---------------------------------------------------------------------------
apply_hysteria() {
    local config="${HYSTERIA_CONFIG}"
    local block_file tmp
    trap 'rm -f "${block_file:-}" "${tmp:-}"' RETURN

    if ! grep -q "# B-UI:RESIDENTIAL-START" "$config"; then
        printf '\n# B-UI:RESIDENTIAL-START\n# B-UI:RESIDENTIAL-END\n' >> "$config"
    fi

    local backup="${config}.bak.$(date +%Y%m%d%H%M%S)"
    cp "$config" "$backup"

    get_server_ip

    block_file=$(mktemp)
    {
        printf 'outbounds:\n'
        printf '  - name: relay\n'
        printf '    type: socks5\n'
        printf '    socks5:\n'
        printf '      addr: "127.0.0.1:%s"\n' "$SINGBOX_RELAY_PORT"
        printf '  - name: direct\n'
        printf '    type: direct\n'
        printf 'acl:\n'
        printf '  inline:\n'
        printf '    - direct(127.0.0.0/8)\n'
        printf '    - direct(10.0.0.0/8)\n'
        printf '    - direct(172.16.0.0/12)\n'
        printf '    - direct(192.168.0.0/16)\n'
        printf '    - direct(169.254.0.0/16)\n'
        [[ -n "${_SERVER_IP:-}" ]] && printf '    - direct(%s)\n' "${_SERVER_IP}"
        printf '    - relay(all)\n'
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
    setup)
        # 初始化：下载 sing-box，配置 Xray/Hysteria2 永久走 sing-box
        # 根据当前 residential-proxy.json 状态写入对应的 sing-box 配置
        ensure_singbox
        if [[ -f "${RESIDENTIAL_CONFIG}" ]] && \
           [[ "$(jq -r '.enabled' "${RESIDENTIAL_CONFIG}" 2>/dev/null)" == "true" ]]; then
            RESI_HOST=$(jq -r '.host'     "${RESIDENTIAL_CONFIG}")
            RESI_PORT=$(jq -r '.port'     "${RESIDENTIAL_CONFIG}")
            RESI_USER=$(jq -r '.username' "${RESIDENTIAL_CONFIG}")
            RESI_PASS=$(jq -r '.password' "${RESIDENTIAL_CONFIG}")
            write_singbox_config_residential "$RESI_HOST" "$RESI_PORT" "$RESI_USER" "$RESI_PASS"
            info "sing-box 配置：住宅代理模式"
        else
            write_singbox_config_direct
            info "sing-box 配置：直连模式"
        fi
        start_relay_service
        apply_xray
        apply_hysteria
        reload_services
        info "sing-box 中继已就绪，Xray/Hysteria2 永久指向 127.0.0.1:${SINGBOX_RELAY_PORT}"
        ;;

    enable)
        [[ -z "${2:-}" ]] && { err "用法: $0 enable <socks5_url>"; exit 1; }
        parse_url "$2"
        verify "$RESI_HOST" "$RESI_PORT" "$RESI_USER" "$RESI_PASS"
        ensure_singbox
        write_singbox_config_residential "$RESI_HOST" "$RESI_PORT" "$RESI_USER" "$RESI_PASS"
        reload_relay_service
        save_config true
        echo "$RESI_EXIT_IP"
        echo "${RESI_ISP_INFO:-}"
        ;;

    disable)
        ensure_singbox
        write_singbox_config_direct
        reload_relay_service
        RESI_HOST="" RESI_PORT=0 RESI_USER="" RESI_PASS="" RESI_EXIT_IP="" RESI_ISP_INFO=""
        if [[ -f "${RESIDENTIAL_CONFIG}" ]]; then
            jq '.enabled = false | .host = "" | .username = "" | .password = "" |
                .lastVerifiedIp = "" | .lastVerifiedIspInfo = ""' \
                "${RESIDENTIAL_CONFIG}" > "${RESIDENTIAL_CONFIG}.tmp" \
                && mv "${RESIDENTIAL_CONFIG}.tmp" "${RESIDENTIAL_CONFIG}"
        fi
        ;;

    status)
        if [[ -f "${RESIDENTIAL_CONFIG}" ]]; then
            cat "${RESIDENTIAL_CONFIG}"
        else
            echo '{"enabled":false}'
        fi
        ;;

    reapply)
        # update.sh 更新后调用：确保 sing-box 运行，Xray/Hysteria2 配置正确
        ensure_singbox
        if [[ -f "${RESIDENTIAL_CONFIG}" ]] && \
           [[ "$(jq -r '.enabled' "${RESIDENTIAL_CONFIG}" 2>/dev/null)" == "true" ]]; then
            RESI_HOST=$(jq -r '.host'     "${RESIDENTIAL_CONFIG}")
            RESI_PORT=$(jq -r '.port'     "${RESIDENTIAL_CONFIG}")
            RESI_USER=$(jq -r '.username' "${RESIDENTIAL_CONFIG}")
            RESI_PASS=$(jq -r '.password' "${RESIDENTIAL_CONFIG}")
            write_singbox_config_residential "$RESI_HOST" "$RESI_PORT" "$RESI_USER" "$RESI_PASS"
        else
            write_singbox_config_direct
        fi
        start_relay_service
        apply_xray
        apply_hysteria
        reload_services
        ;;

    set-domains)
        [[ -z "${2:-}" ]] && { err "用法: $0 set-domains <json_array>"; exit 1; }
        echo "$2" | jq 'if type == "array" then . else error("not an array") end' >/dev/null 2>&1 \
            || { err "参数必须是 JSON 数组，如 [\"openai\",\"google\"]"; exit 1; }

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
            write_singbox_config_residential "$RESI_HOST" "$RESI_PORT" "$RESI_USER" "$RESI_PASS"
            reload_relay_service
        fi
        ;;

    *)
        echo "Usage: $0 {setup|enable <url>|disable|status|reapply|set-domains <json>}" >&2
        exit 1
        ;;
esac
