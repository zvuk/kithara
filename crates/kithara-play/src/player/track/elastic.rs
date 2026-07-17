use std::ops::{Range, RangeInclusive};

use num_traits::ToPrimitive;
use smallvec::SmallVec;

use crate::{
    api::{SyncUnavailable, TrackBinding},
    session::render::RenderContext,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ElasticRenderRequest {
    request_id: u64,
    revision: u64,
    source_start: f64,
    source_end: f64,
    output_frames: usize,
}

impl ElasticRenderRequest {
    pub(crate) const fn request_id(self) -> u64 {
        self.request_id
    }

    pub(crate) const fn revision(self) -> u64 {
        self.revision
    }

    pub(crate) const fn source_start(self) -> f64 {
        self.source_start
    }

    pub(crate) const fn source_end(self) -> f64 {
        self.source_end
    }

    pub(crate) const fn output_frames(self) -> usize {
        self.output_frames
    }
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub(crate) enum ElasticPlanError {
    #[error("render output range is invalid for its context")]
    InvalidOutputRange,
    #[error("elastic render request must contain at least one output frame")]
    EmptyOutput,
    #[error("render context has no active session beat range")]
    SessionBeatsUnavailable,
    #[error("render context sample rate {context} does not match binding sample rate {binding}")]
    SampleRateMismatch { context: u32, binding: u32 },
    #[error("track binding could not resolve the requested beat: {0}")]
    Binding(#[from] SyncUnavailable),
    #[error("track beat {beat} lies outside the analysed marker domain")]
    OutsideMarkerDomain { beat: f64 },
    #[error("elastic render request crosses the marker boundary at track beat {boundary}")]
    MarkerBoundaryCrossing { boundary: f64 },
    #[error("elastic rate envelope [{minimum}, {maximum}] is invalid")]
    InvalidRateEnvelope { minimum: f64, maximum: f64 },
    #[error("source rate {rate} lies outside the elastic renderer envelope [{minimum}, {maximum}]")]
    UnsupportedRate {
        rate: f64,
        minimum: f64,
        maximum: f64,
    },
    #[error("render block crosses more than four analysed marker segments")]
    TooManySegments,
    #[error("analysed marker boundary cannot be placed inside the render block")]
    InvalidBoundarySplit,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ElasticRenderSegment {
    pub(crate) output_start: usize,
    pub(crate) request: ElasticRenderRequest,
}

pub(crate) fn plan_elastic_segments(
    binding: &TrackBinding,
    context: &RenderContext,
    output_range: Range<usize>,
    first_request_id: u64,
    revision: u64,
    supported_rates: RangeInclusive<f64>,
) -> Result<SmallVec<[ElasticRenderSegment; 4]>, ElasticPlanError> {
    const MAX_SEGMENTS: usize = 4;
    let mut pending = SmallVec::<[Range<usize>; 4]>::new();
    pending.push(output_range);
    let mut segments = SmallVec::<[ElasticRenderSegment; 4]>::new();

    while let Some(range) = pending.pop() {
        let request_id = first_request_id
            .checked_add(
                u64::try_from(segments.len()).map_err(|_| ElasticPlanError::TooManySegments)?,
            )
            .ok_or(ElasticPlanError::TooManySegments)?;
        match plan_elastic_render(
            binding,
            context,
            range.clone(),
            request_id,
            revision,
            supported_rates.clone(),
        ) {
            Ok(request) => {
                if segments.len() == MAX_SEGMENTS {
                    return Err(ElasticPlanError::TooManySegments);
                }
                segments.push(ElasticRenderSegment {
                    output_start: range.start,
                    request,
                });
            }
            Err(ElasticPlanError::MarkerBoundaryCrossing { boundary }) => {
                if pending.len() + segments.len() + 2 > MAX_SEGMENTS {
                    return Err(ElasticPlanError::TooManySegments);
                }
                let split = boundary_output_frame(binding, context, range.clone(), boundary)?;
                pending.push(split..range.end);
                pending.push(range.start..split);
            }
            Err(error) => return Err(error),
        }
    }
    segments.sort_unstable_by_key(|segment| segment.output_start);
    Ok(segments)
}

pub(crate) fn plan_elastic_render(
    binding: &TrackBinding,
    context: &RenderContext,
    output_range: Range<usize>,
    request_id: u64,
    revision: u64,
    supported_rates: RangeInclusive<f64>,
) -> Result<ElasticRenderRequest, ElasticPlanError> {
    let output_frames = output_range
        .end
        .checked_sub(output_range.start)
        .ok_or(ElasticPlanError::InvalidOutputRange)?;
    if output_frames == 0 {
        return Err(ElasticPlanError::EmptyOutput);
    }
    let context = context
        .for_output_range(output_range)
        .ok_or(ElasticPlanError::InvalidOutputRange)?;
    let context_rate = context.sample_rate().get();
    let binding_rate = binding.map().host_sample_rate().get();
    if context_rate != binding_rate {
        return Err(ElasticPlanError::SampleRateMismatch {
            context: context_rate,
            binding: binding_rate,
        });
    }
    let beats = context
        .session_beats()
        .ok_or(ElasticPlanError::SessionBeatsUnavailable)?;
    let track_start = binding.track_beat_at(beats.start)?;
    let track_end = binding.track_beat_at(beats.end)?;
    let source_start = binding
        .map()
        .source_frame_at(track_start)
        .ok_or_else(|| ElasticPlanError::OutsideMarkerDomain {
            beat: track_start.get(),
        })?
        .get();
    let source_end = binding
        .map()
        .source_frame_at(track_end)
        .ok_or_else(|| ElasticPlanError::OutsideMarkerDomain {
            beat: track_end.get(),
        })?
        .get();
    if let Some(boundary) = crossed_marker_boundary(track_start.get(), track_end.get()) {
        return Err(ElasticPlanError::MarkerBoundaryCrossing { boundary });
    }

    let (minimum, maximum) = supported_rates.into_inner();
    if !minimum.is_finite() || !maximum.is_finite() || minimum <= 0.0 || minimum > maximum {
        return Err(ElasticPlanError::InvalidRateEnvelope { minimum, maximum });
    }
    let output_frames_f64 = output_frames
        .to_f64()
        .ok_or(ElasticPlanError::InvalidOutputRange)?;
    let rate = (source_end - source_start).abs() / output_frames_f64;
    if rate < minimum || rate > maximum {
        return Err(ElasticPlanError::UnsupportedRate {
            rate,
            minimum,
            maximum,
        });
    }

    Ok(ElasticRenderRequest {
        request_id,
        revision,
        source_start,
        source_end,
        output_frames,
    })
}

fn crossed_marker_boundary(start: f64, end: f64) -> Option<f64> {
    let lower = start.min(end);
    let upper = start.max(end);
    let boundary = lower.floor() + 1.0;
    (boundary < upper).then_some(boundary)
}

fn boundary_output_frame(
    binding: &TrackBinding,
    context: &RenderContext,
    output_range: Range<usize>,
    boundary: f64,
) -> Result<usize, ElasticPlanError> {
    let subcontext = context
        .for_output_range(output_range.clone())
        .ok_or(ElasticPlanError::InvalidOutputRange)?;
    let beats = subcontext
        .session_beats()
        .ok_or(ElasticPlanError::SessionBeatsUnavailable)?;
    let start = binding.track_beat_at(beats.start)?.get();
    let end = binding.track_beat_at(beats.end)?.get();
    let fraction = (boundary - start) / (end - start);
    let offset = (fraction
        * output_range
            .len()
            .to_f64()
            .ok_or(ElasticPlanError::InvalidBoundarySplit)?)
    .round()
    .to_usize()
    .ok_or(ElasticPlanError::InvalidBoundarySplit)?;
    let split = output_range
        .start
        .checked_add(offset)
        .ok_or(ElasticPlanError::InvalidBoundarySplit)?;
    if split <= output_range.start || split >= output_range.end {
        return Err(ElasticPlanError::InvalidBoundarySplit);
    }
    Ok(split)
}

#[cfg(test)]
mod tests {
    use std::{
        num::NonZeroU32,
        ops::{Range, RangeInclusive},
    };

    use kithara_audio::{BeatGrid, TrackBeat, analysis::TrackAnalysis};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        api::{PlaybackDirection, SessionBeat, Tempo, TrackBinding},
        session::render::{RenderContext, RenderFrame, SessionTransportCommit},
    };

    struct TestSpec;

    impl TestSpec {
        const SAMPLE_RATE: u32 = 48_000;
        const OUTPUT_FRAMES: usize = 480;
        const REQUEST_ID: u64 = 17;
        const REVISION: u64 = 23;
        const MINIMUM_RATE: f64 = 2.0 / 3.0;
        const MAXIMUM_RATE: f64 = 4.0 / 3.0;

        fn supported_rates() -> RangeInclusive<f64> {
            Self::MINIMUM_RATE..=Self::MAXIMUM_RATE
        }
    }

    fn binding(track_tempo: f64) -> TrackBinding {
        let sample_rate = NonZeroU32::new(TestSpec::SAMPLE_RATE).expect("static sample rate");
        let beat_frames = (60.0 * f64::from(TestSpec::SAMPLE_RATE) / track_tempo)
            .to_u64()
            .expect("test tempo resolves to an integer frame span");
        let analysis = TrackAnalysis::with_source_rate(
            Some(BeatGrid::new(
                track_tempo,
                vec![
                    0,
                    beat_frames,
                    beat_frames * 2,
                    beat_frames * 3,
                    beat_frames * 4,
                ],
                vec![0],
                Vec::new(),
            )),
            None,
            beat_frames * 4,
            sample_rate,
        );
        TrackBinding::new(
            &analysis,
            sample_rate,
            SessionBeat::new(0.0).expect("finite session beat"),
            TrackBeat::new(0.0).expect("finite track beat"),
            PlaybackDirection::Forward,
        )
        .expect("valid track binding")
    }

    fn context(beats: Range<f64>) -> RenderContext {
        context_with_frames(beats, TestSpec::OUTPUT_FRAMES)
    }

    fn context_with_frames(beats: Range<f64>, output_frames: usize) -> RenderContext {
        RenderContext::new(
            RenderFrame::new(0)
                ..RenderFrame::new(
                    i64::try_from(output_frames).expect("block size fits the render axis"),
                ),
            NonZeroU32::new(TestSpec::SAMPLE_RATE).expect("static sample rate"),
            Some(
                SessionBeat::new(beats.start).expect("finite start beat")
                    ..SessionBeat::new(beats.end).expect("finite end beat"),
            ),
            Some(SessionTransportCommit::new(
                Tempo::new(120.0).expect("valid tempo"),
                true,
                TestSpec::REVISION,
            )),
        )
        .expect("valid render context")
    }

    #[kithara::test]
    fn maps_track_tempo_to_a_numeric_source_span() {
        let request = plan_elastic_render(
            &binding(100.0),
            &context(0.0..0.02),
            0..TestSpec::OUTPUT_FRAMES,
            TestSpec::REQUEST_ID,
            TestSpec::REVISION,
            TestSpec::supported_rates(),
        )
        .expect("supported elastic request");

        assert_eq!(request.request_id(), TestSpec::REQUEST_ID);
        assert_eq!(request.revision(), TestSpec::REVISION);
        assert_eq!(request.source_start(), 0.0);
        assert_eq!(request.source_end(), 576.0);
        assert_eq!(request.output_frames(), TestSpec::OUTPUT_FRAMES);
    }

    #[kithara::test]
    fn maps_only_the_requested_render_subrange() {
        let request = plan_elastic_render(
            &binding(100.0),
            &context(0.0..0.02),
            240..TestSpec::OUTPUT_FRAMES,
            TestSpec::REQUEST_ID,
            TestSpec::REVISION,
            TestSpec::supported_rates(),
        )
        .expect("supported elastic request");

        assert_eq!(request.source_start(), 288.0);
        assert_eq!(request.source_end(), 576.0);
        assert_eq!(request.output_frames(), 240);
    }

    #[kithara::test]
    fn rejects_an_internal_marker_boundary() {
        let error = plan_elastic_render(
            &binding(100.0),
            &context(0.75..1.25),
            0..TestSpec::OUTPUT_FRAMES,
            TestSpec::REQUEST_ID,
            TestSpec::REVISION,
            TestSpec::supported_rates(),
        )
        .expect_err("request must stay inside one linear marker segment");

        assert_eq!(
            error,
            ElasticPlanError::MarkerBoundaryCrossing { boundary: 1.0 }
        );
    }

    #[kithara::test]
    fn splits_one_marker_into_ordered_continuous_segments() {
        let segments = plan_elastic_segments(
            &binding(100.0),
            &context(0.99..1.01),
            0..TestSpec::OUTPUT_FRAMES,
            TestSpec::REQUEST_ID,
            TestSpec::REVISION,
            TestSpec::supported_rates(),
        )
        .expect("one marker boundary is split");

        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].output_start, 0);
        assert_eq!(segments[1].output_start, 240);
        assert_eq!(
            segments
                .iter()
                .map(|segment| segment.request.output_frames())
                .sum::<usize>(),
            TestSpec::OUTPUT_FRAMES
        );
        assert_eq!(
            segments[0].request.source_end(),
            segments[1].request.source_start()
        );
        assert_eq!(segments[0].request.request_id(), TestSpec::REQUEST_ID);
        assert_eq!(segments[1].request.request_id(), TestSpec::REQUEST_ID + 1);
    }

    #[kithara::test]
    fn keeps_four_marker_segments_inline() {
        const FRAMES: usize = 86_400;
        let segments = plan_elastic_segments(
            &binding(100.0),
            &context_with_frames(0.5..3.5, FRAMES),
            0..FRAMES,
            TestSpec::REQUEST_ID,
            TestSpec::REVISION,
            TestSpec::supported_rates(),
        )
        .expect("four marker segments fit the inline plan");

        assert_eq!(segments.len(), 4);
        assert!(!segments.spilled());
        assert_eq!(
            segments
                .iter()
                .map(|segment| segment.request.output_frames())
                .sum::<usize>(),
            FRAMES
        );
        assert!(segments.windows(2).all(|pair| {
            pair[0].output_start + pair[0].request.output_frames() == pair[1].output_start
                && pair[0].request.source_end() == pair[1].request.source_start()
        }));
    }

    #[kithara::test]
    fn accepts_a_marker_at_the_request_endpoint() {
        let request = plan_elastic_render(
            &binding(100.0),
            &context(0.98..1.0),
            0..TestSpec::OUTPUT_FRAMES,
            TestSpec::REQUEST_ID,
            TestSpec::REVISION,
            TestSpec::supported_rates(),
        )
        .expect("endpoint marker belongs to the current segment");

        assert_eq!(request.source_end(), 28_800.0);
    }

    #[kithara::test]
    fn rejects_a_rate_outside_the_renderer_envelope() {
        let error = plan_elastic_render(
            &binding(50.0),
            &context(0.0..0.02),
            0..TestSpec::OUTPUT_FRAMES,
            TestSpec::REQUEST_ID,
            TestSpec::REVISION,
            TestSpec::supported_rates(),
        )
        .expect_err("source rate exceeds the renderer envelope");

        assert_eq!(
            error,
            ElasticPlanError::UnsupportedRate {
                rate: 2.4,
                minimum: TestSpec::MINIMUM_RATE,
                maximum: TestSpec::MAXIMUM_RATE,
            }
        );
    }
}
