//! The dispatcher: ready queues, context switches, waits, timeouts and
//! preemption.
//!
//! Design (see docs/KERNEL_DESIGN.md):
//! * 32 priority levels; one FIFO ready queue per level plus a summary
//!   bitmap, so picking the next thread is O(1).
//! * One dispatcher lock protects all scheduling and dispatcher-object state.
//!   It is only ever taken at DISPATCH_LEVEL, never from interrupt handlers.
//! * Preemption is requested by setting `need_resched` and raising the
//!   DISPATCH software interrupt; it runs as soon as the processor's IRQL
//!   drops below DISPATCH_LEVEL, so code holding a spin lock is never
//!   preempted.
//! * A context switch happens with the dispatcher lock held; the thread that
//!   is switched *to* releases it (`finish_switch`).

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use super::clock;
use super::dpc;
use super::ipi;
use super::irql::{self, DISPATCH_LEVEL, Irql};
use super::objects::WaitObject;
use super::pcr::{self, MAX_CPUS, Pcr};
use super::thread::{Thread, ThreadState, WAIT_PENDING, WAIT_SIGNALED, WAIT_TIMEOUT, priority};
use crate::arch::x86_64::idt::{self, TrapFrame};
use crate::arch::x86_64::{context, cpu, vectors};
use crate::mm::kva;
use crate::sync::RacyCell;

/// Clock ticks (ms) a thread may run before yielding to an equal-priority one.
pub const QUANTUM_TICKS: u32 = 10;
/// Kernel stack size for new threads (pages); a guard page sits below.
const STACK_PAGES: u64 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitStatus {
    Signaled,
    Timeout,
}

pub(super) struct State {
    ready: [VecDeque<Arc<Thread>>; 32],
    summary: u32,
    /// Waits with a timeout, keyed by (deadline tick, thread id).
    timers: BTreeMap<(u64, u64), Arc<Thread>>,
    threads: BTreeMap<u64, Weak<Thread>>,
    next_id: u64,
}

static LOCK: AtomicBool = AtomicBool::new(false);
static STATE: RacyCell<State> = RacyCell::new(State {
    ready: [const { VecDeque::new() }; 32],
    summary: 0,
    timers: BTreeMap::new(),
    threads: BTreeMap::new(),
    next_id: 1,
});
static NEXT_DEADLINE: AtomicU64 = AtomicU64::new(u64::MAX);
/// Lock-free copy of `State::summary` so idle processors can notice work.
static READY_SUMMARY: AtomicU32 = AtomicU32::new(0);
static STARTED: AtomicBool = AtomicBool::new(false);
static CONTEXT_SWITCHES: AtomicU64 = AtomicU64::new(0);

/// Terminated threads whose last reference was dropped during a context
/// switch. Freeing a kernel stack unmaps memory (and so shoots down TLBs on
/// other processors), which must not happen in the switch path where
/// interrupts may be disabled; the reaper thread frees them instead.
static REAP_LIST: crate::sync::SpinLock<Vec<Arc<Thread>>> = crate::sync::SpinLock::new(Vec::new());
static REAPER_WAKE: super::objects::Event = super::objects::Event::new(false, false);

fn acquire() -> Irql {
    let old = irql::raise(DISPATCH_LEVEL);
    while LOCK.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
        while LOCK.load(Ordering::Relaxed) {
            core::hint::spin_loop();
        }
    }
    old
}

fn release() {
    LOCK.store(false, Ordering::Release);
}

/// # Safety
/// The dispatcher lock must be held.
unsafe fn state() -> &'static mut State {
    unsafe { &mut *STATE.get() }
}

/// Runs `f` with the dispatcher lock held. `f` must not switch threads.
pub(super) fn with_lock<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let old = acquire();
    // SAFETY: lock held.
    let r = f(unsafe { state() });
    release();
    irql::lower(old);
    r
}

fn is_idle(pcr: &Pcr, t: *const Thread) -> bool {
    ptr::eq(pcr.idle_thread.load(Ordering::Relaxed), t)
}

