//! A Wayland compositor, in the kernel: how Linux programs with a graphical
//! interface (GTK, Qt, SDL, Firefox...) show windows on justOS.
//!
//! It listens at `$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY` ([`RUNTIME_DIR`],
//! [`DISPLAY`]), a local socket of the kernel's own, and serves each client
//! from a thread of its own. Clients draw into shared memory (`wl_shm`
//! pools: a memfd passed with SCM_RIGHTS); on each commit the compositor
//! copies what changed, composes the surfaces of a window (the toplevel,
//! its subsurfaces, its popups) and hands the result to the desktop, which
//! shows it in a justOS window, with justOS's frame: decorations are the
//! server's (`xdg-decoration`, and KDE's protocol, which GTK reads). Pointer
//! and keyboard come back as `wl_pointer` and `wl_keyboard` events, the keys
//! through an XKB keymap of our own ([`keymap`]).
//!
//! What is there: `wl_compositor`, `wl_subcompositor`, `wl_shm` (ARGB8888,
//! XRGB8888), `wl_output` (with the screen's scale, for sharp HiDPI
//! drawing), `wl_seat` with pointer and keyboard, `xdg_wm_base` with
//! toplevels and popups (placed with their positioner inside the window),
//! `wp_viewporter` (surfaces cropped and scaled: Wine's windows), and the
//! data device (no clipboard yet).

mod keymap;
mod window;
mod wire;

pub use window::take_new;

use super::fd::{Desc, Kind};
use super::shm::Shm;
use super::unix::{self, Conn};
use crate::gui::gfx::{self, Canvas, Rect};
use crate::sync::{IrqLock, Mutex};
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use window::{Inbox, Input, Win};
use wire::{Args, Out};

/// `XDG_RUNTIME_DIR` and `WAYLAND_DISPLAY` for Linux programs.
pub const RUNTIME_DIR: &str = "/home/tmp";
pub const DISPLAY: &str = "wayland-0";

/// Largest window content (units), as for justOS's own programs.
const MAX_W: i32 = crate::gui::window::MAX_W;
const MAX_H: i32 = crate::gui::window::MAX_H;
/// A client that leaves this much unread is stuck: it is let go.
const MAX_UNREAD: usize = 8 << 20;

/// Starts the compositor (once the desktop runs).
pub fn start() {
    // The runtime folder, and /tmp, on the system volume.
    for tmp in [crate::fs::Path::root("sys").join("home").join("tmp"), crate::fs::Path::root("sys").join("tmp")] {
        if !crate::fs::exists(&tmp) {
            let _ = crate::fs::make_dir(&tmp);
        }
    }
    crate::task::spawn("wayland", serve);
}

fn serve() {
    let path = alloc::format!("{RUNTIME_DIR}/{DISPLAY}");
    let l = match unix::kernel_listen(&path) {
        Ok(l) => l,
        Err(e) => {
            crate::warn!("wayland", "cannot listen at {path}: {e}");
            return;
        }
    };
    loop {
        if let Some(conn) = l.accept(60_000) {
            if crate::task::spawn("wayland-client", move || Client::new(conn).run()).is_none() {
                crate::warn!("wayland", "no thread for a client");
            }
        }
    }
}

// ----------------------------------------------------------------------------
// Objects

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Iface {
    Display,
    Registry,
    Callback,
    Compositor,
    Surface,
    Region,
    Shm,
    Pool,
    Buffer,
    Subcompositor,
    Subsurface,
    WmBase,
    Positioner,
    XdgSurface,
    Toplevel,
    Popup,
    Seat,
    Pointer,
    Keyboard,
    Touch,
    Output,
    DataManager,
    DataSource,
    DataDevice,
    DecoManager,
    Deco,
    KdeManager,
    KdeDeco,
    Viewporter,
    Viewport,
}

/// The globals, by name (index + 1), with the versions offered.
const GLOBALS: &[(&str, u32, Iface)] = &[
    ("wl_compositor", 6, Iface::Compositor),
    ("wl_subcompositor", 1, Iface::Subcompositor),
    ("wl_shm", 1, Iface::Shm),
    ("wl_output", 4, Iface::Output),
    ("wl_seat", 7, Iface::Seat),
    ("wl_data_device_manager", 3, Iface::DataManager),
    ("xdg_wm_base", 5, Iface::WmBase),
    ("zxdg_decoration_manager_v1", 1, Iface::DecoManager),
    ("org_kde_kwin_server_decoration_manager", 1, Iface::KdeManager),
    ("wp_viewporter", 1, Iface::Viewporter),
];

/// A region: rectangles added and taken away, in order.
#[derive(Clone, Default)]
struct Region(Vec<(bool, Rect)>);

impl Region {
    fn contains(&self, x: i32, y: i32) -> bool {
        let mut inside = false;
        for (add, r) in &self.0 {
            if r.contains(x, y) {
                inside = *add;
            }
        }
        inside
    }
}

/// A buffer in a pool.
#[derive(Clone)]
struct Buf {
    shm: Option<Arc<Shm>>,
    off: u64,
    w: i32,
    h: i32,
    stride: i32,
    format: u32,
}

const FORMAT_ARGB: u32 = 0;
const FORMAT_XRGB: u32 = 1;

#[derive(Clone, Copy, Default)]
struct Positioner {
    w: i32,
    h: i32,
    anchor: Rect,
    edge: u32,
    gravity: u32,
    adjust: u32,
    off: (i32, i32),
}

enum Data {
    None,
    Region(Region),
    Pool(Option<Arc<Shm>>),
    Buffer(Buf),
    Positioner(Positioner),
    /// The wl_surface an xdg_surface, toplevel, popup, subsurface or
    /// decoration belongs to.
    Surface(u32),
    Source(Vec<String>),
}

struct Obj {
    iface: Iface,
    version: u32,
    data: Data,
}

// ----------------------------------------------------------------------------
// Surfaces

#[derive(Default)]
struct Pending {
    /// attach: a buffer, or 0 to take the content away.
    buffer: Option<u32>,
    /// Damage in surface units, and in buffer pixels.
    damage: Rect,
    bdamage: Rect,
    frames: Vec<u32>,
    scale: Option<i32>,
    /// set_input_region: Some(None) is all of the surface.
    input: Option<Option<Region>>,
    /// wp_viewport's source and destination (Some(None): unset).
    view_src: Option<Option<[i32; 4]>>,
    view_dst: Option<Option<(i32, i32)>>,
}

/// An xdg_surface's part of a toplevel or popup.
#[derive(Default)]
struct Xdg {
    id: u32,
    geometry: Option<Rect>,
    /// The first configure went out, and the client answered one.
    sent: bool,
    acked: bool,
}

struct Top {
    xdg: Xdg,
    id: u32,
    win: Option<Arc<Mutex<Win>>>,
    title: String,
    min: (i32, i32),
    max: (i32, i32),
    /// Popups, bottom to top (their surfaces).
    popups: Vec<u32>,
    /// The size last asked for (0: the client chooses).
    asked: (i32, i32),
    /// The frame a Windows program drew around its content (left, top,
    /// right, bottom, units): left out, as the desktop frames the window.
    nc: Option<[i32; 4]>,
}

struct Pop {
    xdg: Xdg,
    id: u32,
    /// The parent's wl_surface.
    parent: u32,
    /// Where it is, relative to the parent's window geometry.
    rect: Rect,
    grab: bool,
}

struct Sub {
    parent: u32,
    x: i32,
    y: i32,
}

enum Role {
    None,
    /// An xdg_surface waiting for get_toplevel or get_popup.
    Xdg(Xdg),
    Top(Top),
    Popup(Pop),
    Sub(Sub),
    Cursor,
}

struct Surface {
    pend: Pending,
    /// The content: a copy of the last buffer (straight alpha), its size
    /// in pixels and the pixels per unit.
    px: Vec<u32>,
    pw: i32,
    ph: i32,
    scale: i32,
    mapped: bool,
    /// Where it takes input; None is all of it.
    input: Option<Region>,
    role: Role,
    /// Its subsurfaces and itself, bottom to top.
    stack: Vec<u32>,
    /// Frame callbacks for the next time it is shown.
    frames: Vec<u32>,
    /// wp_viewport: the part of the content shown (x, y, w, h in surface
    /// units before scaling, 24.8 fixed point) and the size it is shown at
    /// (units). Unset: all of it, at its own size.
    view_src: Option<[i32; 4]>,
    view_dst: Option<(i32, i32)>,
}

impl Surface {
    fn new(id: u32) -> Surface {
        Surface {
            pend: Pending::default(),
            px: Vec::new(),
            pw: 0,
            ph: 0,
            scale: 1,
            mapped: false,
            input: None,
            role: Role::None,
            stack: alloc::vec![id],
            frames: Vec::new(),
            view_src: None,
            view_dst: None,
        }
    }

    /// The size in units: the viewport's destination, else its source,
    /// else the content's.
    fn size(&self) -> (i32, i32) {
        if let Some(d) = self.view_dst {
            return d;
        }
        if let Some([_, _, w, h]) = self.view_src {
            return ((w >> 8).max(1), (h >> 8).max(1));
        }
        (self.pw / self.scale, self.ph / self.scale)
    }

    fn viewported(&self) -> bool {
        self.view_src.is_some() || self.view_dst.is_some()
    }

