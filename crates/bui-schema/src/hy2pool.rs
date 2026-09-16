//! 住宅 HY2 的静态凭据池（spec §3.1 / §4.1 / §4.3）：全是纯函数，不做 IO、不打日志
//! （迁移「只写个数不写凭据」的日志由 `bui` 侧调用方打）。
//!
//! 一条凭据 = [`ReservedCred`]，写进 sing-box 的 `users[].password` 是 `"{name}:{secret}"`；
//! 用户侧的指针是 [`Credentials::hy2_resi_cred`](crate::model::Credentials::hy2_resi_cred)。
//! 池容量恒为「2 × 住宅 hysteria2 用户数」向上取整到 16 的倍数，夹在
//! [`POOL_MIN`] / [`POOL_MAX`] 之间：空闲凭据是预留的门位，用户生命周期动作只切门、
//! 不改配置、不重启内核。
use crate::model::{Hy2Pool, Protocol, ReservedCred, Residential, State, User};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use std::collections::BTreeSet;
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

/// 池容量下限：再少的用户也预留这么多门位。
pub const POOL_MIN: usize = 32;
/// 池容量上限（**裁决值**，spec §14 裁决 2：128 凭据已实测 RSS 与 `check` 耗时不涨，
/// 256 离已实测点最近；512 属于未实测的推断上限）。凭据 id 域随它收成 `r000`…`r255`。
pub const POOL_MAX: usize = 256;
/// 空闲率告警门槛（spec §3.1：空闲 < 20% ⇒ 一次性告警 `hy2_resi_pool_low`）。
pub const LOW_FREE_RATIO: f64 = 0.20;

/// 释放后的冷却期（小时）：没过这么久的凭据不许再分配出去，
/// 否则前任持有人手里的旧订阅会直接连上新人的门。
const RECLAIM_COOLDOWN_HOURS: i64 = 24;
/// 新发 secret 的随机字节数（base64url 无填充 ⇒ 22 字符）。
const SECRET_BYTES: usize = 16;

/// 池容量 = `clamp(ceil16(2 × users), POOL_MIN, POOL_MAX)`（spec §3.1）。
pub fn size_for(users: usize) -> usize {
    (users.saturating_mul(2).div_ceil(16) * 16).clamp(POOL_MIN, POOL_MAX)
}

/// 「有住宅权益且开 hysteria2」的用户数 —— 池容量的基数。
pub fn resi_hy2_users(s: &State) -> usize {
    s.users.iter().filter(|u| is_resi_hy2(u)).count()
}

/// 把池补到 `target` 条（上限 [`POOL_MAX`]），返回新增条数。
///
/// `id` 取最小空闲 `r%03d`，`name = id`；与 `taken`（现有用户名 + 现有凭据名）冲突的值
/// 整个跳过 —— `validate_username` 允许 `r017` 这种用户名，撞上了 `auth_user` 就归错人。
pub fn grow(p: &mut Hy2Pool, target: usize, taken: &BTreeSet<String>) -> usize {
    let target = target.min(POOL_MAX);
    let mut added = 0;
    while p.creds.len() < target {
        let Some(id) = next_free_id(p, taken) else {
            break; // id 域用尽（256 条）
        };
        p.creds.push(ReservedCred {
            name: id.clone(),
            id,
            secret: new_secret(),
            released_at: None,
        });
        added += 1;
    }
    added
}

/// 给用户分一条空闲凭据，返回凭据 `id`；池里没有可用的（含「全是 24 小时内释放的」）
/// 返回 `None` —— 调用方据此扩容，**不拒绝建用户**（spec §3.1）。
///
/// **幂等**：已持凭据的用户原样拿回那一条。换凭据（rotate、spec §3.3）必须显式
/// [`release`] 再 `assign` —— 否则旧凭据会被静默孤立（`released_at` 仍是 `None` ⇒
/// 在 [`pick_free`] 里落到最高优先级的「从未用过」那一档，冷却期与「重写时重随机
/// secret」两道防线同时失效，前任持有人手里的旧订阅直接连上新人的门）。
pub fn assign(s: &mut State, user_id: Uuid) -> Option<String> {
    assign_at(s, user_id, OffsetDateTime::now_utc())
}

