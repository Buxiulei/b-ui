# B-UI CLI 交互重设计

**日期:** 2026-05-07  
**范围:** bui-c (b-ui-client.sh) + b-ui (server/b-ui-cli.sh)  
**目标:** 引入 gum + fzf 实现现代 TUI 体验，同时暴露非交互子命令接口供 agent 自动化调用

---

## 背景与问题

当前两个 CLI 的主要痛点：
- 节点切换需要 3 层菜单（主 → 节点管理 → 切换）
- 所有选择都是"输入数字"，无法箭头键导航
- 耗时操作无进度反馈
- 无法通过命令行参数直接执行操作（无法被 agent 调用）
- 配置切换偶发"配置文件不存在"错误（已在独立 PR 修复）

---

## 技术依赖

### gum（Charmbracelet）
- 用途：箭头键菜单、spinner、confirm、多行输入、状态样式
- 安装：从 GitHub Releases 下载二进制到 `/usr/local/bin/gum`
- 大小：~11MB，支持 Linux x86_64 / arm64

### fzf
- 用途：节点模糊搜索选择器
- 安装：从 GitHub Releases 下载到 `/usr/local/bin/fzf`
- 大小：~3MB

### 自动安装时机
- `install.sh` 安装过程中，在安装主服务后执行 `install_tui_tools`
- `update.sh` 检测是否存在，不存在则补装
- 安装失败时打印警告，不阻断主流程（降级为传统交互）

---

## bui-c 客户端重设计

### 调用模式

```
bui-c                    # 无参数：进入 gum TUI 主菜单
bui-c <subcommand> ...   # 有参数：非交互直接执行
```

### 非交互子命令接口

| 命令 | 说明 |
|------|------|
| `bui-c switch <节点名>` | 切换到指定节点（精确匹配名称） |
| `bui-c tun on` | 启动 TUN 模式 |
| `bui-c tun off` | 停止 TUN 模式 |
| `bui-c tun status` | 输出 TUN 当前状态 |
| `bui-c import <uri>` | 导入单个节点 URI（不激活） |
| `bui-c import <uri> --activate` | 导入并立即激活 |
| `bui-c start` | 启动当前激活节点的客户端服务 |
| `bui-c stop` | 停止所有客户端服务（含 TUN） |
| `bui-c restart` | 重启当前客户端服务 |
| `bui-c status` | 输出当前状态（人类可读） |
| `bui-c status --json` | 输出 JSON 格式状态 |
| `bui-c list` | 列出所有已保存节点 |
| `bui-c list --json` | JSON 格式节点列表 |

**通用 flags：**
- `-y` / `--yes`：跳过所有确认提示
- `--json`：机器可读输出（配合 `status`、`list`）
- `--quiet`：只输出结果，不输出过程信息

**退出码：**
- `0`：成功
- `1`：操作失败（具体错误输出到 stderr）
- `2`：参数错误
- `3`：节点/配置不存在

**`bui-c status --json` 输出示例：**
```json
{
  "active_node": "Tokyo-1",
  "protocol": "hysteria2",
  "service": "running",
  "socks_port": 1080,
  "http_port": 8080,
  "tun": "stopped"
}
```

**`bui-c list --json` 输出示例：**
```json
[
  {"name": "Tokyo-1", "protocol": "hysteria2", "server": "tokyo.example.com:443", "active": true},
  {"name": "HK-VLESS", "protocol": "vless-reality", "server": "hk.example.com:8443", "active": false}
]
```

---

### TUI 主界面

无参数调用时进入交互模式。顶部状态栏 + gum choose 菜单：

```
╔══════════════════════════════════════════╗
║  B-UI Client                            ║
╠══════════════════════════════════════════╣
║  节点   🟢  Tokyo-1  │  hysteria2       ║
║  代理       SOCKS5 :1080  HTTP :8080    ║
║  TUN    🔴  关闭                         ║
╚══════════════════════════════════════════╝

  > 切换节点
    开启 TUN
    ─────────
    导入节点
    服务控制
    高级设置
    ─────────
    更新
    卸载
    退出
```

箭头键导航，Enter 确认。TUN 条目标签根据状态动态显示"开启 TUN"或"停止 TUN"。

---

### 关键 TUI 流程

#### 切换节点
主菜单 → Enter → fzf 模糊选择器（含预览）→ 确认 → spinner 分步执行

```
  搜索节点...
> Tokyo-1        hysteria2   tokyo.example.com:443   ★ 当前
  HK-VLESS       vless       hk.example.com:8443
  SG-Hysteria2   hysteria2   sg.example.com:443

  ↑↓ 选择   Enter 确认   Esc 取消
```

