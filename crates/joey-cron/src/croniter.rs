//! Croniter-compatible cron expression matcher.
//!
//! The `cron` crate has semantics incompatible with upstream hermes-agent's
//! croniter (DOW numbering, the Vixie DOM/DOW OR-rule, seconds-field
//! position), so this module implements the subset of croniter that hermes
//! cron jobs rely on:
//!
//! - 5 fields: `minute hour day-of-month month day-of-week`
//! - optional trailing 6th field: seconds (croniter's default 6-field form)
//! - `*`, lists (`,`), ranges (`-`, wrap-around allowed), steps (`/`)
//! - month names `jan`-`dec`, day names `sun`-`sat` (case-insensitive)
//! - day-of-week 0-7 where both 0 and 7 are Sunday
//! - Vixie OR-rule: when BOTH day-of-month and day-of-week are restricted
//!   (not `*`), a day matches when EITHER matches
//!
//! Known gap vs croniter: 7-field expressions (with a year column) are
//! rejected with a clear error instead of being accepted.
//!
//! Next-run computation steps minute-by-minute through the configured local
//! timezone's wall clock (the same clock `joey_core::time::now()` reads),
//! capped at four years.

use anyhow::{bail, Result};
use chrono::{
    DateTime, Datelike, Duration, FixedOffset, Local, LocalResult, NaiveDateTime, TimeZone, Timelike,
};
use chrono_tz::Tz;

const MONTH_NAMES: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const DOW_NAMES: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

/// Cap on the next-run search: four years of minutes.
const SEARCH_CAP_DAYS: i64 = 1461;

/// A parsed, validated cron expression.
#[derive(Debug, Clone)]
pub struct CronExpr {
    minutes: [bool; 60],
    hours: [bool; 24],
    /// Indexed 1-31 (index 0 unused).
    dom: [bool; 32],
    /// Indexed 1-12 (index 0 unused).
    months: [bool; 13],
    /// Indexed 0-6, 0 = Sunday.
    dow: [bool; 7],
    /// Present for the 6-field (trailing seconds) form.
    seconds: Option<[bool; 60]>,
    /// True when the raw day-of-month field was exactly `*` (unrestricted).
    dom_star: bool,
    /// True when the raw day-of-week field was exactly `*` (unrestricted).
    dow_star: bool,
}

/// Which local-timezone wall clock schedule math runs on.
#[derive(Debug, Clone, Copy)]
pub(crate) enum LocalZone {
    Named(Tz),
    System,
}

impl LocalZone {
    /// The configured joey timezone, falling back to the system zone —
    /// exactly the clock `joey_core::time::now()` reads.
    pub(crate) fn configured() -> Self {
        match joey_core::time::configured_tz() {
            Some(tz) => LocalZone::Named(tz),
            None => LocalZone::System,
        }
    }

    pub(crate) fn naive_local(&self, instant: DateTime<FixedOffset>) -> NaiveDateTime {
        match self {
            LocalZone::Named(tz) => instant.with_timezone(tz).naive_local(),
            LocalZone::System => instant.with_timezone(&Local).naive_local(),
        }
    }

    /// Re-express an instant in this zone's offset (Python `astimezone`).
    pub(crate) fn convert_fixed(&self, instant: DateTime<FixedOffset>) -> DateTime<FixedOffset> {
        match self {
            LocalZone::Named(tz) => instant.with_timezone(tz).fixed_offset(),
            LocalZone::System => instant.with_timezone(&Local).fixed_offset(),
        }
    }

    /// Resolve a local wall-clock time to an instant. Ambiguous (fall-back)
    /// times take the earlier occurrence; nonexistent (spring-forward gap)
    /// times return None so the caller skips past the gap.
    pub(crate) fn localize(&self, naive: NaiveDateTime) -> Option<DateTime<FixedOffset>> {
        match self {
            LocalZone::Named(tz) => match tz.from_local_datetime(&naive) {
                LocalResult::Single(dt) => Some(dt.fixed_offset()),
                LocalResult::Ambiguous(a, _) => Some(a.fixed_offset()),
                LocalResult::None => None,
            },
            LocalZone::System => match Local.from_local_datetime(&naive) {
                LocalResult::Single(dt) => Some(dt.fixed_offset()),
                LocalResult::Ambiguous(a, _) => Some(a.fixed_offset()),
                LocalResult::None => None,
            },
        }
    }

