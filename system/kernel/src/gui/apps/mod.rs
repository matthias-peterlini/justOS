//! The apps of the desktop, the catalogue the launcher shows, and which app
//! opens which file.

mod about;
mod juststudio;
mod browser;
mod calculator;
mod calendar;
mod clock;
mod files;
mod monitor;
mod music;
mod notes;
mod photos;
mod picker;
mod prefs;
pub mod prompt;
mod reminders;
pub mod search;
pub mod store;
mod terminal;
mod textedit;
mod viewer;

use super::wm::App;
use crate::fs;
use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

pub use files::{icon_for, Files};
pub use viewer::Viewer;

/// Opens a file or folder dropped on an app's dock icon with that app (the
/// app's window kind); None when the app does not take it.
pub fn open_with(kind: &str, path: &str) -> Option<Box<dyn App>> {
    let p = fs::abs(path).ok()?;
    let dir = fs::is_dir(&p);
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    Some(match kind {
        "files" if dir => Box::new(Files::new(path)),
        "files" => Box::new(Files::new(&alloc::format!("{}", p.parent()))),
        "textedit" if !dir => Box::new(textedit::TextEdit::new(Some(path))),
        "photos" | "viewer" if ext == "qoi" => Box::new(Viewer::new(path)),
        "music" if ext == "wav" => Box::new(music::Music::with_file(path)),
        "juststudio" if !dir => Box::new(juststudio::JustStudio::new(Some(path))),
        _ => return None,
    })
}

/// The built-in apps: id, name, icon, window kind.
pub const BUILT_IN: [(&str, &str, &str); 18] = [
    ("browser", "Browser", "browser"),
    ("files", "File", "files"),
    ("textedit", "Editor di testo", "text-editor"),
    ("juststudio", "JustStudio", "code"),
    ("notes", "Note", "notes"),
    ("reminders", "Promemoria", "reminders"),
    ("calendar", "Calendario", "calendar"),
    ("clock", "Orologio", "clock"),
    ("calculator", "Calcolatrice", "calculator"),
    ("photos", "Foto", "photos"),
    ("music", "Musica", "music"),
    ("store", "Negozio", "store"),
    ("settings", "Impostazioni", "settings"),
    ("monitor", "Monitor attività", "activity"),
    ("about", "Info sistema", "info"),
    ("terminal", "Terminale", "terminal"),
    ("trash", "Cestino", "trash"),
    ("viewer", "Anteprima", "image-viewer"),
];

/// Opens an app by id.
pub fn open(id: &str) -> Option<Box<dyn App>> {
    Some(match id {
        "files" => Box::new(Files::new("sys:/home")),
        "terminal" => Box::new(terminal::Terminal::new()),
        "settings" => Box::new(prefs::Prefs::new()),
        "about" => Box::new(about::About::new()),
        "store" => Box::new(store::Store::new()),
        "calculator" => Box::new(calculator::Calculator::new()),
        "textedit" | "textedit-new" => Box::new(textedit::TextEdit::new(None)),
        "juststudio" => Box::new(juststudio::JustStudio::new(None)),
        "notes" => Box::new(notes::Notes::new()),
        "reminders" => Box::new(reminders::Reminders::new()),
        "calendar" => Box::new(calendar::Calendar::new()),
        "clock" => Box::new(clock::Clock::new()),
        "photos" => Box::new(photos::Photos::new()),
        "music" => Box::new(music::Music::new()),
        "monitor" => Box::new(monitor::Monitor::new()),
        "browser" => Box::new(browser::Browser::new(None)),
        "trash" => {
            if let Ok(p) = fs::abs("sys:/home/Cestino") {
                if !fs::exists(&p) {
                    let _ = fs::make_dir(&p);
                }
            }
            Box::new(Files::trash())
        }
        _ => return None,
    })
}

/// Does the app have one window at most?
pub fn single(id: &str) -> bool {
    !matches!(id, "files" | "textedit" | "textedit-new" | "viewer")
}

/// The window kind of an app id.
pub fn kind_of(id: &str) -> &str {
    match id {
        "launcher" => "launcher",
        id => id,
    }
}

/// The name shown for a window: the app's, for the built-in ones.
pub fn app_name(kind: &str, title: &str) -> String {
    match BUILT_IN.iter().find(|(id, _, _)| *id == kind) {
        Some((_, name, _)) => String::from(*name),
        None => String::from(title),
    }
}

