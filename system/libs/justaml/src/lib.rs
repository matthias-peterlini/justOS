//! An interpreter of AML, the bytecode in the ACPI tables (DSDT and SSDTs)
//! through which the firmware describes the computer's devices and how to
//! drive them: `_STA` says whether a device is there, `_BST` how full the
//! battery is, `_TMP` how hot a thermal zone is, `_PTS` and `\_S3` how to
//! put the machine to sleep.
//!
//! [`Interpreter::load_table`] builds the namespace from the tables,
//! [`Interpreter::initialize`] runs `_REG`, `_STA` and `_INI` the way ACPI
//! asks, and [`Interpreter::evaluate`] runs any method. The hardware (memory,
//! I/O ports, PCI configuration space, the embedded controller) is reached
//! through a [`Handler`], which the kernel provides.
//!
//! It follows ACPI 6.5, chapters 5, 19 and 20, and the behaviour of ACPICA
//! where firmware relies on it (method-local names, implicit conversions,
//! `_OSI` answering like Windows).

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(test)]
extern crate std;

mod exec;
mod field;
mod name;
mod namespace;
pub mod resource;
mod value;

#[cfg(test)]
mod tests;

pub use name::{seg_str, NameString, Seg};
pub use namespace::ObjectType;
pub use value::{eisa_id, Holder, Reference, Value};

use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use exec::Frame;
use namespace::{Builtin, Namespace, Object};

/// A node of the namespace.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub(crate) u32);

impl NodeId {
    pub const ROOT: NodeId = NodeId(0);
}

/// Address spaces of operation regions (ACPI 6.5, 19.6.100).
pub mod space {
    pub const MEMORY: u8 = 0;
    pub const IO: u8 = 1;
    pub const PCI: u8 = 2;
    pub const EMBEDDED_CONTROL: u8 = 3;
    pub const SMBUS: u8 = 4;
    pub const CMOS: u8 = 5;
    pub const PCI_BAR: u8 = 6;
    pub const IPMI: u8 = 7;
    pub const GPIO: u8 = 8;
    pub const SERIAL_BUS: u8 = 9;
    pub const PCC: u8 = 10;
}

/// A PCI function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PciAddress {
    pub segment: u16,
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

/// What the interpreter needs from the system it runs in.
pub trait Handler {
    /// Reads `bits` (8, 16, 32 or 64) at `addr` in an address space
    /// ([`space`]): memory, I/O ports, the embedded controller, CMOS...
    fn read(&mut self, space: u8, addr: u64, bits: u8) -> Result<u64, Error>;
    fn write(&mut self, space: u8, addr: u64, bits: u8, value: u64) -> Result<(), Error>;
    /// Reads the configuration space of a PCI function.
    fn read_pci(&mut self, pci: PciAddress, offset: u16, bits: u8) -> Result<u64, Error>;
    fn write_pci(&mut self, pci: PciAddress, offset: u16, bits: u8, value: u64) -> Result<(), Error>;
    /// Waits (the task may sleep).
    fn sleep(&mut self, ms: u64);
    /// Waits without giving the processor away.
    fn stall(&mut self, us: u64);
    /// A clock that only goes forward, in units of 100 ns.
    fn timer(&mut self) -> u64;
    /// A method called Notify on a device (0x80: status changed...).
    fn notify(&mut self, _node: NodeId, _path: &str, _value: u64) {}
    /// Something stored to the Debug object.
    fn debug(&mut self, _text: &str) {}
    /// A problem met while loading or running AML.
    fn log(&mut self, _text: &str) {}
    /// The firmware reports a fatal error.
    fn fatal(&mut self, _kind: u8, _code: u32, _arg: u64) {}
    /// An ACPI table for LoadTable, found by its signature and OEM ids.
    fn table(&mut self, _signature: &str, _oem: &str, _oem_table: &str) -> Option<Vec<u8>> {
        None
    }
    /// The address and length of an ACPI table, for DataTableRegion.
    fn table_address(&mut self, _signature: &str, _oem: &str, _oem_table: &str) -> Option<(u64, u64)> {
        None
    }
    /// `\_OSI`: does the system support this interface? By default it
    /// answers like Windows 11, which is what firmware is tested with.
    fn osi(&mut self, interface: &str) -> bool {
        OSI.contains(&interface)
    }
}