    /// The part of the content shown, in its pixels (x, y, w, h).
    fn source_px(&self) -> (i32, i32, i32, i32) {
        let Some([x, y, w, h]) = self.view_src else { return (0, 0, self.pw, self.ph) };
        let k = self.scale.max(1) as i64;
        let px = |v: i32| ((v as i64 * k) >> 8) as i32;
        let (x, y) = (px(x).clamp(0, self.pw), px(y).clamp(0, self.ph));
        (x, y, px(w).clamp(1, (self.pw - x).max(1)), px(h).clamp(1, (self.ph - y).max(1)))
    }

    /// Pixels per unit the content has where it is shown (at least 1): the
    /// frame is made with as many, to keep it sharp.
    fn pixel_scale(&self) -> i32 {
        if !self.viewported() {
            return self.scale;
        }
        let (_, _, sw, _) = self.source_px();
        let (w, _) = self.size();
        ((sw + w - 1) / w.max(1)).clamp(1, gfx::MAX_SCALE)
    }

    fn xdg(&self) -> Option<&Xdg> {
        match &self.role {
            Role::Xdg(x) => Some(x),
            Role::Top(t) => Some(&t.xdg),
            Role::Popup(p) => Some(&p.xdg),
            _ => None,
        }
    }

    fn xdg_mut(&mut self) -> Option<&mut Xdg> {
        match &mut self.role {
            Role::Xdg(x) => Some(x),
            Role::Top(t) => Some(&mut t.xdg),
            Role::Popup(p) => Some(&mut p.xdg),
            _ => None,
        }
    }

    /// Where the window geometry starts on the surface: as the client set
    /// it, else inside a Windows program's own frame.
    fn geometry_offset(&self) -> (i32, i32) {
        if let Role::Top(Top { nc: Some(n), .. }) = &self.role {
            return (n[0], n[1]);
        }
        self.client_geometry_offset()
    }

    /// The window geometry's offset as the client set it: what its popups
    /// are placed from.
    fn client_geometry_offset(&self) -> (i32, i32) {
        self.xdg().and_then(|x| x.geometry).map_or((0, 0), |g| (g.x, g.y))
    }
}

/// Clamps what clients send as "everything" (INT32_MAX) to sizes that add
/// up without overflowing.
fn sane_rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
    const M: i32 = 1 << 16;
    Rect::new(x.clamp(-M, M), y.clamp(-M, M), w.clamp(0, M), h.clamp(0, M))
}

fn now() -> u32 {
    crate::time::uptime_ms() as u32
}

// ----------------------------------------------------------------------------
// A client

struct Client {
    conn: Conn,
    pid: u32,
    icon: String,
    /// A Windows program (Wine): it draws a frame of its own.
    windows: bool,
    inbuf: Vec<u8>,
    infds: VecDeque<Arc<Desc>>,
    out: Out,
    objs: BTreeMap<u32, Obj>,
    surfs: BTreeMap<u32, Surface>,
    inbox: Arc<Inbox>,
    serial: u32,
    keymap: keymap::Keymap,
    /// The surface under the pointer (and its window), and where.
    pointer: Option<(u32, u32)>,
    /// The surface with the keyboard, and the modifiers it was last told.
    keyboard: Option<u32>,
    mods: u32,
    /// Frame callbacks of surfaces on no window: when to answer them.
    timers: Vec<(u64, u32)>,
    dead: bool,
}

impl Client {
    fn new(conn: Conn) -> Client {
        let pid = conn.peer_pid();
        let name = crate::user::name_of(pid);
        let icon = crate::gui::window::icon_for(&name);
        let mut objs = BTreeMap::new();
        objs.insert(1, Obj { iface: Iface::Display, version: 1, data: Data::None });
        Client {
            conn,
            pid,
            windows: name.starts_with("wine"),
            icon,
            inbuf: Vec::new(),
            infds: VecDeque::new(),
            out: Out::default(),
            objs,
            surfs: BTreeMap::new(),
            inbox: Inbox::new(),
            serial: 0,
            keymap: keymap::Keymap::default(),
            pointer: None,
            keyboard: None,
            mods: 0,
            timers: Vec::new(),
            dead: false,
        }
    }

    fn run(mut self) {
        crate::debug!("wayland", "client {} connected", self.pid);
        loop {
            let wait = match self.timers.iter().map(|t| t.0).min() {
                Some(t) => t.saturating_sub(crate::time::uptime_ms()).clamp(1, 1000),
                None => 1000,
            };
            {
                // Asleep until the client writes or input comes.
                let (conn, inbox) = (&self.conn, &self.inbox);
                let until = crate::time::uptime_ms() + wait;
                crate::task::wait_any(&[conn.queue(), &inbox.wq], Some(until), || (conn.ready() || !inbox.is_empty()).then_some(()));
            }
            let open = self.conn.take(&mut self.inbuf, &mut self.infds);
            self.requests();
            for (id, ev) in self.inbox.take() {
                self.input(id, ev);
            }
            self.fire_timers();
            self.flush();
            if !open || self.dead {
                break;
            }
        }
        // The windows go with the client.
        for s in self.surfs.values() {
            if let Role::Top(t) = &s.role {
                if let Some(w) = &t.win {
                    w.lock().ended = true;
                }
            }
        }
        crate::debug!("wayland", "client {} gone", self.pid);
    }

    fn flush(&mut self) {
        if self.out.data.is_empty() {
            return;
        }
        let out = core::mem::take(&mut self.out);
        if !self.conn.send(out.data, out.fds) || self.conn.unread() > MAX_UNREAD {
            self.dead = true;
        }
    }

    fn next_serial(&mut self) -> u32 {
        self.serial = self.serial.wrapping_add(1);
        self.serial
    }

    fn requests(&mut self) {
        let buf = core::mem::take(&mut self.inbuf);
        let mut at = 0;
        while let Some((obj, op, body, len)) = wire::next(&buf, at) {
            at += len;
            let body = body.to_vec();
            let mut fds = core::mem::take(&mut self.infds);
            {
                let mut a = Args::new(&body, &mut fds);
                self.request(obj, op, &mut a);
            }
            self.infds = fds;
            if self.out.fds.len() >= 8 {
                self.flush();
            }
            if self.dead {
                return;
            }
        }
        // Keep the start of a message that is not all there yet.
        self.inbuf = buf[at..].to_vec();
    }

    fn iface(&self, id: u32) -> Option<Iface> {
        self.objs.get(&id).map(|o| o.iface)
    }

    fn version(&self, id: u32) -> u32 {
        self.objs.get(&id).map_or(1, |o| o.version)
    }

    fn add(&mut self, id: u32, iface: Iface, version: u32, data: Data) {
        if id != 0 {
            self.objs.insert(id, Obj { iface, version, data });
        }
    }

    /// The client destroyed `id`: it may use the number again.
    fn destroyed(&mut self, id: u32) {
        self.objs.remove(&id);
        self.out.event(1, 1).uint(id);
    }

    /// A protocol error: the client is told and let go.
    fn error(&mut self, obj: u32, code: u32, msg: &str) {
        crate::warn!("wayland", "client {}: {msg}", self.pid);
        self.out.event(1, 0).object(obj).uint(code).string(msg);
        self.dead = true;
    }

    fn objects_of(&self, iface: Iface) -> Vec<(u32, u32)> {
        self.objs.iter().filter(|(_, o)| o.iface == iface).map(|(&id, o)| (id, o.version)).collect()
    }

    fn surface_of(&self, id: u32) -> Option<u32> {
        match self.objs.get(&id)?.data {
            Data::Surface(s) => Some(s),
            _ => None,
        }
    }

    // ------------------------------------------------------------------------
    // Requests

