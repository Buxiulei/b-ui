//! 运行时数据 `runtime.json`：可丢可重建，落盘 best-effort（spec §2.1）。
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

/// 守护进程的运行时数据。`extra` 必须是最后一个字段（`flatten` 会吃掉所有未知键）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RuntimeData {
    /// 已落盘配置的重启判据（`restart_key` → 上次写盘时的值）。
    #[serde(default)]
    pub restart_keys: BTreeMap<String, String>,
    #[serde(default)]
    pub watchdog: BTreeMap<String, WatchdogRecord>,
    #[serde(default)]
    pub cert_sha256: Option<String>,
    #[serde(default)]
    pub drift: Vec<DriftItem>,
    #[serde(default)]
    pub last_reconcile: Option<ReconcileReport>,
    #[serde(default)]
    pub started_at: Option<String>,
    /// 每日自检发现的新版本号（spec §7），`None` = 已是最新
    #[serde(default)]
    pub upgrade_available: Option<String>,
    /// P2/P3 追加的运行时字段（健康 streak、黑名单候选计数、上次选中上游、采样游标，spec §2.1）
    /// 直接落在这里，P2/P3 不必回头改本任务的文件
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// 单个受管单元的 watchdog 状态（spec §3.4）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct WatchdogRecord {
    pub fails: u32,
    pub restarts: u32,
    pub last_restart_at: Option<String>,
    pub backoff_until: Option<String>,
}

/// 漂移项：非受管的改动只报不改（spec §2.3）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DriftItem {
    pub kind: String,
    pub path: String,
    pub detail: String,
}

/// 一轮对账的报告，落 `runtime.json` 供 `/api/health` 与 `bui status` 读。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ReconcileReport {
    #[serde(default)]
    pub at: String,
    #[serde(default)]
    pub changed: Vec<String>,
    #[serde(default)]
    pub restarted: Vec<String>,
    #[serde(default)]
    pub notes: Vec<String>,
    #[serde(default)]
    pub verify_failures: Vec<String>,
    #[serde(default)]
    pub errors: Vec<String>,
    #[serde(default)]
    pub drift: Vec<DriftItem>,
    #[serde(default)]
    pub dry_run: bool,
    /// 本轮 `b-ui.service` 自身需要重启（apply 从不在同步重启循环里动它，见 Task 5 第 12 步）；
    /// 由调用方执行：CLI 路径在报告落盘后 `systemctl restart b-ui`，守护进程内用 `restart --no-block`
    #[serde(default)]
    pub self_restart_required: bool,
}

impl ReconcileReport {
    /// 「什么都没发生」：只看 changed/restarted/errors/verify_failures，
    /// `notes` 与 `drift` 不影响（提示与漂移都是只报不改）。
    pub fn is_clean(&self) -> bool {
        self.changed.is_empty()
            && self.restarted.is_empty()
            && self.errors.is_empty()
            && self.verify_failures.is_empty()
    }
}

/// `runtime.json` 的进程内持有者。
#[derive(Clone)]
pub struct Runtime(Arc<Inner>);

struct Inner {
    path: PathBuf,
    data: RwLock<RuntimeData>,
}

impl Runtime {
    pub fn load(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let data = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!(error = %e, path = %path.display(), "runtime.json 解析失败，按空白重建");
                RuntimeData::default()
            }),
            Err(_) => RuntimeData::default(),
        };
        Self(Arc::new(Inner {
            path,
            data: RwLock::new(data),
        }))
    }

    pub async fn read(&self) -> RuntimeData {
        self.0.data.read().await.clone()
    }

    /// 落盘是 best-effort：runtime.json 丢了能从零重建（spec §2.1）。
    /// 走 `store::write_atomic`（tmp + 0600 + rename）并放进 `spawn_blocking`——本方法在守护进程的
    /// 每一轮对账、watchdog 的每 60 秒都会被调，直接 `std::fs::write` 会阻塞 runtime 线程，而
    /// 「先 write 再 chmod」在这两次系统调用之间会把 `runtime.json` 暴露成 0644（里面有 `restart_keys`
    /// 与上一轮报告）。Global Constraints 要求它恒为 0600。
    pub async fn update(&self, f: impl FnOnce(&mut RuntimeData)) -> RuntimeData {
        let mut guard = self.0.data.write().await;
        f(&mut guard);
        let snapshot = guard.clone();
        drop(guard);
        match serde_json::to_vec_pretty(&snapshot) {
            Ok(bytes) => {
                let path = self.0.path.clone();
                let joined = tokio::task::spawn_blocking(move || {
                    crate::state::store::write_atomic(&path, &bytes)
                })
                .await;
                match joined {
                    Ok(Err(e)) => tracing::warn!(error = %e, "runtime.json 落盘失败（忽略）"),
                    Err(e) => tracing::warn!(error = %e, "runtime.json 落盘任务 panic（忽略）"),
                    Ok(Ok(())) => {}
                }
            }
            Err(e) => tracing::warn!(error = %e, "runtime.json 序列化失败（忽略）"),
        }
        snapshot
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn missing_file_loads_defaults() {
        let d = tempfile::tempdir().unwrap();
        let rt = Runtime::load(d.path().join("runtime.json"));
        assert_eq!(rt.read().await, RuntimeData::default());
    }

    #[tokio::test]
    async fn corrupt_file_loads_defaults_without_panicking() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("runtime.json");
        std::fs::write(&p, b"{ this is not json").unwrap();
        let rt = Runtime::load(&p);
        assert_eq!(rt.read().await.restart_keys.len(), 0);
    }

    #[tokio::test]
    async fn update_persists_and_reloads() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("runtime.json");
        let rt = Runtime::load(&p);
        rt.update(|r| {
            r.restart_keys.insert("xray-config".into(), "abc123".into());
            r.watchdog.insert(
                "hysteria-server".into(),
                WatchdogRecord {
                    fails: 1,
                    restarts: 2,
                    last_restart_at: Some("2026-09-11T00:00:00Z".into()),
                    backoff_until: None,
                },
            );
        })
        .await;
        let back = Runtime::load(&p).read().await;
        assert_eq!(back.restart_keys["xray-config"], "abc123");
        assert_eq!(back.watchdog["hysteria-server"].restarts, 2);
    }

    #[tokio::test]
    async fn unknown_fields_survive_a_round_trip() {
        // P2/P3 会往 runtime.json 里加自己的字段；P1 读写不能把它们吃掉
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("runtime.json");
        std::fs::write(&p, br#"{"cert_sha256":"abc","resi_streak":{"u1":3}}"#).unwrap();
        let rt = Runtime::load(&p);
        assert_eq!(
            rt.read().await.extra["resi_streak"],
            serde_json::json!({"u1": 3})
        );
        rt.update(|r| r.upgrade_available = Some("4.0.1".into()))
            .await;
        let back: serde_json::Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        assert_eq!(back["resi_streak"], serde_json::json!({"u1": 3}));
        assert_eq!(back["upgrade_available"], "4.0.1");
        assert_eq!(back["cert_sha256"], "abc");
    }

    #[test]
    fn report_is_clean_only_when_nothing_happened() {
        let mut r = ReconcileReport {
            at: "2026-09-11T00:00:00Z".into(),
            ..Default::default()
        };
        assert!(r.is_clean());
        r.changed.push("/opt/b-ui/config.yaml".into());
        assert!(!r.is_clean());
    }
}
