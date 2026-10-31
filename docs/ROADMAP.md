# Roadmap

This is the persistent plan that every development session continues from.
Status markers: ✅ done and tested · 🚧 in progress · ⬜ not started.
Update this file whenever a milestone moves.

## Phase 1 — Architecture and research ✅

* ✅ Environment survey, kernel model (hybrid, NT-inspired), language split
  (Rust kernel; C for Win32 ABI; C++ for desktop/COM/graphics)
* ✅ Repository, build tool, CI, documentation set

## Phase 2 — Original kernel boots ✅

* ✅ UEFI boot manager (own UEFI bindings, ELF loader, page tables, GOP)
* ✅ Higher-half kernel, CPU protection features, GDT/TSS/IDT
* ✅ Serial + framebuffer logging, stop screen
* ✅ Boots in QEMU (q35, OVMF), automated smoke tests in CI

## Phase 3 — Core kernel 🚧

* ✅ Physical memory manager, page tables, kernel pool, guard-paged stacks
* ✅ Exceptions with recoverable fault probes
* ✅ ACPI static tables, local APIC (xAPIC/x2APIC), I/O APIC, IRQL via CR8
* ✅ HPET/PIT calibration, TSC clock, 1 kHz clock interrupt
* ✅ Threads, context switching, priority scheduler, DISPATCH-level preemption, DPCs
* ✅ Dispatcher objects: events (auto/manual reset), mutexes, semaphores, timed waits, sleep
* ⬜ Wait for multiple objects, APCs, priority boosts, timer objects
* ✅ SMP: AP startup, per-CPU state, IPIs, cross-processor calls, TLB shootdown, multicore scheduling
* ⬜ Per-processor ready queues, load balancing, thread affinity
* ✅ Handle tables (per process, NT-style handle values); 🚧 object namespace and access masks
* ✅ System calls (SYSCALL/SYSRET), user-mode address spaces, safe user-memory copies (SMAP-aware)
* ⬜ PFN database, demand paging, sections (shared/mapped memory)

## Phase 4 — Storage and devices ⬜

* ⬜ Driver model and I/O manager (IRPs or message-based), PnP enumeration
* ⬜ PCIe ECAM, MSI/MSI-X
* ⬜ Storage: virtio-blk, AHCI, NVMe
* ⬜ VFS, FAT32 (read/write), exFAT; NTFS read-only first
* ⬜ USB: xHCI, HID keyboard/mouse; PS/2 mouse
* ⬜ ACPI AML interpreter (evaluate a vetted existing implementation vs. own)

## Phase 5 — User mode ⬜

* ✅ PE32+ loader (sections, protections, base relocations); ⬜ imports, TLS, exception tables
* ✅ Native runtime (`runtime/rt`) + first user program (`system/init`); ⬜ session manager; service manager
* ✅ Process isolation tests; a crashing user process never stops the kernel
* ⬜ User-mode thread creation, multi-threaded processes, process creation from user mode
* ⬜ User-mode driver host prototype

## Phase 6 — Graphics and desktop ⬜

* ⬜ Display driver interface; GOP framebuffer driver; virtio-gpu
* ⬜ Compositor + window manager; input routing
* ⬜ UI toolkit (C++); fonts (open fonts, e.g. Inter/Noto); theming
* ⬜ Desktop shell: taskbar, Start menu, tray, notifications, snapping,
  virtual desktops, Alt+Tab, lock/login screens
* ⬜ File Explorer, Settings (every control wired to real functionality)

## Phase 7 — Windows compatibility ⬜

* ⬜ ntdll/kernel32/kernelbase core (processes, threads, files, sync, heap)
* ⬜ Registry, environment, paths, SEH/exception dispatch, TLS
* ⬜ user32/gdi32 windows and drawing on the Meridian compositor
* ⬜ Compatibility test suite and database (see WINDOWS_COMPATIBILITY.md)

## Phase 8 — Graphics acceleration and gaming ⬜

* ⬜ GPU driver infrastructure; Vulkan loader; port of Mesa drivers (RADV/ANV/NVK)
* ⬜ DXVK / VKD3D-Proton integration for Direct3D 9–12
* ⬜ Controllers (XInput/HID), low-latency audio, gaming overlay
* ⬜ Steam client bring-up (see GAMING_COMPATIBILITY.md)

## Phase 9 — Networking, multimedia, hardware ⬜

* ⬜ TCP/IP stack (IPv4/IPv6, UDP, DHCP, DNS), e1000/virtio-net, Intel/Realtek NICs, Wi-Fi
* ⬜ Audio engine; HD Audio; per-app volume
* ⬜ Power management: S3/s2idle, CPU P-states, battery
* ⬜ Bluetooth

## Phase 10 — Ecosystem ⬜

* ⬜ SDK, package format with signatures, software center (no fake listings)

## Phase 11 — Security and stability ⬜

* ⬜ Security audit, fuzzing of parsers and system calls, KASLR, IOMMU

## Phase 12 — Installer and release ⬜

* ⬜ Installer (VM first), recovery environment, signed updates

## Phase 13 — Continuous improvement ⬜

## Next session starts here

1. Object manager: typed, reference-counted kernel objects, a namespace
   (`\Device`, `\ObjectTypes`, ...) and per-process handle tables with
   granted-access masks.
2. Process objects and user address spaces (PML4 per process sharing the
   kernel half), SYSCALL/SYSRET entry with a service table, safe user-memory
   copy routines built on the fixup table, and the first ring-3 program.
3. PE32+ loader for that first program (native subsystem).
