#!/usr/bin/env bash
# B-UI Linux 客户端（bui-c）首次安装。装完之后的升级走 `bui-c update`。
#   curl -fsSL https://<面板域名>/packages/bui-c-install.sh | sudo bash
#   sudo BUI_C_SOURCE=https://<面板域名>/packages bash bui-c-install.sh
set -euo pipefail

# 制品源按序试三条，取到 manifest 的那条也用来取二进制：
#   ① $BUI_C_SOURCE，或面板下发本脚本时写进 PANEL_SOURCE 的那个 /packages
#   ② GitHub releases/latest
#   ③ releases 列表里版本号最大的预发布 rc tag（仓库里只有预发布时 ② 必然 404）
SOURCE="${BUI_C_SOURCE:-}"

# 面板经 /packages/bui-c-install.sh 下发本脚本时，把下面这行的占位符替换成
# https://<面板域名>/packages（crates/bui/src/modules/panel/packages.rs::fill_panel_source），
# 从面板拿到的那份于是默认就从面板自己取制品，不用再设 BUI_C_SOURCE。
# 仓库里这份没被替换过，值还是占位符本身。
PANEL_SOURCE="__BUI_C_PANEL_SOURCE__"

# 这两个只为测试（scripts/tests/test-bui-c-install.sh 把它们指到 127.0.0.1）与镜像存在，
# 正常安装不用设。
GITHUB="${BUI_C_GITHUB:-https://github.com/Buxiulei/b-ui}"
RELEASES_API="${BUI_C_RELEASES_API:-https://api.github.com/repos/Buxiulei/b-ui/releases?per_page=100}"

PREFIX="${BUI_C_PREFIX:-/usr/local/bin}"
TARGET="$PREFIX/bui-c"

print_info()    { printf '  [*] %s\n' "$1"; }
print_success() { printf '  [+] %s\n' "$1"; }
print_warning() { printf '  [!] %s\n' "$1" >&2; }
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
MANIFEST="$TMP/manifest.json"

# 占位符被面板替换过（值成了 http(s) 地址）就默认用面板自己的 /packages。
# 判的是 URL 形状而不是「值 != 占位符字面量」：面板做的是全文替换，判定里再写一遍
# 占位符字面量的话那处会被一起换掉，判定就永远成立了。
if [ -z "$SOURCE" ]; then
    case "$PANEL_SOURCE" in
        http://*|https://*) SOURCE="$PANEL_SOURCE" ;;
    esac
fi

# $2=quiet：探测型尝试（① 与 ②）失败时后面有自己的中文提示，curl 那行英文报错只是噪音
# （2026-09-13 baiyi 真机：releases/latest 必然 404，每次都先蹦一行 `curl: (22) … 404`）。
fetch_manifest() {
    if [ "${2:-}" = quiet ]; then
        curl -fsL --max-time 30 "$1/manifest.json" -o "$MANIFEST"
    else
        curl -fsSL --max-time 30 "$1/manifest.json" -o "$MANIFEST"
    fi
}

GOT=""
# ① 面板下发的源，或用户显式指定的源
if [ -n "$SOURCE" ]; then
    print_info "读取 $SOURCE/manifest.json"
    if fetch_manifest "$SOURCE" quiet; then
        GOT=1
    else
        print_warning "$SOURCE/manifest.json 取不到，改试 GitHub Releases"
    fi
fi

# ② GitHub releases/latest：仓库里只有预发布时它**必然** 404，所以这条失败不报错，交给 ③
if [ -z "$GOT" ]; then
    SOURCE="$GITHUB/releases/latest/download"
    print_info "读取 $SOURCE/manifest.json"
    if fetch_manifest "$SOURCE" quiet; then
        GOT=1
    fi
fi

