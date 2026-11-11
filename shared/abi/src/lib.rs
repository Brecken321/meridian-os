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
    /// `read_input(events, max_events, timeout_ms) -> count`: takes over the
    /// keyboard and mouse for the calling process and returns up to
    /// `max_events` [`super::InputEvent`]s (0 on timeout; `u64::MAX` waits
    /// forever).
    pub const READ_INPUT: u64 = 15;
    /// `acquire_display(info) -> address`: maps the framebuffer into the
    /// calling process, fills a [`super::DisplayInfo`] and stops the kernel
    /// console from drawing. One process may own the display at a time.
    pub const ACQUIRE_DISPLAY: u64 = 16;
    /// `list_directory(path, path_len, buffer, length) -> bytes`: fills the
    /// buffer with directory records (see [`super::dir_record`]).
    pub const LIST_DIRECTORY: u64 = 17;
    /// `read_file(path, path_len, buffer, length) -> size`: reads a whole
    /// file; `BUFFER_TOO_SMALL` if `length` is less than its size.
    pub const READ_FILE: u64 = 18;
    /// `file_size(path, path_len) -> size`
    pub const FILE_SIZE: u64 = 19;
    /// `create_process(path, path_len, argument) -> process handle`
    pub const CREATE_PROCESS: u64 = 20;
    /// `process_status(process handle) -> exit status or STILL_ACTIVE`
    pub const PROCESS_STATUS: u64 = 21;
    /// `query_threads(buffer, length) -> count` of [`super::ThreadInfo`]
    pub const QUERY_THREADS: u64 = 22;
    /// `query_processes(buffer, length) -> count` of [`super::ProcessInfo`]
    pub const QUERY_PROCESSES: u64 = 23;
    /// `query_time(buffer)`: fills a [`super::WallTime`] (local time).
    pub const QUERY_TIME: u64 = 24;
    /// `power(action)`: [`super::power`] action; returns only on failure.
    pub const POWER: u64 = 25;
    /// `write_file(path, path_len, buffer, length)`: creates the file or
    /// replaces its contents. Only drives on a Meridian data partition are
    /// writable (`MEDIA_WRITE_PROTECTED` otherwise).
    pub const WRITE_FILE: u64 = 26;
    /// `create_directory(path, path_len)`
    pub const CREATE_DIRECTORY: u64 = 27;
    /// `delete_file(path, path_len)`: deletes a file or an empty folder.
    pub const DELETE_FILE: u64 = 28;
    /// `rename(from, from_len, to, to_len)`: renames or moves within a drive.
    pub const RENAME: u64 = 29;
    /// `query_volumes(buffer, length) -> count` of [`super::VolumeInfo`]
    pub const QUERY_VOLUMES: u64 = 30;

    pub const COUNT: u64 = 31;
}

/// Actions for `POWER`.
pub mod power {
    pub const SHUT_DOWN: u64 = 0;
    pub const RESTART: u64 = 1;
}

/// Keyboard and mouse event delivered by `READ_INPUT`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct InputEvent {
    /// [`input::KEY`] or [`input::MOUSE`].
    pub kind: u32,
    /// Key code ([`key`]) for key events.
    pub key: u32,
    /// The character a key produces (with shift/caps applied), or 0.
    pub ch: u32,
    /// [`input`] flag bits.
    pub flags: u32,
    /// Relative mouse movement (right and down are positive), or the
    /// absolute position when `flags` has [`input::ABSOLUTE`].
    pub dx: i32,
    pub dy: i32,
    /// Mouse buttons held: bit 0 left, bit 1 right, bit 2 middle.
    pub buttons: u32,
    /// Mouse wheel movement in notches; positive scrolls down.
    pub wheel: i32,
}

/// `InputEvent` kinds and flags.
pub mod input {
    pub const KEY: u32 = 1;
    pub const MOUSE: u32 = 2;
    pub const PRESSED: u32 = 1 << 0;
    pub const SHIFT: u32 = 1 << 1;
    pub const CTRL: u32 = 1 << 2;
    pub const ALT: u32 = 1 << 3;
    /// Mouse events: `dx`/`dy` are an absolute position scaled to
    /// 0..=0xFFFF across the screen instead of a movement.
    pub const ABSOLUTE: u32 = 1 << 4;
}

