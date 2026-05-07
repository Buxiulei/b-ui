#!/bin/bash

#===============================================================================
# B-UI 更新模块
# 功能：检查版本更新并执行升级
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
GITHUB_RAW="https://raw.githubusercontent.com/Buxiulei/b-ui/main"
GITHUB_CDN="https://raw.githack.com/Buxiulei/b-ui/main"
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
    # 优先使用 GitHub 直连 (实时性最佳，无缓存)
    # 只有当 GitHub 完全无法访问时才回退到 CDN
    if curl -fsSL --max-time 5 "${GITHUB_RAW}/version.json" -o /dev/null 2>/dev/null; then
        DOWNLOAD_URL="$GITHUB_RAW"
    else
        # GitHub 不可访问，回退到 CDN (有缓存但稳定)
        DOWNLOAD_URL="$GITHUB_CDN"
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
    
    # 显示使用的下载源
    if [[ "$DOWNLOAD_URL" == "$GITHUB_RAW" ]]; then
        print_info "使用 GitHub 源检测 (实时)"
    else
        print_info "使用 CDN 源检测 (GitHub 不可达)"
    fi
    
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
        "server/residential-helper.sh:${BASE_DIR}/residential-helper.sh"
        "b-ui-client.sh:${BASE_DIR}/b-ui-client.sh"
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
        mkdir -p "$(dirname "$local")"
        echo -n "  更新 ${remote}... "
        if curl -fsSL "${DOWNLOAD_URL}/${remote}" -o "${local}" 2>/dev/null; then
            echo -e "${GREEN}✓${NC}"
        else
            echo -e "${RED}✗${NC}"
            ((failed++))
        fi
    done
    
    # 同步到 packages 分发目录（供客户端下载）
    if [[ -d "${BASE_DIR}/packages" ]]; then
        cp -f "${BASE_DIR}/version.json" "${BASE_DIR}/packages/version.json" 2>/dev/null || true
        cp -f "${BASE_DIR}/b-ui-client.sh" "${BASE_DIR}/packages/b-ui-client.sh" 2>/dev/null || true
    fi
    
    # 设置权限
    chmod +x "${BASE_DIR}/core.sh" 2>/dev/null
    chmod +x "${BASE_DIR}/b-ui-cli.sh" 2>/dev/null
    chmod +x "${BASE_DIR}/update.sh" 2>/dev/null
    chmod +x "${BASE_DIR}/residential-helper.sh" 2>/dev/null
    
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
    
    # 应用 systemd 资源隔离配置（确保 Hy2/VLESS 协议隔离）
    apply_systemd_configs
    
    # 安装 Web 面板依赖
    if [[ -f "${ADMIN_DIR}/package.json" ]]; then
        print_info "安装 Web 面板依赖..."
        cd "${ADMIN_DIR}" && npm install 2>&1 && cd - > /dev/null
        print_success "依赖安装完成"
    fi
    
    # 确保 CLI 命令存在
    if [[ -f "${BASE_DIR}/b-ui-cli.sh" ]]; then
        ln -sf "${BASE_DIR}/b-ui-cli.sh" /usr/local/bin/b-ui
        chmod +x /usr/local/bin/b-ui
        chmod +x "${BASE_DIR}/b-ui-cli.sh"
    fi
    
    # 重启服务
    print_info "重启服务..."
    systemctl start b-ui-admin 2>/dev/null || true
    systemctl restart hysteria-server 2>/dev/null || true
    systemctl restart xray 2>/dev/null || true

    # 重新应用住宅 IP 配置（包含下载 sing-box、启动中继、更新 xray/hy2 出站）
    if [[ -f "${BASE_DIR}/residential-helper.sh" ]]; then
        print_info "重新应用住宅 IP 配置..."
        "${BASE_DIR}/residential-helper.sh" reapply 2>/dev/null || true
    fi

    # 确保定时任务配置正确
    ensure_cron_jobs
    
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
    
    # 检查并更新 Hysteria 服务的 override.conf（重要！）
    if [[ -f /etc/systemd/system/hysteria-server.service.d/override.conf ]]; then
        if grep -q "/opt/hysteria" /etc/systemd/system/hysteria-server.service.d/override.conf 2>/dev/null; then
            sed -i 's|/opt/hysteria|/opt/b-ui|g' /etc/systemd/system/hysteria-server.service.d/override.conf
            print_info "  ✓ 更新 hysteria-server override.conf 路径"
            updated=1
        fi
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
# 应用 systemd 资源隔离配置
# 确保 Hysteria2 和 Xray 服务有正确的资源限制和隔离设置
#===============================================================================

