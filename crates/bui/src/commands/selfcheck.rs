//! 装完自检（2026-09-12 裁决「安装：一行命令与零手工配置」的「装完自检打印 PASS/FAIL 表」）。
//!
//! 判据照 `scripts/m1-acceptance.sh`：六单元在跑、三个内核校验器过、关键端口在监听、漂移为空，
//! 再加一条真正的**端到端**验证——用自带的 hysteria 客户端拿首个用户的凭据打一次外网
//! （m1-acceptance 没有这条，而「装完能不能用」只有它能回答）。
//!
//! 4.1 的住宅 HY2（sing-box 单入站 + nft 端口跳跃）另加五行：`table inet bui` 在位且规则等价、
//! **端口 1 没人监听**（`deny` 出站指向 `127.0.0.1:1`，前提没了就等于放行被封用户）、
//! 凭据池的已用 / 空闲 / 代数、两个回环控制面（Clash API + v2ray_api）在听，以及一条
//! **带整段跳跃**的住宅回环鉴权 —— 最后那条是 `inet bui` 的 `output` 链唯一的验证路径。
//!
//! 任一 FAIL 让 `bui install` 退 2，但**不回滚**：配置已经落盘、v3 已经卸掉，回滚只会把机器
//! 推到更糟的中间态；运维要的是「哪一项没过、怎么查」。
//!
//! 全新装机有一段**必然的**未就绪窗口（2026-09-12 审查 blocking）：`config.yaml` 的
//! `tls.cert` 指向 `<certs_dir>/fullchain.pem`，而那张证书要等 Caddy 跑完 ACME、再由守护进程的
//! 证书同步（[`crate::modules::certs`]）复制过来才存在；在那之前两个 hysteria 一起来就 FATAL
//! （`failed to load server config … tls.cert: no such file or directory`）进 auto-restart，
//! 对应的 UDP 端口也没在听。所以自检**先有界等一等**（[`Wait`]：≤120s、每 3s 一轮），
//! 等不到证书就把「两个 hysteria 单元 / HY2 的 UDP 端口 / 两条回环鉴权 / 住宅管理面端口」
//! 判成 SKIP 而不是 FAIL
//! ——否则一行命令装完必然退 2，与「一行命令完成新服务器的所有安装」直接冲突。
//! 同一段等待顺带消掉 `finish_self_restart` 刚 restart 过 `b-ui` 时 admin 口的竞态。

use crate::state::runtime::DriftItem;
use crate::sys::{Host, Proto};
use bui_schema::model::State;
use bui_schema::paths::Paths;
use std::time::Duration;

/// HY2 回环探测用的本机 socks5 端口：只在自检那十几秒里存在，选一个没人用的高位端口。
pub const PROBE_SOCKS_PORT: u16 = 45899;
/// 住宅 HY2 回环探测的 socks5 端口。**与直连那条不同号**：两条探测前后脚跑，
/// 前一个客户端被 `kill` 之后端口未必立刻释放，同号会让后一条假 FAIL。
pub const PROBE_SOCKS_PORT_RESI: u16 = 45900;
/// 回环探测打的外网地址（与 [`crate::sys::IP_PROBE_URLS`] 同源，200 即证明出网与鉴权都通）。
pub const PROBE_URL: &str = "https://api.ipify.org";

/// 自检前那段有界等待的预算。`Default` 是装机真跑的那一组；测试用 [`Wait::NONE`]（只查一轮，不睡）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wait {
    pub budget: Duration,
    pub poll: Duration,
}

impl Default for Wait {
    fn default() -> Self {
        // 120s 是「Caddy 签到 + 证书同步一轮（30s 兜底轮询）+ 两个 hysteria 重启」的宽裕上限；
        // 等不到就 SKIP 往下走，不在最后一屏无限期干等。
        Wait {
            budget: Duration::from_secs(120),
            poll: Duration::from_secs(3),
        }
    }
}

impl Wait {
    /// 一轮都不等（`poll` 为零即「查一次就返回」）。
    pub const NONE: Wait = Wait {
        budget: Duration::ZERO,
        poll: Duration::ZERO,
    };
}

/// 证书还没到位时那三行的说明。
pub const CERT_PENDING: &str =
    "待证书（Caddy 尚未签到；证书同步到位后 systemd 会自动拉起，`bui status` 复检）";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Fail,
    Skip,
}

impl Verdict {
    pub fn label(&self) -> &'static str {
        match self {
            Verdict::Pass => "PASS",
            Verdict::Fail => "FAIL",
            Verdict::Skip => "SKIP",
        }
    }
}

/// 自检表的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    pub verdict: Verdict,
    pub detail: String,
}

fn pass(name: impl Into<String>, detail: impl Into<String>) -> Check {
    Check {
        name: name.into(),
        verdict: Verdict::Pass,
        detail: detail.into(),
    }
}

fn fail(name: impl Into<String>, detail: impl Into<String>) -> Check {
    Check {
        name: name.into(),
        verdict: Verdict::Fail,
        detail: detail.into(),
    }
}

fn skip(name: impl Into<String>, detail: impl Into<String>) -> Check {
    Check {
        name: name.into(),
        verdict: Verdict::Skip,
        detail: detail.into(),
    }
}

/// FAIL 的条数（SKIP 不算）。
pub fn failures(rows: &[Check]) -> usize {
    rows.iter().filter(|c| c.verdict == Verdict::Fail).count()
}

/// 文件在不在。用 `list_dir` 而不是 `read_file`：内核动辄几十 MB，只为测存在把整个文件读进内存
/// 太亏（2026-09-12 审查 nit）。
fn file_exists(host: &dyn Host, path: &std::path::Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    host.list_dir(parent)
        .map(|kids| kids.iter().any(|k| k == path))
        .unwrap_or(false)
}

/// `<bin>/<program>` 优先，其次 PATH（与 apply 的校验器查找同一套规则）；都没有 `None`。
fn kernel_bin(host: &dyn Host, paths: &Paths, program: &str) -> Option<String> {
    let bundled = paths.bin_dir.join(program);
    if file_exists(host, &bundled) {
        return Some(bundled.display().to_string());
    }
    host.which(program).then(|| program.to_string())
}

/// 证书（`tls.cert`，两个 hysteria 起不来的唯一常见原因）在不在。
fn cert_present(host: &dyn Host, paths: &Paths) -> bool {
    file_exists(host, &paths.certs_dir.join("fullchain.pem"))
}

/// 这个单元起不来是不是「在等证书」。两个 hysteria 的 `config*.yaml` 都写着 `tls.cert`；
/// xray（REALITY）、relay、caddy、b-ui 都不读那张证书。
fn needs_cert(unit: &str) -> bool {
    unit.starts_with("hysteria")
}

/// 等证书的那两个 UDP 端口（HY2 直连与住宅）。
fn cert_bound_port(state: &State, proto: Proto, port: u16) -> bool {
    proto == Proto::Udp && (port == state.node.ports.hy2 || port == state.node.ports.hy2_resi)
}

/// 自检前的有界等待：等到「证书在位 + 六单元 active + 关键端口在听 + 住宅那两个回环控制面
/// 在听」，或耗尽预算。返回值是**证书到底有没有到位**——那几行是判 SKIP 还是 FAIL 全看它。
fn wait_ready(
    host: &dyn Host,
    paths: &Paths,
    state: &State,
    wait: Wait,
    sleep: &dyn Fn(Duration),
) -> bool {
    let mut waited = Duration::ZERO;
    loop {
        let cert = cert_present(host, paths);
        let units = crate::reconcile::managed_units(state)
            .iter()
            .all(|u| host.unit_is_active(u).unwrap_or(false));
        let ports = {
            let tcp = host.listening_ports(Proto::Tcp).unwrap_or_default();
            let udp = host.listening_ports(Proto::Udp).unwrap_or_default();
            listen_ports(state)
                .into_iter()
                .all(|(proto, port)| match proto {
                    Proto::Tcp => tcp.contains(&port),
                    Proto::Udp => udp.contains(&port),
                })
                // 住宅 sing-box 的两个回环控制面：inbound 先绑 `:40000`、experimental 的
                // 这两个面稍后才起，采样落进那个窗口会把一台健康机器判红（那一行是 FAIL
                // 级 ⇒ `bui install` 退 2）。只进**等待条件**，不进 `listen_ports`
                // ——那会改「关键端口」那一行的语义与它的 PASS 文案。
                && resi_api_ports().iter().all(|p| tcp.contains(p))
        };
        if cert && units && ports {
            return true;
        }
        if wait.poll.is_zero() || waited >= wait.budget {
            return cert;
        }
        sleep(wait.poll);
        waited += wait.poll;
    }
}

