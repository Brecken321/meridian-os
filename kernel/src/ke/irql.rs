//! Interrupt request levels (IRQL).
//!
//! Like Windows NT on x64, Meridian maps IRQL directly onto the local APIC's
//! task priority via CR8. An interrupt vector's priority class is its upper
//! nibble, so a CPU running at IRQL `n` only accepts vectors `>= (n+1)*16`.
//! Vector numbers are therefore chosen so that `vector >> 4` equals the IRQL
//! the handler runs at (see `arch::x86_64::vectors`).

use crate::arch::x86_64::cpu;

pub type Irql = u8;

/// Normal thread execution.
pub const PASSIVE_LEVEL: Irql = 0;
/// Asynchronous procedure calls are masked.
pub const APC_LEVEL: Irql = 1;
/// Thread dispatching (preemption) is masked; spin locks run here.
pub const DISPATCH_LEVEL: Irql = 2;
/// Clock interrupts.
pub const CLOCK_LEVEL: Irql = 13;
/// Inter-processor interrupts.
pub const IPI_LEVEL: Irql = 14;
/// Everything maskable is blocked.
pub const HIGH_LEVEL: Irql = 15;

#[inline]
pub fn current() -> Irql {
    (cpu::read_cr8() & 0xF) as Irql
}

/// Raises the IRQL to `level` if it is currently lower and returns the
/// previous IRQL, which must later be passed to [`lower`].
#[inline]
pub fn raise(level: Irql) -> Irql {
    let old = current();
    if level > old {
        // SAFETY: raising the task priority only masks interrupts.
        unsafe { cpu::write_cr8(level as u64) };
    }
    old
}

/// Restores an IRQL previously returned by [`raise`].
#[inline]
pub fn lower(level: Irql) {
    // SAFETY: restoring a previously valid IRQL.
    unsafe { cpu::write_cr8(level as u64) };
}

pub fn name(level: Irql) -> &'static str {
    match level {
        PASSIVE_LEVEL => "PASSIVE",
        APC_LEVEL => "APC",
        DISPATCH_LEVEL => "DISPATCH",
        CLOCK_LEVEL => "CLOCK",
        IPI_LEVEL => "IPI",
        HIGH_LEVEL => "HIGH",
        _ => "DEVICE",
    }
}
