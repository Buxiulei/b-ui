#!/bin/bash
# residential-helper.sh — 住宅 IP 出站代理助手 (v3.5.0)
#
# 架构：sing-box 作为永久本地出站中继 (127.0.0.1:2080)
#   b-ui-relay (sing-box) 永远运行，singbox-relay.json 决定路由行为
#   住宅 URL 池非空 → selector resi-pool → 粘住当前出口，由 resi-health.sh 经 Clash API 热切换
#   住宅 URL 池空   → 全部直连（hy2-resi / vless-residential fallback 直连）
#   global=ON  → 池有效时 final 强制走 resi-pool（全流量住宅）
#   global=OFF → 池有效时按 domain_keyword 分流（AI 域名走住宅，其余直连）
#   Xray / Hysteria2 配置由 core.sh 一次写定，此脚本不再重写它们
#
# v3.6.0 R10: 上游支持 SOCKS5 与 HTTP 两种协议。URL 可以是
#   socks5://user:pass@host:port / http://user:pass@host:port（显式类型）
#   host:port:user:pass / user:pass@host:port（自动探测：先 SOCKS5，失败再 HTTP）
# 探测结果记进 residential-proxy.json 的 type 字段（缺省=socks5），中继按 type 出 socks/http 出站。
#
# Usage:
#   residential-helper.sh setup                  → 初始化：启动 b-ui-relay sing-box 服务
#   residential-helper.sh enable <url>           → 开启住宅代理（单 URL，覆盖现有）
#   residential-helper.sh enable --add <url>     → 新增一个住宅 URL（多 URL 模式）
#   residential-helper.sh enable --add -         → 同上，URL 从 stdin 读一行（凭据不进 argv）
#   residential-helper.sh enable --remove <url>  → 移除一个住宅 URL（按 host:port 匹配，与协议无关）
#   residential-helper.sh disable                → 关闭住宅代理，sing-box 改为空池直连
#   residential-helper.sh status                 → 输出 residential-proxy.json
#   residential-helper.sh domains                → 输出生效分流域名关键字 (JSON 数组)
#   residential-helper.sh reapply                → 重新应用当前配置（update.sh 调用）
#   residential-helper.sh set-domains <json>     → 更新分流域名，重载 sing-box
#   residential-helper.sh global on|off          → 切换全局/分流模式，重载 sing-box

set -euo pipefail

BASE_DIR="${BASE_DIR:-/opt/b-ui}"
RESIDENTIAL_CONFIG="${BASE_DIR}/residential-proxy.json"
SINGBOX_BIN="${BASE_DIR}/sing-box"
SINGBOX_CONFIG="${BASE_DIR}/singbox-relay.json"
SINGBOX_RELAY_PORT=2080
SINGBOX_RELAY_API="127.0.0.1:9091"
RELAY_SERVICE="b-ui-relay"
RELAY_UNIT="${RELAY_UNIT_FILE:-/etc/systemd/system/${RELAY_SERVICE}.service}"
RELAY_LOCK="${BASE_DIR}/.relay.lock"

# v3.6.0 R2: 写路径互斥（同一把锁只在并发的 residential-helper 之间抢）；持锁到进程退出
acquire_relay_lock() {
    command -v flock >/dev/null 2>&1 || { err "缺少 flock (util-linux)，无法安全写入住宅配置"; exit 1; }
    exec 9>"${RELAY_LOCK}"
    chmod 600 "${RELAY_LOCK}" 2>/dev/null || true
    flock -w 30 9 || { err "获取 relay 锁超时(30s)，可能有另一个 residential-helper 在运行"; exit 1; }
}

PRIVATE_CIDRS='["127.0.0.0/8","10.0.0.0/8","172.16.0.0/12","192.168.0.0/16","169.254.0.0/16","::1/128","fc00::/7","fe80::/10"]'

RED='\033[0;31m'; YELLOW='\033[1;33m'; BLUE='\033[0;34m'; NC='\033[0m'
err()  { echo -e "${RED}ERROR: $*${NC}" >&2; }
info() { echo -e "${BLUE}$*${NC}" >&2; }

