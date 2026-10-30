use chrono::{DateTime, Datelike, FixedOffset, Local, Months, NaiveDate, NaiveDateTime};
use rust_i18n::t;

/// Present the original calendar date and its age without replacing the full-date
/// tooltip. RFC 3339 dates retain their stated offset for calendar-day comparisons.
/// A timezone-free firmware build date stays a calendar date, compared with the
/// current local day; it is never interpreted as a UTC/local timestamp.
pub(crate) fn display_date(raw: &str) -> Option<String> {
    display_date_at(raw, Local::now().fixed_offset())
}

fn display_date_at(raw: &str, now: DateTime<FixedOffset>) -> Option<String> {
    let (date, age) = date_and_age_at(raw, now)?;
    let age = match age {
        RelativeDate::Today => t!("dates.relative.today").to_string(),
        RelativeDate::Yesterday => t!("dates.relative.yesterday").to_string(),
        RelativeDate::Days(count) => t!("dates.relative.days_ago", count = count).to_string(),
        RelativeDate::Months(1) => t!("dates.relative.month_ago").to_string(),
        RelativeDate::Months(count) => t!("dates.relative.months_ago", count = count).to_string(),
        RelativeDate::Years(1) => t!("dates.relative.year_ago").to_string(),
        RelativeDate::Years(count) => t!("dates.relative.years_ago", count = count).to_string(),
        RelativeDate::Future => t!("dates.relative.future").to_string(),
    };
    Some(format!("{} ({age})", date.format("%Y-%m-%d")))
}

#[derive(Debug, PartialEq, Eq)]
enum RelativeDate {
    Today,
    Yesterday,
    Days(u32),
    Months(u32),
    Years(u32),
    Future,
}

fn date_and_age_at(raw: &str, now: DateTime<FixedOffset>) -> Option<(NaiveDate, RelativeDate)> {
    let raw = raw.trim();
    let (date, today) = match DateTime::parse_from_rfc3339(raw) {
        Ok(date) => (
            date.date_naive(),
            now.with_timezone(date.offset()).date_naive(),
        ),
        Err(_) => (
            NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S")
                .ok()?
                .date(),
            now.date_naive(),
        ),
    };
    Some((date, calendar_age(date, today)))
}

