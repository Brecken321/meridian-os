//! Small reusable controls: a single-line text field and a prompt bar
//! (a text field with OK/Cancel buttons) that apps show for names and
//! paths.

use alloc::string::{String, ToString};

use meridian_rt::sys::{InputEvent, input, key};

use crate::apps::{Clipboard, ctrl};
use crate::font::{Fonts, Weight};
use crate::gfx::{Canvas, Rect};
use crate::ui::{self, Theme};

/// Byte offset of character `col` in `s`.
pub fn char_index(s: &str, col: usize) -> usize {
    s.char_indices().nth(col).map_or(s.len(), |(i, _)| i)
}

pub enum FieldEvent {
    None,
    Changed,
    Submit,
    Cancel,
}

/// A one-line editable text with a caret and an optional selection.
pub struct TextField {
    pub text: String,
    /// Caret position in characters.
    pub caret: usize,
    /// Selection anchor (the other end is the caret).
    pub anchor: Option<usize>,
    scroll_x: i32,
}

impl TextField {
    pub fn new(text: &str) -> Self {
        Self { text: text.to_string(), caret: text.chars().count(), anchor: None, scroll_x: 0 }
    }

    /// Selects characters `from..to`.
    pub fn select(&mut self, from: usize, to: usize) {
        self.anchor = Some(from);
        self.caret = to;
    }

    fn len(&self) -> usize {
        self.text.chars().count()
    }

    fn selection(&self) -> Option<(usize, usize)> {
        let a = self.anchor?;
        if a == self.caret { None } else { Some((a.min(self.caret), a.max(self.caret))) }
    }

    fn delete_selection(&mut self) -> bool {
        let Some((s, e)) = self.selection() else { return false };
        let (bs, be) = (char_index(&self.text, s), char_index(&self.text, e));
        self.text.replace_range(bs..be, "");
        self.caret = s;
        self.anchor = None;
        true
    }

    fn insert(&mut self, s: &str) {
        self.delete_selection();
        let at = char_index(&self.text, self.caret);
        self.text.insert_str(at, s);
        self.caret += s.chars().count();
    }

    pub fn key(&mut self, e: &InputEvent, clip: &mut Clipboard) -> FieldEvent {
        let shift = e.flags & input::SHIFT != 0;
        if ctrl(e, 'a') {
            self.select(0, self.len());
            return FieldEvent::None;
        }
        if ctrl(e, 'c') || ctrl(e, 'x') {
            if let Some((s, en)) = self.selection() {
                clip.text = self.text[char_index(&self.text, s)..char_index(&self.text, en)].to_string();
                if ctrl(e, 'x') {
                    self.delete_selection();
                    return FieldEvent::Changed;
                }
            }
            return FieldEvent::None;
        }
        if ctrl(e, 'v') {
            let text: String = clip.text.lines().next().unwrap_or("").chars().filter(|c| !c.is_control()).collect();
            self.insert(&text);
            return FieldEvent::Changed;
        }
        if e.flags & input::CTRL != 0 && e.key == key::CHAR {
            return FieldEvent::None;
        }
        let mv = |f: &mut Self, to: usize| {
            if shift {
                if f.anchor.is_none() {
                    f.anchor = Some(f.caret);
                }
            } else {
                f.anchor = None;
            }
            f.caret = to;
        };
        match e.key {
            key::CHAR => {
                if let Some(c) = char::from_u32(e.ch).filter(|c| !c.is_control()) {
                    let mut b = [0u8; 4];
                    self.insert(c.encode_utf8(&mut b));
                    return FieldEvent::Changed;
                }
            }
            key::BACKSPACE => {
                if !self.delete_selection() && self.caret > 0 {
                    let at = char_index(&self.text, self.caret - 1);
                    self.text.remove(at);
                    self.caret -= 1;
                }
                return FieldEvent::Changed;
            }
            key::DELETE => {
                if !self.delete_selection() && self.caret < self.len() {
                    let at = char_index(&self.text, self.caret);
                    self.text.remove(at);
                }
                return FieldEvent::Changed;
            }
            key::LEFT => {
                let to = match (shift, self.selection()) {
                    (false, Some((s, _))) => s,
                    _ => self.caret.saturating_sub(1),
                };
                mv(self, to);
            }
            key::RIGHT => {
                let to = match (shift, self.selection()) {
                    (false, Some((_, e))) => e,
                    _ => (self.caret + 1).min(self.len()),
                };
                mv(self, to);
            }
            key::HOME => mv(self, 0),
            key::END => {
                let n = self.len();
                mv(self, n);
            }
            key::ENTER => return FieldEvent::Submit,
            key::ESCAPE => return FieldEvent::Cancel,
            _ => {}
        }
        FieldEvent::None
    }

