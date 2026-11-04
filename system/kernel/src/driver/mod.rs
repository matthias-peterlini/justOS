//! The driver manager: finds, installs and loads drivers by itself.
//!
//! Drivers are `.adp` packages (see `arcdk::package`) kept in three places:
//!
//! * `boot:/drivers` — the drivers needed before the system disk is mounted
//!   (disk controllers, keyboard), inside the boot pack;
//! * `boot:/system` — every other driver that comes with the system, also
//!   in the boot pack (so a system update brings its drivers along, and
//!   going back to the previous system brings back its own); used from the
//!   second pass on, as before the store;
//! * `sys:/system/drivers` — the **driver store**: drivers added later.
//!
//! Whenever a device has no driver, the manager matches the device against
//! the manifests of every package, picks the most specific match (newest
//! version first), loads the module and lets it probe the device. This runs
//! at boot, after every device scan and whenever a package is added.
//!
//! Adding a driver needs no command: a package dropped into `sys:/inbox` is
//! checked, moved into the store and installed within a couple of seconds.
//! `drv add <file>` does the same on demand.
//!
//! Devices that drivers find (USB devices behind a controller) are added
//! from the drivers' own threads; the monitor thread wakes up and installs
//! their drivers right away. When such a device goes away, its driver is
//! told and the device's children go with it.

mod api;
pub mod elf;

use crate::dev;
use crate::fs::{self, Path};
use crate::sync::Mutex;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use arcdk::manifest::{compare_versions, Manifest};
use arcdk::package::Package;
use arcdk::{DriverEntry, DriverInfo, Status, ABI_VERSION, DRIVER_MAGIC, E_BUSY, E_NODEV, OK};

pub const STORE: &str = "sys:/system/drivers";
pub const BOOT_DRIVERS: &str = "boot:/drivers";
pub const SYSTEM_DRIVERS: &str = "boot:/system";
pub const INBOX: &str = "sys:/inbox";

/// Where a package is. With the same version, the later one wins.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Origin {
    Boot,
    System,
    Store,
}

impl Origin {
    pub fn name(self) -> &'static str {
        match self {
            Origin::Boot => "boot pack",
            Origin::System => "system",
            Origin::Store => "store",
        }
    }
}

/// A package known to the manager (its manifest; the module stays on disk).
#[derive(Clone)]
pub struct PackageEntry {
    pub path: String,
    pub origin: Origin,
    pub manifest: String,
    pub size: usize,
}

impl PackageEntry {
    pub fn manifest(&self) -> Manifest<'_> {
        Manifest::parse(&self.manifest).expect("validated when the package was read")
    }
}

/// A module loaded in memory.
pub struct Loaded {
    pub name: String,
    pub version: String,
    image: elf::Image,
    info: &'static DriverInfo,
    pub devices: Vec<u32>,
    /// The module has driven a device. Such a module stays loaded when its
    /// devices go away (a call into it may still be returning); only an
    /// explicit unload removes it.
    used: bool,
}

impl Loaded {
    pub fn image_size(&self) -> u64 {
        self.image.size()
    }

    pub fn can_unload(&self) -> bool {
        self.info.remove.is_some()
    }
}

struct Manager {
    packages: Vec<PackageEntry>,
    loaded: Vec<Loaded>,
    /// Past the first pass: the system's other drivers count too.
    all: bool,
}

static MANAGER: Mutex<Manager> = Mutex::new(Manager { packages: Vec::new(), loaded: Vec::new(), all: false });

/// Reads and validates a package file.
fn read_package(path: &Path) -> Result<(Vec<u8>, String), String> {
    let bytes = fs::read_file(path).map_err(|e| alloc::format!("{path}: {e}"))?;
    let manifest = {
        let p = Package::parse(&bytes).map_err(|e| alloc::format!("{path}: {e}"))?;
        if p.abi_version != ABI_VERSION {
            return Err(alloc::format!("{path}: built for driver ABI {}, this kernel speaks {}", p.abi_version, ABI_VERSION));
        }
        p.manifest.text().to_string()
    };
    Ok((bytes, manifest))
}

