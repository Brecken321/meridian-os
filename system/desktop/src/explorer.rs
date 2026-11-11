//! File Explorer: browse drives and folders, open files with their app,
//! and on writable drives create, rename, copy, move and delete.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use meridian_rt::Handle;
use meridian_rt::sys::{self, DirEntry, InputEvent, VolumeInfo, input, key, volume_flags};

use crate::apps::{
    Action, App, AppKind, Ctx, MenuItem, Opened, ctrl, error_text, file_icon, file_kind, file_name, human_size, join,
    open_path, parent, unique_name,
};
use crate::font::{Fonts, Weight};
use crate::gfx::{Canvas, Rect};
use crate::ui::{self, Icon, Theme};
use crate::widgets::{FieldEvent, TextField};

const TOOLBAR_H: i32 = 44;
const COMMAND_H: i32 = 40;
const TOP: i32 = TOOLBAR_H + COMMAND_H;
const NAV_W: i32 = 184;
const ROW_H: i32 = 28;
const STATUS_H: i32 = 26;
const CONFIRM_H: i32 = 52;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Cmd {
    NewFolder,
    NewFile,
    Cut,
    Copy,
    Paste,
    Rename,
    Delete,
}

const COMMANDS: [(&str, i32, Cmd); 7] = [
    ("New folder", 96, Cmd::NewFolder),
    ("New file", 82, Cmd::NewFile),
    ("Cut", 52, Cmd::Cut),
    ("Copy", 60, Cmd::Copy),
    ("Paste", 62, Cmd::Paste),
    ("Rename", 74, Cmd::Rename),
    ("Delete", 68, Cmd::Delete),
];

// Context-menu ids.
const M_OPEN: u32 = 1;
const M_CUT: u32 = 2;
const M_COPY: u32 = 3;
const M_RENAME: u32 = 4;
const M_DELETE: u32 = 5;
const M_NEW_FOLDER: u32 = 10;
const M_NEW_FILE: u32 = 11;
const M_PASTE: u32 = 12;
const M_REFRESH: u32 = 13;
const M_TERMINAL: u32 = 14;

pub struct Explorer {
    path: String,
    entries: Vec<DirEntry>,
    volumes: Vec<VolumeInfo>,
    selected: Option<usize>,
    scroll: usize,
    back: Vec<String>,
    forward: Vec<String>,
    status: String,
    error: Option<String>,
    children: Vec<(Handle, String)>,
    w: i32,
    h: i32,
    rename: Option<(usize, TextField)>,
    confirm: Option<(String, bool)>,
}

/// Places the quick-access sidebar lists: (label, path, icon).
fn quick_access(volumes: &[VolumeInfo]) -> Vec<(String, String, Icon)> {
    let mut out = Vec::new();
    if let Some(d) = volumes.iter().find(|v| v.flags & volume_flags::WRITABLE != 0) {
        let l = char::from_u32(d.letter).unwrap_or('D');
        for (name, icon) in [("Desktop", Icon::Desktop), ("Documents", Icon::Documents), ("Pictures", Icon::Pictures)] {
            out.push((name.to_string(), format!("{l}:\\{name}"), icon));
        }
    }
    out.push(("This PC".to_string(), String::new(), Icon::ThisPc));
    for v in volumes {
        let l = char::from_u32(v.letter).unwrap_or('?');
        out.push((drive_name(v), format!("{l}:\\"), Icon::Drive));
    }
    out
}

fn drive_name(v: &VolumeInfo) -> String {
    let l = char::from_u32(v.letter).unwrap_or('?');
    if v.flags & volume_flags::WRITABLE != 0 { format!("Data ({l}:)") } else { format!("Boot disk ({l}:)") }
}

impl Explorer {
    pub fn new(path: String) -> Self {
        let mut e = Self {
            path: String::new(),
            entries: Vec::new(),
            volumes: sys::query_volumes(),
            selected: None,
            scroll: 0,
            back: Vec::new(),
            forward: Vec::new(),
            status: String::new(),
            error: None,
            children: Vec::new(),
            w: 900,
            h: 560,
            rename: None,
            confirm: None,
        };
        e.load(path);
        e
    }