# config.yaml 完整性修复：从 users.json 重新生成 auth/trafficStats/masquerade/sniff 段
# 保留 listen / tls / quic / RESIDENTIAL-START..END 等已有段（避免覆盖端口/证书路径/中继配置）
repair_hysteria_config() {
    local config_file="${BASE_DIR}/config.yaml"
    local users_file="${BASE_DIR}/users.json"
    [[ -f "$config_file" ]] || return 1

    # 提取已有的关键段：listen / tls / quic / RESIDENTIAL block
    local port
    port=$(awk '/^listen:/{sub(/^listen: *:/,""); print; exit}' "$config_file")
    [[ -z "$port" ]] && port=10000

    local cert_path key_path
    cert_path=$(awk '/^  cert:/{print $2; exit}' "$config_file")
    key_path=$(awk '/^  key:/{print $2; exit}' "$config_file")
    [[ -z "$cert_path" ]] && cert_path="${BASE_DIR}/certs/fullchain.pem"
    [[ -z "$key_path" ]]  && key_path="${BASE_DIR}/certs/privkey.pem"

    # 读取 RESIDENTIAL block（包含 START/END 标记）
    local residential_block
    residential_block=$(awk '/^# B-UI:RESIDENTIAL-START/,/^# B-UI:RESIDENTIAL-END/' "$config_file")
    [[ -z "$residential_block" ]] && residential_block=$'# B-UI:RESIDENTIAL-START\n# B-UI:RESIDENTIAL-END'

    # 重新生成完整 config（HTTP 认证模式，配合 b-ui-admin 实现用户管理 + 限速）
    cat > "${config_file}.tmp" <<EOF
# Hysteria2 服务器配置
# 修复时间: $(date)

listen: :${port}

tls:
  sniGuard: disable
  cert: ${cert_path}
  key: ${key_path}

quic:
  maxIdleTimeout: 120s

# HTTP 认证 (支持用户级别限速)
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
    url: https://www.bing.com/
    rewriteHost: true

sniff:
  enable: true
  timeout: 2s
  rewriteDomain: true
  tcpPorts: 80,443,8000-9000
  udpPorts: all

${residential_block}
EOF
    mv "${config_file}.tmp" "$config_file"
    chmod 644 "$config_file"
}

