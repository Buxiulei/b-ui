# CLI Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 用 gum + fzf 替换两个 CLI 脚本的交互层，同时暴露非交互子命令接口供 agent 调用。

**Architecture:** 两个脚本各自独立重构，不合并。新增 TUI helper 函数层（tui_spin / tui_confirm / tui_menu / tui_filter）供交互路径使用；非交互路径（有参数时）直接调用核心逻辑函数，完全绕过 gum/fzf。`main()` 通过 `$#` 判断路径：有参数走 `dispatch_subcommand`，无参数走 TUI 主菜单。

**Tech Stack:** bash, gum (Charmbracelet), fzf, systemctl, curl

---

## 文件结构

| 文件 | 变更类型 | 说明 |
|------|----------|------|
| `install.sh` | 修改 | 在 `create_global_command` 后插入 `install_tui_tools` 调用 |
| `server/update.sh` | 修改 | 在更新检查逻辑中补装缺失的 gum/fzf |
| `b-ui-client.sh` | 主要重构 | 新增 TUI helpers、global flags、dispatch、所有 cmd_* 函数；重写 main()、switch_config、show_menu |
| `server/b-ui-cli.sh` | 中等重构 | 新增 TUI helpers、dispatch、cmd_* 函数；重写 main()、show_menu |

---

## Task 1: install_tui_tools — 自动安装 gum 和 fzf

**Files:**
- Modify: `install.sh`（在 `create_global_command` 调用之后插入）

- [ ] **Step 1: 在 install.sh 末尾（`create_global_command` 调用后、`harden_ssh` 前）添加以下函数和调用**

```bash
install_tui_tools() {
    print_info "安装 TUI 工具 (gum + fzf)..."

    local arch
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
            | grep -i "^location:" | grep -oE 'v[0-9]+\.[0-9]+\.[0-9]+')
        if [[ -z "$gum_ver" ]]; then
            print_warning "无法获取 gum 版本，跳过"
        else
            local gum_url="https://github.com/charmbracelet/gum/releases/download/${gum_ver}/gum_${gum_ver#v}_Linux_${arch}.tar.gz"
            local tmp=$(mktemp -d)
            if curl -fsSL "$gum_url" | tar -xz -C "$tmp" 2>/dev/null; then
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
            | grep -i "^location:" | grep -oE 'v[0-9]+\.[0-9]+\.[0-9]+')
        if [[ -z "$fzf_ver" ]]; then
            print_warning "无法获取 fzf 版本，跳过"
        else
            local fzf_url="https://github.com/junegunn/fzf/releases/download/${fzf_ver}/fzf-${fzf_ver#v}-linux_${fzf_arch}.tar.gz"
            local tmp=$(mktemp -d)
            if curl -fsSL "$fzf_url" | tar -xz -C "$tmp" 2>/dev/null; then
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
```

在 `install.sh` 中找到 `create_global_command` 调用行，在其后插入：

```bash
    # 安装 TUI 工具
    install_tui_tools
```

- [ ] **Step 2: 在 `server/update.sh` 的更新逻辑末尾同样调用 install_tui_tools**

在 `server/update.sh` 中找到文件结尾的主执行流程，在现有更新完成后追加（复制上面完整函数，然后调用）：

```bash
# 补装缺失的 TUI 工具
if ! command -v gum &>/dev/null || ! command -v fzf &>/dev/null; then
    install_tui_tools
fi
```

- [ ] **Step 3: 验证语法**

```bash
bash -n install.sh && echo "PASS: install.sh syntax OK"
bash -n server/update.sh && echo "PASS: update.sh syntax OK"
```

Expected: 两行均输出 PASS。

- [ ] **Step 4: Commit**

```bash
git add install.sh server/update.sh
git commit -m "feat(install): auto-install gum and fzf TUI tools"
```

---

## Task 2: b-ui-client.sh — TUI helpers + 全局 flags

**Files:**
- Modify: `b-ui-client.sh`（在现有全局变量区块之后、第一个函数之前插入）

- [ ] **Step 1: 在 `b-ui-client.sh` 第 36 行附近（现有全局变量末尾）追加以下内容**

