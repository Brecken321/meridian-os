//! Meridian Boot Manager.
//!
//! A UEFI application (`\EFI\BOOT\BOOTX64.EFI`) that:
//!   1. reads `\meridian\kernel.elf` and the optional `\meridian\cmdline.txt`
//!      from the volume it was loaded from,
//!   2. selects a graphics mode (GOP),
//!   3. loads the kernel's ELF segments and builds 4-level page tables
//!      (higher-half direct map, kernel image, boot stack),
//!   4. exits boot services and jumps to the kernel with a [`BootInfo`].
//!
//! See docs/KERNEL_DESIGN.md for the memory layout contract.

#![no_std]
#![no_main]

mod elf;
mod paging;
mod uefi;

use core::convert::Infallible;
use core::ffi::c_void;
use core::fmt::{self, Write};
use core::mem::size_of;
use core::ptr;
use core::sync::atomic::{AtomicPtr, Ordering};

use meridian_bootinfo::{self as bi, BootInfo, FramebufferInfo, MemoryRegion, memory_kind, pixel_format};
use uefi::*;

/// OS-loader-defined UEFI memory types (0x8000_0000 and up are reserved for
/// OS loaders by the UEFI spec). They let the kernel tell our allocations
/// apart from ordinary loader data in the final memory map.
const MT_KERNEL_IMAGE: u32 = 0x8000_0001;
const MT_KERNEL_STACK: u32 = 0x8000_0002;
const MT_PAGE_TABLES: u32 = 0x8000_0003;
const MT_BOOT_INFO: u32 = 0x8000_0004;
const MT_AP_TRAMPOLINE: u32 = 0x8000_0005;

const BOOT_INFO_PAGES: usize = 8;
const FOUR_GIB: u64 = 4 << 30;

static SYSTEM_TABLE: AtomicPtr<SystemTable> = AtomicPtr::new(ptr::null_mut());

// ---------------------------------------------------------------------------
// Console output: UEFI text console before ExitBootServices, COM1 after.
// ---------------------------------------------------------------------------

struct Console;

impl Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let st = SYSTEM_TABLE.load(Ordering::Relaxed);
        if st.is_null() {
            serial_write(s);
            return Ok(());
        }
        // SAFETY: the system table is valid until ExitBootServices, after
        // which SYSTEM_TABLE is cleared.
        let out = unsafe { (*st).con_out };
        let mut buf = [0u16; 130];
        let mut n = 0;
        let flush = |buf: &mut [u16; 130], n: &mut usize| {
            buf[*n] = 0;
            // SAFETY: `buf` is NUL terminated UCS-2.
            unsafe { ((*out).output_string)(out, buf.as_ptr()) };
            *n = 0;
        };
        for c in s.chars() {
            if c == '\n' {
                buf[n] = '\r' as u16;
                n += 1;
            }
            let mut tmp = [0u16; 2];
            for unit in c.encode_utf16(&mut tmp) {
                buf[n] = *unit;
                n += 1;
            }
            if n >= 126 {
                flush(&mut buf, &mut n);
            }
        }
        if n > 0 {
            flush(&mut buf, &mut n);
        }
        Ok(())
    }
}

macro_rules! println {
    ($($arg:tt)*) => {{
        let _ = writeln!(Console, $($arg)*);
    }};
}

const COM1: u16 = 0x3F8;

fn serial_write(s: &str) {
    for b in s.bytes() {
        if b == b'\n' {
            serial_byte(b'\r');
        }
        serial_byte(b);
    }
}

fn serial_byte(b: u8) {
    // SAFETY: COM1 port I/O; the firmware already initialised the UART.
    unsafe {
        for _ in 0..100_000 {
            if inb(COM1 + 5) & 0x20 != 0 {
                break;
            }
        }
        outb(COM1, b);
    }
}

unsafe fn outb(port: u16, value: u8) {
    unsafe { core::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags)) };
}

unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    unsafe { core::arch::asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack, preserves_flags)) };
    value
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

struct BootError {
    what: &'static str,
    status: Status,
}

