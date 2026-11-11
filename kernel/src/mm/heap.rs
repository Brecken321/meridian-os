//! Kernel pool: the allocator behind `alloc::{Box, Vec, ...}`.
//!
//! Requests up to 2 KiB come from power-of-two size classes carved out of
//! whole pages (objects are naturally aligned to their class size). Larger
//! requests take physically contiguous pages straight from the frame
//! allocator. Both are addressed through the direct map. Allocation must not
//! happen above DISPATCH_LEVEL (i.e. not in interrupt handlers).

use core::alloc::{GlobalAlloc, Layout};
use core::ptr::{self, null_mut};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::{PAGE_SIZE, direct_map_to_phys, phys_to_virt, pmm};
use crate::sync::SpinLock;

const CLASSES: [usize; 8] = [16, 32, 64, 128, 256, 512, 1024, 2048];

struct FreeObject {
    next: *mut FreeObject,
}

struct Lists {
    heads: [*mut FreeObject; CLASSES.len()],
}

// SAFETY: the lists are only touched under the heap lock.
unsafe impl Send for Lists {}

pub struct KernelHeap {
    lists: SpinLock<Lists>,
}

static READY: AtomicBool = AtomicBool::new(false);
static SLAB_PAGES: AtomicU64 = AtomicU64::new(0);
static LARGE_PAGES: AtomicU64 = AtomicU64::new(0);
static BYTES_IN_USE: AtomicU64 = AtomicU64::new(0);

#[global_allocator]
static HEAP: KernelHeap = KernelHeap { lists: SpinLock::new(Lists { heads: [null_mut(); CLASSES.len()] }) };

pub fn init() {
    READY.store(true, Ordering::Release);
}

fn class_index(layout: &Layout) -> Option<usize> {
    let need = layout.size().max(layout.align());
    CLASSES.iter().position(|&c| c >= need)
}

fn large_pages(layout: &Layout) -> u64 {
    (layout.size() as u64).div_ceil(PAGE_SIZE).max(1)
}

/// (bytes requested and not yet freed, pages used by size classes, pages
/// used by large allocations)
pub fn stats() -> (u64, u64, u64) {
    (BYTES_IN_USE.load(Ordering::Relaxed), SLAB_PAGES.load(Ordering::Relaxed), LARGE_PAGES.load(Ordering::Relaxed))
}

unsafe impl GlobalAlloc for KernelHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if !READY.load(Ordering::Acquire) {
            return null_mut();
        }
        debug_assert!(
            crate::ke::irql::current() <= crate::ke::irql::DISPATCH_LEVEL,
            "pool allocation above DISPATCH_LEVEL"
        );
        let p = match class_index(&layout) {
            Some(class) => {
                let mut lists = self.lists.lock();
                if lists.heads[class].is_null() {
                    let Some(page) = pmm::alloc_frame() else { return null_mut() };
                    SLAB_PAGES.fetch_add(1, Ordering::Relaxed);
                    let size = CLASSES[class];
                    let base = phys_to_virt(page) as *mut u8;
                    // Thread every object in the new page onto the list.
                    for i in (0..PAGE_SIZE as usize / size).rev() {
                        // SAFETY: inside the freshly allocated page.
                        unsafe {
                            let obj = base.add(i * size) as *mut FreeObject;
                            (*obj).next = lists.heads[class];
                            lists.heads[class] = obj;
                        }
                    }
                }
                let obj = lists.heads[class];
                // SAFETY: non-null list head owned by the allocator.
                lists.heads[class] = unsafe { (*obj).next };
                obj as *mut u8
            }
            None => {
                let pages = large_pages(&layout);
                let align_frames = (layout.align() as u64 / PAGE_SIZE).max(1);
                match pmm::alloc_contiguous(pages, align_frames) {
                    Some(pa) => {
                        LARGE_PAGES.fetch_add(pages, Ordering::Relaxed);
                        phys_to_virt(pa) as *mut u8
                    }
                    None => null_mut(),
                }
            }
        };
        if !p.is_null() {
            BYTES_IN_USE.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        BYTES_IN_USE.fetch_sub(layout.size() as u64, Ordering::Relaxed);
        match class_index(&layout) {
            Some(class) => {
                let mut lists = self.lists.lock();
                let obj = ptr as *mut FreeObject;
                // SAFETY: `ptr` came from this class and is now unused.
                unsafe { (*obj).next = lists.heads[class] };
                lists.heads[class] = obj;
            }
            None => {
                let pages = large_pages(&layout);
                LARGE_PAGES.fetch_sub(pages, Ordering::Relaxed);
                pmm::free_frames(direct_map_to_phys(ptr as u64), pages);
            }
        }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarding to `alloc` with the same layout.
        let p = unsafe { self.alloc(layout) };
        if !p.is_null() {
            unsafe { ptr::write_bytes(p, 0, layout.size()) };
        }
        p
    }
}
