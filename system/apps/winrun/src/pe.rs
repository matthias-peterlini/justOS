//! Loading a PE32+ executable: map its sections, relocate it if it cannot sit
//! at its preferred address, fill the import table, set up a thread block and
//! jump to the entry point.

use crate::win;
use alloc::format;
use alloc::string::{String, ToString};
use justrt::sys;

fn u16_at(b: &[u8], o: usize) -> Result<u16, String> {
    b.get(o..o + 2).map(|s| u16::from_le_bytes(s.try_into().unwrap())).ok_or_else(|| "truncated".to_string())
}
fn u32_at(b: &[u8], o: usize) -> Result<u32, String> {
    b.get(o..o + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap())).ok_or_else(|| "truncated".to_string())
}
fn u64_at(b: &[u8], o: usize) -> Result<u64, String> {
    b.get(o..o + 8).map(|s| u64::from_le_bytes(s.try_into().unwrap())).ok_or_else(|| "truncated".to_string())
}

struct Pe {
    base: u64,
    entry: u64,
    import_dir: (u32, u32),
    reloc_dir: (u32, u32),
    delta: i64,
}

/// Loads and runs the program; returns its exit code.
pub fn run(image: &[u8], argv: &[String]) -> Result<i32, String> {
    let pe = load(image)?;
    win::set_args(argv);
    win::set_image_base(pe.base);
    resolve_imports(image, &pe)?;
    setup_teb()?;
    // SAFETY: `entry` points at mapped, executable code set up above. The
    // program follows the Windows x64 ABI and normally ends by calling
    // ExitProcess or exit (neither returns); if it returns anyway, that is a
    // clean exit with code 0.
    let entry: extern "win64" fn() = unsafe { core::mem::transmute(pe.entry) };
    entry();
    Ok(0)
}

fn load(image: &[u8]) -> Result<Pe, String> {
    if image.len() < 0x40 || &image[..2] != b"MZ" {
        return Err("not an MS-DOS/PE executable".to_string());
    }
    let pe_off = u32_at(image, 0x3C)? as usize;
    if image.get(pe_off..pe_off + 4) != Some(b"PE\0\0") {
        return Err("no PE signature".to_string());
    }
    let coff = pe_off + 4;
    let machine = u16_at(image, coff)?;
    if machine != 0x8664 {
        return Err("not a 64-bit (x64) program".to_string());
    }
    let sections = u16_at(image, coff + 2)? as usize;
    let opt_size = u16_at(image, coff + 16)? as usize;
    let opt = coff + 20;
    if u16_at(image, opt)? != 0x20B {
        return Err("not a PE32+ (64-bit) image".to_string());
    }
    let entry_rva = u32_at(image, opt + 16)?;
    let want_base = u64_at(image, opt + 24)?;
    let size_of_image = u32_at(image, opt + 56)? as u64;
    let size_of_headers = u32_at(image, opt + 60)? as u64;
    let n_dirs = u32_at(image, opt + 108)? as usize;
    let dir = |i: usize| -> Result<(u32, u32), String> {
        if i >= n_dirs {
            return Ok((0, 0));
        }
        let at = opt + 112 + i * 8;
        Ok((u32_at(image, at)?, u32_at(image, at + 4)?))
    };
    let import_dir = dir(1)?;
    let reloc_dir = dir(5)?;

    // Try the program's preferred address first; fall back to one the kernel
    // picks (then we must apply base relocations).
    let (base, delta) = match sys::map_at(want_base, size_of_image) {
        Ok(()) => (want_base, 0i64),
        Err(_) => {
            let got = sys::map(size_of_image).map_err(|e| format!("out of memory: {}", e.as_str()))?;
            (got, got.wrapping_sub(want_base) as i64)
        }
    };

    // Headers, then each section at its virtual address.
    copy_in(base, 0, &image[..size_of_headers.min(image.len() as u64) as usize]);
    let sec_table = opt + opt_size;
    for i in 0..sections {
        let s = sec_table + i * 40;
        let vaddr = u32_at(image, s + 12)? as u64;
        let raw_size = u32_at(image, s + 16)? as usize;
        let raw_ptr = u32_at(image, s + 20)? as usize;
        if raw_size == 0 {
            continue;
        }
        let end = raw_ptr.checked_add(raw_size).ok_or("bad section")?;
        let data = image.get(raw_ptr..end.min(image.len())).ok_or("section outside the file")?;
        copy_in(base, vaddr, data);
    }

    let pe = Pe { base, entry: base + entry_rva as u64, import_dir, reloc_dir, delta };
    if delta != 0 {
        apply_relocations(&pe)?;
    }
    Ok(pe)
}

