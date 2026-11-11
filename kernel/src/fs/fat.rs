//! FAT32 file system driver, written from Microsoft's published FAT
//! specification ("FAT: General Overview of On-Disk Format", 1.03).
//!
//! Supports directory listing with long file names (VFAT), case-insensitive
//! path lookup, reading whole files and, on writable devices, creating,
//! replacing, deleting and renaming files and folders. Every on-disk value
//! is validated: cluster numbers, chain lengths and directory sizes are
//! bounded so a corrupted volume produces an error, never a hang or a crash.
//! FAT12/16 are not implemented.
//!
//! Writes are ordered so an interruption loses at most the operation in
//! progress: file data first, then the cluster chain, then the directory
//! entry, and only then is the old data released. The FSInfo free-cluster
//! hint is marked unknown on the first write (allowed by the specification).

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use super::{DirEntry, FsError};
use crate::io::block::BlockDevice;
use crate::ke::objects::Mutex;
use crate::sync::SpinLock;

const ATTR_READ_ONLY: u8 = 0x01;
const ATTR_HIDDEN: u8 = 0x02;
const ATTR_SYSTEM: u8 = 0x04;
const ATTR_VOLUME_ID: u8 = 0x08;
const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_ARCHIVE: u8 = 0x20;
const ATTR_LONG_NAME: u8 = 0x0F;
const END_OF_CHAIN: u32 = 0x0FFF_FFF8;
const END_OF_CHAIN_MARK: u32 = 0x0FFF_FFFF;
const BAD_CLUSTER: u32 = 0x0FFF_FFF7;
/// Largest file this driver will read into memory in one go.
const MAX_FILE: u64 = 256 * 1024 * 1024;
/// Largest directory (in bytes) it will walk.
const MAX_DIRECTORY: usize = 4 * 1024 * 1024;
/// FAT sectors read at a time when scanning for free clusters.
const SCAN_SECTORS: u64 = 64;

pub struct FatVolume {
    dev: Arc<dyn BlockDevice>,
    bytes_per_sector: usize,
    sectors_per_cluster: u64,
    fat_start: u64,
    fat_size: u64,
    fats: u64,
    data_start: u64,
    root_cluster: u32,
    clusters: u32,
    fs_info_sector: u64,
    pub label: String,
    /// One cached FAT sector: (sector number, contents).
    fat_cache: SpinLock<Option<(u64, Vec<u8>)>>,
    /// Serialises every operation on the volume (recursive).
    lock: Mutex,
    free: AtomicU32,
    next_free: AtomicU32,
    fs_info_dirty: AtomicBool,
}

