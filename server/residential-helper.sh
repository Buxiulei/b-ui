#!/bin/bash
# residential-helper.sh — 住宅 IP 出站代理助手
#
# 架构：sing-box 作为永久本地出站中继 (127.0.0.1:2080)
#   开启住宅代理 → sing-box 按域名关键词分流，匹配域名走住宅 SOCKS5，其余直连
#   关闭住宅代理 → sing-box 全部直连，开销可忽略（loopback 转发）
#   Xray / Hysteria2 永远指向 sing-box，路由和 DNS 由 sing-box 统一负责
#
# Usage:
#   residential-helper.sh setup                  → 初始化：配置 Xray/Hy2 永久走 sing-box（只需跑一次）
#   residential-helper.sh enable <url>           → 开启住宅代理（单 URL，覆盖现有）
#   residential-helper.sh enable --add <url>     → 新增一个住宅 URL（v3.4.19 多 URL）
#   residential-helper.sh enable --remove <url>  → 移除一个住宅 URL（v3.4.19 多 URL）
#   residential-helper.sh disable                → 关闭住宅代理，sing-box 改为全直连
#   residential-helper.sh status                 → 输出 residential-proxy.json
#   residential-helper.sh reapply                → 重新应用当前配置（update.sh 调用）
#   residential-helper.sh set-domains <json>     → 更新分流域名，重载 sing-box

set -euo pipefail

BASE_DIR="${BASE_DIR:-/opt/b-ui}"
RESIDENTIAL_CONFIG="${BASE_DIR}/residential-proxy.json"
XRAY_CONFIG="${BASE_DIR}/xray-config.json"
HYSTERIA_CONFIG="${BASE_DIR}/config.yaml"
SINGBOX_BIN="${BASE_DIR}/sing-box"
SINGBOX_CONFIG="${BASE_DIR}/singbox-relay.json"
SINGBOX_RELAY_PORT=2080
RELAY_SERVICE="b-ui-relay"

# sing-box 直连规则中的私有/保留 CIDR（两个配置函数共用）
PRIVATE_CIDRS='["127.0.0.0/8","10.0.0.0/8","172.16.0.0/12","192.168.0.0/16","169.254.0.0/16"]'

RED='\033[0;31m'; YELLOW='\033[1;33m'; BLUE='\033[0;34m'; NC='\033[0m'
err()  { echo -e "${RED}ERROR: $*${NC}" >&2; }
info() { echo -e "${BLUE}$*${NC}" >&2; }

# 默认分流关键词（v3.4.18 起）
# 设计原则：
#   1) 只针对 AI / 强风控站点，不大面积匹配 google/gstatic（避全站误伤 + 避 urltest 自循环）
#   2) statsig/featuregates 是 OpenAI/Anthropic 共用的 telemetry / feature flag 域名，必须随主站走
#   3) ping0 / ip.sb 用于用户验证流量是否真的从住宅出去
# 注意：旧默认含 "google" "googleapis" "gstatic"，已被精化移除（迁移见 update.sh）
DEFAULT_DOMAINS=(
    "openai" "chatgpt" "oai" "oaistatic"
    "anthropic" "claude"
    "aistudio" "generativelanguage" "gemini.google" "makersuite"
    "grok" "githubcopilot" "cursor" "perplexity"
    "mistral" "cohere" "huggingface" "replicate" "together" "groq"
    "statsig" "featuregates"
    "ping0" "ip.sb"
    "tiktok"
)

