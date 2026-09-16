#!/usr/bin/env bash
# 按 kernels.lock 把内核二进制取到 <out>：GitHub 直连 → 镜像前缀回退，sha256 校验，已就位则跳过。
# CI（setup-kernels action）与本机都用这一份；配合 actions/cache 时缓存命中就是零下载。
set -uo pipefail
LC_ALL=C
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

OUT=""
LOCK="$HERE/../release/kernels.lock"
ARCH="amd64"
ROLE="all"
# ${VAR-默认} 而非 ${VAR:-默认}：BUI_MIRRORS="" 表示「只用直连」，测试与离网环境都要这个语义
MIRRORS="${BUI_MIRRORS-https://ghfast.top/ https://gh-proxy.com/}"

usage() {
    printf '用法：%s --out <dir> [--lock <kernels.lock>] [--arch amd64|arm64] [--role all|target|check]\n' "$0" >&2
    exit 2
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --out) OUT="${2:-}"; shift 2 ;;
        --lock) LOCK="${2:-}"; shift 2 ;;
        --arch) ARCH="${2:-}"; shift 2 ;;
        --role) ROLE="${2:-}"; shift 2 ;;
        *) usage ;;
    esac
done
[[ -n "$OUT" ]] || usage
[[ -f "$LOCK" ]] || { printf '找不到 lock：%s\n' "$LOCK" >&2; exit 2; }

MANIFEST="$OUT/.fetched"
mkdir -p "$OUT/bin"
touch "$MANIFEST"

download() {
    # $1 = 完整 GitHub URL, $2 = 落地路径
    local prefix
    # shellcheck disable=SC2086
    for prefix in "" $MIRRORS; do
        if curl -fsSL --connect-timeout 10 --max-time 600 --retry 2 -o "$2" "${prefix}$1"; then
            return 0
        fi
    done
    printf '下载失败（GitHub 与镜像均不可达）：%s\n' "$1" >&2
    return 1
}

build_kernel() {
    # $1 = build:<repo>@v<ver>;go=<go>;tags=<tags>, $2 = 落地路径（裸二进制）
    local spec repo rest ver go tags builder args
    spec="${1#build:}"
    repo="${spec%%@*}"
    rest="${spec#*@}"
    ver="${rest%%;*}"; ver="${ver#v}"
    case "$rest" in *';go='*) go="${rest#*;go=}"; go="${go%%;*}" ;; *) go="" ;; esac
    case "$rest" in *';tags='*) tags="${rest##*;tags=}" ;; *) tags="" ;; esac
    # go= 是必填：少了它构建器会回落 GOTOOLCHAIN=auto，sha 就只在宿主 Go 恰好等于
    # 当初 pin 那版的机器上可重现（宿主一升级就全网 sha 不符，报错还像被投毒）。
    if [[ -z "$repo" || -z "$ver" || -z "$go" ]]; then
        printf 'build: URI 不合规（要 build:<repo>@v<ver>;go=<go1.x.y>;tags=<...>）：%s\n' "$1" >&2
        return 1
    fi
    builder="${BUI_SINGBOX_BUILDER:-$HERE/../release/build-singbox.sh}"
    args=(--repo "$repo" --version "$ver" --arch "$ARCH" --out "$2" --go "$go")
    [[ -n "$tags" ]] && args+=(--tags "$tags")
    # 构建器的 stdout（go= tags= sha256=）在这里只是日志，锁已经带了这三项
    if ! "$builder" "${args[@]}" >&2; then
        printf '构建失败：%s\n' "$1" >&2
        return 1
    fi
    return 0
}

