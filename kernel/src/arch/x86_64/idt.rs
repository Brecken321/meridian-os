//! Interrupt descriptor table and trap dispatch.
//!
//! All 256 vectors enter through generated stubs (see `build.rs`) that push a
//! uniform [`TrapFrame`] and call [`kernel_trap_dispatch`]. Exceptions (0-31)
//! are handled here; interrupt vectors are routed to handlers registered with
//! [`register`], with the IRQL raised to the vector's priority class.

use core::arch::{asm, global_asm};
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use super::gdt::{IST_DOUBLE_FAULT, IST_MACHINE_CHECK, IST_NMI, KERNEL_CS};
use super::{apic, cpu, vectors};
use crate::ke::{irql, pcr};
use crate::sync::RacyCell;

global_asm!(include_str!(concat!(env!("OUT_DIR"), "/isr_stubs.s")));

global_asm!(
    r#"
.section .text.isr_common,"ax"
.global isr_common
isr_common:
    test qword ptr [rsp + 24], 3
    jz 2f
    swapgs
2:
    push rax
    push rbx
    push rcx
    push rdx
    push rsi
    push rdi
    push rbp
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15
    cld
    mov rdi, rsp
    call kernel_trap_dispatch
    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rbp
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rbx
    pop rax
    test qword ptr [rsp + 24], 3
    jz 3f
    swapgs
3:
    add rsp, 16
    iretq
.text
"#
);

/// Register state saved on entry to any interrupt or exception.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TrapFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
    pub vector: u64,
    pub error_code: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

