//! Software 2D graphics: rectangles, rounded shapes with anti-aliased
//! edges, alpha blending, gradients and soft shadows on a 32-bit canvas.
//!
//! Colours are 0xAARRGGBB; canvases hold opaque pixels (alpha byte used only
//! for window corner masks when compositing).

use alloc::vec;
use alloc::vec::Vec;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }
    pub fn right(&self) -> i32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }
    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }
    pub fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && py >= self.y && px < self.right() && py < self.bottom()
    }
    pub fn intersect(&self, o: &Rect) -> Option<Rect> {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        let r = self.right().min(o.right());
        let b = self.bottom().min(o.bottom());
        (r > x && b > y).then(|| Rect::new(x, y, r - x, b - y))
    }
    pub fn union(&self, o: &Rect) -> Rect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        let x = self.x.min(o.x);
        let y = self.y.min(o.y);
        Rect::new(x, y, self.right().max(o.right()) - x, self.bottom().max(o.bottom()) - y)
    }
    pub fn inset(&self, d: i32) -> Rect {
        Rect::new(self.x + d, self.y + d, self.w - 2 * d, self.h - 2 * d)
    }
    pub fn grow(&self, d: i32) -> Rect {
        self.inset(-d)
    }
    pub fn offset(&self, dx: i32, dy: i32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w, self.h)
    }
}

pub type Color = u32;

pub const fn rgb(r: u32, g: u32, b: u32) -> Color {
    0xFF00_0000 | r << 16 | g << 8 | b
}

pub const fn with_alpha(c: Color, a: u32) -> Color {
    (c & 0x00FF_FFFF) | (if a > 255 { 255 } else { a }) << 24
}

#[inline]
fn mix_channel(d: u32, s: u32, a: u32) -> u32 {
    let v = s * a + d * (255 - a) + 128;
    (v + (v >> 8)) >> 8
}

/// Blends `src` (with its alpha) over the opaque `dst`.
#[inline]
pub fn blend(dst: u32, src: Color) -> u32 {
    let a = src >> 24;
    if a == 255 {
        return src;
    }
    if a == 0 {
        return dst;
    }
    let r = mix_channel((dst >> 16) & 0xFF, (src >> 16) & 0xFF, a);
    let g = mix_channel((dst >> 8) & 0xFF, (src >> 8) & 0xFF, a);
    let b = mix_channel(dst & 0xFF, src & 0xFF, a);
    0xFF00_0000 | r << 16 | g << 8 | b
}

/// Linear interpolation between two colours (`t` 0..=255).
pub fn lerp(a: Color, b: Color, t: u32) -> Color {
    let t = t.min(255);
    let ch = |s: u32| mix_channel((a >> s) & 0xFF, (b >> s) & 0xFF, t) << s;
    ch(24) | ch(16) | ch(8) | ch(0)
}

/// Lightens (positive) or darkens (negative) a colour.
pub fn shade(c: Color, amount: i32) -> Color {
    if amount >= 0 {
        lerp(c, rgb(255, 255, 255), amount as u32) | (c & 0xFF00_0000)
    } else {
        lerp(c, rgb(0, 0, 0), (-amount) as u32) | (c & 0xFF00_0000)
    }
}

pub mod math {
    #[inline]
    pub fn sqrt(x: f32) -> f32 {
        let mut v = x.max(0.0);
        // SAFETY: SSE2 is part of the x86-64 baseline.
        unsafe { core::arch::asm!("sqrtss {0}, {0}", inout(xmm_reg) v, options(nomem, nostack, pure)) };
        v
    }
    #[inline]
    pub fn floor(x: f32) -> f32 {
        let t = x as i64 as f32;
        if t > x { t - 1.0 } else { t }
    }
    #[inline]
    pub fn ceil(x: f32) -> f32 {
        let t = x as i64 as f32;
        if t < x { t + 1.0 } else { t }
    }
    #[inline]
    pub fn abs(x: f32) -> f32 {
        if x < 0.0 { -x } else { x }
    }
}

pub struct Canvas {
    pub w: i32,
    pub h: i32,
    pub px: Vec<u32>,
    clip: Rect,
}

