//! File systems and volumes.
//!
//! Every partition with a recognised file system is mounted under a drive
//! letter, starting at `C:`, in discovery order. Paths use Windows syntax
//! (`C:\MERIDIAN\INIT.EXE`, case-insensitive; `/` also accepted). Access is
//! read-only.
//!
//! Note: today the only recognised file system is FAT32, so on a typical PC
//! the EFI system partition becomes `C:`. That changes once Meridian has its
//! own system volume.

pub mod fat;

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::io::block::{self, BlockDevice, IoError};
use crate::sync::SpinLock;
use fat::FatVolume;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
    Io(IoError),
    NotRecognized,
    Unsupported,
    Corrupt,
    NotFound,
    NotADirectory,
    IsADirectory,
    TooLarge,
    NoMemory,
    NoSuchVolume,
    BadPath,
}

impl FsError {
    pub fn message(self) -> &'static str {
        match self {
            FsError::Io(_) => "device I/O error",
            FsError::NotRecognized => "file system not recognised",
            FsError::Unsupported => "file system type not supported yet",
            FsError::Corrupt => "file system is corrupt",
            FsError::NotFound => "file not found",
            FsError::NotADirectory => "not a directory",
            FsError::IsADirectory => "is a directory",
            FsError::TooLarge => "file too large",
            FsError::NoMemory => "out of memory",
            FsError::NoSuchVolume => "no such drive",
            FsError::BadPath => "invalid path",
        }
    }
}

#[derive(Debug, Clone)]
pub struct DirEntry {
    /// Long name if there is one, otherwise the 8.3 name.
    pub name: String,
    /// The 8.3 name (the same as `name` when there is no long name).
    pub short_name: String,
    pub is_dir: bool,
    pub size: u64,
    pub first_cluster: u32,
    pub read_only: bool,
    pub hidden: bool,
}

pub struct Volume {
    pub letter: char,
    pub device: Arc<dyn BlockDevice>,
    pub fs: Arc<FatVolume>,
}

static VOLUMES: SpinLock<Vec<Arc<Volume>>> = SpinLock::new(Vec::new());

/// Mounts every partition that holds a supported file system. Returns the
/// number of volumes mounted.
pub fn mount_all() -> usize {
    let mut next = 'C';
    let mut mounted = 0;
    for dev in block::partitions() {
        match FatVolume::mount(dev.clone()) {
            Ok(fs) => {
                crate::info!(
                    "fs: {next}: FAT32 \"{}\" on {} ({}, {} MiB)",
                    fs.label,
                    dev.name(),
                    dev.description(),
                    fs.size_bytes() >> 20
                );
                VOLUMES.lock().push(Arc::new(Volume { letter: next, device: dev, fs }));
                mounted += 1;
                next = (next as u8 + 1) as char;
                if next > 'Z' {
                    break;
                }
            }
            Err(FsError::NotRecognized) => {}
            Err(e) => crate::info!("fs: {} not mounted: {}", dev.name(), e.message()),
        }
    }
    mounted
}

pub fn volumes() -> Vec<Arc<Volume>> {
    VOLUMES.lock().clone()
}

/// Splits `C:\a\b` into its volume and the path within it.
fn resolve(path: &str) -> Result<(Arc<Volume>, &str), FsError> {
    let mut chars = path.chars();
    let (Some(letter), Some(':')) = (chars.next(), chars.next()) else {
        return Err(FsError::BadPath);
    };
    let letter = letter.to_ascii_uppercase();
    let volume = VOLUMES.lock().iter().find(|v| v.letter == letter).cloned().ok_or(FsError::NoSuchVolume)?;
    Ok((volume, &path[2..]))
}

pub fn read_file(path: &str) -> Result<Vec<u8>, FsError> {
    let (volume, rest) = resolve(path)?;
    volume.fs.read_file(rest)
}

/// The directory entry for `path` (`None` for a volume's root directory).
pub fn metadata(path: &str) -> Result<Option<DirEntry>, FsError> {
    let (volume, rest) = resolve(path)?;
    volume.fs.lookup(rest)
}

pub fn list(path: &str) -> Result<Vec<DirEntry>, FsError> {
    let (volume, rest) = resolve(path)?;
    volume.fs.list(rest)
}

/// Brings up storage: disk controllers, partitions and volumes.
pub fn init_storage() {
    let disks = crate::drivers::ahci::probe();
    if disks == 0 {
        crate::info!("storage: no supported disks found");
        return;
    }
    for disk in block::disks() {
        match crate::io::partition::scan(&disk) {
            Ok(n) => crate::info!("storage: {} has {} partition(s)", disk.name(), n),
            Err(e) => crate::error!("storage: cannot read the partition table of {}: {e:?}", disk.name()),
        }
    }
    mount_all();
}