/// The interfaces `\_OSI` says yes to by default.
pub const OSI: &[&str] = &[
    "Windows 2000",
    "Windows 2001",
    "Windows 2001 SP1",
    "Windows 2001.1",
    "Windows 2001 SP2",
    "Windows 2001.1 SP1",
    "Windows 2006",
    "Windows 2006.1",
    "Windows 2006 SP1",
    "Windows 2006 SP2",
    "Windows 2009",
    "Windows 2012",
    "Windows 2013",
    "Windows 2015",
    "Windows 2016",
    "Windows 2017",
    "Windows 2017.2",
    "Windows 2018",
    "Windows 2018.2",
    "Windows 2019",
    "Windows 2020",
    "Windows 2021",
    "Windows 2022",
    "Module Device",
    "Processor Device",
    "3.0 Thermal Model",
    "3.0 _SCP Extensions",
    "Processor Aggregator Device",
    "Extended Address Space Descriptor",
];

#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    /// The code ends in the middle of something.
    Truncated,
    /// An opcode that does not exist, and where it is.
    BadOpcode(u16, usize),
    BadName(usize),
    NotFound(String),
    Exists(String),
    /// An operand of the wrong kind.
    Type(&'static str),
    /// An index past the end of a package, buffer or string.
    Range,
    DivideByZero,
    /// A While loop that never ends.
    Timeout,
    /// Methods calling methods too deeply.
    Depth,
    /// A reference to something that no longer exists.
    Dead,
    /// An operation region in an address space nobody handles.
    NoHandler(u8),
    /// The hardware did not answer.
    Hardware,
    /// The firmware asked to stop (Fatal).
    Fatal,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Truncated => f.write_str("the code ends too early"),
            Error::BadOpcode(op, at) => write!(f, "unknown opcode {op:#x} at {at:#x}"),
            Error::BadName(at) => write!(f, "bad name at {at:#x}"),
            Error::NotFound(n) => write!(f, "{n} does not exist"),
            Error::Exists(n) => write!(f, "{n} already exists"),
            Error::Type(what) => write!(f, "wrong kind of operand ({what})"),
            Error::Range => f.write_str("index out of range"),
            Error::DivideByZero => f.write_str("division by zero"),
            Error::Timeout => f.write_str("a loop never ended"),
            Error::Depth => f.write_str("methods nested too deeply"),
            Error::Dead => f.write_str("a reference to something gone"),
            Error::NoHandler(s) => write!(f, "no handler for address space {s}"),
            Error::Hardware => f.write_str("the hardware did not answer"),
            Error::Fatal => f.write_str("the firmware stopped with Fatal"),
        }
    }
}

/// What [`Interpreter::initialize`] did.
#[derive(Clone, Copy, Debug, Default)]
pub struct Init {
    pub devices: usize,
    pub present: usize,
    /// `_INI` methods run.
    pub ini: usize,
    /// `_REG` methods run.
    pub reg: usize,
    pub errors: usize,
}

pub struct Interpreter<H: Handler> {
    pub handler: H,
    pub(crate) ns: Namespace,
    pub(crate) tables: Vec<Arc<[u8]>>,
    pub(crate) frames: Vec<Frame>,
    /// The table whose code is running (method definitions point into it).
    pub(crate) table_now: u16,
    /// Integers are 64 bits (DSDT revision 2 and later), else 32.
    pub(crate) wide: bool,
    /// (device, address space) pairs whose `_REG` has run.
    reg_done: Vec<(NodeId, u8)>,
    /// Problems met while loading the tables.
    pub load_errors: usize,
}

