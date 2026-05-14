import http from "http";
import fs from "fs";
import crypto from "crypto";
import { execSync, exec, spawn, spawnSync, execFile } from "child_process";
import path from "path";
import https from "https";
import { fileURLToPath } from "url";
import { convertOutboundToLink } from "singbox-converter";

// ESM 模式下获取 __dirname
const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

// 本地开发支持：如果 /opt/b-ui 不存在，使用当前目录
const BASE_DIR = process.env.BASE_DIR || (fs.existsSync("/opt/b-ui") ? "/opt/b-ui" : path.dirname(__dirname));
const ADMIN_DIR = process.env.ADMIN_DIR || (fs.existsSync("/opt/b-ui") ? path.join(BASE_DIR, "admin") : __dirname);

function getVersion() {
    try {
        const versionFile = path.join(BASE_DIR, "version.json");
        if (fs.existsSync(versionFile)) {
            return JSON.parse(fs.readFileSync(versionFile, "utf8")).version || "unknown";
        }
    } catch { }
    return "unknown";
}
const VERSION = getVersion();

// 分发 b-ui-client.sh 时把脚本中的 SCRIPT_VERSION 替换成 version.json 里的版本，
// 防止仓库 b-ui-client.sh 滞后于 version.json 时客户端下载到旧版本号。
function injectClientVersion(scriptContent) {
    const v = getVersion();
    if (!v || v === "unknown") return scriptContent;
    return scriptContent.replace(/^SCRIPT_VERSION=".*"/m, `SCRIPT_VERSION="${v}"`);
}

const CONFIG = {
    port: process.env.ADMIN_PORT || 8080,
    bind: process.env.ADMIN_BIND || "127.0.0.1",
    adminPassword: process.env.ADMIN_PASSWORD || "admin123",
    jwtSecret: process.env.JWT_SECRET || crypto.randomBytes(32).toString("hex"),
    hysteriaConfig: process.env.HYSTERIA_CONFIG || `${BASE_DIR}/config.yaml`,
    xrayConfig: process.env.XRAY_CONFIG || `${BASE_DIR}/xray-config.json`,
    xrayKeysFile: process.env.XRAY_KEYS || `${BASE_DIR}/reality-keys.json`,
    residentialConfig: process.env.RESIDENTIAL_CONFIG || `${BASE_DIR}/residential-proxy.json`,
    residentialHelper: `${BASE_DIR}/residential-helper.sh`,
    usersFile: process.env.USERS_FILE || `${BASE_DIR}/users.json`,
    adminEnvFile: process.env.ADMIN_ENV_FILE || `${BASE_DIR}/admin.env`,
    hysteriaResidentialConfig: process.env.HYSTERIA_RESIDENTIAL_CONFIG || `${BASE_DIR}/config-residential.yaml`,
    trafficPort: 9999,
    trafficPortResidential: 9998,
    xrayApiPort: 10085
};

// v3.5.0: 获取服务器 IP — 订阅 URL 用 IP literal 防客户端 DNS 投毒
// 优先 SERVER_IP env → /opt/b-ui/server_ip.txt → ip route 探测出口 IP → 127.0.0.1 兜底
function getServerIP() {
    if (process.env.SERVER_IP) return process.env.SERVER_IP.trim();
    try {
        const ipFile = path.join(BASE_DIR, "server_ip.txt");
        if (fs.existsSync(ipFile)) {
            const ip = fs.readFileSync(ipFile, "utf8").trim();
            if (ip) return ip;
        }
    } catch { }
    try {
        const out = spawnSync("ip", ["route", "get", "8.8.8.8"], { encoding: "utf8", timeout: 3000 });
        const m = (out.stdout || "").match(/src\s+([0-9.]+)/);
        if (m) return m[1];
    } catch { }
    return "127.0.0.1";
}

// --- 客户端安装 Key 管理 ---
const INSTALL_KEY_FILE = path.join(BASE_DIR, "install-key.txt");

function getOrCreateInstallKey() {
    try {
        if (fs.existsSync(INSTALL_KEY_FILE)) {
            return fs.readFileSync(INSTALL_KEY_FILE, "utf8").trim();
        }
    } catch { }
    // 生成新的安装 key
    const key = crypto.randomBytes(16).toString("hex");
    try {
        fs.writeFileSync(INSTALL_KEY_FILE, key);
    } catch { }
    return key;
}

function verifyInstallKey(key) {
    const validKey = getOrCreateInstallKey();
    return key === validKey;
}

// 生成引导安装脚本 (当服务端没有完整脚本时使用)
function generateBootstrapScript(serverDomain, key) {
    return `
# B-UI 客户端引导安装脚本
# 从服务端下载安装包并安装

set -e

RED='\\033[0;31m'
GREEN='\\033[0;32m'
YELLOW='\\033[1;33m'
NC='\\033[0m'

echo -e "\${GREEN}B-UI 客户端安装程序\${NC}"
echo -e "服务端: \${YELLOW}${serverDomain}\${NC}"
echo ""

# 检查 root 权限
if [[ $EUID -ne 0 ]]; then
    echo -e "\${RED}[ERROR]\${NC} 此脚本需要 root 权限"
    exit 1
fi

# 创建目录
mkdir -p /opt/hysteria-client
echo "${serverDomain}" > /opt/hysteria-client/server_address

# 下载安装包
ARCH=$(uname -m)
case "$ARCH" in
    x86_64) ARCH_SUFFIX="amd64" ;;
    aarch64) ARCH_SUFFIX="arm64" ;;
    *) ARCH_SUFFIX="amd64" ;;
esac

echo -e "\${GREEN}[INFO]\${NC} 下载 Hysteria2..."
curl -fsSL -k "https://${serverDomain}/packages/hysteria-linux-\${ARCH_SUFFIX}" -o /usr/local/bin/hysteria
chmod +x /usr/local/bin/hysteria

echo -e "\${GREEN}[INFO]\${NC} 下载 sing-box..."
curl -fsSL -k "https://${serverDomain}/packages/sing-box-linux-\${ARCH_SUFFIX}.tar.gz" -o /tmp/sing-box.tar.gz
tar -xzf /tmp/sing-box.tar.gz -C /tmp
find /tmp -name "sing-box" -type f -exec mv {} /usr/bin/sing-box \\;
chmod +x /usr/bin/sing-box
rm -rf /tmp/sing-box*

echo -e "\${GREEN}[INFO]\${NC} 下载 Xray..."
curl -fsSL -k "https://${serverDomain}/packages/xray-linux-\${ARCH_SUFFIX}.zip" -o /tmp/xray.zip
unzip -o /tmp/xray.zip -d /tmp/xray_temp >/dev/null 2>&1 || true
mv /tmp/xray_temp/xray /usr/local/bin/xray 2>/dev/null || true
chmod +x /usr/local/bin/xray 2>/dev/null || true
rm -rf /tmp/xray* 

echo ""
echo -e "\${GREEN}[SUCCESS]\${NC} 核心安装完成!"
echo -e "  Hysteria2: $(hysteria version 2>/dev/null | grep Version | awk '{print $2}' || echo '已安装')"
echo -e "  sing-box: $(sing-box version 2>/dev/null | head -n1 | awk '{print $3}' || echo '已安装')"
echo -e "  Xray: $(xray version 2>/dev/null | head -n1 | awk '{print $2}' || echo '已安装')"
echo ""
echo -e "运行 \${YELLOW}sudo bui-c\${NC} 进行配置管理"
`;
}

// --- Security: Rate Limiting & Audit ---
const loginAttempts = {};
const RATE_LIMIT = { maxAttempts: 5, windowMs: 300000 };

function checkRateLimit(ip) {
    const now = Date.now(), rec = loginAttempts[ip];
    if (!rec) return true;
    if (now - rec.first > RATE_LIMIT.windowMs) { delete loginAttempts[ip]; return true; }
    return rec.count < RATE_LIMIT.maxAttempts;
}

function recordAttempt(ip, success) {
    const now = Date.now(), rec = loginAttempts[ip];
    if (!rec) loginAttempts[ip] = { first: now, count: 1 };
    else rec.count++;
    if (success) delete loginAttempts[ip];
    log("AUDIT", ip + " login " + (success ? "SUCCESS" : "FAILED") + " (attempts: " + (loginAttempts[ip]?.count || 0) + ")");
}

function getClientIP(req) {
    return req.headers["x-forwarded-for"]?.split(",")[0].trim() || req.socket.remoteAddress || "unknown";
}

// --- Backend Logic ---
function log(l, m) { console.log("[" + new Date().toISOString() + "] [" + l + "] " + m); }

function genToken(d) {
    const p = Buffer.from(JSON.stringify({ ...d, exp: Date.now() + 864e5, iat: Date.now() })).toString("base64");
    return p + "." + crypto.createHmac("sha256", CONFIG.jwtSecret).update(p).digest("hex");
}

function verifyToken(t) {
    try {
        const [p, s] = t.split(".");
        if (s !== crypto.createHmac("sha256", CONFIG.jwtSecret).update(p).digest("hex")) return null;
        const d = JSON.parse(Buffer.from(p, "base64").toString());
        return d.exp < Date.now() ? null : d;
    } catch { return null; }
}

function parseBody(r) {
    return new Promise(s => {
        let b = "";
        r.on("data", c => b += c);
        r.on("end", () => {
            if (!b) return s({ __empty: true });
            try { s(JSON.parse(b)); } catch { s({ __invalidJson: true }); }
        });
    });
}

// 验证 username 白名单（用于 POST 创建和 PUT 路径）
function validateUsername(name) {
    if (typeof name !== "string" || name.length === 0) return "username 不能为空";
    if (name.length > 64) return "username 长度不能超过 64 字符";
    if (!/^[\p{L}\p{N}_\-.]+$/u.test(name)) return "username 仅允许字母/数字/中文/下划线/连字符/点";
    return null;
}

function validatePassword(pwd) {
    if (typeof pwd !== "string" || pwd.length === 0) return "password 不能为空";
    if (pwd.length > 256) return "password 长度不能超过 256 字符";
    return null;
}

function sendJSON(r, d, s = 200, headers = {}) {
    r.writeHead(s, {
        "Content-Type": "application/json",
        "Access-Control-Allow-Origin": "*",
        "Access-Control-Allow-Methods": "*",
        "Access-Control-Allow-Headers": "*",
        ...headers
    });
    r.end(JSON.stringify(d));
}

function loadUsers() {
    try {
        return fs.existsSync(CONFIG.usersFile) ? JSON.parse(fs.readFileSync(CONFIG.usersFile, "utf8")) : [];
    } catch { return []; }
}

function saveUsers(u) {
    try {
        fs.writeFileSync(CONFIG.usersFile, JSON.stringify(u, null, 2));
        // fusion 用户需要同时添加到两个配置
        const hy2Users = u.filter(x => !x.protocol || x.protocol === "hysteria2" || x.protocol === "fusion");
        // 所有有 uuid 的用户都添加到 Xray Reality（支持 VLESS 备用）
        const vlessUsers = u.filter(x => x.uuid);
        updateHysteriaConfig(hy2Users);
        // v3.5.0: 同步 hy2-residential 实例（直连/住宅版共用密码）
        updateHysteriaResidentialConfig(hy2Users);
        updateXrayConfig(vlessUsers, u.filter(x => x.protocol === "vless-ws-tls"));
        return true;
    } catch { return false; }
}

// v3.5.0: 写 config-residential.yaml 的 auth.userpass 段并 reload hysteria-residential service
function updateHysteriaResidentialConfig(users) {
    try {
        if (!fs.existsSync(CONFIG.hysteriaResidentialConfig)) return; // v3.4 老服务器没装第二实例，跳过
        let c = fs.readFileSync(CONFIG.hysteriaResidentialConfig, "utf8");
        const up = users.reduce((a, u) => { a[u.username] = u.password; return a; }, {});
        const auth = "auth:\n  type: userpass\n  userpass:\n" + Object.entries(up).map(([u, p]) => "    " + u + ": " + p).join("\n");
        c = c.replace(/auth:[\s\S]*?(?=\n[a-zA-Z]|$)/, auth + "\n\n");
        fs.writeFileSync(CONFIG.hysteriaResidentialConfig, c);
        execSync("systemctl reload-or-restart hysteria-residential 2>/dev/null || true", { stdio: "pipe" });
    } catch (e) { log("ERROR", "HysteriaResidential: " + e.message); }
}

