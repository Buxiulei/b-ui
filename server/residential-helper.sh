#!/bin/bash
# residential-helper.sh — 住宅 IP 出站代理助手 (v3.5.0)
#
# 架构：sing-box 作为永久本地出站中继 (127.0.0.1:2080)
#   b-ui-relay (sing-box) 永远运行，singbox-relay.json 决定路由行为
#   住宅 URL 池非空 → urltest resi-pool → 选最优住宅出口
#   住宅 URL 池空   → 全部直连（hy2-resi / vless-residential fallback 直连）
#   global=ON  → 池有效时 final 强制走 resi-pool（全流量住宅）
#   global=OFF → 池有效时按 domain_keyword 分流（AI 域名走住宅，其余直连）
#   Xray / Hysteria2 配置由 core.sh 一次写定，此脚本不再重写它们
#
# Usage:
#   residential-helper.sh setup                  → 初始化：启动 b-ui-relay sing-box 服务
#   residential-helper.sh enable <url>           → 开启住宅代理（单 URL，覆盖现有）
#   residential-helper.sh enable --add <url>     → 新增一个住宅 URL（多 URL 模式）
#   residential-helper.sh enable --remove <url>  → 移除一个住宅 URL
#   residential-helper.sh disable                → 关闭住宅代理，sing-box 改为空池直连
#   residential-helper.sh status                 → 输出 residential-proxy.json
#   residential-helper.sh reapply                → 重新应用当前配置（update.sh 调用）
#   residential-helper.sh set-domains <json>     → 更新分流域名，重载 sing-box
#   residential-helper.sh global on|off          → 切换全局/分流模式，重载 sing-box

set -euo pipefail

BASE_DIR="${BASE_DIR:-/opt/b-ui}"
RESIDENTIAL_CONFIG="${BASE_DIR}/residential-proxy.json"
SINGBOX_BIN="${BASE_DIR}/sing-box"
SINGBOX_CONFIG="${BASE_DIR}/singbox-relay.json"
SINGBOX_RELAY_PORT=2080
RELAY_SERVICE="b-ui-relay"

PRIVATE_CIDRS='["127.0.0.0/8","10.0.0.0/8","172.16.0.0/12","192.168.0.0/16","169.254.0.0/16"]'

RED='\033[0;31m'; YELLOW='\033[1;33m'; BLUE='\033[0;34m'; NC='\033[0m'
err()  { echo -e "${RED}ERROR: $*${NC}" >&2; }
info() { echo -e "${BLUE}$*${NC}" >&2; }

DEFAULT_DOMAINS=(
    "openai" "chatgpt" "oai" "oaistatic"
    "anthropic" "claude"
    "aistudio" "generativelanguage" "makersuite"
    "grok" "githubcopilot" "cursor" "perplexity"
    "mistral" "cohere" "huggingface" "replicate" "together" "groq"
    "statsig" "featuregates"
    "ping0" "ip.sb" "ip-api"
    "tiktok"
    "cloudcode" "antigravity"
    "gstatic" "ggpht" "googleapis" "googleusercontent"
)

LEGACY_DEFAULT_DOMAINS_V3_4_17=(
    "openai" "chatgpt" "google" "googleapis" "gstatic"
    "anthropic" "claude" "ping0" "grok" "tiktok"
)

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

