use arc_swap::ArcSwap;
use kithara_decode::PcmChunk;
use kithara_events::{AudioEvent, DeferredBus};
use kithara_platform::sync::Arc;
use kithara_stream::{Activity, PlayheadWrite, SeekControl, SeekObserve, StreamType};

pub(crate) use crate::pipeline::{
    decode::core::{DecodeCore, DecodeInit, DecoderFactory},
    stream::{offset::OffsetReader, shared::SharedStream},
};
use crate::{
    pipeline::{
        decode::{gate::ReadinessGate, resume::ResumeCursor},
        parts::SourceParts,
        rebuild::{port::RebuildPort, retire::RetiredDecoders},
        seek::SeekEngine,
        track::{self, CurrentFsm, Decoding, Track, TrackStep},
    },
    renderer::AudioWorkerSource,
};

/// Audio source for Stream with format change detection.
///
/// Monitors `media_info` changes and recreates decoder at segment boundaries.
/// The old decoder naturally decodes all data from the current segment.
/// When it encounters new segment data (different format), it errors or returns EOF.
/// At that point, we seek to the segment boundary and recreate the decoder.
pub(crate) struct StreamAudioSource<T: StreamType> {
    /// Explicit FSM state — single source of truth for track phase.
    pub(crate) state: CurrentFsm,
    pub(crate) decode: DecodeCore,
    /// Narrow activity handle — set/query the `PLAYING` flag.
    pub(crate) activity: Arc<dyn Activity>,
    pub(crate) seek_engine: SeekEngine,
    /// Narrow mutating playhead handle — committed position and total duration.
    pub(crate) playhead: Arc<dyn PlayheadWrite>,
    /// Narrow seek-control handle — begin / complete / clear-pending.
    pub(crate) seek: Arc<dyn SeekControl>,
    /// Narrow seek-observe handle — read seek state without mutation.
    pub(crate) seek_obs: Arc<dyn SeekObserve>,
    pub(crate) rebuild: RebuildPort<T>,
    /// Absolute content frame offset just past the most recently emitted chunk
    /// (the producer's decode head), tagged with its epoch. A mid-playback
    /// variant-switch recreate continues the new decoder from here — NOT from
    /// the consumer's lagging `committed_position`: the chunks in
    /// `[committed..decode_head]` are already queued in the outlet ring (a
    /// `FormatBoundary` recreate neither flushes it nor bumps the seek epoch),
    /// so resuming at `committed` would re-emit them and rewind content. Stored
    /// as an exact frame plus the sample rate of that produced chunk, then
    /// converted back with `duration_for_frames`; the demuxer quantizes the
    /// seek landing to a sample and `frame_offset_for` rounds to the nearest
    /// frame, so the rebuilt decoder relabels its first chunk at this point. See
    /// `execute_recreation`.
    pub(crate) resume: ResumeCursor,
    /// Deferred sink for FSM lifecycle events ([`AudioEvent`]). The FSM runs on
    /// the produce core, so `emit_event` enqueues lock-free; the scheduler shell
    /// flushes via [`flush_deferred`](AudioWorkerSource::flush_deferred) and on
    /// `Drop`, keeping the cross-thread `broadcast::send` (a `kevent`) off the
    /// forbid path. `None` for sources built without an event bus.
    pub(crate) emit: Option<DeferredBus<AudioEvent>>,
    pub(crate) readiness: ReadinessGate,
    /// `(seek_epoch, target)` of the most recent applied seek.
    /// `committed_position` lags `target` until the seek's first
    /// (trim-aligned) chunk is consumed: the decoder lands at the
    /// containing segment's start and trims forward, so
    /// `commit_seek_landed` records the segment boundary, not the
    /// requested instant. A variant-switch recreate firing inside that
    /// window must resume at the real target, not at the lagging
    /// committed boundary — otherwise playback rewinds to the segment
    /// start. Tagged with the seek epoch so a later seek (especially a
    /// backward one) never resumes against a stale forward target. See
    /// `execute_recreation`.
    /// Decoders displaced on the produce core. They are dropped from
    /// `flush_deferred`, outside the forbid-blocking region.
    pub(crate) retired: RetiredDecoders,
    pub(crate) shared_stream: SharedStream<T>,
}

// Construction, lifecycle, and state access
impl<T: StreamType> StreamAudioSource<T> {
    /// Bounded off-RT retire queue for decoders displaced on the produce core.
    const DECODER_RETIRE_CAPACITY: usize = 4;

    pub(crate) fn new(shared_stream: SharedStream<T>, parts: SourceParts<T>) -> Self {
        let SourceParts {
            activity,
            decode,
            playhead,
            readiness,
            rebuild,
            resume,
            seek,
            seek_engine,
            seek_obs,
        } = parts;
        activity.set_playing(true);
        Self {
            shared_stream,
            decode,
            rebuild,
            seek_engine,
            playhead,
            seek,
            seek_obs,
            activity,
            readiness,
            resume,
            state: Track::<Decoding>::new(()).erase(),
            emit: None,
            retired: RetiredDecoders::new(Self::DECODER_RETIRE_CAPACITY),
        }
    }

