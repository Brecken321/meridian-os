//! Wrappers for the system services the desktop and other interactive
//! programs use: input, display, files, processes, clock and power.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::{Handle, service, status, syscall};
pub use meridian_abi::{DisplayInfo, InputEvent, ProcessInfo, ThreadInfo, WallTime, dir_record, input, key, power};

fn check(r: i64) -> Result<i64, i64> {
    if status::is_error(r) { Err(r) } else { Ok(r) }
}

/// Takes over keyboard and mouse input and waits up to `timeout_ms` for
/// events (`None` = forever). Returns how many were stored in `out`.
pub fn read_input(out: &mut [InputEvent], timeout_ms: Option<u64>) -> Result<usize, i64> {
    let r = syscall(service::READ_INPUT, out.as_mut_ptr() as u64, out.len() as u64, timeout_ms.unwrap_or(u64::MAX), 0);
    check(r).map(|n| n as usize)
}

/// Maps the framebuffer into this process. Returns its address and layout.
pub fn acquire_display() -> Result<(*mut u32, DisplayInfo), i64> {
    let mut info = DisplayInfo::default();
    let r = syscall(service::ACQUIRE_DISPLAY, &mut info as *mut DisplayInfo as u64, 0, 0, 0);
    check(r).map(|address| (address as *mut u32, info))
}

#[derive(Clone, Debug)]
pub struct DirEntry {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
    pub hidden: bool,
    pub read_only: bool,
}

/// Lists the directory at `path` (for example `C:\MERIDIAN`).
pub fn list_directory(path: &str) -> Result<Vec<DirEntry>, i64> {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let r = syscall(
            service::LIST_DIRECTORY,
            path.as_ptr() as u64,
            path.len() as u64,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        );
        if r == status::BUFFER_TOO_SMALL && buf.len() < 16 * 1024 * 1024 {
            buf.resize(buf.len() * 4, 0);
            continue;
        }
        let len = check(r)? as usize;
        let mut entries = Vec::new();
        let mut at = 0;
        while at + dir_record::HEADER <= len {
            let size = u64::from_le_bytes(buf[at..at + 8].try_into().unwrap());
            let flags = u32::from_le_bytes(buf[at + 8..at + 12].try_into().unwrap());
            let name_len = u32::from_le_bytes(buf[at + 12..at + 16].try_into().unwrap()) as usize;
            let name_start = at + dir_record::HEADER;
            let name = String::from_utf8_lossy(&buf[name_start..(name_start + name_len).min(len)]).into_owned();
            entries.push(DirEntry {
                name,
                size,
                is_dir: flags & dir_record::DIRECTORY != 0,
                hidden: flags & dir_record::HIDDEN != 0,
                read_only: flags & dir_record::READ_ONLY != 0,
            });
            at = (name_start + name_len).next_multiple_of(8);
        }
        return Ok(entries);
    }
}

pub fn file_size(path: &str) -> Result<u64, i64> {
    check(syscall(service::FILE_SIZE, path.as_ptr() as u64, path.len() as u64, 0, 0)).map(|n| n as u64)
}

/// Reads a whole file.
pub fn read_file(path: &str) -> Result<Vec<u8>, i64> {
    let size = file_size(path)? as usize;
    let mut buf = vec![0u8; size.max(1)];
    let n = check(syscall(
        service::READ_FILE,
        path.as_ptr() as u64,
        path.len() as u64,
        buf.as_mut_ptr() as u64,
        buf.len() as u64,
    ))? as usize;
    buf.truncate(n);
    Ok(buf)
}

/// Starts the program at `path`; the handle can be waited on.
pub fn create_process(path: &str, argument: u64) -> Result<Handle, i64> {
    check(syscall(service::CREATE_PROCESS, path.as_ptr() as u64, path.len() as u64, argument, 0))
        .map(|h| Handle(h as u64))
}

/// Exit status of a process, or `None` while it runs.
pub fn process_status(process: &Handle) -> Result<Option<i64>, i64> {
    let r = syscall(service::PROCESS_STATUS, process.raw(), 0, 0, 0);
    if r == status::STILL_ACTIVE { Ok(None) } else { check(r).map(Some).or(Ok(Some(r))) }
}

pub fn query_threads() -> Vec<ThreadInfo> {
    let mut out = vec![ThreadInfo::default(); 256];
    let size = core::mem::size_of_val(out.as_slice()) as u64;
    let n = syscall(service::QUERY_THREADS, out.as_mut_ptr() as u64, size, 0, 0).max(0) as usize;
    out.truncate(n);
    out
}

pub fn query_processes() -> Vec<ProcessInfo> {
    let mut out = vec![ProcessInfo::default(); 128];
    let size = core::mem::size_of_val(out.as_slice()) as u64;
    let n = syscall(service::QUERY_PROCESSES, out.as_mut_ptr() as u64, size, 0, 0).max(0) as usize;
    out.truncate(n);
    out
}

/// Local date and time from the real-time clock.
pub fn query_time() -> WallTime {
    let mut t = WallTime::default();
    syscall(service::QUERY_TIME, &mut t as *mut WallTime as u64, 0, 0, 0);
    t
}

/// Shuts the computer down or restarts it. Returns only on failure.
pub fn power(action: u64) -> i64 {
    syscall(service::POWER, action, 0, 0, 0)
}

/// A C-string-like name field (NUL padded) as text.
pub fn name_of(bytes: &[u8]) -> &str {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    core::str::from_utf8(&bytes[..end]).unwrap_or("?")
}