pub fn current_ptr() -> *const Thread {
    pcr::current().current_thread.load(Ordering::Relaxed)
}

/// The thread running on this processor.
pub fn current() -> Arc<Thread> {
    let p = current_ptr();
    assert!(!p.is_null(), "no current thread");
    // SAFETY: the current-thread slot owns one strong reference.
    unsafe {
        Arc::increment_strong_count(p);
        Arc::from_raw(p)
    }
}

pub fn current_id() -> u64 {
    // SAFETY: the current thread stays alive while it runs.
    unsafe { (*current_ptr()).id }
}

/// Turns the code running on this processor into its idle thread.
pub fn init_processor(pcr: &Pcr, name: &str) {
    let id = with_lock(|st| {
        st.next_id += 1;
        st.next_id - 1
    });
    let idle = Arc::new(Thread::new(id, name, priority::IDLE, None, None, None));
    idle.set_state(ThreadState::Running);
    idle.last_cpu.store(pcr.index, Ordering::Relaxed);
    with_lock(|st| st.threads.insert(id, Arc::downgrade(&idle)));
    pcr.idle_thread.store(Arc::into_raw(idle.clone()) as *mut Thread, Ordering::Release);
    pcr.current_thread.store(Arc::into_raw(idle) as *mut Thread, Ordering::Release);
}

/// Starts the dispatcher on the bootstrap processor.
pub fn init() {
    idt::register(vectors::DISPATCH, dispatch_interrupt);
    init_processor(pcr::current(), "idle-0");
    STARTED.store(true, Ordering::Release);
    spawn("reaper", priority::HIGH, || {
        loop {
            REAPER_WAKE.wait(None);
            let dead = core::mem::take(&mut *REAP_LIST.lock());
            drop(dead); // frees stacks with interrupts enabled at PASSIVE_LEVEL
        }
    })
    .expect("cannot start the reaper thread");
}

/// Creates a kernel thread and makes it ready to run.
pub fn spawn(name: &str, prio: u8, entry: impl FnOnce() + Send + 'static) -> Result<Arc<Thread>, &'static str> {
    spawn_in(None, name, prio, entry)
}

/// Creates a thread belonging to `process` (or a kernel thread for `None`).
/// The entry closure runs in kernel mode with the process's address space
/// active; user threads use it to drop to ring 3.
pub fn spawn_in(
    process: Option<Arc<crate::ps::process::Process>>,
    name: &str,
    prio: u8,
    entry: impl FnOnce() + Send + 'static,
) -> Result<Arc<Thread>, &'static str> {
    let stack = kva::alloc_stack(STACK_PAGES).ok_or("out of memory for a kernel stack")?;
    let top = stack.top();
    let id = with_lock(|st| {
        st.next_id += 1;
        st.next_id - 1
    });
    let thread =
        Arc::new(Thread::new(id, name, prio.clamp(1, priority::MAX), Some(stack), Some(Box::new(entry)), process));
    // SAFETY: the stack is fresh and the thread has not been published yet.
    unsafe { *thread.context.get() = context::initial_frame(top, Arc::as_ptr(&thread) as u64) };
    with_lock(|st| {
        st.threads.insert(id, Arc::downgrade(&thread));
        make_ready(st, thread.clone());
    });
    Ok(thread)
}

/// Puts `t` on its ready queue and asks the processor best placed to run it
/// (an idle one, else the one running the lowest-priority thread) to
/// reschedule, if `t` outranks what that processor is running.
pub(super) fn make_ready(st: &mut State, t: Arc<Thread>) {
    let p = t.priority() as usize;
    t.set_state(ThreadState::Ready);
    st.ready[p].push_back(t);
    set_summary(st, st.summary | 1 << p);

    let me = pcr::current();
    let mut target: Option<(&Pcr, i32)> = None;
    for c in (0..MAX_CPUS).filter_map(pcr::by_index) {
        let cur = c.current_thread.load(Ordering::Relaxed);
        if cur.is_null() {
            continue; // still starting up
        }
        // SAFETY: a processor's current thread cannot change while we hold
        // the dispatcher lock, and the current slot keeps it alive.
        let prio = if is_idle(c, cur) { -1 } else { unsafe { (*cur).priority() as i32 } };
        if prio >= p as i32 || (c.index != me.index && c.need_resched.load(Ordering::Relaxed)) {
            continue;
        }
        let better = match target {
            None => true,
            Some((_, best)) => prio < best || (prio == best && c.index == me.index),
        };
        if better {
            target = Some((c, prio));
        }
    }
    if let Some((c, _)) = target {
        c.need_resched.store(true, Ordering::Release);
        if c.index == me.index {
            dpc::request_dispatch_interrupt();
        } else {
            ipi::send(c, ipi::REQUEST_RESCHEDULE);
        }
    }
}

