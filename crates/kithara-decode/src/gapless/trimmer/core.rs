use smallvec::SmallVec;

use crate::{GaplessInfo, PcmChunk, duration_for_frames, gapless::heuristic::SilenceTrimParams};

/// Inline batch of chunks released by one `GaplessTrimmer` operation.
pub type GaplessOutput = SmallVec<[PcmChunk; 2]>;
type TailBuffer = SmallVec<[PcmChunk; 4]>;

struct Consts;
impl Consts {
    /// Length of the click-suppression fade-in applied after every
    /// heuristic trim (silence or codec-priming). 3 ms is short
    /// enough to be inaudible as a transient but long enough to mask
    /// the level discontinuity at the trim boundary.
    const FADE_IN_DURATION_MS: u64 = 3;

    /// Length of the click-suppression fade-out applied to the very
    /// end of the buffered audio after a heuristic trailing-silence
    /// trim. Mirror of `Consts::FADE_IN_DURATION_MS` for the trailing side;
    /// same reasoning (mask any sub-sample boundary mismatch left by
    /// the trim search).
    const FADE_OUT_DURATION_MS: u64 = 3;

    /// Window length (in milliseconds) used by the trailing silence
    /// search. Per-sample threshold tests false-positive on zero-
    /// crossings of any periodic signal — at 800 Hz a sine passes
    /// below `1e-3` for ~3 frames every cycle, which the old
    /// algorithm classified as silence and ate into audible content.
    /// A 10 ms window contains many full cycles of typical audio and
    /// integrates over them to get a stable energy estimate; it also
    /// averages out lossy-codec quantisation noise floors (AAC
    /// commonly sits around -50..-60 dB in quiet regions) so a real
    /// silent suffix is recognised reliably.
    const TRAILING_SILENCE_WINDOW_MS: u64 = 10;
}

/// Stateful PCM trimmer that applies one track's gapless contract.
#[derive(Debug, Default)]
pub struct GaplessTrimmer {
    mode: GaplessMode,
    /// Tail hold-back size. Reused for two purposes:
    ///   - in `Fixed` mode it is the metadata-driven trailing trim,
    ///   - in `Heuristic` mode it is `scan_window_frames` so we always
    ///     have enough buffered tail for the EOF silence search.
    ///
    /// The two roles never collide — only one mode is active per
    /// trimmer instance — but watch out when reading the buffer
    /// helpers below: `trailing_frames` does not always mean "frames
    /// to drop", sometimes it just means "minimum buffered tail".
    trailing_frames: u64,
    tail_buffer: TailBuffer,
    tail_buffered_frames: u64,
}

#[derive(Debug, Default)]
enum GaplessMode {
    #[default]
    Disabled,
    Fixed {
        leading_remaining: u64,
        /// Click-suppression fade applied to the first `Consts::FADE_IN_DURATION_MS`
        /// of audio that survives the leading trim. `None` for
        /// metadata-driven trim — that boundary is sample-exact.
        fade_in: Option<FadeInState>,
    },
    Heuristic(Box<HeuristicState>),
}

#[derive(Debug)]
struct HeuristicState {
    params: SilenceTrimParams,
    /// Pre-computed linear amplitude floor — recomputing on every
    /// frame would be wasteful and `params` is immutable for the
    /// lifetime of the trimmer.
    silence_threshold_amp: f32,
    /// Buffered chunks while we look for the first non-silent frame.
    /// Once the search ends, the buffer is drained into `tail_buffer`
    /// (with leading frames trimmed) and never refilled.
    leading_buffer: TailBuffer,
    leading_buffered_frames: u64,
    leading_enabled: bool,
    /// Fade-in applied to the first frames after a successful leading
    /// trim. `None` while we're still buffering or if no trim happened.
    fade_in: Option<FadeInState>,
    /// Same `params.trim_trailing`, copied for fast access in the flush
    /// path so we don't keep matching against the parent enum.
    trim_trailing: bool,
}