    fn request(&mut self, obj: u32, op: u16, a: &mut Args<'_>) {
        let Some(iface) = self.iface(obj) else {
            // A request to an object the client just destroyed: nothing to do.
            return;
        };
        let ver = self.version(obj);
        match (iface, op) {
            // wl_display
            (Iface::Display, 0) => {
                let cb = a.uint();
                self.out.event(cb, 0).uint(self.serial);
                self.out.event(1, 1).uint(cb);
            }
            (Iface::Display, 1) => {
                let id = a.uint();
                self.add(id, Iface::Registry, 1, Data::None);
                for (i, (name, v, _)) in GLOBALS.iter().enumerate() {
                    self.out.event(id, 0).uint(i as u32 + 1).string(name).uint(*v);
                }
            }
            // wl_registry.bind
            (Iface::Registry, 0) => {
                let name = a.uint();
                let _iface = a.text();
                let v = a.uint();
                let id = a.uint();
                match GLOBALS.get((name as usize).wrapping_sub(1)) {
                    Some(&(_, max, kind)) => self.bind(id, kind, v.clamp(1, max)),
                    None => self.error(obj, 0, "no such global"),
                }
            }
            // wl_compositor
            (Iface::Compositor, 0) => {
                let id = a.uint();
                self.add(id, Iface::Surface, ver, Data::None);
                self.surfs.insert(id, Surface::new(id));
            }
            (Iface::Compositor, 1) => {
                let id = a.uint();
                self.add(id, Iface::Region, 1, Data::Region(Region::default()));
            }
            // wl_region
            (Iface::Region, 0) => self.destroyed(obj),
            (Iface::Region, 1 | 2) => {
                let r = sane_rect(a.int(), a.int(), a.int(), a.int());
                if let Some(Obj { data: Data::Region(reg), .. }) = self.objs.get_mut(&obj) {
                    reg.0.push((op == 1, r));
                }
            }
            (Iface::Surface, _) => self.surface_request(obj, op, a),
            // wl_shm
            (Iface::Shm, 0) => {
                let id = a.uint();
                let fd = a.fd();
                let _size = a.int();
                let shm = match fd.as_deref().map(|d| &d.kind) {
                    Some(Kind::Memfd { shm, .. }) => Some(shm.clone()),
                    _ => {
                        crate::warn!("wayland", "client {}: a pool that is not shared memory", self.pid);
                        None
                    }
                };
                self.add(id, Iface::Pool, 1, Data::Pool(shm));
            }
            (Iface::Shm, 1) => self.destroyed(obj),
            // wl_shm_pool
            (Iface::Pool, 0) => {
                let id = a.uint();
                let (off, w, h, stride, format) = (a.int(), a.int(), a.int(), a.int(), a.uint());
                let shm = match &self.objs.get(&obj).map(|o| &o.data) {
                    Some(Data::Pool(s)) => s.clone(),
                    _ => None,
                };
                let ok = off >= 0 && w > 0 && h > 0 && stride >= w.saturating_mul(4) && w <= MAX_W * 4 && h <= MAX_H * 4;
                if !ok || (format != FORMAT_ARGB && format != FORMAT_XRGB) {
                    return self.error(obj, 0, "bad buffer");
                }
                self.add(id, Iface::Buffer, 1, Data::Buffer(Buf { shm, off: off as u64, w, h, stride, format }));
            }
            (Iface::Pool, 1) => self.destroyed(obj),
            (Iface::Pool, 2) => {} // the memfd grew; the pool is all of it anyway
            (Iface::Buffer, 0) => self.destroyed(obj),
            // wl_subcompositor
            (Iface::Subcompositor, 0) => self.destroyed(obj),
            (Iface::Subcompositor, 1) => {
                let (id, surf, parent) = (a.uint(), a.uint(), a.uint());
                if !self.surfs.contains_key(&surf) || !self.surfs.contains_key(&parent) || surf == parent {
                    return self.error(obj, 0, "bad subsurface");
                }
                self.add(id, Iface::Subsurface, 1, Data::Surface(surf));
                if let Some(s) = self.surfs.get_mut(&surf) {
                    s.role = Role::Sub(Sub { parent, x: 0, y: 0 });
                }
                if let Some(p) = self.surfs.get_mut(&parent) {
                    p.stack.push(surf);
                }
            }
            (Iface::Subsurface, _) => self.subsurface_request(obj, op, a),
            // xdg_wm_base
            (Iface::WmBase, 0) => self.destroyed(obj),
            (Iface::WmBase, 1) => {
                let id = a.uint();
                self.add(id, Iface::Positioner, 1, Data::Positioner(Positioner::default()));
            }
            (Iface::WmBase, 2) => {
                let (id, surf) = (a.uint(), a.uint());
                let Some(s) = self.surfs.get_mut(&surf) else { return self.error(obj, 0, "no surface") };
                s.role = Role::Xdg(Xdg { id, ..Xdg::default() });
                self.add(id, Iface::XdgSurface, ver, Data::Surface(surf));
            }
            (Iface::WmBase, 3) => {} // pong
            (Iface::Positioner, 0) => self.destroyed(obj),
            (Iface::Positioner, _) => {
                if let Some(Obj { data: Data::Positioner(p), .. }) = self.objs.get_mut(&obj) {
                    match op {
                        1 => (p.w, p.h) = (a.int(), a.int()),
                        2 => p.anchor = sane_rect(a.int(), a.int(), a.int(), a.int()),
                        3 => p.edge = a.uint(),
                        4 => p.gravity = a.uint(),
                        5 => p.adjust = a.uint(),
                        6 => p.off = (a.int(), a.int()),
                        _ => {}
                    }
                }
            }
            (Iface::XdgSurface, _) => self.xdg_surface_request(obj, op, a),
            (Iface::Toplevel, _) => self.toplevel_request(obj, op, a),
            (Iface::Popup, _) => self.popup_request(obj, op, a),
            // wl_seat
            (Iface::Seat, 0) => {
                let id = a.uint();
                self.add(id, Iface::Pointer, ver, Data::None);
            }
            (Iface::Seat, 1) => {
                let id = a.uint();
                self.add(id, Iface::Keyboard, ver, Data::None);
                self.send_keymap(id);
                if ver >= 4 {
                    // justOS repeats keys itself.
                    self.out.event(id, 5).int(0).int(0);
                }
                if let Some(s) = self.keyboard {
                    let serial = self.next_serial();
                    self.out.event(id, 1).uint(serial).object(s).array(&[]);
                    self.out.event(id, 4).uint(serial).uint(self.mods).uint(0).uint(0).uint(0);
                }
            }
            (Iface::Seat, 2) => {
                let id = a.uint();
                self.add(id, Iface::Touch, ver, Data::None);
            }
            (Iface::Seat, 3) | (Iface::Pointer, 1) | (Iface::Keyboard, 0) | (Iface::Touch, 0) | (Iface::Output, 0) => {
                self.destroyed(obj)
            }
            (Iface::Pointer, 0) => {
                let (_serial, surf) = (a.uint(), a.uint());
                if let Some(s) = self.surfs.get_mut(&surf) {
                    if matches!(s.role, Role::None) {
                        s.role = Role::Cursor;
                    }
                }
            }
            // The data device: enough for programs to start; no clipboard yet.
            (Iface::DataManager, 0) => {
                let id = a.uint();
                self.add(id, Iface::DataSource, ver, Data::Source(Vec::new()));
            }
            (Iface::DataManager, 1) => {
                let (id, _seat) = (a.uint(), a.uint());
                self.add(id, Iface::DataDevice, ver, Data::None);
            }
            (Iface::DataSource, 0) => {
                let mime = a.text();
                if let Some(Obj { data: Data::Source(m), .. }) = self.objs.get_mut(&obj) {
                    m.push(mime);
                }
            }
            (Iface::DataSource, 1) | (Iface::DataDevice, 2) => self.destroyed(obj),
            (Iface::DataSource, _) | (Iface::DataDevice, _) => {}
            // Decorations: always the server's.
            // wp_viewporter
            (Iface::Viewporter, 0) => self.destroyed(obj),
            (Iface::Viewporter, 1) => {
                let (id, surf) = (a.uint(), a.uint());
                if !self.surfs.contains_key(&surf) {
                    return self.error(obj, 0, "no surface");
                }
                self.add(id, Iface::Viewport, 1, Data::Surface(surf));
            }
            (Iface::Viewport, _) => self.viewport_request(obj, op, a),
            (Iface::DecoManager, 0) => self.destroyed(obj),
            (Iface::DecoManager, 1) => {
                let (id, top) = (a.uint(), a.uint());
                let surf = self.surface_of(top).unwrap_or(0);
                self.add(id, Iface::Deco, 1, Data::Surface(surf));
                self.out.event(id, 0).uint(2);
                self.reconfigure_shown(surf);
            }
            (Iface::Deco, 0) => self.destroyed(obj),
            (Iface::Deco, _) => {
                self.out.event(obj, 0).uint(2);
                let surf = self.surface_of(obj).unwrap_or(0);
                self.reconfigure_shown(surf);
            }
            (Iface::KdeManager, 0) => {
                let (id, surf) = (a.uint(), a.uint());
                self.add(id, Iface::KdeDeco, 1, Data::Surface(surf));
                self.out.event(id, 0).uint(2);
            }
            (Iface::KdeDeco, 0) => self.destroyed(obj),
            (Iface::KdeDeco, _) => {
                self.out.event(obj, 0).uint(2);
            }
            _ => crate::debug!("wayland", "client {}: {:?} request {op} ignored", self.pid, iface),
        }
        if a.bad {
            self.error(obj, 1, "malformed request");
        }
    }

    fn bind(&mut self, id: u32, kind: Iface, v: u32) {
        self.add(id, kind, v, Data::None);
        match kind {
            Iface::Shm => {
                self.out.event(id, 0).uint(FORMAT_ARGB);
                self.out.event(id, 0).uint(FORMAT_XRGB);
            }
            Iface::Seat => {
                self.out.event(id, 0).uint(3); // pointer and keyboard
                if v >= 2 {
                    self.out.event(id, 1).string("seat0");
                }
            }
            Iface::Output => self.send_output(id, v),
            Iface::KdeManager => {
                self.out.event(id, 0).uint(2); // default mode: the server's
            }
            _ => {}
        }
    }

    fn send_output(&mut self, id: u32, v: u32) {
        let (pw, ph) = crate::console::framebuffer().map_or((1024, 768), |f| (f.width as i32, f.height as i32));
        let k = crate::gui::scale().max(1);
        // About 96 dots per inch at one pixel per unit.
        let (mw, mh) = (pw * 254 / 96 / 10 / k, ph * 254 / 96 / 10 / k);
        self.out.event(id, 0).int(0).int(0).int(mw).int(mh).int(0).string("justOS").string("Display").int(0);
        self.out.event(id, 1).uint(3).int(pw).int(ph).int(60_000);
        if v >= 2 {
            self.out.event(id, 3).int(k);
        }
        if v >= 4 {
            self.out.event(id, 4).string("justOS-1");
            self.out.event(id, 5).string("justOS display");
        }
        if v >= 2 {
            self.out.event(id, 2);
        }
    }