# 旧默认列表（v3.4.17 及之前）—— 用于 update.sh 判断"用户从未自定义"
LEGACY_DEFAULT_DOMAINS_V3_4_17=(
    "openai" "chatgpt" "google" "googleapis" "gstatic"
    "anthropic" "claude" "ping0" "grok" "tiktok"
)

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
    # 顺便填充 _SERVER_IP 缓存，避免后续 get_server_ip 重复请求
    _SERVER_IP=$(curl -sS --max-time 5 https://api.ipify.org 2>/dev/null) \
        || { err "无法获取 VPS 公网 IP，请检查网络连接"; return 1; }
    _SERVER_IP_FETCHED=1
    local vps_ip="$_SERVER_IP"

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
# 获取服务器公网 IP（缓存到 _SERVER_IP，供 sing-box 配置使用）
# ---------------------------------------------------------------------------
get_server_ip() {
    [[ "${_SERVER_IP_FETCHED:-}" == "1" ]] && return 0
    _SERVER_IP=$(curl -sS --max-time 5 https://api.ipify.org 2>/dev/null || \
                 curl -sS --max-time 5 https://ifconfig.me 2>/dev/null || true)
    _SERVER_IP_FETCHED=1
}

# ---------------------------------------------------------------------------
# sing-box 配置：住宅代理模式（按域名关键词分流 + DNS 路由）
# sing-box 统一负责所有路由和 DNS 决策
#
# v3.4.19 起支持两种配置形态：
#   单 URL（旧）：write_singbox_config_residential <host> <port> <user> <pass>
#   多 URL（新）：write_singbox_config_residential_multi <urls_json>
#                urls_json 为 [{"host":"","port":N,"username":"","password":"","name":""},...]
# ---------------------------------------------------------------------------
write_singbox_config_residential() {
    local host="$1" port="$2" user="$3" pass="$4"
    # 包装成 1-元素 urls 数组复用 multi 实现
    local urls_json
    urls_json=$(jq -n \
        --arg  host "$host" \
        --argjson port "$port" \
        --arg  user "$user" \
        --arg  pass "$pass" \
        '[{host:$host, port:$port, username:$user, password:$pass, name:"primary"}]')
    write_singbox_config_residential_multi "$urls_json"
}

write_singbox_config_residential_multi() {
    local urls_json="$1"

    get_server_ip
    get_domains
    local kw_json
    kw_json=$(printf '%s\n' "${DOMAINS[@]}" | jq -R . | jq -s '.')

    # 多 URL outbound：每个 URL 一个 socks outbound，tag 为 residential-N
    # tag 名称用递增 N 避免 special 字符与重复
    local outbounds_resi outbound_tags
    outbounds_resi=$(echo "$urls_json" | jq '
      to_entries | map({
        type: "socks",
        tag: ("residential-" + ((.key + 1) | tostring)),
        server: .value.host,
        server_port: (.value.port | tonumber),
        username: .value.username,
        password: .value.password,
        version: "5"
      })')
    outbound_tags=$(echo "$urls_json" | jq '
      to_entries | map("residential-" + ((.key + 1) | tostring))')

    # 路由命中 tag "residential"（urltest），urltest 包含所有 residential-N。
    # urltest 探测 URL 必须不命中任何分流 keyword，否则探测流量被路由回 residential 形成自循环。
    jq -n \
        --argjson outbounds_resi "$outbounds_resi" \
        --argjson outbound_tags  "$outbound_tags" \
        --argjson relay_port "$SINGBOX_RELAY_PORT" \
        --argjson kw      "$kw_json" \
        --arg  server_ip  "${_SERVER_IP:-}" \
        --argjson private  "$PRIVATE_CIDRS" \
        '{
          "log": {"level": "error"},
          "dns": {
            "servers": [
              {"tag": "dns_resi",   "type": "udp", "server": "8.8.8.8", "detour": "residential"},
              {"tag": "dns_direct", "type": "udp", "server": "1.1.1.1"}
            ],
            "rules": [{"domain_keyword": $kw, "action": "route", "server": "dns_resi"}],
            "final": "dns_direct",
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
                "tag": "residential",
                "outbounds": $outbound_tags,
                "url": "https://cp.cloudflare.com/generate_204",
                "interval": "3m",
                "tolerance": 50,
                "idle_timeout": "30m"
              },
              {"type": "direct", "tag": "direct"}]
          ),
          "route": {
            "rules": [
              {"action": "sniff"},
              {
                "ip_cidr": ($private + (if $server_ip != "" then [($server_ip + "/32")] else [] end)),
                "outbound": "direct"
              },
              {"domain_keyword": $kw, "outbound": "residential"}
            ],
            "final": "direct",
            "default_domain_resolver": "dns_direct"
          }
        }' > "${SINGBOX_CONFIG}"
    chmod 600 "${SINGBOX_CONFIG}"
}

# ---------------------------------------------------------------------------
# sing-box 配置：直连模式（全部直连，无住宅代理）
# sing-box 统一负责所有路由和 DNS 决策
# ---------------------------------------------------------------------------
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

# ---------------------------------------------------------------------------
# sing-box 服务管理
# ---------------------------------------------------------------------------
start_relay_service() {
    # 日志策略：systemd 默认走 journal（不再 redirect 到 null，便于排查）
    # 加速率限防日志风暴：10s 窗口内最多 200 条
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

# ---------------------------------------------------------------------------
# Xray 配置：永久指向 sing-box，路由和 DNS 由 sing-box 统一负责
# ---------------------------------------------------------------------------
apply_xray() {
    local outbounds
    outbounds=$(jq -n \
        --argjson relay_port "$SINGBOX_RELAY_PORT" \
        '[{"tag":"relay","protocol":"socks","settings":{"servers":[{
            "address":"127.0.0.1","port":$relay_port
          }]}},
          {"tag":"direct","protocol":"freedom"}]')

    local rules
    rules='[
      {"type":"field","inboundTag":["api"],"outboundTag":"api"},
      {"type":"field","outboundTag":"relay","network":"tcp,udp"}
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
    local existing_domains existing_urls
    existing_domains=$(jq '.domains // null' "${RESIDENTIAL_CONFIG}" 2>/dev/null || echo "null")
    existing_urls=$(jq '.urls // []'    "${RESIDENTIAL_CONFIG}" 2>/dev/null || echo "[]")

    jq -n \
        --argjson enabled  "$enabled" \
        --argjson domains  "${existing_domains}" \
        --argjson urls     "${existing_urls}" \
        --arg  host      "${RESI_HOST:-}" \
        --argjson port   "${RESI_PORT:-0}" \
        --arg  username  "${RESI_USER:-}" \
        --arg  password  "${RESI_PASS:-}" \
        --arg  lastVerifiedIp      "${RESI_EXIT_IP:-}" \
        --arg  lastVerifiedIspInfo "${RESI_ISP_INFO:-}" \
        --arg  lastVerifiedAt "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        '{enabled:$enabled,domains:$domains,urls:$urls,host:$host,port:$port,username:$username,
          password:$password,lastVerifiedIp:$lastVerifiedIp,
          lastVerifiedIspInfo:$lastVerifiedIspInfo,lastVerifiedAt:$lastVerifiedAt}' \
        > "${RESIDENTIAL_CONFIG}"
    chmod 600 "${RESIDENTIAL_CONFIG}"
}

