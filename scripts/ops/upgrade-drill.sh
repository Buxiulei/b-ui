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
# 适用范围（4.1 追加）：本脚本按「**4.0.x → 4.1 与回滚**」写死判据——升级相位要求 `inet bui`
# 表在且 4 条 redirect、只有 :40000 在听、4.0.x 的槽位实例（单元与 40000+i）一个不剩；回滚相位
# 要求表消失、4.0.x 每槽一个的 40000+i 逐个在听、每槽的 hysteria-residential[-<i>] 单元 active
# （spec §9.1）。这两组断言全靠重启后的守护进程**异步**对账，所以不是固定睡几秒，而是轮询到
# 收敛或 `--settle` 超时（默认 300 秒——这一跳要从 GitHub 拉回自建 sing-box 与 hysteria）。
# 「升级计划里有 sing-box 那一行」只在**执行升级的二进制已是 4.1.x** 时才判 FAIL：4.0.x 的
# `plan_upgrade` 只比版本号、不比资产 sha256（第二波裁决 P-A），而自建与官方归档同打 1.14.1
# ⇒ 4.0.x → 4.1 这一跳的计划里本来就不会有它，一次完全健康的升级也会被判 FAIL。这一跳的真
# 不变量是升级后 `bin/sing-box version` 的 Tags 含 with_v2ray_api（下面那道闸门）。
set -uo pipefail
LC_ALL=C

TO=""
MANIFEST_URL=""
USERS=""
OUT="/var/log/bui-drill"
BASE="/opt/b-ui"
BUI="/opt/b-ui/bin/bui"
API="http://127.0.0.1:8080"
# 受管单元：4.1 期望态是固定六个；4.0.x 另有每槽一个的 `hysteria-residential-<i>`（槽 0 用
# 无后缀的名字，所以后缀只有 1..MAX_SLOTS-1 = 1..7）。**三个相位都采 V40 的全集**（`units_for`），
# 「必须 active」的清单才按相位收窄（`units_required`）——升级相位只采固定六个的话，残留的
# 4.0.x 槽位实例连指纹都不记。生产上的槽数一般远小于 7，`hysteria-residential-2..7` 本来就
# 没有单元文件，所以 `snapshot` 把 LoadState 一并记下，`all_active` 只对**槽位实例**跳过
# `not-found`（固定六个缺一个照判 FAIL）；「该有的槽位单元到底回来没有」另由槽位表点名。
UNITS_V41="b-ui hysteria-server hysteria-residential xray b-ui-relay caddy"
UNITS_V40="$UNITS_V41 hysteria-residential-1 hysteria-residential-2 hysteria-residential-3 hysteria-residential-4 hysteria-residential-5 hysteria-residential-6 hysteria-residential-7"
# `--units` 覆盖：给了就三个相位都用它（调试用）。按槽实例的单元名随之不在采样清单里，
# 所以按槽的**单元**断言跟着跳过（见 `slot_units_sampled`）
UNITS=""
# Global Constraints 钉死的期望态端口，与 `bui_schema::render::nft` 同源：住宅 HY2 只剩
# `:40000`，整段 41000-50000 与 4.0 兼容段 40001-40007 由 `table inet bui` REDIRECT 过去。
HY2_RESI=40000
HY2_RESI_HOP_START=41000
NFT_TABLE="inet bui"
# `render::nft::rule_count(true)`：两条链 ×（整段 + 兼容段）
NFT_RULES_EXPECTED=4
# 等对账收敛的上限（秒）与采样间隔：`bui upgrade` 在守护进程在跑时只 `systemctl restart b-ui`
# 就返回（commands/upgrade.rs），内核下载安装（自建 sing-box ≈40 MB + hysteria）、
# `hy2-residential.json` 渲染 + `sing-box check`、删旧 yaml、`nft apply`、停旧槽单元起新单元
# 全在重启后的守护进程里 ⇒ 固定睡 5 秒取指纹必判出假 FAIL。
SETTLE=300
SETTLE_STEP=5

usage() {
    printf '用法：%s --users a,b [--to <x.y.z>] [--manifest-url <url|file>] [--out <dir>] [--base /opt/b-ui] [--bui <path>] [--api http://127.0.0.1:8080] [--settle <秒>]\n' "$0" >&2
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
        --settle) SETTLE="${2:-}"; shift 2 ;;
        *) usage ;;
    esac
