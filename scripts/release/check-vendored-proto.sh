#!/usr/bin/env bash
# vendored proto 与**当次内核锁**的绑定门：`crates/bui/proto-*/` 里的 .proto 必须逐字节等于
# `scripts/release/kernels.lock` 里那个 sing-box tag 的上游文件。
# 用法：check-vendored-proto.sh [--warn] [--repo <dir>]
# 退出码：0 通过 / 2 核实不了（锁或环境坏了）/ 3 漂移（proto 与内核不配套）。
#
# 为什么需要它（2026-10-10）：内核从自建换成官方发行物、并跟浮动轨道（minor:1.14）之后，
# 上游每个 patch 都可能悄悄改 `daemon/started_service.proto` 的字段/编号。我们编译的是
# **本地 vendored 的副本**，字段编号一旦与运行的内核对不上，protobuf 不会报错，只会把
# 数字读错（静默解析错）——住宅计量会算出错的字节数且没人看得见。这条门把「锁里的内核」
# 与「vendored 的 proto」钉在一起。
#
# 与 check-kernel-consistency.sh 的分工：那条查「内核来源 vs 渲染器是否产出 v2ray_api」，
# 全程只读本地文件；本条要联网取上游原文，两件事、两种环境依赖，所以是两条脚本。
#
# 落点（同 check-kernel-consistency.sh）：`ci.yml` 的 lint job 用 `--warn`，`release.yml`
# 的 verify job 是硬闸门。
#
# 只覆盖**逐字节 vendor** 的树。`proto/`（Xray）不在内：它的上游不是 sing-box、
# 也不随这个 tag 走。
set -uo pipefail
LC_ALL=C
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
WARN=0
REPO="$ROOT"
RAW_BASE=${RAW_BASE:-https://raw.githubusercontent.com}

usage() {
    printf '用法：%s [--warn] [--repo <dir>]\n' "$0" >&2
    exit 2
}

while [[ "$#" -gt 0 ]]; do
    case "$1" in
        --warn) WARN=1; shift ;;
        --repo) [[ "$#" -ge 2 ]] || usage; REPO="$2"; shift 2 ;;
        -*) printf '未知选项：%s\n' "$1" >&2; usage ;;
        *) usage ;;
    esac
done

# 漂移：--warn 下改成 GitHub 注解走 stdout 并退 0，文案一字不改，方便 grep。
deny() {
    if [[ "$WARN" -eq 1 ]]; then
        printf '::warning::%s\n' "$1"
        exit 0
    fi
    printf '%s\n' "$1" >&2
    exit 3
}

# 核实不了：与 --warn 无关，永远退 2。锁或环境坏了不是「上游动了」，吞成告警就是 fail-open
# （同 pin-kernels.sh --warn 的口径）。
unverifiable() {
    printf '%s\n' "$1" >&2
    exit 2
}

TREES=(crates/bui/proto-singbox)

LOCK="$REPO/scripts/release/kernels.lock"
[[ -f "$LOCK" ]] || unverifiable "内核锁读不到：$LOCK"

# ① 锁里 sing-box 的版本与仓库。两行（amd64/arm64）必须同一个版本，URL 的 tag 必须与版本列一致。
versions=$(awk '$1 == "sing-box" && $2 == "target" { print $3 }' "$LOCK" | sort -u)
[[ -n "$versions" ]] || unverifiable "内核锁里没有 sing-box target 行：$LOCK"
[[ "$(printf '%s\n' "$versions" | wc -l)" -eq 1 ]] || unverifiable "sing-box 两行的版本列不一致：$versions"
ver="$versions"
url=$(awk '$1 == "sing-box" && $2 == "target" { print $6; exit }' "$LOCK")
case "$url" in
    https://github.com/*) ;;
    *) unverifiable "锁里 sing-box 的 URL 列不是 GitHub 资产：$url" ;;
