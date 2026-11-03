//! App updates in the background (the system's own are in [`crate::update`]).
//!
//! Two sources:
//!
//! - **arcOS apps** (`type=native`): a signed index, `index.json` with its
//!   Ed25519 signature next to it (`index.json.sig`, in hex). The key is
//!   [`KEY`]; its private half stays with whoever publishes arcOS, so a
//!   changed index or a changed download is refused. Each entry names the
//!   new program's address, size and SHA-256, carries the new `info`, and
//!   may name the oldest arcOS it runs on (`system`).
//!   The index has a serial number that never goes back, so an old index
//!   (with old, known bugs) cannot be passed off as new.
//! - **Linux and Windows programs** from GitHub: their latest release,
//!   downloaded over HTTPS as the store installs them.
//!
//! The service `arc.updates` looks once a day while the desktop runs.
//! Updates download into `sys:/system/updates/<id>/` and replace the app
//! once it is not running (at once if it is closed, else when it closes, or
//! at the next start). An update that asks for a permission the installed
//! version did not declare waits for the user (Negozio → Aggiornamenti, or
//! `updates install <id>`). With automatic updates off, it only tells.

use crate::fs;
use crate::net::http;
use crate::store::{self, CatalogApp};
use crate::sync::IrqLock;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use arcjson::Value;
use arctls::ed25519;
use core::sync::atomic::{AtomicBool, Ordering};

/// The public key that signs the index of arcOS apps.
pub const KEY: [u8; 32] = [
    0x83, 0x7a, 0x8d, 0x8d, 0x11, 0xc6, 0x5f, 0x04, 0xfb, 0xb9, 0x28, 0x3d, 0xd4, 0x23, 0x63, 0xe1, 0x10, 0x39, 0xa7, 0xf3, 0x6b, 0x00, 0x52,
    0x78, 0xfc, 0x57, 0x20, 0x8f, 0xa7, 0x07, 0xa1, 0xdb,
];

/// Where the index is published (the `updates` branch of the repository).
pub const INDEX_URL: &str = "https://raw.githubusercontent.com/matthias-peterlini/ArcOs/updates/index.json";

const DIR: &str = "sys:/system/updates";
const APPS: &str = "sys:/apps";
const DAY: u64 = 24 * 3600;

/// Where updates come from and where they wait.
pub struct Source {
    /// The index; its signature is the same address with `.sig`.
    pub index: String,
    pub key: [u8; 32],
    /// Downloads and `state.json`.
    pub dir: String,
}

impl Source {
    /// The published index, or the one named in `sys:/system/updates/source`
    /// (a mirror, or a test): its signature is checked with [`KEY`] all the
    /// same.
    pub fn system() -> Source {
        let mirror = fs::abs(&format!("{DIR}/source"))
            .and_then(|p| fs::read_file(&p))
            .ok()
            .and_then(|b| String::from_utf8_lossy(&b).lines().next().map(|l| l.trim().to_string()))
            .filter(|l| !l.is_empty());
        Source { index: mirror.unwrap_or_else(|| String::from(INDEX_URL)), key: KEY, dir: String::from(DIR) }
    }

    fn pending(&self, id: &str) -> String {
        format!("{}/{id}", self.dir)
    }
}

/// A file the index points to.
#[derive(Clone)]
struct Remote {
    url: String,
    sha256: [u8; 32],
    size: u64,
}

#[derive(Clone)]
enum Origin {
    Index { app: Remote, icon: Option<Remote>, info: String },
    GitHub(CatalogApp),
    /// Downloaded before (found in the folder at startup).
    Folder,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum State {
    Available,
    Downloading { done: u64, total: Option<u64> },
    /// Downloaded; installs when the app is not running.
    Ready,
    Installed,
    Failed(String),
}

#[derive(Clone)]
pub struct Update {
    pub id: String,
    pub name: String,
    /// The installed version and the new one.
    pub current: String,
    pub version: String,
    /// What the new version says it will ask for.
    pub permissions: Vec<String>,
    /// Of those, the ones the installed version did not declare.
    pub new_permissions: Vec<String>,
    /// The user agreed (to the new permissions, or to install now).
    pub approved: bool,
    pub state: State,
    origin: Origin,
}

impl Update {
    pub fn needs_approval(&self) -> bool {
        !self.new_permissions.is_empty() && !self.approved
    }

