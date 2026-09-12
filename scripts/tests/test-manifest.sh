#!/usr/bin/env bash
# manifest 生成（C4 形状）+ 校验 + 非法字段拒绝（零网络：lock 与产物都是本地假数据）。
set -uo pipefail
LC_ALL=C
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
# shellcheck source=/dev/null
. "$HERE/lib.sh"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
DIST="$WORK/dist"
mkdir -p "$DIST"
# C4：12 个资产全部是裸二进制，键名 <name>-linux-<amd64|arm64>
for n in bui bui-c hysteria xray sing-box caddy; do
    for a in amd64 arm64; do
        printf 'fake-binary-%s-%s\n' "$n" "$a" > "$DIST/$n-linux-$a"
    done
done

LOCK="$WORK/kernels.lock"
cat > "$LOCK" <<'EOF'
# kernel role version arch sha256 url
sing-box target 1.14.5 amd64 9999999999999999999999999999999999999999999999999999999999999999 https://github.com/SagerNet/sing-box/releases/download/v1.14.5/sing-box-1.14.5-linux-amd64.tar.gz
sing-box target 1.14.5 arm64 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa https://github.com/SagerNet/sing-box/releases/download/v1.14.5/sing-box-1.14.5-linux-arm64.tar.gz
xray target 26.3.27 amd64 7777777777777777777777777777777777777777777777777777777777777777 https://github.com/XTLS/Xray-core/releases/download/v26.3.27/Xray-linux-64.zip
xray target 26.3.27 arm64 8888888888888888888888888888888888888888888888888888888888888888 https://github.com/XTLS/Xray-core/releases/download/v26.3.27/Xray-linux-arm64-v8a.zip
hysteria target 2.12.2 amd64 5555555555555555555555555555555555555555555555555555555555555555 https://github.com/apernet/hysteria/releases/download/app/v2.12.2/hysteria-linux-amd64
hysteria target 2.12.2 arm64 6666666666666666666666666666666666666666666666666666666666666666 https://github.com/apernet/hysteria/releases/download/app/v2.12.2/hysteria-linux-arm64
caddy target 2.11.4 amd64 bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb https://github.com/caddyserver/caddy/releases/download/v2.11.4/caddy_2.11.4_linux_amd64.tar.gz
caddy target 2.11.4 arm64 cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc https://github.com/caddyserver/caddy/releases/download/v2.11.4/caddy_2.11.4_linux_arm64.tar.gz
sing-box check 1.13.21 amd64 dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd https://github.com/SagerNet/sing-box/releases/download/v1.13.21/sing-box-1.13.21-linux-amd64.tar.gz
EOF

M="$WORK/manifest.json"
bash "$ROOT/scripts/release/gen-manifest.sh" --version 4.0.0 --dist "$DIST" --lock "$LOCK" \
    --released 2026-09-18T07:22:10Z > "$M"
assert_eq "0" "$?" "gen-manifest 成功"

assert_eq "4.0.0" "$(jq -r '.version' "$M")" "顶层 version"
assert_eq "null" "$(jq -r '.schema // "null"' "$M")" "C4 没有 schema 字段"
assert_eq "null" "$(jq -r '.binaries // "null"' "$M")" "C4 没有 binaries 段（放弃草稿形状）"
assert_eq "5" "$(jq -r '.kernels | keys | length' "$M")" "kernels 恰好五项版本号"
assert_eq "1.14.5" "$(jq -r '.kernels.sing_box' "$M")" "kernels.sing_box 用下划线键，取 target 行"
assert_eq "1.14.5" "$(jq -r '.kernels.client_sing_box' "$M")" "client_sing_box 与服务端同版本"
assert_eq "2.11.4" "$(jq -r '.kernels.caddy' "$M")" "kernels.caddy 取 target 行"
assert_eq "string" "$(jq -r '.kernels.xray | type' "$M")" "kernels 的值是版本号字符串，不是对象"
assert_eq "12" "$(jq -r '.artifacts | keys | length' "$M")" "artifacts 恰好 12 项"
assert_eq "https://github.com/Buxiulei/b-ui/releases/download/v4.0.0/sing-box-linux-arm64" \
    "$(jq -r '.artifacts."sing-box-linux-arm64".url' "$M")" "默认 base-url 指向该 tag 的 Release 资产"
assert_eq "$(sha256sum "$DIST/bui-c-linux-arm64" | cut -d' ' -f1)" \
    "$(jq -r '.artifacts."bui-c-linux-arm64".sha256' "$M")" "sha256 来自 dist 里的真实文件"
assert_eq "0" "$(jq -r '[.artifacts[].url | select(test("\\.tar\\.gz$|\\.zip$"))] | length' "$M")" \
    "没有任何 url 指向归档（内核已由 Actions 解包重传）"
