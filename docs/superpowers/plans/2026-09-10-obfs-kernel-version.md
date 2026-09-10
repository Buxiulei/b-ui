# salamander obfs 补齐 + 内核版本探测修正 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 开了 salamander obfs 的服务器上，Clash 订阅与 Linux 客户端不再静默连不上；Xray 版本探测不再卡在 v26.3.27；sing-box 自动升级不超过 1.14。

**Architecture:** obfs 沿 URI → `parse_hysteria_uri` → meta.json → 两份客户端配置的既有数据流补字段；Clash 生成器与 sing-box 生成器同法。版本探测在每个独立分发的脚本里各放一个 `gh_latest_tag` 小函数，`web/server.js` 的 `fetchLatestRelease` 内部分支。

**Tech Stack:** bash + curl + grep/sed；Node.js ESM。

**Spec:** `docs/superpowers/specs/2026-09-10-obfs-kernel-version-design.md`

## Global Constraints

- Task A 只动 `web/server.js`（Clash 生成器）、`b-ui-client.sh`、`server/b-ui-cli.sh`；Task B 只动 `server/core.sh`、`server/update.sh`、`b-ui-client.sh`、`server/residential-helper.sh`、`web/server.js`（`fetchLatestRelease`）。不改 `version.json`。
- obfs 只加在直连 HY2（`config.yaml` 的 obfs 块），住宅 HY2 不带。
- 版本探测：Xray 用 releases 列表首个 `tag_name`（含 prerelease）；sing-box latest 高于 `SINGBOX_MAX_MINOR="1.14"` 时改取列表中第一个 `^v1\.14\.` tag，无则回退并警告；Hysteria2 不变。
- Surgical；每个 Task 一个提交（`fix(obfs): …`、`fix(kernel): …`），尾注：
  Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_01GqNZ1FqiwWYBXC3CGmBqsU
- 只提交不 push；显式 `git add`。测试放 `scratchpad/obfs-tests/` 与 `scratchpad/kernel-tests/`；不碰 `/opt/b-ui`。sing-box 三版二进制：`/usr/bin/sing-box`、`scratchpad/singbox-bins/v1.14.0/sing-box`、`scratchpad/singbox-bins/v1.15.0-alpha.2/sing-box`。

---

### Task A: salamander obfs 全链路补齐

**Files:**
- Modify: `web/server.js`（`generateClashConfig` 的 `mkHy2` 与两处直连/住宅调用）
- Modify: `b-ui-client.sh`（`parse_hysteria_uri`；节点 meta 保存/恢复；`generate_config`；`generate_singbox_tun_config` hysteria2 出站）
- Modify: `server/b-ui-cli.sh`（`cmd_obfs on` 分支提示）
- Test: `scratchpad/obfs-tests/test_obfs_clash.sh`、`scratchpad/obfs-tests/test_obfs_client.sh`

**Interfaces:**
- Produces: `parse_hysteria_uri` 新输出行 `OBFS_TYPE=`、`OBFS_PASSWORD=`；meta.json 新字段 `obfs_type`、`obfs_password`（命名跟随文件既有风格，若既有字段用驼峰则用 `obfsType`/`obfsPassword`）。

- [ ] **Step 1: 写测试（预期失败）**

`test_obfs_clash.sh`（复用 `scratchpad/ipv6-tests/run_server.sh`，端口 18081；先读它确认 `config.yaml` 由参数写入）：

