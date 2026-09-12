#!/usr/bin/env bash
# v3 → v4 切换的保险绳（spec §9「bwg-tizi 上线」）：
#   snapshot         打快照（/opt/b-ui + v3 单元/定时器/drop-in + cron + enable 状态），保留 30 天
#   snapshot --list  列出现有快照与年龄
#   snapshot --prune 删掉 30 天以上的快照与清单
#   restore          恢复 v3：停 v4 → 挪走 /opt/b-ui → 解包 → 删 v4 独有单元 → daemon-reload
#                    → 起 v3 单元与定时器 → 重建 CLI 符号链接 → 合并回灌 cron → 核对 active
# 生产用法（root）：
#   bash /opt/b-ui/ops/v3-cutover.sh snapshot
#   bash /opt/b-ui/ops/v3-cutover.sh restore --from /var/backups/b-ui/v3-20260918T020000Z.tar.gz
# 不用 set -e：恢复过程中单个 systemctl 失败要继续走完并在末尾汇总。
set -uo pipefail
# 快照归档与清单含 users.json（hy2 密码）、residential-proxy.json（上游凭据）、
# reality-keys.json，而 /var/backups 通常 755：按总纲「凭据不落宽权限」一律 0600 落盘。
# root 下 tar -x 默认 --preserve-permissions，umask 不影响解包出来的文件模式。
umask 077
LC_ALL=C

BASE="/opt/b-ui"
OUT="/var/backups/b-ui"
UNITDIR="/etc/systemd/system"
FROM=""
LIST=0
PRUNE=0
KEEP_DAYS=30
# v3 的常驻服务（CLAUDE.md「Config files on the server」）——恢复后逐个核对 is-active
V3_UNITS="hysteria-server hysteria-residential xray b-ui-admin b-ui-relay caddy"
# v3 的三组 oneshot + timer（server/core.sh:1008 hy2-watchdog、:1095 b-ui-cert-sync、
# server/update.sh:1158 b-ui-resi-health）。每组两个文件都要进快照：只打 .timer 的话，
# 恢复后 timer 找不到同名 service，起不来而 .service 的 is-active 又查不到，会假绿。
V3_TIMER_UNITS="hy2-watchdog b-ui-cert-sync b-ui-resi-health"
# v4 独有、v3 不该有的单元文件：v3 的 caddy 是 apt 包的 /lib/systemd/system/caddy.service
# 加 /etc/systemd/system/caddy.service.d/override.conf（server/core.sh:886），v4 自己写
# /etc/systemd/system/caddy.service（ExecStart /opt/b-ui/bin/caddy --config /opt/b-ui/Caddyfile）。
# 恢复前不删掉它，caddy 会继续跑 v4 单元、反代到 v4 端口，而 is-active 仍是 active。
V4_ONLY_CANDIDATES="b-ui.service caddy.service"
# v4 新增的常驻单元（总纲 C3），恢复 v3 前必须先停掉（它会把配置对账回 v4 期望态）
V4_UNITS="b-ui"
# CLI 入口符号链接所在目录与两个名字：v3 的 /usr/local/bin/b-ui 指向 /opt/b-ui/b-ui-cli.sh，
# v4 install 把它改指 <base>/bin/bui 并另建 /usr/local/bin/bui（crates/bui/src/paths.rs 的
# CLI_LINKS）。归档里没有 /usr/local/bin，只恢复 /opt/b-ui 的话 b-ui 会指着 v4 的路径悬空、
# `sudo b-ui` 直接报错，所以快照逐条记下两个路径当时的指向（不存在记 absent），restore 照单重建。
LINKDIR="/usr/local/bin"
CLI_LINK_NAMES="b-ui bui"

usage() {
    printf '用法：\n  %s snapshot [--out <dir>] [--base <dir>] [--unit-dir <dir>] [--link-dir <dir>] [--list] [--prune]\n  %s restore --from <v3-*.tar.gz> [--base <dir>] [--unit-dir <dir>]\n' "$0" "$0" >&2
    exit 2
}

log() { printf '%s %s\n' "$(date -u +%FT%TZ)" "$1"; }

# v3 机器上「可能存在」的单元文件与 drop-in 目录；只把真实存在的收进来（快照与清单都按实况）
v3_unit_files() {
    local u d
    for u in $V3_UNITS; do printf '%s\n' "$UNITDIR/$u.service"; done
    for u in $V3_TIMER_UNITS; do printf '%s\n%s\n' "$UNITDIR/$u.service" "$UNITDIR/$u.timer"; done
    while IFS= read -r d; do printf '%s\n' "$d"; done < <(find "$UNITDIR" -maxdepth 1 -name '*.service.d' -type d 2>/dev/null | sort)
}

