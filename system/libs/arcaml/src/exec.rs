//! Running AML: the term lists of tables and methods, and every operator
//! (ACPI 6.5, chapter 20: the AML grammar; 19.6: what each one does).

use crate::name::{is_lead, NameString, Seg};
use crate::namespace::{Builtin, BufferField, FieldKind, Method, Object, Region};
use crate::value::{self, Holder, Reference, Value};
use crate::{space, Error, Handler, Interpreter, NodeId};
use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cmp::Ordering;

pub(crate) type Res<T> = Result<T, Error>;

/// Methods calling methods deeper than this are a runaway recursion.
const MAX_DEPTH: usize = 32;
/// A While loop running longer than this (in 100 ns units: 10 s) is stuck.
const LOOP_TIME: u64 = 100_000_000;
const LOOP_ROUNDS: u64 = 50_000_000;
/// The largest buffer or package AML may make.
const MAX_SIZE: usize = 1 << 22;

pub(crate) mod op {
    pub const ZERO: u8 = 0x00;
    pub const ONE: u8 = 0x01;
    pub const ALIAS: u8 = 0x06;
    pub const NAME: u8 = 0x08;
    pub const BYTE_PREFIX: u8 = 0x0A;
    pub const WORD_PREFIX: u8 = 0x0B;
    pub const DWORD_PREFIX: u8 = 0x0C;
    pub const STRING_PREFIX: u8 = 0x0D;
    pub const QWORD_PREFIX: u8 = 0x0E;
    pub const SCOPE: u8 = 0x10;
    pub const BUFFER: u8 = 0x11;
    pub const PACKAGE: u8 = 0x12;
    pub const VAR_PACKAGE: u8 = 0x13;
    pub const METHOD: u8 = 0x14;
    pub const EXTERNAL: u8 = 0x15;
    pub const EXT: u8 = 0x5B;
    pub const LOCAL0: u8 = 0x60;
    pub const LOCAL7: u8 = 0x67;
    pub const ARG0: u8 = 0x68;
    pub const ARG6: u8 = 0x6E;
    pub const STORE: u8 = 0x70;
    pub const REF_OF: u8 = 0x71;
    pub const ADD: u8 = 0x72;
    pub const CONCAT: u8 = 0x73;
    pub const SUBTRACT: u8 = 0x74;
    pub const INCREMENT: u8 = 0x75;
    pub const DECREMENT: u8 = 0x76;
    pub const MULTIPLY: u8 = 0x77;
    pub const DIVIDE: u8 = 0x78;
    pub const SHIFT_LEFT: u8 = 0x79;
    pub const SHIFT_RIGHT: u8 = 0x7A;
    pub const AND: u8 = 0x7B;
    pub const NAND: u8 = 0x7C;
    pub const OR: u8 = 0x7D;
    pub const NOR: u8 = 0x7E;
    pub const XOR: u8 = 0x7F;
    pub const NOT: u8 = 0x80;
    pub const FIND_SET_LEFT: u8 = 0x81;
    pub const FIND_SET_RIGHT: u8 = 0x82;
    pub const DEREF_OF: u8 = 0x83;
    pub const CONCAT_RES: u8 = 0x84;
    pub const MOD: u8 = 0x85;
    pub const NOTIFY: u8 = 0x86;
    pub const SIZE_OF: u8 = 0x87;
    pub const INDEX: u8 = 0x88;
    pub const MATCH: u8 = 0x89;
    pub const CREATE_DWORD: u8 = 0x8A;
    pub const CREATE_WORD: u8 = 0x8B;
    pub const CREATE_BYTE: u8 = 0x8C;
    pub const CREATE_BIT: u8 = 0x8D;
    pub const OBJECT_TYPE: u8 = 0x8E;
    pub const CREATE_QWORD: u8 = 0x8F;
    pub const LAND: u8 = 0x90;
    pub const LOR: u8 = 0x91;
    pub const LNOT: u8 = 0x92;
    pub const LEQUAL: u8 = 0x93;
    pub const LGREATER: u8 = 0x94;
    pub const LLESS: u8 = 0x95;
    pub const TO_BUFFER: u8 = 0x96;
    pub const TO_DEC_STRING: u8 = 0x97;
    pub const TO_HEX_STRING: u8 = 0x98;
    pub const TO_INTEGER: u8 = 0x99;
    pub const TO_STRING: u8 = 0x9C;
    pub const COPY_OBJECT: u8 = 0x9D;
    pub const MID: u8 = 0x9E;
    pub const CONTINUE: u8 = 0x9F;
    pub const IF: u8 = 0xA0;
    pub const ELSE: u8 = 0xA1;
    pub const WHILE: u8 = 0xA2;
    pub const NOOP: u8 = 0xA3;
    pub const RETURN: u8 = 0xA4;
    pub const BREAK: u8 = 0xA5;
    pub const BREAKPOINT: u8 = 0xCC;
    pub const ONES: u8 = 0xFF;
}

/// Opcodes after the 0x5B prefix.
pub(crate) mod ext {
    pub const MUTEX: u8 = 0x01;
    pub const EVENT: u8 = 0x02;
    pub const COND_REF_OF: u8 = 0x12;
    pub const CREATE_FIELD: u8 = 0x13;
    pub const LOAD_TABLE: u8 = 0x1F;
    pub const LOAD: u8 = 0x20;
    pub const STALL: u8 = 0x21;
    pub const SLEEP: u8 = 0x22;
    pub const ACQUIRE: u8 = 0x23;
    pub const SIGNAL: u8 = 0x24;
    pub const WAIT: u8 = 0x25;
    pub const RESET: u8 = 0x26;
    pub const RELEASE: u8 = 0x27;
    pub const FROM_BCD: u8 = 0x28;
    pub const TO_BCD: u8 = 0x29;
    pub const UNLOAD: u8 = 0x2A;
    pub const REVISION: u8 = 0x30;
    pub const DEBUG: u8 = 0x31;
    pub const FATAL: u8 = 0x32;
    pub const TIMER: u8 = 0x33;
    pub const OP_REGION: u8 = 0x80;
    pub const FIELD: u8 = 0x81;
    pub const DEVICE: u8 = 0x82;
    pub const PROCESSOR: u8 = 0x83;
    pub const POWER_RES: u8 = 0x84;
    pub const THERMAL: u8 = 0x85;
    pub const INDEX_FIELD: u8 = 0x86;
    pub const BANK_FIELD: u8 = 0x87;
    pub const DATA_REGION: u8 = 0x88;
}

/// How a term list ended.
pub(crate) enum Flow {
    Normal,
    Break,
    Continue,
    Return(Value),
}

/// A running method.
pub(crate) struct Frame {
    pub locals: [Value; 8],
    pub args: [Value; 7],
    /// The named package or buffer each argument came from: Index and
    /// CreateField on the argument change that object, as firmware expects.
    pub origin: [Option<NodeId>; 7],
    /// Names the method made, removed when it returns.
    pub created: Vec<NodeId>,
}

/// Where Store and the other operators put their result.
pub(crate) enum Target {
    None,
    Debug,
    Local(u8),
    Arg(u8),
    Node(NodeId),
    Ref(Reference),
    /// A name that does not exist (fine for CondRefOf).
    Missing(NameString),
}

pub(crate) fn byte(c: &[u8], pos: &mut usize) -> Res<u8> {
    let b = *c.get(*pos).ok_or(Error::Truncated)?;
    *pos += 1;
    Ok(b)
}

/// A little-endian number of `n` bytes.
pub(crate) fn le(c: &[u8], pos: &mut usize, n: usize) -> Res<u64> {
    let s = c.get(*pos..*pos + n).ok_or(Error::Truncated)?;
    *pos += n;
    Ok(s.iter().rev().fold(0, |a, &b| a << 8 | b as u64))
}

/// PkgLength: one to four bytes.
pub(crate) fn pkg_length(c: &[u8], pos: &mut usize) -> Res<u64> {
    let lead = byte(c, pos)?;
    let extra = (lead >> 6) as usize;
    if extra == 0 {
        return Ok((lead & 0x3F) as u64);
    }
    let mut len = (lead & 0x0F) as u64;
    for i in 0..extra {
        len |= (byte(c, pos)? as u64) << (4 + 8 * i);
    }
    Ok(len)
}

/// Where something that starts with a PkgLength at `pos` ends.
pub(crate) fn pkg_end(c: &[u8], pos: &mut usize) -> Res<usize> {
    let start = *pos;
    let end = start + pkg_length(c, pos)? as usize;
    if end < *pos || end > c.len() {
        return Err(Error::Truncated);
    }
    Ok(end)
}

