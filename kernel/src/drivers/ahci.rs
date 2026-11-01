//! AHCI (Serial ATA) host controller driver, written from the AHCI 1.3.1
//! and ATA/ATAPI-8 specifications.
//!
//! Current scope: ATA disks, read-only. Each port issues one command at a
//! time (slot 0) and polls for completion; reads go through a 64 KiB DMA
//! bounce buffer. Interrupt-driven operation, native command queuing and
//! writes are planned. ATAPI (optical) devices are detected and skipped.

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{Ordering, fence};

use crate::hal::pci::{self, PciDevice};
use crate::io::block::{self, BlockDevice, IoError};
use crate::ke::objects::Mutex;
use crate::ke::{clock, sched};
use crate::mm::{PAGE_SIZE, kva, phys_to_virt, pmm};

const HBA_CAP: u64 = 0x00;
const HBA_GHC: u64 = 0x04;
const HBA_PI: u64 = 0x0C;
const HBA_VS: u64 = 0x10;
const HBA_CAP2: u64 = 0x24;
const HBA_BOHC: u64 = 0x28;
const GHC_AE: u32 = 1 << 31;
const GHC_IE: u32 = 1 << 1;
const CAP_S64A: u32 = 1 << 31;
const CAP_SSS: u32 = 1 << 27;
const CAP2_BOH: u32 = 1 << 0;
const BOHC_BOS: u32 = 1 << 0;
const BOHC_OOS: u32 = 1 << 1;
const BOHC_BB: u32 = 1 << 4;

const PX_CLB: u64 = 0x00;
const PX_CLBU: u64 = 0x04;
const PX_FB: u64 = 0x08;
const PX_FBU: u64 = 0x0C;
const PX_IS: u64 = 0x10;
const PX_IE: u64 = 0x14;
const PX_CMD: u64 = 0x18;
const PX_TFD: u64 = 0x20;
const PX_SIG: u64 = 0x24;
const PX_SSTS: u64 = 0x28;
const PX_SERR: u64 = 0x30;
const PX_CI: u64 = 0x38;
const CMD_ST: u32 = 1 << 0;
const CMD_SUD: u32 = 1 << 1;
const CMD_POD: u32 = 1 << 2;
const CMD_FRE: u32 = 1 << 4;
const CMD_FR: u32 = 1 << 14;
const CMD_CR: u32 = 1 << 15;
const TFD_ERR: u32 = 1 << 0;
const TFD_DRQ: u32 = 1 << 3;
const TFD_BSY: u32 = 1 << 7;
const IS_TFES: u32 = 1 << 30;
const SIG_ATA: u32 = 0x0000_0101;
const SIG_ATAPI: u32 = 0xEB14_0101;

const ATA_IDENTIFY: u8 = 0xEC;
const ATA_READ_DMA_EXT: u8 = 0x25;
const FIS_REG_H2D: u8 = 0x27;

/// DMA bounce buffer size (and so the largest single read command).
const CHUNK_BYTES: usize = 64 * 1024;
/// Offsets inside the per-port control page.
const FIS_OFFSET: u64 = 1024;
const TABLE_OFFSET: u64 = 2048;

#[derive(Clone, Copy)]
struct Regs(u64);

impl Regs {
    fn read(self, reg: u64) -> u32 {
        // SAFETY: `self.0` maps the HBA register block uncached.
        unsafe { read_volatile((self.0 + reg) as *const u32) }
    }

    fn write(self, reg: u64, value: u32) {
        // SAFETY: as in `read`.
        unsafe { write_volatile((self.0 + reg) as *mut u32, value) }
    }

    /// Waits up to `ms` for `(read(reg) & mask) == want`.
    fn wait(self, reg: u64, mask: u32, want: u32, ms: u64) -> bool {
        let deadline = clock::uptime_us() + ms * 1000;
        loop {
            if self.read(reg) & mask == want {
                return true;
            }
            if clock::uptime_us() > deadline {
                return false;
            }
            core::hint::spin_loop();
        }
    }
}

