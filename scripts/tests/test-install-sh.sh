#!/usr/bin/env bash
# install.sh 的「一行命令零手工安装」四条：releases/latest 404 回退预发布、wget 回退、
# 「有没有终端可问」的两种形态（stdin 是管道 / stdin 就是终端）与无域名即报错不装一半、
# 参数原样透传。全部 stub，零网络、无需 root。
#
# 与 test-install.sh 分家的理由：那份测的是 M1 的引导骨架（架构/镜像顺序/manifest 解析/sha256），
# 这份测 2026-09-12 裁决「安装：一行命令与零手工配置」「发布：预发布与首推」新加的四条。
set -uo pipefail
LC_ALL=C
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
# shellcheck source=/dev/null
. "$HERE/lib.sh"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/bin" "$WORK/fresh" "$WORK/fixtures"

# releases 列表的形状：一行紧凑 JSON，含预发布；顺序不可信（见下面「按版本号取最大」那段）。
# 头一条故意是 v3 的补丁版（v3 热修可能比 rc 更新）：它不是预发布 rc，回退必须跳过它。
# 放 fixtures/ 而不是 $WORK 根：latest_v4_tag 会往 <manifest 同目录>/releases.json 落地，
# 同名会被 `> "$out"` 先截断，cat 到的就是空文件。
cat > "$WORK/fixtures/releases.json" <<'EOF'
[{"tag_name":"v3.6.3","prerelease":false},{"tag_name":"v4.0.0-rc2","prerelease":true},{"tag_name":"v4.0.0-rc1","prerelease":true}]
EOF

# uname stub：按 FAKE_MACHINE 返回架构
cat > "$WORK/bin/uname" <<'STUB'
#!/usr/bin/env bash
if [[ "${1:-}" == "-m" ]]; then printf '%s\n' "${FAKE_MACHINE:-x86_64}"; else /usr/bin/uname "$@"; fi
STUB
# curl stub：记录 URL；releases/latest 一律 404（仓库里只有预发布），releases 列表回上面那份，
# 其余 URL 回一份假 manifest。
cat > "$WORK/bin/curl" <<'STUB'
#!/usr/bin/env bash
out=""; url=""
while [[ $# -gt 0 ]]; do
  case "$1" in -o) out="$2"; shift 2 ;; http*) url="$1"; shift ;; *) shift ;; esac
done
printf 'curl %s\n' "$url" >> "$DL_LOG"
case "$url" in
  *releases/latest/download/*) exit 22 ;;
  *api.github.com/repos/*/releases*) cat "$FAKE_RELEASES" > "$out" ;;
  *) printf 'MANIFEST %s' "$url" > "$out" ;;
esac
STUB
# wget stub：同样的分派，参数形状换成 -O
cat > "$WORK/bin/wget" <<'STUB'
#!/usr/bin/env bash
out=""; url=""
while [[ $# -gt 0 ]]; do
  case "$1" in -O) out="$2"; shift 2 ;; http*) url="$1"; shift ;; *) shift ;; esac
