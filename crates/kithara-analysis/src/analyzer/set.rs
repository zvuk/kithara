use std::num::{NonZeroU32, NonZeroU64};

use kithara_bufpool::{HasPool, PoolRegion};
use kithara_resampler::ResamplerBackend;
use kithara_waveform::AnalyzerError;
use rangemap::RangeSet;

use super::{
    AnalysisDemand, AnalysisFingerprint, AnalysisToken, config::BeatAnalysisConfig,
    session::TrackAnalyzers,
};
use crate::{
    AnalysisProgress, BlobError,
    slots::{
        beat::{self, Config, Slot},
        waveform,
    },
};

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
#[derive_where::derive_where(Clone; B: ResamplerBackend)]
pub struct AnalyzerBuilder<B, S>
where
    B: ResamplerBackend,
{
    beat: Config<B>,
    beat_config: Option<BeatAnalysisConfig<B>>,
    /// The bucket ceiling a waveform is asked to fill, when one is wanted at
    /// all. No waveform configured means no waveform slot is ever built.
    waveform: Option<usize>,
    #[field(get, vis = "pub(crate)")]
    pools: PoolRegion<S>,
}

impl<B, S> AnalyzerBuilder<B, S>
where
    B: ResamplerBackend,
    S: HasPool<f32> + Send + Sync + 'static,
{
    /// Starts an analyzer configuration with caller-owned pooled storage.
    #[must_use]
    pub fn new(pools: PoolRegion<S>) -> Self {
        Self {
            pools,
            beat: Config::default(),
            waveform: None,
            beat_config: None,
        }
    }

    pub(crate) fn build(
        &self,
        rate: NonZeroU32,
        token: AnalysisToken,
        revision: u64,
        demand: AnalysisDemand,
    ) -> Result<TrackAnalyzers<B, S>, AnalyzerError> {
        Ok(TrackAnalyzers {
            revision,
            token,
            beat: if demand.beat() {
                self.beat.build(rate, &self.pools)
            } else {
                Slot::default()
            },
            waveform: match self.waveform.filter(|_| demand.waveform()) {
                Some(buckets) => waveform::Slot::try_from((buckets, rate, &self.pools))?,
                None => waveform::Slot::default(),
            },
            coverage: RangeSet::new(),
            fingerprint: self.fingerprint_for(demand),
            settled: false,
            source_sample_rate: rate,
            media_origin: None,
            visible_origin: None,
            pools: self.pools.clone(),
        })
    }

    /// What this configuration produces, per artifact. The two tags are
    /// separate so a waveform resolution change cannot invalidate stored beat
    /// results.
    #[must_use]
    pub fn fingerprint(&self) -> AnalysisFingerprint {
        self.fingerprint_for(AnalysisDemand::ALL)
    }

    /// What a pass opened for `demand` produces. A narrowed pass carries a
    /// narrowed fingerprint, so its result is never read as a full one.
    #[must_use]
    pub fn fingerprint_for(&self, demand: AnalysisDemand) -> AnalysisFingerprint {
        AnalysisFingerprint::new(
            demand
                .beat()
                .then(|| {
                    self.beat_config
                        .as_ref()
                        .and_then(BeatAnalysisConfig::cache_tag)
                })
                .flatten()
                .as_deref(),
            demand
                .waveform()
                .then(|| waveform::cache_tag(self.waveform))
                .flatten()
                .as_deref(),
        )
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.waveform.is_none() && self.beat.is_empty()
    }

    pub(crate) fn restore(
        &self,
        progress: &AnalysisProgress,
        chunk_frames: NonZeroU64,
    ) -> Result<TrackAnalyzers<B, S>, BlobError> {
        let analysis = progress.analysis();
        if analysis.fingerprint() != &self.fingerprint() {
            return Err(BlobError::Fingerprint);
        }
        let resume = progress.decode_resume()?.ok_or(BlobError::Corrupt)?;
        let mut analyzers = self
            .build(
                analysis.source_sample_rate(),
                analysis.token().clone(),
                analysis.revision(),
                AnalysisDemand::ALL,
            )
            .map_err(|_| BlobError::Corrupt)?;
        analyzers.restore(analysis, resume, chunk_frames)?;
        Ok(analyzers)
    }

    pub(crate) fn resume_shape(&self) -> (bool, bool) {
        (self.waveform.is_some(), !self.beat.is_empty())
    }

    pub(crate) fn take_detector(&mut self) -> Option<beat::Detector> {
        let beat_enabled = !self.beat.is_empty();
        let detector = self.beat.take_detector(&self.pools);
        if beat_enabled && detector.is_none() {
            self.beat_config = None;
        }
        detector
    }

    #[must_use]
    pub fn with_beat(self) -> Self
    where
        B: Default,
    {
        let mut builder = self;
        let beat_config = builder.beat_config.clone().unwrap_or_default();
        builder.beat.with_default(beat_config.clone());
        builder.beat_config = Some(beat_config);
        builder
    }

    #[must_use]
    pub fn with_beat_config(self, config: BeatAnalysisConfig<B>) -> Self {
        let mut builder = self;
        builder.beat_config = Some(config.clone());
        builder.beat.set_resampler(config);
        builder
    }

    #[cfg(all(test, feature = "analysis-beat"))]
    pub(crate) fn with_beat_detector(
        self,
        detector: Box<dyn crate::beat::BeatDetector>,
        params: crate::beat::GridParams,
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
    #[cfg(feature = "analysis-waveform")]
    pub const fn with_waveform(self, buckets: usize) -> Self {
        let mut builder = self;
        builder.waveform = Some(buckets);
        builder
    }
}

#[cfg(all(
    test,
    feature = "analysis-beat",
    feature = "analysis-waveform",
    not(target_arch = "wasm32")
))]
mod tests {
    use std::num::{NonZeroU32, NonZeroU64};

