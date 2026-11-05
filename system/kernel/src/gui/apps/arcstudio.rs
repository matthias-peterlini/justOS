//! ArcStudio: the ArcScript IDE, as in ArcScript's own `arc-studio-ide`. A
//! project explorer at the left; the editor with line numbers, syntax
//! colours and errors underlined as you type; the live preview of the
//! program's `view` at the right, which runs the program in the window and
//! answers clicks; the console (what `print` wrote) and the problems below.
//!
//! The compiler and the VM are the same as `arcc`'s (`arcc-core`,
//! `arc-vm`), linked into the kernel: the preview needs no program of its
//! own. Run in the terminal and Build go through `arcc`.

use super::textedit::free_name;
use crate::dev::input::{Key, KeyEvent};
use crate::fs;
use crate::gui::gfx::{Canvas, Rect};
use crate::gui::text::{self, Font};
use crate::gui::textarea::TextArea;
use crate::gui::ui::{self, TEXT, TEXT_DIM, TEXT_FAINT};
use crate::gui::wm::{Action, App, GAP};
use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use arc_vm::ui::{Attr, Color, ColorToken, UiKind};
use arc_vm::{ActionId, Host, Limits, UiEvent, UiNode, UiValue, Vm};
use arcc_core::highlight::{highlight, HighlightKind};
use arcc_core::{compile_with_modules, CompileOptions, Diagnostic, Severity, SourceFile};
use core::cell::RefCell;

/// Where the examples and new projects live.
const HOME: &str = "sys:/home/arcscript";
const SIDE: i32 = 210;
const TOOLBAR: i32 = 52;
const BOTTOM: i32 = 176;
const PREVIEW_W: i32 = 400;
const ROW_H: i32 = 30;
/// Instructions a run, an event or a redraw of the view may take.
const STEPS: u64 = 3_000_000;
/// Lines of output kept.
const KEEP: usize = 400;

const NEW_FILE: &str = "# Un programma ArcScript\n\nlet clicks = 0\n\nfn click()\n    clicks += 1\n\nprint(\"Ciao da JustOS!\")\n\nview\n    window \"Nuovo\" width: 320\n        column gap: 12, align: \"center\"\n            label \"Hai cliccato {clicks} volte\"\n            button \"Clicca\" style: \"filled\" -> click()\n";

/// One row of the explorer.
struct Row {
    path: String,
    name: String,
    depth: i32,
    dir: bool,
}

struct Problem {
    error: bool,
    /// The file as diagnostics name it (relative to the project).
    file: String,
    line: u32,
    col: u32,
    end: (u32, u32),
    message: String,
}

/// What a click on the preview does.
#[derive(Clone)]
enum Hit {
    Click(ActionId),
    Bool(ActionId, bool),
    /// A slider: min, max, step.
    Slide(ActionId, f64, f64, f64),
    /// The nth input, and its text.
    Input(ActionId, usize, String),
}

/// The output of the program, shared with its host.
type Output = Rc<RefCell<Vec<String>>>;

struct StudioHost {
    out: Output,
    /// Relative paths are taken from here.
    dir: String,
}

impl StudioHost {
    fn resolve(&self, path: &str) -> String {
        if path.contains(':') {
            String::from(path)
        } else {
            join(&self.dir, path)
        }
    }
}

impl Host for StudioHost {
    fn print(&mut self, s: &str) {
        let mut out = self.out.borrow_mut();
        for line in s.strip_suffix('\n').unwrap_or(s).split('\n') {
            out.push(String::from(line));
        }
        let n = out.len();
        if n > KEEP {
            out.drain(..n - KEEP);
        }
    }

    fn now(&mut self) -> f64 {
        crate::time::unix_now() as f64
    }

    fn clock_ms(&mut self) -> u64 {
        crate::time::uptime_ms()
    }

    fn read_file(&mut self, path: &str) -> Result<Vec<u8>, String> {
        let p = self.resolve(path);
        fs::abs(&p).and_then(|p| fs::read_file(&p)).map_err(|e| format!("{path}: {e}"))
    }
}

/// The folder part of a path, keeping the volume's root.
fn parent(p: &str) -> &str {
    match p.trim_end_matches('/').rfind('/') {
        Some(i) if p[..i].ends_with(':') => &p[..=i],
        Some(i) => &p[..i],
        None => "",
    }
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        String::from(name)
    } else if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

