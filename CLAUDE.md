# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

B-UI is a lightweight Hysteria2 + Xray multi-protocol proxy deployment tool with a built-in Web admin panel. It targets Linux servers (Ubuntu/Debian/CentOS/RHEL) and consists of server-side services and a client-side script. The project is primarily in Chinese.

## Architecture

The project has three layers:

1. **Install/Bootstrap** (`install.sh`) — Entry point. Downloads all files from GitHub to `/opt/b-ui/`, detects install type (fresh/upgrade/reinstall), handles environment prep (port cleanup, proxy variable clearing, service conflicts), then delegates to `core.sh`.

2. **Server-side Shell Scripts** (`server/`)
   - `core.sh` (1265 lines) — Core installer: installs Hysteria2, Node.js, Caddy, Xray binaries; collects user config (domain, ports, passwords); generates Hysteria2 YAML config, Xray JSON config, Caddy reverse proxy config; creates systemd services.
   - `b-ui-cli.sh` (565 lines) — Terminal management CLI (`sudo b-ui`): service start/stop/restart, status display, log viewing, password reset, BBR toggle, update check.
   - `update.sh` (732 lines) — Auto-update logic: compares `version.json` against GitHub, downloads changed files, restarts services. Also handles kernel binary caching for client downloads.

3. **Web Admin Panel** (`web/`)
   - `server.js` (1725 lines) — Node.js HTTP server (ESM). Provides REST API for user CRUD, traffic stats, config management, QR code generation, sing-box/Clash subscription endpoints, client install script serving. Uses JWT auth, rate limiting.
   - `app.js` (519 lines) — Vanilla JS frontend SPA. Handles login, user management UI, traffic display, subscription link generation.
   - `index.html` + `style.css` — Single-page admin panel with Chinese UI.

4. **Client Script** (`b-ui-client.sh`, 4240 lines) — Standalone bash script for Linux clients. Manages Hysteria2/Xray/sing-box client processes, TUN mode, node import (auto-detect protocol links/subscriptions), service control, kernel updates from server-first then GitHub fallback.

## Key Design Patterns

- **Version source of truth**: `version.json` at repo root. All scripts read version dynamically from this file (never hardcoded).
- **Dual download sources**: GitHub Raw (primary) + raw.githack.com CDN (China fallback). Network detection via Google connectivity test.
- **Deploy path**: Server files deploy to `/opt/b-ui/`, web panel to `/opt/b-ui/admin/`, client to `/opt/hysteria-client/`.
- **Web server local dev**: `server.js` auto-detects environment — uses `/opt/b-ui` on servers, falls back to repo-relative paths locally via `BASE_DIR`/`ADMIN_DIR` env vars.
- **No build step for frontend**: Vanilla JS/CSS/HTML served directly. Only dependency is `singbox-converter` npm package.
- **Residential IP helper**: `server/residential-helper.sh` (deploys to `/opt/b-ui/residential-helper.sh`) is the single control point for residential SOCKS5 outbound. Call with `enable <url>`, `disable`, `status`, or `reapply`. State persists in `/opt/b-ui/residential-proxy.json` (chmod 600). Hysteria2 config uses `# B-UI:RESIDENTIAL-START/END` awk anchor markers; Xray config uses `jq` atomic replace on `outbounds` + `routing.rules`.

## Development Commands

```bash
# Run web admin panel locally
cd web && npm install && npm start
# Server runs on port 8080 by default (override with ADMIN_PORT env var)

# Test install script syntax
bash -n install.sh
bash -n server/core.sh
bash -n server/b-ui-cli.sh
bash -n b-ui-client.sh

# Check shell scripts with shellcheck (if available)
shellcheck install.sh server/core.sh server/b-ui-cli.sh b-ui-client.sh
```

## Important Conventions

- All shell scripts use consistent color-coded output: `print_info` (blue), `print_success` (green), `print_warning` (yellow), `print_error` (red).
- Config files on server: `config.yaml` (Hysteria2), `xray-config.json` (Xray), `users.json` (user database), `reality-keys.json` (VLESS keys).
- Systemd services: `hysteria-server`, `b-ui-admin`, `caddy`, `xray`.
- Client install key mechanism: `install-key.txt` generated server-side, required in client install URL for authentication.
- The client script intentionally does NOT use `set -e` because `((count++))` exits when variable is 0.

## Version Bumping

When releasing a new version:
1. Update `version` field in `version.json`
2. Add changelog entry to `version.json`'s `changelog` object
3. Commit message convention: `bump: vX.Y.Z <description>` for releases, `fix(scope): description` for fixes
