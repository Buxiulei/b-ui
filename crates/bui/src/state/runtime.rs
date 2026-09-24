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
    /// 落盘串行化锁：见 [`Runtime::update`]。
    write: tokio::sync::Mutex<()>,
}

/// 临时文件名的进程内序号，配合 pid 保证每次落盘的临时文件互不相同（见 [`persist`]）。
static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 落盘一份快照：**独占**临时文件 + fsync + 0600 + rename，错误带上具体路径与 errno。
///
/// 事故（2026-09-12 bwg-rick，M5 回滚演练）：守护进程日志里有「runtime.json 落盘失败（忽略）」
/// 而看不出原因。根因是临时文件名固定：`state::store::write_atomic` 取
/// `path.with_extension("json.tmp")`，于是**同一个** `runtime.json.tmp` 被多个写者共用——
/// 守护进程里的 watchdog（每 60 秒）、对账循环、住宅巡检各自 `update()`，而 `update()` 在
/// 拿到快照后就放开了数据锁再去写盘；同一台机器上 `bui reconcile` / `bui status` 这些 CLI 进程
/// 也写同一个文件。两个写者交错时后者的 `create` 会截断前者的临时文件，先完成的那个 `rename`
/// 把它搬走，另一个的 `set_permissions`/`rename` 就 ENOENT —— 正是那句被吞掉的告警。
///
/// 修法两条：① 临时文件名带 pid + 进程内序号，谁也不踩谁；② 失败时把临时文件清掉，
/// 不在 `<base>` 里留下漂移扫描要报的垃圾（`.tmp` 后缀虽在 `TRANSIENT_SUFFIXES` 里，
/// 但留着毫无用处）。同进程内的写者顺序另由 [`Runtime::update`] 的 `write` 锁保证。
fn persist(path: &PathBuf, bytes: &[u8]) -> anyhow::Result<()> {
    use anyhow::Context;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{} 没有父目录", path.display()))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("创建目录 {} 失败", parent.display()))?;
    let seq = TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("runtime.json");
    let tmp = parent.join(format!(".{name}.{}.{seq}.tmp", std::process::id()));
    let r = (|| -> anyhow::Result<()> {
        let mut f = std::fs::File::create(&tmp)
            .with_context(|| format!("创建临时文件 {} 失败", tmp.display()))?;
        f.write_all(bytes)
            .with_context(|| format!("写入 {} 失败", tmp.display()))?;
        f.sync_all()
            .with_context(|| format!("fsync {} 失败", tmp.display()))?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 600 {} 失败", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .with_context(|| format!("rename {} → {} 失败", tmp.display(), path.display()))
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r
}

impl Runtime {
    pub fn load(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let data = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!(error = %e, path = %path.display(), "runtime.json 解析失败，按空白重建：{e}");
                RuntimeData::default()
            }),
            Err(_) => RuntimeData::default(),
        };
        Self(Arc::new(Inner {
            path,
            data: RwLock::new(data),
            write: tokio::sync::Mutex::new(()),
        }))
    }

    pub async fn read(&self) -> RuntimeData {
        self.0.data.read().await.clone()
    }

    /// 落盘是 best-effort：runtime.json 丢了能从零重建（spec §2.1）。
    /// 走 [`persist`]（独占 tmp + fsync + 0600 + rename）并放进 `spawn_blocking`——本方法在守护
    /// 进程的每一轮对账、watchdog 的每 60 秒都会被调，直接 `std::fs::write` 会阻塞 runtime 线程，而
    /// 「先 write 再 chmod」在这两次系统调用之间会把 `runtime.json` 暴露成 0644（里面有 `restart_keys`
    /// 与上一轮报告）。Global Constraints 要求它恒为 0600。
    ///
    /// `write` 锁在**整个读-改-写盘**期间持有（不是只在改内存那一段）：守护进程里有三四个任务
    /// 并发 `update()`，放开锁再写盘会让两次落盘交错，落在文件里的可能是**较旧**那份快照。
    /// 锁的粒度是「一次 update」，写盘本身在 `spawn_blocking` 里，不占 async 工作线程。
    pub async fn update(&self, f: impl FnOnce(&mut RuntimeData)) -> RuntimeData {
        let _write = self.0.write.lock().await;
        let mut guard = self.0.data.write().await;
        f(&mut guard);
        let snapshot = guard.clone();
        drop(guard);
        match serde_json::to_vec_pretty(&snapshot) {
            Ok(bytes) => {
                let path = self.0.path.clone();
                let joined = tokio::task::spawn_blocking(move || persist(&path, &bytes)).await;
                match joined {
                    // `{e:#}` 带上 anyhow 的整条 context 链（哪一步、哪个路径、errno）；
                    // 原来只有 `%e`，真机上就只剩一句「落盘失败（忽略）」查不下去
                    Ok(Err(e)) => tracing::warn!(
                        path = %self.0.path.display(),
                        error = format!("{e:#}"),
                        "runtime.json 落盘失败（忽略）：{e:#}"
                    ),
                    Err(e) => match join_error_kind(&e) {
                        // SIGTERM 退出时 runtime 关停会取消还没跑完的 blocking 任务，不是故障
                        JoinFailure::Cancelled => {
                            tracing::debug!("runtime.json 落盘任务被取消（进程退出中）")
                        }
                        JoinFailure::Panic => {
                            tracing::warn!(error = %e, "runtime.json 落盘任务 panic（忽略）：{e}")
                        }
                    },
                    Ok(Ok(())) => {}
                }
            }
            Err(e) => tracing::warn!(error = %e, "runtime.json 序列化失败（忽略）：{e}"),
        }
        snapshot
    }
}