/// 关键端口：面板与证书的 443、REALITY 两个入站、HY2 直连与住宅、面板自己的 admin 口。
/// 两段跳跃与 4.0 兼容段都不查：那些端口上**没有任何进程监听**，包是由 `inet bui` 表
/// REDIRECT 到 `hy2_resi` 的（表本身由「nft 表 inet bui」那一行查）。
pub fn listen_ports(state: &State) -> Vec<(Proto, u16)> {
    let p = &state.node.ports;
    vec![
        (Proto::Tcp, 443),
        (Proto::Tcp, p.reality_direct),
        (Proto::Tcp, p.reality_resi),
        (Proto::Tcp, p.admin),
        (Proto::Udp, p.hy2),
        (Proto::Udp, p.hy2_resi),
    ]
}

/// 住宅 sing-box 的两个回环控制面端口（Clash API 与 v2ray_api）。端口只有一处来源——
/// 写出 `hy2-residential.json` 的那个渲染器；这里只把它们解析成数字。
/// **不在** [`listen_ports`] 里（那一行的语义是「关键端口」），只进有界等待的就绪条件与
/// 「住宅管理面端口」那一行。
fn resi_api_ports() -> Vec<u16> {
    use bui_schema::render::hy2_singbox::{HY2_RESI_CLASH_API, HY2_RESI_V2RAY_API};
    [HY2_RESI_CLASH_API, HY2_RESI_V2RAY_API]
        .iter()
        .filter_map(|a| api_port(a))
        .collect()
}

/// hysteria 客户端配置（凭据只落这里，0600，用完即删）。
pub fn hy2_client_yaml(state: &State, username: &str, hy2_password: &str) -> String {
    let n = &state.node;
    let mut y = format!(
        "server: 127.0.0.1:{}\nauth: {username}:{hy2_password}\ntls:\n  sni: {}\n  insecure: true\nsocks5:\n  listen: 127.0.0.1:{PROBE_SOCKS_PORT}\n",
        n.ports.hy2, n.domain
    );
    if n.obfs.enabled {
        y.push_str(&format!(
            "obfs:\n  type: salamander\n  salamander:\n    password: {}\n",
            n.obfs.password
        ));
    }
    y
}

/// 住宅 HY2 的 hysteria 客户端配置（凭据只落这里，0600，用完即删）。
///
/// 与直连那份差三处：① 打的是 `ports.hy2_resi`；② 认证串是**凭据池**里的
/// `{name}:{secret}`（不是用户的直连密码）；③ 地址里带上整段跳跃
/// （`:40000,41000-50000` —— hysteria 的端口跳跃写法，等价于订阅里的 `mport=`）。
///
/// 第三处是这条探测存在的理由：客户端随机挑跳跃段里的端口发包，而本机发往自身地址的包
/// **不过 prerouting**，只有 `inet bui` 的 **output** 链能把它们 REDIRECT 到 `:40000`。
/// 少了那条链时这一行就是唯一会红的判据（spec §2.4，tizi PoC 实测）。
pub fn hy2_resi_client_yaml(state: &State, name: &str, secret: &str) -> String {
    let n = &state.node;
    let (hop_a, hop_b) = n.ports.hy2_resi_hop;
    let mut y = format!(
        "server: 127.0.0.1:{},{hop_a}-{hop_b}\nauth: {name}:{secret}\ntls:\n  sni: {}\n  insecure: true\nsocks5:\n  listen: 127.0.0.1:{PROBE_SOCKS_PORT_RESI}\n",
        n.ports.hy2_resi, n.domain
    );
    if n.obfs.enabled {
        y.push_str(&format!(
            "obfs:\n  type: salamander\n  salamander:\n    password: {}\n",
            n.obfs.password
        ));
    }
    y
}

/// 回环探测最多试几轮、每轮 curl 最多等几秒：最坏 6×(1s+4s)≈30s 就放弃。
/// （连不上时这一项 FAIL，运维照 detail 去看 journalctl，不值得在最后一屏干等两分钟。）
const PROBE_ROUNDS: u32 = 6;
const PROBE_TIMEOUT: u32 = 4;

/// 探测脚本：起客户端 → 经它的 socks5 打一次外网 → 打印 HTTP 状态码 → 收掉客户端。
/// 凭据在配置文件里，**不进 argv**（`ps` 会泄露）。`socks` 要与 `cfg` 里的
/// `socks5.listen` 同号（直连与住宅两条探测各用一个端口）。
pub fn hy2_probe_sh(hysteria: &str, cfg: &str, socks: u16) -> String {
    format!(
        "#!/bin/sh\n\
         # bui install 自检生成，用完即删。\n\
         # 精简系统只有 wget（install.sh 也支持那种机器）：curl 不在就报 nocurl，让这一项 SKIP，\n\
         # 不是 FAIL —— wget 不会 socks5，这里没有等价替代。\n\
         command -v curl > /dev/null 2>&1 || {{ printf 'nocurl\\n'; exit 0; }}\n\
         \"{hysteria}\" client -c \"{cfg}\" > /dev/null 2>&1 &\n\
         pid=$!\n\
         code=000\n\
         i=0\n\
         while [ $i -lt {PROBE_ROUNDS} ]; do\n\
         \x20 i=$((i+1))\n\
         \x20 sleep 1\n\
         \x20 code=$(curl -s -o /dev/null -w '%{{http_code}}' --max-time {PROBE_TIMEOUT} \
         --socks5-hostname 127.0.0.1:{socks} '{PROBE_URL}')\n\
         \x20 [ \"$code\" != \"000\" ] && break\n\
         done\n\
         kill \"$pid\" > /dev/null 2>&1\n\
         printf '%s\\n' \"$code\"\n"
    )
}

/// 系统没有 curl 时两条回环探测共用的 SKIP 说明。
const NOCURL: &str = "系统没有 curl（wget 不支持 socks5），跳过；装上 curl 后 `bui status` 复检";

/// 跑一次回环探测：写 0600 配置 + 0700 脚本 → `sh` → **无论成败都删掉**这两个临时文件
/// （配置里有明文凭据），返回脚本打印的最后一行（HTTP 状态码或 `nocurl`）。
///
/// 直连与住宅两条探测共用它：`stem` 是临时文件名前缀，`socks` 要与 `yaml` 里的
/// `socks5.listen` 同号。凭据只在配置文件里，一个字都不进 argv（`ps` 会泄露）。
fn probe_once(
    host: &dyn Host,
    paths: &Paths,
    stem: &str,
    yaml: &str,
    hysteria: &str,
    socks: u16,
) -> anyhow::Result<String> {
    let dir = crate::paths::verify_dir(paths);
    let (cfg, script) = (
        dir.join(format!("{stem}.yaml")),
        dir.join(format!("{stem}.sh")),
    );
    let sh = hy2_probe_sh(hysteria, &cfg.display().to_string(), socks);
    let out = match host
        .write_file(&cfg, yaml.as_bytes(), 0o600)
        .and_then(|()| host.write_file(&script, sh.as_bytes(), 0o700))
    {
        Ok(()) => host.run("sh", &[&script.display().to_string()]),
        Err(e) => Err(e),
    };
    let _ = host.remove_file(&cfg);
    let _ = host.remove_file(&script);
    Ok(out?
        .stdout
        .trim()
        .lines()
        .last()
        .unwrap_or_default()
        .trim()
        .to_string())
}

/// HY2 端到端：首个可用用户的凭据经回环打一次外网，期望 200。
/// 没有用户（全新装机）或没有 hysteria 二进制 ⇒ SKIP，不算失败。
fn hy2_loopback(host: &dyn Host, paths: &Paths, state: &State, cert_ready: bool) -> Check {
    let name = "HY2 回环鉴权";
    if !cert_ready {
        // 证书还没到 ⇒ hysteria 根本没在跑，打过去必然 000：这不是「装坏了」，别染红。
        return skip(name, CERT_PENDING);
    }
    let Some(user) = state.users.iter().find(|u| !u.disabled) else {
        return skip(name, "还没有用户（全新装机）；加完用户后 `bui status` 复检");
    };
    let Some(hysteria) = kernel_bin(host, paths, "hysteria") else {
        return skip(name, "找不到 hysteria 二进制，跳过");
    };
    let yaml = hy2_client_yaml(state, &user.username, &user.credentials.hy2_password);
    match probe_once(
        host,
        paths,
        "selfcheck-hy2",
        &yaml,
        &hysteria,
        PROBE_SOCKS_PORT,
    ) {
        Ok(code) if code == "200" => pass(name, format!("{} 经本机 HY2 出网 200", user.username)),
        Ok(code) if code == "nocurl" => skip(name, NOCURL),
        Ok(code) => fail(
            name,
            format!(
                "{} 经本机 HY2 打 {PROBE_URL} 得到 {}（0/000 = 连不上：看 journalctl -u hysteria-server）",
                user.username,
                if code.is_empty() { "空" } else { &code }
            ),
        ),
        Err(e) => fail(name, format!("探测脚本跑不起来：{e}")),
    }
}