    fn send_keymap(&mut self, kb: u32) {
        let text = self.keymap.text();
        let shm = Shm::sealed(String::from("/memfd:xkb-keymap (deleted)"), &text);
        let d = Desc::new(Kind::Memfd { shm, pos: IrqLock::new(0) }, 0);
        self.out.event(kb, 0).uint(1).fd(d).uint(text.len() as u32);
    }

    fn surface_request(&mut self, id: u32, op: u16, a: &mut Args<'_>) {
        if op == 0 {
            self.destroy_surface(id);
            return self.destroyed(id);
        }
        if op == 3 {
            let cb = a.uint();
            self.add(cb, Iface::Callback, 1, Data::None);
            if let Some(s) = self.surfs.get_mut(&id) {
                s.pend.frames.push(cb);
            }
            return;
        }
        let input = if op == 5 {
            let r = a.uint();
            Some(match self.objs.get(&r).map(|o| &o.data) {
                Some(Data::Region(reg)) => Some(reg.clone()),
                _ => None,
            })
        } else {
            None
        };
        let buffer = if op == 1 { Some(a.uint()) } else { None };
        let Some(s) = self.surfs.get_mut(&id) else { return };
        match op {
            1 => {
                let _ = (a.int(), a.int());
                s.pend.buffer = buffer;
            }
            2 => s.pend.damage = s.pend.damage.union(&sane_rect(a.int(), a.int(), a.int(), a.int())),
            4 => {
                let _ = a.uint(); // the opaque region: a hint
            }
            5 => s.pend.input = input,
            6 => self.commit(id),
            7 => {
                let _ = a.int(); // transforms are not supported: drawn as is
            }
            8 => s.pend.scale = Some(a.int().clamp(1, gfx::MAX_SCALE)),
            9 => s.pend.bdamage = s.pend.bdamage.union(&sane_rect(a.int(), a.int(), a.int(), a.int())),
            10 => {
                let _ = (a.int(), a.int());
            }
            _ => {}
        }
    }

    /// wp_viewport: what part of the surface's content to show, and at what
    /// size; both take effect on the surface's next commit.
    fn viewport_request(&mut self, obj: u32, op: u16, a: &mut Args<'_>) {
        let Some(surf) = self.surface_of(obj) else { return };
        if op == 0 {
            self.destroyed(obj);
        }
        let Some(s) = self.surfs.get_mut(&surf) else { return };
        match op {
            0 => {
                s.pend.view_src = Some(None);
                s.pend.view_dst = Some(None);
            }
            1 => {
                let r = [a.int(), a.int(), a.int(), a.int()];
                const UNSET: i32 = -256; // -1.0
                if r == [UNSET; 4] {
                    s.pend.view_src = Some(None);
                } else if r[0] >= 0 && r[1] >= 0 && r[2] > 0 && r[3] > 0 {
                    s.pend.view_src = Some(Some(r));
                }
            }
            2 => {
                let (w, h) = (a.int(), a.int());
                if (w, h) == (-1, -1) {
                    s.pend.view_dst = Some(None);
                } else if w > 0 && h > 0 {
                    s.pend.view_dst = Some(Some((w.min(MAX_W), h.min(MAX_H))));
                }
            }
            _ => {}
        }
    }

    fn subsurface_request(&mut self, obj: u32, op: u16, a: &mut Args<'_>) {
        let Some(surf) = self.surface_of(obj) else { return };
        match op {
            0 => {
                let root = self.root(surf);
                self.unparent(surf);
                if let Some(s) = self.surfs.get_mut(&surf) {
                    s.role = Role::None;
                }
                self.destroyed(obj);
                if let Some(t) = root {
                    self.compose(t, None);
                }
            }
            1 => {
                let (x, y) = (a.int(), a.int());
                if let Some(Surface { role: Role::Sub(b), .. }) = self.surfs.get_mut(&surf) {
                    (b.x, b.y) = (x.clamp(-MAX_W, MAX_W), y.clamp(-MAX_H, MAX_H));
                }
            }
            2 | 3 => {
                let sibling = a.uint();
                let Some(Surface { role: Role::Sub(b), .. }) = self.surfs.get(&surf) else { return };
                let parent = b.parent;
                if let Some(p) = self.surfs.get_mut(&parent) {
                    p.stack.retain(|&c| c != surf);
                    let at = p.stack.iter().position(|&c| c == sibling).unwrap_or(p.stack.len().saturating_sub(1));
                    let at = if op == 2 { at + 1 } else { at };
                    p.stack.insert(at.min(p.stack.len()), surf);
                }
            }
            _ => {} // sync and desync: every commit shows at once here
        }
    }

    fn xdg_surface_request(&mut self, obj: u32, op: u16, a: &mut Args<'_>) {
        let Some(surf) = self.surface_of(obj) else { return };
        match op {
            0 => self.destroyed(obj),
            1 => {
                let id = a.uint();
                let ver = self.version(obj);
                self.add(id, Iface::Toplevel, ver, Data::Surface(surf));
                let Some(s) = self.surfs.get_mut(&surf) else { return };
                let xdg = match core::mem::replace(&mut s.role, Role::None) {
                    Role::Xdg(x) => x,
                    _ => Xdg { id: obj, ..Xdg::default() },
                };
                s.role = Role::Top(Top { xdg, id, win: None, title: String::new(), min: (0, 0), max: (0, 0), popups: Vec::new(), asked: (0, 0), nc: None });
            }
            2 => {
                let (id, parent_xdg, positioner) = (a.uint(), a.uint(), a.uint());
                let ver = self.version(obj);
                self.add(id, Iface::Popup, ver, Data::Surface(surf));
                let parent = self.surface_of(parent_xdg).unwrap_or(0);
                let pos = match self.objs.get(&positioner).map(|o| &o.data) {
                    Some(Data::Positioner(p)) => *p,
                    _ => Positioner::default(),
                };
                let rect = self.place(parent, &pos);
                let Some(s) = self.surfs.get_mut(&surf) else { return };
                let xdg = match core::mem::replace(&mut s.role, Role::None) {
                    Role::Xdg(x) => x,
                    _ => Xdg { id: obj, ..Xdg::default() },
                };
                s.role = Role::Popup(Pop { xdg, id, parent, rect, grab: false });
                if let Some(t) = self.root(surf) {
                    if let Some(Surface { role: Role::Top(top), .. }) = self.surfs.get_mut(&t) {
                        top.popups.push(surf);
                    }
                }
            }
            3 => {
                let g = sane_rect(a.int(), a.int(), a.int(), a.int());
                if let Some(x) = self.surfs.get_mut(&surf).and_then(|s| s.xdg_mut()) {
                    if !g.is_empty() {
                        x.geometry = Some(g);
                    }
                }
            }
            4 => {
                let _serial = a.uint();
                if let Some(x) = self.surfs.get_mut(&surf).and_then(|s| s.xdg_mut()) {
                    x.acked = true;
                }
            }
            _ => {}
        }
    }

    fn toplevel_request(&mut self, obj: u32, op: u16, a: &mut Args<'_>) {
        let Some(surf) = self.surface_of(obj) else { return };
        let Some(Surface { role: Role::Top(t), .. }) = self.surfs.get_mut(&surf) else {
            if op == 0 {
                self.destroyed(obj);
            }
            return;
        };
        match op {
            0 => {
                if let Some(w) = t.win.take() {
                    w.lock().ended = true;
                }
                let xdg = core::mem::take(&mut t.xdg);
                if let Some(s) = self.surfs.get_mut(&surf) {
                    s.role = Role::Xdg(xdg);
                }
                if self.keyboard == Some(surf) {
                    self.keyboard = None;
                }
                if self.pointer.is_some_and(|p| p.1 == surf) {
                    self.pointer = None;
                }
                self.destroyed(obj);
            }
            2 => {
                t.title = a.text();
                if let Some(w) = &t.win {
                    w.lock().title = t.title.clone();
                }
            }
            7 => {
                t.max = (a.int(), a.int());
                self.update_resizable(surf);
            }
            8 => {
                t.min = (a.int(), a.int());
                self.update_resizable(surf);
            }
            // move, resize, maximize, fullscreen, minimize: the desktop's
            // frame does these.
            _ => {}
        }
    }

    fn update_resizable(&mut self, surf: u32) {
        if let Some(Surface { role: Role::Top(t), .. }) = self.surfs.get(&surf) {
            // A Windows window that cannot be resized has a thinner frame
            // (a dialog's 3 pixels, a sizing frame's 4).
            let fixed = (t.min.0 > 0 && t.min == t.max) || t.nc.is_some_and(|n| n[0] < 4);
            if let Some(w) = &t.win {
                w.lock().resizable = !fixed;
            }
        }
    }

