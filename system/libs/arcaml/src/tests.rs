//! Tests against tables made with iasl from testdata/*.asl (the expected
//! values were checked with ACPICA's acpiexec), and optionally against the
//! tables of real computers.

use crate::resource::{self, Resource};
use crate::*;
use std::collections::BTreeMap;
use std::string::String;
use std::vec::Vec;
use std::{format, println, vec};

const BASIC: &[u8] = include_bytes!("testdata/basic.aml");
const NARROW: &[u8] = include_bytes!("testdata/narrow.aml");

/// A pretend computer: memory, I/O ports with a CMOS at 0x70, an embedded
/// controller and PCI configuration space, all reading 0 until written.
struct Mock {
    mem: BTreeMap<u64, u8>,
    io: BTreeMap<u64, u8>,
    cmos: [u8; 256],
    cmos_index: u8,
    ec: [u8; 256],
    pci: BTreeMap<(u8, u8, u8, u16), u8>,
    clock: u64,
    notes: Vec<(String, u64)>,
    debug: Vec<String>,
    log: Vec<String>,
    /// For real tables: unknown address spaces read 0 instead of failing.
    lenient: bool,
}

impl Mock {
    fn new() -> Mock {
        Mock {
            mem: BTreeMap::new(),
            io: BTreeMap::new(),
            cmos: [0; 256],
            cmos_index: 0,
            ec: [0; 256],
            pci: BTreeMap::new(),
            clock: 0,
            notes: Vec::new(),
            debug: Vec::new(),
            log: Vec::new(),
            lenient: false,
        }
    }

    fn read8(&mut self, space: u8, addr: u64) -> Result<u8, Error> {
        Ok(match space {
            space::MEMORY => self.mem.get(&addr).copied().unwrap_or(0),
            space::IO if addr == 0x70 => self.cmos_index,
            space::IO if addr == 0x71 => self.cmos[self.cmos_index as usize],
            space::IO => self.io.get(&addr).copied().unwrap_or(0),
            space::EMBEDDED_CONTROL => self.ec[addr as usize & 0xFF],
            space::CMOS => self.cmos[addr as usize & 0xFF],
            _ if self.lenient => 0,
            s => return Err(Error::NoHandler(s)),
        })
    }

    fn write8(&mut self, space: u8, addr: u64, v: u8) -> Result<(), Error> {
        match space {
            space::MEMORY => {
                self.mem.insert(addr, v);
            }
            space::IO if addr == 0x70 => self.cmos_index = v,
            space::IO if addr == 0x71 => self.cmos[self.cmos_index as usize] = v,
            space::IO => {
                self.io.insert(addr, v);
            }
            space::EMBEDDED_CONTROL => self.ec[addr as usize & 0xFF] = v,
            space::CMOS => self.cmos[addr as usize & 0xFF] = v,
            _ if self.lenient => {}
            s => return Err(Error::NoHandler(s)),
        }
        Ok(())
    }
}

impl Handler for Mock {
    fn read(&mut self, space: u8, addr: u64, bits: u8) -> Result<u64, Error> {
        let mut v = 0;
        for i in (0..bits as u64 / 8).rev() {
            v = v << 8 | self.read8(space, addr + i)? as u64;
        }
        Ok(v)
    }

    fn write(&mut self, space: u8, addr: u64, bits: u8, value: u64) -> Result<(), Error> {
        for i in 0..bits as u64 / 8 {
            self.write8(space, addr + i, (value >> (8 * i)) as u8)?;
        }
        Ok(())
    }

    fn read_pci(&mut self, p: PciAddress, offset: u16, bits: u8) -> Result<u64, Error> {
        let mut v = 0;
        for i in (0..bits as u16 / 8).rev() {
            v = v << 8 | *self.pci.get(&(p.bus, p.device, p.function, offset + i)).unwrap_or(&0) as u64;
        }
        Ok(v)
    }