fn file_name(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

fn read(path: &str) -> Option<String> {
    let bytes = fs::abs(path).and_then(|p| fs::read_file(&p)).ok()?;
    Some(String::from_utf8_lossy(&bytes).replace("\r\n", "\n"))
}

fn exists(path: &str) -> bool {
    fs::abs(path).is_ok_and(|p| fs::exists(&p))
}

/// Names in a folder (folders first, then by name), and whether each is a
/// folder.
fn list(dir: &str) -> Vec<(String, bool)> {
    let Ok(p) = fs::abs(dir) else { return Vec::new() };
    let mut v: Vec<(String, bool)> = fs::list_dir(&p)
        .map(|l| l.into_iter().map(|(e, st)| (e.name, st.kind == justfs::Kind::Dir)).collect())
        .unwrap_or_default();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
    v
}

/// The project a file belongs to (the nearest folder with an `Arc.toml`)
/// and its entry file.
fn find_project(path: &str) -> Option<(String, String)> {
    let mut dir = parent(path);
    for _ in 0..4 {
        if dir.is_empty() {
            break;
        }
        let manifest = join(dir, "Arc.toml");
        if let Some(text) = read(&manifest) {
            let mut entry = String::from("src/main.as");
            let mut section = "";
            for line in text.lines().map(str::trim) {
                if let Some(s) = line.strip_prefix('[') {
                    section = s.trim_end_matches(']').trim();
                } else if section == "package" {
                    if let Some((k, v)) = line.split_once('=') {
                        if k.trim() == "entry" {
                            entry = String::from(v.split('#').next().unwrap_or("").trim().trim_matches('"'));
                        }
                    }
                }
            }
            return Some((String::from(dir), join(dir, &entry)));
        }
        dir = parent(dir);
    }
    None
}

/// Colours of the syntax, on the editor's dark field.
fn syntax_color(k: HighlightKind) -> u32 {
    match k {
        HighlightKind::Keyword => 0xFFC5_86C0,
        HighlightKind::UiPrimitive => 0xFF4F_C1FF,
        HighlightKind::Attribute => 0xFF9C_DCFE,
        HighlightKind::Identifier => 0xFFE6_E1E5,
        HighlightKind::Function => 0xFFDC_DCAA,
        HighlightKind::Builtin => 0xFF4E_C9B0,
        HighlightKind::Number => 0xFFB5_CEA8,
        HighlightKind::String => 0xFFF0_A57C,
        HighlightKind::Constant => 0xFF56_9CD6,
        HighlightKind::Operator => 0xFFD4_D4D4,
        HighlightKind::Punctuation => 0xFFA0_A0A8,
        HighlightKind::Comment => 0xFF6A_9955,
    }
}

/// The character index of `line`:`col` (both from 1) in `text`.
fn char_index(text: &str, line: u32, col: u32) -> usize {
    let mut n = 1;
    let mut start = 0;
    for (i, c) in text.chars().enumerate() {
        if n == line {
            start = i;
            break;
        }
        if c == '\n' {
            n += 1;
            start = i + 1;
        }
    }
    start + col.saturating_sub(1) as usize
}

pub struct ArcStudio {
    /// The file in the editor.
    path: String,
    /// The project folder, or the folder of a single file.
    root: String,
    /// The file that is compiled and run.
    entry: String,
    area: TextArea,
    rows: Vec<Row>,
    /// `area.edits` when the text was last coloured and compiled.
    colored_at: u64,
    compiled_at: u64,
    /// Ticks since the text last changed.
    idle: u32,
    /// The last tick compiled the program again (errors and colours changed).
    recompiled: bool,
    problems: Vec<Problem>,
    out: Output,
    vm: Option<Vm>,
    view: Vec<UiNode>,
    run_error: Option<String>,
    /// The view is from an earlier version (the current one has errors).
    stale: bool,
    hits: Vec<(Rect, Hit)>,
    /// The input of the preview being typed into (by position).
    focus: Option<usize>,
    sliding: Option<(Rect, ActionId, f64, f64, f64)>,
    /// 0: console, 1: problems.
    tab: usize,
    preview: bool,
    pscroll: i32,
    pointer: (i32, i32),
    hover: Option<usize>,
    note: String,
    /// The editor's rectangle when last drawn.
    tr: Rect,
}

impl ArcStudio {
    pub fn new(path: Option<&str>) -> ArcStudio {
        let mut s = ArcStudio {
            path: String::new(),
            root: String::new(),
            entry: String::new(),
            area: TextArea::new(""),
            rows: Vec::new(),
            colored_at: u64::MAX,
            compiled_at: u64::MAX,
            idle: 0,
            recompiled: false,
            problems: Vec::new(),
            out: Rc::new(RefCell::new(Vec::new())),
            vm: None,
            view: Vec::new(),
            run_error: None,
            stale: false,
            hits: Vec::new(),
            focus: None,
            sliding: None,
            tab: 0,
            preview: true,
            pscroll: 0,
            pointer: (0, 0),
            hover: None,
            note: String::new(),
            tr: Rect::new(0, 0, 600, 400),
        };
        s.area.code = true;
        s.area.font = Font::Mono;
        let start = match path {
            Some(p) => String::from(p),
            None => {
                let counter = join(HOME, "counter/src/main.as");
                if exists(&counter) {
                    counter
                } else {
                    s.new_file_path()
                }
            }
        };
        s.open(&start);
        s
    }

    /// A free name for a new file, in the project's source folder.
    fn new_file_path(&self) -> String {
        let dir = if self.entry.is_empty() { HOME } else { parent(&self.entry) };
        if let Ok(d) = fs::abs(dir) {
            if !fs::exists(&d) {
                let _ = fs::make_dir(&d);
            }
        }
        free_name(dir, "nuovo", "as")
    }

    fn open(&mut self, path: &str) {
        if self.area.dirty {
            self.save();
        }
        let text = match read(path) {
            Some(t) => t,
            None => {
                // A new file starts from the example.
                let _ = fs::abs(path).and_then(|p| fs::write_file(&p, NEW_FILE.as_bytes()));
                String::from(NEW_FILE)
            }
        };
        self.path = String::from(path);
        match find_project(path) {
            Some((root, entry)) => {
                self.root = root;
                self.entry = entry;
            }
            None => {
                self.root = String::from(parent(path));
                self.entry = String::from(path);
            }
        }
        self.area.set_text(&text);
        self.focus = None;
        self.pscroll = 0;
        self.refresh_tree();
        self.compile();
    }

    fn save(&mut self) {
        let r = fs::abs(&self.path).and_then(|p| fs::write_file(&p, self.area.text().as_bytes()));
        match r {
            Ok(()) => {
                self.area.mark_saved();
                self.note = String::from("Salvato");
            }
            Err(e) => self.note = format!("Non salvato: {e}"),
        }
    }

    fn is_project(&self) -> bool {
        self.entry != self.path || exists(&join(&self.root, "Arc.toml"))
    }

    /// The path as the compiler names it: relative to the project.
    fn display(&self, path: &str) -> String {
        String::from(path.strip_prefix(self.root.as_str()).unwrap_or(path).trim_start_matches('/'))
    }

    fn refresh_tree(&mut self) {
        let mut rows = Vec::new();
        if !self.path.starts_with(HOME) {
            rows.push(Row { path: self.path.clone(), name: String::from(file_name(&self.path)), depth: 0, dir: false });
        }
        for (name, dir) in list(HOME) {
            let p = join(HOME, &name);
            if dir {
                if !exists(&join(&p, "Arc.toml")) {
                    continue;
                }
                let open = p == self.root;
                rows.push(Row { path: p.clone(), name, depth: 0, dir: true });
                if open {
                    let src = String::from(parent(&self.entry));
                    for (f, d) in list(&src) {
                        if !d && f.ends_with(".as") {
                            rows.push(Row { path: join(&src, &f), name: f, depth: 1, dir: false });
                        }
                    }
                    rows.push(Row { path: join(&p, "Arc.toml"), name: String::from("Arc.toml"), depth: 1, dir: false });
                }
            } else if name.ends_with(".as") {
                rows.push(Row { path: p, name, depth: 0, dir: false });
            }
        }
        self.rows = rows;
    }

    /// Colours the text by its tokens.
    fn color(&mut self) {
        self.colored_at = self.area.edits;
        let text = self.area.text();
        if !self.path.ends_with(".as") {
            self.area.colors.clear();
            return;
        }
        let spans = highlight(&text);
        let mut colors = Vec::with_capacity(text.len());
        let mut k = 0;
        for (b, _) in text.char_indices() {
            let b = b as u32;
            while k < spans.len() && spans[k].span.end <= b {
                k += 1;
            }
            colors.push(match spans.get(k) {
                Some(s) if s.span.start <= b => syntax_color(s.kind),
                _ => 0xFFD4_D4D4,
            });
        }
        self.area.colors = colors;
    }

    fn problem(d: &Diagnostic, source: &SourceFile) -> Problem {
        let (line, col) = source.line_col(d.span.start);
        let end = source.line_col(d.span.end.max(d.span.start));
        Problem {
            error: d.severity == Severity::Error,
            file: String::from(source.name_at(d.span.start)),
            line,
            col,
            end,
            message: d.message.clone(),
        }
    }

    /// Compiles the project (with the editor's text for the open file) and,
    /// when that works, runs it again.
    fn compile(&mut self) {
        self.compiled_at = self.area.edits;
        self.idle = 0;
        if !self.path.ends_with(".as") {
            return;
        }
        let text = self.area.text();
        let main = if self.path == self.entry { text.clone() } else { read(&self.entry).unwrap_or_default() };
        let dir = String::from(parent(&self.entry));
        let main_name = self.display(&self.entry);
        let prefix = String::from(parent(&main_name));
        let current = self.path.clone();
        let mut load = |module: &str| -> Result<(String, String), String> {
            let file = format!("{module}.as");
            let full = join(&dir, &file);
            let name = join(&prefix, &file);
            if full == current {
                return Ok((name, text.clone()));
            }
            read(&full).map(|t| (name, t)).ok_or_else(|| format!("non trovo il modulo `{module}`: manca {module}.as accanto a questo file"))
        };
        let result = compile_with_modules(&main_name, &main, CompileOptions::DEBUG, &mut load);
        let program = match result {
            Ok(out) => {
                self.problems = out.warnings.iter().map(|d| Self::problem(d, &out.source)).collect();
                Some(out.program)
            }
            Err(f) => {
                self.problems = f.diagnostics.iter().map(|d| Self::problem(d, &f.source)).collect();
                None
            }
        };
        // Errors in this file are underlined.
        let here = self.display(&self.path);
        let editor_text = self.area.text();
        self.area.squiggles = self
            .problems
            .iter()
            .filter(|p| p.error && p.file == here)
            .map(|p| {
                let a = char_index(&editor_text, p.line, p.col);
                let b = char_index(&editor_text, p.end.0, p.end.1).max(a + 1);
                (a, b)
            })
            .collect();
        match program {
            Some(p) => {
                self.stale = false;
                self.start(p);
            }
            None => {
                self.stale = !self.view.is_empty();
                if self.problems.iter().any(|p| p.error) && self.tab == 0 && self.view.is_empty() {
                    self.tab = 1;
                }
            }
        }
    }

    fn start(&mut self, program: arcc_core::Program) {
        self.out.borrow_mut().clear();
        self.run_error = None;
        self.focus = None;
        let host = StudioHost { out: self.out.clone(), dir: self.root.clone() };
        let mut vm = match Vm::with_host(program, Box::new(host)) {
            Ok(vm) => vm,
            Err(e) => {
                self.run_error = Some(format!("{e:?}"));
                self.vm = None;
                return;
            }
        };
        vm.set_limits(Limits { max_steps: Some(STEPS), max_frames: 2_000 });
        if let Err(e) = vm.run_main() {
            self.run_error = Some(e.message.clone());
            self.vm = None;
            self.view.clear();
            return;
        }
        if vm.has_view() {
            self.vm = Some(vm);
            self.render();
        } else {
            self.vm = None;
            self.view.clear();
        }
    }

    fn render(&mut self) {
        if let Some(vm) = &mut self.vm {
            match vm.render_view() {
                Ok(v) => self.view = v,
                Err(e) => self.run_error = Some(e.message.clone()),
            }
        }
    }

    fn dispatch(&mut self, ev: UiEvent) {
        if let Some(vm) = &mut self.vm {
            if let Err(e) = vm.dispatch(ev) {
                self.run_error = Some(e.message.clone());
            }
        }
        self.render();
    }

    /// Run again from the start, with the file saved.
    fn run(&mut self) {
        if self.area.dirty {
            self.save();
        }
        self.compile();
        self.tab = if self.problems.iter().any(|p| p.error) { 1 } else { 0 };
    }

    /// What `arcc` gets: the project folder or the file.
    fn target(&self) -> String {
        let t = if self.is_project() { self.root.clone() } else { self.path.clone() };
        if t.contains(' ') {
            format!("\"{t}\"")
        } else {
            t
        }
    }

    // Layout, from the content area.

    fn main(area: Rect) -> Rect {
        Rect::new(area.x + SIDE + GAP, area.y, area.w - SIDE - GAP, area.h)
    }

    fn buttons(area: Rect) -> [Rect; 5] {
        let m = Self::main(area);
        let y = m.y + 10;
        [
            Rect::new(m.x + 12, y, 104, 32),
            Rect::new(m.x + 124, y, 92, 32),
            Rect::new(m.x + 224, y, 120, 32),
            Rect::new(m.x + 352, y, 80, 32),
            Rect::new(m.right() - 128, y, 116, 32),
        ]
    }

    fn show_preview(&self, area: Rect) -> bool {
        self.preview && Self::main(area).w > PREVIEW_W + 300
    }

    fn editor_rect(&self, area: Rect) -> Rect {
        let m = Self::main(area);
        let w = m.w - 20 - if self.show_preview(area) { PREVIEW_W + 10 } else { 0 };
        Rect::new(m.x + 10, m.y + TOOLBAR, w, m.h - TOOLBAR - BOTTOM)
    }

    fn preview_rect(&self, area: Rect) -> Rect {
        let e = self.editor_rect(area);
        Rect::new(e.right() + 10, e.y, PREVIEW_W, e.h)
    }

    fn bottom_rect(&self, area: Rect) -> Rect {
        let m = Self::main(area);
        let e = self.editor_rect(area);
        Rect::new(m.x + 10, e.bottom() + 8, m.w - 20, m.bottom() - e.bottom() - 16)
    }

    fn tabs_rect(&self, area: Rect) -> Rect {
        let b = self.bottom_rect(area);
        Rect::new(b.x + 8, b.y + 8, 250, 30)
    }

    fn row_rect(area: Rect, i: usize) -> Rect {
        Rect::new(area.x + 8, area.y + 46 + i as i32 * ROW_H, SIDE - 16, ROW_H - 2)
    }

    fn new_button(area: Rect) -> Rect {
        Rect::new(area.x + 12, area.bottom() - 48, SIDE - 24, 34)
    }

    fn problem_rows(&self, area: Rect) -> Rect {
        let b = self.bottom_rect(area);
        Rect::new(b.x + 8, b.y + 46, b.w - 16, b.h - 52)
    }

    // Drawing.

    fn draw_sidebar(&self, c: &mut Canvas, area: Rect) {
        ui::caps(c, area.x + 18, area.y + 18, "Esplora", TEXT_DIM);
        let max = ((Self::new_button(area).y - area.y - 52) / ROW_H).max(0) as usize;
        for (i, row) in self.rows.iter().take(max).enumerate() {
            let r = Self::row_rect(area, i);
            let current = row.path == self.path || (row.dir && row.path == self.root);
            if row.path == self.path {
                c.fill_round(r, 8, ui::SELECTED);
            }
            let x = r.x + 10 + row.depth * 16;
            let (lh, _) = text::metrics(Font::Body);
            let ty = r.y + (r.h - lh) / 2;
            let mark = if row.dir { if row.path == self.root { "−" } else { "+" } } else { "·" };
            text::draw(c, Font::Strong, x, ty, mark, TEXT_FAINT);
            let font = if row.dir { Font::Strong } else { Font::Body };
            let name = text::fit(font, &row.name, r.right() - x - 22);
            text::draw(c, font, x + 16, ty, &name, if current { TEXT } else { TEXT_DIM });
        }
        ui::button(c, Self::new_button(area), "+ Nuovo file", false, self.hover == Some(10));
    }

    fn draw_toolbar(&self, c: &mut Canvas, area: Rect) {
        let labels = ["    Esegui", "Compila", "Nel terminale", "Salva", "Anteprima"];
        for (i, (b, label)) in Self::buttons(area).iter().zip(labels).enumerate() {
            let primary = i == 0 || (i == 4 && self.preview);
            ui::button(c, *b, label, primary, self.hover == Some(i));
        }
        // The font has no ▶: a triangle drawn by hand.
        let b = Self::buttons(area)[0];
        let (x, y) = (b.x + 20, b.y + b.h / 2);
        for k in 0..10 {
            let half = (10 - k) * 5 / 10;
            c.fill(Rect::new(x + k, y - half, 1, 2 * half + 1), 0xFF0E_0B0C);
        }
        let m = Self::main(area);
        if !self.note.is_empty() {
            text::draw_right(c, Font::Small, Self::buttons(area)[4].x - 16, m.y + 20, &self.note, TEXT_DIM);
        }
    }

    fn draw_bottom(&self, c: &mut Canvas, area: Rect) {
        let b = self.bottom_rect(area);
        c.fill_round(b, 14, 0xF214_1018);
        let errors = self.problems.iter().filter(|p| p.error).count();
        let label = format!("Problemi ({})", self.problems.len());
        ui::segmented(c, self.tabs_rect(area), &["Console", &label], self.tab);
        // The status at the right: errors, then where the cursor is.
        let (line, col) = self.area.cursor_line_col();
        let pos = format!("Riga {line}, col {col}");
        let (lh, _) = text::metrics(Font::Small);
        let sy = b.y + 8 + (30 - lh) / 2;
        let w = text::width(Font::Small, &pos);
        text::draw_right(c, Font::Small, b.right() - 16, sy, &pos, TEXT_FAINT);
        let (status, color) = if errors > 0 {
            (if errors == 1 { String::from("1 errore") } else { format!("{errors} errori") }, ui::RED)
        } else if self.run_error.is_some() {
            (String::from("Errore durante l'esecuzione"), ui::RED)
        } else {
            (String::from("Nessun problema"), ui::GREEN)
        };
        let sx = b.right() - 16 - w - 24;
        text::draw_right(c, Font::Small, sx, sy, &status, color);
        c.fill_circle(sx - text::width(Font::Small, &status) - 10, sy + lh / 2, 4, color);

        let body = self.problem_rows(area);
        let saved = c.clip();
        c.set_clip(saved.intersect(&body));
        if self.tab == 0 {
            let (lh, _) = text::metrics(Font::Mono);
            let out = self.out.borrow();
            let mut lines: Vec<(&str, u32)> = out.iter().map(|l| (l.as_str(), 0xFFE6_E1E5)).collect();
            if let Some(e) = &self.run_error {
                lines.push((e.as_str(), ui::RED));
            }
            if lines.is_empty() {
                let hint = if self.vm.is_some() { "Il programma non ha scritto niente con print()." } else { "L'output di print() compare qui." };
                text::draw(c, Font::Small, body.x + 4, body.y + 2, hint, TEXT_FAINT);
            }
            let fit = (body.h / lh).max(1) as usize;
            let skip = lines.len().saturating_sub(fit);
            for (i, (l, color)) in lines.iter().skip(skip).enumerate() {
                text::draw(c, Font::Mono, body.x + 4, body.y + i as i32 * lh, l, *color);
            }
        } else {
            if self.problems.is_empty() {
                text::draw(c, Font::Small, body.x + 4, body.y + 2, "Nessun problema nel progetto.", TEXT_FAINT);
            }
            for (i, p) in self.problems.iter().enumerate() {
                let y = body.y + i as i32 * 26;
                if y > body.bottom() {
                    break;
                }
                let (mark, color) = if p.error { ("×", ui::RED) } else { ("!", ui::ORANGE) };
                text::draw(c, Font::Strong, body.x + 6, y + 2, mark, color);
                let at = format!("{}:{}:{}", p.file, p.line, p.col);
                let w = text::draw(c, Font::Small, body.x + 24, y + 4, &at, TEXT_FAINT);
                let msg = text::fit(Font::Body, &p.message, body.right() - body.x - 40 - w);
                text::draw(c, Font::Body, body.x + 36 + w, y + 2, &msg, TEXT);
            }
        }
        c.set_clip(saved);
    }

    fn draw_preview(&mut self, c: &mut Canvas, area: Rect) {
        let r = self.preview_rect(area);
        c.fill_round(r, 14, 0xF20E_0C12);
        ui::caps(c, r.x + 16, r.y + 14, "Anteprima dal vivo", TEXT_FAINT);
        let inner = Rect::new(r.x + 12, r.y + 38, r.w - 24, r.h - 46);
        self.hits.clear();
        if self.view.is_empty() {
            let msg = match (&self.run_error, self.problems.iter().any(|p| p.error)) {
                (Some(e), _) => format!("Il programma si è fermato: {e}"),
                (None, true) => String::from("Correggi gli errori per vedere l'anteprima."),
                (None, false) if self.path.ends_with(".as") => {
                    String::from("Questo programma non ha una view: il suo output è nella console. Aggiungi un blocco «view» per disegnare una finestra.")
                }
                _ => String::from("Apri un file .as per vederne l'anteprima."),
            };
            let (lh, _) = text::metrics(Font::Body);
            for (i, l) in text::wrap(Font::Body, &msg, inner.w - 16).iter().enumerate() {
                text::draw(c, Font::Body, inner.x + 8, inner.y + 8 + i as i32 * lh, l, TEXT_DIM);
            }
            return;
        }
        let pal = Palette::of(self.view.first());
        let mut cx = Ctx { pal, hits: Vec::new(), focus: self.focus, inputs: 0 };
        let saved = c.clip();
        c.set_clip(saved.intersect(&inner));
        let mut y = inner.y + 4 - self.pscroll;
        for node in &self.view {
            let (w, h) = measure(node, inner.w - 8);
            let x = inner.x + (inner.w - w) / 2;
            paint(c, node, Rect::new(x, y, w, h), &mut cx);
            y += h + 16;
        }
        // Not past the end.
        let total = y + self.pscroll - inner.y;
        if self.pscroll > 0 && total < inner.h {
            self.pscroll = 0;
        }
        c.set_clip(saved);
        if self.stale {
            let band = Rect::new(r.x + 12, r.bottom() - 40, r.w - 24, 30);
            c.fill_round(band, 10, 0xE07A_2222);
            text::draw_centered(c, Font::Small, band.x + band.w / 2, band.y + 7, "Errori nel codice: l'anteprima è dell'ultima versione buona", TEXT);
        }
        self.hits = cx.hits.into_iter().filter(|(h, _)| h.intersect(&inner).w > 0).collect();
    }

    fn press_preview(&mut self, ax: i32, ay: i32) -> bool {
        let Some((r, hit)) = self.hits.iter().rev().find(|(r, _)| r.contains(ax, ay)).cloned() else {
            let had = self.focus.take().is_some();
            return had;
        };
        self.focus = None;
        match hit {
            Hit::Click(id) => self.dispatch(UiEvent::Click(id)),
            Hit::Bool(id, v) => self.dispatch(UiEvent::Change(id, UiValue::Bool(!v))),
            Hit::Slide(id, min, max, step) => {
                self.sliding = Some((r, id, min, max, step));
                self.slide(ax);
            }
            Hit::Input(_, n, _) => self.focus = Some(n),
        }
        true
    }

    fn slide(&mut self, ax: i32) {
        let Some((r, id, min, max, step)) = self.sliding else { return };
        let t = ((ax - r.x) as f64 / r.w.max(1) as f64).clamp(0.0, 1.0);
        let mut v = min + (max - min) * t;
        if step > 0.0 {
            v = min + libm_round((v - min) / step) * step;
        }
        self.dispatch(UiEvent::Change(id, UiValue::Float(v)));
    }

    /// The focused input of the preview, and its text.
    fn focused_input(&self) -> Option<(ActionId, String)> {
        let n = self.focus?;
        self.hits.iter().find_map(|(_, h)| match h {
            Hit::Input(id, k, t) if *k == n => Some((*id, t.clone())),
            _ => None,
        })
    }
}

/// Rounds to the nearest whole number (no `f64::round` without std).
fn libm_round(x: f64) -> f64 {
    let t = x as i64 as f64;
    if x - t >= 0.5 {
        t + 1.0
    } else if t - x >= 0.5 {
        t - 1.0
    } else {
        t
    }
}

impl App for ArcStudio {
    fn title(&self) -> String {
        let name = file_name(&self.path);
        if self.area.dirty {
            format!("ArcStudio — {name} •")
        } else {
            format!("ArcStudio — {name}")
        }
    }

    fn icon(&self) -> &str {
        "code"
    }

    fn kind(&self) -> &'static str {
        "arcstudio"
    }

    fn size(&self) -> (i32, i32) {
        (1120, 680)
    }

    fn resizable(&self) -> bool {
        true
    }

    fn sidebar(&self) -> i32 {
        SIDE
    }

    fn draw(&mut self, c: &mut Canvas, area: Rect, _blur: &Canvas, focused: bool) {
        if self.colored_at != self.area.edits {
            self.color();
        }
        self.draw_sidebar(c, area);
        self.draw_toolbar(c, area);
        let tr = self.editor_rect(area);
        self.tr = tr;
        c.fill_round(tr, 14, 0xF21C_1622);
        self.area.draw(c, tr, focused && self.focus.is_none());
        if self.show_preview(area) {
            self.draw_preview(c, area);
        } else {
            self.hits.clear();
        }
        self.draw_bottom(c, area);
    }

    fn press(&mut self, x: i32, y: i32, double: bool, area: Rect) -> Action {
        let (ax, ay) = (area.x + x, area.y + y);
        if x < SIDE {
            if Self::new_button(area).contains(ax, ay) {
                let p = self.new_file_path();
                self.open(&p);
                return Action::Redraw;
            }
            let i = (0..self.rows.len()).find(|&i| Self::row_rect(area, i).contains(ax, ay));
            if let Some(i) = i {
                let row = &self.rows[i];
                let target = if row.dir {
                    find_project(&join(&row.path, "Arc.toml")).map(|(_, e)| e)
                } else if row.path.ends_with(".as") {
                    Some(row.path.clone())
                } else {
                    // Arc.toml and the like: the text editor.
                    return Action::OpenFile(row.path.clone());
                };
                if let Some(t) = target {
                    if t != self.path {
                        self.open(&t);
                    }
                }
                return Action::Redraw;
            }
            return Action::None;
        }
        let b = Self::buttons(area);
        if b[0].contains(ax, ay) {
            self.run();
            return Action::Redraw;
        }
        if b[1].contains(ax, ay) {
            self.save();
            return Action::Run(format!("arcc build {}", self.target()));
        }
        if b[2].contains(ax, ay) {
            self.save();
            return Action::Run(format!("arcc run {}", self.target()));
        }
        if b[3].contains(ax, ay) {
            self.save();
            self.refresh_tree();
            return Action::Redraw;
        }
        if b[4].contains(ax, ay) {
            self.preview = !self.preview;
            return Action::Redraw;
        }
        let tr = self.editor_rect(area);
        if tr.contains(ax, ay) {
            self.focus = None;
            self.area.press(tr, ax, ay, double, false);
            return Action::Redraw;
        }
        if self.show_preview(area) && self.preview_rect(area).contains(ax, ay) {
            return if self.press_preview(ax, ay) { Action::Redraw } else { Action::None };
        }
        let tabs = self.tabs_rect(area);
        if tabs.contains(ax, ay) {
            self.tab = if ax < tabs.x + tabs.w / 2 { 0 } else { 1 };
            return Action::Redraw;
        }
        let rows = self.problem_rows(area);
        if self.tab == 1 && rows.contains(ax, ay) {
            let i = ((ay - rows.y) / 26) as usize;
            if let Some(p) = self.problems.get(i) {
                let (file, line) = (p.file.clone(), p.line);
                if file != self.display(&self.path) {
                    let full = join(&self.root, &file);
                    if exists(&full) {
                        self.open(&full);
                    }
                }
                self.area.go_to_line(line as usize);
                self.focus = None;
            }
            return Action::Redraw;
        }
        Action::None
    }

    fn drag(&mut self, x: i32, y: i32, area: Rect) -> bool {
        if self.sliding.is_some() {
            self.slide(area.x + x);
            return true;
        }
        self.area.drag(self.editor_rect(area), area.x + x, area.y + y)
    }

    fn release(&mut self, _area: Rect) -> Action {
        self.area.release();
        self.sliding = None;
        Action::None
    }

    fn hover(&mut self, at: Option<(i32, i32)>, area: Rect) -> bool {
        if let Some((x, y)) = at {
            self.pointer = (area.x + x, area.y + y);
        }
        let h = at.and_then(|(x, y)| {
            let (ax, ay) = (area.x + x, area.y + y);
            if Self::new_button(area).contains(ax, ay) {
                return Some(10);
            }
            Self::buttons(area).iter().position(|b| b.contains(ax, ay))
        });
        let changed = h != self.hover;
        self.hover = h;
        changed
    }

    fn key(&mut self, ev: KeyEvent) -> Action {
        match ev.key {
            Key::F(5) => {
                self.run();
                return Action::Redraw;
            }
            Key::Char(ch) if ev.ctrl => match ch.to_ascii_lowercase() {
                's' => {
                    self.save();
                    self.refresh_tree();
                    return Action::Redraw;
                }
                'r' => {
                    self.run();
                    return Action::Redraw;
                }
                'b' => {
                    self.save();
                    return Action::Run(format!("arcc build {}", self.target()));
                }
                'n' => {
                    let p = self.new_file_path();
                    self.open(&p);
                    return Action::Redraw;
                }
                _ => {}
            },
            Key::Escape if self.focus.is_some() => {
                self.focus = None;
                return Action::Redraw;
            }
            _ => {}
        }
        if let Some((id, mut text)) = self.focused_input() {
            let changed = match ev.key {
                Key::Char(ch) if ev.ctrl && ch.eq_ignore_ascii_case(&'v') => {
                    text.push_str(&ui::paste());
                    true
                }
                _ => ui::edit_text(&mut text, &ev, 500),
            };
            if changed {
                self.dispatch(UiEvent::Change(id, UiValue::Text(text)));
            }
            return Action::Redraw;
        }
        if self.area.key(ev, self.tr) {
            self.note.clear();
            self.idle = 0;
            return Action::Redraw;
        }
        Action::None
    }

    fn scroll(&mut self, lines: i32) -> bool {
        let (px, py) = self.pointer;
        let e = self.tr;
        if px > e.right() && py < e.bottom() && self.preview {
            let old = self.pscroll;
            self.pscroll = (self.pscroll + lines * 40).max(0);
            return old != self.pscroll;
        }
        self.area.scroll_by(lines)
    }

    fn tick(&mut self) -> bool {
        let blinked = self.focus.is_none() && self.area.tick();
        self.idle += 1;
        // Compiled again a moment after the typing stops.
        self.recompiled = false;
        if self.compiled_at != self.area.edits && self.idle >= 3 {
            self.compile();
            self.recompiled = true;
        }
        blinked || self.recompiled
    }

    fn changed(&mut self, _area: Rect) -> Option<Rect> {
        // Errors and colours change after a compilation: all of it then.
        (!self.recompiled).then(|| self.area.caret())
    }

    fn close(&mut self) {
        if self.area.dirty {
            self.save();
        }
    }
}