    /// Whether the app is open right now.
    pub fn in_use(&self) -> bool {
        store::installed(&self.id).and_then(|p| fs::abs(&store::program_path(&p)).ok()).is_some_and(|p| crate::user::running(&p))
    }
}

/// How an installation went.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Applied {
    Done,
    /// The app is open: it waits until it is closed.
    InUse,
}

// ----------------------------------------------------------------------------
// Settings (`state.json`)

struct Settings {
    auto: bool,
    /// When the index was last read (seconds since 2000).
    last_check: u64,
    /// The highest index serial seen.
    serial: u64,
}

fn load_settings(src: &Source) -> Settings {
    let v = fs::abs(&format!("{}/state.json", src.dir)).and_then(|p| fs::read_file(&p)).ok().and_then(|b| arcjson::parse_bytes(&b)).unwrap_or(Value::Null);
    Settings {
        auto: v.get("auto").as_bool().unwrap_or(true),
        last_check: v.get("last_check").as_f64().unwrap_or(0.0) as u64,
        serial: v.get("serial").as_f64().unwrap_or(0.0) as u64,
    }
}

fn save_settings(src: &Source, s: &Settings) {
    make_dir(&src.dir);
    let v = Value::object(alloc::vec![
        ("auto", Value::Bool(s.auto)),
        ("last_check", Value::Num(s.last_check as f64)),
        ("serial", Value::Num(s.serial as f64)),
    ]);
    if let Err(e) = fs::abs(&format!("{}/state.json", src.dir)).and_then(|p| fs::write_file(&p, format!("{v}\n").as_bytes())) {
        crate::warn!("updates", "cannot save the state: {e}");
    }
}

fn make_dir(dir: &str) {
    if let Ok(p) = fs::abs(dir) {
        if !fs::exists(&p) {
            let _ = fs::make_dir(&p);
        }
    }
}

// ----------------------------------------------------------------------------
// Checking

