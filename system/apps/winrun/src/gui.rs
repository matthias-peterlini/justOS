//! Windows with a message loop: a part of `user32` and `gdi32` on top of the
//! arcOS windows of [`arcui`].
//!
//! Each Win32 window is an arcOS window whose canvas keeps what the program
//! drew, as a Windows window does. Mouse, keyboard, resizing and closing come
//! back from arcOS as events and become `WM_*` messages; `GetMessage` merges
//! them with posted messages, timers and `WM_PAINT`. A device context draws
//! with GDI's pen, brush and font onto the window or onto a bitmap (for the
//! usual double buffering with `BitBlt`).
//!
//! Child controls (buttons, edit boxes), menus and dialog resources are not
//! there: a program that builds its whole window from them shows an empty one.

// The Windows structures keep their Windows names.
#![allow(non_snake_case, static_mut_refs, clippy::upper_case_acronyms)]

use crate::crt::{cstr, wstr};
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use arcui::{ui, Canvas, Event, Font, Key, Rect, Window};

pub fn resolve(name: &str) -> Option<u64> {
    let f: u64 = match name {
        // user32: classes and windows
        "RegisterClassA" | "RegisterClassW" => RegisterClass as *const () as u64,
        "RegisterClassExA" => RegisterClassExA as *const () as u64,
        "RegisterClassExW" => RegisterClassExW as *const () as u64,
        "UnregisterClassA" | "UnregisterClassW" => ret1 as *const () as u64,
        "CreateWindowExA" => CreateWindowExA as *const () as u64,
        "CreateWindowExW" => CreateWindowExW as *const () as u64,
        "DestroyWindow" => DestroyWindow as *const () as u64,
        "ShowWindow" => ShowWindow as *const () as u64,
        "UpdateWindow" => UpdateWindow as *const () as u64,
        "DefWindowProcA" | "DefWindowProcW" => DefWindowProc as *const () as u64,
        "CallWindowProcA" | "CallWindowProcW" => CallWindowProc as *const () as u64,
        "SetWindowTextA" => SetWindowTextA as *const () as u64,
        "SetWindowTextW" => SetWindowTextW as *const () as u64,
        "GetClientRect" => GetClientRect as *const () as u64,
        "GetWindowRect" => GetWindowRect as *const () as u64,
        "AdjustWindowRect" => AdjustWindowRect as *const () as u64,
        "AdjustWindowRectEx" => AdjustWindowRect as *const () as u64,
        "GetWindowLongPtrA" | "GetWindowLongPtrW" | "GetWindowLongA" | "GetWindowLongW" => GetWindowLongPtr as *const () as u64,
        "SetWindowLongPtrA" | "SetWindowLongPtrW" | "SetWindowLongA" | "SetWindowLongW" => SetWindowLongPtr as *const () as u64,
        "IsWindow" => IsWindow as *const () as u64,
        "IsWindowVisible" => IsWindow as *const () as u64,
        "MoveWindow" | "SetWindowPos" | "SetForegroundWindow" | "BringWindowToTop" | "EnableWindow" => ret1 as *const () as u64,
        "SetFocus" | "GetFocus" | "GetActiveWindow" | "GetForegroundWindow" => GetFocus as *const () as u64,
        "SetCapture" | "ReleaseCapture" | "GetDesktopWindow" | "GetParent" => ret0 as *const () as u64,
        "GetSystemMetrics" => GetSystemMetrics as *const () as u64,
        "LoadCursorA" | "LoadCursorW" | "LoadIconA" | "LoadIconW" | "LoadImageA" | "LoadImageW" => ret1 as *const () as u64,
        "SetCursor" | "ShowCursor" | "DestroyIcon" | "DestroyCursor" | "LoadMenuA" | "LoadMenuW" | "SetMenu" => ret0 as *const () as u64,
        "GetCursorPos" => GetCursorPos as *const () as u64,
        "ScreenToClient" | "ClientToScreen" => ret1 as *const () as u64,
        "GetAsyncKeyState" | "GetKeyState" => GetKeyState as *const () as u64,
        "MessageBoxA" => MessageBoxA as *const () as u64,
        "MessageBoxW" => MessageBoxW as *const () as u64,
        "MessageBeep" => ret1 as *const () as u64,
        "SetProcessDPIAware" | "SetProcessDpiAwarenessContext" => ret1 as *const () as u64,
        // user32: messages and timers
        "GetMessageA" | "GetMessageW" => GetMessage as *const () as u64,
        "PeekMessageA" | "PeekMessageW" => PeekMessage as *const () as u64,
        "TranslateMessage" => ret0 as *const () as u64,
        "DispatchMessageA" | "DispatchMessageW" => DispatchMessage as *const () as u64,
        "PostMessageA" | "PostMessageW" => PostMessage as *const () as u64,
        "SendMessageA" | "SendMessageW" => SendMessage as *const () as u64,
        "PostQuitMessage" => PostQuitMessage as *const () as u64,
        "WaitMessage" => WaitMessage as *const () as u64,
        "SetTimer" => SetTimer as *const () as u64,
        "KillTimer" => KillTimer as *const () as u64,
        "InvalidateRect" | "InvalidateRgn" => InvalidateRect as *const () as u64,
        "ValidateRect" => ValidateRect as *const () as u64,
        "RedrawWindow" => InvalidateRect as *const () as u64,
        // user32 / gdi32: painting
        "BeginPaint" => BeginPaint as *const () as u64,
        "EndPaint" => EndPaint as *const () as u64,
        "GetDC" | "GetWindowDC" => GetDC as *const () as u64,
        "ReleaseDC" => ReleaseDC as *const () as u64,
        "FillRect" => FillRect as *const () as u64,
        "FrameRect" => FrameRect as *const () as u64,
        "DrawTextA" => DrawTextA as *const () as u64,
        "DrawTextW" => DrawTextW as *const () as u64,
        "GetSysColor" => GetSysColor as *const () as u64,
        "GetSysColorBrush" => GetSysColorBrush as *const () as u64,
        "TextOutA" => TextOutA as *const () as u64,
        "TextOutW" => TextOutW as *const () as u64,
        "GetTextExtentPoint32A" | "GetTextExtentPointA" => GetTextExtentPoint32A as *const () as u64,
        "GetTextExtentPoint32W" | "GetTextExtentPointW" => GetTextExtentPoint32W as *const () as u64,
        "GetTextMetricsA" | "GetTextMetricsW" => GetTextMetrics as *const () as u64,
        "SetTextColor" => SetTextColor as *const () as u64,
        "SetBkColor" => SetBkColor as *const () as u64,
        "SetBkMode" => SetBkMode as *const () as u64,
        "SetTextAlign" => ret0 as *const () as u64,
        "SetDCBrushColor" => SetDCBrushColor as *const () as u64,
        "SetDCPenColor" => SetDCPenColor as *const () as u64,
        "GetStockObject" => GetStockObject as *const () as u64,
        "CreateSolidBrush" => CreateSolidBrush as *const () as u64,
        "CreatePen" => CreatePen as *const () as u64,
        "CreateFontA" | "CreateFontW" => CreateFont as *const () as u64,
        "CreateFontIndirectA" | "CreateFontIndirectW" => CreateFontIndirect as *const () as u64,
        "CreateCompatibleDC" => CreateCompatibleDC as *const () as u64,
        "CreateCompatibleBitmap" => CreateCompatibleBitmap as *const () as u64,
        "DeleteDC" => ret1 as *const () as u64,
        "SelectObject" => SelectObject as *const () as u64,
        "DeleteObject" => DeleteObject as *const () as u64,
        "BitBlt" => BitBlt as *const () as u64,
        "StretchBlt" => StretchBlt as *const () as u64,
        "Rectangle" => Rectangle as *const () as u64,
        "RoundRect" => RoundRect as *const () as u64,
        "Ellipse" => Ellipse as *const () as u64,
        "MoveToEx" => MoveToEx as *const () as u64,
        "LineTo" => LineTo as *const () as u64,
        "Polyline" => Polyline as *const () as u64,
        "Polygon" => Polygon as *const () as u64,
        "SetPixel" | "SetPixelV" => SetPixel as *const () as u64,
        "GetPixel" => GetPixel as *const () as u64,
        "GdiFlush" => ret1 as *const () as u64,
        _ => return None,
    };
    Some(f)
}