impl<H: Handler> Interpreter<H> {
    pub fn new(handler: H) -> Self {
        let mut ns = Namespace::new();
        for s in [b"_GPE", b"_PR_", b"_SB_", b"_SI_", b"_TZ_"] {
            ns.add(NodeId::ROOT, *s, Object::Scope);
        }
        ns.add(NodeId::ROOT, *b"_OSI", Object::Builtin(Builtin::Osi));
        ns.add(NodeId::ROOT, *b"_OS_", Object::Data(Value::String("Microsoft Windows NT".into())));
        ns.add(NodeId::ROOT, *b"_REV", Object::Data(Value::Integer(2)));
        ns.add(NodeId::ROOT, *b"_GL_", Object::Mutex);
        Interpreter {
            handler,
            ns,
            tables: Vec::new(),
            frames: Vec::new(),
            table_now: 0,
            wide: true,
            reg_done: Vec::new(),
            load_errors: 0,
        }
    }

    /// Loads a DSDT or an SSDT, header included: its definitions join the
    /// namespace. Parts that fail are reported through [`Handler::log`] and
    /// skipped; the error returned is only for a table that is not one.
    pub fn load_table(&mut self, table: &[u8]) -> Result<(), Error> {
        if table.len() < 36 {
            return Err(Error::Truncated);
        }
        let len = (u32::from_le_bytes(table[4..8].try_into().unwrap()) as usize).min(table.len());
        if len < 36 {
            return Err(Error::Truncated);
        }
        if &table[..4] == b"DSDT" {
            self.wide = table[8] >= 2;
        }
        let code: Arc<[u8]> = Arc::from(&table[..len]);
        let index = self.tables.len() as u16;
        self.tables.push(code.clone());
        // A Load from inside a method: the new definitions are not the
        // method's own.
        let frames = core::mem::take(&mut self.frames);
        let table_now = core::mem::replace(&mut self.table_now, index);
        let r = self.term_list(&code, 36, len, NodeId::ROOT);
        self.table_now = table_now;
        self.frames = frames;
        r.map(|_| ())
    }

    /// Readies the namespace after the tables are loaded: runs `_REG` for
    /// memory, I/O and PCI regions, then `\_SB._INI`, then `_INI` of every
    /// device that `_STA` says is present (ACPI 6.5, 6.5.1).
    pub fn initialize(&mut self) -> Init {
        let mut s = Init::default();
        for sp in [space::MEMORY, space::IO, space::PCI] {
            s.reg += self.region_ready(sp);
        }
        if let Some(sb) = self.find("\\_SB") {
            if self.child(sb, "_INI").is_some() {
                match self.evaluate_child(sb, "_INI", &[]) {
                    Ok(_) => s.ini += 1,
                    Err(e) => {
                        self.complain_at(sb, "_INI", &e);
                        s.errors += 1;
                    }
                }
            }
        }
        self.init_devices(NodeId::ROOT, &mut s);
        s
    }

    fn init_devices(&mut self, node: NodeId, s: &mut Init) {
        let children = self.ns.get(node).map(|n| n.children.clone()).unwrap_or_default();
        for c in children {
            let kind = match self.ns.get(c).map(|n| &n.object) {
                Some(Object::Device | Object::Processor | Object::ThermalZone) => 1,
                Some(Object::Scope) => 2,
                _ => 0,
            };
            if kind == 2 {
                self.init_devices(c, s);
            }
            if kind != 1 {
                continue;
            }
            s.devices += 1;
            let sta = match self.evaluate_child(c, "_STA", &[]) {
                Ok(Some(v)) => value::to_integer(&v, true).unwrap_or(0),
                Ok(None) => 0x0F,
                Err(e) => {
                    self.complain_at(c, "_STA", &e);
                    s.errors += 1;
                    // Not present, but its children are still looked at.
                    0x08
                }
            };
            if sta & 1 != 0 {
                s.present += 1;
                if self.child(c, "_INI").is_some() {
                    match self.evaluate_child(c, "_INI", &[]) {
                        Ok(_) => s.ini += 1,
                        Err(e) => {
                            self.complain_at(c, "_INI", &e);
                            s.errors += 1;
                        }
                    }
                }
            }
            // Present or at least functioning: look at what is inside.
            if sta & 0x09 != 0 {
                self.init_devices(c, s);
            }
        }
    }

