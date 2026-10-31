//! Machine power control: reset, soft-off and the QEMU test exit device.

use crate::arch::x86_64::cpu;
use crate::hal::acpi;

/// Exit codes understood by `tools/mbuild.py` (QEMU reports `(code << 1) | 1`).
pub const QEMU_EXIT_SUCCESS: u32 = 0x10;
pub const QEMU_EXIT_FAILURE: u32 = 0x11;

/// Exits QEMU through the `isa-debug-exit` device (port 0xF4). Only does
/// anything when the kernel runs under the test harness; on real hardware
/// the write goes nowhere and the caller falls through.
pub fn qemu_exit(code: u32) {
    // SAFETY: port 0xF4 is unassigned on PC hardware.
    unsafe { cpu::outl(0xF4, code) };
}

/// Restarts the machine: ACPI reset register, then the keyboard controller,
/// then a deliberate triple fault.
pub fn reboot() -> ! {
    // SAFETY: interrupts off; we are tearing the machine down.
    unsafe { cpu::disable_interrupts() };
    if let Some((reg, value)) = acpi::get().and_then(|a| a.fadt.as_ref()).and_then(|f| f.reset_register) {
        // SAFETY: the firmware-described reset register.
        unsafe {
            match reg.space {
                1 => cpu::outb(reg.address as u16, value),
                0 => core::ptr::write_volatile(crate::mm::phys_to_virt(reg.address) as *mut u8, value),
                _ => {}
            }
        }
        crate::ke::clock::busy_wait_us(50_000);
    }
    // SAFETY: pulsing the 8042 reset line is the classic PC reset.
    unsafe {
        for _ in 0..10_000 {
            if cpu::inb(0x64) & 0x02 == 0 {
                break;
            }
        }
        cpu::outb(0x64, 0xFE);
    }
    crate::ke::clock::busy_wait_us(50_000);
    // Last resort: an empty IDT turns the next exception into a triple fault.
    // SAFETY: intentionally fatal.
    unsafe {
        let null_idt = [0u64; 2];
        core::arch::asm!("lidt [{}]", "int3", in(reg) &null_idt, options(noreturn));
    }
}

/// Enters ACPI S5 (soft off). Returns an error if the platform does not
/// describe how to do so.
pub fn shutdown() -> Result<(), &'static str> {
    let acpi = acpi::get().ok_or("ACPI is not available")?;
    let fadt = acpi.fadt.as_ref().ok_or("no FADT")?;
    let (slp_a, slp_b) = acpi.s5.ok_or("the DSDT does not define \\_S5")?;
    if fadt.pm1a_control == 0 {
        return Err("no PM1a control register");
    }
    const SLP_EN: u16 = 1 << 13;
    // SAFETY: interrupts off; writing the firmware-described PM1 control
    // registers is how ACPI requests a sleep state.
    unsafe {
        cpu::disable_interrupts();
        enable_acpi_mode(fadt);
        cpu::outw(fadt.pm1a_control as u16, (slp_a << 10) | SLP_EN);
        if fadt.pm1b_control != 0 {
            cpu::outw(fadt.pm1b_control as u16, (slp_b << 10) | SLP_EN);
        }
    }
    crate::ke::clock::busy_wait_us(1_000_000);
    Err("the machine did not power off")
}

/// Switches the chipset from legacy (SMM) to ACPI mode if it is not already.
unsafe fn enable_acpi_mode(fadt: &acpi::Fadt) {
    const SCI_EN: u16 = 1;
    unsafe {
        if fadt.smi_command == 0 || fadt.acpi_enable == 0 || cpu::inw(fadt.pm1a_control as u16) & SCI_EN != 0 {
            return;
        }
        cpu::outb(fadt.smi_command as u16, fadt.acpi_enable);
        for _ in 0..300 {
            if cpu::inw(fadt.pm1a_control as u16) & SCI_EN != 0 {
                break;
            }
            crate::ke::clock::busy_wait_us(10_000);
        }
    }
}