extern "win64" fn ret0() -> u64 {
    0
}
extern "win64" fn ret1() -> u64 {
    1
}

// ----------------------------------------------------------------------------
// Messages

const WM_CREATE: u32 = 0x0001;
const WM_DESTROY: u32 = 0x0002;
const WM_SIZE: u32 = 0x0005;
const WM_SETFOCUS: u32 = 0x0007;
const WM_PAINT: u32 = 0x000F;
const WM_CLOSE: u32 = 0x0010;
const WM_QUIT: u32 = 0x0012;
const WM_ERASEBKGND: u32 = 0x0014;
const WM_SHOWWINDOW: u32 = 0x0018;
const WM_KEYDOWN: u32 = 0x0100;
const WM_KEYUP: u32 = 0x0101;
const WM_CHAR: u32 = 0x0102;
const WM_TIMER: u32 = 0x0113;
const WM_MOUSEMOVE: u32 = 0x0200;
const WM_LBUTTONDOWN: u32 = 0x0201;
const WM_LBUTTONUP: u32 = 0x0202;
const WM_LBUTTONDBLCLK: u32 = 0x0203;
const WM_RBUTTONDOWN: u32 = 0x0204;
const WM_RBUTTONUP: u32 = 0x0205;
const WM_MOUSEWHEEL: u32 = 0x020A;

#[derive(Clone, Copy)]
struct Msg {
    hwnd: u64,
    msg: u32,
    wp: u64,
    lp: i64,
}

/// The `MSG` structure.
#[repr(C)]
struct MSG {
    hwnd: u64,
    message: u32,
    wparam: u64,
    lparam: i64,
    time: u32,
    pt: [i32; 2],
    private: u32,
}

// ----------------------------------------------------------------------------
// State (winrun is single-threaded)

struct Class {
    name: String,
    atom: u16,
    proc_: u64,
    brush: u64,
    style: u32,
}

struct Win {
    window: Window,
    proc_: u64,
    brush: u64,
    style: u32,
    dblclk: bool,
    dirty: bool,
    erase: bool,
    user: u64,
    /// When the left button last went down, for double clicks.
    mouse: (i32, i32),
}

enum Obj {
    /// A brush; `None` is the hollow brush.
    Brush(Option<u32>),
    Pen(Option<u32>, i32),
    Font(Font),
    Bitmap(Canvas),
    Free,
}

#[derive(Clone, Copy)]
struct Dc {
    /// The window it draws on, or (for a memory DC) none.
    hwnd: u64,
    bitmap: u64,
    brush: u64,
    pen: u64,
    font: u64,
    text: u32,
    bk: u32,
    opaque: bool,
    x: i32,
    y: i32,
    dc_brush: u32,
    dc_pen: u32,
}

struct Timer {
    hwnd: u64,
    id: u64,
    ms: u64,
    due: u64,
    proc_: u64,
}

struct State {
    classes: Vec<Class>,
    wins: Vec<Option<Win>>,
    objs: Vec<Obj>,
    dcs: Vec<Dc>,
    queue: VecDeque<Msg>,
    timers: Vec<Timer>,
    quit: Option<i32>,
    /// When each virtual key last went down (ms), for GetKeyState.
    keys: [u64; 256],
    pointer: (i32, i32),
    next_timer: u64,
    ever_had_window: bool,
}

static mut STATE: Option<State> = None;

fn st() -> &'static mut State {
    // SAFETY: winrun is single-threaded, and no reference is kept across a
    // call into the program.
    unsafe {
        STATE.get_or_insert_with(|| {
            let mut objs = Vec::new();
            // The stock objects, at their own numbers.
            for n in 0..=19 {
                objs.push(match n {
                    0 => Obj::Brush(Some(0x00FF_FFFF)),
                    1 => Obj::Brush(Some(0x00C0_C0C0)),
                    2 => Obj::Brush(Some(0x0080_8080)),
                    3 => Obj::Brush(Some(0x0040_4040)),
                    4 => Obj::Brush(Some(0)),
                    5 => Obj::Brush(None),
                    6 => Obj::Pen(Some(0x00FF_FFFF), 1),
                    7 => Obj::Pen(Some(0), 1),
                    8 => Obj::Pen(None, 1),
                    11 => Obj::Font(Font::Mono),
                    18 => Obj::Brush(Some(0x00FF_FFFF)), // DC_BRUSH (see Dc::dc_brush)
                    19 => Obj::Pen(Some(0), 1),          // DC_PEN
                    _ => Obj::Font(Font::Body),
                });
            }
            State {
                classes: Vec::new(),
                wins: Vec::new(),
                objs,
                dcs: Vec::new(),
                queue: VecDeque::new(),
                timers: Vec::new(),
                quit: None,
                keys: [0; 256],
                pointer: (0, 0),
                next_timer: 0x7000,
                ever_had_window: false,
            }
        })
    }
}

const HWND_BASE: u64 = 0x0001_0000;
const HDC_BASE: u64 = 0x0002_0000;
const OBJ_BASE: u64 = 0x0003_0000;
const DC_BRUSH: u64 = OBJ_BASE + 18;
const DC_PEN: u64 = OBJ_BASE + 19;

fn win(hwnd: u64) -> Option<&'static mut Win> {
    let i = hwnd.checked_sub(HWND_BASE)? as usize;
    st().wins.get_mut(i)?.as_mut()
}

fn dc(hdc: u64) -> Option<&'static mut Dc> {
    let i = hdc.checked_sub(HDC_BASE)? as usize;
    st().dcs.get_mut(i)
}

fn obj(h: u64) -> Option<&'static mut Obj> {
    let i = h.checked_sub(OBJ_BASE)? as usize;
    st().objs.get_mut(i)
}

fn new_obj(o: Obj) -> u64 {
    let objs = &mut st().objs;
    // Reuse a freed slot (past the stock objects).
    if let Some(i) = objs.iter().skip(20).position(|o| matches!(o, Obj::Free)) {
        objs[i + 20] = o;
        return OBJ_BASE + (i + 20) as u64;
    }
    objs.push(o);
    OBJ_BASE + (objs.len() - 1) as u64
}

fn new_dc(hwnd: u64) -> u64 {
    let d = Dc {
        hwnd,
        bitmap: 0,
        brush: OBJ_BASE,
        pen: OBJ_BASE + 7,
        font: OBJ_BASE + 13,
        text: 0,
        bk: 0x00FF_FFFF,
        opaque: true,
        x: 0,
        y: 0,
        dc_brush: 0x00FF_FFFF,
        dc_pen: 0,
    };
    let dcs = &mut st().dcs;
    dcs.push(d);
    HDC_BASE + (dcs.len() - 1) as u64
}

/// A COLORREF (0x00BBGGRR) as an opaque canvas colour.
fn rgb(c: u32) -> u32 {
    0xFF00_0000 | (c & 0xFF) << 16 | (c & 0xFF00) | (c >> 16 & 0xFF)
}

/// The system colours (`COLOR_*`), as COLORREFs.
fn sys_color(i: u32) -> u32 {
    match i {
        0 | 4 | 5 => 0x00FF_FFFF,          // scrollbar, menu, window
        1..=3 => 0x00A0_6000,              // desktop, captions
        6..=9 | 18 => 0x0000_0000,         // frame, menu text, window text
        13 => 0x00D7_7800,                 // highlight
        14 => 0x00FF_FFFF,                 // highlight text
        16 => 0x00A0_A0A0,                 // shadow
        17 => 0x006D_6D6D,                 // grey text
        _ => 0x00F0_F0F0,                  // 3D face
    }
}

/// The colour a brush handle paints with: a brush object or `COLOR_x + 1`.
fn brush_color(h: u64, d: Option<&Dc>) -> Option<u32> {
    if h == 0 {
        return None;
    }
    if h < 0x100 {
        return Some(sys_color(h as u32 - 1));
    }
    if h == DC_BRUSH {
        return Some(d.map_or(0x00FF_FFFF, |d| d.dc_brush));
    }
    match obj(h)? {
        Obj::Brush(c) => *c,
        _ => None,
    }
}

