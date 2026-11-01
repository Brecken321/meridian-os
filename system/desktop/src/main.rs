//! The Meridian desktop: a user-mode program that owns the screen, keyboard
//! and mouse and provides the window manager, taskbar, Start menu, desktop
//! icons and the built-in apps.
//!
//! Rendering is damage-based: only the parts of the screen that changed are
//! recomposed into a back buffer and copied to the framebuffer; windows keep
//! their own surfaces, and the pointer is drawn straight onto the
//! framebuffer.

#![no_std]
#![no_main]

extern crate alloc;

mod apps;
mod font;
mod gfx;
mod ui;

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicI32, Ordering};

use apps::{Action, App, AppKind, Ctx};
use font::{Font, Fonts, Mono, Weight};
use gfx::{Canvas, Rect, rgb, shade, with_alpha};
use meridian_rt::sys::{self, DirEntry, DisplayInfo, InputEvent, input, key, power};
use meridian_rt::{self as rt};
use ui::{Icon, Theme};

pub const TITLE_H: i32 = 32;
const TASKBAR_H: i32 = 48;
const CAPTION_W: i32 = 46;
const RADIUS: i32 = 8;
const START_W: i32 = 600;
const START_H: i32 = 560;

static REGULAR: &[u8] = include_bytes!("../assets/selawik-regular.ttf");
static SEMIBOLD: &[u8] = include_bytes!("../assets/selawik-semibold.ttf");
static MONO_FONT: &[u8; 4096] = include_bytes!("../../../kernel/assets/font8x16.bin");

static SCREEN_W: AtomicI32 = AtomicI32::new(0);
static SCREEN_H: AtomicI32 = AtomicI32::new(0);

pub fn screen_size() -> (i32, i32) {
    (SCREEN_W.load(Ordering::Relaxed), SCREEN_H.load(Ordering::Relaxed))
}

const PINNED: [AppKind; 5] =
    [AppKind::Explorer, AppKind::Terminal, AppKind::TaskManager, AppKind::Settings, AppKind::Notepad];
const START_APPS: [AppKind; 6] =
    [AppKind::Explorer, AppKind::Terminal, AppKind::Notepad, AppKind::TaskManager, AppKind::Settings, AppKind::About];

struct Window {
    id: u32,
    app: Box<dyn App>,
    rect: Rect,
    restore: Rect,
    minimized: bool,
    maximized: bool,
    surface: Canvas,
    dirty: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hover {
    None,
    Taskbar(usize),
    Caption(u32, u8),
    StartApp(usize),
    Recommended(usize),
    Power,
    PowerItem(usize),
    Icon(usize),
}

#[derive(Clone, Copy)]
enum TaskItem {
    Start,
    App(AppKind),
}

struct DesktopIcon {
    icon: Icon,
    label: &'static str,
    kind: AppKind,
    arg: Option<&'static str>,
}

const DESKTOP_ICONS: [DesktopIcon; 3] = [
    DesktopIcon { icon: Icon::ThisPc, label: "This PC", kind: AppKind::Explorer, arg: None },
    DesktopIcon {
        icon: Icon::TextFile,
        label: "Welcome to Meridian",
        kind: AppKind::Notepad,
        arg: Some("C:\\Welcome to Meridian.txt"),
    },
    DesktopIcon { icon: Icon::Terminal, label: "Terminal", kind: AppKind::Terminal, arg: None },
];

struct Desktop {
    fb: *mut u32,
    info: DisplayInfo,
    screen: Rect,
    back: Canvas,
    wallpaper: Canvas,
    fonts: Fonts,
    mono: Mono,
    theme: Theme,
    windows: Vec<Window>,
    focused: Option<u32>,
    next_id: u32,
    start_open: bool,
    search: String,
    recommended: Vec<(String, DirEntry)>,
    power_open: bool,
    notice: Option<String>,
    mouse: (i32, i32),
    buttons: u32,
    drag: Option<(u32, i32, i32)>,
    hover: Hover,
    selected_icon: Option<usize>,
    last_click: (u64, i32, i32),
    clock: String,
    date: String,
    damage: Option<Rect>,
    cursor_drawn: Rect,
}

fn now_ms() -> u64 {
    rt::uptime_us() / 1000
}

impl Desktop {
    fn new(fb: *mut u32, info: DisplayInfo) -> Self {
        let (w, h) = (info.width as i32, info.height as i32);
        let theme = Theme { dark: true, accent: ui::ACCENTS[0].1, wallpaper: 0, taskbar_left: false };
        let regular = Font::parse(REGULAR).expect("bundled font is valid");
        let semibold = Font::parse(SEMIBOLD).expect("bundled font is valid");
        Self {
            fb,
            info,
            screen: Rect::new(0, 0, w, h),
            back: Canvas::new(w, h, rgb(0, 0, 0)),
            wallpaper: ui::wallpaper(w, h, &theme),
            fonts: Fonts::new(regular, semibold),
            mono: Mono::new(MONO_FONT),
            theme,
            windows: Vec::new(),
            focused: None,
            next_id: 1,
            start_open: false,
            search: String::new(),
            recommended: Vec::new(),
            power_open: false,
            notice: None,
            mouse: (w / 2, h / 2),
            buttons: 0,
            drag: None,
            hover: Hover::None,
            selected_icon: None,
            last_click: (0, -100, -100),
            clock: String::new(),
            date: String::new(),
            damage: Some(Rect::new(0, 0, w, h)),
            cursor_drawn: Rect::default(),
        }
    }

