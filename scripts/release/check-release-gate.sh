#!/usr/bin/env bash
# 发布 tag 门禁：受管 tag 前缀的必需提交必须已经是 HEAD 的祖先。
# 用法：check-release-gate.sh <tag> [--warn] [--repo <dir>] [--required <file>]
# 退出码：0 通过 / 不在门禁范围 / --warn 下不过；2 用法错误；3 门禁不过。
# 判据是提交祖先（git merge-base --is-ancestor），不是「某个测试名存在」——测试名改个名就绕过去了。
# 全程只读本地仓库：没有 curl / gh / git fetch。
set -uo pipefail
LC_ALL=C
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
TAG=""
WARN=0
REPO="$ROOT"
REQ=""

need_value() { [[ "$1" -ge 2 ]] || { printf '选项 %s 缺参数\n' "$2" >&2; exit 2; }; }
usage() {
    printf '用法：%s <v<x.y.z>[-rcN]> [--warn] [--repo <dir>] [--required <file>]\n' "$0" >&2
    exit 2
}

while [[ "$#" -gt 0 ]]; do
    case "$1" in
        --warn) WARN=1; shift ;;
        --repo) need_value "$#" "$1"; REPO="$2"; shift 2 ;;
        --required) need_value "$#" "$1"; REQ="$2"; shift 2 ;;
        -*) printf '未知选项：%s\n' "$1" >&2; usage ;;
        *) [[ -z "$TAG" ]] || usage; TAG="$1"; shift ;;
    esac
done

[[ "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-rc[0-9]+)?$ ]] || usage
REQ="${REQ:-$ROOT/scripts/release/required-commits.env}"
[[ -f "$REQ" ]] || { printf '门禁清单读不到：%s\n' "$REQ" >&2; exit 2; }

# 退 3 的三种情形：--warn 下改成 GitHub 注解走 stdout 并退 0，文案一字不改，方便 grep。
deny() {
    if [[ "$WARN" -eq 1 ]]; then
        printf '::warning::%s\n' "$1"
        exit 0
    fi
    printf '%s\n' "$1" >&2
    exit 3
}

rel="${TAG%-rc*}"             # v4.1.0-rc1 → v4.1.0：先剥 -rcN
minor="${rel%.*}"             # v4.1.0 → v4.1
key="GATE_${minor//./_}"      # v4.1 → GATE_v4_1

if ! grep -qE "^${key}=" "$REQ"; then
    printf 'tag %s 不在门禁范围（清单里没有 %s），放过。\n' "$TAG" "$key"
    exit 0
fi
sha=$(awk -F= -v k="$key" '$1 == k {print $2}' "$REQ" | tail -1)
if [[ -z "$sha" || "$sha" == "pending" ]]; then
    deny "发布门禁未配置：$REQ 里的 $key 还是 pending。填上那条提交的完整 sha 再打 tag（门禁是 fail-closed 的，不许空过）。"
fi

short="${sha:0:12}"
if ! git -C "$REPO" cat-file -e "$sha^{commit}" 2> /dev/null; then
    deny "发布门禁不通过：提交 $short 在本仓库里找不到（浅克隆？CI 的 checkout 要 fetch-depth: 0）。"
fi
head_short=$(git -C "$REPO" rev-parse --short=12 HEAD)
if ! git -C "$REPO" merge-base --is-ancestor "$sha" HEAD; then
    deny "发布门禁不通过：${minor#v}.x 需要的提交 $short 不是 HEAD 的祖先（HEAD = $head_short）。先把 bui-c「同账号重复节点不自动合并」那条改动合进本分支再打 tag（依据：spec §7.6 事实 3）。"
fi
printf '发布门禁通过：%s 的提交 %s 已在 HEAD（%s）里。\n' "$key" "$short" "$head_short"
