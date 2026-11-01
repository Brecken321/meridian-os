//! I/O manager ("Io"): the block device layer and partition discovery.
//!
//! Storage is read-only by design at this stage: no block device exposes a
//! write operation, so nothing Meridian does can modify a disk.

pub mod block;
pub mod partition;
