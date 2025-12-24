#!/bin/bash

#===============================================================================
# Hysteria2 客户端一键安装脚本 (Ubuntu/Debian)
# 功能：SOCKS5/HTTP 代理、TUN 模式、路由规则、SSH 保护
# 版本: 动态读取自 GitHub
#===============================================================================

# 版本号会在安装时从 GitHub 同步更新
SCRIPT_VERSION="2.15.2"

# 注意: 不使用 set -e，因为它会导致 ((count++)) 等算术运算在变量为0时退出脚本

# 颜色定义
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m'

# 路径配置 - 使用固定路径避免管道运行时 $0 问题
BASE_DIR="/opt/hysteria-client"
CONFIG_FILE="${BASE_DIR}/config.yaml"
RULES_FILE="${BASE_DIR}/bypass-rules.txt"
CLIENT_SERVICE="hysteria-client.service"

# 多配置管理路径
CONFIGS_DIR="${BASE_DIR}/configs"       # 存储所有配置
ACTIVE_CONFIG="${BASE_DIR}/active"      # 当前激活的配置名称

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
    echo -e "║     版本: ${YELLOW}${SCRIPT_VERSION}${CYAN}                                               ║"
    echo "╚══════════════════════════════════════════════════════════════╝"
    echo -e "${NC}"
}

print_info() { echo -e "${BLUE}[INFO]${NC} $1"; }
print_success() { echo -e "${GREEN}[SUCCESS]${NC} $1"; }
print_warning() { echo -e "${YELLOW}[WARNING]${NC} $1"; }
print_error() { echo -e "${RED}[ERROR]${NC} $1"; }

# 进度动画 - 在后台运行命令时显示旋转动画
# JSON 字符串转义函数
json_escape() {
    local str="$1"
    # 转义反斜杠、双引号、换行等特殊字符
    str="${str//\\/\\\\}"    # 反斜杠
    str="${str//\"/\\\"}"    # 双引号
    str="${str//$'\n'/\\n}"  # 换行
    str="${str//$'\r'/\\r}"  # 回车
    str="${str//$'\t'/\\t}"  # Tab
    echo "$str"
}

spinner_pid=""
start_spinner() {
    local msg="${1:-请稍候...}"
    (
        chars="⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"
        while true; do
            for (( i=0; i<${#chars}; i++ )); do
                echo -ne "\r${CYAN}${chars:$i:1}${NC} $msg"
                sleep 0.1
            done
        done
    ) &
    spinner_pid=$!
}

stop_spinner() {
    if [[ -n "$spinner_pid" ]]; then
        kill "$spinner_pid" 2>/dev/null
        wait "$spinner_pid" 2>/dev/null
        echo -ne "\r\033[K"  # 清除行
        spinner_pid=""
    fi
}

# 带进度动画执行命令
run_with_spinner() {
    local msg="$1"
    shift
    start_spinner "$msg"
    "$@" > /dev/null 2>&1
    local ret=$?
    stop_spinner
    return $ret
}

# 远程版本检查 URL (使用 jsDelivr CDN)
REMOTE_VERSION_URL="https://cdn.jsdelivr.net/gh/Buxiulei/b-ui@main/b-ui-client.sh"
GITHUB_RAW_URL="https://raw.githubusercontent.com/Buxiulei/b-ui/main/b-ui-client.sh"

# 全局变量存储更新状态
UPDATE_AVAILABLE=""
REMOTE_VERSION=""

# 检查客户端更新
check_client_update() {
    # 尝试获取远程脚本的版本号
    local remote_script=""
    
    # 先尝试 jsDelivr CDN
    remote_script=$(curl -fsSL --max-time 5 "$REMOTE_VERSION_URL" 2>/dev/null | head -20)
    
    # 如果失败，尝试 GitHub Raw
    if [[ -z "$remote_script" ]]; then
        remote_script=$(curl -fsSL --max-time 5 "$GITHUB_RAW_URL" 2>/dev/null | head -20)
    fi
    
    if [[ -n "$remote_script" ]]; then
        REMOTE_VERSION=$(echo "$remote_script" | grep -oP 'SCRIPT_VERSION="\K[^"]+' | head -1)
        if [[ -n "$REMOTE_VERSION" && "$REMOTE_VERSION" != "$SCRIPT_VERSION" ]]; then
            # 版本比较
            if [[ "$(printf '%s\n' "$REMOTE_VERSION" "$SCRIPT_VERSION" | sort -V | tail -n1)" == "$REMOTE_VERSION" ]]; then
                UPDATE_AVAILABLE="true"
                return 0
            fi
        fi
    fi
    
    UPDATE_AVAILABLE=""
    return 1
}

# 执行客户端更新
do_client_update() {
    print_info "正在更新客户端..."
    
    local temp_script="/tmp/b-ui-client-new.sh"
    
    # 下载新版本
    if curl -fsSL --max-time 60 "$REMOTE_VERSION_URL" -o "$temp_script" 2>/dev/null || \
       curl -fsSL --max-time 60 "$GITHUB_RAW_URL" -o "$temp_script" 2>/dev/null; then
        
        # 验证下载的脚本
        local lines=$(wc -l < "$temp_script" 2>/dev/null || echo "0")
        if [[ "$lines" -gt 100 ]]; then
            # 更新 bui-c
            cp "$temp_script" /usr/local/bin/bui-c
            chmod +x /usr/local/bin/bui-c
            rm -f "$temp_script"
            
            print_success "客户端已更新至 v${REMOTE_VERSION}"
            echo ""
            print_info "请重新运行 bui-c 使更新生效"
            exit 0
        else
            rm -f "$temp_script"
            print_error "下载的脚本不完整"
            return 1
        fi
    else
        print_error "下载失败"
        return 1
    fi
}

check_root() {
    if [[ $EUID -ne 0 ]]; then
        print_error "此脚本需要 root 权限"
        exit 1
    fi
}

check_os() {
    if [[ -f /etc/os-release ]]; then
        . /etc/os-release
        OS_ID="$ID"
        OS_VERSION="$VERSION_ID"
        print_info "操作系统: $OS_ID $OS_VERSION"
    else
        OS_ID="unknown"
        print_warning "无法识别操作系统"
    fi
}

#===============================================================================
# 智能下载函数 - 国内优先
# 默认假设无法直连 GitHub/Google，优先使用国内镜像
#===============================================================================

# 国内镜像列表
MIRROR_GHPROXY="https://ghproxy.com"
MIRROR_GHPROXY2="https://mirror.ghproxy.com"  
MIRROR_FASTGIT="https://hub.fastgit.xyz"
MIRROR_JSR="https://cdn.jsdelivr.net/gh"

# 智能下载文件 (优先国内镜像)
# 用法: smart_download <url> <output_file> [description]
smart_download() {
    local url="$1"
    local output="$2"
    local desc="${3:-下载文件}"
    local success=false
    
    # 检测 URL 类型
    if [[ "$url" == *"github.com"* ]] || [[ "$url" == *"raw.githubusercontent.com"* ]]; then
        # GitHub URL，使用镜像
        local github_path=$(echo "$url" | sed -E 's|https://(raw\.)?github(usercontent)?\.com/||')
        
        # 方法1: ghproxy 镜像 (国内首选)
        print_info "尝试 ghproxy 镜像..."
        if curl -fsSL --max-time 60 "${MIRROR_GHPROXY}/${url}" -o "$output" 2>/dev/null; then
            success=true
        fi
        
        # 方法2: ghproxy2 镜像
        if [[ "$success" == "false" ]]; then
            print_info "尝试 mirror.ghproxy 镜像..."
            if curl -fsSL --max-time 60 "${MIRROR_GHPROXY2}/${url}" -o "$output" 2>/dev/null; then
                success=true
            fi
        fi
        
        # 方法3: jsdelivr CDN (如果是 raw 文件)
        if [[ "$success" == "false" ]] && [[ "$url" == *"raw.githubusercontent.com"* ]]; then
            # 转换: raw.githubusercontent.com/user/repo/branch/path -> cdn.jsdelivr.net/gh/user/repo@branch/path
            local jsr_path=$(echo "$url" | sed -E 's|https://raw\.githubusercontent\.com/([^/]+)/([^/]+)/([^/]+)/(.*)|\1/\2@\3/\4|')
            print_info "尝试 jsdelivr CDN..."
            if curl -fsSL --max-time 60 "${MIRROR_JSR}/${jsr_path}" -o "$output" 2>/dev/null; then
                success=true
            fi
        fi
        
        # 方法4: 如果有本地代理，通过代理下载
        if [[ "$success" == "false" ]] && ss -tuln 2>/dev/null | grep -q ":1080 "; then
            print_info "通过本地代理下载..."
            if curl --socks5 127.0.0.1:1080 -fsSL --max-time 120 "$url" -o "$output" 2>/dev/null; then
                success=true
            fi
        fi
        
        # 方法5: 直连 (最后尝试)
        if [[ "$success" == "false" ]]; then
            print_info "尝试直接下载..."
            if curl -fsSL --max-time 120 "$url" -o "$output" 2>/dev/null; then
                success=true
            fi
        fi
    else
        # 非 GitHub URL，直接下载或使用代理
        # 先尝试直连
        if curl -fsSL --max-time 30 "$url" -o "$output" 2>/dev/null; then
            success=true
        elif ss -tuln 2>/dev/null | grep -q ":1080 "; then
            # 通过本地代理
            print_info "通过本地代理下载..."
            if curl --socks5 127.0.0.1:1080 -fsSL --max-time 120 "$url" -o "$output" 2>/dev/null; then
                success=true
            fi
        fi
    fi
    
    if [[ "$success" == "true" ]] && [[ -f "$output" ]] && [[ -s "$output" ]]; then
        return 0
    else
        rm -f "$output" 2>/dev/null
        return 1
    fi
}

# 执行远程脚本 (优先国内镜像)
# 用法: smart_run_script <url> [args...]
smart_run_script() {
    local url="$1"
    shift
    local args="$@"
    local tmp_script=$(mktemp)
    
    if smart_download "$url" "$tmp_script" "下载安装脚本"; then
        chmod +x "$tmp_script"
        bash "$tmp_script" $args
        local ret=$?
        rm -f "$tmp_script"
        return $ret
    else
        rm -f "$tmp_script"
        print_error "脚本下载失败: $url"
        return 1
    fi
}

#===============================================================================
# 从 B-UI 服务端下载安装包
# 优先使用服务端中转，解决国内无法直连 GitHub 的问题
#===============================================================================

# 存储服务端地址 (从导入的配置中获取)
SERVER_ADDRESS=""
SERVER_ADDRESS_FILE="${BASE_DIR}/server_address"

# 设置服务端地址
set_server_address() {
    local addr="$1"
    SERVER_ADDRESS="$addr"
    echo "$addr" > "$SERVER_ADDRESS_FILE"
}

# 加载已保存的服务端地址
load_server_address() {
    if [[ -f "$SERVER_ADDRESS_FILE" ]]; then
        SERVER_ADDRESS=$(cat "$SERVER_ADDRESS_FILE" 2>/dev/null)
    fi
}

# 从服务端下载安装包
# 用法: download_from_server <filename> <output_file>
download_from_server() {
    local filename="$1"
    local output="$2"
    
    load_server_address
    
    if [[ -z "$SERVER_ADDRESS" ]]; then
        return 1
    fi
    
    # 构建下载 URL
    local url="https://${SERVER_ADDRESS}/packages/${filename}"
    
    print_info "从服务端下载 ${filename}..."
    if curl -fsSL --max-time 120 -k "$url" -o "$output" 2>/dev/null; then
        if [[ -f "$output" ]] && [[ -s "$output" ]]; then
            return 0
        fi
    fi
    
    return 1
}

# 智能安装核心 - 优先服务端，降级到镜像
# 用法: smart_install_core <core_name>
smart_install_core() {
    local core_name="$1"
    local arch=$(uname -m)
    local arch_suffix
    case "$arch" in
        x86_64) arch_suffix="amd64" ;;
        aarch64) arch_suffix="arm64" ;;
        *) arch_suffix="amd64" ;;
    esac
    
    case "$core_name" in
        hysteria)
            local filename="hysteria-linux-${arch_suffix}"
            local output="/tmp/hysteria"
            
            # 方法1: 从服务端下载
            if download_from_server "$filename" "$output"; then
                chmod +x "$output"
                mv "$output" /usr/local/bin/hysteria
                print_success "Hysteria2 安装完成 (从服务端)"
                return 0
            fi
            
            # 方法2: 使用智能下载
            print_info "服务端下载失败，使用镜像下载..."
            install_hysteria
            ;;
            
        xray)
            local filename="xray-linux-${arch_suffix}.zip"
            local output="/tmp/xray.zip"
            
            # 方法1: 从服务端下载
            if download_from_server "$filename" "$output"; then
                unzip -o "$output" -d /tmp/xray_temp >/dev/null 2>&1
                mv /tmp/xray_temp/xray /usr/local/bin/xray
                chmod +x /usr/local/bin/xray
                rm -rf "$output" /tmp/xray_temp
                print_success "Xray 安装完成 (从服务端)"
                return 0
            fi
            
            # 方法2: 使用智能下载
            print_info "服务端下载失败，使用镜像下载..."
            install_xray_client
            ;;
            
        singbox)
            local filename="sing-box-linux-${arch_suffix}.tar.gz"
            local output="/tmp/sing-box.tar.gz"
            
            # 方法1: 从服务端下载
            if download_from_server "$filename" "$output"; then
                tar -xzf "$output" -C /tmp >/dev/null 2>&1
                find /tmp -name "sing-box" -type f -exec mv {} /usr/bin/sing-box \;
                chmod +x /usr/bin/sing-box
                rm -rf "$output" /tmp/sing-box-*
                print_success "sing-box 安装完成 (从服务端)"
                return 0
            fi
            
            # 方法2: 使用智能下载
            print_info "服务端下载失败，使用镜像下载..."
            install_singbox
            ;;
    esac
}

#===============================================================================
# 依赖检测与安装
#===============================================================================

# 检测包管理器
detect_package_manager() {
    if command -v apt-get &> /dev/null; then
        PKG_MANAGER="apt"
        PKG_UPDATE="apt-get update -qq"
        PKG_INSTALL="apt-get install -y -qq"
    elif command -v dnf &> /dev/null; then
        PKG_MANAGER="dnf"
        PKG_UPDATE="dnf check-update -q || true"
        PKG_INSTALL="dnf install -y -q"
    elif command -v yum &> /dev/null; then
        PKG_MANAGER="yum"
        PKG_UPDATE="yum check-update -q || true"
        PKG_INSTALL="yum install -y -q"
    elif command -v pacman &> /dev/null; then
        PKG_MANAGER="pacman"
        PKG_UPDATE="pacman -Sy --noconfirm"
        PKG_INSTALL="pacman -S --noconfirm"
    elif command -v apk &> /dev/null; then
        PKG_MANAGER="apk"
        PKG_UPDATE="apk update"
        PKG_INSTALL="apk add --no-cache"
    else
        PKG_MANAGER="unknown"
        print_warning "未检测到支持的包管理器"
    fi
}

