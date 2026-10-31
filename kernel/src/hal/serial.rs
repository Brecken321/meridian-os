//! 16550-compatible UART on COM1: the kernel's debug log and the remote
//! debug console. Used before any other output device exists.

use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch::x86_64::cpu::{inb, outb};
use crate::arch::x86_64::idt::{self, TrapFrame};
use crate::arch::x86_64::{apic, ioapic, vectors};
use crate::ke::irql::HIGH_LEVEL;
use crate::sync::SpinLock;

const COM1: u16 = 0x3F8;
const DATA: u16 = 0;
const INT_ENABLE: u16 = 1;
const FIFO_CTRL: u16 = 2;
const LINE_CTRL: u16 = 3;
const MODEM_CTRL: u16 = 4;
const LINE_STATUS: u16 = 5;

static PRESENT: AtomicBool = AtomicBool::new(false);
static RX_INTERRUPT: AtomicBool = AtomicBool::new(false);

const RX_LEN: usize = 256;

struct RxRing {
    buf: [u8; RX_LEN],
    head: usize,
    len: usize,
}

static RX: SpinLock<RxRing> = SpinLock::new(RxRing { buf: [0; RX_LEN], head: 0, len: 0 });

/// Initialises COM1 at 115200 8N1. Returns false if no UART responds.
pub fn init() -> bool {
    // SAFETY: standard 16550 programming sequence on the COM1 ports.
    unsafe {
        outb(COM1 + INT_ENABLE, 0x00);
        outb(COM1 + LINE_CTRL, 0x80); // DLAB on
        outb(COM1 + DATA, 0x01); // divisor 1 = 115200 baud
        outb(COM1 + INT_ENABLE, 0x00);
        outb(COM1 + LINE_CTRL, 0x03); // 8 bits, no parity, 1 stop bit
        outb(COM1 + FIFO_CTRL, 0xC7); // enable + clear FIFOs, 14-byte threshold
        outb(COM1 + MODEM_CTRL, 0x1E); // loopback mode for the self-check
        outb(COM1 + DATA, 0xAE);
        if inb(COM1 + DATA) != 0xAE {
            return false;
        }
        outb(COM1 + MODEM_CTRL, 0x0F); // normal operation, OUT1/OUT2, RTS/DTR
    }
    PRESENT.store(true, Ordering::Relaxed);
    true
}

pub fn present() -> bool {
    PRESENT.load(Ordering::Relaxed)
}

pub fn write_byte(b: u8) {
    if !present() {
        return;
    }
    // SAFETY: COM1 was detected in `init`.
    unsafe {
        for _ in 0..100_000 {
            if inb(COM1 + LINE_STATUS) & 0x20 != 0 {
                break;
            }
            core::hint::spin_loop();
        }
        outb(COM1 + DATA, b);
    }
}

pub fn write_str(s: &str) {
    for b in s.bytes() {
        if b == b'\n' {
            write_byte(b'\r');
        }
        write_byte(b);
    }
}

/// Returns a received byte, if one is waiting.
pub fn read_byte() -> Option<u8> {
    if !present() {
        return None;
    }
    if RX_INTERRUPT.load(Ordering::Acquire) {
        let mut rx = RX.lock_at(HIGH_LEVEL);
        if rx.len == 0 {
            return None;
        }
        let b = rx.buf[rx.head];
        rx.head = (rx.head + 1) % RX_LEN;
        rx.len -= 1;
        return Some(b);
    }
    // SAFETY: COM1 was detected in `init`.
    unsafe { (inb(COM1 + LINE_STATUS) & 1 != 0).then(|| inb(COM1 + DATA)) }
}

/// Switches COM1 input from polling to its interrupt (ISA IRQ 4).
pub fn enable_receive_interrupt() -> bool {
    if !present() || !idt::register(vectors::SERIAL, receive_interrupt) {
        return false;
    }
    if !ioapic::route_isa_irq(4, vectors::SERIAL, apic::id()) {
        idt::unregister(vectors::SERIAL);
        return false;
    }
    RX_INTERRUPT.store(true, Ordering::Release);
    // SAFETY: COM1 exists; enable the "received data available" interrupt.
    unsafe { outb(COM1 + INT_ENABLE, 0x01) };
    true
}

fn receive_interrupt(_: &mut TrapFrame) {
    let mut rx = RX.lock_at(HIGH_LEVEL);
    // SAFETY: draining COM1's receive buffer in its interrupt handler.
    while unsafe { inb(COM1 + LINE_STATUS) } & 1 != 0 {
        let b = unsafe { inb(COM1 + DATA) };
        if rx.len < RX_LEN {
            let tail = (rx.head + rx.len) % RX_LEN;
            rx.buf[tail] = b;
            rx.len += 1;
        }
    }
    drop(rx);
    crate::drivers::input::notify();
}
