#!/usr/bin/env bash
# v4 M5：升级 / 回滚演练。三个相位各取一次指纹（版本、单元状态、配置 sha、三种订阅 sha），逐项比对。
# manifest 来源用 C5 的 `bui upgrade --manifest-url <url|file>`：M5 不发任何 Release，
# 两份 manifest 由本机 `python3 -m http.server` 托管 CI 产物的 dist/ 目录（见 Task 12 Step 2）。
# 生产用法（bwg-rick，root）：
#   nohup bash /opt/b-ui/ops/upgrade-drill.sh --to 4.0.1 \
#     --manifest-url http://127.0.0.1:8000/v4.0.1/manifest.json \
#     --users alice,bob --out /var/log/bui-drill >/dev/null 2>&1 &
# 适用范围：升级前后两端都得是 v4.0.0-rc12 及以上。订阅指纹按 state.json 的 users[].sub_token
# 取（2026-09-14 订阅 token），rc11 及更早的 state 没有这个字段——任一相位的 state.json 里任一
# 目标用户没有 sub_token 就 FATAL 退 2（前置条件不满足，不报成订阅漂移或取订阅失败），所以
# rc11→rc12 这一跳不能用本脚本演练。
set -uo pipefail
LC_ALL=C

TO=""
MANIFEST_URL=""
USERS=""
OUT="/var/log/bui-drill"
BASE="/opt/b-ui"
BUI="/opt/b-ui/bin/bui"
API="http://127.0.0.1:8080"
UNITS="b-ui hysteria-server hysteria-residential xray b-ui-relay caddy"

usage() {
    printf '用法：%s --users a,b [--to <x.y.z>] [--manifest-url <url|file>] [--out <dir>] [--base /opt/b-ui] [--bui <path>] [--api http://127.0.0.1:8080]\n' "$0" >&2
    exit 2
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --to) TO="${2:-}"; shift 2 ;;
        --manifest-url) MANIFEST_URL="${2:-}"; shift 2 ;;
        --users) USERS="${2:-}"; shift 2 ;;
        --out) OUT="${2:-}"; shift 2 ;;
        --base) BASE="${2:-}"; shift 2 ;;
        --bui) BUI="${2:-}"; shift 2 ;;
        --api) API="${2:-}"; shift 2 ;;
        --units) UNITS="${2:-}"; shift 2 ;;
        *) usage ;;
    esac
done
[[ -n "$USERS" ]] || usage
# state.json 里的订阅 token 要用 python3 解（口径同 scripts/ops/sentinel-drill.sh）
command -v python3 >/dev/null 2>&1 || { printf '需要 python3 来解析 state.json\n' >&2; exit 2; }

mkdir -p "$OUT" || exit 2
CSV="$OUT/drill.csv"
LOG="$OUT/drill.log"
DONE="$OUT/DONE"
rm -f "$DONE"
printf 'phase,key,value\n' > "$CSV"
exec 3>&1
log() { printf '%s %s\n' "$(date -u +%FT%TZ)" "$1" | tee -a "$LOG" >&3; }
rec() { printf '%s,%s,%s\n' "$1" "$2" "$3" >> "$CSV"; }

sha_str() { printf '%s' "$1" | sha256sum | cut -d' ' -f1; }
sha_file() { [[ -f "$1" ]] && sha256sum "$1" | cut -d' ' -f1 || printf 'missing\n'; }

# $1 = 用户名 → 该用户的订阅 token（state.json 的 users[].sub_token），取不到就空串。
# 2026-09-14 裁决：四个免鉴权端点认随机 token，用户名链接只在全局宽限期内还认 ⇒ 演练必须
# 按 token 取订阅。按用户名取会拿到 404 + 空 body，而 `sha_str ""` 前后两相位相同，
# `compare_subs` 就此判「订阅无漂移」——比直接失败更坏的假绿。
sub_token() {
    python3 - "$BASE/state.json" "$1" <<'PY'
import json, sys
try:
    d = json.load(open(sys.argv[1]))
except Exception:
    raise SystemExit(0)
for u in d.get("users") or []:
    if u.get("username") == sys.argv[2]:
        print(u.get("sub_token") or "")
        break
PY
}