/// 住宅 HY2 端到端（4.1）：首个持有凭据池凭据的用户，用 `{name}:{secret}` **带整段跳跃**
/// 打一次外网，期望 200。
///
/// 它一次验四件事：凭据池里那条凭据能过 sing-box 的鉴权、`auth_user` 路由到了门、门后的
/// 槽出站通、以及 `inet bui` 的 **output** 链把跳跃段 REDIRECT 到了 `:40000`
/// ——最后一件只有「本机带 `mport` 打自己」这条路能验（spec §2.4）。
///
/// 证书未到、没有凭据（还没迁移 / 没有住宅 hysteria2 用户）、没有 hysteria 二进制 ⇒ SKIP。
fn hy2_resi_loopback(host: &dyn Host, paths: &Paths, state: &State, cert_ready: bool) -> Check {
    let name = "HY2 住宅回环鉴权";
    if !cert_ready {
        return skip(name, CERT_PENDING);
    }
    // 取首个**门开着**的持凭据用户。门在 `deny` 上的人拿他的凭据探测必然打不通（那是
    // 正确行为，不是故障），会把这一行变成假 FAIL 并让 `bui install` 退 2。「门在 deny 上」
    // 远不止停用：到期、超总量 / 超月量（`users::is_blocked` 四种）与**撤掉了住宅
    // hysteria2 权益**的持凭据用户（`traffic::resi_kick_targets` 的 `restore_to` 口径）
    // 同样停在 deny —— 住宅 HY2 的到期 / 限额**只**靠门落地（鉴权在 sing-box 的静态凭据
    // 池里，没有 auth-hook）。
    let now = host.now();
    let open = |u: &bui_schema::model::User| {
        crate::modules::panel::users::is_blocked(u, crate::modules::panel::TxRx::default(), now)
            .is_none()
            && bui_schema::hy2pool::is_resi_hy2(u, &state.residential)
    };
    let Some((user, cred)) = state
        .users
        .iter()
        .filter(|u| open(u))
        .find_map(|u| bui_schema::hy2pool::cred_of(u, &state.residential).map(|c| (u, c)))
    else {
        // 挑不出人就跳过这一行（不是 FAIL），并说清是哪一种「挑不出」
        let held = state
            .users
            .iter()
            .any(|u| bui_schema::hy2pool::cred_of(u, &state.residential).is_some());
        return skip(
            name,
            if held {
                "持凭据的用户都停在 deny（停用 / 到期 / 超量 / 撤了住宅 hysteria2 权益），\
                 拿他们的凭据探测必然打不通；给一个在用的住宅用户后 `bui status` 复检"
            } else {
                "还没有住宅 HY2 凭据（没有住宅 hysteria2 用户，或凭据池还没建起来）"
            },
        );
    };
    let Some(hysteria) = kernel_bin(host, paths, "hysteria") else {
        return skip(name, "找不到 hysteria 二进制，跳过");
    };
    let yaml = hy2_resi_client_yaml(state, &cred.name, &cred.secret);
    match probe_once(
        host,
        paths,
        "selfcheck-hy2-resi",
        &yaml,
        &hysteria,
        PROBE_SOCKS_PORT_RESI,
    ) {
        Ok(code) if code == "200" => pass(
            name,
            format!("{} 带跳跃段经本机住宅 HY2 出网 200", user.username),
        ),
        Ok(code) if code == "nocurl" => skip(name, NOCURL),
        Ok(code) => fail(
            name,
            format!(
                "{} 带跳跃段打 {PROBE_URL} 得到 {}（0/000 = 连不上：查 `bui nft status` 的 output 链与 journalctl -u hysteria-residential）",
                user.username,
                if code.is_empty() { "空" } else { &code }
            ),
        ),
        Err(e) => fail(name, format!("探测脚本跑不起来：{e}")),
    }
}

/// nft 表那一行：`nft` 二进制在不在 + `table inet bui` 在不在 + 规则与期望是否等价。
///
/// 缺 `nft` 时**本行直接 FAIL**（住宅 HY2 等于全断：带 `mport` 的客户端只往跳跃段发、
/// 从不发 `:40000`）。判据是规范化后的规则集（[`crate::modules::watchdog::redirect_rules`]），
/// 不逐字比 `nft` 的回显——`nft list` 会重排空白、改写 priority、插 counter。
fn nft_table_row(host: &dyn Host, state: &State) -> Check {
    use bui_schema::render::nft;
    let name = "nft 表 inet bui";
    let (hop_a, hop_b) = state.node.ports.hy2_resi_hop;
    if !host.which("nft") {
        return fail(
            name,
            format!(
                "缺少 nft 二进制，住宅 HY2 等于全断（带 mport 的客户端只往 {hop_a}-{hop_b} 发、\
                 从不发 :{}）：先装 nftables 包，再 `bui nft apply`",
                state.node.ports.hy2_resi
            ),
        );
    }
    let compat = state.system.hy2_resi_compat_ports;
    let want = nft::ruleset(&state.node.ports, compat);
    let listed = match host.run("nft", &["list", "table", nft::FAMILY, nft::NAME]) {
        Ok(o) if o.ok() => o.stdout,
        Ok(_) => {
            return fail(
                name,
                format!(
                    "table {} 不存在：住宅 HY2 的端口跳跃整段不通，执行 `bui nft apply`",
                    nft::TABLE
                ),
            )
        }
        Err(e) => return fail(name, format!("nft list table 跑不起来：{e}")),
    };
    let got = crate::modules::watchdog::redirect_rules(&listed);
    let expect = crate::modules::watchdog::redirect_rules(&want);
    if got != expect {
        return fail(
            name,
            format!(
                "规则不符（期望 {} 条 redirect，实到 {} 条）：执行 `bui nft apply` 重放，\
                 `bui nft status` 看现状",
                expect.len(),
                got.len()
            ),
        );
    }
    pass(
        name,
        format!(
            "{} 条规则在位：{hop_a}-{hop_b}{} → :{}",
            nft::rule_count(compat),
            if compat {
                let (a, b) = nft::compat_range(&state.node.ports);
                format!("、兼容段 {a}-{b}")
            } else {
                String::new()
            },
            state.node.ports.hy2_resi
        ),
    )
}

/// `deny` 出站指向 `127.0.0.1:1`，所以端口 1 上**永不许**有人监听（spec §2.2、§8.3）：
/// 一旦有，被封 / 到期用户的流量就会经那个进程出网，而不是被拒。
fn port_one_row(
    tcp: &std::collections::BTreeSet<u16>,
    udp: &std::collections::BTreeSet<u16>,
) -> Check {
    let name = "端口 1 未被监听";
    if tcp.contains(&1) || udp.contains(&1) {
        return fail(
            name,
            "有进程在监听端口 1：住宅 HY2 的 deny 出站指向 127.0.0.1:1，\
             被封 / 到期的用户会经它出网而不是被拒",
        );
    }
    pass(name, "没有进程监听端口 1（deny 出站的前提）")
}

/// 住宅 HY2 的静态凭据池：已用 / 空闲 / 代数。空闲耗尽 ⇒ FAIL（新住宅用户分不到门位）。
/// 池还没建起来（没有住宅 hysteria2 用户、或还没迁移过）⇒ SKIP。
fn hy2_pool_row(state: &State) -> Check {
    let name = "住宅 HY2 凭据池";
    let pool = &state.residential.hy2_pool;
    if pool.creds.is_empty() {
        return skip(name, "凭据池还没建起来（没有住宅 hysteria2 用户）");
    }
    let used: std::collections::BTreeSet<String> = state
        .users
        .iter()
        .filter_map(|u| u.credentials.hy2_resi_cred.clone())
        .collect();
    let free = bui_schema::hy2pool::free_count(pool, &used);
    let detail = format!(
        "已用 {} / 空闲 {free} / 第 {} 代",
        pool.creds.len() - free,
        pool.generation
    );
    if free == 0 {
        return fail(
            name,
            format!("{detail}：空闲耗尽，新的住宅用户分不到门位（下一轮对账会扩容）"),
        );
    }
    pass(name, detail)
}

