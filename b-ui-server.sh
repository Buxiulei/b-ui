#!/bin/bash

#===============================================================================
# Hysteria2 一键安装脚本 (含 Web 管理面板)
# 功能：安装 Hysteria2、配置多用户、Web 管理面板、BBR 优化
# 官方文档：https://v2.hysteria.network/zh/
# 版本: 2.3.0
#===============================================================================

SCRIPT_VERSION="3.0.2"

set -e

# 颜色定义
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m'

# 路径配置 (使用固定目录，确保系统服务可以访问)
BASE_DIR="/opt/hysteria"
CONFIG_FILE="${BASE_DIR}/config.yaml"
USERS_FILE="${BASE_DIR}/users.json"
ADMIN_DIR="${BASE_DIR}/admin"
HYSTERIA_SERVICE="hysteria-server.service"
ADMIN_SERVICE="b-ui-admin.service"

# 全局变量
DOMAIN=""
EMAIL=""
PORT="10000"
ADMIN_PORT="8080"
ADMIN_PASSWORD=""

#===============================================================================
# 工具函数
#===============================================================================

print_banner() {
    clear
    echo -e "${CYAN}"
    echo "╔══════════════════════════════════════════════════════════════╗"
    echo "║                                                              ║"
    echo "║     Hysteria2 一键安装脚本 + Web 管理面板                    ║"
    echo "║                                                              ║"
    echo "║     支持：多用户 / 自动证书 / 流量统计 / BBR                ║"
    echo "║                                                              ║"
    echo -e "║     版本: ${YELLOW}${SCRIPT_VERSION}${CYAN}                                           ║"
    echo "╚══════════════════════════════════════════════════════════════╝"
    echo -e "${NC}"
}

print_info() { echo -e "${BLUE}[INFO]${NC} $1"; }
print_success() { echo -e "${GREEN}[SUCCESS]${NC} $1"; }
print_warning() { echo -e "${YELLOW}[WARNING]${NC} $1"; }
print_error() { echo -e "${RED}[ERROR]${NC} $1"; }

check_root() {
    if [[ $EUID -ne 0 ]]; then
        print_error "此脚本需要 root 权限运行"
        print_info "请使用以下命令运行:"
        echo -e "  ${YELLOW}curl -fsSL https://raw.githubusercontent.com/Buxiulei/b-ui/main/b-ui-server.sh -o b-ui-server.sh${NC}"
        echo -e "  ${YELLOW}sudo bash b-ui-server.sh${NC}"
        exit 1
    fi
}

check_os() {
    if [[ -f /etc/os-release ]]; then
        . /etc/os-release
        OS=$ID
        OS_VERSION=$VERSION_ID
    else
        print_error "无法识别操作系统"
        exit 1
    fi
    print_info "检测到操作系统: $OS $OS_VERSION"
    
    if ! command -v systemctl &> /dev/null; then
        print_error "此系统不支持 systemd，无法继续安装"
        exit 1
    fi
}

