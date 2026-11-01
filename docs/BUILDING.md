# Building Meridian OS

Meridian builds on Windows, Linux and macOS hosts. Everything happens inside
the repository: the build writes only to `target/` (Rust) and `build/`
(disk image, firmware variables, logs). It never writes to real disks or
changes your bootloader.

## Requirements

| Tool | Version | Notes |
|------|---------|-------|
| Rust (rustup) | stable | `rust-toolchain.toml` selects the channel, targets (`x86_64-unknown-uefi`, `x86_64-unknown-none`) and components automatically |
| Python | 3.10+ | Runs `tools/mbuild.py` (no extra packages needed) |
| QEMU | 7.1+ | `qemu-system-x86_64`; only needed to boot and test |
| UEFI firmware | edk2/OVMF | Bundled with QEMU on Windows; `ovmf` package on Debian/Ubuntu |

There are **no third-party Rust crates**: the build works offline once the
toolchain is installed. A host C/C++ compiler is only needed by Cargo for the
kernel's build script (Visual Studio Build Tools on Windows, the system
compiler elsewhere).

### Windows

```bash
winget install Rustlang.Rustup
```

```bash
winget install SoftwareFreedomConservancy.QEMU
```

### Ubuntu / Debian

```bash
sudo apt install qemu-system-x86 ovmf python3 && curl https://sh.rustup.rs -sSf | sh
```

## Commands

All commands run from the repository root.

| Command | What it does |
|---------|--------------|
| `python tools/mbuild.py build` | Compile boot manager + kernel, write `build/meridian.img` |
| `run-meridian.cmd` (Windows) | Release build + QEMU window; double-click to try Meridian |
| `python tools/mbuild.py run` | Build and boot in a QEMU window; the kernel log and shell also appear in your terminal |
| `python tools/mbuild.py run --headless` | Same without a window (serial console only) |
| `python tools/mbuild.py test` | Build and run the kernel self tests headless; exit code 0 = pass |
| `python tools/mbuild.py screenshot` | Boot headless and save `build/screenshot.png` |
| `python tools/mbuild.py exec cpu mem shutdown --expect-poweroff` | Boot headless, type shell commands on the virtual keyboard, print the output |
| `python tools/mbuild.py clean` | Remove `build/` and `target/` |

Useful options: `--release`, `--smp 4`, `--memory 1G`, `--cmdline "mm.verbose"`,
`--video video=1920x1080`, `--accel whpx` (Windows Hypervisor Platform, if you
have enabled it) or `--accel kvm` (Linux).

Overrides: `MERIDIAN_QEMU` (QEMU binary), `MERIDIAN_OVMF_CODE` and
`MERIDIAN_OVMF_VARS` (firmware images).

## Disk usage

The build is kept small on purpose: debug builds carry only line tables and
no incremental caches, so `target/` stays around 15 MB, and the disk image
is 48 MB. `python tools/mbuild.py clean` deletes both. QEMU itself is the
largest dependency (the Windows installer includes emulators for many CPU
architectures; Meridian only uses `qemu-system-x86_64`).

## Kernel command line

`build/meridian.img` contains `\MERIDIAN\CMDLINE.TXT`. Options:

| Option | Effect |
|--------|--------|
| `video=WxH` | Ask the boot manager for that UEFI display mode |
| `selftest` | Run the self tests at boot, then exit QEMU with the result |
| `mm.verbose` | Log the firmware memory map at boot |

## Disk image layout

GPT disk, 48 MiB, one EFI System Partition (FAT32) starting at LBA 2048:

```
EFI/BOOT/BOOTX64.EFI   Meridian Boot Manager
MERIDIAN/KERNEL.ELF    kernel
MERIDIAN/CMDLINE.TXT   kernel command line
MERIDIAN/INIT.EXE      first user-mode program (PE32+)
Welcome to Meridian.txt  (long file name; read it with `type`)
```

The image is reproducible (fixed GUIDs and timestamps). Writing it to a USB
stick to try real hardware is possible but **not yet recommended**: no real
machine has been tested, and Meridian has no storage drivers yet, so it
cannot damage disks — but it also cannot do anything useful beyond the
kernel shell. See [HARDWARE_SUPPORT.md](HARDWARE_SUPPORT.md).

## Debugging

* The kernel log goes to COM1; `run` shows it in your terminal.
* Stop screens print a stack trace; resolve addresses with
  `python tools/symbolize.py [--release] <address> ...` (uses `llvm-nm` from
  the `llvm-tools` component that `rust-toolchain.toml` installs).
* QEMU's GDB stub: add `-s -S` to the QEMU command printed by `mbuild.py run`
  and attach `gdb target/x86_64-unknown-none/debug/kernel` with
  `target remote :1234`.