    fn work_area(&self) -> Rect {
        Rect::new(0, 0, self.screen.w, self.screen.h - TASKBAR_H)
    }

    fn damage(&mut self, r: Rect) {
        if let Some(r) = r.intersect(&self.screen) {
            self.damage = Some(self.damage.map_or(r, |d| d.union(&r)));
        }
    }

    // ---- windows ----------------------------------------------------------

    fn index_of(&self, id: u32) -> Option<usize> {
        self.windows.iter().position(|w| w.id == id)
    }

    fn launch(&mut self, kind: AppKind, arg: Option<String>) {
        // Single-instance apps are brought forward instead of reopened.
        if matches!(kind, AppKind::TaskManager | AppKind::Settings | AppKind::About) {
            if let Some(id) = self.windows.iter().find(|w| w.app.kind() == kind).map(|w| w.id) {
                self.restore_and_focus(id);
                return;
            }
        }
        let app = apps::create(kind, arg);
        let (cw, ch) = app.size();
        let n = self.windows.len() as i32 % 6;
        let wa = self.work_area();
        let (w, h) = (cw.min(wa.w - 40), (ch + TITLE_H).min(wa.h - 40));
        let x = ((wa.w - w) / 2 - 100 + n * 36).clamp(0, wa.w - w);
        let y = (60 + n * 32).clamp(0, wa.h - h);
        let id = self.next_id;
        self.next_id += 1;
        let rect = Rect::new(x, y, w, h);
        self.windows.push(Window {
            id,
            app,
            rect,
            restore: rect,
            minimized: false,
            maximized: false,
            surface: Canvas::new(1, 1, 0),
            dirty: true,
        });
        self.focus(id);
    }

    fn focus(&mut self, id: u32) {
        let old = self.focused;
        if let Some(i) = self.index_of(id) {
            let w = self.windows.remove(i);
            self.windows.push(w);
        }
        self.focused = Some(id);
        for w in self.windows.iter_mut() {
            if Some(w.id) == old || w.id == id {
                w.dirty = true;
            }
        }
        if let Some(r) = self.index_of(id).map(|i| self.windows[i].rect) {
            self.damage(r.grow(24));
        }
        self.damage_taskbar();
    }

    fn restore_and_focus(&mut self, id: u32) {
        if let Some(i) = self.index_of(id) {
            self.windows[i].minimized = false;
            let r = self.windows[i].rect;
            self.damage(r.grow(24));
        }
        self.focus(id);
    }

    fn close(&mut self, id: u32) {
        if let Some(i) = self.index_of(id) {
            let w = self.windows.remove(i);
            self.damage(w.rect.grow(24));
            if self.focused == Some(id) {
                self.focused = self.windows.iter().rev().find(|w| !w.minimized).map(|w| w.id);
                if let Some(f) = self.focused.and_then(|f| self.index_of(f)) {
                    self.windows[f].dirty = true;
                }
            }
            self.damage_taskbar();
        }
    }

    fn minimize(&mut self, id: u32) {
        if let Some(i) = self.index_of(id) {
            self.windows[i].minimized = true;
            let r = self.windows[i].rect;
            self.damage(r.grow(24));
            if self.focused == Some(id) {
                self.focused = self.windows.iter().rev().find(|w| !w.minimized).map(|w| w.id);
            }
            self.damage_taskbar();
        }
    }

    fn toggle_maximize(&mut self, id: u32) {
        let wa = self.work_area();
        if let Some(i) = self.index_of(id) {
            let w = &mut self.windows[i];
            let old = w.rect;
            if w.maximized {
                w.rect = w.restore;
            } else {
                w.restore = w.rect;
                w.rect = wa;
            }
            w.maximized = !w.maximized;
            w.dirty = true;
            let new = w.rect;
            self.damage(old.grow(24).union(&new.grow(24)));
        }
    }

    fn apply(&mut self, id: u32, actions: Vec<Action>) {
        for a in actions {
            match a {
                Action::Open(kind, arg) => self.launch(kind, arg),
                Action::SetTheme(t) => self.set_theme(t),
                Action::Close => self.close(id),
            }
        }
        if let Some(i) = self.index_of(id) {
            self.windows[i].dirty = true;
            let r = self.windows[i].rect;
            self.damage(r);
        }
        self.damage_taskbar();
    }

    fn set_theme(&mut self, t: Theme) {
        let regenerate =
            t.wallpaper != self.theme.wallpaper || t.dark != self.theme.dark || t.accent != self.theme.accent;
        self.theme = t;
        if regenerate {
            self.wallpaper = ui::wallpaper(self.screen.w, self.screen.h, &t);
        }
        for w in self.windows.iter_mut() {
            w.dirty = true;
        }
        self.damage(self.screen);
    }

    // ---- taskbar and Start menu layout ----------------------------------

