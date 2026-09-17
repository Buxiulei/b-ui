#!/usr/bin/env bash
# 演练脚本：成功路径退 0；订阅在升级后漂移时退 1。全 stub，零网络。
# 4.1 追加（spec §9.1）：缺 `nft` 退 2、升级相位的 nft 表与 `:40000` 断言、4.0.x 槽位实例
# 「一个不剩」、自建 sing-box 的两道闸门（计划那一行按执行二进制的代次分档）、回滚相位
# 「表必须消失 + 按槽端口逐个在听 + 每槽单元 active」。nft / ss / sleep 一律 stub。
set -uo pipefail
LC_ALL=C
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
# shellcheck source=/dev/null
. "$HERE/lib.sh"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
# nft 的 stub 单独放一个目录：「PATH 里没有 nft」那条分支要在不丢其余 stub 的前提下把它摘掉
mkdir -p "$WORK/bin" "$WORK/nftbin" "$WORK/base/bin" "$WORK/nftstate"
# 演练按 state.json 里的 users[].sub_token 拼订阅 URL（2026-09-14 裁决：四个免鉴权端点
# 不再认用户名），所以 stub 的 state.json 得是真 JSON 且带 token
ALICE_TOK=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
BOB_TOK=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
write_state() {  # $1 = bob 的 sub_token（空串 = bob 还没补齐 token）
    local bob='{"username":"bob"}'
    [[ -n "${1:-}" ]] && bob=$(printf '{"username":"bob","sub_token":"%s"}' "$1")
    # residential.slots 是回滚相位「按槽端口逐个在听 + 每槽单元 active」的口径来源（spec §9.1）：
    # 两个槽 ⇒ 4.0.x 的 hysteria-residential（:40000）与 hysteria-residential-1（:40001）
    printf '{"users":[{"username":"alice","sub_token":"%s"},%s],"residential":{"slots":[{"index":0},{"index":1}]}}\n' \
        "$ALICE_TOK" "$bob" > "$WORK/base/state.json"
}
write_state "$BOB_TOK"
printf 'listen: :10000,20000-30000\n' > "$WORK/base/config.yaml"
printf '{}\n' > "$WORK/base/xray-config.json"
printf '4.0.0\n' > "$WORK/version"

