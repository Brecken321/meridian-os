//! `syscall`/`sysret` entry and the first transition to user mode.
//!
//! On `syscall` the CPU loads the kernel code segment from IA32_STAR, jumps
//! to IA32_LSTAR with interrupts masked (IA32_FMASK), and leaves the user
//! return address in RCX and flags in R11. The entry stub switches to the
//! current thread's kernel stack (kept in the PCR at GS:16), saves a
//! [`SyscallFrame`] and calls the dispatcher with interrupts enabled.

use core::arch::{asm, global_asm};

use super::cpu;
use super::gdt::{KERNEL_CS, USER_CS, USER_CS32, USER_DS};

global_asm!(
    r#"
.section .text.syscall,"ax"
.global meridian_syscall_entry
meridian_syscall_entry:
    swapgs
    mov gs:[8], rsp
    mov rsp, gs:[16]
    push qword ptr gs:[8]
    push r11
    push rcx
    push rbp
    push rbx
    push r12
    push r13
    push r14
    push r15
    push r9
    push r8
    push r10
    push rdx
    push rsi
    push rdi
    push rax
    mov rdi, rsp
    sti
    call meridian_syscall_dispatch
    cli
    add rsp, 8
    pop rdi
    pop rsi
    pop rdx
    pop r10
    pop r8
    pop r9
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    pop rcx
    pop r11
    pop rsp
    swapgs
    sysretq
.text
"#
);

unsafe extern "C" {
    fn meridian_syscall_entry();
}

/// Register state saved by the system call entry stub.
#[repr(C)]
#[derive(Debug)]
pub struct SyscallFrame {
    pub rax: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub r10: u64,
    pub r8: u64,
    pub r9: u64,
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub rbx: u64,
    pub rbp: u64,
    /// User return address.
    pub rcx: u64,
    /// User RFLAGS.
    pub r11: u64,
    pub user_rsp: u64,
}

#[unsafe(no_mangle)]
extern "C" fn meridian_syscall_dispatch(frame: &mut SyscallFrame) -> i64 {
    crate::ps::syscall::dispatch(frame)
}

/// Programs the system call MSRs on the calling processor.
pub fn init_processor() {
    // SYSRET loads CS = STAR[63:48] + 16 and SS = STAR[63:48] + 8 (both with
    // RPL 3), which is why the user data segment precedes the 64-bit user
    // code segment in the GDT.
    let star = ((USER_CS32 as u64) << 48) | ((KERNEL_CS as u64) << 32);
    debug_assert!(USER_CS32 + 8 == USER_DS && USER_CS32 + 16 == USER_CS);
    // Mask IF, TF, DF and AC on entry.
    let fmask = (1 << 9) | (1 << 8) | (1 << 10) | (1 << 18);
    // SAFETY: standard SYSCALL configuration; EFER.SCE is already enabled.
    unsafe {
        cpu::wrmsr(cpu::IA32_STAR, star);
        cpu::wrmsr(cpu::IA32_LSTAR, meridian_syscall_entry as *const () as u64);
        cpu::wrmsr(cpu::IA32_FMASK, fmask);
        // User threads start with FS/GS bases of zero; IA32_KERNEL_GS_BASE
        // holds the user GS base while the kernel runs (swapgs swaps them).
        cpu::wrmsr(cpu::IA32_FS_BASE, 0);
        cpu::wrmsr(cpu::IA32_KERNEL_GS_BASE, 0);
    }
}

/// Leaves the kernel for the first time on a new user thread: jumps to
/// `rip` in ring 3 with `rsp` as the stack and `arg` in RCX (the first
/// argument under the Windows x64 calling convention).
///
/// # Safety
/// The current thread's process address space must be active and `rip` and
/// `rsp` must be mapped user addresses in it.
pub unsafe fn enter_user(rip: u64, stack_top: u64, arg: u64) -> ! {
    // Windows x64 entry state: RSP is 8 mod 16 (as if called) with 32 bytes
    // of shadow space above the (dummy) return address.
    let rsp = (stack_top & !0xF) - 40;
    unsafe {
        asm!(
            "cli",
            "swapgs",
            "push {ss}",
            "push {usp}",
            "push 0x202",
            "push {cs}",
            "push {rip}",
            "xor eax, eax",
            "xor ebx, ebx",
            "xor edx, edx",
            "xor esi, esi",
            "xor edi, edi",
            "xor ebp, ebp",
            "xor r8d, r8d",
            "xor r9d, r9d",
            "xor r10d, r10d",
            "xor r11d, r11d",
            "xor r12d, r12d",
            "xor r13d, r13d",
            "xor r14d, r14d",
            "xor r15d, r15d",
            "iretq",
            ss = const USER_DS as u64,
            cs = const USER_CS as u64,
            usp = in(reg) rsp,
            rip = in(reg) rip,
            in("rcx") arg,
            options(noreturn),
        );
    }
}