    fn writable(&self) -> bool {
        let letter = self.path.chars().next().map(|c| c.to_ascii_uppercase() as u32);
        self.volumes.iter().any(|v| Some(v.letter) == letter && v.flags & volume_flags::WRITABLE != 0)
    }

    fn load(&mut self, path: String) {
        self.rename = None;
        self.confirm = None;
        self.selected = None;
        self.scroll = 0;
        self.error = None;
        self.path = path;
        self.reload();
    }

    /// Re-reads the current folder, keeping the selection by name.
    fn reload(&mut self) {
        let keep = self.selected.and_then(|i| self.entries.get(i)).map(|e| e.name.clone());
        self.volumes = sys::query_volumes();
        if self.path.is_empty() {
            self.entries = Vec::new();
            self.status = format!("{} drive(s)", self.volumes.len());
            return;
        }
        match sys::list_directory(&self.path) {
            Ok(mut list) => {
                list.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
                self.status =
                    format!("{} items{}", list.len(), if self.writable() { "" } else { "  -  read-only drive" });
                self.entries = list;
                self.selected = keep.and_then(|k| self.entries.iter().position(|e| e.name == k));
            }
            Err(e) => {
                self.entries = Vec::new();
                self.error = Some(format!("Cannot open {}: {}", self.path, error_text(e)));
                self.status.clear();
            }
        }
    }

    fn navigate(&mut self, path: String) {
        if self.path != path {
            self.back.push(self.path.clone());
            self.forward.clear();
        }
        self.load(path);
    }

    fn rows_visible(&self) -> usize {
        ((self.h - TOP - ROW_H - STATUS_H) / ROW_H).max(1) as usize
    }

    fn keep_visible(&mut self) {
        if let Some(s) = self.selected {
            let vis = self.rows_visible();
            if s < self.scroll {
                self.scroll = s;
            } else if s >= self.scroll + vis {
                self.scroll = s + 1 - vis;
            }
        }
    }

    fn select_name(&mut self, name: &str) {
        self.selected = self.entries.iter().position(|e| e.name.eq_ignore_ascii_case(name));
        self.keep_visible();
    }

