//! Calculator: + - * / % ^, parentheses, sqrt, abs, pi, the last result as
//! `ans`. Run with an expression, or interactively.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use justrt::console::color::*;
use justrt::{print, println};

justrt::main!(main);

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    ans: f64,
}

type R = Result<f64, String>;

impl Parser<'_> {
    fn skip(&mut self) {
        while self.i < self.s.len() && self.s[self.i] == b' ' {
            self.i += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip();
        self.s.get(self.i).copied()
    }

    fn expr(&mut self) -> R {
        let mut v = self.term()?;
        while let Some(op @ (b'+' | b'-')) = self.peek() {
            self.i += 1;
            let r = self.term()?;
            v = if op == b'+' { v + r } else { v - r };
        }
        Ok(v)
    }

    fn term(&mut self) -> R {
        let mut v = self.power()?;
        while let Some(op @ (b'*' | b'/' | b'%')) = self.peek() {
            self.i += 1;
            let r = self.power()?;
            v = match op {
                b'*' => v * r,
                _ if r == 0.0 => return Err("division by zero".into()),
                b'/' => v / r,
                _ => v - r * trunc(v / r),
            };
        }
        Ok(v)
    }

    fn power(&mut self) -> R {
        let base = self.unary()?;
        if self.peek() == Some(b'^') {
            self.i += 1;
            let exp = self.power()?;
            return pow(base, exp);
        }
        Ok(base)
    }

    fn unary(&mut self) -> R {
        match self.peek() {
            Some(b'-') => {
                self.i += 1;
                Ok(-self.unary()?)
            }
            Some(b'+') => {
                self.i += 1;
                self.unary()
            }
            _ => self.atom(),
        }
    }

    fn atom(&mut self) -> R {
        match self.peek() {
            Some(b'(') => {
                self.i += 1;
                let v = self.expr()?;
                if self.peek() != Some(b')') {
                    return Err("missing )".into());
                }
                self.i += 1;
                Ok(v)
            }
            Some(c) if c.is_ascii_digit() || c == b'.' => {
                let start = self.i;
                while self.i < self.s.len() && (self.s[self.i].is_ascii_digit() || self.s[self.i] == b'.') {
                    self.i += 1;
                }
                let text = core::str::from_utf8(&self.s[start..self.i]).unwrap();
                text.parse::<f64>().map_err(|_| alloc::format!("bad number {text}"))
            }
            Some(c) if c.is_ascii_alphabetic() => {
                let start = self.i;
                while self.i < self.s.len() && self.s[self.i].is_ascii_alphabetic() {
                    self.i += 1;
                }
                let name = core::str::from_utf8(&self.s[start..self.i]).unwrap();
                match name {
                    "pi" => Ok(core::f64::consts::PI),
                    "e" => Ok(core::f64::consts::E),
                    "ans" => Ok(self.ans),
                    "sqrt" | "abs" => {
                        let v = self.atom()?;
                        if name == "abs" {
                            Ok(abs(v))
                        } else if v < 0.0 {
                            Err("square root of a negative number".into())
                        } else {
                            Ok(sqrt(v))
                        }
                    }
                    _ => Err(alloc::format!("unknown name {name}")),
                }
            }
            Some(c) => Err(alloc::format!("unexpected '{}'", c as char)),
            None => Err("incomplete expression".into()),
        }
    }
}

fn abs(v: f64) -> f64 {
    if v < 0.0 {
        -v
    } else {
        v
    }
}

fn trunc(v: f64) -> f64 {
    if abs(v) >= 9.0e15 {
        v
    } else {
        (v as i64) as f64
    }
}

fn sqrt(v: f64) -> f64 {
    if v == 0.0 {
        return 0.0;
    }
    let mut x = if v > 1.0 { v / 2.0 } else { 1.0 };
    for _ in 0..60 {
        x = 0.5 * (x + v / x);
    }
    x
}

fn pow(base: f64, exp: f64) -> R {
    if exp != trunc(exp) || abs(exp) > 4096.0 {
        return Err("only whole exponents up to 4096".into());
    }
    let mut r = 1.0;
    for _ in 0..abs(exp) as u32 {
        r *= base;
    }
    Ok(if exp < 0.0 { 1.0 / r } else { r })
}

fn format(v: f64) -> String {
    if v == trunc(v) && abs(v) < 1e15 {
        alloc::format!("{}", v as i64)
    } else {
        let s = alloc::format!("{:.10}", v);
        String::from(s.trim_end_matches('0').trim_end_matches('.'))
    }
}

fn eval(line: &str, ans: f64) -> R {
    let mut p = Parser { s: line.as_bytes(), i: 0, ans };
    let v = p.expr()?;
    if p.peek().is_some() {
        return Err(alloc::format!("unexpected '{}'", p.s[p.i] as char));
    }
    Ok(v)
}

fn main(args: &str) -> i32 {
    if !args.trim().is_empty() {
        return match eval(args, 0.0) {
            Ok(v) => {
                println!("{}", format(v));
                0
            }
            Err(e) => {
                justrt::eprintln!("{e}");
                1
            }
        };
    }
    println!("{ACCENT}calc{RESET} {DIM}— type an expression, Esc or an empty line to leave{RESET}");
    let mut ans = 0.0;
    loop {
        print!("{ACCENT}›{RESET} ");
        let Some(line) = justrt::console::read_line() else { break };
        if line.trim().is_empty() {
            break;
        }
        match eval(&line, ans) {
            Ok(v) => {
                ans = v;
                println!("  = {WHITE}{}{RESET}", format(v));
            }
            Err(e) => println!("  {RED}{e}{RESET}"),
        }
    }
    0
}
