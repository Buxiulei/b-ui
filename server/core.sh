#!/bin/bash

#===============================================================================
# B-UI 核心安装模块
# 包含所有核心安装函数
# 版本: 2.4.0
#===============================================================================

# 颜色定义
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m'

# 路径配置
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
        mkdir -p /etc/systemd/system/hysteria-server.service.d
        cat > /etc/systemd/system/hysteria-server.service.d/override.conf << EOF
[Service]
ExecStart=
ExecStart=/usr/local/bin/hysteria server --config ${CONFIG_FILE}
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
    
    # 保存配置到全局变量供其他函数使用
    export DOMAIN EMAIL PORT ADMIN_PASSWORD FIRST_USER FIRST_USER_PASS MASQUERADE_URL
}

#===============================================================================
# 配置 Hysteria2
#===============================================================================

configure_hysteria() {
    print_info "生成 Hysteria2 配置文件..."
    
    mkdir -p "$BASE_DIR"
    chmod 755 "$BASE_DIR"
    
    # 创建用户文件
    cat > "$USERS_FILE" << EOF
[{"username":"${FIRST_USER}","password":"${FIRST_USER_PASS}","createdAt":"$(date -Iseconds)"}]
EOF
    
    # 生成配置文件
    cat > "$CONFIG_FILE" << EOF
# Hysteria2 服务器配置
# 生成时间: $(date)

listen: :${PORT}

tls:
  cert: /etc/letsencrypt/live/${DOMAIN}/fullchain.pem
  key: /etc/letsencrypt/live/${DOMAIN}/privkey.pem

auth:
  type: userpass
  userpass:
    ${FIRST_USER}: ${FIRST_USER_PASS}

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
    
    print_success "配置文件已生成: $CONFIG_FILE"
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
    
    print_info "配置防火墙..."
    
    if command -v ufw &> /dev/null && ufw status | grep -q "active"; then
        ufw allow 22/tcp
        ufw allow ${port}/udp
        ufw allow ${port}/tcp
        ufw allow 80/tcp
        ufw allow 443/tcp
        ufw allow 10001/tcp
        print_success "ufw 规则已添加"
    elif command -v firewall-cmd &> /dev/null && systemctl is-active --quiet firewalld; then
        firewall-cmd --permanent --add-port=22/tcp
        firewall-cmd --permanent --add-port=${port}/udp
        firewall-cmd --permanent --add-port=${port}/tcp
        firewall-cmd --permanent --add-port=80/tcp
        firewall-cmd --permanent --add-port=443/tcp
        firewall-cmd --permanent --add-port=10001/tcp
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
    
    # 创建 package.json
    cat > "$ADMIN_DIR/package.json" << 'EOF'
{"name":"b-ui-admin","version":"2.4.0","main":"server.js","scripts":{"start":"node server.js"}}
EOF
    
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
    mkdir -p /etc/systemd/system/xray.service.d
    cat > /etc/systemd/system/xray.service.d/override.conf << EOF
[Service]
ExecStart=
ExecStart=/usr/local/bin/xray run -config ${BASE_DIR}/xray-config.json
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
