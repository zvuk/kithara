use std::{future::Future, num::NonZeroU32, task::Poll};

use firewheel::{
    clock::InstantSamples,
    dsp::{buffer::ConstSequentialBuffer, declick::DeclickValues},
    log::{RealtimeLoggerConfig, realtime_logger},
    mask::{ConnectedMask, ConstantMask, SilenceMask},
    node::{
        AudioNodeProcessor, NUM_SCRATCH_BUFFERS, ProcBuffers, ProcExtra, ProcInfo, ProcStore,
        StreamStatus,
    },
};
use futures::future::poll_fn;
use kithara_assets::{AssetStore, StorageBackend};
use kithara_audio::AudioObserverSlot;
use kithara_command::{
    Batch, ChannelConfig, Inbox, OpenError, Outcome, Receipt, ScopeId, ScopedConfig, ScopedInbox,
    ScopedReceipt, ScopedSender, Sender, When, channel, scoped_channel,
};
use kithara_platform::{
    time::{Duration, timeout},
    tokio::task::yield_now,
};
use kithara_render::{
    Dispatched, DispatcherCommand, DispatcherProtocol, LoadRefusal, LoadRequest, Loaded, Open,
    bridge::{DeckEnds, DeckEvents, DeckPart, DeckProtocol, Slot, scope_channels},
    mock::{MockDeck, TestSessionInbox},
    rt::{DeckMixer, DeckMixerConfig, StreamShape, install_render_context, publish_render_context},
};
use kithara_signal::{FrameCount, OutputContext, SessionEpoch, SessionFrame};
use kithara_test_utils::kithara;
use kithara_warp::RenderContext;
use num_traits::ToPrimitive;

use crate::{
    DeckPass, OpenedTrack, PlayError, ResourceLoad, TrackSettings, player::Outbox,
    session::SessionOutputView,
};
pub use crate::{
    api::equalizer::EqualizerMock,
    resource::source_mock::{resource_tracks, track_load},
};

/// Sample rate every mock session runs at.
pub const SAMPLE_RATE: NonZeroU32 = match NonZeroU32::new(44_100) {
    Some(sample_rate) => sample_rate,
    None => unreachable!(),
};

/// Observes the cancellation subtree and output rate passed to a fixture open.
#[must_use]
pub fn prepared_resource_context<S>(
    config: &crate::ResourceConfig<S>,
) -> (Option<kithara_platform::CancelToken>, Option<NonZeroU32>)
where
    S: kithara_bufpool::HasPool<u8> + Send + Sync + 'static,
{
    (config.cancel.clone(), config.host_sample_rate)
}

/// Writes generated PCM as a float WAV for tests opening a real URI source.
///
/// # Errors
/// Returns an invalid WAV geometry or the destination's write failure.
#[kithara::hang_watchdog(timeout = Duration::from_secs(1), ctx = String)]
pub fn write_pcm_wav(
    path: &std::path::Path,
    samples: &[f32],
    spec: kithara_signal::AudioSpec,
) -> std::io::Result<()> {
    use std::io::{BufWriter, Write};

    let _io = kithara_platform::flash::real_io();
    hang_tick!("write_pcm_wav: writing fixture PCM".to_owned());
    let (data_bytes, riff_size, block_align, byte_rate) = wav_sizes(samples, spec)?;
    let mut file = BufWriter::new(std::fs::File::create(path)?);
    write_wav_header(
        &mut file,
        spec,
        data_bytes,
        riff_size,
        block_align,
        byte_rate,
    )?;
    hang_reset!();
    let mut bytes = [0_u8; 4096];
    for (index, chunk) in samples.chunks(bytes.len() / 4).enumerate() {
        hang_tick!(format!("write_pcm_wav: {path:?}, chunk {index}"));
        let encoded = &mut bytes[..chunk.len() * 4];
        encoded
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(chunk)
            .for_each(|(target, sample)| target.copy_from_slice(&sample.to_le_bytes()));
        file.write_all(encoded)?;
        hang_reset!();
    }
    file.flush()
}

