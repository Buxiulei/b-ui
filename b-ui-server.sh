#!/bin/bash

#===============================================================================
# Hysteria2 一键安装脚本 (含 Web 管理面板)
# 功能：安装 Hysteria2、配置多用户、Web 管理面板、BBR 优化
# 官方文档：https://v2.hysteria.network/zh/
# 版本: 2.3.0
#===============================================================================

SCRIPT_VERSION="2.3.0"

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
    
    # 生成配置文件 (使用 certbot 证书，因为 Nginx 已占用 443)
    cat > "$CONFIG_FILE" << EOF
# Hysteria2 服务器配置
# 生成时间: $(date)

listen: :${PORT}

# 使用 certbot 证书 (Nginx 已占用 443 端口，无法使用 ACME)
tls:
  cert: /etc/letsencrypt/live/${DOMAIN}/fullchain.pem
  key: /etc/letsencrypt/live/${DOMAIN}/privkey.pem

# 多用户认证
auth:
  type: userpass
  userpass:
    ${FIRST_USER}: ${FIRST_USER_PASS}

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

deploy_admin_panel() {
    print_info "部署 Web 管理面板 (Redesigned UI)..."
    
    mkdir -p "$ADMIN_DIR"
    
    # 创建 package.json
    cat > "$ADMIN_DIR/package.json" << 'PKGEOF'
{"name":"hysteria2-admin","version":"2.0.0","main":"server.js","scripts":{"start":"node server.js"}}
PKGEOF

    # 创建 server.js (内嵌)
    # 创建 server.js (内嵌完整版 - 支持 Hysteria2 + VLESS-Reality)
    cat > "$ADMIN_DIR/server.js" << 'SERVEREOF'
const http=require("http"),fs=require("fs"),crypto=require("crypto"),{execSync,exec}=require("child_process");
const VERSION="2.3.0";
const CONFIG={port:process.env.ADMIN_PORT||8080,adminPassword:process.env.ADMIN_PASSWORD||"admin123",
jwtSecret:process.env.JWT_SECRET||crypto.randomBytes(32).toString("hex"),
hysteriaConfig:process.env.HYSTERIA_CONFIG||"/opt/hysteria/config.yaml",
xrayConfig:process.env.XRAY_CONFIG||"/opt/hysteria/xray-config.json",
xrayKeysFile:process.env.XRAY_KEYS||"/opt/hysteria/reality-keys.json",
usersFile:process.env.USERS_FILE||"/opt/hysteria/users.json",trafficPort:9999,xrayApiPort:10085};

// --- Security: Rate Limiting & Audit ---
const loginAttempts={};const RATE_LIMIT={maxAttempts:5,windowMs:300000};
function checkRateLimit(ip){const now=Date.now(),rec=loginAttempts[ip];if(!rec)return true;if(now-rec.first>RATE_LIMIT.windowMs){delete loginAttempts[ip];return true}return rec.count<RATE_LIMIT.maxAttempts}
function recordAttempt(ip,success){const now=Date.now(),rec=loginAttempts[ip];if(!rec)loginAttempts[ip]={first:now,count:1};else rec.count++;if(success)delete loginAttempts[ip];log("AUDIT",ip+" login "+(success?"SUCCESS":"FAILED")+" (attempts: "+(loginAttempts[ip]?.count||0)+")")}
function getClientIP(req){return req.headers["x-forwarded-for"]?.split(",")[0].trim()||req.socket.remoteAddress||"unknown"}

// --- Backend Logic ---
function log(l,m){console.log("["+new Date().toISOString()+"] ["+l+"] "+m)}
function genToken(d){const p=Buffer.from(JSON.stringify({...d,exp:Date.now()+864e5,iat:Date.now()})).toString("base64");
return p+"."+crypto.createHmac("sha256",CONFIG.jwtSecret).update(p).digest("hex")}
function verifyToken(t){try{const[p,s]=t.split(".");if(s!==crypto.createHmac("sha256",CONFIG.jwtSecret).update(p).digest("hex"))return null;
const d=JSON.parse(Buffer.from(p,"base64").toString());return d.exp<Date.now()?null:d}catch{return null}}
function parseBody(r){return new Promise(s=>{let b="";r.on("data",c=>b+=c);r.on("end",()=>{try{s(b?JSON.parse(b):{})}catch{s({})}})})}
function sendJSON(r,d,s=200,headers={}){r.writeHead(s,{"Content-Type":"application/json","Access-Control-Allow-Origin":"*","Access-Control-Allow-Methods":"*","Access-Control-Allow-Headers":"*",...headers});r.end(JSON.stringify(d))}
function loadUsers(){try{return fs.existsSync(CONFIG.usersFile)?JSON.parse(fs.readFileSync(CONFIG.usersFile,"utf8")):[]}catch{return[]}}
function saveUsers(u){try{fs.writeFileSync(CONFIG.usersFile,JSON.stringify(u,null,2));updateHysteriaConfig(u.filter(x=>!x.protocol||x.protocol==="hysteria2"));updateXrayConfig(u.filter(x=>x.protocol==="vless-reality"),u.filter(x=>x.protocol==="vless-ws-tls"));return true}catch{return false}}
function updateHysteriaConfig(users){try{let c=fs.readFileSync(CONFIG.hysteriaConfig,"utf8");
const up=users.reduce((a,u)=>{a[u.username]=u.password;return a},{});
const auth="auth:\n  type: userpass\n  userpass:\n"+Object.entries(up).map(([u,p])=>"    "+u+": "+p).join("\n");
c=c.replace(/auth:[\s\S]*?(?=\n[a-zA-Z]|$)/,auth+"\n\n");
fs.writeFileSync(CONFIG.hysteriaConfig,c);execSync("systemctl restart hysteria-server",{stdio:"pipe"})}catch(e){log("ERROR","Hysteria: "+e.message)}}
function updateXrayConfig(realityUsers,wsUsers=[]){try{if(!fs.existsSync(CONFIG.xrayConfig))return;
let c=JSON.parse(fs.readFileSync(CONFIG.xrayConfig,"utf8"));
// Update Reality inbound
const realityClients=realityUsers.map(u=>({id:u.uuid,flow:"xtls-rprx-vision",email:u.username}));
const inbound=c.inbounds.find(i=>i.tag==="vless-reality");
if(inbound){
  inbound.settings.clients=realityClients;
  const userSnis=realityUsers.filter(u=>u.sni).map(u=>u.sni);
  const baseSni=inbound.streamSettings?.realitySettings?.dest?.split(":")[0]||"www.bing.com";
  const allSnis=[...new Set([baseSni,...userSnis])];
  if(inbound.streamSettings?.realitySettings)inbound.streamSettings.realitySettings.serverNames=allSnis;
}
// Update or create WS+TLS inbound
const wsClients=wsUsers.map(u=>({id:u.uuid,email:u.username}));
let wsInbound=c.inbounds.find(i=>i.tag==="vless-ws-tls");
if(wsUsers.length>0){
  if(!wsInbound){
    // Get domain from hysteria config for TLS cert
    const hc=fs.readFileSync(CONFIG.hysteriaConfig,"utf8");
    const dm=hc.match(/\/live\/([^\/]+)\/fullchain/);
    const domain=dm?dm[1]:"localhost";
    wsInbound={tag:"vless-ws-tls",port:10002,protocol:"vless",settings:{clients:wsClients,decryption:"none"},streamSettings:{network:"ws",security:"tls",tlsSettings:{serverName:domain,certificates:[{certificateFile:"/etc/letsencrypt/live/"+domain+"/fullchain.pem",keyFile:"/etc/letsencrypt/live/"+domain+"/privkey.pem"}]},wsSettings:{path:"/ws",headers:{}}}};
    c.inbounds.push(wsInbound);
  }else{
    wsInbound.settings.clients=wsClients;
  }
}
fs.writeFileSync(CONFIG.xrayConfig,JSON.stringify(c,null,2));execSync("systemctl restart xray 2>/dev/null||true",{stdio:"pipe"})}catch(e){log("ERROR","Xray: "+e.message)}}
function getConfig(){try{let dm,pm;
const hc=fs.readFileSync(CONFIG.hysteriaConfig,"utf8");
dm=hc.match(/\/live\/([^\/]+)\/fullchain/);pm=hc.match(/listen:\s*:(\d+)/);
let xrayPort=10001,pubKey="",shortId="",sni="www.bing.com";
try{const xc=JSON.parse(fs.readFileSync(CONFIG.xrayConfig,"utf8"));const xi=xc.inbounds.find(i=>i.tag==="vless-reality");if(xi){xrayPort=xi.port;
const dest=xi.streamSettings?.realitySettings?.dest||"";sni=dest.split(":")[0]||"www.bing.com";shortId=xi.streamSettings?.realitySettings?.shortIds?.[0]||""}}catch{}
try{const k=JSON.parse(fs.readFileSync(CONFIG.xrayKeysFile,"utf8"));pubKey=k.publicKey||"";shortId=shortId||k.shortId||""}catch{}
return{domain:dm?dm[1]:"localhost",port:pm?pm[1]:"443",xrayPort,pubKey,shortId,sni}}catch{return{domain:"localhost",port:"443",xrayPort:10001,pubKey:"",shortId:"",sni:"www.bing.com"}}}
function fetchStats(ep){return new Promise(s=>{const r=http.request({hostname:"127.0.0.1",port:CONFIG.trafficPort,path:ep,method:"GET"},
res=>{let d="";res.on("data",c=>d+=c);res.on("end",()=>{try{s(JSON.parse(d))}catch{s({})}})});
r.on("error",()=>s({}));r.setTimeout(3e3,()=>{r.destroy();s({})});r.end()})}
function postStats(ep,b){return new Promise(s=>{const d=JSON.stringify(b);const r=http.request({hostname:"127.0.0.1",port:CONFIG.trafficPort,path:ep,method:"POST",headers:{"Content-Type":"application/json","Content-Length":Buffer.byteLength(d)}},
res=>s(res.statusCode===200));r.on("error",()=>s(false));r.write(d);r.end()})}

// --- Traffic Tracking ---
function getCurrentMonth(){return new Date().toISOString().slice(0,7)}
function updateUserTraffic(stats){const users=loadUsers();let changed=false;
Object.entries(stats).forEach(([username,{tx,rx}])=>{const u=users.find(x=>x.username===username);if(u){
if(!u.usage)u.usage={total:0,monthly:{}};const m=getCurrentMonth();
u.usage.total=(u.usage.total||0)+tx+rx;u.usage.monthly[m]=(u.usage.monthly[m]||0)+tx+rx;changed=true}});
if(changed){try{fs.writeFileSync(CONFIG.usersFile,JSON.stringify(users,null,2))}catch(e){log("ERROR","Save traffic: "+e.message)}}}
function checkUserLimits(u){const now=Date.now(),m=getCurrentMonth();
if(u.limits?.expiresAt&&new Date(u.limits.expiresAt).getTime()<now)return{ok:false,reason:"expired"};
if(u.limits?.trafficLimit&&(u.usage?.total||0)>=u.limits.trafficLimit)return{ok:false,reason:"traffic_exceeded"};
if(u.limits?.monthlyLimit&&(u.usage?.monthly?.[m]||0)>=u.limits.monthlyLimit)return{ok:false,reason:"monthly_exceeded"};
return{ok:true}}
function handleManage(params,res){
const key=params.get("key"),action=params.get("action"),user=params.get("user");
if(key!==CONFIG.adminPassword)return sendJSON(res,{error:"Invalid key"},403);
if(!action)return sendJSON(res,{error:"Missing action"},400);
const users=loadUsers();
if(action==="create"){
if(!user)return sendJSON(res,{error:"Missing user"},400);
if(users.find(u=>u.username===user))return sendJSON(res,{error:"User exists"},400);
const protocol=params.get("protocol")||"hysteria2";
const pass=params.get("pass")||((protocol==="vless-reality"||protocol==="vless-ws-tls")?crypto.randomUUID():crypto.randomBytes(8).toString("hex"));
const days=parseInt(params.get("days"))||0;const traffic=parseFloat(params.get("traffic"))||0;const monthly=parseFloat(params.get("monthly"))||0;const speed=parseFloat(params.get("speed"))||0;
const sni=params.get("sni")||"www.bing.com"; // User-specific SNI, default to bing
const newUser={username:user,protocol,createdAt:new Date().toISOString(),limits:{},usage:{total:0,monthly:{}}};
if(protocol==="vless-reality"||protocol==="vless-ws-tls"){newUser.uuid=pass;newUser.sni=sni}else{newUser.password=pass}
if(days>0)newUser.limits.expiresAt=new Date(Date.now()+days*864e5).toISOString();
if(traffic>0)newUser.limits.trafficLimit=traffic*1073741824;
if(monthly>0)newUser.limits.monthlyLimit=monthly*1073741824;if(speed>0)newUser.limits.speedLimit=speed*1000000;
users.push(newUser);
if(saveUsers(users))return sendJSON(res,{success:true,user:user,password:pass,sni:sni});
return sendJSON(res,{error:"Save failed"},500)}
if(action==="delete"){
if(!user)return sendJSON(res,{error:"Missing user"},400);
const idx=users.findIndex(u=>u.username===user);if(idx<0)return sendJSON(res,{error:"User not found"},404);
users.splice(idx,1);
if(saveUsers(users))return sendJSON(res,{success:true});
return sendJSON(res,{error:"Save failed"},500)}
if(action==="update"){
if(!user)return sendJSON(res,{error:"Missing user"},400);
const u=users.find(x=>x.username===user);if(!u)return sendJSON(res,{error:"User not found"},404);
const days=params.get("days"),traffic=params.get("traffic"),monthly=params.get("monthly"),pass=params.get("pass");
if(!u.limits)u.limits={};
if(days!==null)u.limits.expiresAt=parseInt(days)>0?new Date(Date.now()+parseInt(days)*864e5).toISOString():null;
if(traffic!==null)u.limits.trafficLimit=parseFloat(traffic)>0?parseFloat(traffic)*1073741824:null;
if(monthly!==null)u.limits.monthlyLimit=parseFloat(monthly)>0?parseFloat(monthly)*1073741824:null;
if(pass)u.password=pass;
if(saveUsers(users))return sendJSON(res,{success:true});
return sendJSON(res,{error:"Save failed"},500)}
if(action==="list")return sendJSON(res,users.map(u=>({username:u.username,limits:u.limits,usage:u.usage})));
return sendJSON(res,{error:"Unknown action"},400)}

// --- Enhanced UI ---
const HTML=`<!DOCTYPE html>
<html lang="zh-CN">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>B-UI 管理面板</title>
    <link href="https://fonts.googleapis.com/css2?family=Inter:wght@400;500;600;700&display=swap" rel="stylesheet">
    <style>
        :root {
            --primary: #B22222;
            --primary-gradient: linear-gradient(135deg, #8B0000, #B22222, #D4AF37);
            --bg: #FBF7F0;
            --card-bg: rgba(255, 252, 245, 0.7);
            --header-bg: rgba(255, 252, 245, 0.8);
            --text: #4A0404;
            --text-dim: #8B4513;
            --border: rgba(139, 0, 0, 0.12);
            --success: #2E8B57;
            --danger: #C41E3A;
            --warning: #ff9f0a;
            --radius: 20px;
        }
        * { margin: 0; padding: 0; box-sizing: border-box; outline: none; -webkit-tap-highlight-color: transparent; }
        body {
            font-family: -apple-system, BlinkMacSystemFont, "SF Pro Text", "Inter", sans-serif;
            background: var(--bg);
            color: var(--text);
            min-height: 100vh;
            overflow-x: hidden;
            background-image:
                radial-gradient(circle at 15% 15%, rgba(255, 107, 107, 0.15) 0%, transparent 45%),
                radial-gradient(circle at 85% 15%, rgba(255, 142, 83, 0.15) 0%, transparent 45%),
                radial-gradient(circle at 85% 85%, rgba(157, 78, 221, 0.12) 0%, transparent 45%),
                radial-gradient(circle at 15% 85%, rgba(255, 107, 107, 0.12) 0%, transparent 45%);
            background-attachment: fixed;
        }
        .view { display: none; padding-top: 90px; padding-bottom: 40px; }
        .view.active { display: block; animation: fadeIn 0.5s cubic-bezier(0.16, 1, 0.3, 1); }
        @keyframes fadeIn { from { opacity: 0; transform: scale(0.98); } to { opacity: 1; transform: scale(1); } }

        /* Navigation - Apple Style Glass */
        .nav {
            position: fixed; top: 0; left: 0; right: 0; height: 60px;
            background: rgba(28, 28, 30, 0.7);
            backdrop-filter: blur(20px) saturate(180%);
            -webkit-backdrop-filter: blur(20px) saturate(180%);
            border-bottom: 1px solid var(--border);
            display: flex; justify-content: space-between; align-items: center;
            padding: 0 24px; z-index: 100;
        }
        .brand { 
            font-size: 19px; font-weight: 600; display: flex; align-items: center; gap: 10px; 
            background: linear-gradient(135deg, #fff, #cecece); -webkit-background-clip: text; -webkit-text-fill-color: transparent;
        }
        .brand i { 
            width: 28px; height: 28px; 
            background: var(--primary-gradient); 
            border-radius: 8px; display: grid; place-items: center; 
            font-style: normal; font-size: 14px; color: white; -webkit-text-fill-color: white; 
            box-shadow: 0 4px 12px rgba(255, 107, 107, 0.3);
        }

        /* Dashboard */
        .container { max-width: 1000px; margin: 0 auto; padding: 0 20px; }
        
        .stats { display: grid; grid-template-columns: repeat(auto-fit, minmax(220px, 1fr)); gap: 16px; margin-bottom: 24px; }
        .stat-card {
            background: var(--card-bg);
            backdrop-filter: blur(25px) saturate(180%);
            -webkit-backdrop-filter: blur(25px) saturate(180%);
            border: 1px solid var(--border); border-radius: var(--radius);
            padding: 20px; transition: transform 0.2s cubic-bezier(0.34, 1.56, 0.64, 1);
        }
        .stat-card:hover { transform: scale(1.02); }
        .stat-val { font-size: 32px; font-weight: 700; margin-top: 8px; letter-spacing: -0.5px; }
        .stat-lbl { font-size: 13px; color: var(--text-dim); font-weight: 500; }

        /* Table Card */
        .table-card {
            background: var(--card-bg);
            backdrop-filter: blur(25px) saturate(180%);
            -webkit-backdrop-filter: blur(25px) saturate(180%);
            border: 1px solid var(--border); border-radius: var(--radius);
            overflow: hidden;
        }
        .table-header {
            padding: 20px 24px; display: flex; justify-content: space-between; align-items: center;
            border-bottom: 1px solid var(--border); background: rgba(255,255,255,0.02);
        }
        .table-header h2 { font-size: 17px; font-weight: 600; }
        
        /* Buttons */
        .btn {
            background: var(--primary-gradient);
            color: white; border: none; padding: 10px 20px; border-radius: 99px;
            font-weight: 600; font-size: 13px; cursor: pointer; transition: .2s;
            box-shadow: 0 4px 12px rgba(255, 107, 107, 0.25);
        }
        .btn:hover { transform: scale(1.02); box-shadow: 0 6px 16px rgba(255, 107, 107, 0.35); }
        
        /* Icon Button */
        .ibtn {
            width: 32px; height: 32px; border-radius: 50%; border: none;
            background: rgba(255,255,255,0.1); color: var(--text);
            cursor: pointer; display: grid; place-items: center; transition: .2s; font-size: 14px;
        }
        .ibtn:hover { background: rgba(255,255,255,0.2); }
        .ibtn.danger { color: var(--danger); background: rgba(255, 69, 58, 0.1); }
        .ibtn.danger:hover { background: rgba(255, 69, 58, 0.2); }
        
        table { width: 100%; border-collapse: collapse; }
        th, td { padding: 18px 24px; text-align: left; border-bottom: 1px solid var(--border); }
        th { color: var(--text-dim); font-size: 12px; font-weight: 500; text-transform: uppercase; letter-spacing: 0.5px; }
        td { font-size: 14px; font-weight: 400; }
        tr:last-child td { border-bottom: none; }
        tr:hover td { background: rgba(255,255,255,0.03); }

        /* Tags */
        .tag { padding: 4px 10px; border-radius: 6px; font-size: 11px; font-weight: 600; background: rgba(255,255,255,0.1); color: var(--text-dim); }
        .tag.on { background: rgba(50, 215, 75, 0.15); color: var(--success); }
        .proto-tag { padding: 2px 6px; border-radius: 4px; font-size: 10px; font-weight: 700; margin-left: 4px; }
        .proto-hy2 { background: rgba(255, 159, 10, 0.2); color: #ff9f0a; }
        .proto-vless { background: rgba(50, 215, 75, 0.2); color: #32d74b; }
        .proto-ws { background: rgba(10, 132, 255, 0.2); color: #0a84ff; }
        
        /* Forms */
        input, select {
            width: 100%; background: rgba(0,0,0,0.2); border: 1px solid var(--border);
            padding: 12px 16px; border-radius: 12px; color: var(--text); margin-bottom: 16px;
            font-size: 15px; transition: .2s;
        }
        input:focus, select:focus { border-color: var(--primary); background: rgba(0,0,0,0.3); }

        /* Modal */
        .modal {
            position: fixed; inset: 0; background: rgba(139,0,0,0.08); backdrop-filter: blur(10px); -webkit-backdrop-filter: blur(10px);
            z-index: 200; display: none; align-items: center; justify-content: center; opacity: 0; transition: .3s;
        }
        .modal.on { display: flex; opacity: 1; }
        .modal-card {
            background: #FFFCF5; border: 1px solid var(--border); border-radius: 24px;
            width: 90%; max-width: 380px; padding: 32px;
            box-shadow: 0 40px 80px -20px rgba(0,0,0,0.6);
            transform: scale(0.95); transition: .3s cubic-bezier(0.16, 1, 0.3, 1);
        }
        .modal.on .modal-card { transform: scale(1); }
        
        /* Login */
        .login-wrap { display: flex; justify-content: center; align-items: center; min-height: 100vh; }
        .login-card { width: 100%; max-width: 360px; text-align: center; border-radius: 28px; padding: 40px 30px; }

        /* Toast */
        .toast-box { position: fixed; top: 20px; left: 50%; transform: translateX(-50%); display: flex; flex-direction: column; gap: 10px; z-index: 300; }
        .toast {
            background: #FFFCF5; backdrop-filter: blur(20px); -webkit-backdrop-filter: blur(20px);
            color: var(--text); border: 1px solid var(--border);
            padding: 12px 24px; border-radius: 99px; display: flex; align-items: center; gap: 10px; font-size: 14px; font-weight: 500;
            box-shadow: 0 10px 30px -10px rgba(0,0,0,0.3); animation: toastIn 0.3s cubic-bezier(0.16, 1, 0.3, 1);
        }
        @keyframes toastIn { from { opacity: 0; transform: translateY(-20px); } to { opacity: 1; transform: translateY(0); } }

        @media (max-width: 768px) {
            .stats { grid-template-columns: 1fr; }
            .hide-m { display: none; }
            .container { padding: 0 16px; }
            td, th { padding: 16px; }
        }
    </style>
</head>
<body>

<!-- Login View -->
<div id="v-login" class="view active" style="padding:0">
    <div class="login-wrap">
        <div class="stat-card login-card">
            <h1 style="font-size:24px; margin-bottom:8px">B-UI 管理面板</h1>
            <p style="color:var(--text-dim); font-size:14px; margin-bottom:30px">安全访问中心 · v${VERSION}</p>
            <input type="password" id="lp" placeholder="Enter Admin Password">
            <button class="btn" style="width:100%" onclick="login()">登 录</button>
        </div>
    </div>
</div>

<!-- Dashboard View -->
<div id="v-dash" class="view">
    <nav class="nav">
        <div class="brand"><i>⚡</i><span>B-UI</span><span style="font-size:11px;color:#888;margin-left:8px">v${VERSION}</span></div>
        <div style="display:flex; gap:10px">
            <button class="ibtn" onclick="openMasq()" title="伪装网站设置">🎭</button>
            <button class="ibtn" onclick="openM('m-pwd')" title="Change Password">🔑</button>
            <button class="ibtn danger" onclick="logout()" title="Logout">✕</button>
        </div>
    </nav>
    
    <div class="container">
        <!-- Stats Grid -->
        <div class="stats">
            <div class="stat-card">
                <div class="stat-lbl">总用户数</div>
                <div class="stat-val" id="st-u">0</div>
            </div>
            <div class="stat-card">
                <div class="stat-lbl">在线设备</div>
                <div class="stat-val" id="st-o" style="color:var(--success)">0</div>
            </div>
            <div class="stat-card">
                <div class="stat-lbl">上传流量</div>
                <div class="stat-val" id="st-up">0</div>
            </div>
            <div class="stat-card">
                <div class="stat-lbl">下载</div>
                <div class="stat-val" id="st-dl">0</div>
            </div>
        </div>

        <!-- User Table -->
        <div class="table-card">
            <div class="table-header">
                <h2>用户管理</h2>
                <button class="btn" onclick="openM('m-add')">+ 新建用户</button>
            </div>
            <div style="overflow-x:auto">
                <table>
                    <thead>
                        <tr>
                            <th>用户名</th>
                            <th>状态</th>
                            <th class="hide-m">本月流量</th>
                            <th class="hide-m">总流量</th>
                            <th>操作</th>
                        </tr>
                    </thead>
                    <tbody id="tb"></tbody>
                </table>
            </div>
        </div>
    </div>
</div>

<!-- Add User Modal -->
<div id="m-add" class="modal">
    <div class="modal-card">
        <h3 style="margin-bottom:20px; font-size:18px">新建用户</h3>
        <select id="nproto" onchange="toggleSniSelect()">
            <option value="hysteria2">Hysteria2 (推荐)</option>
            <option value="vless-reality">VLESS + Reality</option>
            <option value="vless-ws-tls">VLESS + WS + TLS (免流测试)</option>
        </select>
        <div id="sni-group" style="display:none">
            <select id="nsni">
                <optgroup label="默认">
                    <option value="www.bing.com" selected>www.bing.com (默认)</option>
                    <option value="www.microsoft.com">www.microsoft.com</option>
                </optgroup>
                <optgroup label="电信免流">
                    <option value="www.189.cn">电信营业厅 (www.189.cn) ⭐</option>
                    <option value="vod3.nty.tv189.cn">天翼视讯 (vod3.nty.tv189.cn)</option>
                    <option value="cloudgame.189.cn">天翼云游戏 (cloudgame.189.cn)</option>
                    <option value="ltewap.tv189.com">电信爱看 (ltewap.tv189.com)</option>
                    <option value="h5.nty.tv189.com">天翼视讯H5 (h5.nty.tv189.com)</option>
                    <option value="open.4g.play.cn">电信爱玩 (open.4g.play.cn)</option>
                </optgroup>
                <optgroup label="联通免流">
                    <option value="iservice.10010.com">联通营业厅 (iservice.10010.com) ⭐</option>
                    <option value="game.hxll.wostore.cn">沃商店游戏 (game.hxll.wostore.cn)</option>
                    <option value="music.hxll.wostore.cn">沃商店音乐 (music.hxll.wostore.cn)</option>
                    <option value="box.10155.com">沃音乐 (box.10155.com)</option>
                    <option value="partner.iread.wo.com.cn">沃阅读 (partner.iread.wo.com.cn)</option>
                    <option value="wotv.17wo.cn">联通WOTV (wotv.17wo.cn)</option>
                </optgroup>
                <optgroup label="移动免流">
                    <option value="shop.10086.cn">移动商城 (shop.10086.cn) ⭐</option>
                    <option value="mm.10086.cn">移动MM (mm.10086.cn)</option>
                    <option value="www.10086.cn">移动官网 (www.10086.cn)</option>
                    <option value="dm.toutiao.com">抖音/头条 (dm.toutiao.com)</option>
                </optgroup>
                <optgroup label="定向流量">
                    <option value="short.weixin.qq.com">微信 (short.weixin.qq.com)</option>
                    <option value="data.video.qiyi.com">爱奇艺 (data.video.qiyi.com)</option>
                    <option value="api.mobile.youku.com">优酷 (api.mobile.youku.com)</option>
                    <option value="dl.stream.qqmusic.com">QQ音乐 (dl.stream.qqmusic.com)</option>
                </optgroup>
            </select>
            <input id="nsni-custom" placeholder="或输入自定义域名 (留空使用上面选择)" style="margin-top:8px">
        </div>
        <input id="nu" placeholder="用户名">
        <input id="np" placeholder="密码 / UUID (自动生成)">
        <div style="display:grid; grid-template-columns:1fr 1fr; gap:10px">
            <input id="nd" type="number" placeholder="天数" min="0">
            <input id="nt" type="number" placeholder="总流量GB" min="0" step="0.1"></div><div style="display:grid; grid-template-columns:1fr 1fr; gap:10px; margin-top:10px"><input id="nm" type="number" placeholder="月流量GB" min="0" step="0.1"><input id="ns" type="number" placeholder="限mb/s" min="0">
        </div>
        <div style="display:grid; grid-template-columns:1fr 1fr; gap:15px; margin-top:10px">
            <button class="btn" style="background:#F5F0E8;color:#4A0404;box-shadow:none" onclick="closeM()">取消</button>
            <button class="btn" onclick="addUser()">创建</button>
        </div>
    </div>
</div>

<!-- Config Modal -->
<div id="m-cfg" class="modal">
    <div class="modal-card" style="text-align:center">
        <h3 style="margin-bottom:10px">客户端配置</h3>
        <p style="font-size:12px; color:var(--text-dim); margin-bottom:20px">Compatible with v2rayN / Shadowrocket / Clash Meta</p>
        <div id="qrcode" style="margin:20px auto; background:#fff; padding:15px; border-radius:12px; width:fit-content"></div>
        <div class="code-box" id="uri" style="background:#0f172a; padding:15px; border-radius:12px; font-family:monospace; font-size:12px; word-break:break-all; margin-bottom:20px; text-align:left; border:1px solid var(--border); color:var(--text-dim)"></div>
        <div style="display:grid; grid-template-columns:1fr 1fr; gap:15px">
            <button class="btn" onclick="copy()">Copy Link</button>
            <button class="btn" style="background:#F5F0E8;color:#4A0404;box-shadow:none" onclick="closeM()">Close</button>
        </div>
    </div>
</div>

<!-- Password Modal -->
<div id="m-pwd" class="modal">
    <div class="modal-card">
        <h3 style="margin-bottom:20px">修改管理员密码</h3>
        <input type="password" id="newpwd" placeholder="新密码 (最少6位)">
        <div style="display:grid; grid-template-columns:1fr 1fr; gap:15px; margin-top:10px">
            <button class="btn" style="background:#F5F0E8;color:#4A0404;box-shadow:none" onclick="closeM()">取消</button>
            <button class="btn" onclick="changePwd()">Save</button>
        </div>
</div>
</div>

<!-- Masquerade Settings Modal -->
<div id="m-masq" class="modal">
    <div class="modal-card">
        <h3 style="margin-bottom:20px">伪装网站设置</h3>
        <p style="font-size:12px;color:#666;margin-bottom:15px">此设置同时应用于 Hysteria2 和 VLESS-Reality</p>
        <input type="text" id="masqurl" placeholder="https://www.bing.com/">
        <div style="display:grid; grid-template-columns:1fr 1fr; gap:15px; margin-top:10px">
            <button class="btn" style="background:#F5F0E8;color:#4A0404;box-shadow:none" onclick="closeM()">取消</button>
            <button class="btn" onclick="saveMasq()">保存</button>
        </div>
    </div>
</div>

<div class="toast-box" id="t-box"></div>

<script>
const $=s=>document.querySelector(s);let tok=localStorage.getItem("t"),cfg={};
const esc=s=>String(s).replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
const sz=b=>{if(!b)return"0 B";const i=Math.floor(Math.log(b)/Math.log(1024));return(b/Math.pow(1024,i)).toFixed(2)+" "+["B","KB","MB","GB"][i]};

function toast(m,e){const d=document.createElement("div");d.className="toast";d.innerHTML="<span style='font-size:18px'>"+(e?"⚠️":"✅")+"</span><div>"+m+"</div>";$("#t-box").appendChild(d);setTimeout(()=>d.remove(),3000)}
function openM(id){$("#"+id).classList.add("on")}
function closeM(){document.querySelectorAll(".modal").forEach(e=>e.classList.remove("on"))}

function api(ep,opt={}){return fetch("/api"+ep,{...opt,headers:{...opt.headers,Authorization:"Bearer "+tok}}).then(r=>{if(r.status==401)logout();return r.json()})}
function login(){const pw=$("#lp").value;fetch("/api/login",{method:"POST",body:JSON.stringify({password:pw})}).then(r=>r.json()).then(d=>{if(d.token){tok=d.token;localStorage.setItem("t",tok);localStorage.setItem("ap",pw);init()}else toast("Authentication failed",1)})}
function logout(){localStorage.removeItem("t");location.reload()}

function init(){$("#v-login").classList.remove("active");setTimeout(()=>$("#v-login").style.display="none",300);$("#v-dash").classList.add("active");api("/config").then(d=>cfg=d);load();setInterval(load,5000)}

function load(){
    Promise.all([api("/users"),api("/online"),api("/stats")]).then(([u,o,s])=>{
        $("#st-u").innerText=u.length;$("#st-o").innerText=Object.keys(o).length;
        let tu=0,td=0;Object.values(s).forEach(v=>{tu+=v.tx||0;td+=v.rx||0});
        $("#st-up").innerText=sz(tu);$("#st-dl").innerText=sz(td);
        const m=new Date().toISOString().slice(0,7);
        allUsers=u;u.forEach(x=>{const uri=genUri(x);new Image().src="https://api.qrserver.com/v1/create-qr-code/?size=200x200&data="+encodeURIComponent(uri)});
        $("#tb").innerHTML=u.map(x=>{
            const on=o[x.username],monthly=x.usage?.monthly?.[m]||0,total=x.usage?.total||0;
            const exp=x.limits?.expiresAt?new Date(x.limits.expiresAt)<new Date():"",tlim=x.limits?.trafficLimit,over=tlim&&total>=tlim;
            const badge=exp?' <span class="tag" style="color:var(--danger)">EXPIRED</span>':(over?' <span class="tag" style="color:var(--danger)">LIMIT</span>':"");
            const proto=x.protocol||"hysteria2",ptag=proto==="vless-reality"?'<span class="proto-tag proto-vless">VLESS</span>':(proto==="vless-ws-tls"?'<span class="proto-tag proto-ws">WS</span>':'<span class="proto-tag proto-hy2">HY2</span>');
            
            return '<tr>'+
                '<td><div style="display:flex;align-items:center;gap:8px"><span style="font-weight:600">'+esc(x.username)+'</span>'+ptag+badge+'</div></td>'+
                '<td><span class="tag '+(on?'on':'')+' ">'+(on?on+' Online':'Offline')+'</span></td>'+
                '<td class="hide-m" style="font-family:monospace;color:var(--text-dim)">'+sz(monthly)+'</td>'+
                '<td class="hide-m" style="font-family:monospace;color:var(--text-dim)">'+sz(total)+(tlim?' / '+sz(tlim):'')+'</td>'+
                '<td>'+
                    '<div style="display:flex;gap:8px">'+
                        '<button class="ibtn" onclick="showU(&apos;'+esc(x.username)+'&apos;)" title="Config">⚙</button>'+
                        (on?'<button class="ibtn danger" onclick="kick(&apos;'+esc(x.username)+'&apos;)" title="Kick">⚡</button>':'')+
                        '<button class="ibtn danger" onclick="del(&apos;'+esc(x.username)+'&apos;)" title="Delete">🗑</button>'+
                    '</div>'+
                '</td>'+
            '</tr>'
        }).join("")
    })
}

function addUser(){
    const u=$("#nu").value,p=$("#np").value,d=$("#nd").value||0,t=$("#nt").value||0,m=$("#nm").value||0,s=$("#ns").value||0,proto=$("#nproto").value;
    const customSni=$("#nsni-custom")?.value||$("#nsni")?.value||"";
    let url="/api/manage?key="+encodeURIComponent(cfg.adminPass||localStorage.getItem("ap")||"")+"&action=create&user="+encodeURIComponent(u)+(p?"&pass="+encodeURIComponent(p):"")+"&days="+d+"&traffic="+t+"&monthly="+m+"&speed="+s+"&protocol="+proto;
    if(customSni)url+="&sni="+encodeURIComponent(customSni);
    fetch(url).then(r=>r.json()).then(r=>{if(r.success){closeM();toast("User "+u+" created");load()}else toast(r.error||"Failed",1)})
}

function del(u){if(confirm("Delete user "+u+"?"))api("/users/"+encodeURIComponent(u),{method:"DELETE"}).then(()=>load())}
function kick(u){api("/kick",{method:"POST",body:JSON.stringify([u])}).then(()=>toast("User "+u+" kicked offline"))}

let allUsers=[];
function genUri(x){
    if(x.protocol==="vless-reality"){
        const userSni=x.sni||cfg.sni||"www.bing.com";
        return "vless://"+x.uuid+"@"+cfg.domain+":"+cfg.xrayPort+"?encryption=none&flow=xtls-rprx-vision&security=reality&sni="+userSni+"&fp=chrome&pbk="+cfg.pubKey+"&sid="+cfg.shortId+"&spx=%2F&type=tcp#"+encodeURIComponent(x.username+(x.sni?" ["+x.sni+"]":""));
    }
    if(x.protocol==="vless-ws-tls"){
        const hostSni=x.sni||"www.bing.com";
        return "vless://"+x.uuid+"@"+cfg.domain+":"+(cfg.wsPort||10002)+"?encryption=none&security=tls&sni="+cfg.domain+"&type=ws&host="+hostSni+"&path=%2Fws#"+encodeURIComponent(x.username+" [WS:"+hostSni+"]");
    }
    return "hysteria2://"+encodeURIComponent(x.username)+":"+encodeURIComponent(x.password)+"@"+cfg.domain+":"+cfg.port+"/?sni="+cfg.domain+"&insecure=0#"+encodeURIComponent(x.username)
}

function showU(uname){
    const x=allUsers.find(u=>u.username===uname);if(!x)return;
    const uri=genUri(x);
    $("#uri").innerText=uri;
    $("#qrcode").innerHTML='<img src="https://api.qrserver.com/v1/create-qr-code/?size=200x200&data='+encodeURIComponent(uri)+'" alt="QR Code" style="display:block;border-radius:8px">';
    openM("m-cfg")
}

function copy(){navigator.clipboard.writeText($("#uri").innerText);toast("Copied to clipboard")}
function changePwd(){
    const np=$("#newpwd").value;if(np.length<6)return toast("Password min 6 chars",1);
    api("/password",{method:"POST",body:JSON.stringify({newPassword:np})}).then(r=>{if(r.success){closeM();toast("Password updated, please login again");setTimeout(()=>logout(),2000)}else toast(r.error||"Failed",1)})
}

function openMasq(){
    api("/masquerade").then(r=>{$("#masqurl").value=r.masqueradeUrl||"https://www.bing.com/";openM("m-masq")})
}
function saveMasq(){
    const url=$("#masqurl").value;if(!url)return toast("请输入URL",1);
    api("/masquerade",{method:"POST",body:JSON.stringify({url})}).then(r=>{if(r.success){closeM();toast("伪装网站已更新: "+r.domain);setTimeout(()=>location.reload(),2000)}else toast(r.error||"Failed",1)})
}

function toggleSniSelect(){
    const proto=$("#nproto").value;
    const sniGroup=$("#sni-group");
    if(proto==="vless-reality"||proto==="vless-ws-tls"){sniGroup.style.display="block"}else{sniGroup.style.display="none"}
}

if(tok)init();
</script>
</body>
</html>`;

// --- Traffic Sync Loop ---
let lastTraffic = {};
setInterval(async () => {
    try {
        const stats = await fetchStats("/traffic"); // { user: { tx: 123, rx: 456 } }
        // TODO: Merge Xray stats here if needed
        
        let users = loadUsers();
        let changed = false;
        const now = new Date();
        const m = now.toISOString().slice(0, 7);

        for (const [uName, stat] of Object.entries(stats)) {
            const u = users.find(x => x.username === uName);
            if (!u) continue;

            if (!u.usage) u.usage = { total: 0, monthly: {} };
            if (!u.usage.monthly) u.usage.monthly = {};

            // Calculate delta
            const last = lastTraffic[uName] || { tx: 0, rx: 0 };
            // If current stat is less than last, service restarted -> delta is current
            const deltaTx = (stat.tx < last.tx) ? stat.tx : (stat.tx - last.tx);
            const deltaRx = (stat.rx < last.rx) ? stat.rx : (stat.rx - last.rx);

            if (deltaTx > 0 || deltaRx > 0) {
                const totalDelta = deltaTx + deltaRx;
                u.usage.total = (u.usage.total || 0) + totalDelta;
                u.usage.monthly[m] = (u.usage.monthly[m] || 0) + totalDelta;
                changed = true;
            }
            
            // Check limits
            if (u.limits && u.limits.trafficLimit && u.usage.total >= u.limits.trafficLimit) {
                 // Logic to kick/disable user could go here
            }

            lastTraffic[uName] = stat;
        }

        if (changed) { try { fs.writeFileSync(CONFIG.usersFile, JSON.stringify(users, null, 2)); } catch (e) { log("ERROR", "Save usage: " + e.message); } }
    } catch (e) {
        console.error("Traffic sync failed:", e);
    }
}, 10000); // Sync every 10s

// --- Server Startup ---
const server = http.createServer(async(req,res)=>{
    const u=new URL(req.url,`http://${req.headers.host}`),p=u.pathname;
    if(req.method==="OPTIONS"){res.writeHead(200,{"Access-Control-Allow-Origin":"*","Access-Control-Allow-Methods":"*","Access-Control-Allow-Headers":"*"});return res.end()}
    if(p==="/"||p==="/index.html"){res.writeHead(200,{"Content-Type":"text/html; charset=utf-8"});return res.end(HTML)}
    
    // ... (rest of the server logic)

if(p.startsWith("/api/")){const r=p.slice(5);const clientIP=getClientIP(req);
try{
if(r==="login"&&req.method==="POST"){
const b=await parseBody(req);
if(!checkRateLimit(clientIP)){recordAttempt(clientIP,false);return sendJSON(res,{error:"Too many attempts. Try again later."},429)}
const ok=b.password===CONFIG.adminPassword;recordAttempt(clientIP,ok);
if(ok)return sendJSON(res,{token:genToken({admin:true})});else return sendJSON(res,{error:"Auth failed"},401)}
if(r==="manage")return handleManage(u.searchParams,res);
const auth=verifyToken((req.headers.authorization||"").replace("Bearer ",""));if(!auth)return sendJSON(res,{error:"Unauthorized"},401);
if(r==="users"){if(req.method==="GET")return sendJSON(res,loadUsers());
if(req.method==="POST"){const b=await parseBody(req),users=loadUsers();if(users.find(u=>u.username===b.username))return sendJSON(res,{error:"Exists"},400);users.push({username:b.username,password:b.password||crypto.randomBytes(8).toString("hex"),createdAt:new Date()});return saveUsers(users)?sendJSON(res,{success:true}):sendJSON(res,{error:"Save failed"},500)}}
if(r.startsWith("users/")&&req.method==="DELETE"){let users=loadUsers();users=users.filter(u=>u.username!==decodeURIComponent(r.slice(6)));return saveUsers(users)?sendJSON(res,{success:true}):sendJSON(res,{error:"Fail"},500)}
if(r==="stats")return sendJSON(res,await fetchStats("/traffic"));
if(r==="online")return sendJSON(res,await fetchStats("/online"));
if(r==="kick"&&req.method==="POST")return sendJSON(res,await postStats("/kick",await parseBody(req)));
if(r==="config")return sendJSON(res,getConfig());
if(r==="masquerade"){
  const masqFile=CONFIG.hysteriaConfig.replace("config.yaml","masquerade.json");
  if(req.method==="GET"){try{const m=JSON.parse(fs.readFileSync(masqFile,"utf8"));return sendJSON(res,m)}catch{return sendJSON(res,{masqueradeUrl:"https://www.bing.com/",masqueradeDomain:"www.bing.com"})}}
  if(req.method==="POST"){const b=await parseBody(req);if(!b.url)return sendJSON(res,{error:"URL required"},400);
    const domain=b.url.replace(/https?:\/\/([^/:]+).*/,"$1")||"www.bing.com";
    try{fs.writeFileSync(masqFile,JSON.stringify({masqueradeUrl:b.url,masqueradeDomain:domain},null,2));
    // Update Hysteria config
    let hyc=fs.readFileSync(CONFIG.hysteriaConfig,"utf8");
    hyc=hyc.replace(/masquerade:[\s\S]*?(?=\n[a-zA-Z]|$)/,`masquerade:\n  type: proxy\n  proxy:\n    url: ${b.url}\n    rewriteHost: true`);
    fs.writeFileSync(CONFIG.hysteriaConfig,hyc);
    // Update Xray config
    if(fs.existsSync(CONFIG.xrayConfig)){let xc=JSON.parse(fs.readFileSync(CONFIG.xrayConfig,"utf8"));
    const xi=xc.inbounds.find(i=>i.tag==="vless-reality");
    if(xi&&xi.streamSettings?.realitySettings){xi.streamSettings.realitySettings.dest=domain+":443";xi.streamSettings.realitySettings.serverNames=[domain]}
    fs.writeFileSync(CONFIG.xrayConfig,JSON.stringify(xc,null,2));execSync("systemctl restart xray 2>/dev/null||true",{stdio:"pipe"})}
    execSync("systemctl restart hysteria-server 2>/dev/null||true",{stdio:"pipe"});
    return sendJSON(res,{success:true,domain})}catch(e){return sendJSON(res,{error:e.message},500)}}}
if(r==="password"&&req.method==="POST"){const b=await parseBody(req);
if(!b.newPassword||b.newPassword.length<6)return sendJSON(res,{error:"密码至少6位"},400);
try{const svc="/etc/systemd/system/b-ui-admin.service";let c=require("fs").readFileSync(svc,"utf8");
c=c.replace(/ADMIN_PASSWORD=[^\n]*/,"ADMIN_PASSWORD="+b.newPassword);
require("fs").writeFileSync(svc,c);require("child_process").execSync("systemctl daemon-reload");
return sendJSON(res,{success:true,message:"密码已更新，请重新登录"})}
catch(e){return sendJSON(res,{error:e.message},500)}}
}catch(e){return sendJSON(res,{error:e.message},500)}}
// Hysteria2 HTTP Auth Endpoint
if(p==="/auth/hysteria" && req.method==="POST"){
  const body = await parseBody(req);
  const authStr = body.auth || "";
  // auth format: username:password
  const [username, password] = authStr.split(":");
  const users = loadUsers();
  const user = users.find(u => u.username === username && u.password === password);
  if(user){
    // Check limits
    const check = checkUserLimits(user);
    if(check.ok){
      return sendJSON(res, {ok: true, id: username});
    } else {
      return sendJSON(res, {ok: false, id: username});
    }
  }
  return sendJSON(res, {ok: false});
}
sendJSON(res,{error:"Not found"},404)}).listen(CONFIG.port,()=>console.log("Admin Panel Running"));

SERVEREOF


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

create_hui_cli() {
    print_info "创建 b-ui 命令行工具..."
    
    cat > /usr/local/bin/b-ui << 'HUIEOF'
#!/bin/bash
# B-UI 终端管理面板
# Hysteria2 + Web 管理面板 完整版

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BLUE='\033[0;34m'
NC='\033[0m'

CONFIG_FILE="/opt/hysteria/config.yaml"
USERS_FILE="/opt/hysteria/users.json"
HYSTERIA_SERVICE="hysteria-server.service"
ADMIN_SERVICE="b-ui-admin.service"

print_info() { echo -e "${BLUE}[INFO]${NC} $1"; }
print_success() { echo -e "${GREEN}[SUCCESS]${NC} $1"; }
print_error() { echo -e "${RED}[ERROR]${NC} $1"; }

get_domain() {
    grep -A2 "^tls:" "$CONFIG_FILE" 2>/dev/null | grep "cert:" | sed 's|.*/live/\([^/]*\)/.*|\1|' || echo "未配置"
}

get_port() {
    grep "^listen:" "$CONFIG_FILE" 2>/dev/null | sed 's/listen: *:\?//' || echo "10000"
}

get_admin_password() {
    grep "ADMIN_PASSWORD=" /etc/systemd/system/b-ui-admin.service 2>/dev/null | cut -d= -f3 || echo "未找到"
}

show_banner() {
    clear
    echo -e "${CYAN}╔══════════════════════════════════════════════════════════════╗${NC}"
    echo -e "${CYAN}║                      ${YELLOW}B-UI 管理面板${CYAN}                          ║${NC}"
    echo -e "${CYAN}╚══════════════════════════════════════════════════════════════╝${NC}"
    echo ""
}

show_status() {
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo -e "${YELLOW}[系统状态]${NC}"
    
    if command -v hysteria &> /dev/null; then
        local hy_ver=$(hysteria version 2>/dev/null | grep "^Version:" | awk '{print $2}' || echo '未知')
        echo -e "  Hysteria2: ${YELLOW}${hy_ver}${NC}"
    else
        echo -e "  Hysteria2: ${RED}未安装${NC}"
    fi
    
    if command -v xray &> /dev/null; then
        local xray_ver=$(xray version 2>/dev/null | head -n1 | awk '{print $2}' || echo '未知')
        echo -e "  Xray: ${YELLOW}${xray_ver}${NC}"
    else
        echo -e "  Xray: ${RED}未安装${NC}"
    fi
    
    if systemctl is-active --quiet hysteria-server 2>/dev/null; then
        echo -e "  Hysteria 服务: ${GREEN}✓ 运行中${NC}"
    else
        echo -e "  Hysteria 服务: ${RED}✗ 未运行${NC}"
    fi
    
    if systemctl is-active --quiet b-ui-admin 2>/dev/null; then
        echo -e "  管理面板服务: ${GREEN}✓ 运行中${NC}"
    else
        echo -e "  管理面板服务: ${RED}✗ 未运行${NC}"
    fi
    
    if command -v xray &> /dev/null; then
        if systemctl is-active --quiet xray 2>/dev/null; then
            echo -e "  Xray 服务: ${GREEN}✓ 运行中${NC}"
        else
            echo -e "  Xray 服务: ${RED}✗ 未运行${NC}"
        fi
    fi
    
    local bbr=$(sysctl -n net.ipv4.tcp_congestion_control 2>/dev/null)
    if [[ "$bbr" == "bbr" ]]; then
        echo -e "  BBR: ${GREEN}已启用${NC}"
    else
        echo -e "  BBR: ${YELLOW}未启用${NC}"
    fi
    
    # 显示开机自启动状态
    echo ""
    echo -e "${YELLOW}[开机自启动]${NC}"
    local hy_auto=$(systemctl is-enabled hysteria-server 2>/dev/null); hy_auto=${hy_auto:-未配置}
    local xray_auto=$(systemctl is-enabled xray 2>/dev/null); xray_auto=${xray_auto:-未配置}
    local admin_auto=$(systemctl is-enabled b-ui-admin 2>/dev/null); admin_auto=${admin_auto:-未配置}
    if [[ "$hy_auto" == "enabled" ]]; then
        echo -e "  Hysteria: ${GREEN}✓ 已启用${NC}"
    else
        echo -e "  Hysteria: ${RED}✗ 未启用${NC}"
    fi
    if [[ "$xray_auto" == "enabled" ]]; then
        echo -e "  Xray: ${GREEN}✓ 已启用${NC}"
    else
        echo -e "  Xray: ${RED}✗ 未启用${NC}"
    fi
    if [[ "$admin_auto" == "enabled" ]]; then
        echo -e "  管理面板: ${GREEN}✓ 已启用${NC}"
    else
        echo -e "  管理面板: ${RED}✗ 未启用${NC}"
    fi
    
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo ""
    
    local domain=$(get_domain)
    local port=$(get_port)
    local admin_pass=$(get_admin_password)
    
    echo -e "${YELLOW}[配置信息]${NC}"
    echo -e "  绑定域名: ${GREEN}${domain}${NC}"
    echo -e "  Hysteria 端口: ${GREEN}${port}${NC}"
    echo -e "  管理面板: ${GREEN}https://${domain}${NC}"
    echo -e "  管理密码: ${GREEN}${admin_pass}${NC}"
    echo ""
}

show_menu() {
    echo -e "${CYAN}╔══════════════════════════════════════════════════════════════╗${NC}"
    echo -e "${CYAN}║${NC}                      ${GREEN}B-UI 操作菜单${NC}                          ${CYAN}║${NC}"
    echo -e "${CYAN}╠══════════════════════════════════════════════════════════════╣${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}1.${NC} 查看 API 文档                                          ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}2.${NC} 重启服务                                               ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}3.${NC} 查看日志                                               ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}4.${NC} 修改管理密码                                           ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}5.${NC} 开启 BBR                                               ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}6.${NC} 开机自启动开关                                         ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}7.${NC} ${GREEN}更新内核 (Hysteria2 + Xray)${NC}                            ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}8.${NC} ${RED}完全卸载${NC}                                               ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}0.${NC} 退出                                                   ${CYAN}║${NC}"
    echo -e "${CYAN}╚══════════════════════════════════════════════════════════════╝${NC}"
}

show_api_docs() {
    local domain=$(get_domain)
    local admin_pass=$(get_admin_password)
    
    echo ""
    echo -e "${YELLOW}[URL 管理 API]${NC}"
    echo -e "  基础 URL: ${GREEN}https://${domain}/api/manage${NC}"
    echo ""
    echo -e "  ${CYAN}┌─ action 参数 ────────────────────────────────────────────┐${NC}"
    echo -e "  ${CYAN}│${NC}  create  - 创建新用户                                    ${CYAN}│${NC}"
    echo -e "  ${CYAN}│${NC}  delete  - 删除用户                                      ${CYAN}│${NC}"
    echo -e "  ${CYAN}│${NC}  update  - 修改用户配置                                  ${CYAN}│${NC}"
    echo -e "  ${CYAN}│${NC}  list    - 列出所有用户                                  ${CYAN}│${NC}"
    echo -e "  ${CYAN}└──────────────────────────────────────────────────────────┘${NC}"
    echo ""
    echo -e "  ${CYAN}┌─ 参数说明 ───────────────────────────────────────────────┐${NC}"
    echo -e "  ${CYAN}│${NC}  key     - 管理密码 (必填)                               ${CYAN}│${NC}"
    echo -e "  ${CYAN}│${NC}  user    - 用户名 (必填)                                 ${CYAN}│${NC}"
    echo -e "  ${CYAN}│${NC}  pass    - 密码 (可选)                                   ${CYAN}│${NC}"
    echo -e "  ${CYAN}│${NC}  days    - 有效天数 (0=永久)                             ${CYAN}│${NC}"
    echo -e "  ${CYAN}│${NC}  traffic - 总流量 GB (0=不限)                            ${CYAN}│${NC}"
    echo -e "  ${CYAN}└──────────────────────────────────────────────────────────┘${NC}"
    echo ""
    echo -e "  ${YELLOW}示例:${NC}"
    echo -e "  ${GREEN}创建:${NC} https://${domain}/api/manage?key=${admin_pass}&action=create&user=test&days=30&traffic=10"
    echo -e "  ${GREEN}删除:${NC} https://${domain}/api/manage?key=${admin_pass}&action=delete&user=test"
    echo ""
}

change_password() {
    echo ""
    read -p "请输入新密码 (至少6位): " new_pass
    if [[ ${#new_pass} -lt 6 ]]; then
        print_error "密码至少6位"
        return 1
    fi
    
    local svc="/etc/systemd/system/b-ui-admin.service"
    if [[ ! -f "$svc" ]]; then
        print_error "服务配置文件不存在"
        return 1
    fi
    
    sed -i "s/ADMIN_PASSWORD=[^ ]*/ADMIN_PASSWORD=${new_pass}/" "$svc"
    systemctl daemon-reload
    systemctl restart b-ui-admin
    
    print_success "密码已更新为: ${new_pass}"
}

enable_bbr() {
    local cc=$(sysctl -n net.ipv4.tcp_congestion_control 2>/dev/null)
    if [[ "$cc" == "bbr" ]]; then
        print_success "BBR 已启用"
        return 0
    fi
    
    modprobe tcp_bbr 2>/dev/null || true
    echo "net.core.default_qdisc=fq" >> /etc/sysctl.conf
    echo "net.ipv4.tcp_congestion_control=bbr" >> /etc/sysctl.conf
    sysctl -p > /dev/null 2>&1
    print_success "BBR 启用成功"
}

update_hysteria() {
    print_info "正在更新内核..."
    echo ""
    
    # 更新 Hysteria2
    print_info "更新 Hysteria2..."
    local old_hy=$(hysteria version 2>/dev/null | grep "^Version:" | awk '{print $2}' || echo "未知")
    bash <(curl -fsSL https://get.hy2.sh/)
    local new_hy=$(hysteria version 2>/dev/null | grep "^Version:" | awk '{print $2}' || echo "未知")
    echo -e "  Hysteria2: ${YELLOW}${old_hy}${NC} -> ${GREEN}${new_hy}${NC}"
    systemctl restart hysteria-server 2>/dev/null || true
    
    # 更新 Xray
    if command -v xray &> /dev/null; then
        print_info "更新 Xray..."
        local old_xray=$(xray version 2>/dev/null | head -n1 | awk '{print $2}' || echo "未知")
        bash -c "$(curl -L https://github.com/XTLS/Xray-install/raw/main/install-release.sh)" @ install
        local new_xray=$(xray version 2>/dev/null | head -n1 | awk '{print $2}' || echo "未知")
        echo -e "  Xray: ${YELLOW}${old_xray}${NC} -> ${GREEN}${new_xray}${NC}"
        systemctl restart xray 2>/dev/null || true
    fi
    
    print_success "内核更新完成！"
}

uninstall_all() {
    echo ""
    echo -e "${RED}警告：此操作将完全卸载 B-UI 和 Hysteria2${NC}"
    read -p "确定要继续吗? (输入 YES 确认): " confirm
    
    if [[ "$confirm" != "YES" ]]; then
        print_info "已取消"
        return
    fi
    
    print_info "正在卸载..."
    systemctl stop hysteria-server 2>/dev/null || true
    systemctl stop b-ui-admin 2>/dev/null || true
    systemctl stop xray 2>/dev/null || true
    systemctl disable hysteria-server 2>/dev/null || true
    systemctl disable b-ui-admin 2>/dev/null || true
    systemctl disable xray 2>/dev/null || true
    rm -f /etc/systemd/system/hysteria-server.service
    rm -f /etc/systemd/system/b-ui-admin.service
    rm -rf /etc/systemd/system/hysteria-server.service.d
    rm -rf /etc/systemd/system/xray.service.d
    rm -f /usr/local/bin/hysteria
    rm -f /usr/local/bin/xray
    rm -rf /usr/local/share/xray
    rm -rf /opt/hysteria
    rm -f /usr/local/bin/b-ui
    rm -f /etc/nginx/conf.d/b-ui-admin.conf
    systemctl daemon-reload
    apt-get purge -y nginx nginx-common nodejs certbot 2>/dev/null || true
    apt-get autoremove -y 2>/dev/null || true
    
    print_success "卸载完成！"
    exit 0
}

main() {
    if [[ $EUID -ne 0 ]]; then
        print_error "请使用 sudo b-ui 运行"
        exit 1
    fi
    
    while true; do
        show_banner
        show_status
        show_menu
        
        read -p "请选择 [0-8]: " choice
        
        case $choice in
            1) show_api_docs ;;
            2) 
                systemctl restart hysteria-server 2>/dev/null || true
                systemctl restart b-ui-admin 2>/dev/null || true
                systemctl restart xray 2>/dev/null || true
                print_success "所有服务已重启"
                ;;
            3) 
                echo ""
                echo -e "${YELLOW}选择日志类型:${NC}"
                echo "  1. Hysteria2"
                echo "  2. Xray"
                echo "  3. 管理面板"
                read -p "请选择 [1-3]: " log_choice
                case $log_choice in
                    1) journalctl -u hysteria-server --no-pager -n 30 ;;
                    2) journalctl -u xray --no-pager -n 30 ;;
                    3) journalctl -u b-ui-admin --no-pager -n 30 ;;
                    *) print_error "无效选项" ;;
                esac
                ;;
            4) change_password ;;
            5) enable_bbr ;;
            6)
                echo ""
                local hy_auto=$(systemctl is-enabled hysteria-server 2>/dev/null); hy_auto=${hy_auto:-disabled}
                if [[ "$hy_auto" == "enabled" ]]; then
                    systemctl disable hysteria-server 2>/dev/null || true
                    systemctl disable xray 2>/dev/null || true
                    systemctl disable b-ui-admin 2>/dev/null || true
                    print_success "已关闭开机自启动"
                else
                    systemctl enable hysteria-server 2>/dev/null || true
                    systemctl enable xray 2>/dev/null || true
                    systemctl enable b-ui-admin 2>/dev/null || true
                    print_success "已开启开机自启动"
                fi
                ;;
            7)
                print_info "正在更新内核..."
                echo ""
                print_info "更新 Hysteria2..."
                local old_hy=$(hysteria version 2>/dev/null | grep "^Version:" | awk '{print $2}' || echo "未知")
                bash <(curl -fsSL https://get.hy2.sh/)
                local new_hy=$(hysteria version 2>/dev/null | grep "^Version:" | awk '{print $2}' || echo "未知")
                echo -e "  Hysteria2: ${YELLOW}${old_hy}${NC} -> ${GREEN}${new_hy}${NC}"
                
                print_info "更新 Xray..."
                local old_xray=$(xray version 2>/dev/null | head -n1 | awk '{print $2}' || echo "未知")
                bash -c "$(curl -L https://github.com/XTLS/Xray-install/raw/main/install-release.sh)" @ install
                local new_xray=$(xray version 2>/dev/null | head -n1 | awk '{print $2}' || echo "未知")
                echo -e "  Xray: ${YELLOW}${old_xray}${NC} -> ${GREEN}${new_xray}${NC}"
                
                systemctl restart hysteria-server 2>/dev/null || true
                systemctl restart xray 2>/dev/null || true
                print_success "内核更新完成！"
                ;;
            8) uninstall_all ;;
            0) echo ""; print_info "再见！"; exit 0 ;;
            *) print_error "无效选项" ;;
        esac
        
        echo ""
        read -p "按 Enter 继续..."
    done
}