done
[[ -n "$USERS" ]] || usage
[[ "$SETTLE" =~ ^[0-9]+$ ]] || usage
# 4.1 的 `nft` 前置：住宅整段跳跃全靠 `table inet bui`，缺了它客户端只往 41000-50000 发、
# 住宅 HY2 对所有带 mport 的客户端等于全断（不是「只是跳跃失效」）。**这是 4.0.1→4.1 那一跳
# 唯一的机器闸门**：`bui upgrade` 由**旧**二进制执行，4.1 自己那道 `nft_blocking_for` 闸门
# （commands/upgrade.rs）在这一跳里根本没被执行到。一个相位都不进、一个字都不落盘。
command -v nft >/dev/null 2>&1 || { printf '缺少 nft：4.1 住宅跳跃全靠它，先 apt-get install -y nftables\n' >&2; exit 2; }
# `ss` 前置：全部 listen 判据（升级后只有 :40000 在听、回滚后 40000+i 逐个回来）都走 `ss -lnu`，
# iproute2 缺了 `listening()` 一律返回 false ⇒ 先白等满 `--settle`（默认 300 秒）再判「住宅入站
# 没起来」，把「本机没装 iproute2」报成住宅全断。口径同上下两道守卫：一个相位都不进、一个字都
# 不落盘。排在 python3 之前，好让「摘掉含 ss 的目录」时先撞上这一条（usr-merge 的机器上
# `ss` 与 `python3` 常在同一个目录）。
command -v ss >/dev/null 2>&1 || { printf '缺少 ss：监听端口判据全靠它，先 apt-get install -y iproute2\n' >&2; exit 2; }
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

# state.json 的 `residential.slots[].index`。4.0.x 每槽一个 apernet 实例：槽 0 是无后缀的
# `hysteria-residential` + `:40000`，槽 i 是 `hysteria-residential-<i>` + `:(40000+i)`；
# 回滚相位按这份槽位表逐个断言「端口在听 + 单元 active」（spec §9.1）。
slot_indices() {
    python3 - "$BASE/state.json" <<'PY'
import json, sys
try:
    d = json.load(open(sys.argv[1]))
except Exception:
    raise SystemExit(0)
for s in ((d.get("residential") or {}).get("slots") or []):
    i = s.get("index")
    if isinstance(i, int) and not isinstance(i, bool):
        print(i)
PY
}

# 要**采**的单元清单：三个相位都采 V40 的全集（所以相位参数收了不用，只为调用点对称）。
# 升级相位只采固定六个的话，残留的 4.0.x 槽位实例（尤其槽序号 ≥ 2 的）连 `load:` / `unit:`
# 都不记，结构上就察觉不到。
units_for() {
    if [[ -n "$UNITS" ]]; then
        printf '%s\n' "$UNITS"
    else
        printf '%s\n' "$UNITS_V40"
    fi
}

# $1 = phase → 该相位「必须 active」的单元清单：4.1 收回固定六个，4.0.x 另有槽位实例
units_required() {
    if [[ -n "$UNITS" ]]; then
        printf '%s\n' "$UNITS"
    elif [[ "$1" == after-upgrade ]]; then
        printf '%s\n' "$UNITS_V41"
    else
        printf '%s\n' "$UNITS_V40"
    fi
}

# 按槽实例的**单元**断言（升级相位「一个不剩」、回滚相位「每槽都 active」）能不能判：`--units`
# 一给，采样清单里就没有 `hysteria-residential-<i>` 了，那两条断言只会 `awk` 出空值、判出
# 「期望 not-found，实测（没记到）」的假 FAIL（4.1 机器上最自然的调试写法就是把固定六个抄进
# `--units`）。所以 `--units` 覆盖时跳过它们；按槽的**端口**断言来源是 state.json 的槽位表，
# 与 `--units` 无关，照判。
slot_units_sampled() { [[ -z "$UNITS" ]]; }

