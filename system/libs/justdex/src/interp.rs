//! The Dalvik interpreter: one method at a time, one instruction at a time.
//!
//! Instructions: <https://source.android.com/docs/core/runtime/dalvik-bytecode>.

use crate::dex::{Code, NO_INDEX};
use crate::{compress, java_name, new_ref, Inst, JResult, Method, Native, Obj, Ref, Throw, Val, Vm, MAX_DEPTH};
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use arcc_core::float;

enum Flow {
    Next(usize),
    Return(Val),
}

/// A method's registers and where it is.
struct Frame {
    r: Vec<Val>,
    result: Val,
    exc: Option<Ref>,
}

impl Vm {
    /// Runs a method of the program; `args` fill its last registers.
    pub(crate) fn exec(&mut self, m: &Rc<Method>, args: Vec<Val>) -> JResult<Val> {
        let Some(code) = m.code.clone() else {
            return self.throw("Ljava/lang/AbstractMethodError;", &format!("{}.{}", java_name(&m.class), m.name));
        };
        if self.depth >= MAX_DEPTH {
            return self.throw("Ljava/lang/StackOverflowError;", "");
        }
        self.depth += 1;
        let n = code.registers as usize;
        let mut f = Frame { r: vec![Val::NULL; n + 1], result: Val::NULL, exc: None };
        let first = n.saturating_sub(code.ins as usize);
        for (i, a) in args.into_iter().enumerate() {
            if first + i < n {
                f.r[first + i] = a;
            }
        }
        let mut pc = 0;
        let out = loop {
            match self.step(m, &code, &mut f, pc) {
                Ok(Flow::Next(next)) => pc = next,
                Ok(Flow::Return(v)) => break Ok(v),
                Err(Throw::Ex(ex)) => match self.handler(&code, m.dex, pc, &ex) {
                    Some(at) => {
                        f.exc = Some(ex);
                        pc = at;
                    }
                    None => break Err(Throw::Ex(ex)),
                },
                Err(t) => break Err(t),
            }
        };
        self.depth -= 1;
        out
    }

    /// Where a try block around `pc` catches `ex`.
    fn handler(&mut self, code: &Code, dex: usize, pc: usize, ex: &Ref) -> Option<usize> {
        let class = Self::class_of(ex);
        for t in &code.tries {
            if (t.start as usize..t.end as usize).contains(&pc) {
                for &(ty, addr) in &t.handlers {
                    if ty == NO_INDEX {
                        return Some(addr as usize);
                    }
                    let target = self.dexes[dex].types[ty as usize].clone();
                    if self.is_subclass(&class, &target) {
                        return Some(addr as usize);
                    }
                }
            }
        }
        None
    }

    fn type_at(&self, dex: usize, idx: u16) -> Rc<str> {
        self.dexes[dex].types.get(idx as usize).cloned().unwrap_or_else(|| Rc::from("?"))
    }

    /// (class, name) of a field.
    fn field_at(&self, dex: usize, idx: u16) -> (Rc<str>, Rc<str>) {
        let d = &self.dexes[dex];
        let f = &d.fields[idx as usize];
        (d.types[f.class as usize].clone(), d.strings[f.name as usize].clone())
    }

    /// (class, name, signature) of a method.
    fn method_at(&self, dex: usize, idx: u16) -> (Rc<str>, Rc<str>, Rc<str>) {
        let d = &self.dexes[dex];
        let m = &d.methods[idx as usize];
        (d.types[m.class as usize].clone(), d.strings[m.name as usize].clone(), d.protos[m.proto as usize].sig.clone())
    }