# ③ releases 列表里版本号最大的预发布（与服务端 install.sh 的 latest_v4_tag、bui-c 的 latest_rc_tag 同口径）
if [ -z "$GOT" ]; then
    TAG=""
    if curl -fsSL --max-time 30 "$RELEASES_API" -o "$TMP/releases.json"; then
        # prerelease=true 且形如 vX.Y.Z-rcN 的里按 (x, y, z, N) 数值取最大，任一段超出 u32
        # （4294967295）的跳过。**不看列表顺序**：这个列表不按创建时间倒序（2026-09-13 实测返回
        # rc9 → rc8 → rc7 → rc10 → rc6，最新的 rc10 排第 4，取第一个会装成 rc9）。
        # 逐字段比对键名（$2 == "tag_name"）而不是 /"tag_name"/：release 正文里出现字面
        # \"tag_name\" 时正则会误命中，字段比对不会（tr 后每片形如 {"tag_name":"v4.0.0-rc9"，
        # 以 " 切分 $2 即键名）。GitHub 的 release 对象里 tag_name 在 prerelease 之前，所以
        # prerelease 那片配的就是刚读到的 t。
        # awk 读完整个流才在 END 打印（取最大值本来就得读完）：真实响应有 200KB+（7 个 release
        # 带正文），提前 exit 会让上游的 tr 往已关闭的管道继续写而吃到 SIGPIPE（141），
        # set -o pipefail 把整条管道判为非零、set -e 于是静默杀掉整个脚本（2026-09-13 在 baiyi
        # 真机复现：rc=141，一行都不打印）。
        TAG="$(tr ',' '\n' < "$TMP/releases.json" | awk -F'"' '
            $2 == "tag_name" { t = $4 }
            $2 == "prerelease" && $3 ~ /true/ && t ~ /^v[0-9]+\.[0-9]+\.[0-9]+-rc[0-9]+$/ {
                split(substr(t, 2), v, /\.|-rc/)
                for (i = 1; i <= 4; i++) if (v[i] + 0 > 4294967295) next
                for (i = 1; i <= 4 && v[i] + 0 == b[i] + 0; i++);
                if (i > 4 || v[i] + 0 > b[i] + 0) { best = t; split(substr(t, 2), b, /\.|-rc/) }
            }
            END { if (best != "") print best }')"
    fi
    [ -n "$TAG" ] || {
        print_error "取不到 manifest.json：面板源、releases/latest 与 releases 列表都不可达。用 BUI_C_SOURCE=https://<面板域名>/packages 指定源再重试"
        exit 1
    }
    print_warning "releases/latest 里没有 manifest.json（仓库里只有预发布），回退到预发布 $TAG"
    SOURCE="$GITHUB/releases/download/$TAG"
    print_info "读取 $SOURCE/manifest.json"
    fetch_manifest "$SOURCE" || { print_error "manifest.json 下载失败（$SOURCE）"; exit 1; }
fi

# 只用 sed 取两个字段：新机器上不一定有 jq。总纲 C4 的 artifact 是 {"url":…,"sha256":…}，
# 字段顺序不保证，所以先切出这个对象，再从对象里取 sha256。
COMPACT="$(tr -d ' \n\t' < "$MANIFEST")"
VERSION="$(printf '%s' "$COMPACT" | sed -n 's/.*"version":"\([^"]*\)".*/\1/p')"
OBJ="$(printf '%s' "$COMPACT" | sed -n "s/.*\"$ART\":{\([^}]*\)}.*/\1/p")"
SHA="$(printf '%s' "$OBJ" | sed -n 's/.*"sha256":"\([0-9a-f]\{64\}\)".*/\1/p')"
[ -n "$VERSION" ] || { print_error "manifest 里没有 version"; exit 1; }
[ -n "$SHA" ] || { print_error "manifest 里没有 $ART 的 sha256"; exit 1; }

print_info "下载 $ART（v$VERSION）"
curl -fsSL --max-time 300 "$SOURCE/$ART" -o "$TMP/$ART" \
    || { print_error "$ART 下载失败"; exit 1; }

GOT_SHA="$(sha256sum "$TMP/$ART" | cut -d' ' -f1)"
if [ "$GOT_SHA" != "$SHA" ]; then
    print_error "sha256 不符（期望 $SHA，实际 $GOT_SHA），已丢弃，未写任何文件"
    exit 1
fi

install -d -m 0755 "$PREFIX"
install -m 0755 "$TMP/$ART" "$TARGET"
print_success "已安装 $TARGET（v$VERSION）"
print_info "下一步：sudo bui-c        # 数字菜单；机器上有 v3 客户端时会先问要不要导入"
