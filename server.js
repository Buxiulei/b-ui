const http=require("http"),fs=require("fs"),crypto=require("crypto"),{execSync,exec}=require("child_process");
const CONFIG={port:process.env.ADMIN_PORT||8080,adminPassword:process.env.ADMIN_PASSWORD||"admin123",
jwtSecret:process.env.JWT_SECRET||crypto.randomBytes(32).toString("hex"),
hysteriaConfig:process.env.HYSTERIA_CONFIG||"/opt/hysteria/config.yaml",usersFile:process.env.USERS_FILE||"/opt/hysteria/users.json",trafficPort:9999};

// --- Security: Rate Limiting & Audit ---
const loginAttempts={};const RATE_LIMIT={maxAttempts:5,windowMs:300000};
function checkRateLimit(ip){const now=Date.now(),rec=loginAttempts[ip];if(!rec)return true;if(now-rec.first>RATE_LIMIT.windowMs){delete loginAttempts[ip];return true}return rec.count<RATE_LIMIT.maxAttempts}
function recordAttempt(ip,success){const now=Date.now(),rec=loginAttempts[ip];if(!rec)loginAttempts[ip]={first:now,count:1};else rec.count++;if(success)delete loginAttempts[ip];log("AUDIT",ip+" login "+(success?"SUCCESS":"FAILED")+" (attempts: "+(loginAttempts[ip]?.count||0)+")")}
function getClientIP(req){return req.headers["x-forwarded-for"]?.split(",")[0].trim()||req.socket.remoteAddress||"unknown"}

// --- Backend Logic ---
function log(l,m){console.log("["+new Date().toISOString()+"] ["+l+"] "+m)}
function genToken(d){const p=Buffer.from(JSON.stringify({...d,exp:Date.now()+864e5,iat:Date.now()})).toString("base64");
return p+"."+crypto.createHmac("sha256",CONFIG.jwtSecret).update(p).digest("hex")}
function verifyToken(t){try{const[p,s]=t.split(".");if(s!==crypto.createHmac("sha256",CONFIG.jwtSecret).update(p).digest("hex"))return null;
const d=JSON.parse(Buffer.from(p,"base64").toString());return d.exp<Date.now()?null:d}catch{return null}}
function parseBody(r){return new Promise(s=>{let b="";r.on("data",c=>b+=c);r.on("end",()=>{try{s(b?JSON.parse(b):{})}catch{s({})}})})}
function sendJSON(r,d,s=200,headers={}){r.writeHead(s,{"Content-Type":"application/json","Access-Control-Allow-Origin":"*","Access-Control-Allow-Methods":"*","Access-Control-Allow-Headers":"*",...headers});r.end(JSON.stringify(d))}
function loadUsers(){try{return fs.existsSync(CONFIG.usersFile)?JSON.parse(fs.readFileSync(CONFIG.usersFile,"utf8")):[]}catch{return[]}}
function saveUsers(u){try{fs.writeFileSync(CONFIG.usersFile,JSON.stringify(u,null,2));updateConfig(u);return true}catch{return false}}
function updateConfig(users){try{let c=fs.readFileSync(CONFIG.hysteriaConfig,"utf8");
const up=users.reduce((a,u)=>{a[u.username]=u.password;return a},{});
const auth="auth:\n  type: userpass\n  userpass:\n"+Object.entries(up).map(([u,p])=>"    "+u+": "+p).join("\n");
c=c.replace(/auth:[\s\S]*?(?=\n[a-zA-Z]|$)/,auth+"\n\n");
fs.writeFileSync(CONFIG.hysteriaConfig,c);execSync("systemctl restart hysteria-server",{stdio:"pipe"})}catch(e){log("ERROR",e.message)}}
function getConfig(){try{const c=fs.readFileSync(CONFIG.hysteriaConfig,"utf8");
const dm=c.match(/domains:\s*\n\s*-\s*(\S+)/),pm=c.match(/listen:\s*:?(\d+)/);
return{domain:dm?dm[1]:"localhost",port:pm?pm[1]:"443"}}catch{return{domain:"localhost",port:"443"}}}
function fetchStats(ep){return new Promise(s=>{const r=http.request({hostname:"127.0.0.1",port:CONFIG.trafficPort,path:ep,method:"GET"},
res=>{let d="";res.on("data",c=>d+=c);res.on("end",()=>{try{s(JSON.parse(d))}catch{s({})}})});
r.on("error",()=>s({}));r.setTimeout(3e3,()=>{r.destroy();s({})});r.end()})}
function postStats(ep,b){return new Promise(s=>{const d=JSON.stringify(b);const r=http.request({hostname:"127.0.0.1",port:CONFIG.trafficPort,path:ep,method:"POST",headers:{"Content-Type":"application/json","Content-Length":Buffer.byteLength(d)}},
res=>s(res.statusCode===200));r.on("error",()=>s(false));r.write(d);r.end()})}

