//! justtool — builds justOS: driver packages, bootable disk images, and access
//! to JustFS volumes from the host.
//!
//! ```text
//! justtool pack <driver.manifest> <module> <out.jdp>
//! justtool inspect <file.jdp>
//! justtool image <out.img> --stage1 <bin> --stage2 <bin> --kernel <elf> --drivers <dir>
//!               [--efi <BOOTX64.efi>] [--size 256M] [--files <dir>] [--version <v>] [--slot-size 24M]
//! justtool iso <out.iso> (the same, --efi required) [--size 16M]   a live CD
//! justtool slot <out.slot> --kernel <elf> --drivers <dir> [--version <v>]   a system update
//! justtool fs <image> ls [path] | cat <path> | put <local> <path> | get <path> <local>
//!               | mkdir <path> | rm <path> | check
//! justtool updates <store dir> <out dir> --key <file> --base-url <url> [--slot <file>]   the signed index of updates
//! justtool pubkey <key file>
//! ```

mod disk;
mod elf;
mod fat;
mod iso;
mod slot;
mod updates;

use justdk::manifest::Manifest;
use justdk::package::{self, Package};
use justfs::{BlockDevice, Fs, Kind, MemDisk, BLOCK_SIZE};
use disk::{FileBlocks, CLOCK};
use std::fs::{self, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

type Result<T> = std::result::Result<T, String>;

const KERNEL_LINK_BASE: u64 = 0xFFFF_FFFF_8010_0000;
const SECTOR: u64 = 512;
/// Stage 2 sits right after the MBR (stage 1 reads it in one go).
const STAGE2_MAX_SECTORS: u64 = 63;
/// Where the EFI system partition starts (1 MiB).
const ESP_LBA: u64 = 2048;
/// Room for one version of the system (kernel and boot pack).
const SLOT_SIZE: u64 = 24 << 20;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("pack") => cmd_pack(&args[1..]),
        Some("inspect") => cmd_inspect(&args[1..]),
        Some("image") => cmd_image(&args[1..]),
        Some("iso") => cmd_iso(&args[1..]),
        Some("slot") => cmd_slot(&args[1..]),
        Some("fs") => cmd_fs(&args[1..]),
        Some("mkfs") => cmd_mkfs(&args[1..]),
        Some("updates") => updates::cmd_updates(&args[1..]),
        Some("pubkey") => updates::cmd_pubkey(&args[1..]),
        _ => Err(String::from(
            "usage:\n  justtool pack <driver.manifest> <module> <out.jdp>\n  justtool inspect <file.jdp>\n  justtool image <out.img> --stage1 <bin> --stage2 <bin> --kernel <elf> --drivers <dir> [--efi <BOOTX64.efi>] [--size 256M] [--files <dir>] [--version <v>] [--slot-size 24M]\n  justtool iso <out.iso> --stage1 <bin> --stage2 <bin> --kernel <elf> --drivers <dir> --efi <BOOTX64.efi> [--size 16M] [--files <dir>]\n  justtool slot <out.slot> --kernel <elf> --drivers <dir> [--version <v>]\n  justtool fs <image> ls [path] | cat <path> | put <local> <path> | get <path> <local> | mkdir <path> | rm <path> | check\n  justtool mkfs <image> [--size 64M] [--label Data]\n  justtool updates <store dir> <out dir> --key <file> --base-url <url> [--serial <n>] [--slot <file>]\n  justtool pubkey <key file>",
        )),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("justtool: {e}");
            ExitCode::FAILURE
        }
    }
}

fn read(path: impl AsRef<Path>) -> Result<Vec<u8>> {
    let path = path.as_ref();
    fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}

// ----------------------------------------------------------------------------
// pack / inspect

