//! CMOS real-time clock: the battery-backed date and time.
//!
//! PC firmware keeps local time here (Windows convention; QEMU is started
//! with `-rtc base=localtime` to match).

use meridian_abi::WallTime;

use crate::arch::x86_64::cpu::{inb, outb};
use crate::sync::SpinLock;

static CMOS: SpinLock<()> = SpinLock::new(());

fn read(reg: u8) -> u8 {
    // SAFETY: CMOS index/data ports; bit 7 of the index keeps NMIs enabled
    // state unchanged on modern chipsets (we always write it clear).
    unsafe {
        outb(0x70, reg);
        inb(0x71)
    }
}

fn snapshot() -> [u8; 7] {
    [read(0x00), read(0x02), read(0x04), read(0x07), read(0x08), read(0x09), read(0x32)]
}

/// Reads the current date and time.
pub fn now() -> WallTime {
    let _guard = CMOS.lock();
    // Wait out an update in progress, then read until two reads agree.
    for _ in 0..10_000 {
        if read(0x0A) & 0x80 == 0 {
            break;
        }
    }
    let mut a = snapshot();
    loop {
        let b = snapshot();
        if a == b {
            break;
        }
        a = b;
    }
    let status_b = read(0x0B);
    let binary = status_b & 0x04 != 0;
    let hour24 = status_b & 0x02 != 0;
    let bcd = |v: u8| if binary { v } else { (v & 0x0F) + (v >> 4) * 10 };
    let [sec, min, hour_raw, day, month, year, century] = a;
    let pm = hour_raw & 0x80 != 0;
    let mut hour = bcd(hour_raw & 0x7F);
    if !hour24 {
        hour %= 12;
        if pm {
            hour += 12;
        }
    }
    let century = if century != 0 && bcd(century) >= 19 { bcd(century) as u16 } else { 20 };
    let year = century * 100 + bcd(year) as u16;
    let (month, day) = (bcd(month), bcd(day));
    WallTime { year, month, day, hour, minute: bcd(min), second: bcd(sec), weekday: weekday(year, month, day) }
}

/// Day of the week (0 = Sunday), Sakamoto's method.
fn weekday(year: u16, month: u8, day: u8) -> u8 {
    const T: [u16; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if month < 3 { year - 1 } else { year };
    let m = (month.clamp(1, 12) - 1) as usize;
    ((y + y / 4 - y / 100 + y / 400 + T[m] + day as u16) % 7) as u8
}
