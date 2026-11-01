//! The Meridian kernel.
//!
//! Entered from the Meridian Boot Manager in 64-bit mode with paging on,
//! interrupts off, the boot page tables active and the boot stack loaded.
//! `kernel_entry` brings up the processor, memory manager, display, platform
//! firmware tables, interrupt controllers, timers and boot-critical devices,
//! then either runs the self tests or starts the kernel shell.
//!
//! See docs/ARCHITECTURE.md and docs/KERNEL_DESIGN.md.

#![no_std]
#![no_main]

extern crate alloc;

#[macro_use]
mod log;

mod arch;
mod drivers;
mod fs;
mod hal;
mod io;
mod ke;
mod mm;
mod ob;
mod panic;
mod ps;
mod selftest;
mod shell;
mod sync;

use core::ptr::null_mut;
use core::sync::atomic::{AtomicPtr, Ordering};

use arch::x86_64::idt::{self, TrapFrame};
use arch::x86_64::{apic, cpu, gdt, ioapic, pic, vectors};
use ke::{clock, pcr};
use meridian_bootinfo::{BOOTINFO_MAGIC, BOOTINFO_VERSION, BootInfo};
use sync::{Once, RacyCell};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const GIT_HASH: &str = env!("MERIDIAN_GIT_HASH");

static BOOT_INFO: AtomicPtr<BootInfo> = AtomicPtr::new(null_mut());
static FEATURES: Once<cpu::Features> = Once::new();

pub fn boot_info_opt() -> Option<&'static BootInfo> {
    let p = BOOT_INFO.load(Ordering::Acquire);
    // SAFETY: set once from the boot manager's pointer, which stays valid.
    (!p.is_null()).then(|| unsafe { &*p })
}

pub fn boot_info() -> &'static BootInfo {
    boot_info_opt().expect("boot information not available")
}

/// Contents of the boot module called `name` (e.g. `init.exe`).
pub fn boot_module(name: &str) -> Option<&'static [u8]> {
    let m = boot_info().modules().iter().find(|m| m.name().eq_ignore_ascii_case(name))?;
    // SAFETY: boot modules live in BOOT_MODULE memory, which is never reused.
    Some(unsafe { core::slice::from_raw_parts(mm::phys_to_virt(m.base) as *const u8, m.size as usize) })
}

pub fn cpu_features() -> &'static cpu::Features {
    FEATURES.get().expect("CPU features not detected yet")
}

/// Interrupt stacks for the bootstrap processor: double fault, NMI and
/// machine check each get their own so they work even on a corrupt stack.
#[repr(C, align(16))]
struct IstStack([u8; 16 * 1024]);
static BSP_IST_STACKS: RacyCell<[IstStack; 3]> = RacyCell::new([const { IstStack([0; 16 * 1024]) }; 3]);

