//! `Date`. arcOS keeps no time zone, so local time is UTC.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use arcc_core::float as fm;

use crate::builtins::{ctor, func, num_arg};
use crate::interp::*;
use crate::value::*;

const DAY: f64 = 86_400_000.0;
const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// Days since 1970-01-01 of a proleptic Gregorian date (month 1..=12).
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The inverse of [`days_from_civil`]: (year, month 1..=12, day).
pub fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Broken-down time: year, month 0..=11, day, hours, minutes, seconds,
/// milliseconds, weekday.
#[derive(Clone, Copy)]
pub struct Parts {
    pub f: [f64; 7],
    pub wday: usize,
}

pub fn split(t: f64) -> Parts {
    let days = fm::floor(t / DAY);
    let ms = t - days * DAY;
    let (y, m, d) = civil_from_days(days as i64);
    let ms_i = ms as i64;
    Parts {
        f: [
            y as f64,
            (m - 1) as f64,
            d as f64,
            (ms_i / 3_600_000) as f64,
            (ms_i / 60_000 % 60) as f64,
            (ms_i / 1000 % 60) as f64,
            (ms_i % 1000) as f64,
        ],
        wday: (days as i64 + 4).rem_euclid(7) as usize,
    }
}

/// Builds a time value from possibly out-of-range fields, like `Date.UTC`.
pub fn make(f: &[f64; 7]) -> f64 {
    if f.iter().any(|x| !x.is_finite()) {
        return f64::NAN;
    }
    let f: Vec<f64> = f.iter().map(|x| fm::trunc(*x)).collect();
    let y = f[0] + fm::floor(f[1] / 12.0);
    let m = f[1] - fm::floor(f[1] / 12.0) * 12.0;
    if y.abs() > 400_000.0 {
        return f64::NAN;
    }
    let days = days_from_civil(y as i64, m as i64 + 1, 1) as f64 + f[2] - 1.0;
    clip(days * DAY + f[3] * 3_600_000.0 + f[4] * 60_000.0 + f[5] * 1000.0 + f[6])
}

fn clip(t: f64) -> f64 {
    if !t.is_finite() || t.abs() > 8.64e15 {
        f64::NAN
    } else {
        fm::trunc(t) + 0.0
    }
}

fn month_of(word: &str) -> Option<usize> {
    let w = word.to_ascii_lowercase();
    if w.len() < 3 {
        return None;
    }
    MONTHS.iter().position(|m| w.starts_with(&m.to_ascii_lowercase()))
}

/// Parses ISO 8601 and the common `toString`/`toUTCString` shapes.
pub fn parse(s: &str) -> f64 {
    let s = s.trim();
    iso(s).unwrap_or_else(|| loose(s).unwrap_or(f64::NAN))
}

