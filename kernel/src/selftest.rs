//! In-kernel self tests.
//!
//! Booting with `selftest` on the kernel command line runs every test below
//! on the real kernel (under QEMU in CI), prints one machine-readable line
//! per test and exits QEMU with a pass/fail code. The same tests can be run
//! interactively with the shell's `selftest` command.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::arch::x86_64::idt::{self, TrapFrame, fixup};
use crate::arch::x86_64::{apic, cpu};
use crate::hal::{acpi, hpet, pci};
use crate::ke::dpc::Dpc;
use crate::ke::objects::{Event, Mutex, Semaphore};
use crate::ke::sched::{self, WaitStatus};
use crate::ke::thread::{Thread, ThreadState, priority};
use crate::ke::{clock, irql};
use crate::mm::{PAGE_SIZE, kva, paging, phys_to_virt, pmm};
use crate::sync::SpinLock;

type TestResult = Result<(), String>;

struct Test {
    name: &'static str,
    run: fn() -> TestResult,
}

macro_rules! ensure {
    ($cond:expr, $($fmt:tt)*) => {
        if !$cond {
            return Err(format!($($fmt)*));
        }
    };
}

const TESTS: &[Test] = &[
    Test { name: "pmm.alloc_free", run: pmm_alloc_free },
    Test { name: "pmm.contiguous", run: pmm_contiguous },
    Test { name: "heap.collections", run: heap_collections },
    Test { name: "heap.large", run: heap_large },
    Test { name: "heap.alignment", run: heap_alignment },
    Test { name: "paging.map_unmap", run: paging_map_unmap },
    Test { name: "paging.guard_page_probe", run: guard_page_probe },
    Test { name: "paging.kernel_protection", run: kernel_protection },
    Test { name: "trap.breakpoint", run: breakpoint },
    Test { name: "trap.noncanonical_probe", run: noncanonical_probe },
    Test { name: "irql.raise_lower", run: irql_raise_lower },
    Test { name: "sync.spinlock_irql", run: spinlock_irql },
    Test { name: "apic.self_ipi", run: apic_self_ipi },
    Test { name: "clock.ticks", run: clock_ticks },
    Test { name: "clock.tsc_calibrated", run: tsc_calibrated },
    Test { name: "hpet.counter", run: hpet_counter },
    Test { name: "acpi.tables", run: acpi_tables },
    Test { name: "pci.enumeration", run: pci_enumeration },
    Test { name: "ps2.decode", run: ps2_decode },
    Test { name: "dpc.deferred_until_irql_drops", run: dpc_deferred },
    Test { name: "sched.spawn_join", run: sched_spawn_join },
    Test { name: "sched.many_threads", run: sched_many_threads },
    Test { name: "sched.time_slicing", run: sched_time_slicing },
    Test { name: "sched.priority_preemption", run: sched_priority_preemption },
    Test { name: "sched.sleep", run: sched_sleep },
    Test { name: "sched.stack_reclaimed", run: sched_stack_reclaimed },
    Test { name: "sync.event", run: sync_event },
    Test { name: "sync.event_timeout", run: sync_event_timeout },
    Test { name: "sync.notification_event", run: sync_notification_event },
    Test { name: "sync.mutex", run: sync_mutex },
    Test { name: "sync.semaphore", run: sync_semaphore },
    Test { name: "smp.processors_online", run: smp_processors_online },
    Test { name: "smp.cross_call", run: smp_cross_call },
    Test { name: "smp.tlb_shootdown", run: smp_tlb_shootdown },
    Test { name: "smp.parallel_threads", run: smp_parallel_threads },
];