    pub(crate) fn with_emit(mut self, emit: DeferredBus<AudioEvent>) -> Self {
        self.emit = Some(emit);
        self
    }
    /// Publish the current FSM phase to the shared activity flag and assign
    /// the new state.
    ///
    /// `PLAYING` mirrors "audio FSM has an active decode target": every
    /// non-terminal state keeps it set (`Decoding`,
    /// `SeekRequested`, `ApplyingSeek`, `AwaitingResume`,
    /// `WaitingForSource`, `RecreatingDecoder`), while terminal states
    /// (`AtEof`, `Failed`) clear it. The Downloader's peer
    /// `priority()` reads this flag to decide between High and Low
    /// priority slots — keeping PLAYING set through buffering and
    /// mid-seek windows is deliberate, because the listener is still
    /// attached to this track.
    pub(crate) fn update_state(&mut self, new: CurrentFsm) {
        self.activity.set_playing(playing_for_state(&new));
        self.state = new;
    }
}

impl<T: StreamType> Drop for StreamAudioSource<T> {
    fn drop(&mut self) {
        // Publish any lifecycle event enqueued on the final produce pass before
        // the terminal node is dropped — `scheduler::run_loop` removes a
        // removable slot via `retain` without another `flush_deferred`, so a
        // terminal `EndOfStream` would otherwise be lost. Runs in the unchecked
        // shell (retain is outside `produce_pass`), off the forbid path.
        if let Some(ref emit) = self.emit {
            emit.flush();
        }
        self.retired.drain();
    }
}

impl<T: StreamType> AudioWorkerSource for StreamAudioSource<T> {
    type Chunk = PcmChunk;

    fn decode_epoch(&self) -> u64 {
        // The epoch the current decode belongs to — stored when a seek is
        // applied (`ApplyingSeek` / `try_apply_seek`), and the same value
        // stamped on produced chunks (`decode_one_step`). It LAGS
        // `timeline().seek_epoch()`, which the consumer bumps the instant it
        // requests a seek, long before the worker applies it. A terminal
        // marker (EOF / failure) must carry this decode epoch so a stale
        // end-of-stream produced for a superseded seek is discarded by the
        // consumer's validator rather than mistaken for the new seek's
        // terminal (the oversubscription false-EOF race).
        self.seek_engine.epoch()
    }

    fn flush_deferred(&mut self) {
        self.decode.flush_reader_signals();
        self.retired.drain();
        self.rebuild.submit();
        // Publish the FSM lifecycle events the produce core enqueued this pass,
        // off the forbid path (the `broadcast::send` is a `kevent`).
        if let Some(ref emit) = self.emit {
            emit.flush();
        }
        // Deliver the peer wake the produce core armed this pass (a blocked
        // `probe_read`, a seek-apply / finalize). The `notify_one` is a
        // cross-thread `kevent` the forbid-blocking core must not make, so it
        // lands here in the shell. Same `Arc<DeferredWake>` the reader drivers
        // and the FSM arm, so one flush covers both. `None` for file streams.
        self.readiness.flush_peer_wake();
    }

    fn seek_observe(&self) -> Arc<dyn SeekObserve> {
        Arc::clone(&self.seek_obs)
    }

    fn step_track(&mut self) -> TrackStep<PcmChunk> {
        track::dispatch(self)
    }

    fn warm_up(&mut self) {
        // The storage committed-read fast path (`MemDriver::committed_len` /
        // `read_committed` behind an `arc_swap::ArcSwapOption`) lazily
        // `Box`-allocates this thread's `arc_swap` debt node on its FIRST load.
        // Left to the produce core, that one-time alloc lands inside the
        // forbid-blocking region (the first committed `len`/`contains_range`/
        // read after the resource opens). The debt node is process-global per
        // thread and shared by every `ArcSwap` regardless of payload type, so a
        // throwaway load here — in the scheduler shell, before any checked
        // `tick` — allocates it off the RT path and warms every real storage
        // read. It is resource-independent, so it works even before this
        // source's resource has been opened (the `len()` path only reaches
        // `committed_len` once the resource is live).
        let warm = ArcSwap::from_pointee(());
        let _ = warm.load();
        let _ = self.shared_stream.len();
    }
}

