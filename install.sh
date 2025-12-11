#!/bin/bash

#===============================================================================
# B-UI 安装脚本 (模块化版本)
# 功能：下载并安装 B-UI 所有组件
# 版本: 2.4.0
#===============================================================================

set -e

# 颜色定义
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m'

# 配置
SCRIPT_VERSION="2.4.0"
GITHUB_RAW="https://raw.githubusercontent.com/Buxiulei/b-ui/main"
GITHUB_CDN="https://cdn.jsdelivr.net/gh/Buxiulei/b-ui@main"
BASE_DIR="/opt/hysteria"
ADMIN_DIR="${BASE_DIR}/admin"

# 打印函数
print_info() { echo -e "${BLUE}[INFO]${NC} $1"; }
print_success() { echo -e "${GREEN}[SUCCESS]${NC} $1"; }
print_warning() { echo -e "${YELLOW}[WARNING]${NC} $1"; }
print_error() { echo -e "${RED}[ERROR]${NC} $1"; }

print_banner() {
    clear
    echo -e "${CYAN}"
    echo "╔══════════════════════════════════════════════════════════════╗"
    echo "║                                                              ║"
    echo "║     B-UI 一键安装脚本 (模块化版本)                          ║"
    echo "║                                                              ║"
    echo "║     Hysteria2 + VLESS-Reality + Web 管理面板                ║"
    echo "║                                                              ║"
    echo -e "║     版本: ${YELLOW}${SCRIPT_VERSION}${CYAN}                                           ║"
    echo "╚══════════════════════════════════════════════════════════════╝"
    echo -e "${NC}"
}

#===============================================================================
# 环境检查
#===============================================================================

