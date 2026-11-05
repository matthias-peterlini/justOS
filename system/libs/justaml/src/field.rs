//! Fields: named bits of an operation region (memory, I/O ports, PCI
//! configuration space, the embedded controller...), reached through the
//! [`Handler`], and buffer fields, named bits of a buffer (ACPI 6.5, 19.6.46
//! and 19.6.18; the access rules follow ACPICA's exfldio.c).

use crate::exec::Res;
use crate::namespace::{BufferField, Field, FieldKind, Object};
use crate::value::{self, Value};
use crate::{space, Error, Handler, Interpreter, NodeId, PciAddress};
use alloc::vec;
use alloc::vec::Vec;

/// The largest field read or written at once (bits).
const MAX_BITS: u64 = 8 << 20;

fn get_bit(b: &[u8], i: u64) -> bool {
    b.get((i / 8) as usize).is_some_and(|x| x >> (i % 8) & 1 != 0)
}

fn set_bit(b: &mut [u8], i: u64, on: bool) {
    if let Some(x) = b.get_mut((i / 8) as usize) {
        if on {
            *x |= 1 << (i % 8);
        } else {
            *x &= !(1 << (i % 8));
        }
    }
}

/// The bytes a value gives when written to a field: an integer's bytes, a
/// buffer, or a string's characters.
fn source_bytes(v: &Value, wide: bool) -> Res<Vec<u8>> {
    match v {
        Value::String(s) => Ok(value::string_bytes(s)),
        v => value::to_buffer(v, wide),
    }
}

/// How many bytes each access to the field reads or writes.
fn access_bytes(f: &Field, space: u8, region_len: u64) -> u64 {
    if matches!(space, space::EMBEDDED_CONTROL | space::CMOS) {
        return 1;
    }
    let mut w = match f.access {
        1 => 1,
        2 => 2,
        3 => 4,
        4 => 8,
        0 => {
            // AnyAcc: the narrowest access that holds the whole field, if
            // one does.
            let first = f.bit_off / 8;
            let last = (f.bit_off + f.bit_len.max(1) - 1) / 8;
            [1, 2, 4].into_iter().find(|&w| first / w == last / w).unwrap_or(if f.bit_off.is_multiple_of(32) { 4 } else { 1 })
        }
        _ => 1,
    };
    // A region smaller than the access: narrower accesses.
    while w > 1 && w > region_len {
        w /= 2;
    }
    w
}

impl<H: Handler> Interpreter<H> {
    /// The region a field's bits finally live in, for its address space
    /// and length.
    fn field_region(&self, kind: &FieldKind) -> Res<(u8, u64)> {
        match kind {
            FieldKind::Region(r) | FieldKind::Bank { region: r, .. } => match self.object(*r)? {
                Object::Region(x) => Ok((x.space, x.len)),
                _ => Err(Error::Type("not an operation region")),
            },
            // Through an index register: as wide as the data register.
            FieldKind::Index { data, .. } => match self.object(*data)? {
                Object::Field(d) => Ok((space::IO, d.bit_len.div_ceil(8).max(1))),
                _ => Err(Error::Type("IndexField data is not a field")),
            },
        }
    }

    pub(crate) fn read_field(&mut self, f: &Field) -> Res<Value> {
        if f.bit_len > MAX_BITS {
            return Err(Error::Range);
        }
        let (space, region_len) = self.field_region(&f.kind)?;
        if matches!(space, space::SMBUS | space::IPMI | space::SERIAL_BUS | space::PCC) {
            return Err(Error::NoHandler(space));
        }
        let w = access_bytes(f, space, region_len);
        let wb = w * 8;
        let mut out = vec![0u8; f.bit_len.div_ceil(8) as usize];
        if f.bit_len > 0 {
            let first = f.bit_off / wb;
            let last = (f.bit_off + f.bit_len - 1) / wb;
            for u in first..=last {
                let v = self.read_unit(&f.kind, u * w, w)?;
                let lo = f.bit_off.max(u * wb);
                let hi = (f.bit_off + f.bit_len).min((u + 1) * wb);
                for bit in lo..hi {
                    if v >> (bit - u * wb) & 1 != 0 {
                        set_bit(&mut out, bit - f.bit_off, true);
                    }
                }
            }
        }
        Ok(self.bits_value(out, f.bit_len))
    }

