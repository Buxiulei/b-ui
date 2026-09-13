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
        let r = self
            .client(Via::Direct, timeout)?
            .get(url)
            .send()
            .map_err(|e| request_error(url, e))?;
        let code = r.status().as_u16();
        if !(200..300).contains(&code) {
            return Err(Error::Net {
                url: redact_url(url),
                detail: format!("HTTP {code}"),
            });
        }
        r.text().map_err(|e| request_error(url, e))
    }

    fn bytes(&self, url: &str, timeout: Duration) -> Result<Vec<u8>> {
        let r = self
            .client(Via::Direct, timeout)?
            .get(url)
            .send()
            .map_err(|e| request_error(url, e))?;
        let code = r.status().as_u16();
        if !(200..300).contains(&code) {
            return Err(Error::Net {
                url: redact_url(url),
                detail: format!("HTTP {code}"),
            });
        }
        Ok(r.bytes().map_err(|e| request_error(url, e))?.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_errors_never_carry_the_full_url() {
        // 订阅与 /api/nodes 路径的最后一段是用户名（等价凭据）。reqwest 的错误文本自带
        // `for url (<完整 URL>)`，原样塞进 detail 就绕过了 redact_url，「失败：…」里会带出用户名。
        // 127.0.0.1:1 没人监听，连接立刻被拒：拿到一个真实的 reqwest 错误，不出网。
        let url = "http://127.0.0.1:1/api/nodes/alice-secret";
        let e = ReqwestNet
            .text(url, Duration::from_secs(2))
            .expect_err("端口 1 不该连得上");
        let msg = e.to_string();
        assert!(!msg.contains("alice-secret"), "错误信息带出了用户名：{msg}");
        assert!(msg.contains("127.0.0.1"), "主机还要留着，排障要看：{msg}");
    }
}
