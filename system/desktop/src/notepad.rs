//! Notepad: a plain-text editor with selection, clipboard, undo, zoom and
//! saving (to writable drives).

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use meridian_rt::sys::{self, InputEvent, input, key};

use crate::apps::{Action, App, AppKind, Ctx, MenuItem, ctrl, error_text, file_name, parent};
use crate::font::{Fonts, Weight};
use crate::gfx::{Canvas, Rect};
use crate::ui;
use crate::widgets::{PROMPT_H, Prompt, PromptEvent, char_index};

const MENU_H: i32 = 30;
const STATUS_H: i32 = 26;
const MARGIN: i32 = 12;

const M_NEW: u32 = 1;
const M_OPEN: u32 = 2;
const M_SAVE: u32 = 3;
const M_SAVE_AS: u32 = 4;
const M_CLOSE: u32 = 5;
const M_UNDO: u32 = 10;
const M_CUT: u32 = 11;
const M_COPY: u32 = 12;
const M_PASTE: u32 = 13;
const M_DELETE: u32 = 14;
const M_SELECT_ALL: u32 = 15;
const M_TIME_DATE: u32 = 16;
const M_ZOOM_IN: u32 = 20;
const M_ZOOM_OUT: u32 = 21;
const M_ZOOM_RESET: u32 = 22;

const P_SAVE_AS: u32 = 1;
const P_OPEN: u32 = 2;

type Pos = (usize, usize);

#[derive(PartialEq, Eq, Clone, Copy)]
enum EditKind {
    Typing,
    Other,
}

pub struct Notepad {
    path: Option<String>,
    lines: Vec<String>,
    caret: Pos,
    anchor: Option<Pos>,
    scroll: usize,
    scroll_x: i32,
    modified: bool,
    crlf: bool,
    size: u16,
    note: String,
    undo: Option<(Vec<String>, Pos)>,
    last_edit: Option<EditKind>,
    prompt: Option<Prompt>,
    closing: bool,
    w: i32,
    h: i32,
    /// Remembered column for vertical movement.
    goal_x: Option<i32>,
}

impl Notepad {
    pub fn new(path: Option<String>) -> Self {
        let mut n = Self {
            path: None,
            lines: vec![String::new()],
            caret: (0, 0),
            anchor: None,
            scroll: 0,
            scroll_x: 0,
            modified: false,
            crlf: true,
            size: 14,
            note: String::new(),
            undo: None,
            last_edit: None,
            prompt: None,
            closing: false,
            w: 760,
            h: 500,
            goal_x: None,
        };
        if let Some(p) = path {
            n.open(&p);
        }
        n
    }

    fn open(&mut self, path: &str) -> bool {
        match sys::read_file(path) {
            Ok(data) => {
                let text = String::from_utf8_lossy(&data);
                self.crlf = text.contains("\r\n") || !text.contains('\n');
                self.lines = text.replace('\r', "").split('\n').map(String::from).collect();
                self.path = Some(path.to_string());
                self.caret = (0, 0);
                self.anchor = None;
                self.scroll = 0;
                self.modified = false;
                self.undo = None;
                self.note.clear();
                true
            }
            Err(e) => {
                self.note = format!("Could not open {path}: {}", error_text(e));
                false
            }
        }
    }

    fn text(&self) -> String {
        self.lines.join(if self.crlf { "\r\n" } else { "\n" })
    }

    fn save_to(&mut self, path: &str) -> Vec<Action> {
        match sys::write_file(path, self.text().as_bytes()) {
            Ok(()) => {
                self.path = Some(path.to_string());
                self.modified = false;
                self.note = format!("Saved to {path}");
                let mut out = vec![Action::Notify(format!("Saved {}", file_name(path))), Action::FilesChanged];
                if self.closing {
                    out.push(Action::Close);
                }
                out
            }
            Err(e) => {
                let msg = format!("Could not save: {}", error_text(e));
                match &mut self.prompt {
                    Some(p) => p.error = msg,
                    None => {
                        self.note = msg;
                        self.ask_save_as();
                    }
                }
                Vec::new()
            }
        }
    }

