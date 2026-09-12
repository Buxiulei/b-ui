#!/usr/bin/env bash
# v3 快照/恢复：全 stub（tar / systemctl / crontab 都记日志），零网络、无需 root。
set -uo pipefail
LC_ALL=C
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
# shellcheck source=/dev/null
. "$HERE/lib.sh"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/bin" "$WORK/base" "$WORK/out" "$WORK/units/caddy.service.d" "$WORK/sbin"
printf 'users\n' > "$WORK/base/users.json"
printf 'v3 cli\n' > "$WORK/base/b-ui-cli.sh"
# v3 的 CLI 入口现状：/usr/local/bin/b-ui → /opt/b-ui/b-ui-cli.sh，没有 /usr/local/bin/bui
ln -sfn "$WORK/base/b-ui-cli.sh" "$WORK/sbin/b-ui"
# 造一台「典型 v3 机器」的单元目录：五个服务单元 + 两组 timer（hy2-watchdog、b-ui-resi-health）
# + caddy 的 drop-in 目录。故意不放 caddy.service（v3 用 apt 包的 /lib 单元）、
# 也不放 b-ui-cert-sync.*（没配域名的机器就没有它）。
for f in hysteria-server hysteria-residential xray b-ui-admin b-ui-relay; do
    : > "$WORK/units/$f.service"
done
for f in hy2-watchdog b-ui-resi-health; do
    : > "$WORK/units/$f.service"
    : > "$WORK/units/$f.timer"
done
printf '[Service]\nReadWritePaths=/var/log/caddy\n' > "$WORK/units/caddy.service.d/override.conf"

# tar stub：记录参数；-czf 造出空归档并把「-C / 之后的成员」写进 TAR_LIST；-tzf 原样回放
cat > "$WORK/bin/tar" <<'STUB'
#!/usr/bin/env bash
printf 'tar %s\n' "$*" >> "$TAR_LOG"
case "$*" in
  *-tzf*) cat "$TAR_LIST" ;;
  *-czf*)
    for a in "$@"; do case "$a" in *.tar.gz) : > "$a" ;; esac; done
    seen=0
    for a in "$@"; do
      [[ "$seen" -eq 1 ]] && printf '%s\n' "$a"
      [[ "$a" == "/" ]] && seen=1
    done > "$TAR_LIST"
    ;;
esac
exit 0
STUB
# systemctl stub：记录动作；is-active 按 FAKE_DEAD 决定死活
cat > "$WORK/bin/systemctl" <<'STUB'
#!/usr/bin/env bash
printf 'systemctl %s\n' "$*" >> "$SC_LOG"
case "${1:-}" in
  is-active)
    if [[ "${FAKE_DEAD:-}" == "$2" ]]; then printf 'failed\n'; exit 3; fi
    printf 'active\n' ;;
  is-enabled) printf 'enabled\n' ;;
  *) : ;;
esac
STUB
# crontab stub：当前 crontab 存在 CRON_CUR 文件里（测试可随时改写，模拟切换后的真实机器）；
# -l 原样回放（空则退 1，跟真 crontab 一致）；FAKE_CRON_EMPTY=1 装作空；
# `crontab -` 收进 CRON_IN 并同步进 CRON_CUR（这样二次 restore 看到的就是上次写进去的）
cat > "$WORK/bin/crontab" <<'STUB'
#!/usr/bin/env bash
if [[ "${1:-}" == "-" ]]; then cat > "$CRON_IN"; cp "$CRON_IN" "$CRON_CUR"; exit 0; fi
[[ "${FAKE_CRON_EMPTY:-0}" == "1" ]] && exit 0
[[ -s "$CRON_CUR" ]] || exit 1
cat "$CRON_CUR"
STUB
chmod +x "$WORK/bin"/*
export PATH="$WORK/bin:$PATH" TAR_LOG="$WORK/tar.log" SC_LOG="$WORK/sc.log" \
    TAR_LIST="$WORK/tar.list" CRON_IN="$WORK/cron.in" CRON_CUR="$WORK/cron.cur"
# 打快照时机器上的 v3 三行 cron（update.sh auto / update.sh kernel / cert-check.sh）
cat > "$CRON_CUR" <<'CRON'
0 */6 * * * /opt/b-ui/update.sh auto
0 */12 * * * /opt/b-ui/update.sh kernel
17 3 * * * /opt/b-ui/cert-check.sh
CRON

CUT="$ROOT/scripts/ops/v3-cutover.sh"

