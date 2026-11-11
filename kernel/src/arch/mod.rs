//! Architecture-specific code. Only x86-64 exists today; everything that
//! depends on the instruction set lives below this module so that other
//! architectures can be added without touching the executive.

pub mod x86_64;
