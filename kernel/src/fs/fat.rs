//! FAT32 file system driver (read-only), written from Microsoft's published
//! FAT specification ("FAT: General Overview of On-Disk Format", 1.03).
//!
//! Supports directory listing with long file names (VFAT), case-insensitive
//! path lookup and reading whole files. Every on-disk value is validated:
//! cluster numbers, chain lengths and directory sizes are bounded so a
//! corrupted volume produces an error, never a hang or a crash. FAT12/16
//! and writing are not implemented yet.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use super::{DirEntry, FsError};
use crate::io::block::BlockDevice;
use crate::sync::SpinLock;

const ATTR_READ_ONLY: u8 = 0x01;
const ATTR_HIDDEN: u8 = 0x02;
const ATTR_SYSTEM: u8 = 0x04;
const ATTR_VOLUME_ID: u8 = 0x08;
const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_LONG_NAME: u8 = 0x0F;
const END_OF_CHAIN: u32 = 0x0FFF_FFF8;
const BAD_CLUSTER: u32 = 0x0FFF_FFF7;
/// Largest file this driver will read into memory in one go.
const MAX_FILE: u64 = 256 * 1024 * 1024;
/// Largest directory (in bytes) it will walk.
const MAX_DIRECTORY: usize = 4 * 1024 * 1024;

pub struct FatVolume {
    dev: Arc<dyn BlockDevice>,
    bytes_per_sector: usize,
    sectors_per_cluster: u64,
    fat_start: u64,
    data_start: u64,
    root_cluster: u32,
    clusters: u32,
    pub label: String,
    /// One cached FAT sector: (sector number, contents).
    fat_cache: SpinLock<Option<(u64, Vec<u8>)>>,
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

impl FatVolume {
    /// Recognises and mounts a FAT32 volume on `dev`.
    pub fn mount(dev: Arc<dyn BlockDevice>) -> Result<Arc<Self>, FsError> {
        let ss = dev.sector_size();
        let mut boot = vec![0u8; ss];
        dev.read_sectors(0, &mut boot).map_err(FsError::Io)?;
        if boot[510..512] != [0x55, 0xAA] {
            return Err(FsError::NotRecognized);
        }
        let bytes_per_sector = u16_at(&boot, 11) as usize;
        let sectors_per_cluster = boot[13] as u64;
        let reserved = u16_at(&boot, 14) as u64;
        let fats = boot[16] as u64;
        let root_entries = u16_at(&boot, 17);
        let total16 = u16_at(&boot, 19) as u64;
        let fat_size16 = u16_at(&boot, 22);
        let total32 = u32_at(&boot, 32) as u64;
        let fat_size32 = u32_at(&boot, 36) as u64;
        let root_cluster = u32_at(&boot, 44);
        if bytes_per_sector != ss
            || !sectors_per_cluster.is_power_of_two()
            || sectors_per_cluster > 128
            || reserved == 0
            || fats == 0
        {
            return Err(FsError::NotRecognized);
        }
        if fat_size16 != 0 || root_entries != 0 || fat_size32 == 0 {
            // FAT12/FAT16 (or not FAT at all).
            return Err(FsError::Unsupported);
        }
        let total = if total16 != 0 { total16 } else { total32 };
        if total > dev.sector_count() {
            return Err(FsError::Corrupt);
        }
        let data_start = reserved + fats * fat_size32;
        let clusters = total.saturating_sub(data_start) / sectors_per_cluster;
        if clusters < 65525 || clusters > 0x0FFF_FFF5 || root_cluster < 2 || root_cluster as u64 >= clusters + 2 {
            return Err(FsError::Corrupt);
        }
        let label = String::from_utf8_lossy(&boot[71..82]).trim_end().into();
        Ok(Arc::new(Self {
            dev,
            bytes_per_sector,
            sectors_per_cluster,
            fat_start: reserved,
            data_start,
            root_cluster,
            clusters: clusters as u32,
            label,
            fat_cache: SpinLock::new(None),
        }))
    }

