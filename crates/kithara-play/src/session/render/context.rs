use std::{num::NonZeroU32, ops::Range};

use num_traits::ToPrimitive;

use crate::api::{SessionBeat, Tempo};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SessionTransportCommit {
    playing: bool,
    revision: u64,
    seek_target: Option<SessionBeat>,
    tempo: Tempo,
}

impl SessionTransportCommit {
    pub(crate) const fn new(tempo: Tempo, playing: bool, revision: u64) -> Self {
        Self {
            playing,
            revision,
            seek_target: None,
            tempo,
        }
    }

    pub(crate) const fn new_at_beat(
        tempo: Tempo,
        playing: bool,
        revision: u64,
        seek_target: SessionBeat,
    ) -> Self {
        Self {
            playing,
            revision,
            seek_target: Some(seek_target),
            tempo,
        }
    }

    pub(crate) const fn is_playing(self) -> bool {
        self.playing
    }

    pub(crate) const fn revision(self) -> u64 {
        self.revision
    }

    pub(crate) const fn seek_target(self) -> Option<SessionBeat> {
        self.seek_target
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

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RenderContext {
    render_frames: Range<RenderFrame>,
    sample_rate: NonZeroU32,
    session_beats: Option<Range<SessionBeat>>,
    transport_commit: Option<SessionTransportCommit>,
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
            render_frames,
            sample_rate,
            session_beats,
            transport_commit,
        })
    }

    pub(crate) fn render_frames(&self) -> &Range<RenderFrame> {
        &self.render_frames
    }

    pub(crate) const fn sample_rate(&self) -> NonZeroU32 {
        self.sample_rate
    }

    pub(crate) const fn transport_revision(&self) -> Option<u64> {
        match self.transport_commit {
            Some(commit) => Some(commit.revision()),
            None => None,
        }
    }

    pub(crate) const fn transport_seek_target(&self) -> Option<SessionBeat> {
        match self.transport_commit {
            Some(commit) => commit.seek_target(),
            None => None,
        }
    }

    #[cfg(any(not(target_arch = "wasm32"), test))]
    pub(crate) fn session_beats(&self) -> Option<&Range<SessionBeat>> {
        self.session_beats.as_ref()
    }

    #[cfg(any(not(target_arch = "wasm32"), test))]
    pub(crate) const fn transport_commit(&self) -> Option<SessionTransportCommit> {
        self.transport_commit
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