    pub(crate) fn write_field(&mut self, f: &Field, v: &Value) -> Res<()> {
        if f.bit_len > MAX_BITS {
            return Err(Error::Range);
        }
        let (space, region_len) = self.field_region(&f.kind)?;
        if matches!(space, space::SMBUS | space::IPMI | space::SERIAL_BUS | space::PCC) {
            return Err(Error::NoHandler(space));
        }
        let src = source_bytes(v, self.wide)?;
        let w = access_bytes(f, space, region_len);
        let wb = w * 8;
        if f.bit_len == 0 {
            return Ok(());
        }
        let first = f.bit_off / wb;
        let last = (f.bit_off + f.bit_len - 1) / wb;
        let unit_mask = if wb >= 64 { u64::MAX } else { (1u64 << wb) - 1 };
        for u in first..=last {
            let lo = f.bit_off.max(u * wb);
            let hi = (f.bit_off + f.bit_len).min((u + 1) * wb);
            let whole = hi - lo == wb;
            let mut x = if whole {
                0
            } else {
                match f.update {
                    1 => unit_mask,
                    2 => 0,
                    _ => self.read_unit(&f.kind, u * w, w)?,
                }
            };
            for bit in lo..hi {
                let at = bit - u * wb;
                if get_bit(&src, bit - f.bit_off) {
                    x |= 1 << at;
                } else {
                    x &= !(1 << at);
                }
            }
            self.write_unit(&f.kind, u * w, w, x)?;
        }
        Ok(())
    }

    /// An integer if it fits, else a buffer (ACPI 6.5, 19.6.46).
    fn bits_value(&self, bytes: Vec<u8>, bit_len: u64) -> Value {
        if bit_len <= self.integer_bits() as u64 {
            Value::Integer(bytes.iter().rev().fold(0, |a, &b| a << 8 | b as u64))
        } else {
            Value::Buffer(bytes)
        }
    }

    fn read_unit(&mut self, kind: &FieldKind, at: u64, w: u64) -> Res<u64> {
        match kind {
            FieldKind::Region(r) => self.region_read(*r, at, w),
            FieldKind::Bank { region, bank, value } => {
                self.write_node(*bank, Value::Integer(*value))?;
                self.region_read(*region, at, w)
            }
            FieldKind::Index { index, data } => {
                self.write_node(*index, Value::Integer(at))?;
                let v = self.value_of_node(*data)?;
                let x = value::to_integer(&v, true)?;
                Ok(if w >= 8 { x } else { x & ((1u64 << (w * 8)) - 1) })
            }
        }
    }

    fn write_unit(&mut self, kind: &FieldKind, at: u64, w: u64, x: u64) -> Res<()> {
        match kind {
            FieldKind::Region(r) => self.region_write(*r, at, w, x),
            FieldKind::Bank { region, bank, value } => {
                self.write_node(*bank, Value::Integer(*value))?;
                self.region_write(*region, at, w, x)
            }
            FieldKind::Index { index, data } => {
                self.write_node(*index, Value::Integer(at))?;
                self.write_node(*data, Value::Integer(x))
            }
        }
    }

    fn region_of(&self, r: NodeId) -> Res<crate::namespace::Region> {
        match self.object(r)? {
            Object::Region(x) => Ok(x.clone()),
            _ => Err(Error::Type("not an operation region")),
        }
    }

    fn region_read(&mut self, r: NodeId, at: u64, w: u64) -> Res<u64> {
        let reg = self.region_of(r)?;
        if at + w > reg.len {
            return Err(Error::Range);
        }
        let addr = reg.base.wrapping_add(at);
        match reg.space {
            space::PCI => {
                let pci = self.pci_address(r)?;
                self.handler.read_pci(pci, addr as u16, (w * 8) as u8)
            }
            s => self.handler.read(s, addr, (w * 8) as u8),
        }
    }

    fn region_write(&mut self, r: NodeId, at: u64, w: u64, x: u64) -> Res<()> {
        let reg = self.region_of(r)?;
        if at + w > reg.len {
            return Err(Error::Range);
        }
        let addr = reg.base.wrapping_add(at);
        match reg.space {
            space::PCI => {
                let pci = self.pci_address(r)?;
                self.handler.write_pci(pci, addr as u16, (w * 8) as u8, x)
            }
            s => self.handler.write(s, addr, (w * 8) as u8, x),
        }
    }

    fn is_root_bridge(&mut self, n: NodeId) -> bool {
        self.ids(n).iter().any(|i| i == "PNP0A03" || i == "PNP0A08")
    }

