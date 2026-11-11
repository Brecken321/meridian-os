//! The built-in desktop apps: the types they share with the shell, and the
//! smaller apps (Terminal, Task Manager, Settings, About). Each app talks
//! to the kernel through real system calls; nothing on screen is a mock-up.
//! (They run inside the desktop process for now; separate processes need
//! the window-server IPC that is on the roadmap.)

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use meridian_rt::sys::{self, InputEvent, input, key, volume_flags};
use meridian_rt::{Handle, status};

use crate::font::{Fonts, MONO_H, MONO_W, Mono, Weight};
use crate::gfx::{Canvas, Rect, rgb, with_alpha};
use crate::ui::{self, ACCENTS, Icon, Theme, WALLPAPERS};

/// Text and file clipboard shared by every app.
#[derive(Default)]
pub struct Clipboard {
    pub text: String,
    /// Paths copied (or cut, if `cut`) in File Explorer.
    pub files: Vec<String>,
    pub cut: bool,
}

pub struct Ctx<'a> {
    pub fonts: &'a mut Fonts,
    pub mono: &'a Mono,
    pub theme: Theme,
    pub clipboard: &'a mut Clipboard,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AppKind {
    Explorer,
    Notepad,
    Terminal,
    TaskManager,
    Settings,
    About,
    Calculator,
    Paint,
    Clock,
    Photos,
    Mines,
    Puzzle,
}

/// Every app, in Start menu order.
pub const ALL_APPS: [AppKind; 12] = [
    AppKind::Explorer,
    AppKind::Notepad,
    AppKind::Terminal,
    AppKind::Calculator,
    AppKind::Paint,
    AppKind::Photos,
    AppKind::Clock,
    AppKind::TaskManager,
    AppKind::Settings,
    AppKind::Mines,
    AppKind::Puzzle,
    AppKind::About,
];

/// One entry of a context menu; `id` 0 is a separator.
#[derive(Clone)]
pub struct MenuItem {
    pub label: String,
    pub id: u32,
    pub enabled: bool,
}

impl MenuItem {
    pub fn new(label: &str, id: u32) -> Self {
        Self { label: label.to_string(), id, enabled: true }
    }
    pub fn when(label: &str, id: u32, enabled: bool) -> Self {
        Self { label: label.to_string(), id, enabled }
    }
    pub fn separator() -> Self {
        Self { label: String::new(), id: 0, enabled: false }
    }
}

pub enum Action {
    Open(AppKind, Option<String>),
    SetTheme(Theme),
    Close,
    /// Shows a notification in the corner of the screen.
    Notify(String),
    /// Shows a context menu at content-relative (x, y); the choice comes
    /// back through `App::menu`.
    Menu(Vec<MenuItem>, i32, i32),
    /// Files changed on disk: other windows and the desktop refresh.
    FilesChanged,
}