    use kithara_platform::{sync::Arc, time::Duration};
    use kithara_resampler::{NoResamplerBackend, rubato::RubatoBackend};
    use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec, FrameCoverage};
    use kithara_test_fixtures::analysis_fixtures::analysis_silence;
    use kithara_test_utils::kithara;
    use kithara_waveform::Waveform;
    use num_traits::cast::ToPrimitive;
    use unimock::{MockFn, Unimock, matching};

    use super::{
        super::{
            demand::AnalysisDemand,
            extent::Extent,
            session::{Ingest, TrackAnalyzers},
        },
        AnalyzerBuilder,
    };
    use crate::{
        BeatGridModel, BeatGridState, BeatState,
        beat::{BeatDetector, BeatDetectorMock, BeatMark, GridParams, RawBeats},
        test_pools::{Pools, TestPools, pools, sample_buffer},
    };

    fn spec() -> AudioSpec {
        AudioSpec {
            channels: 2,
            sample_rate: NonZeroU32::new(44_100).expect("test sample rate is non-zero"),
        }
    }

    fn chunk(pools: &Pools, pcm: &[f32], frames: usize, at: u64) -> AudioChunk {
        let samples = &pcm[..frames * 2];
        AudioChunk::new(
            AudioChunkInfo {
                spec: spec(),
                frames: u32::try_from(frames).unwrap_or(0),
                frame_offset: at,
                media_origin: Some(Duration::ZERO),
                visible_origin: Some(Duration::ZERO),
                ..Default::default()
            },
            sample_buffer(pools, samples),
        )
    }

    fn beat_detector() -> Box<dyn BeatDetector> {
        let raw = RawBeats::new(
            Vec::<BeatMark>::new(),
            (0..9u8)
                .map(|n| BeatMark::new(f32::from(n) * 2.0, 0.9))
                .collect(),
        );
        let mock = Unimock::new(
            BeatDetectorMock
                .next_call(matching!(_))
                .answers_arc(Arc::new(move |_, _| Ok(raw.clone()))),
        );
        Box::new(mock)
    }

    fn waveform_pass(
        pools: Pools,
        buckets: usize,
    ) -> TrackAnalyzers<NoResamplerBackend, TestPools> {
        AnalyzerBuilder::<NoResamplerBackend, _>::new(pools)
            .with_waveform(buckets)
            .build(spec().sample_rate, "track-a".into(), 0, AnalysisDemand::ALL)
            .expect("waveform buffers fit the test region")
    }

    #[kithara::test(native, flash(false))]
    fn a_waveform_pass_publishes_a_waveform_and_no_beat(analysis_silence: Vec<f32>) {
        let pools = pools();
        let mut analyzers = waveform_pass(pools.clone(), 8);
        analyzers.push(
            &chunk(&pools, &analysis_silence, 8192, 0),
            &mut Extent::default(),
            None,
        );

        let snapshot = analyzers.snapshot(None, true, Some(8192));
        assert!(snapshot.waveform().is_some(), "the waveform slot is filled");
        assert!(snapshot.beat().is_none(), "no beat pass was configured");
    }

    #[kithara::test(native, flash(false))]
    fn a_beat_pass_publishes_both_artifacts_at_once(analysis_silence: Vec<f32>) {
        let pools = pools();
        let mut builder = AnalyzerBuilder::<RubatoBackend, _>::new(pools.clone())
            .with_waveform(8)
            .with_beat_detector(beat_detector(), GridParams::default());
        let mut detector = builder.take_detector();
        let mut analyzers = builder
            .build(spec().sample_rate, "track-a".into(), 0, AnalysisDemand::ALL)
            .expect("analysis buffers fit the test region");
        analyzers.push(
            &chunk(&pools, &analysis_silence, 8192, 0),
            &mut Extent::default(),
            detector.as_mut(),
        );

        let snapshot = analyzers.snapshot(detector.as_mut(), true, Some(8192));
        assert!(snapshot.waveform().is_some(), "the waveform is published");
        assert!(
            snapshot.beat().is_some(),
            "the grid rides the same snapshot"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_rejected_range_leaves_the_coverage_alone(analysis_silence: Vec<f32>) {
        let pools = pools();
        let mut extent = Extent::default();
        let mut analyzers = waveform_pass(pools.clone(), 8);
        analyzers.push(
            &chunk(&pools, &analysis_silence, 8192, 0),
            &mut extent,
            None,
        );
        let covered = analyzers.snapshot(None, false, None).coverage().clone();

        // A rate the pass was not opened with.
        let foreign = AudioChunk::new(
            AudioChunkInfo {
                spec: AudioSpec {
                    channels: 2,
                    sample_rate: NonZeroU32::new(48_000).expect("test rate is non-zero"),
                },
                frames: 1024,
                frame_offset: 0,
                media_origin: Some(Duration::ZERO),
                visible_origin: Some(Duration::ZERO),
                ..Default::default()
            },
            sample_buffer(&pools, &analysis_silence[..2048]),
        );
        assert_eq!(
            analyzers.push(&foreign, &mut Extent::default(), None),
            Ingest::ForeignRate
        );
        assert_eq!(
            analyzers.snapshot(None, false, None).coverage(),
            &covered,
            "a foreign rate must not move the coverage"
        );

        assert_eq!(
            analyzers.push(
                &chunk(&pools, &analysis_silence, 8192, 0),
                &mut extent,
                None
            ),
            Ingest::Covered
        );
        assert_eq!(analyzers.snapshot(None, false, None).coverage(), &covered);
    }

    #[kithara::test(native, flash(false))]
    fn a_pass_keeps_the_axis_it_was_opened_on(analysis_silence: Vec<f32>) {
        let pools = pools();
        // Opened at 48 kHz; the reader turns out to decode at 44.1 kHz.
        let axis = NonZeroU32::new(48_000).expect("test rate is non-zero");
        let mut analyzers = AnalyzerBuilder::<NoResamplerBackend, _>::new(pools.clone())
            .with_waveform(8)
            .build(axis, "track-a".into(), 0, AnalysisDemand::ALL)
            .expect("analysis buffers fit the test region");

        assert_eq!(
            analyzers.push(
                &chunk(&pools, &analysis_silence, 8192, 0),
                &mut Extent::default(),
                None
            ),
            Ingest::ForeignRate,
            "the first chunk does not get to redefine the axis"
        );

        let snapshot = analyzers.snapshot(None, false, None);
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
    fn a_range_no_one_covered_is_missing_until_it_arrives(analysis_silence: Vec<f32>) {
        let pools = pools();
        let mut analyzers = waveform_pass(pools.clone(), 8);
        // A producer was starved over [8192, 16384) and carried on past it.
        analyzers.push(
            &chunk(&pools, &analysis_silence, 8192, 0),
            &mut Extent::default(),
            None,
        );
        analyzers.push(
            &chunk(&pools, &analysis_silence, 8192, 16_384),
            &mut Extent::default(),
            None,
        );

        assert_eq!(
            analyzers.snapshot(None, false, None).missing(),
            vec![8192..8192 + 8192],
            "the hole is known to exist because something landed past it"
        );

        analyzers.push(
            &chunk(&pools, &analysis_silence, 8192, 8192),
            &mut Extent::default(),
            None,
        );
        assert!(
            analyzers.snapshot(None, false, None).missing().is_empty(),
            "a range taken on a second offer leaves the missing set"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_scattered_coverage_is_measured_against_where_it_reaches(analysis_silence: Vec<f32>) {
        let pools = pools();
        let mut analyzers = waveform_pass(pools.clone(), 8);
        // A range decoded away from the start, which is what a schedule
        // covers first.
        analyzers.push(
            &chunk(&pools, &analysis_silence, 8192, 65_536),
            &mut Extent::default(),
            None,
        );

        let snapshot = analyzers.snapshot(None, false, None);
        assert_eq!(
            snapshot.source_frames(),
            73_728,
            "the denominator must reach the covered range, not just count it"
        );
        assert!(
            snapshot
                .coverage()
                .iter()
                .all(|run| run.end <= snapshot.source_frames()),
            "no covered frame may sit past the denominator it is divided by"
        );
    }

    #[kithara::test(native, flash(false))]
    fn nothing_past_the_frontier_is_claimed_missing(analysis_silence: Vec<f32>) {
        let pools = pools();
        let mut analyzers = waveform_pass(pools.clone(), 8);
        analyzers.push(
            &chunk(&pools, &analysis_silence, 8192, 0),
            &mut Extent::default(),
            None,
        );

        assert!(
            analyzers.snapshot(None, false, None).missing().is_empty(),
            "a pass that has not been told how long the track is claims nothing beyond what it saw"
        );

        // End of stream proves the extent is the frontier, so still nothing.
        assert!(
            analyzers
                .snapshot(None, true, Some(8192))
                .missing()
                .is_empty()
        );
    }

    #[kithara::test(native, flash(false))]
    fn revisions_strictly_increase_across_publications(analysis_silence: Vec<f32>) {
        let pools = pools();
        let mut analyzers = waveform_pass(pools.clone(), 8);
        let mut revisions = Vec::new();
        for block in 0..3u64 {
            analyzers.push(
                &chunk(&pools, &analysis_silence, 8192, block * 8192),
                &mut Extent::default(),
                None,
            );
            revisions.push(analyzers.snapshot(None, false, None).revision());
        }
        revisions.push(analyzers.snapshot(None, true, Some(8192)).revision());

        assert!(
            revisions.windows(2).all(|pair| pair[1] > pair[0]),
            "each publication must outrank the last: {revisions:?}"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_pass_opened_above_a_held_revision_publishes_above_it(analysis_silence: Vec<f32>) {
        let pools = pools();
        let mut analyzers = AnalyzerBuilder::<NoResamplerBackend, _>::new(pools.clone())
            .with_waveform(8)
            .build(spec().sample_rate, "track-a".into(), 3, AnalysisDemand::ALL)
            .expect("waveform buffers fit the test region");
        analyzers.push(
            &chunk(&pools, &analysis_silence, 8192, 0),
            &mut Extent::default(),
            None,
        );

        assert_eq!(analyzers.snapshot(None, false, None).revision(), 4);
    }

    #[kithara::test(native, flash(false))]
    fn a_snapshot_carries_the_token_its_pass_was_opened_with(analysis_silence: Vec<f32>) {
        let pools = pools();
        let mut first = AnalyzerBuilder::<NoResamplerBackend, _>::new(pools.clone())
            .with_waveform(8)
            .build(spec().sample_rate, "track-a".into(), 0, AnalysisDemand::ALL)
            .expect("analysis buffers fit the test region");
        let mut second = AnalyzerBuilder::<NoResamplerBackend, _>::new(pools.clone())
            .with_waveform(8)
            .build(spec().sample_rate, "track-b".into(), 0, AnalysisDemand::ALL)
            .expect("analysis buffers fit the test region");
        first.push(
            &chunk(&pools, &analysis_silence, 8192, 0),
            &mut Extent::default(),
            None,
        );
        second.push(
            &chunk(&pools, &analysis_silence, 8192, 0),
            &mut Extent::default(),
            None,
        );

        assert_eq!(
            first.snapshot(None, true, Some(8192)).token().as_str(),
            "track-a"
        );
        assert_eq!(
            second.snapshot(None, true, Some(8192)).token().as_str(),
            "track-b"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_waveform_resolution_change_leaves_the_beat_fingerprint_alone() {
        let fingerprint = |buckets: usize| {
            AnalyzerBuilder::<RubatoBackend, _>::new(pools())
                .with_waveform(buckets)
                .with_beat()
                .build(spec().sample_rate, "track-a".into(), 0, AnalysisDemand::ALL)
                .expect("analysis buffers fit the test region")
                .snapshot(None, false, None)
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
    fn a_grid_is_provisional_until_the_pass_settles_over_a_covered_extent(
        analysis_silence: Vec<f32>,
    ) {
        let pools = pools();
        let mut builder = AnalyzerBuilder::<RubatoBackend, _>::new(pools.clone())
            .with_waveform(8)
            .with_beat_detector(beat_detector(), GridParams::default());
        let mut detector = builder.take_detector();
        let mut analyzers = builder
            .build(spec().sample_rate, "track-a".into(), 0, AnalysisDemand::ALL)
            .expect("analysis buffers fit the test region");
        analyzers.push(
            &chunk(&pools, &analysis_silence, 8192, 0),
            &mut Extent::default(),
            detector.as_mut(),
        );

        let early = analyzers.snapshot(detector.as_mut(), false, None);
        assert!(early.extent().is_none(), "the extent is not known yet");
        assert!(
            early
                .beat()
                .is_none_or(|beat| beat.state() == BeatState::Provisional),
            "a grid without a known extent cannot be final"
        );

        let read = analyzers.snapshot(detector.as_mut(), false, Some(8192));
        assert!(
            read.beat()
                .is_none_or(|beat| beat.state() == BeatState::Provisional),
            "a pass still to settle can change its grid"
        );

        analyzers.settle();
        let ended = analyzers.snapshot(detector.as_mut(), true, Some(8192));
        assert_eq!(
            ended.extent(),
            Some(8192),
            "end of stream proves the extent"
        );
        assert!(
            ended
                .beat()
                .is_none_or(|beat| beat.state() == BeatState::Final),
            "the pass settled over a covered extent, so the grid is final"
        );
        assert_eq!(ended.coverage().frames(), ended.extent().unwrap_or(0));
    }

    /// A detector that hears a beat every half second, whatever it is handed:
    /// the pass, not the hearing, is what this test is about.
    fn steady_detector() -> Box<dyn BeatDetector> {
        let raw = RawBeats::new(
            (0..8u8)
                .map(|n| BeatMark::new(f32::from(n) * 0.5, 0.9))
                .collect(),
            (0..2u8)
                .map(|n| BeatMark::new(f32::from(n) * 2.0, 0.9))
                .collect(),
        );
        Box::new(Unimock::new(
            BeatDetectorMock
                .each_call(matching!(_))
                .answers_arc(Arc::new(move |_, _| Ok(raw.clone())))
                .at_least_times(1),
        ))
    }

    /// Every publication of a real pass states the shared grid, and it states
    /// it in media seconds on the source axis rather than in the frames the
    /// artifact keeps. The same pass run on a server writes these documents;
    /// a client that cannot run a detector reads them back unchanged, which is
    /// what the round trip at the end stands for.
    #[kithara::test(native, flash(false))]
    fn every_publication_of_a_pass_states_the_grid_it_found(analysis_silence: Vec<f32>) {
        /// Chunks of source the pass reads before it is asked for a grid: enough
        /// to cover the markers the steady detector states.
        const COVERED_CHUNKS: u32 = 24;

        let pools = pools();
        let mut builder = AnalyzerBuilder::<RubatoBackend, _>::new(pools.clone())
            .with_waveform(8)
            .with_beat_detector(steady_detector(), GridParams::default());
        let mut detector = builder.take_detector();
        let mut analyzers = builder
            .build(spec().sample_rate, "track-a".into(), 0, AnalysisDemand::ALL)
            .expect("analysis buffers fit the test region");
        // Four seconds of source, so the markers the detector states all fall
        // inside what the pass has actually covered.
        let mut extent = Extent::default();
        for index in 0..COVERED_CHUNKS {
            let at = u64::from(index) * 8192;
            analyzers.push(
                &chunk(&pools, &analysis_silence, 8192, at),
                &mut extent,
                detector.as_mut(),
            );
        }
        let covered = u64::from(COVERED_CHUNKS) * 8192;

        let early = analyzers.snapshot(detector.as_mut(), true, None);
        let provisional = early.grid().expect("the pass states what it heard");
        assert_eq!(provisional.as_raw().state, BeatGridState::Provisional);
        assert!(
            provisional.as_raw().duration.is_none(),
            "an unknown length is absent rather than zero"
        );
        assert!(
            (provisional.as_raw().bpm - 120.0).abs() < 1e-6,
            "half-second beats are 120 bpm, got {}",
            provisional.as_raw().bpm
        );
        let seconds: Vec<f64> = provisional
            .as_raw()
            .beats
            .iter()
            .map(|beat| beat.at)
            .collect();
        assert!(
            seconds
                .iter()
                .enumerate()
                .all(|(index, at)| (at - 0.5 * index.to_f64().unwrap_or(f64::NAN)).abs() < 1e-9),
            "the markers stand on the source axis in media seconds: {seconds:?}"
        );
        assert_eq!(
            provisional
                .as_raw()
                .beats
                .iter()
                .map(|beat| beat.ordinal)
                .collect::<Vec<_>>(),
            (0..seconds.len().to_i64().unwrap_or(0)).collect::<Vec<_>>(),
            "the ordinals are the beats the music names, not array positions"
        );

        analyzers.settle();
        let ended = analyzers.snapshot(detector.as_mut(), true, Some(covered));
        let settled = ended.grid().expect("the settled pass states its grid");
        assert_eq!(settled.as_raw().state, BeatGridState::Final);
        assert!(
            settled.as_raw().revision > provisional.as_raw().revision,
            "a later publication carries a later revision"
        );

        let document = serde_json::to_string(settled).expect("a grid serializes");
        let read_back: BeatGridModel =
            serde_json::from_str(&document).expect("a client reads the stored grid");
        assert_eq!(
            &read_back, settled,
            "what a server stores is what a client without an analyzer gets back"
        );
    }
    #[kithara::test]
    fn direct_intake_and_restore_keep_origins_separate_from_raw_coverage(
        analysis_silence: Vec<f32>,
    ) {
        let pools = pools();
        let builder = AnalyzerBuilder::<NoResamplerBackend, _>::new(pools.clone()).with_waveform(8);
        let mut pass = builder
            .build(
                spec().sample_rate,
                "origin-track".into(),
                0,
                AnalysisDemand::ALL,
            )
            .expect("analysis buffers fit");
        let mut audio = chunk(&pools, &analysis_silence, 1024, 22_050);
        audio.meta.media_origin = Some(Duration::from_nanos(25_056_689));
        audio.meta.visible_origin = Some(Duration::from_nanos(11_995_465));
        assert_eq!(
            pass.push(&audio, &mut Extent::default(), None),
            Ingest::Accepted
        );
        assert_eq!(pass.visible_origin(), audio.meta.visible_origin);
        let chunk_frames = NonZeroU64::new(44_100).expect("resume chunk size is non-zero");
        let progress = pass.progress(None, false, chunk_frames, Some(100_000));
        assert_eq!(progress.analysis().media_origin(), audio.meta.media_origin);
        assert_eq!(
            progress.analysis().coverage().iter().collect::<Vec<_>>(),
            [&(22_050..23_074)]
        );
        assert_eq!(progress.analysis().extent(), Some(100_000));
        let mut restored = builder
            .restore(&progress, chunk_frames)
            .expect("the pass resumes");
        assert_eq!(
            restored.visible_origin(),
            None,
            "the persisted raw extent does not invent the visible basis"
        );
        let after = restored.snapshot(None, false, progress.analysis().extent());
        assert_eq!(after.media_origin(), audio.meta.media_origin);
        assert_eq!(after.coverage(), progress.analysis().coverage());
        assert_eq!(
            after.waveform().map(Waveform::buckets),
            progress.analysis().waveform().map(Waveform::buckets)
        );
    }

    #[kithara::test]
    fn unknown_origin_intake_still_publishes_raw_waveform_and_coverage(analysis_silence: Vec<f32>) {
        let pools = pools();
        let mut pass = waveform_pass(pools.clone(), 8);
        let mut audio = chunk(&pools, &analysis_silence, 1024, 529);
        audio.meta.media_origin = None;
        audio.meta.visible_origin = None;
        assert_eq!(
            pass.push(&audio, &mut Extent::default(), None),
            Ingest::Accepted
        );
        let snapshot = pass.snapshot(None, false, Some(1553));
        assert_eq!(snapshot.media_origin(), None);
        assert_eq!(
            snapshot.coverage().iter().collect::<Vec<_>>(),
            [&(529..1553)]
        );
        assert_eq!(snapshot.extent(), Some(1553));
        assert!(snapshot.waveform().is_some());
    }

    #[kithara::test]
    fn conflicting_original_clock_facts_are_refused_before_artifact_ingest(
        analysis_silence: Vec<f32>,
    ) {
        let pools = pools();
        let mut pass = waveform_pass(pools.clone(), 8);
        let mut extent = Extent::default();
        let first = chunk(&pools, &analysis_silence, 1024, 22_050);
        assert_eq!(pass.push(&first, &mut extent, None), Ingest::Accepted);
        let before = pass.snapshot(None, false, None);
        for (media_origin, visible_origin) in [
            (Some(Duration::from_millis(25)), Some(Duration::ZERO)),
            (Some(Duration::ZERO), Some(Duration::from_millis(12))),
        ] {
            let mut foreign = chunk(&pools, &analysis_silence, 1024, 44_100);
            foreign.meta.media_origin = media_origin;
            foreign.meta.visible_origin = visible_origin;
            assert_eq!(pass.push(&foreign, &mut extent, None), Ingest::ForeignClock);
            let after = pass.snapshot(None, false, None);
            assert_eq!(after.media_origin(), before.media_origin());
            assert_eq!(after.coverage(), before.coverage());
            assert_eq!(
                after.waveform().map(Waveform::buckets),
                before.waveform().map(Waveform::buckets)
            );
            extent.prove_end(0);
            assert_eq!(
                extent.frames(),
                Some(23_074),
                "refused audio did not extend the raw frontier"
            );
        }
    }

    #[kithara::test]
    fn producer_intake_keeps_original_facts_and_refuses_a_foreign_clock(
        analysis_silence: Vec<f32>,
    ) {
        let pools = pools();
        let mut pass = waveform_pass(pools.clone(), 8);
        let mut extent = Extent::default();
        let (mut writer, mut reader) =
            crate::producer::ring::open(&pools, 64, 4).expect("ring fits");
        let mut out = pools.get::<f32>();
        let metadata = AudioChunkInfo {
            media_origin: Some(Duration::from_nanos(25_056_689)),
            visible_origin: Some(Duration::from_nanos(11_995_465)),
            frame_offset: 22_050,
            spec: spec(),
            ..Default::default()
        };
        assert!(writer.push(&metadata, 4, analysis_silence[..4].iter().copied()));
        let span = reader.pop(&mut out).expect("the first span is accepted");
        assert_eq!(
            pass.push_mono(&out, span, &mut extent, None),
            Ingest::Accepted
        );
        let before = pass.snapshot(None, false, None);
        assert_eq!(before.media_origin(), metadata.media_origin);
        assert_eq!(
            before.coverage().iter().collect::<Vec<_>>(),
            [&(22_050..22_054)]
        );
        for media_origin in [None, Some(Duration::from_millis(50))] {
            let info = AudioChunkInfo {
                media_origin,
                frame_offset: 44_100,
                ..metadata
            };
            assert!(writer.push(&info, 4, analysis_silence[..4].iter().copied()));
            let span = reader
                .pop(&mut out)
                .expect("the ring carries the source facts");
            let expected = if media_origin.is_none() {
                Ingest::Accepted
            } else {
                Ingest::ForeignClock
            };
            assert_eq!(pass.push_mono(&out, span, &mut extent, None), expected);
            assert_eq!(
                pass.snapshot(None, false, None).media_origin(),
                metadata.media_origin
            );
        }
        assert_eq!(
            pass.snapshot(None, false, None)
                .coverage()
                .iter()
                .collect::<Vec<_>>(),
            [&(22_050..22_054), &(44_100..44_104)]
        );
    }

    #[kithara::test]
    fn a_foreign_visible_clock_cannot_supply_an_unknown_media_origin(analysis_silence: Vec<f32>) {
        let pools = pools();
        let mut pass = waveform_pass(pools.clone(), 8);
        let mut first = chunk(&pools, &analysis_silence, 1024, 529);
        first.meta.media_origin = None;
        assert_eq!(
            pass.push(&first, &mut Extent::default(), None),
            Ingest::Accepted
        );
        let before = pass.snapshot(None, false, None);
        let mut foreign = chunk(&pools, &analysis_silence, 1024, 22_050);
        foreign.meta.media_origin = Some(Duration::from_nanos(25_056_689));
        foreign.meta.visible_origin = Some(Duration::from_millis(12));
        assert_eq!(
            pass.push(&foreign, &mut Extent::default(), None),
            Ingest::ForeignClock
        );
        assert_eq!(pass.snapshot(None, false, None).media_origin(), None);
        assert_eq!(
            pass.snapshot(None, false, None).coverage(),
            before.coverage()
        );
        foreign.meta.visible_origin = first.meta.visible_origin;
        assert_eq!(
            pass.push(&foreign, &mut Extent::default(), None),
            Ingest::Accepted
        );
        assert_eq!(
            pass.snapshot(None, false, None).media_origin(),
            foreign.meta.media_origin
        );
    }
}
