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
CERTS_DIR="${BASE_DIR}/certs"
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
    # 兼容 bbr / bbr3 / bbrv3 / bbr_v3
    [[ "$cc" == "bbr" || "$cc" == "bbr3" || "$cc" == "bbrv3" || "$cc" == "bbr_v3" ]]
}

# 检测系统支持的最佳 BBR 拥塞算法（BBRv3 优先）
# 输出：bbr3 / bbrv3 / bbr_v3 / bbr / 空字符串（不支持）
detect_best_bbr_algo() {
    local available
    available=$(cat /proc/sys/net/ipv4/tcp_available_congestion_control 2>/dev/null || echo "")
    # 按优先级匹配（BBRv3 > BBR）
    for algo in bbr3 bbrv3 bbr_v3 bbr; do
        if [[ " $available " == *" $algo "* ]]; then
            echo "$algo"
            return 0
        fi
    done
    echo ""
    return 1
}

#===============================================================================
# 安装 Hysteria2
#===============================================================================

install_hysteria() {
    print_info "正在安装 Hysteria2（双实例：direct + residential）..."

    if command -v hysteria &> /dev/null; then
        print_warning "Hysteria2 已安装，版本: $(hysteria version 2>/dev/null | head -n1)"
        read -p "是否重新安装/升级？(y/n): " reinstall
        if [[ "$reinstall" != "y" && "$reinstall" != "Y" ]]; then
            : # 二进制不重装，但仍要生成 systemd unit（首次升级到 v3.5 必经路径）
        else
            HYSTERIA_USER=root bash <(curl -fsSL https://get.hy2.sh/)
        fi
    else
        HYSTERIA_USER=root bash <(curl -fsSL https://get.hy2.sh/)
    fi

    if ! command -v hysteria &> /dev/null; then
        print_error "Hysteria2 安装失败"
        exit 1
    fi
    print_success "Hysteria2 二进制就绪"

    mkdir -p "$BASE_DIR"

    # ────────────────────────────────────────────────────────────────────
    # 写"按实例"端口跳跃孤儿链清理 helper（ExecStartPre 用）
    # hysteria 内置端口跳跃用的是 iptables/ip6tables 的 HYSTERIA-PR-<hash> NAT 链
    # （不是 nft 的 hysteria_* 表——旧 hy2-nft-cleanup.sh 清错了对象且 nft 常未安装）。
    # SIGKILL/OOM 后该链残留，下次启动 `ip6tables -N` 报 "Chain already exists" → FATAL 崩溃循环。
    # 关键：只清"REDIRECT 到本实例 base 端口"的链，绝不碰另一实例
    #       （v3.5.1 共享 cleanup 跨实例误删 regression 的正确修法）。
    # 用法：hy2-portjump-cleanup.sh <base_port>
    # ────────────────────────────────────────────────────────────────────
    cat > /opt/b-ui/hy2-portjump-cleanup.sh <<'CLEANUP_EOF'
#!/bin/sh
# 删除"只属于本实例"的孤儿端口跳跃 NAT 规则。按本实例 base 端口(REDIRECT 目标) + 跳跃端口段
# (--dport range) 双重定位：既清"有 redirect 规则的完整孤儿"，也清"crash 在 -N 之后、加 redirect
# 之前残留的空链(0 内部规则但有 dport 跳转)"——后者是 v3.5.15 实测的崩溃循环元凶。range 按实例唯一
# (直连 20000-30000 / 住宅 41000-50000)，绝不跨实例误删。同时覆盖 iptables 与 nft 后端，不强制切换。
# 正常 SIGTERM 退出 hysteria 已自清，本脚本为 no-op；仅 SIGKILL/OOM 残留时清理。
# 参数：hysteria 配置文件路径(从 listen: :BASE,RANGE 提取)，或纯 base 端口数字(无 range 只清完整孤儿)。
arg="$1"
[ -z "$arg" ] && exit 0
case "$arg" in
    *[!0-9]*)
        base=$(awk '/^listen:/{ sub(/^listen: *:/,""); split($0,a,","); print a[1]; exit }' "$arg" 2>/dev/null)
        range=$(awk '/^listen:/{ sub(/^listen: *:/,""); split($0,a,","); print a[2]; exit }' "$arg" 2>/dev/null)
        ;;
    *)  base="$arg"; range="" ;;
esac
[ -z "$base" ] && exit 0
rangec=$(printf '%s' "$range" | tr '-' ':')

# 后端 A: iptables / ip6tables —— 收集本实例链名(redirect 到 base 的完整链 + dport range 跳转引用的链含空链)
for ipt in iptables ip6tables; do
    command -v "$ipt" >/dev/null 2>&1 || continue
    S=$("$ipt" -t nat -S 2>/dev/null)
    chains=$(printf '%s\n' "$S" | sed -n "s/^-A \\(HYSTERIA-PR-[A-Za-z0-9]*\\) .*--to-ports ${base}\$/\\1/p")
    if [ -n "$rangec" ]; then
        chains="${chains}
$(printf '%s\n' "$S" | sed -n "s/^-A .* --dport ${rangec} -j \\(HYSTERIA-PR-[A-Za-z0-9]*\\)\$/\\1/p")"
    fi
    chains=$(printf '%s\n' "$chains" | sort -u | grep -v '^$')
    [ -z "$chains" ] && continue
    for ch in $chains; do
        printf '%s\n' "$S" | grep -- "-j ${ch}\$" | while read -r line; do
            # shellcheck disable=SC2086
            "$ipt" -t nat $(printf '%s' "$line" | sed 's/^-A /-D /') 2>/dev/null || true
        done
        "$ipt" -t nat -F "$ch" 2>/dev/null || true
        "$ipt" -t nat -X "$ch" 2>/dev/null || true
    done
done

# 后端 B: nft —— 删 REDIRECT 到本 base 端口的 hysteria_<hash> 表（仅本实例，按 redirect 目标端口判定）
if command -v nft >/dev/null 2>&1; then
    nft list tables 2>/dev/null | awk '$3 ~ /^hysteria_/{print $2, $3}' | while read -r fam tbl; do
        if nft list table "$fam" "$tbl" 2>/dev/null | grep -qE "redirect to :${base}([^0-9]|\$)"; then
            nft delete table "$fam" "$tbl" 2>/dev/null || true
        fi
    done
fi
exit 0
CLEANUP_EOF
    chmod 755 /opt/b-ui/hy2-portjump-cleanup.sh

    # ────────────────────────────────────────────────────────────────────
    # 实例 1: hysteria-server.service (direct) — 监听 :10000+20000-30000
    # 保留旧 unit 名兼容老订阅 URL（vless@host:10000 仍指向 direct）
    # ────────────────────────────────────────────────────────────────────
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

# 内存回收：让 Go runtime 主动归还内存给 OS（Go 1.19+）
# 不设此变量时 hysteria 长跑后 RSS 会缓慢爬升至历史峰值不释放
Environment=GOMEMLIMIT=400MiB

# 日志级别：默认 info 在大流量下每秒数十条 disconnect 信息，污染 journal 且 IO 开销大
# warn 仅保留异常和错误，能显著降低 SSD 写入与日志噪音
Environment=HYSTERIA_LOG_LEVEL=warn

# cgroup 兜底：超过 500M 开始 throttle，700M 硬上限触发 OOM-restart
# 适配 1G 小内存机器；2G+ 机器可手动放宽到 800M/1G
MemoryHigh=500M
MemoryMax=700M

# 启动前清理"仅本实例"的孤儿端口跳跃 NAT 链（base 端口从 config.yaml 的 listen 提取，支持自定义端口）
# SIGKILL/OOM 后 hysteria 没走完 closer chain 会残留 HYSTERIA-PR-<hash> 链，
# 下次启动 ip6tables -N 报 "Chain already exists" → FATAL 崩溃循环（v3.5.13 实测踩坑）。
# 按 --to-ports <base> 精确清理，绝不碰住宅实例（:40000）的链。`-` 前缀使清理失败不致命。
ExecStartPre=-/opt/b-ui/hy2-portjump-cleanup.sh ${CONFIG_FILE}

# 给 hy2 充足时间走完 closer chain 删自己的 NAT 链（正常 <1s）
TimeoutStopSec=15

# 确保服务稳定运行
Restart=always
RestartSec=3
EOF

    # ────────────────────────────────────────────────────────────────────
    # 实例 2: hysteria-residential.service — 监听 :40000+41000-50000
    # 出战指向 socks5 127.0.0.1:2080 (b-ui-relay sing-box 自动选住宅 URL)
    # ────────────────────────────────────────────────────────────────────
    cat > /etc/systemd/system/hysteria-residential.service <<'UNIT_EOF'
[Unit]
Description=Hysteria Server (Residential) Service
Documentation=https://v2.hysteria.network/
After=network.target network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/hysteria server --config /opt/b-ui/config-residential.yaml
WorkingDirectory=/etc/hysteria
User=root
Group=root
LimitNPROC=512
LimitNOFILE=1048576
CPUSchedulingPolicy=other
Nice=-5
Environment=GOMEMLIMIT=200MiB
Environment=HYSTERIA_LOG_LEVEL=warn
MemoryHigh=300M
MemoryMax=500M
# 启动前清理"仅本实例"的孤儿端口跳跃 NAT 链（从 config-residential.yaml 提取 base=40000+range）
# 绝不碰直连实例（:10000）的链。`-` 前缀使清理失败不致命。详见 hy2-portjump-cleanup.sh
ExecStartPre=-/opt/b-ui/hy2-portjump-cleanup.sh /opt/b-ui/config-residential.yaml
TimeoutStopSec=15
Restart=always
RestartSec=3

[Install]
WantedBy=multi-user.target
UNIT_EOF

    systemctl daemon-reload

    # 小内存机器（≤2G）自动降低 swappiness，避免 hysteria 工作集被换出导致卡顿
    local mem_mb
    mem_mb=$(awk '/^MemTotal:/ {print int($2/1024)}' /proc/meminfo)
    if [[ $mem_mb -le 2048 ]]; then
        print_info "检测到物理内存 ${mem_mb}MB ≤ 2G，应用 swappiness 优化"
        cat > /etc/sysctl.d/99-b-ui-memory.conf <<'SYSCTL_EOF'
# b-ui 内存策略：小内存机器（≤2G）降低 swap 倾向
# 配合 hysteria 两个实例的 GOMEMLIMIT/MemoryHigh/MemoryMax 一起生效
vm.swappiness = 10
SYSCTL_EOF
        sysctl -p /etc/sysctl.d/99-b-ui-memory.conf >/dev/null 2>&1 && \
            print_success "swappiness 已设为 10"
    else
        print_info "物理内存 ${mem_mb}MB > 2G，保持系统默认 swappiness"
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

install_caddy() {
    print_info "检查 Caddy..."
    
    if command -v caddy &> /dev/null; then
        print_success "Caddy 已安装: $(caddy version 2>/dev/null | head -1)"
        return 0
    fi
    
    print_info "安装 Caddy..."
    if command -v apt-get &> /dev/null; then
        # Debian/Ubuntu: 使用官方 APT 仓库
        apt-get install -y debian-keyring debian-archive-keyring apt-transport-https curl 2>/dev/null || true
        curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' | gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg 2>/dev/null
        curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' | tee /etc/apt/sources.list.d/caddy-stable.list > /dev/null
        apt-get update -qq && apt-get install -y caddy
    elif command -v yum &> /dev/null; then
        # CentOS/RHEL: 使用官方 YUM 仓库
        yum install -y yum-plugin-copr 2>/dev/null || true
        yum copr enable -y @caddy/caddy 2>/dev/null || true
        yum install -y caddy
    elif command -v dnf &> /dev/null; then
        dnf install -y 'dnf-command(copr)' 2>/dev/null || true
        dnf copr enable -y @caddy/caddy 2>/dev/null || true
        dnf install -y caddy
    fi
    
    if ! command -v caddy &> /dev/null; then
        print_error "Caddy 安装失败"
        return 1
    fi
    
    systemctl enable caddy
    print_success "Caddy 安装完成"
}

# 兼容旧调用
install_nginx() {
    install_caddy
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
    mkdir -p "$CERTS_DIR"
    chmod 755 "$BASE_DIR"
    
    # 创建用户文件 (包含限速信息)
    cat > "$USERS_FILE" << EOF
[{"username":"${FIRST_USER}","password":"${FIRST_USER_PASS}","createdAt":"$(date -Iseconds)","limits":{"speedLimit":100000000}}]
EOF
    
    # 决定 listen 字段：启用端口跳跃时使用 hysteria 2.9+ 内置多端口语法
    local direct_listen=":${PORT}"
    if [[ "$PORT_HOPPING_ENABLED" =~ ^[yY]$ ]]; then
        direct_listen=":${PORT},${PORT_HOPPING_START:-20000}-${PORT_HOPPING_END:-30000}"
    fi
    local resi_listen=":40000,41000-50000"
    local resi_traffic_port="9998"

    # ────────────────────────────────────────────────────────────────────
    # 配置 1：config.yaml — hysteria-direct (无 outbounds 块，hy2 内置 direct)
    # ────────────────────────────────────────────────────────────────────
    cat > "$CONFIG_FILE" << EOF
# Hysteria2 服务器配置 — Direct 实例 (v3.5)
# 生成时间: $(date)

listen: ${direct_listen}

tls:
  sniGuard: disable
  cert: ${CERTS_DIR}/fullchain.pem
  key: ${CERTS_DIR}/privkey.pem

quic:
  maxIdleTimeout: 60s

ignoreClientBandwidth: true

# DoH 防 DNS 投毒（hy2 v2.x 原生支持）
resolver:
  type: https
  https:
    addr: "1.1.1.1:443"
    sni: cloudflare-dns.com

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

sniff:
  enable: true
  timeout: 2s
  rewriteDomain: true
  tcpPorts: 80,443,8000-9000
  udpPorts: 443,53
EOF

    # ────────────────────────────────────────────────────────────────────
    # 配置 2：config-residential.yaml — hysteria-residential
    # 流量出战指向 127.0.0.1:2080 (b-ui-relay sing-box 决定 direct or 住宅池)
    # ────────────────────────────────────────────────────────────────────
    cat > "${BASE_DIR}/config-residential.yaml" << EOF
# Hysteria2 服务器配置 — Residential 实例 (v3.5)
# 出战 SOCKS5 → b-ui-relay sing-box → direct 或 住宅 URL urltest 池
# 生成时间: $(date)

listen: ${resi_listen}

tls:
  sniGuard: disable
  cert: ${CERTS_DIR}/fullchain.pem
  key: ${CERTS_DIR}/privkey.pem

quic:
  maxIdleTimeout: 60s

ignoreClientBandwidth: true

resolver:
  type: https
  https:
    addr: "1.1.1.1:443"
    sni: cloudflare-dns.com

auth:
  type: http
  http:
    url: http://127.0.0.1:8080/auth/hysteria
    insecure: false

trafficStats:
  listen: 127.0.0.1:${resi_traffic_port}
  secret: ""

masquerade:
  type: proxy
  proxy:
    url: ${MASQUERADE_URL}
    rewriteHost: true

sniff:
  enable: true
  timeout: 2s
  rewriteDomain: true
  tcpPorts: 80,443,8000-9000
  udpPorts: 443,53

outbounds:
  - name: relay
    type: socks5
    socks5:
      addr: "127.0.0.1:2080"
  - name: direct
    type: direct

acl:
  inline:
    - relay(all)
EOF

    chmod 644 "$CONFIG_FILE" "${BASE_DIR}/config-residential.yaml" "$USERS_FILE"

    # v3.5.15: 安装即把两个 hy2 实例的 auth 从 http 转成本地 userpass
    # http auth 每条连接都回调 b-ui-admin /auth/hysteria，几十客户端共享一条订阅、
    # 重连风暴时单线程面板成 SPOF；userpass 本地鉴权无依赖，高并发更稳。
    # （这是 b-ui 既有设计：原本等首次面板存用户才切，这里提前到安装期。）
    apply_hy2_userpass_auth

    print_success "Hysteria 双配置已生成: config.yaml (direct) + config-residential.yaml (resi)"
}

# 把 hy2 两实例的 auth 块从 http 改成本地 userpass（读 users.json）。幂等：已是 userpass 则跳过。
# 安装期服务未起→仅改文件；update 期服务在跑→改完 restart。
apply_hy2_userpass_auth() {
    local uf="${USERS_FILE:-${BASE_DIR}/users.json}"
    [[ -f "$uf" ]] || return 0
    command -v jq >/dev/null 2>&1 || return 0
    local tmp_up; tmp_up=$(mktemp)
    jq -r '.[] | "    \(.username): \(.password)"' "$uf" 2>/dev/null > "$tmp_up"
    [[ -s "$tmp_up" ]] || { rm -f "$tmp_up"; return 0; }
    local pair cfg svc
    for pair in "config.yaml:hysteria-server" "config-residential.yaml:hysteria-residential"; do
        cfg="${BASE_DIR}/${pair%%:*}"; svc="${pair##*:}"
        [[ -f "$cfg" ]] || continue
        grep -q '^  type: http' "$cfg" || continue   # 已是 userpass 就跳过
        awk -v upfile="$tmp_up" '
            /^auth:/ {print "auth:"; print "  type: userpass"; print "  userpass:"; while ((getline line < upfile) > 0) print line; close(upfile); skip=1; next}
            skip && /^[a-zA-Z]/ {skip=0}
            !skip {print}
        ' "$cfg" > "${cfg}.tmp" && mv "${cfg}.tmp" "$cfg"
        chmod 644 "$cfg"
        systemctl is-active --quiet "$svc" 2>/dev/null && systemctl restart "$svc" 2>/dev/null || true
    done
    rm -f "$tmp_up"
}

#===============================================================================
# 配置 Xray
#===============================================================================

generate_reality_keys() {
    print_info "生成 Reality 密钥对..."
    
    if [[ -f "$BASE_DIR/reality-keys.json" ]]; then
        local existing_priv=$(cat "$BASE_DIR/reality-keys.json" 2>/dev/null | grep '"privateKey"' | cut -d'"' -f4)
        local existing_pub=$(cat "$BASE_DIR/reality-keys.json" 2>/dev/null | grep '"publicKey"' | cut -d'"' -f4)
        if [[ -n "$existing_priv" && -n "$existing_pub" ]]; then
            print_info "Reality 密钥已存在，跳过生成"
            return 0
        fi
    fi
    
    if ! command -v xray &> /dev/null; then
        print_warning "Xray 未安装，跳过密钥生成"
        return 1
    fi
    
    local keys=$(xray x25519 2>&1)
    # xray x25519 输出格式：
    #   PrivateKey: xxx
    #   Password (PublicKey): xxx    (Xray 26.x+)
    #   PublicKey: xxx               (旧版)
    #   Public key: xxx              (更旧版)
    local privkey=$(echo "$keys" | grep -i "^PrivateKey:" | awk -F': ' '{print $2}' | tr -d ' ')
    local pubkey=$(echo "$keys" | grep -i "PublicKey" | awk -F': ' '{print $2}' | tr -d ' ')

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

    # 备份到外部目录（防 b-ui 重装时丢密钥导致客户端订阅 reality verification failed）
    mkdir -p /root/.b-ui-backup /var/backups/b-ui 2>/dev/null
    cp "$BASE_DIR/reality-keys.json" /root/.b-ui-backup/reality-keys.json 2>/dev/null || true
    cp "$BASE_DIR/reality-keys.json" /var/backups/b-ui/reality-keys.json 2>/dev/null || true
    chmod 600 /root/.b-ui-backup/reality-keys.json /var/backups/b-ui/reality-keys.json 2>/dev/null || true

    print_success "Reality 密钥已生成（已备份到 /root/.b-ui-backup/ 和 /var/backups/b-ui/）"
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
  "dns": {
    "servers": [
      "https+local://1.1.1.1/dns-query",
      "8.8.8.8"
    ],
    "queryStrategy": "UseIPv4"
  },
  "inbounds": [
    {"tag": "api", "port": 10085, "listen": "127.0.0.1", "protocol": "dokodemo-door", "settings": {"address": "127.0.0.1"}},
    {
      "tag": "vless-direct",
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
    },
    {
      "tag": "vless-residential",
      "port": 10002,
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
  "outbounds": [
    {"tag": "direct", "protocol": "freedom"},
    {"tag": "relay", "protocol": "socks", "settings": {"servers": [{"address": "127.0.0.1", "port": 2080}]}}
  ],
  "routing": {
    "rules": [
      {"type": "field", "inboundTag": ["api"], "outboundTag": "api"},
      {"type": "field", "inboundTag": ["vless-direct"], "outboundTag": "direct"},
      {"type": "field", "inboundTag": ["vless-residential"], "outboundTag": "relay"}
    ]
  }
}
EOF
    chmod 644 "$BASE_DIR/xray-config.json"
    print_success "Xray 配置已生成（双 inbound：vless-direct + vless-residential）"
}

#===============================================================================
# 配置 Nginx
#===============================================================================

configure_caddy_proxy() {
    print_info "配置 Caddy HTTPS 反向代理..."
    
    # 如果有旧的 nginx 配置，停用 nginx 释放 80/443 端口
    if command -v nginx &> /dev/null; then
        systemctl stop nginx 2>/dev/null || true
        systemctl disable nginx 2>/dev/null || true
        print_info "已停用 Nginx (Caddy 接管 80/443)"
    fi
    
    # 创建共享证书目录
    mkdir -p "$CERTS_DIR"
    chmod 755 "$CERTS_DIR"
    
    # 生成 Caddyfile (自动 HTTPS + 反向代理)
    cat > /etc/caddy/Caddyfile << CADDYEOF
# B-UI Web 管理面板 - 由 Caddy 自动管理 HTTPS 证书
${DOMAIN} {
    # 反代到 Node.js 管理面板
    reverse_proxy 127.0.0.1:${ADMIN_PORT}

    # 日志
    log {
        output file /var/log/caddy/b-ui-access.log {
            roll_size 10mb
            roll_keep 5
        }
    }
}
CADDYEOF

    # 创建日志目录 (Caddy 以 caddy 用户运行)
    mkdir -p /var/log/caddy
    chown -R caddy:caddy /var/log/caddy

    # 添加 systemd override: Caddy 官方 service 使用 ProtectSystem=full
    # 导致 /var/log 只读，必须显式放行日志目录写入
    mkdir -p /etc/systemd/system/caddy.service.d
    cat > /etc/systemd/system/caddy.service.d/override.conf << 'OVEOF'
[Service]
ReadWritePaths=/var/log/caddy
OVEOF
    systemctl daemon-reload

    # 验证配置
    if caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile 2>/dev/null; then
        print_success "Caddy 配置验证通过"
    else
        print_warning "Caddy 配置验证失败，请检查域名配置"
    fi

    # caddy validate 以 root 运行会创建 owner=root 的日志文件，
    # 导致后续 caddy 用户启动时 permission denied，必须清理
    rm -f /var/log/caddy/b-ui-access.log 2>/dev/null
    chown -R caddy:caddy /var/log/caddy

    # 重启 Caddy (自动申请 SSL 证书)
    systemctl restart caddy
    print_success "Caddy 反向代理已配置 (自动 HTTPS: ${DOMAIN})"
    
    # 创建证书同步脚本和服务
    setup_cert_sync
}

# 兼容旧调用
configure_nginx_proxy() {
    configure_caddy_proxy
}

#===============================================================================
# Caddy 证书同步到 Hysteria2
# Caddy 自动管理证书，同步到 /opt/b-ui/certs/ 供 Hysteria2 使用
#===============================================================================

setup_cert_sync() {
    print_info "配置证书同步机制..."
    
    local sync_script="${BASE_DIR}/cert-sync.sh"
    local caddy_data_dir="/var/lib/caddy/.local/share/caddy"
    
    # 创建证书同步脚本
    cat > "$sync_script" << 'SYNCEOF'
#!/bin/bash
# Caddy 证书同步脚本
# 从 Caddy 数据目录复制证书到共享目录供 Hysteria2 使用

CERTS_DIR="/opt/b-ui/certs"
CADDY_DATA="/var/lib/caddy/.local/share/caddy"
DOMAIN_FILE="/opt/b-ui/certs/.domain"
LOG_FILE="/var/log/b-ui-cert-sync.log"

log() {
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] $1" >> "$LOG_FILE"
}

# 读取域名
if [[ ! -f "$DOMAIN_FILE" ]]; then
    log "ERROR: 域名配置文件不存在: $DOMAIN_FILE"
    exit 1
fi
DOMAIN=$(cat "$DOMAIN_FILE")

# 轮询 60s 等 Caddy ACME 流程完成（OnBootSec=30s 触发时 Caddy 可能还在签发）
# 找不到/不完整时 exit 0 而不是 exit 1，避免 systemd 误报失败；下次 timer 会再触发
CERT_SOURCE=""
for i in $(seq 1 30); do
    CERT_SOURCE="${CADDY_DATA}/certificates/acme-v02.api.letsencrypt.org-directory/${DOMAIN}"
    [[ -d "$CERT_SOURCE" ]] || CERT_SOURCE=$(find "$CADDY_DATA/certificates" -type d -name "$DOMAIN" 2>/dev/null | head -1)
    if [[ -n "$CERT_SOURCE" && -d "$CERT_SOURCE" \
          && -f "${CERT_SOURCE}/${DOMAIN}.crt" \
          && -f "${CERT_SOURCE}/${DOMAIN}.key" ]]; then
        break
    fi
    CERT_SOURCE=""
    sleep 2
done
if [[ -z "$CERT_SOURCE" ]]; then
    log "INFO: Caddy 证书暂未就绪 (域名: $DOMAIN)，等待下次 timer/cron 触发"
    exit 0
fi

# 证书和密钥文件路径
CERT_FILE="${CERT_SOURCE}/${DOMAIN}.crt"
KEY_FILE="${CERT_SOURCE}/${DOMAIN}.key"

# 比较文件是否有变化
mkdir -p "$CERTS_DIR"
if [[ -f "${CERTS_DIR}/fullchain.pem" ]] && cmp -s "$CERT_FILE" "${CERTS_DIR}/fullchain.pem"; then
    # 证书未变化，无需同步
    exit 0
fi

# 同步证书
cp "$CERT_FILE" "${CERTS_DIR}/fullchain.pem"
cp "$KEY_FILE" "${CERTS_DIR}/privkey.pem"
chmod 644 "${CERTS_DIR}/fullchain.pem"
chmod 600 "${CERTS_DIR}/privkey.pem"

log "SUCCESS: 证书已同步 (${DOMAIN})"

# 证书变了 → 重启所有在跑的 hysteria 实例
# hysteria 仅在启动时读证书、无热重载（CanReload=no，reload 是空操作），必须 restart；
# 两个实例（direct + residential）共用同一份 /opt/b-ui/certs，都要重启，否则住宅节点续期后用旧证书 → 过期即挂。
for u in hysteria-server hysteria-residential; do
    if systemctl is-active --quiet "$u" 2>/dev/null; then
        systemctl restart "$u" 2>/dev/null && log "$u 已重启以加载新证书"
    fi
done

# 保留最近 200 行日志
tail -200 "$LOG_FILE" > "${LOG_FILE}.tmp" && mv "${LOG_FILE}.tmp" "$LOG_FILE" 2>/dev/null
SYNCEOF

    chmod +x "$sync_script"
    
    # 保存域名到文件供同步脚本使用
    echo "$DOMAIN" > "${CERTS_DIR}/.domain"
    
    # 创建 systemd 定时同步服务
    cat > /etc/systemd/system/b-ui-cert-sync.service << EOF
[Unit]
Description=B-UI Certificate Sync (Caddy -> Hysteria2)
After=caddy.service

[Service]
Type=oneshot
# 等 Caddy 完全就绪：进程 active + 证书目录已生成（最多 30s）
ExecStartPre=/bin/bash -c 'until systemctl is-active --quiet caddy; do sleep 1; done; sleep 3'
ExecStart=${sync_script}
EOF

    cat > /etc/systemd/system/b-ui-cert-sync.timer << EOF
[Unit]
Description=B-UI Certificate Sync Timer

[Timer]
# 首次启动后 30 秒执行 (等待 Caddy 申请证书)
OnBootSec=30s
# 之后每 6 小时检查一次证书更新
OnUnitActiveSec=6h
AccuracySec=1min

[Install]
WantedBy=timers.target
EOF

    systemctl daemon-reload
    systemctl enable b-ui-cert-sync.timer 2>/dev/null
    systemctl start b-ui-cert-sync.timer 2>/dev/null

    print_success "证书同步机制已配置"
}

#===============================================================================
# Hysteria2 半死自愈 watchdog
# 进程在但 UDP 监听失活时强制重启（每 5 min 检测，连续 3 次失败触发）
#===============================================================================

setup_hy2_watchdog() {
    print_info "配置 Hysteria2 watchdog（半死自愈）..."

    local watchdog_script="${BASE_DIR}/hy2-watchdog.sh"
    local listen_port=${PORT:-10000}

    # 写入 watchdog 脚本（覆盖 direct + residential 两实例，各自独立失败计数）
    cat > "$watchdog_script" << WDOGEOF
#!/bin/bash
# Hysteria2 半死自愈：进程在但 UDP 端口失活时强制重启（direct + residential 双实例）
# 每 5 min 触发；某实例连续 3 次（15min）UDP 监听失活 → systemctl restart 该实例
LOG=/var/log/b-ui-hy2-watchdog.log
log() { echo "[\$(date '+%Y-%m-%d %H:%M:%S')] \$1" >> "\$LOG"; }

# check_instance <service> <udp_port>
check_instance() {
    svc="\$1"; port="\$2"
    fail_file="/tmp/hy2-watchdog-fail-\${svc}"
    # 1. 进程不在 → 交给 systemd Restart=always，并重置计数
    if ! systemctl is-active --quiet "\$svc"; then
        rm -f "\$fail_file"; return 0
    fi
    # 2. UDP 端口失活 → 累计失败，连续 3 次重启本实例
    if ! ss -lun "( sport = :\${port} )" 2>/dev/null | grep -q ":\${port}"; then
        fail=\$(( \$(cat "\$fail_file" 2>/dev/null || echo 0) + 1 ))
        echo "\$fail" > "\$fail_file"
        log "WARN: \${svc} UDP :\${port} 监听失活，失败计数 \${fail}/3"
        if [ "\$fail" -ge 3 ]; then
            log "ACTION: \${svc} 连续 3 次失败，重启"
            systemctl restart "\$svc"
            rm -f "\$fail_file"
        fi
        return 0
    fi
    # 监听正常，重置失败计数
    rm -f "\$fail_file"
}

check_instance hysteria-server ${listen_port}
systemctl list-unit-files hysteria-residential.service >/dev/null 2>&1 && check_instance hysteria-residential 40000

# 保留最近 200 行日志
tail -200 "\$LOG" > "\${LOG}.tmp" 2>/dev/null && mv "\${LOG}.tmp" "\$LOG" 2>/dev/null
exit 0
WDOGEOF
    chmod 755 "$watchdog_script"

    # systemd service（oneshot）
    cat > /etc/systemd/system/hy2-watchdog.service << EOF
[Unit]
Description=Hysteria2 Watchdog (semi-dead self-heal)
After=hysteria-server.service

[Service]
Type=oneshot
ExecStart=${watchdog_script}
EOF

    # systemd timer（每 5 min）
    cat > /etc/systemd/system/hy2-watchdog.timer << 'EOF'
[Unit]
Description=Hysteria2 Watchdog Timer

[Timer]
OnBootSec=2min
OnUnitActiveSec=5min
AccuracySec=30s
Persistent=true

[Install]
WantedBy=timers.target
EOF

    systemctl daemon-reload
    systemctl enable hy2-watchdog.timer 2>/dev/null
    systemctl start hy2-watchdog.timer 2>/dev/null

    print_success "Hysteria2 watchdog 已启用（每 5min 检测，连续 3 次失败重启）"
}

# 等待 Caddy 证书就绪并同步
wait_and_sync_certs() {
    print_info "等待 Caddy 申请 SSL 证书..."
    
    local max_wait=60
    local waited=0
    local caddy_data="/var/lib/caddy/.local/share/caddy"
    
    while [[ $waited -lt $max_wait ]]; do
        # 查找域名证书目录
        local cert_dir=$(find "$caddy_data/certificates" -type d -name "$DOMAIN" 2>/dev/null | head -1)
        if [[ -n "$cert_dir" ]] && [[ -f "${cert_dir}/${DOMAIN}.crt" ]]; then
            print_success "Caddy 证书已就绪"
            # 执行同步
            bash "${BASE_DIR}/cert-sync.sh" 2>/dev/null
            if [[ -f "${CERTS_DIR}/fullchain.pem" ]]; then
                print_success "证书已同步到 ${CERTS_DIR}/"
                return 0
            fi
        fi
        sleep 2
        ((waited+=2))
        echo -ne "\r  等待中... ${waited}/${max_wait}s"
    done
    
    echo ""
    print_warning "证书等待超时 (${max_wait}s)，Caddy 可能仍在申请中"
    print_info "Hysteria2 将在证书就绪后通过定时任务自动同步"
    return 1
}

#===============================================================================
# 配置定时任务 (证书续期 + b-ui 自动更新 + 证书健康检查)
#===============================================================================

configure_cron_tasks() {
    print_info "配置定时任务..."
    
    # 清除旧的 b-ui 相关 cron 任务
    crontab -l 2>/dev/null | grep -v "certbot renew" | grep -v "b-ui" | grep -v "cert-check" | crontab - 2>/dev/null || true
    
    # 创建 Caddy + 证书健康检查脚本
    cat > "${BASE_DIR}/cert-check.sh" << 'CERTEOF'
#!/bin/bash
# B-UI 健康检查 (Caddy 服务 + 证书同步)
LOG="/var/log/b-ui-cert-check.log"

log() {
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] $1" >> "$LOG"
}

# 1. 检查 Caddy 服务状态
if systemctl is-active --quiet caddy 2>/dev/null; then
    log "OK: Caddy 运行正常"
else
    log "WARNING: Caddy 未运行，尝试重启..."
    systemctl restart caddy >> "$LOG" 2>&1
    if systemctl is-active --quiet caddy 2>/dev/null; then
        log "SUCCESS: Caddy 重启成功"
    else
        log "CRITICAL: Caddy 重启失败！"
    fi
fi

# 2. 同步证书 (从 Caddy 到 Hysteria2)
if [[ -x "/opt/b-ui/cert-sync.sh" ]]; then
    /opt/b-ui/cert-sync.sh
fi

# 3. 检查 Hysteria2 两实例（direct + residential），证书已就绪但未运行则拉起
if [[ -f "/opt/b-ui/certs/fullchain.pem" ]]; then
    for u in hysteria-server hysteria-residential; do
        systemctl list-unit-files "${u}.service" >/dev/null 2>&1 || continue
        if ! systemctl is-active --quiet "$u" 2>/dev/null; then
            log "WARNING: $u 未运行但证书存在，尝试启动..."
            systemctl start "$u" 2>/dev/null
            sleep 2
            if systemctl is-active --quiet "$u" 2>/dev/null; then
                log "SUCCESS: $u 启动成功"
            else
                log "ERROR: $u 启动失败"
            fi
        fi
    done
fi

# 保留最近 200 行日志
tail -200 "$LOG" > "${LOG}.tmp" && mv "${LOG}.tmp" "$LOG" 2>/dev/null
CERTEOF
    chmod +x "${BASE_DIR}/cert-check.sh"
    
    # 添加 cron 任务
    (
        crontab -l 2>/dev/null
        echo '# === B-UI 定时任务 ==='
        echo '0 */6 * * * /opt/b-ui/update.sh auto >> /var/log/b-ui-update.log 2>&1'
        echo '30 */12 * * * /opt/b-ui/update.sh kernel >> /var/log/b-ui-kernel-update.log 2>&1'
        echo '0 */12 * * * /opt/b-ui/cert-check.sh >> /var/log/b-ui-cert-check.log 2>&1'
    ) | crontab -
    
    print_success "定时任务已配置:"
    echo -e "  ${CYAN}• B-UI 自动更新: 每 6 小时检查并静默更新${NC}"
    echo -e "  ${CYAN}• 健康检查: 每 12 小时 (Caddy + 证书同步 + Hysteria2)${NC}"
}

