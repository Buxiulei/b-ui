#!/bin/bash

#===============================================================================
# B-UI 核心安装模块
# 包含所有核心安装函数
# 版本: 动态读取自 version.json
#===============================================================================

# 颜色定义
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m'

# 路径配置
BASE_DIR="/opt/b-ui"
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
MASQUERADE_URL="https://www.bing.com/"

# 打印函数
print_info() { echo -e "${BLUE}[INFO]${NC} $1"; }
print_success() { echo -e "${GREEN}[SUCCESS]${NC} $1"; }
print_warning() { echo -e "${YELLOW}[WARNING]${NC} $1"; }
print_error() { echo -e "${RED}[ERROR]${NC} $1"; }

#===============================================================================
# 工具函数
#===============================================================================

generate_password() {
    tr -dc 'A-Za-z0-9' < /dev/urandom | head -c 16
}

generate_uuid() {
    cat /proc/sys/kernel/random/uuid 2>/dev/null || uuidgen 2>/dev/null || openssl rand -hex 16 | sed 's/\(.\{8\}\)\(.\{4\}\)\(.\{4\}\)\(.\{4\}\)\(.\{12\}\)/\1-\2-\3-\4-\5/'
}

get_server_ip() {
    curl -s --max-time 5 https://api.ipify.org 2>/dev/null || \
    curl -s --max-time 5 https://ifconfig.me 2>/dev/null || \
    curl -s --max-time 5 https://icanhazip.com 2>/dev/null || \
    hostname -I | awk '{print $1}'
}

verify_domain_dns() {
    local domain="$1"
    local server_ip=$(get_server_ip)
    
    print_info "验证域名 DNS 解析..."
    
    local dns_ip=$(dig +short "$domain" A 2>/dev/null | tail -1)
    
    if [[ -z "$dns_ip" ]]; then
        print_error "无法解析域名 $domain"
        return 1
    fi
    
    if [[ "$dns_ip" == "$server_ip" ]]; then
        print_success "DNS 验证通过: $domain -> $server_ip"
        return 0
    else
        print_warning "DNS 解析 IP ($dns_ip) 与服务器 IP ($server_ip) 不匹配"
        return 1
    fi
}

check_bbr_status() {
    local cc=$(sysctl -n net.ipv4.tcp_congestion_control 2>/dev/null)
    [[ "$cc" == "bbr" ]]
}

#===============================================================================
# 安装 Hysteria2
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
        
        mkdir -p "$BASE_DIR"
        
        # 创建 systemd 服务覆盖配置
        # 添加服务依赖和资源隔离设置，避免 Hysteria2 和 Xray 相互干扰
        mkdir -p /etc/systemd/system/hysteria-server.service.d
        cat > /etc/systemd/system/hysteria-server.service.d/override.conf << EOF
[Unit]
# 与 Xray 服务解耦，确保独立运行
# Hysteria2 使用 QUIC (UDP)，Xray 使用 TCP，互不干扰
Wants=network-online.target
After=network-online.target

[Service]
ExecStart=
ExecStart=/usr/local/bin/hysteria server --config ${CONFIG_FILE}

# 资源隔离：防止 UDP 大流量影响 TCP 服务
CPUSchedulingPolicy=other
Nice=-5
LimitNOFILE=1048576

# 确保服务稳定运行
Restart=always
RestartSec=3
EOF
        systemctl daemon-reload
    else
        print_error "Hysteria2 安装失败"
        exit 1
    fi
}

#===============================================================================
# 安装 Node.js
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

#===============================================================================
# 安装 Nginx
#===============================================================================

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

#===============================================================================
# 安装 Xray
#===============================================================================

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

#===============================================================================
# 安装中文字体
#===============================================================================

