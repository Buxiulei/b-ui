#!/usr/bin/env bash
# b-ui v4 一键安装引导：架构识别 → 多源下载 manifest 与 bui → sha256 校验 → exec bui install。
#
# 新服务器一行命令（唯一必填是面板域名；其余全自动）：
#   curl -fsSL https://raw.githubusercontent.com/Buxiulei/b-ui/v4/install.sh | bash -s -- --domain panel.example.com
#
# 三个常用环境变量：
#   BUI_DOMAIN=<面板域名>   等价于 --domain（凭据/域名不想进 argv 时用它）
#   BUI_VERSION=v4.0.1      指定版本（默认 latest；只有预发布时 latest 会 404，自动回退到最新 v4* 标签）
#   BUI_MANIFEST_URL=<url>  直接指定 manifest（覆盖 BUI_VERSION；M5 演练用本机 http.server 托管的那份，
#                           环境变量会随 exec 传给 bui install，与 C5 的 BUI_MANIFEST_URL 同名同义）。
#                           没设时本脚本会把**自己实际用的**那个地址（BUI_VERSION 指定的 tag，或
#                           latest→预发布回退后的 tag）export 成它，免得 bui install 又去问 latest。
# 其余：
#   BUI_MIRRORS="https://a/ https://b/"  覆盖镜像前缀（按序回退，拼在完整 URL 前）
#   BUI_MIRRORS=""      只用直连，不试任何镜像（故用 ${VAR-默认} 而非 ${VAR:-默认}）
#   BUI_SHA256=<hex>    跳过 manifest 里的 sha256，直接钉死 bui 的 sha256
#
# 除 curl（或 wget）/ coreutils / awk 外零依赖；系统依赖、交互、配置全部由 bui install 负责。
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

# 下载器：curl 优先，缺 curl 用 wget（精简镜像里常只装 wget）；两个都没有返回空串
pick_dl() { if command -v curl > /dev/null 2>&1; then printf 'curl\n'; elif command -v wget > /dev/null 2>&1; then printf 'wget\n'; fi; }

# 两个独立判定，别混成一个（混过一次：`sudo bash install.sh` 不给域名被直接拒，
# 只有 curl|bash 那一种形态碰巧能过）：
#   has_tty     有没有终端可问 —— stdin 本身就是终端，或 /dev/tty 能打开。require_domain 用它。
#   tty_source  要不要把 /dev/tty 交接给 bui install —— 见下面那段注释，三个条件缺一不交接。
# 测试里覆盖 tty_readable 即可；`[[ -t 0 ]]` 那一半由伪终端里的真跑覆盖。
tty_readable() { { : < /dev/tty; } 2> /dev/null; }
has_tty() { [[ -t 0 ]] || tty_readable; }

answers_domain() {
    # --answers 文件（`--answers <path>` 与 `--answers=<path>` 两种写法都认）里有没有非空的
    # domain（bui install 的 AnswersFile.domain）。只认 "domain"：`"masquerade_domain"` 不误命中。
    local f=""
    while [[ $# -gt 0 ]]; do case "$1" in --answers=*) f="${1#--answers=}"; break ;; --answers) f="${2:-}"; break ;; *) shift ;; esac; done
    [[ -n "$f" && -r "$f" ]] || return 1
    grep -Eq '"domain"[[:space:]]*:[[:space:]]*"[^"]+"' "$f"
}

# 域名来源判定分两档，差别只在 --answers：
#   domain_pinned 域名已定、bui install 那一问根本不会开口 —— 答案文件里要真有 domain。tty_source 用它。
#   domain_known  给了答案文件就算有来源（内容留给 bui install 解释）—— require_domain 只为
#                 「什么都没有又没终端」时早退，宁松不宁紧，不替 bui install 解释文件。
domain_pinned() {
    local arg; [[ -n "${BUI_DOMAIN:-}" ]] && return 0
    for arg in "$@"; do case "$arg" in --domain* | --import-v3*) return 0 ;; esac; done
    answers_domain "$@"
}
domain_known() {
    local arg; domain_pinned "$@" && return 0
    for arg in "$@"; do case "$arg" in --answers*) return 0 ;; esac; done
    return 1
}

