//! Windows bitmap (.bmp) files: writing 24-bit images, reading
//! uncompressed 8-, 24- and 32-bit ones (the formats Paint programs save).

use alloc::vec;
use alloc::vec::Vec;

use crate::gfx::{Canvas, rgb};

const MAX_PIXELS: u64 = 32 * 1024 * 1024;

pub fn encode(cv: &Canvas) -> Vec<u8> {
    let (w, h) = (cv.w.max(0) as usize, cv.h.max(0) as usize);
    let row = (w * 3).next_multiple_of(4);
    let size = 54 + row * h;
    let mut out = vec![0u8; size];
    out[0..2].copy_from_slice(b"BM");
    out[2..6].copy_from_slice(&(size as u32).to_le_bytes());
    out[10..14].copy_from_slice(&54u32.to_le_bytes());
    out[14..18].copy_from_slice(&40u32.to_le_bytes());
    out[18..22].copy_from_slice(&(w as i32).to_le_bytes());
    out[22..26].copy_from_slice(&(h as i32).to_le_bytes());
    out[26..28].copy_from_slice(&1u16.to_le_bytes());
    out[28..30].copy_from_slice(&24u16.to_le_bytes());
    out[34..38].copy_from_slice(&((row * h) as u32).to_le_bytes());
    out[38..42].copy_from_slice(&2835u32.to_le_bytes()); // 72 DPI
    out[42..46].copy_from_slice(&2835u32.to_le_bytes());
    for y in 0..h {
        let dst = 54 + (h - 1 - y) * row;
        for x in 0..w {
            let p = cv.px[y * w + x];
            out[dst + x * 3] = p as u8;
            out[dst + x * 3 + 1] = (p >> 8) as u8;
            out[dst + x * 3 + 2] = (p >> 16) as u8;
        }
    }
    out
}

fn u16_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?) as u32)
}
fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

/// Decodes a bitmap, or explains why it cannot.
pub fn decode(data: &[u8]) -> Result<Canvas, &'static str> {
    if data.get(0..2) != Some(b"BM") {
        return Err("not a BMP file");
    }
    let bad = "the file is damaged";
    let offset = u32_at(data, 10).ok_or(bad)? as usize;
    let header = u32_at(data, 14).ok_or(bad)? as usize;
    if header < 40 {
        return Err("old-style (OS/2) bitmaps are not supported");
    }
    let w = u32_at(data, 18).ok_or(bad)? as i32;
    let raw_h = u32_at(data, 22).ok_or(bad)? as i32;
    let bpp = u16_at(data, 28).ok_or(bad)?;
    let compression = u32_at(data, 30).ok_or(bad)?;
    let (h, top_down) = if raw_h < 0 { (-raw_h, true) } else { (raw_h, false) };
    if w <= 0 || h <= 0 || w as u64 * h as u64 > MAX_PIXELS {
        return Err("the picture is empty or too large");
    }
    if !(compression == 0 || compression == 3 && bpp == 32) {
        return Err("compressed bitmaps are not supported yet");
    }
    let mut palette = Vec::new();
    if bpp == 8 {
        let colors = match u32_at(data, 46).ok_or(bad)? {
            0 => 256,
            n => n.min(256) as usize,
        };
        let base = 14 + header;
        for i in 0..colors {
            let b = data.get(base + i * 4..base + i * 4 + 3).ok_or(bad)?;
            palette.push(rgb(b[2] as u32, b[1] as u32, b[0] as u32));
        }
    } else if bpp != 24 && bpp != 32 {
        return Err("only 8-, 24- and 32-bit bitmaps are supported");
    }
    let bytes = bpp as usize / 8;
    let row = (w as usize * bytes).next_multiple_of(4);
    if data.len() < offset + row * (h as usize - 1) + w as usize * bytes {
        return Err(bad);
    }
    let mut cv = Canvas::new(w, h, rgb(255, 255, 255));
    for y in 0..h as usize {
        let src_y = if top_down { y } else { h as usize - 1 - y };
        let line = &data[offset + src_y * row..];
        for x in 0..w as usize {
            let p = &line[x * bytes..];
            cv.px[y * w as usize + x] = match bpp {
                8 => palette.get(p[0] as usize).copied().unwrap_or(0xFF00_0000),
                _ => rgb(p[2] as u32, p[1] as u32, p[0] as u32),
            };
        }
    }
    Ok(cv)
}