fn iso(s: &str) -> Option<f64> {
    let b = s.as_bytes();
    let mut i = 0;
    let num = |i: &mut usize, n: usize| -> Option<f64> {
        let t = s.get(*i..*i + n)?;
        if !t.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        *i += n;
        t.parse().ok()
    };
    let year = match b.first()? {
        b'+' | b'-' => {
            let neg = b[0] == b'-';
            i = 1;
            let y = num(&mut i, 6)?;
            if neg {
                -y
            } else {
                y
            }
        }
        _ => num(&mut i, 4)?,
    };
    let mut f = [year, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
    if b.get(i) == Some(&b'-') {
        i += 1;
        f[1] = num(&mut i, 2)? - 1.0;
        if b.get(i) == Some(&b'-') {
            i += 1;
            f[2] = num(&mut i, 2)?;
        }
    }
    let mut offset = 0.0;
    if matches!(b.get(i), Some(b'T' | b't' | b' ')) {
        i += 1;
        f[3] = num(&mut i, 2)?;
        if b.get(i) != Some(&b':') {
            return None;
        }
        i += 1;
        f[4] = num(&mut i, 2)?;
        if b.get(i) == Some(&b':') {
            i += 1;
            f[5] = num(&mut i, 2)?;
            if b.get(i) == Some(&b'.') {
                i += 1;
                let start = i;
                while b.get(i).is_some_and(u8::is_ascii_digit) {
                    i += 1;
                }
                let frac = &s[start..i];
                let ms: String = frac.chars().chain("000".chars()).take(3).collect();
                f[6] = ms.parse().ok()?;
            }
        }
        match b.get(i) {
            Some(b'Z' | b'z') => i += 1,
            Some(c @ (b'+' | b'-')) => {
                let sign = if *c == b'-' { -1.0 } else { 1.0 };
                i += 1;
                let h = num(&mut i, 2)?;
                if b.get(i) == Some(&b':') {
                    i += 1;
                }
                let m = num(&mut i, 2)?;
                offset = sign * (h * 60.0 + m) * 60_000.0;
            }
            _ => {}
        }
    }
    if i != b.len() || f[1] > 11.0 || f[2] > 31.0 || f[3] > 24.0 || f[4] > 59.0 || f[5] > 59.0 {
        return None;
    }
    Some(make(&f) - offset)
}

/// "Fri Jan 05 2024 10:00:00 GMT+0100", "Fri, 05 Jan 2024 10:00:00 GMT",
/// "January 5, 2024", "2024/01/05 10:00".
fn loose(s: &str) -> Option<f64> {
    let mut nums: Vec<f64> = Vec::new();
    let mut month = None;
    let mut time: Option<[f64; 3]> = None;
    let mut offset = 0.0;
    let mut pm = None;
    let s = s.split('(').next().unwrap_or(s);
    for word in s.split([' ', ',']) {
        if word.is_empty() {
            continue;
        }
        if word.contains(':') {
            let mut t = [0.0; 3];
            for (k, p) in word.split(':').take(3).enumerate() {
                t[k] = p.parse().ok()?;
            }
            time = Some(t);
        } else if let Some(rest) = word.strip_prefix("GMT").or_else(|| word.strip_prefix("UTC")).or(if word.starts_with(['+', '-']) && !nums.is_empty() { Some(word) } else { None }) {
            if rest.len() >= 5 {
                let sign = if rest.starts_with('-') { -1.0 } else { 1.0 };
                let h: f64 = rest.get(1..3)?.parse().ok()?;
                let m: f64 = rest.get(rest.len() - 2..)?.parse().ok()?;
                offset = sign * (h * 60.0 + m) * 60_000.0;
            }
        } else if word.contains('/') || (word.contains('-') && word.as_bytes()[0].is_ascii_digit()) {
            for p in word.split(['/', '-']) {
                nums.push(p.parse().ok()?);
            }
        } else if let Ok(n) = word.parse::<f64>() {
            nums.push(n);
        } else if let Some(m) = month_of(word) {
            month = Some(m);
        } else if word.eq_ignore_ascii_case("pm") {
            pm = Some(true);
        } else if word.eq_ignore_ascii_case("am") {
            pm = Some(false);
        } else if word != "Z" && !DAYS.iter().any(|d| word.get(..3).is_some_and(|w| w.eq_ignore_ascii_case(d))) {
            return None;
        }
    }
    let (y, m, d) = match (month, nums.as_slice()) {
        (Some(m), [a, b]) => {
            if *a > 31.0 {
                (*a, m as f64, *b)
            } else if *b > 31.0 {
                (*b, m as f64, *a)
            } else {
                return None;
            }
        }
        (None, [a, b, c]) => {
            if *a > 31.0 {
                (*a, b - 1.0, *c)
            } else {
                (*c, a - 1.0, *b)
            }
        }
        _ => return None,
    };
    let mut t = time.unwrap_or([0.0; 3]);
    match pm {
        Some(true) if t[0] < 12.0 => t[0] += 12.0,
        Some(false) if t[0] == 12.0 => t[0] = 0.0,
        _ => {}
    }
    Some(make(&[y, m, d, t[0], t[1], t[2], 0.0]) - offset)
}

fn pad(n: f64, w: usize) -> String {
    format!("{:0w$}", n as i64)
}

pub fn iso_string(t: f64) -> String {
    let p = split(t);
    let y = p.f[0];
    let year = if (0.0..=9999.0).contains(&y) {
        pad(y, 4)
    } else {
        format!("{}{:06}", if y < 0.0 { '-' } else { '+' }, y.abs() as i64)
    };
    format!("{year}-{}-{}T{}:{}:{}.{}Z", pad(p.f[1] + 1.0, 2), pad(p.f[2], 2), pad(p.f[3], 2), pad(p.f[4], 2), pad(p.f[5], 2), pad(p.f[6], 3))
}

fn year_text(y: f64) -> String {
    if y < 0.0 {
        format!("-{}", pad(-y, 6))
    } else {
        pad(y, 4)
    }
}

fn date_text(p: &Parts) -> String {
    format!("{} {} {} {}", DAYS[p.wday], MONTHS[p.f[1] as usize], pad(p.f[2], 2), year_text(p.f[0]))
}

fn time_text(p: &Parts) -> String {
    format!("{}:{}:{} GMT+0000 (Coordinated Universal Time)", pad(p.f[3], 2), pad(p.f[4], 2), pad(p.f[5], 2))
}

pub fn to_string(t: f64) -> String {
    if t.is_nan() {
        return String::from("Invalid Date");
    }
    let p = split(t);
    format!("{} {}", date_text(&p), time_text(&p))
}

fn locale_date(p: &Parts) -> String {
    format!("{}/{}/{}", p.f[2], p.f[1] + 1.0, p.f[0])
}

fn locale_time(p: &Parts) -> String {
    format!("{}:{}:{}", pad(p.f[3], 2), pad(p.f[4], 2), pad(p.f[5], 2))
}

fn this_time(it: &mut Interp, this: &Value) -> R<f64> {
    if let Value::Obj(o) = this {
        if let Kind::Date(t) = o.borrow().kind {
            return Ok(t);
        }
    }
    Err(it.type_error("this is not a Date object."))
}

fn set_time(this: &Value, t: f64) -> Value {
    if let Value::Obj(o) = this {
        o.borrow_mut().kind = Kind::Date(t);
    }
    Value::Num(t)
}

fn now(it: &Interp) -> f64 {
    fm::floor(it.epoch + it.now)
}

fn fields(it: &mut Interp, args: &[Value]) -> R<[f64; 7]> {
    let mut f = [f64::NAN, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
    for (k, slot) in f.iter_mut().enumerate().take(args.len().min(7)) {
        *slot = num_arg(it, args, k)?;
    }
    if (0.0..=99.0).contains(&fm::trunc(f[0])) {
        f[0] = 1900.0 + fm::trunc(f[0]);
    }
    Ok(f)
}

fn date_ctor(it: &mut Interp, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    if it.new_target.nullish() {
        return Ok(Value::str(&to_string(now(it))));
    }
    let t = match args.len() {
        0 => now(it),
        1 => {
            let v = arg(args, 0);
            let inner = match &v {
                Value::Obj(o) => match o.borrow().kind {
                    Kind::Date(t) => Some(t),
                    _ => None,
                },
                _ => None,
            };
            match inner {
                Some(t) => t,
                None => match it.to_prim(&v, Hint::Default)? {
                    Value::Str(s) => parse(&s),
                    p => clip(it.to_number(&p)?),
                },
            }
        }
        _ => {
            let f = fields(it, args)?;
            make(&f)
        }
    };
    if let Value::Obj(o) = this {
        o.borrow_mut().kind = Kind::Date(t);
    }
    Ok(Value::Undef)
}

/// A method whose behaviour is picked by `data`.
fn method_with(it: &mut Interp, o: &ObjRef, name: &str, arity: u32, f: NativeFn, data: f64) {
    let m = it.native(name, arity, f, Value::Num(data), false);
    o.borrow_mut().props.insert(Key::str(name), Prop::hidden(Value::Obj(m)));
}

fn getter(it: &mut Interp, this: &Value, _: &[Value], data: &Value) -> R<Value> {
    let t = this_time(it, this)?;
    if t.is_nan() {
        return Ok(Value::Num(f64::NAN));
    }
    let Value::Num(k) = data else { return Ok(Value::Undef) };
    let p = split(t);
    Ok(Value::Num(match *k as usize {
        7 => p.wday as f64,
        8 => p.f[0] - 1900.0,
        k => p.f[k],
    }))
}

/// `data` is `first field * 10 + how many fields the setter takes`.
fn setter(it: &mut Interp, this: &Value, args: &[Value], data: &Value) -> R<Value> {
    let t = this_time(it, this)?;
    let Value::Num(d) = data else { return Ok(Value::Undef) };
    let (first, count) = ((*d as usize) / 10, (*d as usize) % 10);
    if first > 0 && t.is_nan() {
        for k in 0..args.len().min(count) {
            num_arg(it, args, k)?;
        }
        return Ok(Value::Num(f64::NAN));
    }
    let mut p = split(if t.is_nan() { 0.0 } else { t });
    let n = args.len().clamp(1, count);
    for k in 0..n {
        p.f[first + k] = num_arg(it, args, k)?;
    }
    Ok(set_time(this, make(&p.f)))
}

pub fn install(it: &mut Interp) {
    let proto = it.p.date.clone();
    let c = ctor(it, "Date", 7, date_ctor, &proto);
    func(it, &c, "now", 0, |it, _, _, _| Ok(Value::Num(now(it))));
    func(it, &c, "parse", 1, |it, _, args, _| {
        let s = crate::builtins::str_arg(it, args, 0)?;
        Ok(Value::Num(parse(&s)))
    });
    func(it, &c, "UTC", 7, |it, _, args, _| {
        let f = fields(it, args)?;
        Ok(Value::Num(make(&f)))
    });

    let gets = [("FullYear", 0.0), ("Month", 1.0), ("Date", 2.0), ("Hours", 3.0), ("Minutes", 4.0), ("Seconds", 5.0), ("Milliseconds", 6.0), ("Day", 7.0)];
    for (name, k) in gets {
        method_with(it, &proto, &format!("get{name}"), 0, getter, k);
        method_with(it, &proto, &format!("getUTC{name}"), 0, getter, k);
    }
    method_with(it, &proto, "getYear", 0, getter, 8.0);
    let sets = [("FullYear", 3.0), ("Month", 12.0), ("Date", 21.0), ("Hours", 34.0), ("Minutes", 43.0), ("Seconds", 52.0), ("Milliseconds", 61.0)];
    for (name, d) in sets {
        method_with(it, &proto, &format!("set{name}"), (d as u32 % 10).max(1), setter, d);
        method_with(it, &proto, &format!("setUTC{name}"), (d as u32 % 10).max(1), setter, d);
    }
    func(it, &proto, "getTime", 0, |it, this, _, _| Ok(Value::Num(this_time(it, this)?)));
    func(it, &proto, "valueOf", 0, |it, this, _, _| Ok(Value::Num(this_time(it, this)?)));
    func(it, &proto, "getTimezoneOffset", 0, |it, this, _, _| {
        let t = this_time(it, this)?;
        Ok(Value::Num(if t.is_nan() { f64::NAN } else { 0.0 }))
    });
    func(it, &proto, "setTime", 1, |it, this, args, _| {
        this_time(it, this)?;
        let t = clip(num_arg(it, args, 0)?);
        Ok(set_time(this, t))
    });
    func(it, &proto, "toISOString", 0, |it, this, _, _| {
        let t = this_time(it, this)?;
        if t.is_nan() {
            return Err(it.range_error("Invalid time value"));
        }
        Ok(Value::str(&iso_string(t)))
    });
    func(it, &proto, "toJSON", 1, |it, this, _, _| {
        let t = match this {
            Value::Obj(o) => match o.borrow().kind {
                Kind::Date(t) => Some(t),
                _ => None,
            },
            _ => None,
        };
        match t {
            Some(t) if t.is_nan() => Ok(Value::Null),
            Some(t) => Ok(Value::str(&iso_string(t))),
            None => {
                let f = it.get(this, &Key::str("toISOString"))?;
                it.call(&f, this.clone(), &[])
            }
        }
    });
    func(it, &proto, "toString", 0, |it, this, _, _| Ok(Value::str(&to_string(this_time(it, this)?))));

    fn text(it: &mut Interp, this: &Value, f: fn(&Parts) -> String) -> R<Value> {
        let t = this_time(it, this)?;
        Ok(Value::str(&if t.is_nan() { String::from("Invalid Date") } else { f(&split(t)) }))
    }
    func(it, &proto, "toDateString", 0, |it, this, _, _| text(it, this, date_text));
    func(it, &proto, "toTimeString", 0, |it, this, _, _| text(it, this, time_text));
    func(it, &proto, "toUTCString", 0, |it, this, _, _| {
        text(it, this, |p| format!("{}, {} {} {} {}:{}:{} GMT", DAYS[p.wday], pad(p.f[2], 2), MONTHS[p.f[1] as usize], year_text(p.f[0]), pad(p.f[3], 2), pad(p.f[4], 2), pad(p.f[5], 2)))
    });
    let utc = it.get(&Value::Obj(proto.clone()), &Key::str("toUTCString")).unwrap_or(Value::Undef);
    it.set_hidden(&proto, "toGMTString", utc);
    func(it, &proto, "toLocaleDateString", 0, |it, this, _, _| text(it, this, locale_date));
    func(it, &proto, "toLocaleTimeString", 0, |it, this, _, _| text(it, this, locale_time));
    func(it, &proto, "toLocaleString", 0, |it, this, _, _| text(it, this, |p| format!("{}, {}", locale_date(p), locale_time(p))));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trip() {
        for z in [-800_000i64, -1, 0, 1, 11_016, 19_723, 2_932_896] {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
    }

    #[test]
    fn parse_and_format() {
        let t = parse("2024-01-05T10:20:30.250Z");
        assert_eq!(iso_string(t), "2024-01-05T10:20:30.250Z");
        assert_eq!(to_string(t), "Fri Jan 05 2024 10:20:30 GMT+0000 (Coordinated Universal Time)");
        assert_eq!(parse("2024-01-05"), parse("2024-01-05T00:00:00Z"));
        assert_eq!(parse("2024-01-05T12:00:00+02:00"), parse("2024-01-05T10:00:00Z"));
        assert_eq!(parse(&to_string(t)), parse("2024-01-05T10:20:30Z"));
        assert_eq!(parse("Fri, 05 Jan 2024 10:20:30 GMT"), parse("2024-01-05T10:20:30Z"));
        assert_eq!(parse("January 5, 2024"), parse("2024-01-05"));
        assert!(parse("ciao").is_nan());
        assert_eq!(make(&[2024.0, 13.0, 1.0, 0.0, 0.0, 0.0, 0.0]), parse("2025-02-01"));
    }
}