fn wav_sizes(
    samples: &[f32],
    spec: kithara_signal::AudioSpec,
) -> std::io::Result<(u32, u32, u16, u32)> {
    use std::io::{Error, ErrorKind};

    let invalid = || {
        Error::new(
            ErrorKind::InvalidInput,
            "float WAV geometry exceeds RIFF limits",
        )
    };
    let data_bytes = samples
        .len()
        .checked_mul(4)
        .and_then(|bytes| u32::try_from(bytes).ok())
        .ok_or_else(invalid)?;
    let riff_size = data_bytes.checked_add(36).ok_or_else(invalid)?;
    let block_align = spec.channels.checked_mul(4).ok_or_else(invalid)?;
    let byte_rate = spec
        .sample_rate
        .get()
        .checked_mul(u32::from(block_align))
        .ok_or_else(invalid)?;
    if spec.channels == 0 || !samples.len().is_multiple_of(usize::from(spec.channels)) {
        return Err(invalid());
    }
    Ok((data_bytes, riff_size, block_align, byte_rate))
}

fn write_wav_header(
    file: &mut impl std::io::Write,
    spec: kithara_signal::AudioSpec,
    data_bytes: u32,
    riff_size: u32,
    block_align: u16,
    byte_rate: u32,
) -> std::io::Result<()> {
    file.write_all(b"RIFF")?;
    file.write_all(&riff_size.to_le_bytes())?;
    file.write_all(b"WAVEfmt ")?;
    file.write_all(&16_u32.to_le_bytes())?;
    file.write_all(&3_u16.to_le_bytes())?;
    file.write_all(&spec.channels.to_le_bytes())?;
    file.write_all(&spec.sample_rate.get().to_le_bytes())?;
    file.write_all(&byte_rate.to_le_bytes())?;
    file.write_all(&block_align.to_le_bytes())?;
    file.write_all(&32_u16.to_le_bytes())?;
    file.write_all(b"data")?;
    file.write_all(&data_bytes.to_le_bytes())?;
    Ok(())
}

/// The output of a session at [`SAMPLE_RATE`] that measured `shape`.
#[must_use]
pub fn output(shape: Option<StreamShape>) -> SessionOutputView {
    let output = SessionOutputView::new(SAMPLE_RATE);
    output.publish(output.get().sample_rate, shape);
    output
}

/// Opens player-prepared float WAV input and checks two real session render blocks.
///
/// # Errors
/// Returns fixture, preparation, lane, or offline mixer failures.
///
/// # Panics
/// Panics if the prepared fixture fails to play, advance, produce PCM, or keep
/// render-side events off the source event bus.
#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
pub async fn assert_prepared_render_off_bus<S>(
    prep: &crate::ResourcePrep<S>,
    output: &crate::OutputSnapshot,
    pools: &kithara_bufpool::PoolRegion<S>,
    path: &std::path::Path,
) -> Result<(), PlayError>
where
    S: kithara_bufpool::HasPool<u8> + kithara_bufpool::HasPool<f32> + Send + Sync + 'static,
{
    use kithara_signal::AudioSpec;

    let rate = NonZeroU32::new(output.sample_rate.output()).ok_or(PlayError::Closed)?;
    write_pcm_wav(path, &[0.5; 8_192], AudioSpec::new(2, rate))
        .map_err(|error| PlayError::Internal(error.to_string()))?;
    let config: crate::ResourceConfig<S> =
        crate::ResourceConfig::for_src(crate::ResourceSrc::Path(path.to_owned()))
            .store(
                AssetStore::builder(pools.clone())
                    .backend(StorageBackend::Memory)
                    .build(),
            )
            .build();
    let prepared = prep.prepare(config, output)?;
    let bus = prepared.bus.clone().ok_or(PlayError::Closed)?;
    let item = ResourceLoad::new(prepared, Box::new(AudioObserverSlot::default().relay()));
    let (_control, inbox) = item.lane_channel()?;
    assert_requested_render_off_bus(
        LoadRequest {
            item,
            class: kithara_render::ServiceClass::Warm,
            inbox,
            position: Duration::ZERO,
            start: TrackSettings::default().lane_start(),
        },
        &bus,
        output,
        pools,
    )
    .await
}

