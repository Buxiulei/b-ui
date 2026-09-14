//! 进程锁 `/run/bui-c.lock`（spec §8.3、§0.2 R11）：改配置的路径互斥，巡检遇锁跳过。
//! 真正加锁的是 [`Sys::try_lock`]（真机是 `flock`，测试是 `FakeSys` 的记账），这里只管
//! 「试一次」还是「每 250ms 试一次，等到时限」。
//!
//! 分层：只有顶层入口（菜单的一个动作、一条子命令、巡检的一轮）拿锁，改机器的函数
//! （`apply_with_ufw`、`cli::delete_nodes` 的数据面段、`teardown_all`、将来的 `converge`、
//! `update::install`）按引用收下 [`LockGuard`]，自己绝不拿锁，也绝不在持锁期间提问。

use crate::paths::Paths;
use crate::sys::Sys;
use crate::Result;
use std::time::Duration;

/// 拿到锁的凭证：不可 Clone、不可 Copy。Drop 时调用 `release`——真锁里那个闭包持着
/// `Flock<File>`（测试里记一条 `unlock`），所以「放锁」就是把它扔掉。
pub struct LockGuard {
    release: Option<Box<dyn FnOnce()>>,
}

impl LockGuard {
    /// 不持有任何锁、Drop 时什么都不做。只给单元测试直接调用「收下凭证」的函数时用；
    /// 产品代码一律经 [`acquire`] 拿。
    pub fn stub() -> Self {
        Self { release: None }
    }

    /// `release` 里持有真正的锁（真机是 `Flock<File>`，FakeSys 是记 `unlock` 的闭包）。
    pub fn new(release: Box<dyn FnOnce()>) -> Self {
        Self {
            release: Some(release),
        }
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        if let Some(f) = self.release.take() {
            f();
        }
    }
}

impl std::fmt::Debug for LockGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LockGuard")
    }
}

/// 拿不到锁时怎么办：等一段时间（菜单、命令行），还是只试一次（timer 的巡检，拿不到就
/// 跳过本轮）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    Wait(Duration),
    Once,
}

/// [`How::Wait`] 两次尝试之间隔多久。
pub const RETRY: Duration = Duration::from_millis(250);

/// 拿锁。`Ok(None)` = 没拿到（等不到 / 别人正持着）；`Err` = 锁文件本身出了问题（不是 root、
/// 锁文件被换成了符号链接）。
///
/// [`How::Once`] 只试一次、不睡；[`How::Wait`] 先试一次，之后每 [`RETRY`] 试一次，睡够时限
/// 还拿不到就放弃。等待走 [`Sys::sleep`]，测试里是虚拟时间。
pub fn acquire<S: Sys>(sys: &S, paths: &Paths, how: How) -> Result<Option<LockGuard>> {
    let path = paths.lock();
    if let Some(g) = sys.try_lock(&path)? {
        return Ok(Some(g));
    }
    let How::Wait(limit) = how else {
        return Ok(None);
    };
    let mut waited = Duration::ZERO;
    while waited < limit {
        let step = RETRY.min(limit - waited);
        sys.sleep(step);
        waited += step;
        if let Some(g) = sys.try_lock(&path)? {
            return Ok(Some(g));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeSys;
    use std::cell::Cell;
    use std::rc::Rc;

    fn paths() -> Paths {
        Paths::new("/opt/bui-c", "/etc/systemd/system")
    }

    #[test]
    fn the_guard_releases_on_drop_and_only_then() {
        let hits = Rc::new(Cell::new(0));
        let h = hits.clone();
        let g = LockGuard::new(Box::new(move || h.set(h.get() + 1)));
        assert_eq!(hits.get(), 0, "还持着就不能放");
        drop(g);
        assert_eq!(hits.get(), 1);
    }

    #[test]
    fn wait_retries_every_250ms_until_the_deadline_then_gives_up() {
        let s = FakeSys::new();
        s.lock_busy(u32::MAX);
        let got = acquire(&s, &paths(), How::Wait(Duration::from_secs(15))).unwrap();
        assert!(got.is_none(), "一直被占就等不到");
        let sleeps = s.sleeps();
        assert!(sleeps.iter().all(|&ms| ms == 250), "{sleeps:?}");
        assert_eq!(sleeps.iter().sum::<u64>(), 15_000, "正好等满 15 秒");
        assert!(!s.called("lock"), "{:?}", s.calls());
    }

    #[test]
    fn wait_takes_the_lock_as_soon_as_it_is_free() {
        let s = FakeSys::new();
        s.lock_busy(3);
        let g = acquire(&s, &paths(), How::Wait(Duration::from_secs(15))).unwrap();
        assert!(g.is_some());
        assert_eq!(s.sleeps(), vec![250, 250, 250], "第 4 次就拿到了");
        assert_eq!(s.calls(), vec!["lock".to_string()]);
        drop(g);
        assert_eq!(s.calls(), vec!["lock".to_string(), "unlock".to_string()]);
    }

    #[test]
    fn once_tries_exactly_once_and_never_sleeps() {
        let s = FakeSys::new();
        s.lock_busy(1);
        assert!(acquire(&s, &paths(), How::Once).unwrap().is_none());
        assert!(s.sleeps().is_empty(), "巡检只试一次，不等");
        assert!(acquire(&s, &paths(), How::Once).unwrap().is_some());
        assert!(s.sleeps().is_empty());
    }

    #[test]
    fn the_same_process_cannot_take_the_lock_twice() {
        // 真 flock 按「打开的文件」计：持着锁再开一次再锁，自己挡住自己。fake 照做，
        // 嵌套拿锁的写法在测试里就露馅
        let s = FakeSys::new();
        let first = acquire(&s, &paths(), How::Once)
            .unwrap()
            .expect("第一次拿得到");
        assert!(acquire(&s, &paths(), How::Once).unwrap().is_none());
        drop(first);
        assert!(acquire(&s, &paths(), How::Once).unwrap().is_some());
    }
}
