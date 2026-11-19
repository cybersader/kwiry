//! Wall and monotonic time, and the wire's strict UTC timestamp format.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// The holder's view of time. Leases are judged on both clocks; clock-skew
/// evidence compares authority `iat` with the wall clock and measures round
/// trips on the monotonic clock.
pub trait Clock: Send + Sync {
    /// Wall-clock milliseconds since the Unix epoch.
    fn wall_ms(&self) -> i64;
    /// Monotonic milliseconds from an arbitrary, process-local origin.
    fn mono_ms(&self) -> u64;
}

pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn wall_ms(&self) -> i64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(elapsed) => i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX),
            Err(before) => -i64::try_from(before.duration().as_millis()).unwrap_or(i64::MAX),
        }
    }

    fn mono_ms(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        _ => 28,
    }
}

/// Parses `YYYY-MM-DDTHH:MM:SS[.fraction]Z` into Unix milliseconds. Only the
/// UTC `Z` form is accepted. Fractions beyond milliseconds are truncated,
/// which can only move a deadline earlier.
pub fn parse_utc_ms(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    let number = |from: usize, to: usize| -> Option<i64> {
        let digits = &bytes[from..to];
        digits.iter().all(u8::is_ascii_digit).then(|| {
            digits
                .iter()
                .fold(0, |acc, d| acc * 10 + i64::from(d - b'0'))
        })
    };
    let (year, month, day) = (number(0, 4)?, number(5, 7)?, number(8, 10)?);
    let (hour, minute, second) = (number(11, 13)?, number(14, 16)?, number(17, 19)?);
    if !(1..=12).contains(&month)
        || day < 1
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let mut index = 19;
    let mut millis = 0;
    if bytes[index] == b'.' {
        index += 1;
        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        if index == start {
            return None;
        }
        for (position, digit) in bytes[start..index].iter().take(3).enumerate() {
            millis += i64::from(digit - b'0') * [100, 10, 1][position];
        }
    }
    if index + 1 != bytes.len() || bytes[index] != b'Z' {
        return None;
    }
    let seconds = days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second;
    seconds.checked_mul(1000)?.checked_add(millis)
}

/// Formats Unix milliseconds as `YYYY-MM-DDTHH:MM:SSZ` (seconds truncated).
pub fn format_utc_seconds(ms: i64) -> String {
    let seconds = ms.div_euclid(1000);
    let (days, rest) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_round_trip_and_strictness() {
        let ms = parse_utc_ms("2026-11-17T12:15:01Z").unwrap();
        assert_eq!(ms, 1_794_917_701_000);
        assert_eq!(format_utc_seconds(ms), "2026-11-17T12:15:01Z");
        assert_eq!(
            parse_utc_ms("2026-11-17T12:15:01.9999Z"),
            Some(1_794_917_701_999)
        );
        assert_eq!(
            parse_utc_ms("2024-02-29T00:00:00Z"),
            Some(1_709_164_800_000)
        );
        for bad in [
            "2026-11-17T12:15:01",
            "2026-11-17T12:15:01+00:00",
            "2026-11-17 12:15:01Z",
            "2025-02-29T00:00:00Z",
            "2026-13-01T00:00:00Z",
            "2026-11-17T24:00:00Z",
            "2026-11-17T12:15:60Z",
            "2026-11-17T12:15:01.Z",
            "2026-11-17T12:15:01Zx",
        ] {
            assert_eq!(parse_utc_ms(bad), None, "{bad}");
        }
    }
}
