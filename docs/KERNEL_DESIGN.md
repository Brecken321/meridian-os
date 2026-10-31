# Kernel design

The Meridian kernel is a 64-bit, higher-half, hybrid kernel written in Rust
(`#![no_std]`, no third-party crates). This document is the contract between
its parts; keep it in sync with the code.

## Boot handoff

The boot manager (`boot/`) enters `kernel_entry(info: *const BootInfo)` with:

* long mode, paging on, interrupts off, `EFER.NXE` set;
* page tables mapping the kernel image (per-segment permissions), the boot
  stack, a direct map of all RAM (and at least the first 4 GiB) at
  `0xFFFF_8000_0000_0000`, and a temporary identity map;
* `RSP` at the top of a 64 KiB boot stack with an unmapped guard page below;
* `RDI` pointing at the `BootInfo` (through the direct map).

`BootInfo` (`shared/bootinfo`) holds the memory map (sorted, merged, typed),
framebuffer, ACPI RSDP, UEFI system table, command line, firmware vendor and
the reserved low page for the SMP trampoline. The boot manager uses
OS-loader-defined UEFI memory types (`0x8000_0001`…) so the kernel can tell
its image, stack, page tables and boot information apart from free memory.

## Virtual memory layout

| Range | Use |
|-------|-----|
| `0000_0000_0000_0000 – 0000_7FFF_FFFF_FFFF` | User mode (per process, planned) |
| `FFFF_8000_0000_0000 – …` | Direct map of physical memory |
| `FFFF_E000_0000_1000 – FFFF_E000_0001_1000` | Boot stack (guard page below) |
| `FFFF_E000_0100_0000 – FFFF_E010_0000_0000` | Kernel virtual area: stacks, MMIO |
| `FFFF_FFFF_8000_0000 – …` | Kernel image (text R-X, rodata R--, data RW-) |

The kernel half (PML4 slots 256–511) is shared by all address spaces. All 256
kernel PDPTs are created at boot so they never change, which makes creating
a process address space a matter of copying 256 PML4 entries.

The boot identity map is removed (and its page tables freed) as soon as the
memory manager starts; a NULL or low-address dereference in the kernel faults.

## Memory manager

* **Physical frames** (`mm/pmm.rs`): bitmap, one bit per 4 KiB frame, placed
  in the first usable region large enough. Memory below 1 MiB is never
  allocated. Double frees stop the system. Contiguous, aligned allocation is
  supported for DMA buffers and large pool blocks. A full PFN database
  (per-frame reference counts and state, needed for shared and file-backed
  memory) is planned for the user-mode milestone.
* **Paging** (`mm/paging.rs`): map/unmap/translate for 4 KiB pages, huge-page
  aware translation, kernel-half lock. TLB shootdown across processors will
  be added together with SMP.
* **Kernel virtual area** (`mm/kva.rs`): reserve/release ranges; uncached
  MMIO mappings; kernel stacks with guard pages.
* **Pool** (`mm/heap.rs`): Rust global allocator. Power-of-two size classes up
  to 2 KiB carved from pages; larger requests use contiguous frames. Pool use
  is forbidden above `DISPATCH_LEVEL` (debug-asserted).

## Processor setup

Every processor runs with `CR0.WP`, `EFER.NXE`, SMEP, SMAP and UMIP when the
CPU supports them, and global pages once the identity map is gone.

GDT layout (per CPU, chosen to satisfy SYSCALL/SYSRET and leave room for a
32-bit compatibility segment):

| Selector | Segment |
|----------|---------|
| `0x10` | kernel code (64-bit) |
| `0x18` | kernel data |
| `0x23` | user code (32-bit) |
| `0x2B` | user data |
| `0x33` | user code (64-bit) |
| `0x40` | TSS |

Double fault, NMI and machine check run on dedicated IST stacks.

## Interrupts and IRQL

All 256 IDT vectors enter through build-time generated stubs (`build.rs`)
that build a uniform `TrapFrame` and call `kernel_trap_dispatch`, which
handles `swapgs` for traps from user mode.

IRQL is the local APIC task priority, accessed through CR8. A vector's
priority class (`vector >> 4`) is the IRQL its handler runs at:

| IRQL | Name | Vectors |
|------|------|---------|
| 0 | PASSIVE | – |
| 1 | APC | `0x1F` (planned) |
| 2 | DISPATCH | `0x2F` dispatcher software interrupt (planned) |
| 5–12 | device | `0x50–0xCF`: keyboard `0x81`, serial `0x82` |
| 13 | CLOCK | `0xD1` local APIC timer |
| 14 | IPI | `0xE1` generic, `0xE2` TLB shootdown |
| 15 | HIGH | `0xF0–0xFD` masked legacy PIC, `0xFE` APIC error, `0xFF` spurious |

Spin locks (`sync::SpinLock`) raise to `DISPATCH_LEVEL` by default (or a
higher level with `lock_at`) and restore the previous IRQL on release. The
legacy 8259 PICs are remapped to `0xF0` and masked.

### Recoverable faults

Code that must touch memory that might not be mapped (probing firmware
structures now, user buffers later) uses the fixup table: an instruction
registered in the `.meridian_fixup` section resumes at its fixup address
instead of crashing on a page fault or general protection fault
(`idt::fixup::read_u64`). This is the foundation for safe `copy_from_user`.

### Stop errors

Unrecoverable conditions call `panic::bugcheck(code, detail, frame)`, which
NMIs the other processors, paints the stop screen, prints registers, control
registers and a frame-pointer stack trace to screen and serial, and halts.
Under the test harness it exits QEMU with a failure code.

