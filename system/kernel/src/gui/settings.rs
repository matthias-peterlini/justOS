//! The user's settings, kept in `sys:/system/settings` as `key=value` lines.

use crate::dev::input;
use crate::dev::keymap::Layout;
use crate::fs;
use crate::sync::IrqLock;
use alloc::format;
use alloc::string::String;

const FILE: &str = "sys:/system/settings";

#[derive(Clone)]
pub struct Settings {
    pub name: String,
    pub layout: Layout,
    pub wallpaper: String,
    pub setup_done: bool,
    /// Screen resolution chosen by the user (needs a display driver).
    pub resolution: Option<(u32, u32)>,
    /// Sound volume, 0 to 100.
    pub volume: u32,
    /// Play a sound when the desktop appears.
    pub startup_sound: bool,
    /// The password that unlocks the computer, as "salt:hash" in hex
    /// (empty: no password).
    pub password: String,
    /// Lock the screen after this many minutes without input (0: never).
    pub lock_after: u32,
    /// Pixels per unit: 0 chooses by the screen, 1 or 2 as the user wants
    /// (the size of the interface).
    pub scale: u32,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            name: String::new(),
            layout: Layout::It,
            wallpaper: String::from(super::res::WALLPAPERS[0].0),
            setup_done: false,
            resolution: None,
            volume: 70,
            startup_sound: true,
            password: String::new(),
            lock_after: 5,
            scale: 0,
        }
    }
}

static CURRENT: IrqLock<Option<Settings>> = IrqLock::new(None);

fn load() -> Settings {
    let mut s = Settings::default();
    let Ok(bytes) = fs::abs(FILE).and_then(|p| fs::read_file(&p)) else { return s };
    for line in String::from_utf8_lossy(&bytes).lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim();
        match k.trim() {
            "name" => s.name = String::from(v),
            "layout" => s.layout = if v == "us" { Layout::Us } else { Layout::It },
            "wallpaper" => s.wallpaper = String::from(v),
            "setup" => s.setup_done = v == "done",
            "resolution" => {
                s.resolution = v.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)));
            }
            "volume" => s.volume = v.parse::<u32>().unwrap_or(70).min(100),
            "startup-sound" => s.startup_sound = v != "off",
            "password" => s.password = String::from(v),
            "lock-after" => s.lock_after = v.parse().unwrap_or(5),
            "scale" => s.scale = v.parse::<u32>().unwrap_or(0).min(2),
            _ => {}
        }
    }
    s
}

/// The current settings (read from disk the first time).
pub fn get() -> Settings {
    let mut cur = CURRENT.lock();
    if cur.is_none() {
        let s = load();
        if s.setup_done {
            input::set_layout(s.layout);
        }
        crate::dev::audio::set_volume(s.volume);
        *cur = Some(s);
    }
    cur.clone().unwrap()
}

/// Changes the settings, applies them and writes them to disk.
pub fn update(f: impl FnOnce(&mut Settings)) {
    let mut s = get();
    f(&mut s);
    input::set_layout(s.layout);
    crate::dev::audio::set_volume(s.volume);
    let mut text = format!(
        "name={}\nlayout={}\nwallpaper={}\nsetup={}\nvolume={}\nstartup-sound={}\nlock-after={}\n",
        s.name,
        if s.layout == Layout::Us { "us" } else { "it" },
        s.wallpaper,
        if s.setup_done { "done" } else { "no" },
        s.volume,
        if s.startup_sound { "on" } else { "off" },
        s.lock_after
    );
    if !s.password.is_empty() {
        text.push_str(&format!("password={}\n", s.password));
    }
    if let Some((w, h)) = s.resolution {
        text.push_str(&format!("resolution={w}x{h}\n"));
    }
    if s.scale != 0 {
        text.push_str(&format!("scale={}\n", s.scale));
    }
    if let Ok(p) = fs::abs(FILE) {
        if let Err(e) = fs::write_file(&p, text.as_bytes()) {
            crate::warn!("gui", "cannot save the settings: {e}");
        }
    }
    *CURRENT.lock() = Some(s);
}

/// Rounds of SHA-256 over the password: slow enough to make guessing
/// costly, quick enough to unlock without a wait.
const ROUNDS: u32 = 20_000;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn stretch(salt: &str, password: &str) -> String {
    let mut h = justtls::sha2::Sha256::new();
    h.update(salt.as_bytes());
    h.update(password.as_bytes());
    let mut d = h.finish();
    for _ in 0..ROUNDS {
        let mut h = justtls::sha2::Sha256::new();
        h.update(&d);
        h.update(password.as_bytes());
        d = h.finish();
    }
    hex(&d)
}

/// Sets the password that unlocks the computer (empty removes it).
pub fn set_password(password: &str) {
    let stored = if password.is_empty() {
        String::new()
    } else {
        let mut salt = [0u8; 16];
        crate::rand::fill(&mut salt);
        let salt = hex(&salt);
        let hash = stretch(&salt, password);
        format!("{salt}:{hash}")
    };
    update(|s| s.password = stored);
}

/// A password is set.
pub fn has_password() -> bool {
    !get().password.is_empty()
}

/// Whether `password` is the one set (true when none is).
pub fn check_password(password: &str) -> bool {
    let s = get();
    let Some((salt, hash)) = s.password.split_once(':') else { return s.password.is_empty() };
    // Compare every byte, however early they differ.
    let given = stretch(salt, password);
    given.len() == hash.len() && given.bytes().zip(hash.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}
