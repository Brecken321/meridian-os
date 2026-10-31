//! PS/2 keyboard on the i8042 controller. The controller translates the
//! keyboard's output to scancode set 1, which is decoded here using a US
//! layout. USB keyboards will be handled by the future xHCI/HID stack.

use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch::x86_64::cpu::{inb, outb};
use crate::arch::x86_64::idt::{self, TrapFrame};
use crate::arch::x86_64::{apic, ioapic, vectors};
use crate::ke::irql::HIGH_LEVEL;
use crate::sync::SpinLock;

const DATA: u16 = 0x60;
const STATUS: u16 = 0x64;
const COMMAND: u16 = 0x64;
const STATUS_OUTPUT_FULL: u8 = 1 << 0;
const STATUS_INPUT_FULL: u8 = 1 << 1;
const STATUS_AUX_DATA: u8 = 1 << 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Backspace,
    Tab,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    Function(u8),
    Modifier,
    Unknown(u16),
}

#[derive(Clone, Copy, Debug)]
pub struct KeyEvent {
    pub key: Key,
    pub pressed: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

const QUEUE_LEN: usize = 64;

struct State {
    queue: [Option<KeyEvent>; QUEUE_LEN],
    head: usize,
    tail: usize,
    extended: bool,
    shift: bool,
    ctrl: bool,
    alt: bool,
    caps: bool,
}

static STATE: SpinLock<State> = SpinLock::new(State {
    queue: [None; QUEUE_LEN],
    head: 0,
    tail: 0,
    extended: false,
    shift: false,
    ctrl: false,
    alt: false,
    caps: false,
});
static PRESENT: AtomicBool = AtomicBool::new(false);

const NORMAL: &[u8; 58] = b"\0\x1B1234567890-=\x08\tqwertyuiop[]\n\0asdfghjkl;'`\0\\zxcvbnm,./\0*\0 ";
const SHIFTED: &[u8; 58] = b"\0\x1B!@#$%^&*()_+\x08\tQWERTYUIOP{}\n\0ASDFGHJKL:\"~\0|ZXCVBNM<>?\0*\0 ";

fn wait_input_clear() -> bool {
    // SAFETY: reading the i8042 status port.
    (0..100_000).any(|_| unsafe { inb(STATUS) } & STATUS_INPUT_FULL == 0)
}

fn wait_output_full() -> bool {
    (0..100_000).any(|_| unsafe { inb(STATUS) } & STATUS_OUTPUT_FULL != 0)
}

fn command(cmd: u8) -> bool {
    // SAFETY: i8042 command port.
    wait_input_clear() && {
        unsafe { outb(COMMAND, cmd) };
        true
    }
}

fn write_data(value: u8) -> bool {
    wait_input_clear() && {
        unsafe { outb(DATA, value) };
        true
    }
}

fn read_data() -> Option<u8> {
    wait_output_full().then(|| unsafe { inb(DATA) })
}

/// Initialises the controller and keyboard and routes IRQ 1 to this driver.
pub fn init() -> Result<(), &'static str> {
    // SAFETY: reading the i8042 status port; 0xFF means nothing is there.
    if unsafe { inb(STATUS) } == 0xFF {
        return Err("no i8042 controller");
    }
    command(0xAD); // disable keyboard port
    command(0xA7); // disable mouse port
    for _ in 0..16 {
        // SAFETY: draining stale bytes from the output buffer.
        if unsafe { inb(STATUS) } & STATUS_OUTPUT_FULL == 0 {
            break;
        }
        unsafe { inb(DATA) };
    }
    if !command(0x20) {
        return Err("i8042 does not accept commands");
    }
    let config = read_data().ok_or("i8042 did not return its configuration")?;
    // Interrupts off while configuring, keyboard clock enabled (bit 4 was set
    // by the 0xAD command above), mouse clock left disabled, set-1
    // translation on.
    let config = (config & !0b0001_0011) | 0b0110_0000;
    command(0x60);
    write_data(config);
    command(0xAE); // enable keyboard port
    write_data(0xF4); // keyboard: enable scanning
    if read_data() != Some(0xFA) {
        crate::warn!("ps2: keyboard did not acknowledge enable-scanning");
    }
    command(0x60);
    write_data(config | 0b0000_0001); // keyboard interrupt on