    fn step(&mut self, m: &Rc<Method>, code: &Code, f: &mut Frame, pc: usize) -> JResult<Flow> {
        let ins = &code.insns;
        let at = |k: usize| ins.get(pc + k).copied().unwrap_or(0);
        let lit32 = |k: usize| (at(k) as u32 | (at(k + 1) as u32) << 16) as i32;
        let w = at(0);
        let op = (w & 0xFF) as u8;
        let aa = (w >> 8) as usize;
        let a = ((w >> 8) & 0xF) as usize;
        let b = (w >> 12) as usize;
        let dex = m.dex;
        let r = &mut f.r;
        let jump = |off: i32| Flow::Next((pc as i64 + off as i64) as usize);
        let next = |n: usize| Flow::Next(pc + n);
        Ok(match op {
            0x00 => next(1),
            0x01 | 0x07 => {
                r[a] = r[b].clone();
                next(1)
            }
            0x02 | 0x08 => {
                r[aa] = r[at(1) as usize].clone();
                next(2)
            }
            0x03 | 0x09 => {
                r[at(1) as usize] = r[at(2) as usize].clone();
                next(3)
            }
            0x04..=0x06 => {
                let (d, s, n) = match op {
                    0x04 => (a, b, 1),
                    0x05 => (aa, at(1) as usize, 2),
                    _ => (at(1) as usize, at(2) as usize, 3),
                };
                let (lo, hi) = (r[s].clone(), r[s + 1].clone());
                r[d] = lo;
                r[d + 1] = hi;
                next(n)
            }
            0x0A..=0x0C => {
                r[aa] = core::mem::replace(&mut f.result, Val::NULL);
                next(1)
            }
            0x0D => {
                r[aa] = f.exc.take().map(Val::R).unwrap_or(Val::NULL);
                next(1)
            }
            0x0E => Flow::Return(Val::NULL),
            0x0F..=0x11 => Flow::Return(r[aa].clone()),
            0x12 => {
                r[a] = Val::int((w as i16 >> 12) as i32);
                next(1)
            }
            0x13 => {
                r[aa] = Val::int(at(1) as i16 as i32);
                next(2)
            }
            0x14 => {
                r[aa] = Val::int(lit32(1));
                next(3)
            }
            0x15 => {
                r[aa] = Val::int((at(1) as i32) << 16);
                next(2)
            }
            0x16 => {
                r[aa] = Val::long(at(1) as i16 as i64);
                next(2)
            }
            0x17 => {
                r[aa] = Val::long(lit32(1) as i64);
                next(3)
            }
            0x18 => {
                let v = (0..4).fold(0u64, |v, k| v | (at(1 + k) as u64) << (16 * k));
                r[aa] = Val::P(v);
                next(5)
            }
            0x19 => {
                r[aa] = Val::long((at(1) as i64) << 48);
                next(2)
            }
            0x1A | 0x1B => {
                let (idx, n) = if op == 0x1A { (at(1) as u32, 2) } else { (lit32(1) as u32, 3) };
                let s = self.literal(dex, idx);
                f.r[aa] = Val::R(s);
                next(n)
            }
            0x1C => {
                let ty = self.type_at(dex, at(1));
                f.r[aa] = self.class_object(&ty);
                next(2)
            }
            0x1D | 0x1E => next(1),
            0x1F => {
                let ty = self.type_at(dex, at(1));
                let v = r[aa].clone();
                if !v.is_null() && !self.instance_of(&v, &ty) {
                    let from = match &v {
                        Val::R(o) => java_name(&Self::class_of(o)),
                        Val::P(_) => "?".into(),
                    };
                    return self.throw("Ljava/lang/ClassCastException;", &format!("{from} cannot be cast to {}", java_name(&ty)));
                }
                next(2)
            }
            0x20 => {
                let ty = self.type_at(dex, at(1));
                let v = r[b].clone();
                f.r[a] = Val::bool(self.instance_of(&v, &ty));
                next(2)
            }
            0x21 => {
                let len = match &r[b] {
                    Val::R(o) => match &*o.borrow() {
                        Obj::Arr { data, .. } => data.len() as i32,
                        _ => 0,
                    },
                    Val::P(_) => return self.npe("array length of null"),
                };
                r[a] = Val::int(len);
                next(1)
            }
            0x22 => {
                let ty = self.type_at(dex, at(1));
                let o = self.new_object(&ty)?;
                f.r[aa] = Val::R(o);
                next(2)
            }
            0x23 => {
                let ty = self.type_at(dex, at(1));
                let n = r[b].i();
                f.r[a] = self.new_array(&ty, n)?;
                next(2)
            }
            0x24 | 0x25 => {
                let ty = self.type_at(dex, at(1));
                let regs = arg_regs(op == 0x25, w, at(2));
                let data = regs.iter().map(|&x| r[x].clone()).collect();
                f.result = Val::R(new_ref(Obj::Arr { ty, data }));
                next(3)
            }
            0x26 => {
                let p = (pc as i64 + lit32(1) as i64) as usize;
                let width = ins.get(p + 1).copied().unwrap_or(0) as usize;
                let size = ins.get(p + 2).copied().unwrap_or(0) as usize | (ins.get(p + 3).copied().unwrap_or(0) as usize) << 16;
                let bytes: Vec<u8> = ins.iter().skip(p + 4).take((size * width).div_ceil(2)).flat_map(|u| u.to_le_bytes()).collect();
                if let Val::R(o) = &r[aa] {
                    if let Obj::Arr { data, ty } = &mut *o.borrow_mut() {
                        let float = matches!(&**ty, "[F" | "[D");
                        for (k, slot) in data.iter_mut().enumerate().take(size) {
                            let e = &bytes[k * width..k * width + width];
                            let v = e.iter().rev().fold(0u64, |v, &x| v << 8 | x as u64);
                            *slot = match width {
                                1 => Val::int(v as u8 as i8 as i32),
                                2 if ty.as_ref() == "[C" => Val::int(v as u16 as i32),
                                2 => Val::int(v as u16 as i16 as i32),
                                4 if float => Val::P(v),
                                4 => Val::int(v as u32 as i32),
                                _ => Val::P(v),
                            };
                        }
                    }
                } else {
                    return self.npe("filling a null array");
                }
                next(3)
            }
            0x27 => match r[aa].clone() {
                Val::R(o) => return Err(Throw::Ex(o)),
                Val::P(_) => return self.npe("throw null"),
            },
            0x28 => jump(aa as u8 as i8 as i32),
            0x29 => jump(at(1) as i16 as i32),
            0x2A => jump(lit32(1)),
            0x2B | 0x2C => {
                let p = (pc as i64 + lit32(1) as i64) as usize;
                let get = |k: usize| ins.get(p + k).copied().unwrap_or(0);
                let get32 = |k: usize| (get(k) as u32 | (get(k + 1) as u32) << 16) as i32;
                let size = get(1) as usize;
                let key = r[aa].i();
                let mut target = None;
                if op == 0x2B {
                    let first = get32(2);
                    let i = key.wrapping_sub(first);
                    if i >= 0 && (i as usize) < size {
                        target = Some(get32(4 + 2 * i as usize));
                    }
                } else {
                    for k in 0..size {
                        if get32(2 + 2 * k) == key {
                            target = Some(get32(2 + 2 * size + 2 * k));
                            break;
                        }
                    }
                }
                match target {
                    Some(off) => jump(off),
                    None => next(3),
                }
            }
            0x2D..=0x31 => {
                let (x, y) = ((at(1) & 0xFF) as usize, (at(1) >> 8) as usize);
                let c = match op {
                    0x2D | 0x2E => fcmp(r[x].f() as f64, r[y].f() as f64, op == 0x2E),
                    0x2F | 0x30 => fcmp(r[x].d(), r[y].d(), op == 0x30),
                    _ => (r[x].j() > r[y].j()) as i32 - (r[x].j() < r[y].j()) as i32,
                };
                r[aa] = Val::int(c);
                next(2)
            }
            0x32..=0x37 => {
                let ok = if op <= 0x33 {
                    r[a].same(&r[b]) == (op == 0x32)
                } else {
                    test(op - 0x32, r[a].i(), r[b].i())
                };
                if ok {
                    jump(at(1) as i16 as i32)
                } else {
                    next(2)
                }
            }
            0x38..=0x3D => {
                let v = match &r[aa] {
                    Val::R(_) => 1,
                    p => p.i(),
                };
                if test(op - 0x38, v, 0) {
                    jump(at(1) as i16 as i32)
                } else {
                    next(2)
                }
            }
            0x44..=0x51 => {
                let (x, y) = ((at(1) & 0xFF) as usize, (at(1) >> 8) as usize);
                let Val::R(arr) = r[x].clone() else { return self.npe("array access on null") };
                let i = r[y].i();
                let mut o = arr.borrow_mut();
                let Obj::Arr { data, .. } = &mut *o else { return self.npe("not an array") };
                let len = data.len();
                if i < 0 || i as usize >= len {
                    drop(o);
                    return self.throw("Ljava/lang/ArrayIndexOutOfBoundsException;", &format!("Index {i} out of bounds for length {len}"));
                }
                let i = i as usize;
                if op <= 0x4A {
                    f.r[aa] = data[i].clone();
                } else {
                    let v = &f.r[aa];
                    data[i] = match op {
                        0x4F => Val::int(v.i() as i8 as i32),
                        0x50 => Val::int(v.i() as u16 as i32),
                        0x51 => Val::int(v.i() as i16 as i32),
                        _ => v.clone(),
                    };
                }
                next(2)
            }
            0x52..=0x5F => {
                let (_, name) = self.field_at(dex, at(1));
                let Val::R(o) = f.r[b].clone() else { return self.npe(&format!("field {name} of null")) };
                let mut o = o.borrow_mut();
                let Obj::Inst(inst) = &mut *o else { return self.npe(&format!("field {name}")) };
                if op <= 0x58 {
                    f.r[a] = inst.fields.get(&name).cloned().unwrap_or(Val::NULL);
                } else {
                    let v = match op {
                        0x5D => Val::int(f.r[a].i() as i8 as i32),
                        0x5E => Val::int(f.r[a].i() as u16 as i32),
                        0x5F => Val::int(f.r[a].i() as i16 as i32),
                        _ => f.r[a].clone(),
                    };
                    inst.fields.insert(name, v);
                }
                next(2)
            }
            0x60..=0x6D => {
                let (class, name) = self.field_at(dex, at(1));
                self.init_class(&class)?;
                let owner = self.static_owner(&class, &name);
                match owner {
                    Some(c) => {
                        if op <= 0x66 {
                            f.r[aa] = c.statics.borrow().get(&name).cloned().unwrap_or(Val::NULL);
                        } else {
                            c.statics.borrow_mut().insert(name, f.r[aa].clone());
                        }
                    }
                    None if op <= 0x66 => f.r[aa] = self.static_field(&class, &name)?,
                    None => return self.throw("Ljava/lang/NoSuchFieldError;", &format!("{}.{name}", java_name(&class))),
                }
                next(2)
            }
            0x6E..=0x72 | 0x74..=0x78 => {
                let range = op >= 0x74;
                let kind = if range { op - 0x74 } else { op - 0x6E };
                let regs: Vec<Val> = arg_regs(range, w, at(2)).into_iter().map(|x| f.r[x].clone()).collect();
                f.result = self.invoke(kind, m, at(1), regs)?;
                next(3)
            }
            0x7B..=0x8F => {
                let v = &r[b];
                r[a] = match op {
                    0x7B => Val::int(v.i().wrapping_neg()),
                    0x7C => Val::int(!v.i()),
                    0x7D => Val::long(v.j().wrapping_neg()),
                    0x7E => Val::long(!v.j()),
                    0x7F => Val::float(-v.f()),
                    0x80 => Val::double(-v.d()),
                    0x81 => Val::long(v.i() as i64),
                    0x82 => Val::float(v.i() as f32),
                    0x83 => Val::double(v.i() as f64),
                    0x84 => Val::int(v.j() as i32),
                    0x85 => Val::float(v.j() as f32),
                    0x86 => Val::double(v.j() as f64),
                    0x87 => Val::int(v.f() as i32),
                    0x88 => Val::long(v.f() as i64),
                    0x89 => Val::double(v.f() as f64),
                    0x8A => Val::int(v.d() as i32),
                    0x8B => Val::long(v.d() as i64),
                    0x8C => Val::float(v.d() as f32),
                    0x8D => Val::int(v.i() as i8 as i32),
                    0x8E => Val::int(v.i() as u16 as i32),
                    _ => Val::int(v.i() as i16 as i32),
                };
                next(1)
            }
            0x90..=0xCF => {
                let (op, d, x, y, n) = if op >= 0xB0 {
                    (op - 0x20, a, a, b, 1)
                } else {
                    (op, aa, (at(1) & 0xFF) as usize, (at(1) >> 8) as usize, 2)
                };
                let v = match op {
                    0x90..=0x9A => Val::int(self.int_op(op - 0x90, r[x].i(), r[y].i())?),
                    0x9B..=0xA5 => {
                        let k = op - 0x9B;
                        // Shifts take an int for the distance.
                        let yv = if k >= 8 { r[y].i() as i64 } else { r[y].j() };
                        Val::long(self.long_op(k, r[x].j(), yv)?)
                    }
                    0xA6..=0xAA => Val::float(float_op(op - 0xA6, r[x].f() as f64, r[y].f() as f64) as f32),
                    _ => Val::double(float_op(op - 0xAB, r[x].d(), r[y].d())),
                };
                f.r[d] = v;
                next(n)
            }
            0xD0..=0xE2 => {
                let (k, x, lit, n) = if op <= 0xD7 {
                    (op - 0xD0, b, at(1) as i16 as i32, 2)
                } else {
                    (op - 0xD8, (at(1) & 0xFF) as usize, (at(1) >> 8) as u8 as i8 as i32, 2)
                };
                let d = if op <= 0xD7 { a } else { aa };
                let v = r[x].i();
                let out = if k == 1 { lit.wrapping_sub(v) } else { self.int_op(k, v, lit)? };
                f.r[d] = Val::int(out);
                next(n)
            }
            _ => {
                return self.throw("Ljava/lang/VerifyError;", &format!("instruction 0x{op:02x} in {}.{} is not supported", java_name(&m.class), m.name));
            }
        })
    }

