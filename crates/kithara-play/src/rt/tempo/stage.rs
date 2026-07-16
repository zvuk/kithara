use crate::{api::Tempo, rt::StreamShape};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TempoStage {
    playing: bool,
    previous_revision: Option<u64>,
    revision: u64,
    shape: StreamShape,
    tempo: Tempo,
}

impl TempoStage {
    pub(crate) const fn new(
        revision: u64,
        previous_revision: Option<u64>,
        tempo: Tempo,
        playing: bool,
        shape: StreamShape,
    ) -> Self {
        Self {
            playing,
            previous_revision,
            revision,
            shape,
            tempo,
        }
    }

    pub(crate) const fn is_playing(self) -> bool {
        self.playing
    }

    pub(crate) const fn previous_revision(self) -> Option<u64> {
        self.previous_revision
    }

    pub(crate) const fn revision(self) -> u64 {
        self.revision
    }

    pub(crate) const fn shape(self) -> StreamShape {
        self.shape
    }

    pub(crate) const fn tempo(self) -> Tempo {
        self.tempo
    }
}
