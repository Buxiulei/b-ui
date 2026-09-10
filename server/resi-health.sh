#!/bin/bash
# B-UI 住宅线路可靠性监测（reliability-aware failover）
# 对每条住宅 SOCKS5 上游做真实连通探测，按成功率打分（带迟滞防抖）；relay 的 resi-pool 是
# selector，本脚本只在"当前选中的线路不健康"时经 Clash API 把它切到一条健康线路——健康就粘住，
# 既不改写 singbox-relay.json 也不重启 b-ui-relay（重启会掐断所有用户的现有连接）。
# 全部线路都不健康 → 不切，只 WARN（降级总比乱切好）。两次切换间隔有下限（默认 60s）。
# 由 b-ui-resi-health.timer 每 ~2 分钟触发。
#
# 关键：池成员(tag + socks 凭据)直接读自 relay 自身的 socks 出站——relay 是 selector 成员 tag 的
# 唯一真源（住宅 tag 是 resi-1/resi-2，不是 residential-proxy.json 里的 url-1/url-2，别搞混）。
set -u
BASE="${RESI_HEALTH_BASE_DIR:-/opt/b-ui}"
RELAY="$BASE/singbox-relay.json"
STATE="$BASE/.resi-health-state.json"
LOG="${RESI_HEALTH_LOG:-/var/log/b-ui-resi-health.log}"
API="${RESI_HEALTH_API:-127.0.0.1:9091}"
SWITCH_MIN="${RESI_HEALTH_SWITCH_MIN_INTERVAL:-60}"
PROBE_URL="${RESI_HEALTH_PROBE_URL:-https://www.gstatic.com/generate_204}"
# v3.6.0 R5: 一轮 2 次探测、任一成功即健康（迟滞仍是 2 轮），住宅按每 IP 请求速率限流
TRIES="${RESI_HEALTH_TRIES:-2}"
OK_NEED="${RESI_HEALTH_OK_NEED:-1}"
TIMEOUT="${RESI_HEALTH_TIMEOUT:-6}"
FAIL_TO_REMOVE="${RESI_HEALTH_FAIL_TO_REMOVE:-2}"
OK_TO_READD="${RESI_HEALTH_OK_TO_READD:-2}"
DRY_RUN="${RESI_HEALTH_DRY_RUN:-0}"

log(){ echo "[$(date '+%F %T')] $1" >> "$LOG" 2>/dev/null; [ "$DRY_RUN" = "1" ] && echo "$1"; }
# v3.6.0 R6: 决策段现在多处提前 exit（粘住/限速/API 不可达），日志轮转挂 EXIT 才不会漏
rotate_log(){ [ "$DRY_RUN" != "1" ] && { tail -300 "$LOG" > "${LOG}.tmp" 2>/dev/null && mv "${LOG}.tmp" "$LOG" 2>/dev/null; }; return 0; }
trap rotate_log EXIT

# v3.6.0 R3: curl 配置文件双引号内需转义 \ 与 "
curl_cfg_escape() { printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g'; }
# 生成 curl -K - 的配置：proxy 行 + 可选 proxy-user 行（凭据不出现在 argv 里）
curl_socks_cfg() {
    local host="$1" port="$2" user="$3" pass="$4"
    printf 'proxy = "socks5h://%s:%s"\n' "$(curl_cfg_escape "$host")" "$(curl_cfg_escape "$port")"
    [ -n "$user" ] && printf 'proxy-user = "%s:%s"\n' "$(curl_cfg_escape "$user")" "$(curl_cfg_escape "$pass")"
    return 0
}

command -v jq >/dev/null 2>&1 || exit 0
command -v curl >/dev/null 2>&1 || exit 0
[ -f "$RELAY" ] || exit 0

# 住宅上游 = relay 的 socks 出站；<2 条无可切换，退出
nmem=$(jq -r '[.outbounds[]|select(.type=="socks")]|length' "$RELAY" 2>/dev/null || echo 0)
[ "${nmem:-0}" -ge 2 ] || exit 0

[ -f "$STATE" ] || echo '{}' > "$STATE"

healthy=(); alltags=()
while IFS= read -r m; do
    tag=$(echo "$m" | jq -r .tag)
    host=$(echo "$m" | jq -r '.server'); port=$(echo "$m" | jq -r '.server_port')
    user=$(echo "$m" | jq -r '.username // ""'); pass=$(echo "$m" | jq -r '.password // ""')
    [ -z "$tag" ] && continue
    alltags+=("$tag")

    # v3.6.0 R3: 换行会在 curl 配置里注入额外指令；host/port 直接进 proxy 行，必须严格校验。
    # 不合规的一律跳过探测并保留其当前池成员身份，免得一条坏记录既剪掉池成员又白搭一次重启
    skip=""
    case "${user}${pass}" in *$'\n'*|*$'\r'*) skip="凭据含换行符" ;; esac
    case "$host" in ""|*$'\n'*|*$'\r'*|*'"'*|*'\'*) skip="host/port 非法" ;; esac
    [[ "$port" =~ ^[0-9]{1,5}$ ]] || skip="host/port 非法"
    if [ -n "$skip" ]; then
        log "WARN ${tag} ${skip}，跳过探测（保留当前状态）"
        [ "$(jq -r --arg n "$tag" '.[$n].active // true' "$STATE")" = "false" ] || healthy+=("$tag")
        continue
    fi
    ok=0
    for _ in $(seq 1 "$TRIES"); do
        curl_socks_cfg "$host" "$port" "$user" "$pass" \
            | curl -s -o /dev/null --max-time "$TIMEOUT" -K - "$PROBE_URL" 2>/dev/null && ok=$((ok+1))
    done

    active=$(jq -r --arg n "$tag" '.[$n].active // true' "$STATE")
    if [ "$ok" -ge "$OK_NEED" ]; then
        okstreak=$(( $(jq -r --arg n "$tag" '.[$n].okstreak // 0' "$STATE") + 1 )); failstreak=0
    else
        failstreak=$(( $(jq -r --arg n "$tag" '.[$n].failstreak // 0' "$STATE") + 1 )); okstreak=0
    fi
    if [ "$active" = "true" ]  && [ "$failstreak" -ge "$FAIL_TO_REMOVE" ]; then active=false; log "剔除 ${tag}(${host}) —— 连续 ${failstreak} 次探测不达标(本次 ${ok}/${TRIES})"; fi
    if [ "$active" = "false" ] && [ "$okstreak"  -ge "$OK_TO_READD"   ]; then active=true;  log "恢复 ${tag}(${host}) —— 连续 ${okstreak} 次探测健康"; fi

    if [ "$DRY_RUN" != "1" ]; then
        jq --arg n "$tag" --argjson a "$active" --argjson f "$failstreak" --argjson o "$okstreak" \
           '.[$n]={active:$a,failstreak:$f,okstreak:$o}' "$STATE" > "${STATE}.tmp" 2>/dev/null && mv "${STATE}.tmp" "$STATE"
    fi
    [ "$DRY_RUN" = "1" ] && log "probe ${tag} ${host}:${port}: ${ok}/${TRIES} ok → active=${active}"
    [ "$active" = "true" ] && healthy+=("$tag")
