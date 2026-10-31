# Status

Last updated: 2026-10-31. Everything listed under "Working" is exercised by
the self tests or was verified manually in QEMU as noted.

## Working (tested in QEMU 11.1 / edk2, q35, TCG, 4 vCPUs)

* UEFI boot manager: loads the kernel ELF, sets the GOP mode from
  `video=WxH`, builds page tables, reserves the AP trampoline page, passes
  the memory map / RSDP / framebuffer / command line.
* Kernel: higher-half image with per-segment W^X permissions, CR0.WP, NX,
  SMEP, SMAP, UMIP; boot identity map removed after start-up.
* Physical frame allocator, kernel page tables, kernel virtual area, pool
  allocator (size classes + large blocks).
* GDT/TSS with IST stacks, IDT for all 256 vectors, exception reporting,
  recoverable memory probes, stop screen with stack trace.
* ACPI: RSDP/RSDT/XSDT, MADT, FADT, HPET, MCFG, `\_S5` extraction.
* Interrupts: legacy PICs masked, local APIC (xAPIC and x2APIC code paths;
  only xAPIC exercised so far), I/O APIC routing with ISA overrides, IRQL via CR8.
* Time: HPET (PIT fallback, untested) calibration of TSC and APIC timer;
  1 kHz clock.
* PCI enumeration (legacy configuration mechanism).
* PS/2 keyboard (US layout), COM1 serial console, framebuffer console with
  ANSI colours.
* Kernel threads with guard-paged stacks; 32-level priority scheduler with
  O(1) selection, 10 ms time slices, immediate preemption by higher-priority
  threads through the DISPATCH software interrupt; idle thread; thread exit
  and stack reclamation.
* DPCs (interrupt handlers defer work to DISPATCH_LEVEL).
* Dispatcher objects: auto- and manual-reset events, counting semaphores,
  recursive mutexes; waits with timeouts; sleep.
* Interrupt-driven console input: PS/2 keyboard and COM1 receive interrupts
  wake the shell thread through a DPC and an event.
* Kernel shell: help, ver, cpu, mem, acpi, pci, irq, threads, uptime, log,
  clear, selftest, reboot, shutdown, crash.
* Build: reproducible GPT/FAT32 image writer, QEMU runner, self-test harness,
  screenshot capture, GitHub Actions CI.

## Not implemented yet

SMP, system calls, user mode, processes, object manager,
any storage driver, any filesystem, USB, networking, audio, GPU acceleration,
desktop, Windows compatibility, installer. See [ROADMAP.md](ROADMAP.md).

## Known limitations

* Only one processor is used; application processors stay parked. The
  scheduler already uses per-processor state but has one global ready queue
  and dispatcher lock.
* Kernel unmaps do not yet shoot down other processors' TLBs (required
  before SMP is enabled).
* No wait-for-multiple-objects, APCs or priority boosting yet.
* The direct map uses 2 MiB pages across all physical memory below the top of
  RAM, including MMIO holes below 4 GiB; caching for those holes relies on the
  firmware's MTRR setup. Device registers the kernel uses are mapped
  separately as uncached.
* 5-level paging firmware configurations are refused by the boot manager.
* The PIT calibration fallback and x2APIC mode are untested.
* Reboot via the 8042 controller / triple fault fallbacks are untested.
* The keyboard driver only knows the US layout.
* The framebuffer console redraws the whole text area on scroll (slow on very
  large displays without a shadow buffer).

## Known bugs

None open. Fixed during bring-up (2026-10-31):

* Boot manager read GOP mode information after freeing it.
* Boot manager sized the direct map from reserved address space (mapping
  1 TiB under QEMU's AMD CPU model), wasting 8 MiB of page tables.
* PS/2 initialisation wrote back a controller configuration with the
  keyboard-disable bit still set, so no key presses arrived. Now covered by
  the CI step that types shell commands through QEMU's keyboard.

## Failed tests

None. Latest local run: 31/31 passed (debug and release); CI green.

## Real hardware tested

None yet.