# 检查并安装单个依赖
check_and_install_dep() {
    local cmd="$1"
    local pkg="$2"
    local desc="$3"
    
    if command -v "$cmd" &> /dev/null; then
        echo -e "  ${GREEN}✓${NC} $desc ($cmd)"
        return 0
    else
        echo -e "  ${YELLOW}○${NC} $desc - 正在安装..."
        if [[ "$PKG_MANAGER" != "unknown" ]]; then
            $PKG_INSTALL "$pkg" > /dev/null 2>&1
            if command -v "$cmd" &> /dev/null; then
                echo -e "  ${GREEN}✓${NC} $desc - 安装成功"
                return 0
            else
                echo -e "  ${RED}✗${NC} $desc - 安装失败"
                return 1
            fi
        else
            echo -e "  ${RED}✗${NC} $desc - 无法自动安装"
            return 1
        fi
    fi
}

# 主依赖检测函数
check_dependencies() {
    echo ""
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}检测并安装依赖${NC}"
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo ""
    
    detect_package_manager
    print_info "包管理器: $PKG_MANAGER"
    
    # 更新包索引
    if [[ "$PKG_MANAGER" != "unknown" ]]; then
        run_with_spinner "更新软件包索引..." $PKG_UPDATE
        echo -e "  ${GREEN}✓${NC} 软件包索引已更新"
    fi
    
    echo ""
    echo -e "${YELLOW}[核心工具]${NC}"
    
    local missing_critical=0
    
    # curl - 下载必需
    check_and_install_dep "curl" "curl" "curl (HTTP 下载)" || ((missing_critical++))
    
    # dig/nslookup - DNS 解析
    if ! command -v dig &> /dev/null; then
        case "$PKG_MANAGER" in
            apt) check_and_install_dep "dig" "dnsutils" "dig (DNS 解析)" || true ;;
            dnf|yum) check_and_install_dep "dig" "bind-utils" "dig (DNS 解析)" || true ;;
            pacman) check_and_install_dep "dig" "bind" "dig (DNS 解析)" || true ;;
            apk) check_and_install_dep "dig" "bind-tools" "dig (DNS 解析)" || true ;;
            *) echo -e "  ${YELLOW}○${NC} dig (DNS 解析) - 跳过" ;;
        esac
    else
        echo -e "  ${GREEN}✓${NC} dig (DNS 解析)"
    fi
    
    # ss/netstat - 端口检测
    if command -v ss &> /dev/null; then
        echo -e "  ${GREEN}✓${NC} ss (端口检测)"
    elif command -v netstat &> /dev/null; then
        echo -e "  ${GREEN}✓${NC} netstat (端口检测)"
    else
        case "$PKG_MANAGER" in
            apt) check_and_install_dep "ss" "iproute2" "ss (端口检测)" || true ;;
            dnf|yum) check_and_install_dep "ss" "iproute" "ss (端口检测)" || true ;;
            pacman) check_and_install_dep "ss" "iproute2" "ss (端口检测)" || true ;;
            apk) check_and_install_dep "ss" "iproute2" "ss (端口检测)" || true ;;
            *) echo -e "  ${YELLOW}○${NC} ss (端口检测) - 跳过" ;;
        esac
    fi
    
    # iptables - TUN 模式可能需要
    if ! command -v iptables &> /dev/null; then
        case "$PKG_MANAGER" in
            apt) check_and_install_dep "iptables" "iptables" "iptables (防火墙)" || true ;;
            dnf|yum) check_and_install_dep "iptables" "iptables" "iptables (防火墙)" || true ;;
            pacman) check_and_install_dep "iptables" "iptables" "iptables (防火墙)" || true ;;
            apk) check_and_install_dep "iptables" "iptables" "iptables (防火墙)" || true ;;
            *) echo -e "  ${YELLOW}○${NC} iptables (防火墙) - 跳过" ;;
        esac
    else
        echo -e "  ${GREEN}✓${NC} iptables (防火墙)"
    fi
    
    echo ""
    echo -e "${YELLOW}[系统工具]${NC}"
    
    # systemctl - 服务管理
    if command -v systemctl &> /dev/null; then
        echo -e "  ${GREEN}✓${NC} systemctl (服务管理)"
    else
        echo -e "  ${RED}✗${NC} systemctl (服务管理) - 此脚本需要 systemd"
        ((missing_critical++))
    fi
    
    # tar/gzip - 解压
    check_and_install_dep "tar" "tar" "tar (解压)" || true
    check_and_install_dep "gzip" "gzip" "gzip (解压)" || true
    
    # wget - 备用下载
    if ! command -v wget &> /dev/null; then
        check_and_install_dep "wget" "wget" "wget (备用下载)" || true
    else
        echo -e "  ${GREEN}✓${NC} wget (备用下载)"
    fi
    
    # ca-certificates - HTTPS 支持
    case "$PKG_MANAGER" in
        apt)
            if dpkg -l ca-certificates &> /dev/null; then
                echo -e "  ${GREEN}✓${NC} ca-certificates (HTTPS)"
            else
                $PKG_INSTALL ca-certificates > /dev/null 2>&1
                echo -e "  ${GREEN}✓${NC} ca-certificates (HTTPS) - 已安装"
            fi
            ;;
        dnf|yum)
            if rpm -q ca-certificates &> /dev/null; then
                echo -e "  ${GREEN}✓${NC} ca-certificates (HTTPS)"
            else
                $PKG_INSTALL ca-certificates > /dev/null 2>&1
                echo -e "  ${GREEN}✓${NC} ca-certificates (HTTPS) - 已安装"
            fi
            ;;
        *)
            echo -e "  ${YELLOW}○${NC} ca-certificates (HTTPS) - 跳过检测"
            ;;
    esac
    
    echo ""
    
    if [[ $missing_critical -gt 0 ]]; then
        print_error "有 $missing_critical 个关键依赖缺失，无法继续"
        exit 1
    fi
    
    print_success "依赖检测完成"
    echo ""
}

#===============================================================================
# 端口检测工具
#===============================================================================

is_port_in_use() {
    # 检查端口是否被占用
    local port="$1"
    if command -v ss &> /dev/null; then
        # ss 输出格式: 127.0.0.1:1080 或 *:1080 或 [::]:1080
        ss -tuln 2>/dev/null | grep -qE "[:.]${port}(\\s|$)" && return 0
    elif command -v netstat &> /dev/null; then
        # Linux netstat: 127.0.0.1:1080, macOS netstat: 127.0.0.1.1080
        netstat -tuln 2>/dev/null | grep -qE "[:.](${port})\\s" && return 0
    fi
    # 使用 /dev/tcp 检测 (bash 内置，最后手段)
    (echo >/dev/tcp/127.0.0.1/$port) 2>/dev/null && return 0
    return 1
}

get_occupied_ports() {
    # 获取当前所有已占用的端口
    if command -v ss &> /dev/null; then
        ss -tuln 2>/dev/null | grep -E 'LISTEN|ESTAB' | awk '{print $5}' | grep -oE '[0-9]+$' | sort -un
    elif command -v netstat &> /dev/null; then
        netstat -tuln 2>/dev/null | grep -i listen | awk '{print $4}' | grep -oE '[0-9]+$' | sort -un
    fi
}

find_available_port() {
    # 找到一个可用的端口，从指定端口开始
    local start_port="$1"
    local port="$start_port"
    local max_attempts=100
    
    for ((i=0; i<max_attempts; i++)); do
        if ! is_port_in_use "$port"; then
            echo "$port"
            return 0
        fi
        ((port++))
    done
    
    # 如果连续100个端口都被占用，返回原始端口
    echo "$start_port"
    return 1
}

check_and_suggest_ports() {
    # 检查默认端口并建议替代端口
    local socks_default="${1:-1080}"
    local http_default="${2:-8080}"
    
    echo ""
    echo -e "${CYAN}[端口检测]${NC}"
    
    # 显示当前已占用的常用代理端口
    local occupied=$(get_occupied_ports)
    local common_ports=(1080 8080 7890 7891 10808 10809 1081 8081)
    local occupied_common=""
    
    for p in "${common_ports[@]}"; do
        if echo "$occupied" | grep -q "^${p}$"; then
            occupied_common="${occupied_common} ${p}"
        fi
    done
    
    if [[ -n "$occupied_common" ]]; then
        print_warning "以下常用端口已被占用:$occupied_common"
    fi
    
    # 检查并建议 SOCKS5 端口
    if is_port_in_use "$socks_default"; then
        local new_socks=$(find_available_port "$socks_default")
        print_warning "SOCKS5 端口 $socks_default 已被占用，建议使用: $new_socks"
        SUGGESTED_SOCKS_PORT="$new_socks"
    else
        print_success "SOCKS5 端口 $socks_default 可用"
        SUGGESTED_SOCKS_PORT="$socks_default"
    fi
    
    # 检查并建议 HTTP 端口
    if is_port_in_use "$http_default"; then
        local new_http=$(find_available_port "$http_default")
        print_warning "HTTP 端口 $http_default 已被占用，建议使用: $new_http"
        SUGGESTED_HTTP_PORT="$new_http"
    else
        print_success "HTTP 端口 $http_default 可用"
        SUGGESTED_HTTP_PORT="$http_default"
    fi
    
    echo ""
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
        print_success "已安装: $(hysteria version 2>/dev/null | grep 'Version:' | awk '{print $2}')"
        return 0
    fi
    
    # 官方脚本 URL
    local hy2_script="https://raw.githubusercontent.com/apernet/hysteria/master/install_server.sh"
    local hy2_script_alt="https://get.hy2.sh/"
    
    # 优先使用国内镜像下载官方脚本
    if smart_run_script "$hy2_script"; then
        print_success "Hysteria2 安装完成"
        return 0
    fi
    
    # 备选: 直接尝试官方短链接 (可能需要代理)
    print_info "尝试官方安装脚本..."
    if HYSTERIA_USER=root bash <(curl -fsSL --max-time 60 "$hy2_script_alt") 2>/dev/null; then
        print_success "Hysteria2 安装完成"
        return 0
    fi
    
    print_error "Hysteria2 安装失败，请检查网络连接"
    return 1
}

install_xray_client() {
    print_info "安装 Xray..."
    
    if command -v xray &> /dev/null; then
        print_success "已安装: $(xray version 2>/dev/null | head -n1 | awk '{print $2}')"
        return 0
    fi
    
    # Xray 官方安装脚本
    local xray_script="https://raw.githubusercontent.com/XTLS/Xray-install/main/install-release.sh"
    
    # 使用智能下载执行脚本
    if smart_run_script "$xray_script" install; then
        print_success "Xray 安装完成"
        return 0
    fi
    
    print_error "Xray 安装失败，请检查网络连接"
    return 1
}

install_singbox() {
    print_info "安装 sing-box..."
    
    if command -v sing-box &> /dev/null; then
        print_success "已安装: $(sing-box version 2>/dev/null | head -n1 | awk '{print $3}')"
        return 0
    fi
    
    # 方法1: 使用 apt 源 (Debian/Ubuntu)
    if [[ "$PKG_MANAGER" == "apt" ]]; then
        start_spinner "添加 sing-box 源..."
        
        # 尝试下载 GPG 密钥
        mkdir -p /etc/apt/keyrings
        local gpg_success=false
        
        # 直接尝试 (sing-box.app 在国内通常可访问)
        if curl -fsSL --max-time 30 https://sing-box.app/gpg.key -o /etc/apt/keyrings/sagernet.asc 2>/dev/null; then
            gpg_success=true
        fi
        
        # 如果直接下载失败，尝试代理
        if [[ "$gpg_success" == "false" ]] && ss -tuln 2>/dev/null | grep -q ":1080 "; then
            if curl --socks5 127.0.0.1:1080 -fsSL --max-time 30 https://sing-box.app/gpg.key -o /etc/apt/keyrings/sagernet.asc 2>/dev/null; then
                gpg_success=true
            fi
        fi
        
        if [[ "$gpg_success" == "true" ]]; then
            chmod a+r /etc/apt/keyrings/sagernet.asc
            echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/sagernet.asc] https://deb.sagernet.org/ * *" > /etc/apt/sources.list.d/sagernet.list
            stop_spinner
            run_with_spinner "更新软件源..." apt-get update -qq
            run_with_spinner "安装 sing-box..." apt-get install -y -qq sing-box
            print_success "sing-box 安装完成"
            return 0
        fi
        stop_spinner
    fi
    
    # 方法2: 手动下载二进制文件 (使用 GitHub releases + 镜像)
    print_info "使用二进制安装..."
    local arch=$(uname -m)
    case "$arch" in
        x86_64) arch="amd64" ;;
        aarch64) arch="arm64" ;;
        armv7l) arch="armv7" ;;
    esac
    
    # 获取最新版本号 (使用镜像)
    local version=""
    local api_url="https://api.github.com/repos/SagerNet/sing-box/releases/latest"
    
    # 尝试获取版本号
    version=$(curl -fsSL --max-time 15 "$api_url" 2>/dev/null | grep '"tag_name"' | sed -E 's/.*"v([^"]+)".*/\1/')
    
    if [[ -z "$version" ]]; then
        # 使用默认版本
        version="1.10.0"
        print_warning "无法获取最新版本，使用默认版本 $version"
    fi
    
    local download_url="https://github.com/SagerNet/sing-box/releases/download/v${version}/sing-box-${version}-linux-${arch}.tar.gz"
    local tmp_file="/tmp/sing-box.tar.gz"
    
    if smart_download "$download_url" "$tmp_file" "sing-box"; then
        tar -xzf "$tmp_file" -C /tmp
        cp "/tmp/sing-box-${version}-linux-${arch}/sing-box" /usr/bin/
        chmod +x /usr/bin/sing-box
        rm -rf "$tmp_file" "/tmp/sing-box-${version}-linux-${arch}"
        print_success "sing-box 安装完成"
        return 0
    fi
    
    print_error "sing-box 安装失败，请检查网络连接"
    return 1
}

# 一次性安装所有核心
install_all_cores() {
    echo ""
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo -e "${YELLOW}[安装代理核心]${NC}"
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo ""
    
    # 加载服务端地址
    load_server_address
    if [[ -n "$SERVER_ADDRESS" ]]; then
        print_info "检测到服务端: $SERVER_ADDRESS，优先从服务端下载安装包"
    fi
    
    # 使用智能安装 (优先服务端 -> 国内镜像 -> 直连)
    smart_install_core hysteria
    smart_install_core xray  
    smart_install_core singbox
    
    echo ""
    print_success "所有核心安装完成"
    echo ""
}


