# Troubleshooting

### `qemu-system-x86_64 not found`

Install QEMU (`winget install SoftwareFreedomConservancy.QEMU` on Windows,
`sudo apt install qemu-system-x86` on Debian/Ubuntu) or set `MERIDIAN_QEMU`
to the full path of `qemu-system-x86_64`.

### `UEFI firmware (OVMF/edk2) not found`

On Linux install the `ovmf` package. Otherwise set `MERIDIAN_OVMF_CODE` and
`MERIDIAN_OVMF_VARS` to a matching pair of edk2 x86-64 firmware images.
Use a firmware build **without** Secure Boot enforcement (the boot manager is
not signed yet).

### Firmware drops to the UEFI shell or boot menu

The image is probably stale or missing; rerun `python tools/mbuild.py build`
and check that `build/meridian.img` exists.

### The boot manager says the kernel was not found

The image must contain `MERIDIAN/KERNEL.ELF`. Rebuild with `mbuild.py`; do not
copy files by hand.

### "the firmware enabled 5-level paging"

Some firmware enables LA57 on CPUs that support it. Meridian does not support
5-level paging yet. Under QEMU use `--cpu max,la57=off`.

### Linker errors about `link.exe` on Windows

The kernel's build script is compiled for the host and needs the MSVC linker.
Install Visual Studio Build Tools with the "Desktop development with C++"
workload, or use the GNU host toolchain (`rustup set default-host x86_64-pc-windows-gnu`).

### The test run times out

Look at `build/serial-test.log`. A hang before `boot: kernel initialised`
points at early initialisation; increase `--timeout` on slow machines
(emulation without hardware acceleration is slow).

### A stop screen appears

Note the stop code and stack trace, then resolve addresses with
`python tools/symbolize.py <address> ...` (add `--release` for release
builds) and open an issue with the serial log attached.
