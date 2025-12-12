#!/bin/bash

#===============================================================================
# B-UI CLI 终端管理工具
# 功能：服务管理、状态查看、更新检查
# 版本: 2.4.0
#===============================================================================

# 颜色定义
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m'

# 配置
BASE_DIR="/opt/b-ui"
CONFIG_FILE="${BASE_DIR}/config.yaml"
USERS_FILE="${BASE_DIR}/users.json"
ADMIN_DIR="${BASE_DIR}/admin"
HYSTERIA_SERVICE="hysteria-server.service"
ADMIN_SERVICE="b-ui-admin.service"

# 动态获取版本号
get_version() {
    if [[ -f "${BASE_DIR}/version.json" ]]; then
        jq -r '.version' "${BASE_DIR}/version.json" 2>/dev/null || echo "2.4.0"
    else
        echo "2.4.0"
    fi
}
SCRIPT_VERSION=$(get_version)

# 打印函数
print_info() { echo -e "${BLUE}[INFO]${NC} $1"; }
print_success() { echo -e "${GREEN}[SUCCESS]${NC} $1"; }
print_warning() { echo -e "${YELLOW}[WARNING]${NC} $1"; }
print_error() { echo -e "${RED}[ERROR]${NC} $1"; }

#===============================================================================
# Banner
#===============================================================================

print_banner() {
    clear
    echo -e "${CYAN}"
    echo "╔══════════════════════════════════════════════════════════════╗"
    echo "║                                                              ║"
    echo "║     B-UI 终端管理面板                                       ║"
    echo "║                                                              ║"
    echo "║     Hysteria2 + VLESS-Reality + Web 管理                    ║"
    echo "║                                                              ║"
    echo -e "║     版本: ${YELLOW}${SCRIPT_VERSION}${CYAN}                                           ║"
    echo "╚══════════════════════════════════════════════════════════════╝"
    echo -e "${NC}"
}

#===============================================================================
# 状态显示
#===============================================================================

check_bbr_status() {
    local cc=$(sysctl -n net.ipv4.tcp_congestion_control 2>/dev/null)
    [[ "$cc" == "bbr" ]]
}

get_domain() {
    if [[ -f "$CONFIG_FILE" ]]; then
        grep -A2 "^tls:" "$CONFIG_FILE" 2>/dev/null | grep "cert:" | sed 's|.*/live/\([^/]*\)/.*|\1|'
    fi
}

get_admin_password() {
    grep "ADMIN_PASSWORD=" /etc/systemd/system/b-ui-admin.service 2>/dev/null | cut -d= -f3
}

show_status() {
    echo ""
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}服务状态${NC}"
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    
    # Hysteria2 版本和状态
    if command -v hysteria &> /dev/null; then
        local hy_ver=$(hysteria version 2>/dev/null | grep -oE 'v[0-9]+\.[0-9]+\.[0-9]+' | head -1)
        hy_ver=${hy_ver:-$(hysteria version 2>/dev/null | awk 'NR==1 {print $2}')}
        hy_ver=${hy_ver:-"未知"}
        echo -e "  Hysteria2: ${YELLOW}${hy_ver}${NC}"
    else
        echo -e "  Hysteria2: ${RED}未安装${NC}"
    fi
    
    if systemctl is-active --quiet "$HYSTERIA_SERVICE"; then
        echo -e "  Hysteria服务: ${GREEN}运行中${NC}"
    else
        echo -e "  Hysteria服务: ${RED}未运行${NC}"
    fi
    
    # Xray 版本和状态
    if command -v xray &> /dev/null; then
        local xray_ver=$(xray version 2>/dev/null | grep -oE 'Xray [0-9]+\.[0-9]+\.[0-9]+' | head -1 | awk '{print $2}')
        xray_ver=${xray_ver:-$(xray version 2>/dev/null | head -1 | awk '{print $2}')}
        xray_ver=${xray_ver:-"未知"}
        echo -e "  Xray: ${YELLOW}v${xray_ver}${NC}"
        if systemctl is-active --quiet xray 2>/dev/null; then
            echo -e "  Xray服务: ${GREEN}运行中${NC}"
        else
            echo -e "  Xray服务: ${RED}未运行${NC}"
        fi
    fi
    
    # 管理面板
    if systemctl is-active --quiet "$ADMIN_SERVICE" 2>/dev/null; then
        echo -e "  管理面板: ${GREEN}运行中${NC}"
    else
        echo -e "  管理面板: ${YELLOW}未运行${NC}"
    fi
    
    # BBR
    if check_bbr_status; then
        echo -e "  BBR: ${GREEN}已启用${NC}"
    else
        echo -e "  BBR: ${YELLOW}未启用${NC}"
    fi
    
    # 开机自启动
    echo ""
    echo -e "${YELLOW}[开机自启动]${NC}"
    local hy_enabled=$(systemctl is-enabled "$HYSTERIA_SERVICE" 2>/dev/null); hy_enabled=${hy_enabled:-未配置}
    local xray_enabled=$(systemctl is-enabled xray 2>/dev/null); xray_enabled=${xray_enabled:-未配置}
    local admin_enabled=$(systemctl is-enabled "$ADMIN_SERVICE" 2>/dev/null); admin_enabled=${admin_enabled:-未配置}
    echo -e "  Hysteria2: ${CYAN}${hy_enabled}${NC}"
    echo -e "  Xray:      ${CYAN}${xray_enabled}${NC}"
    echo -e "  管理面板:  ${CYAN}${admin_enabled}${NC}"
    
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    
    # 网页管理面板信息
    local domain=$(get_domain)
    local admin_pass=$(get_admin_password)
    if [[ -n "$domain" ]]; then
        echo ""
        echo -e "${YELLOW}[网页管理面板]${NC}"
        echo -e "  访问地址: ${GREEN}https://${domain}${NC}"
        echo -e "  管理密码: ${GREEN}${admin_pass:-未设置}${NC}"
    fi
}

