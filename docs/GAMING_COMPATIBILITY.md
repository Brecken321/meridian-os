# Gaming compatibility

**Current state: nothing gaming-related runs yet.** There is no GPU driver,
no Vulkan, no DirectX translation and no Steam support. This document records
the plan, the research behind it, and the design decisions already made for
gaming.

## Decisions already in the kernel

* **IRQL-based preemption control and a 1 kHz clock** keep latency low; the
  planned scheduler has real-time priorities (16–31) for audio and input
  threads and per-CPU run queues for multicore games.
* **Large contiguous allocations and an MMIO mapping service** exist for GPU
  command rings and DMA buffers.
* **Calibrated TSC time source** for frame pacing and high-resolution timers
  (`QueryPerformanceCounter`).

## Graphics stack plan

```
Game (Direct3D 9/10/11/12, Vulkan, OpenGL)
  │
  ├─ D3D9–11 → DXVK (zlib licence)         ┐
  ├─ D3D12   → VKD3D-Proton (LGPL-2.1)     ├→ Vulkan loader → Vulkan driver (user mode)
  ├─ OpenGL  → Mesa Zink or native         ┘        │
  └─ Vulkan  ─────────────────────────────────────→ │
                                                    ▼
                      Meridian GPU kernel interface (memory, command
                      submission, sync objects, display/modesetting)
                                                    │
                                    GPU kernel driver (per vendor)
```

* **Vulkan drivers:** Mesa's RADV (AMD), ANV (Intel) and NVK (NVIDIA) are
  MIT-licensed user-mode drivers. They are written against Linux DRM
  interfaces; porting them means implementing a Meridian kernel GPU interface
  that offers equivalent buffer-object, command-submission and
  synchronisation primitives — the kernel drivers themselves must be written
  for Meridian (Linux's amdgpu/i915/xe/nouveau are GPL and cannot be copied
  into Meridian's permissively licensed kernel). This is the single largest
  engineering item for gaming.
* **NVIDIA proprietary drivers** cannot be used without NVIDIA's cooperation.
  NVK + the GSP firmware route (as on Linux) is the realistic open path for
  modern NVIDIA GPUs.
* **DXVK / VKD3D-Proton** are Windows-API-facing libraries that already run
  on Windows; they need a working Win32 subsystem and a Vulkan driver, not a
  Linux kernel. VKD3D-Proton's LGPL licence is compatible with being shipped
  as a separate dynamically linked library.
* Before hardware GPUs: a software Vulkan implementation (Mesa lavapipe, MIT)
  can validate the whole stack on the framebuffer.

## Steam

The Windows Steam client needs, at minimum: a robust Win32 subsystem
(threads, files, registry, named pipes, services), networking with TLS,
Chromium Embedded Framework (`steamwebhelper`, which itself is a sandboxed
multi-process Chromium), Direct3D/DXGI for overlays, and HID for Steam Input.
Meridian will run the **real, unmodified Steam client** through the Windows
compatibility subsystem; it will never ship a look-alike launcher, and never
handles Steam credentials itself. Steam support will be claimed only after
sign-in, library management, downloads and game launch are actually tested.

Other launchers (Epic, GOG Galaxy, Battle.net, EA app, Ubisoft Connect) are
ordinary Win32 applications with similar CEF/Chromium requirements and will
be tracked in the compatibility database.

## Anti-cheat

Kernel-mode anti-cheat systems (e.g. those used by some competitive
multiplayer games) load Windows kernel drivers and verify Windows internals.
Meridian will **not** fake Windows kernel interfaces to satisfy them and will
not help bypass anti-cheat. Games whose anti-cheat vendors offer a supported
non-Windows mode may work if and when those vendors support Meridian.
Affected games will be listed honestly as unsupported.

## Planned gaming features

* Gaming overlay: FPS, frame times, CPU/GPU utilisation, RAM/VRAM, temperatures.
* Controller support: XInput, DirectInput and SDL-style mappings over HID.
* Low-latency audio path with real-time scheduling.
* Game mode: background-service throttling that never disables security.

Benchmarks will be measured against a documented baseline; no performance
claims without data.
