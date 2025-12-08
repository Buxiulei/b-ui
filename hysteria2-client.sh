#!/bin/bash

#===============================================================================
# Hysteria2 客户端一键安装脚本 (Ubuntu/Debian)
# 功能：安装客户端、配置连接、启动 SOCKS5/HTTP 代理
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
CLIENT_SERVICE="hysteria-client.service"

# 代理配置
SOCKS_PORT="1080"
HTTP_PORT="8080"

#===============================================================================
# 工具函数
#===============================================================================

print_banner() {
    clear
    echo -e "${CYAN}"
    echo "╔══════════════════════════════════════════════════════════════╗"
    echo "║                                                              ║"
    echo "║            Hysteria2 客户端一键安装脚本                      ║"
    echo "║                                                              ║"
    echo "║            支持：SOCKS5 / HTTP 代理                          ║"
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
        print_error "此脚本需要 root 权限运行"
        print_info "请使用 sudo $0 运行"
        exit 1
    fi
}

check_os() {
    if [[ -f /etc/os-release ]]; then
        . /etc/os-release
        OS=$ID
        print_info "检测到操作系统: $OS"
    else
        print_error "无法识别操作系统"
        exit 1
    fi
    
    if ! command -v systemctl &> /dev/null; then
        print_error "此系统不支持 systemd"
        exit 1
    fi
}

#===============================================================================
# Hysteria2 安装
#===============================================================================

install_hysteria() {
    print_info "安装 Hysteria2 客户端..."
    
    if command -v hysteria &> /dev/null; then
        print_success "Hysteria2 已安装: $(hysteria version 2>/dev/null | head -n1)"
        return 0
    fi
    
    HYSTERIA_USER=root bash <(curl -fsSL https://get.hy2.sh/)
    
    if command -v hysteria &> /dev/null; then
        print_success "Hysteria2 安装成功"
    else
        print_error "安装失败"
        exit 1
    fi
}

#===============================================================================
# 配置客户端
#===============================================================================

configure_client() {
    print_info "配置 Hysteria2 客户端..."
    echo ""
    
    # 服务器地址
    read -p "请输入服务器地址 (域名:端口，如 hy2.example.com:443): " SERVER_ADDR
    while [[ -z "$SERVER_ADDR" ]]; do
        print_error "服务器地址不能为空"
        read -p "请输入服务器地址: " SERVER_ADDR
    done
    
    # 认证密码
    read -p "请输入认证密码: " AUTH_PASSWORD
    while [[ -z "$AUTH_PASSWORD" ]]; do
        print_error "密码不能为空"
        read -p "请输入认证密码: " AUTH_PASSWORD
    done
    
    # SOCKS5 端口
    read -p "请输入 SOCKS5 代理端口 [默认: 1080]: " SOCKS_PORT
    SOCKS_PORT=${SOCKS_PORT:-1080}
    
    # HTTP 端口
    read -p "请输入 HTTP 代理端口 [默认: 8080]: " HTTP_PORT
    HTTP_PORT=${HTTP_PORT:-8080}
    
    # 创建目录
    mkdir -p "$BASE_DIR"
    
    # 生成配置文件
    cat > "$CONFIG_FILE" << EOF
# Hysteria2 客户端配置
# 生成时间: $(date)

server: ${SERVER_ADDR}

auth: ${AUTH_PASSWORD}

# TLS 配置
tls:
  insecure: false

# 本地代理
socks5:
  listen: 127.0.0.1:${SOCKS_PORT}

http:
  listen: 127.0.0.1:${HTTP_PORT}
EOF

    print_success "配置文件已生成: $CONFIG_FILE"
    
    # 显示配置摘要
    echo ""
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}配置摘要：${NC}"
    echo -e "  服务器:     ${YELLOW}${SERVER_ADDR}${NC}"
    echo -e "  SOCKS5:     ${YELLOW}127.0.0.1:${SOCKS_PORT}${NC}"
    echo -e "  HTTP:       ${YELLOW}127.0.0.1:${HTTP_PORT}${NC}"
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
}

#===============================================================================
# 服务管理
#===============================================================================

create_service() {
    print_info "创建 systemd 服务..."
    
    cat > "/etc/systemd/system/$CLIENT_SERVICE" << EOF
[Unit]
Description=Hysteria2 Client
After=network.target

[Service]
Type=simple
ExecStart=/usr/local/bin/hysteria client --config ${CONFIG_FILE}
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF

    systemctl daemon-reload
    print_success "服务已创建"
}

start_client() {
    print_info "启动 Hysteria2 客户端..."
    systemctl start "$CLIENT_SERVICE"
    sleep 2
    
    if systemctl is-active --quiet "$CLIENT_SERVICE"; then
        print_success "客户端已启动"
    else
        print_error "启动失败"
        journalctl -u "$CLIENT_SERVICE" --no-pager -n 10
    fi
}