    fn save(&mut self) -> Vec<Action> {
        match self.path.clone() {
            Some(p) => self.save_to(&p),
            None => {
                self.ask_save_as();
                Vec::new()
            }
        }
    }

    fn ask_save_as(&mut self) {
        let suggested = match &self.path {
            Some(p) if !p.to_ascii_uppercase().starts_with("C:") => p.clone(),
            Some(p) => format!("D:\\Documents\\{}", file_name(p)),
            None => "D:\\Documents\\Untitled.txt".to_string(),
        };
        let mut prompt = Prompt::new("Save as", &suggested, "Save", P_SAVE_AS);
        let name_start = parent(&suggested).chars().count() + 1;
        let stem_end = suggested.rfind('.').map_or(suggested.chars().count(), |d| suggested[..d].chars().count());
        prompt.field.select(name_start.min(stem_end), stem_end);
        self.prompt = Some(prompt);
    }

    fn line_h(&self) -> i32 {
        self.size as i32 + 7
    }

    fn text_rect(&self) -> Rect {
        let bottom = self.h - STATUS_H - if self.prompt.is_some() || self.closing { PROMPT_H } else { 0 };
        Rect::new(0, MENU_H, self.w, bottom - MENU_H)
    }

    fn visible(&self) -> usize {
        ((self.text_rect().h - 8) / self.line_h()).max(1) as usize
    }

    fn len(&self, row: usize) -> usize {
        self.lines[row].chars().count()
    }

    fn selection(&self) -> Option<(Pos, Pos)> {
        let a = self.anchor?;
        if a == self.caret { None } else { Some((a.min(self.caret), a.max(self.caret))) }
    }

    fn selected_text(&self) -> String {
        let Some((s, e)) = self.selection() else { return String::new() };
        if s.0 == e.0 {
            let l = &self.lines[s.0];
            return l[char_index(l, s.1)..char_index(l, e.1)].to_string();
        }
        let mut out = String::new();
        let first = &self.lines[s.0];
        out.push_str(&first[char_index(first, s.1)..]);
        for r in s.0 + 1..e.0 {
            out.push('\n');
            out.push_str(&self.lines[r]);
        }
        out.push('\n');
        let last = &self.lines[e.0];
        out.push_str(&last[..char_index(last, e.1)]);
        out
    }

    fn remember(&mut self, kind: EditKind) {
        if self.last_edit != Some(kind) || kind == EditKind::Other {
            self.undo = Some((self.lines.clone(), self.caret));
        }
        self.last_edit = Some(kind);
        self.modified = true;
    }

    fn delete_selection(&mut self) -> bool {
        let Some((s, e)) = self.selection() else { return false };
        let tail = {
            let l = &self.lines[e.0];
            l[char_index(l, e.1)..].to_string()
        };
        let head = &mut self.lines[s.0];
        let at = char_index(head, s.1);
        head.truncate(at);
        head.push_str(&tail);
        self.lines.drain(s.0 + 1..=e.0);
        self.caret = s;
        self.anchor = None;
        true
    }

    fn insert(&mut self, text: &str) {
        self.delete_selection();
        let mut parts = text.split('\n');
        let first = parts.next().unwrap_or("");
        let (row, col) = self.caret;
        let line = &mut self.lines[row];
        let at = char_index(line, col);
        let rest = line.split_off(at);
        line.push_str(first);
        let mut r = row;
        let mut c = col + first.chars().count();
        for p in parts {
            r += 1;
            self.lines.insert(r, p.to_string());
            c = p.chars().count();
        }
        self.lines[r].push_str(&rest);
        self.caret = (r, c);
    }

    fn move_to(&mut self, pos: Pos, extend: bool) {
        if extend {
            if self.anchor.is_none() {
                self.anchor = Some(self.caret);
            }
        } else {
            self.anchor = None;
        }
        self.caret = pos;
        self.last_edit = None;
    }

