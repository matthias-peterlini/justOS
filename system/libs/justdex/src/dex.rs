//! Reading a DEX file: the tables of strings, types, prototypes, fields and
//! methods, the class definitions and the code of each method.
//!
//! Format: <https://source.android.com/docs/core/runtime/dex-format>.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

pub const NO_INDEX: u32 = 0xFFFF_FFFF;

pub const ACC_STATIC: u32 = 0x8;
pub const ACC_INTERFACE: u32 = 0x200;
pub const ACC_ABSTRACT: u32 = 0x400;

#[derive(Debug)]
pub struct Error(pub &'static str);

type Result<T> = core::result::Result<T, Error>;

pub struct Proto {
    pub shorty: Rc<str>,
    /// "(ILjava/lang/String;)V".
    pub sig: Rc<str>,
}

pub struct FieldId {
    pub class: u32,
    pub ty: u32,
    pub name: u32,
}

pub struct MethodId {
    pub class: u32,
    pub proto: u32,
    pub name: u32,
}

pub struct Method {
    pub idx: u32,
    pub access: u32,
    pub code_off: u32,
}

pub struct ClassDef {
    pub class: u32,
    pub access: u32,
    pub superclass: u32,
    pub interfaces: Vec<u32>,
    /// Field indices.
    pub static_fields: Vec<u32>,
    pub instance_fields: Vec<u32>,
    pub direct: Vec<Method>,
    pub virtual_: Vec<Method>,
    pub static_values_off: u32,
}

/// A try block: `[start, end)` in code units, and where its handlers are.
pub struct Try {
    pub start: u32,
    pub end: u32,
    /// (type index or NO_INDEX for catch-all, handler address).
    pub handlers: Vec<(u32, u32)>,
}

pub struct Code {
    pub registers: u16,
    pub ins: u16,
    pub insns: Vec<u16>,
    pub tries: Vec<Try>,
}

/// A constant from a class's static values (`encoded_value`).
#[derive(Clone, Debug)]
pub enum Const {
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    String(u32),
    Type(u32),
    Null,
    Bool(bool),
    Other,
}

pub struct Dex {
    pub data: Vec<u8>,
    pub strings: Vec<Rc<str>>,
    /// Type descriptors ("Ljava/lang/String;", "I", "[I").
    pub types: Vec<Rc<str>>,
    pub protos: Vec<Proto>,
    pub fields: Vec<FieldId>,
    pub methods: Vec<MethodId>,
    pub classes: Vec<ClassDef>,
}

fn u16_at(d: &[u8], o: usize) -> Result<u16> {
    d.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]])).ok_or(Error("truncated"))
}
fn u32_at(d: &[u8], o: usize) -> Result<u32> {
    d.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).ok_or(Error("truncated"))
}

pub fn uleb(d: &[u8], at: &mut usize) -> Result<u32> {
    let mut v = 0u32;
    for shift in (0..35).step_by(7) {
        let b = *d.get(*at).ok_or(Error("truncated"))?;
        *at += 1;
        v |= ((b & 0x7F) as u32) << shift;
        if b & 0x80 == 0 {
            break;
        }
    }
    Ok(v)
}

fn sleb(d: &[u8], at: &mut usize) -> Result<i32> {
    let mut v = 0i32;
    let mut shift = 0;
    loop {
        let b = *d.get(*at).ok_or(Error("truncated"))?;
        *at += 1;
        v |= ((b & 0x7F) as i32) << shift;
        shift += 7;
        if b & 0x80 == 0 {
            if shift < 32 && b & 0x40 != 0 {
                v |= -1 << shift;
            }
            return Ok(v);
        }
        if shift >= 35 {
            return Ok(v);
        }
    }
}

/// Modified UTF-8 (as DEX and Java class files store strings).
fn mutf8(d: &[u8], mut at: usize) -> String {
    let mut units: Vec<u16> = Vec::new();
    while let Some(&b) = d.get(at) {
        if b == 0 {
            break;
        }
        let (c, n) = if b < 0x80 {
            (b as u16, 1)
        } else if b & 0xE0 == 0xC0 {
            let b2 = d.get(at + 1).copied().unwrap_or(0);
            ((((b & 0x1F) as u16) << 6) | (b2 & 0x3F) as u16, 2)
        } else {
            let b2 = d.get(at + 1).copied().unwrap_or(0);
            let b3 = d.get(at + 2).copied().unwrap_or(0);
            ((((b & 0x0F) as u16) << 12) | (((b2 & 0x3F) as u16) << 6) | (b3 & 0x3F) as u16, 3)
        };
        units.push(c);
        at += n;
    }
    String::from_utf16_lossy(&units)
}

