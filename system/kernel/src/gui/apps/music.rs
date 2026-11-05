//! Musica: plays the WAV files in `sys:/home/Musica` (and the system's
//! sounds), one after the other: play and pause, previous and next, a bar to
//! move through the song, the volume.

use crate::dev::audio::{self, Sound};
use crate::dev::input::{Key, KeyEvent};
use crate::fs;
use crate::gui::gfx::{Canvas, Rect};
use crate::gui::res;
use crate::gui::text::{self, Font};
use crate::gui::ui::{self, TEXT, TEXT_DIM, TEXT_FAINT};
use crate::gui::wm::{Action, App};
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

const ROW: i32 = 46;
const BAR: i32 = 96;

struct Song {
    path: String,
    title: String,
    album: String,
    /// Length in seconds.
    secs: u64,
}

pub struct Music {
    songs: Vec<Song>,
    /// The song loaded, its sound and the track number the player gave it.
    current: Option<(usize, Arc<Sound>, u64)>,
    /// Where a paused song stopped (frames).
    paused_at: Option<u64>,
    scroll: i32,
    hover: Option<usize>,
    dragging: Option<&'static str>,
    note: String,
    last_shown: u64,
}

fn mmss(s: u64) -> String {
    format!("{}:{:02}", s / 60, s % 60)
}

/// "una_canzone-bella" -> "Una canzone bella".
fn pretty(stem: &str) -> String {
    let s: String = stem.chars().map(|c| if c == '_' || c == '-' { ' ' } else { c }).collect();
    let mut ch = s.chars();
    ch.next().map(|f| f.to_uppercase().chain(ch).collect()).unwrap_or_default()
}

impl Music {
    pub fn new() -> Music {
        let mut songs = Vec::new();
        let mut add = |dir: &str, album: &str| {
            let Ok(d) = fs::abs(dir) else { return };
            for (e, _) in fs::list_dir(&d).unwrap_or_default() {
                if !e.name.to_ascii_lowercase().ends_with(".wav") {
                    continue;
                }
                let p = d.join(&e.name);
                let mut head = [0u8; 512];
                let n = fs::read_at(&p, 0, &mut head).unwrap_or(0);
                let secs = audio::wav_info(&head[..n]).map_or(0, |(rate, _, frames)| frames / rate.max(1) as u64);
                let stem = e.name.rsplit_once('.').map_or(e.name.as_str(), |(s, _)| s);
                songs.push(Song { path: format!("{p}"), title: pretty(stem), album: String::from(album), secs });
            }
        };
        add("sys:/home/Musica", "La mia musica");
        add("sys:/system/sounds", "Suoni di JustOS");
        Music { songs, current: None, paused_at: None, scroll: 0, hover: None, dragging: None, note: String::new(), last_shown: u64::MAX }
    }

    pub fn with_file(path: &str) -> Music {
        let mut m = Music::new();
        let i = match m.songs.iter().position(|s| s.path == path) {
            Some(i) => i,
            None => {
                let name = path.rsplit('/').next().unwrap_or(path);
                let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
                m.songs.insert(0, Song { path: String::from(path), title: pretty(stem), album: String::new(), secs: 0 });
                0
            }
        };
        m.play(i, 0);
        m
    }

    fn play(&mut self, i: usize, from: u64) {
        let Some(song) = self.songs.get(i) else { return };
        let sound = match &self.current {
            Some((j, s, _)) if *j == i => s.clone(),
            _ => match fs::abs(&song.path).and_then(|p| fs::read_file(&p)) {
                Ok(bytes) => match audio::parse_wav(&bytes) {
                    Ok(s) => Arc::new(s),
                    Err(e) => {
                        self.note = String::from(e);
                        return;
                    }
                },
                Err(e) => {
                    self.note = format!("{e}");
                    return;
                }
            },
        };
        if self.songs[i].secs == 0 {
            self.songs[i].secs = sound.samples.len() as u64 / sound.channels.max(1) as u64 / sound.rate.max(1) as u64;
        }
        match audio::play_track(sound.clone(), from as usize) {
            Ok(track) => {
                self.current = Some((i, sound, track));
                self.paused_at = None;
                self.note.clear();
            }
            Err(_) => {
                self.current = Some((i, sound, 0));
                self.note = String::from("Nessuna scheda audio: collegane una per ascoltare");
            }
        }
    }

    /// (position, length) in frames, and whether it plays.
    fn state(&self) -> Option<(u64, u64, bool, u32)> {
        let (_, sound, track) = self.current.as_ref()?;
        let len = sound.samples.len() as u64 / sound.channels.max(1) as u64;
        if let Some(at) = self.paused_at {
            return Some((at, len, false, sound.rate));
        }
        let (now, pos, _, playing) = audio::progress();
        if now == *track && *track != 0 {
            Some((pos.min(len), len, playing, sound.rate))
        } else {
            Some((0, len, false, sound.rate))
        }
    }