    fn taskbar_items(&self) -> Vec<(Rect, TaskItem)> {
        let mut kinds: Vec<AppKind> = PINNED.to_vec();
        for w in &self.windows {
            if !kinds.contains(&w.app.kind()) {
                kinds.push(w.app.kind());
            }
        }
        let n = kinds.len() as i32 + 1;
        let total = n * 44 + (n - 1) * 4;
        let x0 = if self.theme.taskbar_left { 12 } else { (self.screen.w - total) / 2 };
        let y = self.screen.h - TASKBAR_H + 4;
        let mut items = Vec::new();
        items.push((Rect::new(x0, y, 44, 40), TaskItem::Start));
        for (i, k) in kinds.into_iter().enumerate() {
            items.push((Rect::new(x0 + (i as i32 + 1) * 48, y, 44, 40), TaskItem::App(k)));
        }
        items
    }

    fn damage_taskbar(&mut self) {
        let r = Rect::new(0, self.screen.h - TASKBAR_H, self.screen.w, TASKBAR_H);
        self.damage(r);
    }

    fn start_rect(&self) -> Rect {
        let x = if self.theme.taskbar_left { 12 } else { (self.screen.w - START_W) / 2 };
        Rect::new(x, self.screen.h - TASKBAR_H - 12 - START_H, START_W, START_H)
    }

    fn start_apps(&self) -> Vec<AppKind> {
        let q = self.search.to_lowercase();
        START_APPS.iter().copied().filter(|k| q.is_empty() || apps::name_of(*k).to_lowercase().contains(&q)).collect()
    }

    fn start_app_rect(&self, i: usize) -> Rect {
        let s = self.start_rect();
        Rect::new(s.x + 32 + (i as i32 % 6) * 90, s.y + 104 + (i as i32 / 6) * 92, 84, 84)
    }

    fn recommended_rect(&self, i: usize) -> Rect {
        let s = self.start_rect();
        Rect::new(s.x + 32 + (i as i32 % 2) * 272, s.y + 300 + (i as i32 / 2) * 56, 264, 50)
    }

    fn power_rect(&self) -> Rect {
        let s = self.start_rect();
        Rect::new(s.right() - 60, s.bottom() - 56, 40, 40)
    }

    fn power_item_rect(&self, i: usize) -> Rect {
        let p = self.power_rect();
        Rect::new(p.right() - 180, p.y - 96 + i as i32 * 44, 180, 40)
    }

    fn open_start(&mut self) {
        self.start_open = true;
        self.power_open = false;
        self.search.clear();
        self.notice = None;
        // Recommended: real files from the root of C: and C:\MERIDIAN.
        self.recommended.clear();
        for dir in ["C:\\", "C:\\MERIDIAN"] {
            if let Ok(list) = sys::list_directory(dir) {
                for e in list.into_iter().filter(|e| !e.is_dir) {
                    if self.recommended.len() < 4 {
                        self.recommended.push((String::from(dir), e));
                    }
                }
            }
        }
        let r = self.start_rect();
        self.damage(r.grow(30));
    }

    fn close_start(&mut self) {
        if self.start_open {
            self.start_open = false;
            self.power_open = false;
            let r = self.start_rect();
            self.damage(r.grow(30));
        }
    }

    fn icon_rect(&self, i: usize) -> Rect {
        Rect::new(12, 12 + i as i32 * 100, 88, 92)
    }

    // ---- hit testing -----------------------------------------------------

    fn hover_at(&self, x: i32, y: i32) -> Hover {
        if self.start_open {
            if self.power_open {
                for i in 0..2 {
                    if self.power_item_rect(i).contains(x, y) {
                        return Hover::PowerItem(i);
                    }
                }
            }
            if self.start_rect().contains(x, y) {
                for i in 0..self.start_apps().len() {
                    if self.start_app_rect(i).contains(x, y) {
                        return Hover::StartApp(i);
                    }
                }
                for i in 0..self.recommended.len() {
                    if self.recommended_rect(i).contains(x, y) {
                        return Hover::Recommended(i);
                    }
                }
                if self.power_rect().contains(x, y) {
                    return Hover::Power;
                }
                return Hover::None;
            }
        }
        if y >= self.screen.h - TASKBAR_H {
            return self.taskbar_items().iter().position(|(r, _)| r.contains(x, y)).map_or(Hover::None, Hover::Taskbar);
        }
        for w in self.windows.iter().rev().filter(|w| !w.minimized) {
            if w.rect.contains(x, y) {
                if y < w.rect.y + TITLE_H && x >= w.rect.right() - 3 * CAPTION_W {
                    let b = ((w.rect.right() - x - 1) / CAPTION_W) as u8; // 0 close, 1 max, 2 min
                    return Hover::Caption(w.id, b);
                }
                return Hover::None;
            }
        }
        (0..DESKTOP_ICONS.len()).find(|&i| self.icon_rect(i).contains(x, y)).map_or(Hover::None, Hover::Icon)
    }

    fn hover_rect(&self, h: Hover) -> Rect {
        match h {
            Hover::None => Rect::default(),
            Hover::Taskbar(_) => Rect::new(0, self.screen.h - TASKBAR_H, self.screen.w, TASKBAR_H),
            Hover::Caption(id, _) => self.index_of(id).map_or(Rect::default(), |i| {
                let r = self.windows[i].rect;
                Rect::new(r.x, r.y, r.w, TITLE_H)
            }),
            Hover::StartApp(_) | Hover::Recommended(_) | Hover::Power | Hover::PowerItem(_) => {
                self.start_rect().grow(4)
            }
            Hover::Icon(i) => self.icon_rect(i),
        }
    }