impl HeuristicState {
    fn new(params: SilenceTrimParams) -> Self {
        let silence_threshold_amp = params.threshold_amplitude();
        let trim_trailing = params.trim_trailing;
        Self {
            params,
            silence_threshold_amp,
            leading_buffer: TailBuffer::new(),
            leading_buffered_frames: 0,
            leading_enabled: true,
            fade_in: None,
            trim_trailing,
        }
    }
}

/// Raised-cosine fade-in tracker.
///
/// The state is just a counter of how many frames have already been
/// shaped; the curve is generated on demand in [`FadeInState::apply`].
/// The total fade length is in *frames*, not samples — channels are
/// handled by the apply step itself.
#[derive(Debug, Clone, Copy)]
struct FadeInState {
    total_frames: u16,
    applied_frames: u16,
}

impl FadeInState {
    fn for_sample_rate(sample_rate: u32) -> Self {
        let total_frames =
            u64::from(sample_rate.max(1)).saturating_mul(Consts::FADE_IN_DURATION_MS) / 1000;
        let total_frames = u16::try_from(total_frames.clamp(1, 65_535)).unwrap_or(u16::MAX);
        Self {
            total_frames,
            applied_frames: 0,
        }
    }

    /// Returns true once the fade has finished — caller can drop the state.
    fn is_done(self) -> bool {
        self.applied_frames >= self.total_frames
    }

    /// Apply the next slice of the fade to `chunk`, modifying samples
    /// in place. The chunk may be shorter or longer than the remaining
    /// fade window; we only touch the prefix that still needs shaping.
    fn apply(&mut self, chunk: &mut PcmChunk) {
        if self.is_done() {
            return;
        }
        let frames = chunk_frames(chunk);
        if frames == 0 {
            return;
        }
        let channels = usize::from(chunk.spec().channels.max(1));
        let remaining = self.total_frames.saturating_sub(self.applied_frames);
        let to_shape = remaining.min(u16::try_from(frames).unwrap_or(u16::MAX));
        let total = f32::from(self.total_frames.max(1));
        let mut offset = 0;
        while offset < to_shape {
            let frame = self.applied_frames.saturating_add(offset);
            let position = f32::from(frame) / total;
            let gain = 0.5 - 0.5 * (std::f32::consts::PI * position).cos();
            let frame_start = usize::from(offset).saturating_mul(channels);
            let frame_end = frame_start.saturating_add(channels).min(chunk.pcm.len());
            for sample in &mut chunk.pcm[frame_start..frame_end] {
                *sample *= gain;
            }
            offset = offset.saturating_add(1);
        }
        self.applied_frames = self.applied_frames.saturating_add(to_shape);
    }
}

impl GaplessTrimmer {
    /// Build a trimmer driven by decoder-reported metadata. No
    /// fade-in is applied — the decoder's frame counts are exact and
    /// the trimmed boundary lands on a silent sample.
    #[must_use]
    pub fn from_info(info: GaplessInfo) -> Self {
        let enabled = info.leading_frames > 0 || info.trailing_frames > 0;
        Self {
            mode: if enabled {
                GaplessMode::Fixed {
                    leading_remaining: info.leading_frames,
                    fade_in: None,
                }
            } else {
                GaplessMode::Disabled
            },
            trailing_frames: info.trailing_frames,
            tail_buffer: TailBuffer::new(),
            tail_buffered_frames: 0,
        }
    }

    /// Build a trimmer that drops a fixed number of leading frames
    /// looked up from a codec table. The boundary is by definition
    /// approximate, so a short raised-cosine fade-in is applied to
    /// the first frames of audible output to avoid clicks.
    ///
    /// `sample_rate` is needed to size the fade-in in frames.
    #[must_use]
    pub fn codec_priming(leading_frames: u64, sample_rate: u32) -> Self {
        if leading_frames == 0 {
            return Self::disabled();
        }
        Self {
            mode: GaplessMode::Fixed {
                leading_remaining: leading_frames,
                fade_in: Some(FadeInState::for_sample_rate(sample_rate)),
            },
            trailing_frames: 0,
            tail_buffer: TailBuffer::new(),
            tail_buffered_frames: 0,
        }
    }