/// A directory entry together with where it lives in its directory.
#[derive(Clone)]
struct Slot {
    entry: DirEntry,
    /// Index (in 32-byte entries) of the short entry.
    index: usize,
    /// Index of the first long-name entry belonging to it (== `index` if none).
    first: usize,
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn put16(b: &mut [u8], o: usize, v: u16) {
    b[o..o + 2].copy_from_slice(&v.to_le_bytes());
}
fn put32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
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
        let fs_info_sector = u16_at(&boot, 48) as u64;
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
        // The FAT must have an entry for every cluster.
        if (clusters + 2) * 4 > fat_size32 * bytes_per_sector as u64 {
            return Err(FsError::Corrupt);
        }
        let label = String::from_utf8_lossy(&boot[71..82]).trim_end().into();
        let volume = Arc::new(Self {
            dev,
            bytes_per_sector,
            sectors_per_cluster,
            fat_start: reserved,
            fat_size: fat_size32,
            fats,
            data_start,
            root_cluster,
            clusters: clusters as u32,
            fs_info_sector: if fs_info_sector != 0 && fs_info_sector < reserved { fs_info_sector } else { 0 },
            label,
            fat_cache: SpinLock::new(None),
            lock: Mutex::new(),
            free: AtomicU32::new(0),
            next_free: AtomicU32::new(2),
            fs_info_dirty: AtomicBool::new(false),
        });
        volume.count_free()?;
        Ok(volume)
    }

    fn cluster_bytes(&self) -> usize {
        self.bytes_per_sector * self.sectors_per_cluster as usize
    }

    pub fn size_bytes(&self) -> u64 {
        self.clusters as u64 * self.cluster_bytes() as u64
    }

    pub fn free_bytes(&self) -> u64 {
        self.free.load(Ordering::Relaxed) as u64 * self.cluster_bytes() as u64
    }

    /// True if this volume accepts writes (a Meridian data partition).
    pub fn writable(&self) -> bool {
        self.dev.writable()
    }

    fn valid_cluster(&self, c: u32) -> bool {
        c >= 2 && c < self.clusters + 2
    }

    fn cluster_lba(&self, c: u32) -> u64 {
        self.data_start + (c as u64 - 2) * self.sectors_per_cluster
    }

    fn locked<R>(&self, f: impl FnOnce() -> Result<R, FsError>) -> Result<R, FsError> {
        self.lock.acquire(None);
        let r = f();
        self.lock.release();
        r
    }

    /// Counts free clusters at mount time (for the free-space figure).
    fn count_free(&self) -> Result<(), FsError> {
        let bps = self.bytes_per_sector as u64;
        let fat_sectors = ((self.clusters as u64 + 2) * 4).div_ceil(bps);
        let mut buf = vec![0u8; (SCAN_SECTORS * bps) as usize];
        let mut free = 0u32;
        let mut first_free = 0u32;
        let mut sector = 0;
        while sector < fat_sectors {
            let n = SCAN_SECTORS.min(fat_sectors - sector);
            let chunk = &mut buf[..(n * bps) as usize];
            self.dev.read_sectors(self.fat_start + sector, chunk).map_err(FsError::Io)?;
            for (i, e) in chunk.chunks_exact(4).enumerate() {
                let c = (sector * bps / 4) as u32 + i as u32;
                if self.valid_cluster(c) && u32_at(e, 0) & 0x0FFF_FFFF == 0 {
                    free += 1;
                    if first_free == 0 {
                        first_free = c;
                    }
                }
            }
            sector += n;
        }
        self.free.store(free, Ordering::Relaxed);
        self.next_free.store(first_free.max(2), Ordering::Relaxed);
        Ok(())
    }

    /// The raw (28-bit) FAT entry for cluster `c`.
    fn fat_entry(&self, c: u32) -> Result<u32, FsError> {
        let offset = c as u64 * 4;
        let sector = self.fat_start + offset / self.bytes_per_sector as u64;
        let within = (offset % self.bytes_per_sector as u64) as usize;
        if let Some((s, data)) = self.fat_cache.lock().as_ref() {
            if *s == sector {
                return Ok(u32_at(data, within) & 0x0FFF_FFFF);
            }
        }
        let mut data = vec![0u8; self.bytes_per_sector];
        self.dev.read_sectors(sector, &mut data).map_err(FsError::Io)?;
        let value = u32_at(&data, within) & 0x0FFF_FFFF;
        *self.fat_cache.lock() = Some((sector, data));
        Ok(value)
    }

    /// The cluster after `c` in its chain, or `None` at the end.
    fn next_cluster(&self, c: u32) -> Result<Option<u32>, FsError> {
        let next = self.fat_entry(c)?;
        if next >= END_OF_CHAIN {
            Ok(None)
        } else if next == BAD_CLUSTER || !self.valid_cluster(next) {
            Err(FsError::Corrupt)
        } else {
            Ok(Some(next))
        }
    }

    /// Every cluster of the chain starting at `first`.
    fn chain(&self, first: u32) -> Result<Vec<u32>, FsError> {
        let mut out = Vec::new();
        if first == 0 {
            return Ok(out);
        }
        if !self.valid_cluster(first) {
            return Err(FsError::Corrupt);
        }
        let mut cluster = Some(first);
        while let Some(c) = cluster {
            if out.len() as u32 >= self.clusters {
                return Err(FsError::Corrupt); // cycle in the chain
            }
            out.push(c);
            cluster = self.next_cluster(c)?;
        }
        Ok(out)
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
        out.try_reserve(limit.min(16 * 1024 * 1024)).map_err(|_| FsError::NoMemory)?;
        let mut cluster = Some(first);
        let mut steps = 0u32;
        let mut buf = vec![0u8; cb];
        while let Some(c) = cluster {
            steps += 1;
            if steps > self.clusters {
                return Err(FsError::Corrupt); // cycle in the chain
            }
            self.dev.read_sectors(self.cluster_lba(c), &mut buf).map_err(FsError::Io)?;
            let take = (limit - out.len()).min(cb);
            out.try_reserve(take).map_err(|_| FsError::NoMemory)?;
            out.extend_from_slice(&buf[..take]);
            if out.len() >= limit {
                break;
            }
            cluster = self.next_cluster(c)?;
        }
        Ok(out)
    }

    /// Parses a directory given its first cluster: its raw contents and
    /// every live entry with its position.
    fn read_dir_slots(&self, cluster: u32) -> Result<(Vec<u8>, Vec<Slot>), FsError> {
        let raw = self.read_chain(cluster, MAX_DIRECTORY)?;
        let mut slots = Vec::new();
        let mut long: Vec<u16> = Vec::new();
        let mut long_checksum: Option<u8> = None;
        let mut long_first = 0;
        for (index, e) in raw.chunks_exact(32).enumerate() {
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
                    long_first = index;
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
                long_checksum = None;
                continue;
            }
            let short: [u8; 11] = e[0..11].try_into().unwrap();
            let short_name = format_short_name(&short, e[12]);
            let has_long = long_checksum == Some(short_name_checksum(&short)) && !long.is_empty();
            let name = if has_long {
                let end = long.iter().position(|&c| c == 0 || c == 0xFFFF).unwrap_or(long.len());
                char::decode_utf16(long[..end].iter().copied()).map(|r| r.unwrap_or('?')).collect()
            } else {
                short_name.clone()
            };
            let first = if has_long { long_first } else { index };
            long.clear();
            long_checksum = None;
            if name == "." || name == ".." {
                continue;
            }
            let first_cluster = (u16_at(e, 20) as u32) << 16 | u16_at(e, 26) as u32;
            slots.push(Slot {
                entry: DirEntry {
                    name,
                    short_name,
                    is_dir: attr & ATTR_DIRECTORY != 0,
                    size: u32_at(e, 28) as u64,
                    first_cluster,
                    read_only: attr & ATTR_READ_ONLY != 0,
                    hidden: attr & (ATTR_HIDDEN | ATTR_SYSTEM) != 0,
                },
                index,
                first,
            });
        }
        Ok((raw, slots))
    }

    fn read_dir(&self, cluster: u32) -> Result<Vec<DirEntry>, FsError> {
        Ok(self.read_dir_slots(cluster)?.1.into_iter().map(|s| s.entry).collect())
    }

    /// Lists the directory at `path` (components separated by `\` or `/`).
    pub fn list(&self, path: &str) -> Result<Vec<DirEntry>, FsError> {
        self.locked(|| {
            let cluster = self.dir_cluster(path)?;
            self.read_dir(cluster)
        })
    }

    /// First cluster of the directory at `path`.
    fn dir_cluster(&self, path: &str) -> Result<u32, FsError> {
        match self.lookup(path)? {
            None => Ok(self.root_cluster),
            Some(e) if e.is_dir => Ok(if e.first_cluster == 0 { self.root_cluster } else { e.first_cluster }),
            Some(_) => Err(FsError::NotADirectory),
        }
    }

    /// Finds the entry at `path`; `None` means the root directory.
    pub fn lookup(&self, path: &str) -> Result<Option<DirEntry>, FsError> {
        self.locked(|| {
            let mut cluster = self.root_cluster;
            let mut found = None;
            for component in path.split(['\\', '/']).filter(|c| !c.is_empty()) {
                if let Some(DirEntry { is_dir: false, .. }) = found {
                    return Err(FsError::NotADirectory);
                }
                let entry = self
                    .read_dir(cluster)?
                    .into_iter()
                    .find(|e| matches_name(e, component))
                    .ok_or(FsError::NotFound)?;
                cluster = if entry.first_cluster == 0 { self.root_cluster } else { entry.first_cluster };
                found = Some(entry);
            }
            Ok(found)
        })
    }

    /// Reads a whole file.
    pub fn read_file(&self, path: &str) -> Result<Vec<u8>, FsError> {
        self.locked(|| {
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
        })
    }

    // ---- writing ----------------------------------------------------------

    fn check_writable(&self) -> Result<(), FsError> {
        if self.writable() { Ok(()) } else { Err(FsError::ReadOnly) }
    }

    /// Sets FAT entries (in every FAT copy), one sector read and written per
    /// distinct sector touched.
    fn set_fat(&self, updates: &mut [(u32, u32)]) -> Result<(), FsError> {
        let bps = self.bytes_per_sector as u64;
        updates.sort_unstable_by_key(|u| u.0);
        let mut i = 0;
        let mut data = vec![0u8; self.bytes_per_sector];
        while i < updates.len() {
            let rel = updates[i].0 as u64 * 4 / bps;
            self.dev.read_sectors(self.fat_start + rel, &mut data).map_err(FsError::Io)?;
            while i < updates.len() && updates[i].0 as u64 * 4 / bps == rel {
                let (c, v) = updates[i];
                if !self.valid_cluster(c) {
                    return Err(FsError::Corrupt);
                }
                let o = (c as u64 * 4 % bps) as usize;
                let old = u32_at(&data, o);
                put32(&mut data, o, (old & 0xF000_0000) | (v & 0x0FFF_FFFF));
                i += 1;
            }
            for copy in 0..self.fats {
                self.dev.write_sectors(self.fat_start + copy * self.fat_size + rel, &data).map_err(FsError::Io)?;
            }
            *self.fat_cache.lock() = Some((self.fat_start + rel, data.clone()));
        }
        Ok(())
    }

    /// Marks the FSInfo free-cluster count and hint as unknown (once).
    fn note_write(&self) -> Result<(), FsError> {
        if self.fs_info_dirty.swap(true, Ordering::Relaxed) || self.fs_info_sector == 0 {
            return Ok(());
        }
        let mut s = vec![0u8; self.bytes_per_sector];
        self.dev.read_sectors(self.fs_info_sector, &mut s).map_err(FsError::Io)?;
        if u32_at(&s, 0) == 0x4161_5252 && u32_at(&s, 484) == 0x6141_7272 {
            put32(&mut s, 488, u32::MAX);
            put32(&mut s, 492, u32::MAX);
            self.dev.write_sectors(self.fs_info_sector, &s).map_err(FsError::Io)?;
        }
        Ok(())
    }

    /// Allocates `count` clusters linked into one chain (the last marked as
    /// end of chain) and returns them in chain order.
    fn allocate(&self, count: usize) -> Result<Vec<u32>, FsError> {
        if count == 0 {
            return Ok(Vec::new());
        }
        if count as u64 > self.free.load(Ordering::Relaxed) as u64 {
            return Err(FsError::DiskFull);
        }
        self.note_write()?;
        let bps = self.bytes_per_sector as u64;
        let per_sector = bps / 4;
        let mut found = Vec::new();
        found.try_reserve(count).map_err(|_| FsError::NoMemory)?;
        let mut buf = vec![0u8; (SCAN_SECTORS * bps) as usize];
        let mut buf_first: Option<u64> = None;
        let mut c = self.next_free.load(Ordering::Relaxed).clamp(2, self.clusters + 1);
        for _ in 0..self.clusters {
            let rel = c as u64 / per_sector;
            if buf_first.is_none_or(|f| rel < f || rel >= f + SCAN_SECTORS) {
                let fat_sectors = ((self.clusters as u64 + 2) * 4).div_ceil(bps);
                let n = SCAN_SECTORS.min(fat_sectors - rel);
                self.dev.read_sectors(self.fat_start + rel, &mut buf[..(n * bps) as usize]).map_err(FsError::Io)?;
                buf_first = Some(rel);
            }
            let o = ((rel - buf_first.unwrap()) * bps + c as u64 % per_sector * 4) as usize;
            if u32_at(&buf, o) & 0x0FFF_FFFF == 0 {
                found.push(c);
                if found.len() == count {
                    break;
                }
            }
            c = if c + 1 >= self.clusters + 2 { 2 } else { c + 1 };
        }
        if found.len() < count {
            return Err(FsError::DiskFull);
        }
        let mut updates: Vec<(u32, u32)> =
            found.windows(2).map(|w| (w[0], w[1])).chain([(found[count - 1], END_OF_CHAIN_MARK)]).collect();
        self.set_fat(&mut updates)?;
        self.free.fetch_sub(count as u32, Ordering::Relaxed);
        let last = found[count - 1];
        self.next_free.store(if last + 1 >= self.clusters + 2 { 2 } else { last + 1 }, Ordering::Relaxed);
        Ok(found)
    }

    /// Returns clusters to the free pool.
    fn release(&self, clusters: &[u32]) -> Result<(), FsError> {
        if clusters.is_empty() {
            return Ok(());
        }
        self.note_write()?;
        let mut updates: Vec<(u32, u32)> = clusters.iter().map(|&c| (c, 0)).collect();
        self.set_fat(&mut updates)?;
        self.free.fetch_add(clusters.len() as u32, Ordering::Relaxed);
        if let Some(&low) = clusters.iter().min() {
            self.next_free.fetch_min(low, Ordering::Relaxed);
        }
        Ok(())
    }

    /// Writes `data` across `clusters` (zero-filling the last one), using
    /// one device request per run of consecutive clusters.
    fn write_clusters(&self, clusters: &[u32], data: &[u8]) -> Result<(), FsError> {
        let cb = self.cluster_bytes();
        let mut i = 0;
        while i < clusters.len() {
            let mut run = 1;
            while i + run < clusters.len() && clusters[i + run] == clusters[i] + run as u32 && run < 128 {
                run += 1;
            }
            let start = i * cb;
            let mut buf = vec![0u8; run * cb];
            let end = (start + run * cb).min(data.len());
            if start < end {
                buf[..end - start].copy_from_slice(&data[start..end]);
            }
            self.dev.write_sectors(self.cluster_lba(clusters[i]), &buf).map_err(FsError::Io)?;
            i += run;
        }
        Ok(())
    }

    /// Writes a directory's raw contents back over its chain.
    fn write_dir(&self, first: u32, raw: &[u8]) -> Result<(), FsError> {
        let clusters = self.chain(first)?;
        if clusters.len() * self.cluster_bytes() != raw.len() {
            return Err(FsError::Corrupt);
        }
        self.write_clusters(&clusters, raw)
    }

    /// Stores `entries` in free slots of directory `dir`, growing it if
    /// needed. Returns the index of the last (short) entry.
    fn insert_entries(&self, dir: u32, entries: &[[u8; 32]]) -> Result<usize, FsError> {
        let (mut raw, _) = self.read_dir_slots(dir)?;
        let n = entries.len();
        let slots = raw.len() / 32;
        let (mut start, mut run) = (0, 0);
        for i in 0..slots {
            if matches!(raw[i * 32], 0x00 | 0xE5) {
                if run == 0 {
                    start = i;
                }
                run += 1;
                if run == n {
                    break;
                }
            } else {
                run = 0;
            }
        }
        if run < n {
            // Grow the directory; a free run at the very end continues
            // into the new cluster(s).
            if run == 0 || start + run != slots {
                start = slots;
                run = 0;
            }
            let cb = self.cluster_bytes();
            let extra = ((n - run) * 32).div_ceil(cb);
            if raw.len() + extra * cb > MAX_DIRECTORY {
                return Err(FsError::TooLarge);
            }
            let new = self.allocate(extra)?;
            if let Err(e) = self.write_clusters(&new, &[]) {
                self.release(&new)?;
                return Err(e);
            }
            let last = *self.chain(dir)?.last().ok_or(FsError::Corrupt)?;
            self.set_fat(&mut [(last, new[0])])?;
            raw.resize(raw.len() + extra * cb, 0);
        }
        for (k, e) in entries.iter().enumerate() {
            raw[(start + k) * 32..(start + k + 1) * 32].copy_from_slice(e);
        }
        self.write_dir(dir, &raw)?;
        Ok(start + n - 1)
    }

    /// Marks the entries `first..=last` of directory `dir` as deleted.
    fn remove_entries(&self, dir: u32, first: usize, last: usize) -> Result<(), FsError> {
        let (mut raw, _) = self.read_dir_slots(dir)?;
        if (last + 1) * 32 > raw.len() {
            return Err(FsError::Corrupt);
        }
        for i in first..=last {
            raw[i * 32] = 0xE5;
        }
        self.write_dir(dir, &raw)
    }

    /// Splits `path` into its parent directory's first cluster and the
    /// final name, which is validated.
    fn parent_of<'a>(&self, path: &'a str) -> Result<(u32, &'a str), FsError> {
        let trimmed = path.trim_end_matches(['\\', '/']);
        let (parent, name) = match trimmed.rfind(['\\', '/']) {
            Some(i) => (&trimmed[..i], &trimmed[i + 1..]),
            None => ("", trimmed),
        };
        validate_name(name)?;
        Ok((self.dir_cluster(parent)?, name))
    }

    fn find(&self, dir: u32, name: &str) -> Result<Option<Slot>, FsError> {
        Ok(self.read_dir_slots(dir)?.1.into_iter().find(|s| matches_name(&s.entry, name)))
    }

    /// Creates `path`, or replaces its contents if it exists.
    pub fn write_file(&self, path: &str, data: &[u8]) -> Result<(), FsError> {
        self.check_writable()?;
        if data.len() as u64 > MAX_FILE {
            return Err(FsError::TooLarge);
        }
        if path.ends_with(['\\', '/']) {
            return Err(FsError::InvalidName); // names a folder, not a file
        }
        self.locked(|| {
            let (dir, name) = self.parent_of(path)?;
            let existing = self.find(dir, name)?;
            if let Some(s) = &existing {
                if s.entry.is_dir {
                    return Err(FsError::IsADirectory);
                }
                if s.entry.read_only {
                    return Err(FsError::ReadOnly);
                }
            }
            let clusters = self.allocate(data.len().div_ceil(self.cluster_bytes()))?;
            if let Err(e) = self.write_clusters(&clusters, data) {
                self.release(&clusters)?;
                return Err(e);
            }
            let first = clusters.first().copied().unwrap_or(0);
            let (time, date) = fat_now();
            match existing {
                Some(s) => {
                    let (mut raw, _) = self.read_dir_slots(dir)?;
                    let o = s.index * 32;
                    put16(&mut raw, o + 20, (first >> 16) as u16);
                    put16(&mut raw, o + 26, first as u16);
                    put32(&mut raw, o + 28, data.len() as u32);
                    put16(&mut raw, o + 22, time);
                    put16(&mut raw, o + 24, date);
                    put16(&mut raw, o + 18, date);
                    raw[o + 11] |= ATTR_ARCHIVE;
                    self.write_dir(dir, &raw)?;
                    let old = self.chain(s.entry.first_cluster)?;
                    self.release(&old)?;
                }
                None => {
                    let entries = self.new_entries(dir, name, ATTR_ARCHIVE, first, data.len() as u32)?;
                    if let Err(e) = self.insert_entries(dir, &entries) {
                        self.release(&clusters)?;
                        return Err(e);
                    }
                }
            }
            self.dev.flush().map_err(FsError::Io)
        })
    }

    /// Creates the directory `path`.
    pub fn create_dir(&self, path: &str) -> Result<(), FsError> {
        self.check_writable()?;
        self.locked(|| {
            let (dir, name) = self.parent_of(path)?;
            if self.find(dir, name)?.is_some() {
                return Err(FsError::AlreadyExists);
            }
            let cluster = self.allocate(1)?;
            let (time, date) = fat_now();
            let mut content = vec![0u8; self.cluster_bytes()];
            let parent = if dir == self.root_cluster { 0 } else { dir };
            content[..32].copy_from_slice(&short_entry(b".          ", ATTR_DIRECTORY, 0, cluster[0], 0, time, date));
            content[32..64].copy_from_slice(&short_entry(b"..         ", ATTR_DIRECTORY, 0, parent, 0, time, date));
            let entries = self.new_entries(dir, name, ATTR_DIRECTORY, cluster[0], 0)?;
            let result = self.write_clusters(&cluster, &content).and_then(|_| self.insert_entries(dir, &entries));
            if let Err(e) = result {
                self.release(&cluster)?;
                return Err(e);
            }
            self.dev.flush().map_err(FsError::Io)
        })
    }

    /// Deletes a file or an empty directory.
    pub fn delete(&self, path: &str) -> Result<(), FsError> {
        self.check_writable()?;
        self.locked(|| {
            let (dir, name) = self.parent_of(path)?;
            let slot = self.find(dir, name)?.ok_or(FsError::NotFound)?;
            if slot.entry.read_only {
                return Err(FsError::ReadOnly);
            }
            if slot.entry.is_dir && !self.read_dir(slot.entry.first_cluster)?.is_empty() {
                return Err(FsError::NotEmpty);
            }
            let clusters = self.chain(slot.entry.first_cluster)?;
            self.remove_entries(dir, slot.first, slot.index)?;
            self.release(&clusters)?;
            self.dev.flush().map_err(FsError::Io)
        })
    }

    /// Renames or moves `from` to `to` (both on this volume).
    pub fn rename(&self, from: &str, to: &str) -> Result<(), FsError> {
        self.check_writable()?;
        self.locked(|| {
            let (from_dir, from_name) = self.parent_of(from)?;
            let (to_dir, to_name) = self.parent_of(to)?;
            let slot = self.find(from_dir, from_name)?.ok_or(FsError::NotFound)?;
            if let Some(existing) = self.find(to_dir, to_name)? {
                // Only a change of letter case of the same entry is allowed.
                if from_dir != to_dir || existing.index != slot.index {
                    return Err(FsError::AlreadyExists);
                }
            }
            if slot.entry.is_dir {
                // A folder cannot move into itself or one of its subfolders.
                let mut c = to_dir;
                for _ in 0..64 {
                    if c == slot.entry.first_cluster {
                        return Err(FsError::BadPath);
                    }
                    if c == self.root_cluster {
                        break;
                    }
                    let (raw, _) = self.read_dir_slots(c)?;
                    let up = (u16_at(&raw, 32 + 20) as u32) << 16 | u16_at(&raw, 32 + 26) as u32;
                    c = if up == 0 { self.root_cluster } else { up };
                }
            }
            let (raw, _) = self.read_dir_slots(from_dir)?;
            let old_entries: Vec<[u8; 32]> =
                raw[slot.first * 32..(slot.index + 1) * 32].chunks_exact(32).map(|e| e.try_into().unwrap()).collect();
            let old = *old_entries.last().ok_or(FsError::Corrupt)?;
            let case_only = from_dir == to_dir && self.find(to_dir, to_name)?.is_some();
            if case_only {
                // Remove first so the new entry's short name is free.
                self.remove_entries(from_dir, slot.first, slot.index)?;
            }
            let mut entries = match self.new_entries(to_dir, to_name, old[11], 0, 0) {
                Ok(e) => e,
                Err(e) => {
                    if case_only {
                        self.insert_entries(from_dir, &old_entries)?;
                    }
                    return Err(e);
                }
            };
            // Keep everything but the name: attributes, times, cluster, size.
            let last = entries.len() - 1;
            entries[last][11..32].copy_from_slice(&old[11..32]);
            let mut new_short = [0u8; 11];
            new_short.copy_from_slice(&entries[last][0..11]);
            entries[last][12] = name_case_flags(to_name, &new_short);
            if case_only {
                if let Err(e) = self.insert_entries(to_dir, &entries) {
                    self.insert_entries(from_dir, &old_entries)?;
                    return Err(e);
                }
            } else if from_dir == to_dir {
                self.insert_entries(to_dir, &entries)?;
                self.remove_entries(from_dir, slot.first, slot.index)?;
            } else {
                self.insert_entries(to_dir, &entries)?;
                self.remove_entries(from_dir, slot.first, slot.index)?;
                if slot.entry.is_dir && slot.entry.first_cluster != 0 {
                    let (mut sub, _) = self.read_dir_slots(slot.entry.first_cluster)?;
                    let parent = if to_dir == self.root_cluster { 0 } else { to_dir };
                    if sub.len() >= 64 && &sub[32..34] == b".." {
                        put16(&mut sub, 32 + 20, (parent >> 16) as u16);
                        put16(&mut sub, 32 + 26, parent as u16);
                        self.write_dir(slot.entry.first_cluster, &sub)?;
                    }
                }
            }
            self.dev.flush().map_err(FsError::Io)
        })
    }

    /// Directory entries (long-name entries, then the short entry) for a
    /// new entry called `name` in directory `dir`.
    fn new_entries(&self, dir: u32, name: &str, attr: u8, cluster: u32, size: u32) -> Result<Vec<[u8; 32]>, FsError> {
        let (raw, _) = self.read_dir_slots(dir)?;
        let taken: Vec<[u8; 11]> = raw
            .chunks_exact(32)
            .take_while(|e| e[0] != 0)
            .filter(|e| e[0] != 0xE5 && e[11] & 0x3F != ATTR_LONG_NAME)
            .map(|e| e[0..11].try_into().unwrap())
            .collect();
        let (short, needs_long) = short_name_for(name, &taken)?;
        let (time, date) = fat_now();
        let mut entries = Vec::new();
        if needs_long {
            let units: Vec<u16> = name.encode_utf16().collect();
            let pieces = units.len().div_ceil(13);
            let checksum = short_name_checksum(&short);
            for p in (0..pieces).rev() {
                let mut e = [0u8; 32];
                e[0] = (p + 1) as u8 | if p + 1 == pieces { 0x40 } else { 0 };
                e[11] = ATTR_LONG_NAME;
                e[13] = checksum;
                for k in 0..13 {
                    let i = p * 13 + k;
                    let unit = match i.cmp(&units.len()) {
                        core::cmp::Ordering::Less => units[i],
                        core::cmp::Ordering::Equal => 0x0000,
                        core::cmp::Ordering::Greater => 0xFFFF,
                    };
                    let o = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30][k];
                    put16(&mut e, o, unit);
                }
                entries.push(e);
            }
        }
        let case = if needs_long { 0 } else { name_case_flags(name, &short) };
        entries.push(short_entry(&short, attr, case, cluster, size, time, date));
        Ok(entries)
    }
}