/// Classify a [`CurrentFsm`] phase for the shared activity `PLAYING` flag.
///
/// The Downloader peers read `Activity::is_playing()` in their
/// `priority()` method. Every non-terminal phase keeps this track
/// "listened to" from the user's perspective — buffering, seek-in-
/// progress, and decoder recreation are all transient windows inside
/// an otherwise-active track. Only `AtEof` (natural end) and `Failed`
/// (terminal error) clear the flag.
pub(crate) fn playing_for_state(state: &CurrentFsm) -> bool {
    !matches!(state, CurrentFsm::AtEof(_) | CurrentFsm::Failed(_))
}

#[cfg(test)]
mod resolve_format_change_target_tests {
    use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};
    use kithara_test_utils::kithara;

    use crate::pipeline::decode::format::resolve_target;

    fn info(
        codec: Option<AudioCodec>,
        container: Option<ContainerFormat>,
        variant: Option<u32>,
    ) -> MediaInfo {
        let mut info = MediaInfo::new(codec, container);
        info.variant_index = variant;
        info
    }

    #[kithara::test]
    fn no_change_when_variant_index_matches() {
        let cached = info(
            Some(AudioCodec::AacLc),
            Some(ContainerFormat::Fmp4),
            Some(0),
        );
        let current = info(
            Some(AudioCodec::AacLc),
            Some(ContainerFormat::Fmp4),
            Some(0),
        );
        assert!(resolve_target(Some(&cached), &current).is_none());
    }

    #[kithara::test]
    fn same_codec_fmp4_variant_change_recreates_boundary() {
        let cached = info(
            Some(AudioCodec::AacLc),
            Some(ContainerFormat::Fmp4),
            Some(0),
        );
        let current = info(
            Some(AudioCodec::AacLc),
            Some(ContainerFormat::Fmp4),
            Some(1),
        );
        let target = resolve_target(Some(&cached), &current)
            .expect("same-codec fMP4 variant change must re-prime the demuxer");
        assert_eq!(target.variant_index, Some(1));
        assert_eq!(target.codec, Some(AudioCodec::AacLc));
        assert_eq!(target.container, Some(ContainerFormat::Fmp4));
    }

    #[kithara::test]
    fn same_codec_wav_variant_change_is_byte_continuity() {
        let cached = info(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav), Some(0));
        let current = info(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav), Some(1));
        assert!(resolve_target(Some(&cached), &current).is_none());
    }

    #[kithara::test]
    fn variant_change_keeps_cached_codec_and_container_when_current_disagrees() {
        let cached = info(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav), Some(0));
        let current = info(None, Some(ContainerFormat::Fmp4), Some(1));
        let target = resolve_target(Some(&cached), &current).expect("variant change must trigger");
        assert_eq!(target.codec, Some(AudioCodec::Pcm));
        assert_eq!(target.container, Some(ContainerFormat::Wav));
        assert_eq!(target.variant_index, Some(1));
    }

    #[kithara::test]
    fn variant_change_falls_back_to_current_when_cached_lacks_codec_or_container() {
        let cached = info(None, None, Some(0));
        let current = info(
            Some(AudioCodec::AacLc),
            Some(ContainerFormat::Fmp4),
            Some(2),
        );
        let target = resolve_target(Some(&cached), &current).expect("variant change must trigger");
        assert_eq!(target.codec, Some(AudioCodec::AacLc));
        assert_eq!(target.container, Some(ContainerFormat::Fmp4));
        assert_eq!(target.variant_index, Some(2));
    }

    #[kithara::test]
    fn no_cached_uses_current_directly() {
        let current = info(
            Some(AudioCodec::AacLc),
            Some(ContainerFormat::Fmp4),
            Some(1),
        );
        let target =
            resolve_target(None, &current).expect("None cached + Some(variant) must trigger");
        assert_eq!(target, current);
    }

    #[kithara::test]
    fn explicit_codec_change_takes_current_codec() {
        let cached = info(Some(AudioCodec::AacLc), Some(ContainerFormat::Fmp4), None);
        let current = info(Some(AudioCodec::Flac), Some(ContainerFormat::Fmp4), None);
        let target = resolve_target(Some(&cached), &current).expect("codec change must trigger");
        assert_eq!(target.codec, Some(AudioCodec::Flac));
        assert_eq!(target.container, Some(ContainerFormat::Fmp4));
    }

    #[kithara::test]
    fn current_codec_none_is_not_a_codec_change() {
        let cached = info(
            Some(AudioCodec::AacLc),
            Some(ContainerFormat::Fmp4),
            Some(0),
        );
        let current = info(None, Some(ContainerFormat::Fmp4), Some(0));
        assert!(resolve_target(Some(&cached), &current).is_none());
    }

    #[kithara::test]
    fn no_change_when_neither_side_has_variant() {
        let cached = info(Some(AudioCodec::AacLc), Some(ContainerFormat::Fmp4), None);
        let current = info(Some(AudioCodec::AacLc), Some(ContainerFormat::Fmp4), None);
        assert!(resolve_target(Some(&cached), &current).is_none());
    }
}
