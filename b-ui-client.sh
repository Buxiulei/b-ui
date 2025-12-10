#!/bin/bash

#===============================================================================
# Hysteria2 客户端一键安装脚本 (Ubuntu/Debian)
# 功能：SOCKS5/HTTP 代理、TUN 模式、路由规则、SSH 保护
# 版本: 1.1.0
#===============================================================================

SCRIPT_VERSION="1.1.0"

set -e

# 颜色定义
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m'

# 路径配置
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BASE_DIR="${SCRIPT_DIR}/hysteria-client"
CONFIG_FILE="${BASE_DIR}/config.yaml"
RULES_FILE="${BASE_DIR}/bypass-rules.txt"
CLIENT_SERVICE="hysteria-client.service"

# 默认配置
SOCKS_PORT="1080"
HTTP_PORT="8080"
TUN_ENABLED="false"

#===============================================================================
# 工具函数
#===============================================================================

print_banner() {
    clear
    echo -e "${CYAN}"
    echo "╔══════════════════════════════════════════════════════════════╗"
    echo "║                                                              ║"
    echo "║          Hysteria2 客户端 - 增强版                           ║"
    echo "║                                                              ║"
    echo "║     支持：SOCKS5 / HTTP / TUN / 路由规则 / SSH保护           ║"
    echo "║                                                              ║"
    echo -e "║     版本: ${YELLOW}${SCRIPT_VERSION}${CYAN}                                               ║"
    echo "╚══════════════════════════════════════════════════════════════╝"
    echo -e "${NC}"
}

print_info() { echo -e "${BLUE}[INFO]${NC} $1"; }
print_success() { echo -e "${GREEN}[SUCCESS]${NC} $1"; }
print_warning() { echo -e "${YELLOW}[WARNING]${NC} $1"; }
print_error() { echo -e "${RED}[ERROR]${NC} $1"; }

check_root() {
    if [[ $EUID -ne 0 ]]; then
        print_error "此脚本需要 root 权限"
        exit 1
    fi
}

check_os() {
    if [[ -f /etc/os-release ]]; then
        . /etc/os-release
        OS_ID="$ID"
        OS_VERSION="$VERSION_ID"
        print_info "操作系统: $OS_ID $OS_VERSION"
    else
        OS_ID="unknown"
        print_warning "无法识别操作系统"
    fi
}

#===============================================================================
# 依赖检测与安装
#===============================================================================

# 检测包管理器
detect_package_manager() {
    if command -v apt-get &> /dev/null; then
        PKG_MANAGER="apt"
        PKG_UPDATE="apt-get update -qq"
        PKG_INSTALL="apt-get install -y -qq"
    elif command -v dnf &> /dev/null; then
        PKG_MANAGER="dnf"
        PKG_UPDATE="dnf check-update -q || true"
        PKG_INSTALL="dnf install -y -q"
    elif command -v yum &> /dev/null; then
        PKG_MANAGER="yum"
        PKG_UPDATE="yum check-update -q || true"
        PKG_INSTALL="yum install -y -q"
    elif command -v pacman &> /dev/null; then
        PKG_MANAGER="pacman"
        PKG_UPDATE="pacman -Sy --noconfirm"
        PKG_INSTALL="pacman -S --noconfirm"
    elif command -v apk &> /dev/null; then
        PKG_MANAGER="apk"
        PKG_UPDATE="apk update"
        PKG_INSTALL="apk add --no-cache"
    else
        PKG_MANAGER="unknown"
        print_warning "未检测到支持的包管理器"
    fi
}

# 检查并安装单个依赖
check_and_install_dep() {
    local cmd="$1"
    local pkg="$2"
    local desc="$3"
    
    if command -v "$cmd" &> /dev/null; then
        echo -e "  ${GREEN}✓${NC} $desc ($cmd)"
        return 0
    else
        echo -e "  ${YELLOW}○${NC} $desc - 正在安装..."
        if [[ "$PKG_MANAGER" != "unknown" ]]; then
            $PKG_INSTALL "$pkg" > /dev/null 2>&1
            if command -v "$cmd" &> /dev/null; then
                echo -e "  ${GREEN}✓${NC} $desc - 安装成功"
                return 0
            else
                echo -e "  ${RED}✗${NC} $desc - 安装失败"
                return 1
            fi
        else
            echo -e "  ${RED}✗${NC} $desc - 无法自动安装"
            return 1
        fi
    fi
}

