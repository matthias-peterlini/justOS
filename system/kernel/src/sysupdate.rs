//! System updates with two slots (A/B).
//!
//! The disk holds the system twice, in `ARCOS/SLOTA.BIN` and `ARCOS/SLOTB.BIN`
//! on the EFI system partition (the layout is in tools/arctool/src/slot.rs);
//! `ARCOS/BOOT.CTL` says which one the loader starts. An update is written
//! into the slot that is not running, read back and checked, and becomes
//! the one that starts next, on trial: it gets [`TRIES`] starts. Once it has
//! run for [`CONFIRM_AFTER_MS`] it is known to work. If it stops before (a
//! kernel panic restarts the machine while on trial) or never comes up, the
//! loader goes back to the other slot when the tries are used up, and the
//! old system says so.
//!
//! The running system is never written, so a power cut in the middle of an
//! update leaves it as it was.

use crate::boot::{SLOT_FALLBACK, SLOT_TRIAL};
use crate::dev::block::{self, Disk};
use crate::sync::{IrqLock, Mutex};
use crate::util::Size;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use arctls::sha2::Sha256;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

const SECTOR: usize = 512;
const SLOT_MAGIC: &[u8; 8] = b"ARCSLOT1";
const CTL_MAGIC: &[u8; 8] = b"ARCCTL01";
/// The MBR boot table: BOOT.CTL's sector, and the table's magic.
const MBR_CTL: usize = 0x18C;
const MBR_MAGIC: usize = 0x190;
/// Starts a new system gets before the loader goes back to the other one.
pub const TRIES: u8 = 2;
/// How long a new system must run to be known to work.
const CONFIRM_AFTER_MS: u64 = 15_000;
/// Sectors written or read at a time.
const CHUNK: usize = 128;

/// `BootInfo.slot`.
static BOOTED: AtomicU32 = AtomicU32::new(0);
static CONFIRMED: AtomicBool = AtomicBool::new(false);
/// One change to the slots at a time.
static WORKING: Mutex<()> = Mutex::new(());
static SLOTS: IrqLock<Slots> = IrqLock::new(Slots { previous: None, next: None });

fn get32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn sum(sector: &[u8]) -> u32 {
    (0..SECTOR / 4).fold(0u32, |s, i| s.wrapping_add(get32(sector, i * 4)))
}

/// Sets the last dword so the sector's dwords add up to zero.
fn seal(sector: &mut [u8]) {
    put32(sector, 0x1FC, 0);
    let s = sum(sector);
    put32(sector, 0x1FC, s.wrapping_neg());
}

fn check(st: arcdk::Status, what: &str) -> Result<(), String> {
    if st == arcdk::OK {
        Ok(())
    } else {
        Err(format!("{what}: errore del disco ({st})"))
    }
}

/// Records how this system started (`BootInfo.slot`), first thing: a panic
/// on trial restarts from then on.
pub fn init(slot: u32) {
    BOOTED.store(slot, Ordering::Relaxed);
}

/// The slot this system started from (0 = A, 1 = B), if it started from one
/// (not from a CD, or a disk made before slots).
pub fn booted() -> Option<usize> {
    match BOOTED.load(Ordering::Relaxed) & 0xFF {
        1 => Some(0),
        2 => Some(1),
        _ => None,
    }
}

/// Whether updates can be installed here.
pub fn supported() -> bool {
    booted().is_some()
}

pub fn name(slot: usize) -> char {
    if slot == 0 { 'A' } else { 'B' }
}

/// A new system that has not yet shown it works.
pub fn on_trial() -> bool {
    BOOTED.load(Ordering::Relaxed) & SLOT_TRIAL != 0 && !CONFIRMED.load(Ordering::Relaxed)
}

/// For the panic screen: a system on trial restarts after a moment, so the
/// loader uses up its tries and goes back to the version that worked.
pub fn restart_after_panic() {
    if on_trial() {
        crate::time::spin_us(4_000_000);
        crate::acpi::reset();
    }
}

// ----------------------------------------------------------------------------
// The disk

/// Where the slots are: the disk the system volume is on.
struct Layout {
    disk: Arc<Disk>,
    ctl_lba: u64,
}

/// BOOT.CTL.
struct Control {
    b: Vec<u8>,
}

impl Control {
    fn lba(&self, slot: usize) -> u64 {
        get32(&self.b, 0x08 + 4 * slot) as u64
    }