#===============================================================================
# 配置防火墙
#===============================================================================

configure_firewall() {
    local port=${PORT:-10000}
    local start_port=${PORT_HOPPING_START:-20000}
    local end_port=${PORT_HOPPING_END:-30000}
    # v3.5: 住宅 hy2 实例端口
    local resi_port=40000
    local resi_hop_start=41000
    local resi_hop_end=50000
    # v3.5: xray 双 inbound 端口
    local xray_direct=10001
    local xray_resi=10002

    print_info "配置防火墙..."

    if command -v ufw &> /dev/null && ufw status | grep -q "active"; then
        ufw allow 22/tcp
        ufw allow ${port}/udp
        ufw allow ${port}/tcp
        ufw allow 80/tcp
        ufw allow 443/tcp
        ufw allow ${xray_direct}/tcp
        ufw allow ${xray_resi}/tcp                  # v3.5 vless-residential
        ufw allow ${resi_port}/udp                  # v3.5 hy2-residential
        if [[ "$PORT_HOPPING_ENABLED" =~ ^[yY]$ ]]; then
            ufw allow ${start_port}:${end_port}/udp
            print_success "ufw 端口跳跃范围 ${start_port}:${end_port}/udp 已开放"
        fi
        ufw allow ${resi_hop_start}:${resi_hop_end}/udp   # v3.5 hy2-residential hop
        print_success "ufw 规则已添加（含 v3.5 住宅实例端口 ${resi_port}+${resi_hop_start}-${resi_hop_end}）"
    elif command -v firewall-cmd &> /dev/null && systemctl is-active --quiet firewalld; then
        firewall-cmd --permanent --add-port=22/tcp
        firewall-cmd --permanent --add-port=${port}/udp
        firewall-cmd --permanent --add-port=${port}/tcp
        firewall-cmd --permanent --add-port=80/tcp
        firewall-cmd --permanent --add-port=443/tcp
        firewall-cmd --permanent --add-port=${xray_direct}/tcp
        firewall-cmd --permanent --add-port=${xray_resi}/tcp
        firewall-cmd --permanent --add-port=${resi_port}/udp
        if [[ "$PORT_HOPPING_ENABLED" =~ ^[yY]$ ]]; then
            firewall-cmd --permanent --add-port=${start_port}-${end_port}/udp
            print_success "firewalld 端口跳跃范围 ${start_port}-${end_port}/udp 已开放"
        fi
        firewall-cmd --permanent --add-port=${resi_hop_start}-${resi_hop_end}/udp
        firewall-cmd --reload
        print_success "firewalld 规则已添加（含 v3.5 住宅实例端口）"
    fi
}