do_snapshot() {
    local ts snap man files=() present=() f u t
    if [[ "$LIST" -eq 1 ]]; then
        [[ -d "$OUT" ]] || { printf '还没有任何快照：%s 不存在\n' "$OUT" >&2; exit 2; }
        while IFS= read -r f; do
            printf '%s  %s 天前\n' "$f" "$(( ( $(date +%s) - $(stat -c %Y "$f") ) / 86400 ))"
        done < <(find "$OUT" -maxdepth 1 -name 'v3-*.tar.gz' | sort)
        return 0
    fi
    if [[ "$PRUNE" -eq 1 ]]; then
        [[ -d "$OUT" ]] || { printf '还没有任何快照：%s 不存在\n' "$OUT" >&2; exit 2; }
        while IFS= read -r f; do
            log "删除超过 $KEEP_DAYS 天的快照 $f"
            rm -f "$f"
        done < <(find "$OUT" -maxdepth 1 \( -name 'v3-*.tar.gz' -o -name 'v3-*.manifest' \) -mtime "+$((KEEP_DAYS - 1))" | sort)
        return 0
    fi
    install -d -m 700 "$OUT" || exit 2
    ts=$(date -u +%Y%m%dT%H%M%SZ)
    snap="$OUT/v3-$ts.tar.gz"
    man="$OUT/v3-$ts.manifest"
    while IFS= read -r f; do [[ -e "$f" ]] && present+=("$f"); done < <(v3_unit_files)
    {
        printf '# v3 快照清单 %s\n' "$ts"
        printf '# base\n%s\n' "$BASE"
        printf '# units\n'
        for u in $V3_UNITS; do
            printf '%s.service enabled=%s active=%s\n' "$u" "$(systemctl is-enabled "$u" 2>/dev/null)" "$(systemctl is-active "$u" 2>/dev/null)"
        done
        printf '# timers\n'
        for u in $V3_TIMER_UNITS; do
            printf '%s.timer enabled=%s active=%s\n' "$u" "$(systemctl is-enabled "$u.timer" 2>/dev/null)" "$(systemctl is-active "$u.timer" 2>/dev/null)"
        done
        printf '# unitfiles\n'
        for f in ${present[@]+"${present[@]}"}; do printf '%s\n' "$f"; done
        printf '# clilinks\n'
        for u in $CLI_LINK_NAMES; do
            t=$(readlink "$LINKDIR/$u" 2>/dev/null)
            printf '%s %s\n' "$LINKDIR/$u" "${t:-absent}"
        done
        printf '# crontab\n'
        crontab -l 2>/dev/null || printf '(空)\n'
    } > "$man"

    files=("${BASE#/}")
    for f in ${present[@]+"${present[@]}"}; do files+=("${f#/}"); done
    if ! tar -czf "$snap" --ignore-failed-read -C / "${files[@]}"; then
        printf '打快照失败：%s\n' "$snap" >&2
        exit 2
    fi
    log "快照：$snap（$(du -h "$snap" 2>/dev/null | cut -f1)）"
    log "清单：$man（单元文件 ${#present[@]} 个）"
    log "恢复命令：bash $0 restore --from $snap"
    log "按 spec §9 保留 $KEEP_DAYS 天；清理：bash $0 snapshot --prune --out $OUT"
}

