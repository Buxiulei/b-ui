#!/usr/bin/env node
'use strict';
// Actual whole app.js and actual index.html controls; synthetic accounts only.
const fs=require('node:fs'), path=require('node:path'), vm=require('node:vm'), assert=require('node:assert/strict');
const repo=path.resolve(__dirname,'../..'), sourcePath=path.join(repo,'web/app.js');
const source=fs.readFileSync(sourcePath,'utf8'), html=fs.readFileSync(path.join(repo,'web/index.html'),'utf8');
const TOKEN='0123456789abcdef0123456789abcdef', NEXT='aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa';
const URL='https://panel.example:8443/api/profile/'+TOKEN+'/v2rayn-sb1142-macos?remarks=BUI%20Managed%20macOS';
const caps={v4Tcp:'unknown',v4Udp:'unknown',v6Tcp:'unknown',v6Udp:'unknown'};
const base={username:'alice',protocol:'hysteria2',limits:{},usage:{total:0,monthly:{}},subToken:TOKEN,nodeUri:'hysteria2://synthetic@edge.example:10443#alice',managedProfile:{selectedEgress:'vps',allowedChoices:['vps','residential'],revision:1,delivery:'available',capabilities:caps}};
const user=(changes={}, managed={})=>({...base,...changes,managedProfile:{...base.managedProfile,...managed}});
const settle=async()=>{for(let i=0;i<20;i++)await Promise.resolve();};
class Element {
    constructor(name) {
        this.value = ""; this.checked = false; this.disabled = false; this.dataset = {}; this.name = name; this.style = {}; this.children = []; this._text = ''; this._html = '';
        const classes = new Set();
        this.classList = {
            add: name => classes.add(name), remove: name => classes.delete(name),
            contains: name => classes.has(name),
        };
    }
    get innerText() { return this._text; }
    set innerText(value) { this._text = String(value); this._html = ''; this.children = []; }
    get textContent() { return this._text; }
    set textContent(value) { this.innerText = value; }
    get innerHTML() { return this._html; }
    set innerHTML(value) { this._html = String(value); this._text = ''; this.children = []; }
    appendChild(element) { this.children.push(element); return element; }
    append(...elements) { this.children.push(...elements); }
    querySelector() { return this.children[0] || null; }
    remove() {}
}

