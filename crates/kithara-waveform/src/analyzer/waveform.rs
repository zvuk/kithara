use std::{array, collections::BTreeMap};

use kithara_blob::{BlobError, Writer};
use kithara_bufpool::{HasPool, PoolError, PoolRegion, SampleBuffer};
use kithara_platform::sync::Arc;
use kithara_signal::CoverageWrite;
use num_traits::cast::ToPrimitive;
use rangemap::RangeSet;
use realfft::{RealFftPlanner, RealToComplex, num_complex::Complex};

use crate::{
    Band,
    bucket::{Bucket, Waveform},
    bucketize::bucketize,
    params::AnalysisParams,
    resume::WaveformResume,
};

pub(super) struct Consts;

impl Consts {
    const HANN_A0: f32 = 0.5;
    const HOP_DIVISOR: usize = 4;
    pub(super) const MAX_PARTIAL: usize = 256;
    const MIN_FFT_SIZE: usize = 2;
}

pub(super) struct Partial {
    pub(super) written: RangeSet<u64>,
    pub(super) samples: SampleBuffer,
    pub(super) seq: u64,
}

/// Position-addressed waveform analyzer: mono downmix, then a band-energy
/// series indexed by absolute window position. A window is reduced once its
/// span sits inside one covered run, so ranges may arrive in any order, twice
/// or overlapping. [`Self::snapshot`] folds it into low/mid/high bucket
/// heights.
pub struct WaveformAnalyzer {
    pub(super) params: AnalysisParams,
    pub(super) fft: Arc<dyn RealToComplex<f32>>,
    pub(super) bands: BTreeMap<u64, [f32; Band::COUNT]>,
    pub(super) partial: BTreeMap<u64, Partial>,
    downmix: SampleBuffer,
    pub(super) fft_input: SampleBuffer,
    pub(super) hann: SampleBuffer,
    pub(super) fft_output: Vec<Complex<f32>>,
    pub(super) fft_scratch: Vec<Complex<f32>>,
    pub(super) band_bin_inv: [f32; Band::COUNT],
    pub(super) opened: u64,
    pub(super) low_mid_bin: usize,
    pub(super) mid_high_bin: usize,
    pub(super) window_hop: usize,
}

impl WaveformAnalyzer {
    /// Create a waveform analyzer using the registered sample pool.
    ///
    /// # Errors
    ///
    /// Returns [`PoolError`] when the FFT or window buffers do not fit the
    /// shared region budget.
    ///
    /// Divides each band's summed energy by its bin count so a wide band does not outweigh a narrow
    /// one by sheer bin count, making every band an energy density comparable across bands.
    pub fn new<S>(
        sample_rate: u32,
        params: AnalysisParams,
        pools: &PoolRegion<S>,
    ) -> Result<Self, PoolError>
    where
        S: HasPool<f32>,
    {
        let fft_size = params.fft_size().max(Consts::MIN_FFT_SIZE);
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(fft_size);
        let fft_input = pools.get_with_len::<f32>(fft_size)?;
        let fft_output = fft.make_output_vec();
        let fft_scratch = fft.make_scratch_vec();

        let hann = hann_window(fft_size, pools)?;
        let bins = fft_output.len();
        let rate = sample_rate.to_f32().unwrap_or(0.0);
        let size_f = fft_size.to_f32().unwrap_or(1.0);
        let bin_hz = if size_f > 0.0 { rate / size_f } else { 0.0 };
        let low_mid_bin = crossover_bin(params.low_mid_hz(), bin_hz, bins);
        let mid_high_bin = crossover_bin(params.mid_high_hz(), bin_hz, bins).max(low_mid_bin);
        let inv = |count: usize| 1.0 / count.max(1).to_f32().unwrap_or(1.0);
        let band_bin_inv = [
            inv(low_mid_bin.saturating_sub(1)),
            inv(mid_high_bin.saturating_sub(low_mid_bin)),
            inv(bins.saturating_sub(mid_high_bin)),
        ];

        Ok(Self {
            params,
            fft,
            hann,
            low_mid_bin,
            mid_high_bin,
            band_bin_inv,
            fft_input,
            fft_output,
            fft_scratch,
            downmix: pools.get::<f32>(),
            window_hop: (fft_size / Consts::HOP_DIVISOR).max(1),
            bands: BTreeMap::new(),
            partial: BTreeMap::new(),
            opened: 0,
        })
    }

