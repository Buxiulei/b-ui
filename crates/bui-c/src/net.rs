//! HTTP 出口收在 [`Net`] 一个 trait 后面：204 探测（可经本地 SOCKS）、取文本、取字节，
//! 以及连接检查与测速用的带耗时探测、限量限时下载。
//! 生产用 [`ReqwestNet`]（`reqwest::blocking` + rustls，无 openssl），
//! 单元测试注入 [`FakeNet`](crate::fake::FakeNet)，于是测试不发真实请求。

use crate::error::redact_url;
use crate::{Error, Result};
use std::io::Read;
use std::time::{Duration, Instant};

/// 请求走哪条腿。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    Direct,
    Socks5 { port: u16 },
}

/// 一次探测：状态码，与从发出请求到收到响应头的用时（不含建 client）。
#[derive(Debug, Clone, PartialEq)]
pub struct Probe {
    pub code: u16,
    pub elapsed: Duration,
}

/// 探测失败的类别。`Other` 里只放一个类别词（`tls` / `connect` / `http` / `body` / `other`），
/// 不放 reqwest 的原始错误文本：那里面可能带 URL，而 URL 的末段可能是用户名。
#[derive(Debug, Clone, PartialEq)]
pub enum ProbeError {
    Timeout,
    Refused,
    Dns,
    Other(String),
}

/// 限量限时下载的结果：实际读到的字节、实际用时、是否读满了 `max_bytes`。
#[derive(Debug, Clone, PartialEq)]
pub struct Download {
    pub bytes: u64,
    pub elapsed: Duration,
    pub complete: bool,
}

/// 可注入的 HTTP 客户端。`Sync` 是 supertrait（D22）：测速在 `std::thread::scope` 里并发探测。
pub trait Net: Sync {
    /// 只要状态码，用于 `generate_204` 类探测。
    fn status(&self, url: &str, via: Via, timeout: Duration) -> Result<u16>;
    /// 直连取文本（manifest / 订阅）。非 2xx 视为失败。
    fn text(&self, url: &str, timeout: Duration) -> Result<String>;
    /// 直连取字节（二进制下载）。非 2xx 视为失败。
    fn bytes(&self, url: &str, timeout: Duration) -> Result<Vec<u8>>;
    /// 按 `via` 取文本（出口检测经 SOCKS 或直连）。非 2xx 视为失败。
    fn text_via(&self, url: &str, via: Via, timeout: Duration) -> Result<String>;
    /// GET 只等响应头、不读正文、不跟重定向，带回状态码与用时。拿到任何状态码
    /// （包括 3xx）都算探测成功，失败按 [`ProbeError`] 分类。
    fn probe(
        &self,
        url: &str,
        via: Via,
        timeout: Duration,
    ) -> std::result::Result<Probe, ProbeError>;
    /// 读到 `max_bytes` 或 `cap` 到点就停，两种都是 `Ok`；非 2xx、响应头之前的失败报错。
    fn download_via(&self, url: &str, via: Via, max_bytes: u64, cap: Duration) -> Result<Download>;
}

/// 生产实现。
pub struct ReqwestNet;

impl ReqwestNet {
    /// `no_proxy()` 是必须的：root shell 会 source /etc/profile.d/proxy.sh，
    /// 不关掉 env 代理的话「直连探测」会经隧道出去，误判成通。
    fn builder(&self, via: Via, timeout: Duration) -> Result<reqwest::blocking::ClientBuilder> {
        let mut b = reqwest::blocking::Client::builder()
            .timeout(timeout)
            .no_proxy();
        if let Via::Socks5 { port } = via {
            let p = reqwest::Proxy::all(format!("socks5h://127.0.0.1:{port}")).map_err(|e| {
                Error::Net {
                    url: format!("socks5h://127.0.0.1:{port}"),
                    detail: e.to_string(),
                }
            })?;
            b = b.proxy(p);
        }
        Ok(b)
    }

    fn client(&self, via: Via, timeout: Duration) -> Result<reqwest::blocking::Client> {
        build(self.builder(via, timeout)?)
    }

