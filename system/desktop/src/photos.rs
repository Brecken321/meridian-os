//! Photos: a gallery of the pictures in a folder (D:\Pictures by default)
//! and a viewer with next/previous, fit/actual size and "Edit in Paint".

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use meridian_rt::sys::{self, InputEvent, key};

use crate::apps::{Action, App, AppKind, Ctx, error_text, extension, file_name, human_size, join, parent};
use crate::bmp;
use crate::font::Weight;
use crate::gfx::{Canvas, Rect, rgb};
use crate::ui::{self, Icon};

const BAR_H: i32 = 48;
const THUMB_W: i32 = 180;
const THUMB_H: i32 = 130;

pub struct Photos {
    folder: String,
    files: Vec<(String, u64)>,
    thumbs: Vec<Option<Canvas>>,
    current: Option<usize>,
    image: Option<Result<Canvas, String>>,
    actual_size: bool,
    scroll: i32,
    w: i32,
    h: i32,
}

impl Photos {
    pub fn new(path: Option<String>) -> Self {
        let folder = path.as_deref().map(parent).unwrap_or_else(|| "D:\\Pictures".to_string());
        let mut p = Self {
            folder,
            files: Vec::new(),
            thumbs: Vec::new(),
            current: None,
            image: None,
            actual_size: false,
            scroll: 0,
            w: 920,
            h: 600,
        };
        p.scan();
        if let Some(path) = path {
            let name = file_name(&path).to_string();
            if let Some(i) = p.files.iter().position(|(n, _)| n.eq_ignore_ascii_case(&name)) {
                p.show(i);
            }
        }
        p
    }

    fn scan(&mut self) {
        let mut files: Vec<(String, u64)> = sys::list_directory(&self.folder)
            .map(|l| {
                l.into_iter().filter(|e| !e.is_dir && extension(&e.name) == "bmp").map(|e| (e.name, e.size)).collect()
            })
            .unwrap_or_default();
        files.sort_by_key(|f| f.0.to_lowercase());
        self.thumbs = vec![None; files.len()];
        self.files = files;
        if self.current.is_some_and(|c| c >= self.files.len()) {
            self.current = None;
        }
    }

    fn path(&self, i: usize) -> String {
        join(&self.folder, &self.files[i].0)
    }

    fn load(&self, i: usize) -> Result<Canvas, String> {
        let data = sys::read_file(&self.path(i)).map_err(|e| error_text(e).to_string())?;
        bmp::decode(&data).map_err(String::from)
    }

    fn show(&mut self, i: usize) {
        self.current = Some(i);
        self.image = Some(self.load(i));
        self.actual_size = false;
    }

    fn step(&mut self, d: i32) {
        if let Some(c) = self.current {
            let n = self.files.len() as i32;
            if n > 0 {
                self.show(((c as i32 + d).rem_euclid(n)) as usize);
            }
        }
    }

    fn columns(&self) -> i32 {
        ((self.w - 24) / (THUMB_W + 16)).max(1)
    }

    fn thumb_rect(&self, i: usize) -> Rect {
        let cols = self.columns();
        Rect::new(
            16 + (i as i32 % cols) * (THUMB_W + 16),
            BAR_H + 16 + (i as i32 / cols) * (THUMB_H + 44) - self.scroll,
            THUMB_W,
            THUMB_H,
        )
    }

    fn bar_buttons(&self) -> Vec<(Rect, &'static str)> {
        if self.current.is_some() {
            vec![
                (Rect::new(8, 8, 92, 32), "All photos"),
                (Rect::new(108, 8, 40, 32), "<"),
                (Rect::new(152, 8, 40, 32), ">"),
                (Rect::new(200, 8, 100, 32), if self.actual_size { "Fit" } else { "Actual size" }),
                (Rect::new(self.w - 128, 8, 120, 32), "Edit in Paint"),
            ]
        } else {
            vec![(Rect::new(self.w - 108, 8, 100, 32), "Refresh")]
        }
    }
}

