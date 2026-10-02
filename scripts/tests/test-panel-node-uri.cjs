#!/usr/bin/env node
'use strict';

// Regression driver. Every credential below is synthetic; product source is read-only.
// Execute the WHOLE current app.js; do not copy or replace genUri/showU/syncOpenConfig.
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const vm = require('node:vm');
const assert = require('node:assert/strict');

const repo = path.resolve(__dirname, '../..');
const sourcePath = path.join(repo, 'web/app.js');
const source = fs.readFileSync(sourcePath, 'utf8');
const sourceSha256 = crypto.createHash('sha256').update(source).digest('hex');
const label = 'panel-node-uri';

// Serialized PanelUser shape, with distinct reserved credentials in the canonical URI.
const CANONICAL = 'hysteria2://r077:reserved@edge.example:40000?sni=edge.example&insecure=0&mport=41000-50000&obfs=salamander&obfs-password=obfs%3A%40%2F%E4%B8%AD#alice-HY2%E4%BD%8F%E5%AE%85';
const CHANGED = 'hysteria2://r078:reserved-next@next.example:40000?sni=next.example&insecure=0&mport=41000-50000#alice-HY2%E4%BD%8F%E5%AE%85';
const DIRECT = 'hysteria2://alice:canonical-direct@edge.example:10443?sni=edge.example&insecure=0#alice-HY2%E7%9B%B4%E8%BF%9E';
const REALITY = 'vless://11111111-1111-4111-8111-111111111111@edge.example:10002?security=reality&encryption=none&pbk=canonical-key&headerType=&fp=chrome&spx=%2F&type=tcp&flow=xtls-rprx-vision&sni=canonical.example&sid=abcd#alice-Reality%E4%BD%8F%E5%AE%85';
const base = {
    username: 'alice', protocol: 'hysteria2', createdAt: '2026-09-29T12:00:00Z',
    limits: {}, usage: { total: 0, monthly: {} }, password: 'legacy-direct-different',
    uuid: '22222222-2222-4222-8222-222222222222', subToken: 'synthetic-token/%',
    sni: 'legacy-user-sni.invalid', residential: true, slot: 7, slotIp: '192.0.2.77',
    slotPort: 40000, slotHop: [41000, 50000], hy2ResiGate: 'slot-7-out',
    disabled: false, blocked: false, nodeUri: CANONICAL,
};
function serialized(changes = {}, omitted = []) {
    const value = { ...base, ...changes };
    omitted.forEach(key => delete value[key]);
    return JSON.parse(JSON.stringify(value));
}
const legacyCfg = {
    domain: 'legacy-cfg.invalid', port: 19999, sni: 'legacy-cfg-sni.invalid',
    pubKey: 'legacy-cfg-key', shortId: 'dead', xrayPort: 29999, wsPort: 39999,
    portHopping: { enabled: true, start: 20000, end: 29999 },
    obfs: { enabled: true, type: 'salamander', password: 'legacy-cfg-obfs' },
};