/// Renders the exact load request emitted by an owner and checks its source bus.
///
/// # Errors
/// Returns source-open or offline mixer failures.
///
/// # Panics
/// Panics if either block is silent, stopped, stationary, or publishes source events.
#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
pub async fn assert_requested_render_off_bus<S>(
    request: LoadRequest<ResourceLoad<S>>,
    bus: &kithara_events::EventBus,
    output: &crate::OutputSnapshot,
    pools: &kithara_bufpool::PoolRegion<S>,
) -> Result<(), PlayError>
where
    S: kithara_bufpool::HasPool<u8> + kithara_bufpool::HasPool<f32> + Send + Sync + 'static,
{
    use kithara_events::TryRecvError;
    use kithara_render::{
        LaneTask,
        bridge::{Fade, Slot, SlotState},
    };
    use kithara_signal::SegmentId;
    use kithara_worker::Task;

    #[derive(Debug, kithara_events::EventSet)]
    enum RenderEvents {
        Audio(kithara_audio::AudioEvent),
        Decoder(kithara_audio::DecoderEvent),
        File(kithara_file::FileEvent),
        Hls(kithara_hls::HlsEvent),
        Drm(kithara_hls::DrmEvent),
        Player(crate::PlayerEvent),
    }

    let _io = kithara_platform::flash::real_io();
    let rate = NonZeroU32::new(output.sample_rate.output()).ok_or(PlayError::Closed)?;
    let frames = NonZeroU32::new(128).ok_or(PlayError::Closed)?;
    let shape = StreamShape::new(frames, rate);
    let mut events = bus.subscribe::<RenderEvents>();
    let position = request.position;
    let (opened, mut lane) = open_render_request(request).await?;
    hang_reset!();
    preload_lane(&mut lane).await?;
    let required_span = f64::from(frames.get() * 2) / f64::from(rate.get());
    let mut buffered = (opened.pcm.decoded_frontier() - position.as_secs_f64()).max(0.0);
    while buffered < required_span {
        hang_tick!();
        {
            let mut context = std::task::Context::from_waker(std::task::Waker::noop());
            let _ = lane.poll_commands(&mut context);
        }
        lane.recycle();
        let _ = lane.tick();
        lane.preload_status()
            .map_err(|error| PlayError::Internal(format!("{error:?}")))?;
        kithara_platform::time::sleep(Duration::from_millis(1)).await;
        let available = (opened.pcm.decoded_frontier() - position.as_secs_f64()).max(0.0);
        if available > buffered {
            hang_reset!();
        }
        buffered = available;
    }
    let mut mixer = MixerRig::new(DeckMixerConfig::default(), shape, pools)?;
    let slot = Slot::new(0);
    mixer.send(
        When::Next,
        DeckPart::Attach {
            item_id: kithara_events::TrackId::allocate(),
            slot,
            pcm: opened.pcm,
            segment: SegmentId::FIRST,
        },
    )?;
    mixer.send(
        When::Next,
        DeckPart::Start {
            slot,
            fade: Fade::Declick,
        },
    )?;
    while events.try_recv().is_ok() {
        hang_tick!();
        hang_reset!();
    }
    let mut position = 0.0;
    for block in 0..2 {
        hang_tick!();
        let mut pcm = [[0.0; 128]; 2];
        let [left, right] = &mut pcm;
        mixer.block(
            SessionFrame::new(block * i64::from(frames.get())),
            [left, right],
        )?;
        assert!(matches!(events.try_recv(), Err(TryRecvError::Empty)));
        let snapshot = mixer.ends.snapshot.read();
        assert_eq!(snapshot.slots[0].state, SlotState::Playing);
        assert!(snapshot.slots[0].position > position);
        assert!(pcm.iter().flatten().any(|sample| *sample != 0.0));
        position = snapshot.slots[0].position;
        hang_reset!();
    }
    Ok(())
}