# bui stub：记录 argv；--version 读文件；upgrade 打印升级计划、改版本、换内核二进制、
# 把盘上形态从 4.0.x 翻到 4.1；--rollback 反向翻回去（内核除外，见 FAKE_KERNEL_STUCK）
cat > "$WORK/bin/bui" <<'STUB'
#!/usr/bin/env bash
printf 'bui %s\n' "$*" >> "$BUI_LOG"
kernels() { printf 'bui hysteria xray caddy\n'; }
# 四个内核用纯文本占位；sing-box 是可执行桩——它 `version` 的 Tags 行就是演练那道
# with_v2ray_api 闸门读的东西（只有自建那一份带）
write_kernels() {  # $1 = v1（4.0.x，官方归档）| v2（4.1，自建）
  local b tags="with_quic,with_utls"
  for b in $(kernels); do printf 'bin-%s-%s\n' "$b" "$1" > "$BASEDIR/bin/$b"; done
  [[ "$1" == v2 && "${FAKE_SINGBOX_NO_V2RAY_API:-0}" != "1" ]] && tags="$tags,with_v2ray_api"
  cat > "$BASEDIR/bin/sing-box" <<EOF
#!/usr/bin/env bash
printf 'sing-box version 1.14.1 ($1)\nTags: $tags\n'
EOF
  chmod 755 "$BASEDIR/bin/sing-box"
  return 0
}
# 4.0.x 的盘上形态：每槽一份 config-residential*.yaml、每槽一个监听端口、没有 inet bui 表
lay_40() {
  printf 'listen: :40000,41000-50000\n' > "$BASEDIR/config-residential.yaml"
  printf 'listen: :40001,41000-50000\n' > "$BASEDIR/config-residential-1.yaml"
  rm -f "$BASEDIR/hy2-residential.json"
  printf 'UNCONN 0 0 0.0.0.0:40000 0.0.0.0:*\n' > "$NFT_STATE/listen"
  # FAKE_RESI_PORT_DOWN=1：回滚后槽 1 的 apernet 实例没回到 :40001
  [[ "${FAKE_RESI_PORT_DOWN:-0}" == "1" ]] ||
    printf 'UNCONN 0 0 0.0.0.0:40001 0.0.0.0:*\n' >> "$NFT_STATE/listen"
  # 回滚必须先删表（spec §9.1）；NFT_KEEP_AFTER_ROLLBACK=1 模拟没删
  if [[ "${NFT_KEEP_AFTER_ROLLBACK:-0}" != "1" ]]; then
    : > "$NFT_STATE/tables"
    : > "$NFT_STATE/bui"
  fi
  return 0
}
# 4.1 的盘上形态：一份 hy2-residential.json、只有 :40000 在听、inet bui 四条 redirect
lay_41() {
  # FAKE_SLOW_SETTLE=<轮数>：真机上这些全由**重启后的守护进程**异步做（拉内核、渲染 +
  # check、删旧 yaml、nft apply、换单元），`bui upgrade` 早就返回了。这里把翻面推迟 n 轮采样，
  # 由 sleep 桩当时钟触发（`$0 __apply41`），钉住演练是「轮询到收敛」而不是「固定睡 5 秒」
  if [[ -n "${FAKE_SLOW_SETTLE:-}" ]]; then
    printf '%s\n' "$FAKE_SLOW_SETTLE" > "$NFT_STATE/pending"
    printf '%s __apply41\n' "$0" > "$NFT_STATE/converge"
    return 0
  fi
  # FAKE_RESI_YAML_KEPT=1：配置转换没发生（旧 yaml 还在、新 json 没落）
  if [[ "${FAKE_RESI_YAML_KEPT:-0}" != "1" ]]; then
    rm -f "$BASEDIR"/config-residential*.yaml
    printf '{"inbounds":[{"tag":"hy2-resi"}]}\n' > "$BASEDIR/hy2-residential.json"
  fi
  # FAKE_40000_DOWN=1：4.1 的住宅入站没起来（:40000 无人监听 ⇒ 住宅 HY2 全断）
  if [[ "${FAKE_40000_DOWN:-0}" == "1" ]]; then
    : > "$NFT_STATE/listen"
  else
    printf 'UNCONN 0 0 0.0.0.0:40000 0.0.0.0:*\n' > "$NFT_STATE/listen"
  fi
  # FAKE_RESI_PORT_STUCK=1：4.0.x 槽 1 的 apernet 实例没被停掉，还占着 :40001
  [[ "${FAKE_RESI_PORT_STUCK:-0}" != "1" ]] ||
    printf 'UNCONN 0 0 0.0.0.0:40001 0.0.0.0:*\n' >> "$NFT_STATE/listen"
  # FAKE_NFT_APPLY_FAILS=1：表压根没落地（住宅整段跳跃不通）
  if [[ "${FAKE_NFT_APPLY_FAILS:-0}" == "1" ]]; then
    : > "$NFT_STATE/tables"
    : > "$NFT_STATE/bui"
    return 0
  fi
  printf 'table inet bui\n' > "$NFT_STATE/tables"
  {
    printf 'table inet bui {\n  chain prerouting {\n'
    printf '    udp dport 41000-50000 counter redirect to :40000 comment "hy2 residential hop"\n'
    # FAKE_NFT_RULES=2：兼容段那两条丢了（未刷订阅的 4.0 用户当场断联）
    [[ "${FAKE_NFT_RULES:-4}" == "2" ]] ||
      printf '    udp dport 40001-40007 counter redirect to :40000 comment "hy2 residential 4.0 compat"\n'
    printf '  }\n  chain output {\n'
    printf '    udp dport 41000-50000 counter redirect to :40000 comment "hy2 residential hop (local)"\n'
    [[ "${FAKE_NFT_RULES:-4}" == "2" ]] ||
      printf '    udp dport 40001-40007 counter redirect to :40000 comment "hy2 residential 4.0 compat (local)"\n'
    printf '  }\n}\n'
  } > "$NFT_STATE/bui"
  return 0
}
case "${1:-}" in
  --version) cat "$VERFILE" ;;
  # 收敛时钟到点：把盘上形态真的翻到 4.1（清掉 FAKE_SLOW_SETTLE 免得又推迟一轮）
  __apply41) FAKE_SLOW_SETTLE="" lay_41 ;;
  upgrade)
    if [[ "${2:-}" == "--rollback" ]]; then
      printf '%s\n' "$(cat "$VERFILE.prev")" > "$VERFILE"
      if [[ "${FAKE_KERNEL_STUCK:-0}" != "1" ]]; then write_kernels v1; fi
      lay_40
      printf 'rollback ok\n'
    else
      printf '%s\n' "$(cat "$VERFILE")" > "$VERFILE.prev"
      # --version <x.y.z> 在 $2 $3；后面可能还跟 --manifest-url <url>
      printf '%s\n' "${3:-4.0.1}" > "$VERFILE"
      write_kernels v2
      lay_41
      # FAKE_UPGRADE_DROPS_TOKEN=1：模拟目标版本写出的 state 不带 sub_token（rc11 形状）
      if [[ "${FAKE_UPGRADE_DROPS_TOKEN:-0}" == "1" ]]; then
        printf '{"users":[{"username":"alice"},{"username":"bob"}]}\n' > "$BASEDIR/state.json"
      fi
      # `bui upgrade` 先打印升级计划再动手（commands/upgrade.rs::format_plan）；自建 sing-box
      # 与官方归档同打 1.14.1，所以它那一行的文案是「同版本的新构建」
      printf 'bui          %s → %s\n' "$(cat "$VERFILE.prev")" "$(cat "$VERFILE")"
      # FAKE_SINGBOX_NOT_PLANNED=1：只比版本号的旧判据 ⇒ 计划里根本没有 sing-box 那一行
      [[ "${FAKE_SINGBOX_NOT_PLANNED:-0}" == "1" ]] ||
        printf 'sing-box     1.14.1（同版本的新构建）\n'
      printf 'upgrade ok\n'
    fi
    ;;
  *) printf 'unknown\n'; exit 1 ;;
esac
STUB
# systemctl stub：存在的单元一律 active、NRestarts 固定。LoadState 只对存在的单元报 loaded——
# 生产上槽数远小于 7，`hysteria-residential-2..7` 本来就没有单元文件，演练不能把它们算失败；
# 而 4.1 的形态（hy2-residential.json 在盘上）下连槽 1 的实例单元都该被停掉删掉
cat > "$WORK/bin/systemctl" <<'STUB'
#!/usr/bin/env bash
unit=""
for a in "$@"; do unit="$a"; done
loadstate() {
  # FAKE_CORE_UNIT_GONE=<单元名>：固定六个受管单元里的某一个单元文件不在盘上。
  # `not-found` 的跳过只该给槽位实例，固定单元缺一个必须照判 FAIL
  if [[ -n "${FAKE_CORE_UNIT_GONE:-}" && "$unit" == "${FAKE_CORE_UNIT_GONE}" ]]; then
    printf 'not-found\n'
  elif [[ "$unit" == hysteria-residential-[1-7] ]]; then
    if [[ -f "$BASEDIR/hy2-residential.json" ]]; then
      # 4.1 的形态：槽位实例一个不剩。FAKE_RESI_UNIT_STUCK=1 模拟残留一个（槽 1）
      if [[ "${FAKE_RESI_UNIT_STUCK:-0}" == "1" && "$unit" == "hysteria-residential-1" ]]; then
        printf 'loaded\n'
      else
        printf 'not-found\n'
      fi
    elif [[ "$unit" != "hysteria-residential-1" ]]; then
      # 4.0.x 的形态：只有 state.json 里真有的那个槽（槽 1）有单元文件
      printf 'not-found\n'
    elif [[ "${FAKE_RESI_UNIT_GONE:-0}" == "1" && -f "$VERFILE.prev" ]]; then
      # FAKE_RESI_UNIT_GONE=1：回滚后 4.0.x 的对账没把槽 1 的单元文件写回来
      printf 'not-found\n'
    else
      printf 'loaded\n'
    fi
  else
    printf 'loaded\n'
  fi
}
case "${1:-}" in
  is-active)
    # 单元文件不在盘上时 systemd 报 inactive 并退 3，不是 active
    if [[ "$(loadstate)" == "not-found" ]]; then printf 'inactive\n'; exit 3; fi
    printf 'active\n' ;;
  show)
    case "$*" in
      *LoadState*) loadstate ;;
      *) printf '0\n' ;;
    esac ;;
  *) printf '\n' ;;
