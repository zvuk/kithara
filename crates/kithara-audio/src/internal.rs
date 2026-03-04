#![forbid(unsafe_code)]

pub mod audio {
    use std::sync::Arc;

    use kithara_platform::tokio::sync::Notify;

    pub use crate::{AudioConfig, pipeline::audio::Audio};

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
}

pub mod source {
    use std::{
        io::{self, Read, Seek, SeekFrom},
        sync::{Arc, atomic::AtomicU64},
        time::Duration,
    };

    use kithara_decode::{InnerDecoder, PcmChunk};
    use kithara_stream::{Fetch, MediaInfo, Stream, StreamType, Timeline};

    use crate::{pipeline::worker::AudioWorkerSource, traits::AudioEffect};

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
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.0.read(buf)
        }
    }

    impl<T: StreamType> Seek for SharedStream<T> {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            self.0.seek(pos)
        }
    }

    pub struct OffsetReader<T: StreamType>(crate::pipeline::source::OffsetReader<T>);

    impl<T: StreamType> Read for OffsetReader<T> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.0.read(buf)
        }
    }

    impl<T: StreamType> Seek for OffsetReader<T> {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            self.0.seek(pos)
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

    pub fn apply_pending_seek<T: StreamType>(source: &mut StreamAudioSource<T>) {
        AudioWorkerSource::apply_pending_seek(&mut source.0);
    }

    pub fn clear_variant_fence<T: StreamType>(shared: &SharedStream<T>) {
        shared.0.clear_variant_fence();
    }

    #[must_use]
    pub fn current_base_offset<T: StreamType>(source: &StreamAudioSource<T>) -> u64 {
        source.0.base_offset
    }

    pub fn fetch_next<T: StreamType>(source: &mut StreamAudioSource<T>) -> Fetch<PcmChunk> {
        AudioWorkerSource::fetch_next(&mut source.0)
    }

    #[must_use]
    pub fn has_pending_format_change<T: StreamType>(source: &StreamAudioSource<T>) -> bool {
        source.0.pending_format_change.is_some()
    }

    pub fn retry_decode_error_after_seek<T: StreamType>(source: &mut StreamAudioSource<T>) -> bool {
        source.0.retry_decode_error_after_seek()
    }

    pub fn set_base_offset<T: StreamType>(source: &mut StreamAudioSource<T>, base_offset: u64) {
        source.0.base_offset = base_offset;
    }

    pub fn set_cached_media_info<T: StreamType>(
        source: &mut StreamAudioSource<T>,
        media_info: Option<MediaInfo>,
    ) {
        source.0.cached_media_info = media_info;
    }

    pub fn set_seek_recover_state<T: StreamType>(
        source: &mut StreamAudioSource<T>,
        base_offset: u64,
        decode_started_epoch: Option<u64>,
        target: Option<(u64, Duration)>,
        attempts: u8,
    ) {
        source.0.base_offset = base_offset;
        source.0.pending_decode_started_epoch = decode_started_epoch;
        source.0.pending_seek_recover_target = target;
        source.0.pending_seek_recover_attempts = attempts;
    }

    #[must_use]
    pub fn timeline<T: StreamType>(source: &StreamAudioSource<T>) -> &Timeline {
        &source.0.timeline
    }
}

pub use crate::resampler::{ResamplerParams, ResamplerProcessor};
