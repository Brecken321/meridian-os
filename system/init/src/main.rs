//! `init`: the first user-mode program the Meridian kernel starts.
//!
//! For now it checks the user-mode environment end to end (system calls,
//! memory, handles, events, timeouts and the kernel's validation of bad
//! arguments) and reports the result through its exit status. It will grow
//! into the session manager that starts system services.
//!
//! The kernel passes a mode in the first argument; non-zero modes make the
//! program misbehave on purpose so the kernel's tests can verify that a
//! faulting process is terminated without affecting the rest of the system.

#![no_std]
#![no_main]

use core::arch::asm;

use meridian_rt::{self as rt, println, protection, service, status};

const MODE_SELF_CHECK: u64 = 0;
const MODE_WRITE_UNMAPPED: u64 = 1;
const MODE_PRIVILEGED_INSTRUCTION: u64 = 2;
const MODE_READ_KERNEL_MEMORY: u64 = 3;

/// Program entry point (named by the linker's `-entry` option).
#[unsafe(no_mangle)]
pub extern "C" fn meridian_start(mode: u64) -> ! {
    // SAFETY (all three): deliberately invalid operations; the kernel is
    // expected to terminate this process at the faulting instruction.
    match mode {
        MODE_WRITE_UNMAPPED => unsafe { core::ptr::write_volatile(0x100 as *mut u64, 1) },
        MODE_PRIVILEGED_INSTRUCTION => unsafe { asm!("cli", options(nomem, nostack)) },
        MODE_READ_KERNEL_MEMORY => unsafe {
            core::ptr::read_volatile(0xFFFF_FFFF_8000_0000 as *const u64);
        },
        _ => {}
    }
    if mode != MODE_SELF_CHECK {
        println!("init: mode {mode} did not fault as expected");
        rt::exit_process(2);
    }
    match self_check() {
        Ok(()) => {
            println!("init: all user-mode checks passed");
            rt::exit_process(0)
        }
        Err(what) => {
            println!("init: FAILED: {what}");
            rt::exit_process(1)
        }
    }
}

fn self_check() -> Result<(), &'static str> {
    let pid = rt::current_process_id();
    let tid = rt::current_thread_id();
    println!("init: hello from ring 3 (process {pid}, thread {tid})");

    // Memory: fresh pages are zeroed, writable and returned on free.
    const SIZE: usize = 256 * 1024;
    let p = rt::allocate_memory(SIZE, protection::READ_WRITE).map_err(|_| "allocate_memory failed")?;
    // SAFETY: the kernel just committed SIZE bytes at `p`.
    let words = unsafe { core::slice::from_raw_parts_mut(p as *mut u64, SIZE / 8) };
    if words.iter().any(|&w| w != 0) {
        return Err("newly allocated memory is not zeroed");
    }
    for (i, w) in words.iter_mut().enumerate() {
        *w = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
    if words.iter().enumerate().any(|(i, &w)| w != (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)) {
        return Err("memory read back differs");
    }
    rt::free_memory(p, SIZE).map_err(|_| "free_memory failed")?;
    if rt::free_memory(p, SIZE).is_ok() {
        return Err("freeing memory twice succeeded");
    }

    // Events, waits with timeouts and handle lifetime.
    let event = rt::create_event(false, false).map_err(|_| "create_event failed")?;
    let t0 = rt::uptime_us();
    if rt::wait(&event, Some(20)) != status::TIMEOUT {
        return Err("waiting on an unset event did not time out");
    }
    if rt::uptime_us() - t0 < 20_000 {
        return Err("a 20 ms timeout returned early");
    }
    rt::set_event(&event).map_err(|_| "set_event failed")?;
    if rt::wait(&event, Some(1000)) != status::SUCCESS {
        return Err("waiting on a set event failed");
    }
    let raw = event.raw();
    drop(event);
    if rt::syscall(service::WAIT, raw, 0, 0, 0) != status::INVALID_HANDLE {
        return Err("a closed handle still works");
    }

    // The kernel must reject bad arguments instead of crashing.
    if rt::syscall(service::DEBUG_PRINT, 0xFFFF_FFFF_8000_0000, 16, 0, 0) != status::ACCESS_VIOLATION {
        return Err("debug_print accepted a kernel pointer");
    }
    if rt::syscall(service::DEBUG_PRINT, 0x100, 16, 0, 0) != status::ACCESS_VIOLATION {
        return Err("debug_print accepted an unmapped pointer");
    }
    if rt::syscall(999, 0, 0, 0, 0) != status::INVALID_SYSTEM_SERVICE {
        return Err("an unknown system service was accepted");
    }
    if rt::syscall(service::ALLOCATE_MEMORY, 0, protection::READ_WRITE, 0, 0) != status::INVALID_PARAMETER {
        return Err("a zero-byte allocation was accepted");
    }

    // Scheduling services.
    rt::yield_execution();
    let t1 = rt::uptime_us();
    rt::sleep(5);
    if rt::uptime_us() - t1 < 5_000 {
        return Err("sleep(5) returned early");
    }
    Ok(())
}
