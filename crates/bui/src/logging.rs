//! tracing 初始化：优先 journald（systemd 下），失败回落 stderr。
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// 优先 journald（systemd 下），失败回落 stderr。默认 INFO。
pub fn init(level: Option<&str>) {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(level.unwrap_or("info")))
        .unwrap_or_else(|_| EnvFilter::new("info"));
    let registry = tracing_subscriber::registry().with(filter);
    match tracing_journald::layer() {
        Ok(journald) => registry.with(journald).init(),
        Err(_) => registry
            .with(tracing_subscriber::fmt::layer().with_target(false))
            .init(),
    }
}