fn set_summary(st: &mut State, summary: u32) {
    st.summary = summary;
    READY_SUMMARY.store(summary, Ordering::Relaxed);
}

fn highest_ready(st: &State) -> Option<usize> {
    (st.summary != 0).then(|| 31 - st.summary.leading_zeros() as usize)
}

fn pop_highest(st: &mut State) -> Option<Arc<Thread>> {
    let p = highest_ready(st)?;
    let t = st.ready[p].pop_front();
    if st.ready[p].is_empty() {
        set_summary(st, st.summary & !(1 << p));
    }
    t
}

/// Puts the running thread back at the tail of its ready queue.
///
/// # Safety
/// Dispatcher lock held.
unsafe fn requeue_current(st: &mut State) {
    let pcr = pcr::current();
    if is_idle(pcr, current_ptr()) {
        return;
    }
    let t = current();
    let p = t.priority() as usize;
    t.set_state(ThreadState::Ready);
    st.ready[p].push_back(t);
    set_summary(st, st.summary | 1 << p);
}

/// Switches to the best ready thread (or this processor's idle thread).
/// Called with the dispatcher lock held at DISPATCH_LEVEL; returns with the
/// lock released once the calling thread runs again.
///
/// # Safety
/// Dispatcher lock held; the current thread's state must already reflect
/// why it is giving up the processor.
unsafe fn switch_locked(st: &mut State) {
    let pcr = pcr::current();
    let cur = pcr.current_thread.load(Ordering::Relaxed);
    let next = pop_highest(st).unwrap_or_else(|| {
        let idle = pcr.idle_thread.load(Ordering::Relaxed);
        // SAFETY: the PCR keeps a permanent reference to its idle thread.
        unsafe {
            Arc::increment_strong_count(idle);
            Arc::from_raw(idle)
        }
    });
    if ptr::eq(Arc::as_ptr(&next), cur) {
        next.set_state(ThreadState::Running);
        drop(next);
        release();
        return;
    }
    if is_idle(pcr, cur) {
        // SAFETY: the idle thread is permanently referenced by the PCR.
        unsafe { (*cur).set_state(ThreadState::Ready) };
    }
    next.set_state(ThreadState::Running);
    next.quantum.store(QUANTUM_TICKS, Ordering::Relaxed);
    next.last_cpu.store(pcr.index, Ordering::Relaxed);
    if let Some(top) = next.stack_top() {
        // Interrupts from user mode and system calls enter on this stack.
        // SAFETY: only this processor touches its TSS.
        unsafe { (*pcr.tss.get()).rsp = [top, 0, 0] };
        pcr.kernel_rsp.store(top, Ordering::Relaxed);
    }
    let root = next.page_table_root();
    if cpu::read_cr3() & !0xFFF != root {
        // SAFETY: every address space maps the shared kernel half, so this
        // code and both stacks stay mapped across the switch.
        unsafe { cpu::write_cr3(root) };
    }
    // SAFETY: `next` is valid; its saved context came from a switch or
    // `initial_frame`.
    let load = unsafe { *next.context.get() };
    let next_raw = Arc::into_raw(next);
    pcr.previous_thread.store(cur, Ordering::Relaxed);
    pcr.current_thread.store(next_raw as *mut Thread, Ordering::Relaxed);
    CONTEXT_SWITCHES.fetch_add(1, Ordering::Relaxed);
    // SAFETY: `cur` stays alive until `finish_switch` on the other side drops
    // the reference handed over through `previous_thread`.
    unsafe { context::switch((*cur).context.get(), load) };
    finish_switch();
}