stop_client() {
    print_info "停止客户端..."
    systemctl stop "$CLIENT_SERVICE" 2>/dev/null || true
    print_success "已停止"
}

show_status() {
    echo ""
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}客户端状态${NC}"
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    
    if systemctl is-active --quiet "$CLIENT_SERVICE"; then
        echo -e "  状态: ${GREEN}运行中${NC}"
    else
        echo -e "  状态: ${RED}未运行${NC}"
    fi
    
    if [[ -f "$CONFIG_FILE" ]]; then
        local server=$(grep "^server:" "$CONFIG_FILE" 2>/dev/null | awk '{print $2}')
        local socks=$(grep -A1 "^socks5:" "$CONFIG_FILE" 2>/dev/null | grep "listen:" | awk '{print $2}')
        local http=$(grep -A1 "^http:" "$CONFIG_FILE" 2>/dev/null | grep "listen:" | awk '{print $2}')
        echo -e "  服务器: ${YELLOW}${server:-未配置}${NC}"
        echo -e "  SOCKS5: ${YELLOW}${socks:-未配置}${NC}"
        echo -e "  HTTP:   ${YELLOW}${http:-未配置}${NC}"
    else
        echo -e "  ${YELLOW}未配置${NC}"
    fi
    
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
}

test_proxy() {
    print_info "测试代理连接..."
    
    local socks_port=$(grep -A1 "^socks5:" "$CONFIG_FILE" 2>/dev/null | grep "listen:" | sed 's/.*://')
    
    if [[ -n "$socks_port" ]]; then
        if curl -s --max-time 10 --socks5 "127.0.0.1:${socks_port}" https://www.google.com > /dev/null 2>&1; then
            print_success "代理测试成功！可以访问 Google"
        else
            print_warning "代理测试失败，请检查配置"
        fi
    else
        print_error "未找到代理配置"
    fi
}

#===============================================================================
# 一键安装
#===============================================================================

quick_install() {
    print_info "开始一键安装..."
    echo ""
    
    install_hysteria
    echo ""
    configure_client
    echo ""
    create_service
    start_client
    
    echo ""
    echo -e "${GREEN}════════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}  安装完成！${NC}"
    echo -e "${GREEN}════════════════════════════════════════════════════════════════${NC}"
    echo ""
    echo -e "  SOCKS5 代理: ${YELLOW}127.0.0.1:${SOCKS_PORT}${NC}"
    echo -e "  HTTP 代理:   ${YELLOW}127.0.0.1:${HTTP_PORT}${NC}"
    echo ""
    echo -e "  使用示例:"
    echo -e "    curl --socks5 127.0.0.1:${SOCKS_PORT} https://www.google.com"
    echo -e "    export https_proxy=http://127.0.0.1:${HTTP_PORT}"
    echo ""
}

#===============================================================================
# 卸载
#===============================================================================

uninstall() {
    echo ""
    echo -e "${RED}警告: 即将卸载 Hysteria2 客户端${NC}"
    read -p "确定要卸载吗? 输入 'YES' 确认: " confirm
    
    if [[ "$confirm" == "YES" ]]; then
        systemctl stop "$CLIENT_SERVICE" 2>/dev/null || true
        systemctl disable "$CLIENT_SERVICE" 2>/dev/null || true
        rm -f "/etc/systemd/system/$CLIENT_SERVICE"
        systemctl daemon-reload
        rm -rf "$BASE_DIR"
        
        read -p "是否同时删除 Hysteria2 程序? (y/n): " del_bin
        if [[ "$del_bin" == "y" || "$del_bin" == "Y" ]]; then
            bash <(curl -fsSL https://get.hy2.sh/) --remove 2>/dev/null || rm -f /usr/local/bin/hysteria
        fi
        
        print_success "卸载完成"
    else
        print_info "已取消"
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
    echo -e "${CYAN}║${NC}  ${YELLOW}3.${NC} 启动客户端                                             ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}4.${NC} 停止客户端                                             ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}5.${NC} 重新配置                                               ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}6.${NC} 测试代理                                               ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}7.${NC} 查看日志                                               ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}8.${NC} ${RED}卸载${NC}                                                   ${CYAN}║${NC}"
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
        read -p "请选择 [0-8]: " choice
        
        case $choice in
            1) quick_install ;;
            2) show_status ;;
            3) 
                systemctl enable "$CLIENT_SERVICE" 2>/dev/null || true
                systemctl start "$CLIENT_SERVICE"
                print_success "客户端已启动"
                ;;
            4) stop_client ;;
            5) 
                configure_client
                systemctl restart "$CLIENT_SERVICE" 2>/dev/null || true
                ;;
            6) test_proxy ;;
            7) journalctl -u "$CLIENT_SERVICE" --no-pager -n 30 ;;
            8) uninstall ;;
            0) print_info "再见！"; exit 0 ;;
            *) print_error "无效选项" ;;
        esac
        
        echo ""
        read -p "按 Enter 继续..."
    done
}

main "$@"
