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
}