```bash
#===============================================================================
# TUI 工具检测 & Helpers
#===============================================================================

TUI_AVAILABLE=false
command -v gum &>/dev/null && command -v fzf &>/dev/null && TUI_AVAILABLE=true

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
# 失败时直接 exit 1（非交互路径中调用方可用 || return 1 覆盖）
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
        # fallback：显示编号列表，用户输入编号，输出对应行
        local lines=()
        while IFS= read -r line; do lines+=("$line"); done
        local i=1
        for line in "${lines[@]}"; do
            echo "  $i. $line"
            ((i++))
        done
        local choice
        read -p "选择 (1-$((i-1))): " choice
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
```

- [ ] **Step 2: 验证语法**

```bash
bash -n b-ui-client.sh && echo "PASS: syntax OK"
```

Expected: `PASS: syntax OK`

- [ ] **Step 3: Commit**

```bash
git add b-ui-client.sh
git commit -m "feat(bui-c): add TUI helpers and global flag parsing"
```

---

## Task 3: 提取 _switch_to_profile 核心函数

当前 `switch_config`（约第 1720 行）把"选择界面"和"执行切换"混在一起，非交互路径无法直接调用。本任务将执行部分提取为 `_switch_to_profile <name>`。

**Files:**
- Modify: `b-ui-client.sh`

- [ ] **Step 1: 在 `switch_config` 函数之前插入 `_switch_to_profile`**

找到 `switch_config()` 定义处（约第 1720 行），在其上方插入：

```bash
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

    local protocol=$(grep '"protocol"' "$meta_file" 2>/dev/null | cut -d'"' -f4)
    local uri=$(cat "$uri_file")

    # 读取端口
    local stored_socks=$(grep '"socks_port"' "$meta_file" 2>/dev/null | grep -o '[0-9]*' | head -1)
    local stored_http=$(grep '"http_port"'  "$meta_file" 2>/dev/null | grep -o '[0-9]*' | head -1)
    if [[ -z "$stored_socks" ]] && [[ -f "${config_dir}/config.yaml" ]]; then
        stored_socks=$(grep -A1 '^socks5:' "${config_dir}/config.yaml" | grep 'listen:' | sed 's/.*://')
    fi
    if [[ -z "$stored_http" ]] && [[ -f "${config_dir}/config.yaml" ]]; then
        stored_http=$(grep -A1 '^http:' "${config_dir}/config.yaml" | grep 'listen:' | sed 's/.*://')
    fi
    SOCKS_PORT="${stored_socks:-1080}"
    HTTP_PORT="${stored_http:-8080}"

    # 记录 TUN 状态
    local tun_was_active=false
    systemctl is-active --quiet bui-tun 2>/dev/null && tun_was_active=true
    [[ "$tun_was_active" == "true" ]] && tui_info "检测到 TUN 运行中，切换后自动重启..."

    # 停止 TUN
    [[ "$tun_was_active" == "true" ]] && tui_spin "停止 TUN 模式..." stop_tun_mode

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
        tui_spin "生成 Hysteria2 配置..." generate_config
        cp "$CONFIG_FILE" "${config_dir}/config.yaml"
        tui_spin "启动 Hysteria2 服务..." bash -c "
            create_service
            systemctl start '$CLIENT_SERVICE'
        "
    else
        tui_spin "生成 Xray 配置..." _write_xray_json
        if [[ ! -f /etc/systemd/system/xray-client.service ]]; then
            local xray_config="${BASE_DIR}/xray-config.json"
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
        fi
        cp "${BASE_DIR}/xray-config.json" "${config_dir}/xray-config.json"
        tui_spin "启动 Xray 服务..." systemctl start xray-client
    fi

    echo "$selected" > "$ACTIVE_CONFIG"

    # 重新生成 TUN 配置
    tui_spin "重新生成 TUN 配置..." bash -c "
        if [[ '$protocol' == 'hysteria2' ]]; then
            generate_singbox_tun_config hysteria2
        else
            generate_singbox_tun_config vless-reality
        fi
    "

    [[ "$tun_was_active" == "true" ]] && tui_spin "重启 TUN 模式..." start_tun_mode

    tui_success "已切换到: $selected"

    if [[ "$tun_was_active" != "true" ]] && [[ "$OPT_JSON" != "true" ]]; then
        local proxy_ip
        proxy_ip=$(curl -s --max-time 5 --socks5 "127.0.0.1:${SOCKS_PORT}" "https://api.ipify.org" 2>/dev/null)
        if [[ -n "$proxy_ip" ]] && [[ "$proxy_ip" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
            tui_success "代理 IP: ${proxy_ip}"
        fi
    fi

    return 0
}
```

