use std::{num::NonZeroU32, ops::Range};

use num_traits::ToPrimitive;

use crate::api::{SessionBeat, Tempo, TransportRevision};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) enum TransportBoundary {
    #[default]
    Continuous,
    Relocate(SessionBeat),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SessionTransportCommit {
    tempo: Tempo,
    boundary: TransportBoundary,
    revision: TransportRevision,
    playing: bool,
}

impl SessionTransportCommit {
    pub(crate) const fn new(tempo: Tempo, playing: bool, revision: TransportRevision) -> Self {
        Self {
            boundary: TransportBoundary::Continuous,
            tempo,
            playing,
            revision,
        }
    }

    pub(crate) const fn boundary(self) -> TransportBoundary {
        self.boundary
    }

    pub(crate) const fn is_playing(self) -> bool {
        self.playing
    }

    pub(crate) const fn relocate(
        tempo: Tempo,
        playing: bool,
        revision: TransportRevision,
        target: SessionBeat,
    ) -> Self {
        Self {
            boundary: TransportBoundary::Relocate(target),
            tempo,
            playing,
            revision,
        }
    }

    pub(crate) const fn revision(self) -> TransportRevision {
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

#[derive(Clone, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct RenderContext {
    sample_rate: NonZeroU32,
    session_beats: Option<Range<SessionBeat>>,
    transport_commit: Option<SessionTransportCommit>,
    #[field(get, vis = "pub(crate)")]
    render_frames: Range<RenderFrame>,
}

impl RenderContext {
    pub(crate) fn new(
        render_frames: Range<RenderFrame>,
        sample_rate: NonZeroU32,
        session_beats: Option<Range<SessionBeat>>,
        transport_commit: Option<SessionTransportCommit>,
    ) -> Option<Self> {
        let transport_matches_beats = match (session_beats.is_some(), transport_commit) {
            (true, Some(commit)) => commit.is_playing(),
            (false, Some(commit)) => !commit.is_playing(),
            (false, None) => true,
            (true, None) => false,
        };
        (render_frames.start <= render_frames.end && transport_matches_beats).then_some(Self {
            sample_rate,
            session_beats,
            transport_commit,
            render_frames,
        })
    }

    pub(crate) fn beat_is_before_output(&self, target: SessionBeat) -> bool {
        self.session_beats
            .as_ref()
            .is_some_and(|beats| target < beats.start)
    }

    pub(crate) fn for_output_range(&self, range: Range<usize>) -> Option<Self> {
        let total_frames = self
            .render_frames
            .end
            .get()
            .checked_sub(self.render_frames.start.get())?;
        let total_frames = usize::try_from(total_frames).ok()?;
        if range.start > range.end || range.end > total_frames {
            return None;
        }
        let start_offset = i64::try_from(range.start).ok()?;
        let end_offset = i64::try_from(range.end).ok()?;
        let render_start = self.render_frames.start.get().checked_add(start_offset)?;
        let render_end = self.render_frames.start.get().checked_add(end_offset)?;
        let session_beats = match self.session_beats.as_ref() {
            Some(beats) => Some(beat_subrange(beats, range, total_frames)?),
            None => None,
        };
        Self::new(
            RenderFrame::new(render_start)..RenderFrame::new(render_end),
            self.sample_rate,
            session_beats,
            self.transport_commit,
        )
    }

    pub(crate) fn output_offset_for_beat(&self, target: SessionBeat) -> Option<usize> {
        let beats = self.session_beats.as_ref()?;
        let frames = self
            .render_frames
            .end
            .get()
            .checked_sub(self.render_frames.start.get())
            .and_then(|frames| usize::try_from(frames).ok())?;
        if target == beats.start {
            return Some(0);
        }
        if target < beats.start || target >= beats.end || frames == 0 {
            return None;
        }
        let span = beats.end.get() - beats.start.get();
        if span <= 0.0 {
            return None;
        }
        let exact = (target.get() - beats.start.get()) * frames.to_f64()? / span;
        let nearest = exact.round();
        let offset = if (exact - nearest).abs() <= 1.0e-6 {
            nearest
        } else {
            exact.ceil()
        }
        .to_usize()?;
        (offset < frames).then_some(offset)
    }

    pub(crate) const fn sample_rate(&self) -> NonZeroU32 {
        self.sample_rate
    }

    #[cfg(any(not(target_arch = "wasm32"), test))]
    pub(crate) fn session_beats(&self) -> Option<&Range<SessionBeat>> {
        self.session_beats.as_ref()
    }

    pub(crate) const fn transport_boundary(&self) -> Option<TransportBoundary> {
        match self.transport_commit {
            Some(commit) => Some(commit.boundary()),
            None => None,
        }
    }

    #[cfg(any(not(target_arch = "wasm32"), test))]
    pub(crate) const fn transport_commit(&self) -> Option<SessionTransportCommit> {
        self.transport_commit
    }

    pub(crate) const fn transport_revision(&self) -> Option<TransportRevision> {
        match self.transport_commit {
            Some(commit) => Some(commit.revision()),
            None => None,
        }
    }
}

fn beat_subrange(
    beats: &Range<SessionBeat>,
    range: Range<usize>,
    total_frames: usize,
) -> Option<Range<SessionBeat>> {
    if total_frames == 0 {
        return (range.is_empty() && range.start == 0).then_some(beats.start..beats.start);
    }
    let span = beats.end.get() - beats.start.get();
    let total = total_frames.to_f64()?;
    let start = beats.start.get() + span * range.start.to_f64()? / total;
    let end = beats.start.get() + span * range.end.to_f64()? / total;
    Some(SessionBeat::new(start).ok()?..SessionBeat::new(end).ok()?)
}
