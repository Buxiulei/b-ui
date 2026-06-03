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
# 下载并校验完整性后原子替换
# 用法: download_and_validate <url> <local_path>
# 校验失败/下载失败时丢弃 .tmp，不覆盖现有文件（避免半截下载把能跑的配置冲掉）
# 按扩展名做相应校验：.json 用 jq，.sh 检 shebang+bash -n，.yaml 至少有顶层 key
#===============================================================================

download_and_validate() {
    local url="$1" local_path="$2"
    local tmp="${local_path}.dl.$$"

    if ! curl -fsSL --max-time 60 "$url" -o "$tmp" 2>/dev/null; then
        rm -f "$tmp"
        return 1
    fi

    # 大小合理性：空文件直接拒绝
    if [[ ! -s "$tmp" ]]; then
        rm -f "$tmp"
        return 1
    fi

    # 按扩展名校验语法/结构
    local ext="${local_path##*.}"
    case "$ext" in
        json)
            if command -v jq &>/dev/null; then
                jq empty "$tmp" 2>/dev/null || { rm -f "$tmp"; return 1; }
            fi
            ;;
        sh)
            head -1 "$tmp" | grep -q '^#!' || { rm -f "$tmp"; return 1; }
            bash -n "$tmp" 2>/dev/null || { rm -f "$tmp"; return 1; }
            ;;
        yaml|yml)
            # YAML 没有轻量校验器，至少要有一个顶层 key（行首字母+冒号）
            grep -qE '^[a-zA-Z_][a-zA-Z0-9_-]*:' "$tmp" || { rm -f "$tmp"; return 1; }
            ;;
        # html/css/jpg 等二进制/纯文本：仅大小校验，已通过
    esac

    # 同盘 mv 是原子的；目标目录已被 mkdir -p 创建好
    mv -f "$tmp" "$local_path"
    return 0
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
# 清理孤儿 CLI 进程
#===============================================================================
# 只杀 PPID==1（systemd 收养）且 cmdline 匹配 b-ui CLI 的进程组。
# 主动跳过当前 update.sh 所在的 PGID（update.sh 是被 b-ui-cli.sh source 调用的，
# $$ 即为 b-ui-cli.sh 自身），避免把自己 / 用户当前菜单 session 一起干掉。
cleanup_orphan_cli_processes() {
    local self_pgid victims=()
    self_pgid=$(ps -o pgid= -p $$ 2>/dev/null | tr -d ' ')
    [[ -z "$self_pgid" ]] && return 0

    while read -r pid ppid pgid rest; do
        [[ -z "$pid" || "$pid" == "$$" ]] && continue
        [[ "$ppid" != "1" ]] && continue
        [[ "$pgid" == "$self_pgid" ]] && continue
        case "$rest" in
            *b-ui-cli.sh*|*"/usr/local/bin/b-ui "*|*"/usr/local/bin/b-ui") victims+=("$pgid") ;;
        esac
    done < <(ps -eo pid=,ppid=,pgid=,args=)

    [[ ${#victims[@]} -eq 0 ]] && return 0

    local unique_pgids
    unique_pgids=$(printf '%s\n' "${victims[@]}" | sort -u)
    print_warning "发现孤儿 b-ui CLI 进程组: $(echo "$unique_pgids" | tr '\n' ' ')— 清理中"

    local pgid
    for pgid in $unique_pgids; do
        kill -TERM -- "-$pgid" 2>/dev/null || true
    done
    sleep 2
    for pgid in $unique_pgids; do
        kill -KILL -- "-$pgid" 2>/dev/null || true
    done
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

    # 清理孤儿 b-ui CLI 进程（PPID==1，不动当前调用链）
    # 老 b-ui-cli.sh 实例若被 update.sh 重写文件，会 hold deleted inode 持续跑；
    # 配合一条没匹配的 grep|head 管道就能死锁卡 CPU 数天 → hysteria keepalive 超时
    cleanup_orphan_cli_processes
    
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
        "web/qrcode.min.js:${ADMIN_DIR}/qrcode.min.js"
        "web/logo.jpg:${ADMIN_DIR}/logo.jpg"
    )
    
    local failed=0
    for item in "${files[@]}"; do
        IFS=':' read -r remote local <<< "$item"
        mkdir -p "$(dirname "$local")"
        echo -n "  更新 ${remote}... "
        if download_and_validate "${DOWNLOAD_URL}/${remote}" "${local}"; then
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
    
    # v3.4.23: 智能重启——只在必要时重启 hysteria/xray，避免每次升级都让所有客户端断流
    # apply_systemd_configs 已经处理了 config 变更触发的 restart（line 1142+ 的 hysteria_config_changed 逻辑）
    # 这里只在服务挂了 / 没运行时启动；运行中的服务**保持现状**避免无谓的瞬断
    print_info "确保服务运行..."
    systemctl start b-ui-admin 2>/dev/null || true
    if ! systemctl is-active --quiet hysteria-server 2>/dev/null; then
        systemctl start hysteria-server 2>/dev/null || true
        print_info "  ↑ hysteria-server 之前未运行，已启动"
    else
        print_info "  ✓ hysteria-server 运行中（不必要重启，避免客户端瞬断）"
    fi
    if ! systemctl is-active --quiet xray 2>/dev/null; then
        systemctl start xray 2>/dev/null || true
    fi

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
# 保留 listen / tls / quic 已有段（避免覆盖端口/证书路径）
# v3.5+: hy2-direct (config.yaml) 永远内置直连，不写 outbounds/acl/RESIDENTIAL block
repair_hysteria_config() {
    local config_file="${BASE_DIR}/config.yaml"
    local users_file="${BASE_DIR}/users.json"
    [[ -f "$config_file" ]] || return 1

    # 提取已有的关键段：listen / tls
    local port
    port=$(awk '/^listen:/{sub(/^listen: *:/,""); print; exit}' "$config_file")
    [[ -z "$port" ]] && port=10000

    local cert_path key_path
    cert_path=$(awk '/^  cert:/{print $2; exit}' "$config_file")
    key_path=$(awk '/^  key:/{print $2; exit}' "$config_file")
    [[ -z "$cert_path" ]] && cert_path="${BASE_DIR}/certs/fullchain.pem"
    [[ -z "$key_path" ]]  && key_path="${BASE_DIR}/certs/privkey.pem"

    # 重新生成完整 config（HTTP 认证模式，配合 b-ui-admin 实现用户管理 + 限速）
    cat > "${config_file}.tmp" <<EOF
# Hysteria2 服务器配置 — Direct 实例 (v3.5+)
# hy2-direct 内置直连，绕开 b-ui-relay；住宅路径走 config-residential.yaml
# 修复时间: $(date)

listen: :${port}

tls:
  sniGuard: disable
  cert: ${cert_path}
  key: ${key_path}

quic:
  # maxIdleTimeout 默认 30s，源码硬上限 120s；60s 在抖动链路下兼顾稳定与连接回收速度
  maxIdleTimeout: 60s

# 强制走服务端拥塞控制（BBR/Reno），忽略客户端 Brutal 宣告
# 多用户共享 VPS 必开：防一个客户端宣告 1Gbps Brutal 抢光带宽
ignoreClientBandwidth: true

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
  # UDP 嗅探仅限 443 (QUIC/HTTP3) 和 53 (DoH/DoQ)
  udpPorts: 443,53

# 强制走服务端拥塞控制（BBR/Reno），忽略客户端 Brutal 宣告
ignoreClientBandwidth: true

# DoH 防 GFW DNS 投毒（hy2 sniff 出 host 后用 DoH 重解析）
resolver:
  type: https
  https:
    addr: "1.1.1.1:443"
    sni: cloudflare-dns.com
EOF
    mv "${config_file}.tmp" "$config_file"
    chmod 644 "$config_file"
}

# residential-proxy.json 关键词迁移（v3.4.17 → v3.4.18）
# 老默认 10 个含 "google" "googleapis" "gstatic"（粗匹配 → 全站误伤 + urltest 自循环）
# 新默认精化为 AI/敏感站点 24 个
migrate_residential_keywords() {
    local cfg="${BASE_DIR}/residential-proxy.json"
    [[ -f "$cfg" ]] || return 0
    command -v jq >/dev/null 2>&1 || return 0

    # 旧默认列表（v3.4.17 及之前，保序）
    local legacy_default
    legacy_default=$(jq -nc '["openai","chatgpt","google","googleapis","gstatic","anthropic","claude","ping0","grok","tiktok"]')

    # 新默认列表（必须与 residential-helper.sh DEFAULT_DOMAINS 完全一致）
    local new_default
    new_default=$(jq -nc '[
        "openai","chatgpt","oai","oaistatic",
        "anthropic","claude",
        "aistudio","generativelanguage","gemini.google","makersuite",
        "grok","githubcopilot","cursor","perplexity",
        "mistral","cohere","huggingface","replicate","together","groq",
        "statsig","featuregates",
        "ping0","ip.sb",
        "tiktok"
    ]')

    local current
    current=$(jq -c '.domains // null' "$cfg" 2>/dev/null || echo "null")

    # 已经是新默认 → 无需任何动作（防止重复打印）
    if [[ "$current" == "$new_default" ]]; then
        return 0
    fi

    # 自动迁移条件：用户从未自定义
    #   1) domains == null（老服务器初始化时就这样）
    #   2) domains 精确等于老默认 10 个（保序对比）
    local should_auto_replace=0
    if [[ "$current" == "null" ]] || [[ "$current" == "$legacy_default" ]]; then
        should_auto_replace=1
    fi

    if [[ "$should_auto_replace" == "1" ]]; then
        local tmp="${cfg}.tmp.$$"
        if jq --argjson nd "$new_default" '.domains = $nd' "$cfg" > "$tmp" 2>/dev/null; then
            mv "$tmp" "$cfg"
            chmod 600 "$cfg"
            print_info "  ✓ 住宅代理分流关键词已升级为 v3.4.18 默认（24 个 AI / 敏感站点关键词）"
        else
            rm -f "$tmp"
        fi
        return 0
    fi

    # 用户已自定义 —— 不覆盖（这是用户数据），仅在含老粗关键词时打印提示
    local has_stale
    has_stale=$(jq -r '
        (.domains // []) as $d |
        if (($d | index("google"))      != null) or
           (($d | index("googleapis")) != null) or
           (($d | index("gstatic"))    != null)
        then "yes" else "" end
    ' "$cfg" 2>/dev/null)
    if [[ "$has_stale" == "yes" ]]; then
        print_warning "  住宅代理含 google/googleapis/gstatic 关键词（v3.4.18 已弃用）"
        print_warning "    原因：会让整个 google 全站走住宅 + 触发 sing-box urltest 探测自循环"
        print_warning "    建议：在 b-ui 菜单的「住宅 IP 出口」中重新应用默认关键词"
        print_warning "    （当前为用户自定义列表，已保留不动）"
    fi
}

#===============================================================================
# v3.4.18 安全加固迁移（C1: admin.env / C2: SSH 99-conf）
# - 老用户从 unit Environment= 迁移到 /opt/b-ui/admin.env (chmod 600) + bind 127.0.0.1
# - 升级时检测 pubkey，若有则写 /etc/ssh/sshd_config.d/99-b-ui-hardening.conf
#===============================================================================
migrate_admin_env() {
    # 仅当 admin.env 不存在且 unit 文件存在时迁移
    [[ -f /opt/b-ui/admin.env ]] && return 0
    [[ -f /etc/systemd/system/b-ui-admin.service ]] || return 0

    local OLD_PWD OLD_PORT
    OLD_PWD=$(grep '^Environment=ADMIN_PASSWORD=' /etc/systemd/system/b-ui-admin.service 2>/dev/null \
              | sed 's/^Environment=ADMIN_PASSWORD=//')
    OLD_PORT=$(grep '^Environment=ADMIN_PORT=' /etc/systemd/system/b-ui-admin.service 2>/dev/null \
              | sed 's/^Environment=ADMIN_PORT=//')
    # 兜底默认值
    [[ -z "$OLD_PORT" ]] && OLD_PORT=8080
    [[ -z "$OLD_PWD" ]]  && OLD_PWD=admin123

    cat > /opt/b-ui/admin.env <<EOF
ADMIN_PORT=${OLD_PORT}
ADMIN_BIND=127.0.0.1
ADMIN_PASSWORD=${OLD_PWD}
HYSTERIA_CONFIG=/opt/b-ui/config.yaml
USERS_FILE=/opt/b-ui/users.json
XRAY_CONFIG=/opt/b-ui/xray-config.json
XRAY_KEYS=/opt/b-ui/reality-keys.json
EOF
    chmod 600 /opt/b-ui/admin.env

    # 重写 unit：去掉 Environment=ADMIN_*/HYSTERIA_/USERS_/XRAY_，加 EnvironmentFile=
    sed -i '/^Environment=ADMIN_/d' /etc/systemd/system/b-ui-admin.service
    sed -i '/^Environment=HYSTERIA_CONFIG/d' /etc/systemd/system/b-ui-admin.service
    sed -i '/^Environment=USERS_FILE/d' /etc/systemd/system/b-ui-admin.service
    sed -i '/^Environment=XRAY_CONFIG/d' /etc/systemd/system/b-ui-admin.service
    sed -i '/^Environment=XRAY_KEYS/d' /etc/systemd/system/b-ui-admin.service
    if ! grep -q '^EnvironmentFile' /etc/systemd/system/b-ui-admin.service; then
        sed -i '/^\[Service\]/a EnvironmentFile=-/opt/b-ui/admin.env' /etc/systemd/system/b-ui-admin.service
    fi
    systemctl daemon-reload
    systemctl restart b-ui-admin 2>/dev/null || true
    print_info "  ✓ b-ui-admin: 密码迁移到 /opt/b-ui/admin.env (chmod 600)，bind 收紧到 127.0.0.1"
}

migrate_ssh_hardening() {
    # 已加固 → 跳过
    [[ -f /etc/ssh/sshd_config.d/99-b-ui-hardening.conf ]] && return 0
    # 用户明确不加固（首装时无 pubkey）→ 跳过
    [[ -f /opt/b-ui/.ssh-not-hardened ]] && return 0

    local pubkey_count=0
    if [[ -f /root/.ssh/authorized_keys ]] && [[ -s /root/.ssh/authorized_keys ]]; then
        pubkey_count=$(grep -cE '^[^#]*\s*(ssh-(rsa|ed25519|dss)|ecdsa-sha2-)' \
                       /root/.ssh/authorized_keys 2>/dev/null || echo 0)
    fi
    pubkey_count=${pubkey_count//[^0-9]/}
    [[ -z "$pubkey_count" ]] && pubkey_count=0

    if [[ $pubkey_count -lt 1 ]]; then
        # 没有 pubkey，不加固，但留一个提示文件防止下次重复检测
        return 0
    fi

    mkdir -p /etc/ssh/sshd_config.d
    cat > /etc/ssh/sshd_config.d/99-b-ui-hardening.conf <<'EOF'
# B-UI SSH 加固（覆盖 50-cloud-init.conf）
PasswordAuthentication no
PermitRootLogin prohibit-password
KbdInteractiveAuthentication no
ChallengeResponseAuthentication no
EOF
    chmod 644 /etc/ssh/sshd_config.d/99-b-ui-hardening.conf

    if sshd -t 2>/dev/null; then
        systemctl reload sshd 2>/dev/null || systemctl reload ssh 2>/dev/null || \
            systemctl restart sshd 2>/dev/null || systemctl restart ssh 2>/dev/null
        print_info "  ✓ SSH 加固已启用（密码登录禁用，pubkey 数量: ${pubkey_count}）"
    else
        rm -f /etc/ssh/sshd_config.d/99-b-ui-hardening.conf
        print_warning "  sshd -t 校验失败，已回滚 SSH 加固"
    fi
}

migrate_relay_log() {
    # C3: 老 b-ui-relay unit 含 StandardOutput=null/StandardError=null → 让 reapply 重写
    if [[ -f /etc/systemd/system/b-ui-relay.service ]] && \
       grep -qE '^Standard(Output|Error)=null' /etc/systemd/system/b-ui-relay.service; then
        if [[ -f "${BASE_DIR}/residential-helper.sh" ]]; then
            bash "${BASE_DIR}/residential-helper.sh" reapply 2>/dev/null || true
            print_info "  ✓ b-ui-relay: 日志改走 journal（清理 StandardOutput=null）"
        fi
    fi
}

apply_systemd_configs() {
    print_info "应用 systemd 资源隔离配置..."
    local updated=0

    # v3.4.18 安全加固迁移（先于 systemd 配置，因为 b-ui-admin 会被 reapply 重启）
    migrate_admin_env
    migrate_ssh_hardening
    migrate_relay_log

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

    # 迁移 config.yaml：确保 udpPorts 存在；旧值 'all' 收窄为 '443,53'
    # 'all' 在游戏/视频会议等高频小包场景下嗅探会浪费 CPU 且容易误判
    if [[ -f "$config_file" ]] && grep -q 'tcpPorts:' "$config_file" && ! grep -q 'udpPorts:' "$config_file"; then
        sed -i '/tcpPorts:.*80,443/a\  udpPorts: 443,53' "$config_file"
        print_info "  ✓ config.yaml 添加 udpPorts: 443,53"
        hysteria_config_changed=1
    elif [[ -f "$config_file" ]] && grep -qE '^\s*udpPorts:\s*all\s*$' "$config_file"; then
        sed -i 's/^\(\s*udpPorts:\s*\)all\s*$/\1443,53/' "$config_file"
        print_info "  ✓ config.yaml: udpPorts all → 443,53（收窄嗅探范围）"
        hysteria_config_changed=1
    fi

    # 迁移 config.yaml：单端口 listen → 多端口 listen（端口跳跃，hysteria 2.9+ 内置语法）
    # 老服务器若 port-hopping.json 标记 enabled，把 listen: :10000 升级为 listen: :10000,20000-30000
    if [[ -f "$config_file" ]] && [[ -f "${BASE_DIR}/port-hopping.json" ]]; then
        local ph_enabled ph_listen ph_start ph_end
        ph_enabled=$(jq -r '.enabled // false' "${BASE_DIR}/port-hopping.json" 2>/dev/null)
        if [[ "$ph_enabled" == "true" ]]; then
            ph_listen=$(jq -r '.listenPort // 10000' "${BASE_DIR}/port-hopping.json" 2>/dev/null)
            ph_start=$(jq -r '.startPort // 20000' "${BASE_DIR}/port-hopping.json" 2>/dev/null)
            ph_end=$(jq -r '.endPort // 30000' "${BASE_DIR}/port-hopping.json" 2>/dev/null)
            # 仅当 listen 是单端口（无逗号）时才升级
            if grep -qE "^listen:\s*:[0-9]+\s*\$" "$config_file"; then
                sed -i "s|^listen:\s*:[0-9]*\s*\$|listen: :${ph_listen},${ph_start}-${ph_end}|" "$config_file"
                print_info "  ✓ config.yaml: listen 升级为多端口 :${ph_listen},${ph_start}-${ph_end}（hysteria 内置端口跳跃）"
                hysteria_config_changed=1
            fi
        fi
    fi

    # 迁移 config.yaml：添加 QUIC maxIdleTimeout（默认 30s 过短，空闲断连后客户端需约 1 分钟恢复）
    if [[ -f "$config_file" ]] && ! grep -q 'maxIdleTimeout' "$config_file"; then
        if grep -q '^quic:' "$config_file"; then
            sed -i '/^quic:/a\  maxIdleTimeout: 60s' "$config_file"
        else
            awk '/^listen:/{print; print ""; print "quic:"; print "  maxIdleTimeout: 60s"; next}1' \
                "$config_file" > "${config_file}.tmp" && mv "${config_file}.tmp" "$config_file"
        fi
        print_info "  ✓ config.yaml 添加 QUIC maxIdleTimeout: 60s（防止空闲断连）"
        hysteria_config_changed=1
    fi

    # 迁移 config.yaml：旧值 120s 调整为 60s（源码硬上限 120s，60s 兼顾稳定与连接回收）
    if [[ -f "$config_file" ]] && grep -qE '^\s*maxIdleTimeout:\s*120s' "$config_file"; then
        sed -i 's/^\(\s*maxIdleTimeout:\s*\)120s\s*$/\160s/' "$config_file"
        print_info "  ✓ config.yaml: maxIdleTimeout 120s → 60s"
        hysteria_config_changed=1
    fi

    # 迁移 config.yaml：添加 ignoreClientBandwidth（多用户共享 VPS 防 Brutal 抢带宽）
    if [[ -f "$config_file" ]] && ! grep -q '^ignoreClientBandwidth:' "$config_file"; then
        if [[ -n "$(tail -c 1 "$config_file")" ]]; then echo "" >> "$config_file"; fi
        cat >> "$config_file" <<'EOF'

# 强制走服务端拥塞控制，忽略 Brutal 宣告，防多用户带宽抢占
ignoreClientBandwidth: true
EOF
        print_info "  ✓ config.yaml 加 ignoreClientBandwidth: true"
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
        # 检测旧配置是否缺少内存优化或日志级别（用于决定是否需要 restart）
        if ! grep -q 'GOMEMLIMIT' /etc/systemd/system/hysteria-server.service.d/override.conf 2>/dev/null; then
            hysteria_mem_changed=1
        fi
        # HYSTERIA_LOG_LEVEL 是环境变量，必须 restart 才生效
        if ! grep -q 'HYSTERIA_LOG_LEVEL' /etc/systemd/system/hysteria-server.service.d/override.conf 2>/dev/null; then
            hysteria_mem_changed=1
        fi
        # 缺少新的"按实例"端口跳跃孤儿链清理 ExecStartPre → 触发一次 restart 让它生效
        # （重启时 ExecStartPre 会先清掉本实例可能残留的孤儿链/表，再启动，安全）。
        if ! grep -q 'hy2-portjump-cleanup.sh' /etc/systemd/system/hysteria-server.service.d/override.conf 2>/dev/null; then
            hysteria_mem_changed=1
        fi
        # 同步清掉 v3.5.0 临时 hotfix drop-in
        rm -f /etc/systemd/system/hysteria-server.service.d/99-no-cross-cleanup.conf
        rm -f /etc/systemd/system/hysteria-residential.service.d/99-no-cross-cleanup.conf
        rmdir /etc/systemd/system/hysteria-residential.service.d 2>/dev/null || true
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

# 日志级别：默认 info 在大流量下每秒数十条 disconnect 信息，污染 journal 且 IO 开销大
# warn 仅保留异常和错误，能显著降低 SSD 写入与日志噪音
Environment=HYSTERIA_LOG_LEVEL=warn

# cgroup 兜底：超过 500M 开始 throttle，700M 硬上限触发 OOM-restart
# 适配 1G 小内存机器；2G+ 机器可手动放宽到 800M/1G
MemoryHigh=500M
MemoryMax=700M

# 启动前清理"仅本实例"的孤儿端口跳跃 NAT 链（base 端口从 config.yaml listen 提取，支持自定义端口）
# hysteria 内置端口跳跃用 iptables/ip6tables 的 HYSTERIA-PR-<hash> 链；SIGKILL/OOM 残留时，
# 下次启动 ip6tables -N 报 "Chain already exists" → FATAL 崩溃循环（v3.5.13 实测踩坑）。
# 只清 --to-ports <base> 的链，绝不碰住宅实例（:40000）。`-` 前缀使清理失败不致命。
ExecStartPre=-/opt/b-ui/hy2-portjump-cleanup.sh ${config_file}

# 给 hy2 充足时间走完 closer chain 删自己的 NAT 链（正常 <1s）
TimeoutStopSec=15

# 确保服务稳定运行
Restart=always
RestartSec=3
EOF

        # 写入 / 更新"按实例"端口跳跃孤儿链清理 helper（幂等）
        # 旧 hy2-nft-cleanup.sh 清的是 nft hysteria_* 表，但 hysteria 实际用的是 iptables-nft
        # 的 HYSTERIA-PR-* 链，且多数机器没装 nft —— 清错对象，等于没清。这里换成按 base 端口
        # 精确清理的版本（见 core.sh 同名脚本），并删掉误导的旧脚本。
        cat > /opt/b-ui/hy2-portjump-cleanup.sh <<'CLEANUP_EOF'
#!/bin/sh
# 删除"只属于本实例"的孤儿端口跳跃 NAT 规则。按 base 端口(REDIRECT 目标) + 跳跃端口段(--dport range)
# 双重定位：既清"有 redirect 规则的完整孤儿"，也清"crash 在 -N 后、加 redirect 前残留的空链(0 内部
# 规则但有 dport 跳转)"——v3.5.15 实测崩溃循环元凶。range 按实例唯一，绝不跨实例误删。覆盖 iptables+nft。
# 参数：hysteria 配置文件路径(从 listen: :BASE,RANGE 提取)，或纯 base 端口数字(无 range 只清完整孤儿)。
arg="$1"
[ -z "$arg" ] && exit 0
case "$arg" in
    *[!0-9]*)
        base=$(awk '/^listen:/{ sub(/^listen: *:/,""); split($0,a,","); print a[1]; exit }' "$arg" 2>/dev/null)
        range=$(awk '/^listen:/{ sub(/^listen: *:/,""); split($0,a,","); print a[2]; exit }' "$arg" 2>/dev/null)
        ;;
    *)  base="$arg"; range="" ;;
esac
[ -z "$base" ] && exit 0
rangec=$(printf '%s' "$range" | tr '-' ':')
# 后端 A: iptables / ip6tables
for ipt in iptables ip6tables; do
    command -v "$ipt" >/dev/null 2>&1 || continue
    S=$("$ipt" -t nat -S 2>/dev/null)
    chains=$(printf '%s\n' "$S" | sed -n "s/^-A \\(HYSTERIA-PR-[A-Za-z0-9]*\\) .*--to-ports ${base}\$/\\1/p")
    if [ -n "$rangec" ]; then
        chains="${chains}
$(printf '%s\n' "$S" | sed -n "s/^-A .* --dport ${rangec} -j \\(HYSTERIA-PR-[A-Za-z0-9]*\\)\$/\\1/p")"
    fi
    chains=$(printf '%s\n' "$chains" | sort -u | grep -v '^$')
    [ -z "$chains" ] && continue
    for ch in $chains; do
        printf '%s\n' "$S" | grep -- "-j ${ch}\$" | while read -r line; do
            # shellcheck disable=SC2086
            "$ipt" -t nat $(printf '%s' "$line" | sed 's/^-A /-D /') 2>/dev/null || true
        done
        "$ipt" -t nat -F "$ch" 2>/dev/null || true
        "$ipt" -t nat -X "$ch" 2>/dev/null || true
    done
done
# 后端 B: nft —— 删 REDIRECT 到本 base 端口的 hysteria_<hash> 表（仅本实例）
if command -v nft >/dev/null 2>&1; then
    nft list tables 2>/dev/null | awk '$3 ~ /^hysteria_/{print $2, $3}' | while read -r fam tbl; do
        if nft list table "$fam" "$tbl" 2>/dev/null | grep -qE "redirect to :${base}([^0-9]|\$)"; then
            nft delete table "$fam" "$tbl" 2>/dev/null || true
        fi
    done
fi
exit 0
CLEANUP_EOF
        chmod 755 /opt/b-ui/hy2-portjump-cleanup.sh
        rm -f /opt/b-ui/hy2-nft-cleanup.sh

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

    # UDP 缓冲区优化（老用户补丁）：与 core.sh 新装模板保持一致
    # 缺失时会导致 Hysteria2 QUIC 掉包、客户端报 "no recent network activity"
    if [[ ! -f /etc/sysctl.d/99-hysteria-perf.conf ]]; then
        cat > /etc/sysctl.d/99-hysteria-perf.conf <<'SYSCTL_EOF'
# Hysteria2 性能优化 - UDP 缓冲区
net.core.rmem_max=16777216
net.core.wmem_max=16777216
net.core.rmem_default=1048576
net.core.wmem_default=1048576
SYSCTL_EOF
        sysctl -p /etc/sysctl.d/99-hysteria-perf.conf >/dev/null 2>&1 && \
            print_success "  ✓ 应用 UDP 缓冲区优化 (16MB)"
    fi

    # v3.4.19 D1: 网络栈调优（缺失时写入；老用户补丁）
    if [[ ! -f /etc/sysctl.d/99-b-ui-network.conf ]]; then
        cat > /etc/sysctl.d/99-b-ui-network.conf <<'SYSCTL_EOF'
# B-UI 网络栈调优 v3.4.19
net.ipv4.tcp_retries2=8
net.ipv4.tcp_mtu_probing=1
net.ipv4.tcp_keepalive_time=600
net.ipv4.tcp_keepalive_intvl=30
net.ipv4.tcp_keepalive_probes=3
net.ipv4.tcp_no_metrics_save=1
net.ipv4.tcp_slow_start_after_idle=0
net.ipv4.tcp_notsent_lowat=131072
net.ipv4.tcp_rmem=4096 262144 16777216
net.ipv4.tcp_wmem=4096 65536 16777216
net.ipv4.udp_mem=262144 524288 1048576
net.ipv4.ip_local_port_range=10000 65535
net.ipv4.ip_local_reserved_ports=10000-10002,20000-30000,40000,41000-50000
net.core.netdev_max_backlog=5000
net.ipv4.tcp_max_syn_backlog=8192
SYSCTL_EOF
        sysctl -p /etc/sysctl.d/99-b-ui-network.conf >/dev/null 2>&1 && \
            print_success "  ✓ 应用网络栈调优 (99-b-ui-network.conf)"
    fi

    # v3.5.14 D3: 已有 99-b-ui-network.conf 但缺端口保留 → 补 reserved_ports
    # 防止出向连接抢占端口跳跃段（20000-30000/41000-50000）造成间歇 UDP 出向失败
    if [[ -f /etc/sysctl.d/99-b-ui-network.conf ]] && \
       ! grep -q 'ip_local_reserved_ports' /etc/sysctl.d/99-b-ui-network.conf; then
        echo 'net.ipv4.ip_local_reserved_ports=10000-10002,20000-30000,40000,41000-50000' \
            >> /etc/sysctl.d/99-b-ui-network.conf
        sysctl -p /etc/sysctl.d/99-b-ui-network.conf >/dev/null 2>&1 && \
            print_success "  ✓ 补充端口保留 (ip_local_reserved_ports)"
        updated=1
    fi

    # v3.5.14 D4: conntrack 容量（默认 8192 是多用户真正天花板，hy2 端口跳跃消耗极快）
    if [[ ! -f /etc/sysctl.d/99-b-ui-conntrack.conf ]]; then
        local ct_mem_mb ct_max
        ct_mem_mb=$(awk '/^MemTotal:/ {print int($2/1024)}' /proc/meminfo)
        ct_max=131072
        if   [[ $ct_mem_mb -gt 4096 ]]; then ct_max=524288
        elif [[ $ct_mem_mb -gt 2048 ]]; then ct_max=262144
        fi
        cat > /etc/sysctl.d/99-b-ui-conntrack.conf <<EOF
# B-UI conntrack 容量（多用户 hy2 端口跳跃 + xray + 住宅 relay 共享一张表）
net.netfilter.nf_conntrack_max=${ct_max}
net.netfilter.nf_conntrack_udp_timeout=20
net.netfilter.nf_conntrack_udp_timeout_stream=60
EOF
        echo "options nf_conntrack hashsize=$((ct_max / 4))" > /etc/modprobe.d/b-ui-nf_conntrack.conf
        echo "nf_conntrack" > /etc/modules-load.d/b-ui-conntrack.conf
        modprobe nf_conntrack 2>/dev/null || true
        sysctl --system >/dev/null 2>&1
        print_success "  ✓ 上调 conntrack 容量 (nf_conntrack_max=${ct_max})"
        updated=1
    fi

    # v3.5.14 D5: b-ui-admin 内存上限（防 Node 泄漏触发 OOM-killer 误杀代理进程）
    # guard 必须判断真正写入的 drop-in（之前误判主 unit 文件 → 每 6h 重启一次 admin 的 thrash bug）
    if [[ -f /etc/systemd/system/b-ui-admin.service ]] && \
       [[ ! -f /etc/systemd/system/b-ui-admin.service.d/10-memory.conf ]] && \
       ! grep -q 'MemoryMax' /etc/systemd/system/b-ui-admin.service; then
        mkdir -p /etc/systemd/system/b-ui-admin.service.d
        cat > /etc/systemd/system/b-ui-admin.service.d/10-memory.conf <<'EOF'
[Service]
MemoryHigh=150M
MemoryMax=200M
EOF
        systemctl daemon-reload
        systemctl restart b-ui-admin 2>/dev/null || true
        print_success "  ✓ b-ui-admin 内存上限已设置 (MemoryMax=200M)"
        updated=1
    fi

    # v3.5.15 D6: hy2 auth http→userpass（高并发硬化）
    # http auth 每条连接回调 b-ui-admin /auth/hysteria，几十客户端共享订阅 + 重连风暴时
    # 单线程面板成 SPOF；userpass 本地鉴权无依赖。v3.4→v3.5 迁移机的 residential 实例
    # 常卡在 http（加 unit 后没经过面板存用户）。读 users.json 就地转 userpass，幂等。
    if [[ -f "${BASE_DIR}/users.json" ]] && command -v jq >/dev/null 2>&1; then
        local _upf; _upf=$(mktemp)
        jq -r '.[] | "    \(.username): \(.password)"' "${BASE_DIR}/users.json" 2>/dev/null > "$_upf"
        if [[ -s "$_upf" ]]; then
            local _pair _cfg _svc
            for _pair in "config.yaml:hysteria-server" "config-residential.yaml:hysteria-residential"; do
                _cfg="${BASE_DIR}/${_pair%%:*}"; _svc="${_pair##*:}"
                [[ -f "$_cfg" ]] || continue
                grep -q '^  type: http' "$_cfg" || continue
                awk -v upfile="$_upf" '
                    /^auth:/ {print "auth:"; print "  type: userpass"; print "  userpass:"; while ((getline line < upfile) > 0) print line; close(upfile); skip=1; next}
                    skip && /^[a-zA-Z]/ {skip=0}
                    !skip {print}
                ' "$_cfg" > "${_cfg}.tmp" && mv "${_cfg}.tmp" "$_cfg"
                chmod 644 "$_cfg"
                systemctl is-active --quiet "$_svc" 2>/dev/null && systemctl restart "$_svc" 2>/dev/null || true
                print_success "  ✓ ${_svc} auth http→userpass（本地鉴权，高并发更稳）"
                updated=1
            done
        fi
        rm -f "$_upf"
    fi

    # v3.5.17 D7: 补下被 index.html 引用但缺失的 web 静态文件（自愈）
    # 新增静态文件(如 qrcode.min.js)有 chicken-and-egg：老 update.sh 的 file_map 没有它，
    # 同版本不会重下 → 文件永远缺。这里每次 apply 都补下 index 引用却不存在的资源。
    if [[ -f "${ADMIN_DIR}/index.html" ]]; then
        local _asset _src
        select_download_source 2>/dev/null || true
        for _asset in qrcode.min.js; do
            if grep -q "$_asset" "${ADMIN_DIR}/index.html" && [[ ! -s "${ADMIN_DIR}/${_asset}" ]]; then
                _src="${DOWNLOAD_URL:-$GITHUB_RAW}/web/${_asset}"
                if download_and_validate "$_src" "${ADMIN_DIR}/${_asset}"; then
                    print_success "  ✓ 补下缺失的 web 资源 ${_asset}"
                    systemctl restart b-ui-admin 2>/dev/null || true
                    updated=1
                fi
            fi
        done
    fi

    # v3.4.19 D2: BBRv3 自动升级
    # 已开 bbr 但系统支持更优的 bbr3/bbrv3/bbr_v3 → 升级
    if [[ -f /etc/sysctl.d/99-hysteria-bbr.conf ]]; then
        local current_algo available_algos best_algo=""
        current_algo=$(grep -oE 'tcp_congestion_control=\S+' /etc/sysctl.d/99-hysteria-bbr.conf 2>/dev/null | cut -d= -f2)
        available_algos=$(cat /proc/sys/net/ipv4/tcp_available_congestion_control 2>/dev/null || echo "")
        for algo in bbr3 bbrv3 bbr_v3; do
            if [[ " $available_algos " == *" $algo "* ]]; then
                best_algo="$algo"
                break
            fi
        done
        if [[ -n "$best_algo" && "$current_algo" != "$best_algo" ]]; then
            cat > /etc/sysctl.d/99-hysteria-bbr.conf <<EOF
net.core.default_qdisc=fq
net.ipv4.tcp_congestion_control=${best_algo}
EOF
            sysctl --system >/dev/null 2>&1
            print_success "  ✓ BBR 升级：${current_algo:-bbr} → ${best_algo}"
        fi
    fi

    # v3.4.19 I: reality-keys.json 备份（防 b-ui 重装丢密钥）
    if [[ -f /opt/b-ui/reality-keys.json ]]; then
        mkdir -p /root/.b-ui-backup /var/backups/b-ui 2>/dev/null
        # 仅在差异时同步
        if ! cmp -s /opt/b-ui/reality-keys.json /root/.b-ui-backup/reality-keys.json 2>/dev/null; then
            cp /opt/b-ui/reality-keys.json /root/.b-ui-backup/reality-keys.json 2>/dev/null && \
                chmod 600 /root/.b-ui-backup/reality-keys.json 2>/dev/null
        fi
        if ! cmp -s /opt/b-ui/reality-keys.json /var/backups/b-ui/reality-keys.json 2>/dev/null; then
            cp /opt/b-ui/reality-keys.json /var/backups/b-ui/reality-keys.json 2>/dev/null && \
                chmod 600 /var/backups/b-ui/reality-keys.json 2>/dev/null
        fi
    fi

    # v3.4.19 G1: 时钟同步（缺失时安装 systemd-timesyncd）
    if ! systemctl is-active --quiet systemd-timesyncd 2>/dev/null && \
       ! systemctl is-active --quiet chrony 2>/dev/null && \
       ! systemctl is-active --quiet ntp 2>/dev/null; then
        if command -v apt-get &>/dev/null; then
            apt-get install -y systemd-timesyncd 2>/dev/null || true
            systemctl enable --now systemd-timesyncd 2>/dev/null || true
            if systemctl is-active --quiet systemd-timesyncd 2>/dev/null; then
                print_info "  ✓ 启用 systemd-timesyncd（时钟同步影响 TLS/ACME）"
            fi
        fi
    fi

    # v3.4.19 G2: fail2ban（SSH 暴力扫日志噪音）
    if ! command -v fail2ban-client &>/dev/null && command -v apt-get &>/dev/null; then
        apt-get install -y fail2ban 2>/dev/null || true
    fi
    if command -v fail2ban-client &>/dev/null && [[ ! -f /etc/fail2ban/jail.d/b-ui-sshd.local ]]; then
        mkdir -p /etc/fail2ban/jail.d
        cat > /etc/fail2ban/jail.d/b-ui-sshd.local <<'EOF'
[sshd]
enabled = true
port = ssh
maxretry = 5
bantime = 1h
findtime = 10m
EOF
        systemctl enable --now fail2ban 2>/dev/null || true
        if systemctl is-active --quiet fail2ban 2>/dev/null; then
            print_info "  ✓ fail2ban 已启用（SSH 暴力扫自动封禁 1h）"
        fi
    fi

    # v3.4.19 G3: journald 限额
    if [[ ! -f /etc/systemd/journald.conf.d/99-b-ui.conf ]]; then
        mkdir -p /etc/systemd/journald.conf.d
        cat > /etc/systemd/journald.conf.d/99-b-ui.conf <<'EOF'
[Journal]
SystemMaxUse=500M
SystemKeepFree=1G
MaxRetentionSec=14day
Compress=yes
EOF
        systemctl restart systemd-journald 2>/dev/null || true
        print_info "  ✓ journald 限额已应用 (500M/14day)"
    fi

    # 应用 Xray 服务配置
    # 重命名 override.conf → 99-b-ui-override.conf 确保字典序最大，覆盖发行版/upstream drop-in
    if [[ -d /etc/systemd/system/xray.service.d ]]; then
        # 老用户迁移：清理旧文件名
        if [[ -f /etc/systemd/system/xray.service.d/override.conf ]]; then
            rm -f /etc/systemd/system/xray.service.d/override.conf
            print_info "  ✓ xray drop-in 重命名为 99-b-ui-override.conf（确保字典序最大）"
        fi
        cat > /etc/systemd/system/xray.service.d/99-b-ui-override.conf << EOF
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
    
    # 端口跳跃迁移：hysteria 2.9+ 内置 listen 多端口语法接管 iptables REDIRECT
    # 1) 清理 b-ui 历史遗留的 iptables Hysteria2-PortHopping 规则（让 hysteria 接管）
    # 2) UFW/firewalld 放行端口范围
    if [[ -f "${BASE_DIR}/port-hopping.json" ]]; then
        local enabled
        enabled=$(jq -r '.enabled // false' "${BASE_DIR}/port-hopping.json" 2>/dev/null)
        if [[ "$enabled" == "true" ]]; then
            local start_port end_port
            start_port=$(jq -r '.startPort // 20000' "${BASE_DIR}/port-hopping.json")
            end_port=$(jq -r '.endPort // 30000' "${BASE_DIR}/port-hopping.json")

            # 安全清理 iptables 旧规则（保留其它规则，仅删 Hysteria2-PortHopping 注释）
            if command -v iptables-save &>/dev/null && \
               iptables -t nat -L PREROUTING -n 2>/dev/null | grep -q "Hysteria2-PortHopping"; then
                iptables-save 2>/dev/null | grep -v "Hysteria2-PortHopping" | iptables-restore 2>/dev/null || true
                if command -v ip6tables-save &>/dev/null; then
                    ip6tables-save 2>/dev/null | grep -v "Hysteria2-PortHopping" | ip6tables-restore 2>/dev/null || true
                fi
                if [[ -d /etc/iptables ]]; then
                    iptables-save > /etc/iptables/rules.v4 2>/dev/null || true
                    ip6tables-save > /etc/iptables/rules.v6 2>/dev/null || true
                fi
                print_info "  ✓ 清理旧 iptables Hysteria2-PortHopping 规则（hysteria 内置已接管）"
            fi

            # UFW 放行
            if command -v ufw &>/dev/null && ufw status 2>/dev/null | grep -q "active"; then
                if ! ufw status 2>/dev/null | grep -qE "${start_port}:${end_port}/udp"; then
                    ufw allow ${start_port}:${end_port}/udp comment "Hysteria2 端口跳跃" 2>/dev/null || true
                    print_info "  ✓ UFW 放行 udp ${start_port}:${end_port}"
                fi
            fi

            # firewalld 放行
            if command -v firewall-cmd &>/dev/null && systemctl is-active --quiet firewalld 2>/dev/null; then
                firewall-cmd --permanent --add-port=${start_port}-${end_port}/udp 2>/dev/null || true
                firewall-cmd --reload 2>/dev/null || true
            fi
        fi
    fi

    # Hysteria2 watchdog 迁移：老服务器没有 hy2-watchdog.timer 则部署
    if [[ ! -f /etc/systemd/system/hy2-watchdog.timer ]]; then
        local watchdog_script="${BASE_DIR}/hy2-watchdog.sh"
        local listen_port_w
        listen_port_w=$(awk '/^listen:/{ sub(/^listen: *:/,""); split($0,a,","); print a[1]; exit }' "$config_file" 2>/dev/null)
        [[ -z "$listen_port_w" ]] && listen_port_w=10000

        cat > "$watchdog_script" << WDOGEOF
#!/bin/bash
# Hysteria2 半死自愈：进程在但 UDP 端口失活时强制重启（direct + residential 双实例）
LOG=/var/log/b-ui-hy2-watchdog.log
log() { echo "[\$(date '+%Y-%m-%d %H:%M:%S')] \$1" >> "\$LOG"; }

check_instance() {
    svc="\$1"; port="\$2"
    fail_file="/tmp/hy2-watchdog-fail-\${svc}"
    if ! systemctl is-active --quiet "\$svc"; then
        rm -f "\$fail_file"; return 0
    fi
    if ! ss -lun "( sport = :\${port} )" 2>/dev/null | grep -q ":\${port}"; then
        fail=\$(( \$(cat "\$fail_file" 2>/dev/null || echo 0) + 1 ))
        echo "\$fail" > "\$fail_file"
        log "WARN: \${svc} UDP :\${port} 监听失活，失败计数 \${fail}/3"
        if [ "\$fail" -ge 3 ]; then
            log "ACTION: \${svc} 连续 3 次失败，重启"
            systemctl restart "\$svc"
            rm -f "\$fail_file"
        fi
        return 0
    fi
    rm -f "\$fail_file"
}

check_instance hysteria-server ${listen_port_w}
systemctl list-unit-files hysteria-residential.service >/dev/null 2>&1 && check_instance hysteria-residential 40000

tail -200 "\$LOG" > "\${LOG}.tmp" 2>/dev/null && mv "\${LOG}.tmp" "\$LOG" 2>/dev/null
exit 0
WDOGEOF
        chmod 755 "$watchdog_script"

        cat > /etc/systemd/system/hy2-watchdog.service << EOF
[Unit]
Description=Hysteria2 Watchdog (semi-dead self-heal)
After=hysteria-server.service

[Service]
Type=oneshot
ExecStart=${watchdog_script}
EOF

        cat > /etc/systemd/system/hy2-watchdog.timer << 'EOF'
[Unit]
Description=Hysteria2 Watchdog Timer

[Timer]
OnBootSec=2min
OnUnitActiveSec=5min
AccuracySec=30s
Persistent=true

[Install]
WantedBy=timers.target
EOF

        systemctl daemon-reload
        systemctl enable hy2-watchdog.timer 2>/dev/null
        systemctl start hy2-watchdog.timer 2>/dev/null
        print_info "  ✓ 部署 Hysteria2 watchdog（每 5 min 检测，连续 3 次失败重启）"
        updated=1
    fi

    # 迁移 residential-proxy.json 关键词（v3.4.18）
    # 老默认 10 个含 google/googleapis/gstatic（全站误伤 + urltest 自循环风险）
    # 新默认 24 个 AI/敏感站点精准 keyword
    # 策略：
    #   - 用户从未自定义（domains == null 或精确等于老默认）→ 自动迁移到新默认
    #   - 用户已自定义 → 保留原样，仅打印提示让用户决定（这是用户数据，不强制覆盖）
    migrate_residential_keywords

    # 重新部署 cert-sync.sh + service：修复 race condition（exit 1 → exit 0 + 60s 轮询 + ExecStartPre 等 caddy）
    # 仅当老服务器已有 cert-sync 部署时才覆盖（新装由 core.sh 的 setup_cert_sync 处理）
    if [[ -f /opt/b-ui/cert-sync.sh ]] || [[ -f /etc/systemd/system/b-ui-cert-sync.service ]]; then
        cat > /opt/b-ui/cert-sync.sh << 'SYNCEOF'
#!/bin/bash
# Caddy 证书同步脚本
# 从 Caddy 数据目录复制证书到共享目录供 Hysteria2 使用

CERTS_DIR="/opt/b-ui/certs"
CADDY_DATA="/var/lib/caddy/.local/share/caddy"
DOMAIN_FILE="/opt/b-ui/certs/.domain"
LOG_FILE="/var/log/b-ui-cert-sync.log"

log() {
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] $1" >> "$LOG_FILE"
}

# 读取域名
if [[ ! -f "$DOMAIN_FILE" ]]; then
    log "ERROR: 域名配置文件不存在: $DOMAIN_FILE"
    exit 1
fi
DOMAIN=$(cat "$DOMAIN_FILE")

# 轮询 60s 等 Caddy ACME 流程完成（OnBootSec=30s 触发时 Caddy 可能还在签发）
# 找不到/不完整时 exit 0 而不是 exit 1，避免 systemd 误报失败；下次 timer 会再触发
CERT_SOURCE=""
for i in $(seq 1 30); do
    CERT_SOURCE="${CADDY_DATA}/certificates/acme-v02.api.letsencrypt.org-directory/${DOMAIN}"
    [[ -d "$CERT_SOURCE" ]] || CERT_SOURCE=$(find "$CADDY_DATA/certificates" -type d -name "$DOMAIN" 2>/dev/null | head -1)
    if [[ -n "$CERT_SOURCE" && -d "$CERT_SOURCE" \
          && -f "${CERT_SOURCE}/${DOMAIN}.crt" \
          && -f "${CERT_SOURCE}/${DOMAIN}.key" ]]; then
        break
    fi
    CERT_SOURCE=""
    sleep 2
done
if [[ -z "$CERT_SOURCE" ]]; then
    log "INFO: Caddy 证书暂未就绪 (域名: $DOMAIN)，等待下次 timer/cron 触发"
    exit 0
fi

# 证书和密钥文件路径
CERT_FILE="${CERT_SOURCE}/${DOMAIN}.crt"
KEY_FILE="${CERT_SOURCE}/${DOMAIN}.key"

# 比较文件是否有变化
mkdir -p "$CERTS_DIR"
if [[ -f "${CERTS_DIR}/fullchain.pem" ]] && cmp -s "$CERT_FILE" "${CERTS_DIR}/fullchain.pem"; then
    # 证书未变化，无需同步
    exit 0
fi

# 同步证书
cp "$CERT_FILE" "${CERTS_DIR}/fullchain.pem"
cp "$KEY_FILE" "${CERTS_DIR}/privkey.pem"
chmod 644 "${CERTS_DIR}/fullchain.pem"
chmod 600 "${CERTS_DIR}/privkey.pem"

log "SUCCESS: 证书已同步 (${DOMAIN})"

# 证书变了 → 重启所有在跑的 hysteria 实例（hysteria 无热重载，CanReload=no；
# direct + residential 共用同一份 /opt/b-ui/certs，都要 restart 才能加载新证书）
for u in hysteria-server hysteria-residential; do
    if systemctl is-active --quiet "$u" 2>/dev/null; then
        systemctl restart "$u" 2>/dev/null && log "$u 已重启以加载新证书"
    fi
done

# 保留最近 200 行日志
tail -200 "$LOG_FILE" > "${LOG_FILE}.tmp" && mv "${LOG_FILE}.tmp" "$LOG_FILE" 2>/dev/null
SYNCEOF
        chmod +x /opt/b-ui/cert-sync.sh

        cat > /etc/systemd/system/b-ui-cert-sync.service << 'EOF'
[Unit]
Description=B-UI Certificate Sync (Caddy -> Hysteria2)
After=caddy.service

[Service]
Type=oneshot
# 等 Caddy 完全就绪：进程 active + 证书目录已生成（最多 30s）
ExecStartPre=/bin/bash -c 'until systemctl is-active --quiet caddy; do sleep 1; done; sleep 3'
ExecStart=/opt/b-ui/cert-sync.sh
EOF
        updated=1
        print_info "  ✓ cert-sync 修复 race condition (exit 0 + ExecStartPre 等 caddy)"
    fi

    # ─────────────────────────────────────────────────────────────────────────
    # v3.5 迁移块（幂等，每块独立 guard，老 v3.4.x 自动补齐新架构）
    # ─────────────────────────────────────────────────────────────────────────

    # A. hysteria-residential systemd unit + config-residential.yaml
    if [[ -f "${BASE_DIR}/config.yaml" ]] && \
       [[ ! -f /etc/systemd/system/hysteria-residential.service ]]; then
        print_info "v3.5 迁移: 安装 hysteria-residential 实例 (:40000+41000-50000)"
        local _cert _key _masq
        _cert=$(grep -E '^\s+cert:\s' "${BASE_DIR}/config.yaml" | awk '{print $2}' | head -1)
        _key=$(grep -E '^\s+key:\s' "${BASE_DIR}/config.yaml" | awk '{print $2}' | head -1)
        _masq=$(grep -A2 'type: proxy' "${BASE_DIR}/config.yaml" | grep 'url:' | awk '{print $2}' | head -1)
        [[ -z "$_cert" ]] && _cert="/opt/b-ui/certs/fullchain.pem"
        [[ -z "$_key"  ]] && _key="/opt/b-ui/certs/privkey.pem"
        [[ -z "$_masq" ]] && _masq="https://www.bing.com/"

        cat > /etc/systemd/system/hysteria-residential.service <<'RESI_UNIT_EOF'
[Unit]
Description=Hysteria Server (Residential) Service
Documentation=https://v2.hysteria.network/
After=network.target network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/hysteria server --config /opt/b-ui/config-residential.yaml
WorkingDirectory=/etc/hysteria
User=root
Group=root
LimitNPROC=512
LimitNOFILE=1048576
CPUSchedulingPolicy=other
Nice=-5
Environment=GOMEMLIMIT=200MiB
Environment=HYSTERIA_LOG_LEVEL=warn
MemoryHigh=300M
MemoryMax=500M
# 启动前清理"仅本实例"的孤儿端口跳跃 NAT 链（从 config-residential.yaml 提取 base=40000+range）
ExecStartPre=-/opt/b-ui/hy2-portjump-cleanup.sh /opt/b-ui/config-residential.yaml
TimeoutStopSec=15
Restart=always
RestartSec=3

[Install]
WantedBy=multi-user.target
RESI_UNIT_EOF

        cat > "${BASE_DIR}/config-residential.yaml" <<EOF
# Hysteria2 服务器配置 — Residential 实例 (v3.5 迁移)
# 生成时间: $(date)
listen: :40000,41000-50000
tls:
  sniGuard: disable
  cert: ${_cert}
  key: ${_key}
quic:
  maxIdleTimeout: 60s
ignoreClientBandwidth: true
resolver:
  type: https
  https:
    addr: "1.1.1.1:443"
    sni: cloudflare-dns.com
auth:
  type: http
  http:
    url: http://127.0.0.1:8080/auth/hysteria
    insecure: false
trafficStats:
  listen: 127.0.0.1:9998
  secret: ""
masquerade:
  type: proxy
  proxy:
    url: ${_masq}
    rewriteHost: true
sniff:
  enable: true
  timeout: 2s
  rewriteDomain: true
  tcpPorts: 80,443,8000-9000
  udpPorts: 443,53
outbounds:
  - name: relay
    type: socks5
    socks5:
      addr: "127.0.0.1:2080"
  - name: direct
    type: direct
acl:
  inline:
    - relay(all)
EOF
        chmod 644 "${BASE_DIR}/config-residential.yaml"
        systemctl daemon-reload
        systemctl enable hysteria-residential 2>/dev/null || true
        systemctl start hysteria-residential 2>/dev/null || \
            print_warning "  hy2-residential 启动失败，请检查: journalctl -u hysteria-residential"
        print_info "  ✓ hysteria-residential unit + config-residential.yaml 已部署"
        updated=1
    fi

    # A.fix (v3.5.14): 老 hysteria-residential.service 自愈（unit 存在 guard 让它不被 A 块重写）
    #   1) 删除清错对象的老 nft cleanup ExecStartPre 行
    #   2) 注入"按实例"端口跳跃孤儿链清理 ExecStartPre（base=40000），缺失才加
    #   3) 确保开机自启 —— 核心 reboot-survival bug：core.sh 从未 enable 过 residential，
    #      老机器重启后住宅 HY2 节点不恢复
    if [[ -f /etc/systemd/system/hysteria-residential.service ]]; then
        local _resi_changed=0
        # 期望的 ExecStartPre（config 路径，脚本从中提取 base+range，能清空链孤儿）。
        # 若当前不是这一行（老 nft 脚本 / 旧 40000 数字参数 / 缺失），统一删旧再注入。
        if ! grep -qF 'ExecStartPre=-/opt/b-ui/hy2-portjump-cleanup.sh /opt/b-ui/config-residential.yaml' /etc/systemd/system/hysteria-residential.service; then
            sed -i '/ExecStartPre=-\/opt\/b-ui\/hy2-.*cleanup\.sh/d' /etc/systemd/system/hysteria-residential.service
            sed -i '/^ExecStart=\/usr\/local\/bin\/hysteria/a ExecStartPre=-/opt/b-ui/hy2-portjump-cleanup.sh /opt/b-ui/config-residential.yaml' /etc/systemd/system/hysteria-residential.service
            _resi_changed=1
        fi
        if [[ $_resi_changed -eq 1 ]]; then
            systemctl daemon-reload
            systemctl restart hysteria-residential 2>/dev/null || true
            print_info "  ✓ hysteria-residential ExecStartPre 已更新为按实例端口跳跃清理"
            updated=1
        fi
        if [[ "$(systemctl is-enabled hysteria-residential 2>/dev/null)" != "enabled" ]]; then
            systemctl enable hysteria-residential 2>/dev/null || true
            print_info "  ✓ hysteria-residential 已设为开机自启（修复重启后住宅节点不恢复）"
            updated=1
        fi
    fi

    # A.fix2 (v3.5.7): 清理 config.yaml (hy2-direct) 残留 RESIDENTIAL marker block
    # v3.4 时代 residential-helper apply_hysteria 在 config.yaml 插 socks5:2080 + relay(all) acl，
    # v3.5 升级路径 repair_hysteria_config 又把这块保留下来 → hy2-direct 实际走 b-ui-relay → 全局住宅模式下「直连」节点也变住宅出口
    # v3.5.7: hy2-direct 永远内置直连，主动剥离所有 RESIDENTIAL marker block
    if [[ -f "${BASE_DIR}/config.yaml" ]] && \
       grep -q '# B-UI:RESIDENTIAL-START' "${BASE_DIR}/config.yaml"; then
        print_info "v3.5.7 fix: 清理 config.yaml 残留 RESIDENTIAL marker block (hy2-direct 应内置直连)"
        cp "${BASE_DIR}/config.yaml" "${BASE_DIR}/config.yaml.bak.v357.$(date +%s)"
        awk '/# B-UI:RESIDENTIAL-START/{flag=1;next} /# B-UI:RESIDENTIAL-END/{flag=0;next} !flag' \
            "${BASE_DIR}/config.yaml" > "${BASE_DIR}/config.yaml.v357.tmp"
        mv "${BASE_DIR}/config.yaml.v357.tmp" "${BASE_DIR}/config.yaml"
        chmod 644 "${BASE_DIR}/config.yaml"
        systemctl restart hysteria-server 2>/dev/null || true
        print_info "  ✓ config.yaml 已剥离 outbounds/acl，hysteria-server 重启完成"
        updated=1
    fi

    # B. xray 双 inbound 迁移
    if [[ -f "${BASE_DIR}/xray-config.json" ]] && \
       ! jq -e '.inbounds[] | select(.tag == "vless-residential")' "${BASE_DIR}/xray-config.json" &>/dev/null; then
        print_info "v3.5 迁移: 升级 xray 到双 inbound (vless-direct + vless-residential)"
        local _xray_bak="${BASE_DIR}/xray-config.json.bak.$(date +%s)"
        cp "${BASE_DIR}/xray-config.json" "$_xray_bak"
        jq '
          .inbounds = (.inbounds | map(if .tag == "vless-reality" then .tag = "vless-direct" else . end))
          | .inbounds += [(.inbounds[] | select(.tag == "vless-direct") | .tag = "vless-residential" | .port = 10002)]
          | .dns = (.dns // {"servers": ["https+local://1.1.1.1/dns-query", "8.8.8.8"], "queryStrategy": "UseIPv4"})
          | .outbounds = ([.outbounds[] | select(.tag != "relay")] + [{"tag": "relay", "protocol": "socks", "settings": {"servers": [{"address": "127.0.0.1", "port": 2080}]}}])
          | .routing.rules = ([.routing.rules[]? | select(
              (.inboundTag // []) as $t |
              ($t | index("vless-reality")) == null and
              ($t | index("vless-direct")) == null and
              ($t | index("vless-residential")) == null and
              # v3.5.9: 过滤 v3.4 全局无条件 relay 规则 (无 inboundTag 限定，会覆盖 vless-direct → direct)
              (.outboundTag != "relay" or ($t | length) > 0)
            )] + [
              {"type": "field", "inboundTag": ["vless-direct"], "outboundTag": "direct"},
              {"type": "field", "inboundTag": ["vless-residential"], "outboundTag": "relay"}
            ])
        ' "${BASE_DIR}/xray-config.json" > "${BASE_DIR}/xray-config.json.tmp" \
        && mv "${BASE_DIR}/xray-config.json.tmp" "${BASE_DIR}/xray-config.json" \
        || { print_warning "  xray jq 转换失败，已还原备份"
             cp "$_xray_bak" "${BASE_DIR}/xray-config.json" 2>/dev/null || true; }
        systemctl restart xray 2>/dev/null || true
        print_info "  ✓ xray-config.json: 双 inbound + dns + relay outbound + routing"
        updated=1
    fi

    # B.fix (v3.5.9): xray routing 残留 v3.4 全局无条件 relay 规则
    # 老 v3.5 升级路径的 B 块 select 没过滤 inboundTag 为空的规则 →
    # {"outboundTag":"relay","network":"tcp,udp"} 被保留 → 永远命中，覆盖 vless-direct → direct →
    # Reality 直连 节点流量被强制走 b-ui-relay → 出口拿到住宅 IP（应为机房 IP）
    if [[ -f "${BASE_DIR}/xray-config.json" ]] && \
       jq -e '.routing.rules[] | select(.outboundTag == "relay" and ((.inboundTag // []) | length == 0))' \
           "${BASE_DIR}/xray-config.json" &>/dev/null; then
        print_info "v3.5.9 fix: 清除 xray routing 残留无条件 relay 规则 (v3.4 → v3.5 升级遗留)"
        cp "${BASE_DIR}/xray-config.json" "${BASE_DIR}/xray-config.json.bak.v359.$(date +%s)"
        jq '.routing.rules = [.routing.rules[] | select(
                .outboundTag != "relay" or ((.inboundTag // []) | length > 0)
            )]' \
            "${BASE_DIR}/xray-config.json" > "${BASE_DIR}/xray-config.json.tmp"
        mv "${BASE_DIR}/xray-config.json.tmp" "${BASE_DIR}/xray-config.json"
        chmod 644 "${BASE_DIR}/xray-config.json"
        systemctl restart xray
        print_info "  ✓ routing 已清理，xray 重启完成"
        updated=1
    fi

    # C. config.yaml 加 DoH resolver
    if [[ -f "${BASE_DIR}/config.yaml" ]] && ! grep -q '^resolver:' "${BASE_DIR}/config.yaml"; then
        print_info "v3.5 迁移: config.yaml 加 DoH resolver"
        awk '/^ignoreClientBandwidth:/{
            print; print ""
            print "resolver:"
            print "  type: https"
            print "  https:"
            print "    addr: \"1.1.1.1:443\""
            print "    sni: cloudflare-dns.com"
            next
        }1' "${BASE_DIR}/config.yaml" > "${BASE_DIR}/config.yaml.tmp" \
        && mv "${BASE_DIR}/config.yaml.tmp" "${BASE_DIR}/config.yaml"
        hysteria_config_changed=1
        print_info "  ✓ config.yaml 加 resolver DoH 块"
    fi

    # D. 静态 /etc/resolv.conf
    if [[ -L /etc/resolv.conf ]] || grep -q '127.0.0.53' /etc/resolv.conf 2>/dev/null; then
        print_info "v3.5 迁移: 切静态 /etc/resolv.conf + 关 systemd-resolved"
        systemctl disable --now systemd-resolved 2>/dev/null || true
        chattr -i /etc/resolv.conf 2>/dev/null || true
        rm -f /etc/resolv.conf
        cat > /etc/resolv.conf <<'RESOLV_EOF'
nameserver 1.1.1.1
nameserver 8.8.8.8
nameserver 9.9.9.9
options edns0 timeout:2 attempts:2 single-request
RESOLV_EOF
        chattr +i /etc/resolv.conf 2>/dev/null || true
        print_info "  ✓ /etc/resolv.conf 静态化"
    fi

    # E. 防火墙放行 v3.5 新端口
    if command -v ufw &>/dev/null && ufw status 2>/dev/null | grep -q "active"; then
        ufw status 2>/dev/null | grep -q "10002/tcp" || ufw allow 10002/tcp 2>/dev/null || true
        ufw status 2>/dev/null | grep -q "40000/udp" || ufw allow 40000/udp 2>/dev/null || true
        ufw status 2>/dev/null | grep -qE "41000:50000/udp" || ufw allow 41000:50000/udp 2>/dev/null || true
        print_info "  ✓ UFW 放行 v3.5 新端口"
    fi
    if command -v firewall-cmd &>/dev/null && systemctl is-active --quiet firewalld 2>/dev/null; then
        firewall-cmd --permanent --add-port=10002/tcp 2>/dev/null || true
        firewall-cmd --permanent --add-port=40000/udp 2>/dev/null || true
        firewall-cmd --permanent --add-port=41000-50000/udp 2>/dev/null || true
        firewall-cmd --reload 2>/dev/null || true
        print_info "  ✓ firewalld 放行 v3.5 新端口"
    fi

    # install-key.txt 权限收紧到 0600（幂等）
    if [[ -f /opt/b-ui/install-key.txt ]]; then
        chmod 600 /opt/b-ui/install-key.txt
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
    else
        # v3.5.10: 版本相同也跑一次 apply_systemd_configs，让 .fix 块的 guard 自检 + 自愈
        # 历史 bug：新 .fix 块加到 update.sh 后，bash 已加载老函数到 RAM，本次 do_update 调用
        # 仍跑老函数定义；新 .fix 块要等"下次 update"才生效。但"下次"只有有新版本时才会触发，
        # 没新版本就永远卡住——所以让"无更新"路径也跑一次 apply_systemd_configs (所有 .fix 块
        # 都有 guard，无残留即 no-op)，把"打不死"的迁移块改成"打得死"。
        echo ""
        print_info "幂等自愈检查（无残留时为 no-op）..."
        apply_systemd_configs
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
            ["web/qrcode.min.js"]="${ADMIN_DIR}/qrcode.min.js"
            ["web/logo.jpg"]="${ADMIN_DIR}/logo.jpg"
            ["b-ui-client.sh"]="${BASE_DIR}/b-ui-client.sh"
        )
        
        local auto_failed=0
        for remote in "${!file_map[@]}"; do
            local local_path="${file_map[$remote]}"
            mkdir -p "$(dirname "$local_path")"

            if download_and_validate "${DOWNLOAD_URL}/${remote}" "$local_path"; then
                chmod +x "$local_path" 2>/dev/null || true
            else
                echo "[$(date '+%Y-%m-%d %H:%M:%S')] 文件下载/校验失败: ${remote}" >> "$LOG_FILE"
                ((auto_failed++))
            fi
        done
        if [[ $auto_failed -gt 0 ]]; then
            echo "[$(date '+%Y-%m-%d %H:%M:%S')] 警告: 共 ${auto_failed} 个文件未通过校验，未覆盖现有版本" >> "$LOG_FILE"
        fi
        
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
        
        # 重启服务（含住宅实例，否则版本升级后 config-residential 变更不生效）
        systemctl restart b-ui-admin 2>/dev/null || true
        systemctl restart hysteria-server 2>/dev/null || true
        systemctl is-active --quiet hysteria-residential 2>/dev/null && systemctl restart hysteria-residential 2>/dev/null || true
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
        # v3.5.10: 无更新也跑一次 apply_systemd_configs 做幂等自愈（详见 check_and_update 注释）
        apply_systemd_configs 2>/dev/null >> "$LOG_FILE" || true
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