impl fmt::Display for BootError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{} (EFI status {:#x})", self.what, self.status)
    }
}

fn check(status: Status, what: &'static str) -> Result<(), BootError> {
    if is_error(status) { Err(BootError { what, status }) } else { Ok(()) }
}

fn fail<T>(what: &'static str) -> Result<T, BootError> {
    Err(BootError { what, status: LOAD_ERROR })
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
extern "efiapi" fn efi_main(image: Handle, st: *mut SystemTable) -> Status {
    SYSTEM_TABLE.store(st, Ordering::Relaxed);
    // SAFETY: called by firmware with a valid image handle and system table.
    match unsafe { boot(image, st) } {
        Ok(never) => match never {},
        Err(e) => {
            println!("\nMeridian Boot Manager could not start the kernel:\n  {e}");
            println!("Returning to the firmware in 10 seconds.");
            // SAFETY: boot services are still active on every error path.
            unsafe { ((*(*st).boot_services).stall)(10_000_000) };
            e.status
        }
    }
}

unsafe fn boot(image: Handle, st: *mut SystemTable) -> Result<Infallible, BootError> {
    // SAFETY: firmware guarantees these pointers while boot services run.
    let bs = unsafe { &*(*st).boot_services };
    unsafe {
        (bs.set_watchdog_timer)(0, 0, 0, ptr::null());
        ((*(*st).con_out).clear_screen)((*st).con_out);
    }
    println!("Meridian Boot Manager {}", env!("CARGO_PKG_VERSION"));

    check_cpu()?;

    let root = unsafe { open_boot_volume(bs, image)? };
    let Some(kernel_file) = (unsafe { read_file(bs, root, "\\meridian\\kernel.elf")? }) else {
        return fail("\\meridian\\kernel.elf was not found on the boot volume");
    };
    let cmdline_file = unsafe { read_file(bs, root, "\\meridian\\cmdline.txt")? };

    let mut cmdline = [0u8; bi::CMDLINE_MAX];
    let mut cmdline_len = 0;
    if let Some(raw) = cmdline_file {
        let text = core::str::from_utf8(raw).unwrap_or("").trim();
        let n = text.len().min(bi::CMDLINE_MAX);
        cmdline[..n].copy_from_slice(&text.as_bytes()[..n]);
        cmdline_len = n;
    }
    let cmdline_str = core::str::from_utf8(&cmdline[..cmdline_len]).unwrap_or("");
    println!("Kernel command line: \"{cmdline_str}\"");

    let framebuffer = unsafe { setup_graphics(bs, cmdline_str) };

    // --- Load the kernel image -------------------------------------------
    let kernel = elf::Elf::parse(kernel_file).map_err(|what| BootError { what, status: LOAD_ERROR })?;
    let Some((lo, hi)) = kernel.span() else {
        return fail("kernel ELF has no loadable segments");
    };
    if lo < bi::KERNEL_BASE || lo % paging::PAGE_4K != 0 {
        return fail("kernel ELF is not linked at the kernel base address");
    }
    let image_size = (hi - lo).next_multiple_of(paging::PAGE_4K);
    let kernel_phys = allocate_pages(bs, MT_KERNEL_IMAGE, (image_size / paging::PAGE_4K) as usize)?;
    for seg in kernel.segments() {
        let dst = (kernel_phys + (seg.vaddr - lo)) as *mut u8;
        // SAFETY: the destination lies inside the zeroed image allocation.
        unsafe { ptr::copy_nonoverlapping(seg.file.as_ptr(), dst, seg.file.len()) };
    }
    println!("Loaded kernel: {} KiB at physical {:#x}, entry {:#x}", image_size / 1024, kernel_phys, kernel.entry);

    // --- Other allocations the kernel needs -------------------------------
    let rsdp = unsafe { find_rsdp(st) };
    let trampoline = allocate_below(bs, MT_AP_TRAMPOLINE, 1, 0x9_FFFF).unwrap_or(0);
    let stack_pages = (bi::BOOT_STACK_SIZE / paging::PAGE_4K) as usize;
    let stack_phys = allocate_pages(bs, MT_KERNEL_STACK, stack_pages)?;
    let info_phys = allocate_pages(bs, MT_BOOT_INFO, BOOT_INFO_PAGES)?;

    // --- Page tables ------------------------------------------------------
    let max_phys = unsafe { highest_physical_address(bs)? }.max(FOUR_GIB).next_multiple_of(paging::PAGE_2M);
    let mut pt = paging::PageTables::new(|| allocate_pages(bs, MT_PAGE_TABLES, 1).ok())
        .ok_or(BootError { what: "out of memory for page tables", status: OUT_OF_RESOURCES })?;
    let oom = || BootError { what: "out of memory for page tables", status: OUT_OF_RESOURCES };

    use paging::{GLOBAL, NO_EXECUTE, WRITABLE};
    // Direct map of all physical memory (read/write, never executable).
    pt.map_range_2m(bi::HHDM_BASE, 0, max_phys, WRITABLE | NO_EXECUTE | GLOBAL).ok_or_else(oom)?;
    // Temporary identity map so this code keeps running after the CR3
    // switch. The kernel removes it during memory manager initialisation.
    pt.map_range_2m(0, 0, max_phys, WRITABLE).ok_or_else(oom)?;
    if let Some(fb) = &framebuffer {
        if fb.base + fb.size > max_phys {
            let start = fb.base & !(paging::PAGE_2M - 1);
            let len = fb.base + fb.size - start;
            pt.map_range_2m(bi::HHDM_BASE + start, start, len, WRITABLE | NO_EXECUTE).ok_or_else(oom)?;
        }
    }
    for seg in kernel.segments() {
        let first = seg.vaddr & !(paging::PAGE_4K - 1);
        let end = seg.vaddr + seg.memsz;
        let mut flags = GLOBAL;
        if seg.writable {
            flags |= WRITABLE;
        }
        if !seg.executable {
            flags |= NO_EXECUTE;
        }
        let mut va = first;
        while va < end {
            pt.map_4k(va, kernel_phys + (va - lo), flags).ok_or_else(oom)?;
            va += paging::PAGE_4K;
        }
    }
    for i in 0..stack_pages as u64 {
        let off = i * paging::PAGE_4K;
        pt.map_4k(bi::BOOT_STACK_BASE + off, stack_phys + off, WRITABLE | NO_EXECUTE | GLOBAL).ok_or_else(oom)?;
    }

    // --- Boot information -------------------------------------------------
    // SAFETY: `info_phys` is a fresh, zeroed, identity-mapped allocation.
    let info = unsafe { &mut *(info_phys as *mut BootInfo) };
    let regions_offset = size_of::<BootInfo>().next_multiple_of(64);
    let regions_ptr = (info_phys as usize + regions_offset) as *mut MemoryRegion;
    let regions_cap = (BOOT_INFO_PAGES * 4096 - regions_offset) / size_of::<MemoryRegion>();

    info.magic = bi::BOOTINFO_MAGIC;
    info.version = bi::BOOTINFO_VERSION;
    info.size = size_of::<BootInfo>() as u32;
    info.hhdm_base = bi::HHDM_BASE;
    info.kernel_phys_base = kernel_phys;
    info.kernel_virt_base = lo;
    info.kernel_size = image_size;
    info.memory_map = regions_ptr as u64;
    if let Some(fb) = framebuffer {
        info.framebuffer = fb;
        info.has_framebuffer = 1;
    }
    info.cmdline = cmdline;
    info.cmdline_len = cmdline_len as u32;
    info.rsdp = rsdp;
    info.uefi_system_table = st as u64;
    info.ap_trampoline = trampoline;
    info.stack_base = bi::BOOT_STACK_BASE;
    info.stack_size = bi::BOOT_STACK_SIZE;
    unsafe {
        info.firmware_revision = (*st).firmware_revision;
        let mut vendor = (*st).firmware_vendor;
        let mut n = 0;
        while !vendor.is_null() && *vendor != 0 && n < bi::VENDOR_MAX {
            let c = *vendor;
            info.firmware_vendor[n] = if c < 0x80 { c as u8 } else { b'?' };
            n += 1;
            vendor = vendor.add(1);
        }
        info.firmware_vendor_len = n as u32;
    }

    println!("Starting the Meridian kernel...");

    // --- Exit boot services ---------------------------------------------------
    let (map, map_size, desc_size) = unsafe { exit_boot_services(bs, image)? };
    // Boot services are gone: route output to COM1 from now on.
    SYSTEM_TABLE.store(ptr::null_mut(), Ordering::Relaxed);

    // SAFETY: the region array lives in our boot-info allocation.
    let regions = unsafe { core::slice::from_raw_parts_mut(regions_ptr, regions_cap) };
    let count = unsafe { convert_memory_map(map, map_size, desc_size, regions) };
    info.memory_map_len = count as u64;

    let entry = kernel.entry;
    let stack_top = bi::BOOT_STACK_BASE + bi::BOOT_STACK_SIZE;
    let info_virt = bi::HHDM_BASE + info_phys;
    let pml4 = pt.pml4;
    let _ = writeln!(Console, "[boot] entering kernel at {entry:#x}");

    // SAFETY: the new page tables map this code (identity), the kernel image,
    // its stack and the boot info. EFER.NXE must be set before CR3 is loaded
    // because the tables use the NX bit.
    unsafe {
        let efer = rdmsr(0xC000_0080);
        wrmsr(0xC000_0080, efer | (1 << 11));
        core::arch::asm!(
            "cli",
            "mov cr3, {pml4}",
            "mov rsp, {stack}",
            "xor ebp, ebp",
            "push 0",
            "jmp {entry}",
            pml4 = in(reg) pml4,
            stack = in(reg) stack_top,
            entry = in(reg) entry,
            in("rdi") info_virt,
            options(noreturn),
        );
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn cpuid(leaf: u32) -> (u32, u32, u32, u32) {
    let (a, b, c, d): (u32, u64, u32, u32);
    // SAFETY: CPUID is always available on x86-64. RBX is reserved by LLVM,
    // so it is saved and restored around the instruction.
    unsafe {
        core::arch::asm!(
            "mov {tmp}, rbx",
            "cpuid",
            "xchg {tmp}, rbx",
            tmp = out(reg) b,
            inout("eax") leaf => a,
            inout("ecx") 0 => c,
            out("edx") d,
            options(nostack, preserves_flags),
        );
    }
    (a, b as u32, c, d)
}

unsafe fn rdmsr(msr: u32) -> u64 {
    let (lo, hi): (u32, u32);
    unsafe {
        core::arch::asm!("rdmsr", in("ecx") msr, out("eax") lo, out("edx") hi, options(nomem, nostack, preserves_flags))
    };
    (hi as u64) << 32 | lo as u64
}

unsafe fn wrmsr(msr: u32, value: u64) {
    unsafe {
        core::arch::asm!("wrmsr", in("ecx") msr, in("eax") value as u32, in("edx") (value >> 32) as u32, options(nomem, nostack, preserves_flags))
    };
}

fn check_cpu() -> Result<(), BootError> {
    let (max_ext, ..) = cpuid(0x8000_0000);
    if max_ext < 0x8000_0001 {
        return fail("this processor does not report extended CPUID leaves");
    }
    let (_, _, _, edx) = cpuid(0x8000_0001);
    if edx & (1 << 20) == 0 {
        return fail("this processor lacks no-execute (NX) page protection, which Meridian requires");
    }
    let cr4: u64;
    // SAFETY: reading CR4 has no side effects.
    unsafe { core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack, preserves_flags)) };
    if cr4 & (1 << 12) != 0 {
        return fail("the firmware enabled 5-level paging, which Meridian does not support yet");
    }
    Ok(())
}

fn allocate_pages(bs: &BootServices, mem_type: u32, pages: usize) -> Result<u64, BootError> {
    let mut addr = 0u64;
    // SAFETY: plain boot-services call with valid out pointer.
    check(unsafe { (bs.allocate_pages)(ALLOCATE_ANY_PAGES, mem_type, pages, &mut addr) }, "out of memory")?;
    // SAFETY: freshly allocated, identity mapped.
    unsafe { ptr::write_bytes(addr as *mut u8, 0, pages * 4096) };
    Ok(addr)
}

fn allocate_below(bs: &BootServices, mem_type: u32, pages: usize, max: u64) -> Option<u64> {
    let mut addr = max;
    // SAFETY: as above.
    let status = unsafe { (bs.allocate_pages)(ALLOCATE_MAX_ADDRESS, mem_type, pages, &mut addr) };
    if is_error(status) {
        return None;
    }
    unsafe { ptr::write_bytes(addr as *mut u8, 0, pages * 4096) };
    Some(addr)
}

fn allocate_pool(bs: &BootServices, size: usize) -> Result<*mut u8, BootError> {
    let mut buf = ptr::null_mut();
    check(unsafe { (bs.allocate_pool)(LOADER_DATA, size, &mut buf) }, "out of memory")?;
    Ok(buf)
}

unsafe fn open_boot_volume(bs: &BootServices, image: Handle) -> Result<*mut File, BootError> {
    unsafe {
        let mut loaded: *mut c_void = ptr::null_mut();
        check((bs.handle_protocol)(image, &LOADED_IMAGE_GUID, &mut loaded), "cannot query the loaded image")?;
        let device = (*(loaded as *mut LoadedImage)).device_handle;
        let mut sfs: *mut c_void = ptr::null_mut();
        check(
            (bs.handle_protocol)(device, &SIMPLE_FILE_SYSTEM_GUID, &mut sfs),
            "the boot volume has no readable file system",
        )?;
        let sfs = sfs as *mut SimpleFileSystem;
        let mut root = ptr::null_mut();
        check(((*sfs).open_volume)(sfs, &mut root), "cannot open the boot volume")?;
        Ok(root)
    }
}

/// Reads a whole file into pool memory. Returns `Ok(None)` if it does not exist.
unsafe fn read_file(bs: &BootServices, root: *mut File, path: &str) -> Result<Option<&'static [u8]>, BootError> {
    let mut name = [0u16; 128];
    for (i, c) in path.encode_utf16().take(127).enumerate() {
        name[i] = c;
    }
    unsafe {
        let mut file = ptr::null_mut();
        let status = ((*root).open)(root, &mut file, name.as_ptr(), FILE_MODE_READ, 0);
        if status == NOT_FOUND {
            return Ok(None);
        }
        check(status, "cannot open a boot file")?;

        // EFI_FILE_INFO: Size, FileSize, PhysicalSize, 3x EFI_TIME, Attribute, FileName.
        let mut info = [0u64; 128];
        let mut info_size = size_of::<[u64; 128]>();
        check(
            ((*file).get_info)(file, &FILE_INFO_GUID, &mut info_size, info.as_mut_ptr() as *mut u8),
            "cannot query a boot file's size",
        )?;
        let size = info[1] as usize;
        let buf = allocate_pool(bs, size.max(1))?;
        let mut read = size;
        check(((*file).read)(file, &mut read, buf), "cannot read a boot file")?;
        ((*file).close)(file);
        if read != size {
            return fail("short read from a boot file");
        }
        Ok(Some(core::slice::from_raw_parts(buf, size)))
    }
}