/// Runs every test; returns (passed, failed).
pub fn run_all() -> (usize, usize) {
    let (mut passed, mut failed) = (0, 0);
    crate::kprintln!("[selftest] running {} tests", TESTS.len());
    for test in TESTS {
        match (test.run)() {
            Ok(()) => {
                passed += 1;
                crate::kprintln!("[selftest] \x1b[92mPASS\x1b[0m {}", test.name);
            }
            Err(why) => {
                failed += 1;
                crate::kprintln!("[selftest] \x1b[91mFAIL\x1b[0m {}: {}", test.name, why);
            }
        }
    }
    crate::kprintln!("[selftest] SUMMARY passed={} failed={}", passed, failed);
    if failed == 0 {
        crate::kprintln!("SELFTEST: ALL PASSED");
    } else {
        crate::kprintln!("SELFTEST: FAILURES");
    }
    (passed, failed)
}

fn pmm_alloc_free() -> TestResult {
    let (free_before, owned) = pmm::stats();
    ensure!(free_before <= owned, "free memory ({free_before}) exceeds owned memory ({owned})");
    let mut frames = Vec::new();
    for _ in 0..64 {
        let pa = pmm::alloc_frame().ok_or("out of frames")?;
        ensure!(pa % PAGE_SIZE == 0, "frame {pa:#x} is not page aligned");
        ensure!(pa >= 0x10_0000, "frame {pa:#x} is below 1 MiB");
        frames.push(pa);
    }
    let mut sorted = frames.clone();
    sorted.sort_unstable();
    sorted.dedup();
    ensure!(sorted.len() == frames.len(), "allocator returned a frame twice");
    for pa in frames {
        pmm::free_frame(pa);
    }
    let (free_after, _) = pmm::stats();
    ensure!(free_after == free_before, "leaked {} bytes", free_before as i64 - free_after as i64);
    Ok(())
}

fn pmm_contiguous() -> TestResult {
    let pa = pmm::alloc_contiguous(16, 16).ok_or("no 64 KiB contiguous block")?;
    ensure!(pa % (16 * PAGE_SIZE) == 0, "block {pa:#x} is not 64 KiB aligned");
    // Write the whole block through the direct map to prove it is real RAM.
    let p = phys_to_virt(pa) as *mut u64;
    for i in 0..(16 * PAGE_SIZE as usize / 8) {
        // SAFETY: inside the block we own.
        unsafe { p.add(i).write_volatile(i as u64) };
    }
    for i in 0..(16 * PAGE_SIZE as usize / 8) {
        ensure!(unsafe { p.add(i).read_volatile() } == i as u64, "readback mismatch at word {i}");
    }
    pmm::free_frames(pa, 16);
    Ok(())
}

fn heap_collections() -> TestResult {
    let boxed = Box::new(0xDEAD_BEEF_u64);
    ensure!(*boxed == 0xDEAD_BEEF, "box content corrupted");
    let v: Vec<u64> = (0..10_000).collect();
    ensure!(v.iter().sum::<u64>() == 49_995_000, "vector sum wrong");
    let mut map = BTreeMap::new();
    for i in 0..1000u32 {
        map.insert(i.wrapping_mul(2_654_435_761), i);
    }
    ensure!(map.len() == 1000, "map lost entries");
    ensure!(map.values().map(|&v| v as u64).sum::<u64>() == 499_500, "map values corrupted");
    let s = format!("{}-{:x}", "meridian", 0xC0FFEE);
    ensure!(s == "meridian-c0ffee", "formatting produced {s}");
    Ok(())
}

fn heap_large() -> TestResult {
    let (in_use_before, _, _) = crate::mm::heap::stats();
    {
        let mut big = alloc::vec![0u8; 1 << 20];
        for (i, b) in big.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        ensure!(big.iter().enumerate().all(|(i, &b)| b == (i % 251) as u8), "1 MiB buffer corrupted");
    }
    let (in_use_after, _, _) = crate::mm::heap::stats();
    ensure!(in_use_after == in_use_before, "large allocation was not released");
    Ok(())
}

