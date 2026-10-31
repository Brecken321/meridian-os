//! Processor control region (PCR): per-CPU kernel state, reached through
//! the GS segment base while running in kernel mode.

use core::cell::UnsafeCell;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, Ordering};

use super::dpc::DpcQueue;
use super::thread::Thread;
use crate::arch::x86_64::cpu;
use crate::arch::x86_64::gdt::{Gdt, Tss};
use crate::sync::{RacyCell, SpinLock};

pub const MAX_CPUS: usize = 64;

#[repr(C, align(64))]
pub struct Pcr {
    /// Points at this structure; lives at GS:0 so [`current`] is one load.
    pub self_ptr: *const Pcr,
    /// Scratch slot used by the system call entry path (GS:8).
    pub user_rsp: u64,
    /// Kernel stack top for system calls (GS:16).
    pub kernel_rsp: u64,
    /// Logical processor number (0 = bootstrap processor).
    pub index: u32,
    pub apic_id: AtomicU32,
    pub ticks: AtomicU64,
    pub interrupts: AtomicU64,
    pub gdt: UnsafeCell<Gdt>,
    pub tss: UnsafeCell<Tss>,
    /// Thread running on this processor (owns one strong reference).
    pub current_thread: AtomicPtr<Thread>,
    /// This processor's idle thread (owns one strong reference).
    pub idle_thread: AtomicPtr<Thread>,
    /// Hand-off slot for the thread being switched away from.
    pub previous_thread: AtomicPtr<Thread>,
    /// Set when the running thread should be preempted at the next
    /// dispatch interrupt (quantum end or a higher-priority thread is ready).
    pub need_resched: AtomicBool,
    pub dpcs: SpinLock<DpcQueue>,
}

// SAFETY: fields are either atomics, immutable after setup, or only touched
// by the owning processor (GDT/TSS).
unsafe impl Sync for Pcr {}

impl Pcr {
    pub const fn new(index: u32, apic_id: u32) -> Self {
        Self {
            self_ptr: ptr::null(),
            user_rsp: 0,
            kernel_rsp: 0,
            index,
            apic_id: AtomicU32::new(apic_id),
            ticks: AtomicU64::new(0),
            interrupts: AtomicU64::new(0),
            gdt: UnsafeCell::new(Gdt::new()),
            tss: UnsafeCell::new(Tss::new()),
            current_thread: AtomicPtr::new(ptr::null_mut()),
            idle_thread: AtomicPtr::new(ptr::null_mut()),
            previous_thread: AtomicPtr::new(ptr::null_mut()),
            need_resched: AtomicBool::new(false),
            dpcs: SpinLock::new(DpcQueue::new()),
        }
    }
}

static BSP_PCR: RacyCell<Pcr> = RacyCell::new(Pcr::new(0, 0));
static CPUS: [AtomicPtr<Pcr>; MAX_CPUS] = [const { AtomicPtr::new(ptr::null_mut()) }; MAX_CPUS];
static CPU_COUNT: AtomicU32 = AtomicU32::new(0);

/// The bootstrap processor's PCR (statically allocated so it exists before
/// the heap does).
pub fn bsp() -> *mut Pcr {
    BSP_PCR.get()
}

/// Makes `pcr` the current processor's PCR by loading the GS base.
///
/// # Safety
/// `pcr` must stay valid forever and must not be installed on another CPU.
pub unsafe fn install(pcr: *mut Pcr) {
    unsafe {
        (*pcr).self_ptr = pcr;
        cpu::wrmsr(cpu::IA32_GS_BASE, pcr as u64);
        CPUS[(*pcr).index as usize].store(pcr, Ordering::Release);
    }
    CPU_COUNT.fetch_add(1, Ordering::AcqRel);
}

/// The current processor's PCR. Only valid after [`install`] ran on this CPU.
#[inline]
pub fn current() -> &'static Pcr {
    let p: *const Pcr;
    // SAFETY: GS:0 holds the self pointer once the PCR is installed.
    unsafe {
        core::arch::asm!("mov {}, gs:[0]", out(reg) p, options(nostack, preserves_flags, readonly));
        &*p
    }
}

pub fn by_index(index: usize) -> Option<&'static Pcr> {
    let p = CPUS.get(index)?.load(Ordering::Acquire);
    // SAFETY: installed PCRs are never freed.
    (!p.is_null()).then(|| unsafe { &*p })
}

/// Number of processors that have installed a PCR (i.e. are online).
pub fn online_count() -> u32 {
    CPU_COUNT.load(Ordering::Acquire)
}
