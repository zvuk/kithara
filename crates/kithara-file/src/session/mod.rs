#![forbid(unsafe_code)]

//! Per-session state and Source/Peer implementations for the file protocol.
//!
//! Split into focused submodules:
//! - [`peer`] — `Peer` trait implementation (`FilePeer`).
//! - [`inner`] — owned mutable state (`FileInner`, `FilePhase`, `FileStreamState`).
//! - [`source`] — public `FileSource` API and `Source` trait implementation.
//! - [`download`] — async download driver (full file + on-demand range fills).

mod download;
mod inner;
mod peer;
mod reader;
mod source;
#[cfg(test)]
mod tests;

pub(crate) use inner::FileStreamState;
pub(crate) use peer::FilePeer;
pub(crate) use source::FileSource;
