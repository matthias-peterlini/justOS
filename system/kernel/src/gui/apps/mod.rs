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
    /// A text program, in the terminal.
    Run(String),
    /// A package to install (`store::install_file`).
    Install(String),
    None,
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

pub fn for_file(path: &str) -> Opener {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    let quoted = if path.contains(' ') { alloc::format!("\"{path}\"") } else { String::from(path) };
    if crate::store::is_package(&name) {
        return Opener::Install(String::from(path));
    }
    match ext {
        "qoi" => Opener::App(Box::new(Viewer::new(path))),
        "wav" => Opener::App(Box::new(music::Music::with_file(path))),
        "txt" | "md" | "toml" | "json" | "rs" | "c" | "h" | "sh" | "py" | "ini" | "cfg" | "log" | "csv" | "html" | "css" | "js" => {
            Opener::App(Box::new(textedit::TextEdit::new(Some(path))))
        }
        "app" => {
            let stem = name.strip_suffix(".app").unwrap_or(&name);
            if crate::store::installed(stem).is_some_and(|p| p.gui) {
                Opener::Spawn(String::from(path))
            } else {
                Opener::Run(quoted)
            }
        }
        // A Windows program with a window of its own needs no terminal.
        "exe" if windowed_exe(path) => Opener::Spawn(String::from(path)),
        // JustScript sources open in the IDE, which runs them too.
        "jt" => Opener::App(Box::new(juststudio::JustStudio::new(Some(path)))),
        // A JustScript package runs through `just`, like a program.
        "exe" | "jpk" | "" => Opener::Run(quoted),
        // Android apps get a window; Java programs (DEX) a terminal.
        "apk" => Opener::Spawn(String::from(path)),
        "dex" | "jar" => Opener::Run(quoted),
        // Anything else that is a program (by its first bytes) runs too.
        _ => {
            let mut magic = [0u8; 4];
            let n = fs::abs(path).ok().and_then(|p| fs::read_at(&p, 0, &mut magic).ok()).unwrap_or(0);
            if n == 4 && (magic == *b"\x7fELF" || magic.starts_with(b"MZ")) {
                Opener::Run(quoted)
            } else {
                Opener::None
            }
        }
    }
}
