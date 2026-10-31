//! Kernel debug shell: an interactive command line on the framebuffer
//! console (PS/2 keyboard) and COM1 at the same time. It exists so the
//! kernel can be inspected on real hardware before user mode is available;
//! the user-facing terminal will be a normal user-mode application.

use alloc::string::String;

use crate::arch::x86_64::{apic, idt};
use crate::drivers::{fbcon, ps2};
use crate::hal::{acpi, pci, power, serial};
use crate::ke::{self, clock, irql, pcr};
use crate::mm::{heap, pmm};

const PROMPT: &str = "\x1b[96mmeridian\x1b[0m> ";

struct Command {
    name: &'static str,
    help: &'static str,
    run: fn(&str),
}

const COMMANDS: &[Command] = &[
    Command { name: "help", help: "list commands", run: cmd_help },
    Command { name: "ver", help: "kernel version and boot information", run: cmd_ver },
    Command { name: "cpu", help: "processor identification and features", run: cmd_cpu },
    Command { name: "mem", help: "physical memory and kernel pool usage (mem map: firmware map)", run: cmd_mem },
    Command { name: "acpi", help: "ACPI tables, processors and interrupt controllers", run: cmd_acpi },
    Command { name: "pci", help: "PCI devices", run: cmd_pci },
    Command { name: "irq", help: "interrupt statistics", run: cmd_irq },
    Command { name: "threads", help: "kernel threads and scheduler statistics", run: cmd_threads },
    Command { name: "ps", help: "user-mode processes", run: cmd_ps },
    Command { name: "run", help: "run a boot module as a user process: run init [arg]", run: cmd_run },
    Command { name: "uptime", help: "time since boot", run: cmd_uptime },
    Command { name: "log", help: "replay the kernel log", run: cmd_log },
    Command { name: "clear", help: "clear the screen", run: cmd_clear },
    Command { name: "selftest", help: "run the kernel self tests", run: cmd_selftest },
    Command { name: "reboot", help: "restart the computer", run: cmd_reboot },
    Command { name: "shutdown", help: "power off (ACPI S5)", run: cmd_shutdown },
    Command { name: "crash", help: "stop the system on purpose (tests the stop screen)", run: cmd_crash },
];

/// Runs the shell forever on the calling processor.
pub fn run() -> ! {
    kprintln!("\nType \x1b[96mhelp\x1b[0m for a list of kernel shell commands.");
    let mut line = String::new();
    kprint!("{PROMPT}");
    loop {
        let mut idle = true;
        while let Some(ev) = ps2::read_event() {
            idle = false;
            if !ev.pressed {
                continue;
            }
            match ev.key {
                ps2::Key::Char(c) if ev.ctrl && (c == 'l' || c == 'L') => {
                    cmd_clear("");
                    kprint!("{PROMPT}{line}");
                }
                ps2::Key::Char(c) => input(&mut line, c),
                ps2::Key::Enter => input(&mut line, '\n'),
                ps2::Key::Backspace => input(&mut line, '\x08'),
                _ => {}
            }
        }
        while let Some(b) = serial::read_byte() {
            idle = false;
            match b {
                b'\r' | b'\n' => input(&mut line, '\n'),
                0x7F | 0x08 => input(&mut line, '\x08'),
                0x20..=0x7E => input(&mut line, b as char),
                _ => {}
            }
        }
        if idle {
            // Block until the keyboard or serial driver reports input.
            crate::drivers::input::AVAILABLE.wait(None);
        }
    }
}

fn input(line: &mut String, c: char) {
    match c {
        '\n' => {
            kprintln!();
            execute(line.trim());
            line.clear();
            kprint!("{PROMPT}");
        }
        '\x08' => {
            if line.pop().is_some() {
                kprint!("\x08 \x08");
            }
        }
        c if line.len() < 200 => {
            line.push(c);
            kprint!("{c}");
        }
        _ => {}
    }
}

fn execute(line: &str) {
    if line.is_empty() {
        return;
    }
    let (name, args) = line.split_once(' ').unwrap_or((line, ""));
    match COMMANDS.iter().find(|c| c.name == name) {
        Some(cmd) => (cmd.run)(args.trim()),
        None => kprintln!("unknown command '{name}' (try 'help')"),
    }
}

fn cmd_help(_: &str) {
    for c in COMMANDS {
        kprintln!("  \x1b[96m{:<10}\x1b[0m {}", c.name, c.help);
    }
}

