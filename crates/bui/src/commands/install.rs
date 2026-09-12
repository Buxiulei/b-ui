//! `bui install`：收集参数 → 装内核 → 写 `state.json` →（可选）卸载 v3 → 写初版
//! `auth-snapshot.json` → 跑一次完整对账（spec §7）。
//!
//! 幂等是硬要求：`state.json` 已存在就不覆盖，只对账；manifest 缓存与鉴权快照都「内容相同不写」，
//! 于是第二次 `bui install` 除 `.verify/` 的候选文件外零写入。
//!
//! **所有阻塞调用都在 `tokio::task::spawn_blocking` 里**：`reqwest::blocking`（manifest 与内核
//! 下载）在 async 上下文里会 panic，[`Host`] 的接口本身也全是同步的。

use crate::api::EventBus;
use crate::kernels::{Fetcher, HttpFetcher, KernelInstaller, Manifest};
use crate::reconcile::apply::BinaryInstaller;
use crate::reconcile::DaemonCtx;
use crate::state::runtime::Runtime;
use crate::state::store::Store;
use crate::sys::Host;
use bui_schema::model::{
    Admin, NodeParams, Obfs, Ports, Reality, Residential, State, SystemSettings, Versions,
    SCHEMA_VERSION,
};
use bui_schema::paths::Paths;
use serde::Deserialize;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 装机要问的全部问题（住宅上游归 P3，这里没有那一问）。
#[derive(Debug, Clone, PartialEq)]
pub struct Answers {
    pub domain: String,
    pub admin_password: String,
    pub node_name: String,
    pub public_ip: String,
    pub ports: Ports,
    pub masquerade: String,
}

impl Answers {
    /// 端口布局与 v3 一致（`server/core.sh`），伪装域用合成值 `www.bing.com:443`。
    /// `admin_password` 是**空串**：密码只由 `--admin-password-stdin` 或随机生成填。
    pub fn defaults(hostname: &str, public_ip: &str) -> Answers {
        Answers {
            domain: String::new(),
            admin_password: String::new(),
            node_name: hostname.to_string(),
            public_ip: public_ip.to_string(),
            ports: Ports {
                hy2: 10000,
                hy2_hop: Some((20000, 30000)),
                hy2_resi: 40000,
                hy2_resi_hop: (41000, 50000),
                reality_direct: 10001,
                reality_resi: 10002,
                admin: 8080,
            },
            masquerade: "www.bing.com:443".into(),
        }
    }
}

/// `--non-interactive --answers <file>` 的文件形状（总纲 C5 只给了参数名，格式在这里定死，
/// 因为 P5 的 `scripts/ops/v3-cutover.sh` 要照它生成）。所有键都可省，省掉的沿用
/// [`Answers::defaults`]：
///
/// ```jsonc
/// { "domain": "example.com", "node_name": "node-a", "public_ip": "203.0.113.10",
///   "masquerade": "www.bing.com:443",
///   "ports": { "hy2": 10000, "hy2_hop": [20000, 30000], "hy2_resi": 40000,
///              "hy2_resi_hop": [41000, 50000], "reality_direct": 10001,
///              "reality_resi": 10002, "admin": 8080 } }
/// ```
///
/// `ports` 给了就必须**给全**（`bui_schema::model::Ports` 除 `hy2_hop` 外没有 serde 默认值）。
/// **文件里没有管理员密码**：它会留在磁盘上、还会进 P5 脚本的 git 历史，所以密码只能经
/// `--admin-password-stdin` 从 stdin 读；两者都没给时生成随机密码并打印一次（不写日志）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AnswersFile {
    pub domain: Option<String>,
    pub node_name: Option<String>,
    pub public_ip: Option<String>,
    pub masquerade: Option<String>,
    pub ports: Option<Ports>,
}

pub fn load_answers(path: &Path, defaults: Answers) -> anyhow::Result<Answers> {
    let bytes =
        std::fs::read(path).map_err(|e| anyhow::anyhow!("读取 {} 失败：{e}", path.display()))?;
    let f: AnswersFile = serde_json::from_slice(&bytes)
        .map_err(|e| anyhow::anyhow!("{} 不是合法的 answers JSON：{e}", path.display()))?;
    Ok(Answers {
        domain: f.domain.unwrap_or(defaults.domain),
        // 密码永远不从文件里来（会落盘、会进 P5 脚本的 git 历史）：只认 --admin-password-stdin
        admin_password: defaults.admin_password,
        node_name: f.node_name.unwrap_or(defaults.node_name),
        public_ip: f.public_ip.unwrap_or(defaults.public_ip),
        ports: f.ports.unwrap_or(defaults.ports),
        masquerade: f.masquerade.unwrap_or(defaults.masquerade),
    })
}

pub struct InstallOpts {
    pub domain: Option<String>,
    pub port: Option<u16>,
    pub admin_password_stdin: bool,
    pub import_v3: Option<PathBuf>,
    pub non_interactive: bool,
    pub answers: Option<PathBuf>,
    pub yes: bool,
    /// 第 9 步探测守护进程用的 unix socket。**必须由调用方传入**（`main.rs` 传
    /// `PathBuf::from(crate::paths::SOCKET_PATH)`），不许在函数体里读常量：install 的测试都会
    /// 走到第 9 步，硬编码 `/run/b-ui.sock` 会让它们在跑着 v4 守护进程的机器上真的
    /// `connect()` 并真发一次 `POST /api/reconcile`，违反「单元测试不碰真实系统」。
    pub socket: PathBuf,
}

impl InstallOpts {
    /// 一个问题都不问。
    pub fn quiet(&self) -> bool {
        self.yes || self.non_interactive
    }
}

/// `xray x25519` 的两种输出格式都要认（26.x 的 `PrivateKey:` / `Password (PublicKey):`，
/// 老版本的 `Private key:` / `Public key:`）。
pub fn parse_x25519(stdout: &str) -> Option<(String, String)> {
    let grab = |prefixes: [&str; 2]| -> Option<String> {
        stdout.lines().find_map(|l| {
            let l = l.trim();
            prefixes
                .iter()
                .find_map(|p| l.strip_prefix(p))
                .map(|v| v.trim().to_string())
        })
    };
    let private = grab(["PrivateKey:", "Private key:"])?;
    let public = grab(["Password (PublicKey):", "Public key:"])?;
    (!private.is_empty() && !public.is_empty()).then_some((private, public))
}

