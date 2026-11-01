//! TrueType font rendering, written from the OpenType specification:
//! `cmap` (format 4), `loca`/`glyf` (simple and composite glyphs),
//! `hmtx`/`hhea` metrics, and an anti-aliasing scanline rasteriser that
//! accumulates exact signed area coverage per pixel. Rendered glyphs are
//! cached per size and weight.

use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;

use crate::gfx::{Canvas, Color, math, with_alpha};

pub struct Font {
    data: &'static [u8],
    cmap: usize,
    loca: usize,
    glyf: usize,
    hmtx: usize,
    num_hmetrics: u16,
    num_glyphs: u16,
    units_per_em: f32,
    ascender: f32,
    descender: f32,
    long_loca: bool,
}

fn u16_at(d: &[u8], o: usize) -> u16 {
    d.get(o..o + 2).map_or(0, |b| u16::from_be_bytes([b[0], b[1]]))
}
fn i16_at(d: &[u8], o: usize) -> i16 {
    u16_at(d, o) as i16
}
fn u32_at(d: &[u8], o: usize) -> u32 {
    d.get(o..o + 4).map_or(0, |b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

#[derive(Clone, Copy)]
struct Pt {
    x: f32,
    y: f32,
    on: bool,
}

impl Font {
    pub fn parse(data: &'static [u8]) -> Option<Self> {
        let tables = u16_at(data, 4) as usize;
        let find = |tag: &[u8; 4]| {
            (0..tables).find_map(|i| {
                let rec = 12 + i * 16;
                (data.get(rec..rec + 4)? == tag).then(|| u32_at(data, rec + 8) as usize)
            })
        };
        let head = find(b"head")?;
        let hhea = find(b"hhea")?;
        let maxp = find(b"maxp")?;
        let cmap_table = find(b"cmap")?;
        let loca = find(b"loca")?;
        let glyf = find(b"glyf")?;
        let hmtx = find(b"hmtx")?;
        // Windows Unicode BMP subtable (platform 3, encoding 1), format 4.
        let subtables = u16_at(data, cmap_table + 2) as usize;
        let cmap = (0..subtables).find_map(|i| {
            let rec = cmap_table + 4 + i * 8;
            let offset = cmap_table + u32_at(data, rec + 4) as usize;
            (u16_at(data, rec) == 3 && u16_at(data, rec + 2) == 1 && u16_at(data, offset) == 4).then_some(offset)
        })?;
        Some(Self {
            data,
            cmap,
            loca,
            glyf,
            hmtx,
            num_hmetrics: u16_at(data, hhea + 34),
            num_glyphs: u16_at(data, maxp + 4),
            units_per_em: u16_at(data, head + 18).max(1) as f32,
            ascender: i16_at(data, hhea + 4) as f32,
            descender: i16_at(data, hhea + 6) as f32,
            long_loca: i16_at(data, head + 50) != 0,
        })
    }

    fn glyph_index(&self, ch: char) -> u16 {
        let c = ch as u32;
        if c > 0xFFFF {
            return 0;
        }
        let d = self.data;
        let seg_count = u16_at(d, self.cmap + 6) as usize / 2;
        let ends = self.cmap + 14;
        let starts = ends + seg_count * 2 + 2;
        let deltas = starts + seg_count * 2;
        let ranges = deltas + seg_count * 2;
        for i in 0..seg_count {
            if c > u16_at(d, ends + i * 2) as u32 {
                continue;
            }
            let start = u16_at(d, starts + i * 2) as u32;
            if c < start {
                return 0;
            }
            let delta = u16_at(d, deltas + i * 2) as u32;
            let range = u16_at(d, ranges + i * 2) as usize;
            if range == 0 {
                return ((c + delta) & 0xFFFF) as u16;
            }
            let at = ranges + i * 2 + range + (c - start) as usize * 2;
            let g = u16_at(d, at) as u32;
            return if g == 0 { 0 } else { ((g + delta) & 0xFFFF) as u16 };
        }
        0
    }

    fn advance(&self, gid: u16) -> f32 {
        let i = gid.min(self.num_hmetrics.saturating_sub(1)) as usize;
        u16_at(self.data, self.hmtx + i * 4) as f32
    }

    fn glyph_range(&self, gid: u16) -> Option<(usize, usize)> {
        if gid >= self.num_glyphs {
            return None;
        }
        let (a, b) = if self.long_loca {
            (
                u32_at(self.data, self.loca + gid as usize * 4) as usize,
                u32_at(self.data, self.loca + gid as usize * 4 + 4) as usize,
            )
        } else {
            (
                u16_at(self.data, self.loca + gid as usize * 2) as usize * 2,
                u16_at(self.data, self.loca + gid as usize * 2 + 2) as usize * 2,
            )
        };
        (b > a).then(|| (self.glyf + a, self.glyf + b))
    }

    /// Appends the glyph's contours (font units, y up) to `out`.
    fn outline(&self, gid: u16, transform: [f32; 6], depth: u32, out: &mut Vec<Vec<Pt>>) {
        let Some((start, end)) = self.glyph_range(gid) else { return };
        let d = &self.data[..end.min(self.data.len())];
        let contours = i16_at(d, start);
        if contours >= 0 {
            let n = contours as usize;
            let ends: Vec<usize> = (0..n).map(|i| u16_at(d, start + 10 + i * 2) as usize).collect();
            let points = ends.last().map_or(0, |e| e + 1);
            if points > 4096 {
                return;
            }
            let mut p = start + 10 + n * 2;
            p += 2 + u16_at(d, p) as usize; // skip instructions
            let mut flags = Vec::with_capacity(points);
            while flags.len() < points && p < d.len() {
                let f = d[p];
                p += 1;
                flags.push(f);
                if f & 8 != 0 && p < d.len() {
                    let repeat = d[p];
                    p += 1;
                    for _ in 0..repeat {
                        flags.push(f);
                    }
                }
            }
            flags.truncate(points);
            let mut read_coords = |short: u8, same: u8| -> Vec<f32> {
                let mut v = 0i32;
                flags
                    .iter()
                    .map(|&f| {
                        if f & short != 0 {
                            let b = *d.get(p).unwrap_or(&0) as i32;
                            p += 1;
                            v += if f & same != 0 { b } else { -b };
                        } else if f & same == 0 {
                            v += i16_at(d, p) as i32;
                            p += 2;
                        }
                        v as f32
                    })
                    .collect()
            };
            let xs = read_coords(0x02, 0x10);
            let ys = read_coords(0x04, 0x20);
            let [a, b, c, dd, e, f] = transform;
            let mut first = 0;
            for &last in &ends {
                if last < first || last >= flags.len() {
                    break;
                }
                out.push(
                    (first..=last)
                        .map(|i| Pt {
                            x: a * xs[i] + c * ys[i] + e,
                            y: b * xs[i] + dd * ys[i] + f,
                            on: flags[i] & 1 != 0,
                        })
                        .collect(),
                );
                first = last + 1;
            }
        } else if depth < 4 {
            let mut p = start + 10;
            loop {
                let flags = u16_at(d, p);
                let child = u16_at(d, p + 2);
                p += 4;
                let (dx, dy) = if flags & 1 != 0 {
                    let v = (i16_at(d, p) as f32, i16_at(d, p + 2) as f32);
                    p += 4;
                    v
                } else {
                    let v =
                        (d.get(p).copied().unwrap_or(0) as i8 as f32, d.get(p + 1).copied().unwrap_or(0) as i8 as f32);
                    p += 2;
                    v
                };
                let f2dot14 = |o: usize| i16_at(d, o) as f32 / 16384.0;
                let (mut xa, mut xb, mut xc, mut xd) = (1.0, 0.0, 0.0, 1.0);
                if flags & 0x08 != 0 {
                    xa = f2dot14(p);
                    xd = xa;
                    p += 2;
                } else if flags & 0x40 != 0 {
                    xa = f2dot14(p);
                    xd = f2dot14(p + 2);
                    p += 4;
                } else if flags & 0x80 != 0 {
                    xa = f2dot14(p);
                    xb = f2dot14(p + 2);
                    xc = f2dot14(p + 4);
                    xd = f2dot14(p + 6);
                    p += 8;
                }
                let [a, b, c, dd, e, f] = transform;
                let (ox, oy) = if flags & 2 != 0 { (dx, dy) } else { (0.0, 0.0) };
                let combined = [
                    a * xa + c * xb,
                    b * xa + dd * xb,
                    a * xc + c * xd,
                    b * xc + dd * xd,
                    a * ox + c * oy + e,
                    b * ox + dd * oy + f,
                ];
                self.outline(child, combined, depth + 1, out);
                if flags & 0x20 == 0 {
                    break;
                }
            }
        }
    }
}

/// A rasterised glyph: coverage bitmap plus placement relative to the pen.
pub struct Glyph {
    pub w: i32,
    pub h: i32,
    pub left: i32,
    pub top: i32,
    pub advance: f32,
    pub coverage: Vec<u8>,
}

struct Raster {
    w: usize,
    h: usize,
    acc: Vec<f32>,
}

impl Raster {
    fn add(&mut self, i: isize, v: f32) {
        if i >= 0 && (i as usize) < self.acc.len() {
            self.acc[i as usize] += v;
        }
    }

    fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32) {
        if math::abs(y0 - y1) <= 1e-6 {
            return;
        }
        let (dir, (x0, y0), (x1, y1)) = if y0 < y1 { (1.0, (x0, y0), (x1, y1)) } else { (-1.0, (x1, y1), (x0, y0)) };
        let dxdy = (x1 - x0) / (y1 - y0);
        let mut x = x0;
        let ystart = if y0 < 0.0 {
            x -= y0 * dxdy;
            0
        } else {
            y0 as usize
        };
        let yend = (math::ceil(y1) as usize).min(self.h);
        let w = self.w as isize;
        for y in ystart..yend {
            let line = y as isize * w;
            let dy = ((y + 1) as f32).min(y1) - (y as f32).max(y0);
            let xnext = x + dxdy * dy;
            let d = dy * dir;
            let (xa, xb) = if x < xnext { (x, xnext) } else { (xnext, x) };
            let xa_floor = math::floor(xa);
            let xai = xa_floor as isize;
            let xb_ceil = math::ceil(xb);
            let xbi = xb_ceil as isize;
            if xbi <= xai + 1 {
                let xmf = 0.5 * (x + xnext) - xa_floor;
                self.add(line + xai, d - d * xmf);
                self.add(line + xai + 1, d * xmf);
            } else {
                let s = 1.0 / (xb - xa);
                let xaf = xa - xa_floor;
                let a0 = 0.5 * s * (1.0 - xaf) * (1.0 - xaf);
                let xbf = xb - xb_ceil + 1.0;
                let am = 0.5 * s * xbf * xbf;
                self.add(line + xai, d * a0);
                if xbi == xai + 2 {
                    self.add(line + xai + 1, d * (1.0 - a0 - am));
                } else {
                    let a1 = s * (1.5 - xaf);
                    self.add(line + xai + 1, d * (a1 - a0));
                    for xi in xai + 2..xbi - 1 {
                        self.add(line + xi, d * s);
                    }
                    let a2 = a1 + (xbi - xai - 3) as f32 * s;
                    self.add(line + xbi - 1, d * (1.0 - a2 - am));
                }
                self.add(line + xbi, d * am);
            }
            x = xnext;
        }
    }

    fn quad(&mut self, p0: (f32, f32), p1: (f32, f32), p2: (f32, f32)) {
        let dev = math::abs(p0.0 - 2.0 * p1.0 + p2.0) + math::abs(p0.1 - 2.0 * p1.1 + p2.1);
        let n = (math::sqrt(dev * 2.0) as usize).clamp(1, 16);
        let mut prev = p0;
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let mt = 1.0 - t;
            let p = (
                mt * mt * p0.0 + 2.0 * mt * t * p1.0 + t * t * p2.0,
                mt * mt * p0.1 + 2.0 * mt * t * p1.1 + t * t * p2.1,
            );
            self.line(prev.0, prev.1, p.0, p.1);
            prev = p;
        }
    }
}