```bash
#!/bin/bash
set -u
S=/tmp/claude-1000/-home-roots-b-ui/71917ef4-b1f0-4466-927f-5df467756568/scratchpad
fail=0
W="$S/obfs-tests/w_clash"; rm -rf "$W"; bash "$S/ipv6-tests/run_server.sh" "$W" hop:on resi:list
# 注入 obfs 块后让 getConfig 重新读取（每次请求都读文件）
printf '# obfs\nobfs:\n  type: salamander\n  salamander:\n    password: "s3cret"\n' | cat - "$W/config.yaml" > "$W/c.tmp" && mv "$W/c.tmp" "$W/config.yaml"
y=$(curl -s http://127.0.0.1:18081/api/clash/alice)
direct=$(awk '/name: .*HY2直连/{f=1} f&&/^  - name:/&&!/HY2直连/{f=0} f' <<<"$y")
resi=$(awk '/name: .*HY2住宅/{f=1} f&&/^  - name:/&&!/HY2住宅/{f=0} f' <<<"$y")
grep -q 'obfs: salamander' <<<"$direct" && grep -q 'obfs-password: "s3cret"' <<<"$direct" || { echo "FAIL 直连缺 obfs: $direct"; fail=1; }
grep -q 'obfs' <<<"$resi" && { echo "FAIL 住宅不该有 obfs"; fail=1; }
# 去掉 obfs 块 → 都不含
sed -i '1,5d' "$W/config.yaml"
y=$(curl -s http://127.0.0.1:18081/api/clash/alice); grep -q 'obfs' <<<"$y" && { echo "FAIL 无 obfs 时不该输出"; fail=1; }
kill "$(cat "$W/pid")" 2>/dev/null
[[ $fail == 0 ]] && echo "PASS obfs clash" || exit 1
```

Clash YAML 里节点是 `proxies:` 下的 `- name: …` 多行块；按实际输出形态调整 awk 截取（目标是分别拿到直连与住宅两个节点块）。

`test_obfs_client.sh`（复用 `scratchpad/ipv6-client-tests/gen_client_tun.sh` 与其 awk 抽取器；再抽 `parse_hysteria_uri`、`generate_config`）：

```bash
#!/bin/bash
set -u
S=/tmp/claude-1000/-home-roots-b-ui/71917ef4-b1f0-4466-927f-5df467756568/scratchpad
REPO=/home/roots/b-ui; fail=0; T=$(mktemp -d)
# A. 解析
awk -f "$S/ipv6-client-tests/extract_fn.awk" -v fn=parse_hysteria_uri "$REPO/b-ui-client.sh" > "$T/p.sh" 2>/dev/null || sed -n '/^parse_hysteria_uri() {/,/^}/p' "$REPO/b-ui-client.sh" > "$T/p.sh"
out=$(bash -c "source '$T/p.sh'; parse_hysteria_uri 'hysteria2://u:pw@proxy.example.com:443?sni=proxy.example.com&insecure=0&mport=20000-30000&obfs=salamander&obfs-password=p%40ss#n'")
grep -qx 'OBFS_TYPE=salamander' <<<"$out" || { echo "FAIL A type: $out"; fail=1; }
grep -qx 'OBFS_PASSWORD=p@ss' <<<"$out" || { echo "FAIL A pass"; fail=1; }
out=$(bash -c "source '$T/p.sh'; parse_hysteria_uri 'hysteria2://u:pw@proxy.example.com:443?sni=proxy.example.com#n'")
grep -qx 'OBFS_TYPE=' <<<"$out" || { echo "FAIL A2 no-obfs 应输出空"; fail=1; }
# B. sing-box TUN 出站含 obfs
export OBFS_TYPE=salamander OBFS_PASSWORD='p@ss'
bash "$S/ipv6-client-tests/gen_client_tun.sh" hysteria2 "$T/tun.json" 1 || { echo "FAIL B gen"; fail=1; }
jq -e '.outbounds[]|select(.tag=="proxy-out")|.obfs=={"type":"salamander","password":"p@ss"}' "$T/tun.json" >/dev/null || { echo "FAIL B obfs 字段"; fail=1; }
"$S/ipv6-client-tests/check_singbox.sh" "$T/tun.json" || fail=1
unset OBFS_TYPE OBFS_PASSWORD
bash "$S/ipv6-client-tests/gen_client_tun.sh" hysteria2 "$T/tun2.json" 1
jq -e '.outbounds[]|select(.tag=="proxy-out")|has("obfs")|not' "$T/tun2.json" >/dev/null || { echo "FAIL B2 不该有 obfs"; fail=1; }
# C. Hysteria2 原生 YAML（generate_config）含 obfs 段：按 gen_client_tun.sh 的方式抽 generate_config 与其依赖，设置 OBFS_* 后生成 $BASE_DIR/config.yaml 并 grep
rm -rf "$T"
[[ $fail == 0 ]] && echo "PASS obfs client" || exit 1
```

