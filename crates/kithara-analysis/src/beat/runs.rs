use std::num::NonZeroU32;

use kithara_bufpool::{HasPool, PoolError, PoolRegion, SampleBuffer};
use kithara_resampler::{MonoStream, MonoStreamConfig, ResamplerBackend, ResamplerOptions};
use num_traits::cast::ToPrimitive;
use tracing::debug;

use super::detector::BeatDetectError;
use crate::{
    BlobError,
    analyzer::BeatAnalysisConfig,
    blob::Writer,
    progress::BeatRunResume,
};

/// What the detector was fed under. A grid built from audio assembled another
/// way is not this build's answer, so it names itself in the analysis
/// fingerprint.
#[cfg(any(feature = "beat-nn", feature = "beat-dsp"))]
pub(crate) const DETECTOR_AUDIO_TAG: &str = "detector_audio_seamless_v1";

struct Run<B>
where
    B: ResamplerBackend,
{
    start: u64,
    end: u64,
    mono: SampleBuffer,
    stream: Option<MonoStream<B>>,
}

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(super) struct Runs<B>
where
    B: ResamplerBackend,
{
    runs: Vec<Run<B>>,
    config: BeatAnalysisConfig<B>,
    budget: usize,
    #[field(get, vis = "pub(super)")]
    dropped: Vec<(u64, u64)>,
    ratio: f64,
    source_rate: u32,
    #[field(get, copy, vis = "pub(super)")]
    target_rate: u32,
}