pub struct AhciDisk {
    name: String,
    model: String,
    port: Regs,
    sector_size: usize,
    sectors: u64,
    /// Serialises commands on this port (slot 0 is the only slot used).
    lock: Mutex,
    /// Physical page holding the command list, received-FIS area and the
    /// command table.
    control: u64,
    /// Physical address of the DMA bounce buffer.
    buffer: u64,
}

impl AhciDisk {
    /// Issues one ATA command whose data (if any) lands in the bounce buffer.
    fn issue(&self, command: u8, lba: u64, count: u16, bytes: usize) -> Result<(), IoError> {
        let page = phys_to_virt(self.control);
        let table = page + TABLE_OFFSET;
        // SAFETY: the control page belongs to this port and only the
        // lock holder touches it; the HBA is idle (CI clear) between commands.
        unsafe {
            core::ptr::write_bytes(table as *mut u8, 0, 0x80 + 16);
            let fis = table as *mut u8;
            let device = if command == ATA_IDENTIFY { 0 } else { 1 << 6 }; // LBA mode
            let bytes_fis = [
                FIS_REG_H2D,
                0x80, // command (not control) register update
                command,
                0,
                lba as u8,
                (lba >> 8) as u8,
                (lba >> 16) as u8,
                device,
                (lba >> 24) as u8,
                (lba >> 32) as u8,
                (lba >> 40) as u8,
                0,
                count as u8,
                (count >> 8) as u8,
                0,
                0,
            ];
            for (i, b) in bytes_fis.iter().enumerate() {
                write_volatile(fis.add(i), *b);
            }
            let prdt = (table + 0x80) as *mut u32;
            write_volatile(prdt, self.buffer as u32);
            write_volatile(prdt.add(1), (self.buffer >> 32) as u32);
            write_volatile(prdt.add(2), 0);
            write_volatile(prdt.add(3), (bytes - 1) as u32);

            let header = page as *mut u32;
            // CFL = 5 dwords (a register FIS), one PRDT entry, read.
            write_volatile(header, 5 | (1 << 16));
            write_volatile(header.add(1), 0);
            let table_phys = self.control + TABLE_OFFSET;
            write_volatile(header.add(2), table_phys as u32);
            write_volatile(header.add(3), (table_phys >> 32) as u32);
        }
        fence(Ordering::SeqCst);

        let port = self.port;
        if !port.wait(PX_TFD, TFD_BSY | TFD_DRQ, 0, 1000) {
            return Err(IoError::Timeout);
        }
        port.write(PX_IS, u32::MAX);
        port.write(PX_CI, 1);
        let deadline = clock::uptime_us() + 5_000_000;
        let mut spins = 0u32;
        loop {
            if port.read(PX_IS) & IS_TFES != 0 {
                return Err(IoError::DeviceError);
            }
            if port.read(PX_CI) & 1 == 0 {
                break;
            }
            if clock::uptime_us() > deadline {
                return Err(IoError::Timeout);
            }
            spins += 1;
            if spins % 64 == 0 {
                sched::yield_now();
            } else {
                core::hint::spin_loop();
            }
        }
        fence(Ordering::SeqCst);
        if port.read(PX_TFD) & TFD_ERR != 0 {
            return Err(IoError::DeviceError);
        }
        // SAFETY: as above; PRDBC (bytes transferred) is in header dword 1.
        let transferred = unsafe { read_volatile((page as *const u32).add(1)) } as usize;
        if transferred != bytes {
            return Err(IoError::DeviceError);
        }
        Ok(())
    }

    fn with_lock<R>(&self, f: impl FnOnce() -> R) -> R {
        self.lock.acquire(None);
        let r = f();
        self.lock.release();
        r
    }

