//! 期望态 `state.json` 的唯一持有者：临时文件 + rename 落盘，写盘前备份旧版。
use anyhow::{Context, Result};
use bui_schema::model::State;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

/// 备份保留份数（spec §2.1）。
pub const BACKUP_KEEP: usize = 10;

/// 单进程唯一持有 `State`；写 = 临时文件 + rename + 备份 10 份。
#[derive(Clone)]
pub struct Store(Arc<Inner>);

struct Inner {
    path: PathBuf,
    backups: PathBuf,
    cache: RwLock<Arc<State>>,
    write: Mutex<()>,
}

impl Store {
    /// 读现成的 `state.json`；文件缺失或内容不合 schema 一律报错（装机前的判据）。
    pub async fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let bytes =
            std::fs::read(&path).with_context(|| format!("读取 {} 失败", path.display()))?;
        let state: State = serde_json::from_slice(&bytes)
            .with_context(|| format!("解析 {} 失败", path.display()))?;
        Ok(Self::wrap(path, state))
    }

    /// 首次落盘（`bui install`）：没有旧版可备份，直接写。
    pub async fn create(path: impl Into<PathBuf>, state: State) -> Result<Self> {
        let path = path.into();
        let store = Self::wrap(path, state);
        let bytes = serde_json::to_vec_pretty(&*store.read().await)?;
        let inner = store.0.clone();
        tokio::task::spawn_blocking(move || write_atomic(&inner.path, &bytes)).await??;
        Ok(store)
    }

    fn wrap(path: PathBuf, state: State) -> Self {
        let backups = path
            .parent()
            .unwrap_or(Path::new("."))
            .join("state.backups");
        Self(Arc::new(Inner {
            path,
            backups,
            cache: RwLock::new(Arc::new(state)),
            write: Mutex::new(()),
        }))
    }

    pub async fn read(&self) -> Arc<State> {
        self.0.cache.read().await.clone()
    }

    /// 改期望态：串行化（`write` 锁）+ 零变更不写盘 + 写盘前备份上一版。
    pub async fn update(&self, f: impl FnOnce(&mut State)) -> Result<Arc<State>> {
        let _guard = self.0.write.lock().await;
        let current = self.read().await;
        let old_bytes = serde_json::to_vec_pretty(&*current)?;
        let mut next = (*current).clone();
        f(&mut next);
        let new_bytes = serde_json::to_vec_pretty(&next)?;
        if new_bytes == old_bytes {
            return Ok(current);
        }
        let inner = self.0.clone();
        let bytes = new_bytes.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            if inner.path.exists() {
                backup(&inner.path, &inner.backups)?;
            }
            write_atomic(&inner.path, &bytes)
        })
        .await??;
        let next = Arc::new(next);
        *self.0.cache.write().await = next.clone();
        Ok(next)
    }
}

