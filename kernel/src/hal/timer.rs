//! Timer calibration. The TSC and local APIC timer run at frequencies the
//! kernel has to measure against a reference clock of known rate: the HPET
//! when the firmware describes one, otherwise the legacy PIT.

use crate::arch::x86_64::{apic, cpu};
use crate::hal::hpet;

const PIT_HZ: u64 = 1_193_182;
const CALIBRATION_MS: u64 = 10;

/// Waits `ms` milliseconds using PIT channel 2 (polled, no interrupt).
/// Returns false if the PIT did not respond.
fn pit_wait_ms(ms: u64) -> bool {
    let count = (PIT_HZ * ms / 1000) as u16;
    // SAFETY: channel 2 is gated through port 0x61 and is not used by
    // anything else in Meridian.
    unsafe {
        let gate = cpu::inb(0x61);
        cpu::outb(0x61, (gate & !0x02) | 0x01); // gate on, speaker off
        cpu::outb(0x43, 0b1011_0000); // channel 2, lo/hi byte, mode 0
        cpu::outb(0x42, count as u8);
        cpu::outb(0x42, (count >> 8) as u8);
        let g = cpu::inb(0x61) & !0x01;
        cpu::outb(0x61, g);
        cpu::outb(0x61, g | 0x01); // restart the count
        for _ in 0..50_000_000u64 {
            if cpu::inb(0x61) & 0x20 != 0 {
                return true;
            }
        }
    }
    false
}

pub struct Calibration {
    pub tsc_khz: u64,
    pub apic_ticks_per_ms: u32,
    pub reference: &'static str,
}

/// Measures the TSC and local APIC timer frequencies.
pub fn calibrate() -> Result<Calibration, &'static str> {
    apic::timer_start_free_running();
    let apic0 = apic::timer_current();
    let tsc0 = cpu::rdtsc();
    let reference = if let Some(h) = hpet::get() {
        h.wait_us(CALIBRATION_MS * 1000);
        "HPET"
    } else if pit_wait_ms(CALIBRATION_MS) {
        "PIT"
    } else {
        apic::timer_stop();
        return Err("no reference timer (HPET or PIT) responded");
    };
    let tsc1 = cpu::rdtsc();
    let apic1 = apic::timer_current();
    apic::timer_stop();

    let apic_ticks_per_ms = (apic0 - apic1) / CALIBRATION_MS as u32;
    let tsc_khz = (tsc1 - tsc0) / CALIBRATION_MS;
    if apic_ticks_per_ms == 0 || tsc_khz == 0 {
        return Err("timer calibration produced a zero frequency");
    }
    Ok(Calibration { tsc_khz, apic_ticks_per_ms, reference })
}
