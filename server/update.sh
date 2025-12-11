#!/bin/bash

#===============================================================================
# B-UI 更新模块
# 功能：检查版本更新并执行升级
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
GITHUB_RAW="https://raw.githubusercontent.com/Buxiulei/b-ui/main"
GITHUB_CDN="https://cdn.jsdelivr.net/gh/Buxiulei/b-ui@main"
BASE_DIR="/opt/b-ui"
ADMIN_DIR="${BASE_DIR}/admin"

# 打印函数
print_info() { echo -e "${BLUE}[INFO]${NC} $1"; }
print_success() { echo -e "${GREEN}[SUCCESS]${NC} $1"; }
print_warning() { echo -e "${YELLOW}[WARNING]${NC} $1"; }
print_error() { echo -e "${RED}[ERROR]${NC} $1"; }

#===============================================================================
# 选择下载源
#===============================================================================

select_download_source() {
    # 测试 GitHub 直连
    local github_time=$(curl -o /dev/null -s -w '%{time_total}' --max-time 5 "${GITHUB_RAW}/version.json" 2>/dev/null || echo "999")
    
    # 测试 CDN
    local cdn_time=$(curl -o /dev/null -s -w '%{time_total}' --max-time 5 "${GITHUB_CDN}/version.json" 2>/dev/null || echo "999")
    
    if (( $(echo "$cdn_time < $github_time" | bc -l 2>/dev/null || echo "0") )); then
        DOWNLOAD_URL="$GITHUB_CDN"
    else
        DOWNLOAD_URL="$GITHUB_RAW"
    fi
}

#===============================================================================
# 获取版本信息
#===============================================================================

get_local_version() {
    if [[ -f "${BASE_DIR}/version.json" ]]; then
        jq -r '.version' "${BASE_DIR}/version.json" 2>/dev/null || echo "0.0.0"
    else
        echo "0.0.0"
    fi
}

get_remote_version() {
    select_download_source
    curl -fsSL "${DOWNLOAD_URL}/version.json" 2>/dev/null | jq -r '.version' 2>/dev/null || echo ""
}

#===============================================================================
# 版本比较
#===============================================================================