    /// `probe` / `download_via` 用：连接超时也钉在同一个时限上。整次请求（含读正文）的上限
    /// 另靠请求级的 `.timeout()`：blocking 客户端级的 timeout 不下传给内部的异步请求，
    /// 读正文时每次 `read` 都重新计时；请求级的才从发出请求一直算到正文读完。
    fn bounded_builder(
        &self,
        via: Via,
        timeout: Duration,
    ) -> Result<reqwest::blocking::ClientBuilder> {
        Ok(self.builder(via, timeout)?.connect_timeout(timeout))
    }
}

fn build(b: reqwest::blocking::ClientBuilder) -> Result<reqwest::blocking::Client> {
    b.build().map_err(|e| Error::Net {
        url: "<client>".into(),
        detail: e.to_string(),
    })
}

/// 请求与读响应体的错误：先 `without_url()` 再转文本。reqwest 的 Display 会追加
/// ` for url (<完整 URL>)`，而订阅与 `/api/nodes` 路径的末段是用户名（等价凭据），
/// 原样放进 detail 就绕过了 [`redact_url`]。
fn request_error(url: &str, e: reqwest::Error) -> Error {
    Error::Net {
        url: redact_url(url),
        detail: e.without_url().to_string(),
    }
}

impl Net for ReqwestNet {
    fn status(&self, url: &str, via: Via, timeout: Duration) -> Result<u16> {
        let r = self
            .client(via, timeout)?
            .get(url)
            .send()
            .map_err(|e| request_error(url, e))?;
        Ok(r.status().as_u16())
    }

    fn text(&self, url: &str, timeout: Duration) -> Result<String> {
        self.text_via(url, Via::Direct, timeout)
    }

    fn bytes(&self, url: &str, timeout: Duration) -> Result<Vec<u8>> {
        let r = self
            .client(Via::Direct, timeout)?
            .get(url)
            .send()
            .map_err(|e| request_error(url, e))?;
        ensure_2xx(url, r.status().as_u16())?;
        Ok(r.bytes().map_err(|e| request_error(url, e))?.to_vec())
    }

    fn text_via(&self, url: &str, via: Via, timeout: Duration) -> Result<String> {
        let r = self
            .client(via, timeout)?
            .get(url)
            .send()
            .map_err(|e| request_error(url, e))?;
        ensure_2xx(url, r.status().as_u16())?;
        r.text().map_err(|e| request_error(url, e))
    }

    fn probe(
        &self,
        url: &str,
        via: Via,
        timeout: Duration,
    ) -> std::result::Result<Probe, ProbeError> {
        // 不跟重定向：spec §6.2 里 2xx / 3xx 都算通，拿到 3xx 就该停；默认策略会再跟
        // 最多 10 跳，把后面几跳的用时也算进来
        let client = self
            .bounded_builder(via, timeout)
            .and_then(|b| build(b.redirect(reqwest::redirect::Policy::none())))
            .map_err(|_| ProbeError::Other("other".into()))?;
        let start = Instant::now();
        let r = client
            .get(url)
            .timeout(timeout)
            .send()
            .map_err(|e| classify(&e))?;
        let elapsed = start.elapsed();
        // 不读正文：r 在这里丢掉，连接随之关闭
        Ok(Probe {
            code: r.status().as_u16(),
            elapsed,
        })
    }

    fn download_via(&self, url: &str, via: Via, max_bytes: u64, cap: Duration) -> Result<Download> {
        let client = build(self.bounded_builder(via, cap)?)?;
        let start = Instant::now();
        let mut r = client
            .get(url)
            .timeout(cap)
            .send()
            .map_err(|e| request_error(url, e))?;
        ensure_2xx(url, r.status().as_u16())?;
        let mut buf = vec![0u8; 64 * 1024];
        let mut bytes = 0u64;
        while bytes < max_bytes && start.elapsed() < cap {
            let want = (max_bytes - bytes).min(buf.len() as u64) as usize;
            match r.read(&mut buf[..want]) {
                Ok(0) => break,
                Ok(n) => bytes += n as u64,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                // 请求级总时限在读正文时以超时报出来：这就是「cap 到点就停」
                Err(e) if read_timed_out(&e) || start.elapsed() >= cap => break,
                Err(e) => return Err(read_error(url, e)),
            }
        }
        Ok(Download {
            bytes,
            elapsed: start.elapsed(),
            complete: bytes >= max_bytes,
        })
    }
}