/// A file from the internet, or from this computer (a path, for tests and
/// local mirrors: the index is signed either way).
fn fetch(url: &str, progress: http::Progress<'_>) -> Result<Vec<u8>, String> {
    if url.starts_with("https://") || url.starts_with("http://") {
        let r = http::fetch(&http::Request::get(url), progress)?;
        if !r.ok() {
            return Err(format!("{url}: {}", r.status));
        }
        return Ok(r.body);
    }
    fs::abs(url).and_then(|p| fs::read_file(&p)).map_err(|e| format!("{url}: {e}"))
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn remote(v: &Value) -> Option<Remote> {
    let sha = unhex(v.get("sha256").as_str()?)?;
    Some(Remote { url: v.get("url").as_str()?.to_string(), sha256: sha.try_into().ok()?, size: v.get("size").as_f64().unwrap_or(0.0) as u64 })
}

/// Reads and checks the signed index: its serial and the updates it has for
/// the installed arcOS apps.
fn read_index(src: &Source, min_serial: u64) -> Result<(u64, Vec<Update>), String> {
    let index = fetch(&src.index, &mut |_, _| true)?;
    let sig = fetch(&format!("{}.sig", src.index), &mut |_, _| true)?;
    let sig = unhex(&String::from_utf8_lossy(&sig)).ok_or("la firma dell'indice è illeggibile")?;
    if !ed25519::verify(&src.key, &index, &sig) {
        return Err("la firma dell'indice non è valida".to_string());
    }
    let v = arcjson::parse_bytes(&index).ok_or("l'indice è illeggibile")?;
    let serial = v.get("serial").as_f64().unwrap_or(0.0) as u64;
    if serial < min_serial {
        return Err(format!("l'indice ({serial}) è più vecchio di uno già visto ({min_serial})"));
    }
    let mut out = Vec::new();
    for a in v.get("apps").as_array() {
        let (Some(id), Some(app), Some(info)) = (a.get("id").as_str(), remote(a.get("app")), a.get("info").as_str()) else { continue };
        // Built for a newer arcOS: it waits for the system update.
        if a.get("system").as_str().is_some_and(|min| crate::update::newer(min, crate::VERSION)) {
            continue;
        }
        let Some(installed) = store::installed(id).filter(|p| p.native) else { continue };
        let new = store::parse_info(info, id);
        if !new.native || !crate::update::newer(&new.version, &installed.version) {
            continue;
        }
        let icon = if a.get("icon").is_null() { None } else { remote(a.get("icon")) };
        out.push(Update {
            id: id.to_string(),
            name: new.name.clone(),
            current: installed.version.clone(),
            version: new.version.clone(),
            new_permissions: new.permissions.iter().filter(|p| !installed.permissions.contains(p)).cloned().collect(),
            permissions: new.permissions,
            approved: false,
            state: State::Available,
            origin: Origin::Index { app, icon, info: info.to_string() },
        });
    }
    Ok((serial, out))
}

/// Newer releases of the installed Linux and Windows programs from GitHub.
fn github_updates() -> Vec<Update> {
    store::installed_all()
        .into_iter()
        .filter(|p| !p.native && !p.version.is_empty())
        .filter_map(|p| {
            let app = store::catalog_app_for(&p)?;
            let (tag, _) = store::github_release(&app.github).ok()?;
            let version = tag.trim_start_matches(['v', 'V']).to_string();
            crate::update::newer(&version, &p.version).then(|| Update {
                id: p.id.clone(),
                name: p.name.clone(),
                current: p.version.clone(),
                version,
                permissions: Vec::new(),
                new_permissions: Vec::new(),
                approved: false,
                state: State::Available,
                origin: Origin::GitHub(app),
            })
        })
        .collect()
}

/// Asks the index (and GitHub, if `github`) what is new. The updates found,
/// and why the index could not be read, if it could not.
pub fn check_with(src: &Source, github: bool) -> (Vec<Update>, Option<String>) {
    let mut s = load_settings(src);
    let (mut found, error) = match read_index(src, s.serial) {
        Ok((serial, found)) => {
            s.serial = serial;
            (found, None)
        }
        Err(e) => (Vec::new(), Some(e)),
    };
    s.last_check = crate::time::now();
    save_settings(src, &s);
    if github {
        found.extend(github_updates());
    }
    (found, error)
}

// ----------------------------------------------------------------------------
// Downloading and installing

fn write(file: &str, data: &[u8]) -> Result<(), String> {
    fs::abs(file).and_then(|p| fs::write_file(&p, data)).map_err(|e| format!("{file}: {e}"))
}

fn fetch_checked(r: &Remote, progress: http::Progress<'_>) -> Result<Vec<u8>, String> {
    let data = fetch(&r.url, progress)?;
    if (r.size != 0 && data.len() as u64 != r.size) || arctls::sha2::Sha256::digest(&data) != r.sha256 {
        return Err("il file scaricato non corrisponde all'indice".to_string());
    }
    Ok(data)
}

/// Downloads an update into its folder (`<dir>/<id>/app`, `info`, `icon.qoi`
/// and `ready` once it is all there).
pub fn download(src: &Source, u: &Update, progress: http::Progress<'_>) -> Result<(), String> {
    let dir = src.pending(&u.id);
    make_dir(&src.dir);
    remove_dir(&dir);
    make_dir(&dir);
    match &u.origin {
        Origin::Index { app, icon, info } => {
            let data = fetch_checked(app, progress)?;
            write(&format!("{dir}/app"), &data)?;
            write(&format!("{dir}/info"), info.as_bytes())?;
            if let Some(icon) = icon {
                write(&format!("{dir}/icon.qoi"), &fetch_checked(icon, &mut |_, _| true)?)?;
            }
        }
        Origin::GitHub(app) => {
            let (version, url, program) = store::download_release(app, &mut |_| {}, progress)?;
            write(&format!("{dir}/app"), &program)?;
            write(&format!("{dir}/info"), store::info_text(app, &version, &url).as_bytes())?;
        }
        Origin::Folder => return Ok(()),
    }
    write(&format!("{dir}/ready"), b"")
}

fn remove_dir(dir: &str) {
    if let Ok(p) = fs::abs(dir) {
        if fs::exists(&p) {
            let _ = fs::remove_all(&p);
        }
    }
}

/// Installs a downloaded update, unless the app is running.
pub fn apply(src: &Source, id: &str) -> Result<Applied, String> {
    let dir = src.pending(id);
    let ready = fs::abs(&format!("{dir}/ready")).is_ok_and(|p| fs::exists(&p));
    let info = fs::abs(&format!("{dir}/info")).and_then(|p| fs::read_file(&p)).map_err(|_| "l'aggiornamento non è scaricato".to_string())?;
    if !ready {
        return Err("l'aggiornamento non è scaricato".to_string());
    }
    let new = store::parse_info(&String::from_utf8_lossy(&info), id);
    let old = store::installed(id);
    let target = store::program_path(&new);
    let running = |file: &str| fs::abs(file).is_ok_and(|p| crate::user::running(&p));
    if running(&target) || old.as_ref().is_some_and(|o| running(&store::program_path(o))) {
        return Ok(Applied::InUse);
    }
    let copy = |from: &str, to: &str| fs::abs(from).and_then(|f| fs::abs(to).and_then(|t| fs::copy_file(&f, &t))).map_err(|e| format!("{to}: {e}"));
    copy(&format!("{dir}/app"), &target)?;
    copy(&format!("{dir}/info"), &format!("{APPS}/{id}.info"))?;
    if fs::abs(&format!("{dir}/icon.qoi")).is_ok_and(|p| fs::exists(&p)) {
        copy(&format!("{dir}/icon.qoi"), &format!("{APPS}/{id}.qoi"))?;
    }
    if let Some(old) = old {
        let was = store::program_path(&old);
        if !was.eq_ignore_ascii_case(&target) {
            if let Ok(p) = fs::abs(&was) {
                let _ = fs::remove(&p);
            }
        }
    }
    remove_dir(&dir);
    Ok(Applied::Done)
}

/// Updates downloaded before the last shutdown.
fn waiting(src: &Source) -> Vec<Update> {
    let Ok(dir) = fs::abs(&src.dir) else { return Vec::new() };
    fs::list_dir(&dir)
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, st)| st.kind == arcfs::Kind::Dir)
        .filter_map(|(e, _)| {
            let id = e.name;
            let pending = src.pending(&id);
            if !fs::abs(&format!("{pending}/ready")).is_ok_and(|p| fs::exists(&p)) {
                remove_dir(&pending);
                return None;
            }
            let info = fs::abs(&format!("{pending}/info")).and_then(|p| fs::read_file(&p)).ok()?;
            let new = store::parse_info(&String::from_utf8_lossy(&info), &id);
            let Some(installed) = store::installed(&id).filter(|p| crate::update::newer(&new.version, &p.version)) else {
                remove_dir(&pending);
                return None;
            };
            Some(Update {
                id,
                name: new.name.clone(),
                current: installed.version.clone(),
                version: new.version.clone(),
                new_permissions: new.permissions.iter().filter(|p| !installed.permissions.contains(p)).cloned().collect(),
                permissions: new.permissions,
                approved: false,
                state: State::Ready,
                origin: Origin::Folder,
            })
        })
        .collect()
}