#[unsafe(no_mangle)]
pub extern "C" fn kernel_entry(info: *const BootInfo) -> ! {
    clock::mark_boot();
    hal::serial::init();
    // SAFETY: the boot manager passes a pointer into the direct map.
    let info = unsafe { &*info };
    if info.magic != BOOTINFO_MAGIC || info.version != BOOTINFO_VERSION {
        panic::emergency_halt("boot information is missing or from an incompatible boot manager");
    }
    BOOT_INFO.store(info as *const BootInfo as *mut BootInfo, Ordering::Release);

    info!("Meridian OS kernel {} ({}) starting", VERSION, GIT_HASH);
    let features = FEATURES.call_once(cpu::detect);
    info!("cpu: {} [{}]", features.brand_str(), features.vendor_str());

    // --- Processor ---------------------------------------------------------
    // SAFETY: early single-threaded initialisation of the boot processor.
    unsafe { cpu::enable_protection_features(features) };
    init_bsp_descriptors();
    idt::init();
    idt::load();
    arch::x86_64::syscall::init_processor();

    // --- Memory ------------------------------------------------------------
    mm::init(info);
    // SAFETY: the boot identity map was removed by mm::init.
    unsafe { cpu::enable_global_pages(features) };
    mm::user::init(features.smap);
    let (free, total) = mm::pmm::stats();
    info!("mm: {} MiB of RAM, {} MiB free after boot", total >> 20, free >> 20);
    if info.has_option("mm.verbose") {
        for r in mm::pmm::memory_map(info) {
            info!(
                "mm: {:#014x}-{:#014x} {:>9} KiB {}",
                r.base,
                r.base + r.length,
                r.length / 1024,
                meridian_bootinfo::memory_kind::name(r.kind)
            );
        }
    }

    // --- Display -----------------------------------------------------------
    if drivers::fbcon::init(info) {
        log::attach_console();
        let f = &info.framebuffer;
        info!("display: {}x{} framebuffer at {:#x}", f.width, f.height, f.base);
    } else {
        info!("display: none; using the serial console only");
    }

    // --- Platform firmware and interrupt controllers -----------------------
    let acpi = hal::acpi::init(info.rsdp).map_err(|e| warn!("acpi: {e}")).ok();
    let madt = acpi.and_then(|a| a.madt.as_ref());
    if let Some(a) = acpi {
        info!(
            "acpi: revision {}, {} tables, {} processors, {} I/O APICs",
            a.revision,
            a.tables.len(),
            madt.map_or(0, |m| m.cpus.iter().filter(|c| c.enabled).count()),
            madt.map_or(0, |m| m.ioapics.len())
        );
    }
    if let Some(base) = acpi.and_then(|a| a.hpet_base) {
        match hal::hpet::init(base) {
            Ok(h) => info!("hpet: {}.{:03} MHz counter", h.frequency_hz() / 1_000_000, h.frequency_hz() / 1000 % 1000),
            Err(e) => warn!("hpet: {e}"),
        }
    }

    pic::disable();
    if let Err(e) = apic::init_bsp(madt.map(|m| m.lapic_base)) {
        panic!("local APIC initialisation failed: {e}");
    }
    pcr::current().apic_id.store(apic::id(), Ordering::Relaxed);
    match madt.map(ioapic::init) {
        Some(Ok(n)) => info!(
            "apic: local APIC id {} ({}), {} I/O APIC(s)",
            apic::id(),
            if apic::is_x2apic() { "x2APIC" } else { "xAPIC" },
            n
        ),
        Some(Err(e)) => warn!("ioapic: {e}"),
        None => warn!("no MADT: device interrupts are unavailable"),
    }

    // --- Time --------------------------------------------------------------
    let cal = hal::timer::calibrate().unwrap_or_else(|e| panic!("timer calibration failed: {e}"));
    clock::set_tsc_khz(cal.tsc_khz);
    apic::set_timer_ticks_per_ms(cal.apic_ticks_per_ms);
    info!(
        "timer: TSC {} MHz, APIC timer {} kHz (calibrated against the {})",
        cal.tsc_khz / 1000,
        cal.apic_ticks_per_ms,
        cal.reference
    );
    idt::register(vectors::CLOCK, clock_interrupt);
    idt::register(vectors::APIC_ERROR, apic_error_interrupt);
    apic::start_periodic_timer(clock::TICK_HZ as u32);
    // SAFETY: IDT, APIC and handlers are ready.
    unsafe { cpu::enable_interrupts() };

    // --- Buses and boot devices -------------------------------------------
    let devices = hal::pci::enumerate();
    info!("pci: {} devices", devices.len());
    for d in devices {
        info!(
            "pci: {:02x}:{:02x}.{} {:04x}:{:04x} {} {}",
            d.bus,
            d.device,
            d.function,
            d.vendor_id,
            d.device_id,
            d.vendor_name(),
            d.class_name()
        );
    }
    match drivers::ps2::init() {
        Ok(()) => info!("ps2: keyboard ready"),
        Err(e) => warn!("ps2: {e}"),
    }
    if hal::serial::enable_receive_interrupt() {
        info!("serial: COM1 receive interrupt enabled");
    }

    // --- Threads and processors -------------------------------------------
    // From here on this code is the bootstrap processor's idle thread.
    ke::sched::init();
    ke::ipi::init();
    if let Some(m) = madt {
        let started = arch::x86_64::smp::start_application_processors(info.ap_trampoline, m);
        info!("smp: {} of {} processors online", started + 1, m.cpus.iter().filter(|c| c.enabled).count());
    }
    info!("boot: kernel initialised in {} ms", clock::uptime_ms());

    let selftest = info.has_option("selftest");
    let started = ke::sched::spawn("shell", ke::thread::priority::NORMAL, move || {
        fs::init_storage();
        if !selftest {
            start_init();
        }
        if selftest {
            let (_, failed) = selftest::run_all();
            let code = if failed == 0 { hal::power::QEMU_EXIT_SUCCESS } else { hal::power::QEMU_EXIT_FAILURE };
            hal::power::qemu_exit(code);
            // Still running: not under the test harness. Continue to the shell.
        }
        shell::run()
    });
    if let Err(e) = started {
        panic!("cannot start the shell thread: {e}");
    }
    ke::sched::idle_loop()
}

/// Runs the first user-mode program and reports how it ended. It is read
/// from the boot disk when storage works, otherwise taken from the copy the
/// boot manager loaded.
fn start_init() {
    const PATH: &str = "C:\\MERIDIAN\\INIT.EXE";
    let from_disk = fs::read_file(PATH);
    let image: &[u8] = match &from_disk {
        Ok(data) => {
            info!("init: loaded {PATH} from disk ({} KiB)", data.len().div_ceil(1024));
            data
        }
        Err(e) => match boot_module("init.exe") {
            Some(module) => {
                info!("init: {PATH} not readable ({}); using the boot module", e.message());
                module
            }
            None => {
                warn!("init: no init program found; no user-mode programs will run");
                return;
            }
        },
    };
    match ps::process::spawn("init", image, 0) {
        Ok(p) => {
            info!("init: started process {} at image base {:#x}", p.id, p.image_base());
            p.terminated.wait(Some(10_000));
            match p.exit_status() {
                Some(0) => info!("init: exited normally"),
                Some(s) => warn!("init: exited with status {:#x}", s as u32),
                None => warn!("init: still running after 10 s"),
            }
        }
        Err(e) => warn!("init: cannot start: {e}"),
    }
}

/// Loads the bootstrap processor's GDT/TSS (with interrupt stacks) and
/// installs its processor control region.
fn init_bsp_descriptors() {
    let pcr = pcr::bsp();
    // SAFETY: runs once, before interrupts are enabled, on the BSP.
    unsafe {
        let stacks = &*BSP_IST_STACKS.get();
        let tss = (*pcr).tss.get();
        let mut ist = (*tss).ist;
        for (slot, stack) in stacks.iter().enumerate() {
            ist[slot] = stack.0.as_ptr_range().end as u64;
        }
        (*tss).ist = ist;
        gdt::load((*pcr).gdt.get(), tss);
        pcr::install(pcr);
    }
}

fn clock_interrupt(_: &mut TrapFrame) {
    let p = pcr::current();
    p.ticks.fetch_add(1, Ordering::Relaxed);
    if p.index == 0 {
        clock::tick();
    }
    ke::sched::clock_tick(p);
}

fn apic_error_interrupt(_: &mut TrapFrame) {
    warn!("local APIC error, status {:#x}", apic::error_status());
}