    fn keep_visible(&mut self, f: &mut Fonts) {
        let vis = self.visible();
        if self.caret.0 < self.scroll {
            self.scroll = self.caret.0;
        } else if self.caret.0 >= self.scroll + vis {
            self.scroll = self.caret.0 + 1 - vis;
        }
        let l = &self.lines[self.caret.0];
        let x = f.measure(&l[..char_index(l, self.caret.1)], self.size, Weight::Regular);
        let room = self.w - 2 * MARGIN - 10;
        if x - self.scroll_x > room {
            self.scroll_x = x - room;
        } else if x < self.scroll_x {
            self.scroll_x = (x - 40).max(0);
        }
    }

    /// The text position under content point (`x`, `y`).
    fn pos_at(&self, f: &mut Fonts, x: i32, y: i32) -> Pos {
        let tr = self.text_rect();
        let row = (self.scroll as i32 + ((y - tr.y - 4).max(0) / self.line_h())) as usize;
        let row = row.min(self.lines.len() - 1);
        (row, self.col_at(f, row, x - MARGIN + self.scroll_x))
    }

    fn col_at(&self, f: &mut Fonts, row: usize, px: i32) -> usize {
        let line = &self.lines[row];
        let mut col = 0;
        let mut prev = 0;
        for i in 1..=line.chars().count() {
            let w = f.measure(&line[..char_index(line, i)], self.size, Weight::Regular);
            if (prev + w) / 2 > px {
                break;
            }
            col = i;
            prev = w;
        }
        col
    }

    fn command(&mut self, id: u32, cx: &mut Ctx) -> Vec<Action> {
        match id {
            M_NEW => {
                if self.modified {
                    self.note = "Save or close this document first (or use File > Close).".into();
                } else {
                    *self = Notepad::new(None);
                }
            }
            M_OPEN => {
                let dir = self.path.as_deref().map(parent).unwrap_or_else(|| "D:\\Documents".into());
                self.prompt = Some(Prompt::new("Open file", &format!("{dir}\\"), "Open", P_OPEN));
            }
            M_SAVE => return self.save(),
            M_SAVE_AS => self.ask_save_as(),
            M_CLOSE => {
                return if self.close_requested() { vec![Action::Close] } else { Vec::new() };
            }
            M_UNDO => {
                if let Some((lines, caret)) = self.undo.take() {
                    let current = (core::mem::replace(&mut self.lines, lines), self.caret);
                    self.caret = caret;
                    self.anchor = None;
                    self.undo = Some(current);
                    self.modified = true;
                    self.last_edit = None;
                }
            }
            M_CUT | M_COPY => {
                let text = self.selected_text();
                if !text.is_empty() {
                    cx.clipboard.text = text;
                    if id == M_CUT {
                        self.remember(EditKind::Other);
                        self.delete_selection();
                    }
                }
            }
            M_PASTE => {
                if !cx.clipboard.text.is_empty() {
                    self.remember(EditKind::Other);
                    let text = cx.clipboard.text.replace('\r', "").replace('\t', "    ");
                    self.insert(&text);
                }
            }
            M_DELETE => {
                if self.selection().is_some() {
                    self.remember(EditKind::Other);
                    self.delete_selection();
                }
            }
            M_SELECT_ALL => {
                let last = self.lines.len() - 1;
                self.anchor = Some((0, 0));
                self.caret = (last, self.len(last));
            }
            M_TIME_DATE => {
                let t = sys::query_time();
                let (h, ampm) = match t.hour {
                    0 => (12, "AM"),
                    1..=11 => (t.hour, "AM"),
                    12 => (12, "PM"),
                    h => (h - 12, "PM"),
                };
                self.remember(EditKind::Other);
                self.insert(&format!("{h}:{:02} {ampm} {}/{}/{}", t.minute, t.month, t.day, t.year));
            }
            M_ZOOM_IN => self.size = (self.size + 2).min(32),
            M_ZOOM_OUT => self.size = self.size.saturating_sub(2).max(10),
            M_ZOOM_RESET => self.size = 14,
            _ => {}
        }
        self.keep_visible(cx.fonts);
        Vec::new()
    }