/// [`assign`] 的可注入时钟版本。**生产一律走这个**：24 小时冷却期是安全判据，
/// 判定时钟必须与 [`release`] 盖 `released_at` 的那个时钟同源（`bui` 侧的 `Host::now()`），
/// 否则一个偏移 / 冻结的时钟就能把冷却期静默作废，而且测不到。
pub fn assign_at(s: &mut State, user_id: Uuid, now: OffsetDateTime) -> Option<String> {
    let idx = s.users.iter().position(|u| u.user_id == user_id)?;
    if let Some(held) = s.users[idx].credentials.hy2_resi_cred.clone() {
        return Some(held); // 幂等：不许静默孤立仍然有效的凭据
    }
    let id = pick_free(&s.residential.hy2_pool, &used_ids(s), now)?;
    s.users[idx].credentials.hy2_resi_cred = Some(id.clone());
    Some(id)
}

/// 释放用户占的凭据：清 `hy2_resi_cred`、给凭据记 `released_at`，返回凭据 `id`。
/// 门切 `deny` 由调用方做（本模块不碰 IO）。
pub fn release(s: &mut State, user_id: Uuid, now: OffsetDateTime) -> Option<String> {
    let u = s.users.iter_mut().find(|u| u.user_id == user_id)?;
    let id = u.credentials.hy2_resi_cred.take()?;
    if let Some(c) = s.residential.hy2_pool.creds.iter_mut().find(|c| c.id == id) {
        c.released_at = Some(rfc3339(now));
    }
    Some(id)
}

/// 该用户当前占的那条凭据（没分过、或池里已经没有那个 id ⇒ `None`）。
pub fn cred_of<'a>(u: &User, r: &'a Residential) -> Option<&'a ReservedCred> {
    let id = u.credentials.hy2_resi_cred.as_deref()?;
    r.hy2_pool.creds.iter().find(|c| c.id == id)
}

/// 重随机全部空闲凭据的 `secret` 并清 `released_at`，返回改动条数。
///
/// 每次因 spec §3.5 四件事重写配置时顺带做：空闲凭据的旧密码只有前任持有人知道，
/// 重启是唯一能换掉它的时机。**在用的（`used` 里的）一条都不动。**
pub fn regenerate_idle_secrets(p: &mut Hy2Pool, used: &BTreeSet<String>) -> usize {
    let mut n = 0;
    for c in p.creds.iter_mut().filter(|c| !used.contains(&c.id)) {
        c.secret = new_secret();
        c.released_at = None;
        n += 1;
    }
    n
}

/// 空闲凭据条数（`used` = 已被用户占着的 id 集合）。
pub fn free_count(p: &Hy2Pool, used: &BTreeSet<String>) -> usize {
    p.creds.iter().filter(|c| !used.contains(&c.id)).count()
}

/// [`migrate`] 的结果：改动的用户数，以及**仍然没拿到凭据**的用户数。
///
/// `unassigned > 0` 时调用方必须打 Error 级事件（spec §3.1：空闲耗尽时建用户不拒绝，
/// 当场扩容 + 记 Error 事件）—— 这些用户 [`cred_of`] 恒为 `None`，渲染不出住宅 HY2 节点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MigrateReport {
    /// 这一轮拿到（或新建）凭据的用户数；零变更 = 幂等。
    pub changed: usize,
    /// 扩容之后仍然分不到凭据的住宅 hysteria2 用户数（id 域 [`POOL_MAX`] 用尽才会非零）。
    pub unassigned: usize,
}

/// 空闲耗尽时的当场扩容步长（spec §3.1：建用户不拒绝）。与 `size_for` 的取整基数同为 16。
const EMERGENCY_GROW_STEP: usize = 16;

