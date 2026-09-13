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

/// 一次「经 socks5 上游 UDP ASSOCIATE → STUN Binding」的结果。
/// **`ok` 为真才说明 UDP 真的能用**：能建关联但收不到 Binding Response 的上游（中间
/// 设备吞 UDP）一样不可用，所以判据是「收到回复」，不是「关联建立」。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UdpProbe {
    /// 收到了 Binding Response
    pub ok: bool,
    /// XOR-MAPPED-ADDRESS 解出的 **UDP 出口 IP**（与 TCP 出口 IP 可能不同，要能对照）
    pub exit_ip: Option<String>,
    /// 往返耗时（毫秒）；`None` = 没测到（与 TCP / HTTP 延迟分开存）
    pub ms: Option<u64>,
    /// 不通的原因（`HTTP 上游无 UDP` / 超时 / 认证被拒…），面板直接显示
    pub note: Option<String>,
}

/// 一次测速的结果（Mbps）。失败**不影响健康判定**，只记 `note`（主理人口径）
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SpeedSample {
    pub down_mbps: Option<f64>,
    pub up_mbps: Option<f64>,
    pub note: Option<String>,
}

/// 传输速率（Mbps）。`bytes == 0`（一个字节都没传）或 `ms == 0`（时钟粒度不够，
/// 算出来是无穷大）一律不当成速度。
pub fn mbps(bytes: u64, ms: u64) -> Option<f64> {
    if bytes == 0 || ms == 0 {
        return None;
    }
    Some(bytes as f64 * 8.0 / (ms as f64 / 1000.0) / 1_000_000.0)
}

/// RFC 5389 §6：20 字节头（type=0x0001 Binding Request、length=0、magic cookie、
/// 12 字节 transaction id），没有属性。
pub fn stun_binding_request(tid: &[u8; 12]) -> Vec<u8> {
    let mut v = Vec::with_capacity(20);
    v.extend_from_slice(&0x0001u16.to_be_bytes());
    v.extend_from_slice(&0x0000u16.to_be_bytes());
    v.extend_from_slice(&super::STUN_MAGIC_COOKIE.to_be_bytes());
    v.extend_from_slice(tid);
    v
}

/// Binding Response → XOR-MAPPED-ADDRESS 里的 IPv4（RFC 5389 §15.2：地址与端口都跟
/// magic cookie 异或过）。**transaction id 不匹配一律不信**：UDP 没有连接语义，
/// 收到的可能是上一次关联的迟到回包。只解 IPv4（住宅出口没有 IPv6，spec §0）。
pub fn parse_stun_xor_mapped(buf: &[u8], tid: &[u8; 12]) -> Option<String> {
    if buf.len() < 20 {
        return None;
    }
    // 0x0101 = Binding Response（0x0111 是 Error Response，不算成功）
    if buf[0] != 0x01 || buf[1] != 0x01 {
        return None;
    }
    let cookie = super::STUN_MAGIC_COOKIE.to_be_bytes();
    if buf[4..8] != cookie || buf[8..20] != *tid {
        return None;
    }
    let mut i = 20;
    while i + 4 <= buf.len() {
        let kind = u16::from_be_bytes([buf[i], buf[i + 1]]);
        let len = u16::from_be_bytes([buf[i + 2], buf[i + 3]]) as usize;
        let val = buf.get(i + 4..i + 4 + len)?;
        // 0x0020 = XOR-MAPPED-ADDRESS：RSV(1) family(1) x-port(2) x-address(4)
        if kind == 0x0020 && len >= 8 && val[1] == 0x01 {
            let ip: Vec<String> = (0..4)
                .map(|k| (val[4 + k] ^ cookie[k]).to_string())
                .collect();
            return Some(ip.join("."));
        }
        // 属性按 4 字节对齐填充
        i += 4 + len.next_multiple_of(4);
    }
    None
}

/// RFC1928 §7 的 UDP 请求头：`RSV(2) FRAG(1) ATYP(1) DST.ADDR DST.PORT`。
/// **ATYP=03（域名）**：目标域名原样交上游解析（socks5h 口径，v3.6.0 R10 的结论）
pub fn socks5_udp_header(host: &str, port: u16) -> Vec<u8> {
    let mut v = vec![0x00, 0x00, 0x00, 0x03, host.len() as u8];
    v.extend_from_slice(host.as_bytes());
    v.extend_from_slice(&port.to_be_bytes());
    v
}

