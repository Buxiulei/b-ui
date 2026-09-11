//! v3 状态导入：把 `/opt/b-ui` 下 v3 的散装配置读成一份 [`State`]（spec §2.3、§4.1）。
//!
//! 只读不写：`import` 不碰 v3 文件，也不做卸载动作（卸载在 P1 的 `bui import-v3`）。
use crate::model::*;
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHasher, SaltString};
use argon2::Argon2;
use rand::RngCore;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// v3 没有记录时间的字段（如上游体检时间）用的占位时间戳。
const EPOCH: &str = "1970-01-01T00:00:00Z";
/// `config-residential.yaml` 缺失时的住宅监听端口。
const DEFAULT_RESI_PORT: u16 = 40000;
/// `config-residential.yaml` 缺失时的住宅端口跳跃区间。
const DEFAULT_RESI_HOP: (u16, u16) = (41000, 50000);
/// `admin.env` 里没有 `ADMIN_PORT` 时的面板端口。
const DEFAULT_ADMIN_PORT: u16 = 8080;

/// 导入结果：期望态 + 需要主理人知道的降级说明。
#[derive(Debug, Clone)]
pub struct ImportReport {
    /// 导入出的期望态。
    pub state: State,
    /// 可选文件缺失等降级情况的中文说明（fixture 齐全时为空）。
    pub warnings: Vec<String>,
}