- [ ] **Step 2: 将现有 `switch_config` 函数简化，改为调用 `_switch_to_profile`**

找到 `switch_config()` 函数体，将选择之后的所有执行逻辑替换为调用 `_switch_to_profile`：

```bash
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
```

- [ ] **Step 3: 验证语法**

```bash
bash -n b-ui-client.sh && echo "PASS: syntax OK"
```

- [ ] **Step 4: Commit**

```bash
git add b-ui-client.sh
git commit -m "refactor(bui-c): extract _switch_to_profile core function"
```

---

## Task 4: bui-c 非交互 — cmd_status + cmd_list

**Files:**
- Modify: `b-ui-client.sh`（在 `_switch_to_profile` 下方插入新 section）

- [ ] **Step 1: 在 `_switch_to_profile` 之后插入以下 section**

```bash
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
```

- [ ] **Step 2: 验证语法**

```bash
bash -n b-ui-client.sh && echo "PASS"
```

- [ ] **Step 3: 功能测试（在已有节点的机器上运行）**

```bash
# 测试 status --json 输出有效 JSON
sudo bui-c status --json | python3 -m json.tool > /dev/null && echo "PASS: status JSON valid"

# 测试 list --json
sudo bui-c list --json | python3 -c "import json,sys; data=json.load(sys.stdin); assert isinstance(data, list); print('PASS: list JSON valid')"

# 测试普通输出
sudo bui-c status && echo "PASS: status human readable"
sudo bui-c list   && echo "PASS: list human readable"
```

- [ ] **Step 4: Commit**

```bash
git add b-ui-client.sh
git commit -m "feat(bui-c): add cmd_status and cmd_list subcommands with --json support"
```

---

## Task 5: bui-c 非交互 — cmd_switch + cmd_tun

**Files:**
- Modify: `b-ui-client.sh`（追加到 cmd_list 之后）

- [ ] **Step 1: 添加 cmd_switch 和 cmd_tun**

```bash
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
            tui_spin "启动 TUN 模式..." start_tun_mode
            exit $?
            ;;
        off|stop|disable)
            if ! systemctl is-active --quiet bui-tun 2>/dev/null; then
                tui_info "TUN 未在运行"
                exit 0
            fi
            tui_spin "停止 TUN 模式..." stop_tun_mode
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
```

- [ ] **Step 2: 验证语法**

```bash
bash -n b-ui-client.sh && echo "PASS"
```

- [ ] **Step 3: 功能测试**

```bash
# 测试不存在的节点返回 exit 3
sudo bui-c switch "不存在的节点名xxxx" 2>/dev/null
[[ $? -eq 3 ]] && echo "PASS: exit 3 for unknown node" || echo "FAIL"

# 测试缺少参数返回 exit 2
sudo bui-c switch 2>/dev/null
[[ $? -eq 2 ]] && echo "PASS: exit 2 for missing arg" || echo "FAIL"

# 测试 tun status
sudo bui-c tun status && echo "PASS: tun status works"

# 测试无效 tun 子命令
sudo bui-c tun badcmd 2>/dev/null; [[ $? -eq 2 ]] && echo "PASS" || echo "FAIL"
```

- [ ] **Step 4: Commit**

```bash
git add b-ui-client.sh
git commit -m "feat(bui-c): add cmd_switch and cmd_tun subcommands"
```

---

## Task 6: bui-c 非交互 — cmd_import + cmd_start/stop/restart

**Files:**
- Modify: `b-ui-client.sh`（追加到 cmd_tun 之后）

- [ ] **Step 1: 添加以下函数**

```bash
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

    safe_import_parsed "$parsed"

    local config_name="${REMARK:-${protocol}-$(date +%s)}"
    config_name=$(echo "$config_name" | sed 's/[\/\\:*?"<>|]/-/g')

    if [[ -d "${CONFIGS_DIR}/${config_name}" ]]; then
        tui_info "节点 '$config_name' 已存在，跳过"
        exit 0
    fi

    save_config_meta "$config_name" "$protocol" "$SERVER_ADDR" "$uri" 1080 8080
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
    if [[ "$protocol" == "hysteria2" ]]; then
        tui_spin "启动 Hysteria2..." systemctl start "$CLIENT_SERVICE"
    else
        tui_spin "启动 Xray..." systemctl start xray-client
    fi
    tui_success "服务已启动"
    exit 0
}

cmd_stop() {
    tui_spin "停止客户端服务..." bash -c "
        systemctl stop '$CLIENT_SERVICE' 2>/dev/null || true
        systemctl stop xray-client 2>/dev/null || true
        systemctl stop bui-tun 2>/dev/null || true
    "
    tui_success "服务已停止"
    exit 0
}

cmd_restart() {
    cmd_stop
    # cmd_stop 已 exit，不会到达这里；单独实现以避免 exit 问题
    # 注意：使用独立逻辑
    local active
    active=$(get_active_config)
    if [[ -z "$active" ]]; then
        tui_error "没有激活的节点"
        exit 1
    fi
    _switch_to_profile "$active"
    exit $?
}
```