#===============================================================================
# 菜单
#===============================================================================

show_menu() {
    echo ""
    echo -e "${CYAN}╔══════════════════════════════════════════════════════════════╗${NC}"
    echo -e "${CYAN}║${NC}                    ${GREEN}B-UI 操作菜单${NC}                            ${CYAN}║${NC}"
    echo -e "${CYAN}╠══════════════════════════════════════════════════════════════╣${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}1.${NC} 查看客户端配置                                           ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}2.${NC} 重启所有服务                                             ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}3.${NC} 查看日志                                                 ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}4.${NC} 修改管理密码                                             ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}5.${NC} 开启 BBR                                                 ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}6.${NC} 开机自启动设置                                           ${CYAN}║${NC}"
    echo -e "${CYAN}╠══════════════════════════════════════════════════════════════╣${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}7.${NC} ${GREEN}检查 B-UI 更新${NC}                                          ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}8.${NC} ${GREEN}更新内核/客户端 (Hysteria2 + Xray + Client)${NC}               ${CYAN}║${NC}"
    echo -e "${CYAN}╠══════════════════════════════════════════════════════════════╣${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}9.${NC} ${RED}完全卸载${NC}                                                 ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}0.${NC} 退出                                                     ${CYAN}║${NC}"
    echo -e "${CYAN}╚══════════════════════════════════════════════════════════════╝${NC}"
}

#===============================================================================
# 功能函数
#===============================================================================

show_client_config() {
    if [[ ! -f "$USERS_FILE" ]]; then
        print_error "未找到用户配置"
        return
    fi
    
    local domain=$(get_domain)
    local port=$(grep "listen:" "$CONFIG_FILE" 2>/dev/null | head -1 | sed 's/.*://' | tr -d ' ')
    port=${port:-10000}
    
    echo ""
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}客户端配置${NC}"
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    
    # 解析用户列表
    local users=$(cat "$USERS_FILE" 2>/dev/null)
    echo "$users" | jq -r '.[] | "\(.username):\(.password)"' 2>/dev/null | while IFS=':' read -r uname upass; do
        echo -e "  用户: ${YELLOW}$uname${NC}"
        echo -e "  URI:  ${GREEN}hysteria2://${upass}@${domain}:${port}/?insecure=0#${uname}${NC}"
        echo ""
    done
    
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
}

restart_services() {
    systemctl restart "$HYSTERIA_SERVICE" 2>/dev/null || true
    systemctl restart "$ADMIN_SERVICE" 2>/dev/null || true
    systemctl restart xray 2>/dev/null || true
    print_success "所有服务已重启"
}

