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
bad=$(grep -vE '^#|^$' "$lock" | awk 'NF != 6 || $5 !~ /^[0-9a-f]{64}$/ || $6 !~ /^https:\/\/github\.com\// {print}' | head -1)
assert_eq "" "$bad" "kernels.lock 每行 6 字段、sha256 合法、URL 是 github 资产"
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
finish
