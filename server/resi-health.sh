#!/bin/bash
# B-UI 住宅线路可靠性监测（reliability-aware failover）
# 对每条住宅 SOCKS5 上游做真实连通探测，按成功率打分；让 sing-box relay 的 urltest 只在"健康"
# 线路里选——某条质量变差自动从池中剔除（流量转到好的那条），恢复后自动加回。带迟滞防抖，
# 绝不清空池子（至少留 1 条），仅在池子真发生变化时才 reload relay（平时零打扰）。
# 由 b-ui-resi-health.timer 每 ~2 分钟触发。重启有冷却（默认 10 分钟，RESI_HEALTH_RESTART_COOLDOWN 可调）。
#
# 关键：池成员(tag + socks 凭据)直接读自 relay 自身的 socks 出站——relay 是 urltest 池 tag 的
# 唯一真源（住宅 tag 是 resi-1/resi-2，不是 residential-proxy.json 里的 url-1/url-2，别搞混）。
set -u
BASE="${RESI_HEALTH_BASE_DIR:-/opt/b-ui}"
RELAY="$BASE/singbox-relay.json"
STATE="$BASE/.resi-health-state.json"
LOCK="$BASE/.relay.lock"
LOG="${RESI_HEALTH_LOG:-/var/log/b-ui-resi-health.log}"
RESTART_COOLDOWN="${RESI_HEALTH_RESTART_COOLDOWN:-600}"
PROBE_URL="${RESI_HEALTH_PROBE_URL:-https://www.gstatic.com/generate_204}"
TRIES="${RESI_HEALTH_TRIES:-3}"
OK_NEED="${RESI_HEALTH_OK_NEED:-2}"
TIMEOUT="${RESI_HEALTH_TIMEOUT:-6}"
FAIL_TO_REMOVE="${RESI_HEALTH_FAIL_TO_REMOVE:-2}"
OK_TO_READD="${RESI_HEALTH_OK_TO_READD:-2}"
DRY_RUN="${RESI_HEALTH_DRY_RUN:-0}"

log(){ echo "[$(date '+%F %T')] $1" >> "$LOG" 2>/dev/null; [ "$DRY_RUN" = "1" ] && echo "$1"; }

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

desired=(); alltags=()
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
        [ "$(jq -r --arg n "$tag" '.[$n].active // true' "$STATE")" = "false" ] || desired+=("$tag")
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
    [ "$active" = "true" ] && desired+=("$tag")
done < <(jq -c '.outbounds[]|select(.type=="socks")|{tag,server,server_port,username,password}' "$RELAY")

# 绝不清空：全坏则保留全部（降级总比断流好）
if [ "${#desired[@]}" -eq 0 ]; then desired=("${alltags[@]}"); log "WARN 全部线路探测不达标，保留全部避免断流"; fi

# v3.6.0 R2: 成员集一律排序去重；空数组必须得到 [] 而不是 [""]（printf 无参会吐一个空行）
des='[]'
[ "${#desired[@]}" -gt 0 ] && des=$(printf '%s\n' "${desired[@]}" | sort -u | jq -R . | jq -cs .)
was='[]'
[ "${#alltags[@]}" -gt 0 ] && was=$(printf '%s\n' "${alltags[@]}" | sort -u | jq -R . | jq -cs .)

# v3.6.0 R2: 只在读-改-写-重启这一段持锁（探测循环不持锁），持锁后重读当前池，避免用陈旧快照覆盖管理员刚做的修改
exec 9>"$LOCK"
if ! flock -w 30 9; then log "WARN 获取 relay 锁超时，本轮跳过"; exit 0; fi

# alltags/desired 是探测开始时的成员快照。持锁后复核成员集：探测期间管理员增删了住宅 URL、
# 或 relay 正被改写（读到不足 2 条），本轮一律弃写——否则会拿陈旧成员集覆盖掉管理员
# 刚做的修改，还白搭一个重启冷却窗口。
nowtags=$(jq -c '[.outbounds[]|select(.type=="socks").tag]|unique' "$RELAY" 2>/dev/null || echo '[]')
[ -n "$nowtags" ] || nowtags='[]'
nowcnt=$(jq -r 'length' <<<"$nowtags" 2>/dev/null || echo 0)
if [ "${nowcnt:-0}" -lt 2 ] || [ "$nowtags" != "$was" ]; then
    log "住宅池成员在本轮探测期间发生变化 ${was} → ${nowtags}，本轮不改池、不重启"
    exit 0
fi

cur=$(jq -c '[.outbounds[]|select(.type=="urltest" and .tag=="resi-pool").outbounds[]]|sort' "$RELAY" 2>/dev/null)
if [ "$des" != "$cur" ]; then
    now=$(date +%s)
    last=$(jq -r '._last_restart // 0' "$STATE" 2>/dev/null); last=${last:-0}
    [ "$last" -gt "$now" ] && last=0   # 时钟回跳（NTP 校时/手工改表）不致于把冷却锁死
    if [ $((now - last)) -lt "$RESTART_COOLDOWN" ]; then
        log "住宅池需变更 ${cur} → ${des}，重启冷却中（剩余 $((RESTART_COOLDOWN - now + last))s），延后"
    else
        log "住宅池变化: ${cur} → ${des}"
        if [ "$DRY_RUN" != "1" ]; then
            jq --argjson d "$des" '.outbounds |= map(if (.type=="urltest" and .tag=="resi-pool") then (.outbounds=$d) else . end)' \
               "$RELAY" > "${RELAY}.tmp" 2>/dev/null && mv "${RELAY}.tmp" "$RELAY" \
               && systemctl restart b-ui-relay 2>/dev/null \
               && jq --argjson t "$now" '._last_restart=$t' "$STATE" > "${STATE}.tmp" 2>/dev/null && mv "${STATE}.tmp" "$STATE" \
               && log "已更新 singbox-relay.json + 重启 b-ui-relay（住宅池=$(IFS=,; echo "${desired[*]}")）"
        fi
    fi
fi
flock -u 9

[ "$DRY_RUN" != "1" ] && { tail -300 "$LOG" > "${LOG}.tmp" 2>/dev/null && mv "${LOG}.tmp" "$LOG" 2>/dev/null; }
exit 0
