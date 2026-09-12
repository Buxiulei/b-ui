#!/usr/bin/env bash
# 校验 manifest.json 是否符合总纲 C4 契约。退出码：0 合法 / 2 用法或文件问题 / 3 违规（逐条打印到 stderr）。
#   --require-prefix <url>  额外要求每个 artifact 的 url 以该前缀开头（release.yml 用它钉死 Release 资产地址）
# 未知的顶层字段按 C4「消费方忽略未知字段」放行，不报错。
set -uo pipefail
LC_ALL=C

M="${1:-}"
shift || true
PREFIX=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --require-prefix) PREFIX="${2:-}"; shift 2 ;;
        *) printf '未知参数 %s\n' "$1" >&2; exit 2 ;;
    esac
done
if [[ -z "$M" || ! -f "$M" ]]; then
    printf '用法：%s <manifest.json> [--require-prefix <url>]\n' "$0" >&2
    exit 2
fi
if ! jq -e . "$M" > /dev/null 2>&1; then
    printf 'C4 违规：不是合法 JSON\n' >&2
    exit 3
fi

problems=$(jq -r --arg prefix "$PREFIX" '
    def hex64: type == "string" and test("^[0-9a-f]{64}$");
    def semver: type == "string" and test("^[0-9]+\\.[0-9]+\\.[0-9]+$");
    ["hysteria", "xray", "sing_box", "caddy", "client_sing_box"] as $kkeys
    | ([["bui", "bui-c", "hysteria", "xray", "sing-box", "caddy"][] as $n
        | ["amd64", "arm64"][] as $a
        | "\($n)-linux-\($a)"]) as $akeys
    | (if (.version | semver) then [] else ["version 不是 semver：\(.version)"] end)
    + (if (.kernels | type) == "object" then
          (if ((.kernels | keys_unsorted | sort) == ($kkeys | sort)) then []
           else ["kernels 的键必须恰好是 \($kkeys | join("/"))，实际 \(.kernels | keys_unsorted | join("/"))"] end)
        + [$kkeys[] as $k
           | (.kernels[$k] // null) as $v
           | if ($v | type) == "string" and ($v | length) > 0 and ($v | startswith("v") | not) then empty
             else "kernels.\($k) 必须是非空、不带 v 前缀的版本号：\($v)" end]
       else ["kernels 缺失或不是对象"] end)
    + (if (.artifacts | type) == "object" then
          (if ((.artifacts | keys_unsorted | sort) == ($akeys | sort)) then []
           else ["artifacts 的键必须恰好是这 12 个：\($akeys | join(" "))；实际 \(.artifacts | keys_unsorted | join(" "))"] end)
        + [$akeys[] as $k
           | (.artifacts[$k] // null) as $e
           | if $e == null then "artifacts.\($k) 缺失"
             else empty end]
        + [(.artifacts | to_entries[]) as $e
           | (if (($e.value.url // "") | test("^https?://")) then empty
              else "artifacts.\($e.key).url 必须是 http(s) URL：\($e.value.url)" end),
             (if (($e.value.url // "") | endswith("/" + $e.key)) then empty
              else "artifacts.\($e.key).url 必须以 /\($e.key) 结尾（裸二进制，不是归档）：\($e.value.url)" end),
             (if ($e.value.sha256 | hex64) then empty
              else "artifacts.\($e.key).sha256 不是 64 位 hex：\($e.value.sha256)" end),
             (if ($prefix == "" or (($e.value.url // "") | startswith($prefix))) then empty
              else "artifacts.\($e.key).url 不以要求的前缀 \($prefix) 开头" end)]
       else ["artifacts 缺失或不是对象"] end)
    + (if (.released == null or ((.released | type) == "string" and (.released | test("^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$")))) then []
       else ["released 不是 ISO8601 UTC：\(.released)"] end)
    + (if (.changelog_url == null or ((.changelog_url | type) == "string" and (.changelog_url | startswith("https://")))) then []
       else ["changelog_url 必须是 https URL：\(.changelog_url)"] end)
    + (if (.min_upgrade_from == null or (.min_upgrade_from | semver)) then []
       else ["min_upgrade_from 不是 semver：\(.min_upgrade_from)"] end)
    | .[]
' "$M" 2>&1)

if [[ -n "$problems" ]]; then
    while IFS= read -r line; do
        printf 'C4 违规：%s\n' "$line" >&2
    done <<< "$problems"
    exit 3
fi
printf '%s: C4 契约校验通过（version %s，artifacts 12 项）\n' "$M" "$(jq -r .version "$M")"