fn calendar_age(date: NaiveDate, today: NaiveDate) -> RelativeDate {
    let days = today.signed_duration_since(date).num_days();
    if days < 0 {
        return RelativeDate::Future;
    }
    if days == 0 {
        return RelativeDate::Today;
    }
    if days == 1 {
        return RelativeDate::Yesterday;
    }

    // Use calendar anniversaries, not fixed 30/365-day approximations. Chrono
    // clamps month-end anniversaries (Jan 31 -> Feb 28/29; Feb 29 -> Feb 28).
    let mut months =
        ((today.year() - date.year()) * 12 + today.month() as i32 - date.month() as i32) as u32;
    if months > 0
        && date
            .checked_add_months(Months::new(months))
            .is_none_or(|anniversary| anniversary > today)
    {
        months -= 1;
    }
    if months >= 12 {
        RelativeDate::Years(months / 12)
    } else if months > 0 {
        RelativeDate::Months(months)
    } else {
        RelativeDate::Days(days as u32)
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, FixedOffset};

    use super::{RelativeDate, date_and_age_at, display_date_at};

    fn relative_date_at(raw: &str, now: DateTime<FixedOffset>) -> Option<RelativeDate> {
        date_and_age_at(raw, now).map(|(_, age)| age)
    }

    fn now(raw: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(raw).unwrap()
    }

    #[test]
    fn day_boundaries_use_calendar_days_not_elapsed_hours() {
        let clock = now("2026-10-11T00:01:00Z");
        assert_eq!(
            relative_date_at("2026-10-11T00:00:00Z", clock),
            Some(RelativeDate::Today)
        );
        assert_eq!(
            relative_date_at("2026-10-10T23:59:59Z", clock),
            Some(RelativeDate::Yesterday)
        );
        assert_eq!(
            relative_date_at("2026-10-09T23:59:59Z", clock),
            Some(RelativeDate::Days(2))
        );
    }

    #[test]
    fn offset_dates_keep_their_calendar_day() {
        let clock = now("2026-10-11T00:30:00Z");
        assert_eq!(
            relative_date_at("2026-10-10T20:00:00-05:00", clock),
            Some(RelativeDate::Today)
        );
        assert_eq!(
            relative_date_at("2026-10-10T23:59:00+08:00", clock),
            Some(RelativeDate::Yesterday)
        );
        assert_eq!(
            relative_date_at("2026-10-11T01:00:00+01:00", clock),
            Some(RelativeDate::Today)
        );
        assert_eq!(
            relative_date_at("2026-10-11T00:30:00.123Z", clock),
            Some(RelativeDate::Today)
        );
    }

    #[test]
    fn displayed_absolute_dates_keep_the_original_calendar_day() {
        let clock = now("2026-10-11T00:30:00Z");
        for (raw, expected_date) in [
            ("2026-10-10T20:00:00-05:00", "2026-10-10"),
            ("2026-10-11T01:00:00+08:00", "2026-10-11"),
            (" 2026-10-10 23:59:59 ", "2026-10-10"),
        ] {
            let label = display_date_at(raw, clock).unwrap();
            let (date, age) = label.split_once(" (").unwrap();
            assert_eq!(date, expected_date, "{raw}");
            assert!(!age.strip_suffix(')').unwrap().is_empty(), "{raw}");
        }
    }

    #[test]
    fn naive_build_dates_follow_the_current_local_day() {
        let clock = now("2026-10-11T01:48:00+08:00");
        assert_eq!(
            relative_date_at("2026-10-09 01:36:30", clock),
            Some(RelativeDate::Days(2))
        );
        assert_eq!(
            relative_date_at("2026-10-10 23:59:59", clock),
            Some(RelativeDate::Yesterday)
        );
        assert_eq!(
            relative_date_at("2026-10-11 23:59:59", clock),
            Some(RelativeDate::Today)
        );
    }

    #[test]
    fn naive_firmware_dates_do_not_acquire_a_timezone_or_hour_age() {
        let clock = now("2026-10-11T00:01:00Z");
        assert_eq!(
            relative_date_at("2026-10-11 23:59:59", clock),
            Some(RelativeDate::Today)
        );
        assert_eq!(
            relative_date_at("2026-10-10 23:59:59", clock),
            Some(RelativeDate::Yesterday)
        );
        assert_eq!(
            relative_date_at(" 2026-10-09 00:00:00 ", clock),
            Some(RelativeDate::Days(2))
        );
    }

    #[test]
    fn month_anniversaries_are_calendar_based() {
        let clock = now("2026-10-11T12:00:00Z");
        assert_eq!(
            relative_date_at("2026-09-12 00:00:00", clock),
            Some(RelativeDate::Days(29))
        );
        assert_eq!(
            relative_date_at("2026-09-11 00:00:00", clock),
            Some(RelativeDate::Months(1))
        );
        assert_eq!(
            relative_date_at("2026-08-11 00:00:00", clock),
            Some(RelativeDate::Months(2))
        );
        assert_eq!(
            relative_date_at("2026-01-31 00:00:00", now("2026-02-28T00:00:00Z")),
            Some(RelativeDate::Months(1))
        );
    }

    #[test]
    fn leap_days_and_month_end_anniversaries_are_validated() {
        assert_eq!(
            relative_date_at("2024-02-29 12:00:00", now("2024-03-01T00:00:00Z")),
            Some(RelativeDate::Yesterday)
        );
        assert_eq!(
            relative_date_at("2024-01-31 00:00:00", now("2024-02-28T00:00:00Z")),
            Some(RelativeDate::Days(28))
        );
        assert_eq!(
            relative_date_at("2024-01-31 00:00:00", now("2024-02-29T00:00:00Z")),
            Some(RelativeDate::Months(1))
        );
        assert_eq!(
            relative_date_at("2024-02-29 00:00:00", now("2025-02-28T00:00:00Z")),
            Some(RelativeDate::Years(1))
        );
    }

    #[test]
    fn year_anniversaries_are_not_fixed_day_counts() {
        let clock = now("2026-10-11T12:00:00Z");
        assert_eq!(
            relative_date_at("2025-10-12 00:00:00", clock),
            Some(RelativeDate::Months(11))
        );
        assert_eq!(
            relative_date_at("2025-10-11 00:00:00", clock),
            Some(RelativeDate::Years(1))
        );
        assert_eq!(
            relative_date_at("2024-10-11 00:00:00", clock),
            Some(RelativeDate::Years(2))
        );
    }

    #[test]
    fn future_days_never_claim_elapsed_time() {
        let clock = now("2026-10-11T12:00:00Z");
        assert_eq!(
            relative_date_at("2026-10-12 00:00:00", clock),
            Some(RelativeDate::Future)
        );
        assert_eq!(
            relative_date_at("2030-01-01T00:00:00+08:00", clock),
            Some(RelativeDate::Future)
        );
    }

    #[test]
    fn empty_invalid_and_impossible_dates_are_not_presented() {
        let clock = now("2026-10-11T12:00:00Z");
        for raw in [
            "",
            "   ",
            "unknown",
            "2026-02-29 00:00:00",
            "2024-02-30T00:00:00Z",
            "2026-13-01 00:00:00",
            "2026-10-11 25:00:00",
            "2026-10-11",
            "2026-10-11T00:00:00+25:00",
        ] {
            assert_eq!(relative_date_at(raw, clock), None, "{raw}");
        }
    }
}
