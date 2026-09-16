use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write,
};

use kithara::warp::{
    AssetFrame, Beat, BeatGridQuery, BeatGridSnapshot, BeatOrdinal, MapPoint, MapPosition,
};
use num_traits::ToPrimitive;
use serde::Serialize;

use crate::{underrun_ledger::UnderrunLedger, usdt_trace::ProbeEvent};

const WIDTH: u64 = 1_280;
const LEFT: u64 = 180;
const RIGHT: u64 = 24;
const LANE_HEIGHT: u64 = 52;

/// One observable point or span on a test artifact's session timeline.
#[derive(Clone, Debug, Serialize)]
pub struct TimelineEvent {
    pub lane: String,
    pub kind: String,
    pub label: String,
    pub output_start: u64,
    pub output_end: u64,
    pub source_start: Option<u64>,
    pub source_end: Option<u64>,
}

/// Test-owned coordinates rendered beside listening audio.
#[derive(Default, Serialize)]
pub struct ArtifactTimeline {
    events: Vec<TimelineEvent>,
}

impl ArtifactTimeline {
    pub fn point(&mut self, lane: &str, frame: u64, kind: &str, label: &str) {
        self.span(lane, frame, frame, kind, label, None);
    }

    pub fn span(
        &mut self,
        lane: &str,
        output_start: u64,
        output_end: u64,
        kind: &str,
        label: &str,
        source: Option<(u64, u64)>,
    ) {
        let (source_start, source_end) =
            source.map_or((None, None), |(start, end)| (Some(start), Some(end)));
        self.events.push(TimelineEvent {
            lane: lane.to_owned(),
            kind: kind.to_owned(),
            label: label.to_owned(),
            output_start,
            output_end: output_end.max(output_start),
            source_start,
            source_end,
        });
    }

    pub fn record_probes(&mut self, probes: &[ProbeEvent]) {
        for probe in probes {
            match probe.probe {
                "render" => self.record_render(probe),
                "pcm_consumed" => self.record_pcm(probe),
                "warp_plan_published" => self.record_warp_plan(probe),
                _ => {}
            }
        }
        self.compact();
    }

