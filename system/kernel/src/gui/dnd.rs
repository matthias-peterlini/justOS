//! Carrying files between windows: where they land, and whether they are
//! moved or copied there.
//!
//! A drop moves files within a volume and copies them onto another one;
//! Ctrl or Alt held copies anyway, Shift moves anyway. A name already taken
//! in the folder gets a number ("foto 2.qoi"). A folder never goes inside
//! itself.

use crate::fs::{self, Path};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// The bin.
pub const BIN: &str = "sys:/home/Cestino";

/// A name for `name` that is free in `dir`: the name itself, or with a
/// number before the extension ("foto 2.qoi", "foto 3.qoi"...).
pub fn free_name(dir: &Path, name: &str) -> String {
    if !fs::exists(&dir.join(name)) {
        return String::from(name);
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() && !e.is_empty() && !e.contains(' ') => (s, format!(".{e}")),
        _ => (name, String::new()),
    };
    let mut n = 2;
    loop {
        let candidate = format!("{stem} {n}{ext}");
        if !fs::exists(&dir.join(&candidate)) {
            return candidate;
        }
        n += 1;
    }
}

/// Should files carried to the folder `dir` be copied rather than moved?
/// Ctrl or Alt held: copied; Shift: moved; otherwise copied only when they
/// come from another volume.
pub fn copying(paths: &[String], dir: &Path) -> bool {
    let (ctrl, alt, shift) = crate::dev::input::mods();
    if shift {
        return false;
    }
    ctrl || alt || paths.iter().any(|p| fs::abs(p).is_ok_and(|p| p.volume != dir.volume))
}

/// Moves (or copies) files and folders into the folder `dir`. Files already
/// there are left alone when moving and duplicated when copying. Returns
/// how many went; an error names the first that could not (the ones before
/// it have gone).
pub fn transfer(paths: &[String], dir: &Path, copy: bool) -> Result<usize, String> {
    if !fs::is_dir(dir) {
        return Err(format!("{} non è una cartella", dir.name()));
    }
    let mut done = 0;
    for p in paths {
        let from = fs::abs(p).map_err(|e| format!("{p}: {e}"))?;
        let name = String::from(from.name());
        if name.is_empty() {
            return Err(format!("{}: non si può spostare un volume", from.volume));
        }
        if !fs::exists(&from) {
            return Err(format!("{name} non c'è più"));
        }
        if from.parent() == *dir && !copy {
            continue;
        }
        if dir.within(&from) {
            return Err(format!("{name} non può andare dentro sé stessa"));
        }
        let to = dir.join(&free_name(dir, &name));
        let r = if copy {
            fs::copy_all(&from, &to)
        } else if from.volume == dir.volume {
            fs::rename(&from, &to)
        } else {
            fs::copy_all(&from, &to).and_then(|_| fs::remove_all(&from))
        };
        if let Err(e) = r {
            if copy || from.volume != dir.volume {
                // Do not leave half a copy behind.
                if fs::exists(&from) {
                    let _ = fs::remove_all(&to);
                }
            }
            return Err(format!("{name}: {e}"));
        }
        done += 1;
    }
    Ok(done)
}

/// Moves files to the bin.
pub fn to_bin(paths: &[String]) -> Result<usize, String> {
    let bin = fs::abs(BIN).map_err(|e| format!("{e}"))?;
    if !fs::exists(&bin) {
        fs::make_dir(&bin).map_err(|e| format!("{e}"))?;
    }
    transfer(paths, &bin, false)
}

/// "foto.qoi" or "3 elementi": what is being carried, in words.
pub fn describe(paths: &[String]) -> String {
    match paths {
        [one] => fs::abs(one).map(|p| String::from(p.name())).unwrap_or_default(),
        _ => format!("{} elementi", paths.len()),
    }
}

// ----------------------------------------------------------------------------
// `dnd test`

/// Moves and copies files and folders in a folder of its own, the way drops
/// do, and checks where everything ends up. Prints what failed; true if
/// nothing did.
pub fn self_test() -> bool {
    let mut failed = 0;
    let mut check = |ok: bool, what: &str| {
        if !ok {
            crate::println!("\x1b[91mdnd: {what}\x1b[0m");
            failed += 1;
        }
    };
    let Ok(base) = fs::abs("sys:/home/.dnd-test") else { return false };
    let _ = fs::remove_all(&base);
    let p = |s: &str| fs::abs(&format!("sys:/home/.dnd-test/{s}")).unwrap_or_else(|_| base.clone());
    let s = |s: &str| format!("sys:/home/.dnd-test/{s}");
    let holds = |path: &Path, data: &[u8]| fs::read_file(path).is_ok_and(|d| d == data);
    let ok = fs::make_dir(&p("a/inner")).is_ok()
        && fs::make_dir(&p("b")).is_ok()
        && fs::write_file(&p("a/inner/x.txt"), b"x").is_ok()
        && fs::write_file(&p("foto.qoi"), b"q").is_ok()
        && fs::write_file(&p("b/foto.qoi"), b"old").is_ok();
    check(ok, "cannot make the test folder");

    // Names.
    check(free_name(&p("b"), "nuovo.qoi") == "nuovo.qoi", "free name of a free one");
    check(free_name(&p("b"), "foto.qoi") == "foto 2.qoi", "free name before the extension");
    check(free_name(&base, "a") == "a 2", "free name of a folder");

    // A move into a folder where the name is taken.
    check(transfer(&[s("foto.qoi")], &p("b"), false) == Ok(1), "move a file");
    check(!fs::exists(&p("foto.qoi")) && holds(&p("b/foto 2.qoi"), b"q"), "the moved file");
    check(holds(&p("b/foto.qoi"), b"old"), "the file that was there");

    // Moving to where it is already: nothing; copying there: a duplicate.
    check(transfer(&[s("b/foto.qoi")], &p("b"), false) == Ok(0), "move to the same folder");
    check(transfer(&[s("b/foto.qoi")], &p("b"), true) == Ok(1) && fs::exists(&p("b/foto 3.qoi")), "copy to the same folder");

    // A folder never goes inside itself.
    check(transfer(&[s("a")], &p("a"), false).is_err(), "a folder into itself");
    check(transfer(&[s("a")], &p("a/inner"), true).is_err(), "a folder into its subfolder");
    check(fs::exists(&p("a/inner/x.txt")), "the folder after a refused move");

    // Folders copy with everything in them, then move.
    check(transfer(&[s("a")], &p("b"), true) == Ok(1), "copy a folder");
    check(holds(&p("b/a/inner/x.txt"), b"x") && fs::exists(&p("a/inner/x.txt")), "the copied folder");
    check(transfer(&[s("a")], &p("b"), false) == Ok(1), "move a folder");
    check(!fs::exists(&p("a")) && fs::exists(&p("b/a 2/inner/x.txt")), "the moved folder");

    // Several at once; one missing stops there.
    let r = transfer(&[s("b/foto.qoi"), s("b/nessuno"), s("b/foto 2.qoi")], &base, false);
    check(r.is_err() && fs::exists(&p("foto.qoi")) && fs::exists(&p("b/foto 2.qoi")), "a missing file among several");
    check(transfer(&[], &p("foto.qoi"), false).is_err(), "a drop on a file");

    let _ = fs::remove_all(&base);
    if failed == 0 {
        crate::println!("dnd: all checks passed");
    }
    failed == 0
}

/// The paths as one line for a command: each quoted when it has a space.
pub fn as_words(paths: &[String]) -> String {
    let words: Vec<String> = paths.iter().map(|p| if p.contains(' ') { format!("\"{p}\"") } else { p.clone() }).collect();
    words.join(" ")
}
