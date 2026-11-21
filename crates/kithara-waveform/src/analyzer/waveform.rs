use std::{
    array,
    collections::BTreeMap,
    num::{NonZeroU32, NonZeroU64},
};

use kithara_blob::{BlobError, Writer};
use kithara_bufpool::{HasPool, PoolRegion, SampleBuffer};
use kithara_dsp::filter::FilterError;
use kithara_signal::CoverageWrite;
use rangemap::RangeSet;

use super::envelope::Envelope;
use crate::{AnalysisParams, AnalyzerError, MAX_BUCKETS, Waveform, WaveformResume, consts};

pub(super) type Columns = [[u8; 8]; consts::COLUMNS];

pub(super) struct Partial {
    pub(super) written: RangeSet<u64>,
    pub(super) samples: SampleBuffer,
    pub(super) seq: u64,
}

/// Position-addressed stereo peak analysis with independent pre-rolled windows.
pub struct WaveformAnalyzer {
    pub(super) envelope: Envelope,
    pub(super) columns: BTreeMap<u64, Columns>,
    pub(super) partial: BTreeMap<u64, Partial>,
    pub(super) opened: u64,
    pub(super) column_frames: usize,
}

impl WaveformAnalyzer {
    /// # Errors
    /// Invalid sample rate, crossover or gain, unavailable filter setup, or pool exhaustion.
    pub fn new<S>(
        sample_rate: u32,
        params: AnalysisParams,
        pools: &PoolRegion<S>,
    ) -> Result<Self, AnalyzerError>
    where
        S: HasPool<f32>,
    {
        if sample_rate == 0 {
            return Err(FilterError::Parameters.into());
        }
        let scale = sample_rate.div_ceil(48_000).next_power_of_two();
        let mut column_frames = 64 * usize::try_from(scale).map_err(|_| FilterError::Shape)?;
        while u64::try_from(column_frames).unwrap_or(u64::MAX) * 500 > u64::from(sample_rate)
            && column_frames > 1
        {
            column_frames /= 2;
        }
        let envelope = Envelope::new(sample_rate, params, column_frames, pools)?;
        Ok(Self {
            envelope,
            columns: BTreeMap::new(),
            partial: BTreeMap::new(),
            opened: 0,
            column_frames,
        })
    }

    /// Accepts complete interleaved frames at absolute source position `at`.
    /// Mono fills both sides; multichannel input contributes its first two channels.
    ///
    /// # Errors
    /// Nonfinite input, failed window filtering, or exhausted partial-window storage.
    pub fn push<S>(
        &mut self,
        pools: &PoolRegion<S>,
        pcm: &[f32],
        channels: usize,
        at: u64,
    ) -> Result<(), AnalyzerError>
    where
        S: HasPool<f32>,
    {
        if channels == 0 || pcm.len() < channels {
            return Ok(());
        }
        if pcm.iter().any(|sample| !sample.is_finite()) {
            return Err(FilterError::Parameters.into());
        }
        let frames = pcm.len() / channels;
        let end = at.saturating_add(u64::try_from(frames).unwrap_or(u64::MAX));
        let size = self.size();
        let hop = self.hop();
        let first = if at >= size { (at - size) / hop + 1 } else { 0 };
        let last = end.saturating_sub(1) / hop;
        for index in (first..=last).filter(|index| selected(*index)) {
            self.scatter(pools, index, pcm, channels, at, end)?;
            self.reduce_if_complete(index)?;
            self.evict_overflow();
        }
        Ok(())
    }

