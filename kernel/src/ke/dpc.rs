//! Deferred procedure calls.
//!
//! Interrupt handlers run at device IRQL and must not touch the dispatcher
//! (they could interrupt code that holds its lock). Instead they queue a DPC,
//! which runs on the same processor at DISPATCH_LEVEL as soon as the IRQL
//! drops below it, where waking threads is allowed.

use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch::x86_64::{apic, vectors};
use crate::ke::irql::HIGH_LEVEL;
use crate::ke::pcr;

const QUEUE_LEN: usize = 32;

pub struct Dpc {
    routine: fn(),
    queued: AtomicBool,
}

impl Dpc {
    pub const fn new(routine: fn()) -> Self {
        Self { routine, queued: AtomicBool::new(false) }
    }

    /// Queues this DPC on the current processor (a no-op if it is already
    /// queued). Safe at any IRQL.
    pub fn queue(&'static self) {
        if self.queued.swap(true, Ordering::AcqRel) {
            return;
        }
        let pcr = pcr::current();
        {
            let mut q = pcr.dpcs.lock_at(HIGH_LEVEL);
            if q.len == QUEUE_LEN {
                // Cannot happen with fewer than QUEUE_LEN DPC objects.
                drop(q);
                panic!("DPC queue overflow");
            }
            let tail = (q.head + q.len) % QUEUE_LEN;
            q.items[tail] = Some(self);
            q.len += 1;
        }
        request_dispatch_interrupt();
    }
}

/// Per-processor ring of queued DPCs.
pub struct DpcQueue {
    items: [Option<&'static Dpc>; QUEUE_LEN],
    head: usize,
    len: usize,
}

impl DpcQueue {
    pub const fn new() -> Self {
        Self { items: [None; QUEUE_LEN], head: 0, len: 0 }
    }
}

/// Asks the current processor to run its DISPATCH_LEVEL work (DPCs, timer
/// expiry, rescheduling) once its IRQL drops below DISPATCH_LEVEL.
pub fn request_dispatch_interrupt() {
    apic::self_ipi(vectors::DISPATCH);
}

/// Runs every queued DPC. Called from the dispatch interrupt.
pub fn drain() {
    let pcr = pcr::current();
    loop {
        let next = {
            let mut q = pcr.dpcs.lock_at(HIGH_LEVEL);
            if q.len == 0 {
                None
            } else {
                let head = q.head;
                let item = q.items[head].take();
                q.head = (head + 1) % QUEUE_LEN;
                q.len -= 1;
                item
            }
        };
        match next {
            Some(dpc) => {
                dpc.queued.store(false, Ordering::Release);
                (dpc.routine)();
            }
            None => break,
        }
    }
}
