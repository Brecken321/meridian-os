//! Block devices: anything that reads fixed-size sectors (disks, partitions).

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::sync::SpinLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoError {
    /// The request is malformed (length not a multiple of the sector size).
    BadRequest,
    /// The request reaches past the end of the device.
    OutOfRange,
    /// The device reported an error.
    DeviceError,
    /// The device did not complete the request in time.
    Timeout,
}

pub trait BlockDevice: Send + Sync {
    /// Kernel name, e.g. `Harddisk0` or `Harddisk0\Partition1`.
    fn name(&self) -> &str;
    /// Human-readable description (model, partition type).
    fn description(&self) -> String;
    fn sector_size(&self) -> usize;
    fn sector_count(&self) -> u64;
    /// Reads `buf.len() / sector_size()` sectors starting at `lba`.
    fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> Result<(), IoError>;

    fn size_bytes(&self) -> u64 {
        self.sector_count() * self.sector_size() as u64
    }
}

/// Validates a read request against a device's geometry.
pub fn check_request(dev: &dyn BlockDevice, lba: u64, len: usize) -> Result<u64, IoError> {
    let ss = dev.sector_size();
    if len == 0 || len % ss != 0 {
        return Err(IoError::BadRequest);
    }
    let count = (len / ss) as u64;
    if lba.checked_add(count).is_none_or(|end| end > dev.sector_count()) {
        return Err(IoError::OutOfRange);
    }
    Ok(count)
}

static DISKS: SpinLock<Vec<Arc<dyn BlockDevice>>> = SpinLock::new(Vec::new());
static PARTITIONS: SpinLock<Vec<Arc<dyn BlockDevice>>> = SpinLock::new(Vec::new());

/// Registers a whole disk.
pub fn register_disk(dev: Arc<dyn BlockDevice>) {
    DISKS.lock().push(dev);
}

pub fn register_partition(dev: Arc<dyn BlockDevice>) {
    PARTITIONS.lock().push(dev);
}

pub fn disks() -> Vec<Arc<dyn BlockDevice>> {
    DISKS.lock().clone()
}

pub fn partitions() -> Vec<Arc<dyn BlockDevice>> {
    PARTITIONS.lock().clone()
}

/// Number of disks registered so far (used to name the next one).
pub fn disk_count() -> usize {
    DISKS.lock().len()
}
