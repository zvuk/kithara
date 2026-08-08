use kithara_events::PlaybackDirection;
use kithara_platform::sync::Arc;

use super::{CoordinateError, SessionAnchorCell, SourceFrame, TrackBeat, TrackBeatMap};

/// A deck's own output frame cannot be resolved to a source coordinate.
#[derive(Clone, Copy, Debug, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum ScheduleError {
    /// No transport commit has published a session grid to follow yet.
    #[error("no committed session grid to follow")]
    Unanchored,
    /// The composed track-beat coordinate is not representable.
    #[error("track beat {beats} beats into the deck is not representable")]
    TrackBeat {
        beats: f64,
        #[source]
        source: CoordinateError,
    },
    /// The resolved track beat lies outside the analysed marker domain.
    #[error("{beats} beats into the deck resolves outside the analysed beat map")]
    OutsideMap { beats: f64 },
}

/// One deck's binding projected onto that deck's own output frames.
///
/// A binding relates session beats to track beats and a beat map relates track
/// beats to source frames. The schedule owns neither relation's geometry: the
/// tempo is read live from [`SessionAnchorCell`] and the source side stays with
/// the analysed map, which is what keeps a drifting grid following its local
/// slope instead of an average tempo.
///
/// It carries no position either. A deck's place on the grid is *how far it
/// has advanced*, counted by the renderer that emits its frames — never a pin
/// on the session's frame axis. A pin would be correct only while that axis
/// stays put, and it does not: a resume moves beats and frames apart by the
/// length of the pause, which would silently reinterpret every frame the deck
/// had already rendered.
#[derive(Clone, Debug, fieldwork::Fieldwork)]
#[fieldwork(get)]
#[non_exhaustive]
pub struct SourceSchedule {
    /// Returns the analysed map this schedule reads.
    map: TrackBeatMap,
    /// Returns the track beat aligned with output frame zero.
    #[field(get, copy)]
    origin: TrackBeat,
    /// Returns the direction the source is walked in.
    #[field(get, copy)]
    direction: PlaybackDirection,
    anchor: Arc<SessionAnchorCell>,
}

impl SourceSchedule {
    /// Projects a binding onto a deck whose output frame zero plays `origin`,
    /// following whatever grid `anchor` publishes.
    #[must_use]
    pub fn new(
        map: TrackBeatMap,
        origin: TrackBeat,
        direction: PlaybackDirection,
        anchor: Arc<SessionAnchorCell>,
    ) -> Self {
        Self {
            map,
            origin,
            direction,
            anchor,
        }
    }

    /// Session beats one output frame advances at the committed tempo.
    ///
    /// # Errors
    ///
    /// Returns [`ScheduleError::Unanchored`] when no grid is committed.
    pub fn beats_per_frame(&self) -> Result<f64, ScheduleError> {
        Ok(self
            .anchor
            .load()
            .ok_or(ScheduleError::Unanchored)?
            .beats_per_frame())
    }