/// Runs on the incoming thread right after a switch: releases the dispatcher
/// lock and drops the outgoing thread's current-slot reference (which frees
/// a terminated thread's stack, now that nobody runs on it).
fn finish_switch() {
    let pcr = pcr::current();
    let prev = pcr.previous_thread.swap(ptr::null_mut(), Ordering::Relaxed);
    release();
    if !prev.is_null() {
        // SAFETY: this is the reference the current-thread slot held.
        let prev = unsafe { Arc::from_raw(prev) };
        if Arc::strong_count(&prev) == 1 {
            REAP_LIST.lock().push(prev);
            REAPER_WAKE.set();
        }
    }
}

/// First code a new thread runs (reached from `context::initial_frame`).
#[unsafe(no_mangle)]
extern "C" fn meridian_thread_startup(_thread: *const Thread) -> ! {
    finish_switch();
    irql::lower(irql::PASSIVE_LEVEL);
    // SAFETY: the thread is fully set up.
    unsafe { cpu::enable_interrupts() };
    // SAFETY: only the thread itself takes its entry closure.
    let entry = unsafe { (*(*current_ptr()).entry.get()).take() };
    if let Some(entry) = entry {
        entry();
    }
    exit_current()
}

/// Terminates the calling thread.
pub fn exit_current() -> ! {
    let _ = acquire();
    // SAFETY: lock held.
    let st = unsafe { state() };
    let me = current();
    me.set_state(ThreadState::Terminated);
    // SAFETY: lock held.
    unsafe { me.terminated.set_locked(st) };
    drop(me);
    // SAFETY: lock held; a terminated thread is never queued again.
    unsafe { switch_locked(st) };
    unreachable!("a terminated thread was scheduled");
}

/// Gives up the rest of the time slice to other ready threads of the same
/// or higher priority.
pub fn yield_now() {
    let old = acquire();
    // SAFETY: lock held.
    unsafe {
        let st = state();
        requeue_current(st);
        switch_locked(st);
    }
    irql::lower(old);
}

/// Blocks the calling thread for at least `ms` milliseconds.
pub fn sleep_ms(ms: u64) {
    wait(None, Some(ms));
}

/// Waits for `object` to be signalled, or for the timeout (milliseconds) to
/// elapse. With no object this is a sleep.
pub(super) fn wait(object: Option<&WaitObject>, timeout_ms: Option<u64>) -> WaitStatus {
    assert!(object.is_some() || timeout_ms.is_some(), "wait with neither object nor timeout");
    assert!(irql::current() < DISPATCH_LEVEL, "cannot wait at or above DISPATCH_LEVEL");
    let old = acquire();
    // SAFETY: lock held for everything below until switch_locked releases it.
    unsafe {
        let st = state();
        let me = current();
        if let Some(o) = object {
            if o.try_acquire(&me) {
                release();
                irql::lower(old);
                return WaitStatus::Signaled;
            }
        }
        if timeout_ms == Some(0) {
            release();
            irql::lower(old);
            return WaitStatus::Timeout;
        }
        let w = me.wait_state();
        w.object = object.map_or(ptr::null(), |o| o as *const WaitObject);
        w.status = WAIT_PENDING;
        w.timer = None;
        if let Some(o) = object {
            o.state().waiters.push_back(me.clone());
        }
        if let Some(ms) = timeout_ms {
            // +1: the current tick is already partly over.
            let key = (clock::ticks() + ms + 1, me.id);
            st.timers.insert(key, me.clone());
            w.timer = Some(key);
            NEXT_DEADLINE.fetch_min(key.0, Ordering::Relaxed);
        }
        me.set_state(ThreadState::Waiting);
        switch_locked(st);
        irql::lower(old);
        if me.wait_state().status == WAIT_TIMEOUT { WaitStatus::Timeout } else { WaitStatus::Signaled }
    }
}