check_dependencies() {
    print_info "检查并安装必要的依赖..."
    
    # 定义需要检查的命令及其对应的包名
    # 格式: "命令:apt包名:yum/dnf包名"
    local deps_map=(
        "curl:curl:curl"
        "grep:grep:grep"
        "awk:gawk:gawk"
        "sed:sed:sed"
        "dig:dnsutils:bind-utils"
        "host:bind9-host:bind-utils"
        "openssl:openssl:openssl"
        "jq:jq:jq"
        "ss:iproute2:iproute"
        "tar:tar:tar"
        "gzip:gzip:gzip"
        "crontab:cron:cronie"
    )
    
    local apt_pkgs=()
    local yum_pkgs=()
    local missing_cmds=()
    
    for item in "${deps_map[@]}"; do
        IFS=':' read -r cmd apt_pkg yum_pkg <<< "$item"
        if ! command -v "$cmd" &> /dev/null; then
            missing_cmds+=("$cmd")
            apt_pkgs+=("$apt_pkg")
            yum_pkgs+=("$yum_pkg")
        fi
    done
    
    if [[ ${#missing_cmds[@]} -gt 0 ]]; then
        print_warning "缺少以下工具: ${missing_cmds[*]}"
        print_info "正在安装依赖包..."
        
        # 去重
        apt_pkgs=($(printf '%s\n' "${apt_pkgs[@]}" | sort -u))
        yum_pkgs=($(printf '%s\n' "${yum_pkgs[@]}" | sort -u))
        
        if command -v apt-get &> /dev/null; then
            apt-get update -qq
            apt-get install -y "${apt_pkgs[@]}"
        elif command -v yum &> /dev/null; then
            yum install -y "${yum_pkgs[@]}"
        elif command -v dnf &> /dev/null; then
            dnf install -y "${yum_pkgs[@]}"
        else
            print_error "无法识别的包管理器，请手动安装: ${missing_cmds[*]}"
            exit 1
        fi
        
        # 验证安装结果
        local still_missing=()
        for cmd in "${missing_cmds[@]}"; do
            if ! command -v "$cmd" &> /dev/null; then
                still_missing+=("$cmd")
            fi
        done
        
        if [[ ${#still_missing[@]} -gt 0 ]]; then
            print_error "以下工具安装失败: ${still_missing[*]}"
            exit 1
        fi
        
        print_success "所有依赖已安装完成"
    else
        print_success "所有依赖检查通过"
    fi
}

#===============================================================================
# 网络检测
#===============================================================================

SERVER_IP=""

get_server_ip() {
    print_info "获取服务器公网 IP..."
    SERVER_IP=$(curl -s4 --max-time 5 ip.sb 2>/dev/null)
    
    if [[ -z "$SERVER_IP" ]]; then
        SERVER_IP=$(curl -s4 --max-time 5 ifconfig.me 2>/dev/null)
    fi
    
    if [[ -z "$SERVER_IP" ]]; then
        SERVER_IP=$(curl -s4 --max-time 5 api.ipify.org 2>/dev/null)
    fi
    
    if [[ -n "$SERVER_IP" ]]; then
        print_success "服务器 IP: $SERVER_IP"
    else
        print_warning "无法获取服务器公网 IP，请确保网络连接正常"
        read -p "手动输入服务器 IP (或按 Enter 跳过): " SERVER_IP
    fi
}

verify_domain_dns() {
    local domain="$1"
    print_info "验证域名 DNS 解析..."
    
    local resolved_ip=$(dig +short "$domain" A 2>/dev/null | head -1)
    
    if [[ -z "$resolved_ip" ]]; then
        resolved_ip=$(host "$domain" 2>/dev/null | grep "has address" | head -1 | awk '{print $NF}')
    fi
    
    if [[ -z "$resolved_ip" ]]; then
        resolved_ip=$(nslookup "$domain" 2>/dev/null | grep -A1 "Name:" | grep "Address" | awk '{print $2}' | head -1)
    fi
    
    if [[ -z "$resolved_ip" ]]; then
        print_error "无法解析域名 $domain"
        print_info "请确保域名已正确配置 DNS A 记录"
        return 1
    fi
    
    print_info "域名解析 IP: $resolved_ip"
    
    if [[ "$resolved_ip" == "$SERVER_IP" ]]; then
        print_success "域名 DNS 验证通过！"
        return 0
    else
        print_error "域名解析 IP ($resolved_ip) 与服务器 IP ($SERVER_IP) 不匹配！"
        print_info "请检查 DNS 配置，确保 A 记录指向本服务器"
        read -p "是否继续？(y/n): " continue_anyway
        [[ "$continue_anyway" == "y" || "$continue_anyway" == "Y" ]]
        return $?
    fi
}

check_port_accessibility() {
    local port="$1"
    local protocol="${2:-tcp}"
    
    print_info "检测端口 $port ($protocol) 连通性..."
    
    # 方法1: 使用外部服务检测 (针对 TCP)
    if [[ "$protocol" == "tcp" ]]; then
        # 先在本地启动临时监听
        local test_result=""
        
        # 检查本地防火墙是否开放
        local local_open=false
        
        if command -v firewall-cmd &> /dev/null && systemctl is-active --quiet firewalld 2>/dev/null; then
            if firewall-cmd --query-port=${port}/tcp 2>/dev/null || firewall-cmd --query-port=${port}/udp 2>/dev/null; then
                local_open=true
            fi
        elif command -v ufw &> /dev/null && ufw status 2>/dev/null | grep -q "active"; then
            if ufw status | grep -qE "^${port}[/ ]"; then
                local_open=true
            fi
        else
            # 假设没有防火墙或已开放
            local_open=true
        fi
        
        if [[ "$local_open" == "false" ]]; then
            print_warning "端口 $port 在本地防火墙中未开放"
            print_info "脚本将自动配置本地防火墙"
            return 1
        fi
        
        # 使用外部服务检测端口
        local external_check=$(curl -s --max-time 10 "https://ports.yougetsignal.com/short-get-port.php" \
            -d "remoteAddress=${SERVER_IP}&portNumber=${port}" 2>/dev/null | grep -o '"portStatus":"[^"]*"' | cut -d'"' -f4)
        
        if [[ "$external_check" == "open" ]]; then
            print_success "端口 $port 外部可访问"
            return 0
        else
            print_warning "端口 $port 外部不可访问"
            
            if [[ "$local_open" == "true" ]]; then
                echo ""
                print_error "诊断结果: 可能是云服务商安全组/防火墙问题"
                echo -e "  ${YELLOW}请检查以下设置：${NC}"
                echo -e "  1. AWS EC2 → Security Groups → 添加入站规则 TCP/UDP 端口 $port"
                echo -e "  2. 阿里云 ECS → 安全组 → 添加入站规则"
                echo -e "  3. 腾讯云 CVM → 安全组 → 添加入站规则"
                echo -e "  4. 其他云服务商 → 查找安全组/防火墙设置"
                echo ""
            fi
            return 1
        fi
    fi
    
    return 0
}

run_network_checks() {
    echo ""
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}网络环境检测${NC}"
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    
    get_server_ip
    
    echo ""
    print_info "检测关键端口..."
    
    local port80_ok=false
    local port443_ok=false
    
    # 简化检测：检查本地是否有服务占用
    if ss -tuln 2>/dev/null | grep -q ":80 " || netstat -tuln 2>/dev/null | grep -q ":80 "; then
        print_info "端口 80: 已有服务监听"
    else
        print_info "端口 80: 未占用 (将用于 HTTPS 证书验证)"
    fi
    
    if ss -tuln 2>/dev/null | grep -q ":443 " || netstat -tuln 2>/dev/null | grep -q ":443 "; then
        print_warning "端口 443: 已被占用，可能需要先停止相关服务"
    else
        print_info "端口 443: 未占用"
    fi
    
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo ""
}

generate_password() {
    tr -dc 'A-Za-z0-9' < /dev/urandom | head -c 16
}

#===============================================================================
# 预下载客户端安装包
# 解决国内客户端无法直连 GitHub 的问题
#===============================================================================

PACKAGES_DIR="${BASE_DIR}/packages"

download_client_packages() {
    print_info "预下载客户端安装包..."
    mkdir -p "$PACKAGES_DIR"
    
    local arch=$(uname -m)
    local arch_suffix
    case "$arch" in
        x86_64) arch_suffix="amd64" ;;
        aarch64) arch_suffix="arm64" ;;
        *) arch_suffix="amd64" ;;
    esac
    
    # 获取最新版本号
    print_info "获取最新版本信息..."
    
    # Hysteria2 版本
    local hy2_version=$(curl -fsSL --max-time 15 "https://api.github.com/repos/apernet/hysteria/releases/latest" 2>/dev/null | grep '"tag_name"' | sed -E 's/.*"app\/v([^"]+)".*/\1/')
    [[ -z "$hy2_version" ]] && hy2_version="2.6.1"
    
    # Xray 版本
    local xray_version=$(curl -fsSL --max-time 15 "https://api.github.com/repos/XTLS/Xray-core/releases/latest" 2>/dev/null | grep '"tag_name"' | sed -E 's/.*"v([^"]+)".*/\1/')
    [[ -z "$xray_version" ]] && xray_version="25.1.1"
    
    # sing-box 版本
    local singbox_version=$(curl -fsSL --max-time 15 "https://api.github.com/repos/SagerNet/sing-box/releases/latest" 2>/dev/null | grep '"tag_name"' | sed -E 's/.*"v([^"]+)".*/\1/')
    [[ -z "$singbox_version" ]] && singbox_version="1.10.0"
    
    # 保存版本信息
    cat > "$PACKAGES_DIR/versions.json" << EOF
{
  "hysteria2": "${hy2_version}",
  "xray": "${xray_version}",
  "singbox": "${singbox_version}",
  "updated": "$(date -Iseconds)"
}
EOF
    
    # 下载 Hysteria2
    print_info "下载 Hysteria2 v${hy2_version}..."
    local hy2_url="https://github.com/apernet/hysteria/releases/download/app/v${hy2_version}/hysteria-linux-${arch_suffix}"
    if curl -fsSL --max-time 120 "$hy2_url" -o "$PACKAGES_DIR/hysteria-linux-${arch_suffix}" 2>/dev/null; then
        chmod +x "$PACKAGES_DIR/hysteria-linux-${arch_suffix}"
        print_success "Hysteria2 下载完成"
    else
        print_warning "Hysteria2 下载失败，客户端将使用备用方式安装"
    fi
    
    # 下载 Xray
    print_info "下载 Xray v${xray_version}..."
    local xray_url="https://github.com/XTLS/Xray-core/releases/download/v${xray_version}/Xray-linux-64.zip"
    [[ "$arch_suffix" == "arm64" ]] && xray_url="https://github.com/XTLS/Xray-core/releases/download/v${xray_version}/Xray-linux-arm64-v8a.zip"
    if curl -fsSL --max-time 120 "$xray_url" -o "$PACKAGES_DIR/xray-linux-${arch_suffix}.zip" 2>/dev/null; then
        print_success "Xray 下载完成"
    else
        print_warning "Xray 下载失败，客户端将使用备用方式安装"
    fi
    
    # 下载 sing-box
    print_info "下载 sing-box v${singbox_version}..."
    local singbox_url="https://github.com/SagerNet/sing-box/releases/download/v${singbox_version}/sing-box-${singbox_version}-linux-${arch_suffix}.tar.gz"
    if curl -fsSL --max-time 120 "$singbox_url" -o "$PACKAGES_DIR/sing-box-linux-${arch_suffix}.tar.gz" 2>/dev/null; then
        print_success "sing-box 下载完成"
    else
        print_warning "sing-box 下载失败，客户端将使用备用方式安装"
    fi
    
    # 复制客户端脚本
    print_info "准备客户端安装脚本..."
    local client_script_url="https://raw.githubusercontent.com/Buxiulei/b-ui/main/b-ui-client.sh"
    if curl -fsSL --max-time 60 "$client_script_url" -o "$PACKAGES_DIR/b-ui-client.sh" 2>/dev/null; then
        chmod +x "$PACKAGES_DIR/b-ui-client.sh"
        print_success "客户端脚本准备完成"
    else
        print_warning "客户端脚本下载失败"
    fi
    
    # 显示下载结果
    echo ""
    print_info "已下载的安装包:"
    ls -lh "$PACKAGES_DIR" 2>/dev/null | grep -v "^total"
    echo ""
}

#===============================================================================
# Hysteria2 安装
#===============================================================================

install_hysteria() {
    print_info "正在安装 Hysteria2..."
    
    if command -v hysteria &> /dev/null; then
        print_warning "Hysteria2 已安装，版本: $(hysteria version 2>/dev/null | head -n1)"
        read -p "是否重新安装/升级？(y/n): " reinstall
        if [[ "$reinstall" != "y" && "$reinstall" != "Y" ]]; then
            return
        fi
    fi
    
    HYSTERIA_USER=root bash <(curl -fsSL https://get.hy2.sh/)
    
    if command -v hysteria &> /dev/null; then
        print_success "Hysteria2 安装成功！"
        
        # 创建自定义目录并配置 systemd 使用自定义路径
        mkdir -p "$BASE_DIR"
        
        # 创建 systemd 服务覆盖配置
        mkdir -p /etc/systemd/system/hysteria-server.service.d
        cat > /etc/systemd/system/hysteria-server.service.d/override.conf << EOF
[Service]
ExecStart=
ExecStart=/usr/local/bin/hysteria server --config ${CONFIG_FILE}
EOF
        systemctl daemon-reload
        print_info "已配置 Hysteria 使用自定义配置路径: $CONFIG_FILE"
    else
        print_error "Hysteria2 安装失败"
        exit 1
    fi
}

#===============================================================================
# 收集用户配置输入
#===============================================================================

collect_user_input() {
    print_info "配置 Hysteria2 服务器..."
    echo ""
    
    # 获取域名
    while true; do
        read -p "请输入您的域名 (例如: hy2.example.com): " DOMAIN
        while [[ -z "$DOMAIN" ]]; do
            print_error "域名不能为空"
            read -p "请输入您的域名: " DOMAIN
        done
        
        # 验证 DNS 解析
        if verify_domain_dns "$DOMAIN"; then
            break
        else
            read -p "是否重新输入域名？(y/n): " retry
            if [[ "$retry" != "y" && "$retry" != "Y" ]]; then
                print_warning "继续使用域名: $DOMAIN (DNS 验证未通过)"
                break
            fi
        fi
    done
    
    # 获取邮箱
    read -p "请输入邮箱 (用于 Let's Encrypt) [默认: test@gmail.com]: " EMAIL
    EMAIL=${EMAIL:-test@gmail.com}
    while [[ ! "$EMAIL" =~ ^[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}$ ]]; do
        print_error "请输入有效的邮箱"
        read -p "请输入邮箱: " EMAIL
    done
    
    # 获取端口
    read -p "请输入监听端口 [默认: 10000]: " PORT
    PORT=${PORT:-10000}
    
    # 管理面板密码
    DEFAULT_ADMIN_PASS=$(generate_password)
    read -p "请输入管理面板密码 [默认: $DEFAULT_ADMIN_PASS]: " ADMIN_PASSWORD
    ADMIN_PASSWORD=${ADMIN_PASSWORD:-$DEFAULT_ADMIN_PASS}
    
    # 创建第一个用户
    DEFAULT_USER_PASS=$(generate_password)
    read -p "请输入第一个用户名 [默认: 低空飞行]: " FIRST_USER
    FIRST_USER=${FIRST_USER:-低空飞行}
    read -p "请输入用户密码 [默认: $DEFAULT_USER_PASS]: " FIRST_USER_PASS
    FIRST_USER_PASS=${FIRST_USER_PASS:-$DEFAULT_USER_PASS}
    
    # 伪装网站
    read -p "请输入伪装网站 URL [默认: https://www.bing.com/]: " MASQUERADE_URL
    MASQUERADE_URL=${MASQUERADE_URL:-"https://www.bing.com/"}
    
    # 显示配置摘要
    echo ""
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}配置摘要：${NC}"
    echo -e "  域名:       ${YELLOW}${DOMAIN}${NC}"
    echo -e "  端口:       ${YELLOW}${PORT}${NC}"
    echo -e "  管理密码:   ${YELLOW}${ADMIN_PASSWORD}${NC}"
    echo -e "  首个用户:   ${YELLOW}${FIRST_USER}${NC}"
    echo -e "  用户密码:   ${YELLOW}${FIRST_USER_PASS}${NC}"
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
}

#===============================================================================
# 生成 Hysteria2 配置文件
#===============================================================================

configure_hysteria() {
    print_info "生成 Hysteria2 配置文件..."
    
    # 创建目录并设置权限
    mkdir -p "$BASE_DIR"
    chmod 755 "$BASE_DIR"
    
    # 创建用户文件
    cat > "$USERS_FILE" << EOF
[{"username":"${FIRST_USER}","password":"${FIRST_USER_PASS}","createdAt":"$(date -Iseconds)"}]
EOF
    
    # 生成配置文件 — 使用 HTTP 认证模式（与 core.sh 保持一致）
    cat > "$CONFIG_FILE" << EOF
# Hysteria2 服务器配置
# 生成时间: $(date)

listen: :${PORT}

# 使用 certbot 证书 (Nginx 已占用 443 端口，无法使用 ACME)
tls:
  sniGuard: disable
  cert: /etc/letsencrypt/live/${DOMAIN}/fullchain.pem
  key: /etc/letsencrypt/live/${DOMAIN}/privkey.pem

# HTTP 认证 (由管理面板 server.js 处理)
auth:
  type: http
  http:
    url: http://127.0.0.1:${ADMIN_PORT}/auth/hysteria
    insecure: true

# 流量统计 API
trafficStats:
  listen: 127.0.0.1:9999
  secret: ""

# 伪装配置
masquerade:
  type: proxy
  proxy:
    url: ${MASQUERADE_URL}
    rewriteHost: true
EOF

    # 设置文件权限 (确保 Hysteria 服务可以读取)
    chmod 644 "$CONFIG_FILE"
    chmod 644 "$USERS_FILE"
    
    print_success "配置文件已生成: $CONFIG_FILE"
}

#===============================================================================
# BBR 优化
#===============================================================================

check_bbr_status() {
    local cc=$(sysctl -n net.ipv4.tcp_congestion_control 2>/dev/null)
    [[ "$cc" == "bbr" ]]
}

enable_bbr() {
    print_info "配置 BBR 优化..."
    
    local kernel_major=$(uname -r | cut -d'.' -f1)
    local kernel_minor=$(uname -r | cut -d'.' -f2 | cut -d'-' -f1)
    
    if [[ $kernel_major -lt 4 ]] || [[ $kernel_major -eq 4 && $kernel_minor -lt 9 ]]; then
        print_warning "内核版本低于 4.9，不支持 BBR"
        return 1
    fi
    
    if check_bbr_status; then
        print_success "BBR 已启用"
        return 0
    fi
    
    modprobe tcp_bbr 2>/dev/null || true
    
    cat > /etc/sysctl.d/99-hysteria-bbr.conf << EOF
net.core.default_qdisc=fq
net.ipv4.tcp_congestion_control=bbr
EOF
    
    sysctl --system > /dev/null 2>&1
    
    if check_bbr_status; then
        print_success "BBR 启用成功"
    else
        print_warning "BBR 配置完成，可能需要重启生效"
    fi
}

#===============================================================================
# 防火墙配置
#===============================================================================

configure_firewall() {
    local port=${1:-443}
    local admin_port=${2:-8080}
    
    print_info "配置防火墙..."
    
    if command -v firewall-cmd &> /dev/null && systemctl is-active --quiet firewalld; then
        firewall-cmd --permanent --add-port=22/tcp  # SSH
        firewall-cmd --permanent --add-port=${port}/udp
        firewall-cmd --permanent --add-port=${port}/tcp
        firewall-cmd --permanent --add-port=80/tcp
        firewall-cmd --permanent --add-port=443/tcp
        firewall-cmd --permanent --add-port=10001/tcp  # Xray Reality
        firewall-cmd --reload
        print_success "firewalld 规则已添加"
    elif command -v ufw &> /dev/null && ufw status | grep -q "active"; then
        ufw allow 22/tcp  # SSH
        ufw allow ${port}/udp
        ufw allow ${port}/tcp
        ufw allow 80/tcp
        ufw allow 443/tcp
        ufw allow 10001/tcp  # Xray Reality
        print_success "ufw 规则已添加"
    elif command -v iptables &> /dev/null; then
        iptables -I INPUT -p tcp --dport 22 -j ACCEPT  # SSH
        iptables -I INPUT -p udp --dport ${port} -j ACCEPT
        iptables -I INPUT -p tcp --dport ${port} -j ACCEPT
        iptables -I INPUT -p tcp --dport 80 -j ACCEPT
        iptables -I INPUT -p tcp --dport 443 -j ACCEPT
        iptables -I INPUT -p tcp --dport 10001 -j ACCEPT  # Xray Reality
        print_success "iptables 规则已添加"
    else
        print_warning "未检测到防火墙，请手动开放端口"
    fi
}

#===============================================================================
# Node.js 和 Nginx 安装
#===============================================================================

install_nodejs() {
    print_info "检查 Node.js..."
    
    if command -v node &> /dev/null; then
        local ver=$(node -v | cut -d'v' -f2 | cut -d'.' -f1)
        if [[ $ver -ge 14 ]]; then
            print_success "Node.js 已安装: $(node -v)"
            return 0
        fi
    fi
    
    print_info "安装 Node.js 20.x..."
    if command -v apt-get &> /dev/null; then
        curl -fsSL https://deb.nodesource.com/setup_20.x | bash -
        apt-get install -y nodejs
    elif command -v yum &> /dev/null; then
        curl -fsSL https://rpm.nodesource.com/setup_20.x | bash -
        yum install -y nodejs
    elif command -v dnf &> /dev/null; then
        curl -fsSL https://rpm.nodesource.com/setup_20.x | bash -
        dnf install -y nodejs
    fi
    print_success "Node.js 安装完成"
}

install_nginx() {
    print_info "检查 Nginx..."
    
    if command -v nginx &> /dev/null; then
        print_success "Nginx 已安装"
        return 0
    fi
    
    print_info "安装 Nginx..."
    if command -v apt-get &> /dev/null; then
        apt-get update && apt-get install -y nginx
    elif command -v yum &> /dev/null; then
        yum install -y nginx
    elif command -v dnf &> /dev/null; then
        dnf install -y nginx
    fi
    systemctl enable nginx
    print_success "Nginx 安装完成"
}

install_xray() {
    print_info "安装 Xray..."
    
    if command -v xray &> /dev/null; then
        print_success "Xray 已安装: $(xray version 2>/dev/null | head -1 | awk '{print $2}' || echo '版本未知')"
        return 0
    fi
    
    print_info "下载并安装 Xray..."
    bash -c "$(curl -L https://github.com/XTLS/Xray-install/raw/main/install-release.sh)" @ install
    
    if command -v xray &> /dev/null; then
        print_success "Xray 安装完成"
    else
        print_warning "Xray 安装失败，VLESS-Reality 功能将不可用"
    fi
}

generate_reality_keys() {
    print_info "生成 Reality 密钥对..."
    
    if [[ -f "$BASE_DIR/reality-keys.json" ]]; then
        # 检查现有密钥是否有效
        local existing_priv=$(cat "$BASE_DIR/reality-keys.json" 2>/dev/null | grep '"privateKey"' | cut -d'"' -f4)
        if [[ -n "$existing_priv" ]]; then
            print_info "Reality 密钥已存在，跳过生成"
            return 0
        fi
        # 密钥无效，重新生成
        print_warning "发现无效密钥，重新生成..."
    fi
    
    if ! command -v xray &> /dev/null; then
        print_warning "Xray 未安装，跳过密钥生成"
        return 1
    fi
    
    # 新版 Xray x25519 输出格式:
    # PrivateKey: xxx (服务端私钥)
    # Password: xxx (客户端公钥 pbk)
    local keys=$(xray x25519 2>&1)
    local privkey=$(echo "$keys" | grep -i "^PrivateKey:" | awk -F': ' '{print $2}' | tr -d ' ')
    local pubkey=$(echo "$keys" | grep -i "^Password:" | awk -F': ' '{print $2}' | tr -d ' ')
    
    # 如果新格式失败，尝试旧格式
    if [[ -z "$privkey" ]]; then
        privkey=$(echo "$keys" | grep "Private key:" | awk '{print $3}')
        pubkey=$(echo "$keys" | grep "Public key:" | awk '{print $3}')
    fi
    
    local shortid=$(openssl rand -hex 8)
    
    if [[ -z "$privkey" ]]; then
        print_error "无法生成 Reality 密钥"
        return 1
    fi
    
    cat > "$BASE_DIR/reality-keys.json" << EOF
{
    "privateKey": "$privkey",
    "publicKey": "$pubkey",
    "shortId": "$shortid"
}
EOF
    chmod 600 "$BASE_DIR/reality-keys.json"
    print_success "Reality 密钥已生成"
}

configure_xray() {
    print_info "配置 Xray (VLESS-Reality)..."
    
    if [[ ! -f "$BASE_DIR/reality-keys.json" ]]; then
        print_warning "Reality 密钥不存在，跳过 Xray 配置"
        return 1
    fi
    
    local privkey=$(cat "$BASE_DIR/reality-keys.json" | grep '"privateKey"' | cut -d'"' -f4)
    local shortid=$(cat "$BASE_DIR/reality-keys.json" | grep '"shortId"' | cut -d'"' -f4)
    
    # 从 MASQUERADE_URL 提取域名用于 Xray Reality，默认 www.bing.com
    local masq_domain="www.bing.com"
    if [[ -n "$MASQUERADE_URL" ]]; then
        masq_domain=$(echo "$MASQUERADE_URL" | sed -E 's|https?://([^/:]+).*|\1|')
    fi
    
    # 保存伪装网站配置供后续修改
    echo "{\"masqueradeUrl\": \"$MASQUERADE_URL\", \"masqueradeDomain\": \"$masq_domain\"}" > "$BASE_DIR/masquerade.json"
    
    cat > "$BASE_DIR/xray-config.json" << EOF
{
  "log": {"loglevel": "warning"},
  "stats": {},
  "api": {"tag": "api", "services": ["StatsService", "HandlerService"]},
  "policy": {
    "levels": {"0": {"statsUserUplink": true, "statsUserDownlink": true}},
    "system": {"statsInboundUplink": true, "statsInboundDownlink": true}
  },
  "inbounds": [
    {"tag": "api", "port": 10085, "listen": "127.0.0.1", "protocol": "dokodemo-door", "settings": {"address": "127.0.0.1"}},
    {
      "tag": "vless-reality",
      "port": 10001,
      "protocol": "vless",
      "settings": {"clients": [], "decryption": "none"},
      "streamSettings": {
        "network": "tcp",
        "security": "reality",
        "realitySettings": {
          "dest": "${masq_domain}:443",
          "serverNames": ["${masq_domain}"],
          "privateKey": "$privkey",
          "shortIds": ["$shortid"]
        }
      },
      "sniffing": {"enabled": true, "destOverride": ["http", "tls"]}
    }
  ],
  "outbounds": [{"protocol": "freedom", "tag": "direct"}],
  "routing": {"rules": [{"type": "field", "inboundTag": ["api"], "outboundTag": "api"}]}
}
EOF
    chmod 644 "$BASE_DIR/xray-config.json"
    print_success "Xray 配置已生成 (伪装: $masq_domain)"
}

create_xray_service() {
    print_info "配置 Xray 服务..."
    
    if [[ ! -f "$BASE_DIR/xray-config.json" ]]; then
        print_warning "Xray 配置不存在，跳过服务配置"
        return 1
    fi
    
    # 创建 systemd 服务覆盖配置
    mkdir -p /etc/systemd/system/xray.service.d
    cat > /etc/systemd/system/xray.service.d/override.conf << EOF
[Service]
ExecStart=
ExecStart=/usr/local/bin/xray run -config $BASE_DIR/xray-config.json
EOF
    
    systemctl daemon-reload
    systemctl enable xray
    systemctl restart xray
    
    sleep 2
    if systemctl is-active --quiet xray; then
        print_success "Xray 服务已启动"
    else
        print_warning "Xray 服务启动失败，请检查日志"
    fi
}

install_chinese_fonts() {
    print_info "安装中文字体 (Noto Sans CJK)..."
    
    if command -v apt-get &> /dev/null; then
        apt-get install -y fonts-noto-cjk
    elif command -v yum &> /dev/null; then
        yum install -y google-noto-sans-cjk-sc-fonts
    elif command -v dnf &> /dev/null; then
        dnf install -y google-noto-sans-cjk-sc-fonts
    fi
    
    # 刷新字体缓存
    if command -v fc-cache &> /dev/null; then
        fc-cache -fv > /dev/null 2>&1
    fi
    
    print_success "中文字体安装完成"
}

#===============================================================================
# Web 管理面板部署
#===============================================================================

GITHUB_RAW="https://raw.githubusercontent.com/Buxiulei/b-ui/main"
GITHUB_CDN="https://cdn.jsdelivr.net/gh/Buxiulei/b-ui@main"

# 从 GitHub 下载文件（带 CDN 回退）
download_from_github() {
    local path="$1"  # 相对路径，如 web/server.js
    local dest="$2"  # 目标路径
    
    if curl -fsSL --max-time 30 "${GITHUB_RAW}/${path}" -o "$dest" 2>/dev/null; then
        return 0
    fi
    
    if curl -fsSL --max-time 30 "${GITHUB_CDN}/${path}" -o "$dest" 2>/dev/null; then
        return 0
    fi
    
    print_error "下载失败: ${path}"
    return 1
}

deploy_admin_panel() {
    print_info "部署 Web 管理面板..."
    
    mkdir -p "$ADMIN_DIR"
    
    # 创建 package.json
    cat > "$ADMIN_DIR/package.json" << 'PKGEOF'
{"name":"b-ui-admin","version":"2.0.0","main":"server.js","scripts":{"start":"node server.js"}}
PKGEOF

    # 从 GitHub 下载管理面板文件（不再内嵌）
    print_info "下载管理面板 server.js..."
    if ! download_from_github "web/server.js" "$ADMIN_DIR/server.js"; then
        print_error "server.js 下载失败，安装无法继续"
        exit 1
    fi
    
    # 下载前端文件
    print_info "下载前端文件..."
    download_from_github "web/index.html" "$ADMIN_DIR/index.html" || true
    download_from_github "web/style.css" "$ADMIN_DIR/style.css" || true
    download_from_github "web/app.js" "$ADMIN_DIR/app.js" || true
    
    # 下载版本信息
    download_from_github "version.json" "${BASE_DIR}/version.json" || true
    
    # 下载服务端管理工具
    print_info "下载服务端管理工具..."
    download_from_github "server/core.sh" "${BASE_DIR}/core.sh" || true
    download_from_github "server/update.sh" "${BASE_DIR}/update.sh" || true
    download_from_github "server/b-ui-cli.sh" "${BASE_DIR}/b-ui-cli.sh" || true
    chmod +x "${BASE_DIR}/update.sh" "${BASE_DIR}/b-ui-cli.sh" 2>/dev/null || true

    print_success "管理面板文件已部署"
}

create_admin_service() {
    print_info "创建管理面板服务..."
    
    # 安装依赖
    cd "$ADMIN_DIR"
    npm install --production 2>/dev/null || true
    
    cat > "/etc/systemd/system/$ADMIN_SERVICE" << EOF
[Unit]
Description=Hysteria2 Admin Panel
After=network.target

[Service]
Type=simple
WorkingDirectory=${ADMIN_DIR}
Environment=ADMIN_PORT=${ADMIN_PORT}
Environment=ADMIN_PASSWORD=${ADMIN_PASSWORD}
Environment=HYSTERIA_CONFIG=${CONFIG_FILE}
Environment=USERS_FILE=${USERS_FILE}
ExecStart=/usr/bin/node ${ADMIN_DIR}/server.js
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF

    systemctl daemon-reload
    systemctl enable "$ADMIN_SERVICE"
    systemctl start "$ADMIN_SERVICE"
    
    sleep 2
    if systemctl is-active --quiet "$ADMIN_SERVICE"; then
        print_success "管理面板服务已启动"
    else
        print_error "管理面板服务启动失败"
        journalctl -u "$ADMIN_SERVICE" --no-pager -n 5
    fi
}

create_bui_cli() {
    print_info "创建 b-ui 命令行工具..."
    
    # 创建轻量入口脚本，实际逻辑由 b-ui-cli.sh 提供
    cat > /usr/local/bin/b-ui << 'BUIEOF'
#!/bin/bash
# B-UI 终端管理面板 - 入口脚本
# 实际逻辑由 /opt/hysteria/b-ui-cli.sh 提供
BASE_DIR="/opt/hysteria"
CLI_SCRIPT="${BASE_DIR}/b-ui-cli.sh"

if [[ $EUID -ne 0 ]]; then
    echo -e "\033[0;31m[ERROR]\033[0m 请使用 sudo b-ui 运行"
    exit 1
fi

if [[ -f "$CLI_SCRIPT" ]]; then
    source "$CLI_SCRIPT"
else
    echo -e "\033[0;31m[ERROR]\033[0m CLI 工具未找到: $CLI_SCRIPT"
    echo -e "\033[0;34m[INFO]\033[0m 尝试重新下载..."
    curl -fsSL "https://raw.githubusercontent.com/Buxiulei/b-ui/main/server/b-ui-cli.sh" -o "$CLI_SCRIPT" && chmod +x "$CLI_SCRIPT" && source "$CLI_SCRIPT"
fi
BUIEOF
    
    chmod +x /usr/local/bin/b-ui
    print_success "b-ui 命令已创建，可在终端输入 'sudo b-ui' 打开管理面板"
}

configure_nginx_proxy() {
    print_info "配置 Nginx HTTPS 反向代理..."
    
    # 安装 certbot
    print_info "安装 Certbot..."
    if command -v apt-get &> /dev/null; then
        apt-get install -y certbot python3-certbot-nginx
    elif command -v yum &> /dev/null; then
        yum install -y certbot python3-certbot-nginx
    elif command -v dnf &> /dev/null; then
        dnf install -y certbot python3-certbot-nginx
    fi
    
    # 先创建 HTTP 配置用于证书验证
    cat > "/etc/nginx/conf.d/b-ui-admin.conf" << EOF
server {
    listen 80;
    server_name ${DOMAIN};
    
    location /.well-known/acme-challenge/ {
        root /var/www/html;
    }
    
    location / {
        proxy_pass http://127.0.0.1:${ADMIN_PORT};
        proxy_http_version 1.1;
        proxy_set_header Host \$host;
        proxy_set_header X-Real-IP \$remote_addr;
    }
}
EOF
    
    mkdir -p /var/www/html
    nginx -t && systemctl reload nginx
    
    # 检测端口 80 是否可从外部访问
    print_info "检测端口 80 连通性..."
    
    # 创建临时测试文件
    local test_id=$(head /dev/urandom | tr -dc 'a-z0-9' | head -c 8)
    mkdir -p /var/www/html/.well-known/acme-challenge
    echo "test-${test_id}" > /var/www/html/.well-known/acme-challenge/test-${test_id}
    
    # 等待 nginx 加载
    sleep 2
    
    # 尝试从外部访问
    local port80_ok=false
    local test_result=$(curl -s --max-time 10 "http://${DOMAIN}/.well-known/acme-challenge/test-${test_id}" 2>/dev/null)
    
    if [[ "$test_result" == "test-${test_id}" ]]; then
        port80_ok=true
        print_success "端口 80 可正常访问"
    else
        print_error "端口 80 无法从外部访问！"
        echo ""
        echo -e "${RED}╔══════════════════════════════════════════════════════════════╗${NC}"
        echo -e "${RED}║            SSL 证书申请将失败 - 请先解决端口问题                  ║${NC}"
        echo -e "${RED}╚══════════════════════════════════════════════════════════════╝${NC}"
        echo ""
        echo -e "${YELLOW}如果您使用云服务器，请在云平台控制台开放端口 80：${NC}"
        echo ""
        echo -e "  ${CYAN}AWS EC2:${NC}"
        echo -e "    1. 进入 EC2 控制台 → Security Groups"
        echo -e "    2. 选择实例使用的安全组"
        echo -e "    3. 添加入站规则: Type=HTTP, Port=80, Source=0.0.0.0/0"
        echo ""
        echo -e "  ${CYAN}阿里云 ECS:${NC}"
        echo -e "    1. 进入 ECS 控制台 → 安全组"
        echo -e "    2. 添加入站规则: 端口 80/80, 授权对象 0.0.0.0/0"
        echo ""
        echo -e "  ${CYAN}腾讯云 CVM:${NC}"
        echo -e "    1. 进入 CVM 控制台 → 安全组"
        echo -e "    2. 添加入站规则: 端口 80, 来源 0.0.0.0/0"
        echo ""
        
        read -p "已开放端口 80 后，按 Enter 重试，或输入 'skip' 跳过 SSL: " retry_choice
        
        if [[ "$retry_choice" != "skip" ]]; then
            # 重新测试
            test_result=$(curl -s --max-time 10 "http://${DOMAIN}/.well-known/acme-challenge/test-${test_id}" 2>/dev/null)
            if [[ "$test_result" == "test-${test_id}" ]]; then
                port80_ok=true
                print_success "端口 80 现在可以访问了！"
            else
                print_warning "端口仍然无法访问，跳过 SSL 证书申请"
            fi
        fi
    fi
    
    # 清理测试文件
    rm -f /var/www/html/.well-known/acme-challenge/test-${test_id}
    
    # 检查是否已有有效证书（避免重复申请导致 Let's Encrypt 速率限制）
    local cert_exists=false
    local cert_path="/etc/letsencrypt/live/${DOMAIN}/fullchain.pem"
    if [[ -f "$cert_path" ]]; then
        # 检查证书是否还有效（至少还有7天有效期）
        local expiry_date=$(openssl x509 -enddate -noout -in "$cert_path" 2>/dev/null | cut -d= -f2)
        if [[ -n "$expiry_date" ]]; then
            local expiry_epoch=$(date -d "$expiry_date" +%s 2>/dev/null || date -j -f "%b %d %T %Y %Z" "$expiry_date" +%s 2>/dev/null)
            local now_epoch=$(date +%s)
            local days_left=$(( (expiry_epoch - now_epoch) / 86400 ))
            if [[ $days_left -gt 7 ]]; then
                cert_exists=true
                print_success "检测到有效的 SSL 证书（剩余 ${days_left} 天），跳过申请"
            else
                print_warning "证书即将过期（剩余 ${days_left} 天），尝试续期..."
            fi
        fi
    fi
    
    # 申请证书（仅在没有有效证书时）
    if [[ "$cert_exists" == "false" ]] && [[ "$port80_ok" == "true" ]]; then
        print_info "申请 SSL 证书..."
        certbot --nginx -d "${DOMAIN}" --non-interactive --agree-tos -m "${EMAIL}" --redirect
        
        if [[ $? -eq 0 ]]; then
            print_success "SSL 证书申请成功！"
            
            # 修复证书目录权限 (让 Hysteria 服务可以读取)
            chmod 755 /etc/letsencrypt 2>/dev/null || true
            chmod 755 /etc/letsencrypt/live 2>/dev/null || true
            chmod 755 /etc/letsencrypt/live/${DOMAIN} 2>/dev/null || true
            chmod 755 /etc/letsencrypt/archive 2>/dev/null || true
            chmod 755 /etc/letsencrypt/archive/${DOMAIN} 2>/dev/null || true
            chmod 644 /etc/letsencrypt/archive/${DOMAIN}/*.pem 2>/dev/null || true
            
            # 设置证书自动续期
            # 移除旧的不带 deploy-hook 的 cron 任务
            crontab -l 2>/dev/null | grep -v "certbot renew" | crontab - 2>/dev/null || true
            (crontab -l 2>/dev/null; echo '0 3 * * * certbot renew --quiet --deploy-hook "systemctl restart hysteria-server && systemctl reload nginx 2>/dev/null || true"') | crontab -
            print_info "已设置证书自动续期 (每天 3:00, 续期后自动重启服务)"
        else
            print_warning "SSL 证书申请失败，将使用 HTTP"
        fi
    elif [[ "$cert_exists" == "true" ]]; then
        # 证书已存在，确保权限正确
        chmod 755 /etc/letsencrypt 2>/dev/null || true
        chmod 755 /etc/letsencrypt/live 2>/dev/null || true
        chmod 755 /etc/letsencrypt/live/${DOMAIN} 2>/dev/null || true
        chmod 755 /etc/letsencrypt/archive 2>/dev/null || true
        chmod 755 /etc/letsencrypt/archive/${DOMAIN} 2>/dev/null || true
        chmod 644 /etc/letsencrypt/archive/${DOMAIN}/*.pem 2>/dev/null || true
        
        # 确保自动续期已设置
        # 移除旧的不带 deploy-hook 的 cron 任务
        crontab -l 2>/dev/null | grep -v "certbot renew" | crontab - 2>/dev/null || true
        (crontab -l 2>/dev/null; echo '0 3 * * * certbot renew --quiet --deploy-hook "systemctl restart hysteria-server && systemctl reload nginx 2>/dev/null || true"') | crontab -
        print_info "已设置证书自动续期 (每天 3:00, 续期后自动重启服务)"
    else
        print_warning "跳过 SSL 证书申请，管理面板将使用 HTTP"
        print_info "稍后可以手动运行 certbot 申请证书"
    fi
    
    print_success "Nginx 配置完成"
}

#===============================================================================
# 服务管理
#===============================================================================

start_hysteria() {
    print_info "启动 Hysteria2 服务..."
    
    # 确保目录和文件权限正确
    chmod 755 "$BASE_DIR" 2>/dev/null || true
    chmod 644 "$CONFIG_FILE" 2>/dev/null || true
    chmod 644 "$USERS_FILE" 2>/dev/null || true
    
    # 确保证书目录可访问 (certbot 创建的目录默认权限过严)
    chmod 755 /etc/letsencrypt 2>/dev/null || true
    chmod 755 /etc/letsencrypt/live 2>/dev/null || true
    chmod 755 /etc/letsencrypt/archive 2>/dev/null || true
    if [[ -n "$DOMAIN" && -d "/etc/letsencrypt/archive/$DOMAIN" ]]; then
        chmod 755 /etc/letsencrypt/live/$DOMAIN 2>/dev/null || true
        chmod 755 /etc/letsencrypt/archive/$DOMAIN 2>/dev/null || true
        chmod 644 /etc/letsencrypt/archive/$DOMAIN/*.pem 2>/dev/null || true
    fi
    
    systemctl daemon-reload
    systemctl enable "$HYSTERIA_SERVICE" --now
    sleep 2
    if systemctl is-active --quiet "$HYSTERIA_SERVICE"; then
        print_success "Hysteria2 服务已启动"
    else
        print_error "服务启动失败"
        journalctl -u "$HYSTERIA_SERVICE" --no-pager -n 10
    fi
}

#===============================================================================
# 简化状态显示（仅安装完成后使用，完整功能由 b-ui CLI 提供）
#===============================================================================

show_install_status() {
    echo ""
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}服务状态${NC}"
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    
    if command -v hysteria &> /dev/null; then
        echo -e "  Hysteria2: ${YELLOW}$(hysteria version 2>/dev/null | head -n1 || echo '未知')${NC}"
    else
        echo -e "  Hysteria2: ${RED}未安装${NC}"
    fi
    
    if systemctl is-active --quiet "$HYSTERIA_SERVICE"; then
        echo -e "  Hysteria服务: ${GREEN}运行中${NC}"
    else
        echo -e "  Hysteria服务: ${RED}未运行${NC}"
    fi
    
    if command -v xray &> /dev/null; then
        if systemctl is-active --quiet xray 2>/dev/null; then
            echo -e "  Xray服务: ${GREEN}运行中${NC}"
        else
            echo -e "  Xray服务: ${RED}未运行${NC}"
        fi
    fi
    
    if systemctl is-active --quiet "$ADMIN_SERVICE" 2>/dev/null; then
        echo -e "  管理面板: ${GREEN}运行中${NC}"
    else
        echo -e "  管理面板: ${YELLOW}未安装${NC}"
    fi
    
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
}

#===============================================================================
# 一键安装编排
#===============================================================================

quick_install() {
    print_info "开始一键安装..."
    echo ""
    
    # 网络环境预检
    run_network_checks
    
    # 1. 收集用户配置 (域名、端口、密码等)
    collect_user_input
    echo ""
    
    # 2. 安装 Hysteria2 和启用 BBR
    install_hysteria
    echo ""
    enable_bbr
    echo ""
    
    # 3. 安装 Web 服务相关组件
    print_info "安装 Web 管理面板..."
    install_nodejs
    install_nginx
    install_chinese_fonts
    
    # 4. 启动 Nginx 并开放端口 (用于 Certbot 验证)
    systemctl start nginx 2>/dev/null || true
    configure_firewall "$PORT" "$ADMIN_PORT"
    
    # 5. 申请 SSL 证书 (此时 DOMAIN 已设置)
    configure_nginx_proxy
    echo ""
    
    # 6. 生成 Hysteria 配置 (此时证书已存在)
    configure_hysteria
    echo ""
    
    # 7. 启动 Hysteria 服务
    start_hysteria
    echo ""
    
    # 8. 安装和配置 Xray (VLESS-Reality)
    install_xray
    generate_reality_keys
    configure_xray
    create_xray_service
    echo ""
    
    # 9. 部署管理面板
    deploy_admin_panel
    create_admin_service
    create_bui_cli
    
    # 10. 预下载客户端安装包 (可选，用于国内客户端)
    echo ""
    read -p "是否预下载客户端安装包 (便于国内客户端安装)? (y/n) [默认 y]: " download_packages
    download_packages=${download_packages:-y}
    if [[ "$download_packages" =~ ^[yY]$ ]]; then
        download_client_packages
    fi

    echo ""
    echo -e "${GREEN}════════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}  安装完成！${NC}"
    echo -e "${GREEN}════════════════════════════════════════════════════════════════${NC}"
    echo ""
    echo -e "  管理面板: ${YELLOW}https://${DOMAIN}${NC}"
    echo -e "  管理密码: ${YELLOW}${ADMIN_PASSWORD}${NC}"
    echo ""
    
    # 安装后自动打开 CLI 管理面板
    echo -e "${CYAN}正在打开 B-UI 终端管理面板...${NC}"
    sleep 2
    b-ui
}

main() {
    check_root
    check_os
    check_dependencies
    
    print_banner
    
    # 检查是否已安装
    if systemctl is-active --quiet "$HYSTERIA_SERVICE" 2>/dev/null; then
        show_install_status
        echo ""
        echo -e "${GREEN}B-UI 已安装，正在打开管理面板...${NC}"
        echo -e "${CYAN}提示：后续管理请直接使用 'sudo b-ui' 命令${NC}"
        echo ""
        sleep 1
        # 委托给已安装的 CLI 工具
        if [[ -x /usr/local/bin/b-ui ]]; then
            exec /usr/local/bin/b-ui
        else
            print_warning "CLI 工具未找到，请重新安装"
        fi
    fi
    
    # 未安装，执行一键安装
    quick_install
}

main "$@"