/// v4 → 4.1 迁移（spec §4.3 的 1–3 步），**幂等**：
///
/// 1. 池为空 ⇒ 按 `created_at` 升序给每个住宅 hysteria2 用户建
///    `{ id: r%03d, name: 用户名, secret: hy2_password 的副本 }` —— 于是迁移用户的住宅
///    HY2 节点与今天逐字相同，v3 golden 与「升级零刷新」都成立；
/// 2. 把池补到 `size_for(resi_hy2_users(s))`（**先扩容再分配**：grow 放在分配之后时，
///    「空闲凭据全在冷却期内」这一档的目标容量早已达标、一条都不补，pending 用户会静默
///    地一直没有凭据）；
/// 3. 仍没有凭据的用户（升级后新建、或 4.0.x 回滚再升）从空闲里分配；分配不出来就按
///    spec §3.1 当场再扩容 [`EMERGENCY_GROW_STEP`] 条重试一轮，最终仍分不到的人数
///    走 [`MigrateReport::unassigned`] 报给调用方。
///
/// **直连的 `hy2_password` 不动**（spec §4.3 第 3 条）。
pub fn migrate(s: &mut State, now: OffsetDateTime) -> MigrateReport {
    let mut changed = 0;
    if s.residential.hy2_pool.creds.is_empty() {
        for uid in in_creation_order(s) {
            let taken = taken_names(s);
            let Some(id) = next_free_id(&s.residential.hy2_pool, &taken) else {
                break;
            };
            let Some(u) = s.users.iter_mut().find(|u| u.user_id == uid) else {
                continue;
            };
            let cred = ReservedCred {
                id: id.clone(),
                name: u.username.clone(),
                secret: u.credentials.hy2_password.clone(),
                released_at: None,
            };
            u.credentials.hy2_resi_cred = Some(id);
            s.residential.hy2_pool.creds.push(cred);
            changed += 1;
        }
    }
    let target = size_for(resi_hy2_users(s));
    let taken = taken_names(s);
    grow(&mut s.residential.hy2_pool, target, &taken);

    let mut pending: Vec<Uuid> = s
        .users
        .iter()
        .filter(|u| is_resi_hy2(u) && u.credentials.hy2_resi_cred.is_none())
        .map(|u| u.user_id)
        .collect();
    changed += assign_pending(s, &mut pending, now);
    if !pending.is_empty() {
        let bigger = (s.residential.hy2_pool.creds.len() + EMERGENCY_GROW_STEP).min(POOL_MAX);
        let taken = taken_names(s);
        grow(&mut s.residential.hy2_pool, bigger, &taken);
        changed += assign_pending(s, &mut pending, now);
    }
    MigrateReport {
        changed,
        unassigned: pending.len(),
    }
}

/// 给 `pending` 里的用户各分一条凭据，分到的从 `pending` 里剔掉，返回分出去的条数。
fn assign_pending(s: &mut State, pending: &mut Vec<Uuid>, now: OffsetDateTime) -> usize {
    let before = pending.len();
    pending.retain(|uid| assign_at(s, *uid, now).is_none());
    before - pending.len()
}

/// 「有住宅权益且开 hysteria2」—— 池容量、迁移与门位收敛共用的判据。
fn is_resi_hy2(u: &User) -> bool {
    u.entitlements.residential.is_some() && u.entitlements.protocols.contains(&Protocol::Hysteria2)
}

/// 住宅 hysteria2 用户的 id，按 `created_at` 升序（同刻按 `state.json` 里的原序）。
fn in_creation_order(s: &State) -> Vec<Uuid> {
    let mut v: Vec<(&str, Uuid)> = s
        .users
        .iter()
        .filter(|u| is_resi_hy2(u))
        .map(|u| (u.created_at.as_str(), u.user_id))
        .collect();
    v.sort_by_key(|(t, _)| *t); // 稳定排序：同一时刻保持原序
    v.into_iter().map(|(_, id)| id).collect()
}

/// 已被用户占着的凭据 id。
fn used_ids(s: &State) -> BTreeSet<String> {
    s.users
        .iter()
        .filter_map(|u| u.credentials.hy2_resi_cred.clone())
        .collect()
}

/// 不许被新发凭据撞上的名字：现有用户名 + 现有凭据名。
fn taken_names(s: &State) -> BTreeSet<String> {
    s.users
        .iter()
        .map(|u| u.username.clone())
        .chain(s.residential.hy2_pool.creds.iter().map(|c| c.name.clone()))
        .collect()
}

/// 最小空闲 `r%03d`（池里已有的 id 与 `taken` 都跳过）；id 域用尽 ⇒ `None`。
fn next_free_id(p: &Hy2Pool, taken: &BTreeSet<String>) -> Option<String> {
    let mine: BTreeSet<&str> = p.creds.iter().map(|c| c.id.as_str()).collect();
    (0..POOL_MAX)
        .map(|i| format!("r{i:03}"))
        .find(|id| !mine.contains(id.as_str()) && !taken.contains(id))
}

