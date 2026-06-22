//! A minimal 5-field cron evaluator for recurrent tasks: `minute hour day-of-month month
//! day-of-week`. Enough to compute the next run after a given instant, with no dependency —
//! the task board only needs "when does this fire next", and recurrence skips missed slots
//! (we always ask for the next fire strictly after *now*, never replay the past).
//!
//! Supported per field: `*`, a number, a list `a,b,c`, a range `a-b`, and steps `*/n` /
//! `a-b/n`. Day-of-week is `0..=6` with `0` = Sunday (`7` also accepted as Sunday). When both
//! day-of-month and day-of-week are restricted, a day matches if *either* does (standard cron).

/// A parsed cron expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cron {
    minute: Vec<u32>,
    hour: Vec<u32>,
    dom: Vec<u32>,
    month: Vec<u32>,
    dow: Vec<u32>,
    dom_restricted: bool,
    dow_restricted: bool,
}

impl Cron {
    /// Parse a 5-field cron string. Returns `None` on the wrong field count or an
    /// out-of-range / malformed field.
    pub fn parse(expr: &str) -> Option<Cron> {
        let fields: Vec<&str> = expr.split_whitespace().collect();
        if fields.len() != 5 {
            return None;
        }
        let minute = parse_field(fields[0], 0, 59)?;
        let hour = parse_field(fields[1], 0, 23)?;
        let dom = parse_field(fields[2], 1, 31)?;
        let month = parse_field(fields[3], 1, 12)?;
        let dow = parse_dow(fields[4])?;
        Some(Cron {
            minute,
            hour,
            dom,
            month,
            dow,
            dom_restricted: fields[2] != "*",
            dow_restricted: fields[4] != "*",
        })
    }

    /// The next unix timestamp (seconds, UTC) strictly after `after` that matches, searching
    /// up to ~4 years ahead. `None` if nothing matches in that window (e.g. an impossible
    /// `30 4 31 2 *`).
    pub fn next_after(&self, after: i64) -> Option<i64> {
        // Start at the next whole minute strictly after `after`.
        let mut t = (after.div_euclid(60) + 1) * 60;
        for _ in 0..(366 * 4 * 24 * 60) {
            let (year, month, day, hour, minute, dow) = decompose(t);
            let _ = year;
            if self.minute.contains(&minute)
                && self.hour.contains(&hour)
                && self.month.contains(&month)
                && self.day_matches(day, dow)
            {
                return Some(t);
            }
            t += 60;
        }
        None
    }

    fn day_matches(&self, day: u32, dow: u32) -> bool {
        if self.dom_restricted && self.dow_restricted {
            self.dom.contains(&day) || self.dow.contains(&dow)
        } else {
            self.dom.contains(&day) && self.dow.contains(&dow)
        }
    }
}

fn parse_dow(spec: &str) -> Option<Vec<u32>> {
    let raw = parse_field(spec, 0, 7)?;
    // Normalise 7 → 0 (both mean Sunday) and dedupe.
    let mut values: Vec<u32> = raw
        .into_iter()
        .map(|v| if v == 7 { 0 } else { v })
        .collect();
    values.sort_unstable();
    values.dedup();
    Some(values)
}

/// Parse one comma-separated field into the set of allowed values within `[min, max]`.
fn parse_field(spec: &str, min: u32, max: u32) -> Option<Vec<u32>> {
    let mut values = Vec::new();
    for term in spec.split(',') {
        let (range, step) = match term.split_once('/') {
            Some((range, step)) => (range, step.parse::<u32>().ok().filter(|n| *n > 0)?),
            None => (term, 1),
        };
        let (lo, hi) = if range == "*" {
            (min, max)
        } else if let Some((a, b)) = range.split_once('-') {
            (a.parse().ok()?, b.parse().ok()?)
        } else {
            let n = range.parse().ok()?;
            (n, n)
        };
        if lo < min || hi > max || lo > hi {
            return None;
        }
        let mut v = lo;
        while v <= hi {
            values.push(v);
            v += step;
        }
    }
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    values.dedup();
    Some(values)
}

