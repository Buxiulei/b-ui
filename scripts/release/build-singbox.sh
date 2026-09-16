#!/usr/bin/env bash
# 自建随发布分发的那一个 sing-box（唯一动机：官方归档不带 with_v2ray_api，住宅计量要它）。
# CI 里跑 `sing-box check` 的 1.12 / 1.13 不自建，继续用上游归档（spec §5.3）。
#   build-singbox.sh --version <x.y.z> --arch amd64|arm64 --out <path> [--repo <owner/repo>] [--tags <逗号分隔>] [--go <go1.x.y>]
# `--go` 是**可重现性的开关**：给了就把 GOTOOLCHAIN 钉死在这一版并断言探到的就是它，
# 于是 sha256 与宿主 Go 解耦（取件时 fetch-kernels.sh 从锁的 `go=` 透传）；
# 不给才回落 GOTOOLCHAIN=auto —— 那只用于 pin 时**发现**上游 go.mod 要求的版本。
# 成功：stdout 最后一行是锁需要的三项 `go=<go1.x.y> tags=<...> sha256=<...>`，退 0。
# 失败：任一步（clone / 工具链 / go build / 落地）失败即退非 0 且**不产出 --out**——
#       调用方（pin-kernels.sh --write、fetch-kernels.sh）据此保持锁不动、发布保持上一版（spec §5.4 第 8 条）。
# 交叉编译靠 Go 原生（CGO_ENABLED=0），arm64 不需要容器。
set -euo pipefail
LC_ALL=C
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=/dev/null
. "$HERE/kernel-versions.env"

print_info()  { printf '  [*] %s\n' "$1" >&2; }
print_error() { printf '  [!] %s\n' "$1" >&2; }

VERSION=""
ARCH=""
OUT=""
REPO="$SINGBOX_REPO"
TAGS="${SINGBOX_TAGS:-}"
GO_PIN=""
SRC=""
BUILD_TMP=""

usage() {
    printf '用法：%s --version <x.y.z> --arch amd64|arm64 --out <path> [--repo <owner/repo>] [--tags <a,b,c>] [--go <go1.x.y>]\n' "$0" >&2
    exit 2
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --version) VERSION="${2:-}"; shift 2 ;;
        --arch) ARCH="${2:-}"; shift 2 ;;
        --out) OUT="${2:-}"; shift 2 ;;
        --repo) REPO="${2:-}"; shift 2 ;;
        --tags) TAGS="${2:-}"; shift 2 ;;
        --go) GO_PIN="${2:-}"; shift 2 ;;
        *) usage ;;
    esac
done
[[ -n "$VERSION" && -n "$ARCH" && -n "$OUT" ]] || usage
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { print_error "版本号形如 x.y.z：$VERSION"; exit 2; }
[[ "$ARCH" == "amd64" || "$ARCH" == "arm64" ]] || { print_error "架构只支持 amd64 / arm64：$ARCH"; exit 2; }
# 标签集不许猜：为空就报错（首次构建时由实测的官方同版本 Tags 填进 kernel-versions.env）
[[ -n "$TAGS" ]] || { print_error 'SINGBOX_TAGS 为空：构建标签集必须显式给出（--tags 或 kernel-versions.env）'; exit 2; }
[[ -z "$GO_PIN" || "$GO_PIN" =~ ^go[0-9]+\.[0-9]+(\.[0-9]+)?$ ]] || { print_error "--go 形如 go1.25.5：$GO_PIN"; exit 2; }

cleanup() {
    [[ -n "$SRC" ]] && rm -rf "$SRC"
    [[ -n "$BUILD_TMP" ]] && rm -f "$BUILD_TMP"
    return 0
}
trap cleanup EXIT

command -v go >/dev/null 2>&1 || { print_error '找不到 go（CI 用 actions/setup-go）'; exit 2; }
command -v git >/dev/null 2>&1 || { print_error '找不到 git'; exit 2; }

SRC=$(mktemp -d)
print_info "clone $REPO v$VERSION"
git clone --quiet --depth 1 -b "v$VERSION" "https://github.com/$REPO" "$SRC"

# 工具链：--go 给了就钉死在这一版（sha 与宿主 Go 解耦），没给才 auto（让 go 按上游 go.mod 自取，
# 供 pin-kernels.sh --write 发现版本并写进锁的 go=）。探到的版本与 --go 不符即退非 0。
GO_TOOLCHAIN="${GO_PIN:-auto}"
GO_VER=$(cd "$SRC" && GOTOOLCHAIN="$GO_TOOLCHAIN" go version | awk '{print $3}')
[[ -n "$GO_VER" ]] || { print_error '取不到 go 版本'; exit 1; }
if [[ -n "$GO_PIN" && "$GO_VER" != "$GO_PIN" ]]; then
    print_error "工具链不符：要求 $GO_PIN 实际 $GO_VER"
    exit 1
fi
# 上游 release/LDFLAGS 也是构建参数的一部分（v1.14.1 = `-X runtime.godebugDefault=multipathtcp=0,tlssha1=1
# -checklinkname=0`，见上游 Makefile 的 LDFLAGS_SHARED）。读文件而不是抄一遍：抄漏了 godebug
# 默认值就与官方归档不同，而 Tags 断言查不出这一项。
LDFLAGS_SHARED=$(tr -d '\n' < "$SRC/release/LDFLAGS" 2>/dev/null) || true
[[ -n "$LDFLAGS_SHARED" ]] || { print_error "上游 release/LDFLAGS 缺失或为空：$SRC/release/LDFLAGS"; exit 1; }
print_info "go $GO_VER tags $TAGS arch $ARCH"

mkdir -p "$(dirname "$OUT")"
# 先落临时文件再 install：失败时 --out 不留半成品（本脚本的失败语义）
BUILD_TMP=$(mktemp)
# 构建命令（ldflags 的拼法照上游 Makefile 的 PARAMS：constant.Version + release/LDFLAGS + -s -w -buildid=；
# -checklinkname=0 由 release/LDFLAGS 带来，见 https://sing-box.sagernet.org/installation/build-from-source/ ）
( cd "$SRC" && CGO_ENABLED=0 GOOS=linux GOARCH="$ARCH" GOTOOLCHAIN="$GO_TOOLCHAIN" \
  go build -trimpath -tags "$TAGS" \
    -ldflags "-X github.com/sagernet/sing-box/constant.Version=$VERSION $LDFLAGS_SHARED -s -w -buildid=" \
    -o "$BUILD_TMP" ./cmd/sing-box )
install -m 755 "$BUILD_TMP" "$OUT"
printf 'go=%s tags=%s sha256=%s\n' "$GO_VER" "$TAGS" "$(sha256sum "$OUT" | cut -d' ' -f1)"
