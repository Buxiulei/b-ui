#!/bin/bash

#===============================================================================
# B-UI 安装脚本 (模块化版本)
# 功能：下载并安装 B-UI 所有组件
# 版本: 动态读取自 version.json
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
GITHUB_RAW="https://raw.githubusercontent.com/Buxiulei/b-ui/main"
GITHUB_CDN="https://raw.githack.com/Buxiulei/b-ui/main"
BASE_DIR="/opt/b-ui"
ADMIN_DIR="${BASE_DIR}/admin"

# 动态获取版本号 (不依赖 jq，在依赖安装前也能工作)
get_version() {
    local ver=""
    # 方法1: 使用 jq (如果可用)
    if command -v jq &> /dev/null; then
        if [[ -f "${BASE_DIR}/version.json" ]]; then
            ver=$(jq -r '.version' "${BASE_DIR}/version.json" 2>/dev/null)
        fi
        if [[ -z "$ver" || "$ver" == "null" ]]; then
            ver=$(curl -fsSL "${GITHUB_RAW}/version.json" 2>/dev/null | jq -r '.version' 2>/dev/null)
        fi
    fi
    # 方法2: 使用 sed (fallback，不依赖 jq，兼容 macOS 和 Linux)
    if [[ -z "$ver" || "$ver" == "null" ]]; then
        ver=$(curl -fsSL "${GITHUB_RAW}/version.json" 2>/dev/null | sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)
    fi
    echo "${ver:-2.15.0}"
}
SCRIPT_VERSION=$(get_version)

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
    
    # 安装中文语言包并配置（消除 setlocale 警告）
    print_info "配置系统语言环境..."
    if command -v apt-get &> /dev/null; then
        # Debian/Ubuntu
        apt-get install -y -qq locales > /dev/null 2>&1 || true
        # 取消注释 zh_CN.UTF-8 和 en_US.UTF-8
        sed -i '/^#.*zh_CN.UTF-8/s/^#//' /etc/locale.gen 2>/dev/null || true
        sed -i '/^#.*en_US.UTF-8/s/^#//' /etc/locale.gen 2>/dev/null || true
        # 生成 locale
        locale-gen > /dev/null 2>&1 || true
        # 设置默认 locale
        update-locale LANG=en_US.UTF-8 LC_ALL=en_US.UTF-8 2>/dev/null || true
    elif command -v yum &> /dev/null || command -v dnf &> /dev/null; then
        # CentOS/RHEL
        yum install -y -q glibc-langpack-en glibc-langpack-zh > /dev/null 2>&1 || \
        dnf install -y -q glibc-langpack-en glibc-langpack-zh > /dev/null 2>&1 || true
    fi
    
    # 设置当前 session 的环境变量（立即生效）
    export LANG=en_US.UTF-8
    export LC_ALL=en_US.UTF-8
    
    # 写入 /etc/environment 永久生效
    if ! grep -q "LC_ALL" /etc/environment 2>/dev/null; then
        echo 'LANG=en_US.UTF-8' >> /etc/environment
        echo 'LC_ALL=en_US.UTF-8' >> /etc/environment
    fi
}

#===============================================================================
# 网络检测与下载
#===============================================================================

# 全局网络环境变量
NETWORK_ENV=""  # "global" = 国外环境, "china" = 国内环境
FONT_CDN=""     # 字体下载源

