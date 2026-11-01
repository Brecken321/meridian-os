//! The built-in desktop apps. Each one talks to the kernel through real
//! system calls; nothing on screen is a mock-up. (They run inside the
//! desktop process for now; separate processes need the window-server IPC
//! that is on the roadmap.)

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use meridian_rt::sys::{self, DirEntry, InputEvent, key};
use meridian_rt::{Handle, status};

use crate::font::{Fonts, MONO_H, MONO_W, Mono, Weight};
use crate::gfx::{Canvas, Rect, rgb, with_alpha};
use crate::ui::{self, ACCENTS, Icon, Theme, WALLPAPERS};

pub struct Ctx<'a> {
    pub fonts: &'a mut Fonts,
    pub mono: &'a Mono,
    pub theme: Theme,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AppKind {
    Explorer,
    Notepad,
    Terminal,
    TaskManager,
    Settings,
    About,
}

pub enum Action {
    Open(AppKind, Option<String>),
    SetTheme(Theme),
    Close,
}

pub trait App {
    fn kind(&self) -> AppKind;
    fn title(&self) -> String;
    fn icon(&self) -> Icon {
        icon_for(self.kind())
    }
    fn size(&self) -> (i32, i32);
    /// Draws the content into `r` (canvas coordinates).
    fn draw(&mut self, cv: &mut Canvas, r: Rect, cx: &mut Ctx);
    /// Left click at content-relative (`x`, `y`).
    fn click(&mut self, x: i32, y: i32, double: bool, cx: &mut Ctx) -> Vec<Action>;
    /// Pointer moved to content-relative (`x`, `y`); returns true to redraw.
    fn hover(&mut self, _x: i32, _y: i32) -> bool {
        false
    }
    fn key(&mut self, _e: &InputEvent, _cx: &mut Ctx) -> Vec<Action> {
        Vec::new()
    }
    /// Mouse wheel turned by `notches` (positive scrolls down); returns
    /// true to redraw.
    fn wheel(&mut self, _notches: i32) -> bool {
        false
    }
    /// Called about once a second; returns true to redraw.
    fn tick(&mut self) -> bool {
        false
    }
}

pub fn icon_for(kind: AppKind) -> Icon {
    match kind {
        AppKind::Explorer => Icon::Explorer,
        AppKind::Notepad => Icon::Notepad,
        AppKind::Terminal => Icon::Terminal,
        AppKind::TaskManager => Icon::TaskManager,
        AppKind::Settings => Icon::Settings,
        AppKind::About => Icon::About,
    }
}

pub fn name_of(kind: AppKind) -> &'static str {
    match kind {
        AppKind::Explorer => "File Explorer",
        AppKind::Notepad => "Notepad",
        AppKind::Terminal => "Terminal",
        AppKind::TaskManager => "Task Manager",
        AppKind::Settings => "Settings",
        AppKind::About => "About Meridian",
    }
}

pub fn create(kind: AppKind, arg: Option<String>) -> Box<dyn App> {
    match kind {
        AppKind::Explorer => Box::new(Explorer::new(arg.unwrap_or_default())),
        AppKind::Notepad => Box::new(Notepad::new(arg)),
        AppKind::Terminal => Box::new(Terminal::new()),
        AppKind::TaskManager => Box::new(TaskManager::new()),
        AppKind::Settings => Box::new(Settings::new()),
        AppKind::About => Box::new(About { w: 500, h: 300 }),
    }
}

pub fn error_text(code: i64) -> &'static str {
    match code {
        status::OBJECT_NAME_NOT_FOUND => "not found",
        status::ACCESS_DENIED => "access denied",
        status::FILE_IS_A_DIRECTORY => "is a folder",
        status::NOT_A_DIRECTORY => "not a folder",
        status::INVALID_IMAGE_FORMAT => "not a program Meridian can run yet",
        status::NO_MEMORY => "out of memory",
        status::DEVICE_NOT_READY => "disk error",
        _ => "failed",
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit('\\').next().unwrap_or(path)
}

fn extension(name: &str) -> String {
    name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('\\') { format!("{dir}{name}") } else { format!("{dir}\\{name}") }
}

fn parent(path: &str) -> String {
    let trimmed = path.trim_end_matches('\\');
    match trimmed.rfind('\\') {
        Some(i) if i >= 2 => {
            let p = &trimmed[..i];
            if p.len() == 2 { format!("{p}\\") } else { p.to_string() }
        }
        _ => String::new(), // "This PC"
    }
}

fn human_size(bytes: u64) -> String {
    match bytes {
        0..1024 => format!("{bytes} bytes"),
        1024..1_048_576 => format!("{} KB", bytes.div_ceil(1024)),
        _ => format!("{}.{} MB", bytes / 1_048_576, bytes % 1_048_576 * 10 / 1_048_576),
    }
}

/// Opens a file the way the shell would: folders in Explorer, text in
/// Notepad, programs by starting a process.
pub enum Opened {
    Actions(Vec<Action>),
    Started(Handle, String),
    Message(String),
}

pub fn open_path(path: &str, is_dir: bool) -> Opened {
    if is_dir {
        return Opened::Actions(vec![Action::Open(AppKind::Explorer, Some(path.to_string()))]);
    }
    match extension(file_name(path)).as_str() {
        "txt" | "log" | "md" | "ini" | "cfg" | "json" | "csv" => {
            Opened::Actions(vec![Action::Open(AppKind::Notepad, Some(path.to_string()))])
        }
        "exe" => match sys::create_process(path, 0) {
            Ok(h) => Opened::Started(h, file_name(path).to_string()),
            Err(e) => Opened::Message(format!("Could not start {}: {}", file_name(path), error_text(e))),
        },
        _ => Opened::Message(format!("Meridian has no app to open {} files yet", file_name(path))),
    }
}

// ---------------------------------------------------------------------------
// File Explorer
// ---------------------------------------------------------------------------

pub struct Explorer {
    path: String,
    entries: Vec<DirEntry>,
    drives: Vec<char>,
    selected: Option<usize>,
    scroll: usize,
    history: Vec<String>,
    status: String,
    error: Option<String>,
    children: Vec<(Handle, String)>,
    w: i32,
    h: i32,
}

const TOOLBAR_H: i32 = 44;
const NAV_W: i32 = 170;
const ROW_H: i32 = 28;
const STATUS_H: i32 = 26;

impl Explorer {
    fn new(path: String) -> Self {
        let drives = ('C'..='H').filter(|d| sys::list_directory(&format!("{d}:\\")).is_ok()).collect();
        let mut e = Self {
            path: String::new(),
            entries: Vec::new(),
            drives,
            selected: None,
            scroll: 0,
            history: Vec::new(),
            status: String::new(),
            error: None,
            children: Vec::new(),
            w: 860,
            h: 520,
        };
        e.navigate(path, false);
        e
    }