/// 非 2xx 视为失败；错误里只有脱敏后的 URL 与状态码。
fn ensure_2xx(url: &str, code: u16) -> Result<()> {
    if (200..300).contains(&code) {
        return Ok(());
    }
    Err(Error::Net {
        url: redact_url(url),
        detail: format!("HTTP {code}"),
    })
}

/// 读正文时的 io 错误：里面包着 reqwest 的错误就照样经 [`request_error`] 去掉 URL；
/// 否则只留错误种类（`ErrorKind` 的描述里没有 URL）。
fn read_error(url: &str, e: std::io::Error) -> Error {
    let kind = e.kind();
    match e
        .into_inner()
        .map(|inner| inner.downcast::<reqwest::Error>())
    {
        Some(Ok(re)) => request_error(url, *re),
        _ => Error::Net {
            url: redact_url(url),
            detail: kind.to_string(),
        },
    }
}

/// 读正文时撞上了时限：请求级总时限到点，或单次 read 超时。
fn read_timed_out(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::TimedOut
        || e.get_ref()
            .and_then(|inner| inner.downcast_ref::<reqwest::Error>())
            .is_some_and(|re| re.is_timeout())
}

/// reqwest 错误 → 探测失败的类别（spec §7.5）。
fn classify(e: &reqwest::Error) -> ProbeError {
    if e.is_timeout() {
        ProbeError::Timeout
    } else if e.is_connect() {
        connect_failure(&source_chain(e))
    } else if e.is_body() || e.is_decode() {
        ProbeError::Other("body".into())
    } else if e.is_request() || e.is_redirect() {
        ProbeError::Other("http".into())
    } else {
        ProbeError::Other("other".into())
    }
}

/// 错误链的文本，从最外层的下一层开始（最外层的 Display 带 ` for url (…)`）。
/// 只拿来分类，不进错误、不进日志。
fn source_chain(e: &reqwest::Error) -> String {
    let mut out = String::new();
    let mut cur = std::error::Error::source(e);
    while let Some(s) = cur {
        out.push_str(&s.to_string());
        out.push('\n');
        cur = s.source();
    }
    out
}

/// 证书与 TLS 告警（rustls 的原文）。先于解析失败判断：证书错误的文本里带主机名。
const TLS_MARKS: &[&str] = &[
    "certificate",
    "tls",
    "fatal alert",
    "peer is incompatible",
    "peer misbehaved",
    "corrupt message",
];

/// 解析失败：只认直连时 hyper-util 报的 `dns error`。
///
/// SOCKS 应答 0x01（hyper-util 写成 `general server failure`）**不算**解析失败：sing-box 的
/// SOCKS 入站只把 ENETUNREACH / EHOSTUNREACH / ECONNREFUSED / EPERM 四个 errno 映射成具体应答码，
/// 其余错误一律回 0x01。于是「节点地址解析失败」和「拨节点服务器超时」（vless 的 5 秒拨号超时，
/// 比隧道 8 秒、网站 6 秒的时限都短，它先到）回的是同一个字节、同一段文本，客户端无从区分。
/// 宁可笼统说「连不上」，也不说一个可能是假的「域名解析失败」把人带去查 DNS。
/// builder 用的是 socks5h，本地解析那一路的报错碰不到，不收。
const DNS_MARKS: &[&str] = &["dns"];

