//! 进程锁（spec §8.3、§0.2 R11）。**本任务是桩**：[`acquire`] 永远给得到一把
//! [`LockGuard`]，什么都不锁；T12a 换成真的 `flock`（`/run/bui-c.lock`），签名不再变。
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
    /// 本任务的桩：不持有任何东西，Drop 时什么都不做。
    pub fn stub() -> Self {
        Self { release: None }
    }

    /// T12a 用：`release` 里持有真正的锁（或 FakeSys 的记账）。
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

/// 拿锁。`Ok(None)` = 没拿到（等不到 / 别人正持着）；`Err` = 锁文件本身出了问题。
///
/// 桩实现永远 `Ok(Some(..))`，且一个系统调用都不发：调用点与顺序现在就排好，T12a 只换本体。
pub fn acquire<S: Sys>(_sys: &S, _paths: &Paths, _how: How) -> Result<Option<LockGuard>> {
    Ok(Some(LockGuard::stub()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeSys;
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn the_stub_lock_is_handed_out_and_released_on_drop() {
        let s = FakeSys::new();
        let p = Paths::new("/opt/bui-c", "/etc/systemd/system");
        for how in [How::Wait(Duration::from_secs(15)), How::Once] {
            assert!(acquire(&s, &p, how).unwrap().is_some(), "桩永远拿得到");
        }
        assert!(s.calls().is_empty(), "桩不碰系统：{:?}", s.calls());
        // 放锁 = 把 guard 扔掉；T12a 的真锁靠这条路释放 flock
        let hits = Rc::new(Cell::new(0));
        let h = hits.clone();
        let g = LockGuard::new(Box::new(move || h.set(h.get() + 1)));
        assert_eq!(hits.get(), 0, "还持着就不能放");
        drop(g);
        assert_eq!(hits.get(), 1);
    }
}