    fn write_pci(&mut self, p: PciAddress, offset: u16, bits: u8, value: u64) -> Result<(), Error> {
        for i in 0..bits as u16 / 8 {
            self.pci.insert((p.bus, p.device, p.function, offset + i), (value >> (8 * i)) as u8);
        }
        Ok(())
    }

    fn sleep(&mut self, ms: u64) {
        self.clock += ms * 10_000;
    }

    fn stall(&mut self, us: u64) {
        self.clock += us * 10;
    }

    fn timer(&mut self) -> u64 {
        self.clock += 1_000_000;
        self.clock
    }

    fn notify(&mut self, _node: NodeId, path: &str, value: u64) {
        self.notes.push((path.into(), value));
    }

    fn debug(&mut self, text: &str) {
        self.debug.push(text.into());
    }

    fn log(&mut self, text: &str) {
        self.log.push(text.into());
    }
}

fn load(aml: &[u8]) -> Interpreter<Mock> {
    let mut i = Interpreter::new(Mock::new());
    i.load_table(aml).unwrap();
    assert_eq!(i.load_errors, 0, "{:?}", i.handler.log);
    i
}

fn basic() -> Interpreter<Mock> {
    let mut i = load(BASIC);
    i.handler.cmos[0x10] = 0x21;
    i.handler.cmos[0x12] = 0x03;
    i.handler.ec[0x10] = 2;
    i.handler.ec[0x11] = 0x34;
    i.handler.ec[0x12] = 0x12;
    // A PCI bridge at 00:1c.0 with secondary bus 3, and 03:00.2 behind it.
    i.handler.pci.insert((0, 0x1C, 0, 0x0E), 0x01);
    i.handler.pci.insert((0, 0x1C, 0, 0x19), 3);
    for (k, b) in [0x86u8, 0x80, 0x34, 0x12].into_iter().enumerate() {
        i.handler.pci.insert((3, 0, 2, k as u16), b);
    }
    i
}

fn int(i: &mut Interpreter<Mock>, path: &str) -> u64 {
    match i.evaluate(path, &[]) {
        Ok(Value::Integer(v)) => v,
        Ok(other) => panic!("{path} gave {other}"),
        Err(e) => panic!("{path}: {e} {:?}", i.handler.log),
    }
}

#[test]
fn operators() {
    let mut i = basic();
    for (t, want) in [
        ("\\T01", 0x7FF),
        ("\\T02", 0x7),
        ("\\T03", 0x1FFF),
        ("\\T04", 0xFF),
        ("\\T05", 0x7F),
        ("\\T07", 0xF),
        ("\\T15", 0x3FF),
        ("\\T16", 0xF),
        ("\\T17", 0x1F),
    ] {
        assert_eq!(int(&mut i, t), want, "{t}: {:x}", int(&mut i, t));
    }
    // Twice: method-local names are gone after each run.
    assert_eq!(int(&mut i, "\\T05"), 0x7F);
    assert!(i.handler.debug.iter().any(|d| d == "\"dbg\""));
}

#[test]
fn switch_and_args() {
    let mut i = basic();
    for (arg, want) in [(1, "one"), (3, "two-three"), (2, "two-three"), (9, "other")] {
        let v = i.evaluate("\\T06", &[Value::Integer(arg)]).unwrap();
        assert_eq!(v.as_str(), Some(want));
    }
    assert_eq!(int(&mut i, "\\T12"), 130);
    assert_eq!(int(&mut i, "\\T13"), 0x55);
    assert_eq!(int(&mut i, "\\T14"), 0xEE);
    assert!(i.find("\\MLN.LOCN").is_none());
}

