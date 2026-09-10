# 客户端切换稳定性 + 双栈出口检测 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `bui-c` 开 TUN 后切换节点稳定；「连接测试」与 TUN 启动后的自检显示 IPv4/IPv6 出口 IP、归属地与机房判定（ippure.com）。

**Architecture:** 全部在 `b-ui-client.sh`。切换稳定性靠：健康巡检脚本 TUN 感知、systemd `Conflicts=`/`TimeoutStopSec`、`ensure_tun_config_ready` 用 `.node` 侧车替代死掉的 grep 比较、`start_tun_mode` 轮询、`stop_tun_mode --no-restore`。出口检测靠新函数 `probe_egress` + `print_egress_rows`，`test_proxy` 测试 6 与 `check_public_ip` 共用。

**Tech Stack:** bash + curl（无 jq 硬依赖，有 jq 优先）+ systemd。

**Spec:** `docs/superpowers/specs/2026-09-10-client-switch-nettest-design.md`

## Global Constraints

- 只动 `b-ui-client.sh`。不改 `version.json`（随 IPv6 计划 Task 5 统一 bump；changelog 由 Task 5 补两条）。
- 客户端脚本无 `set -e`；沿用 `print_*` 与既有注释风格（`# v3.6.0: …`）。
- 测试在 `scratchpad/client-tests/`，复用 `scratchpad/ipv6-client-tests/extract_fn.awk` 抽函数；不碰 `/opt/hysteria-client`，`systemctl`/`curl`/`ip`/`dig` 全部用 PATH 前置 stub。
- 每个 Task 一个提交：`fix(client): …`、`feat(client): …`，尾注：
  Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_017PpJPoZWnVfYKrCaxsoZa5
- 只提交不 push；显式 `git add b-ui-client.sh`；不 amend。

---

### Task 1: TUN 模式切换稳定性

**Files:**
- Modify: `b-ui-client.sh`：`create_health_check`（~3725）、`create_service` 的 unit 文本（~3691）、`generate_singbox_tun_config` 末尾侧车写入（~1590 附近写 `.schema` 处）、`ensure_tun_config_ready`（~1740-1830）、`start_tun_mode`（~1831-1897）、`stop_tun_mode`（~1904-1958）、`_switch_to_profile` 的 `stop_tun_mode` 调用（~2320）、bui-tun unit（~1605-1618）、xray-client unit（`create_xray_service` 或同类，grep `xray-client.service`）
- Test: `scratchpad/client-tests/test_switch_stability.sh`

**Interfaces:**
- Produces: 侧车文件 `${BASE_DIR}/singbox-tun.json.node`；`stop_tun_mode [--no-restore]`；健康脚本在 bui-tun 活动时退出 0。

- [ ] **Step 1: 写测试（预期失败）**