    fn cluster_bytes(&self) -> usize {
        self.bytes_per_sector * self.sectors_per_cluster as usize
    }

    pub fn size_bytes(&self) -> u64 {
        self.clusters as u64 * self.cluster_bytes() as u64
    }

    fn valid_cluster(&self, c: u32) -> bool {
        c >= 2 && c < self.clusters + 2
    }

    /// The cluster after `c` in its chain, or `None` at the end.
    fn next_cluster(&self, c: u32) -> Result<Option<u32>, FsError> {
        let offset = c as u64 * 4;
        let sector = self.fat_start + offset / self.bytes_per_sector as u64;
        let within = (offset % self.bytes_per_sector as u64) as usize;
        if let Some((s, data)) = self.fat_cache.lock().as_ref() {
            if *s == sector {
                return self.decode_fat_entry(u32_at(data, within));
            }
        }
        let mut data = vec![0u8; self.bytes_per_sector];
        self.dev.read_sectors(sector, &mut data).map_err(FsError::Io)?;
        let value = u32_at(&data, within);
        *self.fat_cache.lock() = Some((sector, data));
        self.decode_fat_entry(value)
    }

    fn decode_fat_entry(&self, raw: u32) -> Result<Option<u32>, FsError> {
        let next = raw & 0x0FFF_FFFF;
        if next >= END_OF_CHAIN {
            Ok(None)
        } else if next == BAD_CLUSTER || !self.valid_cluster(next) {
            Err(FsError::Corrupt)
        } else {
            Ok(Some(next))
        }
    }

    /// Reads up to `limit` bytes of the chain starting at `first`.
    fn read_chain(&self, first: u32, limit: usize) -> Result<Vec<u8>, FsError> {
        let cb = self.cluster_bytes();
        let mut out = Vec::new();
        if limit == 0 {
            return Ok(out);
        }
        if !self.valid_cluster(first) {
            return Err(FsError::Corrupt);
        }
        out.try_reserve(limit).map_err(|_| FsError::NoMemory)?;
        let mut cluster = Some(first);
        let mut steps = 0u32;
        let mut buf = vec![0u8; cb];
        while let Some(c) = cluster {
            steps += 1;
            if steps > self.clusters {
                return Err(FsError::Corrupt); // cycle in the chain
            }
            let lba = self.data_start + (c as u64 - 2) * self.sectors_per_cluster;
            self.dev.read_sectors(lba, &mut buf).map_err(FsError::Io)?;
            let take = (limit - out.len()).min(cb);
            out.extend_from_slice(&buf[..take]);
            if out.len() >= limit {
                break;
            }
            cluster = self.next_cluster(c)?;
        }
        Ok(out)
    }

    /// Lists a directory given its first cluster.
    fn read_dir(&self, cluster: u32) -> Result<Vec<DirEntry>, FsError> {
        let raw = self.read_chain(cluster, MAX_DIRECTORY)?;
        let mut entries = Vec::new();
        let mut long: Vec<u16> = Vec::new();
        let mut long_checksum: Option<u8> = None;
        for e in raw.chunks_exact(32) {
            match e[0] {
                0x00 => break,
                0xE5 => {
                    long.clear();
                    long_checksum = None;
                    continue;
                }
                _ => {}
            }
            let attr = e[11];
            if attr & 0x3F == ATTR_LONG_NAME {
                let seq = e[0];
                if seq & 0x40 != 0 {
                    long.clear();
                    long_checksum = Some(e[13]);
                }
                let part: Vec<u16> = [(1usize, 5usize), (14, 6), (28, 2)]
                    .iter()
                    .flat_map(|&(o, n)| (0..n).map(move |i| u16_at(e, o + i * 2)))
                    .collect();
                // Pieces arrive last-first; prepend each.
                long.splice(0..0, part);
                continue;
            }
            if attr & ATTR_VOLUME_ID != 0 {
                long.clear();
                continue;
            }
            let short: [u8; 11] = e[0..11].try_into().unwrap();
            let short_name = format_short_name(&short, e[12]);
            let name = if long_checksum == Some(short_name_checksum(&short)) && !long.is_empty() {
                let end = long.iter().position(|&c| c == 0 || c == 0xFFFF).unwrap_or(long.len());
                char::decode_utf16(long[..end].iter().copied()).map(|r| r.unwrap_or('?')).collect()
            } else {
                short_name.clone()
            };
            long.clear();
            long_checksum = None;
            if name == "." || name == ".." {
                continue;
            }
            let first_cluster = (u16_at(e, 20) as u32) << 16 | u16_at(e, 26) as u32;
            entries.push(DirEntry {
                name,
                short_name,
                is_dir: attr & ATTR_DIRECTORY != 0,
                size: u32_at(e, 28) as u64,
                first_cluster,
                read_only: attr & ATTR_READ_ONLY != 0,
                hidden: attr & (ATTR_HIDDEN | ATTR_SYSTEM) != 0,
            });
        }
        Ok(entries)
    }