fn cmd_pack(args: &[String]) -> Result<()> {
    let [manifest, module, out] = args else {
        return Err("usage: justtool pack <driver.manifest> <module> <out.jdp>".into());
    };
    let text = String::from_utf8(read(manifest)?).map_err(|_| format!("{manifest}: not UTF-8"))?;
    let m = Manifest::parse(&text).map_err(|e| format!("{manifest}: {e}"))?;
    if m.rules().next().is_none() {
        eprintln!("justtool: warning: {manifest} has no 'match' rule; the driver will never be installed automatically");
    }
    let image = read(module)?;
    let elf = elf::Elf::parse(&image).map_err(|e| format!("{module}: {e}"))?;
    elf.check_module().map_err(|e| format!("{module}: {e}"))?;
    let mut buf = vec![0u8; package::packed_size(&text, &image)];
    package::write(&mut buf, justdk::ABI_VERSION, &text, &image);
    fs::write(out, &buf).map_err(|e| format!("{out}: {e}"))?;
    println!("{} {} -> {} ({} bytes)", m.name(), m.version(), out, buf.len());
    Ok(())
}

fn cmd_inspect(args: &[String]) -> Result<()> {
    let [file] = args else { return Err("usage: justtool inspect <file.jdp>".into()) };
    let bytes = read(file)?;
    let p = Package::parse(&bytes).map_err(|e| format!("{file}: {e}"))?;
    println!("driver ABI {}, module {} bytes", p.abi_version, p.module.len());
    print!("{}", p.manifest.text());
    Ok(())
}

// ----------------------------------------------------------------------------
// image

fn parse_size(s: &str) -> Result<u64> {
    let s = s.trim();
    let (num, mult) = match s.chars().last() {
        Some('K' | 'k') => (&s[..s.len() - 1], 1 << 10),
        Some('M' | 'm') => (&s[..s.len() - 1], 1 << 20),
        Some('G' | 'g') => (&s[..s.len() - 1], 1 << 30),
        _ => (s, 1),
    };
    num.parse::<u64>().map(|n| n * mult).map_err(|_| format!("bad size {s}"))
}

struct DriverFile {
    name: String,
    file_name: String,
    bytes: Vec<u8>,
    boot: bool,
}

fn load_drivers(dir: &Path) -> Result<Vec<DriverFile>> {
    let mut out = Vec::new();
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "jdp"))
        .collect();
    entries.sort();
    for path in entries {
        let bytes = read(&path)?;
        let p = Package::parse(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        out.push(DriverFile {
            name: p.manifest.name().to_string(),
            file_name: path.file_name().unwrap().to_string_lossy().to_lowercase(),
            boot: p.manifest.is_boot(),
            bytes: bytes.clone(),
        });
    }
    Ok(out)
}

/// The boot pack: a small read-only JustFS image with every driver that
/// comes with the system. Those needed before the system disk is mounted
/// are in `drivers/`, the others in `system/`; both travel with the kernel,
/// so a system update brings its own drivers.
fn build_boot_pack(drivers: &[DriverFile]) -> Result<Vec<u8>> {
    let payload: usize = drivers.iter().map(|d| d.bytes.len().div_ceil(BLOCK_SIZE) + 2).sum();
    let blocks = (payload + 32).max(64);
    let mut fs = Fs::format(MemDisk::new(blocks), "Boot", &CLOCK).map_err(|e| format!("boot pack: {e}"))?;
    for dir in ["drivers", "system"] {
        fs.create_dirs(dir).map_err(|e| format!("boot pack: {e}"))?;
    }
    for d in drivers {
        let dir = if d.boot { "drivers" } else { "system" };
        fs.write_file(&format!("{dir}/{}", d.file_name), &d.bytes).map_err(|e| format!("boot pack: {}: {e}", d.name))?;
    }
    fs.sync().map_err(|e| format!("boot pack: {e}"))?;
    Ok(fs.into_device().data)
}

