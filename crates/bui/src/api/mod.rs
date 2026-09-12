//! HTTP API。Task 4 追加 `pub use state::{AppState, Event, EventBus};`，Task 13 追加 `router()`。
pub mod auth;
pub mod health;
pub mod state;
pub mod system;

// `AppState` 与 `EventBus` 本任务就有调用方（`Module::routes` / `DaemonCtx.bus`），`Event` 的第一个
// 调用方在 Task 13 的 `POST /api/reconcile`。bin crate 里 `pub` 不对外可达，所以还没人用的那个名字会被
// `unused_imports` 判成死导入——与 `main.rs` 顶部 `#![allow(dead_code)]` 是同一个「被调用方先于调用方
// 合并」的处境，只是分属两条 lint。这一行随那条 crate 级 allow 一起在 Task 17 收口时删掉。
#[allow(unused_imports)]
pub use state::{AppState, Event, EventBus};