/// Fits `w`x`h` into `area`, keeping the aspect ratio (never enlarging
/// beyond 4x).
fn fit(w: i32, h: i32, area: Rect) -> Rect {
    let scale_w = area.w as f32 / w as f32;
    let scale_h = area.h as f32 / h as f32;
    let s = scale_w.min(scale_h).min(4.0);
    let (dw, dh) = ((w as f32 * s) as i32, (h as f32 * s) as i32);
    Rect::new(area.x + (area.w - dw) / 2, area.y + (area.h - dh) / 2, dw.max(1), dh.max(1))
}

impl App for Photos {
    fn kind(&self) -> AppKind {
        AppKind::Photos
    }
    fn title(&self) -> String {
        match self.current {
            Some(i) => format!("{} - Photos", self.files[i].0),
            None => "Photos".into(),
        }
    }
    fn size(&self) -> (i32, i32) {
        (920, 600)
    }
    fn min_size(&self) -> (i32, i32) {
        (520, 360)
    }
    fn files_changed(&mut self) -> bool {
        let current = self.current.map(|i| self.files[i].0.clone());
        self.scan();
        if let Some(name) = current {
            self.current = self.files.iter().position(|f| f.0 == name);
            if let Some(i) = self.current {
                self.image = Some(self.load(i));
            }
        }
        true
    }
    fn wheel(&mut self, notches: i32) -> bool {
        if self.current.is_some() {
            self.step(notches.signum());
            return true;
        }
        let cols = self.columns();
        let rows = (self.files.len() as i32 + cols - 1) / cols;
        let max = (rows * (THUMB_H + 44) + BAR_H + 16 - self.h).max(0);
        let old = self.scroll;
        self.scroll = (self.scroll + notches * 60).clamp(0, max);
        old != self.scroll
    }

