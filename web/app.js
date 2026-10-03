/**
 * B-UI Admin Panel - Frontend JavaScript
 * Version: 动态读取自 server.js
 */

const $ = s => document.querySelector(s);
let tok = localStorage.getItem("t"), cfg = {};
let allUsers = [];
let userListRequest = 0;

// Security: Escape HTML
const esc = s => String(s).replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));

// Format bytes
const sz = b => {
    if (!b) return "0 B";
    const units = ["B", "KB", "MB", "GB", "TB", "PB"];
    const i = Math.min(Math.floor(Math.log(b) / Math.log(1024)), units.length - 1);
    return (b / Math.pow(1024, i)).toFixed(2) + " " + units[i];
};

// Toast notification
function toast(m, e) {
    const d = document.createElement("div");
    d.className = "toast";
    const _warnSvg = `<svg xmlns="http://www.w3.org/2000/svg" width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="#FF9500" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M10.29 3.86L1.82 18a2 2 0 001.71 3h16.94a2 2 0 001.71-3L13.71 3.86a2 2 0 00-3.42 0z"/><line x1="12" y1="9" x2="12" y2="13"/><line x1="12" y1="17" x2="12.01" y2="17"/></svg>`;
    const _okSvg = `<svg xmlns="http://www.w3.org/2000/svg" width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="#34C759" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="20 6 9 17 4 12"/></svg>`;
    d.innerHTML = "<span style='display:inline-flex;align-items:center'>" + (e ? _warnSvg : _okSvg) + "</span><div>" + m + "</div>"; // eslint-disable-line
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
    initSysStatusOnce();
    setInterval(load, 5000);
}

// spec §6 的到期 / 封禁语义，**一处文案**：徽标 tooltip 与槽位列 tooltip 都用它。
// 两条路的拒绝时机不一样，说不清就会被当成「面板数据不对」：
//   - 住宅 HY2 = sing-box 的静态凭据池 + 门位（selector）。凭据没变，握手照旧成功，
//     门位指向 deny（那个打 127.0.0.1:1 的出站）⇒ **客户端一直显示已连接，但每个请求被拒**。
//   - 直连 = apernet hysteria2 的 auth 钩子，鉴权那一步就回绝 ⇒ 连接时即被拒。
const _DENY_SEMANTICS =
    "已停用 / 已到期：住宅 HY2 客户端仍会显示已连接，但所有请求会被拒绝（门位 deny，握手照旧成功）；" +
    "直连节点在连接时即被拒。";

const _RESI_UNAVAILABLE_REASONS = new Map([
    ["account disabled", "账户已停用"],
    ["account blocked", "账户到期、流量耗尽或暂不可用"],
    ["protocol not granted", "未开通所需协议"],
    ["direct path not granted", "未开通直连通路"],
    ["residential path not granted", "未开通住宅通路"],
    ["residential group missing", "住宅分组不存在"],
    ["residential group unsupported", "住宅分组尚不支持"],
    ["residential pool disabled", "住宅池未启用"],
    ["residential pool empty", "住宅池没有可用上游"],
    ["residential slot unassigned", "住宅出口尚未分配"],
    ["residential slot missing", "住宅槽位不存在"],
    ["residential slot ambiguous", "住宅槽位绑定冲突"],
    ["residential slot index invalid", "住宅槽位配置无效"],
    ["residential upstream missing", "住宅上游不存在"],
    ["residential upstream ambiguous", "住宅上游绑定冲突"],
    ["residential upstream invalid", "住宅上游配置无效"],
    ["residential HY2 credential missing", "住宅 HY2 凭据尚未分配"],
    ["residential HY2 credential invalid", "住宅 HY2 凭据无效"]
]);
const _RESI_REQUIRED_HINT = "住宅业务仅走住宅出口，不可用时拒绝连接。";
const _RESI_GLOBAL_HINT = "完整配置：全部业务走住宅";
const _RESI_SPLIT_HINT = "完整配置：关键词走住宅，其余走授权直连";

function _resiUnavailableHint(x) {
    if (!x.residentialUnavailable) return "";
    return _RESI_UNAVAILABLE_REASONS.get(x.residentialUnavailable) || "住宅出口不可用";
}

// 「重置订阅链接与凭据」（rotate）的后果，**一处文案**：二次确认与成功提示都用它
// （spec §7.4 第 3 条，2026-09-17 裁决）。rotate 把住宅凭据连名字一起换掉
// （迁移用户的用户名 → 池里的 rNNN），所以对按账号匹配的 bui-c 等于换了账号：
// 重新导入只**新增**新节点、旧的留在原地，而旧凭据的门已切 deny ⇒ 旧的住宅节点显示已连接
// 但所有请求被拒。光说「重新导入」不说「切换」，当前节点还是旧的那条，用户就是
// 「连着但什么都打不开」。
// 而且**不能只说「导入完那一问答 y」**（2026-09-17 复核订正）：那一问问的是**第一个**新节点，
// 而 rotate 同时换 VLESS UUID、Reality 按 uuid 认账号 ⇒ 融合权益的用户 Reality 两条也各新增
// 一条，节点顺序又是 Reality直连 → Reality住宅 → HY2直连 → HY2住宅 ⇒ 第一个新节点通常是
// Reality 直连那条，答 y 等于把住宅出口静默丢掉。真正的出路是菜单 [1] 选节点或 bui-c switch。
// 节点名的具体形态是 bui-c 侧的命名规则，这里不写死。
// 「新增」也不是无条件的（2026-09-17 复核订正，与 README「服务端改了节点参数之后」同口径）：
// bui-c 的 HY2 账号看的是**用户名**（profiles::same_account），而 rotate 只换直连的密码、
// 不换用户名 ⇒ HY2 直连是原地更新、不新增；新增的只有住宅 HY2（凭据连名字一起换了）与
// 有 Reality 权益时那两条 Reality（uuid 换了）。不说清运维就会去列表里找一条根本不会出现的
// 「新的 HY2 直连」，甚至把已经原地更新过的那条当成旧节点删掉。
const _ROTATE_SWITCH =
    "仅节点入口：Linux 客户端（bui-c）导入后还要明确切换到新的住宅 HY2 节点：住宅凭据连名字一起换了，" +
    "重新导入只会新增新节点、旧的留在原地，而旧的住宅节点会一直显示已连接但所有请求被拒。" +
    "新增的只有住宅 HY2 与（有 Reality 权益时）两条 Reality；HY2 直连是原地更新、不新增" +
    "（换的只是密码、用户名没换）。" +
    "菜单 [3] 导入完那一问问的是第一个新节点（rotate 也换了 UUID，有 Reality 权益时那通常是 " +
    "Reality 直连），所以要用菜单 [1] 选节点，或 sudo bui-c switch <新的住宅 HY2 节点名>。";

// 住宅槽位那一列的 tooltip：端口固定（4.1 起住宅 HY2 单端口 + 整段跳跃由 nft 送进去），
// 再加一句门位语义。**到期 / 封禁的住宅 HY2 客户端仍会显示已连接，但所有请求会被拒绝**
// （门位 = deny，握手照旧成功），直连节点则在连接时就被拒 —— 这一句是运维唯一看得到的
// 解释，不写用户会以为「显示连着就是能用」。
function _resiGateHint(x) {
    const unavailable = _resiUnavailableHint(x);
    if (unavailable) return unavailable + '\n' + _RESI_REQUIRED_HINT;
    const port = 'HY2 住宅 :' + x.slotPort +
        (Array.isArray(x.slotHop) ? ' + ' + x.slotHop[0] + '-' + x.slotHop[1] : '');
    const g = x.hy2ResiGate;
    if (g === 'deny') {
        return port + '\n门位 deny：' + _DENY_SEMANTICS;
    }
    if (g === '未分配') {
        return port + '\n还没分到住宅 HY2 凭据：订阅里没有这个节点（下一轮收敛会补上）。';
    }
    if (g) return port + '\n门位 ' + g + '：放行，走本槽的住宅 IP 出网。';
    return port;
}