esac
rest="${url#https://github.com/}"
repo="${rest%%/releases/*}"
[[ "$repo" == */* ]] || unverifiable "解析不出仓库名：$url"
tail_path="${rest#*/releases/download/}"
[[ "$tail_path" != "$rest" ]] || unverifiable "URL 里没有 /releases/download/：$url"
tag="${tail_path%%/*}"
[[ "$tag" == "v$ver" ]] || unverifiable "锁里 sing-box 的 URL tag（$tag）与版本列（$ver）不一致：$url"

for tree in "${TREES[@]}"; do
    dir="$REPO/$tree"
    [[ -d "$dir" ]] || unverifiable "vendored proto 目录不存在：$dir"
    sums="$dir/SHA256SUMS"
    upfile="$dir/UPSTREAM"
    [[ -f "$sums" ]] || unverifiable "缺 $tree/SHA256SUMS"
    [[ -f "$upfile" ]] || unverifiable "缺 $tree/UPSTREAM"

    # ② 离线那一半：记录的上游身份必须与锁一致。内核换了 patch、proto 没重核，这里就断。
    rec_repo=$(awk -F= '$1 == "repo" { print $2 }' "$upfile")
    rec_tag=$(awk -F= '$1 == "tag" { print $2 }' "$upfile")
    [[ "$rec_repo" == "$repo" ]] || deny "$tree/UPSTREAM 的 repo（$rec_repo）与内核锁（$repo）不一致"
    [[ "$rec_tag" == "$tag" ]] || deny "$tree/UPSTREAM 记的 tag（$rec_tag）与内核锁（$tag）不一致："\
"内核升了版，vendored proto 没跟它核过。按 $tag 重新核对："\
"curl -fsSL $RAW_BASE/$repo/$tag/<path> | sha256sum，与 $tree/SHA256SUMS 比；"\
"逐字节一致就只改 UPSTREAM 的 tag= 行，不一致则更新 proto 与 SHA256SUMS（编译面前的行为一并复核）。"

    # ③ 本地文件必须与 SHA256SUMS 相符（`panel::xray::tests::vendored_protos_match_the_recorded_checksums`
    #    守同一条；这里再算一次是为了让 release 期的报错直指文件）。
    while read -r want rel; do
        [[ -n "${rel:-}" ]] || continue
        f="$dir/$rel"
        [[ -f "$f" ]] || unverifiable "SHA256SUMS 记了 $rel，但 $tree/$rel 不存在"
        got=$(sha256sum "$f" | awk '{ print $1 }')
        [[ "$got" == "$want" ]] || deny "$tree/$rel 的内容与 SHA256SUMS 记录不符："\
"记录 $want / 实际 $got。改 proto 必须同步 SHA256SUMS（否则那条 cargo 用例也会红）。"

        # ④ 联网那一半：上游在该 tag 下的原文必须与本地逐字节相同。
        url_raw="$RAW_BASE/$repo/$tag/$rel"
        err=$(mktemp)
        if ! code=$(curl -sS -o /dev/null -w '%{http_code}' --max-time 60 "$url_raw" 2>"$err"); then
            unverifiable "取上游失败（核实不了）：$url_raw：$(cat "$err")"
        fi
        rm -f "$err"
        case "$code" in
            200) ;;
            404) deny "上游 $tag 下没有 $rel（HTTP 404）：上游把文件挪了或改名了。"\
"要么在 protoc 的 include 路径里找到新位置重新 vendor，要么确认这条 API 面没了、把这条从 SHA256SUMS 删掉并改掉消费方。" ;;
            *) unverifiable "取上游返回 HTTP $code（核实不了）：$url_raw" ;;
        esac
        tmp=$(mktemp)
        curl -fsSL --max-time 60 -o "$tmp" "$url_raw" || unverifiable "取上游失败（核实不了）：$url_raw"
        up=$(sha256sum "$tmp" | awk '{ print $1 }')
        rm -f "$tmp"
        [[ "$up" == "$want" ]] || deny "$tree/$rel 与上游 $tag 的原文不一致："\
"vendored $want / 上游 $up。protobuf 字段编号不一致时不会报错、只会读错数字 ——"\
"必须按上游原文重新 vendor（并在同一提交里核对消费方的字段访问），不得只改 sha。"
    done < "$sums"
done

printf 'vendored proto 与内核锁自洽：sing-box %s（%s）逐字节一致\n' "$ver" "$tag"
exit 0