```bash
#!/bin/bash
set -u
S=/tmp/claude-1000/-home-roots-b-ui/71917ef4-b1f0-4466-927f-5df467756568/scratchpad
REPO=/home/roots/b-ui; CL="$REPO/b-ui-client.sh"; fail=0; T=$(mktemp -d); mkdir -p "$T/bin" "$T/base/configs/n1"
AWK="$S/ipv6-client-tests/extract_fn.awk"
ext() { awk -f "$AWK" -v fn="$1" "$CL"; }
# systemctl stub：记录调用；is-active 结果由环境 ACTIVE_UNITS(逗号分隔)决定；可用 ISACTIVE_FAIL_N 让前 N 次 is-active 失败
cat > "$T/bin/systemctl" <<'STUB'
#!/bin/bash
echo "$@" >> "${STUB_LOG}"
if [[ "$1" == is-active ]]; then
  u="${@: -1}"
  n=$(cat "${STUB_DIR}/n_$u" 2>/dev/null || echo 0); n=$((n+1)); echo $n > "${STUB_DIR}/n_$u"
  [[ -n "${ISACTIVE_FAIL_N:-}" && "$u" == bui-tun && $n -le ${ISACTIVE_FAIL_N} ]] && exit 3
  [[ ",${ACTIVE_UNITS:-}," == *",$u,"* ]] && exit 0 || exit 3
fi
exit 0
STUB
chmod +x "$T/bin/systemctl"; export STUB_LOG="$T/sc.log" STUB_DIR="$T"; export PATH="$T/bin:$PATH"
for c in ip dig journalctl sysctl; do printf '#!/bin/bash\nexit 0\n' > "$T/bin/$c"; chmod +x "$T/bin/$c"; done
lib() { { grep -E '^(RED|GREEN|YELLOW|BLUE|CYAN|NC|DIM|BOLD)=' "$CL" | head -8; echo "BASE_DIR='$T/base'; CONFIGS_DIR='$T/base/configs'; ACTIVE_CONFIG='$T/base/active'; CONFIG_FILE='$T/base/config.yaml'; CLIENT_SERVICE=hysteria-client.service; TUN_SCHEMA_VERSION=7"; for f in print_info print_success print_warning print_error "$@"; do ext "$f"; done; } > "$T/lib.sh"; }

# A. 健康脚本 TUN 感知：抽 create_health_check，让它把脚本写到 $T，然后在 bui-tun 活动/不活动两种情况下跑
lib create_health_check
( source "$T/lib.sh"; HEALTH_SCRIPT="$T/health.sh" create_health_check ) >/dev/null 2>&1   # 若函数硬编码路径，测试里 sed 替换成 $T/health.sh 后再 source
[[ -f "$T/health.sh" ]] || { echo "FAIL A 未生成健康脚本(路径需可覆盖)"; fail=1; }
: > "$STUB_LOG"; ACTIVE_UNITS=bui-tun bash "$T/health.sh" >/dev/null 2>&1
grep -q 'restart' "$STUB_LOG" && { echo "FAIL A TUN 活动时不该重启 hysteria-client"; fail=1; }
: > "$STUB_LOG"; ACTIVE_UNITS= bash "$T/health.sh" >/dev/null 2>&1
grep -q 'restart hysteria-client.service' "$STUB_LOG" || { echo "FAIL A 非 TUN 时应重启"; fail=1; }

# B. unit 文本：抽 create_service 与写 bui-tun unit 的函数，生成到 $T/sysd（需函数支持路径覆盖或测试 sed 替换 /etc/systemd/system → $T/sysd）
mkdir -p "$T/sysd"
lib create_service generate_singbox_tun_config json_escape host_ipv6_enabled
sed -i "s#/etc/systemd/system#$T/sysd#g" "$T/lib.sh"
echo 'listen: 127.0.0.1:1080' > "$T/base/config.yaml"
( source "$T/lib.sh"; SERVER_ADDR=proxy.example.com:443 AUTH_PASSWORD=x SOCKS_PORT=1080 HTTP_PORT=8080 create_service; BUI_FORCE_IPV6=0 SERVER_ADDR=proxy.example.com:443 AUTH_PASSWORD=x SOCKS_PORT=1080 HTTP_PORT=8080 generate_singbox_tun_config hysteria2 ) >/dev/null 2>&1
grep -q 'Conflicts=bui-tun.service' "$T/sysd/hysteria-client.service" || { echo "FAIL B hysteria-client 缺 Conflicts"; fail=1; }
grep -q 'Conflicts=hysteria-client.service xray-client.service' "$T/sysd/bui-tun.service" && grep -q 'TimeoutStopSec=10' "$T/sysd/bui-tun.service" || { echo "FAIL B bui-tun unit"; fail=1; }

# C. ensure_tun_config_ready 侧车判定
lib ensure_tun_config_ready get_active_config
echo 'generate_singbox_tun_config(){ echo GEN >> "'"$T"'/gen.log"; echo "{}" > "'"$T"'/base/singbox-tun.json"; echo 7 > "'"$T"'/base/singbox-tun.json.schema"; echo "$(cat '"$T"'/base/active)" > "'"$T"'/base/singbox-tun.json.node"; }' >> "$T/lib.sh"
echo n1 > "$T/base/active"; echo 'hysteria2://u:p@proxy.example.com:443?sni=proxy.example.com#n1' > "$T/base/configs/n1/uri.txt"; echo '{"server":"proxy.example.com:443","protocol":"hysteria2"}' > "$T/base/configs/n1/meta.json"
echo '{"dns":{"servers":[{"server":"1.1.1.1"}]},"outbounds":[{"tag":"proxy-out","server":"93.184.216.34","server_port":443}]}' > "$T/base/singbox-tun.json"; echo 7 > "$T/base/singbox-tun.json.schema"; echo n1 > "$T/base/singbox-tun.json.node"
: > "$T/gen.log"; ( source "$T/lib.sh"; ensure_tun_config_ready ) >/dev/null 2>&1
[[ -s "$T/gen.log" ]] && { echo "FAIL C1 侧车一致仍重生成"; fail=1; }
echo n0 > "$T/base/singbox-tun.json.node"; : > "$T/gen.log"; ( source "$T/lib.sh"; ensure_tun_config_ready ) >/dev/null 2>&1
[[ -s "$T/gen.log" ]] || { echo "FAIL C2 节点变化未重生成"; fail=1; }
rm -f "$T/base/singbox-tun.json.node"; : > "$T/gen.log"; ( source "$T/lib.sh"; ensure_tun_config_ready ) >/dev/null 2>&1
[[ -s "$T/gen.log" ]] || { echo "FAIL C3 缺侧车未重生成"; fail=1; }

# D. start_tun_mode 轮询：前 3 次 is-active 失败、第 4 次成功 → 成功；一直失败 → 失败
lib start_tun_mode ensure_tun_config_ready get_active_config check_public_ip host_ipv6_enabled
echo 'ensure_tun_config_ready(){ return 0; }; check_public_ip(){ :; }' >> "$T/lib.sh"
: > "$STUB_LOG"; rm -f "$T"/n_*; ( source "$T/lib.sh"; ISACTIVE_FAIL_N=3 ACTIVE_UNITS=bui-tun start_tun_mode ) >/dev/null 2>&1; rc=$?
[[ $rc == 0 ]] || { echo "FAIL D1 轮询应在第 4 次成功 rc=$rc"; fail=1; }
: > "$STUB_LOG"; rm -f "$T"/n_*; ( source "$T/lib.sh"; ACTIVE_UNITS= start_tun_mode ) > "$T/d2.out" 2>&1; rc=$?
[[ $rc != 0 ]] && grep -q 'journalctl' "$STUB_LOG" || { echo "FAIL D2 失败时应提示 journalctl"; fail=1; }

# E. stop_tun_mode --no-restore
lib stop_tun_mode setup_system_proxy
: > "$STUB_LOG"; ( source "$T/lib.sh"; ACTIVE_UNITS=bui-tun stop_tun_mode --no-restore ) >/dev/null 2>&1
grep -q 'start hysteria-client' "$STUB_LOG" && { echo "FAIL E --no-restore 仍重启 hysteria-client"; fail=1; }
: > "$STUB_LOG"; ( source "$T/lib.sh"; ACTIVE_UNITS=bui-tun stop_tun_mode ) >/dev/null 2>&1
grep -q 'start hysteria-client' "$STUB_LOG" || { echo "FAIL E 默认应恢复 hysteria-client"; fail=1; }
rm -rf "$T"; [[ $fail == 0 ]] && echo "PASS switch stability" || exit 1
```

