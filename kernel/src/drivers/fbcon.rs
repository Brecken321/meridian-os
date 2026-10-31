//! Framebuffer console: the boot screen header plus a scrolling text area
//! that understands the ANSI colour sequences the kernel log uses.
//!
//! Text is rendered into a RAM shadow buffer and copied to the framebuffer,
//! because reading video memory back (needed to scroll) is very slow on real
//! hardware. If the shadow cannot be allocated the console draws directly
//! and wraps instead of scrolling.

use alloc::vec::Vec;
use core::ptr;

use meridian_bootinfo::{BootInfo, pixel_format};

use crate::ke::irql::HIGH_LEVEL;
use crate::mm::phys_to_virt;
use crate::sync::SpinLock;

static FONT: &[u8; 4096] = include_bytes!("../../assets/font8x16.bin");
const GW: usize = 8;
const GH: usize = 16;
const HEADER_HEIGHT: usize = 56;

#[derive(Clone, Copy)]
struct Rgb(u8, u8, u8);

const BACKGROUND: Rgb = Rgb(0x0B, 0x11, 0x20);
const FOREGROUND: Rgb = Rgb(0xD6, 0xDD, 0xE8);
const ACCENT_LEFT: Rgb = Rgb(0x25, 0x63, 0xEB);
const ACCENT_RIGHT: Rgb = Rgb(0x06, 0xB6, 0xD4);
const CRASH_BACKGROUND: Rgb = Rgb(0x1A, 0x1D, 0x2E);
const CRASH_ACCENT: Rgb = Rgb(0xEF, 0x44, 0x44);
const WHITE: Rgb = Rgb(0xFF, 0xFF, 0xFF);

/// ANSI colours 0-7 and their bright variants 8-15.
const PALETTE: [Rgb; 16] = [
    Rgb(0x1E, 0x24, 0x33),
    Rgb(0xEF, 0x44, 0x44),
    Rgb(0x22, 0xC5, 0x5E),
    Rgb(0xEA, 0xB3, 0x08),
    Rgb(0x3B, 0x82, 0xF6),
    Rgb(0xA8, 0x55, 0xF7),
    Rgb(0x06, 0xB6, 0xD4),
    Rgb(0xD6, 0xDD, 0xE8),
    Rgb(0x64, 0x74, 0x8B),
    Rgb(0xF8, 0x71, 0x71),
    Rgb(0x4A, 0xDE, 0x80),
    Rgb(0xFA, 0xCC, 0x15),
    Rgb(0x60, 0xA5, 0xFA),
    Rgb(0xC0, 0x84, 0xFC),
    Rgb(0x22, 0xD3, 0xEE),
    Rgb(0xFF, 0xFF, 0xFF),
];

struct Surface {
    base: *mut u32,
    width: usize,
    height: usize,
    stride: usize,
    bgr: bool,
}

impl Surface {
    fn pack(&self, c: Rgb) -> u32 {
        let (r, g, b) = (c.0 as u32, c.1 as u32, c.2 as u32);
        if self.bgr { r << 16 | g << 8 | b } else { b << 16 | g << 8 | r }
    }

    fn put(&self, x: usize, y: usize, color: u32) {
        if x < self.width && y < self.height {
            // SAFETY: bounds checked; base maps the whole framebuffer.
            unsafe { ptr::write_volatile(self.base.add(y * self.stride + x), color) };
        }
    }

    fn fill(&self, x: usize, y: usize, w: usize, h: usize, color: u32) {
        for row in y..(y + h).min(self.height) {
            for col in x..(x + w).min(self.width) {
                // SAFETY: bounds clamped above.
                unsafe { ptr::write_volatile(self.base.add(row * self.stride + col), color) };
            }
        }
    }

    fn text(&self, x: usize, y: usize, s: &str, scale: usize, color: u32) {
        for (i, ch) in s.chars().enumerate() {
            let glyph = glyph(ch);
            for (gy, bits) in glyph.iter().enumerate() {
                for gx in 0..GW {
                    if bits & (0x80 >> gx) != 0 {
                        self.fill(x + (i * GW + gx) * scale, y + gy * scale, scale, scale, color);
                    }
                }
            }
        }
    }
}

fn glyph(ch: char) -> &'static [u8] {
    let code = if (ch as u32) < 256 { ch as usize } else { b'?' as usize };
    &FONT[code * GH..code * GH + GH]
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Escape {
    None,
    Started,
    Csi,
}

struct Console {
    fb: Surface,
    shadow: Vec<u32>,
    top: usize,
    cols: usize,
    rows: usize,
    col: usize,
    row: usize,
    fg: u32,
    bg: u32,
    default_fg: u32,
    default_bg: u32,
    escape: Escape,
    params: [u16; 4],
    nparams: usize,
    utf8_value: u32,
    utf8_left: u8,
}

// SAFETY: the console is only reached through its spin lock.
unsafe impl Send for Console {}

static CONSOLE: SpinLock<Option<Console>> = SpinLock::new(None);

