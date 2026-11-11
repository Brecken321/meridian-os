# Hardware support

Meridian claims support for hardware only after it has been tested.

## Tested platforms

| Platform | Firmware | Result | Date |
|----------|----------|--------|------|
| QEMU 11.1.0 `q35`, TCG, `-cpu max`, 1–4 vCPUs, 512 MiB | edk2 (bundled with QEMU for Windows) | Boots; all 48 self tests pass; framebuffer, PS/2 keyboard, serial, HPET, AHCI disk, ACPI power-off | 2026-10-31 |
| QEMU (Ubuntu 24.04 package) `q35`, TCG | OVMF (Ubuntu package) | Run by CI on every push | continuous |

**No physical computer has been tested yet.**

## Requirements (current kernel)

* x86-64 processor with NX (all x86-64 CPUs from the last ~20 years)
* UEFI firmware (no legacy BIOS boot), Secure Boot disabled for now
* 4-level paging (firmware must not enable 5-level paging)
* ACPI with a MADT
* A UEFI GOP framebuffer for on-screen output (otherwise serial only)

## Device support matrix

| Class | Device | Status |
|-------|--------|--------|
| Interrupts | Local APIC (xAPIC) | ✅ tested in QEMU |
| Interrupts | x2APIC | ⚠️ implemented, untested |
| Interrupts | I/O APIC | ✅ tested in QEMU |
| Timers | HPET, TSC, APIC timer | ✅ tested in QEMU |
| Timers | PIT (calibration fallback) | ⚠️ implemented, untested |
| Display | UEFI GOP framebuffer | ✅ tested in QEMU |
| Input | PS/2 keyboard (i8042) | ✅ tested in QEMU |
| Serial | 16550 UART (COM1) | ✅ tested in QEMU |
| Bus | PCI configuration (CF8/CFC) | ✅ tested in QEMU |
| Power | ACPI S5 soft-off | ✅ tested in QEMU |
| Power | ACPI reset register | ⚠️ implemented, untested |
| Storage | AHCI SATA disks (read-only) | ✅ tested in QEMU (ICH9 AHCI) |
| Storage | ATAPI optical drives | ⚠️ detected and skipped |
| Storage | NVMe, virtio-blk, USB mass storage | ⬜ planned |
| USB | xHCI, HID | ⬜ planned |
| Network | e1000, virtio-net, Intel/Realtek | ⬜ planned |
| Audio | HD Audio | ⬜ planned |
| GPU | AMD, Intel, NVIDIA | ⬜ planned (see GAMING_COMPATIBILITY.md) |

## Driver priorities

1. QEMU virtual hardware (virtio, e1000, AHCI, xHCI)
2. Storage (AHCI, NVMe)
3. USB (xHCI) and HID input
4. Networking
5. Audio (HD Audio)
6. Display/GPU
7. Bluetooth, printers, cameras

Drivers are original code written from public specifications (UEFI, ACPI,
PCI, AHCI, NVMe, xHCI, HD Audio, virtio). Existing open-source drivers may
be studied for hardware quirks; code is only reused when its licence allows
and is recorded in [LICENSES.md](LICENSES.md). Windows drivers cannot be
loaded; a Windows driver compatibility layer would be a separate, much later
project.

## Reporting a test on real hardware

Record: vendor and model, CPU, RAM, firmware vendor and version (the `ver`
shell command prints it), what was attached, the serial or on-screen log, and
what worked or failed.
