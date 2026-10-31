//! I/O APIC: routes device interrupt lines (global system interrupts, GSIs)
//! to local APIC vectors.

use alloc::vec::Vec;

use crate::hal::acpi;
use crate::mm::kva;
use crate::sync::SpinLock;

const REG_VERSION: u32 = 0x01;
const REG_REDIRECTION: u32 = 0x10;
const MASKED: u64 = 1 << 16;
const LEVEL_TRIGGERED: u64 = 1 << 15;
const ACTIVE_LOW: u64 = 1 << 13;

struct IoApic {
    base: u64,
    gsi_base: u32,
    inputs: u32,
}

impl IoApic {
    fn read(&self, reg: u32) -> u32 {
        // SAFETY: base maps the IOREGSEL/IOWIN window (uncached).
        unsafe {
            core::ptr::write_volatile(self.base as *mut u32, reg);
            core::ptr::read_volatile((self.base + 0x10) as *const u32)
        }
    }

    fn write(&self, reg: u32, value: u32) {
        // SAFETY: as in `read`.
        unsafe {
            core::ptr::write_volatile(self.base as *mut u32, reg);
            core::ptr::write_volatile((self.base + 0x10) as *mut u32, value);
        }
    }

    fn set_entry(&self, input: u32, entry: u64) {
        self.write(REG_REDIRECTION + input * 2, MASKED as u32);
        self.write(REG_REDIRECTION + input * 2 + 1, (entry >> 32) as u32);
        self.write(REG_REDIRECTION + input * 2, entry as u32);
    }
}

static IOAPICS: SpinLock<Vec<IoApic>> = SpinLock::new(Vec::new());

/// Maps every I/O APIC listed in the MADT and masks all of their inputs.
pub fn init(madt: &acpi::Madt) -> Result<usize, &'static str> {
    let mut list = IOAPICS.lock();
    for info in &madt.ioapics {
        let base = kva::map_mmio(info.address as u64, 4096).ok_or("cannot map I/O APIC")?;
        let mut ioapic = IoApic { base, gsi_base: info.gsi_base, inputs: 0 };
        ioapic.inputs = ((ioapic.read(REG_VERSION) >> 16) & 0xFF) + 1;
        for input in 0..ioapic.inputs {
            ioapic.set_entry(input, MASKED);
        }
        list.push(ioapic);
    }
    Ok(list.len())
}

/// Routes a GSI to `vector` on the processor with `apic_id`.
pub fn route_gsi(gsi: u32, vector: u8, apic_id: u32, level: bool, active_low: bool) -> bool {
    let list = IOAPICS.lock();
    let Some(ioapic) = list.iter().find(|a| gsi >= a.gsi_base && gsi < a.gsi_base + a.inputs) else {
        return false;
    };
    let mut entry = vector as u64 | (apic_id as u64) << 56;
    if level {
        entry |= LEVEL_TRIGGERED;
    }
    if active_low {
        entry |= ACTIVE_LOW;
    }
    ioapic.set_entry(gsi - ioapic.gsi_base, entry);
    true
}

/// Routes a legacy ISA IRQ, honouring MADT interrupt source overrides.
pub fn route_isa_irq(irq: u8, vector: u8, apic_id: u32) -> bool {
    let (gsi, level, active_low) =
        acpi::get().and_then(|a| a.madt.as_ref()).map(|m| m.isa_irq_to_gsi(irq)).unwrap_or((irq as u32, false, false));
    route_gsi(gsi, vector, apic_id, level, active_low)
}

pub fn mask_gsi(gsi: u32) {
    let list = IOAPICS.lock();
    if let Some(ioapic) = list.iter().find(|a| gsi >= a.gsi_base && gsi < a.gsi_base + a.inputs) {
        ioapic.set_entry(gsi - ioapic.gsi_base, MASKED);
    }
}
