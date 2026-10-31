//! Hardware abstraction layer: platform firmware tables, timers, buses and
//! power control. Code above this layer should not touch chipset registers
//! directly.

pub mod acpi;
pub mod hpet;
pub mod pci;
pub mod power;
pub mod serial;
pub mod timer;
