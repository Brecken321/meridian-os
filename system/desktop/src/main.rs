//! The Meridian desktop: a user-mode program that owns the screen, keyboard
//! and mouse and provides the window manager, taskbar, Start menu, desktop
//! icons, notifications, the lock screen and the built-in apps.
//!
//! Rendering is damage-based: only the parts of the screen that changed are
//! recomposed into a back buffer and copied to the framebuffer. Every window
//! keeps two surfaces (title bar and content); the taskbar, menus and the
//! focused title bar are drawn over a blurred copy of the wallpaper
//! ("Mica"). The pointer is drawn straight onto the framebuffer.

#![no_std]
#![no_main]

extern crate alloc;

mod apps;
mod bmp;
mod calc;
mod clock;
mod explorer;
mod font;
mod games;
mod gfx;
mod notepad;
mod paint;
mod photos;
mod ui;
mod widgets;

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicI32, Ordering};

use apps::{ALL_APPS, Action, App, AppKind, Clipboard, Ctx, MenuItem};
use font::{Font, Fonts, Mono, Weight};
use gfx::{Canvas, Color, Rect, rgb, shade, with_alpha};
use meridian_rt::sys::{self, DisplayInfo, InputEvent, input, key, power, volume_flags};
use meridian_rt::{self as rt};
use ui::{Icon, Theme};

pub const TITLE_H: i32 = 32;
const TASKBAR_H: i32 = 48;
const CAPTION_W: i32 = 46;
const RADIUS: i32 = 8;
const START_W: i32 = 640;
const START_H: i32 = 640;
const GRIP: i32 = 6;
const ICON_W: i32 = 88;
const ICON_H: i32 = 96;
const SETTINGS_DIR: &str = "D:\\Meridian";
const SETTINGS_FILE: &str = "D:\\Meridian\\settings.ini";

static REGULAR: &[u8] = include_bytes!("../assets/selawik-regular.ttf");
static SEMIBOLD: &[u8] = include_bytes!("../assets/selawik-semibold.ttf");
static MONO_FONT: &[u8; 4096] = include_bytes!("../../../kernel/assets/font8x16.bin");

static SCREEN_W: AtomicI32 = AtomicI32::new(0);
static SCREEN_H: AtomicI32 = AtomicI32::new(0);

pub fn screen_size() -> (i32, i32) {
    (SCREEN_W.load(Ordering::Relaxed), SCREEN_H.load(Ordering::Relaxed))
}

const PINNED: [AppKind; 7] = [
    AppKind::Explorer,
    AppKind::Terminal,
    AppKind::Notepad,
    AppKind::Paint,
    AppKind::Calculator,
    AppKind::TaskManager,
    AppKind::Settings,
];

fn now_ms() -> u64 {
    rt::uptime_us() / 1000
}

struct Window {
    id: u32,
    app: Box<dyn App>,
    rect: Rect,
    /// Where the window returns to from maximised or snapped.
    restore: Rect,
    minimized: bool,
    maximized: bool,
    snapped: bool,
    title: Canvas,
    content: Canvas,
    title_dirty: bool,
    content_dirty: bool,
    next_tick: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hover {
    None,
    Taskbar(usize),
    Caption(u32, u8),
    StartApp(usize),
    StartResult(usize),
    Recommended(usize),
    Power,
    PowerItem(usize),
    Icon(usize),
    Popup(usize),
    Clock,
    ShowDesktop,
    CalendarNav(i32),
}

#[derive(Clone, Copy)]
enum TaskItem {
    Start,
    App(AppKind),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Drag {
    Move { id: u32, ox: i32, oy: i32 },
    Resize { id: u32, edges: u8, start: Rect, mx: i32, my: i32 },
    App { id: u32 },
}

const LEFT: u8 = 1;
const RIGHT: u8 = 2;
const TOP: u8 = 4;
const BOTTOM: u8 = 8;

#[derive(Clone, Copy, PartialEq, Eq)]
enum CursorKind {
    Arrow,
    Horizontal,
    Vertical,
    Diagonal,
    AntiDiagonal,
}

#[derive(Clone)]
enum PopupOwner {
    Window(u32),
    Title(u32),
    Desktop,
    DesktopIcon(usize),
    Taskbar(AppKind),
}

struct Popup {
    owner: PopupOwner,
    items: Vec<MenuItem>,
    rect: Rect,
    hover: Option<usize>,
}

struct Toast {
    text: String,
    until: u64,
}

#[derive(Clone)]
enum DeskTarget {
    App(AppKind, Option<String>),
    Path(String, bool),
}

#[derive(Clone)]
struct DeskIcon {
    icon: Icon,
    label: String,
    target: DeskTarget,
}

/// A Start menu search hit: an app or a file.
#[derive(Clone)]
enum Hit {
    App(AppKind),
    File(String, bool),
}

// Context-menu ids used by the shell itself.
const D_NEW_FOLDER: u32 = 1;
const D_NEW_FILE: u32 = 2;
const D_REFRESH: u32 = 3;
const D_TERMINAL: u32 = 4;
const D_PERSONALISE: u32 = 5;
const D_OPEN: u32 = 10;
const D_LOCATION: u32 = 11;
const D_DELETE: u32 = 12;
const T_NEW: u32 = 20;
const T_CLOSE_ALL: u32 = 21;
const W_RESTORE: u32 = 30;
const W_MINIMIZE: u32 = 31;
const W_MAXIMIZE: u32 = 32;
const W_CLOSE: u32 = 33;

struct Desktop {
    fb: *mut u32,
    info: DisplayInfo,
    screen: Rect,
    back: Canvas,
    wallpaper: Canvas,
    blur: Canvas,
    fonts: Fonts,
    mono: Mono,
    theme: Theme,
    clipboard: Clipboard,
    windows: Vec<Window>,
    focused: Option<u32>,
    next_id: u32,
    start_open: bool,
    search: String,
    recommended: Vec<(String, bool)>,
    power_open: bool,
    calendar: Option<(i32, u32)>,
    popup: Option<Popup>,
    toasts: Vec<Toast>,
    locked: bool,
    icons: Vec<DeskIcon>,
    selected_icon: Option<usize>,
    data_drive: bool,
    mouse: (i32, i32),
    buttons: u32,
    drag: Option<Drag>,
    snap_preview: Option<Rect>,
    hover: Hover,
    super_down: bool,
    super_used: bool,
    tooltip_hidden: bool,
    last_click: (u64, i32, i32),
    clock: String,
    date: String,
    damage: Option<Rect>,
    cursor_drawn: Rect,
    cursor: CursorKind,
    cursors: Vec<(CursorKind, Canvas)>,
}

/// Day of the week (0 = Sunday) by Sakamoto's method.
fn weekday(y: i32, m: u32, d: u32) -> u32 {
    const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if m < 3 { y - 1 } else { y };
    ((y + y / 4 - y / 100 + y / 400 + T[(m - 1) as usize] + d as i32).rem_euclid(7)) as u32
}

fn days_in(y: i32, m: u32) -> u32 {
    match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

impl Desktop {
    fn new(fb: *mut u32, info: DisplayInfo) -> Self {
        let (w, h) = (info.width as i32, info.height as i32);
        let mut theme = Theme { dark: true, accent: ui::ACCENTS[0].1, wallpaper: 0, taskbar_left: false };
        if let Ok(data) = sys::read_file(SETTINGS_FILE) {
            theme = theme.from_ini(&String::from_utf8_lossy(&data));
        }
        let regular = Font::parse(REGULAR).expect("bundled font is valid");
        let semibold = Font::parse(SEMIBOLD).expect("bundled font is valid");
        let wallpaper = ui::wallpaper(w, h, &theme);
        let blur = wallpaper.blurred(8);
        let data_drive = sys::query_volumes().iter().any(|v| v.flags & volume_flags::WRITABLE != 0);
        let mut d = Self {
            fb,
            info,
            screen: Rect::new(0, 0, w, h),
            back: Canvas::new(w, h, rgb(0, 0, 0)),
            wallpaper,
            blur,
            fonts: Fonts::new(regular, semibold),
            mono: Mono::new(MONO_FONT),
            theme,
            clipboard: Clipboard::default(),
            windows: Vec::new(),
            focused: None,
            next_id: 1,
            start_open: false,
            search: String::new(),
            recommended: Vec::new(),
            power_open: false,
            calendar: None,
            popup: None,
            toasts: Vec::new(),
            locked: false,
            icons: Vec::new(),
            selected_icon: None,
            data_drive,
            mouse: (w / 2, h / 2),
            buttons: 0,
            drag: None,
            snap_preview: None,
            hover: Hover::None,
            super_down: false,
            super_used: false,
            tooltip_hidden: false,
            last_click: (0, -100, -100),
            clock: String::new(),
            date: String::new(),
            damage: Some(Rect::new(0, 0, w, h)),
            cursor_drawn: Rect::default(),
            cursor: CursorKind::Arrow,
            cursors: Vec::new(),
        };
        d.load_icons();
        d.build_cursors();
        d
    }

    fn work_area(&self) -> Rect {
        Rect::new(0, 0, self.screen.w, self.screen.h - TASKBAR_H)
    }

    fn damage(&mut self, r: Rect) {
        if let Some(r) = r.intersect(&self.screen) {
            self.damage = Some(self.damage.map_or(r, |d| d.union(&r)));
        }
    }

    fn damage_all(&mut self) {
        self.damage(self.screen);
    }

    fn notify(&mut self, text: String) {
        self.toasts.push(Toast { text, until: now_ms() + 4500 });
        if self.toasts.len() > 3 {
            self.toasts.remove(0);
        }
        self.damage(self.toast_area());
    }

    fn toast_area(&self) -> Rect {
        Rect::new(self.screen.w - 400, self.screen.h - TASKBAR_H - 12 - 3 * 84, 400, 3 * 84 + 12)
    }

    // ---- desktop icons ---------------------------------------------------

    fn load_icons(&mut self) {
        let mut icons = vec![
            DeskIcon { icon: Icon::ThisPc, label: "This PC".into(), target: DeskTarget::App(AppKind::Explorer, None) },
            DeskIcon {
                icon: Icon::TextFile,
                label: "Welcome to Meridian".into(),
                target: DeskTarget::Path("C:\\Welcome to Meridian.txt".into(), false),
            },
        ];
        if self.data_drive {
            icons.push(DeskIcon {
                icon: Icon::Documents,
                label: "Documents".into(),
                target: DeskTarget::Path("D:\\Documents".into(), true),
            });
        }
        icons.push(DeskIcon {
            icon: Icon::Terminal,
            label: "Terminal".into(),
            target: DeskTarget::App(AppKind::Terminal, None),
        });
        if let Ok(mut list) = sys::list_directory("D:\\Desktop") {
            list.sort_by_key(|e| e.name.to_lowercase());
            for e in list {
                let path = apps::join("D:\\Desktop", &e.name);
                icons.push(DeskIcon {
                    icon: apps::file_icon(&e.name, e.is_dir),
                    label: e.name.clone(),
                    target: DeskTarget::Path(path, e.is_dir),
                });
            }
        }
        self.icons = icons;
        self.selected_icon = None;
        self.damage(self.work_area());
    }

    fn icon_rect(&self, i: usize) -> Rect {
        let per_col = ((self.screen.h - TASKBAR_H - 12) / ICON_H).max(1) as usize;
        Rect::new(10 + (i / per_col) as i32 * (ICON_W + 8), 10 + (i % per_col) as i32 * ICON_H, ICON_W, ICON_H - 4)
    }

    fn open_icon(&mut self, i: usize) {
        let Some(ic) = self.icons.get(i).cloned() else { return };
        match ic.target {
            DeskTarget::App(kind, arg) => self.launch(kind, arg),
            DeskTarget::Path(path, is_dir) => self.open_path(&path, is_dir),
        }
    }

    fn open_path(&mut self, path: &str, is_dir: bool) {
        match apps::open_path(path, is_dir) {
            apps::Opened::Actions(a) => self.apply(0, a),
            apps::Opened::Started(_, name) => self.notify(format!("Started {name}")),
            apps::Opened::Message(m) => self.notify(m),
        }
    }

    // ---- windows ----------------------------------------------------------

    fn index_of(&self, id: u32) -> Option<usize> {
        self.windows.iter().position(|w| w.id == id)
    }

    fn launch(&mut self, kind: AppKind, arg: Option<String>) {
        // Single-instance apps are brought forward instead of reopened.
        if matches!(
            kind,
            AppKind::TaskManager | AppKind::Settings | AppKind::About | AppKind::Calculator | AppKind::Clock
        ) {
            if let Some(id) = self.windows.iter().find(|w| w.app.kind() == kind).map(|w| w.id) {
                self.restore_and_focus(id);
                return;
            }
        }
        let app = apps::create(kind, arg);
        let (cw, ch) = app.size();
        let n = self.windows.iter().filter(|w| !w.minimized).count() as i32 % 6;
        let wa = self.work_area();
        let (w, h) = (cw.min(wa.w - 40), (ch + TITLE_H).min(wa.h - 40));
        let x = ((wa.w - w) / 2 - 80 + n * 32).clamp(0, (wa.w - w).max(0));
        let y = ((wa.h - h) / 2 - 40 + n * 28).clamp(0, (wa.h - h).max(0));
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
            snapped: false,
            title: Canvas::new(1, 1, 0),
            content: Canvas::new(1, 1, 0),
            title_dirty: true,
            content_dirty: true,
            next_tick: now_ms() + 50,
        });
        self.focus(id);
    }