verify() {
    local host="$1" port="$2" user="$3" pass="$4"

    info "获取 VPS 公网 IP..."
    _SERVER_IP=$(curl -sS --max-time 5 https://api.ipify.org 2>/dev/null) \
        || { err "无法获取 VPS 公网 IP"; return 1; }
    _SERVER_IP_FETCHED=1
    local vps_ip="$_SERVER_IP"

    info "通过 SOCKS5 测试出口..."
    local exit_ip
    exit_ip=$(curl -sS --max-time 10 \
        --socks5-hostname "${user}:${pass}@${host}:${port}" \
        https://api.ipify.org 2>/dev/null) \
        || { err "连接住宅代理失败 (${host}:${port})"; return 1; }

    [[ "$exit_ip" == "$vps_ip" ]] \
        && { err "出口 IP 与 VPS 相同 (${vps_ip})，代理未生效"; return 1; }

    RESI_EXIT_IP="$exit_ip"
    RESI_ISP_INFO=$(curl -sS --max-time 5 "https://ipinfo.io/${exit_ip}/json" 2>/dev/null \
        | jq -r '((.org // "") + ", " + (.city // "") + ", " + (.country // "")) | gsub("null"; "")' \
        2>/dev/null || echo "")
}

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

get_server_ip() {
    [[ "${_SERVER_IP_FETCHED:-}" == "1" ]] && return 0
    _SERVER_IP=$(curl -sS --max-time 5 https://api.ipify.org 2>/dev/null || \
                 curl -sS --max-time 5 https://ifconfig.me 2>/dev/null || true)
    _SERVER_IP_FETCHED=1
}

write_singbox_config_residential() {
    local host="$1" port="$2" user="$3" pass="$4"
    local urls_json
    urls_json=$(jq -n \
        --arg  host "$host" --argjson port "$port" \
        --arg  user "$user" --arg pass "$pass" \
        '[{host:$host, port:$port, username:$user, password:$pass, name:"primary"}]')
    write_singbox_config_residential_multi "$urls_json"
}

# v3.5.0: 多 URL urltest 池 + global toggle 支持
write_singbox_config_residential_multi() {
    local urls_json="$1"

    get_server_ip
    get_domains
    local kw_json
    kw_json=$(printf '%s\n' "${DOMAINS[@]}" | jq -R . | jq -s '. | unique')

    local is_global="false"
    if [[ -f "${RESIDENTIAL_CONFIG}" ]]; then
        is_global=$(jq -r '.global // false' "${RESIDENTIAL_CONFIG}" 2>/dev/null || echo "false")
    fi

    local outbounds_resi outbound_tags
    outbounds_resi=$(echo "$urls_json" | jq '
      to_entries | map({
        type: "socks",
        tag: ("resi-" + ((.key + 1) | tostring)),
        server: .value.host,
        server_port: (.value.port | tonumber),
        username: .value.username,
        password: .value.password,
        version: "5"
      })')
    outbound_tags=$(echo "$urls_json" | jq '
      to_entries | map("resi-" + ((.key + 1) | tostring))')

    # global=true:  final → resi-pool（全部走住宅），无域名分流规则
    # global=false: final → direct，domain_keyword 命中时走 resi-pool
    jq -n \
        --argjson outbounds_resi "$outbounds_resi" \
        --argjson outbound_tags  "$outbound_tags" \
        --argjson relay_port "$SINGBOX_RELAY_PORT" \
        --argjson kw      "$kw_json" \
        --arg  server_ip  "${_SERVER_IP:-}" \
        --argjson private  "$PRIVATE_CIDRS" \
        --argjson is_global "$is_global" \
        '{
          "log": {"level": "error"},
          "dns": {
            "servers": [
              {"tag": "dns_resi",   "type": "udp", "server": "8.8.8.8", "detour": "resi-pool"},
              {"tag": "dns_direct", "type": "udp", "server": "1.1.1.1"}
            ],
            "rules": (if $is_global then []
                      else [{"domain_keyword": $kw, "server": "dns_resi"}]
                      end),
            "final": (if $is_global then "dns_resi" else "dns_direct" end),
            "strategy": "prefer_ipv4"
          },
          "inbounds": [{
            "type": "socks",
            "tag": "socks-in",
            "listen": "127.0.0.1",
            "listen_port": $relay_port
          }],
          "outbounds": (
            $outbounds_resi
            + [{
                "type": "urltest",
                "tag": "resi-pool",
                "outbounds": $outbound_tags,
                "url": "https://www.gstatic.com/generate_204",
                "interval": "30s",
                "tolerance": 50
              },
              {"type": "direct", "tag": "direct"}]
          ),
          "route": {
            "rules": (
              [{"action": "sniff"},
               {"ip_cidr": ($private + (if $server_ip != "" then [($server_ip + "/32")] else [] end)),
                "outbound": "direct"}]
              + (if $is_global then []
                 else [{"domain_keyword": $kw, "outbound": "resi-pool"}]
                 end)
            ),
            "final": (if $is_global then "resi-pool" else "direct" end),
            "default_domain_resolver": "dns_direct"
          }
        }' > "${SINGBOX_CONFIG}"
    chmod 600 "${SINGBOX_CONFIG}"
}

# 直连模式（空池或 disable）
write_singbox_config_direct() {
    get_server_ip

    jq -n \
        --argjson relay_port "$SINGBOX_RELAY_PORT" \
        --arg  server_ip  "${_SERVER_IP:-}" \
        --argjson private  "$PRIVATE_CIDRS" \
        '{
          "log": {"level": "error"},
          "dns": {
            "servers": [
              {"tag": "dns_direct", "type": "udp", "server": "1.1.1.1"}
            ],
            "final": "dns_direct",
            "strategy": "prefer_ipv4"
          },
          "inbounds": [{
            "type": "socks",
            "tag": "socks-in",
            "listen": "127.0.0.1",
            "listen_port": $relay_port
          }],
          "outbounds": [{"type": "direct", "tag": "direct"}],
          "route": {
            "rules": [
              {"action": "sniff"},
              {
                "ip_cidr": ($private + (if $server_ip != "" then [($server_ip + "/32")] else [] end)),
                "outbound": "direct"
              }
            ],
            "final": "direct",
            "default_domain_resolver": "dns_direct"
          }
        }' > "${SINGBOX_CONFIG}"
    chmod 600 "${SINGBOX_CONFIG}"
}

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
LogRateLimitIntervalSec=10s
LogRateLimitBurst=200

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

save_config() {
    local enabled="$1"
    local existing_domains existing_urls existing_global
    existing_domains=$(jq '.domains // null' "${RESIDENTIAL_CONFIG}" 2>/dev/null || echo "null")
    existing_urls=$(jq '.urls // []'         "${RESIDENTIAL_CONFIG}" 2>/dev/null || echo "[]")
    existing_global=$(jq '.global // false'  "${RESIDENTIAL_CONFIG}" 2>/dev/null || echo "false")

    jq -n \
        --argjson enabled  "$enabled" \
        --argjson global   "${existing_global}" \
        --argjson domains  "${existing_domains}" \
        --argjson urls     "${existing_urls}" \
        --arg  host      "${RESI_HOST:-}" \
        --argjson port   "${RESI_PORT:-0}" \
        --arg  username  "${RESI_USER:-}" \
        --arg  password  "${RESI_PASS:-}" \
        --arg  lastVerifiedIp      "${RESI_EXIT_IP:-}" \
        --arg  lastVerifiedIspInfo "${RESI_ISP_INFO:-}" \
        --arg  lastVerifiedAt "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        '{enabled:$enabled,global:$global,domains:$domains,urls:$urls,host:$host,port:$port,
          username:$username,password:$password,lastVerifiedIp:$lastVerifiedIp,
          lastVerifiedIspInfo:$lastVerifiedIspInfo,lastVerifiedAt:$lastVerifiedAt}' \
        > "${RESIDENTIAL_CONFIG}"
    chmod 600 "${RESIDENTIAL_CONFIG}"
}

load_credentials_from_config() {
    RESI_HOST=$(jq -r '.host'     "${RESIDENTIAL_CONFIG}")
    RESI_PORT=$(jq -r '.port'     "${RESIDENTIAL_CONFIG}")
    RESI_USER=$(jq -r '.username' "${RESIDENTIAL_CONFIG}")
    RESI_PASS=$(jq -r '.password' "${RESIDENTIAL_CONFIG}")
}

build_urls_json_from_config() {
    local urls_count
    urls_count=$(jq '.urls // [] | length' "${RESIDENTIAL_CONFIG}" 2>/dev/null || echo 0)
    if [[ "$urls_count" -gt 0 ]]; then
        jq '.urls' "${RESIDENTIAL_CONFIG}"
        return 0
    fi
    local h p u pw
    h=$(jq -r '.host // ""'     "${RESIDENTIAL_CONFIG}")
    p=$(jq -r '.port // 0'      "${RESIDENTIAL_CONFIG}")
    u=$(jq -r '.username // ""' "${RESIDENTIAL_CONFIG}")
    pw=$(jq -r '.password // ""' "${RESIDENTIAL_CONFIG}")
    if [[ -z "$h" || "$p" == "0" ]]; then
        echo "[]"
        return 0
    fi
    jq -n --arg h "$h" --argjson p "$p" --arg u "$u" --arg pw "$pw" \
        '[{host:$h, port:$p, username:$u, password:$pw, name:"primary"}]'
}

write_singbox_config_from_state() {
    if [[ -f "${RESIDENTIAL_CONFIG}" ]] && \
       [[ "$(jq -r '.enabled' "${RESIDENTIAL_CONFIG}" 2>/dev/null)" == "true" ]]; then
        local urls_json
        urls_json=$(build_urls_json_from_config)
        local cnt
        cnt=$(echo "$urls_json" | jq 'length')
        if [[ "$cnt" -gt 0 ]]; then
            local is_global
            is_global=$(jq -r '.global // false' "${RESIDENTIAL_CONFIG}" 2>/dev/null || echo "false")
            write_singbox_config_residential_multi "$urls_json"
            info "sing-box 配置：住宅代理模式（${cnt} 个 URL，global=${is_global}）"
        else
            write_singbox_config_direct
            info "sing-box 配置：直连模式（urls 为空）"
        fi
    else
        write_singbox_config_direct
        info "sing-box 配置：直连模式"
    fi
}

add_url_to_config() {
    local host="$1" port="$2" user="$3" pass="$4"
    if [[ ! -f "${RESIDENTIAL_CONFIG}" ]]; then
        echo '{"enabled":false,"global":false,"urls":[]}' > "${RESIDENTIAL_CONFIG}"
        chmod 600 "${RESIDENTIAL_CONFIG}"
    fi
    local next_n
    next_n=$(jq '(.urls // []) | length + 1' "${RESIDENTIAL_CONFIG}")

    jq --arg h "$host" --argjson p "$port" --arg u "$user" --arg pw "$pass" --arg n "url-${next_n}" \
        '.urls = ((.urls // []) | map(select(.host != $h or .port != $p)) + [{host:$h, port:$p, username:$u, password:$pw, name:$n}])' \
        "${RESIDENTIAL_CONFIG}" > "${RESIDENTIAL_CONFIG}.tmp" \
    && mv "${RESIDENTIAL_CONFIG}.tmp" "${RESIDENTIAL_CONFIG}"
    chmod 600 "${RESIDENTIAL_CONFIG}"
}

remove_url_from_config() {
    local host="$1" port="$2"
    [[ -f "${RESIDENTIAL_CONFIG}" ]] || return 0
    jq --arg h "$host" --argjson p "$port" \
        '.urls = ((.urls // []) | map(select(.host != $h or .port != $p)))' \
        "${RESIDENTIAL_CONFIG}" > "${RESIDENTIAL_CONFIG}.tmp" \
    && mv "${RESIDENTIAL_CONFIG}.tmp" "${RESIDENTIAL_CONFIG}"
    chmod 600 "${RESIDENTIAL_CONFIG}"
}

# ---------------------------------------------------------------------------
# 主入口
# ---------------------------------------------------------------------------
cmd="${1:-}"
case "$cmd" in
    setup)
        ensure_singbox
        write_singbox_config_from_state
        start_relay_service
        info "sing-box 中继已就绪，b-ui-relay 监听 127.0.0.1:${SINGBOX_RELAY_PORT}"
        ;;

    enable)
        if [[ "${2:-}" == "--add" ]]; then
            [[ -z "${3:-}" ]] && { err "用法: $0 enable --add <socks5_url>"; exit 1; }
            parse_url "$3"
            verify "$RESI_HOST" "$RESI_PORT" "$RESI_USER" "$RESI_PASS"
            ensure_singbox
            add_url_to_config "$RESI_HOST" "$RESI_PORT" "$RESI_USER" "$RESI_PASS"
            save_config true
            write_singbox_config_from_state
            reload_relay_service
            total=$(jq '(.urls // []) | length' "${RESIDENTIAL_CONFIG}")
            info "已新增 URL（共 ${total} 个住宅出口）"
            echo "$RESI_EXIT_IP"
            echo "${RESI_ISP_INFO:-}"
            exit 0
        elif [[ "${2:-}" == "--remove" ]]; then
            [[ -z "${3:-}" ]] && { err "用法: $0 enable --remove <socks5_url>"; exit 1; }
            parse_url "$3"
            ensure_singbox
            remove_url_from_config "$RESI_HOST" "$RESI_PORT"
            total=$(jq '(.urls // []) | length' "${RESIDENTIAL_CONFIG}" 2>/dev/null || echo 0)
            if [[ "$total" -eq 0 ]]; then
                jq '.enabled = false' "${RESIDENTIAL_CONFIG}" > "${RESIDENTIAL_CONFIG}.tmp" \
                  && mv "${RESIDENTIAL_CONFIG}.tmp" "${RESIDENTIAL_CONFIG}"
                write_singbox_config_direct
                info "最后一个 URL 已移除，住宅代理已禁用"
            else
                write_singbox_config_from_state
                info "已移除 URL（剩余 ${total} 个住宅出口）"
            fi
            reload_relay_service
            exit 0
        fi

        [[ -z "${2:-}" ]] && { err "用法: $0 enable <socks5_url>"; exit 1; }
        parse_url "$2"
        verify "$RESI_HOST" "$RESI_PORT" "$RESI_USER" "$RESI_PASS"
        ensure_singbox
        write_singbox_config_residential "$RESI_HOST" "$RESI_PORT" "$RESI_USER" "$RESI_PASS"
        reload_relay_service
        if [[ -f "${RESIDENTIAL_CONFIG}" ]]; then
            jq '.urls = []' "${RESIDENTIAL_CONFIG}" > "${RESIDENTIAL_CONFIG}.tmp" 2>/dev/null \
              && mv "${RESIDENTIAL_CONFIG}.tmp" "${RESIDENTIAL_CONFIG}" || true
        fi
        save_config true
        echo "$RESI_EXIT_IP"
        echo "${RESI_ISP_INFO:-}"
        ;;

    disable)
        # v3.5.0: 不停 b-ui-relay 服务，写空池直连配置 → hy2-resi/vless-resi fallback 直连
        ensure_singbox
        write_singbox_config_direct
        reload_relay_service
        if [[ -f "${RESIDENTIAL_CONFIG}" ]]; then
            jq '.enabled = false | .host = "" | .username = "" | .password = "" |
                .urls = [] |
                .lastVerifiedIp = "" | .lastVerifiedIspInfo = ""' \
                "${RESIDENTIAL_CONFIG}" > "${RESIDENTIAL_CONFIG}.tmp" \
                && mv "${RESIDENTIAL_CONFIG}.tmp" "${RESIDENTIAL_CONFIG}"
        fi
        info "住宅代理已关闭，b-ui-relay 继续运行（直连模式）"
        ;;

    status)
        if [[ -f "${RESIDENTIAL_CONFIG}" ]]; then
            cat "${RESIDENTIAL_CONFIG}"
        else
            echo '{"enabled":false,"global":false}'
        fi
        ;;

    reapply)
        ensure_singbox
        write_singbox_config_from_state
        start_relay_service
        ;;

    set-domains)
        [[ -z "${2:-}" ]] && { err "用法: $0 set-domains <json_array>"; exit 1; }
        echo "$2" | jq 'if type == "array" then . else error("not an array") end' >/dev/null 2>&1 \
            || { err "参数必须是 JSON 数组"; exit 1; }

        if [[ -f "${RESIDENTIAL_CONFIG}" ]]; then
            jq --argjson domains "$2" '.domains = $domains' \
               "${RESIDENTIAL_CONFIG}" > "${RESIDENTIAL_CONFIG}.tmp" \
            && mv "${RESIDENTIAL_CONFIG}.tmp" "${RESIDENTIAL_CONFIG}"
        else
            jq -n --argjson domains "$2" '{enabled:false,global:false,domains:$domains}' \
               > "${RESIDENTIAL_CONFIG}"
            chmod 600 "${RESIDENTIAL_CONFIG}"
        fi

        local_enabled=$(jq -r '.enabled // false' "${RESIDENTIAL_CONFIG}" 2>/dev/null || echo "false")
        if [[ "$local_enabled" == "true" ]]; then
            write_singbox_config_from_state
            reload_relay_service
        fi
        ;;

    global)
        # v3.5.0: 切换全局/分流模式
        local_val="${2:-}"
        case "$local_val" in
            on|ON)   new_global="true"  ;;
            off|OFF) new_global="false" ;;
            *) err "用法: $0 global on|off"; exit 1 ;;
        esac

        if [[ -f "${RESIDENTIAL_CONFIG}" ]]; then
            jq --argjson g "$new_global" '.global = $g' \
               "${RESIDENTIAL_CONFIG}" > "${RESIDENTIAL_CONFIG}.tmp" \
            && mv "${RESIDENTIAL_CONFIG}.tmp" "${RESIDENTIAL_CONFIG}"
        else
            jq -n --argjson g "$new_global" '{"enabled":false,"global":$g}' \
               > "${RESIDENTIAL_CONFIG}"
            chmod 600 "${RESIDENTIAL_CONFIG}"
        fi

        write_singbox_config_from_state
        reload_relay_service
        info "global 模式已设置为: ${local_val}"
        ;;

    *)
        echo "Usage: $0 {setup|enable <url>|disable|status|reapply|set-domains <json>|global on|off}" >&2
        exit 1
        ;;
esac