    /// The PCI function of a PCI configuration region: the `_ADR` of the
    /// device it is in, on the bus of the bridges above it (ACPI 6.5,
    /// 6.5.6 and ACPICA's AcpiHwDerivePciId).
    fn pci_address(&mut self, r: NodeId) -> Res<PciAddress> {
        if let Object::Region(x) = self.object(r)? {
            if let Some(p) = x.pci {
                return Ok(p);
            }
        }
        // The devices from the region up to the root bridge.
        let mut chain = Vec::new();
        let mut at = self.parent(r);
        let mut root = None;
        while let Some(n) = at {
            if n == NodeId::ROOT {
                break;
            }
            if matches!(self.object(n)?, Object::Device) {
                if self.is_root_bridge(n) {
                    root = Some(n);
                    break;
                }
                chain.push(n);
            }
            at = self.parent(n);
        }
        let (segment, mut bus) = match root {
            Some(rb) => (self.integer(rb, "_SEG").unwrap_or(0) as u16, self.integer(rb, "_BBN").unwrap_or(0) as u8),
            None => (0, 0),
        };
        // The region of the root bridge itself is the host bridge's
        // configuration space, at the bridge's own _ADR.
        let target = chain.first().copied().or(root);
        chain.reverse();
        // Every device between the root bridge and the region's device is
        // a bridge: its secondary bus is where the next one is.
        if chain.len() > 1 {
            for &b in &chain[..chain.len() - 1] {
                let adr = self.integer(b, "_ADR").unwrap_or(0);
                let p = PciAddress { segment, bus, device: (adr >> 16) as u8, function: adr as u8 };
                let header = self.handler.read_pci(p, 0x0E, 8).unwrap_or(0xFF);
                if header & 0x7F == 1 || header & 0x7F == 2 {
                    bus = self.handler.read_pci(p, 0x19, 8).unwrap_or(bus as u64) as u8;
                }
            }
        }
        let adr = target.and_then(|t| self.integer(t, "_ADR")).unwrap_or(0);
        let p = PciAddress { segment, bus, device: (adr >> 16) as u8 & 0x1F, function: adr as u8 & 7 };
        if let Some(Object::Region(x)) = self.ns.get_mut(r).map(|n| &mut n.object) {
            x.pci = Some(p);
        }
        Ok(p)
    }

    fn buffer_bytes(&self, bf: &BufferField) -> Res<Vec<u8>> {
        match self.holder_ref(&bf.holder)? {
            Value::Buffer(b) => Ok(b.clone()),
            Value::String(s) => Ok(value::string_bytes(s)),
            _ => Err(Error::Type("a buffer field of something not a buffer")),
        }
    }

    pub(crate) fn read_buffer_field(&mut self, bf: &BufferField) -> Res<Value> {
        let b = self.buffer_bytes(bf)?;
        if bf.bit_len > MAX_BITS || bf.bit_off + bf.bit_len > b.len() as u64 * 8 {
            return Err(Error::Range);
        }
        let mut out = vec![0u8; bf.bit_len.div_ceil(8) as usize];
        if bf.bit_off.is_multiple_of(8) {
            let start = (bf.bit_off / 8) as usize;
            let n = out.len();
            out.copy_from_slice(&b[start..start + n]);
            if !bf.bit_len.is_multiple_of(8) {
                let last = out.len() - 1;
                out[last] &= (1u8 << (bf.bit_len % 8)) - 1;
            }
        } else {
            for i in 0..bf.bit_len {
                if get_bit(&b, bf.bit_off + i) {
                    set_bit(&mut out, i, true);
                }
            }
        }
        if bf.any_size {
            return Ok(Value::Buffer(out));
        }
        Ok(self.bits_value(out, bf.bit_len))
    }

    pub(crate) fn write_buffer_field(&mut self, bf: &BufferField, v: &Value) -> Res<()> {
        let len = self.buffer_bytes(bf)?.len() as u64;
        if bf.bit_len > MAX_BITS || bf.bit_off + bf.bit_len > len * 8 {
            return Err(Error::Range);
        }
        let src = source_bytes(v, self.wide)?;
        let (off, n) = (bf.bit_off, bf.bit_len);
        self.with_holder(&bf.holder, |h| {
            let bytes = match h {
                Value::Buffer(b) => b,
                _ => return Err(Error::Type("a buffer field of something not a buffer")),
            };
            for i in 0..n {
                set_bit(bytes, off + i, get_bit(&src, i));
            }
            Ok(())
        })
    }
}