fn heap_alignment() -> TestResult {
    use core::alloc::Layout;
    for align in [8usize, 64, 256, 4096, 16384] {
        let layout = Layout::from_size_align(align / 2 + 1, align).unwrap();
        // SAFETY: matching alloc/dealloc with the same layout.
        let p = unsafe { alloc::alloc::alloc(layout) };
        ensure!(!p.is_null(), "allocation with align {align} failed");
        ensure!(p as usize % align == 0, "pointer {p:p} not aligned to {align}");
        unsafe { alloc::alloc::dealloc(p, layout) };
    }
    Ok(())
}

fn paging_map_unmap() -> TestResult {
    let va = kva::reserve(1).ok_or("no kernel address space")?;
    let pa = pmm::alloc_zeroed_frame().ok_or("out of frames")?;
    paging::map_kernel(va, pa, paging::WRITABLE | paging::NO_EXECUTE).map_err(|e| format!("map failed: {e:?}"))?;
    // SAFETY: freshly mapped page.
    unsafe { (va as *mut u64).write_volatile(0x4D45_5249_4449_414E) };
    let through_direct_map = unsafe { (phys_to_virt(pa) as *const u64).read_volatile() };
    ensure!(through_direct_map == 0x4D45_5249_4449_414E, "write not visible through the direct map");
    let (tpa, flags) = paging::translate(paging::kernel_pml4(), va).ok_or("translate failed")?;
    ensure!(tpa == pa, "translate returned {tpa:#x}, expected {pa:#x}");
    ensure!(flags & paging::NO_EXECUTE != 0, "NX bit missing");
    ensure!(paging::map_kernel(va, pa, paging::WRITABLE) == Err(paging::MapError::AlreadyMapped), "double map allowed");
    ensure!(paging::unmap_kernel(va) == Some(pa), "unmap returned the wrong frame");
    ensure!(paging::translate(paging::kernel_pml4(), va).is_none(), "page still mapped");
    ensure!(fixup::read_u64(va).is_none(), "unmapped page is still readable (stale TLB?)");
    pmm::free_frame(pa);
    kva::release(va, 1);
    Ok(())
}

fn guard_page_probe() -> TestResult {
    let stack = kva::alloc_stack(4).ok_or("cannot allocate a kernel stack")?;
    ensure!(fixup::read_u64(stack.base).is_some(), "stack page unreadable");
    ensure!(fixup::read_u64(stack.base - 8).is_none(), "guard page below the stack is mapped");
    kva::free_stack(stack);
    Ok(())
}

fn kernel_protection() -> TestResult {
    unsafe extern "C" {
        static __kernel_start: u8;
    }
    let text = &raw const __kernel_start as u64;
    let (_, flags) = paging::translate(paging::kernel_pml4(), text).ok_or("kernel text not mapped")?;
    ensure!(flags & paging::WRITABLE == 0, "kernel code is writable");
    ensure!(flags & paging::NO_EXECUTE == 0, "kernel code is not executable");
    ensure!(cpu::read_cr0() & (1 << 16) != 0, "CR0.WP is off");
    let (_, flags) = paging::translate(paging::kernel_pml4(), phys_to_virt(0x10_0000)).ok_or("direct map missing")?;
    ensure!(flags & paging::NO_EXECUTE != 0, "direct map is executable");
    ensure!(paging::translate(paging::kernel_pml4(), 0x10_0000).is_none(), "boot identity map still present");
    Ok(())
}

fn breakpoint() -> TestResult {
    let before = idt::BREAKPOINTS.load(Ordering::Relaxed);
    // SAFETY: int3 is handled and returns.
    unsafe { core::arch::asm!("int3") };
    ensure!(idt::BREAKPOINTS.load(Ordering::Relaxed) == before + 1, "breakpoint handler did not run");
    Ok(())
}

fn noncanonical_probe() -> TestResult {
    ensure!(fixup::read_u64(0x8000_0000_0000_0000).is_none(), "non-canonical read did not fault");
    ensure!(fixup::read_u64(0).is_none(), "null page is mapped");
    Ok(())
}