#===============================================================================
# v3.5: 静态 DNS — 锁 /etc/resolv.conf 防 systemd-resolved 改 + 防 GFW 投毒兜底
#===============================================================================

setup_static_dns() {
    print_info "配置静态 DNS（绕 systemd-resolved，DoH 防投毒在 hy2/xray 自己的 resolver）..."

    # 关停 systemd-resolved（实测会挂 + 它管的 /etc/resolv.conf 是 stub 127.0.0.53）
    if systemctl is-active --quiet systemd-resolved 2>/dev/null; then
        systemctl disable --now systemd-resolved 2>/dev/null || true
        print_info "  ✓ 已停 systemd-resolved"
    fi

    # 若 /etc/resolv.conf 是 systemd-resolved 的 symlink，删了重写
    if [[ -L /etc/resolv.conf ]]; then
        rm -f /etc/resolv.conf
    fi

    # 解锁老的 immutable 标志（如果之前锁过）
    chattr -i /etc/resolv.conf 2>/dev/null || true

    cat > /etc/resolv.conf <<'RESOLV_EOF'
# B-UI v3.5: 静态 DNS（防 systemd-resolved 挂死 + 防 GFW UDP 投毒兜底）
# hy2/xray/sing-box 自己用 DoH 解析，这里给 cron/apt/curl 等次要进程用
nameserver 1.1.1.1
nameserver 8.8.8.8
nameserver 9.9.9.9
options edns0 timeout:2 attempts:2 single-request
RESOLV_EOF

    # immutable 锁住，防止任何进程修改（NetworkManager / dhclient 都拦下）
    chattr +i /etc/resolv.conf 2>/dev/null && \
        print_success "  ✓ /etc/resolv.conf 写入并 chattr +i 锁定（1.1.1.1 + 8.8.8.8 + 9.9.9.9）" || \
        print_warning "  ⚠ chattr +i 失败（文件系统可能不支持），DNS 仍配置但未锁定"
}

