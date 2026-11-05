//! What the launcher finds besides apps, as in Spotlight: the result of a
//! calculation, settings, and files and folders in the home by name.

use super::Launch;
use crate::fs;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// Files found at most.
const MAX_FILES: usize = 6;
/// Folders looked into at most, so a big home does not stall the typing.
const MAX_DIRS: usize = 300;

pub struct Found {
    /// The heading of its group: "Calcolo", "Impostazioni", "File".
    pub section: &'static str,
    pub name: String,
    pub detail: String,
    pub icon: &'static str,
    pub launch: Option<Launch>,
    /// A calculation's result, copied when chosen.
    pub copy: Option<String>,
}

/// Settings by the words people look for them with: words, name, where,
/// app.
const SETTINGS: &[(&str, &str, &str, &str)] = &[
    ("sfondo wallpaper immagine desktop", "Sfondo", "Impostazioni", "settings"),
    ("risoluzione schermo display monitor", "Risoluzione dello schermo", "Impostazioni", "settings"),
    ("volume audio suono", "Volume", "Impostazioni", "settings"),
    ("suono avvio chime", "Suono all'avvio", "Impostazioni", "settings"),
    ("tastiera keyboard layout lingua", "Tastiera", "Impostazioni", "settings"),
    ("password sicurezza account", "Password", "Impostazioni", "settings"),
    ("blocco schermo lock automatico", "Blocco automatico", "Impostazioni", "settings"),
    ("nome utente account", "Nome", "Impostazioni", "settings"),
    ("info sistema processore memoria versione cpu ram", "Info sistema", "Processore, memoria, versione", "about"),
    ("monitor attività processi cpu prestazioni", "Monitor attività", "Processi e uso del processore", "monitor"),
    ("aggiornamenti update app negozio store", "Negozio", "App e aggiornamenti", "store"),
    ("cestino trash eliminati", "Cestino", "File eliminati", "trash"),
];

/// A file or folder of the home, for searching by name.
pub struct Item {
    path: String,
    /// The name in lower case.
    key: String,
    dir: bool,
}

/// What [`find`] needs to know about the files: built on the first search
/// and kept while the launcher is open.
pub type Index = Option<Vec<Item>>;

pub fn find(query: &str, index: &mut Index) -> Vec<Found> {
    let q = query.trim().to_lowercase();
    let mut out = Vec::new();
    if q.is_empty() {
        return out;
    }
    if let Some(v) = calculate(&q) {
        out.push(Found {
            section: "Calcolo",
            name: format!("= {v}"),
            detail: String::from("Invio per copiarlo"),
            icon: "calculator",
            launch: None,
            copy: Some(v),
        });
        return out;
    }
    for &(words, name, detail, app) in SETTINGS {
        if words.split(' ').any(|w| w.starts_with(&q)) || name.to_lowercase().contains(&q) {
            let icon = super::BUILT_IN.iter().find(|(id, _, _)| *id == app).map_or("settings", |(_, _, i)| i);
            out.push(Found {
                section: "Impostazioni",
                name: String::from(name),
                detail: String::from(detail),
                icon,
                launch: Some(Launch::App(app)),
                copy: None,
            });
        }
    }
    if q.chars().count() >= 2 {
        let items = index.get_or_insert_with(home_files);
        for it in items.iter().filter(|it| it.key.contains(&q)).take(MAX_FILES) {
            let name = String::from(it.path.rsplit('/').next().unwrap_or(&it.path));
            let shown = it.path.strip_prefix("sys:/home/").unwrap_or(&it.path);
            out.push(Found {
                section: "File",
                detail: String::from(shown.rsplit_once('/').map_or("Inizio", |(d, _)| d)),
                icon: super::files::icon_for(&name, it.dir),
                name,
                launch: Some(Launch::File(it.path.clone())),
                copy: None,
            });
        }
    }
    out
}

