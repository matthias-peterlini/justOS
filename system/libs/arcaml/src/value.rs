//! The data AML works with, and the conversions between them (ACPI 6.5,
//! 19.3.5: data type conversion rules).

use crate::name::NameString;
use crate::{Error, NodeId};
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

#[derive(Clone, Debug, PartialEq, Default)]
pub enum Value {
    #[default]
    Uninitialized,
    Integer(u64),
    /// Text; each character stands for one byte (0–255).
    String(String),
    Buffer(Vec<u8>),
    Package(Vec<Value>),
    /// A named object that is not plain data (a device, a method, a
    /// mutex, a region...); also a name written inside a package.
    Node(NodeId),
    /// A name inside a package that did not exist yet when the package was
    /// made, with the scope to look it up from.
    Name(NameString, NodeId),
    /// What RefOf, CondRefOf and Index give.
    Ref(Box<Reference>),
}

/// Where a reference points.
#[derive(Clone, Debug, PartialEq)]
pub enum Reference {
    Node(NodeId),
    /// A local variable or argument of the method at that depth.
    Local(u32, u8),
    Arg(u32, u8),
    /// One element of a package, byte of a buffer or character of a string.
    Index(Holder, usize),
}

/// What holds a package, buffer or string that is indexed or has fields.
#[derive(Clone, Debug, PartialEq)]
pub enum Holder {
    Node(NodeId),
    Local(u32, u8),
    Arg(u32, u8),
    /// A value computed on the spot: writes to it are lost.
    Temp(Box<Value>),
}

impl Value {
    pub fn as_integer(&self) -> Option<u64> {
        match self {
            Value::Integer(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_buffer(&self) -> Option<&[u8]> {
        match self {
            Value::Buffer(b) => Some(b),
            _ => None,
        }
    }

    pub fn as_package(&self) -> Option<&[Value]> {
        match self {
            Value::Package(p) => Some(p),
            _ => None,
        }
    }

    /// The ObjectType number of plain data (ACPI 6.5, 19.6.97).
    pub(crate) fn type_number(&self) -> u64 {
        match self {
            Value::Uninitialized => 0,
            Value::Integer(_) => 1,
            Value::String(_) => 2,
            Value::Buffer(_) => 3,
            Value::Package(_) => 4,
            Value::Node(_) | Value::Name(..) | Value::Ref(_) => 0,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Uninitialized => f.write_str("(none)"),
            Value::Integer(v) => write!(f, "{v:#x}"),
            Value::String(s) => write!(f, "\"{s}\""),
            Value::Buffer(b) => {
                write!(f, "Buffer({}) {{", b.len())?;
                for (i, x) in b.iter().take(32).enumerate() {
                    write!(f, "{}{x:02x}", if i == 0 { "" } else { " " })?;
                }
                f.write_str(if b.len() > 32 { " …}" } else { "}" })
            }
            Value::Package(p) => {
                f.write_str("[")?;
                for (i, x) in p.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{x}")?;
                }
                f.write_str("]")
            }
            Value::Node(n) => write!(f, "(object {})", n.0),
            Value::Name(n, _) => write!(f, "{n}"),
            Value::Ref(_) => f.write_str("(reference)"),
        }
    }
}

pub(crate) fn string_from_bytes(b: &[u8]) -> String {
    b.iter().map(|&c| c as char).collect()
}

pub(crate) fn string_bytes(s: &str) -> Vec<u8> {
    s.chars().map(|c| c as u32 as u8).collect()
}

fn mask(wide: bool) -> u64 {
    if wide {
        u64::MAX
    } else {
        0xFFFF_FFFF
    }
}

fn hex_prefix(s: &str) -> Option<&str> {
    s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))
}

fn parse_radix(s: &str, radix: u32) -> u64 {
    let mut v = 0u64;
    for c in s.chars() {
        let Some(d) = c.to_digit(radix) else { break };
        v = v.wrapping_mul(radix as u64).wrapping_add(d as u64);
    }
    v
}