    fn open_entry(&mut self, i: usize) -> Vec<Action> {
        let Some(e) = self.entries.get(i).cloned() else { return Vec::new() };
        let path = join(&self.path, &e.name);
        if e.is_dir {
            self.navigate(path);
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

    fn start_rename(&mut self, i: usize) {
        let Some(e) = self.entries.get(i) else { return };
        let mut field = TextField::new(&e.name);
        let stem = if e.is_dir { e.name.chars().count() } else { e.name.rfind('.').map_or(e.name.len(), |d| d) };
        field.select(0, e.name[..stem].chars().count());
        self.rename = Some((i, field));
        self.selected = Some(i);
        self.keep_visible();
    }

    fn finish_rename(&mut self) -> Vec<Action> {
        let Some((i, field)) = self.rename.take() else { return Vec::new() };
        let Some(old) = self.entries.get(i).map(|e| e.name.clone()) else { return Vec::new() };
        let new = field.text.trim().to_string();
        if new.is_empty() || new == old {
            return Vec::new();
        }
        match sys::rename(&join(&self.path, &old), &join(&self.path, &new)) {
            Ok(()) => {
                self.reload();
                self.select_name(&new);
                vec![Action::FilesChanged]
            }
            Err(e) => {
                self.status = format!("Cannot rename {old}: {}", error_text(e));
                Vec::new()
            }
        }
    }

    fn command(&mut self, cmd: Cmd, cx: &mut Ctx) -> Vec<Action> {
        let sel = self.selected.and_then(|i| self.entries.get(i).cloned());
        match cmd {
            Cmd::NewFolder | Cmd::NewFile => {
                if !self.writable() {
                    self.status = "This drive is read-only. Use drive D: to create files.".into();
                    return Vec::new();
                }
                let (name, result) = if cmd == Cmd::NewFolder {
                    let n = unique_name(&self.path, "New folder", "");
                    let r = sys::create_directory(&join(&self.path, &n));
                    (n, r)
                } else {
                    let n = unique_name(&self.path, "New Text Document", "txt");
                    let r = sys::write_file(&join(&self.path, &n), b"");
                    (n, r)
                };
                match result {
                    Ok(()) => {
                        self.reload();
                        self.select_name(&name);
                        if let Some(i) = self.selected {
                            self.start_rename(i);
                        }
                        return vec![Action::FilesChanged];
                    }
                    Err(e) => self.status = format!("Cannot create {name}: {}", error_text(e)),
                }
            }
            Cmd::Cut | Cmd::Copy => {
                if let Some(e) = sel {
                    cx.clipboard.files = vec![join(&self.path, &e.name)];
                    cx.clipboard.cut = cmd == Cmd::Cut;
                    self.status = format!("{} {}", if cmd == Cmd::Cut { "Cut" } else { "Copied" }, e.name);
                }
            }
            Cmd::Paste => return self.paste(cx),
            Cmd::Rename => {
                if let Some(i) = self.selected.filter(|_| self.writable()) {
                    self.start_rename(i);
                }
            }
            Cmd::Delete => {
                if let Some(e) = sel.filter(|_| self.writable()) {
                    self.confirm = Some((e.name.clone(), e.is_dir));
                }
            }
        }
        Vec::new()
    }

    fn paste(&mut self, cx: &mut Ctx) -> Vec<Action> {
        if self.path.is_empty() || cx.clipboard.files.is_empty() {
            return Vec::new();
        }
        if !self.writable() {
            self.status = "This drive is read-only. Paste into a folder on drive D:.".into();
            return Vec::new();
        }
        let mut done = 0;
        for src in cx.clipboard.files.clone() {
            let name = file_name(&src).to_string();
            let same_folder = parent(&src).eq_ignore_ascii_case(&self.path);
            if cx.clipboard.cut && same_folder {
                continue;
            }
            let is_dir = sys::list_directory(&src).is_ok();
            let target = if sys::file_size(&join(&self.path, &name)).is_ok()
                || sys::list_directory(&join(&self.path, &name)).is_ok()
            {
                let (stem, ext) = match name.rfind('.') {
                    Some(d) if !is_dir && d > 0 => (&name[..d], &name[d + 1..]),
                    _ => (name.as_str(), ""),
                };
                unique_name(&self.path, &format!("{stem} - Copy"), ext)
            } else {
                name.clone()
            };
            let dest = join(&self.path, &target);
            if is_dir && (dest.to_lowercase() + "\\").starts_with(&(src.to_lowercase() + "\\")) {
                self.status = "A folder cannot be pasted inside itself.".into();
                continue;
            }
            let same_drive = src.as_bytes().first().map(u8::to_ascii_uppercase)
                == self.path.as_bytes().first().map(u8::to_ascii_uppercase);
            let result = if cx.clipboard.cut && same_drive {
                sys::rename(&src, &dest)
            } else {
                copy_tree(&src, &dest, is_dir, 0)
                    .and_then(|_| if cx.clipboard.cut { delete_tree(&src, is_dir, 0) } else { Ok(()) })
            };
            match result {
                Ok(()) => {
                    done += 1;
                    self.reload();
                    self.select_name(&target);
                }
                Err(e) => self.status = format!("Cannot paste {name}: {}", error_text(e)),
            }
        }
        if cx.clipboard.cut && done > 0 {
            cx.clipboard.files.clear();
        }
        if done > 0 {
            self.status = format!("Pasted {done} item(s)");
            vec![Action::FilesChanged]
        } else {
            Vec::new()
        }
    }

    fn confirm_delete(&mut self) -> Vec<Action> {
        let Some((name, is_dir)) = self.confirm.take() else { return Vec::new() };
        match delete_tree(&join(&self.path, &name), is_dir, 0) {
            Ok(()) => {
                self.reload();
                self.status = format!("Deleted {name}");
                vec![Action::FilesChanged]
            }
            Err(e) => {
                self.status = format!("Cannot delete {name}: {}", error_text(e));
                self.reload();
                Vec::new()
            }
        }
    }

    fn command_rects(&self) -> Vec<(Rect, Cmd)> {
        let mut x = 8;
        COMMANDS
            .iter()
            .map(|&(_, w, c)| {
                let r = Rect::new(x, TOOLBAR_H + 4, w, 32);
                x += w + 4 + if matches!(c, Cmd::NewFile | Cmd::Paste) { 10 } else { 0 };
                (r, c)
            })
            .collect()
    }

    fn command_enabled(&self, c: Cmd, has_clip: bool) -> bool {
        let w = self.writable() && !self.path.is_empty();
        match c {
            Cmd::NewFolder | Cmd::NewFile => w,
            Cmd::Cut | Cmd::Rename | Cmd::Delete => w && self.selected.is_some(),
            Cmd::Copy => self.selected.is_some(),
            Cmd::Paste => w && has_clip,
        }
    }

    fn draw_toolbar(&self, cv: &mut Canvas, f: &mut Fonts, t: &Theme, r: Rect, has_clip: bool) {
        for i in 0..4 {
            let b = Rect::new(r.x + 8 + i * 36, r.y + 8, 30, 28);
            let enabled = match i {
                0 => !self.back.is_empty(),
                1 => !self.forward.is_empty(),
                2 => !self.path.is_empty(),
                _ => true,
            };
            cv.fill_rounded(b, 4, t.hover());
            let (cx_, cy) = (b.x as f32 + 15.0, b.y as f32 + 14.0);
            let c = if enabled { t.text() } else { t.text_dim() };
            match i {
                0 | 1 => {
                    let d = if i == 0 { -1.0 } else { 1.0 };
                    cv.line(cx_ - 6.0, cy, cx_ + 6.0, cy, 1.4, c);
                    cv.line(cx_ + 6.0 * d, cy, cx_ + 1.0 * d, cy - 5.0, 1.4, c);
                    cv.line(cx_ + 6.0 * d, cy, cx_ + 1.0 * d, cy + 5.0, 1.4, c);
                }
                2 => {
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
        // Address bar: "This PC > D: > Documents".
        let addr = Rect::new(r.x + 156, r.y + 8, r.w - 168, 28);
        cv.rounded_box(addr, 4, t.border(), t.surface());
        let crumbs = if self.path.is_empty() {
            "This PC".to_string()
        } else {
            let mut s = "This PC".to_string();
            for part in self.path.split('\\').filter(|p| !p.is_empty()) {
                s.push_str("  >  ");
                s.push_str(part);
            }
            s
        };
        f.draw_fit(cv, addr.x + 10, addr.y + 5, &crumbs, addr.w - 20, 14, Weight::Regular, t.text());
        // Command bar.
        cv.fill(Rect::new(r.x, r.y + TOOLBAR_H + COMMAND_H - 1, r.w, 1), t.border());
        for (i, (b, c)) in self.command_rects().into_iter().enumerate() {
            let enabled = self.command_enabled(c, has_clip);
            let label = COMMANDS[i].0;
            let b = b.offset(r.x, r.y);
            let color = if enabled { t.text() } else { t.text_dim() };
            let tw = f.measure(label, 12, Weight::Regular);
            f.draw(cv, b.x + (b.w - tw) / 2, b.y + 8, label, 12, Weight::Regular, color);
        }
    }

    fn row_at(&self, y: i32) -> Option<usize> {
        let row = (y - TOP - ROW_H).div_euclid(ROW_H);
        if y < TOP + ROW_H || y >= self.h - STATUS_H {
            return None;
        }
        let i = self.scroll + row as usize;
        (i < self.entries.len()).then_some(i)
    }

    fn drive_tile(&self, i: usize, main: Rect) -> Rect {
        Rect::new(main.x + 20 + (i % 2) as i32 * 290, main.y + 44 + (i / 2) as i32 * 92, 276, 80)
    }
}

/// Copies a file or a whole folder tree.
fn copy_tree(src: &str, dst: &str, is_dir: bool, depth: u32) -> Result<(), i64> {
    if depth > 16 {
        return Err(meridian_rt::status::INVALID_PARAMETER);
    }
    if !is_dir {
        let data = sys::read_file(src)?;
        return sys::write_file(dst, &data);
    }
    sys::create_directory(dst)?;
    for e in sys::list_directory(src)? {
        copy_tree(&join(src, &e.name), &join(dst, &e.name), e.is_dir, depth + 1)?;
    }
    Ok(())
}

/// Deletes a file or a whole folder tree.
fn delete_tree(path: &str, is_dir: bool, depth: u32) -> Result<(), i64> {
    if depth > 16 {
        return Err(meridian_rt::status::INVALID_PARAMETER);
    }
    if is_dir {
        for e in sys::list_directory(path)? {
            delete_tree(&join(path, &e.name), e.is_dir, depth + 1)?;
        }
    }
    sys::delete_file(path)
}

impl App for Explorer {
    fn kind(&self) -> AppKind {
        AppKind::Explorer
    }
    fn title(&self) -> String {
        if self.path.is_empty() {
            "This PC - File Explorer".into()
        } else {
            let name = self.path.trim_end_matches('\\').rsplit('\\').next().unwrap_or(&self.path);
            format!("{name} - File Explorer")
        }
    }
    fn size(&self) -> (i32, i32) {
        (900, 560)
    }
    fn min_size(&self) -> (i32, i32) {
        (640, 320)
    }
    fn wheel(&mut self, notches: i32) -> bool {
        let max = self.entries.len().saturating_sub(self.rows_visible());
        let old = self.scroll;
        self.scroll = (self.scroll as i32 + notches * 3).clamp(0, max as i32) as usize;
        self.scroll != old
    }
    fn files_changed(&mut self) -> bool {
        if self.rename.is_none() {
            self.reload();
        }
        true
    }

    fn draw(&mut self, cv: &mut Canvas, r: Rect, cx: &mut Ctx) {
        let t = cx.theme;
        let has_clip = !cx.clipboard.files.is_empty();
        let f = &mut *cx.fonts;
        self.w = r.w;
        self.h = r.h;
        cv.fill(r, t.window());
        self.draw_toolbar(cv, f, &t, r, has_clip);
        // Navigation pane.
        let nav = Rect::new(r.x, r.y + TOP, NAV_W, r.h - TOP - STATUS_H);
        cv.fill(nav, t.surface());
        let mut ny = nav.y + 8;
        for (label, path, icon) in quick_access(&self.volumes) {
            if path.is_empty() {
                cv.fill(Rect::new(nav.x + 12, ny + 2, NAV_W - 24, 1), t.border());
                ny += 8;
            }
            if path.eq_ignore_ascii_case(&self.path) {
                cv.fill_rounded(Rect::new(nav.x + 6, ny, NAV_W - 12, 28), 4, t.selection());
            }
            ui::icon(cv, icon, nav.x + 14, ny + 6, 16, &t);
            f.draw_fit(cv, nav.x + 38, ny + 5, &label, NAV_W - 44, 14, Weight::Regular, t.text());
            ny += 32;
        }
        // Main area.
        let main = Rect::new(r.x + NAV_W, r.y + TOP, r.w - NAV_W, r.h - TOP - STATUS_H);
        cv.fill(main, t.window());
        let old_clip = cv.clip();
        cv.set_clip(main);
        if let Some(err) = &self.error {
            f.draw(cv, main.x + 20, main.y + 20, err, 14, Weight::Regular, t.danger());
        } else if self.path.is_empty() {
            f.draw(cv, main.x + 20, main.y + 14, "Devices and drives", 14, Weight::Semibold, t.text());
            for (i, v) in self.volumes.iter().enumerate() {
                let tile = self.drive_tile(i, main);
                cv.rounded_box(tile, 6, t.border(), t.surface());
                ui::icon(cv, Icon::Drive, tile.x + 12, tile.y + 16, 44, &t);
                f.draw(cv, tile.x + 68, tile.y + 10, &drive_name(v), 14, Weight::Semibold, t.text());
                let bar = Rect::new(tile.x + 68, tile.y + 36, tile.w - 84, 8);
                cv.fill_rounded(bar, 4, t.hover());
                let used = v.total_bytes.saturating_sub(v.free_bytes);
                let fill = (bar.w as u64 * used / v.total_bytes.max(1)) as i32;
                cv.fill_rounded(Rect::new(bar.x, bar.y, fill.max(8), bar.h), 4, t.accent);
                let line = format!("{} free of {}", human_size(v.free_bytes), human_size(v.total_bytes));
                f.draw(cv, tile.x + 68, tile.y + 52, &line, 12, Weight::Regular, t.text_dim());
            }
        } else {
            let header = Rect::new(main.x, main.y, main.w, ROW_H);
            let (cname, ctype, csize) = (main.x + 44, main.x + main.w - 290, main.x + main.w - 120);
            f.draw(cv, cname, header.y + 6, "Name", 12, Weight::Regular, t.text_dim());
            f.draw(cv, ctype, header.y + 6, "Type", 12, Weight::Regular, t.text_dim());
            f.draw(cv, csize, header.y + 6, "Size", 12, Weight::Regular, t.text_dim());
            cv.fill(Rect::new(main.x, header.bottom() - 1, main.w, 1), t.border());
            if self.entries.is_empty() {
                f.draw(cv, cname, main.y + ROW_H + 16, "This folder is empty.", 14, Weight::Regular, t.text_dim());
            }
            let rows = self.rows_visible();
            for (row, (i, e)) in self.entries.iter().enumerate().skip(self.scroll).take(rows).enumerate() {
                let y = main.y + ROW_H + row as i32 * ROW_H;
                let cut = cx.clipboard.cut
                    && cx.clipboard.files.iter().any(|p| p.eq_ignore_ascii_case(&join(&self.path, &e.name)));
                if self.selected == Some(i) {
                    cv.fill_rounded(Rect::new(main.x + 4, y + 1, main.w - 8, ROW_H - 2), 4, t.selection());
                }
                ui::icon(cv, file_icon(&e.name, e.is_dir), main.x + 16, y + 5, 18, &t);
                let name_color = if cut { t.text_dim() } else { t.text() };
                match &mut self.rename {
                    Some((ri, field)) if *ri == i => {
                        field.draw(cv, f, &t, Rect::new(cname - 6, y + 2, ctype - cname - 4, ROW_H - 4), true);
                    }
                    _ => f.draw_fit(cv, cname, y + 5, &e.name, ctype - cname - 12, 14, Weight::Regular, name_color),
                }
                f.draw(cv, ctype, y + 5, &file_kind(&e.name, e.is_dir), 14, Weight::Regular, t.text_dim());
                if !e.is_dir {
                    f.draw(cv, csize, y + 5, &human_size(e.size), 14, Weight::Regular, t.text_dim());
                }
            }
            // Scroll bar.
            if self.entries.len() > rows {
                let track = Rect::new(main.right() - 6, main.y + ROW_H + 2, 4, main.h - ROW_H - 4);
                let thumb_h = (track.h * rows as i32 / self.entries.len() as i32).max(20);
                let thumb_y =
                    track.y + (track.h - thumb_h) * self.scroll as i32 / (self.entries.len() - rows).max(1) as i32;
                cv.fill_rounded(Rect::new(track.x, thumb_y, 4, thumb_h), 2, t.text_dim());
            }
        }
        cv.set_clip(old_clip);
        // Delete confirmation bar, or the status bar.
        if let Some((name, is_dir)) = &self.confirm {
            let bar = Rect::new(r.x, r.bottom() - CONFIRM_H, r.w, CONFIRM_H);
            cv.fill(bar, t.surface());
            cv.fill(Rect::new(bar.x, bar.y, bar.w, 1), t.danger());
            let what = if *is_dir { "the folder" } else { "" };
            let msg = format!("Permanently delete {what} \"{name}\"? This cannot be undone.");
            f.draw_fit(cv, bar.x + 16, bar.y + 17, &msg, bar.w - 220, 14, Weight::Regular, t.text());
            ui::button(cv, f, &t, Rect::new(bar.right() - 196, bar.y + 10, 92, 32), "Delete", true, false);
            ui::button(cv, f, &t, Rect::new(bar.right() - 96, bar.y + 10, 84, 32), "Cancel", false, false);
        } else {
            let sb = Rect::new(r.x, r.bottom() - STATUS_H, r.w, STATUS_H);
            cv.fill(sb, t.surface());
            cv.fill(Rect::new(sb.x, sb.y, sb.w, 1), t.border());
            let mut text = self.status.clone();
            if let Some(e) = self.selected.and_then(|i| self.entries.get(i)) {
                if !e.is_dir {
                    text = format!("{text}     {} selected, {}", e.name, human_size(e.size));
                }
            }
            f.draw_fit(cv, sb.x + 12, sb.y + 5, &text, sb.w - 24, 12, Weight::Regular, t.text_dim());
        }
    }

    fn click(&mut self, x: i32, y: i32, double: bool, cx: &mut Ctx) -> Vec<Action> {
        if self.confirm.is_some() && y >= self.h - CONFIRM_H {
            if Rect::new(self.w - 196, self.h - CONFIRM_H + 10, 92, 32).contains(x, y) {
                return self.confirm_delete();
            }
            if Rect::new(self.w - 96, self.h - CONFIRM_H + 10, 84, 32).contains(x, y) {
                self.confirm = None;
            }
            return Vec::new();
        }
        self.confirm = None;
        // Clicking anywhere else finishes a rename (as on Windows).
        if let Some((ri, _)) = &self.rename {
            let row_y = TOP + ROW_H + (*ri as i32 - self.scroll as i32) * ROW_H;
            if !(row_y..row_y + ROW_H).contains(&y) || x < NAV_W {
                let mut out = self.finish_rename();
                out.extend(self.click(x, y, false, cx));
                return out;
            }
            if let Some((_, field)) = &mut self.rename {
                let ctype = self.w - 290;
                field.click(cx.fonts, Rect::new(NAV_W + 38, row_y + 2, ctype - NAV_W - 42, ROW_H - 4), x);
            }
            return Vec::new();
        }
        if y < TOOLBAR_H {
            match (x - 8).div_euclid(36) {
                0 => {
                    if let Some(prev) = self.back.pop() {
                        self.forward.push(self.path.clone());
                        self.load(prev);
                    }
                }
                1 => {
                    if let Some(next) = self.forward.pop() {
                        self.back.push(self.path.clone());
                        self.load(next);
                    }
                }
                2 => {
                    if !self.path.is_empty() {
                        let up = parent(&self.path);
                        self.navigate(up);
                    }
                }
                3 => self.reload(),
                _ => {}
            }
            return Vec::new();
        }
        if y < TOP {
            let has_clip = !cx.clipboard.files.is_empty();
            if let Some((_, c)) = self.command_rects().into_iter().find(|(b, _)| b.contains(x, y)) {
                if self.command_enabled(c, has_clip) {
                    return self.command(c, cx);
                }
            }
            return Vec::new();
        }
        if x < NAV_W {
            let mut ny = TOP + 8;
            for (_, path, _) in quick_access(&self.volumes) {
                if path.is_empty() {
                    ny += 8;
                }
                if (ny..ny + 32).contains(&y) {
                    self.navigate(path);
                    break;
                }
                ny += 32;
            }
            return Vec::new();
        }
        if self.path.is_empty() {
            let main = Rect::new(NAV_W, TOP, self.w - NAV_W, self.h - TOP - STATUS_H);
            for i in 0..self.volumes.len() {
                if self.drive_tile(i, main).contains(x, y) && double {
                    let p = format!("{}:\\", char::from_u32(self.volumes[i].letter).unwrap_or('C'));
                    self.navigate(p);
                }
            }
            return Vec::new();
        }
        match self.row_at(y) {
            Some(i) => {
                self.selected = Some(i);
                if double {
                    return self.open_entry(i);
                }
            }
            None => self.selected = None,
        }
        Vec::new()
    }

    fn right_click(&mut self, x: i32, y: i32, cx: &mut Ctx) -> Vec<Action> {
        if self.path.is_empty() || x < NAV_W || y < TOP {
            return Vec::new();
        }
        if self.rename.is_some() {
            self.finish_rename();
        }
        let w = self.writable();
        let items = match self.row_at(y) {
            Some(i) => {
                self.selected = Some(i);
                vec![
                    MenuItem::new("Open", M_OPEN),
                    MenuItem::separator(),
                    MenuItem::when("Cut", M_CUT, w),
                    MenuItem::new("Copy", M_COPY),
                    MenuItem::separator(),
                    MenuItem::when("Rename", M_RENAME, w),
                    MenuItem::when("Delete", M_DELETE, w),
                ]
            }
            None => {
                self.selected = None;
                vec![
                    MenuItem::when("New folder", M_NEW_FOLDER, w),
                    MenuItem::when("New text document", M_NEW_FILE, w),
                    MenuItem::separator(),
                    MenuItem::when("Paste", M_PASTE, w && !cx.clipboard.files.is_empty()),
                    MenuItem::new("Refresh", M_REFRESH),
                    MenuItem::separator(),
                    MenuItem::new("Open in Terminal", M_TERMINAL),
                ]
            }
        };
        vec![Action::Menu(items, x, y)]
    }

    fn menu(&mut self, id: u32, cx: &mut Ctx) -> Vec<Action> {
        match id {
            M_OPEN => self.selected.map_or(Vec::new(), |i| self.open_entry(i)),
            M_CUT => self.command(Cmd::Cut, cx),
            M_COPY => self.command(Cmd::Copy, cx),
            M_RENAME => self.command(Cmd::Rename, cx),
            M_DELETE => self.command(Cmd::Delete, cx),
            M_NEW_FOLDER => self.command(Cmd::NewFolder, cx),
            M_NEW_FILE => self.command(Cmd::NewFile, cx),
            M_PASTE => self.command(Cmd::Paste, cx),
            M_REFRESH => {
                self.reload();
                Vec::new()
            }
            M_TERMINAL => vec![Action::Open(AppKind::Terminal, Some(self.path.clone()))],
            _ => Vec::new(),
        }
    }

    fn key(&mut self, e: &InputEvent, cx: &mut Ctx) -> Vec<Action> {
        if let Some((_, field)) = &mut self.rename {
            match field.key(e, cx.clipboard) {
                FieldEvent::Submit => return self.finish_rename(),
                FieldEvent::Cancel => self.rename = None,
                _ => {}
            }
            return Vec::new();
        }
        if self.confirm.is_some() {
            match e.key {
                key::ENTER => return self.confirm_delete(),
                key::ESCAPE => self.confirm = None,
                _ => {}
            }
            return Vec::new();
        }
        let has_clip = !cx.clipboard.files.is_empty();
        for (letter, c) in [('c', Cmd::Copy), ('x', Cmd::Cut), ('v', Cmd::Paste)] {
            if ctrl(e, letter) {
                return if self.command_enabled(c, has_clip) { self.command(c, cx) } else { Vec::new() };
            }
        }
        if e.flags & input::CTRL != 0 && e.flags & input::SHIFT != 0 && e.key == key::CHAR && e.ch == 'N' as u32 {
            return self.command(Cmd::NewFolder, cx);
        }
        let n = self.entries.len();
        match e.key {
            key::DOWN if n > 0 => self.selected = Some(self.selected.map_or(0, |s| (s + 1).min(n - 1))),
            key::UP if n > 0 => self.selected = Some(self.selected.map_or(0, |s| s.saturating_sub(1))),
            key::HOME if n > 0 => self.selected = Some(0),
            key::END if n > 0 => self.selected = Some(n - 1),
            key::ENTER => {
                if let Some(i) = self.selected {
                    return self.open_entry(i);
                }
            }
            key::BACKSPACE => {
                if !self.path.is_empty() {
                    let up = parent(&self.path);
                    self.navigate(up);
                }
            }
            key::DELETE if self.command_enabled(Cmd::Delete, has_clip) => return self.command(Cmd::Delete, cx),
            k if k == key::FUNCTION + 2 && self.command_enabled(Cmd::Rename, has_clip) => {
                return self.command(Cmd::Rename, cx);
            }
            k if k == key::FUNCTION + 5 => self.reload(),
            key::CHAR if e.flags & input::CTRL == 0 => {
                // Type-to-select: the next item starting with the letter.
                if let Some(c) = char::from_u32(e.ch) {
                    let start = self.selected.map_or(0, |s| s + 1);
                    let found = (0..n)
                        .map(|k| (start + k) % n.max(1))
                        .find(|&k| self.entries[k].name.chars().next().is_some_and(|f| f.eq_ignore_ascii_case(&c)));
                    if found.is_some() {
                        self.selected = found;
                    }
                }
            }
            _ => {}
        }
        self.keep_visible();
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