fn irql_raise_lower() -> TestResult {
    ensure!(irql::current() == irql::PASSIVE_LEVEL, "tests must start at PASSIVE_LEVEL");
    let old = irql::raise(irql::DISPATCH_LEVEL);
    ensure!(irql::current() == irql::DISPATCH_LEVEL, "raise to DISPATCH failed");
    let mid = irql::raise(irql::HIGH_LEVEL);
    ensure!(mid == irql::DISPATCH_LEVEL && irql::current() == irql::HIGH_LEVEL, "raise to HIGH failed");
    irql::lower(mid);
    irql::lower(old);
    ensure!(irql::current() == irql::PASSIVE_LEVEL, "lower did not restore PASSIVE_LEVEL");
    Ok(())
}

fn spinlock_irql() -> TestResult {
    static LOCK: SpinLock<u32> = SpinLock::new(0);
    {
        let mut g = LOCK.lock();
        *g += 1;
        ensure!(irql::current() == irql::DISPATCH_LEVEL, "spin lock did not raise IRQL");
        ensure!(LOCK.try_lock_at(irql::DISPATCH_LEVEL).is_none(), "lock acquired twice");
    }
    ensure!(irql::current() == irql::PASSIVE_LEVEL, "IRQL not restored after unlock");
    ensure!(*LOCK.lock() == 1, "protected value wrong");
    Ok(())
}

static TEST_IPI_COUNT: AtomicU64 = AtomicU64::new(0);

fn test_ipi_handler(_: &mut TrapFrame) {
    TEST_IPI_COUNT.fetch_add(1, Ordering::Relaxed);
}

fn apic_self_ipi() -> TestResult {
    const VECTOR: u8 = 0x5E;
    ensure!(cpu::interrupts_enabled(), "interrupts are disabled");
    ensure!(idt::register(VECTOR, test_ipi_handler), "test vector busy");
    let before = TEST_IPI_COUNT.load(Ordering::Relaxed);
    // While at DISPATCH_LEVEL the interrupt (IRQL 5) must stay pending...
    let old = irql::raise(irql::HIGH_LEVEL);
    apic::self_ipi(VECTOR);
    clock::busy_wait_us(1000);
    let masked_ok = TEST_IPI_COUNT.load(Ordering::Relaxed) == before;
    // ...and be delivered as soon as the IRQL drops.
    irql::lower(old);
    clock::busy_wait_us(1000);
    let delivered = TEST_IPI_COUNT.load(Ordering::Relaxed) == before + 1;
    idt::unregister(VECTOR);
    ensure!(masked_ok, "interrupt delivered while masked by IRQL");
    ensure!(delivered, "self-IPI was not delivered after lowering IRQL");
    Ok(())
}

fn clock_ticks() -> TestResult {
    let t0 = clock::ticks();
    let u0 = clock::uptime_us();
    clock::busy_wait_us(50_000);
    let dt = clock::ticks() - t0;
    let du = clock::uptime_us() - u0;
    ensure!(du >= 50_000, "uptime advanced only {du} us");
    // Emulators can deliver ticks late; require a sane range, not precision.
    ensure!((10..=500).contains(&dt), "{dt} clock ticks in 50 ms (expected ~50)");
    Ok(())
}

fn tsc_calibrated() -> TestResult {
    let khz = clock::tsc_khz();
    ensure!(khz > 10_000, "TSC frequency {khz} kHz is implausible");
    ensure!(apic::timer_ticks_per_ms() > 0, "APIC timer not calibrated");
    Ok(())
}

fn hpet_counter() -> TestResult {
    let Some(h) = hpet::get() else { return Ok(()) };
    let a = h.counter();
    clock::busy_wait_us(100);
    ensure!(h.counter() != a, "HPET counter is not running");
    Ok(())
}