    fn set_hover(&mut self, h: Hover) {
        if h != self.hover {
            let (a, b) = (self.hover_rect(self.hover), self.hover_rect(h));
            for hv in [self.hover, h] {
                if let Hover::Caption(id, _) = hv {
                    if let Some(i) = self.index_of(id) {
                        self.windows[i].dirty = true;
                    }
                }
            }
            self.hover = h;
            self.damage(a);
            self.damage(b);
        }
    }

    // ---- input -----------------------------------------------------------

    fn mouse_down(&mut self) {
        let (x, y) = self.mouse;
        let t = now_ms();
        let double =
            t - self.last_click.0 < 450 && (x - self.last_click.1).abs() < 6 && (y - self.last_click.2).abs() < 6;
        self.last_click = if double { (0, -100, -100) } else { (t, x, y) };

        if self.start_open {
            match self.hover_at(x, y) {
                Hover::StartApp(i) => {
                    let kind = self.start_apps()[i];
                    self.close_start();
                    self.launch(kind, None);
                    return;
                }
                Hover::Recommended(i) => {
                    let (dir, e) = self.recommended[i].clone();
                    self.close_start();
                    let path =
                        if dir.ends_with('\\') { format!("{dir}{}", e.name) } else { format!("{dir}\\{}", e.name) };
                    self.open_file(&path);
                    return;
                }
                Hover::Power => {
                    self.power_open = !self.power_open;
                    let r = self.start_rect().grow(30);
                    self.damage(r);
                    return;
                }
                Hover::PowerItem(i) => {
                    let action = if i == 0 { power::SHUT_DOWN } else { power::RESTART };
                    let status = sys::power(action);
                    self.notice = Some(format!(
                        "Could not {} (status {:#x})",
                        if i == 0 { "shut down" } else { "restart" },
                        status as u32
                    ));
                    let r = self.start_rect().grow(30);
                    self.damage(r);
                    return;
                }
                _ if self.start_rect().contains(x, y) => return,
                _ => {
                    if !self.taskbar_items().first().is_some_and(|(r, _)| r.contains(x, y)) {
                        self.close_start();
                    }
                }
            }
        }

        if y >= self.screen.h - TASKBAR_H {
            if let Some((_, item)) = self.taskbar_items().into_iter().find(|(r, _)| r.contains(x, y)) {
                match item {
                    TaskItem::Start => {
                        if self.start_open {
                            self.close_start()
                        } else {
                            self.open_start()
                        }
                    }
                    TaskItem::App(kind) => {
                        let top = self.windows.iter().rev().find(|w| w.app.kind() == kind).map(|w| (w.id, w.minimized));
                        match top {
                            Some((id, false)) if self.focused == Some(id) => self.minimize(id),
                            Some((id, _)) => self.restore_and_focus(id),
                            None => self.launch(kind, None),
                        }
                    }
                }
            }
            return;
        }

        let hit = self.windows.iter().rev().find(|w| !w.minimized && w.rect.contains(x, y)).map(|w| (w.id, w.rect));
        if let Some((id, rect)) = hit {
            if self.focused != Some(id) {
                self.focus(id);
            }
            if y < rect.y + TITLE_H {
                if x >= rect.right() - 3 * CAPTION_W {
                    match (rect.right() - x - 1) / CAPTION_W {
                        0 => self.close(id),
                        1 => self.toggle_maximize(id),
                        _ => self.minimize(id),
                    }
                } else if double {
                    self.toggle_maximize(id);
                } else {
                    self.drag = Some((id, x - rect.x, y - rect.y));
                }
                return;
            }
            let Some(i) = self.index_of(id) else { return };
            let mut cx = Ctx { fonts: &mut self.fonts, mono: &self.mono, theme: self.theme };
            let actions = self.windows[i].app.click(x - rect.x, y - rect.y - TITLE_H, double, &mut cx);
            self.apply(id, actions);
            return;
        }

        // Desktop background and icons.
        let old = self.selected_icon;
        self.selected_icon = (0..DESKTOP_ICONS.len()).find(|&i| self.icon_rect(i).contains(x, y));
        for i in [old, self.selected_icon].into_iter().flatten() {
            let r = self.icon_rect(i);
            self.damage(r);
        }
        if double {
            if let Some(i) = self.selected_icon {
                let ic = &DESKTOP_ICONS[i];
                self.launch(ic.kind, ic.arg.map(String::from));
            }
        }
    }

    fn open_file(&mut self, path: &str) {
        match apps::open_path(path, false) {
            apps::Opened::Actions(a) => self.apply(0, a),
            apps::Opened::Started(_, name) => rt::debug_print(&format!("desktop: started {name}")),
            apps::Opened::Message(m) => rt::debug_print(&format!("desktop: {m}")),
        }
    }

