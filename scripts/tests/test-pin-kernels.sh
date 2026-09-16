#!/usr/bin/env bash
# 轨道解析与资产 URL 拼装（stub git ls-remote，零网络）。
set -uo pipefail
LC_ALL=C
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
# shellcheck source=/dev/null
. "$HERE/lib.sh"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/bin"
# git stub：只认 ls-remote --tags。tag 列表故意乱序且混入 prerelease 与「大 patch」，
# 验证脚本自己用 sort -V 排序（真实世界里 GitHub 的顺序既不是版本序也不止一页）。
cat > "$WORK/bin/git" <<'STUB'
#!/usr/bin/env bash
url=""
for a in "$@"; do case "$a" in https://github.com/*) url="$a" ;; esac; done
case "$url" in
  *SagerNet/sing-box*) tags='v1.12.8 v1.15.0 v1.14.4 v1.12.25 v1.13.21 v1.14.5 v1.15.0-alpha.1 v1.12.9' ;;
  *XTLS/Xray-core*)    tags='v26.3.27 v26.4.0' ;;
  *apernet/hysteria*)  tags='app/v2.12.1 app/v1.3.5 app/v2.12.2' ;;
  *caddyserver/caddy*) tags='v2.9.1 v2.11.4 v2.10.1' ;;
  *) exit 128 ;;
esac
for t in $tags; do printf '%040d\trefs/tags/%s\n' 0 "$t"; done
STUB
chmod +x "$WORK/bin/git"
export PATH="$WORK/bin:$PATH"

BUI_PIN_SOURCED=1
# shellcheck source=/dev/null
. "$ROOT/scripts/release/pin-kernels.sh"
# pin-kernels.sh 顶部是 set -euo pipefail，源入后会污染测试外壳，必须关掉
set +eu

assert_eq "1.14.5" "$(resolve_track SagerNet/sing-box 'minor:1.14')" "minor:1.14 取 1.14 的最高 patch，忽略 1.15"
assert_eq "1.12.25" "$(resolve_track SagerNet/sing-box 'minor:1.12')" "minor:1.12 取 1.12.25（sort -V：25 > 9，且不受列表顺序影响）"
assert_eq "1.13.21" "$(resolve_track SagerNet/sing-box 'minor:1.13')" "minor:1.13 取 1.13.21"
assert_eq "1.15.0" "$(resolve_track SagerNet/sing-box 'minor:1.15')" "prerelease tag v1.15.0-alpha.1 不入选（正则要求 ^v<minor>\\.<patch>$）"
assert_eq "26.3.27" "$(resolve_track XTLS/Xray-core 'pin:v26.3.27')" "pin 轨道原样返回，不查网络"
assert_eq "2.12.2" "$(resolve_track apernet/hysteria 'appmajor:2')" "appmajor:2 去掉 app/v 前缀且不选 v1"
assert_eq "2.11.4" "$(resolve_track caddyserver/caddy 'major:2')" "major:2 取最新 2.x"

assert_eq "https://github.com/SagerNet/sing-box/releases/download/v1.14.5/sing-box-1.14.5-linux-amd64.tar.gz" \
    "$(asset_url sing-box 1.14.5 amd64)" "sing-box amd64 资产 URL"
assert_eq "https://github.com/XTLS/Xray-core/releases/download/v26.3.27/Xray-linux-arm64-v8a.zip" \
    "$(asset_url xray 26.3.27 arm64)" "xray arm64 资产 URL"
assert_eq "https://github.com/apernet/hysteria/releases/download/app/v2.12.2/hysteria-linux-amd64" \
    "$(asset_url hysteria 2.12.2 amd64)" "hysteria amd64 资产 URL（app/v 前缀）"
assert_eq "https://github.com/caddyserver/caddy/releases/download/v2.11.4/caddy_2.11.4_linux_arm64.tar.gz" \
    "$(asset_url caddy 2.11.4 arm64)" "caddy arm64 资产 URL"

