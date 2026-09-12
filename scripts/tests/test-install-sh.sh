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

# releases 列表的真实形状：一行紧凑 JSON，按「新 → 旧」，含预发布。
# 头一条故意是 v3 的补丁版（v3 热修可能比 rc 更新），回退必须跳过它只认 v4*。
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

# ---- 1) BUI_VERSION 未设：releases/latest 404 → 回退到 releases 列表里最新的 v4* 标签 ----
: > "$DL_LOG"
TAG=latest
MANIFEST_URL=""
get_manifest "$WORK/m.json" > "$WORK/out.txt" 2>&1
assert_eq "0" "$?" "latest 404 后回退成功"
assert_eq "v4.0.0-rc2" "$TAG" "回退取 releases 列表里第一个 v4* 标签（跳过更新的 v3 补丁版）"
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

# 列表里没有 v4* → 明确提示怎么办（不静默装个 v3）
: > "$DL_LOG"
TAG=latest
printf '[{"tag_name":"v3.6.3","prerelease":false}]\n' > "$WORK/fixtures/only-v3.json"
out=$(FAKE_RELEASES="$WORK/fixtures/only-v3.json" get_manifest "$WORK/m2.json" 2>&1); rc=$?
assert_eq "1" "$rc" "取不到 v4 标签 → 退 1"
assert_contains "BUI_VERSION=vX.Y.Z-rcN" "$out" "提示可指定预发布版本"
assert_contains "BUI_MANIFEST_URL" "$out" "提示可直接指定 manifest 地址"

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

# 交接不是无条件的：stdin 正在给 --admin-password-stdin 送密码、或域名已定时，换成终端就是拆台
# （`sudo bash install.sh --non-interactive --answers a.json --admin-password-stdin < pw.txt`
#  被换掉 stdin ⇒ bui install 读到空密码、密码文件被静默忽略。2026-09-12 第三轮审查 blocking）
BUI_DOMAIN=""
assert_eq "" "$(tty_readable() { true; }; tty_source --non-interactive --answers /root/a.json --admin-password-stdin < /dev/null)" \
    "参数里有 --admin-password-stdin → 不交接（stdin 留给密码）"
assert_eq "" "$(tty_readable() { true; }; tty_source --domain panel.example.com < /dev/null)" \
    "域名已给 → 不交接（bui install 那一问不会开口）"
assert_eq "/dev/tty" "$(tty_readable() { true; }; tty_source --yes --port 20000 < /dev/null)" \
    "两者都没有 + stdin 非终端 + /dev/tty 可开 → 交接"
# 域名「已定」的其余三种来源，与 domain_known 的宽松判定（答案文件给了就算）区分开
assert_eq "" "$(tty_readable() { true; }; BUI_DOMAIN=panel.example.com tty_source --yes < /dev/null)" \
    "BUI_DOMAIN 也算域名已定 → 不交接"
assert_eq "" "$(tty_readable() { true; }; tty_source --import-v3 < /dev/null)" \
    "从 v3 导入沿用 v3 域名 → 不交接"
printf '{"node_name":"bwg","masquerade_domain":"www.bing.com"}\n' > "$WORK/fixtures/answers-nodomain.json"
printf '{"domain": "panel.example.com"}\n' > "$WORK/fixtures/answers-domain.json"
assert_eq "/dev/tty" "$(tty_readable() { true; }; tty_source --non-interactive --answers "$WORK/fixtures/answers-nodomain.json" < /dev/null)" \
    "答案文件里没有 domain（且没有 --admin-password-stdin）→ 仍要交接，那一问还得问"
assert_eq "" "$(tty_readable() { true; }; tty_source --non-interactive --answers="$WORK/fixtures/answers-domain.json" < /dev/null)" \
    "答案文件里有 domain（--answers=值 写法）→ 不交接"
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