/// Copies a host folder into the volume, recursively (symbolic links stay
/// links).
fn copy_tree<D: BlockDevice>(fs: &mut Fs<D>, host: &Path, inner: &str) -> Result<()> {
    let mut entries: Vec<PathBuf> =
        fs::read_dir(host).map_err(|e| format!("{}: {e}", host.display()))?.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let target = format!("{inner}/{name}");
        if path.is_symlink() {
            let to = fs::read_link(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            fs.symlink(&target, &to.to_string_lossy()).map_err(|e| format!("{target}: {e}"))?;
        } else if path.is_dir() {
            fs.create_dirs(&target).map_err(|e| format!("{target}: {e}"))?;
            copy_tree(fs, &path, &target)?;
        } else {
            fs.write_file(&target, &read(&path)?).map_err(|e| format!("{target}: {e}"))?;
        }
    }
    Ok(())
}

/// What both `image` and `iso` are built from.
struct Inputs {
    out: String,
    size: Option<u64>,
    stage1: Vec<u8>,
    stage2: Vec<u8>,
    /// The kernel as one flat image from its link address.
    flat: Vec<u8>,
    memsz: u64,
    entry: u64,
    drivers: Vec<DriverFile>,
    pack: Vec<u8>,
    files: Option<String>,
    efi: Option<Vec<u8>>,
    /// The system's version, for the slot header.
    version: String,
    slot_size: u64,
}

/// `stages`: whether the BIOS loader's stages are needed (not for a slot).
fn parse_inputs(args: &[String], stages: bool) -> Result<Inputs> {
    let mut out = None;
    let mut size = None;
    let (mut stage1, mut stage2, mut kernel, mut drivers, mut files, mut efi) = (None, None, None, None, None, None);
    let (mut version, mut slot_size) = (None, SLOT_SIZE);
    let mut i = 0;
    while i < args.len() {
        let next = || args.get(i + 1).cloned().ok_or(format!("{} needs a value", args[i]));
        match args[i].as_str() {
            "--size" => size = Some(parse_size(&next()?)?),
            "--stage1" => stage1 = Some(next()?),
            "--stage2" => stage2 = Some(next()?),
            "--kernel" => kernel = Some(next()?),
            "--drivers" => drivers = Some(next()?),
            "--files" => files = Some(next()?),
            "--efi" => efi = Some(next()?),
            "--version" => version = Some(next()?),
            "--slot-size" => slot_size = parse_size(&next()?)?,
            a if a.starts_with("--") => return Err(format!("unknown option {a}")),
            a => {
                out = Some(a.to_string());
                i += 1;
                continue;
            }
        }
        i += 2;
    }
    let out = out.ok_or("missing output file")?;
    let (stage1, stage2) = if stages {
        (read(stage1.ok_or("missing --stage1")?)?, read(stage2.ok_or("missing --stage2")?)?)
    } else {
        (vec![0; 512], Vec::new())
    };
    let kernel_path = kernel.ok_or("missing --kernel")?;
    let kernel_elf = read(&kernel_path)?;
    let drivers = load_drivers(Path::new(&drivers.ok_or("missing --drivers")?))?;
    let efi = efi.map(read).transpose()?;
    let version = version.unwrap_or_else(|| String::from("0"));
    if version.len() > 31 {
        return Err("the version is longer than 31 bytes".into());
    }
    if slot_size % (1 << 20) != 0 {
        return Err("the slot size must be whole MiB".into());
    }

    if stage1.len() != 512 {
        return Err("stage 1 must be exactly 512 bytes".into());
    }
    if (stage2.len() as u64).div_ceil(SECTOR) > STAGE2_MAX_SECTORS {
        return Err(format!("stage 2 is {} bytes, the limit is {}", stage2.len(), STAGE2_MAX_SECTORS * SECTOR));
    }
    let elf = elf::Elf::parse(&kernel_elf).map_err(|e| format!("{kernel_path}: {e}"))?;
    let (base, flat, memsz) = elf.flatten().map_err(|e| format!("{kernel_path}: {e}"))?;
    if base != KERNEL_LINK_BASE {
        return Err(format!("{kernel_path}: linked at {base:#x}, expected {KERNEL_LINK_BASE:#x}"));
    }
    if memsz > (1 << 30) {
        return Err("kernel too big".into());
    }
    let pack = build_boot_pack(&drivers)?;
    Ok(Inputs { out, size, stage1, stage2, flat, memsz, entry: elf.entry, drivers, pack, files, efi, version, slot_size })
}

