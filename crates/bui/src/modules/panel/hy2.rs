//! 两个 hysteria 实例的 trafficStats HTTP API 客户端（spec §4.2、调研 H10–H13）。
//!
//! - `GET /traffic?clear=1`：读取与清零在内核的同一把锁里完成，返回的就是**增量**，不丢不重（H10）。
//! - `GET /online`：值是该 id 当前的 QUIC 连接数，`>0` 即在线（H11）。
//! - `POST /kick`：体是 JSON 字符串数组；只是**标记**，要等该用户下次有流量才断连（H12）。
//! - `trafficStats.secret` 非空时，`Authorization` 头的值**精确等于** secret，**没有** `Bearer ` 前缀（H13）。
//!   `bui_schema::render::hysteria` 目前渲染空串，所以默认不发这个头。

use super::{Hy2Api, TxRx};
use std::collections::BTreeMap;
use std::time::Duration;

pub const REQ_TIMEOUT: Duration = Duration::from_secs(3);

pub struct Hy2Client {
    base: String,
    secret: String,
    http: reqwest::Client,
}

impl Hy2Client {
    pub fn new() -> Self {
        Self::with_base("http://127.0.0.1")
    }

    pub fn with_base(base: impl Into<String>) -> Self {
        Self {
            base: base.into(),
            secret: super::HY2_STATS_SECRET.to_string(),
            http: reqwest::Client::builder()
                .timeout(REQ_TIMEOUT)
                .build()
                .expect("reqwest 客户端构造不会失败"),
        }
    }

    pub fn with_secret(mut self, secret: impl Into<String>) -> Self {
        self.secret = secret.into();
        self
    }

    pub fn url(&self, port: u16, path: &str) -> String {
        format!("{}:{}{}", self.base.trim_end_matches('/'), port, path)
    }

    pub fn secret(&self) -> &str {
        &self.secret
    }

    fn with_auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match auth_header(&self.secret) {
            Some((k, v)) => rb.header(k, v),
            None => rb,
        }
    }
}

impl Default for Hy2Client {
    fn default() -> Self {
        Self::new()
    }
}

/// `trafficStats.secret` 非空时要发的头；**头值精确等于 secret，没有 `Bearer ` 前缀**（H13）。
pub fn auth_header(secret: &str) -> Option<(&'static str, String)> {
    (!secret.is_empty()).then(|| ("authorization", secret.to_string()))
}

/// `{"<id>":{"tx":n,"rx":n}}` → 表；坏条目跳过（H10 的返回体形状）。
pub fn parse_traffic(body: &str) -> BTreeMap<String, TxRx> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return BTreeMap::new();
    };
    let Some(obj) = v.as_object() else {
        return BTreeMap::new();
    };
    obj.iter()
        .filter_map(|(id, e)| {
            let tx = e.get("tx")?.as_u64()?;
            let rx = e.get("rx")?.as_u64()?;
            Some((id.clone(), TxRx { tx, rx }))
        })
        .collect()
}

/// `{"<id>":n}` → 表；`n <= 0` 视为不在线、不进表（H11）。
pub fn parse_online(body: &str) -> BTreeMap<String, u32> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return BTreeMap::new();
    };
    let Some(obj) = v.as_object() else {
        return BTreeMap::new();
    };
    obj.iter()
        .filter_map(|(id, n)| {
            let n = n.as_i64()?;
            (n > 0).then(|| (id.clone(), n as u32))
        })
        .collect()
}

/// `/kick` 的请求体：JSON 字符串数组（H12）。
pub fn kick_body(ids: &[String]) -> Vec<u8> {
    serde_json::to_vec(ids).expect("字符串数组序列化不会失败")
}

#[async_trait::async_trait]
impl Hy2Api for Hy2Client {
    async fn traffic_clear(&self, port: u16) -> anyhow::Result<BTreeMap<String, TxRx>> {
        let url = self.url(port, "/traffic?clear=1");
        let resp = self
            .with_auth(self.http.get(&url))
            .send()
            .await?
            .error_for_status()?;
        Ok(parse_traffic(&resp.text().await?))
    }

    async fn online(&self, port: u16) -> anyhow::Result<BTreeMap<String, u32>> {
        let url = self.url(port, "/online");
        let resp = self
            .with_auth(self.http.get(&url))
            .send()
            .await?
            .error_for_status()?;
        Ok(parse_online(&resp.text().await?))
    }

    async fn kick(&self, port: u16, ids: &[String]) -> anyhow::Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let url = self.url(port, "/kick");
        self.with_auth(self.http.post(&url))
            .header("content-type", "application/json")
            .body(kick_body(ids))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn urls_are_built_from_base_and_port() {
        let c = Hy2Client::new();
        assert_eq!(
            c.url(9999, "/traffic?clear=1"),
            "http://127.0.0.1:9999/traffic?clear=1"
        );
        assert_eq!(c.url(9998, "/online"), "http://127.0.0.1:9998/online");
        assert_eq!(
            Hy2Client::with_base("http://127.0.0.1/").url(9999, "/kick"),
            "http://127.0.0.1:9999/kick"
        );
    }