done
printf 'wget %s\n' "$url" >> "$DL_LOG"
case "$url" in
  *releases/latest/download/*) exit 8 ;;
  *) printf 'MANIFEST %s' "$url" > "$out" ;;
esac
STUB
chmod +x "$WORK/bin/uname" "$WORK/bin/curl" "$WORK/bin/wget"
export PATH="$WORK/bin:$PATH"
export DL_LOG="$WORK/dl.log"
export FAKE_RELEASES="$WORK/fixtures/releases.json"

BUI_BOOTSTRAP_SOURCED=1
BUI_MIRRORS=""   # 只试直连，日志里只剩要断言的那几条
# shellcheck source=/dev/null
. "$ROOT/install.sh"
# install.sh 顶部是 set -euo pipefail，源入后会污染测试外壳（测试要主动读非零退出码），必须关掉
set +eu

# ---- 1) BUI_VERSION 未设：releases/latest 404 → 回退到 releases 列表里版本号最大的预发布 rc ----
: > "$DL_LOG"
TAG=latest
MANIFEST_URL=""
get_manifest "$WORK/m.json" > "$WORK/out.txt" 2>&1
assert_eq "0" "$?" "latest 404 后回退成功"
assert_eq "v4.0.0-rc2" "$TAG" "回退取版本号最大的预发布 rc（跳过更新的 v3 补丁版）"
assert_eq "MANIFEST https://github.com/Buxiulei/b-ui/releases/download/v4.0.0-rc2/manifest.json" \
    "$(cat "$WORK/m.json")" "落地的是那个预发布 tag 的 manifest"
assert_eq "curl https://github.com/Buxiulei/b-ui/releases/latest/download/manifest.json" \
    "$(sed -n 1p "$DL_LOG")" "第 1 次请求仍是 releases/latest"
assert_eq "curl https://api.github.com/repos/Buxiulei/b-ui/releases?per_page=100" \
    "$(sed -n 2p "$DL_LOG")" "404 后才去问 releases 列表"
assert_contains "回退到预发布 v4.0.0-rc2" "$(cat "$WORK/out.txt")" "回退有中文提示"
# 404 不是「源不可达」：第一次尝试静默，别先甩一句误导性的「直连与全部镜像均不可达」
assert_not_contains "直连与全部镜像均不可达" "$(cat "$WORK/out.txt")" \
    "releases/latest 404 时不报「均不可达」（源没坏，只是还没正式版）"
assert_contains "只有预发布" "$(cat "$WORK/out.txt")" "回退前说清为什么回退"
# fetch 的 quiet 只作用于第三个参数给了的那一次；平时照旧报错
: > "$DL_LOG"
out=$(fetch "https://github.com/Buxiulei/b-ui/releases/latest/download/manifest.json" "$WORK/q.json" 2>&1); rc=$?
assert_eq "1" "$rc" "取不到就返回 1"
assert_contains "直连与全部镜像均不可达" "$out" "不给 quiet 时照旧报错"
out=$(fetch "https://github.com/Buxiulei/b-ui/releases/latest/download/manifest.json" "$WORK/q.json" quiet 2>&1); rc=$?
assert_eq "1" "$rc" "quiet 也返回 1"
assert_not_contains "直连与全部镜像均不可达" "$out" "quiet 时不报那句"

# 列表里没有预发布 rc → 明确提示怎么办（不静默装个 v3）
: > "$DL_LOG"
TAG=latest
printf '[{"tag_name":"v3.6.3","prerelease":false}]\n' > "$WORK/fixtures/only-v3.json"
out=$(FAKE_RELEASES="$WORK/fixtures/only-v3.json" get_manifest "$WORK/m2.json" 2>&1); rc=$?
assert_eq "1" "$rc" "取不到预发布 rc → 退 1"
assert_contains "BUI_VERSION=vX.Y.Z-rcN" "$out" "提示可指定预发布版本"
assert_contains "BUI_MANIFEST_URL" "$out" "提示可直接指定 manifest 地址"

# 大响应 + pipefail（bui-c 负责人 2026-09-13 报告，客户端脚本 784c525 修过同一处）：真实的
# releases 列表 200KB+，最新 rc 在最前。`tr | awk '{…; exit}'` 命中即退，tr 还有 ≫64KB（管道缓冲）
# 没写完 ⇒ SIGPIPE（141）⇒ pipefail 判整条管道失败 ⇒ 回退失效。本测试外壳开着 pipefail，
# 与 install.sh 的 set -euo pipefail 同条件。3000 条旧 rc 各带 100 字节正文，把体积撑到 ~400KB。
awk 'BEGIN {
    printf "[{\"tag_name\":\"v4.0.1-rc3\",\"prerelease\":true,\"body\":\"最新预发布\"}"
    for (i = 3000; i > 0; i--) printf ",{\"tag_name\":\"v4.0.0-rc%d\",\"prerelease\":true,\"body\":\"%0100d\"}", i, 0
    print "]"
}' > "$WORK/fixtures/releases-big.json"
assert_eq "1" "$([[ $(wc -c < "$WORK/fixtures/releases-big.json") -gt 262144 ]] && echo 1 || echo 0)" \
    "大夹具确实超过 256KB（小了测不出 SIGPIPE）"
mkdir -p "$WORK/big"
tag=$(FAKE_RELEASES="$WORK/fixtures/releases-big.json" latest_v4_tag "$WORK/big" 2> /dev/null); rc=$?
assert_eq "0" "$rc" "releases 列表 >256KB 时 latest_v4_tag 退出码 0（不被 SIGPIPE 误杀）"
assert_eq "v4.0.1-rc3" "$tag" "大列表里取版本号最大的那个（只打印一个）"
: > "$DL_LOG"
TAG=latest
FAKE_RELEASES="$WORK/fixtures/releases-big.json" get_manifest "$WORK/big/m.json" > /dev/null 2>&1
assert_eq "0" "$?" "大列表下 latest 404 → 回退预发布照样成功"
assert_eq "v4.0.1-rc3" "$TAG" "回退到大列表里最新的 rc"
TAG=latest

# rc 通道按版本号 (x, y, z, N) 取最大、不看列表顺序（与 bui / bui-c 的 latest_rc_tag 同口径）：
# 2026-09-13 实测 GitHub 的 releases 列表返回 rc9 → rc8 → rc7 → rc10 → rc6，最新的 rc10 排第 4。
# 第一条用带空格与换行的 pretty 形状，其余用紧凑形状，两种写法都要认。
mkdir -p "$WORK/pick"
pick() { printf '%s\n' "$1" > "$WORK/fixtures/pick.json"; FAKE_RELEASES="$WORK/fixtures/pick.json" latest_v4_tag "$WORK/pick" 2> /dev/null; }
assert_eq "v4.0.0-rc10" "$(pick '[
  {"tag_name": "v4.0.0-rc9", "prerelease": true},
  {"tag_name": "v4.0.0-rc8", "prerelease": true},
  {"tag_name": "v4.0.0-rc7", "prerelease": true},
  {"tag_name": "v4.0.0-rc10", "prerelease": true},
  {"tag_name": "v4.0.0-rc6", "prerelease": true}
]')" "真实乱序里取 rc10（不是排第一的 rc9）"
assert_eq "v4.0.1-rc1" "$(pick '[{"tag_name":"v4.0.0-rc9","prerelease":true},{"tag_name":"v4.0.0-rc10","prerelease":true},{"tag_name":"v4.0.1-rc1","prerelease":true}]')" \
    "跨版本按数值比：v4.0.1-rc1 > v4.0.0-rc10 > v4.0.0-rc9"
assert_eq "v10.0.0-rc1" "$(pick '[{"tag_name":"v9.9.9-rc9","prerelease":true},{"tag_name":"v10.0.0-rc1","prerelease":true},{"tag_name":"v4.1.0-rc1","prerelease":true}]')" \
    "主版本也按数值比（v10 > v9，不是字典序）"
assert_eq "v4.0.0-rc10" "$(pick '[{"tag_name":"v4.0.2-rc1","prerelease":false},{"tag_name":"v4.0.0-rc9","prerelease":true},{"tag_name":"v4.0.0-rc10","prerelease":true}]')" \
    "prerelease=false 的 rc 形 tag 跳过（哪怕版本号最大）"
assert_eq "v4.0.0-rc10" "$(pick '[{"tag_name":"v4.0.0-rc4294967296","prerelease":true},{"tag_name":"v4.0.99999999999999999999-rc1","prerelease":true},{"tag_name":"v5.0.0-rc1x","prerelease":true},{"tag_name":"v5.0-rc1","prerelease":true},{"tag_name":"v5.0.0","prerelease":true},{"tag_name":"nightly","prerelease":true},{"tag_name":"v4.0.0-rc9","prerelease":true},{"tag_name":"v4.0.0-rc10","prerelease":true}]')" \
    "畸形与超出 u32 的 tag 跳过（与 Rust 侧同一个上界）"
assert_eq "v4.0.0-rc4294967295" "$(pick '[{"tag_name":"v4.0.0-rc10","prerelease":true},{"tag_name":"v4.0.0-rc4294967295","prerelease":true}]')" \
    "u32 上界本身还算数"
# 前导零：release.yml 的 tag 正则 ^v[0-9]+\.[0-9]+\.[0-9]+-rc[0-9]+$ 放它过、会被标成预发布。每段按数值比，
# 不是字典序（rc010 = 10 > 9，v4.010.0 = v4.10.0 > v4.9.0），两种排列取到同一个；数值相同（rc1 与 rc01）
# 取列表里后出现的那个。这六条与 bui / bui-c 的 leading_zeros_compare_by_value_not_lexically、
# equal_versions_resolve_to_the_later_listed_tag 逐条相同：改了 awk 的比较分支或 max_by_key，两边就在这里分叉。
pick2() { pick "[{\"tag_name\":\"$1\",\"prerelease\":true},{\"tag_name\":\"$2\",\"prerelease\":true}]"; }
assert_eq "v4.0.0-rc010" "$(pick2 v4.0.0-rc9 v4.0.0-rc010)" "rc010 = 10 > rc9（不是字典序）"
assert_eq "v4.0.0-rc010" "$(pick2 v4.0.0-rc010 v4.0.0-rc9)" "rc010 > rc9，换个顺序也一样"
assert_eq "v4.010.0-rc1" "$(pick2 v4.9.0-rc1 v4.010.0-rc1)" "v4.010.0 = v4.10.0 > v4.9.0（不是字典序）"
assert_eq "v4.010.0-rc1" "$(pick2 v4.010.0-rc1 v4.9.0-rc1)" "v4.010.0 > v4.9.0，换个顺序也一样"
assert_eq "v4.0.0-rc01" "$(pick2 v4.0.0-rc1 v4.0.0-rc01)" "rc1 与 rc01 平局取后出现的 rc01"
assert_eq "v4.0.0-rc1" "$(pick2 v4.0.0-rc01 v4.0.0-rc1)" "rc01 与 rc1 平局取后出现的 rc1"

# BUI_MANIFEST_URL 给了就只认它，不去问 releases 列表（M5 演练 / 离线源）
: > "$DL_LOG"
TAG=latest
MANIFEST_URL="http://127.0.0.1:8000/v4.0.0/manifest.json"
get_manifest "$WORK/m3.json" > /dev/null 2>&1
assert_eq "0" "$?" "BUI_MANIFEST_URL 直连即可"
assert_eq "1" "$(wc -l < "$DL_LOG")" "只请求一次，不回退到 releases 列表"
MANIFEST_URL=""
TAG=latest

# ---- 2) 缺 curl 用 wget ----
assert_eq "curl" "$(pick_dl)" "curl 在就用 curl"
mkdir -p "$WORK/wgetonly"
cp "$WORK/bin/wget" "$WORK/wgetonly/wget"
# command -v 是内建命令，不需要 PATH 上有任何东西：把 PATH 收成只剩 wget 即可
assert_eq "wget" "$(PATH="$WORK/wgetonly" pick_dl)" "没有 curl 时选 wget"
assert_eq "" "$(PATH="$WORK/fresh" pick_dl)" "两个都没有时输出空串（main 据此报错退出）"
: > "$DL_LOG"
(
    pick_dl() { printf 'wget\n'; }
    fetch "https://github.com/Buxiulei/b-ui/releases/download/v4.0.0-rc2/manifest.json" "$WORK/w.json"
) > /dev/null 2>&1
assert_eq "0" "$?" "只有 wget 时下载照样成功"
assert_eq "wget https://github.com/Buxiulei/b-ui/releases/download/v4.0.0-rc2/manifest.json" \
    "$(cat "$DL_LOG")" "走的是 wget 那条路径（curl 一次都没调）"
assert_contains "MANIFEST" "$(cat "$WORK/w.json")" "wget 也把内容落地了"

# ---- 3) 有没有终端可问：stdin 是管道与 stdin 就是终端两种形态都要覆盖；
#         两种都问不到又没给域名 ⇒ 报用法、退 2（此时还没下载任何东西）----
: > "$DL_LOG"
out=$(
    tty_readable() { false; }
    BUI_DOMAIN="" require_domain --yes 2>&1
    printf 'rc=%s' "$?"
)
assert_contains "rc=2" "$out" "无 TTY 无域名 → 退 2"
assert_contains "缺少面板域名" "$out" "报错点明唯一必填项"
assert_contains "--domain" "$out" "错误里带用法"
assert_eq "" "$(cat "$DL_LOG")" "报错时一个字节都没下载（不装一半）"
out=$(
    tty_readable() { false; }
    BUI_DOMAIN="" require_domain --domain panel.example.com 2>&1
    printf 'rc=%s' "$?"
)
assert_eq "rc=0" "$out" "给了 --domain 就放行（无 TTY 也能装）"
out=$(
    tty_readable() { false; }
    BUI_DOMAIN="panel.example.com" require_domain --yes 2>&1
    printf 'rc=%s' "$?"
)
assert_eq "rc=0" "$out" "BUI_DOMAIN 等价于 --domain"
out=$(
    tty_readable() { true; }
    BUI_DOMAIN="" require_domain --yes < /dev/null 2>&1
    printf 'rc=%s' "$?"
)
assert_eq "rc=0" "$out" "没给域名但 /dev/tty 能开 → 放行，由 bui install 问那一问"
# tty 交接判定本身：stdin 是管道（测试里重定向自 /dev/null）+ /dev/tty 可开 → 交出 /dev/tty
assert_eq "/dev/tty" "$(tty_readable() { true; }; tty_source < /dev/null)" "管道里跑且 /dev/tty 可开 → /dev/tty"
assert_eq "" "$(tty_readable() { false; }; tty_source < /dev/null)" "/dev/tty 开不了 → 空串（原样继承 stdin）"

# 交接条件 = 「bui install 将要提问」（2026-09-12 裁决「一行命令后问答式配置」）：
# 非 --yes/--non-interactive + 参数里没有 --admin-password-stdin + $BUI_DOMAIN 为空 +
# stdin 非终端 + /dev/tty 可开。
# 不再只看命令行上的域名是否已知：`--domain` 之后还有面板密码（默认随机）、HY2 端口、
# REALITY 伪装站、第一个用户名、节点名、公网 IP 要问，`--domain` 给了那几问照样得开口。
BUI_DOMAIN=""
assert_eq "" "$(tty_readable() { true; }; tty_source --non-interactive --answers /root/a.json --admin-password-stdin < /dev/null)" \
    "参数里有 --admin-password-stdin → 不交接（stdin 留给密码）"
assert_eq "" "$(tty_readable() { true; }; tty_source --domain panel.example.com --admin-password-stdin < /dev/null)" \
    "--admin-password-stdin 单独出现也不交接"
assert_eq "" "$(tty_readable() { true; }; tty_source --yes --port 20000 < /dev/null)" \
    "--yes 一个问题都不问 → 不交接"
assert_eq "" "$(tty_readable() { true; }; tty_source -y < /dev/null)" \
    "-y 是 --yes 的简写 → 不交接"
assert_eq "" "$(tty_readable() { true; }; tty_source --non-interactive --answers /root/a.json < /dev/null)" \
    "--non-interactive → 不交接"
assert_eq "/dev/tty" "$(tty_readable() { true; }; tty_source --port 20000 < /dev/null)" \
    "要提问 + stdin 非终端 + /dev/tty 可开 → 交接"
assert_eq "" "$(tty_readable() { false; }; tty_source --port 20000 < /dev/null)" \
    "/dev/tty 开不了 → 不交接（原样继承 stdin）"
# --domain 已给也照样交接：这正是本次改掉的那一条（旧版只看域名，密码等几问全被闷掉）
assert_eq "/dev/tty" "$(tty_readable() { true; }; tty_source --domain panel.example.com < /dev/null)" \
    "--domain 已给但面板密码/端口/伪装站/首用户还要问 → 交接"
# $BUI_DOMAIN 非空则相反：裁决「已给的项不问」把它定成等同 --yes（bui install 的 asks_nothing
# 同一条），一个问题都不问，这里交接一个 fd 过去没人读
assert_eq "" "$(tty_readable() { true; }; BUI_DOMAIN=panel.example.com tty_source < /dev/null)" \
    "BUI_DOMAIN 非空 = 无人值守 → 不交接"
assert_eq "/dev/tty" "$(tty_readable() { true; }; BUI_DOMAIN="   " tty_source --port 20000 < /dev/null)" \
    "BUI_DOMAIN 纯空白 = 打错了，bui install 的 asks_nothing 照样要问域名 → 交接"
assert_eq "/dev/tty" "$(tty_readable() { true; }; tty_source --import-v3 < /dev/null)" \
    "--import-v3 沿用 v3 的值、其实什么都不问，但交接无害（bui install 不会去读）"
printf '{"node_name":"bwg","masquerade_domain":"www.bing.com"}\n' > "$WORK/fixtures/answers-nodomain.json"
printf '{"domain": "panel.example.com"}\n' > "$WORK/fixtures/answers-domain.json"
assert_eq "/dev/tty" "$(tty_readable() { true; }; tty_source --answers "$WORK/fixtures/answers-nodomain.json" < /dev/null)" \
    "答案文件没有 --non-interactive 时照旧要问 → 交接"
assert_eq "" "$(tty_readable() { true; }; tty_source --non-interactive --answers="$WORK/fixtures/answers-domain.json" < /dev/null)" \
    "--non-interactive 一个都不问 → 不交接"
answers_domain --answers "$WORK/fixtures/answers-nodomain.json"
assert_eq "1" "$?" "masquerade_domain 这种带前缀的键不误命中 domain"

# stdin 本身就是终端（`sudo bash install.sh`、`bash <(curl …)`、先下载再跑）：`[[ -t 0 ]]`
# 真假没法 stub，起个伪终端真跑一遍——并把 /dev/tty 摁成不可用，于是「可问」的唯一证据
# 就是 stdin 自己是终端。这正是把 tty_source 当「有没有终端」判据时漏掉的分支：那时它
# 输出空串（含义只是「不需要交接 /dev/tty」），require_domain 会把这种形态一并误拒退 2。
if command -v python3 > /dev/null 2>&1; then
    out=$(python3 - "$ROOT" <<'PYEOF'
import os, pty, sys

root = sys.argv[1]
snippet = (
    "export BUI_BOOTSTRAP_SOURCED=1 BUI_DOMAIN=; "
    ". '%s/install.sh'; set +eu; "
    "tty_readable() { false; }; "
    "require_domain --yes; printf 'rc=%%s;handoff=[%%s]' \"$?\" \"$(tty_source)\""
) % root
pid, fd = pty.fork()
if pid == 0:
    os.execv("/bin/bash", ["bash", "-c", snippet])
buf = b""
while True:
    try:
        chunk = os.read(fd, 4096)
    except OSError:
        break
    if not chunk:
        break
    buf += chunk
os.waitpid(pid, 0)
sys.stdout.write(buf.decode("utf-8", "replace"))
PYEOF
)
    assert_contains "rc=0" "$out" "stdin 真是终端时不给域名也放行（这一问交给 bui install）"
    assert_not_contains "缺少面板域名" "$out" "stdin 是终端时不该报「缺少面板域名」"
    assert_contains "handoff=[]" "$out" "stdin 已是终端 → 不交接 /dev/tty（原样继承 stdin）"
else
    printf '# skip: 没有 python3，跳过伪终端那一条\n'
fi

# 域名来源：--answers 文件与 v3 导入也算（前者文件里带 domain，后者沿用 v3 的域名）
BUI_DOMAIN=""
domain_known --non-interactive --answers /root/answers.json
assert_eq "0" "$?" "--answers 文件算一个域名来源"
domain_known --import-v3
assert_eq "0" "$?" "从 v3 导入算一个域名来源"
domain_known --yes --port 20000
assert_eq "1" "$?" "只有 --yes / --port 不算"
domain_known --domain=panel.example.com
assert_eq "0" "$?" "--domain=值 这种写法也认"

# ---- 4) 参数原样透传给 bui install（顺序与写法都不改）----
assert_eq "$(printf -- '--domain\npanel.example.com\n--yes\n--port\n20000')" \
    "$(install_args "$WORK/fresh" --domain panel.example.com --yes --port 20000)" \
    "一行命令的参数逐个原样透传"
assert_eq "$(printf -- '--domain=panel.example.com\n--admin-password-stdin')" \
    "$(install_args "$WORK/fresh" --domain=panel.example.com --admin-password-stdin)" \
    "--key=value 与 stdin 密码开关照样透传"
: > "$WORK/fresh/users.json"
assert_eq "$(printf -- '--domain\npanel.example.com\n--import-v3')" \
    "$(install_args "$WORK/fresh" --domain panel.example.com)" \
    "v3 机器上自动追加 --import-v3，用户参数不动"
rm -f "$WORK/fresh/users.json"

# ---- 5) 选定的 manifest 地址要交给 bui install（2026-09-12 真机 bwg-tizi）----
# 那次 install.sh 从 releases/download/v4.0.0-rc2/ 正确下到了 manifest 与 bui，可 exec 之后
# bui install 又去问 releases/latest 并 404 ⇒ 四个内核一个都没装、中止守卫把安装拦了。
# 修法：get_manifest 把**实际用的**那个地址 export 成 BUI_MANIFEST_URL（与 C4 的覆盖同名同义），
# 于是 exec 出去的 bui install 认同一份 manifest。
manifest_env() { bash -c 'printf "%s" "${BUI_MANIFEST_URL-unset}"'; }

# BUI_VERSION=vX ⇒ 交下去的是 releases/download/vX/manifest.json
unset BUI_MANIFEST_URL
MANIFEST_URL=""
TAG="v4.0.1"
get_manifest "$WORK/m4.json" > /dev/null 2>&1
assert_eq "0" "$?" "指定版本时 manifest 下载成功"
assert_eq "https://github.com/Buxiulei/b-ui/releases/download/v4.0.1/manifest.json" \
    "$BUI_MANIFEST_URL" "BUI_VERSION=vX ⇒ BUI_MANIFEST_URL 指向该 tag 的 manifest"
assert_eq "https://github.com/Buxiulei/b-ui/releases/download/v4.0.1/manifest.json" \
    "$(manifest_env)" "真的 export 了（exec 出去的 bui install 能看到）"

# latest 404 回退到预发布 ⇒ 交下去的是回退后那个 tag，不是 latest
unset BUI_MANIFEST_URL
MANIFEST_URL=""
TAG=latest
get_manifest "$WORK/m5.json" > /dev/null 2>&1
assert_eq "0" "$?" "回退成功"
assert_eq "https://github.com/Buxiulei/b-ui/releases/download/v4.0.0-rc2/manifest.json" \
    "$BUI_MANIFEST_URL" "回退到预发布后交下去的是那个 tag 的 manifest（不是 latest）"
assert_eq "https://github.com/Buxiulei/b-ui/releases/download/v4.0.0-rc2/manifest.json" \
    "$(manifest_env)" "回退后的地址也 export 了"

# 用户已显式设了 BUI_MANIFEST_URL ⇒ 原样不动（M5 演练的本机源）
unset BUI_MANIFEST_URL
MANIFEST_URL="http://127.0.0.1:8000/v4.0.0/manifest.json"
TAG=latest
get_manifest "$WORK/m6.json" > /dev/null 2>&1
assert_eq "http://127.0.0.1:8000/v4.0.0/manifest.json" "$BUI_MANIFEST_URL" \
    "用户显式给的 BUI_MANIFEST_URL 保持不变"

# latest 成功（仓库已有正式版）⇒ 交下去的就是 latest 那一串
unset BUI_MANIFEST_URL
MANIFEST_URL=""
TAG=latest
(
    fetch() { printf 'MANIFEST %s' "$1" > "$2"; }
    get_manifest "$WORK/m7.json" > /dev/null 2>&1
    printf '%s' "$BUI_MANIFEST_URL" > "$WORK/env7.txt"
)
assert_eq "https://github.com/Buxiulei/b-ui/releases/latest/download/manifest.json" \
    "$(cat "$WORK/env7.txt")" "latest 拿到了就交 latest 那一串"

# 取不到 manifest（列表里没有 v4*）⇒ 不设它，让 bui install 自己按 C4 解析
unset BUI_MANIFEST_URL
MANIFEST_URL=""
TAG=latest
FAKE_RELEASES="$WORK/fixtures/only-v3.json" get_manifest "$WORK/m8.json" > /dev/null 2>&1
assert_eq "1" "$?" "取不到就退 1"
assert_eq "unset" "$(manifest_env)" "没取到 manifest 时不乱设 BUI_MANIFEST_URL"
MANIFEST_URL=""
TAG=latest

# ---- 顶部注释：一行命令与三个环境变量写清楚了 ----
head=$(sed -n '1,20p' "$ROOT/install.sh")
assert_contains "bash -s -- --domain" "$head" "顶部注释给出一行命令"
assert_contains "BUI_DOMAIN" "$head" "顶部注释写了 BUI_DOMAIN"
assert_contains "BUI_VERSION" "$head" "顶部注释写了 BUI_VERSION"
assert_contains "BUI_MANIFEST_URL" "$head" "顶部注释写了 BUI_MANIFEST_URL"
finish
