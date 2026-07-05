use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use kithara_bufpool::{PcmBuf, PcmPool};
use kithara_platform::time::Duration;
use kithara_stream::{AudioCodec, BoxedEventSink, ReaderChunkSignal, ReaderSeekSignal};
use kithara_test_utils::kithara;

use crate::{
    codec::FrameCodec,
    demuxer::{DemuxOutcome, DemuxSeekOutcome, Demuxer},
    duration_for_frames,
    error::DecodeResult,
    traits::{Decoder, DecoderChunkOutcome, DecoderSeekOutcome},
    types::{PcmChunk, PcmMeta, PcmSpec, TrackMetadata},
};

/// Generic decoder built by composition: a [`Demuxer`] feeds raw frames
/// into a [`FrameCodec`] which produces PCM. One implementation, one
/// dispatch path — no per-backend duplication.
pub(crate) struct ComposedDecoder<D: Demuxer, C: FrameCodec> {
    codec: C,
    demuxer: D,
    byte_len_handle: Option<Arc<AtomicU64>>,
    duration: Option<Duration>,
    /// Reader-side event sink. Single-owner `Box<dyn ReaderEventSink>` —
    /// `None` skips emission entirely; `Some(_)` calls `on_chunk` /
    /// `on_seek` directly via `&mut` after the inner outcome resolves.
    /// No lock on the produce-core. Folded in from the former
    /// `HookedDecoder` decorator — every decoder is hookable now.
    hooks: Option<BoxedEventSink>,
    /// When `Some`, frames whose decode-time end is `<= target` are
    /// dropped before the next chunk is emitted. Cleared after the
    /// first frame past the target is consumed. Lets `seek(target)`
    /// land precisely at `target` instead of at the granule boundary.
    pending_seek_target: Option<Duration>,
    pool: PcmPool,
    spec: PcmSpec,
    /// Set on every seek; the next emitted chunk re-anchors
    /// `frame_offset` to its own pts so that the chunk-level invariant
    /// `frame_offset / sample_rate ≈ timestamp` holds even when the
    /// demuxer snapped to a packet boundary inside / behind the
    /// requested seek target.
    resync_frame_offset_to_pts: bool,
    epoch: u64,
    /// Cumulative frame counter. Anchored to `landed_at` on seek (so the
    /// next chunk's `frame_offset / sample_rate ≈ timestamp`) and
    /// incremented by `decoded.frames` per emitted chunk. Tracking it
    /// cumulatively avoids the precision loss that sneaks in if every
    /// chunk recomputes `floor(pts * sample_rate)` from a `Duration`
    /// nanosecond value.
    frame_offset: u64,
}

/// Runtime wiring for a [`ComposedDecoder`], separated from the
/// `(demuxer, codec)` pair: the PCM buffer `pool`, the initial seek
/// `epoch`, an optional `byte_len_handle` for byte-length observability,
/// and optional reader-event `hooks`.
///
/// `pool` is a required field with no `Default`: the host must thread its
/// configured pool all the way to the decoder, and a missing pool must be
/// a compile error, not a silent fall-back to the process-global
/// `PcmPool::default()`. The test-only [`DecoderRuntime::for_test`] is the
/// single place that opts into the global pool, and it is `#[cfg(test)]`
/// so production code physically cannot reach it.
pub(crate) struct DecoderRuntime {
    pub(crate) byte_len_handle: Option<Arc<AtomicU64>>,
    pub(crate) hooks: Option<BoxedEventSink>,
    pub(crate) pool: PcmPool,
    pub(crate) epoch: u64,
}

impl<D: Demuxer, C: FrameCodec> ComposedDecoder<D, C> {
    /// Build a decoder from a `(demuxer, codec)` pair and its runtime wiring.
    pub(crate) fn new(demuxer: D, codec: C, runtime: DecoderRuntime) -> Self {
        let spec = codec.spec();
        let duration = demuxer.duration();
        Self {
            demuxer,
            codec,
            spec,
            duration,
            pool: runtime.pool,
            epoch: runtime.epoch,
            byte_len_handle: runtime.byte_len_handle,
            hooks: runtime.hooks,
            frame_offset: 0,
            pending_seek_target: None,
            resync_frame_offset_to_pts: true,
        }
    }

    /// Build the output `PcmChunk` from a just-filled pool buffer plus
    /// the demuxed frame's metadata. Inlined fields (rather than taking
    /// `&Frame<'_>`) so the caller can release the demuxer borrow before
    /// invoking this — needed because `Frame<'_>` borrows into the
    /// demuxer state and would conflict with `&mut self` here.
    #[kithara::probe(timestamp, frames)]
    fn build_chunk(
        &mut self,
        buf: PcmBuf,
        frames: u32,
        timestamp: Duration,
        source_bytes: u64,
    ) -> PcmChunk {
        let live_spec = self.codec.spec();
        self.spec = live_spec;

        let chunk_secs = f64::from(frames) / f64::from(live_spec.sample_rate.get());
        let frame_duration = Duration::from_secs_f64(chunk_secs);
        let end_timestamp = timestamp.saturating_add(frame_duration);

        if self.resync_frame_offset_to_pts {
            self.resync_frame_offset_to_pts = false;
            self.frame_offset = frame_offset_for(timestamp, live_spec.sample_rate.get());
        }
        let frame_offset = self.frame_offset;
        self.frame_offset = self.frame_offset.saturating_add(u64::from(frames));

        let meta = PcmMeta {
            end_timestamp,
            timestamp,
            frames,
            frame_offset,
            source_bytes,
            segment_index: self.demuxer.current_segment_index(),
            source_byte_offset: None,
            variant_index: self.demuxer.current_variant_index(),
            spec: live_spec,
            epoch: self.epoch,
        };
        PcmChunk::new(meta, buf)
    }

    fn emit_chunk_signal(&mut self, outcome: &DecoderChunkOutcome) {
        let signal = match outcome {
            DecoderChunkOutcome::Chunk(_) => ReaderChunkSignal::Chunk,
            DecoderChunkOutcome::Pending(reason) => ReaderChunkSignal::Pending(*reason),
            DecoderChunkOutcome::Eof => ReaderChunkSignal::Eof,
        };
        if let Some(hooks) = self.hooks.as_mut() {
            hooks.on_chunk(signal);
        }
    }