/// Copies `data` to `base + rva` in the mapped image.
fn copy_in(base: u64, rva: u64, data: &[u8]) {
    // SAFETY: the whole image was mapped writable at `base`, and callers pass
    // offsets and lengths that come from the image's own headers.
    unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), (base + rva) as *mut u8, data.len()) };
}

fn read_u32(addr: u64) -> u32 {
    // SAFETY: `addr` is inside the mapped image.
    unsafe { ((addr) as *const u32).read_unaligned() }
}
fn write_u64(addr: u64, v: u64) {
    // SAFETY: `addr` is inside the mapped image.
    unsafe { ((addr) as *mut u64).write_unaligned(v) };
}

/// Applies base relocations after the image was placed somewhere other than
/// its preferred address.
fn apply_relocations(pe: &Pe) -> Result<(), String> {
    let (rva, size) = pe.reloc_dir;
    if rva == 0 || size == 0 {
        return Err("the program cannot be relocated (no relocation table)".to_string());
    }
    let mut at = pe.base + rva as u64;
    let end = at + size as u64;
    while at + 8 <= end {
        let page_rva = read_u32(at);
        let block_size = read_u32(at + 4);
        if block_size < 8 {
            break;
        }
        let count = (block_size as u64 - 8) / 2;
        for i in 0..count {
            // SAFETY: inside the relocation block, bounded by block_size.
            let e = unsafe { ((at + 8 + i * 2) as *const u16).read_unaligned() };
            let kind = e >> 12;
            let off = (e & 0xFFF) as u64;
            // Type 10 = DIR64 (a full 64-bit pointer); type 0 = padding.
            if kind == 10 {
                let target = pe.base + page_rva as u64 + off;
                // SAFETY: target is inside the mapped image.
                let v = unsafe { (target as *const u64).read_unaligned() };
                write_u64(target, (v as i64).wrapping_add(pe.delta) as u64);
            }
        }
        at += block_size as u64;
    }
    Ok(())
}

/// Reads a NUL-terminated ASCII string from the mapped image.
fn cstr(addr: u64) -> String {
    let mut s = String::new();
    let mut p = addr;
    loop {
        // SAFETY: import names are NUL-terminated inside the image.
        let b = unsafe { *(p as *const u8) };
        if b == 0 {
            break;
        }
        s.push(b as char);
        p += 1;
    }
    s
}

const IMAGE_ORDINAL_FLAG: u64 = 1 << 63;

