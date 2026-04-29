use smallvec::SmallVec;

use crate::{GaplessInfo, PcmChunk, duration_for_frames, gapless::heuristic::SilenceTrimParams};

/// Inline batch of chunks released by one `GaplessTrimmer` operation.
pub type GaplessOutput = SmallVec<[PcmChunk; 2]>;
type TailBuffer = SmallVec<[PcmChunk; 4]>;

/// Length of the click-suppression fade-in applied after every
/// heuristic trim (silence or codec-priming). 3 ms is short enough to
/// be inaudible as a transient but long enough to mask the level
/// discontinuity at the trim boundary.
const FADE_IN_DURATION_MS: u64 = 3;

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
        /// Click-suppression fade applied to the first `FADE_IN_DURATION_MS`
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
        // sample_rate * ms / 1000, rounded up to at least one frame
        // so a misconfigured 0 Hz spec still produces a valid fade.
        let total_frames = u64::from(sample_rate.max(1)).saturating_mul(FADE_IN_DURATION_MS) / 1000;
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
        // Number of frames in this chunk that still need shaping —
        // anything past `total_frames - applied_frames` is at full
        // amplitude already.
        let remaining = self.total_frames.saturating_sub(self.applied_frames);
        let to_shape = remaining.min(u16::try_from(frames).unwrap_or(u16::MAX));
        let total = f32::from(self.total_frames.max(1));
        let mut offset = 0;
        while offset < to_shape {
            // Raised-cosine ramp: smooth at both endpoints, no DC step
            // and no derivative discontinuity at the fade end.
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
            // The tail buffer must always have at least
            // `scan_window_frames` worth of audio held back so the
            // EOF flush has enough material to scan for trailing
            // silence. Even when `trim_trailing == false` we keep the
            // hold-back for code uniformity — the cost is bounded by
            // `scan_window_frames`.
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
        // Past the leading scan: stream goes straight through the tail
        // buffer (with fade-in if one is pending from a successful trim).
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

    // Try to find the silence/content boundary in everything we've
    // buffered so far. `find_leading_trim_frames` returns:
    //   - Some(n) — we found the first non-silent frame at offset n,
    //     and n meets `min_trim_frames`. Drain the buffer trimming n
    //     frames and arm the fade-in.
    //   - None — either the boundary hasn't been seen yet (keep
    //     buffering) or the candidate was below `min_trim_frames`.
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

    // Bail out once we've buffered the whole scan window. We saw no
    // clear silence-to-content boundary, so we leave the audio alone.
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

    // EOF before the leading scan finished: do one more pass on what
    // we have. If we still don't see the boundary, give up and emit
    // everything (drain with trim_frames=0).
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
        // Three guards on the trailing trim:
        //   1. silent_suffix > 0 — there's something to drop;
        //   2. < total buffered  — never wipe the entire buffer (an
        //      all-silent track stays as it is, same as the leading
        //      side);
        //   3. >= min_trim_frames — protects intentional micro-decay.
        if silent_suffix > 0
            && silent_suffix < *tail_buffered_frames
            && silent_suffix >= state.params.min_trim_frames
        {
            trim_tail_frames(tail_buffer, tail_buffered_frames, silent_suffix);
        }
    }

    ready.extend(drain_tail(tail_buffer, tail_buffered_frames));
    ready
}