    fn sectors(&self) -> u64 {
        get32(&self.b, 0x10) as u64
    }

    fn active(&self) -> usize {
        (self.b[0x14] & 1) as usize
    }

    fn tries(&self) -> u8 {
        self.b[0x15]
    }

    fn good(&self, slot: usize) -> bool {
        self.b[0x16] & (1 << slot) != 0
    }

    fn set_active(&mut self, slot: usize, tries: u8) {
        self.b[0x14] = slot as u8;
        self.b[0x15] = tries;
    }

    fn set_good(&mut self, slot: usize, good: bool) {
        if good {
            self.b[0x16] |= 1 << slot;
        } else {
            self.b[0x16] &= !(1 << slot);
        }
    }
}

/// The header of a slot image.
struct Header {
    version: String,
    /// Sectors, the header included.
    total: u64,
    sha256: [u8; 32],
}

impl Header {
    fn parse(image: &[u8]) -> Option<Header> {
        let h = image.get(..SECTOR)?;
        if &h[..8] != SLOT_MAGIC || sum(h) != 0 {
            return None;
        }
        let (kernel, pack_at, pack, total) = (get32(h, 0x08), get32(h, 0x18), get32(h, 0x1C), get32(h, 0x28));
        // The kernel, then the boot pack, inside the image.
        if kernel == 0 || pack == 0 || pack_at < 1 + kernel || pack_at.checked_add(pack)? > total {
            return None;
        }
        let v = &h[0x50..0x70];
        let len = v.iter().position(|&b| b == 0).unwrap_or(v.len());
        Some(Header { version: String::from_utf8_lossy(&v[..len]).into_owned(), total: total as u64, sha256: h[0x30..0x50].try_into().ok()? })
    }
}

fn layout() -> Result<Layout, String> {
    let v = crate::fs::volume("sys").ok_or("non c'è il volume di sistema")?;
    let part = block::find(&v.source).ok_or("il disco di sistema non c'è")?;
    let disk = match part.parent.clone() {
        Some((d, _)) => d,
        None => part,
    };
    if disk.sector_size as usize != SECTOR {
        return Err("il disco di sistema non ha settori da 512 byte".to_string());
    }
    let mut mbr = vec![0u8; SECTOR];
    check(disk.read(0, &mut mbr), "MBR")?;
    let ctl_lba = get32(&mbr, MBR_CTL) as u64;
    if &mbr[MBR_MAGIC..MBR_MAGIC + 4] != b"ARCB" || ctl_lba == 0 || ctl_lba >= disk.sectors {
        return Err("il disco di sistema non ha gli slot di arcOS".to_string());
    }
    Ok(Layout { disk, ctl_lba })
}

impl Layout {
    fn control(&self) -> Result<Control, String> {
        let mut b = vec![0u8; SECTOR];
        check(self.disk.read(self.ctl_lba, &mut b), "BOOT.CTL")?;
        if &b[..8] != CTL_MAGIC || sum(&b) != 0 {
            return Err("BOOT.CTL è danneggiato".to_string());
        }
        let c = Control { b };
        if (0..2).any(|s| c.lba(s) == 0 || c.lba(s) + c.sectors() > self.disk.sectors) || c.sectors() < 2 {
            return Err("BOOT.CTL non corrisponde al disco".to_string());
        }
        Ok(c)
    }

    fn save(&self, c: &mut Control) -> Result<(), String> {
        let seq = get32(&c.b, 0x18).wrapping_add(1);
        put32(&mut c.b, 0x18, seq);
        seal(&mut c.b);
        check(self.disk.write(self.ctl_lba, &c.b), "BOOT.CTL")?;
        check(self.disk.flush(), "BOOT.CTL")
    }

    fn header(&self, c: &Control, slot: usize) -> Option<Header> {
        let mut b = vec![0u8; SECTOR];
        (self.disk.read(c.lba(slot), &mut b) == arcdk::OK).then_some(())?;
        Header::parse(&b)
    }
}

// ----------------------------------------------------------------------------
// What the shell and Info sistema use

/// What the other slot means for the user.
#[derive(Clone, Default)]
pub struct Slots {
    /// The version in the other slot, if it has worked (to go back to).
    pub previous: Option<String>,
    /// The version that starts at the next restart, when it is not this one.
    pub next: Option<String>,
}