install_chinese_fonts() {
    print_info "安装中文字体..."
    
    if command -v apt-get &> /dev/null; then
        apt-get install -y fonts-noto-cjk 2>/dev/null || true
    elif command -v yum &> /dev/null; then
        yum install -y google-noto-sans-cjk-sc-fonts 2>/dev/null || true
    elif command -v dnf &> /dev/null; then
        dnf install -y google-noto-sans-cjk-sc-fonts 2>/dev/null || true
    fi
    
    if command -v fc-cache &> /dev/null; then
        fc-cache -fv > /dev/null 2>&1 || true
    fi
    
    print_success "中文字体安装完成"
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
    
    # 获取端口
    read -p "请输入 Hysteria2 监听端口 [默认: 10000]: " PORT
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
    
    # 端口跳跃配置
    read -p "是否启用端口跳跃 (抗 QoS 限速)? [默认: y]: " PORT_HOPPING_ENABLED
    PORT_HOPPING_ENABLED=${PORT_HOPPING_ENABLED:-y}
    if [[ "$PORT_HOPPING_ENABLED" =~ ^[yY]$ ]]; then
        read -p "端口跳跃范围起始 [默认: 20000]: " PORT_HOPPING_START
        PORT_HOPPING_START=${PORT_HOPPING_START:-20000}
        read -p "端口跳跃范围结束 [默认: 30000]: " PORT_HOPPING_END
        PORT_HOPPING_END=${PORT_HOPPING_END:-30000}
    fi
    
    # 预下载客户端核心（便于国内客户端安装）
    read -p "是否预下载客户端核心包 (便于国内客户端安装)? [默认: y]: " PREDOWNLOAD_PACKAGES
    PREDOWNLOAD_PACKAGES=${PREDOWNLOAD_PACKAGES:-y}
    
    # 显示配置摘要
    echo ""
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}配置摘要：${NC}"
    echo -e "  域名:       ${YELLOW}${DOMAIN}${NC}"
    echo -e "  端口:       ${YELLOW}${PORT}${NC}"
    if [[ "$PORT_HOPPING_ENABLED" =~ ^[yY]$ ]]; then
        echo -e "  端口跳跃:   ${YELLOW}${PORT_HOPPING_START}-${PORT_HOPPING_END}${NC}"
    else
        echo -e "  端口跳跃:   ${YELLOW}禁用${NC}"
    fi
    echo -e "  管理密码:   ${YELLOW}${ADMIN_PASSWORD}${NC}"
    echo -e "  首个用户:   ${YELLOW}${FIRST_USER}${NC}"
    echo -e "  用户密码:   ${YELLOW}${FIRST_USER_PASS}${NC}"
    if [[ "$PREDOWNLOAD_PACKAGES" =~ ^[yY]$ ]]; then
        echo -e "  预下载核心: ${YELLOW}是${NC}"
    else
        echo -e "  预下载核心: ${YELLOW}否${NC}"
    fi
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    
    # 保存配置到全局变量供其他函数使用
    export DOMAIN EMAIL PORT ADMIN_PASSWORD FIRST_USER FIRST_USER_PASS MASQUERADE_URL PREDOWNLOAD_PACKAGES
    export PORT_HOPPING_ENABLED PORT_HOPPING_START PORT_HOPPING_END
}

#===============================================================================
# 配置 Hysteria2
#===============================================================================

configure_hysteria() {
    print_info "生成 Hysteria2 配置文件..."
    
    mkdir -p "$BASE_DIR"
    chmod 755 "$BASE_DIR"
    
    # 创建用户文件 (包含限速信息)
    cat > "$USERS_FILE" << EOF
[{"username":"${FIRST_USER}","password":"${FIRST_USER_PASS}","createdAt":"$(date -Iseconds)","limits":{"speedLimit":100000000}}]
EOF
    
    # 生成配置文件（使用 HTTP 认证支持用户级别限速）
    cat > "$CONFIG_FILE" << EOF
# Hysteria2 服务器配置 (v2.9.0)
# 生成时间: $(date)

listen: :${PORT}

tls:
  cert: /etc/letsencrypt/live/${DOMAIN}/fullchain.pem
  key: /etc/letsencrypt/live/${DOMAIN}/privkey.pem

# QUIC 流控优化 (提升高带宽场景性能)
quic:
  initStreamReceiveWindow: 26843545
  maxStreamReceiveWindow: 26843545
  initConnReceiveWindow: 67108864
  maxConnReceiveWindow: 67108864

# HTTP 认证 (支持用户级别限速)
auth:
  type: http
  http:
    url: http://127.0.0.1:8080/auth/hysteria
    insecure: false

trafficStats:
  listen: 127.0.0.1:9999
  secret: ""

masquerade:
  type: proxy
  proxy:
    url: ${MASQUERADE_URL}
    rewriteHost: true
EOF

    chmod 644 "$CONFIG_FILE"
    chmod 644 "$USERS_FILE"
    
    print_success "配置文件已生成: $CONFIG_FILE (HTTP 认证模式)"
}