#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn open_render_request<S>(
    request: LoadRequest<ResourceLoad<S>>,
) -> Result<(OpenedTrack, crate::ResourceLane), PlayError>
where
    S: kithara_bufpool::HasPool<u8> + kithara_bufpool::HasPool<f32> + Send + Sync + 'static,
{
    let mut opening = std::pin::pin!(request.item.open(
        request.position,
        request.start,
        request.inbox
    ));
    let (opened, lane, _) = {
        // WHY: Source reads run on real threads, so their watchdog must not leap in virtual time.
        let _real_io = kithara_platform::flash::real_io();
        loop {
            hang_tick!();
            if let Ok(opened) = timeout(Duration::from_millis(10), opening.as_mut()).await {
                break opened.map_err(|error| PlayError::Internal(format!("{error:?}")))?;
            }
        }
    };
    Ok((opened, lane))
}

/// Drives a directly opened lane through the dispatcher's preload admission.
///
/// # Errors
/// Returns the refusal that prevents the lane from becoming ready.
#[kithara::hang_watchdog]
pub async fn preload_lane<Lane>(lane: &mut Lane) -> Result<(), PlayError>
where
    Lane: kithara_render::LaneTask,
{
    let _real_io = kithara_platform::flash::real_io();
    lane.warm_up();
    loop {
        hang_tick!();
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        let _ = lane.poll_commands(&mut context);
        lane.recycle();
        if lane.tick() == kithara_worker::TickResult::Progress {
            hang_reset!();
        }
        if lane
            .preload_status()
            .map_err(|error| PlayError::Internal(format!("{error:?}")))?
        {
            return Ok(());
        }
        yield_now().await;
    }
}

/// A real offline mixer driven by exact host blocks and scoped deck commands.
pub struct MixerRig {
    pub ring: ScopedSender<DeckProtocol, DeckProtocol>,
    pub scope: ScopeId,
    pub ends: DeckEnds,
    mixer: DeckMixer<TestSessionInbox>,
    extra: ProcExtra,
    shape: StreamShape,
}

impl MixerRig {
    /// Builds the same processor store and output shape a host supplies.
    ///
    /// # Errors
    /// Returns command, pool, or output geometry failures.
    pub fn new<S>(
        config: DeckMixerConfig,
        shape: StreamShape,
        pools: &kithara_bufpool::PoolRegion<S>,
    ) -> Result<Self, PlayError>
    where
        S: kithara_bufpool::HasPool<f32> + Send + Sync + 'static,
    {
        let targets = config.slots().get();
        let (mut ring, inbox) = scoped_channel(
            ScopedConfig::builder()
                .scope(ChannelConfig::builder().targets(targets).build())
                .build(),
        );
        let scope = ring
            .open(targets)
            .map_err(|error| PlayError::Internal(error.to_string()))?;
        let (ends, inputs) = scope_channels(scope, config);
        let mixer = DeckMixer::new(inputs, shape, pools)
            .map_err(|error| PlayError::Internal(error.to_string()))?;
        let frames = usize::try_from(shape.max_block_frames.get())
            .map_err(|error| PlayError::Internal(error.to_string()))?;
        let mut store = ProcStore::with_capacity(2);
        if store.insert(TestSessionInbox(inbox)).is_err() {
            return Err(PlayError::Internal(
                "mock session inbox store is full".into(),
            ));
        }
        install_render_context(&mut store)
            .map_err(|error| PlayError::Internal(error.to_string()))?;
        let (logger, _) = realtime_logger(RealtimeLoggerConfig::default());
        Ok(Self {
            ring,
            scope,
            ends,
            mixer,
            extra: ProcExtra {
                logger,
                store,
                scratch_buffers: ConstSequentialBuffer::<f32, NUM_SCRATCH_BUFFERS>::new(frames),
                declick_values: DeclickValues::new(shape.max_block_frames),
            },
            shape,
        })
    }

    /// Processes one host block into the caller's stereo PCM storage.
    ///
    /// # Errors
    /// Returns invalid host geometry or command publication failures.
    pub fn block(&mut self, at: SessionFrame, out: [&mut [f32]; 2]) -> Result<(), PlayError> {
        self.block_frames(at, out)
    }