extract() {
    # $1 = 归档路径, $2 = kernel, $3 = version, $4 = 目标二进制路径
    local tmp rc
    tmp=$(mktemp -d)
    case "$2" in
        hysteria)
            install -m 755 "$1" "$4"
            ;;
        xray)
            unzip -q -o "$1" xray -d "$tmp" && install -m 755 "$tmp/xray" "$4"
            ;;
        caddy)
            tar -xzf "$1" -C "$tmp" caddy && install -m 755 "$tmp/caddy" "$4"
            ;;
        sing-box)
            tar -xzf "$1" -C "$tmp" "sing-box-$3-linux-$ARCH/sing-box" \
                && install -m 755 "$tmp/sing-box-$3-linux-$ARCH/sing-box" "$4"
            ;;
        *)
            printf '未知内核 %s\n' "$2" >&2
            rm -rf "$tmp"
            return 1
            ;;
    esac
    rc=$?
    rm -rf "$tmp"
    return "$rc"
}

rc_final=0
while read -r kernel role version arch sha url; do
    case "$kernel" in '#'* | '') continue ;; esac
    [[ "$arch" == "$ARCH" ]] || continue
    [[ "$ROLE" == "all" || "$ROLE" == "$role" ]] || continue

    if [[ "$role" == "check" ]]; then
        rel="singbox/${version%.*}/$kernel"
    else
        rel="bin/$kernel"
    fi
    dst="$OUT/$rel"
    mkdir -p "$(dirname "$dst")"

    if [[ -f "$dst" ]] && grep -qxF "$rel $sha" "$MANIFEST"; then
        printf 'cached %s (%s %s)\n' "$rel" "$kernel" "$version"
        continue
    fi

    tmp=$(mktemp)
    if [[ "$url" == build:* ]]; then
        # 自建来源（今天只有 sing-box target 两行，spec §5.3）：build:<repo>@v<ver>;go=<go>;tags=<tags>
        # 锁里的 go= 是**被强制的构建输入**（透传 --go，构建器钉死 GOTOOLCHAIN 并断言），不是记账：
        # 不钉的话 sha 只在宿主 Go 恰好等于当初 pin 那版的机器上可重现。
        if ! build_kernel "$url" "$tmp"; then
            rm -f "$tmp"
            rc_final=3
            continue
        fi
        got=$(sha256sum "$tmp" | cut -d' ' -f1)
        if [[ "$got" != "$sha" ]]; then
            printf 'sha256 不匹配 %s：期望 %s 实际 %s\n' "$url" "$sha" "$got" >&2
            rm -f "$tmp" "$dst"
            rc_final=4
            continue
        fi
        # build: 行的产物已经是裸二进制，不走 extract；落地失败与下载分支同码（3）
        if ! install -m 755 "$tmp" "$dst"; then
            printf '落地失败：%s\n' "$dst" >&2
            rm -f "$tmp"
            rc_final=3
            continue
        fi
        line="built $kernel $version ($rel)"
    else
        if ! download "$url" "$tmp"; then
            rm -f "$tmp"
            rc_final=3
            continue
        fi
        got=$(sha256sum "$tmp" | cut -d' ' -f1)
        if [[ "$got" != "$sha" ]]; then
            printf 'sha256 不匹配 %s：期望 %s 实际 %s\n' "$url" "$sha" "$got" >&2
            rm -f "$tmp" "$dst"
            rc_final=4
            continue
        fi
        if ! extract "$tmp" "$kernel" "$version" "$dst"; then
            printf '解包失败：%s\n' "$url" >&2
            rm -f "$tmp"
            rc_final=3
            continue
        fi
        line="fetched $rel ($kernel $version)"
    fi
    rm -f "$tmp"
    # 按路径去重（不带 sha）：lock 升级后旧的 `<rel> <旧sha>` 行必须被顶掉，
    # 否则 lock 回退（restore-keys 前缀命中新缓存 / dist 跨分支复用）时会对旧 sha 假命中，
    # 打印 cached 却保留磁盘上的新版本二进制。
    awk -v r="$rel" '$1 != r' "$MANIFEST" > "$MANIFEST.new" 2>/dev/null || true
    mv -f "$MANIFEST.new" "$MANIFEST"
    printf '%s %s\n' "$rel" "$sha" >> "$MANIFEST"
    printf '%s\n' "$line"
done < "$LOCK"

exit "$rc_final"
