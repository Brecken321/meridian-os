# Windows application compatibility

**Current state: not implemented.** No Windows program runs on Meridian yet.
This document is the plan and the rules.

## Approach

Meridian runs Windows programs natively, not in a virtual machine and not
through Linux:

1. **PE32+ loader** in the kernel/user boundary: parse headers, map sections
   with correct protections, apply base relocations, resolve imports
   (including forwarders, delay-load and API sets), run TLS callbacks, set
   up the process environment (PEB/TEB), stacks and heaps.
2. **Native API layer (`ntdll`)**: a thin library that turns `Nt*`/`Zw*`
   calls into Meridian system calls. Meridian's kernel objects (processes,
   threads, sections, events, files…) are designed so these map directly.
3. **Win32 subsystem DLLs** written from scratch in C: `kernelbase`,
   `kernel32`, `advapi32` (registry, security), `user32`/`gdi32` (windows,
   messages, drawing on the Meridian compositor), `shell32`, `ole32`/COM,
   `ws2_32`, common controls and dialogs, audio and multimedia APIs.
4. **Win32 server** (user mode): window stations/desktops, console hosting,
   per-session state.

### Rules

* Implementations come from public documentation (Microsoft Learn, the PE/COFF
  specification, published books) and black-box testing of behaviour.
* No Microsoft source code (leaked or otherwise) is used or consulted. No
  Microsoft DLLs are redistributed. Users may not need any Windows files.
* Wine (LGPL-2.1) is a valuable reference for behaviour that is undocumented.
  Copying Wine code into Meridian's permissively licensed tree is not allowed;
  if any Wine component is ever reused it will be as a separately licensed,
  dynamically linked library with its licence obligations met, and recorded
  in [LICENSES.md](LICENSES.md).
* No stub returns fake success. An unimplemented API fails with
  `ERROR_CALL_NOT_IMPLEMENTED`/`STATUS_NOT_IMPLEMENTED` and is logged, so the
  compatibility database can rank what to implement next.

## 32-bit programs

Planned after 64-bit support works, using the 32-bit user code segment that
is already reserved in the GDT (`0x23`) and thunking DLLs (WoW64-style).

## Installers

`.exe` installers (NSIS, Inno Setup, InstallShield…) are ordinary Win32
programs and will work when the APIs they use work. `.msi` needs an
independent Windows Installer service implementation; `.msix` needs package
deployment APIs. Both are tracked as separate milestones.

## Compatibility database (planned)

Stored in `tests/compat/apps.toml`, one entry per application version:

```toml
[[app]]
name = "Example App"
version = "1.2.3"
category = "utility"
installs = "untested"     # untested | fails | works
launches = "untested"
basic_function = "untested"
apis_missing = []
known_problems = []
last_tested = ""          # YYYY-MM-DD, only when actually tested
```

The status page will be generated from this file. It will list only results
that were actually observed; "launches" and "works" are separate columns,
and no aggregate compatibility percentage is published until there is a
statistically meaningful, representative test set.

## Milestone order

1. Console "hello world" built with the Meridian SDK as a PE32+ (native).
2. Console Win32 test programs compiled from source (C, MSVC and MinGW).
3. Small real-world console tools.
4. GUI test programs (window, message loop, GDI drawing, common dialogs).
5. Small real GUI applications; then installers.
6. Larger applications by category (see the brief): browsers, office,
   creative tools, IDEs, launchers.
