# Meridian OS — notes for development sessions

Original Windows-first OS with its own NT-inspired hybrid kernel in Rust.
Read `docs/ROADMAP.md` ("Next session starts here") and `docs/STATUS.md`
before starting; update both when a milestone moves.

## Commands

- Build image: `python tools/mbuild.py build`
- Self tests in QEMU (must pass before committing): `python tools/mbuild.py test`
- Also check the release build: `python tools/mbuild.py test --release`
- Boot screenshot: `python tools/mbuild.py screenshot` → `build/screenshot.png`
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
- Storage is read-only by design (no write path exists); AHCI in `drivers/ahci.rs`, FAT32 in `fs/fat.rs`
- `run-meridian.cmd` is the one-click way for the user to try a release build; `demo-meridian.cmd` / `mbuild.py demo` types a guided tour into a visible window (steps in `DEMO_STEPS`)
- Don't rebuild while the user has a Meridian window open: `build/meridian.img` is in use by that VM
- `shared/abi/` syscall numbers + NTSTATUS; `runtime/rt/` native user runtime; `system/init/` first program
- User programs: PE32+ via `cargo rustc --target x86_64-pc-windows-msvc` + rust-lld (see `USER_PROGRAMS` in mbuild.py)
- `tools/mbuild.py` build/run/test/screenshot; writes GPT+FAT32 image itself
- Languages: Rust (kernel/services), C (Win32 DLLs, later), C++ (desktop/COM, later)

## Conventions

- Rust 2024, no third-party crates, `// SAFETY:` on every unsafe block.
- IRQL = CR8; spin locks raise to DISPATCH; no pool allocation above DISPATCH.
- Interrupt vector class (vector >> 4) == handler IRQL; see `arch::x86_64::vectors`.
- New kernel features get a self test in `kernel/src/selftest.rs`.
