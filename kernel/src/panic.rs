//! Fatal error handling ("stop errors").
//!
//! When the kernel hits an unrecoverable condition it stops every processor,
//! paints the stop screen, writes a full report (registers, control
//! registers, stack trace) to the screen and serial port, and halts. Under
//! the automated test harness it also exits QEMU with a failure code so CI
//! never hangs.

use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch::x86_64::idt::{self, TrapFrame, fixup};
use crate::arch::x86_64::{apic, cpu};
use crate::drivers::fbcon;
use crate::hal::{power, serial};
use crate::ke::{clock, irql, pcr};

/// Stop codes.
pub mod stop {
    pub const KERNEL_PANIC: u32 = 0x0000_0001;
    pub const KERNEL_EXCEPTION: u32 = 0x0000_0002;
    pub const KERNEL_PAGE_FAULT: u32 = 0x0000_0003;
    pub const DOUBLE_FAULT: u32 = 0x0000_0004;
    pub const MANUALLY_INITIATED: u32 = 0x0000_0005;

    pub fn name(code: u32) -> &'static str {
        match code {
            KERNEL_PANIC => "KERNEL_PANIC",
            KERNEL_EXCEPTION => "KERNEL_MODE_EXCEPTION_NOT_HANDLED",
            KERNEL_PAGE_FAULT => "PAGE_FAULT_IN_KERNEL_MODE",
            DOUBLE_FAULT => "DOUBLE_FAULT",
            MANUALLY_INITIATED => "MANUALLY_INITIATED_CRASH",
            _ => "UNKNOWN_STOP",
        }
    }
}

static IN_PROGRESS: AtomicBool = AtomicBool::new(false);

pub fn in_progress() -> bool {
    IN_PROGRESS.load(Ordering::Relaxed)
}

/// Prints a message straight to the serial port and stops this CPU.
pub fn emergency_halt(message: &str) -> ! {
    serial::write_str("\n*** EMERGENCY HALT: ");
    serial::write_str(message);
    serial::write_str("\n");
    cpu::halt_forever();
}

pub fn exception_bugcheck(frame: &TrapFrame, cr2: u64) -> ! {
    let code = match frame.vector {
        8 => stop::DOUBLE_FAULT,
        14 => stop::KERNEL_PAGE_FAULT,
        _ => stop::KERNEL_EXCEPTION,
    };
    let name = idt::exception_name(frame.vector);
    if frame.vector == 14 {
        let access = match (frame.error_code & 0x10 != 0, frame.error_code & 0x2 != 0) {
            (true, _) => "executing",
            (false, true) => "writing",
            (false, false) => "reading",
        };
        let cause = if frame.error_code & 1 != 0 { "protection violation" } else { "page not present" };
        bugcheck(code, format_args!("{name} {access} {cr2:#018x} ({cause})"), Some(frame));
    } else {
        bugcheck(code, format_args!("{name} (error code {:#x})", frame.error_code), Some(frame));
    }
}

/// Stops the system with `code`.
pub fn bugcheck(code: u32, detail: fmt::Arguments, frame: Option<&TrapFrame>) -> ! {
    // SAFETY: we are stopping the machine; nothing else may run.
    unsafe { cpu::disable_interrupts() };
    if IN_PROGRESS.swap(true, Ordering::AcqRel) {
        // Another processor is already reporting a crash.
        cpu::halt_forever();
    }
    irql::raise(irql::HIGH_LEVEL);
    if pcr::online_count() > 1 {
        apic::nmi_all_others();
    }
    // SAFETY: all other processors are halted (or never started).
    unsafe {
        crate::log::force_unlock();
        fbcon::enter_crash_screen();
    }

    let mut out = CrashWriter;
    let _ = writeln!(out, "\nMeridian OS ran into a problem and has stopped to protect your data.\n");
    let _ = writeln!(out, "  Stop code:  {:#010X}  {}", code, stop::name(code));
    let _ = writeln!(out, "  What failed: {detail}\n");
    if let Some(f) = frame {
        let _ = writeln!(out, "  RIP {:#018x}  CS  {:#06x}  RFLAGS {:#010x}", f.rip, f.cs, f.rflags);
        let _ = writeln!(out, "  RSP {:#018x}  SS  {:#06x}  ERROR  {:#x}", f.rsp, f.ss, f.error_code);
        let _ = writeln!(out, "  RAX {:#018x}  RBX {:#018x}  RCX {:#018x}", f.rax, f.rbx, f.rcx);
        let _ = writeln!(out, "  RDX {:#018x}  RSI {:#018x}  RDI {:#018x}", f.rdx, f.rsi, f.rdi);
        let _ = writeln!(out, "  RBP {:#018x}  R8  {:#018x}  R9  {:#018x}", f.rbp, f.r8, f.r9);
        let _ = writeln!(out, "  R10 {:#018x}  R11 {:#018x}  R12 {:#018x}", f.r10, f.r11, f.r12);
        let _ = writeln!(out, "  R13 {:#018x}  R14 {:#018x}  R15 {:#018x}", f.r13, f.r14, f.r15);
    }
    let _ = writeln!(
        out,
        "  CR0 {:#010x}  CR2 {:#018x}  CR3 {:#018x}  CR4 {:#010x}",
        cpu::read_cr0(),
        cpu::read_cr2(),
        cpu::read_cr3(),
        cpu::read_cr4()
    );

    let _ = writeln!(out, "\n  Stack trace (resolve with: python tools/symbolize.py <address>):");
    let mut rbp = match frame {
        Some(f) => {
            let _ = writeln!(out, "    #0  {:#018x}", f.rip);
            f.rbp
        }
        None => {
            let rbp: u64;
            // SAFETY: reading the frame pointer register.
            unsafe { core::arch::asm!("mov {}, rbp", out(reg) rbp, options(nomem, nostack, preserves_flags)) };
            rbp
        }
    };
    for depth in 1..20 {
        if rbp < 0xFFFF_8000_0000_0000 || rbp % 8 != 0 {
            break;
        }
        let (Some(next), Some(ret)) = (fixup::read_u64(rbp), fixup::read_u64(rbp + 8)) else { break };
        if ret == 0 {
            break;
        }
        let _ = writeln!(out, "    #{depth:<2} {ret:#018x}");
        rbp = next;
    }

    let us = clock::uptime_us();
    let _ = writeln!(
        out,
        "\n  Processor {} of {}, uptime {}.{:03} s, kernel {} ({})",
        // The PCR does not exist yet if we crash very early.
        if pcr::online_count() > 0 { pcr::current().index } else { 0 },
        pcr::online_count().max(1),
        us / 1_000_000,
        (us / 1000) % 1000,
        crate::VERSION,
        crate::GIT_HASH
    );
    let _ = writeln!(out, "\n  The system has been halted. Restart the computer to continue.");

    if crate::boot_info_opt().is_some_and(|i| i.has_option("selftest")) {
        power::qemu_exit(power::QEMU_EXIT_FAILURE);
    }
    cpu::halt_forever();
}

/// Writes to every log sink without taking locks other code might hold.
struct CrashWriter;

impl Write for CrashWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        crate::log::print(format_args!("{s}"));
        Ok(())
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    match info.location() {
        Some(loc) => {
            bugcheck(stop::KERNEL_PANIC, format_args!("{} ({}:{})", info.message(), loc.file(), loc.line()), None)
        }
        None => bugcheck(stop::KERNEL_PANIC, format_args!("{}", info.message()), None),
    }
}