函数抽取范围与依赖按实际报错补齐（`start_tun_mode` 内的 `ufw`/`sysctl`/`ip link` 等外部命令都用 stub）。若 `create_health_check`/unit 写入路径硬编码，允许在函数里引入 `HEALTH_SCRIPT="${HEALTH_SCRIPT:-/opt/hysteria-client/health-check.sh}"` 与 `SYSTEMD_DIR="${SYSTEMD_DIR:-/etc/systemd/system}"` 两个可覆盖变量（默认值不变）。

- [ ] **Step 2: 运行确认失败** → Expected: A（TUN 活动时仍 restart）、B（缺 Conflicts/TimeoutStopSec）、C1（侧车一致仍重生成）、D1（轮询）、E（无 `--no-restore`）、F（toggle_tun 丢 obfs）FAIL。

- [ ] **Step 3: 实现**（按 spec §2.1 六条；要点）

- 健康脚本开头：
  ```bash
  # v3.6.0: TUN 模式由 bui-tun 接管端口，hysteria-client 本该停着；巡检拉起它会和 sing-box 抢 1080/8080
  if systemctl is-active --quiet bui-tun 2>/dev/null; then echo "[$(date '+%F %T')] TUN 模式，跳过 hysteria-client 巡检" >> "$LOG"; exit 0; fi
  ```