    /// Processes an exact block within the configured maximum output geometry.
    /// Writes PCM into caller-owned storage independent of the mixer and its pools.
    ///
    /// # Errors
    /// Returns invalid host geometry or command publication failures.
    pub fn block_frames(
        &mut self,
        at: SessionFrame,
        mut out: [&mut [f32]; 2],
    ) -> Result<(), PlayError> {
        let frames = out[0].len();
        if frames == 0
            || frames != out[1].len()
            || frames > self.shape.max_block_frames.get() as usize
        {
            return Err(PlayError::Internal(
                "mock block exceeds its output geometry".into(),
            ));
        }
        let start: i64 = at.into();
        let count =
            i64::try_from(frames).map_err(|error| PlayError::Internal(error.to_string()))?;
        let end = start
            .checked_add(count)
            .ok_or_else(|| PlayError::Internal("mock block frame overflow".into()))?;
        let context = OutputContext::new(
            at..SessionFrame::new(end),
            self.shape.sample_rate,
            SessionEpoch::new(0),
            None,
        )
        .ok_or_else(|| PlayError::Internal("mock output block is invalid".into()))?;
        let context = RenderContext::new_linear(context, None)
            .ok_or_else(|| PlayError::Internal("mock render context is invalid".into()))?;
        publish_render_context(&mut self.extra.store, context)
            .map_err(|error| PlayError::Internal(error.to_string()))?;
        self.ring
            .publish()
            .map_err(|error| PlayError::Internal(error.to_string()))?;
        self.extra
            .store
            .try_get_mut::<TestSessionInbox>()
            .ok_or(PlayError::Closed)?
            .0
            .drain();
        let info = ProcInfo {
            frames,
            sample_rate: self.shape.sample_rate,
            in_silence_mask: SilenceMask::default(),
            out_silence_mask: SilenceMask::default(),
            in_constant_mask: ConstantMask::default(),
            out_constant_mask: ConstantMask::default(),
            in_connected_mask: ConnectedMask::default(),
            out_connected_mask: ConnectedMask::default(),
            total_cpu_seconds_recip: 1.0,
            process_to_playback_delay: None,
            did_just_unbypass: false,
            last_marker_instant: InstantSamples(start),
            sample_rate_recip: f64::from(self.shape.sample_rate.get()).recip(),
            clock_samples: InstantSamples(start),
            duration_since_stream_start: Duration::try_from_secs_f64(
                ToPrimitive::to_f64(&start)
                    .ok_or_else(|| PlayError::Internal("mock block time exceeds f64".into()))?
                    / f64::from(self.shape.sample_rate.get()),
            )
            .map_err(|error| PlayError::Internal(error.to_string()))?,
            stream_status: StreamStatus::empty(),
            dropped_frames: 0,
        };
        for channel in &mut out {
            channel.fill(0.0);
        }
        let _ = self.mixer.process(
            &info,
            ProcBuffers {
                inputs: &[],
                outputs: &mut out,
            },
            &mut self.extra,
        );
        Ok(())
    }

    /// Admits one deck part on the requested session frame.
    ///
    /// # Errors
    /// Returns a closed or full scoped command channel.
    pub fn send(
        &mut self,
        at: When<SessionFrame>,
        part: DeckPart,
    ) -> Result<kithara_command::Seq, PlayError> {
        use kithara_command::Port;

        self.ring
            .scope(self.scope)
            .ok_or(PlayError::Closed)?
            .send(
                at,
                Batch {
                    basis: Vec::new(),
                    commands: vec![part],
                },
            )
            .map_err(|error| PlayError::Internal(error.to_string()))
    }
}

/// A deck no audio thread runs and a dispatcher no worker drains: what a
/// player sends lands here for the test to answer as they would.
pub struct DeckRig<S> {
    pub ring: ScopedSender<DeckProtocol, DeckProtocol>,
    pub scope: ScopeId,
    pub inbox: ScopedInbox<DeckProtocol, DeckProtocol>,
    pub mixer: MockDeck,
    pub events: DeckEvents,
    pub dispatcher: Sender<DispatcherProtocol<ResourceLoad<S>>>,
    pub opens: Inbox<DispatcherProtocol<ResourceLoad<S>>>,
    fixture_dispatcher: Option<FixtureDispatcher<S>>,
    fixture_lanes: Vec<Sender<kithara_render::LaneProtocol>>,
    now: SessionFrame,
    observation: kithara_render::bridge::DeckSnapshot,
}

