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
    /// `--manifest-url`（与 `bui upgrade` 的同名选项同义，总纲 C4）
    pub manifest_url: Option<String>,
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

/// manifest 的来源：总纲 C4 的两个覆盖（`--manifest-url` 与 `$BUI_MANIFEST_URL`）在入口读一次
/// 就固定下来，一路传到 [`fetch_and_install_kernels`]，由
/// [`crate::kernels::fetch_manifest_with`] 按同一顺序解析（`bui upgrade` 走的也是它），
/// 包括「latest 404 ⇒ 改跟 releases 列表里最新预发布」那一步。
///
/// 传结构体而不是解析好的 url：回退需要知道「三个覆盖是不是都没给」，url 一旦算成字符串就分不清
/// 「用户把地址设成了内置 latest」和「什么都没设」。`env` 也显式带着，于是单元测试不读开发机环境。
#[derive(Debug, Clone, Default)]
pub struct ManifestSource {
    /// `--manifest-url`（最高优先）
    pub cli_override: Option<String>,
    /// `$BUI_MANIFEST_URL`：`install.sh` 会把它**实际用的**那个地址 export 下来
    pub env: Option<String>,
}

impl ManifestSource {
    /// `--manifest-url` + 进程环境里的 `$BUI_MANIFEST_URL`。
    pub fn from_env(cli_override: Option<String>) -> Self {
        Self {
            cli_override,
            env: std::env::var(crate::kernels::MANIFEST_URL_ENV).ok(),
        }
    }

    /// 按 C4 的顺序算出的地址（只用于报错文案；真正的拉取走 `fetch_manifest_with`，
    /// 它可能因 404 回退到另一个 tag）。
    fn resolved(&self) -> String {
        crate::kernels::resolve_manifest_url(
            self.cli_override.as_deref(),
            None,
            self.env.as_deref(),
        )
    }
}

