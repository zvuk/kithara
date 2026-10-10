use std::{
    future::poll_fn,
    marker::PhantomData,
    num::{NonZeroU32, NonZeroUsize},
    task::Poll,
};

use kithara::{
    audio::{
        Audio, AudioConfig, AudioControl, AudioRead, AudioReadError, AudioSession, ChunkOutcome,
        FailureSource, PendingReason, ReadOutcome, ResamplerBackend, SeekOutcome, TrackFailureKind,
    },
    bufpool::HasPool,
    decode::{DecodeError, TrackMetadata},
    events::{EventBus, EventReceiver, EventSet},
    platform::time::Duration,
    play::{LoadRefusal, PlayWorker, TrackConfig},
    signal::{AudioChunk, AudioSpec, SegmentId},
    stream::{Stream, StreamType, WorkerWake},
    warp::SpeedCurve,
};
use kithara_command::{Batch, Outcome, Rejection, SendError, Sender, When};
use kithara_render::{
    Dispatched, DispatcherCommand, LaneCommand, LaneProtocol, LaneStart, LoadRequest, PcmPacket,
    PcmReceiver, ServiceClass,
};

use super::dispatcher::{LaneLoader, LaneOpen, ReleaseLane};

#[cfg(all(feature = "all", not(target_arch = "wasm32")))]
pub async fn open_resource(
    config: &kithara::play::ResourceConfig<crate::bufpool_ext::TestPools>,
) -> Result<kithara::play::Resource, LoadRefusal> {
    let (worker, track) = kithara::play::mock::resource_tracks(config)?;
    let resource = match track {
        futures::future::Either::Left(track) => {
            kithara::play::Resource::from_reader(load_audio(&worker, track).await?, None)
        }
        futures::future::Either::Right(track) => {
            kithara::play::Resource::from_reader(load_audio(&worker, track).await?, None)
        }
    };
    Ok(resource)
}

struct SourceWake<S>(PlayWorker<S>);

impl<S: Send + Sync + 'static> WorkerWake for SourceWake<S> {
    delegate::delegate! {
        to self.0 {
            #[call(wake)]
            fn defer(&self);
            fn wake(&self);
        }
    }
}

pub async fn load_source_audio<T, B, S>(
    worker: &PlayWorker<S>,
    config: AudioConfig<T, B>,
) -> Result<Audio<Stream<T>>, DecodeError>
where
    T: StreamType<Events = EventBus>,
    B: Default + ResamplerBackend,
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    Audio::prepare(
        config,
        kithara::platform::sync::Arc::new(SourceWake(worker.clone())),
        worker.pools().clone(),
    )
    .await
}

#[cfg(all(feature = "all", not(target_arch = "wasm32")))]
pub struct PcmDeck {
    pub(super) _directory: kithara_test_utils::TestTempDir,
    source: kithara::queue::TrackSource<crate::bufpool_ext::TestPools>,
}