pub fn random_hex(bytes: usize) -> String {
    use rand::RngCore;
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

/// REALITY 的 shortId：8 字节随机 → 16 位 hex。
pub fn random_short_id() -> String {
    random_hex(8)
}

pub fn build_state(
    answers: &Answers,
    reality: Reality,
    versions: Versions,
) -> anyhow::Result<State> {
    let mut reality = reality;
    // dest 与 serverNames 必须同源：用户在交互里改了伪装域名而 server_names 还写死 www.bing.com 时，
    // xray 的 REALITY 握手直接失败。`Reality::sni()` 就是「dest 去掉端口」的语义。
    reality.dest = answers.masquerade.clone();
    reality.server_names = vec![reality.sni().to_string()];
    Ok(State {
        schema_version: SCHEMA_VERSION,
        node: NodeParams {
            id: uuid::Uuid::new_v4(),
            name: answers.node_name.clone(),
            domain: answers.domain.clone(),
            public_ip: answers.public_ip.clone(),
            ports: answers.ports.clone(),
            reality,
            obfs: Obfs::default(),
        },
        admin: Admin {
            password_hash: crate::api::auth::hash_password(&answers.admin_password)?,
            jwt_secret: random_hex(32),
        },
        users: Vec::new(),
        residential: Residential::default(),
        system: SystemSettings::default(),
        versions,
        catalog: Vec::new(),
    })
}

/// hysteria 鉴权钩子读的快照（spec §3.2）。形状**逐字照总纲 C5**：
/// `{"schema":1,"users":{"<username>":{user_id,hy2_password,expires_at,blocked}}}`；
/// 没有用户时 `users` 是空对象。P2 的钩子与快照重写按同一形状。
pub fn auth_snapshot(state: &State) -> serde_json::Value {
    // 写成「顶层直接以用户名为键」的扁平对象，P2 的钩子就读不到用户，
    // `bui install --import-v3` 之后导入的用户建连一律 fail-closed。
    let mut users = serde_json::Map::new();
    for u in &state.users {
        users.insert(
            u.username.clone(),
            serde_json::json!({
                "user_id": u.user_id.to_string(),
                "hy2_password": u.credentials.hy2_password,
                "expires_at": u.entitlements.expires_at,
                "blocked": u.disabled,
            }),
        );
    }
    serde_json::json!({ "schema": 1, "users": serde_json::Value::Object(users) })
}

/// 只在内容变化时写（0600）；否则第二次 `bui install` 就不是零写入了。
fn write_auth_snapshot(host: &dyn Host, paths: &Paths, bytes: &[u8]) -> anyhow::Result<()> {
    let path = crate::paths::auth_snapshot_file(paths);
    if host.read_file(&path)?.as_deref() == Some(bytes) {
        return Ok(());
    }
    host.write_file(&path, bytes, 0o600)?;
    Ok(())
}

/// `<bin>/xray` 优先，其次 PATH 上的 `xray`（v3 机器上是现成的），都没有则 `None`。
/// 与 apply 的校验器查找同一套规则。
fn xray_program(host: &dyn Host, paths: &Paths) -> Option<String> {
    let bundled = paths.bin_dir.join("xray");
    if host.read_file(&bundled).ok().flatten().is_some() {
        return Some(bundled.display().to_string());
    }
    host.which("xray").then(|| "xray".to_string())
}

/// `xray x25519` 解析失败时**唯一**可以进错误信息的上下文：行数 + 首行前 40 字符（先脱敏再截）。
/// 那段输出里就有 REALITY 私钥，整段回传会让它进用户的终端记录与 journal。
fn x25519_parse_hint(text: &str) -> String {
    let head: String =
        crate::redact::url_credentials(text.lines().next().unwrap_or_default().trim())
            .chars()
            .take(40)
            .collect();
    format!("共 {} 行，首行前 40 字符：{head}", text.lines().count())
}

/// 生成 REALITY 密钥；`dest` 与 `server_names` 留空占位，由 [`build_state`] 按
/// `answers.masquerade` 一起填。
fn generate_reality(host: &dyn Host, paths: &Paths) -> anyhow::Result<Reality> {
    let Some(prog) = xray_program(host, paths) else {
        anyhow::bail!(
            "未找到 xray 二进制（{} 与 PATH 都没有）：manifest 拉取失败时请设 {} 指向可用的 manifest，或先把 xray 放进 {}/",
            paths.bin_dir.join("xray").display(),
            crate::kernels::MANIFEST_URL_ENV,
            paths.bin_dir.display()
        );
    };
    let out = host.run(&prog, &["x25519"])?;
    let text = if out.stdout.is_empty() {
        &out.stderr
    } else {
        &out.stdout
    };
    let Some((private_key, public_key)) = parse_x25519(text) else {
        anyhow::bail!(
            "解析 `xray x25519` 的输出失败（{}）；请核对该版本 xray 的输出格式",
            x25519_parse_hint(text)
        );
    };
    Ok(Reality {
        private_key,
        public_key,
        short_ids: vec![random_short_id()],
        dest: String::new(),
        server_names: Vec::new(),
    })
}

/// 已装内核的版本表（探不到的留空串）。
fn versions_from_disk(host: &dyn Host, paths: &Paths) -> Versions {
    let m = crate::kernels::installed_versions(host, &paths.bin_dir);
    let g = |k: &str| m.get(k).cloned().unwrap_or_default();
    Versions {
        bui: env!("CARGO_PKG_VERSION").to_string(),
        hysteria: g("hysteria"),
        xray: g("xray"),
        sing_box: g("sing-box"),
        caddy: g("caddy"),
        client_sing_box: g("sing-box"),
    }
}

/// 第 3 + 4 步：拉 manifest → 缓存（内容相同不写）→ 把四个内核装到 `bin/`（版本一致则跳过）。
/// manifest 拉不到只警告并返回 `None`（离线装机继续，对账跳过 `Binary`）。
fn fetch_and_install_kernels(
    host: &dyn Host,
    fetcher: &dyn Fetcher,
    paths: &Paths,
    manifest_url: &str,
) -> Option<Manifest> {
    let manifest = match Manifest::from_url(fetcher, manifest_url) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(
                error = %e,
                url = %crate::redact::url_credentials(manifest_url),
                env = crate::kernels::MANIFEST_URL_ENV,
                "拉取 manifest 失败，跳过内核安装（可用该环境变量覆盖地址）"
            );
            return None;
        }
    };
    if let Ok(bytes) = serde_json::to_vec_pretty(&manifest) {
        let path = crate::paths::manifest_file(paths);
        // 内容相同就不写：`a_second_install_changes_nothing` 要求第二次 install 零写入
        if host.read_file(&path).ok().flatten().as_deref() != Some(bytes.as_slice()) {
            if let Err(e) = host.write_file(&path, &bytes, 0o644) {
                tracing::warn!(error = %e, "缓存 manifest 失败");
            }
        }
    }
    let arch = host.arch().unwrap_or_default();
    let installed = crate::kernels::installed_versions(host, &paths.bin_dir);
    let installer = KernelInstaller { fetcher, host };
    for name in crate::kernels::KERNELS {
        let (version, asset) = match manifest.kernel_asset(name, &arch) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(kernel = name, error = %e, "manifest 里没有该内核，跳过");
                continue;
            }
        };
        if installed.get(name).map(String::as_str) == Some(version) {
            continue;
        }
        if let Err(e) = installer.install(
            name,
            version,
            &asset.sha256,
            &asset.url,
            &paths.bin_dir.join(name),
        ) {
            tracing::warn!(kernel = name, error = %e, "内核安装失败");
        }
    }
    // 第 4 步：不齐就打印怎么补，但不中断（v3 机器上内核可能已在 PATH 里）
    let have = crate::kernels::installed_versions(host, &paths.bin_dir);
    let missing: Vec<&str> = crate::kernels::KERNELS
        .iter()
        .copied()
        .filter(|k| !have.contains_key(*k))
        .collect();
    if !missing.is_empty() {
        println!(
            "缺少内核：{}；请设 {} 指向可用的 manifest，或把二进制放进 {}/ 后重跑 bui install",
            missing.join("、"),
            crate::kernels::MANIFEST_URL_ENV,
            paths.bin_dir.display()
        );
    }
    Some(manifest)
}