// The preview: ArcScript's elements drawn like the IDE's Material renderer.

/// Colours of the view, from its window's accent and theme.
struct Palette {
    dark: bool,
    primary: u32,
}

fn rgb(c: u32) -> (i32, i32, i32) {
    (((c >> 16) & 0xFF) as i32, ((c >> 8) & 0xFF) as i32, (c & 0xFF) as i32)
}

/// `a` mixed with `b`, `t` thousandths of `b`.
fn mix(a: u32, b: u32, t: i32) -> u32 {
    let (ar, ag, ab) = rgb(a);
    let (br, bg, bb) = rgb(b);
    let m = |x: i32, y: i32| (x + (y - x) * t / 1000).clamp(0, 255) as u32;
    0xFF00_0000 | (m(ar, br) << 16) | (m(ag, bg) << 8) | m(ab, bb)
}

fn luma(c: u32) -> i32 {
    let (r, g, b) = rgb(c);
    (r * 299 + g * 587 + b * 114) / 1000
}

impl Palette {
    fn of(root: Option<&UiNode>) -> Palette {
        let dark = root.and_then(|n| n.choice(Attr::Theme)).is_none_or(|t| t != "light");
        let accent = match root.and_then(|n| n.color(Attr::Accent)) {
            Some(Color::Rgba([r, g, b, _])) => 0xFF00_0000 | (r as u32) << 16 | (g as u32) << 8 | b as u32,
            _ => 0xFF67_50A4,
        };
        // Light enough to read on the dark surface, dark enough on the light.
        let l = luma(accent);
        let primary = if dark && l < 170 {
            mix(accent, 0xFFFF_FFFF, (170 - l) * 1000 / (255 - l).max(1))
        } else if !dark && l > 110 {
            mix(accent, 0xFF00_0000, (l - 110) * 1000 / l.max(1))
        } else {
            accent
        };
        Palette { dark, primary }
    }

