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
* PS/2 keyboard (US layout) and PS/2 mouse, with VMware's absolute-pointer
  ("vmmouse") protocol when the hypervisor offers it, so the mouse is never
  captured by the QEMU window; COM1 serial console; framebuffer console with
  ANSI colours.
* Real-time clock (CMOS RTC) for the date and time.
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
  half), ring-3 threads, SYSCALL/SYSRET system call entry with 26 services
  (debug output, exit, yield, sleep, ids, memory allocate/free, events, wait,
  close handle, uptime, system information, keyboard/mouse input, display
  ownership, list directory, read file, file size, create process, process
  status, thread and process lists, wall-clock time, shut down/restart),
  per-process handle tables, argument validation with fault-safe user copies,
  CPU exceptions in user mode terminate only the process (with NTSTATUS
  codes), complete teardown of process memory. User threads keep their own
  SSE/x87 state (FXSAVE/FXRSTOR on every switch).
* Desktop (`system/desktop`, a user-mode PE32+ program started at boot from
  `C:\MERIDIAN\DESKTOP.EXE`): it takes over the screen, keyboard and mouse
  and draws everything itself (software compositor with damage tracking,
  TrueType text, rounded corners, shadows, and a "Mica" material: the
  taskbar, Start menu, flyouts, menus and the focused title bar are drawn
  over a blurred copy of the wallpaper). Shell: window manager (focus,
  z-order, drag, resize from any edge or corner, minimise/maximise/close,
  double-click title to maximise, snap to the left/right half or maximise by
  dragging to a screen edge, Win+arrows, Alt+Tab, Alt+F4), taskbar with
  pinned and running apps, tooltips, a live clock with a calendar flyout and
  a Show desktop button; Start menu with search over apps and files,
  pinned apps, recommended files and Lock/Restart/Shut down; desktop icons
  including the files in `D:\Desktop`; right-click menus on the desktop,
  icons, taskbar buttons, title bars and inside apps; notifications; a lock
  screen (Win+L; there are no accounts, so no password); keyboard shortcuts
  Win, Win+D/E/R/L/I, Ctrl+Shift+Esc; settings saved in
  `D:\Meridian\settings.ini`.
* Desktop apps (all using real system calls): File Explorer (quick access,
  drives with free space, folders, open with the right app, new
  folder/file, cut/copy/paste including whole folders, rename in place,
  delete with confirmation, back/forward/up, type-to-select), Notepad
  (open, save, save as, selection with mouse and keyboard, clipboard,
  undo, time/date, zoom, keeps CRLF or LF, asks to save on close), Terminal
  (dir, cd, type, mkdir, del, rmdir, copy, move, ren, `echo text > file`,
  drives, run, open, starting apps, ps, threads, mem, uptime, date; command
  history and paste), Calculator (standard operations, percent, square,
  square root, reciprocal, history, keyboard input), Paint (pencil, brush,
  eraser, line, rectangle, ellipse, fill, colour picker, 20-colour palette,
  sizes, undo/redo, open and save 24-bit BMP), Photos (gallery of a folder,
  viewer with next/previous, fit/actual size, Edit in Paint), Clock (analog
  and digital clock, stopwatch with laps, countdown timer with a
  notification), Task Manager (processes, live CPU graph and memory, kernel
  threads), Settings (light/dark mode, accent colour, wallpaper, taskbar
  alignment, drive usage, system information), Minesweeper (three levels,
  flags, chording) and 2048. Verified by the `ui` walk-through in CI. When
  the desktop exits, the kernel console takes the screen back.
* PE32+ loader: header validation, section mapping with per-section
  protections, DIR64 base relocations (images are loaded 256 MiB above their
  preferred base, so relocation is always exercised), stack sizing.
* Native runtime (`runtime/rt`) and the first program `init.exe`
  (`system/init`), read from `C:\MERIDIAN\INIT.EXE` and run at every boot
  (the boot manager's copy is the fallback).
* Storage: AHCI SATA driver (IDENTIFY, 48-bit DMA reads and writes through
  a 64 KiB bounce buffer, cache flush, polled completion, BIOS/OS handoff),
  GPT and MBR partition discovery, FAT32 with long file names, short-name
  aliases and corruption checks, drive letters from `C:`. Writing (create,
  replace, delete, rename/move, folders) is allowed only on partitions with
  Meridian's own data-partition type GUID, so a disk prepared by another
  operating system is never modified; the boot drive `C:` stays read-only.
  The build attaches such a data disk as `D:` (`build/data.img`, kept
  between runs). After every test run, `tools/fatcheck.py` checks the data
  disk independently (FAT copies, chains, cross-links, lost clusters, long
  names, "." and "..").
* Kernel shell: help, ver, cpu, mem, acpi, pci, irq, threads, ps, run, disks,
  vol, dir, type, uptime, log, clear, selftest, reboot, shutdown, crash.
* Build: reproducible GPT/FAT32 image writer, QEMU runner, self-test harness,
  screenshot capture, scripted desktop walk-through (`mbuild.py ui`, mouse
  and keyboard through QMP), GitHub Actions CI. All crates build without
  compiler warnings. `run-meridian.cmd` builds a release image and opens the
  desktop in QEMU with short instructions.

## Not implemented yet

Multi-threaded user processes, object namespace and access checks, PE
imports/TLS, NTFS, NVMe, USB, networking, audio, GPU acceleration, Windows
compatibility (Win32), windows for programs other than the built-in apps,
user accounts and passwords, a recycle bin, PNG/JPEG images, installer.
See [ROADMAP.md](ROADMAP.md).

## Known limitations

* The scheduler has one global ready queue and dispatcher lock; that is
  simple and correct but will contend on many-core machines. Per-processor
  queues are planned.
* No thread affinity, wait-for-multiple-objects, APCs or priority boosting yet.
* User processes have exactly one thread; all user memory is committed up
  front (no demand paging); programs must not import DLLs; user programs can
  read files and list folders but not open handles to them.
* The desktop's apps are built into the desktop program itself; other
  programs cannot open windows yet (that needs a window-system protocol
  between processes). Everything is drawn by the CPU; there is no GPU
  driver.
* Without the vmmouse protocol (real hardware, or `novmmouse`), the PS/2
  mouse is relative and has no wheel.
* Storage is polled (no AHCI interrupts or command queuing); only FAT32 is
  recognised, so the EFI system partition becomes `C:`; only Meridian data
  partitions are writable (by design); files are read and written whole
  (64 MiB per write, 256 MiB per read); optical drives are skipped.
* Deleting is permanent (there is no recycle bin yet); Explorer asks first.
* Paint saves and Photos opens 24-bit BMP only (no PNG/JPEG decoder yet).
* Under QEMU on Windows, guest timer interrupts arrive at the host's timer
  resolution (15.6 ms unless another program raises it), so short timeouts
  are coarse; the kernel's clock and the desktop cope with late ticks.
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

None. Latest local runs (2026-11-10): 52/52 self tests passed (debug and
release, 4 vCPUs) with the test data disk consistent afterwards; the
desktop walk-through (`mbuild.py ui`, 15 steps through every major app,
ending in Start > Power > Shut down) passed with the absolute and the
relative pointer. The clock test was widened from 50 ms to 200 ms after it
failed on a Windows host at the default 15.6 ms timer resolution (the
previous commit failed the same way on the same host).

## Real hardware tested

None yet.
