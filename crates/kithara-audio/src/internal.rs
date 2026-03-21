#![forbid(unsafe_code)]

pub mod audio {
    use std::sync::Arc;

    use kithara_platform::tokio::sync::Notify;

    pub use crate::{AudioConfig, audio::Audio};

    impl<S> Audio<S> {
        #[must_use]
        pub fn test_has_current_chunk(&self) -> bool {
            self.current_chunk.is_some()
        }

        #[must_use]
        pub fn test_preload_notify(&self) -> Arc<Notify> {
            Arc::clone(&self.preload_notify)
        }

        #[must_use]
        pub fn test_seek_epoch(&self) -> u64 {
            self.validator.epoch
        }
    }

    #[must_use]
    pub fn has_current_chunk<S>(audio: &Audio<S>) -> bool {
        audio.test_has_current_chunk()
    }

    #[must_use]
    pub fn preload_notify<S>(audio: &Audio<S>) -> Arc<Notify> {
        audio.test_preload_notify()
    }

    #[must_use]
    pub fn seek_epoch<S>(audio: &Audio<S>) -> u64 {
        audio.test_seek_epoch()
    }

    #[must_use]
    pub fn is_preloaded<S>(audio: &Audio<S>) -> bool {
        audio.is_preloaded()
    }
}

pub mod source {
    use std::{
        io::{self, Read, Seek, SeekFrom},
        sync::{Arc, atomic::AtomicU64},
        time::Duration,
    };

    use delegate::delegate;
    use kithara_decode::{InnerDecoder, PcmChunk};
    use kithara_stream::{Fetch, MediaInfo, Stream, StreamType, Timeline};

    pub use crate::pipeline::track_fsm::{TrackPhaseTag, TrackStep, WaitingReason};
    use crate::{
        pipeline::track_fsm::{
            RecreateCause, RecreateNext, RecreateState, ResumeState, SeekContext, SeekRequest,
            TrackState, WaitContext,
        },
        traits::AudioEffect,
        worker::AudioWorkerSource,
    };

    pub struct SharedStream<T: StreamType>(crate::pipeline::source::SharedStream<T>);

    impl<T: StreamType> SharedStream<T> {
        fn into_inner(self) -> crate::pipeline::source::SharedStream<T> {
            self.0
        }
    }

    impl<T: StreamType> Clone for SharedStream<T> {
        fn clone(&self) -> Self {
            Self(self.0.clone())
        }
    }

    impl<T: StreamType> Read for SharedStream<T> {
        delegate! {
            to self.0 {
                fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
            }
        }
    }

    impl<T: StreamType> Seek for SharedStream<T> {
        delegate! {
            to self.0 {
                fn seek(&mut self, pos: SeekFrom) -> io::Result<u64>;
            }
        }
    }

    pub struct OffsetReader<T: StreamType>(crate::pipeline::source::OffsetReader<T>);

    impl<T: StreamType> Read for OffsetReader<T> {
        delegate! {
            to self.0 {
                fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
            }
        }
    }

    impl<T: StreamType> Seek for OffsetReader<T> {
        delegate! {
            to self.0 {
                fn seek(&mut self, pos: SeekFrom) -> io::Result<u64>;
            }
        }
    }

    pub type DecoderFactory<T> =
        Box<dyn Fn(SharedStream<T>, &MediaInfo, u64) -> Option<Box<dyn InnerDecoder>> + Send>;

    pub struct StreamAudioSource<T: StreamType>(crate::pipeline::source::StreamAudioSource<T>);

    #[must_use]
    pub fn new_offset_reader<T: StreamType>(
        shared: SharedStream<T>,
        base_offset: u64,
    ) -> OffsetReader<T> {
        OffsetReader(crate::pipeline::source::OffsetReader::new(
            shared.into_inner(),
            base_offset,
        ))
    }

    pub fn new_shared_stream<T: StreamType>(stream: Stream<T>) -> SharedStream<T> {
        SharedStream(crate::pipeline::source::SharedStream::new(stream))
    }

    pub fn new_stream_audio_source<T: StreamType>(
        shared_stream: SharedStream<T>,
        decoder: Box<dyn InnerDecoder>,
        decoder_factory: DecoderFactory<T>,
        initial_media_info: Option<MediaInfo>,
        epoch: Arc<AtomicU64>,
        effects: Vec<Box<dyn AudioEffect>>,
    ) -> StreamAudioSource<T> {
        let inner_factory: crate::pipeline::source::DecoderFactory<T> =
            Box::new(move |shared, media_info, base_offset| {
                decoder_factory(SharedStream(shared), media_info, base_offset)
            });
        StreamAudioSource(crate::pipeline::source::StreamAudioSource::new(
            shared_stream.into_inner(),
            decoder,
            inner_factory,
            initial_media_info,
            epoch,
            effects,
        ))
    }