/// A program's name from its file name.
pub fn program_name(file: &str) -> String {
    let stem = file.rsplit('/').next().unwrap_or(file);
    let stem = stem.strip_suffix(".app").unwrap_or(stem);
    if let Some(e) = linux_app_for(file) {
        return e.name;
    }
    match crate::store::installed(stem) {
        Some(info) => info.name,
        None => String::from(stem),
    }
}

/// What a launcher entry does.
#[derive(Clone)]
pub enum Launch {
    App(&'static str),
    /// An installed program with a window of its own.
    Spawn(String),
    /// A text program, typed into the terminal.
    Run(String),
    /// A file, with the app made for it; a folder, in File.
    File(String),
}

pub struct Entry {
    pub name: String,
    pub icon: String,
    pub launch: Launch,
}

/// The icon of an installed package: its own picture, if it has one.
pub fn package_icon(id: &str) -> String {
    crate::store::icon(id).unwrap_or_else(|| String::from("window"))
}

/// Everything the launcher lists: the built-in apps and the installed
/// packages.
pub fn catalog() -> Vec<Entry> {
    let mut v: Vec<Entry> = BUILT_IN
        .iter()
        .filter(|(id, _, _)| *id != "viewer")
        .map(|&(id, name, icon)| Entry { name: name.to_string(), icon: icon.to_string(), launch: Launch::App(id) })
        .collect();
    for p in crate::store::installed_all() {
        let launch = if p.gui { Launch::Spawn(crate::store::program_path(&p)) } else { Launch::Run(p.open.clone()) };
        v.push(Entry { icon: package_icon(&p.id), name: p.name, launch });
    }
    v.extend(linux_apps());
    v
}

/// Where Linux programs with a window describe themselves (one .desktop
/// file each), and where their icons are, best size first.
const LINUX_APPS: &str = "sys:/usr/share/applications";
const LINUX_ICONS: [&str; 6] = [
    "sys:/usr/share/icons/hicolor/128x128/apps",
    "sys:/usr/share/icons/hicolor/256x256/apps",
    "sys:/usr/share/icons/hicolor/64x64/apps",
    "sys:/usr/share/icons/hicolor/48x48/apps",
    "sys:/usr/share/icons/hicolor/32x32/apps",
    "sys:/usr/share/pixmaps",
];

/// One .desktop file as a launcher entry: its name (the Italian one if it
/// has one), its program and its icon. `None` for what is not an app to
/// show (NoDisplay, a terminal program, a program that is not there).
fn linux_app(text: &str) -> Option<Entry> {
    let (mut name, mut name_it, mut exec, mut icon) = (None, None, None, None);
    let mut main = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            main = line == "[Desktop Entry]";
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        if !main {
            continue;
        }
        let v = v.trim();
        match k.trim() {
            "Name" => name = Some(v),
            "Name[it]" => name_it = Some(v),
            "Exec" => exec = Some(v),
            "Icon" => icon = Some(v),
            "NoDisplay" | "Hidden" | "Terminal" if v == "true" => return None,
            "Type" if v != "Application" => return None,
            _ => {}
        }
    }
    // The program, without its arguments ("firefox-esr %u").
    let program = exec?.split_whitespace().next()?;
    let path = if program.starts_with('/') { alloc::format!("sys:{program}") } else { alloc::format!("sys:/usr/bin/{program}") };
    if !fs::abs(&path).is_ok_and(|p| fs::exists(&p)) {
        return None;
    }
    let picture = icon.and_then(|icon| {
        if icon.starts_with('/') {
            return Some(alloc::format!("@sys:{icon}"));
        }
        LINUX_ICONS.iter().map(|d| alloc::format!("{d}/{icon}.png")).find(|f| fs::abs(f).is_ok_and(|p| fs::exists(&p))).map(|f| alloc::format!("@{f}"))
    });
    // "Firefox ESR" is Firefox to who uses it.
    let name = name_it.or(name)?.trim_end_matches(" ESR");
    Some(Entry { name: name.to_string(), icon: picture.unwrap_or_else(|| String::from("window")), launch: Launch::Spawn(path) })
}

/// The Linux programs with a window that are in the system (Firefox).
pub fn linux_apps() -> Vec<Entry> {
    let Ok(dir) = fs::abs(LINUX_APPS) else { return Vec::new() };
    let mut v: Vec<Entry> = fs::list_dir(&dir)
        .unwrap_or_default()
        .iter()
        .filter(|(e, _)| e.name.ends_with(".desktop"))
        .filter_map(|(e, _)| fs::abs(&alloc::format!("{LINUX_APPS}/{}", e.name)).ok())
        .filter_map(|p| fs::read_file(&p).ok())
        .filter_map(|d| linux_app(&String::from_utf8_lossy(&d)))
        .collect();
    v.sort_by(|a, b| a.name.cmp(&b.name));
    v
}