esac
STUB
# curl stub：订阅内容 = URL 末段（订阅 token）+ 升级后是否漂移；请求过的 URL 记账，
# 用来验「拼的是 token 不是用户名」。FAKE_404=1 模拟按用户名取时的 404：curl -f 退 22、body 空。
# URL 只从 `-K -` 的 stdin 配置里读（token 进了 argv 就取不到内容 ⇒ 判失败），argv 另记一份
# 进 $CURL_ARGV_LOG，断言凭据没进命令行
cat > "$WORK/bin/curl" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$CURL_ARGV_LOG"
url=""
if [[ "$*" == *"-K -"* ]]; then
  while IFS= read -r line; do
    case "$line" in url\ =\ *) url=${line#url = }; url=${url//\"/} ;; esac
  done
fi
printf '%s\n' "$url" >> "$CURL_LOG"
if [[ "${FAKE_404:-0}" == "1" ]]; then exit 22; fi
body="sub:${url##*/}"
if [[ "${FAKE_DRIFT:-0}" == "1" && -f "$VERFILE.prev" ]]; then body="$body-drifted"; fi
# FAKE_DRIFT_UPGRADE_ONLY=1：只在 4.1 的形态（hy2-residential.json 在盘上）漂移 ⇒ 漂移被
# 升级相位抓住而不是回滚相位，钉住 `compare_subs before after-upgrade` 这条 4.1「零刷新」判据
if [[ "${FAKE_DRIFT_UPGRADE_ONLY:-0}" == "1" && -f "$BASEDIR/hy2-residential.json" ]]; then
  body="$body-drifted"
fi
printf '%s\n' "$body"
STUB
# tar stub：只记录被调用，避免真的打包 /opt
cat > "$WORK/bin/tar" <<'STUB'
#!/usr/bin/env bash
printf 'tar %s\n' "$*" >> "$TAR_LOG"
for a in "$@"; do case "$a" in *.tar.gz) : > "$a" ;; esac; done
STUB
# ss stub：`ss -lnu` 的形状（表头 + 每行第 4 列是 Local Address:Port）
cat > "$WORK/bin/ss" <<'STUB'
#!/usr/bin/env bash
printf 'State Recv-Q Send-Q Local-Address:Port Peer-Address:Port\n'
cat "$NFT_STATE/listen" 2>/dev/null
exit 0
STUB
# sleep stub：演练轮询等对账收敛，测试不真等（口径同 test-build-singbox.sh）。它被 wait_settle
# 每轮调一次，所以顺手当 FAKE_SLOW_SETTLE 的时钟：倒数到 0 就执行 bui 桩留下的翻面动作
cat > "$WORK/bin/sleep" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$1" >> "${SLEEP_LOG:-/dev/null}"
[[ -f "$NFT_STATE/pending" ]] || exit 0
n=$(cat "$NFT_STATE/pending")
if [[ "$n" -le 1 ]]; then
  rm -f "$NFT_STATE/pending"
  bash "$NFT_STATE/converge"
else
  printf '%s\n' "$((n - 1))" > "$NFT_STATE/pending"
fi
exit 0
STUB
# nft stub：只认演练用的两条只读命令。表不在时 `list table` 退 1（真 nft 也这样）
cat > "$WORK/nftbin/nft" <<'STUB'
#!/usr/bin/env bash
case "$*" in
  "list tables") cat "$NFT_STATE/tables" 2>/dev/null ;;
  "list table inet bui")
    [[ -s "$NFT_STATE/bui" ]] || exit 1
    cat "$NFT_STATE/bui" ;;
  *) : ;;