#[cfg(all(feature = "all", not(target_arch = "wasm32")))]
impl PcmDeck {
    pub fn new(mut reader: Box<dyn kithara::audio::AudioReader>) -> Self {
        use std::io::{Seek, SeekFrom, Write};

        let directory = kithara_test_utils::TestTempDir::new();
        let path = directory.path().join("deck.wav");
        let mut file =
            std::io::BufWriter::new(std::fs::File::create(&path).expect("create PCM deck WAV"));
        let spec = reader.spec();
        let block_align = spec.channels.checked_mul(4).expect("WAV block alignment");
        let byte_rate = spec
            .sample_rate
            .get()
            .checked_mul(u32::from(block_align))
            .expect("WAV byte rate");
        file.write_all(b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0\x03\0")
            .expect("write float WAV header");
        file.write_all(&spec.channels.to_le_bytes())
            .expect("write WAV channels");
        file.write_all(&spec.sample_rate.get().to_le_bytes())
            .expect("write WAV rate");
        file.write_all(&byte_rate.to_le_bytes())
            .expect("write WAV byte rate");
        file.write_all(&block_align.to_le_bytes())
            .expect("write WAV block alignment");
        file.write_all(&32u16.to_le_bytes())
            .expect("write WAV bit depth");
        file.write_all(b"data\0\0\0\0")
            .expect("write WAV data header");
        let mut samples = vec![0.0; usize::from(spec.channels) * 4096];
        let mut bytes = 0u32;
        loop {
            match reader.read(&mut samples).expect("read PCM deck samples") {
                ReadOutcome::Frames { count, .. } => {
                    for sample in &samples[..count.get()] {
                        file.write_all(&sample.to_le_bytes())
                            .expect("write PCM sample");
                    }
                    let written = u32::try_from(count.get())
                        .expect("WAV sample count")
                        .checked_mul(4)
                        .expect("WAV sample bytes");
                    bytes = bytes.checked_add(written).expect("PCM deck fits RIFF");
                }
                ReadOutcome::Pending { .. } => {
                    panic!("PCM deck reader must provide samples synchronously");
                }
                ReadOutcome::Eof { .. } => break,
            }
        }
        file.seek(SeekFrom::Start(4)).expect("seek RIFF size");
        file.write_all(&bytes.checked_add(36).expect("RIFF length").to_le_bytes())
            .expect("write RIFF size");
        file.seek(SeekFrom::Start(40)).expect("seek WAV data size");
        file.write_all(&bytes.to_le_bytes())
            .expect("write WAV data size");
        file.flush().expect("flush PCM deck WAV");
        Self {
            _directory: directory,
            source: kithara::queue::TrackSource::Uri(
                path.to_str().expect("UTF-8 test WAV path").to_owned(),
            ),
        }
    }

    pub fn source(&self) -> kithara::queue::TrackSource<crate::bufpool_ext::TestPools> {
        self.source.clone()
    }
}

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct LaneAudio<T, S> {
    worker: PlayWorker<S>,
    release: ReleaseLane,
    block_on_underrun: bool,
    sender: Sender<LaneProtocol>,
    pcm: PcmReceiver,
    bus: EventBus,
    #[field(get, copy)]
    segment: SegmentId,
    speed: SpeedCurve,
    ready: Option<SegmentId>,
    #[field(get, copy)]
    committed_segment: Option<SegmentId>,
    chunk: Option<Box<AudioChunk>>,
    offset: usize,
    failure: Option<TrackFailureKind>,
    eof: bool,
    marker: PhantomData<fn() -> T>,
}

pub async fn load_audio<T, B, S, C>(
    worker: &PlayWorker<S>,
    config: C,
) -> Result<LaneAudio<Stream<T>, S>, LoadRefusal>
where
    T: StreamType<Events = EventBus>,
    B: Default + ResamplerBackend,
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    C: Into<TrackConfig<T, B>>,
{
    let mut loader = LaneLoader::new(worker)?;
    loader.load(config).await
}

impl<T, B, S> LaneLoader<T, B, S>
where
    T: StreamType<Events = EventBus>,
    B: Default + ResamplerBackend,
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    pub async fn load<C>(&mut self, config: C) -> Result<LaneAudio<Stream<T>, S>, LoadRefusal>
    where
        C: Into<TrackConfig<T, B>>,
    {
        let worker = &self.worker;
        let config = config.into();
        let bus = T::event_bus(config.audio().stream())
            .or_else(|| config.audio().bus().cloned())
            .unwrap_or_default();
        let start = LaneStart {
            speed: SpeedCurve::Constant(config.warp().speed()),
            keylock: config.warp().keylock(),
            backend: config.warp().backend(),
        };
        let speed = start.speed.clone();
        let (sender, inbox) = worker.lane_channel();
        let block_on_underrun = config.block_on_underrun();
        let config = kithara_render::mock::with_blocking_reads(config);
        let seq = self
            .owner
            .lock()
            .sender
            .send(
                When::Next,
                Batch {
                    basis: Vec::new(),
                    commands: vec![DispatcherCommand::Load(Box::new(LoadRequest {
                        class: ServiceClass::Warm,
                        item: LaneOpen {
                            worker: worker.clone(),
                            config,
                        },
                        position: Duration::ZERO,
                        start,
                        inbox,
                    }))],
                },
            )
            .map_err(|error| {
                LoadRefusal::Open(DecodeError::audio_stream(
                    "test lane load",
                    format!("{error:?}"),
                ))
            })?;
        worker.wake();
        let receipt = poll_fn(|context| {
            let mut owner = self.owner.lock();
            let loads = &mut owner.sender;
            loads.hold(context.waker().clone());
            let found = loads.receipts().find(|receipt| receipt.seq() == seq);
            drop(owner);
            found.map_or(Poll::Pending, Poll::Ready)
        })
        .await;
        self.owner.lock().sender.release();
        let (outcome, _) = receipt.into();
        let loaded = match outcome {
            Outcome::Applied {
                data: Dispatched::Loaded(loaded),
                ..
            } => loaded,
            Outcome::Rejected(Rejection::Refused(refusal)) => return Err(refusal),
            outcome => panic!("unexpected load receipt: {outcome:?}"),
        };
        let lane = loaded.lane;
        let loads = self.owner.clone();
        let release_worker = worker.clone();
        Ok(LaneAudio {
            worker: worker.clone(),
            release: Box::new(move || {
                let result = loads.lock().sender.send(
                    When::Next,
                    Batch {
                        basis: Vec::new(),
                        commands: vec![DispatcherCommand::Release(lane)],
                    },
                );
                match result {
                    Ok(_) => release_worker.wake(),
                    Err(SendError::Closed(_)) => {}
                    Err(error) => panic!("release test reader lane: {error:?}"),
                }
            }),
            block_on_underrun,
            sender,
            pcm: loaded.opened,
            bus,
            segment: SegmentId::FIRST,
            speed,
            ready: Some(SegmentId::FIRST),
            committed_segment: None,
            chunk: None,
            offset: 0,
            failure: None,
            eof: false,
            marker: PhantomData,
        })
    }
}