/// `spawn_blocking(..).await` 的 `JoinError` 只有这两种来源。
#[derive(Debug, PartialEq, Eq)]
enum JoinFailure {
    Cancelled,
    Panic,
}

fn join_error_kind(e: &tokio::task::JoinError) -> JoinFailure {
    if e.is_cancelled() {
        JoinFailure::Cancelled
    } else {
        JoinFailure::Panic
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// 退出中被取消的落盘任务与真的 panic 走不同的日志级别：SIGTERM 退出时那条
    /// 「落盘任务 panic」WARN 其实是 cancelled，不该让人去查 panic。
    #[tokio::test]
    async fn join_error_kind_tells_cancelled_from_panic() {
        let panicked = tokio::task::spawn_blocking(|| panic!("boom"))
            .await
            .unwrap_err();
        assert_eq!(join_error_kind(&panicked), JoinFailure::Panic);

        let pending = tokio::spawn(std::future::pending::<()>());
        pending.abort();
        let cancelled = pending.await.unwrap_err();
        assert_eq!(join_error_kind(&cancelled), JoinFailure::Cancelled);
    }

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

    /// 事故排查（2026-09-12 bwg-rick「runtime.json 落盘失败（忽略）」）：并发 `update()`
    /// 一次都不许失败，最终文件必须是完整 JSON 且恒为 0600，`<base>` 里不留临时文件。
    ///
    /// 旧实现的临时文件名固定（`runtime.json.tmp`），并发写者互相截断 / rename 走对方的
    /// 临时文件 → ENOENT。这条测试跑的是「10 个任务同时 update」，旧实现下必留下失败痕迹
    /// （文件残缺或 `.tmp` 遗留）。
    #[tokio::test]
    async fn concurrent_updates_all_land_without_leaving_temp_files() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("runtime.json");
        let rt = Runtime::load(&p);
        let mut tasks = Vec::new();
        for i in 0..10u32 {
            let rt = rt.clone();
            tasks.push(tokio::spawn(async move {
                rt.update(|r| {
                    r.restart_keys.insert(format!("k{i}"), i.to_string());
                })
                .await
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }
        // 最终快照 = 内存态，且文件能解析回来（不是被截断的半份）
        let back = Runtime::load(&p).read().await;
        assert_eq!(back, rt.read().await);
        assert_eq!(back.restart_keys.len(), 10);
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let leftovers: Vec<_> = std::fs::read_dir(d.path())
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .filter(|n| n != "runtime.json")
            .collect();
        assert!(leftovers.is_empty(), "临时文件没清掉：{leftovers:?}");
    }

    /// 目录不存在也要能写（`persist` 自己 `create_dir_all`），失败时理由必须点名具体路径与步骤
    /// ——原来整条 context 链被 `%e` 吞成一句「落盘失败（忽略）」。
    #[tokio::test]
    async fn persist_creates_the_directory_and_reports_why_it_failed() {
        let d = tempfile::tempdir().unwrap();
        let nested = d.path().join("a/b/runtime.json");
        persist(&nested, b"{}").unwrap();
        assert_eq!(std::fs::read(&nested).unwrap(), b"{}");

        // 父路径是个**文件**：create_dir_all 必失败，错误里要有那条路径
        let blocked = d.path().join("afile");
        std::fs::write(&blocked, b"x").unwrap();
        let e = persist(&blocked.join("runtime.json"), b"{}").unwrap_err();
        let msg = format!("{e:#}");
        assert!(msg.contains("afile"), "{msg}");
        assert!(msg.contains("创建目录"), "{msg}");
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
