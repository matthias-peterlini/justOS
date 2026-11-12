//! Dialogs the system shows for programs, outside them: may this app use a
//! folder or a device ([`crate::sandbox`]), and the file picker that hands
//! an app the file the user chooses (`PICK_FILE`). Each is a small window of
//! its own; the program that asked waits for the answer.

use super::picker::{Picked, Picker};
use crate::dev::input::{Key, KeyEvent};
use crate::gui::gfx::{Canvas, Rect};
use crate::gui::res;
use crate::gui::text::{self, Font};
use crate::gui::ui;
use crate::gui::wm::{Action, App};
use crate::sync::{IrqLock, Mutex};
use crate::task::WaitQueue;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

enum Ask {
    /// "«Disegno» vuole usare la cartella Immagini".
    Permission { app: String, icon: String, what: String },
    Pick { app: String, save: bool, name: String },
}

enum Answer {
    Allow(bool),
    Path(String),
    /// Closed, or the program stopped.
    Cancel,
}

struct Request {
    ask: Ask,
    answer: Option<Answer>,
    /// The answer was taken (or the asker gave up): the window goes.
    done: bool,
}

type Shared = Arc<Mutex<Request>>;

/// Dialogs for the desktop to open.
static NEW: IrqLock<Vec<Shared>> = IrqLock::new(Vec::new());
static ANSWERED: WaitQueue = WaitQueue::new();

/// Shows the dialog and waits for the answer.
fn ask(ask: Ask) -> Answer {
    let req = Arc::new(Mutex::new(Request { ask, answer: None, done: false }));
    NEW.lock().push(req.clone());
    loop {
        let got = ANSWERED.wait_for_timeout(100, || {
            let mut r = req.lock();
            r.answer.take().map(|a| {
                r.done = true;
                a
            })
        });
        if let Some(a) = got {
            return a;
        }
        if crate::user::kill_requested() {
            req.lock().done = true;
            return Answer::Cancel;
        }
    }
}

pub fn permission(app: &crate::sandbox::App, what: &str) -> Option<bool> {
    match ask(Ask::Permission { app: app.name.clone(), icon: app.icon.clone(), what: String::from(what) }) {
        Answer::Allow(yes) => Some(yes),
        _ => None,
    }
}

pub fn pick(app: &str, save: bool, name: &str) -> Option<String> {
    match ask(Ask::Pick { app: String::from(app), save, name: String::from(name) }) {
        Answer::Path(p) => Some(p),
        _ => None,
    }
}

pub fn take_new() -> Vec<Box<dyn App>> {
    let list = core::mem::take(&mut *NEW.lock());
    list.into_iter()
        .map(|req| {
            let picker = match &req.lock().ask {
                Ask::Pick { save: true, name, .. } => Some(Picker::save("sys:/home/Documenti", name)),
                Ask::Pick { save: false, .. } => Some(Picker::open("sys:/home/Documenti", &[])),
                Ask::Permission { .. } => None,
            };
            Box::new(Prompt { req, picker, hover: None }) as Box<dyn App>
        })
        .collect()
}

struct Prompt {
    req: Shared,
    picker: Option<Picker>,
    hover: Option<Rect>,
}

impl Prompt {
    fn answer(&self, a: Answer) {
        let mut r = self.req.lock();
        if r.answer.is_none() && !r.done {
            r.answer = Some(a);
        }
        drop(r);
        ANSWERED.notify_all();
    }

    /// "Non consentire", "Consenti".
    fn buttons(area: Rect) -> [Rect; 2] {
        let y = area.bottom() - 54;
        [Rect::new(area.right() - 300, y, 140, 38), Rect::new(area.right() - 150, y, 130, 38)]
    }

    fn picked(&mut self, p: Picked) -> Action {
        match p {
            Picked::Nothing => return Action::Redraw,
            Picked::Cancel => self.answer(Answer::Cancel),
            Picked::Open(path) | Picked::Save(path) => self.answer(Answer::Path(path)),
        }
        Action::Redraw
    }
}

impl App for Prompt {
    fn title(&self) -> String {
        match &self.req.lock().ask {
            Ask::Permission { .. } => String::from("Privacy"),
            Ask::Pick { app, save: false, .. } => format!("{app}: apri"),
            Ask::Pick { app, save: true, .. } => format!("{app}: salva"),
        }
    }

    fn icon(&self) -> &str {
        "settings"
    }

    fn kind(&self) -> &'static str {
        "prompt"
    }

    fn size(&self) -> (i32, i32) {
        if self.picker.is_some() {
            (600, 410)
        } else {
            (460, 200)
        }
    }

    fn draw(&mut self, c: &mut Canvas, area: Rect, _blur: &Canvas, _focused: bool) {
        if let Some(p) = &mut self.picker {
            p.draw(c, area);
            return;
        }
        let (app, icon, what) = match &self.req.lock().ask {
            Ask::Permission { app, icon, what } => (app.clone(), icon.clone(), what.clone()),
            Ask::Pick { .. } => return,
        };
        res::draw_icon(c, &icon, 52, area.x + 24, area.y + 24);
        let x = area.x + 96;
        let mut y = area.y + 24;
        for line in text::wrap(Font::Strong, &format!("«{app}» vuole usare {what}"), area.w - 120) {
            text::draw(c, Font::Strong, x, y, &line, ui::text());
            y += 24;
        }
        y += 6;
        let detail = "Potrà leggere e cambiare quello che c'è. Puoi cambiare idea in Impostazioni, Privacy.";
        for line in text::wrap(Font::Small, detail, area.w - 120) {
            text::draw(c, Font::Small, x, y, &line, ui::text_dim());
            y += 18;
        }
        let [no, yes] = Self::buttons(area);
        ui::button(c, no, "Non consentire", false, self.hover == Some(no));
        ui::button(c, yes, "Consenti", true, self.hover == Some(yes));
    }

    fn press(&mut self, x: i32, y: i32, double: bool, area: Rect) -> Action {
        let (ax, ay) = (area.x + x, area.y + y);
        if let Some(p) = &mut self.picker {
            let got = p.press(ax, ay, double, area);
            return self.picked(got);
        }
        let [no, yes] = Self::buttons(area);
        if no.contains(ax, ay) {
            self.answer(Answer::Allow(false));
        } else if yes.contains(ax, ay) {
            self.answer(Answer::Allow(true));
        }
        Action::None
    }

    fn hover(&mut self, at: Option<(i32, i32)>, area: Rect) -> bool {
        let at = at.map(|(x, y)| (area.x + x, area.y + y));
        if let Some(p) = &mut self.picker {
            return p.hover(at, area);
        }
        let h = at.and_then(|(x, y)| Self::buttons(area).into_iter().find(|r| r.contains(x, y)));
        let changed = h != self.hover;
        self.hover = h;
        changed
    }

    fn key(&mut self, ev: KeyEvent) -> Action {
        if let Some(p) = &mut self.picker {
            let got = p.key(ev);
            return self.picked(got);
        }
        // Escape says no; saying yes takes a click.
        if ev.key == Key::Escape {
            self.answer(Answer::Allow(false));
        }
        Action::None
    }

    fn scroll(&mut self, lines: i32) -> bool {
        self.picker.as_mut().is_some_and(|p| p.scroll(lines))
    }

    fn close(&mut self) {
        // Closing a question says no; closing the picker cancels.
        let a = if self.picker.is_some() { Answer::Cancel } else { Answer::Allow(false) };
        self.answer(a);
    }

    fn alive(&self) -> bool {
        let r = self.req.lock();
        r.answer.is_none() && !r.done
    }
}
