//! x86-64 architecture support.

pub mod apic;
pub mod context;
pub mod cpu;
pub mod gdt;
pub mod idt;
pub mod ioapic;
pub mod pic;
pub mod smp;
pub mod syscall;

/// Interrupt vector assignments. The upper nibble of a vector is the IRQL
/// its handler runs at (see `ke::irql`).
pub mod vectors {
    /// Software interrupt used to run the dispatcher at DISPATCH_LEVEL.
    pub const DISPATCH: u8 = 0x2F;
    /// PS/2 keyboard (IRQL 8).
    pub const KEYBOARD: u8 = 0x81;
    /// COM1 serial port (IRQL 8).
    pub const SERIAL: u8 = 0x82;
    /// PS/2 mouse (IRQL 8).
    pub const MOUSE: u8 = 0x83;
    /// Local APIC timer (CLOCK_LEVEL).
    pub const CLOCK: u8 = 0xD1;
    /// Generic inter-processor interrupt (IPI_LEVEL).
    pub const IPI: u8 = 0xE1;
    /// Legacy 8259 PIC range after remapping; masked, so only spurious IRQs.
    pub const PIC_BASE: u8 = 0xF0;
    /// Local APIC error interrupt.
    pub const APIC_ERROR: u8 = 0xFE;
    /// Local APIC spurious interrupt.
    pub const SPURIOUS: u8 = 0xFF;

    /// Whether an end-of-interrupt must be signalled to the local APIC after
    /// the handler runs. Spurious interrupts and (masked) legacy PIC vectors
    /// must not get one; DISPATCH gets its EOI *before* the handler because
    /// the handler may switch to another thread.
    pub fn needs_eoi(vector: u8) -> bool {
        !(PIC_BASE..APIC_ERROR).contains(&vector) && vector != SPURIOUS && vector != DISPATCH
    }
}