fn scan_folder(dir: &str, origin: Origin, out: &mut Vec<PackageEntry>) {
    let Ok(dir) = fs::abs(dir) else { return };
    let Ok(entries) = fs::list_dir(&dir) else { return };
    for (e, st) in entries {
        if st.kind != arcfs::Kind::File || !e.name.to_ascii_lowercase().ends_with(".adp") {
            continue;
        }
        let path = dir.join(&e.name);
        match read_package(&path) {
            Ok((bytes, manifest)) => out.push(PackageEntry { path: path.to_string(), origin, manifest, size: bytes.len() }),
            Err(e) => crate::warn!("drivers", "skipping {e}"),
        }
    }
}

impl Manager {
    fn refresh(&mut self) {
        let mut packages = Vec::new();
        scan_folder(BOOT_DRIVERS, Origin::Boot, &mut packages);
        if self.all {
            scan_folder(SYSTEM_DRIVERS, Origin::System, &mut packages);
        }
        scan_folder(STORE, Origin::Store, &mut packages);
        self.packages = packages;
    }

    fn loaded_index(&self, name: &str) -> Option<usize> {
        self.loaded.iter().position(|l| l.name.eq_ignore_ascii_case(name))
    }

    /// Packages that match a device, best first.
    fn candidates(&self, dev: &arcdk::DeviceInfo) -> Vec<(u32, PackageEntry)> {
        let mut c: Vec<(u32, PackageEntry)> =
            self.packages.iter().filter_map(|p| p.manifest().score(dev).map(|s| (s, p.clone()))).collect();
        c.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| compare_versions(b.1.manifest().version(), a.1.manifest().version()))
                .then_with(|| b.1.origin.cmp(&a.1.origin))
        });
        // One entry per driver name: the best version of it.
        let mut seen: Vec<String> = Vec::new();
        c.retain(|(_, p)| {
            let n = p.manifest().name().to_ascii_lowercase();
            if seen.contains(&n) {
                false
            } else {
                seen.push(n);
                true
            }
        });
        c
    }

    fn load(&mut self, pkg: &PackageEntry) -> Result<usize, String> {
        let name = pkg.manifest().name().to_string();
        if let Some(i) = self.loaded_index(&name) {
            return Ok(i);
        }
        let path = fs::abs(&pkg.path).map_err(|e| e.to_string())?;
        let (bytes, _) = read_package(&path)?;
        let module = Package::parse(&bytes).map_err(String::from)?.module;
        let image = elf::load(module).map_err(|e| alloc::format!("{name}: {e}"))?;
        // SAFETY: the entry point of a verified module; it only stores the
        // table pointer and returns its static DriverInfo.
        let info = unsafe {
            let entry: DriverEntry = core::mem::transmute::<u64, DriverEntry>(image.entry);
            entry(&api::KERNEL_API)
        };
        // SAFETY: a non-null DriverInfo lives in the module's image.
        let valid = !info.is_null() && unsafe { (*info).magic == DRIVER_MAGIC && (*info).abi_version == ABI_VERSION };
        if !valid {
            image.unload();
            return Err(alloc::format!("{name}: the module did not start (wrong ABI or corrupt image)"));
        }
        // SAFETY: valid for as long as the image stays loaded.
        let info: &'static DriverInfo = unsafe { &*info };
        let version = pkg.manifest().version().to_string();
        crate::debug!("drivers", "loaded {name} {version} at {:#x} ({})", image.base, crate::util::Size(image.size()));
        self.loaded.push(Loaded { name, version, image, info, devices: Vec::new(), used: false });
        Ok(self.loaded.len() - 1)
    }

    /// Unloads modules that ended up driving nothing.
    fn drop_idle(&mut self) {
        let mut i = 0;
        while i < self.loaded.len() {
            if self.loaded[i].devices.is_empty() && !self.loaded[i].used {
                let l = self.loaded.remove(i);
                crate::debug!("drivers", "unloaded {} (no device)", l.name);
                l.image.unload();
            } else {
                i += 1;
            }
        }
    }

    /// Finds a driver for every device that lacks one. Returns how many
    /// devices got a driver.
    fn configure(&mut self) -> usize {
        let mut bound = 0;
        for d in dev::list() {
            if d.has_driver() || !d.needs_driver() {
                continue;
            }
            let candidates = self.candidates(&d.info);
            if candidates.is_empty() {
                dev::set_problem(d.info.id, Some(String::from("no driver available")));
                continue;
            }
            let mut last_error = None;
            for (_, pkg) in candidates {
                let idx = match self.load(&pkg) {
                    Ok(i) => i,
                    Err(e) => {
                        crate::warn!("drivers", "{e}");
                        last_error = Some(e);
                        continue;
                    }
                };
                // SAFETY: probe of a loaded module, with a valid device.
                let status = unsafe { (self.loaded[idx].info.probe)(&d.info) };
                if status == OK {
                    let l = &mut self.loaded[idx];
                    l.devices.push(d.info.id);
                    l.used = true;
                    dev::set_driver(d.info.id, Some(&l.name));
                    crate::info!("drivers", "{} {} installed for {} ({})", l.name, l.version, d.description(), d.location());
                    bound += 1;
                    last_error = None;
                    break;
                } else if status != E_NODEV {
                    let e = alloc::format!("{} could not start the device: {}", self.loaded[idx].name, arcdk::status_name(status));
                    crate::warn!("drivers", "{e} ({})", d.location());
                    last_error = Some(e);
                }
            }
            dev::set_problem(d.info.id, last_error);
        }
        self.drop_idle();
        bound
    }

    fn unload(&mut self, name: &str) -> Result<(), String> {
        let i = self.loaded_index(name).ok_or_else(|| alloc::format!("{name} is not loaded"))?;
        let l = &self.loaded[i];
        let Some(remove) = l.info.remove else {
            return Err(alloc::format!("{} cannot be unloaded while the system runs; it is replaced at the next start", l.name));
        };
        for &d in &l.devices {
            if dev::block::list().iter().any(|disk| disk.device == d && fs::is_mounted_on(&disk.name)) {
                return Err(alloc::format!("{} drives a disk with a mounted volume", l.name));
            }
        }
        // The devices these found (USB devices behind a controller) go
        // first, while the module can still serve them.
        let devices = self.loaded[i].devices.clone();
        for &d in &devices {
            for child in dev::children(d) {
                self.remove_device(child);
            }
        }
        let i = self.loaded_index(name).expect("still loaded");
        let l = self.loaded.remove(i);
        for &d in &l.devices {
            // SAFETY: the module's own remove callback for a device it owns.
            unsafe { remove(d) };
            dev::pm::forget(d);
            dev::irq::detach(d, None);
            let _ = dev::block::unregister_device(d);
            dev::net::remove_device(d);
            dev::set_driver(d, None);
        }
        l.image.unload();
        Ok(())
    }

    /// A device went away: its children first, then the device. Returns
    /// whether every driver involved let go of it.
    fn remove_device(&mut self, id: u32) -> bool {
        let Some(d) = dev::get(id) else { return true };
        let mut released = true;
        for child in dev::mark_gone(id) {
            released &= self.remove_device(child);
        }
        // No new I/O reaches the hardware from here on.
        dev::pm::forget(id);
        dev::irq::detach(id, None);
        dev::block::device_gone(id);
        dev::net::remove_device(id);
        dev::audio::remove_device(id);
        dev::display::remove_device(id);
        if let Some(i) = self.loaded.iter().position(|l| l.devices.contains(&id)) {
            let l = &mut self.loaded[i];
            l.devices.retain(|&x| x != id);
            match l.info.remove {
                // SAFETY: the module's own remove callback for a device it owns.
                Some(remove) => unsafe { remove(id) },
                None => released = false,
            }
            crate::info!("drivers", "{} removed from {} ({})", d.description(), l.name, d.location());
        } else if !d.owned {
            crate::debug!("drivers", "{} ({}) removed", d.description(), d.location());
        }
        released
    }
}