impl Canvas {
    pub fn new(w: i32, h: i32, fill: Color) -> Self {
        Self { w, h, px: vec![fill; (w.max(0) * h.max(0)) as usize], clip: Rect::new(0, 0, w, h) }
    }

    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.w, self.h)
    }

    pub fn set_clip(&mut self, r: Rect) {
        self.clip = r.intersect(&self.bounds()).unwrap_or_default();
    }

    pub fn reset_clip(&mut self) {
        self.clip = self.bounds();
    }

    pub fn clip(&self) -> Rect {
        self.clip
    }

    #[inline]
    pub fn put(&mut self, x: i32, y: i32, c: Color) {
        if self.clip.contains(x, y) {
            let i = (y * self.w + x) as usize;
            self.px[i] = blend(self.px[i], c);
        }
    }

    pub fn fill(&mut self, r: Rect, c: Color) {
        let Some(r) = r.intersect(&self.clip) else { return };
        let opaque = c >> 24 == 255;
        for y in r.y..r.bottom() {
            let row = &mut self.px[(y * self.w + r.x) as usize..(y * self.w + r.right()) as usize];
            if opaque {
                row.fill(c);
            } else {
                for p in row {
                    *p = blend(*p, c);
                }
            }
        }
    }

    /// Fills a rounded rectangle with anti-aliased corners.
    pub fn fill_rounded(&mut self, r: Rect, radius: i32, c: Color) {
        let radius = radius.min(r.w / 2).min(r.h / 2).max(0);
        if radius == 0 {
            self.fill(r, c);
            return;
        }
        self.fill(Rect::new(r.x, r.y + radius, r.w, r.h - 2 * radius), c);
        self.fill(Rect::new(r.x + radius, r.y, r.w - 2 * radius, radius), c);
        self.fill(Rect::new(r.x + radius, r.bottom() - radius, r.w - 2 * radius, radius), c);
        let rf = radius as f32;
        let a = (c >> 24) as f32;
        for dy in 0..radius {
            for dx in 0..radius {
                let fx = rf - dx as f32 - 0.5;
                let fy = rf - dy as f32 - 0.5;
                let cov = (rf - math::sqrt(fx * fx + fy * fy) + 0.5).clamp(0.0, 1.0);
                if cov <= 0.0 {
                    continue;
                }
                let col = with_alpha(c, (a * cov) as u32);
                self.put(r.x + dx, r.y + dy, col);
                self.put(r.right() - 1 - dx, r.y + dy, col);
                self.put(r.x + dx, r.bottom() - 1 - dy, col);
                self.put(r.right() - 1 - dx, r.bottom() - 1 - dy, col);
            }
        }
    }

    /// A rounded rectangle with a 1-pixel border.
    pub fn rounded_box(&mut self, r: Rect, radius: i32, border: Color, fill: Color) {
        self.fill_rounded(r, radius, border);
        self.fill_rounded(r.inset(1), (radius - 1).max(0), fill);
    }

    pub fn stroke(&mut self, r: Rect, c: Color) {
        self.fill(Rect::new(r.x, r.y, r.w, 1), c);
        self.fill(Rect::new(r.x, r.bottom() - 1, r.w, 1), c);
        self.fill(Rect::new(r.x, r.y, 1, r.h), c);
        self.fill(Rect::new(r.right() - 1, r.y, 1, r.h), c);
    }

    /// Anti-aliased filled circle.
    pub fn circle(&mut self, cx: f32, cy: f32, radius: f32, c: Color) {
        let a = (c >> 24) as f32;
        let x0 = math::floor(cx - radius - 1.0) as i32;
        let x1 = math::ceil(cx + radius + 1.0) as i32;
        let y0 = math::floor(cy - radius - 1.0) as i32;
        let y1 = math::ceil(cy + radius + 1.0) as i32;
        for y in y0..y1 {
            for x in x0..x1 {
                let dx = x as f32 + 0.5 - cx;
                let dy = y as f32 + 0.5 - cy;
                let cov = (radius - math::sqrt(dx * dx + dy * dy) + 0.5).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.put(x, y, with_alpha(c, (a * cov) as u32));
                }
            }
        }
    }

    /// Anti-aliased ring (circle outline) of the given thickness.
    pub fn ring(&mut self, cx: f32, cy: f32, radius: f32, thickness: f32, c: Color) {
        let a = (c >> 24) as f32;
        let outer = radius + thickness / 2.0;
        let x0 = math::floor(cx - outer - 1.0) as i32;
        let x1 = math::ceil(cx + outer + 1.0) as i32;
        let y0 = math::floor(cy - outer - 1.0) as i32;
        let y1 = math::ceil(cy + outer + 1.0) as i32;
        for y in y0..y1 {
            for x in x0..x1 {
                let dx = x as f32 + 0.5 - cx;
                let dy = y as f32 + 0.5 - cy;
                let d = math::abs(math::sqrt(dx * dx + dy * dy) - radius);
                let cov = (thickness / 2.0 - d + 0.5).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.put(x, y, with_alpha(c, (a * cov) as u32));
                }
            }
        }
    }

    /// A thick anti-aliased line segment (round caps).
    pub fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, c: Color) {
        let a = (c >> 24) as f32;
        let (minx, maxx) = (x0.min(x1) - width, x0.max(x1) + width);
        let (miny, maxy) = (y0.min(y1) - width, y0.max(y1) + width);
        let (vx, vy) = (x1 - x0, y1 - y0);
        let len2 = (vx * vx + vy * vy).max(1e-6);
        for y in math::floor(miny) as i32..math::ceil(maxy) as i32 {
            for x in math::floor(minx) as i32..math::ceil(maxx) as i32 {
                let (px, py) = (x as f32 + 0.5 - x0, y as f32 + 0.5 - y0);
                let t = ((px * vx + py * vy) / len2).clamp(0.0, 1.0);
                let (dx, dy) = (px - vx * t, py - vy * t);
                let cov = (width / 2.0 - math::sqrt(dx * dx + dy * dy) + 0.5).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.put(x, y, with_alpha(c, (a * cov) as u32));
                }
            }
        }
    }

    pub fn gradient_v(&mut self, r: Rect, top: Color, bottom: Color) {
        let Some(clip) = r.intersect(&self.clip) else { return };
        for y in clip.y..clip.bottom() {
            let t = ((y - r.y) * 255 / r.h.max(1)) as u32;
            self.fill(Rect::new(clip.x, y, clip.w, 1), lerp(top, bottom, t));
        }
    }

    /// Soft drop shadow around `r` (drawn before the window itself).
    /// Soft shadow of `r` moved down by `drop`; pixels that `r` itself
    /// will cover (apart from its rounded corners) are skipped.
    pub fn shadow(&mut self, r: Rect, drop: i32, radius: i32, spread: i32, strength: u32) {
        let hole = r;
        let r = r.offset(0, drop);
        let area = r.grow(spread);
        let Some(clip) = area.intersect(&self.clip) else { return };
        let sf = spread as f32;
        for y in clip.y..clip.bottom() {
            for x in clip.x..clip.right() {
                let corner = (x < hole.x + radius || x >= hole.right() - radius)
                    && (y < hole.y + radius || y >= hole.bottom() - radius);
                if hole.contains(x, y) && !corner {
                    continue;
                }
                // Distance from the (rounded) rectangle edge.
                let dx = (r.x + radius - x).max(x - (r.right() - 1 - radius)).max(0) as f32;
                let dy = (r.y + radius - y).max(y - (r.bottom() - 1 - radius)).max(0) as f32;
                let d = math::sqrt(dx * dx + dy * dy) - radius as f32;
                let t = (1.0 - d.max(0.0) / sf).clamp(0.0, 1.0);
                let alpha = (strength as f32 * t * t) as u32;
                if alpha > 0 {
                    let i = (y * self.w + x) as usize;
                    self.px[i] = blend(self.px[i], with_alpha(0, alpha));
                }
            }
        }
    }

    /// Copies `src_rect` of `src` to (`dx`, `dy`), blending pixels whose
    /// alpha is below 255 (window corners).
    pub fn blit(&mut self, src: &Canvas, src_rect: Rect, dx: i32, dy: i32) {
        let dst_rect = Rect::new(dx, dy, src_rect.w, src_rect.h);
        let Some(vis) = dst_rect.intersect(&self.clip) else { return };
        for y in vis.y..vis.bottom() {
            let sy = src_rect.y + (y - dy);
            if sy < 0 || sy >= src.h {
                continue;
            }
            for x in vis.x..vis.right() {
                let sx = src_rect.x + (x - dx);
                if sx < 0 || sx >= src.w {
                    continue;
                }
                let s = src.px[(sy * src.w + sx) as usize];
                let i = (y * self.w + x) as usize;
                self.px[i] = if s >> 24 == 255 { s } else { blend(self.px[i], s) };
            }
        }
    }

    /// Makes the pixels outside a rounded rectangle of this canvas's size
    /// transparent (anti-aliased), for rounded window corners.
    pub fn round_corners(&mut self, radius: i32) {
        let rf = radius as f32;
        for dy in 0..radius {
            for dx in 0..radius {
                let fx = rf - dx as f32 - 0.5;
                let fy = rf - dy as f32 - 0.5;
                let cov = (rf - math::sqrt(fx * fx + fy * fy) + 0.5).clamp(0.0, 1.0);
                let a = (cov * 255.0) as u32;
                for (x, y) in
                    [(dx, dy), (self.w - 1 - dx, dy), (dx, self.h - 1 - dy), (self.w - 1 - dx, self.h - 1 - dy)]
                {
                    if x >= 0 && y >= 0 && x < self.w && y < self.h {
                        let i = (y * self.w + x) as usize;
                        self.px[i] = with_alpha(self.px[i], a.min(self.px[i] >> 24));
                    }
                }
            }
        }
    }
}
