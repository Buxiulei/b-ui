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

# —— 4.1：build: 行走构建分支（stub build-singbox.sh，零网络零 go）——
BLOCK="$WORK/kernels-build.lock"
cat > "$BLOCK" <<EOF
# kernel role version arch sha256 url
sing-box target 1.14.1 amd64 SHA_PLACEHOLDER build:SagerNet/sing-box@v1.14.1;go=go1.25.4;tags=with_quic,with_v2ray_api
EOF
# 假构建器：argv 先落盘（否则 build: URI 的解析回归只会以 sha 不符出现，与工具链漂移没法区分），
# 再把固定内容写到 --out，并打印锁需要的三项
mkdir -p "$WORK/stub-release"
export ARGV_LOG="$WORK/argv.log"
cat > "$WORK/stub-release/build-singbox.sh" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$ARGV_LOG"
out=""; while [[ $# -gt 0 ]]; do case "$1" in --out) out="$2"; shift 2 ;; *) shift ;; esac; done
printf '#!/bin/sh\necho "sing-box version 1.14.1"\n' > "$out"; chmod 755 "$out"
printf 'go=go1.25.4 tags=with_quic,with_v2ray_api sha256=%s\n' "$(sha256sum "$out" | cut -d' ' -f1)"
STUB
chmod 755 "$WORK/stub-release/build-singbox.sh"
: > "$ARGV_LOG"
built_sha=$(printf '#!/bin/sh\necho "sing-box version 1.14.1"\n' | sha256sum | cut -d' ' -f1)
sed -i "s/SHA_PLACEHOLDER/$built_sha/" "$BLOCK"

out=$(BUI_SINGBOX_BUILDER="$WORK/stub-release/build-singbox.sh" \
  bash "$ROOT/scripts/ci/fetch-kernels.sh" --out "$WORK/out-build" --lock "$BLOCK" --role target 2>&1)
rc=$?
assert_eq "0" "$rc" "build: 行构建成功退 0"
assert_contains "built sing-box" "$out" "打印了构建分支的日志行"
assert_eq "$built_sha" "$(sha256sum "$WORK/out-build/bin/sing-box" | cut -d' ' -f1)" "落地的是构建产物"

# build: URI 的每一项都必须原样落到构建器的 argv（只断言 sha 的话，参数拼错也全绿）
argv=$(cat "$ARGV_LOG")
assert_eq "1" "$(grep -c . "$ARGV_LOG")" "只调了一次构建器"
assert_contains "--repo SagerNet/sing-box" "$argv" "透传 build: URI 里的 repo"
assert_contains "--version 1.14.1" "$argv" "透传版本且去掉 v 前缀"
assert_contains "--arch amd64" "$argv" "透传 --arch"
assert_contains "--tags with_quic,with_v2ray_api" "$argv" "透传锁里的 tags="
assert_contains "--go go1.25.4" "$argv" "透传锁里的 go=（工具链被钉死，sha 才与宿主 Go 解耦）"

# 第二次跑：缓存命中，不再调构建器（构建器换成会失败的版本也必须退 0）
out=$(BUI_SINGBOX_BUILDER=/bin/false \
  bash "$ROOT/scripts/ci/fetch-kernels.sh" --out "$WORK/out-build" --lock "$BLOCK" --role target 2>&1)
assert_eq "0" "$?" "缓存命中不再构建"
assert_contains "cached" "$out" "命中打印 cached"

# sha 与锁不符 ⇒ 退 4（与下载 sha 不符同码）
sed -i "s/$built_sha/0000000000000000000000000000000000000000000000000000000000000000/" "$BLOCK"
rm -rf "$WORK/out-build"
BUI_SINGBOX_BUILDER="$WORK/stub-release/build-singbox.sh" \
  bash "$ROOT/scripts/ci/fetch-kernels.sh" --out "$WORK/out-build" --lock "$BLOCK" --role target >/dev/null 2>&1
assert_eq "4" "$?" "构建产物 sha 与锁不符退 4"
assert_eq "0" "$([[ -f "$WORK/out-build/bin/sing-box" ]] && echo 1 || echo 0)" "sha 不符时不留下二进制"

# 锁里少了 go= ⇒ 当场判不合规（退 3），不许静默回落到宿主工具链：
# 那样 sha 只在宿主 Go 恰好等于当初 pin 那版的机器上可重现。
NOGO="$WORK/kernels-nogo.lock"
{ printf '# kernel role version arch sha256 url\n'
  printf 'sing-box target 1.14.1 amd64 %s build:SagerNet/sing-box@v1.14.1;tags=with_quic\n' "$built_sha"
} > "$NOGO"
: > "$ARGV_LOG"
rm -rf "$WORK/out-nogo"
out=$(BUI_SINGBOX_BUILDER="$WORK/stub-release/build-singbox.sh" \
  bash "$ROOT/scripts/ci/fetch-kernels.sh" --out "$WORK/out-nogo" --lock "$NOGO" --role target 2>&1); rc=$?
assert_eq "3" "$rc" "build: URI 缺 go= 退 3"
assert_contains "build: URI 不合规" "$out" "点名 URI 不合规"
assert_eq "0" "$(grep -c . "$ARGV_LOG")" "缺 go= 时根本不调构建器"

# 落地（install）失败必须与下载分支同语义：退 3、不写清单、不谎报 built
BLOCK2="$WORK/kernels-build-ok.lock"
{ printf '# kernel role version arch sha256 url\n'
  printf 'sing-box target 1.14.1 amd64 %s build:SagerNet/sing-box@v1.14.1;go=go1.25.4;tags=with_quic,with_v2ray_api\n' "$built_sha"
} > "$BLOCK2"
rm -rf "$WORK/out-noperm"
mkdir -p "$WORK/out-noperm/bin"
chmod 500 "$WORK/out-noperm/bin"
out=$(BUI_SINGBOX_BUILDER="$WORK/stub-release/build-singbox.sh" \
  bash "$ROOT/scripts/ci/fetch-kernels.sh" --out "$WORK/out-noperm" --lock "$BLOCK2" --role target 2>&1); rc=$?
chmod 755 "$WORK/out-noperm/bin"
assert_eq "3" "$rc" "build: 行落地失败退 3"
assert_contains "落地失败" "$out" "有中文错误"
assert_not_contains "built sing-box" "$out" "落地失败不许打印 built"
assert_eq "0" "$(grep -c '^bin/sing-box ' "$WORK/out-noperm/.fetched")" "落地失败不写清单"
finish
