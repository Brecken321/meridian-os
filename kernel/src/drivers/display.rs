//! Display ownership. The firmware framebuffer belongs to the kernel
//! console until a user-mode program (the desktop) acquires it; the kernel
//! then maps the framebuffer into that process and stops drawing on it.
//! When the owner exits, the kernel console takes the screen back.

use core::sync::atomic::{AtomicU64, Ordering};

use meridian_abi::DisplayInfo;

use super::fbcon;
use crate::mm::PAGE_SIZE;
use crate::mm::user::{AddressSpace, Protection};

/// User virtual address the framebuffer is mapped at.
pub const USER_FRAMEBUFFER: u64 = 0x0000_0300_0000_0000;

static OWNER: AtomicU64 = AtomicU64::new(0);

/// Maps the framebuffer into `space` for process `pid`.
pub fn acquire(pid: u64, space: &AddressSpace) -> Result<(u64, DisplayInfo), &'static str> {
    let info = crate::boot_info();
    if info.has_framebuffer == 0 {
        return Err("there is no framebuffer");
    }
    if OWNER.compare_exchange(0, pid, Ordering::AcqRel, Ordering::Acquire).is_err() {
        return Err("another process owns the display");
    }
    let fb = info.framebuffer;
    let bytes = (fb.stride as u64 * fb.height as u64 * 4).max(fb.size);
    let pages = bytes.div_ceil(PAGE_SIZE);
    if space.map_device(USER_FRAMEBUFFER, fb.base & !(PAGE_SIZE - 1), pages, Protection::ReadWrite).is_err() {
        OWNER.store(0, Ordering::Release);
        return Err("cannot map the framebuffer");
    }
    fbcon::suspend();
    let display = DisplayInfo { width: fb.width, height: fb.height, stride: fb.stride, format: fb.format, size: bytes };
    Ok((USER_FRAMEBUFFER + (fb.base & (PAGE_SIZE - 1)), display))
}

/// Returns the screen to the kernel console if `pid` owns it.
pub fn release(pid: u64) {
    if OWNER.compare_exchange(pid, 0, Ordering::AcqRel, Ordering::Acquire).is_ok() {
        fbcon::resume();
        crate::info!("display: returned to the kernel console");
    }
}

pub fn owner() -> u64 {
    OWNER.load(Ordering::Acquire)
}