#===============================================================================
# 启用 BBR
#===============================================================================

enable_bbr() {
    print_info "配置 BBR 优化..."

    # 检测系统支持的最佳 BBR 算法（BBRv3 优先）
    local algo
    algo=$(detect_best_bbr_algo)

    # 如果当前已经是 bbr 系且系统支持的"最佳"也是同一个，直接 OK
    local current_cc
    current_cc=$(sysctl -n net.ipv4.tcp_congestion_control 2>/dev/null)
    if [[ -n "$current_cc" && "$current_cc" == "$algo" ]]; then
        print_success "BBR 已启用（${current_cc}）"
        return 0
    fi

    # 模块自动加载（部分内核 bbr3 内置无独立模块）
    modprobe tcp_bbr 2>/dev/null || true

    # 没探测到任何 bbr 算法 → 退回常规 bbr 写入（让 sysctl 在加载模块后生效）
    if [[ -z "$algo" ]]; then
        algo="bbr"
        print_warning "未检测到 BBR 算法（/proc/.../tcp_available_congestion_control），仍写入 bbr 兜底"
    fi

    cat > /etc/sysctl.d/99-hysteria-bbr.conf <<EOF
net.core.default_qdisc=fq
net.ipv4.tcp_congestion_control=${algo}
EOF

    sysctl --system > /dev/null 2>&1

    if check_bbr_status; then
        local now_cc
        now_cc=$(sysctl -n net.ipv4.tcp_congestion_control 2>/dev/null)
        print_success "BBR 启用成功（${now_cc}）"
    else
        print_warning "BBR 配置完成（${algo}），可能需要重启生效"
    fi
}