/// The name and icon a Linux program goes by on the desktop, from its
/// .desktop file.
pub fn linux_app_for(path: &str) -> Option<Entry> {
    if !path.starts_with("sys:/usr/") {
        return None;
    }
    linux_apps().into_iter().find(|e| matches!(&e.launch, Launch::Spawn(p) if p == path))
}

/// How a file opens.
pub enum Opener {
    /// In a new window of this app.
    App(Box<dyn App>),
    /// A program with a window.
    Spawn(String),
    /// A program started with arguments in a folder (Wine, Firefox, the
    /// command of a .desktop file).
    Start(Start),
    /// A text program, in the terminal.
    Run(String),
    /// A package to install (`store::install_file`).
    Install(String),
    /// Nothing on justOS opens it, and why: title, text, icon.
    Tell(String, String, String),
    None,
}

/// A program to start: without a terminal, its icon waiting in the dock
/// until its first window comes; or, with `terminal`, typed into the
/// terminal (a text program).
pub struct Start {
    /// The program file, and its arguments as one line (quoted where they
    /// have spaces).
    pub program: String,
    pub args: String,
    /// The folder it starts in. For a Linux program "/" is the root of
    /// this folder's volume.
    pub cwd: String,
    pub terminal: bool,
    /// What was opened: opening it again while it starts starts no second
    /// copy.
    pub key: String,
    /// The name it goes by, and the icon its windows will have (their
    /// window kind; None when that cannot be known).
    pub name: String,
    pub icon: Option<String>,
    /// What the notification says while it starts (None: no notification).
    pub note: Option<String>,
    /// What to say if it ends without a window (None: the usual words).
    pub fail: Option<String>,
    /// It may take minutes (Wine preparing its C: drive the first time).
    pub slow: bool,
}

/// Where programs started from the desktop run, unless they say otherwise.
const HOME: &str = "sys:/home";
/// Wine (in the image made by `make wine-image`), and its C: drive (the
/// prefix: `$HOME/.wine`, or what `export WINEPREFIX=` set).
const WINE: &str = "sys:/usr/lib/wine/wine64";
const FIREFOX: &str = "sys:/usr/bin/firefox-esr";
/// What a Linux program with a window links: a toolkit or a display
/// protocol (the names of its DT_NEEDED libraries start like this).
const TOOLKITS: [&str; 6] = ["libwayland-client", "libgtk", "libgdk", "libQt", "libSDL", "libX11"];
const MAC: &str = "Le app per macOS non funzionano su justOS.";
const STARTING: &str = "Si sta avviando: la finestra arriva tra poco.";

fn is_file(path: &str) -> bool {
    fs::abs(path).is_ok_and(|p| fs::exists(&p) && !fs::is_dir(&p))
}

/// The first bytes of a file (fewer if it is shorter).
fn head(path: &str, n: usize) -> Vec<u8> {
    let mut b = alloc::vec![0u8; n];
    let got = fs::abs(path).ok().and_then(|p| fs::read_at(&p, 0, &mut b).ok()).unwrap_or(0);
    b.truncate(got);
    b
}

/// An argument as the terminal and the program loader split them: in
/// quotes when it has spaces.
fn quote(arg: &str) -> String {
    if arg.is_empty() {
        String::from("\"\"")
    } else if arg.contains('"') {
        alloc::format!("'{arg}'")
    } else if arg.contains(char::is_whitespace) || arg.contains('\'') {
        alloc::format!("\"{arg}\"")
    } else {
        String::from(arg)
    }
}

/// The file a symbolic link leads to (`path` itself when it is not one).
/// A Linux program started by a link's path sees that path as
/// /proc/self/exe, and Firefox then looks for its libraries beside the link.
fn real_path(path: &str) -> String {
    let mut cur = String::from(path);
    for _ in 0..8 {
        let Ok(p) = fs::abs(&cur) else { break };
        if !fs::lstat(&p).is_ok_and(|s| s.kind == justfs::Kind::Link) {
            break;
        }
        let Ok(target) = fs::read_link(&p) else { break };
        let (vol, inner) = cur.split_once(':').unwrap_or(("sys", cur.as_str()));
        let joined = if target.starts_with('/') {
            target
        } else {
            alloc::format!("{}/{target}", inner.rsplit_once('/').map_or("", |(dir, _)| dir))
        };
        let mut parts: Vec<&str> = Vec::new();
        for c in joined.split('/') {
            match c {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                c => parts.push(c),
            }
        }
        cur = alloc::format!("{vol}:/{}", parts.join("/"));
    }
    cur
}