function updateHysteriaConfig(users) {
    try {
        let c = fs.readFileSync(CONFIG.hysteriaConfig, "utf8");
        const up = users.reduce((a, u) => { a[u.username] = u.password; return a; }, {});
        const auth = "auth:\n  type: userpass\n  userpass:\n" + Object.entries(up).map(([u, p]) => "    " + u + ": " + p).join("\n");
        c = c.replace(/auth:[\s\S]*?(?=\n[a-zA-Z]|$)/, auth + "\n\n");
        fs.writeFileSync(CONFIG.hysteriaConfig, c);
        execSync("systemctl restart hysteria-server", { stdio: "pipe" });
    } catch (e) { log("ERROR", "Hysteria: " + e.message); }
}

function updateXrayConfig(realityUsers, wsUsers = []) {
    try {
        if (!fs.existsSync(CONFIG.xrayConfig)) return;
        let c = JSON.parse(fs.readFileSync(CONFIG.xrayConfig, "utf8"));

        // v3.5.0: 双 Reality inbound — vless-direct + vless-residential 共用 clients/SNI
        // 兼容 v3.4 老 tag "vless-reality" — 若存在则当作 vless-direct 处理
        const realityClients = realityUsers.map(u => ({ id: u.uuid, flow: "xtls-rprx-vision", email: u.username }));
        const userSnis = realityUsers.filter(u => u.sni).map(u => u.sni);

        for (const inboundTag of ["vless-direct", "vless-residential", "vless-reality"]) {
            const inbound = c.inbounds.find(i => i.tag === inboundTag);
            if (inbound) {
                inbound.settings.clients = realityClients;
                const baseSni = inbound.streamSettings?.realitySettings?.dest?.split(":")[0] || "www.bing.com";
                const allSnis = [...new Set([baseSni, ...userSnis])];
                if (inbound.streamSettings?.realitySettings) inbound.streamSettings.realitySettings.serverNames = allSnis;
            }
        }

        // Update WS+TLS inbound
        // v3.5.0: 端口从 10002 改到 10003（10002 被 vless-residential 占用）
        const wsClients = wsUsers.map(u => ({ id: u.uuid, email: u.username }));
        let wsInbound = c.inbounds.find(i => i.tag === "vless-ws-tls");
        // 老配置若有 vless-ws-tls 在 :10002 且 vless-residential 也在 :10002，强制把 ws-tls 改到 :10003
        if (wsInbound && wsInbound.port === 10002 && c.inbounds.some(i => i.tag === "vless-residential" && i.port === 10002)) {
            wsInbound.port = 10003;
        }
        if (wsUsers.length > 0) {
            if (!wsInbound) {
                const hc = fs.readFileSync(CONFIG.hysteriaConfig, "utf8");
                const dm = hc.match(/\/live\/([^\/]+)\/fullchain/);
                const domain = dm ? dm[1] : "localhost";
                wsInbound = {
                    tag: "vless-ws-tls",
                    port: 10003,
                    protocol: "vless",
                    settings: { clients: wsClients, decryption: "none" },
                    streamSettings: {
                        network: "ws",
                        security: "tls",
                        tlsSettings: {
                            serverName: domain,
                            certificates: [{
                                certificateFile: "/etc/letsencrypt/live/" + domain + "/fullchain.pem",
                                keyFile: "/etc/letsencrypt/live/" + domain + "/privkey.pem"
                            }]
                        },
                        wsSettings: { path: "/ws", headers: {} }
                    }
                };
                c.inbounds.push(wsInbound);
            } else {
                wsInbound.settings.clients = wsClients;
            }
        }
        fs.writeFileSync(CONFIG.xrayConfig, JSON.stringify(c, null, 2));
        execSync("systemctl restart xray 2>/dev/null||true", { stdio: "pipe" });
    } catch (e) { log("ERROR", "Xray: " + e.message); }
}

function getConfig() {
    try {
        let dm, pm;
        const hc = fs.readFileSync(CONFIG.hysteriaConfig, "utf8");
        pm = hc.match(/listen:\s*:(\d+)/);
        
        // 域名提取: 优先 certs/.domain → Caddyfile → 旧证书路径
        const domainFile = path.join(BASE_DIR, "certs", ".domain");
        if (fs.existsSync(domainFile)) {
            dm = fs.readFileSync(domainFile, "utf8").trim();
        }
        if (!dm) {
            try {
                const caddyfile = fs.readFileSync("/etc/caddy/Caddyfile", "utf8");
                const m = caddyfile.match(/^([a-zA-Z0-9][-a-zA-Z0-9.]*\.[a-zA-Z]{2,})/m);
                if (m) dm = m[1];
            } catch { }
        }
        if (!dm) {
            const certMatch = hc.match(/\/live\/([^\/]+)\/fullchain/);
            if (certMatch) dm = certMatch[1];
        }
        let xrayPort = 10001, pubKey = "", shortId = "", sni = "www.bing.com";
        try {
            const xc = JSON.parse(fs.readFileSync(CONFIG.xrayConfig, "utf8"));
            const xi = xc.inbounds.find(i => i.tag === "vless-direct" || i.tag === "vless-reality");
            if (xi) {
                xrayPort = xi.port;
                const dest = xi.streamSettings?.realitySettings?.dest || "";
                sni = dest.split(":")[0] || "www.bing.com";
                shortId = xi.streamSettings?.realitySettings?.shortIds?.[0] || "";
            }
        } catch { }
        try {
            const k = JSON.parse(fs.readFileSync(CONFIG.xrayKeysFile, "utf8"));
            pubKey = k.publicKey || "";
            shortId = shortId || k.shortId || "";
        } catch { }

        // 读取端口跳跃配置
        let portHopping = { enabled: false, start: 20000, end: 30000 };
        try {
            const phFile = path.join(BASE_DIR, "port-hopping.json");
            if (fs.existsSync(phFile)) {
                const ph = JSON.parse(fs.readFileSync(phFile, "utf8"));
                portHopping = {
                    enabled: ph.enabled || false,
                    start: ph.startPort || 20000,
                    end: ph.endPort || 30000
                };
            }
        } catch { }

        // v3.4.19 Cluster E: 检测 obfs salamander
        // 从 config.yaml 解析 obfs 段（YAML 顶层 obfs:type:salamander 块）
        let obfs = { enabled: false, type: "", password: "" };
        try {
            const obfsBlock = hc.match(/^obfs:\s*\n((?:[ \t].*\n?)+)/m);
            if (obfsBlock) {
                const block = obfsBlock[1];
                const typeMatch = block.match(/^\s+type:\s*(\S+)/m);
                const pwdMatch = block.match(/^\s+password:\s*(\S+)/m);
                if (typeMatch) {
                    obfs.type = typeMatch[1].trim();
                    obfs.enabled = true;
                }
                if (pwdMatch) obfs.password = pwdMatch[1].trim();
            }
        } catch { }

        return {
            domain: dm || "localhost",
            port: pm ? pm[1] : "443",
            xrayPort,
            pubKey,
            shortId,
            sni,
            portHopping,
            obfs
        };
    } catch {
        return {
            domain: "localhost",
            port: "443",
            xrayPort: 10001,
            pubKey: "",
            shortId: "",
            sni: "www.bing.com",
            portHopping: { enabled: false, start: 20000, end: 30000 },
            obfs: { enabled: false, type: "", password: "" }
        };
    }
}

// 生成 sing-box 融合配置 (Hy2 优先 + VLESS 备用)
function generateSingboxConfig(user, cfg, host) {
    const outbounds = [];
    const outboundTags = [];

    // 1. Hysteria2 出站 (如果用户有密码)
    if (user.password) {
        let serverPort = cfg.port;
        // 如果启用端口跳跃，使用端口范围格式
        if (cfg.portHopping && cfg.portHopping.enabled) {
            serverPort = `${cfg.portHopping.start}-${cfg.portHopping.end}`;
        }

        outbounds.push({
            type: "hysteria2",
            tag: "hy2-proxy",
            server: host,
            server_port: parseInt(cfg.port),
            // 连接超时设置 - 2秒无法建立连接就放弃
            connect_timeout: "2s",
            // 端口跳跃配置
            ...(cfg.portHopping?.enabled ? {
                hop_ports: `${cfg.portHopping.start}-${cfg.portHopping.end}`,
                hop_interval: "30s"
            } : {}),
            // v3.4.19 Cluster E: obfs salamander（GFW 高峰期应急）
            ...(cfg.obfs?.enabled && cfg.obfs.type === "salamander" && cfg.obfs.password ? {
                obfs: { type: "salamander", password: cfg.obfs.password }
            } : {}),
            password: `${user.username}:${user.password}`,
            tls: {
                enabled: true,
                server_name: host,
                insecure: false
            }
        });
        outboundTags.push("hy2-proxy");
    }

    // 2. VLESS-Reality 出站 (如果用户有 UUID 且服务器配置了 Xray)
    if (user.uuid && cfg.pubKey && cfg.shortId) {
        outbounds.push({
            type: "vless",
            tag: "vless-proxy",
            server: host,
            server_port: cfg.xrayPort || 10001,
            // 连接超时设置 - 2秒无法建立连接就放弃
            connect_timeout: "2s",
            uuid: user.uuid,
            flow: "xtls-rprx-vision",
            tls: {
                enabled: true,
                server_name: user.sni || cfg.sni || "www.bing.com",
                utls: { enabled: true, fingerprint: "chrome" },
                reality: {
                    enabled: true,
                    public_key: cfg.pubKey,
                    short_id: cfg.shortId
                }
            }
        });
        outboundTags.push("vless-proxy");
    }

    // 3. 自动选择出站 (urltest - 快速故障切换)
    // interval: 10s - 每10秒检测一次连接状态
    // tolerance: 50 - 允许50ms的延迟差异
    // interrupt_exist_connections: true - 切换时立即中断现有连接
    if (outboundTags.length > 0) {
        outbounds.push({
            type: "urltest",
            tag: "auto-select",
            outbounds: outboundTags,
            url: "https://www.gstatic.com/generate_204",
            interval: "10s",        // 每10秒检测一次
            tolerance: 50,          // 50ms 容差，优先选择延迟最低的
            idle_timeout: "30s",    // 30秒无流量后暂停检测
            interrupt_exist_connections: true  // 切换时立即生效
        });
    }

    // 4. 直连和屏蔽
    outbounds.push({ type: "direct", tag: "direct" });
    outbounds.push({ type: "block", tag: "block" });
    outbounds.push({ type: "dns", tag: "dns-out" });

    // 完整 sing-box 配置 (优化版 - 快速故障切换)
    return {
        log: { level: "info", timestamp: true },
        experimental: {
            // 启用 Clash API 用于实时切换
            clash_api: {
                external_controller: "127.0.0.1:9090",
                external_ui: "",
                secret: "",
                default_mode: "rule"
            },
            cache_file: {
                enabled: true,
                path: "cache.db"
            }
        },
        dns: {
            servers: [
                { tag: "google", address: "https://8.8.8.8/dns-query", detour: "auto-select" },
                { tag: "local", address: "223.5.5.5", detour: "direct" }
            ],
            rules: [
                { domain_suffix: [".cn"], server: "local" },
                { query_type: ["A", "AAAA"], server: "google" }
            ],
            final: "google"
        },
        inbounds: [
            {
                type: "mixed",
                tag: "mixed-in",
                listen: "127.0.0.1",
                listen_port: 7890
            },
            {
                type: "tun",
                tag: "tun-in",
                interface_name: "bui-tun",
                inet4_address: "172.19.0.1/30",
                auto_route: true,
                strict_route: true,
                stack: "system",
                sniff: true
            }
        ],
        outbounds,
        route: {
            rules: [
                { protocol: "dns", outbound: "dns-out" },
                { geoip: ["cn", "private"], outbound: "direct" },
                { geosite: "cn", outbound: "direct" }
            ],
            final: "auto-select",
            auto_detect_interface: true
        }
    };
}