- unit 文本：bui-tun `[Unit]` 加 `Conflicts=hysteria-client.service xray-client.service`，`[Service]` 加 `TimeoutStopSec=10`；hysteria-client / xray-client `[Unit]` 加 `Conflicts=bui-tun.service`；写完 `systemctl daemon-reload`。
- 侧车：`generate_singbox_tun_config` 成功写入配置后，`get_active_config > "${singbox_config}.node"`（与 `.schema` 同处）；`ensure_tun_config_ready` 的判定改为 `[[ ! -f "$tun_cfg" ]] || [[ "$(cat "$tun_cfg.schema" 2>/dev/null)" != "$TUN_SCHEMA_VERSION" ]] || [[ "$(cat "$tun_cfg.node" 2>/dev/null)" != "$active" ]] || 关键字段为空 → needs_regen=true`；删除 `cfg_server` 与 `active_server` 的比较。
- `start_tun_mode`：
  ```bash
  systemctl start bui-tun
  local i ok=false
  for i in $(seq 1 10); do sleep 0.5; systemctl is-active --quiet bui-tun && { ok=true; break; }; done
  if ! $ok; then
      print_error "bui-tun 启动失败"; journalctl -u bui-tun -n 20 --no-pager 2>/dev/null | tail -20
      print_warning "若日志显示 IPv6 地址配置失败，可 BUI_FORCE_IPV6=0 后重试"
      return 1
  fi
  ```
  随后原有的 `ip link show bui-tun` 检查保留（若无则加）。
- `toggle_tun`（~3655-3692）开启分支：删掉从 `config.yaml` grep 变量并直接 `generate_singbox_tun_config` 的代码，改为 `rm -f "${BASE_DIR}/singbox-tun.json.node"; start_tun_mode`。测试 F：uri.txt 含 `obfs=salamander&obfs-password=x`、stub 的 `generate_singbox_tun_config` 把 `OBFS_TYPE` 写进 gen.log，调用 `toggle_tun` 开启分支后 gen.log 含 `OBFS_TYPE=salamander`。
- `stop_tun_mode`：`local restore=true; [[ "${1:-}" == "--no-restore" ]] && restore=false`；末尾"重启 hysteria-client + setup_system_proxy"段包在 `if $restore; then … fi`。`_switch_to_profile` 里的调用改 `stop_tun_mode --no-restore`。

- [ ] **Step 4: 验证并提交**

```bash
bash -n b-ui-client.sh && bash $S/client-tests/test_switch_stability.sh && bash $S/ipv6-client-tests/test_client_tun.sh && bash $S/ipv6-client-tests/test_bootstrap_ip.sh
git add b-ui-client.sh
git commit -m "fix(client): TUN 模式切换节点稳定性(巡检 TUN 感知/Conflicts+TimeoutStopSec/侧车判定不再重复生成/启动轮询/切换不重启 hysteria-client/toggle_tun 走 uri.txt 重解析)"
```

