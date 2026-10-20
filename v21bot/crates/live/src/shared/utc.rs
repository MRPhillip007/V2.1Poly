//! UTC times as the journal, the tables and the messages write them: `2026-10-02 03:15:00`, and ISO 8601
//! with milliseconds as Python's `isoformat(timespec='milliseconds')` gives it
//! (`2026-10-02T03:15:00.000+00:00`).

use crate::shared::clock::local_ms;

/// The date of a day counted from 1970-01-01, proleptic Gregorian (H. Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// Unix seconds `s` as `YYYY-MM-DD HH:MM:SS`, UTC.
pub fn fmt_utc(s: i64) -> String {
    let (y, m, d) = civil_from_days(s.div_euclid(86_400));
    let t = s.rem_euclid(86_400);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60)
}

/// Unix ms as ISO 8601 with milliseconds, UTC.
pub fn iso_ms(ms: i64) -> String {
    let (y, m, d) = civil_from_days(ms.div_euclid(86_400_000));
    let t = ms.rem_euclid(86_400_000);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}+00:00", t / 3_600_000, t / 60_000 % 60, t / 1000 % 60, t % 1000)
}

/// This machine's clock now, as `iso_ms`.
pub fn now_iso() -> String {
    iso_ms(local_ms())
}