generate_xray_config() {
    local xray_config="${BASE_DIR}/xray-config.json"
    local server_host=$(echo "$SERVER_ADDR" | cut -d':' -f1)
    local server_port=$(echo "$SERVER_ADDR" | cut -d':' -f2 | tr -cd '0-9')
    
    # 检测端口占用情况
    check_and_suggest_ports 1080 8080
    
    # SOCKS5 端口
    read -p "SOCKS5 端口 [默认 ${SUGGESTED_SOCKS_PORT}]: " SOCKS_PORT
    SOCKS_PORT=${SOCKS_PORT:-$SUGGESTED_SOCKS_PORT}
    
    if is_port_in_use "$SOCKS_PORT"; then
        print_warning "端口 $SOCKS_PORT 已被占用，可能会冲突！"
    fi
    
    # HTTP 端口
    read -p "HTTP 端口 [默认 ${SUGGESTED_HTTP_PORT}]: " HTTP_PORT
    HTTP_PORT=${HTTP_PORT:-$SUGGESTED_HTTP_PORT}
    
    if is_port_in_use "$HTTP_PORT"; then
        print_warning "端口 $HTTP_PORT 已被占用，可能会冲突！"
    fi
    
    cat > "$xray_config" << EOF
{
  "log": {"loglevel": "warning"},
  "inbounds": [
    {"tag": "socks", "port": ${SOCKS_PORT}, "listen": "127.0.0.1", "protocol": "socks", "settings": {"udp": true}},
    {"tag": "http", "port": ${HTTP_PORT}, "listen": "127.0.0.1", "protocol": "http"}
  ],
  "outbounds": [
    {
      "tag": "proxy",
      "protocol": "vless",
      "settings": {
        "vnext": [{
          "address": "${server_host}",
          "port": ${server_port},
          "users": [{"id": "${UUID}", "flow": "${FLOW}", "encryption": "none"}]
        }]
      },
      "streamSettings": {
        "network": "${NETWORK}",
        "security": "reality",
        "realitySettings": {
          "serverName": "${SNI}",
          "fingerprint": "${FINGERPRINT}",
          "publicKey": "${PUBLIC_KEY}",
          "shortId": "${SHORT_ID}",
          "spiderX": "/"
        }
      }
    },
    {"tag": "direct", "protocol": "freedom"},
    {"tag": "block", "protocol": "blackhole"}
  ],
  "routing": {
    "domainStrategy": "IPIfNonMatch",
    "rules": [
      {"type": "field", "domain": ["geosite:private"], "outboundTag": "direct"},
      {"type": "field", "ip": ["geoip:private"], "outboundTag": "direct"},
      {"type": "field", "domain": ["geosite:cn"], "outboundTag": "direct"},
      {"type": "field", "ip": ["geoip:cn"], "outboundTag": "direct"},
      {"type": "field", "domain": ["keyword:wechat", "keyword:weixin", "keyword:tencent", "keyword:qq", "keyword:xiaohongshu", "keyword:douyin", "keyword:bytedance", "keyword:toutiao", "keyword:kuaishou", "keyword:bilibili", "keyword:taobao", "keyword:alipay", "keyword:alibaba", "keyword:tmall", "keyword:jd", "keyword:baidu"], "outboundTag": "direct"}
    ]
  }
}
EOF
    chmod 644 "$xray_config"
    print_success "Xray 配置已生成: $xray_config"
    
    # 创建 Xray 客户端服务
    cat > /etc/systemd/system/xray-client.service << EOF
[Unit]
Description=Xray Client
After=network.target

[Service]
Type=simple
ExecStart=/usr/local/bin/xray run -config ${xray_config}
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF
    systemctl daemon-reload
    systemctl enable xray-client 2>/dev/null || true
    systemctl start xray-client
    
    echo ""
    print_success "Xray 客户端已启动"
    echo -e "  SOCKS5: ${GREEN}127.0.0.1:${SOCKS_PORT}${NC}"
    echo -e "  HTTP:   ${GREEN}127.0.0.1:${HTTP_PORT}${NC}"
}

#===============================================================================
# sing-box TUN 配置生成 (解决 DNS 污染问题)
#===============================================================================

generate_singbox_tun_config() {
    local protocol="$1"  # hysteria2 或 vless-reality
    local singbox_config="${BASE_DIR}/singbox-tun.json"
    
    print_info "生成 sing-box TUN 配置..."
    
    # 解析服务器信息 (移除端口号中的非数字字符，如尾部斜杠)
    local server_host=$(echo "$SERVER_ADDR" | cut -d':' -f1)
    local server_port=$(echo "$SERVER_ADDR" | cut -d':' -f2 | tr -cd '0-9')
    
    # 根据协议生成不同的 outbound
    local outbound_config=""
    
    # JSON 转义敏感字段
    local safe_password=$(json_escape "${AUTH_PASSWORD}")
    local safe_server=$(json_escape "${server_host}")
    local safe_sni=$(json_escape "${SNI:-$server_host}")
    local safe_uuid=$(json_escape "${UUID}")
    local safe_pubkey=$(json_escape "${PUBLIC_KEY}")
    local safe_shortid=$(json_escape "${SHORT_ID}")
    
    if [[ "$protocol" == "hysteria2" ]]; then
        # 端口跳跃配置 (sing-box 1.11+ 使用 server_ports 格式，范围用冒号分隔)
        local hop_config=""
        if [[ -n "$MPORT" ]]; then
            # 将连字符转换为冒号 (例如: 20000-30000 → 20000:30000)
            local singbox_ports="${MPORT//-/:}"
            hop_config=",
      \"server_ports\": \"${singbox_ports}\",
      \"hop_interval\": \"30s\""
        fi
        
        outbound_config=$(cat <<OUTBOUND
    {
      "type": "hysteria2",
      "tag": "proxy-out",
      "server": "${safe_server}",
      "server_port": ${server_port},
      "password": "${safe_password}"${hop_config},
      "tls": {
        "enabled": true,
        "server_name": "${safe_sni}",
        "insecure": ${INSECURE:-false}
      }
    }
OUTBOUND
)
    elif [[ "$protocol" == "vless-reality" ]]; then
        outbound_config=$(cat <<OUTBOUND
    {
      "type": "vless",
      "tag": "proxy-out",
      "server": "${safe_server}",
      "server_port": ${server_port},
      "uuid": "${safe_uuid}",
      "flow": "${FLOW:-xtls-rprx-vision}",
      "tls": {
        "enabled": true,
        "server_name": "${safe_sni}",
        "utls": {
          "enabled": true,
          "fingerprint": "${FINGERPRINT:-chrome}"
        },
        "reality": {
          "enabled": true,
          "public_key": "${safe_pubkey}",
          "short_id": "${safe_shortid}"
        }
      }
    }
OUTBOUND
)
    fi
    
    # 生成完整 sing-box 配置
    cat > "$singbox_config" <<EOF
{
  "log": {
    "level": "info"
  },
  "dns": {
    "servers": [
      {
        "tag": "proxy-dns",
        "address": "https://8.8.8.8/dns-query",
        "address_resolver": "local-dns",
        "detour": "proxy-out"
      },
      {
        "tag": "local-dns",
        "address": "223.5.5.5",
        "detour": "direct-out"
      }
    ],
    "rules": [
      {
        "domain_suffix": [".cn"],
        "server": "local-dns"
      }
    ],
    "final": "proxy-dns",
    "strategy": "ipv4_only"
  },
  "inbounds": [
    {
      "type": "tun",
      "tag": "tun-in",
      "interface_name": "bui-tun",
      "address": ["172.19.0.1/30"],
      "mtu": 1400,
      "auto_route": true,
      "strict_route": true,
      "stack": "system",
      "sniff": true,
      "sniff_override_destination": true
    },
    {
      "type": "socks",
      "tag": "socks-in",
      "listen": "127.0.0.1",
      "listen_port": ${SOCKS_PORT}
    },
    {
      "type": "socks",
      "tag": "antigravity-socks",
      "listen": "127.0.0.1",
      "listen_port": 54321
    },
    {
      "type": "http",
      "tag": "http-in",
      "listen": "127.0.0.1",
      "listen_port": ${HTTP_PORT}
    }
  ],
  "outbounds": [
${outbound_config},
    {
      "type": "direct",
      "tag": "direct-out"
    }
  ],
  "route": {
    "rules": [
      {
        "protocol": "dns",
        "action": "hijack-dns"
      },
      {
        "port": [22, 2222],
        "action": "route",
        "outbound": "direct-out"
      },
      {
        "ip_is_private": true,
        "action": "route",
        "outbound": "direct-out"
      },
      {
        "domain_keyword": ["wechat", "weixin", "tencent", "qq", "xiaohongshu", "douyin", "bytedance", "toutiao", "kuaishou", "bilibili", "taobao", "alipay", "alibaba", "tmall", "jd", "baidu"],
        "action": "route",
        "outbound": "direct-out"
      }
    ],
    "final": "proxy-out",
    "auto_detect_interface": true
  }
}
EOF
    chmod 644 "$singbox_config"
    
    # 创建 sing-box TUN 服务
    cat > /etc/systemd/system/bui-tun.service <<EOF
[Unit]
Description=B-UI TUN Mode (sing-box)
After=network.target
Wants=network.target

[Service]
Type=simple
ExecStart=/usr/bin/sing-box run -c ${singbox_config}
Restart=always
RestartSec=3

[Install]
WantedBy=multi-user.target
EOF
    systemctl daemon-reload
    
    print_success "sing-box TUN 配置已生成: $singbox_config"
}

start_tun_mode() {
    print_info "启动 TUN 模式..."
    
    # 检测端口冲突
    local socks_port=${SOCKS_PORT:-1080}
    local http_port=${HTTP_PORT:-8080}
    
    # 检测并停止可能冲突的服务
    if systemctl is-active --quiet hysteria-client; then
        print_warning "检测到 Hysteria2 客户端正在运行 (占用端口 $socks_port/$http_port)"
        print_info "停止 Hysteria2 客户端..."
        systemctl stop hysteria-client 2>/dev/null || true
        sleep 1
    fi
    
    if systemctl is-active --quiet xray-client; then
        print_warning "检测到 Xray 客户端正在运行"
        print_info "停止 Xray 客户端..."
        systemctl stop xray-client 2>/dev/null || true
        sleep 1
    fi
    
    # 再次检查端口占用
    if ss -tlnp | grep -q ":${socks_port} " 2>/dev/null; then
        print_warning "端口 $socks_port 仍被占用"
        local pid=$(ss -tlnp | grep ":${socks_port} " | grep -oP 'pid=\K\d+')
        if [[ -n "$pid" ]]; then
            print_info "尝试终止占用进程 PID: $pid"
            kill -9 "$pid" 2>/dev/null || true
            sleep 1
        fi
    fi
    
    # 删除旧的 TUN 接口
    ip link delete hystun 2>/dev/null || true
    ip link delete bui-tun 2>/dev/null || true
    
    # 设置 rp_filter (Linux TUN 需要)
    sysctl -w net.ipv4.conf.all.rp_filter=2 >/dev/null 2>&1 || true
    sysctl -w net.ipv4.conf.default.rp_filter=2 >/dev/null 2>&1 || true
    
    # 启动 sing-box TUN
    systemctl daemon-reload 2>/dev/null || true
    systemctl enable bui-tun 2>/dev/null || true
    systemctl start bui-tun
    
    sleep 2
    if systemctl is-active --quiet bui-tun; then
        print_success "TUN 模式已启动"
        echo -e "  接口: ${GREEN}bui-tun${NC}"
        echo -e "  SOCKS5: ${GREEN}127.0.0.1:${socks_port}${NC}"
        echo -e "  HTTP:   ${GREEN}127.0.0.1:${http_port}${NC}"
    else
        print_error "TUN 模式启动失败"
        journalctl -u bui-tun --no-pager -n 10
    fi
}

stop_tun_mode() {
    print_info "停止 TUN 模式..."
    systemctl stop bui-tun 2>/dev/null || true
    ip link delete bui-tun 2>/dev/null || true
    print_success "TUN 模式已停止"
}

#===============================================================================
# URI 解析和导入
#===============================================================================