/// 剥掉回包的 RFC1928 UDP 头，拿到载荷。头长随 ATYP 变；`FRAG ≠ 0` 的分片包不支持
/// （RFC1928 允许实现直接丢），返回 `None`。
pub fn strip_socks5_udp_header(buf: &[u8]) -> Option<&[u8]> {
    if buf.len() < 4 || buf[2] != 0x00 {
        return None;
    }
    let addr_len = match buf[3] {
        0x01 => 4,
        0x04 => 16,
        0x03 => 1 + *buf.get(4)? as usize,
        _ => return None,
    };
    buf.get(4 + addr_len + 2..)
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
    /// 经上游发一次**浏览器式**的 Google 搜索 GET（[`super::GOOGLE_PROBE_URL`]，
    /// 带 [`super::GOOGLE_PROBE_UA`]、超时 [`super::GOOGLE_PROBE_TIMEOUT_SECS`]）。
    /// 单独一个方法而不是复用 [`Prober::get`]：判据要的是「真人搜索能不能用」，
    /// 没有浏览器 UA 时 Google 会直接回 `/sorry/`，那会把「上游没封」误判成「封了」。
    fn google_search(&self, up: &Upstream) -> Result<HttpProbe, ProbeError>;
    /// 经上游做一次 SOCKS5 UDP ASSOCIATE；HTTP 上游一律 `Ok(false)`（协议里没这回事）
    fn udp_associate(&self, up: &Upstream) -> Result<bool, ProbeError>;
    /// **不经上游**的直连 TCP 对照（黑名单确认的第二条件：直连同目标可达）
    fn direct_tcp(&self, host: &str, port: u16) -> bool;
    /// 到上游网关 `host:port` 的 **TCP 建连耗时**（毫秒）；`None` = 连不上 / 没测。
    /// 这一项不经隧道、不发任何请求，量的是「到这条上游有多远」。
    ///
    /// 以下四个**指标**方法都有缺省实现（「没测到」），只有真正量数据的 Prober
    /// 才需要覆盖：它们不参与健康/黑名单判定，缺数据时选路只是少一个排序依据，
    /// 而按判定方法的口径强制每个测试替身都实现一遍纯属噪声。
    fn gateway_tcp_ms(&self, _up: &Upstream) -> Option<u64> {
        None
    }
    /// 哨兵带外快探的第一步（spec §5.7）：到上游网关 `host:port` 能否在 `within` 内建起 TCP，
    /// 返回建连耗时（毫秒），`None` = 连不上。与 [`Prober::gateway_tcp_ms`] 的区别是**整体**限时：
    /// 解析出的各地址并发拨、任一连上即通，不是逐个地址各等一遍超时。缺省退回
    /// `gateway_tcp_ms`（只有真实实现关心时限）
    fn gateway_tcp_within(&self, up: &Upstream, _within: std::time::Duration) -> Option<u64> {
        self.gateway_tcp_ms(up)
    }
    /// 经上游发一次**浏览器 UA** 的 GET 并**计时**，返回 `(完整往返耗时 ms, 结果)`。
    /// `ms` 为 `None` = 请求没成功（失败的耗时只是超时值，记进样本会污染 p50）
    fn timed_get(&self, up: &Upstream, url: &str) -> (Option<u64>, Result<HttpProbe, ProbeError>) {
        (None, self.get(up, url))
    }
    /// 经 socks5 上游 UDP ASSOCIATE 向 [`super::STUN_HOST`] 发一个 STUN Binding Request。
    /// HTTP 上游恒不通并标注（协议里没有 UDP ASSOCIATE），见 [`http_no_udp`]
    fn stun_binding(&self, up: &Upstream) -> UdpProbe {
        http_no_udp(up).unwrap_or_default()
    }
    /// 经上游下载 `down_bytes` / 上传 `up_bytes`，算 Mbps。失败只记 `note`
    fn speedtest(&self, _up: &Upstream, _down_bytes: u64, _up_bytes: u64) -> SpeedSample {
        SpeedSample::default()
    }
}

/// 「HTTP 上游没有 UDP」这句话的**唯一**一份：HTTP 代理协议里没有 UDP ASSOCIATE，
/// sing-box 的 http 出站也没有 UDP 能力（同 `ResidentialGroup::udp_via_pool`）。
/// 真实与假 Prober 都调它，免得同一句文案有两份、面板上两种写法。
pub const HTTP_NO_UDP_NOTE: &str = "HTTP 上游无 UDP";