impl<T, S> Drop for LaneAudio<T, S> {
    fn drop(&mut self) {
        (self.release)();
    }
}

#[kithara_test_utils::kithara::flash(true)]
pub async fn wait_for_preload<T, S>(audio: &mut LaneAudio<T, S>, label: &str) {
    audio.wait_ready(label).await;
}

impl<T, S> LaneAudio<T, S> {
    fn service(&mut self) {
        while self.pcm.peek().is_some_and(|packet| match packet {
            PcmPacket::Chunk(chunk) => chunk.meta.segment != self.segment,
            PcmPacket::Failed { segment, .. } => *segment != self.segment,
        }) {
            let packet = self.pcm.pop().expect("peeked stale packet");
            self.pcm.recycle(packet).expect("recycle stale packet");
        }
        for receipt in self.sender.receipts() {
            if let Outcome::Applied { data, .. } = receipt.outcome() {
                if data.ready == Some(self.segment) {
                    self.ready = data.ready;
                }
            } else {
                panic!("lane command rejected: {:?}", receipt.outcome());
            }
        }
    }

    async fn wait_ready(&mut self, label: &str) {
        poll_fn(|context| {
            self.sender.hold(context.waker().clone());
            self.service();
            if self.ready == Some(self.segment) {
                Poll::Ready(())
            } else {
                assert!(
                    !self.pcm.is_closed(),
                    "{label}: lane closed before segment preload"
                );
                Poll::Pending
            }
        })
        .await;
        self.sender.release();
    }

    pub fn current_segment_ready(&mut self) -> bool {
        self.service();
        self.ready == Some(self.segment)
    }

    pub fn events<E: EventSet>(&self) -> EventReceiver<E> {
        self.bus.subscribe()
    }

    fn send(&mut self, command: LaneCommand) -> Result<(), AudioReadError> {
        self.sender
            .send(
                When::Next,
                Batch {
                    basis: Vec::new(),
                    commands: vec![command],
                },
            )
            .map_err(|error| {
                DecodeError::audio_stream("test lane command", format!("{error:?}"))
            })?;
        self.worker.wake();
        Ok(())
    }