    fn mouse_moved(&mut self) {
        let (x, y) = self.mouse;
        if let Some((id, ox, oy)) = self.drag {
            if let Some(i) = self.index_of(id) {
                let wa = self.work_area();
                let w = &mut self.windows[i];
                let old = w.rect;
                if w.maximized {
                    // Dragging a maximised window restores it under the pointer.
                    w.maximized = false;
                    w.rect = w.restore;
                    w.dirty = true;
                }
                let nx = (x - ox).clamp(-w.rect.w + 80, wa.w - 80);
                let ny = (y - oy).clamp(0, wa.h - TITLE_H);
                w.rect = Rect::new(nx, ny, w.rect.w, w.rect.h);
                let new = w.rect;
                self.damage(old.grow(24).union(&new.grow(24)));
            }
            return;
        }
        let h = self.hover_at(x, y);
        self.set_hover(h);
        // Hover feedback inside windows (where apps support it).
        if let Some(w) = self.windows.iter_mut().rev().find(|w| !w.minimized && w.rect.contains(x, y)) {
            if y > w.rect.y + TITLE_H && w.app.hover(x - w.rect.x, y - w.rect.y - TITLE_H) {
                w.dirty = true;
                let r = w.rect;
                self.damage(r);
            }
        }
    }

    fn wheel(&mut self, notches: i32) {
        let (x, y) = self.mouse;
        if let Some(w) = self.windows.iter_mut().rev().find(|w| !w.minimized && w.rect.contains(x, y)) {
            if w.app.wheel(notches) {
                w.dirty = true;
                let r = w.rect;
                self.damage(r);
            }
        }
    }

    fn key(&mut self, e: &InputEvent) {
        let pressed = e.flags & input::PRESSED != 0;
        if !pressed {
            return;
        }
        let alt = e.flags & input::ALT != 0;
        if e.key == key::SUPER {
            if self.start_open {
                self.close_start()
            } else {
                self.open_start()
            }
            return;
        }
        if self.start_open {
            match e.key {
                key::ESCAPE => self.close_start(),
                key::BACKSPACE => {
                    self.search.pop();
                    let r = self.start_rect();
                    self.damage(r);
                }
                key::ENTER => {
                    if let Some(kind) = self.start_apps().first().copied() {
                        self.close_start();
                        self.launch(kind, None);
                    }
                }
                key::CHAR => {
                    if let Some(c) = char::from_u32(e.ch) {
                        if self.search.len() < 40 {
                            self.search.push(c);
                        }
                        let r = self.start_rect();
                        self.damage(r);
                    }
                }
                _ => {}
            }
            return;
        }
        if alt && e.key == key::FUNCTION + 4 {
            if let Some(id) = self.focused {
                self.close(id);
            }
            return;
        }
        if alt && e.key == key::TAB {
            if let Some(next) = self.windows.iter().find(|w| Some(w.id) != self.focused).map(|w| w.id) {
                self.restore_and_focus(next);
            }
            return;
        }
        if let Some(id) = self.focused {
            if let Some(i) = self.index_of(id) {
                let mut cx = Ctx { fonts: &mut self.fonts, mono: &self.mono, theme: self.theme };
                let actions = self.windows[i].app.key(e, &mut cx);
                self.apply(id, actions);
            }
        }
    }

    fn handle(&mut self, e: &InputEvent) {
        match e.kind {
            input::MOUSE => {
                let (ox, oy) = self.mouse;
                let (x, y) = if e.flags & input::ABSOLUTE != 0 {
                    (
                        (e.dx as i64 * self.screen.w as i64 >> 16) as i32,
                        (e.dy as i64 * self.screen.h as i64 >> 16) as i32,
                    )
                } else {
                    (ox + e.dx, oy + e.dy)
                };
                self.mouse = (x.clamp(0, self.screen.w - 1), y.clamp(0, self.screen.h - 1));
                if self.mouse != (ox, oy) {
                    self.mouse_moved();
                }
                if e.wheel != 0 {
                    self.wheel(e.wheel);
                }
                let pressed = e.buttons & 1 != 0 && self.buttons & 1 == 0;
                let released = e.buttons & 1 == 0 && self.buttons & 1 != 0;
                self.buttons = e.buttons;
                if pressed {
                    self.mouse_down();
                }
                if released {
                    self.drag = None;
                }
            }
            input::KEY => self.key(e),
            _ => {}
        }
    }

    fn tick(&mut self) {
        let t = sys::query_time();
        let (h12, ampm) = match t.hour {
            0 => (12, "AM"),
            1..=11 => (t.hour, "AM"),
            12 => (12, "PM"),
            h => (h - 12, "PM"),
        };
        let clock = format!("{h12}:{:02} {ampm}", t.minute);
        let date = format!("{}/{}/{}", t.month, t.day, t.year);
        if clock != self.clock || date != self.date {
            self.clock = clock;
            self.date = date;
            self.damage(Rect::new(self.screen.w - 140, self.screen.h - TASKBAR_H, 140, TASKBAR_H));
        }
        for i in 0..self.windows.len() {
            if self.windows[i].app.tick() && !self.windows[i].minimized {
                self.windows[i].dirty = true;
                let r = self.windows[i].rect;
                self.damage(r);
            }
        }
    }

    // ---- rendering -------------------------------------------------------