    fn popup_request(&mut self, obj: u32, op: u16, a: &mut Args<'_>) {
        let Some(surf) = self.surface_of(obj) else { return };
        match op {
            0 => {
                self.close_popup(surf);
                self.destroyed(obj);
            }
            1 => {
                let _ = (a.uint(), a.uint());
                if let Some(Surface { role: Role::Popup(p), .. }) = self.surfs.get_mut(&surf) {
                    p.grab = true;
                }
                // Keys go to the menu now.
                self.focus_keyboard(Some(surf));
            }
            2 => {
                let (positioner, token) = (a.uint(), a.uint());
                let pos = match self.objs.get(&positioner).map(|o| &o.data) {
                    Some(Data::Positioner(p)) => *p,
                    _ => Positioner::default(),
                };
                let parent = match self.surfs.get(&surf) {
                    Some(Surface { role: Role::Popup(p), .. }) => p.parent,
                    _ => return,
                };
                let rect = self.place(parent, &pos);
                if let Some(Surface { role: Role::Popup(p), .. }) = self.surfs.get_mut(&surf) {
                    p.rect = rect;
                    self.out.event(obj, 2).uint(token);
                }
                self.configure_popup(surf);
            }
            _ => {}
        }
    }

    /// A popup goes: off its window, and the keyboard back to what is below.
    fn close_popup(&mut self, surf: u32) {
        let root = self.root(surf);
        if let Some(s) = self.surfs.get_mut(&surf) {
            let xdg = match core::mem::replace(&mut s.role, Role::None) {
                Role::Popup(p) => p.xdg,
                other => {
                    s.role = other;
                    return;
                }
            };
            s.role = Role::Xdg(xdg);
        }
        if let Some(t) = root {
            if let Some(Surface { role: Role::Top(top), .. }) = self.surfs.get_mut(&t) {
                top.popups.retain(|&p| p != surf);
            }
            if self.keyboard == Some(surf) {
                let next = self.keyboard_target(t);
                self.focus_keyboard(Some(next));
            }
            if self.pointer.is_some_and(|p| p.0 == surf) {
                self.pointer = None;
            }
            self.compose(t, None);
        }
    }

    fn destroy_surface(&mut self, id: u32) {
        let root = self.root(id);
        if let Some(Surface { role: Role::Top(t), .. }) = self.surfs.get(&id) {
            if let Some(w) = &t.win {
                w.lock().ended = true;
            }
        }
        if matches!(self.surfs.get(&id).map(|s| &s.role), Some(Role::Popup(_))) {
            self.close_popup(id);
        }
        self.unparent(id);
        // Its subsurfaces lose their parent.
        let children: Vec<u32> = self.surfs.get(&id).map_or(Vec::new(), |s| s.stack.iter().copied().filter(|&c| c != id).collect());
        for c in children {
            if let Some(s) = self.surfs.get_mut(&c) {
                s.role = Role::None;
            }
        }
        if let Some(s) = self.surfs.remove(&id) {
            for cb in s.frames.into_iter().chain(s.pend.frames) {
                self.objs.remove(&cb);
            }
        }
        if self.keyboard == Some(id) {
            self.keyboard = None;
        }
        if self.pointer.is_some_and(|p| p.0 == id || p.1 == id) {
            self.pointer = None;
        }
        if let Some(t) = root.filter(|&t| t != id) {
            self.compose(t, None);
        }
    }

    /// Takes a subsurface out of its parent's stack.
    fn unparent(&mut self, id: u32) {
        if let Some(Surface { role: Role::Sub(b), .. }) = self.surfs.get(&id) {
            let parent = b.parent;
            if let Some(p) = self.surfs.get_mut(&parent) {
                p.stack.retain(|&c| c != id);
            }
        }
    }

    // ------------------------------------------------------------------------
    // Commit

    fn commit(&mut self, id: u32) {
        let Some(s) = self.surfs.get_mut(&id) else { return };
        let pend = core::mem::take(&mut s.pend);
        let old_size = s.size();
        let old_scale = s.scale;
        let was_mapped = s.mapped;
        if let Some(k) = pend.scale {
            s.scale = k;
        }
        if let Some(input) = pend.input {
            s.input = input;
        }
        if let Some(v) = pend.view_src {
            s.view_src = v;
        }
        if let Some(v) = pend.view_dst {
            s.view_dst = v;
        }
        s.frames.extend(pend.frames);
        let mut dirty = None;
        if let Some(b) = pend.buffer {
            let buf = match self.objs.get(&b).map(|o| &o.data) {
                Some(Data::Buffer(buf)) => Some(buf.clone()),
                _ => None,
            };
            let s = self.surfs.get_mut(&id).unwrap();
            match buf {
                Some(buf) => {
                    let k = s.scale;
                    let damage = pend.bdamage.union(&Rect::new(pend.damage.x * k, pend.damage.y * k, pend.damage.w * k, pend.damage.h * k));
                    let full = import(s, &buf, damage);
                    s.mapped = true;
                    dirty = Some(if full { Rect::new(0, 0, MAX_W * 4, MAX_H * 4) } else { damage });
                    self.out.event(b, 0); // released: we have our copy
                }
                None => {
                    s.mapped = false;
                    s.px = Vec::new();
                    (s.pw, s.ph) = (0, 0);
                }
            }
        }
        let mut framed = false;
        if self.windows && dirty.is_some() {
            let s = self.surfs.get_mut(&id).unwrap();
            let nc = windows_frame(s);
            if let Role::Top(t) = &mut s.role {
                framed = t.nc != nc;
                t.nc = nc;
            }
            if framed {
                crate::debug!("wayland", "windows frame {:?}", nc);
                self.update_resizable(id);
            }
        }
        let s = self.surfs.get(&id).unwrap();
        // Damage on a cropped or scaled surface is not mapped: all of it.
        let changed = framed || s.size() != old_size || s.scale != old_scale || s.mapped != was_mapped || (dirty.is_some() && s.viewported());
        // What changed, in units of the surface.
        let dirty_units = dirty.map(|d| {
            let k = s.scale.max(1);
            Rect::new(d.x / k, d.y / k, (d.right() + k - 1) / k - d.x / k, (d.bottom() + k - 1) / k - d.y / k)
        });
        match &s.role {
            Role::Top(_) => self.commit_toplevel(id, changed, dirty_units),
            Role::Popup(_) => self.commit_popup(id),
            _ => {}
        }
        let root = self.root(id);
        let shown = root.and_then(|t| match self.surfs.get(&t).map(|s| &s.role) {
            Some(Role::Top(top)) => top.win.clone(),
            _ => None,
        });
        match (root, shown) {
            (Some(t), Some(_)) => {
                let full = changed || !matches!(self.surfs.get(&id).map(|s| &s.role), Some(Role::Top(_)) | Some(Role::Sub(_)));
                if full {
                    self.compose(t, None);
                } else if let Some(d) = dirty_units {
                    let (ox, oy) = self.origin(id, 0);
                    self.compose(t, Some(Rect::new(d.x + ox, d.y + oy, d.w, d.h)));
                } else {
                    self.compose(t, Some(Rect::EMPTY));
                }
            }
            _ => {
                // Not on the screen: answer its frame callbacks soon.
                let at = crate::time::uptime_ms() + 16;
                let frames = core::mem::take(&mut self.surfs.get_mut(&id).unwrap().frames);
                self.timers.extend(frames.into_iter().map(|cb| (at, cb)));
            }
        }
    }

    fn commit_toplevel(&mut self, id: u32, _changed: bool, _dirty: Option<Rect>) {
        let Some(Surface { role: Role::Top(t), mapped, .. }) = self.surfs.get(&id) else { return };
        if !t.xdg.sent {
            return self.reconfigure(id);
        }
        if t.win.is_none() && t.xdg.acked && *mapped {
            self.map_window(id);
        }
    }

    fn commit_popup(&mut self, id: u32) {
        let Some(Surface { role: Role::Popup(p), .. }) = self.surfs.get(&id) else { return };
        if !p.xdg.sent {
            self.configure_popup(id);
        }
    }

    /// Shows a toplevel on the desktop, the first time it has content.
    fn map_window(&mut self, id: u32) {
        let (w, h) = self.window_size(id);
        let title = match self.surfs.get(&id).map(|s| &s.role) {
            Some(Role::Top(t)) => t.title.clone(),
            _ => return,
        };
        let win = Win::new(title.clone(), w, h);
        if let Some(Surface { role: Role::Top(t), .. }) = self.surfs.get_mut(&id) {
            t.win = Some(win.clone());
        }
        self.update_resizable(id);
        // It is on the one screen there is.
        for (out, _) in self.objects_of(Iface::Output) {
            self.out.event(id, 0).object(out);
        }
        if self.version(id) >= 6 {
            self.out.event(id, 2).int(crate::gui::scale().max(1));
        }
        self.compose(id, None);
        crate::info!("wayland", "window \"{title}\" {w}x{h} of process {}", self.pid);
        window::open(win, self.inbox.clone(), id, self.icon.clone());
    }

    /// The content size of a toplevel: its window geometry, or its surface
    /// (without a Windows program's frame).
    fn window_size(&self, id: u32) -> (i32, i32) {
        let Some(s) = self.surfs.get(&id) else { return (1, 1) };
        let (w, h) = match (&s.role, s.xdg().and_then(|x| x.geometry)) {
            (Role::Top(Top { nc: Some(n), .. }), _) => {
                let (w, h) = s.size();
                (w - n[0] - n[2], h - n[1] - n[3])
            }
            (_, Some(g)) => (g.w, g.h),
            _ => s.size(),
        };
        (w.clamp(1, MAX_W), h.clamp(1, MAX_H))
    }

