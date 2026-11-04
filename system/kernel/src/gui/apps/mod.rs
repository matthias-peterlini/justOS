//! The apps of the desktop, the catalogue the launcher shows, and which app
//! opens which file.

mod about;
mod arcstudio;
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
mod ticly;
mod ticly_ai;
mod ticly_do;
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
        "arcstudio" if !dir => Box::new(arcstudio::ArcStudio::new(Some(path))),
        _ => return None,
    })
}

/// The built-in apps: id, name, icon, window kind.
pub const BUILT_IN: [(&str, &str, &str); 20] = [
    ("ticly-do", "ticly Do", "ticly-do"),
    ("ticly-ai", "ticly AI", "ticly-ai"),
    ("browser", "ticly browser", "ticly-browser"),
    ("files", "File", "files"),
    ("textedit", "Editor di testo", "text-editor"),
    ("arcstudio", "ArcStudio", "code"),
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
        "arcstudio" => Box::new(arcstudio::ArcStudio::new(None)),
        "notes" => Box::new(notes::Notes::new()),
        "reminders" => Box::new(reminders::Reminders::new()),
        "calendar" => Box::new(calendar::Calendar::new()),
        "clock" => Box::new(clock::Clock::new()),
        "photos" => Box::new(photos::Photos::new()),
        "music" => Box::new(music::Music::new()),
        "monitor" => Box::new(monitor::Monitor::new()),
        "ticly-do" => Box::new(ticly_do::TiclyDo::new()),
        "ticly-ai" => Box::new(ticly_ai::TiclyAi::new()),
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
    v
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
        // ArcScript sources open in the IDE, which runs them too.
        "as" => Opener::App(Box::new(arcstudio::ArcStudio::new(Some(path)))),
        "exe" | "arc" | "" => Opener::Run(quoted),
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
