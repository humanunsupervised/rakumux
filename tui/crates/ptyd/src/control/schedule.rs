//! When a heartbeat is next due, in this machine's local time.
//!
//! `every` counts from the time asked about (the last due time, or when it
//! was enabled). A time of day is the next such minute after it on a day the
//! schedule names. A time that doesn't exist that day (clocks going forward)
//! runs at the first minute after it that does; one that happens twice
//! (clocks going back) runs at the first.

use chrono::{Datelike, Duration, LocalResult, NaiveDate, NaiveDateTime, TimeZone};
use session_core::heartbeat::Schedule;

/// The next due time of `schedule` after `now` (Unix seconds) in `zone`;
/// None for a schedule that never runs (unknown, or invalid).
pub(crate) fn next_after<Tz: TimeZone>(schedule: &Schedule, now: i64, zone: &Tz) -> Option<i64> {
    if schedule.problem().is_some() {
        return None;
    }
    let days: Vec<u32> = match schedule {
        Schedule::Every { minutes } => return Some(now + i64::from(*minutes) * 60),
        Schedule::Daily { .. } => (0..7).collect(),
        Schedule::Weekdays { days, .. } => days.iter().filter_map(|day| day.index()).collect(),
        Schedule::Unknown => return None,
    };
    let (hour, minute) = schedule.time_of_day()?;
    let today = zone.timestamp_opt(now, 0).single()?.date_naive();
    // Eight days covers next week's same day.
    for offset in 0..8 {
        let date = today.checked_add_signed(Duration::days(offset))?;
        if !days.contains(&date.weekday().num_days_from_monday()) {
            continue;
        }
        let Some(at) = local(zone, date, hour, minute) else {
            continue;
        };
        if at > now {
            return Some(at);
        }
    }
    None
}