    #[test]
    fn traffic_and_online_bodies_parse_and_tolerate_junk() {
        let t = parse_traffic(r#"{"u-1":{"tx":10,"rx":20},"u-2":{"tx":0,"rx":0},"u-3":"junk"}"#);
        assert_eq!(t["u-1"], TxRx { tx: 10, rx: 20 });
        assert_eq!(t["u-2"], TxRx { tx: 0, rx: 0 });
        assert!(!t.contains_key("u-3"), "坏条目跳过，不让整轮采样失败");
        assert!(parse_traffic("not json").is_empty());
        let o = parse_online(r#"{"u-1":2,"u-2":0,"u-3":-1,"u-4":"x"}"#);
        assert_eq!(o, BTreeMap::from([("u-1".to_string(), 2u32)]));
        assert!(parse_online("{}").is_empty());
    }

    #[test]
    fn kick_body_is_a_json_string_array() {
        assert_eq!(
            kick_body(&["u-1".to_string(), "u-2".to_string()]),
            br#"["u-1","u-2"]"#.to_vec()
        );
        assert_eq!(kick_body(&[]), b"[]".to_vec());
    }

    #[test]
    fn the_auth_header_has_no_bearer_prefix_and_is_omitted_when_empty() {
        // 调研 H13：hysteria 要求头值**精确等于** secret
        assert_eq!(
            auth_header(""),
            None,
            "bui-schema 渲染的 secret 是空串 ⇒ 不发头"
        );
        assert_eq!(
            auth_header("s3cr3t"),
            Some(("authorization", "s3cr3t".to_string()))
        );
        assert_eq!(super::super::HY2_STATS_SECRET, "");
    }

    /// 起一个**进程内的回环** HTTP 服务（`127.0.0.1:0`，随机端口）当假 hysteria。
    /// 它不碰机器配置、不出网，只为把「方法 + 路径 + 查询串 + 头」这几件在 HTTP 线上的事
    /// 真的验一遍——这些恰恰是纯函数测不到、又最容易写错的地方
    /// （`?clear=1` 漏了就等于流量翻倍重复计）。
    async fn fake_hysteria(reply: &'static str) -> (u16, tokio::sync::mpsc::Receiver<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                // 必须**读满**再回包：hyper 对 POST 常把请求头与请求体分两次写，
                // 只 read 一次 4 KiB 的话 `req.ends_with(r#"["u-1"]"#)` 会偶发失败。
                // 先读到空行拿完整头，再按 content-length 补齐请求体。
                let mut raw: Vec<u8> = Vec::new();
                let mut chunk = vec![0u8; 4096];
                let head_end = loop {
                    match raw.windows(4).position(|w| w == b"\r\n\r\n") {
                        Some(i) => break i + 4,
                        None => {
                            let n = s.read(&mut chunk).await.unwrap_or(0);
                            if n == 0 {
                                break raw.len();
                            }
                            raw.extend_from_slice(&chunk[..n]);
                        }
                    }
                };
                let head = String::from_utf8_lossy(&raw[..head_end]).to_ascii_lowercase();
                let want = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                while raw.len() < head_end + want {
                    let n = s.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    raw.extend_from_slice(&chunk[..n]);
                }
                let _ = tx.send(String::from_utf8_lossy(&raw).to_string()).await;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    reply.len(),
                    reply
                );
                let _ = s.write_all(resp.as_bytes()).await;
                let _ = s.shutdown().await;
            }
        });
        (port, rx)
    }

    #[tokio::test]
    async fn traffic_clear_sends_get_with_clear_1_and_no_auth_header() {
        let (port, mut rx) = fake_hysteria(r#"{"u-1":{"tx":5,"rx":6}}"#).await;
        let c = Hy2Client::new();
        let got = c.traffic_clear(port).await.unwrap();
        assert_eq!(got["u-1"], TxRx { tx: 5, rx: 6 });
        let req = rx.recv().await.unwrap();
        assert!(
            req.starts_with("GET /traffic?clear=1 HTTP/1.1"),
            "请求行不对：{req}"
        );
        assert!(
            !req.to_ascii_lowercase().contains("authorization:"),
            "secret 为空时不应发 Authorization：{req}"
        );
    }

    #[tokio::test]
    async fn online_and_kick_use_the_documented_shapes_and_a_bare_secret_header() {
        let (port, mut rx) = fake_hysteria(r#"{"u-1":3}"#).await;
        let c = Hy2Client::new().with_secret("s3cr3t");
        assert_eq!(c.secret(), "s3cr3t", "with_secret 必须真的换掉默认空串");
        assert_eq!(Hy2Client::new().secret(), super::super::HY2_STATS_SECRET);
        assert_eq!(c.online(port).await.unwrap()["u-1"], 3);
        let req = rx.recv().await.unwrap();
        assert!(req.starts_with("GET /online HTTP/1.1"), "{req}");
        assert!(
            req.contains("authorization: s3cr3t"),
            "头值必须是裸 secret：{req}"
        );
        assert!(!req.contains("Bearer"), "绝不能带 Bearer 前缀：{req}");

        let (port2, mut rx2) = fake_hysteria("").await;
        c.kick(port2, &["u-1".to_string()]).await.unwrap();
        let req2 = rx2.recv().await.unwrap();
        assert!(req2.starts_with("POST /kick HTTP/1.1"), "{req2}");
        assert!(
            req2.ends_with(r#"["u-1"]"#),
            "请求体必须是 JSON 字符串数组：{req2}"
        );
    }

    #[tokio::test]
    async fn a_dead_instance_is_an_error_not_a_hang() {
        // 端口 1 上没人听：3 秒超时之内必须返回 Err（采样任务靠它把错误记进 /api/health）
        let c = Hy2Client::new();
        assert!(c.online(1).await.is_err());
    }
}