impl Dex {
    pub fn parse(data: Vec<u8>) -> Result<Dex> {
        if data.len() < 0x70 || &data[..4] != b"dex\n" {
            return Err(Error("not a DEX file"));
        }
        let d = &data;
        let table = |o: usize| -> Result<(usize, usize)> { Ok((u32_at(d, o)? as usize, u32_at(d, o + 4)? as usize)) };

        let (n, off) = table(0x38)?;
        let mut strings = Vec::with_capacity(n);
        for i in 0..n {
            let mut at = u32_at(d, off + i * 4)? as usize;
            uleb(d, &mut at)?; // length in UTF-16 units
            strings.push(Rc::from(mutf8(d, at)));
        }
        let s = |i: u32| -> Result<Rc<str>> { strings.get(i as usize).cloned().ok_or(Error("bad string index")) };

        let (n, off) = table(0x40)?;
        let mut types = Vec::with_capacity(n);
        for i in 0..n {
            types.push(s(u32_at(d, off + i * 4)?)?);
        }
        let t = |i: u32| -> Result<Rc<str>> { types.get(i as usize).cloned().ok_or(Error("bad type index")) };

        let (n, off) = table(0x48)?;
        let mut protos = Vec::with_capacity(n);
        for i in 0..n {
            let o = off + i * 12;
            let shorty = s(u32_at(d, o)?)?;
            let ret = t(u32_at(d, o + 4)?)?;
            let params = u32_at(d, o + 8)? as usize;
            let mut sig = String::from("(");
            if params != 0 {
                let count = u32_at(d, params)? as usize;
                for k in 0..count {
                    sig.push_str(&t(u16_at(d, params + 4 + k * 2)? as u32)?);
                }
            }
            sig.push(')');
            sig.push_str(&ret);
            protos.push(Proto { shorty, sig: Rc::from(sig) });
        }

        let (n, off) = table(0x50)?;
        let mut fields = Vec::with_capacity(n);
        for i in 0..n {
            let o = off + i * 8;
            fields.push(FieldId { class: u16_at(d, o)? as u32, ty: u16_at(d, o + 2)? as u32, name: u32_at(d, o + 4)? });
        }

        let (n, off) = table(0x58)?;
        let mut methods = Vec::with_capacity(n);
        for i in 0..n {
            let o = off + i * 8;
            methods.push(MethodId { class: u16_at(d, o)? as u32, proto: u16_at(d, o + 2)? as u32, name: u32_at(d, o + 4)? });
        }

        let (n, off) = table(0x60)?;
        let mut classes = Vec::with_capacity(n);
        for i in 0..n {
            let o = off + i * 32;
            let interfaces_off = u32_at(d, o + 12)? as usize;
            let mut interfaces = Vec::new();
            if interfaces_off != 0 {
                for k in 0..u32_at(d, interfaces_off)? as usize {
                    interfaces.push(u16_at(d, interfaces_off + 4 + k * 2)? as u32);
                }
            }
            let mut c = ClassDef {
                class: u32_at(d, o)?,
                access: u32_at(d, o + 4)?,
                superclass: u32_at(d, o + 8)?,
                interfaces,
                static_fields: Vec::new(),
                instance_fields: Vec::new(),
                direct: Vec::new(),
                virtual_: Vec::new(),
                static_values_off: u32_at(d, o + 28)?,
            };
            let data_off = u32_at(d, o + 24)? as usize;
            if data_off != 0 {
                let mut at = data_off;
                let sizes = [uleb(d, &mut at)?, uleb(d, &mut at)?, uleb(d, &mut at)?, uleb(d, &mut at)?];
                for (list, n) in [(&mut c.static_fields, sizes[0]), (&mut c.instance_fields, sizes[1])] {
                    let mut idx = 0;
                    for _ in 0..n {
                        idx += uleb(d, &mut at)?;
                        uleb(d, &mut at)?; // access flags
                        list.push(idx);
                    }
                }
                for (list, n) in [(&mut c.direct, sizes[2]), (&mut c.virtual_, sizes[3])] {
                    let mut idx = 0;
                    for _ in 0..n {
                        idx += uleb(d, &mut at)?;
                        let access = uleb(d, &mut at)?;
                        let code_off = uleb(d, &mut at)?;
                        list.push(Method { idx, access, code_off });
                    }
                }
            }
            classes.push(c);
        }
        Ok(Dex { data, strings, types, protos, fields, methods, classes })
    }

