//! MP4 box scanner — streaming visitor over `moov` for sample-rate /
//! edit-list / iTunSMPB / mdhd metadata.

mod boxes;
mod event;
mod freeform;
mod parse;
#[cfg(feature = "symphonia")]
mod presentation;
mod scan;
#[cfg(test)]
mod tests;

pub(crate) use event::{ItunSmpb, Mp4EditListEntry, Mp4Event, Mp4MediaTiming};
#[cfg(feature = "symphonia")]
pub(crate) use presentation::{Presentation, probe_presentations};
pub(crate) use scan::{Mp4MetadataError, scan_mp4, sniff_mp4_codec, sniff_mp4_fragmented};