#[test]
fn fields() {
    let mut i = basic();
    let v = i.evaluate("\\T08", &[]).unwrap();
    let p = v.as_package().unwrap();
    assert_eq!(p[0], Value::Integer(0xAB));
    assert_eq!(p[1], Value::Integer(0x1122_3344));
    assert_eq!(p[2], Value::Integer(0x0102_0304_0506_0708));
    assert_eq!(p[3], Value::Buffer(vec![0; 16]));
    assert_eq!(p[4], Value::Integer(0));
    let m = &i.handler.mem;
    assert_eq!(m[&0x1004], 0x44);
    assert_eq!(m[&0x1007], 0x11);
    assert_eq!(m[&0x1010], 0x18);
    assert_eq!(m[&0x1020], 0x08);
    assert_eq!(m[&0x1027], 0x01);
    // WriteAsOnes: the low nibble the field does not cover is set.
    assert_eq!(m[&0x1040], 0x5F);

    assert_eq!(int(&mut i, "\\T09"), 0x5A21);
    assert_eq!(i.handler.cmos[0x11], 0x5A);
    assert_eq!(i.handler.cmos[0x12], 0xC3);

    assert_eq!(int(&mut i, "\\T10"), 0x44);
    assert_eq!(i.handler.io[&0x200], 2);
    assert_eq!(i.handler.io[&0x201], 0x44);

    assert_eq!(int(&mut i, "\\_SB.PCI0.BRG1.DEV0.VDID"), 0x1234_8086);
}

#[test]
fn devices() {
    let mut i = basic();
    assert_eq!(int(&mut i, "\\MLC"), 1);
    let s = i.initialize();
    assert_eq!(int(&mut i, "\\INIC"), 0x100111);
    assert_eq!(s.errors, 0, "{:?}", i.handler.log);
    assert_eq!(s.ini, 4);

    let deva = i.find("\\_SB.DEVA").unwrap();
    assert_eq!(i.ids(deva), ["ARC0001"]);
    assert_eq!(i.path(deva), "\\_SB.DEVA");
    assert_eq!(i.object_type(deva), ObjectType::Device);
    assert_eq!(int(&mut i, "\\_SB.DEVA.UPS"), 1);
    let bats = i.find_devices(&["PNP0C0A"]);
    let paths: Vec<String> = bats.iter().map(|&b| i.path(b)).collect();
    assert_eq!(paths, ["\\_SB.DEVA.DEVB", "\\_SB.EC0.BAT0"]);

    assert_eq!(int(&mut i, "\\T11"), 1);
    assert_eq!(i.handler.notes, [("\\_SB.DEVA.DEVB".into(), 0x80)]);

    assert_eq!(i.sleep_type(5), Some((5, 5)));
    assert_eq!(i.sleep_type(3), Some((1, 1)));
    assert_eq!(i.sleep_type(4), None);

    let tz = i.nodes_of(ObjectType::ThermalZone);
    assert_eq!(tz.len(), 1);
    assert_eq!(i.integer(tz[0], "_TMP"), Some(3132));

    // A package naming a device and some data.
    let v = i.evaluate("\\_SB.PKGN", &[]).unwrap();
    assert_eq!(v.as_package().unwrap()[0], Value::Node(deva));
    assert_eq!(v.as_package().unwrap()[1], Value::Integer(0x10));
}

#[test]
fn embedded_controller() {
    let mut i = basic();
    i.initialize();
    let ec = i.find_devices(&["PNP0C09"])[0];
    assert_eq!(i.integer(ec, "REGC"), Some(0));
    assert_eq!(i.region_ready(space::EMBEDDED_CONTROL), 1);
    assert_eq!(i.integer(ec, "REGC"), Some(1));
    assert_eq!(i.integer(ec, "_GPE"), Some(0x17));
    let crs = i.evaluate_child(ec, "_CRS", &[]).unwrap().unwrap();
    let r = resource::parse(crs.as_buffer().unwrap());
    assert_eq!(resource::io_ports(&r), [0x62, 0x66]);
    assert_eq!(r[0], Resource::Io { min: 0x62, max: 0x62, len: 1 });

    let bst = i.evaluate("\\_SB.EC0.BAT0._BST", &[]).unwrap();
    let want: Vec<Value> = [2, 1000, 0x1234, 12000].into_iter().map(Value::Integer).collect();
    assert_eq!(bst.as_package().unwrap(), want);

    i.evaluate("\\_SB.EC0._Q20", &[]).unwrap();
    assert_eq!(i.handler.notes, [("\\_SB.EC0.BAT0".into(), 0x80)]);
}

