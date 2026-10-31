//! x86-64 architecture support.

pub mod apic;
pub mod cpu;
pub mod gdt;
pub mod idt;
pub mod ioapic;
pub mod pic;

/// Interrupt vector assignments. The upper nibble of a vector is the IRQL
/// its handler runs at (see `ke::irql`).
pub mod vectors {
    /// Software interrupt used to run the dispatcher at DISPATCH_LEVEL.
    pub const DISPATCH: u8 = 0x2F;
    /// First vector handed out to device interrupts (IRQL 5).
    pub const DEVICE_FIRST: u8 = 0x50;
    /// Last vector handed out to device interrupts (IRQL 12).
    pub const DEVICE_LAST: u8 = 0xCF;
    /// PS/2 keyboard (IRQL 8).
    pub const KEYBOARD: u8 = 0x81;
    /// COM1 serial port (IRQL 8).
    pub const SERIAL: u8 = 0x82;
    /// Local APIC timer (CLOCK_LEVEL).
    pub const CLOCK: u8 = 0xD1;
    /// Generic inter-processor interrupt (IPI_LEVEL).
    pub const IPI: u8 = 0xE1;
    /// TLB shootdown request (IPI_LEVEL).
    pub const TLB_SHOOTDOWN: u8 = 0xE2;
    /// Legacy 8259 PIC range after remapping; masked, so only spurious IRQs.
    pub const PIC_BASE: u8 = 0xF0;
    /// Local APIC error interrupt.
    pub const APIC_ERROR: u8 = 0xFE;
    /// Local APIC spurious interrupt.
    pub const SPURIOUS: u8 = 0xFF;

    /// Whether an end-of-interrupt must be signalled to the local APIC.
    /// Spurious interrupts and (masked) legacy PIC vectors must not get one.
    pub fn needs_eoi(vector: u8) -> bool {
        !(PIC_BASE..APIC_ERROR).contains(&vector) && vector != SPURIOUS
    }
}