pub trait App {
    fn kind(&self) -> AppKind;
    fn title(&self) -> String;
    fn icon(&self) -> Icon {
        icon_for(self.kind())
    }
    fn size(&self) -> (i32, i32);
    fn min_size(&self) -> (i32, i32) {
        (360, 240)
    }
    /// Draws the content into `r` (canvas coordinates).
    fn draw(&mut self, cv: &mut Canvas, r: Rect, cx: &mut Ctx);
    /// Left click at content-relative (`x`, `y`).
    fn click(&mut self, x: i32, y: i32, double: bool, cx: &mut Ctx) -> Vec<Action>;
    fn right_click(&mut self, _x: i32, _y: i32, _cx: &mut Ctx) -> Vec<Action> {
        Vec::new()
    }
    /// Pointer moved with the left button held (after a click in the
    /// content); returns true to redraw.
    fn drag(&mut self, _x: i32, _y: i32, _cx: &mut Ctx) -> bool {
        false
    }
    /// Left button released after a click in the content.
    fn release(&mut self, _x: i32, _y: i32, _cx: &mut Ctx) -> Vec<Action> {
        Vec::new()
    }
    /// Pointer moved to content-relative (`x`, `y`); returns true to redraw.
    fn hover(&mut self, _x: i32, _y: i32) -> bool {
        false
    }
    fn key(&mut self, _e: &InputEvent, _cx: &mut Ctx) -> Vec<Action> {
        Vec::new()
    }
    /// A context-menu choice.
    fn menu(&mut self, _id: u32, _cx: &mut Ctx) -> Vec<Action> {
        Vec::new()
    }
    /// Mouse wheel turned by `notches` (positive scrolls down); returns
    /// true to redraw.
    fn wheel(&mut self, _notches: i32) -> bool {
        false
    }
    /// Called every `interval()` milliseconds; returns true to redraw.
    fn tick(&mut self) -> bool {
        false
    }
    fn interval(&self) -> u64 {
        1000
    }
    /// Files changed somewhere; returns true to redraw.
    fn files_changed(&mut self) -> bool {
        false
    }
    /// Actions the app wants to run on its own (timers finishing...).
    fn pending(&mut self) -> Vec<Action> {
        Vec::new()
    }
    /// The user asked to close the window; returning false keeps it open
    /// (Notepad first asks whether to save).
    fn close_requested(&mut self) -> bool {
        true
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
        AppKind::Calculator => Icon::Calculator,
        AppKind::Paint => Icon::Paint,
        AppKind::Clock => Icon::Clock,
        AppKind::Photos => Icon::Photos,
        AppKind::Mines => Icon::Mines,
        AppKind::Puzzle => Icon::Puzzle,
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
        AppKind::Calculator => "Calculator",
        AppKind::Paint => "Paint",
        AppKind::Clock => "Clock",
        AppKind::Photos => "Photos",
        AppKind::Mines => "Minesweeper",
        AppKind::Puzzle => "2048",
    }
}

/// Words the Start menu search also matches.
pub fn keywords_of(kind: AppKind) -> &'static str {
    match kind {
        AppKind::Explorer => "files folders drives this pc",
        AppKind::Notepad => "text editor notes write",
        AppKind::Terminal => "command prompt shell console cmd",
        AppKind::TaskManager => "processes threads cpu memory performance",
        AppKind::Settings => "personalise theme dark light accent wallpaper storage",
        AppKind::About => "version system info",
        AppKind::Calculator => "math numbers calc",
        AppKind::Paint => "draw picture image art",
        AppKind::Clock => "time stopwatch timer alarm",
        AppKind::Photos => "pictures images viewer",
        AppKind::Mines => "game mines",
        AppKind::Puzzle => "game tiles puzzle",
    }
}

pub fn create(kind: AppKind, arg: Option<String>) -> Box<dyn App> {
    match kind {
        AppKind::Explorer => Box::new(crate::explorer::Explorer::new(arg.unwrap_or_default())),
        AppKind::Notepad => Box::new(crate::notepad::Notepad::new(arg)),
        AppKind::Terminal => Box::new(Terminal::new(arg)),
        AppKind::TaskManager => Box::new(TaskManager::new()),
        AppKind::Settings => Box::new(Settings::new()),
        AppKind::About => Box::new(About { w: 500, h: 300 }),
        AppKind::Calculator => Box::new(crate::calc::Calculator::new()),
        AppKind::Paint => Box::new(crate::paint::Paint::new(arg)),
        AppKind::Clock => Box::new(crate::clock::Clock::new()),
        AppKind::Photos => Box::new(crate::photos::Photos::new(arg)),
        AppKind::Mines => Box::new(crate::games::Mines::new()),
        AppKind::Puzzle => Box::new(crate::games::Puzzle::new()),
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
        status::MEDIA_WRITE_PROTECTED => "the drive is read-only (save on D: instead)",
        status::OBJECT_NAME_COLLISION => "a file or folder with that name already exists",
        status::DIRECTORY_NOT_EMPTY => "the folder is not empty",
        status::OBJECT_NAME_INVALID => "that name is not allowed",
        status::DISK_FULL => "the drive is full",
        status::NOT_SAME_DEVICE => "cannot move between drives",
        status::INVALID_PARAMETER => "invalid name or path",
        _ => "failed",
    }
}