    fn emit_seek_signal(&mut self, outcome: &DecoderSeekOutcome) {
        let signal = match outcome {
            DecoderSeekOutcome::Landed {
                landed_byte,
                preroll,
                ..
            } => ReaderSeekSignal::Landed {
                landed_byte: *landed_byte,
                preroll: *preroll,
            },
            DecoderSeekOutcome::PastEof { .. } => ReaderSeekSignal::PastEof,
        };
        if let Some(hooks) = self.hooks.as_mut() {
            hooks.on_seek(signal);
        }
    }

    #[kithara::hang_watchdog]
    fn next_chunk_inner(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        loop {
            hang_tick!();
            let frame = match self.demuxer.next_frame()? {
                DemuxOutcome::Frame(frame) => frame,
                DemuxOutcome::Pending(reason) => {
                    return Ok(DecoderChunkOutcome::Pending(reason));
                }
                DemuxOutcome::Eof => return self.drain_codec_eof(),
            };
            hang_reset!();
            let frame_pts = frame.pts;
            let frame_end = frame_pts.saturating_add(frame.duration);
            let source_bytes = u64::try_from(frame.data.len()).unwrap_or(u64::MAX);
            let mut buf = self.pool.get();
            let mut frames =
                self.codec
                    .decode_frame(frame.data, frame_pts, frame.packet_desc, &mut buf)?;
            let mut chunk_pts = frame_pts;
            if let Some(target) = self.pending_seek_target {
                if frame_end <= target {
                    drop(buf);
                    continue;
                }
                // WHY: frame straddles target — trim leading samples (CONTEXT.md "Seek pre-roll and trim").
                if frame_pts < target && frames > 0 {
                    let live_spec = self.codec.spec();
                    let trim_frames_u64 =
                        frames_to_trim(frame_pts, target, live_spec.sample_rate.get())
                            .min(u64::from(frames));
                    let trim_frames = u32::try_from(trim_frames_u64).unwrap_or(frames);
                    // Duration rounding can leave `frame_end > target` even when
                    // this packet is fully pre-target in sample space.
                    if trim_frames >= frames {
                        drop(buf);
                        continue;
                    }
                    if trim_frames > 0 {
                        let channels = usize::from(live_spec.channels);
                        let trim_samples = usize::try_from(trim_frames)
                            .unwrap_or(0)
                            .saturating_mul(channels);
                        let total_samples = usize::try_from(frames)
                            .unwrap_or(0)
                            .saturating_mul(channels);
                        if trim_samples < total_samples && trim_samples <= buf.len() {
                            buf.copy_within(trim_samples..total_samples, 0);
                            buf.truncate(total_samples - trim_samples);
                            frames = frames.saturating_sub(trim_frames);
                            chunk_pts = target;
                        }
                    }
                }
                self.pending_seek_target = None;
            }
            if frames == 0 {
                continue;
            }
            let chunk = self.build_chunk(buf, frames, chunk_pts, source_bytes);
            return Ok(DecoderChunkOutcome::Chunk(chunk));
        }
    }

    fn drain_codec_eof(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        if self.spec.sample_rate.get() == self.demuxer.track_info().sample_rate {
            return Ok(DecoderChunkOutcome::Eof);
        }

        let mut buf = self.pool.get();
        let timestamp = duration_for_frames(self.spec.sample_rate.get(), self.frame_offset);
        let frames = self.codec.decode_frame(&[], timestamp, &[], &mut buf)?;
        if frames == 0 {
            return Ok(DecoderChunkOutcome::Eof);
        }
        let chunk = self.build_chunk(buf, frames, timestamp, 0);
        Ok(DecoderChunkOutcome::Chunk(chunk))
    }

    fn seek_inner(&mut self, pos: Duration) -> DecodeResult<DecoderSeekOutcome> {
        let priming = self.codec.priming(self.demuxer.track_info().codec);
        match self.demuxer.seek(pos, priming)? {
            DemuxSeekOutcome::Landed {
                landed_at,
                landed_byte,
                preroll,
            } => {
                self.codec.flush()?;
                self.pending_seek_target = (landed_at < pos).then_some(pos);
                self.frame_offset = frame_offset_for(landed_at, self.spec.sample_rate.get());
                self.resync_frame_offset_to_pts = true;
                Ok(DecoderSeekOutcome::Landed {
                    landed_byte,
                    landed_at,
                    preroll,
                    landed_frame: self.frame_offset,
                })
            }
            DemuxSeekOutcome::PastEof { duration } => {
                self.codec.flush()?;
                Ok(DecoderSeekOutcome::PastEof { duration })
            }
        }
    }
}

impl<D: Demuxer + 'static, C: FrameCodec> Decoder for ComposedDecoder<D, C> {
    fn default_priming_frames(&self, codec: AudioCodec) -> u64 {
        AudioCodec::encoder_priming_frames(codec)
            .saturating_add(self.codec.decoder_algo_delay(codec))
    }

    fn duration(&self) -> Option<Duration> {
        self.duration
    }

    fn flush_reader_signals(&mut self) {
        if let Some(hooks) = self.hooks.as_mut() {
            hooks.flush();
        }
    }

    fn metadata(&self) -> TrackMetadata {
        TrackMetadata::default()
    }

    fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        let outcome = self.next_chunk_inner()?;
        self.emit_chunk_signal(&outcome);
        Ok(outcome)
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<DecoderSeekOutcome> {
        let outcome = self.seek_inner(pos)?;
        self.emit_seek_signal(&outcome);
        Ok(outcome)
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }

    fn track_info(&self) -> crate::DecoderTrackInfo {
        self.codec.track_info()
    }

    fn update_byte_len(&self, len: u64) {
        if let Some(handle) = &self.byte_len_handle {
            handle.store(len, Ordering::Release);
        }
    }
}

#[cfg(test)]
impl DecoderRuntime {
    /// Test-only runtime: the process-global [`PcmPool::default`], epoch 0,
    /// no byte-length handle, no hooks. Confined to test builds so a
    /// production decoder can never silently bind the global pool instead
    /// of the host's configured one.
    pub(crate) fn for_test() -> Self {
        Self {
            pool: PcmPool::default(),
            epoch: 0,
            byte_len_handle: None,
            hooks: None,
        }
    }
}

#[cfg(all(test, feature = "symphonia"))]
mod default_priming_tests {
    use std::io::Cursor;

    use kithara_stream::AudioCodec;
    use symphonia::{
        core::{
            formats::{FormatOptions, probe::Hint},
            io::{MediaSourceStream, MediaSourceStreamOptions},
            meta::MetadataOptions,
        },
        default,
    };

    use super::*;
    use crate::symphonia::{SymphoniaCodec, SymphoniaConfig, SymphoniaDemuxer};