/// Runs `f` on the canvas a DC draws on (its window's, or its bitmap).
fn with_canvas<T>(hdc: u64, f: impl FnOnce(&mut Canvas, &Dc) -> T) -> Option<T> {
    let d = *dc(hdc)?;
    if d.bitmap != 0 {
        if let Some(Obj::Bitmap(c)) = obj(d.bitmap) {
            return Some(f(c, &d));
        }
        return None;
    }
    let w = win(d.hwnd)?;
    Some(f(&mut w.window.canvas, &d))
}

fn pen_of(d: &Dc) -> Option<(u32, i32)> {
    if d.pen == DC_PEN {
        return Some((rgb(d.dc_pen), 1));
    }
    match obj(d.pen)? {
        Obj::Pen(Some(c), w) => Some((rgb(*c), (*w).max(1))),
        _ => None,
    }
}

fn brush_of(d: &Dc) -> Option<u32> {
    brush_color(d.brush, Some(d)).map(rgb)
}

fn font_of(d: &Dc) -> Font {
    match obj(d.font) {
        Some(Obj::Font(f)) => *f,
        _ => Font::Body,
    }
}

fn fonts() -> &'static arcui::Fonts {
    arcui::fonts().expect("the system fonts")
}

/// Shows what was drawn on a window.
fn present(hwnd: u64) {
    if let Some(w) = win(hwnd) {
        w.window.present();
    }
}

/// Calls a window procedure.
fn call(proc_: u64, hwnd: u64, msg: u32, wp: u64, lp: i64) -> i64 {
    if proc_ == 0 {
        return DefWindowProc(hwnd, msg, wp, lp);
    }
    // SAFETY: the program registered this as its window procedure.
    let f: extern "win64" fn(u64, u32, u64, i64) -> i64 = unsafe { core::mem::transmute(proc_) };
    f(hwnd, msg, wp, lp)
}

fn send(hwnd: u64, msg: u32, wp: u64, lp: i64) -> i64 {
    match win(hwnd) {
        Some(w) => {
            let p = w.proc_;
            call(p, hwnd, msg, wp, lp)
        }
        None => 0,
    }
}

fn post(hwnd: u64, msg: u32, wp: u64, lp: i64) {
    st().queue.push_back(Msg { hwnd, msg, wp, lp });
}

fn point(x: i32, y: i32) -> i64 {
    (y as i64 & 0xFFFF) << 16 | (x as i64 & 0xFFFF)
}

// ----------------------------------------------------------------------------
// Classes and windows

/// WNDCLASS (A or W: the same layout).
#[repr(C)]
struct WNDCLASS {
    style: u32,
    proc_: u64,
    cls_extra: i32,
    wnd_extra: i32,
    instance: u64,
    icon: u64,
    cursor: u64,
    background: u64,
    menu: u64,
    name: u64,
}

fn register(style: u32, proc_: u64, brush: u64, name: String) -> u16 {
    let s = st();
    let atom = 0xC000 + s.classes.len() as u16;
    s.classes.retain(|c| !c.name.eq_ignore_ascii_case(&name));
    s.classes.push(Class { name, atom, proc_, brush, style });
    atom
}

fn class_name(p: u64, wide: bool) -> String {
    if p < 0x10000 {
        // An atom.
        return st().classes.iter().find(|c| c.atom as u64 == p).map(|c| c.name.clone()).unwrap_or_default();
    }
    if wide {
        wstr(p as *const u16)
    } else {
        cstr(p as *const u8)
    }
}

extern "win64" fn RegisterClass(wc: *const WNDCLASS) -> u16 {
    // SAFETY: the program passes a WNDCLASS; the name is wide for the W call,
    // which is told apart by looking at the string (a UTF-16 ASCII name has a
    // zero second byte).
    let wc = unsafe { &*wc };
    let wide = wc.name >= 0x10000 && unsafe { *((wc.name + 1) as *const u8) } == 0;
    register(wc.style, wc.proc_, wc.background, class_name(wc.name, wide))
}

/// WNDCLASSEX: a size and the WNDCLASS fields, then a small icon.
#[repr(C)]
struct WNDCLASSEX {
    size: u32,
    style: u32,
    proc_: u64,
    cls_extra: i32,
    wnd_extra: i32,
    instance: u64,
    icon: u64,
    cursor: u64,
    background: u64,
    menu: u64,
    name: u64,
    small_icon: u64,
}

extern "win64" fn RegisterClassExA(wc: *const WNDCLASSEX) -> u16 {
    // SAFETY: a WNDCLASSEXA.
    let wc = unsafe { &*wc };
    register(wc.style, wc.proc_, wc.background, class_name(wc.name, false))
}
extern "win64" fn RegisterClassExW(wc: *const WNDCLASSEX) -> u16 {
    // SAFETY: a WNDCLASSEXW.
    let wc = unsafe { &*wc };
    register(wc.style, wc.proc_, wc.background, class_name(wc.name, true))
}

const CW_USEDEFAULT: i32 = 0x8000_0000_u32 as i32;
/// The frame `AdjustWindowRect` adds around the client area.
const FRAME: (i32, i32, i32, i32) = (8, 31, 8, 8);

#[repr(C)]
struct CREATESTRUCT {
    params: u64,
    instance: u64,
    menu: u64,
    parent: u64,
    cy: i32,
    cx: i32,
    y: i32,
    x: i32,
    style: i32,
    name: u64,
    class: u64,
    ex_style: u32,
}

#[allow(clippy::too_many_arguments)]
fn create_window(class: String, title: String, style: u32, w: i32, h: i32, params: u64, name_ptr: u64, class_ptr: u64) -> u64 {
    let Some(cls) = st().classes.iter().find(|c| c.name.eq_ignore_ascii_case(&class)) else {
        // A system control (BUTTON, EDIT, ...): not drawn, but not an error
        // either, so the program goes on.
        return if style & 0x4000_0000 != 0 { 0xFFFF } else { 0 };
    };
    let (proc_, brush, cls_style) = (cls.proc_, cls.brush, cls.style);
    let framed = style & 0x00C0_0000 != 0 || style & 0x0004_0000 != 0;
    let (mut cw, mut ch) = if w == CW_USEDEFAULT || w <= 0 { (640, 480) } else { (w, h) };
    if framed && w != CW_USEDEFAULT {
        cw -= FRAME.0 + FRAME.2;
        ch -= FRAME.1 + FRAME.3;
    }
    let (cw, ch) = (cw.clamp(80, 1600), ch.clamp(40, 1000));
    let title = if title.is_empty() { String::from("Windows") } else { title };
    let window = if style & 0x0004_0000 != 0 { Window::resizable(&title, cw, ch) } else { Window::new(&title, cw, ch) };
    let Ok(mut window) = window else { return 0 };
    // The class background (white when there is none, as Windows shows it).
    let bg = brush_color(brush, None).unwrap_or(0x00FF_FFFF);
    window.canvas.px.fill(rgb(bg));
    window.present();
    let s = st();
    s.ever_had_window = true;
    s.wins.push(Some(Win { window, proc_, brush, style, dblclk: cls_style & 8 != 0, dirty: true, erase: true, user: 0, mouse: (0, 0) }));
    let hwnd = HWND_BASE + (s.wins.len() - 1) as u64;
    let cs = CREATESTRUCT { params, instance: 0, menu: 0, parent: 0, cy: ch, cx: cw, y: 0, x: 0, style: style as i32, name: name_ptr, class: class_ptr, ex_style: 0 };
    if call(proc_, hwnd, WM_CREATE, 0, &cs as *const CREATESTRUCT as i64) == -1 {
        destroy(hwnd);
        return 0;
    }
    send(hwnd, WM_SHOWWINDOW, 1, 0);
    send(hwnd, WM_SIZE, 0, point(cw, ch));
    send(hwnd, WM_SETFOCUS, 0, 0);
    hwnd
}

