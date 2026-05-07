#!/bin/bash

#===============================================================================
# B-UI CLI 终端管理工具
# 功能：服务管理、状态查看、更新检查
# 版本: 动态读取自 version.json
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
        jq -r '.version' "${BASE_DIR}/version.json" 2>/dev/null || echo "unknown"
    else
        echo "unknown"
    fi
}
SCRIPT_VERSION=$(get_version)

# 打印函数
print_info() { echo -e "${BLUE}[INFO]${NC} $1"; }
print_success() { echo -e "${GREEN}[SUCCESS]${NC} $1"; }
print_warning() { echo -e "${YELLOW}[WARNING]${NC} $1"; }
print_error() { echo -e "${RED}[ERROR]${NC} $1"; }

#===============================================================================
# TUI 工具检测 & Helpers
#===============================================================================

TUI_AVAILABLE=false
command -v gum &>/dev/null && command -v fzf &>/dev/null && TUI_AVAILABLE=true

OPT_YES=false
OPT_JSON=false
OPT_QUIET=false

parse_global_flags() {
    for arg in "$@"; do
        case "$arg" in
            -y|--yes)    OPT_YES=true ;;
            --json)      OPT_JSON=true ;;
            --quiet|-q)  OPT_QUIET=true ;;
        esac
    done
}

tui_info() {
    [[ "$OPT_QUIET" == "true" ]] && return 0
    [[ "$TUI_AVAILABLE" == "true" ]] \
        && gum style --foreground 39 "  $*" \
        || echo -e "  ${CYAN}$*${NC}"
}

tui_success() {
    [[ "$OPT_QUIET" == "true" ]] && return 0
    [[ "$TUI_AVAILABLE" == "true" ]] \
        && gum style --foreground 46 "✓ $*" \
        || echo -e "  ${GREEN}✓ $*${NC}"
}

tui_error() {
    [[ "$TUI_AVAILABLE" == "true" ]] \
        && gum style --foreground 196 "✗ $*" >&2 \
        || echo -e "  ${RED}✗ $*${NC}" >&2
}

tui_spin() {
    local title="$1"; shift
    if [[ "$TUI_AVAILABLE" == "true" ]]; then
        gum spin --spinner dot --title "$title" -- "$@"
    else
        tui_info "$title"
        "$@"
    fi
}

tui_confirm() {
    local prompt="$1"
    if [[ "$OPT_YES" == "true" ]]; then return 0; fi
    if [[ "$TUI_AVAILABLE" == "true" ]]; then
        gum confirm "$prompt"
    else
        local ans
        read -p "${prompt} (y/n): " ans
        [[ "$ans" =~ ^[yY]$ ]]
    fi
}