#[test]
fn errors() {
    let mut i = basic();
    assert_eq!(i.evaluate("\\TINF", &[]), Err(Error::Timeout));
    assert_eq!(i.evaluate("\\RECU", &[Value::Integer(0)]), Err(Error::Depth));
    assert_eq!(i.evaluate("\\TDIV", &[Value::Integer(0)]), Err(Error::DivideByZero));
    assert_eq!(i.evaluate("\\TDIV", &[Value::Integer(2)]), Ok(Value::Integer(5)));
    assert_eq!(i.evaluate("\\TIDX", &[]), Err(Error::Range));
    assert!(matches!(i.evaluate("\\NOPE", &[]), Err(Error::NotFound(_))));
    // Everything still works after the failures.
    assert_eq!(int(&mut i, "\\T12"), 130);
    assert!(i.frames.is_empty());
}

#[test]
fn narrow_integers() {
    let mut i = load(NARROW);
    assert_eq!(i.integer_bits(), 32);
    assert_eq!(int(&mut i, "\\T32"), 0xF);
}

#[test]
fn broken_tables() {
    // Garbage inside a method only fails when the method runs; garbage
    // in a scope is skipped up to the end of the scope.
    let mut t = BASIC.to_vec();
    let at = t.windows(4).position(|w| w == b"TDIV").unwrap();
    // Corrupt TDIV's body (after name and flags).
    t[at + 5] = 0xFE;
    t[at + 6] = 0xFE;
    let mut i = Interpreter::new(Mock::new());
    i.load_table(&t).unwrap();
    assert!(i.evaluate("\\TDIV", &[Value::Integer(2)]).is_err());
    assert_eq!(int(&mut i, "\\T12"), 130);
    // Truncated table: whatever loads is usable, nothing panics.
    for cut in [40, 100, 500, 1000, 2000, BASIC.len() - 1] {
        let mut t = BASIC[..cut].to_vec();
        t[4..8].copy_from_slice(&(cut as u32).to_le_bytes());
        let mut i = Interpreter::new(Mock::new());
        let _ = i.load_table(&t);
        i.initialize();
    }
}

#[test]
fn eisa_ids() {
    assert_eq!(eisa_id(0x0A0CD041), "PNP0C0A");
    assert_eq!(eisa_id(0x030AD041), "PNP0A03");
}

#[test]
fn resource_templates() {
    // IRQ(Edge, ActiveHigh, Exclusive){1}, FixedIO(0x60,1),
    // Memory32Fixed(ReadWrite, 0xFED00000, 0x400), ExtendedIrq 9, end.
    let t = [
        0x23, 0x02, 0x00, 0x01, 0x4B, 0x60, 0x00, 0x01, 0x86, 0x09, 0x00, 0x01, 0x00, 0x00, 0xD0, 0xFE, 0x00, 0x04, 0x00,
        0x00, 0x89, 0x06, 0x00, 0x0D, 0x01, 0x09, 0x00, 0x00, 0x00, 0x79, 0x00,
    ];
    let r = resource::parse(&t);
    assert_eq!(r.len(), 4);
    assert_eq!(r[0], Resource::Irq { irqs: vec![1], edge: true, active_low: false, shared: false });
    assert_eq!(r[1], Resource::Io { min: 0x60, max: 0x60, len: 1 });
    assert_eq!(r[2], Resource::Memory { base: 0xFED0_0000, len: 0x400, writable: true });
    assert_eq!(r[3], Resource::Irq { irqs: vec![9], edge: false, active_low: true, shared: true });
    // A descriptor running past the end is dropped.
    assert_eq!(resource::parse(&t[..12]).len(), 2);
}