impl Inputs {
    /// KERNEL.BIN for the UEFI loader: magic, entry, memory size, file
    /// size, then the flat image.
    fn kernel_bin(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(32 + self.flat.len());
        out.extend_from_slice(b"ARCKERN1");
        for v in [self.entry, self.memsz, self.flat.len() as u64] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&self.flat);
        out
    }

    /// The slot image: header, kernel and boot pack.
    fn slot(&self) -> Vec<u8> {
        slot::build(&self.flat, self.memsz, self.entry, &self.pack, &self.version, slot::build_time())
    }

    /// Fills a freshly formatted system volume: the usual folders and the
    /// files of --files. The driver store starts empty: the drivers that
    /// come with the system are in the boot pack.
    fn fill_system<D: BlockDevice>(&self, fs: &mut Fs<D>) -> Result<()> {
        let fserr = |e: justfs::Error| format!("system volume: {e}");
        for dir in ["system/drivers", "inbox", "home"] {
            fs.create_dirs(dir).map_err(fserr)?;
        }
        if let Some(dir) = &self.files {
            copy_tree(fs, Path::new(dir), "")?;
        }
        let sys = fs.lookup("system").map_err(fserr)?;
        fs.set_flags(sys, justfs::FLAG_SYSTEM).map_err(fserr)?;
        // The Linux file tree (from tools/linux/debroot.py) is for Linux
        // programs; the Files app does not show it.
        for name in ["bin", "sbin", "lib", "lib64", "usr"] {
            if let Ok(id) = fs.lookup_link(name) {
                fs.set_flags(id, justfs::FLAG_HIDDEN).map_err(fserr)?;
            }
        }
        fs.sync().map_err(fserr)
    }

    fn summary(&self) -> String {
        let boot: Vec<&str> = self.drivers.iter().filter(|d| d.boot).map(|d| d.name.as_str()).collect();
        format!(
            "justOS {}, kernel {} KiB, boot pack {} KiB with {} drivers (at boot: {})",
            self.version,
            self.flat.len() >> 10,
            self.pack.len() >> 10,
            self.drivers.len(),
            boot.join(", ")
        )
    }
}

/// The boot table in stage 1 (see boot/bios/stage1.asm). Positions and
/// sizes count 512-byte sectors.
#[derive(Default)]
struct BootTable {
    stage2_lba: u64,
    stage2_sectors: u64,
    kernel_lba: u64,
    kernel_sectors: u64,
    memsz: u64,
    pack_lba: u64,
    pack_sectors: u64,
    entry: u64,
    system_lba: u64,
    system_sectors: u64,
    /// BOOT.CTL, which picks the slot (0: start the kernel above).
    ctl_lba: u64,
    /// 2048-byte sectors (a CD).
    cd: bool,
}

impl BootTable {
    fn write(&self, sector: &mut [u8]) {
        let mut put = |at: usize, v: u64, len: usize| sector[at..at + len].copy_from_slice(&v.to_le_bytes()[..len]);
        put(0x180, self.stage2_lba, 4);
        put(0x184, self.system_lba, 4);
        put(0x188, self.system_sectors, 4);
        put(0x18C, self.ctl_lba, 4);
        put(0x190, u64::from(u32::from_le_bytes(*b"ARCB")), 4);
        put(0x194, self.stage2_sectors, 2);
        put(0x196, u64::from(self.cd), 2);
        put(0x198, self.kernel_lba, 4);
        put(0x19C, self.kernel_sectors, 4);
        put(0x1A0, self.memsz, 4);
        put(0x1A4, self.pack_lba, 4);
        put(0x1A8, self.pack_sectors, 4);
        put(0x1AC, self.entry, 8);
    }
}