切换执行：
```
◐ 停止当前服务...          ✓
◐ 解析节点配置...           ✓
◐ 生成 Hysteria2 配置...   ✓
◐ 启动服务...               ✓
◐ 重新生成 TUN 配置...     ✓
✓ 已切换到 HK-VLESS  代理 IP: 1.2.3.4
```

任一步失败立即停止并输出错误，不继续后续步骤。

#### TUN 开关
直接在主菜单选择，gum spin 包裹启停流程，完成后刷新状态栏。

#### 导入节点
gum write 多行输入框，粘贴后空行确认：
```
粘贴链接（每行一个，空行结束）:
> ______________________________________
  支持: hysteria2://  vless://  https://订阅地址
```

解析结果列表展示，提示是否立即激活（gum choose）。

#### 服务控制
动态选项，仅显示与当前状态相关的操作：
```
服务状态
  Hysteria2    🟢 运行中
  Xray         🔴 停止
  TUN          🔴 停止

  > 重启 Hysteria2
    启动 Xray
    查看日志 →
    返回
```

---

## b-ui 服务端重设计

### 非交互子命令接口

| 命令 | 说明 |
|------|------|
| `b-ui restart` | 重启所有服务 |
| `b-ui status` | 输出服务状态 |
| `b-ui status --json` | JSON 格式状态 |
| `b-ui logs <service>` | 输出指定服务日志（hysteria2/xray/admin/caddy） |
| `b-ui update` | 执行更新检查并升级 |
| `b-ui residential enable <url>` | 启用住宅 IP 出口 |
| `b-ui residential disable` | 禁用住宅 IP 出口 |

**`b-ui status --json` 输出示例：**
```json
{
  "hysteria2": "running",
  "xray": "running",
  "admin": "running",
  "caddy": "running",
  "bbr": true,
  "autostart": true,
  "version": "2.x.x"
}
```

### TUI 主界面

```
╔══════════════════════════════════════════╗
║  B-UI Server                            ║
╠══════════════════════════════════════════╣
║  Hysteria2   🟢  运行中                  ║
║  Xray        🟢  运行中                  ║
║  Admin 面板  🟢  :8080                   ║
║  Caddy       🟢  运行中                  ║
║  BBR         ✓   已开启                  ║
╚══════════════════════════════════════════╝

  > 重启所有服务
    查看日志 →
    查看客户端配置
    ─────────
    更新
    端口跳跃设置
    住宅 IP 出口 →
    ─────────
    更多设置 →
    卸载
    退出
```

"更多设置"收纳低频选项：修改管理密码、BBR 开关、自启动设置、VPS 测速。

---

## 实现架构

### 代码组织

两个脚本分别重构，不合并。新增以下辅助函数区块（置于脚本顶部 helper 区）：

```bash
# TUI helpers - gum/fzf wrappers
tui_menu()    # gum choose 封装
tui_spin()    # gum spin 封装，失败时 exit 1
tui_confirm() # gum confirm 封装，-y flag 时直接返回 0
tui_input()   # gum input 封装
tui_write()   # gum write 封装（多行）
tui_filter()  # fzf 封装，用于节点选择

# Non-interactive dispatch
dispatch_subcommand()  # 解析 $1 subcommand，路由到对应函数
```

### 非交互与 TUI 路径分离

```bash
main() {
    parse_global_flags "$@"    # 提取 -y, --json, --quiet
    
    if [[ $# -gt 0 ]]; then
        dispatch_subcommand "$@"   # 非交互路径
    else
        tui_main_menu              # TUI 路径
    fi
}
```

### gum 可用性检测

```bash
TUI_AVAILABLE=true
command -v gum &>/dev/null || TUI_AVAILABLE=false
command -v fzf &>/dev/null || TUI_AVAILABLE=false

# 如果 TUI 不可用，非交互子命令仍然工作（不依赖 gum）
# 如果 TUI 不可用且无参数，提示安装 gum 后重试，或直接走传统数字菜单作为 fallback
```

---

## 不在此版本范围内

- 完整的 bash completion（Tab 补全）
- 配置文件（~/.bui-c.conf）
- 多配置文件组/标签
- Web UI 集成

---

## 实现顺序

1. `install_tui_tools`：gum + fzf 自动安装（install.sh + update.sh）
2. bui-c 非交互子命令接口（dispatch_subcommand + 各子命令函数）
3. bui-c TUI 重构（主菜单 + 节点切换 + TUN 开关）
4. bui-c 其余 TUI 流程（导入、服务控制、高级设置）
5. b-ui 非交互子命令接口
6. b-ui TUI 重构