    fn render_window(&mut self, i: usize) {
        let focused = self.focused == Some(self.windows[i].id);
        let hover = self.hover;
        let theme = self.theme;
        let w = &mut self.windows[i];
        if w.surface.w != w.rect.w || w.surface.h != w.rect.h {
            w.surface = Canvas::new(w.rect.w, w.rect.h, 0);
        }
        let cv = &mut w.surface;
        cv.reset_clip();
        let t = &theme;
        cv.fill(cv.bounds(), t.window());
        let bar = Rect::new(0, 0, cv.w, TITLE_H);
        cv.fill(bar, t.title(focused));
        ui::icon(cv, w.app.icon(), 10, 8, 16, t);
        let title = w.app.title();
        let text_color = if focused { t.text() } else { t.text_dim() };
        self.fonts.draw_fit(cv, 36, 8, &title, cv.w - 36 - 3 * CAPTION_W - 8, 12, Weight::Regular, text_color);
        for b in 0..3u8 {
            let r = Rect::new(cv.w - (b as i32 + 1) * CAPTION_W, 0, CAPTION_W, TITLE_H);
            let hovered = hover == Hover::Caption(w.id, b);
            if hovered {
                cv.fill(r, if b == 0 { rgb(0xC4, 0x2B, 0x1C) } else { t.hover() });
            }
            let c = if hovered && b == 0 { rgb(255, 255, 255) } else { text_color };
            let (cx, cy) = (r.x as f32 + 23.0, 16.0);
            match b {
                0 => {
                    cv.line(cx - 5.0, cy - 5.0, cx + 5.0, cy + 5.0, 1.2, c);
                    cv.line(cx + 5.0, cy - 5.0, cx - 5.0, cy + 5.0, 1.2, c);
                }
                1 => {
                    if w.maximized {
                        cv.stroke(Rect::new(r.x + 18, 13, 9, 9), c);
                        cv.fill(Rect::new(r.x + 20, 11, 9, 1), c);
                        cv.fill(Rect::new(r.x + 28, 11, 1, 9), c);
                    } else {
                        cv.stroke(Rect::new(r.x + 18, 11, 10, 10), c);
                    }
                }
                _ => cv.fill(Rect::new(r.x + 18, 16, 10, 1), c),
            }
        }
        let content = Rect::new(0, TITLE_H, cv.w, cv.h - TITLE_H);
        cv.set_clip(content);
        let mut ctx = Ctx { fonts: &mut self.fonts, mono: &self.mono, theme };
        w.app.draw(cv, content, &mut ctx);
        cv.reset_clip();
        cv.stroke(cv.bounds(), if focused { with_alpha(t.accent, 140) } else { t.border() });
        if !w.maximized {
            cv.round_corners(RADIUS);
        }
        w.dirty = false;
    }

    fn compose(&mut self, area: Rect) {
        let b = &mut self.back;
        b.set_clip(area);
        b.blit(&self.wallpaper, Rect::new(area.x, area.y, area.w, area.h), area.x, area.y);
        // Desktop icons.
        for (i, ic) in DESKTOP_ICONS.iter().enumerate() {
            let r = Rect::new(12, 12 + i as i32 * 100, 88, 92);
            if r.intersect(&area).is_none() {
                continue;
            }
            if self.selected_icon == Some(i) {
                b.fill_rounded(r, 4, with_alpha(rgb(255, 255, 255), 50));
            } else if self.hover == Hover::Icon(i) {
                b.fill_rounded(r, 4, with_alpha(rgb(255, 255, 255), 28));
            }
            ui::icon(b, ic.icon, r.x + 20, r.y + 8, 48, &self.theme);
            let lw = self.fonts.measure(ic.label, 12, Weight::Regular);
            let lx = r.x + (r.w - lw.min(r.w)) / 2;
            self.fonts.draw_fit(b, lx + 1, r.y + 63, ic.label, r.w, 12, Weight::Regular, with_alpha(0, 160));
            self.fonts.draw_fit(b, lx, r.y + 62, ic.label, r.w, 12, Weight::Regular, rgb(255, 255, 255));
        }
        // Windows, bottom to top.
        for w in self.windows.iter().filter(|w| !w.minimized) {
            if w.rect.grow(24).intersect(&area).is_none() {
                continue;
            }
            if !w.maximized {
                b.shadow(w.rect, 6, RADIUS, 22, if self.theme.dark { 120 } else { 70 });
            }
            b.blit(&w.surface, w.surface.bounds(), w.rect.x, w.rect.y);
        }
        self.draw_taskbar(area);
        if self.start_open {
            self.draw_start(area);
        }
        self.back.reset_clip();
    }