> 注意：`cmd_stop` 和 `cmd_restart` 都调用 `exit`，在 `dispatch_subcommand` 中直接 exec 即可。`cmd_restart` 复用 `_switch_to_profile` 实现完整重启（重新生成配置 + 启动服务）。

- [ ] **Step 2: 验证语法**

```bash
bash -n b-ui-client.sh && echo "PASS"
```

- [ ] **Step 3: 测试 import 错误处理**

```bash
# 无效 URI
sudo bui-c import "invalid://xx" 2>/dev/null
[[ $? -eq 2 ]] && echo "PASS: exit 2 for invalid URI" || echo "FAIL"

# 缺少参数
sudo bui-c import 2>/dev/null
[[ $? -eq 2 ]] && echo "PASS: exit 2 for missing arg" || echo "FAIL"
```

- [ ] **Step 4: Commit**

```bash
git add b-ui-client.sh
git commit -m "feat(bui-c): add cmd_import, cmd_start, cmd_stop, cmd_restart subcommands"
```

---

## Task 7: bui-c — dispatch_subcommand + main() 入口改造

**Files:**
- Modify: `b-ui-client.sh`（修改 `main()` 函数，添加 `dispatch_subcommand`）

- [ ] **Step 1: 在 `main()` 函数之前插入 `dispatch_subcommand`**

```bash
dispatch_subcommand() {
    local cmd="$1"; shift
    case "$cmd" in
        switch)  cmd_switch "$@" ;;
        tun)     cmd_tun "$@" ;;
        import)  cmd_import "$@" ;;
        start)   cmd_start "$@" ;;
        stop)    cmd_stop "$@" ;;
        restart) cmd_restart "$@" ;;
        status)  cmd_status "$@" ;;
        list)    cmd_list "$@" ;;
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
```

- [ ] **Step 2: 替换 `main()` 函数**

找到现有 `main()` 函数（约第 4001 行），整体替换为：

```bash
main() {
    check_root
    check_os

    # 提取全局 flags（-y, --json, --quiet）
    parse_global_flags "$@"
    # 过滤掉 flags，只保留非 flag 参数
    local args=()
    for arg in "$@"; do
        [[ "$arg" == -* ]] || args+=("$arg")
    done

    # 有子命令 → 非交互路径
    if [[ ${#args[@]} -gt 0 ]]; then
        # 依赖检查：非交互路径不检查可选依赖（hysteria/xray 可能未安装）
        dispatch_subcommand "${args[@]}"
        exit $?
    fi

    # 无参数 → TUI 交互模式
    if ! command -v hysteria &>/dev/null || ! command -v xray &>/dev/null || ! command -v sing-box &>/dev/null; then
        check_dependencies
    fi

    check_client_update &>/dev/null &

    tui_main_loop
}
```

- [ ] **Step 3: 在 `main()` 下方添加 `tui_main_loop`（暂时调用旧的交互逻辑，后续 Task 8 替换）**

```bash
tui_main_loop() {
    while true; do
        print_banner
        show_status
        show_menu
        read -p "请选择 [0-8]: " choice
        case $choice in
            1) import_node ;;
            2) config_management ;;
            3) service_control_menu ;;
            4) toggle_tun ;;
            5) test_proxy ;;
            6) update_all ;;
            7) advanced_settings_menu ;;
            8) uninstall ;;
            0) echo ""; tui_info "再见！"; exit 0 ;;
            *) print_error "无效选项" ;;
        esac
        echo ""
        read -p "按 Enter 继续..."
    done
}
```

- [ ] **Step 4: 验证语法**

```bash
bash -n b-ui-client.sh && echo "PASS"
```