// ----------------------------------------------------------------------------
// The service and what the Negozio and the shell use

static UPDATES: IrqLock<Vec<Update>> = IrqLock::new(Vec::new());
static ERROR: IrqLock<Option<String>> = IrqLock::new(None);
static CHECKING: AtomicBool = AtomicBool::new(false);
/// One download or installation at a time.
static WORKING: crate::sync::Mutex<()> = crate::sync::Mutex::new(());

/// The updates known (available, downloading, waiting, just installed).
pub fn list() -> Vec<Update> {
    UPDATES.lock().clone()
}

/// How many updates are not installed yet (for the badge in the Negozio).
pub fn pending_count() -> usize {
    UPDATES.lock().iter().filter(|u| u.state != State::Installed).count()
}

pub fn checking() -> bool {
    CHECKING.load(Ordering::Relaxed)
}

/// Why the last check of the index failed.
pub fn last_error() -> Option<String> {
    ERROR.lock().clone()
}

/// When the index was last read (seconds since 2000; 0: never).
pub fn last_check() -> u64 {
    load_settings(&Source::system()).last_check
}

pub fn auto() -> bool {
    load_settings(&Source::system()).auto
}

pub fn set_auto(on: bool) {
    let src = Source::system();
    let mut s = load_settings(&src);
    s.auto = on;
    save_settings(&src, &s);
}

