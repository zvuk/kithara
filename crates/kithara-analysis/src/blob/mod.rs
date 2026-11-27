//! Byte encodings for this crate's stored artifacts.
//!
//! The versioned framing itself belongs to `kithara-blob`, which knows
//! nothing about what it carries; this module holds only the `Blob`
//! implementations for analysis artifacts.

#[cfg(any(feature = "analysis-beat", feature = "analysis-waveform"))]
mod progress;
mod track;

#[cfg(test)]
pub(crate) use kithara_blob::to_bytes;
pub(crate) use kithara_blob::{
    Blob, BlobError, MAX_PREALLOC, Reader, Writer, from_bytes, write_to,
};