#[allow(clippy::too_many_arguments)]
extern "win64" fn CreateWindowExA(_ex: u32, class: u64, title: *const u8, style: u32, _x: i32, _y: i32, w: i32, h: i32, _parent: u64, _menu: u64, _inst: u64, params: u64) -> u64 {
    create_window(class_name(class, false), cstr(title), style, w, h, params, title as u64, class)
}
#[allow(clippy::too_many_arguments)]
extern "win64" fn CreateWindowExW(_ex: u32, class: u64, title: *const u16, style: u32, _x: i32, _y: i32, w: i32, h: i32, _parent: u64, _menu: u64, _inst: u64, params: u64) -> u64 {
    create_window(class_name(class, true), wstr(title), style, w, h, params, title as u64, class)
}

fn destroy(hwnd: u64) {
    if let Some(i) = hwnd.checked_sub(HWND_BASE) {
        if let Some(slot) = st().wins.get_mut(i as usize) {
            *slot = None; // closes the arcOS window
        }
    }
    st().timers.retain(|t| t.hwnd != hwnd);
}

extern "win64" fn DestroyWindow(hwnd: u64) -> u32 {
    if win(hwnd).is_none() {
        return 0;
    }
    send(hwnd, WM_DESTROY, 0, 0);
    destroy(hwnd);
    1
}
extern "win64" fn ShowWindow(hwnd: u64, _cmd: i32) -> u32 {
    if let Some(w) = win(hwnd) {
        w.dirty = true;
        w.window.present();
    }
    0
}
extern "win64" fn UpdateWindow(hwnd: u64) -> u32 {
    if win(hwnd).is_some_and(|w| w.dirty) {
        send(hwnd, WM_PAINT, 0, 0);
    }
    1
}
extern "win64" fn CallWindowProc(proc_: u64, hwnd: u64, msg: u32, wp: u64, lp: i64) -> i64 {
    call(proc_, hwnd, msg, wp, lp)
}

pub extern "win64" fn DefWindowProc(hwnd: u64, msg: u32, wp: u64, _lp: i64) -> i64 {
    match msg {
        WM_CLOSE => {
            DestroyWindow(hwnd);
            0
        }
        WM_PAINT => {
            let mut ps = [0u8; 72];
            BeginPaint(hwnd, ps.as_mut_ptr());
            EndPaint(hwnd, ps.as_ptr());
            0
        }
        WM_ERASEBKGND => {
            let Some(w) = win(hwnd) else { return 0 };
            if let Some(c) = brush_color(w.brush, None) {
                w.window.canvas.px.fill(rgb(c));
            }
            let _ = wp;
            1
        }
        _ => 0,
    }
}

extern "win64" fn SetWindowTextA(hwnd: u64, s: *const u8) -> u32 {
    win(hwnd).map(|w| w.window.set_title(&cstr(s))).is_some() as u32
}
extern "win64" fn SetWindowTextW(hwnd: u64, s: *const u16) -> u32 {
    win(hwnd).map(|w| w.window.set_title(&wstr(s))).is_some() as u32
}
extern "win64" fn GetClientRect(hwnd: u64, r: *mut i32) -> u32 {
    let Some(w) = win(hwnd) else { return 0 };
    let (cw, ch) = (w.window.width(), w.window.height());
    // SAFETY: a RECT.
    unsafe { core::ptr::copy_nonoverlapping([0, 0, cw, ch].as_ptr(), r, 4) };
    1
}
extern "win64" fn GetWindowRect(hwnd: u64, r: *mut i32) -> u32 {
    let Some(w) = win(hwnd) else { return 0 };
    let (cw, ch) = (w.window.width(), w.window.height());
    let v = [0, 0, cw + FRAME.0 + FRAME.2, ch + FRAME.1 + FRAME.3];
    // SAFETY: a RECT.
    unsafe { core::ptr::copy_nonoverlapping(v.as_ptr(), r, 4) };
    1
}
extern "win64" fn AdjustWindowRect(r: *mut i32, style: u32, _menu: i32) -> u32 {
    if style & 0x00C0_0000 != 0 || style & 0x0004_0000 != 0 {
        // SAFETY: a RECT.
        unsafe {
            *r -= FRAME.0;
            *r.add(1) -= FRAME.1;
            *r.add(2) += FRAME.2;
            *r.add(3) += FRAME.3;
        }
    }
    1
}
extern "win64" fn GetWindowLongPtr(hwnd: u64, index: i32) -> u64 {
    match (win(hwnd), index) {
        (Some(w), -21) => w.user,
        (Some(w), -4) => w.proc_,
        (Some(w), -16) => w.style as u64,
        _ => 0,
    }
}
extern "win64" fn SetWindowLongPtr(hwnd: u64, index: i32, v: u64) -> u64 {
    let Some(w) = win(hwnd) else { return 0 };
    match index {
        -21 => core::mem::replace(&mut w.user, v),
        -4 => core::mem::replace(&mut w.proc_, v),
        -16 => core::mem::replace(&mut w.style, v as u32) as u64,
        _ => 0,
    }
}
extern "win64" fn IsWindow(hwnd: u64) -> u32 {
    win(hwnd).is_some() as u32
}
extern "win64" fn GetFocus() -> u64 {
    let s = st();
    s.wins.iter().rposition(|w| w.is_some()).map_or(0, |i| HWND_BASE + i as u64)
}
extern "win64" fn GetSystemMetrics(i: i32) -> i32 {
    match i {
        0 | 16 => 1280, // SM_CXSCREEN, SM_CXFULLSCREEN
        1 | 17 => 800,  // SM_CYSCREEN, SM_CYFULLSCREEN
        4 => FRAME.1 - FRAME.3,
        5 | 6 | 32 | 33 => FRAME.0,
        11 | 12 => 32,
        _ => 0,
    }
}
extern "win64" fn GetCursorPos(p: *mut i32) -> u32 {
    let (x, y) = st().pointer;
    // SAFETY: a POINT.
    unsafe {
        *p = x;
        *p.add(1) = y;
    }
    1
}

// ----------------------------------------------------------------------------
// The keyboard

fn vk(k: Key) -> Option<u8> {
    Some(match k {
        Key::Enter => 0x0D,
        Key::Backspace => 0x08,
        Key::Delete => 0x2E,
        Key::Tab => 0x09,
        Key::Escape => 0x1B,
        Key::Up => 0x26,
        Key::Down => 0x28,
        Key::Left => 0x25,
        Key::Right => 0x27,
        Key::Home => 0x24,
        Key::End => 0x23,
        Key::PageUp => 0x21,
        Key::PageDown => 0x22,
        Key::Insert => 0x2D,
        Key::F(n) => 0x6F + n,
        Key::Char(' ') => 0x20,
        Key::Char(c) if c.is_ascii_alphanumeric() => c.to_ascii_uppercase() as u8,
        _ => return None,
    })
}

fn char_code(k: arcui::KeyEvent) -> Option<u32> {
    Some(match k.key {
        Key::Enter => 13,
        Key::Backspace => 8,
        Key::Tab => 9,
        Key::Escape => 27,
        Key::Char(c) if k.ctrl && c.is_ascii_alphabetic() => (c.to_ascii_uppercase() as u32) & 0x1F,
        Key::Char(c) => c as u32,
        _ => return None,
    })
}

/// A key is "down" for a moment after it was pressed: arcOS reports presses,
/// not releases.
const KEY_HOLD_MS: u64 = 150;

extern "win64" fn GetKeyState(key: i32) -> i16 {
    let t = st().keys[(key & 0xFF) as usize];
    if t != 0 && arcrt::time::uptime_ms().saturating_sub(t) < KEY_HOLD_MS {
        0x8000_u16 as i16 | 1
    } else {
        0
    }
}

// ----------------------------------------------------------------------------
// The message loop

/// Turns the arcOS events of every window into messages.
fn pump(wait_ms: u64) -> bool {
    let n = st().wins.len();
    let mut got = false;
    let alive: Vec<usize> = (0..n).filter(|&i| st().wins[i].is_some()).collect();
    for (k, &i) in alive.iter().enumerate() {
        let hwnd = HWND_BASE + i as u64;
        // Wait on the last window only, so every window is looked at.
        let ms = if k + 1 == alive.len() { wait_ms } else { 0 };
        let Some(w) = st().wins[i].as_mut() else { continue };
        let Some(ev) = w.window.event(ms) else { continue };
        got = true;
        translate(hwnd, ev);
    }
    if alive.is_empty() && wait_ms > 0 {
        arcrt::time::sleep_ms(wait_ms.min(50));
    }
    got
}

