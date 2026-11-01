//! Raw x86-64 processor access: port I/O, MSRs, control registers, CPUID
//! and feature detection.

use core::arch::asm;

pub const IA32_APIC_BASE: u32 = 0x1B;
pub const IA32_EFER: u32 = 0xC000_0080;
pub const IA32_STAR: u32 = 0xC000_0081;
pub const IA32_LSTAR: u32 = 0xC000_0082;
pub const IA32_FMASK: u32 = 0xC000_0084;
pub const IA32_FS_BASE: u32 = 0xC000_0100;
pub const IA32_GS_BASE: u32 = 0xC000_0101;
pub const IA32_KERNEL_GS_BASE: u32 = 0xC000_0102;

#[inline]
pub unsafe fn outb(port: u16, value: u8) {
    unsafe { asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags)) };
}
#[inline]
pub unsafe fn inb(port: u16) -> u8 {
    let v: u8;
    unsafe { asm!("in al, dx", in("dx") port, out("al") v, options(nomem, nostack, preserves_flags)) };
    v
}
#[inline]
pub unsafe fn outw(port: u16, value: u16) {
    unsafe { asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack, preserves_flags)) };
}
#[inline]
pub unsafe fn inw(port: u16) -> u16 {
    let v: u16;
    unsafe { asm!("in ax, dx", in("dx") port, out("ax") v, options(nomem, nostack, preserves_flags)) };
    v
}
#[inline]
pub unsafe fn outl(port: u16, value: u32) {
    unsafe { asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack, preserves_flags)) };
}
#[inline]
pub unsafe fn inl(port: u16) -> u32 {
    let v: u32;
    unsafe { asm!("in eax, dx", in("dx") port, out("eax") v, options(nomem, nostack, preserves_flags)) };
    v
}

/// Small delay used between legacy device register accesses.
#[inline]
pub fn io_wait() {
    // SAFETY: port 0x80 is the POST code port; writing it has no effect.
    unsafe { outb(0x80, 0) };
}

#[inline]
pub unsafe fn rdmsr(msr: u32) -> u64 {
    let (lo, hi): (u32, u32);
    unsafe { asm!("rdmsr", in("ecx") msr, out("eax") lo, out("edx") hi, options(nomem, nostack, preserves_flags)) };
    (hi as u64) << 32 | lo as u64
}

#[inline]
pub unsafe fn wrmsr(msr: u32, value: u64) {
    unsafe {
        asm!("wrmsr", in("ecx") msr, in("eax") value as u32, in("edx") (value >> 32) as u32,
             options(nomem, nostack, preserves_flags))
    };
}

macro_rules! control_register {
    ($read:ident, $write:ident, $reg:literal) => {
        #[inline]
        pub fn $read() -> u64 {
            let v: u64;
            // SAFETY: reading a control register has no side effects.
            unsafe { asm!(concat!("mov {}, ", $reg), out(reg) v, options(nomem, nostack, preserves_flags)) };
            v
        }
        #[inline]
        pub unsafe fn $write(v: u64) {
            unsafe { asm!(concat!("mov ", $reg, ", {}"), in(reg) v, options(nostack, preserves_flags)) };
        }
    };
}

control_register!(read_cr0, write_cr0, "cr0");
control_register!(read_cr3, write_cr3, "cr3");
control_register!(read_cr4, write_cr4, "cr4");
control_register!(read_cr8, write_cr8, "cr8");