snapshot() {
    # $1 = phase
    local phase="$1" u kind url body unit f tok
    rec "$phase" version "$("$BUI" --version 2>/dev/null | tr -d '[:space:]')"
    for unit in $UNITS; do
        rec "$phase" "unit:$unit" "$(systemctl is-active "$unit" 2>/dev/null)"
        rec "$phase" "nrestarts:$unit" "$(systemctl show -p NRestarts --value "$unit" 2>/dev/null)"
    done
    rec "$phase" sha:state "$(sha_file "$BASE/state.json")"
    for f in config.yaml config-residential.yaml xray-config.json singbox-relay.json; do
        rec "$phase" "sha:$f" "$(sha_file "$BASE/$f")"
    done
    # bui 与四个内核的 sha256：验「内核随 manifest 升级」与「--rollback 把内核也退回去」
    for f in bui hysteria xray sing-box caddy; do
        rec "$phase" "sha:bin/$f" "$(sha_file "$BASE/bin/$f")"
    done
    # 适用范围守卫（见文件头）：先把这一相位所有目标用户的 token 查一遍，缺一个就 FATAL，
    # 一条订阅都不取、一条 sub: 指纹都不记
    for u in ${USERS//,/ }; do
        [[ -n "$(sub_token "$u")" ]] && continue
        local hint=""
        [[ "$phase" == after-upgrade ]] && hint="；升级已执行、回滚未执行"
        fatal "$phase：state.json 里 $u 没有 sub_token——本演练要求升级前后两端都是 rc12 及以上（该相位的 state.json 没有订阅 token）$hint" \
            "sub-token-missing:$phase:$u"
    done
    for u in ${USERS//,/ }; do
        tok=$(sub_token "$u")
        for kind in sub subscription clash; do
            case "$kind" in
                sub) url="$API/api/sub/$tok" ;;
                subscription) url="$API/api/subscription/$tok" ;;
                clash) url="$API/api/clash/$tok" ;;
            esac
            # 空 body 也算失败：不记这个 key，`compare_keys` 才不会拿两个空串比出「无漂移」。
            # 末段就是凭据（响应体里有 hy2 明文密码与 vless uuid），URL 经 `-K -` 的 stdin 传，
            # 绝不进 argv（ps 会泄露）
            body=$(printf 'url = "%s"\n' "$url" | curl -fsS --max-time 15 -K - 2>/dev/null)
            if [[ -z "$body" ]]; then
                log "FAIL $phase：取 $u 的 $kind 订阅失败或返回空（$API/api/$kind/<token>）"
                note_fail "sub-fetch:$phase:$u:$kind"
                continue
            fi
            rec "$phase" "sub:$u:$kind" "$(sha_str "$body")"
        done
    done
}

compare_keys() {
    # $1 / $2 = 两个 phase，$3 = key 的 ERE，$4 = 失败标签；逐 key 比对，不同则打印并返回 1
    local a="$1" b="$2" re="$3" label="$4" key va vb rc=0
    while IFS= read -r key; do
        va=$(awk -F, -v p="$a" -v k="$key" '$1 == p && $2 == k {print $3}' "$CSV")
        vb=$(awk -F, -v p="$b" -v k="$key" '$1 == p && $2 == k {print $3}' "$CSV")
        if [[ "$va" != "$vb" ]]; then
            log "FAIL $label $key：$a=$va $b=$vb"
            rc=1
        fi
    done < <(awk -F, -v p="$a" -v re="$re" '$1 == p && $2 ~ re {print $2}' "$CSV")
    return "$rc"
}

compare_subs() { compare_keys "$1" "$2" '^sub:' "订阅漂移"; }
compare_bins() { compare_keys "$1" "$2" '^sha:bin/' "二进制未复原"; }

all_active() {
    local phase="$1" unit st rc=0
    for unit in $UNITS; do
        st=$(awk -F, -v p="$phase" -v k="unit:$unit" '$1 == p && $2 == k {print $3}' "$CSV")
        if [[ "$st" != "active" ]]; then
            log "FAIL 单元 $unit 在 $phase 不是 active（$st）"
            rc=1
        fi
    done
    return "$rc"
}