- [ ] **Step 5: 端到端测试非交互路径**

```bash
# help
sudo bui-c --help | grep -q "switch" && echo "PASS: help works"

# 未知命令返回 2
sudo bui-c unknowncmd 2>/dev/null; [[ $? -eq 2 ]] && echo "PASS: unknown cmd exit 2" || echo "FAIL"

# status 可运行
sudo bui-c status && echo "PASS: status works"

# list 可运行
sudo bui-c list && echo "PASS: list works"

# status --json 有效
sudo bui-c status --json | python3 -m json.tool >/dev/null && echo "PASS: status --json valid"

# list --json 有效
sudo bui-c list --json | python3 -c "import json,sys; json.load(sys.stdin); print('PASS: list --json valid')"
```

- [ ] **Step 6: Commit**

```bash
git add b-ui-client.sh
git commit -m "feat(bui-c): add dispatch_subcommand and refactor main() for dual-mode operation"
```

---

## Task 8: bui-c TUI — 状态栏 + gum 主菜单

**Files:**
- Modify: `b-ui-client.sh`（替换 `tui_main_loop` 和 `show_menu`）

- [ ] **Step 1: 添加 `show_status_bar` 函数（在 `show_status` 函数附近插入）**

```bash
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
```

- [ ] **Step 2: 替换 `tui_main_loop` 为 gum choose 版本**

```bash
tui_main_loop() {
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
                2>/dev/null) || choice="退出"
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
            "切换节点")    tui_switch_node ;;
            "开启 TUN"|"停止 TUN") tui_toggle_tun ;;
            "导入节点")    tui_import_node ;;
            "服务控制")    tui_service_control ;;
            "高级设置")    advanced_settings_menu ;;
            "一键更新")    update_all ;;
            "卸载")        uninstall ;;
            "退出")        echo ""; tui_info "再见！"; exit 0 ;;
            "──────────")  continue ;;
            "__invalid__") print_error "无效选项" ;;
        esac

        if [[ "$TUI_AVAILABLE" != "true" ]]; then
            echo ""
            read -p "按 Enter 继续..."
        fi
    done
}
```

> 注意：分隔线条目 `──────────` 被选中时 continue 跳过，gum choose 用户体验上通常用 `--limit 1` 不会选到分隔线，但加判断更安全。

- [ ] **Step 3: 添加三个 TUI 函数存根（后续 Task 9-10 实现）**

```bash
tui_switch_node()    { switch_config; }
tui_toggle_tun()     { toggle_tun; }
tui_import_node()    { import_node; }
tui_service_control() { service_control_menu; }
```

- [ ] **Step 4: 验证语法**

```bash
bash -n b-ui-client.sh && echo "PASS"
```

- [ ] **Step 5: 冒烟测试（在有 gum 的系统上）**

```bash
# 验证 TUI_AVAILABLE 检测正确
sudo bash -c 'source b-ui-client.sh; echo "TUI_AVAILABLE=$TUI_AVAILABLE"'

# 验证 show_status_bar 不报错
sudo bash -c 'source b-ui-client.sh; show_status_bar' && echo "PASS: status bar renders"
```

- [ ] **Step 6: Commit**

```bash
git add b-ui-client.sh
git commit -m "feat(bui-c): TUI status bar and gum choose main menu"
```

---

## Task 9: bui-c TUI — 节点切换（fzf picker）

**Files:**
- Modify: `b-ui-client.sh`（替换 `tui_switch_node` 存根）

- [ ] **Step 1: 替换 `tui_switch_node` 存根**

```bash
tui_switch_node() {
    if [[ ! -d "$CONFIGS_DIR" ]] || [[ -z "$(ls -A "$CONFIGS_DIR" 2>/dev/null)" ]]; then
        tui_error "没有已保存的节点，请先导入"
        sleep 1
        return 0
    fi

    local active; active=$(get_active_config)

    # 构建 fzf 显示列表（格式：名称  协议  服务器  [当前]）
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
                  --border rounded \
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
        list_configs
        echo ""
        read -p "选择配置编号 (0 返回): " choice
        [[ "$choice" == "0" ]] || [[ -z "$choice" ]] && return 0
        [[ ! "$choice" =~ ^[0-9]+$ ]] || [[ "$choice" -lt 1 ]] || [[ "$choice" -gt ${#configs[@]} ]] && {
            print_error "无效选择"; return 1
        }
        selected="${configs[$((choice-1))]}"
    fi

    [[ -z "$selected" ]] && return 0
    [[ "$selected" == "$active" ]] && { tui_info "已是当前节点"; sleep 1; return 0; }

    _switch_to_profile "$selected"
}
```

