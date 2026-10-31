//! Native Meridian user-mode runtime.
//!
//! Programs built for Meridian are PE32+ images (the same format Windows
//! uses), compiled with Rust's `x86_64-pc-windows-msvc` target and linked
//! with `lld-link` against no system libraries. This crate supplies what a
//! C runtime normally would: memory intrinsics, the panic handler, and safe
//! wrappers for Meridian system calls.

#![no_std]

use core::arch::{asm, global_asm};

pub use meridian_abi::{protection, service, status};

// --------------------------------------------------------------------------
// Memory intrinsics (normally provided by the C runtime on this target).
// `rep movsb` / `rep stosb` are fast on every CPU with ERMSB and cannot be
// turned back into calls to these functions by the optimiser.
// --------------------------------------------------------------------------

global_asm!(
    r#"
.text
.global memcpy
memcpy:
    mov rax, rcx
    push rdi
    push rsi
    mov rdi, rcx
    mov rsi, rdx
    mov rcx, r8
    rep movsb
    pop rsi
    pop rdi
    ret

.global memmove
memmove:
    mov rax, rcx
    push rdi
    push rsi
    mov rdi, rcx
    mov rsi, rdx
    mov rcx, r8
    cmp rdi, rsi
    jbe 2f
    lea r9, [rsi + rcx]
    cmp rdi, r9
    jae 2f
    lea rsi, [rsi + rcx - 1]
    lea rdi, [rdi + rcx - 1]
    std
    rep movsb
    cld
    jmp 3f
2:
    rep movsb
3:
    pop rsi
    pop rdi
    ret

.global memset
memset:
    mov r9, rcx
    push rdi
    mov rdi, rcx
    mov eax, edx
    mov rcx, r8
    rep stosb
    pop rdi
    mov rax, r9
    ret

.global memcmp
memcmp:
    xor eax, eax
    test r8, r8
    jz 3f
2:
    movzx eax, byte ptr [rcx]
    movzx r9d, byte ptr [rdx]
    sub eax, r9d
    jnz 3f
    inc rcx
    inc rdx
    dec r8
    jnz 2b
3:
    ret
"#
);

/// Required by the MSVC ABI whenever floating point is used.
#[unsafe(no_mangle)]
pub static _fltused: i32 = 0;

/// The precompiled `core` library carries unwind tables that name the MSVC
/// C++ personality routine. Meridian programs are built with `panic=abort`,
/// so it is never actually invoked; if it ever were, unwinding is not
/// supported and the process ends.
#[unsafe(no_mangle)]
pub extern "C" fn __CxxFrameHandler3() -> ! {
    exit_process(status::NOT_IMPLEMENTED as u64)
}

// --------------------------------------------------------------------------
// System calls
// --------------------------------------------------------------------------

/// Issues system call `number` with up to four arguments.
#[inline(always)]
pub fn syscall(number: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    let ret: i64;
    // SAFETY: the kernel validates every argument; only rcx/r11 are
    // clobbered by the syscall instruction itself.
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") number as i64 => ret,
            in("rdi") a0,
            in("rsi") a1,
            in("rdx") a2,
            in("r10") a3,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret
}

pub fn debug_print(text: &str) {
    syscall(service::DEBUG_PRINT, text.as_ptr() as u64, text.len() as u64, 0, 0);
}

pub fn exit_process(code: u64) -> ! {
    syscall(service::EXIT_PROCESS, code, 0, 0, 0);
    unreachable_halt()
}

pub fn exit_thread(code: u64) -> ! {
    syscall(service::EXIT_THREAD, code, 0, 0, 0);
    unreachable_halt()
}

fn unreachable_halt() -> ! {
    // The kernel never returns from an exit call; if it somehow did, fault
    // so the process is terminated rather than running on.
    loop {
        // SAFETY: `ud2` raises an invalid-opcode exception by design.
        unsafe { asm!("ud2", options(nomem, nostack, noreturn)) };
    }
}

pub fn yield_execution() {
    syscall(service::YIELD, 0, 0, 0, 0);
}

pub fn sleep(ms: u64) {
    syscall(service::SLEEP, ms, 0, 0, 0);
}

pub fn current_process_id() -> u64 {
    syscall(service::CURRENT_PROCESS_ID, 0, 0, 0, 0) as u64
}

pub fn current_thread_id() -> u64 {
    syscall(service::CURRENT_THREAD_ID, 0, 0, 0, 0) as u64
}

pub fn uptime_us() -> u64 {
    syscall(service::UPTIME_US, 0, 0, 0, 0) as u64
}

/// Commits `size` bytes of zeroed memory.
pub fn allocate_memory(size: usize, protection: u64) -> Result<*mut u8, i64> {
    let r = syscall(service::ALLOCATE_MEMORY, size as u64, protection, 0, 0);
    if status::is_error(r) { Err(r) } else { Ok(r as *mut u8) }
}

pub fn free_memory(address: *mut u8, size: usize) -> Result<(), i64> {
    let r = syscall(service::FREE_MEMORY, address as u64, size as u64, 0, 0);
    if status::is_error(r) { Err(r) } else { Ok(()) }
}

/// A kernel object handle, closed on drop.
pub struct Handle(u64);

impl Handle {
    pub fn raw(&self) -> u64 {
        self.0
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        syscall(service::CLOSE_HANDLE, self.0, 0, 0, 0);
    }
}

pub fn create_event(manual_reset: bool, initially_set: bool) -> Result<Handle, i64> {
    let r = syscall(service::CREATE_EVENT, manual_reset as u64, initially_set as u64, 0, 0);
    if status::is_error(r) { Err(r) } else { Ok(Handle(r as u64)) }
}

pub fn set_event(event: &Handle) -> Result<(), i64> {
    let r = syscall(service::SET_EVENT, event.0, 0, 0, 0);
    if status::is_error(r) { Err(r) } else { Ok(()) }
}

/// Waits for `object`; returns `status::SUCCESS` or `status::TIMEOUT`.
pub fn wait(object: &Handle, timeout_ms: Option<u64>) -> i64 {
    syscall(service::WAIT, object.0, timeout_ms.unwrap_or(u64::MAX), 0, 0)
}

/// Formats into a fixed buffer without allocating (for `debug_print`).
pub struct StackWriter<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> StackWriter<N> {
    pub const fn new() -> Self {
        Self { buf: [0; N], len: 0 }
    }

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("<invalid utf-8>")
    }
}

impl<const N: usize> Default for StackWriter<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> core::fmt::Write for StackWriter<N> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let n = s.len().min(N - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}

/// `print!`-style helper that formats on the stack and calls `debug_print`.
#[macro_export]
macro_rules! println {
    ($($arg:tt)*) => {{
        let mut w = $crate::StackWriter::<256>::new();
        let _ = core::fmt::Write::write_fmt(&mut w, format_args!($($arg)*));
        let _ = core::fmt::Write::write_str(&mut w, "\n");
        $crate::debug_print(w.as_str());
    }};
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    let mut w = StackWriter::<256>::new();
    let _ = core::fmt::Write::write_fmt(&mut w, format_args!("user-mode panic: {info}\n"));
    debug_print(w.as_str());
    exit_process(status::UNSUCCESSFUL as u64)
}