    fn int_op(&mut self, k: u8, x: i32, y: i32) -> JResult<i32> {
        Ok(match k {
            0 => x.wrapping_add(y),
            1 => x.wrapping_sub(y),
            2 => x.wrapping_mul(y),
            3 | 4 if y == 0 => return self.throw("Ljava/lang/ArithmeticException;", "/ by zero"),
            3 => x.wrapping_div(y),
            4 => x.wrapping_rem(y),
            5 => x & y,
            6 => x | y,
            7 => x ^ y,
            8 => x.wrapping_shl(y as u32 & 31),
            9 => x.wrapping_shr(y as u32 & 31),
            _ => ((x as u32) >> (y as u32 & 31)) as i32,
        })
    }

    fn long_op(&mut self, k: u8, x: i64, y: i64) -> JResult<i64> {
        Ok(match k {
            0 => x.wrapping_add(y),
            1 => x.wrapping_sub(y),
            2 => x.wrapping_mul(y),
            3 | 4 if y == 0 => return self.throw("Ljava/lang/ArithmeticException;", "/ by zero"),
            3 => x.wrapping_div(y),
            4 => x.wrapping_rem(y),
            5 => x & y,
            6 => x | y,
            7 => x ^ y,
            8 => x.wrapping_shl(y as u32 & 63),
            9 => x.wrapping_shr(y as u32 & 63),
            _ => ((x as u64) >> (y as u32 & 63)) as i64,
        })
    }

