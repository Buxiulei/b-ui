#!/usr/bin/env bash
# fetch-kernels.sh：缓存跳过 / 镜像回退 / sha 不匹配 / 归档解包（stub curl 提供本地假归档）。
set -uo pipefail
LC_ALL=C
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
# shellcheck source=/dev/null
. "$HERE/lib.sh"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/bin" "$WORK/srv" "$WORK/out"

# 造三份上游资产：sing-box tar.gz、xray zip、hysteria 裸二进制
mkdir -p "$WORK/build/sing-box-1.14.5-linux-amd64"
printf '#!/bin/sh\necho sing-box 1.14.5\n' > "$WORK/build/sing-box-1.14.5-linux-amd64/sing-box"
tar -C "$WORK/build" -czf "$WORK/srv/singbox.tar.gz" sing-box-1.14.5-linux-amd64
printf '#!/bin/sh\necho xray\n' > "$WORK/build/xray"
(cd "$WORK/build" && zip -q "$WORK/srv/xray.zip" xray)
printf '#!/bin/sh\necho hysteria\n' > "$WORK/srv/hysteria"
# caddy 按真实上游布局打包：顶层 caddy + LICENSE + README.md
printf '#!/bin/sh\necho caddy\n' > "$WORK/build/caddy"
printf 'LICENSE\n' > "$WORK/build/LICENSE"
printf 'README\n' > "$WORK/build/README.md"
tar -C "$WORK/build" -czf "$WORK/srv/caddy.tar.gz" caddy LICENSE README.md

sha_of() { sha256sum "$1" | cut -d' ' -f1; }
SB_SHA=$(sha_of "$WORK/srv/singbox.tar.gz")
XR_SHA=$(sha_of "$WORK/srv/xray.zip")
HY_SHA=$(sha_of "$WORK/srv/hysteria")
CD_SHA=$(sha_of "$WORK/srv/caddy.tar.gz")

cat > "$WORK/bin/curl" <<'STUB'
#!/usr/bin/env bash
out=""; url=""
while [[ $# -gt 0 ]]; do
  case "$1" in -o) out="$2"; shift 2 ;; http*) url="$1"; shift ;; *) shift ;; esac
done
printf '%s\n' "$url" >> "$CURL_LOG"
# GitHub 直连按 FAKE_GITHUB_OK 决定成败；镜像前缀总是成功
if [[ "$url" == https://github.com/* && "${FAKE_GITHUB_OK:-0}" != "1" ]]; then exit 22; fi
src=""
case "$url" in
  *sing-box-1.14.5-linux-amd64.tar.gz) src="$SRV/singbox.tar.gz" ;;
  *Xray-linux-64.zip)                  src="$SRV/xray.zip" ;;
  *hysteria-linux-amd64)               src="$SRV/hysteria" ;;
  *caddy_2.11.4_linux_amd64.tar.gz)    src="$SRV/caddy.tar.gz" ;;
  *) exit 22 ;;
esac
if [[ "${FAKE_CORRUPT:-0}" == "1" ]]; then printf 'corrupt' > "$out"; else cp "$src" "$out"; fi
STUB
chmod +x "$WORK/bin/curl"
export PATH="$WORK/bin:$PATH" SRV="$WORK/srv" CURL_LOG="$WORK/curl.log"

LOCK="$WORK/kernels.lock"
{
  printf '# kernel role version arch sha256 url\n'
  printf 'sing-box target 1.14.5 amd64 %s https://github.com/SagerNet/sing-box/releases/download/v1.14.5/sing-box-1.14.5-linux-amd64.tar.gz\n' "$SB_SHA"
  printf 'xray target 26.3.27 amd64 %s https://github.com/XTLS/Xray-core/releases/download/v26.3.27/Xray-linux-64.zip\n' "$XR_SHA"
  printf 'hysteria target 2.12.2 amd64 %s https://github.com/apernet/hysteria/releases/download/app/v2.12.2/hysteria-linux-amd64\n' "$HY_SHA"
  printf 'caddy target 2.11.4 amd64 %s https://github.com/caddyserver/caddy/releases/download/v2.11.4/caddy_2.11.4_linux_amd64.tar.gz\n' "$CD_SHA"
  printf 'sing-box check 1.14.5 amd64 %s https://github.com/SagerNet/sing-box/releases/download/v1.14.5/sing-box-1.14.5-linux-amd64.tar.gz\n' "$SB_SHA"
} > "$LOCK"

run() { BUI_MIRRORS="https://mirror-a.example/" bash "$ROOT/scripts/ci/fetch-kernels.sh" --out "$WORK/out" --lock "$LOCK" --arch amd64 "$@"; }