/// The end of the term at `start` if its length is written in it.
fn skip_term(c: &[u8], start: usize) -> Option<usize> {
    let mut p = start + 1;
    match *c.get(start)? {
        op::SCOPE | op::BUFFER | op::PACKAGE | op::VAR_PACKAGE | op::METHOD | op::IF | op::ELSE | op::WHILE => {}
        op::EXT => match *c.get(p)? {
            ext::FIELD | ext::DEVICE | ext::PROCESSOR | ext::POWER_RES | ext::THERMAL | ext::INDEX_FIELD | ext::BANK_FIELD => p += 1,
            _ => return None,
        },
        _ => return None,
    }
    pkg_end(c, &mut p).ok()
}

impl Frame {
    fn new() -> Frame {
        Frame { locals: Default::default(), args: Default::default(), origin: [None; 7], created: Vec::new() }
    }
}

impl<H: Handler> Interpreter<H> {
    pub(crate) fn ones(&self) -> u64 {
        if self.wide {
            u64::MAX
        } else {
            0xFFFF_FFFF
        }
    }

    pub(crate) fn mask(&self, v: u64) -> u64 {
        v & self.ones()
    }

    fn truth(&self, b: bool) -> Value {
        Value::Integer(if b { self.ones() } else { 0 })
    }

    pub(crate) fn object(&self, n: NodeId) -> Res<&Object> {
        self.ns.get(n).map(|x| &x.object).ok_or(Error::Dead)
    }

    fn frame(&self) -> Res<&Frame> {
        self.frames.last().ok_or(Error::Type("locals and arguments outside a method"))
    }

    fn frame_mut(&mut self) -> Res<&mut Frame> {
        self.frames.last_mut().ok_or(Error::Type("locals and arguments outside a method"))
    }

    fn depth(&self) -> u32 {
        self.frames.len().wrapping_sub(1) as u32
    }

    pub(crate) fn unalias(&self, mut n: NodeId) -> NodeId {
        for _ in 0..16 {
            match self.ns.get(n).map(|x| &x.object) {
                Some(Object::Alias(t)) => n = *t,
                _ => break,
            }
        }
        n
    }

    fn up_from(&self, scope: NodeId, levels: u8) -> NodeId {
        let mut s = scope;
        for _ in 0..levels {
            s = self.ns.get(s).map(|n| n.parent).unwrap_or(NodeId::ROOT);
        }
        s
    }

    /// Finds a name from a scope, following the search rules for plain
    /// one-segment names.
    pub(crate) fn lookup(&self, scope: NodeId, name: &NameString) -> Option<NodeId> {
        let base = if name.root { NodeId::ROOT } else { self.up_from(scope, name.up) };
        if name.searchable() {
            let seg = name.segs[0];
            let mut s = base;
            loop {
                if let Some(n) = self.ns.child(s, seg) {
                    return Some(self.unalias(n));
                }
                if s == NodeId::ROOT {
                    return None;
                }
                s = self.ns.get(s)?.parent;
            }
        }
        let mut at = base;
        for seg in &name.segs {
            at = self.unalias(self.ns.child(at, *seg)?);
        }
        Some(at)
    }

    fn find_or_fail(&self, scope: NodeId, name: &NameString) -> Res<NodeId> {
        self.lookup(scope, name).ok_or_else(|| Error::NotFound(name.to_string()))
    }

    /// Makes a named object.
    pub(crate) fn create(&mut self, scope: NodeId, name: &NameString, object: Object) -> Res<NodeId> {
        let (&last, path) = name.segs.split_last().ok_or(Error::BadName(0))?;
        let mut parent = if name.root { NodeId::ROOT } else { self.up_from(scope, name.up) };
        for seg in path {
            parent = self.unalias(self.ns.child(parent, *seg).ok_or_else(|| Error::NotFound(name.to_string()))?);
        }
        if let Some(old) = self.ns.child(parent, last) {
            let in_method = !self.frames.is_empty();
            let node = self.ns.get_mut(old).ok_or(Error::Dead)?;
            let reopen = matches!(
                (&node.object, &object),
                (Object::Device, Object::Device)
                    | (Object::ThermalZone, Object::ThermalZone)
                    | (Object::Processor, Object::Processor)
                    | (Object::PowerResource, Object::PowerResource)
            );
            if matches!(node.object, Object::Scope) || in_method {
                // A placeholder (Scope, External), or a method that runs
                // again while its earlier names are still there.
                node.object = object;
            } else if !reopen {
                return Err(Error::Exists(self.path(old)));
            }
            return Ok(old);
        }
        let id = self.ns.add(parent, last, object);
        if let Some(f) = self.frames.last_mut() {
            f.created.push(id);
        }
        Ok(id)
    }

    // ---- term lists and statements