apply_systemd_configs() {
    print_info "应用 systemd 资源隔离配置..."
    local updated=0

    # 获取当前配置路径
    local config_file="${BASE_DIR}/config.yaml"
    local xray_config="${BASE_DIR}/xray-config.json"

    # 迁移 config.yaml：移除过大的 QUIC 接收窗口配置（旧版默认 64 MiB/conn 在小内存机器上每会话占用 ~80MB）
    # 关键：只有当 maxConnReceiveWindow 真的存在时才执行 sed 范围删除，
    # 否则 sed 找不到结束锚点会一路删到 EOF，把 auth/masquerade/sniff 等全冲掉（v3.4.3 复盘）。
    local hysteria_config_changed=0
    if [[ -f "$config_file" ]] && grep -q '^  maxConnReceiveWindow:' "$config_file"; then
        cp "$config_file" "${config_file}.bak.$(date +%Y%m%d-%H%M%S)"
        # 优先按注释锚定（更精确，连同注释一起删除）
        if grep -q '^# QUIC 流控优化' "$config_file"; then
            sed -i '/^# QUIC 流控优化/,/^  maxConnReceiveWindow:/d' "$config_file"
        else
            # 没有注释，只能按 quic: 锚定（仅当 maxConnReceiveWindow 还在时才安全）
            sed -i '/^quic:$/,/^  maxConnReceiveWindow:/d' "$config_file"
        fi
        if ! grep -q '^  maxConnReceiveWindow:' "$config_file"; then
            print_info "  ✓ 移除 config.yaml 中过大的 QUIC 窗口配置（已备份）"
            hysteria_config_changed=1
        fi
    fi

    # 迁移 config.yaml：确保 udpPorts: all 存在（供住宅 IP UDP 域名分流）
    if [[ -f "$config_file" ]] && grep -q 'tcpPorts:' "$config_file" && ! grep -q 'udpPorts:' "$config_file"; then
        sed -i '/tcpPorts:.*80,443/a\  udpPorts: all' "$config_file"
        print_info "  ✓ config.yaml 添加 udpPorts: all（供住宅 IP UDP 域名分流）"
        hysteria_config_changed=1
    fi

    # 迁移 config.yaml：添加 QUIC maxIdleTimeout（默认 30s 过短，空闲断连后客户端需约 1 分钟恢复）
    if [[ -f "$config_file" ]] && ! grep -q 'maxIdleTimeout' "$config_file"; then
        if grep -q '^quic:' "$config_file"; then
            sed -i '/^quic:/a\  maxIdleTimeout: 120s' "$config_file"
        else
            awk '/^listen:/{print; print ""; print "quic:"; print "  maxIdleTimeout: 120s"; next}1' \
                "$config_file" > "${config_file}.tmp" && mv "${config_file}.tmp" "$config_file"
        fi
        print_info "  ✓ config.yaml 添加 QUIC maxIdleTimeout: 120s（防止空闲断连）"
        hysteria_config_changed=1
    fi

    # config.yaml 完整性兜底：检查关键段是否齐全，缺失则基于 users.json 重新生成
    # （保护历史损坏的 config —— 如旧版 update.sh 的 sed 范围删除 bug 把 auth/sniff 等冲掉）
    if [[ -f "$config_file" ]]; then
        local missing=()
        grep -qE '^auth:'         "$config_file" || missing+=(auth)
        grep -qE '^trafficStats:' "$config_file" || missing+=(trafficStats)
        grep -qE '^masquerade:'   "$config_file" || missing+=(masquerade)
        grep -qE '^sniff:'        "$config_file" || missing+=(sniff)
        if [[ ${#missing[@]} -gt 0 ]]; then
            cp "$config_file" "${config_file}.bak.broken-$(date +%Y%m%d-%H%M%S)"
            repair_hysteria_config && {
                print_info "  ✓ config.yaml 修复：补回缺失段 [${missing[*]}]"
                hysteria_config_changed=1
            }
        fi
    fi

    # 应用 Hysteria2 服务配置
    local hysteria_mem_changed=0
    if [[ -d /etc/systemd/system/hysteria-server.service.d ]]; then
        # 检测旧配置是否缺少内存优化（用于决定是否需要 restart）
        if ! grep -q 'GOMEMLIMIT' /etc/systemd/system/hysteria-server.service.d/override.conf 2>/dev/null; then
            hysteria_mem_changed=1
        fi
        cat > /etc/systemd/system/hysteria-server.service.d/override.conf << EOF
[Unit]
# Hysteria2 使用 QUIC (UDP)，与 Xray (TCP) 独立运行
Wants=network-online.target
After=network-online.target

[Service]
ExecStart=
ExecStart=/usr/local/bin/hysteria server --config ${config_file}

# 资源隔离：防止 UDP 大流量影响 TCP 服务
CPUSchedulingPolicy=other
Nice=-5
LimitNOFILE=1048576

# 内存回收：让 Go runtime 主动归还内存给 OS（Go 1.19+）
# 不设此变量时 hysteria 长跑后 RSS 会缓慢爬升至历史峰值不释放
Environment=GOMEMLIMIT=400MiB

# cgroup 兜底：超过 500M 开始 throttle，700M 硬上限触发 OOM-restart
# 适配 1G 小内存机器；2G+ 机器可手动放宽到 800M/1G
MemoryHigh=500M
MemoryMax=700M

# 确保服务稳定运行
Restart=always
RestartSec=3
EOF
        print_info "  ✓ 更新 Hysteria2 服务配置"
        updated=1
    fi

    # 小内存机器（≤2G）自动降低 swappiness，避免 hysteria 工作集被换出
    if [[ ! -f /etc/sysctl.d/99-b-ui-memory.conf ]]; then
        local mem_mb
        mem_mb=$(awk '/^MemTotal:/ {print int($2/1024)}' /proc/meminfo)
        if [[ $mem_mb -le 2048 ]]; then
            cat > /etc/sysctl.d/99-b-ui-memory.conf <<'SYSCTL_EOF'
# b-ui 内存策略：小内存机器（≤2G）降低 swap 倾向
# 配合 hysteria-server.service 的 GOMEMLIMIT/MemoryHigh/MemoryMax 一起生效
vm.swappiness = 10
SYSCTL_EOF
            sysctl -p /etc/sysctl.d/99-b-ui-memory.conf >/dev/null 2>&1 && \
                print_info "  ✓ 应用 swappiness=10 (检测到 ${mem_mb}MB ≤ 2G)"
        fi
    fi
    
    # 应用 Xray 服务配置
    if [[ -d /etc/systemd/system/xray.service.d ]]; then
        cat > /etc/systemd/system/xray.service.d/override.conf << EOF
[Unit]
# Xray 使用 TCP，与 Hysteria2 (UDP) 独立运行
Wants=network-online.target
After=network-online.target

[Service]
ExecStart=
ExecStart=/usr/local/bin/xray run -config ${xray_config}

# 资源隔离：确保 TCP 服务稳定
CPUSchedulingPolicy=other
Nice=-5
LimitNOFILE=1048576

# 确保服务稳定运行
Restart=always
RestartSec=3
EOF
        print_info "  ✓ 更新 Xray 服务配置"
        updated=1
    fi
    
    # 应用端口跳跃配置（如果配置文件存在）
    if [[ -f "${BASE_DIR}/port-hopping.json" ]]; then
        local enabled=$(jq -r '.enabled' "${BASE_DIR}/port-hopping.json" 2>/dev/null)
        if [[ "$enabled" == "true" ]]; then
            local start_port=$(jq -r '.startPort' "${BASE_DIR}/port-hopping.json")
            local end_port=$(jq -r '.endPort' "${BASE_DIR}/port-hopping.json")
            local listen_port=$(jq -r '.listenPort' "${BASE_DIR}/port-hopping.json")
            local iface=$(jq -r '.interface' "${BASE_DIR}/port-hopping.json")
            [[ -z "$iface" || "$iface" == "null" ]] && iface=$(ip route get 8.8.8.8 2>/dev/null | grep -oP 'dev \K\S+' | head -1)
            [[ -z "$iface" ]] && iface="eth0"
            
            # 清理旧规则
            iptables -t nat -D PREROUTING -i "$iface" -p udp --dport ${start_port}:${end_port} -j REDIRECT --to-ports ${listen_port} 2>/dev/null || true
            
            # 添加新规则（仅 UDP）
            if iptables -t nat -A PREROUTING -i "$iface" -p udp --dport ${start_port}:${end_port} \
                -m comment --comment "Hysteria2-PortHopping" \
                -j REDIRECT --to-ports ${listen_port} 2>/dev/null; then
                print_info "  ✓ 应用端口跳跃规则 (UDP ${start_port}-${end_port} -> ${listen_port})"
            fi
        fi
    fi
    
    # 重载 systemd
    if [[ $updated -eq 1 ]]; then
        systemctl daemon-reload
    fi

    # cgroup 内存限制、GOMEMLIMIT、config.yaml 任一变化都需要 restart hysteria 才生效
    if { [[ $hysteria_mem_changed -eq 1 ]] || [[ $hysteria_config_changed -eq 1 ]]; } && \
       systemctl is-active --quiet hysteria-server; then
        print_info "  ↻ 重启 hysteria-server 应用内存优化（客户端会自动重连）"
        systemctl restart hysteria-server
    fi

    if [[ $updated -eq 1 ]]; then
        print_success "资源隔离配置已应用"
    fi
}

#===============================================================================
# 更新内核（Hysteria2、Xray、sing-box）— 带版本检查
#===============================================================================

# 辅助: 版本号比较，相等返回 0，$1 更新返回 1
_is_newer() {
    local local_v="$1" remote_v="$2"
    [[ -z "$remote_v" ]] && return 1
    [[ "$local_v" == "$remote_v" ]] && return 1
    [[ "$(printf '%s\n' "$local_v" "$remote_v" | sort -V | tail -n1)" == "$remote_v" ]] && return 0
    return 1
}

update_kernel() {
    print_info "检查内核版本..."
    echo ""
    local has_update=false

    # --- Hysteria2 ---
    local local_hy=$(hysteria version 2>/dev/null | grep "^Version:" | awk '{print $2}' | sed 's/^v//' || echo "")
    local remote_hy=$(curl -fsSL --max-time 10 "https://api.github.com/repos/apernet/hysteria/releases/latest" 2>/dev/null \
        | grep '"tag_name"' | sed -E 's/.*"tag_name":\s*"app\/v?([^"]+)".*/\1/')
    [[ -z "$remote_hy" ]] && remote_hy=$(curl -fsSL --max-time 10 "https://api.github.com/repos/apernet/hysteria/releases/latest" 2>/dev/null \
        | grep '"tag_name"' | sed -E 's/.*"v?([0-9][^"]+)".*/\1/')

    echo -e "${CYAN}[Hysteria2]${NC}"
    if [[ -n "$local_hy" && -n "$remote_hy" ]]; then
        echo -e "  本地: ${YELLOW}v${local_hy}${NC}  远程: ${GREEN}v${remote_hy}${NC}"
        if _is_newer "$local_hy" "$remote_hy"; then
            print_info "发现新版本，正在更新..."
            bash <(curl -fsSL https://get.hy2.sh/)
            has_update=true
        else
            print_success "已是最新版本"
        fi
    elif [[ -z "$local_hy" ]]; then
        print_warning "未安装，跳过"
    else
        print_warning "无法获取远程版本，跳过"
    fi

    # --- Xray ---
    if command -v xray &> /dev/null; then
        local local_xray=$(xray version 2>/dev/null | head -n1 | awk '{print $2}' | sed 's/^v//' || echo "")
        local remote_xray=$(curl -fsSL --max-time 10 "https://api.github.com/repos/XTLS/Xray-core/releases/latest" 2>/dev/null \
            | grep '"tag_name"' | sed -E 's/.*"v?([0-9][^"]+)".*/\1/')
        echo ""
        echo -e "${CYAN}[Xray]${NC}"
        if [[ -n "$local_xray" && -n "$remote_xray" ]]; then
            echo -e "  本地: ${YELLOW}v${local_xray}${NC}  远程: ${GREEN}v${remote_xray}${NC}"
            if _is_newer "$local_xray" "$remote_xray"; then
                print_info "发现新版本，正在更新..."
                bash -c "$(curl -L https://github.com/XTLS/Xray-install/raw/main/install-release.sh)" @ install
                has_update=true
            else
                print_success "已是最新版本"
            fi
        else
            print_warning "无法获取版本信息，跳过"
        fi
    fi

    # --- sing-box ---
    if command -v sing-box &> /dev/null; then
        local local_sb=$(sing-box version 2>/dev/null | head -n1 | awk '{print $3}' | sed 's/^v//' || echo "")
        local remote_sb=$(curl -fsSL --max-time 10 "https://api.github.com/repos/SagerNet/sing-box/releases/latest" 2>/dev/null \
            | grep '"tag_name"' | sed -E 's/.*"v?([0-9][^"]+)".*/\1/')
        echo ""
        echo -e "${CYAN}[sing-box]${NC}"
        if [[ -n "$local_sb" && -n "$remote_sb" ]]; then
            echo -e "  本地: ${YELLOW}v${local_sb}${NC}  远程: ${GREEN}v${remote_sb}${NC}"
            if _is_newer "$local_sb" "$remote_sb"; then
                print_info "发现新版本，正在更新..."
                if command -v apt-get &> /dev/null; then
                    apt-get update -qq && apt-get install -y -qq sing-box
                else
                    bash <(curl -fsSL https://sing-box.app/install.sh)
                fi
                has_update=true
            else
                print_success "已是最新版本"
            fi
        else
            print_warning "无法获取版本信息，跳过"
        fi
    fi

    # 重启服务
    if [[ "$has_update" == "true" ]]; then
        echo ""
        systemctl restart hysteria-server 2>/dev/null || true
        systemctl restart xray 2>/dev/null || true
        print_success "内核更新完成，服务已重启"
    else
        echo ""
        print_success "所有内核均为最新版本"
    fi
}

#===============================================================================
# 静默内核更新 (用于 cron 定时任务)
#===============================================================================

auto_update_kernel() {
    local LOG_FILE="/var/log/b-ui-kernel-update.log"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] 开始检查内核更新..." >> "$LOG_FILE"

    local updated=false

    # Hysteria2
    local local_hy=$(hysteria version 2>/dev/null | grep "^Version:" | awk '{print $2}' | sed 's/^v//' || echo "")
    local remote_hy=$(curl -fsSL --max-time 10 "https://api.github.com/repos/apernet/hysteria/releases/latest" 2>/dev/null \
        | grep '"tag_name"' | sed -E 's/.*"tag_name":\s*"app\/v?([^"]+)".*/\1/')
    [[ -z "$remote_hy" ]] && remote_hy=$(curl -fsSL --max-time 10 "https://api.github.com/repos/apernet/hysteria/releases/latest" 2>/dev/null \
        | grep '"tag_name"' | sed -E 's/.*"v?([0-9][^"]+)".*/\1/')
    if [[ -n "$local_hy" && -n "$remote_hy" ]] && _is_newer "$local_hy" "$remote_hy"; then
        echo "[$(date '+%Y-%m-%d %H:%M:%S')] Hysteria2: v${local_hy} -> v${remote_hy}, 更新中..." >> "$LOG_FILE"
        bash <(curl -fsSL https://get.hy2.sh/) >> "$LOG_FILE" 2>&1 || true
        systemctl restart hysteria-server 2>/dev/null || true
        updated=true
    fi

    # Xray
    if command -v xray &> /dev/null; then
        local local_xray=$(xray version 2>/dev/null | head -n1 | awk '{print $2}' | sed 's/^v//' || echo "")
        local remote_xray=$(curl -fsSL --max-time 10 "https://api.github.com/repos/XTLS/Xray-core/releases/latest" 2>/dev/null \
            | grep '"tag_name"' | sed -E 's/.*"v?([0-9][^"]+)".*/\1/')
        if [[ -n "$local_xray" && -n "$remote_xray" ]] && _is_newer "$local_xray" "$remote_xray"; then
            echo "[$(date '+%Y-%m-%d %H:%M:%S')] Xray: v${local_xray} -> v${remote_xray}, 更新中..." >> "$LOG_FILE"
            bash -c "$(curl -L https://github.com/XTLS/Xray-install/raw/main/install-release.sh)" @ install >> "$LOG_FILE" 2>&1 || true
            systemctl restart xray 2>/dev/null || true
            updated=true
        fi
    fi

    # sing-box
    if command -v sing-box &> /dev/null; then
        local local_sb=$(sing-box version 2>/dev/null | head -n1 | awk '{print $3}' | sed 's/^v//' || echo "")
        local remote_sb=$(curl -fsSL --max-time 10 "https://api.github.com/repos/SagerNet/sing-box/releases/latest" 2>/dev/null \
            | grep '"tag_name"' | sed -E 's/.*"v?([0-9][^"]+)".*/\1/')
        if [[ -n "$local_sb" && -n "$remote_sb" ]] && _is_newer "$local_sb" "$remote_sb"; then
            echo "[$(date '+%Y-%m-%d %H:%M:%S')] sing-box: v${local_sb} -> v${remote_sb}, 更新中..." >> "$LOG_FILE"
            if command -v apt-get &> /dev/null; then
                apt-get update -qq && apt-get install -y -qq sing-box >> "$LOG_FILE" 2>&1 || true
            else
                bash <(curl -fsSL https://sing-box.app/install.sh) >> "$LOG_FILE" 2>&1 || true
            fi
            updated=true
        fi
    fi

    if [[ "$updated" == "true" ]]; then
        echo "[$(date '+%Y-%m-%d %H:%M:%S')] 内核更新完成" >> "$LOG_FILE"
    else
        echo "[$(date '+%Y-%m-%d %H:%M:%S')] 所有内核已是最新" >> "$LOG_FILE"
    fi
}

#===============================================================================
# 确保 CLI 命令存在
#===============================================================================

ensure_cli_exists() {
    # 确保 b-ui CLI 命令存在
    if [[ ! -L "/usr/local/bin/b-ui" ]] || [[ ! -e "/usr/local/bin/b-ui" ]]; then
        if [[ -f "${BASE_DIR}/b-ui-cli.sh" ]]; then
            ln -sf "${BASE_DIR}/b-ui-cli.sh" /usr/local/bin/b-ui
            chmod +x /usr/local/bin/b-ui
            print_info "已创建 b-ui 命令"
        fi
    fi
}

#===============================================================================
# 确保定时任务配置正确
#===============================================================================

ensure_cron_jobs() {
    # 检查 crontab 是否可用
    if ! command -v crontab &> /dev/null; then
        return
    fi
    
    local needs_fix=0
    local current_cron
    current_cron=$(crontab -l 2>/dev/null || echo "")
    
    # 检查是否存在正确路径的定时任务
    if ! echo "$current_cron" | grep -q "${BASE_DIR}/update.sh auto"; then
        needs_fix=1
    fi
    if ! echo "$current_cron" | grep -q "${BASE_DIR}/cert-check.sh"; then
        needs_fix=1
    fi
    if ! echo "$current_cron" | grep -q "${BASE_DIR}/update.sh kernel"; then
        needs_fix=1
    fi
    # 检查是否存在错误路径
    if echo "$current_cron" | grep -q "server/update.sh"; then
        needs_fix=1
    fi
    
    if [[ $needs_fix -eq 1 ]]; then
        print_info "修复定时任务配置..."
        # 移除所有 b-ui 相关的旧条目
        local new_cron
        new_cron=$(echo "$current_cron" | grep -v "b-ui.*update.sh" | grep -v "b-ui.*cert-check" | grep -v "B-UI 定时" || echo "")
        
        # 添加正确的定时任务
        new_cron="${new_cron}
# === B-UI 定时任务 ===
0 */6 * * * ${BASE_DIR}/update.sh auto >> /var/log/b-ui-update.log 2>&1
30 */12 * * * ${BASE_DIR}/update.sh kernel >> /var/log/b-ui-kernel-update.log 2>&1
0 */12 * * * ${BASE_DIR}/cert-check.sh >> /var/log/b-ui-cert-check.log 2>&1"
        
        echo "$new_cron" | sed '/^$/d' | crontab -
        print_success "定时任务已修复: 每6小时检查B-UI更新, 每12小时检查内核更新和证书"
    fi
}

#===============================================================================
# 检查并自动更新入口
#===============================================================================

check_and_update() {
    local yes_flag="${1:-}"
    if check_update; then
        echo ""
        if [[ "$yes_flag" == "-y" || "$yes_flag" == "--yes" ]]; then
            do_update
        else
            read -p "是否立即更新? (y/n): " confirm
            if [[ "$confirm" == "y" || "$confirm" == "Y" ]]; then
                do_update
            else
                print_info "已跳过更新"
            fi
        fi
    fi

    # 无论是否更新，都确保 CLI 命令存在
    ensure_cli_exists
}

#===============================================================================
# 静默自动更新 (用于 cron 定时任务)
#===============================================================================

auto_update() {
    local LOG_FILE="/var/log/b-ui-update.log"
    
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] 开始自动检查更新..." >> "$LOG_FILE"
    
    local local_ver=$(get_local_version)
    local remote_ver=$(get_remote_version)
    
    if [[ -z "$remote_ver" ]]; then
        echo "[$(date '+%Y-%m-%d %H:%M:%S')] 错误: 无法获取远程版本" >> "$LOG_FILE"
        return 1
    fi
    
    version_compare "$remote_ver" "$local_ver"
    local result=$?
    
    if [[ $result -eq 1 ]]; then
        echo "[$(date '+%Y-%m-%d %H:%M:%S')] 发现新版本: v${local_ver} -> v${remote_ver}" >> "$LOG_FILE"
        
        # 执行静默更新
        select_download_source
        
        # 下载新文件（使用正确的路径映射）
        declare -A file_map=(
            ["version.json"]="${BASE_DIR}/version.json"
            ["server/core.sh"]="${BASE_DIR}/core.sh"
            ["server/b-ui-cli.sh"]="${BASE_DIR}/b-ui-cli.sh"
            ["server/update.sh"]="${BASE_DIR}/update.sh"
            ["server/residential-helper.sh"]="${BASE_DIR}/residential-helper.sh"
            ["web/server.js"]="${ADMIN_DIR}/server.js"
            ["web/package.json"]="${ADMIN_DIR}/package.json"
            ["web/index.html"]="${ADMIN_DIR}/index.html"
            ["web/style.css"]="${ADMIN_DIR}/style.css"
            ["web/app.js"]="${ADMIN_DIR}/app.js"
            ["web/logo.jpg"]="${ADMIN_DIR}/logo.jpg"
            ["b-ui-client.sh"]="${BASE_DIR}/b-ui-client.sh"
        )
        
        for remote in "${!file_map[@]}"; do
            local local_path="${file_map[$remote]}"
            mkdir -p "$(dirname "$local_path")"
            
            if curl -fsSL "${DOWNLOAD_URL}/${remote}" -o "$local_path" 2>/dev/null; then
                chmod +x "$local_path" 2>/dev/null || true
            fi
        done
        
        # 同步到 packages 分发目录（供客户端下载）
        if [[ -d "${BASE_DIR}/packages" ]]; then
            cp -f "${BASE_DIR}/version.json" "${BASE_DIR}/packages/version.json" 2>/dev/null || true
            cp -f "${BASE_DIR}/b-ui-client.sh" "${BASE_DIR}/packages/b-ui-client.sh" 2>/dev/null || true
            echo "[$(date '+%Y-%m-%d %H:%M:%S')] packages 目录已同步" >> "$LOG_FILE"
        fi
        
        # 应用 systemd 资源隔离配置
        apply_systemd_configs 2>/dev/null || true
        
        # 安装 Web 面板依赖
        if [[ -f "${ADMIN_DIR}/package.json" ]]; then
            cd "${ADMIN_DIR}" && npm install 2>&1 && cd - > /dev/null || true
        fi
        
        # 重启服务
        systemctl restart b-ui-admin 2>/dev/null || true
        systemctl restart hysteria-server 2>/dev/null || true
        systemctl restart xray 2>/dev/null || true

        # 重新应用住宅 IP 配置（如已启用，防止升级重写 outbound 配置丢失）
        if [[ -f "${BASE_DIR}/residential-helper.sh" ]]; then
            "${BASE_DIR}/residential-helper.sh" reapply 2>/dev/null || true
        fi

        echo "[$(date '+%Y-%m-%d %H:%M:%S')] 更新完成: v${remote_ver}" >> "$LOG_FILE"
        
        # 确保定时任务配置正确
        ensure_cron_jobs 2>/dev/null || true
    else
        echo "[$(date '+%Y-%m-%d %H:%M:%S')] 已是最新版本: v${local_ver}" >> "$LOG_FILE"
    fi

    # B-UI 更新检查后，顺带检查内核更新
    auto_update_kernel
}

#===============================================================================
# 安装 TUI 工具 (gum + fzf)
#===============================================================================

install_tui_tools() {
    print_info "安装 TUI 工具 (gum + fzf)..."

    local arch
    local fzf_arch
    local tarball
    case "$(uname -m)" in
        x86_64)  arch="x86_64" ; fzf_arch="amd64" ;;
        aarch64) arch="arm64"  ; fzf_arch="arm64" ;;
        armv7l)  arch="armv7"  ; fzf_arch="armhf" ;;
        *)       print_warning "不支持的架构，跳过 TUI 工具安装"; return 0 ;;
    esac

    # --- gum ---
    if ! command -v gum &>/dev/null; then
        local gum_ver
        gum_ver=$(curl -sI "https://github.com/charmbracelet/gum/releases/latest" \
            | grep -i "^location:" | grep -oE 'v[0-9]+\.[0-9]+\.[0-9]+' | head -1)
        if [[ -z "$gum_ver" ]]; then
            print_warning "无法获取 gum 版本，跳过"
        else
            local gum_url="https://github.com/charmbracelet/gum/releases/download/${gum_ver}/gum_${gum_ver#v}_Linux_${arch}.tar.gz"
            local tmp
            tmp=$(mktemp -d) || { print_warning "无法创建临时目录，跳过"; return 0; }
            tarball="$tmp/download.tar.gz"
            if curl -fsSL "$gum_url" -o "$tarball" && tar -xz -C "$tmp" -f "$tarball" 2>/dev/null; then
                install -m 755 "$tmp/gum" /usr/local/bin/gum
                print_success "gum ${gum_ver} 已安装"
            else
                print_warning "gum 下载失败，TUI 功能将降级为传统模式"
            fi
            rm -rf "$tmp"
        fi
    else
        print_info "gum 已存在，跳过"
    fi

    # --- fzf ---
    if ! command -v fzf &>/dev/null; then
        local fzf_ver
        fzf_ver=$(curl -sI "https://github.com/junegunn/fzf/releases/latest" \
            | grep -i "^location:" | grep -oE 'v[0-9]+\.[0-9]+\.[0-9]+' | head -1)
        if [[ -z "$fzf_ver" ]]; then
            print_warning "无法获取 fzf 版本，跳过"
        else
            local fzf_url="https://github.com/junegunn/fzf/releases/download/${fzf_ver}/fzf-${fzf_ver#v}-linux_${fzf_arch}.tar.gz"
            local tmp
            tmp=$(mktemp -d) || { print_warning "无法创建临时目录，跳过"; return 0; }
            tarball="$tmp/download.tar.gz"
            if curl -fsSL "$fzf_url" -o "$tarball" && tar -xz -C "$tmp" -f "$tarball" 2>/dev/null; then
                install -m 755 "$tmp/fzf" /usr/local/bin/fzf
                print_success "fzf ${fzf_ver} 已安装"
            else
                print_warning "fzf 下载失败，节点选择将降级为数字菜单"
            fi
            rm -rf "$tmp"
        fi
    else
        print_info "fzf 已存在，跳过"
    fi
}

# 如果直接运行此脚本
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    case "${1:-}" in
        auto)
            # 静默自动更新模式 (用于 cron)
            auto_update
            ;;
        kernel)
            # 静默内核更新模式 (用于 cron)
            auto_update_kernel
            ;;
        -y|--yes)
            # 静默交互更新模式（跳过确认提示）
            check_and_update "-y"
            ;;
        *)
            # 交互模式
            # 补装缺失的 TUI 工具
            if ! command -v gum &>/dev/null || ! command -v fzf &>/dev/null; then
                install_tui_tools
            fi
            check_and_update
            ;;
    esac
fi