fn arm_fade_in(state: &mut HeuristicState) {
    // Pull the sample rate from any chunk currently buffered. The
    // leading buffer always has at least one chunk when we get here
    // (the trim only succeeds after at least one push).
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

    // Walk the buffered chunks and drop `trim_frames` total leading
    // frames, possibly across multiple chunks. Apply the fade-in to
    // surviving frames before they hit the tail buffer — fading them
    // here keeps the buffer ordering stable.
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

    // Keep enough buffered tail frames so `flush()` can trim EOF padding later.
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

fn trailing_silent_frames(tail_buffer: &TailBuffer, threshold_amp: f32) -> u64 {
    let mut silent_frames = 0;
    for chunk in tail_buffer.iter().rev() {
        let chunk_frames = chunk_frames(chunk);
        let samples = chunk.samples();
        let channels = usize::from(chunk.spec().channels.max(1));
        for frame in (0..chunk_frames).rev() {
            let frame_start = usize_from_u64_saturating(frame).saturating_mul(channels);
            let frame_end = frame_start.saturating_add(channels).min(samples.len());
            if frame_end <= frame_start {
                continue;
            }
            if !frame_is_silent(&samples[frame_start..frame_end], threshold_amp) {
                return silent_frames;
            }
            silent_frames = silent_frames.saturating_add(1);
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
    // This is an in-place shift, not a new allocation, but it still copies the
    // retained PCM tail when the leading trim boundary lands inside this chunk.
    chunk.pcm.copy_within(trim_samples..len, 0);
    chunk.pcm.truncate(len.saturating_sub(trim_samples));
    chunk.meta.frame_offset = chunk.meta.frame_offset.saturating_add(trim_frames as u64);
    // Leading trim changes the logical start time seen downstream.
    chunk.meta.timestamp = chunk
        .meta
        .timestamp
        .saturating_add(duration_for_frames(spec.sample_rate, trim_frames as u64));
}

fn trim_chunk_end(chunk: &mut PcmChunk, trim_frames: u64) {
    let keep_frames = usize_from_u64_saturating(chunk_frames(chunk).saturating_sub(trim_frames));
    let keep_samples = keep_frames.saturating_mul(usize::from(chunk.spec().channels.max(1)));
    // EOF trim can cut only the final chunk; earlier chunks stay frame-aligned as-is.
    chunk.pcm.truncate(keep_samples);
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
mod tests {
    use std::time::Duration;

    use kithara_bufpool::pcm_pool;
    use kithara_test_utils::kithara;

    use crate::{GaplessInfo, PcmChunk, PcmMeta, PcmSpec, gapless::heuristic::SilenceTrimParams};

    use super::{FADE_IN_DURATION_MS, GaplessTrimmer};

    fn chunk(spec: PcmSpec, frame_offset: u64, frames: usize) -> PcmChunk {
        let samples = frames.saturating_mul(usize::from(spec.channels));
        let pcm = (0..samples)
            .map(|idx| f32::from(u16::try_from(idx).expect("test sample fits in u16")))
            .collect::<Vec<_>>();
        PcmChunk::new(
            PcmMeta {
                spec,
                frame_offset,
                ..Default::default()
            },
            pcm_pool().attach(pcm),
        )
    }

    fn silent_chunk(spec: PcmSpec, frame_offset: u64, frames: usize) -> PcmChunk {
        let samples = frames.saturating_mul(usize::from(spec.channels));
        PcmChunk::new(
            PcmMeta {
                spec,
                frame_offset,
                ..Default::default()
            },
            pcm_pool().attach(vec![0.0; samples]),
        )
    }

    fn custom_chunk(spec: PcmSpec, frame_offset: u64, pcm: Vec<f32>) -> PcmChunk {
        PcmChunk::new(
            PcmMeta {
                spec,
                frame_offset,
                ..Default::default()
            },
            pcm_pool().attach(pcm),
        )
    }

    fn mono_spec() -> PcmSpec {
        PcmSpec {
            channels: 1,
            sample_rate: 48_000,
        }
    }

    fn stereo_spec() -> PcmSpec {
        PcmSpec {
            channels: 2,
            sample_rate: 48_000,
        }
    }

    fn fade_frames_for(spec: PcmSpec) -> usize {
        // Mirrors FadeInState::for_sample_rate; cast safe for typical
        // sample rates well below usize::MAX.
        let computed = (u64::from(spec.sample_rate.max(1)) * FADE_IN_DURATION_MS) / 1000;
        computed.max(1) as usize
    }

    fn silence_params(threshold_db: f32, min_trim_frames: u64) -> SilenceTrimParams {
        SilenceTrimParams {
            threshold_db,
            min_trim_frames,
            scan_window_frames: 4096,
            trim_trailing: false,
        }
    }

    fn collect_pcm(out: &[PcmChunk]) -> Vec<f32> {
        out.iter().flat_map(|c| c.samples().to_vec()).collect()
    }

    // ---- Existing metadata-driven contract (unchanged behaviour) ----

    #[kithara::test]
    fn leading_trim_updates_offset_and_timestamp() {
        let spec = mono_spec();
        let mut trimmer = GaplessTrimmer::from_info(GaplessInfo {
            leading_frames: 576,
            trailing_frames: 0,
        });

        let mut ready = trimmer.push(chunk(spec, 0, 1024));
        assert_eq!(ready.len(), 1);
        let out = ready.remove(0);
        assert_eq!(out.frames(), 448);
        assert_eq!(out.meta.frame_offset, 576);
        assert_eq!(out.meta.timestamp, Duration::from_millis(12));
        // Metadata-driven trim does NOT apply a fade-in — the boundary
        // is sample-exact, so the first surviving sample stays as-is.
        assert_eq!(out.samples()[0], 576.0);
    }

    #[kithara::test]
    fn leading_trim_can_consume_multiple_chunks() {
        let spec = mono_spec();
        let mut trimmer = GaplessTrimmer::from_info(GaplessInfo {
            leading_frames: 2400,
            trailing_frames: 0,
        });

        assert!(trimmer.push(chunk(spec, 0, 1024)).is_empty());
        assert!(trimmer.push(chunk(spec, 1024, 1024)).is_empty());

        let mut ready = trimmer.push(chunk(spec, 2048, 1024));
        assert_eq!(ready.len(), 1);
        let out = ready.remove(0);
        assert_eq!(out.frames(), 672);
        assert_eq!(out.meta.frame_offset, 2400);
        assert_eq!(out.samples()[0], 352.0);
    }

    #[kithara::test]
    fn trailing_trim_buffers_until_flush() {
        let spec = mono_spec();
        let mut trimmer = GaplessTrimmer::from_info(GaplessInfo {
            leading_frames: 0,
            trailing_frames: 64,
        });

        assert!(trimmer.push(chunk(spec, 0, 32)).is_empty());

        let mut ready = trimmer.push(chunk(spec, 32, 64));
        assert_eq!(ready.len(), 1);
        assert_eq!(ready.remove(0).frames(), 32);

        let ready = trimmer.flush();
        assert!(ready.is_empty());
    }

    #[kithara::test]
    fn trailing_trim_drops_tail_on_flush() {
        let spec = mono_spec();
        let mut trimmer = GaplessTrimmer::from_info(GaplessInfo {
            leading_frames: 0,
            trailing_frames: 2_048,
        });

        assert!(trimmer.push(chunk(spec, 0, 1_024)).is_empty());
        assert!(trimmer.push(chunk(spec, 1_024, 1_024)).is_empty());
        assert_eq!(trimmer.push(chunk(spec, 2_048, 1_024)).len(), 1);

        let ready = trimmer.flush();
        assert!(ready.is_empty());
    }

    #[kithara::test]
    fn trailing_trim_handles_more_than_inline_tail_chunks() {
        let spec = mono_spec();
        let mut trimmer = GaplessTrimmer::from_info(GaplessInfo {
            leading_frames: 0,
            trailing_frames: 8,
        });
        let mut output = super::GaplessOutput::new();

        for frame in 0..8 {
            assert!(trimmer.push(chunk(spec, frame, 1)).is_empty());
        }

        output.extend(trimmer.push(chunk(spec, 8, 1)));
        output.extend(trimmer.flush());

        assert_eq!(output.len(), 1);
        let out = output.remove(0);
        assert_eq!(out.meta.frame_offset, 0);
        assert_eq!(out.frames(), 1);
    }

    #[kithara::test]
    fn disabled_trimmer_passes_through() {
        let spec = mono_spec();
        let mut trimmer = GaplessTrimmer::disabled();
        let mut ready = trimmer.push(chunk(spec, 0, 128));
        assert_eq!(ready.len(), 1);
        assert_eq!(ready.remove(0).frames(), 128);
        assert!(trimmer.flush().is_empty());
    }

    #[kithara::test]
    fn notify_seek_resets_leading_only() {
        let spec = mono_spec();
        let mut trimmer = GaplessTrimmer::from_info(GaplessInfo {
            leading_frames: 128,
            trailing_frames: 64,
        });

        assert!(trimmer.push(chunk(spec, 0, 64)).is_empty());
        trimmer.notify_seek();

        assert!(trimmer.push(chunk(spec, 64, 128)).is_empty());

        let mut ready = trimmer.flush();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready.remove(0).frames(), 64);
    }

    // ---- Codec-priming mode ----

    #[kithara::test]
    fn codec_priming_with_zero_frames_is_disabled() {
        let spec = mono_spec();
        let mut trimmer = GaplessTrimmer::codec_priming(0, spec.sample_rate);
        let mut ready = trimmer.push(chunk(spec, 0, 64));
        assert_eq!(ready.len(), 1);
        assert_eq!(ready.remove(0).frames(), 64);
    }

    #[kithara::test]
    fn codec_priming_drops_leading_frames_and_fades_in() {
        let spec = mono_spec();
        // Use a synthetic constant signal so the fade-in shape is
        // easy to inspect: every sample is 1.0 prior to fading.
        let trim = 100u64;
        let trim_len = usize::try_from(trim).expect("test trim fits in usize");
        let total_frames = trim_len + fade_frames_for(spec) + 32;
        let pcm = vec![1.0_f32; total_frames];
        let mut trimmer = GaplessTrimmer::codec_priming(trim, spec.sample_rate);

        let ready = trimmer.push(custom_chunk(spec, 0, pcm));
        let pcm_out = collect_pcm(&ready);
        assert_eq!(pcm_out.len(), total_frames - trim_len);

        // Anti-click guarantees we want for downstream:
        //   - The first surviving frame is at very low amplitude (no
        //     hard step from 0 to full level).
        //   - Amplitude grows monotonically through the fade window.
        //   - After the fade window we are back at full level.
        let fade_len = fade_frames_for(spec);
        assert!(
            pcm_out[0].abs() < 0.05,
            "first sample {} not soft",
            pcm_out[0]
        );
        for window in pcm_out[..fade_len].windows(2) {
            assert!(
                window[1] >= window[0] - 1e-6,
                "fade-in must be monotonically increasing: {:?}",
                window
            );
        }
        for &sample in &pcm_out[fade_len..] {
            assert!(
                (sample - 1.0).abs() < 1e-5,
                "post-fade sample {sample} != 1.0"
            );
        }
    }

    #[kithara::test]
    fn codec_priming_metadata_takes_precedence_when_combined() {
        // `from_info` is the metadata path; codec_priming is only the
        // fallback. The pipeline picks one or the other — but verify
        // that calling both constructors does not double-trim by
        // accident through some shared field.
        let spec = mono_spec();
        let metadata_trimmer = GaplessTrimmer::from_info(GaplessInfo {
            leading_frames: 50,
            trailing_frames: 0,
        });
        let codec_trimmer = GaplessTrimmer::codec_priming(50, spec.sample_rate);
        let pcm = vec![0.5_f32; 200];

        let mut from_info = metadata_trimmer;
        let from_info_out = collect_pcm(&from_info.push(custom_chunk(spec, 0, pcm.clone())));
        // No fade-in: first surviving sample is intact.
        assert_eq!(from_info_out[0], 0.5);

        let mut from_codec = codec_trimmer;
        let from_codec_out = collect_pcm(&from_codec.push(custom_chunk(spec, 0, pcm)));
        // Fade-in active: first surviving sample is much smaller.
        assert!(from_codec_out[0].abs() < 0.5 * 0.1);
    }

    // ---- Silence-trim mode ----

    #[kithara::test]
    fn silence_trim_below_threshold_is_trimmed() {
        let spec = mono_spec();
        let mut trimmer = GaplessTrimmer::silence_trim(silence_params(60.0, 32));

        // 300 leading frames at -70 dB (below the -60 dB threshold) +
        // 100 frames at full amplitude.
        let mut pcm = vec![0.0003_f32; 300];
        pcm.extend(std::iter::repeat_n(0.5, 100));
        assert!(trimmer.push(custom_chunk(spec, 0, pcm)).is_empty());

        let flushed = trimmer.flush();
        let pcm_out = collect_pcm(&flushed);
        // We expect the 300 quiet frames gone, leaving 100 frames
        // (with a fade-in shaping the first ~144 of them).
        assert_eq!(pcm_out.len(), 100);
        assert!(pcm_out[0].abs() < 0.05, "fade-in must soften the boundary");
    }

    #[kithara::test]
    fn silence_trim_above_threshold_preserves_audio() {
        let spec = mono_spec();
        let mut trimmer = GaplessTrimmer::silence_trim(silence_params(60.0, 32));

        // 300 frames at -50 dB (~3.16e-3) — above the -60 dB threshold.
        // This is a quiet but real signal, and must not be trimmed.
        let pcm = vec![3.16e-3_f32; 300];
        assert!(trimmer.push(custom_chunk(spec, 0, pcm)).is_empty());

        let flushed = trimmer.flush();
        let pcm_out = collect_pcm(&flushed);
        assert_eq!(pcm_out.len(), 300);
        // No fade-in applied — original samples should be intact.
        assert!(pcm_out.iter().all(|s| (*s - 3.16e-3).abs() < 1e-7));
    }

    #[kithara::test]
    fn silence_trim_preserves_quiet_intro_below_threshold_then_above() {
        // Quiet pre-roll that *crosses* the threshold inside the scan
        // window: first 200 frames -70 dB, then 200 frames at -55 dB.
        // The boundary lands at frame 200 → trim 200, fade-in. Then
        // the surviving content should otherwise be untouched.
        let spec = mono_spec();
        let mut trimmer = GaplessTrimmer::silence_trim(silence_params(60.0, 32));

        let mut pcm = vec![0.0003_f32; 200];
        pcm.extend(std::iter::repeat_n(0.001_8, 200));
        assert!(trimmer.push(custom_chunk(spec, 0, pcm)).is_empty());

        let flushed = trimmer.flush();
        let pcm_out = collect_pcm(&flushed);
        assert_eq!(pcm_out.len(), 200);
        let fade_len = fade_frames_for(spec).min(pcm_out.len());
        // After the fade window we should be at the original level.
        for &sample in &pcm_out[fade_len..] {
            assert!((sample - 0.001_8).abs() < 1e-7);
        }
    }

    #[kithara::test]
    fn silence_trim_min_frames_boundary_under_min() {
        // 31 silent frames is below the 32-frame threshold → no trim.
        let spec = mono_spec();
        let mut trimmer = GaplessTrimmer::silence_trim(silence_params(60.0, 32));

        let mut pcm = vec![0.0_f32; 31];
        pcm.extend(std::iter::repeat_n(0.5, 64));
        assert!(trimmer.push(custom_chunk(spec, 0, pcm)).is_empty());

        let flushed = trimmer.flush();
        let pcm_out = collect_pcm(&flushed);
        assert_eq!(pcm_out.len(), 95);
        // First sample is the original 0.0 — no fade-in.
        assert_eq!(pcm_out[0], 0.0);
    }

    #[kithara::test]
    fn silence_trim_min_frames_boundary_at_min() {
        // Exactly 32 silent frames → trim, fade-in armed.
        let spec = mono_spec();
        let mut trimmer = GaplessTrimmer::silence_trim(silence_params(60.0, 32));

        let mut pcm = vec![0.0_f32; 32];
        pcm.extend(std::iter::repeat_n(0.5, 64));
        assert!(trimmer.push(custom_chunk(spec, 0, pcm)).is_empty());

        let flushed = trimmer.flush();
        let pcm_out = collect_pcm(&flushed);
        assert_eq!(pcm_out.len(), 64);
        assert!(pcm_out[0].abs() < 0.05);
    }

    #[kithara::test]
    fn silence_trim_scan_window_exhausted_preserves_audio() {
        let spec = mono_spec();
        let params = SilenceTrimParams {
            threshold_db: 60.0,
            min_trim_frames: 32,
            scan_window_frames: 256,
            trim_trailing: false,
        };
        let mut trimmer = GaplessTrimmer::silence_trim(params);

        // 300 silent frames — longer than the scan window → keep
        // everything. This guards against eating long fade-ins.
        let pcm = vec![0.0_f32; 300];
        assert!(trimmer.push(custom_chunk(spec, 0, pcm)).is_empty());

        let flushed = trimmer.flush();
        let pcm_out = collect_pcm(&flushed);
        assert_eq!(pcm_out.len(), 300);
        assert!(pcm_out.iter().all(|s| *s == 0.0));
    }

    #[kithara::test]
    fn silence_trim_no_op_with_immediate_content() {
        let spec = mono_spec();
        let mut trimmer = GaplessTrimmer::silence_trim(silence_params(60.0, 32));

        let pcm = vec![0.5_f32; 256];
        assert!(trimmer.push(custom_chunk(spec, 0, pcm)).is_empty());

        let flushed = trimmer.flush();
        let pcm_out = collect_pcm(&flushed);
        assert_eq!(pcm_out.len(), 256);
        // No trim, no fade-in.
        assert!(pcm_out.iter().all(|s| (*s - 0.5).abs() < 1e-7));
    }

    #[kithara::test]
    fn silence_trim_trailing_disabled_by_default() {
        let spec = mono_spec();
        let mut trimmer = GaplessTrimmer::silence_trim(silence_params(60.0, 32));

        let mut pcm = vec![0.5_f32; 64];
        pcm.extend(std::iter::repeat_n(0.0, 64));
        assert!(trimmer.push(custom_chunk(spec, 0, pcm)).is_empty());

        let flushed = trimmer.flush();
        let pcm_out = collect_pcm(&flushed);
        // Trailing silence preserved.
        assert_eq!(pcm_out.len(), 128);
    }

    #[kithara::test]
    fn silence_trim_trailing_enabled() {
        let spec = mono_spec();
        let params = SilenceTrimParams {
            threshold_db: 60.0,
            min_trim_frames: 32,
            scan_window_frames: 4096,
            trim_trailing: true,
        };
        let mut trimmer = GaplessTrimmer::silence_trim(params);

        let mut pcm = vec![0.5_f32; 64];
        pcm.extend(std::iter::repeat_n(0.0, 64));
        assert!(trimmer.push(custom_chunk(spec, 0, pcm)).is_empty());

        let flushed = trimmer.flush();
        let pcm_out = collect_pcm(&flushed);
        assert_eq!(pcm_out.len(), 64);
        assert!(pcm_out.iter().all(|s| (*s - 0.5).abs() < 1e-7));
    }

    #[kithara::test]
    fn silence_trim_seek_disables_leading_only() {
        let spec = mono_spec();
        let mut trimmer = GaplessTrimmer::silence_trim(silence_params(60.0, 32));

        assert!(trimmer.push(silent_chunk(spec, 0, 128)).is_empty());
        trimmer.notify_seek();

        // After seek, leading trim is off — silence after the seek
        // point is real content the user requested.
        assert!(
            trimmer
                .push(custom_chunk(spec, 128, vec![0.0, 0.0, 3.0, 4.0]))
                .is_empty()
        );

        let mut flushed = trimmer.flush();
        assert_eq!(flushed.len(), 1);
        let out = flushed.remove(0);
        assert_eq!(out.meta.frame_offset, 128);
        assert_eq!(out.samples(), &[0.0, 0.0, 3.0, 4.0]);
    }

    #[kithara::test]
    fn silence_trim_preserves_all_silence_track() {
        let spec = mono_spec();
        let mut trimmer = GaplessTrimmer::silence_trim(silence_params(60.0, 32));

        assert!(trimmer.push(silent_chunk(spec, 0, 128)).is_empty());

        let mut flushed = trimmer.flush();
        assert_eq!(flushed.len(), 1);
        let out = flushed.remove(0);
        assert_eq!(out.frames(), 128);
        assert!(out.samples().iter().all(|sample| *sample == 0.0));
    }

    #[kithara::test]
    fn silence_trim_respects_multi_channel_threshold() {
        // Per-frame check: a frame is silent only if *all* channels
        // are below threshold. Stereo input with channel 1 louder
        // than threshold ends silence for the whole frame.
        let spec = stereo_spec();
        let mut trimmer = GaplessTrimmer::silence_trim(silence_params(60.0, 32));

        // 32 silent stereo frames + one frame [0.0, 2e-3] (2e-3 is
        // louder than the -60 dB ≈ 1e-3 threshold).
        assert!(
            trimmer
                .push(custom_chunk(
                    spec,
                    0,
                    vec![0.0; 32 * usize::from(spec.channels)]
                ))
                .is_empty()
        );
        assert!(
            trimmer
                .push(custom_chunk(spec, 32, vec![0.0, 2.0e-3]))
                .is_empty()
        );

        let mut flushed = trimmer.flush();
        assert_eq!(flushed.len(), 1);
        let out = flushed.remove(0);
        assert_eq!(out.meta.frame_offset, 32);
        assert_eq!(out.frames(), 1);
        // Fade-in only spans frames; one frame fits inside the fade
        // window so the value is attenuated by the first ramp gain.
        assert!(out.samples()[1].abs() < 2.0e-3);
    }

    #[kithara::test]
    fn silence_trim_does_not_introduce_click_at_boundary() {
        // The whole point of the fade-in: when the audio jumps from
        // pure silence to a non-trivial level, the surviving stream
        // must not start with the full step. Verified by inspecting
        // the first sample after the trim.
        let spec = mono_spec();
        let mut trimmer = GaplessTrimmer::silence_trim(silence_params(60.0, 32));

        let mut pcm = vec![0.0_f32; 64];
        pcm.extend(std::iter::repeat_n(1.0, 256));
        assert!(trimmer.push(custom_chunk(spec, 0, pcm)).is_empty());

        let flushed = trimmer.flush();
        let pcm_out = collect_pcm(&flushed);
        assert!(
            pcm_out[0].abs() < 0.1,
            "boundary sample {} too loud",
            pcm_out[0]
        );
    }
}
