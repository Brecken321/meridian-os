//! PS/2 keyboard and mouse on the i8042 controller. The controller
//! translates the keyboard's output to scancode set 1, decoded here with a
//! US layout; the mouse sends standard 3-byte packets. USB input devices
//! will be handled by the future xHCI/HID stack.
//!
//! Key events go to the kernel shell's queue, or to the user-mode input
//! queue (`drivers::input`) once a program such as the desktop has taken
//! over input. Mouse events only go to the user-mode queue.
//!
//! Under a hypervisor that offers VMware's "vmmouse" protocol (QEMU,
//! VMware, VirtualBox) the mouse is switched to absolute mode: the PS/2
//! packets then only signal that an event is waiting, and the position is
//! read from the hypervisor, so the host pointer drives the guest pointer
//! directly and is never captured by the VM window. `novmmouse` on the
//! command line keeps plain relative PS/2.

use core::arch::asm;
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
    /// The Windows/Super key.
    Super,
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
    packet: [u8; 3],
    packet_len: usize,
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
    packet: [0; 3],
    packet_len: 0,
});
static PRESENT: AtomicBool = AtomicBool::new(false);
static MOUSE_PRESENT: AtomicBool = AtomicBool::new(false);
static ABSOLUTE: AtomicBool = AtomicBool::new(false);

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

/// Sends a byte to the mouse (through the controller's aux port) and
/// returns true if it acknowledged.
fn mouse_command(value: u8) -> bool {
    command(0xD4) && write_data(value) && read_data() == Some(0xFA)
}

/// Initialises the controller, keyboard and (if present) mouse and routes
/// IRQ 1 and IRQ 12 to this driver.
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
    // Interrupts off while configuring, keyboard clock on, mouse clock off
    // for now, set-1 translation on.
    let config = (config & !0b0001_0011) | 0b0110_0000;
    command(0x60);
    write_data(config);
    command(0xAE); // enable keyboard port
    write_data(0xF4); // keyboard: enable scanning
    if read_data() != Some(0xFA) {
        crate::warn!("ps2: keyboard did not acknowledge enable-scanning");
    }

    // Mouse: enable the aux port, restore defaults, enable data reporting.
    command(0xA8);
    let mouse = mouse_command(0xF6) && mouse_command(0xF4);
    let mut final_config = config | 0b0000_0001; // keyboard interrupt on
    if mouse {
        final_config = (final_config & !0b0010_0000) | 0b0000_0010; // mouse clock + interrupt on
    }
    command(0x60);
    write_data(final_config);

    if !idt::register(vectors::KEYBOARD, interrupt) {
        return Err("keyboard vector already in use");
    }
    if !ioapic::route_isa_irq(1, vectors::KEYBOARD, apic::id()) {
        return Err("cannot route IRQ 1");
    }
    PRESENT.store(true, Ordering::Relaxed);
    if mouse && idt::register(vectors::MOUSE, interrupt) && ioapic::route_isa_irq(12, vectors::MOUSE, apic::id()) {
        MOUSE_PRESENT.store(true, Ordering::Relaxed);
        if !crate::boot_info().has_option("novmmouse") && vmmouse::enable() {
            ABSOLUTE.store(true, Ordering::Relaxed);
            crate::info!("ps2: mouse in absolute mode (vmmouse)");
        }
    } else {
        crate::info!("ps2: no PS/2 mouse");
    }
    Ok(())
}

pub fn present() -> bool {
    PRESENT.load(Ordering::Relaxed)
}

pub fn mouse_present() -> bool {
    MOUSE_PRESENT.load(Ordering::Relaxed)
}

/// True when the mouse reports absolute positions (vmmouse).
pub fn mouse_absolute() -> bool {
    ABSOLUTE.load(Ordering::Relaxed)
}

/// Shared handler for IRQ 1 and IRQ 12: the status register says whether
/// each byte came from the keyboard or the mouse.
fn interrupt(_frame: &mut TrapFrame) {
    let mut state = STATE.lock_at(HIGH_LEVEL);
    // SAFETY: draining the i8042 output buffer in its interrupt handler.
    unsafe {
        while inb(STATUS) & STATUS_OUTPUT_FULL != 0 {
            let aux = inb(STATUS) & STATUS_AUX_DATA != 0;
            let byte = inb(DATA);
            if aux {
                mouse_byte(&mut state, byte);
            } else {
                decode(&mut state, byte);
            }
        }
    }
    drop(state);
    super::input::notify();
}

