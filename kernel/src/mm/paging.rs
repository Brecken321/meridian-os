//! Page table management.
//!
//! The kernel half of the address space (PML4 slots 256-511) is shared by
//! every address space: all 256 upper-level tables are created at boot and
//! never freed, so a new process only has to copy those PML4 entries.

use core::sync::atomic::{AtomicU64, Ordering};

use super::{PAGE_SIZE, phys_to_virt, pmm};
use crate::arch::x86_64::cpu;
use crate::sync::SpinLock;

pub const PRESENT: u64 = 1 << 0;
pub const WRITABLE: u64 = 1 << 1;
pub const USER: u64 = 1 << 2;
pub const WRITE_THROUGH: u64 = 1 << 3;
pub const NO_CACHE: u64 = 1 << 4;
pub const ACCESSED: u64 = 1 << 5;
pub const DIRTY: u64 = 1 << 6;
pub const HUGE: u64 = 1 << 7;
pub const GLOBAL: u64 = 1 << 8;
pub const NO_EXECUTE: u64 = 1 << 63;

const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapError {
    OutOfMemory,
    AlreadyMapped,
    NotMapped,
    HugePageInTheWay,
}

static KERNEL_PML4: AtomicU64 = AtomicU64::new(0);
/// Serialises changes to kernel-half page tables.
static KERNEL_LOCK: SpinLock<()> = SpinLock::new(());

pub fn kernel_pml4() -> u64 {
    KERNEL_PML4.load(Ordering::Relaxed)
}

fn table(pa: u64) -> &'static mut [u64; 512] {
    // SAFETY: page table frames are reachable through the direct map.
    unsafe { &mut *(phys_to_virt(pa) as *mut [u64; 512]) }
}

fn index(va: u64, level: u32) -> usize {
    ((va >> (12 + 9 * level)) & 0x1FF) as usize
}

/// Adopts the boot manager's page tables, removes its identity map (freeing
/// those tables) and pre-creates every kernel-half PDPT.
pub fn init() {
    let pml4 = cpu::read_cr3() & ADDR_MASK;
    KERNEL_PML4.store(pml4, Ordering::Relaxed);
    let top = table(pml4);
    for slot in 0..256 {
        let e = top[slot];
        if e & PRESENT == 0 {
            continue;
        }
        let pdpt = e & ADDR_MASK;
        for &pdpte in table(pdpt).iter() {
            if pdpte & PRESENT != 0 && pdpte & HUGE == 0 {
                // The identity map only uses 2 MiB leaves, so PDs have no
                // child tables of their own.
                pmm::free_frame(pdpte & ADDR_MASK);
            }
        }
        pmm::free_frame(pdpt);
        top[slot] = 0;
    }
    for slot in 256..512 {
        if top[slot] & PRESENT == 0 {
            let pdpt = pmm::alloc_zeroed_frame().expect("out of memory creating kernel page tables");
            top[slot] = pdpt | PRESENT | WRITABLE;
        }
    }
    // SAFETY: reloading CR3 flushes the removed identity mappings.
    unsafe { cpu::write_cr3(pml4) };
}

/// Walks to the page table entry for `va`, creating intermediate tables if
/// `create` is set. Intermediate entries get USER if `user` is set.
fn walk(pml4: u64, va: u64, create: bool, user: bool) -> Result<&'static mut u64, MapError> {
    let mut t = pml4;
    for level in (1..=3).rev() {
        let e = &mut table(t)[index(va, level)];
        if *e & PRESENT == 0 {
            if !create {
                return Err(MapError::NotMapped);
            }
            let new = pmm::alloc_zeroed_frame().ok_or(MapError::OutOfMemory)?;
            *e = new | PRESENT | WRITABLE | if user { USER } else { 0 };
        } else if *e & HUGE != 0 {
            return Err(MapError::HugePageInTheWay);
        } else if user {
            *e |= USER;
        }
        t = *e & ADDR_MASK;
    }
    Ok(&mut table(t)[index(va, 0)])
}