fn acpi_tables() -> TestResult {
    let a = acpi::get().ok_or("ACPI was not initialised")?;
    let madt = a.madt.as_ref().ok_or("no MADT")?;
    ensure!(!madt.cpus.is_empty(), "MADT lists no processors");
    ensure!(!madt.ioapics.is_empty(), "MADT lists no I/O APIC");
    ensure!(a.fadt.is_some(), "no FADT");
    Ok(())
}

fn pci_enumeration() -> TestResult {
    let devices = pci::devices();
    ensure!(!devices.is_empty(), "no PCI devices found");
    ensure!(devices.iter().any(|d| d.class == 0x06 && d.subclass == 0x00), "no host bridge found");
    Ok(())
}

fn ps2_decode() -> TestResult {
    use crate::drivers::ps2::{self, Key};
    while ps2::read_event().is_some() {}
    // Shift down, 'a' down/up, shift up, extended up-arrow down.
    for b in [0x2A, 0x1E, 0x9E, 0xAA, 0xE0, 0x48] {
        ps2::inject_scancode(b);
    }
    let e = ps2::read_event().ok_or("no event")?; // shift
    ensure!(e.key == Key::Modifier, "expected modifier, got {:?}", e.key);
    let e = ps2::read_event().ok_or("no event")?;
    ensure!(e.key == Key::Char('A') && e.pressed && e.shift, "expected shifted 'A', got {e:?}");
    let e = ps2::read_event().ok_or("no event")?;
    ensure!(!e.pressed, "expected key release");
    ps2::read_event(); // shift release
    let e = ps2::read_event().ok_or("no event")?;
    ensure!(e.key == Key::Up, "expected Up, got {:?}", e.key);
    Ok(())
}

static DPC_RUNS: AtomicU64 = AtomicU64::new(0);
static TEST_DPC: Dpc = Dpc::new(|| {
    DPC_RUNS.fetch_add(1, Ordering::Relaxed);
});

fn dpc_deferred() -> TestResult {
    let before = DPC_RUNS.load(Ordering::Relaxed);
    let old = irql::raise(irql::HIGH_LEVEL);
    TEST_DPC.queue();
    TEST_DPC.queue(); // already queued: must not run twice
    clock::busy_wait_us(500);
    let early = DPC_RUNS.load(Ordering::Relaxed) != before;
    irql::lower(old);
    ensure!(!early, "DPC ran while IRQL was HIGH");
    ensure!(DPC_RUNS.load(Ordering::Relaxed) == before + 1, "DPC did not run exactly once after IRQL dropped");
    Ok(())
}

fn join(t: &Thread, ms: u64) -> TestResult {
    ensure!(t.terminated.wait(Some(ms)) == WaitStatus::Signaled, "thread '{}' did not finish within {ms} ms", t.name());
    ensure!(t.state() == ThreadState::Terminated, "thread '{}' is {:?}", t.name(), t.state());
    Ok(())
}

fn sched_spawn_join() -> TestResult {
    let value = Arc::new(AtomicU64::new(0));
    let v = value.clone();
    let t = sched::spawn("test-join", priority::NORMAL, move || v.store(42, Ordering::Relaxed))?;
    join(&t, 1000)?;
    ensure!(value.load(Ordering::Relaxed) == 42, "thread body did not run");
    ensure!(sched::current_id() != t.id, "thread ids are not unique");
    Ok(())
}

fn sched_many_threads() -> TestResult {
    let counter = Arc::new(AtomicU64::new(0));
    let mut threads = Vec::new();
    for _ in 0..16 {
        let c = counter.clone();
        threads.push(sched::spawn("test-many", priority::NORMAL, move || {
            for i in 0..100 {
                c.fetch_add(1, Ordering::Relaxed);
                if i % 10 == 0 {
                    sched::yield_now();
                }
            }
        })?);
    }
    for t in &threads {
        join(t, 5000)?;
    }
    ensure!(counter.load(Ordering::Relaxed) == 1600, "counter is {}", counter.load(Ordering::Relaxed));
    Ok(())
}