pub fn http_no_udp(up: &Upstream) -> Option<UdpProbe> {
    (up.kind == UpstreamKind::Http).then(|| UdpProbe {
        note: Some(HTTP_NO_UDP_NOTE.into()),
        ..Default::default()
    })
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

/// Google 搜索页的 GET 结果 →「经这条上游还能不能正常用 Google 搜索」。
/// **唯一**一份判据：巡检（[`super::health::probe_member`]）与体检（[`super::check::run`]）
/// 都调它。`None` = 本轮没探到结论 —— 这一项参与选路（spec R2 ②），把一次抖动记成
/// 「封了」会把好上游踢到队尾，所以宁可不下结论。
pub fn google_ok_of(r: &Result<HttpProbe, ProbeError>) -> Option<bool> {
    let Ok(hp) = r else {
        // 超时 / 连不上 / 407：说明不了 Google 的事
        return None;
    };
    // 调研 §D 的 Bright Data 形态：对 serp 域名整域硬拒（`403 Forbidden serp domain`）；
    // Google 自己判机器人时回 429
    if matches!(hp.status, 403 | 429) {
        return Some(false);
    }
    let b = hp.body.to_ascii_lowercase();
    // 200 也可能是拦截页：`/sorry/` 跳转页与「unusual traffic」提示都是被判机器人
    if b.contains("unusual traffic") || b.contains("/sorry/") {
        return Some(false);
    }
    // 其余 4xx/5xx 是上游或站点自己的毛病，不算「Google 被封」
    (hp.status < 400).then_some(true)
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

    /// 经该上游发 HTTP 请求的客户端（凭据走 `Proxy::basic_auth`，不进 URL）。
    /// `timeout` 单独给：Google 探测比普通 GET 短（每轮每成员都多打一次）
    fn proxied_client(
        &self,
        up: &Upstream,
        timeout: std::time::Duration,
    ) -> Result<reqwest::blocking::Client, ProbeError> {
        let proxy = reqwest::Proxy::all(proxy_url(up))
            .map_err(|e| ProbeError::Unreachable(e.to_string()))?
            .basic_auth(&up.username, &up.password);
        reqwest::blocking::Client::builder()
            .proxy(proxy)
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| ProbeError::Unreachable(e.to_string()))
    }

    /// RFC1928 §7 的 UDP ASSOCIATE：返回 (控制连接, 中继地址)。
    /// **控制连接必须留在调用方手里活着**：RFC1928 规定关联的生命周期就是这条 TCP
    /// 连接的生命周期，提前 drop 它上游会立刻丢弃关联，后面发的 UDP 包全被丢。
    fn socks5_associate(
        &self,
        up: &Upstream,
    ) -> std::io::Result<(std::net::TcpStream, std::net::SocketAddr)> {
        use std::io::{Read, Write};
        let mut s = self.dial(up)?;
        match socks5_handshake(&mut s, up)? {
            ConnectVerdict::Open => {}
            other => return Err(std::io::Error::other(other.label())),
        }
        s.write_all(&[0x05, 0x03, 0x00, 0x01, 0, 0, 0, 0, 0, 0])?;
        s.flush()?;
        let mut head = [0u8; 4];
        s.read_exact(&mut head)?;
        if head[1] != 0x00 {
            return Err(std::io::Error::other(format!(
                "UDP ASSOCIATE 被拒（REP=0x{:02x}）",
                head[1]
            )));
        }
        // BND.ADDR / BND.PORT 就是要往哪里发 UDP，必须完整读出来（`socks5_request`
        // 只读前 4 字节，拿不到中继地址）
        let mut addr = match head[3] {
            0x01 => vec![0u8; 4],
            0x04 => vec![0u8; 16],
            0x03 => {
                let mut l = [0u8; 1];
                s.read_exact(&mut l)?;
                vec![0u8; l[0] as usize]
            }
            t => {
                return Err(std::io::Error::other(format!(
                    "UDP ASSOCIATE 回了未知 ATYP 0x{t:02x}"
                )))
            }
        };
        s.read_exact(&mut addr)?;
        let mut port = [0u8; 2];
        s.read_exact(&mut port)?;
        // 很多实现（Decodo 在内）回 BND.ADDR = 0.0.0.0，意思是「就发到你连着的这台」
        let ip = match (head[3], addr.as_slice()) {
            (0x01, [a, b, c, d]) if [*a, *b, *c, *d] != [0, 0, 0, 0] => {
                std::net::IpAddr::from([*a, *b, *c, *d])
            }
            _ => s.peer_addr()?.ip(),
        };
        Ok((s, std::net::SocketAddr::new(ip, u16::from_be_bytes(port))))
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

/// `reqwest` 的发送错误 → [`ProbeError`]。https 目标经 HTTP 上游走 CONNECT 隧道，
/// 407 在隧道建立阶段就失败了 ⇒ reqwest 只给一个 Err，拿不到状态码。把整条 `source`
/// 链的文字拼出来扫一遍，是这里唯一能识别凭据失效的办法（第二层补判在调用方，
/// 见 [`confirm_auth_failure`] 的注释）。
fn send_error(url: &str, e: reqwest::Error) -> ProbeError {
    tracing::debug!(url = %crate::redact::url_credentials(url), error = %e, "经上游 GET 失败");
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
}

/// 响应 → [`HttpProbe`]（正文截到 64KB）。状态码 407 只有「明文 HTTP 目标」才会走到
/// 这里（本模块的探测 URL 全是 https，407 走 [`send_error`] 那条分支）。留着是为了
/// 将来真加明文目标时不必再想一遍。
fn http_probe_of(resp: reqwest::blocking::Response) -> Result<HttpProbe, ProbeError> {
    let status = resp.status().as_u16();
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
        let resp = self
            .proxied_client(up, self.timeout)?
            .get(url)
            .send()
            .map_err(|e| send_error(url, e))?;
        http_probe_of(resp)
    }

    fn google_search(&self, up: &Upstream) -> Result<HttpProbe, ProbeError> {
        let url = super::GOOGLE_PROBE_URL;
        let resp = self
            .proxied_client(
                up,
                std::time::Duration::from_secs(super::GOOGLE_PROBE_TIMEOUT_SECS),
            )?
            // 无 UA / 脚本 UA 会被 Google 直接判机器人，那测的就不是上游了
            .get(url)
            .header(reqwest::header::USER_AGENT, super::GOOGLE_PROBE_UA)
            .send()
            .map_err(|e| send_error(url, e))?;
        http_probe_of(resp)
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

    fn gateway_tcp_ms(&self, up: &Upstream) -> Option<u64> {
        let t = std::time::Instant::now();
        // `dial` 里含 DNS 解析：到上游网关的实际开销本来就包含它，不单独扣
        self.dial(up).ok().map(|_| elapsed_ms(t))
    }

    /// `dial` 逐个地址各等满超时：网关域名解析出 6 个地址又整段被丢包时，那是 6 × 5 = 30 秒
    /// （2026-09-13 演练判据① 30.3 秒的来源）。这里解析与拨号都放后台线程、各地址并发拨，
    /// 主线程只等 `within`：DNS 卡住也吃同一个时限；全部被拒时发送端全掉线，立刻返回。
    /// 超时后还没结束的线程各自在 `within` 内收场（`connect_timeout`），不会越积越多
    fn gateway_tcp_within(&self, up: &Upstream, within: std::time::Duration) -> Option<u64> {
        use std::net::ToSocketAddrs;
        let t = std::time::Instant::now();
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let target = (up.host.clone(), up.port);
        std::thread::spawn(move || {
            let Ok(addrs) = target.to_socket_addrs() else {
                return;
            };
            for a in addrs {
                let tx = tx.clone();
                std::thread::spawn(move || {
                    if std::net::TcpStream::connect_timeout(&a, within).is_ok() {
                        let _ = tx.send(());
                    }
                });
            }
        });
        rx.recv_timeout(within).ok().map(|()| elapsed_ms(t))
    }

    fn timed_get(&self, up: &Upstream, url: &str) -> (Option<u64>, Result<HttpProbe, ProbeError>) {
        let client = match self.proxied_client(
            up,
            std::time::Duration::from_secs(super::LATENCY_PROBE_TIMEOUT_SECS),
        ) {
            Ok(c) => c,
            Err(e) => return (None, Err(e)),
        };
        let t = std::time::Instant::now();
        let r = client
            .get(url)
            // 浏览器 UA：脚本 UA 会被 Google 当机器人加塞挑战，那测的不是上游的延迟
            .header(reqwest::header::USER_AGENT, super::GOOGLE_PROBE_UA)
            .send()
            .map_err(|e| send_error(url, e))
            .and_then(http_probe_of);
        // 失败的耗时只是超时值，记进样本会把 p50 拉成 8000ms
        let ms = r.is_ok().then(|| elapsed_ms(t));
        (ms, r)
    }

    fn stun_binding(&self, up: &Upstream) -> UdpProbe {
        // 「HTTP 上游没有 UDP」的文案与判据只有 [`http_no_udp`] 这一份
        if let Some(v) = http_no_udp(up) {
            return v;
        }
        let fail = |note: String| UdpProbe {
            note: Some(note),
            ..Default::default()
        };
        // 每次探测新建关联：实测 Decodo 的一次 UDP ASSOCIATE 只服务第一个目标地址
        let (_ctrl, relay) = match self.socks5_associate(up) {
            Ok(x) => x,
            Err(e) => return fail(format!("UDP ASSOCIATE 失败：{e}")),
        };
        let sock = match std::net::UdpSocket::bind("0.0.0.0:0") {
            Ok(s) => s,
            Err(e) => return fail(format!("本地 UDP 套接字创建失败：{e}")),
        };
        let timeout = std::time::Duration::from_secs(super::STUN_TIMEOUT_SECS);
        if let Err(e) = sock.set_read_timeout(Some(timeout)) {
            return fail(format!("本地 UDP 套接字超时设置失败：{e}"));
        }
        let tid: [u8; 12] = rand::random();
        let mut pkt = socks5_udp_header(super::STUN_HOST, super::STUN_PORT);
        pkt.extend_from_slice(&stun_binding_request(&tid));
        let t = std::time::Instant::now();
        if let Err(e) = sock.send_to(&pkt, relay) {
            return fail(format!("STUN 请求发不出去：{e}"));
        }
        let mut buf = [0u8; 1500];
        let n = match sock.recv_from(&mut buf) {
            Ok((n, _)) => n,
            Err(e) => {
                return fail(format!(
                    "STUN 无回复（{}s 内）：{e}",
                    super::STUN_TIMEOUT_SECS
                ))
            }
        };
        let ms = elapsed_ms(t);
        match strip_socks5_udp_header(&buf[..n]).and_then(|p| parse_stun_xor_mapped(p, &tid)) {
            Some(ip) => UdpProbe {
                ok: true,
                exit_ip: Some(ip),
                ms: Some(ms),
                note: None,
            },
            // 收到字节但不是本次请求的 Binding Response ⇒ 不算通（可能是迟到的旧回包）
            None => fail("收到 UDP 回包但不是本次的 STUN Binding Response".into()),
        }
    }

    fn speedtest(&self, up: &Upstream, down_bytes: u64, up_bytes: u64) -> SpeedSample {
        let client = match self.proxied_client(
            up,
            std::time::Duration::from_secs(super::SPEEDTEST_TIMEOUT_SECS),
        ) {
            Ok(c) => c,
            Err(e) => {
                return SpeedSample {
                    note: Some(e.to_string()),
                    ..Default::default()
                }
            }
        };
        let mut notes: Vec<String> = Vec::new();
        let url = format!("{}{}", super::SPEEDTEST_DOWN_URL, down_bytes);
        let t = std::time::Instant::now();
        // 用**实收字节数**算，不用请求的字节数：服务端截断时不能虚报速度
        let down = match client.get(&url).send().and_then(|r| r.bytes()) {
            Ok(b) => mbps(b.len() as u64, elapsed_ms(t)),
            Err(e) => {
                notes.push(format!("下载测速失败：{e}"));
                None
            }
        };
        // 随机字节：全零正文会被链路上的压缩吃掉，测出来是压缩比不是带宽
        let mut body = vec![0u8; up_bytes as usize];
        rand::Rng::fill(&mut rand::thread_rng(), &mut body[..]);
        let t = std::time::Instant::now();
        let up_mbps = match client.post(super::SPEEDTEST_UP_URL).body(body).send() {
            Ok(_) => mbps(up_bytes, elapsed_ms(t)),
            Err(e) => {
                notes.push(format!("上传测速失败：{e}"));
                None
            }
        };
        SpeedSample {
            down_mbps: down,
            up_mbps,
            note: (!notes.is_empty()).then(|| notes.join("；")),
        }
    }
}

/// `Instant` → 毫秒（`u128` 截到 `u64`：测速上限 60 秒，永远溢不出）
fn elapsed_ms(t: std::time::Instant) -> u64 {
    t.elapsed().as_millis() as u64
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
    /// key = URL 或 `"<host>:<port> <URL>"`（按上游限定的那条优先），缺省
    /// `Err(Unreachable("no route"))`。**错误哨兵约定**（跨任务契约，T6/T7/T8 的测试都依赖它）：
    /// 值为 `Err("__auth_failed__")` 时 `get` 返回 `ProbeError::AuthFailed`，
    /// 其余字符串返回 `ProbeError::Unreachable(那个字符串)`
    pub gets: std::collections::BTreeMap<String, Result<HttpProbe, String>>,
    /// [`Prober::google_search`] 的返回，缺省 `Err(Unreachable("no route"))`（= 没结论）。
    /// 错误哨兵与 `gets` 同约定：`Err("__auth_failed__")` ⇒ `ProbeError::AuthFailed`
    pub google: Option<Result<HttpProbe, String>>,
    pub udp: bool,
    /// 直连可达的 `"<host>:<port>"`；不在集合里即不可达
    pub direct: std::collections::BTreeSet<String>,
    /// [`Prober::gateway_tcp_ms`] 与 [`Prober::gateway_tcp_within`] 的返回，缺省 `None`
    /// （= 连不上，「未知」不许变成好成绩）
    pub tcp_ms: Option<u64>,
    /// 按 `"<host>:<port>"` 覆盖上面那一格；未命中才落回 `tcp_ms`。哨兵借用后要**逐条**
    /// 带外验证（spec §5.7），同一网关的不同端口必须能给出不同结论，所以假件按端点作答
    pub tcp_by_endpoint: std::collections::BTreeMap<String, Option<u64>>,
    /// `gateway_tcp_within` 连不上时带着时限调一次：测试用它把「丢包时等满时限」记到假时钟上
    pub on_tcp_fail: Option<Box<dyn Fn(std::time::Duration) + Send>>,
    /// [`Prober::timed_get`] 的耗时；结果本身仍查 `gets`（同一份 URL 表）
    pub http_ms: Option<u64>,
    /// [`Prober::stun_binding`] 的返回，缺省 `UdpProbe::default()`（不通、没耗时）
    pub stun: UdpProbe,
    /// [`Prober::speedtest`] 的返回，缺省 `SpeedSample::default()`（没测到）
    pub speed: SpeedSample,
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

    /// 让这些 `"<host>:<port>"` 的网关连得通、经它的 [`super::LATENCY_PROBE_URL`] 回 204
    /// （= [`super::health::probe_quick`] 判 `ok`）；没列到的上游仍走缺省「网关连不上」。
    /// 哨兵借用后按上游逐条验证，一次调用里要对不同上游给出不同结论
    pub fn with_gateways_up(&self, endpoints: &[&str]) -> &Self {
        self.with(|i| {
            for e in endpoints {
                i.tcp_by_endpoint.insert((*e).to_string(), Some(20));
            }
            i.gets.insert(
                super::LATENCY_PROBE_URL.to_string(),
                Ok(HttpProbe {
                    status: 204,
                    body: String::new(),
                }),
            );
        })
    }
}

#[cfg(test)]
impl Default for FakeProber {
    fn default() -> Self {
        Self::new()
    }
}

/// `gets` / `google` 表里的一格 → [`Prober`] 的返回。**错误哨兵约定**（跨任务契约）：
/// `Err("__auth_failed__")` ⇒ [`ProbeError::AuthFailed`]，其余字符串 ⇒ `Unreachable`。
/// `get` / `google_search` / `timed_get` 共用这一份，免得哨兵约定有三份。
#[cfg(test)]
fn fake_result(v: Option<&Result<HttpProbe, String>>) -> Result<HttpProbe, ProbeError> {
    match v {
        Some(Ok(hp)) => Ok(hp.clone()),
        Some(Err(e)) if e == "__auth_failed__" => Err(ProbeError::AuthFailed),
        Some(Err(e)) => Err(ProbeError::Unreachable(e.clone())),
        None => Err(ProbeError::Unreachable("no route".into())),
    }
}

/// `gets` 表的查法：先找按上游限定的 `"<host>:<port> <url>"`，再找裸 URL。
/// 一次调用里探两条上游（借用后的带外验证）时，同一个 URL 要能对不同上游给出不同结论
#[cfg(test)]
fn fake_get(i: &FakeProberInner, up: &Upstream, url: &str) -> Result<HttpProbe, ProbeError> {
    let keyed = format!("{}:{} {url}", up.host, up.port);
    fake_result(i.gets.get(&keyed).or_else(|| i.gets.get(url)))
}

/// 假件的 `host:port` → 网关 TCP 建连耗时：按端点覆盖优先，未命中落回 `tcp_ms`
#[cfg(test)]
fn fake_tcp_ms(i: &FakeProberInner, up: &Upstream) -> Option<u64> {
    i.tcp_by_endpoint
        .get(&format!("{}:{}", up.host, up.port))
        .copied()
        .unwrap_or(i.tcp_ms)
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

    fn get(&self, up: &Upstream, url: &str) -> Result<HttpProbe, ProbeError> {
        let mut i = self.inner.lock().expect("FakeProber 锁被毒化");
        i.calls.push(format!("get:{url}"));
        fake_get(&i, up, url)
    }

    fn google_search(&self, _up: &Upstream) -> Result<HttpProbe, ProbeError> {
        let mut i = self.inner.lock().expect("FakeProber 锁被毒化");
        i.calls.push("google".into());
        fake_result(i.google.as_ref())
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

    fn gateway_tcp_ms(&self, up: &Upstream) -> Option<u64> {
        let mut i = self.inner.lock().expect("FakeProber 锁被毒化");
        i.calls.push("tcp".into());
        fake_tcp_ms(&i, up)
    }

    fn gateway_tcp_within(&self, up: &Upstream, within: std::time::Duration) -> Option<u64> {
        let mut i = self.inner.lock().expect("FakeProber 锁被毒化");
        i.calls.push("tcp".into());
        let ms = fake_tcp_ms(&i, up);
        if ms.is_none() {
            if let Some(f) = &i.on_tcp_fail {
                f(within);
            }
        }
        ms
    }

    fn timed_get(&self, up: &Upstream, url: &str) -> (Option<u64>, Result<HttpProbe, ProbeError>) {
        let mut i = self.inner.lock().expect("FakeProber 锁被毒化");
        // 只记一条 `timed:`：真实实现就是**一次**请求，记两条会让按 calls() 数请求数的
        // 测试把它当成两次
        i.calls.push(format!("timed:{url}"));
        // 结果查同一份 `gets` 表（真实实现也是一次普通 GET，只是带 UA 并计时）；
        // 失败时不给耗时，与 `ReqwestProber::timed_get` 同口径
        let r = fake_get(&i, up, url);
        (r.is_ok().then_some(i.http_ms).flatten(), r)
    }

    fn stun_binding(&self, up: &Upstream) -> UdpProbe {
        let mut i = self.inner.lock().expect("FakeProber 锁被毒化");
        i.calls.push("stun".into());
        // 「HTTP 上游没有 UDP」是协议事实，不是可编程的假数据（真实 Prober 同一份判据）
        http_no_udp(up).unwrap_or_else(|| i.stun.clone())
    }

    fn speedtest(&self, _up: &Upstream, _down: u64, _up_bytes: u64) -> SpeedSample {
        let mut i = self.inner.lock().expect("FakeProber 锁被毒化");
        i.calls.push("speed".into());
        i.speed.clone()
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
        // 硬拒只有 Refused 一种：凭据失效与连不上都不是「这个目标被拒」，
        // `label()` 的四个取值就是黑名单与体检读到的全部判据
        assert_eq!(ConnectVerdict::Refused { code: 403 }.label(), "refused:403");
        assert_eq!(ConnectVerdict::Open.label(), "open");
        assert_eq!(
            ConnectVerdict::Unreachable { detail: "x".into() }.label(),
            "unreachable"
        );
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
    fn google_verdict_reads_the_serp_block_the_sorry_page_and_stays_none_when_unknown() {
        // 主理人硬要求「住宅上游不封 Google」，判据只有这一份（巡检与体检都调它）
        assert_eq!(
            google_ok_of(&Ok(HttpProbe {
                status: 200,
                body: "<html>weather results".into()
            })),
            Some(true)
        );
        // 调研 §D 的 Bright Data 形态：对 serp 域名整域硬拒
        assert_eq!(
            google_ok_of(&Ok(HttpProbe {
                status: 403,
                body: "403 Forbidden serp domain".into()
            })),
            Some(false)
        );
        assert_eq!(
            google_ok_of(&Ok(HttpProbe {
                status: 429,
                body: String::new()
            })),
            Some(false)
        );
        // 200 也可能是拦截页：Google 判机器人时回 /sorry/ 或「unusual traffic」
        assert_eq!(
            google_ok_of(&Ok(HttpProbe {
                status: 200,
                body: "Our systems have detected unusual traffic from your network".into()
            })),
            Some(false)
        );
        assert_eq!(
            google_ok_of(&Ok(HttpProbe {
                status: 200,
                body: "<a href=\"https://www.google.com/sorry/index\">".into()
            })),
            Some(false)
        );
        // 探不到就不下结论：这一项参与选路，把抖动记成「封了」会把好上游踢到队尾
        assert_eq!(
            google_ok_of(&Err(ProbeError::Unreachable("timeout".into()))),
            None
        );
        assert_eq!(google_ok_of(&Err(ProbeError::AuthFailed)), None);
        assert_eq!(
            google_ok_of(&Ok(HttpProbe {
                status: 502,
                body: String::new()
            })),
            None,
            "上游自己 5xx 不是 Google 封的"
        );
    }

    #[test]
    fn fake_prober_google_search_is_programmable_and_records_a_call() {
        let p = FakeProber::new();
        assert!(
            matches!(
                p.google_search(&up(UpstreamKind::Http)),
                Err(ProbeError::Unreachable(_))
            ),
            "缺省不给结论"
        );
        p.with(|i| {
            i.google = Some(Ok(HttpProbe {
                status: 403,
                body: "403 Forbidden serp domain".into(),
            }))
        });
        assert_eq!(
            google_ok_of(&p.google_search(&up(UpstreamKind::Http))),
            Some(false)
        );
        p.with(|i| i.google = Some(Err("__auth_failed__".into())));
        assert!(matches!(
            p.google_search(&up(UpstreamKind::Http)),
            Err(ProbeError::AuthFailed)
        ));
        assert_eq!(p.calls(), vec!["google", "google", "google"]);
    }

    /// 一条 Binding Response：20 字节头 + 一条 XOR-MAPPED-ADDRESS 属性（RFC 5389 §15.2）
    fn stun_response_fixture(tid: &[u8; 12], ip: [u8; 4], port: u16) -> Vec<u8> {
        let cookie = super::super::STUN_MAGIC_COOKIE.to_be_bytes();
        let mut v = vec![0x01, 0x01, 0x00, 0x0c];
        v.extend_from_slice(&cookie);
        v.extend_from_slice(tid);
        v.extend_from_slice(&[0x00, 0x20, 0x00, 0x08, 0x00, 0x01]);
        v.extend_from_slice(&(port ^ (super::super::STUN_MAGIC_COOKIE >> 16) as u16).to_be_bytes());
        for (i, b) in ip.iter().enumerate() {
            v.push(b ^ cookie[i]);
        }
        v
    }

    #[test]
    fn a_stun_binding_request_has_the_rfc5389_header_and_the_transaction_id() {
        let tid = [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        let req = stun_binding_request(&tid);
        assert_eq!(req.len(), 20, "20 字节头，没有属性");
        assert_eq!(&req[..2], &[0x00, 0x01], "Binding Request 的 type");
        assert_eq!(&req[2..4], &[0x00, 0x00], "没有属性 ⇒ length = 0");
        assert_eq!(
            &req[4..8],
            &super::super::STUN_MAGIC_COOKIE.to_be_bytes(),
            "magic cookie 0x2112A442"
        );
        assert_eq!(&req[8..20], &tid, "12 字节 transaction id 原样带上");
    }

    #[test]
    fn the_xor_mapped_address_is_unxored_back_to_the_udp_exit_ip() {
        // RFC 5389 §15.2：XOR-MAPPED-ADDRESS 的地址与端口都跟 magic cookie 异或过
        let tid = [7u8; 12];
        let resp = stun_response_fixture(&tid, [198, 51, 100, 7], 54321);
        assert_eq!(
            parse_stun_xor_mapped(&resp, &tid).as_deref(),
            Some("198.51.100.7"),
            "解回真实 UDP 出口 IP"
        );
        // transaction id 不匹配 ⇒ 不是这次请求的回复，一律不信（UDP 没有连接语义）
        assert_eq!(parse_stun_xor_mapped(&resp, &[9u8; 12]), None);
        // Binding Error Response（0x0111）不是成功回复
        let mut bad = resp.clone();
        bad[1] = 0x11;
        assert_eq!(parse_stun_xor_mapped(&bad, &tid), None);
        // 截断的包不许 panic
        assert_eq!(parse_stun_xor_mapped(&resp[..12], &tid), None);
        assert_eq!(parse_stun_xor_mapped(&[], &tid), None);
    }

    #[test]
    fn the_socks5_udp_header_carries_the_domain_so_the_upstream_resolves_it() {
        // RFC1928 §7：RSV(2) FRAG(1) ATYP(1) DST.ADDR DST.PORT，然后才是载荷。
        // ATYP=03（域名）= 目标域名原样交上游解析（socks5h 口径，v3.6.0 R10 的结论）
        let h = socks5_udp_header("stun.l.google.com", 19302);
        assert_eq!(&h[..4], &[0x00, 0x00, 0x00, 0x03]);
        assert_eq!(h[4] as usize, "stun.l.google.com".len());
        assert_eq!(&h[5..22], b"stun.l.google.com");
        assert_eq!(&h[22..24], &19302u16.to_be_bytes());
        // 回包的头长随 ATYP 变，载荷要按 ATYP 剥
        let mut pkt = vec![0x00, 0x00, 0x00, 0x01, 127, 0, 0, 1, 0x04, 0x38];
        pkt.extend_from_slice(b"payload");
        assert_eq!(strip_socks5_udp_header(&pkt), Some(&b"payload"[..]));
        let mut dom = vec![0x00, 0x00, 0x00, 0x03, 3, b'a', b'b', b'c', 0x04, 0x38];
        dom.extend_from_slice(b"xy");
        assert_eq!(strip_socks5_udp_header(&dom), Some(&b"xy"[..]));
        // FRAG ≠ 0 = 分片包，RFC1928 允许实现直接丢
        assert_eq!(
            strip_socks5_udp_header(&[0, 0, 1, 1, 0, 0, 0, 0, 0, 0]),
            None
        );
        assert_eq!(strip_socks5_udp_header(&[0, 0, 0, 1]), None, "截断不 panic");
        assert_eq!(strip_socks5_udp_header(&[]), None);
    }

    #[test]
    fn mbps_is_bytes_times_eight_over_seconds_and_refuses_a_zero_duration() {
        // 4 MB / 4 秒 = 8.388608 Mbps
        assert_eq!(mbps(4 * 1024 * 1024, 4_000), Some(8.388608));
        assert_eq!(mbps(1024 * 1024, 1_000), Some(8.388608));
        // 0 字节 / 0 毫秒都不是速度（除零与「一个字节都没传」都要挡掉）
        assert_eq!(mbps(0, 1_000), None);
        assert_eq!(mbps(1024, 0), None);
    }

    /// 哨兵快探的 TCP 门槛（spec §5.7）：连上即返回耗时；解析出的地址全被拒 ⇒ 立刻返回 `None`，
    /// 不等满时限（丢包才会等满，且各地址并发拨、整体不超过时限）
    #[test]
    fn gateway_tcp_within_answers_on_connect_and_gives_up_early_when_refused() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut u = up(UpstreamKind::Socks5);
        u.host = "127.0.0.1".into();
        u.port = l.local_addr().unwrap().port();
        let p = ReqwestProber::new();
        let within = std::time::Duration::from_secs(3);
        assert!(p.gateway_tcp_within(&u, within).is_some());
        drop(l);
        let t = std::time::Instant::now();
        assert_eq!(p.gateway_tcp_within(&u, within), None);
        assert!(
            t.elapsed() < within,
            "被拒是立刻知道的，不该等满时限：{:?}",
            t.elapsed()
        );
    }

    #[test]
    fn the_fake_prober_returns_programmable_latency_udp_and_speed_samples() {
        let p = FakeProber::new();
        let u = up(UpstreamKind::Socks5);
        // 缺省：没测到延迟、UDP 不通、没测速 —— 「未知」不能凭空变成好成绩
        assert_eq!(p.gateway_tcp_ms(&u), None);
        assert_eq!(p.stun_binding(&u), UdpProbe::default());
        assert_eq!(p.speedtest(&u, 1, 1), SpeedSample::default());
        let (ms, r) = p.timed_get(&u, super::super::LATENCY_PROBE_URL);
        assert_eq!(ms, None);
        assert!(matches!(r, Err(ProbeError::Unreachable(_))));

        p.with(|i| {
            i.tcp_ms = Some(31);
            i.http_ms = Some(97);
            i.gets.insert(
                super::super::LATENCY_PROBE_URL.into(),
                Ok(HttpProbe {
                    status: 204,
                    body: String::new(),
                }),
            );
            i.stun = UdpProbe {
                ok: true,
                exit_ip: Some("198.51.100.7".into()),
                ms: Some(42),
                note: None,
            };
            i.speed = SpeedSample {
                down_mbps: Some(88.5),
                up_mbps: Some(12.25),
                note: None,
            };
        });
        assert_eq!(p.gateway_tcp_ms(&u), Some(31));
        let (ms, r) = p.timed_get(&u, super::super::LATENCY_PROBE_URL);
        assert_eq!(ms, Some(97));
        assert_eq!(r.unwrap().status, 204);
        assert_eq!(p.stun_binding(&u).exit_ip.as_deref(), Some("198.51.100.7"));
        assert_eq!(p.speedtest(&u, 1, 1).down_mbps, Some(88.5));
        assert_eq!(
            p.calls(),
            vec![
                "tcp",
                "stun",
                "speed",
                "timed:https://www.google.com/generate_204",
                "tcp",
                "timed:https://www.google.com/generate_204",
                "stun",
                "speed",
            ]
        );
    }

    #[test]
    fn an_http_upstream_never_claims_udp() {
        // sing-box 的 http 出站没有 UDP 能力，协议层也没有 UDP ASSOCIATE：恒不通、要标注
        let p = ReqwestProber::with_timeout(1);
        let v = p.stun_binding(&up(UpstreamKind::Http));
        assert!(!v.ok);
        assert_eq!(v.ms, None);
        assert_eq!(v.exit_ip, None);
        assert_eq!(v.note.as_deref(), Some("HTTP 上游无 UDP"));
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
        drop(l);
    }
}
