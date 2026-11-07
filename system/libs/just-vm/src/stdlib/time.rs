//! `import time`: the clock and calendar dates, in UTC.

use super::{int, num, record, text};
use crate::prelude::*;
use crate::value::Value;
use crate::vm::Vm;

const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

pub(super) fn call(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Value, String> {
    match name {
        "time.now" => Ok(Value::Float(vm.host.now())),
        "time.sleep" => {
            let secs = num(&args[0], name)?;
            if !(0.0..=1e9).contains(&secs) {
                return Err(format!("`time.sleep`: invalid duration {secs}"));
            }
            // In short steps, so that the host can interrupt a long sleep.
            let ms = (secs * 1000.0) as u64;
            let end = vm.host.clock_ms().saturating_add(ms);
            loop {
                let now = vm.host.clock_ms();
                if now >= end {
                    return Ok(Value::Nil);
                }
                if vm.interrupted() {
                    return Err("execution interrupted".into());
                }
                vm.host.sleep_ms((end - now).min(50));
            }
        }
        "time.date" => {
            let d = Date::from_timestamp(timestamp_arg(vm, args.first(), name)?);
            Ok(record([
                ("year", Value::Int(d.year)),
                ("month", Value::Int(d.month)),
                ("day", Value::Int(d.day)),
                ("hour", Value::Int(d.hour)),
                ("minute", Value::Int(d.minute)),
                ("second", Value::Int(d.second)),
                ("weekday", Value::Int(d.weekday)),
                ("yearday", Value::Int(d.yearday)),
            ]))
        }
        "time.format" => {
            let d = Date::from_timestamp(timestamp_arg(vm, args.first(), name)?);
            let pattern = match args.get(1) {
                Some(p) => text(p, name)?,
                None => "%Y-%m-%d %H:%M:%S",
            };
            Ok(Value::str(&d.format(pattern)))
        }
        "time.timestamp" => {
            let part = |i: usize| args.get(i).map_or(Ok(0), |v| int(v, name));
            let (year, month, day) = (part(0)?, part(1)?, part(2)?);
            let (hour, minute, second) = (part(3)?, part(4)?, part(5)?);
            if !(1..=12).contains(&month) || !(1..=days_in_month(year, month)).contains(&day) {
                return Err(format!("`time.timestamp`: {year}-{month:02}-{day:02} is not a date"));
            }
            if !(0..24).contains(&hour) || !(0..60).contains(&minute) || !(0..60).contains(&second) {
                return Err(format!("`time.timestamp`: {hour:02}:{minute:02}:{second:02} is not a time of day"));
            }
            if !(-1_000_000..=1_000_000).contains(&year) {
                return Err(format!("`time.timestamp`: year {year} out of range"));
            }
            Ok(Value::Int(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second))
        }
        _ => Err(format!("builtin `{name}` is not implemented by this runtime")),
    }
}

/// Whole seconds of a timestamp argument (default: now).
fn timestamp_arg(vm: &mut Vm, v: Option<&Value>, name: &str) -> Result<i64, String> {
    let t = match v {
        None => vm.host.now(),
        Some(v) => num(v, name)?,
    };
    // About ±1 million years.
    if !t.is_finite() || t.abs() > 3.2e13 {
        return Err(format!("`{name}`: timestamp {t} out of range"));
    }
    Ok(t.floor() as i64)
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        2 if is_leap(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

struct Date {
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
    /// 1 = Monday ... 7 = Sunday.
    weekday: i64,
    /// 1 = January 1st.
    yearday: i64,
}

impl Date {
    fn from_timestamp(t: i64) -> Date {
        let days = t.div_euclid(86_400);
        let secs = t.rem_euclid(86_400);
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = yoe + era * 400 + i64::from(month <= 2);
        Date {
            year,
            month,
            day,
            hour: secs / 3600,
            minute: secs / 60 % 60,
            second: secs % 60,
            // 1970-01-01 was a Thursday.
            weekday: (days + 3).rem_euclid(7) + 1,
            yearday: days - days_from_civil(year, 1, 1) + 1,
        }
    }

    fn format(&self, pattern: &str) -> String {
        let mut out = String::new();
        let mut chars = pattern.chars();
        while let Some(c) = chars.next() {
            if c != '%' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('Y') => out.push_str(&format!("{:04}", self.year)),
                Some('m') => out.push_str(&format!("{:02}", self.month)),
                Some('d') => out.push_str(&format!("{:02}", self.day)),
                Some('H') => out.push_str(&format!("{:02}", self.hour)),
                Some('M') => out.push_str(&format!("{:02}", self.minute)),
                Some('S') => out.push_str(&format!("{:02}", self.second)),
                Some('j') => out.push_str(&format!("{:03}", self.yearday)),
                Some('a') => out.push_str(WEEKDAYS[(self.weekday - 1) as usize]),
                Some('b') => out.push_str(MONTHS[(self.month - 1) as usize]),
                Some('%') => out.push('%'),
                Some(other) => {
                    out.push('%');
                    out.push(other);
                }
                None => out.push('%'),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates_round_trip() {
        for &(t, y, m, d, wd) in
            &[(0, 1970, 1, 1, 4), (951_782_400, 2000, 2, 29, 2), (1_792_540_800, 2026, 10, 21, 3), (-86_400, 1969, 12, 31, 3)]
        {
            let date = Date::from_timestamp(t);
            assert_eq!((date.year, date.month, date.day, date.weekday), (y, m, d, wd), "timestamp {t}");
            assert_eq!(days_from_civil(y, m, d) * 86_400, t);
        }
        let d = Date::from_timestamp(1_792_540_800 + 9 * 3600 + 30 * 60 + 5);
        assert_eq!(d.format("%Y-%m-%d %H:%M:%S %a %b %j %%"), "2026-10-21 09:30:05 Wed Oct 294 %");
    }
}