/// Parses `video=WIDTHxHEIGHT` from the command line.
fn requested_resolution(cmdline: &str) -> Option<(u32, u32)> {
    let value = cmdline.split_ascii_whitespace().find_map(|w| w.strip_prefix("video="))?;
    let (w, h) = value.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

unsafe fn setup_graphics(bs: &BootServices, cmdline: &str) -> Option<FramebufferInfo> {
    unsafe {
        let mut gop: *mut c_void = ptr::null_mut();
        if is_error((bs.locate_protocol)(&GRAPHICS_OUTPUT_GUID, ptr::null_mut(), &mut gop)) {
            println!("No UEFI graphics output; continuing with serial console only.");
            return None;
        }
        let gop = gop as *mut GraphicsOutput;
        if let Some((want_w, want_h)) = requested_resolution(cmdline) {
            let max_mode = (*(*gop).mode).max_mode;
            let mut found = false;
            for mode in 0..max_mode {
                let mut size = 0;
                let mut info = ptr::null_mut();
                if is_error(((*gop).query_mode)(gop, mode, &mut size, &mut info)) {
                    continue;
                }
                let (w, h, format) = ((*info).horizontal_resolution, (*info).vertical_resolution, (*info).pixel_format);
                (bs.free_pool)(info as *mut u8);
                let usable = matches!(format, PIXEL_RGB_RESERVED_8BIT | PIXEL_BGR_RESERVED_8BIT);
                if usable && w == want_w && h == want_h {
                    found = !is_error(((*gop).set_mode)(gop, mode));
                    break;
                }
            }
            if !found {
                println!("Resolution {want_w}x{want_h} is not available; keeping the current mode.");
            }
        }
        let mode = &*(*gop).mode;
        let info = &*mode.info;
        let format = match info.pixel_format {
            PIXEL_RGB_RESERVED_8BIT => pixel_format::RGBX,
            PIXEL_BGR_RESERVED_8BIT => pixel_format::BGRX,
            _ => {
                println!("The display uses an unsupported pixel format; serial console only.");
                return None;
            }
        };
        println!(
            "Display: {}x{} (stride {}) framebuffer at {:#x}",
            info.horizontal_resolution, info.vertical_resolution, info.pixels_per_scan_line, mode.frame_buffer_base
        );
        Some(FramebufferInfo {
            base: mode.frame_buffer_base,
            size: mode.frame_buffer_size as u64,
            width: info.horizontal_resolution,
            height: info.vertical_resolution,
            stride: info.pixels_per_scan_line,
            format,
        })
    }
}

unsafe fn find_rsdp(st: *mut SystemTable) -> u64 {
    unsafe {
        let tables = core::slice::from_raw_parts((*st).configuration_table, (*st).number_of_table_entries);
        let find = |guid: Guid| tables.iter().find(|t| t.vendor_guid == guid).map(|t| t.vendor_table as u64);
        find(ACPI_20_TABLE_GUID).or_else(|| find(ACPI_10_TABLE_GUID)).unwrap_or(0)
    }
}

/// Returns the end of the highest RAM range in the firmware memory map.
/// Reserved and MMIO ranges are ignored: firmware may report huge reserved
/// holes (QEMU reports one just below 1 TiB) that would otherwise inflate the
/// direct map. Device memory below 4 GiB is covered anyway.
unsafe fn highest_physical_address(bs: &BootServices) -> Result<u64, BootError> {
    unsafe {
        let (mut size, mut key, mut desc_size, mut version) = (0usize, 0usize, 0usize, 0u32);
        (bs.get_memory_map)(&mut size, ptr::null_mut(), &mut key, &mut desc_size, &mut version);
        size += 8 * desc_size.max(48);
        let buf = allocate_pool(bs, size)?;
        check(
            (bs.get_memory_map)(&mut size, buf, &mut key, &mut desc_size, &mut version),
            "cannot read the memory map",
        )?;
        let mut max = 0;
        for i in 0..size / desc_size {
            let d = ptr::read_unaligned(buf.add(i * desc_size) as *const MemoryDescriptor);
            let is_ram = matches!(
                d.ty,
                memory_type::LOADER_CODE
                    | memory_type::LOADER_DATA
                    | memory_type::BOOT_SERVICES_CODE
                    | memory_type::BOOT_SERVICES_DATA
                    | memory_type::RUNTIME_SERVICES_CODE
                    | memory_type::RUNTIME_SERVICES_DATA
                    | memory_type::CONVENTIONAL
                    | memory_type::ACPI_RECLAIM
                    | memory_type::ACPI_NVS
                    | memory_type::PERSISTENT
            );
            if is_ram {
                max = max.max(d.physical_start + d.number_of_pages * 4096);
            }
        }
        (bs.free_pool)(buf);
        Ok(max)
    }
}

/// Fetches the final memory map and exits boot services. Nothing may allocate
/// memory (including printing) between these two calls.
unsafe fn exit_boot_services(bs: &BootServices, image: Handle) -> Result<(*const u8, usize, usize), BootError> {
    unsafe {
        let (mut size, mut key, mut desc_size, mut version) = (0usize, 0usize, 0usize, 0u32);
        (bs.get_memory_map)(&mut size, ptr::null_mut(), &mut key, &mut desc_size, &mut version);
        let capacity = size + 16 * desc_size.max(48) + 4096;
        let buf = allocate_pool(bs, capacity)?;
        for _ in 0..4 {
            size = capacity;
            check(
                (bs.get_memory_map)(&mut size, buf, &mut key, &mut desc_size, &mut version),
                "cannot read the memory map",
            )?;
            if (bs.exit_boot_services)(image, key) == SUCCESS {
                return Ok((buf, size, desc_size));
            }
        }
        fail("ExitBootServices kept failing")
    }
}

fn region_kind(efi_type: u32) -> u32 {
    use memory_type as mt;
    match efi_type {
        mt::LOADER_CODE | mt::LOADER_DATA | mt::BOOT_SERVICES_CODE | mt::BOOT_SERVICES_DATA | mt::CONVENTIONAL => {
            memory_kind::USABLE
        }
        mt::RUNTIME_SERVICES_CODE | mt::RUNTIME_SERVICES_DATA => memory_kind::FIRMWARE_RUNTIME,
        mt::ACPI_RECLAIM => memory_kind::ACPI_RECLAIMABLE,
        mt::ACPI_NVS => memory_kind::ACPI_NVS,
        mt::UNUSABLE => memory_kind::UNUSABLE,
        mt::MMIO | mt::MMIO_PORT_SPACE => memory_kind::MMIO,
        MT_KERNEL_IMAGE => memory_kind::KERNEL_IMAGE,
        MT_KERNEL_STACK => memory_kind::KERNEL_STACK,
        MT_PAGE_TABLES => memory_kind::PAGE_TABLES,
        MT_BOOT_INFO => memory_kind::BOOT_INFO,
        MT_AP_TRAMPOLINE => memory_kind::AP_TRAMPOLINE,
        _ => memory_kind::RESERVED,
    }
}

/// Converts the UEFI memory map into sorted, merged [`MemoryRegion`]s.
/// Runs after ExitBootServices, so it must not allocate.
unsafe fn convert_memory_map(map: *const u8, size: usize, desc_size: usize, out: &mut [MemoryRegion]) -> usize {
    let mut n = 0;
    for i in 0..size / desc_size {
        // SAFETY: `map` holds `size` bytes of descriptors from GetMemoryMap.
        let d = unsafe { ptr::read_unaligned(map.add(i * desc_size) as *const MemoryDescriptor) };
        if d.number_of_pages == 0 || n == out.len() {
            continue;
        }
        out[n] = MemoryRegion {
            base: d.physical_start,
            length: d.number_of_pages * 4096,
            kind: region_kind(d.ty),
            reserved: 0,
        };
        n += 1;
    }
    // Insertion sort: n is small and nothing may allocate here.
    for i in 1..n {
        let mut j = i;
        while j > 0 && out[j - 1].base > out[j].base {
            out.swap(j - 1, j);
            j -= 1;
        }
    }
    let mut merged = 0;
    for i in 0..n {
        let r = out[i];
        if merged > 0 {
            let last = &mut out[merged - 1];
            if last.kind == r.kind && last.base + last.length == r.base {
                last.length += r.length;
                continue;
            }
        }
        out[merged] = r;
        merged += 1;
    }
    merged
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    let _ = writeln!(Console, "\nMeridian Boot Manager panic: {info}");
    loop {
        // SAFETY: halting is always safe.
        unsafe { core::arch::asm!("cli; hlt", options(nomem, nostack)) };
    }
}
