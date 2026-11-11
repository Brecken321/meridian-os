//! Partition table discovery: GUID Partition Table, with a fallback to the
//! classic MBR's four primary entries.

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;

use super::block::{self, BlockDevice, IoError};

/// EFI System Partition type GUID (C12A7328-F81F-11D2-BA4B-00A0C93EC93B) in
/// on-disk (mixed-endian) byte order.
pub const ESP_TYPE: [u8; 16] =
    [0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B];
/// Microsoft basic data partition (EBD0A0A2-B9E5-4433-87C0-68B6B72699C7).
pub const BASIC_DATA_TYPE: [u8; 16] =
    [0xA2, 0xA0, 0xD0, 0xEB, 0xE5, 0xB9, 0x33, 0x44, 0x87, 0xC0, 0x68, 0xB6, 0xB7, 0x26, 0x99, 0xC7];

/// Meridian data partition (7A3E9C42-5B1D-4E8F-9A6C-3D2B1F0E8C57), a type
/// GUID chosen for Meridian. It is the only partition type Meridian writes
/// to: no other operating system creates it, so existing disks are never
/// modified.
pub const MERIDIAN_DATA_TYPE: [u8; 16] =
    [0x42, 0x9C, 0x3E, 0x7A, 0x1D, 0x5B, 0x8F, 0x4E, 0x9A, 0x6C, 0x3D, 0x2B, 0x1F, 0x0E, 0x8C, 0x57];

const MAX_GPT_ENTRIES: u32 = 256;

pub struct Partition {
    name: String,
    disk: Arc<dyn BlockDevice>,
    first_lba: u64,
    sectors: u64,
    pub type_guid: [u8; 16],
    pub mbr_type: u8,
    pub label: String,
}

impl Partition {
    pub fn is_esp(&self) -> bool {
        self.type_guid == ESP_TYPE || self.mbr_type == 0xEF
    }

    pub fn is_meridian_data(&self) -> bool {
        self.type_guid == MERIDIAN_DATA_TYPE
    }

    fn kind(&self) -> &'static str {
        if self.is_meridian_data() {
            "Meridian data partition"
        } else if self.is_esp() {
            "EFI system partition"
        } else if self.type_guid == BASIC_DATA_TYPE || matches!(self.mbr_type, 0x07 | 0x0B | 0x0C) {
            "basic data partition"
        } else {
            "partition"
        }
    }
}

impl BlockDevice for Partition {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> String {
        if self.label.is_empty() { String::from(self.kind()) } else { format!("{} \"{}\"", self.kind(), self.label) }
    }

    fn sector_size(&self) -> usize {
        self.disk.sector_size()
    }

    fn sector_count(&self) -> u64 {
        self.sectors
    }

    fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> Result<(), IoError> {
        block::check_request(self, lba, buf.len())?;
        self.disk.read_sectors(self.first_lba + lba, buf)
    }

    fn writable(&self) -> bool {
        self.is_meridian_data() && self.disk.writable()
    }

    fn write_sectors(&self, lba: u64, buf: &[u8]) -> Result<(), IoError> {
        if !self.writable() {
            return Err(IoError::ReadOnly);
        }
        block::check_request(self, lba, buf.len())?;
        self.disk.write_sectors(self.first_lba + lba, buf)
    }

    fn flush(&self) -> Result<(), IoError> {
        if self.writable() { self.disk.flush() } else { Ok(()) }
    }
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

/// Reads `disk`'s partition table and registers each partition. Returns how
/// many were found.
pub fn scan(disk: &Arc<dyn BlockDevice>) -> Result<usize, IoError> {
    let ss = disk.sector_size();
    let mut sector = vec![0u8; ss];
    disk.read_sectors(1, &mut sector)?;
    if &sector[0..8] == b"EFI PART" {
        return scan_gpt(disk, &sector);
    }
    disk.read_sectors(0, &mut sector)?;
    if sector[510..512] == [0x55, 0xAA] {
        return Ok(scan_mbr(disk, &sector));
    }
    Ok(0)
}

fn add(
    disk: &Arc<dyn BlockDevice>,
    index: usize,
    first: u64,
    sectors: u64,
    type_guid: [u8; 16],
    mbr_type: u8,
    label: String,
) {
    if sectors == 0 || first.checked_add(sectors).is_none_or(|end| end > disk.sector_count()) {
        crate::warn!("{}: partition {} lies outside the disk; ignored", disk.name(), index);
        return;
    }
    let p = Partition {
        name: format!("{}\\Partition{}", disk.name(), index),
        disk: disk.clone(),
        first_lba: first,
        sectors,
        type_guid,
        mbr_type,
        label,
    };
    block::register_partition(Arc::new(p));
}

fn scan_gpt(disk: &Arc<dyn BlockDevice>, header: &[u8]) -> Result<usize, IoError> {
    let entries_lba = u64_at(header, 72);
    let count = u32_at(header, 80).min(MAX_GPT_ENTRIES);
    let entry_size = u32_at(header, 84) as usize;
    if entry_size < 128 || entry_size > 4096 {
        crate::warn!("{}: GPT entry size {} is invalid", disk.name(), entry_size);
        return Ok(0);
    }
    let ss = disk.sector_size();
    let bytes = (count as usize * entry_size).div_ceil(ss) * ss;
    let mut table = vec![0u8; bytes.max(ss)];
    disk.read_sectors(entries_lba, &mut table)?;
    let mut found = 0;
    for i in 0..count as usize {
        let e = &table[i * entry_size..(i + 1) * entry_size];
        let type_guid: [u8; 16] = e[0..16].try_into().unwrap();
        if type_guid == [0; 16] {
            continue;
        }
        let first = u64_at(e, 32);
        let last = u64_at(e, 40);
        let label: String = char::decode_utf16(
            e[56..128].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0),
        )
        .map(|r| r.unwrap_or('?'))
        .collect();
        found += 1;
        add(disk, found, first, last.saturating_sub(first) + 1, type_guid, 0, label);
    }
    Ok(found)
}

fn scan_mbr(disk: &Arc<dyn BlockDevice>, mbr: &[u8]) -> usize {
    let mut found = 0;
    for i in 0..4 {
        let e = &mbr[446 + i * 16..446 + (i + 1) * 16];
        let kind = e[4];
        // 0x00 = unused; 0xEE = GPT protective entry (handled above);
        // 0x05/0x0F = extended partitions (not walked yet).
        if matches!(kind, 0x00 | 0xEE | 0x05 | 0x0F) {
            continue;
        }
        found += 1;
        add(disk, found, u32_at(e, 8) as u64, u32_at(e, 12) as u64, [0; 16], kind, String::new());
    }
    found
}
