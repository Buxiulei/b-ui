# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

B-UI is a Hysteria2 + Xray (VLESS-REALITY) proxy server installer with a built-in Web admin panel, plus a standalone Linux client script. It targets Ubuntu/Debian/CentOS/RHEL VPS hosts. Users are in mainland China; Windows users consume subscriptions with v2rayN, Linux users run the client script. The project, its UI and its commit messages are in Chinese.

## Architecture

Four layers:

1. **Install/Bootstrap** — `install.sh` downloads the files listed in `version.json` to `/opt/b-ui/` (GitHub Raw primary, raw.githack.com CDN fallback), detects fresh/upgrade/reinstall, then delegates to `server/core.sh`.

2. **Server-side shell** (`server/`, deployed flat into `/opt/b-ui/`)
   - `core.sh` — installer: installs Hysteria2, Xray, sing-box, Caddy, Node; generates every server config once at install time (see "Server topology" below); creates systemd units, sysctl tuning, cron.
   - `update.sh` — auto-updater run by cron (`update.sh auto` every 6h, `update.sh kernel` every 12h) and by the CLI. Contains the **idempotent migration blocks** that upgrade existing installs (see "Migration convention").
   - `b-ui-cli.sh` — the `sudo b-ui` terminal menu (services, logs, obfs toggle, uninstall).
   - `residential-helper.sh` / `resi-health.sh` — residential egress via SOCKS5 or HTTP upstreams (see below).
   - `b-ui-server.sh` at the repo root is a **legacy v3.1 all-in-one installer** (`/opt/hysteria`), not in `version.json`'s file list, not used by `install.sh`.

3. **Web admin panel** (`web/`, deployed to `/opt/b-ui/admin/`) — `server.js` is a single-file Node ESM HTTP server (no framework, no build step): JWT auth, user CRUD, traffic accounting, subscription generation, client-script serving. `app.js`/`index.html`/`style.css` are a vanilla SPA. `singbox-converter` in `package.json` is imported but unused.

4. **Linux client** — `b-ui-client.sh` (installed as `bui-c`, `/opt/hysteria-client/`): imports `hysteria2://` / `vless://` nodes, runs local SOCKS/HTTP via the Hysteria2 or Xray client, and a TUN mode via sing-box (`bui-tun.service`) using its **own** config template `generate_singbox_tun_config()`. The server-side sing-box subscription is a separate template; changes to one do not reach the other.

### Server topology (v3.5+)

Two Hysteria2 instances and two Xray inbounds, all written once by `core.sh` and never rewritten by the residential helper:

| Path | Listener | Egress |
|---|---|---|
| `hysteria-server` (`config.yaml`) | `:PORT` (+ built-in port hopping range in the same `listen:` line) | built-in direct, `mode: 4` (IPv4-only) |
| `hysteria-residential` (`config-residential.yaml`) | `:40000,41000-50000` | `outbounds: relay` → `socks5 127.0.0.1:2080` via `acl: relay(all)` |
| xray `vless-direct` (`xray-config.json`) | `:10001` REALITY | `freedom` with `domainStrategy: ForceIPv4` |
| xray `vless-residential` | `:10002` REALITY | `socks 127.0.0.1:2080` |

`127.0.0.1:2080` is a permanent local sing-box relay (`b-ui-relay.service`, config `/opt/b-ui/singbox-relay.json`) written by `residential-helper.sh`: when the residential URL pool is non-empty it routes AI-domain keywords (or everything in global mode) to a `resi-pool` of upstreams (each entry in `residential-proxy.json` has `type: socks5|http`, missing = socks5; v3.6.0 R10 auto-detects it on add, and the panel accepts `socks5://u:p@h:port`, `http://…`, `h:port:u:p` or `u:p@h:port` pasted as-is), otherwise everything goes direct (fail-open). Destination domains are passed through unresolved to the upstream. Helper entry points: `enable <url>|-`, `enable --add <url>|-` (`-` reads the URL from stdin so credentials never hit argv), `enable --remove <url>`, `disable`, `status`, `domains`, `reapply`. For Bright Data use the HTTP port (44445): its SOCKS5 port rejects plain-HTTP targets (tested 2026-09-10). The relay's Clash API on `127.0.0.1:9091` lets `resi-health.sh` (systemd timer) switch the selected upstream without restarting. State: `/opt/b-ui/residential-proxy.json` (chmod 600), `.resi-health-state.json`, lock `.relay.lock` (`flock`, taken only by `residential-helper.sh` writers; the health script is read-only and switches via the Clash API).

The VPS has no IPv6 egress: every server egress is pinned to IPv4, and client configs take over IPv6 and reject bare IPv6 destinations so apps fall back to IPv4 (spec: `docs/superpowers/specs/2026-09-10-ipv6-takeover-design.md`).

### Subscriptions (`web/server.js`)