    /// Fold one interleaved block starting at source frame `at`: downmix to
    /// mono (channel mean), scatter it into every window it touches and reduce
    /// every window the block completes. Blocks may arrive in any order, twice,
    /// or overlapping.
    /// # Errors
    ///
    /// Returns [`PoolError`] when downmix or partial-window storage cannot
    /// grow under the shared region budget.
    pub fn push<S>(
        &mut self,
        pools: &PoolRegion<S>,
        pcm: &[f32],
        channels: usize,
        at: u64,
    ) -> Result<(), PoolError>
    where
        S: HasPool<f32>,
    {
        if channels == 0 {
            return Ok(());
        }
        let frames = pcm.len() / channels;
        let Ok(span) = u64::try_from(frames) else {
            return Ok(());
        };
        if span == 0 {
            return Ok(());
        }

        let inv_channels = 1.0 / channels.to_f32().unwrap_or(1.0);
        self.downmix.ensure_len(frames)?;
        self.downmix.truncate(frames);
        for (dst, frame) in self.downmix.iter_mut().zip(pcm.chunks_exact(channels)) {
            *dst = frame.iter().sum::<f32>() * inv_channels;
        }

        let mono = std::mem::replace(&mut self.downmix, pools.get::<f32>());
        let result = self.push_mono(pools, &mono, at, span);
        self.downmix = mono;
        result
    }

    /// Selects the windows overlapping `[at, end)`: those where `k*hop < end` and `k*hop + size >
    /// at`.
    fn push_mono<S>(
        &mut self,
        pools: &PoolRegion<S>,
        mono: &[f32],
        at: u64,
        span: u64,
    ) -> Result<(), PoolError>
    where
        S: HasPool<f32>,
    {
        let hop = self.hop();
        let size = self.size();
        let end = at.saturating_add(span);
        let first = if at >= size { (at - size) / hop + 1 } else { 0 };
        let last = (end - 1) / hop;

        for index in first..=last {
            self.scatter(pools, index, mono, at, end)?;
        }

        for index in first..=last {
            self.reduce_if_complete(index);
        }
        self.evict_overflow();
        Ok(())
    }

    /// Re-enter a stopped pass from its resume record.
    ///
    /// # Errors
    ///
    /// Returns [`BlobError::Corrupt`] if the record contradicts the pass it
    /// claims to restore, or [`BlobError::Pool`] if its samples do not fit.
    pub fn restore<S>(
        &mut self,
        pools: &PoolRegion<S>,
        resume: WaveformResume,
    ) -> Result<(), BlobError>
    where
        S: HasPool<f32>,
    {
        if resume.partials.len() > Consts::MAX_PARTIAL {
            return Err(BlobError::Corrupt);
        }

        let mut bands = BTreeMap::new();
        for (index, energy) in resume.bands {
            bands.insert(index, energy);
        }

        let mut partial = BTreeMap::new();
        for held in resume.partials {
            if held.samples.len() != self.window_size()
                || held.seq >= resume.opened
                || bands.contains_key(&held.index)
            {
                return Err(BlobError::Corrupt);
            }
            let start = held.index.saturating_mul(self.hop());
            let span = start..start.saturating_add(self.size());
            if held
                .written
                .iter()
                .any(|run| run.start < span.start || run.end > span.end)
            {
                return Err(BlobError::Corrupt);
            }
            partial.insert(
                held.index,
                Partial {
                    samples: sample_buffer(pools, &held.samples)?,
                    written: held.written,
                    seq: held.seq,
                },
            );
        }

        self.bands = bands;
        self.partial = partial;
        self.opened = resume.opened;
        Ok(())
    }

    /// Fold the band-energy series into per-bucket band heights, leaving the
    /// pass able to accept further ranges. `extent` is the source length in
    /// frames when it is known: it sets the window count the buckets are
    /// spread over, so bucket boundaries stay put as coverage grows.
    #[must_use]
    pub fn snapshot(&mut self, buckets: usize, extent: Option<u64>) -> Waveform {
        if self.bands.is_empty()
            && let Some(extent) = extent
        {
            self.reduce_padded(extent);
        }

        let total = self.window_count(extent);
        let mut raw = vec![[0.0; Band::COUNT]; total];
        for (&index, energy) in &self.bands {
            if let Ok(index) = usize::try_from(index)
                && let Some(slot) = raw.get_mut(index)
            {
                *slot = *energy;
            }
        }

        let buckets = buckets.min(total);
        let max = |a: [f32; Band::COUNT], b: [f32; Band::COUNT]| array::from_fn(|i| a[i].max(b[i]));
        let energy = bucketize(&raw, buckets, [0.0; Band::COUNT], max);
        let bands = normalize_bands(energy, self.params.band_gain());

        let out: Vec<Bucket> = bands
            .into_iter()
            .map(|b| Bucket::new(b[Band::Low.idx()], b[Band::Mid.idx()], b[Band::High.idx()]))
            .collect();
        Waveform::analysed(out)
    }