    /// Tells the firmware that an address space can now be used: runs
    /// `_REG(space, 1)` of every device with a region in it. Returns how
    /// many ran.
    pub fn region_ready(&mut self, space: u8) -> usize {
        let regions: Vec<NodeId> = self
            .ns
            .ids()
            .filter(|&n| matches!(self.ns.get(n).map(|x| &x.object), Some(Object::Region(r)) if r.space == space))
            .collect();
        let mut ran = 0;
        for r in regions {
            let Some(parent) = self.ns.get(r).map(|n| n.parent) else { continue };
            if self.reg_done.contains(&(parent, space)) {
                continue;
            }
            self.reg_done.push((parent, space));
            if self.child(parent, "_REG").is_none() {
                continue;
            }
            match self.evaluate_child(parent, "_REG", &[Value::Integer(space as u64), Value::Integer(1)]) {
                Ok(_) => ran += 1,
                Err(e) => self.complain_at(parent, "_REG", &e),
            }
        }
        ran
    }

    pub(crate) fn complain_at(&mut self, node: NodeId, what: &str, e: &Error) {
        let text = format!("{}.{what}: {e}", self.path(node));
        self.handler.log(&text);
    }

    /// The node at an absolute path (`\_SB.PCI0`).
    pub fn find(&self, path: &str) -> Option<NodeId> {
        let n = NameString::parse_str(path)?;
        let mut at = NodeId::ROOT;
        for s in &n.segs {
            at = self.unalias(self.ns.child(at, *s)?);
        }
        Some(at)
    }

    /// The node at a path below `node` (`_STA`, `EC0.BAT0`).
    pub fn child(&self, node: NodeId, path: &str) -> Option<NodeId> {
        let n = NameString::parse_str(path)?;
        let mut at = node;
        for s in &n.segs {
            at = self.unalias(self.ns.child(at, *s)?);
        }
        Some(at)
    }

    pub fn children(&self, node: NodeId) -> Vec<NodeId> {
        self.ns.get(node).map(|n| n.children.clone()).unwrap_or_default()
    }

    pub fn parent(&self, node: NodeId) -> Option<NodeId> {
        (node != NodeId::ROOT).then(|| self.ns.get(node).map(|n| n.parent)).flatten()
    }

    /// The node's own name (`BAT0`).
    pub fn name(&self, node: NodeId) -> String {
        match self.ns.get(node) {
            Some(_) if node == NodeId::ROOT => "\\".into(),
            Some(n) => seg_str(&n.name).into(),
            None => "?".into(),
        }
    }

    /// The node's absolute path (`\_SB.PCI0.LPCB.EC0.BAT0`).
    pub fn path(&self, node: NodeId) -> String {
        let mut segs = Vec::new();
        let mut at = node;
        while at != NodeId::ROOT {
            let Some(n) = self.ns.get(at) else { return "?".into() };
            segs.push(seg_str(&n.name).to_string());
            at = n.parent;
            if segs.len() > 64 {
                break;
            }
        }
        segs.reverse();
        format!("\\{}", segs.join("."))
    }

    pub fn object_type(&self, node: NodeId) -> ObjectType {
        let Some(n) = self.ns.get(node) else { return ObjectType::Uninitialized };
        match &n.object {
            Object::Scope => ObjectType::Scope,
            Object::Data(v) => match v {
                Value::Uninitialized => ObjectType::Uninitialized,
                Value::Integer(_) => ObjectType::Integer,
                Value::String(_) => ObjectType::String,
                Value::Buffer(_) => ObjectType::Buffer,
                Value::Package(_) => ObjectType::Package,
                _ => ObjectType::Reference,
            },
            Object::Device => ObjectType::Device,
            Object::Processor => ObjectType::Processor,
            Object::PowerResource => ObjectType::PowerResource,
            Object::ThermalZone => ObjectType::ThermalZone,
            Object::Method(_) | Object::Builtin(_) => ObjectType::Method,
            Object::Mutex => ObjectType::Mutex,
            Object::Event(_) => ObjectType::Event,
            Object::Region(_) => ObjectType::Region,
            Object::Field(_) => ObjectType::Field,
            Object::BufferField(_) => ObjectType::BufferField,
            Object::Alias(t) => self.object_type(*t),
        }
    }

    /// How many named objects there are.
    pub fn node_count(&self) -> usize {
        self.ns.len()
    }