struct FixtureDispatcher<S> {
    sender: Sender<DispatcherProtocol<ResourceLoad<S>>>,
    _driver: kithara_worker::TaskHandle,
    _worker: crate::PlayWorker<S>,
}

impl<S> DeckRig<S> {
    pub fn new(config: DeckMixerConfig) -> Result<Self, OpenError> {
        let targets = config.slots().get();
        let (mut ring, inbox) = scoped_channel(
            ScopedConfig::builder()
                .scope(ChannelConfig::builder().targets(targets).build())
                .build(),
        );
        let scope = ring.open(targets)?;
        let (mut ends, inputs) = scope_channels(scope, config);
        let observation = ends.snapshot.read().clone();
        let (dispatcher, opens) = channel(ChannelConfig::builder().build());
        Ok(Self {
            ring,
            scope,
            inbox,
            dispatcher,
            opens,
            mixer: MockDeck::new(inputs),
            events: ends.events,
            fixture_dispatcher: None,
            fixture_lanes: Vec::new(),
            now: SessionFrame::new(0),
            observation,
        })
    }

    /// Plays the deck's block at `at` and returns the receipts of the batches
    /// it applied; a slot stopped there stood at `stopped_at` seconds.
    pub fn block(
        &mut self,
        at: SessionFrame,
        stopped_at: f64,
    ) -> Result<Vec<Receipt<DeckProtocol>>, PlayError> {
        self.now = at;
        self.pass(|mixer, level| mixer.block(level, at, stopped_at))
    }

    pub fn end(
        &mut self,
        slot: Slot,
        at: SessionFrame,
    ) -> Result<Vec<Receipt<DeckProtocol>>, PlayError> {
        self.now = at;
        self.pass(|mixer, level| mixer.end(level, slot, at))
    }

    /// Answers the oldest load after acknowledging preceding priority changes.
    ///
    /// # Panics
    /// Panics if a batch is not a load or priority change, or its receipt is missing.
    #[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
    pub fn open(
        &mut self,
        opened: Result<Loaded<OpenedTrack>, LoadRefusal>,
    ) -> Option<Receipt<DispatcherProtocol<ResourceLoad<S>>>> {
        kithara_platform::flash::participate_sync(|| {
            self.opens.drain();
            loop {
                hang_tick!();
                let due = self.opens.next_due((), 1)?;
                let seq = due.seq();
                let loading = match due.commands() {
                    [DispatcherCommand::Load(_)] => true,
                    [DispatcherCommand::SetPriority(_, _)] => false,
                    _ => panic!("expected one fixture load or priority change"),
                };
                if !loading {
                    due.apply(Dispatched::Prioritized);
                    let receipt = self.dispatcher.receipts().next().expect("priority answer");
                    assert_eq!(receipt.seq(), seq, "the dispatcher answers its own command");
                    hang_reset!();
                    continue;
                }
                match opened {
                    Ok(opened) => due.apply(Dispatched::Loaded(opened)),
                    Err(refusal) => due.refuse(refusal),
                }
                let receipt = self
                    .dispatcher
                    .receipts()
                    .next()
                    .expect("dispatcher answer");
                assert_eq!(receipt.seq(), seq, "the dispatcher answers its own command");
                hang_reset!();
                return Some(receipt);
            }
        })
    }