# 检测网络环境并选择下载源
select_download_source() {
    print_info "检测网络环境..."
    
    # 测试 Google 连通性 (3秒超时)
    if curl -fsSL --max-time 3 "https://www.google.com" -o /dev/null 2>/dev/null; then
        NETWORK_ENV="global"
        print_success "检测到国外网络环境"
        DOWNLOAD_URL="$GITHUB_RAW"
        FONT_CDN="https://cdn.jsdelivr.net/fontsource/fonts"
        print_info "使用 GitHub 直连 + Fontsource CDN"
    else
        NETWORK_ENV="china"
        print_warning "检测到国内网络环境 (无法连接 Google)"
        DOWNLOAD_URL="$GITHUB_CDN"
        FONT_CDN="https://cdn.jsdelivr.net/fontsource/fonts"  # jsDelivr 国内也能用
        print_info "使用 jsDelivr CDN 镜像"
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
    mkdir -p "${ADMIN_DIR}/fonts"
    
    # 文件列表: 远程路径 -> 本地路径
    local files=(
        "version.json:${BASE_DIR}/version.json"
        "server/core.sh:${BASE_DIR}/core.sh"
        "server/b-ui-cli.sh:${BASE_DIR}/b-ui-cli.sh"
        "server/update.sh:${BASE_DIR}/update.sh"
        "server/residential-helper.sh:${BASE_DIR}/residential-helper.sh"
        "web/server.js:${ADMIN_DIR}/server.js"
        "web/package.json:${ADMIN_DIR}/package.json"
        "web/index.html:${ADMIN_DIR}/index.html"
        "web/style.css:${ADMIN_DIR}/style.css"
        "web/app.js:${ADMIN_DIR}/app.js"
        "web/logo.jpg:${ADMIN_DIR}/logo.jpg"
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
    
    # 下载字体文件（本地化）- 使用 Fontsource CDN
    print_info "下载字体文件..."
    local FONT_DIR="${ADMIN_DIR}/fonts"
    
    # 字体文件列表: 相对路径|本地文件名 (使用全局 FONT_CDN 变量)
    local fonts=(
        "inter@latest/latin-400-normal.woff2|inter-regular.woff2"
        "inter@latest/latin-500-normal.woff2|inter-medium.woff2"
        "inter@latest/latin-600-normal.woff2|inter-semibold.woff2"
        "inter@latest/latin-700-normal.woff2|inter-bold.woff2"
        "jetbrains-mono@latest/latin-400-normal.woff2|jetbrains-regular.woff2"
        "noto-sans-sc@latest/chinese-simplified-400-normal.woff2|noto-sc-regular.woff2"
        "noto-sans-sc@latest/chinese-simplified-500-normal.woff2|noto-sc-medium.woff2"
        "noto-sans-sc@latest/chinese-simplified-700-normal.woff2|noto-sc-bold.woff2"
    )
    
    local font_failed=0
    for item in "${fonts[@]}"; do
        IFS='|' read -r path filename <<< "$item"
        local url="${FONT_CDN}/${path}"
        local dest="${FONT_DIR}/${filename}"
        if [[ ! -f "$dest" ]]; then
            echo -n "  下载 ${filename}... "
            if curl -fsSL "$url" -o "$dest" 2>/dev/null; then
                echo -e "${GREEN}✓${NC}"
            else
                echo -e "${YELLOW}跳过${NC}"
                ((font_failed++)) || true
            fi
        fi
    done
    
    if [[ $font_failed -gt 0 ]]; then
        print_warning "部分字体下载失败，将使用系统字体作为后备"
    else
        print_success "字体文件下载完成"
    fi
    
    print_success "所有文件下载完成"
}

#===============================================================================
# 检测旧版本并迁移
#===============================================================================

OLD_BASE_DIR="/opt/hysteria"

migrate_old_path() {
    # 检测旧路径 /opt/hysteria 是否存在数据
    if [[ -d "$OLD_BASE_DIR" ]] && [[ ! -d "$BASE_DIR" ]]; then
        print_warning "检测到旧安装路径 /opt/hysteria"
        print_info "正在迁移数据到新路径 /opt/b-ui..."
        
        # 创建新目录
        mkdir -p "$BASE_DIR"
        mkdir -p "$ADMIN_DIR"
        
        # 迁移所有文件
        if [[ -f "${OLD_BASE_DIR}/users.json" ]]; then
            cp "${OLD_BASE_DIR}/users.json" "${BASE_DIR}/"
            print_info "  ✓ 迁移 users.json"
        fi
        if [[ -f "${OLD_BASE_DIR}/config.yaml" ]]; then
            cp "${OLD_BASE_DIR}/config.yaml" "${BASE_DIR}/"
            print_info "  ✓ 迁移 config.yaml"
        fi
        if [[ -f "${OLD_BASE_DIR}/xray-config.json" ]]; then
            cp "${OLD_BASE_DIR}/xray-config.json" "${BASE_DIR}/"
            print_info "  ✓ 迁移 xray-config.json"
        fi
        if [[ -f "${OLD_BASE_DIR}/reality-keys.json" ]]; then
            cp "${OLD_BASE_DIR}/reality-keys.json" "${BASE_DIR}/"
            print_info "  ✓ 迁移 reality-keys.json"
        fi
        
        print_success "数据迁移完成"
        
        # 更新 systemd 服务中的路径
        update_service_paths
    fi
}

update_service_paths() {
    local updated=0
    
    # 检查并更新 Hysteria 服务配置
    if [[ -f "/etc/systemd/system/hysteria-server.service" ]]; then
        if grep -q "/opt/hysteria" /etc/systemd/system/hysteria-server.service 2>/dev/null; then
            sed -i 's|/opt/hysteria|/opt/b-ui|g' /etc/systemd/system/hysteria-server.service
            print_info "  ✓ 更新 hysteria-server.service 路径"
            updated=1
        fi
    fi
    
    # 检查并更新 Hysteria 服务的 override.conf（重要！）
    if [[ -f /etc/systemd/system/hysteria-server.service.d/override.conf ]]; then
        if grep -q "/opt/hysteria" /etc/systemd/system/hysteria-server.service.d/override.conf 2>/dev/null; then
            sed -i 's|/opt/hysteria|/opt/b-ui|g' /etc/systemd/system/hysteria-server.service.d/override.conf
            print_info "  ✓ 更新 hysteria-server override.conf 路径"
            updated=1
        fi
    fi
    
    # 检查并更新管理面板服务配置
    if [[ -f "/etc/systemd/system/b-ui-admin.service" ]]; then
        if grep -q "/opt/hysteria" /etc/systemd/system/b-ui-admin.service 2>/dev/null; then
            sed -i 's|/opt/hysteria|/opt/b-ui|g' /etc/systemd/system/b-ui-admin.service
            print_info "  ✓ 更新 b-ui-admin.service 路径"
            updated=1
        fi
    fi
    
    # 检查并更新 Xray 服务配置
    if [[ -f "/etc/systemd/system/xray.service" ]]; then
        if grep -q "/opt/hysteria" /etc/systemd/system/xray.service 2>/dev/null; then
            sed -i 's|/opt/hysteria|/opt/b-ui|g' /etc/systemd/system/xray.service
            print_info "  ✓ 更新 xray.service 路径"
            updated=1
        fi
    fi
    
    # 重载 systemd
    if [[ $updated -eq 1 ]]; then
        systemctl daemon-reload
        print_success "服务配置路径已更新"
    fi
}

check_old_version() {
    # 先执行路径迁移
    migrate_old_path
    
    # 优先检测模块化版本（检查 version.json）
    if [[ -f "${BASE_DIR}/version.json" ]]; then
        local installed_version=$(jq -r '.version' "${BASE_DIR}/version.json" 2>/dev/null || echo "0")
        print_info "检测到已安装版本: v${installed_version}"
        return 1  # 已是模块化版本，走更新流程
    fi
    
    # 检测旧版单文件安装（只有没有 version.json 时才认为是旧版本）
    if [[ -f "/usr/local/bin/b-ui" ]] && [[ ! -f "${BASE_DIR}/version.json" ]]; then
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
    
    return 2  # 全新安装
}

#===============================================================================
# 安装前环境准备 (覆盖所有安装场景)
# 场景1: 全新安装 — 干净 VPS，无需清理
# 场景2: 旧版升级 — Nginx+Certbot → Caddy，需停 nginx/certbot
# 场景3: 同版重装 — 已有 Caddy+证书，需停服务但保留 Caddy 证书
# 场景4: 代理污染 — 系统有 http_proxy 等变量导致 curl 走死代理
# 场景5: 端口冲突 — 80/443 被 Apache/其他进程占用
#===============================================================================

prepare_install_env() {
    print_info "安装前环境检查与清理..."
    
    # ---- 1. 清理代理环境变量 (场景4) ----
    # 防止 curl/wget 走已死的本地代理
    unset http_proxy https_proxy HTTP_PROXY HTTPS_PROXY all_proxy ALL_PROXY no_proxy NO_PROXY 2>/dev/null || true
    export http_proxy="" https_proxy="" HTTP_PROXY="" HTTPS_PROXY=""
    
    # ---- 2. 备份用户数据 (场景2/3: 重装时保留) ----
    if [[ -f "${BASE_DIR}/users.json" ]]; then
        cp -f "${BASE_DIR}/users.json" /tmp/b-ui-users-backup.json 2>/dev/null
        print_info "已备份用户数据"
    fi
    if [[ -f "${BASE_DIR}/config.yaml" ]]; then
        cp -f "${BASE_DIR}/config.yaml" /tmp/b-ui-config-backup.yaml 2>/dev/null
    fi
    
    # ---- 3. 停止所有可能冲突的服务 ----
    local all_services=(
        hysteria-server b-ui-admin caddy xray nginx apache2 httpd
        b-ui-cert-sync.timer b-ui-cert-sync
        certbot.timer certbot-renew.timer
    )
    for svc in "${all_services[@]}"; do
        if systemctl is-active --quiet "$svc" 2>/dev/null; then
            systemctl stop "$svc" 2>/dev/null || true
            print_info "  已停止: $svc"
        fi
        systemctl disable "$svc" 2>/dev/null || true
    done
    
    # ---- 4. 强制释放 80/443 端口 (场景5) ----
    for port in 80 443; do
        local pid=$(lsof -ti :$port 2>/dev/null | head -5)
        if [[ -n "$pid" ]]; then
            print_info "  释放端口 $port (PID: $pid)"
            kill $pid 2>/dev/null || true
            sleep 0.5
            kill -9 $pid 2>/dev/null || true
        fi
    done
    
    # ---- 5. 清理旧的 systemd 服务文件 (重装时重新生成) ----
    rm -f /etc/systemd/system/hysteria-server.service 2>/dev/null
    rm -f /etc/systemd/system/b-ui-admin.service 2>/dev/null
    rm -f /etc/systemd/system/b-ui-cert-sync.service 2>/dev/null
    rm -f /etc/systemd/system/b-ui-cert-sync.timer 2>/dev/null
    rm -rf /etc/systemd/system/hysteria-server.service.d 2>/dev/null
    rm -rf /etc/systemd/system/xray.service.d 2>/dev/null
    systemctl daemon-reload 2>/dev/null
    
    # ---- 6. 清理旧的 cron 任务 ----
    if crontab -l 2>/dev/null | grep -qE "b-ui|cert-check|cert-sync|certbot"; then
        crontab -l 2>/dev/null | grep -vE "b-ui|cert-check|cert-sync|certbot renew" | crontab - 2>/dev/null || true
        print_info "  已清理旧 cron 任务"
    fi
    
    # ---- 7. 清理旧版 Nginx 配置 (场景2: 从 Nginx 迁移到 Caddy) ----
    if [[ -f /etc/nginx/conf.d/b-ui-admin.conf ]]; then
        rm -f /etc/nginx/conf.d/b-ui-admin.conf 2>/dev/null
        print_info "  已清理旧 Nginx 配置"
    fi
    
    # ---- 8. 检查 Caddy 已有证书 (场景3: 重装时保留有效证书) ----
    local caddy_data="/var/lib/caddy/.local/share/caddy"
    if [[ -d "$caddy_data/certificates" ]]; then
        print_info "  检测到 Caddy 已有证书数据，重装后将自动复用"
    fi
    
    print_success "环境准备完成"
}

#===============================================================================
# 安装核心组件
#===============================================================================

run_core_install() {
    print_info "运行核心安装流程..."
    
    # ====== 安装前环境准备：覆盖全新/重装/旧版升级所有场景 ======
    prepare_install_env
    
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

    # SSH 安全加固 (检测到公钥时自动关闭密码登录)
    harden_ssh

    # 根据用户配置决定是否预下载客户端安装包
    if [[ "$PREDOWNLOAD_PACKAGES" =~ ^[yY]$ ]]; then
        download_client_packages
    fi
}

#===============================================================================
# 创建全局命令
#===============================================================================

create_global_command() {
    print_info "创建全局命令..."
    
    cat > /usr/local/bin/b-ui << 'CLIEOF'
#!/bin/bash
# B-UI 全局入口
exec /opt/b-ui/b-ui-cli.sh "$@"
CLIEOF
    
    chmod +x /usr/local/bin/b-ui
    print_success "已创建 b-ui 命令，可使用 'sudo b-ui' 管理"
}

#===============================================================================
# 住宅 IP 交互配置（全新安装时询问）
#===============================================================================

configure_residential_interactive() {
    echo ""
    print_info "========================================================"
    print_info "可选：配置住宅 IP 出站"
    print_info "启用后 OpenAI / Google / Claude 流量将走住宅 IP，其余直出"
    print_info "========================================================"
    read -rp "$(echo -e "${YELLOW}是否配置住宅 IP？(y/N): ${NC}")" ans
    [[ "${ans,,}" != "y" ]] && return 0

    read -rp "$(echo -e "${YELLOW}请粘贴凭据 (socks5://user:pass@host:port): ${NC}")" resi_url
    [[ -z "$resi_url" ]] && { print_warning "已跳过住宅 IP 配置"; return 0; }

    print_info "正在校验连通性..."
    local output exit_code
    output=$("${BASE_DIR}/residential-helper.sh" enable "$resi_url" 2>&1) && exit_code=0 || exit_code=$?

    if [[ $exit_code -eq 0 ]]; then
        local exit_ip isp_info
        exit_ip=$(echo "$output" | head -1)
        isp_info=$(echo "$output" | sed -n '2p')
        print_success "住宅 IP 已启用，出口 IP: ${exit_ip}"
        [[ -n "$isp_info" ]] && print_info "ISP: ${isp_info}"
    else
        print_error "住宅 IP 校验失败（配置未保存）："
        echo "$output" | grep "ERROR:" | sed 's/.*ERROR: //' >&2
        print_warning "可稍后通过 'sudo b-ui' 菜单或 Web 看板重新配置"
    fi
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
    # 临时关闭 set -e，因为 check_old_version 用返回值表示安装类型（0/1/2）
    set +e
    check_old_version
    install_type=$?
    set -e
    
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
            configure_residential_interactive
            ;;
    esac
    
    # 确保 CLI 命令存在（无论哪种安装类型）
    if [[ -f "${BASE_DIR}/b-ui-cli.sh" ]]; then
        ln -sf "${BASE_DIR}/b-ui-cli.sh" /usr/local/bin/b-ui
        chmod +x /usr/local/bin/b-ui
        chmod +x "${BASE_DIR}/b-ui-cli.sh"
    fi
    
    # 配置定时任务 (自动更新 + 证书检查)
    setup_auto_update() {
        # 检查 crontab 是否可用，不可用则安装
        if ! command -v crontab &> /dev/null; then
            print_info "安装 cron..."
            if command -v apt-get &> /dev/null; then
                apt-get update -qq && apt-get install -y -qq cron > /dev/null 2>&1
                systemctl enable cron 2>/dev/null || true
                systemctl start cron 2>/dev/null || true
            elif command -v yum &> /dev/null; then
                yum install -y -q cronie > /dev/null 2>&1
                systemctl enable crond 2>/dev/null || true
                systemctl start crond 2>/dev/null || true
            elif command -v dnf &> /dev/null; then
                dnf install -y -q cronie > /dev/null 2>&1
                systemctl enable crond 2>/dev/null || true
                systemctl start crond 2>/dev/null || true
            fi
            
            if ! command -v crontab &> /dev/null; then
                print_warning "cron 安装失败，跳过定时任务配置"
                return
            fi
            print_success "cron 已安装"
        fi
        
        print_info "配置定时任务..."
        
        # 清理旧的/错误路径的 cron 条目
        local current_cron
        current_cron=$(crontab -l 2>/dev/null || echo "")
        local new_cron
        new_cron=$(echo "$current_cron" | grep -v "b-ui.*update.sh" | grep -v "b-ui.*cert-check" || echo "")
        
        # 添加标记注释和定时任务
        new_cron="${new_cron}
# === B-UI 定时任务 ===
0 */6 * * * ${BASE_DIR}/update.sh auto >> /var/log/b-ui-update.log 2>&1
0 */12 * * * ${BASE_DIR}/cert-check.sh >> /var/log/b-ui-cert-check.log 2>&1"
        
        # 去除空行并写入
        echo "$new_cron" | sed '/^$/d' | crontab -
        print_success "定时任务已配置: 每6小时检查更新, 每12小时检查证书"
    }
    setup_auto_update
    
    echo ""
    echo -e "${GREEN}════════════════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}  安装完成！${NC}"
    echo -e "${GREEN}════════════════════════════════════════════════════════════════${NC}"
    echo ""
    echo -e "  终端管理: ${YELLOW}sudo b-ui${NC}"
    echo -e "  自动更新: ${YELLOW}每小时${NC} (日志: /var/log/b-ui-update.log)"
    echo ""
}

main "$@"