    /// Build a silence-scan trimmer. Trim boundaries are inferred by
    /// scanning samples; a fade-in is applied after the boundary is
    /// found to mask the level jump.
    #[must_use]
    pub fn silence_trim(params: SilenceTrimParams) -> Self {
        Self {
            trailing_frames: params.scan_window_frames,
            mode: GaplessMode::Heuristic(Box::new(HeuristicState::new(params))),
            tail_buffer: TailBuffer::new(),
            tail_buffered_frames: 0,
        }
    }

    #[must_use]
    pub fn disabled() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn push(&mut self, chunk: PcmChunk) -> GaplessOutput {
        match &mut self.mode {
            GaplessMode::Disabled => output_with(chunk),
            GaplessMode::Fixed {
                leading_remaining,
                fade_in,
            } => {
                let Some(mut chunk) = trim_leading(chunk, leading_remaining) else {
                    return SmallVec::new();
                };
                apply_fade_in(fade_in, &mut chunk);

                buffer_tail(&mut self.tail_buffer, &mut self.tail_buffered_frames, chunk);
                release_ready_chunks(
                    &mut self.tail_buffer,
                    &mut self.tail_buffered_frames,
                    self.trailing_frames,
                )
            }
            GaplessMode::Heuristic(state) => push_heuristic(
                state,
                &mut self.tail_buffer,
                &mut self.tail_buffered_frames,
                self.trailing_frames,
                chunk,
            ),
        }
    }

    #[must_use]
    pub fn flush(&mut self) -> GaplessOutput {
        match &mut self.mode {
            GaplessMode::Disabled => GaplessOutput::new(),
            GaplessMode::Fixed { .. } => {
                trim_tail_frames(
                    &mut self.tail_buffer,
                    &mut self.tail_buffered_frames,
                    self.trailing_frames,
                );
                drain_tail(&mut self.tail_buffer, &mut self.tail_buffered_frames)
            }
            GaplessMode::Heuristic(state) => flush_heuristic(
                state,
                &mut self.tail_buffer,
                &mut self.tail_buffered_frames,
                self.trailing_frames,
            ),
        }
    }

    /// Drop seek-sensitive state. Both heuristic search and pending
    /// fade-in are abandoned: after a seek we land mid-track and
    /// trying to "trim leading silence" or apply a fade-in there
    /// would corrupt audible content.
    pub fn notify_seek(&mut self) {
        match &mut self.mode {
            GaplessMode::Disabled => {}
            GaplessMode::Fixed {
                leading_remaining,
                fade_in,
            } => {
                *leading_remaining = 0;
                *fade_in = None;
            }
            GaplessMode::Heuristic(state) => {
                state.leading_buffer.clear();
                state.leading_buffered_frames = 0;
                state.leading_enabled = false;
                state.fade_in = None;
            }
        }
        clear_tail_buffer(&mut self.tail_buffer, &mut self.tail_buffered_frames);
    }
}

fn push_heuristic(
    state: &mut HeuristicState,
    tail_buffer: &mut TailBuffer,
    tail_buffered_frames: &mut u64,
    trailing_frames: u64,
    chunk: PcmChunk,
) -> GaplessOutput {
    if !state.leading_enabled {
        return forward_post_leading(
            state,
            tail_buffer,
            tail_buffered_frames,
            trailing_frames,
            chunk,
        );
    }

    state.leading_buffered_frames = state
        .leading_buffered_frames
        .saturating_add(chunk_frames(&chunk));
    state.leading_buffer.push(chunk);

    if let Some(trim_frames) = find_leading_trim_frames(
        &state.leading_buffer,
        &state.params,
        state.silence_threshold_amp,
    ) {
        state.leading_enabled = false;
        if trim_frames > 0 {
            arm_fade_in(state);
        }
        return drain_leading_buffer(
            state,
            tail_buffer,
            tail_buffered_frames,
            trailing_frames,
            trim_frames,
        );
    }

    if state.leading_buffered_frames >= state.params.scan_window_frames {
        state.leading_enabled = false;
        return drain_leading_buffer(state, tail_buffer, tail_buffered_frames, trailing_frames, 0);
    }

    GaplessOutput::new()
}