// --- Traffic Tracking ---
function getCurrentMonth(){return new Date().toISOString().slice(0,7)}
function updateUserTraffic(stats){const users=loadUsers();let changed=false;
Object.entries(stats).forEach(([username,{tx,rx}])=>{const u=users.find(x=>x.username===username);if(u){
if(!u.usage)u.usage={total:0,monthly:{}};const m=getCurrentMonth();
u.usage.total=(u.usage.total||0)+tx+rx;u.usage.monthly[m]=(u.usage.monthly[m]||0)+tx+rx;changed=true}});
if(changed){try{fs.writeFileSync(CONFIG.usersFile,JSON.stringify(users,null,2))}catch(e){log("ERROR","Save traffic: "+e.message)}}}
function checkUserLimits(u){const now=Date.now(),m=getCurrentMonth();
if(u.limits?.expiresAt&&new Date(u.limits.expiresAt).getTime()<now)return{ok:false,reason:"expired"};
if(u.limits?.trafficLimit&&(u.usage?.total||0)>=u.limits.trafficLimit)return{ok:false,reason:"traffic_exceeded"};
if(u.limits?.monthlyLimit&&(u.usage?.monthly?.[m]||0)>=u.limits.monthlyLimit)return{ok:false,reason:"monthly_exceeded"};
return{ok:true}}
function handleManage(params,res){
const key=params.get("key"),action=params.get("action"),user=params.get("user");
if(key!==CONFIG.adminPassword)return sendJSON(res,{error:"Invalid key"},403);
if(!action)return sendJSON(res,{error:"Missing action"},400);
const users=loadUsers();
if(action==="create"){
if(!user)return sendJSON(res,{error:"Missing user"},400);
if(users.find(u=>u.username===user))return sendJSON(res,{error:"User exists"},400);
const pass=params.get("pass")||crypto.randomBytes(8).toString("hex");
const days=parseInt(params.get("days"))||0;const traffic=parseFloat(params.get("traffic"))||0;const monthly=parseFloat(params.get("monthly"))||0;
const newUser={username:user,password:pass,createdAt:new Date().toISOString(),limits:{},usage:{total:0,monthly:{}}};
if(days>0)newUser.limits.expiresAt=new Date(Date.now()+days*864e5).toISOString();
if(traffic>0)newUser.limits.trafficLimit=traffic*1073741824;
if(monthly>0)newUser.limits.monthlyLimit=monthly*1073741824;
users.push(newUser);
if(saveUsers(users))return sendJSON(res,{success:true,user:user,password:pass});
return sendJSON(res,{error:"Save failed"},500)}
if(action==="delete"){
if(!user)return sendJSON(res,{error:"Missing user"},400);
const idx=users.findIndex(u=>u.username===user);if(idx<0)return sendJSON(res,{error:"User not found"},404);
users.splice(idx,1);
if(saveUsers(users))return sendJSON(res,{success:true});
return sendJSON(res,{error:"Save failed"},500)}
if(action==="update"){
if(!user)return sendJSON(res,{error:"Missing user"},400);
const u=users.find(x=>x.username===user);if(!u)return sendJSON(res,{error:"User not found"},404);
const days=params.get("days"),traffic=params.get("traffic"),monthly=params.get("monthly"),pass=params.get("pass");
if(!u.limits)u.limits={};
if(days!==null)u.limits.expiresAt=parseInt(days)>0?new Date(Date.now()+parseInt(days)*864e5).toISOString():null;
if(traffic!==null)u.limits.trafficLimit=parseFloat(traffic)>0?parseFloat(traffic)*1073741824:null;
if(monthly!==null)u.limits.monthlyLimit=parseFloat(monthly)>0?parseFloat(monthly)*1073741824:null;
if(pass)u.password=pass;
if(saveUsers(users))return sendJSON(res,{success:true});
return sendJSON(res,{error:"Save failed"},500)}
if(action==="list")return sendJSON(res,users.map(u=>({username:u.username,limits:u.limits,usage:u.usage})));
return sendJSON(res,{error:"Unknown action"},400)}