    fn navigate(&mut self, path: String, remember: bool) {
        if remember && self.path != path {
            self.history.push(self.path.clone());
        }
        self.selected = None;
        self.scroll = 0;
        self.error = None;
        self.path = path;
        if self.path.is_empty() {
            self.entries = Vec::new();
            self.status = format!("{} drive(s)", self.drives.len());
            return;
        }
        match sys::list_directory(&self.path) {
            Ok(mut list) => {
                list.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
                self.status = format!("{} items", list.len());
                self.entries = list;
            }
            Err(e) => {
                self.entries = Vec::new();
                self.error = Some(format!("Cannot open {}: {}", self.path, error_text(e)));
                self.status.clear();
            }
        }
    }

    fn rows_visible(&self) -> usize {
        ((self.h - TOOLBAR_H - ROW_H - STATUS_H) / ROW_H).max(1) as usize
    }

    fn open_entry(&mut self, i: usize) -> Vec<Action> {
        let Some(e) = self.entries.get(i).cloned() else { return Vec::new() };
        let path = join(&self.path, &e.name);
        if e.is_dir {
            self.navigate(path, true);
            return Vec::new();
        }
        match open_path(&path, false) {
            Opened::Actions(a) => a,
            Opened::Started(h, name) => {
                self.status = format!("Started {name}; it reports to the system log");
                self.children.push((h, name));
                Vec::new()
            }
            Opened::Message(m) => {
                self.status = m;
                Vec::new()
            }
        }
    }
}

impl App for Explorer {
    fn kind(&self) -> AppKind {
        AppKind::Explorer
    }
    fn title(&self) -> String {
        if self.path.is_empty() { "This PC - File Explorer".into() } else { format!("{} - File Explorer", self.path) }
    }
    fn size(&self) -> (i32, i32) {
        (860, 520)
    }
    fn wheel(&mut self, notches: i32) -> bool {
        let max = self.entries.len().saturating_sub(self.rows_visible());
        let old = self.scroll;
        self.scroll = (self.scroll as i32 + notches * 3).clamp(0, max as i32) as usize;
        self.scroll != old
    }

    fn draw(&mut self, cv: &mut Canvas, r: Rect, cx: &mut Ctx) {
        let t = cx.theme;
        let f = &mut *cx.fonts;
        self.w = r.w;
        self.h = r.h;
        cv.fill(r, t.window());
        // Toolbar: back, up, refresh, address.
        for i in 0..3 {
            let b = Rect::new(r.x + 8 + i * 36, r.y + 8, 30, 28);
            cv.fill_rounded(b, 4, t.hover());
            let (cx_, cy) = (b.x as f32 + 15.0, b.y as f32 + 14.0);
            let c = t.text();
            match i {
                0 => {
                    cv.line(cx_ - 6.0, cy, cx_ + 6.0, cy, 1.4, c);
                    cv.line(cx_ - 6.0, cy, cx_ - 1.0, cy - 5.0, 1.4, c);
                    cv.line(cx_ - 6.0, cy, cx_ - 1.0, cy + 5.0, 1.4, c);
                }
                1 => {
                    cv.line(cx_, cy - 6.0, cx_, cy + 6.0, 1.4, c);
                    cv.line(cx_, cy - 6.0, cx_ - 5.0, cy - 1.0, 1.4, c);
                    cv.line(cx_, cy - 6.0, cx_ + 5.0, cy - 1.0, 1.4, c);
                }
                _ => {
                    // A 300-degree arc in 30-degree steps, then an arrowhead.
                    let (cos, sin) = (0.866_f32, 0.5_f32);
                    let (mut px, mut py) = (0.0_f32, -6.0_f32);
                    for _ in 0..10 {
                        let (nx, ny) = (px * cos - py * sin, px * sin + py * cos);
                        cv.line(cx_ + px, cy + py, cx_ + nx, cy + ny, 1.4, c);
                        (px, py) = (nx, ny);
                    }
                    cv.line(cx_ + px, cy + py, cx_ + px + 4.0, cy + py - 1.0, 1.4, c);
                    cv.line(cx_ + px, cy + py, cx_ + px + 1.0, cy + py - 4.5, 1.4, c);
                }
            }
        }
        let addr = Rect::new(r.x + 120, r.y + 8, r.w - 132, 28);
        cv.rounded_box(addr, 4, t.border(), t.surface());
        let shown = if self.path.is_empty() { "This PC" } else { self.path.as_str() };
        f.draw_fit(cv, addr.x + 10, addr.y + 5, shown, addr.w - 20, 14, Weight::Regular, t.text());
        // Navigation pane.
        let nav = Rect::new(r.x, r.y + TOOLBAR_H, NAV_W, r.h - TOOLBAR_H - STATUS_H);
        cv.fill(nav, t.surface());
        let mut ny = nav.y + 8;
        let selected_nav = |label_path: &str| label_path == self.path;
        let mut nav_item = |cv: &mut Canvas, f: &mut Fonts, icon: Icon, label: &str, selected: bool| {
            if selected {
                cv.fill_rounded(Rect::new(nav.x + 6, ny, NAV_W - 12, 28), 4, t.selection());
            }
            ui::icon(cv, icon, nav.x + 14, ny + 6, 16, &t);
            f.draw(cv, nav.x + 38, ny + 5, label, 14, Weight::Regular, t.text());
            ny += 32;
        };
        nav_item(cv, f, Icon::ThisPc, "This PC", self.path.is_empty());
        for d in &self.drives {
            let p = format!("{d}:\\");
            nav_item(cv, f, Icon::Drive, &format!("Local Disk ({d}:)"), selected_nav(&p));
        }
        nav_item(cv, f, Icon::Folder, "MERIDIAN", self.path.eq_ignore_ascii_case("C:\\MERIDIAN"));
        // Main area.
        let main = Rect::new(r.x + NAV_W, r.y + TOOLBAR_H, r.w - NAV_W, r.h - TOOLBAR_H - STATUS_H);
        cv.fill(main, t.window());
        if let Some(err) = &self.error {
            f.draw(cv, main.x + 20, main.y + 20, err, 14, Weight::Regular, rgb(0xE1, 0x1D, 0x48));
        } else if self.path.is_empty() {
            f.draw(cv, main.x + 20, main.y + 14, "Devices and drives", 14, Weight::Semibold, t.text());
            for (i, d) in self.drives.iter().enumerate() {
                let tile = Rect::new(main.x + 20 + i as i32 * 230, main.y + 44, 220, 64);
                cv.rounded_box(tile, 6, t.border(), t.surface());
                ui::icon(cv, Icon::Drive, tile.x + 12, tile.y + 12, 40, &t);
                f.draw(cv, tile.x + 64, tile.y + 12, &format!("Local Disk ({d}:)"), 14, Weight::Semibold, t.text());
                f.draw(cv, tile.x + 64, tile.y + 34, "FAT32, read-only", 12, Weight::Regular, t.text_dim());
            }
        } else {
            let header = Rect::new(main.x, main.y, main.w, ROW_H);
            let (cname, ctype, csize) = (main.x + 44, main.x + main.w - 270, main.x + main.w - 120);
            f.draw(cv, cname, header.y + 6, "Name", 12, Weight::Regular, t.text_dim());
            f.draw(cv, ctype, header.y + 6, "Type", 12, Weight::Regular, t.text_dim());
            f.draw(cv, csize, header.y + 6, "Size", 12, Weight::Regular, t.text_dim());
            cv.fill(Rect::new(main.x, header.bottom() - 1, main.w, 1), t.border());
            for (row, (i, e)) in self.entries.iter().enumerate().skip(self.scroll).take(self.rows_visible()).enumerate()
            {
                let y = main.y + ROW_H + row as i32 * ROW_H;
                if self.selected == Some(i) {
                    cv.fill_rounded(Rect::new(main.x + 4, y + 1, main.w - 8, ROW_H - 2), 4, t.selection());
                }
                let ext = extension(&e.name);
                let icon = if e.is_dir {
                    Icon::Folder
                } else if ext == "exe" {
                    Icon::Program
                } else if matches!(ext.as_str(), "txt" | "md" | "log" | "ini" | "cfg") {
                    Icon::TextFile
                } else {
                    Icon::File
                };
                ui::icon(cv, icon, main.x + 16, y + 5, 18, &t);
                f.draw_fit(cv, cname, y + 5, &e.name, ctype - cname - 12, 14, Weight::Regular, t.text());
                let kind = if e.is_dir {
                    "File folder".to_string()
                } else if ext.is_empty() {
                    "File".to_string()
                } else if ext == "exe" {
                    "Application".to_string()
                } else {
                    format!("{} file", ext.to_ascii_uppercase())
                };
                f.draw(cv, ctype, y + 5, &kind, 14, Weight::Regular, t.text_dim());
                if !e.is_dir {
                    f.draw(cv, csize, y + 5, &human_size(e.size), 14, Weight::Regular, t.text_dim());
                }
            }
        }
        // Status bar.
        let sb = Rect::new(r.x, r.bottom() - STATUS_H, r.w, STATUS_H);
        cv.fill(sb, t.surface());
        cv.fill(Rect::new(sb.x, sb.y, sb.w, 1), t.border());
        f.draw_fit(cv, sb.x + 12, sb.y + 5, &self.status, sb.w - 24, 12, Weight::Regular, t.text_dim());
    }