# lock 已提交且格式合法
lock="$ROOT/scripts/release/kernels.lock"
assert_eq "0" "$([[ -f "$lock" ]] && echo 0 || echo 1)" "kernels.lock 已提交"
bad=$(grep -vE '^#|^$' "$lock" | awk 'NF != 6 || $5 !~ /^[0-9a-f]{64}$/ || $6 !~ /^(https:\/\/github\.com\/|build:)/ {print}' | head -1)
assert_eq "" "$bad" "kernels.lock 每行 6 字段、sha256 合法、URL 是 github 资产或 build: URI"
# 随发布分发的 sing-box 必须是自建（spec §5.3：官方归档不带 with_v2ray_api，住宅计量要它）
nbuild=$(awk '$1 == "sing-box" && $2 == "target" && $6 ~ /^build:.*;tags=.*with_v2ray_api/ {c++} END {print c + 0}' "$lock")
assert_eq "2" "$nbuild" "sing-box target 两行是 build: URI 且标签含 with_v2ray_api"
badarch=$(grep -vE '^#|^$' "$lock" | awk '$4 != "amd64" && $4 != "arm64" {print}' | head -1)
assert_eq "" "$badarch" "arch 列只有 amd64 / arm64（与 manifest 的 artifacts 键同口径）"
for k in sing-box xray hysteria caddy; do
    n=$(awk -v k="$k" '$1 == k && $2 == "target" {c++} END {print c + 0}' "$lock")
    assert_eq "2" "$n" "$k 有 amd64/arm64 两条 target 行"
done
n=$(awk '$1 == "sing-box" && $2 == "check" {c++} END {print c + 0}' "$lock")
assert_eq "2" "$n" "sing-box 有 1.12/1.13 两条 check 行（amd64）"

# --check 必须同时盯 check minor：只比 4 个 target 的话，CI 发现不了 1.12 轨道漂移，
# SINGBOX_CHECK_MINORS 里的 1.12 就钉不住，CI 的 1.12 矩阵会失去数据源。
LOCK="$WORK/drift.lock"
cat > "$LOCK" <<'EOF'
# kernel role version arch sha256 url
sing-box target 1.14.5 amd64 9999999999999999999999999999999999999999999999999999999999999999 https://github.com/SagerNet/sing-box/releases/download/v1.14.5/sing-box-1.14.5-linux-amd64.tar.gz
xray target 26.3.27 amd64 7777777777777777777777777777777777777777777777777777777777777777 https://github.com/XTLS/Xray-core/releases/download/v26.3.27/Xray-linux-64.zip
hysteria target 2.12.2 amd64 5555555555555555555555555555555555555555555555555555555555555555 https://github.com/apernet/hysteria/releases/download/app/v2.12.2/hysteria-linux-amd64
caddy target 2.11.4 amd64 bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb https://github.com/caddyserver/caddy/releases/download/v2.11.4/caddy_2.11.4_linux_amd64.tar.gz
sing-box check 1.12.9 amd64 dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd https://github.com/SagerNet/sing-box/releases/download/v1.12.9/sing-box-1.12.9-linux-amd64.tar.gz
sing-box check 1.13.21 amd64 eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee https://github.com/SagerNet/sing-box/releases/download/v1.13.21/sing-box-1.13.21-linux-amd64.tar.gz
EOF
out=$(check_lock 2>&1); rc=$?
assert_eq "1" "$rc" "check minor 漂移（lock 1.12.9 / 轨道 1.12.25）也要退 1"
assert_contains "sing-box(check 1.12)" "$out" "漂移信息点名到 check minor"
assert_not_contains "sing-box(check 1.13)" "$(printf '%s\n' "$out" | grep 漂移)" "1.13 没漂移就不报"

