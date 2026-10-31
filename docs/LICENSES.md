# Licences

Meridian OS's own code is licensed under the **Apache License 2.0** (see
[LICENSE](../LICENSE)).

## Third-party components included in this repository

| Component | Where | Licence | Use |
|-----------|-------|---------|-----|
| Spleen 8x16 bitmap font 2.2.0, © Frederic Cambus | `kernel/assets/font8x16.bin` (converted from `spleen-8x16.bdf` by `tools/bdf2font.py`) | BSD 2-Clause, full text in `kernel/assets/LICENSE.spleen` | Kernel console font |

No third-party Rust crates are used. No Microsoft code or binaries are
included or required.

## Third-party tools used to build or test (not distributed)

| Tool | Licence |
|------|---------|
| Rust toolchain (rustc, cargo, rust-lld, llvm-tools) | MIT / Apache-2.0 (LLVM: Apache-2.0 with LLVM exception) |
| QEMU | GPL-2.0 |
| edk2 / OVMF UEFI firmware | BSD-2-Clause-Patent |
| Python | PSF License |

## Policy for adding third-party code

1. The licence must be compatible with distributing Meridian under
   Apache-2.0. Permissive (MIT, BSD, Apache, zlib, ISC) code may be included
   with attribution. LGPL code may only be shipped as separate, dynamically
   linked libraries with source available. GPL code is not linked into the
   kernel or system libraries.
2. Record every component here: name, version, location, licence, purpose.
3. Keep the original licence text next to the component.