impl<B> Runs<B>
where
    B: ResamplerBackend,
{
    pub(super) fn new(config: BeatAnalysisConfig<B>, source_rate: u32, budget: usize) -> Self {
        let target_rate = config.target_rate().max(1);
        let source = f64::from(source_rate.max(1));
        Self {
            runs: Vec::new(),
            ratio: f64::from(target_rate) / source,
            budget,
            dropped: Vec::new(),
            config,
            source_rate: source_rate.max(1),
            target_rate,
        }
    }

    fn held(&self) -> usize {
        self.runs.iter().map(|run| run.mono.len()).sum()
    }

    #[cfg(test)]
    pub(super) fn held_frames(&self) -> usize {
        self.held()
    }

    fn enforce_budget(&mut self) {
        let mut held = self.held();
        while held > self.budget {
            let over = held - self.budget;
            let Some(run) = self.runs.first_mut() else {
                return;
            };
            let drop_detector = over.min(run.mono.len());
            let source = self.ratio.recip() * drop_detector.to_f64().unwrap_or(0.0);
            let source = source
                .round()
                .to_u64()
                .unwrap_or(0)
                .min(run.end - run.start);
            let at = run.start;
            let exact = (source.to_f64().unwrap_or(0.0) * self.ratio)
                .round()
                .to_usize()
                .unwrap_or(0)
                .min(run.mono.len());
            if exact == 0 {
                return;
            }

            run.mono.drain(..exact);
            run.start = at.saturating_add(source);
            held -= exact;
            self.dropped.push((at, run.start));
            debug!(
                from = at,
                to = run.start,
                "beat analysis: detector mono reclaimed; range left unanalysed"
            );
            if run.start >= run.end {
                self.runs.remove(0);
            }
        }
    }

    fn detector_frames(&self, frames: u64) -> usize {
        scale(frames, self.ratio)
    }

    /// Drops what a run has already fed the detector. `opens_at` answers, for a
    /// run starting at that detector frame, the frame its next window opens at.
    /// A run emptied this way keeps its place: it still holds the resampler the
    /// audio behind it continues.
    pub(super) fn release(&mut self, opens_at: impl Fn(usize) -> usize) {
        let ratio = self.ratio;
        for run in &mut self.runs {
            let base = scale(run.start, ratio);
            let target = (opens_at(base).to_f64().unwrap_or(0.0) / ratio)
                .floor()
                .to_u64()
                .unwrap_or(0)
                .clamp(run.start, run.end);
            let exact = scale(target, ratio)
                .saturating_sub(base)
                .min(run.mono.len());
            if exact == 0 {
                continue;
            }
            run.mono.drain(..exact);
            run.start = target;
        }
    }

    pub(super) fn flush(&mut self) -> Result<(), BeatDetectError> {
        for index in 0..self.runs.len() {
            let Some((span, stream)) = self
                .runs
                .get_mut(index)
                .map(|run| (run.end.saturating_sub(run.start), run.stream.take()))
            else {
                continue;
            };
            let expected = self.detector_frames(span);
            let Some(run) = self.runs.get_mut(index) else {
                continue;
            };
            let mono = &mut run.mono;
            if let Some(stream) = stream {
                finish_stream(stream, mono)?;
            }
            pad(mono, expected)?;
        }
        Ok(())
    }

    pub(super) fn write_resume(&self, writer: &mut Writer<'_>) {
        writer.write_len(self.runs.len());
        for run in &self.runs {
            writer.write_u64(run.start);
            writer.write_u64(run.end);
            // A live resampler still holds this run's tail. The blob carries the
            // span the run declares, so the part still inside reads as silence
            // rather than costing the live run its resampler.
            write_padded(
                writer,
                &run.mono,
                self.detector_frames(run.end.saturating_sub(run.start)),
            );
        }
        writer.write_len(self.dropped.len());
        for (from, to) in &self.dropped {
            writer.write_u64(*from);
            writer.write_u64(*to);
        }
    }

    pub(super) fn restore<S>(
        &mut self,
        pools: &PoolRegion<S>,
        runs: Vec<BeatRunResume>,
        dropped: Vec<(u64, u64)>,
    ) -> Result<(), BlobError>
    where
        S: HasPool<f32>,
    {
        let mut restored: Vec<Run<B>> = Vec::with_capacity(runs.len());
        for run in runs {
            let expected = self.detector_frames(run.end.saturating_sub(run.start));
            if run.mono.len() != expected {
                return Err(BlobError::Corrupt);
            }
            let samples = run.mono.into_vec();
            let mut mono = pools
                .get_with_len::<f32>(samples.len())
                .map_err(|_| BlobError::Corrupt)?;
            mono.copy_from_slice(&samples);
            restored.push(Run {
                start: run.start,
                end: run.end,
                mono,
                stream: None,
            });
        }
        if restored.iter().any(|run| {
            dropped
                .iter()
                .any(|(from, to)| *from < run.end && run.start < *to)
        }) {
            return Err(BlobError::Corrupt);
        }

        self.runs = restored;
        self.dropped = dropped;
        if self.held() > self.budget {
            return Err(BlobError::Corrupt);
        }
        Ok(())
    }

    pub(super) fn push<S>(
        &mut self,
        pools: &PoolRegion<S>,
        mono: &[f32],
        at: u64,
    ) -> Result<(), BeatDetectError>
    where
        S: HasPool<f32>,
    {
        let Ok(span) = u64::try_from(mono.len()) else {
            return Ok(());
        };
        if span == 0 {
            return Ok(());
        }
        let end = at.saturating_add(span);

        let first = self.runs.partition_point(|run| run.end < at);
        let last = self.runs.partition_point(|run| run.start <= end);
        if first == last {
            let run = self.open(pools, mono, at, end)?;
            self.runs.insert(first, run);
            self.enforce_budget();
            return Ok(());
        }

        let absorbed: Vec<Run<B>> = self.runs.splice(first..last, []).collect();
        if let Some(merged) = self.merge(pools, absorbed, mono, at, end)? {
            self.runs.insert(first, merged);
        }
        self.enforce_budget();
        Ok(())
    }

    pub(super) fn spans(&self) -> impl Iterator<Item = (u64, &[f32])> {
        self.runs.iter().map(|run| (run.start, &run.mono[..]))
    }

    pub(super) fn offset_in_run(&self, start: u64, frame: u64) -> usize {
        self.detector_frames(frame.saturating_sub(start))
    }

    fn merge<S>(
        &mut self,
        pools: &PoolRegion<S>,
        absorbed: Vec<Run<B>>,
        mono: &[f32],
        at: u64,
        end: u64,
    ) -> Result<Option<Run<B>>, BeatDetectError>
    where
        S: HasPool<f32>,
    {
        let base = absorbed.first().map_or(at, |run| run.start.min(at));
        let mut out = pools.get::<f32>();
        let mut cursor = base;
        let mut stream = None;

        for mut run in absorbed {
            if cursor < run.start {
                let Some(piece) = slice(mono, at, cursor, run.start) else {
                    return Ok(None);
                };
                self.resample(pools, &mut out, piece, &mut stream)?;
                cursor = run.start;
            }
            // Audio the run already holds came out of its own resampler, so
            // ours ends here and its tail lands before that audio.
            finish_into(&mut out, stream.take())?;
            pad(&mut out, self.detector_frames(cursor.saturating_sub(base)))?;
            if run.end <= cursor {
                continue;
            }
            let skip = self.detector_frames(cursor.saturating_sub(run.start));
            append(&mut out, run.mono.get(skip..).unwrap_or_default())?;
            cursor = run.end;
            // The run the arriving audio continues keeps its resampler. Closing
            // one and opening another leaves a step at the seam, and an onset
            // detector reads a step as a beat.
            stream = run.stream.take();
            if stream.is_none() {
                pad(&mut out, self.detector_frames(cursor.saturating_sub(base)))?;
            }
        }

        if cursor < end {
            let Some(piece) = slice(mono, at, cursor, end) else {
                return Ok(None);
            };
            self.resample(pools, &mut out, piece, &mut stream)?;
            cursor = end;
        }

        Ok(Some(Run {
            start: base,
            end: cursor,
            mono: out,
            stream,
        }))
    }

    fn open<S>(
        &self,
        pools: &PoolRegion<S>,
        mono: &[f32],
        at: u64,
        end: u64,
    ) -> Result<Run<B>, BeatDetectError>
    where
        S: HasPool<f32>,
    {
        let mut out = pools.get::<f32>();
        let stream = if self.source_rate == self.target_rate {
            append(&mut out, mono)?;
            None
        } else {
            let mut stream = self.stream(pools)?;
            push_stream(&mut stream, mono, &mut out)?;
            Some(stream)
        };
        Ok(Run {
            start: at,
            end,
            mono: out,
            stream,
        })
    }

    /// Resamples one contiguous piece through `stream`, opening one when there
    /// is none. What the stream still holds stays in it.
    fn resample<S>(
        &self,
        pools: &PoolRegion<S>,
        out: &mut SampleBuffer,
        mono: &[f32],
        stream: &mut Option<MonoStream<B>>,
    ) -> Result<(), BeatDetectError>
    where
        S: HasPool<f32>,
    {
        if self.source_rate == self.target_rate {
            append(out, mono)?;
            return Ok(());
        }
        let mut inner = match stream.take() {
            Some(inner) => inner,
            None => self.stream(pools)?,
        };
        push_stream(&mut inner, mono, out)?;
        *stream = Some(inner);
        Ok(())
    }

    fn stream<S>(&self, pools: &PoolRegion<S>) -> Result<MonoStream<B>, BeatDetectError>
    where
        S: HasPool<f32>,
    {
        let source_sample_rate = NonZeroU32::new(self.source_rate).unwrap_or(NonZeroU32::MIN);
        let target_sample_rate = NonZeroU32::new(self.target_rate).unwrap_or(NonZeroU32::MIN);
        let config = MonoStreamConfig::builder()
            .backend(self.config.resampler_backend().clone())
            .source_sample_rate(source_sample_rate)
            .target_sample_rate(target_sample_rate)
            .quality(self.config.resampler_quality())
            .options(
                ResamplerOptions::builder()
                    .chunk_size(self.config.block_frames())
                    .build(),
            )
            .pools(pools.clone())
            .build();
        MonoStream::new(config).map_err(resample_error)
    }
}

