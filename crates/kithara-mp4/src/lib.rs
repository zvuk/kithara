//! Fragmented-mp4 box walk over a random-access byte source.
//!
//! An mp4 index needs box headers, not payload. The walk reads each header
//! and seeks over the box it does not need, so `mdat` - every byte of the
//! track - is never transferred and peak memory tracks the layout rather
//! than the track length.
//!
//! Times come out as media ticks against the track's timescale. Converting
//! them to a clock type, and projecting fragments onto whatever per-segment
//! descriptor a protocol uses, belongs to the caller.

#![forbid(unsafe_code)]

mod cursor;
mod error;
mod layout;
mod samples;

#[cfg(test)]
mod fixture;

pub use cursor::ReadAt;
pub use error::Mp4Error;
pub use layout::{Fmp4Layout, Fragment};
pub use samples::{Sample, read_samples};
mod consts;
