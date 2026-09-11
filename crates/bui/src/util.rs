//! 纯时间与格式化工具（不依赖 Host，任何任务都能用）。
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// RFC3339（UTC，秒级）。格式化失败返回空串，绝不 panic。
pub fn fmt_rfc3339(t: OffsetDateTime) -> String {
    t.replace_nanosecond(0)
        .unwrap_or(t)
        .format(&Rfc3339)
        .unwrap_or_default()
}

/// 解析 RFC3339；不合法返回 `None`。
pub fn parse_rfc3339(s: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(s, &Rfc3339).ok()
}

/// 给人看的时长：`0s` / `59s` / `1m 30s` / `1h 2m` / `1d 1h`。
pub fn human_duration(secs: u64) -> String {
    let (d, h, m, s) = (
        secs / 86400,
        secs % 86400 / 3600,
        secs % 3600 / 60,
        secs % 60,
    );
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use time::macros::datetime;

    #[test]
    fn rfc3339_round_trips() {
        let t = datetime!(2026-09-11 00:01:02 UTC);
        assert_eq!(fmt_rfc3339(t), "2026-09-11T00:01:02Z");
        assert_eq!(parse_rfc3339("2026-09-11T00:01:02Z"), Some(t));
        assert_eq!(parse_rfc3339("not a time"), None);
    }

    #[test]
    fn durations_are_human_readable() {
        assert_eq!(human_duration(59), "59s");
        assert_eq!(human_duration(3725), "1h 2m");
        assert_eq!(human_duration(90), "1m 30s");
        assert_eq!(human_duration(90061), "1d 1h");
        assert_eq!(human_duration(0), "0s");
    }
}
