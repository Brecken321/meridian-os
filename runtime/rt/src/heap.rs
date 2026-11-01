//! User-mode memory allocator behind `alloc::{Box, Vec, String}`.
//!
//! Small requests (up to 4 KiB) come from power-of-two size classes carved
//! out of 64 KiB chunks obtained with `ALLOCATE_MEMORY`; larger requests are
//! committed and released directly by the kernel.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr::null_mut;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::{allocate_memory, free_memory, protection};

const CLASSES: [usize; 9] = [16, 32, 64, 128, 256, 512, 1024, 2048, 4096];
const CHUNK: usize = 64 * 1024;

struct FreeObject {
    next: *mut FreeObject,
}

pub struct UserHeap {
    lock: AtomicBool,
    heads: core::cell::UnsafeCell<[*mut FreeObject; CLASSES.len()]>,
}

// SAFETY: the free lists are only touched while `lock` is held.
unsafe impl Sync for UserHeap {}

#[global_allocator]
static HEAP: UserHeap =
    UserHeap { lock: AtomicBool::new(false), heads: core::cell::UnsafeCell::new([null_mut(); CLASSES.len()]) };

fn class_index(layout: &Layout) -> Option<usize> {
    let need = layout.size().max(layout.align());
    CLASSES.iter().position(|&c| c >= need)
}

impl UserHeap {
    fn with_lists<R>(&self, f: impl FnOnce(&mut [*mut FreeObject; CLASSES.len()]) -> R) -> R {
        while self.lock.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            core::hint::spin_loop();
        }
        // SAFETY: lock held.
        let r = f(unsafe { &mut *self.heads.get() });
        self.lock.store(false, Ordering::Release);
        r
    }
}

unsafe impl GlobalAlloc for UserHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        match class_index(&layout) {
            Some(class) => self.with_lists(|heads| {
                if heads[class].is_null() {
                    let Ok(chunk) = allocate_memory(CHUNK, protection::READ_WRITE) else { return null_mut() };
                    let size = CLASSES[class];
                    for i in (0..CHUNK / size).rev() {
                        // SAFETY: inside the freshly committed chunk.
                        unsafe {
                            let obj = chunk.add(i * size) as *mut FreeObject;
                            (*obj).next = heads[class];
                            heads[class] = obj;
                        }
                    }
                }
                let obj = heads[class];
                // SAFETY: non-null head owned by the allocator.
                heads[class] = unsafe { (*obj).next };
                obj as *mut u8
            }),
            None if layout.align() <= 4096 => {
                allocate_memory(layout.size(), protection::READ_WRITE).unwrap_or(null_mut())
            }
            None => null_mut(),
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        match class_index(&layout) {
            Some(class) => self.with_lists(|heads| {
                let obj = ptr as *mut FreeObject;
                // SAFETY: `ptr` came from this class and is no longer used.
                unsafe { (*obj).next = heads[class] };
                heads[class] = obj;
            }),
            None => {
                let _ = free_memory(ptr, layout.size());
            }
        }
    }
}
