//! Boot handoff structures shared by the Meridian boot manager and kernel.
//!
//! The boot manager is a UEFI (PE32+) application and the kernel is a
//! bare-metal ELF image, so everything here is `#[repr(C)]` and built only
//! from plain integers: both sides see exactly the same layout.
//!
//! All addresses stored in [`BootInfo`] are *physical* unless the field name
//! says otherwise. The kernel reaches them through the higher-half direct map
//! at [`HHDM_BASE`].

#![no_std]

/// `"MERIDIAN"` in little-endian ASCII.
pub const BOOTINFO_MAGIC: u64 = u64::from_le_bytes(*b"MERIDIAN");
/// Bumped whenever the layout of [`BootInfo`] changes.
pub const BOOTINFO_VERSION: u32 = 2;

/// Virtual address at which all physical memory is mapped (PML4 slot 256).
pub const HHDM_BASE: u64 = 0xFFFF_8000_0000_0000;
/// Virtual address the kernel image is linked at (top 2 GiB, PML4 slot 511).
pub const KERNEL_BASE: u64 = 0xFFFF_FFFF_8000_0000;
/// Lowest mapped byte of the bootstrap processor's boot stack. The page just
/// below it is left unmapped as a guard page.
pub const BOOT_STACK_BASE: u64 = 0xFFFF_E000_0000_1000;
/// Size of the boot stack in bytes.
pub const BOOT_STACK_SIZE: u64 = 64 * 1024;

/// Physical memory region types reported to the kernel.
pub mod memory_kind {
    /// Free RAM the kernel may use immediately.
    pub const USABLE: u32 = 1;
    /// Firmware-reserved or otherwise unusable address space.
    pub const RESERVED: u32 = 2;
    /// ACPI tables; reusable once the kernel has finished parsing them.
    pub const ACPI_RECLAIMABLE: u32 = 3;
    /// ACPI non-volatile storage; must be preserved across sleep states.
    pub const ACPI_NVS: u32 = 4;
    /// RAM reported as defective.
    pub const UNUSABLE: u32 = 5;
    /// UEFI runtime services code and data; must stay mapped and untouched.
    pub const FIRMWARE_RUNTIME: u32 = 6;
    /// Memory-mapped I/O ranges listed by the firmware.
    pub const MMIO: u32 = 7;
    /// The loaded kernel image.
    pub const KERNEL_IMAGE: u32 = 8;
    /// The bootstrap processor's boot stack.
    pub const KERNEL_STACK: u32 = 9;
    /// Page tables built by the boot manager (still active at kernel entry).
    pub const PAGE_TABLES: u32 = 10;
    /// This structure, the memory map and the command line.
    pub const BOOT_INFO: u32 = 11;
    /// A page below 1 MiB reserved for the application-processor trampoline.
    pub const AP_TRAMPOLINE: u32 = 12;
    /// A file the boot manager loaded for the kernel (see [`super::BootModule`]).
    pub const BOOT_MODULE: u32 = 13;

    /// Human-readable name for diagnostics.
    pub const fn name(kind: u32) -> &'static str {
        match kind {
            USABLE => "usable",
            RESERVED => "reserved",
            ACPI_RECLAIMABLE => "acpi-reclaim",
            ACPI_NVS => "acpi-nvs",
            UNUSABLE => "unusable",
            FIRMWARE_RUNTIME => "fw-runtime",
            MMIO => "mmio",
            KERNEL_IMAGE => "kernel",
            KERNEL_STACK => "kstack",
            PAGE_TABLES => "pagetables",
            BOOT_INFO => "bootinfo",
            AP_TRAMPOLINE => "ap-tramp",
            BOOT_MODULE => "module",
            _ => "unknown",
        }
    }
}

/// One contiguous run of physical memory of a single kind.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MemoryRegion {
    pub base: u64,
    pub length: u64,
    pub kind: u32,
    pub reserved: u32,
}

/// Pixel layouts the kernel framebuffer driver understands.
pub mod pixel_format {
    /// Byte 0 = red, byte 1 = green, byte 2 = blue, byte 3 = unused.
    pub const RGBX: u32 = 0;
    /// Byte 0 = blue, byte 1 = green, byte 2 = red, byte 3 = unused.
    pub const BGRX: u32 = 1;
}