    pub fn record_source_grid(&mut self, track: u64, grid: &BeatGridSnapshot) {
        let spans = self
            .events
            .iter()
            .filter(|event| {
                event.lane == format!("pcm-track-{track}")
                    && event.source_start.is_some()
                    && event.source_end.is_some()
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut recorded = BTreeSet::new();
        for span in spans {
            let (Some(source_start), Some(source_end)) = (span.source_start, span.source_end)
            else {
                continue;
            };
            if source_end <= source_start || span.output_end < span.output_start {
                continue;
            }
            let Some(first) = source_beat(grid, source_start)
                .map(f64::ceil)
                .and_then(|v| v.to_i64())
            else {
                continue;
            };
            let Some(last) = source_beat(grid, source_end)
                .map(f64::floor)
                .and_then(|v| v.to_i64())
            else {
                continue;
            };
            for ordinal in first..=last {
                let Ok(beat) = Beat::try_from(BeatOrdinal::new(ordinal)) else {
                    continue;
                };
                let BeatGridQuery::Resolved(position) =
                    grid.position_at(MapPoint::new(grid.stamp(), beat))
                else {
                    continue;
                };
                let MapPosition::Asset(source) = *position.value().value() else {
                    continue;
                };
                let source: f64 = source.into();
                let Some(source) = source.round().to_u64() else {
                    continue;
                };
                if source < source_start || source > source_end {
                    continue;
                }
                let source_offset = source - source_start;
                let output_len = span.output_end - span.output_start;
                let source_len = source_end - source_start;
                let mapped = (u128::from(source_offset) * u128::from(output_len)
                    + u128::from(source_len / 2))
                    / u128::from(source_len);
                let output = span
                    .output_start
                    .saturating_add(u64::try_from(mapped).unwrap_or(u64::MAX));
                if !recorded.insert((ordinal, output)) {
                    continue;
                }
                let kind = match grid.meter_at(MapPoint::new(grid.stamp(), beat)) {
                    BeatGridQuery::Resolved(meter)
                        if (ordinal - i64::from(meter.value().downbeat()))
                            .rem_euclid(i64::from(meter.value().beats_per_bar()))
                            == 0 =>
                    {
                        "downbeat"
                    }
                    _ => "beat",
                };
                self.span(
                    &format!("source-grid-track-{track}"),
                    output,
                    output,
                    "grid",
                    &format!("mapped source {kind} {ordinal}"),
                    Some((source, source)),
                );
            }
        }
        self.compact();
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    #[must_use]
    pub fn events(&self) -> &[TimelineEvent] {
        &self.events
    }

    #[must_use]
    pub fn svg(&self) -> String {
        let lanes = self
            .events
            .iter()
            .fold(BTreeMap::new(), |mut lanes, event| {
                let next = lanes.len();
                lanes.entry(event.lane.as_str()).or_insert(next);
                lanes
            });
        let start = self
            .events
            .iter()
            .map(|event| event.output_start)
            .min()
            .unwrap_or(0);
        let end = self
            .events
            .iter()
            .map(|event| event.output_end)
            .max()
            .unwrap_or(start)
            .max(start + 1);
        let height = 42 + LANE_HEIGHT * u64::try_from(lanes.len()).unwrap_or(0);
        let plot_width = WIDTH - LEFT - RIGHT;
        let x = |frame: u64| LEFT + frame.saturating_sub(start) * plot_width / (end - start);
        let mut svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {WIDTH} {height}\"><style>text{{font:12px monospace;fill:#d7dde8}}.lane{{stroke:#465064}}.grid{{stroke:#70809b}}.command{{stroke:#f4c95d}}.planned{{stroke:#80cbc4}}.presented{{stroke:#81c784}}.error{{stroke:#ef5350}}.underrun{{stroke:#ef5350;stroke-width:7}}.event{{stroke-width:3}}</style><rect width=\"100%\" height=\"100%\" fill=\"#11151d\"/>"
        );
        for (lane, index) in &lanes {
            let y = 30 + u64::try_from(*index).unwrap_or(0) * LANE_HEIGHT;
            let _ = write!(
                svg,
                "<text x=\"8\" y=\"{}\">{}</text><line class=\"lane\" x1=\"{LEFT}\" x2=\"{}\" y1=\"{y}\" y2=\"{y}\"/>",
                y + 4,
                escape(lane),
                WIDTH - RIGHT,
            );
        }
        for event in &self.events {
            let y = 30 + u64::try_from(lanes[event.lane.as_str()]).unwrap_or(0) * LANE_HEIGHT;
            let x1 = x(event.output_start);
            let x2 = x(event.output_end).max(x1 + 1);
            let title = event_title(event);
            let _ = write!(
                svg,
                "<line class=\"event {}\" x1=\"{x1}\" x2=\"{x2}\" y1=\"{y}\" y2=\"{y}\"><title>{}</title></line>",
                escape(&event.kind),
                escape(&title),
            );
        }
        svg.push_str("</svg>");
        svg
    }

    fn record_render(&mut self, probe: &ProbeEvent) {
        let Some(base) = probe.field("output_base").filter(|base| *base != u64::MAX) else {
            return;
        };
        let start = base.saturating_add(probe.field("range_start").unwrap_or(0));
        let Some(frames) = probe.field("rendered_frames").filter(|frames| *frames > 0) else {
            return;
        };
        let track = probe.field("track_id").unwrap_or(0);
        self.span(
            &format!("track-{track}"),
            start,
            start.saturating_add(frames),
            "presented",
            "rendered PCM",
            None,
        );
    }

    fn record_pcm(&mut self, probe: &ProbeEvent) {
        let (Some(output_start), Some(output_end), Some(source_start), Some(source_end)) = (
            probe.field("output_start"),
            probe.field("output_end"),
            probe.field("source_start"),
            probe.field("source_end"),
        ) else {
            return;
        };
        let revision = probe.field("render_revision").unwrap_or(0);
        self.span(
            &format!("pcm-revision-{revision}"),
            output_start,
            output_end,
            "presented",
            &format!("consumed PCM revision {revision}"),
            Some((source_start, source_end)),
        );
    }

    /// Place the silenced suffix of every starved render on its track's lane.
    ///
    /// Each span starts where real PCM stopped and ends where the block did: the
    /// drawn interval is the silence an oracle later reads as a missing event.
    pub fn record_underruns(&mut self, ledger: &UnderrunLedger) {
        for event in &ledger.events {
            let silence = event.silence();
            let lane = event.track_id.map_or_else(
                || "pcm-untracked".to_owned(),
                |track| format!("pcm-track-{track}"),
            );
            let source_end = event
                .source_end
                .map_or_else(String::new, |frame| format!(", source end {frame}"));
            self.span(
                &lane,
                silence.start,
                silence.end,
                "underrun",
                &format!(
                    "silenced {} of {} frames{source_end}",
                    silence.end - silence.start,
                    event.requested_frames,
                ),
                None,
            );
        }
        self.compact();
    }

    fn record_warp_plan(&mut self, probe: &ProbeEvent) {
        let (Some(output), Some(source)) = (
            probe.field("activation_output"),
            probe.field("activation_source"),
        ) else {
            return;
        };
        self.span(
            "warp-plan",
            output,
            output,
            "planned",
            "map activation",
            Some((source, source)),
        );
    }

    fn compact(&mut self) {
        self.events.sort_by(|left, right| {
            (
                left.lane.as_str(),
                left.output_start,
                left.output_end,
                left.kind.as_str(),
                left.label.as_str(),
            )
                .cmp(&(
                    right.lane.as_str(),
                    right.output_start,
                    right.output_end,
                    right.kind.as_str(),
                    right.label.as_str(),
                ))
        });
        let mut compacted: Vec<TimelineEvent> = Vec::with_capacity(self.events.len());
        for event in self.events.drain(..) {
            let merge = compacted.last_mut().filter(|previous| {
                previous.lane == event.lane
                    && previous.kind == event.kind
                    && previous.label == event.label
                    && previous.output_end == event.output_start
                    && previous.source_end == event.source_start
            });
            if let Some(previous) = merge {
                previous.output_end = event.output_end;
                previous.source_end = event.source_end;
            } else {
                compacted.push(event);
            }
        }
        self.events = compacted;
    }
}

fn source_beat(grid: &BeatGridSnapshot, frame: u64) -> Option<f64> {
    let frame = AssetFrame::new(frame.to_f64()?).ok()?;
    let BeatGridQuery::Resolved(beat) =
        grid.beat_at(MapPoint::new(grid.stamp(), MapPosition::Asset(frame)))
    else {
        return None;
    };
    Some((*beat.value().value()).into())
}

fn event_title(event: &TimelineEvent) -> String {
    match (event.source_start, event.source_end) {
        (Some(start), Some(end)) => format!(
            "{}: output {}..{}, source {}..{}",
            event.label, event.output_start, event.output_end, start, end
        ),
        _ => format!(
            "{}: output {}..{}",
            event.label, event.output_start, event.output_end
        ),
    }
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