esac
STUB
chmod +x "$WORK/bin"/* "$WORK/nftbin"/*
export PATH="$WORK/bin:$WORK/nftbin:$PATH" VERFILE="$WORK/version" TAR_LOG="$WORK/tar.log" \
       BUI_LOG="$WORK/bui.log" BASEDIR="$WORK/base" CURL_LOG="$WORK/curl.log" \
       CURL_ARGV_LOG="$WORK/curl.argv.log" NFT_STATE="$WORK/nftstate" \
       SLEEP_LOG="$WORK/sleep.log"

reset_env() {
    rm -f "$WORK/version.prev" "$WORK/bui.log" "$WORK/curl.log" "$WORK/curl.argv.log" \
        "$WORK/sleep.log" "$WORK/nftstate/pending" "$WORK/nftstate/converge"
    printf '4.0.0\n' > "$WORK/version"
    write_state "$BOB_TOK"
    # 起点是 4.0.x：两份住宅 yaml、两个监听端口、没有 hy2-residential.json、没有 inet bui 表
    printf 'listen: :40000,41000-50000\n' > "$WORK/base/config-residential.yaml"
    printf 'listen: :40001,41000-50000\n' > "$WORK/base/config-residential-1.yaml"
    rm -f "$WORK/base/hy2-residential.json"
    printf 'UNCONN 0 0 0.0.0.0:40000 0.0.0.0:*\nUNCONN 0 0 0.0.0.0:40001 0.0.0.0:*\n' \
        > "$WORK/nftstate/listen"
    : > "$WORK/nftstate/tables"
    : > "$WORK/nftstate/bui"
    for b in bui hysteria xray caddy; do
        printf 'bin-%s-v1\n' "$b" > "$WORK/base/bin/$b"
    done
    printf '#!/usr/bin/env bash\nprintf %s\n' \
        "'sing-box version 1.14.1 (v1)\\nTags: with_quic,with_utls\\n'" > "$WORK/base/bin/sing-box"
    chmod 755 "$WORK/base/bin/sing-box"
}

run_to() {  # $1 = --to 的目标版本，$2 = --out 目录；$3 起原样追加给演练脚本
    # 解释器写 `$BASH` 的绝对路径而不是 `bash`：下面「缺 nft」/「缺 ss」两条分支把含该工具的
    # 目录整段从 PATH 摘掉，而 usr-merge 的发行版上那往往正是 bash 自己所在的目录（本机
    # `ss` 就在 /usr/bin）⇒ 写 `bash` 会退 127，把守卫用例判成假红
    "$BASH" "$ROOT/scripts/ops/upgrade-drill.sh" --to "$1" \
        --manifest-url http://127.0.0.1:8000/v4.0.1/manifest.json --users alice,bob \
        --out "$2" --base "$WORK/base" --bui "$WORK/bin/bui" --api http://127.0.0.1:8080 \
        "${@:3}"
}
run() { run_to 4.0.1 "$@"; }
# `--units` 覆盖的最自然写法：4.1 期望态的固定六个
U41="b-ui hysteria-server hysteria-residential xray b-ui-relay caddy"
# 「缺 nft」/「缺 ss」那两条分支不能靠「白名单目录里没有它」这个外部事实（`/usr/sbin` 合并进
# `/usr/bin` 的发行版上真 nft 照样在 PATH 里）：逐段摘掉含该可执行文件的目录，其余目录一个不动
path_without() {  # $1 = 可执行文件名
    local tool="$1" d out="" IFS=:
    for d in $PATH; do
        [[ -z "$d" || -x "$d/$tool" ]] && continue
        out="${out:+$out:}$d"
    done
    printf '%s\n' "$out"
}
csv() { awk -F, -v p="$1" -v k="$2" '$1 == p && $2 == k {print $3}' "$3"; }

reset_env
out=$(run "$WORK/ok" 2>&1); rc=$?
assert_eq "0" "$rc" "成功路径退 0"
assert_contains "PASS" "$out" "输出 PASS"
assert_eq "1" "$([[ -f "$WORK/ok/DONE" ]] && echo 1 || echo 0)" "写了 DONE"
assert_contains "verdict=PASS" "$(cat "$WORK/ok/DONE")" "DONE 里写结论"
assert_contains "manifest=http://127.0.0.1:8000/v4.0.1/manifest.json" "$(cat "$WORK/ok/DONE")" "DONE 记下用的是哪份 manifest"
assert_contains "upgrade --version 4.0.1 --manifest-url http://127.0.0.1:8000/v4.0.1/manifest.json" \
    "$(cat "$WORK/bui.log")" "按 C5 传 --version 与 --manifest-url"
assert_contains "upgrade --rollback" "$(cat "$WORK/bui.log")" "回滚用 upgrade --rollback"
assert_eq "phase,key,value" "$(head -1 "$WORK/ok/drill.csv")" "CSV 表头"
assert_eq "3" "$(tail -n +2 "$WORK/ok/drill.csv" | awk -F, '{print $1}' | sort -u | wc -l)" "三个 phase 都记了"
assert_eq "4.0.1" "$(csv after-upgrade version "$WORK/ok/drill.csv")" "升级后版本 = --to"
assert_eq "4.0.0" "$(csv after-rollback version "$WORK/ok/drill.csv")" "回滚后版本复原"
assert_eq "6" "$(awk -F, '$1 == "before" && $2 ~ /^sub:/ {c++} END {print c + 0}' "$WORK/ok/drill.csv")" "2 用户 × 3 种订阅指纹"
assert_eq "5" "$(awk -F, '$1 == "before" && $2 ~ /^sha:bin\// {c++} END {print c + 0}' "$WORK/ok/drill.csv")" "bui + 四内核 = 5 条 sha:bin 行"
assert_eq "1" "$([[ "$(csv before sha:bin/sing-box "$WORK/ok/drill.csv")" \
    != "$(csv after-upgrade sha:bin/sing-box "$WORK/ok/drill.csv")" ]] && echo 1 || echo 0)" \
    "升级后内核 sha 变了（内核随 manifest 升级）"
assert_eq "$(csv before sha:bin/sing-box "$WORK/ok/drill.csv")" \
    "$(csv after-rollback sha:bin/sing-box "$WORK/ok/drill.csv")" \
    "回滚后内核 sha 复原"
# 2026-09-14 裁决：订阅 URL 的末段是 sub_token，按用户名取会 404 ⇒ 演练必须拼 token
assert_contains "/api/sub/$ALICE_TOK" "$(cat "$WORK/curl.log")" "订阅 URL 拼的是 sub_token"
assert_contains "/api/clash/$BOB_TOK" "$(cat "$WORK/curl.log")" "三种订阅都按 token 取"
assert_not_contains "/api/sub/alice" "$(cat "$WORK/curl.log")" "不再按用户名取订阅"
# 末段是凭据，只许经 `-K -` 的 stdin 传：进了 argv 就会被 ps 看见（CLAUDE.md 的硬规矩）
assert_not_contains "/api/" "$(cat "$WORK/curl.argv.log")" "订阅 URL 不进 curl 的 argv"
assert_contains "-K -" "$(cat "$WORK/curl.argv.log")" "URL 经 stdin 的 curl 配置传"
assert_contains "backup-" "$(cat "$WORK/tar.log")" "演练前打了快照"
assert_contains "etc/systemd/system/hysteria-residential.service" "$(cat "$WORK/tar.log")" "快照含六个单元文件（不只 b-ui.service）"
assert_contains "etc/systemd/system/hysteria-residential-7.service" "$(cat "$WORK/tar.log")" "快照连 4.0.x 的槽位实例单元一起收"
assert_contains "--ignore-failed-read" "$(cat "$WORK/tar.log")" "缺失单元不让 tar 整体失败"

# —— 4.1：三个相位都采 V40 名单，「必须 active」的清单才按相位收窄（spec §9.1）——
assert_eq "loaded" "$(csv before load:hysteria-residential-1 "$WORK/ok/drill.csv")" "升级前相位记了 4.0.x 的槽位实例单元"
assert_eq "not-found" "$(csv after-upgrade load:hysteria-residential-1 "$WORK/ok/drill.csv")" \
    "升级相位照样采槽位实例单元（否则察觉不到残留的 4.0.x 实例）"
assert_eq "not-found" "$(csv after-upgrade load:hysteria-residential-3 "$WORK/ok/drill.csv")" \
    "槽序号 ≥ 2 的残留也在采样范围里"
assert_eq "loaded" "$(csv after-rollback load:hysteria-residential-1 "$WORK/ok/drill.csv")" "回滚相位把槽位实例单元记回来"
assert_contains "槽位实例单元 hysteria-residential-3 在 after-rollback 不存在" "$out" "不存在的槽位单元只记 note，不算失败"
# 升级后的断言全靠异步对账，所以是轮询到收敛而不是固定睡几秒
assert_contains "after-upgrade：对账已收敛" "$out" "升级相位等对账收敛后才取指纹"
assert_contains "after-rollback：对账已收敛" "$out" "回滚相位同样等收敛"

# —— 4.1：nft 表与监听端口的相位指纹 ——
assert_eq "absent" "$(csv before nft:table "$WORK/ok/drill.csv")" "4.0.x 上没有 inet bui 表"
assert_eq "present" "$(csv after-upgrade nft:table "$WORK/ok/drill.csv")" "升级后表在"
assert_eq "4" "$(csv after-upgrade nft:rules "$WORK/ok/drill.csv")" "升级后 4 条 redirect（整段 + 兼容段各两 hook）"
assert_eq "absent" "$(csv after-rollback nft:table "$WORK/ok/drill.csv")" "回滚后表消失"
assert_eq "yes" "$(csv after-upgrade listen:udp:40000 "$WORK/ok/drill.csv")" "升级后 :40000 在听"
assert_eq "no" "$(csv after-upgrade listen:udp:40001 "$WORK/ok/drill.csv")" "升级后兼容段端口无人监听（靠 nft REDIRECT）"
assert_eq "no" "$(csv after-upgrade listen:udp:41000 "$WORK/ok/drill.csv")" "升级后跳跃段首端口无人监听"
assert_eq "yes" "$(csv after-rollback listen:udp:40001 "$WORK/ok/drill.csv")" "回滚后按槽端口逐个回到在听"
# 4.1 把每槽一份 yaml 换成一份 json（spec §3.5）
assert_eq "0" "$(csv after-upgrade count:resi-yaml "$WORK/ok/drill.csv")" "升级后 config-residential*.yaml 一份不剩"
assert_eq "2" "$(csv after-rollback count:resi-yaml "$WORK/ok/drill.csv")" "回滚后每槽一份 yaml 写回来"
assert_eq "missing" "$(csv before sha:hy2-residential.json "$WORK/ok/drill.csv")" "4.0.x 上没有 hy2-residential.json"
assert_eq "1" "$([[ "$(csv after-upgrade sha:hy2-residential.json "$WORK/ok/drill.csv")" != missing ]] && echo 1 || echo 0)" \
    "升级后 hy2-residential.json 在盘上"
# 自建 sing-box 的两道闸门（Global Constraints P-A：同版本号装不上 ⇒ 住宅永久崩溃循环）。
# 4.0.x → 4.1 这一跳由旧二进制执行，计划里那一行判不得——只记 note，Tags 才是硬判据
assert_contains "sing-box     1.14.1（同版本的新构建）" "$(cat "$WORK/ok/upgrade.out")" "升级计划单独留了一份"
assert_contains "升级由旧二进制（4.0.0）执行" "$out" "4.0.x 执行升级时计划那一行只记 note"
assert_not_contains "升级计划里没有 sing-box" "$out" "健康的 4.0.x → 4.1 不会被判计划缺 sing-box"
assert_contains "with_v2ray_api" "$out" "核过装上的 sing-box 带 with_v2ray_api"

# —— 缺 nft：4.0.1→4.1 那一跳唯一的机器闸门（bui upgrade 由旧二进制执行，进程内拦不住）——
# PATH 里摘掉 nft 的 stub 与真 nft 所在目录，其余 stub 与系统工具一个不少
reset_env
out=$(PATH="$(path_without nft)" run "$WORK/nonft" 2>&1); rc=$?
assert_eq "2" "$rc" "缺 nft 退 2"
assert_contains "缺少 nft" "$out" "点名缺的是 nft"
assert_contains "apt-get install -y nftables" "$out" "打印安装命令"
assert_eq "0" "$([[ -f "$WORK/nonft/drill.csv" ]] && echo 1 || echo 0)" "一个相位都没进（连 CSV 都没建）"
assert_not_contains "upgrade" "$(cat "$WORK/bui.log" 2>/dev/null)" "没去升级"

# —— 缺 ss：全部 listen 判据都走 `ss -lnu`，iproute2 缺了 `listening()` 一律 false ⇒ 先白等满
# `--settle`（默认 300 秒）再判「住宅入站没起来」，把「没装 iproute2」报成住宅全断 ——
reset_env
out=$(PATH="$(path_without ss)" run "$WORK/noss" 2>&1); rc=$?
assert_eq "2" "$rc" "缺 ss 退 2"
assert_contains "缺少 ss" "$out" "点名缺的是 ss"
assert_contains "apt-get install -y iproute2" "$out" "打印安装命令"
assert_eq "0" "$([[ -f "$WORK/noss/drill.csv" ]] && echo 1 || echo 0)" "一个相位都没进（连 CSV 都没建）"
assert_not_contains "对账仍未收敛" "$out" "不白等满 --settle 才报错"
assert_not_contains "住宅入站" "$out" "不把缺 iproute2 报成住宅入站没起来"
assert_not_contains "upgrade" "$(cat "$WORK/bui.log" 2>/dev/null)" "没去升级"

# —— 回滚后表还在 ⇒ 必判 FAIL（4.0.1 的槽 0 会把整段全吸走，比回归事故更糟，spec §9.1）——
reset_env
out=$(NFT_KEEP_AFTER_ROLLBACK=1 run "$WORK/keepnft" 2>&1); rc=$?
assert_eq "1" "$rc" "回滚后表还在 ⇒ 退 1"
assert_contains "nft 表 after-rollback 的 nft:table：期望 absent，实测 present" "$out" "点名表没删"
assert_contains "first_failure=rollback-nft" "$(cat "$WORK/keepnft/DONE")" "DONE 记 rollback-nft"

# —— 升级后表没落地 ⇒ 住宅整段跳跃不通 ——
reset_env
out=$(FAKE_NFT_APPLY_FAILS=1 run "$WORK/nonftapply" 2>&1); rc=$?
assert_eq "1" "$rc" "升级后表没落地退 1"
assert_contains "nft 表 after-upgrade 的 nft:table：期望 present，实测 absent" "$out" "点名表没落地"
assert_contains "first_failure=upgrade-nft" "$(cat "$WORK/nonftapply/DONE")" "DONE 记 upgrade-nft"

# —— 兼容段那两条规则丢了 ⇒ 未刷订阅的 4.0 用户当场断联 ——
reset_env
out=$(FAKE_NFT_RULES=2 run "$WORK/halfnft" 2>&1); rc=$?
assert_eq "1" "$rc" "规则只落一半退 1"
assert_contains "nft 规则 after-upgrade 的 nft:rules：期望 4，实测 2" "$out" "点名规则条数不对"
assert_contains "first_failure=upgrade-nft" "$(cat "$WORK/halfnft/DONE")" "规则条数不对也记 upgrade-nft"

# —— 计划里没有 sing-box 那一行：按**执行升级的二进制**的代次分档 ——
# 4.0.x 执行时它的 plan_upgrade 只比版本号（P-A 之前的判据），同版本异 sha 不进计划 ⇒ 一次
# 完全健康的 4.0.x → 4.1 也不会打那一行，无条件判 FAIL 会把好的 4.1 回滚掉
reset_env
out=$(FAKE_SINGBOX_NOT_PLANNED=1 run "$WORK/nosbplan40" 2>&1); rc=$?
assert_eq "0" "$rc" "4.0.x 执行升级、计划里没有 sing-box ⇒ 不算失败"
assert_contains "升级由旧二进制（4.0.0）执行" "$out" "说清这一跳为什么判不了计划"
assert_not_contains "upgrade-singbox-plan" "$(cat "$WORK/nosbplan40/DONE")" "不写 upgrade-singbox-plan"
# 4.1.x 执行升级（rc → rc 那种同版本跳）时它已带资产 sha256 判据，计划里必须有那一行。
# 桩里的盘上形态仍是 4.0.x → 4.1 那一套，这条分支只验代次判据本身
reset_env
printf '4.1.0\n' > "$WORK/version"
out=$(FAKE_SINGBOX_NOT_PLANNED=1 run_to 4.1.1 "$WORK/nosbplan41" 2>&1); rc=$?
assert_eq "1" "$rc" "4.1.x 执行升级、计划里没有 sing-box 退 1"
assert_contains "执行升级的 4.1.0 已带资产 sha256 判据" "$out" "点名计划里缺 sing-box"
assert_contains "first_failure=upgrade-singbox-plan" "$(cat "$WORK/nosbplan41/DONE")" "DONE 记 upgrade-singbox-plan"
# 同一跳上计划里有那一行 ⇒ 判据放行（证明它不是恒假）
reset_env
printf '4.1.0\n' > "$WORK/version"
out=$(run_to 4.1.1 "$WORK/sbplan41" 2>&1); rc=$?
assert_eq "0" "$rc" "4.1.x 执行升级、计划里有 sing-box ⇒ 放行"
assert_contains "升级计划里的 sing-box：sing-box     1.14.1" "$out" "打印计划里的 sing-box 那一行"
# 代次判据只认 4.1 这一代次：本脚本的判据按「4.0.x → 4.1 与回滚」写死（4.2 及以后要复用得跟
# `plan_upgrade` 的口径一起改），所以将来的 4.10.x 不能落进 4.1.x 那一档 —— glob 一松成 `4.1*`
# 就会（`4.1.[0-9]*` 与 `4.1.*` 都不会：`.` 在 glob 里是字面量）
reset_env
printf '4.10.0\n' > "$WORK/version"
out=$(FAKE_SINGBOX_NOT_PLANNED=1 run_to 4.10.1 "$WORK/nosbplan410" 2>&1); rc=$?
assert_eq "0" "$rc" "4.10.0 不被当成 4.1.x"
assert_contains "升级由旧二进制（4.10.0）执行" "$out" "4.10.x 走的是「旧二进制」那一档"
assert_not_contains "升级计划里没有 sing-box" "$out" "不拿 4.10.x 当 4.1.x 判计划缺 sing-box"

# —— 装上的 sing-box 没有 with_v2ray_api ⇒ hy2-residential.json 每轮 check 必 FATAL ——
reset_env
out=$(FAKE_SINGBOX_NO_V2RAY_API=1 run "$WORK/nov2ray" 2>&1); rc=$?
assert_eq "1" "$rc" "sing-box 缺 with_v2ray_api 退 1"
assert_contains "没有 with_v2ray_api" "$out" "点名 Tags 不达标"
assert_contains "first_failure=upgrade-singbox-tags" "$(cat "$WORK/nov2ray/DONE")" "DONE 记 upgrade-singbox-tags"

# —— 升级后旧住宅 yaml 还在、新 json 没落（配置转换没发生）——
reset_env
out=$(FAKE_RESI_YAML_KEPT=1 run "$WORK/keptyaml" 2>&1); rc=$?
assert_eq "1" "$rc" "旧住宅 yaml 没删退 1"
assert_contains "旧住宅配置 after-upgrade 的 count:resi-yaml：期望 0，实测 2" "$out" "点名旧 yaml 还剩几份"
assert_contains "hy2-residential.json 不在盘上" "$out" "点名新配置没落盘"
assert_contains "first_failure=upgrade-configs" "$(cat "$WORK/keptyaml/DONE")" "DONE 记 upgrade-configs"

# —— 回滚后槽 1 的端口没回来（apernet 实例没起）——
reset_env
out=$(FAKE_RESI_PORT_DOWN=1 run "$WORK/portdown" 2>&1); rc=$?
assert_eq "1" "$rc" "回滚后按槽端口缺一个退 1"
assert_contains "监听端口未复原 listen:udp:40001" "$out" "点名哪个端口没回来"
assert_contains "first_failure=rollback-listen" "$(cat "$WORK/portdown/DONE")" "DONE 记 rollback-listen"

# —— 回滚后槽 1 的单元文件没写回来：LoadState 是 not-found，all_active 会跳过它，
#    所以必须另有一条按 state.json 的槽位表逐个点名的断言，否则就是假绿 ——
reset_env
out=$(FAKE_RESI_UNIT_GONE=1 run "$WORK/unitgone" 2>&1); rc=$?
assert_eq "1" "$rc" "回滚后槽位单元不存在退 1"
assert_contains "槽 1 的单元 after-rollback 的 unit:hysteria-residential-1：期望 active，实测 inactive" \
    "$out" "点名哪个槽的单元没回来"
assert_contains "first_failure=rollback-resi-units" "$(cat "$WORK/unitgone/DONE")" "DONE 记 rollback-resi-units"

# —— 固定六个受管单元里缺一个 ⇒ 必判 FAIL（`not-found` 的跳过只给槽位实例）——
reset_env
out=$(FAKE_CORE_UNIT_GONE=b-ui-relay run "$WORK/coregone" 2>&1); rc=$?
assert_eq "1" "$rc" "固定受管单元的单元文件不在盘上退 1"
assert_contains "FAIL 单元 b-ui-relay 在 after-upgrade 不是 active（inactive）" "$out" "not-found 的跳过不吞固定单元"
assert_not_contains "槽位实例单元 b-ui-relay" "$out" "不把固定单元当槽位实例跳过"
assert_contains "first_failure=upgrade-units" "$(cat "$WORK/coregone/DONE")" "DONE 记 upgrade-units"

# —— 升级后残留一个 4.0.x 的槽位实例单元（plan T7 的「只有一个实例」）——
reset_env
out=$(FAKE_RESI_UNIT_STUCK=1 run "$WORK/resistuck" 2>&1); rc=$?
assert_eq "1" "$rc" "升级后残留槽位实例单元退 1"
assert_contains "槽 1 的 4.0.x 实例单元 after-upgrade 的 load:hysteria-residential-1：期望 not-found，实测 loaded" \
    "$out" "点名哪个槽的 4.0.x 实例没被停掉"
assert_contains "first_failure=upgrade-resi-units" "$(cat "$WORK/resistuck/DONE")" "DONE 记 upgrade-resi-units"

# —— 升级后残留的 4.0.x 实例还占着 40000+i（兼容段那一片的包会落进旧进程）——
reset_env
out=$(FAKE_RESI_PORT_STUCK=1 run "$WORK/resiport" 2>&1); rc=$?
assert_eq "1" "$rc" "升级后按槽端口还有人听退 1"
assert_contains "监听 after-upgrade 的 listen:udp:40001：期望 no，实测 yes" "$out" "点名兼容段首端口还有人听"
assert_contains "槽 1 的 4.0.x 实例端口 after-upgrade 的 listen:udp:40001" "$out" "按槽序号也逐个点名"
assert_contains "first_failure=upgrade-listen" "$(cat "$WORK/resiport/DONE")" "DONE 记 upgrade-listen"

# —— `--units` 覆盖（4.1 机器上最自然的调试写法就是把固定六个抄进去）：按槽实例的单元名不在
# 采样清单里，按槽的**单元**断言只会 awk 出空值 ⇒ 不许因此把一次健康的升级判成 FAIL ——
reset_env
out=$(run "$WORK/units" --units "$U41" 2>&1); rc=$?
assert_eq "0" "$rc" "--units 覆盖固定六个时成功路径照样退 0"
assert_contains "verdict=PASS" "$(cat "$WORK/units/DONE")" "--units 覆盖不判假 FAIL"
assert_not_contains "upgrade-resi-units" "$out" "不拿没采到的槽位单元判升级相位 FAIL"
assert_not_contains "rollback-resi-units" "$out" "回滚相位同样不判"
assert_contains "--units 覆盖了采样清单" "$out" "说清按槽的单元断言为什么跳过"
# `--units` 只关掉按槽的**单元**断言：按槽端口那组的来源是 state.json 的槽位表，与它无关
reset_env
out=$(FAKE_RESI_PORT_STUCK=1 run "$WORK/unitsport" --units "$U41" 2>&1); rc=$?
assert_eq "1" "$rc" "--units 覆盖时按槽端口断言照判"
assert_contains "槽 1 的 4.0.x 实例端口 after-upgrade 的 listen:udp:40001" "$out" "按槽端口不被 --units 关掉"
assert_contains "first_failure=upgrade-listen" "$(cat "$WORK/unitsport/DONE")" "DONE 记 upgrade-listen"

# —— 对账晚几轮才收敛：这是真机 4.0.1→4.1 的常态（内核要从 GitHub 拉回来），固定睡 5 秒
#    取指纹会把一次健康的升级判成 FAIL ——
reset_env
out=$(FAKE_SLOW_SETTLE=3 run "$WORK/slow" 2>&1); rc=$?
assert_eq "0" "$rc" "对账晚几轮才收敛照样 PASS"
assert_contains "after-upgrade：对账已收敛（等了 15s）" "$out" "等到第三轮才取指纹"
assert_eq "3" "$(wc -l < "$WORK/sleep.log")" "只多等了三轮，收敛就往下走"
assert_eq "present" "$(csv after-upgrade nft:table "$WORK/slow/drill.csv")" "指纹取在收敛之后"

# —— 升级后 :40000 没人听（住宅 HY2 全断）；顺带钉住「等不到收敛就往下判」——
reset_env
out=$(FAKE_40000_DOWN=1 run "$WORK/no40000" 2>&1); rc=$?
assert_eq "1" "$rc" "升级后 :40000 没人听退 1"
assert_contains "对账仍未收敛" "$out" "等到 --settle 超时才往下判"
assert_contains "监听 after-upgrade 的 listen:udp:40000：期望 yes，实测 no" "$out" "点名住宅入站没起来"
assert_contains "first_failure=upgrade-listen" "$(cat "$WORK/no40000/DONE")" "DONE 记 upgrade-listen"

# —— 只在升级相位漂移：钉住 `compare_subs before after-upgrade`（4.1「零刷新」的判据本体）——
reset_env
out=$(FAKE_DRIFT_UPGRADE_ONLY=1 run "$WORK/driftup" 2>&1); rc=$?
assert_eq "1" "$rc" "升级后订阅漂移退 1"
assert_contains "FAIL 订阅漂移 sub:alice:sub：before=" "$out" "点名哪个用户哪种订阅漂了"
assert_contains "first_failure=upgrade-subs" "$(cat "$WORK/driftup/DONE")" "DONE 记 upgrade-subs"

reset_env
out=$(FAKE_DRIFT=1 run "$WORK/drift" 2>&1); rc=$?
assert_eq "1" "$rc" "订阅漂移退 1"
assert_contains "FAIL 订阅漂移" "$out" "指出订阅漂移"
assert_contains "verdict=FAIL" "$(cat "$WORK/drift/DONE")" "DONE 记 FAIL"

# --rollback 只回 bui 不回内核 → 必须 FAIL（spec §11 风险 3：内核版本受 manifest 控制可回退）
reset_env
out=$(FAKE_KERNEL_STUCK=1 run "$WORK/stuck" 2>&1); rc=$?
assert_eq "1" "$rc" "回滚没复原内核退 1"
assert_contains "FAIL 二进制未复原 sha:bin/sing-box" "$out" "点名哪个二进制没复原"
assert_contains "first_failure=rollback-kernels" "$(cat "$WORK/stuck/DONE")" "DONE 记 rollback-kernels"

# 订阅取不到（按用户名取的 404、面板没起来……）必须 FAIL：旧写法两个相位都拿空 body，
# `sha_str ""` 两两相等，`compare_subs` 就判「订阅无漂移」——假绿比失败更坏
reset_env
out=$(FAKE_404=1 run "$WORK/notfound" 2>&1); rc=$?
assert_eq "1" "$rc" "订阅取不到退 1"
assert_contains "FAIL before：取 alice 的 sub 订阅失败或返回空" "$out" "点名哪个用户哪种订阅取不到"
assert_contains "first_failure=sub-fetch:before:alice:sub" "$(cat "$WORK/notfound/DONE")" "DONE 记 sub-fetch"
assert_eq "0" "$(awk -F, '$2 ~ /^sub:/ {c++} END {print c + 0}' "$WORK/notfound/drill.csv")" \
    "取不到就不记 sub: 指纹（不留两个空串给 compare_subs 比出假绿）"

# state.json 里没有 sub_token（rc11 及更早的 state 没有这个字段）⇒ 前置条件不满足：FATAL 退 2，
# 不能报成订阅漂移或取订阅失败（在 rc11 上跑 before 相位看起来会像升级坏了），也不能静默跳过这个用户
reset_env
write_state ""
out=$(run "$WORK/notok" 2>&1); rc=$?
assert_eq "2" "$rc" "缺 sub_token 退 2"
assert_contains "FATAL before：state.json 里 bob 没有 sub_token" "$out" "点名哪个相位、谁没有 token"
assert_contains "本演练要求升级前后两端都是 rc12 及以上（该相位的 state.json 没有订阅 token）" "$out" "说清适用范围"
assert_not_contains "订阅漂移" "$out" "不报成订阅漂移"
assert_not_contains "订阅失败" "$out" "不报成取订阅失败"
assert_contains "verdict=FATAL reason=sub-token-missing:before:bob" "$(cat "$WORK/notok/DONE")" "DONE 记 FATAL 与原因"
assert_eq "0" "$([[ -s "$WORK/curl.log" ]] && echo 1 || echo 0)" "一条订阅都没取"
assert_not_contains "upgrade" "$(cat "$WORK/bui.log")" "没去升级"

# 升级之后的相位没有 sub_token（目标版本的 state 不带这个字段）同样 FATAL，并说明回滚没做
reset_env
out=$(FAKE_UPGRADE_DROPS_TOKEN=1 run "$WORK/droptok" 2>&1); rc=$?
assert_eq "2" "$rc" "升级后缺 sub_token 退 2"
assert_contains "FATAL after-upgrade：state.json 里 alice 没有 sub_token" "$out" "点名 after-upgrade 相位"
assert_contains "（该相位的 state.json 没有订阅 token）；升级已执行、回滚未执行" "$out" "提示回滚没做"
assert_not_contains "订阅漂移" "$out" "升级后缺 token 也不报成订阅漂移"
assert_contains "verdict=FATAL reason=sub-token-missing:after-upgrade:alice" "$(cat "$WORK/droptok/DONE")" "DONE 记 after-upgrade 的 FATAL"

# 缺 --users 直接退 2（别在生产上跑出一份没有订阅指纹的空演练）
out=$(bash "$ROOT/scripts/ops/upgrade-drill.sh" --to 4.0.1 --out "$WORK/nouser" 2>&1); rc=$?
assert_eq "2" "$rc" "缺 --users 退 2"
# --settle 只收整数秒（写错了会让「等收敛」变成不等）。这一条给全套参数——不然退 2 会是
# 「默认 --base 下没有 state.json」造成的，校验删掉也照样退 2
reset_env
out=$(bash "$ROOT/scripts/ops/upgrade-drill.sh" --users alice --settle 5s \
    --out "$WORK/badsettle" --base "$WORK/base" --bui "$WORK/bin/bui" 2>&1); rc=$?
assert_eq "2" "$rc" "--settle 不是整数秒退 2"
assert_contains "--settle <秒>" "$out" "用法里列出 --settle"
assert_eq "0" "$([[ -f "$WORK/badsettle/drill.csv" ]] && echo 1 || echo 0)" "--settle 写错就一个相位都不进"
finish