fn cmd_ver(_: &str) {
    let info = crate::boot_info();
    kprintln!("Meridian OS kernel {} ({})", crate::VERSION, crate::GIT_HASH);
    kprintln!("  firmware:     {} (revision {:#x})", info.firmware_vendor(), info.firmware_revision);
    kprintln!("  command line: \"{}\"", info.cmdline());
    kprintln!("  kernel image: {} KiB at physical {:#x}", info.kernel_size / 1024, info.kernel_phys_base);
    match fbcon::resolution() {
        Some((w, h)) => kprintln!("  display:      {w}x{h} framebuffer"),
        None => kprintln!("  display:      none (serial console only)"),
    }
}

fn cmd_cpu(_: &str) {
    let f = crate::cpu_features();
    kprintln!("{} ({})", f.brand_str(), f.vendor_str());
    kprintln!("  family {:#x} model {:#x} stepping {}", f.family, f.model, f.stepping);
    kprintln!(
        "  online processors: {}   this processor: {} (APIC id {}, {})",
        pcr::online_count(),
        pcr::current().index,
        apic::id(),
        if apic::is_x2apic() { "x2APIC" } else { "xAPIC" }
    );
    let flags = [
        ("nx", f.nx),
        ("smep", f.smep),
        ("smap", f.smap),
        ("umip", f.umip),
        ("pge", f.pge),
        ("1g-pages", f.page_1g),
        ("x2apic", f.x2apic),
        ("invariant-tsc", f.tsc_invariant),
        ("sse4.2", f.sse4_2),
        ("avx", f.avx),
        ("avx2", f.avx2),
        ("xsave", f.xsave),
        ("rdrand", f.rdrand),
        ("hypervisor", f.hypervisor),
    ];
    kprint!("  features:");
    for (name, on) in flags {
        if on {
            kprint!(" {name}");
        }
    }
    kprintln!();
    kprintln!("  TSC {} MHz, APIC timer {} ticks/ms", clock::tsc_khz() / 1000, apic::timer_ticks_per_ms());
}

fn cmd_mem(args: &str) {
    let (free, total) = pmm::stats();
    let (in_use, slab_pages, large_pages) = heap::stats();
    kprintln!("Physical memory: {} MiB usable, {} MiB free", total >> 20, free >> 20);
    kprintln!(
        "Kernel pool: {} KiB in use ({} small-object pages, {} large pages)",
        in_use / 1024,
        slab_pages,
        large_pages
    );
    if args == "map" {
        kprintln!("Firmware memory map:");
        for r in pmm::memory_map(crate::boot_info()) {
            kprintln!(
                "  {:#014x}-{:#014x} {:>9} KiB  {}",
                r.base,
                r.base + r.length,
                r.length / 1024,
                meridian_bootinfo::memory_kind::name(r.kind)
            );
        }
    }
}

fn cmd_acpi(_: &str) {
    let Some(a) = acpi::get() else {
        kprintln!("ACPI is not available");
        return;
    };
    kprintln!("ACPI revision {} OEM \"{}\"", a.revision, core::str::from_utf8(&a.oem_id).unwrap_or("?"));
    kprint!("  tables:");
    for (sig, _) in &a.tables {
        kprint!(" {}", core::str::from_utf8(sig).unwrap_or("????"));
    }
    kprintln!();
    if let Some(m) = &a.madt {
        for c in &m.cpus {
            kprintln!(
                "  processor acpi-id {} apic-id {} {}",
                c.acpi_id,
                c.apic_id,
                if c.enabled { "enabled" } else { "disabled" }
            );
        }
        for io in &m.ioapics {
            kprintln!("  I/O APIC id {} at {:#x}, GSI base {}", io.id, io.address, io.gsi_base);
        }
        for o in &m.overrides {
            kprintln!("  override: ISA IRQ {} -> GSI {} (flags {:#x})", o.source, o.gsi, o.flags);
        }
    }
    if let Some(base) = a.hpet_base {
        kprintln!("  HPET at {base:#x}");
    }
    match a.s5 {
        Some((ta, tb)) => kprintln!("  soft-off (S5) sleep type: {ta}/{tb}"),
        None => kprintln!("  soft-off (S5): not described"),
    }
}