fn title_of(path: &str) -> String {
    String::from(path.rsplit('/').next().unwrap_or(path))
}

fn tell(path: &str, text: &str, icon: &str) -> Opener {
    Opener::Tell(title_of(path), String::from(text), String::from(icon))
}

/// How a program the desktop starts by its path (a launcher entry, a
/// double click on a program with a window) is started.
pub fn start_for(path: &str) -> Start {
    let file = path.rsplit('/').next().unwrap_or(path);
    let linux = linux_app_for(path);
    // A big Linux program (Firefox) takes a while to show its window: say
    // that it is on its way.
    let note = linux.as_ref().map(|_| String::from(STARTING));
    // The icon its windows will have (and so their kind, see
    // `window::icon_for` and `window::create`), when known.
    let icon = match linux {
        Some(e) => Some(e.icon),
        None if file.to_ascii_lowercase().ends_with(".app") => Some(package_icon(&file[..file.len() - 4])),
        // A Linux program's windows: the icon of the process, which is
        // named after its file.
        None if head(path, 4) == b"\x7fELF" => Some(super::window::icon_for(file)),
        None => None,
    };
    Start {
        program: String::from(path),
        args: String::new(),
        cwd: String::from(HOME),
        terminal: false,
        key: String::from(path),
        name: program_name(path),
        icon,
        note,
        fail: None,
        slow: false,
    }
}

/// Firefox, when the system has it, opening `arg` (an address or the Linux
/// path of a page).
fn firefox(arg: &str) -> Option<Start> {
    if !is_file(FIREFOX) {
        return None;
    }
    let mut s = start_for(&real_path(FIREFOX));
    s.args = quote(arg);
    s.note = Some(String::from(STARTING));
    Some(s)
}

/// Whether Firefox is open. A second one would not hand the page to it
/// (that goes through D-Bus, which justOS does not have): it would run on
/// the same profile at the same time, which Firefox does not survive.
fn firefox_open() -> bool {
    fs::abs(&real_path(FIREFOX)).is_ok_and(|p| crate::user::running(&p))
}

/// A web address: in Firefox when the system has it (and it is not open
/// already), else in the Browser.
pub fn open_url(url: &str) -> Opener {
    match firefox(url).filter(|_| !firefox_open()) {
        Some(s) => Opener::Start(s),
        None => Opener::App(Box::new(browser::Browser::new(Some(url)))),
    }
}

/// Whether Wine is in the system.
fn has_wine() -> bool {
    is_file(WINE)
}

/// Whether Wine's C: drive is made (else the first start makes it, which
/// takes a minute or more).
fn wine_ready() -> bool {
    let env = crate::linux::extra_env();
    let var = |name: &str| env.iter().find_map(|v| v.strip_prefix(name).and_then(|v| v.strip_prefix('=')).map(String::from));
    let prefix = var("WINEPREFIX").unwrap_or_else(|| alloc::format!("{}/.wine", var("HOME").unwrap_or_else(|| String::from("/home"))));
    is_file(&alloc::format!("sys:{prefix}/system.reg"))
}

/// A Windows file (`ext`: exe, msi, bat, cmd, lnk) run by Wine: the
/// program as Wine's loader takes it (its Linux path), installers through
/// `msiexec /i` and scripts through `cmd /c` (with its Windows path: Wine's
/// Z: drive is the Linux "/"), shortcuts through `start`. It starts in its
/// own folder; Wine picks 64 or 32 bits by itself (WoW64). A console
/// program, or a script, runs in the terminal, where its text shows. None
/// without Wine, or for a file on another volume than Wine's (Wine's "/"
/// is the volume of the folder it starts in).
fn wine(path: &str, ext: &str) -> Option<Opener> {
    if !has_wine() {
        return None;
    }
    let p = fs::abs(path).ok()?;
    if p.volume != "sys" {
        return None;
    }
    let unix = p.inner.clone();
    let dos = alloc::format!("Z:{}", unix.replace('/', "\\"));
    let file = p.name();
    let stem = file.rsplit_once('.').map_or(file, |(s, _)| s);
    let (args, terminal) = match ext {
        "msi" => (alloc::format!("msiexec /i {}", quote(&dos)), false),
        "bat" | "cmd" => (alloc::format!("cmd /c {}", quote(&dos)), true),
        "lnk" => (alloc::format!("start /wait /unix {}", quote(&unix)), false),
        _ => (quote(&unix), !windowed_exe(path)),
    };
    let first = !wine_ready();
    let note = if first {
        Some("Prima apertura: preparo l'ambiente Windows…")
    } else if terminal {
        None
    } else {
        Some("Si sta avviando con Wine: la finestra arriva tra poco.")
    };
    Some(Opener::Start(Start {
        program: String::from(WINE),
        args,
        cwd: alloc::format!("{}", p.parent()),
        terminal,
        key: String::from(path),
        name: String::from(stem),
        // Wine's processes are all named after its loader.
        icon: Some(super::window::icon_for("wine64")),
        note: note.map(String::from),
        fail: Some(String::from("Wine non è riuscito ad avviarlo.")),
        slow: first,
    }))
}