fn forward_post_leading(
    state: &mut HeuristicState,
    tail_buffer: &mut TailBuffer,
    tail_buffered_frames: &mut u64,
    trailing_frames: u64,
    mut chunk: PcmChunk,
) -> GaplessOutput {
    apply_fade_in(&mut state.fade_in, &mut chunk);
    buffer_tail(tail_buffer, tail_buffered_frames, chunk);
    release_ready_chunks(tail_buffer, tail_buffered_frames, trailing_frames)
}

fn flush_heuristic(
    state: &mut HeuristicState,
    tail_buffer: &mut TailBuffer,
    tail_buffered_frames: &mut u64,
    trailing_frames: u64,
) -> GaplessOutput {
    let mut ready = GaplessOutput::new();

    if state.leading_enabled {
        let trim_frames = find_leading_trim_frames(
            &state.leading_buffer,
            &state.params,
            state.silence_threshold_amp,
        )
        .unwrap_or(0);
        state.leading_enabled = false;
        if trim_frames > 0 {
            arm_fade_in(state);
        }
        ready.extend(drain_leading_buffer(
            state,
            tail_buffer,
            tail_buffered_frames,
            trailing_frames,
            trim_frames,
        ));
    }

    if state.trim_trailing {
        let silent_suffix = trailing_silent_frames(tail_buffer, state.silence_threshold_amp);
        if silent_suffix > 0
            && silent_suffix < *tail_buffered_frames
            && silent_suffix >= state.params.min_trim_frames
        {
            trim_tail_frames(tail_buffer, tail_buffered_frames, silent_suffix);
            let sample_rate = tail_buffer
                .last()
                .map_or(0, |chunk| chunk.spec().sample_rate);
            apply_trailing_fade_out(tail_buffer, sample_rate);
        }
    }

    ready.extend(drain_tail(tail_buffer, tail_buffered_frames));
    ready
}

/// Apply a raised-cosine fade-out to the last `Consts::FADE_OUT_DURATION_MS`
/// of audio buffered in `tail_buffer`. Modifies samples in place; if
/// fewer frames are buffered than the fade window, the entire tail is
/// shaped (gain still goes from 1.0 down to ~0.0 across whatever is
/// available).
fn apply_trailing_fade_out(tail_buffer: &mut TailBuffer, sample_rate: u32) {
    use num_traits::AsPrimitive;

    if tail_buffer.is_empty() {
        return;
    }
    let total_frames =
        u64::from(sample_rate.max(1)).saturating_mul(Consts::FADE_OUT_DURATION_MS) / 1000;
    let total_frames = total_frames.max(1);

    let mut frames_remaining = total_frames;
    for chunk in tail_buffer.iter_mut().rev() {
        if frames_remaining == 0 {
            break;
        }
        let chunk_total_frames = chunk_frames(chunk);
        let channels = usize::from(chunk.spec().channels.max(1));
        let to_shape = chunk_total_frames.min(frames_remaining);
        if to_shape == 0 {
            continue;
        }
        let first_to_shape = chunk_total_frames.saturating_sub(to_shape);
        let already_shaped_after_this = total_frames.saturating_sub(frames_remaining);
        let denom: f32 = total_frames.saturating_sub(1).max(1).as_();

        for chunk_local_frame in first_to_shape..chunk_total_frames {
            let distance_from_end = chunk_total_frames
                .saturating_sub(1)
                .saturating_sub(chunk_local_frame)
                .saturating_add(already_shaped_after_this);
            let frame_in_fade = total_frames
                .saturating_sub(1)
                .saturating_sub(distance_from_end);
            let frame_in_fade_f32: f32 = frame_in_fade.as_();
            let position = frame_in_fade_f32 / denom;
            let gain = 0.5 + 0.5 * (std::f32::consts::PI * position).cos();
            let frame_start = usize_from_u64_saturating(chunk_local_frame).saturating_mul(channels);
            let frame_end = frame_start.saturating_add(channels).min(chunk.pcm.len());
            for sample in &mut chunk.pcm[frame_start..frame_end] {
                *sample *= gain;
            }
        }

        frames_remaining = frames_remaining.saturating_sub(to_shape);
    }
}