/// First pass at boot: drivers from the boot pack.
pub fn init() {
    let mut m = MANAGER.lock();
    m.refresh();
    let n = m.packages.len();
    let bound = m.configure();
    crate::debug!("drivers", "{n} packages in the boot pack, {bound} device{} configured", if bound == 1 { "" } else { "s" });
}

/// Set when a driver adds a device: the monitor looks for its driver.
static PENDING: crate::task::Event = crate::task::Event::new();

/// A driver found a device (see `KernelApi::device_add`).
pub fn device_added() {
    PENDING.signal();
}

/// A device went away (see `KernelApi::device_remove`).
pub fn device_removed(id: u32) -> Status {
    let mut m = MANAGER.lock();
    let released = m.remove_device(id);
    m.drop_idle();
    if released {
        OK
    } else {
        E_BUSY
    }
}

/// At boot, without a system volume: gives controllers that find disks by
/// themselves (USB) up to `ms` milliseconds to find it.
pub fn wait_for_system(ms: u64) {
    let until = crate::time::uptime_ms() + ms;
    while fs::volume("sys").is_none() && crate::time::uptime_ms() < until {
        if PENDING.wait(Some(100)) {
            configure_all();
        }
    }
}

/// After the system volume is mounted: every driver of the system and the
/// whole store.
pub fn configure_all() -> usize {
    let mut m = MANAGER.lock();
    m.all = true;
    m.refresh();
    m.configure()
}