    fn menus(&self, which: usize, cx: &Ctx) -> Vec<MenuItem> {
        let sel = self.selection().is_some();
        match which {
            0 => vec![
                MenuItem::new("New                  Ctrl+N", M_NEW),
                MenuItem::new("Open...              Ctrl+O", M_OPEN),
                MenuItem::new("Save                 Ctrl+S", M_SAVE),
                MenuItem::new("Save as...", M_SAVE_AS),
                MenuItem::separator(),
                MenuItem::new("Close", M_CLOSE),
            ],
            1 => vec![
                MenuItem::when("Undo                 Ctrl+Z", M_UNDO, self.undo.is_some()),
                MenuItem::separator(),
                MenuItem::when("Cut                  Ctrl+X", M_CUT, sel),
                MenuItem::when("Copy                 Ctrl+C", M_COPY, sel),
                MenuItem::when("Paste                Ctrl+V", M_PASTE, !cx.clipboard.text.is_empty()),
                MenuItem::when("Delete               Del", M_DELETE, sel),
                MenuItem::separator(),
                MenuItem::new("Select all           Ctrl+A", M_SELECT_ALL),
                MenuItem::new("Time/Date            F5", M_TIME_DATE),
            ],
            _ => vec![
                MenuItem::new("Zoom in              Ctrl+Plus", M_ZOOM_IN),
                MenuItem::new("Zoom out             Ctrl+Minus", M_ZOOM_OUT),
                MenuItem::new("Restore default zoom", M_ZOOM_RESET),
            ],
        }
    }

    fn prompt_answer(&mut self, purpose: u32, value: String) -> Vec<Action> {
        let value = value.trim().to_string();
        match purpose {
            P_SAVE_AS => {
                let path = if value.contains('.') || value.ends_with('\\') { value } else { format!("{value}.txt") };
                let out = self.save_to(&path);
                if !self.modified {
                    self.prompt = None;
                }
                out
            }
            _ => {
                if self.open(&value) {
                    self.prompt = None;
                } else if let Some(p) = &mut self.prompt {
                    p.error = core::mem::take(&mut self.note);
                }
                Vec::new()
            }
        }
    }
}

impl App for Notepad {
    fn kind(&self) -> AppKind {
        AppKind::Notepad
    }
    fn title(&self) -> String {
        let name = self.path.as_deref().map_or("Untitled", file_name);
        format!("{}{name} - Notepad", if self.modified { "*" } else { "" })
    }
    fn size(&self) -> (i32, i32) {
        (760, 500)
    }
    fn wheel(&mut self, notches: i32) -> bool {
        let max = self.lines.len().saturating_sub(self.visible());
        let old = self.scroll;
        self.scroll = (self.scroll as i32 + notches * 3).clamp(0, max as i32) as usize;
        self.scroll != old
    }

    fn close_requested(&mut self) -> bool {
        if self.modified {
            self.closing = true;
            false
        } else {
            true
        }
    }