/// The linear framebuffer set up by UEFI GOP.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct FramebufferInfo {
    pub base: u64,
    pub size: u64,
    pub width: u32,
    pub height: u32,
    /// Pixels (not bytes) per scan line.
    pub stride: u32,
    pub format: u32,
}

/// A file loaded by the boot manager before the kernel has storage drivers
/// (for example the first user-mode program).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct BootModule {
    /// File name (UTF-8, not NUL terminated), e.g. `init.exe`.
    pub name: [u8; 32],
    pub name_len: u32,
    pub reserved: u32,
    /// Physical address and size of the file contents.
    pub base: u64,
    pub size: u64,
}

impl BootModule {
    pub const EMPTY: Self = Self { name: [0; 32], name_len: 0, reserved: 0, base: 0, size: 0 };

    pub fn name(&self) -> &str {
        let len = (self.name_len as usize).min(self.name.len());
        core::str::from_utf8(&self.name[..len]).unwrap_or("")
    }
}

/// Maximum number of boot modules.
pub const MODULES_MAX: usize = 8;

/// Maximum kernel command line length in bytes.
pub const CMDLINE_MAX: usize = 256;
/// Maximum firmware vendor string length in bytes.
pub const VENDOR_MAX: usize = 64;

/// Everything the boot manager hands to the kernel.
#[repr(C)]
pub struct BootInfo {
    pub magic: u64,
    pub version: u32,
    /// `size_of::<BootInfo>()` as seen by the boot manager.
    pub size: u32,
    /// Virtual base of the direct map (always [`HHDM_BASE`] for now).
    pub hhdm_base: u64,
    pub kernel_phys_base: u64,
    pub kernel_virt_base: u64,
    pub kernel_size: u64,
    /// Physical address of a `[MemoryRegion; memory_map_len]` array, sorted
    /// by base address with adjacent regions of the same kind merged.
    pub memory_map: u64,
    pub memory_map_len: u64,
    pub framebuffer: FramebufferInfo,
    /// Non-zero when `framebuffer` is valid.
    pub has_framebuffer: u32,
    pub cmdline_len: u32,
    /// Physical address of the ACPI RSDP, or 0 when the firmware has none.
    pub rsdp: u64,
    /// Physical address of the UEFI system table (runtime services only).
    pub uefi_system_table: u64,
    /// Physical address of a page below 1 MiB reserved for starting
    /// application processors, or 0 if none could be reserved.
    pub ap_trampoline: u64,
    /// Virtual base and size of the boot stack the kernel was entered on.
    pub stack_base: u64,
    pub stack_size: u64,
    /// Firmware vendor string (UTF-8, not NUL terminated).
    pub firmware_vendor: [u8; VENDOR_MAX],
    pub firmware_vendor_len: u32,
    pub firmware_revision: u32,
    pub cmdline: [u8; CMDLINE_MAX],
    pub modules: [BootModule; MODULES_MAX],
    pub module_count: u32,
}

impl BootInfo {
    /// The kernel command line read from `\meridian\cmdline.txt`.
    pub fn cmdline(&self) -> &str {
        let len = (self.cmdline_len as usize).min(CMDLINE_MAX);
        core::str::from_utf8(&self.cmdline[..len]).unwrap_or("")
    }

    /// The firmware vendor reported by UEFI.
    pub fn firmware_vendor(&self) -> &str {
        let len = (self.firmware_vendor_len as usize).min(VENDOR_MAX);
        core::str::from_utf8(&self.firmware_vendor[..len]).unwrap_or("")
    }

    /// True when `option` appears as a whitespace-separated word on the
    /// command line, or as the key of a `key=value` word.
    pub fn has_option(&self, option: &str) -> bool {
        self.cmdline().split_ascii_whitespace().any(|w| w == option || w.split('=').next() == Some(option))
    }

    /// The boot modules that were loaded.
    pub fn modules(&self) -> &[BootModule] {
        &self.modules[..(self.module_count as usize).min(MODULES_MAX)]
    }

    /// Value of a `key=value` command line option.
    pub fn option_value(&self, key: &str) -> Option<&str> {
        self.cmdline().split_ascii_whitespace().find_map(|w| {
            let (k, v) = w.split_once('=')?;
            (k == key).then_some(v)
        })
    }
}
