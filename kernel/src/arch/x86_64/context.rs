//! Kernel thread context switching.
//!
//! A switched-out thread's kernel stack holds its callee-saved registers and
//! RFLAGS; its saved stack pointer lives in the thread object. Everything
//! else (caller-saved registers, the interrupt frame if it was preempted) is
//! already on that stack by the time `switch` is called.

use core::arch::global_asm;

global_asm!(
    r#"
.section .text.context,"ax"
.global meridian_switch_context
meridian_switch_context:
    pushfq
    push rbp
    push rbx
    push r12
    push r13
    push r14
    push r15
    mov [rdi], rsp
    mov rsp, rsi
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    popfq
    ret

.global meridian_thread_trampoline
meridian_thread_trampoline:
    mov rdi, r12
    call meridian_thread_startup
    ud2
.text
"#
);

unsafe extern "C" {
    fn meridian_switch_context(save_rsp: *mut u64, load_rsp: u64);
    fn meridian_thread_trampoline();
}

/// Saves the current context, storing its stack pointer in `*save_rsp`, and
/// resumes the context whose stack pointer is `load_rsp`. Returns when some
/// other processor or thread switches back to the saved context.
///
/// # Safety
/// `load_rsp` must come from [`initial_frame`] or a previous `switch`, and the
/// caller must hold the dispatcher lock (see `ke::sched`).
#[inline(always)]
pub unsafe fn switch(save_rsp: *mut u64, load_rsp: u64) {
    unsafe { meridian_switch_context(save_rsp, load_rsp) };
}

/// Prepares a fresh kernel stack so that switching to it starts executing
/// `meridian_thread_startup(arg)` with interrupts disabled. Returns the stack
/// pointer to store as the thread's saved context.
///
/// # Safety
/// `stack_top` must be the top of a mapped, unused kernel stack.
pub unsafe fn initial_frame(stack_top: u64, arg: u64) -> u64 {
    let top = stack_top & !0xF;
    // Popped in this order by `meridian_switch_context`; the trampoline's
    // address is consumed by its `ret`, leaving RSP 16-byte aligned so the
    // `call` in the trampoline gives the callee the ABI-mandated alignment.
    let frame: [u64; 8] = [
        0,                                          // r15
        0,                                          // r14
        0,                                          // r13
        arg,                                        // r12 -> rdi
        0,                                          // rbx
        0,                                          // rbp (ends stack traces)
        0x2,                                        // rflags: IF clear
        meridian_thread_trampoline as usize as u64, // return address
    ];
    let rsp = top - (frame.len() * 8) as u64;
    // SAFETY: the caller guarantees the stack is mapped and unused.
    unsafe { core::ptr::copy_nonoverlapping(frame.as_ptr(), rsp as *mut u64, frame.len()) };
    rsp
}