/// Maps the 4 KiB page at `va` to `pa` in the address space rooted at `pml4`.
///
/// # Safety
/// The caller must own `va` in that address space and `pa` must be a frame
/// it is allowed to expose with `flags`.
pub unsafe fn map(pml4: u64, va: u64, pa: u64, flags: u64) -> Result<(), MapError> {
    let pte = walk(pml4, va, true, flags & USER != 0)?;
    if *pte & PRESENT != 0 {
        return Err(MapError::AlreadyMapped);
    }
    *pte = (pa & ADDR_MASK) | flags | PRESENT;
    Ok(())
}

/// Removes the mapping for `va` and returns the frame it pointed to.
///
/// # Safety
/// Nothing may still use the mapping. Other processors' TLBs are not yet
/// shot down (single-processor kernel for now).
pub unsafe fn unmap(pml4: u64, va: u64) -> Option<u64> {
    let pte = walk(pml4, va, false, false).ok()?;
    if *pte & PRESENT == 0 {
        return None;
    }
    let pa = *pte & ADDR_MASK;
    *pte = 0;
    // SAFETY: invalidating a TLB entry is always safe.
    unsafe { cpu::invlpg(va) };
    Some(pa)
}

/// Replaces the flags of the 4 KiB mapping at `va`, keeping its frame.
/// Returns false if `va` is not mapped. The caller flushes TLBs.
///
/// # Safety
/// The caller must own `va` in this address space.
pub unsafe fn set_flags(pml4: u64, va: u64, flags: u64) -> bool {
    match walk(pml4, va, false, false) {
        Ok(pte) if *pte & PRESENT != 0 => {
            *pte = (*pte & ADDR_MASK) | flags | PRESENT;
            true
        }
        _ => false,
    }
}

/// Translates `va` to (physical address, leaf flags), following huge pages.
pub fn translate(pml4: u64, va: u64) -> Option<(u64, u64)> {
    let mut t = pml4;
    for level in (1..=3).rev() {
        let e = table(t)[index(va, level)];
        if e & PRESENT == 0 {
            return None;
        }
        if e & HUGE != 0 && level < 3 {
            let page = 1u64 << (12 + 9 * level);
            return Some(((e & ADDR_MASK & !(page - 1)) + (va & (page - 1)), e & !ADDR_MASK));
        }
        t = e & ADDR_MASK;
    }
    let e = table(t)[index(va, 0)];
    (e & PRESENT != 0).then(|| ((e & ADDR_MASK) + (va & (PAGE_SIZE - 1)), e & !ADDR_MASK))
}

/// Maps a page in the shared kernel half of the address space.
pub fn map_kernel(va: u64, pa: u64, flags: u64) -> Result<(), MapError> {
    assert!(va >= 0xFFFF_8000_0000_0000, "map_kernel used for a user address");
    let _guard = KERNEL_LOCK.lock();
    // SAFETY: kernel-half addresses are managed by the kernel allocators.
    unsafe { map(kernel_pml4(), va, pa, flags | GLOBAL) }
}

/// Unmaps a kernel-half page and invalidates it on every processor before
/// returning, so the caller may immediately reuse the frame and address.
pub fn unmap_kernel(va: u64) -> Option<u64> {
    let pa = {
        let _guard = KERNEL_LOCK.lock();
        // SAFETY: the caller owns `va` (kernel allocators only).
        unsafe { unmap(kernel_pml4(), va) }
    };
    if pa.is_some() {
        crate::ke::ipi::shootdown(va);
    }
    pa
}

/// Frees every page-table frame (not the mapped frames themselves) below
/// PML4 slot 256 and clears those slots.
///
/// # Safety
/// No processor may be using the lower half of this address space.
pub unsafe fn free_lower_half_tables(pml4: u64) {
    let top = table(pml4);
    for slot in 0..256 {
        let e = top[slot];
        if e & PRESENT == 0 {
            continue;
        }
        let pdpt = e & ADDR_MASK;
        for &pdpte in table(pdpt).iter() {
            if pdpte & PRESENT == 0 || pdpte & HUGE != 0 {
                continue;
            }
            let pd = pdpte & ADDR_MASK;
            for &pde in table(pd).iter() {
                if pde & PRESENT != 0 && pde & HUGE == 0 {
                    pmm::free_frame(pde & ADDR_MASK);
                }
            }
            pmm::free_frame(pd);
        }
        pmm::free_frame(pdpt);
        top[slot] = 0;
    }
}