tty_source() {
    # 要不要把 /dev/tty 交接给 bui install。三个条件缺一不交接（不输出 = 原样继承 stdin）：
    #   1. stdin 是管道（curl … | bash）且 /dev/tty 能打开 —— stdin 已是终端就不用换
    #   2. 参数里没有 --admin-password-stdin —— 那是把管理员密码喂进 bui install 的唯一通道
    #      （`bash install.sh --non-interactive --answers a.json --admin-password-stdin < pw.txt`），
    #      把 stdin 换成终端会让它读到空密码、密码文件被静默忽略（2026-09-12 第三轮审查 blocking）
    #   3. 域名未知 —— 域名已定时它一个问题都不问，没有任何理由动人家的 stdin
    local arg; { [[ -t 0 ]] || ! tty_readable; } && return 0
    for arg in "$@"; do case "$arg" in --admin-password-stdin) return 0 ;; esac; done
    domain_pinned "$@" || printf '/dev/tty\n'
}

require_domain() {
    # 域名没来源、又没有终端可问（stdin 不是终端且 /dev/tty 开不了）⇒ 打印用法、退 2；
    # 此时一个字节都还没下载（不装一半）。用 has_tty 而不是 tty_source：后者在 stdin 已是终端时
    # 输出空串（含义是「不需要交接」），拿它当判据会把 `sudo bash install.sh` 也一并拒掉。
    if ! domain_known "$@" && ! has_tty; then
        print_error "缺少面板域名（唯一必填项）。用法：curl -fsSL <install.sh> | bash -s -- --domain panel.example.com（或 BUI_DOMAIN=panel.example.com）"; return 2
    fi
}

gh_url() {
    if [[ "$TAG" == "latest" ]]; then
        printf 'https://github.com/%s/releases/latest/download/%s\n' "$REPO" "$1"
    else
        printf 'https://github.com/%s/releases/download/%s/%s\n' "$REPO" "$TAG" "$1"
    fi
}

fetch() {
    # $1 = 完整 URL, $2 = 落地路径, $3 = quiet（可选：失败不报错，由调用方自己解释）；
    # 源顺序：直连 → 镜像前缀（沿用 v3 install.sh 的主备思路）
    local prefix
    # shellcheck disable=SC2086
    for prefix in "" $MIRRORS; do
        print_info "下载 ${1##*/}${prefix:+（镜像 $prefix）}"
        if [[ "$(pick_dl)" == "curl" ]] && curl -fsSL --connect-timeout 10 --max-time 600 --retry 2 -o "$2" "${prefix}$1"; then return 0; fi
        if [[ "$(pick_dl)" == "wget" ]] && wget -q -T 10 --tries 3 -O "$2" "${prefix}$1"; then return 0; fi
    done
    [[ "${3:-}" == "quiet" ]] || print_error "${1##*/} 下载失败：直连与全部镜像均不可达"
    return 1
}

latest_v4_tag() {
    # $1 = 临时目录。GitHub 的 releases/latest 只认正式版：仓库里只有 v4.0.0-rc1 这类**预发布**时
    # 它 404（2026-09-12 裁决「预发布与首推」），于是从 releases 列表（新→旧，含预发布）取第一个 v4*。
    local list="$1/releases.json"; fetch "https://api.github.com/repos/$REPO/releases?per_page=100" "$list" >&2 || return 1
    # 逐字段比对键名（$2=="tag_name"）而不是 /"tag_name"/：release body 里出现字面 \"tag_name\" 时
    # 正则会误命中，字段比对不会（tr 后每片形如 {"tag_name":"v4.0.0-rc2"，以 " 切分 $2 即键名）。
    tr ',' '\n' < "$list" | awk -F'"' '$2 == "tag_name" && $4 ~ /^v4/ { print $4; exit }'
}