    fn build_mp3_decoder() -> ComposedDecoder<SymphoniaDemuxer, SymphoniaCodec> {
        const TEST_MP3_BYTES: &[u8] = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/test.mp3"
        ));
        let cursor = Cursor::new(TEST_MP3_BYTES.to_vec());
        let mss = MediaSourceStream::new(Box::new(cursor), MediaSourceStreamOptions::default());
        let mut hint = Hint::new();
        hint.with_extension("mp3");
        let format_reader = default::get_probe()
            .probe(
                &hint,
                mss,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .expect("BUG: MP3 probe should succeed");
        let demuxer = SymphoniaDemuxer::from_reader_with_layout(format_reader, None, None)
            .expect("BUG: MP3 demuxer should build");
        let track_info = demuxer.track_info().clone();
        let codec = SymphoniaCodec::open_with_config(&track_info, &SymphoniaConfig::default())
            .expect("BUG: MP3 codec should open");
        ComposedDecoder::new(demuxer, codec, DecoderRuntime::for_test())
    }

    #[kithara::test]
    fn composed_decoder_priming_combines_encoder_and_symphonia_mp3_algo_delay() {
        let decoder = build_mp3_decoder();
        // WHY: 1105 = 576 libmp3lame priming + 529 LAME algo delay (CONTEXT.md "Gapless probe contract").
        assert_eq!(decoder.default_priming_frames(AudioCodec::Mp3), 1105);
        assert_eq!(decoder.default_priming_frames(AudioCodec::AacLc), 1024);
        assert_eq!(decoder.default_priming_frames(AudioCodec::Opus), 312);
        assert_eq!(decoder.default_priming_frames(AudioCodec::Flac), 0);
    }
}

/// Absolute sample frame a PTS falls on. Rounds the sub-second part to the
/// nearest frame (half-up) so this map stays consistent with [`frames_to_trim`],
/// which positions trimmed content by `round(delta_secs * sample_rate)`. A floor
/// here disagreed with that round by up to one frame: when a demuxer quantizes a
/// seek landing a fraction of a sample below the exact target frame, a floored
/// label reads one frame short of the contiguous decode head (a −1 seam at a
/// mid-playback recreate), while the trimmed audio itself stays continuous.
fn frame_offset_for(at: Duration, sample_rate: u32) -> u64 {
    let secs = at.as_secs();
    let subsec_frames = (u64::from(at.subsec_nanos()) * u64::from(sample_rate))
        .saturating_add(500_000_000)
        / 1_000_000_000;
    secs.saturating_mul(u64::from(sample_rate))
        .saturating_add(subsec_frames)
}

/// Per-channel sample count to drop from the head of the target packet
/// so the emitted chunk starts at the user's seek target rather than
/// at the packet boundary. Returns 0 when `frame_pts >= target` or
/// `sample_rate == 0`. Rounds to nearest sample (half-up) so the trim
/// matches `round(target_secs * sample_rate)` callers use to index
/// PCM by absolute sample frame.
fn frames_to_trim(frame_pts: Duration, target: Duration, sample_rate: u32) -> u64 {
    if sample_rate == 0 || frame_pts >= target {
        return 0;
    }
    let delta_nanos = target.saturating_sub(frame_pts).as_nanos();
    let sr_u128 = u128::from(sample_rate);
    // WHY: round-to-nearest sample, half-up via +5e8 before the /1e9 divide.
    let frames_u128 = delta_nanos
        .saturating_mul(sr_u128)
        .saturating_add(500_000_000)
        / 1_000_000_000;
    u64::try_from(frames_u128).unwrap_or(u64::MAX)
}

#[cfg(all(test, feature = "symphonia"))]
mod smoke_tests {

    use std::io::Cursor;

    use kithara_stream::AudioCodec;
    use kithara_test_utils::kithara;
    use symphonia::{
        core::{
            formats::{FormatOptions, probe::Hint},
            io::{MediaSourceStream, MediaSourceStreamOptions},
            meta::MetadataOptions,
        },
        default,
    };

    use super::*;
    use crate::{
        symphonia::{FileOpen, SymphoniaCodec, SymphoniaConfig, SymphoniaDemuxer},
        traits::{Decoder, DecoderChunkOutcome, DecoderSeekOutcome},
    };

    const TEST_MP3_BYTES: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/test.mp3"
    ));

    fn build_mp3_demuxer() -> SymphoniaDemuxer {
        let cursor = Cursor::new(TEST_MP3_BYTES.to_vec());
        let mss = MediaSourceStream::new(Box::new(cursor), MediaSourceStreamOptions::default());
        let mut hint = Hint::new();
        hint.with_extension("mp3");
        let format_reader = default::get_probe()
            .probe(
                &hint,
                mss,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .expect("BUG: MP3 probe should succeed");
        SymphoniaDemuxer::from_reader_with_layout(format_reader, None, None)
            .expect("BUG: MP3 demuxer should build")
    }

    #[kithara::test]
    fn mp3_track_info_carries_codec_and_rate() {
        let demuxer = build_mp3_demuxer();
        let info = demuxer.track_info();
        assert_eq!(info.codec, AudioCodec::Mp3);
        assert!(info.sample_rate > 0, "sample rate must be populated");
        assert!(info.channels > 0, "channels must be populated");
    }

    #[kithara::test]
    fn mp3_universal_decoder_emits_non_empty_chunks() {
        let demuxer = build_mp3_demuxer();
        let track_info = demuxer.track_info().clone();
        let codec = SymphoniaCodec::open_with_config(&track_info, &SymphoniaConfig::default())
            .expect("BUG: MP3 codec should open");
        let mut decoder = ComposedDecoder::new(demuxer, codec, DecoderRuntime::for_test());

        let mut got_chunk = false;
        for _ in 0..16 {
            match decoder
                .next_chunk()
                .expect("BUG: next_chunk should not error")
            {
                DecoderChunkOutcome::Chunk(chunk) => {
                    assert!(chunk.frames() > 0, "Chunk frames must be > 0");
                    assert!(chunk.spec().sample_rate.get() > 0);
                    assert!(chunk.spec().channels > 0);
                    got_chunk = true;
                    break;
                }
                DecoderChunkOutcome::Pending(_) => continue,
                DecoderChunkOutcome::Eof => panic!("MP3 fixture must not EOF in 16 packets"),
            }
        }
        assert!(got_chunk, "ComposedDecoder must emit at least one chunk");
    }

    #[kithara::test]
    fn mp3_universal_decoder_seeks_back_to_start_after_pulling_chunks() {
        let demuxer = build_mp3_demuxer();
        let track_info = demuxer.track_info().clone();
        let codec = SymphoniaCodec::open_with_config(&track_info, &SymphoniaConfig::default())
            .expect("BUG: MP3 codec should open");
        let mut decoder = ComposedDecoder::new(demuxer, codec, DecoderRuntime::for_test());

        for _ in 0..4 {
            let _ = decoder
                .next_chunk()
                .expect("BUG: priming chunks should not error");
        }

        let outcome = decoder
            .seek(Duration::ZERO)
            .expect("BUG: seek to start must not error");
        match outcome {
            DecoderSeekOutcome::Landed { landed_at, .. } => {
                assert!(
                    landed_at < Duration::from_millis(50),
                    "seek to ZERO should land near 0, got {landed_at:?}"
                );
            }
            DecoderSeekOutcome::PastEof { .. } => {
                panic!("seek(0) on a real MP3 must not be PastEof")
            }
        }
    }

    #[kithara::test]
    fn symphonia_mp3_demuxer_emits_notneeded_preroll_after_seek() {
        let (demuxer, _byte_len_handle) = SymphoniaDemuxer::open_file(
            Cursor::new(TEST_MP3_BYTES),
            FileOpen {
                hint: Some("mp3".into()),
                container: None,
                byte_len_handle: None,
                byte_map: None,
            },
        )
        .expect("BUG: open_file must succeed");
        let track_info = demuxer.track_info().clone();
        let codec = SymphoniaCodec::open_with_config(&track_info, &SymphoniaConfig::default())
            .expect("BUG: MP3 codec should open");
        let mut decoder = ComposedDecoder::new(demuxer, codec, DecoderRuntime::for_test());

        let outcome = decoder.seek(Duration::from_secs(1)).expect("BUG: seek");
        let DecoderSeekOutcome::Landed {
            landed_byte,
            preroll,
            ..
        } = outcome
        else {
            panic!("expected Landed, got {outcome:?}");
        };
        assert!(
            landed_byte.is_some(),
            "BUG: symphonia MP3 must expose landed_byte"
        );
        assert_eq!(
            preroll,
            kithara_stream::PrerollHint::NotNeeded,
            "Symphonia handles MDCT priming internally; preroll must be NotNeeded"
        );
    }
}