    pub fn write_resume(&self, out: &mut Vec<u8>) {
        let mut writer = Writer::new(out);
        writer.write_len(self.bands.len());
        for (index, bands) in &self.bands {
            writer.write_u64(*index);
            for band in bands {
                writer.write_f32(*band);
            }
        }
        writer.write_len(self.partial.len());
        for (index, partial) in &self.partial {
            writer.write_u64(*index);
            writer.write_samples(&partial.samples);
            writer.write_coverage(&partial.written);
            writer.write_u64(partial.seq);
        }
        writer.write_u64(self.opened);
    }
}

fn sample_buffer<S>(pools: &PoolRegion<S>, samples: &[f32]) -> Result<SampleBuffer, BlobError>
where
    S: HasPool<f32>,
{
    let mut buffer = pools.get_with_len::<f32>(samples.len())?;
    buffer.copy_from_slice(samples);
    Ok(buffer)
}

fn hann_window<S>(size: usize, pools: &PoolRegion<S>) -> Result<SampleBuffer, PoolError>
where
    S: HasPool<f32>,
{
    let mut hann = pools.get_with_len::<f32>(size)?;
    if size <= 1 {
        hann.fill(1.0);
        return Ok(hann);
    }
    let denom = (size - 1).to_f32().unwrap_or(1.0);
    let scale = std::f32::consts::TAU / denom;
    for (n, sample) in hann.iter_mut().enumerate() {
        let phase = scale * n.to_f32().unwrap_or(0.0);
        *sample = Consts::HANN_A0.mul_add(-phase.cos(), Consts::HANN_A0);
    }
    Ok(hann)
}

fn crossover_bin(hz: f32, bin_hz: f32, bins: usize) -> usize {
    if bin_hz <= 0.0 {
        return bins;
    }
    let idx = (hz / bin_hz).to_usize().unwrap_or(bins);
    idx.min(bins)
}

