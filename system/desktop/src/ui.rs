//! Look and feel: themes, accent colours, wallpapers and icons. Every icon
//! and wallpaper is drawn from simple shapes at run time (no bundled
//! artwork).

use crate::font::{Fonts, Weight};
use crate::gfx::{Canvas, Color, Rect, lerp, math, rgb, shade, with_alpha};

pub const ACCENTS: [(&str, Color); 6] = [
    ("Meridian blue", rgb(0x25, 0x63, 0xEB)),
    ("Teal", rgb(0x0D, 0x94, 0x88)),
    ("Violet", rgb(0x7C, 0x3A, 0xED)),
    ("Rose", rgb(0xE1, 0x1D, 0x48)),
    ("Amber", rgb(0xD9, 0x77, 0x06)),
    ("Green", rgb(0x16, 0xA3, 0x4A)),
];

pub const WALLPAPERS: [&str; 3] = ["Meridian", "Aurora", "Graphite"];

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub dark: bool,
    pub accent: Color,
    pub wallpaper: usize,
    pub taskbar_left: bool,
}

impl Theme {
    pub fn window(&self) -> Color {
        if self.dark { rgb(0x20, 0x20, 0x20) } else { rgb(0xF3, 0xF3, 0xF3) }
    }
    pub fn surface(&self) -> Color {
        if self.dark { rgb(0x2B, 0x2B, 0x2B) } else { rgb(0xFF, 0xFF, 0xFF) }
    }
    pub fn text(&self) -> Color {
        if self.dark { rgb(0xFF, 0xFF, 0xFF) } else { rgb(0x1A, 0x1A, 0x1A) }
    }
    pub fn text_dim(&self) -> Color {
        if self.dark { rgb(0xA8, 0xA8, 0xA8) } else { rgb(0x5C, 0x5C, 0x5C) }
    }
    pub fn border(&self) -> Color {
        if self.dark { rgb(0x3C, 0x3C, 0x3C) } else { rgb(0xDE, 0xDE, 0xDE) }
    }
    pub fn hover(&self) -> Color {
        if self.dark { with_alpha(rgb(255, 255, 255), 22) } else { with_alpha(rgb(0, 0, 0), 14) }
    }
    pub fn pressed(&self) -> Color {
        if self.dark { with_alpha(rgb(255, 255, 255), 34) } else { with_alpha(rgb(0, 0, 0), 26) }
    }
    pub fn selection(&self) -> Color {
        with_alpha(self.accent, if self.dark { 90 } else { 60 })
    }
    pub fn title(&self, focused: bool) -> Color {
        match (self.dark, focused) {
            (true, true) => rgb(0x2A, 0x2A, 0x2A),
            (true, false) => rgb(0x24, 0x24, 0x24),
            (false, true) => rgb(0xE9, 0xEE, 0xF6),
            (false, false) => rgb(0xF0, 0xF0, 0xF0),
        }
    }
    /// Translucent bar/menu backgrounds (blended over the wallpaper).
    pub fn taskbar(&self) -> Color {
        if self.dark { with_alpha(rgb(0x1C, 0x1C, 0x1C), 240) } else { with_alpha(rgb(0xEE, 0xEE, 0xEE), 240) }
    }
    pub fn menu(&self) -> Color {
        if self.dark { rgb(0x26, 0x26, 0x26) } else { rgb(0xF6, 0xF6, 0xF6) }
    }
    pub fn accent_text(&self) -> Color {
        rgb(255, 255, 255)
    }
}

