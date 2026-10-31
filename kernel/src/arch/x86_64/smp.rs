//! Application processor (AP) startup.
//!
//! APs begin in 16-bit real mode at the reserved page below 1 MiB that the
//! boot manager set aside. The trampoline copied there switches to 32-bit
//! protected mode, enables PAE paging with a temporary PML4 that maps the
//! trampoline page 1:1 plus the whole kernel half, enters long mode and
//! jumps to [`ap_entry`] in the kernel on a stack the bootstrap processor
//! allocated. Processors are started one at a time, so the trampoline's
//! parameter slots are never shared.

use alloc::boxed::Box;
use alloc::format;
use alloc::vec::Vec;
use core::arch::global_asm;
use core::ptr;
use core::sync::atomic::{AtomicU32, Ordering};

use super::{apic, cpu, gdt, idt};
use crate::hal::acpi::Madt;
use crate::ke::pcr::{self, MAX_CPUS, Pcr};
use crate::ke::{clock, irql, sched};
use crate::mm::{kva, paging, phys_to_virt, pmm};

global_asm!(
    r#"
.section .text.ap_trampoline,"ax"
.p2align 4
.global ap_trampoline_start
ap_trampoline_start:
.code16
    cli
    cld
    mov ax, cs
    mov ds, ax
    // lgdt [ap_tramp_gdtr - ap_trampoline_start] (DS-relative), hand
    // encoded because the assembler rejects label differences in operands.
    .byte 0x0F, 0x01, 0x16
    .word ap_tramp_gdtr - ap_trampoline_start
    mov eax, cr0
    or eax, 1
    mov cr0, eax
    .byte 0x66, 0xEA
.global ap_tramp_pm32_target
ap_tramp_pm32_target:
    .long 0
    .word 0x08
.code32
.global ap_tramp_pm32
ap_tramp_pm32:
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov eax, cr4
    or eax, 0x20
    mov cr4, eax
    // mov eax, imm32: the startup PML4 address, patched at run time.
    .byte 0xB8
.global ap_tramp_cr3
ap_tramp_cr3:
    .long 0
    mov cr3, eax
    mov ecx, 0xC0000080
    rdmsr
    or eax, 0x900
    wrmsr
    mov eax, cr0
    or eax, 0x80010001
    mov cr0, eax
    .byte 0xEA
.global ap_tramp_lm64_target
ap_tramp_lm64_target:
    .long 0
    .word 0x18
.code64
.global ap_tramp_lm64
ap_tramp_lm64:
    mov rsp, qword ptr [rip + ap_tramp_stack]
    mov rdi, qword ptr [rip + ap_tramp_arg]
    mov rax, qword ptr [rip + ap_tramp_entry]
    xor ebp, ebp
    push 0
    jmp rax
.p2align 3
.global ap_tramp_gdt
ap_tramp_gdt:
    .quad 0
    .quad 0x00CF9A000000FFFF
    .quad 0x00CF92000000FFFF
    .quad 0x00AF9A000000FFFF
.global ap_tramp_gdtr
ap_tramp_gdtr:
    .word 31
    .long 0
.p2align 3
.global ap_tramp_stack
ap_tramp_stack:
    .quad 0
.global ap_tramp_arg
ap_tramp_arg:
    .quad 0
.global ap_tramp_entry
ap_tramp_entry:
    .quad 0
.global ap_trampoline_end
ap_trampoline_end:
.code64
.text
"#
);

unsafe extern "C" {
    static ap_trampoline_start: u8;
    static ap_trampoline_end: u8;
    static ap_tramp_pm32_target: u8;
    static ap_tramp_pm32: u8;
    static ap_tramp_lm64_target: u8;
    static ap_tramp_lm64: u8;
    static ap_tramp_gdt: u8;
    static ap_tramp_gdtr: u8;
    static ap_tramp_cr3: u8;
    static ap_tramp_stack: u8;
    static ap_tramp_arg: u8;
    static ap_tramp_entry: u8;
}

/// Number of application processors that completed `ap_entry`.
static AP_READY: AtomicU32 = AtomicU32::new(0);

const AP_STACK_PAGES: u64 = 8;
const IST_STACK_PAGES: u64 = 4;

fn offset(symbol: *const u8) -> usize {
    symbol as usize - (&raw const ap_trampoline_start) as usize
}