/// A Windows file: through Wine when the system has it; else a program
/// through `winrun` (with a window of its own: no terminal), and the rest
/// cannot open.
fn windows(path: &str, ext: &str) -> Opener {
    if let Some(o) = wine(path, ext) {
        return o;
    }
    match ext {
        "exe" if windowed_exe(path) => Opener::Spawn(String::from(path)),
        "exe" => Opener::Run(quote(path)),
        _ => tell(path, "Per aprire questo file di Windows serve Wine, che non è in questo sistema.", "file-exe"),
    }
}

/// The libraries a dynamically linked ELF names (its DT_NEEDED entries),
/// read from its dynamic section without reading the whole file.
fn elf_needed(path: &str) -> Vec<String> {
    let Ok(p) = fs::abs(path) else { return Vec::new() };
    let read = |off: u64, len: usize| {
        let mut b = alloc::vec![0u8; len];
        let n = fs::read_at(&p, off, &mut b).unwrap_or(0);
        b.truncate(n);
        b
    };
    let u16_at = |b: &[u8], o: usize| b.get(o..o + 2).map(|s| u16::from_le_bytes([s[0], s[1]]));
    let u32_at = |b: &[u8], o: usize| b.get(o..o + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]));
    let u64_at = |b: &[u8], o: usize| b.get(o..o + 8).map(|s| u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]));
    let eh = read(0, 64);
    // 64-bit, little-endian.
    if eh.len() < 64 || &eh[..4] != b"\x7fELF" || eh[4] != 2 || eh[5] != 1 {
        return Vec::new();
    }
    let (Some(phoff), Some(phent), Some(phnum)) = (u64_at(&eh, 32), u16_at(&eh, 54), u16_at(&eh, 56)) else { return Vec::new() };
    let (phent, phnum) = (phent as usize, phnum.min(64) as usize);
    if phent < 56 {
        return Vec::new();
    }
    let ph = read(phoff, phent * phnum);
    // PT_LOAD segments (address, file offset, size) and PT_DYNAMIC.
    let mut loads = Vec::new();
    let mut dynamic = None;
    for i in 0..phnum {
        let o = i * phent;
        let (Some(kind), Some(off), Some(vaddr), Some(size)) = (u32_at(&ph, o), u64_at(&ph, o + 8), u64_at(&ph, o + 16), u64_at(&ph, o + 32)) else { break };
        match kind {
            1 => loads.push((vaddr, off, size)),
            2 => dynamic = Some((off, size)),
            _ => {}
        }
    }
    let Some((off, size)) = dynamic else { return Vec::new() };
    let dyn_ = read(off, (size as usize).min(16 << 10));
    let (mut needed, mut strtab, mut strsz) = (Vec::new(), None, 0u64);
    for e in dyn_.chunks_exact(16) {
        let (tag, val) = (u64_at(e, 0).unwrap_or(0), u64_at(e, 8).unwrap_or(0));
        match tag {
            0 => break,
            1 => needed.push(val),
            5 => strtab = Some(val),
            10 => strsz = val,
            _ => {}
        }
    }
    // The string table is named by its address: where that is in the file.
    let Some(at) = strtab.and_then(|a| loads.iter().find(|(v, _, s)| a >= *v && a < v + s).map(|(v, o, _)| a - v + o)) else { return Vec::new() };
    let table = read(at, (strsz as usize).clamp(1, 256 << 10));
    needed
        .iter()
        .filter_map(|&i| {
            let rest = table.get(i as usize..)?;
            let end = rest.iter().position(|&b| b == 0)?;
            core::str::from_utf8(&rest[..end]).ok().map(String::from)
        })
        .collect()
}

