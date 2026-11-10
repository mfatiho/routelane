use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalDate {
    pub year: i32,
    pub month: u8,
    pub day: u8,
}

impl LocalDate {
    pub fn parse(value: &str) -> Option<Self> {
        let bytes = value.as_bytes();
        if bytes.len() != 10
            || bytes[4] != b'-'
            || bytes[7] != b'-'
            || !bytes
                .iter()
                .enumerate()
                .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit())
        {
            return None;
        }
        let date = Self {
            year: value[..4].parse().ok()?,
            month: value[5..7].parse().ok()?,
            day: value[8..].parse().ok()?,
        };
        (date.day > 0 && date.day <= days_in_month(date.year, date.month)).then_some(date)
    }

    // Gregorian day ordinal; independent of timezone and DST for daily retention.
    pub(crate) fn ordinal(self) -> i64 {
        let year = i64::from(self.year) - i64::from(self.month <= 2);
        let era = year.div_euclid(400);
        let year_of_era = year - era * 400;
        let month = i64::from(self.month) + if self.month > 2 { -3 } else { 9 };
        era * 146_097 + year_of_era * 365 + year_of_era / 4 - year_of_era / 100
            + (153 * month + 2) / 5
            + i64::from(self.day)
            - 1
    }
}

impl fmt::Display for LocalDate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

impl Serialize for LocalDate {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for LocalDate {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value)
            .ok_or_else(|| serde::de::Error::custom("expected a valid YYYY-MM-DD date"))
    }
}

pub fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => 0,
    }
}

pub fn period_start(today: LocalDate, reset_day: u8) -> LocalDate {
    let day = reset_day
        .clamp(1, 31)
        .min(days_in_month(today.year, today.month));
    if today.day >= day {
        return LocalDate { day, ..today };
    }
    let (year, month) = if today.month == 1 {
        (today.year - 1, 12)
    } else {
        (today.year, today.month - 1)
    };
    LocalDate {
        year,
        month,
        day: reset_day.clamp(1, 31).min(days_in_month(year, month)),
    }
}

pub fn next_reset(today: LocalDate, reset_day: u8) -> LocalDate {
    let start = period_start(today, reset_day);
    let (year, month) = if start.month == 12 {
        (start.year + 1, 1)
    } else {
        (start.year, start.month + 1)
    };
    LocalDate {
        year,
        month,
        day: reset_day.clamp(1, 31).min(days_in_month(year, month)),
    }
}

pub fn local_date(unix_seconds: i64) -> LocalDate {
    let seconds = unix_seconds as libc::time_t;
    // SAFETY: both pointers reference initialized, correctly aligned objects.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&seconds, &mut tm) }.is_null() {
        return LocalDate {
            year: 1970,
            month: 1,
            day: 1,
        };
    }
    LocalDate {
        year: tm.tm_year + 1900,
        month: (tm.tm_mon + 1) as u8,
        day: tm.tm_mday as u8,
    }
}

pub fn local_midnight(date: LocalDate) -> i64 {
    // SAFETY: zero is valid for libc::tm, and mktime receives an exclusive pointer.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = date.year - 1900;
    tm.tm_mon = i32::from(date.month) - 1;
    tm.tm_mday = i32::from(date.day);
    tm.tm_isdst = -1;
    unsafe { libc::mktime(&mut tm) as i64 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn period_boundaries_follow_the_reset_day_and_clamp_to_month_end() {
        let d = |year, month, day| LocalDate { year, month, day };
        assert_eq!(period_start(d(2026, 11, 9), 1), d(2026, 11, 1));
        assert_eq!(next_reset(d(2026, 11, 9), 1), d(2026, 12, 1));
        assert_eq!(period_start(d(2026, 11, 9), 15), d(2026, 10, 15));
        assert_eq!(period_start(d(2027, 2, 28), 31), d(2027, 2, 28));
        assert_eq!(next_reset(d(2027, 2, 28), 31), d(2027, 3, 31));
        assert_eq!(period_start(d(2027, 3, 30), 31), d(2027, 2, 28));
        assert_eq!(period_start(d(2028, 2, 29), 30), d(2028, 2, 29));
        assert_eq!(period_start(d(2027, 1, 5), 10), d(2026, 12, 10));
        assert_eq!(next_reset(d(2027, 4, 30), 31), d(2027, 5, 31));
        assert_eq!(next_reset(d(2026, 12, 31), 31), d(2027, 1, 31));
        assert_eq!(days_in_month(2000, 2), 29);
        assert_eq!(days_in_month(2100, 2), 28);
        assert_eq!(LocalDate::parse("2028-02-29"), Some(d(2028, 2, 29)));
        for invalid in [
            "2027-02-29",
            "2026-11-31",
            "2026-00-01",
            "2026-1-01",
            "x",
            "2026-11-00",
        ] {
            assert_eq!(LocalDate::parse(invalid), None);
        }
        assert_eq!(d(2026, 11, 9).to_string(), "2026-11-09");
        assert_eq!(
            serde_json::to_string(&d(2026, 11, 9)).unwrap(),
            "\"2026-11-09\""
        );
        assert_eq!(
            serde_json::from_str::<LocalDate>("\"2026-11-09\"").unwrap(),
            d(2026, 11, 9)
        );
    }
}