    fn token(&self, t: ColorToken) -> u32 {
        let d = self.dark;
        let p = self.primary;
        let surface = if d { 0xFF14_1218 } else { 0xFFFE_F7FF };
        let on_surface = if d { 0xFFE6_E0E9 } else { 0xFF1D_1B20 };
        let on_p = if luma(p) > 140 { mix(p, 0xFF00_0000, 780) } else { 0xFFFF_FFFF };
        let toward = |t: i32| mix(p, surface, t);
        match t {
            ColorToken::Primary => p,
            ColorToken::OnPrimary => on_p,
            ColorToken::PrimaryContainer => toward(620),
            ColorToken::OnPrimaryContainer => if d { mix(p, 0xFFFF_FFFF, 600) } else { mix(p, 0xFF00_0000, 600) },
            ColorToken::Secondary => mix(p, 0xFF80_8080, 450),
            ColorToken::OnSecondary => on_p,
            ColorToken::SecondaryContainer => toward(760),
            ColorToken::OnSecondaryContainer => on_surface,
            ColorToken::Tertiary => mix(p, 0xFFEF_B8C8, 600),
            ColorToken::OnTertiary => on_p,
            ColorToken::TertiaryContainer => mix(mix(p, 0xFFEF_B8C8, 600), surface, 650),
            ColorToken::OnTertiaryContainer => on_surface,
            ColorToken::Error => if d { 0xFFF2_B8B5 } else { 0xFFB3_261E },
            ColorToken::OnError => if d { 0xFF60_1410 } else { 0xFFFF_FFFF },
            ColorToken::ErrorContainer => if d { 0xFF8C_1D18 } else { 0xFFF9_DEDC },
            ColorToken::OnErrorContainer => if d { 0xFFF9_DEDC } else { 0xFF41_0E0B },
            ColorToken::Surface | ColorToken::SurfaceDim | ColorToken::SurfaceBright => surface,
            ColorToken::OnSurface => on_surface,
            ColorToken::OnSurfaceVariant => if d { 0xFFCA_C4D0 } else { 0xFF49_454F },
            ColorToken::SurfaceContainerLowest => if d { 0xFF0F_0D13 } else { 0xFFFF_FFFF },
            ColorToken::SurfaceContainerLow => if d { 0xFF1D_1B20 } else { 0xFFF7_F2FA },
            ColorToken::SurfaceContainer => if d { 0xFF21_1F26 } else { 0xFFF3_EDF7 },
            ColorToken::SurfaceContainerHigh => if d { 0xFF2B_2930 } else { 0xFFEC_E6F0 },
            ColorToken::SurfaceContainerHighest => if d { 0xFF36_343B } else { 0xFFE6_E0E9 },
            ColorToken::Outline => if d { 0xFF93_8F99 } else { 0xFF79_747E },
            ColorToken::OutlineVariant => if d { 0xFF49_454F } else { 0xFFCA_C4D0 },
            ColorToken::InverseSurface => on_surface,
            ColorToken::InverseOnSurface => surface,
            #[allow(unreachable_patterns)]
            _ => p,
        }
    }