fn arm_fade_in(state: &mut HeuristicState) {
    let sample_rate = state
        .leading_buffer
        .first()
        .map_or(0, |chunk| chunk.spec().sample_rate);
    state.fade_in = Some(FadeInState::for_sample_rate(sample_rate));
}

fn apply_fade_in(fade: &mut Option<FadeInState>, chunk: &mut PcmChunk) {
    let Some(state) = fade.as_mut() else {
        return;
    };
    state.apply(chunk);
    if state.is_done() {
        *fade = None;
    }
}

fn trim_leading(mut chunk: PcmChunk, leading_remaining: &mut u64) -> Option<PcmChunk> {
    if *leading_remaining > 0 {
        let chunk_frames = chunk_frames(&chunk);
        if chunk_frames <= *leading_remaining {
            *leading_remaining -= chunk_frames;
            return None;
        }

        let trim_frames = usize_from_u64_saturating(*leading_remaining);
        *leading_remaining = 0;
        trim_chunk_start(&mut chunk, trim_frames);
    }

    (chunk_frames(&chunk) > 0).then_some(chunk)
}

fn drain_leading_buffer(
    state: &mut HeuristicState,
    tail_buffer: &mut TailBuffer,
    tail_buffered_frames: &mut u64,
    trailing_frames: u64,
    trim_frames: u64,
) -> GaplessOutput {
    let mut buffer = std::mem::take(&mut state.leading_buffer);
    state.leading_buffered_frames = 0;

    let mut remaining_trim = trim_frames;
    for mut chunk in buffer.drain(..) {
        if remaining_trim > 0 {
            let chunk_frames = chunk_frames(&chunk);
            if chunk_frames <= remaining_trim {
                remaining_trim -= chunk_frames;
                continue;
            }

            let trim = usize_from_u64_saturating(remaining_trim);
            remaining_trim = 0;
            trim_chunk_start(&mut chunk, trim);
        }

        apply_fade_in(&mut state.fade_in, &mut chunk);
        buffer_tail(tail_buffer, tail_buffered_frames, chunk);
    }

    release_ready_chunks(tail_buffer, tail_buffered_frames, trailing_frames)
}

fn buffer_tail(tail_buffer: &mut TailBuffer, tail_buffered_frames: &mut u64, chunk: PcmChunk) {
    *tail_buffered_frames = (*tail_buffered_frames).saturating_add(chunk_frames(&chunk));
    tail_buffer.push(chunk);
}

fn release_ready_chunks(
    tail_buffer: &mut TailBuffer,
    tail_buffered_frames: &mut u64,
    trailing_frames: u64,
) -> GaplessOutput {
    let mut ready = GaplessOutput::new();
    while can_release_front(tail_buffer, *tail_buffered_frames, trailing_frames) {
        if let Some(chunk) = pop_front_chunk(tail_buffer, tail_buffered_frames) {
            ready.push(chunk);
        }
    }
    ready
}

fn can_release_front(
    tail_buffer: &TailBuffer,
    tail_buffered_frames: u64,
    trailing_frames: u64,
) -> bool {
    let Some(front) = tail_buffer.first() else {
        return false;
    };

    tail_buffered_frames.saturating_sub(chunk_frames(front)) >= trailing_frames
}