    fn draw(&mut self, cv: &mut Canvas, r: Rect, cx: &mut Ctx) {
        let t = cx.theme;
        let f = &mut *cx.fonts;
        self.w = r.w;
        self.h = r.h;
        cv.fill(r, t.surface());
        // Menu bar.
        let bar = Rect::new(r.x, r.y, r.w, MENU_H);
        cv.fill(bar, t.window());
        for (i, label) in ["File", "Edit", "View"].iter().enumerate() {
            f.draw(cv, r.x + 12 + i as i32 * 52, r.y + 7, label, 14, Weight::Regular, t.text());
        }
        cv.fill(Rect::new(bar.x, bar.bottom() - 1, bar.w, 1), t.border());
        // Text.
        let tr = self.text_rect().offset(r.x, r.y);
        let old = cv.clip();
        cv.set_clip(tr.intersect(&old).unwrap_or(tr));
        let lh = self.line_h();
        let sel = self.selection();
        for (i, line) in self.lines.iter().enumerate().skip(self.scroll).take(self.visible() + 1) {
            let y = tr.y + 4 + (i - self.scroll) as i32 * lh;
            let x0 = tr.x + MARGIN - self.scroll_x;
            if let Some((s, e)) = sel {
                if i >= s.0 && i <= e.0 {
                    let from = if i == s.0 { s.1 } else { 0 };
                    let to = if i == e.0 { e.1 } else { line.chars().count() };
                    let a = f.measure(&line[..char_index(line, from)], self.size, Weight::Regular);
                    let b = f.measure(&line[..char_index(line, to)], self.size, Weight::Regular);
                    let extra = if i < e.0 { self.size as i32 / 2 } else { 0 };
                    cv.fill(Rect::new(x0 + a, y, b - a + extra, lh), t.selection());
                }
            }
            f.draw(cv, x0, y + 3, line, self.size, Weight::Regular, t.text());
            if i == self.caret.0 && self.prompt.is_none() && !self.closing {
                let cx_ = x0 + f.measure(&line[..char_index(line, self.caret.1)], self.size, Weight::Regular);
                cv.fill(Rect::new(cx_, y + 2, 1, lh - 3), t.text());
            }
        }
        cv.set_clip(old);
        // Prompt or "save changes?" bar.
        if let Some(p) = &mut self.prompt {
            p.draw(cv, f, &t, Rect::new(r.x, r.y, r.w, r.h - STATUS_H));
        } else if self.closing {
            let b = Rect::new(r.x, r.bottom() - STATUS_H - PROMPT_H, r.w, PROMPT_H);
            cv.fill(b, t.window());
            cv.fill(Rect::new(b.x, b.y, b.w, 1), t.accent);
            let name = self.path.as_deref().map_or("Untitled", file_name);
            let q = format!("Do you want to save changes to {name}?");
            f.draw_fit(cv, b.x + 14, b.y + 17, &q, b.w - 340, 14, Weight::Regular, t.text());
            for (k, (label, primary)) in [("Save", true), ("Don't save", false), ("Cancel", false)].iter().enumerate() {
                let br = Rect::new(b.right() - 318 + k as i32 * 104, b.y + 10, 96, 32);
                ui::button(cv, f, &t, br, label, *primary, false);
            }
        }
        // Status bar.
        let sb = Rect::new(r.x, r.bottom() - STATUS_H, r.w, STATUS_H);
        cv.fill(sb, t.window());
        cv.fill(Rect::new(sb.x, sb.y, sb.w, 1), t.border());
        let chars: usize = self.lines.iter().map(|l| l.chars().count()).sum::<usize>() + self.lines.len() - 1;
        let left = if self.note.is_empty() {
            format!("Ln {}, Col {}     {} characters", self.caret.0 + 1, self.caret.1 + 1, chars)
        } else {
            self.note.clone()
        };
        f.draw_fit(cv, sb.x + 12, sb.y + 5, &left, sb.w - 300, 12, Weight::Regular, t.text_dim());
        let right = format!(
            "{}%     {}     UTF-8",
            self.size as u32 * 100 / 14,
            if self.crlf { "Windows (CRLF)" } else { "Unix (LF)" }
        );
        let rw = f.measure(&right, 12, Weight::Regular);
        f.draw(cv, sb.right() - rw - 12, sb.y + 5, &right, 12, Weight::Regular, t.text_dim());
    }

