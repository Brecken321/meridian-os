//! Native system call services. Every pointer and handle that comes from
//! user mode is validated; bad input returns an error status and never
//! crashes the kernel.

use alloc::sync::Arc;
use alloc::vec;

use meridian_abi::{
    InputEvent, ProcessInfo, ThreadInfo, VolumeInfo, dir_record, power, protection, service, status, volume_flags,
};

use crate::arch::x86_64::syscall::SyscallFrame;
use crate::fs::FsError;
use crate::ke::objects::Event;
use crate::ke::sched::{self, WaitStatus};
use crate::mm::user::{self, Protection, USER_LIMIT};
use crate::ob::Object;

const MAX_PATH: usize = 512;

/// Reads a UTF-8 path argument from user memory.
fn user_path(ptr: u64, len: u64) -> Result<alloc::string::String, i64> {
    if len == 0 || len as usize > MAX_PATH {
        return Err(status::INVALID_PARAMETER);
    }
    let mut buf = vec![0u8; len as usize];
    user::copy_from_user(&mut buf, ptr).map_err(|_| status::ACCESS_VIOLATION)?;
    alloc::string::String::from_utf8(buf).map_err(|_| status::INVALID_PARAMETER)
}

fn fs_status(e: FsError) -> i64 {
    match e {
        FsError::NotFound | FsError::NoSuchVolume => status::OBJECT_NAME_NOT_FOUND,
        FsError::BadPath => status::INVALID_PARAMETER,
        FsError::IsADirectory => status::FILE_IS_A_DIRECTORY,
        FsError::NotADirectory => status::NOT_A_DIRECTORY,
        FsError::Io(_) => status::DEVICE_NOT_READY,
        FsError::NoMemory | FsError::TooLarge => status::NO_MEMORY,
        FsError::NotRecognized | FsError::Unsupported | FsError::Corrupt => status::UNSUCCESSFUL,
        FsError::ReadOnly => status::MEDIA_WRITE_PROTECTED,
        FsError::AlreadyExists => status::OBJECT_NAME_COLLISION,
        FsError::NotEmpty => status::DIRECTORY_NOT_EMPTY,
        FsError::InvalidName => status::OBJECT_NAME_INVALID,
        FsError::DiskFull => status::DISK_FULL,
        FsError::NotSameDrive => status::NOT_SAME_DEVICE,
    }
}

/// Views a plain-old-data value as bytes for copying to user mode.
fn bytes_of<T: Copy>(value: &T) -> &[u8] {
    // SAFETY: only used with #[repr(C)] ABI structs without padding bytes
    // that could leak kernel data (all fields are explicitly sized).
    unsafe { core::slice::from_raw_parts(value as *const T as *const u8, core::mem::size_of::<T>()) }
}

fn copy_name(dst: &mut [u8], name: &str) {
    let n = name.len().min(dst.len());
    dst[..n].copy_from_slice(&name.as_bytes()[..n]);
}

const MAX_DEBUG_PRINT: usize = 4096;
/// Largest file `WRITE_FILE` accepts in one call.
const MAX_WRITE: u64 = 64 * 1024 * 1024;
const MAX_ALLOCATION: u64 = 1 << 30;