    fn draw_taskbar(&mut self, area: Rect) {
        let t = self.theme;
        let bar = Rect::new(0, self.screen.h - TASKBAR_H, self.screen.w, TASKBAR_H);
        if bar.intersect(&area).is_none() {
            return;
        }
        let items = self.taskbar_items();
        let b = &mut self.back;
        b.fill(bar, t.taskbar());
        b.fill(Rect::new(0, bar.y, bar.w, 1), with_alpha(rgb(255, 255, 255), if t.dark { 20 } else { 120 }));
        for (i, (r, item)) in items.into_iter().enumerate() {
            let hovered = self.hover == Hover::Taskbar(i);
            let (icon, running, active) = match item {
                TaskItem::Start => (Icon::Start, false, self.start_open),
                TaskItem::App(k) => {
                    let running = self.windows.iter().any(|w| w.app.kind() == k);
                    let active = self
                        .focused
                        .and_then(|f| self.windows.iter().find(|w| w.id == f))
                        .is_some_and(|w| w.app.kind() == k && !w.minimized);
                    (apps::icon_for(k), running, active)
                }
            };
            if hovered || active {
                b.fill_rounded(r, 5, if active { t.pressed() } else { t.hover() });
            }
            ui::icon(b, icon, r.x + 10, r.y + 8, 24, &t);
            if running {
                let iw = if active { 16 } else { 6 };
                b.fill_rounded(
                    Rect::new(r.x + (r.w - iw) / 2, r.bottom() - 4, iw, 3),
                    1,
                    if active { t.accent } else { t.text_dim() },
                );
            }
        }
        // Clock.
        let cw = self.fonts.measure(&self.clock, 12, Weight::Regular).max(self.fonts.measure(
            &self.date,
            12,
            Weight::Regular,
        ));
        let cx = self.screen.w - cw - 16;
        let (clock, date) = (self.clock.clone(), self.date.clone());
        let b = &mut self.back;
        let cw1 = self.fonts.measure(&clock, 12, Weight::Regular);
        let cw2 = self.fonts.measure(&date, 12, Weight::Regular);
        self.fonts.draw(b, cx + cw - cw1, bar.y + 7, &clock, 12, Weight::Regular, t.text());
        self.fonts.draw(b, cx + cw - cw2, bar.y + 25, &date, 12, Weight::Regular, t.text());
    }

    fn draw_start(&mut self, area: Rect) {
        let t = self.theme;
        let s = self.start_rect();
        if s.grow(30).intersect(&area).is_none() {
            return;
        }
        let apps = self.start_apps();
        let recommended = self.recommended.clone();
        let (hover, search, power_open, notice) =
            (self.hover, self.search.clone(), self.power_open, self.notice.clone());
        let f = &mut self.fonts;
        let b = &mut self.back;
        b.shadow(s, 8, 10, 30, if t.dark { 140 } else { 80 });
        b.fill_rounded(s, 10, t.menu());
        b.stroke(Rect::new(s.x, s.y, s.w, s.h), with_alpha(t.border(), 160));
        // Search box.
        let sb = Rect::new(s.x + 32, s.y + 28, s.w - 64, 36);
        b.rounded_box(sb, 18, t.border(), t.surface());
        let (text, color) =
            if search.is_empty() { ("Type to search apps", t.text_dim()) } else { (search.as_str(), t.text()) };
        let tw = f.draw(b, sb.x + 20, sb.y + 9, text, 14, Weight::Regular, color);
        if !search.is_empty() {
            b.fill(Rect::new(sb.x + 20 + tw + 1, sb.y + 9, 1, 18), t.text());
        }
        f.draw(
            b,
            s.x + 40,
            s.y + 78,
            if search.is_empty() { "Pinned" } else { "Best matches" },
            14,
            Weight::Semibold,
            t.text(),
        );
        for (i, kind) in apps.iter().enumerate() {
            let r = Rect::new(s.x + 32 + (i as i32 % 6) * 90, s.y + 104 + (i as i32 / 6) * 92, 84, 84);
            if hover == Hover::StartApp(i) {
                b.fill_rounded(r, 6, t.hover());
            }
            ui::icon(b, apps::icon_for(*kind), r.x + 24, r.y + 10, 36, &t);
            let name = apps::name_of(*kind);
            let nw = f.measure(name, 12, Weight::Regular).min(r.w - 4);
            f.draw_fit(b, r.x + (r.w - nw) / 2, r.y + 54, name, r.w - 4, 12, Weight::Regular, t.text());
        }
        if apps.is_empty() {
            f.draw(b, s.x + 40, s.y + 112, "No apps match your search", 14, Weight::Regular, t.text_dim());
        }
        f.draw(b, s.x + 40, s.y + 270, "Recommended", 14, Weight::Semibold, t.text());
        for (i, (dir, e)) in recommended.iter().enumerate() {
            let r = Rect::new(s.x + 32 + (i as i32 % 2) * 272, s.y + 300 + (i as i32 / 2) * 56, 264, 50);
            if hover == Hover::Recommended(i) {
                b.fill_rounded(r, 6, t.hover());
            }
            let icon = if e.name.to_lowercase().ends_with(".exe") {
                Icon::Program
            } else if e.name.to_lowercase().ends_with(".txt") {
                Icon::TextFile
            } else {
                Icon::File
            };
            ui::icon(b, icon, r.x + 10, r.y + 9, 32, &t);
            f.draw_fit(b, r.x + 52, r.y + 7, &e.name, r.w - 60, 14, Weight::Regular, t.text());
            f.draw_fit(b, r.x + 52, r.y + 27, dir, r.w - 60, 12, Weight::Regular, t.text_dim());
        }
        // Footer: user and power.
        let footer = Rect::new(s.x, s.bottom() - 72, s.w, 72);
        b.fill(Rect::new(footer.x + 1, footer.y, footer.w - 2, 1), t.border());
        b.circle((s.x + 52) as f32, (footer.y + 36) as f32, 16.0, shade(t.accent, -10));
        f.draw(b, s.x + 46, footer.y + 26, "M", 14, Weight::Semibold, rgb(255, 255, 255));
        f.draw(b, s.x + 80, footer.y + 26, "Meridian user", 14, Weight::Regular, t.text());
        if let Some(n) = &notice {
            f.draw_fit(b, s.x + 220, footer.y + 27, n, 300, 12, Weight::Regular, rgb(0xF8, 0x71, 0x71));
        }
        let p = Rect::new(s.right() - 60, s.bottom() - 56, 40, 40);
        if hover == Hover::Power || power_open {
            b.fill_rounded(p, 6, t.hover());
        }
        ui::icon(b, Icon::Power, p.x + 10, p.y + 10, 20, &t);
        if power_open {
            let menu = Rect::new(p.right() - 188, p.y - 104, 188, 96);
            b.shadow(menu, 4, 8, 16, 90);
            b.rounded_box(menu, 8, t.border(), t.surface());
            for (i, (label, icon)) in [("Shut down", Icon::Power), ("Restart", Icon::Restart)].iter().enumerate() {
                let r = Rect::new(p.right() - 180, p.y - 96 + i as i32 * 44, 180, 40);
                if hover == Hover::PowerItem(i) {
                    b.fill_rounded(r.inset(2), 5, t.hover());
                }
                ui::icon(b, *icon, r.x + 12, r.y + 10, 20, &t);
                f.draw(b, r.x + 44, r.y + 11, label, 14, Weight::Regular, t.text());
            }
        }
    }

