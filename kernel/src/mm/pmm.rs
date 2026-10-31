//! Physical page frame allocator.
//!
//! A bitmap with one bit per 4 KiB frame (1 = in use) covering all RAM the
//! firmware reported. The bitmap itself is carved out of the first usable
//! region large enough to hold it. Memory below 1 MiB is never handed out:
//! it holds firmware structures and the application-processor trampoline.

use core::slice;

use meridian_bootinfo::{BootInfo, MemoryRegion, memory_kind};

use super::{PAGE_SIZE, phys_to_virt};
use crate::sync::SpinLock;

const LOW_MEMORY_LIMIT: u64 = 0x10_0000;

struct Bitmap {
    words: &'static mut [u64],
    frames: u64,
    free: u64,
    /// Word index where the next search starts.
    hint: usize,
}

impl Bitmap {
    fn is_used(&self, frame: u64) -> bool {
        self.words[(frame / 64) as usize] & (1 << (frame % 64)) != 0
    }

    fn set_used(&mut self, frame: u64) {
        self.words[(frame / 64) as usize] |= 1 << (frame % 64);
    }

    fn set_free(&mut self, frame: u64) {
        self.words[(frame / 64) as usize] &= !(1 << (frame % 64));
    }
}

static BITMAP: SpinLock<Option<Bitmap>> = SpinLock::new(None);
static TOTAL_USABLE: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

pub fn memory_map(info: &BootInfo) -> &'static [MemoryRegion] {
    // SAFETY: the boot manager placed this array in BOOT_INFO memory, which
    // the allocator never hands out.
    unsafe { slice::from_raw_parts(phys_to_virt(info.memory_map) as *const MemoryRegion, info.memory_map_len as usize) }
}

/// Region kinds the allocator tracks (some start out in use).
fn tracked(kind: u32) -> bool {
    matches!(
        kind,
        memory_kind::USABLE
            | memory_kind::KERNEL_IMAGE
            | memory_kind::KERNEL_STACK
            | memory_kind::PAGE_TABLES
            | memory_kind::BOOT_INFO
            | memory_kind::ACPI_RECLAIMABLE
    )
}

pub fn init(info: &BootInfo) {
    let map = memory_map(info);
    let top = map.iter().filter(|r| tracked(r.kind)).map(|r| r.base + r.length).max().unwrap_or(0);
    let frames = top / PAGE_SIZE;
    let words = frames.div_ceil(64);
    let bytes = words * 8;

    let home = map
        .iter()
        .filter(|r| r.kind == memory_kind::USABLE)
        .map(|r| (r.base.max(LOW_MEMORY_LIMIT).next_multiple_of(PAGE_SIZE), r.base + r.length))
        .find(|(start, end)| end.saturating_sub(*start) >= bytes)
        .map(|(start, _)| start)
        .expect("no usable memory region can hold the frame bitmap");

    // SAFETY: `home` is usable RAM nobody else references yet.
    let words_slice = unsafe { slice::from_raw_parts_mut(phys_to_virt(home) as *mut u64, words as usize) };
    words_slice.fill(u64::MAX);
    let mut bitmap = Bitmap { words: words_slice, frames, free: 0, hint: 0 };

    // RAM the operating system owns: free memory plus what the boot manager
    // used for the kernel, its stack, page tables and boot information.
    let owned: u64 =
        map.iter().filter(|r| tracked(r.kind) && r.kind != memory_kind::ACPI_RECLAIMABLE).map(|r| r.length).sum();
    for r in map.iter().filter(|r| r.kind == memory_kind::USABLE) {
        let start = r.base.max(LOW_MEMORY_LIMIT).next_multiple_of(PAGE_SIZE) / PAGE_SIZE;
        let end = (r.base + r.length) / PAGE_SIZE;
        for frame in start..end {
            bitmap.set_free(frame);
            bitmap.free += 1;
        }
    }
    let first = home / PAGE_SIZE;
    for frame in first..first + bytes.div_ceil(PAGE_SIZE) {
        if !bitmap.is_used(frame) {
            bitmap.set_used(frame);
            bitmap.free -= 1;
        }
    }
    TOTAL_USABLE.store(owned, core::sync::atomic::Ordering::Relaxed);
    *BITMAP.lock() = Some(bitmap);
}

/// Allocates one frame and returns its physical address.
pub fn alloc_frame() -> Option<u64> {
    let mut guard = BITMAP.lock();
    let bm = guard.as_mut()?;
    let n = bm.words.len();
    for i in 0..n {
        let w = (bm.hint + i) % n;
        if bm.words[w] != u64::MAX {
            let bit = bm.words[w].trailing_ones() as u64;
            let frame = w as u64 * 64 + bit;
            if frame >= bm.frames {
                continue;
            }
            bm.set_used(frame);
            bm.free -= 1;
            bm.hint = w;
            return Some(frame * PAGE_SIZE);
        }
    }
    None
}

/// Allocates one zero-filled frame.
pub fn alloc_zeroed_frame() -> Option<u64> {
    let pa = alloc_frame()?;
    // SAFETY: the frame was just allocated and is reachable via the direct map.
    unsafe { core::ptr::write_bytes(phys_to_virt(pa) as *mut u8, 0, PAGE_SIZE as usize) };
    Some(pa)
}

/// Allocates `count` physically contiguous frames whose first frame number
/// is a multiple of `align_frames`.
pub fn alloc_contiguous(count: u64, align_frames: u64) -> Option<u64> {
    if count == 0 {
        return None;
    }
    let align = align_frames.max(1);
    let mut guard = BITMAP.lock();
    let bm = guard.as_mut()?;
    let mut start = (LOW_MEMORY_LIMIT / PAGE_SIZE).next_multiple_of(align);
    while start + count <= bm.frames {
        match (start..start + count).find(|&f| bm.is_used(f)) {
            Some(used) => start = (used + 1).next_multiple_of(align),
            None => {
                for f in start..start + count {
                    bm.set_used(f);
                }
                bm.free -= count;
                return Some(start * PAGE_SIZE);
            }
        }
    }
    None
}

/// Returns `count` frames starting at `pa` to the allocator. Freeing a frame
/// that is already free is a kernel bug and stops the system.
pub fn free_frames(pa: u64, count: u64) {
    let mut guard = BITMAP.lock();
    let bm = guard.as_mut().expect("physical allocator not initialised");
    let first = pa / PAGE_SIZE;
    for frame in first..first + count {
        assert!(frame < bm.frames && bm.is_used(frame), "double free of physical frame {:#x}", frame * PAGE_SIZE);
        bm.set_free(frame);
    }
    bm.free += count;
}

pub fn free_frame(pa: u64) {
    free_frames(pa, 1)
}

/// (free bytes, bytes of RAM owned by the operating system)
pub fn stats() -> (u64, u64) {
    let free = BITMAP.lock().as_ref().map(|b| b.free * PAGE_SIZE).unwrap_or(0);
    (free, TOTAL_USABLE.load(core::sync::atomic::Ordering::Relaxed))
}
