//! Memory manager ("Mm").
//!
//! * [`pmm`]: physical page frame allocator.
//! * [`paging`]: 4-level page table management and address spaces.
//! * [`kva`]: kernel virtual address ranges for stacks and device registers.
//! * [`heap`]: the kernel pool behind Rust's global allocator.
//!
//! Virtual memory layout (see docs/KERNEL_DESIGN.md):
//!
//! | range                                       | use                         |
//! |---------------------------------------------|-----------------------------|
//! | `0000_0000_0000_0000 – 0000_7FFF_FFFF_FFFF` | user mode (per process)     |
//! | `FFFF_8000_0000_0000 – FFFF_BFFF_FFFF_FFFF` | direct map of physical RAM  |
//! | `FFFF_E000_0000_0000 – FFFF_E000_00FF_FFFF` | boot stack                  |
//! | `FFFF_E000_0100_0000 – FFFF_E00F_FFFF_FFFF` | kernel stacks, MMIO (KVA)   |
//! | `FFFF_FFFF_8000_0000 – FFFF_FFFF_FFFF_FFFF` | kernel image                |

pub mod heap;
pub mod kva;
pub mod paging;
pub mod pmm;

use meridian_bootinfo::{BootInfo, HHDM_BASE};

pub const PAGE_SIZE: u64 = 4096;

/// Virtual address of physical address `pa` in the direct map.
#[inline]
pub fn phys_to_virt(pa: u64) -> u64 {
    HHDM_BASE + pa
}

/// Physical address of a direct-map virtual address.
#[inline]
pub fn direct_map_to_phys(va: u64) -> u64 {
    va - HHDM_BASE
}

pub fn init(info: &BootInfo) {
    pmm::init(info);
    paging::init();
    heap::init();
}