#[cfg(test)]
fn write_silent_test_frame(
    spec: PcmSpec,
    frames_per_call: u32,
    out: &mut PcmBuf,
) -> DecodeResult<u32> {
    let frames = usize::try_from(frames_per_call)?;
    let samples = frames
        .checked_mul(usize::from(spec.channels))
        .ok_or_else(|| crate::error::DecodeError::InvalidData {
            detail: "test frame sample count overflow",
        })?;
    out.ensure_len(samples)?;
    for slot in out.iter_mut() {
        *slot = 0.0;
    }
    out.truncate(samples);
    Ok(frames_per_call)
}

#[cfg(test)]
mod test_stub_codec {

    use kithara_bufpool::PcmBuf;
    use kithara_platform::time::Duration;

    use crate::{codec::FrameCodec, error::DecodeResult, types::PcmSpec};

    pub(super) struct ConstFrameCodec {
        spec: PcmSpec,
        frames_per_call: u32,
    }

    impl ConstFrameCodec {
        pub(super) fn new(spec: PcmSpec, frames_per_call: u32) -> Self {
            Self {
                spec,
                frames_per_call,
            }
        }
    }

    impl FrameCodec for ConstFrameCodec {
        fn decode_frame(
            &mut self,
            _bytes: &[u8],
            _pts: Duration,
            _packet_desc: &[u8],
            out: &mut PcmBuf,
        ) -> DecodeResult<u32> {
            super::write_silent_test_frame(self.spec, self.frames_per_call, out)
        }

        fn flush(&mut self) -> DecodeResult<()> {
            Ok(())
        }

        fn spec(&self) -> PcmSpec {
            self.spec
        }
    }
}

#[cfg(test)]
mod test_counting_codec {

    use std::sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    };

    use kithara_bufpool::PcmBuf;
    use kithara_platform::time::Duration;

    use crate::{codec::FrameCodec, error::DecodeResult, types::PcmSpec};

    pub(super) struct CountingCodec {
        pub(super) decode_calls: Arc<AtomicU32>,
        pub(super) flush_calls: Arc<AtomicU32>,
        pub(super) spec: PcmSpec,
        pub(super) frames_per_call: u32,
    }

    impl CountingCodec {
        pub(super) fn new(spec: PcmSpec, frames_per_call: u32) -> Self {
            Self {
                spec,
                frames_per_call,
                decode_calls: Arc::new(AtomicU32::new(0)),
                flush_calls: Arc::new(AtomicU32::new(0)),
            }
        }
    }

    impl FrameCodec for CountingCodec {
        fn decode_frame(
            &mut self,
            _bytes: &[u8],
            _pts: Duration,
            _packet_desc: &[u8],
            out: &mut PcmBuf,
        ) -> DecodeResult<u32> {
            self.decode_calls.fetch_add(1, Ordering::SeqCst);
            super::write_silent_test_frame(self.spec, self.frames_per_call, out)
        }

        fn flush(&mut self) -> DecodeResult<()> {
            self.flush_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn spec(&self) -> PcmSpec {
            self.spec
        }
    }
}

#[cfg(test)]
mod test_eof_drain_codec {

    use std::sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    };

    use kithara_bufpool::PcmBuf;
    use kithara_platform::time::Duration;

    use crate::{codec::FrameCodec, error::DecodeResult, types::PcmSpec};

    pub(super) struct EofDrainCodec {
        pub(super) empty_decode_calls: Arc<AtomicU32>,
        spec: PcmSpec,
        frames_per_call: u32,
        tail_frames: u32,
        tail_pending: bool,
    }

    impl EofDrainCodec {
        pub(super) fn new(spec: PcmSpec, frames_per_call: u32, tail_frames: u32) -> Self {
            Self {
                empty_decode_calls: Arc::new(AtomicU32::new(0)),
                spec,
                frames_per_call,
                tail_frames,
                tail_pending: tail_frames > 0,
            }
        }
    }

    impl FrameCodec for EofDrainCodec {
        fn decode_frame(
            &mut self,
            bytes: &[u8],
            _pts: Duration,
            _packet_desc: &[u8],
            out: &mut PcmBuf,
        ) -> DecodeResult<u32> {
            if bytes.is_empty() {
                self.empty_decode_calls.fetch_add(1, Ordering::SeqCst);
                if !self.tail_pending {
                    out.clear();
                    return Ok(0);
                }
                self.tail_pending = false;
                return super::write_silent_test_frame(self.spec, self.tail_frames, out);
            }

            super::write_silent_test_frame(self.spec, self.frames_per_call, out)
        }

        fn flush(&mut self) -> DecodeResult<()> {
            self.tail_pending = self.tail_frames > 0;
            Ok(())
        }

        fn spec(&self) -> PcmSpec {
            self.spec
        }
    }
}

