//! User-mode address spaces and safe access to user memory.
//!
//! Each process has its own PML4. Slots 256-511 (the kernel half) point at
//! the shared kernel PDPTs; slots 0-255 are private. All user memory is
//! committed when it is allocated (demand paging comes with the PFN
//! database), and every allocation is tracked as a region so the address
//! space can be torn down completely.

use alloc::collections::BTreeMap;
use core::sync::atomic::{AtomicBool, Ordering};

use super::paging::{self, NO_EXECUTE, USER, WRITABLE};
use super::{PAGE_SIZE, phys_to_virt, pmm};
use crate::sync::SpinLock;

/// First address above user space.
pub const USER_END: u64 = 0x0000_8000_0000_0000;
/// Highest usable user address + 1. The top 64 KiB stay unmapped so a
/// `syscall` instruction can never sit where its return address would be
/// non-canonical.
pub const USER_LIMIT: u64 = USER_END - 0x1_0000;
/// Lowest user address handed out; the first 64 KiB catch NULL dereferences.
pub const USER_MIN: u64 = 0x1_0000;
/// Where `allocate` starts searching.
const ALLOC_BASE: u64 = 0x0000_0100_0000_0000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protection {
    ReadOnly,
    ReadWrite,
    ReadExecute,
    ReadWriteExecute,
}

