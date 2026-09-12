//! 住宅上游的网络探测（spec §5.2 / §5.4）：经上游开隧道（HTTP CONNECT / SOCKS5）、
//! 经上游发 HTTPS GET、SOCKS5 UDP ASSOCIATE，以及**不经上游**的直连 TCP 对照。
//!
//! 为什么自己写握手、不 exec curl、也不只用 `reqwest`：`reqwest` 不暴露 CONNECT 的响应
//! 状态码（失败只给一个 `reqwest::Error`），而 CONNECT 的 4xx/5xx 与 407 恰恰是「目标被
//! 硬拒」与「凭据失效」的唯一区分依据（调研 §D）。所以 [`Prober::connect`] /
//! [`Prober::udp_associate`] 用 `std::net::TcpStream` 自己发 CONNECT / SOCKS5 握手；
//! [`Prober::get`] 不需要这层细节，用 `reqwest::blocking` + `Proxy::basic_auth`。

use bui_schema::model::{Upstream, UpstreamKind};

/// 一次「经该上游对某个 host:port 开隧道」的结果。这四个变体是黑名单判定与体检的全部依据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectVerdict {
    /// 隧道建立：HTTP 上游 CONNECT 2xx，或 SOCKS5 REP = 0x00
    Open,
    /// **硬拒**：HTTP 上游 CONNECT 4xx/5xx（不含 407），或 SOCKS5 REP ≠ 0x00。
    /// `code` = HTTP 状态码，或 SOCKS5 的 REP 值（0x02 = not allowed by ruleset）
    Refused { code: u16 },
    /// 凭据失效：CONNECT 407，或 SOCKS5 用户名密码认证被拒（RFC1929 STATUS ≠ 0）。
    /// 调研 §D 的 407 场景：**整条上游不可用**，不是「这个目标被拒」
    AuthFailed,
    /// 连不上上游本身 / 超时 / 协议对不上
    Unreachable { detail: String },
}

impl ConnectVerdict {
    pub fn is_hard_reject(&self) -> bool {
        matches!(self, ConnectVerdict::Refused { .. })
    }

    pub fn label(&self) -> String {
        match self {
            ConnectVerdict::Open => "open".into(),
            ConnectVerdict::Refused { code } => format!("refused:{code}"),
            ConnectVerdict::AuthFailed => "auth_failed".into(),
            ConnectVerdict::Unreachable { .. } => "unreachable".into(),
        }
    }
}

/// 经上游发一次 HTTPS GET 的结果（不跟随重定向，只要状态码与前 64KB 正文）
#[derive(Debug, Clone, PartialEq)]
pub struct HttpProbe {
    pub status: u16,
    pub body: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("上游凭据失效（CONNECT 407 / SOCKS5 认证被拒）")]
    AuthFailed,
    #[error("上游不可用：{0}")]
    Unreachable(String),
}

/// 所有碰网络的操作都走这里；**同步** trait，调用点一律在 `tokio::task::spawn_blocking`
/// 里（`reqwest::blocking` 在 async 上下文会 panic，与 P1 的 `Fetcher` 同一条铁律）。
pub trait Prober: Send + Sync + 'static {
    /// 经上游对 `host:port` 开一次隧道（HTTP → CONNECT；SOCKS5 → CMD=CONNECT、ATYP=域名）
    fn connect(&self, up: &Upstream, host: &str, port: u16) -> ConnectVerdict;
    /// 经上游发一次 HTTPS GET
    fn get(&self, up: &Upstream, url: &str) -> Result<HttpProbe, ProbeError>;
    /// 经上游做一次 SOCKS5 UDP ASSOCIATE；HTTP 上游一律 `Ok(false)`（协议里没这回事）
    fn udp_associate(&self, up: &Upstream) -> Result<bool, ProbeError>;
    /// **不经上游**的直连 TCP 对照（黑名单确认的第二条件：直连同目标可达）
    fn direct_tcp(&self, host: &str, port: u16) -> bool;
}

/// 目标主机名合法性（`\r\n` / 引号 / 空格 / 空串一律拒，绝不让它进请求行）
pub fn valid_target_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'[' | b']'))
}