Three unauthenticated endpoints, each building the same node set (fusion = Reality直连 :10001 / Reality住宅 :10002 / HY2直连 / HY2住宅 :40000) from `users.json` + `getConfig()`:
- `/api/sub/<user>` — base64 `vless://`/`hysteria2://` URIs (what v2rayN uses). Port hopping (`mport=`) is derived from the real `listen:` line of `config.yaml`.
- `/api/subscription/<user>` — a complete sing-box config (TUN + DNS + route). Must stay valid for **sing-box 1.12 through 1.14**: typed DNS servers, TUN `address` array, rule actions (`sniff`/`hijack-dns`/`reject`), `route.default_domain_resolver`, no `rule_set`/`download_detour` (1.13 and 1.15 disagree on those fields).
- `/api/clash/<user>` — mihomo YAML.

The node-set logic is duplicated across the three generators and `web/app.js genUri()`; a change to ports/labels/obfs must be applied to all of them.

### Migration convention (`server/update.sh`)

`apply_systemd_configs()` runs on every update (including "already latest") and holds a series of idempotent blocks (`D1`…`D9`, `A`, `A.fix`, …). Each block: checks for absence of its change, backs up (`*.bak.<ver>.<ts>`), edits, restarts only the service whose file changed, sets `updated=1`. New server-side behaviour that must reach existing installs goes in a new block here, not only in `core.sh`. Version updates restart `b-ui-admin` only when `web/*` or `version.json` changed.

## Development Commands

```bash
# Syntax checks (no committed test suite; CI is these plus manual verification)
bash -n install.sh server/core.sh server/update.sh server/b-ui-cli.sh server/residential-helper.sh server/resi-health.sh b-ui-client.sh
node --check web/server.js && node --check web/app.js
shellcheck -S error server/*.sh b-ui-client.sh install.sh   # if installed

# Run the panel locally against a scratch directory (never against /opt/b-ui)
# BASE_DIR needs: users.json, config.yaml (listen: line), reality-keys.json, xray-config.json
# (vless-direct inbound with realitySettings.dest/shortIds), certs/.domain, residential-proxy.json,
# and a copy of server/residential-helper.sh (server.js shells out to it).
cd web && BASE_DIR=/tmp/bui-dev ADMIN_DIR=$PWD ADMIN_PORT=18080 ADMIN_PASSWORD=test123 SERVER_IP=203.0.113.10 node server.js
curl -s http://127.0.0.1:18080/api/subscription/alice | sing-box check -c /dev/stdin

# Validate generated configs with the real binaries
sing-box check -c singbox-relay.json        # run on 1.13.x AND 1.14.x (v2rayN 7.25 caps at 1.14)
xray run -test -c xray-config.json

# Test a bash function in isolation: extract it and stub externals on PATH
sed -n '/^generate_config() {/,/^}/p' b-ui-client.sh > /tmp/fn.sh   # heredocs with a column-0 "}" need an awk extractor instead
PATH=/tmp/stubs:$PATH BASE_DIR=/tmp/t bash -c 'source /tmp/fn.sh; generate_config'

# Residential helper against a scratch dir (systemctl stubbed, pre-place a fake $BASE_DIR/sing-box to skip download)
BASE_DIR=/tmp/t bash server/residential-helper.sh reapply
```

## Important Conventions

- `version.json` is the version source of truth; scripts read it dynamically. Release: bump `version`, add a `changelog` entry, commit `bump: vX.Y.Z <description>`; fixes use `fix(scope): description`, features `feat(scope): …`.
- Client TUN template changes must bump `TUN_SCHEMA_VERSION` in `b-ui-client.sh` so installed clients regenerate `singbox-tun.json`.
- Shell output helpers: `print_info`/`print_success`/`print_warning`/`print_error`. The client script intentionally has no `set -e` (`((count++))` exits when the value is 0); `residential-helper.sh` has `set -euo pipefail`.
- Config files on the server: `config.yaml`, `config-residential.yaml` (Hysteria2), `xray-config.json`, `users.json`, `reality-keys.json`, `singbox-relay.json`, `residential-proxy.json`. Systemd: `hysteria-server`, `hysteria-residential`, `xray`, `b-ui-admin`, `b-ui-relay`, `caddy`, timers `hy2-watchdog`, `b-ui-resi-health`.
- Never put credentials on a command line (`ps` leaks them): pass curl proxy credentials via `-K -` config on stdin.
- Design docs live in `docs/superpowers/specs/YYYY-MM-DD-<slug>-design.md` with matching plans in `docs/superpowers/plans/`; the 2026-09-10 set documents the current architecture and the reasoning behind the IPv6, residential and restart hardening decisions.
- `packages/versions.json` records the kernel versions the server caches for clients; Xray's GitHub `/releases/latest` is unreliable (all tags are pre-releases), so version probing uses the releases list.