/// 回收顺序（spec §3.1）：先「从未用过」的（按 id 升序），其次 `released_at` 最早且
/// ≥ [`RECLAIM_COOLDOWN_HOURS`] 小时的；全是冷却期内释放的 ⇒ 视同耗尽（`None`）。
fn pick_free(p: &Hy2Pool, used: &BTreeSet<String>, now: OffsetDateTime) -> Option<String> {
    let idle = || p.creds.iter().filter(|c| !used.contains(&c.id));
    if let Some(c) = idle().find(|c| c.released_at.is_none()) {
        return Some(c.id.clone());
    }
    idle()
        .filter_map(|c| released_at(c).map(|t| (t, &c.id)))
        .filter(|(t, _)| now - *t >= Duration::hours(RECLAIM_COOLDOWN_HOURS))
        .min_by_key(|(t, _)| *t)
        .map(|(_, id)| id.clone())
}

/// `released_at` 解析成时刻。解析不了的（手工改坏了）当成「很久以前」：
/// 一个坏时间戳不该把凭据永久锁死在池里。
fn released_at(c: &ReservedCred) -> Option<OffsetDateTime> {
    c.released_at
        .as_deref()
        .map(|s| OffsetDateTime::parse(s, &Rfc3339).unwrap_or(OffsetDateTime::UNIX_EPOCH))
}

/// 16 字节随机 → base64url 无填充（22 字符，不含 `:`）。随机源与
/// [`crate::sub::new_sub_token`] 同一套。
fn new_secret() -> String {
    URL_SAFE_NO_PAD.encode(rand::random::<[u8; SECRET_BYTES]>())
}