/// Decompose a unix timestamp (UTC) into `(year, month, day, hour, minute, weekday)`, with
/// weekday `0` = Sunday. Uses Howard Hinnant's civil-from-days algorithm.
fn decompose(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let hour = (sod / 3600) as u32;
    let minute = ((sod % 3600) / 60) as u32;
    let weekday = (days.rem_euclid(7) + 4).rem_euclid(7) as u32; // 1970-01-01 was a Thursday
    let (year, month, day) = civil_from_days(days);
    (year, month, day, hour, minute, weekday)
}

/// Format a unix timestamp as the same `YYYY-MM-DDTHH:MM:SS.000Z` shape SQLite's
/// `strftime('%Y-%m-%dT%H:%M:%fZ','now')` produces, so stored `next_run`s compare correctly
/// (lexicographically == chronologically) against `now` in the scheduler's SQL.
#[must_use]
pub fn to_rfc3339_millis(secs: i64) -> String {
    let (year, month, day, hour, minute, _) = decompose(secs);
    let second = secs.rem_euclid(60);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.000Z")
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-06-22T12:00:00Z — a Monday.
    const MON_NOON: i64 = 1_782_129_600;

    fn rfc3339(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
        decompose(secs)
    }

    #[test]
    fn decompose_is_correct_for_a_known_instant() {
        let (y, m, d, h, min, dow) = rfc3339(MON_NOON);
        assert_eq!((y, m, d, h, min), (2026, 6, 22, 12, 0));
        assert_eq!(dow, 1, "2026-06-22 is a Monday");
    }

    #[test]
    fn rejects_malformed_expressions() {
        assert!(Cron::parse("* * * *").is_none(), "too few fields");
        assert!(Cron::parse("60 * * * *").is_none(), "minute out of range");
        assert!(Cron::parse("* 24 * * *").is_none(), "hour out of range");
        assert!(Cron::parse("bad * * * *").is_none());
    }

    #[test]
    fn daily_at_eight_picks_next_morning() {
        let cron = Cron::parse("0 8 * * *").expect("parse");
        let next = cron.next_after(MON_NOON).expect("next");
        let (_, _, d, h, min, _) = decompose(next);
        // 12:00 is past 08:00, so the next fire is 08:00 the following day (the 23rd).
        assert_eq!((d, h, min), (23, 8, 0));
    }

    #[test]
    fn every_fifteen_minutes_steps_within_the_hour() {
        let cron = Cron::parse("*/15 * * * *").expect("parse");
        let next = cron.next_after(MON_NOON + 60).expect("next"); // 12:01 → 12:15
        let (_, _, _, h, min, _) = decompose(next);
        assert_eq!((h, min), (12, 15));
    }

    #[test]
    fn strictly_after_skips_the_current_matching_minute() {
        let cron = Cron::parse("0 12 * * *").expect("parse");
        // Exactly at 12:00 → the *next* fire is tomorrow's 12:00, not now.
        let next = cron.next_after(MON_NOON).expect("next");
        assert_eq!(next, MON_NOON + 86_400);
    }

    #[test]
    fn weekday_field_matches_the_right_day() {
        // Sundays at 00:00.
        let cron = Cron::parse("0 0 * * 0").expect("parse");
        let next = cron.next_after(MON_NOON).expect("next");
        let (_, _, _, h, min, dow) = decompose(next);
        assert_eq!((h, min, dow), (0, 0, 0), "next Sunday midnight");
    }

    #[test]
    fn dom_and_dow_both_restricted_is_an_or() {
        // 1st of the month OR any Friday, at noon.
        let cron = Cron::parse("0 12 1 * 5").expect("parse");
        let next = cron.next_after(MON_NOON).expect("next");
        let (_, _, d, _, _, dow) = decompose(next);
        assert!(d == 1 || dow == 5, "matched neither the 1st nor a Friday");
    }
}
