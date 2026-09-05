use std::num::NonZeroU32;

use kithara_bufpool::{HasPool, PoolError, PoolRegion, SampleBuffer};
use kithara_resampler::{MonoStream, MonoStreamConfig, ResamplerBackend, ResamplerOptions};
use num_traits::cast::ToPrimitive;

use super::detector::BeatDetectError;
use crate::{
    BlobError,
    analyzer::BeatAnalysisConfig,
    blob::Writer,
    coverage::{Coverage, FrameRange},
    progress::BeatRunResume,
};

/// What the detector was fed under. A grid built from audio assembled another
/// way is not this build's answer, so it names itself in the analysis
/// fingerprint.
#[cfg(any(feature = "beat-nn", feature = "beat-dsp"))]
pub(crate) const DETECTOR_AUDIO_TAG: &str = "detector_audio_seamless_v2";

/// What audio the pass can take right now.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Intake {
    /// Nothing, until the detector reads what is held.
    Full,
    /// Only audio continuing a run the pass already has.
    Continuing,
    /// Audio anywhere in the track.
    Anywhere,
}

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
    max_runs: usize,
    #[field(get, vis = "pub(super)")]
    taken: Coverage,
    ratio: f64,
    source_rate: u32,
    #[field(get, copy, vis = "pub(super)")]
    target_rate: u32,
}