/// A Linux program: one with a window (it has a .desktop file, or links a
/// toolkit) starts by itself, a text one in the terminal; an AppImage
/// unpacks itself first.
fn linux(path: &str) -> Opener {
    let path = &real_path(path);
    let h = head(path, 12);
    // An AppImage: "AI" and its type after the ELF identification.
    if h.len() == 12 && &h[8..10] == b"AI" {
        return appimage(path, h[10]);
    }
    let gui = linux_app_for(path).is_some() || elf_needed(path).iter().any(|lib| TOOLKITS.iter().any(|t| lib.starts_with(t)));
    if gui {
        Opener::Spawn(String::from(path))
    } else {
        Opener::Run(quote(path))
    }
}

/// An AppImage (`kind` 2: a program followed by a squashfs image of the
/// app). Run as it is, it would mount that image with FUSE, which justOS
/// does not have: `--appimage-extract-and-run` has it unpack the app into
/// /tmp (on the system volume) and run it from there.
fn appimage(path: &str, kind: u8) -> Opener {
    if kind != 2 {
        return tell(path, "Questo tipo di AppImage (il primo, ormai superato) non si apre su justOS.", "file");
    }
    let Ok(p) = fs::abs(path) else { return Opener::None };
    if p.volume != "sys" {
        return tell(path, "Per aprire un'AppImage, copiala prima in una cartella di Inizio.", "file");
    }
    let file = p.name();
    let stem = file.rsplit_once('.').map_or(file, |(s, _)| s);
    Opener::Start(Start {
        program: String::from(path),
        args: String::from("--appimage-extract-and-run"),
        cwd: alloc::format!("{}", p.parent()),
        terminal: false,
        key: String::from(path),
        name: String::from(stem.split(['-', '_']).next().unwrap_or(stem)),
        // The window belongs to the program unpacked from it, whose name is
        // not known here: a window with no icon of its own.
        icon: Some(String::from("window")),
        note: Some(String::from("Un'AppImage si estrae prima di partire: ci vuole un po'.")),
        fail: Some(String::from("Questa AppImage non si è avviata: su justOS servono i programmi per Wayland, senza FUSE.")),
        slow: true,
    })
}

/// The Exec line of a .desktop file as a program and its arguments: the
/// field codes (%f, %u…) go, `env NAME=value` before the program too.
fn exec_words(exec: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut any = false;
    for c in exec.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    words.push(core::mem::take(&mut cur));
                    any = false;
                }
            }
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if any {
        words.push(cur);
    }
    let words: Vec<String> = words
        .into_iter()
        .filter_map(|w| {
            // "%%" is a "%"; any other code stands for files and names the
            // desktop would fill in.
            let mut out = String::new();
            let mut chars = w.chars();
            while let Some(c) = chars.next() {
                if c != '%' {
                    out.push(c);
                } else if chars.next() == Some('%') {
                    out.push('%');
                }
            }
            (!out.is_empty() || w.is_empty()).then_some(out)
        })
        .collect();
    let skip = if words.first().is_some_and(|w| w == "env") { 1 + words[1..].iter().take_while(|w| w.contains('=')).count() } else { 0 };
    words.into_iter().skip(skip).collect()
}

/// Where a program named in a .desktop file is: a path as it is, a name
/// along the Linux programs' PATH.
fn find_linux_program(name: &str) -> Option<String> {
    let found = if name.starts_with('/') {
        Some(alloc::format!("sys:{name}")).filter(|p| is_file(p))
    } else {
        ["sys:/apps", "sys:/usr/local/bin", "sys:/usr/bin", "sys:/bin", "sys:/usr/games"].iter().map(|d| alloc::format!("{d}/{name}")).find(|p| is_file(p))
    };
    found.map(|p| real_path(&p))
}