    pub(crate) fn term_list(&mut self, c: &[u8], mut pos: usize, end: usize, scope: NodeId) -> Res<Flow> {
        while pos < end {
            let start = pos;
            match self.term(c, &mut pos, end, scope) {
                Ok(Flow::Normal) => {}
                Ok(flow) => return Ok(flow),
                Err(e) if self.frames.is_empty() => {
                    // Loading a table: report it, and go on after this term
                    // if its length is known.
                    self.load_errors += 1;
                    let text = format!("{} (at {start:#x}): {e}", self.path(scope));
                    self.handler.log(&text);
                    match skip_term(c, start) {
                        Some(next) if next > start && next <= end => pos = next,
                        _ => return Ok(Flow::Normal),
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Ok(Flow::Normal)
    }

    fn term(&mut self, c: &[u8], pos: &mut usize, lim: usize, scope: NodeId) -> Res<Flow> {
        let b = *c.get(*pos).ok_or(Error::Truncated)?;
        match b {
            op::ALIAS
            | op::NAME
            | op::SCOPE
            | op::METHOD
            | op::EXTERNAL
            | op::CREATE_DWORD
            | op::CREATE_WORD
            | op::CREATE_BYTE
            | op::CREATE_BIT
            | op::CREATE_QWORD => {
                *pos += 1;
                self.define(b, c, pos, scope)?;
            }
            op::IF => return self.if_else(c, pos, lim, scope),
            op::ELSE => {
                // An Else whose If was skipped while loading.
                *pos += 1;
                *pos = pkg_end(c, pos)?;
            }
            op::WHILE => return self.while_loop(c, pos, scope),
            op::RETURN => {
                *pos += 1;
                let v = if *pos < lim { self.expr(c, pos, scope)? } else { Value::Uninitialized };
                return Ok(Flow::Return(v));
            }
            op::BREAK => {
                *pos += 1;
                return Ok(Flow::Break);
            }
            op::CONTINUE => {
                *pos += 1;
                return Ok(Flow::Continue);
            }
            op::NOOP | op::BREAKPOINT => *pos += 1,
            op::NOTIFY => {
                *pos += 1;
                self.notify(c, pos, scope)?;
            }
            op::EXT => {
                let e = *c.get(*pos + 1).ok_or(Error::Truncated)?;
                match e {
                    ext::MUTEX
                    | ext::EVENT
                    | ext::CREATE_FIELD
                    | ext::OP_REGION
                    | ext::FIELD
                    | ext::DEVICE
                    | ext::PROCESSOR
                    | ext::POWER_RES
                    | ext::THERMAL
                    | ext::INDEX_FIELD
                    | ext::BANK_FIELD
                    | ext::DATA_REGION => {
                        *pos += 2;
                        self.define_ext(e, c, pos, scope)?;
                    }
                    ext::SLEEP | ext::STALL | ext::RELEASE | ext::RESET | ext::SIGNAL | ext::FATAL | ext::UNLOAD => {
                        *pos += 2;
                        self.statement_ext(e, c, pos, scope)?;
                    }
                    _ => {
                        self.expr(c, pos, scope)?;
                    }
                }
            }
            _ => {
                self.expr(c, pos, scope)?;
            }
        }
        Ok(Flow::Normal)
    }

    fn define(&mut self, b: u8, c: &[u8], pos: &mut usize, scope: NodeId) -> Res<()> {
        match b {
            op::ALIAS => {
                let from = NameString::parse(c, pos)?;
                let to = NameString::parse(c, pos)?;
                let target = self.find_or_fail(scope, &from)?;
                self.create(scope, &to, Object::Alias(target))?;
            }
            op::NAME => {
                let name = NameString::parse(c, pos)?;
                let v = self.expr(c, pos, scope)?;
                self.create(scope, &name, Object::Data(v))?;
            }
            op::SCOPE => {
                let end = pkg_end(c, pos)?;
                let name = NameString::parse(c, pos)?;
                let node = self.find_or_fail(scope, &name)?;
                let start = *pos;
                *pos = end;
                self.term_list(c, start, end, node)?;
            }
            op::METHOD => {
                let end = pkg_end(c, pos)?;
                let name = NameString::parse(c, pos)?;
                let flags = byte(c, pos)?;
                let m = Method { table: self.table_now, start: *pos as u32, end: end as u32, args: flags & 7 };
                *pos = end;
                self.create(scope, &name, Object::Method(m))?;
            }
            op::EXTERNAL => {
                NameString::parse(c, pos)?;
                byte(c, pos)?;
                byte(c, pos)?;
            }
            _ => {
                let holder = self.holder(c, pos, scope)?;
                let at = self.int(c, pos, scope)?;
                let name = NameString::parse(c, pos)?;
                let (bit_off, bit_len) = match b {
                    op::CREATE_BIT => (at, 1),
                    op::CREATE_BYTE => (at * 8, 8),
                    op::CREATE_WORD => (at * 8, 16),
                    op::CREATE_DWORD => (at * 8, 32),
                    _ => (at * 8, 64),
                };
                self.create(scope, &name, Object::BufferField(BufferField { holder, bit_off, bit_len, any_size: false }))?;
            }
        }
        Ok(())
    }

    fn define_ext(&mut self, e: u8, c: &[u8], pos: &mut usize, scope: NodeId) -> Res<()> {
        match e {
            ext::MUTEX => {
                let name = NameString::parse(c, pos)?;
                byte(c, pos)?;
                self.create(scope, &name, Object::Mutex)?;
            }
            ext::EVENT => {
                let name = NameString::parse(c, pos)?;
                self.create(scope, &name, Object::Event(0))?;
            }
            ext::CREATE_FIELD => {
                let holder = self.holder(c, pos, scope)?;
                let bit_off = self.int(c, pos, scope)?;
                let bit_len = self.int(c, pos, scope)?;
                let name = NameString::parse(c, pos)?;
                self.create(scope, &name, Object::BufferField(BufferField { holder, bit_off, bit_len, any_size: true }))?;
            }
            ext::OP_REGION => {
                let name = NameString::parse(c, pos)?;
                let space = byte(c, pos)?;
                let base = self.int(c, pos, scope)?;
                let len = self.int(c, pos, scope)?;
                self.create(scope, &name, Object::Region(Region { space, base, len, pci: None }))?;
            }
            ext::DATA_REGION => {
                let name = NameString::parse(c, pos)?;
                let sig = self.text(c, pos, scope)?;
                let oem = self.text(c, pos, scope)?;
                let oem_table = self.text(c, pos, scope)?;
                let (base, len) = self.handler.table_address(&sig, &oem, &oem_table).ok_or(Error::NotFound(sig))?;
                self.create(scope, &name, Object::Region(Region { space: space::MEMORY, base, len, pci: None }))?;
            }
            ext::FIELD => {
                let end = pkg_end(c, pos)?;
                let rn = NameString::parse(c, pos)?;
                let region = self.find_or_fail(scope, &rn)?;
                let flags = byte(c, pos)?;
                self.field_list(c, pos, end, scope, FieldKind::Region(region), flags)?;
                *pos = end;
            }
            ext::INDEX_FIELD => {
                let end = pkg_end(c, pos)?;
                let i = NameString::parse(c, pos)?;
                let d = NameString::parse(c, pos)?;
                let index = self.find_or_fail(scope, &i)?;
                let data = self.find_or_fail(scope, &d)?;
                let flags = byte(c, pos)?;
                self.field_list(c, pos, end, scope, FieldKind::Index { index, data }, flags)?;
                *pos = end;
            }
            ext::BANK_FIELD => {
                let end = pkg_end(c, pos)?;
                let rn = NameString::parse(c, pos)?;
                let bn = NameString::parse(c, pos)?;
                let region = self.find_or_fail(scope, &rn)?;
                let bank = self.find_or_fail(scope, &bn)?;
                let value = self.int(c, pos, scope)?;
                let flags = byte(c, pos)?;
                self.field_list(c, pos, end, scope, FieldKind::Bank { region, bank, value }, flags)?;
                *pos = end;
            }
            _ => {
                // Device, Processor, PowerResource, ThermalZone: a block.
                let end = pkg_end(c, pos)?;
                let name = NameString::parse(c, pos)?;
                let object = match e {
                    ext::DEVICE => Object::Device,
                    ext::THERMAL => Object::ThermalZone,
                    ext::PROCESSOR => {
                        // ProcID, PblkAddr, PblkLen
                        le(c, pos, 6)?;
                        Object::Processor
                    }
                    _ => {
                        // SystemLevel, ResourceOrder
                        le(c, pos, 3)?;
                        Object::PowerResource
                    }
                };
                let node = self.create(scope, &name, object)?;
                let start = *pos;
                *pos = end;
                self.term_list(c, start, end, node)?;
            }
        }
        Ok(())
    }

    fn field_list(&mut self, c: &[u8], pos: &mut usize, end: usize, scope: NodeId, kind: FieldKind, flags: u8) -> Res<()> {
        let mut access = flags & 0x0F;
        let update = (flags >> 5) & 3;
        let mut bit = 0u64;
        while *pos < end {
            match c[*pos] {
                0x00 => {
                    // ReservedField, also Offset()
                    *pos += 1;
                    bit += pkg_length(c, pos)?;
                }
                0x01 => {
                    // AccessField: AccessType, AccessAttrib
                    *pos += 1;
                    access = byte(c, pos)? & 0x0F;
                    byte(c, pos)?;
                }
                0x02 => {
                    // ConnectField: a name or a buffer (GPIO, serial bus)
                    *pos += 1;
                    if c.get(*pos) == Some(&op::BUFFER) {
                        *pos += 1;
                        *pos = pkg_end(c, pos)?;
                    } else {
                        NameString::parse(c, pos)?;
                    }
                }
                0x03 => {
                    // ExtendedAccessField: AccessType, attribute, length
                    *pos += 1;
                    access = byte(c, pos)? & 0x0F;
                    le(c, pos, 2)?;
                }
                _ => {
                    let seg: Seg = c.get(*pos..*pos + 4).ok_or(Error::Truncated)?.try_into().unwrap();
                    *pos += 4;
                    let bit_len = pkg_length(c, pos)?;
                    let f = crate::namespace::Field { kind: kind.clone(), bit_off: bit, bit_len, access, update };
                    if let Err(e) = self.create(scope, &NameString::single(seg), Object::Field(f)) {
                        if !self.frames.is_empty() {
                            return Err(e);
                        }
                        self.load_errors += 1;
                        self.handler.log(&format!("field {}: {e}", crate::seg_str(&seg)));
                    }
                    bit += bit_len;
                }
            }
        }
        Ok(())
    }

    fn if_else(&mut self, c: &[u8], pos: &mut usize, lim: usize, scope: NodeId) -> Res<Flow> {
        *pos += 1;
        let end = pkg_end(c, pos)?;
        let pred = self.int(c, pos, scope)?;
        let body = *pos;
        *pos = end;
        let mut other = None;
        if end < lim && c.get(end) == Some(&op::ELSE) {
            let mut p = end + 1;
            let e = pkg_end(c, &mut p)?;
            other = Some((p, e));
            *pos = e;
        }
        if pred != 0 {
            self.term_list(c, body, end, scope)
        } else if let Some((s, e)) = other {
            self.term_list(c, s, e, scope)
        } else {
            Ok(Flow::Normal)
        }
    }

    fn while_loop(&mut self, c: &[u8], pos: &mut usize, scope: NodeId) -> Res<Flow> {
        *pos += 1;
        let end = pkg_end(c, pos)?;
        let pred_at = *pos;
        *pos = end;
        let started = self.handler.timer();
        let mut rounds = 0u64;
        loop {
            let mut p = pred_at;
            if self.int(c, &mut p, scope)? == 0 {
                break;
            }
            match self.term_list(c, p, end, scope)? {
                Flow::Break => break,
                Flow::Return(v) => return Ok(Flow::Return(v)),
                Flow::Normal | Flow::Continue => {}
            }
            rounds += 1;
            if rounds >= LOOP_ROUNDS || (rounds.is_multiple_of(256) &&self.handler.timer().wrapping_sub(started) > LOOP_TIME) {
                return Err(Error::Timeout);
            }
        }
        Ok(Flow::Normal)
    }

    /// The node a Notify, Signal or Reset is about.
    fn node_of(&mut self, t: &Target) -> Res<NodeId> {
        match t {
            Target::Node(n) => Ok(*n),
            Target::Missing(name) => Err(Error::NotFound(name.to_string())),
            Target::Ref(Reference::Node(n)) => Ok(*n),
            other => match self.read_target(other)? {
                Value::Node(n) => Ok(n),
                Value::Ref(r) => match *r {
                    Reference::Node(n) => Ok(n),
                    _ => Err(Error::Type("not an object")),
                },
                _ => Err(Error::Type("not an object")),
            },
        }
    }

    fn notify(&mut self, c: &[u8], pos: &mut usize, scope: NodeId) -> Res<()> {
        let t = self.target(c, pos, scope)?;
        let v = self.int(c, pos, scope)?;
        let n = self.node_of(&t)?;
        let path = self.path(n);
        self.handler.notify(n, &path, v);
        Ok(())
    }

    fn statement_ext(&mut self, e: u8, c: &[u8], pos: &mut usize, scope: NodeId) -> Res<()> {
        match e {
            ext::SLEEP => {
                let ms = self.int(c, pos, scope)?;
                self.handler.sleep(ms.min(10_000));
            }
            ext::STALL => {
                let us = self.int(c, pos, scope)?;
                self.handler.stall(us.min(1000));
            }
            ext::RELEASE | ext::UNLOAD => {
                self.target(c, pos, scope)?;
            }
            ext::RESET | ext::SIGNAL => {
                let t = self.target(c, pos, scope)?;
                let n = self.node_of(&t)?;
                if let Some(Object::Event(count)) = self.ns.get_mut(n).map(|x| &mut x.object) {
                    *count = if e == ext::RESET { 0 } else { count.saturating_add(1) };
                }
            }
            _ => {
                // Fatal: type, code, argument
                let kind = byte(c, pos)?;
                let code = le(c, pos, 4)? as u32;
                let arg = self.int(c, pos, scope)?;
                self.handler.fatal(kind, code, arg);
                return Err(Error::Fatal);
            }
        }
        Ok(())
    }

    // ---- expressions

    /// Evaluates a TermArg. Names give their value (fields are read),
    /// methods are called; references stay references.
    pub(crate) fn expr(&mut self, c: &[u8], pos: &mut usize, scope: NodeId) -> Res<Value> {
        let at = *pos;
        let b = byte(c, pos)?;
        Ok(match b {
            op::ZERO => Value::Integer(0),
            op::ONE => Value::Integer(1),
            op::ONES => Value::Integer(self.ones()),
            op::BYTE_PREFIX => Value::Integer(le(c, pos, 1)?),
            op::WORD_PREFIX => Value::Integer(le(c, pos, 2)?),
            op::DWORD_PREFIX => Value::Integer(le(c, pos, 4)?),
            op::QWORD_PREFIX => Value::Integer(le(c, pos, 8)?),
            op::STRING_PREFIX => {
                let len = c.get(*pos..).and_then(|r| r.iter().position(|&x| x == 0)).ok_or(Error::Truncated)?;
                let s = value::string_from_bytes(&c[*pos..*pos + len]);
                *pos += len + 1;
                Value::String(s)
            }
            op::BUFFER => self.buffer(c, pos, scope)?,
            op::PACKAGE | op::VAR_PACKAGE => self.package(c, pos, scope, b == op::VAR_PACKAGE)?,
            op::LOCAL0..=op::LOCAL7 => self.frame()?.locals[(b - op::LOCAL0) as usize].clone(),
            op::ARG0..=op::ARG6 => self.frame()?.args[(b - op::ARG0) as usize].clone(),
            op::STORE => {
                let v = self.expr(c, pos, scope)?;
                let t = self.target(c, pos, scope)?;
                self.store(v.clone(), &t)?;
                v
            }
            op::COPY_OBJECT => {
                let v = self.expr(c, pos, scope)?;
                let t = self.target(c, pos, scope)?;
                self.copy_object(v.clone(), &t)?;
                v
            }
            op::REF_OF => {
                let t = self.target(c, pos, scope)?;
                Value::Ref(Box::new(self.reference(&t)?))
            }
            op::ADD
            | op::SUBTRACT
            | op::MULTIPLY
            | op::SHIFT_LEFT
            | op::SHIFT_RIGHT
            | op::AND
            | op::NAND
            | op::OR
            | op::NOR
            | op::XOR
            | op::MOD => self.math(b, c, pos, scope)?,
            op::CONCAT => {
                let x = self.data(c, pos, scope)?;
                let y = self.data(c, pos, scope)?;
                let r = self.concat(x, y)?;
                self.put(r, c, pos, scope)?
            }
            op::CONCAT_RES => {
                let x = self.data(c, pos, scope)?;
                let y = self.data(c, pos, scope)?;
                let r = Value::Buffer(concat_res(&value::to_buffer(&x, self.wide)?, &value::to_buffer(&y, self.wide)?));
                self.put(r, c, pos, scope)?
            }
            op::INCREMENT | op::DECREMENT => {
                let t = self.target(c, pos, scope)?;
                let v = self.read_target(&t)?;
                let v = self.resolve(v)?;
                let x = value::to_integer(&v, self.wide)?;
                let r = self.mask(if b == op::INCREMENT { x.wrapping_add(1) } else { x.wrapping_sub(1) });
                self.store(Value::Integer(r), &t)?;
                Value::Integer(r)
            }
            op::DIVIDE => {
                let x = self.int(c, pos, scope)?;
                let y = self.int(c, pos, scope)?;
                let rem = self.target(c, pos, scope)?;
                let quot = self.target(c, pos, scope)?;
                if y == 0 {
                    return Err(Error::DivideByZero);
                }
                self.store(Value::Integer(x % y), &rem)?;
                self.store(Value::Integer(x / y), &quot)?;
                Value::Integer(x / y)
            }
            op::NOT => {
                let x = self.int(c, pos, scope)?;
                let r = Value::Integer(self.mask(!x));
                self.put(r, c, pos, scope)?
            }
            op::FIND_SET_LEFT | op::FIND_SET_RIGHT => {
                let x = self.int(c, pos, scope)?;
                let r = match x {
                    0 => 0,
                    _ if b == op::FIND_SET_LEFT => 64 - x.leading_zeros() as u64,
                    _ => x.trailing_zeros() as u64 + 1,
                };
                self.put(Value::Integer(r), c, pos, scope)?
            }
            op::DEREF_OF => {
                let v = self.expr(c, pos, scope)?;
                self.deref(v, scope)?
            }
            op::SIZE_OF => {
                let t = self.target(c, pos, scope)?;
                let v = self.read_target(&t)?;
                let v = self.resolve(v)?;
                Value::Integer(match &v {
                    Value::Buffer(x) => x.len(),
                    Value::String(s) => s.chars().count(),
                    Value::Package(p) => p.len(),
                    _ => return Err(Error::Type("SizeOf")),
                } as u64)
            }
            op::INDEX => self.index(c, pos, scope)?,
            op::MATCH => self.match_op(c, pos, scope)?,
            op::OBJECT_TYPE => {
                let t = self.target(c, pos, scope)?;
                Value::Integer(self.type_of(&t)?)
            }
            op::LAND | op::LOR => {
                let x = self.int(c, pos, scope)?;
                let y = self.int(c, pos, scope)?;
                self.truth(if b == op::LAND { x != 0 && y != 0 } else { x != 0 || y != 0 })
            }
            op::LNOT => {
                let x = self.int(c, pos, scope)?;
                self.truth(x == 0)
            }
            op::LEQUAL | op::LGREATER | op::LLESS => {
                let x = self.data(c, pos, scope)?;
                let y = self.data(c, pos, scope)?;
                let o = self.compare(&x, &y)?;
                self.truth(match b {
                    op::LEQUAL => o == Ordering::Equal,
                    op::LGREATER => o == Ordering::Greater,
                    _ => o == Ordering::Less,
                })
            }
            op::TO_BUFFER => {
                let v = self.data(c, pos, scope)?;
                let r = Value::Buffer(value::to_buffer(&v, self.wide)?);
                self.put(r, c, pos, scope)?
            }
            op::TO_DEC_STRING | op::TO_HEX_STRING => {
                let v = self.data(c, pos, scope)?;
                let s = if b == op::TO_HEX_STRING {
                    value::to_hex_string(&v, self.wide)?
                } else {
                    value::to_decimal_string(&v, self.wide)?
                };
                self.put(Value::String(s), c, pos, scope)?
            }
            op::TO_INTEGER => {
                let v = self.data(c, pos, scope)?;
                let r = Value::Integer(value::to_integer_explicit(&v, self.wide)?);
                self.put(r, c, pos, scope)?
            }
            op::TO_STRING => {
                let v = self.data(c, pos, scope)?;
                let len = self.int(c, pos, scope)?;
                let bytes = value::to_buffer(&v, self.wide)?;
                let end = bytes.iter().position(|&x| x == 0).unwrap_or(bytes.len()).min(len.min(usize::MAX as u64) as usize);
                let r = Value::String(value::string_from_bytes(&bytes[..end]));
                self.put(r, c, pos, scope)?
            }
            op::MID => {
                let v = self.data(c, pos, scope)?;
                let start = self.int(c, pos, scope)? as usize;
                let len = self.int(c, pos, scope)? as usize;
                let r = match &v {
                    Value::String(s) => Value::String(s.chars().skip(start).take(len).collect()),
                    Value::Buffer(x) => Value::Buffer(x.iter().skip(start).take(len).copied().collect()),
                    _ => return Err(Error::Type("Mid")),
                };
                self.put(r, c, pos, scope)?
            }
            op::EXT => self.expr_ext(c, pos, scope)?,
            _ if is_lead(b) => {
                *pos = at;
                self.name_expr(c, pos, scope)?
            }
            _ => return Err(Error::BadOpcode(b as u16, at)),
        })
    }

    fn expr_ext(&mut self, c: &[u8], pos: &mut usize, scope: NodeId) -> Res<Value> {
        let at = *pos - 1;
        let e = byte(c, pos)?;
        Ok(match e {
            ext::COND_REF_OF => {
                let t = self.target(c, pos, scope)?;
                let dest = self.target(c, pos, scope)?;
                match t {
                    Target::Missing(_) | Target::None => Value::Integer(0),
                    t => {
                        let r = self.reference(&t)?;
                        self.store(Value::Ref(Box::new(r)), &dest)?;
                        Value::Integer(self.ones())
                    }
                }
            }
            ext::ACQUIRE => {
                // One interpreter runs at a time: the mutex is always free.
                self.target(c, pos, scope)?;
                le(c, pos, 2)?;
                Value::Integer(0)
            }
            ext::WAIT => {
                let t = self.target(c, pos, scope)?;
                let ms = self.int(c, pos, scope)?;
                let n = self.node_of(&t)?;
                let signalled = match self.ns.get_mut(n).map(|x| &mut x.object) {
                    Some(Object::Event(count)) if *count > 0 => {
                        *count -= 1;
                        true
                    }
                    _ => false,
                };
                if !signalled && ms != 0 {
                    // Nothing else could signal it meanwhile.
                    self.handler.sleep(ms.min(100));
                }
                Value::Integer(if signalled { 0 } else { self.ones() })
            }
            ext::FROM_BCD | ext::TO_BCD => {
                let x = self.int(c, pos, scope)?;
                let r = if e == ext::FROM_BCD { from_bcd(x) } else { to_bcd(x) };
                self.put(Value::Integer(r), c, pos, scope)?
            }
            ext::REVISION => Value::Integer(2),
            ext::DEBUG => Value::Uninitialized,
            ext::TIMER => Value::Integer(self.handler.timer()),
            ext::LOAD => self.load_op(c, pos, scope)?,
            ext::LOAD_TABLE => self.load_table_op(c, pos, scope)?,
            ext::SLEEP | ext::STALL | ext::RELEASE | ext::RESET | ext::SIGNAL | ext::FATAL | ext::UNLOAD => {
                self.statement_ext(e, c, pos, scope)?;
                Value::Uninitialized
            }
            _ => return Err(Error::BadOpcode(0x5B00 | e as u16, at)),
        })
    }

    /// A name used as a value: a method call, or the object's value.
    fn name_expr(&mut self, c: &[u8], pos: &mut usize, scope: NodeId) -> Res<Value> {
        let name = NameString::parse(c, pos)?;
        let node = self.find_or_fail(scope, &name)?;
        let nargs = match self.object(node)? {
            Object::Method(m) => m.args as usize,
            Object::Builtin(_) => 1,
            _ => return self.value_of_node(node),
        };
        let mut args = Vec::with_capacity(nargs);
        let mut origin = [None; 7];
        for slot in origin.iter_mut().take(nargs) {
            *slot = self.origin_at(c, *pos, scope);
            args.push(self.expr(c, pos, scope)?);
        }
        self.call(node, args, origin)
    }

    /// The named package or buffer an argument names, if it is one.
    fn origin_at(&self, c: &[u8], pos: usize, scope: NodeId) -> Option<NodeId> {
        if !is_lead(*c.get(pos)?) {
            return None;
        }
        let mut p = pos;
        let name = NameString::parse(c, &mut p).ok()?;
        let n = self.lookup(scope, &name)?;
        matches!(self.object(n).ok()?, Object::Data(Value::Package(_) | Value::Buffer(_))).then_some(n)
    }

    /// Runs a method.
    pub(crate) fn call(&mut self, node: NodeId, args: Vec<Value>, origin: [Option<NodeId>; 7]) -> Res<Value> {
        let m = match self.object(node)? {
            Object::Method(m) => m.clone(),
            Object::Builtin(b) => {
                let b = *b;
                return self.builtin(b, &args);
            }
            _ => return self.value_of_node(node),
        };
        if self.frames.len() >= MAX_DEPTH {
            return Err(Error::Depth);
        }
        let code = self.tables.get(m.table as usize).ok_or(Error::Dead)?.clone();
        let mut frame = Frame::new();
        for (slot, a) in frame.args.iter_mut().zip(args) {
            *slot = a;
        }
        frame.origin = origin;
        let table_now = core::mem::replace(&mut self.table_now, m.table);
        self.frames.push(frame);
        let r = self.term_list(&code, m.start as usize, m.end as usize, node);
        let frame = self.frames.pop().expect("frame");
        self.table_now = table_now;
        for &n in frame.created.iter().rev() {
            self.ns.remove(n);
        }
        match r? {
            Flow::Return(v) => Ok(v),
            _ => Ok(Value::Uninitialized),
        }
    }

    fn builtin(&mut self, b: Builtin, args: &[Value]) -> Res<Value> {
        match b {
            Builtin::Osi => {
                let s = match args.first() {
                    Some(Value::String(s)) => s.clone(),
                    _ => return Err(Error::Type("_OSI wants a string")),
                };
                let yes = self.handler.osi(&s);
                Ok(self.truth(yes))
            }
        }
    }

    /// The value of a named object: data, a field read from the hardware,
    /// or the object itself when it is not data.
    pub(crate) fn value_of_node(&mut self, node: NodeId) -> Res<Value> {
        match self.object(node)? {
            Object::Data(v) => Ok(v.clone()),
            Object::Field(f) => {
                let f = f.clone();
                self.read_field(&f)
            }
            Object::BufferField(bf) => {
                let bf = bf.clone();
                self.read_buffer_field(&bf)
            }
            Object::Alias(t) => {
                let t = *t;
                self.value_of_node(t)
            }
            Object::Method(_) | Object::Builtin(_) => self.call(node, Vec::new(), [None; 7]),
            _ => Ok(Value::Node(node)),
        }
    }

    /// Turns names and references into the data they stand for (for
    /// operands that must be integers, strings or buffers).
    pub(crate) fn resolve(&mut self, v: Value) -> Res<Value> {
        let v = match v {
            Value::Ref(r) => self.read_ref(&r)?,
            v => v,
        };
        match v {
            Value::Node(n) => self.resolve_node(n),
            Value::Name(name, scope) => {
                let n = self.find_or_fail(scope, &name)?;
                self.resolve_node(n)
            }
            Value::Ref(r) => self.read_ref(&r),
            v => Ok(v),
        }
    }

    /// A result for the caller: names of data inside packages become
    /// their values.
    pub(crate) fn resolve_deep(&mut self, v: Value, depth: u8) -> Value {
        match v {
            Value::Package(items) if depth < 8 => {
                Value::Package(items.into_iter().map(|x| self.resolve_deep(x, depth + 1)).collect())
            }
            Value::Node(n) => match self.object(n) {
                Ok(Object::Data(_) | Object::Field(_) | Object::BufferField(_)) => self.value_of_node(n).unwrap_or(Value::Node(n)),
                _ => Value::Node(n),
            },
            Value::Name(name, scope) => match self.lookup(scope, &name) {
                Some(n) => self.resolve_deep(Value::Node(n), depth),
                None => Value::Name(name, scope),
            },
            v => v,
        }
    }

    fn resolve_node(&mut self, n: NodeId) -> Res<Value> {
        match self.object(n)? {
            Object::Data(_) | Object::Field(_) | Object::BufferField(_) | Object::Alias(_) => self.value_of_node(n),
            _ => Ok(Value::Node(n)),
        }
    }

    /// An operand that must be data.
    pub(crate) fn data(&mut self, c: &[u8], pos: &mut usize, scope: NodeId) -> Res<Value> {
        let v = self.expr(c, pos, scope)?;
        self.resolve(v)
    }

    /// An operand that must be an integer.
    pub(crate) fn int(&mut self, c: &[u8], pos: &mut usize, scope: NodeId) -> Res<u64> {
        let v = self.data(c, pos, scope)?;
        value::to_integer(&v, self.wide)
    }

    /// An operand that must be a string.
    fn text(&mut self, c: &[u8], pos: &mut usize, scope: NodeId) -> Res<String> {
        let v = self.data(c, pos, scope)?;
        value::to_string_implicit(&v, self.wide)
    }

    /// Stores a result into the target that follows, and gives it back.
    fn put(&mut self, r: Value, c: &[u8], pos: &mut usize, scope: NodeId) -> Res<Value> {
        let t = self.target(c, pos, scope)?;
        self.store(r.clone(), &t)?;
        Ok(r)
    }

    fn math(&mut self, b: u8, c: &[u8], pos: &mut usize, scope: NodeId) -> Res<Value> {
        let x = self.int(c, pos, scope)?;
        let y = self.int(c, pos, scope)?;
        let r = match b {
            op::ADD => x.wrapping_add(y),
            op::SUBTRACT => x.wrapping_sub(y),
            op::MULTIPLY => x.wrapping_mul(y),
            op::SHIFT_LEFT => x.checked_shl(y.min(64) as u32).unwrap_or(0),
            op::SHIFT_RIGHT => x.checked_shr(y.min(64) as u32).unwrap_or(0),
            op::AND => x & y,
            op::NAND => !(x & y),
            op::OR => x | y,
            op::NOR => !(x | y),
            op::XOR => x ^ y,
            _ => {
                if y == 0 {
                    return Err(Error::DivideByZero);
                }
                x % y
            }
        };
        let r = Value::Integer(self.mask(r));
        self.put(r, c, pos, scope)
    }

    fn concat(&self, x: Value, y: Value) -> Res<Value> {
        Ok(match x {
            Value::Integer(a) => {
                let mut b = value::to_buffer(&Value::Integer(a), self.wide)?;
                b.extend(value::to_buffer(&Value::Integer(value::to_integer(&y, self.wide)?), self.wide)?);
                Value::Buffer(b)
            }
            Value::String(mut s) => {
                s.push_str(&match &y {
                    Value::Package(_) => "[Package]".into(),
                    y => value::to_string_implicit(y, self.wide)?,
                });
                Value::String(s)
            }
            Value::Buffer(mut b) => {
                b.extend(value::to_buffer(&y, self.wide)?);
                Value::Buffer(b)
            }
            _ => return Err(Error::Type("Concatenate")),
        })
    }

    fn compare(&self, x: &Value, y: &Value) -> Res<Ordering> {
        Ok(match x {
            Value::Integer(a) => a.cmp(&value::to_integer(y, self.wide)?),
            Value::String(a) => {
                let b = value::to_string_implicit(y, self.wide)?;
                a.chars().map(|c| c as u32).cmp(b.chars().map(|c| c as u32))
            }
            Value::Buffer(a) => a.as_slice().cmp(value::to_buffer(y, self.wide)?.as_slice()),
            _ => return Err(Error::Type("compare")),
        })
    }

    fn buffer(&mut self, c: &[u8], pos: &mut usize, scope: NodeId) -> Res<Value> {
        let end = pkg_end(c, pos)?;
        let size = self.int(c, pos, scope)? as usize;
        if *pos > end || size > MAX_SIZE {
            return Err(Error::Range);
        }
        let init = &c[*pos..end];
        *pos = end;
        let mut b = alloc::vec![0u8; size.max(init.len())];
        b[..init.len()].copy_from_slice(init);
        Ok(Value::Buffer(b))
    }

    fn package(&mut self, c: &[u8], pos: &mut usize, scope: NodeId, var: bool) -> Res<Value> {
        let end = pkg_end(c, pos)?;
        let count = if var { self.int(c, pos, scope)? as usize } else { byte(c, pos)? as usize };
        if count > MAX_SIZE {
            return Err(Error::Range);
        }
        let mut items = Vec::with_capacity(count.min(256));
        while *pos < end {
            if is_lead(c[*pos]) {
                // A name: the value of named data, else the object itself
                // (a device in _PRT, a power resource in _PR0...).
                let name = NameString::parse(c, pos)?;
                items.push(match self.lookup(scope, &name) {
                    Some(n) => match self.object(n)? {
                        Object::Data(v) => v.clone(),
                        // Fields are read, but not while the tables load.
                        Object::Field(_) | Object::BufferField(_) if !self.frames.is_empty() => self.value_of_node(n)?,
                        _ => Value::Node(n),
                    },
                    None => Value::Name(name, scope),
                });
            } else {
                items.push(self.expr(c, pos, scope)?);
            }
        }
        *pos = end;
        // Extra elements are dropped, missing ones are uninitialized.
        items.resize(count, Value::Uninitialized);
        Ok(Value::Package(items))
    }

    /// The first operand of Index and CreateField: what holds the data.
    fn holder(&mut self, c: &[u8], pos: &mut usize, scope: NodeId) -> Res<Holder> {
        let b = *c.get(*pos).ok_or(Error::Truncated)?;
        let depth = self.depth();
        match b {
            op::LOCAL0..=op::LOCAL7 => {
                *pos += 1;
                let i = b - op::LOCAL0;
                if let Value::Ref(r) = &self.frame()?.locals[i as usize] {
                    if let Reference::Node(n) = **r {
                        return self.node_holder(n);
                    }
                }
                Ok(Holder::Local(depth, i))
            }
            op::ARG0..=op::ARG6 => {
                *pos += 1;
                let i = (b - op::ARG0) as usize;
                let f = self.frame()?;
                if let Some(n) = f.origin[i] {
                    return Ok(Holder::Node(n));
                }
                if let Value::Ref(r) = &f.args[i] {
                    if let Reference::Node(n) = **r {
                        return self.node_holder(n);
                    }
                }
                Ok(Holder::Arg(depth, i as u8))
            }
            op::DEREF_OF => {
                *pos += 1;
                match self.expr(c, pos, scope)? {
                    Value::Ref(r) => match *r {
                        Reference::Node(n) => self.node_holder(n),
                        Reference::Local(f, i) => Ok(Holder::Local(f, i)),
                        Reference::Arg(f, i) => Ok(Holder::Arg(f, i)),
                        r => Ok(Holder::Temp(Box::new(self.read_ref(&r)?))),
                    },
                    Value::String(s) => {
                        let name = NameString::parse_str(&s).ok_or(Error::BadName(*pos))?;
                        let n = self.find_or_fail(scope, &name)?;
                        self.node_holder(n)
                    }
                    other => Ok(Holder::Temp(Box::new(other))),
                }
            }
            _ if is_lead(b) => {
                let save = *pos;
                let name = NameString::parse(c, pos)?;
                let node = self.find_or_fail(scope, &name)?;
                if matches!(self.object(node)?, Object::Method(_) | Object::Builtin(_)) {
                    *pos = save;
                    let v = self.data(c, pos, scope)?;
                    return Ok(Holder::Temp(Box::new(v)));
                }
                self.node_holder(node)
            }
            _ => {
                let v = self.data(c, pos, scope)?;
                Ok(Holder::Temp(Box::new(v)))
            }
        }
    }

    fn node_holder(&mut self, n: NodeId) -> Res<Holder> {
        match self.object(n)? {
            Object::Data(_) => Ok(Holder::Node(n)),
            _ => Ok(Holder::Temp(Box::new(self.value_of_node(n)?))),
        }
    }

    pub(crate) fn holder_ref<'a>(&'a self, h: &'a Holder) -> Res<&'a Value> {
        match h {
            Holder::Node(n) => match self.object(*n)? {
                Object::Data(v) => Ok(v),
                _ => Err(Error::Type("not data")),
            },
            Holder::Local(f, i) => Ok(&self.frames.get(*f as usize).ok_or(Error::Dead)?.locals[*i as usize]),
            Holder::Arg(f, i) => Ok(&self.frames.get(*f as usize).ok_or(Error::Dead)?.args[*i as usize]),
            Holder::Temp(v) => Ok(v),
        }
    }

    pub(crate) fn with_holder<R>(&mut self, h: &Holder, f: impl FnOnce(&mut Value) -> Res<R>) -> Res<R> {
        match h {
            Holder::Node(n) => match self.ns.get_mut(*n).map(|x| &mut x.object) {
                Some(Object::Data(v)) => f(v),
                Some(_) => Err(Error::Type("not data")),
                None => Err(Error::Dead),
            },
            Holder::Local(fr, i) => f(&mut self.frames.get_mut(*fr as usize).ok_or(Error::Dead)?.locals[*i as usize]),
            Holder::Arg(fr, i) => f(&mut self.frames.get_mut(*fr as usize).ok_or(Error::Dead)?.args[*i as usize]),
            Holder::Temp(v) => f(&mut (**v).clone()),
        }
    }

    fn index(&mut self, c: &[u8], pos: &mut usize, scope: NodeId) -> Res<Value> {
        let h = self.holder(c, pos, scope)?;
        let i = self.int(c, pos, scope)?;
        let len = match self.holder_ref(&h)? {
            Value::Package(p) => p.len(),
            Value::Buffer(b) => b.len(),
            Value::String(s) => s.chars().count(),
            _ => return Err(Error::Type("Index")),
        };
        if i >= len as u64 {
            return Err(Error::Range);
        }
        let r = Value::Ref(Box::new(Reference::Index(h, i as usize)));
        self.put(r, c, pos, scope)
    }

    fn match_op(&mut self, c: &[u8], pos: &mut usize, scope: NodeId) -> Res<Value> {
        let pkg = self.data(c, pos, scope)?;
        let op1 = byte(c, pos)?;
        let o1 = self.data(c, pos, scope)?;
        let op2 = byte(c, pos)?;
        let o2 = self.data(c, pos, scope)?;
        let start = self.int(c, pos, scope)? as usize;
        let Value::Package(items) = pkg else { return Err(Error::Type("Match")) };
        for (i, e) in items.iter().enumerate().skip(start) {
            if !matches!(e, Value::Integer(_) | Value::String(_) | Value::Buffer(_)) {
                continue;
            }
            if self.test(op1, e, &o1) && self.test(op2, e, &o2) {
                return Ok(Value::Integer(i as u64));
            }
        }
        Ok(Value::Integer(self.ones()))
    }

    fn test(&self, op: u8, e: &Value, o: &Value) -> bool {
        if op == 0 {
            return true;
        }
        let Ok(ord) = self.compare(e, o) else { return false };
        match op {
            1 => ord == Ordering::Equal,
            2 => ord != Ordering::Greater,
            3 => ord == Ordering::Less,
            4 => ord != Ordering::Less,
            5 => ord == Ordering::Greater,
            _ => false,
        }
    }

    fn type_of(&mut self, t: &Target) -> Res<u64> {
        Ok(match t {
            Target::Node(n) => node_type_number(self, *n),
            Target::Missing(name) => return Err(Error::NotFound(name.to_string())),
            Target::Debug => 16,
            other => match self.read_target(other)? {
                Value::Node(n) => node_type_number(self, n),
                Value::Ref(r) => match *r {
                    Reference::Node(n) => node_type_number(self, n),
                    r => self.read_ref(&r)?.type_number(),
                },
                v => v.type_number(),
            },
        })
    }

    // ---- targets, references and storing

    /// A SuperName or Target: where a result goes. Names are not run even
    /// if they are methods (CondRefOf(\_OSI) must not call it).
    pub(crate) fn target(&mut self, c: &[u8], pos: &mut usize, scope: NodeId) -> Res<Target> {
        let b = *c.get(*pos).ok_or(Error::Truncated)?;
        match b {
            0x00 => {
                *pos += 1;
                Ok(Target::None)
            }
            op::LOCAL0..=op::LOCAL7 => {
                *pos += 1;
                Ok(Target::Local(b - op::LOCAL0))
            }
            op::ARG0..=op::ARG6 => {
                *pos += 1;
                Ok(Target::Arg(b - op::ARG0))
            }
            op::EXT if c.get(*pos + 1) == Some(&ext::DEBUG) => {
                *pos += 2;
                Ok(Target::Debug)
            }
            op::DEREF_OF => {
                *pos += 1;
                match self.expr(c, pos, scope)? {
                    Value::Ref(r) => Ok(Target::Ref(*r)),
                    Value::Node(n) => Ok(Target::Node(n)),
                    Value::String(s) => {
                        let name = NameString::parse_str(&s).ok_or(Error::BadName(*pos))?;
                        Ok(match self.lookup(scope, &name) {
                            Some(n) => Target::Node(n),
                            None => Target::Missing(name),
                        })
                    }
                    _ => Err(Error::Type("DerefOf target")),
                }
            }
            _ if is_lead(b) => {
                let name = NameString::parse(c, pos)?;
                Ok(match self.lookup(scope, &name) {
                    Some(n) => Target::Node(n),
                    None => Target::Missing(name),
                })
            }
            _ => match self.expr(c, pos, scope)? {
                // Index, RefOf, or a method giving a reference
                Value::Ref(r) => Ok(Target::Ref(*r)),
                Value::Node(n) => Ok(Target::Node(n)),
                _ => Err(Error::Type("target")),
            },
        }
    }

    fn reference(&self, t: &Target) -> Res<Reference> {
        let d = self.depth();
        match t {
            Target::Node(n) => Ok(Reference::Node(*n)),
            Target::Local(i) => Ok(Reference::Local(d, *i)),
            Target::Arg(i) => Ok(Reference::Arg(d, *i)),
            Target::Ref(r) => Ok(r.clone()),
            Target::Missing(name) => Err(Error::NotFound(name.to_string())),
            _ => Err(Error::Type("RefOf")),
        }
    }

    fn read_target(&mut self, t: &Target) -> Res<Value> {
        match t {
            Target::Local(i) => Ok(self.frame()?.locals[*i as usize].clone()),
            Target::Arg(i) => Ok(self.frame()?.args[*i as usize].clone()),
            Target::Node(n) => self.value_of_node(*n),
            Target::Ref(r) => self.read_ref(r),
            Target::Missing(name) => Err(Error::NotFound(name.to_string())),
            _ => Err(Error::Type("operand")),
        }
    }

    pub(crate) fn read_ref(&mut self, r: &Reference) -> Res<Value> {
        match r {
            Reference::Node(n) => self.value_of_node(*n),
            Reference::Local(f, i) => Ok(self.frames.get(*f as usize).ok_or(Error::Dead)?.locals[*i as usize].clone()),
            Reference::Arg(f, i) => Ok(self.frames.get(*f as usize).ok_or(Error::Dead)?.args[*i as usize].clone()),
            Reference::Index(h, i) => match self.holder_ref(h)? {
                Value::Package(p) => p.get(*i).cloned().ok_or(Error::Range),
                Value::Buffer(b) => b.get(*i).map(|&x| Value::Integer(x as u64)).ok_or(Error::Range),
                Value::String(s) => s.chars().nth(*i).map(|x| Value::Integer(x as u64)).ok_or(Error::Range),
                _ => Err(Error::Type("Index")),
            },
        }
    }

    /// DerefOf: what a reference points to, or the object a string names.
    fn deref(&mut self, v: Value, scope: NodeId) -> Res<Value> {
        match v {
            Value::Ref(r) => match self.read_ref(&r)? {
                Value::Name(name, s) => Ok(self.lookup(s, &name).map(Value::Node).unwrap_or(Value::Name(name, s))),
                x => Ok(x),
            },
            Value::String(s) => {
                let name = NameString::parse_str(&s).ok_or(Error::BadName(0))?;
                let n = self.find_or_fail(scope, &name)?;
                self.value_of_node(n)
            }
            Value::Node(n) => self.resolve_node(n),
            other => Ok(other),
        }
    }

    pub(crate) fn store(&mut self, v: Value, t: &Target) -> Res<()> {
        match t {
            Target::None => Ok(()),
            Target::Debug => {
                let text = match &v {
                    Value::Ref(_) | Value::Node(_) | Value::Name(..) => match self.resolve(v.clone()) {
                        Ok(Value::Node(n)) => self.path(n),
                        Ok(x) => x.to_string(),
                        Err(_) => v.to_string(),
                    },
                    v => v.to_string(),
                };
                self.handler.debug(&text);
                Ok(())
            }
            Target::Local(i) => {
                self.frame_mut()?.locals[*i as usize] = v;
                Ok(())
            }
            Target::Arg(i) => {
                let f = self.frame_mut()?;
                if let Value::Ref(r) = &f.args[*i as usize] {
                    if let Reference::Node(n) = **r {
                        return self.write_node(n, v);
                    }
                }
                f.args[*i as usize] = v;
                f.origin[*i as usize] = None;
                Ok(())
            }
            Target::Node(n) => self.write_node(*n, v),
            Target::Ref(r) => self.write_ref(r, v),
            Target::Missing(name) => Err(Error::NotFound(name.to_string())),
        }
    }

    /// Stores into a named object, converting to its type (ACPI 6.5,
    /// 19.3.5.8: an integer stays an integer, a buffer keeps its size).
    pub(crate) fn write_node(&mut self, n: NodeId, v: Value) -> Res<()> {
        enum Kind {
            Int,
            Str,
            Buf(usize),
            Replace,
            Field(crate::namespace::Field),
            BufferField(BufferField),
            Alias(NodeId),
        }
        let kind = match self.object(n)? {
            Object::Data(Value::Integer(_)) => Kind::Int,
            Object::Data(Value::String(_)) => Kind::Str,
            Object::Data(Value::Buffer(b)) => Kind::Buf(b.len()),
            Object::Data(_) => Kind::Replace,
            Object::Field(f) => Kind::Field(f.clone()),
            Object::BufferField(bf) => Kind::BufferField(bf.clone()),
            Object::Alias(t) => Kind::Alias(*t),
            // Firmware does store into methods (`ECOK () = Arg1`); like
            // ACPICA, the method becomes the value.
            Object::Method(_) => Kind::Replace,
            _ => return Err(Error::Type("cannot store into this object")),
        };
        let typed = matches!(kind, Kind::Int | Kind::Str | Kind::Buf(_) | Kind::Field(_) | Kind::BufferField(_));
        let v = if typed { self.resolve(v)? } else { v };
        let new = match kind {
            Kind::Alias(t) => return self.write_node(t, v),
            Kind::Field(f) => return self.write_field(&f, &v),
            Kind::BufferField(bf) => return self.write_buffer_field(&bf, &v),
            Kind::Int => match value::to_integer(&v, self.wide) {
                Ok(x) => Value::Integer(x),
                Err(_) => v,
            },
            Kind::Str => match value::to_string_implicit(&v, self.wide) {
                Ok(s) => Value::String(s),
                Err(_) => v,
            },
            Kind::Buf(len) => match value::to_buffer(&v, self.wide) {
                Ok(mut b) => {
                    if len > 0 {
                        b.resize(len, 0);
                    }
                    Value::Buffer(b)
                }
                Err(_) => v,
            },
            Kind::Replace => v,
        };
        if let Some(node) = self.ns.get_mut(n) {
            node.object = Object::Data(new);
        }
        Ok(())
    }

    fn write_ref(&mut self, r: &Reference, v: Value) -> Res<()> {
        match r {
            Reference::Node(n) => self.write_node(*n, v),
            Reference::Local(f, i) => {
                self.frames.get_mut(*f as usize).ok_or(Error::Dead)?.locals[*i as usize] = v;
                Ok(())
            }
            Reference::Arg(f, i) => {
                self.frames.get_mut(*f as usize).ok_or(Error::Dead)?.args[*i as usize] = v;
                Ok(())
            }
            Reference::Index(h, i) => {
                let is_bytes = matches!(self.holder_ref(h)?, Value::Buffer(_) | Value::String(_));
                let v = if is_bytes { self.resolve(v)? } else { v };
                let wide = self.wide;
                let i = *i;
                self.with_holder(h, |c| match c {
                    Value::Package(p) => {
                        *p.get_mut(i).ok_or(Error::Range)? = v;
                        Ok(())
                    }
                    Value::Buffer(b) => {
                        *b.get_mut(i).ok_or(Error::Range)? = value::to_integer(&v, wide)? as u8;
                        Ok(())
                    }
                    Value::String(s) => {
                        let mut bytes = value::string_bytes(s);
                        *bytes.get_mut(i).ok_or(Error::Range)? = value::to_integer(&v, wide)? as u8;
                        *s = value::string_from_bytes(&bytes);
                        Ok(())
                    }
                    _ => Err(Error::Type("Index")),
                })
            }
        }
    }

    /// CopyObject: replaces the target without converting.
    fn copy_object(&mut self, v: Value, t: &Target) -> Res<()> {
        match t {
            Target::Node(n) => {
                let v = match v {
                    Value::Node(src) => {
                        let o = self.object(src)?.clone();
                        if let Some(node) = self.ns.get_mut(*n) {
                            node.object = o;
                        }
                        return Ok(());
                    }
                    v => v,
                };
                if let Some(node) = self.ns.get_mut(*n) {
                    node.object = Object::Data(v);
                }
                Ok(())
            }
            Target::Arg(i) => {
                let f = self.frame_mut()?;
                f.args[*i as usize] = v;
                f.origin[*i as usize] = None;
                Ok(())
            }
            other => self.store(v, other),
        }
    }

    // ---- loading tables from AML

    fn load_op(&mut self, c: &[u8], pos: &mut usize, scope: NodeId) -> Res<Value> {
        let src = self.target(c, pos, scope)?;
        let dest = self.target(c, pos, scope)?;
        let bytes = match &src {
            Target::Node(n) => match self.object(*n)?.clone() {
                Object::Region(r) => self.region_bytes(&r)?,
                _ => {
                    let v = self.value_of_node(*n)?;
                    value::to_buffer(&v, self.wide)?
                }
            },
            other => {
                let v = self.read_target(other)?;
                let v = self.resolve(v)?;
                value::to_buffer(&v, self.wide)?
            }
        };
        let ok = self.load_table(&bytes).is_ok();
        let handle = Value::Integer(self.tables.len() as u64);
        self.store(handle, &dest)?;
        Ok(self.truth(ok))
    }

    /// A table in a memory region, for Load.
    fn region_bytes(&mut self, r: &Region) -> Res<Vec<u8>> {
        if r.space != space::MEMORY {
            return Err(Error::NoHandler(r.space));
        }
        let len = self.handler.read(space::MEMORY, r.base + 4, 32)?.min(r.len).min(1 << 20);
        let mut out = Vec::with_capacity(len as usize);
        let mut at = 0;
        while at < len {
            let v = self.handler.read(space::MEMORY, r.base + at, 32)?;
            out.extend_from_slice(&(v as u32).to_le_bytes());
            at += 4;
        }
        out.truncate(len as usize);
        Ok(out)
    }

    fn load_table_op(&mut self, c: &[u8], pos: &mut usize, scope: NodeId) -> Res<Value> {
        let sig = self.text(c, pos, scope)?;
        let oem = self.text(c, pos, scope)?;
        let oem_table = self.text(c, pos, scope)?;
        let _root = self.data(c, pos, scope)?;
        let param_path = self.data(c, pos, scope)?;
        let param = self.expr(c, pos, scope)?;
        let Some(bytes) = self.handler.table(&sig, &oem, &oem_table) else { return Ok(Value::Integer(0)) };
        self.load_table(&bytes)?;
        if let Value::String(p) = &param_path {
            if let Some(n) = NameString::parse_str(p).and_then(|name| self.lookup(scope, &name)) {
                self.write_node(n, param)?;
            }
        }
        Ok(Value::Integer(self.tables.len() as u64))
    }
}

fn node_type_number<H: Handler>(i: &Interpreter<H>, n: NodeId) -> u64 {
    use crate::ObjectType as T;
    match i.object_type(n) {
        T::Scope | T::Uninitialized | T::Reference => 0,
        T::Integer => 1,
        T::String => 2,
        T::Buffer => 3,
        T::Package => 4,
        T::Field => 5,
        T::Device => 6,
        T::Event => 7,
        T::Method => 8,
        T::Mutex => 9,
        T::Region => 10,
        T::PowerResource => 11,
        T::Processor => 12,
        T::ThermalZone => 13,
        T::BufferField => 14,
    }
}

fn from_bcd(mut x: u64) -> u64 {
    let (mut r, mut mul) = (0u64, 1u64);
    while x != 0 {
        r = r.wrapping_add((x & 0xF) * mul);
        mul = mul.wrapping_mul(10);
        x >>= 4;
    }
    r
}

fn to_bcd(mut x: u64) -> u64 {
    let (mut r, mut shift) = (0u64, 0u32);
    while x != 0 && shift < 64 {
        r |= (x % 10) << shift;
        x /= 10;
        shift += 4;
    }
    r
}

/// ConcatenateResTemplate: two resource templates as one, with one end tag.
fn concat_res(a: &[u8], b: &[u8]) -> Vec<u8> {
    let strip = |x: &[u8]| -> usize {
        // Everything before the end tag (0x79 and a checksum).
        let mut at = 0;
        while at < x.len() {
            let t = x[at];
            if t == 0x79 {
                return at;
            }
            let len = if t & 0x80 == 0 {
                1 + (t & 7) as usize
            } else if at + 2 < x.len() {
                3 + u16::from_le_bytes([x[at + 1], x[at + 2]]) as usize
            } else {
                return at;
            };
            at += len;
        }
        x.len().min(at)
    };
    let mut out = a[..strip(a)].to_vec();
    out.extend_from_slice(&b[..strip(b)]);
    out.extend_from_slice(&[0x79, 0x00]);
    out
}
