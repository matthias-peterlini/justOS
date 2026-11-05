//! Ports: messages between programs, and between programs and the kernel's
//! services.
//!
//! A port is a queue of messages owned by one process, which alone receives
//! from it. Others reach it through a handle: from `PORT_LOOKUP` for a named
//! port (`just.notify`), or handed to them with a message (the reply port of
//! a call). The kernel stamps every message with the sender's process id,
//! so a service knows who asks; programs cannot forge it.
//!
//! Ports are counted: a port with no name, nothing queued and nobody left
//! able to send to it is `Closed` for its receiver, which is how a caller
//! learns that a service ended without answering. When the owner ends, the
//! port goes away and sending to it fails with `Closed`.
//!
//! Every send and close wakes the one wait queue; receivers look again.
//! Waits are cut into short slices so a stopped program does not hang in
//! one.

use crate::sync::IrqLock;
use crate::task::WaitQueue;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::vec::Vec;
use justdk::syscall::{self as sys, Error};

/// A queued message.
pub struct Message {
    pub data: Vec<u8>,
    /// Process id of the sender (0: the kernel).
    pub sender: u32,
    /// A port the answer goes to; the message holds a right to send there,
    /// which passes to whoever takes the message.
    pub reply: Option<u32>,
}

struct Port {
    owner: u32,
    name: Option<String>,
    queue: VecDeque<Message>,
    /// Handles (and queued messages) that may send here.
    senders: usize,
}

struct Table {
    ports: BTreeMap<u32, Port>,
    names: BTreeMap<String, u32>,
    next: u32,
}

static TABLE: IrqLock<Table> = IrqLock::new(Table { ports: BTreeMap::new(), names: BTreeMap::new(), next: 1 });
static CHANGED: WaitQueue = WaitQueue::new();
/// How long a wait sleeps before looking whether its program was stopped.
const SLICE_MS: u64 = 50;

/// Names are lowercase words joined by dots: `just.notify`, `org.example.chat`.
fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'_'))
}

/// Makes a port owned by `owner`, named or not.
pub fn create(owner: u32, name: Option<&str>) -> Result<u32, Error> {
    if name.is_some_and(|n| !valid_name(n)) {
        return Err(Error::InvalidName);
    }
    let mut t = TABLE.lock();
    if name.is_some_and(|n| t.names.contains_key(n)) {
        return Err(Error::Exists);
    }
    let id = t.next;
    t.next += 1;
    if let Some(n) = name {
        t.names.insert(String::from(n), id);
    }
    t.ports.insert(id, Port { owner, name: name.map(String::from), queue: VecDeque::new(), senders: 0 });
    Ok(id)
}

/// A right to send to a named port.
pub fn lookup(name: &str) -> Result<u32, Error> {
    let mut t = TABLE.lock();
    let id = *t.names.get(name).ok_or(Error::NotFound)?;
    if let Some(p) = t.ports.get_mut(&id) {
        p.senders += 1;
    }
    Ok(id)
}

/// A right to send was given up (a handle closed, a message dropped).
pub fn drop_sender(id: u32) {
    let mut t = TABLE.lock();
    if let Some(p) = t.ports.get_mut(&id) {
        p.senders = p.senders.saturating_sub(1);
    }
    drop(t);
    CHANGED.notify_all();
}

/// The owner is done with a port: it goes, with what was queued.
pub fn close(id: u32) {
    let port = {
        let mut t = TABLE.lock();
        let p = t.ports.remove(&id);
        if let Some(n) = p.as_ref().and_then(|p| p.name.as_ref()) {
            t.names.remove(n);
        }
        p
    };
    // Unanswered requests: their callers learn the port is gone.
    for m in port.into_iter().flat_map(|p| p.queue) {
        if let Some(r) = m.reply {
            drop_sender(r);
        }
    }
    CHANGED.notify_all();
}