    #[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
    fn pass(
        &mut self,
        run: impl FnOnce(&mut MockDeck, &mut kithara_command::LevelInbox<'_, DeckProtocol>),
    ) -> Result<Vec<Receipt<DeckProtocol>>, PlayError> {
        self.ring
            .publish()
            .map_err(|error| PlayError::Internal(error.to_string()))?;
        self.inbox.drain();
        let mut level = self.inbox.scope(self.scope).ok_or(PlayError::Closed)?;
        run(&mut self.mixer, &mut level);
        let mut receipts: Vec<Receipt<DeckProtocol>> = Vec::new();
        while let Some(receipt) = self.ring.receipt() {
            hang_tick!();
            let ScopedReceipt::Scope(scope, receipt) = receipt else {
                panic!("expected a deck receipt");
            };
            assert_eq!(scope, self.scope);
            receipts.push(receipt);
            hang_reset!();
        }
        Ok(receipts)
    }

    /// The queues a player sends to, lent for one pass.
    pub fn with_outbox<R, F>(&mut self, run: F) -> Result<R, PlayError>
    where
        F: FnOnce(&mut Outbox<'_, S>) -> R,
    {
        let mut scope = self.ring.scope(self.scope).ok_or(PlayError::Closed)?;
        let output = output(None).get();
        let pass = DeckPass {
            mix: crate::DeckMixSettings::default(),
            suspended: false,
            now: self.now,
            delivery: FrameCount::new(0),
            output: &output,
            deck: &self.observation,
        };
        Ok(run(
            &mut Outbox::new(&mut scope, &mut self.dispatcher).in_pass(pass)
        ))
    }
}

impl<S> DeckRig<S>
where
    S: kithara_bufpool::HasPool<u8> + kithara_bufpool::HasPool<f32> + Send + Sync + 'static,
{
    /// Opens a fixture on the worker shared by this rig's prepared resources.
    ///
    /// # Errors
    /// Returns the source, lane, or dispatcher admission failure.
    #[kithara::flash(true)]
    #[kithara::flash(io)]
    #[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
    pub async fn load_fixture(
        &mut self,
        worker: &crate::PlayWorker<S>,
        item: ResourceLoad<S>,
        position: Duration,
    ) -> Result<Loaded<OpenedTrack>, PlayError> {
        let _io = kithara_platform::flash::real_io();
        let (lane_sender, lane) = item.lane_channel()?;
        let loaded = self
            .load_request_fixture(
                worker,
                LoadRequest {
                    item,
                    class: kithara_render::ServiceClass::Warm,
                    position,
                    start: TrackSettings::default().lane_start(),
                    inbox: lane,
                },
            )
            .await?;
        self.fixture_lanes.push(lane_sender);
        Ok(loaded)
    }

    /// Opens an owner's request on this rig's worker, retaining its source and inbox.
    ///
    /// # Errors
    /// Returns source, lane, or dispatcher admission failure.
    #[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
    pub async fn load_request_fixture(
        &mut self,
        worker: &crate::PlayWorker<S>,
        request: LoadRequest<ResourceLoad<S>>,
    ) -> Result<Loaded<OpenedTrack>, PlayError> {
        let dispatcher = match &mut self.fixture_dispatcher {
            Some(dispatcher) => dispatcher,
            missing @ None => {
                let (sender, inbox) = channel::<DispatcherProtocol<ResourceLoad<S>>>(
                    ChannelConfig::builder().build(),
                );
                let driver = worker
                    .start_dispatcher(inbox)
                    .map_err(|error| PlayError::Internal(format!("{error:?}")))?;
                missing.insert(FixtureDispatcher {
                    sender,
                    _driver: driver,
                    _worker: worker.clone(),
                })
            }
        };
        dispatcher
            .sender
            .send(
                When::Next,
                Batch {
                    basis: Vec::new(),
                    commands: vec![DispatcherCommand::Load(Box::new(request))],
                },
            )
            .map_err(|error| PlayError::Internal(error.to_string()))?;
        let mut deadline = None;
        let receipt = poll_fn(|context| {
            hang_tick!();
            dispatcher.sender.hold(context.waker().clone());
            if let Some(receipt) = dispatcher.sender.receipts().next() {
                hang_reset!();
                Poll::Ready(receipt)
            } else {
                hang_park!(|remaining| {
                    let deadline = deadline
                        .get_or_insert_with(|| Box::pin(kithara_platform::time::sleep(remaining)));
                    let _ = deadline.as_mut().poll(context);
                });
                Poll::Pending
            }
        })
        .await;
        dispatcher.sender.release();
        let (outcome, _) = receipt.into();
        match outcome {
            Outcome::Applied {
                data: Dispatched::Loaded(opened),
                ..
            } => Ok(opened),
            outcome => Err(PlayError::Internal(format!(
                "fixture load refused: {outcome:?}"
            ))),
        }
    }
}