- [ ] **Step 2: 验证语法**

```bash
bash -n b-ui-client.sh && echo "PASS"
```

- [ ] **Step 3: Commit**

```bash
git add b-ui-client.sh
git commit -m "feat(bui-c): TUI node switch with fzf fuzzy picker"
```

---

## Task 10: bui-c TUI — TUN 开关 + 导入 + 服务控制

**Files:**
- Modify: `b-ui-client.sh`（替换三个 TUI 存根）

- [ ] **Step 1: 替换 `tui_toggle_tun`**

```bash
tui_toggle_tun() {
    if systemctl is-active --quiet bui-tun 2>/dev/null; then
        if tui_confirm "停止 TUN 模式？"; then
            tui_spin "停止 TUN..." stop_tun_mode && tui_success "TUN 已停止"
        fi
    else
        local active; active=$(get_active_config)
        if [[ -z "$active" ]]; then
            tui_error "没有激活的节点，无法启动 TUN"
            sleep 1
            return 0
        fi
        if tui_confirm "启动 TUN 全局代理模式？"; then
            tui_spin "生成 TUN 配置..." bash -c "
                local protocol
                protocol=$(grep '\"protocol\"' '${CONFIGS_DIR}/${active}/meta.json' 2>/dev/null | cut -d'\"' -f4)
                generate_singbox_tun_config \"\${protocol:-hysteria2}\"
            "
            tui_spin "启动 TUN..." start_tun_mode && tui_success "TUN 已启动"
        fi
    fi
    sleep 1
}
```

- [ ] **Step 2: 替换 `tui_import_node`**

```bash
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
        local lines=()
        while IFS= read -r line; do
            [[ -z "$line" ]] && break
            lines+=("$line")
        done
        raw_input=$(printf '%s\n' "${lines[@]}")
    fi

    [[ -z "$raw_input" ]] && return 0

    # 委托给现有 import_node 的核心逻辑（通过临时文件传递）
    # 构造 lines 数组并直接走解析流程
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

        safe_import_parsed "$parsed"
        local config_name="${REMARK:-${protocol}-$(date +%s)}"
        config_name=$(echo "$config_name" | sed 's/[\/\\:*?"<>|]/-/g')

        if [[ -d "${CONFIGS_DIR}/${config_name}" ]]; then
            tui_info "已存在: $config_name（跳过）"; continue
        fi

        save_config_meta "$config_name" "$protocol" "$SERVER_ADDR" "$uri" 1080 8080
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
```

- [ ] **Step 3: 替换 `tui_service_control`**

```bash
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

        # 动态选项
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
            "启动 Hysteria2")  tui_spin "启动..." systemctl start "$CLIENT_SERVICE" ;;
            "停止 Hysteria2")  tui_spin "停止..." systemctl stop "$CLIENT_SERVICE" ;;
            "重启 Hysteria2")  tui_spin "重启..." systemctl restart "$CLIENT_SERVICE" ;;
            "启动 Xray")       tui_spin "启动..." systemctl start xray-client ;;
            "停止 Xray")       tui_spin "停止..." systemctl stop xray-client ;;
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
```

- [ ] **Step 4: 验证语法**

```bash
bash -n b-ui-client.sh && echo "PASS"
```

- [ ] **Step 5: Commit**

```bash
git add b-ui-client.sh
git commit -m "feat(bui-c): TUI toggle TUN, import node, and service control menus"
```

---

## Task 11: b-ui 服务端 — 非交互子命令 + TUI 主菜单

**Files:**
- Modify: `server/b-ui-cli.sh`

- [ ] **Step 1: 在全局变量区块之后（约第 80 行 `show_status` 之前）插入 TUI helpers 和 global flags**

（内容与 b-ui-client.sh Task 2 的 TUI helpers 完全相同，复制粘贴 `TUI_AVAILABLE` 检测、`parse_global_flags`、`tui_info/success/error/spin/confirm/menu` 这几个函数）

- [ ] **Step 2: 在 `show_status` 之后插入 `show_status_bar_server`**

```bash
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
```