fn sched_time_slicing() -> TestResult {
    // Two threads that never yield. Without preemptive time slicing the
    // second one (and this one) would never run again.
    let stop = Arc::new(AtomicBool::new(false));
    let counts = Arc::new([AtomicU64::new(0), AtomicU64::new(0)]);
    let mut threads = Vec::new();
    for i in 0..2 {
        let (stop, counts) = (stop.clone(), counts.clone());
        threads.push(sched::spawn("test-spin", priority::NORMAL, move || {
            while !stop.load(Ordering::Relaxed) {
                counts[i].fetch_add(1, Ordering::Relaxed);
            }
        })?);
    }
    sched::sleep_ms(100);
    stop.store(true, Ordering::Relaxed);
    for t in &threads {
        join(t, 2000)?;
    }
    let (a, b) = (counts[0].load(Ordering::Relaxed), counts[1].load(Ordering::Relaxed));
    ensure!(a > 0 && b > 0, "spinning threads did not share the CPU ({a} vs {b} iterations)");
    Ok(())
}

fn sched_priority_preemption() -> TestResult {
    let ran = Arc::new(AtomicBool::new(false));
    let r = ran.clone();
    let t = sched::spawn("test-high", priority::HIGH, move || r.store(true, Ordering::Relaxed))?;
    // A higher-priority thread must run immediately (on this or another
    // processor) without this thread yielding.
    let deadline = clock::uptime_us() + 5000;
    while !ran.load(Ordering::Relaxed) && clock::uptime_us() < deadline {
        core::hint::spin_loop();
    }
    ensure!(ran.load(Ordering::Relaxed), "higher-priority thread did not preempt");
    join(&t, 1000)
}

fn sched_sleep() -> TestResult {
    let t0 = clock::uptime_us();
    sched::sleep_ms(30);
    let dt = clock::uptime_us() - t0;
    ensure!(dt >= 30_000, "slept only {dt} us");
    ensure!(dt < 500_000, "slept {dt} us for a 30 ms sleep");
    Ok(())
}

fn sched_stack_reclaimed() -> TestResult {
    // Check that creating and destroying threads does not leak memory.
    sched::sleep_ms(5);
    let (before, _) = pmm::stats();
    for _ in 0..20 {
        let t = sched::spawn("test-reap", priority::NORMAL, || {})?;
        join(&t, 1000)?;
        drop(t);
        sched::yield_now();
    }
    sched::sleep_ms(5);
    let (after, _) = pmm::stats();
    // The pool may keep a few pages for its size classes.
    ensure!(before.abs_diff(after) <= 8 * PAGE_SIZE, "leaked {} KiB", (before as i64 - after as i64) / 1024);
    Ok(())
}

fn sync_event() -> TestResult {
    let ev = Arc::new(Event::new(false, false));
    let woke = Arc::new(AtomicU64::new(0));
    let (e, w) = (ev.clone(), woke.clone());
    let t = sched::spawn("test-event", priority::NORMAL, move || {
        if e.wait(Some(2000)) == WaitStatus::Signaled {
            w.store(1, Ordering::Relaxed);
        }
    })?;
    sched::sleep_ms(20);
    ensure!(woke.load(Ordering::Relaxed) == 0, "waiter woke before the event was set");
    ensure!(t.state() == ThreadState::Waiting, "waiter is {:?}, expected Waiting", t.state());
    ev.set();
    join(&t, 1000)?;
    ensure!(woke.load(Ordering::Relaxed) == 1, "waiter was not released by set()");
    ensure!(!ev.is_set(), "auto-reset event stayed set after releasing a waiter");
    Ok(())
}

fn sync_event_timeout() -> TestResult {
    let ev = Event::new(false, false);
    let t0 = clock::uptime_us();
    ensure!(ev.wait(Some(25)) == WaitStatus::Timeout, "wait on an unset event did not time out");
    let dt = clock::uptime_us() - t0;
    ensure!(dt >= 25_000, "timed out after only {dt} us");
    ensure!(ev.wait(Some(0)) == WaitStatus::Timeout, "zero-timeout poll did not report timeout");
    Ok(())
}

