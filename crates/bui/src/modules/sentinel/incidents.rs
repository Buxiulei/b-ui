//! 事件（spec §5.7，设计裁决 D13）：`runtime.extra["incidents"]` 环形保留最近
//! [`INCIDENTS_MAX`] 条，**新的在前**。面板、`bui status`、`bui incidents` 都读这一份。

use super::INCIDENTS_MAX;
use crate::state::runtime::RuntimeData;
use serde::{Deserialize, Serialize};

/// `RuntimeData.extra` 里的键（落盘后就是 `runtime.json` 顶层的 `incidents` 数组）
pub const INCIDENTS_KEY: &str = "incidents";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// 探测通过 / 冷却外的复核结论：记一笔，不算故障
    Info,
    /// 已自动处置或只影响部分功能
    Warn,
    /// 需要管理员关注（上游不可达、鉴权全员失败、证书签不下来）
    Error,
}

/// 一条事件。字段即 spec §5.7 的「时间、单元、签名、对象、动作、结果」+ 等级 + 原文样本。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Incident {
    pub at: String,
    pub unit: String,
    pub signature: String,
    /// 对象的人读名：住宅上游是 `host:port`（**不带凭据**）、单元名、域名
    pub subject: String,
    pub action: String,
    pub result: String,
    pub level: Level,
    /// 触发它的那行日志（已脱敏、截断）；巡查类事件没有
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample: Option<String>,
}

/// 一个预案的结论：事件的 `subject` / `result` / `level` 由处理它的那一方决定
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub subject: String,
    pub result: String,
    pub level: Level,
}

/// 读全部事件（新的在前）；缺失或解析失败按空
pub fn from_runtime(rt: &RuntimeData) -> Vec<Incident> {
    rt.extra
        .get(INCIDENTS_KEY)
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

/// 追加一条到最前，截到 [`INCIDENTS_MAX`]
pub fn push(rt: &mut RuntimeData, inc: Incident) {
    let mut v = from_runtime(rt);
    v.insert(0, inc);
    v.truncate(INCIDENTS_MAX);
    if let Ok(x) = serde_json::to_value(&v) {
        rt.extra.insert(INCIDENTS_KEY.to_string(), x);
    }
}

impl Level {
    /// 终端与面板上的中文等级
    pub fn label(self) -> &'static str {
        match self {
            Level::Info => "信息",
            Level::Warn => "警告",
            Level::Error => "告警",
        }
    }
}

/// 一行人读：`<时间> [<等级>] <单元> <签名> <对象> → <动作>：<结果>`
pub fn format_line(i: &Incident) -> String {
    format!(
        "{} [{}] {} {} {} → {}：{}",
        i.at,
        i.level.label(),
        i.unit,
        i.signature,
        i.subject,
        i.action,
        i.result
    )
}

pub fn format_list(v: &[Incident]) -> String {
    if v.is_empty() {
        return "（暂无事件）".into();
    }
    v.iter().map(format_line).collect::<Vec<_>>().join("\n")
}

/// CLI 取最近 `n` 条：守护进程在跑就经 socket 调 `/api/incidents`，否则直接读 `runtime.json`。
/// 返回 `(事件, 是否来自守护进程)`。
pub async fn load_recent(
    socket: &std::path::Path,
    paths: &bui_schema::paths::Paths,
    n: usize,
) -> (Vec<Incident>, bool) {
    let client = crate::ipc::Client::new(socket);
    if client.available().await {
        if let Ok((200, v)) = client
            .request("GET", &format!("/api/incidents?limit={n}"), None)
            .await
        {
            if let Ok(r) = serde_json::from_value::<super::api::IncidentsResponse>(v) {
                return (r.incidents, true);
            }
        }
    }
    let rt = crate::state::runtime::Runtime::load(crate::paths::runtime_file(paths))
        .read()
        .await;
    (from_runtime(&rt).into_iter().take(n).collect(), false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn inc(n: usize) -> Incident {
        Incident {
            at: format!("2026-09-11T00:00:{:02}Z", n % 60),
            unit: "b-ui-relay".into(),
            signature: "relay_upstream_error".into(),
            subject: "isp2.example.net:10007".into(),
            action: "probe_and_borrow".into(),
            result: format!("第 {n} 条"),
            level: Level::Error,
            sample: None,
        }
    }

    #[test]
    fn push_keeps_the_newest_first_and_caps_the_ring() {
        let mut rt = RuntimeData::default();
        for n in 0..(INCIDENTS_MAX + 5) {
            push(&mut rt, inc(n));
        }
        let v = from_runtime(&rt);
        assert_eq!(v.len(), INCIDENTS_MAX);
        assert_eq!(
            v[0].result,
            format!("第 {} 条", INCIDENTS_MAX + 4),
            "新的在前"
        );
        assert_eq!(v[INCIDENTS_MAX - 1].result, "第 5 条", "最旧的 5 条被挤掉");
    }

    #[test]
    fn a_corrupt_section_reads_as_no_incidents() {
        let mut rt = RuntimeData::default();
        rt.extra
            .insert(INCIDENTS_KEY.into(), serde_json::json!({"not": "a list"}));
        assert!(from_runtime(&rt).is_empty());
    }

    #[test]
    fn the_wire_shape_has_a_lowercase_level_and_omits_an_empty_sample() {
        let v = serde_json::to_value(inc(1)).unwrap();
        assert_eq!(v["level"], "error");
        for (l, s) in [
            (Level::Info, "info"),
            (Level::Warn, "warn"),
            (Level::Error, "error"),
        ] {
            assert_eq!(serde_json::to_value(l).unwrap(), s);
            assert_eq!(
                serde_json::from_value::<Level>(serde_json::json!(s)).unwrap(),
                l
            );
        }
        assert!(v.get("sample").is_none());
        let mut with = inc(2);
        with.sample = Some("dial tcp 198.51.100.8:10007: i/o timeout".into());
        assert_eq!(
            serde_json::to_value(&with).unwrap()["sample"],
            "dial tcp 198.51.100.8:10007: i/o timeout"
        );
    }

    #[test]
    fn one_line_per_incident_for_the_terminal() {
        let mut i = inc(7);
        i.result = "IP 198.51.100.8 不可达，槽 1 已临时切到 198.51.100.7".into();
        assert_eq!(
            format_line(&i),
            "2026-09-11T00:00:07Z [告警] b-ui-relay relay_upstream_error isp2.example.net:10007 \
             → probe_and_borrow：IP 198.51.100.8 不可达，槽 1 已临时切到 198.51.100.7"
        );
        assert_eq!(format_list(&[]), "（暂无事件）");
        assert_eq!(format_list(&[inc(1), inc(2)]).lines().count(), 2);
    }

    /// 守护进程没跑（socket 连不上）⇒ 直接读 runtime.json，并如实报告来源
    #[tokio::test]
    async fn load_recent_falls_back_to_runtime_json_when_the_daemon_is_down() {
        let d = tempfile::tempdir().unwrap();
        let paths = bui_schema::paths::Paths {
            base_dir: d.path().to_path_buf(),
            certs_dir: d.path().join("certs"),
            bin_dir: d.path().join("bin"),
        };
        let rt = crate::state::runtime::Runtime::load(crate::paths::runtime_file(&paths));
        rt.update(|r| {
            for n in 0..8 {
                push(r, inc(n));
            }
        })
        .await;
        let (v, live) = load_recent(&d.path().join("no.sock"), &paths, 5).await;
        assert!(!live);
        assert_eq!(v.len(), 5);
        assert_eq!(v[0].result, "第 7 条");
    }
}
