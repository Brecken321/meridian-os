//! Low-level synchronisation primitives.
//!
//! [`SpinLock`] follows the NT model: acquiring it raises the processor's
//! IRQL (to `DISPATCH_LEVEL` by default) so the holder cannot be preempted on
//! its own CPU, and releasing it restores the previous IRQL.

use core::cell::UnsafeCell;
use core::marker::PhantomData;
use core::mem::MaybeUninit;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use crate::ke::irql::{self, DISPATCH_LEVEL, Irql};

/// An `UnsafeCell` that may live in a `static`. Callers are responsible for
/// synchronising access (typically: written once during early boot).
pub struct RacyCell<T>(UnsafeCell<T>);

// SAFETY: users of RacyCell uphold exclusive access by construction.
unsafe impl<T> Sync for RacyCell<T> {}

impl<T> RacyCell<T> {
    pub const fn new(value: T) -> Self {
        Self(UnsafeCell::new(value))
    }

    pub fn get(&self) -> *mut T {
        self.0.get()
    }
}

pub struct SpinLock<T: ?Sized> {
    locked: AtomicBool,
    data: UnsafeCell<T>,
}

// SAFETY: the lock serialises all access to `data`.
unsafe impl<T: ?Sized + Send> Sync for SpinLock<T> {}
unsafe impl<T: ?Sized + Send> Send for SpinLock<T> {}

impl<T> SpinLock<T> {
    pub const fn new(data: T) -> Self {
        Self { locked: AtomicBool::new(false), data: UnsafeCell::new(data) }
    }
}

impl<T: ?Sized> SpinLock<T> {
    /// Acquires the lock at `DISPATCH_LEVEL`.
    pub fn lock(&self) -> SpinLockGuard<'_, T> {
        self.lock_at(DISPATCH_LEVEL)
    }

    /// Acquires the lock after raising IRQL to at least `level`. Use
    /// `HIGH_LEVEL` for data that interrupt handlers also touch.
    pub fn lock_at(&self, level: Irql) -> SpinLockGuard<'_, T> {
        let old_irql = irql::raise(level);
        while self.locked.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            while self.locked.load(Ordering::Relaxed) {
                core::hint::spin_loop();
            }
        }
        SpinLockGuard { lock: self, old_irql, _not_send: PhantomData }
    }

    pub fn try_lock_at(&self, level: Irql) -> Option<SpinLockGuard<'_, T>> {
        let old_irql = irql::raise(level);
        if self.locked.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok() {
            Some(SpinLockGuard { lock: self, old_irql, _not_send: PhantomData })
        } else {
            irql::lower(old_irql);
            None
        }
    }

    /// Releases the lock regardless of owner. Only for the crash path, where
    /// the owner may never run again.
    ///
    /// # Safety
    /// No other code may be using the protected data.
    pub unsafe fn force_unlock(&self) {
        self.locked.store(false, Ordering::Release);
    }
}

pub struct SpinLockGuard<'a, T: ?Sized> {
    lock: &'a SpinLock<T>,
    old_irql: Irql,
    _not_send: PhantomData<*const ()>,
}

impl<T: ?Sized> Deref for SpinLockGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: we hold the lock.
        unsafe { &*self.lock.data.get() }
    }
}

impl<T: ?Sized> DerefMut for SpinLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: we hold the lock exclusively.
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T: ?Sized> Drop for SpinLockGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
        irql::lower(self.old_irql);
    }
}

const UNINIT: u8 = 0;
const RUNNING: u8 = 1;
const READY: u8 = 2;

/// A value initialised exactly once, readable from any CPU afterwards.
pub struct Once<T> {
    state: AtomicU8,
    value: UnsafeCell<MaybeUninit<T>>,
}

// SAFETY: the value is written once before READY is published.
unsafe impl<T: Send + Sync> Sync for Once<T> {}

impl<T> Once<T> {
    pub const fn new() -> Self {
        Self { state: AtomicU8::new(UNINIT), value: UnsafeCell::new(MaybeUninit::uninit()) }
    }

    pub fn get(&self) -> Option<&T> {
        if self.state.load(Ordering::Acquire) == READY {
            // SAFETY: READY means the value was fully written.
            Some(unsafe { (*self.value.get()).assume_init_ref() })
        } else {
            None
        }
    }

    /// Stores `value` if nothing was stored yet; returns the stored value.
    pub fn call_once(&self, init: impl FnOnce() -> T) -> &T {
        if self.state.compare_exchange(UNINIT, RUNNING, Ordering::Acquire, Ordering::Acquire).is_ok() {
            // SAFETY: we won the race, so nobody else touches the value.
            unsafe { (*self.value.get()).write(init()) };
            self.state.store(READY, Ordering::Release);
        }
        while self.state.load(Ordering::Acquire) != READY {
            core::hint::spin_loop();
        }
        // SAFETY: READY observed.
        unsafe { (*self.value.get()).assume_init_ref() }
    }
}