    /// Every node, parents before their children.
    pub fn nodes(&self) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = alloc::vec![NodeId::ROOT];
        while let Some(n) = stack.pop() {
            out.push(n);
            if let Some(node) = self.ns.get(n) {
                stack.extend(node.children.iter().rev());
            }
        }
        out
    }

    /// Runs the method at an absolute path, or reads the object there.
    pub fn evaluate(&mut self, path: &str, args: &[Value]) -> Result<Value, Error> {
        let node = self.find(path).ok_or_else(|| Error::NotFound(path.into()))?;
        self.evaluate_node(node, args)
    }

    /// Runs a method, or reads an object.
    pub fn evaluate_node(&mut self, node: NodeId, args: &[Value]) -> Result<Value, Error> {
        let node = self.unalias(node);
        let v = match self.ns.get(node).map(|n| &n.object) {
            Some(Object::Method(_) | Object::Builtin(_)) => self.call(node, args.to_vec(), [None; 7])?,
            Some(_) => self.value_of_node(node)?,
            None => return Err(Error::Dead),
        };
        Ok(self.resolve_deep(v, 0))
    }

    /// Runs (or reads) the object `name` of a device; None if it has none.
    pub fn evaluate_child(&mut self, node: NodeId, name: &str, args: &[Value]) -> Result<Option<Value>, Error> {
        match self.child(node, name) {
            Some(c) => self.evaluate_node(c, args).map(Some),
            None => Ok(None),
        }
    }

    /// An integer from a device's object (`_STA`, `_ADR`), if it has it and
    /// it runs.
    pub fn integer(&mut self, node: NodeId, name: &str) -> Option<u64> {
        let v = self.evaluate_child(node, name, &[]).ok()??;
        value::to_integer(&v, true).ok()
    }

    /// `_STA` of a device: 0x0F (present, enabled, shown, working) if it
    /// has none.
    pub fn status(&mut self, node: NodeId) -> u64 {
        match self.evaluate_child(node, "_STA", &[]) {
            Ok(Some(v)) => value::to_integer(&v, true).unwrap_or(0),
            Ok(None) => 0x0F,
            Err(_) => 0,
        }
    }

    /// The hardware ids of a device: its `_HID` and `_CID`s ("PNP0C0A",
    /// "ACPI0003").
    pub fn ids(&mut self, node: NodeId) -> Vec<String> {
        let mut out = Vec::new();
        for name in ["_HID", "_CID"] {
            if let Ok(Some(v)) = self.evaluate_child(node, name, &[]) {
                push_ids(&v, &mut out);
            }
        }
        out
    }

    /// Every device whose `_HID` or `_CID` is one of `ids`.
    pub fn find_devices(&mut self, ids: &[&str]) -> Vec<NodeId> {
        let devices: Vec<NodeId> = self
            .nodes()
            .into_iter()
            .filter(|&n| matches!(self.ns.get(n).map(|x| &x.object), Some(Object::Device | Object::ThermalZone)))
            .collect();
        devices.into_iter().filter(|&d| self.ids(d).iter().any(|i| ids.contains(&i.as_str()))).collect()
    }

    /// Every node of a kind.
    pub fn nodes_of(&self, kind: ObjectType) -> Vec<NodeId> {
        self.nodes().into_iter().filter(|&n| self.object_type(n) == kind).collect()
    }

    /// SLP_TYPa and SLP_TYPb of a sleep state, from `\_S0`...`\_S5`.
    pub fn sleep_type(&mut self, state: u8) -> Option<(u8, u8)> {
        let path = format!("\\_S{state}");
        let v = self.evaluate(&path, &[]).ok()?;
        let p = v.as_package()?;
        let a = value::to_integer(p.first()?, true).ok()?;
        let b = p.get(1).and_then(|x| value::to_integer(x, true).ok()).unwrap_or(a);
        Some((a as u8, b as u8))
    }

    /// 32 or 64.
    pub fn integer_bits(&self) -> u32 {
        if self.wide {
            64
        } else {
            32
        }
    }
}

fn push_ids(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Integer(x) => out.push(eisa_id(*x as u32)),
        Value::String(s) => out.push(s.clone()),
        Value::Package(p) => p.iter().for_each(|x| push_ids(x, out)),
        _ => {}
    }
}
