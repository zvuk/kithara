use std::num::{NonZeroU16, NonZeroU32};

use kithara_signal::SessionFrame;
use kithara_warp::{SessionAnchor, SessionBeat};

use crate::{Tempo, TrajectoryError};

/// Host tempo over session time, with continuous beats at every tempo step.
#[derive(Clone, Debug)]
pub struct TempoTrajectory {
    steps: Vec<TempoStep>,
    beats_per_bar: NonZeroU16,
    sample_rate: NonZeroU32,
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
    #[must_use]
    pub fn new(start: TempoStep, beats_per_bar: NonZeroU16, sample_rate: NonZeroU32) -> Self {
        Self {
            steps: vec![start],
            beats_per_bar,
            sample_rate,
        }
    }

    /// Inserts a tempo step and reanchors later steps without changing their frames.
    ///
    /// # Errors
    /// Returns a refusal for an occupied frame or a frame at or before the anchor.
    pub fn push(&mut self, frame: SessionFrame, tempo: Tempo) -> Result<(), TrajectoryError> {
        if frame <= self.steps[0].frame {
            return Err(TrajectoryError::BeforeAnchor { frame });
        }
        let index = self.steps.partition_point(|step| step.frame < frame);
        if self
            .steps
            .get(index)
            .is_some_and(|step| step.frame == frame)
        {
            return Err(TrajectoryError::Occupied { frame });
        }
        let beat = self.beat_at(frame);
        self.steps.insert(index, TempoStep { frame, beat, tempo });
        self.reanchor(index + 1);
        Ok(())
    }

    /// Removes a refused step, retaining the initial anchor and continuous beats.
    pub fn withdraw(&mut self, frame: SessionFrame) {
        if let Some(index) = self.steps.iter().position(|step| step.frame == frame)
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
        let index = self
            .steps
            .partition_point(|step| step.frame <= frame)
            .saturating_sub(1);
        match self.anchor(self.steps[index]).beat_at(frame) {
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
            .unwrap_or_else(|| panic!("session frame is not representable"))
    }

    pub(crate) fn try_frame_at(&self, beat: SessionBeat) -> Option<SessionFrame> {
        let index = self
            .steps
            .partition_point(|step| step.beat <= beat)
            .saturating_sub(1);
        self.anchor(self.steps[index]).frame_at(beat).ok()
    }

    /// The piecewise-constant tempo active on a frame.
    #[must_use]
    pub fn tempo_at(&self, frame: SessionFrame) -> Tempo {
        let index = self
            .steps
            .partition_point(|step| step.frame <= frame)
            .saturating_sub(1);
        self.steps[index].tempo
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
    pub fn reaxis_observed(&mut self, anchor: SessionAnchor, tempo: Tempo) {
        self.sample_rate = anchor.sample_rate();
        self.steps.clear();
        self.steps.push(TempoStep {
            frame: anchor.frame(),
            beat: anchor.beat(),
            tempo,
        });
    }

    pub(crate) fn beats_per_bar(&self) -> f64 {
        f64::from(self.beats_per_bar.get())
    }

    /// The output sample rate used for beat and frame conversion.
    #[must_use]
    pub fn sample_rate(&self) -> NonZeroU32 {
        self.sample_rate
    }

    /// Changes the anchor tempo and keeps later tempo steps continuous.
    pub fn initial_tempo(&mut self, tempo: Tempo) {
        self.steps[0].tempo = tempo;
        self.reanchor(1);
    }

    fn anchor(&self, step: TempoStep) -> SessionAnchor {
        match SessionAnchor::new(
            step.frame,
            step.beat,
            step.tempo.beats_per_second(),
            self.sample_rate,
        ) {
            Ok(anchor) => anchor,
            Err(error) => unreachable!("validated tempo and sample rate define an anchor: {error}"),
        }
    }

    fn reanchor(&mut self, from: usize) {
        for index in from..self.steps.len() {
            self.steps[index].beat = match self
                .anchor(self.steps[index - 1])
                .beat_at(self.steps[index].frame)
            {
                Ok(beat) => beat,
                Err(error) => panic!("session beat is not representable: {error}"),
            };
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
        assert_eq!(f64::from(host.beat_at(SessionFrame::new(192_000))), 6.0);
        assert_eq!(
            host.downbeats(SessionFrame::new(1))
                .take(2)
                .collect::<Vec<_>>(),
            vec![SessionFrame::new(96_000), SessionFrame::new(224_000)]
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
        assert_eq!(f64::from(host.beat_at(SessionFrame::new(96_000))), 3.5);
        assert_eq!(host.tempo_at(now).beats_per_minute(), 90.0);
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
        for step in 0..16_i64 {
            let target = if step % 2 == 0 { 124.0 } else { 116.0 };
            let at = SessionFrame::new(step * 128);
            let before = host.beat_at(at);
            let tempo = Tempo::new(target).expect("valid tempo");
            if step == 0 {
                host.initial_tempo(tempo);
            } else {
                host.push(at, tempo).expect("each block has its own frame");
            }
            let played = host.tempo_at(at).beats_per_minute();

            assert!(
                (115.999..=124.001).contains(&played),
                "step {step}: tempo {played} left the knob range"
            );
            assert_eq!(played, target);
            assert_eq!(host.beat_at(at), before);
        }
        for step in 0..16_i64 {
            let target = if step % 2 == 0 { 124.0 } else { 116.0 };
            assert_eq!(
                host.tempo_at(SessionFrame::new(step * 128))
                    .beats_per_minute(),
                target
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
        assert_eq!(host.tempo_at(frame).beats_per_minute(), 60.0);
        assert_eq!(f64::from(host.beat_at(frame)), 4.0);
    }
}