pub fn slots() -> Slots {
    SLOTS.lock().clone()
}

/// One slot, as [`status`] sees it.
pub struct SlotStatus {
    pub version: Option<String>,
    pub good: bool,
    pub size: u64,
}

pub struct Status {
    pub booted: usize,
    pub active: usize,
    /// Starts left to the active slot while it is on trial.
    pub tries: u8,
    pub confirmed: bool,
    pub fallback: bool,
    pub slots: [SlotStatus; 2],
}

pub fn status() -> Result<Status, String> {
    let me = booted().ok_or("questo sistema non è partito da uno slot")?;
    let l = layout()?;
    let c = l.control()?;
    let slot = |s: usize| SlotStatus { version: l.header(&c, s).map(|h| h.version), good: c.good(s), size: c.sectors() * SECTOR as u64 };
    Ok(Status {
        booted: me,
        active: c.active(),
        tries: c.tries(),
        confirmed: !on_trial(),
        fallback: BOOTED.load(Ordering::Relaxed) & SLOT_FALLBACK != 0,
        slots: [slot(0), slot(1)],
    })
}

/// Reads the slots again for [`slots`].
fn refresh() {
    let Some(me) = booted() else { return };
    let s = (|| {
        let l = layout().ok()?;
        let c = l.control().ok()?;
        let other = l.header(&c, 1 - me).map(|h| h.version);
        Some(Slots { previous: other.clone().filter(|_| c.good(1 - me)), next: other.filter(|_| c.active() != me) })
    })();
    *SLOTS.lock() = s.unwrap_or_default();
}

/// Checks a system image (an `arcos-<version>.slot` file) and writes it into
/// the slot that is not running; it starts at the next restart, on trial.
/// `progress(done, total)`. Returns its version.
pub fn install(image: &[u8], progress: &mut dyn FnMut(u64, u64)) -> Result<String, String> {
    let _one = WORKING.try_lock().ok_or("sto già cambiando gli slot di sistema")?;
    let me = booted().ok_or("questo sistema non è partito da uno slot (un CD, o un disco senza slot)")?;
    if on_trial() {
        return Err("questa versione è ancora in prova: riprova tra qualche secondo".to_string());
    }
    let h = Header::parse(image).ok_or("non è un'immagine di sistema di arcOS")?;
    let bytes = h.total as usize * SECTOR;
    if image.len() < bytes {
        return Err("l'immagine di sistema è troncata".to_string());
    }
    if Sha256::digest(&image[SECTOR..bytes]) != h.sha256 {
        return Err("l'immagine di sistema è danneggiata".to_string());
    }
    let l = layout()?;
    let mut c = l.control()?;
    if h.total > c.sectors() {
        return Err(format!("l'immagine ({}) non entra nello slot ({})", Size(bytes as u64), Size(c.sectors() * SECTOR as u64)));
    }
    let target = 1 - me;
    // Until it is written and checked, the loader must not start it.
    c.set_good(target, false);
    c.set_active(me, 0);
    l.save(&mut c)?;
    refresh();
    let base = c.lba(target);
    // The header goes last: a slot cut short has none.
    check(l.disk.write(base, &[0u8; SECTOR]), "slot")?;
    let body = h.total - 1;
    let mut done = 0;
    while done < body {
        let n = (body - done).min(CHUNK as u64);
        let at = SECTOR * (1 + done as usize);
        check(l.disk.write(base + 1 + done, &image[at..at + n as usize * SECTOR]), "slot")?;
        done += n;
        progress(done, 2 * body);
    }
    check(l.disk.flush(), "slot")?;
    // Read back what the disk holds.
    let mut sha = Sha256::new();
    let mut buf = vec![0u8; CHUNK * SECTOR];
    done = 0;
    while done < body {
        let n = (body - done).min(CHUNK as u64) as usize;
        check(l.disk.read(base + 1 + done, &mut buf[..n * SECTOR]), "slot")?;
        sha.update(&buf[..n * SECTOR]);
        done += n as u64;
        progress(body + done, 2 * body);
    }
    if sha.finish() != h.sha256 {
        return Err("lo slot riletto dal disco non corrisponde: il disco ha problemi".to_string());
    }
    check(l.disk.write(base, &image[..SECTOR]), "slot")?;
    check(l.disk.flush(), "slot")?;
    if l.header(&c, target).is_none_or(|r| r.sha256 != h.sha256) {
        return Err("l'intestazione dello slot non si rilegge".to_string());
    }
    c.set_active(target, TRIES);
    l.save(&mut c)?;
    refresh();
    crate::info!("update", "arcOS {} written to slot {}: it starts next, on trial", h.version, name(target));
    Ok(h.version)
}