fn set_state(id: &str, state: State) {
    if let Some(u) = UPDATES.lock().iter_mut().find(|u| u.id == id) {
        u.state = state;
    }
}

fn notify(title: &str, text: &str) {
    crate::gui::notify(title, text, "store");
}

/// Looks for updates now; with automatic updates, downloads them and
/// installs those it can. Returns how many there are.
pub fn check() -> Result<usize, String> {
    if CHECKING.swap(true, Ordering::Relaxed) {
        return Err("sto già controllando".to_string());
    }
    let src = Source::system();
    let (found, error) = check_with(&src, crate::net::online());
    if let Some(e) = &error {
        crate::warn!("updates", "{e}");
    }
    *ERROR.lock() = error.clone();
    let mut fresh = Vec::new();
    {
        let mut list = UPDATES.lock();
        // What was just installed is forgotten; what is being downloaded or
        // waits keeps going.
        list.retain(|u| u.state != State::Installed && !matches!(u.state, State::Failed(_)));
        for f in found {
            match list.iter_mut().find(|u| u.id == f.id) {
                Some(u) if u.version == f.version && u.state != State::Available => {}
                Some(u) => *u = f,
                None => {
                    fresh.push(f.name.clone());
                    list.push(f);
                }
            }
        }
    }
    CHECKING.store(false, Ordering::Relaxed);
    let n = pending_count();
    if load_settings(&src).auto {
        let ids: Vec<String> = list().into_iter().filter(|u| u.state == State::Available).map(|u| u.id).collect();
        for id in ids {
            let _ = fetch_update(&src, &id);
        }
        install_ready(&src, false);
    } else if !fresh.is_empty() {
        let what = if fresh.len() == 1 { format!("È disponibile una nuova versione di {}.", fresh[0]) } else { format!("{} app hanno una nuova versione.", fresh.len()) };
        notify("Aggiornamenti disponibili", &format!("{what} Aprile dal Negozio."));
    }
    match (n, error) {
        (0, Some(e)) => Err(e),
        _ => Ok(n),
    }
}

/// Downloads one update (if it is not already).
fn fetch_update(src: &Source, id: &str) -> Result<(), String> {
    let _one = WORKING.lock();
    let Some(u) = list().into_iter().find(|u| u.id == id) else { return Err("nessun aggiornamento per questa app".to_string()) };
    if !matches!(u.state, State::Available | State::Failed(_)) {
        return Ok(());
    }
    set_state(id, State::Downloading { done: 0, total: None });
    let mut report = |done: u64, total: Option<u64>| {
        set_state(id, State::Downloading { done, total });
        true
    };
    match download(src, &u, &mut report) {
        Ok(()) => {
            set_state(id, State::Ready);
            Ok(())
        }
        Err(e) => {
            crate::warn!("updates", "{id}: {e}");
            set_state(id, State::Failed(e.clone()));
            Err(e)
        }
    }
}

/// Installs the downloaded updates whose apps are closed: all of them when
/// `asked` (or with automatic updates), else none. Those asking for new
/// permissions only once approved.
fn install_ready(src: &Source, asked: bool) {
    let auto = load_settings(src).auto;
    let ready: Vec<Update> = list().into_iter().filter(|u| u.state == State::Ready && !u.needs_approval() && (auto || asked || u.approved)).collect();
    for u in ready {
        let _one = WORKING.lock();
        match apply(src, &u.id) {
            Ok(Applied::Done) => {
                set_state(&u.id, State::Installed);
                notify(&format!("{} è aggiornata", u.name), &format!("Ora c'è la versione {}.", u.version));
            }
            Ok(Applied::InUse) => {}
            Err(e) => {
                crate::warn!("updates", "{}: {e}", u.id);
                set_state(&u.id, State::Failed(e));
            }
        }
    }
}