fn short_entry(name: &[u8; 11], attr: u8, case: u8, cluster: u32, size: u32, time: u16, date: u16) -> [u8; 32] {
    let mut e = [0u8; 32];
    e[0..11].copy_from_slice(name);
    e[11] = attr;
    e[12] = case;
    put16(&mut e, 14, time);
    put16(&mut e, 16, date);
    put16(&mut e, 18, date);
    put16(&mut e, 20, (cluster >> 16) as u16);
    put16(&mut e, 22, time);
    put16(&mut e, 24, date);
    put16(&mut e, 26, cluster as u16);
    put32(&mut e, 28, size);
    e
}

/// The current local time in FAT's packed (time, date) form.
fn fat_now() -> (u16, u16) {
    let t = crate::hal::rtc::now();
    let year = t.year.clamp(1980, 2107) - 1980;
    let time = (t.hour as u16) << 11 | (t.minute as u16) << 5 | (t.second as u16 / 2);
    let date = year << 9 | (t.month as u16) << 5 | t.day as u16;
    (time, date)
}

fn matches_name(e: &DirEntry, name: &str) -> bool {
    e.name.eq_ignore_ascii_case(name) || e.short_name.eq_ignore_ascii_case(name)
}

/// Rejects names Windows would reject: empty, too long, reserved
/// characters, trailing dots or spaces, "." and "..".
fn validate_name(name: &str) -> Result<(), FsError> {
    let units = name.encode_utf16().count();
    if name.is_empty() || units > 255 || name == "." || name == ".." || name.ends_with(['.', ' ']) {
        return Err(FsError::InvalidName);
    }
    if name.chars().any(|c| (c as u32) < 0x20 || "\\/:*?\"<>|".contains(c)) {
        return Err(FsError::InvalidName);
    }
    Ok(())
}