#[inline]
pub fn read_cr2() -> u64 {
    let v: u64;
    // SAFETY: reading CR2 has no side effects.
    unsafe { asm!("mov {}, cr2", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

#[inline]
pub fn read_rflags() -> u64 {
    let v: u64;
    // SAFETY: pushfq/pop only touch the stack.
    unsafe { asm!("pushfq", "pop {}", out(reg) v, options(nomem, preserves_flags)) };
    v
}

#[inline]
pub fn interrupts_enabled() -> bool {
    read_rflags() & (1 << 9) != 0
}

#[inline]
pub unsafe fn enable_interrupts() {
    unsafe { asm!("sti", options(nomem, nostack)) };
}

#[inline]
pub unsafe fn disable_interrupts() {
    unsafe { asm!("cli", options(nomem, nostack)) };
}

/// Enables interrupts and halts atomically (the `sti; hlt` idiom), so an
/// interrupt arriving between the two cannot be missed.
#[inline]
pub fn enable_and_halt() {
    // SAFETY: the caller's context permits taking interrupts.
    unsafe { asm!("sti", "hlt", options(nomem, nostack)) };
}

pub fn halt_forever() -> ! {
    loop {
        // SAFETY: stopping the processor is always safe.
        unsafe { asm!("cli", "hlt", options(nomem, nostack)) };
    }
}

#[inline]
pub fn rdtsc() -> u64 {
    let (lo, hi): (u32, u32);
    // SAFETY: RDTSC is unprivileged unless CR4.TSD is set, which we never do.
    unsafe { asm!("rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack, preserves_flags)) };
    (hi as u64) << 32 | lo as u64
}

#[inline]
pub unsafe fn invlpg(addr: u64) {
    unsafe { asm!("invlpg [{}]", in(reg) addr, options(nostack, preserves_flags)) };
}

#[derive(Clone, Copy, Debug, Default)]
pub struct CpuidResult {
    pub eax: u32,
    pub ebx: u32,
    pub ecx: u32,
    pub edx: u32,
}

pub fn cpuid(leaf: u32, subleaf: u32) -> CpuidResult {
    let (eax, ebx, ecx, edx): (u32, u64, u32, u32);
    // SAFETY: CPUID is always available on x86-64. RBX is reserved by LLVM.
    unsafe {
        asm!(
            "mov {tmp}, rbx",
            "cpuid",
            "xchg {tmp}, rbx",
            tmp = out(reg) ebx,
            inout("eax") leaf => eax,
            inout("ecx") subleaf => ecx,
            out("edx") edx,
            options(nostack, preserves_flags),
        );
    }
    CpuidResult { eax, ebx: ebx as u32, ecx, edx }
}

/// Processor identification and the features the kernel cares about.
#[derive(Clone, Copy, Debug)]
pub struct Features {
    pub vendor: [u8; 12],
    pub brand: [u8; 48],
    pub family: u32,
    pub model: u32,
    pub stepping: u32,
    pub max_leaf: u32,
    pub nx: bool,
    pub pge: bool,
    pub pat: bool,
    pub smep: bool,
    pub smap: bool,
    pub umip: bool,
    pub fsgsbase: bool,
    pub x2apic: bool,
    pub apic: bool,
    pub tsc_invariant: bool,
    pub tsc_deadline: bool,
    pub sse3: bool,
    pub sse4_2: bool,
    pub avx: bool,
    pub avx2: bool,
    pub xsave: bool,
    pub rdrand: bool,
    pub hypervisor: bool,
    pub page_1g: bool,
}

impl Default for Features {
    fn default() -> Self {
        // SAFETY: every field is an integer, bool or byte array; all-zero is valid.
        unsafe { core::mem::zeroed() }
    }
}

impl Features {
    pub fn vendor_str(&self) -> &str {
        core::str::from_utf8(&self.vendor).unwrap_or("?")
    }

    pub fn brand_str(&self) -> &str {
        let end = self.brand.iter().position(|&b| b == 0).unwrap_or(self.brand.len());
        core::str::from_utf8(&self.brand[..end]).unwrap_or("?").trim()
    }
}

pub fn detect() -> Features {
    let mut f = Features::default();
    let l0 = cpuid(0, 0);
    f.max_leaf = l0.eax;
    f.vendor[0..4].copy_from_slice(&l0.ebx.to_le_bytes());
    f.vendor[4..8].copy_from_slice(&l0.edx.to_le_bytes());
    f.vendor[8..12].copy_from_slice(&l0.ecx.to_le_bytes());

    let l1 = cpuid(1, 0);
    let base_family = (l1.eax >> 8) & 0xF;
    let base_model = (l1.eax >> 4) & 0xF;
    f.family = if base_family == 0xF { base_family + ((l1.eax >> 20) & 0xFF) } else { base_family };
    f.model = if base_family == 0x6 || base_family == 0xF { base_model | ((l1.eax >> 12) & 0xF0) } else { base_model };
    f.stepping = l1.eax & 0xF;
    f.sse3 = l1.ecx & (1 << 0) != 0;
    f.sse4_2 = l1.ecx & (1 << 20) != 0;
    f.x2apic = l1.ecx & (1 << 21) != 0;
    f.tsc_deadline = l1.ecx & (1 << 24) != 0;
    f.xsave = l1.ecx & (1 << 26) != 0;
    f.avx = l1.ecx & (1 << 28) != 0;
    f.rdrand = l1.ecx & (1 << 30) != 0;
    f.hypervisor = l1.ecx & (1 << 31) != 0;
    f.apic = l1.edx & (1 << 9) != 0;
    f.pge = l1.edx & (1 << 13) != 0;
    f.pat = l1.edx & (1 << 16) != 0;

    if f.max_leaf >= 7 {
        let l7 = cpuid(7, 0);
        f.fsgsbase = l7.ebx & (1 << 0) != 0;
        f.avx2 = l7.ebx & (1 << 5) != 0;
        f.smep = l7.ebx & (1 << 7) != 0;
        f.smap = l7.ebx & (1 << 20) != 0;
        f.umip = l7.ecx & (1 << 2) != 0;
    }

    let max_ext = cpuid(0x8000_0000, 0).eax;
    if max_ext >= 0x8000_0001 {
        let e1 = cpuid(0x8000_0001, 0);
        f.nx = e1.edx & (1 << 20) != 0;
        f.page_1g = e1.edx & (1 << 26) != 0;
    }
    if max_ext >= 0x8000_0004 {
        for (i, leaf) in (0x8000_0002..=0x8000_0004u32).enumerate() {
            let r = cpuid(leaf, 0);
            let o = i * 16;
            f.brand[o..o + 4].copy_from_slice(&r.eax.to_le_bytes());
            f.brand[o + 4..o + 8].copy_from_slice(&r.ebx.to_le_bytes());
            f.brand[o + 8..o + 12].copy_from_slice(&r.ecx.to_le_bytes());
            f.brand[o + 12..o + 16].copy_from_slice(&r.edx.to_le_bytes());
        }
    }
    if max_ext >= 0x8000_0007 {
        f.tsc_invariant = cpuid(0x8000_0007, 0).edx & (1 << 8) != 0;
    }
    f
}

const CR0_WP: u64 = 1 << 16;
const CR4_PGE: u64 = 1 << 7;
const CR4_OSFXSR: u64 = 1 << 9;
const CR4_OSXMMEXCPT: u64 = 1 << 10;
const CR4_UMIP: u64 = 1 << 11;
const CR4_SMEP: u64 = 1 << 20;
const CR4_SMAP: u64 = 1 << 21;
const EFER_SCE: u64 = 1 << 0;
const EFER_NXE: u64 = 1 << 11;

/// Turns on the protection features every Meridian processor runs with:
/// supervisor write protection, NX, SMEP/SMAP/UMIP where available and SSE
/// state support for user mode. Global pages are enabled later, once the
/// boot identity map is gone.
///
/// # Safety
/// Must run on each processor during early initialisation.
pub unsafe fn enable_protection_features(f: &Features) {
    unsafe {
        write_cr0(read_cr0() | CR0_WP);
        let mut cr4 = read_cr4() | CR4_OSFXSR | CR4_OSXMMEXCPT;
        if f.smep {
            cr4 |= CR4_SMEP;
        }
        if f.smap {
            cr4 |= CR4_SMAP;
        }
        if f.umip {
            cr4 |= CR4_UMIP;
        }
        write_cr4(cr4);
        wrmsr(IA32_EFER, rdmsr(IA32_EFER) | EFER_NXE | EFER_SCE);
    }
}

/// Enables global pages (kernel mappings survive CR3 switches).
///
/// # Safety
/// Only call once user-space-visible identity mappings are gone.
pub unsafe fn enable_global_pages(f: &Features) {
    if f.pge {
        unsafe { write_cr4(read_cr4() | CR4_PGE) };
    }
}