#[cfg(test)]
mod seek_trim_tests {

    use std::{
        num::NonZeroU32,
        sync::{Arc, atomic::Ordering},
    };

    use kithara_platform::time::Duration;
    use kithara_stream::AudioCodec;
    use kithara_test_utils::kithara;

    use super::{test_counting_codec::CountingCodec, *};
    use crate::{
        demuxer::{DemuxOutcome, DemuxSeekOutcome, Frame, TrackInfo},
        duration_for_frames,
        traits::Decoder,
    };

    struct Consts;

    impl Consts {
        const CHANNELS: u16 = 2;
        const OUTPUT_SAMPLE_RATE: u32 = 48_000;
        const PACKET_COUNT: u64 = 6;
        const PACKET_FRAMES: u32 = 1024;
        const SAMPLE_RATE: u32 = 44_100;
    }

    struct ThreeFrameDemuxer {
        track: TrackInfo,
        held: Vec<u8>,
        idx: usize,
    }

    #[derive(Clone, Copy)]
    struct BoundaryFrame {
        pts: Duration,
        duration: Duration,
    }

    struct BoundaryFrameDemuxer {
        track: TrackInfo,
        held: Vec<u8>,
        frames: Vec<BoundaryFrame>,
        idx: usize,
    }

    #[derive(Clone, Copy)]
    enum SeekTrimLayout {
        RegularPackets,
        RoundedPastTarget,
    }