# v3.6.0: 文件摘要。文件不存在 → 空串（与任何真实摘要都不相等 → 视为有变化）；
# md5sum 不可用时每次返回不同的 token，让"拿不到摘要"退化成"当作有变化"——
# 宁可多重启一次，也不能因为两边都是空串而把真的配置变化判成"没变"。
file_digest() {
    [[ -f "$1" ]] || { echo ""; return 0; }
    local d
    d=$(md5sum "$1" 2>/dev/null | awk '{print $1}') || d=""
    [[ -n "$d" ]] && echo "$d" || echo "nodigest-$$-${RANDOM}"
}

# v3.6.0 R3: curl 配置文件双引号内需转义 \ 与 "
curl_cfg_escape() { printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g'; }
# 生成 curl -K - 的配置：proxy 行 + 可选 proxy-user 行（凭据不出现在 argv 里）
# v3.6.0 R10: scheme 由调用方给（socks5h / http），内部常量，不来自用户输入
curl_proxy_cfg() {
    local scheme="$1" host="$2" port="$3" user="$4" pass="$5"
    printf 'proxy = "%s://%s:%s"\n' "$scheme" "$(curl_cfg_escape "$host")" "$(curl_cfg_escape "$port")"
    [ -n "$user" ] && printf 'proxy-user = "%s:%s"\n' "$(curl_cfg_escape "$user")" "$(curl_cfg_escape "$pass")"
    return 0
}

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

# v3.6.0 R10: 四种格式 → RESI_HOST/PORT/USER/PASS + RESI_TYPE(socks5|http|auto)
# 供应商邮件与 IP 列表 CSV 常带首尾空白和成对引号，先剥掉
parse_url() {
    local raw="$1" url s
    url="${raw#"${raw%%[![:space:]]*}"}"; url="${url%"${url##*[![:space:]]}"}"
    case "$url" in
        \"*\") url="${url:1:${#url}-2}" ;;
        \'*\') url="${url:1:${#url}-2}" ;;
    esac
    url="${url#"${url%%[![:space:]]*}"}"; url="${url%"${url##*[![:space:]]}"}"

    RESI_TYPE="auto"
    s="$url"
    case "$url" in
        socks5://*) RESI_TYPE="socks5"; s="${url#socks5://}" ;;
        http://*)   RESI_TYPE="http";   s="${url#http://}"   ;;
    esac
    # https:// / socks4:// 等一律拒绝，别把 scheme 当成用户名静默解析
    if [[ "$s" =~ ^[A-Za-z][A-Za-z0-9+.-]*:// ]]; then
        err "不支持的代理协议 ${s%%://*}:// —— 只支持 socks5:// 与 http://"
        return 1
    fi

    # user:pass@host:port 以**最后一个** @ 切分（密码可含 @），且 @ 之后必须形如 host:port；
    # 否则按 host:port:user:pass 解析（CSV 形态的密码同样可以含 @）
    if [[ "$s" == *"@"* && "${s##*@}" =~ ^[^:@/]+:[0-9]+$ ]]; then
        local userpass="${s%@*}"
        local hostport="${s##*@}"
        [[ "$userpass" == *:* ]] || { err "凭据缺少密码，应为 user:pass@host:port"; return 1; }
        RESI_USER="${userpass%%:*}"
        RESI_PASS="${userpass#*:}"
        RESI_HOST="${hostport%%:*}"
        RESI_PORT="${hostport##*:}"
    elif [[ "$s" =~ ^([^:@]+):([0-9]+):([^:]+):(.+)$ ]]; then
        RESI_HOST="${BASH_REMATCH[1]}"
        RESI_PORT="${BASH_REMATCH[2]}"
        RESI_USER="${BASH_REMATCH[3]}"
        RESI_PASS="${BASH_REMATCH[4]}"
    else
        err "无法解析凭据格式。支持: socks5://user:pass@host:port、http://user:pass@host:port、host:port:user:pass、user:pass@host:port"
        return 1
    fi

    [[ -n "${RESI_HOST:-}" && -n "${RESI_PORT:-}" && -n "${RESI_USER:-}" && -n "${RESI_PASS:-}" ]] \
        || { err "解析结果包含空字段"; return 1; }
    [[ "$RESI_PORT" =~ ^[0-9]+$ ]] \
        || { err "端口必须是数字，实际: ${RESI_PORT}"; return 1; }
}

# v3.6.0 R10: want = socks5|http|auto。auto 先 SOCKS5 再 HTTP（Bright Data 22228=SOCKS5、
# 44445=HTTP，同一套凭据两种都可用），成功后 RESI_TYPE 是实际可用类型
verify() {
    local host="$1" port="$2" user="$3" pass="$4" want="${5:-auto}"

    # v3.6.0 R3: 换行无法安全写进 curl 配置文件（会注入额外指令），视为非法
    case "${user}${pass}" in
        *$'\n'*|*$'\r'*) err "凭据含换行符，非法"; return 1 ;;
    esac
    # host/port 直接进 proxy 行（凭据靠 curl_cfg_escape 转义即可，host 不给这个余地）
    case "$host" in
        ""|*$'\n'*|*$'\r'*|*'"'*|*'\'*) err "host/port 非法"; return 1 ;;
    esac
    [[ "$port" =~ ^[0-9]{1,5}$ ]] || { err "host/port 非法"; return 1; }

    info "获取 VPS 公网 IP..."
    _SERVER_IP=$(curl -sS --max-time 5 https://api.ipify.org 2>/dev/null) \
        || { err "无法获取 VPS 公网 IP"; return 1; }
    _SERVER_IP_FETCHED=1
    local vps_ip="$_SERVER_IP"

    local candidates
    case "$want" in
        socks5) candidates="socks5" ;;
        http)   candidates="http"   ;;
        *)      candidates="socks5 http" ;;
    esac

    local t scheme exit_ip=""
    for t in $candidates; do
        if [[ "$t" == "http" ]]; then scheme="http"; else scheme="socks5h"; fi
        info "通过 ${t^^} 测试出口..."
        if exit_ip=$(curl_proxy_cfg "$scheme" "$host" "$port" "$user" "$pass" \
                     | curl -sS --max-time 10 -K - https://api.ipify.org 2>/dev/null); then
            RESI_TYPE="$t"
            break
        fi
        exit_ip=""
    done

    if [[ -z "$exit_ip" ]]; then
        if [[ "$want" == "auto" ]]; then
            err "SOCKS5 与 HTTP 两种协议都连不上 (${host}:${port}) —— 请核对凭据与端口（供应商的 SOCKS5 与 HTTP 端口通常不同）"
        else
            err "连接住宅代理失败 (${want^^} ${host}:${port})"
        fi
        # v3.6.0 R8: Bright Data 等住宅线路只放开固定目标端口(8080/8443/5678/1962/2000/4443/...)且只允许
        # HTTPS 目标，443 不在名单里——探测必然失败，但报错看起来像"凭据错了"
        err "若供应商限制目标端口（如 Bright Data 住宅仅开放 8080/8443 等），请改用其 HTTP 代理端口或联系供应商；详见 docs/residential-proxy-guide.md"
        return 1
    fi

    [[ "$exit_ip" == "$vps_ip" ]] \
        && { err "出口 IP 与 VPS 相同 (${vps_ip})，代理未生效"; return 1; }

    RESI_EXIT_IP="$exit_ip"
    RESI_ISP_INFO=$(curl -sS --max-time 5 "https://ipinfo.io/${exit_ip}/json" 2>/dev/null \
        | jq -r '((.org // "") + ", " + (.city // "") + ", " + (.country // "")) | gsub("null"; "")' \
        2>/dev/null || echo "")
}

# v3.6.0: sing-box 版本探测。1.15 起 1.14 的弃用项变致命（会拒绝启动），所以自动下载的版本
# 卡在 SINGBOX_MAX_MINOR.x；tag 从 releases 列表（按创建时间倒序）取首个匹配，不用
# /releases/latest（拿不到上限内的老版本）
SINGBOX_MAX_MINOR="1.14"

gh_latest_tag() {
    local repo="$1" re="${2:-.}"
    curl -fsSL --max-time 15 "https://api.github.com/repos/${repo}/releases?per_page=100" 2>/dev/null \
        | grep -oE '"tag_name":[[:space:]]*"[^"]+"' | sed -E 's/.*"([^"]+)"$/\1/' | grep -E "$re" | head -1 || true
}

singbox_latest_version() {
    local latest minor capped
    latest=$(gh_latest_tag SagerNet/sing-box '^v[0-9]+\.[0-9]+\.[0-9]+$' | sed 's/^v//')
    minor=$(echo "$latest" | cut -d. -f1-2)
    if [[ -n "$latest" && "$minor" != "$SINGBOX_MAX_MINOR" ]] \
        && [[ "$(printf '%s\n%s\n' "$SINGBOX_MAX_MINOR" "$minor" | sort -V | head -1)" == "$SINGBOX_MAX_MINOR" ]]; then
        capped=$(gh_latest_tag SagerNet/sing-box "^v${SINGBOX_MAX_MINOR//./\\.}\.[0-9]+$" | sed 's/^v//')
        if [[ -n "$capped" ]]; then echo "$capped"; return 0; fi
        info "sing-box 最新 ${latest} 超过上限 ${SINGBOX_MAX_MINOR}.x 且未找到上限内版本，跳过自动更新"
        echo ""
        return 0
    fi
    echo "$latest"
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
    ver=$(singbox_latest_version)
    [[ -z "$ver" ]] && { err "无法获取 sing-box 最新版本"; return 1; }
    ver="v${ver}"

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
    # v3.6.0: 两个探测都失败时，沿用中继配置里已有的那个 /32（本机公网 IP 直连例外）。
    # 否则一次网络抖动就让 ip_cidr 少一项 → 摘要变化 → 白重启一次，恢复时再重启一次。
    # 探测成功一律以探测结果为准（换 IP 的机器要跟上）。
    if [[ -z "$_SERVER_IP" && -f "${SINGBOX_CONFIG}" ]]; then
        _SERVER_IP=$(jq -r '[.route.rules[]?|select(.ip_cidr)|.ip_cidr[]|select(endswith("/32"))][0] // "" | sub("/32$";"")' \
                     "${SINGBOX_CONFIG}" 2>/dev/null || true)
    fi
    _SERVER_IP_FETCHED=1
}

write_singbox_config_residential() {
    local host="$1" port="$2" user="$3" pass="$4" type="${5:-socks5}"
    local urls_json
    urls_json=$(jq -n \
        --arg  host "$host" --argjson port "$port" \
        --arg  user "$user" --arg pass "$pass" --arg type "$type" \
        '[{host:$host, port:$port, username:$user, password:$pass, name:"primary", type:$type}]')
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

    # v3.6.0 R10: 按条目 type 出站 —— http 上游是 sing-box 的 http 出站（无 version 字段、TCP only，
    # 与 R4 的 UDP 规则一致，无需改路由）；缺 type 的老条目一律当 socks5
    local outbounds_resi outbound_tags
    outbounds_resi=$(echo "$urls_json" | jq '
      to_entries | map(
        ((.value.type // "socks5") as $t
         | {tag: ("resi-" + ((.key + 1) | tostring)),
            server: .value.host,
            server_port: (.value.port | tonumber),
            username: .value.username,
            password: .value.password} as $base
         | if $t == "http"
           then {type: "http"} + $base
           else {type: "socks"} + $base + {version: "5"}
           end))')
    outbound_tags=$(echo "$urls_json" | jq '
      to_entries | map("resi-" + ((.key + 1) | tostring))')

    # global=true:  final → resi-pool（全部走住宅），无域名分流规则
    # global=false: final → direct，domain_keyword 命中时走 resi-pool
    # v3.6.0 R4: 住宅 SOCKS5 基本不支持 UDP ASSOCIATE —— DNS 直连、QUIC 拒绝(浏览器回退 TCP 走住宅)、其余 UDP 直连
    # v3.6.0 R6: selector + Clash API 热切换——巡检按健康度粘住，切换不重启；
    #            urltest 按延迟择优会在会话内换出口 IP（住宅抖动大，AI 登录场景高危）
    jq -n \
        --argjson outbounds_resi "$outbounds_resi" \
        --argjson outbound_tags  "$outbound_tags" \
        --argjson relay_port "$SINGBOX_RELAY_PORT" \
        --argjson kw      "$kw_json" \
        --arg  server_ip  "${_SERVER_IP:-}" \
        --argjson private  "$PRIVATE_CIDRS" \
        --argjson is_global "$is_global" \
        --arg  api        "$SINGBOX_RELAY_API" \
        --arg  cache      "${BASE_DIR}/relay-cache.db" \
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
            "strategy": "ipv4_only"
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
                "type": "selector",
                "tag": "resi-pool",
                "outbounds": $outbound_tags,
                "default": $outbound_tags[0],
                "interrupt_exist_connections": false
              },
              {"type": "direct", "tag": "direct"}]
          ),
          "experimental": {
            "clash_api": {"external_controller": $api},
            "cache_file": {"enabled": true, "path": $cache}
          },
          "route": {
            "rules": (
              [{"action": "sniff"},
               {"network": "udp", "port": 53, "outbound": "direct"},
               {"network": "udp", "port": 443, "action": "reject"},
               {"network": "udp", "outbound": "direct"},
               {"ip_cidr": ($private + (if $server_ip != "" then [($server_ip + "/32")] else [] end)),
                "outbound": "direct"}]
              + (if $is_global then []
                 else [{"domain_keyword": $kw, "outbound": "resi-pool"}]
                 end)
            ),
            "final": (if $is_global then "resi-pool" else "direct" end),
            "default_domain_resolver": "dns_direct"
          }
        }' > "${SINGBOX_CONFIG}.tmp" \
    && chmod 600 "${SINGBOX_CONFIG}.tmp" \
    && mv "${SINGBOX_CONFIG}.tmp" "${SINGBOX_CONFIG}"
    chmod 600 "${SINGBOX_CONFIG}"
}

# 直连模式（空池或 disable）
write_singbox_config_direct() {
    get_server_ip

    # v3.6.0 R4: 住宅 SOCKS5 基本不支持 UDP ASSOCIATE —— DNS 直连、QUIC 拒绝(浏览器回退 TCP 走住宅)、其余 UDP 直连
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
            "strategy": "ipv4_only"
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
              {"network": "udp", "port": 53, "outbound": "direct"},
              {"network": "udp", "port": 443, "action": "reject"},
              {"network": "udp", "outbound": "direct"},
              {
                "ip_cidr": ($private + (if $server_ip != "" then [($server_ip + "/32")] else [] end)),
                "outbound": "direct"
              }
            ],
            "final": "direct",
            "default_domain_resolver": "dns_direct"
          }
        }' > "${SINGBOX_CONFIG}.tmp" \
    && chmod 600 "${SINGBOX_CONFIG}.tmp" \
    && mv "${SINGBOX_CONFIG}.tmp" "${SINGBOX_CONFIG}"
    chmod 600 "${SINGBOX_CONFIG}"
}

start_relay_service() {
    cat > "${RELAY_UNIT}" <<EOF
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
    rm -f "${RELAY_UNIT}"
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
        --arg  type      "${RESI_TYPE:-socks5}" \
        --arg  lastVerifiedIp      "${RESI_EXIT_IP:-}" \
        --arg  lastVerifiedIspInfo "${RESI_ISP_INFO:-}" \
        --arg  lastVerifiedAt "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        '{enabled:$enabled,global:$global,domains:$domains,urls:$urls,host:$host,port:$port,
          username:$username,password:$password,type:$type,lastVerifiedIp:$lastVerifiedIp,
          lastVerifiedIspInfo:$lastVerifiedIspInfo,lastVerifiedAt:$lastVerifiedAt}' \
        > "${RESIDENTIAL_CONFIG}.tmp" \
    && chmod 600 "${RESIDENTIAL_CONFIG}.tmp" \
    && mv "${RESIDENTIAL_CONFIG}.tmp" "${RESIDENTIAL_CONFIG}"
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
    local h p u pw t
    h=$(jq -r '.host // ""'     "${RESIDENTIAL_CONFIG}")
    p=$(jq -r '.port // 0'      "${RESIDENTIAL_CONFIG}")
    u=$(jq -r '.username // ""' "${RESIDENTIAL_CONFIG}")
    pw=$(jq -r '.password // ""' "${RESIDENTIAL_CONFIG}")
    t=$(jq -r '.type // "socks5"' "${RESIDENTIAL_CONFIG}")
    if [[ -z "$h" || "$p" == "0" ]]; then
        echo "[]"
        return 0
    fi
    jq -n --arg h "$h" --argjson p "$p" --arg u "$u" --arg pw "$pw" --arg t "$t" \
        '[{host:$h, port:$p, username:$u, password:$pw, name:"primary", type:$t}]'
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
    local host="$1" port="$2" user="$3" pass="$4" type="${5:-socks5}"
    if [[ ! -f "${RESIDENTIAL_CONFIG}" ]]; then
        echo '{"enabled":false,"global":false,"urls":[]}' > "${RESIDENTIAL_CONFIG}.tmp" \
        && chmod 600 "${RESIDENTIAL_CONFIG}.tmp" \
        && mv "${RESIDENTIAL_CONFIG}.tmp" "${RESIDENTIAL_CONFIG}"
        chmod 600 "${RESIDENTIAL_CONFIG}"
    fi
    # name 按最终数组位置稠密重排（url-1..url-N），避免 length+1 在去重/移除后产生重名
    jq --arg h "$host" --argjson p "$port" --arg u "$user" --arg pw "$pass" --arg t "$type" \
        '.urls = ((.urls // [])
                  | map(select(.host != $h or .port != $p))
                  + [{host:$h, port:$p, username:$u, password:$pw, type:$t}]
                  | to_entries
                  | map(.value + {name: ("url-" + ((.key + 1) | tostring))}))' \
        "${RESIDENTIAL_CONFIG}" > "${RESIDENTIAL_CONFIG}.tmp" \
    && chmod 600 "${RESIDENTIAL_CONFIG}.tmp" \
    && mv "${RESIDENTIAL_CONFIG}.tmp" "${RESIDENTIAL_CONFIG}"
    chmod 600 "${RESIDENTIAL_CONFIG}"
}

