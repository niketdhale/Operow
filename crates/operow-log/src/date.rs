//! Calendar date of the `date` header line, formatted without a date crate.

use std::time::{SystemTime, UNIX_EPOCH};

const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// A UTC date and time of day with millisecond resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AscDate {
    pub year: i32,
    /// 1-12.
    pub month: u32,
    /// 1-31.
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub millis: u32,
    /// 0 = Sunday.
    pub weekday: u32,
}

impl AscDate {
    pub fn from_unix_ms(ms: u64) -> Self {
        let secs = ms / 1000;
        let days = (secs / 86_400) as i64;
        let rem = (secs % 86_400) as u32;
        // Howard Hinnant's civil-from-days.
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        let year = (yoe + era * 400 + i64::from(month <= 2)) as i32;
        AscDate {
            year,
            month,
            day,
            hour: rem / 3600,
            minute: rem % 3600 / 60,
            second: rem % 60,
            millis: (ms % 1000) as u32,
            weekday: ((days + 4).rem_euclid(7)) as u32,
        }
    }

    pub fn now() -> Self {
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        Self::from_unix_ms(ms)
    }

    /// `Fri Oct 03 02:15:30.123 pm 2026`
    pub fn asc_string(&self) -> String {
        let h12 = match self.hour % 12 {
            0 => 12,
            h => h,
        };
        format!(
            "{} {} {:02} {:02}:{:02}:{:02}.{:03} {} {}",
            DAYS[self.weekday as usize % 7],
            MONTHS[(self.month as usize).clamp(1, 12) - 1],
            self.day,
            h12,
            self.minute,
            self.second,
            self.millis,
            if self.hour < 12 { "am" } else { "pm" },
            self.year
        )
    }

    /// `2026-10-03`
    pub fn date_string(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// `14-15-30`
    pub fn time_string(&self) -> String {
        format!("{:02}-{:02}-{:02}", self.hour, self.minute, self.second)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_dates() {
        let d = AscDate::from_unix_ms(0);
        assert_eq!(d.asc_string(), "Thu Jan 01 12:00:00.000 am 1970");
        // 2026-10-03 14:15:30.123 UTC is a Saturday.
        let d = AscDate::from_unix_ms(1_790_000_130_123);
        assert_eq!(d.date_string(), "2026-09-21");
        let d = AscDate::from_unix_ms(1_791_036_930_123);
        assert_eq!(d.asc_string(), "Sat Oct 03 02:15:30.123 pm 2026");
        assert_eq!(d.time_string(), "14-15-30");
        // Leap day.
        let d = AscDate::from_unix_ms(951_782_400_000);
        assert_eq!(d.date_string(), "2000-02-29");
    }
}