    impl Demuxer for ThreeFrameDemuxer {
        fn duration(&self) -> Option<Duration> {
            Some(Duration::from_millis(60))
        }
        fn next_frame(&mut self) -> DecodeResult<DemuxOutcome<'_>> {
            let pts_ms = match self.idx {
                0 => 0,
                1 => 20,
                2 => 40,
                _ => return Ok(DemuxOutcome::Eof),
            };
            self.idx += 1;
            self.held = vec![0u8; 4];
            Ok(DemuxOutcome::Frame(Frame {
                pts: Duration::from_millis(pts_ms),
                duration: Duration::from_millis(20),
                data: &self.held,
                packet_desc: &[],
            }))
        }
        fn seek(
            &mut self,
            _pos: Duration,
            _priming: crate::codec::CodecPriming,
        ) -> DecodeResult<DemuxSeekOutcome> {
            self.idx = 0;
            Ok(DemuxSeekOutcome::Landed {
                landed_at: Duration::ZERO,
                landed_byte: Some(0),
                preroll: crate::demuxer::PrerollHint::NotNeeded,
            })
        }
        fn track_info(&self) -> &TrackInfo {
            &self.track
        }
    }

    impl Demuxer for BoundaryFrameDemuxer {
        fn duration(&self) -> Option<Duration> {
            None
        }

        fn next_frame(&mut self) -> DecodeResult<DemuxOutcome<'_>> {
            let Some(frame) = self.frames.get(self.idx).copied() else {
                return Ok(DemuxOutcome::Eof);
            };
            self.idx += 1;
            self.held = vec![0u8; 4];
            Ok(DemuxOutcome::Frame(Frame {
                pts: frame.pts,
                duration: frame.duration,
                data: &self.held,
                packet_desc: &[],
            }))
        }

        fn seek(
            &mut self,
            _pos: Duration,
            _priming: crate::codec::CodecPriming,
        ) -> DecodeResult<DemuxSeekOutcome> {
            self.idx = 0;
            Ok(DemuxSeekOutcome::Landed {
                landed_at: Duration::ZERO,
                landed_byte: Some(0),
                preroll: crate::demuxer::PrerollHint::NotNeeded,
            })
        }

        fn track_info(&self) -> &TrackInfo {
            &self.track
        }
    }

    fn empty_track() -> TrackInfo {
        track_with_rate(Consts::SAMPLE_RATE)
    }

    fn track_with_rate(sample_rate: u32) -> TrackInfo {
        TrackInfo {
            codec: AudioCodec::AacLc,
            sample_rate,
            channels: 2,
            extra_data: Vec::new(),
            duration: None,
            gapless: None,
        }
    }

    fn packet_duration() -> Duration {
        duration_for_frames(Consts::SAMPLE_RATE, u64::from(Consts::PACKET_FRAMES))
    }

    fn regular_seek_frames() -> Vec<BoundaryFrame> {
        (0..Consts::PACKET_COUNT)
            .map(|packet_idx| BoundaryFrame {
                pts: duration_for_frames(
                    Consts::SAMPLE_RATE,
                    packet_idx.saturating_mul(u64::from(Consts::PACKET_FRAMES)),
                ),
                duration: packet_duration(),
            })
            .collect()
    }

    fn rounded_past_target_frames(target: Duration) -> Vec<BoundaryFrame> {
        vec![
            BoundaryFrame {
                pts: Duration::ZERO,
                duration: target.saturating_add(Duration::from_nanos(1)),
            },
            BoundaryFrame {
                pts: target,
                duration: packet_duration(),
            },
        ]
    }

    fn seek_frames_for(target: Duration, layout: SeekTrimLayout) -> Vec<BoundaryFrame> {
        match layout {
            SeekTrimLayout::RegularPackets => regular_seek_frames(),
            SeekTrimLayout::RoundedPastTarget => rounded_past_target_frames(target),
        }
    }

    #[kithara::test]
    fn pre_target_frames_are_decoded_and_dropped_by_pending_seek_target() {
        const FRAME_FRAMES: u32 = 882;

        let codec = CountingCodec::new(
            PcmSpec::new(2, NonZeroU32::new(44_100).expect("test rate")),
            FRAME_FRAMES,
        );
        let calls = Arc::clone(&codec.decode_calls);
        let demuxer = ThreeFrameDemuxer {
            track: empty_track(),
            idx: 0,
            held: Vec::new(),
        };
        let mut decoder = ComposedDecoder::new(demuxer, codec, DecoderRuntime::for_test());

        let _ = decoder.seek(Duration::from_millis(30)).expect("BUG: seek");

        let outcome = decoder.next_chunk().expect("BUG: next_chunk");
        assert!(
            matches!(outcome, DecoderChunkOutcome::Chunk(_)),
            "expected Chunk, got {outcome:?}"
        );

        // WHY: 2 calls — frame[0] (end=20ms ≤ 30ms target) dropped by the guard; frame[1] (end=40ms) emitted with leading 10ms trimmed.
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "decode_frame must be called for pre-target frames so MDCT advances"
        );
    }

    #[kithara::test]
    fn fully_trimmed_seek_packet_is_dropped_before_target_chunk() {
        const SAMPLE_RATE: u32 = 44_100;
        const PACKET_FRAMES: u32 = 1024;

        let target = duration_for_frames(SAMPLE_RATE, u64::from(PACKET_FRAMES));
        let rounded_past_target = target.saturating_add(Duration::from_nanos(1));
        let codec = CountingCodec::new(
            PcmSpec::new(2, NonZeroU32::new(SAMPLE_RATE).expect("test rate")),
            PACKET_FRAMES,
        );
        let calls = Arc::clone(&codec.decode_calls);
        let demuxer = BoundaryFrameDemuxer {
            track: empty_track(),
            held: Vec::new(),
            idx: 0,
            frames: vec![
                BoundaryFrame {
                    pts: Duration::ZERO,
                    duration: rounded_past_target,
                },
                BoundaryFrame {
                    pts: target,
                    duration: rounded_past_target,
                },
            ],
        };
        let mut decoder = ComposedDecoder::new(demuxer, codec, DecoderRuntime::for_test());

        let _ = decoder.seek(target).expect("BUG: seek");
        let outcome = decoder.next_chunk().expect("BUG: next_chunk");
        let DecoderChunkOutcome::Chunk(chunk) = outcome else {
            panic!("expected Chunk, got {outcome:?}");
        };

        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "the fully pre-target packet must be decode-discarded before emitting"
        );
        assert_eq!(chunk.meta.frame_offset, u64::from(PACKET_FRAMES));
        assert_eq!(chunk.meta.timestamp, target);
    }

    #[kithara::test]
    #[case::packet_boundary(
        duration_for_frames(Consts::SAMPLE_RATE, u64::from(Consts::PACKET_FRAMES)),
        SeekTrimLayout::RegularPackets
    )]
    #[case::mid_packet(
        duration_for_frames(Consts::SAMPLE_RATE, u64::from(Consts::PACKET_FRAMES) + 512),
        SeekTrimLayout::RegularPackets
    )]
    #[case::rounding_hair_past_boundary(
        duration_for_frames(Consts::SAMPLE_RATE, u64::from(Consts::PACKET_FRAMES))
            .saturating_add(Duration::from_nanos(1)),
        SeekTrimLayout::RoundedPastTarget
    )]
    #[case::multiple_fully_pre_target_packets(
        duration_for_frames(Consts::SAMPLE_RATE, u64::from(Consts::PACKET_FRAMES) * 3 + 512),
        SeekTrimLayout::RegularPackets
    )]
    fn seek_trim_lands_first_chunk_on_exact_target_frame(
        #[case] target: Duration,
        #[case] layout: SeekTrimLayout,
    ) {
        let codec = CountingCodec::new(
            PcmSpec::new(
                Consts::CHANNELS,
                NonZeroU32::new(Consts::SAMPLE_RATE).expect("test rate"),
            ),
            Consts::PACKET_FRAMES,
        );
        let demuxer = BoundaryFrameDemuxer {
            track: empty_track(),
            held: Vec::new(),
            frames: seek_frames_for(target, layout),
            idx: 0,
        };
        let mut decoder = ComposedDecoder::new(demuxer, codec, DecoderRuntime::for_test());

        let _ = decoder.seek(target).expect("BUG: seek");
        let outcome = decoder.next_chunk().expect("BUG: next_chunk");
        let DecoderChunkOutcome::Chunk(chunk) = outcome else {
            panic!("expected Chunk, got {outcome:?}");
        };
        let expected_frame = frame_offset_for(target, Consts::SAMPLE_RATE);
        let packet_offset = expected_frame % u64::from(Consts::PACKET_FRAMES);
        let expected_frames = if packet_offset == 0 {
            Consts::PACKET_FRAMES
        } else {
            Consts::PACKET_FRAMES - u32::try_from(packet_offset).expect("packet offset fits in u32")
        };

        assert_eq!(chunk.meta.frame_offset, expected_frame);
        assert_eq!(chunk.meta.timestamp, target);
        assert_eq!(
            frame_offset_for(chunk.meta.timestamp, Consts::SAMPLE_RATE),
            expected_frame
        );
        assert!(
            chunk.meta.timestamp >= target,
            "first chunk timestamp {:?} before seek target {:?}",
            chunk.meta.timestamp,
            target
        );
        assert_eq!(chunk.meta.frames, expected_frames);
    }

    #[kithara::test]
    fn seek_trim_uses_codec_output_rate_for_resampled_chunks() {
        let source_packet_start =
            duration_for_frames(Consts::SAMPLE_RATE, u64::from(Consts::PACKET_FRAMES));
        let target =
            duration_for_frames(Consts::SAMPLE_RATE, u64::from(Consts::PACKET_FRAMES) + 512);
        let output_packet_frames = u32::try_from(frames_to_trim(
            Duration::ZERO,
            packet_duration(),
            Consts::OUTPUT_SAMPLE_RATE,
        ))
        .expect("output packet frame count fits in u32");
        let codec = CountingCodec::new(
            PcmSpec::new(
                Consts::CHANNELS,
                NonZeroU32::new(Consts::OUTPUT_SAMPLE_RATE).expect("test rate"),
            ),
            output_packet_frames,
        );
        let demuxer = BoundaryFrameDemuxer {
            track: track_with_rate(Consts::SAMPLE_RATE),
            held: Vec::new(),
            frames: regular_seek_frames(),
            idx: 0,
        };
        let mut decoder = ComposedDecoder::new(demuxer, codec, DecoderRuntime::for_test());

        let _ = decoder.seek(target).expect("BUG: seek");
        let outcome = decoder.next_chunk().expect("BUG: next_chunk");
        let DecoderChunkOutcome::Chunk(chunk) = outcome else {
            panic!("expected Chunk, got {outcome:?}");
        };
        let expected_trim = frames_to_trim(source_packet_start, target, Consts::OUTPUT_SAMPLE_RATE);
        let expected_frames =
            output_packet_frames - u32::try_from(expected_trim).expect("trim fits in u32");
        let expected_frame = frame_offset_for(target, Consts::OUTPUT_SAMPLE_RATE);

        assert_eq!(
            chunk.meta.spec.sample_rate.get(),
            Consts::OUTPUT_SAMPLE_RATE
        );
        assert_eq!(chunk.meta.frame_offset, expected_frame);
        assert_eq!(chunk.meta.timestamp, target);
        assert_eq!(chunk.meta.frames, expected_frames);
    }
}