fn translate(hwnd: u64, ev: Event) {
    let now = arcrt::time::uptime_ms();
    match ev {
        Event::Press { x, y, double } => {
            st().pointer = (x, y);
            let dbl = double && win(hwnd).is_some_and(|w| w.dblclk);
            if let Some(w) = win(hwnd) {
                w.mouse = (x, y);
            }
            st().keys[1] = now;
            post(hwnd, if dbl { WM_LBUTTONDBLCLK } else { WM_LBUTTONDOWN }, 1, point(x, y));
        }
        Event::Release => {
            let (x, y) = st().pointer;
            st().keys[1] = 0;
            post(hwnd, WM_LBUTTONUP, 0, point(x, y));
        }
        Event::Menu { x, y } => {
            st().pointer = (x, y);
            post(hwnd, WM_RBUTTONDOWN, 2, point(x, y));
            post(hwnd, WM_RBUTTONUP, 0, point(x, y));
        }
        Event::Move { x, y, held } => {
            st().pointer = (x, y);
            post(hwnd, WM_MOUSEMOVE, held as u64, point(x, y));
        }
        Event::Scroll(d) => {
            let (x, y) = st().pointer;
            post(hwnd, WM_MOUSEWHEEL, ((-d * 120) as u16 as u64) << 16, point(x, y));
        }
        Event::Key(k) => {
            if let Some(v) = vk(k.key) {
                st().keys[v as usize] = now;
                post(hwnd, WM_KEYDOWN, v as u64, 1);
            }
            if let Some(c) = char_code(k) {
                post(hwnd, WM_CHAR, c as u64, 1);
            }
            if let Some(v) = vk(k.key) {
                post(hwnd, WM_KEYUP, v as u64, 0xC000_0001);
            }
        }
        Event::Resize { w, h } => {
            if let Some(win) = win(hwnd) {
                let bg = brush_color(win.brush, None).unwrap_or(0x00FF_FFFF);
                win.window.canvas.px.fill(rgb(bg));
                win.dirty = true;
            }
            post(hwnd, WM_SIZE, 0, point(w, h));
        }
        Event::Close => post(hwnd, WM_CLOSE, 0, 0),
        // Windows programs ask for files with DragAcceptFiles, which is not
        // there: their windows are made without WIN_DROPS.
        Event::Leave | Event::Drop { .. } => {}
    }
}

/// The next message: posted ones first, then window events, timers and
/// painting. `None` when there is nothing and `wait` is false.
fn next(wait: bool, remove: bool) -> Option<Msg> {
    loop {
        if let Some(m) = st().queue.front().copied() {
            if remove {
                st().queue.pop_front();
            }
            return Some(m);
        }
        if let Some(code) = st().quit {
            if remove {
                st().quit = None;
            }
            return Some(Msg { hwnd: 0, msg: WM_QUIT, wp: code as u64, lp: 0 });
        }
        if pump(0) {
            continue;
        }
        // Timers.
        let now = arcrt::time::uptime_ms();
        if let Some(t) = st().timers.iter_mut().filter(|t| t.due <= now).min_by_key(|t| t.due) {
            let m = Msg { hwnd: t.hwnd, msg: WM_TIMER, wp: t.id, lp: t.proc_ as i64 };
            if remove {
                t.due = now + t.ms.max(1);
            }
            return Some(m);
        }
        // Painting.
        if let Some(i) = st().wins.iter().position(|w| w.as_ref().is_some_and(|w| w.dirty)) {
            return Some(Msg { hwnd: HWND_BASE + i as u64, msg: WM_PAINT, wp: 0, lp: 0 });
        }
        // A program whose windows are all gone and that has nothing else to
        // wait for would wait forever: end its loop.
        let s = st();
        if s.ever_had_window && s.wins.iter().all(|w| w.is_none()) && s.timers.is_empty() {
            return Some(Msg { hwnd: 0, msg: WM_QUIT, wp: 0, lp: 0 });
        }
        if !wait {
            return None;
        }
        let until = s.timers.iter().map(|t| t.due).min().map_or(1000, |d| d.saturating_sub(now).clamp(1, 1000));
        pump(until);
    }
}

fn store(out: *mut MSG, m: Msg) {
    let (x, y) = st().pointer;
    // SAFETY: the program passes a MSG.
    unsafe {
        *out = MSG { hwnd: m.hwnd, message: m.msg, wparam: m.wp, lparam: m.lp, time: arcrt::time::uptime_ms() as u32, pt: [x, y], private: 0 };
    }
}

extern "win64" fn GetMessage(out: *mut MSG, _hwnd: u64, _min: u32, _max: u32) -> i32 {
    let m = next(true, true).expect("a message");
    store(out, m);
    (m.msg != WM_QUIT) as i32
}
extern "win64" fn PeekMessage(out: *mut MSG, _hwnd: u64, _min: u32, _max: u32, remove: u32) -> i32 {
    match next(false, remove & 1 != 0) {
        Some(m) => {
            store(out, m);
            1
        }
        None => 0,
    }
}
extern "win64" fn WaitMessage() -> u32 {
    if next(false, false).is_none() {
        pump(50);
    }
    1
}
extern "win64" fn DispatchMessage(m: *const MSG) -> i64 {
    // SAFETY: a MSG from GetMessage/PeekMessage.
    let m = unsafe { &*m };
    if m.message == WM_TIMER && m.lparam != 0 {
        // SAFETY: the TIMERPROC the program passed to SetTimer.
        let f: extern "win64" fn(u64, u32, u64, u32) = unsafe { core::mem::transmute(m.lparam) };
        f(m.hwnd, WM_TIMER, m.wparam, arcrt::time::uptime_ms() as u32);
        return 0;
    }
    if m.message == WM_PAINT {
        let r = send(m.hwnd, WM_PAINT, 0, 0);
        // A procedure that does not paint still has to stop the WM_PAINTs.
        if let Some(w) = win(m.hwnd) {
            if w.dirty {
                w.dirty = false;
                w.window.present();
            }
        }
        return r;
    }
    send(m.hwnd, m.message, m.wparam, m.lparam)
}
extern "win64" fn PostMessage(hwnd: u64, msg: u32, wp: u64, lp: i64) -> u32 {
    if msg == WM_QUIT {
        st().quit = Some(wp as i32);
    } else {
        post(hwnd, msg, wp, lp);
    }
    1
}
extern "win64" fn SendMessage(hwnd: u64, msg: u32, wp: u64, lp: i64) -> i64 {
    send(hwnd, msg, wp, lp)
}
extern "win64" fn PostQuitMessage(code: i32) {
    st().quit = Some(code);
}
extern "win64" fn SetTimer(hwnd: u64, id: u64, ms: u32, proc_: u64) -> u64 {
    let s = st();
    let id = if hwnd == 0 {
        s.next_timer += 1;
        s.next_timer
    } else {
        id
    };
    s.timers.retain(|t| !(t.hwnd == hwnd && t.id == id));
    let ms = (ms as u64).max(10);
    s.timers.push(Timer { hwnd, id, ms, due: arcrt::time::uptime_ms() + ms, proc_ });
    id
}
extern "win64" fn KillTimer(hwnd: u64, id: u64) -> u32 {
    let s = st();
    let n = s.timers.len();
    s.timers.retain(|t| !(t.hwnd == hwnd && t.id == id));
    (s.timers.len() != n) as u32
}
extern "win64" fn InvalidateRect(hwnd: u64, _r: u64, erase: u32) -> u32 {
    let s = st();
    let targets: Vec<usize> = if hwnd == 0 { (0..s.wins.len()).collect() } else { hwnd.checked_sub(HWND_BASE).map(|i| i as usize).into_iter().collect() };
    for i in targets {
        if let Some(Some(w)) = s.wins.get_mut(i) {
            w.dirty = true;
            w.erase |= erase != 0;
        }
    }
    1
}
extern "win64" fn ValidateRect(hwnd: u64, _r: u64) -> u32 {
    if let Some(w) = win(hwnd) {
        w.dirty = false;
    }
    1
}

