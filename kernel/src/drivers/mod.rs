//! In-kernel device drivers. These are the boot-critical drivers that must
//! work before user mode exists; most future drivers are planned to run in
//! isolated user-mode driver hosts (see docs/ARCHITECTURE.md).

pub mod ahci;
pub mod fbcon;
pub mod ps2;

/// Console input notification shared by the keyboard and serial drivers:
/// their interrupt handlers queue a DPC that sets this event, and the shell
/// waits on it.
pub mod input {
    use crate::ke::dpc::Dpc;
    use crate::ke::objects::Event;

    pub static AVAILABLE: Event = Event::new(false, false);
    static DPC: Dpc = Dpc::new(|| AVAILABLE.set());

    /// Signals that console input arrived. Safe at any IRQL.
    pub fn notify() {
        DPC.queue();
    }
}
