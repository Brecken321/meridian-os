# Security

## Model

* **Strict kernel/user separation.** Applications will run in isolated
  user-mode address spaces; the kernel half is supervisor-only.
* **W^X everywhere.** Kernel code is read-execute, data is non-executable,
  the physical memory direct map is non-executable.
* **Hardware hardening is always on when available:** NX, SMEP (kernel never
  executes user pages), SMAP (kernel never touches user pages except through
  audited copy routines with fault fixups), UMIP, write-protect in ring 0.
* **Guard pages** below every kernel stack.
* **No identity map** after boot: NULL and low-address dereferences fault.
* **Handles with access checks** *(planned)*: every object access goes
  through a handle carrying granted rights.
* **Drivers out of the kernel** where practical *(planned)*.
* **Fail closed.** Unimplemented functionality returns an error, never fake
  success. Double frees and corrupted kernel state stop the system rather
  than continue in an unknown state.

## Planned

KASLR, IOMMU (VT-d/AMD-Vi) DMA protection, Secure Boot with a Meridian
signing key and TPM 2.0 measured boot, signed updates and packages, disk
encryption, firewall, per-application permissions, system call and file
format fuzzing in CI.

## Privacy

Meridian contains no telemetry, advertising identifiers, sponsored content or
forced accounts, and never will. Any future optional diagnostics will be
off by default, documented, and inspectable before sending.

## Reporting a vulnerability

Please report security issues privately through GitHub's
"Report a vulnerability" feature on the repository's Security tab rather than
in a public issue.