/// A .desktop file: its program (Exec, in the terminal with Terminal=true,
/// in its folder with Path=), or the address of a link (Type=Link).
fn desktop_file(path: &str) -> Opener {
    let Some(data) = fs::abs(path).ok().and_then(|p| fs::read_file(&p).ok()) else { return Opener::None };
    let text = String::from_utf8_lossy(&data);
    let (mut name, mut name_it, mut exec, mut url, mut dir) = (None, None, None, None, None);
    let (mut terminal, mut kind) = (false, "Application");
    let mut main = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            main = line == "[Desktop Entry]";
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        if !main {
            continue;
        }
        let v = v.trim();
        match k.trim() {
            "Name" => name = Some(v),
            "Name[it]" => name_it = Some(v),
            "Exec" => exec = Some(v),
            "URL" => url = Some(v),
            "Path" if !v.is_empty() => dir = Some(v),
            "Terminal" => terminal = v == "true",
            "Type" => kind = v,
            _ => {}
        }
    }
    let shown = String::from(name_it.or(name).unwrap_or(""));
    if kind == "Link" {
        return match url {
            Some(u) if u.starts_with("http://") || u.starts_with("https://") => open_url(u),
            Some(u) => for_file(&alloc::format!("sys:{}", u.strip_prefix("file://").unwrap_or(u))),
            None => Opener::None,
        };
    }
    let words = exec.map(exec_words).unwrap_or_default();
    let Some(program) = words.first() else { return tell(path, "Questo file .desktop non dice quale programma avviare.", "file") };
    let Some(found) = find_linux_program(program) else {
        return tell(path, &alloc::format!("Il programma «{program}» non è in questo sistema."), "file");
    };
    let args: Vec<String> = words[1..].iter().map(|w| quote(w)).collect();
    let file = found.rsplit('/').next().unwrap_or(&found);
    let cwd = dir.map(|d| alloc::format!("sys:{d}")).filter(|d| fs::abs(d).is_ok_and(|p| fs::is_dir(&p))).unwrap_or_else(|| String::from(HOME));
    // A script's windows are its interpreter's or another program's: which
    // ones is not known.
    let script = head(&found, 2) == b"#!";
    Opener::Start(Start {
        program: found.clone(),
        args: args.join(" "),
        cwd,
        terminal,
        key: String::from(path),
        name: if shown.is_empty() { String::from(file) } else { shown },
        icon: (!terminal && !script).then(|| super::window::icon_for(file)),
        note: (!terminal).then(|| String::from(STARTING)),
        fail: None,
        slow: false,
    })
}

/// A Debian package. Debian's own (and Ubuntu's) pack their files with xz
/// or zstd, which justOS cannot unpack yet: say so. A package packed with
/// gzip (or not at all) goes to the installer, as before.
fn deb(path: &str) -> Opener {
    match deb_data(path).as_deref() {
        Some("data.tar") | Some("data.tar.gz") => Opener::Install(String::from(path)),
        _ => tell(path, "I pacchetti .deb non si installano ancora.", "store"),
    }
}

/// The name of a .deb's data member (data.tar.xz…), from the `ar` headers
/// at its start.
fn deb_data(path: &str) -> Option<String> {
    let p = fs::abs(path).ok()?;
    let mut magic = [0u8; 8];
    if fs::read_at(&p, 0, &mut magic).ok()? != 8 || &magic != b"!<arch>\n" {
        return None;
    }
    let mut at = 8u64;
    for _ in 0..4 {
        let mut h = [0u8; 60];
        if fs::read_at(&p, at, &mut h).ok()? != 60 {
            return None;
        }
        let name = core::str::from_utf8(&h[..16]).ok()?.trim_end().trim_end_matches('/');
        if name.starts_with("data.tar") {
            return Some(String::from(name));
        }
        let size: u64 = core::str::from_utf8(&h[48..58]).ok()?.trim().parse().ok()?;
        at += 60 + size + (size & 1);
    }
    None
}

/// A Mach-O program (macOS), from its first bytes: either byte order, 32
/// or 64 bits, or a universal one (CAFEBABE and the number of programs in
/// it, which a Java class file, with the same four bytes, has not: a
/// version of 45 or more is there).
fn mach_o(h: &[u8]) -> bool {
    if h.len() < 8 {
        return false;
    }
    let m = [h[0], h[1], h[2], h[3]];
    let thin = [[0xCF, 0xFA, 0xED, 0xFE], [0xCE, 0xFA, 0xED, 0xFE], [0xFE, 0xED, 0xFA, 0xCF], [0xFE, 0xED, 0xFA, 0xCE]];
    let fat = m == [0xCA, 0xFE, 0xBA, 0xBE] || m == [0xCA, 0xFE, 0xBA, 0xBF];
    thin.contains(&m) || fat && (1..20).contains(&u32::from_be_bytes([h[4], h[5], h[6], h[7]]))
}

/// A macOS app: a folder named *.app with Contents/MacOS in it.
pub fn mac_bundle(path: &str) -> bool {
    path.to_ascii_lowercase().trim_end_matches('/').ends_with(".app") && fs::abs(&alloc::format!("{}/Contents/MacOS", path.trim_end_matches('/'))).is_ok_and(|p| fs::is_dir(&p))
}

