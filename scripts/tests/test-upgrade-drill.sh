#!/usr/bin/env bash
# 演练脚本：成功路径退 0；订阅在升级后漂移时退 1。全 stub，零网络。
set -uo pipefail
LC_ALL=C
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
# shellcheck source=/dev/null
. "$HERE/lib.sh"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/bin" "$WORK/base/bin"
printf 'state\n' > "$WORK/base/state.json"
printf 'listen: :10000,20000-30000\n' > "$WORK/base/config.yaml"
printf '{}\n' > "$WORK/base/xray-config.json"
printf '4.0.0\n' > "$WORK/version"
for b in bui hysteria xray sing-box caddy; do
    printf 'bin-%s-v1\n' "$b" > "$WORK/base/bin/$b"
done

# bui stub：记录 argv；--version 读文件；upgrade 改版本并换掉内核二进制（模拟「内核随 manifest 升级」）；
# --rollback 把版本与内核都改回去，除非 FAKE_KERNEL_STUCK=1（模拟只回 bui 不回内核的实现）
cat > "$WORK/bin/bui" <<'STUB'
#!/usr/bin/env bash
printf 'bui %s\n' "$*" >> "$BUI_LOG"
kernels() { printf 'bui hysteria xray sing-box caddy\n'; }
case "${1:-}" in
  --version) cat "$VERFILE" ;;
  upgrade)
    if [[ "${2:-}" == "--rollback" ]]; then
      printf '%s\n' "$(cat "$VERFILE.prev")" > "$VERFILE"
      if [[ "${FAKE_KERNEL_STUCK:-0}" != "1" ]]; then
        for b in $(kernels); do printf 'bin-%s-v1\n' "$b" > "$BASEDIR/bin/$b"; done
      fi
    else
      printf '%s\n' "$(cat "$VERFILE")" > "$VERFILE.prev"
      # --version <x.y.z> 在 $2 $3；后面可能还跟 --manifest-url <url>
      printf '%s\n' "${3:-4.0.1}" > "$VERFILE"
      for b in $(kernels); do printf 'bin-%s-v2\n' "$b" > "$BASEDIR/bin/$b"; done
    fi
    printf 'upgrade ok\n'
    ;;
  *) printf 'unknown\n'; exit 1 ;;
esac
STUB
# systemctl stub：一律 active、NRestarts 固定
cat > "$WORK/bin/systemctl" <<'STUB'
#!/usr/bin/env bash
case "${1:-}" in
  is-active) printf 'active\n' ;;
  show) printf '0\n' ;;
  *) printf '\n' ;;
esac
STUB
# curl stub：订阅内容 = 用户名 + 升级后是否漂移
cat > "$WORK/bin/curl" <<'STUB'
#!/usr/bin/env bash
url=""; for a in "$@"; do case "$a" in http*) url="$a" ;; esac; done
body="sub:${url##*/}"
if [[ "${FAKE_DRIFT:-0}" == "1" && -f "$VERFILE.prev" ]]; then body="$body-drifted"; fi
printf '%s\n' "$body"
STUB
# tar stub：只记录被调用，避免真的打包 /opt
cat > "$WORK/bin/tar" <<'STUB'
#!/usr/bin/env bash
printf 'tar %s\n' "$*" >> "$TAR_LOG"
for a in "$@"; do case "$a" in *.tar.gz) : > "$a" ;; esac; done
STUB
chmod +x "$WORK/bin"/*
export PATH="$WORK/bin:$PATH" VERFILE="$WORK/version" TAR_LOG="$WORK/tar.log" \
       BUI_LOG="$WORK/bui.log" BASEDIR="$WORK/base"

reset_env() {
    rm -f "$WORK/version.prev" "$WORK/bui.log"
    printf '4.0.0\n' > "$WORK/version"
    for b in bui hysteria xray sing-box caddy; do
        printf 'bin-%s-v1\n' "$b" > "$WORK/base/bin/$b"
    done
}

run() {
    bash "$ROOT/scripts/ops/upgrade-drill.sh" --to 4.0.1 \
        --manifest-url http://127.0.0.1:8000/v4.0.1/manifest.json --users alice,bob \
        --out "$1" --base "$WORK/base" --bui "$WORK/bin/bui" --api http://127.0.0.1:8080
}

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
assert_eq "4.0.1" "$(awk -F, '$1 == "after-upgrade" && $2 == "version" {print $3}' "$WORK/ok/drill.csv")" "升级后版本 = --to"
assert_eq "4.0.0" "$(awk -F, '$1 == "after-rollback" && $2 == "version" {print $3}' "$WORK/ok/drill.csv")" "回滚后版本复原"
assert_eq "6" "$(awk -F, '$1 == "before" && $2 ~ /^sub:/ {c++} END {print c + 0}' "$WORK/ok/drill.csv")" "2 用户 × 3 种订阅指纹"
assert_eq "5" "$(awk -F, '$1 == "before" && $2 ~ /^sha:bin\// {c++} END {print c + 0}' "$WORK/ok/drill.csv")" "bui + 四内核 = 5 条 sha:bin 行"
assert_eq "1" "$([[ "$(awk -F, '$1 == "before" && $2 == "sha:bin/sing-box" {print $3}' "$WORK/ok/drill.csv")" \
    != "$(awk -F, '$1 == "after-upgrade" && $2 == "sha:bin/sing-box" {print $3}' "$WORK/ok/drill.csv")" ]] && echo 1 || echo 0)" \
    "升级后内核 sha 变了（内核随 manifest 升级）"
assert_eq "$(awk -F, '$1 == "before" && $2 == "sha:bin/sing-box" {print $3}' "$WORK/ok/drill.csv")" \
    "$(awk -F, '$1 == "after-rollback" && $2 == "sha:bin/sing-box" {print $3}' "$WORK/ok/drill.csv")" \
    "回滚后内核 sha 复原"
assert_contains "backup-" "$(cat "$WORK/tar.log")" "演练前打了快照"
assert_contains "etc/systemd/system/hysteria-residential.service" "$(cat "$WORK/tar.log")" "快照含六个单元文件（不只 b-ui.service）"
assert_contains "--ignore-failed-read" "$(cat "$WORK/tar.log")" "缺失单元不让 tar 整体失败"

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

# 缺 --users 直接退 2（别在生产上跑出一份没有订阅指纹的空演练）
out=$(bash "$ROOT/scripts/ops/upgrade-drill.sh" --to 4.0.1 --out "$WORK/nouser" 2>&1); rc=$?
assert_eq "2" "$rc" "缺 --users 退 2"
finish