/// `pub(crate)`：`state::runtime::Runtime::update` 复用它，保证 `runtime.json` 也是
/// 「tmp + 0600 + rename」，中途不会出现 0644 的窗口。
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// 旧版复制进 state.backups/，并裁到最近 BACKUP_KEEP 份。
fn backup(path: &Path, backups: &Path) -> Result<()> {
    std::fs::create_dir_all(backups)?;
    let stamp = time::OffsetDateTime::now_utc().format(&time::macros::format_description!(
        "[year][month][day]T[hour][minute][second]Z"
    ))?;
    // 文件名一律带**零填充**的三位计数后缀 `state-<stamp>-<nnn>.json`。不要写成「先试
    // `state-<stamp>.json`，撞了再加 `-1`、`-2`…」：那种命名下同一秒内的多次写盘会产生
    // `state-<stamp>-1.json`…`-13.json`，而 `-` < `.`、`-10` < `-2`，按路径字典序裁剪就会
    // 把**最新**的几份删掉，正好毁掉 `bui upgrade --rollback` 要恢复的那一份。零填充之后
    // 「字典序 == 时间序」。
    // 计数器取「同一 stamp 下已有的最大值 + 1」，**不是**第一个空位：裁剪会先删掉 `-000`，
    // 若按空位复用，同一秒内第 12 次写盘又会落回 `-000`，既让字典序不再等于时间序，也会在
    // 下一次裁剪里把刚写的那一份当最旧删掉（那正是 rollback 要的最新备份）。
    let prefix = format!("state-{stamp}-");
    let mut n = 0u32;
    for entry in std::fs::read_dir(backups)?.filter_map(|e| e.ok()) {
        let name = entry.file_name();
        let Some(rest) = name
            .to_string_lossy()
            .strip_prefix(&prefix)
            .map(String::from)
        else {
            continue;
        };
        if let Some(seq) = rest
            .strip_suffix(".json")
            .and_then(|s| s.parse::<u32>().ok())
        {
            n = n.max(seq + 1);
        }
    }
    std::fs::copy(path, backups.join(format!("state-{stamp}-{n:03}.json")))?;
    // 裁剪按 mtime 升序（取不到 mtime 的当最旧），路径作次序兜底：命名已保证字典序 == 时间序，
    // 两个判据同向，所以「删最旧的」在跨秒与同秒两种情形下都成立。
    let mut entries: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(backups)?
        .filter_map(|e| e.ok())
        .map(|e| {
            let mtime = e
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            (mtime, e.path())
        })
        .collect();
    entries.sort();
    while entries.len() > BACKUP_KEEP {
        let (_, oldest) = entries.remove(0);
        let _ = std::fs::remove_file(oldest);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::sample_state;
    use pretty_assertions::assert_eq;
    use std::os::unix::fs::PermissionsExt;

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[tokio::test]
    async fn create_then_read_round_trips() {
        let d = dir();
        let p = d.path().join("state.json");
        let store = Store::create(&p, sample_state()).await.unwrap();
        assert_eq!(store.read().await.node.ports.hy2, 10000);
        let reopened = Store::open(&p).await.unwrap();
        assert_eq!(reopened.read().await.users[0].username, "alice");
    }

    #[tokio::test]
    async fn state_file_is_0600() {
        let d = dir();
        let p = d.path().join("state.json");
        Store::create(&p, sample_state()).await.unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[tokio::test]
    async fn each_write_backs_up_the_previous_version() {
        let d = dir();
        let p = d.path().join("state.json");
        let store = Store::create(&p, sample_state()).await.unwrap();
        for i in 0..3u32 {
            store
                .update(|s| s.node.name = format!("node-{i}"))
                .await
                .unwrap();
        }
        let backups = std::fs::read_dir(d.path().join("state.backups"))
            .unwrap()
            .count();
        assert_eq!(backups, 3, "每次写入前备份旧版");
        assert_eq!(store.read().await.node.name, "node-2");
    }

    #[tokio::test]
    async fn backups_are_capped_at_ten() {
        let d = dir();
        let p = d.path().join("state.json");
        let store = Store::create(&p, sample_state()).await.unwrap();
        for i in 0..14u32 {
            store
                .update(|s| s.node.name = format!("node-{i}"))
                .await
                .unwrap();
        }
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(d.path().join("state.backups"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        files.sort(); // 文件名是 `state-<stamp>-<nnn>.json`，零填充 ⇒ 字典序 == 时间序
        assert_eq!(files.len(), BACKUP_KEEP);
        // 内容断言，不依赖时间戳落在同一秒：`update` 备份的是**上一版**，所以 14 次写盘留下
        // [sample, node-0 … node-12] 共 15 份候选里最新的 10 份 = node-3 … node-12。
        // 裁剪按字典序而命名又不零填充时（`-1`…`-13` 排在 `.json` 之前、`-10` 排在 `-2` 之前），
        // 这里会看到最新的几份被删掉——正是 `bui upgrade --rollback` 要恢复的那一份。
        let name_in = |p: &std::path::PathBuf| {
            serde_json::from_slice::<bui_schema::model::State>(&std::fs::read(p).unwrap())
                .unwrap()
                .node
                .name
        };
        assert_eq!(
            name_in(files.last().unwrap()),
            "node-12",
            "最新一份备份被裁掉了：{files:?}"
        );
        assert_eq!(
            name_in(files.first().unwrap()),
            "node-3",
            "裁掉的不是最旧的四份：{files:?}"
        );
    }

    #[tokio::test]
    async fn no_change_means_no_write_and_no_backup() {
        let d = dir();
        let p = d.path().join("state.json");
        let store = Store::create(&p, sample_state()).await.unwrap();
        store
            .update(|s| s.node.name = "changed".into())
            .await
            .unwrap();
        let before = std::fs::metadata(&p).unwrap().modified().unwrap();
        let count_before = std::fs::read_dir(d.path().join("state.backups"))
            .unwrap()
            .count();
        store.update(|_| {}).await.unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().modified().unwrap(), before);
        assert_eq!(
            std::fs::read_dir(d.path().join("state.backups"))
                .unwrap()
                .count(),
            count_before
        );
    }

    #[tokio::test]
    async fn concurrent_updates_are_serialized() {
        let d = dir();
        let p = d.path().join("state.json");
        let store = Store::create(&p, sample_state()).await.unwrap();
        let mut handles = Vec::new();
        for i in 0..20u32 {
            let s = store.clone();
            handles.push(tokio::spawn(async move {
                s.update(move |st| {
                    st.catalog.push(bui_schema::model::CatalogItem {
                        sku: format!("sku-{i}"),
                        title: "t".into(),
                        kind: bui_schema::model::CatalogKind::Plan,
                        region: None,
                        price_minor: 1,
                        period_days: None,
                    })
                })
                .await
                .unwrap();
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        assert_eq!(
            store.read().await.catalog.len(),
            20,
            "20 次并发更新不能互相覆盖"
        );
    }

    #[tokio::test]
    async fn open_missing_file_errors() {
        let d = dir();
        assert!(Store::open(d.path().join("nope.json")).await.is_err());
    }
}