    /// The class that declares static field `name`, looking up from `class`.
    fn static_owner(&mut self, class: &str, name: &str) -> Option<Rc<crate::Class>> {
        let mut cur: Rc<str> = Rc::from(class);
        for _ in 0..64 {
            let c = self.class(&cur)?;
            if c.statics.borrow().contains_key(name) {
                return Some(c);
            }
            for i in c.interfaces.clone() {
                let _ = self.init_class(&i);
                if let Some(ic) = self.class(&i) {
                    if ic.statics.borrow().contains_key(name) {
                        return Some(ic);
                    }
                }
            }
            cur = c.superclass.clone()?;
        }
        None
    }

    pub(crate) fn new_array(&mut self, ty: &str, n: i32) -> JResult<Val> {
        if n < 0 {
            return self.throw("Ljava/lang/NegativeArraySizeException;", &format!("{n}"));
        }
        Ok(Val::R(new_ref(Obj::Arr { ty: Rc::from(ty), data: vec![Val::NULL; n as usize] })))
    }

    /// The `Class` object for a type.
    pub(crate) fn class_object(&mut self, ty: &str) -> Val {
        Val::R(new_ref(Obj::Inst(Inst { class: Rc::from("Ljava/lang/Class;"), fields: BTreeMap::new(), native: Native::Class(Rc::from(ty)) })))
    }