fn trim_tail_frames(
    tail_buffer: &mut TailBuffer,
    tail_buffered_frames: &mut u64,
    trim_frames: u64,
) {
    let mut drop_frames = trim_frames.min(*tail_buffered_frames);
    while drop_frames > 0 {
        let Some(back) = tail_buffer.last_mut() else {
            break;
        };

        let back_frames = chunk_frames(back);
        if back_frames <= drop_frames {
            drop_frames -= back_frames;
            *tail_buffered_frames = (*tail_buffered_frames).saturating_sub(back_frames);
            tail_buffer.pop();
            continue;
        }

        trim_chunk_end(back, drop_frames);
        *tail_buffered_frames = (*tail_buffered_frames).saturating_sub(drop_frames);
        drop_frames = 0;
    }
}

/// Walk frames from the end of `tail_buffer`, group them into
/// `Consts::TRAILING_SILENCE_WINDOW_MS` windows, and count frames as silent
/// while window-mean-|sample| stays below `threshold_amp`. Returns the
/// largest tail length whose energy is still below the floor.
///
/// Per-sample testing (the original implementation) misclassifies
/// zero-crossings of any periodic signal as silence: AAC quantisation
/// noise around a ZCR can dip below `1e-3` for a handful of frames at
/// every cycle. Integrating over a few-millisecond window prevents
/// the search from chewing into audible content via those gaps.
///
/// Mean-|sample| (rather than RMS) is used because it is less peak-
/// sensitive: for an audible sine its mean-abs is ≈0.6 of peak, while
/// for a noisy quiet region it tracks the average linear amplitude.
/// This widens the gap between "real" audio and codec quantisation
/// noise, making the threshold easier to pick.
fn trailing_silent_frames(tail_buffer: &TailBuffer, threshold_amp: f32) -> u64 {
    use num_traits::AsPrimitive;

    if tail_buffer.is_empty() {
        return 0;
    }

    let sample_rate = tail_buffer
        .first()
        .map_or(48_000, |chunk| chunk.spec().sample_rate)
        .max(1);
    let window_frames =
        (u64::from(sample_rate).saturating_mul(Consts::TRAILING_SILENCE_WINDOW_MS) / 1000).max(1);
    let threshold = f64::from(threshold_amp);

    let mut silent_frames = 0u64;
    let mut window_sum_abs = 0.0_f64;
    let mut window_count: u64 = 0;

    for chunk in tail_buffer.iter().rev() {
        let chunk_total_frames = chunk_frames(chunk);
        let samples = chunk.samples();
        let channels = usize::from(chunk.spec().channels.max(1));
        let channels_f64: f64 = channels.max(1).as_();
        for frame in (0..chunk_total_frames).rev() {
            let frame_start = usize_from_u64_saturating(frame).saturating_mul(channels);
            let frame_end = frame_start.saturating_add(channels).min(samples.len());
            if frame_end <= frame_start {
                continue;
            }
            let mut frame_sum_abs = 0.0_f64;
            for &sample in &samples[frame_start..frame_end] {
                frame_sum_abs += f64::from(sample.abs());
            }
            let frame_mean_abs = frame_sum_abs / channels_f64;
            window_sum_abs += frame_mean_abs;
            window_count = window_count.saturating_add(1);

            if window_count >= window_frames {
                let window_count_f64: f64 = window_count.as_();
                let mean_abs = window_sum_abs / window_count_f64;
                if mean_abs <= threshold {
                    silent_frames = silent_frames.saturating_add(window_count);
                    window_sum_abs = 0.0;
                    window_count = 0;
                } else {
                    return silent_frames;
                }
            }
        }
    }

    if window_count > 0 {
        let window_count_f64: f64 = window_count.as_();
        let mean_abs = window_sum_abs / window_count_f64;
        if mean_abs <= threshold {
            silent_frames = silent_frames.saturating_add(window_count);
        }
    }

    silent_frames
}