tui_menu() {
    local header="$1"; shift
    if [[ "$TUI_AVAILABLE" == "true" ]]; then
        gum choose --header "$header" "$@"
    else
        local i=1
        echo "$header"
        for opt in "$@"; do
            echo "  $i. $opt"
            ((i++))
        done
        local choice
        read -p "选择 (1-$((i-1))): " choice
        local opts=("$@")
        if ! [[ "$choice" =~ ^[0-9]+$ ]] || (( choice < 1 || choice > ${#opts[@]} )); then
            tui_error "无效选择"
            return 1
        fi
        echo "${opts[$((choice-1))]}"
    fi
}

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
    # 优先从证书同步保存的域名文件读取
    if [[ -f "${BASE_DIR}/certs/.domain" ]]; then
        cat "${BASE_DIR}/certs/.domain"
        return
    fi
    # 备选：从 Caddyfile 第一个站点块解析域名
    if [[ -f /etc/caddy/Caddyfile ]]; then
        grep -oP '^[a-zA-Z0-9][-a-zA-Z0-9.]*\.[a-zA-Z]{2,}' /etc/caddy/Caddyfile 2>/dev/null | head -1
        return
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

show_status_bar_server() {
    local hy2_icon="🔴" xray_icon="🔴" admin_icon="🔴" caddy_icon="🔴"
    systemctl is-active --quiet hysteria-server 2>/dev/null && hy2_icon="🟢"
    systemctl is-active --quiet xray 2>/dev/null            && xray_icon="🟢"
    systemctl is-active --quiet b-ui-admin 2>/dev/null      && admin_icon="🟢"
    systemctl is-active --quiet caddy 2>/dev/null           && caddy_icon="🟢"
    local bbr_status="✗"
    sysctl net.ipv4.tcp_congestion_control 2>/dev/null | grep -q bbr && bbr_status="✓"

    if [[ "$TUI_AVAILABLE" == "true" ]]; then
        gum style \
            --border rounded --border-foreground 39 \
            --padding "0 2" --margin "1 0" \
            "$(gum style --bold 'B-UI Server')" "" \
            "Hysteria2   ${hy2_icon}  运行状态" \
            "Xray        ${xray_icon}  运行状态" \
            "Admin 面板  ${admin_icon}  :8080" \
            "Caddy       ${caddy_icon}  运行状态" \
            "BBR         ${bbr_status}"
    else
        echo -e "  Hysteria2: ${hy2_icon}  Xray: ${xray_icon}  Admin: ${admin_icon}  Caddy: ${caddy_icon}  BBR: ${bbr_status}"
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
    echo -e "${CYAN}║${NC}  ${YELLOW}10.${NC} ${YELLOW}端口跳跃设置${NC}                                            ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}11.${NC} ${BLUE}VPS 质量测试${NC}                                            ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}12.${NC} ${YELLOW}配置住宅 IP 出站${NC}                                        ${CYAN}║${NC}"
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
    
    # 尝试智能选择下载源 (优先使用 update.sh 的智能选择结果)
    local download_src="https://raw.githubusercontent.com/Buxiulei/b-ui/main"
    if type select_download_source &>/dev/null; then
        select_download_source
        if [[ -n "$DOWNLOAD_URL" ]]; then
            download_src="$DOWNLOAD_URL"
        fi
    fi
    
    local client_script="${BASE_DIR}/b-ui-client.sh"
    
    if curl -fsSL "${download_src}/b-ui-client.sh" -o "${client_script}.tmp" 2>/dev/null; then
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

configure_port_hopping_menu() {
    echo ""
    echo -e "${CYAN}════════════════════════════════════════════════════════════════${NC}"
    echo -e "${CYAN}  端口跳跃设置${NC}"
    echo -e "${CYAN}════════════════════════════════════════════════════════════════${NC}"
    
    # 检查当前状态
    local config_file="${BASE_DIR}/port-hopping.json"
    if [[ -f "$config_file" ]]; then
        local enabled=$(jq -r '.enabled // false' "$config_file" 2>/dev/null)
        local start=$(jq -r '.startPort // 20000' "$config_file" 2>/dev/null)
        local end=$(jq -r '.endPort // 30000' "$config_file" 2>/dev/null)
        echo -e "  当前状态: ${GREEN}已启用${NC}"
        echo -e "  端口范围: ${YELLOW}${start}-${end}${NC}"
    else
        echo -e "  当前状态: ${RED}未启用${NC}"
    fi
    
    echo ""
    echo -e "  ${YELLOW}1.${NC} 启用/重新配置端口跳跃"
    echo -e "  ${YELLOW}2.${NC} 禁用端口跳跃"
    echo -e "  ${YELLOW}0.${NC} 返回主菜单"
    echo ""
    read -p "请选择 [0-2]: " ph_choice
    
    case $ph_choice in
        1)
            echo ""
            # 获取当前 Hysteria 端口
            local hy_port=$(grep -oP '^listen:\s*:(\d+)' "${CONFIG_FILE}" 2>/dev/null | grep -oP '\d+' || echo "10000")
            
            read -p "起始端口 [默认: 20000]: " start_port
            start_port=${start_port:-20000}
            
            read -p "结束端口 [默认: 30000]: " end_port
            end_port=${end_port:-30000}
            
            # 验证端口范围
            if [[ $start_port -ge $end_port ]]; then
                print_error "起始端口必须小于结束端口"
                return 1
            fi
            
            print_info "配置端口跳跃 (${start_port}-${end_port} -> ${hy_port})..."
            
            # 设置环境变量并调用 core.sh 中的函数
            export PORT_HOPPING_ENABLED="y"
            export PORT_HOPPING_START="$start_port"
            export PORT_HOPPING_END="$end_port"
            export PORT="$hy_port"
            
            # source core.sh 并调用函数
            if [[ -f "${BASE_DIR}/core.sh" ]]; then
                source "${BASE_DIR}/core.sh"
                configure_port_hopping
                print_success "端口跳跃配置完成！"
                print_info "新用户创建的链接将自动包含 mport 参数"
            else
                print_error "core.sh 未找到"
            fi
            ;;
        2)
            print_info "禁用端口跳跃..."
            
            # 清理 iptables 规则
            local iface=$(ip route get 8.8.8.8 2>/dev/null | grep -oP 'dev \K\S+' | head -1)
            [[ -z "$iface" ]] && iface="eth0"
            
            # 删除所有带 Hysteria2-PortHopping 注释的规则
            local rule_nums=$(iptables -t nat -L PREROUTING --line-numbers -n 2>/dev/null | grep "Hysteria2-PortHopping" | awk '{print $1}' | sort -rn)
            for num in $rule_nums; do
                iptables -t nat -D PREROUTING $num 2>/dev/null || true
            done
            
            # 删除配置文件
            rm -f "${BASE_DIR}/port-hopping.json"
            
            print_success "端口跳跃已禁用"
            ;;
        0|*)
            return 0
            ;;
    esac
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
    systemctl stop hysteria-server caddy b-ui-admin xray b-ui-cert-sync.timer 2>/dev/null || true
    systemctl disable hysteria-server caddy b-ui-admin xray b-ui-cert-sync.timer b-ui-cert-sync 2>/dev/null || true
    
    # 删除服务文件
    rm -f /etc/systemd/system/hysteria-server.service
    rm -f /etc/systemd/system/b-ui-admin.service
    rm -f /etc/systemd/system/b-ui-cert-sync.service
    rm -f /etc/systemd/system/b-ui-cert-sync.timer
    rm -rf /etc/systemd/system/hysteria-server.service.d
    rm -rf /etc/systemd/system/xray.service.d
    systemctl daemon-reload
    
    # 删除程序文件
    rm -f /usr/local/bin/hysteria
    rm -f /usr/local/bin/xray
    rm -rf /usr/local/share/xray
    rm -rf /opt/b-ui
    rm -f /usr/local/bin/b-ui
    
    # 清理 Caddy 配置 (保留 Caddy 程序供其他用途)
    rm -f /etc/caddy/Caddyfile 2>/dev/null
    
    # 清理旧版 Nginx 配置 (如果存在)
    rm -f /etc/nginx/conf.d/b-ui-admin.conf 2>/dev/null
    systemctl reload nginx 2>/dev/null || true
    
    # 清理 cron 任务
    crontab -l 2>/dev/null | grep -vE "b-ui|cert-check|cert-sync" | crontab - 2>/dev/null || true
    
    echo ""
    echo -e "${GREEN}════════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}  卸载完成！${NC}"
    echo -e "${GREEN}════════════════════════════════════════════════════════════════${NC}"
    
    exit 0
}

run_vps_benchmark() {
    echo ""
    echo -e "${CYAN}════════════════════════════════════════════════════════════════${NC}"
    echo -e "${CYAN}  VPS 质量测试 (goecs)${NC}"
    echo -e "${CYAN}════════════════════════════════════════════════════════════════${NC}"
    echo ""
    print_info "正在下载并运行 VPS 测试脚本..."
    echo ""
    
    export noninteractive=true
    curl -L https://raw.githubusercontent.com/oneclickvirt/ecs/master/goecs.sh -o /tmp/goecs.sh && \
    chmod +x /tmp/goecs.sh && \
    /tmp/goecs.sh install && \
    goecs
    
    rm -f /tmp/goecs.sh 2>/dev/null
}

#===============================================================================
# 住宅 IP 配置菜单
#===============================================================================

configure_residential_menu() {
    local helper="${BASE_DIR}/residential-helper.sh"

    while true; do
        echo ""
        echo -e "${CYAN}════════════ 配置住宅 IP 出站 ════════════${NC}"

        local status enabled exit_ip isp_info verified_at
        status=$("$helper" status 2>/dev/null || echo '{"enabled":false}')
        enabled=$(echo "$status" | jq -r '.enabled' 2>/dev/null || echo "false")
        exit_ip=$(echo "$status" | jq -r '.lastVerifiedIp // ""' 2>/dev/null || echo "")
        isp_info=$(echo "$status" | jq -r '.lastVerifiedIspInfo // ""' 2>/dev/null || echo "")
        verified_at=$(echo "$status" | jq -r '.lastVerifiedAt // ""' 2>/dev/null || echo "")

        if [[ "$enabled" == "true" ]]; then
            echo -e "  状态: ${GREEN}已启用 ✓${NC}"
            [[ -n "$exit_ip" ]]     && echo -e "  出口 IP: ${YELLOW}${exit_ip}${NC}"
            [[ -n "$isp_info" ]]    && echo -e "  ISP: ${isp_info}"
            [[ -n "$verified_at" ]] && echo -e "  最后校验: ${verified_at}"
        else
            echo -e "  状态: ${RED}未启用${NC}"
        fi

        echo ""
        echo -e "  ${YELLOW}1.${NC} 启用 / 修改凭据"
        echo -e "  ${YELLOW}2.${NC} 禁用住宅 IP"
        echo -e "  ${YELLOW}3.${NC} 重新校验连通性"
        echo -e "  ${YELLOW}0.${NC} 返回主菜单"
        echo ""
        read -rp "请选择: " sub

        case "$sub" in
            1)
                read -rp "$(echo -e "${YELLOW}请粘贴凭据 (socks5://user:pass@host:port): ${NC}")" resi_url
                [[ -z "$resi_url" ]] && continue
                print_info "正在校验连通性..."
                local out ec
                out=$("$helper" enable "$resi_url" 2>&1) && ec=0 || ec=$?
                if [[ $ec -eq 0 ]]; then
                    print_success "住宅 IP 已启用，出口 IP: $(echo "$out" | head -1)"
                    local isp
                    isp=$(echo "$out" | sed -n '2p')
                    [[ -n "$isp" ]] && print_info "ISP: $isp"
                else
                    print_error "校验失败（配置未保存）:"
                    echo "$out" | grep "ERROR:" | sed 's/.*ERROR: //'
                fi
                ;;
            2)
                "$helper" disable && print_success "住宅 IP 已禁用，流量恢复直出" \
                    || print_error "禁用失败"
                ;;
            3)
                if [[ "$enabled" != "true" ]]; then
                    print_warning "住宅 IP 未启用，无法重新校验"
                    continue
                fi
                local host port user pass
                host=$(echo "$status" | jq -r '.host')
                port=$(echo "$status" | jq -r '.port')
                user=$(echo "$status" | jq -r '.username')
                pass=$(echo "$status" | jq -r '.password')
                print_info "正在重新校验..."
                local out ec
                out=$("$helper" enable "socks5://${user}:${pass}@${host}:${port}" 2>&1) && ec=0 || ec=$?
                if [[ $ec -eq 0 ]]; then
                    print_success "校验成功，出口 IP: $(echo "$out" | head -1)"
                else
                    print_error "校验失败: $(echo "$out" | grep "ERROR:" | sed 's/.*ERROR: //')"
                fi
                ;;
            0) return ;;
            *) print_error "无效选项" ;;
        esac
    done
}

