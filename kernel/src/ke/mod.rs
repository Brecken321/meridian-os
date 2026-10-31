//! Kernel core ("Ke"): IRQL, time keeping and per-processor state.
//! The scheduler and dispatcher objects will live here as well.

pub mod clock;
pub mod irql;
pub mod pcr;