/// The user asked to update `id` ("all": every one): agrees to its new
/// permissions, downloads it and installs it now, or when it is closed.
pub fn install(id: &str) -> Result<Applied, String> {
    let src = Source::system();
    let ids: Vec<String> = list().into_iter().filter(|u| u.state != State::Installed && (id == "all" || u.id == id)).map(|u| u.id).collect();
    if ids.is_empty() {
        return Err("nessun aggiornamento per questa app".to_string());
    }
    let mut result = Applied::Done;
    for id in ids {
        if let Some(u) = UPDATES.lock().iter_mut().find(|u| u.id == id) {
            u.approved = true;
        }
        fetch_update(&src, &id)?;
        install_ready(&src, true);
        match list().into_iter().find(|u| u.id == id).map(|u| u.state) {
            Some(State::Failed(e)) => return Err(e),
            Some(State::Ready) => result = Applied::InUse,
            _ => {}
        }
    }
    Ok(result)
}

/// [`check`] in a thread of its own (for the Negozio).
pub fn check_in_background() {
    if !checking() {
        crate::task::spawn("updates-check", || {
            let _ = check();
        });
    }
}

/// [`install`] in a thread of its own (for the Negozio).
pub fn install_in_background(id: &str) {
    let id = id.to_string();
    crate::task::spawn("updates-install", move || {
        if let Err(e) = install(&id) {
            notify("Aggiornamento non riuscito", &e);
        }
    });
}

/// Starts `arc.updates`: installs what waited from the last time, answers
/// programs and, while the desktop runs, checks once a day.
pub fn start() {
    if crate::task::spawn("arc.updates", service).is_none() {
        crate::warn!("updates", "could not start arc.updates");
    }
}

/// `arc.updates` answers:
///
/// - `{"list": true}`: `{"updates": [{"id", "name", "current", "version",
///   "state", "permissions", "new_permissions"}], "auto": bool}`;
///
/// and from programs without a sandbox only: `{"check": true}` (looks now,
/// answers `{"found": n}`), `{"install": id | "all"}` and `{"auto": bool}`.
fn service() {
    let src = Source::system();
    *UPDATES.lock() = waiting(&src);
    install_ready(&src, false);
    let port = match crate::ipc::create(0, Some("arc.updates")) {
        Ok(p) => p,
        Err(e) => {
            crate::warn!("updates", "arc.updates: {}", e.as_str());
            return;
        }
    };
    let started = crate::time::uptime_ms();
    loop {
        if let Ok(m) = crate::ipc::recv(port, 0, Some(5000), &|| false) {
            let answer = answer(&m.data, crate::user::app_of(m.sender).is_none());
            if let Some(reply) = m.reply {
                let _ = crate::ipc::send(reply, format!("{answer}").into_bytes(), 0, None);
                crate::ipc::drop_sender(reply);
            }
        }
        // Apps that were open when their update came, now closed.
        if list().iter().any(|u| u.state == State::Ready) {
            install_ready(&src, false);
        }
        // Once a day, a minute after the desktop came up.
        let due = crate::time::now() >= load_settings(&src).last_check + DAY;
        if due && crate::gui::active() && crate::net::online() && crate::time::uptime_ms() > started + 60_000 {
            let _ = check();
        }
    }
}

fn answer(data: &[u8], system: bool) -> Value {
    let v = arcjson::parse_bytes(data).unwrap_or(Value::Null);
    let strings = |l: &[String]| Value::Arr(l.iter().map(|s| Value::string(s)).collect());
    let error = |e: &str| Value::object(alloc::vec![("error", Value::string(e))]);
    if v.get("list").as_bool() == Some(true) {
        let updates = list()
            .iter()
            .map(|u| {
                let state = match &u.state {
                    State::Available => "available",
                    State::Downloading { .. } => "downloading",
                    State::Ready => "ready",
                    State::Installed => "installed",
                    State::Failed(_) => "failed",
                };
                Value::object(alloc::vec![
                    ("id", Value::string(&u.id)),
                    ("name", Value::string(&u.name)),
                    ("current", Value::string(&u.current)),
                    ("version", Value::string(&u.version)),
                    ("state", Value::string(state)),
                    ("permissions", strings(&u.permissions)),
                    ("new_permissions", strings(&u.new_permissions)),
                ])
            })
            .collect();
        return Value::object(alloc::vec![("updates", Value::Arr(updates)), ("auto", Value::Bool(auto()))]);
    }
    if !system {
        return error("solo il sistema può farlo");
    }
    if v.get("check").as_bool() == Some(true) {
        return match check() {
            Ok(n) => Value::object(alloc::vec![("found", Value::Num(n as f64))]),
            Err(e) => error(&e),
        };
    }
    if let Some(id) = v.get("install").as_str() {
        return match install(id) {
            Ok(a) => Value::object(alloc::vec![("installed", Value::Bool(a == Applied::Done))]),
            Err(e) => error(&e),
        };
    }
    if let Some(on) = v.get("auto").as_bool() {
        set_auto(on);
        return Value::object(alloc::vec![("auto", Value::Bool(on))]);
    }
    error("richiesta sconosciuta")
}