    fn pull(&mut self) -> Result<ChunkOutcome, AudioReadError> {
        if !self.current_segment_ready() {
            futures::executor::block_on(self.wait_ready("read"));
        }
        self.service();
        if let Some(failure) = self.failure {
            return Err(AudioReadError::Stream {
                what: "read test lane",
                source: FailureSource::Producer { failure },
            });
        }
        if self.eof {
            return Ok(ChunkOutcome::Eof {
                position: self.pcm.position(),
            });
        }
        loop {
            let Some(packet) = self.pcm.pop() else {
                if self.block_on_underrun && !self.pcm.is_closed() {
                    kithara_render::mock::wait_for_packet(&self.pcm);
                    continue;
                }
                return Ok(ChunkOutcome::Pending {
                    reason: PendingReason::Buffering,
                    position: self.pcm.position(),
                });
            };
            match packet {
                PcmPacket::Chunk(chunk) if chunk.meta.segment == self.segment => {
                    if chunk.meta.end_of_track {
                        self.pcm.set_position(chunk.meta.timestamp);
                        self.pcm
                            .recycle(PcmPacket::Chunk(chunk))
                            .expect("recycle EOF packet");
                        self.eof = true;
                        return Ok(ChunkOutcome::Eof {
                            position: self.pcm.position(),
                        });
                    }
                    return Ok(ChunkOutcome::Chunk(chunk));
                }
                PcmPacket::Failed { segment, failure } if segment == self.segment => {
                    self.failure = Some(failure);
                    return Err(AudioReadError::Stream {
                        what: "read test lane",
                        source: FailureSource::Producer { failure },
                    });
                }
                packet => self.pcm.recycle(packet).expect("return stale packet"),
            }
        }
    }
}

impl<T, S> AudioRead for LaneAudio<T, S> {
    delegate::delegate! {
        to self.pcm {
            fn spec(&self) -> AudioSpec;
            fn position(&self) -> Duration;
            fn decoded_frontier(&self) -> Duration;
            fn cached_span(&self) -> Duration;
        }
    }

    fn next_chunk(&mut self) -> Result<ChunkOutcome, AudioReadError> {
        if let Some(mut chunk) = self.chunk.take() {
            let channels = usize::from(chunk.meta.spec.channels);
            let skipped = self.offset / channels;
            chunk.samples.drain(..self.offset);
            chunk.meta.timestamp = chunk
                .meta
                .source_span
                .and_then(|span| span.position_at(skipped as u64))
                .unwrap_or_else(|| {
                    chunk.meta.timestamp
                        + chunk
                            .meta
                            .spec
                            .duration_for(skipped as u64)
                            .expect("packet offset duration")
                });
            chunk.meta.source_span = chunk.meta.source_span.and_then(|span| {
                span.for_output_range(skipped as u64..u64::from(chunk.meta.frames))
            });
            chunk.meta.frame_offset += skipped as u64;
            chunk.meta.lane_frame += skipped as u64;
            chunk.meta.frames -= skipped as u32;
            self.offset = 0;
            self.committed_segment = Some(chunk.meta.segment);
            self.pcm.set_position(
                chunk
                    .meta
                    .source_span
                    .and_then(|span| span.position_at(u64::from(chunk.meta.frames)))
                    .unwrap_or_else(|| {
                        chunk.meta.timestamp
                            + chunk
                                .meta
                                .spec
                                .duration_for(u64::from(chunk.meta.frames))
                                .expect("packet duration")
                    }),
            );
            return Ok(ChunkOutcome::Chunk(chunk));
        }
        let outcome = self.pull()?;
        if let ChunkOutcome::Chunk(chunk) = &outcome {
            self.committed_segment = Some(chunk.meta.segment);
            self.pcm.set_position(
                chunk
                    .meta
                    .source_span
                    .and_then(|span| span.position_at(u64::from(chunk.meta.frames)))
                    .unwrap_or_else(|| {
                        chunk.meta.timestamp
                            + chunk
                                .meta
                                .spec
                                .duration_for(u64::from(chunk.meta.frames))
                                .expect("packet duration")
                    }),
            );
        }
        Ok(outcome)
    }

