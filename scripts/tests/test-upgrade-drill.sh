#!/usr/bin/env bash
# 演练脚本：成功路径退 0；订阅在升级后漂移时退 1。全 stub，零网络。
set -uo pipefail
LC_ALL=C
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
# shellcheck source=/dev/null
. "$HERE/lib.sh"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/bin" "$WORK/base/bin"
# 演练按 state.json 里的 users[].sub_token 拼订阅 URL（2026-09-14 裁决：四个免鉴权端点
# 不再认用户名），所以 stub 的 state.json 得是真 JSON 且带 token
ALICE_TOK=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
BOB_TOK=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
write_state() {  # $1 = bob 的 sub_token（空串 = bob 还没补齐 token）
    if [[ -n "${1:-}" ]]; then
        printf '{"users":[{"username":"alice","sub_token":"%s"},{"username":"bob","sub_token":"%s"}]}\n' \
            "$ALICE_TOK" "$1" > "$WORK/base/state.json"
    else
        printf '{"users":[{"username":"alice","sub_token":"%s"},{"username":"bob"}]}\n' \
            "$ALICE_TOK" > "$WORK/base/state.json"
    fi
}
write_state "$BOB_TOK"
printf 'listen: :10000,20000-30000\n' > "$WORK/base/config.yaml"
printf '{}\n' > "$WORK/base/xray-config.json"
printf '4.0.0\n' > "$WORK/version"
for b in bui hysteria xray sing-box caddy; do
    printf 'bin-%s-v1\n' "$b" > "$WORK/base/bin/$b"
done

# bui stub：记录 argv；--version 读文件；upgrade 改版本并换掉内核二进制（模拟「内核随 manifest 升级」）；
# --rollback 把版本与内核都改回去，除非 FAKE_KERNEL_STUCK=1（模拟只回 bui 不回内核的实现）
cat > "$WORK/bin/bui" <<'STUB'
#!/usr/bin/env bash
printf 'bui %s\n' "$*" >> "$BUI_LOG"
kernels() { printf 'bui hysteria xray sing-box caddy\n'; }
case "${1:-}" in
  --version) cat "$VERFILE" ;;
  upgrade)
    if [[ "${2:-}" == "--rollback" ]]; then
      printf '%s\n' "$(cat "$VERFILE.prev")" > "$VERFILE"
      if [[ "${FAKE_KERNEL_STUCK:-0}" != "1" ]]; then
        for b in $(kernels); do printf 'bin-%s-v1\n' "$b" > "$BASEDIR/bin/$b"; done
      fi
    else
      printf '%s\n' "$(cat "$VERFILE")" > "$VERFILE.prev"
      # --version <x.y.z> 在 $2 $3；后面可能还跟 --manifest-url <url>
      printf '%s\n' "${3:-4.0.1}" > "$VERFILE"
      for b in $(kernels); do printf 'bin-%s-v2\n' "$b" > "$BASEDIR/bin/$b"; done
      # FAKE_UPGRADE_DROPS_TOKEN=1：模拟目标版本写出的 state 不带 sub_token（rc11 形状）
      if [[ "${FAKE_UPGRADE_DROPS_TOKEN:-0}" == "1" ]]; then
        printf '{"users":[{"username":"alice"},{"username":"bob"}]}\n' > "$BASEDIR/state.json"
      fi
    fi
    printf 'upgrade ok\n'
    ;;
  *) printf 'unknown\n'; exit 1 ;;
esac
STUB
# systemctl stub：一律 active、NRestarts 固定
cat > "$WORK/bin/systemctl" <<'STUB'
#!/usr/bin/env bash
case "${1:-}" in
  is-active) printf 'active\n' ;;
  show) printf '0\n' ;;
  *) printf '\n' ;;
