/**
 * B-UI Admin Panel - Frontend JavaScript
 * Version: 2.4.0
 */

const $ = s => document.querySelector(s);
let tok = localStorage.getItem("t"), cfg = {};
let allUsers = [];

// Security: Escape HTML
const esc = s => String(s).replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));

// Format bytes
const sz = b => {
    if (!b) return "0 B";
    const i = Math.floor(Math.log(b) / Math.log(1024));
    return (b / Math.pow(1024, i)).toFixed(2) + " " + ["B", "KB", "MB", "GB"][i];
};

// Toast notification
function toast(m, e) {
    const d = document.createElement("div");
    d.className = "toast";
    d.innerHTML = "<span style='font-size:18px'>" + (e ? "⚠️" : "✅") + "</span><div>" + m + "</div>";
    $("#t-box").appendChild(d);
    setTimeout(() => d.remove(), 3000);
}

// Modal controls
function openM(id) { $("#" + id).classList.add("on"); }
function closeM() { document.querySelectorAll(".modal").forEach(e => e.classList.remove("on")); }

// API helper
function api(ep, opt = {}) {
    return fetch("/api" + ep, {
        ...opt,
        headers: { ...opt.headers, Authorization: "Bearer " + tok }
    }).then(r => {
        if (r.status == 401) logout();
        return r.json();
    });
}

// Login
function login() {
    const pw = $("#lp").value;
    fetch("/api/login", { method: "POST", body: JSON.stringify({ password: pw }) })
        .then(r => r.json())
        .then(d => {
            if (d.token) {
                tok = d.token;
                localStorage.setItem("t", tok);
                localStorage.setItem("ap", pw);
                init();
            } else {
                toast("Authentication failed", 1);
            }
        });
}

function logout() {
    localStorage.removeItem("t");
    location.reload();
}

// 安装命令相关
let installCmd = "";

function loadInstallCommand() {
    fetch("/api/install-command")
        .then(r => r.json())
        .then(d => {
            if (d.command) {
                installCmd = d.command;
                const el = document.getElementById("install-cmd");
                if (el) el.innerText = d.command;
            }
        })
        .catch(() => {
            const el = document.getElementById("install-cmd");
            if (el) el.innerText = "无法加载安装命令";
        });
}

function copyInstallCmd() {
    if (!installCmd) return toast("命令未加载", 1);
    navigator.clipboard.writeText(installCmd)
        .then(() => toast("已复制到剪贴板"))
        .catch(() => toast("复制失败", 1));
}

// Initialize dashboard
function init() {
    $("#v-login").classList.remove("active");
    setTimeout(() => $("#v-login").style.display = "none", 300);
    $("#v-dash").classList.add("active");
    api("/config").then(d => cfg = d);
    load();
    loadInstallCommand();
    setInterval(load, 5000);
}