// ----------------------------------------------------------------------------
// Painting

extern "win64" fn BeginPaint(hwnd: u64, ps: *mut u8) -> u64 {
    let Some(w) = win(hwnd) else { return 0 };
    let (cw, ch) = (w.window.width(), w.window.height());
    let erase = core::mem::replace(&mut w.erase, false);
    w.dirty = false;
    let hdc = new_or_reused_dc(hwnd);
    if erase {
        send(hwnd, WM_ERASEBKGND, hdc, 0);
    }
    // SAFETY: a PAINTSTRUCT: hdc, fErase, rcPaint, then reserved fields.
    unsafe {
        core::ptr::write_bytes(ps, 0, 72);
        *(ps as *mut u64) = hdc;
        core::ptr::copy_nonoverlapping([0, 0, cw, ch].as_ptr(), ps.add(12) as *mut i32, 4);
    }
    hdc
}
extern "win64" fn EndPaint(hwnd: u64, _ps: *const u8) -> u32 {
    present(hwnd);
    1
}
/// One DC per window, kept: a fresh one gets the default objects back.
fn new_or_reused_dc(hwnd: u64) -> u64 {
    let s = st();
    if let Some(i) = s.dcs.iter().position(|d| d.hwnd == hwnd && d.bitmap == 0) {
        let fresh = Dc { hwnd, ..s.dcs[i] };
        s.dcs[i] = Dc { brush: OBJ_BASE, pen: OBJ_BASE + 7, font: OBJ_BASE + 13, text: 0, bk: 0x00FF_FFFF, opaque: true, ..fresh };
        return HDC_BASE + i as u64;
    }
    new_dc(hwnd)
}
extern "win64" fn GetDC(hwnd: u64) -> u64 {
    if hwnd == 0 {
        return new_dc(0);
    }
    new_or_reused_dc(hwnd)
}
extern "win64" fn ReleaseDC(_hwnd: u64, hdc: u64) -> i32 {
    if let Some(d) = dc(hdc) {
        present(d.hwnd);
    }
    1
}

fn rect(r: *const i32) -> Rect {
    // SAFETY: a RECT.
    let v = unsafe { core::slice::from_raw_parts(r, 4) };
    Rect::new(v[0], v[1], v[2] - v[0], v[3] - v[1])
}

extern "win64" fn FillRect(hdc: u64, r: *const i32, brush: u64) -> i32 {
    let r = rect(r);
    let d = dc(hdc).copied();
    let Some(c) = brush_color(brush, d.as_ref()) else { return 1 };
    with_canvas(hdc, |cv, _| cv.fill(r, rgb(c)));
    1
}
extern "win64" fn FrameRect(hdc: u64, r: *const i32, brush: u64) -> i32 {
    let r = rect(r);
    let d = dc(hdc).copied();
    let Some(c) = brush_color(brush, d.as_ref()) else { return 1 };
    with_canvas(hdc, |cv, _| outline(cv, r, rgb(c), 1));
    1
}
fn outline(cv: &mut Canvas, r: Rect, c: u32, w: i32) {
    cv.fill(Rect::new(r.x, r.y, r.w, w), c);
    cv.fill(Rect::new(r.x, r.bottom() - w, r.w, w), c);
    cv.fill(Rect::new(r.x, r.y, w, r.h), c);
    cv.fill(Rect::new(r.right() - w, r.y, w, r.h), c);
}
extern "win64" fn GetSysColor(i: i32) -> u32 {
    sys_color(i as u32)
}
extern "win64" fn GetSysColorBrush(i: i32) -> u64 {
    (i + 1) as u64
}

fn text_out(hdc: u64, x: i32, y: i32, s: &str) {
    let f = fonts();
    with_canvas(hdc, |cv, d| {
        let font = font_of(d);
        if d.opaque {
            let (lh, _) = f.metrics(font);
            cv.fill(Rect::new(x, y, f.width(font, s), lh), rgb(d.bk));
        }
        f.draw(cv, font, x, y, s, rgb(d.text));
    });
}

/// `len` characters of a string, or up to its NUL when `len` is -1.
fn str_a(s: *const u8, len: i32) -> String {
    if len < 0 {
        return cstr(s);
    }
    // SAFETY: `len` bytes.
    String::from_utf8_lossy(unsafe { core::slice::from_raw_parts(s, len as usize) }).into_owned()
}
fn str_w(s: *const u16, len: i32) -> String {
    if len < 0 {
        return wstr(s);
    }
    // SAFETY: `len` UTF-16 units.
    String::from_utf16_lossy(unsafe { core::slice::from_raw_parts(s, len as usize) })
}

extern "win64" fn TextOutA(hdc: u64, x: i32, y: i32, s: *const u8, len: i32) -> u32 {
    text_out(hdc, x, y, &str_a(s, len));
    1
}
extern "win64" fn TextOutW(hdc: u64, x: i32, y: i32, s: *const u16, len: i32) -> u32 {
    text_out(hdc, x, y, &str_w(s, len));
    1
}

const DT_CENTER: u32 = 0x1;
const DT_RIGHT: u32 = 0x2;
const DT_VCENTER: u32 = 0x4;
const DT_BOTTOM: u32 = 0x8;
const DT_WORDBREAK: u32 = 0x10;
const DT_SINGLELINE: u32 = 0x20;
const DT_CALCRECT: u32 = 0x400;

fn draw_text(hdc: u64, s: &str, r: *mut i32, flags: u32) -> i32 {
    let f = fonts();
    let Some(d) = dc(hdc).copied() else { return 0 };
    let font = font_of(&d);
    let area = rect(r);
    let (lh, _) = f.metrics(font);
    let lines: Vec<String> = if flags & DT_SINGLELINE != 0 {
        alloc::vec![s.replace('\n', " ")]
    } else if flags & DT_WORDBREAK != 0 {
        s.split('\n').flat_map(|l| f.wrap(font, l, area.w.max(1))).collect()
    } else {
        s.split('\n').map(String::from).collect()
    };
    let height = lh * lines.len() as i32;
    if flags & DT_CALCRECT != 0 {
        let w = lines.iter().map(|l| f.width(font, l)).max().unwrap_or(0);
        // SAFETY: a RECT.
        unsafe {
            *r.add(2) = area.x + w;
            *r.add(3) = area.y + height;
        }
        return height;
    }
    let mut y = area.y;
    if flags & DT_SINGLELINE != 0 && flags & DT_VCENTER != 0 {
        y = area.y + (area.h - lh) / 2;
    } else if flags & DT_SINGLELINE != 0 && flags & DT_BOTTOM != 0 {
        y = area.bottom() - lh;
    }
    with_canvas(hdc, |cv, d| {
        for l in &lines {
            let w = f.width(font, l);
            let x = if flags & DT_CENTER != 0 {
                area.x + (area.w - w) / 2
            } else if flags & DT_RIGHT != 0 {
                area.right() - w
            } else {
                area.x
            };
            if d.opaque {
                cv.fill(Rect::new(x, y, w, lh), rgb(d.bk));
            }
            f.draw(cv, font, x, y, l, rgb(d.text));
            y += lh;
        }
    });
    height
}
extern "win64" fn DrawTextA(hdc: u64, s: *const u8, len: i32, r: *mut i32, flags: u32) -> i32 {
    draw_text(hdc, &str_a(s, len), r, flags)
}
extern "win64" fn DrawTextW(hdc: u64, s: *const u16, len: i32, r: *mut i32, flags: u32) -> i32 {
    draw_text(hdc, &str_w(s, len), r, flags)
}
fn extent(hdc: u64, s: &str, size: *mut i32) -> u32 {
    let font = dc(hdc).map_or(Font::Body, |d| font_of(d));
    let f = fonts();
    // SAFETY: a SIZE.
    unsafe {
        *size = f.width(font, s);
        *size.add(1) = f.metrics(font).0;
    }
    1
}
extern "win64" fn GetTextExtentPoint32A(hdc: u64, s: *const u8, len: i32, size: *mut i32) -> u32 {
    extent(hdc, &str_a(s, len), size)
}
extern "win64" fn GetTextExtentPoint32W(hdc: u64, s: *const u16, len: i32, size: *mut i32) -> u32 {
    extent(hdc, &str_w(s, len), size)
}
extern "win64" fn GetTextMetrics(hdc: u64, tm: *mut i32) -> u32 {
    let font = dc(hdc).map_or(Font::Body, |d| font_of(d));
    let f = fonts();
    let (lh, base) = f.metrics(font);
    let avg = f.width(font, "x");
    let max = f.width(font, "W");
    // tmHeight, tmAscent, tmDescent, tmInternalLeading, tmExternalLeading,
    // tmAveCharWidth, tmMaxCharWidth, tmWeight.
    let v = [lh, base, lh - base, 0, 0, avg, max, if font == Font::Strong { 700 } else { 400 }];
    // SAFETY: a TEXTMETRIC starts with these eight LONGs.
    unsafe { core::ptr::copy_nonoverlapping(v.as_ptr(), tm, 8) };
    1
}
extern "win64" fn SetTextColor(hdc: u64, c: u32) -> u32 {
    dc(hdc).map_or(0, |d| core::mem::replace(&mut d.text, c))
}
extern "win64" fn SetBkColor(hdc: u64, c: u32) -> u32 {
    dc(hdc).map_or(0, |d| core::mem::replace(&mut d.bk, c))
}
extern "win64" fn SetBkMode(hdc: u64, mode: i32) -> i32 {
    dc(hdc).map_or(0, |d| {
        let old = if d.opaque { 2 } else { 1 };
        d.opaque = mode == 2;
        old
    })
}
extern "win64" fn SetDCBrushColor(hdc: u64, c: u32) -> u32 {
    dc(hdc).map_or(0, |d| core::mem::replace(&mut d.dc_brush, c))
}
extern "win64" fn SetDCPenColor(hdc: u64, c: u32) -> u32 {
    dc(hdc).map_or(0, |d| core::mem::replace(&mut d.dc_pen, c))
}

