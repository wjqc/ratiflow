use time::format_description::FormatItem;
use time::OffsetDateTime;

/// `2026-08-22T10:26:00.123Z`
const LAYOUT: &[FormatItem<'_>] = time::macros::format_description!(
    "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z"
);

pub fn now() -> String {
    format_now(OffsetDateTime::now_utc())
}

pub fn format_now(t: OffsetDateTime) -> String {
    t.format(LAYOUT)
        .unwrap_or_else(|_| "1970-01-01T00:00:00.000Z".to_string())
}

pub fn parse(s: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()
}

/// 当前时间 + N 天（RFC3339）。用于 receipt TTL 与 generation retention 截止。
pub fn now_plus_days(days: i64) -> String {
    let t = OffsetDateTime::now_utc() + time::Duration::days(days);
    format_now(t)
}

/// 当前时间 + N 分钟（RFC3339）。用于 migration job 租约。
pub fn now_plus_minutes(minutes: i64) -> String {
    let t = OffsetDateTime::now_utc() + time::Duration::minutes(minutes);
    format_now(t)
}

/// s（RFC3339）距今的整秒数，负值（未来时钟）截为 0。
/// s 不可解析时返回 i64::MAX：损坏的时间戳按"足够久远"处理，
/// 让幂等回执的接管路径可以自愈，而不是永久阻塞该 key。
pub fn age_secs(s: &str) -> i64 {
    let Some(t) = parse(s) else {
        return i64::MAX;
    };
    let now = OffsetDateTime::now_utc();
    (now - t).whole_seconds().max(0)
}
