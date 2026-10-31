//! Local APIC: interrupt acknowledgement, the per-CPU timer and
//! inter-processor interrupts. Both the xAPIC (MMIO) and x2APIC (MSR)
//! programming interfaces are supported; Meridian uses whichever mode the
//! firmware left the processor in.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use super::cpu;
use super::vectors;
use crate::mm::kva;

const REG_ID: u32 = 0x20;
const REG_VERSION: u32 = 0x30;
const REG_EOI: u32 = 0xB0;
const REG_SVR: u32 = 0xF0;
const REG_ESR: u32 = 0x280;
const REG_ICR_LOW: u32 = 0x300;
const REG_ICR_HIGH: u32 = 0x310;
const REG_LVT_TIMER: u32 = 0x320;
const REG_LVT_THERMAL: u32 = 0x330;
const REG_LVT_PERF: u32 = 0x340;
const REG_LVT_LINT0: u32 = 0x350;
const REG_LVT_LINT1: u32 = 0x360;
const REG_LVT_ERROR: u32 = 0x370;
const REG_TIMER_INITIAL: u32 = 0x380;
const REG_TIMER_CURRENT: u32 = 0x390;
const REG_TIMER_DIVIDE: u32 = 0x3E0;

const APIC_BASE_ENABLE: u64 = 1 << 11;
const APIC_BASE_X2APIC: u64 = 1 << 10;
const SVR_ENABLE: u32 = 1 << 8;
const LVT_MASKED: u32 = 1 << 16;
const TIMER_PERIODIC: u32 = 1 << 17;
const DELIVERY_NMI: u32 = 0b100 << 8;
const DELIVERY_INIT: u32 = 0b101 << 8;
const DELIVERY_STARTUP: u32 = 0b110 << 8;
const ICR_PENDING: u32 = 1 << 12;
const ICR_LEVEL_ASSERT: u32 = 1 << 14;
const ICR_SELF: u32 = 0b01 << 18;
const ICR_ALL_BUT_SELF: u32 = 0b11 << 18;
/// Timer divide configuration value for "divide by 16".
const DIVIDE_BY_16: u32 = 0b0011;

static X2APIC: AtomicBool = AtomicBool::new(false);
static MMIO: AtomicU64 = AtomicU64::new(0);
static TIMER_TICKS_PER_MS: AtomicU32 = AtomicU32::new(0);

fn read(reg: u32) -> u32 {
    if X2APIC.load(Ordering::Relaxed) {
        // SAFETY: x2APIC registers are MSRs 0x800 + offset/16.
        unsafe { cpu::rdmsr(0x800 + (reg >> 4)) as u32 }
    } else {
        // SAFETY: MMIO points at the uncached APIC register page.
        unsafe { core::ptr::read_volatile((MMIO.load(Ordering::Relaxed) + reg as u64) as *const u32) }
    }
}

fn write(reg: u32, value: u32) {
    if X2APIC.load(Ordering::Relaxed) {
        // SAFETY: as in `read`.
        unsafe { cpu::wrmsr(0x800 + (reg >> 4), value as u64) };
    } else {
        // SAFETY: as in `read`.
        unsafe { core::ptr::write_volatile((MMIO.load(Ordering::Relaxed) + reg as u64) as *mut u32, value) };
    }
}

/// Discovers the bootstrap processor's local APIC and maps its registers.
pub fn init_bsp(madt_base: Option<u64>) -> Result<(), &'static str> {
    // SAFETY: IA32_APIC_BASE exists on every x86-64 processor.
    let base = unsafe { cpu::rdmsr(cpu::IA32_APIC_BASE) };
    if base & APIC_BASE_X2APIC != 0 {
        X2APIC.store(true, Ordering::Relaxed);
    } else {
        let phys = madt_base.unwrap_or(base & 0x000F_FFFF_FFFF_F000);
        let va = kva::map_mmio(phys, 4096).ok_or("cannot map local APIC registers")?;
        MMIO.store(va, Ordering::Relaxed);
    }
    init_local();
    Ok(())
}

