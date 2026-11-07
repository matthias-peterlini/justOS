//! Who an app is, and what it may touch.
//!
//! An installed justOS app (a `.app` in `sys:/apps` with its `.info`, or an
//! JustScript package that `just` starts with `APP_ENTER`) runs as its
//! `app_id`, such as `dev.justos.disegno`, in a sandbox:
//!
//! - its own folder, `sys:/home/.apps/<app_id>`, is all its, and it starts
//!   there;
//! - it may read the installed programs and the system's fonts, pictures,
//!   sounds and certificates (not the settings, which hold the password);
//! - a folder of the home (Documenti, Immagini...) or another disk needs the
//!   user's permission, asked once in a dialog and remembered in
//!   `sys:/system/permissions.json` (Impostazioni, Privacy, changes it);
//! - single files come from the system's file picker (`PICK_FILE`), which
//!   runs outside the app: what the user picks, the app may use.
//!
//! Everything else is `Denied`. System programs (a `.app` without `.info`,
//! the shell, the desktop) have no sandbox; a program an app starts runs in
//! the app's. Devices ask through the `just.permissions` port
//! ([`crate::ipc`]), with the same dialog and the same table.

use crate::fs::{self, Path};
use crate::sync::{IrqLock, Mutex};
use crate::task::WaitQueue;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use justdk::syscall::Error;
use justjson::Value;

/// Where apps keep their own files.
pub const DATA: &str = "sys:/home/.apps";
const FILE: &str = "sys:/system/permissions.json";

/// What an app can ask for besides folders, and how the dialog names it.
pub const DEVICES: [(&str, &str); 4] =
    [("camera", "la fotocamera"), ("microphone", "il microfono"), ("location", "la posizione"), ("network", "la rete")];

/// What every app may read (paths in `sys:`, lowercase).
const SHARED: [&str; 8] =
    ["/apps", "/system/fonts.bin", "/system/fonts@2x.bin", "/system/webfonts.bin", "/system/certs.bin", "/system/gui", "/system/sounds", "/etc/ssl"];

/// An app, as the sandbox knows it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct App {
    pub id: String,
    /// Its name for people ("Disegno").
    pub name: String,
    /// Its icon (an icon name, or "@file").
    pub icon: String,
}

/// Is `id` a good app id: reverse-DNS (`org.example.app`; letters, digits,
/// `-` and `_`), outside `just.`, which is the system's? Ids are kept in
/// lowercase, as file names do not mind case.
pub fn valid_id(id: &str) -> bool {
    let parts: Vec<&str> = id.split('.').collect();
    id.len() <= 128
        && parts.len() >= 2
        && !parts[0].eq_ignore_ascii_case("just")
        && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
}

/// The id of a package that does not name one: `dev.justos.<name>`.
pub fn default_id(name: &str) -> String {
    let mut s = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c.to_ascii_lowercase());
        } else if !s.is_empty() && !s.ends_with('-') {
            s.push('-');
        }
    }
    let s = s.trim_end_matches('-');
    match s.bytes().next() {
        None => String::from("dev.justos.app"),
        Some(b) if b.is_ascii_digit() => format!("dev.justos.app-{s}"),
        Some(_) => format!("dev.justos.{s}"),
    }
}

/// The app a program file is: an installed justOS package in `sys:/apps`.
pub fn for_program(path: &Path) -> Option<App> {
    if !path.volume.eq_ignore_ascii_case("sys") {
        return None;
    }
    let inner = path.inner.to_ascii_lowercase();
    let stem = inner.strip_prefix("/apps/")?.strip_suffix(".app")?;
    if stem.contains('/') {
        return None;
    }
    let p = crate::store::installed(stem)?;
    if p.platform != crate::store::Platform::Justos {
        return None;
    }
    let icon = crate::store::icon(stem).unwrap_or_else(|| String::from("window"));
    Some(App { id: p.app_id, name: p.name, icon })
}

/// The app's own folder, made if it is not there yet.
pub fn prepare(app: &App) -> Option<Path> {
    let data = fs::abs(DATA).ok()?;
    let home = data.join(&app.id);
    for dir in [Path::parse("..", &data).ok()?, data, home.clone()] {
        if !fs::exists(&dir) {
            fs::make_dir(&dir).ok()?;
        }
    }
    Some(home)
}

enum Rule {
    Allow,
    Deny,
    /// Allowed if the user agrees to this permission.
    Ask(String),
}