/// Source frames on the detector axis. Rounding, so a join keeps its position
/// instead of drifting by a sample per segment.
fn scale(frames: u64, ratio: f64) -> usize {
    (frames.to_f64().unwrap_or(0.0) * ratio)
        .round()
        .to_usize()
        .unwrap_or(usize::MAX)
}

fn write_padded(writer: &mut Writer<'_>, samples: &[f32], expected: usize) {
    writer.write_len(expected);
    for sample in samples.iter().take(expected) {
        writer.write_f32(*sample);
    }
    for _ in samples.len()..expected {
        writer.write_f32(0.0);
    }
}

fn finish_into<B>(
    out: &mut SampleBuffer,
    stream: Option<MonoStream<B>>,
) -> Result<(), BeatDetectError>
where
    B: ResamplerBackend,
{
    match stream {
        Some(stream) => finish_stream(stream, out),
        None => Ok(()),
    }
}

fn pad(out: &mut SampleBuffer, expected: usize) -> Result<(), PoolError> {
    if out.len() > expected {
        out.truncate(expected);
    } else {
        out.ensure_len(expected)?;
    }
    Ok(())
}

fn append(out: &mut SampleBuffer, src: &[f32]) -> Result<(), PoolError> {
    out.try_extend_from_slice(src)
}