FAILED=""
note_fail() { [[ -n "$FAILED" ]] || FAILED="$1"; }
# 前置条件不满足：$1 = 文案，$2 = 原因标签。退 2（口径同开头的参数与 python3 守卫），并照样写
# DONE —— nohup 跑的时候盯的是 DONE，不写就一直等不到结论
fatal() {
    log "FATAL $1"
    printf 'verdict=FATAL reason=%s finished=%s manifest=%s backup=%s\n' \
        "$2" "$(date -u +%FT%TZ)" "${MANIFEST_URL:-默认}" "$BK" > "$DONE"
    exit 2
}

log "演练开始：base=$BASE bui=$BUI to=${TO:-manifest 里的版本} manifest=${MANIFEST_URL:-默认（GitHub Releases latest）} users=$USERS"
BK="$OUT/backup-$(date -u +%Y%m%dT%H%M%SZ).tar.gz"
# spec §9「bwg-tizi 上线」要求快照 /opt/b-ui + 单元文件；六个单元逐个列清，
# 还没落地的单元用 --ignore-failed-read 跳过而不是让整个 tar 失败。
UNIT_FILES=()
for unit in $UNITS; do
    UNIT_FILES+=("etc/systemd/system/$unit.service")
done
if tar -czf "$BK" --ignore-failed-read -C / "${BASE#/}" "${UNIT_FILES[@]}" >> "$LOG" 2>&1; then
    log "快照：$BK"
else
    log "快照失败（继续演练，但主理人需知悉）"
    note_fail "backup"
fi

snapshot before
before_ver=$(awk -F, '$1 == "before" && $2 == "version" {print $3}' "$CSV")
log "升级前版本：$before_ver"

UP_ARGS=(upgrade)
[[ -n "$TO" ]] && UP_ARGS+=(--version "$TO")
[[ -n "$MANIFEST_URL" ]] && UP_ARGS+=(--manifest-url "$MANIFEST_URL")
"$BUI" "${UP_ARGS[@]}" >> "$LOG" 2>&1 || note_fail "upgrade-exit"
sleep 5
snapshot after-upgrade
after_ver=$(awk -F, '$1 == "after-upgrade" && $2 == "version" {print $3}' "$CSV")
log "升级后版本：$after_ver"
log "升级后二进制指纹（只记录，与 manifest 的比对见 M5 报告）：$(awk -F, '$1 == "after-upgrade" && $2 ~ /^sha:bin\// {printf "%s=%s ", $2, substr($3, 1, 12)}' "$CSV")"
if [[ -n "$TO" && "$after_ver" != "$TO" ]]; then
    log "FAIL 升级后版本 $after_ver != 目标 $TO"
    note_fail "upgrade-version"
fi
all_active after-upgrade || note_fail "upgrade-units"
compare_subs before after-upgrade || note_fail "upgrade-subs"

"$BUI" upgrade --rollback >> "$LOG" 2>&1 || note_fail "rollback-exit"
sleep 5
snapshot after-rollback
back_ver=$(awk -F, '$1 == "after-rollback" && $2 == "version" {print $3}' "$CSV")
log "回滚后版本：$back_ver"
if [[ "$back_ver" != "$before_ver" ]]; then
    log "FAIL 回滚后版本 $back_ver != 升级前 $before_ver"
    note_fail "rollback-version"
fi
all_active after-rollback || note_fail "rollback-units"
compare_subs before after-rollback || note_fail "rollback-subs"
compare_bins before after-rollback || note_fail "rollback-kernels"

if [[ -z "$FAILED" ]]; then
    log "PASS 升级与回滚均成功，订阅逐项未变，bui 与四内核 sha256 已复原"
    printf 'verdict=PASS finished=%s before=%s upgraded=%s rolled_back=%s manifest=%s backup=%s\n' \
        "$(date -u +%FT%TZ)" "$before_ver" "$after_ver" "$back_ver" "${MANIFEST_URL:-默认}" "$BK" > "$DONE"
    exit 0
fi
log "FAIL 首个失败项：$FAILED"
printf 'verdict=FAIL first_failure=%s finished=%s manifest=%s backup=%s\n' \
    "$FAILED" "$(date -u +%FT%TZ)" "${MANIFEST_URL:-默认}" "$BK" > "$DONE"
exit 1