    /// Sends a toplevel the size it should have and its state.
    fn reconfigure(&mut self, id: u32) {
        let serial = self.next_serial();
        let (pw, ph) = crate::console::framebuffer().map_or((1024, 768), |f| (f.width as i32, f.height as i32));
        let k = crate::gui::scale().max(1);
        let Some(Surface { role: Role::Top(t), .. }) = self.surfs.get_mut(&id) else { return };
        t.xdg.sent = true;
        let (tid, xid, asked) = (t.id, t.xdg.id, t.asked);
        // Sizes are the window's; a Windows program's frame comes on top.
        let (ew, eh) = t.nc.map_or((0, 0), |n| (n[0] + n[2], n[1] + n[3]));
        let asked = if asked == (0, 0) { asked } else { (asked.0 + ew, asked.1 + eh) };
        let focused = t.win.as_ref().is_some_and(|w| w.lock().focused);
        let v = self.version(tid);
        if v >= 4 {
            self.out.event(tid, 2).int(pw / k - 2 * crate::gui::wm::GAP + ew).int(ph / k - crate::gui::wm::TITLE_H - 80 + eh);
        }
        if v >= 5 {
            // The window menu and minimising are the desktop's; maximise
            // (full screen) and minimise are there.
            let caps: Vec<u8> = [2u32, 4].iter().flat_map(|c| c.to_le_bytes()).collect();
            self.out.event(tid, 3).array(&caps);
        }
        let states: Vec<u8> = if focused { 4u32.to_le_bytes().to_vec() } else { Vec::new() };
        self.out.event(tid, 0).int(asked.0).int(asked.1).array(&states);
        self.out.event(xid, 0).uint(serial);
    }

    /// A decoration's mode applies with the next configure: sent now if
    /// the toplevel had its first one (else its initial commit sends it).
    fn reconfigure_shown(&mut self, id: u32) {
        if matches!(self.surfs.get(&id).map(|s| &s.role), Some(Role::Top(t)) if t.xdg.sent) {
            self.reconfigure(id);
        }
    }

    fn configure_popup(&mut self, id: u32) {
        let serial = self.next_serial();
        let Some(Surface { role: Role::Popup(p), .. }) = self.surfs.get_mut(&id) else { return };
        p.xdg.sent = true;
        let (pid, xid, r) = (p.id, p.xdg.id, p.rect);
        self.out.event(pid, 0).int(r.x).int(r.y).int(r.w).int(r.h);
        self.out.event(xid, 0).uint(serial);
    }

    // ------------------------------------------------------------------------
    // Where things are

    /// The toplevel a surface shows in (through subsurfaces and popups).
    fn root(&self, mut id: u32) -> Option<u32> {
        for _ in 0..32 {
            match &self.surfs.get(&id)?.role {
                Role::Top(_) => return Some(id),
                Role::Popup(p) => id = p.parent,
                Role::Sub(b) => id = b.parent,
                _ => return None,
            }
        }
        None
    }

    /// Where a surface's top-left corner is in its window (units; the
    /// window starts at the toplevel's window geometry).
    fn origin(&self, id: u32, depth: u32) -> (i32, i32) {
        let Some(s) = self.surfs.get(&id) else { return (0, 0) };
        if depth > 32 {
            return (0, 0);
        }
        let (gx, gy) = s.geometry_offset();
        match &s.role {
            Role::Top(_) => (-gx, -gy),
            Role::Popup(p) => {
                let (px, py) = self.geometry_origin(p.parent, depth + 1);
                (px + p.rect.x - gx, py + p.rect.y - gy)
            }
            Role::Sub(b) => {
                let (px, py) = self.origin(b.parent, depth + 1);
                (px + b.x, py + b.y)
            }
            _ => (0, 0),
        }
    }

    /// Where an xdg surface's window geometry starts in its window.
    fn geometry_origin(&self, id: u32, depth: u32) -> (i32, i32) {
        let (x, y) = self.origin(id, depth);
        let (gx, gy) = self.surfs.get(&id).map_or((0, 0), |s| s.client_geometry_offset());
        (x + gx, y + gy)
    }

    /// Where a popup goes, relative to its parent's window geometry: as its
    /// positioner says, then moved (flipped, slid, shrunk, as it allows) to
    /// stay inside the window.
    fn place(&self, parent: u32, p: &Positioner) -> Rect {
        let (ox, oy) = self.geometry_origin(parent, 0);
        let (ww, wh) = match self.root(parent).and_then(|t| self.surfs.get(&t)) {
            Some(Surface { role: Role::Top(t), .. }) => match &t.win {
                Some(w) => w.lock().area,
                None => self.window_size(parent),
            },
            _ => (MAX_W, MAX_H),
        };
        let bounds = Rect::new(-ox, -oy, ww, wh);
        place(p, bounds)
    }

    /// The surface at (x, y) of a window, and the point on it.
    fn hit(&self, t: u32, x: i32, y: i32) -> Option<(u32, i32, i32)> {
        if let Some(Surface { role: Role::Top(top), .. }) = self.surfs.get(&t) {
            for &p in top.popups.iter().rev() {
                if let Some(h) = self.hit_tree(p, x, y, 0) {
                    return Some(h);
                }
            }
        }
        self.hit_tree(t, x, y, 0)
    }

    fn hit_tree(&self, id: u32, x: i32, y: i32, depth: u32) -> Option<(u32, i32, i32)> {
        let s = self.surfs.get(&id)?;
        if depth > 32 {
            return None;
        }
        for &c in s.stack.iter().rev() {
            if c != id {
                if let Some(h) = self.hit_tree(c, x, y, depth + 1) {
                    return Some(h);
                }
                continue;
            }
            if !s.mapped {
                continue;
            }
            let (ox, oy) = self.origin(id, 0);
            let (lx, ly) = (x - ox, y - oy);
            let (w, h) = s.size();
            let inside = lx >= 0 && ly >= 0 && lx < w && ly < h && s.input.as_ref().is_none_or(|r| r.contains(lx, ly));
            if inside {
                return Some((id, lx, ly));
            }
        }
        None
    }

    // ------------------------------------------------------------------------
    // Composing a window

    /// Draws a window's surfaces into its frame: all of it, or the part
    /// `dirty` (window units) of it.
    fn compose(&mut self, t: u32, dirty: Option<Rect>) {
        let (w, h) = self.window_size(t);
        let Some(s @ Surface { role: Role::Top(top), .. }) = self.surfs.get(&t) else { return };
        let Some(win) = top.win.clone() else { return };
        let k = s.pixel_scale().clamp(1, gfx::MAX_SCALE);
        let popups = top.popups.clone();
        let mut win = win.lock();
        let mut dirty = dirty;
        if win.frame.w != w || win.frame.h != h || win.frame.scale() != k {
            win.frame = Canvas::new_layer_at(w, h, k);
            dirty = None;
        }
        win.size = (w, h);
        let full = Rect::new(0, 0, win.frame.pixel_width(), win.frame.pixel_height());
        let clip = dirty.map_or(full, |d| Rect::new(d.x * k, d.y * k, d.w * k, d.h * k).intersect(&full));
        if !clip.is_empty() {
            let pw = win.frame.pixel_width();
            for y in clip.y..clip.bottom() {
                let row = (y * pw) as usize;
                win.frame.px[row + clip.x as usize..row + clip.right() as usize].fill(0);
            }
            self.paint(t, &mut win.frame, clip, 0);
            for p in popups {
                self.paint(p, &mut win.frame, clip, 0);
            }
        }
        win.generation += 1;
    }

    /// Draws a surface and its subsurfaces, in their order.
    fn paint(&self, id: u32, frame: &mut Canvas, clip: Rect, depth: u32) {
        let Some(s) = self.surfs.get(&id) else { return };
        if depth > 32 {
            return;
        }
        for &c in &s.stack {
            if c != id {
                self.paint(c, frame, clip, depth + 1);
            } else if s.mapped {
                let (x, y) = self.origin(id, 0);
                blit(frame, clip, s, x, y);
            }
        }
    }

    // ------------------------------------------------------------------------
    // Input

