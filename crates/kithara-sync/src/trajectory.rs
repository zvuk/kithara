use std::num::{NonZeroU16, NonZeroU32};

use kithara_signal::SessionFrame;

use crate::{SessionAnchor, SessionBeat, Tempo, TrajectoryError};

/// Host tempo over session time, retaining the executed approach at every target.
#[derive(Clone, Debug)]
pub struct TempoTrajectory {
    steps: Vec<SessionAnchor>,
    beats_per_bar: NonZeroU16,
}

/// One tempo change and the beat reached when it takes effect.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TempoStep {
    pub frame: SessionFrame,
    pub beat: SessionBeat,
    pub tempo: Tempo,
}

impl TempoTrajectory {
    /// Starts the trajectory at an anchor, extrapolating its tempo backwards.
    ///
    /// # Panics
    /// Panics if the initial anchor cannot represent the supplied tempo.
    #[must_use]
    pub fn new(start: TempoStep, beats_per_bar: NonZeroU16, sample_rate: NonZeroU32) -> Self {
        Self {
            steps: vec![
                SessionAnchor::new(
                    start.frame,
                    start.beat,
                    start.tempo.beats_per_second(),
                    sample_rate,
                )
                .unwrap_or_else(|error| {
                    panic!("initial session anchor is not representable: {error}")
                }),
            ],
            beats_per_bar,
        }
    }

    /// Inserts a tempo step and reanchors later steps without changing their frames.
    ///
    /// # Errors
    /// Returns a refusal for an occupied frame or a frame at or before the anchor.
    pub fn push(&mut self, frame: SessionFrame, tempo: Tempo) -> Result<(), TrajectoryError> {
        if frame <= self.steps[0].frame() {
            return Err(TrajectoryError::BeforeAnchor { frame });
        }
        let index = self.steps.partition_point(|step| step.frame() < frame);
        if self
            .steps
            .get(index)
            .is_some_and(|step| step.frame() == frame)
        {
            return Err(TrajectoryError::Occupied { frame });
        }
        let anchor = self.steps[index - 1].retarget_tempo(frame, tempo.beats_per_second())?;
        let mut successors = self.steps[index..].to_vec();
        let mut prior = anchor;
        for next in &mut successors {
            *next = prior.retarget_tempo(next.frame(), next.target_beats_per_second())?;
            prior = *next;
        }
        self.steps.insert(index, anchor);
        self.steps[index + 1..].copy_from_slice(&successors);
        Ok(())
    }

    /// Removes a refused step, retaining the initial anchor and continuous beats.
    pub fn withdraw(&mut self, frame: SessionFrame) {
        if let Some(index) = self.steps.iter().position(|step| step.frame() == frame)
            && index != 0
        {
            self.steps.remove(index);
            self.reanchor(index);
        }
    }

    /// The continuous beat at a session frame.
    ///
    /// # Panics
    /// Panics if the requested frame's beat is not representable on the session axis.
    #[must_use]
    pub fn beat_at(&self, frame: SessionFrame) -> SessionBeat {
        match self.anchor_at(frame).beat_at(frame) {
            Ok(beat) => beat,
            Err(error) => panic!("session beat is not representable: {error}"),
        }
    }

    /// The session frame at a beat, rounded to the nearest output frame.
    ///
    /// # Panics
    /// Panics if the requested beat lies outside the representable session axis.
    #[must_use]
    pub fn frame_at(&self, beat: SessionBeat) -> SessionFrame {
        self.try_frame_at(beat)
            .expect("session frame is not representable")
    }

    pub(crate) fn try_frame_at(&self, beat: SessionBeat) -> Option<SessionFrame> {
        let index = self
            .steps
            .partition_point(|step| step.beat() <= beat)
            .saturating_sub(1);
        self.steps[index].frame_at(beat).ok()
    }

    /// The reached tempo active on a frame.
    ///
    /// # Panics
    /// Panics if the reached tempo is not representable as a `Tempo`.
    #[must_use]
    pub fn tempo_at(&self, frame: SessionFrame) -> Tempo {
        Tempo::new(self.anchor_at(frame).tempo_at(frame) * 60.0)
            .unwrap_or_else(|error| panic!("reached session tempo is not representable: {error}"))
    }

    /// The accepted transport law governing this frame and following frames.
    #[must_use]
    pub fn anchor_at(&self, frame: SessionFrame) -> SessionAnchor {
        let index = self
            .steps
            .partition_point(|step| step.frame() <= frame)
            .saturating_sub(1);
        self.steps[index]
    }