const WEIGHTS: usize = 2;

pub struct Fonts {
    faces: [Font; WEIGHTS],
    cache: BTreeMap<(u8, u16, u32), Glyph>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Weight {
    Regular = 0,
    Semibold = 1,
}

impl Fonts {
    pub fn new(regular: Font, semibold: Font) -> Self {
        Self { faces: [regular, semibold], cache: BTreeMap::new() }
    }

    pub fn ascent(&self, size: u16) -> i32 {
        let f = &self.faces[0];
        math::ceil(f.ascender * size as f32 / f.units_per_em) as i32
    }

    pub fn line_height(&self, size: u16) -> i32 {
        let f = &self.faces[0];
        math::ceil((f.ascender - f.descender) * size as f32 / f.units_per_em) as i32
    }

    fn glyph(&mut self, weight: Weight, size: u16, ch: char) -> &Glyph {
        let key = (weight as u8, size, ch as u32);
        if !self.cache.contains_key(&key) {
            let g = render(&self.faces[weight as usize], size as f32, ch);
            self.cache.insert(key, g);
        }
        &self.cache[&key]
    }

    pub fn measure(&mut self, s: &str, size: u16, weight: Weight) -> i32 {
        let mut pen = 0.0;
        for ch in s.chars() {
            pen += self.glyph(weight, size, ch).advance;
        }
        math::ceil(pen) as i32
    }

