#!/usr/bin/env bash
# b-ui v4 一键安装引导：架构识别 → 多源下载 manifest 与 bui → sha256 校验 → exec bui install。
# 除 curl / coreutils / awk 外零依赖；系统依赖、交互、配置全部由 bui install 负责。
#   BUI_VERSION=v4.0.1  指定版本（默认 latest）
#   BUI_MANIFEST_URL=<url>  直接指定 manifest（覆盖 BUI_VERSION；M5 演练用本机 http.server 托管的那份，
#                           环境变量会随 exec 传给 bui install，与 C5 的 BUI_MANIFEST_URL 同名同义）
#   BUI_MIRRORS="https://a/ https://b/"  覆盖镜像前缀（按序回退，拼在完整 URL 前）
#   BUI_MIRRORS=""      只用直连，不试任何镜像（故用 ${VAR-默认} 而非 ${VAR:-默认}）
#   BUI_SHA256=<hex>    跳过 manifest 里的 sha256，直接钉死 bui 的 sha256
set -euo pipefail
LC_ALL=C

REPO="${BUI_REPO:-Buxiulei/b-ui}"
TAG="${BUI_VERSION:-latest}"
MANIFEST_URL="${BUI_MANIFEST_URL:-}"
MIRRORS="${BUI_MIRRORS-https://ghfast.top/ https://gh-proxy.com/}"
BASE_DIR="${BUI_BASE_DIR:-/opt/b-ui}"
BIN_PATH="$BASE_DIR/bin/bui"

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; BLUE='\033[0;34m'; NC='\033[0m'
print_info()    { echo -e "${BLUE}[INFO]${NC} $1"; }
print_success() { echo -e "${GREEN}[SUCCESS]${NC} $1"; }
print_warning() { echo -e "${YELLOW}[WARNING]${NC} $1"; }
print_error()   { echo -e "${RED}[ERROR]${NC} $1" >&2; }

detect_arch() {
    # 输出 manifest 的 artifacts 键里用的架构名（总纲 C4：amd64 / arm64）
    case "$(uname -m)" in
        x86_64 | amd64) printf 'amd64\n' ;;
        aarch64 | arm64) printf 'arm64\n' ;;
        *) print_error "不支持的架构 $(uname -m)：v4 只发布 amd64 与 arm64 静态二进制"; return 1 ;;
    esac
}

gh_url() {
    if [[ "$TAG" == "latest" ]]; then
        printf 'https://github.com/%s/releases/latest/download/%s\n' "$REPO" "$1"
    else
        printf 'https://github.com/%s/releases/download/%s/%s\n' "$REPO" "$TAG" "$1"
    fi
}

fetch() {
    # $1 = 完整 URL, $2 = 落地路径；源顺序：直连 → 镜像前缀（沿用 v3 install.sh 的主备思路）
    local prefix
    # shellcheck disable=SC2086
    for prefix in "" $MIRRORS; do
        print_info "下载 ${1##*/}${prefix:+（镜像 $prefix）}"
        if curl -fsSL --connect-timeout 10 --max-time 600 --retry 2 -o "$2" "${prefix}$1"; then
            return 0
        fi
    done
    print_error "${1##*/} 下载失败：直连与全部镜像均不可达"
    return 1
}

manifest_field() {
    # $1 = manifest 路径, $2 = artifacts 键（如 bui-linux-amd64）, $3 = url|sha256
    # 不依赖 jq：认 gen-manifest.sh 的 jq 默认排版（每字段一行）。键名带引号比对，
    # 所以 "bui-linux-amd64" 不会误命中 "bui-c-linux-amd64"。
    awk -v key="\"$2\":" -v field="\"$3\":" '
        index($0, key) { inblk = 1; next }
        inblk && index($0, field) {
            v = $0
            sub(/^[^:]*:[[:space:]]*"/, "", v)
            sub(/".*/, "", v)
            print v
            exit
        }
        inblk && index($0, "}") { exit }
    ' "$1"
}

verify_sha256() {
    local got
    got=$(sha256sum "$1" | cut -d' ' -f1)
    if [[ "$got" != "$2" ]]; then
        print_error "sha256 校验失败：期望 $2，实际 $got"
        return 1
    fi
}

install_args() {
    # $1 = BASE_DIR，其余为用户参数；stdout 每行一个参数。
    # 只补 C5 里存在的 --import-v3，不自造「拒绝导入」开关；P1 的 import-v3 把 v3 的
    # users.json 归档进 <base>/v3-backup/，所以装成 v4 之后这里不会再命中（见 Interfaces 的前提）。
    local base="$1" arg auto=1
    shift
    for arg in "$@"; do
        case "$arg" in --import-v3) auto=0 ;; esac
        printf '%s\n' "$arg"
    done
    if [[ "$auto" -eq 1 && -f "$base/users.json" ]]; then
        printf '%s\n' "--import-v3"
    fi
}

main() {
    local arch tmp murl url want args=()
    if [[ "${EUID:-$(id -u)}" -ne 0 ]]; then
        print_error "需要 root 权限：先 sudo -i 再重试"
        exit 1
    fi
    command -v systemctl > /dev/null 2>&1 || { print_error "此系统没有 systemd，无法安装"; exit 1; }
    command -v curl > /dev/null 2>&1 || { print_error "缺少 curl，请先安装（apt install curl / yum install curl）"; exit 1; }
    arch=$(detect_arch)
    tmp=$(mktemp -d)
    # shellcheck disable=SC2064
    trap "rm -rf '$tmp'" EXIT

    murl="${MANIFEST_URL:-$(gh_url manifest.json)}"
    fetch "$murl" "$tmp/manifest.json"
    url=$(manifest_field "$tmp/manifest.json" "bui-linux-$arch" url)
    [[ -n "$url" ]] || { print_error "manifest 里没有 bui-linux-$arch 的 url"; exit 1; }
    want="${BUI_SHA256:-$(manifest_field "$tmp/manifest.json" "bui-linux-$arch" sha256)}"
    [[ "$want" =~ ^[0-9a-f]{64}$ ]] || { print_error "manifest 里的 sha256 不合法：$want"; exit 1; }

    fetch "$url" "$tmp/bui"
    verify_sha256 "$tmp/bui" "$want"
    install -D -m 755 "$tmp/bui" "$BIN_PATH"
    install -D -m 644 "$tmp/manifest.json" "$BASE_DIR/manifest.json"
    ln -sf "$BIN_PATH" /usr/local/bin/bui
    ln -sf "$BIN_PATH" /usr/local/bin/b-ui
    print_success "bui 已安装到 $BIN_PATH（来源 $url）"

    mapfile -t args < <(install_args "$BASE_DIR" "$@")
    if [[ " ${args[*]-} " == *" --import-v3 "* ]]; then
        print_warning "检测到 v3 安装（$BASE_DIR/users.json），将以 --import-v3 迁移现有用户与配置"
    fi
    print_info "交给 bui install ${args[*]-}"
    exec "$BIN_PATH" install "${args[@]+"${args[@]}"}"
}

if [[ "${BUI_BOOTSTRAP_SOURCED:-0}" != "1" ]]; then
    main "$@"
fi