#===============================================================================
# 端口跳跃配置 (Port Hopping)
# 使用 iptables DNAT 将端口范围转发到 Hysteria 监听端口
#===============================================================================

PORT_HOPPING_ENABLED="y"
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

    print_info "配置端口跳跃 (${start_port}-${end_port}) ..."

    # hysteria 2.9+ 使用 config.yaml 内置 listen 多端口语法（如 :10000,20000-30000）
    # hysteria 自动管理 nftables/iptables，shutdown 时自动清理；不再需要手动写 REDIRECT
    # 这里只负责放行防火墙端口范围 + 保存状态

    # 清理 b-ui 历史遗留的 iptables REDIRECT 规则（如果有），让 hysteria 接管
    if command -v iptables &>/dev/null && iptables -t nat -L PREROUTING -n 2>/dev/null | grep -q "Hysteria2-PortHopping"; then
        print_info "清理旧的 iptables REDIRECT 规则（hysteria 内置已接管）"
        # 安全清理：保留其他规则，只删带 Hysteria2-PortHopping 注释的
        iptables-save 2>/dev/null | grep -v "Hysteria2-PortHopping" | iptables-restore 2>/dev/null || true
        if command -v ip6tables-save &>/dev/null; then
            ip6tables-save 2>/dev/null | grep -v "Hysteria2-PortHopping" | ip6tables-restore 2>/dev/null || true
        fi
        # 持久化清理后的 iptables 规则
        if [[ -d /etc/iptables ]]; then
            iptables-save > /etc/iptables/rules.v4 2>/dev/null || true
            ip6tables-save > /etc/iptables/rules.v6 2>/dev/null || true
        fi
    fi

    # 放行 UFW（如果启用）
    if command -v ufw &>/dev/null && ufw status 2>/dev/null | grep -q "active"; then
        ufw allow ${start_port}:${end_port}/udp comment "Hysteria2 端口跳跃" 2>/dev/null || true
        print_success "  ✓ UFW 已放行 udp ${start_port}:${end_port} 端口跳跃范围"
    fi

    # 放行 firewalld（如果启用）
    if command -v firewall-cmd &>/dev/null && systemctl is-active --quiet firewalld 2>/dev/null; then
        firewall-cmd --permanent --add-port=${start_port}-${end_port}/udp 2>/dev/null || true
        firewall-cmd --reload 2>/dev/null || true
        print_success "  ✓ firewalld 已放行 udp ${start_port}-${end_port}"
    fi

    # 保存端口跳跃状态（供 web/server.js 生成 mport 链接、update.sh 迁移用）
    local iface=$(ip route get 8.8.8.8 2>/dev/null | grep -oP 'dev \K\S+' | head -1)
    [[ -z "$iface" ]] && iface="eth0"
    cat > "${BASE_DIR}/port-hopping.json" << EOF
{
    "enabled": true,
    "startPort": ${start_port},
    "endPort": ${end_port},
    "listenPort": ${listen_port},
    "xrayPort": ${xray_port},
    "interface": "${iface}",
    "implementation": "hysteria-builtin-listen",
    "note": "hysteria 2.9+ 内置 listen 多端口语法，已不再使用 iptables REDIRECT"
}
EOF

    print_success "端口跳跃配置完成（hysteria 内置 listen 多端口）"
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

    # 3. 网络栈调优（v3.4.19，与 99-hysteria-perf.conf 分离）
    # 关键点：减少跨境抖动 / GFW PMTU 黑洞 / 僵尸 TCP 卡死感
    cat > /etc/sysctl.d/99-b-ui-network.conf <<'EOF'