#===============================================================================
# 配置 Xray
#===============================================================================

generate_reality_keys() {
    print_info "生成 Reality 密钥对..."
    
    if [[ -f "$BASE_DIR/reality-keys.json" ]]; then
        local existing_priv=$(cat "$BASE_DIR/reality-keys.json" 2>/dev/null | grep '"privateKey"' | cut -d'"' -f4)
        if [[ -n "$existing_priv" ]]; then
            print_info "Reality 密钥已存在，跳过生成"
            return 0
        fi
    fi
    
    if ! command -v xray &> /dev/null; then
        print_warning "Xray 未安装，跳过密钥生成"
        return 1
    fi
    
    local keys=$(xray x25519 2>&1)
    local privkey=$(echo "$keys" | grep -i "^PrivateKey:" | awk -F': ' '{print $2}' | tr -d ' ')
    local pubkey=$(echo "$keys" | grep -i "^Password:" | awk -F': ' '{print $2}' | tr -d ' ')
    
    # 尝试旧格式
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
    
    generate_reality_keys
    
    if [[ ! -f "$BASE_DIR/reality-keys.json" ]]; then
        print_warning "Reality 密钥不存在，跳过 Xray 配置"
        return 1
    fi
    
    local privkey=$(cat "$BASE_DIR/reality-keys.json" | grep '"privateKey"' | cut -d'"' -f4)
    local shortid=$(cat "$BASE_DIR/reality-keys.json" | grep '"shortId"' | cut -d'"' -f4)
    
    local masq_domain="www.bing.com"
    if [[ -n "$MASQUERADE_URL" ]]; then
        masq_domain=$(echo "$MASQUERADE_URL" | sed -E 's|https?://([^/:]+).*|\1|')
    fi
    
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
    print_success "Xray 配置已生成"
}

#===============================================================================
# 配置 Nginx
#===============================================================================

configure_nginx_proxy() {
    print_info "配置 Nginx HTTPS 反向代理..."
    
    # 安装 certbot
    if command -v apt-get &> /dev/null; then
        apt-get install -y certbot python3-certbot-nginx
    elif command -v yum &> /dev/null; then
        yum install -y certbot python3-certbot-nginx
    elif command -v dnf &> /dev/null; then
        dnf install -y certbot python3-certbot-nginx
    fi
    
    # 创建 HTTP 配置
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
    
    # 申请 SSL 证书
    print_info "申请 SSL 证书..."
    certbot --nginx -d "${DOMAIN}" --non-interactive --agree-tos -m "${EMAIL}" --redirect || {
        print_warning "SSL 证书申请失败，请稍后手动配置"
    }
    
    print_success "Nginx 配置完成"
}

#===============================================================================
# 配置防火墙
#===============================================================================