parse_hysteria_uri() {
    local uri="$1"
    
    # 验证 URI 格式
    if [[ ! "$uri" =~ ^(hysteria2|hy2):// ]]; then
        return 1
    fi
    
    # 移除协议前缀
    local content="${uri#*://}"
    
    # 提取备注名 (#后面的部分)
    local remark=""
    if [[ "$content" =~ \#(.+)$ ]]; then
        remark=$(echo "${BASH_REMATCH[1]}" | sed 's/%20/ /g' | sed 's/+/ /g')
        # URL 解码
        remark=$(echo -e "${remark//%/\\x}")
        content="${content%%#*}"
    fi
    
    # 提取密码和服务器 (password@host:port)
    local auth_part="${content%%\?*}"
    local password="${auth_part%%@*}"
    local server_part="${auth_part#*@}"
    
    # URL 解码密码
    password=$(echo -e "${password//%/\\x}")
    
    # 提取查询参数
    local query_part=""
    [[ "$content" =~ \?(.+) ]] && query_part="${BASH_REMATCH[1]}"
    
    # 解析 SNI
    local sni=""
    [[ "$query_part" =~ sni=([^&]+) ]] && sni="${BASH_REMATCH[1]}"
    
    # 解析 insecure
    local insecure="false"
    [[ "$query_part" =~ insecure=1 ]] && insecure="true"
    
    # 解析 mport (端口跳跃)
    local mport=""
    [[ "$query_part" =~ mport=([^&]+) ]] && mport="${BASH_REMATCH[1]}"
    
    # 输出解析结果
    echo "PROTOCOL=hysteria2"
    echo "SERVER_ADDR=$server_part"
    echo "AUTH_PASSWORD=$password"
    echo "SNI=$sni"
    echo "INSECURE=$insecure"
    echo "MPORT=$mport"
    echo "REMARK=$remark"
}

parse_vless_uri() {
    local uri="$1"
    
    # 验证 URI 格式
    if [[ ! "$uri" =~ ^vless:// ]]; then
        return 1
    fi
    
    # 移除协议前缀
    local content="${uri#vless://}"
    
    # 提取备注名 (#后面的部分)
    local remark=""
    if [[ "$content" =~ \#(.+)$ ]]; then
        remark=$(echo "${BASH_REMATCH[1]}" | sed 's/%20/ /g' | sed 's/+/ /g')
        remark=$(echo -e "${remark//%/\\x}")
        content="${content%%#*}"
    fi
    
    # 提取 UUID 和服务器 (uuid@host:port)
    local auth_part="${content%%\?*}"
    local uuid="${auth_part%%@*}"
    local server_part="${auth_part#*@}"
    
    # 提取查询参数
    local query_part=""
    [[ "$content" =~ \?(.+) ]] && query_part="${BASH_REMATCH[1]}"
    
    # 解析各参数
    local security="" sni="" fp="" pbk="" sid="" flow="" type=""
    [[ "$query_part" =~ security=([^&]+) ]] && security="${BASH_REMATCH[1]}"
    [[ "$query_part" =~ sni=([^&]+) ]] && sni="${BASH_REMATCH[1]}"
    [[ "$query_part" =~ fp=([^&]+) ]] && fp="${BASH_REMATCH[1]}"
    [[ "$query_part" =~ pbk=([^&]+) ]] && pbk="${BASH_REMATCH[1]}"
    [[ "$query_part" =~ sid=([^&]+) ]] && sid="${BASH_REMATCH[1]}"
    [[ "$query_part" =~ flow=([^&]+) ]] && flow="${BASH_REMATCH[1]}"
    [[ "$query_part" =~ type=([^&]+) ]] && type="${BASH_REMATCH[1]}"
    
    # 输出解析结果
    echo "PROTOCOL=vless-reality"
    echo "SERVER_ADDR=$server_part"
    echo "UUID=$uuid"
    echo "SECURITY=$security"
    echo "SNI=$sni"
    echo "FINGERPRINT=${fp:-chrome}"
    echo "PUBLIC_KEY=$pbk"
    echo "SHORT_ID=$sid"
    echo "FLOW=${flow:-xtls-rprx-vision}"
    echo "NETWORK=${type:-tcp}"
    echo "REMARK=$remark"
}

#===============================================================================
# 多配置管理
#===============================================================================

get_active_config() {
    # 获取当前激活的配置名
    if [[ -f "$ACTIVE_CONFIG" ]]; then
        cat "$ACTIVE_CONFIG"
    else
        echo ""
    fi
}

save_config_meta() {
    # 保存配置元信息
    local config_name="$1"
    local protocol="$2"
    local server="$3"
    local uri="$4"
    
    local config_dir="${CONFIGS_DIR}/${config_name}"
    mkdir -p "$config_dir"
    
    # 保存元信息
    cat > "${config_dir}/meta.json" << EOF
{
    "name": "${config_name}",
    "protocol": "${protocol}",
    "server": "${server}",
    "createdAt": "$(date -Iseconds)"
}
EOF
    
    # 保存原始 URI
    echo "$uri" > "${config_dir}/uri.txt"
    
    # 保存服务端地址 (用于从服务端下载安装包)
    # 提取服务器域名/IP (去掉端口)
    local server_host=$(echo "$server" | cut -d':' -f1)
    if [[ -n "$server_host" ]]; then
        set_server_address "$server_host"
    fi
}

list_configs() {
    echo ""
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}已保存的配置${NC}"
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo ""
    
    if [[ ! -d "$CONFIGS_DIR" ]] || [[ -z "$(ls -A "$CONFIGS_DIR" 2>/dev/null)" ]]; then
        print_warning "没有保存的配置"
        return 1
    fi
    
    local active=$(get_active_config)
    local index=1
    
    for config_dir in "$CONFIGS_DIR"/*/; do
        [[ ! -d "$config_dir" ]] && continue
        local name=$(basename "$config_dir")
        local meta_file="${config_dir}meta.json"
        
        local protocol="未知"
        local server="未知"
        if [[ -f "$meta_file" ]]; then
            protocol=$(grep '"protocol"' "$meta_file" | cut -d'"' -f4)
            server=$(grep '"server"' "$meta_file" | cut -d'"' -f4)
        fi
        
        # 标记当前激活的配置
        if [[ "$name" == "$active" ]]; then
            echo -e "  ${YELLOW}${index}.${NC} ${GREEN}★ ${name}${NC} ${CYAN}[当前]${NC}"
        else
            echo -e "  ${YELLOW}${index}.${NC} ${name}"
        fi
        echo -e "     协议: ${protocol} | 服务器: ${server}"
        echo ""
        ((index++))
    done
    
    return 0
}

switch_config() {
    echo ""
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}切换配置${NC}"
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    
    if ! list_configs; then
        return 1
    fi
    
    # 获取配置列表
    local configs=()
    for config_dir in "$CONFIGS_DIR"/*/; do
        [[ -d "$config_dir" ]] && configs+=("$(basename "$config_dir")")
    done
    
    echo ""
    read -p "选择配置编号 (0 返回): " choice
    
    if [[ "$choice" == "0" ]] || [[ -z "$choice" ]]; then
        return 0
    fi
    
    if [[ ! "$choice" =~ ^[0-9]+$ ]] || [[ "$choice" -lt 1 ]] || [[ "$choice" -gt ${#configs[@]} ]]; then
        print_error "无效选择"
        return 1
    fi
    
    local selected="${configs[$((choice-1))]}"
    local config_dir="${CONFIGS_DIR}/${selected}"
    
    print_info "切换到配置: $selected"
    
    # 读取配置信息
    local meta_file="${config_dir}/meta.json"
    local protocol=$(grep '"protocol"' "$meta_file" 2>/dev/null | cut -d'"' -f4)
    
    # 停止所有服务
    systemctl stop "$CLIENT_SERVICE" 2>/dev/null || true
    systemctl stop xray-client 2>/dev/null || true
    
    if [[ "$protocol" == "hysteria2" ]]; then
        # 复制 Hysteria2 配置
        if [[ -f "${config_dir}/config.yaml" ]]; then
            cp "${config_dir}/config.yaml" "$CONFIG_FILE"
            create_service
            systemctl start "$CLIENT_SERVICE"
        else
            print_error "配置文件不存在"
            return 1
        fi
    else
        # 复制 Xray 配置
        if [[ -f "${config_dir}/xray-config.json" ]]; then
            cp "${config_dir}/xray-config.json" "${BASE_DIR}/xray-config.json"
            systemctl start xray-client
        else
            print_error "配置文件不存在"
            return 1
        fi
    fi
    
    # 更新激活配置
    echo "$selected" > "$ACTIVE_CONFIG"
    
    print_success "已切换到: $selected"
}

delete_config() {
    echo ""
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo -e "${RED}删除配置${NC}"
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    
    if ! list_configs; then
        return 1
    fi
    
    # 获取配置列表
    local configs=()
    for config_dir in "$CONFIGS_DIR"/*/; do
        [[ -d "$config_dir" ]] && configs+=("$(basename "$config_dir")")
    done
    
    echo ""
    read -p "选择要删除的配置编号 (0 返回): " choice
    
    if [[ "$choice" == "0" ]] || [[ -z "$choice" ]]; then
        return 0
    fi
    
    if [[ ! "$choice" =~ ^[0-9]+$ ]] || [[ "$choice" -lt 1 ]] || [[ "$choice" -gt ${#configs[@]} ]]; then
        print_error "无效选择"
        return 1
    fi
    
    local selected="${configs[$((choice-1))]}"
    local active=$(get_active_config)
    
    if [[ "$selected" == "$active" ]]; then
        print_warning "无法删除当前激活的配置，请先切换到其他配置"
        return 1
    fi
    
    read -p "确认删除 '$selected'? (y/n): " confirm
    if [[ "$confirm" =~ ^[yY]$ ]]; then
        rm -rf "${CONFIGS_DIR}/${selected}"
        print_success "已删除: $selected"
    else
        print_info "已取消"
    fi
}

import_batch() {
    echo ""
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}批量导入配置${NC}"
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo ""
    echo -e "支持格式:"
    echo -e "  ${YELLOW}hysteria2://password@host:port/?sni=xxx#备注${NC}"
    echo -e "  ${YELLOW}vless://uuid@host:port/?security=reality&sni=xxx&pbk=xxx#备注${NC}"
    echo ""
    echo -e "${YELLOW}请粘贴配置链接 (每行一个，输入空行结束):${NC}"
    echo ""
    
    local uris=()
    while IFS= read -r line; do
        # 空行结束输入
        [[ -z "$line" ]] && break
        # 跳过注释
        [[ "$line" =~ ^# ]] && continue
        # 去除首尾空格
        line=$(echo "$line" | xargs)
        [[ -n "$line" ]] && uris+=("$line")
    done
    
    if [[ ${#uris[@]} -eq 0 ]]; then
        print_warning "没有输入任何链接"
        return 1
    fi
    
    echo ""
    print_info "检测到 ${#uris[@]} 个链接，开始解析..."
    echo ""
    
    local success_count=0
    local fail_count=0
    
    for uri in "${uris[@]}"; do
        local parsed=""
        local protocol=""
        
        if [[ "$uri" =~ ^(hysteria2|hy2):// ]]; then
            parsed=$(parse_hysteria_uri "$uri")
            protocol="hysteria2"
        elif [[ "$uri" =~ ^vless:// ]]; then
            parsed=$(parse_vless_uri "$uri")
            protocol="vless-reality"
        else
            echo -e "  ${RED}✗${NC} 不支持的格式: ${uri:0:50}..."
            ((fail_count++))
            continue
        fi
        
        if [[ -z "$parsed" ]]; then
            echo -e "  ${RED}✗${NC} 解析失败: ${uri:0:50}..."
            ((fail_count++))
            continue
        fi
        
        # 导入解析结果
        eval "$parsed"
        
        # 使用备注名或生成配置名
        local config_name="${REMARK:-config-$(date +%s)}"
        # 清理配置名中的特殊字符
        config_name=$(echo "$config_name" | sed 's/[\/\\:*?"<>|]/-/g')
        
        # 检查是否已存在
        if [[ -d "${CONFIGS_DIR}/${config_name}" ]]; then
            echo -e "  ${YELLOW}○${NC} 已存在: ${config_name} (跳过)"
            continue
        fi
        
        # 保存配置元信息
        save_config_meta "$config_name" "$protocol" "$SERVER_ADDR" "$uri"
        
        echo -e "  ${GREEN}✓${NC} ${config_name} (${protocol})"
        ((success_count++))
    done
    
    echo ""
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo -e "导入完成: ${GREEN}成功 ${success_count}${NC} / ${RED}失败 ${fail_count}${NC}"
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    
    if [[ $success_count -gt 0 ]]; then
        echo ""
        read -p "是否现在选择一个配置并启用? (y/n): " activate
        if [[ "$activate" =~ ^[yY]$ ]]; then
            activate_imported_config
        fi
    fi
}

activate_imported_config() {
    # 让用户选择并完整配置一个导入的配置
    if ! list_configs; then
        return 1
    fi
    
    # 获取配置列表
    local configs=()
    for config_dir in "$CONFIGS_DIR"/*/; do
        [[ -d "$config_dir" ]] && configs+=("$(basename "$config_dir")")
    done
    
    echo ""
    read -p "选择要启用的配置编号: " choice
    
    if [[ ! "$choice" =~ ^[0-9]+$ ]] || [[ "$choice" -lt 1 ]] || [[ "$choice" -gt ${#configs[@]} ]]; then
        print_error "无效选择"
        return 1
    fi
    
    local selected="${configs[$((choice-1))]}"
    local config_dir="${CONFIGS_DIR}/${selected}"
    local uri_file="${config_dir}/uri.txt"
    
    if [[ ! -f "$uri_file" ]]; then
        print_error "无法找到原始配置链接"
        return 1
    fi
    
    local uri=$(cat "$uri_file")
    
    # 完整配置流程 (设置端口、TUN等)
    _configure_and_save "$uri" "$selected" "$config_dir"
}

_configure_and_save() {
    # 内部函数：完整配置并保存
    local uri="$1"
    local config_name="$2"
    local config_dir="$3"
    
    # 解析 URI
    local parsed=""
    if [[ "$uri" =~ ^(hysteria2|hy2):// ]]; then
        parsed=$(parse_hysteria_uri "$uri")
    elif [[ "$uri" =~ ^vless:// ]]; then
        parsed=$(parse_vless_uri "$uri")
    fi
    
    eval "$parsed"
    
    mkdir -p "$BASE_DIR"
    mkdir -p "$config_dir"
    
    if [[ "$PROTOCOL" == "hysteria2" ]]; then
        # Hysteria2 配置
        check_and_suggest_ports 1080 8080
        
        read -p "SOCKS5 端口 [默认 ${SUGGESTED_SOCKS_PORT}]: " SOCKS_PORT
        SOCKS_PORT=${SOCKS_PORT:-$SUGGESTED_SOCKS_PORT}
        
        read -p "HTTP 端口 [默认 ${SUGGESTED_HTTP_PORT}]: " HTTP_PORT
        HTTP_PORT=${HTTP_PORT:-$SUGGESTED_HTTP_PORT}
        
        read -p "启用 TUN 模式? (y/n) [默认 n]: " enable_tun
        TUN_ENABLED="false"
        [[ "$enable_tun" =~ ^[yY]$ ]] && TUN_ENABLED="true"
        
        # 带宽设置
        echo ""
        echo -e "${YELLOW}[带宽设置]${NC} (可选，直接回车跳过)"
        read -p "上行带宽 (Mbps): " BANDWIDTH_UP
        read -p "下行带宽 (Mbps): " BANDWIDTH_DOWN
        
        # 安装 Hysteria2
        install_hysteria
        
        create_default_rules
        generate_config
        
        # 复制配置到配置目录
        cp "$CONFIG_FILE" "${config_dir}/config.yaml"
        
        create_service
        systemctl start "$CLIENT_SERVICE"
        
        # 如果选择了启用 TUN 模式，生成配置并启动
        if [[ "$TUN_ENABLED" == "true" ]]; then
            generate_singbox_tun_config "hysteria2"
            start_tun_mode
        fi
    else
        # VLESS-Reality 配置
        install_xray_client
        generate_xray_config
        
        # 询问是否启用 TUN 模式
        read -p "启用 TUN 模式? (y/n) [默认 n]: " enable_tun
        TUN_ENABLED="false"
        [[ "$enable_tun" =~ ^[yY]$ ]] && TUN_ENABLED="true"
        
        # 复制配置到配置目录
        cp "${BASE_DIR}/xray-config.json" "${config_dir}/xray-config.json"
        
        # 如果选择了启用 TUN 模式，生成配置并启动
        if [[ "$TUN_ENABLED" == "true" ]]; then
            generate_singbox_tun_config "vless-reality"
            start_tun_mode
        fi
    fi
    
    # 更新激活配置
    echo "$config_name" > "$ACTIVE_CONFIG"
    
    print_success "配置 '$config_name' 已启用"
}

config_management() {
    while true; do
        echo ""
        echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
        echo -e "${GREEN}配置管理${NC}"
        echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
        echo ""
        echo -e "  ${YELLOW}1.${NC} 查看所有配置"
        echo -e "  ${YELLOW}2.${NC} 切换配置"
        echo -e "  ${YELLOW}3.${NC} 删除配置"
        echo -e "  ${YELLOW}0.${NC} 返回主菜单"
        echo ""
        read -p "请选择 [0-3]: " mgmt_choice
        
        case $mgmt_choice in
            1) list_configs ;;
            2) switch_config ;;
            3) delete_config ;;
            0) return ;;
            *) print_error "无效选项" ;;
        esac
        
        echo ""
        read -p "按 Enter 继续..."
    done
}

#===============================================================================
# 订阅导入 (sing-box / Clash 融合配置)
#===============================================================================

import_from_subscription() {
    echo ""
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}从服务端导入订阅 (Hy2 + VLESS 自动切换)${NC}"
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo ""
    
    # 检查是否已配置服务端地址
    load_server_address
    if [[ -z "$SERVER_ADDRESS" ]]; then
        print_warning "尚未配置服务端地址"
        read -p "请输入服务端地址 (如 example.com:8080): " SERVER_ADDRESS
        if [[ -z "$SERVER_ADDRESS" ]]; then
            print_error "服务端地址不能为空"
            return 1
        fi
        set_server_address "$SERVER_ADDRESS"
    fi
    
    echo -e "服务端地址: ${GREEN}${SERVER_ADDRESS}${NC}"
    echo ""
    
    # 询问用户名
    read -p "请输入您的用户名: " username
    if [[ -z "$username" ]]; then
        print_error "用户名不能为空"
        return 1
    fi
    
    # 选择订阅格式
    echo ""
    echo -e "${YELLOW}选择订阅格式:${NC}"
    echo -e "  ${YELLOW}1.${NC} sing-box (推荐 - 原生 TUN 支持)"
    echo -e "  ${YELLOW}2.${NC} Clash (兼容更多客户端)"
    echo ""
    read -p "请选择 [1-2]: " sub_format
    
    local sub_url=""
    local sub_file=""
    local encoded_username=$(echo -n "$username" | jq -sRr @uri 2>/dev/null || python3 -c "import urllib.parse; print(urllib.parse.quote('$username'))" 2>/dev/null || echo "$username")
    
    case $sub_format in
        1)
            sub_url="https://${SERVER_ADDRESS}/api/subscription/${encoded_username}"
            sub_file="${BASE_DIR}/singbox-subscription.json"
            ;;
        2)
            sub_url="https://${SERVER_ADDRESS}/api/clash/${encoded_username}"
            sub_file="${BASE_DIR}/clash-subscription.yaml"
            ;;
        *)
            print_error "无效选项"
            return 1
            ;;
    esac
    
    # 下载订阅
    print_info "正在下载订阅配置..."
    echo "  URL: $sub_url"
    
    mkdir -p "$BASE_DIR"
    
    if curl -fsSL --max-time 30 -k "$sub_url" -o "$sub_file" 2>/dev/null; then
        local file_size=$(wc -c < "$sub_file" 2>/dev/null || echo "0")
        if [[ "$file_size" -gt 100 ]]; then
            print_success "订阅下载成功 (${file_size} 字节)"
            echo ""
            
            if [[ "$sub_format" == "1" ]]; then
                # sing-box 配置
                print_info "配置 sing-box TUN 模式..."
                
                # 备份原来的配置
                [[ -f "${BASE_DIR}/singbox-tun.json" ]] && cp "${BASE_DIR}/singbox-tun.json" "${BASE_DIR}/singbox-tun.json.bak"
                
                # 使用订阅配置
                cp "$sub_file" "${BASE_DIR}/singbox-tun.json"
                
                echo ""
                echo -e "${GREEN}════════════════════════════════════════════════════════════════${NC}"
                echo -e "${GREEN}  订阅配置导入成功！${NC}"
                echo -e "${GREEN}════════════════════════════════════════════════════════════════${NC}"
                echo ""
                echo -e "  配置类型: ${CYAN}sing-box (Hy2 + VLESS 自动切换)${NC}"
                echo -e "  故障切换: ${GREEN}每 10 秒检测，断连自动切到备用协议${NC}"
                echo ""
                echo -e "  ${YELLOW}下一步:${NC}"
                echo -e "    • 使用菜单选项 ${YELLOW}8${NC} 开启 TUN 模式"
                echo -e "    • 开启后所有流量将自动走代理"
                echo ""
                
                read -p "是否现在开启 TUN 模式? [Y/n]: " start_tun
                if [[ -z "$start_tun" || "$start_tun" =~ ^[yY]$ ]]; then
                    toggle_tun
                fi
            else
                # Clash 配置
                echo ""
                echo -e "${GREEN}════════════════════════════════════════════════════════════════${NC}"
                echo -e "${GREEN}  Clash 订阅下载成功！${NC}"
                echo -e "${GREEN}════════════════════════════════════════════════════════════════${NC}"
                echo ""
                echo -e "  配置文件: ${CYAN}${sub_file}${NC}"
                echo -e "  故障切换: ${GREEN}fallback 策略组，10 秒检测间隔${NC}"
                echo ""
                echo -e "  ${YELLOW}使用方法:${NC}"
                echo -e "    1. 将配置文件导入到 Clash/v2rayN/Shadowrocket"
                echo -e "    2. 选择 '自动切换' 策略组"
                echo ""
                echo "  配置预览:"
                head -30 "$sub_file"
                echo "  ..."
            fi
        else
            print_error "下载的文件太小，可能是无效响应"
            cat "$sub_file"
            rm -f "$sub_file"
            return 1
        fi
    else
        print_error "下载订阅失败，请检查:"
        echo "  • 服务端地址是否正确: $SERVER_ADDRESS"
        echo "  • 用户名是否存在: $username"
        echo "  • 网络是否畅通"
        return 1
    fi
}

import_from_uri() {
    echo ""
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}从链接导入配置${NC}"
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo ""
    echo -e "支持格式:"
    echo -e "  ${YELLOW}hysteria2://password@host:port/?sni=xxx#备注${NC}"
    echo -e "  ${YELLOW}vless://uuid@host:port/?security=reality&sni=xxx&pbk=xxx#备注${NC}"
    echo ""
    
    read -p "请粘贴配置链接: " uri
    
    if [[ -z "$uri" ]]; then
        print_error "链接不能为空"
        return 1
    fi
    
    # 尝试解析 URI
    local parsed=""
    if [[ "$uri" =~ ^(hysteria2|hy2):// ]]; then
        parsed=$(parse_hysteria_uri "$uri")
    elif [[ "$uri" =~ ^vless:// ]]; then
        parsed=$(parse_vless_uri "$uri")
    else
        print_error "不支持的链接格式"
        return 1
    fi
    
    if [[ $? -ne 0 || -z "$parsed" ]]; then
        print_error "链接解析失败"
        return 1
    fi
    
    # 导入解析结果
    eval "$parsed"
    
    echo ""
    print_success "解析成功！"
    echo -e "  协议:   ${GREEN}${PROTOCOL}${NC}"
    echo -e "  服务器: ${GREEN}${SERVER_ADDR}${NC}"
    if [[ "$PROTOCOL" == "hysteria2" ]]; then
        echo -e "  密码:   ${GREEN}${AUTH_PASSWORD}${NC}"
    else
        echo -e "  UUID:   ${GREEN}${UUID}${NC}"
        echo -e "  公钥:   ${GREEN}${PUBLIC_KEY}${NC}"
    fi
    [[ -n "$SNI" ]] && echo -e "  SNI:    ${GREEN}${SNI}${NC}"
    [[ -n "$REMARK" ]] && echo -e "  备注:   ${GREEN}${REMARK}${NC}"
    echo ""
    
    # 生成配置名
    local config_name="${REMARK:-config-$(date +%s)}"
    config_name=$(echo "$config_name" | sed 's/[\/\\:*?"<>|]/-/g')
    
    # 如果已存在，询问是否覆盖
    if [[ -d "${CONFIGS_DIR}/${config_name}" ]]; then
        print_warning "配置 '$config_name' 已存在"
        read -p "覆盖现有配置? (y/n): " overwrite
        if [[ "$overwrite" != "y" ]]; then
            read -p "请输入新的配置名: " config_name
            config_name=$(echo "$config_name" | sed 's/[\/\\:*?"<>|]/-/g')
        fi
    fi
    
    local config_dir="${CONFIGS_DIR}/${config_name}"
    mkdir -p "$BASE_DIR"
    mkdir -p "$config_dir"
    
    # 保存配置元信息和原始 URI
    save_config_meta "$config_name" "$PROTOCOL" "$SERVER_ADDR" "$uri"
    
    if [[ "$PROTOCOL" == "hysteria2" ]]; then
        # Hysteria2 配置 - 检测端口占用情况
        check_and_suggest_ports 1080 8080
        
        read -p "SOCKS5 端口 [默认 ${SUGGESTED_SOCKS_PORT}]: " SOCKS_PORT
        SOCKS_PORT=${SOCKS_PORT:-$SUGGESTED_SOCKS_PORT}
        
        # 验证用户输入的端口
        if is_port_in_use "$SOCKS_PORT"; then
            print_warning "端口 $SOCKS_PORT 已被占用，可能会冲突！"
        fi
        
        read -p "HTTP 端口 [默认 ${SUGGESTED_HTTP_PORT}]: " HTTP_PORT
        HTTP_PORT=${HTTP_PORT:-$SUGGESTED_HTTP_PORT}
        
        if is_port_in_use "$HTTP_PORT"; then
            print_warning "端口 $HTTP_PORT 已被占用，可能会冲突！"
        fi
        
        read -p "启用 TUN 模式 (全局代理)? (y/n) [默认 n]: " enable_tun
        TUN_ENABLED="false"
        [[ "$enable_tun" =~ ^[yY]$ ]] && TUN_ENABLED="true"
        
        # 带宽设置
        echo ""
        echo -e "${YELLOW}[带宽设置]${NC} (可选，直接回车跳过)"
        print_info "提示: 设置带宽可以优化连接，但设置过高会导致性能下降"
        read -p "上行带宽 (Mbps) [直接回车跳过]: " BANDWIDTH_UP
        read -p "下行带宽 (Mbps) [直接回车跳过]: " BANDWIDTH_DOWN
        
        # 安装 Hysteria2 (如果未安装)
        install_hysteria
        
        create_default_rules
        generate_config
        
        # 复制配置到配置目录
        cp "$CONFIG_FILE" "${config_dir}/config.yaml"
        
        create_service
        systemctl start "$CLIENT_SERVICE" 2>/dev/null || true
    else
        # VLESS-Reality 配置 (需要 Xray)
        install_xray_client
        generate_xray_config
        
        # 复制配置到配置目录
        cp "${BASE_DIR}/xray-config.json" "${config_dir}/xray-config.json"
    fi
    
    # 更新当前激活配置
    echo "$config_name" > "$ACTIVE_CONFIG"
    
    print_success "配置 '$config_name' 已导入并生成"
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
    
    # 检测端口占用情况
    check_and_suggest_ports 1080 8080
    
    # SOCKS5 端口
    read -p "SOCKS5 端口 [默认 ${SUGGESTED_SOCKS_PORT}]: " SOCKS_PORT
    SOCKS_PORT=${SOCKS_PORT:-$SUGGESTED_SOCKS_PORT}
    
    if is_port_in_use "$SOCKS_PORT"; then
        print_warning "端口 $SOCKS_PORT 已被占用，可能会冲突！"
    fi
    
    # HTTP 端口
    read -p "HTTP 端口 [默认 ${SUGGESTED_HTTP_PORT}]: " HTTP_PORT
    HTTP_PORT=${HTTP_PORT:-$SUGGESTED_HTTP_PORT}
    
    if is_port_in_use "$HTTP_PORT"; then
        print_warning "端口 $HTTP_PORT 已被占用，可能会冲突！"
    fi
    
    # TUN 模式
    read -p "启用 TUN 模式 (全局代理)? (y/n) [默认 n]: " enable_tun
    TUN_ENABLED="false"
    [[ "$enable_tun" =~ ^[yY]$ ]] && TUN_ENABLED="true"
    
    # 带宽设置
    echo ""
    echo -e "${YELLOW}[带宽设置]${NC} (可选，直接回车跳过)"
    print_info "提示: 设置带宽可以优化连接，但设置过高会导致性能下降"
    read -p "上行带宽 (Mbps) [直接回车跳过]: " BANDWIDTH_UP
    read -p "下行带宽 (Mbps) [直接回车跳过]: " BANDWIDTH_DOWN
    
    # 安装 Hysteria2 (如果未安装)
    install_hysteria
    
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
    
    # 读取自定义规则
    local custom_acl_rules=""
    if [[ -f "$RULES_FILE" ]]; then
        while IFS= read -r line; do
            # 跳过注释和空行
            [[ "$line" =~ ^#.*$ || -z "$line" ]] && continue
            line=$(echo "$line" | xargs)  # trim
            [[ -z "$line" ]] && continue
            
            # 处理不同类型的规则
            if [[ "$line" =~ ^regexp: ]]; then
                # 正则表达式
                local pattern="${line#regexp:}"
                custom_acl_rules="${custom_acl_rules}\n    - ${pattern} direct"
            else
                # 域名/通配符/IP
                custom_acl_rules="${custom_acl_rules}\n    - ${line} direct"
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

# SOCKS5 代理
socks5:
  listen: 127.0.0.1:${SOCKS_PORT}

# HTTP 代理
http:
  listen: 127.0.0.1:${HTTP_PORT}
EOF

    # 添加带宽设置 (如果用户设置了)
    if [[ -n "$BANDWIDTH_UP" || -n "$BANDWIDTH_DOWN" ]]; then
        # 在 tls 后插入带宽配置
        local bw_config=""
        bw_config+="\n# 带宽设置\nbandwidth:"
        [[ -n "$BANDWIDTH_UP" ]] && bw_config+="\n  up: ${BANDWIDTH_UP} mbps"
        [[ -n "$BANDWIDTH_DOWN" ]] && bw_config+="\n  down: ${BANDWIDTH_DOWN} mbps"
        
        # 使用 sed 在 tls 块后插入
        sed -i "/^tls:/a\\$(echo -e "$bw_config")" "$CONFIG_FILE" 2>/dev/null || \
        sed -i '' "/^tls:/a\\
$(echo -e "$bw_config")
" "$CONFIG_FILE"
        
        print_info "带宽设置: 上行 ${BANDWIDTH_UP:-未设置} Mbps, 下行 ${BANDWIDTH_DOWN:-未设置} Mbps"
    fi

    # 添加 TUN 配置
    if [[ "$TUN_ENABLED" == "true" ]]; then
        cat >> "$CONFIG_FILE" << EOF

# TUN 模式 (全局代理) - 仅路由 IPv4 流量
tun:
  name: "hystun"
  mtu: 1500
  timeout: 5m
  address:
    ipv4: 100.100.100.101/30
    ipv6: 2001::ffff:ffff:ffff:fff1/126
  route:
    ipv4: [0.0.0.0/0]
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
      - "::/0"

# DNS 配置 - 解决域名解析问题
dns:
  # 使用代理服务器解析 DNS
  mode: tcp
  hijack:
    - type: override
      addr: 8.8.8.8:53
    - type: override
      addr: 8.8.4.4:53

# ACL 路由规则 - 保护 SSH 连接
acl:
  inline:
    # SSH 端口保护 - 所有 22 端口流量绕过代理
    - :22 direct
    - :22/ direct
    # 常用 SSH 备用端口
    - :2222 direct
    - :2222/ direct
EOF
        
        # 添加自定义 ACL 规则
        if [[ -n "$custom_acl_rules" ]]; then
            echo -e "$custom_acl_rules" >> "$CONFIG_FILE"
        fi
    else
        # 非 TUN 模式，但如果有自定义规则，也添加 ACL
        if [[ -n "$custom_acl_rules" ]]; then
            cat >> "$CONFIG_FILE" << EOF

# ACL 路由规则
acl:
  inline:$(echo -e "$custom_acl_rules")
EOF
        fi
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
    echo -e "${CYAN}路由绕过规则配置${NC}"
    echo ""
    
    # 显示内置默认规则
    echo -e "${YELLOW}[内置默认规则]${NC} (始终生效)"
    echo -e "  ${GREEN}●${NC} SSH 端口: 22, 2222"
    echo -e "  ${GREEN}●${NC} 私有 IP: 10.x.x.x, 192.168.x.x, 172.16-31.x.x"
    echo -e "  ${GREEN}●${NC} 国内应用: 微信 腾讯 QQ 小红书 抖音 快手 B站 淘宝 支付宝 京东 百度"
    echo ""
    
    # 显示自定义规则
    echo -e "${YELLOW}[自定义规则]${NC} (文件: $RULES_FILE)"
    local custom_rules=$(grep -v "^#" "$RULES_FILE" 2>/dev/null | grep -v "^$")
    if [[ -n "$custom_rules" ]]; then
        echo "$custom_rules" | while read -r rule; do
            echo -e "  ${CYAN}+${NC} $rule"
        done
    else
        echo -e "  ${YELLOW}(无自定义规则)${NC}"
    fi
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
        if [[ "$regen" =~ ^[yY]$ ]]; then
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
    echo ""
    
    # 检查 sing-box TUN 服务状态
    if systemctl is-active --quiet bui-tun 2>/dev/null; then
        echo -e "TUN 模式: ${GREEN}✓ 运行中${NC} (sing-box)"
        read -p "停止 TUN 模式? (y/n): " disable
        if [[ "$disable" =~ ^[yY]$ ]]; then
            stop_tun_mode
        fi
    else
        echo -e "TUN 模式: ${RED}✗ 未启用${NC}"
        
        # 检查是否有可用的配置
        local config_file=""
        local protocol=""
        
        # 优先使用 sing-box 配置
        if [[ -f "${BASE_DIR}/singbox-tun.json" ]]; then
            echo -e "  发现已有 sing-box TUN 配置"
            read -p "启用 TUN 模式? (y/n): " enable
            if [[ "$enable" =~ ^[yY]$ ]]; then
                SOCKS_PORT=$(grep -o '"listen_port": [0-9]*' "${BASE_DIR}/singbox-tun.json" | head -1 | grep -o '[0-9]*')
                SOCKS_PORT=${SOCKS_PORT:-1080}
                HTTP_PORT=$((SOCKS_PORT + 1))
                start_tun_mode
            fi
        elif [[ -f "$CONFIG_FILE" ]]; then
            # 从 Hysteria2 配置生成 sing-box TUN
            echo -e "  从 Hysteria2 配置生成 TUN..."
            protocol="hysteria2"
            
            # 读取现有配置
            SERVER_ADDR=$(grep "^server:" "$CONFIG_FILE" | awk '{print $2}')
            AUTH_PASSWORD=$(grep "^auth:" "$CONFIG_FILE" | awk '{print $2}')
            local sni=$(grep -A2 "^tls:" "$CONFIG_FILE" | grep "sni:" | awk '{print $2}')
            SNI="${sni:-$(echo $SERVER_ADDR | cut -d':' -f1)}"
            INSECURE=$(grep -A2 "^tls:" "$CONFIG_FILE" | grep "insecure:" | awk '{print $2}')
            INSECURE=${INSECURE:-false}
            SOCKS_PORT=$(grep -A1 "^socks5:" "$CONFIG_FILE" | grep "listen:" | sed 's/.*://')
            HTTP_PORT=$(grep -A1 "^http:" "$CONFIG_FILE" | grep "listen:" | sed 's/.*://')
            SOCKS_PORT=${SOCKS_PORT:-1080}
            HTTP_PORT=${HTTP_PORT:-8080}
            
            read -p "启用 TUN 模式? (y/n): " enable
            if [[ "$enable" =~ ^[yY]$ ]]; then
                generate_singbox_tun_config "hysteria2"
                start_tun_mode
            fi
        elif [[ -f "${BASE_DIR}/xray-config.json" ]]; then
            # 从 Xray 配置生成 sing-box TUN
            echo -e "  从 Xray (VLESS-Reality) 配置生成 TUN..."
            protocol="vless-reality"
            
            # 读取 Xray 配置
            local xray_config="${BASE_DIR}/xray-config.json"
            SERVER_ADDR=$(grep -o '"address": "[^"]*"' "$xray_config" | head -1 | cut -d'"' -f4)
            local port=$(grep -o '"port": [0-9]*' "$xray_config" | grep -v 'listen' | head -1 | grep -o '[0-9]*')
            SERVER_ADDR="${SERVER_ADDR}:${port}"
            UUID=$(grep -o '"id": "[^"]*"' "$xray_config" | head -1 | cut -d'"' -f4)
            FLOW=$(grep -o '"flow": "[^"]*"' "$xray_config" | head -1 | cut -d'"' -f4)
            SNI=$(grep -o '"serverName": "[^"]*"' "$xray_config" | head -1 | cut -d'"' -f4)
            FINGERPRINT=$(grep -o '"fingerprint": "[^"]*"' "$xray_config" | head -1 | cut -d'"' -f4)
            PUBLIC_KEY=$(grep -o '"publicKey": "[^"]*"' "$xray_config" | head -1 | cut -d'"' -f4)
            SHORT_ID=$(grep -o '"shortId": "[^"]*"' "$xray_config" | head -1 | cut -d'"' -f4)
            SOCKS_PORT=$(grep -o '"port": [0-9]*' "$xray_config" | head -1 | grep -o '[0-9]*')
            HTTP_PORT=$(grep -o '"port": [0-9]*' "$xray_config" | tail -1 | grep -o '[0-9]*')
            SOCKS_PORT=${SOCKS_PORT:-1080}
            HTTP_PORT=${HTTP_PORT:-8080}
            
            read -p "启用 TUN 模式? (y/n): " enable
            if [[ "$enable" =~ ^[yY]$ ]]; then
                generate_singbox_tun_config "vless-reality"
                start_tun_mode
            fi
        else
            print_error "请先导入配置 (菜单选项 1)"
            return
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
After=network-online.target
Wants=network-online.target
StartLimitIntervalSec=300
StartLimitBurst=10

[Service]
Type=simple
ExecStart=/usr/local/bin/hysteria client --config ${CONFIG_FILE}
Restart=always
RestartSec=3
LimitNOFILE=1048576

# 防止服务频繁崩溃时无限重启
StartLimitAction=none

# 网络断开时自动重连
TimeoutStartSec=30
TimeoutStopSec=10

# 看门狗（如连接断开时自动重启）
WatchdogSec=60

[Install]
WantedBy=multi-user.target
EOF

    # 创建健康检查定时任务
    create_health_check

    systemctl daemon-reload
    print_success "服务已创建（含自动重启和健康检查）"
}

# 健康检查脚本
create_health_check() {
    local check_script="/opt/hysteria-client/health-check.sh"
    
    cat > "$check_script" << 'HEALTHEOF'
#!/bin/bash
# Hysteria2 客户端健康检查
# 每分钟检测服务状态，如果异常则重启

SERVICE="hysteria-client.service"
LOG_FILE="/var/log/hysteria-health.log"

log() {
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] $1" >> "$LOG_FILE"
}

# 检查服务是否运行
if ! systemctl is-active --quiet "$SERVICE"; then
    log "服务异常，正在重启..."
    systemctl restart "$SERVICE"
    sleep 5
    if systemctl is-active --quiet "$SERVICE"; then
        log "服务重启成功"
    else
        log "服务重启失败！"
    fi
    exit 0
fi

# 检查是否能建立连接（通过 SOCKS5 代理测试）
SOCKS_PORT=$(grep -oP 'socks5\.listen.*:\K[0-9]+' /opt/hysteria-client/config.yaml 2>/dev/null || echo "1080")

if command -v curl &>/dev/null; then
    # 尝试通过代理访问测试
    if ! timeout 10 curl -x socks5h://127.0.0.1:${SOCKS_PORT} -s -o /dev/null -w "%{http_code}" https://www.google.com 2>/dev/null | grep -q "200\|301\|302"; then
        log "连接测试失败，正在重启服务..."
        systemctl restart "$SERVICE"
        sleep 5
        if systemctl is-active --quiet "$SERVICE"; then
            log "服务重启成功"
        else
            log "服务重启失败！"
        fi
    fi
fi
HEALTHEOF

    chmod +x "$check_script"
    
    # 创建 systemd timer
    cat > /etc/systemd/system/hysteria-health.service << EOF
[Unit]
Description=Hysteria2 Health Check

[Service]
Type=oneshot
ExecStart=$check_script
EOF

    cat > /etc/systemd/system/hysteria-health.timer << EOF
[Unit]
Description=Hysteria2 Health Check Timer

[Timer]
OnBootSec=2min
OnUnitActiveSec=1min
AccuracySec=30s

[Install]
WantedBy=timers.target
EOF

    systemctl daemon-reload
    systemctl enable hysteria-health.timer 2>/dev/null
    systemctl start hysteria-health.timer 2>/dev/null
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
    
    # 当前配置
    local active_config=$(get_active_config)
    if [[ -n "$active_config" ]]; then
        echo -e "${YELLOW}[当前配置]${NC} ${GREEN}★ ${active_config}${NC}"
    else
        echo -e "${YELLOW}[当前配置]${NC} ${RED}未设置${NC}"
    fi
    
    # 版本信息
    echo ""
    echo -e "${YELLOW}[内核版本]${NC}"
    if command -v hysteria &> /dev/null; then
        local hy_ver=$(hysteria version 2>/dev/null | grep "^Version:" | awk '{print $2}' || echo "未知")
        echo -e "  Hysteria2: ${GREEN}${hy_ver}${NC}"
    else
        echo -e "  Hysteria2: ${RED}未安装${NC}"
    fi
    if command -v xray &> /dev/null; then
        local xray_ver=$(xray version 2>/dev/null | head -n1 | awk '{print $2}' || echo "未知")
        echo -e "  Xray:      ${GREEN}${xray_ver}${NC}"
    else
        echo -e "  Xray:      ${RED}未安装${NC}"
    fi
    if command -v sing-box &> /dev/null; then
        local singbox_ver=$(sing-box version 2>/dev/null | head -n1 | awk '{print $3}' || echo "未知")
        echo -e "  sing-box:  ${GREEN}${singbox_ver}${NC}"
    else
        echo -e "  sing-box:  ${RED}未安装${NC}"
    fi
    
    # 服务状态
    echo ""
    echo -e "${YELLOW}[服务状态]${NC}"
    
    # 检测 TUN 模式状态
    local tun_running=false
    local tun_protocol=""
    if systemctl is-active --quiet bui-tun 2>/dev/null; then
        tun_running=true
        # 从 sing-box 配置中读取协议类型
        if [[ -f "${BASE_DIR}/singbox-tun.json" ]]; then
            if grep -q '"type": "hysteria2"' "${BASE_DIR}/singbox-tun.json" 2>/dev/null; then
                tun_protocol="Hysteria2"
            elif grep -q '"type": "vless"' "${BASE_DIR}/singbox-tun.json" 2>/dev/null; then
                tun_protocol="VLESS-Reality"
            fi
        fi
    fi
    
    # TUN 模式优先显示
    if $tun_running; then
        echo -e "  TUN 模式:  ${GREEN}✓ 运行中${NC} (sing-box + ${tun_protocol:-未知协议})"
        echo -e "  └─ 协议:   ${CYAN}${tun_protocol:-未知}${NC} (全局透明代理)"
    elif [[ -f /etc/systemd/system/bui-tun.service ]]; then
        echo -e "  TUN 模式:  ${RED}✗ 已停止${NC}"
    else
        echo -e "  TUN 模式:  ${YELLOW}○ 未配置${NC}"
    fi
    
    # 独立代理服务（TUN 关闭时才显示运行状态）
    if ! $tun_running; then
        if systemctl is-active --quiet "$CLIENT_SERVICE" 2>/dev/null; then
            echo -e "  Hysteria2: ${GREEN}✓ 运行中${NC}"
        elif command -v hysteria &> /dev/null; then
            echo -e "  Hysteria2: ${RED}✗ 已停止${NC}"
        else
            echo -e "  Hysteria2: ${YELLOW}○ 未安装${NC}"
        fi
        
        if systemctl is-active --quiet xray-client 2>/dev/null; then
            echo -e "  Xray:      ${GREEN}✓ 运行中${NC}"
        elif command -v xray &> /dev/null; then
            echo -e "  Xray:      ${RED}✗ 已停止${NC}"
        else
            echo -e "  Xray:      ${YELLOW}○ 未安装${NC}"
        fi
    fi
    
    # 代理端口
    echo ""
    echo -e "${YELLOW}[代理端口]${NC}"
    if [[ -f "$CONFIG_FILE" ]]; then
        local socks=$(grep -A1 "^socks5:" "$CONFIG_FILE" 2>/dev/null | grep "listen:" | awk '{print $2}')
        local http=$(grep -A1 "^http:" "$CONFIG_FILE" 2>/dev/null | grep "listen:" | awk '{print $2}')
        echo -e "  SOCKS5: ${GREEN}${socks:-未配置}${NC}"
        echo -e "  HTTP:   ${GREEN}${http:-未配置}${NC}"
    else
        echo -e "  SOCKS5: ${RED}未配置${NC}"
        echo -e "  HTTP:   ${RED}未配置${NC}"
    fi
    
    # 开机自启动状态
    echo ""
    echo -e "${YELLOW}[开机自启动]${NC}"
    if [[ -f /etc/systemd/system/$CLIENT_SERVICE ]]; then
        local hy_auto=$(systemctl is-enabled "$CLIENT_SERVICE" 2>/dev/null || echo "disabled")
        if [[ "$hy_auto" == "enabled" ]]; then
            echo -e "  Hysteria2: ${GREEN}✓ 已启用${NC}"
        else
            echo -e "  Hysteria2: ${RED}✗ 未启用${NC}"
        fi
    else
        echo -e "  Hysteria2: ${YELLOW}○ 未配置${NC}"
    fi
    
    if [[ -f /etc/systemd/system/xray-client.service ]]; then
        local xray_auto=$(systemctl is-enabled xray-client 2>/dev/null)
        if [[ "$xray_auto" == "enabled" ]]; then
            echo -e "  Xray:      ${GREEN}✓ 已启用${NC}"
        else
            echo -e "  Xray:      ${RED}✗ 未启用${NC}"
        fi
    else
        echo -e "  Xray:      ${YELLOW}○ 未配置${NC}"
    fi
    
    if [[ -f /etc/systemd/system/bui-tun.service ]]; then
        local tun_auto=$(systemctl is-enabled bui-tun 2>/dev/null)
        if [[ "$tun_auto" == "enabled" ]]; then
            echo -e "  TUN 模式:  ${GREEN}✓ 已启用${NC}"
        else
            echo -e "  TUN 模式:  ${RED}✗ 未启用${NC}"
        fi
    else
        echo -e "  TUN 模式:  ${YELLOW}○ 未配置${NC}"
    fi
    
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
}

test_proxy() {
    echo ""
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo -e "${CYAN}                    代理连接测试${NC}"
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo ""
    
    local test_passed=0
    local test_failed=0
    
    # 检测当前运行模式
    local tun_running=false
    local hy_running=false
    local xray_running=false
    local current_protocol=""
    
    if systemctl is-active --quiet bui-tun 2>/dev/null; then
        tun_running=true
        # 从配置读取协议
        if [[ -f "${BASE_DIR}/singbox-tun.json" ]]; then
            if grep -q '"type": "hysteria2"' "${BASE_DIR}/singbox-tun.json" 2>/dev/null; then
                current_protocol="Hysteria2"
            elif grep -q '"type": "vless"' "${BASE_DIR}/singbox-tun.json" 2>/dev/null; then
                current_protocol="VLESS-Reality"
            fi
        fi
        echo -e "${YELLOW}当前模式:${NC} TUN 全局透明代理 (${current_protocol:-未知协议})"
    else
        if systemctl is-active --quiet "$CLIENT_SERVICE" 2>/dev/null; then
            hy_running=true
            current_protocol="Hysteria2"
        fi
        if systemctl is-active --quiet xray-client 2>/dev/null; then
            xray_running=true
            current_protocol="VLESS-Reality"
        fi
        
        if $hy_running || $xray_running; then
            echo -e "${YELLOW}当前模式:${NC} SOCKS5/HTTP 代理 (${current_protocol})"
        else
            echo -e "${RED}警告:${NC} 未检测到运行中的代理服务"
            echo ""
            print_warning "请先启动服务 (选项 5) 或开启 TUN 模式 (选项 8)"
            return 1
        fi
    fi
    echo ""
    
    # 测试 1: 检查代理端口
    echo -e "${YELLOW}[测试 1]${NC} 检查代理端口..."
    local socks_port=""
    local http_port=""
    
    if $tun_running; then
        # TUN 模式从 sing-box 配置读取端口
        socks_port=$(grep -o '"listen_port": [0-9]*' "${BASE_DIR}/singbox-tun.json" 2>/dev/null | head -1 | grep -o '[0-9]*')
        http_port=$(grep -o '"listen_port": [0-9]*' "${BASE_DIR}/singbox-tun.json" 2>/dev/null | tail -1 | grep -o '[0-9]*')
    else
        # 从 Hysteria2 或 Xray 配置读取
        socks_port=$(grep -A1 "^socks5:" "$CONFIG_FILE" 2>/dev/null | grep "listen:" | sed 's/.*://')
        http_port=$(grep -A1 "^http:" "$CONFIG_FILE" 2>/dev/null | grep "listen:" | sed 's/.*://')
    fi
    socks_port=${socks_port:-1080}
    http_port=${http_port:-8080}
    
    if ss -tlnp 2>/dev/null | grep -q ":${socks_port}\b"; then
        echo -e "  SOCKS5 (${socks_port}): ${GREEN}✓ 监听中${NC}"
        ((test_passed++))
    else
        echo -e "  SOCKS5 (${socks_port}): ${RED}✗ 未监听${NC}"
        ((test_failed++))
    fi
    
    if ss -tlnp 2>/dev/null | grep -q ":${http_port}\b"; then
        echo -e "  HTTP   (${http_port}): ${GREEN}✓ 监听中${NC}"
        ((test_passed++))
    else
        echo -e "  HTTP   (${http_port}): ${YELLOW}○ 未监听${NC} (可选)"
    fi
    echo ""
    
    # 测试 2: 通过代理访问外网
    echo -e "${YELLOW}[测试 2]${NC} 测试代理连通性..."
    
    if $tun_running; then
        # TUN 模式直接测试，流量会自动走 TUN
        echo -n "  Google (直连测试): "
        if curl -s --max-time 10 https://www.google.com -o /dev/null 2>&1; then
            echo -e "${GREEN}✓ 连通${NC}"
            ((test_passed++))
        else
            echo -e "${RED}✗ 失败${NC}"
            ((test_failed++))
        fi
    else
        # SOCKS5 代理测试
        echo -n "  Google (SOCKS5): "
        if curl -s --max-time 10 --socks5-hostname "127.0.0.1:${socks_port}" https://www.google.com -o /dev/null 2>&1; then
            echo -e "${GREEN}✓ 连通${NC}"
            ((test_passed++))
        else
            echo -e "${RED}✗ 失败${NC}"
            ((test_failed++))
        fi
    fi
    
    # 测试国内网站（验证直连规则）
    echo -n "  百度 (直连规则): "
    if curl -s --max-time 5 https://www.baidu.com -o /dev/null 2>&1; then
        echo -e "${GREEN}✓ 连通${NC}"
        ((test_passed++))
    else
        echo -e "${YELLOW}○ 超时${NC}"
    fi
    echo ""
    
    # 测试 3: 检查 DNS 解析
    echo -e "${YELLOW}[测试 3]${NC} DNS 解析测试..."
    echo -n "  解析 google.com: "
    local google_ip=$(dig +short google.com A 2>/dev/null | head -1)
    if [[ -n "$google_ip" ]]; then
        echo -e "${GREEN}✓${NC} ($google_ip)"
        ((test_passed++))
    else
        echo -e "${RED}✗ 解析失败${NC}"
        ((test_failed++))
    fi
    echo ""
    
    # 测试 4: 延迟测试
    echo -e "${YELLOW}[测试 4]${NC} 延迟测试..."
    
    # Google 延迟
    echo -n "  Google 延迟: "
    local google_latency=""
    if $tun_running; then
        google_latency=$(curl -s -o /dev/null -w '%{time_total}' --max-time 10 https://www.google.com 2>/dev/null)
    else
        google_latency=$(curl -s -o /dev/null -w '%{time_total}' --max-time 10 --socks5-hostname "127.0.0.1:${socks_port}" https://www.google.com 2>/dev/null)
    fi
    # 检查是否有有效的延迟值（非空且不是纯 0）
    if [[ -n "$google_latency" ]] && awk "BEGIN {exit !($google_latency > 0)}" 2>/dev/null; then
        local latency_ms=$(awk "BEGIN {printf \"%.0f\", $google_latency * 1000}" 2>/dev/null)
        if [[ "$latency_ms" -lt 500 ]]; then
            echo -e "${GREEN}${latency_ms}ms${NC} (优秀)"
        elif [[ "$latency_ms" -lt 1000 ]]; then
            echo -e "${YELLOW}${latency_ms}ms${NC} (良好)"
        else
            echo -e "${RED}${latency_ms}ms${NC} (较慢)"
        fi
        ((test_passed++))
    else
        echo -e "${RED}超时${NC}"
        ((test_failed++))
    fi
    
    # YouTube 延迟 (可选)
    echo -n "  YouTube 延迟: "
    local yt_latency=""
    if $tun_running; then
        yt_latency=$(curl -s -o /dev/null -w '%{time_total}' --max-time 10 https://www.youtube.com 2>/dev/null)
    else
        yt_latency=$(curl -s -o /dev/null -w '%{time_total}' --max-time 10 --socks5-hostname "127.0.0.1:${socks_port}" https://www.youtube.com 2>/dev/null)
    fi
    if [[ -n "$yt_latency" ]] && awk "BEGIN {exit !($yt_latency > 0)}" 2>/dev/null; then
        local yt_ms=$(awk "BEGIN {printf \"%.0f\", $yt_latency * 1000}" 2>/dev/null)
        if [[ "$yt_ms" -lt 500 ]]; then
            echo -e "${GREEN}${yt_ms}ms${NC}"
        elif [[ "$yt_ms" -lt 1000 ]]; then
            echo -e "${YELLOW}${yt_ms}ms${NC}"
        else
            echo -e "${RED}${yt_ms}ms${NC}"
        fi
    else
        echo -e "${YELLOW}超时${NC}"
    fi
    echo ""
    
    # 测试 5: 简单网速测试
    echo -e "${YELLOW}[测试 5]${NC} 下载速度测试..."
    echo -n "  Cloudflare 测速: "
    local start_time=$(date +%s.%N)
    local download_size=0
    
    if $tun_running; then
        download_size=$(curl -s --max-time 5 -o /dev/null -w '%{size_download}' https://speed.cloudflare.com/__down?bytes=1000000 2>/dev/null)
    else
        download_size=$(curl -s --max-time 5 -o /dev/null -w '%{size_download}' --socks5-hostname "127.0.0.1:${socks_port}" https://speed.cloudflare.com/__down?bytes=1000000 2>/dev/null)
    fi
    local end_time=$(date +%s.%N)
    
    if [[ -n "$download_size" && "$download_size" -gt 0 ]]; then
        local duration=$(awk "BEGIN {printf \"%.2f\", $end_time - $start_time}" 2>/dev/null)
        local speed_mbs=$(awk "BEGIN {printf \"%.2f\", $download_size / $duration / 1024 / 1024}" 2>/dev/null)
        
        # 评级标准：>1.25 MB/s (10Mbps) 优秀，>0.625 MB/s (5Mbps) 良好
        local speed_int=${speed_mbs%.*}
        if awk "BEGIN {exit !($speed_mbs >= 1.25)}" 2>/dev/null; then
            echo -e "${GREEN}${speed_mbs} MB/s${NC} (优秀)"
        elif awk "BEGIN {exit !($speed_mbs >= 0.625)}" 2>/dev/null; then
            echo -e "${YELLOW}${speed_mbs} MB/s${NC} (良好)"
        else
            echo -e "${RED}${speed_mbs} MB/s${NC} (较慢)"
        fi
        ((test_passed++))
    else
        echo -e "${YELLOW}测速失败${NC}"
    fi
    echo ""
    
    # 汇总结果
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    if [[ $test_failed -eq 0 ]]; then
        echo -e "${GREEN}测试结果: 全部通过 (${test_passed} 项)${NC}"
    else
        echo -e "${YELLOW}测试结果: 通过 ${test_passed} 项, 失败 ${test_failed} 项${NC}"
    fi
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
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
    echo ""
    echo -e "${RED}╔══════════════════════════════════════════════════════════════╗${NC}"
    echo -e "${RED}║                    完全卸载                                  ║${NC}"
    echo -e "${RED}╚══════════════════════════════════════════════════════════════╝${NC}"
    echo ""
    echo -e "${YELLOW}将卸载以下内容:${NC}"
    echo "  • Hysteria2 客户端服务和配置"
    echo "  • Xray 客户端服务和配置"
    echo "  • 所有已保存的配置 (configs 目录)"
    echo "  • 路由规则和 TUN 配置"
    echo "  • 全局命令 bui-c"
    echo ""
    
    read -p "输入 'YES' 确认完全卸载: " confirm
    
    if [[ ! "$confirm" =~ ^[yY][eE][sS]$ ]]; then
        print_info "已取消卸载"
        return
    fi
    
    echo ""
    print_info "开始卸载..."
    
    # 1. 停止并删除 Hysteria2 服务
    if systemctl is-active --quiet "$CLIENT_SERVICE" 2>/dev/null; then
        print_info "停止 Hysteria2 服务..."
        systemctl stop "$CLIENT_SERVICE" 2>/dev/null || true
    fi
    systemctl disable "$CLIENT_SERVICE" 2>/dev/null || true
    rm -f "/etc/systemd/system/$CLIENT_SERVICE"
    echo -e "  ${GREEN}✓${NC} Hysteria2 服务已移除"
    
    # 2. 停止并删除 Xray 服务
    if systemctl is-active --quiet xray-client 2>/dev/null; then
        print_info "停止 Xray 服务..."
        systemctl stop xray-client 2>/dev/null || true
    fi
    systemctl disable xray-client 2>/dev/null || true
    rm -f /etc/systemd/system/xray-client.service
    echo -e "  ${GREEN}✓${NC} Xray 服务已移除"
    
    # 3. 停止并删除 TUN 模式服务
    if systemctl is-active --quiet bui-tun 2>/dev/null; then
        print_info "停止 TUN 模式..."
        systemctl stop bui-tun 2>/dev/null || true
    fi
    systemctl disable bui-tun 2>/dev/null || true
    rm -f /etc/systemd/system/bui-tun.service
    ip link delete bui-tun 2>/dev/null || true
    echo -e "  ${GREEN}✓${NC} TUN 模式已移除"
    
    # 4. 重载 systemd
    systemctl daemon-reload
    
    # 4. 删除配置目录 (包含所有保存的配置)
    if [[ -d "$BASE_DIR" ]]; then
        rm -rf "$BASE_DIR"
        echo -e "  ${GREEN}✓${NC} 配置目录已删除 ($BASE_DIR)"
    fi
    
    # 5. 删除 sysctl 配置
    rm -f /etc/sysctl.d/99-hysteria.conf 2>/dev/null || true
    
    # 6. 询问是否删除程序
    echo ""
    read -p "删除 Hysteria2 程序? (y/n) [默认 y]: " del_hy
    del_hy=${del_hy:-y}
    if [[ "$del_hy" =~ ^[yY]$ ]]; then
        rm -f /usr/local/bin/hysteria
        echo -e "  ${GREEN}✓${NC} Hysteria2 程序已删除"
    fi
    
    read -p "删除 Xray 程序? (y/n) [默认 y]: " del_xray
    del_xray=${del_xray:-y}
    if [[ "$del_xray" =~ ^[yY]$ ]]; then
        # Xray 可能通过官方脚本安装到不同位置
        rm -f /usr/local/bin/xray
        rm -rf /usr/local/share/xray
        rm -rf /usr/local/etc/xray
        echo -e "  ${GREEN}✓${NC} Xray 程序已删除"
    fi
    
    read -p "删除 sing-box 程序? (y/n) [默认 y]: " del_sb
    del_sb=${del_sb:-y}
    if [[ "$del_sb" =~ ^[yY]$ ]]; then
        if [[ "$PKG_MANAGER" == "apt" ]]; then
            apt-get remove -y -qq sing-box 2>/dev/null || true
            rm -f /etc/apt/sources.list.d/sagernet.list
            rm -f /etc/apt/keyrings/sagernet.asc
        else
            rm -f /usr/local/bin/sing-box /usr/bin/sing-box
        fi
        echo -e "  ${GREEN}✓${NC} sing-box 程序已删除"
    fi
    
    # 7. 删除全局命令 (始终删除，因为用户已确认完全卸载)
    echo ""
    print_info "删除全局命令..."
    if [[ -f /usr/local/bin/bui-c ]]; then
        rm -f /usr/local/bin/bui-c
        echo -e "  ${GREEN}✓${NC} 全局命令 bui-c 已删除"
    else
        echo -e "  ${YELLOW}○${NC} 全局命令 bui-c 不存在"
    fi
    
    # 也检查 b-ui-client 别名 (如果有)
    if [[ -f /usr/local/bin/b-ui-client ]]; then
        rm -f /usr/local/bin/b-ui-client
        echo -e "  ${GREEN}✓${NC} 全局命令 b-ui-client 已删除"
    fi
    
    echo ""
    print_success "卸载完成！"
    echo ""
    print_info "脚本将退出"
    exit 0
}

#===============================================================================
# 主菜单
#===============================================================================

show_menu() {
    echo ""
    echo -e "${CYAN}╔══════════════════════════════════════════════════════════════╗${NC}"
    echo -e "${CYAN}║${NC}              ${GREEN}B-UI 客户端 操作菜单${NC}                            ${CYAN}║${NC}"
    echo -e "${CYAN}╠══════════════════════════════════════════════════════════════╣${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}1.${NC} ${GREEN}从链接导入配置${NC} (Hysteria2 / VLESS-Reality)           ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}2.${NC} ${GREEN}批量导入配置${NC}                                         ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}3.${NC} ${GREEN}配置管理${NC} (列表/切换/删除)                             ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}4.${NC} 手动配置 Hysteria2                                     ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}15.${NC} ${GREEN}📦 导入订阅${NC} (Hy2+VLESS 自动切换)                     ${CYAN}║${NC}"
    echo -e "${CYAN}╠══════════════════════════════════════════════════════════════╣${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}5.${NC} 启动/停止服务                                          ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}6.${NC} 重启服务                                               ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}7.${NC} 查看日志                                               ${CYAN}║${NC}"
    echo -e "${CYAN}╠══════════════════════════════════════════════════════════════╣${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}8.${NC} ${GREEN}TUN 模式开关${NC} (全局透明代理 via sing-box)           ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}9.${NC} 编辑路由规则                                           ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}10.${NC} 测试代理连接                                          ${CYAN}║${NC}"
    echo -e "${CYAN}╠══════════════════════════════════════════════════════════════╣${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}11.${NC} 更新内核 (Hysteria2/Xray/sing-box)                    ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}12.${NC} 开机自启动设置                                        ${CYAN}║${NC}"
    # 显示更新客户端选项，如果有新版本则高亮显示
    if [[ "$UPDATE_AVAILABLE" == "true" ]]; then
        echo -e "${CYAN}║${NC}  ${YELLOW}14.${NC} ${GREEN}⬆ 更新客户端${NC} ${RED}(有新版本 v${REMOTE_VERSION})${NC}               ${CYAN}║${NC}"
    else
        echo -e "${CYAN}║${NC}  ${YELLOW}14.${NC} 检查/更新客户端                                      ${CYAN}║${NC}"
    fi
    echo -e "${CYAN}╠══════════════════════════════════════════════════════════════╣${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}13.${NC} ${RED}卸载${NC}                                                 ${CYAN}║${NC}"
    echo -e "${CYAN}║${NC}  ${YELLOW}0.${NC} 退出                                                   ${CYAN}║${NC}"
    echo -e "${CYAN}╚══════════════════════════════════════════════════════════════╝${NC}"
}

main() {
    check_root
    check_os
    
    # 只在首次运行时检测依赖 (核心未安装时)
    if ! command -v hysteria &> /dev/null || ! command -v xray &> /dev/null || ! command -v sing-box &> /dev/null; then
        check_dependencies
    fi
    
    # 后台检查客户端更新 (静默检查，不阻塞启动)
    check_client_update &>/dev/null &
    
    while true; do
        print_banner
        show_status
        show_menu
        read -p "请选择 [0-15]: " choice
        
        case $choice in
            1) import_from_uri ;;
            2) import_batch ;;
            3) config_management ;;
            4) quick_install ;;
            5) 
                # 启动/停止
                if systemctl is-active --quiet "$CLIENT_SERVICE" 2>/dev/null; then
                    systemctl stop "$CLIENT_SERVICE"
                    systemctl stop xray-client 2>/dev/null || true
                    print_success "服务已停止"
                else
                    systemctl start "$CLIENT_SERVICE" 2>/dev/null || true
                    systemctl start xray-client 2>/dev/null || true
                    print_success "服务已启动"
                fi
                ;;
            6) 
                # 重启
                systemctl restart "$CLIENT_SERVICE" 2>/dev/null || true
                systemctl restart xray-client 2>/dev/null || true
                print_success "服务已重启"
                ;;
            7) 
                # 查看日志
                echo ""
                echo -e "${YELLOW}选择日志类型:${NC}"
                echo "  1. Hysteria2"
                echo "  2. Xray"
                echo "  3. TUN 模式 (sing-box)"
                read -p "请选择 [1-3]: " log_choice
                case $log_choice in
                    1) journalctl -u "$CLIENT_SERVICE" --no-pager -n 30 ;;
                    2) journalctl -u xray-client --no-pager -n 30 ;;
                    3) journalctl -u bui-tun --no-pager -n 30 ;;
                    *) print_error "无效选项" ;;
                esac
                ;;
            8) toggle_tun ;;
            9) edit_rules ;;
            10) test_proxy ;;
            11) 
                # 更新内核
                print_info "更新 Hysteria2..."
                local old_hy=$(hysteria version 2>/dev/null | grep "^Version:" | awk '{print $2}' || echo "未知")
                HYSTERIA_USER=root bash <(curl -fsSL https://get.hy2.sh/)
                local new_hy=$(hysteria version 2>/dev/null | grep "^Version:" | awk '{print $2}' || echo "未知")
                echo -e "  Hysteria2: ${YELLOW}${old_hy}${NC} -> ${GREEN}${new_hy}${NC}"
                
                if command -v xray &> /dev/null; then
                    print_info "更新 Xray..."
                    local old_xray=$(xray version 2>/dev/null | head -n1 | awk '{print $2}' || echo "未知")
                    bash -c "$(curl -L https://github.com/XTLS/Xray-install/raw/main/install-release.sh)" @ install
                    local new_xray=$(xray version 2>/dev/null | head -n1 | awk '{print $2}' || echo "未知")
                    echo -e "  Xray: ${YELLOW}${old_xray}${NC} -> ${GREEN}${new_xray}${NC}"
                fi
                
                if command -v sing-box &> /dev/null; then
                    print_info "更新 sing-box..."
                    local old_sb=$(sing-box version 2>/dev/null | head -n1 | awk '{print $3}' || echo "未知")
                    if [[ "$PKG_MANAGER" == "apt" ]]; then
                        apt-get update -qq && apt-get install -y -qq sing-box
                    else
                        bash <(curl -fsSL https://sing-box.app/install.sh)
                    fi
                    local new_sb=$(sing-box version 2>/dev/null | head -n1 | awk '{print $3}' || echo "未知")
                    echo -e "  sing-box: ${YELLOW}${old_sb}${NC} -> ${GREEN}${new_sb}${NC}"
                fi
                print_success "内核更新完成"
                ;;
            12)
                # 开机自启动
                echo ""
                echo -e "${YELLOW}[当前自启动状态]${NC}"
                local hy_auto=$(systemctl is-enabled "$CLIENT_SERVICE" 2>/dev/null || echo "disabled")
                local xray_auto=$(systemctl is-enabled xray-client 2>/dev/null || echo "disabled")
                local tun_auto=$(systemctl is-enabled bui-tun 2>/dev/null || echo "disabled")
                
                [[ "$hy_auto" == "enabled" ]] && echo -e "  Hysteria2: ${GREEN}✓ 已启用${NC}" || echo -e "  Hysteria2: ${RED}✗ 未启用${NC}"
                [[ "$xray_auto" == "enabled" ]] && echo -e "  Xray:      ${GREEN}✓ 已启用${NC}" || echo -e "  Xray:      ${RED}✗ 未启用${NC}"
                [[ "$tun_auto" == "enabled" ]] && echo -e "  TUN 模式:  ${GREEN}✓ 已启用${NC}" || echo -e "  TUN 模式:  ${RED}✗ 未启用${NC}"
                
                echo ""
                if [[ "$hy_auto" == "enabled" || "$xray_auto" == "enabled" || "$tun_auto" == "enabled" ]]; then
                    read -p "关闭所有开机自启动? (y/n): " disable
                    if [[ "$disable" =~ ^[yY]$ ]]; then
                        systemctl disable "$CLIENT_SERVICE" 2>/dev/null || true
                        systemctl disable xray-client 2>/dev/null || true
                        systemctl disable bui-tun 2>/dev/null || true
                        print_success "已关闭所有开机自启动"
                    fi
                else
                    read -p "开启所有开机自启动? (y/n): " enable
                    if [[ "$enable" =~ ^[yY]$ ]]; then
                        # 只启用已配置的服务
                        [[ -f /etc/systemd/system/$CLIENT_SERVICE ]] && systemctl enable "$CLIENT_SERVICE" 2>/dev/null
                        [[ -f /etc/systemd/system/xray-client.service ]] && systemctl enable xray-client 2>/dev/null
                        [[ -f /etc/systemd/system/bui-tun.service ]] && systemctl enable bui-tun 2>/dev/null
                        print_success "已开启开机自启动"
                    fi
                fi
                ;;
            13) uninstall ;;
            14) 
                # 更新客户端
                echo ""
                if [[ "$UPDATE_AVAILABLE" == "true" ]]; then
                    echo -e "发现新版本: ${YELLOW}v${SCRIPT_VERSION}${NC} -> ${GREEN}v${REMOTE_VERSION}${NC}"
                    read -p "是否立即更新? (y/n): " confirm
                    if [[ "$confirm" =~ ^[yY]$ ]]; then
                        do_client_update
                    fi
                else
                    print_info "检查更新中..."
                    if check_client_update; then
                        echo -e "发现新版本: ${YELLOW}v${SCRIPT_VERSION}${NC} -> ${GREEN}v${REMOTE_VERSION}${NC}"
                        read -p "是否立即更新? (y/n): " confirm
                        if [[ "$confirm" =~ ^[yY]$ ]]; then
                            do_client_update
                        fi
                    else
                        print_success "已是最新版本 (v${SCRIPT_VERSION})"
                    fi
                fi
                ;;
            15) import_from_subscription ;;
            0) echo ""; print_info "再见！"; exit 0 ;;
            *) print_error "无效选项" ;;
        esac
        
        echo ""
        read -p "按 Enter 继续..."
    done
}