impl Console {
    fn shadowed(&self) -> bool {
        !self.shadow.is_empty()
    }

    fn draw_cell(&mut self, col: usize, row: usize, ch: char) {
        let x0 = col * GW;
        let y0 = row * GH;
        let w = self.fb.width;
        for (gy, bits) in glyph(ch).iter().enumerate() {
            let y = y0 + gy;
            for gx in 0..GW {
                let color = if bits & (0x80 >> gx) != 0 { self.fg } else { self.bg };
                if self.shadowed() {
                    self.shadow[y * w + x0 + gx] = color;
                }
                self.fb.put(x0 + gx, self.top + y, color);
            }
        }
    }

    fn blit_area(&self) {
        let w = self.fb.width;
        for y in 0..self.rows * GH {
            // SAFETY: both rows are `w` pixels long and inside their buffers.
            unsafe {
                ptr::copy_nonoverlapping(
                    self.shadow.as_ptr().add(y * w),
                    self.fb.base.add((self.top + y) * self.fb.stride),
                    w,
                );
            }
        }
    }

    fn clear_area(&mut self) {
        self.shadow.fill(self.bg);
        self.fb.fill(0, self.top, self.fb.width, self.rows * GH, self.bg);
        self.col = 0;
        self.row = 0;
    }

    fn newline(&mut self) {
        self.col = 0;
        self.row += 1;
        if self.row < self.rows {
            return;
        }
        if self.shadowed() {
            let w = self.fb.width;
            let area = self.rows * GH * w;
            self.shadow.copy_within(GH * w..area, 0);
            let bg = self.bg;
            self.shadow[area - GH * w..area].fill(bg);
            self.blit_area();
            self.row = self.rows - 1;
        } else {
            // No shadow buffer: wrap to the top and clear the line.
            self.row = 0;
            self.fb.fill(0, self.top, self.fb.width, GH, self.bg);
        }
    }

    fn put_char(&mut self, ch: char) {
        if self.col >= self.cols {
            self.newline();
        }
        self.draw_cell(self.col, self.row, ch);
        self.col += 1;
    }

    fn apply_sgr(&mut self) {
        let n = self.nparams.max(1);
        for i in 0..n {
            let p = self.params[i] as usize;
            match p {
                0 => {
                    self.fg = self.default_fg;
                    self.bg = self.default_bg;
                }
                30..=37 => self.fg = self.fb.pack(PALETTE[p - 30]),
                39 => self.fg = self.default_fg,
                40..=47 => self.bg = self.fb.pack(PALETTE[p - 40]),
                49 => self.bg = self.default_bg,
                90..=97 => self.fg = self.fb.pack(PALETTE[p - 90 + 8]),
                _ => {}
            }
        }
    }

    fn byte(&mut self, b: u8) {
        match self.escape {
            Escape::Started => {
                self.escape = if b == b'[' { Escape::Csi } else { Escape::None };
                self.params = [0; 4];
                self.nparams = 0;
                return;
            }
            Escape::Csi => {
                match b {
                    b'0'..=b'9' => {
                        if self.nparams == 0 {
                            self.nparams = 1;
                        }
                        let i = self.nparams - 1;
                        if i < 4 {
                            self.params[i] = self.params[i].saturating_mul(10).saturating_add((b - b'0') as u16);
                        }
                    }
                    b';' => self.nparams = (self.nparams.max(1) + 1).min(4),
                    b'm' => {
                        self.apply_sgr();
                        self.escape = Escape::None;
                    }
                    b'J' => {
                        if self.params[0] == 2 {
                            self.clear_area();
                        }
                        self.escape = Escape::None;
                    }
                    _ => self.escape = Escape::None,
                }
                return;
            }
            Escape::None => {}
        }
        if self.utf8_left > 0 {
            if b & 0xC0 == 0x80 {
                self.utf8_value = (self.utf8_value << 6) | (b & 0x3F) as u32;
                self.utf8_left -= 1;
                if self.utf8_left == 0 {
                    self.put_char(char::from_u32(self.utf8_value).unwrap_or('?'));
                }
                return;
            }
            self.utf8_left = 0;
        }
        match b {
            0x1B => self.escape = Escape::Started,
            b'\n' => self.newline(),
            b'\r' => self.col = 0,
            0x08 => self.col = self.col.saturating_sub(1),
            b'\t' => {
                let next = (self.col / 8 + 1) * 8;
                while self.col < next.min(self.cols) {
                    self.put_char(' ');
                }
            }
            0x20..=0x7E => self.put_char(b as char),
            0xC0..=0xDF => (self.utf8_value, self.utf8_left) = ((b & 0x1F) as u32, 1),
            0xE0..=0xEF => (self.utf8_value, self.utf8_left) = ((b & 0x0F) as u32, 2),
            0xF0..=0xF7 => (self.utf8_value, self.utf8_left) = ((b & 0x07) as u32, 3),
            _ => {}
        }
    }