    fn input(&mut self, t: u32, ev: Input) {
        let time = now();
        match ev {
            Input::Motion(x, y) => {
                let hit = self.hit(t, x, y);
                let serial = self.next_serial();
                let pointers = self.objects_of(Iface::Pointer);
                match (self.pointer, hit) {
                    (Some((old, _)), Some((s, lx, ly))) if old == s => {
                        for &(p, _) in &pointers {
                            self.out.event(p, 2).uint(time).fixed(lx).fixed(ly);
                        }
                    }
                    (old, Some((s, lx, ly))) => {
                        for &(p, _) in &pointers {
                            if let Some((o, _)) = old {
                                self.out.event(p, 1).uint(serial).object(o);
                            }
                            self.out.event(p, 0).uint(serial).object(s).fixed(lx).fixed(ly);
                        }
                        self.pointer = Some((s, t));
                    }
                    (Some((o, _)), None) => {
                        for &(p, _) in &pointers {
                            self.out.event(p, 1).uint(serial).object(o);
                        }
                        self.pointer = None;
                    }
                    (None, None) => {}
                }
                self.pointer_frame(&pointers);
            }
            Input::Leave => {
                if let Some((o, _)) = self.pointer.take() {
                    let serial = self.next_serial();
                    let pointers = self.objects_of(Iface::Pointer);
                    for &(p, _) in &pointers {
                        self.out.event(p, 1).uint(serial).object(o);
                    }
                    self.pointer_frame(&pointers);
                }
            }
            Input::Button(b, down) => {
                let in_popup = self.pointer.is_some_and(|(s, _)| self.in_popup(s));
                if down && !in_popup && self.grabbing(t) {
                    // A click beside an open menu closes it.
                    self.dismiss_popups(t);
                    return;
                }
                if self.pointer.is_none() {
                    return;
                }
                let serial = self.next_serial();
                let pointers = self.objects_of(Iface::Pointer);
                for &(p, _) in &pointers {
                    self.out.event(p, 3).uint(serial).uint(time).uint(b).uint(down as u32);
                }
                self.pointer_frame(&pointers);
            }
            Input::Scroll(lines) => {
                if self.pointer.is_none() {
                    return;
                }
                let pointers = self.objects_of(Iface::Pointer);
                for &(p, v) in &pointers {
                    if v >= 5 {
                        self.out.event(p, 6).uint(0); // a wheel
                        self.out.event(p, 8).uint(0).int(lines);
                    }
                    self.out.event(p, 4).uint(time).uint(0).fixed(lines * 10);
                }
                self.pointer_frame(&pointers);
            }
            Input::Key(k) => self.key(t, k, time),
            Input::Focus(on) => {
                let target = if on { Some(self.keyboard_target(t)) } else { None };
                self.focus_keyboard(target);
                if let Some(Surface { role: Role::Top(top), .. }) = self.surfs.get(&t) {
                    if top.xdg.sent {
                        self.reconfigure(t);
                    }
                }
            }
            Input::Resize(w, h) => {
                let Some(Surface { role: Role::Top(top), .. }) = self.surfs.get_mut(&t) else { return };
                top.asked = (w, h);
                if self.window_size(t) != (w, h) {
                    self.reconfigure(t);
                }
                // Popups may need to move to stay inside.
                self.compose(t, None);
            }
            Input::Drawn => self.drawn(t),
            Input::Close => {
                if let Some(Surface { role: Role::Top(top), .. }) = self.surfs.get(&t) {
                    let id = top.id;
                    self.out.event(id, 1);
                }
            }
        }
    }

    fn pointer_frame(&mut self, pointers: &[(u32, u32)]) {
        for &(p, v) in pointers {
            if v >= 5 {
                self.out.event(p, 5);
            }
        }
    }

    fn in_popup(&self, mut s: u32) -> bool {
        for _ in 0..32 {
            match self.surfs.get(&s).map(|x| &x.role) {
                Some(Role::Popup(_)) => return true,
                Some(Role::Sub(b)) => s = b.parent,
                _ => return false,
            }
        }
        false
    }

    /// Does a menu of window `t` hold the pointer and keyboard?
    fn grabbing(&self, t: u32) -> bool {
        match self.surfs.get(&t).map(|s| &s.role) {
            Some(Role::Top(top)) => top.popups.iter().any(|p| matches!(self.surfs.get(p).map(|s| &s.role), Some(Role::Popup(pp)) if pp.grab)),
            _ => false,
        }
    }

    /// Tells every popup of window `t` to go, the top one first.
    fn dismiss_popups(&mut self, t: u32) {
        let popups = match self.surfs.get(&t).map(|s| &s.role) {
            Some(Role::Top(top)) => top.popups.clone(),
            _ => return,
        };
        for p in popups.into_iter().rev() {
            if let Some(Surface { role: Role::Popup(pp), .. }) = self.surfs.get(&p) {
                let id = pp.id;
                self.out.event(id, 1);
            }
        }
    }

    /// Where keys go in window `t`: its top grabbing menu, or itself.
    fn keyboard_target(&self, t: u32) -> u32 {
        if let Some(Surface { role: Role::Top(top), .. }) = self.surfs.get(&t) {
            for &p in top.popups.iter().rev() {
                if matches!(self.surfs.get(&p).map(|s| &s.role), Some(Role::Popup(pp)) if pp.grab) {
                    return p;
                }
            }
        }
        t
    }

    fn focus_keyboard(&mut self, to: Option<u32>) {
        if self.keyboard == to {
            return;
        }
        let serial = self.next_serial();
        let keyboards = self.objects_of(Iface::Keyboard);
        if let Some(old) = self.keyboard.take() {
            for &(k, _) in &keyboards {
                self.out.event(k, 2).uint(serial).object(old);
            }
        }
        if let Some(new) = to {
            let (ctrl, alt, shift) = crate::dev::input::mods();
            self.mods = (shift as u32 * keymap::SHIFT) | (ctrl as u32 * keymap::CTRL) | (alt as u32 * keymap::ALT);
            for &(k, _) in &keyboards {
                self.out.event(k, 1).uint(serial).object(new).array(&[]);
                self.out.event(k, 4).uint(serial).uint(self.mods).uint(0).uint(0).uint(0);
            }
            self.keyboard = Some(new);
        }
    }

    fn set_mods(&mut self, mods: u32, keyboards: &[(u32, u32)]) {
        if mods == self.mods {
            return;
        }
        self.mods = mods;
        let serial = self.next_serial();
        for &(k, _) in keyboards {
            self.out.event(k, 4).uint(serial).uint(mods).uint(0).uint(0).uint(0);
        }
    }

    fn key(&mut self, t: u32, ev: crate::dev::input::KeyEvent, time: u32) {
        let Some((press, grew)) = self.keymap.press(ev.key) else { return };
        let target = self.keyboard_target(t);
        self.focus_keyboard(Some(target));
        let keyboards = self.objects_of(Iface::Keyboard);
        if grew {
            for &(k, _) in &keyboards {
                self.send_keymap(k);
            }
        }
        let shift = if matches!(ev.key, crate::dev::input::Key::Char(_)) { press.shift } else { ev.shift };
        let mods = (shift as u32 * keymap::SHIFT) | (ev.ctrl as u32 * keymap::CTRL) | (ev.alt as u32 * keymap::ALT);
        // The modifier keys go down before the key and up after it, as on a
        // keyboard: programs that keep a key state of their own from key
        // events (Windows programs, through Wine) see Shift held too.
        let modkeys: Vec<u32> = [(keymap::SHIFT, keymap::KEY_SHIFT), (keymap::CTRL, keymap::KEY_CTRL), (keymap::ALT, keymap::KEY_ALT)]
            .iter()
            .filter(|&&(m, _)| mods & m != 0)
            .map(|&(_, key)| key - 8)
            .collect();
        let send = |this: &mut Self, code: u32, state: u32| {
            let serial = this.next_serial();
            for &(k, _) in &keyboards {
                this.out.event(k, 3).uint(serial).uint(time).uint(code).uint(state);
            }
        };
        for &m in &modkeys {
            send(self, m, 1);
        }
        self.set_mods(mods, &keyboards);
        let code = press.code - 8;
        for state in [1u32, 0] {
            send(self, code, state);
        }
        for &m in modkeys.iter().rev() {
            send(self, m, 0);
        }
        // Back to what is held now.
        let (ctrl, alt, shift) = crate::dev::input::mods();
        let held = (shift as u32 * keymap::SHIFT) | (ctrl as u32 * keymap::CTRL) | (alt as u32 * keymap::ALT);
        self.set_mods(held, &keyboards);
    }

    /// The desktop showed window `t`: its frame callbacks are answered.
    fn drawn(&mut self, t: u32) {
        let ids: Vec<u32> = self.surfs.keys().copied().filter(|&s| self.root(s) == Some(t)).collect();
        let time = now();
        for s in ids {
            let frames = core::mem::take(&mut self.surfs.get_mut(&s).unwrap().frames);
            for cb in frames {
                self.done(cb, time);
            }
        }
    }

    fn done(&mut self, cb: u32, time: u32) {
        if self.objs.get(&cb).is_some_and(|o| o.iface == Iface::Callback) {
            self.out.event(cb, 0).uint(time);
            self.destroyed(cb);
        }
    }

    fn fire_timers(&mut self) {
        let t = crate::time::uptime_ms();
        let due: Vec<u32> = self.timers.iter().filter(|x| x.0 <= t).map(|x| x.1).collect();
        self.timers.retain(|x| x.0 > t);
        let time = now();
        for cb in due {
            self.done(cb, time);
        }
    }
}

/// Copies a buffer into a surface's content: what `damage` covers (buffer
/// pixels), or all of it when its size changed. True when it was all.
fn import(s: &mut Surface, b: &Buf, damage: Rect) -> bool {
    let Some(shm) = &b.shm else { return false };
    let full = s.pw != b.w || s.ph != b.h || s.px.len() != (b.w * b.h) as usize;
    if full {
        s.px = alloc::vec![0; (b.w * b.h) as usize];
        (s.pw, s.ph) = (b.w, b.h);
    }
    let all = Rect::new(0, 0, b.w, b.h);
    let r = if full { all } else { damage.intersect(&all) };
    if r.is_empty() {
        return full;
    }
    let mut row = alloc::vec![0u8; r.w as usize * 4];
    for y in r.y..r.bottom() {
        let at = b.off + y as u64 * b.stride as u64 + r.x as u64 * 4;
        let n = shm.read_at(at, &mut row);
        row[n..].fill(0);
        let dst = &mut s.px[(y * b.w + r.x) as usize..(y * b.w + r.right()) as usize];
        for (d, p) in dst.iter_mut().zip(row.as_chunks::<4>().0) {
            let v = u32::from_le_bytes(*p);
            *d = if b.format == FORMAT_XRGB { v | 0xff00_0000 } else { unpremultiply(v) };
        }
    }
    full
}