// ----------------------------------------------------------------------------
// GDI objects

extern "win64" fn GetStockObject(n: i32) -> u64 {
    if (0..=19).contains(&n) {
        OBJ_BASE + n as u64
    } else {
        0
    }
}
extern "win64" fn CreateSolidBrush(c: u32) -> u64 {
    new_obj(Obj::Brush(Some(c)))
}
extern "win64" fn CreatePen(style: i32, width: i32, c: u32) -> u64 {
    new_obj(Obj::Pen(if style == 5 { None } else { Some(c) }, width.max(1)))
}
fn font_for(height: i32, weight: i32, face: &str) -> Font {
    let h = height.abs();
    let face = face.to_ascii_lowercase();
    if face.contains("courier") || face.contains("consolas") || face.contains("mono") || face.contains("terminal") {
        Font::Mono
    } else if h >= 34 {
        Font::Display
    } else if h >= 26 {
        Font::Title
    } else if h >= 20 {
        Font::Heading
    } else if weight >= 600 {
        Font::Strong
    } else if h > 0 && h <= 12 {
        Font::Small
    } else {
        Font::Body
    }
}
#[allow(clippy::too_many_arguments)]
extern "win64" fn CreateFont(
    height: i32,
    _w: i32,
    _esc: i32,
    _orient: i32,
    weight: i32,
    _italic: u32,
    _under: u32,
    _strike: u32,
    _charset: u32,
    _out: u32,
    _clip: u32,
    _quality: u32,
    _pitch: u32,
    face: *const u8,
) -> u64 {
    // The face name is ANSI or wide; ASCII names work either way.
    let name = if !face.is_null() && unsafe { *face.add(1) } == 0 { wstr(face as *const u16) } else { cstr(face) };
    new_obj(Obj::Font(font_for(height, weight, &name)))
}
extern "win64" fn CreateFontIndirect(lf: *const i32) -> u64 {
    // SAFETY: a LOGFONT: lfHeight, ..., lfWeight (the fifth LONG), then the
    // face name at byte 28.
    let (h, weight, face) = unsafe { (*lf, *lf.add(4), (lf as *const u8).add(28)) };
    let name = if unsafe { *face.add(1) } == 0 { wstr(face as *const u16) } else { cstr(face) };
    new_obj(Obj::Font(font_for(h, weight, &name)))
}
extern "win64" fn CreateCompatibleDC(_hdc: u64) -> u64 {
    new_dc(0)
}
extern "win64" fn CreateCompatibleBitmap(_hdc: u64, w: i32, h: i32) -> u64 {
    new_obj(Obj::Bitmap(Canvas::new(w.clamp(1, 4096), h.clamp(1, 4096), 0xFF00_0000)))
}
extern "win64" fn SelectObject(hdc: u64, h: u64) -> u64 {
    let Some(d) = dc(hdc) else { return 0 };
    let slot = match obj(h) {
        Some(Obj::Brush(_)) => &mut d.brush,
        Some(Obj::Pen(..)) => &mut d.pen,
        Some(Obj::Font(_)) => &mut d.font,
        Some(Obj::Bitmap(_)) => &mut d.bitmap,
        _ => return 0,
    };
    core::mem::replace(slot, h)
}
extern "win64" fn DeleteObject(h: u64) -> u32 {
    if h >= OBJ_BASE + 20 {
        if let Some(o) = obj(h) {
            *o = Obj::Free;
            return 1;
        }
    }
    0
}
#[allow(clippy::too_many_arguments)]
extern "win64" fn BitBlt(dst: u64, x: i32, y: i32, w: i32, h: i32, src: u64, sx: i32, sy: i32, rop: u32) -> u32 {
    StretchBlt(dst, x, y, w, h, src, sx, sy, w, h, rop)
}
#[allow(clippy::too_many_arguments)]
extern "win64" fn StretchBlt(dst: u64, x: i32, y: i32, w: i32, h: i32, src: u64, sx: i32, sy: i32, sw: i32, sh: i32, rop: u32) -> u32 {
    // BLACKNESS / WHITENESS need no source.
    if rop == 0x0000_0042 || rop == 0x00FF_0062 {
        let c = if rop == 0x42 { 0xFF00_0000 } else { 0xFFFF_FFFF };
        with_canvas(dst, |cv, _| cv.fill(Rect::new(x, y, w, h), c));
        return 1;
    }
    let Some(part) = with_canvas(src, |cv, _| cv.crop(Rect::new(sx, sy, sw, sh).intersect(&cv.bounds()))) else { return 0 };
    let part = if part.w != w || part.h != h { part.scaled(w.max(1), h.max(1)) } else { part };
    with_canvas(dst, |cv, _| cv.blit(&part, 0, 0, Rect::new(x, y, part.w, part.h)));
    1
}

// ----------------------------------------------------------------------------
// Shapes

