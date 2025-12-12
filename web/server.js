const http = require("http");
const fs = require("fs");
const crypto = require("crypto");
const { execSync, exec } = require("child_process");
const path = require("path");
// 从 version.json 读取版本号
const BASE_DIR = process.env.BASE_DIR || "/opt/b-ui";
const ADMIN_DIR = process.env.ADMIN_DIR || path.join(BASE_DIR, "admin");

function getVersion() {
    try {
        const versionFile = path.join(BASE_DIR, "version.json");
        if (fs.existsSync(versionFile)) {
            return JSON.parse(fs.readFileSync(versionFile, "utf8")).version || "2.4.0";
        }
    } catch { }
    return "2.4.0";
}
const VERSION = getVersion();

const CONFIG = {
    port: process.env.ADMIN_PORT || 8080,
    adminPassword: process.env.ADMIN_PASSWORD || "admin123",
    jwtSecret: process.env.JWT_SECRET || crypto.randomBytes(32).toString("hex"),
    hysteriaConfig: process.env.HYSTERIA_CONFIG || `${BASE_DIR}/config.yaml`,
    xrayConfig: process.env.XRAY_CONFIG || `${BASE_DIR}/xray-config.json`,
    xrayKeysFile: process.env.XRAY_KEYS || `${BASE_DIR}/reality-keys.json`,
    usersFile: process.env.USERS_FILE || `${BASE_DIR}/users.json`,
    trafficPort: 9999,
    xrayApiPort: 10085
};

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
        r.on("end", () => { try { s(b ? JSON.parse(b) : {}); } catch { s({}); } });
    });
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
        updateHysteriaConfig(u.filter(x => !x.protocol || x.protocol === "hysteria2"));
        updateXrayConfig(u.filter(x => x.protocol === "vless-reality"), u.filter(x => x.protocol === "vless-ws-tls"));
        return true;
    } catch { return false; }
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

        // Update Reality inbound
        const realityClients = realityUsers.map(u => ({ id: u.uuid, flow: "xtls-rprx-vision", email: u.username }));
        const inbound = c.inbounds.find(i => i.tag === "vless-reality");
        if (inbound) {
            inbound.settings.clients = realityClients;
            const userSnis = realityUsers.filter(u => u.sni).map(u => u.sni);
            const baseSni = inbound.streamSettings?.realitySettings?.dest?.split(":")[0] || "www.bing.com";
            const allSnis = [...new Set([baseSni, ...userSnis])];
            if (inbound.streamSettings?.realitySettings) inbound.streamSettings.realitySettings.serverNames = allSnis;
        }

        // Update WS+TLS inbound
        const wsClients = wsUsers.map(u => ({ id: u.uuid, email: u.username }));
        let wsInbound = c.inbounds.find(i => i.tag === "vless-ws-tls");
        if (wsUsers.length > 0) {
            if (!wsInbound) {
                const hc = fs.readFileSync(CONFIG.hysteriaConfig, "utf8");
                const dm = hc.match(/\/live\/([^\/]+)\/fullchain/);
                const domain = dm ? dm[1] : "localhost";
                wsInbound = {
                    tag: "vless-ws-tls",
                    port: 10002,
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
        dm = hc.match(/\/live\/([^\/]+)\/fullchain/);
        pm = hc.match(/listen:\s*:(\d+)/);
        let xrayPort = 10001, pubKey = "", shortId = "", sni = "www.bing.com";
        try {
            const xc = JSON.parse(fs.readFileSync(CONFIG.xrayConfig, "utf8"));
            const xi = xc.inbounds.find(i => i.tag === "vless-reality");
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
        return { domain: dm ? dm[1] : "localhost", port: pm ? pm[1] : "443", xrayPort, pubKey, shortId, sni };
    } catch {
        return { domain: "localhost", port: "443", xrayPort: 10001, pubKey: "", shortId: "", sni: "www.bing.com" };
    }
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
                // 查询 uplink (上传流量)
                const upCmd = `xray api stats --server=127.0.0.1:${CONFIG.xrayApiPort} -name "user>>>${email}>>>traffic>>>uplink" 2>/dev/null || echo "{}"`;
                const upResult = execSync(upCmd, { encoding: "utf8", timeout: 3000 }).trim();
                let tx = 0;
                try {
                    const upJson = JSON.parse(upResult);
                    tx = upJson.stat?.value || 0;
                } catch { }

                // 查询 downlink (下载流量)
                const downCmd = `xray api stats --server=127.0.0.1:${CONFIG.xrayApiPort} -name "user>>>${email}>>>traffic>>>downlink" 2>/dev/null || echo "{}"`;
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
        const pass = params.get("pass") || ((protocol === "vless-reality" || protocol === "vless-ws-tls") ? crypto.randomUUID() : crypto.randomBytes(8).toString("hex"));
        const days = parseInt(params.get("days")) || 0;
        const traffic = parseFloat(params.get("traffic")) || 0;
        const monthly = parseFloat(params.get("monthly")) || 0;
        const speed = parseFloat(params.get("speed")) || 0;
        const sni = params.get("sni") || "www.bing.com";
        const newUser = { username: user, protocol, createdAt: new Date().toISOString(), limits: {}, usage: { total: 0, monthly: {} } };
        if (protocol === "vless-reality" || protocol === "vless-ws-tls") { newUser.uuid = pass; newUser.sni = sni; } else { newUser.password = pass; }
        if (days > 0) newUser.limits.expiresAt = new Date(Date.now() + days * 864e5).toISOString();
        if (traffic > 0) newUser.limits.trafficLimit = traffic * 1073741824;
        if (monthly > 0) newUser.limits.monthlyLimit = monthly * 1073741824;
        if (speed > 0) newUser.limits.speedLimit = speed * 1000000;
        users.push(newUser);
        if (saveUsers(users)) return sendJSON(res, { success: true, user: user, password: pass, sni: sni });
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

        // 优先从 GitHub 获取最新客户端脚本
        const GITHUB_RAW = "https://raw.githubusercontent.com/Buxiulei/b-ui/main";
        const GITHUB_CDN = "https://cdn.jsdelivr.net/gh/Buxiulei/b-ui@main";

        let clientScript = "";

        // 异步获取 GitHub 脚本
        const fetchFromGitHub = () => {
            return new Promise((resolve) => {
                const https = require("https");
                // 先尝试 GitHub Raw
                https.get(`${GITHUB_RAW}/b-ui-client.sh`, { timeout: 5000 }, (resp) => {
                    if (resp.statusCode === 200) {
                        let data = "";
                        resp.on("data", chunk => data += chunk);
                        resp.on("end", () => resolve(data));
                    } else {
                        // 尝试 CDN
                        https.get(`${GITHUB_CDN}/b-ui-client.sh`, { timeout: 5000 }, (resp2) => {
                            if (resp2.statusCode === 200) {
                                let data = "";
                                resp2.on("data", chunk => data += chunk);
                                resp2.on("end", () => resolve(data));
                            } else {
                                resolve(null);
                            }
                        }).on("error", () => resolve(null));
                    }
                }).on("error", () => {
                    // GitHub 失败，尝试 CDN
                    https.get(`${GITHUB_CDN}/b-ui-client.sh`, { timeout: 5000 }, (resp) => {
                        if (resp.statusCode === 200) {
                            let data = "";
                            resp.on("data", chunk => data += chunk);
                            resp.on("end", () => resolve(data));
                        } else {
                            resolve(null);
                        }
                    }).on("error", () => resolve(null));
                });
            });
        };

        fetchFromGitHub().then(githubScript => {
            if (githubScript) {
                clientScript = githubScript;
            } else {
                // 回退到本地缓存或引导脚本
                const PACKAGES_DIR = path.join(path.dirname(ADMIN_DIR), "packages");
                const clientScriptPath = path.join(PACKAGES_DIR, "b-ui-client.sh");
                try {
                    if (fs.existsSync(clientScriptPath)) {
                        clientScript = fs.readFileSync(clientScriptPath, "utf8");
                    } else {
                        clientScript = generateBootstrapScript(serverDomain, key);
                    }
                } catch (e) {
                    clientScript = generateBootstrapScript(serverDomain, key);
                }
            }

            // 在脚本开头注入服务端地址
            const injectedScript = `#!/bin/bash
# B-UI 客户端一键安装脚本
# 服务端: ${serverDomain}
# 安装时间: $(date)

# 预设服务端地址
export BUI_SERVER="${serverDomain}"
export BUI_INSTALL_KEY="${key}"

# 创建配置目录并保存服务端地址
mkdir -p /opt/hysteria-client
echo "${serverDomain}" > /opt/hysteria-client/server_address

${clientScript.replace(/^#!\/bin\/bash\s*\n?/, "")}
`;

            res.writeHead(200, {
                "Content-Type": "text/x-shellscript; charset=utf-8",
                "Content-Disposition": "inline; filename=\"b-ui-client-install.sh\""
            });
            res.end(injectedScript);
        });
        return;
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
        const filePath = path.join(PACKAGES_DIR, fileName);

        // 安全检查：防止路径遍历
        if (!filePath.startsWith(PACKAGES_DIR)) {
            return sendJSON(res, { error: "Invalid path" }, 400);
        }

        try {
            if (fs.existsSync(filePath) && fs.statSync(filePath).isFile()) {
                const content = fs.readFileSync(filePath);
                const ext = path.extname(fileName).toLowerCase();
                let contentType = "application/octet-stream";
                if (ext === ".sh") contentType = "text/x-shellscript";
                else if (ext === ".json") contentType = "application/json";
                else if (ext === ".zip") contentType = "application/zip";
                else if (ext === ".gz" || ext === ".tar.gz") contentType = "application/gzip";

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

            // 安装命令 API (无需认证)
            if (r === "install-command") {
                const host = req.headers.host || "localhost";
                const key = getOrCreateInstallKey();
                const command = `bash <(curl -fsSL -k https://${host}/install-client?key=${key})`;
                return sendJSON(res, {
                    command,
                    key,
                    server: host,
                    note: "此命令仅在此服务端有效，请勿泄露给不信任的人"
                });
            }

            const auth = verifyToken((req.headers.authorization || "").replace("Bearer ", ""));
            if (!auth) return sendJSON(res, { error: "Unauthorized" }, 401);

            if (r === "users") {
                if (req.method === "GET") return sendJSON(res, loadUsers());
                if (req.method === "POST") {
                    const b = await parseBody(req), users = loadUsers();
                    if (users.find(u => u.username === b.username)) return sendJSON(res, { error: "Exists" }, 400);
                    users.push({
                        username: b.username,
                        password: b.password || crypto.randomBytes(8).toString("hex"),
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
                const b = await parseBody(req);
                let users = loadUsers();
                const userIndex = users.findIndex(u => u.username === origUsername);

                if (userIndex < 0) {
                    return sendJSON(res, { error: "User not found" }, 404);
                }

                const user = users[userIndex];

                // 更新用户名
                if (b.username && b.username !== origUsername) {
                    // 检查新用户名是否已存在
                    if (users.find(u => u.username === b.username)) {
                        return sendJSON(res, { error: "Username already exists" }, 400);
                    }
                    user.username = b.username;
                }

                // 更新密码/UUID
                if (b.password) {
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
                            const xi = xc.inbounds.find(i => i.tag === "vless-reality");
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

            if (r === "password" && req.method === "POST") {
                const b = await parseBody(req);
                if (!b.newPassword || b.newPassword.length < 6) return sendJSON(res, { error: "密码至少6位" }, 400);
                try {
                    const svc = "/etc/systemd/system/b-ui-admin.service";
                    let c = fs.readFileSync(svc, "utf8");
                    c = c.replace(/ADMIN_PASSWORD=[^\n]*/, "ADMIN_PASSWORD=" + b.newPassword);
                    fs.writeFileSync(svc, c);
                    execSync("systemctl daemon-reload");
                    return sendJSON(res, { success: true, message: "密码已更新，请重新登录" });
                } catch (e) { return sendJSON(res, { error: e.message }, 500); }
            }
        } catch (e) { return sendJSON(res, { error: e.message }, 500); }
    }

    // Hysteria2 HTTP Auth Endpoint
    if (p === "/auth/hysteria" && req.method === "POST") {
        const body = await parseBody(req);
        const authStr = body.auth || "";
        const [username, password] = authStr.split(":");
        const users = loadUsers();
        const user = users.find(u => u.username === username && u.password === password);
        if (user) {
            const check = checkUserLimits(user);
            if (check.ok) {
                return sendJSON(res, { ok: true, id: username });
            } else {
                return sendJSON(res, { ok: false, id: username });
            }
        }
        return sendJSON(res, { ok: false });
    }

    sendJSON(res, { error: "Not found" }, 404);
});

server.listen(CONFIG.port, () => console.log(`B-UI Admin Panel v${VERSION} running on port ${CONFIG.port}`));