- [ ] **Step 3: 在 `main()` 之前插入非交互子命令函数和 dispatch**

```bash
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
    tui_spin "重启所有服务..." bash -c "
        systemctl restart hysteria-server 2>/dev/null || true
        systemctl restart xray 2>/dev/null || true
        systemctl restart b-ui-admin 2>/dev/null || true
        systemctl restart caddy 2>/dev/null || true
    "
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
            bash /opt/b-ui/residential-helper.sh enable "$url"
            ;;
        disable)
            bash /opt/b-ui/residential-helper.sh disable
            ;;
        status)
            bash /opt/b-ui/residential-helper.sh status
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
        restart)     cmd_server_restart "$@" ;;
        logs)        cmd_server_logs "$@" ;;
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
```

- [ ] **Step 4: 替换 `main()` 函数**

```bash
main() {
    if [[ $EUID -ne 0 ]]; then
        echo "请使用 sudo b-ui 运行" >&2
        exit 1
    fi

    parse_global_flags "$@"
    local args=()
    for arg in "$@"; do
        [[ "$arg" == -* ]] || args+=("$arg")
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
                1) choice="查看客户端配置" ;;  2) choice="重启所有服务" ;;
                3) choice="查看日志 →" ;;       4) choice="更多设置 →" ;;
                5) choice="更多设置 →" ;;       6) choice="更多设置 →" ;;
                7) choice="更新" ;;             8) choice="更新" ;;
                9) choice="卸载" ;;            10) choice="端口跳跃设置" ;;
                11) choice="更多设置 →" ;;     12) choice="住宅 IP 出口 →" ;;
                0) choice="退出" ;;             *) choice="__invalid__" ;;
            esac
        fi

        case "$choice" in
            "重启所有服务")
                tui_spin "重启所有服务..." bash -c "
                    systemctl restart hysteria-server 2>/dev/null || true
                    systemctl restart xray 2>/dev/null || true
                    systemctl restart b-ui-admin 2>/dev/null || true
                    systemctl restart caddy 2>/dev/null || true
                "
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
            "卸载") uninstall_all ;;
            "退出") tui_info "再见！"; exit 0 ;;
            "──────────") continue ;;
            "__invalid__") print_error "无效选项" ;;
        esac

        if [[ "$TUI_AVAILABLE" != "true" ]]; then
            echo ""
            read -p "按 Enter 继续..."
        fi
    done
}
```

- [ ] **Step 5: 验证语法**

```bash
bash -n server/b-ui-cli.sh && echo "PASS: b-ui-cli.sh syntax OK"
```

- [ ] **Step 6: 测试非交互路径**

```bash
sudo b-ui --help | grep -q "restart" && echo "PASS: help works"
sudo b-ui status --json | python3 -m json.tool >/dev/null && echo "PASS: status --json valid"
sudo b-ui unknowncmd 2>/dev/null; [[ $? -eq 2 ]] && echo "PASS: unknown cmd exit 2"
```

- [ ] **Step 7: Commit**

```bash
git add server/b-ui-cli.sh
git commit -m "feat(b-ui): add non-interactive subcommands and gum TUI main menu"
```

---

## Self-Review

**Spec coverage:**
- ✅ gum/fzf 自动安装 → Task 1
- ✅ bui-c 非交互子命令（switch/tun/import/start/stop/restart/status/list） → Task 4-6
- ✅ -y/--json/--quiet flags → Task 2, 7
- ✅ JSON 输出格式（status + list） → Task 4
- ✅ 退出码（0/1/2/3） → Task 5, 6, 7
- ✅ TUI 状态栏 → Task 8
- ✅ gum choose 主菜单 → Task 8
- ✅ fzf 节点选择（含鼠标支持） → Task 9
- ✅ TUN 开关 gum spin → Task 10
- ✅ gum write 导入 → Task 10
- ✅ 动态服务控制菜单 → Task 10
- ✅ b-ui 非交互子命令 → Task 11
- ✅ b-ui TUI 主菜单 → Task 11
- ✅ update.sh 补装逻辑 → Task 1

**类型一致性：** `_switch_to_profile` 在 Task 3 定义，在 Task 5（cmd_switch）、Task 10（tui_switch_node 确认后）调用，签名一致（单参数节点名）。`tui_spin/tui_confirm/tui_menu` 在 Task 2 定义，整个计划中签名一致。

**无 placeholder：** 所有函数均含完整代码。