/// 问一个问题；回车即接受默认值。
fn ask(prompt: &str, default: &str) -> String {
    use std::io::Write;
    if default.is_empty() {
        print!("{prompt}: ");
    } else {
        print!("{prompt} [{default}]: ");
    }
    let _ = std::io::stdout().flush();
    let mut buf = String::new();
    if std::io::stdin().read_line(&mut buf).is_err() {
        return default.to_string();
    }
    let v = buf.trim();
    if v.is_empty() {
        default.to_string()
    } else {
        v.to_string()
    }
}

/// 已装机时的 `Answers`：事实全部复用 `state.json`，密码留空。
/// （[`run_with`] 的非全新路径只对账，`answers` 一概不看；这里给出的是「与现状一致」的形状，
/// 而不是一组会误导人的新值。）
fn answers_from_state(state: &State) -> Answers {
    Answers {
        domain: state.node.domain.clone(),
        admin_password: String::new(),
        node_name: state.node.name.clone(),
        public_ip: state.node.public_ip.clone(),
        ports: state.node.ports.clone(),
        masquerade: state.node.reality.dest.clone(),
    }
}

/// 收集 `Answers`。返回的第二项是 [`run`] 要打到 stdout 的那一行提示（随机管理员密码**只**在
/// 这里出现一次），`None` = 没有要打的。
///
/// `installed` 是已装机时读到的期望态：非 `None` 时既不探公网 IP（省一次 5s 的 curl，离线机器
/// 上也不再刷提示），也不生成随机密码——活机器上重跑 `bui install` 打印「已生成随机管理员密码」
/// 会让人以为面板密码被换了，而 `state.json` 里的哈希其实一个字没动。
async fn collect_answers(
    opts: &InstallOpts,
    installed: Option<&State>,
    host: Arc<dyn Host>,
) -> anyhow::Result<(Answers, Option<String>)> {
    if let Some(state) = installed {
        return Ok((answers_from_state(state), None));
    }
    let (hostname, probed_ip) = {
        let h = host.clone();
        tokio::task::spawn_blocking(move || {
            let hostname = h.hostname().unwrap_or_default();
            let ip = h
                .run("curl", &["-sS", "--max-time", "5", "https://api.ipify.org"])
                .ok()
                .filter(|o| o.ok())
                .map(|o| o.stdout.trim().to_string())
                .unwrap_or_default();
            (hostname, ip)
        })
        .await?
    };
    if probed_ip.is_empty() {
        println!("提示：未能探测到公网 IP，可稍后在面板里补填（只影响 relay 的本机 IP 直连例外）");
    }
    let defaults = Answers::defaults(&hostname, &probed_ip);
    let mut answers = match &opts.answers {
        Some(f) => load_answers(f, defaults)?,
        None => defaults,
    };
    if !opts.quiet() && std::io::stdin().is_terminal() {
        answers.domain = ask("面板域名", &answers.domain);
        answers.node_name = ask("节点名", &answers.node_name);
        answers.public_ip = ask("公网 IP", &answers.public_ip);
        answers.masquerade = ask("REALITY 伪装目标", &answers.masquerade);
        answers.ports.hy2 = ask("Hysteria2 直连端口", &answers.ports.hy2.to_string())
            .parse()
            .unwrap_or(answers.ports.hy2);
    }
    // 命令行优先于答案文件与交互
    if let Some(d) = &opts.domain {
        answers.domain = d.clone();
    }
    if let Some(p) = opts.port {
        answers.ports.hy2 = p;
    }
    let notice = if opts.admin_password_stdin {
        let mut buf = String::new();
        std::io::stdin().read_line(&mut buf)?;
        answers.admin_password = buf.trim_end_matches('\n').to_string();
        None
    } else {
        let pw = random_hex(8);
        // Global Constraints：密码不进日志。`secret` 只记长度，不记内容
        tracing::info!(password = %crate::redact::secret(&pw), "已生成随机管理员密码");
        let notice = format!("已生成随机管理员密码：{pw}（只显示这一次，请立刻存好）");
        answers.admin_password = pw;
        Some(notice)
    };
    Ok((answers, notice))
}

/// 面向真实终端的入口：读已装机的期望态 → 收集 `Answers` → 交给 [`run_with`]。
pub async fn run(opts: InstallOpts, paths: Paths, host: Arc<dyn Host>) -> anyhow::Result<()> {
    // 判据与 `run_with` 的 `fresh` 完全同一条（`state.json` 在不在），两边不会分叉
    let state_path = crate::paths::state_file(&paths);
    let installed = if state_path.exists() {
        Some(Store::open(&state_path).await?.read().await)
    } else {
        None
    };
    let (answers, notice) = collect_answers(&opts, installed.as_deref(), host.clone()).await?;
    if let Some(line) = notice {
        println!("{line}");
    }
    let manifest_url = crate::kernels::manifest_url(None, None);
    run_with(
        opts,
        answers,
        manifest_url,
        paths,
        host,
        Arc::new(HttpFetcher::new()),
    )
    .await
}

