//! `auth-snapshot.json`：hysteria 鉴权钩子（`bui auth-hook`）唯一读取的文件。
//!
//! 形状**逐字照总纲 C5**：顶层只有 `schema` 与 `users`，`users` 的键是用户名。
//! 它不是对账的 `Artifact`（P1 不比对它的内容，只把它放进 `BASE_WHITELIST`），
//! 由本模块在每次用户 / 限额变化时原子重写（tmp + 0600 + rename）。

use bui_schema::model::{Protocol, State};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use uuid::Uuid;

pub const SNAPSHOT_SCHEMA: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub schema: u32,
    pub users: BTreeMap<String, SnapshotUser>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotUser {
    pub user_id: String,
    pub hy2_password: String,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub blocked: bool,
}

impl Snapshot {
    pub fn empty() -> Self {
        Self {
            schema: SNAPSHOT_SCHEMA,
            users: BTreeMap::new(),
        }
    }

    /// 期望态 + 本轮判定的拒绝集合 → 快照。只收有 `hysteria2` 权益的用户
    /// （只有 Reality 权益的用户建不了 QUIC 连接，放进来只会让「快照里有他
    /// ⇒ 他能用 HY2」这条读法出错）。
    pub fn from_state(state: &State, blocked: &BTreeSet<Uuid>) -> Self {
        let users = state
            .users
            .iter()
            .filter(|u| u.entitlements.protocols.contains(&Protocol::Hysteria2))
            .map(|u| {
                (
                    u.username.clone(),
                    SnapshotUser {
                        user_id: u.user_id.to_string(),
                        hy2_password: u.credentials.hy2_password.clone(),
                        expires_at: u.entitlements.expires_at.clone(),
                        blocked: u.disabled || blocked.contains(&u.user_id),
                    },
                )
            })
            .collect();
        Self {
            schema: SNAPSHOT_SCHEMA,
            users,
        }
    }

    /// pretty JSON + 末尾换行。
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut v = serde_json::to_vec_pretty(self).expect("快照序列化不会失败");
        v.push(b'\n');
        v
    }

    /// [`Snapshot::to_bytes`] 的 sha256 十六进制（64 字符）。
    pub fn sha256(&self) -> String {
        hex::encode(Sha256::digest(self.to_bytes()))
    }
}

/// 读快照；文件缺失或解析失败 → [`Snapshot::empty`]
/// （钩子那边靠 fail-closed 兜底，不靠这里）。
pub fn read(path: &Path) -> Snapshot {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|_| Snapshot::empty()),
        Err(_) => Snapshot::empty(),
    }
}

/// 原子重写：内容与磁盘一致就**不写**（返回 `Ok(false)`）。写 = tmp + 0600 + rename。
pub fn write_if_changed(path: &Path, snap: &Snapshot) -> anyhow::Result<bool> {
    let bytes = snap.to_bytes();
    if std::fs::read(path).map(|old| old == bytes).unwrap_or(false) {
        return Ok(false);
    }
    // 与 state.json / runtime.json 同一条落盘路径：tmp + 0600 + rename
    crate::state::store::write_atomic(path, &bytes)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::sample_state;
    use pretty_assertions::assert_eq;
    use std::os::unix::fs::PermissionsExt;

    fn uid(s: &State, name: &str) -> Uuid {
        s.users.iter().find(|x| x.username == name).unwrap().user_id
    }

    #[test]
    fn shape_is_exactly_c5() {
        let s = sample_state();
        let snap = Snapshot::from_state(&s, &BTreeSet::new());
        let v: serde_json::Value = serde_json::from_slice(&snap.to_bytes()).unwrap();
        assert_eq!(v["schema"], 1);
        assert_eq!(
            v["users"]["alice"],
            serde_json::json!({
                "user_id": "8d5a1a1e-3b2c-4d1e-9f00-0000000000aa",
                "hy2_password": "pw-alice-01",
                "expires_at": serde_json::Value::Null,
                "blocked": false
            })
        );
        assert_eq!(v.as_object().unwrap().len(), 2, "顶层只有 schema 与 users");
    }

    #[test]
    fn empty_snapshot_is_schema_plus_empty_users_not_an_empty_object() {
        let v: serde_json::Value = serde_json::from_slice(&Snapshot::empty().to_bytes()).unwrap();
        assert_eq!(v, serde_json::json!({"schema": 1, "users": {}}));
    }

    #[test]
    fn blocked_and_expiry_come_from_the_caller_and_the_entitlements() {
        let mut s = sample_state();
        s.users[0].entitlements.expires_at = Some("2026-10-01T00:00:00Z".into());
        let a = uid(&s, "alice");
        let snap = Snapshot::from_state(&s, &BTreeSet::from([a]));
        let u = &snap.users["alice"];
        assert_eq!(u.expires_at.as_deref(), Some("2026-10-01T00:00:00Z"));
        assert!(u.blocked, "拒绝集合里的用户必须标 blocked");
        // disabled 自己也要生效，不能只依赖调用方把它放进 blocked 集合
        let mut s2 = sample_state();
        s2.users[0].disabled = true;
        assert!(Snapshot::from_state(&s2, &BTreeSet::new()).users["alice"].blocked);
    }

    #[test]
    fn reality_only_users_are_not_in_the_snapshot() {
        let mut s = sample_state();
        s.users[0].entitlements.protocols = vec![bui_schema::model::Protocol::Reality];
        assert!(Snapshot::from_state(&s, &BTreeSet::new()).users.is_empty());
    }

    #[test]
    fn writes_0600_atomically_and_skips_an_unchanged_write() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("auth-snapshot.json");
        let snap = Snapshot::from_state(&sample_state(), &BTreeSet::new());
        assert!(write_if_changed(&p, &snap).unwrap(), "第一次必须写");
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(!write_if_changed(&p, &snap).unwrap(), "内容不变不写");
        // 没有残留的 .tmp（write_atomic 的 rename 语义）
        let names: Vec<String> = std::fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["auth-snapshot.json".to_string()]);
        let mut s2 = sample_state();
        s2.users[0].credentials.hy2_password = "pw-alice-02".into();
        assert!(write_if_changed(&p, &Snapshot::from_state(&s2, &BTreeSet::new())).unwrap());
        assert_eq!(read(&p).users["alice"].hy2_password, "pw-alice-02");
    }

    #[test]
    fn reading_a_missing_or_broken_file_yields_an_empty_snapshot() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(read(&d.path().join("nope.json")), Snapshot::empty());
        let bad = d.path().join("bad.json");
        std::fs::write(&bad, b"{not json").unwrap();
        assert_eq!(read(&bad), Snapshot::empty());
    }

    #[test]
    fn sha256_changes_with_content_and_is_stable_across_calls() {
        let a = Snapshot::from_state(&sample_state(), &BTreeSet::new());
        let mut s = sample_state();
        s.users[0].disabled = true;
        let b = Snapshot::from_state(&s, &BTreeSet::new());
        assert_eq!(a.sha256(), a.sha256());
        assert_ne!(a.sha256(), b.sha256());
        assert_eq!(a.sha256().len(), 64);
    }
}
