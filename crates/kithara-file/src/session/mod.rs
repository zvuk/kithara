#![forbid(unsafe_code)]

//! Per-session state and Source/Peer implementations for the file protocol.
//!
//! Split into focused submodules:
//! - [`peer`] — `Peer` trait implementation (`FilePeer`); owns the
//!   single full-file fetch driven by the Downloader.
//! - [`inner`] — owned mutable state (`FileInner`, `FilePhase`, `FileStreamState`).
//! - [`source`] — public `FileSource` API and `Source` trait implementation.

mod inner;
mod peer;
mod reader;
mod segments;
mod source;
#[cfg(test)]
mod tests;

pub(crate) use inner::{FileAssetCtx, FileInner, FilePhase, FileSourceCtx, FileStreamState};
pub(crate) use peer::FilePeer;
pub(crate) use source::FileSource;