/// `reqwest::Proxy` 的 URL（**不含凭据**，凭据走 `Proxy::basic_auth`）
pub fn proxy_url(up: &Upstream) -> String {
    let scheme = match up.kind {
        UpstreamKind::Http => "http",
        // socks5h = 远端解析域名（目标域名原样交上游，v3.6.0 R10 的结论）
        UpstreamKind::Socks5 => "socks5h",
    };
    format!("{scheme}://{}:{}", up.host, up.port)
}

/// CONNECT 请求首部（`Proxy-Authorization: Basic` 用 base64）
pub fn connect_request(up: &Upstream, host: &str, port: u16) -> Vec<u8> {
    use base64::Engine as _;
    let cred = base64::engine::general_purpose::STANDARD
        .encode(format!("{}:{}", up.username, up.password));
    format!(
        "CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n\
         Proxy-Authorization: Basic {cred}\r\nProxy-Connection: keep-alive\r\n\r\n"
    )
    .into_bytes()
}

/// CONNECT 响应首行 → 判定（`HTTP/1.1 403 Forbidden serp domain` → `Refused{403}`）
pub fn parse_connect_status(first_line: &str) -> ConnectVerdict {
    let mut parts = first_line.split_whitespace();
    let ver = parts.next().unwrap_or_default();
    let code: u16 = match parts.next().and_then(|c| c.parse().ok()) {
        Some(c) if ver.starts_with("HTTP/") => c,
        _ => {
            return ConnectVerdict::Unreachable {
                detail: "CONNECT 响应不是 HTTP 状态行".into(),
            }
        }
    };
    match code {
        200..=299 => ConnectVerdict::Open,
        // 调研 §D：407 是凭据失效，整条上游不可用；绝不能当成「这个目标被拒」进黑名单
        407 => ConnectVerdict::AuthFailed,
        400..=599 => ConnectVerdict::Refused { code },
        _ => ConnectVerdict::Unreachable {
            detail: format!("CONNECT 返回 {code}"),
        },
    }
}

/// SOCKS5 CONNECT 回复的 REP 字节 → 判定
pub fn socks_verdict(rep: u8) -> ConnectVerdict {
    match rep {
        0x00 => ConnectVerdict::Open,
        // 0x02 not allowed by ruleset / 0x05 connection refused / 0x03 network unreachable…
        // 一律算硬拒：这是「上游明确回绝了这个目标」，与 TCP 层连不上（Unreachable）不同
        r => ConnectVerdict::Refused { code: u16::from(r) },
    }
}

/// 「这条 `reqwest` 错误链的文字里有没有代理鉴权失败的迹象」（纯函数，喂整条
/// `source` 链拼出来的字符串）：命中 `407` / `proxy authentication`（大小写不敏感）
/// 即为真。[`ReqwestProber::get`] 用它做**第一层**判定。
pub fn looks_like_proxy_auth(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    // reqwest / hyper 在 CONNECT 阶段拿到 407 时，错误链里带的是状态行或这句标准原因短语
    m.contains("407") || m.contains("proxy authentication")
}

/// `get` 失败后的**补判**：经该上游对 `host:443` 自写一次 CONNECT / SOCKS5 握手，
/// 只为把「凭据失效」从「不可达」里分出来。返回 `true` = 上游拒绝鉴权。
///
/// **为什么必须补判**：所有探测 URL 都是 `https://`，经 HTTP 上游走的是 CONNECT 隧道，
/// 407 出现在**隧道建立阶段**——`reqwest` 把它作为 `Err(reqwest::Error)` 返回，
/// 调用方永远拿不到一个「状态码 = 407」的 `Response`。若只看 `get` 的返回值，
/// 生产上凭据失效会一律报成 `Unreachable`：巡检仍判不健康（spec §5.4 的「407 视为不健康」
/// 成立），但「凭据失效，请更新凭据」这条告警**永远不会出现**。
/// [`Prober::connect`] 是本模块唯一能读到 CONNECT 状态码与 SOCKS5 认证 STATUS 的路径。
pub fn confirm_auth_failure(p: &dyn Prober, up: &Upstream, host: &str) -> bool {
    // 443：探测 URL 全是 https，补判必须走同一个端口，否则上游的端口白名单
    // （调研 §D：只放行 80/443）会把补判本身判成硬拒，得出「不是凭据问题」的错结论
    matches!(p.connect(up, host, 443), ConnectVerdict::AuthFailed)
}