#===============================================================================
# 非交互子命令（服务端）
#===============================================================================

cmd_server_status() {
    local hy2 xray admin caddy bbr
    hy2=$(systemctl is-active hysteria-server 2>/dev/null || echo "inactive")
    xray=$(systemctl is-active xray 2>/dev/null || echo "inactive")
    admin=$(systemctl is-active b-ui-admin 2>/dev/null || echo "inactive")
    caddy=$(systemctl is-active caddy 2>/dev/null || echo "inactive")
    bbr="false"
    sysctl net.ipv4.tcp_congestion_control 2>/dev/null | grep -q bbr && bbr="true"

    if [[ "$OPT_JSON" == "true" ]]; then
        printf '{\n'
        printf '  "hysteria2": "%s",\n' "$hy2"
        printf '  "xray": "%s",\n' "$xray"
        printf '  "admin": "%s",\n' "$admin"
        printf '  "caddy": "%s",\n' "$caddy"
        printf '  "bbr": %s\n' "$bbr"
        printf '}\n'
    else
        echo "Hysteria2:  $hy2"
        echo "Xray:       $xray"
        echo "Admin:      $admin"
        echo "Caddy:      $caddy"
        echo "BBR:        $bbr"
    fi
}

cmd_server_restart() {
    tui_info "重启所有服务..."
    systemctl restart hysteria-server 2>/dev/null || true
    systemctl restart xray 2>/dev/null || true
    systemctl restart b-ui-admin 2>/dev/null || true
    systemctl restart caddy 2>/dev/null || true
    tui_success "所有服务已重启"
    exit 0
}