    fn toggle(&mut self) {
        match self.state() {
            Some((pos, _, true, _)) => {
                audio::stop();
                self.paused_at = Some(pos);
            }
            Some((pos, len, false, _)) => {
                let i = self.current.as_ref().map_or(0, |c| c.0);
                self.play(i, if pos >= len { 0 } else { pos });
            }
            None => {
                if !self.songs.is_empty() {
                    self.play(0, 0);
                }
            }
        }
    }

    fn skip(&mut self, by: i32) {
        let n = self.songs.len() as i32;
        if n == 0 {
            return;
        }
        let i = self.current.as_ref().map_or(0, |c| c.0 as i32);
        // Back to the start of the song first, as players do.
        if by < 0 {
            if let Some((pos, _, _, rate)) = self.state() {
                if pos > rate as u64 * 3 {
                    self.play(i as usize, 0);
                    return;
                }
            }
        }
        self.play((i + by).rem_euclid(n) as usize, 0);
    }

    fn list(area: Rect) -> Rect {
        Rect::new(area.x + 12, area.y + 64, area.w - 24, area.h - 64 - BAR - 8)
    }

    fn bar(area: Rect) -> Rect {
        Rect::new(area.x + 8, area.bottom() - BAR - 4, area.w - 16, BAR)
    }

    fn controls(area: Rect) -> [Rect; 3] {
        let b = Self::bar(area);
        let cx = b.x + b.w / 2;
        let y = b.y + 12;
        [Rect::new(cx - 76, y + 6, 36, 36), Rect::new(cx - 24, y, 48, 48), Rect::new(cx + 40, y + 6, 36, 36)]
    }

    fn seek_rect(area: Rect) -> Rect {
        let b = Self::bar(area);
        Rect::new(b.x + b.w / 2 - 170, b.y + 70, 340, 12)
    }

    fn volume_rect(area: Rect) -> Rect {
        let b = Self::bar(area);
        Rect::new(b.right() - 150, b.y + 40, 120, 20)
    }

    fn seek_to(&mut self, area: Rect, x: i32) {
        let Some((_, len, playing, _)) = self.state() else { return };
        let v = ui::slider_value(Self::seek_rect(area), x) as u64;
        let at = len * v / 1000;
        let i = self.current.as_ref().map_or(0, |c| c.0);
        if playing {
            self.play(i, at);
        } else {
            self.paused_at = Some(at);
        }
    }
}

impl App for Music {
    fn title(&self) -> String {
        String::from("Musica")
    }

    fn icon(&self) -> &str {
        "music"
    }