# B-UI 网络栈调优 v3.4.19
# 减少 stuck 卡死感（影响用户感受的"断连"）
net.ipv4.tcp_retries2=8
net.ipv4.tcp_mtu_probing=1
net.ipv4.tcp_keepalive_time=600
net.ipv4.tcp_keepalive_intvl=30
net.ipv4.tcp_keepalive_probes=3
net.ipv4.tcp_no_metrics_save=1
net.ipv4.tcp_slow_start_after_idle=0
net.ipv4.tcp_notsent_lowat=131072
# TCP 缓冲池上调到 16 MiB 配 BDP（200ms RTT × 100Mbps = 2.5MB BDP）
net.ipv4.tcp_rmem=4096 262144 16777216
net.ipv4.tcp_wmem=4096 65536 16777216
# UDP 全局内存上限提升（多用户 hy2 突发场景兜底）
net.ipv4.udp_mem=262144 524288 1048576
# 临时端口范围扩大（多用户 + Xray 出向连接复用）
net.ipv4.ip_local_port_range=10000 65535
# 关键：把所有代理监听端口 + 端口跳跃段从临时端口池里排除，
# 否则出向连接（Xray freedom / 住宅 relay / ACME / apt）可能抢占跳跃段端口
# （20000-30000 / 41000-50000）→ 间歇性 "address already in use" UDP 出向失败，极难排查。
net.ipv4.ip_local_reserved_ports=10000-10002,20000-30000,40000,41000-50000
# 队列层
net.core.netdev_max_backlog=5000
net.ipv4.tcp_max_syn_backlog=8192
EOF
    sysctl --system > /dev/null 2>&1
    print_success "网络栈调优已应用 (99-b-ui-network.conf)"

    # 4. conntrack 容量：默认 8192 是多用户真正的并发天花板
    # hy2 端口跳跃让每个客户端横跨多个 UDP 5-tuple，每条 + 其 DNAT 回包都占一条 conntrack；
    # 表满时内核 "table full, dropping packet" 会对所有用户（含 TCP Reality/SSH）静默丢新连接。
    local mem_mb
    mem_mb=$(awk '/^MemTotal:/ {print int($2/1024)}' /proc/meminfo)
    local ct_max=131072
    if   [[ $mem_mb -gt 4096 ]]; then ct_max=524288
    elif [[ $mem_mb -gt 2048 ]]; then ct_max=262144
    fi
    cat > /etc/sysctl.d/99-b-ui-conntrack.conf <<EOF
