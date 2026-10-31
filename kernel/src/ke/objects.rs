//! Dispatcher objects: events, semaphores and mutexes that threads can wait
//! on. All object state is protected by the dispatcher lock.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use core::cell::UnsafeCell;

use super::sched::{self, State, WaitStatus};
use super::thread::Thread;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Stays signalled until reset; releases every waiter.
    NotificationEvent,
    /// Releases one waiter, then resets automatically.
    SynchronizationEvent,
    Semaphore,
    /// Owned and recursive.
    Mutex,
}

pub(super) struct ObjectState {
    kind: Kind,
    signal: i64,
    limit: i64,
    owner: u64,
    recursion: u32,
    pub(super) waiters: VecDeque<Arc<Thread>>,
}

pub struct WaitObject {
    state: UnsafeCell<ObjectState>,
}

// SAFETY: the state is only accessed with the dispatcher lock held.
unsafe impl Send for WaitObject {}
unsafe impl Sync for WaitObject {}

impl WaitObject {
    const fn new(kind: Kind, signal: i64, limit: i64) -> Self {
        Self {
            state: UnsafeCell::new(ObjectState {
                kind,
                signal,
                limit,
                owner: 0,
                recursion: 0,
                waiters: VecDeque::new(),
            }),
        }
    }

    /// # Safety
    /// Dispatcher lock held.
    #[allow(clippy::mut_from_ref)]
    pub(super) unsafe fn state(&self) -> &mut ObjectState {
        unsafe { &mut *self.state.get() }
    }

    /// Consumes the object's signal on behalf of `t` if it is available.
    ///
    /// # Safety
    /// Dispatcher lock held.
    pub(super) unsafe fn try_acquire(&self, t: &Thread) -> bool {
        let s = unsafe { self.state() };
        match s.kind {
            Kind::NotificationEvent => s.signal > 0,
            Kind::SynchronizationEvent | Kind::Semaphore => {
                if s.signal > 0 {
                    s.signal -= 1;
                    true
                } else {
                    false
                }
            }
            Kind::Mutex => {
                if s.owner == 0 {
                    s.owner = t.id;
                    s.recursion = 1;
                    true
                } else if s.owner == t.id {
                    s.recursion += 1;
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Wakes waiters, oldest first, for as long as the signal satisfies them.
    ///
    /// # Safety
    /// Dispatcher lock held.
    pub(super) unsafe fn wake_waiters(&self, st: &mut State) {
        loop {
            let Some(t) = (unsafe { self.state() }).waiters.front().cloned() else { break };
            if !unsafe { self.try_acquire(&t) } {
                break;
            }
            unsafe { self.state() }.waiters.pop_front();
            sched::wake_locked(st, &t, WaitStatus::Signaled);
        }
    }
}

/// An event (Win32 `CreateEvent` semantics).
pub struct Event(WaitObject);

impl Event {
    /// `manual_reset`: stays set and releases all waiters until reset;
    /// otherwise releases exactly one waiter and resets itself.
    pub const fn new(manual_reset: bool, initially_set: bool) -> Self {
        let kind = if manual_reset { Kind::NotificationEvent } else { Kind::SynchronizationEvent };
        Self(WaitObject::new(kind, initially_set as i64, 1))
    }

    pub fn set(&self) {
        sched::with_lock(|st| unsafe { self.set_locked(st) });
    }

    /// # Safety
    /// Dispatcher lock held.
    pub(super) unsafe fn set_locked(&self, st: &mut State) {
        unsafe {
            self.0.state().signal = 1;
            self.0.wake_waiters(st);
        }
    }

    pub fn reset(&self) {
        sched::with_lock(|_| unsafe { self.0.state().signal = 0 });
    }

    pub fn is_set(&self) -> bool {
        sched::with_lock(|_| unsafe { self.0.state().signal > 0 })
    }

    /// Waits until the event is set or `timeout_ms` elapses (`None` = forever).
    pub fn wait(&self, timeout_ms: Option<u64>) -> WaitStatus {
        sched::wait(Some(&self.0), timeout_ms)
    }
}

/// A counting semaphore (Win32 `CreateSemaphore` semantics).
pub struct Semaphore(WaitObject);

impl Semaphore {
    pub const fn new(initial: i64, limit: i64) -> Self {
        Self(WaitObject::new(Kind::Semaphore, initial, limit))
    }

    /// Adds `count` to the semaphore. Fails without changing anything if the
    /// count would exceed the limit. Returns the previous count.
    pub fn release(&self, count: i64) -> Result<i64, ()> {
        sched::with_lock(|st| unsafe {
            let s = self.0.state();
            if count <= 0 || s.signal + count > s.limit {
                return Err(());
            }
            let previous = s.signal;
            s.signal += count;
            self.0.wake_waiters(st);
            Ok(previous)
        })
    }

    pub fn wait(&self, timeout_ms: Option<u64>) -> WaitStatus {
        sched::wait(Some(&self.0), timeout_ms)
    }
}

/// A recursive, owned mutex that sleeps instead of spinning (Win32
/// `CreateMutex` semantics, without abandonment yet).
pub struct Mutex(WaitObject);

impl Mutex {
    pub const fn new() -> Self {
        Self(WaitObject::new(Kind::Mutex, 0, 1))
    }

    pub fn acquire(&self, timeout_ms: Option<u64>) -> WaitStatus {
        sched::wait(Some(&self.0), timeout_ms)
    }

    /// Releases one level of ownership. Releasing a mutex the current
    /// thread does not own is a kernel bug.
    pub fn release(&self) {
        let me = sched::current_id();
        sched::with_lock(|st| unsafe {
            let s = self.0.state();
            assert!(s.owner == me, "mutex released by thread {me}, owned by {}", s.owner);
            s.recursion -= 1;
            if s.recursion == 0 {
                s.owner = 0;
                self.0.wake_waiters(st);
            }
        });
    }
}