    fn kind(&self) -> &'static str {
        "music"
    }

    fn size(&self) -> (i32, i32) {
        (700, 520)
    }

    fn resizable(&self) -> bool {
        true
    }

    fn draw(&mut self, c: &mut Canvas, area: Rect, _blur: &Canvas, _focused: bool) {
        text::draw(c, Font::Title, area.x + 20, area.y + 14, "Brani", TEXT);
        let total: u64 = self.songs.iter().map(|s| s.secs).sum();
        text::draw_right(c, Font::Small, area.right() - 20, area.y + 28, &format!("{} brani, {} minuti", self.songs.len(), total.div_ceil(60)), TEXT_DIM);
        let lr = Self::list(area);
        let saved = c.clip();
        c.set_clip(saved.intersect(&lr));
        if self.songs.is_empty() {
            text::draw_centered(c, Font::Body, lr.x + lr.w / 2, lr.y + 40, "Metti i file WAV nella cartella Musica", TEXT_FAINT);
        }
        let state = self.state();
        let cur = self.current.as_ref().map(|c| c.0);
        for (i, s) in self.songs.iter().enumerate() {
            let r = Rect::new(lr.x, lr.y + i as i32 * ROW - self.scroll, lr.w, ROW - 2);
            if r.bottom() < lr.y || r.y > lr.bottom() {
                continue;
            }
            if cur == Some(i) {
                c.fill_round(r, 10, 0x40FF_2D55);
            } else if self.hover == Some(i) {
                c.fill_round(r, 10, ui::PANEL);
            }
            if cur == Some(i) && state.is_some_and(|s| s.2) {
                // Three bars, as a level meter.
                for (k, h) in [10, 16, 7].iter().enumerate() {
                    c.fill(Rect::new(r.x + 17 + k as i32 * 5, r.y + 30 - h, 3, *h), 0xFFFF_375F);
                }
            } else {
                text::draw_centered(c, Font::Body, r.x + 24, r.y + 12, &format!("{}", i + 1), if cur == Some(i) { 0xFFFF_375F } else { TEXT_FAINT });
            }
            text::draw(c, Font::Strong, r.x + 50, r.y + 4, &text::fit(Font::Strong, &s.title, r.w - 200), TEXT);
            text::draw(c, Font::Small, r.x + 50, r.y + 24, &s.album, TEXT_DIM);
            text::draw_right(c, Font::Body, r.right() - 16, r.y + 12, &mmss(s.secs), TEXT_DIM);
        }
        c.set_clip(saved);
        // The player.
        let b = Self::bar(area);
        c.fill_round(b, 16, 0x33_00_00_00);
        let art = Rect::new(b.x + 14, b.y + 14, 68, 68);
        c.fill_round_grad(art, 12, 0xFFFF_5E7E, 0xFFB0_2BD9);
        res::draw_icon(c, "g-note", 36, art.x + 16, art.y + 16);
        if let Some(i) = cur {
            let s = &self.songs[i];
            text::draw(c, Font::Strong, art.right() + 14, b.y + 22, &text::fit(Font::Strong, &s.title, b.w / 2 - 200), TEXT);
            text::draw(c, Font::Small, art.right() + 14, b.y + 46, &s.album, TEXT_DIM);
        } else {
            text::draw(c, Font::Body, art.right() + 14, b.y + 34, "Scegli un brano", TEXT_DIM);
        }
        let [prev, play, next] = Self::controls(area);
        ui::icon_button(c, prev, "g-prev", false);
        let playing = state.is_some_and(|s| s.2);
        c.fill_circle(play.x + play.w / 2, play.y + play.h / 2, play.w / 2, 0xFFFF_FFFF);
        res::draw_icon(c, if playing { "g-pause-dark" } else { "g-play-dark" }, 24, play.x + 12, play.y + 12);
        ui::icon_button(c, next, "g-next", false);
        let sr = Self::seek_rect(area);
        let (pos, len, rate) = state.map_or((0, 1, 1), |s| (s.0, s.1.max(1), s.3.max(1) as u64));
        ui::slider(c, sr, (pos * 1000 / len) as u32);
        text::draw_right(c, Font::Small, sr.x - 12, sr.y - 3, &mmss(pos / rate), TEXT_DIM);
        text::draw(c, Font::Small, sr.right() + 12, sr.y - 3, &mmss(len / rate), TEXT_DIM);
        let vr = Self::volume_rect(area);
        res::draw_icon(c, "g-volume", 16, vr.x - 26, vr.y + 2);
        ui::slider(c, vr, audio::volume() * 10);
        if !self.note.is_empty() {
            text::draw_right(c, Font::Small, b.right() - 14, b.y + 12, &text::fit(Font::Small, &self.note, b.w / 3), ui::ORANGE);
        }
    }

    fn press(&mut self, x: i32, y: i32, double: bool, area: Rect) -> Action {
        let (ax, ay) = (area.x + x, area.y + y);
        let [prev, play, next] = Self::controls(area);
        if play.contains(ax, ay) {
            self.toggle();
        } else if prev.contains(ax, ay) {
            self.skip(-1);
        } else if next.contains(ax, ay) {
            self.skip(1);
        } else if Self::seek_rect(area).grow(8).contains(ax, ay) {
            self.dragging = Some("seek");
            self.seek_to(area, ax);
        } else if Self::volume_rect(area).grow(8).contains(ax, ay) {
            self.dragging = Some("volume");
            crate::gui::change_volume(ui::slider_value(Self::volume_rect(area), ax) / 10);
        } else {
            let lr = Self::list(area);
            if lr.contains(ax, ay) {
                let i = ((ay - lr.y + self.scroll) / ROW) as usize;
                if i < self.songs.len() && (double || self.current.is_none()) {
                    self.play(i, 0);
                }
            }
        }
        Action::Redraw
    }

    fn drag(&mut self, x: i32, _y: i32, area: Rect) -> bool {
        match self.dragging {
            Some("volume") => {
                crate::gui::change_volume(ui::slider_value(Self::volume_rect(area), area.x + x) / 10);
                true
            }
            Some(_) => {
                // Seeking restarts the sound: only on release.
                false
            }
            None => false,
        }
    }

    fn release(&mut self, _area: Rect) -> Action {
        self.dragging = None;
        Action::None
    }

    fn hover(&mut self, at: Option<(i32, i32)>, area: Rect) -> bool {
        let lr = Self::list(area);
        let h = at.and_then(|(x, y)| {
            let (ax, ay) = (area.x + x, area.y + y);
            let i = ((ay - lr.y + self.scroll) / ROW) as usize;
            (lr.contains(ax, ay) && i < self.songs.len()).then_some(i)
        });
        let changed = h != self.hover;
        self.hover = h;
        changed
    }

    fn key(&mut self, ev: KeyEvent) -> Action {
        match ev.key {
            Key::Char(' ') => self.toggle(),
            Key::Right => self.skip(1),
            Key::Left => self.skip(-1),
            _ => return Action::None,
        }
        Action::Redraw
    }

    fn scroll(&mut self, lines: i32) -> bool {
        let old = self.scroll;
        self.scroll = (self.scroll + lines * ROW).clamp(0, (self.songs.len() as i32 * ROW - 200).max(0));
        old != self.scroll
    }

    fn tick(&mut self) -> bool {
        // The next song when one ends.
        if let Some((pos, len, playing, _)) = self.state() {
            if !playing && self.paused_at.is_none() && pos >= len && len > 0 && self.songs.len() > 1 {
                self.skip(1);
                return true;
            }
            let shown = pos / 4000;
            if shown != self.last_shown {
                self.last_shown = shown;
                return true;
            }
        }
        false
    }

    fn close(&mut self) {
        if self.state().is_some_and(|s| s.2) {
            audio::stop();
        }
    }
}