# B-UI conntrack 容量（多用户 hy2 端口跳跃 + xray + 住宅 relay 共享一张表）
net.netfilter.nf_conntrack_max=${ct_max}
# UDP 流（hy2 QUIC + 端口跳跃）回收更快，避免短命 5-tuple 堆积撑爆表
net.netfilter.nf_conntrack_udp_timeout=20
net.netfilter.nf_conntrack_udp_timeout_stream=60
EOF
    # 哈希桶大小须在模块加载时设定（重启后生效）；并确保开机先加载模块，sysctl.d 才能写入 max
    echo "options nf_conntrack hashsize=$((ct_max / 4))" > /etc/modprobe.d/b-ui-nf_conntrack.conf
    echo "nf_conntrack" > /etc/modules-load.d/b-ui-conntrack.conf
    modprobe nf_conntrack 2>/dev/null || true
    sysctl --system > /dev/null 2>&1
    print_success "conntrack 容量已上调 (nf_conntrack_max=${ct_max}, 物理内存 ${mem_mb}MB)"

    # 5. BBR 拥塞控制（enable_bbr 此前是 dead code，BBR 仅靠发行版镜像运气；显式启用兜底）
    if declare -F enable_bbr >/dev/null; then
        enable_bbr
    fi
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
{"name":"b-ui-admin","version":"${pkg_version}","type":"module","main":"server.js","scripts":{"start":"node server.js"},"dependencies":{"singbox-converter":"^1.0.8","js-yaml":"^4.1.0"}}
EOF
    
    # 安装 npm 依赖 (多种方式尝试，确保成功)
    print_info "安装 Web 面板依赖..."
    cd "$ADMIN_DIR"
    
    local npm_success=false

    # 方法1: 标准安装
    if npm install 2>&1; then
        npm_success=true
    fi

    # 方法2: 使用 --legacy-peer-deps
    if [[ "$npm_success" == "false" ]]; then
        print_warning "尝试 --legacy-peer-deps..."
        if npm install --legacy-peer-deps 2>&1; then
            npm_success=true
        fi
    fi

    # 方法3: 单独安装核心依赖
    if [[ "$npm_success" == "false" ]] || [[ ! -d "$ADMIN_DIR/node_modules/singbox-converter" ]]; then
        print_warning "单独安装 singbox-converter..."
        npm install singbox-converter js-yaml 2>&1 || true
    fi
    
    cd - > /dev/null 2>&1 || true
    
    # 验证安装
    if [[ -d "$ADMIN_DIR/node_modules/singbox-converter" ]]; then
        print_success "依赖安装完成"
    else
        print_error "依赖安装失败，请手动运行: cd $ADMIN_DIR && npm install"
    fi
    
    print_success "Web 面板部署完成"
}

#===============================================================================
# 创建服务
#===============================================================================

create_services() {
    print_info "创建系统服务..."

    # 写入 admin.env (chmod 600) — 把密码/敏感配置移出 unit 文件（unit 全局可读）
    # bind 默认 127.0.0.1，强制 Caddy 反代，禁止 8080 直访
    cat > "${BASE_DIR}/admin.env" <<EOF
ADMIN_PORT=${ADMIN_PORT:-8080}
ADMIN_BIND=127.0.0.1
ADMIN_PASSWORD=${ADMIN_PASSWORD}
HYSTERIA_CONFIG=${CONFIG_FILE}
USERS_FILE=${USERS_FILE}
XRAY_CONFIG=${BASE_DIR}/xray-config.json
XRAY_KEYS=${BASE_DIR}/reality-keys.json
EOF
    chmod 600 "${BASE_DIR}/admin.env"

    # 创建管理面板服务（使用 EnvironmentFile 取代 Environment=）
    cat > /etc/systemd/system/b-ui-admin.service << EOF
[Unit]
Description=B-UI Admin Panel
After=network.target

[Service]
Type=simple
EnvironmentFile=-${BASE_DIR}/admin.env
WorkingDirectory=${ADMIN_DIR}
ExecStart=/usr/bin/node server.js
Restart=always
RestartSec=3

# 内存兜底：面板不在数据通道上，但 Node 累计流量统计/跑 singbox-converter 可能涨 RSS；
# 在小内存机器上限制它，避免它把内核 OOM-killer 引到 hysteria/xray 头上（那会触发孤儿链崩溃循环）。
MemoryHigh=150M
MemoryMax=200M

[Install]
WantedBy=multi-user.target
EOF

    # 创建 Xray 服务覆盖
    # 重命名为 99-b-ui-override.conf 确保字典序最大，覆盖发行版/upstream drop-in
    mkdir -p /etc/systemd/system/xray.service.d
    # 清理可能残留的旧文件名
    rm -f /etc/systemd/system/xray.service.d/override.conf
    cat > /etc/systemd/system/xray.service.d/99-b-ui-override.conf << EOF
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
    mkdir -p "$CERTS_DIR"
    chmod 755 "$CERTS_DIR"
    
    # 1. 先启动 Caddy (需要它申请 SSL 证书)
    systemctl enable caddy --now 2>/dev/null
    sleep 1
    if systemctl is-active --quiet caddy; then
        print_success "Caddy 已启动"
    else
        print_warning "Caddy 启动失败，请检查 80/443 端口"
        print_info "查看详情: journalctl -xeu caddy.service"
    fi
    
    # 2. 等待 Caddy 证书就绪并同步到共享目录
    # `|| true`：超时返回 1 时不能让 set -e 中断后续安装（v3.5.13 botched-install 根因）
    wait_and_sync_certs || true

    # 3. 启动管理面板 (不依赖证书)
    # 所有 enable --now 都加 `|| true`：服务起不来只告警，绝不让 set -e 中断整个安装
    systemctl enable b-ui-admin --now || true
    if systemctl is-active --quiet b-ui-admin; then
        print_success "管理面板已启动"
    else
        print_warning "管理面板启动失败"
    fi

    # 4. 启动 Xray (不依赖证书)
    systemctl enable xray --now || true
    if systemctl is-active --quiet xray; then
        print_success "Xray 服务已启动"
    else
        print_warning "Xray 服务启动失败"
    fi

    # 5. 最后启动 Hysteria2 direct + residential (依赖证书，两实例共用同一份 LE 证书)
    if [[ -f "${CERTS_DIR}/fullchain.pem" ]]; then
        for u in hysteria-server hysteria-residential; do
            systemctl enable "$u" --now || true
            sleep 1
            if systemctl is-active --quiet "$u"; then
                print_success "${u} 已启动"
            else
                print_warning "${u} 启动失败"
                journalctl -u "$u" --no-pager -n 5 2>/dev/null || true
            fi
        done
    else
        # 证书尚未就绪：两实例都设开机自启但不立即启动（cert-sync / cert-check.sh 就绪后拉起）
        systemctl enable hysteria-server hysteria-residential 2>/dev/null || true
        print_warning "Hysteria2: 证书尚未就绪，direct + residential 已设置开机自启"
        print_info "证书同步后将通过健康检查自动启动"
    fi

    # 6. 端口跳跃防火墙放行 + 状态持久化（hysteria 内置 listen 多端口接管 iptables）
    configure_port_hopping

    # 7. 启用 Hysteria2 watchdog（半死自愈，5 min 检测）
    setup_hy2_watchdog
}