impl Protection {
    fn pte_flags(self) -> u64 {
        USER | match self {
            Protection::ReadOnly => NO_EXECUTE,
            Protection::ReadWrite => WRITABLE | NO_EXECUTE,
            Protection::ReadExecute => 0,
            Protection::ReadWriteExecute => WRITABLE,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Region {
    pages: u64,
}

pub struct AddressSpace {
    pml4: u64,
    /// Committed regions keyed by base address.
    regions: SpinLock<BTreeMap<u64, Region>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmError {
    OutOfMemory,
    BadAddress,
    Overlap,
    NotFound,
}

impl AddressSpace {
    /// Creates an empty address space sharing the kernel half.
    pub fn new() -> Option<Self> {
        let pml4 = pmm::alloc_zeroed_frame()?;
        let kernel = phys_to_virt(paging::kernel_pml4()) as *const u64;
        let mine = phys_to_virt(pml4) as *mut u64;
        // SAFETY: both are page-table frames; only the kernel half is copied.
        unsafe { core::ptr::copy_nonoverlapping(kernel.add(256), mine.add(256), 256) };
        Some(Self { pml4, regions: SpinLock::new(BTreeMap::new()) })
    }

    pub fn pml4(&self) -> u64 {
        self.pml4
    }

    fn check_range(base: u64, pages: u64) -> Result<u64, VmError> {
        let end = pages.checked_mul(PAGE_SIZE).and_then(|len| base.checked_add(len)).ok_or(VmError::BadAddress)?;
        if base % PAGE_SIZE != 0 || base < USER_MIN || end > USER_LIMIT || pages == 0 {
            return Err(VmError::BadAddress);
        }
        Ok(end)
    }

    /// Commits `pages` zeroed pages at `base`.
    pub fn commit_at(&self, base: u64, pages: u64, protection: Protection) -> Result<(), VmError> {
        let end = Self::check_range(base, pages)?;
        let mut regions = self.regions.lock();
        let overlaps = regions.range(..end).next_back().is_some_and(|(&b, r)| b + r.pages * PAGE_SIZE > base);
        if overlaps {
            return Err(VmError::Overlap);
        }
        for i in 0..pages {
            let va = base + i * PAGE_SIZE;
            let Some(frame) = pmm::alloc_zeroed_frame() else {
                self.release_pages(base, i);
                return Err(VmError::OutOfMemory);
            };
            // SAFETY: `va` is a free user address in this address space.
            if unsafe { paging::map(self.pml4, va, frame, protection.pte_flags()) }.is_err() {
                pmm::free_frame(frame);
                self.release_pages(base, i);
                return Err(VmError::OutOfMemory);
            }
        }
        regions.insert(base, Region { pages });
        Ok(())
    }

    /// Commits `size` bytes anywhere and returns the base address.
    pub fn allocate(&self, size: u64, protection: Protection) -> Result<u64, VmError> {
        let pages = size.div_ceil(PAGE_SIZE).max(1);
        let base = {
            let regions = self.regions.lock();
            let mut candidate = ALLOC_BASE;
            for (&b, r) in regions.range(ALLOC_BASE..) {
                if candidate + (pages + 1) * PAGE_SIZE <= b {
                    break;
                }
                // Leave a one-page gap after every region as a guard.
                candidate = candidate.max(b + (r.pages + 1) * PAGE_SIZE);
            }
            candidate
        };
        self.commit_at(base, pages, protection)?;
        Ok(base)
    }

    /// Releases a region previously committed at exactly `base`.
    pub fn free(&self, base: u64, size: u64) -> Result<(), VmError> {
        let mut regions = self.regions.lock();
        let region = *regions.get(&base).ok_or(VmError::NotFound)?;
        if size.div_ceil(PAGE_SIZE).max(1) != region.pages {
            return Err(VmError::BadAddress);
        }
        regions.remove(&base);
        drop(regions);
        self.release_pages(base, region.pages);
        Ok(())
    }

    /// Changes the protection of committed pages.
    pub fn protect(&self, base: u64, pages: u64, protection: Protection) -> Result<(), VmError> {
        Self::check_range(base, pages)?;
        for i in 0..pages {
            let va = base + i * PAGE_SIZE;
            // SAFETY: only user-half entries of this address space change.
            if !unsafe { paging::set_flags(self.pml4, va, protection.pte_flags()) } {
                return Err(VmError::NotFound);
            }
            crate::ke::ipi::shootdown(va);
        }
        Ok(())
    }

    /// Physical frame backing a committed user page.
    pub fn frame_of(&self, va: u64) -> Option<u64> {
        paging::translate(self.pml4, va & !(PAGE_SIZE - 1)).map(|(pa, _)| pa)
    }

    /// Copies `data` into this address space through the direct map, so it
    /// works whichever address space the calling thread is running in.
    pub fn write(&self, va: u64, data: &[u8]) -> Result<(), VmError> {
        let mut done = 0;
        while done < data.len() {
            let cur = va + done as u64;
            let frame = self.frame_of(cur).ok_or(VmError::BadAddress)?;
            let off = cur % PAGE_SIZE;
            let n = ((PAGE_SIZE - off) as usize).min(data.len() - done);
            // SAFETY: the frame is committed to this address space.
            unsafe { core::ptr::copy_nonoverlapping(data[done..].as_ptr(), (phys_to_virt(frame) + off) as *mut u8, n) };
            done += n;
        }
        Ok(())
    }

    /// Copies from this address space through the direct map.
    pub fn read(&self, va: u64, buf: &mut [u8]) -> Result<(), VmError> {
        let mut done = 0;
        while done < buf.len() {
            let cur = va + done as u64;
            let frame = self.frame_of(cur).ok_or(VmError::BadAddress)?;
            let off = cur % PAGE_SIZE;
            let n = ((PAGE_SIZE - off) as usize).min(buf.len() - done);
            // SAFETY: as in `write`.
            unsafe {
                core::ptr::copy_nonoverlapping((phys_to_virt(frame) + off) as *const u8, buf[done..].as_mut_ptr(), n)
            };
            done += n;
        }
        Ok(())
    }

    fn release_pages(&self, base: u64, pages: u64) {
        for i in 0..pages {
            let va = base + i * PAGE_SIZE;
            // SAFETY: the pages belong to this address space's region.
            if let Some(frame) = unsafe { paging::unmap(self.pml4, va) } {
                crate::ke::ipi::shootdown(va);
                pmm::free_frame(frame);
            }
        }
    }

    /// Total committed bytes.
    pub fn committed(&self) -> u64 {
        self.regions.lock().values().map(|r| r.pages * PAGE_SIZE).sum()
    }
}

impl Drop for AddressSpace {
    fn drop(&mut self) {
        // No processor runs on this address space any more (it is only
        // dropped after its last thread has been switched out).
        let regions = core::mem::take(&mut *self.regions.lock());
        for (base, region) in regions {
            for i in 0..region.pages {
                // SAFETY: as above.
                if let Some(frame) = unsafe { paging::unmap(self.pml4, base + i * PAGE_SIZE) } {
                    pmm::free_frame(frame);
                }
            }
        }
        // SAFETY: as above.
        unsafe { paging::free_lower_half_tables(self.pml4) };
        pmm::free_frame(self.pml4);
    }
}

// --------------------------------------------------------------------------
// Copying to and from user memory
// --------------------------------------------------------------------------

static SMAP: AtomicBool = AtomicBool::new(false);

pub fn init(smap: bool) {
    SMAP.store(smap, Ordering::Relaxed);
}

fn user_range_ok(addr: u64, len: usize) -> bool {
    addr.checked_add(len as u64).is_some_and(|end| end <= USER_LIMIT)
}

/// Copies `len` bytes with `rep movsb`; a fault (unmapped or protected user
/// page) is caught by the fixup table. Returns false on a fault.
///
/// # Safety
/// Exactly one of `dst`/`src` is a user address already range-checked; the
/// other is kernel memory valid for `len` bytes.
unsafe fn guarded_copy(dst: u64, src: u64, len: usize) -> bool {
    let failed: u64;
    unsafe {
        if SMAP.load(Ordering::Relaxed) {
            core::arch::asm!(
                "stac",
                "2:",
                "rep movsb",
                "xor {f:e}, {f:e}",
                "jmp 4f",
                "3:",
                "mov {f:e}, 1",
                "4:",
                "clac",
                ".pushsection .meridian_fixup, \"a\"",
                ".balign 8",
                ".quad 2b, 3b",
                ".popsection",
                inout("rdi") dst => _,
                inout("rsi") src => _,
                inout("rcx") len => _,
                f = out(reg) failed,
                options(nostack),
            );
        } else {
            core::arch::asm!(
                "2:",
                "rep movsb",
                "xor {f:e}, {f:e}",
                "jmp 4f",
                "3:",
                "mov {f:e}, 1",
                "4:",
                ".pushsection .meridian_fixup, \"a\"",
                ".balign 8",
                ".quad 2b, 3b",
                ".popsection",
                inout("rdi") dst => _,
                inout("rsi") src => _,
                inout("rcx") len => _,
                f = out(reg) failed,
                options(nostack),
            );
        }
    }
    failed == 0
}

/// Copies from user address `src` into `dst`.
pub fn copy_from_user(dst: &mut [u8], src: u64) -> Result<(), VmError> {
    if !user_range_ok(src, dst.len()) {
        return Err(VmError::BadAddress);
    }
    // SAFETY: range checked; faults are recovered.
    if unsafe { guarded_copy(dst.as_mut_ptr() as u64, src, dst.len()) } { Ok(()) } else { Err(VmError::BadAddress) }
}

/// Copies `src` to user address `dst`.
pub fn copy_to_user(dst: u64, src: &[u8]) -> Result<(), VmError> {
    if !user_range_ok(dst, src.len()) {
        return Err(VmError::BadAddress);
    }
    // SAFETY: range checked; faults are recovered.
    if unsafe { guarded_copy(dst, src.as_ptr() as u64, src.len()) } { Ok(()) } else { Err(VmError::BadAddress) }
}
