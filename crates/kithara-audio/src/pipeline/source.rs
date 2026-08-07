use arc_swap::ArcSwap;
use kithara_decode::{ChunkRetire, PcmChunk};
use kithara_events::{AudioEvent, DecoderChangeCause, DeferredBus, Event, TrackFailureKind};
use kithara_platform::sync::Arc;
use kithara_stream::{
    Activity, OpenedVariantReader, PlayheadWrite, SeekControl, SeekObserve, StreamType,
    VariantControl, VariantPromotion, VariantReaderTake, VariantTransition,
};
use tracing::{trace, warn};

pub(crate) use crate::pipeline::{
    decode::{
        core::{ActiveDecode, DecodeInit, DecoderFactory},
        event::{GenerationInstalled, enqueue_generation_installed},
    },
    stream::shared::SharedStream,
};
use crate::{
    pipeline::{
        decode::{
            gate::ReadinessGate,
            resume::ResumeCursor,
            transition::{IncomingPrime, OutgoingFrontier},
        },
        parts::SourceParts,
        rebuild::{DecoderBuildComplete, DecoderBuildPurpose, port::RebuildPort, retire::Retired},
        seek::SeekEngine,
        track::{self, CurrentFsm, Decoding, Track, TrackStep, WaitContext},
    },
    renderer::AudioWorkerSource,
};

/// Audio source for Stream with format change detection.
///
/// Monitors `media_info` changes and recreates decoder at segment boundaries.
/// The old decoder naturally decodes all data from the current segment.
/// When it encounters new segment data (different format), it errors or returns EOF.
/// At that point, we seek to the segment boundary and recreate the decoder.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, with)]
pub(crate) struct StreamAudioSource<T: StreamType> {
    pub(crate) playback_resampler_backend: &'static str,
    pub(crate) decode: ActiveDecode,
    /// Narrow activity handle — set/query the `PLAYING` flag.
    pub(crate) activity: Arc<dyn Activity>,
    /// Narrow mutating playhead handle — committed position and total duration.
    pub(crate) playhead: Arc<dyn PlayheadWrite>,
    /// Narrow seek-control handle — begin / complete / clear-pending.
    pub(crate) seek: Arc<dyn SeekControl>,
    /// Narrow seek-observe handle — read seek state without mutation.
    pub(crate) seek_obs: Arc<dyn SeekObserve>,
    /// Explicit FSM state — single source of truth for track phase.
    pub(crate) state: CurrentFsm,
    pub(crate) decoder_backend: kithara_decode::DecoderBackend,
    /// Deferred sink for FSM lifecycle events ([`AudioEvent`]). The FSM runs on
    /// the produce core, so `emit_event` enqueues lock-free; the scheduler shell
    /// flushes via [`flush_deferred`](AudioWorkerSource::flush_deferred) and on
    /// `Drop`, keeping the cross-thread `broadcast::send` (a `kevent`) off the
    /// forbid path. `None` for sources built without an event bus.
    #[field(with, option_set_some, vis = "pub(crate)")]
    pub(crate) emit: Option<Arc<DeferredBus<Event>>>,
    pub(crate) variant_control: Option<Arc<dyn VariantControl>>,
    pub(crate) readiness: ReadinessGate,
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
    /// Decode generations displaced on the produce core. They are dropped
    /// from `flush_deferred`, outside the forbid-blocking region.
    pub(crate) retired: Retired,
    pub(crate) seek_engine: SeekEngine,
    pub(crate) shared_stream: SharedStream<T>,
}

// Construction, lifecycle, and state access
impl<T: StreamType> StreamAudioSource<T> {
    /// Bounded off-RT retire queue for decode state displaced on the produce core.
    const GENERATION_RETIRE_CAPACITY: usize = 4;

    const CHUNK_RETIRE_CAPACITY: usize = 64;