/// Ends `t`'s wait with `status` and readies it.
pub(super) fn wake_locked(st: &mut State, t: &Arc<Thread>, status: WaitStatus) {
    // SAFETY: lock held.
    let w = unsafe { t.wait_state() };
    if w.status != WAIT_PENDING || t.state() != ThreadState::Waiting {
        return;
    }
    w.status = if status == WaitStatus::Timeout { WAIT_TIMEOUT } else { WAIT_SIGNALED };
    if let Some(key) = w.timer.take() {
        st.timers.remove(&key);
    }
    if status == WaitStatus::Timeout && !w.object.is_null() {
        // SAFETY: the object outlives every wait on it.
        unsafe { (*w.object).state().waiters.retain(|x| !Arc::ptr_eq(x, t)) };
    }
    w.object = ptr::null();
    make_ready(st, t.clone());
}

fn expire_timers(st: &mut State) {
    let now = clock::ticks();
    while let Some((&key, _)) = st.timers.first_key_value() {
        if key.0 > now {
            break;
        }
        let (_, t) = st.timers.pop_first().unwrap();
        // SAFETY: lock held.
        unsafe { t.wait_state().timer = None };
        wake_locked(st, &t, WaitStatus::Timeout);
    }
    NEXT_DEADLINE.store(st.timers.first_key_value().map_or(u64::MAX, |(k, _)| k.0), Ordering::Relaxed);
}

/// DISPATCH_LEVEL software interrupt: run DPCs, expire timeouts, preempt.
fn dispatch_interrupt(_: &mut TrapFrame) {
    dpc::drain();
    let _ = acquire();
    // SAFETY: lock held.
    let st = unsafe { state() };
    expire_timers(st);
    let pcr = pcr::current();
    let quantum_end = pcr.need_resched.swap(false, Ordering::AcqRel);
    let cur = pcr.current_thread.load(Ordering::Relaxed);
    // SAFETY: the current thread is alive while it runs.
    let cur_prio = if is_idle(pcr, cur) { -1 } else { unsafe { (*cur).priority() as i32 } };
    let best = highest_ready(st).map_or(-1, |p| p as i32);
    if best > cur_prio || (quantum_end && best >= 0 && best == cur_prio) {
        // SAFETY: lock held.
        unsafe {
            requeue_current(st);
            switch_locked(st);
        }
    } else {
        release();
    }
}

/// Called by every processor's clock interrupt (CLOCK_LEVEL).
pub fn clock_tick(pcr: &Pcr) {
    if !STARTED.load(Ordering::Acquire) {
        return;
    }
    let cur = pcr.current_thread.load(Ordering::Relaxed);
    let mut request = false;
    if cur.is_null() {
        return;
    }
    if is_idle(pcr, cur) {
        // Pick up work that was queued without kicking this processor.
        request = READY_SUMMARY.load(Ordering::Relaxed) != 0;
    } else {
        // SAFETY: the current thread is alive while it runs.
        let t = unsafe { &*cur };
        t.cpu_ticks.fetch_add(1, Ordering::Relaxed);
        let q = t.quantum.load(Ordering::Relaxed);
        if q <= 1 {
            t.quantum.store(QUANTUM_TICKS, Ordering::Relaxed);
            pcr.need_resched.store(true, Ordering::Release);
            request = true;
        } else {
            t.quantum.store(q - 1, Ordering::Relaxed);
        }
    }
    if clock::ticks() >= NEXT_DEADLINE.load(Ordering::Relaxed) {
        request = true;
    }
    if request {
        dpc::request_dispatch_interrupt();
    }
}

/// The idle loop: halt until an interrupt makes work available.
pub fn idle_loop() -> ! {
    loop {
        cpu::enable_and_halt();
    }
}

/// Snapshot of all live threads (for diagnostics).
pub fn threads() -> Vec<Arc<Thread>> {
    with_lock(|st| {
        st.threads.retain(|_, w| w.strong_count() > 0);
        st.threads.values().filter_map(Weak::upgrade).collect()
    })
}

pub fn context_switches() -> u64 {
    CONTEXT_SWITCHES.load(Ordering::Relaxed)
}