    fn color(&self, c: Color) -> u32 {
        match c {
            Color::Rgba([r, g, b, a]) => (a as u32) << 24 | (r as u32) << 16 | (g as u32) << 8 | b as u32,
            Color::Token { token, opacity } => {
                let v = self.token(token);
                let a = 255 * opacity.min(100) as u32 / 100;
                (a << 24) | (v & 0x00FF_FFFF)
            }
        }
    }
}

struct Ctx {
    pal: Palette,
    hits: Vec<(Rect, Hit)>,
    focus: Option<usize>,
    /// Inputs drawn so far.
    inputs: usize,
}

/// The font closest to a text size in points.
fn font_of(n: &UiNode) -> Font {
    if n.flag(Attr::Mono) == Some(true) {
        return Font::Mono;
    }
    let bold = n.choice(Attr::Weight).is_some_and(|w| w != "regular");
    let Some(size) = n.number(Attr::Size) else {
        return if bold { Font::Strong } else { Font::Body };
    };
    let want = (size * 1.3) as i32;
    let mut best = Font::Body;
    let mut gap = i32::MAX;
    for f in [Font::Small, Font::Body, Font::Heading, Font::Title, Font::Display, Font::Clock] {
        let d = (text::metrics(f).0 - want).abs();
        if d < gap {
            gap = d;
            best = f;
        }
    }
    match best {
        Font::Body if bold => Font::Strong,
        f => f,
    }
}