/// Rescans the hardware and configures new devices.
pub fn rescan() -> (usize, usize) {
    let new = dev::scan().len();
    (new, configure_all())
}

/// Validates a package, copies it into the store and installs it for any
/// matching device. Returns a summary for the user.
pub fn add(path: &Path) -> Result<String, String> {
    let (bytes, manifest_text) = read_package(path)?;
    let manifest = Manifest::parse(&manifest_text).map_err(String::from)?;
    let name = manifest.name().to_string();
    let version = manifest.version().to_string();
    if fs::volume("sys").is_none() {
        return Err(String::from("no system volume (sys:) is mounted"));
    }
    let store = fs::abs(STORE).unwrap();
    fs::make_dir(&store).map_err(|e| alloc::format!("{STORE}: {e}"))?;
    let dest = store.join(&alloc::format!("{}.adp", name.to_ascii_lowercase()));
    let mut m = MANAGER.lock();
    // A loaded older version is replaced right away when it can be unloaded.
    let mut note = String::new();
    if let Some(i) = m.loaded_index(&name) {
        let old = m.loaded[i].version.clone();
        if compare_versions(&version, &old).is_gt() {
            match m.unload(&name) {
                Ok(()) => note = alloc::format!(" (replaced {old})"),
                Err(e) => note = alloc::format!(" ({e})"),
            }
        }
    }
    fs::write_file(&dest, &bytes).map_err(|e| alloc::format!("{dest}: {e}"))?;
    m.refresh();
    let bound = m.configure();
    Ok(alloc::format!(
        "{name} {version} added to the driver store{note}; {}",
        match bound {
            0 => String::from("no device needs it right now"),
            1 => String::from("1 device configured"),
            n => alloc::format!("{n} devices configured"),
        }
    ))
}

/// Unloads a driver (if possible) and deletes it from the store. A driver
/// that cannot be unloaded keeps running until the next start.
pub fn remove(name: &str) -> Result<String, String> {
    let mut m = MANAGER.lock();
    let mut notes: Vec<String> = Vec::new();
    if let Some(i) = m.loaded_index(name) {
        if m.loaded[i].can_unload() {
            m.unload(name)?;
            notes.push(String::from("unloaded"));
        } else {
            notes.push(String::from("it keeps running until the next start"));
        }
    }
    let entries: Vec<PackageEntry> = m.packages.iter().filter(|p| p.manifest().name().eq_ignore_ascii_case(name)).cloned().collect();
    if entries.is_empty() {
        return Err(alloc::format!("no driver named {name}"));
    }
    let mut removed = false;
    for p in entries.iter().filter(|p| p.origin == Origin::Store) {
        let path = fs::abs(&p.path).map_err(|e| e.to_string())?;
        fs::remove(&path).map_err(|e| alloc::format!("{path}: {e}"))?;
        removed = true;
    }
    if removed {
        notes.insert(0, String::from("removed from the driver store"));
    }
    if entries.iter().any(|p| p.origin != Origin::Store) {
        notes.push(String::from("the copy that comes with the system stays"));
    }
    m.refresh();
    // Another driver may take over the freed devices.
    m.configure();
    Ok(notes.join("; "))
}

