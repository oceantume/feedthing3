//! Times as people read them, in the app's time zone.

use jiff::{Timestamp, tz::TimeZone};

use crate::clock::{DAY_MS, HOUR_MS, MINUTE_MS};

/// "Today, 3:45 PM", "Yesterday, 3:45 PM", "Aug 5, 3:45 PM", or
/// "Aug 5, 2024, 3:45 PM" when it's not this year.
pub fn item_time(at_ms: i64, now_ms: i64, tz: &TimeZone) -> String {
    let (Ok(at), Ok(now)) = (
        Timestamp::from_millisecond(at_ms),
        Timestamp::from_millisecond(now_ms),
    ) else {
        return String::new();
    };
    let at = at.to_zoned(tz.clone());
    let now = now.to_zoned(tz.clone());
    let time = at.strftime("%-I:%M %p");
    if at.date() == now.date() {
        format!("Today, {time}")
    } else if now.date().yesterday().is_ok_and(|d| d == at.date()) {
        format!("Yesterday, {time}")
    } else if at.year() == now.year() {
        format!("{}, {time}", at.strftime("%b %-d"))
    } else {
        format!("{}, {time}", at.strftime("%b %-d, %Y"))
    }
}

/// Midnight at the start of yesterday, as epoch milliseconds.
pub fn start_of_yesterday(now_ms: i64, tz: &TimeZone) -> i64 {
    Timestamp::from_millisecond(now_ms)
        .ok()
        .and_then(|now| now.to_zoned(tz.clone()).date().yesterday().ok())
        .and_then(|day| day.to_zoned(tz.clone()).ok())
        .map_or(now_ms - DAY_MS, |midnight| {
            midnight.timestamp().as_millisecond()
        })
}

/// "just now", "5 min ago", "3 h ago", "2 d ago", or "never".
pub fn relative(at_ms: Option<i64>, now_ms: i64) -> String {
    let Some(at_ms) = at_ms else {
        return "never".to_owned();
    };
    let ago = now_ms - at_ms;
    if ago < MINUTE_MS {
        "just now".to_owned()
    } else if ago < HOUR_MS {
        format!("{} min ago", ago / MINUTE_MS)
    } else if ago < DAY_MS {
        format!("{} h ago", ago / HOUR_MS)
    } else {
        format!("{} d ago", ago / DAY_MS)
    }
}

/// When the next poll is due: "pending" before the first, then "soon",
/// "in 5 min" or "in 2 h".
pub fn next_poll(last_polled_at: Option<i64>, interval_minutes: i64, now_ms: i64) -> String {
    let Some(last) = last_polled_at else {
        return "pending".to_owned();
    };
    let left = last + interval_minutes * MINUTE_MS - now_ms;
    if left <= 0 {
        return "soon".to_owned();
    }
    // Both are positive here, so this rounds up.
    let minutes = (left + MINUTE_MS - 1) / MINUTE_MS;
    if minutes < 60 {
        format!("in {minutes} min")
    } else {
        format!("in {} h", (minutes + 59) / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toronto() -> TimeZone {
        TimeZone::get("America/Toronto").unwrap()
    }

    fn ms(rfc3339: &str) -> i64 {
        rfc3339.parse::<Timestamp>().unwrap().as_millisecond()
    }

    #[test]
    fn item_times_name_today_and_yesterday_in_local_time() {
        let now = ms("2026-08-06T14:00:00-04:00");
        assert_eq!(
            item_time(ms("2026-08-06T09:05:00-04:00"), now, &toronto()),
            "Today, 9:05 AM"
        );
        // Late on the 5th in Toronto, though already the 6th in UTC.
        assert_eq!(
            item_time(ms("2026-08-05T23:30:00-04:00"), now, &toronto()),
            "Yesterday, 11:30 PM"
        );
        assert_eq!(
            item_time(ms("2026-08-01T15:45:00-04:00"), now, &toronto()),
            "Aug 1, 3:45 PM"
        );
    }

    #[test]
    fn item_times_show_the_year_only_when_its_not_this_one() {
        let now = ms("2026-01-01T10:00:00-05:00");
        assert_eq!(
            item_time(ms("2025-12-30T20:00:00-05:00"), now, &toronto()),
            "Dec 30, 2025, 8:00 PM"
        );
        assert_eq!(
            item_time(ms("2025-12-31T20:00:00-05:00"), now, &toronto()),
            "Yesterday, 8:00 PM"
        );
    }

    #[test]
    fn yesterday_starts_at_local_midnight_across_a_dst_change() {
        // Clocks went forward at 2:00 on 2026-03-08.
        let now = ms("2026-03-09T12:00:00-04:00");
        assert_eq!(
            start_of_yesterday(now, &toronto()),
            ms("2026-03-08T00:00:00-05:00")
        );
    }

    #[test]
    fn relative_times_round_down() {
        let now = 10 * DAY_MS;
        assert_eq!(relative(None, now), "never");
        assert_eq!(relative(Some(now - 59_000), now), "just now");
        assert_eq!(relative(Some(now - 5 * MINUTE_MS - 1), now), "5 min ago");
        assert_eq!(relative(Some(now - 3 * HOUR_MS), now), "3 h ago");
        assert_eq!(relative(Some(now - 2 * DAY_MS - 1), now), "2 d ago");
    }

    #[test]
    fn next_polls_round_up() {
        let now = 10 * DAY_MS;
        assert_eq!(next_poll(None, 15, now), "pending");
        assert_eq!(next_poll(Some(now - 15 * MINUTE_MS), 15, now), "soon");
        assert_eq!(
            next_poll(Some(now - 10 * MINUTE_MS - 1), 15, now),
            "in 5 min"
        );
        assert_eq!(next_poll(Some(now), 90, now), "in 2 h");
    }
}