// 生成 Clash Meta (mihomo) YAML 配置 (Clash Verge Rev 兼容)
function generateClashConfig(user, cfg, host) {
    const proxies = [];
    const proxyNames = [];

    // 1. Hysteria2 节点
    if (user.password) {
        const hy2Name = `${user.username}-高速版`;
        let hy2Yaml = `  - name: "${hy2Name}"
    type: hysteria2
    server: ${host}
    port: ${parseInt(cfg.port)}`;

        // 端口跳跃
        if (cfg.portHopping && cfg.portHopping.enabled) {
            hy2Yaml += `\n    ports: "${cfg.portHopping.start}-${cfg.portHopping.end}"
    hop-interval: 30`;
        }

        hy2Yaml += `\n    password: "${user.username}:${user.password}"
    sni: ${host}
    skip-cert-verify: false
    alpn:
      - h3`;

        proxies.push(hy2Yaml);
        proxyNames.push(hy2Name);
    }

    // 2. VLESS-Reality 节点
    if (user.uuid && cfg.pubKey && cfg.shortId) {
        const vlessName = `${user.username}-稳定版`;
        const userSni = user.sni || cfg.sni || "www.bing.com";

        const vlessYaml = `  - name: "${vlessName}"
    type: vless
    server: ${host}
    port: ${cfg.xrayPort || 10001}
    uuid: ${user.uuid}
    flow: xtls-rprx-vision
    tls: true
    udp: true
    servername: ${userSni}
    client-fingerprint: chrome
    reality-opts:
      public-key: ${cfg.pubKey}
      short-id: ${cfg.shortId}
    network: tcp`;

        proxies.push(vlessYaml);
        proxyNames.push(vlessName);
    }

    // 代理名称列表 (YAML 格式)
    const nameList = proxyNames.map(n => `      - "${n}"`).join("\n");

    // Clash Meta 订阅配置
    // 包含 DNS (fake-ip) + proxies + proxy-groups + rules
    // 不包含 mixed-port (由 Clash Verge Rev 自身管理)
    const yaml = `# B-UI Clash Meta 订阅配置
# 用户: ${user.username}
# 生成时间: ${new Date().toISOString()}

mode: rule
log-level: warning
unified-delay: true
tcp-concurrent: true
find-process-mode: strict
global-client-fingerprint: chrome

dns:
  enable: true
  enhanced-mode: fake-ip
  fake-ip-range: 198.18.0.1/16
  fake-ip-filter:
    - "*.lan"
    - "*.local"
    - "*.localhost"
    - "${host}"
  default-nameserver:
    - 223.5.5.5
    - 119.29.29.29
  nameserver:
    - https://dns.alidns.com/dns-query
    - https://doh.pub/dns-query
  nameserver-policy:
    "${host}": "223.5.5.5"
  fallback:
    - https://8.8.8.8/dns-query
    - https://1.1.1.1/dns-query
  fallback-filter:
    geoip: true
    geoip-code: CN

proxies:
${proxies.join("\n")}

proxy-groups:
  - name: "自动选择"
    type: url-test
    proxies:
${nameList}
    url: https://www.gstatic.com/generate_204
    interval: 300
    tolerance: 50

  - name: "PROXY"
    type: select
    proxies:
      - "自动选择"
${nameList}

rules:
  - GEOIP,private,DIRECT,no-resolve
  - GEOSITE,cn,DIRECT
  - GEOIP,cn,DIRECT,no-resolve
  - MATCH,PROXY
`;

    return yaml;
}



function fetchStats(ep) {
    return new Promise(s => {
        const r = http.request({
            hostname: "127.0.0.1",
            port: CONFIG.trafficPort,
            path: ep,
            method: "GET"
        }, res => {
            let d = "";
            res.on("data", c => d += c);
            res.on("end", () => { try { s(JSON.parse(d)); } catch { s({}); } });
        });
        r.on("error", () => s({}));
        r.setTimeout(3e3, () => { r.destroy(); s({}); });
        r.end();
    });
}

function postStats(ep, b) {
    return new Promise(s => {
        const d = JSON.stringify(b);
        const r = http.request({
            hostname: "127.0.0.1",
            port: CONFIG.trafficPort,
            path: ep,
            method: "POST",
            headers: { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(d) }
        }, res => s(res.statusCode === 200));
        r.on("error", () => s(false));
        r.write(d);
        r.end();
    });
}

// --- Xray Stats API (gRPC via CLI) ---
let lastXrayTraffic = {};

function fetchXrayUserStats() {
    // 获取所有 VLESS 用户的流量统计
    // Xray stats 格式: user>>>email>>>traffic>>>uplink/downlink
    const result = {};
    try {
        const users = loadUsers().filter(u => u.protocol === "vless-reality" || u.protocol === "vless-ws-tls");
        for (const u of users) {
            const email = u.username;
            try {
                // 用户名安全处理：过滤非法字符防止命令注入
                const safeEmail = email.replace(/[^a-zA-Z0-9._@-]/g, "");
                // 查询 uplink (上传流量)
                const upCmd = `xray api stats --server=127.0.0.1:${CONFIG.xrayApiPort} -name "user>>>${safeEmail}>>>traffic>>>uplink" 2>/dev/null || echo "{}"`;
                const upResult = execSync(upCmd, { encoding: "utf8", timeout: 3000 }).trim();
                let tx = 0;
                try {
                    const upJson = JSON.parse(upResult);
                    tx = upJson.stat?.value || 0;
                } catch { }

                // 查询 downlink (下载流量)
                const downCmd = `xray api stats --server=127.0.0.1:${CONFIG.xrayApiPort} -name "user>>>${safeEmail}>>>traffic>>>downlink" 2>/dev/null || echo "{}"`;
                const downResult = execSync(downCmd, { encoding: "utf8", timeout: 3000 }).trim();
                let rx = 0;
                try {
                    const downJson = JSON.parse(downResult);
                    rx = downJson.stat?.value || 0;
                } catch { }

                if (tx > 0 || rx > 0) {
                    result[email] = { tx, rx };
                }
            } catch (e) {
                // 忽略单个用户查询失败
            }
        }
    } catch (e) {
        log("WARN", "Xray stats query failed: " + e.message);
    }
    return result;
}

function fetchXrayOnline() {
    // 通过检测流量变化判断 VLESS 用户是否在线
    // 如果用户在过去 10 秒内有流量变化，则认为在线
    const result = {};
    try {
        const currentStats = fetchXrayUserStats();
        for (const [email, stat] of Object.entries(currentStats)) {
            const last = lastXrayTraffic[email] || { tx: 0, rx: 0 };
            // 如果有流量变化，认为在线
            if (stat.tx > last.tx || stat.rx > last.rx) {
                result[email] = 1; // 1 表示有一个连接
            }
            lastXrayTraffic[email] = stat;
        }
    } catch (e) {
        log("WARN", "Xray online check failed: " + e.message);
    }
    return result;
}

// 合并 Hysteria2 和 Xray 的统计数据
async function getMergedStats() {
    const hy2Stats = await fetchStats("/traffic");
    const xrayStats = fetchXrayUserStats();
    return { ...hy2Stats, ...xrayStats };
}

async function getMergedOnline() {
    const hy2Online = await fetchStats("/online");
    const xrayOnline = fetchXrayOnline();
    return { ...hy2Online, ...xrayOnline };
}

// --- Traffic Tracking ---
function getCurrentMonth() { return new Date().toISOString().slice(0, 7); }

function checkUserLimits(u) {
    const now = Date.now(), m = getCurrentMonth();
    if (u.limits?.expiresAt && new Date(u.limits.expiresAt).getTime() < now) return { ok: false, reason: "expired" };
    if (u.limits?.trafficLimit && (u.usage?.total || 0) >= u.limits.trafficLimit) return { ok: false, reason: "traffic_exceeded" };
    if (u.limits?.monthlyLimit && (u.usage?.monthly?.[m] || 0) >= u.limits.monthlyLimit) return { ok: false, reason: "monthly_exceeded" };
    return { ok: true };
}

function handleManage(params, res) {
    const key = params.get("key"), action = params.get("action"), user = params.get("user");
    if (key !== CONFIG.adminPassword) return sendJSON(res, { error: "Invalid key" }, 403);
    if (!action) return sendJSON(res, { error: "Missing action" }, 400);
    const users = loadUsers();

    if (action === "create") {
        if (!user) return sendJSON(res, { error: "Missing user" }, 400);
        if (users.find(u => u.username === user)) return sendJSON(res, { error: "User exists" }, 400);
        const protocol = params.get("protocol") || "hysteria2";
        const days = parseInt(params.get("days")) || 0;
        const traffic = parseFloat(params.get("traffic")) || 0;
        const monthly = parseFloat(params.get("monthly")) || 0;
        const speed = parseFloat(params.get("speed")) || 100; // 默认 100Mbps
        const sni = params.get("sni") || "www.bing.com";
        // v3.5.5: per-user 住宅代理订阅开关（默认 true 兼容现有用户）
        const residential = params.get("residential") !== "false";

        // 创建新用户，同时生成 Hy2 和 VLESS 凭据以支持融合切换
        const hyPass = crypto.randomBytes(8).toString("hex");
        const vlessUUID = crypto.randomUUID();
        const passFromUrl = params.get("pass");

        const newUser = {
            username: user,
            protocol,
            createdAt: new Date().toISOString(),
            limits: {},
            usage: { total: 0, monthly: {} },
            // 同时生成两种凭据
            password: passFromUrl || hyPass,  // Hy2 密码
            uuid: vlessUUID,                   // VLESS UUID
            sni: sni,
            residential: residential  // v3.5.5: 订阅是否包含 -住宅 后缀节点
        };

        if (days > 0) newUser.limits.expiresAt = new Date(Date.now() + days * 864e5).toISOString();
        if (traffic > 0) newUser.limits.trafficLimit = traffic * 1073741824;
        if (monthly > 0) newUser.limits.monthlyLimit = monthly * 1073741824;
        if (speed > 0) newUser.limits.speedLimit = speed * 1000000;
        users.push(newUser);
        if (saveUsers(users)) return sendJSON(res, { success: true, user: user, password: newUser.password, uuid: vlessUUID, sni: sni });
        return sendJSON(res, { error: "Save failed" }, 500);
    }

    if (action === "delete") {
        if (!user) return sendJSON(res, { error: "Missing user" }, 400);
        const idx = users.findIndex(u => u.username === user);
        if (idx < 0) return sendJSON(res, { error: "User not found" }, 404);
        users.splice(idx, 1);
        if (saveUsers(users)) return sendJSON(res, { success: true });
        return sendJSON(res, { error: "Save failed" }, 500);
    }

    if (action === "update") {
        if (!user) return sendJSON(res, { error: "Missing user" }, 400);
        const u = users.find(x => x.username === user);
        if (!u) return sendJSON(res, { error: "User not found" }, 404);
        const days = params.get("days"), traffic = params.get("traffic"), monthly = params.get("monthly"), pass = params.get("pass");
        if (!u.limits) u.limits = {};
        if (days !== null) u.limits.expiresAt = parseInt(days) > 0 ? new Date(Date.now() + parseInt(days) * 864e5).toISOString() : null;
        if (traffic !== null) u.limits.trafficLimit = parseFloat(traffic) > 0 ? parseFloat(traffic) * 1073741824 : null;
        if (monthly !== null) u.limits.monthlyLimit = parseFloat(monthly) > 0 ? parseFloat(monthly) * 1073741824 : null;
        if (pass) u.password = pass;
        if (saveUsers(users)) return sendJSON(res, { success: true });
        return sendJSON(res, { error: "Save failed" }, 500);
    }

    if (action === "list") return sendJSON(res, users.map(u => ({ username: u.username, limits: u.limits, usage: u.usage })));
    return sendJSON(res, { error: "Unknown action" }, 400);
}

