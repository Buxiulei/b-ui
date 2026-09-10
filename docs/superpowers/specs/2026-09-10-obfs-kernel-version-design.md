# salamander obfs 全链路补齐 + 内核版本探测修正（v3.6.0 追加两项）

- 日期：2026-09-10
- 目标版本：v3.6.0（独立提交）
- 状态：已批准，待实施
- 来源：内核与协议选型调研报告 §三「必须先修的三件事」P0-2 / P1（主理人已选定并入）

## 1. 背景

| # | 问题 | 证据（当前工作树） |
|---|---|---|
| O | `b-ui obfs on` 往 `config.yaml`（直连实例）顶部插入 `obfs: type: salamander` 块后，只有 sing-box 订阅与 hy2:// URI 带了 obfs；**Clash 订阅的 `mkHy2()` 不输出 `obfs`/`obfs-password`，`b-ui-client.sh` 全文不解析、不生成 obfs**。开了 obfs 的服务器上，Clash 用户和 Linux 客户端会静默连不上。 | `web/server.js:692-705`（Clash `mkHy2` 无 obfs）；`web/server.js:505-513`（sing-box 生成器有）；`:1664-1665`（URI 有）；`b-ui-client.sh` `parse_hysteria_uri()` 只解析 sni/insecure/mport；`server/b-ui-cli.sh:922-990` `cmd_obfs` |
| V | 所有内核版本探测走 GitHub `/releases/latest`。Xray 自 v26.3.27 之后全部标 `prerelease=true`，`/releases/latest` 永远返回 v26.3.27（2026-03-27），自动更新链路已静默失效半年；sing-box 无版本上限，用户可能被自动升到 1.15（1.14 的弃用项在 1.15 变致命）。 | `server/core.sh:1541-1549`；`server/update.sh:1708-1841`；`b-ui-client.sh:1174,4706-4748,5402-5431`；`server/residential-helper.sh:164`；`web/server.js:1100` |

## 2. 关键决策

| 决策 | 选择 | 理由 |
|---|---|---|
| obfs 作用范围 | 只有 **直连** HY2（`config.yaml`）有 obfs；住宅实例（`config-residential.yaml`）没有。Clash 生成器与 URI/sing-box 生成器一致：直连节点带、住宅节点不带 | `cmd_obfs` 只改 `config.yaml`，sing-box 生成器 `useObfs` 已按此区分 |
| 客户端 obfs 表达 | `parse_hysteria_uri` 解析 `obfs=salamander&obfs-password=<urlenc>` → `OBFS_TYPE`/`OBFS_PASSWORD`（URL 解码）；存入节点 `meta.json`；Hysteria2 原生 `config.yaml` 加 `obfs: {type: salamander, salamander: {password}}`；sing-box TUN 出站加 `"obfs": {"type": "salamander", "password": "…"}` | Hysteria2 客户端配置与 sing-box hysteria2 出站的官方字段 |
| 老节点兼容 | `meta.json` 无 obfs 字段 → 视为无 obfs；重新导入 URI 即可获得 | 不做迁移 |
| `cmd_obfs on` 提示 | 开启后同样打印"客户端订阅链接需要重新生成/重新导入" | 与 off 分支对称 |
| Xray 最新版本 | 用 `GET /repos/XTLS/Xray-core/releases?per_page=5` 取第一个非 draft 条目的 `tag_name`（含 prerelease），不再用 `/releases/latest` | GitHub releases 列表按创建时间倒序；Xray 官方 tag 全为 prerelease |
| sing-box 上限 | 常量 `SINGBOX_MAX_MINOR="1.14"`：`/releases/latest` 得到的版本若 minor 高于上限，改从 `releases?per_page=30` 中取第一个 `tag_name` 匹配 `^v1\.14\.` 的（无则回退 latest 并打警告） | v2rayN 7.25 的做法；1.15 会把 1.14 弃用项变致命 |
| Hysteria2 | 保持 `/releases/latest` | apernet 正常打稳定 tag |
| 抽象位置 | 各 bash 文件内各定义一个 `gh_latest_tag <repo> [tag_regex]` 函数（客户端脚本独立分发，不能 source 服务端文件）；`web/server.js` 的 `fetchLatestRelease(repo)` 内部按 repo 分支处理 | 文件独立分发 |
| 手动确认 | 自动更新逻辑不变（仍按 `_is_newer` 决定是否安装）；本项只修"最新版本是什么" | 范围最小 |

## 3. 组件设计

### 3.1 obfs（Task A）