/// spec §7 的十步。`manifest_url` 由调用方算好（[`crate::kernels::manifest_url`]），
/// 测试直接传固定值，不读进程环境。
pub async fn run_with(
    opts: InstallOpts,
    answers: Answers,
    manifest_url: String,
    paths: Paths,
    host: Arc<dyn Host>,
    fetcher: Arc<dyn Fetcher>,
) -> anyhow::Result<()> {
    let state_path = crate::paths::state_file(&paths);
    let fresh = !state_path.exists();
    // 3 + 4：拉 manifest → 缓存 → 装四个内核（阻塞线程）
    let manifest = {
        let (h, f, p, u) = (
            host.clone(),
            fetcher.clone(),
            paths.clone(),
            manifest_url.clone(),
        );
        tokio::task::spawn_blocking(move || {
            fetch_and_install_kernels(h.as_ref(), f.as_ref(), &p, &u)
        })
        .await?
    };
    if fresh {
        // 5 + 6：生成或导入 state
        let state = match &opts.import_v3 {
            Some(dir) => {
                let report = bui_schema::v3::import(dir)?;
                for w in &report.warnings {
                    println!("导入提示：{w}");
                    tracing::warn!("{w}");
                }
                report.state
            }
            None => {
                let keys = {
                    let (h, p) = (host.clone(), paths.clone());
                    tokio::task::spawn_blocking(move || generate_reality(h.as_ref(), &p)).await??
                };
                let versions = {
                    let (h, p) = (host.clone(), paths.clone());
                    tokio::task::spawn_blocking(move || versions_from_disk(h.as_ref(), &p)).await?
                };
                build_state(&answers, keys, versions)?
            }
        };
        let (domain, admin_port) = (state.node.domain.clone(), state.node.ports.admin);
        Store::create(&state_path, state).await?;
        // 7：先卸 v3（停 b-ui-admin 腾出 :8080、停 v3 timer），再对账
        if opts.import_v3.is_some() {
            let (h, p) = (host.clone(), paths.clone());
            let done = tokio::task::spawn_blocking(move || {
                crate::commands::import_v3::uninstall_v3(h.as_ref(), &p, &domain, admin_port)
            })
            .await?;
            match done {
                Ok(lines) => {
                    for line in lines {
                        println!("{line}");
                    }
                }
                // 外部站点通道的闸门没过（2026-09-13 裁决）：此时**一个破坏性动作都还没做**，
                // 发行版 caddy 照旧在跑。把刚写出的 state.json 删掉再退出——留着它，下次重跑
                // install 会走「已安装」分支，v3 就永远卸不掉了。
                Err(e) => {
                    let _ = std::fs::remove_file(&state_path);
                    return Err(e.context(format!(
                        "已中止 v3 导入并回滚 {}（发行版 caddy 仍在运行）；修好站点配置后重跑 install",
                        state_path.display()
                    )));
                }
            }
        }
    } else {
        println!("已安装（{} 已存在），执行对账", state_path.display());
    }
    let store = Store::open(&state_path).await?;
    let runtime = Runtime::load(crate::paths::runtime_file(&paths));
    let ctx = DaemonCtx {
        store,
        runtime,
        bus: EventBus::new(),
        host: host.clone(),
        paths: paths.clone(),
    };
    // 8：初版 auth-snapshot.json（钩子的输入；内容不变则不写，保证第二次 install 零写入）
    {
        let state = ctx.store.read().await;
        let bytes = serde_json::to_vec_pretty(&auth_snapshot(&state))?;
        let (h, p) = (host.clone(), paths.clone());
        tokio::task::spawn_blocking(move || write_auth_snapshot(h.as_ref(), &p, &bytes)).await??;
    }
    // 9：一次完整对账。守护进程已经在跑（活机器上重跑 install）就交给它，别在本进程里并发再跑
    // 一轮——两边会同时 restart 同一个单元、`.verify/<file>` 候选文件互相覆盖、
    // `runtime.json` 交叉写（与 `serve::reconcile_cli` 同一条口径）。
    let client = crate::ipc::Client::new(&opts.socket);
    let report = if client.available().await {
        let (status, body) = client
            .request(
                "POST",
                "/api/reconcile",
                Some(serde_json::json!({"force": false, "dry_run": false})),
            )
            .await?;
        println!("守护进程已在运行，对账已交给它（HTTP {status}）：{body}");
        serde_json::from_value(body).unwrap_or_default()
    } else {
        let reg = crate::serve::modules(manifest);
        let r = crate::serve::reconcile_from_ctx(&ctx, &reg.modules, fetcher, false, false).await?;
        crate::serve::finish_self_restart(&ctx, &r, false).await;
        r
    };
    for line in report
        .notes
        .iter()
        .chain(report.verify_failures.iter())
        .chain(report.errors.iter())
    {
        println!("{line}");
    }
    // 10
    let state = ctx.store.read().await;
    println!(
        "面板: https://{}/    用户: 见 `b-ui` 菜单",
        state.node.domain
    );
    println!("后续: `b-ui` 进菜单 / `bui status` 看体检 / `bui reconcile` 手动对账");
    if !report.errors.is_empty() || !report.verify_failures.is_empty() {
        anyhow::bail!("对账有失败项，见上面的输出");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernels::Fetcher;
    use crate::sys::{fake::FakeHost, CmdOut, Host};
    use pretty_assertions::assert_eq;
    use std::sync::{Arc, Mutex};

    // 本机实测：xray 26.3.27 的输出
    const X25519: &str = "PrivateKey: CBuMG2F9fOCyzMKCniVKSS6lmXyKRmD9stuXyXeKSF4\nPassword (PublicKey): cTpW46LZoWSn3XlHahzkRh3CMpu-pEQUOk7-seT7W1c\nHash32: 3Vnzmd-rq4njP5IfMf_wYgFrGEuMBpJqgOh-UAdLOUY\n";
    // 老版本 xray 的输出
    const X25519_OLD: &str =
        "Private key: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\nPublic key: bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n";

    #[test]
    fn parses_both_x25519_output_formats() {
        assert_eq!(
            parse_x25519(X25519),
            Some((
                "CBuMG2F9fOCyzMKCniVKSS6lmXyKRmD9stuXyXeKSF4".to_string(),
                "cTpW46LZoWSn3XlHahzkRh3CMpu-pEQUOk7-seT7W1c".to_string()
            ))
        );
        assert_eq!(
            parse_x25519(X25519_OLD),
            Some((
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string()
            ))
        );
        assert_eq!(parse_x25519("nothing useful"), None);
    }

    #[test]
    fn x25519_parse_failure_never_echoes_the_whole_output() {
        // `xray x25519` 的输出里就有私钥：解析失败时把整段回传给用户，私钥会进终端记录与
        // journal（错误一路往上冒到 main 的 tracing::error）。错误只许带行数与首行前 40 字符。
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let xray = paths.bin_dir.join("xray").display().to_string();
        let h = FakeHost::new();
        h.with(|i| {
            i.files
                .insert(paths.bin_dir.join("xray"), (b"ELF".to_vec(), 0o755));
            i.scripted.push((
                format!("{xray} x25519"),
                // 首行是个报错，后面两行照旧带着密钥（真机上 xray 换输出格式时就是这形状）
                CmdOut::success(
                    "unknown flag: x25519 —— 这一行超过四十个字符，尾巴必须被截掉\nPrivateKeyX: CBuMG2F9fOCyzMKCniVKSS6lmXyKRmD9stuXyXeKSF4\nHash32: 3Vnzmd-rq4njP5IfMf_wYgFrGEuMBpJqgOh-UAdLOUY\n",
                ),
            ));
        });
        let err = generate_reality(&h, &paths).unwrap_err().to_string();
        assert!(
            !err.contains("CBuMG2F9") && !err.contains("PrivateKeyX"),
            "整段输出不许回传：{err}"
        );
        assert!(err.contains("3 行"), "要带行数：{err}");
        assert!(err.contains("unknown flag: x25519"), "要带首行开头：{err}");
        // 首行前 40 字符（按字符数，不是字节数：中文不能把 UTF-8 切断）
        let head: String = "unknown flag: x25519 —— 这一行超过四十个字符，尾巴必须被截掉"
            .chars()
            .take(40)
            .collect();
        assert!(err.contains(&head), "{err}");
        assert!(!err.contains("尾巴必须被截掉"), "首行也要截断：{err}");
        // userinfo 形态的凭据经 redact
        assert_eq!(
            x25519_parse_hint("socks5://user:pa:ss@isp.example.net:10007 拒绝连接\n"),
            "共 1 行，首行前 40 字符：socks5://***:***@isp.example.net:10007 拒"
        );
    }

    #[test]
    fn random_values_have_the_right_shape() {
        let a = random_short_id();
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, random_short_id(), "两次生成不应相同");
        assert_eq!(random_hex(32).len(), 64);
    }

    #[test]
    fn default_answers_match_the_v3_port_layout() {
        let a = Answers::defaults("node-a", "203.0.113.10");
        assert_eq!(a.ports.hy2, 10000);
        assert_eq!(a.ports.hy2_hop, Some((20000, 30000)));
        assert_eq!(a.ports.hy2_resi, 40000);
        assert_eq!(a.ports.hy2_resi_hop, (41000, 50000));
        assert_eq!(a.ports.reality_direct, 10001);
        assert_eq!(a.ports.reality_resi, 10002);
        assert_eq!(a.ports.admin, 8080);
        assert_eq!(a.masquerade, "www.bing.com:443");
        assert_eq!(a.node_name, "node-a");
    }

    #[test]
    fn build_state_hashes_the_password_and_fills_the_node() {
        let answers = Answers {
            domain: "example.com".into(),
            admin_password: "test123".into(),
            ..Answers::defaults("node-a", "203.0.113.10")
        };
        let reality = bui_schema::model::Reality {
            private_key: "priv".into(),
            public_key: "pub".into(),
            short_ids: vec![random_short_id()],
            // generate_reality 给的就是空占位，两项都由 build_state 按 answers.masquerade 填
            dest: String::new(),
            server_names: Vec::new(),
        };
        let versions = bui_schema::model::Versions {
            bui: "4.0.0".into(),
            ..Default::default()
        };
        let s = build_state(&answers, reality, versions).unwrap();
        assert_eq!(s.schema_version, bui_schema::model::SCHEMA_VERSION);
        assert_eq!(s.node.domain, "example.com");
        assert_eq!(s.node.name, "node-a");
        assert_eq!(s.node.public_ip, "203.0.113.10");
        assert_eq!(s.node.reality.dest, "www.bing.com:443");
        assert_eq!(
            s.node.reality.server_names,
            vec!["www.bing.com".to_string()],
            "serverNames 必须与 dest 同源，否则用户改了伪装域名 REALITY 握手就失败"
        );
        assert_eq!(s.node.reality.short_ids[0].len(), 16);
        assert!(s.admin.password_hash.starts_with("$argon2id$"));
        assert!(crate::api::auth::verify_password(
            &s.admin.password_hash,
            "test123"
        ));
        assert_eq!(s.admin.jwt_secret.len(), 64);
        assert!(s.users.is_empty());
        assert!(s.residential.default_group().is_some());
        assert!(s.system.ssh_hardening && s.system.static_dns);
        assert_eq!(s.versions.bui, "4.0.0");
    }

    struct FakeFetcher(Mutex<Vec<(String, Vec<u8>)>>);
    impl Fetcher for FakeFetcher {
        fn get_bytes(&self, url: &str) -> anyhow::Result<Vec<u8>> {
            self.0
                .lock()
                .unwrap()
                .iter()
                .find(|(u, _)| u == url)
                .map(|(_, b)| b.clone())
                .ok_or_else(|| anyhow::anyhow!("404 {url}"))
        }
    }

    fn fetcher_with_manifest() -> Arc<dyn Fetcher> {
        let payload = b"ELF".to_vec();
        let sum = crate::kernels::sha256_hex(&payload);
        let asset = |n: &str| serde_json::json!({"url": format!("https://x/{n}"), "sha256": sum});
        let manifest = serde_json::json!({
            "version": "4.0.0",
            "kernels": { "hysteria": "2.12.2", "xray": "26.3.27", "sing_box": "1.13.19", "caddy": "2.10.2" },
            "artifacts": {
                "bui-linux-amd64": asset("bui"),
                "hysteria-linux-amd64": asset("hysteria"),
                "xray-linux-amd64": asset("xray"),
                "sing-box-linux-amd64": asset("sing-box"),
                "caddy-linux-amd64": asset("caddy")
            }
        });
        let mut files = vec![(
            crate::kernels::MANIFEST_URL.to_string(),
            manifest.to_string().into_bytes(),
        )];
        for n in ["bui", "hysteria", "xray", "sing-box", "caddy"] {
            files.push((format!("https://x/{n}"), payload.clone()));
        }
        Arc::new(FakeFetcher(Mutex::new(files)))
    }

    /// 全部路径都在 tempdir 下：Global Constraints 的「单元测试不碰真实系统」
    fn scratch(d: &tempfile::TempDir) -> bui_schema::paths::Paths {
        bui_schema::paths::Paths {
            base_dir: d.path().into(),
            certs_dir: d.path().join("certs"),
            bin_dir: d.path().join("bin"),
        }
    }

    /// 装机用的假机器：公钥在位；`bin/` 下的 xray 被脚本化（脚本键按 tempdir 拼）
    fn host_for_install(paths: &bui_schema::paths::Paths) -> Arc<FakeHost> {
        let bin = |n: &str| paths.bin_dir.join(n).display().to_string();
        let h = Arc::new(FakeHost::new());
        h.with(|i| {
            i.files.insert(
                "/root/.ssh/authorized_keys".into(),
                (b"ssh-ed25519 AAAA me\n".to_vec(), 0o600),
            );
            i.scripted
                .push((format!("{} x25519", bin("xray")), CmdOut::success(X25519)));
            i.scripted.push((
                format!("{} version", bin("xray")),
                CmdOut::success("Xray 26.3.27 (Xray) a (go1 linux/amd64)\n"),
            ));
            i.scripted.push((
                format!("{} version", bin("hysteria")),
                CmdOut::success("Version:\tv2.12.2\n"),
            ));
            i.scripted.push((
                format!("{} version", bin("sing-box")),
                CmdOut::success("sing-box version 1.13.19\n"),
            ));
            i.scripted.push((
                format!("{} version", bin("caddy")),
                CmdOut::success("v2.10.2 h1:x\n"),
            ));
            i.scripted
                .push(("curl".into(), CmdOut::success("203.0.113.10")));
        });
        h
    }

    /// `socket` 指向 tempdir 里一个**不存在**的路径：第 9 步的 `Client::available()` 立刻返回
    /// false，对账走进程内那一支，测试全程不碰 `/run/b-ui.sock`
    fn opts(d: &tempfile::TempDir) -> InstallOpts {
        InstallOpts {
            domain: Some("example.com".into()),
            port: None,
            admin_password_stdin: false,
            import_v3: None,
            non_interactive: false,
            answers: None,
            yes: true,
            socket: d.path().join("absent.sock"),
        }
    }

    /// 测试一律显式传 manifest 地址，不读进程环境（`$BUI_MANIFEST_URL` 在开发机上可能有值）
    fn murl() -> String {
        crate::kernels::MANIFEST_URL.to_string()
    }

    fn answers() -> Answers {
        Answers {
            domain: "example.com".into(),
            admin_password: "test123".into(),
            ..Answers::defaults("node-a", "203.0.113.10")
        }
    }

    #[tokio::test]
    async fn install_writes_state_downloads_kernels_and_reconciles() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let host = host_for_install(&paths);
        run_with(
            opts(&d),
            answers(),
            murl(),
            paths.clone(),
            host.clone(),
            fetcher_with_manifest(),
        )
        .await
        .unwrap();
        let at = |rel: &str| d.path().join(rel).display().to_string();
        assert!(
            host.text(&at("manifest.json")).is_some(),
            "manifest 要缓存下来给对账用"
        );
        assert_eq!(host.mode(&at("bin/xray")), Some(0o755));
        assert!(host
            .text(&at("config.yaml"))
            .unwrap()
            .contains("listen: :10000,20000-30000"));
        assert!(host.text(&at("xray-config.json")).is_some());
        assert!(host.text(&at("singbox-relay.json")).is_some());
        assert!(
            host.text(&at("Caddyfile")).unwrap().contains("example.com"),
            "Caddyfile 按 C3 放 <base>"
        );
        assert!(host.text("/etc/systemd/system/b-ui.service").is_some());
        assert_eq!(
            host.read_link(std::path::Path::new("/usr/local/bin/b-ui"))
                .unwrap(),
            Some(paths.bin_dir.join("bui")),
            "sudo b-ui 的入口要建好"
        );
        // state.json 由 Store 写真实文件系统（tempdir 内）
        let state: bui_schema::model::State =
            serde_json::from_slice(&std::fs::read(crate::paths::state_file(&paths)).unwrap())
                .unwrap();
        assert_eq!(
            state.node.reality.public_key,
            "cTpW46LZoWSn3XlHahzkRh3CMpu-pEQUOk7-seT7W1c"
        );
        assert_eq!(state.node.domain, "example.com");
    }

    #[tokio::test]
    async fn install_writes_an_auth_snapshot_the_hook_can_read() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let host = host_for_install(&paths);
        run_with(
            opts(&d),
            answers(),
            murl(),
            paths.clone(),
            host.clone(),
            fetcher_with_manifest(),
        )
        .await
        .unwrap();
        let snap_path = crate::paths::auth_snapshot_file(&paths)
            .display()
            .to_string();
        let snap: serde_json::Value =
            serde_json::from_str(&host.text(&snap_path).unwrap()).unwrap();
        // 形状逐字照总纲 C5：顶层 schema + users；全新装机没有用户 → users 是空对象（不是缺文件）
        assert_eq!(snap, serde_json::json!({ "schema": 1, "users": {} }));
        assert_eq!(
            host.mode(&snap_path),
            Some(0o600),
            "快照含明文 HY2 密码，必须 0600"
        );
        // 有用户时：users 的键 = 用户名，四个字段与 P2 的 auth-hook 约定一致
        let state = crate::testutil::sample_state();
        let u = &state.users[0];
        let snap = auth_snapshot(&state);
        assert_eq!(snap["schema"], 1);
        assert_eq!(
            snap["users"][u.username.as_str()],
            serde_json::json!({
                "user_id": u.user_id.to_string(),
                "hy2_password": u.credentials.hy2_password,
                "expires_at": u.entitlements.expires_at,
                "blocked": u.disabled
            })
        );
        assert!(snap["users"].get("nobody").is_none());
    }

    #[tokio::test]
    async fn reinstall_on_an_installed_machine_generates_no_password_and_probes_no_ip() {
        // 活机器上重跑 `bui install`（升级脚本、`--yes` 的自动化都会）只该对账：
        // ① 再打印一行「已生成随机管理员密码」会让运维以为面板密码被换了（其实 state.json 里
        //    的哈希一个字没动）；② 公网 IP 已在 state 里，没必要再花 5s curl 一次 ipify。
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let host = host_for_install(&paths);
        let state = crate::testutil::sample_state();
        host.clear_ops();
        // `notice` 就是 run() 要打到 stdout 的那一行（密码只在这一处出现）
        let (a, notice) = collect_answers(&opts(&d), Some(&state), host.clone())
            .await
            .unwrap();
        assert_eq!(notice, None, "已装机不生成、不打印随机管理员密码");
        assert!(a.admin_password.is_empty());
        assert_eq!(a.public_ip, "203.0.113.10", "复用 state.node.public_ip");
        assert_eq!(a.node_name, "node-a");
        assert_eq!(a.domain, "example.com");
        assert_eq!(a.ports.hy2, 10000);
        assert_eq!(a.masquerade, "www.bing.com:443");
        // IP 探测是 Host 上的一次 curl（Fetcher 不参与装机探测）：一条流水都不该有
        assert_eq!(host.ops(), Vec::<String>::new(), "{:?}", host.ops());
        // 全新装机（没有 state.json）那一支照旧探 IP、生成并打印密码
        host.clear_ops();
        let (b, notice) = collect_answers(&opts(&d), None, host.clone())
            .await
            .unwrap();
        let line = notice.expect("全新装机必须给出密码");
        assert!(line.contains("已生成随机管理员密码"), "{line}");
        assert!(line.contains(&b.admin_password));
        assert_eq!(b.admin_password.len(), 16);
        assert!(
            host.ops().iter().any(|o| o.contains("api.ipify.org")),
            "{:?}",
            host.ops()
        );
    }

    #[test]
    fn an_answers_file_overrides_only_what_it_names() {
        // 总纲 C5 的 `--non-interactive --answers <file>`：格式在本任务定死，P5 的 v3-cutover.sh 照它生成
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("answers.json");
        std::fs::write(
            &f,
            r#"{"domain":"example.com","ports":{"hy2":10500,"hy2_hop":[20000,30000],"hy2_resi":40000,
                "hy2_resi_hop":[41000,50000],"reality_direct":10001,"reality_resi":10002,"admin":8080}}"#,
        )
        .unwrap();
        let a = load_answers(&f, Answers::defaults("node-a", "203.0.113.10")).unwrap();
        assert_eq!(a.domain, "example.com");
        assert_eq!(a.ports.hy2, 10500);
        assert_eq!(a.node_name, "node-a", "文件里没写的项沿用默认");
        assert_eq!(a.public_ip, "203.0.113.10");
        assert_eq!(a.masquerade, "www.bing.com:443");
        assert!(a.admin_password.is_empty(), "答案文件里不放管理员密码");
        assert!(
            load_answers(&d.path().join("nope.json"), Answers::defaults("node-a", "")).is_err()
        );
    }

    #[tokio::test]
    async fn install_without_a_reachable_manifest_says_how_to_fix_it() {
        // 裁决「M5 前不创建任何 Release/tag」→ M1 时内置 MANIFEST_URL 必然 404、bin/ 里没有 xray。
        // 这一支必须给出点名 BUI_MANIFEST_URL 的可操作错误，而不是静默写出没有密钥的 state。
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let host = Arc::new(FakeHost::new()); // 没有 bin/xray，PATH 上也没有 xray
        let empty: Arc<dyn Fetcher> = Arc::new(FakeFetcher(Mutex::new(vec![])));
        let err = run_with(
            opts(&d),
            answers(),
            murl(),
            paths.clone(),
            host.clone(),
            empty,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("xray") && err.contains("BUI_MANIFEST_URL"),
            "{err}"
        );
        assert!(
            !crate::paths::state_file(&paths).exists(),
            "失败就不该留下半成品 state.json"
        );
    }

    #[tokio::test]
    async fn a_second_install_changes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let host = host_for_install(&paths);
        run_with(
            opts(&d),
            answers(),
            murl(),
            paths.clone(),
            host.clone(),
            fetcher_with_manifest(),
        )
        .await
        .unwrap();
        host.clear_ops();
        run_with(
            opts(&d),
            answers(),
            murl(),
            paths.clone(),
            host.clone(),
            fetcher_with_manifest(),
        )
        .await
        .unwrap();
        let verify_prefix = format!("write:{}", crate::paths::verify_dir(&paths).display());
        let writes: Vec<String> = host
            .ops()
            .into_iter()
            .filter(|o| o.starts_with("write:") && !o.starts_with(&verify_prefix))
            .collect();
        assert_eq!(writes, Vec::<String>::new(), "幂等：第二次 install 零写入");
    }

    /// 2026-09-13 裁决「P1：Caddy 外部站点通道」：新 Caddyfile（含导入的外部站点）过不了
    /// `caddy validate` → 中止 import，发行版 caddy 保持运行，刚写出的 `state.json` 回滚
    /// （不回滚的话下次重跑 install 走「已安装」分支，v3 永远卸不掉）。
    #[tokio::test]
    async fn install_with_import_v3_aborts_and_rolls_back_when_the_new_caddyfile_is_invalid() {
        let src = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../bui-schema/tests/fixtures/v3/src"
        ));
        if !src.exists() {
            eprintln!("skipped: 缺 P0 的 v3 fixture");
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let host = host_for_install(&paths);
        host.with(|i| {
            i.files.insert(
                crate::commands::import_v3::V3_CADDYFILE.into(),
                (
                    b"example.com {\n\treverse_proxy 127.0.0.1:8080\n}\n\nblog.example.com {\n\tbroken_directive\n}\n".to_vec(),
                    0o644,
                ),
            );
            i.scripted.push((
                format!("{} validate", paths.bin_dir.join("caddy").display()),
                CmdOut::failure(1, "unrecognized directive: broken_directive"),
            ));
            i.units_active.insert("caddy.service".into());
        });
        let mut o = opts(&d);
        o.import_v3 = Some(src.to_path_buf());
        let err = run_with(
            o,
            answers(),
            murl(),
            paths.clone(),
            host.clone(),
            fetcher_with_manifest(),
        )
        .await
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("已中止 v3 导入"), "{msg}");
        assert!(msg.contains("broken_directive"), "{msg}");
        assert!(
            !crate::paths::state_file(&paths).exists(),
            "state.json 必须回滚掉"
        );
        assert!(
            !host.ops().iter().any(|o| o == "systemd:stop:caddy"),
            "发行版 caddy 不许停：{:?}",
            host.ops()
        );
        assert!(host.unit_is_active("caddy").unwrap(), "发行版 caddy 还在跑");
    }

    #[tokio::test]
    async fn install_with_import_v3_keeps_the_v4_relay_and_leaves_no_drift() {
        // 回归两个真机事故：① V3_UNITS 含 b-ui-relay 会把刚装好的 v4 relay 卸掉；
        // ② v3 的状态文件留在 /opt/b-ui 下会被漂移扫描永久报告
        let src = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../bui-schema/tests/fixtures/v3/src"
        ));
        if !src.exists() {
            eprintln!("skipped: 缺 P0 的 v3 fixture");
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let host = host_for_install(&paths);
        // 假机器的 `<base>` 从 **fixture 的真实目录清单**播种，而不是从 `V3_FILES` /
        // `V3_STATE_FILES` 这两个常量播种：常量漏了什么，那种播种法就恰好也漏同一个，测试变成
        // 自我印证。再叠加真机上确实存在、但 fixture 不带的那几类残留。
        let fixture_names: Vec<String> = std::fs::read_dir(src)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            fixture_names.contains(&"server_ip.txt".to_string()),
            "fixture 应带 server_ip.txt"
        );
        host.with(|i| {
            for u in crate::commands::import_v3::V3_UNITS {
                i.files.insert(
                    format!("/etc/systemd/system/{u}").into(),
                    (b"x".to_vec(), 0o644),
                );
                i.units_enabled.insert(u.to_string());
                i.units_active.insert(u.to_string());
            }
            // ① fixture 里的每个顶层条目（certs/ 是目录，交给 is_dir 分支，不会被当陌生文件）
            for name in &fixture_names {
                let p = src.join(name);
                if p.is_dir() {
                    i.files.insert(
                        d.path().join(name).join("fullchain.pem"),
                        (b"CERT".to_vec(), 0o644),
                    );
                } else {
                    i.files
                        .insert(d.path().join(name), (std::fs::read(&p).unwrap(), 0o600));
                }
            }
            // ② v3 的 shell / Node / 二进制 + 迁移块留下的备份与临时文件（fixture 不带这些）
            for f in [
                "core.sh",
                "update.sh",
                "b-ui-cli.sh",
                "residential-helper.sh",
                "resi-health.sh",
                "hy2-watchdog.sh",
                "cert-sync.sh",
                "cert-check.sh",
                "hy2-portjump-cleanup.sh",
                "install-key.txt",
                "b-ui-client.sh",
                "version.json",
            ] {
                i.files
                    .insert(d.path().join(f), (b"#!/bin/bash".to_vec(), 0o755));
            }
            // v3 的 relay 二进制在顶层
            i.files
                .insert(d.path().join("sing-box"), (b"ELF".to_vec(), 0o755));
            i.files.insert(
                d.path().join("port-hopping.json"),
                (br#"{"enabled":true}"#.to_vec(), 0o644),
            );
            i.files.insert(
                d.path().join("masquerade.json"),
                (br#"{"masqueradeDomain":"www.bing.com"}"#.to_vec(), 0o644),
            );
            i.files.insert(
                d.path().join(".resi-health-state.json"),
                (b"{}".to_vec(), 0o600),
            );
            i.files
                .insert(d.path().join(".relay.lock"), (b"".to_vec(), 0o600));
            i.files.insert(
                d.path().join("config.yaml.bak.v357.1757000000"),
                (b"old".to_vec(), 0o600),
            );
            i.files.insert(
                d.path().join("xray-config.json.bak.v359.1757000001"),
                (b"{}".to_vec(), 0o600),
            );
            i.files.insert(
                d.path().join("config-residential.yaml.bak.v360.1757000002"),
                (b"old".to_vec(), 0o600),
            );
            i.files
                .insert(d.path().join("config.yaml.tmp"), (b"half".to_vec(), 0o600));
            i.files
                .insert(d.path().join("admin/server.js"), (b"node".to_vec(), 0o644));
            i.files.insert(
                d.path().join("admin/node_modules/x/index.js"),
                (b"x".to_vec(), 0o644),
            );
            i.files
                .insert("/tmp/hy2-watchdog-10000".into(), (b"2".to_vec(), 0o644));
        });
        let mut o = opts(&d);
        o.import_v3 = Some(src.to_path_buf());
        run_with(
            o,
            answers(),
            murl(),
            paths.clone(),
            host.clone(),
            fetcher_with_manifest(),
        )
        .await
        .unwrap();
        // v4 的 relay 单元必须还在、还在跑
        assert!(
            host.text("/etc/systemd/system/b-ui-relay.service")
                .is_some(),
            "v4 relay 单元被删了"
        );
        assert!(
            host.unit_is_active("b-ui-relay").unwrap(),
            "v4 relay 被停了"
        );
        assert!(!host.ops().iter().any(|o| o == "systemd:stop:b-ui-relay"));
        // v3 的痕迹：shell / 二进制删掉、状态文件归档、备份归档、临时文件删掉、
        // Node 目录整棵删掉、/tmp 计数文件删掉
        for f in crate::commands::import_v3::V3_FILES {
            assert!(
                host.text(d.path().join(f).to_str().unwrap()).is_none(),
                "{f} 应删除"
            );
        }
        for f in crate::commands::import_v3::V3_STATE_FILES {
            assert!(
                host.text(d.path().join(f).to_str().unwrap()).is_none(),
                "{f} 应移走"
            );
            assert!(
                host.text(
                    crate::paths::v3_backup_dir(&paths)
                        .join(f)
                        .to_str()
                        .unwrap()
                )
                .is_some(),
                "{f} 应进 v3-backup"
            );
        }
        for f in [
            "config.yaml.bak.v357.1757000000",
            "xray-config.json.bak.v359.1757000001",
            "config-residential.yaml.bak.v360.1757000002",
        ] {
            assert!(
                host.text(
                    crate::paths::v3_backup_dir(&paths)
                        .join(f)
                        .to_str()
                        .unwrap()
                )
                .is_some(),
                "{f} 应进 v3-backup"
            );
        }
        assert!(host
            .text(d.path().join("config.yaml.tmp").to_str().unwrap())
            .is_none());
        // v4 自己的 relay 二进制在 bin/ 下，不能被「删 <base>/sing-box」连带删掉
        assert!(
            host.text(paths.bin_dir.join("sing-box").to_str().unwrap())
                .is_some(),
            "v4 的 bin/sing-box 被删了"
        );
        assert!(host
            .text(
                d.path()
                    .join("admin/node_modules/x/index.js")
                    .to_str()
                    .unwrap()
            )
            .is_none());
        assert!(
            host.text("/tmp/hy2-watchdog-10000").is_none(),
            "spec §3.4：/tmp 计数文件要删"
        );
        // 导入 + 卸载 + 对账之后，体检必须零漂移（M1 验收项）
        let store = crate::state::store::Store::open(crate::paths::state_file(&paths))
            .await
            .unwrap();
        let state = store.read().await;
        let reg = crate::serve::modules(None);
        let ctx = crate::reconcile::RenderCtx {
            paths: paths.clone(),
            facts: crate::reconcile::Facts::probe(host.as_ref()).unwrap(),
        };
        let arts: Vec<crate::reconcile::Artifact> = reg
            .modules
            .iter()
            .flat_map(|m| m.render(&state, &ctx))
            .collect();
        assert_eq!(
            crate::reconcile::drift::scan(host.as_ref(), &arts, &paths),
            vec![],
            "import-v3 之后不能留下任何漂移"
        );
        assert_eq!(state.users.len(), 4, "四个 v3 用户都要导入");
    }
}