    /// Draws `s` with its top-left at (x, y); returns the width drawn.
    pub fn draw(&mut self, cv: &mut Canvas, x: i32, y: i32, s: &str, size: u16, weight: Weight, color: Color) -> i32 {
        let baseline = y + self.ascent(size);
        let mut pen = x as f32;
        let clip = cv.clip();
        let alpha = (color >> 24) as u32;
        for ch in s.chars() {
            let gx = math::floor(pen + 0.5) as i32;
            let g = self.glyph(weight, size, ch);
            let ox = gx + g.left;
            let oy = baseline + g.top;
            if ox < clip.right() && ox + g.w > clip.x && oy < clip.bottom() && oy + g.h > clip.y {
                for row in 0..g.h {
                    for col in 0..g.w {
                        let c = g.coverage[(row * g.w + col) as usize] as u32;
                        if c > 0 {
                            cv.put(ox + col, oy + row, with_alpha(color, c * alpha / 255));
                        }
                    }
                }
            }
            pen += g.advance;
        }
        math::ceil(pen) as i32 - x
    }

    /// Draws `s`, cutting it with an ellipsis to fit `max_w`.
    pub fn draw_fit(
        &mut self,
        cv: &mut Canvas,
        x: i32,
        y: i32,
        s: &str,
        max_w: i32,
        size: u16,
        weight: Weight,
        color: Color,
    ) {
        if self.measure(s, size, weight) <= max_w {
            self.draw(cv, x, y, s, size, weight, color);
            return;
        }
        let mut cut = alloc::string::String::new();
        for ch in s.chars() {
            cut.push(ch);
            if self.measure(&cut, size, weight) + self.measure("...", size, weight) > max_w {
                cut.pop();
                break;
            }
        }
        cut.push_str("...");
        self.draw(cv, x, y, &cut, size, weight, color);
    }
}

fn render(font: &Font, px: f32, ch: char) -> Glyph {
    let gid = font.glyph_index(ch);
    let scale = px / font.units_per_em;
    let advance = font.advance(gid) * scale;
    let mut contours = Vec::new();
    font.outline(gid, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0], 0, &mut contours);
    let pts = contours.iter().flatten();
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for p in pts {
        min_x = min_x.min(p.x);
        max_x = max_x.max(p.x);
        min_y = min_y.min(p.y);
        max_y = max_y.max(p.y);
    }
    if contours.is_empty() || min_x > max_x {
        return Glyph { w: 0, h: 0, left: 0, top: 0, advance, coverage: Vec::new() };
    }
    let left = math::floor(min_x * scale) as i32;
    let top = math::floor(-max_y * scale) as i32;
    let w = (math::ceil(max_x * scale) as i32 - left + 1).clamp(1, 256);
    let h = (math::ceil(-min_y * scale) as i32 - top + 1).clamp(1, 256);
    let mut r = Raster { w: (w + 2) as usize, h: h as usize, acc: vec![0.0; ((w + 2) * h + 4) as usize] };
    let tx = |p: &Pt| (p.x * scale - left as f32, -p.y * scale - top as f32);
    for c in &contours {
        let n = c.len();
        if n < 2 {
            continue;
        }
        // Start from an on-curve point (or the midpoint of two off-curve ones).
        let start = c.iter().position(|p| p.on);
        let (mut prev, first_index) = match start {
            Some(i) => (tx(&c[i]), i),
            None => {
                let (a, b) = (tx(&c[n - 1]), tx(&c[0]));
                (((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0), n - 1)
            }
        };
        let origin = prev;
        let mut control: Option<(f32, f32)> = None;
        for k in 1..=n {
            let p = &c[(first_index + k) % n];
            let q = tx(p);
            if p.on {
                match control.take() {
                    Some(ctl) => r.quad(prev, ctl, q),
                    None => r.line(prev.0, prev.1, q.0, q.1),
                }
                prev = q;
            } else {
                if let Some(ctl) = control {
                    let mid = ((ctl.0 + q.0) / 2.0, (ctl.1 + q.1) / 2.0);
                    r.quad(prev, ctl, mid);
                    prev = mid;
                }
                control = Some(q);
            }
        }
        match control {
            Some(ctl) => r.quad(prev, ctl, origin),
            None => r.line(prev.0, prev.1, origin.0, origin.1),
        }
    }
    let mut coverage = vec![0u8; (w * h) as usize];
    for y in 0..h as usize {
        let mut acc = 0.0;
        for x in 0..r.w {
            acc += r.acc[y * r.w + x];
            if x < w as usize {
                coverage[y * w as usize + x] = (math::abs(acc).min(1.0) * 255.0) as u8;
            }
        }
    }
    Glyph { w, h, left, top, advance, coverage }
}

/// The 8x16 bitmap font (shared with the kernel console) for the terminal.
pub struct Mono {
    bits: &'static [u8; 4096],
}

pub const MONO_W: i32 = 8;
pub const MONO_H: i32 = 16;

impl Mono {
    pub const fn new(bits: &'static [u8; 4096]) -> Self {
        Self { bits }
    }

    pub fn draw(&self, cv: &mut Canvas, x: i32, y: i32, s: &str, color: Color) {
        for (i, ch) in s.chars().enumerate() {
            let code = if (ch as u32) < 256 { ch as usize } else { b'?' as usize };
            for (row, bits) in self.bits[code * 16..code * 16 + 16].iter().enumerate() {
                for col in 0..8 {
                    if bits & (0x80 >> col) != 0 {
                        cv.put(x + i as i32 * MONO_W + col, y + row as i32, color);
                    }
                }
            }
        }
    }
}