// Load data
// 返回 Promise：轮换凭据后要等用户列表刷新完再按新 token 重画配置弹窗（见 rotateSub）
function load() {
    const request = ++userListRequest;
    return Promise.all([api("/users"), api("/online"), api("/stats")]).then(([u, o, s]) => {
        if (request !== userListRequest) return false;
        $("#st-u").innerText = u.length;
        // 在线用户：`/api/online` 的值恒为 1（每人 0/1，量纲见 modules/panel/traffic.rs
        // 的「在线数的量纲」），所以键的个数就是在线用户数。4.0.x 这里累加的是
        // 「直连会话数 + 住宅连接条数 + 常数 1」，量纲混在一起，数字不代表任何东西。
        $("#st-o").innerText = Object.keys(o).length;

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
        syncOpenConfig(u);

        // 二维码现在本地生成（见 renderQR），无需预取外部图片

        $("#tb").innerHTML = u.map(x => {
            const on = o[x.username];
            const monthly = x.usage?.monthly?.[m] || 0;
            const total = x.usage?.total || 0;
            const exp = x.limits?.expiresAt ? new Date(x.limits.expiresAt) < new Date() : "";
            const tlim = x.limits?.trafficLimit;
            const over = tlim && total >= tlim;
            const unavailable = _resiUnavailableHint(x);
            const residentialBadge = unavailable
                ? ' <span class="tag" style="color:var(--danger)" title="' + esc(unavailable + '。' + _RESI_REQUIRED_HINT) + '">住宅不可用</span>'
                : '';
            // spec §6：**住宅 HY2 客户端仍会显示已连接，但所有请求会被拒绝**（门位切到
            // deny，握手照旧成功）；直连节点在连接时即被拒。运维只有这句话能解释
            // 「用户说还连着，为什么打不开网页」。
            const badge = (exp || over || x.disabled)
                ? ' <span class="tag" style="color:var(--danger)" title="' + esc(_DENY_SEMANTICS) + '">' +
                  (x.disabled ? '已停用' : exp ? '已过期' : '流量耗尽') + '</span>'
                : "";
            const proto = x.protocol || "hysteria2";
            const ptag = proto === "fusion" ? '<span class="proto-tag proto-sub">订阅</span>' :
                proto === "vless-reality" ? '<span class="proto-tag proto-vless">VLESS</span>' :
                    proto === "vless-ws-tls" ? '<span class="proto-tag proto-ws">WS</span>' :
                        '<span class="proto-tag proto-hy2">HY2</span>';

            return '<tr>' +
                '<td><div style="display:flex;align-items:center;gap:8px"><span style="font-weight:600">' + esc(x.username) + '</span>' + ptag + badge + residentialBadge + '</div></td>' +
                '<td><span class="tag ' + (on ? 'on' : '') + ' ">' + (on ? '在线' : '离线') + '</span></td>' +
                // v4 P3（spec §5.6）：这个用户走哪个住宅 IP。没有住宅权益就打「—」。
                '<td class="hide-m" style="font-family:monospace;color:var(--text-dim)">' +
                (unavailable ? '<span style="color:var(--danger)" title="' + esc(unavailable + '。' + _RESI_REQUIRED_HINT) + '">' + esc(unavailable) + '</span>' : x.slot == null ? '—' :
                    '<span title="' + esc(_resiGateHint(x)) + '">#' +
                    x.slot + (x.slotIp ? ' ' + esc(x.slotIp) : '') +
                    (x.hy2ResiGate === 'deny' ? ' <span class="tag" style="color:var(--danger)">拒绝</span>' :
                        x.hy2ResiGate === '未分配' ? ' <span class="tag">未分配</span>' : '') +
                    '</span>') +
                '</td>' +
                '<td class="hide-m" style="font-family:monospace;color:var(--text-dim)">' + sz(monthly) + '</td>' +
                '<td class="hide-m" style="font-family:monospace;color:var(--text-dim)">' + sz(total) + (tlim ? ' / ' + sz(tlim) : '') + '</td>' +
                '<td>' +
                '<div style="display:flex;gap:8px">' +
                '<button class="ibtn share" onclick="showU(\'' + esc(x.username).replace(/'/g, "\\'") + '\')" title="分享"><svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M10 13a5 5 0 007.54.54l3-3a5 5 0 00-7.07-7.07l-1.72 1.71"/><path d="M14 11a5 5 0 00-7.54-.54l-3 3a5 5 0 007.07 7.07l1.71-1.71"/></svg></button>' +
                '<button class="ibtn edit" onclick="editUser(\'' + esc(x.username).replace(/'/g, "\\'") + '\')" title="编辑"><svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M11 4H4a2 2 0 00-2 2v14a2 2 0 002 2h14a2 2 0 002-2v-7"/><path d="M18.5 2.5a2.121 2.121 0 013 3L12 15l-4 1 1-4 9.5-9.5z"/></svg></button>' +
                (on ? '<button class="ibtn warn" onclick="kick(\'' + esc(x.username).replace(/'/g, "\\'") + '\')" title="断开"><svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polygon points="13 2 3 14 12 14 11 22 21 10 12 10 13 2"/></svg></button>' : '') +
                '<button class="ibtn danger" onclick="del(\'' + esc(x.username).replace(/'/g, "\\'") + '\')" title="删除"><svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="3 6 5 6 21 6"/><path d="M19 6l-1 14a2 2 0 01-2 2H8a2 2 0 01-2-2L5 6"/><path d="M10 11v6"/><path d="M14 11v6"/><path d="M9 6V4a1 1 0 011-1h4a1 1 0 011 1v2"/></svg></button>' +
                '</div>' +
                '</td>' +
                '</tr>';
        }).join("");
        return true;
    }).catch(error => {
        if (request !== userListRequest) return false;
        if (currentShowUser) {
            managedRefreshPending = true;
            renderManagedProfile(currentShowUser);
        }
        throw error;
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
    const residential = $("#nresi")?.checked !== false; // 默认 true
    const managedEgress = $("#nmanaged-egress").value;
    if (!["vps", "residential"].includes(managedEgress)) return toast("请明确选择完整配置默认出口", 1);

    // v4：创建用户改为 JWT 保护的 POST /api/users（spec §4.3 改动 1）。
    // sni 与 speed 服务端会接受但忽略（v4 的 SNI 全局唯一、内核不支持按用户限速）。
    return api("/users", {
        method: "POST",
        body: JSON.stringify({
            username: u,
            password: p || undefined,
            days: parseFloat(d),
            traffic: parseFloat(t),
            monthly: parseFloat(m),
            protocol: proto,
            residential: residential,
            managed_egress: managedEgress,
            sni: customSni || undefined,
            speed: parseFloat(s)
        })
    }).then(r => {
        if (r.success) {
            closeM();
            toast("用户 " + u + " 已创建");
            return load();
        } else {
            toast(r.error || "操作失败", 1);
        }
    }).catch(e => toast(e.message || "创建失败", 1));
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

    const managed = x.managedProfile || {};
    const choices = managed.allowedChoices || [];
    const selected = managed.selectedEgress || "";
    const select = $("#edit-managed-egress");
    select.innerHTML = (selected ? "" : '<option value="">未选择（保持原值）</option>') +
        [...new Set([...choices, ...(selected ? [selected] : [])])].map(value =>
            '<option value="' + esc(value) + '"' + (!choices.includes(value) ? ' disabled' : '') + '>' +
            (value === 'vps' ? 'VPS 出口' : '住宅出口') + '</option>').join('');
    select.value = selected;
    select.dataset.original = selected;
    $("#edit-managed-hint").innerText = !selected ? "未选择；完整配置将返回 409。编辑其他字段可保持原值。" :
        !choices.includes(selected) ? "当前出口无权选择或不可用；已保留实际选择，不会自动切换。" :
        "选择只保存已授权的出口意图；出口故障时仍可能返回 503，不会自动切换身份。";
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

    const managedSelect = $("#edit-managed-egress");
    const managedEgress = managedSelect.value;
    if (managedEgress !== managedSelect.dataset.original && !["vps", "residential"].includes(managedEgress))
        return toast("请选择已授权出口；不能清空已有选择", 1);
    return api("/users/" + encodeURIComponent(origUsername), {
        method: "PUT",
        body: JSON.stringify({
            username: newUsername,
            password: newPassword || undefined,
            days: parseFloat(days),
            traffic: parseFloat(traffic),
            monthly: parseFloat(monthly),
            speed: parseFloat(speed),
            managed_egress: managedEgress !== managedSelect.dataset.original ? managedEgress : undefined
        })
    }).then(r => {
        if (r.success) {
            closeM();
            toast("用户 " + newUsername + " 已更新");
            return load();
        } else {
            toast(r.error || "更新失败", 1);
        }
    }).catch(e => toast(e.message || "更新失败", 1));
}

// 2026-09-14：四个免鉴权订阅端点的路径末段是每用户的随机订阅 token，不再是用户名
// （响应体里有 hy2 明文密码与 vless uuid，「域名 + 用户名」在旧口径下就等于订阅凭据）。
// 投影里取不到 token 时 subPath 返回 null，调用方只给一行提示，不拼出坏链接。
//
// 这一档在正常安装里不可达：三条建用户路径都自带 token，守护进程每次启动还会无条件补齐。
// 它真出现就只有一种成因 —— 面板与服务端版本不匹配（投影没发 subToken），而那时
// 「重启」和「点重置」都救不回来，所以文案不承诺任何自救动作（审查 6）。
const SUB_TOKEN_MISSING = "取不到该用户的订阅 token，请联系运维核对面板与服务端版本是否匹配";
const SINGLE_NODE_UNAVAILABLE = "该账户暂不提供单节点连接链接，请检查账户状态和通路授权。";

function subPath(x, kind) {
    return x && x.subToken ? "/api/" + kind + "/" + encodeURIComponent(x.subToken) : null;
}

// 单节点 URI 由服务端从同一份授权节点生成；前端只拼融合订阅的 token 地址。
function genUri(x) {
    // 融合订阅用户: 返回 v2rayN 原生订阅 URL (带备注)
    if (x.protocol === "fusion") {
        const path = subPath(x, "sub");
        if (!path) return "";
        // URL 末尾的 #备注 会被 v2rayNG 识别为订阅名称（不编码）
        return "https://" + location.host + path + "#" + x.username;
    }
    if (x.disabled || x.blocked || x.residentialUnavailable) return "";
    return typeof x.nodeUri === "string" ? x.nodeUri : "";
}

// 当前显示的用户名 (用于下载订阅)
let currentShowUser = null;

// A failed refresh/rotation retracts the primary action until an authoritative list arrives.
let managedRefreshPending = false;
const MANAGED_HELP = "适用 Mac v2rayN 7.25.4 / 官方 sing-box 1.14.2；Android / iOS 未验收。将完整链接导入同一订阅组并更新，不要反复导入。首次 TUN 需本地提权，应用重启后重新启用 TUN 并授权。mixed 默认 10808；若改过 GUI 端口，请一次对齐。首次手动启用自动更新，建议 60 分钟，避免短周期轮询。更新可能 reload，内容相同或 ETag 相同也不保证不重启；服务端错误不保证客户端保留旧 profile。下载后的账户与路径变化受下次刷新及核心会话生命周期约束。四格仅代表当前服务路径证据，未验收能力保持拒绝；不代表 Mac 全捕获或物理 IPv6 防泄漏已验收。停止核心或 TUN 不是 OS kill-switch；Custom 不承诺 Clash 统计与节点测速。";
function managedProfileUrl(x) {
    if (!x || managedRefreshPending || !/^[0-9a-f]{32}$/.test(x.subToken || "") ||
        x.disabled || x.blocked || x.managedProfile?.delivery !== "available") return "";
    return "https://" + location.host + "/api/profile/" + encodeURIComponent(x.subToken) +
        "/v2rayn-sb1142-macos?remarks=" + encodeURIComponent("BUI Managed macOS");
}
function renderManagedProfile(x) {
    const profile = x?.managedProfile;
    const messages = {missing_selection:"409：请管理员明确选择完整配置默认出口", account_unavailable:"403：账户不可用", route_unavailable:"503：当前出口暂不可用，保持原选择", available:"可获取配置"};
    const url = managedProfileUrl(x);
    $("#managed-copy").disabled = !url;
    $("#managed-url").innerText = url;
    $("#managed-status").innerText = managedRefreshPending ? "列表刷新失败或正在刷新，完整链接暂不可复制" :
        !x ? "当前用户记录已不可用" : !/^[0-9a-f]{32}$/.test(x.subToken || "") ? SUB_TOKEN_MISSING :
        (messages[profile?.delivery] || "完整配置状态不可用，请刷新列表") +
        (profile?.revision ? " · revision " + profile.revision : "");
    const labels = {unknown:"未验收", unsupported:"不支持", verified:"已验收"};
    $("#managed-capabilities").innerText = [["v4Tcp","IPv4 TCP"],["v4Udp","IPv4 UDP"],["v6Tcp","IPv6 TCP"],["v6Udp","IPv6 UDP"]]
        .map(([key,label]) => label + "：" + (labels[profile?.capabilities?.[key]] || "未验收")).join("；");
    $("#managed-help").innerText = MANAGED_HELP;
}
function copyManagedProfile() {
    const url = managedProfileUrl(currentShowUser);
    if (!url) return toast("完整配置暂不可复制，请检查账户状态并刷新列表", 1);
    return navigator.clipboard.writeText(url).then(() => toast("完整配置链接已复制，请导入同一订阅组"), () => toast("复制失败", 1));
}

// 本地生成二维码（vendored /qrcode.min.js）。替代以前把节点链接(含域名/uuid/密码)
// 发给境外 api.qrserver.com 渲染——既泄露凭据给第三方，国内还常刷不出码。
function renderQR(uri) {
    const el = $("#qrcode");
    if (!el) return;
    el.innerHTML = "";
    if (!uri) return;
    if (typeof QRCode === "undefined") { el.innerText = "二维码库未加载"; return; }
    new QRCode(el, { text: uri, width: 200, height: 200, correctLevel: QRCode.CorrectLevel.M });
    const img = el.querySelector("canvas, img");
    if (img) { img.style.display = "block"; img.style.borderRadius = "8px"; }
}

// Show user config
function showU(uname) {
    const x = allUsers.find(u => u.username === uname);
    if (!x) return;
    currentShowUser = x;
    const uri = genUri(x);
    const unavailable = _resiUnavailableHint(x);
    $("#uri").innerText = uri || (unavailable ? unavailable + '。' + _RESI_REQUIRED_HINT :
        x.protocol === "fusion" ? SUB_TOKEN_MISSING : SINGLE_NODE_UNAVAILABLE);

    // 融合订阅用户显示订阅链接
    if (x.protocol === "fusion") {
        $("#cfg-title").innerText = "融合订阅配置";
        $("#cfg-desc").innerHTML = "按授权提供 Hysteria2 与 VLESS 节点<br><small>可导入 v2rayN / v2rayNG / Shadowrocket / bui-c 客户端</small>" +
            (unavailable ? '<br><small style="color:var(--danger)">' + esc(unavailable + '。' + _RESI_REQUIRED_HINT) + '</small>' : '');

        // 显示二维码（本地生成，节点链接不外发）
        renderQR(uri);

        $("#cfg-buttons").innerHTML = `
            <button class="btn" onclick="copy()"><svg xmlns="http://www.w3.org/2000/svg" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" style="vertical-align:-1px;margin-right:5px"><rect x="9" y="9" width="13" height="13" rx="2" ry="2"/><path d="M5 15H4a2 2 0 01-2-2V4a2 2 0 012-2h9a2 2 0 012 2v1"/></svg>复制仅节点订阅</button>
            <button class="btn btn-secondary" onclick="copyClash()"><svg xmlns="http://www.w3.org/2000/svg" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" style="vertical-align:-1px;margin-right:5px"><path d="M21.44 11.05l-9.19 9.19a6 6 0 01-8.49-8.49l9.19-9.19a4 4 0 015.66 5.66l-9.2 9.19a2 2 0 01-2.83-2.83l8.49-8.48"/></svg>复制 Clash 订阅</button>
            <button class="btn btn-secondary" onclick="downloadSubscription()"><svg xmlns="http://www.w3.org/2000/svg" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" style="vertical-align:-1px;margin-right:5px"><line x1="16.5" y1="9.4" x2="7.5" y2="4.21"/><path d="M21 16V8a2 2 0 00-1-1.73l-7-4a2 2 0 00-2 0l-7 4A2 2 0 002 8v8a2 2 0 001 1.73l7 4a2 2 0 002 0l7-4A2 2 0 0021 16z"/><polyline points="3.27 6.96 12 12.01 20.73 6.96"/><line x1="12" y1="22.08" x2="12" y2="12"/></svg>下载旧 sing-box 配置</button>
        `;

        // 提示
        $("#cfg-hint").innerText = "扫码或复制链接导入客户端，支持 v2rayN / Shadowrocket / Clash Verge Rev";
    } else {
        // 单协议用户
        const protoName = x.protocol === "hysteria2" ? "Hysteria2" :
            x.protocol === "vless-reality" ? "VLESS-Reality" :
                x.protocol === "vless-ws-tls" ? "VLESS-WS" : x.protocol;

        $("#cfg-title").innerText = protoName + " 配置";
        $("#cfg-desc").innerText = uri ? "单协议客户端配置" : "当前没有可用的单节点连接链接";

        // 显示二维码（本地生成，节点链接不外发）
        renderQR(uri);

        // 按钮 - 根据协议类型显示
        let btnHtml = `<button class="btn" onclick="copy()"><svg xmlns="http://www.w3.org/2000/svg" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" style="vertical-align:-1px;margin-right:5px"><rect x="9" y="9" width="13" height="13" rx="2" ry="2"/><path d="M5 15H4a2 2 0 01-2-2V4a2 2 0 012-2h9a2 2 0 012 2v1"/></svg>复制仅节点链接</button>`;

        $("#cfg-buttons").innerHTML = uri ? btnHtml : '';
        $("#cfg-hint").innerText = uri ? "扫码或复制链接导入客户端" :
            unavailable ? unavailable + '。' + _RESI_REQUIRED_HINT : SINGLE_NODE_UNAVAILABLE;
    }

    $("#cfg-title").innerText = "服务端管理的完整配置";
    $("#cfg-desc").innerText = "Mac v2rayN 7.25.4 / sing-box 1.14.2 · 复制完整链接后导入";
    renderManagedProfile(x);
    openM("m-cfg");
}

// 弹窗打开期间凭据被轮换（另一个管理员会话、或服务器上的 CLI）时按新值重画（审查 5）：
// 订阅链接末段的 token、hy2 密码与 vless uuid 三样都是可轮换的凭据，旧值复制或下载出去
// 拿到的是 404（端点对作废 token 一律回「User not found」的不可区分口径），管理员不会
// 收到任何提示。以前末段是用户名、永不变，所以「弹窗打开期间不刷新」是安全的。
// 用户被删、改名或列表刷新失败时撤销动作，直到重新确认当前记录。
function syncOpenConfig(users) {
    const old = currentShowUser;
    if (!old || !$("#m-cfg").classList.contains("on")) { managedRefreshPending = false; return; }
    const fresh = users.find(u => u.username === old.username);
    if (!fresh) {
        currentShowUser = null;
        managedRefreshPending = false;
        renderManagedProfile(null);
        $("#uri").innerText = "当前用户记录已不可用";
        $("#cfg-buttons").innerHTML = "";
        renderQR("");
        return;
    }
    const recovering = managedRefreshPending;
    managedRefreshPending = false;
    const managedChanged = JSON.stringify(fresh.managedProfile) !== JSON.stringify(old.managedProfile);
    const credentialsChanged = !["subToken", "password", "uuid"].every(k => fresh[k] === old[k]);
    if (!recovering && !managedChanged && !credentialsChanged && fresh.nodeUri === old.nodeUri &&
        fresh.residentialUnavailable === old.residentialUnavailable && fresh.blocked === old.blocked && fresh.disabled === old.disabled) return;
    showU(fresh.username);
    // 第二个参数选的是警告样式（橙色三角），这是有意的：管理员屏幕上那条链接刚刚作废，
    // 已经发给用户的旧链接也一起废了，不是一条可以扫过去的普通告知。
    if (!recovering) toast(credentialsChanged ? "该用户的订阅链接与凭据已被重置，弹窗已按新值刷新" : "该用户的连接配置或出口状态已变化，弹窗已刷新", 1);
}

// Copy URI
function copy() {
    // 没有选中用户就别把 #uri 里剩的那份文本递出去：轮换后它是已作废的链接，
    // 端点回 404，而管理员收到的是一句「已复制」。下面两个出口同一门禁。
    if (!currentShowUser || managedRefreshPending) return toast("请先选择用户并刷新列表", 1);
    const fusion = currentShowUser.protocol === "fusion";
    if (fusion && !currentShowUser.subToken) return toast(SUB_TOKEN_MISSING, 1);
    const uri = genUri(currentShowUser);
    if (!uri) return toast(_resiUnavailableHint(currentShowUser) || SINGLE_NODE_UNAVAILABLE, 1);
    navigator.clipboard.writeText(uri);
    if (fusion) {
        toast("订阅链接已复制，可粘贴到 v2rayN / Shadowrocket");
    } else {
        toast("链接已复制到剪贴板");
    }
}

// 下载 sing-box 融合订阅配置
function downloadSubscription() {
    if (!currentShowUser || managedRefreshPending) return toast("请先选择用户并刷新列表", 1);
    const path = subPath(currentShowUser, "subscription");
    if (!path) return toast(SUB_TOKEN_MISSING, 1);
    window.open(path, "_blank");
    toast("正在下载 sing-box 配置...");
}

// 复制 Clash Verge Rev 订阅链接
function copyClash() {
    if (!currentShowUser || managedRefreshPending) return toast("请先选择用户并刷新列表", 1);
    const path = subPath(currentShowUser, "clash");
    if (!path) return toast(SUB_TOKEN_MISSING, 1);
    navigator.clipboard.writeText("https://" + location.host + path)
        .then(() => toast("Clash 订阅链接已复制，可导入 Clash Verge Rev"))
        .catch(() => toast("复制失败", 1));
}

// 重置订阅链接与凭据（2026-09-14）：POST /api/users/{name}/rotate 同时换随机订阅 token、
// hy2 密码与 vless uuid，并停用该用户的旧「用户名链接」。需要二次确认；
// 已有连接的关闭取决于服务端与核心生命周期。刷新列表后才能展示新凭据。
function rotateSub() {
    const x = currentShowUser;
    if (!x) return toast("请先选择用户", 1);
    if (!confirm("重置用户 " + x.username + " 的订阅链接与凭据？\n\n" +
        "将替换订阅链接、Hysteria2 密码与 VLESS UUID，旧链接将不可用于获取配置。" +
        "请在原订阅组替换新链接并更新。已有连接的关闭取决于服务端与核心生命周期。\n\n" + _ROTATE_SWITCH)) return;
    const btn = document.getElementById("cfg-rotate");
    // 文案原文在 index.html 里，这里捕获一次再还原：硬编码一份的话，改了 HTML 忘了改
    // 这里，重置一次按钮就悄悄换回旧文案。
    const label = btn ? btn.textContent : "";
    const done = () => { if (btn) { btn.disabled = false; btn.textContent = label; } };
    if (btn) { btn.disabled = true; btn.textContent = "重置中…"; }
    return api("/users/" + encodeURIComponent(x.username) + "/rotate", { method: "POST", body: JSON.stringify({}) })
        .then(r => {
            if (!r || !r.success) { done(); return toast((r && r.error) || "重置失败", 1); }
            // Keep the username for a later authoritative poll, but revoke every link
            // action immediately while the new token is not yet confirmed.
            ++userListRequest; // Invalidate all polls started before credential revocation.
            managedRefreshPending = true;
            renderManagedProfile(x);
            return load().then(updated => {
                done();
                if (!updated) return;
                showU(x.username);
                toast("已重置，请在原订阅组替换新链接并更新配置");
            }, () => {
                // Keep actions disabled; the next successful poll can recover this record.
                managedRefreshPending = true;
                renderManagedProfile(x);
                done();
                toast("已重置，但这一轮用户列表没刷新成功：弹窗里的链接稍后自动更新", 1);
            });
        })
        .catch(e => { done(); toast(e.message || "请求失败", 1); });
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
    // v3.5.6: 住宅 checkbox 语义随 protocol 变
    const wrap = $("#nresi-wrap");
    const title = $("#nresi-title");
    const hint = $("#nresi-hint");
    if (!wrap || !title || !hint) return;
    if (proto === "fusion") {
        wrap.style.display = "flex";
        title.textContent = "订阅包含住宅节点";
        hint.textContent = "勾选住宅后，住宅不可用时整份订阅不可用。请配置并启用住宅上游、完成出口绑定；需要仅直连时，请新建未勾选住宅的账户。";
    } else if (proto === "hysteria2") {
        wrap.style.display = "flex";
        title.textContent = "使用住宅版 (HY2)";
        hint.textContent = "勾选 → 节点指 :40000 hy2-residential 经住宅 IP；不勾 → :10000 直连";
    } else if (proto === "vless-reality") {
        wrap.style.display = "flex";
        title.textContent = "使用住宅版 (Reality)";
        hint.textContent = "勾选 → 节点指 :10002 vless-residential 经住宅 IP；不勾 → :10001 直连";
    } else {
        wrap.style.display = "none";  // ws-tls 无住宅版
    }
}

// Auto-init if token exists
if (tok) init();

// ─── 住宅 IP 出站 ─────────────────────────────────────────────────────────────

// v3.6.0 R10: 与 residential-helper.sh parse_url 同规则的纯解析（无 DOM，可单测）
// 返回 {host, port, username, password, scheme} 或 {error}
function parseResiInput(text) {
    let s = String(text == null ? "" : text).trim();
    if (!s) return { error: "请粘贴供应商给的代理" };
    if (s.length > 1 && ((s[0] === '"' && s.endsWith('"')) || (s[0] === "'" && s.endsWith("'")))) {
        s = s.slice(1, -1).trim();
    }
    const EG = "示例：socks5://user:pass@host:port 或 host:port:user:pass";
    let scheme = "auto", schemeGiven = false;
    // socks5h:// 是 socks5:// 的别名（供应商文档普遍这么写）；大小写不敏感
    if (/^socks5h:\/\//i.test(s)) { scheme = "socks5"; s = s.slice("socks5h://".length); schemeGiven = true; }
    else if (/^socks5:\/\//i.test(s)) { scheme = "socks5"; s = s.slice("socks5://".length); schemeGiven = true; }
    else if (/^http:\/\//i.test(s)) { scheme = "http"; s = s.slice("http://".length); schemeGiven = true; }
    const other = s.match(/^([A-Za-z][A-Za-z0-9+.-]*):\/\//);
    if (other) return { error: "不支持的协议 " + other[1] + "://，只支持 socks5:// 与 http://" };

    const at = s.lastIndexOf("@");
    const tail = at >= 0 ? s.slice(at + 1) : "";
    const parts = s.split(":");
    const csvLike = parts.length >= 4 && parts[0].indexOf("@") < 0 && /^[0-9]+$/.test(parts[1]);
    let host, port, username, password;   // port 末尾会归一成十进制字符串

    const atOk = at >= 0 && /^[^:@/]+:[0-9]+$/.test(tail);
    // 与 helper parse_url 同优先级：没写 scheme → CSV 优先（密码含 @ 的整行粘贴），写了 → @ 形态优先
    if (csvLike && (!schemeGiven || !atOk)) {
        // host:port:user:pass（供应商 IP 列表的整行，密码可含 : 与 @）
        host = parts[0];
        port = parts[1];
        username = parts[2];
        password = parts.slice(3).join(":");
    } else if (atOk) {
        // user:pass@host:port（以最后一个 @ 切分，密码可含 @）
        const userpass = s.slice(0, at);
        const ci = userpass.indexOf(":");
        if (ci < 0) return { error: "缺少密码，格式应为 user:pass@host:port" };
        username = userpass.slice(0, ci);
        password = userpass.slice(ci + 1);
        const li = tail.lastIndexOf(":");
        host = tail.slice(0, li);
        port = tail.slice(li + 1);
    } else if (at >= 0) {
        if (tail.indexOf(":") < 0) return { error: "缺少端口，格式应为 user:pass@host:port" };
        const li = tail.lastIndexOf(":");
        if (!/^[0-9]+$/.test(tail.slice(li + 1))) return { error: "端口不是数字：" + tail.slice(li + 1) };
        return { error: "认不出格式，" + EG };
    } else {
        if (parts.length < 2) return { error: "认不出格式，" + EG };
        if (!/^[0-9]+$/.test(parts[1])) return { error: "端口不是数字：" + parts[1] };
        return { error: "缺少用户名或密码，格式应为 host:port:user:pass" };
    }

    if (!host) return { error: "缺少主机，" + EG };
    if (!port) return { error: "缺少端口，" + EG };
    if (!/^[0-9]+$/.test(port)) return { error: "端口不是数字：" + port };
    // 与 helper parse_url 同口径：位数先按原串判（065535 这种补零后合法的也拒），再归一成十进制
    if (port.length > 5) return { error: "端口超出范围（1-65535）：" + port };
    const pn = parseInt(port, 10);
    if (pn < 1 || pn > 65535) return { error: "端口超出范围（1-65535）：" + port };
    port = String(pn);   // 08080 → 8080，预览显示的就是 helper 会落库的值
    if (!username) return { error: "缺少用户名" };
    if (!password) return { error: "缺少密码" };
    return { host, port, username, password, scheme };
}

// 上游类型文案：socks5 / http 是已确定的类型，auto 表示交给服务端探测
function _resiTypeLabel(t) {
    return t === "http" ? "HTTP" : t === "socks5" ? "SOCKS5" : "自动探测（SOCKS5 / HTTP）";
}

function _resiErr(msg) {
    const txt = $("#resi-error-text");
    if (txt) txt.textContent = msg;
    $("#resi-error").style.display = "flex";
}
function _resiClearErr() { $("#resi-error").style.display = "none"; }

// v3.5.0: 渲染住宅 URL 池表格
function renderResidentialUrls(urls) {
    const table = $("#resi-urls-table");
    if (!table) return;
    table.replaceChildren();
    if (!urls || !urls.length) {
        const empty = document.createElement("div");
        empty.className = "resi-urls-empty";
        empty.textContent = "暂无代理节点，点击「+ 添加代理」添加";
        table.appendChild(empty);
        return;
    }
    urls.forEach(u => {
        const row = document.createElement("div");
        row.className = "resi-url-row";
        const info = document.createElement("div");
        info.className = "resi-url-info";
        const nameLine = document.createElement("div");
        nameLine.className = "resi-url-nameline";
        const name = document.createElement("span");
        name.className = "resi-url-name";
        name.textContent = u.name || u.host;
        // v3.6.0 R10: 上游类型徽标（老条目无 type → SOCKS5）
        const type = u.type === "http" ? "http" : "socks5";
        const badge = document.createElement("span");
        badge.className = "resi-type-badge resi-type-" + type;
        badge.textContent = type === "http" ? "HTTP" : "SOCKS5";
        badge.title = type === "http" ? "上游是 HTTP 代理" : "上游是 SOCKS5 代理";
        nameLine.append(name, badge);
        const addr = document.createElement("span");
        addr.className = "resi-url-addr";
        addr.textContent = u.host + ":" + u.port + (u.username ? " (" + u.username + ")" : "");
        info.append(nameLine, addr);
        if (u.lastVerifiedIp) {
            const ip = document.createElement("span");
            ip.className = "resi-url-ip";
            ip.title = "上次校验的出口 IP";
            ip.textContent = "出口 " + u.lastVerifiedIp;
            info.appendChild(ip);
        }
        const delBtn = document.createElement("button");
        delBtn.className = "resi-icon-btn resi-del-btn";
        delBtn.title = "删除";
        delBtn.type = "button";
        delBtn.textContent = "✕";
        const hostPort = encodeURIComponent(u.host + ":" + u.port);
        delBtn.onclick = () => removeResidentialUrl(hostPort);
        row.append(info, delBtn);
        table.appendChild(row);
    });
}

let _resiPwShown = false;   // 预览区"显示/隐藏密码"状态

function openAddResiUrl() {
    _resiClearErr();
    const wrap = $("#resi-add-url-wrap");
    if (wrap) wrap.style.display = "";
    const inp = $("#resi-new-url");
    if (inp) { inp.value = ""; inp.focus(); }
    _resiPwShown = false;
    onResiInputChange();
}

function cancelAddResiUrl() {
    const wrap = $("#resi-add-url-wrap");
    if (wrap) wrap.style.display = "none";
    const inp = $("#resi-new-url");
    if (inp) inp.value = "";
    _resiPwShown = false;
    onResiInputChange();
    _resiClearErr();
}

// v3.6.0 R10: 输入即解析预览（主机/端口/用户名/密码打码/类型），解析失败给具体原因
function onResiInputChange() {
    const inp = $("#resi-new-url");
    const box = $("#resi-preview");
    if (!box) return;
    box.replaceChildren();
    const raw = inp ? inp.value : "";
    if (!raw.trim()) { box.style.display = "none"; box.className = "resi-preview"; return; }
    box.style.display = "";
    const p = parseResiInput(raw);
    if (p.error) {
        box.className = "resi-preview resi-preview-bad";
        const e = document.createElement("div");
        e.className = "resi-preview-err";
        e.textContent = p.error;
        box.appendChild(e);
        return;
    }
    box.className = "resi-preview";
    const row = (k, v, mono) => {
        const line = document.createElement("div");
        line.className = "resi-preview-row";
        const key = document.createElement("span");
        key.className = "resi-preview-key";
        key.textContent = k;
        const val = document.createElement("span");
        val.className = "resi-preview-val" + (mono ? " resi-preview-mono" : "");
        val.textContent = v;
        line.append(key, val);
        box.appendChild(line);
        return line;
    };
    row("主机", p.host, true);
    row("端口", p.port, true);
    row("用户名", p.username, true);
    const pwLine = row("密码", _resiPwShown ? p.password : "•".repeat(Math.min(p.password.length, 12)), true);
    const tog = document.createElement("button");
    tog.type = "button";
    tog.className = "resi-pw-toggle";
    tog.textContent = _resiPwShown ? "隐藏" : "显示";
    tog.onclick = () => { _resiPwShown = !_resiPwShown; onResiInputChange(); };
    pwLine.appendChild(tog);
    row("类型", _resiTypeLabel(p.scheme), false);
}

function addResidentialUrl() {
    const inp = $("#resi-new-url");
    const url = inp ? String(inp.value).trim() : "";
    _resiClearErr();
    // 明显解析不出来的先在前端拦掉（错误文案与 helper 同口径），别白等一次上游探测
    const parsed = parseResiInput(url);
    if (parsed.error) { _resiErr(parsed.error); return; }
    // v3.6.0 R10: 服务端要连上游真拨（auto 最坏两轮 ~25s），按钮进行态给用户预期
    const btn = $("#resi-add-confirm");
    const restore = () => {
        if (!btn) return;
        btn.disabled = false;
        btn.textContent = "校验并添加";
        btn.classList.remove("resi-btn-busy");
    };
    if (btn) {
        btn.disabled = true;
        btn.textContent = "正在连接上游校验，通常 30 秒内（最长 60 秒）…";
        btn.classList.add("resi-btn-busy");
    }
    return api("/residential/urls", { method: "POST", body: JSON.stringify({ url }) }).then(r => {
        restore();
        if (r.success) {
            cancelAddResiUrl();
            // 4.1 起改槽不动订阅（住宅 HY2 单端口 + 整段跳跃由 nft 送进去），
            // 所以这里没有「N 个用户需重新拉订阅」可报
            toast("节点已添加（" + _resiTypeLabel(r.type || "socks5") + "）");
            _resiReload();
        } else _resiErr(r.error || "添加失败");
    }).catch(e => { restore(); _resiErr(e.message || "请求失败"); });
}

function removeResidentialUrl(hostPort) {
    _resiClearErr();
    api("/residential/urls/" + hostPort, { method: "DELETE" }).then(r => {
        if (!r.success) { _resiErr(r.error || "移除失败"); return; }
        // 4.1 起删上游只是把那一槽的用户换个出口 IP，订阅内容一个字节都不变
        _resiReload();
        toast("节点已移除");
    }).catch(e => _resiErr(e.message || "请求失败"));
}

function toggleResidentialGlobal(checked) {
    api("/residential/global", { method: "POST", body: JSON.stringify({ global: checked }) }).then(r => {
        if (r.success) {
            const hint = $("#resi-global-hint");
            if (hint) hint.textContent = checked ? _RESI_GLOBAL_HINT : _RESI_SPLIT_HINT;
            toast(checked ? "完整配置已设为全走住宅" : "完整配置已设为域名分流；需要授权直连与住宅两条通路");
        } else {
            _resiErr(r.error || "切换失败");
            const tog = $("#resi-global-toggle");
            if (tog) tog.checked = !checked;
        }
    }).catch(e => {
        _resiErr(e.message || "请求失败");
        const tog = $("#resi-global-toggle");
        if (tog) tog.checked = !checked;
    });
}

function _resiReload() {
    api("/residential").then(r => {
        renderResidentialUrls(r.urls || []);
        const disBtn = $("#resi-disable-btn");
        const tog = $("#resi-global-toggle");
        const hint = $("#resi-global-hint");
        if (disBtn) disBtn.style.display = (r.enabled ? "" : "none");
        if (tog) tog.checked = !!r.global;
        if (hint) hint.textContent = r.global ? _RESI_GLOBAL_HINT : _RESI_SPLIT_HINT;
    }).catch(() => {});
}

// v3.5.3: 住宅代理总开关（启用/禁用）
function toggleResidentialEnabled(checked) {
    _resiClearErr();
    if (checked) {
        // 启用：检查 urls 池非空，触发 helper reapply（设 enabled=true）
        api("/residential/enable", { method: "POST" }).then(r => {
            if (r.success) { toast("住宅代理已启用"); openResi(); }
            else { _resiErr(r.error || "启用失败 — 请先在池里添加至少 1 个 URL"); openResi(); }
        }).catch(e => { _resiErr(e.message || "请求失败"); openResi(); });
    } else {
        api("/residential", { method: "DELETE" }).then(r => {
            if (r.success) { toast("住宅代理已禁用，住宅连接将被拒绝"); openResi(); }
            else { _resiErr(r.error || "禁用失败"); openResi(); }
        }).catch(e => { _resiErr(e.message || "请求失败"); openResi(); });
    }
}

function openResi() {
    const statusEl  = $("#resi-status");
    const disBtn    = $("#resi-disable-btn");
    const domainsEl = $("#resi-domains");
    const countEl   = $("#resi-domains-count");

    statusEl.className = "resi-status-card";
    statusEl.textContent = "";
    const shimmer = document.createElement("div");
    shimmer.className = "resi-shimmer-line";
    shimmer.style.width = "55%";
    statusEl.appendChild(shimmer);

    const table = $("#resi-urls-table");
    if (table) table.replaceChildren();
    cancelAddResiUrl();
    _resiClearErr();
    $("#resi-domains-details").removeAttribute("open");
    openM("m-resi");

    api("/residential").then(r => {
        statusEl.textContent = "";
        const row = document.createElement("div");
        row.className = "resi-status-row";
        const dot = document.createElement("span");
        dot.className = "resi-dot " + (r.enabled ? "active" : "inactive");
        const label = document.createElement("span");
        label.className = "resi-status-label " + (r.enabled ? "active" : "inactive");
        label.textContent = r.enabled ? "住宅池已启用" : "住宅池未启用，住宅连接将被拒绝";
        // v3.5.3: 总开关 toggle — 点亮启用住宅，禁用时回 disable
        const masterTog = document.createElement("label");
        masterTog.className = "switch";
        masterTog.style.cssText = "margin-left:auto";
        masterTog.title = r.enabled ? "禁用后住宅连接将被拒绝" : "点击启用（需池中有至少 1 个 URL）";
        const masterInp = document.createElement("input");
        masterInp.type = "checkbox";
        masterInp.checked = !!r.enabled;
        masterInp.onchange = () => toggleResidentialEnabled(masterInp.checked);
        const masterSlider = document.createElement("span");
        masterSlider.className = "slider";
        masterTog.append(masterInp, masterSlider);
        row.append(dot, label, masterTog);

        if (r.enabled) {
            statusEl.className = "resi-status-card resi-status-enabled";
            const ip = r.lastVerifiedIp || "-";
            const pill = document.createElement("span");
            pill.className = "resi-ip-pill";
            pill.title = "点击复制 IP";
            pill.textContent = ip;
            pill.onclick = () => {
                navigator.clipboard.writeText(ip).then(() => {
                    pill.classList.add("copied");
                    setTimeout(() => pill.classList.remove("copied"), 1400);
                }).catch(() => {});
            };
            row.appendChild(pill);
            statusEl.appendChild(row);
            if (r.lastVerifiedIspInfo) {
                const isp = document.createElement("div");
                isp.className = "resi-isp";
                isp.textContent = r.lastVerifiedIspInfo;
                statusEl.appendChild(isp);
            }
            if (disBtn) disBtn.style.display = "";
        } else {
            statusEl.className = "resi-status-card";
            statusEl.appendChild(row);
            if (disBtn) disBtn.style.display = "none";
        }

        renderResidentialUrls(r.urls || []);
        const tog = $("#resi-global-toggle");
        const hint = $("#resi-global-hint");
        if (tog) tog.checked = !!r.global;
        if (hint) hint.textContent = r.global ? _RESI_GLOBAL_HINT : _RESI_SPLIT_HINT;

        _resiRenderDomains(r);
    }).catch(() => {
        statusEl.className = "resi-status-card";
        statusEl.textContent = "";
        const errMsg = document.createElement("span");
        errMsg.style.cssText = "color:var(--danger);font-size:13px";
        errMsg.textContent = "状态获取失败";
        statusEl.appendChild(errMsg);
    });
}

// v3.6.2 R12: 分流域名区的渲染 —— 标签要把"跟随默认"与"自定义"摆明。
// domains 为 null/空时服务端回的是生效默认表（domainsFollowDefault=true），编辑框照样填上，
// 用户可以看着它改；但只要没点保存，这台服务器仍然跟随默认，版本扩充会自动生效。
function _resiRenderDomains(r) {
    const domainsEl = $("#resi-domains");
    const countEl = $("#resi-domains-count");
    const list = (r && Array.isArray(r.domains)) ? r.domains : [];
    if (domainsEl) domainsEl.value = list.length ? list.join("\n") : "";
    if (countEl) {
        countEl.textContent = list.length
            ? ((r.domainsFollowDefault === false ? "自定义" : "跟随默认") + "（" + list.length + " 条）")
            : "";
        countEl.title = r && r.domainsFollowDefault === false
            ? "这台服务器用的是自定义关键字表，后续版本扩充默认表不会自动生效；点「恢复默认」可以回到跟随默认"
            : "跟随版本内置的默认表，升级后自动跟着扩充";
    }
}

// 回到"跟随默认"：写 domains = null，而不是把当时的默认表固化成自定义
function resetResidentialDomains() {
    _resiClearErr();
    api("/residential", { method: "POST", body: JSON.stringify({ reset: true }) }).then(r => {
        if (r.success) {
            toast("已恢复默认分流域名（今后跟随版本更新）");
            _resiRenderDomains({ domains: r.domains || [], domainsFollowDefault: true });
        } else _resiErr(r.error || "恢复默认失败");
    }).catch(e => _resiErr(e.message || "请求失败"));
}

function _parseDomains() {
    const raw = $("#resi-domains").value.trim();
    if (!raw) return null;
    return raw.split("\n").map(d => d.trim()).filter(Boolean);
}

function saveDomainsOnly() {
    const domains = _parseDomains();
    _resiClearErr();
    if (!domains || !domains.length) { _resiErr("域名列表不能为空"); return; }
    api("/residential", { method: "POST", body: JSON.stringify({ domains }) }).then(r => {
        if (r.success) toast("分流域名已更新");
        else _resiErr(r.error || "更新失败");
    }).catch(e => {
        _resiErr(e.message || "请求失败");
    });
}

function disableResi() {
    _resiClearErr();
    api("/residential", { method: "DELETE" }).then(r => {
        if (r.success) { closeM(); toast("住宅 IP 已禁用，住宅连接将被拒绝"); }
        else _resiErr(r.error || "禁用失败");
    }).catch(e => {
        _resiErr(e.message || "请求失败");
    });
}

// ─── 系统状态卡片：住宅 IP 健康 + hy2 watchdog ───────────────────────────────

function _sysFmt(v) {
    return (v === null || v === undefined || v === "") ? "—" : String(v);
}

function _sysClear(el) {
    while (el.firstChild) el.removeChild(el.firstChild);
}

function _sysShimmer(el) {
    _sysClear(el);
    const a = document.createElement("div");
    a.className = "resi-shimmer-line";
    a.style.cssText = "width:60%;margin-bottom:10px";
    const b = document.createElement("div");
    b.className = "resi-shimmer-line";
    b.style.width = "40%";
    el.appendChild(a);
    el.appendChild(b);
}

function _sysErr(el, msg, retryFn) {
    _sysClear(el);
    const wrap = document.createElement("div");
    wrap.className = "sysstat-err";
    const ico = document.createElement("span");
    ico.className = "sysstat-err-ico";
    ico.textContent = "!";
    const text = document.createElement("span");
    text.textContent = msg;
    const btn = document.createElement("button");
    btn.className = "btn btn-secondary sysstat-retry-btn";
    btn.textContent = "重试";
    btn.onclick = retryFn;
    wrap.append(ico, text, btn);
    el.appendChild(wrap);
}

function _sysKv(label, valueNode) {
    const row = document.createElement("div");
    row.className = "sysstat-kv";
    const k = document.createElement("span");
    k.className = "sysstat-k";
    k.textContent = label;
    const v = document.createElement("span");
    v.className = "sysstat-v";
    if (typeof valueNode === "string") v.textContent = valueNode;
    else v.appendChild(valueNode);
    row.append(k, v);
    return row;
}

function _sysTag(text, kind) {
    const t = document.createElement("span");
    t.className = "sysstat-tag" + (kind ? (" " + kind) : "");
    t.textContent = text;
    return t;
}

function loadResiHealth() {
    const body = document.getElementById("sys-resi-body");
    const btn  = document.getElementById("resi-health-refresh");
    if (!body) return;
    _sysShimmer(body);
    if (btn) { btn.disabled = true; btn.textContent = "检查中…"; }

    api("/residential/health").then(r => {
        if (btn) { btn.disabled = false; btn.textContent = "检查"; }
        if (!r || r.error) {
            _sysErr(body, "读取失败", loadResiHealth);
            return;
        }
        _sysClear(body);

        if (!r.enabled) {
            const row = document.createElement("div");
            row.className = "sysstat-row";
            const dot = document.createElement("span");
            dot.className = "resi-dot inactive";
            const lbl = document.createElement("span");
            lbl.className = "sysstat-label-dim";
            lbl.textContent = "住宅未启用";
            row.append(dot, lbl);
            body.appendChild(row);
            body.appendChild(_sysKv("分流关键词", _sysFmt(r.domains_count) + " 个"));
            return;
        }

        const ip   = r.current_egress_ip_test;
        const isp  = r.via_proxy_isp;
        const type = r.egress_ip_type || "unknown";

        let dotClass = "active";
        let stateText = "正常";
        if (!ip) { dotClass = "warn"; stateText = "待测"; }
        else if (type && /IDC|机房/i.test(type)) { dotClass = "warn"; stateText = "非住宅"; }
        else if (type === "unknown") { dotClass = "warn"; stateText = "类型未知"; }

        const row = document.createElement("div");
        row.className = "sysstat-row";
        const dot = document.createElement("span");
        dot.className = "resi-dot " + dotClass;
        const lbl = document.createElement("span");
        lbl.className = "sysstat-label";
        lbl.textContent = stateText;
        row.append(dot, lbl);
        if (ip) {
            const pill = document.createElement("span");
            pill.className = "resi-ip-pill";
            pill.title = "出口 IP";
            pill.textContent = ip;
            row.appendChild(pill);
        }
        body.appendChild(row);

        body.appendChild(_sysKv("ISP", _sysFmt(isp)));
        const typeKind = dotClass === "active" ? "good" : (dotClass === "warn" ? "warn" : "");
        body.appendChild(_sysKv("IP 类型", _sysTag(_sysFmt(type), typeKind)));
        body.appendChild(_sysKv("分流关键词", _sysFmt(r.domains_count) + " 个"));
        // v4 P3: 选路原因与「更优候选」防抖进度（后端 selection_reason / switch_improve_*）
        if (r.selected_reason) body.appendChild(_sysKv("选路原因", r.selected_reason));
        if (r.switch_improve_needed) {
            const need = r.switch_improve_needed;
            const done = r.switch_improve_rounds || 0;
            body.appendChild(_sysKv("切换条件", r.switch_improve_candidate
                ? r.switch_improve_candidate + " 已连续 " + done + "/" + need + " 轮更优"
                : "无更优候选（0/" + need + " 轮）"));
        }

        // v3.6.0 R7: 中继成员表 —— 真源是 singbox-relay.json 的 socks 出站（与巡检同源），
        // 高亮行是 selector 当前选中的出口；巡检列是 .resi-health-state.json 的迟滞判定
        const members = Array.isArray(r.members) ? r.members : [];
        if (members.length) {
            const tbl = document.createElement("div");
            tbl.className = "resi-members";
            const head = document.createElement("div");
            head.className = "resi-member resi-member-head";
            ["线路", "上游", "巡检", "延迟", "速度", "UDP", "出口 IP", "类型"].forEach(t => {
                const c = document.createElement("span");
                c.textContent = t;
                head.appendChild(c);
            });
            tbl.appendChild(head);
            members.forEach(m => {
                const selected = !!r.selected && m.tag === r.selected;
                const row = document.createElement("div");
                row.className = "resi-member" + (selected ? " selected" : "");
                if (selected) row.title = "当前选中的出口";
                const eg = m.egress || {};
                const cTag = document.createElement("span");
                cTag.textContent = m.tag || "-";
                const cUp = document.createElement("span");
                cUp.textContent = (m.host || "-") + (m.port ? ":" + m.port : "");
                cUp.title = cUp.textContent;
                const cState = document.createElement("span");
                const chip = _sysTag(m.active ? "健康" : "已剔除", m.active ? "good" : "bad");
                chip.title = m.active
                    ? "连续健康 " + (m.okstreak || 0) + " 轮"
                    : "连续不达标 " + (m.failstreak || 0) + " 轮，已从可选线路里剔除";
                cState.appendChild(chip);
                // v4 P3: 延迟 / 速度 / UDP 三列。**没测过一律打「—」**，绝不打 0：
                // 「未知」与「0 毫秒 / 0 Mbps」在面板上是两回事（后者会被读成最优）
                const cLat = document.createElement("span");
                cLat.textContent = m.latency_p50_ms == null ? "—" : m.latency_p50_ms + "ms";
                cLat.title = "p50 " + _sysFmt(m.latency_p50_ms) + " / p95 "
                    + _sysFmt(m.latency_p95_ms) + " ms（到网关 TCP p50 "
                    + _sysFmt(m.tcp_p50_ms) + " ms）";
                const cSpd = document.createElement("span");
                cSpd.textContent = m.down_mbps == null ? "—" : ("↓" + m.down_mbps.toFixed(1));
                cSpd.title = "下行 " + _sysFmt(m.down_mbps) + " / 上行 " + _sysFmt(m.up_mbps)
                    + " Mbps，测于 " + _sysFmt(m.speed_at) + (m.speed_note ? "（" + m.speed_note + "）" : "");
                const cUdp = document.createElement("span");
                cUdp.textContent = m.udp_ok == null ? "—" : (m.udp_ok ? "通" : "不通");
                cUdp.title = m.udp_ok
                    ? "UDP 出口 " + _sysFmt(m.udp_exit_ip) + "，p50 " + _sysFmt(m.udp_p50_ms) + " ms"
                    : (m.udp_note || "还没探过 UDP");
                const cIp = document.createElement("span");
                cIp.textContent = eg.ip || "—";
                const cType = document.createElement("span");
                cType.textContent = eg.type || "unknown";
                cType.title = eg.isp || "";
                row.append(cTag, cUp, cState, cLat, cSpd, cUdp, cIp, cType);
                tbl.appendChild(row);
            });
            body.appendChild(tbl);
        }

        // v4 P3（spec §5.6）：按槽列出 IP、当前实际出口、用户数与用户名。
        // 与成员表分开：成员表是「池里有哪些 IP」，槽位表是「哪个用户群走哪个 IP」。
        const slots = Array.isArray(r.slots) ? r.slots : [];
        if (slots.length) {
            const tbl = document.createElement("div");
            tbl.className = "resi-members";
            const head = document.createElement("div");
            head.className = "resi-member resi-member-head";
            ["槽", "本槽 IP", "当前出口", "用户", "延迟", "HY2 端口", "用户名"].forEach(t => {
                const c = document.createElement("span");
                c.textContent = t;
                head.appendChild(c);
            });
            tbl.appendChild(head);
            slots.forEach(s => {
                const row = document.createElement("div");
                row.className = "resi-member" + (s.borrowed ? " warn" : "");
                const cIdx = document.createElement("span");
                cIdx.textContent = "#" + s.index;
                cIdx.title = "selector " + _sysFmt(s.selector) + "，中继入站 127.0.0.1:" + _sysFmt(s.relay_port);
                const cIp = document.createElement("span");
                cIp.textContent = s.ip || "—";
                cIp.title = _sysFmt(s.host) + ":" + _sysFmt(s.port);
                const cNow = document.createElement("span");
                // 「借用中」必须一眼看出来：它意味着这批用户此刻不在自己的 IP 上
                const tag = s.active_tag || "—";
                cNow.appendChild(_sysTag(
                    tag + (s.pinned ? "（钉住）" : (s.borrowed ? "（借用）" : "")),
                    s.pinned ? "warn" : (s.borrowed ? "warn" : "good")));
                cNow.title = s.borrowed
                    ? "借用自 " + _sysFmt(s.borrowed_from) + "；本槽恢复 "
                      + _sysFmt(s.back_rounds) + "/" + _sysFmt(s.back_rounds_needed) + " 轮后切回"
                    : "正在用本槽自己的 IP";
                const cN = document.createElement("span");
                cN.textContent = _sysFmt(s.user_count);
                const cLat = document.createElement("span");
                cLat.textContent = s.latency_p50_ms == null ? "—" : s.latency_p50_ms + "ms";
                const cPort = document.createElement("span");
                const hop = Array.isArray(s.hop) ? s.hop : [];
                cPort.textContent = _sysFmt(s.hy2_port) + " + " + _sysFmt(hop[0]) + "-" + _sysFmt(hop[1]);
                // 4.1 起住宅 HY2 只有一个监听端口、整段跳跃由 `table inet bui` 送进去，
                // 所以每一槽显示的都是同一对值（与用户列表那一列、与三种订阅同源）。
                cPort.title = "住宅用户在订阅里拿到的 HY2 端口与跳跃区间（4.1 起与槽位无关，每一槽都一样）";
                const cUsers = document.createElement("span");
                cUsers.textContent = (s.users || []).join("、") || "—";
                cUsers.title = cUsers.textContent;
                row.append(cIdx, cIp, cNow, cN, cLat, cPort, cUsers);
                tbl.appendChild(row);
            });
            body.appendChild(tbl);
        }
    }).catch(() => {
        if (btn) { btn.disabled = false; btn.textContent = "检查"; }
        _sysErr(body, "读取失败", loadResiHealth);
    });
}

// v4 P3（spec §5.6）：把住宅用户在各槽间均匀重排。约 1 秒后（对账 + gRPC 收口）生效，
// xray 不重启、现有连接不断。**4.1 起被挪动的用户不用重新导入订阅**：住宅 HY2 只有一个
// 监听端口、整段跳跃由 `table inet bui` 送进去，端口与区间与槽位无关，重排只换出口 IP。
function rebalanceSlots() {
    if (!confirm("按槽重排全部住宅用户？\n约 1 秒后生效（不重启 xray）；用户不必重新获取订阅。")) return;
    const btn = document.getElementById("resi-rebalance");
    if (btn) { btn.disabled = true; btn.textContent = "重排中…"; }
    api("/residential/rebalance", { method: "POST" }).then(r => {
        if (btn) { btn.disabled = false; btn.textContent = "按槽重排"; }
        if (r && r.success) {
            toast("已重排 " + (r.moved || 0) + " 个用户" +
                (r.xray_rules_pending ? "，约 1 秒后槽路由生效（不重启 xray）" : ""));
            loadResiHealth();
            load();   // 用户列表的槽位列跟着刷新
        } else {
            toast((r && r.error) || "重排失败", true);
        }
    }).catch(e => {
        if (btn) { btn.disabled = false; btn.textContent = "按槽重排"; }
        toast(e.message || "请求失败", true);
    });
}

function loadWatchdogStatus() {
    const body = document.getElementById("sys-wd-body");
    const btn  = document.getElementById("wd-refresh");
    if (!body) return;
    _sysShimmer(body);
    if (btn) { btn.disabled = true; btn.textContent = "刷新中…"; }

    api("/hy2/watchdog/status").then(r => {
        if (btn) { btn.disabled = false; btn.textContent = "刷新"; }
        if (!r || r.error) {
            _sysErr(body, "读取失败", loadWatchdogStatus);
            return;
        }
        _sysClear(body);

        const active = !!r.watchdog_active;
        const failCount = r.fail_count || 0;
        const logs = Array.isArray(r.log_recent_lines) ? r.log_recent_lines : [];

        const row = document.createElement("div");
        row.className = "sysstat-row";
        const dot = document.createElement("span");
        dot.className = "resi-dot " + (active ? "active" : "inactive");
        const lbl = document.createElement("span");
        lbl.className = "sysstat-label";
        lbl.textContent = active ? "运行中" : "未启用";
        row.append(dot, lbl);
        body.appendChild(row);

        body.appendChild(_sysKv("下次检查", _sysFmt(r.next_run_at)));
        body.appendChild(_sysKv("上次检查", _sysFmt(r.last_run_at)));
        body.appendChild(_sysKv("失败计数",
            failCount > 0 ? _sysTag(failCount + " 次", "bad") : _sysTag("0", "good")));

        if (logs.length) {
            const det = document.createElement("details");
            det.className = "sysstat-logs";
            const sum = document.createElement("summary");
            sum.textContent = "最近日志（" + logs.length + " 行）";
            const pre = document.createElement("pre");
            pre.textContent = logs.join("\n");
            det.append(sum, pre);
            body.appendChild(det);
        }
    }).catch(() => {
        if (btn) { btn.disabled = false; btn.textContent = "刷新"; }
        _sysErr(body, "读取失败", loadWatchdogStatus);
    });
}

// 日志哨兵事件（spec §5.7）：最近 20 条，新的在前。文本全部来自日志与预案，一律 textContent
function loadIncidents() {
    const body = document.getElementById("sys-inc-body");
    const btn  = document.getElementById("inc-refresh");
    if (!body) return;
    _sysShimmer(body);
    if (btn) { btn.disabled = true; btn.textContent = "刷新中…"; }

    api("/incidents?limit=20").then(r => {
        if (btn) { btn.disabled = false; btn.textContent = "刷新"; }
        if (!r || r.error || !Array.isArray(r.incidents)) {
            _sysErr(body, "读取失败", loadIncidents);
            return;
        }
        _sysClear(body);
        if (!r.incidents.length) {
            const row = document.createElement("div");
            row.className = "sysstat-row";
            const lbl = document.createElement("span");
            lbl.className = "sysstat-label-dim";
            lbl.textContent = "暂无事件";
            row.appendChild(lbl);
            body.appendChild(row);
            return;
        }
        const kinds = { error: "bad", warn: "warn", info: "good" };
        const names = { error: "告警", warn: "警告", info: "信息" };
        r.incidents.forEach(i => {
            const wrap = document.createElement("span");
            const text = document.createElement("span");
            text.textContent = _sysFmt(i.subject) + " · " + _sysFmt(i.result);
            text.title = _sysFmt(i.signature) + " → " + _sysFmt(i.action);
            wrap.append(_sysTag(names[i.level] || _sysFmt(i.level), kinds[i.level] || ""),
                        document.createTextNode(" "), text);
            body.appendChild(_sysKv(_sysFmt(i.at).replace("T", " ").replace("Z", ""), wrap));
        });
        body.appendChild(_sysKv("总数", String(r.total)));
    }).catch(() => {
        if (btn) { btn.disabled = false; btn.textContent = "刷新"; }
        _sysErr(body, "读取失败", loadIncidents);
    });
}

// 在 dashboard 初始化后自动加载一次
function initSysStatusOnce() {
    if (document.getElementById("sys-resi-body")) loadResiHealth();
    if (document.getElementById("sys-wd-body"))   loadWatchdogStatus();
    if (document.getElementById("sys-inc-body")) loadIncidents();
}
