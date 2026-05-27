//! MP4 box scanner — streaming visitor over `moov` for sample-rate /
//! edit-list / iTunSMPB / mdhd metadata.

mod scan;

pub(crate) use scan::{
    ItunSmpb, Mp4EditListEntry, Mp4MediaTiming, Mp4MetadataError, Mp4Visitor, scan_mp4,
    sniff_mp4_codec,
};
