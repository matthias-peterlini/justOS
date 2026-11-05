//! Time.

use crate::sys;

/// Milliseconds since the computer started.
pub fn uptime_ms() -> u64 {
    sys::call(sys::UPTIME, 0, 0, 0, 0, 0) as u64
}

pub fn sleep_ms(ms: u64) {
    sys::call(sys::SLEEP, ms, 0, 0, 0, 0);
}

/// Seconds since 2000-01-01 00:00.
pub fn now() -> u64 {
    sys::call(sys::NOW, 0, 0, 0, 0, 0) as u64
}

#[derive(Clone, Copy, Debug)]
pub struct DateTime {
    pub year: u64,
    pub month: u64,
    pub day: u64,
    pub hour: u64,
    pub min: u64,
    pub sec: u64,
    /// 0 = Monday
    pub weekday: u64,
}

fn leap(y: u64) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}

pub fn days_in_month(y: u64, m: u64) -> u64 {
    match m {
        2 if leap(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

pub fn date(secs: u64) -> DateTime {
    let mut days = secs / 86400;
    let rem = secs % 86400;
    let weekday = (days + 5) % 7;
    let mut year = 2000;
    while days >= if leap(year) { 366 } else { 365 } {
        days -= if leap(year) { 366 } else { 365 };
        year += 1;
    }
    let mut month = 1;
    while days >= days_in_month(year, month) {
        days -= days_in_month(year, month);
        month += 1;
    }
    DateTime { year, month, day: days + 1, hour: rem / 3600, min: rem / 60 % 60, sec: rem % 60, weekday }
}