check_root() {
    if [[ $EUID -ne 0 ]]; then
        print_error "此脚本需要 root 权限运行"
        print_info "请使用: sudo -i 切换到 root 用户后再运行"
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
    print_info "检查并安装必要的依赖..."
    
    local deps_map=(
        "curl:curl:curl"
        "jq:jq:jq"
        "dig:dnsutils:bind-utils"
        "openssl:openssl:openssl"
    )
    
    local apt_pkgs=()
    local yum_pkgs=()
    local missing_cmds=()
    
    for item in "${deps_map[@]}"; do
        IFS=':' read -r cmd apt_pkg yum_pkg <<< "$item"
        if ! command -v "$cmd" &> /dev/null; then
            missing_cmds+=("$cmd")
            apt_pkgs+=("$apt_pkg")
            yum_pkgs+=("$yum_pkg")
        fi
    done
    
    if [[ ${#missing_cmds[@]} -gt 0 ]]; then
        print_warning "缺少以下工具: ${missing_cmds[*]}"
        print_info "正在安装依赖包..."
        
        if command -v apt-get &> /dev/null; then
            apt-get update -qq
            apt-get install -y "${apt_pkgs[@]}"
        elif command -v yum &> /dev/null; then
            yum install -y "${yum_pkgs[@]}"
        elif command -v dnf &> /dev/null; then
            dnf install -y "${yum_pkgs[@]}"
        fi
        
        print_success "依赖安装完成"
    fi
}

#===============================================================================
# 网络检测与下载
#===============================================================================

# 自动选择最快的下载源
select_download_source() {
    print_info "检测最佳下载源..."
    
    # 测试 GitHub 直连
    local github_time=$(curl -o /dev/null -s -w '%{time_total}' --max-time 5 "${GITHUB_RAW}/version.json" 2>/dev/null || echo "999")
    
    # 测试 CDN
    local cdn_time=$(curl -o /dev/null -s -w '%{time_total}' --max-time 5 "${GITHUB_CDN}/version.json" 2>/dev/null || echo "999")
    
    if (( $(echo "$cdn_time < $github_time" | bc -l 2>/dev/null || echo "0") )); then
        DOWNLOAD_URL="$GITHUB_CDN"
        print_info "使用 CDN 镜像 (响应时间: ${cdn_time}s)"
    else
        DOWNLOAD_URL="$GITHUB_RAW"
        print_info "使用 GitHub 直连 (响应时间: ${github_time}s)"
    fi
}

# 下载文件
download_file() {
    local remote_path="$1"
    local local_path="$2"
    local url="${DOWNLOAD_URL}/${remote_path}"
    
    # 确保目录存在
    mkdir -p "$(dirname "$local_path")"
    
    if curl -fsSL "$url" -o "$local_path" 2>/dev/null; then
        return 0
    else
        # 尝试备用源
        local backup_url
        if [[ "$DOWNLOAD_URL" == "$GITHUB_RAW" ]]; then
            backup_url="${GITHUB_CDN}/${remote_path}"
        else
            backup_url="${GITHUB_RAW}/${remote_path}"
        fi
        
        if curl -fsSL "$backup_url" -o "$local_path" 2>/dev/null; then
            return 0
        fi
        return 1
    fi
}

#===============================================================================
# 下载所有模块文件
#===============================================================================

download_all_files() {
    print_info "下载 B-UI 模块文件..."
    
    # 创建目录
    mkdir -p "${BASE_DIR}"
    mkdir -p "${ADMIN_DIR}"
    
    # 文件列表: 远程路径 -> 本地路径
    local files=(
        "version.json:${BASE_DIR}/version.json"
        "server/core.sh:${BASE_DIR}/core.sh"
        "server/b-ui-cli.sh:${BASE_DIR}/b-ui-cli.sh"
        "server/update.sh:${BASE_DIR}/update.sh"
        "web/server.js:${ADMIN_DIR}/server.js"
        "web/index.html:${ADMIN_DIR}/index.html"
        "web/style.css:${ADMIN_DIR}/style.css"
        "web/app.js:${ADMIN_DIR}/app.js"
    )
    
    local failed=0
    for item in "${files[@]}"; do
        IFS=':' read -r remote local <<< "$item"
        echo -n "  下载 ${remote}... "
        if download_file "$remote" "$local"; then
            echo -e "${GREEN}✓${NC}"
        else
            echo -e "${RED}✗${NC}"
            ((failed++))
        fi
    done
    
    if [[ $failed -gt 0 ]]; then
        print_error "有 ${failed} 个文件下载失败"
        return 1
    fi
    
    # 设置执行权限
    chmod +x "${BASE_DIR}/core.sh"
    chmod +x "${BASE_DIR}/b-ui-cli.sh"
    chmod +x "${BASE_DIR}/update.sh"
    
    print_success "所有文件下载完成"
}

#===============================================================================
# 检测旧版本
#===============================================================================

check_old_version() {
    # 检测旧版单文件安装
    if [[ -f "/usr/local/bin/b-ui" ]]; then
        local old_version=$(grep -oP 'SCRIPT_VERSION="\K[^"]+' /usr/local/bin/b-ui 2>/dev/null || echo "未知")
        print_warning "检测到旧版本 B-UI (v${old_version})"
        echo ""
        echo -e "${YELLOW}旧版本使用单文件架构，新版本使用模块化架构${NC}"
        echo -e "升级后将保留您的用户数据和配置"
        echo ""
        read -p "是否继续升级? (y/n): " confirm
        if [[ "$confirm" != "y" && "$confirm" != "Y" ]]; then
            print_info "已取消"
            exit 0
        fi
        
        # 保留用户数据
        if [[ -f "${BASE_DIR}/users.json" ]]; then
            cp "${BASE_DIR}/users.json" "/tmp/b-ui-users-backup.json"
            print_info "已备份用户数据"
        fi
        if [[ -f "${BASE_DIR}/config.yaml" ]]; then
            cp "${BASE_DIR}/config.yaml" "/tmp/b-ui-config-backup.yaml"
            print_info "已备份 Hysteria 配置"
        fi
        
        return 0
    fi
    
    # 检测模块化版本
    if [[ -f "${BASE_DIR}/version.json" ]]; then
        local installed_version=$(jq -r '.version' "${BASE_DIR}/version.json" 2>/dev/null || echo "0")
        print_info "检测到已安装版本: v${installed_version}"
        return 1  # 已是模块化版本，走更新流程
    fi
    
    return 2  # 全新安装
}

#===============================================================================
# 安装核心组件
#===============================================================================

run_core_install() {
    print_info "运行核心安装流程..."
    
    # 加载核心模块
    source "${BASE_DIR}/core.sh"
    
    # 执行安装
    install_hysteria
    install_nodejs
    install_nginx
    install_xray
    
    # 收集用户配置
    collect_user_input
    
    # 配置服务
    configure_hysteria
    configure_xray
    configure_nginx_proxy
    
    # 部署 Web 面板
    deploy_admin_panel
    
    # 创建服务
    create_services
    
    # 启动服务
    start_all_services
    
    # 创建全局命令
    create_global_command
}

#===============================================================================
# 创建全局命令
#===============================================================================

create_global_command() {
    print_info "创建全局命令..."
    
    cat > /usr/local/bin/b-ui << 'CLIEOF'
#!/bin/bash
# B-UI 全局入口
exec /opt/hysteria/b-ui-cli.sh "$@"
CLIEOF
    
    chmod +x /usr/local/bin/b-ui
    print_success "已创建 b-ui 命令，可使用 'sudo b-ui' 管理"
}

#===============================================================================
# 恢复备份数据
#===============================================================================

restore_backup() {
    if [[ -f "/tmp/b-ui-users-backup.json" ]]; then
        cp "/tmp/b-ui-users-backup.json" "${BASE_DIR}/users.json"
        rm -f "/tmp/b-ui-users-backup.json"
        print_success "已恢复用户数据"
    fi
    if [[ -f "/tmp/b-ui-config-backup.yaml" ]]; then
        cp "/tmp/b-ui-config-backup.yaml" "${BASE_DIR}/config.yaml"
        rm -f "/tmp/b-ui-config-backup.yaml"
        print_success "已恢复 Hysteria 配置"
    fi
}

#===============================================================================
# 主函数
#===============================================================================

main() {
    print_banner
    
    check_root
    check_os
    check_dependencies
    
    echo ""
    
    # 选择下载源
    select_download_source
    
    # 检测安装状态
    local install_type
    check_old_version
    install_type=$?
    
    case $install_type in
        0)  # 从旧版本升级
            print_info "开始升级..."
            download_all_files
            restore_backup
            run_core_install
            ;;
        1)  # 模块化版本更新
            print_info "检查更新..."
            source "${BASE_DIR}/update.sh"
            check_and_update
            ;;
        2)  # 全新安装
            print_info "开始全新安装..."
            download_all_files
            run_core_install
            ;;
    esac
    
    echo ""
    echo -e "${GREEN}════════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}  安装完成！${NC}"
    echo -e "${GREEN}════════════════════════════════════════════════════════════════${NC}"
    echo ""
    echo -e "  终端管理: ${YELLOW}sudo b-ui${NC}"
    echo ""
}

main "$@"