    /// # Errors
    /// Corrupt, contradictory or oversized checkpoint, or exhausted sample storage.
    pub fn restore<S>(
        &mut self,
        pools: &PoolRegion<S>,
        resume: WaveformResume,
    ) -> Result<(), BlobError>
    where
        S: HasPool<f32>,
    {
        if resume.partials.len() > consts::MAX_PARTIAL {
            return Err(BlobError::Corrupt);
        }
        let mut columns = BTreeMap::new();
        for (index, values) in resume.columns {
            if !selected(index) || columns.insert(index, values).is_some() {
                return Err(BlobError::Corrupt);
            }
        }
        let mut partial = BTreeMap::new();
        for held in resume.partials {
            let start = held.index.saturating_mul(self.hop());
            let end = start.saturating_add(self.size());
            if !selected(held.index)
                || held.samples.len() != self.window_size() * 2
                || held.samples.iter().any(|x| !x.is_finite())
                || held.seq >= resume.opened
                || columns.contains_key(&held.index)
                || held
                    .written
                    .iter()
                    .any(|run| run.start < start || run.end > end)
            {
                return Err(BlobError::Corrupt);
            }
            let mut samples = pools.get_with_len::<f32>(held.samples.len())?;
            samples.copy_from_slice(&held.samples);
            if partial
                .insert(
                    held.index,
                    Partial {
                        samples,
                        written: held.written,
                        seq: held.seq,
                    },
                )
                .is_some()
            {
                return Err(BlobError::Corrupt);
            }
        }
        self.columns = columns;
        self.partial = partial;
        self.opened = resume.opened;
        Ok(())
    }

    /// Folds absolute columns by peak, leaving the pass able to accept further
    /// ranges. Each bucket folds `fold` consecutive columns, coarsened only to
    /// fit the format limit. `extent` is the source length in frames when known
    /// and sets the column count, including gaps; uncovered columns read as zero
    /// and are never materialised.
    ///
    /// # Errors
    /// Unrepresentable source coordinates or failed filtering of a final partial window.
    pub fn snapshot(
        &mut self,
        fold: NonZeroU32,
        extent: Option<u64>,
    ) -> Result<Waveform, AnalyzerError> {
        let tail = self.tail_columns(extent)?;
        let total = self.column_count(extent)?;
        let fold = NonZeroU64::from(fold);
        let stride = NonZeroU64::new(fold.get().max(total.div_ceil(MAX_BUCKETS as u64)))
            .ok_or(FilterError::Shape)?;
        let column_frames = u64::try_from(self.column_frames).map_err(|_| FilterError::Shape)?;
        let column_frames = NonZeroU64::new(column_frames).ok_or(FilterError::Shape)?;
        let span_frames = stride
            .checked_mul(column_frames)
            .ok_or(FilterError::Shape)?;
        let count =
            usize::try_from(total.div_ceil(stride.get())).map_err(|_| FilterError::Shape)?;
        let mut out = vec![[0_u8; 8]; count];
        let covered = self
            .columns
            .iter()
            .map(|(&index, columns)| (index, columns))
            .chain(tail.as_ref().map(|(index, columns)| (*index, columns)));
        for (index, columns) in covered {
            let start = body(index)
                .checked_mul(consts::COLUMNS as u64)
                .ok_or(FilterError::Shape)?;
            for (offset, column) in columns.iter().enumerate() {
                let position = start.checked_add(offset as u64).ok_or(FilterError::Shape)?;
                if position >= total {
                    break;
                }
                let bucket =
                    usize::try_from(position / stride.get()).map_err(|_| FilterError::Shape)?;
                let bucket = out.get_mut(bucket).ok_or(FilterError::Shape)?;
                *bucket = array::from_fn(|band| bucket[band].max(column[band]));
            }
        }
        Ok(Waveform::analysed(out, span_frames))
    }