    /// Places the caret at pixel `x` within a field drawn at `r`.
    pub fn click(&mut self, f: &mut Fonts, r: Rect, x: i32) {
        let target = x - (r.x + 8) + self.scroll_x;
        let mut col = 0;
        for i in 1..=self.len() {
            if f.measure(&self.text[..char_index(&self.text, i)], 14, Weight::Regular) > target + 3 {
                break;
            }
            col = i;
        }
        self.caret = col;
        self.anchor = None;
    }

    pub fn draw(&mut self, cv: &mut Canvas, f: &mut Fonts, t: &Theme, r: Rect, focused: bool) {
        let border = if focused { t.accent } else { t.border() };
        cv.rounded_box(r, 4, border, t.surface());
        if focused {
            cv.fill(Rect::new(r.x + 2, r.bottom() - 2, r.w - 4, 2), t.accent);
        }
        let inner = r.inset(2);
        let old = cv.clip();
        if let Some(c) = inner.intersect(&old) {
            cv.set_clip(c);
        }
        let caret_px = f.measure(&self.text[..char_index(&self.text, self.caret)], 14, Weight::Regular);
        let room = r.w - 16;
        if caret_px - self.scroll_x > room {
            self.scroll_x = caret_px - room;
        } else if caret_px < self.scroll_x {
            self.scroll_x = caret_px;
        }
        let tx = r.x + 8 - self.scroll_x;
        let ty = r.y + (r.h - f.line_height(14)) / 2;
        if let Some((s, e)) = self.selection() {
            let x0 = f.measure(&self.text[..char_index(&self.text, s)], 14, Weight::Regular);
            let x1 = f.measure(&self.text[..char_index(&self.text, e)], 14, Weight::Regular);
            cv.fill(Rect::new(tx + x0, ty, x1 - x0, f.line_height(14)), t.selection());
        }
        f.draw(cv, tx, ty, &self.text, 14, Weight::Regular, t.text());
        if focused {
            cv.fill(Rect::new(tx + caret_px, ty + 1, 1, f.line_height(14) - 2), t.text());
        }
        cv.set_clip(old);
    }
}

/// A question with a text field and OK/Cancel, drawn as a bar across the
/// bottom of an app's content.
pub struct Prompt {
    pub label: String,
    pub field: TextField,
    pub ok: String,
    /// What the app does with the answer.
    pub purpose: u32,
    pub error: String,
}

pub const PROMPT_H: i32 = 52;

pub enum PromptEvent {
    None,
    Submit(String),
    Cancel,
}

impl Prompt {
    pub fn new(label: &str, value: &str, ok: &str, purpose: u32) -> Self {
        Self {
            label: label.to_string(),
            field: TextField::new(value),
            ok: ok.to_string(),
            purpose,
            error: String::new(),
        }
    }

    fn layout(r: Rect) -> (Rect, Rect, Rect, Rect) {
        let bar = Rect::new(r.x, r.bottom() - PROMPT_H, r.w, PROMPT_H);
        let cancel = Rect::new(bar.right() - 92, bar.y + 10, 80, 32);
        let ok = Rect::new(cancel.x - 96, bar.y + 10, 88, 32);
        let field = Rect::new(bar.x + 150, bar.y + 10, ok.x - bar.x - 160, 32);
        (bar, field, ok, cancel)
    }

    pub fn draw(&mut self, cv: &mut Canvas, f: &mut Fonts, t: &Theme, r: Rect) {
        let (bar, field, ok, cancel) = Self::layout(r);
        cv.fill(bar, t.window());
        cv.fill(Rect::new(bar.x, bar.y, bar.w, 1), t.border());
        let label_color = if self.error.is_empty() { t.text() } else { t.danger() };
        let label = if self.error.is_empty() { self.label.as_str() } else { self.error.as_str() };
        f.draw_fit(cv, bar.x + 12, bar.y + 17, label, 132, 12, Weight::Semibold, label_color);
        self.field.draw(cv, f, t, field, true);
        ui::button(cv, f, t, ok, &self.ok, true, false);
        ui::button(cv, f, t, cancel, "Cancel", false, false);
    }

    /// Handles a click at content coordinates (`r` = the content rect, at
    /// the origin).
    pub fn click(&mut self, f: &mut Fonts, r: Rect, x: i32, y: i32) -> PromptEvent {
        let (_, field, ok, cancel) = Self::layout(r);
        if ok.contains(x, y) {
            PromptEvent::Submit(self.field.text.clone())
        } else if cancel.contains(x, y) {
            PromptEvent::Cancel
        } else {
            if field.contains(x, y) {
                self.field.click(f, field, x);
            }
            PromptEvent::None
        }
    }

    pub fn key(&mut self, e: &InputEvent, clip: &mut Clipboard) -> PromptEvent {
        match self.field.key(e, clip) {
            FieldEvent::Submit => PromptEvent::Submit(self.field.text.clone()),
            FieldEvent::Cancel => PromptEvent::Cancel,
            FieldEvent::Changed => {
                self.error.clear();
                PromptEvent::None
            }
            FieldEvent::None => PromptEvent::None,
        }
    }
}