fn normalize_bands(
    energy: Vec<[f32; Band::COUNT]>,
    gain: [f32; Band::COUNT],
) -> Vec<[f32; Band::COUNT]> {
    let mut mags: Vec<[f32; Band::COUNT]> = energy
        .into_iter()
        .map(|e| array::from_fn(|i| e[i].sqrt() * gain[i]))
        .collect();

    let max = mags
        .iter()
        .flat_map(|m| m.iter().copied())
        .fold(0.0_f32, f32::max);
    if max > 0.0 {
        let inv = 1.0 / max;
        for m in &mut mags {
            for v in &mut *m {
                *v *= inv;
            }
        }
    }
    mags
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use kithara_blob::BlobError;
    use kithara_test_fixtures::analysis_fixtures::{
        analysis_silence, waveform_half, waveform_high, waveform_low, waveform_mid, waveform_mix,
        waveform_opposed, waveform_square, waveform_tiny, waveform_tone,
    };
    use kithara_test_utils::kithara;
    use rangemap::RangeSet;

    use super::{WaveformAnalyzer, hann_window, normalize_bands};
    use crate::{
        AnalysisParams, Band,
        bucket::Bucket,
        resume::{WaveformPartialResume, WaveformResume},
        test_pools::{TestPools, pools},
    };

    struct Consts;

    impl Consts {
        const EPS: f32 = 1e-6;
        const SR: u32 = 44_100;
    }

    struct Pass {
        pools: kithara_bufpool::PoolRegion<TestPools>,
        analyzer: WaveformAnalyzer,
    }

    impl Pass {
        fn new(params: AnalysisParams) -> Self {
            let pools = pools();
            Self {
                analyzer: WaveformAnalyzer::new(Consts::SR, params, &pools)
                    .expect("waveform buffers fit the test region"),
                pools,
            }
        }

        fn push(&mut self, pcm: &[f32], channels: usize, at: u64) {
            self.analyzer
                .push(&self.pools, pcm, channels, at)
                .expect("waveform buffers fit the test region");
        }

        fn whole(&mut self, pcm: &[f32], channels: usize, buckets: usize) -> Vec<Bucket> {
            self.push(pcm, channels, 0);
            let extent = u64::try_from(pcm.len() / channels).unwrap_or(0);
            self.analyzer
                .snapshot(buckets, Some(extent))
                .buckets()
                .to_vec()
        }
    }

    /// Both helpers are macros so the mutation lane judges the contracts under
    /// test rather than a fixture it can rewrite without any test noticing.
    macro_rules! assert_approx {
        ($actual:expr, $want:expr, $($msg:tt)+) => {
            let (actual, want) = ($actual, $want);
            assert!((actual - want).abs() <= Consts::EPS, $($msg)+);
        };
    }

    macro_rules! flat {
        () => {
            AnalysisParams::builder().band_gain([1.0; 3]).build()
        };
    }

    impl Pass {
        fn restore(&mut self, resume: WaveformResume) -> Result<(), BlobError> {
            self.analyzer.restore(&self.pools, resume)
        }

        /// The resume record this pass would hand its successor, rebuilt from
        /// the state itself so a test can bend one field at a time.
        fn resume(&self) -> WaveformResume {
            WaveformResume {
                bands: self
                    .analyzer
                    .bands
                    .iter()
                    .map(|(&index, &energy)| (index, energy))
                    .collect(),
                partials: self
                    .analyzer
                    .partial
                    .iter()
                    .map(|(&index, partial)| WaveformPartialResume {
                        index,
                        samples: partial.samples.to_vec().into_boxed_slice(),
                        written: partial.written.clone(),
                        seq: partial.seq,
                    })
                    .collect(),
                opened: self.analyzer.opened,
            }
        }
    }

    /// A pass stopped mid-window: enough PCM to reduce some windows and leave
    /// one partial behind, which is the only state worth resuming.
    fn interrupted_pass(pcm: &[f32]) -> Pass {
        let mut pass = Pass::new(flat!());
        let head = pcm.len() / 2;
        pass.push(&pcm[..head], 1, 0);
        assert!(
            pass.analyzer.partial_len() > 0,
            "the fixture must leave a partial window to resume"
        );
        pass
    }

    #[kithara::test]
    fn a_restored_pass_snapshots_what_the_stopped_one_would_have(waveform_tone: Vec<f32>) {
        let stopped = interrupted_pass(&waveform_tone);
        let expected = Pass::new(flat!())
            .analyzer
            .snapshot(0, None)
            .buckets()
            .to_vec();
        assert!(expected.is_empty(), "a fresh pass has nothing to show");

        let mut resumed = Pass::new(flat!());
        resumed
            .restore(stopped.resume())
            .expect("the record is its own state");

        let mut stopped = stopped;
        let extent = u64::try_from(waveform_tone.len()).unwrap_or(0);
        assert_eq!(
            resumed.analyzer.snapshot(8, Some(extent)).buckets(),
            stopped.analyzer.snapshot(8, Some(extent)).buckets(),
            "a resumed pass stands exactly where the stopped one did"
        );
    }

    #[kithara::test]
    fn the_partial_limit_is_the_last_count_a_record_may_carry(waveform_tone: Vec<f32>) {
        let stopped = interrupted_pass(&waveform_tone);
        let held = stopped
            .resume()
            .partials
            .pop()
            .expect("the stopped pass holds a partial");
        let fill = |count: usize| {
            let mut resume = stopped.resume();
            resume.partials = (0..count)
                .map(|slot| WaveformPartialResume {
                    samples: held.samples.clone(),
                    written: RangeSet::new(),
                    index: u64::try_from(slot).unwrap_or(0) + u64::from(u32::MAX),
                    seq: 0,
                })
                .collect();
            resume.opened = u64::try_from(count).unwrap_or(0) + 1;
            resume
        };

        assert!(
            Pass::new(flat!())
                .restore(fill(super::Consts::MAX_PARTIAL))
                .is_ok(),
            "a record holding exactly the limit is still a record"
        );
        assert!(
            matches!(
                Pass::new(flat!()).restore(fill(super::Consts::MAX_PARTIAL + 1)),
                Err(BlobError::Corrupt)
            ),
            "one partial past the limit is not a record this pass wrote"
        );
    }

    #[kithara::test]
    #[case::samples_shorter("a window whose samples do not fill it")]
    #[case::seq_at_open("a window opened no earlier than the counter that opened it")]
    #[case::already_reduced("a window that is both partial and already reduced")]
    #[case::run_before_window("coverage reaching before the window starts")]
    #[case::run_past_window("coverage reaching past the window's end")]
    fn a_record_that_disagrees_with_itself_is_corrupt(#[case] flaw: &str, waveform_tone: Vec<f32>) {
        let stopped = interrupted_pass(&waveform_tone);
        let mut resume = stopped.resume();
        let held = resume.partials.first_mut().expect("a held window");
        let start = held.index * stopped.analyzer.hop();
        let span = start..start + stopped.analyzer.size();
        match flaw {
            "a window whose samples do not fill it" => {
                let mut samples = held.samples.to_vec();
                samples.pop();
                held.samples = samples.into_boxed_slice();
            }
            "a window opened no earlier than the counter that opened it" => {
                held.seq = resume.opened;
            }
            "a window that is both partial and already reduced" => {
                let index = held.index;
                resume.bands.push((index, [0.0; Band::COUNT]));
            }
            "coverage reaching before the window starts" => {
                let mut written = RangeSet::new();
                written.insert(span.start.saturating_sub(1)..span.start + 1);
                held.written = written;
            }
            _ => {
                let mut written = RangeSet::new();
                written.insert(span.end..span.end + 1);
                held.written = written;
            }
        }

        assert!(
            matches!(Pass::new(flat!()).restore(resume), Err(BlobError::Corrupt)),
            "{flaw} cannot be resumed"
        );
    }

    #[kithara::test]
    fn a_record_may_cover_the_window_up_to_its_last_frame(waveform_tone: Vec<f32>) {
        let stopped = interrupted_pass(&waveform_tone);
        let mut resume = stopped.resume();
        let held = resume.partials.first_mut().expect("a held window");
        let start = held.index * stopped.analyzer.hop();
        let span = start..start + stopped.analyzer.size();
        let mut written = RangeSet::new();
        // The window's last frame is inside the window: coverage that ends
        // exactly where the window ends is a held window, not a corrupt one.
        written.insert(span.end.saturating_sub(1)..span.end);
        held.written = written;

        assert!(
            Pass::new(flat!()).restore(resume).is_ok(),
            "coverage ending on the window's own end is within it"
        );
    }

    #[kithara::test]
    fn a_record_carries_the_state_that_makes_it_differ(waveform_tone: Vec<f32>) {
        let mut empty = Vec::new();
        Pass::new(flat!()).analyzer.write_resume(&mut empty);

        let mut stopped = Vec::new();
        interrupted_pass(&waveform_tone)
            .analyzer
            .write_resume(&mut stopped);

        assert!(
            stopped.len() > empty.len(),
            "a pass with windows behind it writes more than a fresh one"
        );
    }

    #[kithara::test]
    fn the_analysis_window_rises_from_zero_to_one_and_back() {
        let pools = pools();
        let hann = hann_window(5, &pools).expect("the window fits the test region");
        let expected = [0.0, 0.5, 1.0, 0.5, 0.0];
        for (n, (&actual, &want)) in hann.iter().zip(expected.iter()).enumerate() {
            assert_approx!(
                actual,
                want,
                "sample {n} of a 5-point window is {actual}, expected {want}"
            );
        }
    }

    #[kithara::test]
    #[case(0)]
    #[case(1)]
    fn a_window_too_short_to_taper_is_flat(#[case] size: usize) {
        let pools = pools();
        let hann = hann_window(size, &pools).expect("the window fits the test region");
        assert_eq!(hann.len(), size);
        assert!(
            hann.iter()
                .all(|&sample| (sample - 1.0).abs() <= Consts::EPS),
            "a window with no slope to describe leaves every sample as it was"
        );
    }

    #[kithara::test]
    fn band_gain_scales_a_band_before_the_shared_normalisation() {
        let energy = vec![[1.0, 1.0, 1.0]];
        let bands = normalize_bands(energy, [1.0, 0.5, 0.25]);
        let scaled = bands.first().expect("one bucket in, one bucket out");
        assert_approx!(scaled[0], 1.0, "the loudest band normalises to one");
        assert_approx!(scaled[1], 0.5, "half the gain is half the height");
        assert_approx!(scaled[2], 0.25, "a quarter of the gain is a quarter");
    }

    #[kithara::test]
    fn no_frames_snapshots_empty() {
        assert!(
            Pass::new(AnalysisParams::default())
                .analyzer
                .snapshot(8, None)
                .is_empty()
        );
    }

    #[kithara::test]
    fn zero_buckets_snapshots_empty(waveform_half: Vec<f32>) {
        let mut pass = Pass::new(AnalysisParams::default());
        assert!(pass.whole(&waveform_half, 1, 0).is_empty());
    }

    #[kithara::test]
    fn loudest_band_normalises_to_one(waveform_square: Vec<f32>) {
        // Broadband square wave: after shared normalization the single loudest
        // band-bucket reaches exactly 1.0.
        let pcm = waveform_square;
        let wave = Pass::new(flat!()).whole(&pcm, 1, 10);
        assert_eq!(wave.len(), 10);
        let max = wave
            .iter()
            .map(|b| b.low().max(b.mid()).max(b.high()))
            .fold(0.0_f32, f32::max);
        assert_approx!(max, 1.0, "loudest band must normalise to 1.0, got {max}");
    }

    #[kithara::test]
    fn silence_is_all_zero(analysis_silence: Vec<f32>) {
        let wave = Pass::new(AnalysisParams::default()).whole(&analysis_silence[..16_384], 1, 8);
        assert_eq!(wave.len(), 8);
        for b in &wave {
            assert_eq!(*b, Bucket::default(), "silence -> all-zero bucket: {b:?}");
        }
    }

    #[kithara::test]
    fn fewer_frames_than_buckets_stays_finite(waveform_tiny: Vec<f32>) {
        let wave = Pass::new(AnalysisParams::default()).whole(&waveform_tiny, 1, 5);
        assert!(!wave.is_empty() && wave.len() <= 5, "len {}", wave.len());
        for b in &wave {
            for v in [b.low(), b.mid(), b.high()] {
                assert!(
                    v.is_finite() && (0.0..=1.0).contains(&v),
                    "band must stay finite in [0,1]: {b:?}"
                );
            }
        }
    }

    #[kithara::test]
    fn output_is_native_window_resolution_capped(waveform_tone: Vec<f32>) {
        // Ten full FFT windows (4096 + 9 hops of 1024).
        let samples = 4096 + 1024 * 9;
        let pcm = &waveform_tone[..samples];

        // Above the window count: native resolution, never fabricated.
        assert_eq!(
            Pass::new(flat!()).whole(pcm, 1, 100_000).len(),
            10,
            "large = native count"
        );
        // Below it: still decimates (long-track cap).
        assert_eq!(
            Pass::new(flat!()).whole(pcm, 1, 4).len(),
            4,
            "small request decimates"
        );
    }

    #[kithara::test]
    fn stereo_downmix_is_channel_mean(waveform_opposed: Vec<f32>) {
        // L=1, R=-1 cancels to mono 0 -> silence.
        let pcm = waveform_opposed;
        let wave = Pass::new(AnalysisParams::default()).whole(&pcm, 2, 4);
        for b in &wave {
            assert_eq!(*b, Bucket::default(), "cancelling stereo -> silence: {b:?}");
        }
    }

    #[kithara::test]
    fn deterministic_for_same_input(waveform_tone: Vec<f32>) {
        let pcm = waveform_tone;
        let run = || Pass::new(AnalysisParams::default()).whole(&pcm, 1, 64);
        assert_eq!(run(), run(), "same PCM must produce the same waveform");
    }

    #[kithara::test]
    fn window_split_across_chunks_matches_unsplit(waveform_tone: Vec<f32>) {
        let pcm = waveform_tone;
        let whole = Pass::new(flat!()).whole(&pcm, 1, 12);

        // Split at 1500 frames: no boundary lands on a window edge, so every
        // window is assembled from two blocks.
        let mut split = Pass::new(flat!());
        for (index, part) in pcm.chunks(1500).enumerate() {
            let at = u64::try_from(index * 1500).unwrap_or(0);
            split.push(part, 1, at);
        }
        let extent = u64::try_from(pcm.len()).unwrap_or(0);
        assert_eq!(
            split.analyzer.snapshot(12, Some(extent)).buckets(),
            whole,
            "a window split across blocks must reduce identically"
        );
    }

    #[kithara::test]
    fn shuffled_and_duplicated_blocks_match_ascending(waveform_tone: Vec<f32>) {
        let pcm = waveform_tone;
        let ascending = Pass::new(flat!()).whole(&pcm, 1, 12);

        let blocks: Vec<(u64, &[f32])> = pcm
            .chunks(2048)
            .enumerate()
            .map(|(index, part)| (u64::try_from(index * 2048).unwrap_or(0), part))
            .collect();
        let mut shuffled = Pass::new(flat!());
        for &(at, part) in [6, 1, 7, 0, 3, 5, 2, 4, 3, 0]
            .iter()
            .filter_map(|i| blocks.get(*i))
        {
            shuffled.push(part, 1, at);
        }
        let extent = u64::try_from(pcm.len()).unwrap_or(0);
        assert_eq!(
            shuffled.analyzer.snapshot(12, Some(extent)).buckets(),
            ascending,
            "shuffled and duplicated blocks must yield the same waveform"
        );
    }

    #[kithara::test]
    fn a_gap_leaves_its_windows_out_until_it_is_filled(waveform_tone: Vec<f32>) {
        let pcm = waveform_tone;
        let complete = Pass::new(flat!()).whole(&pcm, 1, 12);
        let extent = u64::try_from(pcm.len()).unwrap_or(0);

        let mut gapped = Pass::new(flat!());
        gapped.push(&pcm[..4096], 1, 0);
        gapped.push(&pcm[8192..], 1, 8192);
        let partial = gapped.analyzer.snapshot(12, Some(extent));
        assert_ne!(
            partial.buckets(),
            complete,
            "an uncovered span must not carry the covered result"
        );

        gapped.push(&pcm[4096..8192], 1, 4096);
        assert_eq!(
            gapped.analyzer.snapshot(12, Some(extent)).buckets(),
            complete,
            "filling the gap must produce the contiguous result"
        );
    }

    #[kithara::test]
    fn partial_windows_are_capped(waveform_half: Vec<f32>) {
        // Isolated single-frame blocks complete no window, so each one only
        // opens the windows that contain it.
        let mut pass = Pass::new(flat!());
        for block in 0..90_u64 {
            pass.push(&waveform_half[..1], 1, block * 100_000);
        }
        assert!(
            pass.analyzer.partial_len() <= 256,
            "live partial windows must stay capped, got {}",
            pass.analyzer.partial_len()
        );
    }

    #[kithara::test]
    fn an_evicted_window_is_not_reduced_from_what_survived(
        waveform_tone: Vec<f32>,
        waveform_half: Vec<f32>,
    ) {
        // Half a window arrives, the cap evicts it, then the other half
        // arrives. The window's span is covered, but this analyzer no longer
        // holds the first half: reducing it now would publish a half-silent
        // window instead of leaving the span unanalysed.
        let pcm = &waveform_tone[..4096];
        let mut pass = Pass::new(flat!());
        pass.push(&pcm[..2048], 1, 0);
        for block in 1..90_u64 {
            pass.push(&waveform_half[..1], 1, block * 100_000);
        }
        pass.push(&pcm[2048..], 1, 2048);

        assert!(
            pass.analyzer.reduced(0).is_none(),
            "an evicted window must stay absent, not be reduced from half its samples"
        );
    }

    #[kithara::test]
    fn snapshot_leaves_the_pass_usable(waveform_tone: Vec<f32>) {
        let pcm = waveform_tone;
        let extent = u64::try_from(pcm.len()).unwrap_or(0);
        let mut pass = Pass::new(flat!());

        pass.push(&pcm[..8192], 1, 0);
        let early = pass.analyzer.snapshot(12, Some(extent));
        pass.push(&pcm[8192..], 1, 8192);
        let late = pass.analyzer.snapshot(12, Some(extent));

        assert_eq!(early.len(), late.len(), "bucket count must not shift");
        assert_ne!(
            early.buckets(),
            late.buckets(),
            "the second snapshot must reflect the added coverage"
        );
        assert_eq!(
            late.buckets(),
            Pass::new(flat!()).whole(&pcm, 1, 12),
            "two snapshots must not change the final result"
        );
    }

    fn dominant(pcm: &[f32]) -> Bucket {
        // Floor disabled so routing isn't coupled to the gate; unity gain so it
        // isn't coupled to the perceptual balance.
        let params = AnalysisParams::builder()
            .band_gain(flat!().band_gain())
            .energy_floor(0.0)
            .build();
        Pass::new(params)
            .whole(pcm, 1, 4)
            .into_iter()
            .max_by(|a, b| {
                a.low()
                    .max(a.mid())
                    .max(a.high())
                    .total_cmp(&b.low().max(b.mid()).max(b.high()))
            })
            .unwrap_or_default()
    }

    #[kithara::test]
    fn low_frequency_lands_in_low_band(waveform_low: Vec<f32>) {
        let b = dominant(&waveform_low);
        assert!(
            b.low() > b.mid() && b.low() > b.high(),
            "80 Hz must be low-dominant: {b:?}"
        );
    }

    #[kithara::test]
    fn mid_frequency_lands_in_mid_band(waveform_mid: Vec<f32>) {
        let b = dominant(&waveform_mid);
        assert!(
            b.mid() > b.low() && b.mid() > b.high(),
            "1 kHz must be mid-dominant: {b:?}"
        );
    }

    #[kithara::test]
    fn high_frequency_lands_in_high_band(waveform_high: Vec<f32>) {
        let b = dominant(&waveform_high);
        assert!(
            b.high() > b.low() && b.high() > b.mid(),
            "10 kHz must be high-dominant: {b:?}"
        );
    }

    #[kithara::test]
    fn full_spectrum_track_has_no_color_gaps(waveform_mix: Vec<f32>) {
        // Regression for a band series coarser than the bucket count, which left
        // columns with no bar. Every column of a full-spectrum track must carry
        let pcm = waveform_mix;
        let wave = Pass::new(AnalysisParams::default()).whole(&pcm, 1, 1500);
        let gaps = wave
            .iter()
            .filter(|b| b.low().max(b.mid()).max(b.high()) <= 0.0)
            .count();
        assert_eq!(gaps, 0, "every column must carry a bar");
    }

    #[kithara::test]
    fn a_stereo_push_carries_the_average_of_its_channels(waveform_tone: Vec<f32>) {
        // A silent right channel halves the signal, and an interleaved pair
        // spans half as many frames as it carries samples.
        let halved: Vec<f32> = waveform_tone.iter().map(|s| s * 0.5).collect();
        let mut mono = Pass::new(flat!());
        mono.push(&halved, 1, 0);

        let interleaved: Vec<f32> = waveform_tone.iter().flat_map(|&s| [s, 0.0]).collect();
        let mut stereo = Pass::new(flat!());
        stereo.push(&interleaved, 2, 0);

        assert_eq!(
            stereo.analyzer.bands, mono.analyzer.bands,
            "the downmix of a half-silent pair is the mono take at half amplitude"
        );
        assert_eq!(
            Pass::new(flat!()).whole(&interleaved, 2, 8),
            Pass::new(flat!()).whole(&halved, 1, 8),
            "an interleaved take spans the frames it holds, not the samples"
        );
    }

    #[kithara::test]
    #[case::just_past_the_first_window(1)]
    #[case::far_down_the_stream(3)]
    fn a_push_reaches_exactly_the_windows_its_span_overlaps(
        #[case] windows_in: u64,
        waveform_tone: Vec<f32>,
    ) {
        let mut pass = Pass::new(flat!());
        let hop = pass.analyzer.hop();
        let size = pass.analyzer.size();
        // Start off the hop grid and end on it: both ends of the reached range
        // have to be computed, and the window starting at `end` touches nothing.
        let at = size * windows_in + hop / 2;
        let end = (at + size * 2).div_ceil(hop) * hop;
        let span = end - at;
        let pcm: Vec<f32> = waveform_tone
            .iter()
            .copied()
            .cycle()
            .take(usize::try_from(span).unwrap_or(0))
            .collect();
        pass.push(&pcm, 1, at);

        let reached: std::collections::BTreeSet<u64> = pass
            .analyzer
            .bands
            .keys()
            .chain(pass.analyzer.partial.keys())
            .copied()
            .collect();
        let expected: std::collections::BTreeSet<u64> = (0..=end / hop)
            .filter(|index| {
                let start = index * hop;
                start < end && start + size > at
            })
            .collect();
        assert_eq!(
            reached, expected,
            "a push must reach every window its span overlaps, and no other"
        );
    }
}