    fn draw_cursor(&self, visible: bool) {
        if self.col >= self.cols || self.row >= self.rows {
            return;
        }
        let color = if visible { self.fg } else { self.bg };
        self.fb.fill(self.col * GW, self.top + self.row * GH + GH - 2, GW, 2, color);
    }
}

fn draw_header(fb: &Surface) {
    let h = HEADER_HEIGHT - 2;
    for x in 0..fb.width {
        let t = (x * 256 / fb.width.max(1)) as u32;
        let mix = |a: u8, b: u8| ((a as u32 * (256 - t) + b as u32 * t) / 256) as u8;
        let c = fb.pack(Rgb(
            mix(ACCENT_LEFT.0, ACCENT_RIGHT.0),
            mix(ACCENT_LEFT.1, ACCENT_RIGHT.1),
            mix(ACCENT_LEFT.2, ACCENT_RIGHT.2),
        ));
        fb.fill(x, 0, 1, h, c);
    }
    let white = fb.pack(WHITE);
    fb.text(24, (h - 32) / 2, "Meridian OS", 2, white);
    let label = heapless_label();
    let text = label.as_str();
    let x = fb.width.saturating_sub(text.len() * GW + 24);
    fb.text(x, (h - GH) / 2, text, 1, white);
}

/// "kernel <version> (<git hash>)" without needing the heap.
fn heapless_label() -> Label {
    let mut l = Label { buf: [0; 64], len: 0 };
    for part in ["kernel ", crate::VERSION, " (", crate::GIT_HASH, ")"] {
        for &b in part.as_bytes() {
            if l.len < l.buf.len() {
                l.buf[l.len] = b;
                l.len += 1;
            }
        }
    }
    l
}

struct Label {
    buf: [u8; 64],
    len: usize,
}

impl Label {
    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

/// Sets up the console on the GOP framebuffer. Returns false when the boot
/// manager found no usable display.
pub fn init(info: &BootInfo) -> bool {
    if info.has_framebuffer == 0 {
        return false;
    }
    let f = &info.framebuffer;
    let fb = Surface {
        base: phys_to_virt(f.base) as *mut u32,
        width: f.width as usize,
        height: f.height as usize,
        stride: f.stride as usize,
        bgr: f.format == pixel_format::BGRX,
    };
    let bg = fb.pack(BACKGROUND);
    let fg = fb.pack(FOREGROUND);
    fb.fill(0, 0, fb.width, fb.height, bg);
    draw_header(&fb);

    let top = HEADER_HEIGHT + 8;
    let rows = fb.height.saturating_sub(top + 8) / GH;
    let cols = fb.width / GW;
    let mut shadow = Vec::new();
    if shadow.try_reserve_exact(fb.width * rows * GH).is_ok() {
        shadow.resize(fb.width * rows * GH, bg);
    }
    *CONSOLE.lock_at(HIGH_LEVEL) = Some(Console {
        fb,
        shadow,
        top,
        cols,
        rows,
        col: 0,
        row: 0,
        fg,
        bg,
        default_fg: fg,
        default_bg: bg,
        escape: Escape::None,
        params: [0; 4],
        nparams: 0,
        utf8_value: 0,
        utf8_left: 0,
    });
    true
}

pub fn write_bytes(bytes: &[u8]) {
    let mut guard = CONSOLE.lock_at(HIGH_LEVEL);
    if let Some(con) = guard.as_mut() {
        con.draw_cursor(false);
        for &b in bytes {
            con.byte(b);
        }
        con.draw_cursor(true);
    }
}

/// (columns, rows) of the text area, if the console exists.
pub fn size() -> Option<(usize, usize)> {
    CONSOLE.lock_at(HIGH_LEVEL).as_ref().map(|c| (c.cols, c.rows))
}

pub fn resolution() -> Option<(usize, usize)> {
    CONSOLE.lock_at(HIGH_LEVEL).as_ref().map(|c| (c.fb.width, c.fb.height))
}

/// Repaints the whole screen as the stop screen. Crash path only.
///
/// # Safety
/// Other processors must be stopped; the console lock is forcibly taken.
pub unsafe fn enter_crash_screen() {
    unsafe { CONSOLE.force_unlock() };
    let mut guard = CONSOLE.lock_at(HIGH_LEVEL);
    let Some(con) = guard.as_mut() else { return };
    let bg = con.fb.pack(CRASH_BACKGROUND);
    let fg = con.fb.pack(WHITE);
    con.fb.fill(0, 0, con.fb.width, con.fb.height, bg);
    con.fb.fill(0, 0, con.fb.width, 6, con.fb.pack(CRASH_ACCENT));
    // Draw straight to the framebuffer from here on. The shadow buffer is
    // leaked rather than freed: the heap lock may be held by the crashed code.
    core::mem::forget(core::mem::take(&mut con.shadow));
    con.top = 24;
    con.rows = (con.fb.height - 32) / GH;
    con.cols = (con.fb.width / GW).saturating_sub(2);
    con.col = 0;
    con.row = 0;
    con.fg = fg;
    con.bg = bg;
    con.default_fg = fg;
    con.default_bg = bg;
    con.escape = Escape::None;
}