main
HUIEOF
    
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
            if ! crontab -l 2>/dev/null | grep -q "certbot renew"; then
                (crontab -l 2>/dev/null; echo "0 3 * * * certbot renew --quiet") | crontab -
                print_info "已设置证书自动续期 (每天 3:00)"
            fi
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
        if ! crontab -l 2>/dev/null | grep -q "certbot renew"; then
            (crontab -l 2>/dev/null; echo "0 3 * * * certbot renew --quiet") | crontab -
            print_info "已设置证书自动续期 (每天 3:00)"
        fi
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

uninstall_all() {
    echo ""
    echo -e "${RED}════════════════════════════════════════════════════════════════${NC}"
    echo -e "${RED}  警告：此操作将完全卸载 Hysteria2 和 B-UI 管理面板${NC}"
    echo -e "${RED}════════════════════════════════════════════════════════════════${NC}"
    echo ""
    echo -e "将删除以下内容："
    echo -e "  - Hysteria2 服务和二进制文件"
    echo -e "  - B-UI 管理面板服务和文件"
    echo -e "  - 所有用户配置和流量数据"
    echo -e "  - Nginx 代理配置"
    echo -e "  - SSL 证书 (可选)"
    echo -e "  - b-ui 命令行工具"
    echo ""
    read -p "确定要继续吗? (输入 YES 确认): " confirm
    
    if [[ "$confirm" != "YES" ]]; then
        print_info "已取消卸载"
        return
    fi
    
    print_info "开始卸载..."
    
    # 停止并禁用服务
    print_info "停止服务..."
    systemctl stop hysteria-server 2>/dev/null || true
    systemctl stop b-ui-admin 2>/dev/null || true
    systemctl stop xray 2>/dev/null || true
    systemctl disable hysteria-server 2>/dev/null || true
    systemctl disable b-ui-admin 2>/dev/null || true
    systemctl disable xray 2>/dev/null || true
    
    # 删除 systemd 服务文件
    print_info "删除服务配置..."
    rm -f /etc/systemd/system/hysteria-server.service
    rm -f /etc/systemd/system/b-ui-admin.service
    rm -rf /etc/systemd/system/hysteria-server.service.d
    rm -rf /etc/systemd/system/xray.service.d
    systemctl daemon-reload
    
    # 删除 Hysteria 和 Xray 二进制文件
    print_info "删除程序文件..."
    rm -f /usr/local/bin/hysteria
    rm -f /usr/local/bin/xray
    rm -rf /usr/local/share/xray
    
    # 删除配置和数据目录
    print_info "删除配置和数据..."
    rm -rf /opt/hysteria
    rm -rf /etc/hysteria
    
    # 删除 b-ui 命令
    print_info "删除 b-ui 命令..."
    rm -f /usr/local/bin/b-ui
    
    # 删除 Nginx 配置
    print_info "删除 Nginx 配置..."
    rm -f /etc/nginx/sites-enabled/b-ui-admin
    rm -f /etc/nginx/sites-available/b-ui-admin
    rm -f /etc/nginx/conf.d/b-ui-admin.conf
    systemctl reload nginx 2>/dev/null || true
    
    # 删除 certbot 自动续期 cron
    print_info "清理定时任务..."
    crontab -l 2>/dev/null | grep -v "certbot renew" | crontab - 2>/dev/null || true
    
    # 删除 SSL 证书
    print_info "删除 SSL 证书..."
    local domain=$(ls /etc/letsencrypt/live/ 2>/dev/null | head -1)
    if [[ -n "$domain" ]]; then
        certbot delete --cert-name "$domain" --non-interactive 2>/dev/null || true
    fi
    rm -rf /etc/letsencrypt/live/*
    rm -rf /etc/letsencrypt/archive/*
    rm -rf /etc/letsencrypt/renewal/*
    
    # 卸载相关软件包
    print_info "卸载相关软件包..."
    apt-get purge -y nginx nginx-common nginx-core 2>/dev/null || true
    apt-get purge -y nodejs npm 2>/dev/null || true
    apt-get purge -y certbot python3-certbot-nginx 2>/dev/null || true
    apt-get autoremove -y 2>/dev/null || true
    
    # 清理残留配置
    rm -rf /etc/nginx
    rm -rf /var/log/nginx
    rm -rf /var/www/html
    
    echo ""
    echo -e "${GREEN}════════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}  完全卸载完成！${NC}"
    echo -e "${GREEN}════════════════════════════════════════════════════════════════${NC}"
    echo ""
    echo -e "已删除: Hysteria2, B-UI, Nginx, Node.js, Certbot, SSL 证书"
    echo ""
}

show_status() {
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
    
    if check_bbr_status; then
        echo -e "  BBR: ${GREEN}已启用${NC}"
    else
        echo -e "  BBR: ${YELLOW}未启用${NC}"
    fi
    
    # 显示开机自启动状态
    echo ""
    echo -e "${YELLOW}[开机自启动]${NC}"
    local hy_enabled=$(systemctl is-enabled "$HYSTERIA_SERVICE" 2>/dev/null); hy_enabled=${hy_enabled:-未配置}
    local xray_enabled=$(systemctl is-enabled xray 2>/dev/null); xray_enabled=${xray_enabled:-未配置}
    local admin_enabled=$(systemctl is-enabled "$ADMIN_SERVICE" 2>/dev/null); admin_enabled=${admin_enabled:-未配置}
    echo -e "  Hysteria2: ${CYAN}${hy_enabled}${NC}"
    echo -e "  Xray:      ${CYAN}${xray_enabled}${NC}"
    echo -e "  管理面板:  ${CYAN}${admin_enabled}${NC}"
    
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    
    # 显示网页看板信息
    if [[ -f "$CONFIG_FILE" ]]; then
        local domain=$(grep -A2 "^tls:" "$CONFIG_FILE" 2>/dev/null | grep "cert:" | sed 's|.*/live/\([^/]*\)/.*|\1|')
        local admin_pass=$(grep "ADMIN_PASSWORD=" /etc/systemd/system/b-ui-admin.service 2>/dev/null | cut -d= -f3)
        if [[ -n "$domain" ]]; then
            echo ""
            echo -e "${YELLOW}[网页管理面板]${NC}"
            echo -e "  访问地址: ${GREEN}https://${domain}${NC}"
            echo -e "  管理密码: ${GREEN}${admin_pass:-未设置}${NC}"
        fi
    fi
}