/// Queues a message. The right to send to `reply` moves into the message.
pub fn send(id: u32, data: Vec<u8>, sender: u32, reply: Option<u32>) -> Result<(), Error> {
    if data.len() as u64 > sys::PORT_MSG_MAX {
        return Err(Error::TooLarge);
    }
    let mut t = TABLE.lock();
    let p = t.ports.get_mut(&id).ok_or(Error::Closed)?;
    if p.queue.len() >= sys::PORT_QUEUE_MAX {
        return Err(Error::Full);
    }
    p.queue.push_back(Message { data, sender, reply });
    drop(t);
    CHANGED.notify_all();
    Ok(())
}

/// Takes the next message of an own port, if there is one.
fn take(id: u32, owner: u32) -> Option<Result<Message, Error>> {
    let mut t = TABLE.lock();
    let Some(p) = t.ports.get_mut(&id) else { return Some(Err(Error::Closed)) };
    if p.owner != owner {
        return Some(Err(Error::BadHandle));
    }
    if let Some(m) = p.queue.pop_front() {
        return Some(Ok(m));
    }
    // Nobody can send any more.
    if p.name.is_none() && p.senders == 0 {
        return Some(Err(Error::Closed));
    }
    None
}

/// Waits for a message on an own port, for up to `timeout_ms` (`None`:
/// forever). `stop` is asked between slices whether to give up.
pub fn recv(id: u32, owner: u32, timeout_ms: Option<u64>, stop: &dyn Fn() -> bool) -> Result<Message, Error> {
    let until = timeout_ms.map(|ms| crate::time::uptime_ms().saturating_add(ms));
    loop {
        let left = until.map_or(SLICE_MS, |u| u.saturating_sub(crate::time::uptime_ms()).min(SLICE_MS));
        if let Some(r) = CHANGED.wait_for_timeout(left, || take(id, owner)) {
            return r;
        }
        if until.is_some_and(|u| crate::time::uptime_ms() >= u) {
            return Err(Error::Timeout);
        }
        if stop() {
            return Err(Error::Closed);
        }
    }
}

/// Sends a request and waits for its answer, on a private port made for it.
pub fn call(id: u32, data: Vec<u8>, sender: u32, timeout_ms: Option<u64>, stop: &dyn Fn() -> bool) -> Result<Vec<u8>, Error> {
    let reply = create(sender, None)?;
    TABLE.lock().ports.get_mut(&reply).expect("just made").senders = 1;
    if let Err(e) = send(id, data, sender, Some(reply)) {
        close(reply);
        return Err(e);
    }
    let r = recv(reply, sender, timeout_ms, stop);
    close(reply);
    r.map(|m| {
        // An answer that asks for an answer: not taken up.
        if let Some(r) = m.reply {
            drop_sender(r);
        }
        m.data
    })
}

/// The ports a process owns, for when it ends.
pub fn owned_by(pid: u32) -> Vec<u32> {
    TABLE.lock().ports.iter().filter(|(_, p)| p.owner == pid).map(|(&id, _)| id).collect()
}

/// Named ports and who owns them, for the shell.
pub fn list() -> Vec<(String, u32, usize)> {
    let t = TABLE.lock();
    t.names.iter().filter_map(|(n, id)| t.ports.get(id).map(|p| (n.clone(), p.owner, p.queue.len()))).collect()
}

// ----------------------------------------------------------------------------
// Services of the kernel

/// Starts the kernel's own services.
pub fn start_services() {
    if crate::task::spawn("just.notify", notify_service).is_none() {
        crate::warn!("ipc", "could not start just.notify");
    }
    if crate::task::spawn("just.permissions", permissions_service).is_none() {
        crate::warn!("ipc", "could not start just.permissions");
    }
}

