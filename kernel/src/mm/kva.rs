//! Kernel virtual address (KVA) space for things that need their own
//! mappings: kernel stacks (with unmapped guard pages) and device registers
//! (mapped uncached).

use alloc::vec::Vec;

use super::paging::{self, NO_CACHE, NO_EXECUTE, WRITABLE, WRITE_THROUGH};
use super::{PAGE_SIZE, pmm};
use crate::sync::SpinLock;

const KVA_BASE: u64 = 0xFFFF_E000_0100_0000;
const KVA_END: u64 = 0xFFFF_E010_0000_0000;

struct Kva {
    next: u64,
    /// Released ranges (base, pages), reused first-fit.
    free: Vec<(u64, u64)>,
}

static KVA: SpinLock<Kva> = SpinLock::new(Kva { next: KVA_BASE, free: Vec::new() });

/// Reserves `pages` pages of kernel virtual address space (unmapped).
pub fn reserve(pages: u64) -> Option<u64> {
    let mut kva = KVA.lock();
    if let Some(i) = kva.free.iter().position(|&(_, n)| n >= pages) {
        let (base, n) = kva.free[i];
        if n == pages {
            kva.free.swap_remove(i);
        } else {
            kva.free[i] = (base + pages * PAGE_SIZE, n - pages);
        }
        return Some(base);
    }
    let base = kva.next;
    let end = base.checked_add(pages * PAGE_SIZE)?;
    if end > KVA_END {
        return None;
    }
    kva.next = end;
    Some(base)
}

pub fn release(base: u64, pages: u64) {
    KVA.lock().free.push((base, pages));
}

/// Maps `size` bytes of device registers at physical `pa` uncached and
/// returns the virtual address corresponding to `pa`.
pub fn map_mmio(pa: u64, size: u64) -> Option<u64> {
    let start = pa & !(PAGE_SIZE - 1);
    let pages = (pa + size - start).div_ceil(PAGE_SIZE);
    let va = reserve(pages)?;
    for i in 0..pages {
        paging::map_kernel(va + i * PAGE_SIZE, start + i * PAGE_SIZE, WRITABLE | NO_CACHE | WRITE_THROUGH | NO_EXECUTE)
            .ok()?;
    }
    Some(va + (pa - start))
}

/// A kernel stack with an unmapped guard page below it.
#[derive(Debug)]
pub struct KernelStack {
    pub base: u64,
    pub pages: u64,
}

impl KernelStack {
    pub fn top(&self) -> u64 {
        self.base + self.pages * PAGE_SIZE
    }
}

pub fn alloc_stack(pages: u64) -> Option<KernelStack> {
    let region = reserve(pages + 1)?;
    let base = region + PAGE_SIZE; // first page stays unmapped (guard)
    for i in 0..pages {
        let frame = pmm::alloc_frame()?;
        paging::map_kernel(base + i * PAGE_SIZE, frame, WRITABLE | NO_EXECUTE).ok()?;
    }
    Some(KernelStack { base, pages })
}

pub fn free_stack(stack: KernelStack) {
    for i in 0..stack.pages {
        if let Some(frame) = paging::unmap_kernel(stack.base + i * PAGE_SIZE) {
            pmm::free_frame(frame);
        }
    }
    release(stack.base - PAGE_SIZE, stack.pages + 1);
}