// Load data
function load() {
    Promise.all([api("/users"), api("/online"), api("/stats")]).then(([u, o, s]) => {
        $("#st-u").innerText = u.length;
        // 在线设备：累加所有用户的连接数
        let totalOnline = 0;
        Object.values(o).forEach(v => { totalOnline += (typeof v === 'number' ? v : 1); });
        $("#st-o").innerText = totalOnline;

        // 流量统计：使用用户的历史累计流量（与用户列表一致）
        let tu = 0, td = 0;
        u.forEach(x => {
            tu += x.usage?.total || 0;
        });
        // 分别计算上传和下载（从实时 stats 获取比例）
        let statsTx = 0, statsRx = 0;
        Object.values(s).forEach(v => { statsTx += v.tx || 0; statsRx += v.rx || 0; });
        const totalStats = statsTx + statsRx;
        if (totalStats > 0) {
            // 按比例分配历史流量到上传和下载
            td = Math.round(tu * (statsRx / totalStats));
            tu = Math.round(tu * (statsTx / totalStats));
        } else {
            td = 0;
        }
        $("#st-up").innerText = sz(tu);
        $("#st-dl").innerText = sz(td);

        const m = new Date().toISOString().slice(0, 7);
        allUsers = u;

        // Preload QR codes
        u.forEach(x => {
            const uri = genUri(x);
            new Image().src = "https://api.qrserver.com/v1/create-qr-code/?size=200x200&data=" + encodeURIComponent(uri);
        });

        $("#tb").innerHTML = u.map(x => {
            const on = o[x.username];
            const monthly = x.usage?.monthly?.[m] || 0;
            const total = x.usage?.total || 0;
            const exp = x.limits?.expiresAt ? new Date(x.limits.expiresAt) < new Date() : "";
            const tlim = x.limits?.trafficLimit;
            const over = tlim && total >= tlim;
            const badge = exp ? ' <span class="tag" style="color:var(--danger)">EXPIRED</span>' : (over ? ' <span class="tag" style="color:var(--danger)">LIMIT</span>' : "");
            const proto = x.protocol || "hysteria2";
            const ptag = proto === "vless-reality" ? '<span class="proto-tag proto-vless">VLESS</span>' :
                (proto === "vless-ws-tls" ? '<span class="proto-tag proto-ws">WS</span>' : '<span class="proto-tag proto-hy2">HY2</span>');

            return '<tr>' +
                '<td><div style="display:flex;align-items:center;gap:8px"><span style="font-weight:600">' + esc(x.username) + '</span>' + ptag + badge + '</div></td>' +
                '<td><span class="tag ' + (on ? 'on' : '') + ' ">' + (on ? on + ' Online' : 'Offline') + '</span></td>' +
                '<td class="hide-m" style="font-family:monospace;color:var(--text-dim)">' + sz(monthly) + '</td>' +
                '<td class="hide-m" style="font-family:monospace;color:var(--text-dim)">' + sz(total) + (tlim ? ' / ' + sz(tlim) : '') + '</td>' +
                '<td>' +
                '<div style="display:flex;gap:8px">' +
                '<button class="ibtn" onclick="showU(\'' + esc(x.username).replace(/'/g, "\\'") + '\')" title="Config">⚙</button>' +
                '<button class="ibtn" onclick="editUser(\'' + esc(x.username).replace(/'/g, "\\'") + '\')" title="Edit">✏️</button>' +
                (on ? '<button class="ibtn danger" onclick="kick(\'' + esc(x.username).replace(/'/g, "\\'") + '\')" title="Kick">⚡</button>' : '') +
                '<button class="ibtn danger" onclick="del(\'' + esc(x.username).replace(/'/g, "\\'") + '\')" title="Delete">🗑</button>' +
                '</div>' +
                '</td>' +
                '</tr>';
        }).join("");
    });
}

// Add user
function addUser() {
    const u = $("#nu").value;
    const p = $("#np").value;
    const d = $("#nd").value || 0;
    const t = $("#nt").value || 0;
    const m = $("#nm").value || 0;
    const s = $("#ns").value || 0;
    const proto = $("#nproto").value;
    const customSni = $("#nsni-custom")?.value || $("#nsni")?.value || "";

    let url = "/api/manage?key=" + encodeURIComponent(cfg.adminPass || localStorage.getItem("ap") || "") +
        "&action=create&user=" + encodeURIComponent(u) +
        (p ? "&pass=" + encodeURIComponent(p) : "") +
        "&days=" + d + "&traffic=" + t + "&monthly=" + m + "&speed=" + s + "&protocol=" + proto;

    if (customSni) url += "&sni=" + encodeURIComponent(customSni);

    fetch(url).then(r => r.json()).then(r => {
        if (r.success) {
            closeM();
            toast("User " + u + " created");
            load();
        } else {
            toast(r.error || "Failed", 1);
        }
    });
}

// Delete user
function del(u) {
    if (confirm("Delete user " + u + "?")) {
        api("/users/" + encodeURIComponent(u), { method: "DELETE" }).then(() => load());
    }
}

// Kick user
function kick(u) {
    api("/kick", { method: "POST", body: JSON.stringify([u]) }).then(() => toast("User " + u + " kicked offline"));
}

// Edit user - open modal with current settings
function editUser(uname) {
    const x = allUsers.find(u => u.username === uname);
    if (!x) return;

    $("#edit-orig-username").value = x.username;
    $("#edit-username").value = x.username;
    $("#edit-password").value = "";  // 不显示密码，留空表示保持不变

    // 填充限制设置
    const limits = x.limits || {};

    // 有效期转换为天数
    if (limits.expiresAt) {
        const expDate = new Date(limits.expiresAt);
        const now = new Date();
        const daysLeft = Math.max(0, Math.ceil((expDate - now) / (1000 * 60 * 60 * 24)));
        $("#edit-days").value = daysLeft;
    } else {
        $("#edit-days").value = "";
    }

    // 流量转换为 GB
    $("#edit-traffic").value = limits.trafficLimit ? (limits.trafficLimit / 1073741824).toFixed(1) : "";
    $("#edit-monthly").value = limits.monthlyLimit ? (limits.monthlyLimit / 1073741824).toFixed(1) : "";
    $("#edit-speed").value = limits.speedLimit ? (limits.speedLimit / 1000000) : "";

    // 显示当前使用量
    const m = new Date().toISOString().slice(0, 7);
    const monthly = x.usage?.monthly?.[m] || 0;
    const total = x.usage?.total || 0;
    $("#edit-usage-info").innerHTML = "本月: " + sz(monthly) + " | 总计: " + sz(total);

    openM("m-edit");
}

// Save user changes
function saveUser() {
    const origUsername = $("#edit-orig-username").value;
    const newUsername = $("#edit-username").value;
    const newPassword = $("#edit-password").value;
    const days = $("#edit-days").value || 0;
    const traffic = $("#edit-traffic").value || 0;
    const monthly = $("#edit-monthly").value || 0;
    const speed = $("#edit-speed").value || 0;

    if (!newUsername) {
        return toast("用户名不能为空", 1);
    }

    api("/users/" + encodeURIComponent(origUsername), {
        method: "PUT",
        body: JSON.stringify({
            username: newUsername,
            password: newPassword || undefined,
            days: parseFloat(days),
            traffic: parseFloat(traffic),
            monthly: parseFloat(monthly),
            speed: parseFloat(speed)
        })
    }).then(r => {
        if (r.success) {
            closeM();
            toast("用户 " + newUsername + " 已更新");
            load();
        } else {
            toast(r.error || "更新失败", 1);
        }
    });
}

// Generate URI
function genUri(x) {
    if (x.protocol === "vless-reality") {
        const userSni = x.sni || cfg.sni || "www.bing.com";
        return "vless://" + x.uuid + "@" + cfg.domain + ":" + cfg.xrayPort +
            "?encryption=none&flow=xtls-rprx-vision&security=reality&sni=" + userSni +
            "&fp=chrome&pbk=" + cfg.pubKey + "&sid=" + cfg.shortId + "&spx=%2F&type=tcp#" +
            encodeURIComponent(x.username);
    }
    if (x.protocol === "vless-ws-tls") {
        const hostSni = x.sni || "www.bing.com";
        return "vless://" + x.uuid + "@" + cfg.domain + ":" + (cfg.wsPort || 10002) +
            "?encryption=none&security=tls&sni=" + cfg.domain +
            "&type=ws&host=" + hostSni + "&path=%2Fws#" +
            encodeURIComponent(x.username);
    }
    return "hysteria2://" + encodeURIComponent(x.username) + ":" + encodeURIComponent(x.password) +
        "@" + cfg.domain + ":" + cfg.port + "/?sni=" + cfg.domain + "&insecure=0#" + encodeURIComponent(x.username);
}

// Show user config
function showU(uname) {
    const x = allUsers.find(u => u.username === uname);
    if (!x) return;
    const uri = genUri(x);
    $("#uri").innerText = uri;
    $("#qrcode").innerHTML = '<img src="https://api.qrserver.com/v1/create-qr-code/?size=200x200&data=' +
        encodeURIComponent(uri) + '" alt="QR Code" style="display:block;border-radius:8px">';
    openM("m-cfg");
}

// Copy URI
function copy() {
    navigator.clipboard.writeText($("#uri").innerText);
    toast("Copied to clipboard");
}

// Change password
function changePwd() {
    const np = $("#newpwd").value;
    if (np.length < 6) return toast("Password min 6 chars", 1);
    api("/password", { method: "POST", body: JSON.stringify({ newPassword: np }) }).then(r => {
        if (r.success) {
            closeM();
            toast("Password updated, please login again");
            setTimeout(() => logout(), 2000);
        } else {
            toast(r.error || "Failed", 1);
        }
    });
}

// Masquerade settings
function openMasq() {
    api("/masquerade").then(r => {
        $("#masqurl").value = r.masqueradeUrl || "https://www.bing.com/";
        openM("m-masq");
    });
}

function saveMasq() {
    const url = $("#masqurl").value;
    if (!url) return toast("请输入URL", 1);
    api("/masquerade", { method: "POST", body: JSON.stringify({ url }) }).then(r => {
        if (r.success) {
            closeM();
            toast("伪装网站已更新: " + r.domain);
            setTimeout(() => location.reload(), 2000);
        } else {
            toast(r.error || "Failed", 1);
        }
    });
}

// Toggle SNI select visibility
function toggleSniSelect() {
    const proto = $("#nproto").value;
    const sniGroup = $("#sni-group");
    if (proto === "vless-reality" || proto === "vless-ws-tls") {
        sniGroup.style.display = "block";
    } else {
        sniGroup.style.display = "none";
    }
}

// Auto-init if token exists
if (tok) init();