impl TrapFrame {
    pub fn from_user_mode(&self) -> bool {
        self.cs & 3 != 0
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct IdtEntry {
    offset_low: u16,
    selector: u16,
    ist: u8,
    type_attr: u8,
    offset_mid: u16,
    offset_high: u32,
    reserved: u32,
}

impl IdtEntry {
    const MISSING: Self =
        Self { offset_low: 0, selector: 0, ist: 0, type_attr: 0, offset_mid: 0, offset_high: 0, reserved: 0 };

    fn interrupt_gate(handler: u64, ist: u8, dpl: u8) -> Self {
        Self {
            offset_low: handler as u16,
            selector: KERNEL_CS,
            ist,
            type_attr: 0x8E | (dpl << 5),
            offset_mid: (handler >> 16) as u16,
            offset_high: (handler >> 32) as u32,
            reserved: 0,
        }
    }
}

static IDT: RacyCell<[IdtEntry; 256]> = RacyCell::new([IdtEntry::MISSING; 256]);

unsafe extern "C" {
    static isr_stub_table: [u64; 256];
}

/// Fills in the IDT. Runs once on the bootstrap processor.
pub fn init() {
    // SAFETY: single-threaded early boot; the stub table is generated code.
    unsafe {
        let idt = &mut *IDT.get();
        for (vector, entry) in idt.iter_mut().enumerate() {
            let ist = match vector {
                2 => IST_NMI,
                8 => IST_DOUBLE_FAULT,
                18 => IST_MACHINE_CHECK,
                _ => 0,
            };
            // int3 may be issued from user mode (debuggers); nothing else.
            let dpl = if vector == 3 { 3 } else { 0 };
            *entry = IdtEntry::interrupt_gate(isr_stub_table[vector], ist, dpl);
        }
    }
}

/// Loads the IDT on the calling processor.
pub fn load() {
    #[repr(C, packed)]
    struct Ptr {
        limit: u16,
        base: u64,
    }
    let ptr = Ptr { limit: (size_of::<[IdtEntry; 256]>() - 1) as u16, base: IDT.get() as u64 };
    // SAFETY: the IDT is a static that lives forever.
    unsafe { asm!("lidt [{}]", in(reg) &ptr, options(readonly, nostack, preserves_flags)) };
}

pub type Handler = fn(&mut TrapFrame);

static HANDLERS: [AtomicUsize; 256] = [const { AtomicUsize::new(0) }; 256];
static UNEXPECTED: AtomicU64 = AtomicU64::new(0);
pub static BREAKPOINTS: AtomicU64 = AtomicU64::new(0);
static IN_EXCEPTION: AtomicBool = AtomicBool::new(false);

/// Installs `handler` for an interrupt vector (>= 32). Returns false if the
/// vector is already taken.
pub fn register(vector: u8, handler: Handler) -> bool {
    assert!(vector >= 32, "vectors below 32 are CPU exceptions");
    HANDLERS[vector as usize].compare_exchange(0, handler as usize, Ordering::AcqRel, Ordering::Acquire).is_ok()
}

pub fn unregister(vector: u8) {
    HANDLERS[vector as usize].store(0, Ordering::Release);
}

/// Number of interrupts that arrived with no registered handler.
pub fn unexpected_count() -> u64 {
    UNEXPECTED.load(Ordering::Relaxed)
}

#[unsafe(no_mangle)]
extern "C" fn kernel_trap_dispatch(frame: &mut TrapFrame) {
    let vector = frame.vector as usize;
    if vector < 32 {
        handle_exception(frame);
        return;
    }

    pcr::current().interrupts.fetch_add(1, Ordering::Relaxed);
    let old_irql = irql::raise((vector >> 4) as irql::Irql);
    let handler = HANDLERS[vector].load(Ordering::Acquire);
    if handler != 0 {
        // SAFETY: only `Handler` values are ever stored in HANDLERS.
        let handler: Handler = unsafe { core::mem::transmute(handler) };
        handler(frame);
    } else if vector != vectors::SPURIOUS as usize {
        UNEXPECTED.fetch_add(1, Ordering::Relaxed);
    }
    if vectors::needs_eoi(vector as u8) {
        apic::eoi();
    }
    irql::lower(old_irql);
}

const EXCEPTION_NAMES: [&str; 32] = [
    "divide error",
    "debug",
    "non-maskable interrupt",
    "breakpoint",
    "overflow",
    "bound range exceeded",
    "invalid opcode",
    "device not available",
    "double fault",
    "coprocessor segment overrun",
    "invalid TSS",
    "segment not present",
    "stack-segment fault",
    "general protection fault",
    "page fault",
    "reserved (15)",
    "x87 floating-point error",
    "alignment check",
    "machine check",
    "SIMD floating-point error",
    "virtualization exception",
    "control protection exception",
    "reserved (22)",
    "reserved (23)",
    "reserved (24)",
    "reserved (25)",
    "reserved (26)",
    "reserved (27)",
    "hypervisor injection exception",
    "VMM communication exception",
    "security exception",
    "reserved (31)",
];

pub fn exception_name(vector: u64) -> &'static str {
    EXCEPTION_NAMES.get(vector as usize).copied().unwrap_or("unknown")
}

fn handle_exception(frame: &mut TrapFrame) {
    match frame.vector {
        3 => {
            BREAKPOINTS.fetch_add(1, Ordering::Relaxed);
            return;
        }
        2 => {
            // NMIs are used to stop other processors during a crash.
            if crate::panic::in_progress() {
                cpu::halt_forever();
            }
            crate::log::warn(format_args!("non-maskable interrupt at {:#x}", frame.rip));
            return;
        }
        13 | 14 => {
            // Faults inside a registered "probe" instruction are recoverable.
            if let Some(fixup) = fixup::lookup(frame.rip) {
                frame.rip = fixup;
                return;
            }
        }
        _ => {}
    }

    if IN_EXCEPTION.swap(true, Ordering::AcqRel) {
        // A fault while reporting a fault: stop immediately.
        crate::panic::emergency_halt("nested kernel exception");
    }
    let cr2 = if frame.vector == 14 { cpu::read_cr2() } else { 0 };
    crate::panic::exception_bugcheck(frame, cr2);
}

/// Exception fixups let carefully written code touch memory that might not be
/// mapped (probing hardware tables, and later user-mode buffers) without
/// crashing: a fault at a registered instruction resumes at its fixup.
pub mod fixup {
    #[repr(C)]
    struct Entry {
        insn: u64,
        fixup: u64,
    }

    unsafe extern "C" {
        static __fixup_start: u8;
        static __fixup_end: u8;
    }

    pub fn lookup(rip: u64) -> Option<u64> {
        // SAFETY: the linker script delimits an array of `Entry`s.
        let entries = unsafe {
            let start = &raw const __fixup_start as *const Entry;
            let end = &raw const __fixup_end as *const Entry;
            core::slice::from_raw_parts(start, end.offset_from(start) as usize)
        };
        entries.iter().find(|e| e.insn == rip).map(|e| e.fixup)
    }

    /// Reads a `u64` from `addr`, returning `None` instead of crashing if the
    /// address is unmapped or non-canonical.
    #[inline(never)]
    pub fn read_u64(addr: u64) -> Option<u64> {
        let value: u64;
        let ok: u64;
        // SAFETY: a fault on the load is redirected to the fixup label.
        unsafe {
            core::arch::asm!(
                "2:",
                "mov {v}, qword ptr [{a}]",
                "mov {ok:e}, 1",
                "jmp 4f",
                "3:",
                "xor {v:e}, {v:e}",
                "xor {ok:e}, {ok:e}",
                "4:",
                ".pushsection .meridian_fixup, \"a\"",
                ".balign 8",
                ".quad 2b, 3b",
                ".popsection",
                a = in(reg) addr,
                v = out(reg) value,
                ok = out(reg) ok,
                options(nostack, readonly),
            );
        }
        (ok != 0).then_some(value)
    }
}