cmd_server_logs() {
    local svc="${1:-hysteria2}"
    case "$svc" in
        hysteria2) journalctl -u hysteria-server --no-pager -n 100 ;;
        xray)      journalctl -u xray --no-pager -n 100 ;;
        admin)     journalctl -u b-ui-admin --no-pager -n 100 ;;
        caddy)     journalctl -u caddy --no-pager -n 100 ;;
        *)
            echo "用法: b-ui logs <hysteria2|xray|admin|caddy>" >&2
            exit 2
            ;;
    esac
    exit 0
}

cmd_server_residential() {
    local action="$1"; shift
    case "$action" in
        enable)
            local url="$1"
            [[ -z "$url" ]] && { echo "用法: b-ui residential enable <url>" >&2; exit 2; }
            bash "${BASE_DIR}/residential-helper.sh" enable "$url"
            ;;
        disable)
            bash "${BASE_DIR}/residential-helper.sh" disable
            ;;
        status)
            bash "${BASE_DIR}/residential-helper.sh" status
            ;;
        *)
            echo "用法: b-ui residential <enable <url>|disable|status>" >&2
            exit 2
            ;;
    esac
    exit 0
}

dispatch_subcommand_server() {
    local cmd="$1"; shift
    case "$cmd" in
        status)      cmd_server_status "$@"; exit 0 ;;
        restart)     cmd_server_restart "$@"; exit $? ;;
        logs)        cmd_server_logs "$@"; exit $? ;;
        residential) cmd_server_residential "$@" ;;
        update)      check_bui_update; exit 0 ;;
        -h|--help|help)
            cat <<'HELP'
