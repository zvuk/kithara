//! Starved renders a capture observed, read at their own output frames.

use std::ops::Range;

use serde::Serialize;

use crate::usdt_trace::ProbeEvent;

/// One `pcm_underrun` probe with the fields a silence interval needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct UnderrunEvent {
    pub track_id: Option<u64>,
    pub output_start: u64,
    pub requested_frames: u64,
    pub available_frames: u64,
    pub source_end: Option<u64>,
}

impl UnderrunEvent {
    const PROBE: &'static str = "pcm_underrun";

    /// Output frames the feeder filled with silence.
    #[must_use]
    pub fn silence(&self) -> Range<u64> {
        let start = self
            .output_start
            .saturating_add(self.available_frames.min(self.requested_frames));
        start..self.output_start.saturating_add(self.requested_frames)
    }

    fn from_probe(probe: &ProbeEvent) -> Option<Self> {
        Some(Self {
            track_id: probe.field("track_id"),
            output_start: probe.field("output_start")?,
            requested_frames: probe.field("requested_frames")?,
            available_frames: probe.field("available_frames")?,
            source_end: probe.field("source_end"),
        })
    }
}

/// One track's starvation over a capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct UnderrunTrack {
    /// `None` gathers probes that carried no track.
    pub track_id: Option<u64>,
    pub underruns: u64,
    pub silenced_frames: u64,
    pub first_output_frame: u64,
    pub last_output_frame: u64,
}

/// Every starved render a capture observed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct UnderrunLedger {
    pub events: Vec<UnderrunEvent>,
    /// Probes that fired without an interval field. A negative output start is never
    /// captured as a number, so it is counted here instead of reading as frame zero.
    pub unparsed: usize,
}

impl UnderrunLedger {
    #[must_use]
    pub fn from_probes(probes: &[ProbeEvent]) -> Self {
        let mut ledger = Self::default();
        for probe in probes
            .iter()
            .filter(|probe| probe.probe == UnderrunEvent::PROBE)
        {
            match UnderrunEvent::from_probe(probe) {
                Some(event) => ledger.events.push(event),
                None => ledger.unparsed += 1,
            }
        }
        ledger
    }

    /// Per-track totals, ordered by track id.
    #[must_use]
    pub fn tracks(&self) -> Vec<UnderrunTrack> {
        let mut tracks: Vec<UnderrunTrack> = Vec::new();
        for event in &self.events {
            let silence = event.silence();
            let known = tracks
                .iter()
                .position(|track| track.track_id == event.track_id);
            let index = known.unwrap_or_else(|| {
                tracks.push(UnderrunTrack {
                    track_id: event.track_id,
                    underruns: 0,
                    silenced_frames: 0,
                    first_output_frame: silence.start,
                    last_output_frame: silence.end,
                });
                tracks.len() - 1
            });
            let track = &mut tracks[index];
            track.underruns += 1;
            track.silenced_frames += silence.end - silence.start;
            track.first_output_frame = track.first_output_frame.min(silence.start);
            track.last_output_frame = track.last_output_frame.max(silence.end);
        }
        tracks.sort_by_key(|track| track.track_id);
        tracks
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::usdt_trace;

    /// Fire one `pcm_underrun` the way the feeder does, through the probe wire.
    fn fire(fields: &[(&'static str, u64)]) {
        fire_named(UnderrunEvent::PROBE, fields);
    }

    fn fire_named(probe: &'static str, fields: &[(&'static str, u64)]) {
        let value = |name: &str| {
            fields
                .iter()
                .find_map(|(key, value)| (*key == name).then_some(*value))
        };
        match (
            value("track_id"),
            value("output_start"),
            value("requested_frames"),
            value("available_frames"),
        ) {
            (track_id, Some(output_start), Some(requested), Some(available)) => tracing::event!(
                target: "kithara_play_probe",
                tracing::Level::TRACE,
                probe = probe,
                track_id = track_id,
                output_start = output_start,
                requested_frames = requested,
                available_frames = available
            ),
            (track_id, None, Some(requested), Some(available)) => tracing::event!(
                target: "kithara_play_probe",
                tracing::Level::TRACE,
                probe = probe,
                track_id = track_id,
                requested_frames = requested,
                available_frames = available
            ),
            _ => panic!("fixture names the fill counts"),
        }
    }

    #[kithara::test]
    fn a_starved_block_is_read_at_the_frames_it_silenced() {
        let trace = usdt_trace::scope();
        fire(&[
            ("track_id", 3),
            ("output_start", 1_000),
            ("requested_frames", 128),
            ("available_frames", 28),
        ]);
        fire(&[
            ("track_id", 3),
            ("output_start", 1_128),
            ("requested_frames", 128),
            ("available_frames", 0),
        ]);

        let ledger = UnderrunLedger::from_probes(&trace.events());

        assert_eq!(ledger.unparsed, 0);
        assert_eq!(ledger.events[0].silence(), 1_028..1_128);
        assert_eq!(
            ledger.tracks(),
            [UnderrunTrack {
                track_id: Some(3),
                underruns: 2,
                silenced_frames: 228,
                first_output_frame: 1_028,
                last_output_frame: 1_256,
            }],
        );
    }

    #[kithara::test]
    fn a_probe_without_an_interval_is_counted_not_placed_at_frame_zero() {
        let trace = usdt_trace::scope();
        fire(&[
            ("track_id", 3),
            ("requested_frames", 128),
            ("available_frames", 0),
        ]);

        let ledger = UnderrunLedger::from_probes(&trace.events());

        assert!(ledger.events.is_empty());
        assert_eq!(ledger.unparsed, 1);
    }

    #[kithara::test]
    fn probes_of_another_name_are_not_underruns() {
        let trace = usdt_trace::scope();
        fire_named(
            "pcm_consumed",
            &[
                ("track_id", 3),
                ("output_start", 64),
                ("requested_frames", 128),
                ("available_frames", 0),
            ],
        );

        let ledger = UnderrunLedger::from_probes(&trace.events());

        assert_eq!(ledger, UnderrunLedger::default());
    }

    #[kithara::test]
    fn a_block_that_delivered_every_frame_silences_nothing() {
        let trace = usdt_trace::scope();
        fire(&[
            ("track_id", 3),
            ("output_start", 64),
            ("requested_frames", 128),
            ("available_frames", 128),
        ]);

        let ledger = UnderrunLedger::from_probes(&trace.events());

        assert!(ledger.events[0].silence().is_empty());
    }

    #[kithara::test]
    fn a_probe_without_a_track_stays_untracked() {
        let trace = usdt_trace::scope();
        fire(&[
            ("output_start", 64),
            ("requested_frames", 128),
            ("available_frames", 64),
        ]);
        let ledger = UnderrunLedger::from_probes(&trace.events());

        assert_eq!(ledger.unparsed, 0);
        assert_eq!(
            ledger.tracks(),
            [UnderrunTrack {
                track_id: None,
                underruns: 1,
                silenced_frames: 64,
                first_output_frame: 128,
                last_output_frame: 192,
            }]
        );
    }
}