    fn click(&mut self, x: i32, y: i32, double: bool, _cx: &mut Ctx) -> Vec<Action> {
        if y < TOOLBAR_H {
            match (x - 8) / 36 {
                0 if x >= 8 => {
                    if let Some(prev) = self.history.pop() {
                        self.navigate(prev, false);
                    }
                }
                1 => {
                    if !self.path.is_empty() {
                        let up = parent(&self.path);
                        self.navigate(up, true);
                    }
                }
                2 => {
                    let p = self.path.clone();
                    self.navigate(p, false);
                }
                _ => {}
            }
            return Vec::new();
        }
        if x < NAV_W {
            let i = (y - TOOLBAR_H - 8) / 32;
            let target = if i == 0 {
                Some(String::new())
            } else if (i as usize) <= self.drives.len() {
                Some(format!("{}:\\", self.drives[i as usize - 1]))
            } else if i as usize == self.drives.len() + 1 {
                Some("C:\\MERIDIAN".to_string())
            } else {
                None
            };
            if let Some(p) = target {
                self.navigate(p, true);
            }
            return Vec::new();
        }
        if self.path.is_empty() {
            let i = ((x - NAV_W - 20) / 230) as usize;
            if y > TOOLBAR_H + 44 && y < TOOLBAR_H + 108 && i < self.drives.len() && double {
                let p = format!("{}:\\", self.drives[i]);
                self.navigate(p, true);
            }
            return Vec::new();
        }
        let row = (y - TOOLBAR_H - ROW_H) / ROW_H;
        if row < 0 || y >= self.h - STATUS_H {
            return Vec::new();
        }
        let i = self.scroll + row as usize;
        if i >= self.entries.len() {
            self.selected = None;
            return Vec::new();
        }
        self.selected = Some(i);
        if double { self.open_entry(i) } else { Vec::new() }
    }

    fn key(&mut self, e: &InputEvent, _cx: &mut Ctx) -> Vec<Action> {
        let n = self.entries.len();
        match e.key {
            key::DOWN if n > 0 => self.selected = Some(self.selected.map_or(0, |s| (s + 1).min(n - 1))),
            key::UP if n > 0 => self.selected = Some(self.selected.map_or(0, |s| s.saturating_sub(1))),
            key::ENTER => {
                if let Some(i) = self.selected {
                    return self.open_entry(i);
                }
            }
            key::BACKSPACE => {
                if !self.path.is_empty() {
                    let up = parent(&self.path);
                    self.navigate(up, true);
                }
            }
            _ => {}
        }
        if let Some(s) = self.selected {
            let vis = self.rows_visible();
            if s < self.scroll {
                self.scroll = s;
            } else if s >= self.scroll + vis {
                self.scroll = s + 1 - vis;
            }
        }
        Vec::new()
    }

    fn tick(&mut self) -> bool {
        let mut changed = false;
        self.children.retain(|(h, name)| match sys::process_status(h) {
            Ok(Some(code)) => {
                self.status = format!("{name} finished with exit code {:#x}", code as u32);
                changed = true;
                false
            }
            _ => true,
        });
        changed
    }
}

// ---------------------------------------------------------------------------
// Notepad
// ---------------------------------------------------------------------------

pub struct Notepad {
    path: Option<String>,
    lines: Vec<String>,
    row: usize,
    col: usize,
    scroll: usize,
    modified: bool,
    note: String,
    h: i32,
}

impl Notepad {
    fn new(path: Option<String>) -> Self {
        let mut n = Self {
            path: path.clone(),
            lines: vec![String::new()],
            row: 0,
            col: 0,
            scroll: 0,
            modified: false,
            note: String::new(),
            h: 400,
        };
        if let Some(p) = path {
            match sys::read_file(&p) {
                Ok(data) => {
                    let text = String::from_utf8_lossy(&data).replace('\r', "");
                    n.lines = text.split('\n').map(String::from).collect();
                }
                Err(e) => n.note = format!("Could not open {p}: {}", error_text(e)),
            }
        }
        n
    }