show_client_config() {
    if [[ ! -f "$USERS_FILE" ]]; then
        print_error "未找到用户配置"
        return
    fi
    
    local domain=$(grep -A1 "domains:" "$CONFIG_FILE" 2>/dev/null | tail -1 | sed 's/.*- //' | tr -d ' ')
    local port=$(grep "listen:" "$CONFIG_FILE" 2>/dev/null | head -1 | sed 's/.*://' | tr -d ' ')
    port=${port:-443}
    
    echo ""
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}客户端配置${NC}"
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    
    # 显示客户端一键安装命令
    local install_key=""
    if [[ -f "${BASE_DIR}/install-key.txt" ]]; then
        install_key=$(cat "${BASE_DIR}/install-key.txt" 2>/dev/null)
    else
        install_key=$(tr -dc 'a-f0-9' < /dev/urandom | head -c 32)
        echo "$install_key" > "${BASE_DIR}/install-key.txt"
    fi
    
    echo ""
    echo -e "${YELLOW}【客户端一键安装命令】${NC} (直接从服务端下载，国内可用)"
    echo -e "${GREEN}bash <(curl -fsSL -k https://${domain}/install-client?key=${install_key})${NC}"
    echo ""
    echo -e "${CYAN}───────────────────────────────────────────────────────────────${NC}"
    
    # 解析用户列表
    local users=$(cat "$USERS_FILE" 2>/dev/null)
    echo "$users" | grep -oP '"username":"[^"]*"' | while read line; do
        local uname=$(echo "$line" | cut -d'"' -f4)
        local upass=$(echo "$users" | grep -oP "\"username\":\"$uname\",\"password\":\"[^\"]*\"" | grep -oP 'password":"[^"]*' | cut -d'"' -f3)
        echo -e "  用户: ${YELLOW}$uname${NC}"
        echo -e "  URI:  ${GREEN}hysteria2://${upass}@${domain}:${port}/?insecure=0#${uname}${NC}"
        echo ""
    done
    
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
}