fn sync_notification_event() -> TestResult {
    let ev = Arc::new(Event::new(true, false));
    let released = Arc::new(AtomicU64::new(0));
    let mut threads = Vec::new();
    for _ in 0..4 {
        let (e, r) = (ev.clone(), released.clone());
        threads.push(sched::spawn("test-notify", priority::NORMAL, move || {
            if e.wait(Some(2000)) == WaitStatus::Signaled {
                r.fetch_add(1, Ordering::Relaxed);
            }
        })?);
    }
    sched::sleep_ms(10);
    ev.set();
    for t in &threads {
        join(t, 1000)?;
    }
    ensure!(released.load(Ordering::Relaxed) == 4, "only {} of 4 waiters released", released.load(Ordering::Relaxed));
    ensure!(ev.is_set(), "manual-reset event reset itself");
    ev.reset();
    ensure!(!ev.is_set(), "reset() did not clear the event");
    Ok(())
}

struct Guarded {
    lock: Mutex,
    value: core::cell::UnsafeCell<u64>,
}

// SAFETY: `value` is only accessed while holding `lock`.
unsafe impl Sync for Guarded {}
unsafe impl Send for Guarded {}

fn sync_mutex() -> TestResult {
    let shared = Arc::new(Guarded { lock: Mutex::new(), value: core::cell::UnsafeCell::new(0) });
    let mut threads = Vec::new();
    for _ in 0..4 {
        let s = shared.clone();
        threads.push(sched::spawn("test-mutex", priority::NORMAL, move || {
            for _ in 0..50 {
                s.lock.acquire(None);
                // Read, yield, write: loses updates unless the mutex excludes.
                // SAFETY: protected by the mutex.
                let v = unsafe { *s.value.get() };
                sched::yield_now();
                unsafe { *s.value.get() = v + 1 };
                s.lock.release();
            }
        })?);
    }
    for t in &threads {
        join(t, 5000)?;
    }
    // SAFETY: all writers have finished.
    let total = unsafe { *shared.value.get() };
    ensure!(total == 200, "mutex lost updates: {total} != 200");
    ensure!(shared.lock.acquire(Some(0)) == WaitStatus::Signaled, "first acquire failed");
    ensure!(shared.lock.acquire(Some(0)) == WaitStatus::Signaled, "recursive acquire failed");
    shared.lock.release();
    shared.lock.release();
    Ok(())
}

fn sync_semaphore() -> TestResult {
    let sem = Arc::new(Semaphore::new(0, 10));
    let consumed = Arc::new(AtomicU64::new(0));
    let mut threads = Vec::new();
    for _ in 0..3 {
        let (s, c) = (sem.clone(), consumed.clone());
        threads.push(sched::spawn("test-sem", priority::NORMAL, move || {
            for _ in 0..5 {
                if s.wait(Some(3000)) == WaitStatus::Signaled {
                    c.fetch_add(1, Ordering::Relaxed);
                }
            }
        })?);
    }
    for _ in 0..3 {
        sem.release(5).map_err(|_| "release(5) failed")?;
        sched::sleep_ms(5);
    }
    for t in &threads {
        join(t, 5000)?;
    }
    ensure!(consumed.load(Ordering::Relaxed) == 15, "consumed {} of 15", consumed.load(Ordering::Relaxed));
    ensure!(sem.release(11).is_err(), "release beyond the limit succeeded");
    ensure!(sem.release(10) == Ok(0), "release up to the limit failed");
    Ok(())
}

fn online_cpus() -> Vec<u32> {
    (0..crate::ke::pcr::MAX_CPUS).filter_map(crate::ke::pcr::by_index).map(|p| p.index).collect()
}

