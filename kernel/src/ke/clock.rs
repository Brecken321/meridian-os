//! System time keeping.
//!
//! Fine-grained time comes from the TSC, calibrated against the HPET or PIT
//! at boot. A 1 kHz clock interrupt on the bootstrap processor maintains the
//! tick count used for timeouts.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::arch::x86_64::cpu;

/// Clock interrupt frequency.
pub const TICK_HZ: u64 = 1000;

static BOOT_TSC: AtomicU64 = AtomicU64::new(0);
static TSC_KHZ: AtomicU64 = AtomicU64::new(0);
static TICKS: AtomicU64 = AtomicU64::new(0);

/// Records the TSC value that represents time zero. Called first thing.
pub fn mark_boot() {
    BOOT_TSC.store(cpu::rdtsc(), Ordering::Relaxed);
}

pub fn set_tsc_khz(khz: u64) {
    TSC_KHZ.store(khz, Ordering::Relaxed);
}

pub fn tsc_khz() -> u64 {
    TSC_KHZ.load(Ordering::Relaxed)
}

/// Microseconds since boot, or 0 before the TSC is calibrated.
pub fn uptime_us() -> u64 {
    let khz = TSC_KHZ.load(Ordering::Relaxed);
    if khz == 0 {
        return 0;
    }
    let delta = cpu::rdtsc().saturating_sub(BOOT_TSC.load(Ordering::Relaxed));
    (delta as u128 * 1000 / khz as u128) as u64
}

pub fn uptime_ms() -> u64 {
    uptime_us() / 1000
}

/// Called from the bootstrap processor's clock interrupt.
pub fn tick() {
    TICKS.fetch_add(1, Ordering::Relaxed);
}

pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Spins for at least `us` microseconds using the calibrated TSC.
pub fn busy_wait_us(us: u64) {
    let khz = TSC_KHZ.load(Ordering::Relaxed);
    if khz == 0 {
        // Uncalibrated: each port 0x80 write takes roughly 1 us on PC hardware.
        for _ in 0..us {
            // SAFETY: port 0x80 is the POST diagnostic port; writes are harmless.
            unsafe { cpu::outb(0x80, 0) };
        }
        return;
    }
    let target = cpu::rdtsc() + us * khz / 1000;
    while cpu::rdtsc() < target {
        core::hint::spin_loop();
    }
}