    /// Like [`localize`] but never fails: a wall time inside a DST gap is
    /// resolved with the zone's offset for that instant (mirrors Python
    /// `datetime.replace(tzinfo=...)`, which attaches without validation).
    pub(crate) fn localize_lenient(&self, naive: NaiveDateTime) -> DateTime<FixedOffset> {
        use chrono::{Offset, Utc};
        if let Some(dt) = self.localize(naive) {
            return dt;
        }
        let off: FixedOffset = match self {
            LocalZone::Named(tz) => tz.offset_from_utc_datetime(&naive).fix(),
            LocalZone::System => Local.offset_from_utc_datetime(&naive).fix(),
        };
        off.from_local_datetime(&naive)
            .single()
            .unwrap_or_else(|| Utc.from_utc_datetime(&naive).fixed_offset())
    }
}

impl CronExpr {
    /// Parse and validate a cron expression (5 fields, or 6 with trailing
    /// seconds).
    pub fn parse(expr: &str) -> Result<CronExpr> {
        let fields: Vec<&str> = expr.split_whitespace().collect();
        match fields.len() {
            5 | 6 => {}
            7 => bail!(
                "7-field cron expressions are not supported (use 5 fields, or 6 with a \
                 trailing seconds field)"
            ),
            n => bail!("expected 5 or 6 fields, got {}", n),
        }

        let (minutes, _) = parse_field(fields[0], 0, 59, None, false)?;
        let (hours, _) = parse_field(fields[1], 0, 23, None, false)?;
        let (dom_set, dom_star) = parse_field(fields[2], 1, 31, None, false)?;
        let (months_set, _) = parse_field(fields[3], 1, 12, Some(&MONTH_NAMES), false)?;
        let (dow_set, dow_star) = parse_field(fields[4], 0, 7, Some(&DOW_NAMES), true)?;
        let seconds = if fields.len() == 6 {
            let (secs, _) = parse_field(fields[5], 0, 59, None, false)?;
            let mut arr = [false; 60];
            for (i, slot) in arr.iter_mut().enumerate() {
                *slot = secs.contains(&(i as u32));
            }
            Some(arr)
        } else {
            None
        };

        let mut expr = CronExpr {
            minutes: [false; 60],
            hours: [false; 24],
            dom: [false; 32],
            months: [false; 13],
            dow: [false; 7],
            seconds,
            dom_star,
            dow_star,
        };
        for v in minutes {
            expr.minutes[v as usize] = true;
        }
        for v in hours {
            expr.hours[v as usize] = true;
        }
        for v in dom_set {
            expr.dom[v as usize] = true;
        }
        for v in months_set {
            expr.months[v as usize] = true;
        }
        for v in dow_set {
            // 7 == Sunday == 0.
            expr.dow[(v % 7) as usize] = true;
        }
        Ok(expr)
    }

    /// Whether this expression carries a trailing seconds field.
    pub fn has_seconds(&self) -> bool {
        self.seconds.is_some()
    }

    /// Day-level match applying the Vixie DOM/DOW rule: with both fields
    /// restricted, EITHER may match; otherwise the restricted one must.
    fn day_matches(&self, date: chrono::NaiveDate) -> bool {
        let dom_ok = self.dom[date.day() as usize];
        let dow_ok = self.dow[date.weekday().num_days_from_sunday() as usize];
        match (self.dom_star, self.dow_star) {
            (true, true) => true,
            (true, false) => dow_ok,
            (false, true) => dom_ok,
            (false, false) => dom_ok || dow_ok,
        }
    }

    /// Minute-level match (ignores any seconds field).
    fn minute_matches(&self, local: NaiveDateTime) -> bool {
        self.minutes[local.minute() as usize]
            && self.hours[local.hour() as usize]
            && self.months[local.month() as usize]
            && self.day_matches(local.date())
    }

    /// First matching second at-or-after `from_second` within a minute.
    fn next_second_in_minute(&self, from_second: u32) -> Option<u32> {
        match &self.seconds {
            None => (from_second == 0).then_some(0),
            Some(set) => (from_second..60).find(|s| set[*s as usize]),
        }
    }

    /// The next occurrence strictly after `after`, evaluated on the
    /// configured local timezone's wall clock (falling back to the system
    /// zone, exactly like `joey_core::time::now()`).
    pub fn next_after(&self, after: DateTime<FixedOffset>) -> Option<DateTime<FixedOffset>> {
        self.next_after_in(after, LocalZone::configured())
    }