# —— 4.1：--write 的自建模式写出 build: URI（stub 构建器）——
# curl stub：remote_sha256 只用 curl 的 stdout 算 sha，给 URL 本身当内容即可（零网络）
cat > "$WORK/bin/curl" <<'STUB'
#!/usr/bin/env bash
url=""
for a in "$@"; do case "$a" in https://*) url="$a" ;; esac; done
[[ -n "$url" ]] || exit 22
printf '%s\n' "$url"
STUB
chmod 755 "$WORK/bin/curl"
export ARGV_LOG="$WORK/argv.log"
cat > "$WORK/bin/build-singbox.sh" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$ARGV_LOG"
out=""; while [[ $# -gt 0 ]]; do case "$1" in --out) out="$2"; shift 2 ;; *) shift ;; esac; done
printf 'fake\n' > "$out"
printf 'go=go1.25.4 tags=with_quic,with_v2ray_api sha256=%s\n' "$(sha256sum "$out" | cut -d' ' -f1)"
STUB
chmod 755 "$WORK/bin/build-singbox.sh"
: > "$ARGV_LOG"
LOCK_OUT="$WORK/out.lock"
# --lock 没被认出来时 --write 会去覆盖仓库里的真 lock（造过一次），所以前后对一次指纹
lock_before=$(sha256sum "$lock" | cut -d' ' -f1)
PATH="$WORK/bin:$PATH" BUI_SINGBOX_BUILDER="$WORK/bin/build-singbox.sh" \
  bash "$ROOT/scripts/release/pin-kernels.sh" --write --lock "$LOCK_OUT" >/dev/null 2>&1
assert_eq "0" "$?" "--write 自建模式退 0"
assert_eq "$lock_before" "$(sha256sum "$lock" | cut -d' ' -f1)" "--write --lock 不许碰仓库里的 kernels.lock"
line=$(grep '^sing-box target 1.14.5 amd64' "$LOCK_OUT")
assert_contains "build:SagerNet/sing-box@v1.14.5;go=go1.25.4;tags=with_quic,with_v2ray_api" "$line" \
  "target 行写成 build: URI（版本来自轨道 minor:1.14 的最高 patch）"
assert_contains "https://github.com/SagerNet/sing-box/releases/download/v1.12.25/" \
  "$(grep '^sing-box check 1.12' "$LOCK_OUT")" "check 行仍是上游归档 URL"
# 构建器的 argv 也要钉：只看锁里那一行的话，把 --arch 丢了、两轮都传 amd64 也照样绿
assert_eq "1" "$(grep -c -- '--arch amd64' "$ARGV_LOG")" "amd64 被构建一次"
assert_eq "1" "$(grep -c -- '--arch arm64' "$ARGV_LOG")" "arm64 被构建一次"
assert_eq "2" "$(grep -c -- '--repo SagerNet/sing-box --version 1.14.5' "$ARGV_LOG")" "两次都用轨道解析出的版本"
assert_eq "0" "$(grep -c -- '--go' "$ARGV_LOG")" "pin 时不钉工具链（GOTOOLCHAIN=auto 发现版本，写进锁的 go=）"

# —— --write 整文件重生成，但**保留手写注记行**：不保留就是静默数据丢失（周更 bot 的 PR
# 正文里也看不见注记被删），而那些注记正是「哪两行是回填的」这类只能手写的事实。
NOTES_LOCK="$WORK/notes.lock"
cat > "$NOTES_LOCK" <<'EOF'
# 由 scripts/release/pin-kernels.sh --write 生成，勿手工编辑（生成时间 2026-01-01T00:00:00Z）
# 手写注记甲：sing-box target 两行是回填的
# 手写注记乙：hysteria 为什么顶了版本
# kernel role version arch sha256 url
sing-box target 1.14.4 amd64 1111111111111111111111111111111111111111111111111111111111111111 build:SagerNet/sing-box@v1.14.4;go=go1.25.4;tags=with_quic,with_v2ray_api
EOF
PATH="$WORK/bin:$PATH" BUI_SINGBOX_BUILDER="$WORK/bin/build-singbox.sh" \
  bash "$ROOT/scripts/release/pin-kernels.sh" --write --lock "$NOTES_LOCK" >/dev/null 2>&1
assert_eq "0" "$?" "--write 到带注记的锁上退 0"
assert_contains "# 手写注记甲：sing-box target 两行是回填的" "$(cat "$NOTES_LOCK")" "手写注记甲被保留"
assert_contains "# 手写注记乙：hysteria 为什么顶了版本" "$(cat "$NOTES_LOCK")" "手写注记乙被保留"
assert_eq "1" "$(grep -c '^# 由 scripts/release/pin-kernels\.sh --write 生成' "$NOTES_LOCK")" \
  "生成时间那行由 --write 重写，不堆叠"
assert_eq "1" "$(grep -c '^# kernel role version arch sha256 url' "$NOTES_LOCK")" "列名行不堆叠"
assert_eq "1.14.5" "$(awk '$1 == "sing-box" && $2 == "target" {print $3; exit}' "$NOTES_LOCK")" \
  "正文照轨道重写（1.14.4 → 1.14.5）"
# 新建锁（文件不存在）时不能因为读不到注记就失败
PATH="$WORK/bin:$PATH" BUI_SINGBOX_BUILDER="$WORK/bin/build-singbox.sh" \
  bash "$ROOT/scripts/release/pin-kernels.sh" --write --lock "$WORK/fresh.lock" >/dev/null 2>&1
assert_eq "0" "$?" "--write 到不存在的锁上仍退 0"
assert_eq "2" "$(grep -c '^#' "$WORK/fresh.lock")" "新锁只有两行锁头"

# --check 也要盯自建行的 tags=：只比版本号的话，改了 env 的 SINGBOX_TAGS 而忘了 --write
# 是静默 no-op（取件读锁里的 tags=），env 里「Tags 必须 ⊇ 官方」那条纪律就没人兜着。
LOCK="$WORK/tags.lock"
tags_lock() { # $1 = 锁里 sing-box target 两行的 tags=
  { printf '# kernel role version arch sha256 url\n'
    for a in amd64 arm64; do
      printf 'sing-box target 1.14.5 %s 9999999999999999999999999999999999999999999999999999999999999999 build:SagerNet/sing-box@v1.14.5;go=go1.25.4;tags=%s\n' "$a" "$1"
    done
    printf 'xray target 26.3.27 amd64 7777777777777777777777777777777777777777777777777777777777777777 https://github.com/XTLS/Xray-core/releases/download/v26.3.27/Xray-linux-64.zip\n'
    printf 'hysteria target 2.12.2 amd64 5555555555555555555555555555555555555555555555555555555555555555 https://github.com/apernet/hysteria/releases/download/app/v2.12.2/hysteria-linux-amd64\n'
    printf 'caddy target 2.11.4 amd64 bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb https://github.com/caddyserver/caddy/releases/download/v2.11.4/caddy_2.11.4_linux_amd64.tar.gz\n'
    printf 'sing-box check 1.12.25 amd64 dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd https://github.com/SagerNet/sing-box/releases/download/v1.12.25/sing-box-1.12.25-linux-amd64.tar.gz\n'
    printf 'sing-box check 1.13.21 amd64 eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee https://github.com/SagerNet/sing-box/releases/download/v1.13.21/sing-box-1.13.21-linux-amd64.tar.gz\n'
  } > "$LOCK"
}

tags_lock "$SINGBOX_TAGS"
out=$(check_lock 2>&1); rc=$?
assert_eq "0" "$rc" "版本与 tags 都贴着 env 时 --check 退 0"
assert_contains "一致：sing-box tags" "$out" "报告 tags 一致"

tags_lock "${SINGBOX_TAGS%,with_v2ray_api}"
out=$(check_lock 2>&1); rc=$?
assert_eq "1" "$rc" "锁里 tags= 与 env 的 SINGBOX_TAGS 不符退 1"
assert_contains "漂移：sing-box tags" "$out" "漂移信息点名 tags"

tags_lock "$SINGBOX_TAGS"
sed -i '2s/;tags=.*/;tags=only-one-row-changed/' "$LOCK"
out=$(check_lock 2>&1); rc=$?
assert_eq "1" "$rc" "两行 tags= 不一致也算漂移"
finish