/// 住宅 sing-box 的两个回环控制面：Clash API（在线数 / 踢连接 / 门位）与
/// v2ray_api（计量）。端口只有一处来源——写出 `hy2-residential.json` 的那个渲染器。
/// 证书未到时住宅内核根本没起来 ⇒ SKIP（与 HY2 的 UDP 口同一口径）。
fn hy2_resi_api_row(tcp: &std::collections::BTreeSet<u16>, cert_ready: bool) -> Check {
    use bui_schema::render::hy2_singbox::{HY2_RESI_CLASH_API, HY2_RESI_V2RAY_API};
    let name = "住宅管理面端口";
    // 时序保护在 [`wait_ready`]：这两个端口进就绪条件，所以走到这里时要么已经绑上、
    // 要么预算等满了（真有问题）。
    if !cert_ready {
        return skip(name, CERT_PENDING);
    }
    let missing: Vec<String> = [HY2_RESI_CLASH_API, HY2_RESI_V2RAY_API]
        .iter()
        .filter(|addr| !api_port(addr).is_some_and(|p| tcp.contains(&p)))
        .map(|addr| (*addr).to_string())
        .collect();
    if !missing.is_empty() {
        return fail(
            name,
            format!(
                "{} 没在监听：住宅的计量（v2ray_api）与门位 / 在线数（Clash API）会双双打空",
                missing.join("、")
            ),
        );
    }
    pass(
        name,
        format!("Clash API {HY2_RESI_CLASH_API} 与 v2ray_api {HY2_RESI_V2RAY_API} 在监听"),
    )
}

/// `127.0.0.1:9092` → `9092`（端口不另写字面量，只从渲染器那两个常量里取）。
fn api_port(addr: &str) -> Option<u16> {
    addr.rsplit(':').next()?.parse().ok()
}

/// 跑一遍自检。`drift` 直接用这一轮对账报告里的漂移（不再重扫一遍）。
pub fn run(
    host: &dyn Host,
    paths: &Paths,
    state: &State,
    drift: &[DriftItem],
    wait: Wait,
) -> Vec<Check> {
    run_with_clock(host, paths, state, drift, wait, &std::thread::sleep)
}

/// [`run`] 的可注入时钟版本（测试用假 `sleep` 把那 120s 压成零）。
pub fn run_with_clock(
    host: &dyn Host,
    paths: &Paths,
    state: &State,
    drift: &[DriftItem],
    wait: Wait,
    sleep: &dyn Fn(Duration),
) -> Vec<Check> {
    // 先等：全新装机的证书、刚被 restart 的 b-ui 的 admin 口，都要几十秒才到位。
    let cert_ready = wait_ready(host, paths, state, wait, sleep);
    let mut rows = Vec::new();
    for unit in crate::reconcile::managed_units(state) {
        let unit = unit.as_str();
        let active = host.unit_is_active(unit).unwrap_or(false);
        rows.push(if active {
            pass(format!("单元 {unit}"), "running")
        } else if !cert_ready && needs_cert(unit) {
            skip(format!("单元 {unit}"), CERT_PENDING)
        } else {
            fail(
                format!("单元 {unit}"),
                format!("未运行：journalctl -u {unit} -n 50"),
            )
        });
    }
    // 三个校验器跑的是**已落盘**的配置（不是 .verify/ 里的候选）
    for (program, name, args) in [
        (
            "xray",
            "xray run -test",
            vec![
                "run".to_string(),
                "-test".to_string(),
                "-c".to_string(),
                paths
                    .base_dir
                    .join("xray-config.json")
                    .display()
                    .to_string(),
            ],
        ),
        (
            "sing-box",
            "sing-box check",
            vec![
                "check".to_string(),
                "-c".to_string(),
                paths
                    .base_dir
                    .join("singbox-relay.json")
                    .display()
                    .to_string(),
            ],
        ),
        (
            "caddy",
            "caddy validate",
            vec![
                "validate".to_string(),
                "--config".to_string(),
                crate::paths::caddyfile(paths).display().to_string(),
                "--adapter".to_string(),
                "caddyfile".to_string(),
            ],
        ),
    ] {
        let Some(bin) = kernel_bin(host, paths, program) else {
            rows.push(skip(name, format!("{program} 二进制不在，跳过")));
            continue;
        };
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        rows.push(match host.run(&bin, &argv) {
            Ok(o) if o.ok() => pass(name, "配置合法"),
            Ok(o) => fail(
                name,
                if o.stderr.trim().is_empty() {
                    o.stdout.trim().to_string()
                } else {
                    o.stderr.trim().to_string()
                },
            ),
            Err(e) => fail(name, format!("跑不起来：{e}")),
        });
    }
    // 关键端口在监听
    let tcp = host.listening_ports(Proto::Tcp).unwrap_or_default();
    let udp = host.listening_ports(Proto::Udp).unwrap_or_default();
    let label = |proto: Proto, port: u16| {
        format!(
            "{port}/{}",
            match proto {
                Proto::Tcp => "tcp",
                Proto::Udp => "udp",
            }
        )
    };
    // 没在听的端口分两堆：等证书的那两个 UDP 口（SKIP）与真有问题的（FAIL）
    let (mut pending, mut missing) = (Vec::new(), Vec::new());
    for (proto, port) in listen_ports(state) {
        let listening = match proto {
            Proto::Tcp => tcp.contains(&port),
            Proto::Udp => udp.contains(&port),
        };
        if listening {
            continue;
        }
        if !cert_ready && cert_bound_port(state, proto, port) {
            pending.push(label(proto, port));
        } else {
            missing.push(label(proto, port));
        }
    }
    rows.push(if !missing.is_empty() {
        fail("关键端口", format!("没在监听：{}", missing.join("、")))
    } else if !pending.is_empty() {
        skip("关键端口", format!("{} {CERT_PENDING}", pending.join("、")))
    } else {
        pass("关键端口", "443/REALITY/HY2/面板 全部在监听")
    });
    // 住宅 HY2 那一套（4.1）：跳跃靠 nft 表、封禁靠 deny 出站、门位与计量靠两个回环面
    rows.push(nft_table_row(host, state));
    rows.push(port_one_row(&tcp, &udp));
    rows.push(hy2_pool_row(state));
    rows.push(hy2_resi_api_row(&tcp, cert_ready));
    // 漂移为空
    rows.push(if drift.is_empty() {
        pass("漂移", "无漂移")
    } else {
        fail(
            "漂移",
            drift
                .iter()
                .map(|d| format!("{} {}", d.kind, d.path))
                .collect::<Vec<_>>()
                .join("；"),
        )
    });
    rows.push(hy2_loopback(host, paths, state, cert_ready));
    rows.push(hy2_resi_loopback(host, paths, state, cert_ready));
    rows
}