view_logs() {
    echo ""
    echo -e "${YELLOW}选择日志类型:${NC}"
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

toggle_autostart() {
    echo ""
    local hy_enabled=$(systemctl is-enabled "$HYSTERIA_SERVICE" 2>/dev/null || echo "disabled")
    local xray_enabled=$(systemctl is-enabled xray 2>/dev/null || echo "disabled")
    local admin_enabled=$(systemctl is-enabled "$ADMIN_SERVICE" 2>/dev/null || echo "disabled")
    
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
}

check_bui_update() {
    # 加载更新模块
    if [[ -f "${BASE_DIR}/update.sh" ]]; then
        source "${BASE_DIR}/update.sh"
        check_and_update
    else
        print_error "更新模块不存在"
    fi
}

update_kernel() {
    if [[ -f "${BASE_DIR}/update.sh" ]]; then
        source "${BASE_DIR}/update.sh"
        update_kernel
    else
        # 内联更新
        print_info "正在更新内核..."
        echo ""
        
        print_info "更新 Hysteria2..."
        local old_hy=$(hysteria version 2>/dev/null | head -n1 || echo "未知")
        bash <(curl -fsSL https://get.hy2.sh/)
        local new_hy=$(hysteria version 2>/dev/null | head -n1 || echo "未知")
        echo -e "  Hysteria2: ${YELLOW}${old_hy}${NC} -> ${GREEN}${new_hy}${NC}"
        
        if command -v xray &> /dev/null; then
            print_info "更新 Xray..."
            local old_xray=$(xray version 2>/dev/null | head -n1 || echo "未知")
            bash -c "$(curl -L https://github.com/XTLS/Xray-install/raw/main/install-release.sh)" @ install
            local new_xray=$(xray version 2>/dev/null | head -n1 || echo "未知")
            echo -e "  Xray: ${YELLOW}${old_xray}${NC} -> ${GREEN}${new_xray}${NC}"
        fi
        
        systemctl restart "$HYSTERIA_SERVICE" 2>/dev/null || true
        systemctl restart xray 2>/dev/null || true
        print_success "内核更新完成！"
    fi
    
    # 更新客户端安装包
    echo ""
    print_info "更新客户端安装包..."
    local GITHUB_RAW="https://raw.githubusercontent.com/Buxiulei/b-ui/main"
    local client_script="${BASE_DIR}/b-ui-client.sh"
    
    if curl -fsSL "${GITHUB_RAW}/b-ui-client.sh" -o "${client_script}.tmp" 2>/dev/null; then
        local new_ver=$(grep "SCRIPT_VERSION=" "${client_script}.tmp" | head -1 | cut -d'"' -f2)
        local old_ver=$(grep "SCRIPT_VERSION=" "${client_script}" 2>/dev/null | head -1 | cut -d'"' -f2 || echo "未知")
        
        mv "${client_script}.tmp" "${client_script}"
        chmod +x "${client_script}"
        echo -e "  客户端脚本: ${YELLOW}v${old_ver}${NC} -> ${GREEN}v${new_ver}${NC}"
        print_success "客户端安装包更新完成！"
    else
        rm -f "${client_script}.tmp"
        print_warning "客户端安装包更新失败，将使用现有版本"
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
    echo -e "  - b-ui 命令行工具"
    echo ""
    read -p "确定要继续吗? (输入 YES 确认): " confirm
    
    if [[ "$confirm" != "YES" ]]; then
        print_info "已取消卸载"
        return
    fi
    
    print_info "开始卸载..."
    
    # 停止服务
    systemctl stop hysteria-server 2>/dev/null || true
    systemctl stop b-ui-admin 2>/dev/null || true
    systemctl stop xray 2>/dev/null || true
    systemctl disable hysteria-server 2>/dev/null || true
    systemctl disable b-ui-admin 2>/dev/null || true
    systemctl disable xray 2>/dev/null || true
    
    # 删除服务文件
    rm -f /etc/systemd/system/hysteria-server.service
    rm -f /etc/systemd/system/b-ui-admin.service
    rm -rf /etc/systemd/system/hysteria-server.service.d
    rm -rf /etc/systemd/system/xray.service.d
    systemctl daemon-reload
    
    # 删除程序文件
    rm -f /usr/local/bin/hysteria
    rm -f /usr/local/bin/xray
    rm -rf /usr/local/share/xray
    rm -rf /opt/b-ui
    rm -f /usr/local/bin/b-ui
    
    # 删除 Nginx 配置
    rm -f /etc/nginx/conf.d/b-ui-admin.conf
    systemctl reload nginx 2>/dev/null || true
    
    echo ""
    echo -e "${GREEN}════════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}  卸载完成！${NC}"
    echo -e "${GREEN}════════════════════════════════════════════════════════════════${NC}"
    
    exit 0
}

#===============================================================================
# 主函数
#===============================================================================

main() {
    if [[ $EUID -ne 0 ]]; then
        print_error "请使用 sudo b-ui 运行"
        exit 1
    fi
    
    while true; do
        print_banner
        show_status
        show_menu
        
        read -p "请选择 [0-9]: " choice
        
        case $choice in
            1) show_client_config ;;
            2) restart_services ;;
            3) view_logs ;;
            4) change_password ;;
            5) enable_bbr ;;
            6) toggle_autostart ;;
            7) check_bui_update ;;
            8) update_kernel ;;
            9) uninstall_all ;;
            0) print_info "再见！"; exit 0 ;;
            *) print_error "无效选项" ;;
        esac
        
        echo ""
        read -p "按 Enter 继续..."
    done
}

main "$@"