/// The next start uses this system again (undoes an update that waits for a
/// restart, or a [`rollback`]).
pub fn cancel() -> Result<(), String> {
    let _one = WORKING.try_lock().ok_or("sto già cambiando gli slot di sistema")?;
    let me = booted().ok_or("questo sistema non è partito da uno slot")?;
    let l = layout()?;
    let mut c = l.control()?;
    if c.active() == me {
        return Err("non c'è nessun cambio in attesa".to_string());
    }
    c.set_active(me, 0);
    l.save(&mut c)?;
    refresh();
    Ok(())
}

/// The next start uses the other slot: the version before, which worked.
/// Returns its version.
pub fn rollback() -> Result<String, String> {
    let _one = WORKING.try_lock().ok_or("sto già cambiando gli slot di sistema")?;
    let me = booted().ok_or("questo sistema non è partito da uno slot")?;
    if on_trial() {
        return Err("questa versione è ancora in prova: se non funziona, arcOS torna indietro da solo".to_string());
    }
    let l = layout()?;
    let mut c = l.control()?;
    let other = 1 - me;
    let h = l.header(&c, other).filter(|_| c.good(other)).ok_or("nell'altro slot non c'è una versione che ha funzionato")?;
    c.set_active(other, 0);
    l.save(&mut c)?;
    refresh();
    Ok(h.version)
}

/// This system works: marks its slot, and after a fallback makes it the one
/// that starts.
fn confirm() -> Result<(), String> {
    let flags = BOOTED.load(Ordering::Relaxed);
    let Some(me) = booted() else { return Ok(()) };
    if flags & (SLOT_TRIAL | SLOT_FALLBACK) != 0 {
        let _one = WORKING.lock();
        let l = layout()?;
        let mut c = l.control()?;
        c.set_good(me, true);
        if flags & SLOT_FALLBACK != 0 {
            // The other one failed to start.
            c.set_good(1 - me, false);
            c.set_active(me, 0);
        } else if c.active() == me {
            c.set_active(me, 0);
        }
        l.save(&mut c)?;
    }
    CONFIRMED.store(true, Ordering::Relaxed);
    Ok(())
}

/// Confirms this system once it has run for a while (a new one on trial
/// that cannot reach its disk restarts into the other), and says how an
/// update went.
pub fn start() {
    let Some(me) = booted() else { return };
    let flags = BOOTED.load(Ordering::Relaxed);
    let how = if flags & SLOT_FALLBACK != 0 {
        ", the other one did not start"
    } else if flags & SLOT_TRIAL != 0 {
        ", on trial"
    } else {
        ""
    };
    crate::info!("update", "system slot {}{how}", name(me));
    let spawned = crate::task::spawn("arc.sysupdate", || {
        refresh();
        crate::task::sleep_ms(CONFIRM_AFTER_MS);
        let flags = BOOTED.load(Ordering::Relaxed);
        match confirm() {
            Ok(()) if flags & SLOT_FALLBACK != 0 => {
                crate::warn!("update", "the new system did not start: back to arcOS {}", crate::VERSION);
                crate::gui::notify("Aggiornamento non riuscito", &format!("La nuova versione non è partita: arcOS è tornato alla {}.", crate::VERSION), "about");
            }
            Ok(()) if flags & SLOT_TRIAL != 0 => {
                crate::info!("update", "arcOS {} works: slot {} confirmed", crate::VERSION, booted().map_or('?', name));
                crate::gui::notify("arcOS è aggiornato", &format!("Ora c'è la versione {}.", crate::VERSION), "about");
            }
            Ok(()) => {}
            Err(e) => {
                crate::warn!("update", "cannot confirm the system slot: {e}");
                if flags & SLOT_TRIAL != 0 {
                    crate::warn!("update", "the new system cannot reach its disk: restarting into the other one");
                    crate::task::sleep_ms(3000);
                    crate::acpi::reboot();
                }
            }
        }
        refresh();
    });
    if spawned.is_none() {
        crate::warn!("update", "could not start arc.sysupdate");
    }
}