    pub fn write_resume(&self, out: &mut Vec<u8>) {
        let mut writer = Writer::new(out);
        writer.write_len(self.columns.len());
        for (index, columns) in &self.columns {
            writer.write_u64(*index);
            for column in columns {
                writer.write_bytes(column);
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

pub(super) const fn selected(index: u64) -> bool {
    index == 0 || index % 3 == 2
}
pub(super) const fn body(index: u64) -> u64 {
    index / 3 + (index % 3 + 1) / 3
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::num::NonZeroU32;

    use kithara_blob::BlobError;
    use kithara_test_fixtures::analysis_fixtures::{
        analysis_silence, waveform_half, waveform_high, waveform_low, waveform_mid, waveform_mix,
        waveform_tiny, waveform_tone,
    };
    use kithara_test_utils::kithara;
    use rangemap::RangeSet;

    use super::{WaveformAnalyzer, selected};
    use crate::{
        AnalysisParams,
        bucket::Bucket,
        consts,
        resume::{WaveformPartialResume, WaveformResume},
        test_pools::{TestPools, pools},
    };

    struct Pass {
        pools: kithara_bufpool::PoolRegion<TestPools>,
        analyzer: WaveformAnalyzer,
    }

    impl Pass {
        fn new(params: AnalysisParams) -> Self {
            let pools = pools();
            Self {
                analyzer: WaveformAnalyzer::new(consts::SR, params, &pools)
                    .expect("the analyzer builds in the test region"),
                pools,
            }
        }

        fn push(&mut self, pcm: &[f32], channels: usize, at: u64) {
            self.analyzer
                .push(&self.pools, pcm, channels, at)
                .expect("waveform buffers fit the test region");
        }

        fn whole(&mut self, pcm: &[f32], channels: usize, fold: u32) -> Vec<Bucket> {
            self.push(pcm, channels, 0);
            let extent = u64::try_from(pcm.len() / channels).unwrap_or(0);
            self.analyzer
                .snapshot(factor(fold), Some(extent))
                .expect("finite windows")
                .buckets()
                .collect::<Vec<_>>()
        }
    }

    fn factor(fold: u32) -> NonZeroU32 {
        NonZeroU32::new(fold).expect("a test factor is nonzero")
    }

    macro_rules! flat {
        () => {
            AnalysisParams::builder().band_gain([1.0; 4]).build()
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
                columns: self
                    .analyzer
                    .columns
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
            !pass.analyzer.partial.is_empty(),
            "the fixture must leave a partial window to resume"
        );
        pass
    }

    #[kithara::test]
    fn a_restored_pass_snapshots_what_the_stopped_one_would_have(waveform_tone: Vec<f32>) {
        let stopped = interrupted_pass(&waveform_tone);
        let expected = Pass::new(flat!())
            .analyzer
            .snapshot(NonZeroU32::MIN, None)
            .expect("finite windows");
        assert!(expected.is_empty(), "a fresh pass has nothing to show");

        let mut resumed = Pass::new(flat!());
        resumed
            .restore(stopped.resume())
            .expect("the record is its own state");

        let mut stopped = stopped;
        let extent = u64::try_from(waveform_tone.len()).unwrap_or(0);
        assert_eq!(
            resumed
                .analyzer
                .snapshot(NonZeroU32::MIN, Some(extent))
                .expect("finite windows"),
            stopped
                .analyzer
                .snapshot(NonZeroU32::MIN, Some(extent))
                .expect("finite windows"),
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
                    index: u64::try_from(slot).unwrap_or(0) * 3 + u64::from(u32::MAX) + 2,
                    seq: 0,
                })
                .collect();
            resume.opened = u64::try_from(count).unwrap_or(0) + 1;
            resume
        };

        assert!(
            Pass::new(flat!())
                .restore(fill(consts::MAX_PARTIAL))
                .is_ok(),
            "a record holding exactly the limit is still a record"
        );
        assert!(
            matches!(
                Pass::new(flat!()).restore(fill(consts::MAX_PARTIAL + 1)),
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
                resume.columns.push((index, [[0; 8]; 48]));
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
    fn no_frames_snapshots_empty() {
        assert!(
            Pass::new(AnalysisParams::default())
                .analyzer
                .snapshot(NonZeroU32::MIN, None)
                .expect("finite windows")
                .is_empty()
        );
    }

    #[kithara::test]
    fn silence_is_all_zero(analysis_silence: Vec<f32>) {
        let wave = Pass::new(AnalysisParams::default()).whole(&analysis_silence[..16_384], 1, 32);
        assert_eq!(wave.len(), 8);
        for b in &wave {
            assert_eq!(*b, Bucket::default(), "silence -> all-zero bucket: {b:?}");
        }
    }

    #[kithara::test]
    fn fewer_frames_than_a_window_stays_finite(waveform_tiny: Vec<f32>) {
        let wave = Pass::new(AnalysisParams::default()).whole(&waveform_tiny, 1, 1);
        assert_eq!(wave.len(), waveform_tiny.len().div_ceil(64));
        for b in &wave {
            for v in b.left().into_iter().chain(b.right()) {
                assert!(
                    v.is_finite() && (0.0..=1.0).contains(&v),
                    "band must stay finite in [0,1]: {b:?}"
                );
            }
        }
    }

    #[kithara::test]
    fn a_factor_counts_native_columns(waveform_tone: Vec<f32>) {
        // A source span spanning several pre-rolled windows.
        let samples = 4096 + 1024 * 9;
        let pcm = &waveform_tone[..samples];

        assert_eq!(
            Pass::new(flat!()).whole(pcm, 1, 1).len(),
            samples.div_ceil(64),
            "factor 1 is one bucket per native column"
        );
        assert_eq!(
            Pass::new(flat!()).whole(pcm, 1, 4).len(),
            samples.div_ceil(64).div_ceil(4),
            "factor 4 folds four columns per bucket"
        );
    }

    #[kithara::test]
    fn deterministic_for_same_input(waveform_tone: Vec<f32>) {
        let pcm = waveform_tone;
        let run = || Pass::new(AnalysisParams::default()).whole(&pcm, 1, 1);
        assert_eq!(run(), run(), "same PCM must produce the same waveform");
    }

    #[kithara::test]
    fn window_split_across_chunks_matches_unsplit(waveform_tone: Vec<f32>) {
        let pcm = waveform_tone;
        let whole = Pass::new(flat!()).whole(&pcm, 1, 1);

        // Split at 1500 frames: no boundary lands on a window edge, so every
        // window is assembled from two blocks.
        let mut split = Pass::new(flat!());
        for (index, part) in pcm.chunks(1500).enumerate() {
            let at = u64::try_from(index * 1500).unwrap_or(0);
            split.push(part, 1, at);
        }
        let extent = u64::try_from(pcm.len()).unwrap_or(0);
        assert_eq!(
            split
                .analyzer
                .snapshot(NonZeroU32::MIN, Some(extent))
                .expect("finite windows")
                .buckets()
                .collect::<Vec<_>>(),
            whole,
            "a window split across blocks must reduce identically"
        );
    }

    #[kithara::test]
    fn shuffled_and_duplicated_blocks_match_ascending(waveform_tone: Vec<f32>) {
        let pcm = waveform_tone;
        let ascending = Pass::new(flat!()).whole(&pcm, 1, 1);

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
            shuffled
                .analyzer
                .snapshot(NonZeroU32::MIN, Some(extent))
                .expect("finite windows")
                .buckets()
                .collect::<Vec<_>>(),
            ascending,
            "shuffled and duplicated blocks must yield the same waveform"
        );
    }

    #[kithara::test]
    fn a_gap_leaves_its_windows_out_until_it_is_filled(waveform_tone: Vec<f32>) {
        let pcm = waveform_tone;
        let complete = Pass::new(flat!()).whole(&pcm, 1, 1);
        let extent = u64::try_from(pcm.len()).unwrap_or(0);

        let mut gapped = Pass::new(flat!());
        gapped.push(&pcm[..4096], 1, 0);
        gapped.push(&pcm[8192..], 1, 8192);
        let partial = gapped
            .analyzer
            .snapshot(NonZeroU32::MIN, Some(extent))
            .expect("finite windows");
        assert_ne!(
            partial.buckets().collect::<Vec<_>>(),
            complete,
            "an uncovered span must not carry the covered result"
        );

        gapped.push(&pcm[4096..8192], 1, 4096);
        assert_eq!(
            gapped
                .analyzer
                .snapshot(NonZeroU32::MIN, Some(extent))
                .expect("finite windows")
                .buckets()
                .collect::<Vec<_>>(),
            complete,
            "filling the gap must produce the contiguous result"
        );
    }

    #[kithara::test]
    fn partial_windows_are_capped(waveform_half: Vec<f32>) {
        // Isolated single-frame blocks complete no window, so each one only
        // opens the windows that contain it.
        let mut pass = Pass::new(flat!());
        for block in 0..300_u64 {
            pass.push(&waveform_half[..1], 1, block * 100_000);
        }
        assert!(
            pass.analyzer.partial.len() <= 256,
            "live partial windows must stay capped, got {}",
            pass.analyzer.partial.len()
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
        for block in 1..300_u64 {
            pass.push(&waveform_half[..1], 1, block * 100_000);
        }
        pass.push(&pcm[2048..], 1, 2048);

        assert!(
            !pass.analyzer.columns.contains_key(&0),
            "an evicted window must stay absent, not be reduced from half its samples"
        );
    }

    #[kithara::test]
    fn snapshot_leaves_the_pass_usable(waveform_tone: Vec<f32>) {
        let pcm = waveform_tone;
        let extent = u64::try_from(pcm.len()).unwrap_or(0);
        let mut pass = Pass::new(flat!());

        pass.push(&pcm[..8192], 1, 0);
        let early = pass
            .analyzer
            .snapshot(NonZeroU32::MIN, Some(extent))
            .expect("finite windows");
        pass.push(&pcm[8192..], 1, 8192);
        let late = pass
            .analyzer
            .snapshot(NonZeroU32::MIN, Some(extent))
            .expect("finite windows");

        assert_eq!(early.len(), late.len(), "bucket count must not shift");
        assert_ne!(
            early, late,
            "the second snapshot must reflect the added coverage"
        );
        assert_eq!(
            late.buckets().collect::<Vec<_>>(),
            Pass::new(flat!()).whole(&pcm, 1, 1),
            "two snapshots must not change the final result"
        );
    }

    fn peak(bucket: &Bucket) -> f32 {
        bucket.left().into_iter().fold(0.0, f32::max)
    }

    fn dominant(pcm: &[f32]) -> Bucket {
        let fold = u32::try_from(pcm.len().div_ceil(64).div_ceil(4)).expect("fixture fits u32");
        Pass::new(flat!())
            .whole(pcm, 1, fold)
            .into_iter()
            .max_by(|a, b| peak(a).total_cmp(&peak(b)))
            .unwrap_or_default()
    }

    #[kithara::test]
    fn low_frequency_lands_in_low_band(waveform_low: Vec<f32>) {
        let b = dominant(&waveform_low);
        assert!(
            b.left()[0] > b.left()[1].max(b.left()[2]) && b.left()[0] > b.left()[3],
            "80 Hz must be low-dominant: {b:?}"
        );
    }

    #[kithara::test]
    fn mid_frequency_lands_in_mid_band(waveform_mid: Vec<f32>) {
        let b = dominant(&waveform_mid);
        assert!(
            b.left()[1].max(b.left()[2]) > b.left()[0]
                && b.left()[1].max(b.left()[2]) > b.left()[3],
            "1 kHz must be mid-dominant: {b:?}"
        );
    }

    #[kithara::test]
    fn high_frequency_lands_in_high_band(waveform_high: Vec<f32>) {
        let b = dominant(&waveform_high);
        assert!(
            b.left()[3] > b.left()[0] && b.left()[3] > b.left()[1].max(b.left()[2]),
            "10 kHz must be high-dominant: {b:?}"
        );
    }

    #[kithara::test]
    fn full_spectrum_track_has_no_color_gaps(waveform_mix: Vec<f32>) {
        // Regression for a band series coarser than the bucket count, which left
        // columns with no bar. Every column of a full-spectrum track must carry
        let pcm = waveform_mix;
        let wave = Pass::new(AnalysisParams::default()).whole(&pcm, 1, 1);
        let gaps = wave.iter().filter(|bucket| peak(bucket) <= 0.0).count();
        assert_eq!(gaps, 0, "every column must carry a bar");
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
            .columns
            .keys()
            .chain(pass.analyzer.partial.keys())
            .copied()
            .collect();
        let expected: std::collections::BTreeSet<u64> = (0..=end / hop)
            .filter(|index| {
                let start = index * hop;
                selected(*index) && start < end && start + size > at
            })
            .collect();
        assert_eq!(
            reached, expected,
            "a push must reach every window its span overlaps, and no other"
        );
    }
}