    /// `next_after` evaluated in an explicit named timezone (test hook and
    /// grace-period math).
    pub fn next_after_in_tz(
        &self,
        after: DateTime<FixedOffset>,
        tz: Tz,
    ) -> Option<DateTime<FixedOffset>> {
        self.next_after_in(after, LocalZone::Named(tz))
    }

    fn next_after_in(
        &self,
        after: DateTime<FixedOffset>,
        zone: LocalZone,
    ) -> Option<DateTime<FixedOffset>> {
        let local_after = zone.naive_local(after);
        let minute_start = local_after
            .with_second(0)
            .and_then(|d| d.with_nanosecond(0))
            .unwrap_or(local_after);
        let cap = local_after + Duration::days(SEARCH_CAP_DAYS);

        // With a seconds field, a later second within the current minute is
        // still a valid "next" occurrence.
        if self.seconds.is_some() && self.minute_matches(minute_start) {
            if let Some(sec) = self.next_second_in_minute(local_after.second() + 1) {
                let candidate = minute_start + Duration::seconds(sec as i64);
                if let Some(resolved) = zone.localize(candidate) {
                    return Some(resolved);
                }
            }
        }

        let mut cursor = minute_start + Duration::minutes(1);
        while cursor <= cap {
            // Fast-forward over whole non-matching months/days/hours. All
            // jumps stay in naive local wall-clock space, so they can never
            // step over a matching minute; DST is resolved only at the end.
            if !self.months[cursor.month() as usize] {
                let (y, m) = if cursor.month() == 12 {
                    (cursor.year() + 1, 1)
                } else {
                    (cursor.year(), cursor.month() + 1)
                };
                cursor = chrono::NaiveDate::from_ymd_opt(y, m, 1)?.and_hms_opt(0, 0, 0)?;
                continue;
            }
            if !self.day_matches(cursor.date()) {
                cursor = cursor.date().succ_opt()?.and_hms_opt(0, 0, 0)?;
                continue;
            }
            if !self.hours[cursor.hour() as usize] {
                cursor = cursor
                    .with_minute(0)
                    .and_then(|d| d.with_second(0))
                    .unwrap_or(cursor)
                    + Duration::hours(1);
                continue;
            }
            if !self.minutes[cursor.minute() as usize] {
                cursor += Duration::minutes(1);
                continue;
            }
            let second = self.next_second_in_minute(0).unwrap_or(0);
            let candidate = cursor + Duration::seconds(second as i64);
            match zone.localize(candidate) {
                Some(resolved) => return Some(resolved),
                // Spring-forward gap: this wall time doesn't exist; keep going.
                None => {
                    cursor += Duration::minutes(1);
                    continue;
                }
            }
        }
        None
    }
}

/// Resolve one scalar token (number or 3-letter name) for a field.
fn parse_value(token: &str, min: u32, max: u32, names: Option<&[&str]>, is_dow: bool) -> Result<u32> {
    let t = token.trim();
    if let Ok(v) = t.parse::<u32>() {
        if v < min || v > max {
            bail!("value {} out of range [{},{}]", v, min, max);
        }
        return Ok(v);
    }
    if let Some(names) = names {
        let lower = t.to_ascii_lowercase();
        if let Some(idx) = names.iter().position(|n| *n == lower) {
            // Month names are 1-based; day names are 0-based (0 = Sunday).
            return Ok(if is_dow { idx as u32 } else { idx as u32 + 1 });
        }
    }
    bail!("invalid field value '{}'", token)
}