    fn identify(&mut self) -> Result<(), IoError> {
        self.with_lock(|| self.issue(ATA_IDENTIFY, 0, 0, 512))?;
        let mut id = [0u16; 256];
        // SAFETY: the bounce buffer holds the 512-byte IDENTIFY data.
        unsafe {
            let p = phys_to_virt(self.buffer) as *const u16;
            for (i, w) in id.iter_mut().enumerate() {
                *w = read_volatile(p.add(i));
            }
        }
        let lba48 = id[83] & (1 << 10) != 0;
        self.sectors = if lba48 {
            id[100] as u64 | (id[101] as u64) << 16 | (id[102] as u64) << 32 | (id[103] as u64) << 48
        } else {
            id[60] as u64 | (id[61] as u64) << 16
        };
        // Word 106: bit 14 set and bit 15 clear = valid; bit 12 = logical
        // sector larger than 512 bytes, size in words 117-118.
        let w106 = id[106];
        self.sector_size = if w106 & 0xC000 == 0x4000 && w106 & (1 << 12) != 0 {
            ((id[117] as usize) | (id[118] as usize) << 16) * 2
        } else {
            512
        };
        if !lba48 || self.sectors == 0 || !self.sector_size.is_power_of_two() || self.sector_size > CHUNK_BYTES {
            return Err(IoError::DeviceError);
        }
        let mut model = String::new();
        for w in &id[27..47] {
            for b in [(w >> 8) as u8, *w as u8] {
                if b.is_ascii_graphic() || b == b' ' {
                    model.push(b as char);
                }
            }
        }
        self.model = String::from(model.trim());
        Ok(())
    }
}

impl BlockDevice for AhciDisk {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> String {
        format!("{} (SATA)", self.model)
    }

    fn sector_size(&self) -> usize {
        self.sector_size
    }

    fn sector_count(&self) -> u64 {
        self.sectors
    }

    fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> Result<(), IoError> {
        block::check_request(self, lba, buf.len())?;
        let ss = self.sector_size;
        self.with_lock(|| {
            let mut done = 0;
            while done < buf.len() {
                let n = (buf.len() - done).min(CHUNK_BYTES);
                let count = (n / ss) as u16;
                self.issue(ATA_READ_DMA_EXT, lba + (done / ss) as u64, count, n)?;
                // SAFETY: the device just wrote `n` bytes to the buffer.
                unsafe {
                    core::ptr::copy_nonoverlapping(phys_to_virt(self.buffer) as *const u8, buf[done..].as_mut_ptr(), n)
                };
                done += n;
            }
            Ok(())
        })
    }
}

/// Takes ownership of the controller from the firmware (BIOS/OS handoff).
fn take_ownership(hba: Regs) {
    if hba.read(HBA_CAP2) & CAP2_BOH == 0 {
        return;
    }
    hba.write(HBA_BOHC, hba.read(HBA_BOHC) | BOHC_OOS);
    // The firmware has 25 ms to release the controller, or 2 s if busy.
    if !hba.wait(HBA_BOHC, BOHC_BOS, 0, 25) && hba.read(HBA_BOHC) & BOHC_BB != 0 {
        hba.wait(HBA_BOHC, BOHC_BOS, 0, 2000);
    }
}

fn stop_port(port: Regs) -> bool {
    port.write(PX_CMD, port.read(PX_CMD) & !CMD_ST);
    if !port.wait(PX_CMD, CMD_CR, 0, 500) {
        return false;
    }
    port.write(PX_CMD, port.read(PX_CMD) & !CMD_FRE);
    port.wait(PX_CMD, CMD_FR, 0, 500)
}