用法: b-ui [subcommand] [options]

  无参数         进入 TUI 交互菜单

子命令:
  status                查看服务状态（--json）
  restart               重启所有服务
  logs <service>        查看日志（hysteria2/xray/admin/caddy）
  update                检查并更新
  residential enable <url>   启用住宅 IP 出口
  residential disable        禁用住宅 IP 出口
  residential status         查看住宅 IP 状态

通用 flags:
  -y, --yes    跳过确认
  --json       JSON 输出
HELP
            exit 0
            ;;
        *)
            echo "未知命令: $cmd。运行 'b-ui --help' 查看帮助。" >&2
            exit 2
            ;;
    esac
}

#===============================================================================
# 主函数
#===============================================================================

main() {
    if [[ $EUID -ne 0 ]]; then
        echo "请使用 sudo b-ui 运行" >&2
        exit 1
    fi

    parse_global_flags "$@"
    local args=()
    for arg in "$@"; do
        case "$arg" in
            -h|--help) dispatch_subcommand_server --help; exit 0 ;;
            -*) : ;;
            *) args+=("$arg") ;;
        esac
    done

    if [[ ${#args[@]} -gt 0 ]]; then
        dispatch_subcommand_server "${args[@]}"
        exit $?
    fi

    # TUI 主循环
    while true; do
        clear
        print_banner
        show_status_bar_server

        local choice
        if [[ "$TUI_AVAILABLE" == "true" ]]; then
            choice=$(gum choose \
                "重启所有服务" \
                "查看日志 →" \
                "查看客户端配置" \
                "──────────" \
                "更新" \
                "端口跳跃设置" \
                "住宅 IP 出口 →" \
                "──────────" \
                "更多设置 →" \
                "卸载" \
                "退出" \
                2>/dev/null) || choice="退出"
        else
            show_menu
            read -p "请选择 [0-12]: " num
            case $num in
                1)  choice="查看客户端配置" ;;
                2)  choice="重启所有服务" ;;
                3)  choice="查看日志 →" ;;
                7)  choice="更新" ;;
                9)  choice="卸载" ;;
                10) choice="端口跳跃设置" ;;
                12) choice="住宅 IP 出口 →" ;;
                0)  choice="退出" ;;
                *)  choice="更多设置 →" ;;
            esac
        fi

        case "$choice" in
            "重启所有服务")
                tui_info "重启所有服务..."
                systemctl restart hysteria-server 2>/dev/null || true
                systemctl restart xray 2>/dev/null || true
                systemctl restart b-ui-admin 2>/dev/null || true
                systemctl restart caddy 2>/dev/null || true
                tui_success "所有服务已重启"
                ;;
            "查看日志 →")
                local svc
                svc=$(tui_menu "查看哪个服务的日志？" "Hysteria2" "Xray" "Admin 面板" "Caddy" "返回")
                case "$svc" in
                    "Hysteria2")  journalctl -u hysteria-server --no-pager -n 100 | less ;;
                    "Xray")       journalctl -u xray --no-pager -n 100 | less ;;
                    "Admin 面板") journalctl -u b-ui-admin --no-pager -n 100 | less ;;
                    "Caddy")      journalctl -u caddy --no-pager -n 100 | less ;;
                esac
                ;;
            "查看客户端配置") show_client_config ;;
            "更新")            check_bui_update ;;
            "端口跳跃设置")    configure_port_hopping_menu ;;
            "住宅 IP 出口 →")  configure_residential_menu ;;
            "更多设置 →")
                local sub
                sub=$(tui_menu "更多设置" "修改管理密码" "BBR 设置" "自启动管理" "VPS 测速" "返回")
                case "$sub" in
                    "修改管理密码") change_password ;;
                    "BBR 设置")     enable_bbr ;;
                    "自启动管理")   toggle_autostart ;;
                    "VPS 测速")     run_vps_benchmark ;;
                esac
                ;;
            "卸载")            uninstall_all ;;
            "退出")            tui_info "再见！"; exit 0 ;;
            "──────────")     continue ;;
            "__invalid__")    print_error "无效选项" ;;
        esac

        if [[ "$TUI_AVAILABLE" != "true" ]]; then
            echo ""
            read -p "按 Enter 继续..."
        fi
    done
}

main "$@"