fn push_stream<B>(
    stream: &mut MonoStream<B>,
    mono: &[f32],
    out: &mut SampleBuffer,
) -> Result<(), BeatDetectError>
where
    B: ResamplerBackend,
{
    let mut buffer_error = None;
    let result = stream.push(mono.iter().copied(), |samples| {
        if buffer_error.is_none() {
            buffer_error = append(out, samples).err();
        }
    });
    if let Some(error) = buffer_error {
        return Err(error.into());
    }
    result.map_err(resample_error)
}

fn finish_stream<B>(stream: MonoStream<B>, out: &mut SampleBuffer) -> Result<(), BeatDetectError>
where
    B: ResamplerBackend,
{
    let mut buffer_error = None;
    let result = stream.finish(|samples| {
        if buffer_error.is_none() {
            buffer_error = append(out, samples).err();
        }
    });
    if let Some(error) = buffer_error {
        return Err(error.into());
    }
    result.map_err(resample_error)
}

fn resample_error(error: impl std::fmt::Display) -> BeatDetectError {
    BeatDetectError::Resample {
        reason: error.to_string(),
    }
}

fn slice(mono: &[f32], at: u64, from: u64, to: u64) -> Option<&[f32]> {
    let start = usize::try_from(from.saturating_sub(at)).ok()?;
    let end = usize::try_from(to.saturating_sub(at)).ok()?;
    mono.get(start..end)
}

#[cfg(test)]
mod tests {
    use kithara_resampler::rubato::RubatoBackend;
    use kithara_test_utils::kithara;

    use super::Runs;
    use crate::{
        BeatAnalysisConfig,
        test_pools::{TestPools, pools},
    };

    const SRC: u32 = 44_100;

    struct TestRuns {
        inner: Runs<RubatoBackend>,
        pools: kithara_bufpool::PoolRegion<TestPools>,
    }

    impl TestRuns {
        fn push(&mut self, mono: &[f32], at: u64) {
            self.inner
                .push(&self.pools, mono, at)
                .expect("run buffers fit the test region");
        }

        fn flush(&mut self) {
            self.inner.flush().expect("run buffers fit the test region");
        }
    }

    impl std::ops::Deref for TestRuns {
        type Target = Runs<RubatoBackend>;

        fn deref(&self) -> &Self::Target {
            &self.inner
        }
    }

    fn runs(source_rate: u32) -> TestRuns {
        budgeted(source_rate, usize::MAX)
    }

    fn budgeted(source_rate: u32, budget: usize) -> TestRuns {
        TestRuns {
            inner: Runs::new(
                BeatAnalysisConfig::<RubatoBackend>::default(),
                source_rate,
                budget,
            ),
            pools: pools(),
        }
    }

    fn ramp(frames: usize, from: u64) -> Vec<f32> {
        (0..frames)
            .map(|n| {
                let t = (from + n as u64) as f32 / 1000.0;
                t.sin()
            })
            .collect()
    }

    fn layout(runs: &Runs<RubatoBackend>) -> Vec<(u64, usize)> {
        runs.spans()
            .map(|(start, mono)| (start, mono.len()))
            .collect()
    }

    fn mono_of(set: &TestRuns) -> Vec<f32> {
        set.spans()
            .flat_map(|(_, mono)| mono.iter().copied())
            .collect()
    }

    /// One resampler per run, carried across arrivals: closing one and opening
    /// another leaves a step at the seam, and an onset detector reads a step
    /// as a beat.
    #[kithara::test]
    fn arrival_size_does_not_change_the_resampled_audio() {
        let source = ramp(88_200, 0);
        let mut whole = runs(SRC);
        whole.push(&source, 0);
        whole.flush();

        let mut piecemeal = runs(SRC);
        for (index, block) in source.chunks(2205).enumerate() {
            piecemeal.push(block, u64::try_from(index * 2205).unwrap_or(0));
        }
        piecemeal.flush();

        let (whole, piecemeal) = (mono_of(&whole), mono_of(&piecemeal));
        assert_eq!(whole.len(), piecemeal.len(), "same source, same length");
        let worst = whole
            .iter()
            .zip(&piecemeal)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(
            worst < 1e-4,
            "arriving in blocks changed the audio by {worst}: a seam the detector reads as an onset"
        );
    }

    #[kithara::test]
    fn adjacent_blocks_form_one_run() {
        let mut set = runs(SRC);
        set.push(&ramp(4410, 0), 0);
        set.push(&ramp(4410, 4410), 4410);
        set.flush();
        assert_eq!(layout(&set), vec![(0, 4410)], "8820 source frames at 2:1");
    }

