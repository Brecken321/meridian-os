# Testing

Meridian's rule: **a feature is not done until a test exercises it on the
real kernel**, and test results are never reported unless they actually ran.

## Kernel self tests

`kernel/src/selftest.rs` contains tests that run inside the booted kernel.
`python tools/mbuild.py test` builds the system, boots it in QEMU (q35
machine, UEFI firmware, 4 vCPUs, 512 MiB) with `selftest` on the kernel
command line, and the kernel:

1. finishes normal initialisation,
2. runs every test, printing `[selftest] PASS name` / `[selftest] FAIL name: reason`,
3. prints `SELFTEST: ALL PASSED` or `SELFTEST: FAILURES`,
4. exits QEMU through the `isa-debug-exit` device (code `0x10` = pass).

A kernel crash during the tests exits QEMU with the failure code, and a hang
is caught by the harness timeout, so CI never waits forever.

Current tests (31):

| Test | Checks |
|------|--------|
| `pmm.alloc_free` | Frame allocation is aligned, above 1 MiB, unique, leak-free; accounting is consistent |
| `pmm.contiguous` | 64 KiB aligned contiguous allocation is real, writable RAM |
| `heap.collections` | `Box`, `Vec`, `BTreeMap`, `format!` |
| `heap.large` | 1 MiB allocation round trip, pool accounting returns to baseline |
| `heap.alignment` | Alignments from 8 B to 16 KiB |
| `paging.map_unmap` | Map, write, translate, double-map rejection, unmap, TLB invalidation |
| `paging.guard_page_probe` | Kernel stacks have an unmapped guard page |
| `paging.kernel_protection` | Kernel text is R-X, direct map is NX, identity map is gone, CR0.WP on |
| `trap.breakpoint` | `int3` is handled and execution resumes |
| `trap.noncanonical_probe` | Faults on non-canonical / NULL addresses are recovered by the fixup table |
| `irql.raise_lower` | IRQL raise/lower through CR8 |
| `sync.spinlock_irql` | Spin locks raise to DISPATCH_LEVEL, exclude, and restore IRQL |
| `apic.self_ipi` | An interrupt stays pending while masked by IRQL and is delivered once IRQL drops |
| `clock.ticks` | The 1 kHz clock interrupt runs and agrees with the TSC |
| `clock.tsc_calibrated` | TSC and APIC timer frequencies were measured |
| `hpet.counter` | HPET counter is running (when present) |
| `acpi.tables` | MADT lists processors and an I/O APIC; FADT present |
| `pci.enumeration` | PCI scan finds devices including a host bridge |
| `ps2.decode` | Scancode decoding incl. shift and extended keys |
| `dpc.deferred_until_irql_drops` | A DPC queued at HIGH_LEVEL runs exactly once, only after IRQL drops |
| `sched.spawn_join` | Thread creation, execution and join |
| `sched.many_threads` | 16 threads with yields complete correctly |
| `sched.time_slicing` | Two never-yielding threads both make progress (preemptive round robin) |
| `sched.priority_preemption` | A higher-priority thread preempts the creator immediately |
| `sched.sleep` | Sleep duration is at least the request and not wildly longer |
| `sched.stack_reclaimed` | Creating and destroying 20 threads leaks no memory |
| `sync.event` | Auto-reset event blocks a waiter, releases it once, resets |
| `sync.event_timeout` | Timed waits time out after at least the timeout; zero timeout polls |
| `sync.notification_event` | Manual-reset event releases all waiters and stays set until reset |
| `sync.mutex` | Mutual exclusion across yields (no lost updates), recursion |
| `sync.semaphore` | Producer/consumer counting and the limit |

### Adding a test

Add a `fn my_test() -> TestResult` to `selftest.rs`, use `ensure!(cond, "message")`,
and append it to `TESTS`. Keep tests independent and leave no state behind.

## Continuous integration

`.github/workflows/ci.yml` runs on every push and pull request (Ubuntu 24.04,
QEMU + OVMF from apt):

1. `cargo fmt --check`
2. self tests, debug build
3. self tests, release build
4. boot screenshot
5. interactive shell check: `mbuild.py exec help cpu mem irq shutdown
   --expect-poweroff` types commands on the emulated PS/2 keyboard (through
   QEMU's QMP `send-key`) and requires the VM to power itself off through
   ACPI, which exercises the keyboard driver, I/O APIC routing, the shell and
   ACPI S5 end to end

Logs, the screenshot and the disk image are uploaded as workflow artifacts.

## Manual checks

* `python tools/mbuild.py exec <command> ... [--screenshot]` automates most of
  these.
* `python tools/mbuild.py run` and try the shell commands (`help`, `cpu`,
  `mem map`, `acpi`, `pci`, `irq`, `uptime`, `crash`, `shutdown`).
* `crash` must show the stop screen with a stack trace.
* `shutdown` must power the VM off; `reboot` must restart it.

## Real hardware

No real hardware has been tested yet. When it is, record the exact machine
(vendor, model, CPU, firmware version) and the results in
[HARDWARE_SUPPORT.md](HARDWARE_SUPPORT.md).

## Planned test layers

* Host-side unit tests for pure logic (parsers, allocators) via `cargo test`.
* User-mode test programs (native and Win32) run by a test service once user
  mode exists.
* The Windows application compatibility database (see
  [WINDOWS_COMPATIBILITY.md](WINDOWS_COMPATIBILITY.md)).
