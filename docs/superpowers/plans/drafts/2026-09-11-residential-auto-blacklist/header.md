# 住宅代理自动黑名单（R13）Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Route everything through the residential upstream except the targets that upstream provably refuses, with a per-upstream blacklist that is probed on add, learned from relay logs, refreshed daily, switched with the upstream, and visible/pinnable in the panel.

**Architecture:** A new `server/resi-blacklist.sh` owns probing (curl through the upstream + direct control), log learning (journal of `b-ui-relay`) and the state file `/opt/b-ui/residential-blacklist.json`. `server/residential-helper.sh` stays the only writer of `singbox-relay.json`: it compiles the selected upstream's blacklist into `domain_suffix`/`port` rules placed before the mode rules, validates with `sing-box check`, and restarts the relay only when the config digest changes (`blacklist-apply`). `server/resi-health.sh` calls `blacklist-apply` after a selector switch; a systemd timer runs the daily refresh; `web/server.js` exposes read/probe/pin endpoints and the panel renders them.

**Tech Stack:** bash (`set -uo pipefail` in the new script; `set -euo pipefail` in the helper), curl (`-K -` config on stdin for credentials), jq, systemd (`systemd-run`, calendar timer), sing-box 1.13/1.14 (`domain_suffix`, `port` rules, Clash API selector), Node 18+ ESM `web/server.js` (no framework), vanilla JS panel. Tests are plain bash scripts under `tests/resi-blacklist/` with PATH stubs; `sing-box check` uses real binaries.

**Spec:** `docs/superpowers/specs/2026-09-11-residential-auto-blacklist-design.md`

## Global Constraints

- sing-box relay config must pass `sing-box check` on 1.13.x and 1.14.x; no `rule_set`, no `download_detour`; `SINGBOX_MAX_MINOR="1.14"` unchanged.
- With an empty blacklist and no pins, `singbox-relay.json` must be byte-identical to the v3.6.3 output (zero-change upgrades do not restart `b-ui-relay`).
- Blacklist rules go after the `ip_cidr` direct rule and before the `domain_keyword` rule; order: pins→resi-pool, blacklist+pins→direct, ports→direct. Entries are exact probed hostnames (`domain_suffix`), never an apex; port entries never for 80/443/8080/8443.
- A target enters the blacklist only after two upstream refusals in one run **and** a successful direct control; it leaves only after `okStreak ≥ 2` daily passes; `unknown`/`unavailable` never change state; an unavailable upstream skips the whole run.
- Restart policy: add / health switch / manual pin or probe apply immediately when the digest changes; daily refresh `OnCalendar=*-*-* 04:00:00 Asia/Shanghai` + `RandomizedDelaySec=30min`, restart only when changed; every rewrite is gated by `sing-box check` (invalid ⇒ keep old config, exit 1, `applied` untouched).
- Credentials never appear on a command line: curl `-K -` config via stdin; state file and relay config are `chmod 600`, written `.tmp` + `mv`.
- Two locks: `.blacklist.lock` (state file only) and `.relay.lock` (relay config only); `resi-blacklist.sh` must release its lock before calling the helper.
- Limits: ≤500 entries and ≤200 learned per upstream, ≤20 port entries; parallel probes `BL_PARALLEL=8`, per-probe `--max-time 12`, 2 attempts 3 s apart.
- New file `server/resi-blacklist.sh` must be registered in `version.json` `files[]`, `install.sh`, and both `update.sh` file tables; migration block `D10` in `apply_systemd_configs()` is idempotent and self-heals the script download.
- Panel copy, comments and commit messages in Chinese; commits `feat(residential): …` / `fix(...)`, release `bump: v3.7.0 …`, each ending with `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`.
- Tests are never deployed (`tests/` is not in `version.json` `files[]`); they run on macOS with PATH stubs; production targets Ubuntu/Debian/CentOS.

---