/// `inner` is `base` or inside it.
fn under(inner: &str, base: &str) -> bool {
    inner == base || (inner.starts_with(base) && inner.as_bytes().get(base.len()) == Some(&b'/'))
}

fn same(a: &Path, b: &Path) -> bool {
    a.volume.eq_ignore_ascii_case(&b.volume) && a.inner.eq_ignore_ascii_case(&b.inner)
}

/// A home folder's name as it is on disk (names do not mind case).
fn folder_name(typed: &str) -> String {
    let home = Path::root("sys").join("home");
    fs::list_dir(&home)
        .ok()
        .and_then(|l| l.into_iter().map(|(e, _)| e.name).find(|n| n.eq_ignore_ascii_case(typed)))
        .unwrap_or_else(|| String::from(typed))
}

fn rule(app: &App, picked: &[(Path, bool)], path: &Path, write: bool) -> Rule {
    if picked.iter().any(|(p, w)| same(p, path) && (*w || !write)) {
        return Rule::Allow;
    }
    if !path.volume.eq_ignore_ascii_case("sys") {
        return Rule::Ask(format!("files:{}:", path.volume));
    }
    let inner = path.inner.to_ascii_lowercase();
    if under(&inner, &format!("/home/.apps/{}", app.id)) {
        return Rule::Allow;
    }
    if SHARED.iter().any(|b| under(&inner, b)) {
        return if write { Rule::Deny } else { Rule::Allow };
    }
    if let Some(rest) = inner.strip_prefix("/home/") {
        // A folder of the home and what is in it (lowercasing keeps the
        // lengths, so the name can be cut from the path as written).
        let len = rest.find('/').unwrap_or(rest.len());
        let folder = &path.inner["/home/".len().."/home/".len() + len];
        let file_in_home = len == rest.len() && fs::stat(path).is_ok_and(|s| s.kind != justfs::Kind::Dir);
        if !folder.is_empty() && !folder.starts_with('.') && !file_in_home {
            return Rule::Ask(format!("files:{}", folder_name(folder)));
        }
    }
    Rule::Deny
}

/// May the calling process touch `path` (to change it if `write`)? Asks
/// the user when the app needs a permission it was never given or refused.
pub fn check(path: &Path, write: bool) -> Result<(), Error> {
    let Some((app, picked)) = crate::user::sandbox() else { return Ok(()) };
    match rule(&app, &picked, path, write) {
        Rule::Allow => Ok(()),
        Rule::Deny => Err(Error::Denied),
        Rule::Ask(perm) if allowed(&app, &perm) => Ok(()),
        Rule::Ask(_) => Err(Error::Denied),
    }
}

/// How the dialog says a permission: "la cartella Immagini".
pub fn describe(perm: &str) -> String {
    if let Some(f) = perm.strip_prefix("files:") {
        return match f.strip_suffix(':') {
            Some(v) => format!("il disco {v}:"),
            None => format!("la cartella {f}"),
        };
    }
    DEVICES.iter().find(|(k, _)| *k == perm).map_or_else(|| String::from(perm), |(_, d)| String::from(*d))
}

/// A permission there is: a device, or `files:<home folder>`, or
/// `files:<disk>:`.
pub fn known(perm: &str) -> bool {
    match perm.strip_prefix("files:") {
        Some(f) => !f.is_empty() && !f.contains('/') && !f.starts_with('.') && f.len() <= 64,
        None => DEVICES.iter().any(|(k, _)| *k == perm),
    }
}

// ----------------------------------------------------------------------------
// What the user said, per app

#[derive(Clone)]
pub struct Grants {
    pub id: String,
    pub name: String,
    /// Permission and answer (true: allowed).
    pub perms: Vec<(String, bool)>,
}

static GRANTS: IrqLock<Option<Vec<Grants>>> = IrqLock::new(None);
/// Writes of the table, one at a time.
static SAVING: Mutex<()> = Mutex::new(());

fn load() -> Vec<Grants> {
    let Some(v) = fs::abs(FILE).and_then(|p| fs::read_file(&p)).ok().and_then(|b| justjson::parse_bytes(&b)) else {
        return Vec::new();
    };
    v.get("apps")
        .as_array()
        .iter()
        .filter(|a| valid_id(a.get("id").str()))
        .map(|a| {
            let mut perms = Vec::new();
            for (key, allow) in [("allow", true), ("deny", false)] {
                for p in a.get(key).as_array().iter().filter_map(Value::as_str).filter(|p| known(p)) {
                    perms.push((String::from(p), allow));
                }
            }
            Grants { id: a.get("id").str().to_ascii_lowercase(), name: a.get("name").str().to_string(), perms }
        })
        .collect()
}