/// 导入失败的原因。
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    /// 读文件失败。
    #[error("读取 v3 文件失败: {0}")]
    Io(#[from] std::io::Error),
    /// JSON 解析失败。
    #[error("解析 v3 JSON 失败: {0}")]
    Json(#[from] serde_json::Error),
    /// 必需文件缺失。
    #[error("缺少 v3 必需文件: {0}")]
    Missing(&'static str),
    /// 内容不符合 v3 约定，无法映射到 v4。
    #[error("v3 内容无法导入: {0}")]
    Invalid(String),
}

/// 从一个 v3 安装目录（通常 `/opt/b-ui`）导入期望态。
pub fn import(dir: &Path) -> Result<ImportReport, ImportError> {
    let mut warnings = Vec::new();

    let users_raw = read_required(dir, "users.json")?;
    let v3_users: Vec<V3User> = serde_json::from_str(&users_raw)?;
    let keys_raw = read_required(dir, "reality-keys.json")?;
    let keys: V3RealityKeys = serde_json::from_str(&keys_raw)?;
    let xray_raw = read_required(dir, "xray-config.json")?;
    let xray: serde_json::Value = serde_json::from_str(&xray_raw)?;
    let direct_yaml = read_required(dir, "config.yaml")?;
    let domain = read_required(dir, "certs/.domain")?.trim().to_string();

    // hy2 直连：listen 行是端口与端口跳跃的真源（与 web/server.js 同解析）
    let (hy2, hy2_hop) = parse_listen(&direct_yaml)
        .ok_or_else(|| ImportError::Invalid("config.yaml 里没有可解析的 listen: 行".into()))?;

    // hy2 住宅：配置可缺（老安装还没有住宅实例），缺则用 v3 固定值
    let (hy2_resi, hy2_resi_hop) = match read_optional(dir, "config-residential.yaml")? {
        Some(yaml) => match parse_listen(&yaml) {
            Some((port, hop)) => (port, hop.unwrap_or(DEFAULT_RESI_HOP)),
            None => {
                return Err(ImportError::Invalid(
                    "config-residential.yaml 里没有可解析的 listen: 行".into(),
                ))
            }
        },
        None => {
            warnings.push(format!(
                "缺少 config-residential.yaml，住宅监听按 v3 默认值 {}/{}-{} 导入",
                DEFAULT_RESI_PORT, DEFAULT_RESI_HOP.0, DEFAULT_RESI_HOP.1
            ));
            (DEFAULT_RESI_PORT, DEFAULT_RESI_HOP)
        }
    };

    let reality_direct = xray_inbound_port(&xray, "vless-direct")?;
    let reality_resi = xray_inbound_port(&xray, "vless-residential")?;
    let reality = reality_from_xray(&xray, &keys)?;
    let obfs = parse_obfs(&direct_yaml);

    // admin.env 可缺（迁移前的老安装），缺则给一个随机管理员密码
    let admin_env = match read_optional(dir, "admin.env")? {
        Some(text) => parse_env(&text),
        None => {
            warnings.push("缺少 admin.env，已生成随机管理员密码，请用 CLI 重设后再登录面板".into());
            BTreeMap::new()
        }
    };
    let admin_password = match admin_env.get("ADMIN_PASSWORD") {
        Some(pw) if !pw.is_empty() => pw.clone(),
        _ => random_hex(16),
    };
    let admin_port = admin_env
        .get("ADMIN_PORT")
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(DEFAULT_ADMIN_PORT);

    // 住宅池可缺（没启用过住宅），缺则留空的默认分组
    let residential = match read_optional(dir, "residential-proxy.json")? {
        Some(raw) => {
            let v3_resi: V3Residential = serde_json::from_str(&raw)?;
            residential_from_v3(v3_resi)
        }
        None => Residential::default(),
    };

    // 订阅用 IP literal 连接，v3 把公网 IP 记在 server_ip.txt；缺则留空由装机阶段现场探测
    let public_ip = read_optional(dir, "server_ip.txt")?
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    let users = v3_users
        .into_iter()
        .map(user_from_v3)
        .collect::<Result<Vec<_>, _>>()?;

    let state = State {
        schema_version: SCHEMA_VERSION,
        node: NodeParams {
            id: Uuid::new_v4(),
            name: domain.clone(),
            domain,
            public_ip,
            ports: Ports {
                hy2,
                hy2_hop,
                hy2_resi,
                hy2_resi_hop,
                reality_direct,
                reality_resi,
                admin: admin_port,
            },
            reality,
            obfs,
        },
        admin: Admin {
            password_hash: hash_password(&admin_password)?,
            jwt_secret: random_hex(32),
        },
        users,
        residential,
        system: SystemSettings::default(),
        // 内核版本由 P1 装机阶段现场探测后填入
        versions: Versions::default(),
        catalog: Vec::new(),
    };

    Ok(ImportReport { state, warnings })
}

// ── v3 文件的原始结构 ────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct V3User {
    username: String,
    password: String,
    uuid: String,
    #[serde(default)]
    protocol: Option<String>,
    #[serde(default)]
    residential: Option<bool>,
    #[serde(default, rename = "createdAt")]
    created_at: Option<String>,
    #[serde(default)]
    limits: V3Limits,
    #[serde(default)]
    usage: V3Usage,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct V3Limits {
    #[serde(default)]
    expires_at: Option<String>,
    #[serde(default)]
    traffic_limit: Option<u64>,
    #[serde(default)]
    monthly_limit: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
struct V3Usage {
    #[serde(default)]
    total: u64,
    #[serde(default)]
    monthly: BTreeMap<String, u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct V3RealityKeys {
    #[serde(default)]
    private_key: String,
    #[serde(default)]
    public_key: String,
    #[serde(default)]
    short_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct V3Residential {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    global: bool,
    #[serde(default)]
    domains: Option<Vec<String>>,
    #[serde(default)]
    urls: Vec<V3Upstream>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct V3Upstream {
    host: String,
    port: u16,
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    last_verified_ip: Option<String>,
}

// ── 映射 ─────────────────────────────────────────────────────────────

fn user_from_v3(u: V3User) -> Result<User, ImportError> {
    let protocol = u.protocol.as_deref().unwrap_or("fusion");
    let protocols = match protocol {
        "fusion" => vec![Protocol::Hysteria2, Protocol::Reality],
        "hysteria2" => vec![Protocol::Hysteria2],
        "vless-reality" => vec![Protocol::Reality],
        other => {
            return Err(ImportError::Invalid(format!(
                "用户 {} 的 protocol「{}」在 v4 没有对应权益，请先在 v3 面板改成 fusion / hysteria2 / vless-reality",
                u.username, other
            )))
        }
    };
    let vless_uuid = Uuid::parse_str(&u.uuid)
        .map_err(|e| ImportError::Invalid(format!("用户 {} 的 uuid 非法: {}", u.username, e)))?;
    // v3 的 residential 缺省视为开通（web/server.js 用 `!== false` 判定）
    let residential = (u.residential != Some(false)).then(|| ResidentialEntitlement {
        group_id: DEFAULT_GROUP.to_string(),
    });
    // 取字典序最大的月份键，即导入时最近的一个月
    let (month_key, monthly_bytes) = u
        .usage
        .monthly
        .iter()
        .next_back()
        .map(|(k, v)| (k.clone(), *v))
        .unwrap_or_default();

    Ok(User {
        user_id: Uuid::new_v4(),
        username: u.username,
        note: String::new(),
        created_at: u.created_at.unwrap_or_else(|| EPOCH.to_string()),
        disabled: false,
        credentials: Credentials {
            hy2_password: u.password,
            vless_uuid,
        },
        entitlements: Entitlements {
            protocols,
            direct: true,
            residential,
            expires_at: u.limits.expires_at,
            traffic_limit: TrafficLimit {
                total_bytes: u.limits.traffic_limit,
                monthly_bytes: u.limits.monthly_limit,
            },
        },
        usage: Usage {
            total_bytes: u.usage.total,
            monthly_bytes,
            month_key,
            last_seen_at: None,
        },
        portal_auth: PortalAuth::default(),
        billing: Billing::default(),
    })
}

fn residential_from_v3(r: V3Residential) -> Residential {
    let upstreams: Vec<Upstream> = r
        .urls
        .into_iter()
        .enumerate()
        .map(|(i, u)| Upstream {
            id: Uuid::new_v4(),
            name: u.name.unwrap_or_else(|| format!("url-{}", i + 1)),
            // v3 缺省类型即 socks5
            kind: match u.kind.as_deref() {
                Some("http") => UpstreamKind::Http,
                _ => UpstreamKind::Socks5,
            },
            host: u.host,
            port: u.port,
            username: u.username,
            password: u.password,
            priority: 100,
            provider: None,
            region: None,
            ports_allowed: None,
            verified: u.last_verified_ip.map(|ip| Verified {
                ip,
                asn: None,
                org: None,
                country: None,
                at: EPOCH.to_string(),
            }),
        })
        .collect();

    let group = ResidentialGroup {
        enabled: r.enabled,
        mode: if r.global {
            ResiMode::Global
        } else {
            ResiMode::Split
        },
        keywords: r.domains.filter(|d| !d.is_empty()),
        selected_upstream_id: upstreams.first().map(|u| u.id),
        upstreams,
        blacklist: Blacklist::default(),
    };
    let mut groups = BTreeMap::new();
    groups.insert(DEFAULT_GROUP.to_string(), group);
    Residential { groups }
}

fn reality_from_xray(
    xray: &serde_json::Value,
    keys: &V3RealityKeys,
) -> Result<Reality, ImportError> {
    let rs = xray_inbound(xray, "vless-direct")?
        .get("streamSettings")
        .and_then(|v| v.get("realitySettings"))
        .ok_or_else(|| {
            ImportError::Invalid("xray-config.json 的 vless-direct 缺 realitySettings".into())
        })?;
    let dest = rs
        .get("dest")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ImportError::Invalid("xray-config.json 缺 realitySettings.dest".into()))?
        .to_string();
    let server_names: Vec<String> = rs
        .get("serverNames")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_else(|| vec![dest.split(':').next().unwrap_or(&dest).to_string()]);
    let mut short_ids: Vec<String> = rs
        .get("shortIds")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if short_ids.is_empty() {
        short_ids.extend(keys.short_id.clone().filter(|s| !s.is_empty()));
    }
    if keys.private_key.is_empty() || keys.public_key.is_empty() {
        return Err(ImportError::Invalid(
            "reality-keys.json 缺 privateKey / publicKey".into(),
        ));
    }
    Ok(Reality {
        private_key: keys.private_key.clone(),
        public_key: keys.public_key.clone(),
        short_ids,
        dest,
        server_names,
    })
}

fn xray_inbound<'a>(
    xray: &'a serde_json::Value,
    tag: &'static str,
) -> Result<&'a serde_json::Value, ImportError> {
    xray.get("inbounds")
        .and_then(|v| v.as_array())
        .and_then(|a| {
            a.iter()
                .find(|i| i.get("tag").and_then(|t| t.as_str()) == Some(tag))
        })
        .ok_or_else(|| ImportError::Invalid(format!("xray-config.json 缺 inbound {}", tag)))
}

fn xray_inbound_port(xray: &serde_json::Value, tag: &'static str) -> Result<u16, ImportError> {
    xray_inbound(xray, tag)?
        .get("port")
        .and_then(|v| v.as_u64())
        .and_then(|p| u16::try_from(p).ok())
        .ok_or_else(|| ImportError::Invalid(format!("xray inbound {} 的 port 非法", tag)))
}

// ── 文本解析 ─────────────────────────────────────────────────────────

/// 解析 hysteria 的 `listen: :PORT[,START-END]` 行（语义同 `web/server.js` 的正则）。
fn parse_listen(yaml: &str) -> Option<(u16, Option<(u16, u16)>)> {
    for line in yaml.lines() {
        let rest = match line.strip_prefix("listen:") {
            Some(r) => r.trim(),
            None => continue,
        };
        let rest = rest.strip_prefix(':').unwrap_or(rest);
        let (port_str, hop) = match rest.split_once(',') {
            Some((p, h)) => {
                let (start, end) = h.split_once('-')?;
                (
                    p,
                    Some((start.trim().parse().ok()?, end.trim().parse().ok()?)),
                )
            }
            None => (rest, None),
        };
        return Some((port_str.trim().parse().ok()?, hop));
    }
    None
}

/// 解析 `config.yaml` 顶层 `obfs:` 块（语义同 `web/server.js`：有 `type` 即算启用）。
fn parse_obfs(yaml: &str) -> Obfs {
    let mut obfs = Obfs::default();
    let mut in_block = false;
    for line in yaml.lines() {
        if line.starts_with("obfs:") {
            in_block = true;
            continue;
        }
        if !in_block {
            continue;
        }
        // 块结束：回到顶层的非空行
        if !line.starts_with(' ') && !line.starts_with('\t') && !line.trim().is_empty() {
            break;
        }
        let trimmed = line.trim();
        if let Some(v) = trimmed.strip_prefix("type:") {
            if !v.trim().is_empty() {
                obfs.enabled = true;
            }
        } else if let Some(v) = trimmed.strip_prefix("password:") {
            obfs.password = v.trim().to_string();
        }
    }
    obfs
}

/// 解析 `admin.env` 的 `KEY=VALUE` 行。
fn parse_env(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().trim_matches('"').to_string()))
        .collect()
}

// ── 辅助 ─────────────────────────────────────────────────────────────

fn path_of(dir: &Path, rel: &str) -> PathBuf {
    dir.join(rel)
}

fn read_required(dir: &Path, rel: &'static str) -> Result<String, ImportError> {
    let p = path_of(dir, rel);
    if !p.exists() {
        return Err(ImportError::Missing(rel));
    }
    Ok(std::fs::read_to_string(p)?)
}

fn read_optional(dir: &Path, rel: &str) -> Result<Option<String>, ImportError> {
    let p = path_of(dir, rel);
    if !p.exists() {
        return Ok(None);
    }
    Ok(Some(std::fs::read_to_string(p)?))
}

fn hash_password(password: &str) -> Result<String, ImportError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| ImportError::Invalid(format!("管理员密码哈希失败: {}", e)))
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
}
