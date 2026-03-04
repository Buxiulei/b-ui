/**
 * B-UI Admin Panel - Frontend JavaScript
 * Version: 动态读取自 server.js
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
    const headers = { Authorization: "Bearer " + tok, ...opt.headers };
    // 如果有 body 且是字符串（JSON），添加 Content-Type
    if (opt.body && typeof opt.body === 'string') {
        headers['Content-Type'] = 'application/json';
    }
    return fetch("/api" + ep, {
        ...opt,
        headers
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
                toast("登录认证失败", 1);
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
            // 没有实时数据时，假设下载流量 = 上传流量（对称估算）
            td = tu;
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
            const badge = exp ? ' <span class="tag" style="color:var(--danger)">已过期</span>' : (over ? ' <span class="tag" style="color:var(--danger)">流量耗尽</span>' : "");
            const proto = x.protocol || "hysteria2";
            const ptag = proto === "fusion" ? '<span class="proto-tag proto-sub">订阅</span>' :
                proto === "vless-reality" ? '<span class="proto-tag proto-vless">VLESS</span>' :
                    proto === "vless-ws-tls" ? '<span class="proto-tag proto-ws">WS</span>' :
                        '<span class="proto-tag proto-hy2">HY2</span>';

            return '<tr>' +
                '<td><div style="display:flex;align-items:center;gap:8px"><span style="font-weight:600">' + esc(x.username) + '</span>' + ptag + badge + '</div></td>' +
                '<td><span class="tag ' + (on ? 'on' : '') + ' ">' + (on ? on + ' 在线' : '离线') + '</span></td>' +
                '<td class="hide-m" style="font-family:monospace;color:var(--text-dim)">' + sz(monthly) + '</td>' +
                '<td class="hide-m" style="font-family:monospace;color:var(--text-dim)">' + sz(total) + (tlim ? ' / ' + sz(tlim) : '') + '</td>' +
                '<td>' +
                '<div style="display:flex;gap:8px">' +
                '<button class="ibtn share" onclick="showU(\'' + esc(x.username).replace(/'/g, "\\'") + '\')" title="分享">🔗</button>' +
                '<button class="ibtn edit" onclick="editUser(\'' + esc(x.username).replace(/'/g, "\\'") + '\')" title="编辑">✏️</button>' +
                (on ? '<button class="ibtn warn" onclick="kick(\'' + esc(x.username).replace(/'/g, "\\'") + '\')" title="断开">⚡</button>' : '') +
                '<button class="ibtn danger" onclick="del(\'' + esc(x.username).replace(/'/g, "\\'") + '\')" title="删除">🗑</button>' +
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
    const s = $("#ns").value || 100;  // 默认 100Mbps 上下行带宽
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
            toast("用户 " + u + " 已创建");
            load();
        } else {
            toast(r.error || "操作失败", 1);
        }
    });
}

// Delete user
function del(u) {
    if (confirm("确认删除用户 " + u + " 吗？")) {
        api("/users/" + encodeURIComponent(u), { method: "DELETE" }).then(() => load());
    }
}

// Kick user
function kick(u) {
    api("/kick", { method: "POST", body: JSON.stringify([u]) }).then(() => toast("用户 " + u + " 已被断开"));
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

// Generate URI - 根据协议类型生成不同的链接
function genUri(x) {
    // 融合订阅用户: 返回 v2rayN 原生订阅 URL (带备注)
    if (x.protocol === "fusion") {
        const host = location.host;
        // URL 末尾的 #备注 会被 v2rayNG 识别为订阅名称（不编码）
        return "https://" + host + "/api/sub/" + x.username + "#" + x.username;
    }
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
    // Hysteria2: v2rayN 兼容格式
    // v2rayN 格式：整个 username:password 一起编码（冒号也编码为 %3A）
    const auth = encodeURIComponent(x.username + ":" + x.password);

    // 查询参数构建
    let queryParams = "sni=" + cfg.domain + "&insecure=0&allowInsecure=0";

    // 端口跳跃使用 mport 参数（v2rayN 格式）
    if (cfg.portHopping && cfg.portHopping.enabled) {
        queryParams += "&mport=" + cfg.portHopping.start + "-" + cfg.portHopping.end;
    }

    return "hysteria2://" + auth + "@" + cfg.domain + ":" + cfg.port + "?" + queryParams + "#" + encodeURIComponent(x.username);
}

// 当前显示的用户名 (用于下载订阅)
let currentShowUser = null;

// Show user config
function showU(uname) {
    const x = allUsers.find(u => u.username === uname);
    if (!x) return;
    currentShowUser = x;
    const uri = genUri(x);
    $("#uri").innerText = uri;

    // 融合订阅用户显示订阅链接
    if (x.protocol === "fusion") {
        $("#cfg-title").innerText = "🔄 融合订阅配置";
        $("#cfg-desc").innerHTML = "Hysteria2 + VLESS 自动故障切换<br><small>可导入 v2rayN / v2rayNG / Shadowrocket / bui-c 客户端</small>";

        // 显示二维码
        $("#qrcode").innerHTML = '<img src="https://api.qrserver.com/v1/create-qr-code/?size=200x200&data=' +
            encodeURIComponent(uri) + '" alt="QR Code" style="display:block;border-radius:8px">';

        $("#cfg-buttons").innerHTML = `
            <button class="btn" onclick="copy()">📋 复制订阅链接</button>
            <button class="btn btn-secondary" onclick="copyClash()">📎 复制 Clash 订阅</button>
            <button class="btn btn-secondary" onclick="downloadSubscription()">📦 下载 sing-box 配置</button>
        `;

        // 提示
        $("#cfg-hint").innerText = "📱 扫码或复制链接导入客户端，支持 v2rayN / Shadowrocket / Clash Verge Rev";
    } else {
        // 单协议用户
        const protoName = x.protocol === "hysteria2" ? "Hysteria2" :
            x.protocol === "vless-reality" ? "VLESS-Reality" :
                x.protocol === "vless-ws-tls" ? "VLESS-WS" : x.protocol;

        $("#cfg-title").innerText = protoName + " 配置";
        $("#cfg-desc").innerText = "单协议客户端配置";

        // 显示二维码
        $("#qrcode").innerHTML = '<img src="https://api.qrserver.com/v1/create-qr-code/?size=200x200&data=' +
            encodeURIComponent(uri) + '" alt="QR Code" style="display:block;border-radius:8px">';

        // 按钮 - 根据协议类型显示
        let btnHtml = `<button class="btn" onclick="copy()">📋 复制链接</button>`;

        $("#cfg-buttons").innerHTML = btnHtml;
        $("#cfg-hint").innerText = "扫码或复制链接导入客户端";
    }

    openM("m-cfg");
}

// Copy URI
function copy() {
    const uri = $("#uri").innerText;
    navigator.clipboard.writeText(uri);
    if (currentShowUser && currentShowUser.protocol === "fusion") {
        toast("订阅链接已复制，可粘贴到 v2rayN / Shadowrocket");
    } else {
        toast("链接已复制到剪贴板");
    }
}

// 下载 sing-box 融合订阅配置
function downloadSubscription() {
    if (!currentShowUser) return toast("请先选择用户", 1);
    const url = "/api/subscription/" + encodeURIComponent(currentShowUser.username);
    window.open(url, "_blank");
    toast("正在下载 sing-box 配置...");
}

// 复制 Clash Verge Rev 订阅链接
function copyClash() {
    if (!currentShowUser) return toast("请先选择用户", 1);
    const clashUrl = "https://" + location.host + "/api/clash/" + encodeURIComponent(currentShowUser.username);
    navigator.clipboard.writeText(clashUrl)
        .then(() => toast("Clash 订阅链接已复制，可导入 Clash Verge Rev"))
        .catch(() => toast("复制失败", 1));
}



// Change password
function changePwd() {
    const np = $("#newpwd").value;
    if (np.length < 6) return toast("密码至少需要6个字符", 1);
    api("/password", { method: "POST", body: JSON.stringify({ newPassword: np }) }).then(r => {
        if (r.success) {
            closeM();
            toast("密码已修改，请重新登录");
            setTimeout(() => logout(), 2000);
        } else {
            toast(r.error || "操作失败", 1);
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
            toast(r.error || "操作失败", 1);
        }
    });
}

// Bandwidth settings
function openBandwidth() {
    api("/bandwidth").then(r => {
        $("#bandwidth-up").value = r.up || "";
        $("#bandwidth-down").value = r.down || "";
        openM("m-bandwidth");
    });
}

function saveBandwidth() {
    const up = $("#bandwidth-up").value || 0;
    const down = $("#bandwidth-down").value || 0;

    if (up < 0 || down < 0) return toast("带宽值不能为负数", 1);

    api("/bandwidth", {
        method: "POST",
        body: JSON.stringify({ up: parseFloat(up), down: parseFloat(down) })
    }).then(r => {
        if (r.success) {
            closeM();
            toast("全局带宽限制已更新");
            setTimeout(() => location.reload(), 2000);
        } else {
            toast(r.error || "操作失败", 1);
        }
    });
}

// Port Hopping settings
function openPortHopping() {
    api("/port-hopping").then(r => {
        $("#ph-enabled").checked = r.enabled || false;
        $("#ph-start").value = r.start || 20000;
        $("#ph-end").value = r.end || 30000;
        openM("m-porthopping");
    });
}

function savePortHopping() {
    const enabled = $("#ph-enabled").checked;
    const start = parseInt($("#ph-start").value) || 20000;
    const end = parseInt($("#ph-end").value) || 30000;

    if (start >= end) return toast("起始端口必须小于结束端口", 1);
    if (start < 1024 || end > 65535) return toast("端口范围应在 1024-65535 之间", 1);

    api("/port-hopping", {
        method: "POST",
        body: JSON.stringify({ enabled, start, end })
    }).then(r => {
        if (r.success) {
            closeM();
            toast(enabled ? "端口跳跃已启用: " + start + "-" + end : "端口跳跃已禁用");
            // 刷新配置
            api("/config").then(d => cfg = d);
        } else {
            toast(r.error || "操作失败", 1);
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
