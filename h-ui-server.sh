#!/bin/bash

#===============================================================================
# Hysteria2 一键安装脚本 (含 Web 管理面板)
# 功能：安装 Hysteria2、配置多用户、Web 管理面板、BBR 优化
# 官方文档：https://v2.hysteria.network/zh/
# 版本: 1.0.0
#===============================================================================

SCRIPT_VERSION="1.0.2"

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
ADMIN_SERVICE="hysteria-admin.service"

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
    echo "║       Hysteria2 一键安装脚本 + Web 管理面板                  ║"
    echo "║                                                              ║"
    echo "║       支持：多用户 / 自动证书 / 流量统计 / BBR              ║"
    echo "║                                                              ║"
    echo -e "║       版本: ${YELLOW}${SCRIPT_VERSION}${CYAN}                                             ║"
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
        echo -e "  ${YELLOW}curl -fsSL https://raw.githubusercontent.com/Buxiulei/h-ui/main/h-ui-server.sh -o h-ui-server.sh${NC}"
        echo -e "  ${YELLOW}sudo bash h-ui-server.sh${NC}"
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
    local deps=("curl" "grep" "awk" "sed")
    local missing=()
    
    for dep in "${deps[@]}"; do
        if ! command -v "$dep" &> /dev/null; then
            missing+=("$dep")
        fi
    done
    
    if [[ ${#missing[@]} -gt 0 ]]; then
        print_warning "缺少依赖: ${missing[*]}"
        print_info "正在安装依赖..."
        if command -v apt-get &> /dev/null; then
            apt-get update && apt-get install -y "${missing[@]}"
        elif command -v yum &> /dev/null; then
            yum install -y "${missing[@]}"
        elif command -v dnf &> /dev/null; then
            dnf install -y "${missing[@]}"
        fi
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
# 配置 Hysteria2 (多用户模式)
#===============================================================================

configure_hysteria() {
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
    read -p "请输入第一个用户名 [默认: user1]: " FIRST_USER
    FIRST_USER=${FIRST_USER:-user1}
    read -p "请输入用户密码 [默认: $DEFAULT_USER_PASS]: " FIRST_USER_PASS
    FIRST_USER_PASS=${FIRST_USER_PASS:-$DEFAULT_USER_PASS}
    
    # 伪装网站
    read -p "请输入伪装网站 URL [默认: https://www.bing.com/]: " MASQUERADE_URL
    MASQUERADE_URL=${MASQUERADE_URL:-"https://www.bing.com/"}
    
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
        firewall-cmd --permanent --add-port=${port}/udp
        firewall-cmd --permanent --add-port=${port}/tcp
        firewall-cmd --permanent --add-port=80/tcp
        firewall-cmd --reload
        print_success "firewalld 规则已添加"
    elif command -v ufw &> /dev/null && ufw status | grep -q "active"; then
        ufw allow ${port}/udp
        ufw allow ${port}/tcp
        ufw allow 80/tcp
        print_success "ufw 规则已添加"
    elif command -v iptables &> /dev/null; then
        iptables -I INPUT -p udp --dport ${port} -j ACCEPT
        iptables -I INPUT -p tcp --dport ${port} -j ACCEPT
        iptables -I INPUT -p tcp --dport 80 -j ACCEPT
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
    cat > "$ADMIN_DIR/server.js" << 'SERVEREOF'
const http=require("http"),fs=require("fs"),crypto=require("crypto"),{execSync,exec}=require("child_process");
const CONFIG={port:process.env.ADMIN_PORT||8080,adminPassword:process.env.ADMIN_PASSWORD||"admin123",
jwtSecret:process.env.JWT_SECRET||crypto.randomBytes(32).toString("hex"),
hysteriaConfig:process.env.HYSTERIA_CONFIG||"/opt/hysteria/config.yaml",usersFile:process.env.USERS_FILE||"/opt/hysteria/users.json",trafficPort:9999};

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
function saveUsers(u){try{fs.writeFileSync(CONFIG.usersFile,JSON.stringify(u,null,2));updateConfig(u);return true}catch{return false}}
function updateConfig(users){try{let c=fs.readFileSync(CONFIG.hysteriaConfig,"utf8");
const up=users.reduce((a,u)=>{a[u.username]=u.password;return a},{});
const auth="auth:\n  type: userpass\n  userpass:\n"+Object.entries(up).map(([u,p])=>"    "+u+": "+p).join("\n");
c=c.replace(/auth:[\s\S]*?(?=\n[a-zA-Z]|$)/,auth+"\n\n");
fs.writeFileSync(CONFIG.hysteriaConfig,c);execSync("systemctl restart hysteria-server",{stdio:"pipe"})}catch(e){log("ERROR",e.message)}}
function getConfig(){try{const c=fs.readFileSync(CONFIG.hysteriaConfig,"utf8");
const dm=c.match(/domains:\s*\n\s*-\s*(\S+)/),pm=c.match(/listen:\s*:?(\d+)/);
return{domain:dm?dm[1]:"localhost",port:pm?pm[1]:"443"}}catch{return{domain:"localhost",port:"443"}}}
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
const pass=params.get("pass")||crypto.randomBytes(8).toString("hex");
const days=parseInt(params.get("days"))||0;const traffic=parseFloat(params.get("traffic"))||0;const monthly=parseFloat(params.get("monthly"))||0;
const newUser={username:user,password:pass,createdAt:new Date().toISOString(),limits:{},usage:{total:0,monthly:{}}};
if(days>0)newUser.limits.expiresAt=new Date(Date.now()+days*864e5).toISOString();
if(traffic>0)newUser.limits.trafficLimit=traffic*1073741824;
if(monthly>0)newUser.limits.monthlyLimit=monthly*1073741824;
users.push(newUser);
if(saveUsers(users))return sendJSON(res,{success:true,user:user,password:pass});
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
const HTML=`<!DOCTYPE html><html lang="zh-CN"><head><meta charset="UTF-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Hysteria2 管理面板</title><style>
:root {--primary:#fb923c;--glow:rgba(251,146,60,0.4);--bg:#fff7ed;--card:rgba(255,255,255,0.7);--text:#431407;--text-dim:#9a3412;--success:#22c55e;--danger:#ef4444}
*{margin:0;padding:0;box-sizing:border-box;outline:none;-webkit-tap-highlight-color:transparent}
body{font-family:'Noto Sans SC','PingFang SC',system-ui,sans-serif;background:var(--bg);color:var(--text);min-height:100vh;overflow-x:hidden}
body::before{content:'';position:fixed;top:-50%;left:-50%;width:200%;height:200%;background:radial-gradient(circle at 50% 50%,rgba(251,146,60,0.15),transparent 60%);z-index:-1;animation:P 15s ease-in-out infinite alternate}
@keyframes P{0%{transform:scale(1)}100%{transform:scale(1.1)}}
.view{display:none}.view.active{display:block;animation:F 0.5s ease}@keyframes F{from{opacity:0;transform:translateY(10px)}to{opacity:1;transform:translateY(0)}}
.card{background:var(--card);backdrop-filter:blur(12px);border:1px solid rgba(251,146,60,0.1);border-radius:24px;padding:32px;box-shadow:0 20px 40px rgba(67,20,7,0.05)}
.btn{width:100%;padding:14px;border:none;border-radius:12px;background:linear-gradient(135deg,var(--primary),#ea580c);color:#fff;font-weight:600;cursor:pointer;transition:.3s}
.btn:hover{transform:translateY(-2px);box-shadow:0 10px 20px rgba(251,146,60,0.3)}
input{width:100%;background:rgba(255,255,255,0.5);border:1px solid rgba(67,20,7,0.05);padding:14px;border-radius:12px;color:var(--text);margin-bottom:16px;transition:.3s}
input:focus{border-color:var(--primary);box-shadow:0 0 0 2px var(--glow);background:#fff}
.login-wrap{display:flex;justify-content:center;align-items:center;min-height:100vh;padding:20px}
.nav{display:flex;justify-content:space-between;align-items:center;padding:20px 32px;background:rgba(255,247,237,0.8);backdrop-filter:blur(10px);position:sticky;top:0;z-index:10;border-bottom:1px solid rgba(67,20,7,0.05)}
.brand{font-size:20px;font-weight:700;display:flex;align-items:center;gap:12px}
.brand i{width:32px;height:32px;background:var(--primary);color:#fff;border-radius:8px;display:grid;place-items:center;font-style:normal}
.stats{display:grid;grid-template-columns:repeat(auto-fit,minmax(240px,1fr));gap:24px;padding:32px;max-width:1400px;margin:0 auto}
.stat{background:var(--card);padding:24px;border-radius:20px;border:1px solid rgba(67,20,7,0.05);transition:.3s}
.stat:hover{transform:translateY(-5px);background:#fff}
.val{font-size:32px;font-weight:700;margin:8px 0}.lbl{color:var(--text-dim);font-size:14px}
.main-area{max-width:1400px;margin:0 auto;padding:0 32px 32px}
.hdr{display:flex;justify-content:space-between;align-items:center;margin-bottom:24px}
table{width:100%;border-collapse:collapse;background:var(--card);border-radius:20px;overflow:hidden}
th,td{padding:20px;text-align:left;border-bottom:1px solid rgba(67,20,7,0.05)}
th{color:var(--text-dim);text-transform:uppercase;font-size:12px;letter-spacing:1px}
.tag{padding:4px 12px;border-radius:20px;font-size:12px;font-weight:600;background:rgba(67,20,7,0.05)}
.tag.on{background:rgba(34,197,94,0.15);color:var(--success);border:1px solid rgba(34,197,94,0.2)}
.act{display:flex;gap:8px}.ibtn{width:32px;height:32px;border-radius:8px;border:none;background:rgba(67,20,7,0.05);color:var(--text-dim);cursor:pointer;display:grid;place-items:center;transition:.2s}
.ibtn:hover{background:var(--primary);color:#fff}.ibtn.danger:hover{background:var(--danger)}
.modal{position:fixed;inset:0;background:rgba(67,20,7,0.2);backdrop-filter:blur(8px);z-index:100;display:none;align-items:center;justify-content:center;opacity:0;transition:.3s}
.modal.on{display:flex;opacity:1}.modal .card{width:90%;max-width:400px;animation:U .3s ease}@keyframes U{from{transform:translateY(20px);opacity:0}to{transform:translateY(0);opacity:1}}
.toast-box{position:fixed;bottom:30px;right:30px;display:flex;flex-direction:column;gap:10px;z-index:200}
.toast{background:#fff;color:var(--text);box-shadow:0 10px 20px rgba(0,0,0,0.1);padding:12px 20px;border-radius:12px;border:1px solid rgba(67,20,7,0.05);display:flex;align-items:center;gap:10px;animation:SI .3s ease}
.toast span{font-size:18px}@keyframes SI{from{transform:translateX(100%)}to{transform:translateX(0)}}
.code-box{background:rgba(67,20,7,0.05);padding:12px;border-radius:8px;word-break:break-all;font-family:monospace;color:var(--text-dim);margin:16px 0;font-size:12px;border:1px solid rgba(67,20,7,0.1)}
@media(max-width:768px){.stats{grid-template-columns:1fr}.main-area{padding:16px}.nav{padding:16px 20px}th,td{padding:16px}.hide-m{display:none}}
</style></head><body>
<div id="v-login" class="view active"><div class="login-wrap"><div class="card" style="max-width:360px">
<h1 style="text-align:center;margin-bottom:8px">Hysteria2</h1><p style="text-align:center;color:var(--text-dim);margin-bottom:32px">管理系统登录</p>
<input type="password" id="lp" placeholder="请输入管理密码"><button class="btn" onclick="login()">登录</button></div></div></div>
<div id="v-dash" class="view">
<nav class="nav"><div class="brand"><i>⚡</i><span>H-UI</span></div><div style="display:flex;gap:8px"><button class="ibtn" onclick="openM('m-pwd')" title="修改密码">🔑</button><button class="ibtn danger" onclick="logout()" title="退出">✕</button></div></nav>
<div class="stats">
<div class="stat"><div class="lbl">用户总数</div><div class="val" id="st-u">0</div></div>
<div class="stat"><div class="lbl">在线设备</div><div class="val" id="st-o" style="color:var(--success)">0</div></div>
<div class="stat"><div class="lbl">上传流量</div><div class="val" id="st-up">0</div></div>
<div class="stat"><div class="lbl">下载流量</div><div class="val" id="st-dl">0</div></div>
</div>
<div class="main-area"><div class="hdr"><h2 style="font-size:20px">用户列表</h2><button class="btn" style="width:auto;padding:10px 24px" onclick="openM('m-add')">+ 新建用户</button></div>
<table><thead><tr><th>用户名</th><th>状态</th><th class="hide-m">本月流量</th><th class="hide-m">累计流量</th><th>操作</th></tr></thead><tbody id="tb"></tbody></table></div>
</div>
<div id="m-add" class="modal"><div class="card"><h3>新建用户</h3><br>
<input id="nu" placeholder="用户名"><input id="np" placeholder="密码 (留空自动生成)">
<input id="nd" type="number" placeholder="有效天数 (0=不限)" min="0"><input id="nt" type="number" placeholder="总流量限制 GB (0=不限)" min="0" step="0.1">
<div style="display:flex;gap:10px"><button class="btn" style="background:rgba(67,20,7,0.1)" onclick="closeM()">取消</button><button class="btn" onclick="addUser()">创建</button></div></div></div>
<div id="m-cfg" class="modal"><div class="card" style="text-align:center"><h3>连接配置</h3><p style="font-size:12px;color:var(--text-dim);margin:0 0 8px">兼容 v2rayN / Shadowrocket / Clash Meta</p><div id="qrcode" style="margin:16px auto;background:#fff;padding:16px;border-radius:12px;width:fit-content"></div><div class="code-box" id="uri" style="margin-bottom:16px"></div>
<div style="display:flex;gap:10px"><button class="btn" onclick="copy()">复制链接</button><button class="btn" style="background:rgba(255,255,255,0.1)" onclick="closeM()">关闭</button></div></div></div>
<div id="m-pwd" class="modal"><div class="card"><h3>修改管理密码</h3><br>
<input type="password" id="newpwd" placeholder="新密码 (至少6位)">
<div style="display:flex;gap:10px"><button class="btn" style="background:rgba(67,20,7,0.1)" onclick="closeM()">取消</button><button class="btn" onclick="changePwd()">保存</button></div></div></div>
<div class="toast-box" id="t-box"></div>
<script>
const $=s=>document.querySelector(s);let tok=localStorage.getItem("t"),cfg={};
const sz=b=>{if(!b)return"0 B";const i=Math.floor(Math.log(b)/Math.log(1024));return(b/Math.pow(1024,i)).toFixed(2)+" "+["B","KB","MB","GB"][i]};
function toast(m,e){const d=document.createElement("div");d.className="toast";d.innerHTML="<span>"+(e?"⚠️":"✅")+"</span>"+m;$("#t-box").appendChild(d);setTimeout(()=>d.remove(),3000)}
function openM(id){$("#"+id).classList.add("on")} function closeM(){document.querySelectorAll(".modal").forEach(e=>e.classList.remove("on"))}
function api(ep,opt={}){return fetch("/api"+ep,{...opt,headers:{...opt.headers,Authorization:"Bearer "+tok}}).then(r=>{if(r.status==401)logout();return r.json()})}
function login(){const pw=$("#lp").value;fetch("/api/login",{method:"POST",body:JSON.stringify({password:pw})}).then(r=>r.json()).then(d=>{if(d.token){tok=d.token;localStorage.setItem("t",tok);localStorage.setItem("ap",pw);init()}else toast("密码错误",1)})}
function logout(){localStorage.removeItem("t");location.reload()}
function init(){$("#v-login").classList.remove("active");setTimeout(()=>$("#v-login").style.display="none",300);$("#v-dash").classList.add("active");
api("/config").then(d=>cfg=d);load();setInterval(load,5000)}
function load(){Promise.all([api("/users"),api("/online"),api("/stats")]).then(([u,o,s])=>{
$("#st-u").innerText=u.length;$("#st-o").innerText=Object.keys(o).length;
let tu=0,td=0;Object.values(s).forEach(v=>{tu+=v.tx||0;td+=v.rx||0});$("#st-up").innerText=sz(tu);$("#st-dl").innerText=sz(td);
const m=new Date().toISOString().slice(0,7);
u.forEach(x=>{const uri="hysteria2://"+encodeURIComponent(x.password)+"@"+cfg.domain+":"+cfg.port+"/?sni="+cfg.domain+"&insecure=0#"+encodeURIComponent(x.username);new Image().src="https://api.qrserver.com/v1/create-qr-code/?size=200x200&data="+encodeURIComponent(uri)});
$("#tb").innerHTML=u.map(x=>{
const on=o[x.username],monthly=x.usage?.monthly?.[m]||0,total=x.usage?.total||0;
const exp=x.limits?.expiresAt?new Date(x.limits.expiresAt)<new Date():"",tlim=x.limits?.trafficLimit,over=tlim&&total>=tlim;
const badge=exp?' <span style="color:var(--danger);font-size:10px">[已过期]</span>':(over?' <span style="color:var(--danger);font-size:10px">[超限]</span>':"");
return '<tr><td><b>'+x.username+'</b>'+badge+'</td><td><span class="tag '+(on?"on":"")+'">'+( on?on+" 个设备在线":"离线")+'</span></td><td class="hide-m" style="font-family:monospace;font-size:12px;color:var(--text-dim)">'+sz(monthly)+'</td><td class="hide-m" style="font-family:monospace;font-size:12px;color:var(--text-dim)">'+sz(total)+(tlim?" / "+sz(tlim):"")+'</td><td><div class="act"><button class="ibtn" onclick="show(\''+x.username+'\',\''+x.password+'\')" title="配置">⚙</button>'+(on?'<button class="ibtn danger" onclick="kick(\''+x.username+'\')">⚡</button>':'')+'<button class="ibtn danger" onclick="del(\''+x.username+'\')">🗑</button></div></td></tr>'
}).join("")})}
function addUser(){const u=$("#nu").value,p=$("#np").value,d=$("#nd").value||0,t=$("#nt").value||0;
fetch("/api/manage?key="+encodeURIComponent(cfg.adminPass||localStorage.getItem("ap")||"")+"&action=create&user="+encodeURIComponent(u)+(p?"&pass="+encodeURIComponent(p):"")+"&days="+d+"&traffic="+t).then(r=>r.json()).then(r=>{if(r.success){closeM();toast("用户 "+u+" 已创建，密码: "+r.password);load()}else toast(r.error||"创建失败",1)})}
function del(u){if(confirm("确定要删除用户 "+u+" 吗?"))api("/users/"+u,{method:"DELETE"}).then(()=>load())}
function kick(u){api("/kick",{method:"POST",body:JSON.stringify([u])}).then(()=>toast("已将用户 "+u+" 强制下线"))}
function show(u,p){const uri="hysteria2://"+encodeURIComponent(p)+"@"+cfg.domain+":"+cfg.port+"/?sni="+cfg.domain+"&insecure=0#"+encodeURIComponent(u);$("#uri").innerText=uri;$("#qrcode").innerHTML='<img src="https://api.qrserver.com/v1/create-qr-code/?size=200x200&data='+encodeURIComponent(uri)+'" alt="QR Code" style="display:block">';openM("m-cfg")}
function copy(){navigator.clipboard.writeText($("#uri").innerText);toast("已复制到剪贴板")}
function changePwd(){const np=$("#newpwd").value;if(np.length<6)return toast("密码至少6位",1);
api("/password",{method:"POST",body:JSON.stringify({newPassword:np})}).then(r=>{if(r.success){closeM();toast("密码已更新，请重新登录");setTimeout(()=>logout(),2000)}else toast(r.error||"操作失败",1)})}
if(tok)init();
</script></body></html>`;

http.createServer(async(req,res)=>{
const u=new URL(req.url,`http://${req.headers.host}`),p=u.pathname;
if(req.method==="OPTIONS"){res.writeHead(200,{"Access-Control-Allow-Origin":"*","Access-Control-Allow-Methods":"*","Access-Control-Allow-Headers":"*"});return res.end()}
if(p==="/"||p==="/index.html"){res.writeHead(200,{"Content-Type":"text/html"});return res.end(HTML)}
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
if(r.startsWith("users/")&&req.method==="DELETE"){let users=loadUsers();users=users.filter(u=>u.username!==r.slice(6));return saveUsers(users)?sendJSON(res,{success:true}):sendJSON(res,{error:"Fail"},500)}
if(r==="stats")return sendJSON(res,await fetchStats("/traffic"));
if(r==="online")return sendJSON(res,await fetchStats("/online"));
if(r==="kick"&&req.method==="POST")return sendJSON(res,await postStats("/kick",await parseBody(req)));
if(r==="config")return sendJSON(res,getConfig());
if(r==="password"&&req.method==="POST"){const b=await parseBody(req);
if(!b.newPassword||b.newPassword.length<6)return sendJSON(res,{error:"密码至少6位"},400);
try{const svc="/etc/systemd/system/hysteria-admin.service";let c=require("fs").readFileSync(svc,"utf8");
c=c.replace(/ADMIN_PASSWORD=[^\n]*/,"ADMIN_PASSWORD="+b.newPassword);
require("fs").writeFileSync(svc,c);require("child_process").execSync("systemctl daemon-reload");
return sendJSON(res,{success:true,message:"密码已更新，请重新登录"})}
catch(e){return sendJSON(res,{error:e.message},500)}}
}catch(e){return sendJSON(res,{error:e.message},500)}}
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
    print_info "创建 h-ui 命令行工具..."
    
    cat > /usr/local/bin/h-ui << 'HUIEOF'
#!/bin/bash
# H-UI 终端管理面板
# Hysteria2 + Web 管理面板

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

CONFIG_FILE="/opt/hysteria/config.yaml"
USERS_FILE="/opt/hysteria/users.json"

get_domain() {
    grep -A2 "^tls:" "$CONFIG_FILE" 2>/dev/null | grep "cert:" | sed 's|.*/live/\([^/]*\)/.*|\1|' || echo "未配置"
}

get_port() {
    grep "^listen:" "$CONFIG_FILE" 2>/dev/null | sed 's/listen: *:\?//' || echo "10000"
}

get_admin_password() {
    grep "ADMIN_PASSWORD=" /etc/systemd/system/hysteria-admin.service 2>/dev/null | cut -d= -f3 || echo "未找到"
}

show_status() {
    clear
    echo -e "${CYAN}╔══════════════════════════════════════════════════════════════╗${NC}"
    echo -e "${CYAN}║                      ${YELLOW}H-UI 管理面板${CYAN}                          ║${NC}"
    echo -e "${CYAN}╚══════════════════════════════════════════════════════════════╝${NC}"
    echo ""
    
    local domain=$(get_domain)
    local port=$(get_port)
    local admin_pass=$(get_admin_password)
    
    echo -e "${YELLOW}[系统状态]${NC}"
    if systemctl is-active --quiet hysteria-server; then
        echo -e "  Hysteria 服务: ${GREEN}✓ 运行中${NC}"
    else
        echo -e "  Hysteria 服务: ${RED}✗ 未运行${NC}"
    fi
    if systemctl is-active --quiet hysteria-admin; then
        echo -e "  管理面板服务: ${GREEN}✓ 运行中${NC}"
    else
        echo -e "  管理面板服务: ${RED}✗ 未运行${NC}"
    fi
    echo ""
    
    echo -e "${YELLOW}[配置信息]${NC}"
    echo -e "  绑定域名: ${GREEN}${domain}${NC}"
    echo -e "  Hysteria 端口: ${GREEN}${port}${NC}"
    echo -e "  管理面板: ${GREEN}https://${domain}${NC}"
    echo -e "  管理密码: ${GREEN}${admin_pass}${NC}"
    echo ""
    
    echo -e "${YELLOW}[URL 管理 API]${NC}"
    echo -e "  基础 URL: ${GREEN}https://${domain}/api/manage${NC}"
    echo ""
    echo -e "  ${CYAN}┌─ action 参数 (必填) ─────────────────────────────────────┐${NC}"
    echo -e "  ${CYAN}│${NC}  create  - 创建新用户                                    ${CYAN}│${NC}"
    echo -e "  ${CYAN}│${NC}  delete  - 删除用户                                      ${CYAN}│${NC}"
    echo -e "  ${CYAN}│${NC}  update  - 修改用户配置                                  ${CYAN}│${NC}"
    echo -e "  ${CYAN}│${NC}  list    - 列出所有用户                                  ${CYAN}│${NC}"
    echo -e "  ${CYAN}└──────────────────────────────────────────────────────────┘${NC}"
    echo ""
    echo -e "  ${CYAN}┌─ 参数说明 ───────────────────────────────────────────────┐${NC}"
    echo -e "  ${CYAN}│${NC}  key     - 管理密码 (必填)                               ${CYAN}│${NC}"
    echo -e "  ${CYAN}│${NC}  user    - 用户名 (必填，除 list 外)                     ${CYAN}│${NC}"
    echo -e "  ${CYAN}│${NC}  pass    - 密码 (可选，留空自动生成8位随机密码)          ${CYAN}│${NC}"
    echo -e "  ${CYAN}│${NC}  days    - 有效天数，单位: 天 (0=永久有效)               ${CYAN}│${NC}"
    echo -e "  ${CYAN}│${NC}  traffic - 总流量限制，单位: GB (0=不限)                 ${CYAN}│${NC}"
    echo -e "  ${CYAN}│${NC}  monthly - 月流量限制，单位: GB (0=不限)                 ${CYAN}│${NC}"
    echo -e "  ${CYAN}└──────────────────────────────────────────────────────────┘${NC}"
    echo ""
    echo -e "  ${YELLOW}示例:${NC}"
    echo -e "  ${GREEN}# 创建用户 (30天有效期，10GB总流量):${NC}"
    echo -e "  https://${domain}/api/manage?key=${admin_pass}&action=create&user=test&days=30&traffic=10"
    echo ""
    echo -e "  ${GREEN}# 删除用户:${NC}"
    echo -e "  https://${domain}/api/manage?key=${admin_pass}&action=delete&user=test"
    echo ""
    echo -e "  ${GREEN}# 修改用户配置 (续期30天，增加流量限制):${NC}"
    echo -e "  https://${domain}/api/manage?key=${admin_pass}&action=update&user=test&days=30&traffic=20"
    echo ""
    echo -e "  ${GREEN}# 列出所有用户:${NC}"
    echo -e "  https://${domain}/api/manage?key=${admin_pass}&action=list"
    echo ""
    echo -e "  ${YELLOW}按 p 修改密码, 按 q 退出, 按其他键刷新${NC}"
}

change_password() {
    echo ""
    read -p "请输入新密码 (至少6位): " new_pass
    if [[ ${#new_pass} -lt 6 ]]; then
        echo -e "${RED}密码至少6位${NC}"
        return 1
    fi
    
    local svc="/etc/systemd/system/hysteria-admin.service"
    if [[ ! -f "$svc" ]]; then
        echo -e "${RED}服务配置文件不存在${NC}"
        return 1
    fi
    
    sed -i "s/ADMIN_PASSWORD=[^ ]*/ADMIN_PASSWORD=${new_pass}/" "$svc"
    systemctl daemon-reload
    systemctl restart hysteria-admin
    
    echo -e "${GREEN}密码已更新为: ${new_pass}${NC}"
    echo -e "${YELLOW}请使用新密码登录 Web 管理面板${NC}"
}

main_loop() {
    while true; do
        show_status
        read -n1 -p "" key
        case $key in
            p|P) change_password ;;
            q|Q) exit 0 ;;
        esac
    done
}

main_loop
HUIEOF
    
    chmod +x /usr/local/bin/h-ui
    print_success "h-ui 命令已创建，可在终端输入 'h-ui' 打开管理面板"
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
    cat > "/etc/nginx/conf.d/hysteria-admin.conf" << EOF
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
        echo -e "${RED}║            SSL 证书申请将失败 - 请先解决端口问题             ║${NC}"
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
    
    # 申请证书
    if [[ "$port80_ok" == "true" ]]; then
        print_info "申请 SSL 证书..."
        certbot --nginx -d "${DOMAIN}" --non-interactive --agree-tos -m "${EMAIL}" --redirect
        
        if [[ $? -eq 0 ]]; then
            print_success "SSL 证书申请成功！"
            
            # 修复证书目录权限 (让 Hysteria 服务可以读取)
            chmod 755 /etc/letsencrypt/live 2>/dev/null || true
            chmod 755 /etc/letsencrypt/archive 2>/dev/null || true
            chmod -R 644 /etc/letsencrypt/archive/${DOMAIN}/*.pem 2>/dev/null || true
            
            # 设置证书自动续期
            if ! crontab -l 2>/dev/null | grep -q "certbot renew"; then
                (crontab -l 2>/dev/null; echo "0 3 * * * certbot renew --quiet") | crontab -
                print_info "已设置证书自动续期 (每天 3:00)"
            fi
        else
            print_warning "SSL 证书申请失败，将使用 HTTP"
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
    chmod 755 /etc/letsencrypt/live 2>/dev/null || true
    chmod 755 /etc/letsencrypt/archive 2>/dev/null || true
    if [[ -n "$DOMAIN" && -d "/etc/letsencrypt/archive/$DOMAIN" ]]; then
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
    echo -e "${RED}  警告：此操作将完全卸载 Hysteria2 和 H-UI 管理面板${NC}"
    echo -e "${RED}════════════════════════════════════════════════════════════════${NC}"
    echo ""
    echo -e "将删除以下内容："
    echo -e "  - Hysteria2 服务和二进制文件"
    echo -e "  - H-UI 管理面板服务和文件"
    echo -e "  - 所有用户配置和流量数据"
    echo -e "  - Nginx 代理配置"
    echo -e "  - SSL 证书 (可选)"
    echo -e "  - h-ui 命令行工具"
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
    systemctl stop hysteria-admin 2>/dev/null || true
    systemctl disable hysteria-server 2>/dev/null || true
    systemctl disable hysteria-admin 2>/dev/null || true
    
    # 删除 systemd 服务文件
    print_info "删除服务配置..."
    rm -f /etc/systemd/system/hysteria-server.service
    rm -f /etc/systemd/system/hysteria-admin.service
    rm -rf /etc/systemd/system/hysteria-server.service.d
    systemctl daemon-reload
    
    # 删除 Hysteria 二进制文件
    print_info "删除 Hysteria 程序..."
    rm -f /usr/local/bin/hysteria
    
    # 删除配置和数据目录
    print_info "删除配置和数据..."
    rm -rf /opt/hysteria
    rm -rf /etc/hysteria
    
    # 删除 h-ui 命令
    print_info "删除 h-ui 命令..."
    rm -f /usr/local/bin/h-ui
    
    # 删除 Nginx 配置
    print_info "删除 Nginx 配置..."
    rm -f /etc/nginx/sites-enabled/hysteria-admin
    rm -f /etc/nginx/sites-available/hysteria-admin
    rm -f /etc/nginx/conf.d/hysteria-admin.conf
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
    echo -e "已删除: Hysteria2, H-UI, Nginx, Node.js, Certbot, SSL 证书"
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
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
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
    
    install_hysteria
    echo ""
    configure_hysteria
    echo ""
    enable_bbr
    echo ""
    start_hysteria
    echo ""
    
    # 安装管理面板
    print_info "安装 Web 管理面板..."
    install_nodejs
    install_nginx
    install_chinese_fonts
    
    # 确保 Nginx 启动并开放 80 端口 (用于 Certbot 验证)
    systemctl start nginx 2>/dev/null || true
    configure_firewall "$PORT" "$ADMIN_PORT"
    
    deploy_admin_panel
    create_admin_service
    create_hui_cli
    configure_nginx_proxy
    
    echo ""
    echo -e "${GREEN}════════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}  安装完成！${NC}"
    echo -e "${GREEN}════════════════════════════════════════════════════════════════${NC}"
    echo ""
    echo -e "  管理面板: ${YELLOW}https://${DOMAIN}${NC}"
    echo -e "  管理密码: ${YELLOW}${ADMIN_PASSWORD}${NC}"
    echo ""
    show_client_config
    
    # 自动打开 h-ui 终端面板
    echo ""
    echo -e "${CYAN}正在打开 H-UI 终端管理面板...${NC}"
    sleep 2
    h-ui
}

#===============================================================================
# 主菜单
#===============================================================================

show_menu() {
    echo ""
    echo -e "${CYAN}╔══════════════════════════════════════════════════════════════╗${NC}"
    echo -e "${CYAN}║${NC}                      ${GREEN}H-UI 操作菜单${NC}                          ${CYAN}║${NC}"
    echo -e "${CYAN}╠══════════════════════════════════════════════════════════════╣${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}1.${NC} 一键安装 (Hysteria2 + 管理面板)                        ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}2.${NC} 查看状态                                               ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}3.${NC} 查看客户端配置                                         ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}4.${NC} 重启所有服务                                           ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}5.${NC} 查看日志                                               ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}6.${NC} 开启 BBR                                               ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}7.${NC} 开机自启动设置                                         ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}8.${NC} ${GREEN}更新 Hysteria2${NC}                                         ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}9.${NC} ${RED}完全卸载${NC}                                               ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}0.${NC} 退出                                                   ${CYAN}║${NC}"
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
            2) show_status ;;
            3) show_client_config ;;
            4) 
                systemctl restart "$HYSTERIA_SERVICE" 2>/dev/null || true
                systemctl restart "$ADMIN_SERVICE" 2>/dev/null || true
                print_success "服务已重启"
                ;;
            5) journalctl -u "$HYSTERIA_SERVICE" --no-pager -n 30 ;;
            6) enable_bbr ;;
            7) 
                echo ""
                echo -e "${CYAN}开机自启动设置${NC}"
                echo ""
                
                # 检查当前状态
                local hy_enabled=$(systemctl is-enabled "$HYSTERIA_SERVICE" 2>/dev/null || echo "disabled")
                local admin_enabled=$(systemctl is-enabled "$ADMIN_SERVICE" 2>/dev/null || echo "disabled")
                
                echo -e "  Hysteria2 服务: ${YELLOW}${hy_enabled}${NC}"
                echo -e "  管理面板服务:   ${YELLOW}${admin_enabled}${NC}"
                echo ""
                
                read -p "切换自启动状态? (y/n): " toggle
                if [[ "$toggle" == "y" || "$toggle" == "Y" ]]; then
                    if [[ "$hy_enabled" == "enabled" ]]; then
                        systemctl disable "$HYSTERIA_SERVICE" 2>/dev/null
                        systemctl disable "$ADMIN_SERVICE" 2>/dev/null
                        print_success "已禁用开机自启动"
                    else
                        systemctl enable "$HYSTERIA_SERVICE" 2>/dev/null
                        systemctl enable "$ADMIN_SERVICE" 2>/dev/null
                        print_success "已启用开机自启动"
                    fi
                fi
                ;;
            8)
                print_info "正在更新 Hysteria2..."
                local old_version=$(hysteria version 2>/dev/null | head -n1 || echo "未知")
                bash <(curl -fsSL https://get.hy2.sh/)
                local new_version=$(hysteria version 2>/dev/null | head -n1 || echo "未知")
                print_success "更新完成！"
                echo -e "  旧版本: ${YELLOW}${old_version}${NC}"
                echo -e "  新版本: ${GREEN}${new_version}${NC}"
                systemctl restart "$HYSTERIA_SERVICE" 2>/dev/null || true
                ;;
            9) uninstall_all ;;
            0) print_info "再见！"; exit 0 ;;
            *) print_error "无效选项" ;;
        esac
        
        echo ""
        read -p "按 Enter 继续..."
    done
}

main "$@"