#===============================================================================
# SSH 安全加固
# 检测 root 用户是否已配置 SSH 公钥，如果有则关闭密码登录
#===============================================================================

harden_ssh() {
    local auth_keys="/root/.ssh/authorized_keys"

    # pubkey 检测：必须有非注释的 ssh-(rsa|ed25519|dss) 或 ecdsa-sha2- 行
    local pubkey_count=0
    if [[ -f "$auth_keys" ]] && [[ -s "$auth_keys" ]]; then
        pubkey_count=$(grep -cE '^[^#]*\s*(ssh-(rsa|ed25519|dss)|ecdsa-sha2-)' "$auth_keys" 2>/dev/null || echo 0)
    fi
    # grep -c 在某些环境下可能返回多行，强制成单数
    pubkey_count=${pubkey_count//[^0-9]/}
    [[ -z "$pubkey_count" ]] && pubkey_count=0

    if [[ $pubkey_count -lt 1 ]]; then
        print_warning "未检测到 SSH 公钥（/root/.ssh/authorized_keys），跳过 SSH 加固"
        print_warning "如需启用密码登录禁用，请先添加公钥后运行: b-ui harden-ssh"
        mkdir -p /opt/b-ui
        touch /opt/b-ui/.ssh-not-hardened
        # 顺带打印 apt-daily-upgrade 提示（C5）
        print_info "如需启用系统自动安全更新，运行: b-ui harden-system"
        return 0
    fi

    # 确保 sshd_config.d 存在并 Include（绝大多数发行版默认已 Include /etc/ssh/sshd_config.d/*.conf）
    mkdir -p /etc/ssh/sshd_config.d

    local target_conf=/etc/ssh/sshd_config.d/99-b-ui-hardening.conf
    local new_content
    new_content=$(cat <<'EOF'
# B-UI SSH 加固（覆盖 50-cloud-init.conf）
PasswordAuthentication no
PermitRootLogin prohibit-password
KbdInteractiveAuthentication no
ChallengeResponseAuthentication no
EOF
)

    # 幂等：内容相同则跳过写入（避免 mtime 改变和无谓的 sshd reload）
    if [[ -f "$target_conf" ]] && [[ "$(cat "$target_conf")" == "$new_content" ]]; then
        print_info "  ✓ SSH 加固已是最新（幂等跳过，pubkey 数量: ${pubkey_count}）"
        rm -f /opt/b-ui/.ssh-not-hardened
        # 顺带打印 apt-daily-upgrade 提示（C5）
        print_info "如需启用系统自动安全更新，运行: b-ui harden-system"
        return 0
    fi

    print_info "检测到 ${pubkey_count} 个 SSH 公钥，写入 99-b-ui-hardening.conf 覆盖 cloud-init 默认..."

    # 写 99-... 后缀确保字典序最大，覆盖 50-cloud-init.conf
    echo "$new_content" > "$target_conf"
    chmod 644 "$target_conf"

    # 校验 sshd 配置
    if sshd -t 2>/dev/null; then
        systemctl reload sshd 2>/dev/null || systemctl reload ssh 2>/dev/null || \
            systemctl restart sshd 2>/dev/null || systemctl restart ssh 2>/dev/null
        print_success "  ✓ SSH 加固已启用（密码登录已禁用，pubkey 数量: ${pubkey_count}）"
        rm -f /opt/b-ui/.ssh-not-hardened
    else
        rm -f "$target_conf"
        print_error "sshd 配置校验失败 (sshd -t)，已回滚"
    fi

    # 顺带打印 apt-daily-upgrade 提示（C5）
    print_info "如需启用系统自动安全更新，运行: b-ui harden-system"
}

#===============================================================================
# 系统卫生：timesyncd / fail2ban / journald 限额（v3.4.19 Cluster G）
#===============================================================================

configure_system_hygiene() {
    print_info "配置系统卫生（时钟同步 / 暴力扫防护 / 日志限额）..."

    # G1. 时钟同步：systemd-timesyncd（缺失时安装）
    # 时钟错乱会导致 TLS/ACME 失败、客户端订阅时间戳异常
    if ! systemctl is-active --quiet systemd-timesyncd 2>/dev/null && \
       ! systemctl is-active --quiet chrony 2>/dev/null && \
       ! systemctl is-active --quiet ntp 2>/dev/null; then
        if command -v apt-get &>/dev/null; then
            apt-get install -y systemd-timesyncd 2>/dev/null || true
        fi
        systemctl enable --now systemd-timesyncd 2>/dev/null || true
        if systemctl is-active --quiet systemd-timesyncd 2>/dev/null; then
            print_success "  ✓ 启用 systemd-timesyncd（时钟同步影响 TLS/ACME）"
        else
            print_warning "  systemd-timesyncd 启用失败，请检查时间同步"
        fi
    else
        print_info "  时钟同步已就绪（timesyncd/chrony/ntp 至少一项在跑）"
    fi

    # G2. fail2ban 自动安装（SSH 暴力扫日志噪音）
    if ! command -v fail2ban-client &>/dev/null && command -v apt-get &>/dev/null; then
        apt-get install -y fail2ban 2>/dev/null || true
    fi
    if command -v fail2ban-client &>/dev/null; then
        mkdir -p /etc/fail2ban/jail.d
        if [[ ! -f /etc/fail2ban/jail.d/b-ui-sshd.local ]]; then
            cat > /etc/fail2ban/jail.d/b-ui-sshd.local <<'EOF'
[sshd]
enabled = true
port = ssh
maxretry = 5
bantime = 1h
findtime = 10m
EOF
        fi
        systemctl enable --now fail2ban 2>/dev/null || true
        if systemctl is-active --quiet fail2ban 2>/dev/null; then
            print_success "  ✓ fail2ban 已启用（SSH 暴力扫自动封禁 1h）"
        fi
    fi

    # G3. journald 限额（避免日志膨胀填满磁盘）
    mkdir -p /etc/systemd/journald.conf.d
    cat > /etc/systemd/journald.conf.d/99-b-ui.conf <<'EOF'
[Journal]
SystemMaxUse=500M
SystemKeepFree=1G
MaxRetentionSec=14day
Compress=yes
EOF
    systemctl restart systemd-journald 2>/dev/null || true
    print_success "  ✓ journald 限额已应用 (500M/14day)"
}