    fn mark(&mut self, i: usize, title: bool, content: bool) {
        let w = &mut self.windows[i];
        w.title_dirty |= title;
        w.content_dirty |= content;
        let r = w.rect;
        self.damage(r);
    }

    fn focus(&mut self, id: u32) {
        let old = self.focused;
        if let Some(i) = self.index_of(id) {
            let w = self.windows.remove(i);
            self.windows.push(w);
        }
        self.focused = Some(id);
        for i in 0..self.windows.len() {
            if Some(self.windows[i].id) == old || self.windows[i].id == id {
                self.windows[i].title_dirty = true;
                let r = self.windows[i].rect.grow(24);
                self.damage(r);
            }
        }
        self.damage_taskbar();
    }

    fn restore_and_focus(&mut self, id: u32) {
        if let Some(i) = self.index_of(id) {
            self.windows[i].minimized = false;
            self.mark(i, true, true);
            let r = self.windows[i].rect.grow(24);
            self.damage(r);
        }
        self.focus(id);
    }

    /// Closes a window if its app agrees (Notepad asks to save first).
    fn request_close(&mut self, id: u32) {
        if let Some(i) = self.index_of(id) {
            if self.windows[i].app.close_requested() {
                self.close(id);
            } else {
                self.mark(i, true, true);
                self.restore_and_focus(id);
            }
        }
    }

    fn close(&mut self, id: u32) {
        if let Some(i) = self.index_of(id) {
            let w = self.windows.remove(i);
            self.damage(w.rect.grow(24));
            if self.focused == Some(id) {
                self.focused = self.windows.iter().rev().find(|w| !w.minimized).map(|w| w.id);
                if let Some(f) = self.focused.and_then(|f| self.index_of(f)) {
                    self.mark(f, true, false);
                }
            }
            if matches!(self.drag, Some(Drag::App { id: d } | Drag::Move { id: d, .. } | Drag::Resize { id: d, .. }) if d == id)
            {
                self.drag = None;
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
                if let Some(f) = self.focused.and_then(|f| self.index_of(f)) {
                    self.mark(f, true, false);
                }
            }
            self.damage_taskbar();
        }
    }

    fn set_rect(&mut self, i: usize, rect: Rect) {
        let old = self.windows[i].rect;
        let w = &mut self.windows[i];
        let resized = old.w != rect.w || old.h != rect.h;
        w.rect = rect;
        w.title_dirty = true;
        w.content_dirty |= resized;
        self.damage(old.grow(24).union(&rect.grow(24)));
    }

    fn toggle_maximize(&mut self, id: u32) {
        let wa = self.work_area();
        if let Some(i) = self.index_of(id) {
            let (maximized, snapped, restore, rect) = {
                let w = &self.windows[i];
                (w.maximized, w.snapped, w.restore, w.rect)
            };
            if maximized {
                self.windows[i].maximized = false;
                self.set_rect(i, restore);
            } else {
                if !snapped {
                    self.windows[i].restore = rect;
                }
                self.windows[i].maximized = true;
                self.windows[i].snapped = false;
                self.set_rect(i, wa);
            }
        }
    }

    /// Snaps a window to the left (-1) or right (1) half of the screen.
    fn snap(&mut self, id: u32, side: i32) {
        let wa = self.work_area();
        if let Some(i) = self.index_of(id) {
            let w = &mut self.windows[i];
            if !w.maximized && !w.snapped {
                w.restore = w.rect;
            }
            w.maximized = false;
            w.snapped = true;
            let half = wa.w / 2;
            let r = if side < 0 { Rect::new(0, 0, half, wa.h) } else { Rect::new(half, 0, wa.w - half, wa.h) };
            self.set_rect(i, r);
        }
    }

    fn unsnap(&mut self, id: u32) {
        if let Some(i) = self.index_of(id) {
            if self.windows[i].maximized || self.windows[i].snapped {
                self.windows[i].maximized = false;
                self.windows[i].snapped = false;
                let r = self.windows[i].restore;
                self.set_rect(i, r);
            }
        }
    }

    fn apply(&mut self, id: u32, actions: Vec<Action>) {
        for a in actions {
            match a {
                Action::Open(kind, arg) => self.launch(kind, arg),
                Action::SetTheme(t) => self.set_theme(t, true),
                Action::Close => self.close(id),
                Action::Notify(text) => self.notify(text),
                Action::Menu(items, x, y) => {
                    if let Some(i) = self.index_of(id) {
                        let r = self.windows[i].rect;
                        self.open_popup(PopupOwner::Window(id), items, r.x + x, r.y + TITLE_H + y);
                    }
                }
                Action::FilesChanged => {
                    for i in 0..self.windows.len() {
                        if self.windows[i].app.files_changed() {
                            self.mark(i, true, true);
                        }
                    }
                    self.load_icons();
                }
            }
        }
        if let Some(i) = self.index_of(id) {
            self.mark(i, true, true);
        }
        self.damage_taskbar();
    }

    fn set_theme(&mut self, t: Theme, save: bool) {
        let regenerate =
            t.wallpaper != self.theme.wallpaper || t.dark != self.theme.dark || t.accent != self.theme.accent;
        self.theme = t;
        if regenerate {
            self.wallpaper = ui::wallpaper(self.screen.w, self.screen.h, &t);
            self.blur = self.wallpaper.blurred(8);
        }
        for w in self.windows.iter_mut() {
            w.title_dirty = true;
            w.content_dirty = true;
        }
        self.damage_all();
        if save {
            let _ = sys::create_directory(SETTINGS_DIR);
            if let Err(e) = sys::write_file(SETTINGS_FILE, t.to_ini().as_bytes()) {
                self.notify(format!("Settings could not be saved: {}", apps::error_text(e)));
            }
        }
    }

    // ---- popups -----------------------------------------------------------

    fn open_popup(&mut self, owner: PopupOwner, items: Vec<MenuItem>, x: i32, y: i32) {
        self.close_popup();
        let width = items.iter().map(|i| self.fonts.measure(&i.label, 14, Weight::Regular)).max().unwrap_or(100) + 48;
        let height: i32 = items.iter().map(|i| if i.id == 0 { 9 } else { 32 }).sum::<i32>() + 8;
        let w = width.max(180);
        let wa = self.work_area();
        let px = x.min(wa.right() - w - 4).max(4);
        let py = if y + height > wa.bottom() { (y - height).max(4) } else { y };
        let rect = Rect::new(px, py, w, height);
        self.popup = Some(Popup { owner, items, rect, hover: None });
        self.damage(rect.grow(20));
    }

    fn close_popup(&mut self) {
        if let Some(p) = self.popup.take() {
            self.damage(p.rect.grow(20));
        }
    }