    /// The continuous source coordinate due after the deck has advanced
    /// `beats` session beats from its own start.
    ///
    /// # Errors
    ///
    /// Returns [`ScheduleError`] when the composed beat is not representable
    /// or falls outside the analysed marker domain.
    pub fn source_after(&self, beats: f64) -> Result<SourceFrame, ScheduleError> {
        let advance = match self.direction {
            PlaybackDirection::Forward => beats,
            PlaybackDirection::Reverse => -beats,
        };
        let beat = TrackBeat::new(f64::from(self.origin) + advance)
            .map_err(|source| ScheduleError::TrackBeat { beats, source })?;
        self.map
            .source_frame_at(beat)
            .ok_or(ScheduleError::OutsideMap { beats })
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_platform::sync::Arc;
    use kithara_test_utils::kithara;
    use num_traits::ToPrimitive;

    use super::{
        PlaybackDirection, ScheduleError, SessionAnchorCell, SourceSchedule, TrackBeat,
        TrackBeatMap,
    };
    use crate::{
        analysis::TrackAnalysis,
        musical::{SessionAnchor, SessionBeat, SessionFrame},
        waveform::BeatGrid,
    };

    struct Consts;

    impl Consts {
        /// Host rate the source-frame axis is expressed in.
        const RATE: u32 = 48_000;
        /// Frames between markers of the even 120 BPM fixture.
        const BEAT_FRAMES: u64 = 24_000;
    }

    fn rate() -> NonZeroU32 {
        NonZeroU32::new(Consts::RATE).expect("invariant: fixture rate is non-zero")
    }

    fn map_from(markers: Vec<u64>) -> TrackBeatMap {
        let source_frames = markers.last().copied().unwrap_or_default() + Consts::BEAT_FRAMES;
        let analysis = TrackAnalysis::with_source_rate(
            Some(BeatGrid::new(120.0, markers, vec![0], Vec::new())),
            None,
            source_frames,
            rate(),
        );
        TrackBeatMap::new(&analysis, rate()).expect("invariant: fixture markers form a map")
    }

    /// Markers every half second: an even 120 BPM grid.
    fn even_map(beats: u64) -> TrackBeatMap {
        map_from((0..beats).map(|k| k * Consts::BEAT_FRAMES).collect())
    }

    fn committed(session_bpm: f64) -> Arc<SessionAnchorCell> {
        let cell = SessionAnchorCell::new();
        cell.publish(
            SessionAnchor::new(
                SessionFrame::new(0),
                SessionBeat::default(),
                session_bpm / 60.0,
                rate(),
            )
            .expect("invariant: the fixture tempo is a positive rate"),
        );
        cell
    }

    fn schedule_on(map: TrackBeatMap, anchor: Arc<SessionAnchorCell>) -> SourceSchedule {
        SourceSchedule::new(
            map,
            TrackBeat::default(),
            PlaybackDirection::Forward,
            anchor,
        )
    }

    /// Tier A: one session beat of advance consumes exactly one track beat of
    /// source when the tempos match, with no accumulated rounding.
    #[kithara::test]
    fn a_beat_of_advance_consumes_its_own_marker_span() {
        let schedule = schedule_on(even_map(9), committed(120.0));

        for beats in 0..8_u64 {
            let landed = schedule
                .source_after(
                    beats
                        .to_f64()
                        .expect("invariant: fixture magnitudes are exact in f64"),
                )
                .expect("invariant: the marker is inside the analysed domain");

            assert_eq!(
                f64::from(landed),
                (beats * Consts::BEAT_FRAMES)
                    .to_f64()
                    .expect("invariant: fixture magnitudes are exact in f64"),
                "beat {beats} must land on its own source frame"
            );
        }
    }

    /// A grid that drifts must be followed by the slope of the segment the
    /// playhead is in, not by the track's average tempo.
    #[kithara::test]
    fn drifting_grid_follows_the_local_slope() {
        let slow = (u64::from(Consts::RATE) * 60) / 118;
        let fast = (u64::from(Consts::RATE) * 60) / 122;
        let mut markers = vec![0_u64];
        for _ in 0..4 {
            let last = *markers.last().expect("invariant: seeded with one marker");
            markers.push(last + slow);
        }
        for _ in 0..4 {
            let last = *markers.last().expect("invariant: seeded with one marker");
            markers.push(last + fast);
        }
        let schedule = schedule_on(map_from(markers), committed(120.0));

        let early = advance_over_one_beat(&schedule, 0.0);
        let late = advance_over_one_beat(&schedule, 6.0);

        assert!(
            (early
                - slow
                    .to_f64()
                    .expect("invariant: fixture magnitudes are exact in f64"))
            .abs()
                < 1.0,
            "the first segment must advance at its own spacing, got {early}"
        );
        assert!(
            (late
                - fast
                    .to_f64()
                    .expect("invariant: fixture magnitudes are exact in f64"))
            .abs()
                < 1.0,
            "the later segment must advance at its own spacing, got {late}"
        );
    }

    fn advance_over_one_beat(schedule: &SourceSchedule, from: f64) -> f64 {
        let start = schedule
            .source_after(from)
            .expect("invariant: inside the analysed domain");
        let end = schedule
            .source_after(from + 1.0)
            .expect("invariant: inside the analysed domain");
        f64::from(end) - f64::from(start)
    }

    /// Past the last marker the schedule has no answer, and says so rather
    /// than extrapolating one.
    #[kithara::test]
    fn advance_past_the_analysed_domain_is_typed() {
        let schedule = schedule_on(even_map(4), committed(120.0));

        assert_eq!(
            schedule.source_after(100.0),
            Err(ScheduleError::OutsideMap { beats: 100.0 })
        );
    }

    /// A reverse binding walks the source backwards from the same origin.
    #[kithara::test]
    fn reverse_binding_walks_the_source_backwards() {
        let reverse = SourceSchedule::new(
            even_map(9),
            TrackBeat::new(4.0).expect("invariant: four is a finite beat"),
            PlaybackDirection::Reverse,
            committed(120.0),
        );

        let behind = reverse
            .source_after(1.0)
            .expect("invariant: inside the analysed domain");

        assert_eq!(
            f64::from(behind),
            3.0 * Consts::BEAT_FRAMES
                .to_f64()
                .expect("invariant: fixture magnitudes are exact in f64")
        );
    }

    /// A deck with no committed grid has nowhere to put its content, and says
    /// so rather than assuming a tempo of its own.
    #[kithara::test]
    fn a_deck_with_no_committed_grid_has_no_beat_advance() {
        let schedule = schedule_on(even_map(9), SessionAnchorCell::new());

        assert_eq!(schedule.beats_per_frame(), Err(ScheduleError::Unanchored));
    }

    /// The defect this wave exists for: after a tempo commit the deck must
    /// advance at the new rate.
    #[kithara::test]
    fn the_beat_advance_follows_a_tempo_commit() {
        let anchor = committed(120.0);
        let schedule = schedule_on(even_map(33), Arc::clone(&anchor));
        let before = schedule
            .beats_per_frame()
            .expect("invariant: the fixture grid is committed");

        anchor.publish(
            SessionAnchor::new(
                SessionFrame::new(0),
                SessionBeat::default(),
                240.0 / 60.0,
                rate(),
            )
            .expect("invariant: the fixture tempo is a positive rate"),
        );

        let after = schedule
            .beats_per_frame()
            .expect("invariant: the recommitted grid is readable");
        assert!(
            (after - before * 2.0).abs() < f64::EPSILON,
            "twice the tempo must advance twice as fast per frame, got {after} from {before}"
        );
    }
}