impl From<&str> for ManifestSource {
    /// 钉死一个地址（等价于 `--manifest-url`）：不读进程环境、也不回退预发布通道。
    fn from(url: &str) -> Self {
        Self {
            cli_override: Some(url.to_string()),
            env: None,
        }
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

/// manifest 拉不到时打到 **stdout** 的那一行。tracing 在 systemd 机器上走 journald，运维在
/// `bui install` 的终端输出里看不到那条 warn（2026-09-12 真机实录：整份日志里没有任何
/// manifest/下载行，于是没人发现内核根本没装）。地址经 `redact` 脱敏。
fn manifest_failure_notice(url: &str, err: &str) -> String {
    format!(
        "⚠ 拉取 manifest 失败（{}）：{err}；未安装任何内核。请设 {}=<可用的 manifest 地址> 或先把内核二进制放进 bin/ 后重跑",
        crate::redact::url_credentials(url),
        crate::kernels::MANIFEST_URL_ENV,
    )
}

/// 第 3 + 4 步：拉 manifest → 缓存（内容相同不写）→ 把四个内核装到 `bin/`（版本一致则跳过）。
/// manifest 拉不到只警告并返回 `None`；**是否继续装机由调用方的内核闸门决定**
/// （[`run_with`] 的「内核缺一即中止」）。
///
/// 返回 **(实际用的 manifest 地址, manifest)**：地址由
/// [`crate::kernels::fetch_manifest_with`] 定（可能是 404 回退后的预发布 tag），闸门的报错文案
/// 要报的正是它，而不是解析前的那一串。
fn fetch_and_install_kernels(
    host: &dyn Host,
    fetcher: &dyn Fetcher,
    paths: &Paths,
    src: &ManifestSource,
) -> (String, Option<Manifest>) {
    let (manifest_url, manifest) = match crate::kernels::fetch_manifest_with(
        fetcher,
        src.cli_override.as_deref(),
        None,
        src.env.as_deref(),
    ) {
        Ok((url, m)) => (url, m),
        Err(e) => {
            let url = src.resolved();
            tracing::warn!(
                error = %e,
                url = %crate::redact::url_credentials(&url),
                env = crate::kernels::MANIFEST_URL_ENV,
                "拉取 manifest 失败，跳过内核安装（可用该环境变量覆盖地址）"
            );
            println!("{}", manifest_failure_notice(&url, &e.to_string()));
            return (url, None);
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
    (manifest_url, Some(manifest))
}

/// 面板域名的环境变量（与 `install.sh` 的 `BUI_DOMAIN` 同名同义：一行命令里不想把域名写进
/// argv 时用它）。
pub const DOMAIN_ENV: &str = "BUI_DOMAIN";

/// 装完自检有 FAIL 时的错误：[`run`] 据它退 **2**（裁决「任一 FAIL 退出码 2 但不回滚」）。
/// 用独立类型而不是一句错误串，`run` 才能把它和别的失败区分开——别的失败退 1。
#[derive(Debug)]
pub struct SelfCheckFailed(pub usize);

impl std::fmt::Display for SelfCheckFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "装完自检有 {} 项未通过（配置已落盘，未回滚）", self.0)
    }
}

impl std::error::Error for SelfCheckFailed {}

/// 面板域名的来源与优先级（**唯一必填项**，2026-09-12 裁决「安装：一行命令与零手工配置」）：
/// `--domain` > `$BUI_DOMAIN` > `--answers` 文件 > `/dev/tty` 只问这一问 > 报错退出。
///
/// `ask` 返回 `None` = 问不到（`--yes` / `--non-interactive`，或既不是终端又没有 `/dev/tty`）：
/// 此时**报错退出，不装一半**。其余问题（节点名、公网 IP、伪装目标、端口）一概不问。
pub fn pick_domain(
    cli: Option<&str>,
    env: Option<&str>,
    from_file: &str,
    ask: impl FnOnce() -> Option<String>,
) -> anyhow::Result<String> {
    for candidate in [cli.unwrap_or_default(), env.unwrap_or_default(), from_file] {
        let candidate = candidate.trim();
        if !candidate.is_empty() {
            return Ok(candidate.to_string());
        }
    }
    if let Some(answer) = ask() {
        let answer = answer.trim().to_string();
        if !answer.is_empty() {
            return Ok(answer);
        }
    }
    anyhow::bail!(
        "缺少面板域名（唯一必填项）：请 `bui install --domain <面板域名>`，或设 {DOMAIN_ENV}=<面板域名>，\
         或在 --answers 文件里给 domain；什么都没改，可直接重跑"
    )
}

/// 只问「面板域名」这一问；回答空串视为没答。
///
/// `curl … | bash` 时 stdin 是管道：`install.sh` 已经把 `/dev/tty` 接了进来（那时 stdin 就是
/// 终端），没接上则这里自己开一次 `/dev/tty`——两条路都不通返回 `None`。
fn ask_domain() -> Option<String> {
    use std::io::{BufRead, Write};
    // 先确定「有没有地方可读」，再打提示：反了的话无终端时会先留下一行悬空的提问
    // 才报「缺少面板域名」（2026-09-12 审查 nit）。
    let mut reader: Box<dyn BufRead> = if std::io::stdin().is_terminal() {
        Box::new(std::io::BufReader::new(std::io::stdin()))
    } else {
        Box::new(std::io::BufReader::new(
            std::fs::File::open("/dev/tty").ok()?,
        ))
    };
    print!("面板域名（唯一必填，例如 panel.example.com）: ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let v = line.trim().to_string();
    (!v.is_empty()).then_some(v)
}

/// 装机最后一屏（spec §7 第 10 步）：面板地址、一次性管理员密码（只有全新装机才有）、
/// 三种订阅地址的形状、两个入口。
pub fn summary(state: &State, password_notice: Option<&str>) -> String {
    let d = &state.node.domain;
    let mut out = vec![format!("面板        https://{d}/")];
    if let Some(line) = password_notice {
        out.push(format!("管理员      {line}"));
    }
    out.push(format!(
        "订阅        https://{d}/api/sub/<用户名>（v2rayN）· /api/subscription/<用户名>（sing-box）· /api/clash/<用户名>（mihomo）"
    ));
    out.push("后续        `b-ui` 进菜单 / `bui status` 看体检 / `bui reconcile` 手动对账".into());
    out.join("\n")
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
            let ip = crate::sys::probe_public_ip(h.as_ref());
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
    // 域名是唯一必填项，也是唯一还会问的一问；节点名 / 公网 IP / 伪装目标 / 端口全用探测值与
    // 默认值（`--answers` 文件仍可逐项覆盖），于是「一行命令完成新服务器的所有安装」成立。
    let env_domain = std::env::var(DOMAIN_ENV).ok();
    answers.domain = if opts.import_v3.is_some() {
        // v3 导入：域名沿用 v3 的 Caddyfile（`run_with` 的导入分支整份用 `report.state`），
        // 所以这里既不要求也不追问；命令行/环境变量给了就以它为准。
        opts.domain.clone().or(env_domain).unwrap_or(answers.domain)
    } else {
        pick_domain(
            opts.domain.as_deref(),
            env_domain.as_deref(),
            &answers.domain,
            || (!opts.quiet()).then(ask_domain).flatten(),
        )?
    };
    if let Some(p) = opts.port {
        answers.ports.hy2 = p;
    }
    let notice = if opts.admin_password_stdin {
        let mut buf = String::new();
        std::io::stdin().read_line(&mut buf)?;
        answers.admin_password = buf.trim_end_matches('\n').to_string();
        None
    } else if opts.import_v3.is_some() {
        // 导入路径的管理员密码来自 v3（`admin.env` 的 `ADMIN_PASSWORD`；v3 缺 admin.env 时由
        // `bui_schema::v3::import` 生成随机密码并带一条「请用 CLI 重设」的导入提示）。
        // `run_with` 的导入分支整份用 `report.state`，`answers.admin_password` 一个字都不看 ——
        // 2026-09-12 真机日志第一行「已生成随机管理员密码：…」而面板实际仍用 v3 密码，纯误导。
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

/// 最后一屏的摘要文本：`state.json` 已落盘就给，否则 `None`（那时什么都还没装成）。
///
/// 判据是「`state.json` 在不在」而**不是**「`run_with` 成功或只是自检 FAIL」：`Store::create`
/// 之后还有写 `auth-snapshot.json`、IPC 对账、`reconcile_from_ctx`、「对账有失败项」一串失败点，
/// 而全新服务器上证书签发或校验器失败非常常见。一次性管理员密码只有这一屏机会（`state.json`
/// 里只存 argon2 hash，面板 `/api/password` 又要先登录），漏打就只剩「删 state.json 整机重装」
/// ——REALITY 密钥等全部重生成——这一条路。
async fn final_summary(state_path: &Path, notice: Option<&str>) -> Option<String> {
    let store = Store::open(state_path).await.ok()?;
    Some(summary(store.read().await.as_ref(), notice))
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
    // C4 的两个覆盖读一次就固定（`install.sh` 把它选定的那个 tag export 成 `$BUI_MANIFEST_URL`）
    let manifest = ManifestSource::from_env(opts.manifest_url.clone());
    let outcome = run_with(
        opts,
        answers,
        manifest,
        paths,
        host,
        Arc::new(HttpFetcher::new()),
    )
    .await;
    // 摘要压在最后一屏（一次性管理员密码只在这里出现这一次）：自检有 FAIL 也要给出面板地址，
    // 不然运维连去哪儿看都不知道。
    let failed = match &outcome {
        Err(e) => e.downcast_ref::<SelfCheckFailed>().map(|f| f.0),
        Ok(()) => None,
    };
    if let Some(text) = final_summary(&state_path, notice.as_deref()).await {
        println!("{text}");
    }
    if let Some(n) = failed {
        // 裁决「任一 FAIL 退出码 2 但不回滚」：配置已落盘、v3 已卸掉，回滚只会更糟。
        eprintln!(
            "自检有 {n} 项未通过（未回滚）：按上表逐项处理后 `bui reconcile`、`bui status` 复检"
        );
        std::process::exit(2);
    }
    outcome
}

/// spec §7 的十步。`manifest` 是 C4 的覆盖来源（[`ManifestSource`]），由调用方读一次；
/// 测试传 `"<url>".into()` 钉死地址，不读进程环境。
pub async fn run_with(
    opts: InstallOpts,
    answers: Answers,
    manifest: ManifestSource,
    paths: Paths,
    host: Arc<dyn Host>,
    fetcher: Arc<dyn Fetcher>,
) -> anyhow::Result<()> {
    run_with_wait(
        opts,
        answers,
        manifest,
        paths,
        host,
        fetcher,
        crate::commands::selfcheck::Wait::default(),
    )
    .await
}

/// [`run_with`] 外加「自检前等多久」（见 [`crate::commands::selfcheck::Wait`]）。
/// 全新装机要等首张证书，所以默认 120s；**故意坏掉的**那几个测试传 `Wait::NONE`，
/// 否则每条都要真睡满两分钟。
#[allow(clippy::too_many_arguments)]
pub async fn run_with_wait(
    opts: InstallOpts,
    answers: Answers,
    manifest_src: ManifestSource,
    paths: Paths,
    host: Arc<dyn Host>,
    fetcher: Arc<dyn Fetcher>,
    wait: crate::commands::selfcheck::Wait,
) -> anyhow::Result<()> {
    let state_path = crate::paths::state_file(&paths);
    let fresh = !state_path.exists();
    // 3 + 4：拉 manifest（C4 解析 + latest 404 回退预发布，与 `bui upgrade` 同一处实现）
    // → 缓存 → 装四个内核（阻塞线程）
    let (manifest_url, manifest) = {
        let (h, f, p, s) = (
            host.clone(),
            fetcher.clone(),
            paths.clone(),
            manifest_src.clone(),
        );
        tokio::task::spawn_blocking(move || {
            fetch_and_install_kernels(h.as_ref(), f.as_ref(), &p, &s)
        })
        .await?
    };
    // 4.5：**内核缺一即中止**（2026-09-12 bwg-rick 真机实录）。manifest 服务没起来时第 3 步只
    // 警告，旧代码接着写单元、接着 `uninstall_v3`：v3 被拆掉而 `bin/` 里只有 bui，
    // hysteria×2/relay 全 203/EXEC、caddy 也起不来，外部生产站点跟着断。
    //
    // 闸门必须在**写任何单元/配置与 `uninstall_v3` 之前**：此时一个破坏性动作都还没做，
    // 报错退出后 v3 一字不动、`state.json` 也还没落盘（下次修好 manifest 重跑即可）。
    // 只管全新装机与导入：活机器上重跑 install 只对账，内核该由 `bui upgrade` 补。
    if fresh {
        // 4.2：环境探测 + 打一张「环境」表（主理人 2026-09-12「安装我需要能自动检测环境」）。
        // 顺手修 SELinux 标签与时钟；唯一会中止的是**关键端口被非本栈进程占着**——此时一个
        // 破坏性动作都还没做，腾了端口重跑即可。v3 迁移例外：那些端口正被要替换掉的 v3 占着。
        let env = {
            let (h, p) = (host.clone(), paths.clone());
            let (ports, domain, ip) = (
                answers.ports.clone(),
                answers.domain.clone(),
                answers.public_ip.clone(),
            );
            let skip_ports = opts.import_v3.is_some();
            tokio::task::spawn_blocking(move || {
                crate::sys::env_probe::probe(
                    h.as_ref(),
                    &domain,
                    &ip,
                    &ports,
                    &p.bin_dir,
                    skip_ports,
                )
            })
            .await?
        };
        println!("{}", crate::sys::env_probe::table(&env, &answers.ports));
        if let Some(msg) = env.blocking() {
            anyhow::bail!(msg);
        }
        let missing = {
            // 探测走 spawn_blocking：`installed_versions` 会 run 四次 `<bin>/<kernel> version`
            let (h, p) = (host.clone(), paths.clone());
            tokio::task::spawn_blocking(move || {
                crate::kernels::missing_kernels(h.as_ref(), &p.bin_dir)
            })
            .await?
        };
        if !missing.is_empty() {
            anyhow::bail!(
                "缺少内核：{}（manifest：{}）；已中止安装，v3 与现有服务一字未动。\
                 请先把这些二进制放进 {}/，或设 {}=<可用的 manifest 地址> 后重跑 bui install",
                missing.join("、"),
                crate::redact::url_credentials(&manifest_url),
                paths.bin_dir.display(),
                crate::kernels::MANIFEST_URL_ENV,
            );
        }
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
    // 10：装完自检（PASS/FAIL 表）。判据照 `scripts/m1-acceptance.sh`，外加一条 HY2 回环鉴权。
    // 摘要（面板地址 / 一次性密码 / 订阅形状）由 [`run`] 在这之后打，是最后一屏。
    let rows = {
        let state = ctx.store.read().await.clone();
        let (h, p, drift) = (host.clone(), paths.clone(), report.drift.clone());
        tokio::task::spawn_blocking(move || {
            crate::commands::selfcheck::run(h.as_ref(), &p, &state, &drift, wait)
        })
        .await?
    };
    println!("{}", crate::commands::selfcheck::table(&rows));
    if !report.errors.is_empty() || !report.verify_failures.is_empty() {
        anyhow::bail!("对账有失败项，见上面的输出");
    }
    let failed = crate::commands::selfcheck::failures(&rows);
    if failed > 0 {
        return Err(anyhow::Error::new(SelfCheckFailed(failed)));
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

    /// 表里没有的地址回 [`crate::kernels::NotFound`]，与 `HttpFetcher` 的 404 同形：
    /// 「latest 404 ⇒ 回退预发布」认的就是这个错误类型。
    struct FakeFetcher(Mutex<Vec<(String, Vec<u8>)>>);
    impl Fetcher for FakeFetcher {
        fn get_bytes(&self, url: &str) -> anyhow::Result<Vec<u8>> {
            self.0
                .lock()
                .unwrap()
                .iter()
                .find(|(u, _)| u == url)
                .map(|(_, b)| b.clone())
                .ok_or_else(|| crate::kernels::NotFound(url.to_string()).into())
        }
    }

    /// 一份能装成的 manifest 挂在 `manifest_url` 上，外加它引用的五个资产。
    fn manifest_files(manifest_url: &str) -> Vec<(String, Vec<u8>)> {
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
        let mut files = vec![(manifest_url.to_string(), manifest.to_string().into_bytes())];
        for n in ["bui", "hysteria", "xray", "sing-box", "caddy"] {
            files.push((format!("https://x/{n}"), payload.clone()));
        }
        files
    }

    fn fetcher_with_manifest() -> Arc<dyn Fetcher> {
        Arc::new(FakeFetcher(Mutex::new(manifest_files(
            crate::kernels::MANIFEST_URL,
        ))))
    }

    /// 2026-09-12 真机 bwg-tizi 的服务端形态：仓库里只有预发布，`releases/latest/download/`
    /// 404，manifest 只挂在 `releases/download/v4.0.0-rc2/` 下。
    const RC_TAG: &str = "v4.0.0-rc2";

    fn fetcher_with_prerelease_only() -> Arc<dyn Fetcher> {
        let mut files = manifest_files(&crate::kernels::manifest_url_for_tag(RC_TAG));
        files.push((
            crate::kernels::RELEASES_API_URL.to_string(),
            format!(r#"[{{"tag_name":"{RC_TAG}","prerelease":true}}]"#).into_bytes(),
        ));
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

    /// 装机用的假机器：公钥在位；`bin/` 下的 xray 被脚本化（脚本键按 tempdir 拼）。
    ///
    /// 环境探测与装完自检也要有得可探，否则每个 install 测试都会被「systemd 缺失」
    /// 「关键端口没在监听」染红：`systemctl` 在 PATH 上、`/etc/os-release` 认得出发行版、
    /// 域名解析到 `answers()` 的公网 IP、六单元起来后该听的端口都在听、HY2 回环探测回 200。
    fn host_for_install(paths: &bui_schema::paths::Paths) -> Arc<FakeHost> {
        let bin = |n: &str| paths.bin_dir.join(n).display().to_string();
        let h = Arc::new(FakeHost::new());
        h.with(|i| {
            i.files.insert(
                "/root/.ssh/authorized_keys".into(),
                (b"ssh-ed25519 AAAA me\n".to_vec(), 0o600),
            );
            i.which.insert("systemctl".into());
            i.files.insert(
                "/etc/os-release".into(),
                (b"ID=debian\nVERSION_ID=\"12\"\n".to_vec(), 0o644),
            );
            i.scripted.push((
                "getent ahosts example.com".into(),
                CmdOut::success("203.0.113.10 STREAM example.com\n"),
            ));
            i.listening.insert(
                crate::sys::Proto::Tcp,
                std::collections::BTreeSet::from([443, 10001, 10002, 8080]),
            );
            i.listening.insert(
                crate::sys::Proto::Udp,
                std::collections::BTreeSet::from([10000, 40000]),
            );
            // 证书已同步（两个 hysteria 的 tls.cert）；缺它时自检等一等再判 SKIP，
            // 见 a_fresh_install_without_a_certificate_yet_still_exits_zero
            i.files.insert(
                paths.certs_dir.join("fullchain.pem"),
                (b"CERT".to_vec(), 0o644),
            );
            // 自检的 HY2 回环探测脚本（有用户时才跑）
            i.scripted.push(("sh ".into(), CmdOut::success("200\n")));
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
            manifest_url: None,
            socket: d.path().join("absent.sock"),
        }
    }

    /// 测试一律显式传 manifest 地址，不读进程环境（`$BUI_MANIFEST_URL` 在开发机上可能有值）
    fn murl() -> ManifestSource {
        crate::kernels::MANIFEST_URL.into()
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
        run_with_wait(
            opts(&d),
            answers(),
            murl(),
            paths.clone(),
            host.clone(),
            fetcher_with_manifest(),
            crate::commands::selfcheck::Wait::NONE,
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
        run_with_wait(
            opts(&d),
            answers(),
            murl(),
            paths.clone(),
            host.clone(),
            fetcher_with_manifest(),
            crate::commands::selfcheck::Wait::NONE,
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

    /// 2026-09-12 真机 bwg-tizi：`install.sh` 从 `releases/download/v4.0.0-rc2/` 正确下到了
    /// manifest 与 bui，`bui install` 却按内置默认去问 `releases/latest` ⇒ 404 ⇒ 四个内核
    /// 一个都没装、被中止守卫拦下。install 与 upgrade 走同一处 manifest 解析后，
    /// 「什么都没指定 + latest 404」就该自己改跟 releases 列表里最新的预发布。
    #[tokio::test]
    async fn install_follows_the_prerelease_channel_when_latest_is_404() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let host = host_for_install(&paths);
        run_with_wait(
            opts(&d),
            answers(),
            // 三个覆盖一个都不给：这正是真机上 `bui install` 的处境
            ManifestSource::default(),
            paths.clone(),
            host.clone(),
            fetcher_with_prerelease_only(),
            crate::commands::selfcheck::Wait::NONE,
        )
        .await
        .unwrap();
        for k in crate::kernels::KERNELS {
            assert_eq!(
                host.mode(&paths.bin_dir.join(k).display().to_string()),
                Some(0o755),
                "{k} 该从预发布 manifest 装上"
            );
        }
        assert!(
            host.text(&d.path().join("manifest.json").display().to_string())
                .is_some(),
            "预发布 manifest 也要缓存下来给对账用"
        );
    }

    /// `install.sh` 把它**实际用的**地址 export 成 `$BUI_MANIFEST_URL`（本次一起修的 A 面）：
    /// `bui install` 必须认它，而不是回到内置 latest。认了它就不再去问 releases 列表
    /// （这个 fetcher 里根本没有那一条，去问就会失败）。
    #[tokio::test]
    async fn install_uses_the_manifest_url_handed_down_by_install_sh() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let host = host_for_install(&paths);
        let rc_url = crate::kernels::manifest_url_for_tag(RC_TAG);
        let fetcher: Arc<dyn Fetcher> = Arc::new(FakeFetcher(Mutex::new(manifest_files(&rc_url))));
        run_with_wait(
            opts(&d),
            answers(),
            ManifestSource {
                cli_override: None,
                env: Some(rc_url),
            },
            paths.clone(),
            host.clone(),
            fetcher,
            crate::commands::selfcheck::Wait::NONE,
        )
        .await
        .unwrap();
        assert_eq!(
            host.mode(&paths.bin_dir.join("xray").display().to_string()),
            Some(0o755),
            "$BUI_MANIFEST_URL 指的那份 manifest 要真被用上"
        );
        // `--manifest-url` 压过环境变量（C4 的顺序，与 upgrade 同）
        let src = ManifestSource {
            cli_override: Some("https://cli/manifest.json".into()),
            env: Some("https://env/manifest.json".into()),
        };
        assert_eq!(src.resolved(), "https://cli/manifest.json");
    }

    #[tokio::test]
    async fn install_without_a_reachable_manifest_says_how_to_fix_it() {
        // 裁决「M5 前不创建任何 Release/tag」→ M1 时内置 MANIFEST_URL 必然 404、bin/ 里没有 xray。
        // 这一支必须给出点名 BUI_MANIFEST_URL 的可操作错误，而不是静默写出没有密钥的 state。
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let host = Arc::new(FakeHost::new()); // 没有 bin/xray，PATH 上也没有 xray
        let empty: Arc<dyn Fetcher> = Arc::new(FakeFetcher(Mutex::new(vec![])));
        let err = run_with_wait(
            opts(&d),
            answers(),
            murl(),
            paths.clone(),
            host.clone(),
            empty,
            crate::commands::selfcheck::Wait::NONE,
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
        run_with_wait(
            opts(&d),
            answers(),
            murl(),
            paths.clone(),
            host.clone(),
            fetcher_with_manifest(),
            crate::commands::selfcheck::Wait::NONE,
        )
        .await
        .unwrap();
        host.clear_ops();
        run_with_wait(
            opts(&d),
            answers(),
            murl(),
            paths.clone(),
            host.clone(),
            fetcher_with_manifest(),
            crate::commands::selfcheck::Wait::NONE,
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

    /// v3 fixture 的路径（缺则跳过：P0 的 fixture 不在时不该红）。
    fn v3_fixture() -> Option<&'static Path> {
        let src = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../bui-schema/tests/fixtures/v3/src"
        ));
        if src.exists() {
            Some(src)
        } else {
            eprintln!("skipped: 缺 P0 的 v3 fixture");
            None
        }
    }

    /// 2026-09-12 bwg-rick 真机实录：本机 manifest 服务没起来 → `bui install --import-v3 --yes`
    /// 拉 manifest 失败**却继续跑**，`uninstall_v3` 把 v3 拆了而 `bin/` 里只有 bui，
    /// hysteria×2/relay 全 203/EXEC、caddy 也起不来，外部生产站点断了几分钟。
    /// 内核缺一即中止：v3 一字不动、`state.json` 不落盘。
    #[tokio::test]
    async fn import_v3_aborts_before_touching_anything_when_a_kernel_is_missing() {
        let Some(src) = v3_fixture() else { return };
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        // `bin/` 里只有 bui（真机形态）；manifest 拉不到 → 一个内核都装不上
        let host = Arc::new(FakeHost::new());
        host.with(|i| {
            i.files
                .insert(paths.bin_dir.join("bui"), (b"ELF".to_vec(), 0o755));
            for u in crate::commands::import_v3::V3_UNITS {
                i.files.insert(
                    format!("/etc/systemd/system/{u}").into(),
                    (b"x".to_vec(), 0o644),
                );
                i.units_enabled.insert(u.to_string());
                i.units_active.insert(u.to_string());
            }
            i.units_active.insert("caddy.service".into());
        });
        let empty: Arc<dyn Fetcher> = Arc::new(FakeFetcher(Mutex::new(vec![])));
        let mut o = opts(&d);
        o.import_v3 = Some(src.to_path_buf());
        let err = run_with_wait(
            o,
            answers(),
            // 带 userinfo：错误信息里的 manifest 地址必须经 redact
            "https://ops:s3cr3t@manifest.example.invalid/manifest.json".into(),
            paths.clone(),
            host.clone(),
            empty,
            crate::commands::selfcheck::Wait::NONE,
        )
        .await
        .unwrap_err();
        let msg = format!("{err:#}");
        for k in crate::kernels::KERNELS {
            assert!(msg.contains(k), "缺的内核要逐个点名：{msg}");
        }
        assert!(msg.contains("BUI_MANIFEST_URL"), "{msg}");
        assert!(
            msg.contains(&paths.bin_dir.display().to_string()),
            "要告诉人把二进制放哪：{msg}"
        );
        assert!(
            msg.contains("manifest.example.invalid") && msg.contains("***:***"),
            "manifest 地址要给、凭据要脱敏：{msg}"
        );
        assert!(!msg.contains("s3cr3t"), "凭据泄漏：{msg}");
        assert!(
            !crate::paths::state_file(&paths).exists(),
            "state.json 不许落盘（留着它下次 install 会走「已安装」分支，v3 永远卸不掉）"
        );
        // v3 一字不动：单元没停没删、没写过任何单元/配置
        for u in crate::commands::import_v3::V3_UNITS {
            assert!(
                host.text(&format!("/etc/systemd/system/{u}")).is_some(),
                "{u} 被删了"
            );
            assert!(host.unit_is_active(u).unwrap(), "{u} 被停了");
        }
        assert!(host.unit_is_active("caddy").unwrap(), "发行版 caddy 被停了");
        let bad: Vec<String> = host
            .ops()
            .into_iter()
            .filter(|o| {
                o.starts_with("write:") || o.starts_with("remove:") || o.starts_with("systemd:")
            })
            .collect();
        assert_eq!(bad, Vec::<String>::new(), "任何破坏性动作都不该发生");
    }

    /// 全新装机路径同样卡住（此时 v3 不在，但也不该写出半成品的单元与配置）。
    #[tokio::test]
    async fn a_fresh_install_aborts_when_a_kernel_is_missing() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let host = host_for_install(&paths);
        // xray 在 bin/ 下（能生成 REALITY 密钥），另外三个内核缺
        host.with(|i| {
            i.files
                .insert(paths.bin_dir.join("xray"), (b"ELF".to_vec(), 0o755));
        });
        let empty: Arc<dyn Fetcher> = Arc::new(FakeFetcher(Mutex::new(vec![])));
        let err = run_with_wait(
            opts(&d),
            answers(),
            murl(),
            paths.clone(),
            host.clone(),
            empty,
            crate::commands::selfcheck::Wait::NONE,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("hysteria") && err.contains("sing-box") && err.contains("caddy"),
            "{err}"
        );
        assert!(!err.contains("xray"), "xray 在位就不该点它的名：{err}");
        assert!(!crate::paths::state_file(&paths).exists());
        assert!(
            host.text("/etc/systemd/system/b-ui.service").is_none(),
            "不该写出任何单元"
        );
        assert!(host
            .text(&d.path().join("config.yaml").display().to_string())
            .is_none());
    }

    /// manifest 拉不到时的提示必须进 **stdout**：守护进程/CLI 的 tracing 在 systemd 机器上
    /// 走 journald，运维在 `bui install` 的终端输出里根本看不到那条 warn（2026-09-12 真机）。
    #[test]
    fn the_manifest_failure_notice_is_actionable_and_redacted() {
        let line = manifest_failure_notice(
            "https://ops:s3cr3t@manifest.example.invalid/manifest.json",
            "connection refused",
        );
        assert!(line.contains("manifest"), "{line}");
        assert!(line.contains("connection refused"), "{line}");
        assert!(line.contains("BUI_MANIFEST_URL"), "{line}");
        assert!(
            line.contains("***:***") && !line.contains("s3cr3t"),
            "{line}"
        );
    }

    /// 2026-09-12 真机：日志第一行「已生成随机管理员密码：…」，而面板实际仍用 v3 的密码
    /// （导入的哈希没被覆盖）——纯误导。`--import-v3` 一律不生成、不打印随机密码。
    #[tokio::test]
    async fn import_v3_neither_generates_nor_prints_a_random_admin_password() {
        let Some(src) = v3_fixture() else { return };
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let host = host_for_install(&paths);
        let mut o = opts(&d);
        o.import_v3 = Some(src.to_path_buf());
        // `notice` 是「已生成随机管理员密码」唯一的出口（run() 把它 println 出去）
        let (a, notice) = collect_answers(&o, None, host.clone()).await.unwrap();
        assert_eq!(notice, None, "导入路径不许打印随机管理员密码");
        assert!(a.admin_password.is_empty(), "导入路径不许生成密码");
        // 端到端：state 里的哈希就是 v3 admin.env 的密码
        run_with_wait(
            {
                let mut o = opts(&d);
                o.import_v3 = Some(src.to_path_buf());
                o
            },
            Answers {
                admin_password: "cli-side-password-must-be-ignored".into(),
                ..answers()
            },
            murl(),
            paths.clone(),
            host.clone(),
            fetcher_with_manifest(),
            crate::commands::selfcheck::Wait::NONE,
        )
        .await
        .unwrap();
        let state: bui_schema::model::State =
            serde_json::from_slice(&std::fs::read(crate::paths::state_file(&paths)).unwrap())
                .unwrap();
        assert!(
            crate::api::auth::verify_password(&state.admin.password_hash, "test123"),
            "面板密码必须是 v3 admin.env 里的那个"
        );
        assert!(!crate::api::auth::verify_password(
            &state.admin.password_hash,
            "cli-side-password-must-be-ignored"
        ));
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
        let err = run_with_wait(
            o,
            answers(),
            murl(),
            paths.clone(),
            host.clone(),
            fetcher_with_manifest(),
            crate::commands::selfcheck::Wait::NONE,
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
        run_with_wait(
            o,
            answers(),
            murl(),
            paths.clone(),
            host.clone(),
            fetcher_with_manifest(),
            crate::commands::selfcheck::Wait::NONE,
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

    /// 2026-09-12 裁决：面板域名是**唯一必填**，来源优先级 `--domain` > `$BUI_DOMAIN` >
    /// `--answers` 文件 > `/dev/tty` 只问这一问 > 报错退出（绝不装一半）。
    #[test]
    fn the_domain_is_the_only_required_answer_and_has_a_strict_priority() {
        let never = || -> Option<String> { panic!("已有来源时不该再问") };
        assert_eq!(
            pick_domain(
                Some("cli.example.com"),
                Some("env.example.com"),
                "file.example.com",
                never
            )
            .unwrap(),
            "cli.example.com"
        );
        assert_eq!(
            pick_domain(None, Some("env.example.com"), "file.example.com", never).unwrap(),
            "env.example.com"
        );
        assert_eq!(
            pick_domain(None, None, "file.example.com", never).unwrap(),
            "file.example.com"
        );
        assert_eq!(
            pick_domain(None, None, "", || Some(" tty.example.com \n".into())).unwrap(),
            "tty.example.com",
            "问来的答案要去掉首尾空白与换行"
        );
        // 空串 / 纯空白都不算来源，继续往下找
        assert_eq!(
            pick_domain(Some("   "), Some(""), "", || Some("tty.example.com".into())).unwrap(),
            "tty.example.com"
        );
        // 问不到（`--yes` / 无 tty）→ 报错，并给出三条出路
        let err = pick_domain(None, None, "", || None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("--domain"), "{err}");
        assert!(err.contains(DOMAIN_ENV), "{err}");
        assert!(err.contains("--answers"), "{err}");
        // 直接回车（空答）也算没答，不许拿空域名装下去
        assert!(pick_domain(None, None, "", || Some("\n".into())).is_err());
    }

    /// `--yes` / `--non-interactive` 语义不变：一个问题都不问，所以没有域名就报错而不是挂住。
    #[tokio::test]
    async fn a_quiet_install_without_a_domain_fails_instead_of_prompting() {
        if std::env::var_os(DOMAIN_ENV).is_some() {
            eprintln!("skipped: 开发机上设了 {DOMAIN_ENV}");
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let host = host_for_install(&paths);
        let mut o = opts(&d);
        o.domain = None; // opts() 里 yes = true
        let err = collect_answers(&o, None, host.clone())
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("缺少面板域名"), "{err}");
    }

    /// 域名之外一个都不问：节点名、公网 IP、伪装目标、端口全用探测值与默认值。
    #[tokio::test]
    async fn nothing_but_the_domain_is_ever_asked() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let host = host_for_install(&paths);
        let mut o = opts(&d);
        (o.yes, o.non_interactive) = (false, false); // 交互模式，但域名已由 --domain 给出
        let (a, _) = collect_answers(&o, None, host.clone()).await.unwrap();
        assert_eq!(a.domain, "example.com");
        assert_eq!(a.node_name, "node-a", "节点名用 hostname，不问");
        assert_eq!(a.public_ip, "203.0.113.10", "公网 IP 用探测值，不问");
        assert_eq!(a.masquerade, "www.bing.com:443", "伪装目标用默认值，不问");
        assert_eq!(a.ports.hy2, 10000, "端口用默认值，不问");
    }

    /// 关键端口被非本栈进程占着 ⇒ 中止，不写单元、不落 `state.json`（腾了端口重跑即可）。
    #[tokio::test]
    async fn a_foreign_process_on_a_key_port_aborts_the_install() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let host = host_for_install(&paths);
        host.with(|i| {
            i.scripted.insert(
                0,
                (
                    "ss -lntupH".into(),
                    CmdOut::success(
                        "tcp LISTEN 0 511 0.0.0.0:443 0.0.0.0:* users:((\"nginx\",pid=7,fd=6))\n",
                    ),
                ),
            );
        });
        let err = run_with_wait(
            opts(&d),
            answers(),
            murl(),
            paths.clone(),
            host.clone(),
            fetcher_with_manifest(),
            crate::commands::selfcheck::Wait::NONE,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("443/tcp 被 nginx 占用"), "{err}");
        assert!(
            !crate::paths::state_file(&paths).exists(),
            "state.json 不许落盘"
        );
        assert!(
            host.text("/etc/systemd/system/b-ui.service").is_none(),
            "一个单元都不该写"
        );
    }

    /// 自检有 FAIL ⇒ 返回 [`SelfCheckFailed`]（`run` 据它退 2），但**不回滚**：
    /// 配置照旧落盘，运维按表逐项修比回到中间态强。
    #[tokio::test]
    async fn a_failed_selfcheck_is_reported_without_rolling_back() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let host = host_for_install(&paths);
        // HY2 的两个 UDP 端口没在监听 → 自检的「关键端口」那一项 FAIL
        host.with(|i| {
            i.listening
                .insert(crate::sys::Proto::Udp, Default::default());
        });
        let err = run_with_wait(
            opts(&d),
            answers(),
            murl(),
            paths.clone(),
            host.clone(),
            fetcher_with_manifest(),
            crate::commands::selfcheck::Wait::NONE,
        )
        .await
        .unwrap_err();
        let marked = err
            .downcast_ref::<SelfCheckFailed>()
            .expect("要能被 run 识别成「自检不过」（退 2）");
        assert_eq!(marked.0, 1, "只有关键端口那一项没过");
        assert!(
            crate::paths::state_file(&paths).exists(),
            "不回滚：state.json 留在原处"
        );
        assert!(
            host.text(&d.path().join("config.yaml").display().to_string())
                .is_some(),
            "不回滚：配置留在原处"
        );
    }

    /// 全新服务器上装完那一刻证书常常还没签下来（Caddy 的 ACME 还在跑、守护进程的证书同步
    /// 还没复制过来）：两个 hysteria 起不来、HY2 的 UDP 口没在听。这**不是**装坏了，
    /// 自检得判 SKIP，`bui install` 必须退 0 —— 否则「一行命令完成新服务器的所有安装」不成立。
    #[tokio::test]
    async fn a_fresh_install_without_a_certificate_yet_still_exits_zero() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let host = host_for_install(&paths);
        host.with(|i| {
            i.files.remove(&paths.certs_dir.join("fullchain.pem"));
            i.units_active.remove("hysteria-server.service");
            i.units_active.remove("hysteria-residential.service");
            i.listening
                .insert(crate::sys::Proto::Udp, Default::default());
        });
        // Wait::NONE：真跑会先等 120s，这里只要证明「等不到也不 FAIL」
        run_with_wait(
            opts(&d),
            answers(),
            murl(),
            paths.clone(),
            host.clone(),
            fetcher_with_manifest(),
            crate::commands::selfcheck::Wait::NONE,
        )
        .await
        .expect("证书未到不该让装机退 2");
    }

    /// 一次性管理员密码只有装机最后一屏这一次机会：`Store::create` 之后的失败（这里用
    /// 「对账有失败项」，全新服务器上证书签发/校验器失败非常常见）也必须把它打出来。
    /// 漏打就只剩「删 state.json 整机重装」一条路——重跑 install 会走「已安装」分支，没有密码行。
    #[tokio::test]
    async fn the_one_time_password_survives_a_failure_after_the_state_landed() {
        let d = tempfile::tempdir().unwrap();
        let paths = scratch(&d);
        let host = host_for_install(&paths);
        // caddy 的 systemd 动作失败 → 对账进 report.errors → `run_with` 以「对账有失败项」退出
        host.with(|i| {
            i.fail_units.insert("caddy".into());
        });
        let err = run_with_wait(
            opts(&d),
            answers(),
            murl(),
            paths.clone(),
            host.clone(),
            fetcher_with_manifest(),
            crate::commands::selfcheck::Wait::NONE,
        )
        .await
        .unwrap_err();
        assert!(
            err.downcast_ref::<SelfCheckFailed>().is_none(),
            "这条路径不是「自检 FAIL」，正是旧判据漏掉的那一类：{err}"
        );
        let state_path = crate::paths::state_file(&paths);
        assert!(
            state_path.exists(),
            "state.json 已落盘（密码的 hash 在里面）"
        );
        let line = "已生成随机管理员密码：hunter2（只显示这一次，请立刻存好）";
        let text = final_summary(&state_path, Some(line))
            .await
            .expect("state.json 在就该有摘要");
        assert!(text.contains("hunter2"), "装到一半失败也要打出密码：{text}");
        assert!(text.contains("https://example.com/"), "{text}");
        // state.json 还没落盘（如端口被占、缺内核）时没有摘要可打：那时密码一次都没用上
        let empty = tempfile::tempdir().unwrap();
        assert!(
            final_summary(&crate::paths::state_file(&scratch(&empty)), Some(line))
                .await
                .is_none(),
            "state.json 不在就不该打摘要"
        );
    }

    #[test]
    fn the_summary_gives_the_panel_the_one_time_password_and_the_subscription_shapes() {
        let state = crate::testutil::sample_state();
        let s = summary(
            &state,
            Some("已生成随机管理员密码：hunter2（只显示这一次，请立刻存好）"),
        );
        assert!(s.contains("https://example.com/"), "{s}");
        assert!(s.contains("hunter2"), "一次性密码只在摘要里出现这一次：{s}");
        for shape in [
            "/api/sub/<用户名>",
            "/api/subscription/<用户名>",
            "/api/clash/<用户名>",
        ] {
            assert!(s.contains(shape), "订阅形状缺 {shape}：{s}");
        }
        assert!(s.contains("`b-ui`") && s.contains("bui status"), "{s}");
        // 已装机重跑（没有一次性密码）时不出现「管理员」那一行
        assert!(!summary(&state, None).contains("管理员"));
    }
}