    fn click(&mut self, x: i32, y: i32, double: bool, cx: &mut Ctx) -> Vec<Action> {
        if y < MENU_H {
            let i = (x - 6) / 52;
            if (0..3).contains(&i) {
                return vec![Action::Menu(self.menus(i as usize, cx), 6 + i * 52, MENU_H - 2)];
            }
            return Vec::new();
        }
        let content = Rect::new(0, 0, self.w, self.h - STATUS_H);
        if let Some(p) = &mut self.prompt {
            if y >= content.bottom() - PROMPT_H {
                let purpose = p.purpose;
                return match p.click(cx.fonts, content, x, y) {
                    PromptEvent::Submit(v) => self.prompt_answer(purpose, v),
                    PromptEvent::Cancel => {
                        self.prompt = None;
                        self.closing = false;
                        Vec::new()
                    }
                    PromptEvent::None => Vec::new(),
                };
            }
        } else if self.closing && y >= content.bottom() - PROMPT_H {
            let base = self.w - 318;
            if (content.bottom() - PROMPT_H + 10..content.bottom() - 10).contains(&y) && x >= base {
                match (x - base) / 104 {
                    0 => return self.save(),
                    1 => {
                        self.modified = false;
                        return vec![Action::Close];
                    }
                    _ => self.closing = false,
                }
            }
            return Vec::new();
        }
        let shift_held = false;
        let pos = self.pos_at(cx.fonts, x, y);
        if double {
            // Select the word under the pointer.
            let line: Vec<char> = self.lines[pos.0].chars().collect();
            let word = |c: char| c.is_alphanumeric() || c == '_';
            let (mut s, mut e) = (pos.1, pos.1);
            while s > 0 && word(line[s - 1]) {
                s -= 1;
            }
            while e < line.len() && word(line[e]) {
                e += 1;
            }
            self.anchor = Some((pos.0, s));
            self.caret = (pos.0, e);
        } else {
            self.move_to(pos, shift_held);
            self.anchor = Some(pos);
        }
        self.goal_x = None;
        Vec::new()
    }

    fn drag(&mut self, x: i32, y: i32, cx: &mut Ctx) -> bool {
        if y < MENU_H - 10 || self.prompt.is_some() || self.closing {
            return false;
        }
        let pos = self.pos_at(cx.fonts, x, y);
        if pos != self.caret {
            self.caret = pos;
            self.keep_visible(cx.fonts);
            return true;
        }
        false
    }

    fn right_click(&mut self, x: i32, y: i32, cx: &mut Ctx) -> Vec<Action> {
        if y < MENU_H || self.prompt.is_some() {
            return Vec::new();
        }
        let sel = self.selection().is_some();
        vec![Action::Menu(
            vec![
                MenuItem::when("Undo", M_UNDO, self.undo.is_some()),
                MenuItem::separator(),
                MenuItem::when("Cut", M_CUT, sel),
                MenuItem::when("Copy", M_COPY, sel),
                MenuItem::when("Paste", M_PASTE, !cx.clipboard.text.is_empty()),
                MenuItem::when("Delete", M_DELETE, sel),
                MenuItem::separator(),
                MenuItem::new("Select all", M_SELECT_ALL),
            ],
            x,
            y,
        )]
    }

    fn menu(&mut self, id: u32, cx: &mut Ctx) -> Vec<Action> {
        self.command(id, cx)
    }