    #[kithara::test]
    fn a_gap_keeps_two_runs_until_it_is_filled() {
        let mut set = runs(SRC);
        set.push(&ramp(4410, 0), 0);
        set.push(&ramp(4410, 88_200), 88_200);
        set.flush();
        assert_eq!(layout(&set), vec![(0, 2205), (88_200, 2205)]);

        set.push(&ramp(83_790, 4410), 4410);
        set.flush();
        assert_eq!(
            layout(&set),
            vec![(0, 46_305)],
            "filling the gap joins the runs and pins the total length"
        );
    }

    #[kithara::test]
    fn a_block_before_a_run_extends_it_backwards() {
        let mut set = runs(SRC);
        set.push(&ramp(4410, 4410), 4410);
        set.push(&ramp(4410, 0), 0);
        set.flush();
        assert_eq!(layout(&set), vec![(0, 4410)]);
    }

    #[kithara::test]
    fn shuffled_blocks_land_at_the_same_detector_offsets() {
        let blocks: Vec<(u64, Vec<f32>)> = (0..8u64)
            .map(|i| (i * 4410, ramp(4410, i * 4410)))
            .collect();

        let mut ascending = runs(SRC);
        for (at, pcm) in &blocks {
            ascending.push(pcm, *at);
        }
        ascending.flush();

        let mut shuffled = runs(SRC);
        for index in [5usize, 0, 7, 2, 1, 6, 3, 4] {
            let Some((at, pcm)) = blocks.get(index) else {
                continue;
            };
            shuffled.push(pcm, *at);
        }
        shuffled.flush();

        assert_eq!(layout(&ascending), layout(&shuffled));
        let (_, want) = ascending.spans().next().expect("one run");
        let (_, got) = shuffled.spans().next().expect("one run");
        let drift = want
            .iter()
            .zip(got.iter())
            .filter(|(a, b)| (*a - *b).abs() > 1e-3)
            .count();
        assert!(
            drift * 200 < want.len(),
            "shuffled assembly must track the ascending one, {drift} of {} samples differ",
            want.len()
        );
    }

    #[kithara::test]
    fn the_budget_reclaims_the_earliest_mono_and_reports_it() {
        let mut set = budgeted(SRC, 20_000);
        for block in 0..10u64 {
            set.push(&ramp(4410, block * 4410), block * 4410);
        }
        assert!(
            set.held_frames() <= 20_000,
            "held detector frames must stay under the budget, got {}",
            set.held_frames()
        );

        let dropped = set.dropped();
        assert!(!dropped.is_empty(), "the reclaimed ranges must be reported");
        let (from, _) = dropped.first().copied().unwrap_or((1, 1));
        assert_eq!(from, 0, "the earliest source frames go first");
        let reclaimed: u64 = dropped.iter().map(|(from, to)| to - from).sum();
        assert!(
            reclaimed > 0 && reclaimed < 44_100,
            "the budget reclaims the overflow, not the track: {reclaimed}"
        );
    }

    #[kithara::test]
    fn released_audio_leaves_the_rest_where_it_was() {
        // 48 kHz -> 22.05 kHz is not a whole ratio, so a release rounded up
        // would move the run past the window it must resume at, and one
        // rounded anywhere would shift the audio behind it.
        let source = ramp(48_000 * 4, 0);
        let mut whole = runs(48_000);
        whole.push(&source, 0);
        whole.flush();
        let held = mono_of(&whole);

        for opens_at in [1, 4321, 22_050, 40_000] {
            let mut set = runs(48_000);
            set.push(&source, 0);
            set.flush();
            set.release(|_| opens_at);

            let (start, mono) = set.spans().next().expect("one run");
            let base = set.offset_in_run(0, start);
            assert!(
                base <= opens_at,
                "a run resuming at {base} skips the window opening at {opens_at}"
            );
            assert_eq!(
                mono,
                &held[base..],
                "released at {opens_at}, the audio behind it moved"
            );
        }
    }

    #[kithara::test]
    fn a_non_integer_ratio_keeps_joins_on_position() {
        // 48 kHz -> 22.05 kHz is not a whole ratio, so a per-segment rounding
        // error would show up as a length drift at every join.
        let mut set = runs(48_000);
        for block in 0..10u64 {
            set.push(&ramp(4801, block * 4801), block * 4801);
        }
        set.flush();
        let total = set.offset_in_run(0, 48_010);
        assert_eq!(layout(&set), vec![(0, total)]);
    }
}