    fn visible(&self) -> usize {
        ((self.h - 32) / 20).max(1) as usize
    }

    fn keep_visible(&mut self) {
        let vis = self.visible();
        if self.row < self.scroll {
            self.scroll = self.row;
        } else if self.row >= self.scroll + vis {
            self.scroll = self.row + 1 - vis;
        }
    }
}

fn char_index(s: &str, col: usize) -> usize {
    s.char_indices().nth(col).map_or(s.len(), |(i, _)| i)
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
        (720, 480)
    }
    fn wheel(&mut self, notches: i32) -> bool {
        let max = self.lines.len().saturating_sub(self.visible());
        let old = self.scroll;
        self.scroll = (self.scroll as i32 + notches * 3).clamp(0, max as i32) as usize;
        self.scroll != old
    }

    fn draw(&mut self, cv: &mut Canvas, r: Rect, cx: &mut Ctx) {
        let t = cx.theme;
        let f = &mut *cx.fonts;
        self.h = r.h;
        cv.fill(r, t.surface());
        let text_area = Rect::new(r.x, r.y, r.w, r.h - 26);
        cv.set_clip(text_area);
        for (i, line) in self.lines.iter().enumerate().skip(self.scroll).take(self.visible() + 1) {
            let y = r.y + 8 + (i - self.scroll) as i32 * 20;
            f.draw(cv, r.x + 12, y, line, 14, Weight::Regular, t.text());
            if i == self.row {
                let prefix = &line[..char_index(line, self.col)];
                let cx_ = r.x + 12 + f.measure(prefix, 14, Weight::Regular);
                cv.fill(Rect::new(cx_, y + 1, 1, 18), t.text());
            }
        }
        cv.set_clip(r);
        let sb = Rect::new(r.x, r.bottom() - 26, r.w, 26);
        cv.fill(sb, t.window());
        cv.fill(Rect::new(sb.x, sb.y, sb.w, 1), t.border());
        let left =
            if self.note.is_empty() { format!("Ln {}, Col {}", self.row + 1, self.col + 1) } else { self.note.clone() };
        f.draw(cv, sb.x + 12, sb.y + 5, &left, 12, Weight::Regular, t.text_dim());
        let right = if self.modified {
            "Edited in memory - Meridian's storage is read-only, so it can't be saved yet"
        } else {
            "UTF-8"
        };
        let rw = f.measure(right, 12, Weight::Regular);
        f.draw(cv, sb.right() - rw - 12, sb.y + 5, right, 12, Weight::Regular, t.text_dim());
    }

    fn click(&mut self, x: i32, y: i32, _double: bool, cx: &mut Ctx) -> Vec<Action> {
        let row = (self.scroll + ((y - 8).max(0) / 20) as usize).min(self.lines.len() - 1);
        self.row = row;
        let line = &self.lines[row];
        let mut col = 0;
        for (i, _) in line.chars().enumerate() {
            let w = cx.fonts.measure(&line[..char_index(line, i + 1)], 14, Weight::Regular);
            if 12 + w > x + 3 {
                break;
            }
            col = i + 1;
        }
        self.col = col;
        Vec::new()
    }

    fn key(&mut self, e: &InputEvent, _cx: &mut Ctx) -> Vec<Action> {
        let len = |s: &String| s.chars().count();
        match e.key {
            key::CHAR => {
                if let Some(c) = char::from_u32(e.ch) {
                    let line = &mut self.lines[self.row];
                    let at = char_index(line, self.col);
                    line.insert(at, c);
                    self.col += 1;
                    self.modified = true;
                }
            }
            key::ENTER => {
                let line = &mut self.lines[self.row];
                let at = char_index(line, self.col);
                let rest = line.split_off(at);
                self.lines.insert(self.row + 1, rest);
                self.row += 1;
                self.col = 0;
                self.modified = true;
            }
            key::BACKSPACE => {
                if self.col > 0 {
                    let line = &mut self.lines[self.row];
                    let at = char_index(line, self.col - 1);
                    line.remove(at);
                    self.col -= 1;
                    self.modified = true;
                } else if self.row > 0 {
                    let line = self.lines.remove(self.row);
                    self.row -= 1;
                    self.col = len(&self.lines[self.row]);
                    self.lines[self.row].push_str(&line);
                    self.modified = true;
                }
            }
            key::LEFT => {
                if self.col > 0 {
                    self.col -= 1;
                } else if self.row > 0 {
                    self.row -= 1;
                    self.col = len(&self.lines[self.row]);
                }
            }
            key::RIGHT => {
                if self.col < len(&self.lines[self.row]) {
                    self.col += 1;
                } else if self.row + 1 < self.lines.len() {
                    self.row += 1;
                    self.col = 0;
                }
            }
            key::UP => self.row = self.row.saturating_sub(1),
            key::DOWN => self.row = (self.row + 1).min(self.lines.len() - 1),
            key::PAGE_UP => self.row = self.row.saturating_sub(self.visible()),
            key::PAGE_DOWN => self.row = (self.row + self.visible()).min(self.lines.len() - 1),
            key::HOME => self.col = 0,
            key::END => self.col = len(&self.lines[self.row]),
            _ => return Vec::new(),
        }
        self.col = self.col.min(len(&self.lines[self.row]));
        self.keep_visible();
        Vec::new()
    }
}

// ---------------------------------------------------------------------------
// Terminal
// ---------------------------------------------------------------------------

pub struct Terminal {
    lines: Vec<String>,
    input: String,
    cwd: String,
    children: Vec<(Handle, String)>,
    rows: usize,
}

impl Terminal {
    fn new() -> Self {
        let mut t =
            Self { lines: Vec::new(), input: String::new(), cwd: "C:\\".into(), children: Vec::new(), rows: 24 };
        t.print("Meridian Terminal (user mode). Type 'help' for commands.");
        t.print("");
        t
    }

    fn print(&mut self, s: &str) {
        for line in s.split('\n') {
            self.lines.push(line.to_string());
        }
        let excess = self.lines.len().saturating_sub(500);
        self.lines.drain(..excess);
    }

    fn resolve(&self, arg: &str) -> String {
        let arg = arg.trim_matches('"').replace('/', "\\");
        if arg.len() >= 2 && arg.as_bytes()[1] == b':' {
            return arg;
        }
        let mut path = self.cwd.clone();
        for part in arg.split('\\').filter(|p| !p.is_empty()) {
            match part {
                "." => {}
                ".." => {
                    let p = parent(&path);
                    if !p.is_empty() {
                        path = p;
                    }
                }
                name => path = join(&path, name),
            }
        }
        path
    }

