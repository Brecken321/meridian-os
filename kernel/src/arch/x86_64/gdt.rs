//! Global descriptor table and task state segment (one of each per CPU).
//!
//! The selector layout mirrors the one SYSCALL/SYSRET require and leaves
//! room for a 32-bit user code segment (future WoW64-style compatibility):
//!
//! | selector | descriptor                    |
//! |----------|-------------------------------|
//! | 0x10     | kernel code (64-bit)          |
//! | 0x18     | kernel data                   |
//! | 0x23     | user code (32-bit, DPL 3)     |
//! | 0x2B     | user data (DPL 3)             |
//! | 0x33     | user code (64-bit, DPL 3)     |
//! | 0x40     | TSS (16-byte system entry)    |

use core::arch::asm;
use core::mem::size_of;

pub const KERNEL_CS: u16 = 0x10;
pub const KERNEL_DS: u16 = 0x18;
pub const USER_CS32: u16 = 0x20 | 3;
pub const USER_DS: u16 = 0x28 | 3;
pub const USER_CS: u16 = 0x30 | 3;
pub const TSS_SELECTOR: u16 = 0x40;

/// Interrupt stack table slots (1-based, as encoded in IDT entries).
pub const IST_DOUBLE_FAULT: u8 = 1;
pub const IST_NMI: u8 = 2;
pub const IST_MACHINE_CHECK: u8 = 3;

const KERNEL_CODE64: u64 = 0x00AF_9B00_0000_FFFF;
const KERNEL_DATA: u64 = 0x00CF_9300_0000_FFFF;
const USER_CODE32: u64 = 0x00CF_FB00_0000_FFFF;
const USER_DATA: u64 = 0x00CF_F300_0000_FFFF;
const USER_CODE64: u64 = 0x00AF_FB00_0000_FFFF;

#[repr(C, packed(4))]
pub struct Tss {
    reserved0: u32,
    /// Stack pointers loaded on a privilege change to ring 0..2.
    pub rsp: [u64; 3],
    reserved1: u64,
    /// Interrupt stack table.
    pub ist: [u64; 7],
    reserved2: u64,
    reserved3: u16,
    pub iomap_base: u16,
}

impl Tss {
    pub const fn new() -> Self {
        Self {
            reserved0: 0,
            rsp: [0; 3],
            reserved1: 0,
            ist: [0; 7],
            reserved2: 0,
            reserved3: 0,
            // No I/O permission bitmap: user mode gets no port access.
            iomap_base: size_of::<Tss>() as u16,
        }
    }
}

#[repr(C, align(16))]
pub struct Gdt {
    entries: [u64; 10],
}

impl Gdt {
    pub const fn new() -> Self {
        Self { entries: [0, 0, KERNEL_CODE64, KERNEL_DATA, USER_CODE32, USER_DATA, USER_CODE64, 0, 0, 0] }
    }

    fn set_tss(&mut self, tss: *const Tss) {
        let base = tss as u64;
        let limit = (size_of::<Tss>() - 1) as u64;
        let low = (limit & 0xFFFF)
            | ((base & 0xFF_FFFF) << 16)
            | (0x89 << 40) // present, available 64-bit TSS
            | (((limit >> 16) & 0xF) << 48)
            | (((base >> 24) & 0xFF) << 56);
        self.entries[8] = low;
        self.entries[9] = base >> 32;
    }
}

#[repr(C, packed)]
struct DescriptorPointer {
    limit: u16,
    base: u64,
}

/// Loads `gdt` (with `tss` installed in it), reloads every segment register
/// and the task register.
///
/// # Safety
/// Both structures must remain valid for as long as the CPU runs.
pub unsafe fn load(gdt: *mut Gdt, tss: *const Tss) {
    unsafe {
        (*gdt).set_tss(tss);
        let ptr = DescriptorPointer { limit: (size_of::<Gdt>() - 1) as u16, base: gdt as u64 };
        asm!("lgdt [{}]", in(reg) &ptr, options(readonly, nostack, preserves_flags));
        asm!(
            "push {cs}",
            "lea {tmp}, [rip + 2f]",
            "push {tmp}",
            "retfq",
            "2:",
            "mov {tmp:e}, {ds}",
            "mov ds, {tmp:x}",
            "mov es, {tmp:x}",
            "mov ss, {tmp:x}",
            "xor {tmp:e}, {tmp:e}",
            "mov fs, {tmp:x}",
            "mov gs, {tmp:x}",
            cs = const KERNEL_CS as u64,
            ds = const KERNEL_DS as u32,
            tmp = out(reg) _,
        );
        asm!("ltr {0:x}", in(reg) TSS_SELECTOR, options(nostack, preserves_flags));
    }
}