/// PASS/FAIL 表（装机最后一屏）。
pub fn table(rows: &[Check]) -> String {
    // 名字列按**显示宽度**对齐：「关键端口」占 8 列而 chars() 只数 4
    let width = rows
        .iter()
        .map(|c| crate::sys::env_probe::display_width(&c.name))
        .max()
        .unwrap_or(0);
    let mut out = vec![format!(
        "自检（{} 项，{} 项未通过）",
        rows.len(),
        failures(rows)
    )];
    for c in rows {
        out.push(format!(
            "  {}  {}  {}",
            c.verdict.label(),
            crate::sys::env_probe::pad_display(&c.name, width),
            c.detail,
        ));
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::{fake::FakeHost, CmdOut, Host};
    use pretty_assertions::assert_eq;

    /// 测试一律走不等待的时钟版：真 `run` 会睡满 120s。
    fn no_wait(h: &dyn Host, p: &Paths, state: &State, drift: &[DriftItem]) -> Vec<Check> {
        run_with_clock(h, p, state, drift, Wait::NONE, &|_| {})
    }

    fn paths(d: &tempfile::TempDir) -> Paths {
        Paths {
            base_dir: d.path().into(),
            certs_dir: d.path().join("certs"),
            bin_dir: d.path().join("bin"),
        }
    }

    /// 一台机器上 `nft list table inet bui` 的回显：由渲染器那份规则集按**真机的打印方式**
    /// 改写（`priority dstnat`、每条规则插一段 `counter packets N bytes N`、没有开头那两行
    /// `table` / `flush table`）。夹具这么造才证明得了「比的是规范化后的规则，不是回显文本」。
    fn nft_listing(state: &State) -> String {
        let mut out: String =
            bui_schema::render::nft::ruleset(&state.node.ports, state.system.hy2_resi_compat_ports)
                .lines()
                .skip(2)
                .map(|l| {
                    l.replace("priority -100", "priority dstnat")
                        .replace("counter redirect", "counter packets 7 bytes 280 redirect")
                        + "\n"
                })
                .collect();
        out.push('\n');
        out
    }

    /// 一台「什么都对」的假机器：六单元在跑、四内核在位、证书已同步、关键端口在听、
    /// `inet bui` 表在位（4.1 的住宅跳跃）、住宅那两个回环面在听、探测脚本回 200。
    fn healthy(p: &Paths, state: &State) -> FakeHost {
        let h = FakeHost::new();
        h.with(|i| {
            i.files
                .insert(p.certs_dir.join("fullchain.pem"), (b"CERT".to_vec(), 0o644));
            for u in crate::reconcile::managed_units(state) {
                i.units_active.insert(format!("{u}.service"));
            }
            for k in crate::kernels::KERNELS {
                i.files.insert(p.bin_dir.join(k), (b"ELF".to_vec(), 0o755));
            }
            let (mut tcp, mut udp) = (
                std::collections::BTreeSet::new(),
                std::collections::BTreeSet::new(),
            );
            for (proto, port) in listen_ports(state) {
                match proto {
                    Proto::Tcp => tcp.insert(port),
                    Proto::Udp => udp.insert(port),
                };
            }
            // 住宅 sing-box 的两个回环面（Clash API + v2ray_api）
            for addr in [
                bui_schema::render::hy2_singbox::HY2_RESI_CLASH_API,
                bui_schema::render::hy2_singbox::HY2_RESI_V2RAY_API,
            ] {
                tcp.insert(api_port(addr).unwrap());
            }
            i.listening.insert(Proto::Tcp, tcp);
            i.listening.insert(Proto::Udp, udp);
            i.which.insert("nft".into());
            i.scripted.push((
                "nft list table inet bui".into(),
                CmdOut::success(&nft_listing(state)),
            ));
            i.scripted.push(("sh ".into(), CmdOut::success("200\n")));
        });
        h
    }

    #[test]
    fn a_healthy_machine_passes_every_check() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        let state = crate::testutil::sample_state();
        let h = healthy(&p, &state);
        let rows = no_wait(&h, &p, &state, &[]);
        assert_eq!(failures(&rows), 0, "{}", table(&rows));
        // 六单元 + 三校验器 + 端口 + nft 表 + 端口 1 + 凭据池 + 住宅管理面 + 漂移
        // + HY2 回环 + HY2 住宅回环
        assert_eq!(
            rows.len(),
            crate::reconcile::managed_units(&state).len() + 11
        );
        let t = table(&rows);
        for want in [
            "单元 b-ui",
            "单元 caddy",
            "xray run -test",
            "sing-box check",
            "caddy validate",
            "关键端口",
            "nft 表 inet bui",
            "端口 1 未被监听",
            "住宅管理面端口",
            "无漂移",
            "HY2 回环鉴权",
        ] {
            assert!(t.contains(want), "自检表缺「{want}」：\n{t}");
        }
        assert!(!t.contains("FAIL"), "{t}");
    }

    #[test]
    fn every_kind_of_breakage_shows_up_as_fail() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        let state = crate::testutil::sample_state();
        let h = healthy(&p, &state);
        h.with(|i| {
            i.units_active.remove("caddy.service");
            i.listening.insert(Proto::Udp, Default::default());
            i.scripted.insert(
                0,
                (
                    format!("{} run -test", p.bin_dir.join("xray").display()),
                    CmdOut::failure(1, "bad inbound"),
                ),
            );
            // 回环探测：客户端起不来，脚本打印 000
            i.scripted
                .insert(0, ("sh ".into(), CmdOut::success("000\n")));
        });
        let rows = no_wait(
            &h,
            &p,
            &state,
            &[DriftItem {
                kind: "cron".into(),
                path: "crontab".into(),
                detail: "0 */6 * * * /opt/b-ui/update.sh".into(),
            }],
        );
        let by = |n: &str| rows.iter().find(|c| c.name == n).unwrap().clone();
        assert_eq!(by("单元 caddy").verdict, Verdict::Fail);
        assert!(by("单元 caddy").detail.contains("journalctl -u caddy"));
        assert_eq!(by("单元 b-ui").verdict, Verdict::Pass);
        assert_eq!(by("xray run -test").verdict, Verdict::Fail);
        assert_eq!(by("xray run -test").detail, "bad inbound");
        assert_eq!(by("关键端口").verdict, Verdict::Fail);
        assert!(
            by("关键端口").detail.contains("10000/udp")
                && by("关键端口").detail.contains("40000/udp"),
            "{:?}",
            by("关键端口").detail
        );
        assert_eq!(by("漂移").verdict, Verdict::Fail);
        assert!(by("漂移").detail.contains("crontab"));
        assert_eq!(by("HY2 回环鉴权").verdict, Verdict::Fail);
        assert!(by("HY2 回环鉴权").detail.contains("000"));
        // caddy 没起 + xray 校验失败 + 端口没听 + 有漂移 + HY2 回环不通
        assert_eq!(failures(&rows), 5);
        assert!(table(&rows).contains("5 项未通过"));
    }

    /// 全新装机没有用户 ⇒ HY2 回环无从可试，SKIP 而不是 FAIL（否则一行命令装完必然退 2）。
    #[test]
    fn a_fresh_machine_skips_the_loopback_probe_instead_of_failing() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        let mut state = crate::testutil::sample_state();
        state.users.clear();
        let h = healthy(&p, &state);
        let rows = no_wait(&h, &p, &state, &[]);
        let probe = rows.iter().find(|c| c.name == "HY2 回环鉴权").unwrap();
        assert_eq!(probe.verdict, Verdict::Skip);
        assert!(probe.detail.contains("还没有用户"), "{}", probe.detail);
        assert_eq!(failures(&rows), 0);
        // 一条 sh 都不该跑（没有用户就不写临时配置）
        assert!(
            !h.ops().iter().any(|o| o.starts_with("run:sh")),
            "{:?}",
            h.ops()
        );
    }

    /// 凭据只进 0600 的临时配置、不进 argv，且无论成败都清理掉。
    #[test]
    fn the_probe_keeps_credentials_out_of_argv_and_always_cleans_up() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        let state = crate::testutil::sample_state();
        let pw = state.users[0].credentials.hy2_password.clone();
        let h = healthy(&p, &state);
        h.with(|i| {
            i.scripted
                .insert(0, ("sh ".into(), CmdOut::failure(1, "boom")))
        });
        let rows = no_wait(&h, &p, &state, &[]);
        assert_eq!(
            rows.iter()
                .find(|c| c.name == "HY2 回环鉴权")
                .unwrap()
                .verdict,
            Verdict::Fail
        );
        let cfg = crate::paths::verify_dir(&p).join("selfcheck-hy2.yaml");
        let sh = crate::paths::verify_dir(&p).join("selfcheck-hy2.sh");
        assert_eq!(h.mode(&cfg.display().to_string()), None, "临时配置必须删掉");
        assert_eq!(h.mode(&sh.display().to_string()), None, "临时脚本必须删掉");
        let ops = h.ops();
        assert!(
            ops.contains(&format!("write:{}:600", cfg.display())),
            "配置要 0600：{ops:?}"
        );
        assert!(
            ops.contains(&format!("remove:{}", cfg.display())),
            "{ops:?}"
        );
        assert!(ops.contains(&format!("remove:{}", sh.display())), "{ops:?}");
        for op in &ops {
            assert!(!op.contains(&pw), "凭据进了命令行/流水：{op}");
        }
        // 配置里才有凭据，脚本里没有
        let yaml = hy2_client_yaml(&state, &state.users[0].username, &pw);
        assert!(
            yaml.contains(&format!("auth: {}:{pw}", state.users[0].username)),
            "{yaml}"
        );
        assert!(
            yaml.contains(&format!("listen: 127.0.0.1:{PROBE_SOCKS_PORT}")),
            "{yaml}"
        );
        let sh_text = hy2_probe_sh(
            "/opt/b-ui/bin/hysteria",
            "/opt/b-ui/.verify/selfcheck-hy2.yaml",
            PROBE_SOCKS_PORT,
        );
        assert!(!sh_text.contains(&pw), "{sh_text}");
        assert!(
            sh_text.contains("--socks5-hostname 127.0.0.1:45899"),
            "{sh_text}"
        );
        assert!(sh_text.contains(PROBE_URL), "{sh_text}");
        assert!(sh_text.contains("%{http_code}"), "{sh_text}");
    }

    #[test]
    fn obfs_is_carried_into_the_probe_config_only_when_enabled() {
        let mut state = crate::testutil::sample_state();
        let u = state.users[0].username.clone();
        let pw = state.users[0].credentials.hy2_password.clone();
        assert!(!hy2_client_yaml(&state, &u, &pw).contains("obfs"));
        state.node.obfs = bui_schema::model::Obfs {
            enabled: true,
            password: "obfs-pw".into(),
        };
        let y = hy2_client_yaml(&state, &u, &pw);
        assert!(
            y.contains("type: salamander") && y.contains("password: obfs-pw"),
            "{y}"
        );
    }

    /// 精简系统（只装了 wget）：install.sh 刻意支持那种机器，带用户的 import-v3 路径不该因为
    /// 缺 curl 就把自检染红、退 2。探测脚本自己报 nocurl ⇒ 这一项 SKIP。
    #[test]
    fn a_system_without_curl_skips_the_loopback_probe_instead_of_failing() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        let state = crate::testutil::sample_state();
        let h = healthy(&p, &state);
        h.with(|i| {
            i.scripted
                .insert(0, ("sh ".into(), CmdOut::success("nocurl\n")));
        });
        let rows = no_wait(&h, &p, &state, &[]);
        let c = rows.iter().find(|c| c.name == "HY2 回环鉴权").unwrap();
        assert_eq!(c.verdict, Verdict::Skip, "{:?}", c.detail);
        assert!(c.detail.contains("curl"), "{}", c.detail);
        assert_eq!(failures(&rows), 0, "缺 curl 不算失败");
        // 脚本里那一行就是 SKIP 的来源，顺带钉住「最后一屏不干等两分钟」的预算
        let sh = hy2_probe_sh("/opt/b-ui/bin/hysteria", "/x.yaml", PROBE_SOCKS_PORT);
        assert!(sh.contains("command -v curl"), "{sh}");
        assert!(sh.contains("nocurl"), "{sh}");
        assert!(sh.contains("--max-time 4") && sh.contains("-lt 6"), "{sh}");
    }

    /// 全新装机的那台机器：一开始没证书、两个 hysteria 没起、HY2 的 UDP 口没在听
    /// （`config.yaml` 的 `tls.cert` 还不存在，hysteria 启动即 FATAL 进 auto-restart）；
    /// 几轮之后 Caddy 签到、证书同步复制过来、两个单元起稳。自检必须**等到那时**再判，
    /// 一项都不 FAIL —— 否则一行命令装完必然退 2。
    #[test]
    fn a_fresh_machine_waits_for_the_first_certificate_instead_of_failing() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        let state = crate::testutil::sample_state();
        let h = healthy(&p, &state);
        let cert = p.certs_dir.join("fullchain.pem");
        // 起始态：没证书、两个 hysteria 没 active、两个 HY2 的 UDP 口没在听
        h.with(|i| {
            i.files.remove(&cert);
            i.units_active.remove("hysteria-server.service");
            i.units_active.remove("hysteria-residential.service");
            i.listening.insert(Proto::Udp, Default::default());
        });
        let rounds = std::cell::Cell::new(0u32);
        let rows = run_with_clock(&h, &p, &state, &[], Wait::default(), &|dur| {
            assert_eq!(dur, Duration::from_secs(3), "每 3s 一轮");
            rounds.set(rounds.get() + 1);
            if rounds.get() == 4 {
                // 第 4 轮：证书到位 + 两个单元起稳 + UDP 口挂上
                h.with(|i| {
                    i.files.insert(cert.clone(), (b"CERT".to_vec(), 0o644));
                    i.units_active.insert("hysteria-server.service".into());
                    i.units_active.insert("hysteria-residential.service".into());
                    let mut udp = std::collections::BTreeSet::new();
                    for (proto, port) in listen_ports(&state) {
                        if proto == Proto::Udp {
                            udp.insert(port);
                        }
                    }
                    i.listening.insert(Proto::Udp, udp);
                });
            }
        });
        assert_eq!(rounds.get(), 4, "等到就绪就不再睡");
        assert_eq!(failures(&rows), 0, "{}", table(&rows));
        assert!(!table(&rows).contains("FAIL"), "{}", table(&rows));
    }

    /// 等满预算证书还是没来（DNS 没指过来 / ACME 限流）：那三行报 SKIP 而不是 FAIL，
    /// 装机不退 2；其余各项照旧判定（这里 xray 仍然要 PASS）。
    #[test]
    fn a_certificate_that_never_arrives_skips_those_three_rows() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        let state = crate::testutil::sample_state();
        let h = healthy(&p, &state);
        h.with(|i| {
            i.files.remove(&p.certs_dir.join("fullchain.pem"));
            i.units_active.remove("hysteria-server.service");
            i.units_active.remove("hysteria-residential.service");
            i.listening.insert(Proto::Udp, Default::default());
            // 住宅内核根本没起来 ⇒ 它那两个回环控制面也没绑（只清 UDP 口是假现场：
            // 那样「住宅管理面端口」那一行照样 PASS，证书闸门就无人看守了）
            let mut tcp = i.listening.get(&Proto::Tcp).cloned().unwrap_or_default();
            tcp.retain(|port| !resi_api_ports().contains(port));
            i.listening.insert(Proto::Tcp, tcp);
        });
        let slept = std::cell::Cell::new(Duration::ZERO);
        let rows = run_with_clock(&h, &p, &state, &[], Wait::default(), &|d| {
            slept.set(slept.get() + d)
        });
        assert_eq!(slept.get(), Duration::from_secs(120), "等待有界：最多 120s");
        let by = |n: &str| rows.iter().find(|c| c.name == n).unwrap().clone();
        for n in ["单元 hysteria-server", "单元 hysteria-residential"] {
            assert_eq!(by(n).verdict, Verdict::Skip, "{n}");
            assert!(by(n).detail.contains("待证书"), "{}", by(n).detail);
        }
        assert_eq!(by("关键端口").verdict, Verdict::Skip);
        assert!(
            by("关键端口").detail.contains("10000/udp")
                && by("关键端口").detail.contains("40000/udp")
                && by("关键端口").detail.contains("待证书"),
            "{}",
            by("关键端口").detail
        );
        assert_eq!(by("HY2 回环鉴权").verdict, Verdict::Skip);
        // 住宅那两个回环面也没绑：这一行是 FAIL 级，判 SKIP 是「首装还没签到证书时
        // `bui install` 不退 2」的唯一保险
        assert_eq!(by("住宅管理面端口").verdict, Verdict::Skip);
        assert!(
            by("住宅管理面端口").detail.contains("待证书"),
            "{}",
            by("住宅管理面端口").detail
        );
        assert_eq!(by("单元 caddy").verdict, Verdict::Pass, "caddy 不等证书");
        assert_eq!(by("xray run -test").verdict, Verdict::Pass);
        assert_eq!(failures(&rows), 0, "{}", table(&rows));
        // 证书没到就别去打回环（凭据都不用写）
        assert!(
            !h.ops().iter().any(|o| o.starts_with("run:sh")),
            "{:?}",
            h.ops()
        );
    }

    /// 证书在位时该 FAIL 的还得 FAIL：别把「等证书」变成万能免罪牌。
    #[test]
    fn units_still_fail_once_the_certificate_is_there() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        let state = crate::testutil::sample_state();
        let h = healthy(&p, &state);
        h.with(|i| {
            i.units_active.remove("hysteria-server.service");
            i.listening.insert(Proto::Udp, Default::default());
        });
        let rows = no_wait(&h, &p, &state, &[]);
        let by = |n: &str| rows.iter().find(|c| c.name == n).unwrap().clone();
        assert_eq!(by("单元 hysteria-server").verdict, Verdict::Fail);
        assert_eq!(by("关键端口").verdict, Verdict::Fail);
        // 单元 + 端口两项；回环探测的 scripted 仍回 200（假机器上单元起没起与脚本无关）
        assert_eq!(failures(&rows), 2, "{}", table(&rows));
    }

    /// 迁移过凭据池的健康机器：4.1 新增的五行全 PASS，凭据一个字都不进流水。
    #[test]
    fn a_healthy_machine_passes_the_new_residential_rows() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        let mut state = crate::testutil::sample_state();
        bui_schema::hy2pool::migrate(&mut state, time::OffsetDateTime::now_utc());
        let secret = bui_schema::hy2pool::cred_of(&state.users[0], &state.residential)
            .unwrap()
            .secret
            .clone();
        let h = healthy(&p, &state);
        let rows = no_wait(&h, &p, &state, &[]);
        for name in [
            "nft 表 inet bui",
            "端口 1 未被监听",
            "住宅 HY2 凭据池",
            "住宅管理面端口",
            "HY2 住宅回环鉴权",
        ] {
            let r = rows
                .iter()
                .find(|r| r.name == name)
                .unwrap_or_else(|| panic!("缺少检查项 {name}"));
            assert_eq!(r.verdict.label(), "PASS", "{name}: {}", r.detail);
        }
        let by = |n: &str| rows.iter().find(|c| c.name == n).unwrap().clone();
        assert!(by("nft 表 inet bui").detail.contains("41000-50000"));
        assert!(
            by("住宅 HY2 凭据池").detail.contains("已用 1 / 空闲 31"),
            "{}",
            by("住宅 HY2 凭据池").detail
        );
        assert_eq!(failures(&rows), 0, "{}", table(&rows));
        // 探测配置里有凭据 ⇒ 只能落 0600 文件、用完即删，argv 与流水里一个字都没有
        let cfg = crate::paths::verify_dir(&p).join("selfcheck-hy2-resi.yaml");
        let sh = crate::paths::verify_dir(&p).join("selfcheck-hy2-resi.sh");
        let ops = h.ops();
        assert!(
            ops.contains(&format!("write:{}:600", cfg.display())),
            "{ops:?}"
        );
        assert!(
            ops.contains(&format!("remove:{}", cfg.display()))
                && ops.contains(&format!("remove:{}", sh.display())),
            "{ops:?}"
        );
        assert_eq!(h.mode(&cfg.display().to_string()), None, "临时配置必须删掉");
        for op in &ops {
            assert!(!op.contains(&secret), "凭据进了命令行/流水：{op}");
        }
    }

    /// 端口 1 被占 ⇒ FAIL：`deny` 出站指向 `127.0.0.1:1`，那个前提没了，
    /// 被封 / 到期的用户反而能经它出网。
    #[test]
    fn something_listening_on_port_one_fails_the_check() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        let state = crate::testutil::sample_state();
        let h = healthy(&p, &state);
        h.with(|i| {
            i.listening.get_mut(&Proto::Tcp).unwrap().insert(1);
        });
        let rows = no_wait(&h, &p, &state, &[]);
        let r = rows.iter().find(|r| r.name == "端口 1 未被监听").unwrap();
        assert_eq!(r.verdict, Verdict::Fail);
        assert!(r.detail.contains("deny"), "{}", r.detail);
        // UDP 那一侧同样不许有人听
        let h2 = healthy(&p, &state);
        h2.with(|i| {
            i.listening.get_mut(&Proto::Udp).unwrap().insert(1);
        });
        let rows = no_wait(&h2, &p, &state, &[]);
        assert_eq!(
            rows.iter()
                .find(|r| r.name == "端口 1 未被监听")
                .unwrap()
                .verdict,
            Verdict::Fail
        );
    }

    /// 住宅回环探测带整段跳跃（验 nft 的 **output** 链），用的是凭据池里那条
    /// `{name}:{secret}`；证书没到时 SKIP 而不是 FAIL（住宅内核此时根本没起来）。
    #[test]
    fn the_residential_probe_carries_mport_and_skips_before_the_first_certificate() {
        let mut state = crate::testutil::sample_state();
        bui_schema::hy2pool::migrate(&mut state, time::OffsetDateTime::now_utc());
        let c = bui_schema::hy2pool::cred_of(&state.users[0], &state.residential).unwrap();
        let y = hy2_resi_client_yaml(&state, &c.name, &c.secret);
        assert!(y.contains("server: 127.0.0.1:40000"), "{y}");
        assert!(y.contains(&format!("auth: {}:{}", c.name, c.secret)), "{y}");
        assert!(
            y.contains("41000-50000"),
            "带 mport 才验得到 output 链：{y}"
        );
        assert!(
            y.contains(&format!("listen: 127.0.0.1:{PROBE_SOCKS_PORT_RESI}")),
            "socks 口与直连那条不同号：{y}"
        );
        assert!(!y.contains("obfs"), "sample 没开 obfs：{y}");

        // 证书未到 ⇒ SKIP，一条 sh 都不跑（凭据都不用写）
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        let h = healthy(&p, &state);
        h.with(|i| {
            i.files.remove(&p.certs_dir.join("fullchain.pem"));
        });
        let rows = no_wait(&h, &p, &state, &[]);
        let r = rows.iter().find(|r| r.name == "HY2 住宅回环鉴权").unwrap();
        assert_eq!(r.verdict, Verdict::Skip);
        assert!(r.detail.contains("待证书"), "{}", r.detail);
        assert!(
            !h.ops().iter().any(|o| o.starts_with("run:sh")),
            "{:?}",
            h.ops()
        );
        assert_eq!(failures(&rows), 0, "{}", table(&rows));
    }

    /// 住宅回环打不通（000 = 客户端连不上）⇒ FAIL，并指向 output 链与住宅单元的日志
    /// ——这一行红而直连那一行绿，就是「nft 的 output 链没生效」的指纹。
    #[test]
    fn a_residential_probe_that_cannot_connect_fails_with_the_nft_hint() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        let mut state = crate::testutil::sample_state();
        bui_schema::hy2pool::migrate(&mut state, time::OffsetDateTime::now_utc());
        let h = healthy(&p, &state);
        h.with(|i| {
            // 住宅那条探测脚本回 000（配置名里带 -resi，键按它前缀匹配）
            let sh = crate::paths::verify_dir(&p).join("selfcheck-hy2-resi.sh");
            i.scripted.insert(
                0,
                (format!("sh {}", sh.display()), CmdOut::success("000\n")),
            );
        });
        let rows = no_wait(&h, &p, &state, &[]);
        let by = |n: &str| rows.iter().find(|c| c.name == n).unwrap().clone();
        assert_eq!(by("HY2 住宅回环鉴权").verdict, Verdict::Fail);
        assert!(
            by("HY2 住宅回环鉴权").detail.contains("000"),
            "{}",
            by("HY2 住宅回环鉴权").detail
        );
        assert!(
            by("HY2 住宅回环鉴权").detail.contains("output"),
            "要指向 nft 的 output 链：{}",
            by("HY2 住宅回环鉴权").detail
        );
        assert_eq!(
            by("HY2 回环鉴权").verdict,
            Verdict::Pass,
            "直连那条不受影响"
        );
        assert_eq!(failures(&rows), 1, "{}", table(&rows));
    }

    /// 缺 `nft` ⇒ 那一行直接 FAIL，正文按真实量级写：带 `mport` 的客户端只往跳跃段发、
    /// 从不发 `:40000`，所以那不是「跳跃失效」而是住宅全断。
    #[test]
    fn a_missing_nft_binary_fails_the_nft_row() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        let state = crate::testutil::sample_state();
        let h = healthy(&p, &state);
        h.with(|i| {
            i.which.remove("nft");
        });
        let rows = no_wait(&h, &p, &state, &[]);
        let r = rows.iter().find(|r| r.name == "nft 表 inet bui").unwrap();
        assert_eq!(r.verdict, Verdict::Fail);
        assert!(r.detail.contains("缺少 nft 二进制"), "{}", r.detail);
        assert!(r.detail.contains("住宅 HY2 等于全断"), "{}", r.detail);
        // 缺二进制时一条 nft 命令都不发
        assert!(
            !h.ops().iter().any(|o| o.starts_with("run:nft")),
            "{:?}",
            h.ops()
        );
    }

    /// 表不在、或规则被人改过（少了 output 链、兼容段被手删）⇒ FAIL 并给出重放命令；
    /// 判据是规范化后的规则条数，`nft` 回显里的 counter 与 `priority dstnat` 不算不符。
    #[test]
    fn a_flushed_or_half_rewritten_table_fails_the_nft_row() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        let state = crate::testutil::sample_state();
        for (listing, want) in [
            (
                CmdOut::failure(1, "Error: No such file or directory\n"),
                "不存在",
            ),
            // 只剩 prerouting（output 链被人删了）：本机带 mport 打自己就不通了
            (
                CmdOut::success(
                    "table inet bui {\n\tchain prerouting {\n\t\tudp dport 41000-50000 counter \
                     packets 0 bytes 0 redirect to :40000\n\t\tudp dport 40001-40007 counter \
                     packets 0 bytes 0 redirect to :40000\n\t}\n}\n",
                ),
                "规则不符",
            ),
            // **四条都挂在 prerouting 上**（有人把 output 那两条粘错了链）：条数对、端口对、
            // 目标对，只有链不对 —— 双 hook 是硬要求，这一行只比条数就会把它判成 PASS
            (
                CmdOut::success(
                    &nft_listing(&state).replace("chain output {", "chain prerouting {"),
                ),
                "规则不符",
            ),
        ] {
            let h = healthy(&p, &state);
            h.with(|i| {
                i.scripted
                    .insert(0, ("nft list table inet bui".into(), listing.clone()));
            });
            let rows = no_wait(&h, &p, &state, &[]);
            let r = rows.iter().find(|r| r.name == "nft 表 inet bui").unwrap();
            assert_eq!(r.verdict, Verdict::Fail, "{}", r.detail);
            assert!(r.detail.contains(want), "{}", r.detail);
            assert!(
                r.detail.contains("bui nft"),
                "要给出可操作的下一步：{}",
                r.detail
            );
        }
    }

    /// 住宅那两个回环面（Clash API 9092 / v2ray_api 10086）没在听 ⇒ FAIL：计量与门位
    /// 会双双打空，而单元本身照样 active，只有这一行看得出来。
    #[test]
    fn the_residential_control_plane_ports_must_be_listening() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        let state = crate::testutil::sample_state();
        for gone in [
            bui_schema::render::hy2_singbox::HY2_RESI_CLASH_API,
            bui_schema::render::hy2_singbox::HY2_RESI_V2RAY_API,
        ] {
            let h = healthy(&p, &state);
            h.with(|i| {
                i.listening
                    .get_mut(&Proto::Tcp)
                    .unwrap()
                    .remove(&api_port(gone).unwrap());
            });
            let rows = no_wait(&h, &p, &state, &[]);
            let r = rows.iter().find(|r| r.name == "住宅管理面端口").unwrap();
            assert_eq!(r.verdict, Verdict::Fail, "{gone}: {}", r.detail);
            assert!(r.detail.contains(gone), "要点名是哪个面：{}", r.detail);
        }
        // 端口只从渲染器那两个常量取，自检里不许有第二份字面量
        assert_eq!(
            api_port(bui_schema::render::hy2_singbox::HY2_RESI_CLASH_API),
            Some(9092)
        );
        assert_eq!(
            api_port(bui_schema::render::hy2_singbox::HY2_RESI_V2RAY_API),
            Some(10086)
        );
    }

    /// 住宅回环探测只许挑「门开着」的人：首个持凭据用户已到期 / 超量 / 被撤了住宅
    /// hysteria2 权益时，他的门在 `deny` 上、拿他的凭据打过去必然 000。挑不出人就 SKIP
    /// 并说清原因（不是 FAIL —— 那会让 `bui install` 退 2，还把运维指向 nft 的 output 链
    /// 这个错方向）。
    #[test]
    fn the_residential_probe_skips_users_whose_gate_is_on_deny() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        // 四种「门在 deny 上」各一格：到期 / 超总量 / 超月量 / 撤了住宅 hysteria2 权益
        // （停用那一格由 `is_blocked` 的 Disabled 覆盖，这里连它一起摆）
        type Broke = fn(&mut bui_schema::model::User);
        let brokers: [(&str, Broke); 5] = [
            ("停用", |u| u.disabled = true),
            ("到期", |u| {
                u.entitlements.expires_at = Some("2026-09-10T00:00:00Z".into())
            }),
            ("超总量", |u| {
                u.entitlements.traffic_limit.total_bytes = Some(100);
                u.usage.total_bytes = 200;
            }),
            ("超月量", |u| {
                u.entitlements.traffic_limit.monthly_bytes = Some(100);
                u.usage.month_key = "2026-09".into();
                u.usage.monthly_bytes = 200;
            }),
            ("撤了住宅 hysteria2 权益", |u| {
                u.entitlements.protocols = vec![bui_schema::model::Protocol::Reality]
            }),
        ];
        for (why, break_it) in brokers {
            let mut state = crate::testutil::sample_state();
            bui_schema::hy2pool::migrate(&mut state, time::OffsetDateTime::now_utc());
            assert!(
                state.users[0].credentials.hy2_resi_cred.is_some(),
                "夹具前提：这个人手里有凭据"
            );
            break_it(&mut state.users[0]);
            let h = healthy(&p, &state);
            let rows = no_wait(&h, &p, &state, &[]);
            let r = rows.iter().find(|r| r.name == "HY2 住宅回环鉴权").unwrap();
            assert_eq!(r.verdict, Verdict::Skip, "{why}：{}", r.detail);
            assert!(r.detail.contains("deny"), "{why}：{}", r.detail);
            assert_eq!(failures(&rows), 0, "{why}：{}", table(&rows));
            // 探测都不该跑（凭据也不写进临时文件）
            let sh = crate::paths::verify_dir(&p).join("selfcheck-hy2-resi.sh");
            assert!(
                !h.ops()
                    .iter()
                    .any(|o| o == &format!("run:sh {}", sh.display())),
                "{why}：{:?}",
                h.ops()
            );
        }

        // 门开着的那个人照旧被挑中 ⇒ PASS（别把过滤写成「谁都挑不出来」）
        let mut state = crate::testutil::sample_state();
        bui_schema::hy2pool::migrate(&mut state, time::OffsetDateTime::now_utc());
        let rows = no_wait(&healthy(&p, &state), &p, &state, &[]);
        assert_eq!(
            rows.iter()
                .find(|r| r.name == "HY2 住宅回环鉴权")
                .unwrap()
                .verdict,
            Verdict::Pass
        );
    }

    /// 住宅那两个回环控制面在**有界等待**的就绪条件里：首装时 sing-box 先绑 `:40000`、
    /// experimental 的两个面稍后才起，采样落进那个窗口不许把一台健康机器判红。
    #[test]
    fn the_residential_control_plane_ports_are_waited_for_before_being_failed() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        let state = crate::testutil::sample_state();
        let clash = api_port(bui_schema::render::hy2_singbox::HY2_RESI_CLASH_API).unwrap();

        // ① 晚绑几轮：等到了就 PASS，一项都不 FAIL
        let h = healthy(&p, &state);
        h.with(|i| {
            i.listening.get_mut(&Proto::Tcp).unwrap().remove(&clash);
        });
        let rounds = std::cell::Cell::new(0u32);
        let rows = run_with_clock(&h, &p, &state, &[], Wait::default(), &|dur| {
            assert_eq!(dur, Duration::from_secs(3), "每 3s 一轮");
            rounds.set(rounds.get() + 1);
            if rounds.get() == 3 {
                h.with(|i| {
                    i.listening.get_mut(&Proto::Tcp).unwrap().insert(clash);
                });
            }
        });
        assert_eq!(rounds.get(), 3, "等到就绪就不再睡");
        assert_eq!(
            rows.iter()
                .find(|r| r.name == "住宅管理面端口")
                .unwrap()
                .verdict,
            Verdict::Pass
        );
        assert_eq!(failures(&rows), 0, "{}", table(&rows));

        // ② 一直不绑：等满预算才 FAIL（等待有界，不在最后一屏无限期干等）
        let h2 = healthy(&p, &state);
        h2.with(|i| {
            i.listening.get_mut(&Proto::Tcp).unwrap().remove(&clash);
        });
        let slept = std::cell::Cell::new(Duration::ZERO);
        let rows = run_with_clock(&h2, &p, &state, &[], Wait::default(), &|d| {
            slept.set(slept.get() + d)
        });
        assert_eq!(slept.get(), Duration::from_secs(120), "等待有界：最多 120s");
        let r = rows.iter().find(|r| r.name == "住宅管理面端口").unwrap();
        assert_eq!(r.verdict, Verdict::Fail, "{}", r.detail);
        assert!(
            r.detail
                .contains(bui_schema::render::hy2_singbox::HY2_RESI_CLASH_API),
            "{}",
            r.detail
        );
        // 这两个端口只进等待条件，不进「关键端口」那一行
        assert!(!listen_ports(&state).contains(&(Proto::Tcp, clash)));
        assert_eq!(
            rows.iter().find(|r| r.name == "关键端口").unwrap().verdict,
            Verdict::Pass
        );
    }

    /// 凭据池耗尽 ⇒ FAIL（新住宅用户分不到门位）；池还没建起来 ⇒ SKIP（不是「装坏了」）。
    #[test]
    fn an_exhausted_credential_pool_fails_while_an_empty_one_skips() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        let mut state = crate::testutil::sample_state();
        let h = healthy(&p, &state);
        let rows = no_wait(&h, &p, &state, &[]);
        let r = rows.iter().find(|r| r.name == "住宅 HY2 凭据池").unwrap();
        assert_eq!(r.verdict, Verdict::Skip, "{}", r.detail);

        // 池里只有一条、正好被那个用户占着 ⇒ 空闲 0
        bui_schema::hy2pool::migrate(&mut state, time::OffsetDateTime::now_utc());
        let held = state.users[0].credentials.hy2_resi_cred.clone().unwrap();
        state.residential.hy2_pool.creds.retain(|c| c.id == held);
        state.residential.hy2_pool.generation = 3;
        let rows = no_wait(&healthy(&p, &state), &p, &state, &[]);
        let r = rows.iter().find(|r| r.name == "住宅 HY2 凭据池").unwrap();
        assert_eq!(r.verdict, Verdict::Fail, "{}", r.detail);
        assert!(
            r.detail.contains("已用 1 / 空闲 0 / 第 3 代"),
            "{}",
            r.detail
        );
    }

    #[test]
    fn missing_kernels_are_skipped_not_failed() {
        // 离线装机 / manifest 拉不到：校验器不在不该把自检染红（内核闸门另有一条）
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        let state = crate::testutil::sample_state();
        let h = healthy(&p, &state);
        h.with(|i| {
            i.files.remove(&p.bin_dir.join("caddy"));
        });
        let rows = no_wait(&h, &p, &state, &[]);
        let c = rows.iter().find(|c| c.name == "caddy validate").unwrap();
        assert_eq!(c.verdict, Verdict::Skip);
        assert_eq!(failures(&rows), 0);
    }
}