/// A disk: MBR and stage 2, the EFI system partition (FAT16) with the UEFI
/// loader, BOOT.CTL and the two system slots, then the system volume.
fn cmd_image(args: &[String]) -> Result<()> {
    let inputs = parse_inputs(args, true)?;
    let out = &inputs.out;
    let size = inputs.size.unwrap_or(256 << 20);
    let stage2_sectors = (inputs.stage2.len() as u64).div_ceil(SECTOR);
    let slot_image = inputs.slot();
    let slot_size = inputs.slot_size;
    if slot_image.len() as u64 > slot_size {
        return Err(format!("the system ({} KiB) does not fit a slot of {} MiB", slot_image.len() >> 10, slot_size >> 20));
    }
    let header = slot::parse(&slot_image).expect("a slot just built");
    // Two slots, the loader and a little room.
    let esp_size = 2 * slot_size + (8 << 20);
    let esp_lba = ESP_LBA;
    let esp_sectors = esp_size / SECTOR;
    let part_lba = esp_lba + esp_sectors;
    let total_sectors = size / SECTOR;
    if part_lba + 8192 > total_sectors {
        return Err(format!("the image is too small: the system needs at least {} MiB", ((part_lba + 8192) * SECTOR) >> 20));
    }

    let mut padded = slot_image.clone();
    padded.resize(slot_size as usize, 0);
    let mut files = Vec::new();
    if let Some(loader) = &inputs.efi {
        files.push(("EFI/BOOT/BOOTX64.EFI", loader.clone()));
    }
    files.push(("JUSTOS/BOOT.CTL", vec![0; SECTOR as usize]));
    files.push(("JUSTOS/SLOTA.BIN", padded.clone()));
    files.push(("JUSTOS/SLOTB.BIN", padded));
    let (mut esp, offsets) = fat::build(esp_size, esp_lba, files)?;
    let at = |name: &str| offsets.iter().find(|(n, _)| n == name).map(|(_, o)| *o).unwrap();
    let (slot_a, slot_b) = (esp_lba + at("JUSTOS/SLOTA.BIN") / SECTOR, esp_lba + at("JUSTOS/SLOTB.BIN") / SECTOR);
    let ctl_at = at("JUSTOS/BOOT.CTL") as usize;
    esp[ctl_at..ctl_at + SECTOR as usize].copy_from_slice(&slot::control(slot_a, slot_b, slot_size / SECTOR));

    let mut f = OpenOptions::new().read(true).write(true).create(true).truncate(true).open(out).map_err(|e| format!("{out}: {e}"))?;
    f.set_len(size).map_err(|e| format!("{out}: {e}"))?;

    // MBR: boot code, boot table, the JustFS partition and the ESP. Without
    // a readable BOOT.CTL, stage 2 starts slot A from the table.
    let mut mbr = inputs.stage1.clone();
    BootTable {
        stage2_lba: 1,
        stage2_sectors,
        kernel_lba: slot_a + 1,
        kernel_sectors: header.kernel_sectors as u64,
        memsz: inputs.memsz,
        pack_lba: slot_a + header.pack_sector as u64,
        pack_sectors: header.pack_sectors as u64,
        entry: inputs.entry,
        ctl_lba: esp_lba + ctl_at as u64 / SECTOR,
        ..BootTable::default()
    }
    .write(&mut mbr);
    let signature: u32 = 0x4152_4300 | (std::process::id() & 0xFF);
    mbr[0x1B8..0x1BC].copy_from_slice(&signature.to_le_bytes());
    let part_sectors = total_sectors - part_lba;
    let mut partition = |slot: usize, active: bool, kind: u8, lba: u64, sectors: u64| {
        let entry = &mut mbr[0x1BE + slot * 16..0x1CE + slot * 16];
        entry.fill(0);
        entry[0] = if active { 0x80 } else { 0 };
        entry[1..4].copy_from_slice(&[0xFE, 0xFF, 0xFF]);
        entry[4] = kind;
        entry[5..8].copy_from_slice(&[0xFE, 0xFF, 0xFF]);
        entry[8..12].copy_from_slice(&(lba as u32).to_le_bytes());
        entry[12..16].copy_from_slice(&(sectors as u32).to_le_bytes());
    };
    partition(0, true, 0xAC, part_lba, part_sectors);
    partition(1, false, 0xEF, esp_lba, esp_sectors);

    let mut write_at = |lba: u64, data: &[u8]| -> Result<()> {
        f.seek(SeekFrom::Start(lba * SECTOR)).and_then(|_| f.write_all(data)).map_err(|e| format!("{out}: {e}"))
    };
    write_at(0, &mbr)?;
    write_at(1, &inputs.stage2)?;
    write_at(esp_lba, &esp)?;

    // The system volume.
    let file = f.try_clone().map_err(|e| e.to_string())?;
    let dev = FileBlocks { file, offset: part_lba * SECTOR, blocks: part_sectors * SECTOR / BLOCK_SIZE as u64 };
    let mut fs = Fs::format(dev, "System", &CLOCK).map_err(|e| format!("system volume: {e}"))?;
    inputs.fill_system(&mut fs)?;
    println!(
        "{out}: {} MiB, {} — {}; slots of {} MiB, {} KiB used",
        size >> 20,
        if inputs.efi.is_some() { "BIOS and UEFI" } else { "BIOS" },
        inputs.summary(),
        slot_size >> 20,
        slot_image.len() >> 10
    );
    Ok(())
}