`gen_client_tun.sh` 若不透传 `OBFS_*` 环境变量（它 export 固定变量集），补上 `OBFS_TYPE`/`OBFS_PASSWORD` 的透传。步骤 C 的具体抽取按 `generate_config()` 依赖的函数补齐（`print_*`、`json_escape`、可能的 `create_default_rules`），断言 `config.yaml` 含 `obfs:` / `type: salamander` / `password: p@ss`。

- [ ] **Step 2: 运行，确认失败**

Run: `bash $S/obfs-tests/test_obfs_clash.sh; bash $S/obfs-tests/test_obfs_client.sh`
Expected: Clash 直连缺 obfs → FAIL；解析无 OBFS_* 行 → FAIL；TUN 无 obfs 字段 → FAIL。

- [ ] **Step 3: 实现**

`web/server.js` `generateClashConfig()`：

```js
    const mkHy2 = (name, port, hopStart, hopEnd, useObfs) => {
        …现有字段…
        const obfsLines = (useObfs && cfg.obfs?.enabled && cfg.obfs.type === "salamander" && cfg.obfs.password)
            ? `\n    obfs: salamander\n    obfs-password: ${JSON.stringify(cfg.obfs.password)}` : "";
        …把 obfsLines 拼进节点块…
    };
```

直连调用传 `true`，住宅传 `false`（对齐 `generateSingboxConfig` 的 `useObfs`）。

`b-ui-client.sh`：
- `parse_hysteria_uri()`：在 mport 之后加
  ```bash
    # v3.6.0: obfs salamander（服务端 b-ui obfs on 后订阅带 obfs 参数）
    local obfs_type="" obfs_password=""
    [[ "$query_part" =~ obfs=([^&]+) ]] && obfs_type="${BASH_REMATCH[1]}"
    [[ "$query_part" =~ obfs-password=([^&]+) ]] && obfs_password=$(echo -e "${BASH_REMATCH[1]//%/\\x}")
  ```
  输出 `echo "OBFS_TYPE=$obfs_type"`、`echo "OBFS_PASSWORD=$obfs_password"`。
- 找到 `MPORT` 被写入/读出 meta.json 的所有位置（`grep -n 'MPORT\|mport' b-ui-client.sh`），同处补 `OBFS_TYPE`/`OBFS_PASSWORD`（json_escape 密码）。
- `generate_config()`：在 `tls:` 段之后（YAML 顶层）追加
  ```bash
    local obfs_block=""
    if [[ "${OBFS_TYPE:-}" == "salamander" && -n "${OBFS_PASSWORD:-}" ]]; then
        obfs_block=$'obfs:\n  type: salamander\n  salamander:\n    password: '"$(printf '%s' "$OBFS_PASSWORD" | sed 's/"/\\"/g' | sed 's/^/"/;s/$/"/')"$'\n'
    fi
  ```
  写进 heredoc（用 `${obfs_block}` 占位；若 heredoc 是 quoted 则改成拼接）。
- `generate_singbox_tun_config()` hysteria2 出站：与 `hop_config` 同法生成
  ```bash
        local obfs_config=""
        if [[ "${OBFS_TYPE:-}" == "salamander" && -n "${OBFS_PASSWORD:-}" ]]; then
            obfs_config=",
      \"obfs\": { \"type\": \"salamander\", \"password\": \"$(json_escape "$OBFS_PASSWORD")\" }"
        fi
  ```
  插在 `"password": "${safe_password}"${hop_config}${obfs_config},` 位置。

`server/b-ui-cli.sh` `cmd_obfs on` 成功分支：`print_warning "客户端订阅链接/节点需要重新生成或重新导入（新增 obfs 参数）"`。

- [ ] **Step 4: 验证并提交**