/// The implicit conversion to an integer: a buffer gives its first bytes
/// (little endian), a string is read as hexadecimal.
pub(crate) fn to_integer(v: &Value, wide: bool) -> Result<u64, Error> {
    match v {
        Value::Integer(x) => Ok(*x & mask(wide)),
        Value::Buffer(b) => {
            let n = if wide { 8 } else { 4 };
            Ok(b.iter().take(n).rev().fold(0, |a, &x| a << 8 | x as u64))
        }
        Value::String(s) => {
            let s = s.trim_start();
            Ok(parse_radix(hex_prefix(s).unwrap_or(s), 16) & mask(wide))
        }
        _ => Err(Error::Type("not an integer")),
    }
}

/// ToInteger: a string is decimal unless it starts with 0x.
pub(crate) fn to_integer_explicit(v: &Value, wide: bool) -> Result<u64, Error> {
    match v {
        Value::String(s) => {
            let s = s.trim_start();
            let v = match hex_prefix(s) {
                Some(h) => parse_radix(h, 16),
                None => parse_radix(s, 10),
            };
            Ok(v & mask(wide))
        }
        _ => to_integer(v, wide),
    }
}

/// The conversion to a buffer: an integer gives its 8 (or 4) bytes, a
/// string its characters and a terminating zero.
pub(crate) fn to_buffer(v: &Value, wide: bool) -> Result<Vec<u8>, Error> {
    match v {
        Value::Integer(x) => {
            let n = if wide { 8 } else { 4 };
            Ok(x.to_le_bytes()[..n].to_vec())
        }
        Value::String(s) => {
            let mut b = string_bytes(s);
            b.push(0);
            Ok(b)
        }
        Value::Buffer(b) => Ok(b.clone()),
        _ => Err(Error::Type("not a buffer")),
    }
}

/// The implicit conversion to a string: an integer in hexadecimal with all
/// its digits, a buffer as hexadecimal bytes separated by spaces.
pub(crate) fn to_string_implicit(v: &Value, wide: bool) -> Result<String, Error> {
    match v {
        Value::Integer(x) => Ok(if wide { format!("{x:016X}") } else { format!("{:08X}", x & 0xFFFF_FFFF) }),
        Value::String(s) => Ok(s.clone()),
        Value::Buffer(b) => {
            let parts: Vec<String> = b.iter().map(|x| format!("{x:02X}")).collect();
            Ok(parts.join(" "))
        }
        _ => Err(Error::Type("not a string")),
    }
}

/// ToHexString.
pub(crate) fn to_hex_string(v: &Value, wide: bool) -> Result<String, Error> {
    match v {
        Value::Integer(x) => Ok(format!("0x{:X}", x & mask(wide))),
        Value::Buffer(b) => {
            let parts: Vec<String> = b.iter().map(|x| format!("0x{x:02X}")).collect();
            Ok(parts.join(","))
        }
        Value::String(s) => Ok(s.clone()),
        _ => Err(Error::Type("ToHexString")),
    }
}

/// ToDecimalString.
pub(crate) fn to_decimal_string(v: &Value, wide: bool) -> Result<String, Error> {
    match v {
        Value::Integer(x) => Ok(format!("{}", x & mask(wide))),
        Value::Buffer(b) => {
            let parts: Vec<String> = b.iter().map(|x| format!("{x}")).collect();
            Ok(parts.join(","))
        }
        Value::String(s) => Ok(s.clone()),
        _ => Err(Error::Type("ToDecimalString")),
    }
}

/// The text of a compressed EISA id (`_HID` as an integer): "PNP0C0A".
pub fn eisa_id(v: u32) -> String {
    let v = v.swap_bytes();
    let c = |shift: u32| (((v >> shift) & 0x1F) as u8 + 0x40) as char;
    format!("{}{}{}{:04X}", c(26), c(21), c(16), v & 0xFFFF)
}
