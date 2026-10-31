//! Kernel thread objects.

use alloc::boxed::Box;
use core::cell::UnsafeCell;
use core::ptr;
use core::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering};

use super::objects::{Event, WaitObject};
use crate::mm::kva::{self, KernelStack};

/// Thread scheduling priorities: 1-15 are variable (normal) priorities,
/// 16-31 real-time. 0 is reserved for the per-processor idle threads.
pub mod priority {
    pub const IDLE: u8 = 0;
    pub const LOW: u8 = 4;
    pub const NORMAL: u8 = 8;
    pub const HIGH: u8 = 12;
    pub const REALTIME: u8 = 16;
    pub const MAX: u8 = 31;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ThreadState {
    Initialized = 0,
    Ready = 1,
    Running = 2,
    Waiting = 3,
    Terminated = 4,
}

impl ThreadState {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Ready,
            2 => Self::Running,
            3 => Self::Waiting,
            4 => Self::Terminated,
            _ => Self::Initialized,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Initialized => "init",
            Self::Ready => "ready",
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Terminated => "terminated",
        }
    }
}

pub(super) const WAIT_PENDING: u8 = 0;
pub(super) const WAIT_SIGNALED: u8 = 1;
pub(super) const WAIT_TIMEOUT: u8 = 2;

/// Wait bookkeeping, only touched with the dispatcher lock held.
pub(super) struct WaitState {
    pub object: *const WaitObject,
    pub status: u8,
    pub timer: Option<(u64, u64)>,
}

pub type ThreadEntry = Box<dyn FnOnce() + Send + 'static>;

pub struct Thread {
    pub id: u64,
    name: [u8; 24],
    name_len: usize,
    pub(super) priority: AtomicU8,
    state: AtomicU8,
    /// Saved stack pointer while switched out.
    pub(super) context: UnsafeCell<u64>,
    stack: Option<KernelStack>,
    pub(super) entry: UnsafeCell<Option<ThreadEntry>>,
    pub(super) wait: UnsafeCell<WaitState>,
    pub(super) quantum: AtomicU32,
    /// Clock ticks this thread was running when the clock interrupt fired.
    pub cpu_ticks: AtomicU64,
    pub(super) last_cpu: AtomicU32,
    /// Signalled when the thread terminates (used to join it).
    pub terminated: Event,
}

// SAFETY: interior-mutable fields are either atomics or only accessed with
// the dispatcher lock held / by the owning processor during a switch.
unsafe impl Send for Thread {}
unsafe impl Sync for Thread {}

impl Thread {
    pub(super) fn new(
        id: u64,
        name: &str,
        priority: u8,
        stack: Option<KernelStack>,
        entry: Option<ThreadEntry>,
    ) -> Self {
        let mut buf = [0u8; 24];
        let len = name.len().min(buf.len());
        buf[..len].copy_from_slice(&name.as_bytes()[..len]);
        Self {
            id,
            name: buf,
            name_len: len,
            priority: AtomicU8::new(priority.min(priority::MAX)),
            state: AtomicU8::new(ThreadState::Initialized as u8),
            context: UnsafeCell::new(0),
            stack,
            entry: UnsafeCell::new(entry),
            wait: UnsafeCell::new(WaitState { object: ptr::null(), status: WAIT_PENDING, timer: None }),
            quantum: AtomicU32::new(0),
            cpu_ticks: AtomicU64::new(0),
            last_cpu: AtomicU32::new(0),
            terminated: Event::new(true, false),
        }
    }

    pub fn name(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len]).unwrap_or("?")
    }

    pub fn priority(&self) -> u8 {
        self.priority.load(Ordering::Relaxed)
    }

    pub fn state(&self) -> ThreadState {
        ThreadState::from_u8(self.state.load(Ordering::Acquire))
    }

    pub(super) fn set_state(&self, s: ThreadState) {
        self.state.store(s as u8, Ordering::Release);
    }

    pub fn last_cpu(&self) -> u32 {
        self.last_cpu.load(Ordering::Relaxed)
    }

    pub(super) fn stack_top(&self) -> Option<u64> {
        self.stack.as_ref().map(|s| s.top())
    }

    /// # Safety
    /// Dispatcher lock held.
    #[allow(clippy::mut_from_ref)]
    pub(super) unsafe fn wait_state(&self) -> &mut WaitState {
        unsafe { &mut *self.wait.get() }
    }
}

impl Drop for Thread {
    fn drop(&mut self) {
        if let Some(stack) = self.stack.take() {
            kva::free_stack(stack);
        }
    }
}