impl<B> Runs<B>
where
    B: ResamplerBackend,
{
    pub(super) fn new(
        config: BeatAnalysisConfig<B>,
        source_rate: u32,
        budget: usize,
        max_runs: usize,
    ) -> Self {
        let target_rate = config.target_rate().max(1);
        let source = f64::from(source_rate.max(1));
        Self {
            runs: Vec::new(),
            ratio: f64::from(target_rate) / source,
            budget,
            max_runs: max_runs.max(1),
            taken: Coverage::default(),
            config,
            source_rate: source_rate.max(1),
            target_rate,
        }
    }

    pub(super) fn held(&self) -> usize {
        self.runs.iter().map(|run| run.mono.len()).sum()
    }

    /// What is held is what the detector has yet to read, so a full hold waits
    /// on it. Capping runs at the windows the budget carries means a full hold
    /// always includes a run long enough for the detector to read.
    pub(super) fn intake(&self) -> Intake {
        if self.held() >= self.budget {
            Intake::Full
        } else if self.runs.len() >= self.max_runs {
            Intake::Continuing
        } else {
            Intake::Anywhere
        }
    }

    fn admits(&self, range: FrameRange, opens: bool) -> bool {
        match self.intake() {
            Intake::Full => false,
            // A run opened in front of one the pass holds reads through to it,
            // so it costs a run only until the two meet. The reader crosses
            // one such stretch at a time, and the hold budget covers that run.
            Intake::Continuing => {
                self.meets(range)
                    || (opens && self.runs.len() <= self.max_runs && self.reaches(range))
            }
            Intake::Anywhere => opens || self.meets(range),
        }
    }

    fn meets(&self, range: FrameRange) -> bool {
        self.runs
            .iter()
            .any(|run| run.start <= range.end() && range.start() <= run.end)
    }

    /// Whether reading on from `range` arrives at audio the pass has taken.
    /// What a run holds begins where the detector has read to, and that moves;
    /// what the pass took does not, so this asks there. `range` is one the pass
    /// has yet to take, so the first region past it has nothing taken in front.
    fn reaches(&self, range: FrameRange) -> bool {
        self.taken
            .runs()
            .iter()
            .any(|region| region.start() >= range.end())
    }

    /// The next stretch of `[from, until)` the pass has not taken.
    fn missing(&self, from: u64, until: u64) -> Option<FrameRange> {
        let mut at = from;
        for run in self.taken.runs() {
            if run.end() <= at {
                continue;
            }
            if at < run.start() {
                let end = run.start().min(until);
                return (at < end).then(|| FrameRange::new(at, end - at));
            }
            at = run.end();
        }
        (at < until).then(|| FrameRange::new(at, until - at))
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
            let target = (opens_at(base).to_f64().unwrap_or(f64::MAX) / ratio)
                .floor()
                .to_u64()
                .unwrap_or(u64::MAX)
                .clamp(run.start, run.end);
            let exact = scale(target, ratio)
                .saturating_sub(base)
                .min(run.mono.len());
            if exact == 0 {
                continue;
            }
            run.mono.drain(..exact);
            run.start = target;
            // The charge follows what the run still holds, so the hold budget
            // bounds the bytes as well as the frames.
            run.mono.shrink_to_fit();
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
            finish_into(mono, stream)?;
            pad(mono, expected)?;
        }
        Ok(())
    }

    /// Takes what of `mono` the pass has not taken and has room for. Audio it
    /// turns down stays outside its coverage, so the pass is asked for it again.
    /// `opens` says whether this audio may start a run of its own.
    pub(super) fn push<S>(
        &mut self,
        pools: &PoolRegion<S>,
        mono: &[f32],
        at: u64,
        opens: bool,
    ) -> Result<bool, BeatDetectError>
    where
        S: HasPool<f32>,
    {
        let Ok(span) = u64::try_from(mono.len()) else {
            return Ok(false);
        };
        let end = at.saturating_add(span);
        let mut cursor = at;
        let mut took = false;

        while let Some(piece) = self.missing(cursor, end) {
            if !self.admits(piece, opens) {
                break;
            }
            let Some(block) = slice(mono, at, piece.start(), piece.end()) else {
                break;
            };
            self.absorb(pools, block, piece.start(), piece.end())?;
            self.taken.insert(piece);
            cursor = piece.end();
            took = true;
        }
        Ok(took)
    }

    fn absorb<S>(
        &mut self,
        pools: &PoolRegion<S>,
        mono: &[f32],
        at: u64,
        end: u64,
    ) -> Result<(), BeatDetectError>
    where
        S: HasPool<f32>,
    {
        let first = self.runs.partition_point(|run| run.end < at);
        let last = self.runs.partition_point(|run| run.start <= end);
        if first == last {
            let run = self.open(pools, mono, at, end)?;
            self.runs.insert(first, run);
            return Ok(());
        }

        let absorbed: Vec<Run<B>> = self.runs.splice(first..last, []).collect();
        if let Some(merged) = self.merge(pools, absorbed, mono, at, end)? {
            self.runs.insert(first, merged);
        }
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
            // The run the arriving audio continues keeps its resampler, so the
            // seam carries no step for an onset detector to read as a beat.
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
        &mut self,
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

/// What a run set writes into a resume blob, and what it reads back.
impl<B> Runs<B>
where
    B: ResamplerBackend,
{
    pub(super) fn write_resume(&self, writer: &mut Writer<'_>) {
        // A run the detector has read to its end holds nothing but the
        // resampler continuing it, which the blob does not carry.
        let live = || self.runs.iter().filter(|run| run.start < run.end);
        writer.write_len(live().count());
        for run in live() {
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
        writer.write_len(self.taken.runs().len());
        for run in self.taken.runs() {
            writer.write_u64(run.start());
            writer.write_u64(run.end());
        }
    }

    pub(super) fn restore<S>(
        &mut self,
        pools: &PoolRegion<S>,
        runs: Vec<BeatRunResume>,
        taken: Vec<(u64, u64)>,
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
            mono.shrink_to_fit();
            restored.push(Run {
                start: run.start,
                end: run.end,
                mono,
                stream: None,
            });
        }
        let mut coverage = Coverage::default();
        for (from, to) in taken {
            coverage.insert(FrameRange::new(from, to.saturating_sub(from)));
        }
        if restored
            .iter()
            .any(|run| !coverage.contains(FrameRange::new(run.start, run.end - run.start)))
        {
            return Err(BlobError::Corrupt);
        }

        self.runs = restored;
        self.taken = coverage;
        if self.held() > self.budget || self.runs.len() > self.max_runs.saturating_add(1) {
            return Err(BlobError::Corrupt);
        }
        Ok(())
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
    stream.map_or(Ok(()), |stream| finish_stream(stream, out))
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
    use kithara_bufpool::PoolConfig;
    use kithara_resampler::rubato::RubatoBackend;
    use kithara_test_utils::kithara;

    use super::{Intake, Runs};
    use crate::{
        BeatAnalysisConfig,
        coverage::FrameRange,
        test_pools::{Pools, pools, pools_with},
    };

    const SRC: u32 = 44_100;

    struct TestRuns {
        inner: Runs<RubatoBackend>,
        pools: Pools,
    }

    impl TestRuns {
        fn push(&mut self, mono: &[f32], at: u64, opens: bool) -> bool {
            self.inner
                .push(&self.pools, mono, at, opens)
                .expect("run buffers fit the test region")
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

    impl std::ops::DerefMut for TestRuns {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.inner
        }
    }

    fn runs(source_rate: u32) -> TestRuns {
        budgeted(source_rate, usize::MAX, usize::MAX)
    }

    fn budgeted(source_rate: u32, budget: usize, max_runs: usize) -> TestRuns {
        budgeted_with_pools(source_rate, budget, max_runs, pools())
    }

    fn budgeted_with_pools(
        source_rate: u32,
        budget: usize,
        max_runs: usize,
        pools: Pools,
    ) -> TestRuns {
        TestRuns {
            inner: Runs::new(
                BeatAnalysisConfig::<RubatoBackend>::default(),
                source_rate,
                budget,
                max_runs,
            ),
            pools,
        }
    }

    fn non_retaining_pools(max_bytes: usize) -> Pools {
        pools_with(
            max_bytes,
            PoolConfig::builder().max_buffers(32).build(),
            PoolConfig::builder()
                .max_buffers(8)
                .max_retained_capacity(1)
                .build(),
        )
    }

    fn ramp(frames: usize, from: u64) -> Vec<f32> {
        (0..frames)
            .map(|n| {
                let t = (from + n as u64) as f32 / 1000.0;
                t.sin()
            })
            .collect()
    }

    fn layout(runs: &TestRuns) -> Vec<(u64, usize)> {
        runs.spans()
            .map(|(start, mono)| (start, mono.len()))
            .collect()
    }

    fn mono_of(set: &TestRuns) -> Vec<f32> {
        set.spans()
            .flat_map(|(_, mono)| mono.iter().copied())
            .collect()
    }

    #[kithara::test]
    fn arrival_size_does_not_change_the_resampled_audio() {
        let source = ramp(88_200, 0);
        let mut whole = runs(SRC);
        whole.push(&source, 0, true);
        whole.flush();

        let mut piecemeal = runs(SRC);
        for (index, block) in source.chunks(2205).enumerate() {
            piecemeal.push(block, (index * 2205) as u64, true);
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
        set.push(&ramp(4410, 0), 0, true);
        set.push(&ramp(4410, 4410), 4410, true);
        set.flush();
        assert_eq!(layout(&set), vec![(0, 4410)], "8820 source frames at 2:1");
    }

    #[kithara::test]
    fn a_gap_keeps_two_runs_until_it_is_filled() {
        let mut set = runs(SRC);
        set.push(&ramp(4410, 0), 0, true);
        set.push(&ramp(4410, 88_200), 88_200, true);
        set.flush();
        assert_eq!(layout(&set), vec![(0, 2205), (88_200, 2205)]);

        set.push(&ramp(83_790, 4410), 4410, true);
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
        set.push(&ramp(4410, 4410), 4410, true);
        set.push(&ramp(4410, 0), 0, true);
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
            ascending.push(pcm, *at, true);
        }
        ascending.flush();

        let mut shuffled = runs(SRC);
        for index in [5usize, 0, 7, 2, 1, 6, 3, 4] {
            let Some((at, pcm)) = blocks.get(index) else {
                continue;
            };
            shuffled.push(pcm, *at, true);
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
    fn a_full_hold_turns_audio_down_instead_of_giving_it_up() {
        // Ten blocks of 4410 source frames are 22_050 at the detector rate,
        // well past a 10_000 frame hold.
        let mut set = budgeted(SRC, 10_000, usize::MAX);
        let mut refused = 0;
        for block in 0..10u64 {
            if !set.push(&ramp(4410, block * 4410), block * 4410, true) {
                refused += 1;
            }
        }

        assert!(refused > 0, "ten blocks must not fit the hold");
        assert!(
            set.taken().contains(FrameRange::new(0, 4410)),
            "what it took starts at the front of the track"
        );
        let taken: u64 = set.taken().frames();
        assert!(
            taken < 10 * 4410,
            "audio it turned down must stay outside its coverage, got {taken}"
        );
        assert_eq!(
            set.taken().gaps(10 * 4410).len(),
            1,
            "the tail it has not taken is one stretch the pass can be asked for again"
        );
    }

    #[kithara::test]
    fn audio_turned_down_is_taken_once_there_is_room() {
        let mut set = budgeted(SRC, 10_000, usize::MAX);
        for block in 0..10u64 {
            set.push(&ramp(4410, block * 4410), block * 4410, true);
        }
        let stopped = set.taken().frontier();

        set.release(|_| 10_000);
        assert_eq!(
            set.intake(),
            Intake::Anywhere,
            "the detector read everything it held"
        );
        assert!(
            set.push(&ramp(4410, stopped), stopped, true),
            "the stretch it turned down is taken on the next offer"
        );
        assert_eq!(set.taken().frontier(), stopped + 4410);
    }

    #[kithara::test]
    fn audio_the_pass_did_not_read_extends_a_run_without_opening_one() {
        let mut set = budgeted(SRC, usize::MAX, usize::MAX);
        assert!(
            !set.push(&ramp(4410, 88_200), 88_200, false),
            "audio offered from elsewhere is backlog the pass did not plan for"
        );
        assert!(
            set.push(&ramp(4410, 0), 0, true),
            "the pass reads for itself"
        );
        assert!(
            set.push(&ramp(4410, 4410), 4410, false),
            "the same audio continuing a run costs the pass nothing new"
        );
    }

    #[kithara::test]
    fn a_run_cap_leaves_room_for_a_window_in_every_run() {
        // The cap keeps blocks far apart from filling the hold with runs too
        // short for the detector to read.
        let mut set = budgeted(SRC, usize::MAX, 2);
        assert!(set.push(&ramp(4410, 0), 0, true));
        assert!(set.push(&ramp(4410, 88_200), 88_200, true));
        assert!(
            set.intake() == Intake::Continuing,
            "the cap is reached, so audio away from both runs waits"
        );
        assert!(
            !set.push(&ramp(4410, 441_000), 441_000, true),
            "a third run is what the cap turns down"
        );
        assert!(
            set.push(&ramp(4410, 4410), 4410, true),
            "audio continuing a run is what carries it to a full window"
        );
    }

    #[kithara::test]
    fn released_audio_leaves_the_rest_where_it_was() {
        // 48 kHz -> 22.05 kHz is not a whole ratio, so a release rounded up
        // would move the run past the window it must resume at, and one
        // rounded anywhere would shift the audio behind it.
        let source = ramp(48_000 * 4, 0);
        let mut whole = runs(48_000);
        whole.push(&source, 0, true);
        whole.flush();
        let held = mono_of(&whole);

        for opens_at in [1, 4321, 22_050, 40_000] {
            let mut set = runs(48_000);
            set.push(&source, 0, true);
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

    /// Two runs at the cap, and the one bounding the leading gap has fed the
    /// detector, so its held audio starts past where it began.
    fn capped_with_a_released_front() -> TestRuns {
        let mut set = budgeted(48_000, usize::MAX, 2);
        set.push(&ramp(9600, 100_000), 100_000, true);
        set.push(&ramp(4800, 300_000), 300_000, true);
        set.flush();
        let advance = set.offset_in_run(100_000, 104_000);
        set.release(|base| base + advance);
        set
    }

    #[kithara::test]
    fn one_run_reaches_for_another_and_the_rest_wait() {
        let mut set = capped_with_a_released_front();
        assert_eq!(set.intake(), Intake::Continuing, "the cap is reached");
        let front = set.spans().next().map(|(start, _)| start).expect("one run");
        assert!(front > 100_000, "the release must move the run's start");

        assert!(
            set.push(&ramp(4800, 0), 0, true),
            "audio reading through to the run in front of it is taken"
        );
        assert_eq!(set.spans().count(), 3, "the run under way is the third");
        assert!(
            !set.push(&ramp(4800, 200_000), 200_000, true),
            "a second stretch waits until the one under way arrives"
        );
        assert_eq!(set.spans().count(), 3, "{:?}", layout(&set));
    }

    #[kithara::test]
    fn audio_the_pass_did_not_read_waits_for_the_run_it_belongs_to() {
        let mut set = capped_with_a_released_front();

        assert!(
            !set.push(&ramp(4800, 0), 0, false),
            "a producer's range does not open the run that reads a gap through"
        );
        assert_eq!(set.spans().count(), 2, "{:?}", layout(&set));
    }

    #[kithara::test]
    fn audio_in_front_of_a_released_run_costs_it_nothing() {
        let mut set = runs(48_000);
        set.push(&ramp(96_000, 32_000), 32_000, true);
        set.flush();
        let advance = set.offset_in_run(32_000, 60_000);
        set.release(|base| base + advance);
        let (started_at, held) = set
            .spans()
            .next()
            .map(|(start, mono)| (start, mono.to_vec()))
            .expect("one run");
        assert!(started_at > 32_000, "the release must move the run's start");

        set.push(&ramp(32_000, 0), 0, true);

        assert!(
            set.spans()
                .any(|(start, mono)| start == started_at && mono == held),
            "the run kept its audio: {:?}",
            layout(&set)
        );
    }

    #[kithara::test]
    fn reclaimed_mono_releases_charged_capacity() {
        const BUDGET: usize = 4096;
        const MAX_CHARGED_BYTES: usize = BUDGET * size_of::<f32>();
        const TARGET_RATE: u32 = 22_050;

        let pools = non_retaining_pools(4 * MAX_CHARGED_BYTES);
        let mut set = budgeted_with_pools(TARGET_RATE, BUDGET, usize::MAX, pools.clone());
        for cycle in 0..4u64 {
            let at = cycle * BUDGET as u64;
            set.push(&ramp(BUDGET, at), at, true);

            assert_eq!(set.held(), BUDGET);
            assert!(
                pools.stats().allocated_bytes <= MAX_CHARGED_BYTES,
                "cycle {cycle} retains {} charged bytes for a {MAX_CHARGED_BYTES}-byte mono budget",
                pools.stats().allocated_bytes
            );
            set.release(|base| base + BUDGET);
        }
    }

    #[kithara::test]
    fn fragmented_runs_share_the_region_with_loader_scratch() {
        // Every run holds its own resampler, so the run cap is what bounds the
        // region a source arriving in scattered fragments can take.
        const RUNS: usize = 4;
        const LOADER_BYTES: usize = 512 * 1024;
        const POOL_BYTES: usize = 2 * LOADER_BYTES;

        let pools = non_retaining_pools(POOL_BYTES);
        let loader = pools
            .get_with_len::<u8>(LOADER_BYTES)
            .expect("initial loader scratch must fit");

        let mut set = budgeted_with_pools(SRC, usize::MAX, RUNS, pools.clone());
        for fragment in 0..24u16 {
            set.push(&[f32::from(fragment)], u64::from(fragment) * 2, true);
        }
        assert_eq!(
            set.spans().count(),
            RUNS,
            "the cap bounds the runs a fragmented source opens"
        );
        assert_eq!(loader.len(), LOADER_BYTES);

        drop(loader);
        let loader = pools
            .get_with_len::<u8>(LOADER_BYTES)
            .expect("fragmented analysis must leave capacity for loader scratch");
        assert_eq!(loader.len(), LOADER_BYTES);

        drop(set);
        drop(loader);
        pools
            .get_with_len::<u8>(POOL_BYTES)
            .expect("completed analysis must return its capacity to the region");
    }

    #[kithara::test]
    fn a_non_integer_ratio_keeps_joins_on_position() {
        // 48 kHz -> 22.05 kHz is not a whole ratio, so a per-segment rounding
        // error would show up as a length drift at every join.
        let mut set = runs(48_000);
        for block in 0..10u64 {
            set.push(&ramp(4801, block * 4801), block * 4801, true);
        }
        set.flush();
        let total = set.offset_in_run(0, 48_010);
        assert_eq!(layout(&set), vec![(0, total)]);
    }
}