/// A system update: the slot image alone (what `sysupdate install` writes
/// into the other slot), named in the signed index by `justtool updates`.
fn cmd_slot(args: &[String]) -> Result<()> {
    let inputs = parse_inputs(args, false)?;
    let image = inputs.slot();
    if image.len() as u64 > SLOT_SIZE {
        return Err(format!("the system ({} KiB) does not fit a slot of {} MiB", image.len() >> 10, SLOT_SIZE >> 20));
    }
    fs::write(&inputs.out, &image).map_err(|e| format!("{}: {e}", inputs.out))?;
    println!("{}: {} KiB — {}", inputs.out, image.len() >> 10, inputs.summary());
    Ok(())
}

// ----------------------------------------------------------------------------
// iso

/// A live CD: ISO 9660 with El Torito entries for BIOS and UEFI. The system
/// volume travels as a file that the loaders read into memory; the kernel
/// mounts it as `sys:`, so changes last until the computer is turned off.
fn cmd_iso(args: &[String]) -> Result<()> {
    let inputs = parse_inputs(args, true)?;
    let efi = inputs.efi.clone().ok_or("missing --efi: the ISO boots with UEFI as well")?;
    let system_size = inputs.size.unwrap_or(16 << 20);

    let mut fs =
        Fs::format(MemDisk::new((system_size as usize) / BLOCK_SIZE), "System", &CLOCK).map_err(|e| format!("system volume: {e}"))?;
    inputs.fill_system(&mut fs)?;
    let system = fs.into_device().data;

    // The EFI image (FAT) holds the loader, the kernel, the boot pack and
    // the system volume; the BIOS loader reads the last two from inside it.
    let payload = efi.len() + inputs.flat.len() + inputs.pack.len() + system.len();
    let fat_size = ((payload as u64 + (2 << 20)) * 9 / 8).next_multiple_of(1 << 20).max(9 << 20);
    let files = vec![
        ("EFI/BOOT/BOOTX64.EFI", efi),
        ("JUSTOS/KERNEL.BIN", inputs.kernel_bin()),
        ("JUSTOS/BOOTPACK.IMG", inputs.pack.clone()),
        ("JUSTOS/SYSTEM.IMG", system.clone()),
    ];
    let (fat_image, offsets) = fat::build(fat_size, 0, files)?;
    let offset_of = |name: &str| offsets.iter().find(|(n, _)| n == name).map(|(_, o)| *o).unwrap();

    let mut iso = iso::Iso::new("JUSTOS");
    let stage1_at = iso.reserve(iso::SECTOR as u64);
    let stage2_at = iso.reserve(inputs.stage2.len() as u64);
    let kernel_at = iso.reserve(inputs.flat.len() as u64);
    let fat_at = iso.reserve(fat_image.len() as u64);
    let cd = |sector: u64| sector * (iso::SECTOR as u64 / SECTOR);
    let in_fat = |name: &str| {
        let off = offset_of(name);
        assert!(off % iso::SECTOR as u64 == 0, "{name} is not on a CD sector boundary");
        cd(fat_at) + off / SECTOR
    };
    let mut stage1 = inputs.stage1.clone();
    BootTable {
        stage2_lba: cd(stage2_at),
        stage2_sectors: (inputs.stage2.len() as u64).div_ceil(SECTOR),
        kernel_lba: cd(kernel_at),
        kernel_sectors: (inputs.flat.len() as u64).div_ceil(SECTOR),
        memsz: inputs.memsz,
        pack_lba: in_fat("JUSTOS/BOOTPACK.IMG"),
        pack_sectors: (inputs.pack.len() as u64).div_ceil(SECTOR),
        entry: inputs.entry,
        system_lba: in_fat("JUSTOS/SYSTEM.IMG"),
        system_sectors: (system.len() as u64).div_ceil(SECTOR),
        ctl_lba: 0,
        cd: true,
    }
    .write(&mut stage1);
    iso.put(stage1_at, &stage1);
    iso.put(stage2_at, &inputs.stage2);
    iso.put(kernel_at, &inputs.flat);
    iso.put(fat_at, &fat_image);
    iso.boot_bios(stage1_at, 4);
    iso.boot_efi(fat_at, fat_image.len() as u64);
    let image = iso.finish();
    fs::write(&inputs.out, &image).map_err(|e| format!("{}: {e}", inputs.out))?;
    println!(
        "{}: {} MiB live CD, BIOS and UEFI, system volume {} MiB — {}",
        inputs.out,
        image.len() >> 20,
        system_size >> 20,
        inputs.summary()
    );
    Ok(())
}