/// The frame a Windows program draws around a window, as Wine does under
/// Wayland (it has no other way): borders, and a title bar in the colours
/// justOS's Wine settings give it (tools/linux/winelook.py), active and
/// not. Found on the content, it is left out of the window: the desktop
/// frames it, with its own title and buttons. Returns its edges in units
/// (left, top, right, bottom), None when there is none: windows without a
/// title bar, or programs that set a window geometry of their own (one
/// that is all of the surface, as Wine's, is none).
fn windows_frame(s: &Surface) -> Option<[i32; 4]> {
    const ACTIVE: u32 = 0x1b_1a1e;
    const INACTIVE: u32 = 0x1b_1a1d;
    if let Some(g) = s.xdg().and_then(|x| x.geometry) {
        if (g.x, g.y) != (0, 0) || (g.w, g.h) != s.size() {
            return None;
        }
    }
    // Only the part of the content that is shown (Wine's buffers are larger
    // than its windows, a viewport source cutting them).
    let (x0, y0, w, h) = s.source_px();
    let title = |x: i32, y: i32| {
        x >= 0
            && y >= 0
            && x < w
            && y < h
            && s.px.get(((y0 + y) * s.pw + x0 + x) as usize).is_some_and(|&v| matches!(v & 0xff_ffff, ACTIVE | INACTIVE))
    };
    // The border is as wide on every side; the title bar's two ends (the
    // icon is a little inside the left one, the buttons inside the right).
    let b = (0..12).find(|&b| title(b, b + 1) && title(w - 1 - b, b + 1))?;
    let r = w - 1 - b;
    let mut bottom = b;
    while bottom < h && title(r, bottom) {
        bottom += 1;
    }
    if !(8..=96).contains(&(bottom - b)) || bottom + b >= h {
        return None;
    }
    let (uw, _) = s.size();
    let units = |v: i32| (v as i64 * uw as i64 / w.max(1) as i64) as i32;
    Some([units(b), units(bottom), units(b), units(b)])
}

/// Wayland's ARGB is premultiplied; justOS's pictures are not.
fn unpremultiply(v: u32) -> u32 {
    let a = v >> 24;
    if a == 255 || a == 0 {
        return if a == 0 { 0 } else { v };
    }
    let ch = |c: u32| ((c * 255 + a / 2) / a).min(255);
    (a << 24) | (ch((v >> 16) & 0xff) << 16) | (ch((v >> 8) & 0xff) << 8) | ch(v & 0xff)
}

/// Draws a surface's content at (x, y) (units) of a frame, inside `clip`
/// (pixels of the frame).
fn blit(frame: &mut Canvas, clip: Rect, s: &Surface, x: i32, y: i32) {
    let k = frame.scale();
    let ss = s.scale.max(1);
    let (w, h) = s.size();
    let dst = Rect::new(x * k, y * k, w * k, h * k).intersect(&clip);
    if dst.is_empty() {
        return;
    }
    let fw = frame.pixel_width();
    if s.viewported() {
        // Cropped and scaled: each pixel of the frame from the nearest one
        // of the part shown.
        let (sx0, sy0, sw, sh) = s.source_px();
        let (dw, dh) = ((w * k).max(1) as i64, (h * k).max(1) as i64);
        for dy in dst.y..dst.bottom() {
            let sy = sy0 + ((dy - y * k) as i64 * sh as i64 / dh) as i32;
            if sy < 0 || sy >= s.ph {
                continue;
            }
            let src_row = (sy * s.pw) as usize;
            let dst_row = (dy * fw) as usize;
            for dx in dst.x..dst.right() {
                let sx = sx0 + ((dx - x * k) as i64 * sw as i64 / dw) as i32;
                if sx < 0 || sx >= s.pw {
                    continue;
                }
                let p = s.px[src_row + sx as usize];
                let a = p >> 24;
                let d = &mut frame.px[dst_row + dx as usize];
                if a == 255 {
                    *d = p;
                } else if a != 0 {
                    *d = gfx::over(*d, p, a);
                }
            }
        }
        return;
    }
    if ss == k {
        // Pixel for pixel: whole rows.
        for dy in dst.y..dst.bottom() {
            let at = ((dy - y * k) * s.pw + dst.x - x * k) as usize;
            let Some(src) = s.px.get(at..at + dst.w as usize) else { continue };
            let row = (dy * fw + dst.x) as usize;
            for (d, &p) in frame.px[row..row + dst.w as usize].iter_mut().zip(src) {
                let a = p >> 24;
                if a == 255 {
                    *d = p;
                } else if a != 0 {
                    *d = gfx::over(*d, p, a);
                }
            }
        }
        return;
    }
    for dy in dst.y..dst.bottom() {
        let sy = (dy - y * k) * ss / k;
        if sy < 0 || sy >= s.ph {
            continue;
        }
        let src_row = (sy * s.pw) as usize;
        let dst_row = (dy * fw) as usize;
        for dx in dst.x..dst.right() {
            let sx = (dx - x * k) * ss / k;
            if sx < 0 || sx >= s.pw {
                continue;
            }
            let p = s.px[src_row + sx as usize];
            let a = p >> 24;
            let d = &mut frame.px[dst_row + dx as usize];
            if a == 255 {
                *d = p;
            } else if a != 0 {
                *d = gfx::over(*d, p, a);
            }
        }
    }
}

/// xdg_positioner's anchors and gravities: none, top, bottom, left, right,
/// top left, bottom left, top right, bottom right.
fn edge_point(r: Rect, e: u32) -> (i32, i32) {
    let x = match e {
        3 | 5 | 6 => r.x,
        4 | 7 | 8 => r.right(),
        _ => r.x + r.w / 2,
    };
    let y = match e {
        1 | 5 | 7 => r.y,
        2 | 6 | 8 => r.bottom(),
        _ => r.y + r.h / 2,
    };
    (x, y)
}

fn flip_x(e: u32) -> u32 {
    match e {
        3 => 4,
        4 => 3,
        5 => 7,
        7 => 5,
        6 => 8,
        8 => 6,
        e => e,
    }
}

fn flip_y(e: u32) -> u32 {
    match e {
        1 => 2,
        2 => 1,
        5 => 6,
        6 => 5,
        7 => 8,
        8 => 7,
        e => e,
    }
}

fn place_at(p: &Positioner, edge: u32, gravity: u32, off: (i32, i32)) -> Rect {
    let (w, h) = (p.w.max(1), p.h.max(1));
    let (ax, ay) = edge_point(p.anchor, edge);
    let x = match gravity {
        3 | 5 | 6 => ax - w,
        4 | 7 | 8 => ax,
        _ => ax - w / 2,
    };
    let y = match gravity {
        1 | 5 | 7 => ay - h,
        2 | 6 | 8 => ay,
        _ => ay - h / 2,
    };
    Rect::new(x + off.0, y + off.1, w, h)
}

fn place(p: &Positioner, b: Rect) -> Rect {
    const SLIDE_X: u32 = 1;
    const SLIDE_Y: u32 = 2;
    const FLIP_X: u32 = 4;
    const FLIP_Y: u32 = 8;
    const RESIZE_X: u32 = 16;
    const RESIZE_Y: u32 = 32;
    let (edge, gravity, off) = (p.edge, p.gravity, p.off);
    let mut r = place_at(p, edge, gravity, off);
    let out_x = |r: &Rect| r.x < b.x || r.right() > b.right();
    let out_y = |r: &Rect| r.y < b.y || r.bottom() > b.bottom();
    if p.adjust & FLIP_X != 0 && out_x(&r) {
        let f = place_at(p, flip_x(edge), flip_x(gravity), (-off.0, off.1));
        if !out_x(&f) {
            r.x = f.x;
        }
    }
    if p.adjust & FLIP_Y != 0 && out_y(&r) {
        let f = place_at(p, flip_y(edge), flip_y(gravity), (off.0, -off.1));
        if !out_y(&f) {
            r.y = f.y;
        }
    }
    if p.adjust & SLIDE_X != 0 && out_x(&r) {
        r.x = r.x.min(b.right() - r.w).max(b.x);
    }
    if p.adjust & SLIDE_Y != 0 && out_y(&r) {
        r.y = r.y.min(b.bottom() - r.h).max(b.y);
    }
    if p.adjust & RESIZE_X != 0 && out_x(&r) {
        let x0 = r.x.max(b.x);
        r.w = (r.right().min(b.right()) - x0).max(1);
        r.x = x0;
    }
    if p.adjust & RESIZE_Y != 0 && out_y(&r) {
        let y0 = r.y.max(b.y);
        r.h = (r.bottom().min(b.bottom()) - y0).max(1);
        r.y = y0;
    }
    r
}