#===============================================================================
# 创建全局命令
#===============================================================================

SCRIPT_URL="https://raw.githubusercontent.com/Buxiulei/b-ui/main/b-ui-client.sh"

create_global_command() {
    print_info "创建/更新全局命令 bui-c..."
    
    # 获取当前脚本的实际路径
    local current_script=""
    
    # 如果当前脚本可读且行数足够，直接复制
    if [[ -r "$0" ]] && [[ "$0" != "/usr/local/bin/bui-c" ]]; then
        local self_lines=$(wc -l < "$0" 2>/dev/null || echo "0")
        if [[ "$self_lines" -gt 1000 ]]; then
            current_script="$0"
        fi
    fi
    
    # 方法1: 直接复制当前脚本（最可靠）
    if [[ -n "$current_script" ]]; then
        cp "$current_script" /usr/local/bin/bui-c
        chmod +x /usr/local/bin/bui-c
        print_success "全局命令已更新 (当前版本 v${SCRIPT_VERSION})"
        return 0
    fi
    
    # 方法2: 从服务端下载
    if download_from_server "b-ui-client.sh" "/usr/local/bin/bui-c"; then
        chmod +x /usr/local/bin/bui-c
        local lines=$(wc -l < /usr/local/bin/bui-c 2>/dev/null || echo "0")
        if [[ "$lines" -gt 1000 ]]; then
            print_success "全局命令已创建 (从服务端下载)"
            return 0
        fi
        rm -f /usr/local/bin/bui-c
    fi
    
    # 降级到镜像下载
    print_info "服务端下载失败，尝试镜像..."
    if smart_download "$SCRIPT_URL" "/usr/local/bin/bui-c" "b-ui-client.sh"; then
        chmod +x /usr/local/bin/bui-c
        local lines=$(wc -l < /usr/local/bin/bui-c 2>/dev/null || echo "0")
        if [[ "$lines" -gt 1000 ]]; then
            print_success "全局命令已创建，可使用 'sudo bui-c' 运行"
            return 0
        else
            print_error "下载的文件不完整 (只有 $lines 行)"
            rm -f /usr/local/bin/bui-c
        fi
    fi
    
    print_error "下载失败，请稍后重试或手动下载"
    return 1
}