    fn popup_item_at(&self, x: i32, y: i32) -> Option<usize> {
        let p = self.popup.as_ref()?;
        if !p.rect.contains(x, y) {
            return None;
        }
        let mut iy = p.rect.y + 4;
        for (i, it) in p.items.iter().enumerate() {
            let h = if it.id == 0 { 9 } else { 32 };
            if (iy..iy + h).contains(&y) {
                return (it.id != 0 && it.enabled).then_some(i);
            }
            iy += h;
        }
        None
    }

    fn choose_popup(&mut self, index: usize) {
        let Some(p) = self.popup.take() else { return };
        self.damage(p.rect.grow(20));
        let id = p.items[index].id;
        match p.owner {
            PopupOwner::Window(wid) => {
                if let Some(i) = self.index_of(wid) {
                    let mut cx = Ctx {
                        fonts: &mut self.fonts,
                        mono: &self.mono,
                        theme: self.theme,
                        clipboard: &mut self.clipboard,
                    };
                    let actions = self.windows[i].app.menu(id, &mut cx);
                    self.apply(wid, actions);
                }
            }
            PopupOwner::Title(wid) => match id {
                W_RESTORE => self.unsnap(wid),
                W_MINIMIZE => self.minimize(wid),
                W_MAXIMIZE => self.toggle_maximize(wid),
                _ => self.request_close(wid),
            },
            PopupOwner::Desktop => match id {
                D_NEW_FOLDER | D_NEW_FILE => {
                    let (name, r) = if id == D_NEW_FOLDER {
                        let n = apps::unique_name("D:\\Desktop", "New folder", "");
                        let r = sys::create_directory(&apps::join("D:\\Desktop", &n));
                        (n, r)
                    } else {
                        let n = apps::unique_name("D:\\Desktop", "New Text Document", "txt");
                        let r = sys::write_file(&apps::join("D:\\Desktop", &n), b"");
                        (n, r)
                    };
                    match r {
                        Ok(()) => {
                            self.apply(0, vec![Action::FilesChanged]);
                            self.selected_icon = self.icons.iter().position(|i| i.label == name);
                        }
                        Err(e) => self.notify(format!("Cannot create {name}: {}", apps::error_text(e))),
                    }
                }
                D_REFRESH => self.load_icons(),
                D_TERMINAL => self.launch(AppKind::Terminal, Some("D:\\Desktop".into())),
                _ => self.launch(AppKind::Settings, None),
            },
            PopupOwner::DesktopIcon(i) => {
                let Some(ic) = self.icons.get(i).cloned() else { return };
                match (id, &ic.target) {
                    (D_OPEN, _) => self.open_icon(i),
                    (D_LOCATION, DeskTarget::Path(p, _)) => {
                        self.launch(AppKind::Explorer, Some(apps::parent(p)));
                    }
                    (D_DELETE, DeskTarget::Path(p, _)) => {
                        // Folders must be empty (File Explorer deletes whole trees).
                        match sys::delete_file(p) {
                            Ok(()) => self.apply(0, vec![Action::FilesChanged]),
                            Err(e) => self.notify(format!("Cannot delete {}: {}", ic.label, apps::error_text(e))),
                        }
                    }
                    _ => {}
                }
            }
            PopupOwner::Taskbar(kind) => match id {
                T_NEW => self.launch_new(kind),
                _ => {
                    let ids: Vec<u32> = self.windows.iter().filter(|w| w.app.kind() == kind).map(|w| w.id).collect();
                    for wid in ids {
                        self.request_close(wid);
                    }
                }
            },
        }
    }

    /// Opens another window even for single-instance apps' taskbar menu.
    fn launch_new(&mut self, kind: AppKind) {
        self.launch(kind, None);
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
        let mut items = vec![(Rect::new(x0, y, 44, 40), TaskItem::Start)];
        for (i, k) in kinds.into_iter().enumerate() {
            items.push((Rect::new(x0 + (i as i32 + 1) * 48, y, 44, 40), TaskItem::App(k)));
        }
        items
    }

    fn taskbar_rect(&self) -> Rect {
        Rect::new(0, self.screen.h - TASKBAR_H, self.screen.w, TASKBAR_H)
    }

    fn damage_taskbar(&mut self) {
        let r = self.taskbar_rect();
        self.damage(Rect::new(r.x, r.y - 44, r.w, r.h + 44));
    }

    fn clock_rect(&self) -> Rect {
        Rect::new(self.screen.w - 126, self.screen.h - TASKBAR_H + 4, 110, 40)
    }

    fn show_desktop_rect(&self) -> Rect {
        Rect::new(self.screen.w - 10, self.screen.h - TASKBAR_H, 10, TASKBAR_H)
    }

    fn start_rect(&self) -> Rect {
        let x = if self.theme.taskbar_left { 12 } else { (self.screen.w - START_W) / 2 };
        Rect::new(x, self.screen.h - TASKBAR_H - 12 - START_H, START_W, START_H)
    }

    fn search_hits(&self) -> Vec<Hit> {
        let q = self.search.to_lowercase();
        let mut hits: Vec<Hit> = ALL_APPS
            .iter()
            .copied()
            .filter(|k| apps::name_of(*k).to_lowercase().contains(&q) || apps::keywords_of(*k).contains(&q))
            .map(Hit::App)
            .collect();
        for dir in ["D:\\Desktop", "D:\\Documents", "D:\\Pictures", "D:\\", "C:\\"] {
            if let Ok(list) = sys::list_directory(dir) {
                for e in list {
                    if e.name.to_lowercase().contains(&q) && hits.len() < 9 {
                        hits.push(Hit::File(apps::join(dir, &e.name), e.is_dir));
                    }
                }
            }
        }
        hits.truncate(9);
        hits
    }

    fn start_app_rect(&self, i: usize) -> Rect {
        let s = self.start_rect();
        Rect::new(s.x + 32 + (i as i32 % 6) * 96, s.y + 108 + (i as i32 / 6) * 92, 92, 86)
    }

    fn start_result_rect(&self, i: usize) -> Rect {
        let s = self.start_rect();
        Rect::new(s.x + 24, s.y + 108 + i as i32 * 48, s.w - 48, 44)
    }

    fn recommended_rect(&self, i: usize) -> Rect {
        let s = self.start_rect();
        Rect::new(s.x + 24 + (i as i32 % 2) * 300, s.y + 340 + (i as i32 / 2) * 58, 292, 52)
    }

    fn power_rect(&self) -> Rect {
        let s = self.start_rect();
        Rect::new(s.right() - 60, s.bottom() - 56, 40, 40)
    }

    fn power_item_rect(&self, i: usize) -> Rect {
        let p = self.power_rect();
        Rect::new(p.right() - 184, p.y - 140 + i as i32 * 44, 184, 40)
    }

    fn calendar_rect(&self) -> Rect {
        Rect::new(self.screen.w - 352, self.screen.h - TASKBAR_H - 12 - 392, 340, 392)
    }

    fn open_start(&mut self) {
        self.close_popup();
        self.calendar = None;
        self.start_open = true;
        self.power_open = false;
        self.search.clear();
        self.recommended.clear();
        for dir in ["D:\\Desktop", "D:\\Documents", "D:\\Pictures", "C:\\"] {
            if let Ok(list) = sys::list_directory(dir) {
                for e in list.into_iter().filter(|e| !e.is_dir) {
                    if self.recommended.len() < 6 {
                        self.recommended.push((apps::join(dir, &e.name), false));
                    }
                }
            }
        }
        let r = self.start_rect();
        self.damage(r.grow(30));
        self.damage_taskbar();
    }

    fn close_start(&mut self) {
        if self.start_open {
            self.start_open = false;
            self.power_open = false;
            let r = self.start_rect();
            self.damage(r.grow(30));
            self.damage_taskbar();
        }
    }

    fn toggle_calendar(&mut self) {
        if self.calendar.is_some() {
            self.calendar = None;
        } else {
            self.close_start();
            let t = sys::query_time();
            self.calendar = Some((t.year as i32, t.month as u32));
        }
        let r = self.calendar_rect();
        self.damage(r.grow(30));
    }

    fn lock(&mut self) {
        self.close_start();
        self.close_popup();
        self.calendar = None;
        self.locked = true;
        self.damage_all();
    }

    // ---- hit testing -----------------------------------------------------

    fn edges_at(&self, x: i32, y: i32) -> Option<(u32, u8)> {
        for w in self.windows.iter().rev().filter(|w| !w.minimized) {
            let r = w.rect;
            if w.maximized {
                if r.contains(x, y) {
                    return None;
                }
                continue;
            }
            let outer = r.grow(GRIP);
            if !outer.contains(x, y) {
                continue;
            }
            let mut e = 0;
            if x < r.x + 2 {
                e |= LEFT;
            }
            if x >= r.right() - 2 {
                e |= RIGHT;
            }
            if y < r.y + 2 {
                e |= TOP;
            }
            if y >= r.bottom() - 2 {
                e |= BOTTOM;
            }
            return if e == 0 { None } else { Some((w.id, e)) };
        }
        None
    }