configure_firewall() {
    local port=${PORT:-10000}
    local start_port=${PORT_HOPPING_START:-20000}
    local end_port=${PORT_HOPPING_END:-30000}
    
    print_info "配置防火墙..."
    
    if command -v ufw &> /dev/null && ufw status | grep -q "active"; then
        ufw allow 22/tcp
        ufw allow ${port}/udp
        ufw allow ${port}/tcp
        ufw allow 80/tcp
        ufw allow 443/tcp
        ufw allow 10001/tcp
        # 端口跳跃范围
        if [[ "$PORT_HOPPING_ENABLED" =~ ^[yY]$ ]]; then
            ufw allow ${start_port}:${end_port}/udp
            print_success "ufw 端口跳跃范围 ${start_port}:${end_port}/udp 已开放"
        fi
        print_success "ufw 规则已添加"
    elif command -v firewall-cmd &> /dev/null && systemctl is-active --quiet firewalld; then
        firewall-cmd --permanent --add-port=22/tcp
        firewall-cmd --permanent --add-port=${port}/udp
        firewall-cmd --permanent --add-port=${port}/tcp
        firewall-cmd --permanent --add-port=80/tcp
        firewall-cmd --permanent --add-port=443/tcp
        firewall-cmd --permanent --add-port=10001/tcp
        # 端口跳跃范围
        if [[ "$PORT_HOPPING_ENABLED" =~ ^[yY]$ ]]; then
            firewall-cmd --permanent --add-port=${start_port}-${end_port}/udp
            print_success "firewalld 端口跳跃范围 ${start_port}-${end_port}/udp 已开放"
        fi
        firewall-cmd --reload
        print_success "firewalld 规则已添加"
    fi
}

#===============================================================================
# 启用 BBR
#===============================================================================

