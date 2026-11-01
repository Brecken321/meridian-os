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
* Symmetric multiprocessing: every enabled processor in the MADT is started
  through a real-mode trampoline (tested with 1, 2 and 4 vCPUs); per-processor
  GDT/TSS/IST stacks, PCR, idle thread and clock; IPIs for rescheduling,
  cross-processor function calls and TLB shootdown; the scheduler places
  newly ready threads on idle or lower-priority processors.
* Interrupt-driven console input: PS/2 keyboard and COM1 receive interrupts
  wake the shell thread through a DPC and an event.
* User mode: per-process address spaces (private lower half, shared kernel
  half), ring-3 threads, SYSCALL/SYSRET system call entry with 15 services
  (debug output, exit, yield, sleep, ids, memory allocate/free, events, wait,
  close handle, uptime, system information), per-process handle tables, argument validation with
  fault-safe user copies, CPU exceptions in user mode terminate only the
  process (with NTSTATUS codes), complete teardown of process memory.
* PE32+ loader: header validation, section mapping with per-section
  protections, DIR64 base relocations (images are loaded 256 MiB above their
  preferred base, so relocation is always exercised), stack sizing.
* Native runtime (`runtime/rt`) and the first program `init.exe`
  (`system/init`), read from `C:\MERIDIAN\INIT.EXE` and run at every boot
  (the boot manager's copy is the fallback).
* Storage (read-only by design): AHCI SATA driver (IDENTIFY, 48-bit DMA
  reads through a 64 KiB bounce buffer, polled completion, BIOS/OS handoff),
  GPT and MBR partition discovery, FAT32 file system with long file names,
  short-name aliases and corruption checks, drive letters from `C:`.
* Kernel shell: help, ver, cpu, mem, acpi, pci, irq, threads, ps, run, disks,
  vol, dir, type, uptime, log, clear, selftest, reboot, shutdown, crash.
* Build: reproducible GPT/FAT32 image writer, QEMU runner, self-test harness,
  screenshot capture, GitHub Actions CI. All crates build without compiler
  warnings. `run-meridian.cmd` builds a release image and opens it in QEMU
  with short instructions.

## Not implemented yet

Multi-threaded user processes, process creation from user mode, object
namespace and access checks, PE imports/TLS, writing to disks, NVMe,
NTFS, file access from user mode, any filesystem, USB, networking, audio, GPU acceleration,
desktop, Windows compatibility, installer. See [ROADMAP.md](ROADMAP.md).

## Known limitations

* The scheduler has one global ready queue and dispatcher lock; that is
  simple and correct but will contend on many-core machines. Per-processor
  queues are planned.
* No thread affinity, wait-for-multiple-objects, APCs or priority boosting yet.
* User processes have exactly one thread; all user memory is committed up
  front (no demand paging); programs must not import DLLs; user programs
  cannot open files yet (only the kernel reads the disk).
* Storage is read-only and polled (no AHCI interrupts or command queuing);
  only FAT32 is recognised, so the EFI system partition becomes `C:`;
  optical drives are skipped.
* Every TLB shootdown interrupts all other processors (no tracking of which
  processors might cache the address).
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
* SMP deadlock: a terminated thread's stack was freed during the context
  switch, sometimes with interrupts disabled, and the resulting TLB
  shootdown could wait forever on another processor doing the same. Dead
  threads now go to a reaper thread, and every processor waiting on others
  services TLB flush requests addressed to it.
* xAPIC IPIs could be misdirected: an interrupt arriving between the two
  ICR register writes could overwrite the destination. The write sequence
  now runs with interrupts disabled.
* Found by repeated test runs: the IPI handler could drop a TLB flush request
  (introduced while fixing the first issue). Shootdown timeouts now name the
  processor that failed to respond and the thread it was running.

## Failed tests

None. Latest local runs: 48/48 passed (repeated debug and release runs,
4 vCPUs).

## Real hardware tested

None yet.