- `web/server.js` `generateClashConfig()` `mkHy2(name, port, hopStart, hopEnd)` 增加第 5 参 `useObfs`；直连调用传 `true`、住宅传 `false`；`useObfs && cfg.obfs?.enabled && cfg.obfs.type === "salamander" && cfg.obfs.password` 时在 YAML 节点里追加 `obfs: salamander` 与 `obfs-password: <值>`（YAML 需引号安全：用 JSON.stringify 生成带引号字符串）。
- `b-ui-client.sh`：
  - `parse_hysteria_uri()`：解析 `obfs=([^&]+)` 与 `obfs-password=([^&]+)`，密码 URL 解码（同 `password` 的解码方式），追加输出 `OBFS_TYPE=…`、`OBFS_PASSWORD=…`。
  - 节点保存/读取（`safe_import_parsed` / `save_config_meta` / 读取 meta 的地方）把两个字段一起持久化与恢复（跟随 `MPORT` 的处理路径逐处补齐）。
  - `generate_config()`（Hysteria2 原生 YAML）：`OBFS_TYPE == salamander && OBFS_PASSWORD` 非空时输出
    ```yaml
    obfs:
      type: salamander
      salamander:
        password: <OBFS_PASSWORD>
    ```
  - `generate_singbox_tun_config()` hysteria2 出站：同条件下追加 `"obfs": {"type": "salamander", "password": "<json_escape>"}`。
- `server/b-ui-cli.sh` `cmd_obfs on`：成功后追加 `print_warning "客户端订阅链接需要重新生成/重新导入（新增 obfs 参数）"`。

### 3.2 内核版本探测（Task B）

- 新函数（每个 bash 文件各一份，放在版本探测代码附近）：
  ```bash
  # v3.6.0: 取 GitHub 最新 release tag。$2 为可选正则；Xray 的 tag 全是 prerelease，/releases/latest 会卡在 v26.3.27，
  # 所以一律从 releases 列表取第一个匹配项（列表按创建时间倒序）
  gh_latest_tag() {
      local repo="$1" re="${2:-.}"
      curl -fsSL --max-time 15 "https://api.github.com/repos/${repo}/releases?per_page=30" 2>/dev/null \
          | grep -oE '"tag_name":\s*"[^"]+"' | sed -E 's/.*"([^"]+)"$/\1/' | grep -E "$re" | head -1
  }
  ```
  - Xray：`gh_latest_tag XTLS/Xray-core '^v[0-9]'` → 去 `v`。
  - sing-box：`latest=$(gh_latest_tag SagerNet/sing-box '^v[0-9]+\.[0-9]+\.[0-9]+$')`（排除 alpha/beta/rc）；若 `minor > SINGBOX_MAX_MINOR` 则 `gh_latest_tag SagerNet/sing-box '^v1\.14\.[0-9]+$'`，为空回退 latest 并 `print_warning`。
  - Hysteria2：保持现状。
- 改造点：`server/core.sh:1541-1549`（安装）、`server/update.sh` `update_kernel()`（~1708-1760）与 `auto_update_kernel()`（~1799-1850）、`b-ui-client.sh:1174`（`install_singbox` 回退）、`:4706-4748`（`update_all`）、`:5402-5431`（版本检查）、`server/residential-helper.sh:164`（`ensure_singbox`）、`web/server.js:1100`（`fetchLatestRelease`：Xray 用 `releases?per_page=30` 列表第一个；sing-box 加同样上限逻辑；常量 `SINGBOX_MAX_MINOR = "1.14"`）。
- 排除 draft：GitHub 列表默认不返回其他人的 draft；`grep tag_name` 即可。

## 4. 验收标准

1. `bash -n` 全部脚本；`node --check web/server.js`。
2. obfs：本地起 server.js，`config.yaml` 含 obfs 块 → `/api/clash/<fusion 用户>` 的 HY2直连 节点含 `obfs: salamander` 与 `obfs-password`，HY2住宅 不含；无 obfs 块时两者都不含。`parse_hysteria_uri` 对含 `obfs=salamander&obfs-password=p%40ss` 的 URI 输出 `OBFS_TYPE=salamander`、`OBFS_PASSWORD=p@ss`；生成的 Hysteria2 YAML 与 sing-box TUN 配置含对应 obfs（sing-box 三版 `check` 通过）；无 obfs 的 URI 不含。`b-ui obfs on` 输出含"重新"提示。
3. 版本：PATH 前置 `curl` stub 返回固定 JSON（Xray 列表首项 `v26.9.9` prerelease；sing-box latest `v1.15.0`、列表含 `v1.15.0`、`v1.14.3`、`v1.14.0`、`v1.15.0-alpha.2`），各文件的探测函数得到 Xray `26.9.9`、sing-box `1.14.3`；把上限改成 `1.15` 时得到 `1.15.0`；Hysteria 仍走 latest。