# 首次运行检测 - 安装所有核心和创建全局命令
first_run_setup() {
    local cores_installed=true
    
    # 检查是否需要安装核心
    if ! command -v hysteria &> /dev/null || ! command -v xray &> /dev/null || ! command -v sing-box &> /dev/null; then
        cores_installed=false
    fi
    
    # 首次运行时安装所有核心
    if [[ "$cores_installed" == "false" ]]; then
        echo ""
        print_warning "检测到首次运行，需要安装代理核心组件"
        read -p "是否现在安装 Hysteria2/Xray/sing-box? [Y/n]: " install
        # 默认为 Y，回车即安装
        if [[ -z "$install" || "$install" =~ ^[yY]$ ]]; then
            install_all_cores
        fi
    fi
    
    # 创建或更新全局命令
    if [[ "$0" != "/usr/local/bin/bui-c" ]]; then
        if [[ -f /usr/local/bin/bui-c ]]; then
            # 自动更新
            print_info "更新全局命令 bui-c..."
            create_global_command
        else
            # 首次创建，默认 Y
            read -p "是否创建全局命令 'bui-c'? [Y/n]: " create_cmd
            if [[ -z "$create_cmd" || "$create_cmd" =~ ^[yY]$ ]]; then
                create_global_command
            fi
        fi
    fi
}

# 入口
first_run_setup
main "$@"