fn cmd_pci(_: &str) {
    for d in pci::devices() {
        kprintln!(
            "  {:02x}:{:02x}.{} {:04x}:{:04x} {:<16} {}",
            d.bus,
            d.device,
            d.function,
            d.vendor_id,
            d.device_id,
            d.vendor_name(),
            d.class_name()
        );
    }
    kprintln!("{} devices", pci::devices().len());
}

fn cmd_irq(_: &str) {
    for i in 0..pcr::online_count() as usize {
        if let Some(p) = pcr::by_index(i) {
            kprintln!(
                "  cpu{}: {} interrupts, {} clock ticks",
                i,
                p.interrupts.load(core::sync::atomic::Ordering::Relaxed),
                p.ticks.load(core::sync::atomic::Ordering::Relaxed)
            );
        }
    }
    kprintln!("  unexpected interrupts: {}", idt::unexpected_count());
    kprintln!("  current IRQL: {}", irql::name(irql::current()));
}

fn cmd_threads(_: &str) {
    kprintln!("  {:>4}  {:<24} {:>4}  {:<10} {:>4}  {:>10}", "id", "name", "prio", "state", "cpu", "cpu time");
    for t in ke::sched::threads() {
        let ticks = t.cpu_ticks.load(core::sync::atomic::Ordering::Relaxed);
        kprintln!(
            "  {:>4}  {:<24} {:>4}  {:<10} {:>4}  {:>7}.{:03}s",
            t.id,
            t.name(),
            t.priority(),
            t.state().name(),
            t.last_cpu(),
            ticks / 1000,
            ticks % 1000
        );
    }
    kprintln!("  context switches: {}", ke::sched::context_switches());
}

fn cmd_ps(_: &str) {
    let list = crate::ps::process::list();
    if list.is_empty() {
        kprintln!("no user processes are running");
    }
    for p in list {
        let state = match p.exit_status() {
            Some(s) => alloc::format!("exited ({:#x})", s as u32),
            None => String::from("running"),
        };
        kprintln!(
            "  {:>5}  {:<16} {:>8} KiB  {:>3} handles  {}",
            p.id,
            p.name(),
            p.space.committed() / 1024,
            p.handles.lock().open_count(),
            state
        );
    }
}

fn cmd_run(args: &str) {
    let mut words = args.split_whitespace();
    let Some(name) = words.next() else {
        kprintln!("usage: run <module> [argument]");
        return;
    };
    let arg = words.next().and_then(|a| a.parse::<u64>().ok()).unwrap_or(0);
    let file = if name.contains('.') { String::from(name) } else { alloc::format!("{name}.exe") };
    let Some(image) = crate::boot_module(&file) else {
        kprintln!("no boot module named {file}");
        return;
    };
    match crate::ps::process::spawn(name, image, arg) {
        Ok(p) => {
            kprintln!("started process {} ({})", p.id, name);
            if p.terminated.wait(Some(30_000)) == ke::sched::WaitStatus::Timeout {
                kprintln!("process {} is still running", p.id);
            } else {
                kprintln!("process {} exited with status {:#x}", p.id, p.exit_status().unwrap_or(0) as u32);
            }
        }
        Err(e) => kprintln!("cannot start {file}: {e}"),
    }
}

fn cmd_uptime(_: &str) {
    let us = clock::uptime_us();
    kprintln!("up {}.{:03} s ({} clock ticks)", us / 1_000_000, (us / 1000) % 1000, clock::ticks());
}

fn cmd_log(_: &str) {
    // Copy first: printing while holding the log lock would deadlock. The
    // copy is reserved up front because the lock is held at HIGH_LEVEL,
    // where the pool must not be used.
    let mut copy = alloc::vec::Vec::with_capacity(64 * 1024);
    crate::log::dump(|chunk| copy.extend_from_slice(chunk));
    kprint!("{}", String::from_utf8_lossy(&copy));
}

fn cmd_clear(_: &str) {
    kprint!("\x1b[2J");
}

fn cmd_selftest(_: &str) {
    crate::selftest::run_all();
}

fn cmd_reboot(_: &str) {
    kprintln!("Restarting...");
    power::reboot();
}

fn cmd_shutdown(_: &str) {
    kprintln!("Powering off...");
    if let Err(e) = power::shutdown() {
        kprintln!("shutdown failed: {e}");
    }
}

fn cmd_crash(_: &str) {
    crate::panic::bugcheck(
        crate::panic::stop::MANUALLY_INITIATED,
        format_args!("the 'crash' shell command was used"),
        None,
    );
}
