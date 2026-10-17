mod facts;
mod geometry;

pub use facts::{
    BeatEvidence, BeatMarker, BeatsPerMinute, BeatsPerMinuteError, Meter, MeterError, MeterFacts,
    SegmentFacts,
};
#[cfg(test)]
pub(crate) use geometry::partial_start;
pub use geometry::{
    MapRegion, MapRegionError, MapSegment, SegmentEndpoint, SegmentError, SegmentSet,
};
