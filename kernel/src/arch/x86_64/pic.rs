//! Legacy 8259 programmable interrupt controllers. Meridian uses the APICs,
//! so the PICs are remapped away from the exception vectors and masked.

use super::cpu::{io_wait, outb};
use super::vectors::PIC_BASE;

const PIC1_CMD: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_CMD: u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;

pub fn disable() {
    // SAFETY: standard 8259 initialisation sequence; the PICs exist on every
    // PC-compatible chipset (and writes are ignored where they do not).
    unsafe {
        outb(PIC1_CMD, 0x11);
        io_wait();
        outb(PIC2_CMD, 0x11);
        io_wait();
        outb(PIC1_DATA, PIC_BASE);
        io_wait();
        outb(PIC2_DATA, PIC_BASE + 8);
        io_wait();
        outb(PIC1_DATA, 4);
        io_wait();
        outb(PIC2_DATA, 2);
        io_wait();
        outb(PIC1_DATA, 1);
        io_wait();
        outb(PIC2_DATA, 1);
        io_wait();
        outb(PIC1_DATA, 0xFF);
        outb(PIC2_DATA, 0xFF);
    }
}