/// Key codes in `InputEvent::key`.
pub mod key {
    pub const CHAR: u32 = 1;
    pub const ENTER: u32 = 2;
    pub const BACKSPACE: u32 = 3;
    pub const TAB: u32 = 4;
    pub const ESCAPE: u32 = 5;
    pub const UP: u32 = 6;
    pub const DOWN: u32 = 7;
    pub const LEFT: u32 = 8;
    pub const RIGHT: u32 = 9;
    pub const HOME: u32 = 10;
    pub const END: u32 = 11;
    pub const PAGE_UP: u32 = 12;
    pub const PAGE_DOWN: u32 = 13;
    pub const INSERT: u32 = 14;
    pub const DELETE: u32 = 15;
    /// The Windows/Super key.
    pub const SUPER: u32 = 16;
    pub const MODIFIER: u32 = 17;
    /// F1 is `FUNCTION + 1`, and so on.
    pub const FUNCTION: u32 = 0x100;
    pub const OTHER: u32 = 0xFFFF;
}

/// Returned by `ACQUIRE_DISPLAY`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct DisplayInfo {
    pub width: u32,
    pub height: u32,
    /// Pixels per scan line.
    pub stride: u32,
    /// 0 = RGBX (red in the low byte), 1 = BGRX.
    pub format: u32,
    pub size: u64,
}

/// Directory records written by `LIST_DIRECTORY`, packed back to back:
/// `size: u64`, `flags: u32`, `name_len: u32`, then `name_len` bytes of
/// UTF-8, padded to a multiple of 8 bytes.
pub mod dir_record {
    pub const DIRECTORY: u32 = 1 << 0;
    pub const HIDDEN: u32 = 1 << 1;
    pub const READ_ONLY: u32 = 1 << 2;
    pub const HEADER: usize = 16;
}

/// Returned (as an array) by `QUERY_THREADS`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct ThreadInfo {
    pub id: u64,
    pub cpu_ms: u64,
    pub priority: u32,
    /// 0 initialised, 1 ready, 2 running, 3 waiting, 4 terminated.
    pub state: u32,
    pub cpu: u32,
    /// Owning process id, or 0 for kernel threads.
    pub process: u32,
    pub name: [u8; 24],
}

/// Returned (as an array) by `QUERY_PROCESSES`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessInfo {
    pub id: u64,
    pub committed: u64,
    pub exit_status: i64,
    pub handles: u32,
    /// 1 while the process is running.
    pub running: u32,
    pub name: [u8; 32],
}

/// Returned (as an array) by `QUERY_VOLUMES`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct VolumeInfo {
    /// Drive letter (ASCII).
    pub letter: u32,
    /// [`volume_flags`] bits.
    pub flags: u32,
    pub total_bytes: u64,
    pub free_bytes: u64,
    /// Volume label (UTF-8, zero-padded).
    pub label: [u8; 16],
}

pub mod volume_flags {
    /// The drive accepts writes.
    pub const WRITABLE: u32 = 1 << 0;
}

/// Returned by `QUERY_TIME`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct WallTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    /// 0 = Sunday.
    pub weekday: u8,
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
    pub const ACCESS_DENIED: i64 = 0xC000_0022_u32 as i32 as i64;
    pub const OBJECT_NAME_NOT_FOUND: i64 = 0xC000_0034_u32 as i32 as i64;
    pub const FILE_IS_A_DIRECTORY: i64 = 0xC000_00BA_u32 as i32 as i64;
    pub const NOT_A_DIRECTORY: i64 = 0xC000_0103_u32 as i32 as i64;
    pub const INVALID_IMAGE_FORMAT: i64 = 0xC000_007B_u32 as i32 as i64;
    pub const DEVICE_NOT_READY: i64 = 0xC000_00A3_u32 as i32 as i64;
    pub const MEDIA_WRITE_PROTECTED: i64 = 0xC000_00A2_u32 as i32 as i64;
    pub const OBJECT_NAME_INVALID: i64 = 0xC000_0033_u32 as i32 as i64;
    pub const OBJECT_NAME_COLLISION: i64 = 0xC000_0035_u32 as i32 as i64;
    pub const DISK_FULL: i64 = 0xC000_007F_u32 as i32 as i64;
    pub const DIRECTORY_NOT_EMPTY: i64 = 0xC000_0101_u32 as i32 as i64;
    pub const NOT_SAME_DEVICE: i64 = 0xC000_00D4_u32 as i32 as i64;
    /// Returned by `PROCESS_STATUS` while the process runs.
    pub const STILL_ACTIVE: i64 = 0x0000_0103;
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