/// Whether a file's start reads as text (UTF-8, no NUL bytes).
fn text_like(h: &[u8]) -> bool {
    !h.contains(&0)
        && match core::str::from_utf8(h) {
            Ok(_) => true,
            // Cut in the middle of a character at the end: still text.
            Err(e) => e.error_len().is_none(),
        }
}

/// Whether a Windows program is a windowed one (PE subsystem 2, what
/// `-mwindows` builds) rather than a console one.
fn windowed_exe(path: &str) -> bool {
    let Ok(p) = fs::abs(path) else { return false };
    let mut head = [0u8; 64];
    if fs::read_at(&p, 0, &mut head).ok() != Some(64) || &head[..2] != b"MZ" {
        return false;
    }
    let pe = u32::from_le_bytes([head[60], head[61], head[62], head[63]]) as u64;
    // The subsystem: 4 (signature) + 20 (file header) + 68 into the
    // optional header.
    let mut sub = [0u8; 2];
    fs::read_at(&p, pe + 92, &mut sub).ok() == Some(2) && u16::from_le_bytes(sub) == 2
}

/// How a file (or a web address) opens.
pub fn for_file(path: &str) -> Opener {
    let lower = path.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return open_url(path);
    }
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    let quoted = quote(path);
    // A folder comes here only as a macOS app.
    if fs::abs(path).is_ok_and(|p| fs::is_dir(&p)) {
        return if mac_bundle(path) { tell(path, MAC, "file") } else { Opener::None };
    }
    // A macOS installer .pkg goes to the store's installer below, which
    // unpacks it and says what it found.
    match ext {
        "deb" => return deb(path),
        "dmg" => return tell(path, MAC, "file"),
        _ => {}
    }
    if crate::store::is_package(&name) {
        return Opener::Install(String::from(path));
    }
    // Web pages in Firefox, when the system has it (it sees the files of
    // the system volume only).
    if matches!(ext, "html" | "htm" | "xhtml") {
        if let Some(s) = fs::abs(path).ok().filter(|p| p.volume == "sys").and_then(|p| firefox(&p.inner)) {
            if firefox_open() {
                let icon = s.icon.unwrap_or_else(|| String::from("window"));
                return tell(path, "Firefox è già aperto: apri la pagina da lì (Ctrl+O), oppure chiudilo e riprova.", &icon);
            }
            return Opener::Start(s);
        }
    }
    match ext {
        "qoi" => Opener::App(Box::new(Viewer::new(path))),
        "wav" => Opener::App(Box::new(music::Music::with_file(path))),
        "txt" | "md" | "toml" | "json" | "rs" | "c" | "h" | "sh" | "py" | "ini" | "cfg" | "log" | "csv" | "html" | "htm" | "xhtml" | "css" | "js" => {
            Opener::App(Box::new(textedit::TextEdit::new(Some(path))))
        }
        "desktop" => desktop_file(path),
        "app" => {
            let stem = name.strip_suffix(".app").unwrap_or(&name);
            if crate::store::installed(stem).is_some_and(|p| p.gui) {
                Opener::Spawn(String::from(path))
            } else {
                Opener::Run(quoted)
            }
        }
        // Windows programs, installers, scripts and shortcuts: Wine when
        // the system has it; else a program with a window of its own needs
        // no terminal.
        "exe" | "msi" | "bat" | "cmd" | "lnk" => windows(path, ext),
        // JustScript sources open in the IDE, which runs them too.
        "jt" => Opener::App(Box::new(juststudio::JustStudio::new(Some(path)))),
        // A JustScript package runs through `just`, like a program.
        "jpk" => Opener::Run(quoted),
        // Android apps get a window; Java programs (DEX) a terminal.
        "apk" => Opener::Spawn(String::from(path)),
        "dex" | "jar" => Opener::Run(quoted),
        // Anything else by its first bytes: a program runs (a Linux one
        // with a window by itself), a macOS one cannot, and a file with no
        // extension that is text opens in the editor.
        _ => {
            let h = head(path, 512);
            if h.starts_with(b"\x7fELF") {
                linux(path)
            } else if h.starts_with(b"MZ") || h.starts_with(b"dex\n") {
                Opener::Run(quoted)
            } else if mach_o(&h) {
                tell(path, MAC, "file")
            } else if ext.is_empty() && !h.is_empty() && text_like(&h) {
                Opener::App(Box::new(textedit::TextEdit::new(Some(path))))
            } else {
                Opener::None
            }
        }
    }
}
