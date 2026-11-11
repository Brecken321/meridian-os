//! Process objects.

use alloc::collections::BTreeMap;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

use meridian_abi::status;

use super::pe;
use crate::arch::x86_64::idt::{self, TrapFrame};
use crate::ke::objects::Event;
use crate::ke::sched;
use crate::ke::thread::priority;
use crate::mm::user::{AddressSpace, Protection};
use crate::ob::HandleTable;
use crate::sync::SpinLock;

pub struct Process {
    pub id: u64,
    name: [u8; 32],
    name_len: usize,
    pub space: AddressSpace,
    pub handles: SpinLock<HandleTable>,
    exit_status: AtomicI64,
    exited: AtomicBool,
    /// Set when the process has terminated (manual reset).
    pub terminated: Event,
    image_base: AtomicU64,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(100);
static PROCESSES: SpinLock<BTreeMap<u64, Weak<Process>>> = SpinLock::new(BTreeMap::new());

impl Process {
    fn new(name: &str) -> Option<Arc<Self>> {
        let mut buf = [0u8; 32];
        let len = name.len().min(buf.len());
        buf[..len].copy_from_slice(&name.as_bytes()[..len]);
        let p = Arc::new(Self {
            id: NEXT_ID.fetch_add(4, Ordering::Relaxed),
            name: buf,
            name_len: len,
            space: AddressSpace::new()?,
            handles: SpinLock::new(HandleTable::new()),
            exit_status: AtomicI64::new(status::SUCCESS),
            exited: AtomicBool::new(false),
            terminated: Event::new(true, false),
            image_base: AtomicU64::new(0),
        });
        PROCESSES.lock().insert(p.id, Arc::downgrade(&p));
        Some(p)
    }

    pub fn name(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len]).unwrap_or("?")
    }

    /// The exit status, once the process has terminated.
    pub fn exit_status(&self) -> Option<i64> {
        self.exited.load(Ordering::Acquire).then(|| self.exit_status.load(Ordering::Relaxed))
    }

    pub fn image_base(&self) -> u64 {
        self.image_base.load(Ordering::Relaxed)
    }

    /// Records the exit status (the first one wins), closes every handle and
    /// signals `terminated`. The address space is freed when the last
    /// thread is gone and the last reference is dropped.
    pub fn mark_exited(&self, code: i64) {
        if self.exited.swap(true, Ordering::AcqRel) {
            return;
        }
        self.exit_status.store(code, Ordering::Relaxed);
        let objects = self.handles.lock().drain();
        drop(objects);
        // Give the screen and keyboard back to the kernel console if this
        // process had taken them.
        crate::drivers::display::release(self.id);
        crate::drivers::input::release(self.id);
        self.terminated.set();
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        PROCESSES.lock().remove(&self.id);
    }
}

/// Creates a process from a PE32+ image and starts its first thread at the
/// image entry point with `arg` as its first argument (RCX).
pub fn spawn(name: &str, image: &[u8], arg: u64) -> Result<Arc<Process>, &'static str> {
    let process = Process::new(name).ok_or("out of memory for a new address space")?;
    let loaded = pe::load(&process.space, image)?;
    match loaded.subsystem {
        pe::subsystem::NATIVE => {}
        pe::subsystem::WINDOWS_GUI | pe::subsystem::WINDOWS_CUI => {
            return Err("this is a Windows program; the Win32 subsystem is not implemented yet");
        }
        _ => return Err("the program's subsystem is not supported"),
    }
    process.image_base.store(loaded.base, Ordering::Relaxed);
    crate::info!(
        "ps: process {} ({}): {} KiB image at {:#x}{}",
        process.id,
        name,
        loaded.size / 1024,
        loaded.base,
        if loaded.relocated { " (relocated)" } else { "" }
    );
    let stack = process
        .space
        .allocate(loaded.stack_size, Protection::ReadWrite)
        .map_err(|_| "out of memory for the user stack")?;
    let stack_top = stack + loaded.stack_size;
    let entry = loaded.entry;
    sched::spawn_in(Some(process.clone()), name, priority::NORMAL, move || {
        // SAFETY: entry and stack were just mapped into this thread's
        // process, whose address space is active.
        unsafe { crate::arch::x86_64::syscall::enter_user(entry, stack_top, arg) }
    })?;
    Ok(process)
}

/// Snapshot of live processes.
pub fn list() -> Vec<Arc<Process>> {
    PROCESSES.lock().values().filter_map(Weak::upgrade).collect()
}

/// Handles a CPU exception raised by user-mode code: the faulting process
/// is terminated with the matching NT status and the kernel carries on.
pub fn user_exception(frame: &TrapFrame) -> ! {
    let code = match frame.vector {
        0 => status::INTEGER_DIVIDE_BY_ZERO,
        3 => status::BREAKPOINT,
        6 => status::ILLEGAL_INSTRUCTION,
        13 => status::PRIVILEGED_INSTRUCTION,
        14 => status::ACCESS_VIOLATION,
        _ => status::UNSUCCESSFUL,
    };
    let thread = sched::current();
    let detail = if frame.vector == 14 {
        alloc::format!(" accessing {:#x}", crate::arch::x86_64::cpu::read_cr2())
    } else {
        alloc::string::String::new()
    };
    if let Some(p) = thread.process() {
        crate::warn!(
            "process {} ({}) stopped: {} at {:#x}{} (status {:#010x})",
            p.id,
            p.name(),
            idt::exception_name(frame.vector),
            frame.rip,
            detail,
            code as u32
        );
        p.mark_exited(code);
    }
    drop(thread);
    sched::exit_current()
}