/// Starts every enabled processor in the MADT. Returns how many started.
pub fn start_application_processors(trampoline: u64, madt: &Madt) -> usize {
    let bsp = apic::id();
    let targets: Vec<u32> = madt.cpus.iter().filter(|c| c.enabled && c.apic_id != bsp).map(|c| c.apic_id).collect();
    if targets.is_empty() {
        return 0;
    }
    if trampoline == 0 || trampoline >= 0x10_0000 {
        crate::warn!("smp: no startup page below 1 MiB was reserved; using one processor");
        return 0;
    }

    // Temporary page tables: the kernel half plus the trampoline page 1:1.
    // CR3 is loaded in 32-bit mode, so the PML4 must lie below 4 GiB.
    let Some(pml4) = pmm::alloc_zeroed_frame_below(1 << 32) else {
        crate::warn!("smp: no memory below 4 GiB for startup page tables");
        return 0;
    };
    let kernel_top = phys_to_virt(paging::kernel_pml4()) as *const u64;
    let startup_top = phys_to_virt(pml4) as *mut u64;
    // SAFETY: both are page-table frames reachable through the direct map.
    unsafe { ptr::copy_nonoverlapping(kernel_top.add(256), startup_top.add(256), 256) };
    // SAFETY: the trampoline page is reserved for exactly this purpose.
    if unsafe { paging::map(pml4, trampoline, trampoline, paging::WRITABLE) }.is_err() {
        crate::warn!("smp: cannot map the startup page");
        return 0;
    }

    // Copy and patch the trampoline.
    // SAFETY: the symbols delimit the trampoline code assembled above; the
    // destination is the reserved page (direct-mapped).
    unsafe {
        let start = &raw const ap_trampoline_start;
        let len = (&raw const ap_trampoline_end) as usize - start as usize;
        assert!(len <= 4096, "AP trampoline larger than a page");
        let page = phys_to_virt(trampoline) as *mut u8;
        ptr::copy_nonoverlapping(start, page, len);
        let patch32 = |sym: *const u8, value: u64| (page.add(offset(sym)) as *mut u32).write_unaligned(value as u32);
        patch32(&raw const ap_tramp_pm32_target, trampoline + offset(&raw const ap_tramp_pm32) as u64);
        patch32(&raw const ap_tramp_lm64_target, trampoline + offset(&raw const ap_tramp_lm64) as u64);
        patch32((&raw const ap_tramp_gdtr).add(2), trampoline + offset(&raw const ap_tramp_gdt) as u64);
        patch32(&raw const ap_tramp_cr3, pml4);
        (page.add(offset(&raw const ap_tramp_entry)) as *mut u64).write_unaligned(ap_entry as *const () as u64);
    }

    let mut started = 0;
    let mut all_responded = true;
    let mut next_index = 1u32;
    for apic_id in targets {
        if next_index as usize >= MAX_CPUS {
            crate::warn!("smp: more than {MAX_CPUS} processors; ignoring the rest");
            break;
        }
        match start_one(trampoline, apic_id, next_index) {
            Ok(()) => started += 1,
            Err(e) => {
                crate::warn!("smp: processor with APIC id {apic_id} did not start: {e}");
                all_responded = false;
            }
        }
        // A processor that failed may still wake up later and use its PCR,
        // so indices are never reused.
        next_index += 1;
    }

    if all_responded {
        // SAFETY: every AP has left the trampoline and runs on kernel tables.
        unsafe { paging::free_lower_half_tables(pml4) };
        pmm::free_frame(pml4);
    }
    started
}

fn start_one(trampoline: u64, apic_id: u32, index: u32) -> Result<(), &'static str> {
    let stack = kva::alloc_stack(AP_STACK_PAGES).ok_or("out of memory for its stack")?;
    let pcr: &'static mut Pcr = Box::leak(Box::new(Pcr::new(index, apic_id)));
    let mut ist = [0u64; 7];
    for slot in ist.iter_mut().take(3) {
        let s = kva::alloc_stack(IST_STACK_PAGES).ok_or("out of memory for its interrupt stacks")?;
        *slot = s.top();
        core::mem::forget(s); // owned by the processor forever
    }
    // SAFETY: the PCR is not visible to anyone else yet.
    unsafe { (*pcr.tss.get()).ist = ist };

    // SAFETY: parameter slots in the copied trampoline (patched above).
    unsafe {
        let page = phys_to_virt(trampoline) as *mut u8;
        (page.add(offset(&raw const ap_tramp_stack)) as *mut u64).write_unaligned(stack.top());
        (page.add(offset(&raw const ap_tramp_arg)) as *mut u64).write_unaligned(pcr as *mut Pcr as u64);
    }
    core::mem::forget(stack); // becomes the processor's idle thread stack

    let before = AP_READY.load(Ordering::Acquire);
    let started = || AP_READY.load(Ordering::Acquire) != before;
    let wait = |us: u64| {
        let deadline = clock::uptime_us() + us;
        while !started() && clock::uptime_us() < deadline {
            core::hint::spin_loop();
        }
        started()
    };
    // INIT, then up to two STARTUP IPIs (the classic MP startup sequence).
    apic::send_init(apic_id);
    clock::busy_wait_us(10_000);
    apic::send_startup(apic_id, (trampoline >> 12) as u8);
    if !wait(1_000) {
        apic::send_startup(apic_id, (trampoline >> 12) as u8);
    }
    if wait(500_000) { Ok(()) } else { Err("no response to INIT/STARTUP") }
}

/// First kernel code on an application processor.
extern "C" fn ap_entry(pcr: *mut Pcr) -> ! {
    // SAFETY: early per-processor initialisation, mirroring the bootstrap
    // processor's sequence in `kernel_entry`.
    unsafe {
        cpu::write_cr3(paging::kernel_pml4());
        let features = crate::cpu_features();
        cpu::enable_protection_features(features);
        cpu::enable_global_pages(features);
        cpu::write_cr8(irql::PASSIVE_LEVEL as u64);
        gdt::load((*pcr).gdt.get(), (*pcr).tss.get());
        pcr::install(pcr);
    }
    idt::load();
    super::syscall::init_processor();
    apic::init_local();
    let me = pcr::current();
    sched::init_processor(me, &format!("idle-{}", me.index));
    apic::start_periodic_timer(clock::TICK_HZ as u32);
    AP_READY.fetch_add(1, Ordering::AcqRel);
    sched::idle_loop()
}
