//! Kernel log. Every message goes to COM1, the framebuffer console (once it
//! exists) and a 64 KiB in-memory ring buffer that the shell's `log` command
//! and the console replay read back.

use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, Ordering};

use crate::drivers::fbcon;
use crate::hal::serial;
use crate::ke::clock;
use crate::ke::irql::HIGH_LEVEL;
use crate::sync::SpinLock;

const RING_SIZE: usize = 64 * 1024;

struct Ring {
    buf: [u8; RING_SIZE],
    head: usize,
    len: usize,
}

impl Ring {
    fn push(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.buf[self.head] = b;
            self.head = (self.head + 1) % RING_SIZE;
            self.len = (self.len + 1).min(RING_SIZE);
        }
    }

    /// Calls `f` with the buffered bytes, oldest first (in up to two pieces).
    fn for_each(&self, mut f: impl FnMut(&[u8])) {
        let start = (self.head + RING_SIZE - self.len) % RING_SIZE;
        if start + self.len <= RING_SIZE {
            f(&self.buf[start..start + self.len]);
        } else {
            f(&self.buf[start..]);
            f(&self.buf[..self.head]);
        }
    }
}

static LOG: SpinLock<Ring> = SpinLock::new(Ring { buf: [0; RING_SIZE], head: 0, len: 0 });
static CONSOLE_ATTACHED: AtomicBool = AtomicBool::new(false);

struct Sinks<'a> {
    ring: &'a mut Ring,
}

impl Write for Sinks<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        serial::write_str(s);
        if CONSOLE_ATTACHED.load(Ordering::Relaxed) {
            fbcon::write_bytes(s.as_bytes());
        }
        self.ring.push(s.as_bytes());
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

/// Writes raw text (no timestamp).
pub fn print(args: fmt::Arguments) {
    let mut ring = LOG.lock_at(HIGH_LEVEL);
    let _ = Sinks { ring: &mut ring }.write_fmt(args);
}

/// Writes one timestamped log line.
pub fn log(level: Level, args: fmt::Arguments) {
    let us = clock::uptime_us();
    let mut ring = LOG.lock_at(HIGH_LEVEL);
    let mut out = Sinks { ring: &mut ring };
    let _ = write!(out, "\x1b[90m[{:5}.{:06}]\x1b[0m ", us / 1_000_000, us % 1_000_000);
    let _ = match level {
        Level::Info => Ok(()),
        Level::Warn => out.write_str("\x1b[93mwarning:\x1b[0m "),
        Level::Error => out.write_str("\x1b[91merror:\x1b[0m "),
    };
    let _ = out.write_fmt(args);
    let _ = out.write_str("\n");
}

pub fn info(args: fmt::Arguments) {
    log(Level::Info, args);
}

pub fn warn(args: fmt::Arguments) {
    log(Level::Warn, args);
}

pub fn error(args: fmt::Arguments) {
    log(Level::Error, args);
}

/// Starts mirroring the log to the framebuffer console, first replaying
/// everything logged so far so early boot messages appear on screen too.
pub fn attach_console() {
    let ring = LOG.lock_at(HIGH_LEVEL);
    ring.for_each(fbcon::write_bytes);
    CONSOLE_ATTACHED.store(true, Ordering::Relaxed);
}

/// Copies the ring buffer contents to `f` (for the `log` command).
pub fn dump(f: impl FnMut(&[u8])) {
    LOG.lock_at(HIGH_LEVEL).for_each(f);
}

/// Releases the log lock unconditionally. Crash path only.
///
/// # Safety
/// The previous owner must never run again (the system is stopping).
pub unsafe fn force_unlock() {
    unsafe { LOG.force_unlock() };
}

#[macro_export]
macro_rules! kprint {
    ($($arg:tt)*) => { $crate::log::print(format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! kprintln {
    () => { $crate::log::print(format_args!("\n")) };
    ($($arg:tt)*) => { $crate::log::print(format_args!("{}\n", format_args!($($arg)*))) };
}

#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => { $crate::log::info(format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => { $crate::log::warn(format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! error {
    ($($arg:tt)*) => { $crate::log::error(format_args!($($arg)*)) };
}