    /// invoke-virtual (0), -super (1), -direct (2), -static (3),
    /// -interface (4).
    fn invoke(&mut self, kind: u8, caller: &Rc<Method>, idx: u16, regs: Vec<Val>) -> JResult<Val> {
        let (class, name, sig) = self.method_at(caller.dex, idx);
        match kind {
            3 => {
                self.init_class(&class)?;
                if let Some(m) = self.find_method(&class, &name, &sig) {
                    return self.exec(&m, regs);
                }
                let (_, args) = compress(&sig, false, regs);
                self.native(&class, &name, &sig, None, args)
            }
            2 => {
                if let Some(m) = self.class(&class).and_then(|c| c.method(&name, &sig).cloned()) {
                    return self.exec(&m, regs);
                }
                let base = self.library_base(&class);
                let (this, args) = compress(&sig, true, regs);
                if this.as_ref().is_none_or(|t| t.is_null()) {
                    return self.npe(&format!("{name}() on null"));
                }
                self.native(&base, &name, &sig, this, args)
            }
            1 => {
                let parent = self.class(&caller.class).and_then(|c| c.superclass.clone()).unwrap_or_else(|| Rc::from("Ljava/lang/Object;"));
                if let Some(m) = self.find_virtual(&parent, &name, &sig) {
                    return self.exec(&m, regs);
                }
                let base = self.library_base(&parent);
                let (this, args) = compress(&sig, true, regs);
                self.native(&base, &name, &sig, this, args)
            }
            _ => {
                let Some(Val::R(o)) = regs.first().cloned() else {
                    return self.npe(&format!("calling {}.{name}() on null", java_name(&class)));
                };
                let runtime = Self::class_of(&o);
                if let Some(m) = self.find_virtual(&runtime, &name, &sig) {
                    return self.exec(&m, regs);
                }
                let base = self.library_base(&runtime);
                let (this, args) = compress(&sig, true, regs);
                self.native(&base, &name, &sig, this, args)
            }
        }
    }
}