done < <(jq -c '.outbounds[]|select(.type=="socks")|{tag,server,server_port,username,password}' "$RELAY")

# v3.6.0 R6: 不再改写 relay 配置、不再重启 b-ui-relay（重启会掐断所有用户的现有连接）。
# 只经 Clash API 读 selector 当前选择：当前线路健康就粘住；不健康才切到一条健康线路
# （按 relay 里的成员顺序取第一条）。全部不健康 → 不切，只 WARN。
was='[]'
[ "${#alltags[@]}" -gt 0 ] && was=$(printf '%s\n' "${alltags[@]}" | sort -u | jq -R . | jq -cs .)

# alltags 是探测开始时的成员快照。探测期间管理员增删了住宅 URL（或 relay 正被改写）→ 本轮不切：
# 那时 selector 的成员已随 relay 重写而变，探测结论与切换目标都可能已经过期，下一轮重新评估。
nowtags=$(jq -c '[.outbounds[]|select(.type=="socks").tag]|unique' "$RELAY" 2>/dev/null || echo '[]')
[ -n "$nowtags" ] || nowtags='[]'
if [ "$nowtags" != "$was" ]; then
    log "住宅池成员在本轮探测期间发生变化 ${was} → ${nowtags}，本轮不切换"
    exit 0
fi

sel=$(curl -s --max-time 2 "http://${API}/proxies/resi-pool" 2>/dev/null | jq -r '.now // empty' 2>/dev/null)
if [ -z "$sel" ]; then
    log "relay 未启用 Clash API（旧配置或未运行），跳过切换；升级后 reapply 会启用"
    exit 0
fi

if [ "${#healthy[@]}" -eq 0 ]; then
    log "WARN 全部线路探测不达标，保持当前 ${sel}（降级总比乱切好）"
    exit 0
fi
for h in "${healthy[@]}"; do
    [ "$h" = "$sel" ] && { [ "$DRY_RUN" = "1" ] && log "当前 ${sel} 健康，保持"; exit 0; }
done

target="${healthy[0]}"
now=$(date +%s)
last=$(jq -r '._last_switch // 0' "$STATE" 2>/dev/null); last=${last:-0}
[ "$last" -gt "$now" ] && last=0   # 时钟回跳（NTP 校时/手工改表）不致于把限速锁死
if [ $((now - last)) -lt "$SWITCH_MIN" ]; then
    log "当前 ${sel} 不健康，需切到 ${target}，但切换限速中（剩余 $((SWITCH_MIN - now + last))s）"
    exit 0
fi
if [ "$DRY_RUN" = "1" ]; then
    log "当前 ${sel} 不健康，将切到 ${target}"
elif curl -sf --max-time 3 -X PUT -H 'Content-Type: application/json' \
          -d "{\"name\":\"${target}\"}" "http://${API}/proxies/resi-pool" >/dev/null 2>&1; then
    jq --argjson t "$now" '._last_switch=$t' "$STATE" > "${STATE}.tmp" 2>/dev/null && mv "${STATE}.tmp" "$STATE"
    log "切换住宅出口 ${sel} → ${target}（${sel} 连续探测不达标）"
else
    log "WARN 切换到 ${target} 失败（Clash API PUT 出错）"
fi
exit 0