// --- Enhanced UI ---
const HTML=`<!DOCTYPE html><html lang="zh-CN"><head><meta charset="UTF-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Hysteria2 管理面板</title><style>
:root {--primary:#fb923c;--glow:rgba(251,146,60,0.4);--bg:#fff7ed;--card:rgba(255,255,255,0.7);--text:#431407;--text-dim:#9a3412;--success:#22c55e;--danger:#ef4444}
*{margin:0;padding:0;box-sizing:border-box;outline:none;-webkit-tap-highlight-color:transparent}
body{font-family:'Noto Sans SC','PingFang SC',system-ui,sans-serif;background:var(--bg);color:var(--text);min-height:100vh;overflow-x:hidden}
body::before{content:'';position:fixed;top:-50%;left:-50%;width:200%;height:200%;background:radial-gradient(circle at 50% 50%,rgba(251,146,60,0.15),transparent 60%);z-index:-1;animation:P 15s ease-in-out infinite alternate}
@keyframes P{0%{transform:scale(1)}100%{transform:scale(1.1)}}
.view{display:none}.view.active{display:block;animation:F 0.5s ease}@keyframes F{from{opacity:0;transform:translateY(10px)}to{opacity:1;transform:translateY(0)}}
.card{background:var(--card);backdrop-filter:blur(12px);border:1px solid rgba(251,146,60,0.1);border-radius:24px;padding:32px;box-shadow:0 20px 40px rgba(67,20,7,0.05)}
.btn{width:100%;padding:14px;border:none;border-radius:12px;background:linear-gradient(135deg,var(--primary),#ea580c);color:#fff;font-weight:600;cursor:pointer;transition:.3s}
.btn:hover{transform:translateY(-2px);box-shadow:0 10px 20px rgba(251,146,60,0.3)}
input{width:100%;background:rgba(255,255,255,0.5);border:1px solid rgba(67,20,7,0.05);padding:14px;border-radius:12px;color:var(--text);margin-bottom:16px;transition:.3s}
input:focus{border-color:var(--primary);box-shadow:0 0 0 2px var(--glow);background:#fff}
.login-wrap{display:flex;justify-content:center;align-items:center;min-height:100vh;padding:20px}
.nav{display:flex;justify-content:space-between;align-items:center;padding:20px 32px;background:rgba(255,247,237,0.8);backdrop-filter:blur(10px);position:sticky;top:0;z-index:10;border-bottom:1px solid rgba(67,20,7,0.05)}
.brand{font-size:20px;font-weight:700;display:flex;align-items:center;gap:12px}
.brand i{width:32px;height:32px;background:var(--primary);color:#fff;border-radius:8px;display:grid;place-items:center;font-style:normal}
.stats{display:grid;grid-template-columns:repeat(auto-fit,minmax(240px,1fr));gap:24px;padding:32px;max-width:1400px;margin:0 auto}
.stat{background:var(--card);padding:24px;border-radius:20px;border:1px solid rgba(67,20,7,0.05);transition:.3s}
.stat:hover{transform:translateY(-5px);background:#fff}
.val{font-size:32px;font-weight:700;margin:8px 0}.lbl{color:var(--text-dim);font-size:14px}
.main-area{max-width:1400px;margin:0 auto;padding:0 32px 32px}
.hdr{display:flex;justify-content:space-between;align-items:center;margin-bottom:24px}
table{width:100%;border-collapse:collapse;background:var(--card);border-radius:20px;overflow:hidden}
th,td{padding:20px;text-align:left;border-bottom:1px solid rgba(67,20,7,0.05)}
th{color:var(--text-dim);text-transform:uppercase;font-size:12px;letter-spacing:1px}
.tag{padding:4px 12px;border-radius:20px;font-size:12px;font-weight:600;background:rgba(67,20,7,0.05)}
.tag.on{background:rgba(34,197,94,0.15);color:var(--success);border:1px solid rgba(34,197,94,0.2)}
.act{display:flex;gap:8px}.ibtn{width:32px;height:32px;border-radius:8px;border:none;background:rgba(67,20,7,0.05);color:var(--text-dim);cursor:pointer;display:grid;place-items:center;transition:.2s}
.ibtn:hover{background:var(--primary);color:#fff}.ibtn.danger:hover{background:var(--danger)}
.modal{position:fixed;inset:0;background:rgba(67,20,7,0.2);backdrop-filter:blur(8px);z-index:100;display:none;align-items:center;justify-content:center;opacity:0;transition:.3s}
.modal.on{display:flex;opacity:1}.modal .card{width:90%;max-width:400px;animation:U .3s ease}@keyframes U{from{transform:translateY(20px);opacity:0}to{transform:translateY(0);opacity:1}}
.toast-box{position:fixed;bottom:30px;right:30px;display:flex;flex-direction:column;gap:10px;z-index:200}
.toast{background:#fff;color:var(--text);box-shadow:0 10px 20px rgba(0,0,0,0.1);padding:12px 20px;border-radius:12px;border:1px solid rgba(67,20,7,0.05);display:flex;align-items:center;gap:10px;animation:SI .3s ease}
.toast span{font-size:18px}@keyframes SI{from{transform:translateX(100%)}to{transform:translateX(0)}}
.code-box{background:rgba(67,20,7,0.05);padding:12px;border-radius:8px;word-break:break-all;font-family:monospace;color:var(--text-dim);margin:16px 0;font-size:12px;border:1px solid rgba(67,20,7,0.1)}
@media(max-width:768px){.stats{grid-template-columns:1fr}.main-area{padding:16px}.nav{padding:16px 20px}th,td{padding:16px}.hide-m{display:none}}
</style></head><body>
<div id="v-login" class="view active"><div class="login-wrap"><div class="card" style="max-width:360px">
<h1 style="text-align:center;margin-bottom:8px">Hysteria2</h1><p style="text-align:center;color:var(--text-dim);margin-bottom:32px">管理系统登录</p>
<input type="password" id="lp" placeholder="请输入管理密码"><button class="btn" onclick="login()">登录</button></div></div></div>
<div id="v-dash" class="view">
<nav class="nav"><div class="brand"><i>⚡</i><span>B-UI</span></div><div style="display:flex;gap:8px"><button class="ibtn" onclick="openM('m-pwd')" title="修改密码">🔑</button><button class="ibtn danger" onclick="logout()" title="退出">✕</button></div></nav>
<div class="stats">
<div class="stat"><div class="lbl">用户总数</div><div class="val" id="st-u">0</div></div>
<div class="stat"><div class="lbl">在线设备</div><div class="val" id="st-o" style="color:var(--success)">0</div></div>
<div class="stat"><div class="lbl">上传流量</div><div class="val" id="st-up">0</div></div>
<div class="stat"><div class="lbl">下载流量</div><div class="val" id="st-dl">0</div></div>
</div>
<div class="main-area"><div class="hdr"><h2 style="font-size:20px">用户列表</h2><button class="btn" style="width:auto;padding:10px 24px" onclick="openM('m-add')">+ 新建用户</button></div>
<table><thead><tr><th>用户名</th><th>状态</th><th class="hide-m">本月流量</th><th class="hide-m">累计流量</th><th>操作</th></tr></thead><tbody id="tb"></tbody></table></div>
</div>
<div id="m-add" class="modal"><div class="card"><h3>新建用户</h3><br>
<input id="nu" placeholder="用户名"><input id="np" placeholder="密码 (留空自动生成)">
<input id="nd" type="number" placeholder="有效天数 (0=不限)" min="0"><input id="nt" type="number" placeholder="总流量限制 GB (0=不限)" min="0" step="0.1">
<div style="display:flex;gap:10px"><button class="btn" style="background:rgba(67,20,7,0.1)" onclick="closeM()">取消</button><button class="btn" onclick="addUser()">创建</button></div></div></div>
<div id="m-cfg" class="modal"><div class="card" style="text-align:center"><h3>连接配置</h3><p style="font-size:12px;color:var(--text-dim);margin:0 0 8px">兼容 v2rayN / Shadowrocket / Clash Meta</p><div id="qrcode" style="margin:16px auto;background:#fff;padding:16px;border-radius:12px;width:fit-content"></div><div class="code-box" id="uri" style="margin-bottom:16px"></div>
<div style="display:flex;gap:10px"><button class="btn" onclick="copy()">复制链接</button><button class="btn" style="background:rgba(255,255,255,0.1)" onclick="closeM()">关闭</button></div></div></div>
<div id="m-pwd" class="modal"><div class="card"><h3>修改管理密码</h3><br>
<input type="password" id="newpwd" placeholder="新密码 (至少6位)">
<div style="display:flex;gap:10px"><button class="btn" style="background:rgba(67,20,7,0.1)" onclick="closeM()">取消</button><button class="btn" onclick="changePwd()">保存</button></div></div></div>
<div class="toast-box" id="t-box"></div>
<script>
const $=s=>document.querySelector(s);let tok=localStorage.getItem("t"),cfg={};
const sz=b=>{if(!b)return"0 B";const i=Math.floor(Math.log(b)/Math.log(1024));return(b/Math.pow(1024,i)).toFixed(2)+" "+["B","KB","MB","GB"][i]};
function toast(m,e){const d=document.createElement("div");d.className="toast";d.innerHTML="<span>"+(e?"⚠️":"✅")+"</span>"+m;$("#t-box").appendChild(d);setTimeout(()=>d.remove(),3000)}
function openM(id){$("#"+id).classList.add("on")} function closeM(){document.querySelectorAll(".modal").forEach(e=>e.classList.remove("on"))}
function api(ep,opt={}){return fetch("/api"+ep,{...opt,headers:{...opt.headers,Authorization:"Bearer "+tok}}).then(r=>{if(r.status==401)logout();return r.json()})}
function login(){const pw=$("#lp").value;fetch("/api/login",{method:"POST",body:JSON.stringify({password:pw})}).then(r=>r.json()).then(d=>{if(d.token){tok=d.token;localStorage.setItem("t",tok);localStorage.setItem("ap",pw);init()}else toast("密码错误",1)})}
function logout(){localStorage.removeItem("t");location.reload()}
function init(){$("#v-login").classList.remove("active");setTimeout(()=>$("#v-login").style.display="none",300);$("#v-dash").classList.add("active");
api("/config").then(d=>cfg=d);load();setInterval(load,5000)}
function load(){Promise.all([api("/users"),api("/online"),api("/stats")]).then(([u,o,s])=>{
$("#st-u").innerText=u.length;$("#st-o").innerText=Object.keys(o).length;
let tu=0,td=0;Object.values(s).forEach(v=>{tu+=v.tx||0;td+=v.rx||0});$("#st-up").innerText=sz(tu);$("#st-dl").innerText=sz(td);
const m=new Date().toISOString().slice(0,7);
u.forEach(x=>{const uri="hysteria2://"+encodeURIComponent(x.password)+"@"+cfg.domain+":"+cfg.port+"/?sni="+cfg.domain+"&insecure=0#"+encodeURIComponent(x.username);new Image().src="https://api.qrserver.com/v1/create-qr-code/?size=200x200&data="+encodeURIComponent(uri)});
$("#tb").innerHTML=u.map(x=>{
const on=o[x.username],monthly=x.usage?.monthly?.[m]||0,total=x.usage?.total||0;
const exp=x.limits?.expiresAt?new Date(x.limits.expiresAt)<new Date():"",tlim=x.limits?.trafficLimit,over=tlim&&total>=tlim;
const badge=exp?' <span style="color:var(--danger);font-size:10px">[已过期]</span>':(over?' <span style="color:var(--danger);font-size:10px">[超限]</span>':"");
return '<tr><td><b>'+x.username+'</b>'+badge+'</td><td><span class="tag '+(on?"on":"")+'">'+( on?on+" 个设备在线":"离线")+'</span></td><td class="hide-m" style="font-family:monospace;font-size:12px;color:var(--text-dim)">'+sz(monthly)+'</td><td class="hide-m" style="font-family:monospace;font-size:12px;color:var(--text-dim)">'+sz(total)+(tlim?" / "+sz(tlim):"")+'</td><td><div class="act"><button class="ibtn" onclick="show(&apos;"'+x.username+'"&apos;,&apos;"'+x.password+'"&apos;)">🗑</button></div></td></tr>'
}).join("")})}
function addUser(){const u=$("#nu").value,p=$("#np").value,d=$("#nd").value||0,t=$("#nt").value||0;
fetch("/api/manage?key="+encodeURIComponent(cfg.adminPass||localStorage.getItem("ap")||"")+"&action=create&user="+encodeURIComponent(u)+(p?"&pass="+encodeURIComponent(p):"")+"&days="+d+"&traffic="+t).then(r=>r.json()).then(r=>{if(r.success){closeM();toast("用户 "+u+" 已创建，密码: "+r.password);load()}else toast(r.error||"创建失败",1)})}
function del(u){if(confirm("确定要删除用户 "+u+" 吗?"))api("/users/"+u,{method:"DELETE"}).then(()=>load())}
function kick(u){api("/kick",{method:"POST",body:JSON.stringify([u])}).then(()=>toast("已将用户 "+u+" 强制下线"))}
function show(u,p){const uri="hysteria2://"+encodeURIComponent(p)+"@"+cfg.domain+":"+cfg.port+"/?sni="+cfg.domain+"&insecure=0#"+encodeURIComponent(u);$("#uri").innerText=uri;$("#qrcode").innerHTML='<img src="https://api.qrserver.com/v1/create-qr-code/?size=200x200&data='+encodeURIComponent(uri)+'" alt="QR Code" style="display:block">';openM("m-cfg")}
function copy(){navigator.clipboard.writeText($("#uri").innerText);toast("已复制到剪贴板")}
function changePwd(){const np=$("#newpwd").value;if(np.length<6)return toast("密码至少6位",1);
api("/password",{method:"POST",body:JSON.stringify({newPassword:np})}).then(r=>{if(r.success){closeM();toast("密码已更新，请重新登录");setTimeout(()=>logout(),2000)}else toast(r.error||"操作失败",1)})}
if(tok)init();
</script></body></html>`;