/// The registers of an invoke or filled-new-array instruction.
fn arg_regs(range: bool, w: u16, third: u16) -> Vec<usize> {
    if range {
        let n = (w >> 8) as usize;
        (0..n).map(|k| third as usize + k).collect()
    } else {
        let n = (w >> 12) as usize;
        let g = ((w >> 8) & 0xF) as usize;
        let list = [(third & 0xF) as usize, (third >> 4 & 0xF) as usize, (third >> 8 & 0xF) as usize, (third >> 12) as usize, g];
        list[..n.min(5)].to_vec()
    }
}

/// if-eq, -ne, -lt, -ge, -gt, -le.
fn test(k: u8, x: i32, y: i32) -> bool {
    match k {
        0 => x == y,
        1 => x != y,
        2 => x < y,
        3 => x >= y,
        4 => x > y,
        _ => x <= y,
    }
}

/// cmpl/cmpg: NaN gives -1 or 1.
fn fcmp(x: f64, y: f64, g: bool) -> i32 {
    if x.is_nan() || y.is_nan() {
        if g {
            1
        } else {
            -1
        }
    } else {
        (x > y) as i32 - (x < y) as i32
    }
}

fn float_op(k: u8, x: f64, y: f64) -> f64 {
    match k {
        0 => x + y,
        1 => x - y,
        2 => x * y,
        3 => x / y,
        _ => fmod(x, y),
    }
}

/// The remainder of `x / y` with the sign of `x`, as Java's `%`.
pub(crate) fn fmod(x: f64, y: f64) -> f64 {
    if y == 0.0 || x.is_nan() || y.is_nan() || x.is_infinite() {
        return f64::NAN;
    }
    if y.is_infinite() {
        return x;
    }
    x - float::trunc(x / y) * y
}