// ----------------------------------------------------------------------------
// mkfs

/// A data disk (a USB stick, a second hard disk): an MBR with one JustFS
/// partition from 1 MiB to the end.
fn cmd_mkfs(args: &[String]) -> Result<()> {
    let out = args.first().ok_or("usage: justtool mkfs <image> [--size 64M] [--label Data]")?;
    let mut size = 64 << 20;
    let mut label = String::from("Data");
    let mut i = 1;
    while i < args.len() {
        let value = args.get(i + 1).ok_or_else(|| format!("{}: missing value", args[i]))?;
        match args[i].as_str() {
            "--size" => size = parse_size(value)?,
            "--label" => label = value.clone(),
            other => return Err(format!("unknown option {other}")),
        }
        i += 2;
    }
    let start: u64 = 2048;
    let sectors = size / SECTOR;
    if sectors < start + 2048 {
        return Err("the disk must be at least 2 MiB".into());
    }
    let part = sectors - start;
    let f = OpenOptions::new().read(true).write(true).create(true).truncate(true).open(out).map_err(|e| format!("{out}: {e}"))?;
    f.set_len(size).map_err(|e| format!("{out}: {e}"))?;
    let mut mbr = [0u8; 512];
    let e = &mut mbr[0x1BE..0x1CE];
    e[4] = 0xAC;
    e[8..12].copy_from_slice(&(start as u32).to_le_bytes());
    e[12..16].copy_from_slice(&(part as u32).to_le_bytes());
    mbr[510] = 0x55;
    mbr[511] = 0xAA;
    use std::os::unix::fs::FileExt;
    f.write_all_at(&mbr, 0).map_err(|e| format!("{out}: {e}"))?;
    let dev = FileBlocks { file: f, offset: start * SECTOR, blocks: part * SECTOR / BLOCK_SIZE as u64 };
    let mut fs = Fs::format(dev, &label, &CLOCK).map_err(|e| format!("{out}: {e}"))?;
    fs.sync().map_err(|e| format!("{out}: {e}"))?;
    println!("{out}: {} MiB, JustFS volume \"{label}\"", size >> 20);
    Ok(())
}