esac
STUB
# curl stub：订阅内容 = URL 末段（订阅 token）+ 升级后是否漂移；请求过的 URL 记账，
# 用来验「拼的是 token 不是用户名」。FAKE_404=1 模拟按用户名取时的 404：curl -f 退 22、body 空。
# URL 只从 `-K -` 的 stdin 配置里读（token 进了 argv 就取不到内容 ⇒ 判失败），argv 另记一份
# 进 $CURL_ARGV_LOG，断言凭据没进命令行
cat > "$WORK/bin/curl" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$CURL_ARGV_LOG"
url=""
if [[ "$*" == *"-K -"* ]]; then
  while IFS= read -r line; do
    case "$line" in url\ =\ *) url=${line#url = }; url=${url//\"/} ;; esac
  done
fi
printf '%s\n' "$url" >> "$CURL_LOG"
if [[ "${FAKE_404:-0}" == "1" ]]; then exit 22; fi
body="sub:${url##*/}"
if [[ "${FAKE_DRIFT:-0}" == "1" && -f "$VERFILE.prev" ]]; then body="$body-drifted"; fi
printf '%s\n' "$body"
STUB
# tar stub：只记录被调用，避免真的打包 /opt
cat > "$WORK/bin/tar" <<'STUB'
#!/usr/bin/env bash
printf 'tar %s\n' "$*" >> "$TAR_LOG"
for a in "$@"; do case "$a" in *.tar.gz) : > "$a" ;; esac; done
STUB
chmod +x "$WORK/bin"/*
export PATH="$WORK/bin:$PATH" VERFILE="$WORK/version" TAR_LOG="$WORK/tar.log" \
       BUI_LOG="$WORK/bui.log" BASEDIR="$WORK/base" CURL_LOG="$WORK/curl.log" \
       CURL_ARGV_LOG="$WORK/curl.argv.log"

reset_env() {
    rm -f "$WORK/version.prev" "$WORK/bui.log" "$WORK/curl.log" "$WORK/curl.argv.log"
    printf '4.0.0\n' > "$WORK/version"
    write_state "$BOB_TOK"
    for b in bui hysteria xray sing-box caddy; do
        printf 'bin-%s-v1\n' "$b" > "$WORK/base/bin/$b"
    done
}

run() {
    bash "$ROOT/scripts/ops/upgrade-drill.sh" --to 4.0.1 \
        --manifest-url http://127.0.0.1:8000/v4.0.1/manifest.json --users alice,bob \
        --out "$1" --base "$WORK/base" --bui "$WORK/bin/bui" --api http://127.0.0.1:8080
}

reset_env
out=$(run "$WORK/ok" 2>&1); rc=$?
assert_eq "0" "$rc" "成功路径退 0"
assert_contains "PASS" "$out" "输出 PASS"
assert_eq "1" "$([[ -f "$WORK/ok/DONE" ]] && echo 1 || echo 0)" "写了 DONE"
assert_contains "verdict=PASS" "$(cat "$WORK/ok/DONE")" "DONE 里写结论"
assert_contains "manifest=http://127.0.0.1:8000/v4.0.1/manifest.json" "$(cat "$WORK/ok/DONE")" "DONE 记下用的是哪份 manifest"
assert_contains "upgrade --version 4.0.1 --manifest-url http://127.0.0.1:8000/v4.0.1/manifest.json" \
    "$(cat "$WORK/bui.log")" "按 C5 传 --version 与 --manifest-url"
assert_contains "upgrade --rollback" "$(cat "$WORK/bui.log")" "回滚用 upgrade --rollback"
assert_eq "phase,key,value" "$(head -1 "$WORK/ok/drill.csv")" "CSV 表头"
assert_eq "3" "$(tail -n +2 "$WORK/ok/drill.csv" | awk -F, '{print $1}' | sort -u | wc -l)" "三个 phase 都记了"
assert_eq "4.0.1" "$(awk -F, '$1 == "after-upgrade" && $2 == "version" {print $3}' "$WORK/ok/drill.csv")" "升级后版本 = --to"
assert_eq "4.0.0" "$(awk -F, '$1 == "after-rollback" && $2 == "version" {print $3}' "$WORK/ok/drill.csv")" "回滚后版本复原"
assert_eq "6" "$(awk -F, '$1 == "before" && $2 ~ /^sub:/ {c++} END {print c + 0}' "$WORK/ok/drill.csv")" "2 用户 × 3 种订阅指纹"
assert_eq "5" "$(awk -F, '$1 == "before" && $2 ~ /^sha:bin\// {c++} END {print c + 0}' "$WORK/ok/drill.csv")" "bui + 四内核 = 5 条 sha:bin 行"
assert_eq "1" "$([[ "$(awk -F, '$1 == "before" && $2 == "sha:bin/sing-box" {print $3}' "$WORK/ok/drill.csv")" \
    != "$(awk -F, '$1 == "after-upgrade" && $2 == "sha:bin/sing-box" {print $3}' "$WORK/ok/drill.csv")" ]] && echo 1 || echo 0)" \
    "升级后内核 sha 变了（内核随 manifest 升级）"
assert_eq "$(awk -F, '$1 == "before" && $2 == "sha:bin/sing-box" {print $3}' "$WORK/ok/drill.csv")" \
    "$(awk -F, '$1 == "after-rollback" && $2 == "sha:bin/sing-box" {print $3}' "$WORK/ok/drill.csv")" \
    "回滚后内核 sha 复原"
# 2026-09-14 裁决：订阅 URL 的末段是 sub_token，按用户名取会 404 ⇒ 演练必须拼 token
assert_contains "/api/sub/$ALICE_TOK" "$(cat "$WORK/curl.log")" "订阅 URL 拼的是 sub_token"
assert_contains "/api/clash/$BOB_TOK" "$(cat "$WORK/curl.log")" "三种订阅都按 token 取"
assert_not_contains "/api/sub/alice" "$(cat "$WORK/curl.log")" "不再按用户名取订阅"
# 末段是凭据，只许经 `-K -` 的 stdin 传：进了 argv 就会被 ps 看见（CLAUDE.md 的硬规矩）
assert_not_contains "/api/" "$(cat "$WORK/curl.argv.log")" "订阅 URL 不进 curl 的 argv"
assert_contains "-K -" "$(cat "$WORK/curl.argv.log")" "URL 经 stdin 的 curl 配置传"
assert_contains "backup-" "$(cat "$WORK/tar.log")" "演练前打了快照"
assert_contains "etc/systemd/system/hysteria-residential.service" "$(cat "$WORK/tar.log")" "快照含六个单元文件（不只 b-ui.service）"
assert_contains "--ignore-failed-read" "$(cat "$WORK/tar.log")" "缺失单元不让 tar 整体失败"

reset_env
out=$(FAKE_DRIFT=1 run "$WORK/drift" 2>&1); rc=$?
assert_eq "1" "$rc" "订阅漂移退 1"
assert_contains "FAIL 订阅漂移" "$out" "指出订阅漂移"
assert_contains "verdict=FAIL" "$(cat "$WORK/drift/DONE")" "DONE 记 FAIL"

# --rollback 只回 bui 不回内核 → 必须 FAIL（spec §11 风险 3：内核版本受 manifest 控制可回退）
reset_env
out=$(FAKE_KERNEL_STUCK=1 run "$WORK/stuck" 2>&1); rc=$?
assert_eq "1" "$rc" "回滚没复原内核退 1"
assert_contains "FAIL 二进制未复原 sha:bin/sing-box" "$out" "点名哪个二进制没复原"
assert_contains "first_failure=rollback-kernels" "$(cat "$WORK/stuck/DONE")" "DONE 记 rollback-kernels"

# 订阅取不到（按用户名取的 404、面板没起来……）必须 FAIL：旧写法两个相位都拿空 body，
# `sha_str ""` 两两相等，`compare_subs` 就判「订阅无漂移」——假绿比失败更坏
reset_env
out=$(FAKE_404=1 run "$WORK/notfound" 2>&1); rc=$?
assert_eq "1" "$rc" "订阅取不到退 1"
assert_contains "FAIL before：取 alice 的 sub 订阅失败或返回空" "$out" "点名哪个用户哪种订阅取不到"
assert_contains "first_failure=sub-fetch:before:alice:sub" "$(cat "$WORK/notfound/DONE")" "DONE 记 sub-fetch"
assert_eq "0" "$(awk -F, '$2 ~ /^sub:/ {c++} END {print c + 0}' "$WORK/notfound/drill.csv")" \
    "取不到就不记 sub: 指纹（不留两个空串给 compare_subs 比出假绿）"

# state.json 里没有 sub_token（rc11 及更早的 state 没有这个字段）⇒ 前置条件不满足：FATAL 退 2，
# 不能报成订阅漂移或取订阅失败（在 rc11 上跑 before 相位看起来会像升级坏了），也不能静默跳过这个用户
reset_env
write_state ""
out=$(run "$WORK/notok" 2>&1); rc=$?
assert_eq "2" "$rc" "缺 sub_token 退 2"
assert_contains "FATAL before：state.json 里 bob 没有 sub_token" "$out" "点名哪个相位、谁没有 token"
assert_contains "本演练要求升级前后两端都是 rc12 及以上（该相位的 state.json 没有订阅 token）" "$out" "说清适用范围"
assert_not_contains "订阅漂移" "$out" "不报成订阅漂移"
assert_not_contains "订阅失败" "$out" "不报成取订阅失败"
assert_contains "verdict=FATAL reason=sub-token-missing:before:bob" "$(cat "$WORK/notok/DONE")" "DONE 记 FATAL 与原因"
assert_eq "0" "$([[ -s "$WORK/curl.log" ]] && echo 1 || echo 0)" "一条订阅都没取"
assert_not_contains "upgrade" "$(cat "$WORK/bui.log")" "没去升级"

# 升级之后的相位没有 sub_token（目标版本的 state 不带这个字段）同样 FATAL，并说明回滚没做
reset_env
out=$(FAKE_UPGRADE_DROPS_TOKEN=1 run "$WORK/droptok" 2>&1); rc=$?
assert_eq "2" "$rc" "升级后缺 sub_token 退 2"
assert_contains "FATAL after-upgrade：state.json 里 alice 没有 sub_token" "$out" "点名 after-upgrade 相位"
assert_contains "（该相位的 state.json 没有订阅 token）；升级已执行、回滚未执行" "$out" "提示回滚没做"
assert_not_contains "订阅漂移" "$out" "升级后缺 token 也不报成订阅漂移"
assert_contains "verdict=FATAL reason=sub-token-missing:after-upgrade:alice" "$(cat "$WORK/droptok/DONE")" "DONE 记 after-upgrade 的 FATAL"

# 缺 --users 直接退 2（别在生产上跑出一份没有订阅指纹的空演练）
out=$(bash "$ROOT/scripts/ops/upgrade-drill.sh" --to 4.0.1 --out "$WORK/nouser" 2>&1); rc=$?
assert_eq "2" "$rc" "缺 --users 退 2"
finish