    /// Copies `area` of the back buffer to the screen.
    fn present(&mut self, area: Rect) {
        let Some(a) = area.intersect(&self.screen) else { return };
        let stride = self.info.stride as usize;
        let swap = self.info.format == 0; // RGBX: red in the low byte
        for y in a.y..a.bottom() {
            let src = &self.back.px[(y * self.back.w + a.x) as usize..(y * self.back.w + a.right()) as usize];
            // SAFETY: the framebuffer mapping covers stride * height pixels.
            unsafe {
                let dst = self.fb.add(y as usize * stride + a.x as usize);
                if swap {
                    for (i, p) in src.iter().enumerate() {
                        let v = (p & 0xFF00FF00) | (p & 0xFF) << 16 | (p >> 16) & 0xFF;
                        dst.add(i).write_volatile(v);
                    }
                } else {
                    core::ptr::copy_nonoverlapping(src.as_ptr(), dst, src.len());
                }
            }
        }
    }

    fn draw_cursor(&mut self) {
        let (mx, my) = self.mouse;
        let stride = self.info.stride as usize;
        let (black, white) = (0xFF00_0000u32, 0xFFFF_FFFFu32);
        for (row, line) in ui::CURSOR.iter().enumerate() {
            for (col, &c) in line.iter().enumerate() {
                let (x, y) = (mx + col as i32, my + row as i32);
                if x >= self.screen.w || y >= self.screen.h || !(c == b'1' || c == b'2') {
                    continue;
                }
                // SAFETY: bounds checked against the screen.
                unsafe {
                    self.fb.add(y as usize * stride + x as usize).write_volatile(if c == b'1' { black } else { white })
                };
            }
        }
        self.cursor_drawn = Rect::new(mx, my, 12, 19);
    }

    fn flush(&mut self) {
        let Some(area) = self.damage.take() else {
            if self.cursor_drawn.x != self.mouse.0 || self.cursor_drawn.y != self.mouse.1 {
                let old = self.cursor_drawn;
                self.present(old);
                self.draw_cursor();
            }
            return;
        };
        for i in 0..self.windows.len() {
            if self.windows[i].dirty && !self.windows[i].minimized {
                self.render_window(i);
            }
        }
        self.compose(area);
        let old = self.cursor_drawn;
        self.present(area);
        if old.intersect(&area).is_none() || self.cursor_drawn.x != self.mouse.0 || self.cursor_drawn.y != self.mouse.1
        {
            self.present(old);
        }
        self.draw_cursor();
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn meridian_start(_arg: u64) -> ! {
    let (fb, info) = match sys::acquire_display() {
        Ok(v) => v,
        Err(e) => {
            rt::debug_print(&format!("desktop: cannot take the display (status {:#x})", e as u32));
            rt::exit_process(1);
        }
    };
    SCREEN_W.store(info.width as i32, Ordering::Relaxed);
    SCREEN_H.store(info.height as i32, Ordering::Relaxed);
    let mut desk = Desktop::new(fb, info);
    desk.tick();
    desk.flush();
    rt::debug_print(&format!("desktop: ready ({}x{})", info.width, info.height));

    let mut events = [InputEvent::default(); 64];
    let mut next_tick = now_ms() + 1000;
    loop {
        let now = now_ms();
        let timeout = if desk.drag.is_some() { 15 } else { next_tick.saturating_sub(now).clamp(1, 1000) };
        let n = sys::read_input(&mut events, Some(timeout)).unwrap_or(0);
        for e in &events[..n] {
            desk.handle(e);
        }
        if now_ms() >= next_tick {
            desk.tick();
            next_tick = now_ms() + 1000;
        }
        desk.flush();
    }
}
