//! In-kernel device drivers. These are the boot-critical drivers that must
//! work before user mode exists; most future drivers are planned to run in
//! isolated user-mode driver hosts (see docs/ARCHITECTURE.md).

pub mod fbcon;
pub mod ps2;