    /// The code of a method (at `off`, from its class data).
    pub fn code(&self, off: u32) -> Result<Code> {
        let d = &self.data;
        let o = off as usize;
        let registers = u16_at(d, o)?;
        let ins = u16_at(d, o + 2)?;
        let tries_size = u16_at(d, o + 6)? as usize;
        let n = u32_at(d, o + 12)? as usize;
        let mut insns = Vec::with_capacity(n);
        for i in 0..n {
            insns.push(u16_at(d, o + 16 + i * 2)?);
        }
        let mut tries = Vec::new();
        if tries_size > 0 {
            let mut t = o + 16 + n * 2;
            if n % 2 == 1 {
                t += 2;
            }
            let handlers_base = t + tries_size * 8;
            for k in 0..tries_size {
                let start = u32_at(d, t + k * 8)?;
                let count = u16_at(d, t + k * 8 + 4)? as u32;
                let hoff = u16_at(d, t + k * 8 + 6)? as usize;
                let mut at = handlers_base + hoff;
                let size = sleb(d, &mut at)?;
                let mut handlers = Vec::new();
                for _ in 0..size.unsigned_abs() {
                    let ty = uleb(d, &mut at)?;
                    let addr = uleb(d, &mut at)?;
                    handlers.push((ty, addr));
                }
                if size <= 0 {
                    handlers.push((NO_INDEX, uleb(d, &mut at)?));
                }
                tries.push(Try { start, end: start + count, handlers });
            }
        }
        Ok(Code { registers, ins, insns, tries })
    }

    /// The static values of a class, in the order of its static fields.
    pub fn static_values(&self, off: u32) -> Vec<Const> {
        let mut out = Vec::new();
        if off == 0 {
            return out;
        }
        let d = &self.data;
        let mut at = off as usize;
        let Ok(n) = uleb(d, &mut at) else { return out };
        for _ in 0..n {
            match encoded_value(d, &mut at) {
                Some(v) => out.push(v),
                None => break,
            }
        }
        out
    }
}

fn encoded_value(d: &[u8], at: &mut usize) -> Option<Const> {
    let head = *d.get(*at)?;
    *at += 1;
    let (kind, arg) = (head & 0x1F, (head >> 5) as usize);
    let n = arg + 1;
    let raw = |at: &mut usize, n: usize| -> Option<u64> {
        let b = d.get(*at..*at + n)?;
        *at += n;
        Some(b.iter().rev().fold(0u64, |v, &x| v << 8 | x as u64))
    };
    let signed = |v: u64, n: usize| -> i64 { ((v << (64 - 8 * n)) as i64) >> (64 - 8 * n) };
    Some(match kind {
        0x00 | 0x02 | 0x04 => Const::Int(signed(raw(at, n)?, n) as i32),
        0x03 => Const::Int(raw(at, n)? as i32),
        0x06 => Const::Long(signed(raw(at, n)?, n)),
        // Floating point values are stored with their low bytes left out.
        0x10 => Const::Float(f32::from_bits((raw(at, n)? << (32 - 8 * n)) as u32)),
        0x11 => Const::Double(f64::from_bits(raw(at, n)? << (64 - 8 * n))),
        0x17 => Const::String(raw(at, n)? as u32),
        0x18 => Const::Type(raw(at, n)? as u32),
        0x19..=0x1B => {
            raw(at, n)?;
            Const::Other
        }
        0x1C => {
            let mut a = *at;
            let count = uleb(d, &mut a).ok()?;
            for _ in 0..count {
                encoded_value(d, &mut a)?;
            }
            *at = a;
            Const::Other
        }
        0x1D => {
            let mut a = *at;
            uleb(d, &mut a).ok()?;
            let count = uleb(d, &mut a).ok()?;
            for _ in 0..count {
                uleb(d, &mut a).ok()?;
                encoded_value(d, &mut a)?;
            }
            *at = a;
            Const::Other
        }
        0x1E => Const::Null,
        0x1F => Const::Bool(arg != 0),
        _ => return None,
    })
}
