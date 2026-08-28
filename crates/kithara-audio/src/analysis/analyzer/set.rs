use std::num::NonZeroU32;

use kithara_bufpool::PcmPool;
use kithara_resampler::ResamplerBackend;

use super::{
    config::BeatAnalysisConfig,
    session::TrackAnalyzers,
    track::{AnalysisFingerprint, AnalysisToken},
};
use crate::{
    analysis::slots::{
        beat::{self, Config},
        waveform,
    },
    coverage::Coverage,
};

#[derive(Default, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct AnalyzerBuilder<B>
where
    B: ResamplerBackend,
{
    beat: Config<B>,
    waveform: waveform::Config,
    beat_config: Option<BeatAnalysisConfig<B>>,
    #[field(get, vis = "pub(crate)")]
    pcm_pool: PcmPool,
}

impl<B> AnalyzerBuilder<B>
where
    B: ResamplerBackend,
{
    pub(crate) fn build(&self, rate: NonZeroU32, token: AnalysisToken) -> TrackAnalyzers<B> {
        TrackAnalyzers {
            beat: Config::build(&self.beat, rate, &self.pcm_pool),
            waveform: waveform::build(&self.waveform, rate, &self.pcm_pool),
            coverage: Coverage::default(),
            fingerprint: self.fingerprint(),
            extent: None,
            revision: 0,
            settled: false,
            source_sample_rate: rate,
            token,
        }
    }

    /// What this configuration produces, per artifact. The two tags are
    /// separate so a waveform resolution change cannot invalidate stored beat
    /// results.
    #[must_use]
    pub fn fingerprint(&self) -> AnalysisFingerprint {
        AnalysisFingerprint::new(
            self.beat_config
                .as_ref()
                .and_then(BeatAnalysisConfig::cache_tag)
                .as_deref(),
            waveform::cache_tag(&self.waveform).as_deref(),
        )
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn run_frames(&self, rate: NonZeroU32) -> Option<u64> {
        let seconds = self.beat_config.as_ref()?.ready_seconds();
        Some(u64::from(rate.get()).saturating_mul(u64::from(seconds)))
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        waveform::config_is_empty(&self.waveform) && Config::is_empty(&self.beat)
    }

    pub(crate) fn take_detector(&mut self) -> Option<beat::Detector> {
        Config::take_detector(&mut self.beat)
    }

    #[must_use]
    pub fn with_beat(self) -> Self
    where
        B: Default,
    {
        let mut builder = self;
        let beat_config = builder.beat_config.clone().unwrap_or_default();
        Config::with_default(&mut builder.beat, beat_config.clone());
        builder.beat_config = Some(beat_config);
        builder
    }

    #[must_use]
    pub fn with_beat_config(self, config: BeatAnalysisConfig<B>) -> Self {
        let mut builder = self;
        builder.beat_config = Some(config.clone());
        Config::set_resampler(&mut builder.beat, config);
        builder
    }

    #[cfg(all(test, feature = "analysis-beat"))]
    pub(crate) fn with_beat_detector(
        self,
        detector: Box<dyn crate::analysis::beat::BeatDetector>,
        params: crate::analysis::beat::GridParams,
    ) -> Self
    where
        B: Default,
    {
        let mut builder = self;
        let beat_config = builder.beat_config.clone().unwrap_or_default();
        builder
            .beat
            .with_detector(detector, params, beat_config.clone());
        builder.beat_config = Some(beat_config);
        builder
    }

    #[must_use]
    pub fn with_pcm_pool(self, pool: PcmPool) -> Self {
        Self {
            pcm_pool: pool,
            ..self
        }
    }

    #[must_use]
    #[cfg(feature = "analysis-waveform")]
    pub const fn with_waveform(self, buckets: usize) -> Self {
        let mut builder = self;
        waveform::with_buckets(&mut builder.waveform, buckets);
        builder
    }
}

#[cfg(all(test, feature = "analysis-beat", feature = "analysis-waveform"))]
mod tests {
    use std::num::NonZeroU32;

    use kithara_bufpool::PcmPool;
    use kithara_decode::{PcmChunk, PcmMeta, PcmSpec};
    use kithara_platform::sync::Arc;
    use kithara_resampler::{NoResamplerBackend, rubato::RubatoBackend};
    use kithara_test_utils::kithara;
    use unimock::{MockFn, Unimock, matching};

    use super::{
        super::{
            session::{Ingest, TrackAnalyzers},
            snapshot::GridState,
        },
        AnalyzerBuilder, BeatAnalysisConfig,
    };
    use crate::{
        analysis::beat::{BeatDetector, BeatDetectorMock, BeatMark, GridParams, RawBeats},
        coverage::FrameRange,
    };

    fn spec() -> PcmSpec {
        PcmSpec {
            channels: 2,
            sample_rate: NonZeroU32::new(44_100).expect("test sample rate is non-zero"),
        }
    }

    fn chunk(frames: usize, at: u64) -> PcmChunk {
        let samples = vec![0.0_f32; frames * 2];
        PcmChunk::new(
            PcmMeta {
                spec: spec(),
                frames: u32::try_from(frames).unwrap_or(0),
                frame_offset: at,
                ..Default::default()
            },
            PcmPool::default().attach(samples),
        )
    }

    fn beat_detector() -> Box<dyn BeatDetector> {
        let raw = RawBeats {
            beats: Vec::new(),
            downbeats: (0..9u8).map(|n| BeatMark::at(f32::from(n) * 2.0)).collect(),
        };
        let mock = Unimock::new(
            BeatDetectorMock
                .next_call(matching!(_))
                .answers_arc(Arc::new(move |_, _| Ok(raw.clone()))),
        );
        Box::new(mock)
    }

    fn waveform_pass(buckets: usize) -> TrackAnalyzers<NoResamplerBackend> {
        AnalyzerBuilder::<NoResamplerBackend>::default()
            .with_waveform(buckets)
            .build(spec().sample_rate, "track-a".into())
    }

    #[kithara::test(native, flash(false))]
    fn a_waveform_pass_publishes_a_waveform_and_no_beat() {
        let mut analyzers = waveform_pass(8);
        analyzers.push(&chunk(8192, 0), None);

        let snapshot = analyzers.snapshot(None, true);
        assert!(snapshot.waveform().is_some(), "the waveform slot is filled");
        assert!(snapshot.beat().is_none(), "no beat pass was configured");
    }

    #[kithara::test(native, flash(false))]
    fn a_beat_pass_publishes_both_artifacts_at_once() {
        let mut builder = AnalyzerBuilder::<RubatoBackend>::default()
            .with_waveform(8)
            .with_beat_detector(beat_detector(), GridParams::default());
        let mut detector = builder.take_detector();
        let mut analyzers = builder.build(spec().sample_rate, "track-a".into());
        analyzers.push(&chunk(8192, 0), detector.as_mut());

        let snapshot = analyzers.snapshot(detector.as_mut(), true);
        assert!(snapshot.waveform().is_some(), "the waveform is published");
        assert!(
            snapshot.beat().is_some(),
            "the grid rides the same snapshot"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_rejected_range_leaves_the_coverage_alone() {
        let mut analyzers = waveform_pass(8);
        analyzers.push(&chunk(8192, 0), None);
        analyzers.snapshot(None, true);
        let covered = analyzers.snapshot(None, false).coverage().clone();

        // Past the extent the pass pinned at end of stream.
        assert_eq!(
            analyzers.push(&chunk(8192, 8192), None),
            Ingest::OutOfExtent
        );
        assert_eq!(
            analyzers.snapshot(None, false).coverage(),
            &covered,
            "a rejected range must not move the coverage"
        );

        // A rate the pass was not opened with.
        let foreign = PcmChunk::new(
            PcmMeta {
                spec: PcmSpec {
                    channels: 2,
                    sample_rate: NonZeroU32::new(48_000).expect("test rate is non-zero"),
                },
                frames: 1024,
                frame_offset: 0,
                ..Default::default()
            },
            PcmPool::default().attach(vec![0.0_f32; 2048]),
        );
        assert_eq!(analyzers.push(&foreign, None), Ingest::ForeignRate);
        assert_eq!(
            analyzers.snapshot(None, false).coverage(),
            &covered,
            "a foreign rate must not move the coverage"
        );

        // Already covered.
        assert_eq!(analyzers.push(&chunk(8192, 0), None), Ingest::Covered);
        assert_eq!(analyzers.snapshot(None, false).coverage(), &covered);
    }

    #[kithara::test(native, flash(false))]
    fn a_pass_keeps_the_axis_it_was_opened_on() {
        // Opened at 48 kHz; the reader turns out to decode at 44.1 kHz.
        let axis = NonZeroU32::new(48_000).expect("test rate is non-zero");
        let mut analyzers = AnalyzerBuilder::<NoResamplerBackend>::default()
            .with_waveform(8)
            .build(axis, "track-a".into());

        assert_eq!(
            analyzers.push(&chunk(8192, 0), None),
            Ingest::ForeignRate,
            "the first chunk does not get to redefine the axis"
        );

        let snapshot = analyzers.snapshot(None, false);
        assert_eq!(
            snapshot.source_sample_rate(),
            axis,
            "the snapshot is measured on the axis the pass was opened with"
        );
        assert_eq!(
            snapshot.coverage().frames(),
            0,
            "a range on another axis is not covered"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_range_no_one_covered_is_missing_until_it_arrives() {
        let mut analyzers = waveform_pass(8);
        // A producer was starved over [8192, 16384) and carried on past it.
        analyzers.push(&chunk(8192, 0), None);
        analyzers.push(&chunk(8192, 16_384), None);

        assert_eq!(
            analyzers.snapshot(None, false).missing(),
            vec![FrameRange::new(8192, 8192)],
            "the hole is known to exist because something landed past it"
        );

        analyzers.push(&chunk(8192, 8192), None);
        assert!(
            analyzers.snapshot(None, false).missing().is_empty(),
            "a range taken on a second offer leaves the missing set"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_run_is_sized_by_the_window_the_detector_really_uses() {
        let rate = NonZeroU32::new(44_100).unwrap_or(NonZeroU32::MIN);
        let sized = |window, overlap| {
            AnalyzerBuilder::<NoResamplerBackend>::default()
                .with_beat_config(
                    BeatAnalysisConfig::builder()
                        .resampler_backend(NoResamplerBackend)
                        .detector_window_seconds(window)
                        .detector_overlap_seconds(overlap)
                        .build(),
                )
                .run_frames(rate)
        };

        assert_eq!(sized(2, 1), Some(3 * 44_100), "window plus its overlap");
        assert_eq!(
            sized(2, 7),
            Some(3 * 44_100),
            "an overlap wider than its window is clamped to one short of it"
        );
        assert_eq!(
            sized(0, 5),
            Some(44_100),
            "a window of nothing is one second, and takes no overlap with it"
        );
        assert_eq!(
            AnalyzerBuilder::<NoResamplerBackend>::default().run_frames(rate),
            None,
            "a pass with no beat configuration names no window"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_scattered_coverage_is_measured_against_where_it_reaches() {
        let mut analyzers = waveform_pass(8);
        // A range decoded away from the start, which is what a schedule
        // covers first.
        analyzers.push(&chunk(8192, 65_536), None);

        let snapshot = analyzers.snapshot(None, false);
        assert_eq!(
            snapshot.source_frames(),
            73_728,
            "the denominator must reach the covered range, not just count it"
        );
        assert!(
            snapshot
                .coverage()
                .runs()
                .iter()
                .all(|run| run.end() <= snapshot.source_frames()),
            "no covered frame may sit past the denominator it is divided by"
        );
    }

    #[kithara::test(native, flash(false))]
    fn nothing_past_the_frontier_is_claimed_missing() {
        let mut analyzers = waveform_pass(8);
        analyzers.push(&chunk(8192, 0), None);

        assert!(
            analyzers.snapshot(None, false).missing().is_empty(),
            "a pass that has not been told how long the track is claims nothing beyond what it saw"
        );

        // End of stream pins the extent to the frontier, so still nothing.
        assert!(analyzers.snapshot(None, true).missing().is_empty());
    }

    #[kithara::test(native, flash(false))]
    fn revisions_strictly_increase_across_publications() {
        let mut analyzers = waveform_pass(8);
        let mut revisions = Vec::new();
        for block in 0..3u64 {
            analyzers.push(&chunk(8192, block * 8192), None);
            revisions.push(analyzers.snapshot(None, false).revision());
        }
        revisions.push(analyzers.snapshot(None, true).revision());

        assert!(
            revisions.windows(2).all(|pair| pair[1] > pair[0]),
            "each publication must outrank the last: {revisions:?}"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_snapshot_carries_the_token_its_pass_was_opened_with() {
        let mut first = AnalyzerBuilder::<NoResamplerBackend>::default()
            .with_waveform(8)
            .build(spec().sample_rate, "track-a".into());
        let mut second = AnalyzerBuilder::<NoResamplerBackend>::default()
            .with_waveform(8)
            .build(spec().sample_rate, "track-b".into());
        first.push(&chunk(8192, 0), None);
        second.push(&chunk(8192, 0), None);

        assert_eq!(first.snapshot(None, true).token().as_str(), "track-a");
        assert_eq!(second.snapshot(None, true).token().as_str(), "track-b");
    }

    #[kithara::test(native, flash(false))]
    fn a_waveform_resolution_change_leaves_the_beat_fingerprint_alone() {
        let fingerprint = |buckets: usize| {
            AnalyzerBuilder::<RubatoBackend>::default()
                .with_waveform(buckets)
                .with_beat()
                .build(spec().sample_rate, "track-a".into())
                .snapshot(None, false)
                .fingerprint()
                .clone()
        };

        let coarse = fingerprint(64);
        let fine = fingerprint(2048);
        assert_eq!(
            coarse.beat(),
            fine.beat(),
            "the bucket count is not part of beat identity"
        );
        assert_ne!(
            coarse.waveform(),
            fine.waveform(),
            "the bucket count is part of waveform identity"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_grid_is_provisional_until_the_extent_is_covered() {
        let mut builder = AnalyzerBuilder::<RubatoBackend>::default()
            .with_waveform(8)
            .with_beat_detector(beat_detector(), GridParams::default());
        let mut detector = builder.take_detector();
        let mut analyzers = builder.build(spec().sample_rate, "track-a".into());
        analyzers.push(&chunk(8192, 0), detector.as_mut());

        let early = analyzers.snapshot(detector.as_mut(), false);
        assert!(early.extent().is_none(), "the extent is not known yet");
        assert!(
            early
                .beat()
                .is_none_or(|beat| beat.state() == GridState::Provisional),
            "a grid without a known extent cannot be final"
        );

        let ended = analyzers.snapshot(detector.as_mut(), true);
        assert_eq!(ended.extent(), Some(8192), "end of stream pins the extent");
        assert!(
            ended
                .beat()
                .is_none_or(|beat| beat.state() == GridState::Final),
            "the whole extent is covered, so the grid is final"
        );
        assert_eq!(
            ended.waveform_completeness(),
            Some(1.0),
            "a fully covered extent is a complete waveform"
        );
    }
}