# 主依赖检测函数
check_dependencies() {
    echo ""
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}检测并安装依赖${NC}"
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo ""
    
    detect_package_manager
    print_info "包管理器: $PKG_MANAGER"
    
    # 更新包索引
    if [[ "$PKG_MANAGER" != "unknown" ]]; then
        print_info "更新软件包索引..."
        $PKG_UPDATE > /dev/null 2>&1 || true
    fi
    
    echo ""
    echo -e "${YELLOW}[核心工具]${NC}"
    
    local missing_critical=0
    
    # curl - 下载必需
    check_and_install_dep "curl" "curl" "curl (HTTP 下载)" || ((missing_critical++))
    
    # dig/nslookup - DNS 解析
    if ! command -v dig &> /dev/null; then
        case "$PKG_MANAGER" in
            apt) check_and_install_dep "dig" "dnsutils" "dig (DNS 解析)" || true ;;
            dnf|yum) check_and_install_dep "dig" "bind-utils" "dig (DNS 解析)" || true ;;
            pacman) check_and_install_dep "dig" "bind" "dig (DNS 解析)" || true ;;
            apk) check_and_install_dep "dig" "bind-tools" "dig (DNS 解析)" || true ;;
            *) echo -e "  ${YELLOW}○${NC} dig (DNS 解析) - 跳过" ;;
        esac
    else
        echo -e "  ${GREEN}✓${NC} dig (DNS 解析)"
    fi
    
    # ss/netstat - 端口检测
    if command -v ss &> /dev/null; then
        echo -e "  ${GREEN}✓${NC} ss (端口检测)"
    elif command -v netstat &> /dev/null; then
        echo -e "  ${GREEN}✓${NC} netstat (端口检测)"
    else
        case "$PKG_MANAGER" in
            apt) check_and_install_dep "ss" "iproute2" "ss (端口检测)" || true ;;
            dnf|yum) check_and_install_dep "ss" "iproute" "ss (端口检测)" || true ;;
            pacman) check_and_install_dep "ss" "iproute2" "ss (端口检测)" || true ;;
            apk) check_and_install_dep "ss" "iproute2" "ss (端口检测)" || true ;;
            *) echo -e "  ${YELLOW}○${NC} ss (端口检测) - 跳过" ;;
        esac
    fi
    
    # iptables - TUN 模式可能需要
    if ! command -v iptables &> /dev/null; then
        case "$PKG_MANAGER" in
            apt) check_and_install_dep "iptables" "iptables" "iptables (防火墙)" || true ;;
            dnf|yum) check_and_install_dep "iptables" "iptables" "iptables (防火墙)" || true ;;
            pacman) check_and_install_dep "iptables" "iptables" "iptables (防火墙)" || true ;;
            apk) check_and_install_dep "iptables" "iptables" "iptables (防火墙)" || true ;;
            *) echo -e "  ${YELLOW}○${NC} iptables (防火墙) - 跳过" ;;
        esac
    else
        echo -e "  ${GREEN}✓${NC} iptables (防火墙)"
    fi
    
    echo ""
    echo -e "${YELLOW}[系统工具]${NC}"
    
    # systemctl - 服务管理
    if command -v systemctl &> /dev/null; then
        echo -e "  ${GREEN}✓${NC} systemctl (服务管理)"
    else
        echo -e "  ${RED}✗${NC} systemctl (服务管理) - 此脚本需要 systemd"
        ((missing_critical++))
    fi
    
    # tar/gzip - 解压
    check_and_install_dep "tar" "tar" "tar (解压)" || true
    check_and_install_dep "gzip" "gzip" "gzip (解压)" || true
    
    # wget - 备用下载
    if ! command -v wget &> /dev/null; then
        check_and_install_dep "wget" "wget" "wget (备用下载)" || true
    else
        echo -e "  ${GREEN}✓${NC} wget (备用下载)"
    fi
    
    # ca-certificates - HTTPS 支持
    case "$PKG_MANAGER" in
        apt)
            if dpkg -l ca-certificates &> /dev/null; then
                echo -e "  ${GREEN}✓${NC} ca-certificates (HTTPS)"
            else
                $PKG_INSTALL ca-certificates > /dev/null 2>&1
                echo -e "  ${GREEN}✓${NC} ca-certificates (HTTPS) - 已安装"
            fi
            ;;
        dnf|yum)
            if rpm -q ca-certificates &> /dev/null; then
                echo -e "  ${GREEN}✓${NC} ca-certificates (HTTPS)"
            else
                $PKG_INSTALL ca-certificates > /dev/null 2>&1
                echo -e "  ${GREEN}✓${NC} ca-certificates (HTTPS) - 已安装"
            fi
            ;;
        *)
            echo -e "  ${YELLOW}○${NC} ca-certificates (HTTPS) - 跳过检测"
            ;;
    esac
    
    echo ""
    
    if [[ $missing_critical -gt 0 ]]; then
        print_error "有 $missing_critical 个关键依赖缺失，无法继续"
        exit 1
    fi
    
    print_success "依赖检测完成"
    echo ""
}

#===============================================================================
# 端口检测工具
#===============================================================================

is_port_in_use() {
    # 检查端口是否被占用
    local port="$1"
    if command -v ss &> /dev/null; then
        # ss 输出格式: 127.0.0.1:1080 或 *:1080 或 [::]:1080
        ss -tuln 2>/dev/null | grep -qE "[:.]${port}(\\s|$)" && return 0
    elif command -v netstat &> /dev/null; then
        # Linux netstat: 127.0.0.1:1080, macOS netstat: 127.0.0.1.1080
        netstat -tuln 2>/dev/null | grep -qE "[:.](${port})\\s" && return 0
    fi
    # 使用 /dev/tcp 检测 (bash 内置，最后手段)
    (echo >/dev/tcp/127.0.0.1/$port) 2>/dev/null && return 0
    return 1
}

get_occupied_ports() {
    # 获取当前所有已占用的端口
    if command -v ss &> /dev/null; then
        ss -tuln 2>/dev/null | grep -E 'LISTEN|ESTAB' | awk '{print $5}' | grep -oE '[0-9]+$' | sort -un
    elif command -v netstat &> /dev/null; then
        netstat -tuln 2>/dev/null | grep -i listen | awk '{print $4}' | grep -oE '[0-9]+$' | sort -un
    fi
}

find_available_port() {
    # 找到一个可用的端口，从指定端口开始
    local start_port="$1"
    local port="$start_port"
    local max_attempts=100
    
    for ((i=0; i<max_attempts; i++)); do
        if ! is_port_in_use "$port"; then
            echo "$port"
            return 0
        fi
        ((port++))
    done
    
    # 如果连续100个端口都被占用，返回原始端口
    echo "$start_port"
    return 1
}

check_and_suggest_ports() {
    # 检查默认端口并建议替代端口
    local socks_default="${1:-1080}"
    local http_default="${2:-8080}"
    
    echo ""
    echo -e "${CYAN}[端口检测]${NC}"
    
    # 显示当前已占用的常用代理端口
    local occupied=$(get_occupied_ports)
    local common_ports=(1080 8080 7890 7891 10808 10809 1081 8081)
    local occupied_common=""
    
    for p in "${common_ports[@]}"; do
        if echo "$occupied" | grep -q "^${p}$"; then
            occupied_common="${occupied_common} ${p}"
        fi
    done
    
    if [[ -n "$occupied_common" ]]; then
        print_warning "以下常用端口已被占用:$occupied_common"
    fi
    
    # 检查并建议 SOCKS5 端口
    if is_port_in_use "$socks_default"; then
        local new_socks=$(find_available_port "$socks_default")
        print_warning "SOCKS5 端口 $socks_default 已被占用，建议使用: $new_socks"
        SUGGESTED_SOCKS_PORT="$new_socks"
    else
        print_success "SOCKS5 端口 $socks_default 可用"
        SUGGESTED_SOCKS_PORT="$socks_default"
    fi
    
    # 检查并建议 HTTP 端口
    if is_port_in_use "$http_default"; then
        local new_http=$(find_available_port "$http_default")
        print_warning "HTTP 端口 $http_default 已被占用，建议使用: $new_http"
        SUGGESTED_HTTP_PORT="$new_http"
    else
        print_success "HTTP 端口 $http_default 可用"
        SUGGESTED_HTTP_PORT="$http_default"
    fi
    
    echo ""
}

get_server_ip() {
    # 从配置中提取服务器 IP
    if [[ -f "$CONFIG_FILE" ]]; then
        local server=$(grep "^server:" "$CONFIG_FILE" | awk '{print $2}' | cut -d':' -f1)
        # 解析域名为 IP
        if [[ -n "$server" ]]; then
            dig +short "$server" A 2>/dev/null | head -1
        fi
    fi
}

get_current_ssh_ip() {
    # 获取当前 SSH 连接的服务器端 IP
    echo "$SSH_CONNECTION" | awk '{print $3}'
}