    fn hover_at(&self, x: i32, y: i32) -> Hover {
        if self.locked {
            return Hover::None;
        }
        if let Some(i) = self.popup_item_at(x, y) {
            return Hover::Popup(i);
        }
        if self.popup.as_ref().is_some_and(|p| p.rect.contains(x, y)) {
            return Hover::None;
        }
        if self.calendar.is_some() {
            let c = self.calendar_rect();
            if c.contains(x, y) {
                for d in [-1, 1] {
                    if self.calendar_nav_rect(d).contains(x, y) {
                        return Hover::CalendarNav(d);
                    }
                }
                return Hover::None;
            }
        }
        if self.start_open {
            if self.power_open {
                for i in 0..3 {
                    if self.power_item_rect(i).contains(x, y) {
                        return Hover::PowerItem(i);
                    }
                }
            }
            if self.start_rect().contains(x, y) {
                if self.search.is_empty() {
                    for i in 0..ALL_APPS.len() {
                        if self.start_app_rect(i).contains(x, y) {
                            return Hover::StartApp(i);
                        }
                    }
                    for i in 0..self.recommended.len() {
                        if self.recommended_rect(i).contains(x, y) {
                            return Hover::Recommended(i);
                        }
                    }
                } else {
                    for i in 0..9 {
                        if self.start_result_rect(i).contains(x, y) {
                            return Hover::StartResult(i);
                        }
                    }
                }
                if self.power_rect().contains(x, y) {
                    return Hover::Power;
                }
                return Hover::None;
            }
        }
        if y >= self.screen.h - TASKBAR_H {
            if self.clock_rect().contains(x, y) {
                return Hover::Clock;
            }
            if self.show_desktop_rect().contains(x, y) {
                return Hover::ShowDesktop;
            }
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
        (0..self.icons.len()).find(|&i| self.icon_rect(i).contains(x, y)).map_or(Hover::None, Hover::Icon)
    }

    fn calendar_nav_rect(&self, d: i32) -> Rect {
        let c = self.calendar_rect();
        Rect::new(if d < 0 { c.right() - 84 } else { c.right() - 44 }, c.y + 84, 32, 32)
    }

    fn hover_rect(&self, h: Hover) -> Rect {
        match h {
            Hover::None => Rect::default(),
            Hover::Taskbar(_) | Hover::Clock | Hover::ShowDesktop => {
                let t = self.taskbar_rect();
                Rect::new(t.x, t.y - 44, t.w, t.h + 44)
            }
            Hover::Caption(id, _) => self.index_of(id).map_or(Rect::default(), |i| {
                let r = self.windows[i].rect;
                Rect::new(r.x, r.y, r.w, TITLE_H)
            }),
            Hover::StartApp(_) | Hover::StartResult(_) | Hover::Recommended(_) | Hover::Power | Hover::PowerItem(_) => {
                self.start_rect().grow(4)
            }
            Hover::Icon(i) => self.icon_rect(i),
            Hover::Popup(_) => self.popup.as_ref().map_or(Rect::default(), |p| p.rect),
            Hover::CalendarNav(_) => self.calendar_rect(),
        }
    }

    fn set_hover(&mut self, h: Hover) {
        if h != self.hover {
            let (a, b) = (self.hover_rect(self.hover), self.hover_rect(h));
            for hv in [self.hover, h] {
                if let Hover::Caption(id, _) = hv {
                    if let Some(i) = self.index_of(id) {
                        self.windows[i].title_dirty = true;
                    }
                }
            }
            if let Some(p) = &mut self.popup {
                p.hover = if let Hover::Popup(i) = h { Some(i) } else { None };
            }
            self.hover = h;
            self.tooltip_hidden = false;
            self.damage(a);
            self.damage(b);
        }
    }

    // ---- input -----------------------------------------------------------

    fn ctx_call<R>(&mut self, i: usize, f: impl FnOnce(&mut dyn App, &mut Ctx) -> R) -> R {
        let mut cx =
            Ctx { fonts: &mut self.fonts, mono: &self.mono, theme: self.theme, clipboard: &mut self.clipboard };
        f(self.windows[i].app.as_mut(), &mut cx)
    }

    fn mouse_down(&mut self) {
        let (x, y) = self.mouse;
        let t = now_ms();
        let double =
            t - self.last_click.0 < 450 && (x - self.last_click.1).abs() < 6 && (y - self.last_click.2).abs() < 6;
        self.last_click = if double { (0, -100, -100) } else { (t, x, y) };

        if self.locked {
            self.locked = false;
            self.damage_all();
            return;
        }
        // Toasts dismiss on click.
        if let Some(k) = self.toast_index_at(x, y) {
            self.toasts.remove(k);
            let r = self.toast_area();
            self.damage(r);
            return;
        }
        if self.popup.is_some() {
            if let Some(i) = self.popup_item_at(x, y) {
                self.choose_popup(i);
                return;
            }
            let inside = self.popup.as_ref().is_some_and(|p| p.rect.contains(x, y));
            self.close_popup();
            if inside {
                return;
            }
        }
        if let Some((year, month)) = self.calendar {
            let c = self.calendar_rect();
            if c.contains(x, y) {
                for d in [-1, 1] {
                    if self.calendar_nav_rect(d).contains(x, y) {
                        let m = month as i32 - 1 + d;
                        self.calendar = Some((year + m.div_euclid(12), (m.rem_euclid(12) + 1) as u32));
                        self.damage(c);
                    }
                }
                return;
            }
            if !self.clock_rect().contains(x, y) {
                self.calendar = None;
                self.damage(c.grow(30));
            }
        }
        if self.start_open {
            match self.hover_at(x, y) {
                Hover::StartApp(i) => {
                    self.close_start();
                    self.launch(ALL_APPS[i], None);
                    return;
                }
                Hover::StartResult(i) => {
                    let hits = self.search_hits();
                    self.close_start();
                    if let Some(h) = hits.get(i).cloned() {
                        self.open_hit(h);
                    }
                    return;
                }
                Hover::Recommended(i) => {
                    let (path, is_dir) = self.recommended[i].clone();
                    self.close_start();
                    self.open_path(&path, is_dir);
                    return;
                }
                Hover::Power => {
                    self.power_open = !self.power_open;
                    let r = self.start_rect().grow(30);
                    self.damage(r);
                    return;
                }
                Hover::PowerItem(i) => {
                    match i {
                        0 => self.lock(),
                        _ => {
                            let action = if i == 2 { power::SHUT_DOWN } else { power::RESTART };
                            let status = sys::power(action);
                            self.notify(format!("Power action failed (status {:#x})", status as u32));
                        }
                    }
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
            self.tooltip_hidden = true;
            self.damage_taskbar();
            if self.clock_rect().contains(x, y) {
                self.toggle_calendar();
                return;
            }
            if self.show_desktop_rect().contains(x, y) {
                self.show_desktop();
                return;
            }
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

        if let Some((id, edges)) = self.edges_at(x, y) {
            if self.focused != Some(id) {
                self.focus(id);
            }
            if let Some(i) = self.index_of(id) {
                let start = self.windows[i].rect;
                self.windows[i].snapped = false;
                self.drag = Some(Drag::Resize { id, edges, start, mx: x, my: y });
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
                        0 => self.request_close(id),
                        1 => self.toggle_maximize(id),
                        _ => self.minimize(id),
                    }
                } else if double {
                    self.toggle_maximize(id);
                } else {
                    self.drag = Some(Drag::Move { id, ox: x - rect.x, oy: y - rect.y });
                }
                return;
            }
            let Some(i) = self.index_of(id) else { return };
            let actions = self.ctx_call(i, |app, cx| app.click(x - rect.x, y - rect.y - TITLE_H, double, cx));
            self.drag = Some(Drag::App { id });
            self.apply(id, actions);
            return;
        }

        // Desktop background and icons.
        let old = self.selected_icon;
        self.selected_icon = (0..self.icons.len()).find(|&i| self.icon_rect(i).contains(x, y));
        for i in [old, self.selected_icon].into_iter().flatten() {
            let r = self.icon_rect(i);
            self.damage(r);
        }
        if double {
            if let Some(i) = self.selected_icon {
                self.open_icon(i);
            }
        }
    }

    fn right_down(&mut self) {
        let (x, y) = self.mouse;
        if self.locked {
            return;
        }
        self.close_popup();
        if self.start_open || self.calendar.is_some() {
            self.close_start();
            if self.calendar.take().is_some() {
                let r = self.calendar_rect();
                self.damage(r.grow(30));
            }
            return;
        }
        if y >= self.screen.h - TASKBAR_H {
            if let Some((r, TaskItem::App(kind))) = self.taskbar_items().into_iter().find(|(r, _)| r.contains(x, y)) {
                let running = self.windows.iter().any(|w| w.app.kind() == kind);
                let items = vec![
                    MenuItem::new(apps::name_of(kind), T_NEW),
                    MenuItem::separator(),
                    MenuItem::when("Close window", T_CLOSE_ALL, running),
                ];
                self.open_popup(PopupOwner::Taskbar(kind), items, r.x, r.y - 4 - 3 * 32);
            }
            return;
        }
        let hit = self.windows.iter().rev().find(|w| !w.minimized && w.rect.contains(x, y)).map(|w| (w.id, w.rect));
        if let Some((id, rect)) = hit {
            if self.focused != Some(id) {
                self.focus(id);
            }
            let Some(i) = self.index_of(id) else { return };
            if y < rect.y + TITLE_H {
                let w = &self.windows[i];
                let items = vec![
                    MenuItem::when("Restore", W_RESTORE, w.maximized || w.snapped),
                    MenuItem::new("Minimize", W_MINIMIZE),
                    MenuItem::when("Maximize", W_MAXIMIZE, !w.maximized),
                    MenuItem::separator(),
                    MenuItem::new("Close            Alt+F4", W_CLOSE),
                ];
                self.open_popup(PopupOwner::Title(id), items, x, y);
                return;
            }
            let actions = self.ctx_call(i, |app, cx| app.right_click(x - rect.x, y - rect.y - TITLE_H, cx));
            self.apply(id, actions);
            return;
        }
        if let Some(i) = (0..self.icons.len()).find(|&i| self.icon_rect(i).contains(x, y)) {
            self.selected_icon = Some(i);
            let r = self.icon_rect(i);
            self.damage(r);
            let is_file = matches!(self.icons[i].target, DeskTarget::Path(ref p, _) if p.starts_with("D:\\Desktop"));
            let mut items = vec![MenuItem::new("Open", D_OPEN)];
            if matches!(self.icons[i].target, DeskTarget::Path(..)) {
                items.push(MenuItem::new("Open file location", D_LOCATION));
            }
            if is_file {
                items.push(MenuItem::separator());
                items.push(MenuItem::new("Delete", D_DELETE));
            }
            self.open_popup(PopupOwner::DesktopIcon(i), items, x, y);
            return;
        }
        let w = self.data_drive;
        let items = vec![
            MenuItem::when("New folder", D_NEW_FOLDER, w),
            MenuItem::when("New text document", D_NEW_FILE, w),
            MenuItem::separator(),
            MenuItem::new("Refresh", D_REFRESH),
            MenuItem::new("Open in Terminal", D_TERMINAL),
            MenuItem::separator(),
            MenuItem::new("Personalise", D_PERSONALISE),
        ];
        self.open_popup(PopupOwner::Desktop, items, x, y);
    }

    fn open_hit(&mut self, h: Hit) {
        match h {
            Hit::App(k) => self.launch(k, None),
            Hit::File(p, is_dir) => self.open_path(&p, is_dir),
        }
    }

    fn show_desktop(&mut self) {
        let visible: Vec<u32> = self.windows.iter().filter(|w| !w.minimized).map(|w| w.id).collect();
        if visible.is_empty() {
            let ids: Vec<u32> = self.windows.iter().map(|w| w.id).collect();
            for id in ids {
                self.restore_and_focus(id);
            }
        } else {
            for id in visible {
                self.minimize(id);
            }
        }
    }

    fn mouse_moved(&mut self) {
        let (x, y) = self.mouse;
        match self.drag {
            Some(Drag::Move { id, ox, oy }) => {
                let Some(i) = self.index_of(id) else { return };
                let wa = self.work_area();
                if self.windows[i].maximized || self.windows[i].snapped {
                    // Dragging a maximised or snapped window restores it
                    // under the pointer.
                    let restore = self.windows[i].restore;
                    self.windows[i].maximized = false;
                    self.windows[i].snapped = false;
                    let ox = (ox * restore.w / self.windows[i].rect.w.max(1)).min(restore.w - 140);
                    self.drag = Some(Drag::Move { id, ox, oy });
                    self.set_rect(i, Rect::new(x - ox, y - oy, restore.w, restore.h));
                    return;
                }
                let r = self.windows[i].rect;
                let nx = (x - ox).clamp(-r.w + 80, wa.w - 80);
                let ny = (y - oy).clamp(0, wa.h - TITLE_H);
                self.set_rect(i, Rect::new(nx, ny, r.w, r.h));
                let half = wa.w / 2;
                let preview = if x <= 1 {
                    Some(Rect::new(0, 0, half, wa.h))
                } else if x >= self.screen.w - 2 {
                    Some(Rect::new(half, 0, wa.w - half, wa.h))
                } else if y <= 1 {
                    Some(wa)
                } else {
                    None
                };
                if preview != self.snap_preview {
                    if let Some(p) = self.snap_preview {
                        self.damage(p.grow(4));
                    }
                    if let Some(p) = preview {
                        self.damage(p.grow(4));
                    }
                    self.snap_preview = preview;
                }
                return;
            }
            Some(Drag::Resize { id, edges, start, mx, my }) => {
                let Some(i) = self.index_of(id) else { return };
                let (min_w, min_h) = self.windows[i].app.min_size();
                let (min_w, min_h) = (min_w.min(self.screen.w), (min_h + TITLE_H).min(self.screen.h));
                let (dx, dy) = (x - mx, y - my);
                let mut r = start;
                if edges & LEFT != 0 {
                    let nx = (start.x + dx).min(start.right() - min_w);
                    r.w = start.right() - nx;
                    r.x = nx;
                }
                if edges & RIGHT != 0 {
                    r.w = (start.w + dx).max(min_w);
                }
                if edges & TOP != 0 {
                    let ny = (start.y + dy).min(start.bottom() - min_h).max(0);
                    r.h = start.bottom() - ny;
                    r.y = ny;
                }
                if edges & BOTTOM != 0 {
                    r.h = (start.h + dy).max(min_h);
                }
                if r != self.windows[i].rect {
                    self.set_rect(i, r);
                }
                return;
            }
            Some(Drag::App { id }) => {
                if let Some(i) = self.index_of(id) {
                    let r = self.windows[i].rect;
                    if self.ctx_call(i, |app, cx| app.drag(x - r.x, y - r.y - TITLE_H, cx)) {
                        self.mark(i, false, true);
                    }
                }
                return;
            }
            None => {}
        }
        let h = self.hover_at(x, y);
        self.set_hover(h);
        let kind = match self.edges_at(x, y).map(|e| e.1) {
            Some(e) if e == LEFT || e == RIGHT => CursorKind::Horizontal,
            Some(e) if e == TOP || e == BOTTOM => CursorKind::Vertical,
            Some(e) if e == LEFT | TOP || e == RIGHT | BOTTOM => CursorKind::Diagonal,
            Some(_) => CursorKind::AntiDiagonal,
            None => CursorKind::Arrow,
        };
        self.cursor = if self.locked || self.start_open || self.popup.is_some() { CursorKind::Arrow } else { kind };
        // Hover feedback inside windows (where apps support it).
        if let Some(i) = self.windows.iter().rposition(|w| !w.minimized && w.rect.contains(x, y)) {
            let r = self.windows[i].rect;
            if y > r.y + TITLE_H && self.windows[i].app.hover(x - r.x, y - r.y - TITLE_H) {
                self.mark(i, false, true);
            }
        }
    }

    fn mouse_up(&mut self) {
        let (x, y) = self.mouse;
        match self.drag.take() {
            Some(Drag::Move { id, .. }) => {
                if let Some(p) = self.snap_preview.take() {
                    self.damage(p.grow(4));
                    let wa = self.work_area();
                    if p == wa {
                        if let Some(i) = self.index_of(id) {
                            if !self.windows[i].maximized {
                                self.toggle_maximize(id);
                            }
                        }
                    } else {
                        self.snap(id, if p.x == 0 { -1 } else { 1 });
                    }
                }
            }
            Some(Drag::App { id }) => {
                if let Some(i) = self.index_of(id) {
                    let r = self.windows[i].rect;
                    let actions = self.ctx_call(i, |app, cx| app.release(x - r.x, y - r.y - TITLE_H, cx));
                    self.apply(id, actions);
                }
            }
            _ => {}
        }
    }

    fn key(&mut self, e: &InputEvent) {
        let pressed = e.flags & input::PRESSED != 0;
        if e.key == key::SUPER {
            if pressed {
                self.super_down = true;
                self.super_used = false;
            } else {
                if self.super_down && !self.super_used && !self.locked {
                    if self.start_open { self.close_start() } else { self.open_start() }
                }
                self.super_down = false;
            }
            return;
        }
        if !pressed || e.key == key::MODIFIER {
            return;
        }
        if self.locked {
            self.locked = false;
            self.damage_all();
            return;
        }
        let alt = e.flags & input::ALT != 0;
        let ctrl = e.flags & input::CTRL != 0;
        let shift = e.flags & input::SHIFT != 0;
        if self.super_down {
            self.super_used = true;
            let c = char::from_u32(e.ch).unwrap_or(' ').to_ascii_lowercase();
            match (e.key, c) {
                (key::CHAR, 'd') => self.show_desktop(),
                (key::CHAR, 'e') => self.launch(AppKind::Explorer, None),
                (key::CHAR, 'l') => self.lock(),
                (key::CHAR, 'r') => self.launch(AppKind::Terminal, None),
                (key::CHAR, 'i') => self.launch(AppKind::Settings, None),
                (key::LEFT, _) | (key::RIGHT, _) => {
                    if let Some(id) = self.focused {
                        let side = if e.key == key::LEFT { -1 } else { 1 };
                        let snapped_same = self
                            .index_of(id)
                            .is_some_and(|i| self.windows[i].snapped && (self.windows[i].rect.x == 0) == (side < 0));
                        if snapped_same { self.unsnap(id) } else { self.snap(id, side) }
                    }
                }
                (key::UP, _) => {
                    if let Some(id) =
                        self.focused.filter(|id| self.index_of(*id).is_some_and(|i| !self.windows[i].maximized))
                    {
                        self.toggle_maximize(id);
                    }
                }
                (key::DOWN, _) => {
                    if let Some(id) = self.focused {
                        let state = self.index_of(id).map(|i| (self.windows[i].maximized, self.windows[i].snapped));
                        match state {
                            Some((true, _)) | Some((_, true)) => self.unsnap(id),
                            Some(_) => self.minimize(id),
                            None => {}
                        }
                    }
                }
                _ => {}
            }
            return;
        }
        if ctrl && shift && e.key == key::ESCAPE {
            self.launch(AppKind::TaskManager, None);
            return;
        }
        if let Some(p) = &mut self.popup {
            let n = p.items.len();
            let usable = |k: usize, p: &Popup| p.items[k].id != 0 && p.items[k].enabled;
            match e.key {
                key::ESCAPE => self.close_popup(),
                key::DOWN | key::UP => {
                    let mut k = p.hover.unwrap_or(if e.key == key::DOWN { n - 1 } else { 0 });
                    for _ in 0..n {
                        k = if e.key == key::DOWN { (k + 1) % n } else { (k + n - 1) % n };
                        if usable(k, p) {
                            break;
                        }
                    }
                    p.hover = Some(k);
                    let r = p.rect;
                    self.damage(r);
                }
                key::ENTER => {
                    if let Some(k) = p.hover.filter(|&k| usable(k, p)) {
                        self.choose_popup(k);
                    }
                }
                _ => {}
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
                    if self.search.is_empty() {
                        return;
                    }
                    let first = self.search_hits().into_iter().next();
                    self.close_start();
                    if let Some(h) = first {
                        self.open_hit(h);
                    }
                }
                key::CHAR if !ctrl => {
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
                self.request_close(id);
            }
            return;
        }
        if alt && e.key == key::TAB {
            if let Some(next) = self.windows.iter().find(|w| Some(w.id) != self.focused).map(|w| w.id) {
                self.restore_and_focus(next);
            }
            return;
        }
        if e.key == key::ESCAPE && self.calendar.take().is_some() {
            let r = self.calendar_rect();
            self.damage(r.grow(30));
            return;
        }
        if let Some(id) = self.focused {
            if let Some(i) = self.index_of(id) {
                let actions = self.ctx_call(i, |app, cx| app.key(e, cx));
                self.apply(id, actions);
            }
        } else if e.key == key::ENTER {
            if let Some(i) = self.selected_icon {
                self.open_icon(i);
            }
        }
    }

    fn wheel(&mut self, notches: i32) {
        let (x, y) = self.mouse;
        if let Some(i) = self.windows.iter().rposition(|w| !w.minimized && w.rect.contains(x, y)) {
            if self.windows[i].app.wheel(notches) {
                self.mark(i, false, true);
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
                let changed = e.buttons ^ self.buttons;
                self.buttons = e.buttons;
                if changed & 1 != 0 {
                    if e.buttons & 1 != 0 { self.mouse_down() } else { self.mouse_up() }
                }
                if changed & 2 != 0 && e.buttons & 2 != 0 {
                    self.right_down();
                }
            }
            input::KEY => self.key(e),
            _ => {}
        }
    }

    fn toast_rect(&self, k: usize) -> Rect {
        let n = self.toasts.len();
        Rect::new(self.screen.w - 388, self.screen.h - TASKBAR_H - 12 - (n - k) as i32 * 84 + 8, 376, 76)
    }

    fn toast_index_at(&self, x: i32, y: i32) -> Option<usize> {
        (0..self.toasts.len()).find(|&k| self.toast_rect(k).contains(x, y))
    }

    /// Clock, toasts and app timers. Returns the next time anything is due.
    fn tick(&mut self, now: u64) -> u64 {
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
            self.damage_taskbar();
            if self.locked {
                self.damage_all();
            }
        }
        let before = self.toasts.len();
        self.toasts.retain(|t| t.until > now);
        if self.toasts.len() != before {
            let r = self.toast_area();
            self.damage(r);
        }
        let mut next = now + 1000;
        for i in 0..self.windows.len() {
            if self.windows[i].next_tick <= now {
                let changed = self.windows[i].app.tick();
                self.windows[i].next_tick = now + self.windows[i].app.interval();
                if changed && !self.windows[i].minimized {
                    self.mark(i, true, true);
                }
                let pending = self.windows[i].app.pending();
                if !pending.is_empty() {
                    let id = self.windows[i].id;
                    self.apply(id, pending);
                    return now + 10;
                }
            }
            next = next.min(self.windows[i].next_tick);
        }
        for t in &self.toasts {
            next = next.min(t.until);
        }
        next
    }

    // ---- rendering -------------------------------------------------------

    /// A canvas covering `r` filled with the blurred wallpaper under it and
    /// a tint: the "Mica" material of the taskbar, menus and flyouts.
    fn mica(&self, r: Rect, tint: Color) -> Canvas {
        let mut c = Canvas::new(r.w.max(1), r.h.max(1), 0);
        c.blit(&self.blur, Rect::new(r.x, r.y, r.w, r.h), 0, 0);
        let b = c.bounds();
        c.fill(b, tint);
        c
    }

    fn render_title(&mut self, i: usize) {
        let focused = self.focused == Some(self.windows[i].id);
        let hover = self.hover;
        let t = self.theme;
        let r = self.windows[i].rect;
        let mut cv = if focused {
            self.mica(Rect::new(r.x, r.y, r.w, TITLE_H), t.title_tint(true))
        } else {
            Canvas::new(r.w.max(1), TITLE_H, t.title_tint(false))
        };
        let w = &mut self.windows[i];
        ui::icon(&mut cv, w.app.icon(), 10, 8, 16, &t);
        let title = w.app.title();
        let text_color = if focused { t.text() } else { t.text_dim() };
        let room = cv.w - 36 - 3 * CAPTION_W - 8;
        self.fonts.draw_fit(&mut cv, 36, 8, &title, room, 12, Weight::Regular, text_color);
        for b in 0..3u8 {
            let br = Rect::new(cv.w - (b as i32 + 1) * CAPTION_W, 0, CAPTION_W, TITLE_H);
            let hovered = hover == Hover::Caption(w.id, b);
            if hovered {
                cv.fill(br, if b == 0 { rgb(0xC4, 0x2B, 0x1C) } else { t.hover() });
            }
            let c = if hovered && b == 0 { rgb(255, 255, 255) } else { text_color };
            let (cx, cy) = (br.x as f32 + 23.0, 16.0);
            match b {
                0 => {
                    cv.line(cx - 5.0, cy - 5.0, cx + 5.0, cy + 5.0, 1.2, c);
                    cv.line(cx + 5.0, cy - 5.0, cx - 5.0, cy + 5.0, 1.2, c);
                }
                1 => {
                    if w.maximized {
                        cv.stroke(Rect::new(br.x + 18, 13, 9, 9), c);
                        cv.fill(Rect::new(br.x + 20, 11, 9, 1), c);
                        cv.fill(Rect::new(br.x + 28, 11, 1, 9), c);
                    } else {
                        cv.stroke(Rect::new(br.x + 18, 11, 10, 10), c);
                    }
                }
                _ => cv.fill(Rect::new(br.x + 18, 16, 10, 1), c),
            }
        }
        let border = if focused { with_alpha(t.accent, 140) } else { t.border() };
        if !w.maximized {
            cv.fill(Rect::new(0, 0, cv.w, 1), border);
            cv.fill(Rect::new(0, 0, 1, TITLE_H), border);
            cv.fill(Rect::new(cv.w - 1, 0, 1, TITLE_H), border);
            cv.round_corners_sides(RADIUS, true, false);
        }
        w.title = cv;
        w.title_dirty = false;
    }

    fn render_content(&mut self, i: usize) {
        let focused = self.focused == Some(self.windows[i].id);
        let theme = self.theme;
        let w = &mut self.windows[i];
        let (cw, ch) = (w.rect.w.max(1), (w.rect.h - TITLE_H).max(1));
        if w.content.w != cw || w.content.h != ch {
            w.content = Canvas::new(cw, ch, 0);
        }
        let cv = &mut w.content;
        cv.reset_clip();
        let r = cv.bounds();
        let mut ctx = Ctx { fonts: &mut self.fonts, mono: &self.mono, theme, clipboard: &mut self.clipboard };
        w.app.draw(cv, r, &mut ctx);
        cv.reset_clip();
        if !w.maximized {
            let border = if focused { with_alpha(theme.accent, 140) } else { theme.border() };
            cv.fill(Rect::new(0, 0, 1, ch), border);
            cv.fill(Rect::new(cw - 1, 0, 1, ch), border);
            cv.fill(Rect::new(0, ch - 1, cw, 1), border);
            cv.round_corners_sides(RADIUS, false, true);
        }
        w.content_dirty = false;
    }

    fn compose(&mut self, area: Rect) {
        let b = &mut self.back;
        b.set_clip(area);
        b.blit(&self.wallpaper, area, area.x, area.y);
        // Desktop icons.
        for i in 0..self.icons.len() {
            let r = self.icon_rect(i);
            if r.intersect(&area).is_none() {
                continue;
            }
            let b = &mut self.back;
            if self.selected_icon == Some(i) {
                b.fill_rounded(r, 4, with_alpha(rgb(255, 255, 255), 50));
            } else if self.hover == Hover::Icon(i) {
                b.fill_rounded(r, 4, with_alpha(rgb(255, 255, 255), 28));
            }
            let ic = &self.icons[i];
            ui::icon(b, ic.icon, r.x + 20, r.y + 6, 48, &self.theme);
            // Up to two lines of label.
            let label = ic.label.clone();
            let f = &mut self.fonts;
            let (first, second) = if f.measure(&label, 12, Weight::Regular) <= r.w - 4 {
                (label.clone(), String::new())
            } else {
                let mut cut = label.len();
                while cut > 0
                    && (f.measure(&label[..cut], 12, Weight::Regular) > r.w - 4 || !label.is_char_boundary(cut))
                {
                    cut -= 1;
                }
                let split = label[..cut].rfind(' ').filter(|&s| s > 0).unwrap_or(cut);
                (label[..split].to_string(), label[split..].trim().to_string())
            };
            for (k, line) in [first, second].iter().enumerate() {
                if line.is_empty() {
                    continue;
                }
                let lw = f.measure(line, 12, Weight::Regular).min(r.w - 4);
                let lx = r.x + (r.w - lw) / 2;
                let ly = r.y + 58 + k as i32 * 16;
                f.draw_fit(b, lx + 1, ly + 1, line, r.w - 4, 12, Weight::Regular, with_alpha(0, 170));
                f.draw_fit(b, lx, ly, line, r.w - 4, 12, Weight::Regular, rgb(255, 255, 255));
            }
        }
        // Windows, bottom to top.
        for w in self.windows.iter().filter(|w| !w.minimized) {
            if w.rect.grow(24).intersect(&area).is_none() {
                continue;
            }
            let b = &mut self.back;
            if !w.maximized {
                b.shadow(w.rect, 6, RADIUS, 22, if self.theme.dark { 120 } else { 70 });
            }
            b.blit(&w.title, w.title.bounds(), w.rect.x, w.rect.y);
            b.blit(&w.content, w.content.bounds(), w.rect.x, w.rect.y + TITLE_H);
        }
        if let Some(p) = self.snap_preview {
            let b = &mut self.back;
            b.fill_rounded(p.inset(8), 8, with_alpha(self.theme.accent, 70));
            b.fill_rounded(p.inset(8), 8, with_alpha(rgb(255, 255, 255), 30));
        }
        self.draw_taskbar(area);
        if self.start_open {
            self.draw_start(area);
        }
        if self.calendar.is_some() {
            self.draw_calendar(area);
        }
        self.draw_popup(area);
        self.draw_toasts(area);
        if self.locked {
            self.draw_lock(area);
        }
        self.back.reset_clip();
    }

    fn draw_taskbar(&mut self, area: Rect) {
        let t = self.theme;
        let bar = self.taskbar_rect();
        if Rect::new(bar.x, bar.y - 44, bar.w, bar.h + 44).intersect(&area).is_none() {
            return;
        }
        let items = self.taskbar_items();
        let mut cv = self.mica(bar, t.taskbar_tint());
        cv.fill(Rect::new(0, 0, bar.w, 1), with_alpha(rgb(255, 255, 255), if t.dark { 24 } else { 140 }));
        let mut tooltip = None;
        for (i, (r, item)) in items.into_iter().enumerate() {
            let r = r.offset(0, -bar.y);
            let hovered = self.hover == Hover::Taskbar(i);
            let (icon, running, active, name) = match item {
                TaskItem::Start => (Icon::Start, false, self.start_open, "Start"),
                TaskItem::App(k) => {
                    let running = self.windows.iter().any(|w| w.app.kind() == k);
                    let active = self
                        .focused
                        .and_then(|f| self.windows.iter().find(|w| w.id == f))
                        .is_some_and(|w| w.app.kind() == k && !w.minimized);
                    (apps::icon_for(k), running, active, apps::name_of(k))
                }
            };
            if hovered || active {
                cv.fill_rounded(r, 5, if active { t.pressed() } else { t.hover() });
            }
            if hovered && !self.tooltip_hidden && !self.start_open && self.popup.is_none() {
                tooltip = Some((r.offset(0, bar.y), name));
            }
            ui::icon(&mut cv, icon, r.x + 10, r.y + 8, 24, &t);
            if running {
                let iw = if active { 16 } else { 6 };
                cv.fill_rounded(
                    Rect::new(r.x + (r.w - iw) / 2, r.bottom() - 4, iw, 3),
                    1,
                    if active { t.accent } else { t.text_dim() },
                );
            }
        }
        // Clock (opens the calendar) and the show-desktop sliver.
        let cr = self.clock_rect().offset(0, -bar.y);
        if self.hover == Hover::Clock || self.calendar.is_some() {
            cv.fill_rounded(cr, 5, if self.calendar.is_some() { t.pressed() } else { t.hover() });
        }
        let (clock, date) = (self.clock.clone(), self.date.clone());
        let f = &mut self.fonts;
        let (w1, w2) = (f.measure(&clock, 12, Weight::Regular), f.measure(&date, 12, Weight::Regular));
        f.draw(&mut cv, cr.right() - 10 - w1, cr.y + 4, &clock, 12, Weight::Regular, t.text());
        f.draw(&mut cv, cr.right() - 10 - w2, cr.y + 21, &date, 12, Weight::Regular, t.text());
        let sd = self.show_desktop_rect().offset(0, -bar.y);
        cv.fill(Rect::new(sd.x, sd.y + 12, 1, sd.h - 24), t.border());
        if self.hover == Hover::ShowDesktop {
            cv.fill(sd, t.hover());
        }
        self.back.blit(&cv, cv.bounds(), bar.x, bar.y);
        if let Some((r, name)) = tooltip {
            let f = &mut self.fonts;
            let tw = f.measure(name, 12, Weight::Regular) + 20;
            let tr = Rect::new((r.x + r.w / 2 - tw / 2).max(4), r.y - 40, tw, 28);
            let b = &mut self.back;
            b.shadow(tr, 2, 4, 8, 60);
            b.rounded_box(tr, 4, t.border(), t.menu());
            f.draw(b, tr.x + 10, tr.y + 6, name, 12, Weight::Regular, t.text());
        }
    }

    fn draw_start(&mut self, area: Rect) {
        let t = self.theme;
        let s = self.start_rect();
        if s.grow(30).intersect(&area).is_none() {
            return;
        }
        let mut cv = self.mica(s, t.menu_tint());
        let hits = if self.search.is_empty() { Vec::new() } else { self.search_hits() };
        let recommended = self.recommended.clone();
        let (hover, search, power_open) = (self.hover, self.search.clone(), self.power_open);
        let local = |r: Rect| r.offset(-s.x, -s.y);
        let result_rects: Vec<Rect> = (0..hits.len()).map(|i| local(self.start_result_rect(i))).collect();
        let f = &mut self.fonts;
        // Search box.
        let sb = Rect::new(32, 28, s.w - 64, 40);
        cv.rounded_box(sb, 20, t.border(), t.surface());
        let (cx, cy) = (sb.x as f32 + 24.0, sb.y as f32 + 19.0);
        cv.ring(cx, cy, 6.0, 1.5, t.text_dim());
        cv.line(cx + 4.5, cy + 4.5, cx + 9.0, cy + 9.0, 1.6, t.text_dim());
        let (text, color) = if search.is_empty() {
            ("Search for apps, settings and documents", t.text_dim())
        } else {
            (search.as_str(), t.text())
        };
        let tw = f.draw(&mut cv, sb.x + 42, sb.y + 11, text, 14, Weight::Regular, color);
        if !search.is_empty() {
            cv.fill(Rect::new(sb.x + 42 + tw + 1, sb.y + 10, 1, 20), t.text());
        }
        if search.is_empty() {
            f.draw(&mut cv, 40, 82, "Pinned", 14, Weight::Semibold, t.text());
            for (i, kind) in ALL_APPS.iter().enumerate() {
                let r = Rect::new(32 + (i as i32 % 6) * 96, 108 + (i as i32 / 6) * 92, 92, 86);
                if hover == Hover::StartApp(i) {
                    cv.fill_rounded(r, 6, t.hover());
                }
                ui::icon(&mut cv, apps::icon_for(*kind), r.x + 28, r.y + 10, 36, &t);
                let name = apps::name_of(*kind);
                let nw = f.measure(name, 12, Weight::Regular).min(r.w - 4);
                f.draw_fit(&mut cv, r.x + (r.w - nw) / 2, r.y + 56, name, r.w - 4, 12, Weight::Regular, t.text());
            }
            f.draw(&mut cv, 40, 312, "Recommended", 14, Weight::Semibold, t.text());
            if recommended.is_empty() {
                f.draw(&mut cv, 40, 344, "Files you save appear here.", 12, Weight::Regular, t.text_dim());
            }
            for (i, (path, is_dir)) in recommended.iter().enumerate() {
                let r = Rect::new(24 + (i as i32 % 2) * 300, 340 + (i as i32 / 2) * 58, 292, 52);
                if hover == Hover::Recommended(i) {
                    cv.fill_rounded(r, 6, t.hover());
                }
                let name = apps::file_name(path);
                ui::icon(&mut cv, apps::file_icon(name, *is_dir), r.x + 10, r.y + 10, 32, &t);
                f.draw_fit(&mut cv, r.x + 52, r.y + 8, name, r.w - 60, 14, Weight::Regular, t.text());
                f.draw_fit(
                    &mut cv,
                    r.x + 52,
                    r.y + 28,
                    &apps::parent(path),
                    r.w - 60,
                    12,
                    Weight::Regular,
                    t.text_dim(),
                );
            }
        } else {
            f.draw(&mut cv, 40, 82, "Best matches", 14, Weight::Semibold, t.text());
            if hits.is_empty() {
                f.draw(&mut cv, 40, 116, "No results. Try another word.", 14, Weight::Regular, t.text_dim());
            }
            for (i, h) in hits.iter().enumerate() {
                let r = result_rects[i];
                if hover == Hover::StartResult(i) || (i == 0 && !matches!(hover, Hover::StartResult(_))) {
                    cv.fill_rounded(r, 6, if i == 0 { t.selection() } else { t.hover() });
                }
                let (icon, name, sub) = match h {
                    Hit::App(k) => (apps::icon_for(*k), apps::name_of(*k).to_string(), "App".to_string()),
                    Hit::File(p, d) => {
                        let n = apps::file_name(p).to_string();
                        (apps::file_icon(&n, *d), n, apps::parent(p))
                    }
                };
                ui::icon(&mut cv, icon, r.x + 10, r.y + 8, 28, &t);
                f.draw_fit(&mut cv, r.x + 50, r.y + 4, &name, r.w - 60, 14, Weight::Regular, t.text());
                f.draw_fit(&mut cv, r.x + 50, r.y + 23, &sub, r.w - 60, 12, Weight::Regular, t.text_dim());
            }
        }
        // Footer: user and power.
        let footer_y = s.h - 72;
        cv.fill(Rect::new(0, footer_y, s.w, 72), with_alpha(0, if t.dark { 40 } else { 12 }));
        cv.fill(Rect::new(0, footer_y, s.w, 1), t.border());
        cv.circle(52.0, (footer_y + 36) as f32, 16.0, shade(t.accent, -10));
        f.draw(&mut cv, 47, footer_y + 26, "M", 14, Weight::Semibold, rgb(255, 255, 255));
        f.draw(&mut cv, 80, footer_y + 26, "Meridian user", 14, Weight::Regular, t.text());
        let p = local(self.power_rect());
        if hover == Hover::Power || power_open {
            cv.fill_rounded(p, 6, t.hover());
        }
        ui::icon(&mut cv, Icon::Power, p.x + 10, p.y + 10, 20, &t);
        cv.round_corners(10);
        let b = &mut self.back;
        b.shadow(s, 8, 10, 30, if t.dark { 140 } else { 80 });
        b.blit(&cv, cv.bounds(), s.x, s.y);
        b.stroke(s, with_alpha(t.border(), 120));
        if power_open {
            let pr = self.power_rect();
            let menu = Rect::new(pr.right() - 192, pr.y - 148, 192, 140);
            let mut mc = self.mica(menu, t.menu_tint());
            let mb = mc.bounds();
            mc.stroke(mb, t.border());
            for (i, (label, icon)) in
                [("Lock", Icon::Lock), ("Restart", Icon::Restart), ("Shut down", Icon::Power)].iter().enumerate()
            {
                let r = Rect::new(8, 8 + i as i32 * 44, 176, 40);
                if hover == Hover::PowerItem(i) {
                    mc.fill_rounded(r, 5, t.hover());
                }
                ui::icon(&mut mc, *icon, r.x + 12, r.y + 10, 20, &t);
                self.fonts.draw(&mut mc, r.x + 44, r.y + 11, label, 14, Weight::Regular, t.text());
            }
            mc.round_corners(8);
            let b = &mut self.back;
            b.shadow(menu, 4, 8, 16, 90);
            b.blit(&mc, mc.bounds(), menu.x, menu.y);
        }
    }

    fn draw_calendar(&mut self, area: Rect) {
        let Some((year, month)) = self.calendar else { return };
        let t = self.theme;
        let c = self.calendar_rect();
        if c.grow(30).intersect(&area).is_none() {
            return;
        }
        let now = sys::query_time();
        let mut cv = self.mica(c, t.menu_tint());
        let nav = [(-1, self.calendar_nav_rect(-1)), (1, self.calendar_nav_rect(1))];
        let f = &mut self.fonts;
        let title = format!(
            "{}, {} {}",
            clock::WEEKDAYS.get(now.weekday as usize).copied().unwrap_or(""),
            clock::MONTHS.get(now.month as usize - 1).copied().unwrap_or(""),
            now.day
        );
        f.draw(&mut cv, 20, 16, &title, 16, Weight::Semibold, t.text());
        let clock_line = format!("{} {}", self.clock, now.year);
        f.draw(&mut cv, 20, 42, &clock_line, 12, Weight::Regular, t.text_dim());
        cv.fill(Rect::new(0, 72, c.w, 1), t.border());
        let month_name = format!("{} {}", clock::MONTHS[(month - 1) as usize], year);
        f.draw(&mut cv, 20, 90, &month_name, 14, Weight::Semibold, t.text());
        for (d, r) in nav {
            let r = r.offset(-c.x, -c.y);
            if self.hover == Hover::CalendarNav(d) {
                cv.fill_rounded(r, 4, t.hover());
            }
            let (mx, my) = (r.x as f32 + 16.0, r.y as f32 + 16.0);
            let s = if d < 0 { 1.0 } else { -1.0 };
            cv.line(mx + 3.0 * s, my - 5.0, mx - 3.0 * s, my, 1.4, t.text());
            cv.line(mx - 3.0 * s, my, mx + 3.0 * s, my + 5.0, 1.4, t.text());
        }
        for (k, d) in ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"].iter().enumerate() {
            let dw = f.measure(d, 12, Weight::Regular);
            f.draw(&mut cv, 24 + k as i32 * 42 + (32 - dw) / 2, 128, d, 12, Weight::Regular, t.text_dim());
        }
        let first = weekday(year, month, 1) as i32;
        for day in 1..=days_in(year, month) {
            let slot = first + day as i32 - 1;
            let (col, row) = (slot % 7, slot / 7);
            let r = Rect::new(24 + col * 42, 152 + row * 38, 32, 32);
            let today = year == now.year as i32 && month == now.month as u32 && day == now.day as u32;
            if today {
                cv.circle(r.x as f32 + 16.0, r.y as f32 + 16.0, 16.0, t.accent);
            }
            let s = format!("{day}");
            let sw = f.measure(&s, 14, Weight::Regular);
            let color = if today { t.accent_text() } else { t.text() };
            f.draw(&mut cv, r.x + (32 - sw) / 2, r.y + 7, &s, 14, Weight::Regular, color);
        }
        cv.round_corners(10);
        let b = &mut self.back;
        b.shadow(c, 6, 10, 24, if t.dark { 130 } else { 70 });
        b.blit(&cv, cv.bounds(), c.x, c.y);
    }