/// Walks the import directory and writes each resolved function address into
/// the import address table.
fn resolve_imports(_image: &[u8], pe: &Pe) -> Result<(), String> {
    let (rva, size) = pe.import_dir;
    if rva == 0 || size == 0 {
        return Ok(()); // nothing imported
    }
    let mut desc = pe.base + rva as u64;
    loop {
        let original_thunk = read_u32(desc); // ILT (names)
        let name_rva = read_u32(desc + 12);
        let first_thunk = read_u32(desc + 16); // IAT (to fill)
        if name_rva == 0 && first_thunk == 0 {
            break; // the null descriptor ends the list
        }
        let dll = cstr(pe.base + name_rva as u64).to_ascii_lowercase();
        // If there is no separate name table, the IAT itself holds the names.
        let names_at = if original_thunk != 0 { original_thunk } else { first_thunk };
        let mut lookup = pe.base + names_at as u64;
        let mut target = pe.base + first_thunk as u64;
        loop {
            // SAFETY: thunk arrays are NUL-terminated (a zero entry).
            let thunk = unsafe { (lookup as *const u64).read_unaligned() };
            if thunk == 0 {
                break;
            }
            let addr = if thunk & IMAGE_ORDINAL_FLAG != 0 {
                let ordinal = (thunk & 0xFFFF) as u16;
                win::resolve_ordinal(&dll, ordinal)
            } else {
                // The thunk is an RVA to IMAGE_IMPORT_BY_NAME { hint u16, name }.
                let name = cstr(pe.base + (thunk & 0x7FFF_FFFF) + 2);
                win::resolve(&dll, &name)
            };
            let addr = match addr {
                Some(a) => a,
                None => {
                    let what = if thunk & IMAGE_ORDINAL_FLAG != 0 {
                        format!("{dll}#{}", thunk & 0xFFFF)
                    } else {
                        format!("{dll}!{}", cstr(pe.base + (thunk & 0x7FFF_FFFF) + 2))
                    };
                    missing_stub(what)?
                }
            };
            write_u64(target, addr);
            lookup += 8;
            target += 8;
        }
        desc += 20;
    }
    Ok(())
}

/// Code for the imports winrun does not have: each stub passes its name to
/// [`win::missing`] (which says so once and returns 0), so a program that
/// only touches them on a side path still runs.
static mut STUBS: (u64, u64) = (0, 0); // (next free byte, end)

fn missing_stub(name: String) -> Result<u64, String> {
    const SIZE: u64 = 22; // movabs rcx, imm64; movabs rax, imm64; jmp rax
    // SAFETY: single-threaded; the page is ours.
    let (at, end) = unsafe { STUBS };
    let at = if at == 0 || at + SIZE > end {
        let page = sys::map(0x4000).map_err(|e| format!("out of memory: {}", e.as_str()))?;
        // SAFETY: as above.
        unsafe { STUBS = (page, page + 0x4000) };
        page
    } else {
        at
    };
    let mut cname = name.into_bytes();
    cname.push(0);
    let cname = cname.leak().as_ptr() as u64;
    let mut code = [0u8; SIZE as usize];
    code[0..2].copy_from_slice(&[0x48, 0xB9]);
    code[2..10].copy_from_slice(&cname.to_le_bytes());
    code[10..12].copy_from_slice(&[0x48, 0xB8]);
    code[12..20].copy_from_slice(&(win::missing as *const () as u64).to_le_bytes());
    code[20..22].copy_from_slice(&[0xFF, 0xE0]);
    // SAFETY: inside the mapped page (user memory is executable).
    unsafe {
        core::ptr::copy_nonoverlapping(code.as_ptr(), at as *mut u8, code.len());
        STUBS.0 = at + 24;
    }
    Ok(at)
}

/// Sets up a minimal thread environment block, which the C runtime and the
/// stack-check code reach through the GS segment (`GS:[0x30]` is the TEB
/// itself, `GS:[0x60]` the process block).
fn setup_teb() -> Result<(), String> {
    let mem = sys::map(0x2000).map_err(|e| format!("out of memory: {}", e.as_str()))?;
    let teb = mem;
    let peb = mem + 0x1000;
    // SAFETY: freshly mapped, writable, large enough for the fields we set.
    unsafe {
        *((teb + 0x30) as *mut u64) = teb; // NT_TIB.Self / TEB
        *((teb + 0x60) as *mut u64) = peb; // ProcessEnvironmentBlock
    }
    sys::set_tls(sys::TLS_GS, teb).map_err(|e| format!("cannot set the thread block: {}", e.as_str()))?;
    Ok(())
}
