#!/usr/bin/env bash
# B-UI Linux 客户端（bui-c）首次安装。装完之后的升级走 `bui-c update`。
#   curl -fsSL https://<面板域名>/packages/bui-c-install.sh | sudo bash
#   sudo BUI_C_SOURCE=https://<面板域名>/packages bash bui-c-install.sh
set -euo pipefail

SOURCE="${BUI_C_SOURCE:-https://github.com/Buxiulei/b-ui/releases/latest/download}"
PREFIX="${BUI_C_PREFIX:-/usr/local/bin}"
TARGET="$PREFIX/bui-c"

print_info()    { printf '  [*] %s\n' "$1"; }
print_success() { printf '  [+] %s\n' "$1"; }
print_error()   { printf '  [!] %s\n' "$1" >&2; }

need() { command -v "$1" >/dev/null 2>&1 || { print_error "缺少 $1，先装它再重试"; exit 1; }; }
need curl
need sha256sum
need install

case "$(uname -m)" in
    x86_64|amd64)  ARCH=amd64 ;;
    aarch64|arm64) ARCH=arm64 ;;
    *) print_error "不支持的架构 $(uname -m)（只发布 amd64 / arm64）"; exit 1 ;;
esac
ART="bui-c-linux-$ARCH"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

print_info "读取 $SOURCE/manifest.json"
curl -fsSL --max-time 30 "$SOURCE/manifest.json" -o "$TMP/manifest.json" \
    || { print_error "manifest.json 下载失败（检查面板地址或换 GitHub 源）"; exit 1; }

# 只用 sed 取两个字段：新机器上不一定有 jq。总纲 C4 的 artifact 是 {"url":…,"sha256":…}，
# 字段顺序不保证，所以先切出这个对象，再从对象里取 sha256。
COMPACT="$(tr -d ' \n\t' < "$TMP/manifest.json")"
VERSION="$(printf '%s' "$COMPACT" | sed -n 's/.*"version":"\([^"]*\)".*/\1/p')"
OBJ="$(printf '%s' "$COMPACT" | sed -n "s/.*\"$ART\":{\([^}]*\)}.*/\1/p")"
SHA="$(printf '%s' "$OBJ" | sed -n 's/.*"sha256":"\([0-9a-f]\{64\}\)".*/\1/p')"
[ -n "$VERSION" ] || { print_error "manifest 里没有 version"; exit 1; }
[ -n "$SHA" ] || { print_error "manifest 里没有 $ART 的 sha256"; exit 1; }

print_info "下载 $ART（v$VERSION）"
curl -fsSL --max-time 300 "$SOURCE/$ART" -o "$TMP/$ART" \
    || { print_error "$ART 下载失败"; exit 1; }

GOT="$(sha256sum "$TMP/$ART" | cut -d' ' -f1)"
if [ "$GOT" != "$SHA" ]; then
    print_error "sha256 不符（期望 $SHA，实际 $GOT），已丢弃，未写任何文件"
    exit 1
fi

install -d -m 0755 "$PREFIX"
install -m 0755 "$TMP/$ART" "$TARGET"
print_success "已安装 $TARGET（v$VERSION）"
print_info "下一步：sudo bui-c        # 数字菜单；机器上有 v3 客户端时会先问要不要导入"