enable_bbr() {
    print_info "配置 BBR 优化..."
    
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
# 端口跳跃配置 (Port Hopping)
# 使用 iptables DNAT 将端口范围转发到 Hysteria 监听端口
#===============================================================================

PORT_HOPPING_ENABLED="n"
PORT_HOPPING_START="20000"
PORT_HOPPING_END="30000"

configure_port_hopping() {
    if [[ "$PORT_HOPPING_ENABLED" != "y" && "$PORT_HOPPING_ENABLED" != "Y" ]]; then
        print_info "端口跳跃未启用，跳过配置"
        return 0
    fi
    
    local listen_port=${PORT:-10000}
    local start_port=${PORT_HOPPING_START:-20000}
    local end_port=${PORT_HOPPING_END:-30000}
    local xray_port=${XRAY_PORT:-10001}
    
    print_info "配置端口跳跃 (${start_port}-${end_port} -> ${listen_port})..."
    
    # 检测网卡名称
    local iface=$(ip route get 8.8.8.8 2>/dev/null | grep -oP 'dev \K\S+' | head -1)
    [[ -z "$iface" ]] && iface="eth0"
    
    # =========================================================================
    # 清理旧的端口跳跃规则（通过注释标识符精确匹配）
    # =========================================================================
    print_info "清理旧的端口跳跃规则..."
    
    # 获取所有带 Hysteria2-PortHopping 注释的规则行号并删除
    local rule_nums=$(iptables -t nat -L PREROUTING --line-numbers -n 2>/dev/null | grep "Hysteria2-PortHopping" | awk '{print $1}' | sort -rn)
    for num in $rule_nums; do
        iptables -t nat -D PREROUTING $num 2>/dev/null || true
    done
    
    # IPv6 清理
    local rule_nums6=$(ip6tables -t nat -L PREROUTING --line-numbers -n 2>/dev/null | grep "Hysteria2-PortHopping" | awk '{print $1}' | sort -rn)
    for num in $rule_nums6; do
        ip6tables -t nat -D PREROUTING $num 2>/dev/null || true
    done
    
    # 兼容旧规则格式清理
    iptables -t nat -D PREROUTING -i "$iface" -p udp --dport ${start_port}:${end_port} -j REDIRECT --to-ports ${listen_port} 2>/dev/null || true
    ip6tables -t nat -D PREROUTING -i "$iface" -p udp --dport ${start_port}:${end_port} -j REDIRECT --to-ports ${listen_port} 2>/dev/null || true
    
    # =========================================================================
    # 添加端口跳跃规则 - 仅处理 UDP 流量
    # 使用 -m comment 添加标识符，便于后续管理和清理
    # =========================================================================
    
    # 添加 IPv4 UDP 端口跳跃规则
    if iptables -t nat -A PREROUTING -i "$iface" -p udp --dport ${start_port}:${end_port} \
        -m comment --comment "Hysteria2-PortHopping" \
        -j REDIRECT --to-ports ${listen_port}; then
        print_success "IPv4 端口跳跃规则已添加 (UDP only)"
    else
        print_warning "IPv4 端口跳跃规则添加失败"
    fi
    
    # 添加 IPv6 UDP 端口跳跃规则
    if ip6tables -t nat -A PREROUTING -i "$iface" -p udp --dport ${start_port}:${end_port} \
        -m comment --comment "Hysteria2-PortHopping" \
        -j REDIRECT --to-ports ${listen_port} 2>/dev/null; then
        print_success "IPv6 端口跳跃规则已添加 (UDP only)"
    else
        print_info "IPv6 端口跳跃规则跳过（可能不支持）"
    fi
    
    # =========================================================================
    # 验证规则：确保 TCP 流量不受影响
    # =========================================================================
    print_info "验证协议隔离..."
    local udp_rules=$(iptables -t nat -L PREROUTING -n 2>/dev/null | grep -c "Hysteria2-PortHopping")
    local tcp_rules=$(iptables -t nat -L PREROUTING -n 2>/dev/null | grep "Hysteria2-PortHopping" | grep -c "tcp" || echo "0")
    
    if [[ "$udp_rules" -gt 0 && "$tcp_rules" -eq 0 ]]; then
        print_success "协议隔离验证通过：UDP=${udp_rules} 条规则，TCP=无影响"
    else
        print_warning "协议隔离需要验证，请检查 iptables 规则"
    fi
    
    # 持久化 iptables 规则
    if command -v iptables-save &> /dev/null; then
        mkdir -p /etc/iptables
        iptables-save > /etc/iptables/rules.v4
        ip6tables-save > /etc/iptables/rules.v6 2>/dev/null || true
        print_success "iptables 规则已持久化"
    fi
    
    # 保存端口跳跃配置（增加 xray_port 信息）
    cat > "${BASE_DIR}/port-hopping.json" << EOF
{
    "enabled": true,
    "startPort": ${start_port},
    "endPort": ${end_port},
    "listenPort": ${listen_port},
    "xrayPort": ${xray_port},
    "interface": "${iface}",
    "protocolIsolation": {
        "hysteria2": "UDP only",
        "xray": "TCP only (unaffected)"
    }
}
EOF
    
    print_success "端口跳跃配置完成（已确保 TCP/UDP 协议隔离）"
}

#===============================================================================
# 性能优化配置
# UDP 缓冲区 + QUIC 流控 + 进程优先级
#===============================================================================

configure_performance() {
    print_info "配置性能优化..."
    
    # 1. 系统 UDP 缓冲区优化
    cat > /etc/sysctl.d/99-hysteria-perf.conf << 'EOF'
# Hysteria2 性能优化 - UDP 缓冲区
net.core.rmem_max=16777216
net.core.wmem_max=16777216
net.core.rmem_default=1048576
net.core.wmem_default=1048576
EOF
    
    sysctl --system > /dev/null 2>&1
    print_success "UDP 缓冲区已优化 (16MB)"
    
    # 2. Hysteria 进程优先级优化
    mkdir -p /etc/systemd/system/hysteria-server.service.d
    cat > /etc/systemd/system/hysteria-server.service.d/priority.conf << 'EOF'
[Service]
CPUSchedulingPolicy=rr
CPUSchedulingPriority=99
EOF
    
    systemctl daemon-reload
    print_success "进程优先级已优化 (RT priority 99)"
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
# 部署 Web 面板
#===============================================================================

deploy_admin_panel() {
    print_info "部署 Web 管理面板..."
    
    mkdir -p "$ADMIN_DIR"
    
    # 从 web/ 目录复制文件（由 install.sh 下载）
    if [[ -f "${ADMIN_DIR}/server.js" ]]; then
        print_success "Web 面板文件已就位"
    else
        print_error "Web 面板文件缺失，请检查下载"
        return 1
    fi
    
    # 创建 package.json (版本号从 version.json 读取)
    local pkg_version="unknown"
    if [[ -f "${BASE_DIR}/version.json" ]]; then
        pkg_version=$(jq -r '.version' "${BASE_DIR}/version.json" 2>/dev/null || echo "unknown")
    fi
    cat > "$ADMIN_DIR/package.json" << EOF
{"name":"b-ui-admin","version":"${pkg_version}","type":"module","main":"server.js","scripts":{"start":"node server.js"},"dependencies":{"singbox-converter":"^0.0.4"}}
EOF
    
    # 安装 npm 依赖
    print_info "安装 Web 面板依赖..."
    if cd "$ADMIN_DIR" && npm install --silent 2>/dev/null; then
        print_success "依赖安装完成"
    else
        print_warning "npm 依赖安装失败，尝试使用 --legacy-peer-deps"
        cd "$ADMIN_DIR" && npm install --legacy-peer-deps 2>/dev/null || true
    fi
    cd - > /dev/null 2>&1 || true
    
    print_success "Web 面板部署完成"
}

#===============================================================================
# 创建服务
#===============================================================================

create_services() {
    print_info "创建系统服务..."
    
    # 创建管理面板服务
    cat > /etc/systemd/system/b-ui-admin.service << EOF
[Unit]
Description=B-UI Admin Panel
After=network.target

[Service]
Type=simple
Environment=ADMIN_PORT=${ADMIN_PORT}
Environment=ADMIN_PASSWORD=${ADMIN_PASSWORD}
Environment=HYSTERIA_CONFIG=${CONFIG_FILE}
Environment=USERS_FILE=${USERS_FILE}
Environment=XRAY_CONFIG=${BASE_DIR}/xray-config.json
Environment=XRAY_KEYS=${BASE_DIR}/reality-keys.json
WorkingDirectory=${ADMIN_DIR}
ExecStart=/usr/bin/node server.js
Restart=always
RestartSec=3

[Install]
WantedBy=multi-user.target
EOF

    # 创建 Xray 服务覆盖
    # 添加资源隔离设置，确保 TCP 服务稳定运行
    mkdir -p /etc/systemd/system/xray.service.d
    cat > /etc/systemd/system/xray.service.d/override.conf << EOF
[Unit]
# Xray 使用 TCP，与 Hysteria2 (UDP) 独立运行
Wants=network-online.target
After=network-online.target

[Service]
ExecStart=
ExecStart=/usr/local/bin/xray run -config ${BASE_DIR}/xray-config.json

# 资源隔离：确保 TCP 服务稳定
CPUSchedulingPolicy=other
Nice=-5
LimitNOFILE=1048576

# 确保服务稳定运行
Restart=always
RestartSec=3
EOF

    systemctl daemon-reload
    print_success "服务配置完成"
}

#===============================================================================
# 启动所有服务
#===============================================================================

start_all_services() {
    print_info "启动所有服务..."
    
    # 确保证书目录权限
    chmod 755 /etc/letsencrypt 2>/dev/null || true
    chmod 755 /etc/letsencrypt/live 2>/dev/null || true
    chmod 755 /etc/letsencrypt/archive 2>/dev/null || true
    
    systemctl enable hysteria-server --now
    systemctl enable b-ui-admin --now
    systemctl enable xray --now
    
    sleep 2
    
    if systemctl is-active --quiet hysteria-server; then
        print_success "Hysteria2 服务已启动"
    else
        print_warning "Hysteria2 服务启动失败"
    fi
    
    if systemctl is-active --quiet b-ui-admin; then
        print_success "管理面板已启动"
    else
        print_warning "管理面板启动失败"
    fi
    
    if systemctl is-active --quiet xray; then
        print_success "Xray 服务已启动"
    else
        print_warning "Xray 服务启动失败"
    fi
}