```bash
bash -n b-ui-client.sh server/b-ui-cli.sh && node --check web/server.js
bash $S/obfs-tests/test_obfs_clash.sh && bash $S/obfs-tests/test_obfs_client.sh && bash $S/ipv6-client-tests/test_client_tun.sh && bash $S/ipv6-tests/test_subscription.sh
git add web/server.js b-ui-client.sh server/b-ui-cli.sh
git commit -m "fix(obfs): Clash 订阅与 Linux 客户端补齐 salamander obfs(开 obfs 后不再静默连不上)"
```

---

### Task B: 内核版本探测修正

**Files:**
- Modify: `server/core.sh:1541-1549`、`server/update.sh`（`update_kernel`、`auto_update_kernel`）、`b-ui-client.sh:1174`、`:4706-4748`、`:5402-5431`、`server/residential-helper.sh:164`、`web/server.js:1100`（`fetchLatestRelease`）
- Test: `scratchpad/kernel-tests/test_version_probe.sh`

**Interfaces:**
- Produces: bash 函数 `gh_latest_tag <repo> [tag_regex]`（每文件一份）；bash 常量 `SINGBOX_MAX_MINOR="1.14"`（每文件一份）；JS 常量 `SINGBOX_MAX_MINOR = "1.14"`。

- [ ] **Step 1: 写测试（预期失败）**

```bash
#!/bin/bash
set -u
REPO=/home/roots/b-ui; fail=0; T=$(mktemp -d); mkdir -p "$T/bin"
# curl stub：按 URL 返回固定 JSON
cat > "$T/bin/curl" <<'EOF'
#!/bin/bash
url="${@: -1}"
case "$url" in
  *Xray-core/releases/latest*) echo '{"tag_name": "v26.3.27", "prerelease": false}';;
  *Xray-core/releases*) echo '[{"tag_name": "v26.9.9", "prerelease": true, "draft": false},{"tag_name": "v26.9.8", "prerelease": true},{"tag_name": "v26.3.27", "prerelease": false}]';;
  *sing-box/releases/latest*) echo '{"tag_name": "v1.15.0"}';;
  *sing-box/releases*) echo '[{"tag_name": "v1.15.0"},{"tag_name": "v1.15.0-alpha.2"},{"tag_name": "v1.14.3"},{"tag_name": "v1.14.0"},{"tag_name": "v1.13.21"}]';;
  *hysteria/releases/latest*) echo '{"tag_name": "app/v2.12.2"}';;
  *) exit 22;;
esac
EOF
chmod +x "$T/bin/curl"; export PATH="$T/bin:$PATH"
extract() { sed -n "/^$2() {/,/^}/p" "$1"; }
for f in server/core.sh server/update.sh b-ui-client.sh server/residential-helper.sh; do
  { grep -E '^(RED|GREEN|YELLOW|BLUE|NC)=' "$REPO/$f" | head -5; grep -E '^SINGBOX_MAX_MINOR=' "$REPO/$f"; extract "$REPO/$f" gh_latest_tag; extract "$REPO/$f" print_warning; } > "$T/lib.sh"
  x=$(bash -c "source '$T/lib.sh' 2>/dev/null; gh_latest_tag XTLS/Xray-core '^v[0-9]'")
  [[ "$x" == "v26.9.9" ]] || { echo "FAIL $f xray tag: '$x'"; fail=1; }
  sb=$(bash -c "source '$T/lib.sh' 2>/dev/null; gh_latest_tag SagerNet/sing-box '^v1\.14\.[0-9]+$'")
  [[ "$sb" == "v1.14.3" ]] || { echo "FAIL $f sing-box capped tag: '$sb'"; fail=1; }
done
# server.js：fetchLatestRelease 是 async，写一个 node 单测：把函数用正则从源码抽出不可靠 → 改为起服务调用内核同步接口？
# 简化：node -e 导入不可行(server.js 有副作用)。改为静态断言 + 逻辑复用：server.js 必须包含 SINGBOX_MAX_MINOR 常量与 'releases?per_page' 字符串
grep -q 'SINGBOX_MAX_MINOR' "$REPO/web/server.js" && grep -q 'releases?per_page' "$REPO/web/server.js" || { echo "FAIL server.js 未改造"; fail=1; }
rm -rf "$T"
[[ $fail == 0 ]] && echo "PASS version probe" || exit 1
```