## Time

The TSC and local APIC timer are calibrated against the HPET (or the PIT if
there is no HPET). The bootstrap processor's 1 kHz APIC timer interrupt
maintains the tick count; `clock::uptime_us()` uses the TSC. Tickless
(one-shot/deadline) operation is planned to reduce idle power use.

## Threads and the dispatcher (`ke/sched.rs`, `ke/thread.rs`)

* **Threads** have a 32 KiB kernel stack with a guard page, a priority
  (0 idle, 1–15 variable, 16–31 real-time), a state, a saved stack pointer
  and a "terminated" notification event used for joining. Each processor's
  boot context becomes its idle thread.
* **Context switch** (`arch/x86_64/context.rs`): pushes callee-saved
  registers and RFLAGS on the outgoing stack, swaps stack pointers, pops on
  the incoming stack. New threads start in a trampoline that calls
  `meridian_thread_startup`.
* **Ready queues**: one FIFO per priority plus a 32-bit summary bitmap
  (O(1) selection). One dispatcher lock protects all scheduling and
  dispatcher-object state; it is only taken at DISPATCH_LEVEL and never in
  interrupt handlers. A switch happens with the lock held and the incoming
  thread releases it (`finish_switch`), which also drops the outgoing
  thread's reference so a terminated thread's stack is freed only once
  nothing runs on it.
* **Preemption**: the clock interrupt counts down a 10-tick quantum and
  checks timeout deadlines; readying a higher-priority thread sets
  `need_resched`. Both raise the DISPATCH software interrupt (vector `0x2F`,
  a self-IPI), which the APIC holds pending until the IRQL drops below
  DISPATCH_LEVEL, so preemption never happens while a spin lock is held.
  The dispatch interrupt gets its EOI before its handler runs because the
  handler may switch threads.
* **DPCs** (`ke/dpc.rs`): interrupt handlers queue a DPC on the current
  processor; DPCs run first thing in the dispatch interrupt.
* **Waits** (`ke/objects.rs`): events (auto- and manual-reset), counting
  semaphores and recursive mutexes, with optional timeouts. Timeouts live in
  a deadline-ordered map processed in the dispatch interrupt.

Planned: wait-for-multiple-objects, APCs, priority boosts and decay, timer
objects, per-processor ready queues and load balancing.

## User mode (`mm/user.rs`, `ps/`, `ob/`, `arch/x86_64/syscall.rs`)

* **Address spaces**: one PML4 per process; slots 256-511 point at the
  shared kernel PDPTs. User memory is tracked as committed regions and fully
  released when the address space is dropped (after the last thread has been
  switched out). The first and last 64 KiB of user space are never mapped.
* **Switching**: `switch_locked` loads the incoming thread's page-table root
  (the kernel PML4 for kernel threads) and points TSS.RSP0 and the PCR's
  system-call stack slot at its kernel stack.
* **Entering ring 3**: `enter_user` builds an `iretq` frame (user CS/SS,
  RFLAGS.IF set), clears every general-purpose register except the argument
  in RCX, and executes `swapgs; iretq`.
* **System calls**: `syscall` enters `meridian_syscall_entry`, which switches
  to the thread's kernel stack, saves a `SyscallFrame`, enables interrupts
  and calls the service dispatcher (`ps/syscall.rs`). Arguments follow
  `shared/abi`; results are non-negative values or negative NTSTATUS codes.
  Return goes through `sysretq` after checking that the return address is
  canonical.
* **User pointers**: `copy_from_user`/`copy_to_user` range-check against
  user space and copy with `rep movsb` registered in the fixup table (with
  `stac`/`clac` when SMAP is on), so a bad pointer returns an error.
* **Faults**: any CPU exception raised in ring 3 terminates the process with
  the matching NTSTATUS code and logs the faulting address.
* **Handles** (`ob/`): per-process tables; handle values are multiples of 4.
* **PE32+ loader** (`ps/pe.rs`): validates headers, maps sections with
  per-section protections, applies DIR64 relocations, sizes the stack;
  rejects images that need imports or TLS.

## Multiprocessor support (`arch/x86_64/smp.rs`, `ke/ipi.rs`)

* **Startup**: the boot manager reserves a page below 1 MiB. The kernel
  copies a trampoline there (16-bit → 32-bit → 64-bit) and patches in the
  absolute addresses it needs. A temporary PML4 below 4 GiB maps the
  trampoline page 1:1 plus the shared kernel half. Each processor is started
  with INIT and up to two STARTUP IPIs, one at a time, and jumps to
  `ap_entry` on a stack the bootstrap processor allocated, where it loads
  the kernel page tables, its own GDT/TSS (with IST stacks) and PCR, enables
  its local APIC and timer and becomes an idle thread.
* **IPIs**: one vector (`0xE1`); requests are bits in the target's PCR
  (`REQUEST_RESCHEDULE`, `REQUEST_CALL`, `REQUEST_TLB_FLUSH`).
* **TLB shootdown**: `paging::unmap_kernel` invalidates the page on every
  processor before returning, so frames and addresses can be reused
  immediately. Shootdowns must never be issued with the dispatcher lock held.
* **Scheduling**: `make_ready` picks an idle processor, or the one running
  the lowest-priority thread, and IPIs it if the new thread outranks it.
  Idle processors also check a lock-free copy of the ready summary on every
  clock tick.
* Panics NMI all other processors so the stop screen is never overwritten.