// ----------------------------------------------------------------------------
// `updates test`

/// Checks the whole path with a test key and files in a folder of its own: signature,
/// old indexes, a damaged download, an app that is open, an update asking
/// for a new permission. Prints what failed; true if nothing did.
pub fn self_test() -> bool {
    const SEED: [u8; 32] = [0x5a; 32];
    let base = "sys:/system/updates-test";
    let src = Source { index: format!("{base}/index.json"), key: ed25519::public_key(&SEED), dir: format!("{base}/pending") };
    let mut failed = 0;
    let mut check = |ok: bool, what: &str| {
        if !ok {
            crate::println!("\x1b[91mupdates: {what}\x1b[0m");
            failed += 1;
        }
    };
    let Ok(program) = fs::abs("sys:/apps/sandbox.app").and_then(|p| fs::read_file(&p)) else {
        crate::println!("updates: sys:/apps/sandbox.app is missing");
        return false;
    };
    remove_dir(base);
    make_dir(base);
    let fake = ["provaagg", "provabad", "provaperm", "provanext"];
    for id in fake {
        let _ = write(&format!("{APPS}/{id}.app"), &program);
        let _ = write(&format!("{APPS}/{id}.info"), format!("name={id}\nversion=1.0\ntype=native\n").as_bytes());
    }
    let mut newer = program.clone();
    newer.extend_from_slice(b"\0arcos-update-1.1");
    let _ = write(&format!("{base}/new.app"), &newer);
    let entry = |id: &str, version: &str, sha: &[u8], extra: &str| {
        let app = Value::object(alloc::vec![
            ("url", Value::string(&format!("{base}/new.app"))),
            ("sha256", Value::string(&hex(sha))),
            ("size", Value::Num(newer.len() as f64)),
        ]);
        let info = format!("name={id}\nversion={version}\ntype=native\n{extra}");
        Value::object(alloc::vec![("id", Value::string(id)), ("app", app), ("info", Value::string(&info))])
    };
    let sha = arctls::sha2::Sha256::digest(&newer);
    let publish = |serial: u64, seed: &[u8; 32]| {
        // provanone is not installed; provanext needs a newer arcOS.
        let mut next = entry("provanext", "1.1", &sha, "");
        if let Value::Obj(members) = &mut next {
            members.push((String::from("system"), Value::string("99.0")));
        }
        let apps = alloc::vec![
            entry("provaagg", "1.1", &sha, ""),
            entry("provabad", "2.0", &[0; 32], ""),
            entry("provaperm", "1.1", &sha, "permissions=camera\n"),
            entry("provanone", "3.0", &sha, ""),
            next,
        ];
        let index = format!("{}", Value::object(alloc::vec![("serial", Value::Num(serial as f64)), ("apps", Value::Arr(apps))]));
        let _ = write(&format!("{base}/index.json"), index.as_bytes());
        let _ = write(&format!("{base}/index.json.sig"), hex(&ed25519::sign(seed, index.as_bytes())).as_bytes());
    };

    // A signature by another key, then a changed index: refused.
    publish(5, &[0x11; 32]);
    let (found, error) = check_with(&src, false);
    check(found.is_empty() && error.is_some(), "an index signed by another key was accepted");
    publish(5, &SEED);
    if let Ok(p) = fs::abs(&format!("{base}/index.json")) {
        let mut b = fs::read_file(&p).unwrap_or_default();
        if let Some(i) = b.windows(3).position(|w| w == b"2.0") {
            b[i] = b'9';
        }
        let _ = fs::write_file(&p, &b);
    }
    let (found, error) = check_with(&src, false);
    check(found.is_empty() && error.is_some(), "a changed index was accepted");

    // The real one: three updates for installed apps.
    publish(5, &SEED);
    let (found, error) = check_with(&src, false);
    check(error.is_none(), &format!("the index was refused: {error:?}"));
    let ids: Vec<&str> = found.iter().map(|u| u.id.as_str()).collect();
    check(ids == ["provaagg", "provabad", "provaperm"], &format!("wrong updates found: {ids:?}"));
    let get = |id: &str| found.iter().find(|u| u.id == id).cloned();

    // An older index after this one: refused.
    publish(4, &SEED);
    let (old, error) = check_with(&src, false);
    check(old.is_empty() && error.is_some(), "an older index was accepted");
    publish(5, &SEED);

    // A download that does not match the index.
    if let Some(u) = get("provabad") {
        check(download(&src, &u, &mut |_, _| true).is_err(), "a damaged download was accepted");
        check(apply(&src, "provabad").is_err(), "a damaged download was installed");
    }

    // An app that is open waits until it is closed.
    if let Some(u) = get("provaagg") {
        check(download(&src, &u, &mut |_, _| true).is_ok(), "the download failed");
        let pid = fs::abs(&format!("{APPS}/provaagg.app")).ok().and_then(|p| crate::user::spawn(&p, "wait 1500", fs::abs("sys:/").ok()?).ok());
        check(pid.is_some(), "could not start the app");
        crate::task::sleep_ms(200);
        check(apply(&src, "provaagg") == Ok(Applied::InUse), "an open app was replaced");
        if let Some(pid) = pid {
            crate::user::wait(pid);
        }
        check(apply(&src, "provaagg") == Ok(Applied::Done), "the update was not installed once the app closed");
        let installed = store::installed("provaagg").map(|p| p.version);
        check(installed.as_deref() == Some("1.1"), &format!("installed version {installed:?}"));
        let bytes = fs::abs(&format!("{APPS}/provaagg.app")).and_then(|p| fs::read_file(&p)).unwrap_or_default();
        check(bytes.ends_with(b"arcos-update-1.1"), "the program was not replaced");
        check(!fs::abs(&src.pending("provaagg")).is_ok_and(|p| fs::exists(&p)), "the download was left behind");
        // The new version runs.
        let code = fs::abs(&format!("{APPS}/provaagg.app")).ok().and_then(|p| crate::user::spawn(&p, "wait 1", fs::abs("sys:/").ok()?).ok()).and_then(crate::user::wait);
        check(code == Some(0), &format!("the new version did not run ({code:?})"));
    }

    // A new permission: the user is asked first.
    if let Some(mut u) = get("provaperm") {
        check(u.new_permissions == ["camera"] && u.needs_approval(), "a new permission went unnoticed");
        u.approved = true;
        check(!u.needs_approval(), "approval was not kept");
        check(download(&src, &u, &mut |_, _| true).is_ok() && apply(&src, "provaperm") == Ok(Applied::Done), "the approved update failed");
        let perms = store::installed("provaperm").map(|p| p.permissions).unwrap_or_default();
        check(perms == ["camera"], &format!("permissions after the update: {perms:?}"));
    }

    // Up to date now (apart from the damaged one).
    let (again, _) = check_with(&src, false);
    let ids: Vec<&str> = again.iter().map(|u| u.id.as_str()).collect();
    check(ids == ["provabad"], &format!("still offered after updating: {ids:?}"));

    for id in fake {
        let _ = store::remove(id);
        remove_dir(&format!("sys:/home/.apps/org.arcos.{id}"));
    }
    remove_dir(base);
    if failed == 0 {
        crate::println!("updates: all checks passed");
    }
    failed == 0
}

/// The text for `updates`: one line per update.
pub fn describe(u: &Update) -> String {
    let state = match &u.state {
        State::Available => String::from("disponibile"),
        State::Downloading { done, total: Some(t) } if *t > 0 => format!("scarico {}%", done * 100 / t),
        State::Downloading { .. } => String::from("scarico"),
        State::Ready if u.needs_approval() => String::from("scaricato, aspetta il tuo consenso"),
        State::Ready if u.in_use() => String::from("scaricato, si installa alla chiusura"),
        State::Ready => String::from("scaricato"),
        State::Installed => String::from("installato"),
        State::Failed(e) => format!("non riuscito: {e}"),
    };
    format!("{} {} → {} ({state})", u.name, u.current, u.version)
}