fn padding(n: &UiNode) -> i32 {
    n.number(Attr::Padding).map(|v| v as i32).unwrap_or(match n.kind {
        UiKind::Window => 20,
        UiKind::Card => 16,
        UiKind::Group if !n.text.is_empty() => 12,
        _ => 0,
    })
}

fn gap(n: &UiNode) -> i32 {
    n.number(Attr::Gap).map(|v| v as i32).unwrap_or(match n.kind {
        UiKind::Window => 12,
        _ => 8,
    })
}

/// Material Symbols drawn with the characters the font has.
fn icon_text(n: &UiNode) -> Option<&'static str> {
    Some(match n.icon()? as u32 {
        0xE145 => "+",
        0xE15B => "−",
        0xE5CD => "×",
        0xE5C8 => "→",
        0xE5CC => "›",
        0xE838 => "★",
        _ => return None,
    })
}

fn button_label(n: &UiNode) -> String {
    match icon_text(n) {
        Some(i) if n.text.is_empty() => String::from(i),
        Some(i) => format!("{i}  {}", n.text),
        // An icon the font cannot show: a dot, when there is nothing else.
        None if n.text.is_empty() && n.icon().is_some() => String::from("•"),
        None => n.text.clone(),
    }
}

/// Is the element laid out as a stack of its children, and across?
fn stack(kind: UiKind) -> Option<bool> {
    match kind {
        UiKind::Window | UiKind::Column | UiKind::Card | UiKind::Scroll | UiKind::Group | UiKind::Accordion => Some(false),
        UiKind::Row | UiKind::Split => Some(true),
        _ => None,
    }
}

/// The space above the children of an element (title bars, headers).
fn header(n: &UiNode) -> i32 {
    match n.kind {
        UiKind::Window => 40,
        UiKind::Accordion => 40,
        UiKind::Group if !n.text.is_empty() => 24,
        _ => 0,
    }
}

fn shows_children(n: &UiNode) -> bool {
    n.kind != UiKind::Accordion || n.flag(Attr::Open) == Some(true)
}

/// The size an element wants, at most `avail` wide.
fn measure(n: &UiNode, avail: i32) -> (i32, i32) {
    let fixed = n.number(Attr::Width).map(|v| (v as i32).min(avail));
    let (w, h) = natural(n, fixed.unwrap_or(avail));
    (fixed.unwrap_or(w).min(avail.max(0)), n.number(Attr::Height).map(|v| v as i32).unwrap_or(h).max(0))
}

fn natural(n: &UiNode, avail: i32) -> (i32, i32) {
    let lh = |f: Font| text::metrics(f).0;
    match n.kind {
        UiKind::Label => {
            let f = font_of(n);
            let w = text::width(f, &n.text);
            if w <= avail {
                (w, lh(f))
            } else {
                (avail, lh(f) * text::wrap(f, &n.text, avail).len().max(1) as i32)
            }
        }
        UiKind::Button => (text::width(Font::Strong, &button_label(n)) + 40, 40),
        UiKind::Input => (avail.min(260), 44),
        UiKind::Checkbox | UiKind::Switch => (text::width(Font::Body, &n.text) + if n.kind == UiKind::Switch { 62 } else { 30 }, 30),
        UiKind::Slider => (avail.min(240), 30),
        UiKind::Progress => (avail.min(240), 8),
        UiKind::Spacer => {
            let v = n.value.as_ref().and_then(|v| v.as_f64()).unwrap_or(0.0) as i32;
            (v, v)
        }
        UiKind::Divider => (0, 1),
        UiKind::Image => (avail.min(160), 100),
        UiKind::Grid => {
            let cols = n.number(Attr::Columns).map(|v| v as i32).unwrap_or(2).max(1);
            let (pad, g) = (padding(n), gap(n));
            let cell = ((avail - 2 * pad - g * (cols - 1)) / cols).max(0);
            let mut h = 0;
            for chunk in n.children.chunks(cols as usize) {
                h += chunk.iter().map(|k| measure(k, cell).1).max().unwrap_or(0) + g;
            }
            (avail, (h - g).max(0) + 2 * pad)
        }
        kind => {
            let across = stack(kind).unwrap_or(false);
            let (pad, g) = (padding(n), gap(n));
            let inner = avail - 2 * pad;
            let head = header(n);
            let mut head_w = 0;
            if head > 0 {
                head_w = text::width(Font::Strong, &n.text) + 60;
            }
            if !shows_children(n) || n.children.is_empty() {
                return (head_w.min(avail).max(if head > 0 { avail.min(240) } else { 0 }), head + if head > 0 { 0 } else { 2 * pad });
            }
            let count = n.children.len() as i32;
            let each = if kind == UiKind::Split { (inner - g * (count - 1)) / count.max(1) } else { inner };
            let sizes: Vec<(i32, i32)> = n.children.iter().map(|k| measure(k, each)).collect();
            let gaps = g * (count - 1);
            let (w, h) = if across {
                if kind == UiKind::Split {
                    (inner, sizes.iter().map(|s| s.1).max().unwrap_or(0))
                } else {
                    (sizes.iter().map(|s| s.0).sum::<i32>() + gaps, sizes.iter().map(|s| s.1).max().unwrap_or(0))
                }
            } else {
                (sizes.iter().map(|s| s.0).max().unwrap_or(0), sizes.iter().map(|s| s.1).sum::<i32>() + gaps)
            };
            let w = (w + 2 * pad).max(head_w).max(if n.kind == UiKind::Window { 240 } else { 0 });
            (w.min(avail), h + 2 * pad + head)
        }
    }
}