/// `just.permissions`, the sandbox's table ([`crate::sandbox`]):
///
/// - `{"request": "camera"}`: may the sending app use it? Asks the user
///   the first time. `{"granted": true}`; programs without a sandbox may
///   always.
/// - `{"check": "files:Immagini"}`: `{"state": "allow" | "deny" | "ask"}`,
///   without asking.
///
/// And from programs without a sandbox only (the system's tools):
/// `{"list": true}` (every answer), `{"set": perm, "app": id, "allow":
/// true | false | null}` and `{"forget": id}`.
fn permissions_service() {
    use crate::sandbox;
    use justjson::Value;
    let port = match create(0, Some("just.permissions")) {
        Ok(p) => p,
        Err(e) => {
            crate::warn!("ipc", "just.permissions: {}", e.as_str());
            return;
        }
    };
    let error = |e: &str| Value::object(alloc::vec![("error", Value::string(e))]);
    loop {
        let Ok(m) = recv(port, 0, None, &|| false) else { continue };
        let app = crate::user::app_of(m.sender);
        let v = justjson::parse_bytes(&m.data).unwrap_or(Value::Null);
        let answer = if let Some(perm) = v.get("request").as_str().or(v.get("check").as_str()) {
            if !sandbox::known(perm) {
                error("permesso sconosciuto")
            } else if v.get("request").as_str().is_some() {
                let granted = app.as_ref().is_none_or(|a| sandbox::allowed(a, perm));
                Value::object(alloc::vec![("granted", Value::Bool(granted))])
            } else {
                let state = match app.as_ref().map(|a| sandbox::state(&a.id, perm)) {
                    None | Some(Some(true)) => "allow",
                    Some(Some(false)) => "deny",
                    Some(None) => "ask",
                };
                Value::object(alloc::vec![("state", Value::string(state))])
            }
        } else if app.is_some() {
            error("solo i programmi di sistema")
        } else if v.get("list").as_bool() == Some(true) {
            let apps = sandbox::all()
                .into_iter()
                .map(|g| {
                    let perms = g.perms.iter().map(|(p, allow)| (p.as_str(), Value::Bool(*allow))).collect();
                    Value::object(alloc::vec![("id", Value::string(&g.id)), ("name", Value::string(&g.name)), ("perms", Value::object(perms))])
                })
                .collect();
            Value::object(alloc::vec![("apps", Value::Arr(apps))])
        } else if let Some(perm) = v.get("set").as_str() {
            let id = v.get("app").str();
            if !sandbox::valid_id(id) || !sandbox::known(perm) {
                error("app o permesso non validi")
            } else {
                sandbox::set(id, v.get("name").str(), perm, v.get("allow").as_bool());
                Value::object(alloc::vec![("ok", Value::Bool(true))])
            }
        } else if let Some(id) = v.get("forget").as_str() {
            Value::object(alloc::vec![("ok", Value::Bool(sandbox::forget(id)))])
        } else {
            error("atteso {\"request\": …} o {\"check\": …}")
        };
        if let Some(r) = m.reply {
            let _ = send(r, alloc::format!("{answer}").into_bytes(), 0, None);
            drop_sender(r);
        }
    }
}

/// `just.notify`: `{"title": "…", "text": "…"}` shows a notification on the
/// desktop, under the sending program's name if there is no title. Answers
/// `{"ok":true}`, or `{"ok":false,"error":"…"}`.
fn notify_service() {
    let port = match create(0, Some("just.notify")) {
        Ok(p) => p,
        Err(e) => {
            crate::warn!("ipc", "just.notify: {}", e.as_str());
            return;
        }
    };
    loop {
        let Ok(m) = recv(port, 0, None, &|| false) else { continue };
        let answer = match justjson::parse_bytes(&m.data) {
            Some(v) if v.get("text").as_str().is_some() => {
                let from = crate::user::app_of(m.sender).map(|a| a.name).or_else(|| crate::user::process_name(m.sender)).unwrap_or_default();
                let title = v.get("title").as_str().filter(|t| !t.is_empty()).unwrap_or(&from);
                let text = v.get("text").str();
                crate::debug!("ipc", "notification from {} ({}): {title}: {text}", from, m.sender);
                crate::gui::notify(title, text, "window");
                String::from("{\"ok\":true}")
            }
            _ => String::from("{\"ok\":false,\"error\":\"atteso {\\\"title\\\": …, \\\"text\\\": …}\"}"),
        };
        if let Some(r) = m.reply {
            let _ = send(r, answer.into_bytes(), 0, None);
            drop_sender(r);
        }
    }
}