fn init_port(hba: Regs, cap: u32, index: u32) -> Result<Option<AhciDisk>, &'static str> {
    let port = Regs(hba.0 + 0x100 + index as u64 * 0x80);
    if port.read(PX_SSTS) & 0xF != 3 {
        return Ok(None); // no device, or the link is not up
    }
    match port.read(PX_SIG) {
        SIG_ATA => {}
        SIG_ATAPI => {
            crate::info!("ahci: port {index}: optical (ATAPI) drive, not supported yet");
            return Ok(None);
        }
        other => {
            crate::info!("ahci: port {index}: unsupported device signature {other:#x}");
            return Ok(None);
        }
    }
    if !stop_port(port) {
        return Err("port did not stop");
    }

    let limit = if cap & CAP_S64A != 0 { u64::MAX } else { 1 << 32 };
    let control = pmm::alloc_zeroed_frame_below(limit).ok_or("out of memory")?;
    let buffer = pmm::alloc_contiguous((CHUNK_BYTES as u64) / PAGE_SIZE, 1).ok_or("out of memory")?;
    if buffer + CHUNK_BYTES as u64 > limit {
        return Err("controller cannot reach its DMA buffer (32-bit only)");
    }
    port.write(PX_CLB, control as u32);
    port.write(PX_CLBU, (control >> 32) as u32);
    port.write(PX_FB, (control + FIS_OFFSET) as u32);
    port.write(PX_FBU, ((control + FIS_OFFSET) >> 32) as u32);
    port.write(PX_SERR, u32::MAX);
    port.write(PX_IS, u32::MAX);
    port.write(PX_IE, 0);
    if cap & CAP_SSS != 0 {
        port.write(PX_CMD, port.read(PX_CMD) | CMD_SUD | CMD_POD);
    }
    port.write(PX_CMD, port.read(PX_CMD) | CMD_FRE);
    if !port.wait(PX_TFD, TFD_BSY | TFD_DRQ, 0, 1000) {
        return Err("device stayed busy");
    }
    port.write(PX_CMD, port.read(PX_CMD) | CMD_ST);

    let mut disk = AhciDisk {
        name: format!("Harddisk{}", block::disk_count()),
        model: String::new(),
        port,
        sector_size: 512,
        sectors: 0,
        lock: Mutex::new(),
        control,
        buffer,
    };
    disk.identify().map_err(|_| "IDENTIFY DEVICE failed")?;
    Ok(Some(disk))
}

fn init_controller(dev: &PciDevice) -> Result<usize, &'static str> {
    let abar = dev.bars[5] as u64 & !0xF;
    if dev.bars[5] & 1 != 0 || abar == 0 {
        return Err("BAR5 is not a memory BAR");
    }
    dev.enable_bus_mastering();
    let hba = Regs(kva::map_mmio(abar, 0x1100).ok_or("cannot map controller registers")?);
    take_ownership(hba);
    hba.write(HBA_GHC, (hba.read(HBA_GHC) | GHC_AE) & !GHC_IE);
    let cap = hba.read(HBA_CAP);
    let implemented = hba.read(HBA_PI);
    let version = hba.read(HBA_VS);
    crate::info!(
        "ahci: controller {:02x}:{:02x}.{} AHCI {}.{}, {} ports implemented",
        dev.bus,
        dev.device,
        dev.function,
        version >> 16,
        (version >> 8) & 0xFF,
        implemented.count_ones()
    );
    let mut found = 0;
    for index in 0..32 {
        if implemented & (1 << index) == 0 {
            continue;
        }
        match init_port(hba, cap, index) {
            Ok(Some(disk)) => {
                crate::info!(
                    "ahci: port {index}: {} \"{}\", {} MiB ({}-byte sectors)",
                    disk.name,
                    disk.model,
                    disk.sectors * disk.sector_size as u64 >> 20,
                    disk.sector_size
                );
                block::register_disk(Arc::new(disk));
                found += 1;
            }
            Ok(None) => {}
            Err(e) => crate::warn!("ahci: port {index}: {e}"),
        }
    }
    Ok(found)
}

/// Initialises every AHCI controller on the PCI bus. Returns the number of
/// disks found.
pub fn probe() -> usize {
    let mut disks = 0;
    for dev in pci::devices().iter().filter(|d| d.class == 0x01 && d.subclass == 0x06 && d.prog_if == 0x01) {
        match init_controller(dev) {
            Ok(n) => disks += n,
            Err(e) => crate::warn!("ahci: controller {:02x}:{:02x}.{}: {e}", dev.bus, dev.device, dev.function),
        }
    }
    disks
}