/// Expand one field spec into its value set, reporting whether the raw spec
/// was exactly `*` (unrestricted — matters for the Vixie DOM/DOW rule).
fn parse_field(
    spec: &str,
    min: u32,
    max: u32,
    names: Option<&[&str]>,
    is_dow: bool,
) -> Result<(Vec<u32>, bool)> {
    let spec = spec.trim();
    if spec.is_empty() {
        bail!("empty cron field");
    }
    let is_star = spec == "*";
    let mut values: Vec<u32> = Vec::new();

    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            bail!("empty list item in cron field '{}'", spec);
        }
        let (base, step) = match part.split_once('/') {
            Some((b, s)) => {
                let step: u32 = s
                    .trim()
                    .parse()
                    .ok()
                    .filter(|v| *v > 0)
                    .ok_or_else(|| anyhow::anyhow!("invalid step '{}' in '{}'", s, part))?;
                (b.trim(), step)
            }
            None => (part, 1),
        };

        // Expand the base into an ordered value sequence.
        let sequence: Vec<u32> = if base == "*" {
            (min..=max).collect()
        } else if let Some((lo_raw, hi_raw)) = base.split_once('-') {
            let lo = parse_value(lo_raw, min, max, names, is_dow)?;
            let hi = parse_value(hi_raw, min, max, names, is_dow)?;
            if lo <= hi {
                (lo..=hi).collect()
            } else {
                // Wrap-around range (croniter-compatible), e.g. `22-2` or `fri-mon`.
                (lo..=max).chain(min..=hi).collect()
            }
        } else {
            let v = parse_value(base, min, max, names, is_dow)?;
            if part.contains('/') {
                // croniter treats `N/step` as `N-max/step`.
                (v..=max).collect()
            } else {
                vec![v]
            }
        };

        for v in sequence.into_iter().step_by(step as usize) {
            if !values.contains(&v) {
                values.push(v);
            }
        }
    }

    if values.is_empty() {
        bail!("cron field '{}' matches nothing", spec);
    }
    Ok((values, is_star))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono_tz::Tz;

    fn utc(s: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(s).unwrap()
    }

    fn next_utc(expr: &str, after: &str) -> String {
        CronExpr::parse(expr)
            .unwrap()
            .next_after_in_tz(utc(after), chrono_tz::UTC)
            .unwrap()
            .format("%Y-%m-%dT%H:%M:%S")
            .to_string()
    }

    #[test]
    fn dow_monday_is_one() {
        // 2026-07-15 is a Wednesday; the next Monday is 2026-07-20.
        assert_eq!(
            next_utc("0 9 * * 1", "2026-07-15T00:00:00Z"),
            "2026-07-20T09:00:00"
        );
    }

    #[test]
    fn dow_zero_and_seven_are_sunday() {
        // Next Sunday after Wed 2026-07-15 is 2026-07-19.
        assert_eq!(
            next_utc("0 9 * * 0", "2026-07-15T00:00:00Z"),
            "2026-07-19T09:00:00"
        );
        assert_eq!(
            next_utc("0 9 * * 7", "2026-07-15T00:00:00Z"),
            "2026-07-19T09:00:00"
        );
    }

    #[test]
    fn vixie_dom_dow_or_rule() {
        // `0 9 1 * 1`: both dom and dow restricted → fires on the 1st OR on
        // Mondays. From Wed 2026-07-15 the next Monday (Jul 20) comes before
        // the next 1st (Aug 1).
        assert_eq!(
            next_utc("0 9 1 * 1", "2026-07-15T00:00:00Z"),
            "2026-07-20T09:00:00"
        );
        // ...and 2026-08-01 (a Saturday) still fires via the dom half.
        assert_eq!(
            next_utc("0 9 1 * 1", "2026-07-31T23:00:00Z"),
            "2026-08-01T09:00:00"
        );
        // With dow unrestricted, only the dom half applies.
        assert_eq!(
            next_utc("0 9 1 * *", "2026-07-15T00:00:00Z"),
            "2026-08-01T09:00:00"
        );
    }

    #[test]
    fn step_minutes() {
        assert_eq!(
            next_utc("*/15 * * * *", "2026-07-15T12:07:00Z"),
            "2026-07-15T12:15:00"
        );
        assert_eq!(
            next_utc("*/15 * * * *", "2026-07-15T12:15:00Z"),
            "2026-07-15T12:30:00"
        );
        // Range with step.
        assert_eq!(
            next_utc("10-30/10 * * * *", "2026-07-15T12:00:00Z"),
            "2026-07-15T12:10:00"
        );
    }

    #[test]
    fn month_and_dow_names() {
        // Next Monday in February after Jul 2026 is 2027-02-01.
        assert_eq!(
            next_utc("0 9 * feb mon", "2026-07-15T00:00:00Z"),
            "2027-02-01T09:00:00"
        );
        assert_eq!(
            next_utc("0 9 * FEB MON", "2026-07-15T00:00:00Z"),
            "2027-02-01T09:00:00"
        );
        // Name range: mon-fri.
        assert_eq!(
            next_utc("0 9 * * mon-fri", "2026-07-18T00:00:00Z"), // Saturday
            "2026-07-20T09:00:00"
        );
    }

    #[test]
    fn wrap_around_ranges() {
        // Hours 22,23,0,1,2.
        assert_eq!(
            next_utc("0 22-2 * * *", "2026-07-15T12:00:00Z"),
            "2026-07-15T22:00:00"
        );
        assert_eq!(
            next_utc("0 22-2 * * *", "2026-07-15T23:30:00Z"),
            "2026-07-16T00:00:00"
        );
        // fri-mon → {5,6,0,1}: Saturday matches.
        assert_eq!(
            next_utc("0 9 * * fri-mon", "2026-07-17T10:00:00Z"), // Friday after 9
            "2026-07-18T09:00:00"
        );
    }

    #[test]
    fn trailing_seconds_field() {
        // Later second within the current minute is allowed.
        assert_eq!(
            next_utc("* * * * * 30", "2026-07-15T12:00:10Z"),
            "2026-07-15T12:00:30"
        );
        // Past the second → next minute.
        assert_eq!(
            next_utc("* * * * * 30", "2026-07-15T12:00:45Z"),
            "2026-07-15T12:01:30"
        );
        assert_eq!(
            next_utc("0 9 * * * 15", "2026-07-15T12:00:00Z"),
            "2026-07-16T09:00:15"
        );
        // 5-field expressions land on second 0.
        assert_eq!(
            next_utc("0 9 * * *", "2026-07-15T12:00:00Z"),
            "2026-07-16T09:00:00"
        );
    }

    #[test]
    fn strictly_after_base() {
        // Base exactly on a match → the following occurrence.
        assert_eq!(
            next_utc("* * * * *", "2026-07-15T12:00:00Z"),
            "2026-07-15T12:01:00"
        );
        // Mid-minute base rounds up to the next minute.
        assert_eq!(
            next_utc("* * * * *", "2026-07-15T12:00:30Z"),
            "2026-07-15T12:01:00"
        );
    }

    #[test]
    fn named_timezone_wall_clock() {
        let expr = CronExpr::parse("0 9 * * 1").unwrap();
        let tz: Tz = "Asia/Kolkata".parse().unwrap();
        let next = expr
            .next_after_in_tz(utc("2026-07-15T00:00:00+05:30"), tz)
            .unwrap();
        assert_eq!(next.to_rfc3339(), "2026-07-20T09:00:00+05:30");
    }

    #[test]
    fn dst_spring_forward_gap_is_skipped() {
        // America/New_York 2026-03-08: 02:30 local does not exist.
        let expr = CronExpr::parse("30 2 * * *").unwrap();
        let tz: Tz = "America/New_York".parse().unwrap();
        let next = expr
            .next_after_in_tz(utc("2026-03-08T00:00:00-05:00"), tz)
            .unwrap();
        assert_eq!(next.to_rfc3339(), "2026-03-09T02:30:00-04:00");
    }

    #[test]
    fn impossible_date_returns_none() {
        let expr = CronExpr::parse("0 9 31 2 *").unwrap();
        assert!(expr
            .next_after_in_tz(utc("2026-07-15T00:00:00Z"), chrono_tz::UTC)
            .is_none());
    }

    #[test]
    fn parse_rejections() {
        assert!(CronExpr::parse("61 * * * *").is_err());
        assert!(CronExpr::parse("* 24 * * *").is_err());
        assert!(CronExpr::parse("* * 0 * *").is_err());
        assert!(CronExpr::parse("* * * 13 *").is_err());
        assert!(CronExpr::parse("* * * * 8").is_err());
        assert!(CronExpr::parse("* * * *").is_err());
        assert!(CronExpr::parse("bad").is_err());
        assert!(CronExpr::parse("*/0 * * * *").is_err());
        let seven = CronExpr::parse("0 0 * * * 0 2026");
        assert!(seven.is_err());
        assert!(seven
            .unwrap_err()
            .to_string()
            .contains("7-field cron expressions are not supported"));
    }

    #[test]
    fn single_value_with_step_extends_to_max() {
        // croniter: `5/15` in the minute field == `5-59/15` → 5,20,35,50.
        assert_eq!(
            next_utc("5/15 * * * *", "2026-07-15T12:21:00Z"),
            "2026-07-15T12:35:00"
        );
    }
}
