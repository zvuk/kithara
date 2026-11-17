use kithara::analysis::MusicalKey;

/// What the library table shows about one track, as far as analysis knows it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct TrackFacts {
    pub(crate) bpm: Option<f64>,
    pub(crate) key: Option<MusicalKey>,
}
