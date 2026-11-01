//! In-kernel device drivers. These are the boot-critical drivers that must
//! work before user mode exists; most future drivers are planned to run in
//! isolated user-mode driver hosts (see docs/ARCHITECTURE.md).

pub mod ahci;
pub mod display;
pub mod fbcon;
pub mod ps2;

/// Console input routing.
///
/// Until a user-mode program takes over input (the desktop does, through
/// the `READ_INPUT` system call), key presses go to the kernel shell.
/// Afterwards every key and mouse event is queued for that program; the
/// kernel shell keeps working over the serial port.
pub mod input {
    use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use meridian_abi::{InputEvent, input, key};

    use super::ps2::{Key, KeyEvent};
    use crate::ke::dpc::Dpc;
    use crate::ke::irql::HIGH_LEVEL;
    use crate::ke::objects::Event;
    use crate::sync::SpinLock;

    /// Set when kernel-shell input arrived (keyboard or serial).
    pub static AVAILABLE: Event = Event::new(false, false);
    /// Set when events are queued for the capturing program.
    static USER_AVAILABLE: Event = Event::new(false, false);
    static DPC: Dpc = Dpc::new(|| {
        AVAILABLE.set();
        if CAPTURED.load(Ordering::Acquire) {
            USER_AVAILABLE.set();
        }
    });

    static CAPTURED: AtomicBool = AtomicBool::new(false);
    static OWNER: AtomicU64 = AtomicU64::new(0);

    const RING: usize = 256;
    const EMPTY: InputEvent = InputEvent { kind: 0, key: 0, ch: 0, flags: 0, dx: 0, dy: 0, buttons: 0, wheel: 0 };

    struct Ring {
        events: [InputEvent; RING],
        head: usize,
        len: usize,
    }

    static QUEUE: SpinLock<Ring> = SpinLock::new(Ring { events: [EMPTY; RING], head: 0, len: 0 });

    /// Signals that input arrived. Safe at any IRQL.
    pub fn notify() {
        DPC.queue();
    }

    pub fn captured() -> bool {
        CAPTURED.load(Ordering::Acquire)
    }

    /// Routes all input to the process `pid`. Fails if another process
    /// already owns it.
    pub fn capture(pid: u64) -> bool {
        match OWNER.compare_exchange(0, pid, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => {
                CAPTURED.store(true, Ordering::Release);
                true
            }
            Err(owner) => owner == pid,
        }
    }

    /// Gives input back to the kernel shell if `pid` owns it.
    pub fn release(pid: u64) {
        if OWNER.compare_exchange(pid, 0, Ordering::AcqRel, Ordering::Acquire).is_ok() {
            CAPTURED.store(false, Ordering::Release);
            QUEUE.lock_at(HIGH_LEVEL).len = 0;
        }
    }

    fn push(event: InputEvent) {
        let mut q = QUEUE.lock_at(HIGH_LEVEL);
        if q.len == RING {
            // Full: drop the oldest event rather than the newest.
            q.head = (q.head + 1) % RING;
            q.len -= 1;
        }
        let tail = (q.head + q.len) % RING;
        q.events[tail] = event;
        q.len += 1;
    }

    /// Queues a key event for the capturing program (interrupt context).
    pub fn push_key(e: &KeyEvent) {
        let (code, ch) = match e.key {
            Key::Char(c) => (key::CHAR, c as u32),
            Key::Enter => (key::ENTER, '\n' as u32),
            Key::Backspace => (key::BACKSPACE, 0),
            Key::Tab => (key::TAB, '\t' as u32),
            Key::Escape => (key::ESCAPE, 0),
            Key::Up => (key::UP, 0),
            Key::Down => (key::DOWN, 0),
            Key::Left => (key::LEFT, 0),
            Key::Right => (key::RIGHT, 0),
            Key::Home => (key::HOME, 0),
            Key::End => (key::END, 0),
            Key::PageUp => (key::PAGE_UP, 0),
            Key::PageDown => (key::PAGE_DOWN, 0),
            Key::Insert => (key::INSERT, 0),
            Key::Delete => (key::DELETE, 0),
            Key::Function(n) => (key::FUNCTION + n as u32, 0),
            Key::Super => (key::SUPER, 0),
            Key::Modifier => (key::MODIFIER, 0),
            Key::Unknown(_) => (key::OTHER, 0),
        };
        let mut flags = 0;
        if e.pressed {
            flags |= input::PRESSED;
        }
        if e.shift {
            flags |= input::SHIFT;
        }
        if e.ctrl {
            flags |= input::CTRL;
        }
        if e.alt {
            flags |= input::ALT;
        }
        push(InputEvent { kind: input::KEY, key: code, ch, flags, ..Default::default() });
    }

    /// Queues a mouse movement/button report (interrupt context).
    pub fn push_mouse(dx: i32, dy: i32, buttons: u32) {
        if captured() {
            push(InputEvent { kind: input::MOUSE, dx, dy, buttons, ..Default::default() });
        }
    }

    /// Queues an absolute pointer position (0..=0xFFFF on each axis).
    pub fn push_mouse_absolute(x: i32, y: i32, buttons: u32, wheel: i32) {
        if captured() {
            push(InputEvent {
                kind: input::MOUSE,
                flags: input::ABSOLUTE,
                dx: x,
                dy: y,
                buttons,
                wheel,
                ..Default::default()
            });
        }
    }

    /// Copies up to `out.len()` queued events into `out`, waiting up to
    /// `timeout_ms` (`None` = forever) for at least one.
    pub fn read(out: &mut [InputEvent], timeout_ms: Option<u64>) -> usize {
        loop {
            {
                let mut q = QUEUE.lock_at(HIGH_LEVEL);
                if q.len > 0 {
                    let n = q.len.min(out.len());
                    for slot in out.iter_mut().take(n) {
                        *slot = q.events[q.head];
                        q.head = (q.head + 1) % RING;
                        q.len -= 1;
                    }
                    return n;
                }
            }
            if USER_AVAILABLE.wait(timeout_ms) == crate::ke::sched::WaitStatus::Timeout {
                return 0;
            }
        }
    }
}