#===============================================================================
# 一键安装
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
    create_hui_cli
    
    # 10. 预下载客户端安装包 (可选，用于国内客户端)
    echo ""
    read -p "是否预下载客户端安装包 (便于国内客户端安装)? (y/n): " download_packages
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
    show_client_config
    
    # 自动打开 b-ui 终端面板
    echo ""
    echo -e "${CYAN}正在打开 B-UI 终端管理面板...${NC}"
    sleep 2
    b-ui
}

#===============================================================================
# 主菜单
#===============================================================================

show_menu() {
    echo ""
    echo -e "${CYAN}╔══════════════════════════════════════════════════════════════╗${NC}"
    echo -e "${CYAN}║${NC}                    ${GREEN}B-UI 操作菜单${NC}                            ${CYAN}║${NC}"
    echo -e "${CYAN}╠══════════════════════════════════════════════════════════════╣${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}1.${NC} 一键安装 (Hysteria2 + Xray + 管理面板)                    ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}2.${NC} 查看客户端配置                                           ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}3.${NC} 重启所有服务                                             ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}4.${NC} 查看日志                                                 ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}5.${NC} 开启 BBR                                                 ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}6.${NC} 开机自启动设置                                           ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}7.${NC} ${GREEN}更新内核 (Hysteria2 + Xray)${NC}                              ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}8.${NC} ${RED}完全卸载${NC}                                                 ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}0.${NC} 退出                                                     ${CYAN}║${NC}"
    echo -e "${CYAN}╚══════════════════════════════════════════════════════════════╝${NC}"
}

