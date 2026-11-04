//! System updates: what is new, and getting it.
//!
//! The signed index of [`crate::appupdate`] may offer a newer arcOS as a
//! system image: it downloads (checked against the index), goes into the
//! slot that is not running ([`crate::sysupdate`]) and starts at the next
//! restart. With automatic updates that happens by itself, else Info
//! sistema offers it. A newer release on GitHub that the index does not
//! offer yet is only announced.
//!
//! Checked once the network is up, with the daily check of the index, and
//! on request from Info sistema.

use crate::appupdate::SystemOffer;
use crate::sync::IrqLock;
use alloc::format;
use alloc::string::String;

/// Where arcOS is released.
pub const REPO: &str = "matthias-peterlini/ArcOs";

#[derive(Clone, PartialEq, Eq)]
pub enum State {
    NotChecked,
    Checking,
    UpToDate,
    /// A newer version that installs from here, as "0.7.0".
    Available(String),
    /// A newer release on GitHub only.
    Released(String),
    Downloading { version: String, done: u64, total: u64 },
    Installing { version: String, done: u64, total: u64 },
    /// In the other slot: starts at the next restart.
    Staged(String),
    Failed(String),
}

impl State {
    fn busy(&self) -> bool {
        matches!(self, State::Checking | State::Downloading { .. } | State::Installing { .. })
    }
}

static STATE: IrqLock<State> = IrqLock::new(State::NotChecked);
static OFFER: IrqLock<Option<SystemOffer>> = IrqLock::new(None);

pub fn state() -> State {
    let s = STATE.lock().clone();
    match s {
        // A change of slot asked for elsewhere (the shell, a rollback).
        State::NotChecked | State::UpToDate | State::Available(_) | State::Released(_) | State::Failed(_) => match crate::sysupdate::slots().next {
            Some(v) => State::Staged(v),
            None => s,
        },
        State::Staged(_) if crate::sysupdate::slots().next.is_none() => State::NotChecked,
        s => s,
    }
}

fn set(s: State) {
    *STATE.lock() = s;
}

/// "v0.7.0" or "0.7" as numbers, for comparing.
fn parse(v: &str) -> [u32; 3] {
    let mut out = [0; 3];
    for (i, part) in v.trim().trim_start_matches(['v', 'V']).split(['.', '-']).take(3).enumerate() {
        out[i] = part.parse().unwrap_or(0);
    }
    out
}

/// Whether `tag` is a newer version than `current`.
pub fn newer(tag: &str, current: &str) -> bool {
    parse(tag) > parse(current)
}

fn notify(title: &str, text: &str) {
    crate::gui::notify(title, text, "about");
}

/// Asks the index (and GitHub) in this thread; `announce` shows a
/// notification for what is new.
pub fn check_now(announce: bool) -> State {
    {
        let mut s = STATE.lock();
        if s.busy() {
            return s.clone();
        }
        *s = State::Checking;
    }
    let indexed = crate::appupdate::system_offer();
    if let Ok(Some(o)) = indexed {
        set(State::NotChecked);
        offered(o, announce);
        return state();
    }
    let result = match crate::store::github_release(REPO) {
        Ok((tag, _)) if newer(&tag, crate::VERSION) => State::Released(String::from(tag.trim_start_matches(['v', 'V']))),
        Ok(_) => State::UpToDate,
        Err(e) => State::Failed(indexed.err().unwrap_or(e)),
    };
    if let (true, State::Released(v)) = (announce, &result) {
        notify("Aggiornamento disponibile", &format!("È uscito arcOS {v}: lo trovi nelle release su GitHub."));
    }
    set(result);
    state()
}

/// [`check_now`] in a thread of its own.
pub fn check(announce: bool) {
    if state().busy() {
        return;
    }
    if crate::task::spawn("update", move || {
        check_now(announce);
    })
    .is_none()
    {
        set(State::Failed(String::from("no thread")));
    }
}

/// The index offers a newer arcOS: with automatic updates it installs, else
/// it waits for the user (announced if `announce`).
pub fn offered(o: SystemOffer, announce: bool) {
    let v = o.version.clone();
    match state() {
        State::Staged(x) | State::Downloading { version: x, .. } | State::Installing { version: x, .. } if x == v => return,
        s if s.busy() && s != State::Checking => return,
        _ => {}
    }
    *OFFER.lock() = Some(o);
    set(State::Available(v.clone()));
    if !crate::sysupdate::supported() {
        if announce {
            notify("Aggiornamento disponibile", &format!("È uscito arcOS {v}: questo computer non è partito da un disco con gli slot, va installato a mano."));
        }
        return;
    }
    // On trial, this system may still go back: the daily check installs it.
    if crate::appupdate::auto() && !crate::sysupdate::on_trial() {
        let _ = install();
    } else if announce {
        notify("Aggiornamento disponibile", &format!("arcOS {v} è pronto da installare: trovalo in Info sistema."));
    }
}

/// Downloads the offered arcOS and writes it into the other slot (in this
/// thread). Returns its version.
pub fn install() -> Result<String, String> {
    let o = OFFER.lock().clone().ok_or_else(|| String::from("nessun aggiornamento di sistema"))?;
    let v = o.version.clone();
    {
        let mut s = STATE.lock();
        if matches!(*s, State::Downloading { .. } | State::Installing { .. }) {
            return Err(String::from("sto già aggiornando"));
        }
        *s = State::Downloading { version: v.clone(), done: 0, total: o.size() };
    }
    let result = crate::appupdate::fetch_system(&o, &mut |done, total| {
        set(State::Downloading { version: v.clone(), done, total: total.unwrap_or(o.size()) });
        true
    })
    .and_then(|image| {
        set(State::Installing { version: v.clone(), done: 0, total: 1 });
        crate::sysupdate::install(&image, &mut |done, total| set(State::Installing { version: v.clone(), done, total }))
    });
    match result {
        Ok(version) => {
            set(State::Staged(version.clone()));
            notify(&format!("arcOS {version} è pronto"), "Parte al prossimo riavvio: riavvia quando vuoi.");
            Ok(version)
        }
        Err(e) => {
            crate::warn!("update", "arcOS {v}: {e}");
            set(State::Failed(e.clone()));
            notify("Aggiornamento di sistema non riuscito", &e);
            Err(e)
        }
    }
}

/// [`install`] in a thread of its own (for Info sistema).
pub fn install_in_background() {
    crate::task::spawn("update-install", || {
        let _ = install();
    });
}

/// Waits (up to a minute) for the network, then checks once.
pub fn check_when_online() {
    crate::task::spawn("update-wait", || {
        for _ in 0..60 {
            if crate::net::online() {
                check(true);
                return;
            }
            crate::task::sleep_ms(1000);
        }
    });
}