pub struct ReqwestProber {
    timeout: std::time::Duration,
}

impl ReqwestProber {
    pub fn new() -> Self {
        Self::with_timeout(super::PROBE_TIMEOUT_SECS)
    }

    pub fn with_timeout(secs: u64) -> Self {
        Self {
            timeout: std::time::Duration::from_secs(secs),
        }
    }

    fn dial(&self, up: &Upstream) -> std::io::Result<std::net::TcpStream> {
        use std::net::ToSocketAddrs;
        let mut last = std::io::Error::other("上游地址解析为空");
        for a in (up.host.as_str(), up.port).to_socket_addrs()? {
            match std::net::TcpStream::connect_timeout(&a, self.timeout) {
                Ok(s) => {
                    s.set_read_timeout(Some(self.timeout))?;
                    s.set_write_timeout(Some(self.timeout))?;
                    return Ok(s);
                }
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    /// RFC7231 §4.3.6：CONNECT + `Proxy-Authorization`，只读响应首行
    fn http_connect(
        &self,
        up: &Upstream,
        host: &str,
        port: u16,
    ) -> std::io::Result<ConnectVerdict> {
        use std::io::{BufRead, BufReader, Write};
        let mut s = self.dial(up)?;
        s.write_all(&connect_request(up, host, port))?;
        s.flush()?;
        let mut line = String::new();
        BufReader::new(&s).read_line(&mut line)?;
        Ok(parse_connect_status(line.trim_end()))
    }

    /// RFC1928 + RFC1929：greeting(05 01 02) → 用户名密码认证 → CMD=01、ATYP=03（域名）
    fn socks5_connect(
        &self,
        up: &Upstream,
        host: &str,
        port: u16,
    ) -> std::io::Result<ConnectVerdict> {
        let mut s = self.dial(up)?;
        match socks5_handshake(&mut s, up)? {
            ConnectVerdict::Open => {}
            other => return Ok(other),
        }
        let mut req = vec![0x05, 0x01, 0x00, 0x03, host.len() as u8];
        req.extend_from_slice(host.as_bytes());
        req.extend_from_slice(&port.to_be_bytes());
        socks5_request(&mut s, &req)
    }

    /// RFC1928 §4：CMD=03（UDP ASSOCIATE），绑定地址 0.0.0.0:0
    fn socks5_udp(&self, up: &Upstream) -> Result<bool, ProbeError> {
        let mut s = self
            .dial(up)
            .map_err(|e| ProbeError::Unreachable(e.to_string()))?;
        let v = socks5_handshake(&mut s, up).map_err(|e| ProbeError::Unreachable(e.to_string()))?;
        if v == ConnectVerdict::AuthFailed {
            return Err(ProbeError::AuthFailed);
        }
        let req = [0x05, 0x03, 0x00, 0x01, 0, 0, 0, 0, 0, 0];
        match socks5_request(&mut s, &req).map_err(|e| ProbeError::Unreachable(e.to_string()))? {
            ConnectVerdict::Open => Ok(true),
            ConnectVerdict::AuthFailed => Err(ProbeError::AuthFailed),
            _ => Ok(false),
        }
    }
}

impl Default for ReqwestProber {
    fn default() -> Self {
        Self::new()
    }
}

/// greeting + RFC1929 认证；认证被拒 → `AuthFailed`（凭据失效，不是目标被拒）
fn socks5_handshake(s: &mut std::net::TcpStream, up: &Upstream) -> std::io::Result<ConnectVerdict> {
    use std::io::{Read, Write};
    // 只声明 0x02（用户名密码）：住宅上游一律要鉴权，声明 0x00 只会让错配更难发现
    s.write_all(&[0x05, 0x01, 0x02])?;
    let mut sel = [0u8; 2];
    s.read_exact(&mut sel)?;
    if sel[1] != 0x02 {
        return Ok(ConnectVerdict::Unreachable {
            detail: format!("上游不接受用户名密码认证（METHOD=0x{:02x}）", sel[1]),
        });
    }
    let (u, p) = (up.username.as_bytes(), up.password.as_bytes());
    let mut auth = vec![0x01, u.len() as u8];
    auth.extend_from_slice(u);
    auth.push(p.len() as u8);
    auth.extend_from_slice(p);
    s.write_all(&auth)?;
    let mut st = [0u8; 2];
    s.read_exact(&mut st)?;
    Ok(if st[1] == 0 {
        ConnectVerdict::Open
    } else {
        ConnectVerdict::AuthFailed
    })
}

/// 发一条 SOCKS5 请求并读回 REP 字节（前 4 字节即够判定，BND 地址不读）
fn socks5_request(s: &mut std::net::TcpStream, req: &[u8]) -> std::io::Result<ConnectVerdict> {
    use std::io::{Read, Write};
    s.write_all(req)?;
    s.flush()?;
    let mut head = [0u8; 4];
    s.read_exact(&mut head)?;
    Ok(socks_verdict(head[1]))
}

impl Prober for ReqwestProber {
    fn connect(&self, up: &Upstream, host: &str, port: u16) -> ConnectVerdict {
        if !valid_target_host(host) {
            return ConnectVerdict::Unreachable {
                detail: "目标主机名非法".into(),
            };
        }
        let r = match up.kind {
            UpstreamKind::Http => self.http_connect(up, host, port),
            UpstreamKind::Socks5 => self.socks5_connect(up, host, port),
        };
        match r {
            Ok(v) => v,
            Err(e) => {
                // url 里本来只有 host:port，仍统一过 redact，防后人把凭据拼进来
                tracing::debug!(
                    upstream = %crate::redact::url_credentials(&proxy_url(up)),
                    target = %host,
                    error = %e,
                    "隧道探测失败"
                );
                ConnectVerdict::Unreachable {
                    detail: e.to_string(),
                }
            }
        }
    }

    fn get(&self, up: &Upstream, url: &str) -> Result<HttpProbe, ProbeError> {
        let proxy = reqwest::Proxy::all(proxy_url(up))
            .map_err(|e| ProbeError::Unreachable(e.to_string()))?
            .basic_auth(&up.username, &up.password);
        let client = reqwest::blocking::Client::builder()
            .proxy(proxy)
            .timeout(self.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| ProbeError::Unreachable(e.to_string()))?;
        let resp = client.get(url).send().map_err(|e| {
            tracing::debug!(url = %crate::redact::url_credentials(url), error = %e, "经上游 GET 失败");
            // https 目标经 HTTP 上游走 CONNECT 隧道，407 在隧道建立阶段就失败了 ⇒
            // reqwest 只给一个 Err，拿不到状态码。把整条 source 链的文字拼出来扫一遍，
            // 这是 `get` 自己唯一能识别凭据失效的办法（第二层补判在调用方，见
            // `confirm_auth_failure` 的注释）。
            let mut chain = e.to_string();
            let mut src: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(&e);
            while let Some(s) = src {
                chain.push_str("; ");
                chain.push_str(&s.to_string());
                src = s.source();
            }
            if looks_like_proxy_auth(&chain) {
                ProbeError::AuthFailed
            } else {
                ProbeError::Unreachable(e.to_string())
            }
        })?;
        let status = resp.status().as_u16();
        // 只有「明文 HTTP 目标」才会走到这里（本模块的探测 URL 全是 https，407 走上面
        // 那条 Err 分支）。留着是为了将来真加明文目标时不必再想一遍。
        if status == 407 {
            return Err(ProbeError::AuthFailed);
        }
        let body: String = resp
            .text()
            .unwrap_or_default()
            .chars()
            .take(65_536)
            .collect();
        Ok(HttpProbe { status, body })
    }

    fn udp_associate(&self, up: &Upstream) -> Result<bool, ProbeError> {
        if up.kind == UpstreamKind::Http {
            return Ok(false); // HTTP 代理协议里没有 UDP ASSOCIATE
        }
        self.socks5_udp(up)
    }

    fn direct_tcp(&self, host: &str, port: u16) -> bool {
        use std::net::ToSocketAddrs;
        let Ok(addrs) = (host, port).to_socket_addrs() else {
            return false;
        };
        addrs
            .into_iter()
            .any(|a| std::net::TcpStream::connect_timeout(&a, self.timeout).is_ok())
    }
}

#[cfg(test)]
pub struct FakeProber {
    inner: std::sync::Mutex<FakeProberInner>,
}

#[cfg(test)]
#[derive(Default)]
pub struct FakeProberInner {
    /// key = `"<host>:<port>"`，缺省 `Open`
    pub connects: std::collections::BTreeMap<String, ConnectVerdict>,
    /// key = URL，缺省 `Err(Unreachable("no route"))`。**错误哨兵约定**（跨任务契约，
    /// T6/T7/T8 的测试都依赖它）：值为 `Err("__auth_failed__")` 时 `get` 返回
    /// `ProbeError::AuthFailed`，其余字符串返回 `ProbeError::Unreachable(那个字符串)`
    pub gets: std::collections::BTreeMap<String, Result<HttpProbe, String>>,
    pub udp: bool,
    /// 直连可达的 `"<host>:<port>"`；不在集合里即不可达
    pub direct: std::collections::BTreeSet<String>,
    pub calls: Vec<String>,
}

#[cfg(test)]
impl FakeProber {
    pub fn new() -> Self {
        Self {
            inner: std::sync::Mutex::new(FakeProberInner::default()),
        }
    }

    pub fn with(&self, f: impl FnOnce(&mut FakeProberInner)) -> &Self {
        f(&mut self.inner.lock().expect("FakeProber 锁被毒化"));
        self
    }

    pub fn calls(&self) -> Vec<String> {
        self.inner
            .lock()
            .expect("FakeProber 锁被毒化")
            .calls
            .clone()
    }

    pub fn clear_calls(&self) {
        self.inner
            .lock()
            .expect("FakeProber 锁被毒化")
            .calls
            .clear();
    }
}

#[cfg(test)]
impl Default for FakeProber {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl Prober for FakeProber {
    fn connect(&self, _up: &Upstream, host: &str, port: u16) -> ConnectVerdict {
        let mut i = self.inner.lock().expect("FakeProber 锁被毒化");
        i.calls.push(format!("connect:{host}:{port}"));
        i.connects
            .get(&format!("{host}:{port}"))
            .cloned()
            .unwrap_or(ConnectVerdict::Open)
    }

    fn get(&self, _up: &Upstream, url: &str) -> Result<HttpProbe, ProbeError> {
        let mut i = self.inner.lock().expect("FakeProber 锁被毒化");
        i.calls.push(format!("get:{url}"));
        match i.gets.get(url) {
            Some(Ok(hp)) => Ok(hp.clone()),
            // 哨兵：让测试能构造「上游凭据失效」这一条路径（T7 的 407 用例）
            Some(Err(e)) if e == "__auth_failed__" => Err(ProbeError::AuthFailed),
            Some(Err(e)) => Err(ProbeError::Unreachable(e.clone())),
            None => Err(ProbeError::Unreachable("no route".into())),
        }
    }

    fn udp_associate(&self, _up: &Upstream) -> Result<bool, ProbeError> {
        let mut i = self.inner.lock().expect("FakeProber 锁被毒化");
        i.calls.push("udp".into());
        Ok(i.udp)
    }

    fn direct_tcp(&self, host: &str, port: u16) -> bool {
        let mut i = self.inner.lock().expect("FakeProber 锁被毒化");
        i.calls.push(format!("direct:{host}:{port}"));
        i.direct.contains(&format!("{host}:{port}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bui_schema::model::{Upstream, UpstreamKind};
    use pretty_assertions::assert_eq;
    use uuid::Uuid;

    fn up(kind: UpstreamKind) -> Upstream {
        Upstream {
            id: Uuid::nil(),
            name: "url-1".into(),
            kind,
            host: "isp.example.net".into(),
            port: 10007,
            username: "user1".into(),
            password: "pw1".into(),
            priority: 10,
            provider: None,
            region: None,
            ports_allowed: None,
            verified: None,
        }
    }

    #[test]
    fn connect_status_line_maps_to_the_four_verdicts() {
        assert_eq!(
            parse_connect_status("HTTP/1.1 200 Connection established"),
            ConnectVerdict::Open
        );
        // 调研 §D 实测形态：Decodo 的硬拒就是 CONNECT 403
        assert_eq!(
            parse_connect_status("HTTP/1.1 403 Forbidden serp domain"),
            ConnectVerdict::Refused { code: 403 }
        );
        assert_eq!(
            parse_connect_status("HTTP/1.1 502 Bad Gateway"),
            ConnectVerdict::Refused { code: 502 }
        );
        // 407 是凭据失效，不是目标被拒：不能进黑名单，要把整条上游判不健康
        assert_eq!(
            parse_connect_status("HTTP/1.1 407 Proxy Authentication Required"),
            ConnectVerdict::AuthFailed
        );
        assert!(matches!(
            parse_connect_status("garbage"),
            ConnectVerdict::Unreachable { .. }
        ));
        assert!(matches!(
            parse_connect_status(""),
            ConnectVerdict::Unreachable { .. }
        ));
    }

    #[test]
    fn socks5_reply_codes_map_to_the_four_verdicts() {
        assert_eq!(socks_verdict(0x00), ConnectVerdict::Open);
        assert_eq!(
            socks_verdict(0x02),
            ConnectVerdict::Refused { code: 2 },
            "not allowed by ruleset"
        );
        assert_eq!(
            socks_verdict(0x05),
            ConnectVerdict::Refused { code: 5 },
            "connection refused"
        );
        assert!(ConnectVerdict::Refused { code: 2 }.is_hard_reject());
        assert!(
            !ConnectVerdict::AuthFailed.is_hard_reject(),
            "凭据失效不是目标被拒"
        );
        assert!(!ConnectVerdict::Unreachable { detail: "x".into() }.is_hard_reject());
        assert_eq!(ConnectVerdict::Refused { code: 403 }.label(), "refused:403");
        assert_eq!(ConnectVerdict::AuthFailed.label(), "auth_failed");
    }

    #[test]
    fn proxy_url_never_carries_credentials() {
        assert_eq!(
            proxy_url(&up(UpstreamKind::Http)),
            "http://isp.example.net:10007"
        );
        // socks5h = 远端解析域名（目标域名原样交上游，v3.6.0 R10 的结论）
        assert_eq!(
            proxy_url(&up(UpstreamKind::Socks5)),
            "socks5h://isp.example.net:10007"
        );
        assert!(!proxy_url(&up(UpstreamKind::Http)).contains("pw1"));
    }

    #[test]
    fn connect_request_has_basic_auth_and_a_clean_request_line() {
        let req = String::from_utf8(connect_request(
            &up(UpstreamKind::Http),
            "pay.google.com",
            443,
        ))
        .unwrap();
        assert!(
            req.starts_with("CONNECT pay.google.com:443 HTTP/1.1\r\n"),
            "实际 {req:?}"
        );
        assert!(req.contains("Host: pay.google.com:443\r\n"));
        // base64("user1:pw1")
        assert!(
            req.contains("Proxy-Authorization: Basic dXNlcjE6cHcx\r\n"),
            "实际 {req:?}"
        );
        assert!(req.ends_with("\r\n\r\n"));
    }

    #[test]
    fn target_host_validation_blocks_request_line_injection() {
        assert!(valid_target_host("gateway.icloud.com"));
        assert!(valid_target_host("198.51.100.7"));
        assert!(!valid_target_host(""));
        assert!(!valid_target_host("a.com\r\nX-Evil: 1"));
        assert!(!valid_target_host("a.com b.com"));
        assert!(!valid_target_host("a\"b.com"));
    }

    #[test]
    fn fake_prober_defaults_are_open_and_calls_are_recorded() {
        let p = FakeProber::new();
        p.with(|i| {
            i.connects.insert(
                "pay.google.com:443".into(),
                ConnectVerdict::Refused { code: 403 },
            );
            i.direct.insert("pay.google.com:443".into());
        });
        assert_eq!(
            p.connect(&up(UpstreamKind::Http), "www.google.com", 443),
            ConnectVerdict::Open
        );
        assert_eq!(
            p.connect(&up(UpstreamKind::Http), "pay.google.com", 443),
            ConnectVerdict::Refused { code: 403 }
        );
        assert!(p.direct_tcp("pay.google.com", 443));
        assert!(!p.direct_tcp("www.google.com", 443));
        assert_eq!(
            p.calls(),
            vec![
                "connect:www.google.com:443",
                "connect:pay.google.com:443",
                "direct:pay.google.com:443",
                "direct:www.google.com:443",
            ]
        );
        // 错误哨兵约定（跨任务契约）：T7 的 `add_rejects_a_407…` 与 T8 的 407 用例都吃它
        p.with(|i| {
            i.gets.insert(
                "https://auth.example.com".into(),
                Err("__auth_failed__".into()),
            );
            i.gets
                .insert("https://boom.example.com".into(), Err("boom".into()));
        });
        assert!(matches!(
            p.get(&up(UpstreamKind::Http), "https://auth.example.com"),
            Err(ProbeError::AuthFailed)
        ));
        assert!(matches!(
            p.get(&up(UpstreamKind::Http), "https://boom.example.com"),
            Err(ProbeError::Unreachable(ref s)) if s == "boom"
        ));
        assert!(
            matches!(
                p.get(&up(UpstreamKind::Http), "https://unmapped.example.com"),
                Err(ProbeError::Unreachable(_))
            ),
            "未登记的 URL 缺省不可达"
        );
        assert!(
            !p.udp_associate(&up(UpstreamKind::Socks5)).unwrap(),
            "inner.udp 默认 false"
        );
    }

    #[test]
    fn a_407_is_recognised_from_the_error_text_and_confirmed_by_a_connect_probe() {
        // 第一层：reqwest 在 CONNECT 阶段的 407 只留下文字（没有 Response 可读状态码）
        assert!(looks_like_proxy_auth(
            "error following redirect: HTTP/1.1 407 Proxy Authentication Required"
        ));
        assert!(looks_like_proxy_auth(
            "tunnel failed: Proxy Authentication Required"
        ));
        assert!(
            looks_like_proxy_auth("PROXY AUTHENTICATION required"),
            "大小写不敏感"
        );
        assert!(!looks_like_proxy_auth(
            "dns error: failed to lookup address"
        ));
        assert!(!looks_like_proxy_auth("connection refused"));
        // 第二层：文字里没有线索时，经上游对 host:443 自写一次 CONNECT 补判
        let p = FakeProber::new();
        p.with(|i| {
            i.connects
                .insert("www.gstatic.com:443".into(), ConnectVerdict::AuthFailed);
            i.connects.insert(
                "api.ipify.org:443".into(),
                ConnectVerdict::Refused { code: 403 },
            );
        });
        let u = up(UpstreamKind::Http);
        assert!(
            confirm_auth_failure(&p, &u, "www.gstatic.com"),
            "CONNECT 407 ⇒ 凭据失效"
        );
        assert!(
            !confirm_auth_failure(&p, &u, "api.ipify.org"),
            "403 是目标被硬拒，不是凭据问题"
        );
        assert!(
            !confirm_auth_failure(&p, &u, "open.example.com"),
            "缺省 Open ⇒ 不是凭据问题"
        );
        assert_eq!(
            p.calls(),
            vec![
                "connect:www.gstatic.com:443",
                "connect:api.ipify.org:443",
                "connect:open.example.com:443"
            ],
            "补判必须走 443：探测 URL 全是 https，换端口会撞上游的端口白名单"
        );
    }

    /// 进程内假代理：绑 `127.0.0.1:0`，接一条连接，按 `script` 逐段「读一段 → 回一段」，
    /// 返回它收到的全部字节供断言。
    fn fake_proxy(script: Vec<Vec<u8>>) -> (u16, std::thread::JoinHandle<Vec<u8>>) {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut seen = Vec::new();
            for chunk in script {
                let mut buf = [0u8; 1024];
                let n = s.read(&mut buf).unwrap_or(0);
                seen.extend_from_slice(&buf[..n]);
                if s.write_all(&chunk).is_err() {
                    break;
                }
                let _ = s.flush();
            }
            seen
        });
        (port, h)
    }

    fn local(kind: UpstreamKind, port: u16) -> Upstream {
        let mut u = up(kind);
        u.host = "127.0.0.1".into();
        u.port = port;
        u
    }

    #[test]
    fn the_http_connect_handshake_reads_the_real_status_line() {
        let (port, h) = fake_proxy(vec![b"HTTP/1.1 403 Forbidden serp domain\r\n\r\n".to_vec()]);
        let p = ReqwestProber::with_timeout(2);
        assert_eq!(
            p.connect(&local(UpstreamKind::Http, port), "pay.google.com", 443),
            ConnectVerdict::Refused { code: 403 },
            "调研 §D 的硬拒形态"
        );
        let req = String::from_utf8(h.join().unwrap()).unwrap();
        assert!(
            req.starts_with("CONNECT pay.google.com:443 HTTP/1.1\r\n"),
            "实际 {req:?}"
        );
        assert!(
            req.contains("Proxy-Authorization: Basic dXNlcjE6cHcx\r\n"),
            "实际 {req:?}"
        );
    }

    #[test]
    fn the_socks5_handshake_maps_rep_bytes_and_auth_rejection() {
        // greeting → (05,02)；RFC1929 认证 → (01,00)；CONNECT → REP=0x02
        let (port, h) = fake_proxy(vec![
            vec![0x05, 0x02],
            vec![0x01, 0x00],
            vec![0x05, 0x02, 0x00, 0x01, 0, 0, 0, 0, 0, 0],
        ]);
        let p = ReqwestProber::with_timeout(2);
        assert_eq!(
            p.connect(
                &local(UpstreamKind::Socks5, port),
                "gateway.icloud.com",
                443
            ),
            ConnectVerdict::Refused { code: 2 },
            "REP=0x02 not allowed by ruleset ⇒ 硬拒"
        );
        let seen = h.join().unwrap();
        assert_eq!(&seen[..3], &[0x05, 0x01, 0x02], "只声明用户名密码认证");
        assert!(
            seen.windows(5).any(|w| w == b"user1"),
            "RFC1929 里带了用户名"
        );
        assert!(
            seen.windows(18).any(|w| w == b"gateway.icloud.com"),
            "ATYP=03 把域名原样交上游"
        );

        // 认证被拒（STATUS ≠ 0）⇒ AuthFailed，不是「这个目标被拒」
        let (port2, h2) = fake_proxy(vec![vec![0x05, 0x02], vec![0x01, 0x01]]);
        assert_eq!(
            p.connect(
                &local(UpstreamKind::Socks5, port2),
                "gateway.icloud.com",
                443
            ),
            ConnectVerdict::AuthFailed
        );
        h2.join().unwrap();

        // UDP ASSOCIATE 成功（CMD=03、REP=0x00）
        let (port3, h3) = fake_proxy(vec![
            vec![0x05, 0x02],
            vec![0x01, 0x00],
            vec![0x05, 0x00, 0x00, 0x01, 127, 0, 0, 1, 0x04, 0x38],
        ]);
        assert!(p
            .udp_associate(&local(UpstreamKind::Socks5, port3))
            .unwrap());
        let seen3 = h3.join().unwrap();
        assert!(
            seen3.windows(2).any(|w| w == [0x05, 0x03]),
            "CMD=03 才是 UDP ASSOCIATE"
        );
    }

    #[test]
    fn an_upstream_that_never_answers_is_unreachable_not_refused() {
        // 绑了但不 accept：连上后读超时 ⇒ Unreachable。这条必须区分清楚，
        // 否则网络抖动会被当成硬拒写进黑名单。
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let p = ReqwestProber::with_timeout(1);
        let v = p.connect(&local(UpstreamKind::Http, port), "gateway.icloud.com", 443);
        assert!(
            matches!(v, ConnectVerdict::Unreachable { .. }),
            "实际 {v:?}"
        );
        assert!(!v.is_hard_reject());
        drop(l);
    }
}