    fn draw_popup(&mut self, area: Rect) {
        let Some(p) = &self.popup else { return };
        if p.rect.grow(20).intersect(&area).is_none() {
            return;
        }
        let t = self.theme;
        let r = p.rect;
        let mut cv = self.mica(r, t.menu_tint());
        let cb = cv.bounds();
        cv.stroke(cb, t.border());
        let mut y = 4;
        let items = p.items.clone();
        let hover = p.hover;
        for (i, it) in items.iter().enumerate() {
            if it.id == 0 {
                cv.fill(Rect::new(8, y + 4, r.w - 16, 1), t.border());
                y += 9;
                continue;
            }
            let row = Rect::new(4, y, r.w - 8, 32);
            if hover == Some(i) && it.enabled {
                cv.fill_rounded(row, 4, t.hover());
            }
            let color = if it.enabled { t.text() } else { t.text_dim() };
            self.fonts.draw(&mut cv, 16, y + 7, &it.label, 14, Weight::Regular, color);
            y += 32;
        }
        cv.round_corners(8);
        let b = &mut self.back;
        b.shadow(r, 4, 8, 16, if t.dark { 120 } else { 70 });
        b.blit(&cv, cv.bounds(), r.x, r.y);
    }

    fn draw_toasts(&mut self, area: Rect) {
        if self.toasts.is_empty() || self.toast_area().intersect(&area).is_none() {
            return;
        }
        let t = self.theme;
        for k in 0..self.toasts.len() {
            let r = self.toast_rect(k);
            let mut cv = self.mica(r, t.menu_tint());
            let cb = cv.bounds();
            cv.stroke(cb, t.border());
            cv.fill_rounded(Rect::new(0, 0, 4, r.h), 2, t.accent);
            ui::icon(&mut cv, Icon::Start, 16, 22, 32, &t);
            self.fonts.draw(&mut cv, 60, 12, "Meridian", 12, Weight::Semibold, t.text_dim());
            let text = self.toasts[k].text.clone();
            self.fonts.draw_fit(&mut cv, 60, 34, &text, r.w - 72, 14, Weight::Regular, t.text());
            cv.round_corners(8);
            let b = &mut self.back;
            b.shadow(r, 4, 8, 16, 90);
            b.blit(&cv, cv.bounds(), r.x, r.y);
        }
    }