get_manifest() {
    # $1 = manifest 落地路径。BUI_MANIFEST_URL 直接用；否则先试 releases/latest，
    # 它 404（仓库里只有预发布）时回退到 releases 列表里最新的 v4* 标签。
    #
    # 第一次尝试走 quiet：预发布期 releases/latest **必然** 404，那条「直连与全部镜像均不可达」
    # 对 404 是误导（源好得很，只是还没有正式版）。真的取不到时由下面两条自己说清楚。
    #
    # 取到之后把**实际用的**那个地址 export 成 BUI_MANIFEST_URL 交给 bui install（见 main 末尾的
    # exec）：2026-09-12 真机 bwg-tizi 上 install.sh 从 releases/download/v4.0.0-rc2/ 正确下到了
    # manifest 与 bui，可 bui install 又按内置默认去问 releases/latest、404、一个内核都没装。
    # 用户已显式设了 BUI_MANIFEST_URL 时 murl 就是它本身，export 回去是原样不动。
    local murl="${MANIFEST_URL:-$(gh_url manifest.json)}"
    if fetch "$murl" "$1" quiet; then export BUI_MANIFEST_URL="$murl"; return 0; fi
    [[ -z "$MANIFEST_URL" && "$TAG" == "latest" ]] || { print_error "manifest.json 下载失败（$murl）：直连与全部镜像均不可达"; return 1; }
    TAG=$(latest_v4_tag "$(dirname "$1")") || TAG=""
    [[ -n "$TAG" ]] || { print_error "取不到可用版本：releases/latest 与 releases 列表都不可达。请设 BUI_VERSION=vX.Y.Z-rcN（预发布也可）或 BUI_MANIFEST_URL=<manifest 地址>"; return 1; }
    print_warning "releases/latest 里没有 manifest.json（仓库里只有预发布），回退到预发布 $TAG"
    murl=$(gh_url manifest.json)
    fetch "$murl" "$1" || return 1
    export BUI_MANIFEST_URL="$murl"
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
    local arch tmp url want args=()
    if [[ "${EUID:-$(id -u)}" -ne 0 ]]; then
        print_error "需要 root 权限：先 sudo -i 再重试"
        exit 1
    fi
    command -v systemctl > /dev/null 2>&1 || { print_error "此系统没有 systemd，无法安装"; exit 1; }
    [[ -n "$(pick_dl)" ]] || { print_error "缺少 curl 与 wget，请先装一个（apt install curl / yum install curl）"; exit 1; }
    mapfile -t args < <(install_args "$BASE_DIR" "$@")
    require_domain "${args[@]+"${args[@]}"}" || exit $?
    arch=$(detect_arch)
    tmp=$(mktemp -d)
    # shellcheck disable=SC2064
    trap "rm -rf '$tmp'" EXIT

    get_manifest "$tmp/manifest.json"
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

    if [[ " ${args[*]-} " == *" --import-v3 "* ]]; then
        print_warning "检测到 v3 安装（$BASE_DIR/users.json），将以 --import-v3 迁移现有用户与配置"
    fi
    print_info "交给 bui install ${args[*]-}"
    # 管道里跑、域名又没来源时把 /dev/tty 接给 bui install，它才能问出「面板域名」那一问；
    # 域名已定或 stdin 正给 --admin-password-stdin 送密码时原样继承 stdin（见 tty_source）
    if [[ -n "$(tty_source "${args[@]+"${args[@]}"}")" ]]; then exec "$BIN_PATH" install "${args[@]+"${args[@]}"}" < /dev/tty; fi
    exec "$BIN_PATH" install "${args[@]+"${args[@]}"}"
}

if [[ "${BUI_BOOTSTRAP_SOURCED:-0}" != "1" ]]; then
    main "$@"
fi