    /// Lists the directory at `path` (components separated by `\` or `/`).
    pub fn list(&self, path: &str) -> Result<Vec<DirEntry>, FsError> {
        let cluster = match self.lookup(path)? {
            None => self.root_cluster,
            Some(e) if e.is_dir => e.first_cluster,
            Some(_) => return Err(FsError::NotADirectory),
        };
        self.read_dir(cluster)
    }

    /// Finds the entry at `path`; `None` means the root directory.
    pub fn lookup(&self, path: &str) -> Result<Option<DirEntry>, FsError> {
        let mut cluster = self.root_cluster;
        let mut found = None;
        for component in path.split(['\\', '/']).filter(|c| !c.is_empty()) {
            if let Some(DirEntry { is_dir: false, .. }) = found {
                return Err(FsError::NotADirectory);
            }
            let entry = self
                .read_dir(cluster)?
                .into_iter()
                .find(|e| e.name.eq_ignore_ascii_case(component) || e.short_name.eq_ignore_ascii_case(component))
                .ok_or(FsError::NotFound)?;
            cluster = entry.first_cluster;
            found = Some(entry);
        }
        Ok(found)
    }

    /// Reads a whole file.
    pub fn read_file(&self, path: &str) -> Result<Vec<u8>, FsError> {
        let entry = self.lookup(path)?.ok_or(FsError::IsADirectory)?;
        if entry.is_dir {
            return Err(FsError::IsADirectory);
        }
        if entry.size > MAX_FILE {
            return Err(FsError::TooLarge);
        }
        if entry.size == 0 {
            return Ok(Vec::new());
        }
        let data = self.read_chain(entry.first_cluster, entry.size as usize)?;
        if data.len() as u64 != entry.size {
            return Err(FsError::Corrupt); // chain shorter than the file size
        }
        Ok(data)
    }
}

fn short_name_checksum(name: &[u8; 11]) -> u8 {
    name.iter().fold(0u8, |sum, &c| (sum >> 1 | (sum & 1) << 7).wrapping_add(c))
}

/// "README  TXT" -> "README.TXT", honouring the Windows NT lower-case flags
/// (byte 12: 0x08 = lower-case base name, 0x10 = lower-case extension).
fn format_short_name(raw: &[u8; 11], case_flags: u8) -> String {
    let mut base: String = raw[0..8].iter().map(|&b| b as char).collect::<String>().trim_end().into();
    let mut ext: String = raw[8..11].iter().map(|&b| b as char).collect::<String>().trim_end().into();
    if raw[0] == 0x05 {
        base.replace_range(0..1, "\u{E5}");
    }
    if case_flags & 0x08 != 0 {
        base = base.to_ascii_lowercase();
    }
    if case_flags & 0x10 != 0 {
        ext = ext.to_ascii_lowercase();
    }
    if ext.is_empty() { base } else { alloc::format!("{base}.{ext}") }
}