/// 连接阶段的失败按错误链文本分（spec §7.5）：TLS → `Other("tls")`；有 refused → `Refused`；
/// 直连的解析失败 → `Dns`；其余（SOCKS 应答 0x01、不可达、本地测速端口没人听、SOCKS 握手或
/// 认证失败、连接被重置等）→ `Other("connect")`，不能报成「服务器拒绝连接」。
fn connect_failure(chain: &str) -> ProbeError {
    let c = chain.to_ascii_lowercase();
    let has = |marks: &[&str]| marks.iter().any(|m| c.contains(m));
    if has(TLS_MARKS) {
        ProbeError::Other("tls".into())
    } else if c.contains("refused") {
        ProbeError::Refused
    } else if has(DNS_MARKS) {
        ProbeError::Dns
    } else {
        ProbeError::Other("connect".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_errors_never_carry_the_full_url() {
        // 订阅与 /api/nodes 路径的最后一段是用户名（等价凭据）。reqwest 的错误文本自带
        // `for url (<完整 URL>)`，原样塞进 detail 就绕过了 redact_url，「失败：…」里会带出用户名。
        // 用 ftp:// 拿一个真实的、带完整 URL 的 reqwest 错误，不开任何套接字（见 bad_scheme_error）。
        let url = "ftp://panel.example.com/api/nodes/alice-secret";
        let t = Duration::from_secs(2);
        let errs = [
            ReqwestNet
                .text(url, t)
                .expect_err("text 不该成功")
                .to_string(),
            ReqwestNet
                .bytes(url, t)
                .expect_err("bytes 不该成功")
                .to_string(),
            ReqwestNet
                .status(url, Via::Direct, t)
                .expect_err("status 不该成功")
                .to_string(),
        ];
        for msg in &errs {
            assert!(!msg.contains("alice-secret"), "错误信息带出了用户名：{msg}");
            assert!(
                msg.contains("panel.example.com"),
                "主机还要留着，排障要看：{msg}"
            );
        }
    }

    #[test]
    fn net_errors_never_carry_the_url() {
        // 新增的三个方法同样不许把完整 URL 带进错误（j-risk ⑦）。ftp:// 在建连接之前就被
        // reqwest 以 builder 错误拒掉：错误文本自带完整 URL，但不开任何套接字（单元测试不联网）。
        let url = "ftp://panel.example.com/api/nodes/alice-secret";
        let t = Duration::from_secs(2);
        let raw = bad_scheme_error(url).to_string();
        assert!(
            raw.contains("alice-secret"),
            "前提：reqwest 原文带 URL：{raw}"
        );
        let errs = [
            ReqwestNet
                .text_via(url, Via::Socks5 { port: 1080 }, t)
                .expect_err("text_via 不该成功")
                .to_string(),
            ReqwestNet
                .download_via(url, Via::Direct, 1_000, t)
                .expect_err("download_via 不该成功")
                .to_string(),
        ];
        for msg in &errs {
            assert!(!msg.contains("alice-secret"), "错误信息带出了用户名：{msg}");
            assert!(msg.contains("panel.example.com"), "主机还要留着：{msg}");
        }
        // probe 的失败只有类别词，不带任何原文
        assert_eq!(
            ReqwestNet.probe(url, Via::Direct, t),
            Err(ProbeError::Other("other".into()))
        );
    }

    #[test]
    fn connect_failures_are_classified_from_the_error_chain() {
        // 链文本照 hyper-util 0.1.20（直连的 dns / tcp 错误、SOCKS 应答与握手）与 rustls 0.23 的原文写。
        // 规则照 spec §7.5：有 refused 才算拒绝；只有直连的 dns error 算解析失败；SOCKS 应答 0x01
        // （general server failure）与其余连接错误一律 Other("connect")，理由见 DNS_MARKS 的注释。
        let tls = ProbeError::Other("tls".into());
        let connect = ProbeError::Other("connect".into());
        let cases = [
            (
                "client error (Connect)\ndns error\n\
                 failed to lookup address information: Name or service not known",
                ProbeError::Dns,
            ),
            (
                "client error (Connect)\nerror connecting to socks proxy\n\
                 SOCKS error: general server failure",
                connect.clone(),
            ),
            (
                "client error (Connect)\nerror connecting to socks proxy\n\
                 SOCKS error: connection refused",
                ProbeError::Refused,
            ),
            (
                "client error (Connect)\ntcp connect error\nConnection refused (os error 111)",
                ProbeError::Refused,
            ),
            (
                // 本地测速端口没人听：是本机的问题，不是服务器拒绝
                "client error (Connect)\nerror connecting to socks proxy\n\
                 SOCKS error: failed to create underlying connection",
                connect.clone(),
            ),
            (
                "client error (Connect)\nerror connecting to socks proxy\n\
                 SOCKS error: host unreachable",
                connect.clone(),
            ),
            (
                "client error (Connect)\nerror connecting to socks proxy\n\
                 SOCKS error: credentials not accepted",
                connect.clone(),
            ),
            (
                "client error (Connect)\nConnection reset by peer (os error 104)",
                connect.clone(),
            ),
            (
                // socks5h 下碰不到（这是本地解析模式的错误）；万一出现也不按解析失败算
                "client error (Connect)\nerror connecting to socks proxy\n\
                 SOCKS error: could not resolve to acceptable address type",
                connect,
            ),
            (
                // 证书错误里带主机名；主机名里有 dns 也不能被判成解析失败
                "client error (Connect)\ninvalid peer certificate: \
                 certificate not valid for name \"dns.example.com\"",
                tls.clone(),
            ),
            (
                "client error (Connect)\nreceived fatal alert: HandshakeFailure",
                tls,
            ),
        ];
        for (chain, want) in cases {
            assert_eq!(connect_failure(chain), want, "{chain}");
        }
    }

    #[test]
    fn socks_general_failure_is_a_connect_failure_not_a_dns_failure() {
        // sing-box 的 SOCKS 入站对「节点地址解析失败」和「拨节点服务器超时」回同一个字节 0x01，
        // hyper-util 0.1.20 都写成 general server failure：客户端分不出是哪一种，只能算连不上。
        // 链文本照 hyper-util 的 SocksError / Status 原文（外层 error connecting to socks proxy）。
        let socks_0x01 = [
            "client error (Connect)\nerror connecting to socks proxy\n\
             SOCKS error: general server failure",
            // 大小写与外层措辞变了也一样：判据是 0x01 那半句，不是整句
            "client error (Connect)\nSOCKS error: General Server Failure",
        ];
        for chain in socks_0x01 {
            assert_eq!(
                connect_failure(chain),
                ProbeError::Other("connect".into()),
                "{chain}"
            );
        }
        // 直连（TUN 模式、百度直连）的本机解析失败仍是 Dns：hyper-util 报 dns error
        let direct_dns = [
            "client error (Connect)\ndns error\n\
             failed to lookup address information: Name or service not known",
            "client error (Connect)\ndns error\n\
             failed to lookup address information: nodename nor servname provided, or not known",
        ];
        for chain in direct_dns {
            assert_eq!(connect_failure(chain), ProbeError::Dns, "{chain}");
        }
    }

    /// ftp:// 在建连接之前就被 reqwest 以 builder 错误拒掉：拿到一个 Display 带完整 URL 的
    /// 真实 reqwest 错误，却不开任何套接字（Global Constraints：单元测试不联网，包括回环）。
    fn bad_scheme_error(url: &str) -> reqwest::Error {
        reqwest::blocking::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(url)
            .send()
            .expect_err("ftp:// 不该发得出去")
    }

    #[test]
    fn body_read_helpers_keep_urls_out_and_spot_timeouts() {
        let url = "ftp://panel.example.com/api/nodes/alice-secret";
        let e = bad_scheme_error(url);
        assert!(
            e.to_string().contains("alice-secret"),
            "前提：reqwest 原文带 URL：{e}"
        );
        // source_chain 从最外层的下一层取，最外层带的 ` for url (…)` 不在里面
        let chain = source_chain(&e);
        assert!(chain.contains("scheme"), "链里应有下一层的原因：{chain}");
        assert!(
            !chain.contains("alice-secret") && !chain.contains("panel.example.com"),
            "链文本带出了 URL：{chain}"
        );
        // 读正文的 io 错误：包着 reqwest 错误的经 request_error；没包的只留错误种类
        let wrapped = std::io::Error::other(bad_scheme_error(url));
        assert!(
            wrapped.to_string().contains("alice-secret"),
            "前提：包进去的原文带 URL"
        );
        let plain = std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            format!("reset while reading {url}"),
        );
        for err in [read_error(url, wrapped), read_error(url, plain)] {
            let Error::Net { url: shown, detail } = err else {
                panic!("读正文的错误应是 Error::Net");
            };
            assert!(!shown.contains("alice-secret"), "URL 没脱敏：{shown}");
            assert!(
                !detail.contains("alice-secret") && !detail.contains("panel.example.com"),
                "detail 里既不该有凭据也不该有 URL：{detail}"
            );
        }
        // 超时判定
        use std::io::{Error as IoError, ErrorKind};
        assert!(read_timed_out(&IoError::from(ErrorKind::TimedOut)));
        assert!(!read_timed_out(&IoError::from(ErrorKind::ConnectionReset)));
        assert!(
            !read_timed_out(&IoError::other(bad_scheme_error(url))),
            "包着的 reqwest 错误不是超时"
        );
    }
}