: > "$CURL_LOG"
out=$(run --role all 2>&1); rc=$?
assert_eq "0" "$rc" "首次获取成功（GitHub 失败 → 镜像回退）"
assert_eq "1" "$([[ -x "$WORK/out/bin/sing-box" ]] && echo 1 || echo 0)" "tar.gz 解出可执行 sing-box"
assert_eq "1" "$([[ -x "$WORK/out/bin/xray" ]] && echo 1 || echo 0)" "zip 解出可执行 xray"
assert_eq "1" "$([[ -x "$WORK/out/bin/hysteria" ]] && echo 1 || echo 0)" "裸二进制落地并可执行"
assert_eq "1" "$([[ -x "$WORK/out/bin/caddy" ]] && echo 1 || echo 0)" "caddy tar.gz 解出可执行裸二进制"
assert_eq "0" "$([[ -e "$WORK/out/bin/LICENSE" || -e "$WORK/out/bin/README.md" ]] && echo 1 || echo 0)" "归档里的非二进制文件不泄漏进 bin/"
assert_eq "1" "$([[ -x "$WORK/out/singbox/1.14/sing-box" ]] && echo 1 || echo 0)" "check 版本按 minor 目录落地"
assert_contains "https://mirror-a.example/https://github.com/" "$(cat "$CURL_LOG")" "用了镜像前缀"
assert_eq "5" "$(grep -c . "$WORK/out/.fetched")" "清单记录 5 项"

: > "$CURL_LOG"
out=$(run --role all 2>&1); rc=$?
assert_eq "0" "$rc" "二次运行成功"
assert_eq "0" "$(grep -c . "$CURL_LOG")" "已就位则零下载（cache 命中语义）"
assert_contains "cached" "$out" "输出说明命中缓存"

: > "$CURL_LOG"
rm -rf "$WORK/out"
out=$(FAKE_CORRUPT=1 run --role target 2>&1); rc=$?
assert_eq "4" "$rc" "sha256 不匹配退 4"
assert_contains "sha256 不匹配" "$out" "有中文错误"
assert_eq "0" "$([[ -e "$WORK/out/bin/sing-box" ]] && echo 1 || echo 0)" "坏文件不留在输出目录"

: > "$CURL_LOG"
rm -rf "$WORK/out"
out=$(BUI_MIRRORS="" bash "$ROOT/scripts/ci/fetch-kernels.sh" --out "$WORK/out" --lock "$LOCK" --arch amd64 --role target 2>&1); rc=$?
assert_eq "3" "$rc" "全部源失败退 3"

out=$(bash "$ROOT/scripts/ci/fetch-kernels.sh" --out "$WORK/out" --lock "$WORK/nope.lock" 2>&1); rc=$?
assert_eq "2" "$rc" "lock 不存在退 2"

# lock 升级后再回退（CI 里 restore-keys 前缀命中新缓存 / 本机 dist 跨分支复用）：
# 旧 sha 的清单行必须被同路径的新行顶掉，否则回退后会对旧 sha 假命中、保留磁盘上的新版本二进制。
mkdir -p "$WORK/out2"
hy_lock() { # $1 = hysteria sha
  { printf '# kernel role version arch sha256 url\n'
    printf 'hysteria target 2.12.2 amd64 %s https://github.com/apernet/hysteria/releases/download/app/v2.12.2/hysteria-linux-amd64\n' "$1"
  } > "$WORK/lock-hy"
}
hy_run() { BUI_MIRRORS="https://mirror-a.example/" bash "$ROOT/scripts/ci/fetch-kernels.sh" --out "$WORK/out2" --lock "$WORK/lock-hy" --arch amd64 --role target; }

printf '#!/bin/sh\necho hysteria 2.12.1\n' > "$WORK/srv/hysteria"
HY_OLD=$(sha_of "$WORK/srv/hysteria")
hy_lock "$HY_OLD"; hy_run > /dev/null 2>&1

printf '#!/bin/sh\necho hysteria 2.12.2\n' > "$WORK/srv/hysteria"
HY_NEW=$(sha_of "$WORK/srv/hysteria")
hy_lock "$HY_NEW"; hy_run > /dev/null 2>&1

printf '#!/bin/sh\necho hysteria 2.12.1\n' > "$WORK/srv/hysteria"
hy_lock "$HY_OLD"
out=$(hy_run 2>&1); rc=$?
assert_eq "0" "$rc" "lock 回退后重新获取成功"
assert_not_contains "cached" "$out" "lock 回退时不得按旧 sha 假命中缓存"
assert_eq "$HY_OLD" "$(sha_of "$WORK/out2/bin/hysteria")" "磁盘二进制与回退后的 lock 一致"
assert_eq "1" "$(grep -c '^bin/hysteria ' "$WORK/out2/.fetched")" "清单每个路径只有一行"
finish