function runtime() {
    const elements = new Map();
    const ids = new Set([...html.matchAll(/id="([^"]+)"/g)].map(m => "#" + m[1]));
    const el = name => {
        if (name.startsWith("#") && !ids.has(name)) throw Error("Missing actual index.html ID: " + name);
        if (!elements.has(name)) elements.set(name, new Element(name));
        return elements.get(name);
    };
    const qr = [], clipboard = [], requests = [], opened = [], confirmations = [];
    let apiUsers = [], failure = null, mutation = {success:true}, mutationStatus=200;
    const userDeferrals = [];
    class QR {
        constructor(element, options) { qr.push(options.text); element.appendChild(new Element('canvas')); }
    }
    QR.CorrectLevel = { M: 'M' };
    const context = vm.createContext({
        document: {
            querySelector: el, getElementById: id => el('#' + id),
            querySelectorAll: () => [el('#m-cfg'), el('#m-edit'), el('#m-add')],
            createElement: name => new Element(name), createTextNode: text => ({ textContent: text }),
        },
        localStorage: { getItem: () => null, setItem() {}, removeItem() {} },
        location: { host: 'panel.example:8443', reload() {} },
        navigator: { clipboard: { writeText: value => { clipboard.push(value); return Promise.resolve(); } } },
        window: { open: (...args) => opened.push(args) },
        QRCode: QR, setTimeout: () => 0, clearTimeout() {}, setInterval: () => 0, clearInterval() {},
        confirm: message => { confirmations.push(message); return true; }, alert() {},
        fetch: async (url, options = {}) => {
            requests.push({url, ...options, body: options.body ? JSON.parse(options.body) : undefined});
            if (url === '/api/users' && !options.method && userDeferrals.length) return userDeferrals.shift().promise;
            if (url === '/api/users' && !options.method && failure) throw Error(failure);
            const isMutation = options.method && options.method !== 'GET';
            const data = isMutation ? mutation : url === '/api/users' ? apiUsers : {};
            return {status: isMutation ? mutationStatus : 200, json: async () => JSON.parse(JSON.stringify(data))};
        },
    });
    new vm.Script(source, { filename: sourcePath }).runInContext(context);
    // This appended bridge accesses lexical bindings without modifying any product function.
    vm.runInContext(`globalThis.__review = {
        setUsers(json) { allUsers = JSON.parse(json); },
        show: showU, copyManaged() { return copyManagedProfile(); }, copy, copyClash, downloadSubscription,
        load, addUser, editUser, saveUser, rotateSub,
        current() { return JSON.stringify(currentShowUser); }
    };`, context);
    return {
        bridge: context.__review, el, qr, clipboard, requests, opened, confirmations,
        deferUsers() {
            let resolve, reject;
            const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
            userDeferrals.push({promise});
            return {answer(users) {resolve({status:200,json:async()=>JSON.parse(JSON.stringify(users))});},fail(message) {reject(Error(message));}};
        },
        failUsers(message) {failure=message;},
        mutation(status, data) {mutationStatus=status;mutation=data;},
        apiUsers(users) {apiUsers=JSON.parse(JSON.stringify(users));},
        setUsers(users) { context.__review.setUsers(JSON.stringify(users)); },
        async update(users) { apiUsers = JSON.parse(JSON.stringify(users)); await context.__review.load(); },
        toasts() { return el('#t-box').children.map(child => child.innerHTML); },
    };
}


function shown(x=user()){const r=runtime();r.setUsers([x]);r.apiUsers([x]);r.bridge.show(x.username);return r;}
const tests=[
['Caddy actual emitted-regex source masks new path and redirect without losing target',()=>{
    const rust=fs.readFileSync(path.join(repo,'crates/bui/src/modules/core_files.rs'),'utf8');
    const encoded=rust.match(/const SUB_SEG_REGEXP: &str =\s*(.+);/);
    assert.ok(encoded,'actual Caddy expression constant');
    const args=JSON.parse(encoded[1]).match(/^"(.*)" "(.*)"$/);
    const expression=new RegExp(args[1].replace('(?i)',''),'gi');
    const replacement=args[2].replace('${1}','$1');
    for(const prefix of ['/api/profile/','https://panel.example/API/PROFILE/']) {
        const input=prefix+TOKEN+'/v2rayn-sb1142-macos?remarks=BUI%20Managed%20macOS';
        assert.equal(input.replace(expression,replacement),prefix+'***/v2rayn-sb1142-macos?remarks=BUI%20Managed%20macOS');
    }
}],
['primary action copies fixed native URL and display query',()=>{const r=shown(); assert.equal(r.el('#managed-copy').disabled,false);r.bridge.copyManaged();assert.deepEqual(r.clipboard,[URL]);assert.match(r.el('#cfg-title').innerText,/完整配置/);assert.match(r.el('#managed-status').innerText,/可获取配置/);} ],
['available unknown capabilities remain unaccepted',()=>{const r=shown();assert.match(r.el('#managed-capabilities').innerText,/IPv4 TCP.*未验收/);assert.match(r.el('#managed-capabilities').innerText,/IPv6 UDP.*未验收/);assert.doesNotMatch(r.el('#managed-status').innerText,/已保护|双栈可用/);} ],
['four capability cells show verified and unsupported distinctly',()=>{const r=shown(user({}, {capabilities:{v4Tcp:'verified',v4Udp:'unknown',v6Tcp:'unsupported',v6Udp:'verified'}}));const t=r.el('#managed-capabilities').innerText;assert.match(t,/IPv4 TCP.*已验收/);assert.match(t,/IPv6 TCP.*不支持/);assert.match(t,/IPv6 UDP.*已验收/);} ],
...['missing_selection','account_unavailable','route_unavailable'].map((delivery,i)=>['delivery '+delivery+' disables primary with status',()=>{const r=shown(user({}, {delivery}));assert.equal(r.el('#managed-copy').disabled,true);r.bridge.copyManaged();assert.deepEqual(r.clipboard,[]);assert.match(r.el('#managed-status').innerText,new RegExp(['409','403','503'][i]));}]),
['missing malformed token never falls back to username',()=>{for(const token of [undefined,'','alice','ABCDEF0123456789abcdef0123456789ab']){const r=shown(user({subToken:token,username:'name / secret'}));r.bridge.copyManaged();assert.equal(r.el('#managed-copy').disabled,true);assert.deepEqual(r.clipboard,[]);assert.doesNotMatch(r.el('#managed-url').innerText,/https:/);}}],
['legacy copy download and Clash remain secondary canonical actions',()=>{const r=shown(user({protocol:'fusion'}));r.bridge.copy();r.bridge.copyClash();r.bridge.downloadSubscription();assert.deepEqual(r.clipboard,['https://panel.example:8443/api/sub/'+TOKEN+'#alice','https://panel.example:8443/api/clash/'+TOKEN]);assert.deepEqual(r.opened,[['/api/subscription/'+TOKEN,'_blank']]);assert.match(html,/<details[^>]*>\s*<summary>仅节点/);assert.doesNotMatch(html,/<details[^>]*open/);} ],
['managed state refresh does not claim credential rotation',async()=>{const r=shown();await r.update([user({}, {revision:2,delivery:'route_unavailable'})]);assert.equal(r.el('#managed-copy').disabled,true);assert.match(r.el('#managed-status').innerText,/503/);assert.equal(r.toasts().filter(t=>t.includes('已被重置')).length,0);} ],
['unchanged refresh neither redraws QR nor toasts',async()=>{const r=shown();const qr=r.qr.length;await r.update([user()]);assert.equal(r.qr.length,qr);assert.deepEqual(r.toasts(),[]);} ],
['deleted record immediately retracts primary',async()=>{const r=shown();await r.update([]);r.bridge.copyManaged();assert.deepEqual(r.clipboard,[]);assert.equal(r.el('#managed-copy').disabled,true);} ],
['failed list refresh retracts old action until successful poll',async()=>{const r=shown();r.failUsers('offline');await assert.rejects(r.bridge.load());r.bridge.copyManaged();assert.deepEqual(r.clipboard,[]);r.failUsers(null);await r.update([user()]);r.bridge.copyManaged();assert.deepEqual(r.clipboard,[URL]);} ],
['rotate success then reload failure never copies revoked URL and later poll recovers',async()=>{const r=shown();r.failUsers('offline');r.bridge.rotateSub();await settle();r.bridge.copyManaged();assert.deepEqual(r.clipboard,[]);assert.equal(r.el('#managed-copy').disabled,true);r.failUsers(null);await r.update([user({subToken:NEXT})]);r.bridge.copyManaged();assert.deepEqual(r.clipboard,[URL.replace(TOKEN,NEXT)]);} ],
['pre-rotation poll cannot reenable revoked token while reload is pending or failed',async()=>{
    const r=shown(), old=r.deferUsers(), oldLoad=r.bridge.load();
    const fresh=r.deferUsers(), rotation=r.bridge.rotateSub();await settle();
    assert.equal(r.el('#managed-copy').disabled,true);
    old.answer([user()]);await oldLoad;r.bridge.copyManaged();assert.deepEqual(r.clipboard,[],'pre-rotation poll must not restore revoked token');
    fresh.fail('post-rotation offline');await rotation;await settle();r.bridge.copyManaged();assert.deepEqual(r.clipboard,[]);
    await r.update([user({subToken:NEXT})]);r.bridge.copyManaged();assert.deepEqual(r.clipboard,[URL.replace(TOKEN,NEXT)]);
}],
['older overlapping poll cannot overwrite a newer authoritative record',async()=>{
    const r=shown(), old=r.deferUsers(), oldLoad=r.bridge.load(), fresh=r.deferUsers(), freshLoad=r.bridge.load();
    fresh.answer([user({subToken:NEXT})]);await freshLoad;old.answer([user()]);await oldLoad;
    r.bridge.copyManaged();assert.deepEqual(r.clipboard,[URL.replace(TOKEN,NEXT)]);
}],
['older failed poll cannot disable newer successful record',async()=>{
    const r=shown(), old=r.deferUsers(), oldLoad=r.bridge.load(), fresh=r.deferUsers(), freshLoad=r.bridge.load();
    fresh.answer([user({subToken:NEXT})]);await freshLoad;old.fail('obsolete error');await oldLoad.catch(()=>{});
    assert.equal(r.el('#managed-copy').disabled,false);r.bridge.copyManaged();assert.deepEqual(r.clipboard,[URL.replace(TOKEN,NEXT)]);
}],
['rotation confirmation and success describe refresh and lifecycle without instant-disconnect promise',async()=>{
    const r=shown();r.apiUsers([user({subToken:NEXT})]);await r.bridge.rotateSub();
    const text=r.confirmations.join(' ');assert.match(text,/原订阅组.*替换新链接.*更新/);
    assert.match(text,/已有连接.*服务端.*核心.*生命周期/);
    assert.doesNotMatch(text,/立刻失效|客户端会断连|立即断开/);
    assert.match(text,/仅节点.*Linux.*切换到新的住宅 HY2 节点/);
    assert.match(r.toasts().join(' '),/已重置.*原订阅组.*更新/);
}],
['create requires explicit choice independent of residential checkbox',async()=>{const r=runtime();r.el('#nu').value='bob';r.el('#nproto').value='fusion';r.el('#nresi').checked=true;r.bridge.addUser();await settle();assert.equal(r.requests.length,0);assert.match(r.toasts().join(' '),/选择.*出口/);} ],
...['vps','residential'].map(choice=>['create sends explicit '+choice+' intent and retains 403 form',async()=>{const r=runtime();r.el('#nu').value='bob';r.el('#nproto').value='fusion';r.el('#nmanaged-egress').value=choice;r.el('#m-add').classList.add('on');r.mutation(403,{success:false,error:'not granted'});r.bridge.addUser();await settle();assert.equal(r.requests[0].method,'POST');assert.equal(r.requests[0].body.managed_egress,choice);assert.equal(r.el('#nmanaged-egress').value,choice);assert.equal(r.el('#m-add').classList.contains('on'),true);assert.doesNotMatch(r.toasts().join(' '),/已创建/);}]),
['edit legacy none remains empty and unchanged request omits selection',async()=>{const x=user({}, {selectedEgress:null,revision:null,delivery:'missing_selection'}),r=shown(x);r.bridge.editUser('alice');assert.equal(r.el('#edit-managed-egress').value,'');assert.match(r.el('#edit-managed-hint').innerText,/409/);r.mutation(403,{success:false});r.bridge.saveUser();await settle();assert.ok(!Object.hasOwn(r.requests[0].body,'managed_egress'));} ],
['edit preloads actual disallowed selection rather than first allowed',()=>{const r=shown(user({}, {selectedEgress:'residential',allowedChoices:['vps'],delivery:'account_unavailable'}));r.bridge.editUser('alice');assert.equal(r.el('#edit-managed-egress').value,'residential');assert.match(r.el('#edit-managed-hint').innerText,/不可用|无权/);} ],
['edit changed choice uses server grant and failure preserves intent',async()=>{const r=shown();r.bridge.editUser('alice');r.el('#edit-managed-egress').value='residential';r.mutation(503,{success:false,error:'unavailable'});r.bridge.saveUser();await settle();assert.equal(r.requests[0].body.managed_egress,'residential');assert.equal(r.el('#m-edit').classList.contains('on'),true);assert.equal(r.el('#edit-managed-egress').value,'residential');assert.equal(JSON.parse(r.bridge.current()).managedProfile.selectedEgress,'vps');} ],
['edit unchanged current selection is omitted',async()=>{const r=shown();r.bridge.editUser('alice');r.mutation(403,{success:false});r.bridge.saveUser();await settle();assert.ok(!Object.hasOwn(r.requests[0].body,'managed_egress'));} ],
['successful save rereads authority and displays outage without fallback',async()=>{const r=shown();r.bridge.editUser('alice');r.el('#edit-managed-egress').value='residential';r.apiUsers([user({}, {selectedEgress:'residential',delivery:'route_unavailable',revision:2})]);r.bridge.saveUser();await settle();r.bridge.show('alice');assert.match(r.el('#managed-status').innerText,/503/);assert.equal(JSON.parse(r.bridge.current()).managedProfile.selectedEgress,'residential');assert.ok(r.requests.some(x=>x.url==='/api/users'&&!x.method));} ],
['client boundaries are visible with fixed consumer version',()=>{const r=shown();const t=r.el('#managed-help').innerText;for(const pattern of [/7\.25\.4/,/1\.14\.2/,/10808/,/60.*分钟/,/reload/i,/Android.*iOS.*未验收/,/重启.*TUN/,/kill-switch/,/旧.*配置|旧.*profile/,/订阅组/])assert.match(t,pattern);} ],
];
(async()=>{let passed=0;for(const [name,run]of tests){try{await run();passed++;console.log('PASS '+name);}catch(e){console.log('FAIL '+name+'\n'+e.message);}}console.log(JSON.stringify({label:'panel-managed-profile',cases:tests.length,passed,failed:tests.length-passed}));process.exitCode=passed===tests.length?0:1;})().catch(e=>{console.error(e);process.exitCode=2;});
