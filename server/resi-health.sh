#!/bin/bash
# B-UI 住宅线路可靠性监测（reliability-aware failover）
# 对每条住宅 SOCKS5 上游做真实连通探测，按成功率打分；让 sing-box relay 的 urltest 只在"健康"
# 线路里选——某条质量变差自动从池中剔除（流量转到好的那条），恢复后自动加回。带迟滞防抖，
# 绝不清空池子（至少留 1 条），仅在池子真发生变化时才 reload relay（平时零打扰）。
# 由 b-ui-resi-health.timer 每 ~2 分钟触发。
#
# 关键：池成员(tag + socks 凭据)直接读自 relay 自身的 socks 出站——relay 是 urltest 池 tag 的
# 唯一真源（住宅 tag 是 resi-1/resi-2，不是 residential-proxy.json 里的 url-1/url-2，别搞混）。
set -u
RELAY=/opt/b-ui/singbox-relay.json
STATE=/opt/b-ui/.resi-health-state.json
LOG=/var/log/b-ui-resi-health.log
PROBE_URL="${RESI_HEALTH_PROBE_URL:-https://www.gstatic.com/generate_204}"
TRIES="${RESI_HEALTH_TRIES:-3}"
OK_NEED="${RESI_HEALTH_OK_NEED:-2}"
TIMEOUT="${RESI_HEALTH_TIMEOUT:-6}"
FAIL_TO_REMOVE="${RESI_HEALTH_FAIL_TO_REMOVE:-2}"
OK_TO_READD="${RESI_HEALTH_OK_TO_READD:-2}"
DRY_RUN="${RESI_HEALTH_DRY_RUN:-0}"

log(){ echo "[$(date '+%F %T')] $1" >> "$LOG" 2>/dev/null; [ "$DRY_RUN" = "1" ] && echo "$1"; }

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

    creds=""; [ -n "$user" ] && creds="${user}:${pass}@"
    ok=0
    for _ in $(seq 1 "$TRIES"); do
        curl -s -o /dev/null --max-time "$TIMEOUT" \
            --socks5-hostname "${creds}${host}:${port}" "$PROBE_URL" 2>/dev/null && ok=$((ok+1))
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

des=$(printf '%s\n' "${desired[@]}" | sort -u | jq -R . | jq -cs .)
cur=$(jq -c '[.outbounds[]|select(.type=="urltest" and .tag=="resi-pool").outbounds[]]|sort' "$RELAY" 2>/dev/null)
if [ "$des" != "$cur" ]; then
    log "住宅池变化: ${cur} → ${des}"
    if [ "$DRY_RUN" != "1" ]; then
        jq --argjson d "$des" '.outbounds |= map(if (.type=="urltest" and .tag=="resi-pool") then (.outbounds=$d) else . end)' \
           "$RELAY" > "${RELAY}.tmp" 2>/dev/null && mv "${RELAY}.tmp" "$RELAY" \
           && systemctl restart b-ui-relay 2>/dev/null \
           && log "已更新 singbox-relay.json + 重启 b-ui-relay（住宅池=$(IFS=,; echo "${desired[*]}")）"
    fi
fi

[ "$DRY_RUN" != "1" ] && { tail -300 "$LOG" > "${LOG}.tmp" 2>/dev/null && mv "${LOG}.tmp" "$LOG" 2>/dev/null; }
exit 0