---

### Task 2: 双栈出口检测（ippure.com + ip-api）

**Files:**
- Modify: `b-ui-client.sh`：新增 `probe_egress()`、`print_egress_rows()`；改 `test_proxy` 测试 6（~4231-4266）与 `check_public_ip`（~1655-1728）
- Test: `scratchpad/client-tests/test_probe_egress.sh`

**Interfaces:**
- Produces: `probe_egress <4|6> [socks_port]` → stdout `key=value` 行（`ip country region city org type score source`）；`print_egress_rows <tun_running:true|false> [socks_port]`。

- [ ] **Step 1: 写测试（预期失败）**

```bash
#!/bin/bash
set -u
S=/tmp/claude-1000/-home-roots-b-ui/71917ef4-b1f0-4466-927f-5df467756568/scratchpad
REPO=/home/roots/b-ui; CL="$REPO/b-ui-client.sh"; fail=0; T=$(mktemp -d); mkdir -p "$T/bin"
AWK="$S/ipv6-client-tests/extract_fn.awk"; ext() { awk -f "$AWK" -v fn="$1" "$CL"; }
{ grep -E '^(RED|GREEN|YELLOW|BLUE|CYAN|NC|DIM|BOLD)=' "$CL" | head -8; for f in print_info print_success print_warning print_error probe_egress print_egress_rows; do ext "$f"; done; } > "$T/lib.sh"
# curl stub：按 -4/-6 与 URL 返回；MODE 控制：ippure_ok / ippure_fail / v6_ok / v6_fail
cat > "$T/bin/curl" <<'STUB'
#!/bin/bash
a="$*"
if [[ "$a" == *"my.ippure.com"* ]]; then
  [[ "${MODE:-}" == *ippure_fail* ]] && exit 7
  echo '{"ip":"199.19.108.8","asn":25820,"asOrganization":"Cluster Logic Inc","country":"United States","countryCode":"US","region":"California","city":"Los Angeles","fraudScore":12,"isResidential":false}'; exit 0
fi
if [[ "$a" == *"api6.ipify.org"* || "$a" == *"ipv6.icanhazip.com"* ]]; then [[ "${MODE:-}" == *v6_ok* ]] && { echo "2001:db8::1234"; exit 0; } || exit 7; fi
if [[ "$a" == *"ip-api.com/json/2001:db8::1234"* ]]; then echo '{"status":"success","country":"Japan","regionName":"Tokyo","city":"Tokyo","isp":"NTT","org":"NTT","as":"AS2914","mobile":false,"proxy":false,"hosting":false}'; exit 0; fi
if [[ "$a" == *"ip-api.com/json/?"* ]]; then echo '{"status":"success","country":"Germany","regionName":"Hesse","city":"Frankfurt","isp":"Hetzner","org":"Hetzner","as":"AS24940","mobile":false,"proxy":false,"hosting":true,"query":"5.6.7.8"}'; exit 0; fi
exit 7
STUB
chmod +x "$T/bin/curl"; export PATH="$T/bin:$PATH"
run4() { bash -c "source '$T/lib.sh'; probe_egress 4 ${1:-}"; }
run6() { bash -c "source '$T/lib.sh'; probe_egress 6 ${1:-}"; }
# A. ippure 成功
out=$(MODE=ippure_ok run4)
grep -qx 'ip=199.19.108.8' <<<"$out" && grep -qx 'source=ippure' <<<"$out" && grep -q '^type=.*机房' <<<"$out" && grep -qx 'score=12' <<<"$out" && grep -q '^country=United States' <<<"$out" || { echo "FAIL A: $out"; fail=1; }
# B. ippure 失败 → ip-api 回退
out=$(MODE=ippure_fail run4)
grep -qx 'ip=5.6.7.8' <<<"$out" && grep -qx 'source=ip-api' <<<"$out" && grep -q '^type=.*机房' <<<"$out" || { echo "FAIL B: $out"; fail=1; }
# C. IPv6 成功 → 地址 + ip-api 按地址归属
out=$(MODE=v6_ok run6)
grep -qx 'ip=2001:db8::1234' <<<"$out" && grep -q '^country=Japan' <<<"$out" && grep -q '^type=.*家庭宽带' <<<"$out" || { echo "FAIL C: $out"; fail=1; }
# D. IPv6 失败 → ip 空
out=$(MODE=v6_fail run6); grep -qx 'ip=' <<<"$out" || { echo "FAIL D: $out"; fail=1; }
# E. socks 参数透传：stub 记录 argv 含 --socks5-hostname 127.0.0.1:1080
cat > "$T/bin/curl" <<'STUB'
#!/bin/bash
echo "$*" >> "${ARGLOG}"; exit 7
STUB
chmod +x "$T/bin/curl"; export ARGLOG="$T/args"; : > "$ARGLOG"; MODE= run4 1080 >/dev/null; grep -q -- '--socks5-hostname 127.0.0.1:1080' "$ARGLOG" || { echo "FAIL E socks 未透传"; fail=1; }
# F. 渲染分支：TUN 且无 v6 → "无泄漏"；TUN 有 v6 → "泄漏"
cat > "$T/bin/curl" <<'STUB'
#!/bin/bash
a="$*"; [[ "$a" == *"my.ippure.com"* ]] && { echo '{"ip":"199.19.108.8","asOrganization":"X","country":"US","region":"CA","city":"LA","fraudScore":1,"isResidential":true}'; exit 0; }
[[ "$a" == *"api6.ipify.org"* ]] && { [[ "${MODE:-}" == v6_ok ]] && { echo "2001:db8::1"; exit 0; }; exit 7; }
[[ "$a" == *"ip-api.com/json/2001"* ]] && { echo '{"status":"success","country":"JP","regionName":"","city":"","isp":"NTT","org":"","as":"","mobile":false,"proxy":false,"hosting":false}'; exit 0; }; exit 7
STUB
chmod +x "$T/bin/curl"
out=$(MODE=v6_fail bash -c "source '$T/lib.sh'; print_egress_rows true"); grep -q '无泄漏' <<<"$out" || { echo "FAIL F1: $out"; fail=1; }
out=$(MODE=v6_ok bash -c "source '$T/lib.sh'; print_egress_rows true"); grep -q '泄漏' <<<"$out" && ! grep -q '无泄漏' <<<"$out" || { echo "FAIL F2: $out"; fail=1; }
out=$(MODE=v6_ok bash -c "source '$T/lib.sh'; print_egress_rows false 1080"); grep -q 'SOCKS' <<<"$out" || { echo "FAIL F3: $out"; fail=1; }
rm -rf "$T"; [[ $fail == 0 ]] && echo "PASS probe egress" || exit 1
```