/// Unloads and reloads a driver (picks up a newer version from the store).
pub fn reload(name: &str) -> Result<String, String> {
    let mut m = MANAGER.lock();
    m.unload(name)?;
    m.refresh();
    let n = m.configure();
    Ok(alloc::format!("{name} reloaded, {n} device{} configured", if n == 1 { "" } else { "s" }))
}

/// Installs every package waiting in `sys:/inbox`. Returns the messages.
pub fn process_inbox() -> Vec<String> {
    let mut out = Vec::new();
    let Ok(inbox) = fs::abs(INBOX) else { return out };
    let Ok(entries) = fs::list_dir(&inbox) else { return out };
    for (e, st) in entries {
        if st.kind != arcfs::Kind::File || !e.name.to_ascii_lowercase().ends_with(".adp") {
            continue;
        }
        let path = inbox.join(&e.name);
        let msg = match add(&path) {
            Ok(m) => {
                crate::info!("drivers", "inbox: {m}");
                m
            }
            Err(err) => {
                crate::warn!("drivers", "inbox: {err}");
                // Keep broken packages out of the way so they are not retried.
                let _ = fs::rename(&path, &inbox.join(&alloc::format!("{}.rejected", e.name)));
                err
            }
        };
        let _ = fs::remove(&path);
        out.push(msg);
    }
    out
}

/// Background thread: installs packages dropped into the inbox and drivers
/// for newly appeared devices.
pub fn start_monitor() {
    crate::task::spawn("drvmon", || {
        let mut last_scan = 0;
        loop {
            if PENDING.wait(Some(2000)) {
                configure_all();
            }
            let now = crate::time::uptime_ms();
            if now - last_scan >= 2000 {
                last_scan = now;
                process_inbox();
                let new = dev::scan();
                if !new.is_empty() {
                    crate::info!("drivers", "{} new devices found", new.len());
                    configure_all();
                }
            }
        }
    });
}

#[derive(Clone)]
pub struct DriverStatus {
    pub name: String,
    pub version: String,
    pub class: String,
    pub origins: Vec<Origin>,
    pub loaded: Option<(String, u64, Vec<u32>, bool)>,
}

/// Every known driver: packages merged by name, with what is loaded.
pub fn status() -> Vec<DriverStatus> {
    let m = MANAGER.lock();
    let mut out: Vec<DriverStatus> = Vec::new();
    for p in &m.packages {
        let mf = p.manifest();
        match out.iter_mut().find(|d| d.name.eq_ignore_ascii_case(mf.name())) {
            Some(d) => {
                if !d.origins.contains(&p.origin) {
                    d.origins.push(p.origin);
                }
                if compare_versions(mf.version(), &d.version).is_gt() {
                    d.version = mf.version().to_string();
                }
            }
            None => out.push(DriverStatus {
                name: mf.name().to_string(),
                version: mf.version().to_string(),
                class: mf.class().to_string(),
                origins: alloc::vec![p.origin],
                loaded: None,
            }),
        }
    }
    for l in &m.loaded {
        let entry = (l.version.clone(), l.image_size(), l.devices.clone(), l.can_unload());
        match out.iter_mut().find(|d| d.name.eq_ignore_ascii_case(&l.name)) {
            Some(d) => d.loaded = Some(entry),
            None => out.push(DriverStatus {
                name: l.name.clone(),
                version: l.version.clone(),
                class: String::new(),
                origins: Vec::new(),
                loaded: Some(entry),
            }),
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Where a driver's packages are: (path, origin, size).
pub type PackageFiles = Vec<(String, Origin, usize)>;

/// Manifest text and package paths of a driver.
pub fn details(name: &str) -> Option<(String, PackageFiles)> {
    let m = MANAGER.lock();
    let pkgs: Vec<&PackageEntry> = m.packages.iter().filter(|p| p.manifest().name().eq_ignore_ascii_case(name)).collect();
    let best = pkgs.iter().max_by(|a, b| compare_versions(a.manifest().version(), b.manifest().version()))?;
    Some((best.manifest.clone(), pkgs.iter().map(|p| (p.path.clone(), p.origin, p.size)).collect()))
}
