//! Games: Minesweeper and 2048.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use meridian_rt::sys::{InputEvent, key};

use crate::apps::{Action, App, AppKind, Ctx, MenuItem};
use crate::font::Weight;
use crate::gfx::{Canvas, Color, Rect, rgb, shade};
use crate::ui;

/// xorshift64* seeded from the clock.
struct Rng(u64);

impl Rng {
    fn new() -> Self {
        Self(meridian_rt::uptime_us() | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn now_ms() -> u64 {
    meridian_rt::uptime_us() / 1000
}

// ---------------------------------------------------------------------------
// Minesweeper
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Ready,
    Playing,
    Won,
    Lost,
}

const LEVELS: [(&str, usize, usize, usize); 3] =
    [("Beginner", 9, 9, 10), ("Intermediate", 16, 16, 40), ("Expert", 30, 16, 99)];

pub struct Mines {
    level: usize,
    cols: usize,
    rows: usize,
    mines: usize,
    mine: Vec<bool>,
    open: Vec<bool>,
    flag: Vec<bool>,
    state: State,
    started: u64,
    finished: u64,
    exploded: Option<usize>,
    rng: Rng,
    w: i32,
}

const CELL: i32 = 26;
const TOP: i32 = 64;

impl Mines {
    pub fn new() -> Self {
        let mut m = Self {
            level: 0,
            cols: 9,
            rows: 9,
            mines: 10,
            mine: Vec::new(),
            open: Vec::new(),
            flag: Vec::new(),
            state: State::Ready,
            started: 0,
            finished: 0,
            exploded: None,
            rng: Rng::new(),
            w: 840,
        };
        m.reset();
        m
    }

    fn reset(&mut self) {
        let (_, c, r, n) = LEVELS[self.level];
        (self.cols, self.rows, self.mines) = (c, r, n);
        self.mine = vec![false; c * r];
        self.open = vec![false; c * r];
        self.flag = vec![false; c * r];
        self.state = State::Ready;
        self.exploded = None;
    }

    /// Places mines away from the first cell clicked (and its neighbours).
    fn place(&mut self, safe: usize) {
        let near = self.neighbours(safe);
        let mut placed = 0;
        while placed < self.mines {
            let i = self.rng.below(self.cols * self.rows);
            if i != safe && !near.contains(&i) && !self.mine[i] {
                self.mine[i] = true;
                placed += 1;
            }
        }
    }

    fn neighbours(&self, i: usize) -> Vec<usize> {
        let (x, y) = ((i % self.cols) as i32, (i / self.cols) as i32);
        let mut out = Vec::new();
        for dy in -1..=1 {
            for dx in -1..=1 {
                let (nx, ny) = (x + dx, y + dy);
                if (dx, dy) != (0, 0) && nx >= 0 && ny >= 0 && (nx as usize) < self.cols && (ny as usize) < self.rows {
                    out.push(ny as usize * self.cols + nx as usize);
                }
            }
        }
        out
    }

    fn count(&self, i: usize) -> usize {
        self.neighbours(i).iter().filter(|&&n| self.mine[n]).count()
    }

    fn reveal(&mut self, i: usize) {
        if self.open[i] || self.flag[i] {
            return;
        }
        if self.state == State::Ready {
            self.place(i);
            self.state = State::Playing;
            self.started = now_ms();
        }
        if self.mine[i] {
            self.exploded = Some(i);
            self.state = State::Lost;
            self.finished = now_ms();
            for k in 0..self.mine.len() {
                if self.mine[k] {
                    self.open[k] = true;
                }
            }
            return;
        }
        let mut stack = vec![i];
        while let Some(c) = stack.pop() {
            if self.open[c] || self.flag[c] {
                continue;
            }
            self.open[c] = true;
            if self.count(c) == 0 {
                stack.extend(self.neighbours(c));
            }
        }
        let closed = self.open.iter().filter(|o| !**o).count();
        if closed == self.mines {
            self.state = State::Won;
            self.finished = now_ms();
            for k in 0..self.mine.len() {
                self.flag[k] = self.mine[k];
            }
        }
    }

    /// Opening a number with that many flags around it opens the rest.
    fn chord(&mut self, i: usize) {
        let n = self.neighbours(i);
        if n.iter().filter(|&&k| self.flag[k]).count() == self.count(i) {
            for k in n {
                if self.state == State::Playing {
                    self.reveal(k);
                }
            }
        }
    }

    fn board(&self, w: i32) -> Rect {
        let bw = self.cols as i32 * CELL;
        Rect::new((w - bw) / 2, TOP + 8, bw, self.rows as i32 * CELL)
    }

    fn cell_at(&self, w: i32, x: i32, y: i32) -> Option<usize> {
        let b = self.board(w);
        if !b.contains(x, y) {
            return None;
        }
        Some(((y - b.y) / CELL) as usize * self.cols + ((x - b.x) / CELL) as usize)
    }

    fn elapsed(&self) -> u64 {
        match self.state {
            State::Ready => 0,
            State::Playing => (now_ms() - self.started) / 1000,
            _ => (self.finished - self.started) / 1000,
        }
    }
}

const NUMBER_COLORS: [Color; 8] = [
    rgb(0x25, 0x63, 0xEB),
    rgb(0x16, 0xA3, 0x4A),
    rgb(0xDC, 0x26, 0x26),
    rgb(0x7C, 0x3A, 0xED),
    rgb(0xB4, 0x53, 0x09),
    rgb(0x0D, 0x94, 0x88),
    rgb(0x11, 0x18, 0x27),
    rgb(0x6B, 0x72, 0x80),
];

impl App for Mines {
    fn kind(&self) -> AppKind {
        AppKind::Mines
    }
    fn title(&self) -> String {
        "Minesweeper".into()
    }
    fn size(&self) -> (i32, i32) {
        (840, 560)
    }
    fn min_size(&self) -> (i32, i32) {
        (820, 540)
    }
    fn tick(&mut self) -> bool {
        self.state == State::Playing
    }

    fn draw(&mut self, cv: &mut Canvas, r: Rect, cx: &mut Ctx) {
        let t = cx.theme;
        let f = &mut *cx.fonts;
        self.w = r.w;
        cv.fill(r, t.window());
        // Header: level buttons, counters, status.
        for (i, (name, ..)) in LEVELS.iter().enumerate() {
            let b = Rect::new(r.x + 12 + i as i32 * 116, r.y + 14, 108, 34);
            ui::button(cv, f, &t, b, name, self.level == i, false);
        }
        let flags = self.flag.iter().filter(|f| **f).count();
        let left = self.mines as i32 - flags as i32;
        let info = format!("Mines: {left}     Time: {}", self.elapsed());
        let iw = f.measure(&info, 14, Weight::Semibold);
        f.draw(cv, r.right() - iw - 140, r.y + 22, &info, 14, Weight::Semibold, t.text());
        let face = Rect::new(r.right() - 116, r.y + 14, 104, 34);
        let label = match self.state {
            State::Won => "You won!",
            State::Lost => "Try again",
            _ => "New game",
        };
        ui::button(cv, f, &t, face, label, self.state == State::Won, false);
        // Board.
        let b = self.board(r.w).offset(r.x, r.y);
        for i in 0..self.mine.len() {
            let cell = Rect::new(b.x + (i % self.cols) as i32 * CELL, b.y + (i / self.cols) as i32 * CELL, CELL, CELL);
            let inner = cell.inset(1);
            if self.open[i] {
                let bg = if Some(i) == self.exploded {
                    rgb(0xEF, 0x44, 0x44)
                } else if t.dark {
                    rgb(0x2A, 0x2A, 0x2A)
                } else {
                    rgb(0xE5, 0xE7, 0xEB)
                };
                cv.fill(inner, bg);
                if self.mine[i] {
                    let (mx, my) = (cell.x as f32 + 13.0, cell.y as f32 + 13.0);
                    cv.circle(mx, my, 6.0, rgb(0x11, 0x18, 0x27));
                    cv.line(mx - 9.0, my, mx + 9.0, my, 1.6, rgb(0x11, 0x18, 0x27));
                    cv.line(mx, my - 9.0, mx, my + 9.0, 1.6, rgb(0x11, 0x18, 0x27));
                } else {
                    let n = self.count(i);
                    if n > 0 {
                        let s = format!("{n}");
                        let w = f.measure(&s, 16, Weight::Semibold);
                        let c = if t.dark { shade(NUMBER_COLORS[n - 1], 60) } else { NUMBER_COLORS[n - 1] };
                        f.draw(cv, cell.x + (CELL - w) / 2, cell.y + 3, &s, 16, Weight::Semibold, c);
                    }
                }
            } else {
                let top = if t.dark { rgb(0x4B, 0x55, 0x63) } else { rgb(0x9C, 0xA3, 0xAF) };
                cv.fill_rounded(inner, 3, top);
                cv.fill(Rect::new(inner.x + 2, inner.y + 1, inner.w - 4, 2), shade(top, 30));
                if self.flag[i] {
                    let (fx, fy) = (cell.x as f32 + 10.0, cell.y as f32 + 6.0);
                    cv.line(fx, fy, fx, fy + 15.0, 1.6, rgb(0x11, 0x18, 0x27));
                    for k in 0..6 {
                        cv.line(fx, fy + k as f32, fx + 9.0 - k as f32 * 1.4, fy + 3.5, 1.2, rgb(0xDC, 0x26, 0x26));
                    }
                }
            }
        }
        let hint = "Left-click to open a square, right-click to flag a mine. Double-click a number to open around it.";
        let hw = f.measure(hint, 12, Weight::Regular);
        f.draw(cv, r.x + (r.w - hw) / 2, b.bottom() + 14, hint, 12, Weight::Regular, t.text_dim());
    }

    fn click(&mut self, x: i32, y: i32, double: bool, _cx: &mut Ctx) -> Vec<Action> {
        if (14..48).contains(&y) && (12..12 + 3 * 116).contains(&x) {
            self.level = ((x - 12) / 116) as usize;
            self.reset();
            return Vec::new();
        }
        if Rect::new(self.w - 116, 14, 104, 34).contains(x, y) {
            self.reset();
            return Vec::new();
        }
        if matches!(self.state, State::Won | State::Lost) {
            return Vec::new();
        }
        if let Some(i) = self.cell_at(self.w, x, y) {
            if self.open[i] && double {
                self.chord(i);
            } else {
                self.reveal(i);
            }
        }
        Vec::new()
    }

    fn right_click(&mut self, x: i32, y: i32, _cx: &mut Ctx) -> Vec<Action> {
        if matches!(self.state, State::Won | State::Lost) {
            return Vec::new();
        }
        if let Some(i) = self.cell_at(self.w, x, y) {
            if !self.open[i] {
                self.flag[i] = !self.flag[i];
            }
        }
        Vec::new()
    }

    fn key(&mut self, e: &InputEvent, _cx: &mut Ctx) -> Vec<Action> {
        if e.key == key::CHAR && (e.ch == 'n' as u32 || e.ch == 'N' as u32) || e.key == key::FUNCTION + 2 {
            self.reset();
        }
        Vec::new()
    }
}

// ---------------------------------------------------------------------------
// 2048
// ---------------------------------------------------------------------------

pub struct Puzzle {
    grid: [[u32; 4]; 4],
    score: u32,
    best: u32,
    won: bool,
    keep_going: bool,
    over: bool,
    rng: Rng,
    w: i32,
}

impl Puzzle {
    pub fn new() -> Self {
        let mut p = Self {
            grid: [[0; 4]; 4],
            score: 0,
            best: 0,
            won: false,
            keep_going: false,
            over: false,
            rng: Rng::new(),
            w: 480,
        };
        p.restart();
        p
    }

    fn restart(&mut self) {
        self.grid = [[0; 4]; 4];
        self.score = 0;
        self.won = false;
        self.keep_going = false;
        self.over = false;
        self.spawn();
        self.spawn();
    }

    fn spawn(&mut self) {
        let empty: Vec<(usize, usize)> =
            (0..16).map(|i| (i / 4, i % 4)).filter(|&(r, c)| self.grid[r][c] == 0).collect();
        if !empty.is_empty() {
            let (r, c) = empty[self.rng.below(empty.len())];
            self.grid[r][c] = if self.rng.below(10) == 0 { 4 } else { 2 };
        }
    }

    /// Slides one row towards index 0, merging equal neighbours once.
    fn slide(line: [u32; 4]) -> ([u32; 4], u32) {
        let tiles: Vec<u32> = line.iter().copied().filter(|&v| v != 0).collect();
        let mut out = [0u32; 4];
        let (mut i, mut k, mut gained) = (0, 0, 0);
        while i < tiles.len() {
            if i + 1 < tiles.len() && tiles[i] == tiles[i + 1] {
                out[k] = tiles[i] * 2;
                gained += out[k];
                i += 2;
            } else {
                out[k] = tiles[i];
                i += 1;
            }
            k += 1;
        }
        (out, gained)
    }

    /// dir: 0 left, 1 right, 2 up, 3 down.
    fn shift(&mut self, dir: u8) {
        if self.over || (self.won && !self.keep_going) {
            return;
        }
        let before = self.grid;
        for i in 0..4 {
            let line: [u32; 4] = match dir {
                0 => self.grid[i],
                1 => [self.grid[i][3], self.grid[i][2], self.grid[i][1], self.grid[i][0]],
                2 => [self.grid[0][i], self.grid[1][i], self.grid[2][i], self.grid[3][i]],
                _ => [self.grid[3][i], self.grid[2][i], self.grid[1][i], self.grid[0][i]],
            };
            let (new, gained) = Self::slide(line);
            self.score += gained;
            for k in 0..4 {
                match dir {
                    0 => self.grid[i][k] = new[k],
                    1 => self.grid[i][3 - k] = new[k],
                    2 => self.grid[k][i] = new[k],
                    _ => self.grid[3 - k][i] = new[k],
                }
            }
        }
        if self.grid != before {
            self.spawn();
        }
        self.best = self.best.max(self.score);
        if !self.won && self.grid.iter().flatten().any(|&v| v >= 2048) {
            self.won = true;
        }
        let can_move = self.grid.iter().flatten().any(|&v| v == 0)
            || (0..4).any(|r| {
                (0..3).any(|c| self.grid[r][c] == self.grid[r][c + 1] || self.grid[c][r] == self.grid[c + 1][r])
            });
        self.over = !can_move;
    }
}

fn tile_color(v: u32) -> (Color, Color) {
    let dark_text = rgb(0x77, 0x6E, 0x65);
    let light = rgb(0xF9, 0xF6, 0xF2);
    match v {
        2 => (rgb(0xEE, 0xE4, 0xDA), dark_text),
        4 => (rgb(0xED, 0xE0, 0xC8), dark_text),
        8 => (rgb(0xF2, 0xB1, 0x79), light),
        16 => (rgb(0xF5, 0x95, 0x63), light),
        32 => (rgb(0xF6, 0x7C, 0x5F), light),
        64 => (rgb(0xF6, 0x5E, 0x3B), light),
        128 => (rgb(0xED, 0xCF, 0x72), light),
        256 => (rgb(0xED, 0xCC, 0x61), light),
        512 => (rgb(0xED, 0xC8, 0x50), light),
        1024 => (rgb(0xED, 0xC5, 0x3F), light),
        2048 => (rgb(0xED, 0xC2, 0x2E), light),
        _ => (rgb(0x3C, 0x3A, 0x32), light),
    }
}

impl App for Puzzle {
    fn kind(&self) -> AppKind {
        AppKind::Puzzle
    }
    fn title(&self) -> String {
        "2048".into()
    }
    fn size(&self) -> (i32, i32) {
        (480, 600)
    }
    fn min_size(&self) -> (i32, i32) {
        (460, 580)
    }

    fn draw(&mut self, cv: &mut Canvas, r: Rect, cx: &mut Ctx) {
        let t = cx.theme;
        let f = &mut *cx.fonts;
        self.w = r.w;
        cv.fill(r, if t.dark { rgb(0x1E, 0x1C, 0x19) } else { rgb(0xFA, 0xF8, 0xEF) });
        f.draw(
            cv,
            r.x + 24,
            r.y + 14,
            "2048",
            44,
            Weight::Semibold,
            if t.dark { rgb(0xED, 0xE0, 0xC8) } else { rgb(0x77, 0x6E, 0x65) },
        );
        for (k, (label, v)) in [("SCORE", self.score), ("BEST", self.best)].iter().enumerate() {
            let b = Rect::new(r.right() - 196 + k as i32 * 92, r.y + 16, 84, 52);
            cv.fill_rounded(b, 6, rgb(0xBB, 0xAD, 0xA0));
            let lw = f.measure(label, 12, Weight::Semibold);
            f.draw(cv, b.x + (b.w - lw) / 2, b.y + 6, label, 12, Weight::Semibold, rgb(0xEE, 0xE4, 0xDA));
            let s = format!("{v}");
            let sw = f.measure(&s, 20, Weight::Semibold);
            f.draw(cv, b.x + (b.w - sw) / 2, b.y + 22, &s, 20, Weight::Semibold, rgb(255, 255, 255));
        }
        f.draw(
            cv,
            r.x + 24,
            r.y + 80,
            "Join the tiles to reach 2048. Use the arrow keys.",
            12,
            Weight::Regular,
            t.text_dim(),
        );
        let nb = Rect::new(r.right() - 132, r.y + 76, 108, 30);
        ui::button(cv, f, &t, nb, "New game", true, false);
        let size = (r.w - 48).min(r.h - 140);
        let board = Rect::new(r.x + (r.w - size) / 2, r.y + 120, size, size);
        cv.fill_rounded(board, 8, rgb(0xBB, 0xAD, 0xA0));
        let gap = 12;
        let cell = (size - 5 * gap) / 4;
        for row in 0..4 {
            for col in 0..4 {
                let v = self.grid[row][col];
                let tr = Rect::new(
                    board.x + gap + col as i32 * (cell + gap),
                    board.y + gap + row as i32 * (cell + gap),
                    cell,
                    cell,
                );
                if v == 0 {
                    cv.fill_rounded(tr, 6, rgb(0xCD, 0xC1, 0xB4));
                    continue;
                }
                let (bg, fg) = tile_color(v);
                cv.fill_rounded(tr, 6, bg);
                let s = format!("{v}");
                let mut sz = 40u16;
                while sz > 16 && f.measure(&s, sz, Weight::Semibold) > cell - 12 {
                    sz -= 4;
                }
                let sw = f.measure(&s, sz, Weight::Semibold);
                let sh = f.line_height(sz);
                f.draw(cv, tr.x + (cell - sw) / 2, tr.y + (cell - sh) / 2, &s, sz, Weight::Semibold, fg);
            }
        }
        if self.over || (self.won && !self.keep_going) {
            cv.fill_rounded(board, 8, if self.won { 0x80ED_C22E } else { 0xA0EE_E4DA });
            let msg = if self.won { "You win!" } else { "Game over!" };
            let mw = f.measure(msg, 40, Weight::Semibold);
            f.draw(
                cv,
                board.x + (size - mw) / 2,
                board.y + size / 2 - 60,
                msg,
                40,
                Weight::Semibold,
                rgb(0x77, 0x6E, 0x65),
            );
            let sub =
                if self.won { "Click to keep going, or start a new game" } else { "Start a new game to try again" };
            let sw = f.measure(sub, 14, Weight::Regular);
            f.draw(cv, board.x + (size - sw) / 2, board.y + size / 2, sub, 14, Weight::Regular, rgb(0x77, 0x6E, 0x65));
        }
    }

    fn click(&mut self, x: i32, y: i32, _double: bool, _cx: &mut Ctx) -> Vec<Action> {
        if Rect::new(self.w - 132, 76, 108, 30).contains(x, y) {
            self.restart();
        } else if self.won && !self.keep_going && y > 120 {
            self.keep_going = true;
        }
        Vec::new()
    }

    fn right_click(&mut self, x: i32, y: i32, _cx: &mut Ctx) -> Vec<Action> {
        vec![Action::Menu(vec![MenuItem::new("New game", 1)], x, y)]
    }

    fn menu(&mut self, id: u32, _cx: &mut Ctx) -> Vec<Action> {
        if id == 1 {
            self.restart();
        }
        Vec::new()
    }

    fn key(&mut self, e: &InputEvent, _cx: &mut Ctx) -> Vec<Action> {
        let dir = match e.key {
            key::LEFT => 0,
            key::RIGHT => 1,
            key::UP => 2,
            key::DOWN => 3,
            key::CHAR => match char::from_u32(e.ch).unwrap_or(' ').to_ascii_lowercase() {
                'a' => 0,
                'd' => 1,
                'w' => 2,
                's' => 3,
                'n' => {
                    self.restart();
                    return Vec::new();
                }
                _ => return Vec::new(),
            },
            _ => return Vec::new(),
        };
        self.shift(dir);
        Vec::new()
    }
}