fn paint(c: &mut Canvas, n: &UiNode, r: Rect, cx: &mut Ctx) {
    let pal = &cx.pal;
    let on_surface = pal.token(ColorToken::OnSurface);
    let enabled = n.flag(Attr::Enabled) != Some(false);
    let color_attr = |a: Attr| n.color(a).map(|c| pal.color(c));
    let radius = n.number(Attr::Radius).map(|v| v as i32);
    match n.kind {
        UiKind::Label => {
            let f = font_of(n);
            let color = color_attr(Attr::Color).unwrap_or(on_surface);
            let lh = text::metrics(f).0;
            if text::width(f, &n.text) <= r.w {
                text::draw(c, f, r.x, r.y, &n.text, color);
            } else {
                for (i, l) in text::wrap(f, &n.text, r.w).iter().enumerate() {
                    text::draw(c, f, r.x, r.y + i as i32 * lh, l, color);
                }
            }
        }
        UiKind::Button => {
            let style = n.choice(Attr::Style).unwrap_or("tonal");
            let selected = n.flag(Attr::Selected) == Some(true);
            let (bg, fg, border) = match style {
                _ if selected => (pal.token(ColorToken::SecondaryContainer), pal.token(ColorToken::OnSecondaryContainer), 0),
                "filled" => (pal.token(ColorToken::Primary), pal.token(ColorToken::OnPrimary), 0),
                "outlined" => (0, pal.token(ColorToken::Primary), pal.token(ColorToken::Outline)),
                "text" => (0, pal.token(ColorToken::Primary), 0),
                "elevated" => (pal.token(ColorToken::SurfaceContainerLow), pal.token(ColorToken::Primary), 0),
                "light" | "frosted" => (0x33FF_FFFF, on_surface, 0x29FF_FFFF),
                _ => (pal.token(ColorToken::SecondaryContainer), pal.token(ColorToken::OnSecondaryContainer), 0),
            };
            let bg = color_attr(Attr::Fill).unwrap_or(bg);
            let fg = color_attr(Attr::Color).unwrap_or(fg);
            let rad = radius.unwrap_or(r.h / 2).min(r.h / 2);
            if style == "elevated" {
                c.shadow(r, rad, 6, 40);
            }
            if bg != 0 {
                c.fill_round(r, rad, if enabled { bg } else { (bg & 0x00FF_FFFF) | 0x1F00_0000 });
            }
            if border != 0 {
                c.stroke_round(r, rad, 1, border);
            }
            let (lh, _) = text::metrics(Font::Strong);
            let fg = if enabled { fg } else { (fg & 0x00FF_FFFF) | 0x6000_0000 };
            text::draw_centered(c, Font::Strong, r.x + r.w / 2, r.y + (r.h - lh) / 2, &button_label(n), fg);
            if enabled {
                if let Some(id) = n.action {
                    cx.hits.push((r, Hit::Click(id)));
                }
            }
        }
        UiKind::Checkbox => {
            let on = n.value.as_ref().and_then(|v| v.as_bool()).unwrap_or(false);
            let b = Rect::new(r.x + 2, r.y + (r.h - 18) / 2, 18, 18);
            if on {
                c.fill_round(b, 4, pal.token(ColorToken::Primary));
                let ink = pal.token(ColorToken::OnPrimary);
                c.line(b.x + 4, b.y + 9, b.x + 8, b.y + 13, 2, ink);
                c.line(b.x + 8, b.y + 13, b.x + 14, b.y + 5, 2, ink);
            } else {
                c.stroke_round(b, 4, 2, pal.token(ColorToken::OnSurfaceVariant));
            }
            let (lh, _) = text::metrics(Font::Body);
            text::draw(c, Font::Body, r.x + 30, r.y + (r.h - lh) / 2, &n.text, on_surface);
            if let (Some(id), true) = (n.action, enabled) {
                cx.hits.push((r, Hit::Bool(id, on)));
            }
        }
        UiKind::Switch => {
            let on = n.value.as_ref().and_then(|v| v.as_bool()).unwrap_or(false);
            let (lh, _) = text::metrics(Font::Body);
            text::draw(c, Font::Body, r.x, r.y + (r.h - lh) / 2, &n.text, on_surface);
            let t = Rect::new(r.right() - 52, r.y + (r.h - 28) / 2, 52, 28);
            if on {
                c.fill_round(t, 14, pal.token(ColorToken::Primary));
                c.fill_circle(t.right() - 14, t.y + 14, 10, pal.token(ColorToken::OnPrimary));
            } else {
                c.fill_round(t, 14, pal.token(ColorToken::SurfaceContainerHighest));
                c.stroke_round(t, 14, 2, pal.token(ColorToken::Outline));
                c.fill_circle(t.x + 14, t.y + 14, 7, pal.token(ColorToken::Outline));
            }
            if let (Some(id), true) = (n.action, enabled) {
                cx.hits.push((r, Hit::Bool(id, on)));
            }
        }
        UiKind::Slider => {
            let min = n.number(Attr::Min).unwrap_or(0.0) as f64;
            let max = n.number(Attr::Max).unwrap_or(1.0) as f64;
            let step = n.number(Attr::Step).unwrap_or(0.0) as f64;
            let v = n.value.as_ref().and_then(|v| v.as_f64()).unwrap_or(min);
            let t = if max > min { ((v - min) / (max - min)).clamp(0.0, 1.0) } else { 0.0 };
            let track = Rect::new(r.x + 8, r.y + r.h / 2 - 2, r.w - 16, 4);
            c.fill_round(track, 2, pal.token(ColorToken::SecondaryContainer));
            let x = track.x + (track.w as f64 * t) as i32;
            c.fill_round(Rect::new(track.x, track.y, x - track.x, 4), 2, pal.token(ColorToken::Primary));
            c.fill_circle(x, r.y + r.h / 2, 9, pal.token(ColorToken::Primary));
            if let (Some(id), true) = (n.action, enabled) {
                cx.hits.push((track, Hit::Slide(id, min, max, step)));
            }
        }
        UiKind::Progress => {
            let max = n.number(Attr::Max).unwrap_or(1.0) as f64;
            let v = n.value.as_ref().and_then(|v| v.as_f64()).unwrap_or(0.0);
            let t = if max > 0.0 { (v / max).clamp(0.0, 1.0) } else { 0.0 };
            let bar = Rect::new(r.x, r.y + r.h / 2 - 3, r.w, 6);
            c.fill_round(bar, 3, pal.token(ColorToken::SecondaryContainer));
            c.fill_round(Rect::new(bar.x, bar.y, ((bar.w as f64 * t) as i32).max(6), 6), 3, pal.token(ColorToken::Primary));
        }
        UiKind::Input => {
            let k = cx.inputs;
            cx.inputs += 1;
            let focused = cx.focus == Some(k);
            c.fill_round(r, 10, pal.token(ColorToken::SurfaceContainerHighest));
            c.stroke_round(r, 10, if focused { 2 } else { 1 }, if focused { pal.token(ColorToken::Primary) } else { pal.token(ColorToken::Outline) });
            let (lh, _) = text::metrics(Font::Body);
            let ty = r.y + (r.h - lh) / 2;
            let shown = if n.flag(Attr::Password) == Some(true) { "•".repeat(n.text.chars().count()) } else { n.text.clone() };
            let w = if shown.is_empty() {
                let ph = n.text_attr(Attr::Placeholder).unwrap_or("");
                text::draw(c, Font::Body, r.x + 14, ty, ph, pal.token(ColorToken::OnSurfaceVariant));
                0
            } else {
                let fit = text::fit(Font::Body, &shown, r.w - 30);
                text::draw(c, Font::Body, r.x + 14, ty, &fit, on_surface)
            };
            if focused {
                c.fill(Rect::new(r.x + 15 + w, ty + 2, 2, lh - 4), pal.token(ColorToken::Primary));
            }
            if let (Some(id), true) = (n.action, enabled) {
                cx.hits.push((r, Hit::Input(id, k, n.text.clone())));
            }
        }
        UiKind::Divider => {
            c.fill(Rect::new(r.x, r.y, r.w, 1), pal.token(ColorToken::OutlineVariant));
        }
        UiKind::Spacer => {}
        UiKind::Image => {
            c.fill_round(r, radius.unwrap_or(12), pal.token(ColorToken::SurfaceContainerHigh));
            let name = text::fit(Font::Small, file_name(&n.text), r.w - 16);
            text::draw_centered(c, Font::Small, r.x + r.w / 2, r.y + r.h / 2 - 8, &name, pal.token(ColorToken::OnSurfaceVariant));
        }
        UiKind::Grid => {
            let cols = n.number(Attr::Columns).map(|v| v as i32).unwrap_or(2).max(1);
            let (pad, g) = (padding(n), gap(n));
            let cell = ((r.w - 2 * pad - g * (cols - 1)) / cols).max(0);
            let mut y = r.y + pad;
            for chunk in n.children.chunks(cols as usize) {
                let h = chunk.iter().map(|k| measure(k, cell).1).max().unwrap_or(0);
                for (i, k) in chunk.iter().enumerate() {
                    let (kw, kh) = measure(k, cell);
                    let x = r.x + pad + i as i32 * (cell + g);
                    let stretch = matches!(k.kind, UiKind::Card | UiKind::Column | UiKind::Divider | UiKind::Input | UiKind::Slider | UiKind::Progress);
                    paint(c, k, Rect::new(x, y, if stretch { cell } else { kw }, if stretch { h } else { kh }), cx);
                }
                y += h + g;
            }
        }
        kind => {
            let across = stack(kind).unwrap_or(false);
            // The surface of the element.
            match kind {
                UiKind::Window => {
                    let bg = color_attr(Attr::Fill).unwrap_or(pal.token(ColorToken::Surface));
                    c.shadow(r, 16, 14, 60);
                    c.fill_round(r, radius.unwrap_or(16), bg);
                    c.stroke_round(r, radius.unwrap_or(16), 1, pal.token(ColorToken::OutlineVariant));
                    let (lh, _) = text::metrics(Font::Strong);
                    text::draw(c, Font::Strong, r.x + 18, r.y + (40 - lh) / 2 + 2, &n.text, on_surface);
                    c.fill(Rect::new(r.x + 1, r.y + 39, r.w - 2, 1), pal.token(ColorToken::OutlineVariant));
                }
                UiKind::Card => {
                    let rad = radius.unwrap_or(12);
                    let style = n.choice(Attr::Style).unwrap_or("filled");
                    let bg = color_attr(Attr::Fill).unwrap_or(match style {
                        "outlined" => pal.token(ColorToken::Surface),
                        "elevated" => pal.token(ColorToken::SurfaceContainerLow),
                        _ => pal.token(ColorToken::SurfaceContainerHighest),
                    });
                    if style == "elevated" || n.number(Attr::Elevation).is_some_and(|e| e > 0.0) {
                        c.shadow(r, rad, 8, 50);
                    }
                    c.fill_round(r, rad, bg);
                    if style == "outlined" || n.color(Attr::Border).is_some() {
                        c.stroke_round(r, rad, 1, color_attr(Attr::Border).unwrap_or(pal.token(ColorToken::OutlineVariant)));
                    }
                }
                UiKind::Accordion => {
                    let h = Rect::new(r.x, r.y, r.w, 40);
                    c.fill_round(h, 10, pal.token(ColorToken::SurfaceContainerHigh));
                    let (lh, _) = text::metrics(Font::Strong);
                    text::draw(c, Font::Strong, h.x + 14, h.y + (40 - lh) / 2, &n.text, on_surface);
                    let mark = if shows_children(n) { "−" } else { "+" };
                    text::draw_right(c, Font::Heading, h.right() - 14, h.y + 6, mark, pal.token(ColorToken::OnSurfaceVariant));
                    if let Some(id) = n.action {
                        cx.hits.push((h, Hit::Click(id)));
                    }
                }
                UiKind::Group if !n.text.is_empty() => {
                    ui::caps(c, r.x + padding(n), r.y + 4, &n.text, pal.token(ColorToken::Primary));
                }
                _ => {
                    if let Some(bg) = color_attr(Attr::Fill) {
                        c.fill_round(r, radius.unwrap_or(0), bg);
                    }
                }
            }
            if !shows_children(n) {
                return;
            }
            let (pad, g) = (padding(n), gap(n));
            let head = header(n);
            let inner = Rect::new(r.x + pad, r.y + head + pad, r.w - 2 * pad, r.h - head - 2 * pad);
            let count = n.children.len() as i32;
            if count == 0 {
                return;
            }
            let each = if kind == UiKind::Split { (inner.w - g * (count - 1)) / count } else { inner.w };
            let sizes: Vec<(i32, i32)> = n.children.iter().map(|k| measure(k, each)).collect();
            let align = n.choice(Attr::Align).unwrap_or("start");
            let stretch_all = n.flag(Attr::Stretch) == Some(true);
            let flexible = |k: &UiNode| k.kind == UiKind::Spacer && k.value.is_none();
            let springs = n.children.iter().filter(|k| flexible(k)).count() as i32;
            let used: i32 = sizes.iter().map(|s| if across { s.0 } else { s.1 }).sum::<i32>() + g * (count - 1);
            let room = if across { inner.w } else { inner.h };
            let extra = (room - used).max(0);
            let spring = if springs > 0 { extra / springs } else { 0 };
            let mut pos = if springs > 0 || kind == UiKind::Split {
                0
            } else if across || kind == UiKind::Window {
                match align {
                    "center" => extra / 2,
                    "end" => extra,
                    _ => 0,
                }
            } else {
                0
            };
            for (k, &(kw, kh)) in n.children.iter().zip(&sizes) {
                if across {
                    let w = if kind == UiKind::Split { each } else if flexible(k) { spring } else { kw };
                    let y = inner.y + (inner.h - kh) / 2;
                    paint(c, k, Rect::new(inner.x + pos, y, w, kh), cx);
                    pos += w + g;
                } else {
                    let h = if flexible(k) { spring } else { kh };
                    let full = stretch_all || matches!(k.kind, UiKind::Divider) || k.flag(Attr::Stretch) == Some(true);
                    let w = if full { inner.w } else { kw };
                    let x = match align {
                        _ if full => inner.x,
                        "center" => inner.x + (inner.w - w) / 2,
                        "end" => inner.right() - w,
                        _ => inner.x,
                    };
                    paint(c, k, Rect::new(x, inner.y + pos, w, h), cx);
                    pos += h + g;
                }
            }
        }
    }
}
