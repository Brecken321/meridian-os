//! Builds the 4-level page tables the kernel is entered with.
//!
//! The boot manager runs on the firmware's identity mapping, so page table
//! frames are addressed directly by their physical address here.

pub const PRESENT: u64 = 1 << 0;
pub const WRITABLE: u64 = 1 << 1;
pub const HUGE: u64 = 1 << 7;
pub const GLOBAL: u64 = 1 << 8;
pub const NO_EXECUTE: u64 = 1 << 63;

const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;
pub const PAGE_4K: u64 = 0x1000;
pub const PAGE_2M: u64 = 0x20_0000;

pub struct PageTables<F: FnMut() -> Option<u64>> {
    pub pml4: u64,
    alloc: F,
}

fn index(virt: u64, level: u32) -> usize {
    ((virt >> (12 + 9 * level)) & 0x1FF) as usize
}

impl<F: FnMut() -> Option<u64>> PageTables<F> {
    /// `alloc` must return the physical address of a zeroed 4 KiB frame.
    pub fn new(mut alloc: F) -> Option<Self> {
        let pml4 = alloc()?;
        Some(Self { pml4, alloc })
    }

    fn entry(table: u64, index: usize) -> *mut u64 {
        (table as *mut u64).wrapping_add(index)
    }

    /// Returns the next-level table referenced by `table[index]`, creating it
    /// if needed. Intermediate entries are always present + writable; leaf
    /// entries carry the real permissions.
    fn next(&mut self, table: u64, index: usize) -> Option<u64> {
        let e = Self::entry(table, index);
        // SAFETY: `table` is a page table frame we allocated (identity mapped).
        unsafe {
            if *e & PRESENT == 0 {
                let new = (self.alloc)()?;
                *e = new | PRESENT | WRITABLE;
            }
            Some(*e & ADDR_MASK)
        }
    }

    pub fn map_4k(&mut self, virt: u64, phys: u64, flags: u64) -> Option<()> {
        let pdpt = self.next(self.pml4, index(virt, 3))?;
        let pd = self.next(pdpt, index(virt, 2))?;
        let pt = self.next(pd, index(virt, 1))?;
        let e = Self::entry(pt, index(virt, 0));
        // SAFETY: as above.
        unsafe {
            if *e & PRESENT != 0 {
                // Two segments share this page: grant the union of access.
                let mut merged = *e | (flags & WRITABLE);
                if flags & NO_EXECUTE == 0 {
                    merged &= !NO_EXECUTE;
                }
                *e = merged;
            } else {
                *e = (phys & ADDR_MASK) | flags | PRESENT;
            }
        }
        Some(())
    }

    pub fn map_2m(&mut self, virt: u64, phys: u64, flags: u64) -> Option<()> {
        let pdpt = self.next(self.pml4, index(virt, 3))?;
        let pd = self.next(pdpt, index(virt, 2))?;
        // SAFETY: as above.
        unsafe { *Self::entry(pd, index(virt, 1)) = (phys & ADDR_MASK) | flags | PRESENT | HUGE };
        Some(())
    }

    /// Maps `[phys, phys + len)` at `virt` with 2 MiB pages (`len` rounded up).
    pub fn map_range_2m(&mut self, virt: u64, phys: u64, len: u64, flags: u64) -> Option<()> {
        let mut off = 0;
        while off < len {
            self.map_2m(virt + off, phys + off, flags)?;
            off += PAGE_2M;
        }
        Some(())
    }
}