fn mouse_byte(s: &mut State, byte: u8) {
    // The first byte of a packet always has bit 3 set; resynchronise on it.
    if s.packet_len == 0 && byte & 0x08 == 0 {
        return;
    }
    s.packet[s.packet_len] = byte;
    s.packet_len += 1;
    if s.packet_len < 3 {
        return;
    }
    s.packet_len = 0;
    if ABSOLUTE.load(Ordering::Relaxed) {
        vmmouse::drain();
        return;
    }
    let [flags, x, y] = s.packet;
    if flags & 0xC0 != 0 {
        return; // overflow: drop the packet
    }
    let dx = x as i32 - if flags & 0x10 != 0 { 256 } else { 0 };
    let dy = y as i32 - if flags & 0x20 != 0 { 256 } else { 0 };
    // PS/2 reports up as positive; Meridian uses screen coordinates.
    super::input::push_mouse(dx, -dy, (flags & 0x07) as u32);
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
            0x5B | 0x5C => Key::Super,
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
    let event = KeyEvent { key, pressed, ctrl: s.ctrl, alt: s.alt, shift: s.shift };
    if super::input::captured() {
        super::input::push_key(&event);
        return;
    }
    let next = (s.head + 1) % QUEUE_LEN;
    if next != s.tail {
        s.queue[s.head] = Some(event);
        s.head = next;
    }
}

/// Takes the oldest queued key event (kernel shell).
pub fn read_event() -> Option<KeyEvent> {
    let mut s = STATE.lock_at(HIGH_LEVEL);
    if s.tail == s.head {
        return None;
    }
    let tail = s.tail;
    s.tail = (tail + 1) % QUEUE_LEN;
    s.queue[tail].take()
}

/// VMware's absolute-pointer "backdoor": an `in` from port 0x5658 with a
/// magic value in EAX and a command in ECX, answered in EAX..EDX.
mod vmmouse {
    use super::asm;

    const PORT: u16 = 0x5658;
    const MAGIC: u32 = 0x564D_5868;
    const GET_VERSION: u32 = 10;
    const POINTER_DATA: u32 = 39;
    const POINTER_STATUS: u32 = 40;
    const POINTER_COMMAND: u32 = 41;
    const CMD_ENABLE: u32 = 0x4541_4552;
    const CMD_REQUEST_ABSOLUTE: u32 = 0x5342_4152;
    const VERSION_ID: u32 = 0x3442_554A;
    const STATUS_ERROR: u32 = 0xFFFF_0000;
    const RELATIVE_PACKET: u32 = 0x0001_0000;
    const LEFT: u32 = 0x20;
    const RIGHT: u32 = 0x10;
    const MIDDLE: u32 = 0x08;

    fn call(command: u32, arg: u32) -> [u32; 4] {
        let (eax, ecx, edx): (u32, u32, u32);
        let mut rbx = arg as u64;
        // SAFETY: the backdoor port only exists under a hypervisor; on real
        // hardware the read returns 0xFFFFFFFF and leaves EBX unchanged,
        // which `enable` treats as absent. RBX is reserved by the compiler,
        // so it is swapped in and out around the instruction.
        unsafe {
            asm!(
                "xchg {b}, rbx",
                "in eax, dx",
                "xchg {b}, rbx",
                b = inout(reg) rbx,
                inout("eax") MAGIC => eax,
                inout("ecx") command => ecx,
                inout("edx") PORT as u32 => edx,
                options(nomem, nostack, preserves_flags),
            );
        }
        [eax, rbx as u32, ecx, edx]
    }

    /// Detects the protocol and switches the pointer to absolute mode.
    pub fn enable() -> bool {
        let [eax, ebx, ..] = call(GET_VERSION, 0);
        if ebx != MAGIC || eax == u32::MAX {
            return false;
        }
        call(POINTER_COMMAND, CMD_ENABLE);
        if call(POINTER_STATUS, 0)[0] & 0xFFFF == 0 {
            return false;
        }
        if call(POINTER_DATA, 1)[0] != VERSION_ID {
            return false;
        }
        call(POINTER_COMMAND, CMD_REQUEST_ABSOLUTE);
        true
    }

    /// Moves every queued event (four words each) to the input queue.
    pub fn drain() {
        for _ in 0..16 {
            let status = call(POINTER_STATUS, 0)[0];
            if status & STATUS_ERROR == STATUS_ERROR {
                return;
            }
            let queued = status & 0xFFFF;
            if queued < 4 {
                return;
            }
            let [flags, x, y, z] = call(POINTER_DATA, 4);
            let buttons =
                (flags & LEFT != 0) as u32 | ((flags & RIGHT != 0) as u32) << 1 | ((flags & MIDDLE != 0) as u32) << 2;
            let wheel = z as u8 as i8 as i32;
            if flags & RELATIVE_PACKET != 0 {
                crate::drivers::input::push_mouse(x as i32, y as i32, buttons);
            } else {
                crate::drivers::input::push_mouse_absolute(x as i32 & 0xFFFF, y as i32 & 0xFFFF, buttons, wheel);
            }
        }
    }
}

/// Injects a raw scancode as if it came from the keyboard (used by tests).
pub fn inject_scancode(byte: u8) {
    decode(&mut STATE.lock_at(HIGH_LEVEL), byte);
}