- [ ] **Step 2: 运行确认失败** → Expected: 函数不存在，全部 FAIL。

- [ ] **Step 3: 实现**（按 spec §2.2）

`probe_egress()` 骨架：

```bash
# v3.6.0: 出口 IP 检测。IPv4 用 ippure.com 公开接口(仅 IPv4, isResidential/fraudScore)，失败回退 ip-api；
# IPv6 先取地址(api6.ipify.org → ipv6.icanhazip.com)，再用 ip-api 按地址查归属/类型（ip-api 免费层无 IPv6 传输）
probe_egress() {
    local fam="$1" sp="${2:-}" proxy=()
    [[ -n "$sp" ]] && proxy=(--socks5-hostname "127.0.0.1:${sp}")
    local ip="" country="" region="" city="" org="" type="" score="" source=""
    if [[ "$fam" == "4" ]]; then
        local j; j=$(curl -4 -sL --max-time 8 "${proxy[@]}" https://my.ippure.com/v1/info 2>/dev/null)
        if [[ "$j" == *'"ip"'* && "$j" == *'"isResidential"'* ]]; then
            ip=$(_jf "$j" ip); country=$(_jf "$j" country); region=$(_jf "$j" region); city=$(_jf "$j" city)
            org=$(_jf "$j" asOrganization); score=$(_jf "$j" fraudScore); source=ippure
            [[ "$(_jf "$j" isResidential)" == "true" ]] && type="家庭宽带 IP（住宅）" || type="IDC 机房 IP（数据中心）"
        else
            j=$(curl -4 -sL --max-time 8 "${proxy[@]}" 'http://ip-api.com/json/?fields=status,country,regionName,city,isp,org,as,mobile,proxy,hosting,query' 2>/dev/null)
            [[ "$(_jf "$j" status)" == "success" ]] && { ip=$(_jf "$j" query); country=$(_jf "$j" country); region=$(_jf "$j" regionName); city=$(_jf "$j" city); org=$(_jf "$j" isp); source=ip-api; type=$(_ipapi_type "$j"); }
        fi
    else
        ip=$(curl -6 -sS --max-time 6 "${proxy[@]}" https://api6.ipify.org 2>/dev/null | tr -d '[:space:]')
        [[ "$ip" == *:* ]] || ip=$(curl -6 -sS --max-time 6 "${proxy[@]}" https://ipv6.icanhazip.com 2>/dev/null | tr -d '[:space:]')
        [[ "$ip" == *:* ]] || ip=""
        if [[ -n "$ip" ]]; then
            local j; j=$(curl -4 -sL --max-time 8 "http://ip-api.com/json/${ip}?fields=status,country,regionName,city,isp,org,as,mobile,proxy,hosting" 2>/dev/null)
            [[ "$(_jf "$j" status)" == "success" ]] && { country=$(_jf "$j" country); region=$(_jf "$j" regionName); city=$(_jf "$j" city); org=$(_jf "$j" isp); type=$(_ipapi_type "$j"); source=ip-api; }
        fi
    fi
    printf 'ip=%s\ncountry=%s\nregion=%s\ncity=%s\norg=%s\ntype=%s\nscore=%s\nsource=%s\n' "$ip" "$country" "$region" "$city" "$org" "$type" "$score" "$source"
}
```

