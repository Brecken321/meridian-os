//! Object manager ("Ob"): kernel objects that user mode refers to through
//! per-process handles.
//!
//! Handles are small integers that are multiples of 4 (as on Windows, so
//! Win32 code that inspects handle values behaves as expected). A handle
//! table entry holds a strong reference; closing the handle drops it.
//! Access masks and the object namespace are planned (see ROADMAP.md).

use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::ke::objects::Event;

/// Every kind of object a handle can refer to.
#[derive(Clone)]
pub enum Object {
    Event(Arc<Event>),
}

const MAX_HANDLES: usize = 16 * 1024;

pub struct HandleTable {
    entries: Vec<Option<Object>>,
    open: usize,
}

impl HandleTable {
    pub const fn new() -> Self {
        Self { entries: Vec::new(), open: 0 }
    }

    fn index(handle: u64) -> Option<usize> {
        (handle != 0 && handle % 4 == 0).then(|| (handle / 4 - 1) as usize)
    }

    /// Stores `object` and returns its new handle, or `None` if the table
    /// is full.
    pub fn insert(&mut self, object: Object) -> Option<u64> {
        let index = match self.entries.iter().position(Option::is_none) {
            Some(i) => i,
            None if self.entries.len() < MAX_HANDLES => {
                self.entries.push(None);
                self.entries.len() - 1
            }
            None => return None,
        };
        self.entries[index] = Some(object);
        self.open += 1;
        Some((index as u64 + 1) * 4)
    }

    pub fn get(&self, handle: u64) -> Option<Object> {
        self.entries.get(Self::index(handle)?)?.clone()
    }

    /// Removes the handle and returns the object so the caller can drop it
    /// outside any lock.
    pub fn remove(&mut self, handle: u64) -> Option<Object> {
        let object = self.entries.get_mut(Self::index(handle)?)?.take()?;
        self.open -= 1;
        Some(object)
    }

    pub fn open_count(&self) -> usize {
        self.open
    }

    /// Removes every handle (process teardown).
    pub fn drain(&mut self) -> Vec<Object> {
        self.open = 0;
        self.entries.drain(..).flatten().collect()
    }
}