pub fn dispatch(frame: &mut SyscallFrame) -> i64 {
    let thread = sched::current();
    let Some(process) = thread.process() else {
        panic!("system call from kernel thread {}", thread.id);
    };
    // A non-canonical return address would make SYSRET fault in kernel mode.
    if frame.rcx >= USER_LIMIT {
        drop(thread);
        process.mark_exited(status::ACCESS_VIOLATION);
        sched::exit_current();
    }
    let (a0, a1) = (frame.rdi, frame.rsi);
    match frame.rax {
        service::DEBUG_PRINT => {
            let len = a1 as usize;
            if len > MAX_DEBUG_PRINT {
                return status::INVALID_PARAMETER;
            }
            let mut buf = vec![0u8; len];
            if user::copy_from_user(&mut buf, a0).is_err() {
                return status::ACCESS_VIOLATION;
            }
            let text = alloc::string::String::from_utf8_lossy(&buf);
            crate::info!("[{} {}] {}", process.name(), process.id, text.trim_end());
            status::SUCCESS
        }
        service::EXIT_PROCESS | service::EXIT_THREAD => {
            // Processes have exactly one thread until thread creation from
            // user mode is implemented, so both end the process.
            drop(thread);
            process.mark_exited(a0 as i64);
            drop(process);
            sched::exit_current()
        }
        service::YIELD => {
            sched::yield_now();
            status::SUCCESS
        }
        service::SLEEP => {
            sched::sleep_ms(a0.min(u32::MAX as u64));
            status::SUCCESS
        }
        service::CURRENT_PROCESS_ID => process.id as i64,
        service::CURRENT_THREAD_ID => thread.id as i64,
        service::UPTIME_US => crate::ke::clock::uptime_us() as i64,
        service::ALLOCATE_MEMORY => {
            if a0 == 0 || a0 > MAX_ALLOCATION {
                return status::INVALID_PARAMETER;
            }
            let prot = match a1 {
                protection::READ => Protection::ReadOnly,
                protection::READ_WRITE => Protection::ReadWrite,
                _ => return status::INVALID_PARAMETER,
            };
            match process.space.allocate(a0, prot) {
                Ok(base) => base as i64,
                Err(_) => status::NO_MEMORY,
            }
        }
        service::FREE_MEMORY => match process.space.free(a0, a1) {
            Ok(()) => status::SUCCESS,
            Err(_) => status::INVALID_PARAMETER,
        },
        service::CREATE_EVENT => {
            let event = Arc::new(Event::new(a0 != 0, a1 != 0));
            match process.handles.lock().insert(Object::Event(event)) {
                Some(h) => h as i64,
                None => status::NO_MEMORY,
            }
        }
        service::SET_EVENT => match process.handles.lock().get(a0) {
            Some(Object::Event(e)) => {
                e.set();
                status::SUCCESS
            }
            Some(_) => status::OBJECT_TYPE_MISMATCH,
            None => status::INVALID_HANDLE,
        },
        service::WAIT => {
            let object = process.handles.lock().get(a0);
            let timeout = if a1 == u64::MAX { None } else { Some(a1) };
            let result = match object {
                Some(Object::Event(e)) => e.wait(timeout),
                Some(Object::Process(p)) => p.terminated.wait(timeout),
                None => return status::INVALID_HANDLE,
            };
            match result {
                WaitStatus::Signaled => status::SUCCESS,
                WaitStatus::Timeout => status::TIMEOUT,
            }
        }
        service::CLOSE_HANDLE => {
            let removed = process.handles.lock().remove(a0);
            match removed {
                Some(object) => {
                    drop(object);
                    status::SUCCESS
                }
                None => status::INVALID_HANDLE,
            }
        }
        service::QUERY_SYSTEM_INFO => {
            let size = core::mem::size_of::<meridian_abi::SystemInfo>();
            if (a1 as usize) < size {
                return status::BUFFER_TOO_SMALL;
            }
            let (free, total) = crate::mm::pmm::stats();
            let mut info = meridian_abi::SystemInfo {
                page_size: crate::mm::PAGE_SIZE as u32,
                processor_count: crate::ke::pcr::online_count(),
                physical_memory: total,
                available_memory: free,
                uptime_us: crate::ke::clock::uptime_us(),
                kernel_version: [0; 16],
            };
            let v = crate::VERSION.as_bytes();
            info.kernel_version[..v.len().min(16)].copy_from_slice(&v[..v.len().min(16)]);
            // SAFETY: SystemInfo is plain old data with no padding bytes.
            let bytes = unsafe { core::slice::from_raw_parts(&info as *const _ as *const u8, size) };
            match user::copy_to_user(a0, bytes) {
                Ok(()) => size as i64,
                Err(_) => status::ACCESS_VIOLATION,
            }
        }
        service::READ_INPUT => {
            if !crate::drivers::input::capture(process.id) {
                return status::ACCESS_DENIED;
            }
            let max = (a1 as usize).clamp(1, 64);
            let timeout = if frame.rdx == u64::MAX { None } else { Some(frame.rdx) };
            let mut events = vec![InputEvent::default(); max];
            let n = crate::drivers::input::read(&mut events, timeout);
            for (i, e) in events.iter().take(n).enumerate() {
                if user::copy_to_user(a0 + (i * core::mem::size_of::<InputEvent>()) as u64, bytes_of(e)).is_err() {
                    return status::ACCESS_VIOLATION;
                }
            }
            n as i64
        }
        service::ACQUIRE_DISPLAY => match crate::drivers::display::acquire(process.id, &process.space) {
            Ok((address, info)) => {
                if user::copy_to_user(a0, bytes_of(&info)).is_err() {
                    return status::ACCESS_VIOLATION;
                }
                crate::info!("display: owned by process {} ({})", process.id, process.name());
                address as i64
            }
            Err(e) => {
                crate::warn!("display: {} cannot have the display: {e}", process.name());
                status::ACCESS_DENIED
            }
        },
        service::LIST_DIRECTORY => {
            let path = match user_path(a0, a1) {
                Ok(p) => p,
                Err(s) => return s,
            };
            let entries = match crate::fs::list(&path) {
                Ok(e) => e,
                Err(e) => return fs_status(e),
            };
            let mut out = alloc::vec::Vec::new();
            for e in entries {
                let mut flags = 0;
                if e.is_dir {
                    flags |= dir_record::DIRECTORY;
                }
                if e.hidden {
                    flags |= dir_record::HIDDEN;
                }
                if e.read_only {
                    flags |= dir_record::READ_ONLY;
                }
                out.extend_from_slice(&e.size.to_le_bytes());
                out.extend_from_slice(&flags.to_le_bytes());
                out.extend_from_slice(&(e.name.len() as u32).to_le_bytes());
                out.extend_from_slice(e.name.as_bytes());
                out.resize(out.len().next_multiple_of(8), 0);
            }
            if out.len() as u64 > frame.r10 {
                return status::BUFFER_TOO_SMALL;
            }
            match user::copy_to_user(frame.rdx, &out) {
                Ok(()) => out.len() as i64,
                Err(_) => status::ACCESS_VIOLATION,
            }
        }
        service::READ_FILE => {
            let path = match user_path(a0, a1) {
                Ok(p) => p,
                Err(s) => return s,
            };
            match crate::fs::read_file(&path) {
                Ok(data) if data.len() as u64 > frame.r10 => status::BUFFER_TOO_SMALL,
                Ok(data) => match user::copy_to_user(frame.rdx, &data) {
                    Ok(()) => data.len() as i64,
                    Err(_) => status::ACCESS_VIOLATION,
                },
                Err(e) => fs_status(e),
            }
        }
        service::FILE_SIZE => {
            let path = match user_path(a0, a1) {
                Ok(p) => p,
                Err(s) => return s,
            };
            match crate::fs::metadata(&path) {
                Ok(Some(e)) if !e.is_dir => e.size as i64,
                Ok(_) => status::FILE_IS_A_DIRECTORY,
                Err(e) => fs_status(e),
            }
        }
        service::CREATE_PROCESS => {
            let path = match user_path(a0, a1) {
                Ok(p) => p,
                Err(s) => return s,
            };
            let image = match crate::fs::read_file(&path) {
                Ok(data) => data,
                Err(e) => return fs_status(e),
            };
            let name = path.rsplit(['\\', '/']).next().unwrap_or("program");
            match crate::ps::process::spawn(name, &image, frame.rdx) {
                Ok(child) => match process.handles.lock().insert(Object::Process(child)) {
                    Some(h) => h as i64,
                    None => status::NO_MEMORY,
                },
                Err(e) => {
                    crate::warn!("{} could not start {path}: {e}", process.name());
                    status::INVALID_IMAGE_FORMAT
                }
            }
        }
        service::PROCESS_STATUS => match process.handles.lock().get(a0) {
            Some(Object::Process(p)) => p.exit_status().unwrap_or(status::STILL_ACTIVE),
            Some(_) => status::OBJECT_TYPE_MISMATCH,
            None => status::INVALID_HANDLE,
        },
        service::QUERY_THREADS => {
            let max = a1 as usize / core::mem::size_of::<ThreadInfo>();
            let mut written = 0usize;
            for t in sched::threads().into_iter().take(max) {
                let mut info = ThreadInfo {
                    id: t.id,
                    cpu_ms: t.cpu_ticks.load(core::sync::atomic::Ordering::Relaxed),
                    priority: t.priority() as u32,
                    state: t.state() as u32,
                    cpu: t.last_cpu(),
                    process: t.process().map_or(0, |p| p.id as u32),
                    name: [0; 24],
                };
                copy_name(&mut info.name, t.name());
                let at = a0 + (written * core::mem::size_of::<ThreadInfo>()) as u64;
                if user::copy_to_user(at, bytes_of(&info)).is_err() {
                    return status::ACCESS_VIOLATION;
                }
                written += 1;
            }
            written as i64
        }
        service::QUERY_PROCESSES => {
            let max = a1 as usize / core::mem::size_of::<ProcessInfo>();
            let mut written = 0usize;
            for p in crate::ps::process::list().into_iter().take(max) {
                let mut info = ProcessInfo {
                    id: p.id,
                    committed: p.space.committed(),
                    exit_status: p.exit_status().unwrap_or(status::STILL_ACTIVE),
                    handles: p.handles.lock().open_count() as u32,
                    running: p.exit_status().is_none() as u32,
                    name: [0; 32],
                };
                copy_name(&mut info.name, p.name());
                let at = a0 + (written * core::mem::size_of::<ProcessInfo>()) as u64;
                if user::copy_to_user(at, bytes_of(&info)).is_err() {
                    return status::ACCESS_VIOLATION;
                }
                written += 1;
            }
            written as i64
        }
        service::QUERY_TIME => {
            let now = crate::hal::rtc::now();
            match user::copy_to_user(a0, bytes_of(&now)) {
                Ok(()) => status::SUCCESS,
                Err(_) => status::ACCESS_VIOLATION,
            }
        }
        service::WRITE_FILE => {
            let path = match user_path(a0, a1) {
                Ok(p) => p,
                Err(s) => return s,
            };
            if frame.r10 > MAX_WRITE {
                return status::INVALID_PARAMETER;
            }
            let mut data = alloc::vec::Vec::new();
            if data.try_reserve_exact(frame.r10 as usize).is_err() {
                return status::NO_MEMORY;
            }
            data.resize(frame.r10 as usize, 0);
            if user::copy_from_user(&mut data, frame.rdx).is_err() {
                return status::ACCESS_VIOLATION;
            }
            match crate::fs::write_file(&path, &data) {
                Ok(()) => status::SUCCESS,
                Err(e) => fs_status(e),
            }
        }
        service::CREATE_DIRECTORY | service::DELETE_FILE => {
            let path = match user_path(a0, a1) {
                Ok(p) => p,
                Err(s) => return s,
            };
            let result = if frame.rax == service::CREATE_DIRECTORY {
                crate::fs::create_dir(&path)
            } else {
                crate::fs::delete(&path)
            };
            match result {
                Ok(()) => status::SUCCESS,
                Err(e) => fs_status(e),
            }
        }
        service::RENAME => {
            let (from, to) = match (user_path(a0, a1), user_path(frame.rdx, frame.r10)) {
                (Ok(f), Ok(t)) => (f, t),
                (Err(s), _) | (_, Err(s)) => return s,
            };
            match crate::fs::rename(&from, &to) {
                Ok(()) => status::SUCCESS,
                Err(e) => fs_status(e),
            }
        }
        service::QUERY_VOLUMES => {
            let max = a1 as usize / core::mem::size_of::<VolumeInfo>();
            let mut written = 0usize;
            for v in crate::fs::volumes().into_iter().take(max) {
                let mut info = VolumeInfo {
                    letter: v.letter as u32,
                    flags: if v.fs.writable() { volume_flags::WRITABLE } else { 0 },
                    total_bytes: v.fs.size_bytes(),
                    free_bytes: v.fs.free_bytes(),
                    label: [0; 16],
                };
                copy_name(&mut info.label, &v.fs.label);
                let at = a0 + (written * core::mem::size_of::<VolumeInfo>()) as u64;
                if user::copy_to_user(at, bytes_of(&info)).is_err() {
                    return status::ACCESS_VIOLATION;
                }
                written += 1;
            }
            written as i64
        }
        service::POWER => match a0 {
            power::SHUT_DOWN => {
                crate::info!("power: shutdown requested by {} ({})", process.name(), process.id);
                match crate::hal::power::shutdown() {
                    Ok(()) => status::SUCCESS,
                    Err(e) => {
                        crate::warn!("power: shutdown failed: {e}");
                        status::UNSUCCESSFUL
                    }
                }
            }
            power::RESTART => {
                crate::info!("power: restart requested by {} ({})", process.name(), process.id);
                crate::hal::power::reboot()
            }
            _ => status::INVALID_PARAMETER,
        },
        _ => status::INVALID_SYSTEM_SERVICE,
    }
}
