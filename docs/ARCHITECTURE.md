# Meridian OS architecture

This document describes the system as designed. Components that do not exist
yet are marked *(planned)*; see [STATUS.md](STATUS.md) for what is actually
implemented.

## Overview

```
┌──────────────────────────────────────────────────────────────────────┐
│  Applications                                                        │
│  Native Meridian apps (PE32+, Meridian SDK)  │  Windows apps (.exe)   │
├──────────────────────────────────────────────┼───────────────────────┤
│  Native runtime: msys.dll, UI toolkit        │  Win32 subsystem:      │
│  (planned)                                   │  ntdll, kernel32,      │
│                                              │  user32, gdi32… (planned)│
├──────────────────────────────────────────────┴───────────────────────┤
│  System services (user mode, planned): session manager, service      │
│  manager, window manager/compositor, audio engine, network service,  │
│  user-mode driver hosts, Win32 server                                │
╞══════════════════════════ system call boundary ══════════════════════╡
│  Executive: object manager (Ob), memory manager (Mm), process and    │
│  thread manager (Ps), I/O manager (Io), security (Se), executive     │
│  support (Ex)                                                        │
├──────────────────────────────────────────────────────────────────────┤
│  Kernel core (Ke): IRQL, per-CPU state, clock, scheduler/dispatcher, │
│  synchronisation primitives                                          │
├──────────────────────────────────────────────────────────────────────┤
│  HAL: ACPI tables, timers (HPET/PIT/TSC/APIC), PCI, power control     │
│  Arch (x86-64): GDT/IDT/TSS, APIC, paging, context switch            │
├──────────────────────────────────────────────────────────────────────┤
│  Meridian Boot Manager (UEFI application)                            │
└──────────────────────────────────────────────────────────────────────┘
```

## Kernel model: hybrid, NT-inspired

Meridian follows the broad structure that made Windows NT successful for
desktop compatibility, re-implemented independently:

* **One kernel address space, many isolated user address spaces.** The upper
  half of every address space maps the kernel; user processes get private
  lower halves.
* **Layering.** Hardware specifics live in `arch/` and `hal/`; the kernel core
  (`ke/`) provides scheduling primitives; the executive (`mm/`, and the
  planned `ob/`, `ps/`, `io/`, `se/`) builds policy on top.
* **Objects and handles** *(planned)*. Processes, threads, files, sections,
  events and so on are kernel objects referenced through per-process handle
  tables with access checks. This maps naturally onto the Win32 API and
  gives the native API the same capability-style model.
* **IRQL.** Interrupt priority levels are implemented exactly the way the x64
  architecture intends: the local APIC task priority (CR8). Spin locks raise
  to `DISPATCH_LEVEL`; device interrupts run at IRQL = vector >> 4.
* **Drivers in user mode where practical** *(planned)*. Storage, network,
  audio, input and most bus drivers are intended to run in isolated user-mode
  driver hosts that talk to the I/O manager over IPC, so a driver crash
  restarts the driver instead of stopping the machine. Only code that must be
  in the kernel (interrupt dispatch, DMA/IOMMU setup, the GPU memory manager
  core, boot-critical consoles) stays there.

### Why not a microkernel or a monolith?

A pure microkernel pays an IPC cost on every I/O path that hurts games and
low-latency audio; a pure monolith lets any driver bug take the machine down.
The hybrid design keeps the scheduler, memory manager and object manager in
the kernel for speed and keeps drivers out wherever latency allows.

## Languages

The user asked for Rust, C and C++, each where it fits best:

| Language | Used for | Why |
|----------|----------|-----|
| **Rust** | Boot manager, kernel, HAL, kernel drivers, core system services | Memory safety in the most privileged code; no runtime; excellent bare-metal support |
| **C** *(planned)* | Win32 compatibility DLLs (ntdll, kernel32, …), the C runtime and native SDK headers | The Windows ABI is a C ABI described by C headers; C keeps structure layouts and calling conventions exact |
| **C++** *(planned)* | Desktop shell, compositor and UI toolkit, COM/OLE, DirectX translation layers | COM is defined in terms of C++ vtables; existing translation layers (DXVK, VKD3D-Proton) are C++ |
| **Assembly** | Interrupt entry stubs, context switching, the SMP trampoline | Inline in Rust via `asm!`/`global_asm!`; no separate assembler needed |

C and C++ will be compiled with Clang (LLVM) targeting Meridian's PE32+
format. Clang is not needed until the first user-mode component lands.

## Executable format

Meridian uses **PE32+** (the Windows executable format) for *both* native
applications and Windows applications. One loader serves both; native
programs simply import from Meridian's own system DLLs instead of the Win32
ones, and are marked with their own subsystem value. This avoids maintaining
two loaders and makes the Windows compatibility path first-class. The kernel
image itself is ELF64 because that is what the Rust bare-metal toolchain
produces; only the boot manager reads it.

## Boot flow

1. UEFI firmware loads `\EFI\BOOT\BOOTX64.EFI` (Meridian Boot Manager).
2. The boot manager reads `\meridian\kernel.elf` and `\meridian\cmdline.txt`,
   sets the display mode, loads the kernel, builds page tables, reserves a
   low page for SMP startup, captures the ACPI RSDP and memory map, exits
   boot services and jumps to the kernel.
3. The kernel initialises the CPU, memory, display, ACPI, interrupt
   controllers, timers and boot devices, then starts the system (today: the
   kernel shell or the self tests).

## Source tree

| Path | Contents |
|------|----------|
| `boot/` | Meridian Boot Manager (UEFI, Rust) |
| `shared/bootinfo/` | Boot manager ↔ kernel handoff structures |
| `kernel/src/arch/x86_64/` | CPU, GDT/TSS, IDT and trap dispatch, local/I-O APIC, PIC |
| `kernel/src/hal/` | ACPI, HPET, timer calibration, PCI, power, serial |
| `kernel/src/ke/` | IRQL, per-processor control region, clock |
| `kernel/src/mm/` | Physical allocator, paging, kernel virtual space, pool |
| `kernel/src/drivers/` | Framebuffer console, PS/2 keyboard |
| `kernel/src/shell.rs` | Kernel debug shell |
| `kernel/src/selftest.rs` | In-kernel test suite |
| `kernel/assets/` | Console font (Spleen, BSD-2-Clause) |
| `tools/` | Build tool, disk image writer, font converter |
| `docs/` | Documentation |

Planned top-level directories as components arrive: `executive/` (if the
executive is split out of the kernel crate), `system/` (user-mode services),
`runtime/` (native runtime and C library), `wincompat/` (Win32 subsystem),
`graphics/`, `desktop/`, `shell/`, `applications/`, `filesystem/`,
`network/`, `audio/`, `sdk/`, `installer/`, `tests/` (host-side and
application compatibility tests).
