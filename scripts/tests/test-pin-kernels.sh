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
  *SagerNet/sing-box*) tags='v1.14.8 v1.15.0 v1.14.4 v1.14.25 v1.14.5 v1.15.0-alpha.1 v1.14.9' ;;
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

assert_eq "1.14.25" "$(resolve_track SagerNet/sing-box 'minor:1.14')" "minor 按版本排序，25 > 9，忽略其他 minor"
assert_eq "1.15.0" "$(resolve_track SagerNet/sing-box 'minor:1.15')" "prerelease tag 不入选"
# 2026-10-10 计划 §1.1：sing-box 从 pin:v1.14.3 改成浮动 minor:1.14（吃到 1.14.x 最新 patch，
# 不自动跨 minor）。resolved_singbox 是下面各条断言的锚（stub 的 tag 表钉死了它）。
assert_eq "minor:1.14" "$SINGBOX_TRACK" "sing-box 跟 1.14.x 最新补丁走，不自动跨 minor"
resolved_singbox=$(resolve_track "$SINGBOX_REPO" "$SINGBOX_TRACK")
assert_eq "1.14.25" "$resolved_singbox" "浮动轨道解析到该 minor 下最高 patch"
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
bad=$(grep -vE '^#|^$' "$lock" | awk 'NF != 6 || length($5) != 64 || $5 !~ /^[0-9a-f]+$/ || $6 !~ /^https:\/\/github\.com\// {print}' | head -1)
assert_eq "" "$bad" "kernels.lock 每行 6 字段、sha256 合法、URL 是 github 资产"
# 4.1 起 sing-box 用上游官方发行物（自建链与它带出来的 build: 行一起退役）
assert_eq "0" "$(awk '$1 != "#" && $6 ~ /^build:/ {c++} END {print c + 0}' "$lock")" "锁里没有 build: 行"
nurl=$(awk '$1 == "sing-box" && $2 == "target" &&
    $6 == "https://github.com/SagerNet/sing-box/releases/download/v" $3 "/sing-box-" $3 "-linux-" $4 ".tar.gz" {c++} END {print c + 0}' "$lock")
assert_eq "2" "$nurl" "sing-box target 两行都是官方发行归档"
badarch=$(grep -vE '^#|^$' "$lock" | awk '$4 != "amd64" && $4 != "arm64" {print}' | head -1)
assert_eq "" "$badarch" "arch 列只有 amd64 / arm64（与 manifest 的 artifacts 键同口径）"
for k in sing-box xray hysteria caddy; do
    n=$(awk -v k="$k" '$1 == k && $2 == "target" {c++} END {print c + 0}' "$lock")
    assert_eq "2" "$n" "$k 有 amd64/arm64 两条 target 行"
done
n=$(awk '$1 == "sing-box" && $2 == "check" {c++} END {print c + 0}' "$lock")
assert_eq "0" "$n" "不再下载旧版本 check 内核"

# 锁必须贴着轨道解析值：锁里还钉着旧版本（1.14.3）而轨道是浮动 minor 时必须报漂移
# ——从前「轨道解析=1.14.3」是一致，现在 1.14.3 ≠ 1.14.25 就是漂移。
LOCK="$WORK/drift.lock"
cp "$lock" "$LOCK"
sed -i 's/^sing-box target [0-9][^ ]* /sing-box target 1.14.3 /' "$LOCK"
out=$(check_lock 2>&1); rc=$?
assert_eq "1" "$rc" "lock 与轨道解析不一致必须退 1"
assert_contains "漂移：sing-box lock=1.14.3 轨道解析=$resolved_singbox" "$out" "漂移信息给出轨道解析值"

# --warn 只降级「漂移」：同一个漂移的锁，带上 --warn 变成告警 + 退 0
WARN=1
out=$(check_lock 2>&1); rc=$?
WARN=0
assert_eq "0" "$rc" "--warn 下漂移退 0"
assert_contains "::warning::kernels.lock 落后于版本轨道" "$out" "--warn 下漂移发 GitHub 告警注解"

# ⚠️ 但 --warn **不能**把「锁坏了 / 核实不了」一起吞掉——那正是 fail-open
# （2026-10-10 安全审查与对抗复审各自独立指出过这个洞）。
LOCK="$WORK/short.lock"
grep -v '^sing-box ' "$lock" > "$LOCK"
WARN=1
out=$(check_lock 2>&1); rc=$?
WARN=0
assert_eq "1" "$rc" "lock 缺 sing-box 行：--warn 也退 1（lock 坏了不是漂移）"
assert_contains "锁里没有 sing-box 的行" "$out" "缺行给出可操作的诊断"
assert_not_contains "::warning::" "$out" "缺行不许降级成告警"

# —— --write：四个内核一律下载上游资产并现算 sha256（stub curl）——
# curl stub：remote_sha256 只用 curl 的 stdout 算 sha，给 URL 本身当内容即可（零网络）
cat > "$WORK/bin/curl" <<'STUB'
#!/usr/bin/env bash
url=""
for a in "$@"; do case "$a" in https://*) url="$a" ;; esac; done
[[ -n "$url" ]] || exit 22
printf '%s\n' "$url"
STUB
chmod 755 "$WORK/bin/curl"
LOCK_OUT="$WORK/out.lock"
# --lock 没被认出来时 --write 会去覆盖仓库里的真 lock（造过一次），所以前后对一次指纹
lock_before=$(sha256sum "$lock" | cut -d' ' -f1)
PATH="$WORK/bin:$PATH" bash "$ROOT/scripts/release/pin-kernels.sh" --write --lock "$LOCK_OUT" >/dev/null 2>&1
assert_eq "0" "$?" "--write 退 0"
assert_eq "$lock_before" "$(sha256sum "$lock" | cut -d' ' -f1)" "--write --lock 不许碰仓库里的 kernels.lock"
line=$(grep "^sing-box target $resolved_singbox amd64" "$LOCK_OUT")
assert_contains "https://github.com/SagerNet/sing-box/releases/download/v$resolved_singbox/sing-box-$resolved_singbox-linux-amd64.tar.gz" "$line" \
  "target 行是官方发行归档 URL（浮动轨道解析出的版本）"
# sha256 是「自己下载自己算」的，不是从别处抄来的：stub curl 的内容就是 URL 本身
want_sha=$(printf '%s\n' "https://github.com/SagerNet/sing-box/releases/download/v$resolved_singbox/sing-box-$resolved_singbox-linux-amd64.tar.gz" | sha256sum | cut -d' ' -f1)
assert_contains "$want_sha" "$line" "sha256 按下载内容现算"
assert_eq "0" "$(grep -c '^build:' "$LOCK_OUT")" "--write 不再生成 build: 行"
assert_eq "0" "$(grep -c '^sing-box check ' "$LOCK_OUT")" "--write 不再生成 check 行"

# —— --write 整文件重生成，但**保留手写注记行**：不保留就是静默数据丢失（周更 bot 的 PR
# 正文里也看不见注记被删），而那些注记正是「哪两行是回填的」这类只能手写的事实。
NOTES_LOCK="$WORK/notes.lock"
cat > "$NOTES_LOCK" <<'EOF'
# 由 scripts/release/pin-kernels.sh --write 生成，勿手工编辑（生成时间 2026-01-01T00:00:00Z）
# 手写注记甲：sing-box 为什么钉在固定版本
# 手写注记乙：hysteria 为什么顶了版本
# kernel role version arch sha256 url
sing-box target 1.14.4 amd64 1111111111111111111111111111111111111111111111111111111111111111 https://github.com/SagerNet/sing-box/releases/download/v1.14.4/sing-box-1.14.4-linux-amd64.tar.gz
EOF
PATH="$WORK/bin:$PATH" bash "$ROOT/scripts/release/pin-kernels.sh" --write --lock "$NOTES_LOCK" >/dev/null 2>&1
assert_eq "0" "$?" "--write 到带注记的锁上退 0"
assert_contains "# 手写注记甲：sing-box 为什么钉在固定版本" "$(cat "$NOTES_LOCK")" "手写注记甲被保留"
assert_contains "# 手写注记乙：hysteria 为什么顶了版本" "$(cat "$NOTES_LOCK")" "手写注记乙被保留"
assert_eq "1" "$(grep -c '^# 由 scripts/release/pin-kernels\.sh --write 生成' "$NOTES_LOCK")" \
  "生成时间那行由 --write 重写，不堆叠"
assert_eq "1" "$(grep -c '^# kernel role version arch sha256 url' "$NOTES_LOCK")" "列名行不堆叠"
assert_eq "$resolved_singbox" "$(awk '$1 == "sing-box" && $2 == "target" {print $3; exit}' "$NOTES_LOCK")" \
  "正文回到轨道解析值（1.14.4 → $resolved_singbox）"
# 新建锁（文件不存在）时不能因为读不到注记就失败
PATH="$WORK/bin:$PATH" bash "$ROOT/scripts/release/pin-kernels.sh" --write --lock "$WORK/fresh.lock" >/dev/null 2>&1
assert_eq "0" "$?" "--write 到不存在的锁上仍退 0"
assert_eq "2" "$(grep -c '^#' "$WORK/fresh.lock")" "新锁只有两行锁头"
finish