// --- Load HTML from file ---
function loadHTML() {
    try {
        const htmlPath = path.join(ADMIN_DIR, "index.html");
        let html = fs.readFileSync(htmlPath, "utf8");
        html = html.replace(/\${VERSION}/g, VERSION);
        return html;
    } catch (e) {
        log("ERROR", "Failed to load index.html: " + e.message);
        return `<!DOCTYPE html><html><body><h1>Error loading panel</h1><p>${e.message}</p></body></html>`;
    }
}

// --- 内核同步：从 GitHub 下载最新内核缓存到 packages 目录 ---
const PACKAGES_DIR_SYNC = path.join(path.dirname(ADMIN_DIR), "packages");
const KERNEL_VERSIONS_FILE = path.join(PACKAGES_DIR_SYNC, "versions.json");

// HTTPS GET 辅助函数（支持重定向）
function fetchUrl(url, opts = {}) {
    return new Promise((resolve, reject) => {
        const maxRedirects = opts.maxRedirects || 5;
        const doRequest = (reqUrl, redirectCount) => {
            if (redirectCount > maxRedirects) return reject(new Error("Too many redirects"));
            https.get(reqUrl, {
                headers: { "User-Agent": "B-UI-Server/" + VERSION },
                timeout: opts.timeout || 30000
            }, (res) => {
                if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
                    return doRequest(res.headers.location, redirectCount + 1);
                }
                if (res.statusCode !== 200) {
                    res.resume();
                    return reject(new Error(`HTTP ${res.statusCode}`));
                }
                if (opts.file) {
                    const ws = fs.createWriteStream(opts.file);
                    res.pipe(ws);
                    ws.on("finish", () => { ws.close(); resolve(opts.file); });
                    ws.on("error", reject);
                } else {
                    let data = "";
                    res.on("data", c => data += c);
                    res.on("end", () => resolve(data));
                }
            }).on("error", reject).on("timeout", function() { this.destroy(); reject(new Error("Timeout")); });
        };
        doRequest(url, 0);
    });
}

// 获取 GitHub 最新 Release 信息
async function getGitHubLatest(repo) {
    const data = await fetchUrl(`https://api.github.com/repos/${repo}/releases/latest`, { timeout: 15000 });
    return JSON.parse(data);
}

// 读取已缓存的版本信息
function loadCachedVersions() {
    try {
        if (fs.existsSync(KERNEL_VERSIONS_FILE)) {
            return JSON.parse(fs.readFileSync(KERNEL_VERSIONS_FILE, "utf8"));
        }
    } catch { }
    return {};
}

function saveCachedVersions(versions) {
    try {
        fs.mkdirSync(PACKAGES_DIR_SYNC, { recursive: true });
        fs.writeFileSync(KERNEL_VERSIONS_FILE, JSON.stringify(versions, null, 2));
    } catch (e) { log("ERROR", "保存版本缓存失败: " + e.message); }
}

// 同步单个内核
async function syncOneKernel(name, repo, getAssets, cached) {
    try {
        const release = await getGitHubLatest(repo);
        const version = (release.tag_name || "").replace(/^v/, "").replace(/^app\/v?/, "");
        if (!version || !/^\d+\.\d+/.test(version)) return;

        // 版本未变则跳过
        if (cached[name]?.version === version) {
            log("INFO", `[内核同步] ${name} v${version} 已是最新`);
            return;
        }

        log("INFO", `[内核同步] ${name} 发现新版本 v${version}，开始下载...`);
        const assets = getAssets(release, version);

        for (const { url, filename } of assets) {
            const filePath = path.join(PACKAGES_DIR_SYNC, filename);
            try {
                await fetchUrl(url, { file: filePath, timeout: 120000 });
                // 对非压缩的可执行文件设置权限
                if (!filename.endsWith(".zip") && !filename.endsWith(".tar.gz") && !filename.endsWith(".gz")) {
                    fs.chmodSync(filePath, 0o755);
                }
                log("INFO", `[内核同步] ✓ ${filename}`);
            } catch (e) {
                log("WARN", `[内核同步] ✗ ${filename}: ${e.message}`);
            }
        }

        cached[name] = { version, syncedAt: new Date().toISOString() };
        saveCachedVersions(cached);
        log("INFO", `[内核同步] ${name} v${version} 同步完成`);
    } catch (e) {
        log("WARN", `[内核同步] ${name} 同步失败: ${e.message}`);
    }
}

// 主同步函数
async function syncKernels() {
    log("INFO", "[内核同步] 开始同步内核...");
    fs.mkdirSync(PACKAGES_DIR_SYNC, { recursive: true });
    const cached = loadCachedVersions();

    // Hysteria2
    await syncOneKernel("hysteria2", "apernet/hysteria", (release, ver) => {
        return ["amd64", "arm64"].map(arch => {
            const asset = release.assets?.find(a => a.name === `hysteria-linux-${arch}`) || {};
            return {
                url: asset.browser_download_url || `https://github.com/apernet/hysteria/releases/download/app/v${ver}/hysteria-linux-${arch}`,
                filename: `hysteria-linux-${arch}`
            };
        });
    }, cached);

    // Xray
    await syncOneKernel("xray", "XTLS/Xray-core", (release, ver) => {
        return ["64", "arm64-v8a"].map(arch => {
            const suffix = arch === "64" ? "amd64" : "arm64";
            const asset = release.assets?.find(a => a.name === `Xray-linux-${arch}.zip`) || {};
            return {
                url: asset.browser_download_url || `https://github.com/XTLS/Xray-core/releases/download/v${ver}/Xray-linux-${arch}.zip`,
                filename: `xray-linux-${suffix}.zip`
            };
        });
    }, cached);

    // sing-box
    await syncOneKernel("singbox", "SagerNet/sing-box", (release, ver) => {
        return ["amd64", "arm64"].map(arch => {
            const asset = release.assets?.find(a => a.name === `sing-box-${ver}-linux-${arch}.tar.gz`) || {};
            return {
                url: asset.browser_download_url || `https://github.com/SagerNet/sing-box/releases/download/v${ver}/sing-box-${ver}-linux-${arch}.tar.gz`,
                filename: `sing-box-linux-${arch}.tar.gz`
            };
        });
    }, cached);

    log("INFO", "[内核同步] 同步完成");
}

// 启动时执行一次，之后每 6 小时同步
setTimeout(() => syncKernels().catch(e => log("ERROR", "内核同步异常: " + e.message)), 10000);
setInterval(() => syncKernels().catch(e => log("ERROR", "内核同步异常: " + e.message)), 6 * 60 * 60 * 1000);

// --- Traffic Sync Loop ---
let lastTraffic = {};
setInterval(async () => {
    try {
        // 合并 Hysteria2 和 Xray 的流量统计
        const stats = await getMergedStats();
        let users = loadUsers();
        let changed = false;
        const now = new Date();
        const m = now.toISOString().slice(0, 7);

        for (const [uName, stat] of Object.entries(stats)) {
            const u = users.find(x => x.username === uName);
            if (!u) continue;

            if (!u.usage) u.usage = { total: 0, monthly: {} };
            if (!u.usage.monthly) u.usage.monthly = {};

            const last = lastTraffic[uName] || { tx: 0, rx: 0 };
            const deltaTx = (stat.tx < last.tx) ? stat.tx : (stat.tx - last.tx);
            const deltaRx = (stat.rx < last.rx) ? stat.rx : (stat.rx - last.rx);

            if (deltaTx > 0 || deltaRx > 0) {
                const totalDelta = deltaTx + deltaRx;
                u.usage.total = (u.usage.total || 0) + totalDelta;
                u.usage.monthly[m] = (u.usage.monthly[m] || 0) + totalDelta;
                changed = true;
            }

            lastTraffic[uName] = stat;
        }

        if (changed) {
            try { fs.writeFileSync(CONFIG.usersFile, JSON.stringify(users, null, 2)); }
            catch (e) { log("ERROR", "Save usage: " + e.message); }
        }
    } catch (e) {
        console.error("Traffic sync failed:", e);
    }
}, 10000);

