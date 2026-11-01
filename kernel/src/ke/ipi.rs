//! Inter-processor interrupts: rescheduling requests, cross-processor
//! function calls and TLB shootdown.
//!
//! Every request sets a bit in the target processor's `ipi_requests` word
//! and sends the single IPI vector; the handler services all pending bits.

use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use super::pcr::{self, MAX_CPUS, Pcr};
use crate::arch::x86_64::idt::{self, TrapFrame};
use crate::arch::x86_64::{apic, cpu, vectors};
use crate::ke::dpc;
use crate::sync::SpinLock;

pub const REQUEST_RESCHEDULE: u32 = 1 << 0;
pub const REQUEST_CALL: u32 = 1 << 1;
pub const REQUEST_TLB_FLUSH: u32 = 1 << 2;

/// Invalidate the whole TLB (non-global entries) instead of one page.
const FLUSH_ALL: u64 = u64::MAX;

static SHOOTDOWN_LOCK: SpinLock<()> = SpinLock::new(());
static SHOOTDOWN_VA: AtomicU64 = AtomicU64::new(0);
/// Processors (by index bit) that have not yet performed the current flush.
static SHOOTDOWN_PENDING: AtomicU64 = AtomicU64::new(0);

static CALL_LOCK: SpinLock<()> = SpinLock::new(());
static CALL_FN: AtomicUsize = AtomicUsize::new(0);
static CALL_ARG: AtomicU64 = AtomicU64::new(0);
static CALL_RESULT: AtomicU64 = AtomicU64::new(0);
static CALL_DONE: AtomicBool = AtomicBool::new(false);

pub fn init() {
    idt::register(vectors::IPI, ipi_interrupt);
}

/// Sets `request` on `target` and interrupts it.
pub fn send(target: &Pcr, request: u32) {
    target.ipi_requests.fetch_or(request, Ordering::AcqRel);
    apic::send_ipi(target.apic_id.load(Ordering::Relaxed), vectors::IPI);
}

fn ipi_interrupt(_: &mut TrapFrame) {
    let pcr = pcr::current();
    // Claim every request except TLB flushes, which only
    // `service_tlb_request` may claim (it also completes them).
    let requests = pcr.ipi_requests.fetch_and(REQUEST_TLB_FLUSH, Ordering::AcqRel);
    service_tlb_request(pcr);
    if requests & REQUEST_CALL != 0 {
        let f = CALL_FN.load(Ordering::Acquire);
        if f != 0 {
            // SAFETY: only `fn(u64) -> u64` values are stored in CALL_FN.
            let f: fn(u64) -> u64 = unsafe { core::mem::transmute(f) };
            CALL_RESULT.store(f(CALL_ARG.load(Ordering::Acquire)), Ordering::Release);
            CALL_DONE.store(true, Ordering::Release);
        }
    }
    if requests & REQUEST_RESCHEDULE != 0 {
        pcr.need_resched.store(true, Ordering::Release);
        dpc::request_dispatch_interrupt();
    }
}

/// Performs this processor's pending TLB flush request, if any. Besides the
/// IPI handler, every loop that spins waiting for other processors calls
/// this, so two processors that both wait with interrupts disabled (for
/// example while freeing memory during a context switch) cannot deadlock.
fn service_tlb_request(pcr: &Pcr) {
    if pcr.ipi_requests.fetch_and(!REQUEST_TLB_FLUSH, Ordering::AcqRel) & REQUEST_TLB_FLUSH != 0 {
        flush_local(SHOOTDOWN_VA.load(Ordering::Acquire));
        SHOOTDOWN_PENDING.fetch_and(!(1 << pcr.index), Ordering::AcqRel);
    }
}

fn flush_local(va: u64) {
    if va == FLUSH_ALL {
        // SAFETY: reloading CR3 flushes all non-global translations.
        unsafe { cpu::write_cr3(cpu::read_cr3()) };
    } else {
        // SAFETY: invalidating a TLB entry is always safe.
        unsafe { cpu::invlpg(va) };
    }
}

/// Every online processor other than the caller.
fn others(me: &Pcr) -> impl Iterator<Item = &'static Pcr> + '_ {
    (0..MAX_CPUS).filter_map(pcr::by_index).filter(move |p| p.index != me.index)
}

/// Invalidates `va` in every processor's TLB. Must not be called with the
/// dispatcher lock held or at an IRQL that blocks IPIs on other processors
/// waiting for a lock the caller holds.
pub fn shootdown(va: u64) {
    flush_local(va);
    if pcr::online_count() < 2 {
        return;
    }
    let me = pcr::current();
    let _guard = loop {
        if let Some(guard) = SHOOTDOWN_LOCK.try_lock_at(crate::ke::irql::DISPATCH_LEVEL) {
            break guard;
        }
        service_tlb_request(me);
        core::hint::spin_loop();
    };
    let targets = others(me).fold(0u64, |mask, p| mask | 1 << p.index);
    SHOOTDOWN_VA.store(va, Ordering::Release);
    SHOOTDOWN_PENDING.store(targets, Ordering::Release);
    for p in others(me) {
        send(p, REQUEST_TLB_FLUSH);
    }
    wait_for(|| SHOOTDOWN_PENDING.load(Ordering::Acquire) == 0, "TLB shootdown");
}

/// Runs `f(arg)` on the processor with logical index `cpu` (in its IPI
/// handler, at IPI_LEVEL) and returns the result. Must be called below
/// IPI_LEVEL; `f` must be short and must not block.
pub fn call_on(cpu: u32, f: fn(u64) -> u64, arg: u64) -> Option<u64> {
    let target = pcr::by_index(cpu as usize)?;
    if target.index == pcr::current().index {
        return Some(f(arg));
    }
    let _guard = CALL_LOCK.lock();
    CALL_DONE.store(false, Ordering::Release);
    CALL_ARG.store(arg, Ordering::Release);
    CALL_FN.store(f as usize, Ordering::Release);
    send(target, REQUEST_CALL);
    wait_for(|| CALL_DONE.load(Ordering::Acquire), "cross-processor call");
    CALL_FN.store(0, Ordering::Release);
    Some(CALL_RESULT.load(Ordering::Acquire))
}

/// Spins until `done` holds; stops the system if another processor fails
/// to respond within a second (a deadlock we must not hide).
fn wait_for(done: impl Fn() -> bool, what: &str) {
    let start = cpu::rdtsc();
    let limit = crate::ke::clock::tsc_khz().max(1_000_000) * 1000;
    let me = pcr::current();
    while !done() {
        service_tlb_request(me);
        core::hint::spin_loop();
        if cpu::rdtsc() - start > limit {
            let pending = SHOOTDOWN_PENDING.load(Ordering::Acquire);
            let mut stuck = 0;
            let mut thread = "?";
            for p in others(me) {
                if pending & (1 << p.index) != 0 || what != "TLB shootdown" {
                    stuck = p.index;
                    let t = p.current_thread.load(Ordering::Relaxed);
                    // SAFETY: diagnostic read; the system is about to stop.
                    thread = if t.is_null() { "none" } else { unsafe { (*t).name() } };
                    break;
                }
            }
            panic!(
                "{what}: processor {stuck} (running thread '{thread}', requests {:#x}) did not respond within 1 second",
                pcr::by_index(stuck as usize).map_or(0, |p| p.ipi_requests.load(Ordering::Relaxed))
            );
        }
    }
}