/// Enables and configures the calling processor's local APIC.
pub fn init_local() {
    // SAFETY: setting the global enable bit; the base address is unchanged.
    unsafe {
        let base = cpu::rdmsr(cpu::IA32_APIC_BASE);
        if base & APIC_BASE_ENABLE == 0 {
            cpu::wrmsr(cpu::IA32_APIC_BASE, base | APIC_BASE_ENABLE);
        }
    }
    let max_lvt = (read(REG_VERSION) >> 16) & 0xFF;
    write(REG_LVT_TIMER, LVT_MASKED);
    if max_lvt >= 4 {
        write(REG_LVT_PERF, LVT_MASKED);
    }
    if max_lvt >= 5 {
        write(REG_LVT_THERMAL, LVT_MASKED);
    }
    // LINT0 carries legacy PIC interrupts (unused); LINT1 is wired to NMI on
    // PC platforms.
    write(REG_LVT_LINT0, LVT_MASKED);
    write(REG_LVT_LINT1, DELIVERY_NMI);
    write(REG_LVT_ERROR, vectors::APIC_ERROR as u32);
    write(REG_ESR, 0);
    write(REG_ESR, 0);
    write(REG_SVR, SVR_ENABLE | vectors::SPURIOUS as u32);
    eoi();
}

pub fn is_x2apic() -> bool {
    X2APIC.load(Ordering::Relaxed)
}

/// The calling processor's APIC ID.
pub fn id() -> u32 {
    let raw = read(REG_ID);
    if is_x2apic() { raw } else { raw >> 24 }
}

#[inline]
pub fn eoi() {
    write(REG_EOI, 0);
}

/// Reads and clears the error status register.
pub fn error_status() -> u32 {
    write(REG_ESR, 0);
    read(REG_ESR)
}

fn send_icr(dest: u32, low: u32) {
    if is_x2apic() {
        // SAFETY: x2APIC ICR is a single 64-bit MSR.
        unsafe { cpu::wrmsr(0x830, (dest as u64) << 32 | low as u64) };
    } else {
        write(REG_ICR_HIGH, dest << 24);
        write(REG_ICR_LOW, low);
        while read(REG_ICR_LOW) & ICR_PENDING != 0 {
            core::hint::spin_loop();
        }
    }
}

/// Sends a fixed interrupt with `vector` to the processor with `apic_id`.
pub fn send_ipi(apic_id: u32, vector: u8) {
    send_icr(apic_id, vector as u32);
}

/// Sends a fixed interrupt to the calling processor.
pub fn self_ipi(vector: u8) {
    send_icr(0, ICR_SELF | vector as u32);
}

pub fn send_init(apic_id: u32) {
    send_icr(apic_id, DELIVERY_INIT | ICR_LEVEL_ASSERT);
}

pub fn send_startup(apic_id: u32, page: u8) {
    send_icr(apic_id, DELIVERY_STARTUP | page as u32);
}

/// Sends an NMI to every processor except this one (used to stop the
/// machine on a fatal error).
pub fn nmi_all_others() {
    send_icr(0, ICR_ALL_BUT_SELF | DELIVERY_NMI);
}

/// Starts the timer counting down from its maximum value (masked, one-shot);
/// used while calibrating.
pub fn timer_start_free_running() {
    write(REG_TIMER_DIVIDE, DIVIDE_BY_16);
    write(REG_LVT_TIMER, LVT_MASKED);
    write(REG_TIMER_INITIAL, u32::MAX);
}

pub fn timer_current() -> u32 {
    read(REG_TIMER_CURRENT)
}

pub fn timer_stop() {
    write(REG_TIMER_INITIAL, 0);
    write(REG_LVT_TIMER, LVT_MASKED);
}

pub fn set_timer_ticks_per_ms(ticks: u32) {
    TIMER_TICKS_PER_MS.store(ticks, Ordering::Relaxed);
}

pub fn timer_ticks_per_ms() -> u32 {
    TIMER_TICKS_PER_MS.load(Ordering::Relaxed)
}

/// Starts the calling processor's timer firing `vectors::CLOCK` at `hz`.
pub fn start_periodic_timer(hz: u32) {
    let per_ms = TIMER_TICKS_PER_MS.load(Ordering::Relaxed);
    let count = (per_ms as u64 * 1000 / hz as u64).max(1) as u32;
    write(REG_TIMER_DIVIDE, DIVIDE_BY_16);
    write(REG_LVT_TIMER, vectors::CLOCK as u32 | TIMER_PERIODIC);
    write(REG_TIMER_INITIAL, count);
}