    pub(crate) fn new(shared_stream: SharedStream<T>, parts: SourceParts<T>) -> Self {
        let SourceParts {
            activity,
            decode,
            decoder_backend,
            playhead,
            playback_resampler_backend,
            readiness,
            rebuild,
            resume,
            seek,
            seek_engine,
            seek_obs,
            variant_control,
        } = parts;
        activity.set_playing(true);
        Self {
            shared_stream,
            decode,
            decoder_backend,
            rebuild,
            seek_engine,
            playhead,
            playback_resampler_backend,
            seek,
            seek_obs,
            activity,
            readiness,
            resume,
            variant_control,
            state: Track::<Decoding>::new(()).erase(),
            emit: None,
            retired: Retired::new(
                Self::GENERATION_RETIRE_CAPACITY,
                Self::CHUNK_RETIRE_CAPACITY,
            ),
        }
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
        if let CurrentFsm::Failed(handle) = &new
            && let Some(ref emit) = self.emit
        {
            emit.enqueue(
                AudioEvent::TrackFailed {
                    failure: map_track_failure_kind(handle.data()),
                    seek_epoch: self.seek_obs.epoch(),
                }
                .into(),
            );
        }
        self.activity.set_playing(playing_for_state(&new));
        self.state = new;
    }
}

impl<T: StreamType> Drop for StreamAudioSource<T> {
    fn drop(&mut self) {
        // A failed node can be removed before another deferred pass.
        if matches!(self.state, CurrentFsm::AtEof(_) | CurrentFsm::Failed(_)) {
            self.progress_variant_transition();
        }
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

impl<T: StreamType> StreamAudioSource<T> {
    fn abort_local_incoming(
        &mut self,
        control: &dyn VariantControl,
        transition: VariantTransition,
    ) {
        self.discard_local_incoming();
        let _ = control.abort_variant(transition);
    }

    fn discard_local_incoming(&mut self) {
        if let Some(generation) = self.decode.discard_incoming() {
            self.retired.retire_generation(generation);
        }
    }

    fn prepare_incoming_transition(
        &mut self,
        control: &dyn VariantControl,
        outgoing_frontier: OutgoingFrontier,
    ) -> Option<VariantTransition> {
        let plan = match control.plan_variant_reader(self.decode.landing_for(outgoing_frontier)) {
            Ok(plan) => plan,
            Err(error) => {
                warn!(?error, "failed to plan exact incoming variant reader");
                if let Some(transition) = self.decode.incoming_transition() {
                    self.abort_local_incoming(control, transition);
                }
                return None;
            }
        };
        let Some(plan) = plan else {
            self.discard_local_incoming();
            return None;
        };
        let transition = plan.transition();
        if self.decode.incoming_transition() != Some(transition)
            && let Some(generation) = self.decode.begin_incoming(transition)
        {
            self.retired.retire_generation(generation);
        }
        if !self.decode.incoming_is_preparing(transition) || !self.rebuild.can_prepare() {
            return None;
        }

        let byte_map = self.shared_stream.byte_map();
        let profile = self
            .rebuild
            .reader_profile(plan.media_info(), byte_map.as_deref());
        match control.prepare_variant_reader(plan, profile) {
            Ok(Some(prepared)) if prepared == transition => Some(transition),
            Ok(Some(prepared)) => {
                warn!(
                    ?transition,
                    ?prepared,
                    "source prepared a different exact variant transition"
                );
                self.abort_local_incoming(control, transition);
                None
            }
            Ok(None) => {
                self.discard_local_incoming();
                None
            }
            Err(error) => {
                warn!(
                    ?error,
                    ?transition,
                    "failed to prepare exact incoming reader"
                );
                self.abort_local_incoming(control, transition);
                None
            }
        }
    }

    fn progress_variant_transition(&mut self) {
        // A reader starved on a variant that stopped delivering parks in
        // `WaitingForSource` with the active decoder intact and only source
        // bytes missing — the exact state an urgent down-switch exists to
        // leave, so the transition has to keep advancing through it. The seek
        // and recreate phases stay excluded: they are about to replace the very
        // decoder a promotion would install into.
        let outgoing_unavailable = match &self.state {
            CurrentFsm::Decoding(_) => false,
            CurrentFsm::WaitingForSource(track) => {
                if matches!(track.data().context, WaitContext::Playback) {
                    true
                } else {
                    return;
                }
            }
            CurrentFsm::AtEof(_) | CurrentFsm::Failed(_) => {
                if let (Some(control), Some(transition)) = (
                    self.variant_control.clone(),
                    self.decode.incoming_transition(),
                ) {
                    self.abort_local_incoming(control.as_ref(), transition);
                }
                return;
            }
            _ => return,
        };
        let Some(control) = self.variant_control.clone() else {
            return;
        };

        let outgoing_frontier = match self.resume.decode_head(self.seek_obs.epoch()) {
            Some((frame, rate)) => OutgoingFrontier::Exact { frame, rate },
            None if outgoing_unavailable => OutgoingFrontier::Unavailable,
            None => OutgoingFrontier::Awaiting,
        };
        self.retire_failed_incoming(control.as_ref());
        // Priming is bounded per pass and may mint the overlap proof consumed
        // immediately below. A publication lock leaves both generations intact
        // and the next pass extends the staged range to the newer frontier.
        let prime = self.decode.prime_incoming(outgoing_frontier);
        if let Some(incoming) = self.decode.incoming_transition() {
            // The frontier and the prime outcome together are the only thing
            // that separates "the incoming is still staging" from "the splice
            // has nothing to land against": both look identical from outside as
            // a switch that simply never commits.
            trace!(
                ?outgoing_frontier,
                ?prime,
                ?incoming,
                "variant transition pass"
            );
        }
        if prime == IncomingPrime::Advanced {
            self.rebuild.wake();
        }
        if !self.promote_ready_incoming(control.as_ref(), outgoing_frontier) {
            return;
        }
        let Some(transition) =
            self.prepare_incoming_transition(control.as_ref(), outgoing_frontier)
        else {
            return;
        };
        self.take_prepared_incoming(control.as_ref(), transition);
    }

    fn promote_ready_incoming(
        &mut self,
        control: &dyn VariantControl,
        outgoing_frontier: OutgoingFrontier,
    ) -> bool {
        let Some(proof) = self.decode.ready_to_promote(outgoing_frontier) else {
            return true;
        };
        match control.promote_variant(proof.transition()) {
            VariantPromotion::Promoted => {
                if let Some(outgoing) = self.decode.promote_incoming(proof) {
                    if let Some(ref emit) = self.emit {
                        enqueue_generation_installed(
                            emit,
                            &GenerationInstalled {
                                backend: self.decoder_backend,
                                cause: DecoderChangeCause::VariantSwitch,
                                epoch: self.seek_obs.epoch(),
                                generation: self.decode.active(),
                                host_sample_rate: self.resume.host_rate(),
                                playback_resampler_backend: self.playback_resampler_backend,
                                recreates_on_route: self.resume.recreates_on_route(),
                            },
                        );
                    }
                    self.retired.retire_generation(outgoing);
                } else {
                    warn!(
                        transition = ?proof.transition(),
                        "source promoted a variant without matching primed audio"
                    );
                }
                true
            }
            VariantPromotion::Deferred => false,
            VariantPromotion::Stale => {
                self.discard_local_incoming();
                true
            }
            _ => {
                warn!(
                    transition = ?proof.transition(),
                    "source returned an unsupported variant promotion result"
                );
                false
            }
        }
    }

    fn retire_failed_incoming(&mut self, control: &dyn VariantControl) {
        if let Some((transition, generation)) = self.decode.take_failed_incoming() {
            self.retired.retire_generation(generation);
            let _ = control.abort_variant(transition);
        }
    }

    fn route_build_completions(&mut self) {
        while let Some(complete) = self
            .rebuild
            .pop_replacement_completion()
            .or_else(|| self.rebuild.pop_incoming_completion())
        {
            match complete.purpose {
                DecoderBuildPurpose::Replacement => {
                    let expected = match &self.state {
                        CurrentFsm::RebuildingDecoder(handle) => Some(handle.data().build),
                        _ => None,
                    };
                    if expected == Some(complete.build) {
                        if let Some(displaced) = self.rebuild.cache_replacement(complete) {
                            retire_completion(&self.retired, displaced);
                        }
                    } else {
                        retire_completion(&self.retired, complete);
                    }
                }
                DecoderBuildPurpose::Incoming(transition) => match complete.result {
                    Ok(generation) => {
                        if let Some(generation) =
                            self.decode
                                .install_incoming(transition, complete.build, generation)
                        {
                            self.retired.retire_generation(generation);
                        }
                    }
                    Err(outcome) => {
                        warn!(
                            ?transition,
                            ?outcome,
                            "incoming decoder build failed; aborting variant transition"
                        );
                        if self.decode.incoming_transition() == Some(transition) {
                            if let Some(generation) = self.decode.discard_incoming() {
                                self.retired.retire_generation(generation);
                            }
                            if let Some(ref control) = self.variant_control {
                                let _ = control.abort_variant(transition);
                            }
                        }
                    }
                },
            }
        }
    }

    fn start_incoming_build(
        &mut self,
        control: &dyn VariantControl,
        transition: VariantTransition,
        reader: OpenedVariantReader,
    ) {
        match self.rebuild.prepare_incoming(reader) {
            Some((prepared, build))
                if prepared == transition
                    && self.decode.mark_incoming_building(transition, build) => {}
            Some((prepared, _)) => {
                warn!(
                    ?transition,
                    ?prepared,
                    "incoming decoder build lost its exact transition owner"
                );
                self.abort_local_incoming(control, transition);
            }
            None => {
                warn!(
                    ?transition,
                    "incoming decoder build port was not available after reader transfer"
                );
                self.abort_local_incoming(control, transition);
            }
        }
    }

    fn take_prepared_incoming(
        &mut self,
        control: &dyn VariantControl,
        transition: VariantTransition,
    ) {
        match control.take_prepared_variant_reader(transition) {
            Ok(VariantReaderTake::Preparing) => {}
            Ok(VariantReaderTake::Ready(reader)) => {
                self.start_incoming_build(control, transition, reader);
            }
            Ok(VariantReaderTake::Taken) => {
                warn!(
                    ?transition,
                    "incoming reader was transferred without a matching decoder build"
                );
                self.abort_local_incoming(control, transition);
            }
            Ok(VariantReaderTake::Stale) => self.discard_local_incoming(),
            Err(error) => {
                warn!(?error, ?transition, "failed to take exact incoming reader");
                self.abort_local_incoming(control, transition);
            }
            Ok(_) => {
                warn!(
                    ?transition,
                    "source returned an unsupported incoming reader state"
                );
                self.abort_local_incoming(control, transition);
            }
        }
    }
}

fn retire_completion(retired: &Retired, complete: DecoderBuildComplete) {
    if let Ok(generation) = complete.result {
        retired.retire_generation(generation);
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
        self.route_build_completions();
        self.progress_variant_transition();
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

    fn retire_chunk(&self, chunk: PcmChunk) {
        ChunkRetire::retire(&self.retired, chunk);
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

fn map_track_failure_kind(failure: &track::TrackFailure) -> TrackFailureKind {
    match failure {
        track::TrackFailure::Decode(_) => TrackFailureKind::Decode,
        track::TrackFailure::RecreateFailed { offset } => {
            TrackFailureKind::RecreateFailed { offset: *offset }
        }
        track::TrackFailure::SourceCancelled => TrackFailureKind::SourceCancelled,
    }
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
        let mut info = MediaInfo::builder()
            .maybe_codec(codec)
            .maybe_container(container)
            .build();
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
