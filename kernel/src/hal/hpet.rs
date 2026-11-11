//! High Precision Event Timer: a fixed-frequency counter used to calibrate
//! the TSC and local APIC timers.

use crate::mm::kva;
use crate::sync::Once;

const REG_CAPABILITIES: u64 = 0x000;
const REG_CONFIG: u64 = 0x010;
const REG_COUNTER: u64 = 0x0F0;
const CAP_64BIT: u64 = 1 << 13;
const CONFIG_ENABLE: u64 = 1;

pub struct Hpet {
    base: u64,
    /// Counter period in femtoseconds.
    pub period_fs: u64,
    mask: u64,
}

static HPET: Once<Hpet> = Once::new();

impl Hpet {
    fn read(&self, reg: u64) -> u64 {
        // SAFETY: base maps the HPET register block (uncached).
        unsafe { core::ptr::read_volatile((self.base + reg) as *const u64) }
    }

    fn write(&self, reg: u64, value: u64) {
        // SAFETY: as in `read`.
        unsafe { core::ptr::write_volatile((self.base + reg) as *mut u64, value) }
    }

    pub fn counter(&self) -> u64 {
        self.read(REG_COUNTER) & self.mask
    }

    pub fn frequency_hz(&self) -> u64 {
        1_000_000_000_000_000 / self.period_fs
    }

    /// Spins for `us` microseconds.
    pub fn wait_us(&self, us: u64) {
        let ticks = us * 1_000_000_000 / self.period_fs;
        let start = self.counter();
        while (self.counter().wrapping_sub(start) & self.mask) < ticks {
            core::hint::spin_loop();
        }
    }
}

/// Maps and starts the HPET at physical address `phys`.
pub fn init(phys: u64) -> Result<&'static Hpet, &'static str> {
    let base = kva::map_mmio(phys, 1024).ok_or("cannot map HPET registers")?;
    let mut hpet = Hpet { base, period_fs: 0, mask: u64::MAX };
    let caps = hpet.read(REG_CAPABILITIES);
    hpet.period_fs = caps >> 32;
    // The specification caps the period at 100 ns.
    if hpet.period_fs == 0 || hpet.period_fs > 100_000_000 {
        return Err("HPET reports an invalid counter period");
    }
    if caps & CAP_64BIT == 0 {
        hpet.mask = u32::MAX as u64;
    }
    hpet.write(REG_CONFIG, hpet.read(REG_CONFIG) | CONFIG_ENABLE);
    Ok(HPET.call_once(|| hpet))
}

pub fn get() -> Option<&'static Hpet> {
    HPET.get()
}