/// The files and folders under the home, nearer ones first.
fn home_files() -> Vec<Item> {
    let mut items = Vec::new();
    let mut queue: Vec<String> = alloc::vec![String::from("sys:/home")];
    let mut seen = 0;
    while !queue.is_empty() && seen < MAX_DIRS {
        let dir = queue.remove(0);
        seen += 1;
        let Ok(p) = fs::abs(&dir) else { continue };
        let Ok(list) = fs::list_dir(&p) else { continue };
        for (e, st) in list {
            if e.name.starts_with('.') {
                continue;
            }
            let path = format!("{dir}/{}", e.name);
            let dir = st.kind == justfs::Kind::Dir;
            if dir {
                queue.push(path.clone());
            }
            items.push(Item { key: e.name.to_lowercase(), path, dir });
        }
    }
    items
}

/// The value of an arithmetic expression (+ - * / % ^, brackets, decimals
/// with a dot or a comma), if `s` is one and not just a number.
pub fn calculate(s: &str) -> Option<String> {
    let s = s.strip_prefix('=').unwrap_or(s);
    let chars: Vec<char> = s.chars().filter(|c| !c.is_whitespace()).map(|c| if c == ',' { '.' } else { c }).collect();
    if chars.is_empty() || !chars.iter().any(|c| c.is_ascii_digit()) || !chars.iter().any(|c| "+-*/%^×÷(".contains(*c)) {
        return None;
    }
    let mut p = Parser { s: &chars, i: 0 };
    let v = p.sum()?;
    if p.i != chars.len() || !v.is_finite() {
        return None;
    }
    Some(show(v))
}

/// A number the short way: no ".0", at most nine decimals.
fn show(v: f64) -> String {
    if v == (v as i64) as f64 && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    let s = format!("{v:.9}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    String::from(s).replace('.', ",")
}

struct Parser<'a> {
    s: &'a [char],
    i: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    fn sum(&mut self) -> Option<f64> {
        let mut v = self.product()?;
        while let Some(c @ ('+' | '-')) = self.peek() {
            self.i += 1;
            let r = self.product()?;
            v = if c == '+' { v + r } else { v - r };
        }
        Some(v)
    }

    fn product(&mut self) -> Option<f64> {
        let mut v = self.power()?;
        while let Some(c @ ('*' | '/' | '%' | 'x' | '×' | '÷')) = self.peek() {
            self.i += 1;
            let r = self.power()?;
            v = match c {
                '%' => v % r,
                '/' | '÷' => v / r,
                _ => v * r,
            };
        }
        Some(v)
    }

    fn power(&mut self) -> Option<f64> {
        let base = self.unary()?;
        if self.peek() == Some('^') {
            self.i += 1;
            let e = self.power()?;
            return Some(pow(base, e));
        }
        Some(base)
    }

    fn unary(&mut self) -> Option<f64> {
        match self.peek()? {
            '-' => {
                self.i += 1;
                Some(-self.unary()?)
            }
            '+' => {
                self.i += 1;
                self.unary()
            }
            '(' => {
                self.i += 1;
                let v = self.sum()?;
                if self.peek() != Some(')') {
                    return None;
                }
                self.i += 1;
                Some(v)
            }
            _ => self.number(),
        }
    }

    fn number(&mut self) -> Option<f64> {
        let start = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_digit() || c == '.') {
            self.i += 1;
        }
        if start == self.i {
            return None;
        }
        let text: String = self.s[start..self.i].iter().collect();
        text.parse().ok()
    }
}

/// `b` to the power `e`: whole exponents exactly, others not at all.
fn pow(b: f64, e: f64) -> f64 {
    if e != (e as i64) as f64 || e.abs() > 1024.0 {
        return f64::NAN;
    }
    let mut r = 1.0;
    for _ in 0..(e as i64).abs() {
        r *= b;
    }
    if e < 0.0 {
        1.0 / r
    } else {
        r
    }
}
