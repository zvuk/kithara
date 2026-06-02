use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use kithara_bufpool::{PcmBuf, PcmPool};
use kithara_stream::{AudioCodec, BoxedEventSink, ReaderChunkSignal, ReaderSeekSignal};
use kithara_test_utils::kithara;

use crate::{
    codec::FrameCodec,
    demuxer::{DemuxOutcome, DemuxSeekOutcome, Demuxer},
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

impl<D: Demuxer, C: FrameCodec> ComposedDecoder<D, C> {
    /// Build a decoder from a `(demuxer, codec)` pair.
    pub(crate) fn new(
        demuxer: D,
        codec: C,
        pool: PcmPool,
        epoch: u64,
        byte_len_handle: Option<Arc<AtomicU64>>,
        hooks: Option<BoxedEventSink>,
    ) -> Self {
        let spec = codec.spec();
        let duration = demuxer.duration();
        Self {
            demuxer,
            codec,
            spec,
            duration,
            pool,
            epoch,
            byte_len_handle,
            hooks,
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
                DemuxOutcome::Eof => return Ok(DecoderChunkOutcome::Eof),
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
                // WHY: frame straddles target — trim leading samples (README "Seek trim").
                if frame_pts < target && frames > 0 {
                    let live_spec = self.codec.spec();
                    let trim_frames_u64 =
                        frames_to_trim(frame_pts, target, live_spec.sample_rate.get())
                            .min(u64::from(frames));
                    let trim_frames = u32::try_from(trim_frames_u64).unwrap_or(frames);
                    if trim_frames > 0 && trim_frames < frames {
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

    fn flush_reader_signals(&mut self) {
        if let Some(hooks) = self.hooks.as_mut() {
            hooks.flush();
        }
    }
}

#[cfg(all(test, feature = "symphonia"))]
mod default_priming_tests {
    use std::io::Cursor;

    use kithara_stream::AudioCodec;
    use symphonia::core::{
        formats::{FormatOptions, probe::Hint},
        io::{MediaSourceStream, MediaSourceStreamOptions},
        meta::MetadataOptions,
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
        let format_reader = symphonia::default::get_probe()
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
        ComposedDecoder::new(demuxer, codec, PcmPool::default().clone(), 0, None, None)
    }

    #[kithara::test]
    fn composed_decoder_priming_combines_encoder_and_symphonia_mp3_algo_delay() {
        let decoder = build_mp3_decoder();
        // WHY: 1105 = 576 libmp3lame priming + 529 LAME algo delay (README "Gapless probe contract").
        assert_eq!(decoder.default_priming_frames(AudioCodec::Mp3), 1105);
        assert_eq!(decoder.default_priming_frames(AudioCodec::AacLc), 1024);
        assert_eq!(decoder.default_priming_frames(AudioCodec::Opus), 312);
        assert_eq!(decoder.default_priming_frames(AudioCodec::Flac), 0);
    }
}

fn frame_offset_for(at: Duration, sample_rate: u32) -> u64 {
    let secs = at.as_secs();
    let subsec_frames = u64::from(at.subsec_nanos()) * u64::from(sample_rate) / 1_000_000_000;
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

    use kithara_bufpool::PcmPool;
    use kithara_stream::AudioCodec;
    use kithara_test_utils::kithara;
    use symphonia::core::{
        formats::{FormatOptions, probe::Hint},
        io::{MediaSourceStream, MediaSourceStreamOptions},
        meta::MetadataOptions,
    };

    use super::*;
    use crate::{
        symphonia::{SymphoniaCodec, SymphoniaConfig, SymphoniaDemuxer},
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
        let format_reader = symphonia::default::get_probe()
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
        let mut decoder =
            ComposedDecoder::new(demuxer, codec, PcmPool::default().clone(), 0, None, None);

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
        let mut decoder =
            ComposedDecoder::new(demuxer, codec, PcmPool::default().clone(), 0, None, None);

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
            Some("mp3".into()),
            None,
            None,
            None,
        )
        .expect("BUG: open_file must succeed");
        let track_info = demuxer.track_info().clone();
        let codec = SymphoniaCodec::open_with_config(&track_info, &SymphoniaConfig::default())
            .expect("BUG: MP3 codec should open");
        let mut decoder =
            ComposedDecoder::new(demuxer, codec, PcmPool::default().clone(), 0, None, None);

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
            let samples = self.frames_per_call as usize * self.spec.channels as usize;
            out.ensure_len(samples)?;
            for slot in out.iter_mut() {
                *slot = 0.0;
            }
            out.truncate(samples);
            Ok(self.frames_per_call)
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
            let samples = self.frames_per_call as usize * self.spec.channels as usize;
            out.ensure_len(samples)?;
            for slot in out.iter_mut() {
                *slot = 0.0;
            }
            out.truncate(samples);
            Ok(self.frames_per_call)
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
mod seek_trim_tests {

    use std::{
        num::NonZeroU32,
        sync::{Arc, atomic::Ordering},
    };

    use kithara_bufpool::PcmPool;
    use kithara_platform::time::Duration;
    use kithara_stream::AudioCodec;
    use kithara_test_utils::kithara;

    use super::{test_counting_codec::CountingCodec, *};
    use crate::{
        demuxer::{DemuxOutcome, DemuxSeekOutcome, Frame, TrackInfo},
        traits::Decoder,
    };

    struct ThreeFrameDemuxer {
        track: TrackInfo,
        held: Vec<u8>,
        idx: usize,
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
    fn pre_target_frames_are_decoded_and_dropped_by_pending_seek_target() {
        let codec = CountingCodec::new(
            PcmSpec::new(2, NonZeroU32::new(44_100).expect("test rate")),
            1,
        );
        let calls = Arc::clone(&codec.decode_calls);
        let demuxer = ThreeFrameDemuxer {
            track: empty_track(),
            idx: 0,
            held: Vec::new(),
        };
        let mut decoder =
            ComposedDecoder::new(demuxer, codec, PcmPool::default().clone(), 0, None, None);

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
}

#[cfg(test)]
mod hook_tests {

    use std::{
        num::NonZeroU32,
        sync::{Arc, Mutex},
    };

    use kithara_bufpool::PcmPool;
    use kithara_stream::{
        BoxedEventSink, PendingReason, ReaderChunkSignal, ReaderEventSink, ReaderSeekSignal,
    };
    use kithara_test_utils::kithara;

    use super::*;
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

    use super::test_stub_codec::ConstFrameCodec;

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
            PcmPool::default().clone(),
            0,
            None,
            Some(hooks),
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
