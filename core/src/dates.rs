//! How dates are shown, the same in both apps.

use chrono::{DateTime, Local, Utc};

/// The notes list section a date falls in: "Today", "Yesterday",
/// "Previous 7 Days", "Previous 30 Days", a month of this year, or a year.
pub fn date_group(t: DateTime<Utc>, today: chrono::NaiveDate) -> String {
    let d = t.with_timezone(&Local).date_naive();
    let days = (today - d).num_days();
    use chrono::Datelike;
    match days {
        i64::MIN..=0 => "Today".into(),
        1 => "Yesterday".into(),
        2..=7 => "Previous 7 Days".into(),
        8..=30 => "Previous 30 Days".into(),
        _ if d.year() == today.year() => d.format("%B").to_string(),
        _ => d.year().to_string(),
    }
}

/// A note's time in the list: "11:32" today, "Yesterday", a weekday within a
/// week, else the date.
pub fn list_time(t: DateTime<Utc>) -> String {
    let lt = t.with_timezone(&Local);
    let today = Local::now().date_naive();
    let days = (today - lt.date_naive()).num_days();
    match days {
        i64::MIN..=0 => lt.format("%H:%M").to_string(),
        1 => "Yesterday".into(),
        2..=6 => lt.format("%A").to_string(),
        _ => lt.format("%-d %b %Y").to_string(),
    }
}

/// The date at the top of a note: "15 November 2026 at 11:32".
pub fn note_header_time(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local).format("%-d %B %Y at %H:%M").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn date_groups() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();
        let at = |y, m, d| Local.with_ymd_and_hms(y, m, d, 12, 0, 0).unwrap().with_timezone(&Utc);
        assert_eq!(date_group(at(2026, 11, 15), today), "Today");
        assert_eq!(date_group(at(2026, 11, 14), today), "Yesterday");
        assert_eq!(date_group(at(2026, 11, 10), today), "Previous 7 Days");
        assert_eq!(date_group(at(2026, 10, 25), today), "Previous 30 Days");
        assert_eq!(date_group(at(2026, 3, 1), today), "March");
        assert_eq!(date_group(at(2025, 12, 1), today), "2025");
    }
}
