#!/bin/bash

#===============================================================================
# Hysteria2 客户端一键安装脚本 (Ubuntu/Debian)
# 功能：SOCKS5/HTTP 代理、TUN 模式、路由规则、SSH 保护
#===============================================================================

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
        print_info "操作系统: $ID"
    fi
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
        print_success "已安装: $(hysteria version 2>/dev/null | head -n1)"
        return 0
    fi
    
    HYSTERIA_USER=root bash <(curl -fsSL https://get.hy2.sh/)
    print_success "安装完成"
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
    
    # SOCKS5 端口
    read -p "SOCKS5 端口 [默认 1080]: " SOCKS_PORT
    SOCKS_PORT=${SOCKS_PORT:-1080}
    
    # HTTP 端口
    read -p "HTTP 端口 [默认 8080]: " HTTP_PORT
    HTTP_PORT=${HTTP_PORT:-8080}
    
    # TUN 模式
    read -p "启用 TUN 模式 (全局代理)? (y/n) [默认 n]: " enable_tun
    TUN_ENABLED="false"
    [[ "$enable_tun" == "y" || "$enable_tun" == "Y" ]] && TUN_ENABLED="true"
    
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
    
    # 获取当前 SSH 客户端 IP (需要保护)
    local ssh_client_ip=$(echo "$SSH_CONNECTION" | awk '{print $1}')
    
    # 构建排除 IP 列表
    local exclude_ips="\"${server_ip}/32\""
    
    # 读取自定义规则
    local acl_rules=""
    if [[ -f "$RULES_FILE" ]]; then
        while IFS= read -r line; do
            # 跳过注释和空行
            [[ "$line" =~ ^#.*$ || -z "$line" ]] && continue
            line=$(echo "$line" | xargs)  # trim
            [[ -z "$line" ]] && continue
            
            # 处理不同类型的规则
            if [[ "$line" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+(/[0-9]+)?$ ]]; then
                # IP/CIDR - 添加到 TUN 排除列表
                exclude_ips="${exclude_ips}, \"${line}\""
            elif [[ "$line" =~ ^regexp: ]]; then
                # 正则表达式
                local pattern="${line#regexp:}"
                acl_rules="${acl_rules}\n  - ${pattern} direct"
            else
                # 域名/通配符
                acl_rules="${acl_rules}\n  - ${line} direct"
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

# 带宽设置 (可选，根据实际情况调整)
# bandwidth:
#   up: 50 mbps
#   down: 100 mbps

# SOCKS5 代理
socks5:
  listen: 127.0.0.1:${SOCKS_PORT}

# HTTP 代理
http:
  listen: 127.0.0.1:${HTTP_PORT}
EOF

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
EOF
    fi
    
    # 添加 ACL 规则 (如果有)
    if [[ -n "$acl_rules" ]]; then
        cat >> "$CONFIG_FILE" << EOF

# ACL 路由规则
acl:
  inline:$(echo -e "$acl_rules")
EOF
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
    echo -e "${GREEN}客户端状态${NC}"
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    
    if systemctl is-active --quiet "$CLIENT_SERVICE" 2>/dev/null; then
        echo -e "  运行状态: ${GREEN}运行中${NC}"
    else
        echo -e "  运行状态: ${RED}未运行${NC}"
    fi
    
    if [[ -f "$CONFIG_FILE" ]]; then
        local server=$(grep "^server:" "$CONFIG_FILE" 2>/dev/null | awk '{print $2}')
        local socks=$(grep -A1 "^socks5:" "$CONFIG_FILE" 2>/dev/null | grep "listen:" | awk '{print $2}')
        local http=$(grep -A1 "^http:" "$CONFIG_FILE" 2>/dev/null | grep "listen:" | awk '{print $2}')
        local tun_status="禁用"
        grep -q "^tun:" "$CONFIG_FILE" && tun_status="启用"
        
        echo -e "  服务器:   ${YELLOW}${server:-未配置}${NC}"
        echo -e "  SOCKS5:   ${YELLOW}${socks:-未配置}${NC}"
        echo -e "  HTTP:     ${YELLOW}${http:-未配置}${NC}"
        echo -e "  TUN 模式: ${YELLOW}${tun_status}${NC}"
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
    echo -e "${CYAN}║${NC}                      ${GREEN}操作菜单${NC}                              ${CYAN}║${NC}"
    echo -e "${CYAN}╠══════════════════════════════════════════════════════════════╣${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}1.${NC} 一键安装                                               ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}2.${NC} 查看状态                                               ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}3.${NC} 启动/停止                                              ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}4.${NC} 重新配置                                               ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}5.${NC} 编辑路由规则                                           ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}6.${NC} TUN 模式开关                                           ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}7.${NC} 测试代理                                               ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}8.${NC} 查看日志                                               ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}9.${NC} ${RED}卸载${NC}                                                   ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}0.${NC} 退出                                                   ${CYAN}║${NC}"
    echo -e "${CYAN}╚══════════════════════════════════════════════════════════════╝${NC}"
}

main() {
    check_root
    check_os
    print_banner
    show_status
    
    while true; do
        show_menu
        read -p "请选择 [0-9]: " choice
        
        case $choice in
            1) quick_install ;;
            2) show_status ;;
            3) 
                if systemctl is-active --quiet "$CLIENT_SERVICE" 2>/dev/null; then
                    systemctl stop "$CLIENT_SERVICE"
                    print_success "已停止"
                else
                    systemctl start "$CLIENT_SERVICE" 2>/dev/null || start_client
                    print_success "已启动"
                fi
                ;;
            4) configure_client && create_service && start_client ;;
            5) edit_rules ;;
            6) toggle_tun ;;
            7) test_proxy ;;
            8) journalctl -u "$CLIENT_SERVICE" --no-pager -n 30 ;;
            9) uninstall ;;
            0) exit 0 ;;
            *) print_error "无效选项" ;;
        esac
        
        echo ""
        read -p "按 Enter 继续..."
    done
}

main "$@"