# 从 residential-proxy.json 加载凭据到 RESI_* 变量
load_credentials_from_config() {
    RESI_HOST=$(jq -r '.host'     "${RESIDENTIAL_CONFIG}")
    RESI_PORT=$(jq -r '.port'     "${RESIDENTIAL_CONFIG}")
    RESI_USER=$(jq -r '.username' "${RESIDENTIAL_CONFIG}")
    RESI_PASS=$(jq -r '.password' "${RESIDENTIAL_CONFIG}")
}

# v3.4.19 Cluster F: 获取住宅 URL 列表的 JSON 数组
# 优先使用新 schema 的 urls 字段；为空则降级用旧的 host/port/username/password
build_urls_json_from_config() {
    local urls_count
    urls_count=$(jq '.urls // [] | length' "${RESIDENTIAL_CONFIG}" 2>/dev/null || echo 0)
    if [[ "$urls_count" -gt 0 ]]; then
        jq '.urls' "${RESIDENTIAL_CONFIG}"
        return 0
    fi
    # 兼容旧 schema：把 host/port/username/password 包装成单元素 urls
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

# 根据当前 residential-proxy.json 状态写入对应的 sing-box 配置
write_singbox_config_from_state() {
    if [[ -f "${RESIDENTIAL_CONFIG}" ]] && \
       [[ "$(jq -r '.enabled' "${RESIDENTIAL_CONFIG}" 2>/dev/null)" == "true" ]]; then
        local urls_json
        urls_json=$(build_urls_json_from_config)
        local cnt
        cnt=$(echo "$urls_json" | jq 'length')
        if [[ "$cnt" -gt 0 ]]; then
            write_singbox_config_residential_multi "$urls_json"
            info "sing-box 配置：住宅代理模式（${cnt} 个 URL）"
        else
            write_singbox_config_direct
            info "sing-box 配置：直连模式（urls 为空）"
        fi
    else
        write_singbox_config_direct
        info "sing-box 配置：直连模式"
    fi
}

# v3.4.19 Cluster F: 添加 URL 到 urls 数组（去重 host:port）
add_url_to_config() {
    local host="$1" port="$2" user="$3" pass="$4"

    # 确保 RESIDENTIAL_CONFIG 存在
    if [[ ! -f "${RESIDENTIAL_CONFIG}" ]]; then
        echo '{"enabled":false,"urls":[]}' > "${RESIDENTIAL_CONFIG}"
        chmod 600 "${RESIDENTIAL_CONFIG}"
    fi

    # 计算下一个 name（递增 N，N 从 1 开始）
    local next_n
    next_n=$(jq '(.urls // []) | length + 1' "${RESIDENTIAL_CONFIG}")

    jq --arg h "$host" --argjson p "$port" --arg u "$user" --arg pw "$pass" --arg n "url-${next_n}" \
        '.urls = ((.urls // []) | map(select(.host != $h or .port != $p)) + [{host:$h, port:$p, username:$u, password:$pw, name:$n}])' \
        "${RESIDENTIAL_CONFIG}" > "${RESIDENTIAL_CONFIG}.tmp" \
    && mv "${RESIDENTIAL_CONFIG}.tmp" "${RESIDENTIAL_CONFIG}"
    chmod 600 "${RESIDENTIAL_CONFIG}"
}

# v3.4.19 Cluster F: 从 urls 数组移除（按 host:port 匹配）
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
        apply_xray
        apply_hysteria
        reload_services
        info "sing-box 中继已就绪，Xray/Hysteria2 永久指向 127.0.0.1:${SINGBOX_RELAY_PORT}"
        ;;

    enable)
        # v3.4.19 Cluster F: 多 URL 子操作（--add / --remove）
        if [[ "${2:-}" == "--add" ]]; then
            [[ -z "${3:-}" ]] && { err "用法: $0 enable --add <socks5_url>"; exit 1; }
            parse_url "$3"
            verify "$RESI_HOST" "$RESI_PORT" "$RESI_USER" "$RESI_PASS"
            ensure_singbox
            add_url_to_config "$RESI_HOST" "$RESI_PORT" "$RESI_USER" "$RESI_PASS"
            # 写 enable=true（保留旧 host/port 字段供向后兼容）
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
                # urls 空 → 直连模式
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

        # 单 URL 模式（旧行为，覆盖现有 host/port/...）
        [[ -z "${2:-}" ]] && { err "用法: $0 enable <socks5_url>"; exit 1; }
        parse_url "$2"
        verify "$RESI_HOST" "$RESI_PORT" "$RESI_USER" "$RESI_PASS"
        ensure_singbox
        write_singbox_config_residential "$RESI_HOST" "$RESI_PORT" "$RESI_USER" "$RESI_PASS"
        reload_relay_service
        # 单 URL 模式：清空 urls 数组（保持 schema 干净）
        if [[ -f "${RESIDENTIAL_CONFIG}" ]]; then
            jq '.urls = []' "${RESIDENTIAL_CONFIG}" > "${RESIDENTIAL_CONFIG}.tmp" 2>/dev/null \
              && mv "${RESIDENTIAL_CONFIG}.tmp" "${RESIDENTIAL_CONFIG}" || true
        fi
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
            # v3.4.19: 同时清空 urls
            jq '.enabled = false | .host = "" | .username = "" | .password = "" |
                .urls = [] |
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
        ensure_singbox
        write_singbox_config_from_state
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
            load_credentials_from_config
            write_singbox_config_residential "$RESI_HOST" "$RESI_PORT" "$RESI_USER" "$RESI_PASS"
            reload_relay_service
        fi
        ;;

    *)
        echo "Usage: $0 {setup|enable <url>|disable|status|reapply|set-domains <json>}" >&2
        exit 1
        ;;
esac