/// Loads the tables of real computers if ARCAML_TABLES names a folder with
/// one folder per computer (dsdt.dat, ssdt*.dat as acpixtract makes them),
/// and shows what it finds. Nothing to do otherwise.
#[test]
fn real_tables() {
    let Ok(dir) = std::env::var("ARCAML_TABLES") else { return };
    let mut machines: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).collect();
    machines.sort();
    for m in machines {
        let mut files: Vec<_> = std::fs::read_dir(&m)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                let n = p.file_name().unwrap().to_string_lossy().to_lowercase();
                n.ends_with(".dat") && (n.starts_with("dsdt") || n.starts_with("ssdt"))
            })
            .collect();
        files.sort_by_key(|p| !p.file_name().unwrap().to_string_lossy().to_lowercase().starts_with("dsdt"));
        let mut mock = Mock::new();
        mock.lenient = true;
        let mut i = Interpreter::new(mock);
        for f in &files {
            let t = std::fs::read(f).unwrap();
            if let Err(e) = i.load_table(&t) {
                println!("  {}: {e}", f.display());
            }
        }
        let loaded = i.node_count();
        let errs = i.load_errors;
        let s = i.initialize();
        for sp in [space::EMBEDDED_CONTROL, space::CMOS] {
            i.region_ready(sp);
        }
        println!(
            "{}: {} tables, {loaded} names, {errs} load errors; {} devices, {} present, {} _INI, {} _REG, {} errors",
            m.file_name().unwrap().to_string_lossy(),
            files.len(),
            s.devices,
            s.present,
            s.ini,
            s.reg,
            s.errors
        );
        for l in i.handler.log.iter().take(8) {
            println!("    log: {l}");
        }
        let mut out = Vec::new();
        for b in i.find_devices(&["PNP0C0A"]) {
            let sta = i.status(b);
            let mut line = format!("battery {} _STA={sta:#x}", i.path(b));
            for m in ["_BIX", "_BIF", "_BST"] {
                if let Some(r) = i.evaluate_child(b, m, &[]).transpose() {
                    line += &format!(" {m}={}", r.map(|v| v.to_string()).unwrap_or_else(|e| format!("error: {e}")));
                }
            }
            out.push(line);
        }
        for a in i.find_devices(&["ACPI0003"]) {
            let r = i.evaluate_child(a, "_PSR", &[]);
            out.push(format!("ac {} _PSR={r:?}", i.path(a)));
        }
        for l in i.find_devices(&["PNP0C0D"]) {
            let r = i.evaluate_child(l, "_LID", &[]);
            out.push(format!("lid {} _LID={r:?}", i.path(l)));
        }
        for e in i.find_devices(&["PNP0C09"]) {
            let crs = i.evaluate_child(e, "_CRS", &[]).ok().flatten();
            let ports = crs.as_ref().and_then(|c| c.as_buffer()).map(|b| resource::io_ports(&resource::parse(b)));
            let gpe = i.integer(e, "_GPE");
            out.push(format!("ec {} ports={ports:x?} _GPE={gpe:x?}", i.path(e)));
        }
        for tz in i.nodes_of(ObjectType::ThermalZone) {
            let r = i.evaluate_child(tz, "_TMP", &[]);
            out.push(format!("thermal {} _TMP={r:?}", i.path(tz)));
        }
        for st in 0..=5 {
            if let Some(t) = i.sleep_type(st) {
                out.push(format!("S{st} = {t:?}"));
            }
        }
        for l in out {
            println!("    {l}");
        }
        for l in i.handler.log.iter().skip(8).take(4) {
            println!("    later log: {l}");
        }
    }
}