/// `date` at `hour:minute` in `zone`, as Unix seconds: the earlier of a time
/// that happens twice, the first minute after one that doesn't happen.
fn local<Tz: TimeZone>(zone: &Tz, date: NaiveDate, hour: u32, minute: u32) -> Option<i64> {
    let mut wanted: NaiveDateTime = date.and_hms_opt(hour, minute, 0)?;
    // A gap is at most a few hours.
    for _ in 0..(4 * 60) {
        match zone.from_local_datetime(&wanted) {
            LocalResult::Single(at) => return Some(at.timestamp()),
            LocalResult::Ambiguous(first, _) => return Some(first.timestamp()),
            LocalResult::None => wanted += Duration::minutes(1),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, NaiveTime, Offset, Utc};
    use session_core::heartbeat::Weekday;

    fn at(zone: &impl TimeZone, y: i32, m: u32, d: u32, h: u32, min: u32) -> i64 {
        zone.from_local_datetime(
            &NaiveDate::from_ymd_opt(y, m, d)
                .unwrap()
                .and_hms_opt(h, min, 0)
                .unwrap(),
        )
        .earliest()
        .unwrap()
        .timestamp()
    }

    fn daily(time: &str) -> Schedule {
        Schedule::Daily { at: time.into() }
    }

    #[test]
    fn every_counts_from_now_and_daily_is_the_next_such_minute() {
        let zone = FixedOffset::east_opt(10 * 3600).unwrap();
        let now = at(&zone, 2026, 11, 12, 8, 30);
        assert_eq!(
            next_after(&Schedule::Every { minutes: 30 }, now, &zone),
            Some(now + 1800)
        );
        assert_eq!(
            next_after(&daily("09:00"), now, &zone),
            Some(at(&zone, 2026, 11, 12, 9, 0))
        );
        // Exactly due now: the next day's.
        let nine = at(&zone, 2026, 11, 12, 9, 0);
        assert_eq!(
            next_after(&daily("09:00"), nine, &zone),
            Some(nine + 86_400)
        );
        assert_eq!(
            next_after(&daily("7:15"), now, &zone),
            Some(at(&zone, 2026, 11, 13, 7, 15))
        );
        assert_eq!(next_after(&Schedule::Unknown, now, &zone), None);
        assert_eq!(
            next_after(&Schedule::Every { minutes: 1 }, now, &zone),
            None
        );
    }

    #[test]
    fn weekdays_skip_to_a_day_the_schedule_names() {
        let zone = Utc;
        // 2026-11-12 is a Thursday.
        let thursday = at(&zone, 2026, 11, 12, 12, 0);
        let mon_fri = Schedule::Weekdays {
            days: vec![Weekday::Mon, Weekday::Fri],
            at: "09:00".into(),
        };
        assert_eq!(
            next_after(&mon_fri, thursday, &zone),
            Some(at(&zone, 2026, 11, 13, 9, 0))
        );
        let thursdays = Schedule::Weekdays {
            days: vec![Weekday::Thu],
            at: "09:00".into(),
        };
        assert_eq!(
            next_after(&thursdays, thursday, &zone),
            Some(at(&zone, 2026, 11, 19, 9, 0))
        );
    }

    /// UTC+10, and UTC+11 from 02:00 on 2026-10-04 (Sydney's spring) until
    /// 03:00 on 2027-04-04 (its autumn): a gap and a repeated hour.
    #[derive(Clone, Copy)]
    struct Sydney;

    impl Sydney {
        fn spring() -> NaiveDateTime {
            NaiveDate::from_ymd_opt(2026, 10, 4)
                .unwrap()
                .and_time(NaiveTime::from_hms_opt(2, 0, 0).unwrap())
        }
        fn autumn() -> NaiveDateTime {
            NaiveDate::from_ymd_opt(2027, 4, 4)
                .unwrap()
                .and_time(NaiveTime::from_hms_opt(3, 0, 0).unwrap())
        }
        fn standard() -> FixedOffset {
            FixedOffset::east_opt(10 * 3600).unwrap()
        }
        fn summer() -> FixedOffset {
            FixedOffset::east_opt(11 * 3600).unwrap()
        }
    }

    impl TimeZone for Sydney {
        type Offset = FixedOffset;
        fn from_offset(_: &FixedOffset) -> Self {
            Sydney
        }
        fn offset_from_local_date(&self, local: &NaiveDate) -> LocalResult<FixedOffset> {
            self.offset_from_local_datetime(&local.and_hms_opt(12, 0, 0).unwrap())
        }
        fn offset_from_local_datetime(&self, local: &NaiveDateTime) -> LocalResult<FixedOffset> {
            let spring = Sydney::spring();
            let autumn = Sydney::autumn();
            if *local < spring {
                LocalResult::Single(Sydney::standard())
            } else if *local < spring + Duration::hours(1) {
                LocalResult::None
            } else if *local < autumn - Duration::hours(1) {
                LocalResult::Single(Sydney::summer())
            } else if *local < autumn {
                LocalResult::Ambiguous(Sydney::summer(), Sydney::standard())
            } else {
                LocalResult::Single(Sydney::standard())
            }
        }
        fn offset_from_utc_date(&self, utc: &NaiveDate) -> FixedOffset {
            self.offset_from_utc_datetime(&utc.and_hms_opt(12, 0, 0).unwrap())
        }
        fn offset_from_utc_datetime(&self, utc: &NaiveDateTime) -> FixedOffset {
            let spring = Sydney::spring() - Duration::hours(10);
            let autumn = Sydney::autumn() - Duration::hours(11);
            if *utc >= spring && *utc < autumn {
                Sydney::summer()
            } else {
                Sydney::standard()
            }
        }
    }

    #[test]
    fn a_time_clocks_skip_runs_at_the_next_minute_and_a_repeated_one_once() {
        let zone = Sydney;
        let evening = at(&Sydney::standard(), 2026, 10, 3, 20, 0);
        // 02:30 doesn't happen on 2026-10-04: 03:00 (summer time) does.
        let due = next_after(&daily("02:30"), evening, &zone).unwrap();
        assert_eq!(due, at(&Sydney::summer(), 2026, 10, 4, 3, 0));
        // 02:30 happens twice on 2027-04-04: the first one, then the next day's.
        let before = at(&Sydney::summer(), 2027, 4, 3, 20, 0);
        let first = next_after(&daily("02:30"), before, &zone).unwrap();
        assert_eq!(first, at(&Sydney::summer(), 2027, 4, 4, 2, 30));
        let next = next_after(&daily("02:30"), first, &zone).unwrap();
        assert_eq!(next, at(&Sydney::standard(), 2027, 4, 5, 2, 30));
        assert_eq!(
            Sydney
                .offset_from_utc_datetime(&Utc.timestamp_opt(first, 0).unwrap().naive_utc())
                .fix(),
            Sydney::summer()
        );
    }
}