assert_eq "2" "$(jq -r '[.artifacts | keys[] | select(startswith("bui-c-"))] | length' "$M")" "bui-c 两个架构各一项"
assert_not_contains "1.13.19" "$(cat "$M")" "check 用的旧 minor 不进 manifest"
assert_not_contains "1.13.21" "$(cat "$M")" "check 行的版本不进 kernels"
assert_eq "https://github.com/Buxiulei/b-ui/releases/tag/v4.0.0" "$(jq -r '.changelog_url' "$M")" "changelog_url 指向 Release 页"
assert_eq "2026-09-18T07:22:10Z" "$(jq -r '.released' "$M")" "released 原样写入"
assert_eq "4.0.0" "$(jq -r '.min_upgrade_from' "$M")" "min_upgrade_from"

# --base-url：M5 演练把 manifest 指到本机 python3 -m http.server
ML="$WORK/manifest-local.json"
bash "$ROOT/scripts/release/gen-manifest.sh" --version 4.0.1 --dist "$DIST" --lock "$LOCK" \
    --base-url http://127.0.0.1:8000/v4.0.1 > "$ML"
assert_eq "http://127.0.0.1:8000/v4.0.1/bui-linux-amd64" \
    "$(jq -r '.artifacts."bui-linux-amd64".url' "$ML")" "--base-url 覆盖且自动补斜杠"

rm "$DIST/caddy-linux-arm64"
out=$(bash "$ROOT/scripts/release/gen-manifest.sh" --version 4.0.0 --dist "$DIST" --lock "$LOCK" 2>&1); rc=$?
assert_eq "2" "$rc" "缺任一裸二进制退 2"
assert_contains "缺少构建产物" "$out" "点名缺哪个产物"
printf 'fake-binary-caddy-arm64\n' > "$DIST/caddy-linux-arm64"

bash "$ROOT/scripts/release/validate-manifest.sh" "$M" > /dev/null
assert_eq "0" "$?" "合法 manifest 校验通过"
bash "$ROOT/scripts/release/validate-manifest.sh" "$M" \
    --require-prefix "https://github.com/Buxiulei/b-ui/releases/download/v4.0.0/" > /dev/null
assert_eq "0" "$?" "--require-prefix 匹配时通过"
out=$(bash "$ROOT/scripts/release/validate-manifest.sh" "$M" --require-prefix "http://127.0.0.1:8000/" 2>&1); rc=$?
assert_eq "3" "$rc" "--require-prefix 不匹配退 3"
assert_contains "不以要求的前缀" "$out" "指出前缀不符"

# 逐项破坏：每种违规都必须被 3 号退出码挡住
break_and_check() {
    local filter="$1" desc="$2" out rc
    jq "$filter" "$M" > "$WORK/bad.json"
    out=$(bash "$ROOT/scripts/release/validate-manifest.sh" "$WORK/bad.json" 2>&1); rc=$?
    assert_eq "3" "$rc" "$desc"
    assert_contains "C4 违规" "$out" "$desc（有 stderr 说明）"
}
break_and_check '.version = "4.0"' "version 非 semver 被拒"
break_and_check 'del(.kernels.client_sing_box)' "kernels 少 client_sing_box 被拒"
break_and_check '.kernels."sing-box" = .kernels.sing_box | del(.kernels.sing_box)' "kernels 用连字符键被拒"
break_and_check '.kernels.xray = "v26.3.27"' "内核版本带 v 前缀被拒"
break_and_check '.kernels.caddy = {"version": "2.11.4"}' "kernels 的值写成对象被拒"
break_and_check '.artifacts."bui-linux-amd64".sha256 = "zz"' "非 64 位 hex sha256 被拒"
break_and_check 'del(.artifacts."xray-linux-arm64")' "缺 artifact 被拒"
break_and_check '.artifacts."bui-linux-x86_64" = .artifacts."bui-linux-amd64" | del(.artifacts."bui-linux-amd64")' "键名用 x86_64 被拒"
break_and_check '.artifacts."caddy-linux-arm64".url = "https://github.com/o/r/releases/download/v4.0.0/caddy_2.11.4_linux_arm64.tar.gz"' "url 指向归档被拒"
break_and_check '.artifacts."xray-linux-amd64".url = "ftp://example.com/xray-linux-amd64"' "非 http(s) url 被拒"
break_and_check '.released = "2026-09-18 07:22"' "released 非 ISO8601Z 被拒"
break_and_check '.min_upgrade_from = "4.0"' "min_upgrade_from 非 semver 被拒"

# C4：消费方忽略未知字段，校验器也不许拦
jq '.future_field = {"a": 1}' "$M" > "$WORK/extra.json"
bash "$ROOT/scripts/release/validate-manifest.sh" "$WORK/extra.json" > /dev/null
assert_eq "0" "$?" "未知顶层字段放行（C4 前向兼容）"

out=$(bash "$ROOT/scripts/release/validate-manifest.sh" 2>&1); rc=$?
assert_eq "2" "$rc" "缺参数退 2"
finish