pub fn file_name(path: &str) -> &str {
    path.rsplit('\\').next().unwrap_or(path)
}

pub fn extension(name: &str) -> String {
    name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

pub fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('\\') { format!("{dir}{name}") } else { format!("{dir}\\{name}") }
}

pub fn parent(path: &str) -> String {
    let trimmed = path.trim_end_matches('\\');
    match trimmed.rfind('\\') {
        Some(i) if i >= 2 => {
            let p = &trimmed[..i];
            if p.len() == 2 { format!("{p}\\") } else { p.to_string() }
        }
        _ => String::new(), // "This PC"
    }
}

pub fn human_size(bytes: u64) -> String {
    match bytes {
        0..1024 => format!("{bytes} bytes"),
        1024..1_048_576 => format!("{} KB", bytes.div_ceil(1024)),
        1_048_576..1_073_741_824 => format!("{}.{} MB", bytes / 1_048_576, bytes % 1_048_576 * 10 / 1_048_576),
        _ => format!("{}.{} GB", bytes >> 30, (bytes & ((1 << 30) - 1)) * 10 >> 30),
    }
}

pub const TEXT_EXTENSIONS: [&str; 9] = ["txt", "log", "md", "ini", "cfg", "json", "csv", "rs", "py"];

pub fn file_icon(name: &str, is_dir: bool) -> Icon {
    if is_dir {
        return match name.to_ascii_lowercase().as_str() {
            "documents" => Icon::Documents,
            "pictures" => Icon::Pictures,
            "desktop" => Icon::Desktop,
            _ => Icon::Folder,
        };
    }
    let ext = extension(name);
    if ext == "exe" {
        Icon::Program
    } else if ext == "bmp" {
        Icon::Image
    } else if TEXT_EXTENSIONS.contains(&ext.as_str()) {
        Icon::TextFile
    } else {
        Icon::File
    }
}

pub fn file_kind(name: &str, is_dir: bool) -> String {
    let ext = extension(name);
    if is_dir {
        "File folder".to_string()
    } else if ext.is_empty() {
        "File".to_string()
    } else if ext == "exe" {
        "Application".to_string()
    } else if ext == "bmp" {
        "Bitmap image".to_string()
    } else if ext == "txt" {
        "Text document".to_string()
    } else {
        format!("{} file", ext.to_ascii_uppercase())
    }
}

/// A free name like "New folder", "New folder (2)" in `dir`.
pub fn unique_name(dir: &str, base: &str, ext: &str) -> String {
    let existing: Vec<String> =
        sys::list_directory(dir).map(|l| l.into_iter().map(|e| e.name.to_lowercase()).collect()).unwrap_or_default();
    let dot = if ext.is_empty() { String::new() } else { format!(".{ext}") };
    for n in 1..1000 {
        let name = if n == 1 { format!("{base}{dot}") } else { format!("{base} ({n}){dot}") };
        if !existing.contains(&name.to_lowercase()) {
            return name;
        }
    }
    format!("{base}{dot}")
}

/// Opens a file the way the shell would: folders in Explorer, text in
/// Notepad, pictures in Photos, programs by starting a process.
pub enum Opened {
    Actions(Vec<Action>),
    Started(Handle, String),
    Message(String),
}

pub fn open_path(path: &str, is_dir: bool) -> Opened {
    if is_dir {
        return Opened::Actions(vec![Action::Open(AppKind::Explorer, Some(path.to_string()))]);
    }
    let ext = extension(file_name(path));
    if TEXT_EXTENSIONS.contains(&ext.as_str()) {
        return Opened::Actions(vec![Action::Open(AppKind::Notepad, Some(path.to_string()))]);
    }
    match ext.as_str() {
        "bmp" => Opened::Actions(vec![Action::Open(AppKind::Photos, Some(path.to_string()))]),
        "exe" => match sys::create_process(path, 0) {
            Ok(h) => Opened::Started(h, file_name(path).to_string()),
            Err(e) => Opened::Message(format!("Could not start {}: {}", file_name(path), error_text(e))),
        },
        _ => Opened::Message(format!("Meridian has no app to open {} files yet", file_name(path))),
    }
}

