//! Read-only access to input files via memory mapping.

use std::fs::File;
use std::ops::Deref;
use std::path::Path;

use memmap2::Mmap;

use crate::error::{Result, io_err};

/// A read-only view of an input file.
pub enum Input {
    Mapped(Mmap),
    /// Zero-length files can't be mapped on every platform.
    Empty,
}

impl Deref for Input {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match self {
            Input::Mapped(map) => map,
            Input::Empty => &[],
        }
    }
}

pub fn open(path: &Path) -> Result<Input> {
    let file = File::open(path).map_err(io_err(path))?;
    if file.metadata().map_err(io_err(path))?.len() == 0 {
        return Ok(Input::Empty);
    }
    // SAFETY: the mapping is read-only and audscan never writes to its input. As with any
    // mmap, another process truncating the file while we read it can fault the process.
    let map = unsafe { Mmap::map(&file) }.map_err(io_err(path))?;
    Ok(Input::Mapped(map))
}
