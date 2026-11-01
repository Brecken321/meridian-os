//! Native system call services. Every pointer and handle that comes from
//! user mode is validated; bad input returns an error status and never
//! crashes the kernel.

use alloc::sync::Arc;
use alloc::vec;

use meridian_abi::{protection, service, status};

use crate::arch::x86_64::syscall::SyscallFrame;
use crate::ke::objects::Event;
use crate::ke::sched::{self, WaitStatus};
use crate::mm::user::{self, Protection, USER_LIMIT};
use crate::ob::Object;

const MAX_DEBUG_PRINT: usize = 4096;
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
            None => status::INVALID_HANDLE,
        },
        service::WAIT => {
            let object = process.handles.lock().get(a0);
            let timeout = if a1 == u64::MAX { None } else { Some(a1) };
            match object {
                Some(Object::Event(e)) => match e.wait(timeout) {
                    WaitStatus::Signaled => status::SUCCESS,
                    WaitStatus::Timeout => status::TIMEOUT,
                },
                None => status::INVALID_HANDLE,
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
        _ => status::INVALID_SYSTEM_SERVICE,
    }
}