对 `web/server.js` 的行为验证：把 `fetchLatestRelease` 的选择逻辑抽成纯函数 `pickLatestTag(repo, releasesJson, latestJson)`（导出不必要，测试用 `node -e` 复制函数体调用即可），plan 允许这一处小重构。

- [ ] **Step 2: 运行，确认失败**

Run: `bash $S/kernel-tests/test_version_probe.sh`
Expected: 各文件无 `gh_latest_tag` → FAIL。

- [ ] **Step 3: 实现**

每个 bash 文件加（放在最先用到版本探测的函数前）：

```bash
# v3.6.0: 内核版本探测。Xray 的 tag 全部标 prerelease，/releases/latest 永远返回 v26.3.27（2026-03），
# 所以从 releases 列表（按创建时间倒序）取第一个匹配 tag；sing-box 限制在 SINGBOX_MAX_MINOR（1.15 起 1.14 弃用项变致命）
SINGBOX_MAX_MINOR="1.14"
gh_latest_tag() {
    local repo="$1" re="${2:-.}"
    curl -fsSL --max-time 15 "https://api.github.com/repos/${repo}/releases?per_page=30" 2>/dev/null \
        | grep -oE '"tag_name":[[:space:]]*"[^"]+"' | sed -E 's/.*"([^"]+)"$/\1/' | grep -E "$re" | head -1
}
singbox_latest_version() {
    local latest minor
    latest=$(gh_latest_tag SagerNet/sing-box '^v[0-9]+\.[0-9]+\.[0-9]+$' | sed 's/^v//')
    minor=$(echo "$latest" | cut -d. -f1-2)
    if [[ -n "$latest" ]] && [[ "$(printf '%s\n%s\n' "$SINGBOX_MAX_MINOR" "$minor" | sort -V | head -1)" == "$SINGBOX_MAX_MINOR" ]] && [[ "$minor" != "$SINGBOX_MAX_MINOR" ]]; then
        local capped
        capped=$(gh_latest_tag SagerNet/sing-box "^v${SINGBOX_MAX_MINOR//./\\.}\.[0-9]+$" | sed 's/^v//')
        if [[ -n "$capped" ]]; then echo "$capped"; return 0; fi
        print_warning "sing-box 最新 ${latest} 超过上限 ${SINGBOX_MAX_MINOR}.x 且找不到上限内版本，回退使用最新版" >&2
    fi
    echo "$latest"
}
xray_latest_version() { gh_latest_tag XTLS/Xray-core '^v[0-9]' | sed 's/^v//'; }
```

各探测点替换：Xray 的 `curl … releases/latest … sed` → `$(xray_latest_version)`；sing-box 的 → `$(singbox_latest_version)`；Hysteria 不动。`b-ui-client.sh:1174` 的 `install_singbox` 用 `singbox_latest_version`。`server/residential-helper.sh:164` 同理（该文件无 `print_warning`，用 `info`）。

`web/server.js` `fetchLatestRelease(repo)`：

```js
const SINGBOX_MAX_MINOR = "1.14";
// v3.6.0: Xray tag 全为 prerelease → 取 releases 列表首个；sing-box 限制 minor ≤ SINGBOX_MAX_MINOR
function pickLatestTag(repo, releases) {
    const tags = (Array.isArray(releases) ? releases : []).filter(r => !r.draft).map(r => r.tag_name);
    if (repo === "SagerNet/sing-box") {
        const stable = tags.filter(t => /^v\d+\.\d+\.\d+$/.test(t));
        const inCap = stable.find(t => t.startsWith(`v${SINGBOX_MAX_MINOR}.`));
        const latest = stable[0];
        if (latest && inCap && !latest.startsWith(`v${SINGBOX_MAX_MINOR}.`)) return inCap;
        return latest || null;
    }
    if (repo === "XTLS/Xray-core") return tags.find(t => /^v\d/.test(t)) || null;
    return tags.find(t => !/-(alpha|beta|rc)/.test(t)) || null;
}
```