# $1 = 端口 → `ss -lnu` 上有没有人在听。`-F:` 取末段，免得 41000 被 141000 命中
listening() {
    ss -lnu 2>/dev/null | awk 'NR > 1 {print $4}' |
        awk -F: -v p="$1" '$NF == p {f = 1} END {exit !f}'
}

snapshot() {
    # $1 = phase
    local phase="$1" u kind url body unit f tok tables rules port n g
    rec "$phase" version "$("$BUI" --version 2>/dev/null | tr -d '[:space:]')"
    for unit in $(units_for "$phase"); do
        rec "$phase" "unit:$unit" "$(systemctl is-active "$unit" 2>/dev/null)"
        rec "$phase" "nrestarts:$unit" "$(systemctl show -p NRestarts --value "$unit" 2>/dev/null)"
        # 单元存不存在（`not-found` = 单元文件不在盘上）。4.0.x 的槽数一般小于 7，
        # `all_active` 得靠它把本来就不该有的**槽位实例**跳过，而不是把它们算成 FAIL；
        # 升级相位反过来用它断言 4.0.x 的槽位实例单元已经一个不剩。
        rec "$phase" "load:$unit" "$(systemctl show -p LoadState --value "$unit" 2>/dev/null)"
    done
    rec "$phase" sha:state "$(sha_file "$BASE/state.json")"
    for f in config.yaml config-residential.yaml xray-config.json singbox-relay.json hy2-residential.json; do
        rec "$phase" "sha:$f" "$(sha_file "$BASE/$f")"
    done
    # 4.1 把 4.0.x 每槽一份的 `config-residential*.yaml` 换成一份 `hy2-residential.json`
    # （spec §3.5）。升级相位要求前者一份不剩、后者在盘上；回滚相位 4.0.x 的对账会写回前者。
    n=0
    for g in "$BASE"/config-residential*.yaml; do
        [[ -e "$g" ]] && n=$((n + 1))
    done
    rec "$phase" count:resi-yaml "$n"
    # nft 指纹：表在不在 + redirect 规则条数。回滚相位要求表**消失**——表留着会把整段
    # 41000-50000 与兼容段 40001-40007 全部 REDIRECT 到 `:40000`，而 4.0.1 的槽 0 只服务
    # 第 0 片 ⇒ 全体用户从槽 0 的 IP 出去、40000+i 无人应答，比回归事故更糟（spec §9.1）。
    tables=$(nft list tables 2>/dev/null)
    if printf '%s\n' "$tables" | grep -qx "table $NFT_TABLE"; then
        rec "$phase" nft:table present
        rules=$(nft list table "$NFT_TABLE" 2>/dev/null | grep -c redirect)
        rec "$phase" nft:rules "$rules"
    else
        rec "$phase" nft:table absent
        rec "$phase" nft:rules 0
    fi
    # 监听端口指纹：升级后只有 `:40000` 在听（兼容段与整段都靠 nft REDIRECT 过去），
    # 回滚后 4.0.x 的每槽一个实例要逐个回到 40000+i。
    for port in $LISTEN_PORTS; do
        if listening "$port"; then
            rec "$phase" "listen:udp:$port" yes
        else
            rec "$phase" "listen:udp:$port" no
        fi
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
compare_nft() { compare_keys "$1" "$2" '^nft:' "nft 指纹未复原"; }
compare_listen() { compare_keys "$1" "$2" '^listen:udp:' "监听端口未复原"; }

# 相位相关的期望值（不是逐相位相等）：$1 = phase，$2 = key，$3 = 期望值，$4 = 失败前缀
expect() {
    local got
    got=$(awk -F, -v p="$1" -v k="$2" '$1 == p && $2 == k {print $3}' "$CSV")
    [[ "$got" == "$3" ]] && return 0
    log "FAIL $4 $1 的 $2：期望 $3，实测 ${got:-（没记到）}"
    return 1
}

all_active() {
    local phase="$1" unit st load rc=0
    for unit in $(units_required "$phase"); do
        load=$(awk -F, -v p="$phase" -v k="load:$unit" '$1 == p && $2 == k {print $3}' "$CSV")
        # 跳过只给 4.0.x 的槽位实例（生产上的槽数一般远小于 7，`hysteria-residential-2..7` 本来
        # 就没有单元文件）。固定六个受管单元缺一个是**故障**，不许被这条跳过吞掉；而「该有的
        # 槽位单元到底回来没有」另由 state.json 的槽位表逐个点名。
        if [[ "$load" == "not-found" && "$unit" == hysteria-residential-[1-7] ]]; then
            log "note 槽位实例单元 $unit 在 $phase 不存在（跳过：4.0.x 的槽数一般小于 7）"
            continue
        fi
        st=$(awk -F, -v p="$phase" -v k="unit:$unit" '$1 == p && $2 == k {print $3}' "$CSV")
        if [[ "$st" != "active" ]]; then
            log "FAIL 单元 $unit 在 $phase 不是 active（$st）"
            rc=1
        fi
    done
    return "$rc"
}

# $1 = 相位名（只用于日志），$2 = 判据函数名。轮询到判据成立或 SETTLE 超时。判据用的条件是
# 下面那组硬断言的子集，所以超时只记 note——具体哪一项不达标由硬断言点名，first_failure
# 才指得准。
wait_settle() {
    local phase="$1" pred="$2" waited=0
    while :; do
        if "$pred"; then
            log "$phase：对账已收敛（等了 ${waited}s）"
            return 0
        fi
        [[ "$waited" -ge "$SETTLE" ]] && break
        sleep "$SETTLE_STEP"
        waited=$((waited + SETTLE_STEP))
    done
    log "note $phase：等了 ${SETTLE}s 对账仍未收敛，下面的硬断言会点名具体哪一项"
    return 1
}

# 升级相位的收敛判据：表在且规则齐、:40000 在听、旧 yaml 已删、新 json 已落盘
upgrade_settled() {
    local rules g
    listening "$HY2_RESI" || return 1
    nft list tables 2>/dev/null | grep -qx "table $NFT_TABLE" || return 1
    rules=$(nft list table "$NFT_TABLE" 2>/dev/null | grep -c redirect)
    [[ "$rules" == "$NFT_RULES_EXPECTED" ]] || return 1
    [[ -f "$BASE/hy2-residential.json" ]] || return 1
    for g in "$BASE"/config-residential*.yaml; do
        [[ -e "$g" ]] && return 1
    done
    return 0
}

# 回滚相位的收敛判据：表已删、4.0.x 每槽一个实例回到 40000+i 且单元 active
rollback_settled() {
    local i unit
    nft list tables 2>/dev/null | grep -qx "table $NFT_TABLE" && return 1
    for i in $SLOTS; do
        listening "$((HY2_RESI + i))" || return 1
        unit="hysteria-residential"
        [[ "$i" == "0" ]] || unit="hysteria-residential-$i"
        [[ "$(systemctl is-active "$unit" 2>/dev/null)" == active ]] || return 1
    done
    return 0
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
# 升级前那份 state.json 的槽位表 = 4.0.x 的槽位表（回滚会把 state 恢复到同一份）
SLOTS=$(slot_indices | sort -n | tr '\n' ' ')
log "升级前的住宅槽位：${SLOTS:-（无，住宅未启用）}"
slot_units_sampled ||
    log "note --units 覆盖了采样清单（$UNITS）：按槽实例的单元断言跳过，按槽端口断言照判"
# 监听指纹要采的端口：住宅基础端口、跳跃段首端口（4.1 里被 REDIRECT 掉、永远无人监听）、
# 兼容段第一个，再加 4.0.x 每槽一个的 40000+i
LISTEN_PORTS="$HY2_RESI $HY2_RESI_HOP_START $((HY2_RESI + 1))"
for i in $SLOTS; do
    LISTEN_PORTS="$LISTEN_PORTS $((HY2_RESI + i))"
done
# shellcheck disable=SC2086 # 端口是纯数字，就是要按空格切开去重
LISTEN_PORTS=$(printf '%s\n' $LISTEN_PORTS | sort -nu | tr '\n' ' ')
BK="$OUT/backup-$(date -u +%Y%m%dT%H%M%SZ).tar.gz"
# spec §9「bwg-tizi 上线」要求快照 /opt/b-ui + 单元文件；两个相位的单元名全收（4.1 的固定
# 六个 + 4.0.x 的槽位实例），还没落地的单元用 --ignore-failed-read 跳过而不是让整个 tar 失败。
UNIT_FILES=()
for unit in $(units_for before); do
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
# 升级输出单独留一份：开头那几行是 `format_plan` 打的升级计划，下面要在里面找 sing-box
UPOUT="$OUT/upgrade.out"
"$BUI" "${UP_ARGS[@]}" > "$UPOUT" 2>&1 || note_fail "upgrade-exit"
cat "$UPOUT" >> "$LOG"
wait_settle after-upgrade upgrade_settled
snapshot after-upgrade
after_ver=$(awk -F, '$1 == "after-upgrade" && $2 == "version" {print $3}' "$CSV")
log "升级后版本：$after_ver"
log "升级后二进制指纹（只记录，与 manifest 的比对见 M5 报告）：$(awk -F, '$1 == "after-upgrade" && $2 ~ /^sha:bin\// {printf "%s=%s ", $2, substr($3, 1, 12)}' "$CSV")"
if [[ -n "$TO" && "$after_ver" != "$TO" ]]; then
    log "FAIL 升级后版本 $after_ver != 目标 $TO"
    note_fail "upgrade-version"
fi
all_active after-upgrade || note_fail "upgrade-units"
# 三种订阅的 sha 对**每个**用户逐行不变 —— 4.1「零刷新」的判据本体
compare_subs before after-upgrade || note_fail "upgrade-subs"

# 自建 sing-box 必须真的装上（Global Constraints 第二波裁决 P-A）：内核身份现在是
# 「版本号 + 资产 sha256」，装不上就会让 `hy2-residential.json` 每轮 `sing-box check` 必 FATAL
# （`v2ray api is not included in this build`）、永不落盘，而同轮单元已指向它、旧
# config-residential*.yaml 已删 ⇒ 住宅 HY2 永久崩溃循环、没有自愈点。所以演练独立验，
# 不信任升级本身的退出码。
# 计划里那一行按**执行升级的二进制**的代次分档：4.0.x 的 `plan_upgrade` 只比版本号，同版本异
# sha 不进 `p.kernels`、`format_plan` 也就不打这一行 ⇒ 在 4.0.x → 4.1 这一跳上无条件要求它
# 等于把健康的升级判成 FAIL（而计划 Step 6 的口径是「任一相位 FAIL 就按 §9 回滚」）。自建那份
# 照样会被装上，只是由重启后的 4.1 守护进程按 `kernel_build_differs` 装（reconcile/diff.rs）。
# 4.2 及以后要复用本脚本，这里的代次判据得跟着 `plan_upgrade` 的口径一起改——glob 钉成
# `4.1.[0-9]*`：patch 段必须是数字，这样将来的 `4.10.x`（写成 `4.1*` 就会被吃掉）与 `4.1.`
# 这类残缺串（写成 `4.1.*` 会被吃掉）都落不进这一档。
if [[ "$before_ver" == 4.1.[0-9]* ]]; then
    if grep -q '^sing-box ' "$UPOUT"; then
        log "升级计划里的 sing-box：$(grep -m1 '^sing-box ' "$UPOUT")"
    else
        log "FAIL 升级计划里没有 sing-box 那一行——执行升级的 $before_ver 已带资产 sha256 判据，缺这一行就是自建二进制不会被装上"
        note_fail "upgrade-singbox-plan"
    fi
else
    log "note 升级由旧二进制（$before_ver）执行，它只比版本号（P-A 之前的判据），计划里不会有 sing-box 那一行——这一跳改由下面的 Tags 闸门判"
fi
if "$BASE/bin/sing-box" version 2>/dev/null | grep -q with_v2ray_api; then
    log "装上的 sing-box 带 with_v2ray_api"
else
    log "FAIL $BASE/bin/sing-box 的 Tags 里没有 with_v2ray_api——住宅配置过不了 sing-box check"
    note_fail "upgrade-singbox-tags"
fi
# 升级相位的 nft 与监听端口（spec §9.1）
expect after-upgrade nft:table present "nft 表" || note_fail "upgrade-nft"
expect after-upgrade nft:rules "$NFT_RULES_EXPECTED" "nft 规则" || note_fail "upgrade-nft"
expect after-upgrade "listen:udp:$HY2_RESI" yes "监听" || note_fail "upgrade-listen"
expect after-upgrade "listen:udp:$((HY2_RESI + 1))" no "监听" || note_fail "upgrade-listen"
# 配置转换落地没有：旧的每槽一份 yaml 一份不剩，新的那份 json 在盘上（spec §3.5）
expect after-upgrade count:resi-yaml 0 "旧住宅配置" || note_fail "upgrade-configs"
if [[ "$(awk -F, '$1 == "after-upgrade" && $2 == "sha:hy2-residential.json" {print $3}' "$CSV")" == "missing" ]]; then
    log "FAIL 升级后 $BASE/hy2-residential.json 不在盘上——住宅入站没有配置可跑"
    note_fail "upgrade-configs"
fi
# 4.0.x 的槽位实例必须一个不剩（plan T7 的部署闸门：`systemctl list-units 'hysteria-residential*'`
# 只有一个实例）：残留实例还占着 40000+i，而 4.1 指望这些端口被 nft REDIRECT 到 :40000
# ⇒ 那个槽的用户半通半不通。按槽序号逐个点名（上面那条 40001 只覆盖兼容段首端口，槽序号
# 有洞时覆盖不到别的槽）。
for i in $SLOTS; do
    [[ "$i" == "0" ]] && continue
    if slot_units_sampled; then
        expect after-upgrade "load:hysteria-residential-$i" not-found "槽 $i 的 4.0.x 实例单元" || note_fail "upgrade-resi-units"
    fi
    expect after-upgrade "listen:udp:$((HY2_RESI + i))" no "槽 $i 的 4.0.x 实例端口" || note_fail "upgrade-listen"
done

"$BUI" upgrade --rollback >> "$LOG" 2>&1 || note_fail "rollback-exit"
wait_settle after-rollback rollback_settled
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
# 回滚相位（spec §9.1「为什么表必须删」）：表留着 = 整段与兼容段全被吸进槽 0 的 apernet
# 进程，全体用户从槽 0 的 IP 出去、40000+i 无人应答 —— 比 2026-09-15 那次回归事故更糟。
expect after-rollback nft:table absent "nft 表" || note_fail "rollback-nft"
# 下面两条是 plan Produces 与 spec §9.1 点名的**冗余**守门，各自没有独立的失败路径：
# `compare_nft` 与上面那条 `nft:table absent` 冗余（表还在时先被它抓到），按槽的
# `listen:udp:40000+i` 与 `compare_listen` 冗余（端口没回来时先被它抓到）。故意留着。
compare_nft before after-rollback || note_fail "rollback-nft"
compare_listen before after-rollback || note_fail "rollback-listen"
# 按 state.json 的槽位表逐个点名：4.0.x 的每槽一个实例要回到 40000+i 且单元 active。
# `all_active` 会跳过 `not-found` 的单元，所以这一条是「该有的槽位单元没回来」的唯一守门。
for i in $SLOTS; do
    expect after-rollback "listen:udp:$((HY2_RESI + i))" yes "槽 $i 的监听" || note_fail "rollback-listen"
    slot_units_sampled || continue
    unit="hysteria-residential"
    [[ "$i" == "0" ]] || unit="hysteria-residential-$i"
    expect after-rollback "unit:$unit" active "槽 $i 的单元" || note_fail "rollback-resi-units"
done

if [[ -z "$FAILED" ]]; then
    log "PASS 升级与回滚均成功，订阅逐项未变，nft 表升级后 $NFT_RULES_EXPECTED 条 / 回滚后已删，按槽端口与单元已复原，bui 与四内核 sha256 已复原"
    printf 'verdict=PASS finished=%s before=%s upgraded=%s rolled_back=%s manifest=%s backup=%s\n' \
        "$(date -u +%FT%TZ)" "$before_ver" "$after_ver" "$back_ver" "${MANIFEST_URL:-默认}" "$BK" > "$DONE"
    exit 0
fi
log "FAIL 首个失败项：$FAILED"
printf 'verdict=FAIL first_failure=%s finished=%s manifest=%s backup=%s\n' \
    "$FAILED" "$(date -u +%FT%TZ)" "${MANIFEST_URL:-默认}" "$BK" > "$DONE"
exit 1