    pub fn step_track<T: StreamType>(source: &mut StreamAudioSource<T>) -> TrackStep<PcmChunk> {
        AudioWorkerSource::step_track(&mut source.0)
    }

    pub fn clear_variant_fence<T: StreamType>(shared: &SharedStream<T>) {
        shared.0.clear_variant_fence();
    }

    #[must_use]
    pub fn current_base_offset<T: StreamType>(source: &StreamAudioSource<T>) -> u64 {
        source.0.session.base_offset
    }

    #[must_use]
    pub fn timeline<T: StreamType>(source: &StreamAudioSource<T>) -> &Timeline {
        &source.0.timeline
    }

    #[must_use]
    pub fn track_state<T: StreamType>(source: &StreamAudioSource<T>) -> TrackPhaseTag {
        source.0.state.phase_tag()
    }

    // Test compatibility helpers
    // These wrap step_track() to provide the old fetch_next/apply_pending_seek
    // calling patterns used by integration tests.

    /// Drive the FSM until it produces a fetch or reaches a terminal/blocked state.
    ///
    /// Replaces the old `fetch_next` — loops `step_track()` internally.
    pub fn fetch_next<T: StreamType>(source: &mut StreamAudioSource<T>) -> Fetch<PcmChunk> {
        loop {
            match step_track(source) {
                TrackStep::Produced(fetch) => return fetch,
                TrackStep::StateChanged => continue,
                TrackStep::Blocked(_) | TrackStep::Eof | TrackStep::Failed => {
                    let epoch = timeline(source).seek_epoch();
                    return Fetch::new(PcmChunk::default(), true, epoch);
                }
            }
        }
    }

    /// Drive the FSM to process a pending seek.
    ///
    /// Replaces the old `apply_pending_seek` — loops `step_track()` until the
    /// seek is fully applied. Stops once the FSM exits `SeekRequested` state,
    /// meaning the seek was applied (or failed/blocked). Does NOT continue
    /// decoding — callers use `fetch_next` separately for that.
    pub fn apply_pending_seek<T: StreamType>(source: &mut StreamAudioSource<T>) {
        loop {
            let result = step_track(source);
            match result {
                TrackStep::StateChanged => {
                    let phase = track_state(source);
                    if !matches!(
                        phase,
                        TrackPhaseTag::SeekRequested
                            | TrackPhaseTag::ApplyingSeek
                            | TrackPhaseTag::RecreatingDecoder
                    ) {
                        return;
                    }
                }
                _ => return,
            }
        }
    }

    // Test-only state accessors (overlay fields still present)

    pub fn set_base_offset<T: StreamType>(source: &mut StreamAudioSource<T>, base_offset: u64) {
        source.0.session.base_offset = base_offset;
    }

    pub fn set_cached_media_info<T: StreamType>(
        source: &mut StreamAudioSource<T>,
        media_info: Option<MediaInfo>,
    ) {
        source.0.session.media_info = media_info;
    }

    pub fn set_awaiting_resume_state<T: StreamType>(
        source: &mut StreamAudioSource<T>,
        epoch: u64,
        target: Duration,
        recover_attempts: u8,
        skip: Option<Duration>,
    ) {
        source.0.state = TrackState::AwaitingResume(ResumeState {
            recover_attempts,
            seek: SeekContext { epoch, target },
            skip,
            anchor_offset: None,
        });
    }

    pub fn set_recreating_decoder<T: StreamType>(
        source: &mut StreamAudioSource<T>,
        epoch: u64,
        target: Duration,
        media_info: MediaInfo,
        offset: u64,
    ) {
        source.0.state = TrackState::RecreatingDecoder(RecreateState {
            attempt: 0,
            cause: RecreateCause::VariantSwitch,
            media_info,
            next: RecreateNext::Seek(SeekRequest {
                attempt: 0,
                seek: SeekContext { epoch, target },
            }),
            offset,
        });
    }

    pub fn set_waiting_recreation<T: StreamType>(
        source: &mut StreamAudioSource<T>,
        epoch: u64,
        target: Duration,
        media_info: MediaInfo,
        offset: u64,
        reason: WaitingReason,
    ) {
        source.0.state = TrackState::WaitingForSource {
            context: WaitContext::Recreation(RecreateState {
                attempt: 0,
                cause: RecreateCause::VariantSwitch,
                media_info,
                next: RecreateNext::Seek(SeekRequest {
                    attempt: 0,
                    seek: SeekContext { epoch, target },
                }),
                offset,
            }),
            reason,
        };
    }
}

pub use crate::resampler::{ResamplerParams, ResamplerProcessor};