    fn draw_lock(&mut self, area: Rect) {
        let b = &mut self.back;
        b.blit(&self.blur, area, area.x, area.y);
        b.fill(area, with_alpha(0, 90));
        let now = sys::query_time();
        let (w, h) = (self.screen.w, self.screen.h);
        let h12 = match now.hour {
            0 => 12,
            1..=12 => now.hour,
            h => h - 12,
        };
        let time = format!("{h12}:{:02}", now.minute);
        let date = format!(
            "{}, {} {}",
            clock::WEEKDAYS.get(now.weekday as usize).copied().unwrap_or(""),
            clock::MONTHS.get(now.month as usize - 1).copied().unwrap_or(""),
            now.day
        );
        let f = &mut self.fonts;
        let tw = f.measure(&time, 96, Weight::Semibold);
        f.draw(b, (w - tw) / 2, h / 5, &time, 96, Weight::Semibold, rgb(255, 255, 255));
        let dw = f.measure(&date, 24, Weight::Regular);
        f.draw(b, (w - dw) / 2, h / 5 + 130, &date, 24, Weight::Regular, rgb(255, 255, 255));
        b.circle((w / 2) as f32, (h - 190) as f32, 36.0, shade(self.theme.accent, -10));
        let m = f.measure("M", 28, Weight::Semibold);
        f.draw(b, w / 2 - m / 2, h - 210, "M", 28, Weight::Semibold, rgb(255, 255, 255));
        let user = "Meridian user";
        let uw = f.measure(user, 20, Weight::Semibold);
        f.draw(b, (w - uw) / 2, h - 140, user, 20, Weight::Semibold, rgb(255, 255, 255));
        let hint = "Click or press any key to unlock (no password is set)";
        let hw = f.measure(hint, 14, Weight::Regular);
        f.draw(b, (w - hw) / 2, h - 100, hint, 14, Weight::Regular, with_alpha(rgb(255, 255, 255), 200));
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

    /// Pre-draws the resize pointers (double-headed arrows) as images.
    fn build_cursors(&mut self) {
        for (kind, (dx, dy)) in [
            (CursorKind::Horizontal, (1.0f32, 0.0f32)),
            (CursorKind::Vertical, (0.0, 1.0)),
            (CursorKind::Diagonal, (0.707, 0.707)),
            (CursorKind::AntiDiagonal, (0.707, -0.707)),
        ] {
            let mut c = Canvas::new(24, 24, 0);
            for (width, color) in [(4.5, rgb(0, 0, 0)), (1.6, rgb(255, 255, 255))] {
                let (x0, y0, x1, y1) = (12.0 - dx * 9.0, 12.0 - dy * 9.0, 12.0 + dx * 9.0, 12.0 + dy * 9.0);
                c.line(x0, y0, x1, y1, width, color);
                for (ex, ey, s) in [(x0, y0, 1.0), (x1, y1, -1.0)] {
                    let (px, py) = (-dy, dx);
                    c.line(ex, ey, ex + (dx * 4.0 + px * 4.0) * s, ey + (dy * 4.0 + py * 4.0) * s, width, color);
                    c.line(ex, ey, ex + (dx * 4.0 - px * 4.0) * s, ey + (dy * 4.0 - py * 4.0) * s, width, color);
                }
            }
            self.cursors.push((kind, c));
        }
    }

    fn draw_cursor(&mut self) {
        let (mx, my) = self.mouse;
        let stride = self.info.stride as usize;
        let swap = self.info.format == 0;
        if let Some((_, img)) = self.cursors.iter().find(|(k, _)| *k == self.cursor) {
            let (ox, oy) = (mx - 12, my - 12);
            for y in 0..img.h {
                for x in 0..img.w {
                    let (sx, sy) = (ox + x, oy + y);
                    let p = img.px[(y * img.w + x) as usize];
                    if p >> 24 < 96 || sx < 0 || sy < 0 || sx >= self.screen.w || sy >= self.screen.h {
                        continue;
                    }
                    let v = if swap { (p & 0xFF00FF00) | (p & 0xFF) << 16 | (p >> 16) & 0xFF } else { p };
                    // SAFETY: bounds checked against the screen.
                    unsafe { self.fb.add(sy as usize * stride + sx as usize).write_volatile(v | 0xFF00_0000) };
                }
            }
            self.cursor_drawn = Rect::new(ox, oy, 24, 24);
            return;
        }
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

    fn cursor_moved(&self) -> bool {
        let want = match self.cursor {
            CursorKind::Arrow => Rect::new(self.mouse.0, self.mouse.1, 12, 19),
            _ => Rect::new(self.mouse.0 - 12, self.mouse.1 - 12, 24, 24),
        };
        want != self.cursor_drawn
    }

    fn flush(&mut self) {
        let Some(area) = self.damage.take() else {
            if self.cursor_moved() {
                let old = self.cursor_drawn;
                self.present(old);
                self.draw_cursor();
            }
            return;
        };
        for i in 0..self.windows.len() {
            if self.windows[i].minimized {
                continue;
            }
            if self.windows[i].content_dirty {
                self.render_content(i);
            }
            if self.windows[i].title_dirty {
                self.render_title(i);
            }
        }
        self.compose(area);
        let old = self.cursor_drawn;
        self.present(area);
        if old.intersect(&area).is_none() || self.cursor_moved() {
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
    let mut next_tick = desk.tick(now_ms());
    desk.flush();
    rt::debug_print(&format!("desktop: ready ({}x{})", info.width, info.height));

    let mut events = [InputEvent::default(); 64];
    loop {
        let now = now_ms();
        let timeout = if desk.drag.is_some() { 15 } else { next_tick.saturating_sub(now).clamp(1, 1000) };
        let n = sys::read_input(&mut events, Some(timeout)).unwrap_or(0);
        for e in &events[..n] {
            desk.handle(e);
        }
        let now = now_ms();
        if now >= next_tick {
            next_tick = desk.tick(now);
        }
        desk.flush();
    }
}
