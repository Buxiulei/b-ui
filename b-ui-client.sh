#!/bin/bash

#===============================================================================
# B-UI 多协议客户端 (Hysteria2 / VLESS-Reality / TUN)
# 功能：SOCKS5/HTTP 代理、TUN 模式、路由规则、SSH 保护
# 版本: 动态读取自 GitHub
#===============================================================================

# 版本号占位符，分发时由 web/server.js 从 version.json 动态注入
# 直接从 GitHub clone 时该值可能滞后于实际仓库版本
SCRIPT_VERSION="3.4.4"

# 注意: 不使用 set -e，因为它会导致 ((count++)) 等算术运算在变量为0时退出脚本

# 颜色定义
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m'
DIM='\033[2m'

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
    echo ""
    echo -e "${CYAN}┌──────────────────────────────────────────┐${NC}"
    echo -e "${CYAN}│${NC}                                          ${CYAN}│${NC}"
    echo -e "${CYAN}│${NC}       ${GREEN}B-UI 多协议客户端${NC}                 ${CYAN}│${NC}"
    echo -e "${CYAN}│${NC}                                          ${CYAN}│${NC}"
    echo -e "${CYAN}├──────────────────────────────────────────┘${NC}"
    echo -e "${CYAN}│${NC}  Hysteria2 / VLESS-Reality / TUN"
    echo -e "${CYAN}│${NC}  ${DIM}版本: ${NC}${YELLOW}${SCRIPT_VERSION}${NC}"
    echo -e "${CYAN}└──────────────────────────────────────────${NC}"
    echo ""
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

# 远程版本检查 URL (使用 raw.githack.com CDN)
REMOTE_VERSION_URL="https://raw.githack.com/Buxiulei/b-ui/main/b-ui-client.sh"
GITHUB_RAW_URL="https://raw.githubusercontent.com/Buxiulei/b-ui/main/b-ui-client.sh"
# 版本 JSON 文件 URL (推荐: 版本号统一管理)
VERSION_JSON_URL="https://raw.githack.com/Buxiulei/b-ui/main/version.json"
VERSION_JSON_RAW_URL="https://raw.githubusercontent.com/Buxiulei/b-ui/main/version.json"

# 全局变量存储更新状态
UPDATE_AVAILABLE=""
REMOTE_VERSION=""

#===============================================================================
# TUI 工具检测 & Helpers
#===============================================================================

TUI_AVAILABLE=false
command -v gum &>/dev/null && command -v fzf &>/dev/null && TUI_AVAILABLE=true

# 自动安装 gum + fzf（仅在 root 且当前缺失时执行，失败不致命）
ensure_tui_tools() {
    [[ "$TUI_AVAILABLE" == "true" ]] && return 0
    [[ $EUID -ne 0 ]] && return 0

    local arch fzf_arch
    case "$(uname -m)" in
        x86_64)  arch="x86_64" ; fzf_arch="amd64" ;;
        aarch64) arch="arm64"  ; fzf_arch="arm64" ;;
        armv7l)  arch="armv7"  ; fzf_arch="armhf" ;;
        *) return 0 ;;
    esac

    print_info "安装 TUI 工具（gum + fzf）以启用交互式菜单..."

    if ! command -v gum &>/dev/null; then
        local ver
        ver=$(curl -sI "https://github.com/charmbracelet/gum/releases/latest" \
            | grep -i "^location:" | grep -oE 'v[0-9]+\.[0-9]+\.[0-9]+' | head -1)
        if [[ -n "$ver" ]]; then
            local url="https://github.com/charmbracelet/gum/releases/download/${ver}/gum_${ver#v}_Linux_${arch}.tar.gz"
            local tmp; tmp=$(mktemp -d 2>/dev/null) || tmp=""
            local bin=""
            if [[ -n "$tmp" ]] && curl -fsSL "$url" -o "$tmp/d.tar.gz" 2>/dev/null \
               && tar -xz -C "$tmp" -f "$tmp/d.tar.gz" 2>/dev/null; then
                bin=$(find "$tmp" -name gum -type f 2>/dev/null | head -1)
            fi
            [[ -n "$bin" ]] && install -m 755 "$bin" /usr/local/bin/gum && print_success "gum ${ver} 已安装"
            [[ -n "$tmp" ]] && rm -rf "$tmp"
        fi
    fi

    if ! command -v fzf &>/dev/null; then
        local ver
        ver=$(curl -sI "https://github.com/junegunn/fzf/releases/latest" \
            | grep -i "^location:" | grep -oE 'v[0-9]+\.[0-9]+\.[0-9]+' | head -1)
        if [[ -n "$ver" ]]; then
            local url="https://github.com/junegunn/fzf/releases/download/${ver}/fzf-${ver#v}-linux_${fzf_arch}.tar.gz"
            local tmp; tmp=$(mktemp -d 2>/dev/null) || tmp=""
            local bin=""
            if [[ -n "$tmp" ]] && curl -fsSL "$url" -o "$tmp/d.tar.gz" 2>/dev/null \
               && tar -xz -C "$tmp" -f "$tmp/d.tar.gz" 2>/dev/null; then
                bin=$(find "$tmp" -name fzf -type f 2>/dev/null | head -1)
            fi
            [[ -n "$bin" ]] && install -m 755 "$bin" /usr/local/bin/fzf && print_success "fzf ${ver} 已安装"
            [[ -n "$tmp" ]] && rm -rf "$tmp"
        fi
    fi

    command -v gum &>/dev/null && command -v fzf &>/dev/null && TUI_AVAILABLE=true
}

# 全局 flags（由 parse_global_flags 设置）
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

# 输出（受 --quiet 控制）
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

# gum spin 封装：tui_spin "标题" cmd args...
tui_spin() {
    local title="$1"; shift
    if [[ "$TUI_AVAILABLE" == "true" ]]; then
        gum spin --spinner dot --title "$title" -- "$@"
    else
        tui_info "$title"
        "$@"
    fi
}

# gum confirm 封装：-y flag 时直接返回 0
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

# gum choose 封装（箭头键菜单）：首个参数为 header，其余为选项
# 返回：用户选中的字符串（echoed）
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