version_compare() {
    # 比较两个版本号
    # 返回: 0 = 相等, 1 = $1 > $2, 2 = $1 < $2
    local v1="$1"
    local v2="$2"
    
    if [[ "$v1" == "$v2" ]]; then
        return 0
    fi
    
    local IFS='.'
    local i
    local ver1=($v1)
    local ver2=($v2)
    
    for ((i=0; i<${#ver1[@]} || i<${#ver2[@]}; i++)); do
        local n1=${ver1[i]:-0}
        local n2=${ver2[i]:-0}
        
        if ((n1 > n2)); then
            return 1
        elif ((n1 < n2)); then
            return 2
        fi
    done
    
    return 0
}

#===============================================================================
# 检查更新
#===============================================================================

check_update() {
    print_info "检查 B-UI 更新..."
    
    local local_ver=$(get_local_version)
    local remote_ver=$(get_remote_version)
    
    if [[ -z "$remote_ver" ]]; then
        print_error "无法获取远程版本信息"
        return 1
    fi
    
    echo ""
    echo -e "  本地版本: ${YELLOW}v${local_ver}${NC}"
    echo -e "  远程版本: ${GREEN}v${remote_ver}${NC}"
    echo ""
    
    version_compare "$remote_ver" "$local_ver"
    local result=$?
    
    if [[ $result -eq 1 ]]; then
        # 远程版本更新
        print_success "发现新版本！"
        
        # 获取更新日志
        local changelog=$(curl -fsSL "${DOWNLOAD_URL}/version.json" 2>/dev/null | jq -r ".changelog[\"$remote_ver\"]" 2>/dev/null)
        if [[ -n "$changelog" && "$changelog" != "null" ]]; then
            echo ""
            echo -e "${CYAN}更新内容:${NC}"
            echo -e "  $changelog"
            echo ""
        fi
        
        return 0  # 有更新
    elif [[ $result -eq 0 ]]; then
        print_success "已是最新版本"
        return 1  # 无更新
    else
        print_info "当前版本比远程版本新（开发版本）"
        return 1
    fi
}

#===============================================================================
# 执行更新
#===============================================================================

do_update() {
    print_info "开始更新..."
    
    select_download_source
    
    # 备份配置文件
    print_info "备份配置..."
    [[ -f "${BASE_DIR}/users.json" ]] && cp "${BASE_DIR}/users.json" "/tmp/b-ui-users-backup.json"
    [[ -f "${BASE_DIR}/config.yaml" ]] && cp "${BASE_DIR}/config.yaml" "/tmp/b-ui-config-backup.yaml"
    [[ -f "${BASE_DIR}/xray-config.json" ]] && cp "${BASE_DIR}/xray-config.json" "/tmp/b-ui-xray-backup.json"
    [[ -f "${BASE_DIR}/reality-keys.json" ]] && cp "${BASE_DIR}/reality-keys.json" "/tmp/b-ui-keys-backup.json"
    
    # 停止服务
    print_info "停止服务..."
    systemctl stop b-ui-admin 2>/dev/null || true
    
    # 下载新文件
    print_info "下载更新文件..."
    
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
        mkdir -p "$(dirname "$local")"
        echo -n "  更新 ${remote}... "
        if curl -fsSL "${DOWNLOAD_URL}/${remote}" -o "${local}" 2>/dev/null; then
            echo -e "${GREEN}✓${NC}"
        else
            echo -e "${RED}✗${NC}"
            ((failed++))
        fi
    done
    
    # 设置权限
    chmod +x "${BASE_DIR}/core.sh" 2>/dev/null
    chmod +x "${BASE_DIR}/b-ui-cli.sh" 2>/dev/null
    chmod +x "${BASE_DIR}/update.sh" 2>/dev/null
    
    # 恢复配置
    print_info "恢复配置..."
    [[ -f "/tmp/b-ui-users-backup.json" ]] && cp "/tmp/b-ui-users-backup.json" "${BASE_DIR}/users.json"
    [[ -f "/tmp/b-ui-config-backup.yaml" ]] && cp "/tmp/b-ui-config-backup.yaml" "${BASE_DIR}/config.yaml"
    [[ -f "/tmp/b-ui-xray-backup.json" ]] && cp "/tmp/b-ui-xray-backup.json" "${BASE_DIR}/xray-config.json"
    [[ -f "/tmp/b-ui-keys-backup.json" ]] && cp "/tmp/b-ui-keys-backup.json" "${BASE_DIR}/reality-keys.json"
    
    # 清理临时文件
    rm -f /tmp/b-ui-*-backup.* 2>/dev/null
    
    # 更新服务配置路径（如果需要）
    update_service_paths
    
    # 重启服务
    print_info "重启服务..."
    systemctl start b-ui-admin 2>/dev/null || true
    systemctl restart hysteria-server 2>/dev/null || true
    systemctl restart xray 2>/dev/null || true
    
    if [[ $failed -gt 0 ]]; then
        print_warning "更新完成，但有 ${failed} 个文件更新失败"
    else
        print_success "更新完成！"
    fi
    
    echo ""
    echo -e "  新版本: ${GREEN}v$(get_local_version)${NC}"
    echo ""
}

#===============================================================================
# 更新服务配置路径
#===============================================================================

update_service_paths() {
    local updated=0
    
    # 检查并更新 Hysteria 服务配置
    if grep -q "/opt/hysteria" /etc/systemd/system/hysteria-server.service 2>/dev/null; then
        sed -i 's|/opt/hysteria|/opt/b-ui|g' /etc/systemd/system/hysteria-server.service
        print_info "  ✓ 更新 hysteria-server.service 路径"
        updated=1
    fi
    
    # 检查并更新管理面板服务配置
    if grep -q "/opt/hysteria" /etc/systemd/system/b-ui-admin.service 2>/dev/null; then
        sed -i 's|/opt/hysteria|/opt/b-ui|g' /etc/systemd/system/b-ui-admin.service
        print_info "  ✓ 更新 b-ui-admin.service 路径"
        updated=1
    fi
    
    # 检查并更新 Xray 服务配置
    if grep -q "/opt/hysteria" /etc/systemd/system/xray.service 2>/dev/null; then
        sed -i 's|/opt/hysteria|/opt/b-ui|g' /etc/systemd/system/xray.service
        print_info "  ✓ 更新 xray.service 路径"
        updated=1
    fi
    
    # 重载 systemd
    if [[ $updated -eq 1 ]]; then
        systemctl daemon-reload
        print_success "服务配置路径已更新"
    fi
}

#===============================================================================
# 更新内核（Hysteria2 和 Xray）
#===============================================================================

update_kernel() {
    print_info "正在更新内核..."
    echo ""
    
    # 更新 Hysteria2
    print_info "更新 Hysteria2..."
    local old_hy=$(hysteria version 2>/dev/null | head -n1 || echo "未知")
    bash <(curl -fsSL https://get.hy2.sh/)
    local new_hy=$(hysteria version 2>/dev/null | head -n1 || echo "未知")
    echo -e "  Hysteria2: ${YELLOW}${old_hy}${NC} -> ${GREEN}${new_hy}${NC}"
    
    # 更新 Xray
    if command -v xray &> /dev/null; then
        print_info "更新 Xray..."
        local old_xray=$(xray version 2>/dev/null | head -n1 || echo "未知")
        bash -c "$(curl -L https://github.com/XTLS/Xray-install/raw/main/install-release.sh)" @ install
        local new_xray=$(xray version 2>/dev/null | head -n1 || echo "未知")
        echo -e "  Xray: ${YELLOW}${old_xray}${NC} -> ${GREEN}${new_xray}${NC}"
    fi
    
    # 重启服务
    systemctl restart hysteria-server 2>/dev/null || true
    systemctl restart xray 2>/dev/null || true
    
    print_success "内核更新完成！"
}

#===============================================================================
# 检查并自动更新入口
#===============================================================================

check_and_update() {
    if check_update; then
        echo ""
        read -p "是否立即更新? (y/n): " confirm
        if [[ "$confirm" == "y" || "$confirm" == "Y" ]]; then
            do_update
        else
            print_info "已跳过更新"
        fi
    fi
}

# 如果直接运行此脚本
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    check_and_update
fi