/// True for Ctrl+`letter` key events.
pub fn ctrl(e: &InputEvent, letter: char) -> bool {
    e.flags & input::CTRL != 0
        && e.key == key::CHAR
        && char::from_u32(e.ch).is_some_and(|c| c.eq_ignore_ascii_case(&letter) || c as u32 == letter as u32 - 96)
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
    history: Vec<String>,
    history_pos: usize,
}

const TERMINAL_HELP: &str = "\
  Files and folders
    dir [path]          list a folder          cd <path>          change folder
    type <file>         show a text file       mkdir <name>       make a folder
    copy <from> <to>    copy a file            move <from> <to>   rename or move
    del <file>          delete a file          rmdir <folder>     delete an empty folder
    echo <text> > file  write text to a file (>> appends)
    drives              list drives and free space
  Programs and system
    run <file.exe>      start a program        open <file>        open with its app
    notepad, paint, calc, explorer, taskmgr, settings, photos, clock
    ps                  processes              threads            kernel threads
    mem                 memory                 uptime             time since boot
    date                date and time          ver                version
    cls                 clear the screen       exit               close this window
  Keys: Up/Down recall commands, Ctrl+V pastes, Ctrl+L clears.
  C: is the read-only boot disk; D: is the data drive where files can be saved.";

impl Terminal {
    fn new(cwd: Option<String>) -> Self {
        let mut t = Self {
            lines: Vec::new(),
            input: String::new(),
            cwd: "D:\\".into(),
            children: Vec::new(),
            rows: 24,
            history: Vec::new(),
            history_pos: 0,
        };
        if let Some(dir) = cwd.filter(|d| sys::list_directory(d).is_ok()) {
            t.cwd = dir;
        } else if sys::list_directory("D:\\").is_err() {
            t.cwd = "C:\\".into();
        }
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
            return if arg.len() == 2 { format!("{arg}\\") } else { arg };
        }
        let mut path = if arg.starts_with('\\') { self.cwd[..3].to_string() } else { self.cwd.clone() };
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

    /// Splits arguments on spaces, keeping "quoted strings" together.
    fn args(rest: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = String::new();
        let mut quoted = false;
        for c in rest.chars() {
            match c {
                '"' => quoted = !quoted,
                ' ' if !quoted => {
                    if !cur.is_empty() {
                        out.push(core::mem::take(&mut cur));
                    }
                }
                c => cur.push(c),
            }
        }
        if !cur.is_empty() {
            out.push(cur);
        }
        out
    }

    fn report(&mut self, what: &str, r: Result<(), i64>) -> bool {
        match r {
            Ok(()) => true,
            Err(e) => {
                self.print(&format!("{what}: {}", error_text(e)));
                false
            }
        }
    }