    if !idt::register(vectors::KEYBOARD, interrupt) {
        return Err("keyboard vector already in use");
    }
    if !ioapic::route_isa_irq(1, vectors::KEYBOARD, apic::id()) {
        return Err("cannot route IRQ 1");
    }
    PRESENT.store(true, Ordering::Relaxed);
    Ok(())
}

pub fn present() -> bool {
    PRESENT.load(Ordering::Relaxed)
}

fn interrupt(_frame: &mut TrapFrame) {
    let mut state = STATE.lock_at(HIGH_LEVEL);
    // SAFETY: draining the i8042 output buffer in its interrupt handler.
    unsafe {
        while inb(STATUS) & STATUS_OUTPUT_FULL != 0 {
            let aux = inb(STATUS) & STATUS_AUX_DATA != 0;
            let byte = inb(DATA);
            if !aux {
                decode(&mut state, byte);
            }
        }
    }
}

fn decode(s: &mut State, byte: u8) {
    if byte == 0xE0 {
        s.extended = true;
        return;
    }
    let extended = core::mem::take(&mut s.extended);
    let pressed = byte & 0x80 == 0;
    let code = byte & 0x7F;
    let key = if extended {
        match code {
            0x48 => Key::Up,
            0x50 => Key::Down,
            0x4B => Key::Left,
            0x4D => Key::Right,
            0x47 => Key::Home,
            0x4F => Key::End,
            0x49 => Key::PageUp,
            0x51 => Key::PageDown,
            0x52 => Key::Insert,
            0x53 => Key::Delete,
            0x1C => Key::Enter,
            0x35 => Key::Char('/'),
            0x1D => {
                s.ctrl = pressed;
                Key::Modifier
            }
            0x38 => {
                s.alt = pressed;
                Key::Modifier
            }
            _ => Key::Unknown(0xE000 | code as u16),
        }
    } else {
        match code {
            0x2A | 0x36 => {
                s.shift = pressed;
                Key::Modifier
            }
            0x1D => {
                s.ctrl = pressed;
                Key::Modifier
            }
            0x38 => {
                s.alt = pressed;
                Key::Modifier
            }
            0x3A => {
                if pressed {
                    s.caps = !s.caps;
                }
                Key::Modifier
            }
            0x01 => Key::Escape,
            0x0E => Key::Backspace,
            0x0F => Key::Tab,
            0x1C => Key::Enter,
            0x3B..=0x44 => Key::Function(code - 0x3A),
            0x57 => Key::Function(11),
            0x58 => Key::Function(12),
            c if (c as usize) < NORMAL.len() && NORMAL[c as usize] != 0 => {
                let table = if s.shift { SHIFTED } else { NORMAL };
                let mut ch = table[c as usize] as char;
                if s.caps && ch.is_ascii_alphabetic() {
                    ch = if s.shift { ch.to_ascii_lowercase() } else { ch.to_ascii_uppercase() };
                }
                Key::Char(ch)
            }
            _ => Key::Unknown(code as u16),
        }
    };
    let next = (s.head + 1) % QUEUE_LEN;
    if next != s.tail {
        let event = KeyEvent { key, pressed, ctrl: s.ctrl, alt: s.alt, shift: s.shift };
        s.queue[s.head] = Some(event);
        s.head = next;
    }
}

/// Takes the oldest queued key event.
pub fn read_event() -> Option<KeyEvent> {
    let mut s = STATE.lock_at(HIGH_LEVEL);
    if s.tail == s.head {
        return None;
    }
    let tail = s.tail;
    s.tail = (tail + 1) % QUEUE_LEN;
    s.queue[tail].take()
}

/// Injects a raw scancode as if it came from the keyboard (used by tests).
pub fn inject_scancode(byte: u8) {
    decode(&mut STATE.lock_at(HIGH_LEVEL), byte);
}