`fetchLatestRelease` 对 Xray/sing-box 改请求 `releases?per_page=30` 并 `pickLatestTag`；hysteria 保持 `releases/latest`（tag 形如 `app/v2.12.2`，沿用现有解析）。用 `node -e` 复制 `pickLatestTag` 函数体验证：Xray 列表 → `v26.9.9`；sing-box 列表（latest v1.15.0）→ `v1.14.3`。

- [ ] **Step 4: 验证并提交**

```bash
bash -n server/core.sh server/update.sh b-ui-client.sh server/residential-helper.sh && node --check web/server.js
bash $S/kernel-tests/test_version_probe.sh
git add server/core.sh server/update.sh b-ui-client.sh server/residential-helper.sh web/server.js
git commit -m "fix(kernel): Xray 版本探测改用 releases 列表(不再卡在 v26.3.27) + sing-box 自动升级上限 1.14"
```


---

### Task C: update.sh 迁移块卫生（审查发现的既有重启风暴源 + D9 边角）

**Files:**
- Modify: `server/update.sh`（D6 块守卫；D9 `migrate_ipv4_only_egress()` 的 resi 守卫与恢复路径）；若 `server/core.sh apply_hy2_userpass_auth()` 有同型守卫则同改
- Test: `scratchpad/kernel-tests/test_update_hygiene.sh`

背景：IPv6 T3 审查发现 D6 的守卫 `grep -q '^  type: http' "$_cfg"` 也匹配 resolver 段的 `  type: https`（每份 hy2 配置都有），所以**每次** `apply_systemd_configs`（安装、升级、每 6 小时自愈）都重写 auth 块并重启两个 hysteria 实例——与 P0-3 同类的无谓重启源。另两条 D9 边角：resi direct 出站若已有 `direct:` 子块（手改）会被插入重复键导致 hysteria 拒绝配置；恢复路径每周期留一个新备份。

- [ ] **Step 1: 写测试（预期失败）**

