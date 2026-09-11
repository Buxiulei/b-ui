//! HTTP 出口收在 [`Net`] 一个 trait 后面：204 探测（可经本地 SOCKS）、取文本、取字节。
//! 生产用 [`ReqwestNet`]（`reqwest::blocking` + rustls，无 openssl），
//! 单元测试注入 [`FakeNet`](crate::fake::FakeNet)，于是测试不发真实请求。

use crate::error::redact_url;
use crate::{Error, Result};
use std::time::Duration;

/// 请求走哪条腿。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    Direct,
    Socks5 { port: u16 },
}

/// 可注入的 HTTP 客户端。
pub trait Net {
    /// 只要状态码，用于 `generate_204` 类探测。
    fn status(&self, url: &str, via: Via, timeout: Duration) -> Result<u16>;
    /// 直连取文本（manifest / 订阅）。非 2xx 视为失败。
    fn text(&self, url: &str, timeout: Duration) -> Result<String>;
    /// 直连取字节（二进制下载）。非 2xx 视为失败。
    fn bytes(&self, url: &str, timeout: Duration) -> Result<Vec<u8>>;
}

/// 生产实现。
pub struct ReqwestNet;

impl ReqwestNet {
    /// `no_proxy()` 是必须的：root shell 会 source /etc/profile.d/proxy.sh，
    /// 不关掉 env 代理的话「直连探测」会经隧道出去，误判成通。
    fn client(&self, via: Via, timeout: Duration) -> Result<reqwest::blocking::Client> {
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
        b.build().map_err(|e| Error::Net {
            url: "<client>".into(),
            detail: e.to_string(),
        })
    }
}

impl Net for ReqwestNet {
    fn status(&self, url: &str, via: Via, timeout: Duration) -> Result<u16> {
        let r = self
            .client(via, timeout)?
            .get(url)
            .send()
            .map_err(|e| Error::Net {
                url: redact_url(url),
                detail: e.to_string(),
            })?;
        Ok(r.status().as_u16())
    }

    fn text(&self, url: &str, timeout: Duration) -> Result<String> {
        let r = self
            .client(Via::Direct, timeout)?
            .get(url)
            .send()
            .map_err(|e| Error::Net {
                url: redact_url(url),
                detail: e.to_string(),
            })?;
        let code = r.status().as_u16();
        if !(200..300).contains(&code) {
            return Err(Error::Net {
                url: redact_url(url),
                detail: format!("HTTP {code}"),
            });
        }
        r.text().map_err(|e| Error::Net {
            url: redact_url(url),
            detail: e.to_string(),
        })
    }

    fn bytes(&self, url: &str, timeout: Duration) -> Result<Vec<u8>> {
        let r = self
            .client(Via::Direct, timeout)?
            .get(url)
            .send()
            .map_err(|e| Error::Net {
                url: redact_url(url),
                detail: e.to_string(),
            })?;
        let code = r.status().as_u16();
        if !(200..300).contains(&code) {
            return Err(Error::Net {
                url: redact_url(url),
                detail: format!("HTTP {code}"),
            });
        }
        Ok(r.bytes()
            .map_err(|e| Error::Net {
                url: redact_url(url),
                detail: e.to_string(),
            })?
            .to_vec())
    }
}