    fn key(&mut self, e: &InputEvent, cx: &mut Ctx) -> Vec<Action> {
        if let Some(p) = &mut self.prompt {
            let purpose = p.purpose;
            return match p.key(e, cx.clipboard) {
                PromptEvent::Submit(v) => self.prompt_answer(purpose, v),
                PromptEvent::Cancel => {
                    self.prompt = None;
                    self.closing = false;
                    Vec::new()
                }
                PromptEvent::None => Vec::new(),
            };
        }
        if self.closing {
            if e.key == key::ESCAPE {
                self.closing = false;
            }
            return Vec::new();
        }
        for (letter, id) in [
            ('n', M_NEW),
            ('o', M_OPEN),
            ('s', M_SAVE),
            ('z', M_UNDO),
            ('x', M_CUT),
            ('c', M_COPY),
            ('v', M_PASTE),
            ('a', M_SELECT_ALL),
            ('=', M_ZOOM_IN),
            ('+', M_ZOOM_IN),
            ('-', M_ZOOM_OUT),
            ('0', M_ZOOM_RESET),
        ] {
            if ctrl(e, letter) {
                return self.command(id, cx);
            }
        }
        let shift = e.flags & input::SHIFT != 0;
        let control = e.flags & input::CTRL != 0;
        if control && e.key == key::CHAR {
            return Vec::new();
        }
        self.note.clear();
        let (row, col) = self.caret;
        match e.key {
            key::CHAR => {
                if let Some(c) = char::from_u32(e.ch).filter(|c| !c.is_control()) {
                    self.remember(EditKind::Typing);
                    let mut b = [0u8; 4];
                    self.insert(c.encode_utf8(&mut b));
                }
            }
            key::TAB => {
                self.remember(EditKind::Other);
                self.insert("    ");
            }
            key::ENTER => {
                self.remember(EditKind::Other);
                self.insert("\n");
            }
            key::BACKSPACE => {
                self.remember(EditKind::Other);
                if !self.delete_selection() {
                    if col > 0 {
                        let line = &mut self.lines[row];
                        let at = char_index(line, col - 1);
                        line.remove(at);
                        self.caret.1 -= 1;
                    } else if row > 0 {
                        let line = self.lines.remove(row);
                        self.caret = (row - 1, self.len(row - 1));
                        self.lines[row - 1].push_str(&line);
                    }
                }
            }
            key::DELETE => {
                self.remember(EditKind::Other);
                if !self.delete_selection() {
                    if col < self.len(row) {
                        let line = &mut self.lines[row];
                        let at = char_index(line, col);
                        line.remove(at);
                    } else if row + 1 < self.lines.len() {
                        let next = self.lines.remove(row + 1);
                        self.lines[row].push_str(&next);
                    }
                }
            }
            key::LEFT => {
                let to = match (shift, self.selection()) {
                    (false, Some((s, _))) => s,
                    _ if col > 0 => (row, col - 1),
                    _ if row > 0 => (row - 1, self.len(row - 1)),
                    _ => (row, col),
                };
                self.move_to(to, shift);
            }
            key::RIGHT => {
                let to = match (shift, self.selection()) {
                    (false, Some((_, e))) => e,
                    _ if col < self.len(row) => (row, col + 1),
                    _ if row + 1 < self.lines.len() => (row + 1, 0),
                    _ => (row, col),
                };
                self.move_to(to, shift);
            }
            key::UP | key::DOWN | key::PAGE_UP | key::PAGE_DOWN => {
                let line = &self.lines[row];
                let gx = self
                    .goal_x
                    .unwrap_or_else(|| cx.fonts.measure(&line[..char_index(line, col)], self.size, Weight::Regular));
                let target = match e.key {
                    key::UP => row.saturating_sub(1),
                    key::DOWN => (row + 1).min(self.lines.len() - 1),
                    key::PAGE_UP => row.saturating_sub(self.visible()),
                    _ => (row + self.visible()).min(self.lines.len() - 1),
                };
                let c = self.col_at(cx.fonts, target, gx);
                self.move_to((target, c), shift);
                self.goal_x = Some(gx);
                self.keep_visible(cx.fonts);
                return Vec::new();
            }
            key::HOME => {
                let to = if control { (0, 0) } else { (row, 0) };
                self.move_to(to, shift);
            }
            key::END => {
                let last = self.lines.len() - 1;
                let to = if control { (last, self.len(last)) } else { (row, self.len(row)) };
                self.move_to(to, shift);
            }
            k if k == key::FUNCTION + 5 => return self.command(M_TIME_DATE, cx),
            _ => return Vec::new(),
        }
        self.goal_x = None;
        self.keep_visible(cx.fonts);
        Vec::new()
    }
}