/// Draws a button; returns nothing (hit testing uses the same rect).
pub fn button(cv: &mut Canvas, f: &mut Fonts, t: &Theme, r: Rect, label: &str, primary: bool, hovered: bool) {
    let (bg, fg) = if primary {
        (if hovered { shade(t.accent, 25) } else { t.accent }, t.accent_text())
    } else {
        let base = if t.dark { rgb(0x37, 0x37, 0x37) } else { rgb(0xFB, 0xFB, 0xFB) };
        (if hovered { shade(base, if t.dark { 18 } else { -10 }) } else { base }, t.text())
    };
    cv.rounded_box(r, 5, if primary { bg } else { t.border() }, bg);
    let tw = f.measure(label, 14, Weight::Regular);
    let th = f.line_height(14);
    f.draw(cv, r.x + (r.w - tw) / 2, r.y + (r.h - th) / 2, label, 14, Weight::Regular, fg);
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Icon {
    Start,
    Explorer,
    Terminal,
    Notepad,
    TaskManager,
    Settings,
    About,
    Folder,
    File,
    TextFile,
    Program,
    ThisPc,
    Drive,
    Power,
    Restart,
}

/// Draws `icon` in a `size` x `size` box at (x, y).
pub fn icon(cv: &mut Canvas, icon: Icon, x: i32, y: i32, size: i32, t: &Theme) {
    let s = size as f32;
    let fx = x as f32;
    let fy = y as f32;
    let px = |v: f32| (v * s) as i32;
    match icon {
        Icon::Start => {
            let r = Rect::new(x + px(0.08), y + px(0.08), px(0.84), px(0.84));
            cv.fill_rounded(r, px(0.2), t.accent);
            let (cx, cy, rr) = (fx + s * 0.5, fy + s * 0.5, s * 0.26);
            cv.ring(cx, cy, rr, s * 0.07, rgb(255, 255, 255));
            cv.line(cx, cy - rr, cx, cy + rr, s * 0.07, rgb(255, 255, 255));
            cv.line(cx - rr, cy, cx + rr, cy, s * 0.05, with_alpha(rgb(255, 255, 255), 200));
        }
        Icon::Explorer | Icon::Folder => {
            let back = rgb(0xE0, 0xA0, 0x2A);
            let front = rgb(0xF7, 0xC9, 0x48);
            cv.fill_rounded(Rect::new(x + px(0.06), y + px(0.16), px(0.4), px(0.2)), px(0.06), back);
            cv.fill_rounded(Rect::new(x + px(0.06), y + px(0.24), px(0.88), px(0.6)), px(0.08), back);
            cv.fill_rounded(Rect::new(x + px(0.06), y + px(0.34), px(0.88), px(0.52)), px(0.08), front);
            if icon == Icon::Explorer {
                cv.fill(Rect::new(x + px(0.06), y + px(0.34), px(0.88), px(0.06).max(1)), with_alpha(t.accent, 200));
            }
        }
        Icon::File | Icon::TextFile => {
            let page = Rect::new(x + px(0.2), y + px(0.08), px(0.6), px(0.84));
            cv.rounded_box(page, px(0.06), rgb(0xB8, 0xB8, 0xB8), rgb(0xFF, 0xFF, 0xFF));
            if icon == Icon::TextFile {
                for i in 0..4 {
                    let w = if i == 3 { 0.24 } else { 0.38 };
                    cv.fill(
                        Rect::new(x + px(0.31), y + px(0.3 + 0.14 * i as f32), px(w), px(0.04).max(1)),
                        rgb(0x8A, 0x8A, 0x8A),
                    );
                }
            }
        }
        Icon::Notepad => {
            let page = Rect::new(x + px(0.16), y + px(0.1), px(0.68), px(0.8));
            cv.rounded_box(page, px(0.08), rgb(0x2D, 0x7D, 0xD2), rgb(0xFF, 0xFF, 0xFF));
            cv.fill(Rect::new(page.x, page.y, page.w, px(0.14)), rgb(0x2D, 0x7D, 0xD2));
            for i in 0..3 {
                cv.fill(
                    Rect::new(x + px(0.28), y + px(0.42 + 0.14 * i as f32), px(0.44), px(0.04).max(1)),
                    rgb(0x9A, 0x9A, 0x9A),
                );
            }
        }
        Icon::Program => {
            let win = Rect::new(x + px(0.1), y + px(0.18), px(0.8), px(0.64));
            cv.rounded_box(win, px(0.08), shade(t.accent, -30), rgb(0xFF, 0xFF, 0xFF));
            cv.fill_rounded(Rect::new(win.x, win.y, win.w, px(0.18)), px(0.08), t.accent);
        }
        Icon::Terminal => {
            let r = Rect::new(x + px(0.08), y + px(0.14), px(0.84), px(0.72));
            cv.fill_rounded(r, px(0.1), rgb(0x1E, 0x29, 0x3B));
            let (l, top) = (fx + s * 0.24, fy + s * 0.36);
            cv.line(l, top, l + s * 0.16, top + s * 0.13, s * 0.07, rgb(0xE2, 0xE8, 0xF0));
            cv.line(l + s * 0.16, top + s * 0.13, l, top + s * 0.26, s * 0.07, rgb(0xE2, 0xE8, 0xF0));
            cv.fill(Rect::new(x + px(0.48), y + px(0.6), px(0.24), px(0.07).max(1)), rgb(0x4A, 0xDE, 0x80));
        }
        Icon::TaskManager => {
            let r = Rect::new(x + px(0.08), y + px(0.12), px(0.84), px(0.76));
            cv.rounded_box(
                r,
                px(0.1),
                rgb(0x9C, 0xA3, 0xAF),
                if t.dark { rgb(0x1F, 0x29, 0x37) } else { rgb(0xFF, 0xFF, 0xFF) },
            );
            let pts = [(0.18, 0.66), (0.34, 0.5), (0.46, 0.6), (0.62, 0.32), (0.82, 0.42)];
            for w in pts.windows(2) {
                cv.line(fx + s * w[0].0, fy + s * w[0].1, fx + s * w[1].0, fy + s * w[1].1, s * 0.07, t.accent);
            }
        }
        Icon::Settings => {
            let (cx, cy) = (fx + s * 0.5, fy + s * 0.5);
            let gear = rgb(0x6B, 0x72, 0x80);
            const DIRS: [(f32, f32); 8] = [
                (1.0, 0.0),
                (0.707, 0.707),
                (0.0, 1.0),
                (-0.707, 0.707),
                (-1.0, 0.0),
                (-0.707, -0.707),
                (0.0, -1.0),
                (0.707, -0.707),
            ];
            for (dx, dy) in DIRS {
                cv.circle(cx + dx * s * 0.33, cy + dy * s * 0.33, s * 0.1, gear);
            }
            cv.circle(cx, cy, s * 0.32, gear);
            cv.circle(cx, cy, s * 0.13, if t.dark { rgb(0x2B, 0x2B, 0x2B) } else { rgb(0xF3, 0xF3, 0xF3) });
        }
        Icon::About => {
            let (cx, cy) = (fx + s * 0.5, fy + s * 0.5);
            cv.circle(cx, cy, s * 0.4, t.accent);
            cv.circle(cx, cy - s * 0.17, s * 0.06, rgb(255, 255, 255));
            cv.line(cx, cy - s * 0.02, cx, cy + s * 0.22, s * 0.1, rgb(255, 255, 255));
        }
        Icon::ThisPc => {
            let screen = Rect::new(x + px(0.08), y + px(0.14), px(0.84), px(0.56));
            cv.fill_rounded(screen, px(0.06), rgb(0x37, 0x41, 0x51));
            cv.gradient_v(screen.inset(px(0.05).max(1)), shade(t.accent, 40), shade(t.accent, -40));
            cv.fill(Rect::new(x + px(0.44), y + px(0.7), px(0.12), px(0.12)), rgb(0x6B, 0x72, 0x80));
            cv.fill_rounded(
                Rect::new(x + px(0.28), y + px(0.8), px(0.44), px(0.07).max(2)),
                px(0.03),
                rgb(0x6B, 0x72, 0x80),
            );
        }
        Icon::Drive => {
            let r = Rect::new(x + px(0.08), y + px(0.3), px(0.84), px(0.4));
            cv.fill_rounded(r, px(0.08), rgb(0x9C, 0xA3, 0xAF));
            cv.fill(Rect::new(x + px(0.16), y + px(0.56), px(0.5), px(0.05).max(1)), rgb(0x4B, 0x55, 0x63));
            cv.circle(fx + s * 0.78, fy + s * 0.58, s * 0.05, rgb(0x4A, 0xDE, 0x80));
        }
        Icon::Power => {
            let (cx, cy) = (fx + s * 0.5, fy + s * 0.54);
            let c = t.text();
            cv.ring(cx, cy, s * 0.3, s * 0.08, c);
            cv.fill(
                Rect::new(x + px(0.4), y + px(0.06), px(0.2), px(0.22)),
                if t.dark { rgb(0x26, 0x26, 0x26) } else { rgb(0xF6, 0xF6, 0xF6) },
            );
            cv.line(cx, fy + s * 0.12, cx, cy, s * 0.08, c);
        }
        Icon::Restart => {
            let (cx, cy) = (fx + s * 0.5, fy + s * 0.5);
            let c = t.text();
            cv.ring(cx, cy, s * 0.3, s * 0.08, c);
            cv.fill(
                Rect::new(x + px(0.5), y + px(0.1), px(0.32), px(0.3)),
                if t.dark { rgb(0x26, 0x26, 0x26) } else { rgb(0xF6, 0xF6, 0xF6) },
            );
            cv.line(cx + s * 0.04, fy + s * 0.2, cx + s * 0.22, fy + s * 0.2, s * 0.08, c);
            cv.line(cx + s * 0.04, fy + s * 0.2, cx + s * 0.04, fy + s * 0.04, s * 0.08, c);
        }
    }
}

/// Generates a wallpaper (computed at quarter resolution and scaled up
/// smoothly; the images are pure gradients and glows).
pub fn wallpaper(w: i32, h: i32, t: &Theme) -> Canvas {
    let (sw, sh) = ((w / 4).max(2), (h / 4).max(2));
    let (top, bottom, blobs): (Color, Color, [(f32, f32, f32, Color, f32); 3]) = match (t.wallpaper, t.dark) {
        (0, true) => (
            rgb(0x08, 0x10, 0x2A),
            rgb(0x14, 0x22, 0x5C),
            [
                (0.72, 0.62, 0.55, t.accent, 0.75),
                (0.22, 0.22, 0.42, rgb(0x06, 0xB6, 0xD4), 0.45),
                (0.92, 0.12, 0.35, rgb(0x8B, 0x5C, 0xF6), 0.4),
            ],
        ),
        (0, false) => (
            rgb(0xE4, 0xEC, 0xFB),
            rgb(0xB5, 0xC9, 0xF2),
            [
                (0.72, 0.62, 0.55, t.accent, 0.45),
                (0.22, 0.22, 0.42, rgb(0x67, 0xE8, 0xF9), 0.45),
                (0.92, 0.12, 0.35, rgb(0xC4, 0xB5, 0xFD), 0.5),
            ],
        ),
        (1, true) => (
            rgb(0x04, 0x1C, 0x22),
            rgb(0x0B, 0x13, 0x2B),
            [
                (0.3, 0.35, 0.5, rgb(0x10, 0xB9, 0x81), 0.6),
                (0.75, 0.3, 0.45, rgb(0x8B, 0x5C, 0xF6), 0.55),
                (0.55, 0.85, 0.5, rgb(0x06, 0xB6, 0xD4), 0.45),
            ],
        ),
        (1, false) => (
            rgb(0xDF, 0xF7, 0xEE),
            rgb(0xE9, 0xE3, 0xFB),
            [
                (0.3, 0.35, 0.5, rgb(0x6E, 0xE7, 0xB7), 0.55),
                (0.75, 0.3, 0.45, rgb(0xC4, 0xB5, 0xFD), 0.55),
                (0.55, 0.85, 0.5, rgb(0x67, 0xE8, 0xF9), 0.45),
            ],
        ),
        (_, true) => (
            rgb(0x1A, 0x1C, 0x20),
            rgb(0x2E, 0x31, 0x38),
            [
                (0.7, 0.6, 0.6, rgb(0x4B, 0x55, 0x63), 0.5),
                (0.2, 0.3, 0.4, t.accent, 0.18),
                (0.9, 0.1, 0.3, rgb(0x6B, 0x72, 0x80), 0.3),
            ],
        ),
        (_, false) => (
            rgb(0xF2, 0xF3, 0xF5),
            rgb(0xD5, 0xD8, 0xDE),
            [
                (0.7, 0.6, 0.6, rgb(0xFF, 0xFF, 0xFF), 0.6),
                (0.2, 0.3, 0.4, t.accent, 0.15),
                (0.9, 0.1, 0.3, rgb(0xB0, 0xB6, 0xC0), 0.35),
            ],
        ),
    };
    let mut small = Canvas::new(sw, sh, 0);
    for y in 0..sh {
        let base = lerp(top, bottom, (y * 255 / (sh - 1)) as u32);
        for x in 0..sw {
            let mut c = base;
            let (fx, fy) = (x as f32 / sw as f32, y as f32 / sh as f32 * (h as f32 / w as f32));
            for &(bx, by, br, bc, strength) in &blobs {
                let by = by * (h as f32 / w as f32);
                let (dx, dy) = (fx - bx, fy - by);
                let d = math::sqrt(dx * dx + dy * dy) / br;
                if d < 1.0 {
                    let k = (1.0 - d) * (1.0 - d) * strength;
                    c = lerp(c, bc, (k * 255.0) as u32);
                }
            }
            // A soft horizon arc ("meridian") across the lower part.
            let (dx, dy) = (fx - 0.5, fy - (h as f32 / w as f32) - 0.85);
            let ring = math::abs(math::sqrt(dx * dx + dy * dy) - 1.0);
            if ring < 0.08 {
                let k = (1.0 - ring / 0.08) * if t.dark { 0.25 } else { 0.4 };
                c = lerp(c, rgb(255, 255, 255), (k * k * 255.0) as u32);
            }
            small.px[(y * sw + x) as usize] = c;
        }
    }
    let mut out = Canvas::new(w, h, 0);
    for y in 0..h {
        let sy = (y as f32 + 0.5) / 4.0 - 0.5;
        let y0 = (math::floor(sy) as i32).clamp(0, sh - 1);
        let y1 = (y0 + 1).min(sh - 1);
        let ty = ((sy - y0 as f32).clamp(0.0, 1.0) * 255.0) as u32;
        for x in 0..w {
            let sx = (x as f32 + 0.5) / 4.0 - 0.5;
            let x0 = (math::floor(sx) as i32).clamp(0, sw - 1);
            let x1 = (x0 + 1).min(sw - 1);
            let tx = ((sx - x0 as f32).clamp(0.0, 1.0) * 255.0) as u32;
            let a = lerp(small.px[(y0 * sw + x0) as usize], small.px[(y0 * sw + x1) as usize], tx);
            let b = lerp(small.px[(y1 * sw + x0) as usize], small.px[(y1 * sw + x1) as usize], tx);
            out.px[(y * w + x) as usize] = lerp(a, b, ty) | 0xFF00_0000;
        }
    }
    out
}

/// The mouse pointer: a classic arrow (1 = black outline, 2 = white fill).
pub const CURSOR: [&[u8]; 19] = [
    b"1",
    b"11",
    b"121",
    b"1221",
    b"12221",
    b"122221",
    b"1222221",
    b"12222221",
    b"122222221",
    b"1222222221",
    b"12222222221",
    b"122222211111",
    b"1222122",
    b"122112221",
    b"12  12221",
    b"1    12221",
    b"      12221",
    b"       1221",
    b"        11",
];