/// Find the first non-silent frame in the buffered leading audio.
///
/// Returns:
/// - `Some(n)` — the boundary is at frame `n` (counting silent
///   frames seen so far) AND `n >= params.min_trim_frames`. Caller
///   can drop `n` frames safely.
/// - `None` — either no boundary was found within
///   `params.scan_window_frames` (the audio looks like one long
///   fade-in and we choose to leave it alone), or a boundary was
///   found but with too few preceding silent frames to be considered
///   trim-worthy.
fn find_leading_trim_frames(
    buffer: &[PcmChunk],
    params: &SilenceTrimParams,
    threshold_amp: f32,
) -> Option<u64> {
    let mut scanned_frames = 0_u64;
    let mut trim_frames = 0_u64;

    for chunk in buffer {
        let chunk_frames = chunk_frames(chunk);
        let samples = chunk.samples();
        let channels = usize::from(chunk.spec().channels.max(1));
        for frame in 0..chunk_frames {
            if scanned_frames >= params.scan_window_frames {
                return None;
            }

            let frame_start = usize_from_u64_saturating(frame).saturating_mul(channels);
            let frame_end = frame_start.saturating_add(channels).min(samples.len());
            if frame_end <= frame_start {
                scanned_frames = scanned_frames.saturating_add(1);
                trim_frames = trim_frames.saturating_add(1);
                continue;
            }

            if !frame_is_silent(&samples[frame_start..frame_end], threshold_amp) {
                return (trim_frames >= params.min_trim_frames).then_some(trim_frames);
            }

            scanned_frames = scanned_frames.saturating_add(1);
            trim_frames = trim_frames.saturating_add(1);
        }
    }

    None
}

fn drain_tail(tail_buffer: &mut TailBuffer, tail_buffered_frames: &mut u64) -> GaplessOutput {
    let mut ready = GaplessOutput::new();
    while let Some(chunk) = pop_front_chunk(tail_buffer, tail_buffered_frames) {
        ready.push(chunk);
    }
    ready
}

fn clear_tail_buffer(tail_buffer: &mut TailBuffer, tail_buffered_frames: &mut u64) {
    tail_buffer.clear();
    *tail_buffered_frames = 0;
}

fn pop_front_chunk(
    tail_buffer: &mut TailBuffer,
    tail_buffered_frames: &mut u64,
) -> Option<PcmChunk> {
    if tail_buffer.is_empty() {
        return None;
    }

    let chunk = tail_buffer.remove(0);
    *tail_buffered_frames = tail_buffered_frames.saturating_sub(chunk_frames(&chunk));
    Some(chunk)
}

fn frame_is_silent(samples: &[f32], threshold_amp: f32) -> bool {
    samples.iter().all(|sample| sample.abs() <= threshold_amp)
}

fn trim_chunk_start(chunk: &mut PcmChunk, trim_frames: usize) {
    let spec = chunk.spec();
    let channels = usize::from(spec.channels.max(1));
    let trim_samples = trim_frames.saturating_mul(channels);
    let len = chunk.pcm.len();
    chunk.pcm.copy_within(trim_samples..len, 0);
    chunk.pcm.truncate(len.saturating_sub(trim_samples));
    chunk.meta.frame_offset = chunk.meta.frame_offset.saturating_add(trim_frames as u64);
    chunk.meta.frames = u32::try_from(chunk.pcm.len() / channels.max(1)).unwrap_or(u32::MAX);
    chunk.meta.timestamp = chunk
        .meta
        .timestamp
        .saturating_add(duration_for_frames(spec.sample_rate, trim_frames as u64));
}

fn trim_chunk_end(chunk: &mut PcmChunk, trim_frames: u64) {
    let channels = usize::from(chunk.spec().channels.max(1));
    let keep_frames = usize_from_u64_saturating(chunk_frames(chunk).saturating_sub(trim_frames));
    let keep_samples = keep_frames.saturating_mul(channels);
    chunk.pcm.truncate(keep_samples);
    chunk.meta.frames = u32::try_from(keep_frames).unwrap_or(u32::MAX);
}

fn output_with(chunk: PcmChunk) -> GaplessOutput {
    let mut ready = GaplessOutput::new();
    ready.push(chunk);
    ready
}

fn chunk_frames(chunk: &PcmChunk) -> u64 {
    u64::try_from(chunk.frames()).unwrap_or(u64::MAX)
}

fn usize_from_u64_saturating(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