    fn read(&mut self, output: &mut [f32]) -> Result<ReadOutcome, AudioReadError> {
        if self.chunk.is_none() {
            match self.pull()? {
                ChunkOutcome::Chunk(chunk) => {
                    self.chunk = Some(chunk);
                    self.offset = 0;
                }
                ChunkOutcome::Pending { reason, position } => {
                    return Ok(ReadOutcome::Pending { reason, position });
                }
                ChunkOutcome::Eof { position } => return Ok(ReadOutcome::Eof { position }),
            }
        }
        let chunk = self.chunk.as_ref().expect("a current packet");
        let channels = usize::from(chunk.meta.spec.channels);
        let count = output.len().min(chunk.samples.len() - self.offset) / channels * channels;
        let Some(count) = NonZeroUsize::new(count) else {
            return Ok(ReadOutcome::Pending {
                reason: PendingReason::Buffering,
                position: self.pcm.position(),
            });
        };
        let start = self.offset / channels;
        output[..count.get()]
            .copy_from_slice(&chunk.samples[self.offset..self.offset + count.get()]);
        self.offset += count.get();
        let end = self.offset / channels;
        let source_span = chunk
            .meta
            .source_span
            .and_then(|span| span.for_output_range(start as u64..end as u64));
        let position = chunk
            .meta
            .source_span
            .and_then(|span| span.position_at(end as u64))
            .unwrap_or_else(|| {
                chunk.meta.timestamp
                    + chunk
                        .meta
                        .spec
                        .duration_for(end as u64)
                        .expect("packet duration")
            });
        self.committed_segment = Some(chunk.meta.segment);
        self.pcm.set_position(position);
        if self.offset == chunk.samples.len() {
            let chunk = self.chunk.take().expect("consumed packet");
            self.pcm
                .recycle(PcmPacket::Chunk(chunk))
                .expect("recycle consumed packet");
        }
        Ok(ReadOutcome::Frames {
            count,
            position,
            source_span,
        })
    }

    fn read_planar<'a>(
        &mut self,
        output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, AudioReadError> {
        let channels = usize::from(self.spec().channels);
        let frames = output.first().map_or(0, |plane| plane.len());
        for (channel, plane) in output.iter().enumerate().skip(1) {
            if plane.len() != frames {
                return Err(
                    DecodeError::from(kithara::signal::SignalError::ChannelFrames {
                        channel,
                        expected: frames,
                        actual: plane.len(),
                    })
                    .into(),
                );
            }
        }
        let mut samples = vec![0.0; frames * channels];
        match self.read(&mut samples)? {
            ReadOutcome::Frames {
                count,
                position,
                source_span,
            } => {
                for (frame, samples) in samples[..count.get()].chunks_exact(channels).enumerate() {
                    for (channel, plane) in output.iter_mut().enumerate() {
                        plane[frame] = samples.get(channel).copied().unwrap_or(samples[0]);
                    }
                }
                Ok(ReadOutcome::Frames {
                    count: NonZeroUsize::new(count.get() / channels).expect("whole frames"),
                    position,
                    source_span,
                })
            }
            outcome => Ok(outcome),
        }
    }
}

impl<T, S> AudioSession for LaneAudio<T, S> {
    delegate::delegate! {
        to self.pcm {
            fn duration(&self) -> Option<Duration>;
            fn metadata(&self) -> &TrackMetadata;
            fn abr_handle(&self) -> Option<kithara::abr::AbrHandle>;
        }
    }

    fn event_bus(&self) -> &EventBus {
        &self.bus
    }
    fn is_preloaded(&self) -> bool {
        self.ready == Some(self.segment)
    }
}

impl<T, S> AudioControl for LaneAudio<T, S> {
    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, AudioReadError> {
        let segment = self.segment.next();
        self.send(LaneCommand::Segment {
            id: segment,
            from: position,
            speed: self.speed.clone(),
        })?;
        self.segment = segment;
        self.ready = None;
        self.eof = false;
        self.failure = None;
        if let Some(chunk) = self.chunk.take() {
            self.pcm
                .recycle(PcmPacket::Chunk(chunk))
                .expect("recycle before seek");
        }
        self.offset = 0;
        match self.duration() {
            Some(duration) if position >= duration => Ok(SeekOutcome::PastEof {
                target: position,
                duration,
            }),
            _ => Ok(SeekOutcome::Landed {
                target: position,
                landed_at: position,
            }),
        }
    }

    fn set_host_sample_rate(&mut self, rate: NonZeroU32) {
        let segment = self.segment.next();
        self.send(LaneCommand::SetHostRate { id: segment, rate })
            .expect("set test lane rate");
        self.segment = segment;
        self.ready = None;
        if let Some(chunk) = self.chunk.take() {
            self.pcm
                .recycle(PcmPacket::Chunk(chunk))
                .expect("recycle before rate change");
        }
        self.offset = 0;
        self.eof = false;
        self.failure = None;
    }
}