# ---- snapshot ----
out=$(bash "$CUT" snapshot --out "$WORK/out" --base "$WORK/base" --unit-dir "$WORK/units" --link-dir "$WORK/sbin" 2>&1); rc=$?
assert_eq "0" "$rc" "snapshot 退 0"
assert_eq "1" "$(find "$WORK/out" -name 'v3-*.tar.gz' | wc -l)" "产出一个快照归档"
assert_eq "1" "$(find "$WORK/out" -name 'v3-*.manifest' | wc -l)" "产出一份内容清单"
man=$(find "$WORK/out" -name 'v3-*.manifest' | head -1)
assert_contains "hysteria-residential.service" "$(cat "$man")" "清单含 v3 服务单元"
assert_contains "hy2-watchdog.timer enabled=" "$(cat "$man")" "清单含 v3 定时器状态"
assert_contains "update.sh auto" "$(cat "$man")" "清单含 v3 的 cron 行"
assert_contains "cert-check.sh" "$(cat "$man")" "清单含 v3 的第三条 cron 行"
assert_contains "# clilinks" "$(cat "$man")" "清单记下 CLI 入口符号链接段"
assert_contains "$WORK/sbin/b-ui $WORK/base/b-ui-cli.sh" "$(cat "$man")" "记下 b-ui 当时指向 v3 的 b-ui-cli.sh"
assert_contains "$WORK/sbin/bui absent" "$(cat "$man")" "v3 没有 bui 这个入口，记 absent"
assert_contains "# unitfiles" "$(cat "$man")" "清单记下「快照里有哪些单元文件」（restore 反推 v4 独有单元要用）"
assert_contains "$WORK/units/hy2-watchdog.service" "$(cat "$man")" "unitfiles 段含 timer 的同名 service"
tl=$(cat "$WORK/tar.list")
assert_contains "units/b-ui-admin.service" "$tl" "tar 打包了 v3 面板单元"
assert_contains "units/hy2-watchdog.service" "$tl" "tar 打包了 hy2-watchdog 的 service（只打 timer 会恢复不起来）"
assert_contains "units/hy2-watchdog.timer" "$tl" "tar 打包了 hy2-watchdog 的 timer"
assert_contains "units/b-ui-resi-health.service" "$tl" "tar 打包了 b-ui-resi-health 的 service"
assert_contains "units/b-ui-resi-health.timer" "$tl" "tar 打包了 b-ui-resi-health 的 timer"
assert_contains "units/caddy.service.d" "$tl" "tar 打包了 drop-in 目录（v3 的 caddy 靠 override.conf 才能写日志）"
assert_eq "0" "$(grep -cx "${WORK#/}/units/caddy.service" "$WORK/tar.list")" "v3 没有 /etc/systemd/system/caddy.service（apt 包在 /lib），清单不虚报"
assert_eq "0" "$(grep -cx "${WORK#/}/units/b-ui-cert-sync.timer" "$WORK/tar.list")" "本机没有 b-ui-cert-sync.timer，清单不虚报"
assert_contains "--ignore-failed-read" "$(cat "$WORK/tar.log")" "缺失单元不让 tar 整体失败"
assert_contains "v3-cutover.sh restore --from" "$out" "打印恢复命令"

# ---- snapshot --list / --prune ----
out=$(bash "$CUT" snapshot --list --out "$WORK/out" 2>&1)
assert_contains "v3-" "$out" "--list 列出快照"
assert_contains "天" "$out" "--list 打印年龄"
: > "$WORK/out/v3-19700101T000000Z.tar.gz"
touch -d '40 days ago' "$WORK/out/v3-19700101T000000Z.tar.gz"
out=$(bash "$CUT" snapshot --prune --out "$WORK/out" 2>&1)
assert_contains "v3-19700101T000000Z.tar.gz" "$out" "--prune 点名被删的老快照"
assert_eq "1" "$(find "$WORK/out" -name 'v3-*.tar.gz' | wc -l)" "--prune 只删超过 30 天的，新快照留着"

# ---- restore（成功路径）----
snap=$(find "$WORK/out" -name 'v3-*.tar.gz' | head -1)
: > "$WORK/units/b-ui.service"      # v4 装机时写的新单元
: > "$WORK/units/caddy.service"     # v4 自己的 caddy 单元（ExecStart 指向 /opt/b-ui/bin/caddy）
# v4 install 把 b-ui 改指 <base>/bin/bui 并另建 bui（crates/bui/src/paths.rs CLI_LINKS）
ln -sfn "$WORK/base/bin/bui" "$WORK/sbin/b-ui"
ln -sfn "$WORK/base/bin/bui" "$WORK/sbin/bui"
: > "$SC_LOG"
out=$(bash "$CUT" restore --from "$snap" --base "$WORK/base" --unit-dir "$WORK/units" 2>&1); rc=$?
assert_eq "0" "$rc" "restore 全部 active 时退 0"
sc=$(cat "$SC_LOG")
assert_contains "stop b-ui" "$sc" "先停 v4 的 b-ui"
assert_contains "daemon-reload" "$sc" "解包后 daemon-reload"
assert_contains "start hysteria-server" "$sc" "拉起 v3 服务单元"
assert_contains "start hy2-watchdog.timer" "$sc" "拉起快照里有的 v3 定时器"
assert_not_contains "b-ui-cert-sync" "$sc" "快照里没有的定时器不去 enable/start"
assert_eq "0" "$([[ -e "$WORK/base" ]] && echo 1 || echo 0)" "解包前把 v4 的 base 整个挪走（不与 v3 文件混住）"
assert_eq "1" "$(find "$(dirname "$WORK/base")" -maxdepth 1 -name 'base.v4-*' | wc -l)" "挪走的 v4 目录留在旁边"
assert_contains "已挪到" "$out" "打印 v4 残留目录位置"
assert_eq "0" "$([[ -e "$WORK/units/b-ui.service" ]] && echo 1 || echo 0)" "删掉 v4 独有的 b-ui.service"
assert_eq "0" "$([[ -e "$WORK/units/caddy.service" ]] && echo 1 || echo 0)" "删掉 v4 独有的 caddy.service（否则 caddy 仍跑 v4 单元却 is-active 假绿）"
assert_eq "1" "$([[ -e "$WORK/units/hysteria-server.service" ]] && echo 1 || echo 0)" "快照里有的单元文件不动"
# 顺序：stop b-ui 在 start hysteria-server 之前；删 v4 单元在 daemon-reload 之前
stop_ln=$(grep -n 'stop b-ui' "$SC_LOG" | head -1 | cut -d: -f1)
start_ln=$(grep -n 'start hysteria-server' "$SC_LOG" | head -1 | cut -d: -f1)
assert_eq "1" "$([[ "$stop_ln" -lt "$start_ln" ]] && echo 1 || echo 0)" "先停 v4 再起 v3"
rm_ln=$(printf '%s\n' "$out" | grep -n '删除 v4 独有单元' | head -1 | cut -d: -f1)
rl_ln=$(printf '%s\n' "$out" | grep -n 'daemon-reload' | head -1 | cut -d: -f1)
assert_eq "1" "$([[ "$rm_ln" -lt "$rl_ln" ]] && echo 1 || echo 0)" "先删 v4 单元再 daemon-reload"
assert_contains "无需回灌" "$out" "现有 crontab 已含清单里的行时不重复写"
assert_eq "0" "$([[ -e "$WORK/cron.in" ]] && echo 1 || echo 0)" "无需回灌时根本不动 crontab"
assert_eq "$WORK/base/b-ui-cli.sh" "$(readlink "$WORK/sbin/b-ui")" "b-ui 从 v4 的 bin/bui 改回 v3 的 b-ui-cli.sh（不然 sudo b-ui 悬空）"
assert_eq "0" "$([[ -L "$WORK/sbin/bui" ]] && echo 1 || echo 0)" "快照记 absent 的 bui 被删掉"
assert_contains "重建 $WORK/sbin/b-ui" "$out" "点名重建的符号链接"
assert_contains "删除 $WORK/sbin/bui" "$out" "点名删掉的 v4 符号链接"
assert_contains "恢复完成" "$out" "打印恢复结论"

# ---- restore：crontab 为空则按清单回灌 ----
mkdir -p "$WORK/base"
out=$(FAKE_CRON_EMPTY=1 bash "$CUT" restore --from "$snap" --base "$WORK/base" --unit-dir "$WORK/units" 2>&1); rc=$?
assert_eq "0" "$rc" "回灌 cron 后仍退 0"
assert_contains "update.sh auto" "$(cat "$WORK/cron.in")" "把清单里的 cron 行灌回 crontab"
assert_eq "3" "$(wc -l < "$WORK/cron.in")" "只灌三条 v3 cron 行，不带清单的注释头"

# ---- restore：crontab 非空（import-v3 只删含 /opt/b-ui/ 的行，别的项目的行留着）→ 幂等合并 ----
cat > "$CRON_CUR" <<'CRON'
0 3 * * * /opt/other/backup.sh
*/5 * * * * /usr/local/bin/othercheck
CRON
rm -f "$WORK/cron.in"
mkdir -p "$WORK/base"
out=$(bash "$CUT" restore --from "$snap" --base "$WORK/base" --unit-dir "$WORK/units" 2>&1); rc=$?
assert_eq "0" "$rc" "非空 crontab 下合并回灌仍退 0"
cronin=$(cat "$WORK/cron.in")
assert_contains "/opt/b-ui/update.sh auto" "$cronin" "补回 v3 的 update.sh auto"
assert_contains "/opt/b-ui/update.sh kernel" "$cronin" "补回 v3 的 update.sh kernel"
assert_contains "/opt/b-ui/cert-check.sh" "$cronin" "补回 v3 的 cert-check.sh"
assert_contains "/opt/other/backup.sh" "$cronin" "别的项目的 cron 行原样保留"
assert_contains "/usr/local/bin/othercheck" "$cronin" "别的项目的第二条 cron 行原样保留"
assert_eq "5" "$(wc -l < "$WORK/cron.in")" "合并后 2 行别人的 + 3 行 v3，不重不漏"
assert_contains "补齐 3 行 v3 cron" "$out" "报出补了几行"

# ---- restore：二次恢复幂等（cron 不重复、符号链接不重建）----
rm -f "$WORK/cron.in"
mkdir -p "$WORK/base"
out=$(bash "$CUT" restore --from "$snap" --base "$WORK/base" --unit-dir "$WORK/units" 2>&1); rc=$?
assert_eq "0" "$rc" "二次 restore 退 0"
assert_contains "都已在 crontab 中，无需回灌" "$out" "二次 restore 不再写 crontab"
assert_eq "0" "$([[ -e "$WORK/cron.in" ]] && echo 1 || echo 0)" "二次 restore 完全不调 crontab -"
assert_eq "5" "$(wc -l < "$CRON_CUR")" "crontab 仍是 5 行，没被灌成重复行"
assert_contains "已经指对，跳过" "$out" "符号链接已正确则不重建"
assert_eq "$WORK/base/b-ui-cli.sh" "$(readlink "$WORK/sbin/b-ui")" "二次 restore 后 b-ui 仍指 v3"
assert_eq "0" "$([[ -L "$WORK/sbin/bui" ]] && echo 1 || echo 0)" "二次 restore 不把 absent 的 bui 造回来"

# ---- restore：服务或定时器起不来 → 退 1 并点名 ----
mkdir -p "$WORK/base"
out=$(FAKE_DEAD=xray bash "$CUT" restore --from "$snap" --base "$WORK/base" --unit-dir "$WORK/units" 2>&1); rc=$?
assert_eq "1" "$rc" "有服务没起来退 1"
assert_contains "FAIL xray" "$out" "点名没起来的服务"
mkdir -p "$WORK/base"
out=$(FAKE_DEAD=hy2-watchdog.timer bash "$CUT" restore --from "$snap" --base "$WORK/base" --unit-dir "$WORK/units" 2>&1); rc=$?
assert_eq "1" "$rc" "定时器没起来也退 1（定时器算进核对，不然 timer 没起来还假绿）"
assert_contains "FAIL hy2-watchdog.timer" "$out" "点名没起来的定时器"

# ---- restore：快照旁边没有清单 → 只警告，不回灌 cron ----
cp "$snap" "$WORK/out/lonely.tar.gz"
mkdir -p "$WORK/base"
out=$(FAKE_CRON_EMPTY=1 bash "$CUT" restore --from "$WORK/out/lonely.tar.gz" --base "$WORK/base" --unit-dir "$WORK/units" 2>&1); rc=$?
assert_eq "0" "$rc" "没有清单仍能恢复单元"
assert_contains "没有同名清单" "$out" "明确告知跳过 cron 回灌"

# ---- 用法错误 ----
out=$(bash "$CUT" restore --from "$WORK/out/nope.tar.gz" 2>&1); rc=$?
assert_eq "2" "$rc" "快照不存在退 2"
out=$(bash "$CUT" 2>&1); rc=$?
assert_eq "2" "$rc" "缺子命令退 2"
finish