#===============================================================================
# 安装
#===============================================================================

install_hysteria() {
    print_info "安装 Hysteria2..."
    
    if command -v hysteria &> /dev/null; then
        print_success "已安装: $(hysteria version 2>/dev/null | grep 'Version:' | awk '{print $2}')"
        return 0
    fi
    
    HYSTERIA_USER=root bash <(curl -fsSL https://get.hy2.sh/)
    print_success "安装完成"
}

install_xray_client() {
    print_info "安装 Xray..."
    
    if command -v xray &> /dev/null; then
        print_success "已安装: $(xray version 2>/dev/null | head -n1 | awk '{print $2}')"
        return 0
    fi
    
    bash -c "$(curl -L https://github.com/XTLS/Xray-install/raw/main/install-release.sh)" @ install
    print_success "Xray 安装完成"
}

generate_xray_config() {
    local xray_config="${BASE_DIR}/xray-config.json"
    local server_host=$(echo "$SERVER_ADDR" | cut -d':' -f1)
    local server_port=$(echo "$SERVER_ADDR" | cut -d':' -f2)
    
    # 检测端口占用情况
    check_and_suggest_ports 1080 8080
    
    # SOCKS5 端口
    read -p "SOCKS5 端口 [默认 ${SUGGESTED_SOCKS_PORT}]: " SOCKS_PORT
    SOCKS_PORT=${SOCKS_PORT:-$SUGGESTED_SOCKS_PORT}
    
    if is_port_in_use "$SOCKS_PORT"; then
        print_warning "端口 $SOCKS_PORT 已被占用，可能会冲突！"
    fi
    
    # HTTP 端口
    read -p "HTTP 端口 [默认 ${SUGGESTED_HTTP_PORT}]: " HTTP_PORT
    HTTP_PORT=${HTTP_PORT:-$SUGGESTED_HTTP_PORT}
    
    if is_port_in_use "$HTTP_PORT"; then
        print_warning "端口 $HTTP_PORT 已被占用，可能会冲突！"
    fi
    
    cat > "$xray_config" << EOF
{
  "log": {"loglevel": "warning"},
  "inbounds": [
    {"tag": "socks", "port": ${SOCKS_PORT}, "listen": "127.0.0.1", "protocol": "socks", "settings": {"udp": true}},
    {"tag": "http", "port": ${HTTP_PORT}, "listen": "127.0.0.1", "protocol": "http"}
  ],
  "outbounds": [
    {
      "tag": "proxy",
      "protocol": "vless",
      "settings": {
        "vnext": [{
          "address": "${server_host}",
          "port": ${server_port},
          "users": [{"id": "${UUID}", "flow": "${FLOW}", "encryption": "none"}]
        }]
      },
      "streamSettings": {
        "network": "${NETWORK}",
        "security": "reality",
        "realitySettings": {
          "serverName": "${SNI}",
          "fingerprint": "${FINGERPRINT}",
          "publicKey": "${PUBLIC_KEY}",
          "shortId": "${SHORT_ID}",
          "spiderX": "/"
        }
      }
    },
    {"tag": "direct", "protocol": "freedom"},
    {"tag": "block", "protocol": "blackhole"}
  ],
  "routing": {
    "domainStrategy": "IPIfNonMatch",
    "rules": [
      {"type": "field", "domain": ["geosite:private"], "outboundTag": "direct"},
      {"type": "field", "ip": ["geoip:private"], "outboundTag": "direct"},
      {"type": "field", "domain": ["geosite:cn"], "outboundTag": "direct"},
      {"type": "field", "ip": ["geoip:cn"], "outboundTag": "direct"}
    ]
  }
}
EOF
    chmod 644 "$xray_config"
    print_success "Xray 配置已生成: $xray_config"
    
    # 创建 Xray 客户端服务
    cat > /etc/systemd/system/xray-client.service << EOF
[Unit]
Description=Xray Client
After=network.target

[Service]
Type=simple
ExecStart=/usr/local/bin/xray run -config ${xray_config}
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF
    systemctl daemon-reload
    systemctl enable xray-client 2>/dev/null || true
    systemctl start xray-client
    
    echo ""
    print_success "Xray 客户端已启动"
    echo -e "  SOCKS5: ${GREEN}127.0.0.1:${SOCKS_PORT}${NC}"
    echo -e "  HTTP:   ${GREEN}127.0.0.1:${HTTP_PORT}${NC}"
}

#===============================================================================
# URI 解析和导入
#===============================================================================

parse_hysteria_uri() {
    local uri="$1"
    
    # 验证 URI 格式
    if [[ ! "$uri" =~ ^(hysteria2|hy2):// ]]; then
        return 1
    fi
    
    # 移除协议前缀
    local content="${uri#*://}"
    
    # 提取备注名 (#后面的部分)
    local remark=""
    if [[ "$content" =~ \#(.+)$ ]]; then
        remark=$(echo "${BASH_REMATCH[1]}" | sed 's/%20/ /g' | sed 's/+/ /g')
        # URL 解码
        remark=$(echo -e "${remark//%/\\x}")
        content="${content%%#*}"
    fi
    
    # 提取密码和服务器 (password@host:port)
    local auth_part="${content%%\?*}"
    local password="${auth_part%%@*}"
    local server_part="${auth_part#*@}"
    
    # URL 解码密码
    password=$(echo -e "${password//%/\\x}")
    
    # 提取查询参数
    local query_part=""
    [[ "$content" =~ \?(.+) ]] && query_part="${BASH_REMATCH[1]}"
    
    # 解析 SNI
    local sni=""
    [[ "$query_part" =~ sni=([^&]+) ]] && sni="${BASH_REMATCH[1]}"
    
    # 解析 insecure
    local insecure="false"
    [[ "$query_part" =~ insecure=1 ]] && insecure="true"
    
    # 输出解析结果
    echo "PROTOCOL=hysteria2"
    echo "SERVER_ADDR=$server_part"
    echo "AUTH_PASSWORD=$password"
    echo "SNI=$sni"
    echo "INSECURE=$insecure"
    echo "REMARK=$remark"
}

parse_vless_uri() {
    local uri="$1"
    
    # 验证 URI 格式
    if [[ ! "$uri" =~ ^vless:// ]]; then
        return 1
    fi
    
    # 移除协议前缀
    local content="${uri#vless://}"
    
    # 提取备注名 (#后面的部分)
    local remark=""
    if [[ "$content" =~ \#(.+)$ ]]; then
        remark=$(echo "${BASH_REMATCH[1]}" | sed 's/%20/ /g' | sed 's/+/ /g')
        remark=$(echo -e "${remark//%/\\x}")
        content="${content%%#*}"
    fi
    
    # 提取 UUID 和服务器 (uuid@host:port)
    local auth_part="${content%%\?*}"
    local uuid="${auth_part%%@*}"
    local server_part="${auth_part#*@}"
    
    # 提取查询参数
    local query_part=""
    [[ "$content" =~ \?(.+) ]] && query_part="${BASH_REMATCH[1]}"
    
    # 解析各参数
    local security="" sni="" fp="" pbk="" sid="" flow="" type=""
    [[ "$query_part" =~ security=([^&]+) ]] && security="${BASH_REMATCH[1]}"
    [[ "$query_part" =~ sni=([^&]+) ]] && sni="${BASH_REMATCH[1]}"
    [[ "$query_part" =~ fp=([^&]+) ]] && fp="${BASH_REMATCH[1]}"
    [[ "$query_part" =~ pbk=([^&]+) ]] && pbk="${BASH_REMATCH[1]}"
    [[ "$query_part" =~ sid=([^&]+) ]] && sid="${BASH_REMATCH[1]}"
    [[ "$query_part" =~ flow=([^&]+) ]] && flow="${BASH_REMATCH[1]}"
    [[ "$query_part" =~ type=([^&]+) ]] && type="${BASH_REMATCH[1]}"
    
    # 输出解析结果
    echo "PROTOCOL=vless-reality"
    echo "SERVER_ADDR=$server_part"
    echo "UUID=$uuid"
    echo "SECURITY=$security"
    echo "SNI=$sni"
    echo "FINGERPRINT=${fp:-chrome}"
    echo "PUBLIC_KEY=$pbk"
    echo "SHORT_ID=$sid"
    echo "FLOW=${flow:-xtls-rprx-vision}"
    echo "NETWORK=${type:-tcp}"
    echo "REMARK=$remark"
}

import_from_uri() {
    echo ""
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}从链接导入配置${NC}"
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo ""
    echo -e "支持格式:"
    echo -e "  ${YELLOW}hysteria2://password@host:port/?sni=xxx#备注${NC}"
    echo -e "  ${YELLOW}vless://uuid@host:port/?security=reality&sni=xxx&pbk=xxx#备注${NC}"
    echo ""
    
    read -p "请粘贴配置链接: " uri
    
    if [[ -z "$uri" ]]; then
        print_error "链接不能为空"
        return 1
    fi
    
    # 尝试解析 URI
    local parsed=""
    if [[ "$uri" =~ ^(hysteria2|hy2):// ]]; then
        parsed=$(parse_hysteria_uri "$uri")
    elif [[ "$uri" =~ ^vless:// ]]; then
        parsed=$(parse_vless_uri "$uri")
    else
        print_error "不支持的链接格式"
        return 1
    fi
    
    if [[ $? -ne 0 || -z "$parsed" ]]; then
        print_error "链接解析失败"
        return 1
    fi
    
    # 导入解析结果
    eval "$parsed"
    
    echo ""
    print_success "解析成功！"
    echo -e "  协议:   ${GREEN}${PROTOCOL}${NC}"
    echo -e "  服务器: ${GREEN}${SERVER_ADDR}${NC}"
    if [[ "$PROTOCOL" == "hysteria2" ]]; then
        echo -e "  密码:   ${GREEN}${AUTH_PASSWORD}${NC}"
    else
        echo -e "  UUID:   ${GREEN}${UUID}${NC}"
        echo -e "  公钥:   ${GREEN}${PUBLIC_KEY}${NC}"
    fi
    [[ -n "$SNI" ]] && echo -e "  SNI:    ${GREEN}${SNI}${NC}"
    [[ -n "$REMARK" ]] && echo -e "  备注:   ${GREEN}${REMARK}${NC}"
    echo ""
    
    mkdir -p "$BASE_DIR"
    
    if [[ "$PROTOCOL" == "hysteria2" ]]; then
        # Hysteria2 配置 - 检测端口占用情况
        check_and_suggest_ports 1080 8080
        
        read -p "SOCKS5 端口 [默认 ${SUGGESTED_SOCKS_PORT}]: " SOCKS_PORT
        SOCKS_PORT=${SOCKS_PORT:-$SUGGESTED_SOCKS_PORT}
        
        # 验证用户输入的端口
        if is_port_in_use "$SOCKS_PORT"; then
            print_warning "端口 $SOCKS_PORT 已被占用，可能会冲突！"
        fi
        
        read -p "HTTP 端口 [默认 ${SUGGESTED_HTTP_PORT}]: " HTTP_PORT
        HTTP_PORT=${HTTP_PORT:-$SUGGESTED_HTTP_PORT}
        
        if is_port_in_use "$HTTP_PORT"; then
            print_warning "端口 $HTTP_PORT 已被占用，可能会冲突！"
        fi
        
        read -p "启用 TUN 模式 (全局代理)? (y/n) [默认 n]: " enable_tun
        TUN_ENABLED="false"
        [[ "$enable_tun" == "y" || "$enable_tun" == "Y" ]] && TUN_ENABLED="true"
        
        # 带宽设置
        echo ""
        echo -e "${YELLOW}[带宽设置]${NC} (可选，直接回车跳过)"
        print_info "提示: 设置带宽可以优化连接，但设置过高会导致性能下降"
        read -p "上行带宽 (Mbps) [直接回车跳过]: " BANDWIDTH_UP
        read -p "下行带宽 (Mbps) [直接回车跳过]: " BANDWIDTH_DOWN
        
        create_default_rules
        generate_config
    else
        # VLESS-Reality 配置 (需要 Xray)
        install_xray_client
        generate_xray_config
    fi
    
    print_success "配置已导入并生成"
}

#===============================================================================
# 配置客户端
#===============================================================================

configure_client() {
    print_info "配置客户端..."
    echo ""
    
    # 服务器地址
    read -p "服务器地址 (域名:端口): " SERVER_ADDR
    while [[ -z "$SERVER_ADDR" ]]; do
        read -p "服务器地址不能为空: " SERVER_ADDR
    done
    
    # 密码
    read -p "认证密码: " AUTH_PASSWORD
    while [[ -z "$AUTH_PASSWORD" ]]; do
        read -p "密码不能为空: " AUTH_PASSWORD
    done
    
    # 检测端口占用情况
    check_and_suggest_ports 1080 8080
    
    # SOCKS5 端口
    read -p "SOCKS5 端口 [默认 ${SUGGESTED_SOCKS_PORT}]: " SOCKS_PORT
    SOCKS_PORT=${SOCKS_PORT:-$SUGGESTED_SOCKS_PORT}
    
    if is_port_in_use "$SOCKS_PORT"; then
        print_warning "端口 $SOCKS_PORT 已被占用，可能会冲突！"
    fi
    
    # HTTP 端口
    read -p "HTTP 端口 [默认 ${SUGGESTED_HTTP_PORT}]: " HTTP_PORT
    HTTP_PORT=${HTTP_PORT:-$SUGGESTED_HTTP_PORT}
    
    if is_port_in_use "$HTTP_PORT"; then
        print_warning "端口 $HTTP_PORT 已被占用，可能会冲突！"
    fi
    
    # TUN 模式
    read -p "启用 TUN 模式 (全局代理)? (y/n) [默认 n]: " enable_tun
    TUN_ENABLED="false"
    [[ "$enable_tun" == "y" || "$enable_tun" == "Y" ]] && TUN_ENABLED="true"
    
    # 带宽设置
    echo ""
    echo -e "${YELLOW}[带宽设置]${NC} (可选，直接回车跳过)"
    print_info "提示: 设置带宽可以优化连接，但设置过高会导致性能下降"
    read -p "上行带宽 (Mbps) [直接回车跳过]: " BANDWIDTH_UP
    read -p "下行带宽 (Mbps) [直接回车跳过]: " BANDWIDTH_DOWN
    
    mkdir -p "$BASE_DIR"
    
    # 创建默认绕过规则
    create_default_rules
    
    # 生成配置
    generate_config
    
    print_success "配置已生成"
}

create_default_rules() {
    # 创建默认绕过规则文件
    cat > "$RULES_FILE" << 'EOF'
# Hysteria2 路由绕过规则
# 每行一个规则，支持格式：
#   - IP 地址: 192.168.1.1
#   - CIDR: 10.0.0.0/8
#   - 域名: example.com
#   - 通配符域名: *.example.com
#   - 正则表达式: regexp:.*\.cn$
# 以 # 开头的行为注释

# === 本地/私有网络 (自动绕过) ===
# 这些已在配置中硬编码，无需添加

# === 国内常用域名示例 (取消注释启用) ===
# *.baidu.com
# *.qq.com
# *.taobao.com
# *.aliyun.com
# *.163.com
# *.jd.com
# *.bilibili.com
# *.zhihu.com

# === 正则匹配示例 ===
# regexp:.*\.cn$
# regexp:.*\.com\.cn$

# === 自定义规则 ===
# 在下方添加你的规则

EOF
    print_info "绕过规则文件: $RULES_FILE"
}

generate_config() {
    # 获取服务器 IP 用于 TUN 排除
    local server_host=$(echo "$SERVER_ADDR" | cut -d':' -f1)
    local server_ip=$(dig +short "$server_host" A 2>/dev/null | head -1)
    [[ -z "$server_ip" ]] && server_ip="$server_host"
    
    # 读取自定义规则
    local custom_acl_rules=""
    if [[ -f "$RULES_FILE" ]]; then
        while IFS= read -r line; do
            # 跳过注释和空行
            [[ "$line" =~ ^#.*$ || -z "$line" ]] && continue
            line=$(echo "$line" | xargs)  # trim
            [[ -z "$line" ]] && continue
            
            # 处理不同类型的规则
            if [[ "$line" =~ ^regexp: ]]; then
                # 正则表达式
                local pattern="${line#regexp:}"
                custom_acl_rules="${custom_acl_rules}\n    - ${pattern} direct"
            else
                # 域名/通配符/IP
                custom_acl_rules="${custom_acl_rules}\n    - ${line} direct"
            fi
        done < "$RULES_FILE"
    fi
    
    # 生成配置文件
    cat > "$CONFIG_FILE" << EOF
# Hysteria2 客户端配置
# 生成时间: $(date)
# 服务器: ${SERVER_ADDR}

server: ${SERVER_ADDR}

auth: ${AUTH_PASSWORD}

tls:
  insecure: false

# SOCKS5 代理
socks5:
  listen: 127.0.0.1:${SOCKS_PORT}

# HTTP 代理
http:
  listen: 127.0.0.1:${HTTP_PORT}
EOF

    # 添加带宽设置 (如果用户设置了)
    if [[ -n "$BANDWIDTH_UP" || -n "$BANDWIDTH_DOWN" ]]; then
        # 在 tls 后插入带宽配置
        local bw_config=""
        bw_config+="\n# 带宽设置\nbandwidth:"
        [[ -n "$BANDWIDTH_UP" ]] && bw_config+="\n  up: ${BANDWIDTH_UP} mbps"
        [[ -n "$BANDWIDTH_DOWN" ]] && bw_config+="\n  down: ${BANDWIDTH_DOWN} mbps"
        
        # 使用 sed 在 tls 块后插入
        sed -i "/^tls:/a\\$(echo -e "$bw_config")" "$CONFIG_FILE" 2>/dev/null || \
        sed -i '' "/^tls:/a\\
$(echo -e "$bw_config")
" "$CONFIG_FILE"
        
        print_info "带宽设置: 上行 ${BANDWIDTH_UP:-未设置} Mbps, 下行 ${BANDWIDTH_DOWN:-未设置} Mbps"
    fi

    # 添加 TUN 配置
    if [[ "$TUN_ENABLED" == "true" ]]; then
        cat >> "$CONFIG_FILE" << EOF

# TUN 模式 (全局代理)
tun:
  name: "hystun"
  mtu: 1500
  timeout: 5m
  address:
    ipv4: 100.100.100.101/30
    ipv6: 2001::ffff:ffff:ffff:fff1/126
  route:
    ipv4: [0.0.0.0/0]
    ipv6: ["2000::/3"]
    # 排除的 IP (服务器 IP + 私有网络)
    ipv4Exclude:
      - ${server_ip}/32
      - 10.0.0.0/8
      - 172.16.0.0/12
      - 192.168.0.0/16
      - 127.0.0.0/8
      - 169.254.0.0/16
      - 224.0.0.0/4
      - 255.255.255.255/32
    ipv6Exclude:
      - "fc00::/7"
      - "fe80::/10"
      - "::1/128"

# DNS 配置 - 解决域名解析问题
dns:
  # 使用代理服务器解析 DNS
  mode: tcp
  hijack:
    - type: override
      addr: 8.8.8.8:53
    - type: override
      addr: 8.8.4.4:53

# ACL 路由规则 - 保护 SSH 连接
acl:
  inline:
    # SSH 端口保护 - 所有 22 端口流量绕过代理
    - :22 direct
    - :22/ direct
    # 常用 SSH 备用端口
    - :2222 direct
    - :2222/ direct
EOF
        
        # 添加自定义 ACL 规则
        if [[ -n "$custom_acl_rules" ]]; then
            echo -e "$custom_acl_rules" >> "$CONFIG_FILE"
        fi
    else
        # 非 TUN 模式，但如果有自定义规则，也添加 ACL
        if [[ -n "$custom_acl_rules" ]]; then
            cat >> "$CONFIG_FILE" << EOF

# ACL 路由规则
acl:
  inline:$(echo -e "$custom_acl_rules")
EOF
        fi
    fi
}

#===============================================================================
# 路由规则管理
#===============================================================================

edit_rules() {
    if [[ ! -f "$RULES_FILE" ]]; then
        create_default_rules
    fi
    
    echo ""
    echo -e "${CYAN}路由绕过规则编辑${NC}"
    echo ""
    echo "当前规则文件: $RULES_FILE"
    echo ""
    
    # 显示当前规则
    echo -e "${YELLOW}当前规则:${NC}"
    grep -v "^#" "$RULES_FILE" | grep -v "^$" || echo "  (无自定义规则)"
    echo ""
    
    echo "选择操作:"
    echo "  1. 添加 IP 地址绕过 (如 1.2.3.4 或 10.0.0.0/8)"
    echo "  2. 添加域名绕过 (如 baidu.com)"
    echo "  3. 添加域名关键词匹配 (输入关键词，用逗号分隔)"
    echo "  4. 使用编辑器打开规则文件"
    echo "  5. 重置为默认规则"
    echo "  0. 返回"
    echo ""
    read -p "选择: " rule_choice
    
    case $rule_choice in
        1)
            echo ""
            echo "IP 地址格式说明:"
            echo "  - 单个 IP: 192.168.1.1"
            echo "  - IP 段:   10.0.0.0/8 (表示 10.x.x.x 整个网段)"
            echo ""
            read -p "输入 IP 地址: " ip_rule
            if [[ -n "$ip_rule" ]]; then
                echo "$ip_rule" >> "$RULES_FILE"
                print_success "已添加: $ip_rule"
            fi
            ;;
        2)
            echo ""
            echo "域名格式说明:"
            echo "  - 精确域名: example.com"
            echo "  - 通配域名: *.example.com (匹配所有子域名)"
            echo ""
            read -p "输入域名: " domain_rule
            if [[ -n "$domain_rule" ]]; then
                echo "$domain_rule" >> "$RULES_FILE"
                print_success "已添加: $domain_rule"
            fi
            ;;
        3)
            echo ""
            echo "域名关键词匹配 - 包含这些关键词的域名将绕过代理"
            echo "示例: cn,baidu,taobao,aliyun"
            echo ""
            read -p "输入关键词 (逗号分隔): " keywords
            if [[ -n "$keywords" ]]; then
                # 将逗号分隔的关键词转换为通配符域名规则
                IFS=',' read -ra KEYWORD_ARRAY <<< "$keywords"
                for kw in "${KEYWORD_ARRAY[@]}"; do
                    kw=$(echo "$kw" | xargs)  # trim
                    if [[ -n "$kw" ]]; then
                        # 添加为通配符域名
                        echo "*${kw}*" >> "$RULES_FILE"
                        print_success "已添加: *${kw}* (匹配包含 '$kw' 的域名)"
                    fi
                done
            fi
            ;;
        4)
            ${EDITOR:-nano} "$RULES_FILE"
            ;;
        5)
            create_default_rules
            print_success "规则已重置"
            ;;
    esac
    
    # 提示重新生成配置
    if [[ "$rule_choice" =~ ^[1-5]$ ]]; then
        read -p "是否重新生成配置并重启? (y/n): " regen
        if [[ "$regen" == "y" ]]; then
            generate_config
            systemctl restart "$CLIENT_SERVICE" 2>/dev/null || true
            print_success "配置已更新并重启"
        fi
    fi
}

#===============================================================================
# TUN 模式管理
#===============================================================================

toggle_tun() {
    if [[ ! -f "$CONFIG_FILE" ]]; then
        print_error "请先配置客户端"
        return
    fi
    
    echo ""
    if grep -q "^tun:" "$CONFIG_FILE"; then
        echo -e "TUN 模式: ${GREEN}已启用${NC}"
        read -p "禁用 TUN 模式? (y/n): " disable
        if [[ "$disable" == "y" ]]; then
            # 注释掉 TUN 配置
            sed -i '/^tun:/,/^[a-z]/{ /^tun:/d; /^  /d; }' "$CONFIG_FILE"
            TUN_ENABLED="false"
            systemctl restart "$CLIENT_SERVICE" 2>/dev/null || true
            print_success "TUN 模式已禁用"
        fi
    else
        echo -e "TUN 模式: ${YELLOW}已禁用${NC}"
        read -p "启用 TUN 模式? (y/n): " enable
        if [[ "$enable" == "y" ]]; then
            TUN_ENABLED="true"
            
            # 配置 rp_filter (Linux TUN 必需)
            print_info "配置系统参数..."
            sysctl -w net.ipv4.conf.default.rp_filter=2 > /dev/null 2>&1
            sysctl -w net.ipv4.conf.all.rp_filter=2 > /dev/null 2>&1
            
            # 重新生成配置
            # 读取现有配置
            local server=$(grep "^server:" "$CONFIG_FILE" | awk '{print $2}')
            local auth=$(grep "^auth:" "$CONFIG_FILE" | awk '{print $2}')
            SOCKS_PORT=$(grep -A1 "^socks5:" "$CONFIG_FILE" | grep "listen:" | sed 's/.*://')
            HTTP_PORT=$(grep -A1 "^http:" "$CONFIG_FILE" | grep "listen:" | sed 's/.*://')
            SERVER_ADDR="$server"
            AUTH_PASSWORD="$auth"
            
            generate_config
            systemctl restart "$CLIENT_SERVICE" 2>/dev/null || true
            print_success "TUN 模式已启用"
            echo ""
            print_warning "注意: SSH 连接已自动保护，不会断开"
        fi
    fi
}

#===============================================================================
# 服务管理
#===============================================================================

create_service() {
    print_info "创建服务..."
    
    # 配置 rp_filter (TUN 模式需要)
    if [[ "$TUN_ENABLED" == "true" ]]; then
        sysctl -w net.ipv4.conf.default.rp_filter=2 > /dev/null 2>&1
        sysctl -w net.ipv4.conf.all.rp_filter=2 > /dev/null 2>&1
        # 持久化
        echo "net.ipv4.conf.default.rp_filter=2" > /etc/sysctl.d/99-hysteria.conf
        echo "net.ipv4.conf.all.rp_filter=2" >> /etc/sysctl.d/99-hysteria.conf
    fi
    
    cat > "/etc/systemd/system/$CLIENT_SERVICE" << EOF
[Unit]
Description=Hysteria2 Client
After=network.target

[Service]
Type=simple
ExecStart=/usr/local/bin/hysteria client --config ${CONFIG_FILE}
Restart=always
RestartSec=5
LimitNOFILE=1048576

[Install]
WantedBy=multi-user.target
EOF

    systemctl daemon-reload
    print_success "服务已创建"
}

start_client() {
    systemctl start "$CLIENT_SERVICE"
    sleep 2
    if systemctl is-active --quiet "$CLIENT_SERVICE"; then
        print_success "客户端已启动"
    else
        print_error "启动失败"
        journalctl -u "$CLIENT_SERVICE" --no-pager -n 5
    fi
}

show_status() {
    echo ""
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo -e "${YELLOW}[系统状态]${NC}"
    
    # 版本信息
    if command -v hysteria &> /dev/null; then
        local hy_ver=$(hysteria version 2>/dev/null | grep "^Version:" | awk '{print $2}' || echo "未知")
        echo -e "  Hysteria2: ${YELLOW}${hy_ver}${NC}"
    fi
    if command -v xray &> /dev/null; then
        local xray_ver=$(xray version 2>/dev/null | head -n1 | awk '{print $2}' || echo "未知")
        echo -e "  Xray:      ${YELLOW}${xray_ver}${NC}"
    fi
    
    # Hysteria2 服务状态
    if systemctl is-active --quiet "$CLIENT_SERVICE" 2>/dev/null; then
        echo -e "  Hysteria2 服务: ${GREEN}✓ 运行中${NC}"
    else
        echo -e "  Hysteria2 服务: ${RED}✗ 未运行${NC}"
    fi
    
    # Xray 服务状态
    if systemctl is-active --quiet xray-client 2>/dev/null; then
        echo -e "  Xray 服务:      ${GREEN}✓ 运行中${NC}"
    elif [[ -f /etc/systemd/system/xray-client.service ]]; then
        echo -e "  Xray 服务:      ${RED}✗ 未运行${NC}"
    fi
    
    # 配置信息
    if [[ -f "$CONFIG_FILE" ]]; then
        local server=$(grep "^server:" "$CONFIG_FILE" 2>/dev/null | awk '{print $2}')
        local socks=$(grep -A1 "^socks5:" "$CONFIG_FILE" 2>/dev/null | grep "listen:" | awk '{print $2}')
        local http=$(grep -A1 "^http:" "$CONFIG_FILE" 2>/dev/null | grep "listen:" | awk '{print $2}')
        local tun_status="禁用"
        grep -q "^tun:" "$CONFIG_FILE" && tun_status="启用"
        
        echo ""
        echo -e "${YELLOW}[Hysteria2 配置]${NC}"
        echo -e "  服务器:   ${GREEN}${server:-未配置}${NC}"
        echo -e "  SOCKS5:   ${GREEN}${socks:-未配置}${NC}"
        echo -e "  HTTP:     ${GREEN}${http:-未配置}${NC}"
        echo -e "  TUN 模式: ${GREEN}${tun_status}${NC}"
    fi
    
    # Xray 配置
    local xray_config="${BASE_DIR}/xray-config.json"
    if [[ -f "$xray_config" ]]; then
        local xray_server=$(grep -o '"address": "[^"]*"' "$xray_config" 2>/dev/null | head -1 | cut -d'"' -f4)
        local xray_port=$(grep -o '"port": [0-9]*' "$xray_config" 2>/dev/null | head -1 | awk '{print $2}')
        echo ""
        echo -e "${YELLOW}[VLESS-Reality 配置]${NC}"
        echo -e "  服务器:   ${GREEN}${xray_server:-未配置}:${xray_port:-未配置}${NC}"
    fi
    
    # 开机自启动状态
    echo ""
    echo -e "${YELLOW}[开机自启动]${NC}"
    local hy_auto=$(systemctl is-enabled "$CLIENT_SERVICE" 2>/dev/null); hy_auto=${hy_auto:-未配置}
    local xray_auto=$(systemctl is-enabled xray-client 2>/dev/null); xray_auto=${xray_auto:-未配置}
    if [[ "$hy_auto" == "enabled" ]]; then
        echo -e "  Hysteria2: ${GREEN}✓ 已启用${NC}"
    else
        echo -e "  Hysteria2: ${RED}✗ 未启用${NC}"
    fi
    if [[ -f /etc/systemd/system/xray-client.service ]]; then
        if [[ "$xray_auto" == "enabled" ]]; then
            echo -e "  Xray:      ${GREEN}✓ 已启用${NC}"
        else
            echo -e "  Xray:      ${RED}✗ 未启用${NC}"
        fi
    fi
    
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
}

test_proxy() {
    print_info "测试代理..."
    
    local port=$(grep -A1 "^socks5:" "$CONFIG_FILE" 2>/dev/null | grep "listen:" | sed 's/.*://')
    
    if curl -s --max-time 10 --socks5 "127.0.0.1:${port:-1080}" https://www.google.com > /dev/null 2>&1; then
        print_success "代理连接正常"
    else
        print_warning "无法访问 Google，检查配置"
    fi
}

#===============================================================================
# 一键安装
#===============================================================================

quick_install() {
    print_info "一键安装..."
    echo ""
    
    install_hysteria
    configure_client
    create_service
    start_client
    
    echo ""
    echo -e "${GREEN}════════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}  安装完成！${NC}"
    echo -e "${GREEN}════════════════════════════════════════════════════════════════${NC}"
    echo ""
    echo -e "  SOCKS5: ${YELLOW}127.0.0.1:${SOCKS_PORT}${NC}"
    echo -e "  HTTP:   ${YELLOW}127.0.0.1:${HTTP_PORT}${NC}"
    if [[ "$TUN_ENABLED" == "true" ]]; then
        echo -e "  TUN:    ${GREEN}已启用 (全局代理)${NC}"
    fi
    echo ""
}

#===============================================================================
# 卸载
#===============================================================================

uninstall() {
    echo -e "${RED}警告: 卸载 Hysteria2 客户端${NC}"
    read -p "输入 'YES' 确认: " confirm
    
    if [[ "$confirm" == "YES" ]]; then
        systemctl stop "$CLIENT_SERVICE" 2>/dev/null || true
        systemctl disable "$CLIENT_SERVICE" 2>/dev/null || true
        rm -f "/etc/systemd/system/$CLIENT_SERVICE"
        rm -f /etc/sysctl.d/99-hysteria.conf
        systemctl daemon-reload
        rm -rf "$BASE_DIR"
        
        read -p "删除 Hysteria2 程序? (y/n): " del_bin
        [[ "$del_bin" == "y" ]] && rm -f /usr/local/bin/hysteria
        
        print_success "卸载完成"
    fi
}

#===============================================================================
# 主菜单
#===============================================================================

show_menu() {
    echo ""
    echo -e "${CYAN}╔══════════════════════════════════════════════════════════════╗${NC}"
    echo -e "${CYAN}║${NC}              ${GREEN}B-UI 客户端 操作菜单${NC}                            ${CYAN}║${NC}"
    echo -e "${CYAN}╠══════════════════════════════════════════════════════════════╣${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}1.${NC} ${GREEN}从链接导入配置${NC} (Hysteria2 / VLESS-Reality)           ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}2.${NC} 手动配置 Hysteria2                                     ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}3.${NC} 启动/停止服务                                          ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}4.${NC} 重启服务                                               ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}5.${NC} 查看日志                                               ${CYAN}║${NC}"
    echo -e "${CYAN}╠══════════════════════════════════════════════════════════════╣${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}6.${NC} TUN 模式开关 (全局代理)                                ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}7.${NC} 编辑路由规则                                           ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}8.${NC} 测试代理连接                                           ${CYAN}║${NC}"
    echo -e "${CYAN}╠══════════════════════════════════════════════════════════════╣${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}9.${NC} 更新内核 (Hysteria2 + Xray)                            ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}10.${NC} 开机自启动设置                                         ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}11.${NC} ${RED}卸载${NC}                                                  ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}0.${NC} 退出                                                   ${CYAN}║${NC}"
    echo -e "${CYAN}╚══════════════════════════════════════════════════════════════╝${NC}"
}

main() {
    check_root
    check_os
    check_dependencies
    
    while true; do
        print_banner
        show_status
        show_menu
        read -p "请选择 [0-11]: " choice
        
        case $choice in
            1) import_from_uri ;;
            2) quick_install ;;
            3) 
                # 启动/停止
                if systemctl is-active --quiet "$CLIENT_SERVICE" 2>/dev/null; then
                    systemctl stop "$CLIENT_SERVICE"
                    systemctl stop xray-client 2>/dev/null || true
                    print_success "服务已停止"
                else
                    systemctl start "$CLIENT_SERVICE" 2>/dev/null || true
                    systemctl start xray-client 2>/dev/null || true
                    print_success "服务已启动"
                fi
                ;;
            4) 
                # 重启
                systemctl restart "$CLIENT_SERVICE" 2>/dev/null || true
                systemctl restart xray-client 2>/dev/null || true
                print_success "服务已重启"
                ;;
            5) 
                # 查看日志
                echo ""
                echo -e "${YELLOW}选择日志类型:${NC}"
                echo "  1. Hysteria2"
                echo "  2. Xray"
                read -p "请选择 [1-2]: " log_choice
                case $log_choice in
                    1) journalctl -u "$CLIENT_SERVICE" --no-pager -n 30 ;;
                    2) journalctl -u xray-client --no-pager -n 30 ;;
                    *) print_error "无效选项" ;;
                esac
                ;;
            6) toggle_tun ;;
            7) edit_rules ;;
            8) test_proxy ;;
            9) 
                # 更新内核
                print_info "更新 Hysteria2..."
                local old_hy=$(hysteria version 2>/dev/null | grep "^Version:" | awk '{print $2}' || echo "未知")
                HYSTERIA_USER=root bash <(curl -fsSL https://get.hy2.sh/)
                local new_hy=$(hysteria version 2>/dev/null | grep "^Version:" | awk '{print $2}' || echo "未知")
                echo -e "  Hysteria2: ${YELLOW}${old_hy}${NC} -> ${GREEN}${new_hy}${NC}"
                
                if command -v xray &> /dev/null; then
                    print_info "更新 Xray..."
                    local old_xray=$(xray version 2>/dev/null | head -n1 | awk '{print $2}' || echo "未知")
                    bash -c "$(curl -L https://github.com/XTLS/Xray-install/raw/main/install-release.sh)" @ install
                    local new_xray=$(xray version 2>/dev/null | head -n1 | awk '{print $2}' || echo "未知")
                    echo -e "  Xray: ${YELLOW}${old_xray}${NC} -> ${GREEN}${new_xray}${NC}"
                fi
                print_success "内核更新完成"
                ;;
            10)
                # 开机自启动
                local hy_auto=$(systemctl is-enabled "$CLIENT_SERVICE" 2>/dev/null); hy_auto=${hy_auto:-disabled}
                echo ""
                if [[ "$hy_auto" == "enabled" ]]; then
                    echo -e "当前状态: ${GREEN}已启用${NC}"
                    read -p "关闭开机自启动? (y/n): " disable
                    if [[ "$disable" == "y" ]]; then
                        systemctl disable "$CLIENT_SERVICE" 2>/dev/null || true
                        systemctl disable xray-client 2>/dev/null || true
                        print_success "已关闭开机自启动"
                    fi
                else
                    echo -e "当前状态: ${RED}未启用${NC}"
                    read -p "开启开机自启动? (y/n): " enable
                    if [[ "$enable" == "y" ]]; then
                        systemctl enable "$CLIENT_SERVICE" 2>/dev/null || true
                        systemctl enable xray-client 2>/dev/null || true
                        print_success "已开启开机自启动"
                    fi
                fi
                ;;
            11) uninstall ;;
            0) echo ""; print_info "再见！"; exit 0 ;;
            *) print_error "无效选项" ;;
        esac
        
        echo ""
        read -p "按 Enter 继续..."
    done
}

#===============================================================================
# 创建全局命令
#===============================================================================

SCRIPT_URL="https://raw.githubusercontent.com/Buxiulei/b-ui/main/b-ui-client.sh"

create_global_command() {
    print_info "创建全局命令 b-ui-client..."
    
    # 如果是通过管道运行 (bash <(curl ...))，需要重新下载
    if [[ ! -f "$0" || "$0" == *"/dev/fd/"* || "$0" == "bash" ]]; then
        print_info "从 GitHub 下载脚本..."
        curl -fsSL "$SCRIPT_URL" -o /usr/local/bin/b-ui-client
    else
        # 如果是本地文件，直接复制
        cp "$0" /usr/local/bin/b-ui-client
    fi
    
    chmod +x /usr/local/bin/b-ui-client
    print_success "全局命令已创建，可使用 'sudo b-ui-client' 运行"
}

# 如果是首次运行，提示创建全局命令
if [[ ! -f /usr/local/bin/b-ui-client && "$0" != "/usr/local/bin/b-ui-client" ]]; then
    read -p "是否创建全局命令 'b-ui-client'? (y/n): " create_cmd
    [[ "$create_cmd" == "y" ]] && create_global_command
fi

main "$@"
