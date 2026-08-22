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
    t.format(LAYOUT).unwrap_or_else(|_| "1970-01-01T00:00:00.000Z".to_string())
}

pub fn parse(s: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()
}