#[cfg(test)]
mod eof_drain_tests {

    use std::{
        num::NonZeroU32,
        sync::{Arc, atomic::Ordering},
    };

    use kithara_platform::time::Duration;
    use kithara_stream::AudioCodec;
    use kithara_test_utils::kithara;

    use super::{test_eof_drain_codec::EofDrainCodec, *};
    use crate::{
        demuxer::{DemuxOutcome, DemuxSeekOutcome, Frame, TrackInfo},
        traits::Decoder,
    };

    struct OneFrameDemuxer {
        track: TrackInfo,
        held: Vec<u8>,
        emitted: bool,
    }

    impl Demuxer for OneFrameDemuxer {
        fn duration(&self) -> Option<Duration> {
            None
        }

        fn next_frame(&mut self) -> DecodeResult<DemuxOutcome<'_>> {
            if self.emitted {
                return Ok(DemuxOutcome::Eof);
            }
            self.emitted = true;
            self.held = vec![1u8; 4];
            Ok(DemuxOutcome::Frame(Frame {
                pts: Duration::ZERO,
                duration: Duration::from_millis(20),
                data: &self.held,
                packet_desc: &[],
            }))
        }

        fn seek(
            &mut self,
            _pos: Duration,
            _priming: crate::codec::CodecPriming,
        ) -> DecodeResult<DemuxSeekOutcome> {
            self.emitted = false;
            Ok(DemuxSeekOutcome::Landed {
                landed_at: Duration::ZERO,
                landed_byte: Some(0),
                preroll: crate::demuxer::PrerollHint::NotNeeded,
            })
        }

        fn track_info(&self) -> &TrackInfo {
            &self.track
        }
    }

    fn empty_track() -> TrackInfo {
        TrackInfo {
            codec: AudioCodec::AacLc,
            sample_rate: 44_100,
            channels: 2,
            extra_data: Vec::new(),
            duration: None,
            gapless: None,
        }
    }

    #[kithara::test]
    fn demuxer_eof_drains_codec_tail_before_reporting_eof() {
        const FRAMES: u32 = 960;
        const TAIL_FRAMES: u32 = 17;
        const SAMPLE_RATE: u32 = 48_000;

        let codec = EofDrainCodec::new(
            PcmSpec::new(2, NonZeroU32::new(SAMPLE_RATE).expect("test rate")),
            FRAMES,
            TAIL_FRAMES,
        );
        let empty_calls = Arc::clone(&codec.empty_decode_calls);
        let demuxer = OneFrameDemuxer {
            track: empty_track(),
            held: Vec::new(),
            emitted: false,
        };
        let mut decoder = ComposedDecoder::new(demuxer, codec, DecoderRuntime::for_test());

        let first = decoder.next_chunk().expect("BUG: first chunk");
        let DecoderChunkOutcome::Chunk(first_chunk) = first else {
            panic!("expected first Chunk, got {first:?}");
        };
        let drained = decoder.next_chunk().expect("BUG: EOF drain chunk");
        let DecoderChunkOutcome::Chunk(drained_chunk) = drained else {
            panic!("expected drained Chunk, got {drained:?}");
        };
        let eof = decoder.next_chunk().expect("BUG: EOF after drain");

        assert!(matches!(eof, DecoderChunkOutcome::Eof));
        assert_eq!(first_chunk.meta.frames, FRAMES);
        assert_eq!(drained_chunk.meta.frames, TAIL_FRAMES);
        assert_eq!(drained_chunk.meta.frame_offset, u64::from(FRAMES));
        assert_eq!(
            drained_chunk.meta.timestamp,
            duration_for_frames(SAMPLE_RATE, u64::from(FRAMES))
        );
        assert_eq!(empty_calls.load(Ordering::SeqCst), 2);
    }
}

#[cfg(test)]
mod hook_tests {

    use std::{
        num::NonZeroU32,
        sync::{Arc, Mutex},
    };

    use kithara_stream::{
        BoxedEventSink, PendingReason, ReaderChunkSignal, ReaderEventSink, ReaderSeekSignal,
    };
    use kithara_test_utils::kithara;

    use super::{test_stub_codec::ConstFrameCodec, *};
    use crate::{
        demuxer::{DemuxOutcome, DemuxSeekOutcome, Frame, TrackInfo},
        traits::Decoder,
    };

    #[derive(Default)]
    struct CallLog {
        chunks: Vec<&'static str>,
        seeks: Vec<&'static str>,
    }

    struct LoggingHooks {
        log: Arc<Mutex<CallLog>>,
    }

    impl ReaderEventSink for LoggingHooks {
        fn on_chunk(&mut self, signal: ReaderChunkSignal) {
            let tag = match signal {
                ReaderChunkSignal::Chunk => "chunk",
                ReaderChunkSignal::Pending(_) => "pending",
                ReaderChunkSignal::Eof => "eof",
                _ => "other",
            };
            self.log.lock().unwrap().chunks.push(tag);
        }

        fn on_seek(&mut self, signal: ReaderSeekSignal) {
            let tag = match signal {
                ReaderSeekSignal::Landed { .. } => "landed",
                ReaderSeekSignal::PastEof => "past_eof",
                _ => "other",
            };
            self.log.lock().unwrap().seeks.push(tag);
        }
    }

    /// Owned variant of `DemuxOutcome` used to seed the stub. Kept
    /// separate so the borrowed `DemuxOutcome<'_>` returned by
    /// `next_frame` can be backed by the stub's own buffer.
    enum StubOutcome {
        Frame {
            data: Vec<u8>,
            pts: Duration,
            duration: Duration,
        },
        Pending(PendingReason),
    }

    /// Stub demuxer + codec pair driven by canned outcomes. Exists only so
    /// hook tests can construct a `ComposedDecoder` without needing a real
    /// container/codec.
    struct StubDemuxer {
        track: TrackInfo,
        /// Current frame's owned bytes — live so the returned
        /// `Frame<'_>` slice has somewhere to point. Replaced on each
        /// `next_frame` call.
        held: Vec<u8>,
        next: Vec<StubOutcome>,
        seek: Vec<DemuxSeekOutcome>,
    }