http.createServer(async(req,res)=>{
const u=new URL(req.url,`http://${req.headers.host}`),p=u.pathname;
if(req.method==="OPTIONS"){res.writeHead(200,{"Access-Control-Allow-Origin":"*","Access-Control-Allow-Methods":"*","Access-Control-Allow-Headers":"*"});return res.end()}
if(p==="/"||p==="/index.html"){res.writeHead(200,{"Content-Type":"text/html; charset=utf-8"});return res.end(HTML)}
if(p.startsWith("/api/")){const r=p.slice(5);const clientIP=getClientIP(req);
try{
if(r==="login"&&req.method==="POST"){
const b=await parseBody(req);
if(!checkRateLimit(clientIP)){recordAttempt(clientIP,false);return sendJSON(res,{error:"Too many attempts. Try again later."},429)}
const ok=b.password===CONFIG.adminPassword;recordAttempt(clientIP,ok);
if(ok)return sendJSON(res,{token:genToken({admin:true})});else return sendJSON(res,{error:"Auth failed"},401)}
if(r==="manage")return handleManage(u.searchParams,res);
const auth=verifyToken((req.headers.authorization||"").replace("Bearer ",""));if(!auth)return sendJSON(res,{error:"Unauthorized"},401);
if(r==="users"){if(req.method==="GET")return sendJSON(res,loadUsers());
if(req.method==="POST"){const b=await parseBody(req),users=loadUsers();if(users.find(u=>u.username===b.username))return sendJSON(res,{error:"Exists"},400);users.push({username:b.username,password:b.password||crypto.randomBytes(8).toString("hex"),createdAt:new Date()});return saveUsers(users)?sendJSON(res,{success:true}):sendJSON(res,{error:"Save failed"},500)}}
if(r.startsWith("users/")&&req.method==="DELETE"){let users=loadUsers();users=users.filter(u=>u.username!==r.slice(6));return saveUsers(users)?sendJSON(res,{success:true}):sendJSON(res,{error:"Fail"},500)}
if(r==="stats")return sendJSON(res,await fetchStats("/traffic"));
if(r==="online")return sendJSON(res,await fetchStats("/online"));
if(r==="kick"&&req.method==="POST")return sendJSON(res,await postStats("/kick",await parseBody(req)));
if(r==="config")return sendJSON(res,getConfig());
if(r==="password"&&req.method==="POST"){const b=await parseBody(req);
if(!b.newPassword||b.newPassword.length<6)return sendJSON(res,{error:"密码至少6位"},400);
try{const svc="/etc/systemd/system/b-ui-admin.service";let c=require("fs").readFileSync(svc,"utf8");
c=c.replace(/ADMIN_PASSWORD=[^\n]*/,"ADMIN_PASSWORD="+b.newPassword);
require("fs").writeFileSync(svc,c);require("child_process").execSync("systemctl daemon-reload");
return sendJSON(res,{success:true,message:"密码已更新，请重新登录"})}
catch(e){return sendJSON(res,{error:e.message},500)}}
}catch(e){return sendJSON(res,{error:e.message},500)}}
sendJSON(res,{error:"Not found"},404)}).listen(CONFIG.port,()=>console.log("Admin Panel Running"));
