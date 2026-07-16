use crate::api::Tempo;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SessionTransportCommit {
    playing: bool,
    revision: u64,
    tempo: Tempo,
}

impl SessionTransportCommit {
    pub(crate) const fn new(tempo: Tempo, playing: bool, revision: u64) -> Self {
        Self {
            playing,
            revision,
            tempo,
        }
    }

    pub(crate) const fn is_playing(self) -> bool {
        self.playing
    }

    pub(crate) const fn revision(self) -> u64 {
        self.revision
    }

    pub(crate) const fn tempo(self) -> Tempo {
        self.tempo
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct RenderFrame(i64);

impl RenderFrame {
    pub(crate) const fn new(value: i64) -> Self {
        Self(value)
    }

    pub(crate) const fn get(self) -> i64 {
        self.0
    }
}
