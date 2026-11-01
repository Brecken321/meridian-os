//! PCI bus enumeration through the legacy configuration mechanism
//! (ports 0xCF8/0xCFC), which every PC chipset supports. Extended (PCIe)
//! configuration space via ECAM/MCFG comes with the first driver that needs it.

use alloc::vec::Vec;

use crate::arch::x86_64::cpu::{inl, outl};
use crate::sync::{Once, SpinLock};

const CONFIG_ADDRESS: u16 = 0xCF8;
const CONFIG_DATA: u16 = 0xCFC;

#[derive(Clone, Copy, Debug)]
pub struct PciDevice {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub revision: u8,
    pub header_type: u8,
    pub interrupt_line: u8,
    pub interrupt_pin: u8,
    pub bars: [u32; 6],
}

impl PciDevice {
    pub fn class_name(&self) -> &'static str {
        class_name(self.class, self.subclass, self.prog_if)
    }

    pub fn vendor_name(&self) -> &'static str {
        vendor_name(self.vendor_id)
    }

    /// Enables memory-space decoding and bus mastering (DMA) for the device.
    pub fn enable_bus_mastering(&self) {
        let reg = read32(self.bus, self.device, self.function, 0x04);
        // Keep the command bits; write zeros to the status half (its bits
        // are write-one-to-clear).
        write32(self.bus, self.device, self.function, 0x04, (reg & 0xFFFF) | 0b110);
    }
}

static CONFIG_LOCK: SpinLock<()> = SpinLock::new(());
static DEVICES: Once<Vec<PciDevice>> = Once::new();

fn address(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000 | (bus as u32) << 16 | (device as u32) << 11 | (function as u32) << 8 | (offset as u32 & 0xFC)
}

pub fn read32(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    let _guard = CONFIG_LOCK.lock();
    // SAFETY: the address/data port pair is serialised by CONFIG_LOCK.
    unsafe {
        outl(CONFIG_ADDRESS, address(bus, device, function, offset));
        inl(CONFIG_DATA)
    }
}

pub fn write32(bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    let _guard = CONFIG_LOCK.lock();
    // SAFETY: as in `read32`.
    unsafe {
        outl(CONFIG_ADDRESS, address(bus, device, function, offset));
        outl(CONFIG_DATA, value);
    }
}

fn probe(bus: u8, device: u8, function: u8) -> Option<PciDevice> {
    let id = read32(bus, device, function, 0x00);
    let vendor_id = id as u16;
    if vendor_id == 0xFFFF {
        return None;
    }
    let class = read32(bus, device, function, 0x08);
    let header = read32(bus, device, function, 0x0C);
    let irq = read32(bus, device, function, 0x3C);
    let header_type = (header >> 16) as u8;
    let mut bars = [0u32; 6];
    let bar_count = match header_type & 0x7F {
        0 => 6,
        1 => 2,
        _ => 0,
    };
    for (i, bar) in bars.iter_mut().enumerate().take(bar_count) {
        *bar = read32(bus, device, function, 0x10 + 4 * i as u8);
    }
    Some(PciDevice {
        bus,
        device,
        function,
        vendor_id,
        device_id: (id >> 16) as u16,
        class: (class >> 24) as u8,
        subclass: (class >> 16) as u8,
        prog_if: (class >> 8) as u8,
        revision: class as u8,
        header_type,
        interrupt_line: irq as u8,
        interrupt_pin: (irq >> 8) as u8,
        bars,
    })
}

/// Scans every bus/device/function once and caches the result.
pub fn enumerate() -> &'static [PciDevice] {
    DEVICES.call_once(|| {
        let mut found = Vec::new();
        for bus in 0..=255u8 {
            for device in 0..32u8 {
                let Some(first) = probe(bus, device, 0) else { continue };
                let multifunction = first.header_type & 0x80 != 0;
                found.push(first);
                if multifunction {
                    found.extend((1..8).filter_map(|f| probe(bus, device, f)));
                }
            }
        }
        found
    })
}

pub fn devices() -> &'static [PciDevice] {
    DEVICES.get().map(|v| v.as_slice()).unwrap_or(&[])
}

pub fn vendor_name(id: u16) -> &'static str {
    match id {
        0x8086 => "Intel",
        0x1022 => "AMD",
        0x1002 => "AMD/ATI",
        0x10DE => "NVIDIA",
        0x1AF4 => "Red Hat (virtio)",
        0x1B36 => "Red Hat (QEMU)",
        0x1234 => "QEMU",
        0x15AD => "VMware",
        0x80EE => "VirtualBox",
        0x10EC => "Realtek",
        0x14E4 => "Broadcom",
        0x168C => "Qualcomm Atheros",
        0x144D => "Samsung",
        0x1C5C => "SK hynix",
        0x15B7 => "Western Digital",
        0x1E0F => "KIOXIA",
        0x1B21 => "ASMedia",
        0x1912 => "Renesas",
        0x8087 => "Intel",
        _ => "unknown vendor",
    }
}

pub fn class_name(class: u8, subclass: u8, prog_if: u8) -> &'static str {
    match (class, subclass, prog_if) {
        (0x01, 0x01, _) => "IDE controller",
        (0x01, 0x06, 0x01) => "SATA controller (AHCI)",
        (0x01, 0x06, _) => "SATA controller",
        (0x01, 0x08, 0x02) => "NVMe controller",
        (0x01, 0x00, _) => "SCSI controller",
        (0x01, _, _) => "storage controller",
        (0x02, 0x00, _) => "Ethernet controller",
        (0x02, 0x80, _) => "network controller",
        (0x02, _, _) => "network controller",
        (0x03, 0x00, _) => "VGA-compatible display",
        (0x03, _, _) => "display controller",
        (0x04, 0x03, _) => "HD Audio controller",
        (0x04, _, _) => "multimedia controller",
        (0x05, _, _) => "memory controller",
        (0x06, 0x00, _) => "host bridge",
        (0x06, 0x01, _) => "ISA bridge",
        (0x06, 0x04, _) => "PCI-to-PCI bridge",
        (0x06, _, _) => "bridge",
        (0x07, _, _) => "communication controller",
        (0x08, _, _) => "system peripheral",
        (0x09, _, _) => "input controller",
        (0x0C, 0x03, 0x30) => "USB controller (xHCI)",
        (0x0C, 0x03, 0x20) => "USB controller (EHCI)",
        (0x0C, 0x03, 0x10) => "USB controller (OHCI)",
        (0x0C, 0x03, 0x00) => "USB controller (UHCI)",
        (0x0C, 0x05, _) => "SMBus controller",
        (0x0C, _, _) => "serial bus controller",
        (0x0D, _, _) => "wireless controller",
        _ => "other device",
    }
}