// --- Server Startup ---
const server = http.createServer(async (req, res) => {
    const u = new URL(req.url, `http://${req.headers.host}`), p = u.pathname;

    if (req.method === "OPTIONS") {
        res.writeHead(200, {
            "Access-Control-Allow-Origin": "*",
            "Access-Control-Allow-Methods": "*",
            "Access-Control-Allow-Headers": "*"
        });
        return res.end();
    }

    // Serve static files
    if (p === "/" || p === "/index.html") {
        res.writeHead(200, { "Content-Type": "text/html; charset=utf-8" });
        return res.end(loadHTML());
    }

    if (p === "/style.css") {
        try {
            const css = fs.readFileSync(path.join(ADMIN_DIR, "style.css"), "utf8");
            res.writeHead(200, { "Content-Type": "text/css; charset=utf-8" });
            return res.end(css);
        } catch { return sendJSON(res, { error: "Not found" }, 404); }
    }

    if (p === "/app.js") {
        try {
            const js = fs.readFileSync(path.join(ADMIN_DIR, "app.js"), "utf8");
            res.writeHead(200, { "Content-Type": "application/javascript; charset=utf-8" });
            return res.end(js);
        } catch { return sendJSON(res, { error: "Not found" }, 404); }
    }

    if (p === "/logo.jpg") {
        try {
            const logo = fs.readFileSync(path.join(ADMIN_DIR, "logo.jpg"));
            res.writeHead(200, { "Content-Type": "image/jpeg" });
            return res.end(logo);
        } catch { return sendJSON(res, { error: "Not found" }, 404); }
    }

    // --- 客户端一键安装 (验证 key) ---
    if (p === "/install-client" || p.startsWith("/install-client?")) {
        const key = u.searchParams.get("key");

        if (!key || !verifyInstallKey(key)) {
            res.writeHead(403, { "Content-Type": "text/plain; charset=utf-8" });
            return res.end("# Error: Invalid or missing install key\necho '安装密钥无效或缺失，请从服务端管理面板获取正确的安装命令'\nexit 1\n");
        }

        // 获取服务端域名 (从请求 Host 头获取)
        const host = req.headers.host || "localhost";
        const serverDomain = host.split(":")[0];

        let clientScript = "";

        // 优先使用本地客户端脚本 (服务端更新后立即生效，无 CDN 延迟)
        // BASE_DIR 已定义为 /opt/b-ui，直接使用
        const localClientScript = path.join(BASE_DIR, "b-ui-client.sh");
        const packagesClientScript = path.join(BASE_DIR, "packages", "b-ui-client.sh");

        if (fs.existsSync(localClientScript)) {
            // 方法1: 使用项目根目录的脚本 (推荐，与服务端同步更新)
            clientScript = injectClientVersion(fs.readFileSync(localClientScript, "utf8"));
            console.log(`[install-client] 使用本地脚本: ${localClientScript}`);
        } else if (fs.existsSync(packagesClientScript)) {
            // 方法2: 使用 packages 目录的脚本
            clientScript = injectClientVersion(fs.readFileSync(packagesClientScript, "utf8"));
            console.log(`[install-client] 使用 packages 脚本: ${packagesClientScript}`);
        } else {
            // 方法3: 从 GitHub 获取 (备用)
            console.log("[install-client] 本地脚本不存在，尝试从 GitHub 获取...");
            const GITHUB_RAW = "https://raw.githubusercontent.com/Buxiulei/b-ui/main";

            const https = require("https");
            const fetchPromise = new Promise((resolve) => {
                https.get(`${GITHUB_RAW}/b-ui-client.sh`, { timeout: 10000 }, (resp) => {
                    if (resp.statusCode === 200) {
                        let data = "";
                        resp.on("data", chunk => data += chunk);
                        resp.on("end", () => resolve(data));
                    } else {
                        resolve(null);
                    }
                }).on("error", () => resolve(null));
            });

            fetchPromise.then(githubScript => {
                if (githubScript) {
                    clientScript = injectClientVersion(githubScript);
                } else {
                    clientScript = generateBootstrapScript(serverDomain, key);
                }
                sendClientScript(res, serverDomain, key, clientScript);
            });
            return;
        }

        // 发送客户端脚本
        sendClientScript(res, serverDomain, key, clientScript);
        return;
    }

    // 辅助函数：发送客户端脚本
    function sendClientScript(res, serverDomain, key, clientScript) {
        const injectedScript = `#!/bin/bash
# B-UI 客户端一键安装脚本
# 服务端: ${serverDomain}
# 安装时间: $(date)

# ====== 安装前环境准备 ======

# 1. 清理代理环境变量 (防止后续 curl/wget 走死代理)
unset http_proxy https_proxy HTTP_PROXY HTTPS_PROXY all_proxy ALL_PROXY no_proxy NO_PROXY 2>/dev/null || true
export http_proxy="" https_proxy="" HTTP_PROXY="" HTTPS_PROXY=""

# 2. 停止旧的客户端服务 (避免端口冲突)
for svc in hysteria-client xray-client sing-box-tun sing-box-client; do
    systemctl stop "\$svc" 2>/dev/null || true
    systemctl disable "\$svc" 2>/dev/null || true
done

# 3. 预设服务端地址
export BUI_SERVER="${serverDomain}"
export BUI_INSTALL_KEY="${key}"

# 4. 创建配置目录并保存服务端地址
mkdir -p /opt/hysteria-client
echo "${serverDomain}" > /opt/hysteria-client/server_address

# ====== 加载客户端脚本 ======

${clientScript.replace(/^#!\/bin\/bash\s*\n?/, "")}
`;

        res.writeHead(200, {
            "Content-Type": "text/x-shellscript; charset=utf-8",
            "Content-Disposition": "inline; filename=\"b-ui-client-install.sh\""
        });
        res.end(injectedScript);
    }

    // --- 客户端安装包下载 (无需认证) ---
    const PACKAGES_DIR = path.join(path.dirname(ADMIN_DIR), "packages");

    if (p === "/packages" || p === "/packages/") {
        // 列出可用的安装包
        try {
            const files = fs.readdirSync(PACKAGES_DIR);
            const packages = files.map(f => {
                const stat = fs.statSync(path.join(PACKAGES_DIR, f));
                return { name: f, size: stat.size, modified: stat.mtime };
            });
            return sendJSON(res, { packages });
        } catch {
            return sendJSON(res, { packages: [], message: "No packages available" });
        }
    }

    if (p.startsWith("/packages/")) {
        const fileName = decodeURIComponent(p.slice(10));

        // 安全检查：禁止路径遍历字符
        if (fileName.includes("..") || fileName.includes("/") || fileName.includes("\\")) {
            return sendJSON(res, { error: "Invalid filename" }, 400);
        }

        // 对于 b-ui-client.sh，优先使用根目录的版本
        let filePath;
        if (fileName === "b-ui-client.sh") {
            const rootClientScript = path.join(BASE_DIR, "b-ui-client.sh");
            if (fs.existsSync(rootClientScript)) {
                filePath = rootClientScript;
            } else {
                filePath = path.join(PACKAGES_DIR, fileName);
            }
        } else {
            filePath = path.join(PACKAGES_DIR, fileName);
        }

        // 安全检查：确保路径在 BASE_DIR 内
        const resolvedPath = path.resolve(filePath);
        if (!resolvedPath.startsWith(path.resolve(BASE_DIR))) {
            return sendJSON(res, { error: "Invalid path" }, 400);
        }

        try {
            if (fs.existsSync(filePath) && fs.statSync(filePath).isFile()) {
                let content = fs.readFileSync(filePath);
                const ext = path.extname(fileName).toLowerCase();
                let contentType = "application/octet-stream";
                if (ext === ".sh") contentType = "text/x-shellscript";
                else if (ext === ".json") contentType = "application/json";
                else if (ext === ".zip") contentType = "application/zip";
                else if (ext === ".gz" || ext === ".tar.gz") contentType = "application/gzip";

                if (fileName === "b-ui-client.sh") {
                    content = Buffer.from(injectClientVersion(content.toString("utf8")), "utf8");
                }

                res.writeHead(200, {
                    "Content-Type": contentType,
                    "Content-Length": content.length,
                    "Content-Disposition": `attachment; filename="${fileName}"`
                });
                return res.end(content);
            }
            return sendJSON(res, { error: "File not found" }, 404);
        } catch (e) {
            return sendJSON(res, { error: "Download failed: " + e.message }, 500);
        }
    }

    // API routes
    if (p.startsWith("/api/")) {
        const r = p.slice(5);
        const clientIP = getClientIP(req);

        try {
            if (r === "login" && req.method === "POST") {
                const b = await parseBody(req);
                if (!checkRateLimit(clientIP)) {
                    recordAttempt(clientIP, false);
                    return sendJSON(res, { error: "Too many attempts. Try again later." }, 429);
                }
                const ok = b.password === CONFIG.adminPassword;
                recordAttempt(clientIP, ok);
                if (ok) return sendJSON(res, { token: genToken({ admin: true }) });
                else return sendJSON(res, { error: "Auth failed" }, 401);
            }

            if (r === "manage") return handleManage(u.searchParams, res);
            // 客户端版本查询 API (无需认证, 供客户端检查更新)
            if (r === "version") {
                try {
                    const versionFile = path.join(BASE_DIR, "version.json");
                    if (fs.existsSync(versionFile)) {
                        const data = JSON.parse(fs.readFileSync(versionFile, "utf8"));
                        return sendJSON(res, data);
                    }
                    return sendJSON(res, { version: CONFIG.scriptVersion || "0.0.0" });
                } catch (e) {
                    return sendJSON(res, { version: "0.0.0" });
                }
            }

            // 内核版本查询 API (无需认证, 供客户端检查更新)
            if (r === "kernel-versions") {
                const versions = {};
                try {
                    const hyVer = execSync("hysteria version 2>/dev/null | grep '^Version:' | awk '{print $2}' | sed 's/^v//'", { encoding: "utf8", timeout: 5000 }).trim();
                    if (hyVer) versions.hysteria2 = hyVer;
                } catch { }
                try {
                    const xrayVer = execSync("xray version 2>/dev/null | head -n1 | awk '{print $2}' | sed 's/^v//'", { encoding: "utf8", timeout: 5000 }).trim();
                    if (xrayVer) versions.xray = xrayVer;
                } catch { }
                try {
                    const sbVer = execSync("sing-box version 2>/dev/null | head -n1 | awk '{print $3}' | sed 's/^v//'", { encoding: "utf8", timeout: 5000 }).trim();
                    if (sbVer) versions.singbox = sbVer;
                } catch { }
                return sendJSON(res, versions);
            }

            // 内核下载信息 API (供客户端优先从服务端下载内核, 无需认证)
            if (r === "kernel-downloads") {
                try {
                    const cached = loadCachedVersions();
                    const result = {};
                    const kernelNames = ["hysteria2", "xray", "singbox"];
                    for (const name of kernelNames) {
                        const info = cached[name];
                        if (!info) continue;
                        // 兼容旧版平面格式 ("hysteria2": "2.7.1") 和新版嵌套格式
                        const version = typeof info === "string" ? info : info.version;
                        const syncedAt = typeof info === "object" ? info.syncedAt : cached.updated;
                        if (!version) continue;
                        result[name] = { version, syncedAt, files: {} };
                        for (const arch of ["amd64", "arm64"]) {
                            let filename;
                            if (name === "hysteria2") filename = `hysteria-linux-${arch}`;
                            else if (name === "xray") filename = `xray-linux-${arch}.zip`;
                            else if (name === "singbox") filename = `sing-box-linux-${arch}.tar.gz`;
                            if (filename && fs.existsSync(path.join(PACKAGES_DIR_SYNC, filename))) {
                                result[name].files[arch] = `/packages/${filename}`;
                            }
                        }
                    }
                    return sendJSON(res, result);
                } catch (e) {
                    log("ERROR", "kernel-downloads API error: " + e.message);
                    return sendJSON(res, {});
                }
            }

            // 安装命令 API (无需认证)
            if (r === "install-command") {
                const host = req.headers.host || "localhost";
                const key = getOrCreateInstallKey();
                const command = `bash <(curl -fsSL -k --noproxy '*' 'https://${host}/install-client?key=${key}')`;
                return sendJSON(res, {
                    command,
                    key,
                    server: host,
                    note: "此命令仅在此服务端有效，请勿泄露给不信任的人"
                });
            }

            // sing-box 订阅 API (Hy2 + VLESS 融合配置)
            if (r.startsWith("subscription/")) {
                const username = decodeURIComponent(r.slice(13));
                const users = loadUsers();
                const user = users.find(u => u.username === username);
                if (!user) return sendJSON(res, { error: "User not found" }, 404);

                const cfg = getConfig();
                const host = cfg.domain;  // 使用配置中的域名

                // 构建 sing-box 配置
                const singboxConfig = generateSingboxConfig(user, cfg, host);

                // 使用 ASCII 安全的文件名
                const safeFilename = encodeURIComponent(username).replace(/%/g, "_") + ".json";
                res.writeHead(200, {
                    "Content-Type": "application/json; charset=utf-8",
                    "Content-Disposition": `inline; filename="${safeFilename}"`
                });
                return res.end(JSON.stringify(singboxConfig, null, 2));
            }



            // v2rayN 原生订阅 API — v3.5.0 输出 4 个节点 (Reality直连/Reality住宅/HY2直连/HY2住宅)
            if (r.startsWith("sub/")) {
                const username = decodeURIComponent(r.slice(4));
                const users = loadUsers();
                const user = users.find(u => u.username === username);
                if (!user) return sendJSON(res, { error: "User not found" }, 404);

                const cfg = getConfig();
                // v3.5.8: host 优先用域名 (cfg.domain)；域名缺失才 fallback IP。
                // 客户端 (b-ui-client.sh / singbox-tun.json) 已用 DoH + predefined-rule 防 GFW DNS 投毒
                const serverIp = getServerIP();
                const serverHost = (cfg.domain && cfg.domain !== "localhost") ? cfg.domain : serverIp;
                const userSni = user.sni || cfg.sni || "www.bing.com";
                const links = [];

                // Reality 公共参数生成器（直连和住宅版只差端口 + fragment）
                const buildVlessUrl = (port, label) => {
                    if (!user.uuid || !cfg.pubKey || !cfg.shortId) return null;
                    const vlessParams = [
                        `security=reality`,
                        `encryption=none`,
                        `pbk=${cfg.pubKey}`,
                        `headerType=`,
                        `fp=chrome`,
                        `spx=%2F`,
                        `type=tcp`,
                        `flow=xtls-rprx-vision`,
                        `sni=${userSni}`,
                        `sid=${cfg.shortId}`
                    ].join('&');
                    const name = encodeURIComponent(`${user.username}-${label}`);
                    return `vless://${user.uuid}@${serverHost}:${port}?${vlessParams}#${name}`;
                };

                // v3.5.5/3.5.6: per-user 住宅开关 + protocol 决定节点数
                // - fusion: 默认 4 URL，residential=false 时仅 2 URL (-直连)
                // - hysteria2 / vless-reality: 单节点 URL，residential 决定直连版 vs 住宅版
                // - vless-ws-tls: 单独走 ws-tls inbound（不在此 4-URL 框架内）
                const includeResi = user.residential !== false;
                const proto = user.protocol || "fusion";

                // ① Reality URL (按 proto + residential 决定端口)
                if (proto === "fusion" || proto === "vless-reality") {
                    // fusion: 直连版必给
                    if (proto === "fusion") {
                        const r1 = buildVlessUrl(10001, "Reality直连");
                        if (r1) links.push(r1);
                    }
                    // vless-reality 单协议: 按 residential 选端口
                    if (proto === "vless-reality") {
                        const port = includeResi ? 10002 : 10001;
                        const label = includeResi ? "Reality住宅" : "Reality直连";
                        const u = buildVlessUrl(port, label);
                        if (u) links.push(u);
                    }
                    // fusion: 住宅版可选
                    if (proto === "fusion" && includeResi) {
                        const r2 = buildVlessUrl(10002, "Reality住宅");
                        if (r2) links.push(r2);
                    }
                }

                // HY2 公共参数生成器
                const buildHy2Url = (port, hopRange, label, includeObfs) => {
                    if (!user.password) return null;
                    // v3.5.0: 分段 encode（防止 : 被编码导致客户端无法拆分 user/pass）
                    const auth = `${encodeURIComponent(user.username)}:${encodeURIComponent(user.password)}`;
                    let qp = `sni=${serverHost}&insecure=0&mport=${hopRange}`;
                    if (includeObfs && cfg.obfs && cfg.obfs.enabled && cfg.obfs.type === "salamander" && cfg.obfs.password) {
                        qp += `&obfs=salamander&obfs-password=${encodeURIComponent(cfg.obfs.password)}`;
                    }
                    const name = encodeURIComponent(`${user.username}-${label}`);
                    return `hysteria2://${auth}@${serverHost}:${port}?${qp}#${name}`;
                };

                // ② HY2 URL (按 proto + residential 决定端口)
                if (proto === "fusion" || proto === "hysteria2") {
                    if (proto === "fusion") {
                        const h1 = buildHy2Url(cfg.port || 10000, "20000-30000", "HY2直连", true);
                        if (h1) links.push(h1);
                    }
                    if (proto === "hysteria2") {
                        if (includeResi) {
                            const u = buildHy2Url(40000, "41000-50000", "HY2住宅", false);
                            if (u) links.push(u);
                        } else {
                            const u = buildHy2Url(cfg.port || 10000, "20000-30000", "HY2直连", true);
                            if (u) links.push(u);
                        }
                    }
                    if (proto === "fusion" && includeResi) {
                        const h2 = buildHy2Url(40000, "41000-50000", "HY2住宅", false);
                        if (h2) links.push(h2);
                    }
                }

                const base64Content = Buffer.from(links.join("\n")).toString("base64");

                res.writeHead(200, {
                    "Content-Type": "text/plain; charset=utf-8",
                    "profile-title": `base64:${Buffer.from(user.username).toString('base64')}`,
                    "profile-update-interval": "24",
                    "Subscription-Userinfo": `upload=0; download=0; total=${user.limits?.trafficLimit || 0}; expire=${user.limits?.expiresAt ? new Date(user.limits.expiresAt).getTime() / 1000 : 0}`
                });
                return res.end(base64Content);
            }

            // Clash Meta (mihomo) 订阅 API - Clash Verge Rev 兼容
            if (r.startsWith("clash/")) {
                const username = decodeURIComponent(r.slice(6));
                const users = loadUsers();
                const user = users.find(u => u.username === username);
                if (!user) return sendJSON(res, { error: "User not found" }, 404);

                const cfg = getConfig();
                const host = cfg.domain;

                const clashYaml = generateClashConfig(user, cfg, host);

                res.writeHead(200, {
                    "Content-Type": "text/yaml; charset=utf-8",
                    "Content-Disposition": `inline; filename*=UTF-8''${encodeURIComponent(user.username)}.yaml`,
                    "profile-title": `base64:${Buffer.from(user.username).toString('base64')}`,
                    "profile-update-interval": "24",
                    "Subscription-Userinfo": `upload=0; download=0; total=${user.limits?.trafficLimit || 0}; expire=${user.limits?.expiresAt ? new Date(user.limits.expiresAt).getTime() / 1000 : 0}`
                });
                return res.end(clashYaml);
            }

            const auth = verifyToken((req.headers.authorization || "").replace("Bearer ", ""));
            if (!auth) return sendJSON(res, { error: "Unauthorized" }, 401);

            if (r === "users") {
                if (req.method === "GET") return sendJSON(res, loadUsers());
                if (req.method === "POST") {
                    const b = await parseBody(req);
                    if (b && b.__empty) return sendJSON(res, { error: "缺少请求体" }, 400);
                    if (b && b.__invalidJson) return sendJSON(res, { error: "请求格式错误（JSON 解析失败）" }, 400);
                    if (!b || typeof b !== "object" || Array.isArray(b)) {
                        return sendJSON(res, { error: "请求格式错误（需要 JSON 对象）" }, 400);
                    }
                    const unameErr = validateUsername(b.username);
                    if (unameErr) return sendJSON(res, { error: unameErr }, 400);
                    const pwdErr = validatePassword(b.password);
                    if (pwdErr) return sendJSON(res, { error: pwdErr }, 400);

                    const users = loadUsers();
                    if (users.find(u => u.username === b.username)) return sendJSON(res, { error: "用户名已存在" }, 400);
                    users.push({
                        username: b.username,
                        password: b.password,
                        createdAt: new Date()
                    });
                    return saveUsers(users) ? sendJSON(res, { success: true }) : sendJSON(res, { error: "Save failed" }, 500);
                }
            }

            if (r.startsWith("users/") && req.method === "DELETE") {
                let users = loadUsers();
                users = users.filter(u => u.username !== decodeURIComponent(r.slice(6)));
                return saveUsers(users) ? sendJSON(res, { success: true }) : sendJSON(res, { error: "Fail" }, 500);
            }

            // Update user (PUT /api/users/:username)
            if (r.startsWith("users/") && req.method === "PUT") {
                const origUsername = decodeURIComponent(r.slice(6));
                // path traversal / 非法字符校验
                const pathErr = validateUsername(origUsername);
                if (pathErr) return sendJSON(res, { error: "URL 中的 " + pathErr }, 400);

                const b = await parseBody(req);
                if (b && b.__empty) return sendJSON(res, { error: "缺少请求体" }, 400);
                if (b && b.__invalidJson) return sendJSON(res, { error: "请求格式错误（JSON 解析失败）" }, 400);
                if (!b || typeof b !== "object" || Array.isArray(b)) {
                    return sendJSON(res, { error: "请求格式错误（需要 JSON 对象）" }, 400);
                }

                let users = loadUsers();
                const userIndex = users.findIndex(u => u.username === origUsername);

                if (userIndex < 0) {
                    return sendJSON(res, { error: "User not found" }, 404);
                }

                const user = users[userIndex];

                // 更新用户名
                if (b.username && b.username !== origUsername) {
                    const newUnameErr = validateUsername(b.username);
                    if (newUnameErr) return sendJSON(res, { error: newUnameErr }, 400);
                    // 检查新用户名是否已存在
                    if (users.find(u => u.username === b.username)) {
                        return sendJSON(res, { error: "Username already exists" }, 400);
                    }
                    user.username = b.username;
                }

                // 更新密码/UUID
                if (typeof b.password !== "undefined") {
                    const pwdErr = validatePassword(b.password);
                    if (pwdErr) return sendJSON(res, { error: pwdErr }, 400);
                    if (user.protocol === "vless-reality" || user.protocol === "vless-ws-tls") {
                        user.uuid = b.password;
                    } else {
                        user.password = b.password;
                    }
                }

                // 更新限制
                if (!user.limits) user.limits = {};

                // 有效期 (天数)
                if (typeof b.days !== 'undefined') {
                    if (b.days > 0) {
                        user.limits.expiresAt = new Date(Date.now() + b.days * 86400000).toISOString();
                    } else {
                        delete user.limits.expiresAt;
                    }
                }

                // 总流量限制 (GB)
                if (typeof b.traffic !== 'undefined') {
                    if (b.traffic > 0) {
                        user.limits.trafficLimit = b.traffic * 1073741824;
                    } else {
                        delete user.limits.trafficLimit;
                    }
                }

                // 月流量限制 (GB)
                if (typeof b.monthly !== 'undefined') {
                    if (b.monthly > 0) {
                        user.limits.monthlyLimit = b.monthly * 1073741824;
                    } else {
                        delete user.limits.monthlyLimit;
                    }
                }

                // 速度限制 (Mbps)
                if (typeof b.speed !== 'undefined') {
                    if (b.speed > 0) {
                        user.limits.speedLimit = b.speed * 1000000;
                    } else {
                        delete user.limits.speedLimit;
                    }
                }

                users[userIndex] = user;

                if (saveUsers(users)) {
                    return sendJSON(res, { success: true, user: user.username });
                }
                return sendJSON(res, { error: "Save failed" }, 500);
            }

            if (r === "stats") return sendJSON(res, await getMergedStats());
            if (r === "online") return sendJSON(res, await getMergedOnline());
            if (r === "kick" && req.method === "POST") return sendJSON(res, await postStats("/kick", await parseBody(req)));
            if (r === "config") return sendJSON(res, getConfig());

            // Port Hopping settings API
            if (r === "port-hopping") {
                const phFile = path.join(BASE_DIR, "port-hopping.json");

                if (req.method === "GET") {
                    try {
                        if (fs.existsSync(phFile)) {
                            const ph = JSON.parse(fs.readFileSync(phFile, "utf8"));
                            return sendJSON(res, {
                                enabled: ph.enabled || false,
                                start: ph.startPort || 20000,
                                end: ph.endPort || 30000
                            });
                        }
                        return sendJSON(res, { enabled: false, start: 20000, end: 30000 });
                    } catch {
                        return sendJSON(res, { enabled: false, start: 20000, end: 30000 });
                    }
                }

                if (req.method === "POST") {
                    const b = await parseBody(req);
                    const enabled = !!b.enabled;
                    const start = parseInt(b.start) || 20000;
                    const end = parseInt(b.end) || 30000;

                    if (start >= end) return sendJSON(res, { error: "起始端口必须小于结束端口" }, 400);

                    try {
                        // 获取 Hysteria 监听端口
                        let listenPort = 10000;
                        try {
                            const hyc = fs.readFileSync(CONFIG.hysteriaConfig, "utf8");
                            const pm = hyc.match(/listen:\s*:(\d+)/);
                            if (pm) listenPort = parseInt(pm[1]);
                        } catch { }

                        // 保存配置
                        const phConfig = {
                            enabled,
                            startPort: start,
                            endPort: end,
                            listenPort
                        };
                        fs.writeFileSync(phFile, JSON.stringify(phConfig, null, 2));

                        // 执行 iptables 规则更新 (调用 shell 脚本)
                        if (enabled) {
                            // 启用端口跳跃：添加 iptables 规则
                            const cmd = `
                                iface=$(ip route get 8.8.8.8 2>/dev/null | grep -oP 'dev \\K\\S+' | head -1)
                                [ -z "$iface" ] && iface="eth0"
                                # 清理旧规则
                                rule_nums=$(iptables -t nat -L PREROUTING --line-numbers -n 2>/dev/null | grep "Hysteria2-PortHopping" | awk '{print $1}' | sort -rn)
                                for num in $rule_nums; do iptables -t nat -D PREROUTING $num 2>/dev/null || true; done
                                # 添加新规则
                                iptables -t nat -A PREROUTING -i "$iface" -p udp --dport ${start}:${end} -m comment --comment "Hysteria2-PortHopping" -j REDIRECT --to-ports ${listenPort}
                                # 持久化
                                mkdir -p /etc/iptables
                                iptables-save > /etc/iptables/rules.v4 2>/dev/null || true
                            `;
                            execSync(cmd, { shell: "/bin/bash", stdio: "pipe" });
                            log("INFO", `Port hopping enabled: ${start}-${end} -> ${listenPort}`);
                        } else {
                            // 禁用端口跳跃：删除 iptables 规则
                            const cmd = `
                                rule_nums=$(iptables -t nat -L PREROUTING --line-numbers -n 2>/dev/null | grep "Hysteria2-PortHopping" | awk '{print $1}' | sort -rn)
                                for num in $rule_nums; do iptables -t nat -D PREROUTING $num 2>/dev/null || true; done
                                iptables-save > /etc/iptables/rules.v4 2>/dev/null || true
                            `;
                            execSync(cmd, { shell: "/bin/bash", stdio: "pipe" });
                            log("INFO", "Port hopping disabled");
                        }

                        return sendJSON(res, { success: true, enabled, start, end });
                    } catch (e) {
                        return sendJSON(res, { error: e.message }, 500);
                    }
                }
            }

            if (r === "residential") {
                const helperPath = CONFIG.residentialHelper;
                const DEFAULT_DOMAINS = ["openai","chatgpt","google","googleapis","gstatic","anthropic","claude","ping0","ip.sb","ip-api"];

                if (req.method === "GET") {
                    try {
                        const raw = fs.existsSync(CONFIG.residentialConfig)
                            ? JSON.parse(fs.readFileSync(CONFIG.residentialConfig, "utf8"))
                            : { enabled: false };
                        const display = { ...raw };
                        if (!display.domains || !display.domains.length) display.domains = DEFAULT_DOMAINS;
                        // v3.5.0: 保证 global 字段始终在响应里
                        if (typeof display.global === "undefined") display.global = false;
                        // v3.5.0: 保证 urls 数组始终在响应里（多 URL CRUD 支持）
                        if (!Array.isArray(display.urls)) {
                            display.urls = display.host
                                ? [{ host: display.host, port: display.port, username: display.username, name: "primary" }]
                                : [];
                        } else {
                            // 不暴露明文密码
                            display.urls = display.urls.map(u => ({ host: u.host, port: u.port, username: u.username, name: u.name }));
                        }
                        if (display.password) display.password = display.password.slice(0, 2) + "***";
                        if (display.username && display.host) {
                            display.displayUrl = `socks5://${display.username.slice(0, 2)}***@${display.host}:${display.port}`;
                        }
                        return sendJSON(res, display);
                    } catch {
                        return sendJSON(res, { enabled: false, domains: DEFAULT_DOMAINS });
                    }
                }

                if (req.method === "POST") {
                    const b = await parseBody(req);

                    // domains-only update (already enabled, just change routing)
                    if (!b.url && b.domains) {
                        try {
                            const result = spawnSync(helperPath, ["set-domains", JSON.stringify(b.domains)], {
                                env: { ...process.env, BASE_DIR },
                                encoding: "utf8",
                                timeout: 15000,
                            });
                            if (result.status !== 0) {
                                const errMsg = (result.stderr || "").replace(/\x1b\[[0-9;]*m/g, "").trim();
                                return sendJSON(res, { error: errMsg || "分流域名更新失败" }, 400);
                            }
                            return sendJSON(res, { success: true });
                        } catch (e) {
                            return sendJSON(res, { error: e.message }, 500);
                        }
                    }

                    if (!b.url) return sendJSON(res, { error: "url 字段必填" }, 400);

                    // set domains first if provided, then enable
                    if (b.domains) {
                        spawnSync(helperPath, ["set-domains", JSON.stringify(b.domains)], {
                            env: { ...process.env, BASE_DIR },
                            encoding: "utf8",
                            timeout: 10000,
                        });
                    }

                    try {
                        const result = spawnSync(helperPath, ["enable", b.url], {
                            env: { ...process.env, BASE_DIR },
                            encoding: "utf8",
                            timeout: 30000,
                        });
                        if (result.status !== 0) {
                            const errMsg = (result.stderr || "").replace(/\x1b\[[0-9;]*m/g, "").trim();
                            return sendJSON(res, { error: errMsg || "住宅 IP 启用失败" }, 400);
                        }
                        const lines = result.stdout.trim().split("\n");
                        return sendJSON(res, { success: true, exitIp: lines[0] || "", ispInfo: lines[1] || "" });
                    } catch (e) {
                        return sendJSON(res, { error: e.message }, 500);
                    }
                }

                if (req.method === "DELETE") {
                    try {
                        const result = spawnSync(helperPath, ["disable"], {
                            env: { ...process.env, BASE_DIR },
                            encoding: "utf8",
                            timeout: 15000,
                        });
                        if (result.status !== 0) {
                            return sendJSON(res, { error: (result.stderr || "禁用失败").trim() }, 500);
                        }
                        return sendJSON(res, { success: true });
                    } catch (e) {
                        return sendJSON(res, { error: e.message }, 500);
                    }
                }
            }

            // v3.5.3: POST /api/residential/enable — 总开关启用（池非空时调 helper reapply 设 enabled=true）
            if (r === "residential/enable" && req.method === "POST") {
                try {
                    let rConfig = { enabled: false, urls: [] };
                    if (fs.existsSync(CONFIG.residentialConfig)) {
                        rConfig = JSON.parse(fs.readFileSync(CONFIG.residentialConfig, "utf8"));
                    }
                    if (!Array.isArray(rConfig.urls) || rConfig.urls.length === 0) {
                        return sendJSON(res, { error: "代理节点池为空，请先添加至少 1 个住宅 URL" }, 400);
                    }
                    rConfig.enabled = true;
                    fs.writeFileSync(CONFIG.residentialConfig, JSON.stringify(rConfig, null, 2));
                    const result = spawnSync(CONFIG.residentialHelper, ["reapply"], {
                        env: { ...process.env, BASE_DIR },
                        encoding: "utf8",
                        timeout: 15000,
                    });
                    if (result.status !== 0) {
                        const errMsg = (result.stderr || "").replace(/\x1b\[[0-9;]*m/g, "").trim();
                        return sendJSON(res, { error: errMsg || "reapply 失败" }, 500);
                    }
                    return sendJSON(res, { success: true });
                } catch (e) { return sendJSON(res, { error: e.message }, 500); }
            }

            // v3.5.0: POST /api/residential/global — 切换全局/分流模式
            if (r === "residential/global" && req.method === "POST") {
                const b = await parseBody(req);
                if (!b || typeof b.global !== "boolean") {
                    return sendJSON(res, { error: "body 需要 { global: true|false }" }, 400);
                }
                try {
                    const result = spawnSync(CONFIG.residentialHelper, ["global", b.global ? "on" : "off"], {
                        env: { ...process.env, BASE_DIR },
                        encoding: "utf8",
                        timeout: 15000,
                    });
                    if (result.status !== 0) {
                        const errMsg = (result.stderr || "").replace(/\x1b\[[0-9;]*m/g, "").trim();
                        return sendJSON(res, { error: errMsg || "global toggle 失败" }, 500);
                    }
                    return sendJSON(res, { success: true, global: b.global });
                } catch (e) {
                    return sendJSON(res, { error: e.message }, 500);
                }
            }

            // v3.5.0: POST /api/residential/urls — 追加一个住宅 socks5 URL
            if (r === "residential/urls" && req.method === "POST") {
                const b = await parseBody(req);
                if (!b || typeof b.url !== "string" || !b.url.startsWith("socks5://")) {
                    return sendJSON(res, { error: "url 必须是 socks5:// 开头" }, 400);
                }
                try {
                    const result = spawnSync(CONFIG.residentialHelper, ["enable", "--add", b.url], {
                        env: { ...process.env, BASE_DIR },
                        encoding: "utf8",
                        timeout: 30000,
                    });
                    if (result.status !== 0) {
                        const errMsg = (result.stderr || "").replace(/\x1b\[[0-9;]*m/g, "").trim();
                        return sendJSON(res, { error: errMsg || "添加 URL 失败" }, 400);
                    }
                    const lines = (result.stdout || "").trim().split("\n");
                    return sendJSON(res, { success: true, exitIp: lines[0] || "", ispInfo: lines[1] || "" });
                } catch (e) {
                    return sendJSON(res, { error: e.message }, 500);
                }
            }

            // v3.5.0: DELETE /api/residential/urls/:host:port — 移除指定 URL
            if (r.startsWith("residential/urls/") && req.method === "DELETE") {
                const target = decodeURIComponent(r.slice("residential/urls/".length));
                // target 格式: "host:port"
                try {
                    let rConfig = { urls: [] };
                    if (fs.existsSync(CONFIG.residentialConfig)) {
                        rConfig = JSON.parse(fs.readFileSync(CONFIG.residentialConfig, "utf8"));
                    }
                    if (!Array.isArray(rConfig.urls)) rConfig.urls = [];
                    const match = rConfig.urls.find(u => `${u.host}:${u.port}` === target);
                    if (!match) return sendJSON(res, { error: "未找到匹配 URL" }, 404);
                    // helper 期望 socks5:// 格式（不需要凭据匹配，按 host:port 即可）
                    const url = `socks5://${match.username}:${match.password}@${match.host}:${match.port}`;
                    const result = spawnSync(CONFIG.residentialHelper, ["enable", "--remove", url], {
                        env: { ...process.env, BASE_DIR },
                        encoding: "utf8",
                        timeout: 15000,
                    });
                    if (result.status !== 0) {
                        const errMsg = (result.stderr || "").replace(/\x1b\[[0-9;]*m/g, "").trim();
                        return sendJSON(res, { error: errMsg || "移除 URL 失败" }, 500);
                    }
                    return sendJSON(res, { success: true });
                } catch (e) {
                    return sendJSON(res, { error: e.message }, 500);
                }
            }

            // 住宅 IP 健康检查 - 返回当前出口 IP / ISP / IP 类型
            // 不暴露 socks5 凭据；通过本地 socks5 出口实测 ping0.cc
            if (r === "residential/health" && req.method === "GET") {
                let raw = { enabled: false };
                try {
                    if (fs.existsSync(CONFIG.residentialConfig)) {
                        raw = JSON.parse(fs.readFileSync(CONFIG.residentialConfig, "utf8"));
                    }
                } catch { }

                // 优先读 sing-box 实际生效配置（source of truth）；fallback 用 residential-proxy.json
                let domainsList = [];
                try {
                    const sb = JSON.parse(fs.readFileSync("/opt/b-ui/singbox-relay.json", "utf8"));
                    const rules = sb?.route?.rules || [];
                    for (const rl of rules) {
                        if (Array.isArray(rl.domain_keyword) && rl.domain_keyword.length > domainsList.length) {
                            domainsList = rl.domain_keyword;
                        }
                    }
                } catch { }
                if (!domainsList.length && raw.domains && raw.domains.length) {
                    domainsList = raw.domains;
                }
                const safeUrls = [];
                if (raw.enabled && raw.host) {
                    safeUrls.push({
                        host: raw.host,
                        port: raw.port || null,
                        last_verified_at: raw.lastVerifiedAt || raw.last_verified_at || null,
                        last_verified_ip: raw.lastVerifiedIp || raw.last_verified_ip || null,
                        last_verified_isp: raw.lastVerifiedIspInfo || raw.last_verified_isp || null,
                    });
                }

                const baseResp = {
                    enabled: !!raw.enabled,
                    urls: safeUrls,
                    domains_count: domainsList.length,
                    current_egress_ip_test: null,
                    egress_ip_type: "unknown",
                    via_proxy_isp: null,
                };

                if (!raw.enabled) {
                    return sendJSON(res, baseResp);
                }

                // v3.4.22 关键改进：用 ip-api.com 的 hosting 字段（权威判断）替代 ASN 关键词正则
                // ip-api.com 的 hosting=true/false 综合多个数据库 + IP 行为分析，比单纯按 ASN 名称推断准确得多
                //   - hosting:false + proxy:false + mobile:false → 家庭宽带 IP（真 residential）
                //   - hosting:true → IDC机房 IP
                //   - mobile:true → 移动网络 IP
                //   - proxy:true → 代理/匿名 IP
                // 通过 sing-box socks5 中继访问（127.0.0.1:2080），ip-api.com 不在分流关键词列表，
                // 但本 endpoint 测的就是"住宅链路是否通"——所以**直接拨住宅 socks5**绕开路由
                // 用 ping0.cc/geo 同时拿一份做对照（ping0 命中 keyword 走住宅，是双重验证）
                const socksProxy = (raw.username && raw.password)
                    ? `socks5://${encodeURIComponent(raw.username)}:${encodeURIComponent(raw.password)}@${raw.host}:${raw.port}`
                    : `socks5://${raw.host}:${raw.port}`;
                execFile("curl", [
                    "--proxy", socksProxy,
                    "-m", "8",
                    "-sS",
                    "http://ip-api.com/json/?fields=status,country,city,isp,org,as,mobile,proxy,hosting,query"
                ], { timeout: 9000, maxBuffer: 1 * 1024 * 1024 }, (err, stdout) => {
                    if (err || !stdout) {
                        return sendJSON(res, baseResp);
                    }
                    try {
                        const data = JSON.parse(String(stdout));
                        if (data.status !== "success") {
                            return sendJSON(res, baseResp);
                        }
                        const ip = data.query || null;
                        const isp = data.isp || data.org || "";
                        const asn = data.as || "";
                        const country = data.country || "";
                        const city = data.city || "";

                        // 权威类型判断：ip-api 的 hosting / proxy / mobile boolean
                        let egressType = "家庭宽带 IP"; // 默认假设住宅（true residential）
                        if (data.hosting === true) egressType = "IDC机房 IP";
                        else if (data.proxy === true) egressType = "代理 IP";
                        else if (data.mobile === true) egressType = "移动网络 IP";

                        const ispLabel = [asn, isp].filter(Boolean).join(" — ") +
                                         (country ? ` (${city ? city + ', ' : ''}${country})` : '');

                        return sendJSON(res, {
                            ...baseResp,
                            current_egress_ip_test: ip,
                            egress_ip_type: egressType,
                            via_proxy_isp: ispLabel.trim() || null,
                        });
                    } catch {
                        return sendJSON(res, baseResp);
                    }
                });
                return;
            }

            // hy2 watchdog 状态 - 用于 Web 面板系统状态卡片
            if (r === "hy2/watchdog/status" && req.method === "GET") {
                let watchdogActive = false;
                let nextRunAt = null;
                let lastRunAt = null;
                let failCount = 0;
                let logRecentLines = [];

                try {
                    const out = execSync("systemctl list-timers hy2-watchdog.timer --all --no-pager 2>/dev/null || true", { encoding: "utf8", timeout: 5000 });
                    const lines = out.split("\n").filter(l => l.includes("hy2-watchdog"));
                    if (lines.length) {
                        // NEXT          LEFT       LAST          PASSED       UNIT  ACTIVATES
                        const cols = lines[0].trim().split(/\s{2,}/);
                        if (cols.length >= 4) {
                            nextRunAt = cols[0] && cols[0] !== "-" ? cols[0] : null;
                            lastRunAt = cols[2] && cols[2] !== "-" ? cols[2] : null;
                        }
                        watchdogActive = true;
                    }
                } catch { }

                if (!watchdogActive) {
                    try {
                        const isActive = execSync("systemctl is-active hy2-watchdog.timer 2>/dev/null || true", { encoding: "utf8", timeout: 3000 }).trim();
                        if (isActive === "active") watchdogActive = true;
                    } catch { }
                }

                try {
                    const fc = fs.readFileSync("/tmp/hy2-watchdog-fail-count", "utf8").trim();
                    failCount = parseInt(fc, 10) || 0;
                } catch { }

                try {
                    if (fs.existsSync("/var/log/b-ui-hy2-watchdog.log")) {
                        const tail = execSync("tail -n 5 /var/log/b-ui-hy2-watchdog.log 2>/dev/null || true", { encoding: "utf8", timeout: 3000 });
                        logRecentLines = tail.split("\n").filter(Boolean).slice(-5);
                    }
                } catch { }

                return sendJSON(res, {
                    watchdog_active: watchdogActive,
                    next_run_at: nextRunAt,
                    last_run_at: lastRunAt,
                    fail_count: failCount,
                    log_recent_lines: logRecentLines,
                });
            }

            if (r === "masquerade") {
                const masqFile = CONFIG.hysteriaConfig.replace("config.yaml", "masquerade.json");
                if (req.method === "GET") {
                    try {
                        const m = JSON.parse(fs.readFileSync(masqFile, "utf8"));
                        return sendJSON(res, m);
                    } catch {
                        return sendJSON(res, { masqueradeUrl: "https://www.bing.com/", masqueradeDomain: "www.bing.com" });
                    }
                }
                if (req.method === "POST") {
                    const b = await parseBody(req);
                    if (!b.url) return sendJSON(res, { error: "URL required" }, 400);
                    const domain = b.url.replace(/https?:\/\/([^/:]+).*/, "$1") || "www.bing.com";
                    try {
                        fs.writeFileSync(masqFile, JSON.stringify({ masqueradeUrl: b.url, masqueradeDomain: domain }, null, 2));
                        let hyc = fs.readFileSync(CONFIG.hysteriaConfig, "utf8");
                        hyc = hyc.replace(/masquerade:[\s\S]*?(?=\n[a-zA-Z]|$)/, `masquerade:\n  type: proxy\n  proxy:\n    url: ${b.url}\n    rewriteHost: true`);
                        fs.writeFileSync(CONFIG.hysteriaConfig, hyc);
                        if (fs.existsSync(CONFIG.xrayConfig)) {
                            let xc = JSON.parse(fs.readFileSync(CONFIG.xrayConfig, "utf8"));
                            const xi = xc.inbounds.find(i => i.tag === "vless-direct" || i.tag === "vless-reality");
                            if (xi && xi.streamSettings?.realitySettings) {
                                xi.streamSettings.realitySettings.dest = domain + ":443";
                                xi.streamSettings.realitySettings.serverNames = [domain];
                            }
                            fs.writeFileSync(CONFIG.xrayConfig, JSON.stringify(xc, null, 2));
                            execSync("systemctl restart xray 2>/dev/null||true", { stdio: "pipe" });
                        }
                        execSync("systemctl restart hysteria-server 2>/dev/null||true", { stdio: "pipe" });
                        return sendJSON(res, { success: true, domain });
                    } catch (e) { return sendJSON(res, { error: e.message }, 500); }
                }
            }

            if (r === "bandwidth") {
                if (req.method === "GET") {
                    try {
                        const hyc = fs.readFileSync(CONFIG.hysteriaConfig, "utf8");
                        const upMatch = hyc.match(/up:\s*"?(\d+)\s*[Mm]bps"?/);
                        const downMatch = hyc.match(/down:\s*"?(\d+)\s*[Mm]bps"?/);
                        return sendJSON(res, {
                            up: upMatch ? parseInt(upMatch[1]) : 0,
                            down: downMatch ? parseInt(downMatch[1]) : 0
                        });
                    } catch {
                        return sendJSON(res, { up: 0, down: 0 });
                    }
                }
                if (req.method === "POST") {
                    const b = await parseBody(req);
                    const up = parseFloat(b.up) || 0;
                    const down = parseFloat(b.down) || 0;

                    try {
                        let hyc = fs.readFileSync(CONFIG.hysteriaConfig, "utf8");
                        const bwSection = up > 0 && down > 0
                            ? `bandwidth:\n  up: ${up} mbps\n  down: ${down} mbps`
                            : "# bandwidth: unlimited";
                        hyc = hyc.replace(/bandwidth:[\s\S]*?(?=\n[a-zA-Z]|$)/, bwSection);
                        if (!hyc.includes("bandwidth:")) {
                            hyc = hyc.replace(/(listen:.*\n)/, `$1\n${bwSection}\n`);
                        }
                        fs.writeFileSync(CONFIG.hysteriaConfig, hyc);

                        execSync("systemctl restart hysteria-server 2>/dev/null||true", { stdio: "pipe" });
                        return sendJSON(res, { success: true, up, down });
                    } catch (e) {
                        return sendJSON(res, { error: e.message }, 500);
                    }
                }
            }

            if (r === "password" && req.method === "POST") {
                const b = await parseBody(req);
                if (!b.newPassword || b.newPassword.length < 6) return sendJSON(res, { error: "密码至少6位" }, 400);
                try {
                    // 新方案：写 admin.env (chmod 600) 而非 unit Environment=
                    // 注意：systemd EnvironmentFile reload 不会重读，必须 restart
                    const envFile = CONFIG.adminEnvFile;
                    const newPwd = b.newPassword;
                    let envContent = "";
                    if (fs.existsSync(envFile)) {
                        envContent = fs.readFileSync(envFile, "utf8");
                    }
                    const lines = envContent.split("\n");
                    let replaced = false;
                    for (let i = 0; i < lines.length; i++) {
                        if (/^ADMIN_PASSWORD=/.test(lines[i])) {
                            lines[i] = "ADMIN_PASSWORD=" + newPwd;
                            replaced = true;
                            break;
                        }
                    }
                    if (!replaced) {
                        // 移除尾部空行后追加
                        while (lines.length > 0 && lines[lines.length - 1] === "") lines.pop();
                        lines.push("ADMIN_PASSWORD=" + newPwd);
                        lines.push("");
                    }
                    fs.writeFileSync(envFile, lines.join("\n"));
                    try { fs.chmodSync(envFile, 0o600); } catch { }
                    // 同时更新内存中的密码，立即生效（避免重启窗口期）
                    CONFIG.adminPassword = newPwd;
                    // 异步重启 b-ui-admin 让 systemd 重新读取 EnvironmentFile
                    // 不能 reload —— EnvironmentFile 的 reload 不会重读
                    // 用 spawn + detached + unref 避免 self-restart 时阻塞当前响应
                    try {
                        const child = spawn("systemctl", ["restart", "b-ui-admin"], {
                            detached: true,
                            stdio: "ignore"
                        });
                        child.unref();
                    } catch { }
                    return sendJSON(res, { success: true, message: "密码已更新，请重新登录" });
                } catch (e) { return sendJSON(res, { error: e.message }, 500); }
            }
        } catch (e) { return sendJSON(res, { error: e.message }, 500); }
    }

    // Hysteria2 HTTP Auth Endpoint (支持用户级别限速)
    if (p === "/auth/hysteria" && req.method === "POST") {
        const body = await parseBody(req);
        const authStr = body.auth || "";
        const [username, password] = authStr.split(":");
        const users = loadUsers();
        const user = users.find(u => u.username === username && u.password === password);
        if (user) {
            const check = checkUserLimits(user);
            if (check.ok) {
                // 返回用户级别限速 (根据 Hysteria2 HTTP Auth 规范)
                const response = { ok: true, id: username };
                // 如果用户有限速设置，添加到响应中 (bps)
                if (user.limits?.speedLimit) {
                    response.rx = user.limits.speedLimit; // 下载限制
                    response.tx = user.limits.speedLimit; // 上传限制
                }
                log("AUTH", `User ${username} authenticated with speed limit: ${user.limits?.speedLimit || 'unlimited'}`);
                return sendJSON(res, response);
            } else {
                log("AUTH", `User ${username} rejected: ${check.reason}`);
                return sendJSON(res, { ok: false, id: username });
            }
        }
        log("AUTH", `Auth failed for user: ${username}`);
        return sendJSON(res, { ok: false });
    }

    sendJSON(res, { error: "Not found" }, 404);
});

server.listen(CONFIG.port, CONFIG.bind, () => console.log(`B-UI Admin Panel v${VERSION} running on ${CONFIG.bind}:${CONFIG.port}`));