```bash
#!/bin/bash
set -u
REPO=/home/roots/b-ui; fail=0; T=$(mktemp -d); mkdir -p "$T/bin"
cat > "$T/bin/systemctl" <<'STUB'
#!/bin/bash
echo "$@" >> "${STUB_LOG}"; exit 0
STUB
chmod +x "$T/bin/systemctl"; export STUB_LOG="$T/sc.log"; export PATH="$T/bin:$PATH"
extract() { sed -n "/^$1() {/,/^}/p" "$REPO/server/update.sh"; }
# A. D6 守卫：userpass 已就位 + resolver type: https → 不应重写、不应重启
mkdir -p "$T/base"; cat > "$T/base/config.yaml" <<'CFG'
listen: :10000
resolver:
  type: https
  https:
    addr: "1.1.1.1:443"
auth:
  type: userpass
  userpass:
    a: p

trafficStats:
  listen: 127.0.0.1:9999
CFG
echo '[{"username":"a","password":"p"}]' > "$T/base/users.json"
# 把 D6 块单独抽出包成函数（边界以文件里的 '# v3.5.15 D6' 与下一个 D 标记为准）
awk '/# v3.5.15 D6/{f=1} /# v3.5.17 D7/{f=0} f' "$REPO/server/update.sh" > "$T/d6.txt"
{ grep -E '^(RED|GREEN|YELLOW|BLUE|NC)=' "$REPO/server/update.sh" | head -5; echo "BASE_DIR='$T/base'"; for f in print_info print_success print_warning; do extract "$f"; done; echo 'run_d6(){ local updated=0'; cat "$T/d6.txt"; echo '; echo "updated=$updated"; }'; } > "$T/d6.sh"
: > "$STUB_LOG"; cp "$T/base/config.yaml" "$T/c0"
out=$(bash -c "source '$T/d6.sh'; run_d6" 2>&1)
cmp -s "$T/c0" "$T/base/config.yaml" || { echo "FAIL A D6 重写了已是 userpass 的配置"; fail=1; }
grep -q restart "$STUB_LOG" && { echo "FAIL A D6 无谓重启: $(cat $STUB_LOG)"; fail=1; }
# A2. 真正的 http auth 仍会迁移
printf 'listen: :10000\nresolver:\n  type: https\nauth:\n  type: http\n  http:\n    url: http://127.0.0.1:8080/auth/hysteria\n\ntrafficStats:\n  listen: 127.0.0.1:9999\n' > "$T/base/config.yaml"
: > "$STUB_LOG"; out=$(bash -c "source '$T/d6.sh'; run_d6" 2>&1)
grep -q 'type: userpass' "$T/base/config.yaml" && grep -q 'restart hysteria-server' "$STUB_LOG" || { echo "FAIL A2 http→userpass 未迁移: $out"; fail=1; }
# B. D9 resi 守卫：已有 direct: 子块(mode: 64) → 不插重复键
mkdir -p "$T/up"; printf 'listen: :40000\noutbounds:\n  - name: relay\n    type: socks5\n    socks5:\n      addr: "127.0.0.1:2080"\n  - name: direct\n    type: direct\n    direct:\n      mode: 64\n\nacl:\n  inline:\n    - relay(all)\n' > "$T/up/config-residential.yaml"
{ grep -E '^(RED|GREEN|YELLOW|BLUE|NC)=' "$REPO/server/update.sh" | head -5; echo "BASE_DIR='$T/up'"; for f in print_info print_success print_warning migrate_ipv4_only_egress; do extract "$f"; done; } > "$T/d9.sh"
( source "$T/d9.sh"; migrate_ipv4_only_egress ) >/dev/null 2>&1
[[ "$(grep -c '^    direct:$' "$T/up/config-residential.yaml")" == "1" ]] || { echo "FAIL B 重复 direct: 键"; fail=1; }
# C. 恢复路径不累积备份：构造无法识别的 resi 格式（direct 出站缺 type 行），跑两次
printf 'listen: :40000\noutbounds:\n  - name: relay\n    type: socks5\n  - name: direct\n\nacl:\n  inline:\n    - relay(all)\n' > "$T/up/config-residential.yaml"; rm -f "$T"/up/*.bak.v360.*
( source "$T/d9.sh"; migrate_ipv4_only_egress ) >/dev/null 2>&1; ( source "$T/d9.sh"; migrate_ipv4_only_egress ) >/dev/null 2>&1
n=$(ls "$T"/up/config-residential.yaml.bak.v360.* 2>/dev/null | wc -l); [[ "$n" -le 1 ]] || { echo "FAIL C 恢复路径累积备份: $n"; fail=1; }
rm -rf "$T"; [[ $fail == 0 ]] && echo "PASS update hygiene" || exit 1
```

- [ ] **Step 2: 运行确认失败** → Expected: A（重写+重启）、B（重复键）FAIL。

- [ ] **Step 3: 实现**

- D6：`grep -q '^  type: http' "$_cfg"` → `grep -qE '^  type: http$' "$_cfg"`（`core.sh apply_hy2_userpass_auth()` 有同型守卫则同改）。
- D9 resi 守卫：`! grep -qE '^\s+mode: 4$'` 之外再要求 `! grep -qE '^\s+direct:$' "$rcfg"`（已有 `direct:` 子块的手改配置跳过并 `print_warning` 提示手动加 `mode: 4`）。
- D9 恢复路径：恢复原文件时 `rm -f` 本轮刚创建的备份。

- [ ] **Step 4: 验证并提交**

```bash
bash -n server/update.sh server/core.sh && bash $S/kernel-tests/test_update_hygiene.sh && bash $S/restart-tests/test_auto_update.sh && bash $S/ipv6-tests/test_server_egress.sh
git add server/update.sh server/core.sh
git commit -m "fix(update): D6 守卫锚定 type: http\$ 不再每次自愈都重启 hysteria; D9 resi 已有 direct: 子块时跳过; 恢复路径不累积备份"
```