// ----------------------------------------------------------------------------
// fs

fn open_volume(image: &str) -> Result<Fs<FileBlocks>> {
    let mut file = OpenOptions::new().read(true).write(true).open(image).map_err(|e| format!("{image}: {e}"))?;
    let (offset, len) = disk::locate(&mut file).map_err(|e| format!("{image}: {e}"))?;
    let dev = FileBlocks { file, offset, blocks: len / BLOCK_SIZE as u64 };
    Fs::mount(dev, &CLOCK).map_err(|e| format!("{image}: {e}"))
}

fn fs_err(path: &str) -> impl Fn(justfs::Error) -> String + '_ {
    move |e| format!("{path}: {e}")
}

fn cmd_fs(args: &[String]) -> Result<()> {
    let (Some(image), Some(cmd)) = (args.first(), args.get(1)) else {
        return Err("usage: justtool fs <image> <ls|cat|put|get|mkdir|rm|check> ...".into());
    };
    let mut fs = open_volume(image)?;
    let a = |i: usize| args.get(i).map(String::as_str).ok_or_else(|| format!("fs {cmd}: missing argument"));
    match cmd.as_str() {
        "ls" => {
            let path = args.get(2).map(String::as_str).unwrap_or("/");
            let id = fs.lookup(path).map_err(fs_err(path))?;
            for e in fs.read_dir(id).map_err(fs_err(path))? {
                let n = fs.node(e.node).map_err(fs_err(path))?;
                if e.kind == Kind::Dir {
                    println!("{:>10}  {}/", "", e.name);
                } else {
                    println!("{:>10}  {}", n.size, e.name);
                }
            }
        }
        "cat" => {
            let path = a(2)?;
            let data = fs.read_file(path).map_err(fs_err(path))?;
            std::io::stdout().write_all(&data).map_err(|e| e.to_string())?;
        }
        "put" => {
            let (local, path) = (a(2)?, a(3)?);
            let data = read(local)?;
            let target = if path.ends_with('/') || fs.lookup(path).is_ok_and(|id| fs.node(id).is_ok_and(|n| n.is_dir())) {
                format!("{}/{}", path.trim_end_matches('/'), Path::new(local).file_name().unwrap().to_string_lossy())
            } else {
                path.to_string()
            };
            fs.write_file(&target, &data).map_err(fs_err(&target))?;
            println!("{local} -> {target} ({} bytes)", data.len());
        }
        "get" => {
            let (path, local) = (a(2)?, a(3)?);
            let data = fs.read_file(path).map_err(fs_err(path))?;
            fs::write(local, data).map_err(|e| format!("{local}: {e}"))?;
        }
        "mkdir" => {
            let path = a(2)?;
            fs.create_dirs(path).map_err(fs_err(path))?;
        }
        "rm" => {
            let path = a(2)?;
            let (dir, name) = fs.lookup_parent(path).map_err(fs_err(path))?;
            fs.remove(dir, name).map_err(fs_err(path))?;
        }
        "check" => {
            let n = fs.check().map_err(fs_err(image))?;
            let (total, free) = fs.space();
            println!("{image}: {n} problems, {} of {} KiB free, label {:?}", free >> 10, total >> 10, fs.superblock().label);
            if n != 0 {
                return Err("the volume has errors".into());
            }
        }
        other => return Err(format!("fs: unknown command {other}")),
    }
    fs.sync().map_err(fs_err(image))
}