main() {
    check_root
    check_os
    check_dependencies
    
    print_banner
    show_status
    
    while true; do
        show_menu
        read -p "请选择 [0-9]: " choice
        
        case $choice in
            1) quick_install ;;
            2) show_client_config ;;
            3) 
                systemctl restart "$HYSTERIA_SERVICE" 2>/dev/null || true
                systemctl restart "$ADMIN_SERVICE" 2>/dev/null || true
                systemctl restart xray 2>/dev/null || true
                print_success "所有服务已重启 (Hysteria2 + Xray + 管理面板)"
                ;;
            4) 
                echo ""
                echo -e "${CYAN}查看日志 (选择服务)${NC}"
                echo "  1. Hysteria2"
                echo "  2. Xray"
                echo "  3. 管理面板"
                read -p "请选择 [1-3]: " log_choice
                case $log_choice in
                    1) journalctl -u "$HYSTERIA_SERVICE" --no-pager -n 30 ;;
                    2) journalctl -u xray --no-pager -n 30 ;;
                    3) journalctl -u "$ADMIN_SERVICE" --no-pager -n 30 ;;
                    *) print_error "无效选项" ;;
                esac
                ;;
            5) enable_bbr ;;
            6) 
                echo ""
                echo -e "${CYAN}开机自启动设置${NC}"
                echo ""
                
                # 检查当前状态
                local hy_enabled=$(systemctl is-enabled "$HYSTERIA_SERVICE" 2>/dev/null || echo "disabled")
                local admin_enabled=$(systemctl is-enabled "$ADMIN_SERVICE" 2>/dev/null || echo "disabled")
                local xray_enabled=$(systemctl is-enabled xray 2>/dev/null || echo "disabled")
                
                echo -e "  Hysteria2 服务: ${YELLOW}${hy_enabled}${NC}"
                echo -e "  Xray 服务:      ${YELLOW}${xray_enabled}${NC}"
                echo -e "  管理面板服务:   ${YELLOW}${admin_enabled}${NC}"
                echo ""
                
                read -p "切换自启动状态? (y/n): " toggle
                if [[ "$toggle" == "y" || "$toggle" == "Y" ]]; then
                    if [[ "$hy_enabled" == "enabled" ]]; then
                        systemctl disable "$HYSTERIA_SERVICE" 2>/dev/null
                        systemctl disable "$ADMIN_SERVICE" 2>/dev/null
                        systemctl disable xray 2>/dev/null
                        print_success "已禁用开机自启动"
                    else
                        systemctl enable "$HYSTERIA_SERVICE" 2>/dev/null
                        systemctl enable "$ADMIN_SERVICE" 2>/dev/null
                        systemctl enable xray 2>/dev/null
                        print_success "已启用开机自启动"
                    fi
                fi
                ;;
            7)
                print_info "正在更新内核..."
                echo ""
                
                # 更新 Hysteria2
                print_info "更新 Hysteria2..."
                local old_hy=$(hysteria version 2>/dev/null | head -n1 || echo "未知")
                bash <(curl -fsSL https://get.hy2.sh/)
                local new_hy=$(hysteria version 2>/dev/null | head -n1 || echo "未知")
                echo -e "  Hysteria2: ${YELLOW}${old_hy}${NC} -> ${GREEN}${new_hy}${NC}"
                
                # 更新 Xray
                print_info "更新 Xray..."
                local old_xray=$(xray version 2>/dev/null | head -n1 || echo "未知")
                bash -c "$(curl -L https://github.com/XTLS/Xray-install/raw/main/install-release.sh)" @ install
                local new_xray=$(xray version 2>/dev/null | head -n1 || echo "未知")
                echo -e "  Xray: ${YELLOW}${old_xray}${NC} -> ${GREEN}${new_xray}${NC}"
                
                # 重启服务
                systemctl restart "$HYSTERIA_SERVICE" 2>/dev/null || true
                systemctl restart xray 2>/dev/null || true
                print_success "内核更新完成！"
                ;;
            8) uninstall_all ;;
            0) print_info "再见！"; exit 0 ;;
            *) print_error "无效选项" ;;
        esac
        
        echo ""
        read -p "按 Enter 继续..."
    done
}

main "$@"
# Force cache refresh