    fn run_command(&mut self, line: &str) -> Vec<Action> {
        let line = line.trim();
        // echo text > file / >> file
        if let Some(i) = line.find('>') {
            if line[..i].trim_start().to_ascii_lowercase().starts_with("echo") {
                let append = line[i + 1..].starts_with('>');
                let target = line[i + if append { 2 } else { 1 }..].trim();
                let text = line[..i].trim_start()[4..].trim();
                let path = self.resolve(target);
                let mut data = if append { sys::read_file(&path).unwrap_or_default() } else { Vec::new() };
                data.extend_from_slice(text.as_bytes());
                data.extend_from_slice(b"\r\n");
                if self.report(&path, sys::write_file(&path, &data)) {
                    return vec![Action::FilesChanged];
                }
                return Vec::new();
            }
        }
        let (cmd, rest) = line.split_once(' ').unwrap_or((line, ""));
        let rest = rest.trim();
        let args = Self::args(rest);
        // Commands that take one path use the whole rest of the line, so
        // names with spaces work without quotes.
        let one = rest.trim_matches('"').to_string();
        let opt = if one.is_empty() { None } else { Some(self.resolve(&one)) };
        let app = |kind: AppKind, arg: Option<String>| vec![Action::Open(kind, arg)];
        match cmd.to_ascii_lowercase().as_str() {
            "" => {}
            "help" | "?" => self.print(TERMINAL_HELP),
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
                        let mut bytes = 0;
                        for e in &entries {
                            if e.is_dir {
                                self.print(&format!("  <DIR>          {}", e.name));
                            } else {
                                files += 1;
                                bytes += e.size;
                                self.print(&format!("  {:>14}  {}", e.size, e.name));
                            }
                        }
                        self.print(&format!("  {} file(s) {} bytes, {} dir(s)", files, bytes, entries.len() - files));
                    }
                    Err(e) => self.print(&format!("{path}: {}", error_text(e))),
                }
            }
            "cd" | "chdir" => {
                if rest.is_empty() {
                    let cwd = self.cwd.clone();
                    self.print(&cwd);
                } else {
                    let path = self.resolve(&one);
                    match sys::list_directory(&path) {
                        Ok(_) => self.cwd = path,
                        Err(e) => self.print(&format!("{path}: {}", error_text(e))),
                    }
                }
            }
            "type" | "cat" => {
                let path = self.resolve(&one);
                match sys::read_file(&path) {
                    Ok(data) => {
                        let text = String::from_utf8_lossy(&data[..data.len().min(16384)]).replace('\r', "");
                        self.print(&text);
                    }
                    Err(e) => self.print(&format!("{path}: {}", error_text(e))),
                }
            }
            "mkdir" | "md" if !one.is_empty() => {
                let path = self.resolve(&one);
                if self.report(&path, sys::create_directory(&path)) {
                    return vec![Action::FilesChanged];
                }
            }
            "del" | "rm" | "erase" | "rmdir" | "rd" if !one.is_empty() => {
                let path = self.resolve(&one);
                if self.report(&path, sys::delete_file(&path)) {
                    return vec![Action::FilesChanged];
                }
            }
            "move" | "mv" | "ren" | "rename" if args.len() == 2 => {
                let from = self.resolve(&args[0]);
                let mut to = self.resolve(&args[1]);
                if cmd.eq_ignore_ascii_case("ren") || cmd.eq_ignore_ascii_case("rename") {
                    to = join(&parent(&from), &args[1]);
                } else if sys::list_directory(&to).is_ok() {
                    to = join(&to, file_name(&from));
                }
                if self.report(&from, sys::rename(&from, &to)) {
                    return vec![Action::FilesChanged];
                }
            }
            "copy" | "cp" if args.len() == 2 => {
                let from = self.resolve(&args[0]);
                let mut to = self.resolve(&args[1]);
                if sys::list_directory(&to).is_ok() {
                    to = join(&to, file_name(&from));
                }
                match sys::read_file(&from) {
                    Ok(data) => {
                        if self.report(&to, sys::write_file(&to, &data)) {
                            self.print(&format!("        1 file(s) copied ({} bytes).", data.len()));
                            return vec![Action::FilesChanged];
                        }
                    }
                    Err(e) => self.print(&format!("{from}: {}", error_text(e))),
                }
            }
            "drives" | "vol" => {
                for v in sys::query_volumes() {
                    let letter = char::from_u32(v.letter).unwrap_or('?');
                    let rw = if v.flags & volume_flags::WRITABLE != 0 { "read/write" } else { "read-only" };
                    self.print(&format!(
                        "  {letter}:  {:<12} {:>10} free of {:>10}  {rw}",
                        sys::name_of(&v.label),
                        human_size(v.free_bytes),
                        human_size(v.total_bytes)
                    ));
                }
            }
            "run" | "start" => {
                let target = args.first().map(String::as_str).unwrap_or("");
                let arg = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(0);
                let path = self.resolve(target);
                match sys::create_process(&path, arg) {
                    Ok(h) => {
                        self.print(&format!("Started {} (its messages go to the system log)", file_name(&path)));
                        self.children.push((h, file_name(&path).to_string()));
                    }
                    Err(e) => self.print(&format!("Could not start {path}: {}", error_text(e))),
                }
            }
            "open" if !one.is_empty() => {
                let path = self.resolve(&one);
                let is_dir = sys::list_directory(&path).is_ok();
                match open_path(&path, is_dir) {
                    Opened::Actions(a) => return a,
                    Opened::Started(h, name) => {
                        self.print(&format!("Started {name}"));
                        self.children.push((h, name));
                    }
                    Opened::Message(m) => self.print(&m),
                }
            }
            "notepad" | "edit" => return app(AppKind::Notepad, opt),
            "paint" | "mspaint" => return app(AppKind::Paint, opt),
            "photos" => return app(AppKind::Photos, opt),
            "explorer" => return app(AppKind::Explorer, Some(opt.unwrap_or_else(|| self.cwd.clone()))),
            "calc" => return app(AppKind::Calculator, None),
            "taskmgr" => return app(AppKind::TaskManager, None),
            "settings" | "control" => return app(AppKind::Settings, None),
            "clock" => return app(AppKind::Clock, None),
            "winver" => return app(AppKind::About, None),
            "ps" | "tasklist" => {
                self.print("  PID   Name              Memory    Handles  Status");
                for p in sys::query_processes() {
                    let status = if p.running != 0 {
                        "running".to_string()
                    } else {
                        format!("exited {:#x}", p.exit_status as u32)
                    };
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
                    let state =
                        ["init", "ready", "running", "waiting", "done"].get(t.state as usize).copied().unwrap_or("?");
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
                self.print(&format!(
                    "  {:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                    t.year, t.month, t.day, t.hour, t.minute, t.second
                ));
            }
            "exit" => return vec![Action::Close],
            "mkdir" | "md" | "del" | "rm" | "erase" | "rmdir" | "rd" | "open" => {
                self.print(&format!("usage: {cmd} <path>"));
            }
            "move" | "mv" | "ren" | "rename" | "copy" | "cp" => self.print(&format!("usage: {cmd} <from> <to>")),
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
        format!("Terminal - {}", self.cwd)
    }
    fn size(&self) -> (i32, i32) {
        (780, 470)
    }

    fn draw(&mut self, cv: &mut Canvas, r: Rect, cx: &mut Ctx) {
        cv.fill(r, rgb(0x0C, 0x0C, 0x0C));
        self.rows = ((r.h - 12) / MONO_H).max(1) as usize;
        let cols = ((r.w - 16) / MONO_W).max(10) as usize;
        let prompt = format!("{}> {}", self.cwd, self.input);
        let start = (self.lines.len() + 1).saturating_sub(self.rows);
        let mut y = r.y + 6;
        for line in self.lines.iter().skip(start) {
            let shown: String = line.chars().take(cols).collect();
            cx.mono.draw(cv, r.x + 8, y, &shown, rgb(0xCC, 0xCC, 0xCC));
            y += MONO_H;
        }
        let skip = prompt.chars().count().saturating_sub(cols - 1);
        let shown: String = prompt.chars().skip(skip).collect();
        cx.mono.draw(cv, r.x + 8, y, &shown, rgb(0xF2, 0xF2, 0xF2));
        let caret_x = r.x + 8 + shown.chars().count() as i32 * MONO_W;
        cv.fill(Rect::new(caret_x, y + MONO_H - 3, MONO_W, 2), rgb(0xF2, 0xF2, 0xF2));
    }

    fn click(&mut self, _x: i32, _y: i32, _double: bool, _cx: &mut Ctx) -> Vec<Action> {
        Vec::new()
    }

    fn right_click(&mut self, x: i32, y: i32, cx: &mut Ctx) -> Vec<Action> {
        vec![Action::Menu(
            vec![MenuItem::when("Paste", 1, !cx.clipboard.text.is_empty()), MenuItem::new("Clear", 2)],
            x,
            y,
        )]
    }

    fn menu(&mut self, id: u32, cx: &mut Ctx) -> Vec<Action> {
        match id {
            1 => self.paste(cx),
            2 => self.lines.clear(),
            _ => {}
        }
        Vec::new()
    }

    fn key(&mut self, e: &InputEvent, cx: &mut Ctx) -> Vec<Action> {
        if ctrl(e, 'v') {
            self.paste(cx);
            return Vec::new();
        }
        if ctrl(e, 'l') {
            self.lines.clear();
            return Vec::new();
        }
        if e.flags & input::CTRL != 0 && e.key == key::CHAR {
            return Vec::new();
        }
        match e.key {
            key::CHAR => {
                if let Some(c) = char::from_u32(e.ch) {
                    if self.input.len() < 400 {
                        self.input.push(c);
                    }
                }
            }
            key::BACKSPACE => {
                self.input.pop();
            }
            key::ESCAPE => self.input.clear(),
            key::UP if self.history_pos > 0 => {
                self.history_pos -= 1;
                self.input = self.history[self.history_pos].clone();
            }
            key::DOWN if self.history_pos < self.history.len() => {
                self.history_pos += 1;
                self.input = self.history.get(self.history_pos).cloned().unwrap_or_default();
            }
            key::ENTER => {
                let line = core::mem::take(&mut self.input);
                let echo = format!("{}> {line}", self.cwd);
                self.print(&echo);
                if !line.trim().is_empty() && self.history.last() != Some(&line) {
                    self.history.push(line.clone());
                }
                self.history_pos = self.history.len();
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

impl Terminal {
    fn paste(&mut self, cx: &mut Ctx) {
        let room = 400usize.saturating_sub(self.input.len());
        let text: String =
            cx.clipboard.text.lines().next().unwrap_or("").chars().filter(|c| !c.is_control()).take(room).collect();
        self.input.push_str(&text);
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

const PAGES: [&str; 4] = ["Personalisation", "Storage", "System", "About"];

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
                    "Changes apply immediately and are saved on drive D: (D:\\Meridian\\settings.ini).",
                    12,
                    Weight::Regular,
                    t.text_dim(),
                );
            }
            1 => {
                let volumes = sys::query_volumes();
                if volumes.is_empty() {
                    f.draw(cv, x0, r.y + 80, "No drives are mounted.", 14, Weight::Regular, t.text_dim());
                }
                for (i, v) in volumes.iter().enumerate() {
                    let c = card(cv, r.y + 70 + i as i32 * 96, 84);
                    let letter = char::from_u32(v.letter).unwrap_or('?');
                    let writable = v.flags & volume_flags::WRITABLE != 0;
                    ui::icon(cv, Icon::Drive, c.x + 16, c.y + 20, 40, &t);
                    let name = if writable { format!("Data ({letter}:)") } else { format!("Boot disk ({letter}:)") };
                    let title = format!("{name}  -  {}", sys::name_of(&v.label));
                    f.draw(cv, c.x + 72, c.y + 12, &title, 14, Weight::Semibold, t.text());
                    let bar = Rect::new(c.x + 72, c.y + 40, c.w - 100, 10);
                    cv.fill_rounded(bar, 5, t.hover());
                    let used = v.total_bytes.saturating_sub(v.free_bytes);
                    let fill = (bar.w as u64 * used / v.total_bytes.max(1)) as i32;
                    let color = if used * 10 > v.total_bytes * 9 { t.danger() } else { t.accent };
                    cv.fill_rounded(Rect::new(bar.x, bar.y, fill.max(10), bar.h), 5, color);
                    let detail = format!(
                        "{} free of {}  -  {}",
                        human_size(v.free_bytes),
                        human_size(v.total_bytes),
                        if writable { "you can save files here" } else { "read-only" }
                    );
                    f.draw(cv, c.x + 72, c.y + 58, &detail, 12, Weight::Regular, t.text_dim());
                }
            }
            2 => {
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
                    ("Storage", "SATA (AHCI) disks, FAT32; C: read-only boot disk, D: writable data drive".to_string()),
                    ("Input", "PS/2 keyboard and mouse (absolute pointer in virtual machines)".to_string()),
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