extern "win64" fn Rectangle(hdc: u64, l: i32, t: i32, r: i32, b: i32) -> u32 {
    with_canvas(hdc, |cv, d| {
        let area = Rect::new(l, t, r - l, b - t);
        if let Some(c) = brush_of(d) {
            cv.fill(area, c);
        }
        if let Some((c, w)) = pen_of(d) {
            outline(cv, area, c, w);
        }
    });
    1
}
#[allow(clippy::too_many_arguments)]
extern "win64" fn RoundRect(hdc: u64, l: i32, t: i32, r: i32, b: i32, rw: i32, _rh: i32) -> u32 {
    with_canvas(hdc, |cv, d| {
        let area = Rect::new(l, t, r - l, b - t);
        let rad = (rw / 2).min(area.w / 2).min(area.h / 2);
        if let Some(c) = brush_of(d) {
            cv.fill_round(area, rad, c);
        }
        if let Some((c, w)) = pen_of(d) {
            cv.stroke_round(area, rad, w, c);
        }
    });
    1
}
/// The span `[x0, x1)` that the ellipse inside `r` covers on row `y`.
fn ellipse_span(r: Rect, y: i32) -> Option<(i32, i32)> {
    if r.w <= 0 || r.h <= 0 || y < r.y || y >= r.bottom() {
        return None;
    }
    let (a, b) = (r.w as f64, r.h as i64);
    // Twice the distance from the centre, to stay in whole numbers.
    let dy = 2 * (y - r.y) as i64 + 1 - b;
    let t = b * b - dy * dy;
    if t < 0 {
        return None;
    }
    let half = (a * arcc_core::float::sqrt(t as f64) / b as f64 / 2.0) as i32;
    let cx = r.x + r.w / 2;
    Some((cx - half, cx + half + (r.w & 1)))
}
fn fill_ellipse(cv: &mut Canvas, r: Rect, c: u32) {
    for y in r.y..r.bottom() {
        if let Some((x0, x1)) = ellipse_span(r, y) {
            cv.fill(Rect::new(x0, y, x1 - x0, 1), c);
        }
    }
}
/// The outline: on each row, what the outer ellipse covers and the inner
/// one (`w` pixels in) does not.
fn ring_ellipse(cv: &mut Canvas, r: Rect, w: i32, c: u32) {
    let inner = Rect::new(r.x + w, r.y + w, r.w - 2 * w, r.h - 2 * w);
    for y in r.y..r.bottom() {
        let Some((x0, x1)) = ellipse_span(r, y) else { continue };
        match ellipse_span(inner, y) {
            Some((i0, i1)) => {
                cv.fill(Rect::new(x0, y, i0 - x0, 1), c);
                cv.fill(Rect::new(i1, y, x1 - i1, 1), c);
            }
            None => cv.fill(Rect::new(x0, y, x1 - x0, 1), c),
        }
    }
}
extern "win64" fn Ellipse(hdc: u64, l: i32, t: i32, r: i32, b: i32) -> u32 {
    with_canvas(hdc, |cv, d| {
        let area = Rect::new(l, t, r - l, b - t);
        if let Some(c) = brush_of(d) {
            fill_ellipse(cv, area, c);
        }
        if let Some((c, w)) = pen_of(d) {
            ring_ellipse(cv, area, w, c);
        }
    });
    1
}
extern "win64" fn MoveToEx(hdc: u64, x: i32, y: i32, prev: *mut i32) -> u32 {
    let Some(d) = dc(hdc) else { return 0 };
    if !prev.is_null() {
        // SAFETY: a POINT.
        unsafe {
            *prev = d.x;
            *prev.add(1) = d.y;
        }
    }
    d.x = x;
    d.y = y;
    1
}
extern "win64" fn LineTo(hdc: u64, x: i32, y: i32) -> u32 {
    let Some(d) = dc(hdc) else { return 0 };
    let (x0, y0) = (d.x, d.y);
    d.x = x;
    d.y = y;
    with_canvas(hdc, |cv, d| {
        if let Some((c, w)) = pen_of(d) {
            line(cv, x0, y0, x, y, w, c);
        }
    });
    1
}
fn line(cv: &mut Canvas, x0: i32, y0: i32, x1: i32, y1: i32, w: i32, c: u32) {
    if x0 == x1 || y0 == y1 {
        // Straight lines stay crisp, as GDI draws them.
        let r = Rect::new(x0.min(x1), y0.min(y1), (x1 - x0).abs().max(w), (y1 - y0).abs().max(w));
        cv.fill(r, c);
    } else {
        cv.line(x0, y0, x1, y1, w, c);
    }
}
fn points(p: *const i32, n: i32) -> Vec<(i32, i32)> {
    // SAFETY: n POINTs.
    let v = unsafe { core::slice::from_raw_parts(p, 2 * n.max(0) as usize) };
    v.chunks(2).map(|c| (c[0], c[1])).collect()
}
extern "win64" fn Polyline(hdc: u64, p: *const i32, n: i32) -> u32 {
    let pts = points(p, n);
    with_canvas(hdc, |cv, d| {
        if let Some((c, w)) = pen_of(d) {
            for s in pts.windows(2) {
                line(cv, s[0].0, s[0].1, s[1].0, s[1].1, w, c);
            }
        }
    });
    1
}
extern "win64" fn Polygon(hdc: u64, p: *const i32, n: i32) -> u32 {
    let pts = points(p, n);
    if pts.len() < 3 {
        return 0;
    }
    with_canvas(hdc, |cv, d| {
        if let Some(c) = brush_of(d) {
            // A fan from the first point (right for convex shapes).
            for k in 1..pts.len() - 1 {
                cv.triangle([pts[0], pts[k], pts[k + 1]], c);
            }
        }
        if let Some((c, w)) = pen_of(d) {
            for k in 0..pts.len() {
                let (a, b) = (pts[k], pts[(k + 1) % pts.len()]);
                line(cv, a.0, a.1, b.0, b.1, w, c);
            }
        }
    });
    1
}
extern "win64" fn SetPixel(hdc: u64, x: i32, y: i32, c: u32) -> u32 {
    with_canvas(hdc, |cv, _| cv.fill(Rect::new(x, y, 1, 1), rgb(c)));
    c
}
extern "win64" fn GetPixel(hdc: u64, x: i32, y: i32) -> u32 {
    with_canvas(hdc, |cv, _| {
        if cv.bounds().contains(x, y) {
            let p = cv.get(x, y);
            (p & 0xFF) << 16 | (p & 0xFF00) | (p >> 16 & 0xFF)
        } else {
            u32::MAX
        }
    })
    .unwrap_or(u32::MAX)
}

// ----------------------------------------------------------------------------
// MessageBox

extern "win64" fn MessageBoxA(_hwnd: u64, text: *const u8, caption: *const u8, kind: u32) -> i32 {
    message_box(&cstr(text), &cstr(caption), kind)
}
extern "win64" fn MessageBoxW(_hwnd: u64, text: *const u16, caption: *const u16, kind: u32) -> i32 {
    message_box(&wstr(text), &wstr(caption), kind)
}

/// A small window with the message and its buttons; returns the `ID*` of the
/// button chosen (Enter: the first, Escape or closing: the last).
fn message_box(text: &str, caption: &str, kind: u32) -> i32 {
    let buttons: &[(&str, i32)] = match kind & 0xF {
        1 => &[("OK", 1), ("Annulla", 2)],
        3 => &[("Sì", 6), ("No", 7), ("Annulla", 2)],
        4 => &[("Sì", 6), ("No", 7)],
        5 => &[("Riprova", 4), ("Annulla", 2)],
        _ => &[("OK", 1)],
    };
    let f = fonts();
    let width = 420;
    let lines = f.wrap(Font::Body, text, width - 48);
    let (lh, _) = f.metrics(Font::Body);
    let height = 40 + lh * lines.len() as i32 + 76;
    let caption = if caption.is_empty() { "Messaggio" } else { caption };
    let Ok(mut w) = Window::new(caption, width, height) else {
        crate::win::console_write(alloc::format!("{caption}: {text}\n").as_bytes());
        return buttons[0].1;
    };
    let bw = 110;
    let rects: Vec<Rect> = (0..buttons.len())
        .map(|i| Rect::new(width - 24 - (buttons.len() - i) as i32 * (bw + 10) + 10, height - 56, bw, 36))
        .collect();
    let mut hover = usize::MAX;
    loop {
        w.clear();
        let mut y = 28;
        for l in &lines {
            w.text(Font::Body, 24, y, l, ui::TEXT);
            y += lh;
        }
        for (i, (label, _)) in buttons.iter().enumerate() {
            ui::button(&mut w.canvas, f, rects[i], label, i == 0, i == hover);
        }
        w.present();
        match w.wait() {
            Event::Press { x, y, .. } => {
                if let Some(i) = rects.iter().position(|r| r.contains(x, y)) {
                    return buttons[i].1;
                }
            }
            Event::Move { x, y, .. } => hover = rects.iter().position(|r| r.contains(x, y)).unwrap_or(usize::MAX),
            Event::Key(k) if k.key == Key::Enter => return buttons[0].1,
            Event::Key(k) if k.key == Key::Escape => return buttons[buttons.len() - 1].1,
            Event::Close => return buttons[buttons.len() - 1].1,
            _ => {}
        }
    }
}