`_jf json key`：有 jq 用 `jq -r --arg k "$key" '.[$k] // empty'`，否则 `sed -n 's/.*"key":[[:space:]]*"\{0,1\}\([^",}]*\)"\{0,1\}.*/\1/p'`（布尔/数字无引号）。`_ipapi_type`：`hosting→IDC 机房 IP（数据中心）`、`proxy→代理 IP`、`mobile→移动网络 IP`、否则 `家庭宽带 IP（住宅）`（沿用 test_proxy 现有措辞）。`print_egress_rows tun socks_port`：调用两次 `probe_egress`，按 spec §2.2 四种 IPv6 文案输出；IPv4 行含 `country·city`、org、`[type]`、`风险分 N`（仅 ippure）与 `(source)`。`test_proxy` 测试 6 替换为 `print_egress_rows "$tun_running" "$socks_port"`（TUN 时 socks_port 传空）；`check_public_ip` 的 IP/归属部分替换为 `print_egress_rows true`，其后的 google/youtube/github 可达性检查保留。

- [ ] **Step 4: 验证并提交**

```bash
bash -n b-ui-client.sh && bash $S/client-tests/test_probe_egress.sh && bash $S/client-tests/test_switch_stability.sh && bash $S/ipv6-client-tests/test_client_tun.sh
git add b-ui-client.sh
git commit -m "feat(client): 连接测试与 TUN 自检显示 IPv4/IPv6 出口 IP、归属地与机房判定(ippure.com, 回退 ip-api)"
```