remove_url_from_config() {
    local host="$1" port="$2"
    [[ -f "${RESIDENTIAL_CONFIG}" ]] || return 0
    # 移除后同样稠密重排 name，保持 url-1..url-N 连续无空洞
    jq --arg h "$host" --argjson p "$port" \
        '.urls = ((.urls // [])
                  | map(select(.host != $h or .port != $p))
                  | to_entries
                  | map(.value + {name: ("url-" + ((.key + 1) | tostring))}))' \
        "${RESIDENTIAL_CONFIG}" > "${RESIDENTIAL_CONFIG}.tmp" \
    && chmod 600 "${RESIDENTIAL_CONFIG}.tmp" \
    && mv "${RESIDENTIAL_CONFIG}.tmp" "${RESIDENTIAL_CONFIG}"
    chmod 600 "${RESIDENTIAL_CONFIG}"
}

# ---------------------------------------------------------------------------
# 主入口
# ---------------------------------------------------------------------------
cmd="${1:-}"

# v3.6.0 R2: 写路径统一加锁（status/domains 只读，不加锁）
case "$cmd" in
    setup|enable|disable|reapply|set-domains|global) acquire_relay_lock ;;
esac

case "$cmd" in
    setup)
        ensure_singbox
        write_singbox_config_from_state
        start_relay_service
        info "sing-box 中继已就绪，b-ui-relay 监听 127.0.0.1:${SINGBOX_RELAY_PORT}"
        ;;

    enable)
        if [[ "${2:-}" == "--add" ]]; then
            add_url="${3:-}"
            [[ -z "$add_url" ]] && { err "用法: $0 enable --add <url>|-（- 表示从 stdin 读一行）"; exit 1; }
            # v3.6.0 R10: "-" → 从 stdin 读一行，凭据不进 argv（面板走这条路，ps 看不到）
            if [[ "$add_url" == "-" ]]; then
                IFS= read -r add_url || true
                [[ -z "$add_url" ]] && { err "stdin 未读到代理 URL"; exit 1; }
            fi
            parse_url "$add_url"
            verify "$RESI_HOST" "$RESI_PORT" "$RESI_USER" "$RESI_PASS" "$RESI_TYPE"
            ensure_singbox
            add_url_to_config "$RESI_HOST" "$RESI_PORT" "$RESI_USER" "$RESI_PASS" "$RESI_TYPE"
            save_config true
            write_singbox_config_from_state
            reload_relay_service
            total=$(jq '(.urls // []) | length' "${RESIDENTIAL_CONFIG}")
            info "已新增 URL（共 ${total} 个住宅出口，本条 ${RESI_TYPE}）"
            echo "$RESI_EXIT_IP"
            echo "${RESI_ISP_INFO:-}"
            echo "$RESI_TYPE"
            exit 0
        elif [[ "${2:-}" == "--remove" ]]; then
            [[ -z "${3:-}" ]] && { err "用法: $0 enable --remove <url>"; exit 1; }
            parse_url "$3"
            ensure_singbox
            remove_url_from_config "$RESI_HOST" "$RESI_PORT"
            total=$(jq '(.urls // []) | length' "${RESIDENTIAL_CONFIG}" 2>/dev/null || echo 0)
            if [[ "$total" -eq 0 ]]; then
                jq '.enabled = false' "${RESIDENTIAL_CONFIG}" > "${RESIDENTIAL_CONFIG}.tmp" \
                  && chmod 600 "${RESIDENTIAL_CONFIG}.tmp" \
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

        [[ -z "${2:-}" ]] && { err "用法: $0 enable <url>"; exit 1; }
        parse_url "$2"
        verify "$RESI_HOST" "$RESI_PORT" "$RESI_USER" "$RESI_PASS" "$RESI_TYPE"
        ensure_singbox
        write_singbox_config_residential "$RESI_HOST" "$RESI_PORT" "$RESI_USER" "$RESI_PASS" "$RESI_TYPE"
        reload_relay_service
        if [[ -f "${RESIDENTIAL_CONFIG}" ]]; then
            jq '.urls = []' "${RESIDENTIAL_CONFIG}" > "${RESIDENTIAL_CONFIG}.tmp" 2>/dev/null \
              && chmod 600 "${RESIDENTIAL_CONFIG}.tmp" \
              && mv "${RESIDENTIAL_CONFIG}.tmp" "${RESIDENTIAL_CONFIG}" || true
        fi
        save_config true
        echo "$RESI_EXIT_IP"
        echo "${RESI_ISP_INFO:-}"
        echo "$RESI_TYPE"
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
                && chmod 600 "${RESIDENTIAL_CONFIG}.tmp" \
                && mv "${RESIDENTIAL_CONFIG}.tmp" "${RESIDENTIAL_CONFIG}"
        fi
        info "住宅代理已关闭，b-ui-relay 继续运行（直连模式）"
        ;;

    status)
        if [[ -f "${RESIDENTIAL_CONFIG}" ]]; then
            # v3.6.0 R10: 老条目缺 type → 输出时补 socks5（读侧缺省），文件本身不动
            status_out=$(jq 'if (.urls|type)=="array" then .urls |= map(.type = (.type // "socks5")) else . end' \
                            "${RESIDENTIAL_CONFIG}" 2>/dev/null) \
                && printf '%s\n' "$status_out" || cat "${RESIDENTIAL_CONFIG}"
        else
            echo '{"enabled":false,"global":false}'
        fi
        ;;

    domains)
        # v3.6.0 R1: 输出生效域名关键字（自定义优先，否则 DEFAULT_DOMAINS）
        # server.js 订阅生成器与面板显示都从这里取，默认列表只此一处
        get_domains
        printf '%s\n' "${DOMAINS[@]}" | jq -R . | jq -sc 'unique'
        ;;

    reapply)
        ensure_singbox
        # start_relay_service 会重写 unit（升级迁移依赖它），但 enable --now 不会重启"已在跑"的实例。
        # v3.6.0 R6: 新配置(selector + clash_api)必须真正加载——否则升级后巡检永远拿不到 Clash API，
        # 热切换形同虚设；但无条件 restart 会让每次版本升级都掐断全部住宅连接。
        # 所以取"改前/改后"摘要，只有中继配置或 unit 真的变了才重启已在跑的实例（稳态 = 零重启）。
        relay_was_active=0
        systemctl is-active --quiet "${RELAY_SERVICE}" 2>/dev/null && relay_was_active=1
        relay_cfg_before=$(file_digest "${SINGBOX_CONFIG}")
        relay_unit_before=$(file_digest "${RELAY_UNIT}")
        write_singbox_config_from_state
        start_relay_service
        if [ "$relay_was_active" = "1" ]; then
            if [ "$(file_digest "${SINGBOX_CONFIG}")" != "$relay_cfg_before" ] || \
               [ "$(file_digest "${RELAY_UNIT}")" != "$relay_unit_before" ]; then
                systemctl restart "${RELAY_SERVICE}" 2>/dev/null || true
                info "b-ui-relay 配置/unit 有变化，已重启"
            else
                info "b-ui-relay 配置与 unit 无变化，保持运行（不掐断住宅连接）"
            fi
        fi
        ;;

    set-domains)
        [[ -z "${2:-}" ]] && { err "用法: $0 set-domains <json_array>"; exit 1; }
        echo "$2" | jq 'if type == "array" then . else error("not an array") end' >/dev/null 2>&1 \
            || { err "参数必须是 JSON 数组"; exit 1; }

        if [[ -f "${RESIDENTIAL_CONFIG}" ]]; then
            jq --argjson domains "$2" '.domains = $domains' \
               "${RESIDENTIAL_CONFIG}" > "${RESIDENTIAL_CONFIG}.tmp" \
            && chmod 600 "${RESIDENTIAL_CONFIG}.tmp" \
            && mv "${RESIDENTIAL_CONFIG}.tmp" "${RESIDENTIAL_CONFIG}"
        else
            jq -n --argjson domains "$2" '{enabled:false,global:false,domains:$domains}' \
               > "${RESIDENTIAL_CONFIG}.tmp" \
            && chmod 600 "${RESIDENTIAL_CONFIG}.tmp" \
            && mv "${RESIDENTIAL_CONFIG}.tmp" "${RESIDENTIAL_CONFIG}"
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
            && chmod 600 "${RESIDENTIAL_CONFIG}.tmp" \
            && mv "${RESIDENTIAL_CONFIG}.tmp" "${RESIDENTIAL_CONFIG}"
        else
            jq -n --argjson g "$new_global" '{"enabled":false,"global":$g}' \
               > "${RESIDENTIAL_CONFIG}.tmp" \
            && chmod 600 "${RESIDENTIAL_CONFIG}.tmp" \
            && mv "${RESIDENTIAL_CONFIG}.tmp" "${RESIDENTIAL_CONFIG}"
            chmod 600 "${RESIDENTIAL_CONFIG}"
        fi

        write_singbox_config_from_state
        reload_relay_service
        info "global 模式已设置为: ${local_val}"
        ;;

    *)
        echo "Usage: $0 {setup|enable <url>|disable|status|domains|reapply|set-domains <json>|global on|off}" >&2
        exit 1
        ;;
esac
