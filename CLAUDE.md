# Meridian OS — notes for development sessions

Original Windows-first OS with its own NT-inspired hybrid kernel in Rust.
Read `docs/ROADMAP.md` ("Next session starts here") and `docs/STATUS.md`
before starting; update both when a milestone moves.

## Commands

- Build image: `python tools/mbuild.py build`
- Self tests in QEMU (must pass before committing): `python tools/mbuild.py test`
- Also check the release build: `python tools/mbuild.py test --release`
- Boot screenshot: `python tools/mbuild.py screenshot` → `build/screenshot.png` (waits for "desktop: ready")
- Desktop walk-through: `python tools/mbuild.py ui` (default steps in `UI_STEPS`, screenshots in `build/ui/`); custom: `ui "click X Y" "type ..." "shot NAME"`
- Interactive: `python tools/mbuild.py run` (QEMU window + serial shell)
- Format: `cargo fmt --all` (CI enforces `cargo fmt --check`)

## Hard rules (from the project brief)

- The kernel is original code. Never use Linux/NT/BSD/ReactOS kernel code.
- No fake features, no placeholder success returns, no invented test results.
- Never touch real disks; QEMU only. Build outputs stay in `build/` and `target/`.
- No ads/telemetry. Record any third-party code in `docs/LICENSES.md`.

## Layout

- `boot/` UEFI boot manager (own UEFI bindings), `shared/bootinfo/` handoff ABI
- `kernel/src/{arch/x86_64,hal,ke,mm,ob,ps,io,fs,drivers}` + `shell.rs`, `selftest.rs`, `panic.rs`
- Storage writes only on Meridian data partitions (GPT type 7A3E9C42-…, `io::partition::MERIDIAN_DATA_TYPE`); C: stays read-only. AHCI in `drivers/ahci.rs`, FAT32 in `fs/fat.rs`
- Data disk D: = `build/data.img` (kept between runs, never overwritten by builds; `clean` keeps it). Tests use fresh `build/<command>-data.img` and `tools/fatcheck.py` checks them
- `run-meridian.cmd` is the one-click way for the user to try a release build; `demo-meridian.cmd` / `mbuild.py demo` types a guided tour into a visible window (steps in `DEMO_STEPS`)
- Don't rebuild while the user has a Meridian window open: `build/meridian.img` is in use by that VM
- `shared/abi/` syscall numbers + NTSTATUS; `runtime/rt/` native user runtime (has a heap: `alloc` works); `system/init/` first program
- `system/desktop/` the desktop (user mode, Rust): `main.rs` shell (windows, taskbar, Start, menus, toasts, lock), `apps.rs` App trait + Terminal/TaskManager/Settings/About, one file per other app, `widgets.rs`, `bmp.rs`, `gfx.rs`, `font.rs` (TrueType), `ui.rs`; started by the kernel from `C:\MERIDIAN\DESKTOP.EXE` unless `nodesktop`
- Selawik has no glyphs for many symbols (√ ⌫ − arrows ✓): draw them as vector shapes
- `exec`/`demo` boot with `nodesktop` (they type into the kernel shell)
- User programs: PE32+ via `cargo rustc --target x86_64-pc-windows-msvc` + rust-lld (see `USER_PROGRAMS` in mbuild.py)
- `tools/mbuild.py` build/run/test/screenshot; writes GPT+FAT32 image itself
- Languages: Rust (kernel/services/desktop), C (Win32 DLLs, later), C++ (COM/DirectX layers, later)

## Conventions

- Rust 2024, no third-party crates, `// SAFETY:` on every unsafe block.
- IRQL = CR8; spin locks raise to DISPATCH; no pool allocation above DISPATCH.
- Interrupt vector class (vector >> 4) == handler IRQL; see `arch::x86_64::vectors`.
- New kernel features get a self test in `kernel/src/selftest.rs`.