fn smp_processors_online() -> TestResult {
    let enabled = acpi::get().and_then(|a| a.madt.as_ref()).map_or(1, |m| m.cpus.iter().filter(|c| c.enabled).count());
    let online = crate::ke::pcr::online_count() as usize;
    ensure!(online == enabled, "{online} of {enabled} processors online");
    for p in (0..crate::ke::pcr::MAX_CPUS).filter_map(crate::ke::pcr::by_index) {
        ensure!(!p.idle_thread.load(Ordering::Relaxed).is_null(), "cpu{} has no idle thread", p.index);
        ensure!(p.ticks.load(Ordering::Relaxed) > 0, "cpu{} clock is not ticking", p.index);
    }
    Ok(())
}

fn smp_cross_call() -> TestResult {
    for cpu in online_cpus() {
        let got = crate::ke::ipi::call_on(cpu, |x| x + crate::ke::pcr::current().index as u64, 1000)
            .ok_or("processor vanished")?;
        ensure!(got == 1000 + cpu as u64, "call on cpu{cpu} ran on cpu{}", got.wrapping_sub(1000));
    }
    Ok(())
}

fn smp_tlb_shootdown() -> TestResult {
    let me = crate::ke::pcr::current().index;
    let Some(other) = online_cpus().into_iter().find(|&c| c != me) else { return Ok(()) };
    let va = kva::reserve(1).ok_or("no kernel address space")?;
    let a = pmm::alloc_zeroed_frame().ok_or("out of frames")?;
    let b = pmm::alloc_zeroed_frame().ok_or("out of frames")?;
    // SAFETY: freshly allocated frames, written through the direct map.
    unsafe {
        (phys_to_virt(a) as *mut u64).write_volatile(0xAAAA_AAAA);
        (phys_to_virt(b) as *mut u64).write_volatile(0xBBBB_BBBB);
    }
    fn read(va: u64) -> u64 {
        // SAFETY: the test keeps `va` mapped while the call runs.
        unsafe { (va as *const u64).read_volatile() }
    }
    let flags = paging::WRITABLE | paging::NO_EXECUTE;
    paging::map_kernel(va, a, flags).map_err(|e| format!("{e:?}"))?;
    // The other processor now caches va -> a in its TLB.
    let first = crate::ke::ipi::call_on(other, read, va).ok_or("cross call failed")?;
    ensure!(first == 0xAAAA_AAAA, "first read saw {first:#x}");
    // Remapping must not leave a stale translation on the other processor.
    paging::unmap_kernel(va);
    paging::map_kernel(va, b, flags).map_err(|e| format!("{e:?}"))?;
    let second = crate::ke::ipi::call_on(other, read, va).ok_or("cross call failed")?;
    paging::unmap_kernel(va);
    pmm::free_frame(a);
    pmm::free_frame(b);
    kva::release(va, 1);
    ensure!(second == 0xBBBB_BBBB, "cpu{other} read a stale mapping ({second:#x})");
    Ok(())
}

fn smp_parallel_threads() -> TestResult {
    let cpus = online_cpus().len();
    if cpus < 2 {
        return Ok(());
    }
    // Spinning threads at a higher priority than everything else must spread
    // across processors rather than queue on one. They stop on their own:
    // as intended, they starve this lower-priority thread while they run.
    let seen = Arc::new(AtomicU64::new(0));
    let until = clock::uptime_us() + 50_000;
    let mut threads = Vec::new();
    for _ in 0..cpus {
        let seen = seen.clone();
        threads.push(sched::spawn("test-parallel", priority::HIGH, move || {
            while clock::uptime_us() < until {
                seen.fetch_or(1 << crate::ke::pcr::current().index, Ordering::Relaxed);
                core::hint::spin_loop();
            }
        })?);
    }
    for t in &threads {
        join(t, 2000)?;
    }
    let used = seen.load(Ordering::Relaxed).count_ones() as usize;
    ensure!(used >= 2, "threads ran on only {used} of {cpus} processors");
    Ok(())
}