    fn run_command(&mut self, line: &str) -> Vec<Action> {
        let line = line.trim();
        let (cmd, rest) = line.split_once(' ').unwrap_or((line, ""));
        let rest = rest.trim();
        match cmd.to_ascii_lowercase().as_str() {
            "" => {}
            "help" => self.print(
                "  dir [path]      list a folder        cd <path>     change folder\n  type <file>     show a text file      run <file>    start a program\n  ps              processes             threads       kernel threads\n  mem             memory                uptime        time since boot\n  date / time     clock                 ver           version\n  echo <text>     print text            cls           clear the screen\n  exit            close this window",
            ),
            "cls" | "clear" => self.lines.clear(),
            "ver" => {
                let info = meridian_rt::system_info().ok();
                let version = info.map(|i| sys::name_of(&i.kernel_version).to_string()).unwrap_or_default();
                self.print(&format!("Meridian OS {version} (pre-alpha), original Meridian kernel"));
            }
            "echo" => self.print(rest),
            "dir" | "ls" => {
                let path = if rest.is_empty() { self.cwd.clone() } else { self.resolve(rest) };
                match sys::list_directory(&path) {
                    Ok(entries) => {
                        self.print(&format!(" Directory of {path}"));
                        self.print("");
                        let mut files = 0;
                        for e in &entries {
                            if e.is_dir {
                                self.print(&format!("  <DIR>          {}", e.name));
                            } else {
                                files += 1;
                                self.print(&format!("  {:>14}  {}", e.size, e.name));
                            }
                        }
                        self.print(&format!("  {} file(s), {} dir(s)", files, entries.len() - files));
                    }
                    Err(e) => self.print(&format!("{path}: {}", error_text(e))),
                }
            }
            "cd" => {
                if rest.is_empty() {
                    let cwd = self.cwd.clone();
                    self.print(&cwd);
                } else {
                    let path = self.resolve(rest);
                    match sys::list_directory(&path) {
                        Ok(_) => self.cwd = path,
                        Err(e) => self.print(&format!("{path}: {}", error_text(e))),
                    }
                }
            }
            "type" | "cat" => {
                let path = self.resolve(rest);
                match sys::read_file(&path) {
                    Ok(data) => {
                        let text = String::from_utf8_lossy(&data[..data.len().min(16384)]).replace('\r', "");
                        self.print(&text);
                    }
                    Err(e) => self.print(&format!("{path}: {}", error_text(e))),
                }
            }
            "run" | "start" => {
                let mut words = rest.split_whitespace();
                let target = words.next().unwrap_or("");
                let arg = words.next().and_then(|a| a.parse().ok()).unwrap_or(0);
                let path = self.resolve(target);
                match sys::create_process(&path, arg) {
                    Ok(h) => {
                        self.print(&format!("Started {} (its messages go to the system log)", file_name(&path)));
                        self.children.push((h, file_name(&path).to_string()));
                    }
                    Err(e) => self.print(&format!("Could not start {path}: {}", error_text(e))),
                }
            }
            "ps" => {
                self.print("  PID   Name              Memory    Handles  Status");
                for p in sys::query_processes() {
                    let status = if p.running != 0 { "running".to_string() } else { format!("exited {:#x}", p.exit_status as u32) };
                    self.print(&format!(
                        "  {:<5} {:<17} {:>6} KB  {:>7}  {}",
                        p.id,
                        sys::name_of(&p.name),
                        p.committed / 1024,
                        p.handles,
                        status
                    ));
                }
            }
            "threads" => {
                self.print("  ID    Name                     Prio  State     CPU  Time");
                for t in sys::query_threads() {
                    let state = ["init", "ready", "running", "waiting", "done"].get(t.state as usize).copied().unwrap_or("?");
                    self.print(&format!(
                        "  {:<5} {:<24} {:>4}  {:<8} {:>4}  {}.{:03}s",
                        t.id,
                        sys::name_of(&t.name),
                        t.priority,
                        state,
                        t.cpu,
                        t.cpu_ms / 1000,
                        t.cpu_ms % 1000
                    ));
                }
            }
            "mem" => match meridian_rt::system_info() {
                Ok(i) => self.print(&format!(
                    "  {} MiB of RAM, {} MiB free, {} processors, {}-byte pages",
                    i.physical_memory >> 20,
                    i.available_memory >> 20,
                    i.processor_count,
                    i.page_size
                )),
                Err(_) => self.print("system information unavailable"),
            },
            "uptime" => {
                let s = meridian_rt::uptime_us() / 1_000_000;
                self.print(&format!("  up {}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60));
            }
            "date" | "time" => {
                let t = sys::query_time();
                self.print(&format!("  {:04}-{:02}-{:02} {:02}:{:02}:{:02}", t.year, t.month, t.day, t.hour, t.minute, t.second));
            }
            "exit" => return vec![Action::Close],
            other => self.print(&format!("'{other}' is not a command. Type 'help'.")),
        }
        Vec::new()
    }
}

impl App for Terminal {
    fn kind(&self) -> AppKind {
        AppKind::Terminal
    }
    fn title(&self) -> String {
        "Terminal".into()
    }
    fn size(&self) -> (i32, i32) {
        (760, 460)
    }

    fn draw(&mut self, cv: &mut Canvas, r: Rect, cx: &mut Ctx) {
        cv.fill(r, rgb(0x0C, 0x0C, 0x0C));
        self.rows = ((r.h - 12) / MONO_H).max(1) as usize;
        let prompt = format!("{}> {}", self.cwd, self.input);
        let start = (self.lines.len() + 1).saturating_sub(self.rows);
        let mut y = r.y + 6;
        for line in self.lines.iter().skip(start) {
            cx.mono.draw(cv, r.x + 8, y, line, rgb(0xCC, 0xCC, 0xCC));
            y += MONO_H;
        }
        cx.mono.draw(cv, r.x + 8, y, &prompt, rgb(0xF2, 0xF2, 0xF2));
        let caret_x = r.x + 8 + prompt.chars().count() as i32 * MONO_W;
        cv.fill(Rect::new(caret_x, y + MONO_H - 3, MONO_W, 2), rgb(0xF2, 0xF2, 0xF2));
    }

    fn click(&mut self, _x: i32, _y: i32, _double: bool, _cx: &mut Ctx) -> Vec<Action> {
        Vec::new()
    }

    fn key(&mut self, e: &InputEvent, _cx: &mut Ctx) -> Vec<Action> {
        match e.key {
            key::CHAR => {
                if let Some(c) = char::from_u32(e.ch) {
                    if self.input.len() < 200 {
                        self.input.push(c);
                    }
                }
            }
            key::BACKSPACE => {
                self.input.pop();
            }
            key::ENTER => {
                let line = core::mem::take(&mut self.input);
                let echo = format!("{}> {line}", self.cwd);
                self.print(&echo);
                return self.run_command(&line);
            }
            _ => {}
        }
        Vec::new()
    }

    fn tick(&mut self) -> bool {
        let mut finished = Vec::new();
        self.children.retain(|(h, name)| match sys::process_status(h) {
            Ok(Some(code)) => {
                finished.push(format!("[{name} exited with code {:#x}]", code as u32));
                false
            }
            _ => true,
        });
        let changed = !finished.is_empty();
        for line in finished {
            self.print(&line);
        }
        changed
    }
}

// ---------------------------------------------------------------------------
// Task Manager
// ---------------------------------------------------------------------------

pub struct TaskManager {
    tab: usize,
    processes: Vec<sys::ProcessInfo>,
    threads: Vec<sys::ThreadInfo>,
    info: Option<meridian_rt::SystemInfo>,
    history: Vec<u32>,
    last_busy: u64,
    last_time: u64,
}

impl TaskManager {
    fn new() -> Self {
        let mut t = Self {
            tab: 0,
            processes: Vec::new(),
            threads: Vec::new(),
            info: None,
            history: Vec::new(),
            last_busy: 0,
            last_time: 0,
        };
        t.refresh();
        t
    }

    fn refresh(&mut self) {
        self.processes = sys::query_processes();
        self.threads = sys::query_threads();
        self.info = meridian_rt::system_info().ok();
        // CPU use: time spent in non-idle threads over the wall time of all
        // processors (idle threads accumulate no CPU time).
        let busy: u64 = self.threads.iter().filter(|t| t.priority > 0).map(|t| t.cpu_ms).sum();
        let now = meridian_rt::uptime_us() / 1000;
        let cpus = self.info.map_or(1, |i| i.processor_count.max(1)) as u64;
        if self.last_time > 0 && now > self.last_time {
            let used = busy.saturating_sub(self.last_busy) * 100 / ((now - self.last_time) * cpus).max(1);
            self.history.push(used.min(100) as u32);
            let excess = self.history.len().saturating_sub(60);
            self.history.drain(..excess);
        }
        self.last_busy = busy;
        self.last_time = now;
    }
}

impl App for TaskManager {
    fn kind(&self) -> AppKind {
        AppKind::TaskManager
    }
    fn title(&self) -> String {
        "Task Manager".into()
    }
    fn size(&self) -> (i32, i32) {
        (780, 520)
    }

    fn draw(&mut self, cv: &mut Canvas, r: Rect, cx: &mut Ctx) {
        let t = cx.theme;
        let f = &mut *cx.fonts;
        cv.fill(r, t.window());
        for (i, name) in ["Processes", "Performance", "Threads"].iter().enumerate() {
            let tab = Rect::new(r.x + 12 + i as i32 * 120, r.y + 8, 112, 32);
            if self.tab == i {
                cv.fill_rounded(tab, 5, t.surface());
                cv.fill_rounded(Rect::new(tab.x + 36, tab.bottom() - 4, 40, 3), 1, t.accent);
            }
            let w = f.measure(name, 14, Weight::Regular);
            f.draw(
                cv,
                tab.x + (tab.w - w) / 2,
                tab.y + 6,
                name,
                14,
                if self.tab == i { Weight::Semibold } else { Weight::Regular },
                t.text(),
            );
        }
        let body = Rect::new(r.x + 12, r.y + 48, r.w - 24, r.h - 60);
        cv.rounded_box(body, 6, t.border(), t.surface());
        let cpu_now = self.history.last().copied().unwrap_or(0);
        match self.tab {
            0 => {
                let cols = [("Name", 16), ("PID", 260), ("Status", 340), ("Memory", 480), ("Handles", 600)];
                for (label, x) in cols {
                    f.draw(cv, body.x + x, body.y + 10, label, 12, Weight::Semibold, t.text_dim());
                }
                for (i, p) in self.processes.iter().enumerate() {
                    let y = body.y + 36 + i as i32 * 26;
                    if y > body.bottom() - 26 {
                        break;
                    }
                    let status = if p.running != 0 {
                        "Running".to_string()
                    } else {
                        format!("Exited ({:#x})", p.exit_status as u32)
                    };
                    f.draw(cv, body.x + 16, y, sys::name_of(&p.name), 14, Weight::Regular, t.text());
                    f.draw(cv, body.x + 260, y, &p.id.to_string(), 14, Weight::Regular, t.text());
                    f.draw(cv, body.x + 340, y, &status, 14, Weight::Regular, t.text());
                    f.draw(cv, body.x + 480, y, &format!("{} KB", p.committed / 1024), 14, Weight::Regular, t.text());
                    f.draw(cv, body.x + 600, y, &p.handles.to_string(), 14, Weight::Regular, t.text());
                }
                let n = self.processes.len();
                f.draw(
                    cv,
                    body.x + 16,
                    body.bottom() - 24,
                    &format!("{n} user process(es) - kernel threads are on the Threads tab"),
                    12,
                    Weight::Regular,
                    t.text_dim(),
                );
            }
            1 => {
                f.draw(cv, body.x + 20, body.y + 14, "CPU", 20, Weight::Semibold, t.text());
                f.draw(
                    cv,
                    body.x + 20,
                    body.y + 44,
                    &format!("{cpu_now}% utilisation"),
                    14,
                    Weight::Regular,
                    t.text_dim(),
                );
                let graph = Rect::new(body.x + 20, body.y + 74, body.w - 40, 180);
                cv.stroke(graph, with_alpha(t.accent, 160));
                for gx in 1..6 {
                    cv.fill(Rect::new(graph.x + graph.w * gx / 6, graph.y, 1, graph.h), with_alpha(t.accent, 40));
                }
                let n = self.history.len();
                for (i, v) in self.history.iter().enumerate() {
                    let bw = (graph.w / 60).max(1);
                    let x = graph.right() - (n - i) as i32 * bw;
                    let h = (graph.h - 2) * *v as i32 / 100;
                    cv.fill(Rect::new(x, graph.bottom() - 1 - h, bw, h), with_alpha(t.accent, 150));
                }
                if let Some(info) = self.info {
                    let used = info.physical_memory - info.available_memory;
                    let my = graph.bottom() + 24;
                    f.draw(cv, body.x + 20, my, "Memory", 20, Weight::Semibold, t.text());
                    f.draw(
                        cv,
                        body.x + 20,
                        my + 30,
                        &format!("{} MiB in use of {} MiB", used >> 20, info.physical_memory >> 20),
                        14,
                        Weight::Regular,
                        t.text_dim(),
                    );
                    let bar = Rect::new(body.x + 20, my + 56, body.w - 40, 14);
                    cv.fill_rounded(bar, 7, t.hover());
                    let fill = (bar.w as u64 * used / info.physical_memory.max(1)) as i32;
                    cv.fill_rounded(Rect::new(bar.x, bar.y, fill.max(14), bar.h), 7, t.accent);
                    let s = meridian_rt::uptime_us() / 1_000_000;
                    let facts = format!(
                        "Processors: {}     Threads: {}     Processes: {}     Up time: {}:{:02}:{:02}",
                        info.processor_count,
                        self.threads.len(),
                        self.processes.len(),
                        s / 3600,
                        s / 60 % 60,
                        s % 60
                    );
                    f.draw(cv, body.x + 20, my + 84, &facts, 14, Weight::Regular, t.text());
                }
            }
            _ => {
                let cols =
                    [("Name", 16), ("ID", 250), ("Priority", 310), ("State", 400), ("CPU", 500), ("CPU time", 560)];
                for (label, x) in cols {
                    f.draw(cv, body.x + x, body.y + 10, label, 12, Weight::Semibold, t.text_dim());
                }
                for (i, th) in self.threads.iter().enumerate() {
                    let y = body.y + 36 + i as i32 * 24;
                    if y > body.bottom() - 24 {
                        break;
                    }
                    let state = ["Initialised", "Ready", "Running", "Waiting", "Finished"]
                        .get(th.state as usize)
                        .copied()
                        .unwrap_or("?");
                    f.draw(cv, body.x + 16, y, sys::name_of(&th.name), 14, Weight::Regular, t.text());
                    f.draw(cv, body.x + 250, y, &th.id.to_string(), 14, Weight::Regular, t.text());
                    f.draw(cv, body.x + 310, y, &th.priority.to_string(), 14, Weight::Regular, t.text());
                    f.draw(cv, body.x + 400, y, state, 14, Weight::Regular, t.text());
                    f.draw(cv, body.x + 500, y, &th.cpu.to_string(), 14, Weight::Regular, t.text());
                    f.draw(
                        cv,
                        body.x + 560,
                        y,
                        &format!("{}.{:03} s", th.cpu_ms / 1000, th.cpu_ms % 1000),
                        14,
                        Weight::Regular,
                        t.text(),
                    );
                }
            }
        }
    }

    fn click(&mut self, x: i32, y: i32, _double: bool, _cx: &mut Ctx) -> Vec<Action> {
        if (8..40).contains(&y) && x >= 12 {
            let i = ((x - 12) / 120) as usize;
            if i < 3 {
                self.tab = i;
            }
        }
        Vec::new()
    }

    fn tick(&mut self) -> bool {
        self.refresh();
        true
    }
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

pub struct Settings {
    page: usize,
    thumbs: Option<(Theme, Vec<Canvas>)>,
    w: i32,
}

const PAGES: [&str; 3] = ["Personalisation", "System", "About"];

impl Settings {
    fn new() -> Self {
        Self { page: 0, thumbs: None, w: 820 }
    }
}

impl App for Settings {
    fn kind(&self) -> AppKind {
        AppKind::Settings
    }
    fn title(&self) -> String {
        "Settings".into()
    }
    fn size(&self) -> (i32, i32) {
        (820, 560)
    }

    fn draw(&mut self, cv: &mut Canvas, r: Rect, cx: &mut Ctx) {
        let t = cx.theme;
        let f = &mut *cx.fonts;
        self.w = r.w;
        cv.fill(r, t.window());
        // Navigation.
        for (i, name) in PAGES.iter().enumerate() {
            let item = Rect::new(r.x + 12, r.y + 16 + i as i32 * 40, 200, 36);
            if self.page == i {
                cv.fill_rounded(item, 5, t.hover());
                cv.fill_rounded(Rect::new(item.x + 2, item.y + 10, 3, 16), 1, t.accent);
            }
            f.draw(cv, item.x + 16, item.y + 8, name, 14, Weight::Regular, t.text());
        }
        let x0 = r.x + 240;
        f.draw(cv, x0, r.y + 14, PAGES[self.page], 28, Weight::Semibold, t.text());
        let card = |cv: &mut Canvas, y: i32, h: i32| {
            let c = Rect::new(x0, y, r.right() - x0 - 20, h);
            cv.rounded_box(c, 6, t.border(), t.surface());
            c
        };
        match self.page {
            0 => {
                let c = card(cv, r.y + 70, 64);
                f.draw(cv, c.x + 16, c.y + 12, "Colour mode", 14, Weight::Semibold, t.text());
                f.draw(
                    cv,
                    c.x + 16,
                    c.y + 34,
                    "Light or dark windows, taskbar and menus",
                    12,
                    Weight::Regular,
                    t.text_dim(),
                );
                for (i, label) in ["Light", "Dark"].iter().enumerate() {
                    let b = Rect::new(c.right() - 200 + i as i32 * 92, c.y + 16, 84, 32);
                    ui::button(cv, f, &t, b, label, t.dark == (i == 1), false);
                }
                let c = card(cv, r.y + 146, 96);
                f.draw(cv, c.x + 16, c.y + 12, "Accent colour", 14, Weight::Semibold, t.text());
                for (i, (name, col)) in ACCENTS.iter().enumerate() {
                    let sx = c.x + 16 + i as i32 * 52;
                    let sw = Rect::new(sx, c.y + 44, 40, 40);
                    if t.accent == *col {
                        cv.fill_rounded(sw.grow(3), 8, t.text());
                    }
                    cv.fill_rounded(sw, 6, *col);
                    if t.accent == *col {
                        f.draw(cv, c.x + 340, c.y + 12, name, 12, Weight::Regular, t.text_dim());
                    }
                }
                let c = card(cv, r.y + 254, 64);
                f.draw(cv, c.x + 16, c.y + 12, "Taskbar alignment", 14, Weight::Semibold, t.text());
                f.draw(
                    cv,
                    c.x + 16,
                    c.y + 34,
                    "Where the Start button and apps sit",
                    12,
                    Weight::Regular,
                    t.text_dim(),
                );
                for (i, label) in ["Centre", "Left"].iter().enumerate() {
                    let b = Rect::new(c.right() - 200 + i as i32 * 92, c.y + 16, 84, 32);
                    ui::button(cv, f, &t, b, label, t.taskbar_left == (i == 1), false);
                }
                let c = card(cv, r.y + 330, 140);
                f.draw(cv, c.x + 16, c.y + 12, "Background", 14, Weight::Semibold, t.text());
                if self.thumbs.as_ref().is_none_or(|(th, _)| *th != t) {
                    let mut list = Vec::new();
                    for i in 0..WALLPAPERS.len() {
                        list.push(ui::wallpaper(144, 90, &Theme { wallpaper: i, ..t }));
                    }
                    self.thumbs = Some((t, list));
                }
                if let Some((_, thumbs)) = &self.thumbs {
                    for (i, th) in thumbs.iter().enumerate() {
                        let x = c.x + 16 + i as i32 * 160;
                        if t.wallpaper == i {
                            cv.fill_rounded(Rect::new(x - 3, c.y + 37, 150, 96), 6, t.accent);
                        }
                        cv.blit(th, th.bounds(), x, c.y + 40);
                        f.draw(cv, x + 4, c.y + 112, WALLPAPERS[i], 12, Weight::Semibold, rgb(255, 255, 255));
                    }
                }
                f.draw(
                    cv,
                    x0,
                    r.y + 482,
                    "Changes apply immediately. They last until restart: Meridian cannot save settings until its storage is writable.",
                    12,
                    Weight::Regular,
                    t.text_dim(),
                );
            }
            1 => {
                let info = meridian_rt::system_info().ok();
                let (w, h) = crate::screen_size();
                let rows = [
                    ("Display", format!("{w} x {h}, 32-bit colour (UEFI framebuffer)")),
                    ("Processors", info.map_or("?".into(), |i| format!("{} logical processors", i.processor_count))),
                    (
                        "Memory",
                        info.map_or("?".into(), |i| {
                            format!("{} MiB ({} MiB free)", i.physical_memory >> 20, i.available_memory >> 20)
                        }),
                    ),
                    ("Storage", "SATA (AHCI), FAT32, read-only".to_string()),
                    ("Input", "PS/2 keyboard and mouse".to_string()),
                ];
                for (i, (k, v)) in rows.iter().enumerate() {
                    let c = card(cv, r.y + 70 + i as i32 * 60, 52);
                    f.draw(cv, c.x + 16, c.y + 8, k, 14, Weight::Semibold, t.text());
                    f.draw(cv, c.x + 16, c.y + 28, v, 12, Weight::Regular, t.text_dim());
                }
            }
            _ => {
                let c = card(cv, r.y + 70, 220);
                ui::icon(cv, Icon::Start, c.x + 20, c.y + 20, 56, &t);
                f.draw(cv, c.x + 96, c.y + 22, "Meridian OS", 20, Weight::Semibold, t.text());
                let version =
                    meridian_rt::system_info().map(|i| sys::name_of(&i.kernel_version).to_string()).unwrap_or_default();
                let lines = [
                    format!("Version {version} (pre-alpha)"),
                    "An original operating system with its own kernel.".to_string(),
                    "Licensed under the Apache License 2.0. No ads. No telemetry.".to_string(),
                    "Fonts: Selawik (SIL OFL 1.1, Microsoft), Spleen (BSD 2-Clause).".to_string(),
                    "Source: github.com/Brecken321/meridian-os".to_string(),
                ];
                for (i, l) in lines.iter().enumerate() {
                    f.draw(cv, c.x + 96, c.y + 56 + i as i32 * 26, l, 14, Weight::Regular, t.text_dim());
                }
            }
        }
    }

    fn click(&mut self, x: i32, y: i32, _double: bool, cx: &mut Ctx) -> Vec<Action> {
        let mut t = cx.theme;
        if x < 220 {
            let i = (y - 16) / 40;
            if (0..PAGES.len() as i32).contains(&i) && y >= 16 {
                self.page = i as usize;
            }
            return Vec::new();
        }
        if self.page != 0 {
            return Vec::new();
        }
        let card_x = 240;
        let card_right = self.w - 20;
        let in_btn = |cy: i32, i: i32| {
            let b = Rect::new(card_right - 200 + i * 92, cy + 16, 84, 32);
            b.contains(x, y)
        };
        if in_btn(70, 0) {
            t.dark = false;
        } else if in_btn(70, 1) {
            t.dark = true;
        } else if in_btn(254, 0) {
            t.taskbar_left = false;
        } else if in_btn(254, 1) {
            t.taskbar_left = true;
        } else if (190..230).contains(&y) {
            let i = (x - card_x - 16) / 52;
            if (0..ACCENTS.len() as i32).contains(&i) && (x - card_x - 16) % 52 < 40 {
                t.accent = ACCENTS[i as usize].1;
            }
        } else if (370..460).contains(&y) {
            let i = (x - card_x - 16) / 160;
            if (0..WALLPAPERS.len() as i32).contains(&i) && (x - card_x - 16) % 160 < 144 {
                t.wallpaper = i as usize;
            }
        }
        if t != cx.theme { vec![Action::SetTheme(t)] } else { Vec::new() }
    }
}

// ---------------------------------------------------------------------------
// About
// ---------------------------------------------------------------------------

pub struct About {
    w: i32,
    h: i32,
}

impl App for About {
    fn kind(&self) -> AppKind {
        AppKind::About
    }
    fn title(&self) -> String {
        "About Meridian".into()
    }
    fn size(&self) -> (i32, i32) {
        (500, 330)
    }

    fn draw(&mut self, cv: &mut Canvas, r: Rect, cx: &mut Ctx) {
        let t = cx.theme;
        let f = &mut *cx.fonts;
        self.w = r.w;
        self.h = r.h;
        cv.fill(r, t.window());
        ui::icon(cv, Icon::Start, r.x + 28, r.y + 28, 64, &t);
        f.draw(cv, r.x + 112, r.y + 30, "Meridian OS", 28, Weight::Semibold, t.text());
        let info = meridian_rt::system_info().ok();
        let version = info.map(|i| sys::name_of(&i.kernel_version).to_string()).unwrap_or_default();
        f.draw(
            cv,
            r.x + 112,
            r.y + 72,
            &format!("Version {version} - pre-alpha developer build"),
            14,
            Weight::Regular,
            t.text_dim(),
        );
        let lines = [
            "Original Meridian kernel (Rust), running natively.".to_string(),
            info.map_or(String::new(), |i| {
                format!("{} processors, {} MiB of memory.", i.processor_count, i.physical_memory >> 20)
            }),
            "Windows apps, networking and GPU acceleration are not".to_string(),
            "implemented yet - see docs/STATUS.md.".to_string(),
        ];
        for (i, l) in lines.iter().enumerate() {
            f.draw(cv, r.x + 28, r.y + 120 + i as i32 * 24, l, 14, Weight::Regular, t.text());
        }
        ui::button(cv, f, &t, Rect::new(r.right() - 116, r.bottom() - 52, 96, 32), "OK", true, false);
    }

    fn click(&mut self, x: i32, y: i32, _double: bool, _cx: &mut Ctx) -> Vec<Action> {
        if Rect::new(self.w - 116, self.h - 52, 96, 32).contains(x, y) { vec![Action::Close] } else { Vec::new() }
    }
}