class Element {
    constructor(name) {
        this.name = name; this.style = {}; this.children = []; this._text = ''; this._html = '';
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
    const el = name => {
        if (!elements.has(name)) elements.set(name, new Element(name));
        return elements.get(name);
    };
    const qr = [], clipboard = [], requests = [], opened = [];
    let apiUsers = [];
    class QR {
        constructor(element, options) { qr.push(options.text); element.appendChild(new Element('canvas')); }
    }
    QR.CorrectLevel = { M: 'M' };
    const context = vm.createContext({
        document: {
            querySelector: el, getElementById: id => el('#' + id),
            querySelectorAll: () => [el('#m-cfg')],
            createElement: name => new Element(name), createTextNode: text => ({ textContent: text }),
        },
        localStorage: { getItem: () => null, setItem() {}, removeItem() {} },
        location: { host: 'panel.example:8443', reload() {} },
        navigator: { clipboard: { writeText: value => { clipboard.push(value); return Promise.resolve(); } } },
        window: { open: (...args) => opened.push(args) },
        QRCode: QR, setTimeout: () => 0, clearTimeout() {}, setInterval: () => 0, clearInterval() {},
        confirm: () => false, alert() {},
        fetch: async url => {
            requests.push(url);
            const data = url === '/api/users' ? apiUsers : {};
            return { status: 200, json: async () => JSON.parse(JSON.stringify(data)) };
        },
    });
    new vm.Script(source, { filename: sourcePath }).runInContext(context);
    // This appended bridge accesses lexical bindings without modifying any product function.
    vm.runInContext(`globalThis.__review = {
        setUsers(json) { allUsers = JSON.parse(json); },
        setCfg(json) { cfg = JSON.parse(json); },
        gen(json) { return genUri(JSON.parse(json)); },
        show(name) { return showU(name); },
        copy() { return copy(); },
        load() { return load(); },
        subMissing() { return SUB_TOKEN_MISSING; },
        poisonLegacy() {
            cfg = new Proxy({}, { get(_t, key) { throw Error('unexpected cfg read: ' + String(key)); } });
            const x = JSON.parse(${JSON.stringify(JSON.stringify(base))});
            for (const key of ['username', 'password', 'uuid', 'sni', 'slotPort', 'slotHop']) {
                Object.defineProperty(x, key, { get() { throw Error('unexpected legacy field read: ' + key); } });
            }
            return genUri(x);
        }
    };`, context);
    context.__review.setCfg(JSON.stringify(legacyCfg));
    return {
        bridge: context.__review, el, qr, clipboard, requests, opened,
        setUsers(users) { context.__review.setUsers(JSON.stringify(users)); },
        async update(users) { apiUsers = JSON.parse(JSON.stringify(users)); await context.__review.load(); },
        gen(user) { return context.__review.gen(JSON.stringify(user)); },
        toasts() { return el('#t-box').children.map(child => child.innerHTML); },
    };
}

function shown(user) {
    const r = runtime(); r.setUsers([user]); r.bridge.show(user.username); return r;
}
function assertUnavailable(r, { residential = false } = {}) {
    const text = r.el('#uri').innerText;
    assert.ok(text, 'no URI must show an explicit message');
    assert.notEqual(text, r.bridge.subMissing(), 'single-node absence must not be reported as SUB_TOKEN_MISSING');
    assert.match(text, /不可用|暂不|无法|停用|未提供|未分配|未开通|耗尽|到期/, 'must explain unavailability');
    if (residential) assert.match(text, /住宅/, 'residential denial must mention residential path');
    assert.doesNotMatch(text, /(?:hysteria2|vless|https):\/\//, 'no connection URL may remain visible');
    assert.equal(r.el('#cfg-buttons').innerHTML, '', 'missing URI must have no copy button');
    assert.equal(r.el('#qrcode').children.length, 0, 'missing URI must have no QR content');
    assert.equal(r.el('#qrcode').innerHTML, '', 'missing URI must have no QR markup');
}

const tests = [
    ['serialized reserved HY2 credential is copied byte for byte', () => {
        const r = runtime(); assert.equal(r.gen(serialized()), CANONICAL);
    }],
    ['non-fusion genUri does not read legacy credentials, SNI, ports or cfg', () => {
        const r = runtime(); assert.equal(r.bridge.poisonLegacy(), CANONICAL);
    }],
    ['direct HY2 canonical URI wins over stale fields', () => {
        const r = runtime(); assert.equal(r.gen(serialized({ residential: false, nodeUri: DIRECT })), DIRECT);
    }],
    ['REALITY canonical URI preserves exact parameters and ordering', () => {
        const r = runtime(); assert.equal(r.gen(serialized({ protocol: 'vless-reality', nodeUri: REALITY })), REALITY);
    }],
    ['every single-protocol user without nodeUri produces no URI', () => {
        const r = runtime();
        for (const protocol of ['hysteria2', 'vless-reality', 'vless-ws-tls']) {
            assert.equal(r.gen(serialized({ protocol }, ['nodeUri'])), '', 'must not rebuild absent ' + protocol + ' URI');
        }
        assert.equal(r.gen(serialized({ nodeUri: '' })), '', 'empty nodeUri must not trigger fallback');
    }],
    ['unavailable and blocked realistic PanelUser payloads expose no URL', () => {
        const r = runtime();
        const users = [
            serialized({ residentialUnavailable: 'residential pool disabled' }, ['nodeUri', 'slot', 'slotIp', 'slotPort', 'slotHop']),
            serialized({ blocked: true, residentialUnavailable: 'account blocked' }, ['nodeUri']),
            serialized({ disabled: true, residential: false }, ['nodeUri']),
        ];
        users.forEach(user => assert.equal(r.gen(user), '', 'denied payload must not produce a link'));
    }],
    ['fusion subscription URL remains token based', () => {
        const r = runtime();
        const fusion = serialized({ protocol: 'fusion', residentialUnavailable: 'residential pool disabled' }, ['nodeUri']);
        assert.equal(r.gen(fusion), 'https://panel.example:8443/api/sub/synthetic-token%2F%25#alice');
        assert.equal(r.gen(serialized({ protocol: 'fusion' }, ['nodeUri', 'subToken'])), '');
    }],
    ['popup visible URI, QR and copied string use the exact canonical URI', () => {
        const r = shown(serialized());
        assert.equal(r.el('#uri').innerText, CANONICAL);
        assert.deepEqual(r.qr, [CANONICAL]);
        assert.match(r.el('#cfg-buttons').innerHTML, /onclick="copy\(\)"/);
        r.bridge.copy(); assert.deepEqual(r.clipboard, [CANONICAL]);
    }],
    ['no-node popup has explicit unavailable message, no button and no QR', () => {
        const r = shown(serialized({ residential: false }, ['nodeUri']));
        assertUnavailable(r); assert.deepEqual(r.qr, []);
    }],
    ['residential denial popup names the reason and suppresses URL controls', () => {
        const r = shown(serialized({ residentialUnavailable: 'residential pool disabled' }, ['nodeUri']));
        assertUnavailable(r, { residential: true });
        assert.match(r.el('#uri').innerText, /住宅池未启用/);
        assert.deepEqual(r.qr, []);
    }],
    ['copy() refuses no-node payload even when triggered without a button', () => {
        const r = shown(serialized({ residential: false }, ['nodeUri']));
        r.bridge.copy(); assert.deepEqual(r.clipboard, [], 'copy must not copy an explanatory or stale string');
    }],
    ['open popup refreshes after nodeUri changes without credential fields changing', async () => {
        const r = shown(serialized());
        await r.update([serialized({ nodeUri: CHANGED })]);
        assert.deepEqual(r.requests, ['/api/users', '/api/online', '/api/stats'], 'exercise real load() API path');
        assert.equal(r.el('#uri').innerText, CHANGED);
        assert.deepEqual(r.qr, [CANONICAL, CHANGED]);
        r.bridge.copy(); assert.deepEqual(r.clipboard, [CHANGED]);
        assert.equal(r.toasts().filter(text => text.includes('已被重置')).length, 0, 'node change alone must not claim credential reset');
    }],
    ['open popup retracts absent nodeUri without requiring residential reason change', async () => {
        const r = shown(serialized({ residential: false, nodeUri: DIRECT }));
        await r.update([serialized({ residential: false }, ['nodeUri'])]);
        assertUnavailable(r); assert.deepEqual(r.qr, [DIRECT]);
        r.bridge.copy(); assert.deepEqual(r.clipboard, []);
    }],
    ['open popup refreshes availability denial and recovery', async () => {
        const r = shown(serialized());
        await r.update([serialized({ residentialUnavailable: 'residential pool disabled' }, ['nodeUri'])]);
        assertUnavailable(r, { residential: true });
        r.bridge.copy(); assert.deepEqual(r.clipboard, []);
        await r.update([serialized({ nodeUri: CHANGED })]);
        assert.equal(r.el('#uri').innerText, CHANGED);
        assert.deepEqual(r.qr, [CANONICAL, CHANGED]);
    }],
    ['unchanged open popup does not redraw QR or emit a change toast', async () => {
        const r = shown(serialized()); await r.update([serialized()]);
        assert.deepEqual(r.qr, [CANONICAL]); assert.deepEqual(r.toasts(), []);
    }],
    ['fusion missing-token popup retains the established token diagnostic', () => {
        const r = shown(serialized({ protocol: 'fusion' }, ['nodeUri', 'subToken']));
        assert.equal(r.el('#uri').innerText, r.bridge.subMissing());
        assert.deepEqual(r.qr, []); r.bridge.copy(); assert.deepEqual(r.clipboard, []);
    }],
];

(async () => {
    console.log(JSON.stringify({ label, sourcePath, sourceSha256, driverPath: __filename, cases: tests.length, scope: 'actual whole app.js in isolated Node VM, synthetic serialized PanelUser and minimal DOM/API/QR mocks' }));
    const results = [];
    for (const [name, run] of tests) {
        try { await run(); results.push({ name, passed: true }); console.log('PASS ' + name); }
        catch (error) { results.push({ name, passed: false, error: error.message }); console.log('FAIL ' + name + '\n' + error.message); }
    }
    const passed = results.filter(result => result.passed).length;
    const report = { label, sourcePath, sourceSha256, driverSha256: crypto.createHash('sha256').update(fs.readFileSync(__filename)).digest('hex'), cases: tests.length, passed, failed: tests.length - passed, results };
    console.log(JSON.stringify({ label, cases: report.cases, passed, failed: report.failed }));
    process.exitCode = report.failed ? 1 : 0;
})().catch(error => { console.error(error.stack); process.exitCode = 2; });
