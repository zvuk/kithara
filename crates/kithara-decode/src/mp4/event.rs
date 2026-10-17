/// Media-timing pair extracted from an `mdhd` box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Mp4MediaTiming {
    pub(crate) timescale: u32,
    pub(crate) duration: u64,
}

/// Single `elst` edit-list entry normalized into integer fields.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Mp4EditListEntry {
    pub(crate) media_time: i64,
    pub(crate) segment_duration: u64,
    pub(crate) media_rate: i32,
}

/// Typed iTunes "iTunSMPB" payload. The four hex fields are: encoder version
/// (ignored), encoder delay (front padding), encoder padding (trailing
/// silence), and total non-padding sample count. We only surface the two that
/// matter for gapless trimming.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ItunSmpb {
    pub(crate) leading_frames: u64,
    pub(crate) trailing_frames: u64,
}

/// Metadata emitted in box order by the streaming MP4 scanner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mp4Event<'a> {
    ItunSmpb(ItunSmpb),
    MovieTimescale(u32),
    TrackBegin,
    TrackId(u32),
    TrackCodec([u8; 4]),
    TrackEditList(&'a [Mp4EditListEntry]),
    TrackEnd,
    TrackMediaTiming(Mp4MediaTiming),
    TrackSampleRate(u32),
}