    impl StubDemuxer {
        fn with_outcomes(next: Vec<StubOutcome>, seek: Vec<DemuxSeekOutcome>) -> Self {
            Self {
                next,
                seek,
                track: empty_track(),
                held: Vec::new(),
            }
        }
    }

    impl Demuxer for StubDemuxer {
        fn duration(&self) -> Option<Duration> {
            None
        }
        fn next_frame(&mut self) -> DecodeResult<DemuxOutcome<'_>> {
            match self.next.pop() {
                Some(StubOutcome::Frame {
                    data,
                    pts,
                    duration,
                }) => {
                    self.held = data;
                    Ok(DemuxOutcome::Frame(Frame {
                        pts,
                        duration,
                        data: &self.held,
                        packet_desc: &[],
                    }))
                }
                Some(StubOutcome::Pending(reason)) => Ok(DemuxOutcome::Pending(reason)),
                None => Ok(DemuxOutcome::Eof),
            }
        }
        fn seek(
            &mut self,
            _pos: Duration,
            _priming: crate::codec::CodecPriming,
        ) -> DecodeResult<DemuxSeekOutcome> {
            Ok(self.seek.pop().unwrap_or(DemuxSeekOutcome::PastEof {
                duration: Duration::ZERO,
            }))
        }
        fn track_info(&self) -> &TrackInfo {
            &self.track
        }
    }

    fn empty_track() -> TrackInfo {
        TrackInfo {
            codec: AudioCodec::Flac,
            sample_rate: 44_100,
            channels: 2,
            extra_data: Vec::new(),
            duration: None,
            gapless: None,
        }
    }

    fn build(
        demuxer: StubDemuxer,
        log: Arc<Mutex<CallLog>>,
    ) -> ComposedDecoder<StubDemuxer, ConstFrameCodec> {
        let codec = ConstFrameCodec::new(
            PcmSpec::new(2, NonZeroU32::new(44_100).expect("test rate")),
            1,
        );
        let hooks: BoxedEventSink = Box::new(LoggingHooks { log });
        ComposedDecoder::new(
            demuxer,
            codec,
            DecoderRuntime {
                hooks: Some(hooks),
                ..DecoderRuntime::for_test()
            },
        )
    }

    #[kithara::test]
    #[case::chunk_signal(
        StubOutcome::Frame {
            data: vec![0u8; 4],
            pts: Duration::ZERO,
            duration: Duration::from_millis(20),
        },
        "chunk"
    )]
    #[case::pending_signal(StubOutcome::Pending(PendingReason::SeekPending), "pending")]
    fn next_chunk_emits_signal(#[case] outcome: StubOutcome, #[case] expected_signal: &str) {
        let log = Arc::new(Mutex::new(CallLog::default()));
        let demuxer = StubDemuxer::with_outcomes(vec![outcome], Vec::new());
        let mut decoder = build(demuxer, Arc::clone(&log));
        let _ = decoder.next_chunk().unwrap();
        assert_eq!(log.lock().unwrap().chunks, vec![expected_signal]);
    }

    #[kithara::test]
    #[case::landed(
        DemuxSeekOutcome::Landed {
            landed_at: Duration::from_secs(1),
            landed_byte: Some(123),
            preroll: crate::demuxer::PrerollHint::NotNeeded,
        },
        Duration::from_secs(1),
        "landed"
    )]
    #[case::past_eof(
        DemuxSeekOutcome::PastEof {
            duration: Duration::from_secs(10),
        },
        Duration::from_secs(15),
        "past_eof"
    )]
    fn seek_emits_signal(
        #[case] outcome: DemuxSeekOutcome,
        #[case] target: Duration,
        #[case] expected_signal: &str,
    ) {
        let log = Arc::new(Mutex::new(CallLog::default()));
        let demuxer = StubDemuxer::with_outcomes(Vec::new(), vec![outcome]);
        let mut decoder = build(demuxer, Arc::clone(&log));
        let _ = decoder.seek(target).unwrap();
        assert_eq!(log.lock().unwrap().seeks, vec![expected_signal]);
    }

    #[kithara::test]
    fn owned_hooks_fire_exactly_once_per_chunk() {
        let log = Arc::new(Mutex::new(CallLog::default()));
        let demuxer = StubDemuxer::with_outcomes(
            vec![
                StubOutcome::Frame {
                    data: vec![0u8; 4],
                    pts: Duration::from_millis(40),
                    duration: Duration::from_millis(20),
                },
                StubOutcome::Frame {
                    data: vec![0u8; 4],
                    pts: Duration::from_millis(20),
                    duration: Duration::from_millis(20),
                },
                StubOutcome::Frame {
                    data: vec![0u8; 4],
                    pts: Duration::ZERO,
                    duration: Duration::from_millis(20),
                },
            ],
            Vec::new(),
        );
        let mut decoder = build(demuxer, Arc::clone(&log));

        for _ in 0..3 {
            let _ = decoder.next_chunk().unwrap();
        }
        let _ = decoder.next_chunk().unwrap();

        // WHY: single-owner Box hooks must fire once per next_chunk — three
        // PCM chunks then one Eof, never doubled, never dropped.
        assert_eq!(
            log.lock().unwrap().chunks,
            vec!["chunk", "chunk", "chunk", "eof"],
        );
    }
}

#[cfg(test)]
mod pool_budget_tests {

    use std::num::NonZeroU32;

    use kithara_bufpool::PcmPool;
    use kithara_platform::time::Duration;
    use kithara_test_utils::kithara;

    use super::test_stub_codec::ConstFrameCodec;
    use crate::{codec::FrameCodec, types::PcmSpec};

    #[kithara::test]
    fn codec_warm_pool_does_not_grow_alloc_misses() {
        let pool = PcmPool::new(32, 8192);
        for _ in 0..4 {
            let mut buf = pool.get();
            buf.ensure_len(2048).unwrap();
        }
        let warmup_misses = pool.stats().alloc_misses;

        let mut codec = ConstFrameCodec::new(
            PcmSpec::new(2, NonZeroU32::new(44_100).expect("test rate")),
            1024,
        );
        for _ in 0..200 {
            let mut buf = pool.get();
            let frames = codec
                .decode_frame(&[], Duration::ZERO, &[], &mut buf)
                .expect("BUG: decode_frame");
            assert_eq!(frames, 1024);
        }

        assert_eq!(
            pool.stats().alloc_misses,
            warmup_misses,
            "no further alloc misses expected after warm-up; \
             codec must not allocate fresh buffers per call"
        );
    }
}
