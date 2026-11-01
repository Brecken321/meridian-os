//! The Meridian native system call ABI, shared by the kernel and the
//! user-mode runtime.
//!
//! Calling convention (x86-64 `syscall` instruction):
//! * `rax` = service number, arguments in `rdi, rsi, rdx, r10, r8, r9`;
//! * return value in `rax`: a non-negative result, or a negative
//!   [`status`] code;
//! * `rcx` and `r11` are clobbered (the CPU uses them for the return
//!   address and flags); every other register is preserved.
//!
//! Status codes use the Windows NT `NTSTATUS` numbering so the Win32
//! subsystem can pass them through unchanged.

#![no_std]

/// System service numbers.
pub mod service {
    /// `debug_print(ptr, len)`: writes UTF-8 text to the kernel log.
    pub const DEBUG_PRINT: u64 = 0;
    /// `exit_process(code)`: terminates the calling process. Never returns.
    pub const EXIT_PROCESS: u64 = 1;
    /// `exit_thread(code)`: terminates the calling thread. Never returns.
    pub const EXIT_THREAD: u64 = 2;
    /// `yield_execution()`
    pub const YIELD: u64 = 3;
    /// `sleep(milliseconds)`
    pub const SLEEP: u64 = 4;
    /// `current_process_id() -> id`
    pub const CURRENT_PROCESS_ID: u64 = 5;
    /// `current_thread_id() -> id`
    pub const CURRENT_THREAD_ID: u64 = 6;
    /// `allocate_memory(size, protection) -> address`: commits zeroed,
    /// page-aligned memory. `protection` is a [`super::protection`] value.
    pub const ALLOCATE_MEMORY: u64 = 7;
    /// `free_memory(address, size)`: releases memory from `ALLOCATE_MEMORY`.
    pub const FREE_MEMORY: u64 = 8;
    /// `create_event(manual_reset, initially_set) -> handle`
    pub const CREATE_EVENT: u64 = 9;
    /// `set_event(handle)`
    pub const SET_EVENT: u64 = 10;
    /// `wait(handle, timeout_ms)`: `timeout_ms = u64::MAX` waits forever.
    /// Returns `status::SUCCESS` or `status::TIMEOUT`.
    pub const WAIT: u64 = 11;
    /// `close_handle(handle)`
    pub const CLOSE_HANDLE: u64 = 12;
    /// `uptime_us() -> microseconds since boot`
    pub const UPTIME_US: u64 = 13;
    /// `query_system_info(buffer, length) -> bytes written`: fills a
    /// [`super::SystemInfo`].
    pub const QUERY_SYSTEM_INFO: u64 = 14;

    pub const COUNT: u64 = 15;
}

/// `NTSTATUS`-compatible status codes (as signed 64-bit return values).
pub mod status {
    pub const SUCCESS: i64 = 0;
    pub const TIMEOUT: i64 = 0x0000_0102;
    pub const UNSUCCESSFUL: i64 = 0xC000_0001_u32 as i32 as i64;
    pub const NOT_IMPLEMENTED: i64 = 0xC000_0002_u32 as i32 as i64;
    pub const ACCESS_VIOLATION: i64 = 0xC000_0005_u32 as i32 as i64;
    pub const INVALID_HANDLE: i64 = 0xC000_0008_u32 as i32 as i64;
    pub const INVALID_PARAMETER: i64 = 0xC000_000D_u32 as i32 as i64;
    pub const NO_MEMORY: i64 = 0xC000_0017_u32 as i32 as i64;
    pub const OBJECT_TYPE_MISMATCH: i64 = 0xC000_0024_u32 as i32 as i64;
    pub const BUFFER_TOO_SMALL: i64 = 0xC000_0023_u32 as i32 as i64;
    pub const INVALID_SYSTEM_SERVICE: i64 = 0xC000_001C_u32 as i32 as i64;
    pub const ILLEGAL_INSTRUCTION: i64 = 0xC000_001D_u32 as i32 as i64;
    pub const PRIVILEGED_INSTRUCTION: i64 = 0xC000_0096_u32 as i32 as i64;
    pub const INTEGER_DIVIDE_BY_ZERO: i64 = 0xC000_0094_u32 as i32 as i64;
    pub const BREAKPOINT: i64 = 0x8000_0003_u32 as i32 as i64;

    pub fn is_error(s: i64) -> bool {
        s < 0
    }
}

/// Returned by `QUERY_SYSTEM_INFO`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemInfo {
    pub page_size: u32,
    pub processor_count: u32,
    /// RAM owned by the operating system, in bytes.
    pub physical_memory: u64,
    /// Free RAM, in bytes.
    pub available_memory: u64,
    pub uptime_us: u64,
    /// Kernel version, UTF-8, NUL padded.
    pub kernel_version: [u8; 16],
}

/// Page protections for `ALLOCATE_MEMORY`.
pub mod protection {
    pub const READ: u64 = 1;
    pub const READ_WRITE: u64 = 2;
}