fn short_char_ok(c: u8) -> bool {
    c.is_ascii_uppercase() || c.is_ascii_digit() || b"!#$%&'()-@^_`{}~".contains(&c)
}

/// The 8.3 name for `name` and whether a long name is needed as well. A
/// name that already fits 8.3 (each part all upper or all lower case) is
/// stored as is; otherwise a unique `BASE~N.EXT` alias is generated.
fn short_name_for(name: &str, taken: &[[u8; 11]]) -> Result<([u8; 11], bool), FsError> {
    let (base, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i + 1..]),
        _ => (name, ""),
    };
    let fits = |s: &str, max: usize| {
        !s.is_empty()
            && s.len() <= max
            && s.bytes().all(|b| short_char_ok(b.to_ascii_uppercase()))
            && (s.bytes().all(|b| !b.is_ascii_lowercase()) || s.bytes().all(|b| !b.is_ascii_uppercase()))
    };
    let mut short = [b' '; 11];
    if fits(base, 8) && (ext.is_empty() || fits(ext, 3)) && !base.contains('.') {
        for (i, b) in base.bytes().enumerate() {
            short[i] = b.to_ascii_uppercase();
        }
        for (i, b) in ext.bytes().enumerate() {
            short[8 + i] = b.to_ascii_uppercase();
        }
        if taken.contains(&short) {
            return Err(FsError::AlreadyExists);
        }
        return Ok((short, false));
    }
    let clean = |s: &str| -> Vec<u8> {
        s.chars()
            .filter(|&c| c != ' ' && c != '.')
            .map(|c| {
                let u = c.to_ascii_uppercase();
                if u.is_ascii() && short_char_ok(u as u8) { u as u8 } else { b'_' }
            })
            .collect()
    };
    let b = clean(base);
    let e = clean(ext);
    for (i, &c) in e.iter().take(3).enumerate() {
        short[8 + i] = c;
    }
    for n in 1..1_000_000u32 {
        let tail = alloc::format!("~{n}");
        let keep = (8 - tail.len()).min(b.len()).max(if b.is_empty() { 0 } else { 1 });
        let mut candidate = short;
        candidate[..8].fill(b' ');
        let stem: &[u8] = if b.is_empty() { b"_" } else { &b[..keep] };
        candidate[..stem.len()].copy_from_slice(stem);
        candidate[stem.len()..stem.len() + tail.len()].copy_from_slice(tail.as_bytes());
        if !taken.contains(&candidate) {
            return Ok((candidate, true));
        }
    }
    Err(FsError::AlreadyExists)
}

/// Windows NT lower-case flags for a name stored without a long name.
fn name_case_flags(name: &str, short: &[u8; 11]) -> u8 {
    let (base, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i + 1..]),
        _ => (name, ""),
    };
    let base_short: String = short[0..8].iter().map(|&b| b as char).collect::<String>().trim_end().into();
    if !base.eq_ignore_ascii_case(&base_short) {
        return 0; // an alias: the long name carries the case
    }
    let mut flags = 0;
    if base.bytes().any(|b| b.is_ascii_lowercase()) {
        flags |= 0x08;
    }
    if ext.bytes().any(|b| b.is_ascii_lowercase()) {
        flags |= 0x10;
    }
    flags
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