/// RFC3339（UTC，秒级），口径同 [`crate::sub::legacy_sub_deadline`]。
fn rfc3339(t: OffsetDateTime) -> String {
    t.replace_nanosecond(0)
        .unwrap_or(t)
        .format(&Rfc3339)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;
    use pretty_assertions::assert_eq;
    use time::macros::datetime;
    use uuid::Uuid;

    fn user(n: u128, name: &str, resi: bool) -> User {
        let mut u: User = serde_json::from_str(&format!(
            r#"{{"user_id":"{}","username":"{name}","created_at":"2026-09-01T00:00:00Z",
                 "credentials":{{"hy2_password":"pw-{name}","vless_uuid":"{}"}},
                 "entitlements":{{"protocols":["hysteria2","reality"],"direct":true}}}}"#,
            Uuid::from_u128(n),
            Uuid::from_u128(1000 + n)
        ))
        .unwrap();
        if resi {
            u.entitlements.residential = Some(ResidentialEntitlement {
                group_id: DEFAULT_GROUP.into(),
                slot_id: None,
            });
        }
        u
    }

    fn state(n: usize) -> State {
        let mut s: State = serde_json::from_str(SAMPLE_STATE).unwrap();
        s.users = (0..n as u128)
            .map(|i| user(i + 1, &format!("u{i}"), true))
            .collect();
        s
    }

    /// 池容量始终 ≥ 2 倍用户数，向上取到 16 的倍数，并夹在 [32, 256]（spec §3.1、§14 裁决 2）
    #[test]
    fn pool_size_is_twice_the_users_rounded_up_to_16_and_clamped() {
        assert_eq!(size_for(0), POOL_MIN);
        assert_eq!(size_for(1), 32);
        assert_eq!(size_for(16), 32);
        assert_eq!(size_for(17), 48);
        assert_eq!(size_for(100), 208);
        assert_eq!(size_for(400), POOL_MAX);
        assert_eq!(
            POOL_MAX, 256,
            "上限 256 是裁决值（spec §14 裁决 2），不是 512"
        );
    }

    /// 迁移：每个「有住宅权益且开 hysteria2」的用户拿到 `name = 用户名`、
    /// `secret = 当时的 hy2_password` 的凭据，再补新凭据到 size_for；幂等
    #[test]
    fn migration_keeps_the_subscription_byte_identical_and_is_idempotent() {
        let mut s = state(3);
        let n = migrate(&mut s, datetime!(2026-09-15 00:00 UTC));
        assert_eq!((n.changed, n.unassigned), (3, 0));
        let u0 = &s.users[0];
        let c = cred_of(u0, &s.residential).expect("迁移后必有凭据");
        assert_eq!(c.name, "u0", "迁移用户的 name 就是用户名（订阅逐字不变）");
        assert_eq!(c.secret, "pw-u0", "secret 是当时的 hy2_password 的副本");
        assert_eq!(c.id, "r000");
        assert_eq!(u0.credentials.hy2_password, "pw-u0", "直连密码不动");
        assert_eq!(s.residential.hy2_pool.creds.len(), size_for(3));
        assert_eq!(
            migrate(&mut s, datetime!(2026-09-15 00:00 UTC)),
            MigrateReport::default(),
            "幂等：零变更、零漏分"
        );
    }

    /// 新发凭据：name = id、secret 是 22 字符 base64url（不含 ':'）
    #[test]
    fn newly_minted_creds_use_the_id_as_name_and_a_colon_free_secret() {
        let mut s = state(1);
        migrate(&mut s, datetime!(2026-09-15 00:00 UTC));
        let fresh = s
            .residential
            .hy2_pool
            .creds
            .iter()
            .find(|c| c.id != "r000")
            .unwrap();
        assert_eq!(fresh.name, fresh.id);
        assert_eq!(fresh.secret.len(), 22, "16 字节 base64url 无填充");
        assert!(!fresh.secret.contains(':'), "':' 会把 auth 串切错");
    }

    /// 回收：先给「从未用过」的，其次 released_at 最早且 ≥ 24 小时的；
    /// 全是 24 小时内释放的 ⇒ 视同耗尽（返回 None，由调用方扩容）
    #[test]
    fn recycling_prefers_never_used_then_released_over_24h() {
        let mut s = state(1);
        migrate(&mut s, datetime!(2026-09-15 00:00 UTC));
        s.residential.hy2_pool.creds.truncate(2); // r000（alice 用着）+ r001（从未用过）
        let mut newbie = user(9, "bob", true);
        newbie.credentials.hy2_resi_cred = None;
        s.users.push(newbie);
        assert_eq!(
            assign(&mut s, Uuid::from_u128(9)).as_deref(),
            Some("r001"),
            "先用从未用过的"
        );

        // bob 退订 ⇒ r001 记 released_at；1 小时后不许再发出去
        let t0 = datetime!(2026-09-15 00:00 UTC);
        assert_eq!(
            release(&mut s, Uuid::from_u128(9), t0).as_deref(),
            Some("r001")
        );
        let mut newbie2 = user(10, "carol", true);
        newbie2.credentials.hy2_resi_cred = None;
        s.users.push(newbie2);
        assert_eq!(
            assign_at(&mut s, Uuid::from_u128(10), t0 + time::Duration::hours(1)),
            None,
            "24 小时内释放的一律不发 ⇒ 视同耗尽"
        );
        assert_eq!(
            assign_at(&mut s, Uuid::from_u128(10), t0 + time::Duration::hours(25)).as_deref(),
            Some("r001"),
            "超过 24 小时才可再分配"
        );
    }

    /// 空闲凭据的 secret 在每次重写配置时重随机；**在用的一个都不许动**
    #[test]
    fn rewriting_the_config_rerolls_only_idle_secrets() {
        let mut s = state(1);
        migrate(&mut s, datetime!(2026-09-15 00:00 UTC));
        let used: std::collections::BTreeSet<String> = s
            .users
            .iter()
            .filter_map(|u| u.credentials.hy2_resi_cred.clone())
            .collect();
        let before: Vec<String> = s
            .residential
            .hy2_pool
            .creds
            .iter()
            .map(|c| c.secret.clone())
            .collect();
        let n = regenerate_idle_secrets(&mut s.residential.hy2_pool, &used);
        assert_eq!(n, before.len() - 1);
        let after = &s.residential.hy2_pool.creds;
        assert_eq!(after[0].secret, before[0], "r000 在用，不许动");
        assert_ne!(after[1].secret, before[1]);
        assert!(after[1].released_at.is_none(), "重随机顺带清 released_at");
    }

    /// 空闲率告警门槛（spec §3.1：空闲 < 20%）
    #[test]
    fn low_water_mark_is_twenty_percent() {
        let mut s = state(1);
        migrate(&mut s, datetime!(2026-09-15 00:00 UTC));
        let used: std::collections::BTreeSet<String> = s
            .users
            .iter()
            .filter_map(|u| u.credentials.hy2_resi_cred.clone())
            .collect();
        let free = free_count(&s.residential.hy2_pool, &used);
        assert_eq!(free, size_for(1) - 1);
        assert_eq!(LOW_FREE_RATIO, 0.20, "20% 是裁决值（spec §3.1）");
        assert!((free as f64) / (size_for(1) as f64) >= LOW_FREE_RATIO);

        // 越线一侧：32 条里占了 27 条 ⇒ 空闲 5 条、5/32 = 0.156 < 20% ⇒ 该告警
        let busy: BTreeSet<String> = s
            .residential
            .hy2_pool
            .creds
            .iter()
            .take(27)
            .map(|c| c.id.clone())
            .collect();
        let low = free_count(&s.residential.hy2_pool, &busy);
        assert_eq!(low, size_for(1) - 27);
        assert!(
            (low as f64) / (size_for(1) as f64) < LOW_FREE_RATIO,
            "空闲 < 20% ⇒ 哨兵一次性告警 hy2_resi_pool_low"
        );
    }

    /// `assign` 幂等：已持凭据的用户再分一次拿回原来那条，旧凭据不许被静默孤立
    /// （`released_at` 仍是 `None` ⇒ 立刻发给下一个人，前任的旧订阅连上新人的门）
    #[test]
    fn assigning_twice_returns_the_held_cred_instead_of_orphaning_it() {
        let mut s = state(1);
        let t0 = datetime!(2026-09-15 00:00 UTC);
        migrate(&mut s, t0);
        s.residential.hy2_pool.creds.truncate(2); // r000（u0 用着）+ r001（从未用过）
        let alice = Uuid::from_u128(1);
        assert_eq!(
            assign_at(&mut s, alice, t0).as_deref(),
            Some("r000"),
            "已持凭据 ⇒ 原样拿回，不再挑新的"
        );
        let held = s.users[0].credentials.hy2_resi_cred.as_deref();
        assert_eq!(held, Some("r000"));

        // r000 没被 release 过，所以它既不该出现在空闲里、更不该落到「从未用过」那一档
        let mut newbie = user(9, "bob", true);
        newbie.credentials.hy2_resi_cred = None;
        s.users.push(newbie);
        assert_eq!(
            assign_at(&mut s, Uuid::from_u128(9), t0).as_deref(),
            Some("r001"),
            "r000 仍归 u0，不许转手"
        );

        // 换凭据只能走显式两步：release 记 released_at（冷却期的地基）再 assign
        assert_eq!(release(&mut s, alice, t0).as_deref(), Some("r000"));
        let r000 = s
            .residential
            .hy2_pool
            .creds
            .iter()
            .find(|c| c.id == "r000")
            .unwrap();
        assert_eq!(
            r000.released_at.as_deref(),
            Some("2026-09-15T00:00:00Z"),
            "释放必记 released_at，否则 24 小时冷却期形同不存在"
        );
        assert_eq!(
            assign_at(&mut s, alice, t0 + time::Duration::hours(25)).as_deref(),
            Some("r000")
        );
    }

    /// 空闲凭据全在冷却期内 ⇒ 不许把「没拿到凭据」吞掉：当场扩容再试
    /// （spec §3.1：空闲耗尽时建用户不拒绝）
    #[test]
    fn migrate_grows_on_the_spot_when_every_idle_cred_is_still_cooling_down() {
        let mut s = state(1);
        let t0 = datetime!(2026-09-15 00:00 UTC);
        migrate(&mut s, t0);
        cool_down_idle(&mut s, t0);
        let before = s.residential.hy2_pool.creds.len();
        let mut newbie = user(9, "bob", true);
        newbie.credentials.hy2_resi_cred = None;
        s.users.push(newbie);

        // size_for(2) = 32 = 现有条数 ⇒ 常规 grow 一条都不补，只能靠当场扩容
        assert_eq!(size_for(resi_hy2_users(&s)), before);
        let r = migrate(&mut s, t0 + time::Duration::hours(1));
        assert_eq!((r.changed, r.unassigned), (1, 0), "当场扩容 ⇒ bob 拿到凭据");
        assert_eq!(
            s.residential.hy2_pool.creds.len(),
            before + EMERGENCY_GROW_STEP,
            "冷却期内耗尽 ⇒ 当场扩容 16 条"
        );
        assert!(
            cred_of(&s.users[1], &s.residential).is_some(),
            "否则 bob 永久没有住宅 HY2 凭据、渲染不出他的节点"
        );
    }

    /// 扩到 [`POOL_MAX`] 还分不出来 ⇒ 如实把人数报给调用方（T11 据此打 Error 级事件）
    #[test]
    fn migrate_reports_the_users_left_without_a_cred() {
        let mut s = state(1);
        let t0 = datetime!(2026-09-15 00:00 UTC);
        migrate(&mut s, t0);
        let taken = taken_names(&s);
        grow(&mut s.residential.hy2_pool, POOL_MAX, &taken);
        cool_down_idle(&mut s, t0);
        let mut newbie = user(9, "bob", true);
        newbie.credentials.hy2_resi_cred = None;
        s.users.push(newbie);

        let r = migrate(&mut s, t0 + time::Duration::hours(1));
        assert_eq!(
            (r.changed, r.unassigned),
            (0, 1),
            "id 域用尽也不许静默吞掉「没拿到凭据」"
        );
        assert_eq!(s.residential.hy2_pool.creds.len(), POOL_MAX, "id 域用尽");
        assert!(s.users[1].credentials.hy2_resi_cred.is_none());
    }

    /// 新发凭据跳过与现有用户名 / 凭据名冲突的值（spec §3.1：`validate_username` 允许
    /// `r017` 这种用户名，撞上了 `auth_user` 就归错人）
    #[test]
    fn minted_ids_skip_the_ones_that_collide_with_a_username() {
        let mut s = state(1);
        s.users[0].username = "r000".into();
        migrate(&mut s, datetime!(2026-09-15 00:00 UTC));
        let creds = &s.residential.hy2_pool.creds;
        assert_eq!(creds[0].name, "r000", "迁移用户的 name 仍是用户名");
        assert_eq!(creds[0].id, "r001", "id 跳过与用户名 r000 的冲突");
        assert!(
            creds.iter().all(|c| c.id != "r000"),
            "r000 这个 id 永不启用"
        );
        let names: BTreeSet<&str> = creds.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names.len(), creds.len(), "凭据 name 互不相同");
        assert_eq!(
            creds.iter().filter(|c| c.name == "r000").count(),
            1,
            "只有迁移用户自己那条撞用户名"
        );
    }

    /// 不变量：池里的凭据 `name` 永远互不相同（走公共 API 造得再满也不许重名）。
    /// 重名会渲染出两条一模一样的 `auth_user` 规则，sing-box 静默只认第一条 ⇒ 后一条
    /// 凭据的门永久失效（谁拿到它就走前一个人的门）、两人流量并进一个计数器，
    /// 而 `sing-box check` 退 0、渲染器也抓不到。
    #[test]
    fn cred_names_stay_unique_even_when_a_username_looks_like_an_id() {
        let mut s = state(5);
        s.users[2].username = "r005".into(); // 用户名恰好长成凭据 id 的样子
        migrate(&mut s, datetime!(2026-09-15 00:00 UTC));
        let taken = taken_names(&s);
        grow(&mut s.residential.hy2_pool, POOL_MAX, &taken);

        let creds = &s.residential.hy2_pool.creds;
        assert_eq!(
            creds.len(),
            POOL_MAX - 1,
            "r005 归用户名 ⇒ id 域少一格，grow 到此为止"
        );
        let names: BTreeSet<&str> = creds.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names.len(), creds.len(), "凭据 name 互不相同");
        let ids: BTreeSet<&str> = creds.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids.len(), creds.len(), "凭据 id 互不相同");
        assert!(!ids.contains("r005"), "被用户名占掉的 id 永不启用");
    }

    /// 把全部空闲凭据标成「刚释放」：24 小时内一条都不许发出去
    fn cool_down_idle(s: &mut State, now: OffsetDateTime) {
        let used = used_ids(s);
        for c in s
            .residential
            .hy2_pool
            .creds
            .iter_mut()
            .filter(|c| !used.contains(&c.id))
        {
            c.released_at = Some(rfc3339(now));
        }
    }

    const SAMPLE_STATE: &str = r#"{
      "schema_version": 1,
      "node": { "id": "8d5a1a1e-3b2c-4d1e-9f00-000000000001", "name": "node-a", "domain": "example.com", "public_ip": "203.0.113.10",
                "ports": { "hy2": 10000, "hy2_hop": [20000, 30000], "hy2_resi": 40000, "hy2_resi_hop": [41000, 50000],
                           "reality_direct": 10001, "reality_resi": 10002, "admin": 8080 },
                "reality": { "private_key": "a", "public_key": "b", "short_ids": ["0123456789abcdef"], "dest": "www.bing.com:443", "server_names": ["www.bing.com"] },
                "obfs": { "enabled": false, "password": "" } },
      "admin": { "password_hash": "h", "jwt_secret": "s" }
    }"#;
}