    /// Accepted target frames whose laws depend on an earlier change.
    pub fn changes_after(&self, frame: SessionFrame) -> impl Iterator<Item = SessionFrame> + '_ {
        self.steps
            .iter()
            .map(SessionAnchor::frame)
            .filter(move |at| *at > frame)
    }

    /// Bar lines on or after `from`, while their frames are representable.
    pub fn downbeats(&self, from: SessionFrame) -> impl Iterator<Item = SessionFrame> + '_ {
        self.markers(from, f64::from(self.beats_per_bar.get()))
    }

    /// Beat lines on or after `from`, while their frames are representable.
    pub fn beats(&self, from: SessionFrame) -> impl Iterator<Item = SessionFrame> + '_ {
        self.markers(from, 1.0)
    }

    /// Reanchors the trajectory on observed session geometry and tempo.
    pub fn reaxis_observed(&mut self, anchor: SessionAnchor) {
        self.steps.clear();
        self.steps.push(anchor);
    }

    pub(crate) fn beats_per_bar(&self) -> f64 {
        f64::from(self.beats_per_bar.get())
    }

    /// The output sample rate used for beat and frame conversion.
    #[must_use]
    pub fn sample_rate(&self) -> NonZeroU32 {
        self.steps[0].sample_rate()
    }

    /// Changes the anchor tempo and keeps later tempo steps continuous.
    ///
    /// # Panics
    /// Panics if the new anchor or its successors are not representable.
    pub fn initial_tempo(&mut self, tempo: Tempo) {
        let first = self.steps[0];
        self.steps[0] = SessionAnchor::new(
            first.frame(),
            first.beat(),
            tempo.beats_per_second(),
            first.sample_rate(),
        )
        .unwrap_or_else(|error| panic!("initial session tempo is not representable: {error}"));
        self.reanchor(1);
    }

    fn reanchor(&mut self, from: usize) {
        for index in from..self.steps.len() {
            let next = self.steps[index];
            self.steps[index] = self.steps[index - 1]
                .retarget_tempo(next.frame(), next.target_beats_per_second())
                .unwrap_or_else(|error| {
                    panic!("successor session anchor is not representable: {error}")
                });
        }
    }

    fn markers(&self, from: SessionFrame, stride: f64) -> impl Iterator<Item = SessionFrame> + '_ {
        let first = (f64::from(self.beat_at(from)) / stride).floor() * stride;
        std::iter::successors(Some(first), move |beat| {
            let next = beat + stride;
            (next.is_finite() && next > *beat).then_some(next)
        })
        .map_while(|value| {
            let beat = SessionBeat::new(value).ok()?;
            self.try_frame_at(beat)
        })
        .filter(move |frame| *frame >= from)
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn tempo_steps_round_trip_and_withdraw_reanchors_their_successors() {
        let mut host = TempoTrajectory::new(
            TempoStep {
                frame: SessionFrame::new(0),
                beat: SessionBeat::default(),
                tempo: Tempo::DEFAULT,
            },
            NonZeroU16::new(4).expect("nonzero meter"),
            NonZeroU32::new(48_000).expect("nonzero rate"),
        );
        host.push(
            SessionFrame::new(96_000),
            Tempo::new(60.0).expect("valid tempo"),
        )
        .expect("new frame");
        host.push(
            SessionFrame::new(192_000),
            Tempo::new(180.0).expect("valid tempo"),
        )
        .expect("new frame");
        for frame in [-48_000, 0, 24_000, 96_000, 120_000, 192_000, 240_000] {
            let frame = SessionFrame::new(frame);
            assert_eq!(host.frame_at(host.beat_at(frame)), frame);
        }
        assert_eq!(f64::from(host.beat_at(SessionFrame::new(192_000))), 6.005);
        assert_eq!(
            host.downbeats(SessionFrame::new(1))
                .take(2)
                .collect::<Vec<_>>(),
            vec![SessionFrame::new(96_000), SessionFrame::new(224_080)]
        );
        assert_eq!(
            host.beats(SessionFrame::new(1)).next(),
            Some(SessionFrame::new(24_000))
        );
        assert!(
            host.push(SessionFrame::new(96_000), Tempo::DEFAULT)
                .is_err()
        );
        host.withdraw(SessionFrame::new(96_000));
        assert_eq!(f64::from(host.beat_at(SessionFrame::new(192_000))), 8.0);
        host.withdraw(SessionFrame::new(0));
        assert_eq!(host.tempo_at(SessionFrame::new(0)), Tempo::DEFAULT);
    }

    #[kithara::test]
    fn a_tempo_step_preserves_the_beat_at_its_commit_frame() {
        let mut host = TempoTrajectory::new(
            TempoStep {
                frame: SessionFrame::new(0),
                beat: SessionBeat::default(),
                tempo: Tempo::new(120.0).expect("valid tempo"),
            },
            NonZeroU16::new(4).expect("nonzero meter"),
            NonZeroU32::new(48_000).expect("nonzero rate"),
        );
        let now = SessionFrame::new(48_000);
        let before = host.beat_at(now);
        host.push(now, Tempo::new(90.0).expect("valid tempo"))
            .expect("unoccupied frame after the anchor");

        assert_eq!(host.beat_at(now), before);
        assert_eq!(f64::from(host.beat_at(now)), 2.0);
        assert_eq!(f64::from(host.beat_at(SessionFrame::new(96_000))), 3.5025);
        assert_eq!(host.tempo_at(now).beats_per_minute(), 120.0);
        assert_eq!(host.anchor_at(now).target_beats_per_second(), 1.5);
    }

    #[kithara::test]
    fn every_block_tempo_step_lands_at_its_requested_frame() {
        let mut host = TempoTrajectory::new(
            TempoStep {
                frame: SessionFrame::new(0),
                beat: SessionBeat::default(),
                tempo: Tempo::new(120.0).expect("valid tempo"),
            },
            NonZeroU16::new(4).expect("nonzero meter"),
            NonZeroU32::new(48_000).expect("nonzero rate"),
        );
        let mut reached = 124.0;
        let mut previous_target = 124.0;
        let mut rates = Vec::new();
        for step in 0..16_i64 {
            let target = if step % 2 == 0 { 124.0 } else { 116.0 };
            let at = SessionFrame::new(step * 128);
            let before = host.beat_at(at);
            let tempo = Tempo::new(target).expect("valid tempo");
            if step == 0 {
                host.initial_tempo(tempo);
            } else {
                reached =
                    previous_target + (reached - previous_target) * (-128.0_f64 / 240.0).exp();
                host.push(at, tempo).expect("each block has its own frame");
            }
            let played = host.tempo_at(at).beats_per_minute();

            assert!(
                (115.999..=124.001).contains(&played),
                "step {step}: tempo {played} left the knob range"
            );
            assert!((played - reached).abs() < 1e-12);
            assert_eq!(host.anchor_at(at).target_beats_per_second(), target / 60.0);
            assert_eq!(host.beat_at(at), before);
            rates.push(reached);
            previous_target = target;
        }
        for step in 0..16_i64 {
            assert!(
                (host
                    .tempo_at(SessionFrame::new(step * 128))
                    .beats_per_minute()
                    - rates[usize::try_from(step).expect("nonnegative step")])
                .abs()
                    < 1e-12
            );
        }
    }

    #[kithara::test]
    fn a_new_trajectory_starts_at_its_target_without_prior_geometry() {
        let host = TempoTrajectory::new(
            TempoStep {
                frame: SessionFrame::new(48_000),
                beat: SessionBeat::default(),
                tempo: Tempo::new(120.0).expect("valid tempo"),
            },
            NonZeroU16::new(4).expect("nonzero meter"),
            NonZeroU32::new(48_000).expect("nonzero rate"),
        );

        assert_eq!(f64::from(host.beat_at(SessionFrame::new(48_000))), 0.0);
        assert_eq!(f64::from(host.beat_at(SessionFrame::new(96_000))), 2.0);
        assert_eq!(
            host.tempo_at(SessionFrame::new(48_000)).beats_per_minute(),
            120.0
        );
    }

    #[kithara::test]
    fn refused_tempo_steps_name_the_anchor_or_occupied_frame() {
        let mut host = TempoTrajectory::new(
            TempoStep {
                frame: SessionFrame::new(0),
                beat: SessionBeat::default(),
                tempo: Tempo::DEFAULT,
            },
            NonZeroU16::new(4).expect("nonzero meter"),
            NonZeroU32::new(48_000).expect("nonzero rate"),
        );

        for frame in [-1, 0] {
            let frame = SessionFrame::new(frame);
            assert_eq!(
                host.push(frame, Tempo::DEFAULT),
                Err(TrajectoryError::BeforeAnchor { frame })
            );
        }
        let frame = SessionFrame::new(96_000);
        host.push(frame, Tempo::new(60.0).expect("valid tempo"))
            .expect("unoccupied frame");
        assert_eq!(
            host.push(frame, Tempo::DEFAULT),
            Err(TrajectoryError::Occupied { frame })
        );
        assert_eq!(host.tempo_at(frame).beats_per_minute(), 120.0);
        assert_eq!(host.anchor_at(frame).target_beats_per_second(), 1.0);
        assert_eq!(f64::from(host.beat_at(frame)), 4.0);
    }
}