# fzf 节点选择封装
# 用法：printf '%s\n' "${lines[@]}" | tui_filter "提示"
# 返回：选中行
tui_filter() {
    local prompt="${1:-搜索...}"
    if [[ "$TUI_AVAILABLE" == "true" ]]; then
        fzf --prompt "$prompt " --mouse --height 50% --border rounded \
            --info inline --layout reverse
    else
        local lines=()
        while IFS= read -r line; do lines+=("$line"); done
        if [[ ${#lines[@]} -eq 0 ]]; then
            return 1
        fi
        local i=1
        for line in "${lines[@]}"; do
            echo "  $i. $line"
            ((i++))
        done
        local choice
        read -p "选择 (1-$((i-1))): " choice
        if ! [[ "$choice" =~ ^[0-9]+$ ]] || (( choice < 1 || choice > ${#lines[@]} )); then
            tui_error "无效选择"
            return 1
        fi
        echo "${lines[$((choice-1))]}"
    fi
}

# gum write 封装（多行输入）
tui_write() {
    local placeholder="${1:-请输入...}"
    if [[ "$TUI_AVAILABLE" == "true" ]]; then
        gum write --placeholder "$placeholder" --char-limit 0 \
            --width 70 --height 8
    else
        echo "请粘贴内容，输入空行结束："
        local lines=()
        while IFS= read -r line; do
            [[ -z "$line" ]] && break
            lines+=("$line")
        done
        printf '%s\n' "${lines[@]}"
    fi
}

# 检查客户端更新
# 同时检测所有源，比较版本号，使用最新的那个
check_client_update() {
    local best_version=""
    local best_source=""
    local best_url=""
    
    # 从 JSON 提取版本号的辅助函数
    _extract_version() {
        local json="$1"
        local ver=$(echo "$json" | grep -oP '"version"\s*:\s*"\K[^"]+' 2>/dev/null | head -1)
        if [[ -z "$ver" ]]; then
            ver=$(echo "$json" | sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)
        fi
        echo "$ver"
    }
    
    # 版本比较: 返回较新的版本
    _newer_version() {
        local v1="$1" v2="$2"
        if [[ -z "$v1" ]]; then echo "$v2"; return; fi
        if [[ -z "$v2" ]]; then echo "$v1"; return; fi
        printf '%s\n' "$v1" "$v2" | sort -V | tail -n1
    }
    
    # 加载服务端地址
    load_server_address
    
    print_info "检测更新中 (多源并行)..."
    
    # 源1: 服务端 (如果已配置)
    if [[ -n "$SERVER_ADDRESS" ]]; then
        local server_json=$(curl -fsSL --max-time 5 "https://${SERVER_ADDRESS}/api/version" 2>/dev/null)
        local server_ver=$(_extract_version "$server_json")
        if [[ -n "$server_ver" ]]; then
            if [[ "$(_newer_version "$server_ver" "$best_version")" == "$server_ver" ]]; then
                best_version="$server_ver"
                best_source="服务端"
                best_url="https://${SERVER_ADDRESS}/api/download/b-ui-client.sh"
            fi
            echo -e "  服务端: ${GREEN}v${server_ver}${NC}"
        fi
    fi
    
    # 源2: 国内镜像 (ghproxy)
    local mirror_json=$(curl -fsSL --max-time 5 "${MIRROR_GHPROXY}/${VERSION_JSON_RAW_URL}" 2>/dev/null)
    local mirror_ver=$(_extract_version "$mirror_json")
    if [[ -n "$mirror_ver" ]]; then
        # 仅当严格更新时才覆盖（服务端版本相同时不替换）
        if [[ "$(_newer_version "$mirror_ver" "$best_version")" == "$mirror_ver" && "$mirror_ver" != "$best_version" ]]; then
            best_version="$mirror_ver"
            best_source="国内镜像"
            best_url="${MIRROR_GHPROXY}/${GITHUB_RAW_URL}"
        fi
        echo -e "  国内镜像: ${GREEN}v${mirror_ver}${NC}"
    fi
    
    # 源3: GitHub Raw
    local github_json=$(curl -fsSL --max-time 5 "$VERSION_JSON_RAW_URL" 2>/dev/null)
    local github_ver=$(_extract_version "$github_json")
    if [[ -n "$github_ver" ]]; then
        # 仅当严格更新时才覆盖（服务端/镜像版本相同时不替换）
        if [[ "$(_newer_version "$github_ver" "$best_version")" == "$github_ver" && "$github_ver" != "$best_version" ]]; then
            best_version="$github_ver"
            best_source="GitHub"
            best_url="$GITHUB_RAW_URL"
        fi
        echo -e "  GitHub: ${GREEN}v${github_ver}${NC}"
    fi
    
    # 保存最佳版本和源供 do_client_update 使用
    REMOTE_VERSION="$best_version"
    UPDATE_SOURCE="$best_source"
    UPDATE_URL="$best_url"
    
    # 版本比较
    if [[ -n "$REMOTE_VERSION" && "$REMOTE_VERSION" != "$SCRIPT_VERSION" ]]; then
        if [[ "$(printf '%s\n' "$REMOTE_VERSION" "$SCRIPT_VERSION" | sort -V | tail -n1)" == "$REMOTE_VERSION" ]]; then
            UPDATE_AVAILABLE="true"
            echo ""
            echo -e "  ${CYAN}最新版本: v${REMOTE_VERSION} (来自 ${UPDATE_SOURCE})${NC}"
            return 0
        fi
    fi
    
    UPDATE_AVAILABLE=""
    return 1
}

# 执行客户端更新
# 优先使用 check_client_update 检测到的最佳源
do_client_update() {
    print_info "正在更新客户端..."
    
    local temp_script="/tmp/b-ui-client-new.sh"
    local download_success=false
    
    # 加载服务端地址
    load_server_address
    
    # 构建下载源列表 (服务端永远排第一，确保国内可达)
    local sources=()
    
    # 服务端优先 (国内可达，速度最快)
    if [[ -n "$SERVER_ADDRESS" ]]; then
        sources+=("https://${SERVER_ADDRESS}/packages/b-ui-client.sh|服务端")
    fi
    
    # 检测到的最佳源 (如果不是服务端，作为第二选择)
    if [[ -n "$UPDATE_URL" && "$UPDATE_SOURCE" != "服务端" ]]; then
        sources+=("${UPDATE_URL}|${UPDATE_SOURCE}")
    fi
    
    # 备选源
    sources+=("${MIRROR_GHPROXY}/${GITHUB_RAW_URL}|国内镜像")
    sources+=("${GITHUB_RAW_URL}|GitHub")
    sources+=("${REMOTE_VERSION_URL}|CDN")
    
    # 依次尝试下载
    for item in "${sources[@]}"; do
        IFS='|' read -r url name <<< "$item"
        print_info "尝试: ${name}..."
        if curl -fsSL --max-time 60 "$url" -o "$temp_script" 2>/dev/null; then
            local lines=$(wc -l < "$temp_script" 2>/dev/null || echo "0")
            if [[ "$lines" -gt 100 ]]; then
                download_success=true
                print_success "从 ${name} 下载成功"
                break
            fi
        fi
    done
    
    if [[ "$download_success" == "true" ]]; then
        cp "$temp_script" /usr/local/bin/bui-c
        chmod +x /usr/local/bin/bui-c
        rm -f "$temp_script"
        
        print_success "客户端已更新至 v${REMOTE_VERSION}"
        echo ""
        print_info "请重新运行 bui-c 使更新生效"
        exit 0
    else
        rm -f "$temp_script"
        print_error "所有下载源均失败"
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
MIRROR_JSR="https://raw.githack.com"

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
        
        # 方法3: raw.githack.com CDN (如果是 raw 文件)
        if [[ "$success" == "false" ]] && [[ "$url" == *"raw.githubusercontent.com"* ]]; then
            # 转换: raw.githubusercontent.com -> raw.githack.com (路径格式相同)
            local githack_url=$(echo "$url" | sed 's|raw\.githubusercontent\.com|raw.githack.com|')
            print_info "尝试 raw.githack.com CDN..."
            if curl -fsSL --max-time 60 "$githack_url" -o "$output" 2>/dev/null; then
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
        # 去掉端口号 (Web 面板通过 HTTPS 443，不是 Hysteria2 端口)
        SERVER_ADDRESS="${SERVER_ADDRESS%%:*}"
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


_write_xray_json() {
    local xray_config="${BASE_DIR}/xray-config.json"
    local server_host=$(echo "$SERVER_ADDR" | cut -d':' -f1)
    local server_port=$(echo "$SERVER_ADDR" | cut -d':' -f2 | tr -cd '0-9')

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
}

generate_xray_config() {
    # 检测端口占用情况
    check_and_suggest_ports 1080 8080

    read -p "SOCKS5 端口 [默认 ${SUGGESTED_SOCKS_PORT}]: " SOCKS_PORT
    SOCKS_PORT=${SOCKS_PORT:-$SUGGESTED_SOCKS_PORT}

    if is_port_in_use "$SOCKS_PORT"; then
        print_warning "端口 $SOCKS_PORT 已被占用，可能会冲突！"
    fi

    read -p "HTTP 端口 [默认 ${SUGGESTED_HTTP_PORT}]: " HTTP_PORT
    HTTP_PORT=${HTTP_PORT:-$SUGGESTED_HTTP_PORT}

    if is_port_in_use "$HTTP_PORT"; then
        print_warning "端口 $HTTP_PORT 已被占用，可能会冲突！"
    fi

    _write_xray_json

    local xray_config="${BASE_DIR}/xray-config.json"
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
      "domain_resolver": "local-dns",
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
      "domain_resolver": "local-dns",
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
    # 按 sing-box 官方文档生成配置
    # 参考: https://sing-box.sagernet.org/manual/proxy/client/
    cat > "$singbox_config" <<EOF
{
  "log": { "level": "info" },
  "dns": {
    "servers": [
      {
        "tag": "proxy-dns",
        "type": "udp",
        "server": "8.8.8.8",
        "detour": "proxy-out"
      },
      {
        "tag": "local-dns",
        "type": "udp",
        "server": "223.5.5.5"
      }
    ],
    "rules": [
      { "domain_suffix": [".cn"], "server": "local-dns" }
    ],
    "final": "proxy-dns",
    "strategy": "ipv4_only"
  },
  "inbounds": [
    {
      "type": "tun",
      "address": ["172.19.0.1/30"],
      "auto_route": true,
      "strict_route": true
    },
    {
      "type": "socks",
      "tag": "socks-in",
      "listen": "127.0.0.1",
      "listen_port": ${SOCKS_PORT}
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
      { "action": "sniff" },
      { "protocol": "dns", "action": "hijack-dns" },
      { "port": [22, 2222], "outbound": "direct-out" },
      { "ip_is_private": true, "outbound": "direct-out" },
      {
        "domain_keyword": ["wechat", "weixin", "tencent", "qq", "xiaohongshu", "douyin", "bytedance", "toutiao", "kuaishou", "bilibili", "taobao", "alipay", "alibaba", "tmall", "jd", "baidu"],
        "outbound": "direct-out"
      },
      {
        "domain_keyword": ["github", "google", "googleapis", "googlevideo", "gstatic", "gmail", "gemini", "generativelanguage", "anthropic", "openai", "chatgpt", "antigravity", "cloudcode", "visualstudio", "vscode"],
        "outbound": "proxy-out"
      },
      {
        "domain_suffix": [".github.com", ".github.io", ".githubusercontent.com", ".githubassets.com", ".google.com", ".google.com.hk", ".google.co.jp", ".goog", ".youtube.com", ".ytimg.com", ".googlesyndication.com", ".googleusercontent.com", ".ggpht.com", ".gemini.google.com", ".antigravity.google", ".antigravity-unleash.goog", ".run.app", ".visualstudio.com", ".vscode-cdn.net", ".aka.ms"],
        "outbound": "proxy-out"
      }
    ],
    "final": "proxy-out",
    "auto_detect_interface": true,
    "default_domain_resolver": "local-dns"
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

# 系统代理环境变量管理
# 让 curl/git/wget/apt 等命令自动走代理，无需 TUN 全局劫持
setup_system_proxy() {
    local socks_port="${1:-1080}"
    local http_port="${2:-8080}"
    
    cat > /etc/profile.d/proxy.sh << EOF
# B-UI 自动代理配置 (bui-c 管理，请勿手动修改)
export http_proxy="http://127.0.0.1:${http_port}"
export https_proxy="http://127.0.0.1:${http_port}"
export all_proxy="socks5://127.0.0.1:${socks_port}"
export HTTP_PROXY="http://127.0.0.1:${http_port}"
export HTTPS_PROXY="http://127.0.0.1:${http_port}"
export ALL_PROXY="socks5://127.0.0.1:${socks_port}"
export no_proxy="localhost,127.0.0.1,::1,10.0.0.0/8,172.16.0.0/12,192.168.0.0/16,.cn"
export NO_PROXY="localhost,127.0.0.1,::1,10.0.0.0/8,172.16.0.0/12,192.168.0.0/16,.cn"
EOF
    chmod 644 /etc/profile.d/proxy.sh
    # 立即在当前 shell 生效
    source /etc/profile.d/proxy.sh 2>/dev/null || true
    print_success "系统代理已配置 (SOCKS5:${socks_port} HTTP:${http_port})"
    echo -e "  ${DIM}新终端自动生效 | 当前终端: source /etc/profile.d/proxy.sh${NC}"
}

remove_system_proxy() {
    rm -f /etc/profile.d/proxy.sh
    # 清除当前 shell 变量
    unset http_proxy https_proxy all_proxy HTTP_PROXY HTTPS_PROXY ALL_PROXY no_proxy NO_PROXY 2>/dev/null || true
}

# 检测公网 IP 和连通性
check_public_ip() {
    local mode="${1:-TUN}"  # 模式标识: TUN 或 切换配置
    echo ""
    echo -e "${CYAN}[网络检测]${NC}"
    
    # IP 检测 API 列表（按优先级）
    local ip_apis=(
        "https://api.ipify.org"
        "https://ip.sb"
        "https://icanhazip.com"
        "https://ifconfig.me"
        "https://ipinfo.io/ip"
    )
    
    local public_ip=""
    local api_used=""
    
    # 尝试获取公网 IP
    for api in "${ip_apis[@]}"; do
        public_ip=$(curl -s --max-time 5 "$api" 2>/dev/null | tr -d '\n')
        if [[ -n "$public_ip" ]] && [[ "$public_ip" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
            api_used="$api"
            break
        fi
        public_ip=""
    done
    
    if [[ -n "$public_ip" ]]; then
        echo -e "  ${GREEN}✓${NC} 公网 IP: ${GREEN}${public_ip}${NC}"
        
        # 尝试获取 IP 归属地
        local ip_info=$(curl -s --max-time 3 "https://ipinfo.io/${public_ip}/json" 2>/dev/null)
        if [[ -n "$ip_info" ]]; then
            local country=$(echo "$ip_info" | grep -o '"country": "[^"]*"' | cut -d'"' -f4)
            local city=$(echo "$ip_info" | grep -o '"city": "[^"]*"' | cut -d'"' -f4)
            local org=$(echo "$ip_info" | grep -o '"org": "[^"]*"' | cut -d'"' -f4)
            if [[ -n "$country" ]]; then
                echo -e "  ${GREEN}✓${NC} 归属地: ${YELLOW}${city:-Unknown}, ${country}${NC}"
            fi
            if [[ -n "$org" ]]; then
                echo -e "  ${GREEN}✓${NC} 运营商: ${YELLOW}${org}${NC}"
            fi
        fi
    else
        echo -e "  ${RED}✗${NC} 无法获取公网 IP (网络可能不通)"
    fi
    
    # 连通性测试
    echo ""
    echo -e "${CYAN}[连通性测试]${NC}"
    
    # 测试 Google
    if curl -s --max-time 5 -o /dev/null -w '' "https://www.google.com" 2>/dev/null; then
        echo -e "  ${GREEN}✓${NC} Google: 可访问"
    else
        echo -e "  ${RED}✗${NC} Google: 不可访问"
    fi
    
    # 测试 YouTube
    if curl -s --max-time 5 -o /dev/null -w '' "https://www.youtube.com" 2>/dev/null; then
        echo -e "  ${GREEN}✓${NC} YouTube: 可访问"
    else
        echo -e "  ${RED}✗${NC} YouTube: 不可访问"
    fi
    
    # 测试 GitHub
    if curl -s --max-time 5 -o /dev/null -w '' "https://github.com" 2>/dev/null; then
        echo -e "  ${GREEN}✓${NC} GitHub: 可访问"
    else
        echo -e "  ${RED}✗${NC} GitHub: 不可访问"
    fi
    
    echo ""
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
    
    # 开启 IP 转发 (TUN 必需)
    sysctl -w net.ipv4.ip_forward=1 >/dev/null 2>&1 || true
    
    # 防火墙适配：UFW 与 sing-box TUN strict_route 不兼容
    # TUN 模式下 sing-box 接管全局路由，UFW 的 FORWARD DROP 会阻断 TCP 流量
    if command -v ufw &>/dev/null && ufw status 2>/dev/null | grep -q "active"; then
        print_warning "检测到 UFW 防火墙 (与 TUN 冲突)"
        print_info "暂停 UFW (TUN 停止后自动恢复)..."
        ufw disable >/dev/null 2>&1 || true
        # 标记 UFW 需要在 TUN 停止时恢复
        echo "ufw_was_active=true" > /opt/hysteria-client/.ufw_state
        print_success "UFW 已暂停"
    fi
    
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
        # 自动检测公网 IP 和连通性
        check_public_ip "TUN"
    else
        print_error "TUN 模式启动失败"
        journalctl -u bui-tun --no-pager -n 10
    fi
}

stop_tun_mode() {
    print_info "停止 TUN 模式..."
    
    # 1. 先禁用服务 (防止自动重启)
    systemctl disable bui-tun 2>/dev/null || true
    
    # 2. 停止服务
    systemctl stop bui-tun 2>/dev/null || true
    
    # 3. 等待服务完全停止
    local wait_count=0
    while systemctl is-active --quiet bui-tun 2>/dev/null && [[ $wait_count -lt 10 ]]; do
        sleep 0.5
        ((wait_count++))
    done
    
    # 4. 如果服务还在运行,强制停止
    if systemctl is-active --quiet bui-tun 2>/dev/null; then
        print_warning "服务未能正常停止,强制终止..."
        systemctl kill -s SIGKILL bui-tun 2>/dev/null || true
        sleep 1
    fi
    
    # 5. 清理所有可能的 sing-box TUN 相关进程
    pkill -9 -f "sing-box.*singbox-tun" 2>/dev/null || true
    pkill -9 -f "sing-box run -c /opt/hysteria-client/singbox-tun" 2>/dev/null || true
    
    # 6. 删除 TUN 接口
    ip link delete bui-tun 2>/dev/null || true
    
    # 7. 验证停止成功
    if systemctl is-active --quiet bui-tun 2>/dev/null; then
        print_error "TUN 模式停止失败!"
        return 1
    fi
    
    if ip link show bui-tun 2>/dev/null; then
        print_warning "TUN 接口残留,尝试删除..."
        ip link set bui-tun down 2>/dev/null || true
        ip link delete bui-tun 2>/dev/null || true
    fi
    
    # 8. 恢复 Hysteria2/Xray 客户端服务（TUN 停止后用户仍需代理）
    if [[ -f "/etc/systemd/system/$CLIENT_SERVICE" ]]; then
        print_info "恢复 Hysteria2 客户端..."
        systemctl start "$CLIENT_SERVICE" 2>/dev/null || true
        sleep 1
        if systemctl is-active --quiet "$CLIENT_SERVICE"; then
            print_success "Hysteria2 客户端已恢复"
            local p_socks=$(grep -A1 '^socks5:' "$CONFIG_FILE" 2>/dev/null | grep 'listen:' | sed 's/.*://')
            local p_http=$(grep -A1 '^http:' "$CONFIG_FILE" 2>/dev/null | grep 'listen:' | sed 's/.*://')
            setup_system_proxy "${p_socks:-1080}" "${p_http:-8080}"
        fi
    elif [[ -f /etc/systemd/system/xray-client.service ]]; then
        print_info "恢复 Xray 客户端..."
        systemctl start xray-client 2>/dev/null || true
        sleep 1
        if systemctl is-active --quiet xray-client; then
            print_success "Xray 客户端已恢复"
        fi
    fi
    
    # 9. 恢复 UFW 防火墙（如果被 TUN 暂停过）
    if [[ -f /opt/hysteria-client/.ufw_state ]]; then
        source /opt/hysteria-client/.ufw_state
        if [[ "$ufw_was_active" == "true" ]]; then
            print_info "恢复 UFW 防火墙..."
            ufw --force enable >/dev/null 2>&1 || true
            print_success "UFW 已恢复"
        fi
        rm -f /opt/hysteria-client/.ufw_state
    fi
    
    print_success "TUN 模式已停止"
}

#===============================================================================
# URI 解析和导入
#===============================================================================


# 安全导入 URI 解析结果（替代 eval，防止命令注入）
safe_import_parsed() {
    local parsed="$1"
    # 白名单验证：只允许已知的变量名
    local allowed_vars="PROTOCOL SERVER_ADDR AUTH_PASSWORD SNI INSECURE MPORT REMARK UUID SECURITY FINGERPRINT PUBLIC_KEY SHORT_ID FLOW NETWORK"
    
    while IFS='=' read -r key value; do
        [[ -z "$key" || "$key" =~ ^# ]] && continue
        # 检查变量名是否在白名单中
        local allowed=false
        for var in $allowed_vars; do
            if [[ "$key" == "$var" ]]; then
                allowed=true
                break
            fi
        done
        if [[ "$allowed" == "true" ]]; then
            # 使用 printf %q 确保值被正确转义
            printf -v "$key" '%s' "$value"
        fi
    done <<< "$parsed"
}

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
    local config_name="$1"
    local protocol="$2"
    local server="$3"
    local uri="$4"
    local socks_port="${5:-1080}"
    local http_port="${6:-8080}"

    local config_dir="${CONFIGS_DIR}/${config_name}"
    mkdir -p "$config_dir" || return 1

    cat > "${config_dir}/meta.json" << EOF
{
    "name": "${config_name}",
    "protocol": "${protocol}",
    "server": "${server}",
    "socks_port": ${socks_port},
    "http_port": ${http_port},
    "createdAt": "$(date -Iseconds)"
}
EOF
    [[ $? -eq 0 ]] || return 1

    echo "$uri" > "${config_dir}/uri.txt" || return 1

    local server_host
    server_host=$(echo "$server" | cut -d':' -f1)
    if [[ -n "$server_host" ]]; then
        set_server_address "$server_host" 2>/dev/null || true
    fi

    return 0
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

_switch_to_profile() {
    local selected="$1"
    local config_dir="${CONFIGS_DIR}/${selected}"
    local meta_file="${config_dir}/meta.json"
    local uri_file="${config_dir}/uri.txt"

    if [[ ! -d "$config_dir" ]]; then
        tui_error "节点 '$selected' 不存在"
        return 3
    fi

    if [[ ! -f "$uri_file" ]]; then
        tui_error "配置损坏：缺少 uri.txt"
        return 1
    fi

    local protocol
    protocol=$(grep '"protocol"' "$meta_file" 2>/dev/null | cut -d'"' -f4)
    local uri
    uri=$(cat "$uri_file")

    # 读取端口（三级回退：meta.json → config.yaml → 默认值）
    local stored_socks stored_http
    stored_socks=$(grep '"socks_port"' "$meta_file" 2>/dev/null | grep -o '[0-9]*' | head -1)
    stored_http=$(grep '"http_port"'  "$meta_file" 2>/dev/null | grep -o '[0-9]*' | head -1)
    if [[ -z "$stored_socks" ]] && [[ -f "${config_dir}/config.yaml" ]]; then
        stored_socks=$(grep -A1 '^socks5:' "${config_dir}/config.yaml" | grep 'listen:' | sed 's/.*://')
    fi
    if [[ -z "$stored_http" ]] && [[ -f "${config_dir}/config.yaml" ]]; then
        stored_http=$(grep -A1 '^http:' "${config_dir}/config.yaml" | grep 'listen:' | sed 's/.*://')
    fi
    # 记录 TUN 状态
    local tun_was_active=false
    systemctl is-active --quiet bui-tun 2>/dev/null && tun_was_active=true
    [[ "$tun_was_active" == "true" ]] && tui_info "检测到 TUN 运行中，切换后自动重启..."

    # 停止 TUN
    if [[ "$tun_was_active" == "true" ]]; then
        tui_info "停止 TUN 模式..."; stop_tun_mode
    fi

    # 停止客户端服务
    tui_spin "停止当前服务..." bash -c "
        systemctl stop '$CLIENT_SERVICE' 2>/dev/null || true
        systemctl stop xray-client 2>/dev/null || true
    "

    # 解析 URI
    local parsed=""
    if [[ "$uri" =~ ^(hysteria2|hy2):// ]]; then
        parsed=$(parse_hysteria_uri "$uri")
    elif [[ "$uri" =~ ^vless:// ]]; then
        parsed=$(parse_vless_uri "$uri")
    else
        tui_error "不支持的 URI 格式"
        return 1
    fi
    safe_import_parsed "$parsed"
    SOCKS_PORT="${stored_socks:-1080}"
    HTTP_PORT="${stored_http:-8080}"

    # 生成配置并启动
    if [[ "$protocol" == "hysteria2" ]]; then
        tui_info "生成 Hysteria2 配置..."; generate_config
        cp "$CONFIG_FILE" "${config_dir}/config.yaml"
        tui_info "创建 Hysteria2 服务配置..."; create_service
        tui_spin "启动 Hysteria2 服务..." systemctl start "$CLIENT_SERVICE"
    else
        tui_info "生成 Xray 配置..."; _write_xray_json
        if [[ ! -f /etc/systemd/system/xray-client.service ]]; then
            local xray_cfg="${BASE_DIR}/xray-config.json"
            printf '%s\n' \
                '[Unit]' \
                'Description=Xray Client' \
                'After=network.target' \
                '' \
                '[Service]' \
                'Type=simple' \
                "ExecStart=/usr/local/bin/xray run -config ${xray_cfg}" \
                'Restart=always' \
                'RestartSec=5' \
                '' \
                '[Install]' \
                'WantedBy=multi-user.target' \
                > /etc/systemd/system/xray-client.service
            systemctl daemon-reload
            systemctl enable xray-client 2>/dev/null || true
        fi
        cp "${BASE_DIR}/xray-config.json" "${config_dir}/xray-config.json"
        tui_spin "启动 Xray 服务..." systemctl start xray-client
    fi

    echo "$selected" > "$ACTIVE_CONFIG"

    # 重新生成 TUN 配置
    local tun_protocol="${protocol:-hysteria2}"
    [[ "$tun_protocol" != "hysteria2" ]] && tun_protocol="vless-reality"
    tui_info "重新生成 TUN 配置..."; generate_singbox_tun_config "$tun_protocol"

    if [[ "$tun_was_active" == "true" ]]; then
        tui_info "重启 TUN 模式..."; start_tun_mode
    fi

    tui_success "已切换到: $selected"

    if [[ "$tun_was_active" != "true" ]] && [[ "$OPT_JSON" != "true" ]]; then
        local proxy_ip
        proxy_ip=$(curl -s --max-time 5 --socks5 "127.0.0.1:${SOCKS_PORT}" \
            "https://api.ipify.org" 2>/dev/null)
        if [[ -n "$proxy_ip" ]] && [[ "$proxy_ip" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
            tui_success "代理 IP: ${proxy_ip}"
        fi
    fi

    return 0
}

#===============================================================================
# 非交互子命令
#===============================================================================

cmd_status() {
    local active
    active=$(get_active_config)
    local protocol="" socks_port=1080 http_port=8080

    if [[ -n "$active" ]] && [[ -f "${CONFIGS_DIR}/${active}/meta.json" ]]; then
        local meta="${CONFIGS_DIR}/${active}/meta.json"
        protocol=$(grep '"protocol"' "$meta" | cut -d'"' -f4)
        socks_port=$(grep '"socks_port"' "$meta" | grep -o '[0-9]*' | head -1)
        http_port=$(grep '"http_port"'  "$meta" | grep -o '[0-9]*' | head -1)
    fi
    socks_port="${socks_port:-1080}"
    http_port="${http_port:-8080}"

    local svc_status="stopped"
    systemctl is-active --quiet "$CLIENT_SERVICE" 2>/dev/null && svc_status="running"
    if [[ "$svc_status" == "stopped" ]]; then
        systemctl is-active --quiet xray-client 2>/dev/null && svc_status="running"
    fi

    local tun_status="stopped"
    systemctl is-active --quiet bui-tun 2>/dev/null && tun_status="running"

    if [[ "$OPT_JSON" == "true" ]]; then
        printf '{\n'
        printf '  "active_node": "%s",\n' "${active:-}"
        printf '  "protocol": "%s",\n' "${protocol:-}"
        printf '  "service": "%s",\n' "$svc_status"
        printf '  "socks_port": %s,\n' "$socks_port"
        printf '  "http_port": %s,\n' "$http_port"
        printf '  "tun": "%s"\n' "$tun_status"
        printf '}\n'
    else
        echo "节点:    ${active:-(未设置)}"
        echo "协议:    ${protocol:-(未知)}"
        echo "服务:    $svc_status"
        echo "SOCKS5:  127.0.0.1:$socks_port"
        echo "HTTP:    127.0.0.1:$http_port"
        echo "TUN:     $tun_status"
    fi
}

cmd_list() {
    if [[ ! -d "$CONFIGS_DIR" ]] || [[ -z "$(ls -A "$CONFIGS_DIR" 2>/dev/null)" ]]; then
        if [[ "$OPT_JSON" == "true" ]]; then
            echo "[]"
        else
            echo "没有已保存的节点"
        fi
        return 0
    fi

    local active
    active=$(get_active_config)

    if [[ "$OPT_JSON" == "true" ]]; then
        printf '[\n'
        local first=true
        for config_dir in "$CONFIGS_DIR"/*/; do
            [[ ! -d "$config_dir" ]] && continue
            local name; name=$(basename "$config_dir")
            local meta="${config_dir}meta.json"
            local protocol; protocol=$(grep '"protocol"' "$meta" 2>/dev/null | cut -d'"' -f4)
            local server; server=$(grep '"server"' "$meta" 2>/dev/null | cut -d'"' -f4)
            local is_active="false"
            [[ "$name" == "$active" ]] && is_active="true"
            [[ "$first" == "false" ]] && printf ',\n'
            printf '  {"name": "%s", "protocol": "%s", "server": "%s", "active": %s}' \
                "$name" "$protocol" "$server" "$is_active"
            first=false
        done
        printf '\n]\n'
    else
        for config_dir in "$CONFIGS_DIR"/*/; do
            [[ ! -d "$config_dir" ]] && continue
            local name; name=$(basename "$config_dir")
            local meta="${config_dir}meta.json"
            local protocol; protocol=$(grep '"protocol"' "$meta" 2>/dev/null | cut -d'"' -f4)
            local server; server=$(grep '"server"' "$meta" 2>/dev/null | cut -d'"' -f4)
            local marker=""
            [[ "$name" == "$active" ]] && marker=" ★"
            printf "%-30s  %-16s  %s%s\n" "$name" "$protocol" "$server" "$marker"
        done
    fi
}

cmd_switch() {
    local name="$1"
    if [[ -z "$name" ]]; then
        echo "用法: bui-c switch <节点名>" >&2
        exit 2
    fi
    if [[ ! -d "${CONFIGS_DIR}/${name}" ]]; then
        tui_error "节点 '$name' 不存在。用 'bui-c list' 查看可用节点。"
        exit 3
    fi
    _switch_to_profile "$name"
    local rc=$?
    exit $rc
}

cmd_tun() {
    local action="${1:-status}"
    case "$action" in
        on|start|enable)
            if systemctl is-active --quiet bui-tun 2>/dev/null; then
                tui_info "TUN 已在运行中"
                exit 0
            fi
            tui_info "启动 TUN 模式..."; start_tun_mode
            exit $?
            ;;
        off|stop|disable)
            if ! systemctl is-active --quiet bui-tun 2>/dev/null; then
                tui_info "TUN 未在运行"
                exit 0
            fi
            tui_info "停止 TUN 模式..."; stop_tun_mode
            exit $?
            ;;
        status)
            if systemctl is-active --quiet bui-tun 2>/dev/null; then
                echo "running"
            else
                echo "stopped"
            fi
            exit 0
            ;;
        *)
            echo "用法: bui-c tun <on|off|status>" >&2
            exit 2
            ;;
    esac
}

cmd_import() {
    local uri="$1"
    local activate=false
    [[ "$2" == "--activate" ]] && activate=true

    if [[ -z "$uri" ]]; then
        echo "用法: bui-c import <uri> [--activate]" >&2
        exit 2
    fi

    local parsed="" protocol=""
    if [[ "$uri" =~ ^(hysteria2|hy2):// ]]; then
        parsed=$(parse_hysteria_uri "$uri")
        protocol="hysteria2"
    elif [[ "$uri" =~ ^vless:// ]]; then
        parsed=$(parse_vless_uri "$uri")
        protocol="vless-reality"
    else
        tui_error "不支持的 URI 格式（仅支持 hysteria2:// 和 vless://）"
        exit 2
    fi

    if [[ -z "$parsed" ]]; then
        tui_error "URI 解析失败"
        exit 1
    fi

    # 从 parsed 输出中提取 remark（不污染全局变量）
    local remark
    remark=$(echo "$parsed" | grep '^REMARK=' | cut -d= -f2-)
    local server_addr
    server_addr=$(echo "$parsed" | grep '^SERVER_ADDR=' | cut -d= -f2-)

    # 生成配置名称（sanitize: 只保留字母数字连字符下划线点）
    local config_name="${remark:-${protocol}-$(date +%s)}"
    config_name=$(echo "$config_name" | tr -s ' ' '-' | sed 's/[^a-zA-Z0-9._-]/-/g' | sed 's/--*/-/g' | sed 's/^-//;s/-$//')
    [[ -z "$config_name" ]] && config_name="${protocol}-$(date +%s)"

    if [[ -d "${CONFIGS_DIR}/${config_name}" ]]; then
        tui_info "节点 '$config_name' 已存在，跳过"
        exit 0
    fi

    # 现在才应用到全局变量
    safe_import_parsed "$parsed"

    if ! save_config_meta "$config_name" "$protocol" "${server_addr:-$SERVER_ADDR}" "$uri" 1080 8080; then
        tui_error "保存配置失败"
        exit 1
    fi
    tui_success "已导入: $config_name ($protocol)"

    if [[ "$activate" == "true" ]]; then
        OPT_YES=true
        _switch_to_profile "$config_name"
        exit $?
    fi

    exit 0
}

cmd_start() {
    local active
    active=$(get_active_config)
    if [[ -z "$active" ]]; then
        tui_error "没有激活的节点，请先 bui-c switch <名称>"
        exit 1
    fi
    local protocol
    protocol=$(grep '"protocol"' "${CONFIGS_DIR}/${active}/meta.json" 2>/dev/null | cut -d'"' -f4)
    if [[ -z "$protocol" ]]; then
        tui_error "无法读取节点协议信息，尝试 bui-c switch <名称> 重新激活"
        exit 1
    fi
    if [[ "$protocol" == "hysteria2" ]]; then
        tui_info "启动 Hysteria2..."; systemctl start "$CLIENT_SERVICE"
    else
        tui_info "启动 Xray..."; systemctl start xray-client
    fi
    tui_success "服务已启动"
    exit 0
}

cmd_stop() {
    tui_info "停止客户端服务..."
    systemctl stop "$CLIENT_SERVICE" 2>/dev/null || true
    systemctl stop xray-client 2>/dev/null || true
    systemctl stop bui-tun 2>/dev/null || true
    tui_success "服务已停止"
    exit 0
}

cmd_restart() {
    local active
    active=$(get_active_config)
    if [[ -z "$active" ]]; then
        tui_error "没有激活的节点"
        exit 1
    fi
    tui_info "停止客户端服务..."
    systemctl stop "$CLIENT_SERVICE" 2>/dev/null || true
    systemctl stop xray-client 2>/dev/null || true
    systemctl stop bui-tun 2>/dev/null || true
    _switch_to_profile "$active"
    exit $?
}

switch_config() {
    echo ""
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}切换配置${NC}"
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"

    if ! list_configs; then
        return 1
    fi

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
    _switch_to_profile "$selected"
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
        safe_import_parsed "$parsed"
        
        # 使用备注名或生成配置名
        local config_name="${REMARK:-config-$(date +%s)}"
        # 清理配置名中的特殊字符
        config_name=$(echo "$config_name" | sed 's/[\/\\:*?"<>|]/-/g')
        
        # 检查是否已存在
        if [[ -d "${CONFIGS_DIR}/${config_name}" ]]; then
            echo -e "  ${YELLOW}○${NC} 已存在: ${config_name} (跳过)"
            continue
        fi
        
        # 保存配置元信息（批量导入使用默认端口，激活时可调整）
        save_config_meta "$config_name" "$protocol" "$SERVER_ADDR" "$uri" 1080 8080

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
    
    safe_import_parsed "$parsed"
    
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
        
        # 配置系统代理（让 curl/git/apt 自动走代理）
        setup_system_proxy "$SOCKS_PORT" "$HTTP_PORT"
        
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
    
    # 更新 meta.json 中的端口信息（端口在上面两个分支中已确定）
    save_config_meta "$config_name" "$PROTOCOL" "$SERVER_ADDR" "$uri" "${SOCKS_PORT:-1080}" "${HTTP_PORT:-8080}"

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
    safe_import_parsed "$parsed"
    
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

    if [[ "$PROTOCOL" == "hysteria2" ]]; then
        # Hysteria2 配置 - 检测端口占用情况
        check_and_suggest_ports 1080 8080

        read -p "SOCKS5 端口 [默认 ${SUGGESTED_SOCKS_PORT}]: " SOCKS_PORT
        SOCKS_PORT=${SOCKS_PORT:-$SUGGESTED_SOCKS_PORT}

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
        # VLESS-Reality 配置 (需要 Xray，generate_xray_config 内部会设置 SOCKS_PORT/HTTP_PORT)
        install_xray_client
        generate_xray_config

        # 复制配置到配置目录
        cp "${BASE_DIR}/xray-config.json" "${config_dir}/xray-config.json"
    fi

    # 保存配置元信息（端口已在上面两个分支中确定）
    save_config_meta "$config_name" "$PROTOCOL" "$SERVER_ADDR" "$uri" "${SOCKS_PORT:-1080}" "${HTTP_PORT:-8080}"

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
    local server_ip=$(dig +short "$server_host" A 2>/dev/null | grep -E '^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$' | head -1)
    # fallback: getent 解析
    [[ -z "$server_ip" ]] && server_ip=$(getent ahostsv4 "$server_host" 2>/dev/null | awk '{print $1; exit}')
    # 最终验证：必须是合法 IP 格式
    if [[ ! "$server_ip" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
        server_ip=""
    fi
    
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

# 保活：防止 QUIC 空闲超时静默断连
quic:
  keepAlivePeriod: 10s

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

    # TUN 模式：Hysteria2 只提供 SOCKS/HTTP 代理, sing-box 管全局路由
    # 非 TUN 模式：不需要额外配置
    # 两种模式都不再向 Hysteria2 config 添加 TUN/DNS/ACL 段
    # （sing-box 有自己的 DNS 和路由规则）
    
    # 仅当有自定义绕过规则且非 TUN 模式时，添加 ACL
    if [[ "$TUN_ENABLED" != "true" ]] && [[ -n "$custom_acl_rules" ]]; then
        cat >> "$CONFIG_FILE" << EOF

# ACL 路由规则
acl:
  inline:$(echo -e "$custom_acl_rules")
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
        
        # 始终从当前激活配置重新生成 TUN 配置 (防止切换配置后使用旧的 singbox-tun.json)
        local protocol=""
        
        if [[ -f "$CONFIG_FILE" ]]; then
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

show_status_bar() {
    local active; active=$(get_active_config)
    local protocol="" socks_port=1080 http_port=8080

    if [[ -n "$active" ]] && [[ -f "${CONFIGS_DIR}/${active}/meta.json" ]]; then
        local meta="${CONFIGS_DIR}/${active}/meta.json"
        protocol=$(grep '"protocol"' "$meta" | cut -d'"' -f4)
        socks_port=$(grep '"socks_port"' "$meta" | grep -o '[0-9]*' | head -1)
        http_port=$(grep '"http_port"'  "$meta" | grep -o '[0-9]*' | head -1)
    fi
    socks_port="${socks_port:-1080}"
    http_port="${http_port:-8080}"

    local svc_icon="🔴" tun_icon="🔴" tun_label="关闭"
    systemctl is-active --quiet "$CLIENT_SERVICE" 2>/dev/null && svc_icon="🟢"
    systemctl is-active --quiet xray-client 2>/dev/null && svc_icon="🟢"
    if systemctl is-active --quiet bui-tun 2>/dev/null; then
        tun_icon="🟢"; tun_label="运行中"
    fi

    if [[ "$TUI_AVAILABLE" == "true" ]]; then
        gum style \
            --border rounded --border-foreground 39 \
            --padding "0 2" --margin "1 0" \
            "$(gum style --bold 'B-UI Client')" \
            "" \
            "节点   ${svc_icon}  $(gum style --foreground 46 "${active:-(未设置)}")  $(gum style --faint "${protocol}")" \
            "代理       SOCKS5 :${socks_port}  HTTP :${http_port}" \
            "TUN    ${tun_icon}  ${tun_label}"
    else
        echo ""
        echo -e "  节点: ${svc_icon} ${active:-(未设置)} (${protocol})"
        echo -e "  代理: SOCKS5 :${socks_port}  HTTP :${http_port}"
        echo -e "  TUN:  ${tun_icon} ${tun_label}"
        echo ""
    fi
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
    
    # GitHub 延迟
    echo -n "  GitHub 延迟: "
    local gh_latency=""
    if $tun_running; then
        gh_latency=$(curl -s -o /dev/null -w '%{time_total}' --max-time 10 https://github.com 2>/dev/null)
    else
        gh_latency=$(curl -s -o /dev/null -w '%{time_total}' --max-time 10 --socks5-hostname "127.0.0.1:${socks_port}" https://github.com 2>/dev/null)
    fi
    if [[ -n "$gh_latency" ]] && awk "BEGIN {exit !($gh_latency > 0)}" 2>/dev/null; then
        local gh_ms=$(awk "BEGIN {printf \"%.0f\", $gh_latency * 1000}" 2>/dev/null)
        if [[ "$gh_ms" -lt 500 ]]; then
            echo -e "${GREEN}${gh_ms}ms${NC}"
        elif [[ "$gh_ms" -lt 1000 ]]; then
            echo -e "${YELLOW}${gh_ms}ms${NC}"
        else
            echo -e "${RED}${gh_ms}ms${NC}"
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
    
    # 0. 强制停止所有相关进程（防止残留）
    print_info "停止所有代理进程..."
    systemctl stop bui-tun 2>/dev/null || true
    systemctl stop "$CLIENT_SERVICE" 2>/dev/null || true
    systemctl stop xray-client 2>/dev/null || true
    pkill -9 sing-box 2>/dev/null || true
    pkill -9 hysteria 2>/dev/null || true
    pkill -9 xray 2>/dev/null || true
    ip link delete bui-tun 2>/dev/null || true
    ip link delete hystun 2>/dev/null || true
    sleep 1
    echo -e "  ${GREEN}✓${NC} 所有代理进程已停止"
    
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
# 统一导入节点（自动识别：链接 / 批量 / 订阅）
#===============================================================================

import_node() {
    echo ""
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}📥 导入节点${NC}"
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo ""
    echo -e "支持以下输入（自动识别）:"
    echo -e "  ${YELLOW}• 协议链接${NC}  hysteria2://... 或 vless://..."
    echo -e "  ${YELLOW}• 订阅地址${NC}  https://... (自动识别)"
    echo -e "  ${YELLOW}• 批量粘贴${NC}  每行一个链接，空行结束"
    echo -e "  ${YELLOW}• 输入 m${NC}    手动配置 Hysteria2"
    echo ""
    echo -e "${YELLOW}请粘贴链接 (空行结束):${NC}"
    echo ""
    
    local lines=()
    while IFS= read -r line; do
        [[ -z "$line" ]] && break
        line=$(echo "$line" | xargs)
        [[ -n "$line" ]] && lines+=("$line")
    done
    
    if [[ ${#lines[@]} -eq 0 ]]; then
        print_warning "没有输入任何内容"
        return 1
    fi
    
    local first="${lines[0]}"
    
    # 手动配置快捷键
    if [[ ${#lines[@]} -eq 1 && ( "$first" == "m" || "$first" == "M" ) ]]; then
        quick_install
        return $?
    fi
    
    # 自动识别订阅地址 (https:// 开头)
    if [[ ${#lines[@]} -eq 1 && "$first" =~ ^https?:// ]]; then
        print_info "检测到订阅地址，进入订阅导入..."
        import_from_subscription "$first"
        return $?
    fi
    
    # 收集所有有效 URI
    local uris=()
    for line in "${lines[@]}"; do
        [[ "$line" =~ ^# ]] && continue
        if [[ "$line" =~ ^(hysteria2|hy2|vless):// ]]; then
            uris+=("$line")
        elif [[ "$line" =~ ^https?:// ]]; then
            # 多行粘贴中的订阅地址，提示用户
            echo -e "  ${YELLOW}⚠${NC} 跳过订阅地址 (请单独粘贴): ${line:0:60}..."
        else
            echo -e "  ${YELLOW}⚠${NC} 跳过不支持的格式: ${line:0:60}..."
        fi
    done
    
    if [[ ${#uris[@]} -eq 0 ]]; then
        print_error "没有识别到有效链接"
        return 1
    fi
    
    echo ""
    print_info "识别到 ${#uris[@]} 个链接，开始导入..."
    echo ""
    
    # 复用 import_batch 的核心逻辑：只保存元信息
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
        fi
        
        if [[ -z "$parsed" ]]; then
            echo -e "  ${RED}✗${NC} 解析失败: ${uri:0:50}..."
            ((fail_count++))
            continue
        fi
        
        safe_import_parsed "$parsed"
        
        local config_name="${REMARK:-${protocol}-$(date +%s)}"
        config_name=$(echo "$config_name" | sed 's/[\/\\:*?"<>|]/-/g')
        
        # 已存在则跳过
        if [[ -d "${CONFIGS_DIR}/${config_name}" ]]; then
            echo -e "  ${YELLOW}○${NC} 已存在: ${config_name} (跳过)"
            continue
        fi
        
        # 保存配置元信息和原始 URI（批量导入使用默认端口，激活时可调整）
        save_config_meta "$config_name" "$protocol" "$SERVER_ADDR" "$uri" 1080 8080

        echo -e "  ${GREEN}✓${NC} ${config_name} (${protocol})"
        ((success_count++))
    done

    echo ""
    echo -e "导入完成: ${GREEN}${success_count} 成功${NC}  ${RED}${fail_count} 失败${NC}"
    
    if [[ $success_count -gt 0 ]]; then
        echo ""
        read -p "是否现在选择一个配置并启用? (y/n): " activate
        if [[ "$activate" =~ ^[yY]$ ]]; then
            activate_imported_config
        fi
    fi
}

#===============================================================================
# 子菜单：服务控制
#===============================================================================

service_control_menu() {
    while true; do
        local hy_status xray_status tun_status
        local hy_label xray_label tun_label
        
        if systemctl is-active --quiet "$CLIENT_SERVICE" 2>/dev/null; then
            hy_status="running"
            local hy_ports=$(ss -tlnp 2>/dev/null | grep hysteria | grep -oP ':\K\d+' | sort -u | tr '\n' '/' | sed 's/\/$//')
            hy_label="${GREEN}运行中${NC} (${hy_ports:-?})"
        else
            hy_status="stopped"
            hy_label="${RED}已停止${NC}"
        fi
        
        if systemctl is-active --quiet xray-client 2>/dev/null; then
            xray_status="running"
            xray_label="${GREEN}运行中${NC}"
        elif [[ -f /etc/systemd/system/xray-client.service ]]; then
            xray_status="stopped"
            xray_label="${RED}已停止${NC}"
        else
            xray_status="none"
            xray_label="${DIM}未配置${NC}"
        fi
        
        if systemctl is-active --quiet bui-tun 2>/dev/null; then
            tun_label="${GREEN}运行中${NC}"
        elif [[ -f /etc/systemd/system/bui-tun.service ]]; then
            tun_label="${YELLOW}已停止${NC}"
        else
            tun_label="${DIM}未配置${NC}"
        fi
        
        echo ""
        echo -e "${CYAN}╔══════════════════════════════════════════════════════════════╗${NC}"
        echo -e "${CYAN}║${NC}  ${GREEN}▶ 服务控制${NC}                                                ${CYAN}║${NC}"
        echo -e "${CYAN}╠══════════════════════════════════════════════════════════════╣${NC}"
        echo -e "${CYAN}║${NC}  Hysteria2: $hy_label"
        echo -e "${CYAN}║${NC}  Xray:      $xray_label"
        echo -e "${CYAN}║${NC}  TUN 模式:  $tun_label  ${DIM}(主菜单选 4 控制)${NC}"
        echo -e "${CYAN}╠══════════════════════════════════════════════════════════════╣${NC}"
        
        if [[ "$hy_status" == "running" ]]; then
            echo -e "${CYAN}║${NC}  ${YELLOW}1.${NC} ⏹  停止 Hysteria2"
            echo -e "${CYAN}║${NC}  ${YELLOW}2.${NC} 🔄 重启 Hysteria2"
        else
            echo -e "${CYAN}║${NC}  ${YELLOW}1.${NC} ▶  启动 Hysteria2"
        fi
        
        if [[ "$xray_status" == "running" ]]; then
            echo -e "${CYAN}║${NC}  ${YELLOW}3.${NC} ⏹  停止 Xray"
        elif [[ "$xray_status" == "stopped" ]]; then
            echo -e "${CYAN}║${NC}  ${YELLOW}3.${NC} ▶  启动 Xray"
        fi
        
        echo -e "${CYAN}║${NC}  ${YELLOW}5.${NC} 📋 查看日志"
        echo -e "${CYAN}║${NC}  ${YELLOW}0.${NC} 返回主菜单"
        echo -e "${CYAN}╚══════════════════════════════════════════════════════════════╝${NC}"
        read -p "请选择 [0-5]: " sub
        case $sub in
            1)
                if [[ "$hy_status" == "running" ]]; then
                    systemctl stop "$CLIENT_SERVICE" 2>/dev/null || true
                    remove_system_proxy
                    print_success "Hysteria2 已停止"
                else
                    if [[ -f "/etc/systemd/system/$CLIENT_SERVICE" ]]; then
                        systemctl start "$CLIENT_SERVICE" 2>/dev/null || true
                        sleep 1
                        if systemctl is-active --quiet "$CLIENT_SERVICE"; then
                            local p_socks=$(grep -A1 '^socks5:' "$CONFIG_FILE" 2>/dev/null | grep 'listen:' | sed 's/.*://')
                            local p_http=$(grep -A1 '^http:' "$CONFIG_FILE" 2>/dev/null | grep 'listen:' | sed 's/.*://')
                            setup_system_proxy "${p_socks:-1080}" "${p_http:-8080}"
                            print_success "Hysteria2 已启动"
                        else
                            print_error "Hysteria2 启动失败"
                            journalctl -u "$CLIENT_SERVICE" --no-pager -n 5
                        fi
                    else
                        print_error "请先导入节点 (主菜单选 1)"
                    fi
                fi
                ;;
            2)
                if [[ "$hy_status" == "running" ]]; then
                    systemctl restart "$CLIENT_SERVICE" 2>/dev/null || true
                    sleep 1
                    print_success "Hysteria2 已重启"
                fi
                ;;
            3)
                if [[ "$xray_status" == "none" ]]; then
                    print_warning "Xray 未配置"
                elif [[ "$xray_status" == "running" ]]; then
                    systemctl stop xray-client 2>/dev/null || true
                    print_success "Xray 已停止"
                else
                    systemctl start xray-client 2>/dev/null || true
                    sleep 1
                    if systemctl is-active --quiet xray-client; then
                        print_success "Xray 已启动"
                    else
                        print_error "Xray 启动失败"
                    fi
                fi
                ;;
            5)
                echo ""
                echo -e "${YELLOW}选择日志:${NC}"
                echo "  1. Hysteria2"
                echo "  2. Xray"
                echo "  3. TUN (sing-box)"
                read -p "请选择 [1-3]: " log_choice
                case $log_choice in
                    1) journalctl -u "$CLIENT_SERVICE" --no-pager -n 30 ;;
                    2) journalctl -u xray-client --no-pager -n 30 ;;
                    3) journalctl -u bui-tun --no-pager -n 30 ;;
                    *) print_error "无效选项" ;;
                esac
                ;;
            0) return ;;
            *) print_error "无效选项" ;;
        esac
        echo ""
        read -p "按 Enter 继续..."
    done
}

#===============================================================================
# 统一更新（合并内核更新 + 客户端更新）
#===============================================================================

update_all() {
    echo ""
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}⬆ 组件版本检查${NC}"
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    echo ""
    
    local has_update=false
    
    echo -e "${CYAN}[客户端]${NC}"
    if [[ "$UPDATE_AVAILABLE" == "true" ]]; then
        echo -e "  本地: ${YELLOW}v${SCRIPT_VERSION}${NC}  最新: ${GREEN}v${REMOTE_VERSION}${NC}  ${RED}⬆ 可更新${NC}"
        has_update=true
    else
        print_info "检查更新中..."
        if check_client_update; then
            echo -e "  本地: ${YELLOW}v${SCRIPT_VERSION}${NC}  最新: ${GREEN}v${REMOTE_VERSION}${NC}  ${RED}⬆ 可更新${NC}"
            has_update=true
        else
            echo -e "  版本: ${GREEN}v${SCRIPT_VERSION}${NC}  ✓ 最新"
        fi
    fi
    
    load_server_address
    local server_versions=""
    if [[ -n "$SERVER_ADDRESS" ]]; then
        server_versions=$(curl -fsSL --max-time 5 -k "https://${SERVER_ADDRESS}/api/kernel-versions" 2>/dev/null || echo "")
    fi
    
    # 从 JSON 字符串提取字段值（兼容无 grep -P 的系统）
    _jval() { echo "$1" | sed -n "s/.*\"$2\"[[:space:]]*:[[:space:]]*\"\([^\"]*\)\".*/\1/p" | head -1; }
    # 版本号格式校验：只接受 X.Y.Z 格式（过滤 HTML/CSS 垃圾）
    _is_ver() { [[ "$1" =~ ^[0-9]+\.[0-9]+\.[0-9]+ ]]; }
    # 取两个版本号中较新的
    _newer() { printf '%s\n' "$1" "$2" | sort -V | tail -n1; }
    
    local sv_hy=$(_jval "$server_versions" "hysteria2")
    local sv_xray=$(_jval "$server_versions" "xray")
    local sv_sb=$(_jval "$server_versions" "singbox")
    
    if command -v hysteria &> /dev/null; then
        local local_hy=$(hysteria version 2>/dev/null | grep "^Version:" | awk '{print $2}' | sed 's/^v//' || echo "")
        local gh_hy=$(curl -fsSL --max-time 10 "https://api.github.com/repos/apernet/hysteria/releases/latest" 2>/dev/null \
            | grep '"tag_name"' | sed -E 's/.*"tag_name":\s*"app\/v?([^"]+)".*/\1/')
        [[ -z "$gh_hy" ]] && gh_hy=$(curl -fsSL --max-time 10 "https://api.github.com/repos/apernet/hysteria/releases/latest" 2>/dev/null \
            | grep '"tag_name"' | sed -E 's/.*"v?([0-9][^"]+)".*/\1/')
        _is_ver "$gh_hy" || gh_hy=""
        _is_ver "$sv_hy" || sv_hy=""
        local best_hy=$(_newer "${sv_hy:-0}" "${gh_hy:-0}")
        [[ "$best_hy" == "0" ]] && best_hy=""
        echo ""
        echo -e "${CYAN}[Hysteria2]${NC}"
        if [[ -n "$local_hy" && -n "$best_hy" ]]; then
            if [[ "$local_hy" == "$best_hy" ]]; then
                echo -e "  版本: ${GREEN}v${local_hy}${NC}  ✓ 最新"
            else
                echo -e "  本地: ${YELLOW}v${local_hy}${NC}  最新: ${GREEN}v${best_hy}${NC}  ${RED}⬆ 可更新${NC}"
                has_update=true
            fi
        fi
    fi
    
    if command -v xray &> /dev/null; then
        local local_xray=$(xray version 2>/dev/null | head -n1 | awk '{print $2}' | sed 's/^v//' || echo "")
        local gh_xray=$(curl -fsSL --max-time 10 "https://api.github.com/repos/XTLS/Xray-core/releases/latest" 2>/dev/null \
            | grep '"tag_name"' | sed -E 's/.*"v?([0-9][^"]+)".*/\1/')
        _is_ver "$gh_xray" || gh_xray=""
        _is_ver "$sv_xray" || sv_xray=""
        local best_xray=$(_newer "${sv_xray:-0}" "${gh_xray:-0}")
        [[ "$best_xray" == "0" ]] && best_xray=""
        echo ""
        echo -e "${CYAN}[Xray]${NC}"
        if [[ -n "$local_xray" && -n "$best_xray" ]]; then
            if [[ "$local_xray" == "$best_xray" ]]; then
                echo -e "  版本: ${GREEN}v${local_xray}${NC}  ✓ 最新"
            else
                echo -e "  本地: ${YELLOW}v${local_xray}${NC}  最新: ${GREEN}v${best_xray}${NC}  ${RED}⬆ 可更新${NC}"
                has_update=true
            fi
        fi
    fi
    
    if command -v sing-box &> /dev/null; then
        local local_sb=$(sing-box version 2>/dev/null | head -n1 | awk '{print $3}' | sed 's/^v//' || echo "")
        local gh_sb=$(curl -fsSL --max-time 10 "https://api.github.com/repos/SagerNet/sing-box/releases/latest" 2>/dev/null \
            | grep '"tag_name"' | sed -E 's/.*"v?([0-9][^"]+)".*/\1/')
        _is_ver "$gh_sb" || gh_sb=""
        _is_ver "$sv_sb" || sv_sb=""
        local best_sb=$(_newer "${sv_sb:-0}" "${gh_sb:-0}")
        [[ "$best_sb" == "0" ]] && best_sb=""
        echo ""
        echo -e "${CYAN}[sing-box]${NC}"
        if [[ -n "$local_sb" && -n "$best_sb" ]]; then
            if [[ "$local_sb" == "$best_sb" ]]; then
                echo -e "  版本: ${GREEN}v${local_sb}${NC}  ✓ 最新"
            else
                echo -e "  本地: ${YELLOW}v${local_sb}${NC}  最新: ${GREEN}v${best_sb}${NC}  ${RED}⬆ 可更新${NC}"
                has_update=true
            fi
        fi
    fi
    
    echo ""
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════${NC}"
    
    if [[ "$has_update" == "true" ]]; then
        read -p "发现可更新项，是否立即全部更新? (y/n): " confirm
        if [[ "$confirm" =~ ^[yY]$ ]]; then
            if [[ "$UPDATE_AVAILABLE" == "true" ]]; then
                print_info "更新客户端脚本..."
                do_client_update
            fi
            # 检测当前架构
            local ARCH=$(uname -m)
            local ARCH_SUFFIX="amd64"
            [[ "$ARCH" == "aarch64" ]] && ARCH_SUFFIX="arm64"
            
            if [[ -n "${best_hy:-}" && -n "${local_hy:-}" && "$local_hy" != "$best_hy" ]]; then
                print_info "更新 Hysteria2..."
                local hy_ok=false
                if [[ -n "$SERVER_ADDRESS" ]]; then
                    print_info "尝试从服务端下载..."
                    if curl -fsSL --max-time 60 -k "https://${SERVER_ADDRESS}/packages/hysteria-linux-${ARCH_SUFFIX}" -o /tmp/hysteria-new 2>/dev/null; then
                        chmod +x /tmp/hysteria-new
                        # 验证二进制可执行
                        if /tmp/hysteria-new version &>/dev/null; then
                            mv /tmp/hysteria-new /usr/local/bin/hysteria
                            print_success "从服务端更新成功"
                            hy_ok=true
                        else
                            rm -f /tmp/hysteria-new
                            print_warning "服务端文件校验失败，尝试官方源..."
                        fi
                    else
                        print_warning "服务端下载失败，尝试官方源..."
                    fi
                fi
                if [[ "$hy_ok" == "false" ]]; then
                    HYSTERIA_USER=root bash <(curl -fsSL https://get.hy2.sh/) 2>/dev/null || print_error "Hysteria2 更新失败"
                fi
            fi
            if [[ -n "${best_xray:-}" && -n "${local_xray:-}" && "$local_xray" != "$best_xray" ]]; then
                print_info "更新 Xray..."
                local xray_ok=false
                if [[ -n "$SERVER_ADDRESS" ]]; then
                    print_info "尝试从服务端下载..."
                    if curl -fsSL --max-time 60 -k "https://${SERVER_ADDRESS}/packages/xray-linux-${ARCH_SUFFIX}.zip" -o /tmp/xray-new.zip 2>/dev/null; then
                        rm -rf /tmp/xray_temp && mkdir -p /tmp/xray_temp
                        if unzip -o /tmp/xray-new.zip -d /tmp/xray_temp &>/dev/null && [[ -f /tmp/xray_temp/xray ]]; then
                            mv /tmp/xray_temp/xray /usr/local/bin/xray
                            chmod +x /usr/local/bin/xray
                            print_success "从服务端更新成功"
                            xray_ok=true
                        else
                            print_warning "服务端文件解压失败，尝试官方源..."
                        fi
                        rm -rf /tmp/xray-new.zip /tmp/xray_temp
                    else
                        print_warning "服务端下载失败，尝试官方源..."
                    fi
                fi
                if [[ "$xray_ok" == "false" ]]; then
                    bash -c "$(curl -L https://github.com/XTLS/Xray-install/raw/main/install-release.sh)" @ install 2>/dev/null || print_error "Xray 更新失败"
                fi
            fi
            if [[ -n "${best_sb:-}" && -n "${local_sb:-}" && "$local_sb" != "$best_sb" ]]; then
                print_info "更新 sing-box..."
                local sb_ok=false
                if [[ -n "$SERVER_ADDRESS" ]]; then
                    print_info "尝试从服务端下载..."
                    if curl -fsSL --max-time 60 -k "https://${SERVER_ADDRESS}/packages/sing-box-linux-${ARCH_SUFFIX}.tar.gz" -o /tmp/sing-box-new.tar.gz 2>/dev/null; then
                        rm -rf /tmp/sing-box_temp && mkdir -p /tmp/sing-box_temp
                        if tar -xzf /tmp/sing-box-new.tar.gz -C /tmp/sing-box_temp 2>/dev/null; then
                            local sb_bin=$(find /tmp/sing-box_temp -name "sing-box" -type f | head -1)
                            if [[ -n "$sb_bin" ]]; then
                                mv "$sb_bin" /usr/bin/sing-box
                                chmod +x /usr/bin/sing-box
                                print_success "从服务端更新成功"
                                sb_ok=true
                            fi
                        else
                            print_warning "服务端文件解压失败，尝试官方源..."
                        fi
                        rm -rf /tmp/sing-box-new.tar.gz /tmp/sing-box_temp
                    else
                        print_warning "服务端下载失败，尝试官方源..."
                    fi
                fi
                if [[ "$sb_ok" == "false" ]]; then
                    if [[ "${PKG_MANAGER:-}" == "apt" ]]; then
                        apt-get update -qq && apt-get install -y -qq sing-box 2>/dev/null || print_error "sing-box 更新失败"
                    else
                        bash <(curl -fsSL https://sing-box.app/install.sh) 2>/dev/null || print_error "sing-box 更新失败"
                    fi
                fi
            fi
            echo ""
            print_success "更新完成！"
        fi
    else
        print_success "所有组件均为最新版本"
    fi
}

#===============================================================================
# 子菜单：高级设置
#===============================================================================

advanced_settings_menu() {
    while true; do
        echo ""
        echo -e "${CYAN}╔══════════════════════════════════════════════════════════════╗${NC}"
        echo -e "${CYAN}║${NC}              ${GREEN}⚙ 高级设置${NC}                                     ${CYAN}║${NC}"
        echo -e "${CYAN}╠══════════════════════════════════════════════════════════════╣${NC}"
        echo -e "${CYAN}║${NC}  ${YELLOW}1.${NC} 开机自启动管理                                        ${CYAN}║${NC}"
        echo -e "${CYAN}║${NC}  ${YELLOW}2.${NC} 编辑路由规则                                          ${CYAN}║${NC}"
        echo -e "${CYAN}║${NC}  ${YELLOW}0.${NC} 返回主菜单                                            ${CYAN}║${NC}"
        echo -e "${CYAN}╚══════════════════════════════════════════════════════════════╝${NC}"
        read -p "请选择 [0-2]: " sub
        case $sub in
            1)
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
                        [[ -f /etc/systemd/system/$CLIENT_SERVICE ]] && systemctl enable "$CLIENT_SERVICE" 2>/dev/null
                        [[ -f /etc/systemd/system/xray-client.service ]] && systemctl enable xray-client 2>/dev/null
                        [[ -f /etc/systemd/system/bui-tun.service ]] && systemctl enable bui-tun 2>/dev/null
                        print_success "已开启开机自启动"
                    fi
                fi
                ;;
            2) edit_rules ;;
            0) return ;;
            *) print_error "无效选项" ;;
        esac
        echo ""
        read -p "按 Enter 继续..."
    done
}

#===============================================================================
# 主菜单（精简版 — 8 项）
#===============================================================================

show_menu() {
    echo ""
    echo -e "${CYAN}┌──────────────────────────────────────────┐${NC}"
    echo -e "${CYAN}│${NC}       ${GREEN}B-UI 客户端  操作菜单${NC}             ${CYAN}│${NC}"
    echo -e "${CYAN}├──────────────────────────────────────────┘${NC}"
    echo -e "${CYAN}│${NC}"
    echo -e "${CYAN}│${NC}  ${YELLOW}1.${NC} 📥  导入节点  ${DIM}(粘贴链接自动识别)${NC}"
    echo -e "${CYAN}│${NC}  ${YELLOW}2.${NC} 📋  节点管理  ${DIM}(列表/切换/删除)${NC}"
    echo -e "${CYAN}│${NC}  ${YELLOW}3.${NC} ▶   服务控制  ${DIM}(启动/停止/重启/日志)${NC}"
    echo -e "${CYAN}│${NC}  ${YELLOW}4.${NC} 🌐  TUN 代理  ${DIM}(全局模式 开/关)${NC}"
    echo -e "${CYAN}│${NC}  ${YELLOW}5.${NC} 🔍  连接测试"
    if [[ "$UPDATE_AVAILABLE" == "true" ]]; then
        echo -e "${CYAN}│${NC}  ${YELLOW}6.${NC} ⬆   一键更新  ${RED}★ 有新版本${NC}"
    else
        echo -e "${CYAN}│${NC}  ${YELLOW}6.${NC} ⬆   一键更新  ${DIM}(内核+客户端)${NC}"
    fi
    echo -e "${CYAN}│${NC}  ${YELLOW}7.${NC} ⚙   高级设置  ${DIM}(自启动/路由规则)${NC}"
    echo -e "${CYAN}│${NC}"
    echo -e "${CYAN}├──────────────────────────────────────────${NC}"
    echo -e "${CYAN}│${NC}  ${YELLOW}8.${NC} ${RED}🗑  卸载${NC}"
    echo -e "${CYAN}│${NC}  ${YELLOW}0.${NC} 退出"
    echo -e "${CYAN}└──────────────────────────────────────────${NC}"
}

dispatch_subcommand() {
    local cmd="$1"; shift
    case "$cmd" in
        switch)  cmd_switch "$@" ;;
        tun)     cmd_tun "$@" ;;
        import)  cmd_import "$@" ;;
        start)   cmd_start "$@" ;;
        stop)    cmd_stop "$@" ;;
        restart) cmd_restart "$@" ;;
        status)  cmd_status "$@"; exit 0 ;;
        list)    cmd_list "$@"; exit 0 ;;
        -h|--help|help)
            cat <<'HELP'
用法: bui-c [subcommand] [options]

  无参数          进入 TUI 交互菜单

子命令:
  switch <名称>        切换到指定节点
  tun on|off|status   TUN 模式控制
  import <uri>        导入节点（加 --activate 立即激活）
  start               启动当前节点服务
  stop                停止所有客户端服务
  restart             重启当前节点（重新生成配置）
  status              查看当前状态（--json 输出机器可读格式）
  list                列出所有节点（--json 输出机器可读格式）

通用 flags:
  -y, --yes           跳过所有确认提示
  --json              输出 JSON 格式
  -q, --quiet         只输出结果，不输出过程信息
HELP
            exit 0
            ;;
        *)
            tui_error "未知命令: $cmd。运行 'bui-c --help' 查看帮助。"
            exit 2
            ;;
    esac
}

main() {
    check_root
    check_os

    # 提取全局 flags（-y, --json, --quiet）
    parse_global_flags "$@"
    # 过滤掉 flags，只保留非 flag 参数
    local args=()
    for arg in "$@"; do
        case "$arg" in
            -h|--help) dispatch_subcommand --help; exit 0 ;;
            -*) : ;;
            *) args+=("$arg") ;;
        esac
    done

    # 有子命令 → 非交互路径
    if [[ ${#args[@]} -gt 0 ]]; then
        dispatch_subcommand "${args[@]}"
        exit $?
    fi

    # 无参数 → TUI 交互模式
    ensure_tui_tools
    tui_main_loop
}

tui_main_loop() {
    check_client_update &>/dev/null &

    while true; do
        clear
        print_banner

        show_status_bar

        # 动态 TUN 标签
        local tun_opt="开启 TUN"
        systemctl is-active --quiet bui-tun 2>/dev/null && tun_opt="停止 TUN"

        local choice
        if [[ "$TUI_AVAILABLE" == "true" ]]; then
            choice=$(gum choose \
                "切换节点" \
                "$tun_opt" \
                "──────────" \
                "导入节点" \
                "服务控制" \
                "高级设置" \
                "──────────" \
                "一键更新" \
                "卸载" \
                "退出" \
                ) || choice="退出"
        else
            show_menu
            read -p "请选择 [0-8]: " choice
            case $choice in
                1) choice="切换节点" ;;
                2) choice="$tun_opt" ;;
                3) choice="导入节点" ;;
                4) choice="服务控制" ;;
                5) choice="高级设置" ;;
                6) choice="一键更新" ;;
                7) choice="卸载" ;;
                0) choice="退出" ;;
                *) choice="__invalid__" ;;
            esac
        fi

        case "$choice" in
            "切换节点")                  tui_switch_node ;;
            "开启 TUN"|"停止 TUN")       tui_toggle_tun ;;
            "导入节点")                  tui_import_node ;;
            "服务控制")                  tui_service_control ;;
            "高级设置")                  advanced_settings_menu ;;
            "一键更新")                  update_all ;;
            "卸载")                      uninstall ;;
            "退出")                      echo ""; tui_info "再见！"; exit 0 ;;
            "──────────")               continue ;;
            "__invalid__")              print_error "无效选项" ;;
        esac

        if [[ "$TUI_AVAILABLE" != "true" ]]; then
            echo ""
            read -p "按 Enter 继续..."
        fi
    done
}