do_restore() {
    local u st rc=0 members man moved cron timers=() f
    local p t cur_t cur line merged added
    [[ -n "$FROM" && -f "$FROM" ]] || { printf '找不到快照：%s\n' "${FROM:-<未给 --from>}" >&2; exit 2; }
    log "开始恢复 v3：$FROM"
    members=$(tar -tzf "$FROM") || { printf '快照读不出内容（已损坏？）：%s\n' "$FROM" >&2; exit 2; }
    for u in $V4_UNITS; do
        log "停 v4 单元 $u"
        systemctl stop "$u" 2>/dev/null || true
        systemctl disable "$u" 2>/dev/null || true
    done
    if [[ -d "$BASE" ]]; then
        moved="$BASE.v4-$(date -u +%Y%m%dT%H%M%SZ)"
        while [[ -e "$moved" ]]; do moved="$moved.1"; done
        mv "$BASE" "$moved" && log "原 $BASE 已挪到 $moved（v4 残留；确认 v3 正常后再删，别就地覆盖）"
    else
        log "$BASE 不存在，跳过挪移"
    fi
    if ! tar -xzf "$FROM" -C /; then
        printf '解包失败：%s（v4 单元已停、%s 已挪走，请人工处置）\n' "$FROM" "$BASE" >&2
        exit 1
    fi
    log "解包完成"
    for f in $V4_ONLY_CANDIDATES; do
        if [[ -e "$UNITDIR/$f" ]] && ! printf '%s\n' "$members" | grep -qx "${UNITDIR#/}/$f"; then
            log "删除 v4 独有单元 $UNITDIR/$f（快照里没有它，v3 不该有）"
            rm -f "$UNITDIR/$f"
        fi
    done
    log "daemon-reload"
    systemctl daemon-reload 2>/dev/null || true
    for u in $V3_UNITS; do
        systemctl enable "$u" 2>/dev/null || true
        systemctl start "$u" 2>/dev/null || true
    done
    # 只拉起快照里真有 .timer 文件的那几组（v3 机器不一定装齐：没配域名就没有 b-ui-cert-sync）
    for u in $V3_TIMER_UNITS; do
        if printf '%s\n' "$members" | grep -qx "${UNITDIR#/}/$u.timer"; then
            timers+=("$u.timer")
            systemctl enable "$u.timer" 2>/dev/null || true
            systemctl start "$u.timer" 2>/dev/null || true
        fi
    done
    man="${FROM%.tar.gz}.manifest"
    if [[ -f "$man" ]]; then
        # CLI 符号链接：照 # clilinks 段重建（absent 则删）。幂等——已经指对了就只报一句。
        while read -r p t; do
            [[ -n "$p" && -n "$t" ]] || continue
            cur_t=$(readlink "$p" 2>/dev/null)
            if [[ "$t" == "absent" ]]; then
                # 只删符号链接：v4 装机时 b-ui/bui 都是 ln -s 出来的。万一是普通文件（不是
                # 本脚本认识的形态）就只报警不删，免得吃掉别人的东西。
                if [[ -L "$p" ]]; then
                    rm -f "$p" && log "删除 $p（快照记它当时不存在）"
                elif [[ -e "$p" ]]; then
                    printf '跳过 %s：快照记它当时不存在，但现在是普通文件，未敢删\n' "$p" >&2
                fi
            elif [[ "$cur_t" == "$t" ]]; then
                log "$p → $t 已经指对，跳过"
            elif ln -sfn -- "$t" "$p"; then
                log "重建 $p → $t"
            else
                printf '重建符号链接失败：%s → %s\n' "$p" "$t" >&2
                rc=1
            fi
        done < <(awk '/^# clilinks$/{f=1;next} /^# /{f=0} f' "$man")
        # cron：bui import-v3 只删含 /opt/b-ui/ 的行、别的项目的行留着，所以切换后 crontab
        # 必然非空——不能「非空就跳过」，否则 v3 的三行永远回不来。按快照清单逐行幂等合并：
        # 快照里有、当前没有逐字相同行的才补，已有的不重复，别人的行原样保留。
        cron=$(awk '/^# crontab$/{f=1;next} f&&!/^\(空\)$/' "$man")
        cur=$(crontab -l 2>/dev/null)
        added=0
        merged=""
        [[ -z "$cur" ]] || merged="$cur"$'\n'
        while IFS= read -r line; do
            [[ -n "${line//[[:space:]]/}" ]] || continue
            printf '%s\n' "$cur" | grep -qxF -- "$line" && continue
            merged+="$line"$'\n'
            added=$((added + 1))
        done <<< "$cron"
        if [[ -z "$cron" ]]; then
            log "清单里没有 cron 行，无需回灌"
        elif [[ "$added" -eq 0 ]]; then
            log "清单里的 cron 行都已在 crontab 中，无需回灌"
        elif printf '%s' "$merged" | crontab -; then
            log "已按清单补齐 $added 行 v3 cron（现有 $(printf '%s\n' "$cur" | grep -c .) 行原样保留）"
        else
            printf '回灌 crontab 失败，请手工照 %s 的 # crontab 段恢复\n' "$man" >&2
            rc=1
        fi
    else
        log "快照没有同名清单（${man##*/}），跳过 cron 回灌、CLI 符号链接恢复与单元核对基线"
    fi
    for u in $V3_UNITS ${timers[@]+"${timers[@]}"}; do
        st=$(systemctl is-active "$u" 2>/dev/null)
        if [[ "$st" == "active" ]]; then
            log "OK $u active"
        else
            printf 'FAIL %s 不是 active（%s）\n' "$u" "${st:-unknown}" >&2
            rc=1
        fi
    done
    if [[ "$rc" -eq 0 ]]; then
        log "恢复完成：v3 的 $(printf '%s' "$V3_UNITS" | wc -w) 个服务 + ${#timers[@]} 个定时器全部 active。面板与订阅请人工确认一次。"
    else
        printf '恢复未完成：上面 FAIL 的项需人工处置（journalctl -u <unit> -n 50）\n' >&2
    fi
    return "$rc"
}

SUB="${1:-}"
[[ -n "$SUB" ]] || usage
shift || true
while [[ $# -gt 0 ]]; do
    case "$1" in
        --out) OUT="${2:-}"; shift 2 ;;
        --base) BASE="${2:-}"; shift 2 ;;
        --unit-dir) UNITDIR="${2:-}"; shift 2 ;;
        --link-dir) LINKDIR="${2:-}"; shift 2 ;;
        --from) FROM="${2:-}"; shift 2 ;;
        --list) LIST=1; shift ;;
        --prune) PRUNE=1; shift ;;
        *) usage ;;
    esac
done

case "$SUB" in
    snapshot) do_snapshot ;;
    restore) do_restore ;;
    *) usage ;;
esac