fn with<T>(f: impl FnOnce(&mut Vec<Grants>) -> T) -> T {
    if GRANTS.lock().is_none() {
        // Read outside the lock: the file system may sleep.
        let loaded = load();
        let mut g = GRANTS.lock();
        if g.is_none() {
            *g = Some(loaded);
        }
    }
    let mut g = GRANTS.lock();
    f(g.get_or_insert_with(Vec::new))
}

fn save() {
    let _one = SAVING.lock();
    let apps: Vec<Value> = with(|g| {
        g.iter()
            .filter(|a| !a.perms.is_empty())
            .map(|a| {
                let list = |allow: bool| Value::Arr(a.perms.iter().filter(|p| p.1 == allow).map(|p| Value::string(&p.0)).collect());
                Value::object(alloc::vec![
                    ("id", Value::string(&a.id)),
                    ("name", Value::string(&a.name)),
                    ("allow", list(true)),
                    ("deny", list(false)),
                ])
            })
            .collect()
    });
    let text = format!("{}\n", Value::object(alloc::vec![("apps", Value::Arr(apps))]));
    if let Err(e) = fs::abs(FILE).and_then(|p| fs::write_file(&p, text.as_bytes())) {
        crate::warn!("sandbox", "cannot save the permissions: {e:?}");
    }
}

/// What the user answered for `perm` of app `id` (None: never asked).
pub fn state(id: &str, perm: &str) -> Option<bool> {
    with(|g| g.iter().find(|a| a.id.eq_ignore_ascii_case(id))?.perms.iter().find(|p| p.0 == perm).map(|p| p.1))
}

/// Records an answer (None forgets it: the app will ask again).
pub fn set(id: &str, name: &str, perm: &str, answer: Option<bool>) {
    let id = &id.to_ascii_lowercase();
    with(|g| {
        let i = match g.iter().position(|a| a.id == *id) {
            Some(i) => i,
            None => {
                g.push(Grants { id: String::from(id), name: String::from(name), perms: Vec::new() });
                g.len() - 1
            }
        };
        let a = &mut g[i];
        if !name.is_empty() {
            a.name = String::from(name);
        }
        a.perms.retain(|p| p.0 != perm);
        if let Some(allow) = answer {
            a.perms.push((String::from(perm), allow));
        }
    });
    save();
}

/// Forgets every answer about an app.
pub fn forget(id: &str) -> bool {
    let had = with(|g| {
        let n = g.len();
        g.retain(|a| !a.id.eq_ignore_ascii_case(id));
        n != g.len()
    });
    save();
    had
}

/// Every app the user answered something about.
pub fn all() -> Vec<Grants> {
    with(|g| g.iter().filter(|a| !a.perms.is_empty()).cloned().collect())
}

/// One dialog at a time.
static ASKING: IrqLock<bool> = IrqLock::new(false);
static ASKED: WaitQueue = WaitQueue::new();

/// Does `app` have `perm`? Asks the user the first time, and remembers.
pub fn allowed(app: &App, perm: &str) -> bool {
    if let Some(answer) = state(&app.id, perm) {
        return answer;
    }
    loop {
        {
            let mut a = ASKING.lock();
            if !*a {
                *a = true;
                break;
            }
        }
        if crate::user::kill_requested() {
            return false;
        }
        ASKED.wait_for_timeout(100, || (!*ASKING.lock()).then_some(()));
    }
    // Asked meanwhile, by another program of the same app?
    let answer = match state(&app.id, perm) {
        Some(a) => Some(a),
        None => {
            let a = ask(app, perm);
            if let Some(a) = a {
                set(&app.id, &app.name, perm, Some(a));
            }
            a
        }
    };
    *ASKING.lock() = false;
    ASKED.notify_all();
    answer.unwrap_or(false)
}

fn ask(app: &App, perm: &str) -> Option<bool> {
    let what = describe(perm);
    if crate::gui::active() {
        return crate::gui::ask_permission(app, &what);
    }
    crate::println!("\x1b[93m{} ({}) vuole usare {what}: negato.\x1b[0m Per consentirlo: perms allow {} {perm}", app.name, app.id, app.id);
    None
}