    fn draw(&mut self, cv: &mut Canvas, r: Rect, cx: &mut Ctx) {
        let t = cx.theme;
        let f = &mut *cx.fonts;
        self.w = r.w;
        self.h = r.h;
        let viewing = self.current.is_some();
        cv.fill(r, if viewing { rgb(0x16, 0x16, 0x16) } else { t.window() });
        // Top bar.
        let bar = Rect::new(r.x, r.y, r.w, BAR_H);
        cv.fill(bar, if viewing { rgb(0x20, 0x20, 0x20) } else { t.window() });
        let theme = if viewing { crate::ui::Theme { dark: true, ..t } } else { t };
        for (b, label) in self.bar_buttons() {
            ui::button(cv, f, &theme, b.offset(r.x, r.y), label, label == "Edit in Paint", false);
        }
        match self.current {
            Some(i) => {
                let area = Rect::new(r.x + 8, r.y + BAR_H + 8, r.w - 16, r.h - BAR_H - 44);
                match &self.image {
                    Some(Ok(img)) => {
                        let dst = if self.actual_size {
                            Rect::new(area.x + (area.w - img.w) / 2, area.y + (area.h - img.h) / 2, img.w, img.h)
                        } else {
                            fit(img.w, img.h, area)
                        };
                        let old = cv.clip();
                        cv.set_clip(area.intersect(&old).unwrap_or(area));
                        cv.blit_scaled(img, dst);
                        cv.set_clip(old);
                        let info = format!(
                            "{}     {} x {} pixels     {}     {} of {}",
                            self.files[i].0,
                            img.w,
                            img.h,
                            human_size(self.files[i].1),
                            i + 1,
                            self.files.len()
                        );
                        f.draw_fit(
                            cv,
                            r.x + 16,
                            r.bottom() - 30,
                            &info,
                            r.w - 32,
                            12,
                            Weight::Regular,
                            rgb(0xC8, 0xC8, 0xC8),
                        );
                    }
                    Some(Err(e)) => {
                        let msg = format!("Cannot show {}: {e}", self.files[i].0);
                        f.draw(cv, area.x + 20, area.y + 20, &msg, 14, Weight::Regular, rgb(0xF8, 0x71, 0x71));
                    }
                    None => {}
                }
            }
            None => {
                f.draw(cv, r.x + 16, r.y + 12, &self.folder, 20, Weight::Semibold, t.text());
                if self.files.is_empty() {
                    ui::icon(cv, Icon::Photos, r.x + r.w / 2 - 40, r.y + 150, 80, &t);
                    for (k, line) in
                        ["No pictures here yet.", "Pictures you save in Paint (.bmp) appear in this gallery."]
                            .iter()
                            .enumerate()
                    {
                        let w = f.measure(line, 14, Weight::Regular);
                        f.draw(
                            cv,
                            r.x + (r.w - w) / 2,
                            r.y + 250 + k as i32 * 24,
                            line,
                            14,
                            Weight::Regular,
                            t.text_dim(),
                        );
                    }
                }
                let old = cv.clip();
                let grid = Rect::new(r.x, r.y + BAR_H, r.w, r.h - BAR_H);
                cv.set_clip(grid.intersect(&old).unwrap_or(grid));
                for i in 0..self.files.len() {
                    let tr = self.thumb_rect(i).offset(r.x, r.y);
                    if tr.bottom() + 40 < grid.y || tr.y > grid.bottom() {
                        continue;
                    }
                    if self.thumbs[i].is_none() {
                        let thumb = match self.load(i) {
                            Ok(img) => {
                                let d = fit(img.w, img.h, Rect::new(0, 0, THUMB_W, THUMB_H));
                                let mut c = Canvas::new(d.w, d.h, 0);
                                c.blit_scaled(&img, Rect::new(0, 0, d.w, d.h));
                                c
                            }
                            Err(_) => Canvas::new(1, 1, rgb(0x80, 0x80, 0x80)),
                        };
                        self.thumbs[i] = Some(thumb);
                    }
                    cv.rounded_box(tr, 6, t.border(), t.surface());
                    if let Some(th) = &self.thumbs[i] {
                        let d = fit(th.w, th.h, tr.inset(6));
                        cv.blit(th, th.bounds(), d.x, d.y);
                    }
                    f.draw_fit(
                        cv,
                        tr.x + 2,
                        tr.bottom() + 8,
                        &self.files[i].0,
                        THUMB_W - 4,
                        12,
                        Weight::Regular,
                        t.text(),
                    );
                }
                cv.set_clip(old);
            }
        }
    }

    fn click(&mut self, x: i32, y: i32, double: bool, _cx: &mut Ctx) -> Vec<Action> {
        if let Some((_, label)) = self.bar_buttons().into_iter().find(|(b, _)| b.contains(x, y)) {
            match label {
                "All photos" => {
                    self.current = None;
                    self.image = None;
                }
                "<" => self.step(-1),
                ">" => self.step(1),
                "Fit" | "Actual size" => self.actual_size = !self.actual_size,
                "Edit in Paint" => {
                    if let Some(i) = self.current {
                        return vec![Action::Open(AppKind::Paint, Some(self.path(i)))];
                    }
                }
                _ => {
                    self.scan();
                }
            }
            return Vec::new();
        }
        if self.current.is_none() && y > BAR_H {
            if let Some(i) = (0..self.files.len()).find(|&i| self.thumb_rect(i).grow(4).contains(x, y)) {
                self.show(i);
            }
        } else if self.current.is_some() && double {
            self.actual_size = !self.actual_size;
        }
        Vec::new()
    }

    fn key(&mut self, e: &InputEvent, _cx: &mut Ctx) -> Vec<Action> {
        match e.key {
            key::LEFT => self.step(-1),
            key::RIGHT => self.step(1),
            key::ESCAPE | key::BACKSPACE => {
                self.current = None;
                self.image = None;
            }
            _ => {}
        }
        Vec::new()
    }
}
