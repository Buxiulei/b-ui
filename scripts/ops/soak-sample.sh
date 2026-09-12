#!/usr/bin/env bash
# v4 M5：长跑采样。每 interval 秒给每个单元写一行 CSV（RSS/fd/重启次数 + 全局连接数与内存）。
# 生产用法（bwg-rick，root）：
#   nohup bash /opt/b-ui/ops/soak-sample.sh --hours 72 --out /var/log/bui-soak >/dev/null 2>&1 &
#   然后用 Monitor 等 /var/log/bui-soak/DONE 出现，再跑 soak-report.sh。
# 故意不用 set -e：单次取样失败（单元重启中、pid 消失）必须继续采，不能中断 72 小时。
set -uo pipefail
LC_ALL=C

HOURS=72
SECONDS_TOTAL=""
INTERVAL=60
OUT="/var/log/bui-soak"
UNITS="xray b-ui hysteria-server hysteria-residential b-ui-relay caddy"

usage() {
    printf '用法：%s [--hours 72 | --seconds <n>] [--interval 60] [--out <dir>] [--units "<unit> …"]\n' "$0" >&2
    exit 2
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --hours) HOURS="${2:-}"; SECONDS_TOTAL=""; shift 2 ;;
        --seconds) SECONDS_TOTAL="${2:-}"; shift 2 ;;
        --interval) INTERVAL="${2:-}"; shift 2 ;;
        --out) OUT="${2:-}"; shift 2 ;;
        --units) UNITS="${2:-}"; shift 2 ;;
        *) usage ;;
    esac
done
[[ -z "$SECONDS_TOTAL" ]] && SECONDS_TOTAL=$((HOURS * 3600))

mkdir -p "$OUT" || exit 2
CSV="$OUT/soak.csv"
DONE="$OUT/DONE"
rm -f "$DONE"
printf '%s\n' "$$" > "$OUT/soak.pid"
[[ -s "$CSV" ]] || printf 'ts,elapsed_s,unit,pid,rss_kb,fd,nrestarts,tcp_estab,udp_alloc,load1,mem_avail_kb\n' > "$CSV"

REASON="completed"
on_term() { REASON="signal"; }
trap on_term TERM INT

unit_prop() { systemctl show -p "$2" --value "$1" 2>/dev/null; }
rss_kb()    { awk '/^VmRSS:/ {print $2; exit}' "/proc/$1/status" 2>/dev/null; }
fd_count()  { ls -U "/proc/$1/fd" 2>/dev/null | wc -l; }
tcp_estab() { awk 'NR > 1 && $4 == "01"' /proc/net/tcp /proc/net/tcp6 2>/dev/null | wc -l; }
udp_alloc() { awk '/^UDP:/ {print $3; exit}' /proc/net/sockstat 2>/dev/null; }
load1()     { awk '{print $1; exit}' /proc/loadavg 2>/dev/null; }
mem_avail() { awk '/^MemAvailable:/ {print $2; exit}' /proc/meminfo 2>/dev/null; }

START=$(date +%s)
END=$((START + SECONDS_TOTAL))
while :; do
    now=$(date +%s)
    [[ "$now" -ge "$END" ]] && break
    [[ "$REASON" == "signal" ]] && break
    ts=$(date -u +%FT%TZ)
    elapsed=$((now - START))
    t=$(tcp_estab); u=$(udp_alloc); l=$(load1); m=$(mem_avail)
    for unit in $UNITS; do
        pid=$(unit_prop "$unit" MainPID)
        [[ -n "$pid" ]] || pid=0
        nr=$(unit_prop "$unit" NRestarts)
        [[ -n "$nr" ]] || nr=0
        if [[ "$pid" != "0" && -d "/proc/$pid" ]]; then
            rss=$(rss_kb "$pid"); fds=$(fd_count "$pid")
        else
            rss=0; fds=0
        fi
        printf '%s,%d,%s,%s,%s,%s,%s,%s,%s,%s,%s\n' \
            "$ts" "$elapsed" "$unit" "$pid" "${rss:-0}" "${fds:-0}" "$nr" "${t:-0}" "${u:-0}" "${l:-0}" "${m:-0}" >> "$CSV"
    done
    sleep "$INTERVAL"
done

printf 'reason=%s finished=%s rows=%d interval=%s units="%s"\n' \
    "$REASON" "$(date -u +%FT%TZ)" "$(($(wc -l < "$CSV") - 1))" "$INTERVAL" "$UNITS" > "$DONE"
rm -f "$OUT/soak.pid"