tui_switch_node() {
    if [[ ! -d "$CONFIGS_DIR" ]] || [[ -z "$(ls -A "$CONFIGS_DIR" 2>/dev/null)" ]]; then
        tui_error "没有已保存的节点，请先导入"
        sleep 1
        return 0
    fi

    local active; active=$(get_active_config)

    # 构建显示列表
    local lines=()
    for config_dir in "$CONFIGS_DIR"/*/; do
        [[ ! -d "$config_dir" ]] && continue
        local name; name=$(basename "$config_dir")
        local meta="${config_dir}meta.json"
        local protocol; protocol=$(grep '"protocol"' "$meta" 2>/dev/null | cut -d'"' -f4)
        local server; server=$(grep '"server"' "$meta" 2>/dev/null | cut -d'"' -f4)
        local marker=""
        [[ "$name" == "$active" ]] && marker="  ★ 当前"
        lines+=("$(printf '%-28s  %-16s  %s%s' "$name" "$protocol" "$server" "$marker")")
    done

    local selected
    if [[ "$TUI_AVAILABLE" == "true" ]]; then
        selected=$(printf '%s\n' "${lines[@]}" \
            | fzf --prompt "切换节点 > " \
                  --mouse \
                  --height 50% \
                  --border=rounded \
                  --layout reverse \
                  --info inline \
                  --header "↑↓ 选择   Enter 确认   Esc 取消" \
                  2>/dev/null) || return 0
        # 提取节点名（第一列）
        selected=$(echo "$selected" | awk '{print $1}')
    else
        local configs=()
        for config_dir in "$CONFIGS_DIR"/*/; do
            [[ -d "$config_dir" ]] && configs+=("$(basename "$config_dir")")
        done
        local i=1
        for c in "${configs[@]}"; do
            local marker=""
            [[ "$c" == "$active" ]] && marker=" ★"
            echo "  $i. ${c}${marker}"
            ((i++))
        done
        echo ""
        read -p "选择配置编号 (0 返回): " choice
        [[ "$choice" == "0" ]] || [[ -z "$choice" ]] && return 0
        if ! [[ "$choice" =~ ^[0-9]+$ ]] || (( choice < 1 || choice > ${#configs[@]} )); then
            print_error "无效选择"; return 1
        fi
        selected="${configs[$((choice-1))]}"
    fi

    [[ -z "$selected" ]] && return 0
    [[ "$selected" == "$active" ]] && { tui_info "已是当前节点"; sleep 1; return 0; }

    _switch_to_profile "$selected"
}
tui_toggle_tun() {
    if systemctl is-active --quiet bui-tun 2>/dev/null; then
        if tui_confirm "停止 TUN 模式？"; then
            tui_info "停止 TUN..."; stop_tun_mode
            tui_success "TUN 已停止"
        fi
    else
        local active; active=$(get_active_config)
        if [[ -z "$active" ]]; then
            tui_error "没有激活的节点，无法启动 TUN"
            sleep 1
            return 0
        fi
        if tui_confirm "启动 TUN 全局代理模式？"; then
            local protocol
            protocol=$(grep '"protocol"' "${CONFIGS_DIR}/${active}/meta.json" 2>/dev/null | cut -d'"' -f4)
            tui_info "生成 TUN 配置..."; generate_singbox_tun_config "${protocol:-hysteria2}"
            tui_info "启动 TUN..."; start_tun_mode
            tui_success "TUN 已启动"
        fi
    fi
    sleep 1
}

tui_import_node() {
    echo ""
    local raw_input
    if [[ "$TUI_AVAILABLE" == "true" ]]; then
        echo "支持: hysteria2://  vless://  https://订阅地址"
        echo ""
        raw_input=$(gum write \
            --placeholder "粘贴链接，每行一个（Ctrl+D 或 Esc 完成）..." \
            --char-limit 0 --width 70 --height 8) || return 0
    else
        echo "请粘贴链接（每行一个，空行结束）："
        local lines_input=()
        while IFS= read -r line; do
            [[ -z "$line" ]] && break
            lines_input+=("$line")
        done
        raw_input=$(printf '%s\n' "${lines_input[@]}")
    fi

    [[ -z "$raw_input" ]] && return 0

    local uris=()
    while IFS= read -r line; do
        line=$(echo "$line" | xargs 2>/dev/null || echo "$line")
        [[ -z "$line" || "$line" =~ ^# ]] && continue
        uris+=("$line")
    done <<< "$raw_input"

    [[ ${#uris[@]} -eq 0 ]] && { tui_error "未识别到有效链接"; sleep 1; return 0; }

    local success=0 fail=0
    local imported_names=()

    for uri in "${uris[@]}"; do
        local parsed="" protocol=""
        if [[ "$uri" =~ ^(hysteria2|hy2):// ]]; then
            parsed=$(parse_hysteria_uri "$uri"); protocol="hysteria2"
        elif [[ "$uri" =~ ^vless:// ]]; then
            parsed=$(parse_vless_uri "$uri"); protocol="vless-reality"
        elif [[ "$uri" =~ ^https?:// ]]; then
            tui_info "订阅地址请通过导入节点菜单的订阅功能导入"
            ((fail++)); continue
        else
            tui_error "不支持: ${uri:0:50}"; ((fail++)); continue
        fi

        [[ -z "$parsed" ]] && { tui_error "解析失败: ${uri:0:50}"; ((fail++)); continue; }

        local remark server_addr
        remark=$(echo "$parsed" | grep '^REMARK=' | cut -d= -f2-)
        server_addr=$(echo "$parsed" | grep '^SERVER_ADDR=' | cut -d= -f2-)

        local config_name="${remark:-${protocol}-$(date +%s)}"
        config_name=$(echo "$config_name" | tr -s ' ' '-' | sed 's/[^a-zA-Z0-9._-]/-/g' | sed 's/--*/-/g' | sed 's/^-//;s/-$//')
        [[ -z "$config_name" ]] && config_name="${protocol}-$(date +%s)"

        if [[ -d "${CONFIGS_DIR}/${config_name}" ]]; then
            tui_info "已存在: $config_name（跳过）"; continue
        fi

        safe_import_parsed "$parsed"

        if ! save_config_meta "$config_name" "$protocol" "${server_addr:-$SERVER_ADDR}" "$uri" 1080 8080; then
            tui_error "保存失败: $config_name"; ((fail++)); continue
        fi
        tui_success "已导入: $config_name ($protocol)"
        imported_names+=("$config_name")
        ((success++))
    done

    echo ""
    tui_info "导入完成: 成功 ${success}  失败 ${fail}"

    if [[ ${#imported_names[@]} -gt 0 ]]; then
        echo ""
        if tui_confirm "立即激活其中一个节点？"; then
            tui_switch_node
        fi
    fi
}

tui_service_control() {
    while true; do
        clear
        echo ""
        echo -e "${CYAN}═══ 服务控制 ═══════════════════════════════${NC}"
        echo ""

        local hy2_status="🔴 停止" xray_status="🔴 停止" tun_status="🔴 停止"
        systemctl is-active --quiet "$CLIENT_SERVICE" 2>/dev/null && hy2_status="🟢 运行中"
        systemctl is-active --quiet xray-client 2>/dev/null       && xray_status="🟢 运行中"
        systemctl is-active --quiet bui-tun 2>/dev/null            && tun_status="🟢 运行中"

        echo -e "  Hysteria2  $hy2_status"
        echo -e "  Xray       $xray_status"
        echo -e "  TUN        $tun_status"
        echo ""

        local opts=()
        if systemctl is-active --quiet "$CLIENT_SERVICE" 2>/dev/null; then
            opts+=("停止 Hysteria2" "重启 Hysteria2")
        else
            opts+=("启动 Hysteria2")
        fi
        if systemctl is-active --quiet xray-client 2>/dev/null; then
            opts+=("停止 Xray")
        else
            opts+=("启动 Xray")
        fi
        opts+=("查看日志" "返回")

        local choice
        choice=$(tui_menu "操作" "${opts[@]}") || choice="返回"

        case "$choice" in
            "启动 Hysteria2")
                tui_info "启动..."; systemctl start "$CLIENT_SERVICE"
                tui_success "Hysteria2 已启动"
                ;;
            "停止 Hysteria2")
                tui_info "停止..."; systemctl stop "$CLIENT_SERVICE"
                tui_success "Hysteria2 已停止"
                ;;
            "重启 Hysteria2")
                tui_info "重启..."; systemctl restart "$CLIENT_SERVICE"
                tui_success "Hysteria2 已重启"
                ;;
            "启动 Xray")
                tui_info "启动..."; systemctl start xray-client
                tui_success "Xray 已启动"
                ;;
            "停止 Xray")
                tui_info "停止..."; systemctl stop xray-client
                tui_success "Xray 已停止"
                ;;
            "查看日志")
                local svc_choice
                svc_choice=$(tui_menu "查看哪个日志？" "Hysteria2" "Xray" "TUN" "返回")
                case "$svc_choice" in
                    "Hysteria2") journalctl -u "$CLIENT_SERVICE" --no-pager -n 50 | less ;;
                    "Xray")      journalctl -u xray-client --no-pager -n 50 | less ;;
                    "TUN")       journalctl -u bui-tun --no-pager -n 50 | less ;;
                esac
                ;;
            "返回"|"") return 0 ;;
        esac
        sleep 1
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


#===============================================================================
# 客户端静默自动更新 (支持 `bui-c auto` 调用)
# 包含：客户端脚本更新 + 内核更新
#===============================================================================

auto_update_all() {
    local LOG_FILE="/var/log/bui-c-auto-update.log"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] 开始自动检查更新..." >> "$LOG_FILE"
    
    load_server_address
    
    # 1. 检查客户端脚本更新
    check_client_update > /dev/null 2>&1
    if [[ "$UPDATE_AVAILABLE" == "true" ]]; then
        echo "[$(date '+%Y-%m-%d %H:%M:%S')] 客户端: v${SCRIPT_VERSION} -> v${REMOTE_VERSION}, 更新中..." >> "$LOG_FILE"
        do_client_update >> "$LOG_FILE" 2>&1 || true
    else
        echo "[$(date '+%Y-%m-%d %H:%M:%S')] 客户端已是最新: v${SCRIPT_VERSION}" >> "$LOG_FILE"
    fi
    
    # 2. 检查内核更新 (服务端优先)
    local server_versions=""
    if [[ -n "$SERVER_ADDRESS" ]]; then
        server_versions=$(curl -fsSL --max-time 5 -k "https://${SERVER_ADDRESS}/api/kernel-versions" 2>/dev/null || echo "")
    fi
    
    # 辅助函数
    _jval() { echo "$1" | grep -oP "\"$2\"\\s*:\\s*\"\\K[^\"]+" 2>/dev/null | head -1; }
    _newer() { printf '%s\n' "$1" "$2" | sort -V | tail -n1; }
    _is_newer() {
        local lv="$1" rv="$2"
        [[ -z "$rv" || "$lv" == "$rv" ]] && return 1
        [[ "$(_newer "$lv" "$rv")" == "$rv" ]] && return 0
        return 1
    }
    
    local sv_hy=$(_jval "$server_versions" "hysteria2")
    local sv_xray=$(_jval "$server_versions" "xray")
    local sv_sb=$(_jval "$server_versions" "singbox")
    
    # Hysteria2
    local local_hy=$(hysteria version 2>/dev/null | grep "^Version:" | awk '{print $2}' | sed 's/^v//' || echo "")
    local gh_hy=$(curl -fsSL --max-time 10 "https://api.github.com/repos/apernet/hysteria/releases/latest" 2>/dev/null \
        | grep '"tag_name"' | sed -E 's/.*"tag_name":\s*"app\/v?([^"]+)".*/\1/')
    [[ -z "$gh_hy" ]] && gh_hy=$(curl -fsSL --max-time 10 "https://api.github.com/repos/apernet/hysteria/releases/latest" 2>/dev/null \
        | grep '"tag_name"' | sed -E 's/.*"v?([0-9][^"]+)".*/\1/')
    local best_hy=$(_newer "${sv_hy:-0}" "${gh_hy:-0}")
    [[ "$best_hy" == "0" ]] && best_hy=""
    if [[ -n "$local_hy" && -n "$best_hy" ]] && _is_newer "$local_hy" "$best_hy"; then
        echo "[$(date '+%Y-%m-%d %H:%M:%S')] Hysteria2: v${local_hy} -> v${best_hy}, 更新中..." >> "$LOG_FILE"
        HYSTERIA_USER=root bash <(curl -fsSL https://get.hy2.sh/) >> "$LOG_FILE" 2>&1 || true
        systemctl restart "$CLIENT_SERVICE" 2>/dev/null || true
    fi
    
    # Xray
    if command -v xray &> /dev/null; then
        local local_xray=$(xray version 2>/dev/null | head -n1 | awk '{print $2}' | sed 's/^v//' || echo "")
        local gh_xray=$(curl -fsSL --max-time 10 "https://api.github.com/repos/XTLS/Xray-core/releases/latest" 2>/dev/null \
            | grep '"tag_name"' | sed -E 's/.*"v?([0-9][^"]+)".*/\1/')
        local best_xray=$(_newer "${sv_xray:-0}" "${gh_xray:-0}")
        [[ "$best_xray" == "0" ]] && best_xray=""
        if [[ -n "$local_xray" && -n "$best_xray" ]] && _is_newer "$local_xray" "$best_xray"; then
            echo "[$(date '+%Y-%m-%d %H:%M:%S')] Xray: v${local_xray} -> v${best_xray}, 更新中..." >> "$LOG_FILE"
            bash -c "$(curl -L https://github.com/XTLS/Xray-install/raw/main/install-release.sh)" @ install >> "$LOG_FILE" 2>&1 || true
            systemctl restart xray-client 2>/dev/null || true
        fi
    fi
    
    # sing-box
    if command -v sing-box &> /dev/null; then
        local local_sb=$(sing-box version 2>/dev/null | head -n1 | awk '{print $3}' | sed 's/^v//' || echo "")
        local gh_sb=$(curl -fsSL --max-time 10 "https://api.github.com/repos/SagerNet/sing-box/releases/latest" 2>/dev/null \
            | grep '"tag_name"' | sed -E 's/.*"v?([0-9][^"]+)".*/\1/')
        local best_sb=$(_newer "${sv_sb:-0}" "${gh_sb:-0}")
        [[ "$best_sb" == "0" ]] && best_sb=""
        if [[ -n "$local_sb" && -n "$best_sb" ]] && _is_newer "$local_sb" "$best_sb"; then
            echo "[$(date '+%Y-%m-%d %H:%M:%S')] sing-box: v${local_sb} -> v${best_sb}, 更新中..." >> "$LOG_FILE"
            if command -v apt-get &> /dev/null; then
                apt-get update -qq && apt-get install -y -qq sing-box >> "$LOG_FILE" 2>&1 || true
            else
                bash <(curl -fsSL https://sing-box.app/install.sh) >> "$LOG_FILE" 2>&1 || true
            fi
        fi
    fi
    
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] 自动检查完成" >> "$LOG_FILE"
}

#===============================================================================
# 确保客户端定时任务
#===============================================================================

ensure_client_cron() {
    if ! command -v crontab &> /dev/null; then
        return
    fi
    
    local current_cron
    current_cron=$(crontab -l 2>/dev/null || echo "")
    
    if ! echo "$current_cron" | grep -q "bui-c auto"; then
        # 移除旧的 bui-c 相关条目
        local new_cron
        new_cron=$(echo "$current_cron" | grep -v "bui-c" | grep -v "BUI-C 定时" || echo "")
        
        new_cron="${new_cron}
# === BUI-C 定时任务 ===
0 */6 * * * /usr/local/bin/bui-c auto >> /var/log/bui-c-auto-update.log 2>&1"
        
        echo "$new_cron" | sed '/^$/d' | crontab -
    fi
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
    
    # 确保定时任务存在
    ensure_client_cron
}

# 入口
case "${1:-}" in
    auto)
        # 静默自动更新模式 (用于 cron)
        auto_update_all
        ;;
    *)
        # 交互模式
        first_run_setup
        main "$@"
        ;;
esac
