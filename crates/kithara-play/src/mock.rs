use std::{future::Future, num::NonZeroU32, pin::Pin, task::Poll};

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
use kithara_command::{
    Batch, ChannelConfig, Inbox, OpenError, Outcome, Receipt, ScopeId, ScopedConfig, ScopedInbox,
    ScopedReceipt, ScopedSender, Sender, When, channel, scoped_channel,
};
use kithara_render::{
    Dispatched, DispatcherCommand, DispatcherProtocol, LoadRefusal, LoadRequest, Loaded, Open,
    bridge::{DeckEvents, DeckPart, DeckProtocol, SessionInbox, Slot, scope_channels},
    mock::MockDeck,
    rt::{DeckMixer, DeckMixerConfig, StreamShape, install_render_context, publish_render_context},
};
use kithara_signal::{OutputContext, SessionEpoch, SessionFrame};
use kithara_warp::RenderContext;

use crate::{
    OpenedTrack, PlayError, ResourceLoad, TrackSettings, player::Outbox, session::SessionOutputView,
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

/// Writes generated PCM as a float WAV for tests opening a real URI source.
///
/// # Errors
/// Returns an invalid WAV geometry or the destination's write failure.
pub fn write_pcm_wav(
    path: &std::path::Path,
    samples: &[f32],
    spec: kithara_signal::AudioSpec,
) -> std::io::Result<()> {
    use std::io::{BufWriter, Error, ErrorKind, Write};

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
    let mut file = BufWriter::new(std::fs::File::create(path)?);
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
    for sample in samples {
        file.write_all(&sample.to_le_bytes())?;
    }
    file.flush()
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
pub async fn assert_prepared_render_off_bus<S>(
    prep: &crate::ResourcePrep<S>,
    output: &crate::OutputSnapshot,
    pools: &kithara_bufpool::PoolRegion<S>,
    path: &std::path::Path,
) -> Result<(), PlayError>
where
    S: kithara_bufpool::HasPool<u8> + kithara_bufpool::HasPool<f32> + Send + Sync + 'static,
{
    use kithara_events::TryRecvError;
    use kithara_render::bridge::{Fade, Slot, SlotState};
    use kithara_signal::{AudioSpec, SegmentId};

    #[derive(Debug, kithara_events::EventSet)]
    enum RenderEvents {
        Audio(kithara_audio::AudioEvent),
        Decoder(kithara_audio::DecoderEvent),
        File(kithara_file::FileEvent),
        Hls(kithara_hls::HlsEvent),
        Drm(kithara_hls::DrmEvent),
        Player(crate::PlayerEvent),
    }

    let rate = NonZeroU32::new(output.sample_rate.output()).ok_or(PlayError::Closed)?;
    let frames = NonZeroU32::new(128).ok_or(PlayError::Closed)?;
    let shape = StreamShape::new(frames, rate);
    write_pcm_wav(path, &[0.5; 8_192], AudioSpec::new(2, rate))
        .map_err(|error| PlayError::Internal(error.to_string()))?;
    let config: crate::ResourceConfig<S> =
        crate::ResourceConfig::for_src(crate::ResourceSrc::Path(path.to_owned()))
            .store(
                kithara_assets::AssetStore::builder(pools.clone())
                    .backend(kithara_assets::StorageBackend::Memory)
                    .build(),
            )
            .build();
    let prepared = prep.prepare(config, output)?;
    let mut events = prepared
        .bus
        .as_ref()
        .ok_or(PlayError::Closed)?
        .subscribe::<RenderEvents>();
    let load = ResourceLoad::new(
        prepared,
        Box::new(kithara_audio::AudioObserverSlot::default().relay()),
    );
    let (_control, inbox) = load.lane_channel()?;
    let (opened, _lane, _) = load
        .open(
            kithara_platform::time::Duration::ZERO,
            TrackSettings::default().lane_start(),
            inbox,
        )
        .await
        .map_err(|error| PlayError::Internal(format!("{error:?}")))?;
    let mut mixer = MixerRig::new(DeckMixerConfig::default(), shape, pools)?;
    let slot = Slot::new(0);
    mixer.send(
        When::Next,
        DeckPart::Attach {
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
    while events.try_recv().is_ok() {}
    let mut position = 0.0;
    for block in 0..2 {
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
    }
    Ok(())
}

/// The scoped command inbox installed in a mock audio-thread processor store.
pub struct MixerInbox(pub ScopedInbox<DeckProtocol, DeckProtocol>);

impl SessionInbox for MixerInbox {
    fn scope(&mut self, scope: ScopeId) -> Option<kithara_command::LevelInbox<'_, DeckProtocol>> {
        self.0.scope(scope)
    }
}

/// A real offline mixer driven by exact host blocks and scoped deck commands.
pub struct MixerRig {
    pub ring: ScopedSender<DeckProtocol, DeckProtocol>,
    pub scope: ScopeId,
    pub ends: kithara_render::bridge::DeckEnds,
    mixer: DeckMixer<MixerInbox>,
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
        if store.insert(MixerInbox(inbox)).is_err() {
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
            .try_get_mut::<MixerInbox>()
            .ok_or(PlayError::Closed)?
            .0
            .drain();
        let info = ProcInfo {
            sample_rate: self.shape.sample_rate,
            frames,
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
            duration_since_stream_start: kithara_platform::time::Duration::try_from_secs_f64(
                num_traits::ToPrimitive::to_f64(&start)
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
}

struct FixtureDispatcher<S> {
    sender: Sender<DispatcherProtocol<ResourceLoad<S>>>,
    driver: Pin<Box<dyn Future<Output = ()>>>,
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
        let (ends, inputs) = scope_channels(scope, config);
        let (dispatcher, opens) = channel(ChannelConfig::builder().build());
        Ok(Self {
            ring,
            scope,
            inbox,
            mixer: MockDeck::new(inputs),
            events: ends.events,
            dispatcher,
            opens,
            fixture_dispatcher: None,
            fixture_lanes: Vec::new(),
        })
    }

    /// The queues a player sends to, lent for one pass.
    pub fn with_outbox<R, F>(&mut self, run: F) -> Result<R, PlayError>
    where
        F: FnOnce(&mut Outbox<'_, S>) -> R,
    {
        let mut scope = self.ring.scope(self.scope).ok_or(PlayError::Closed)?;
        Ok(run(&mut Outbox::new(&mut scope, &mut self.dispatcher)))
    }

    /// Answers the oldest open the dispatcher holds with `opened` and returns
    /// its receipt; `None` when no open is waiting.
    pub fn open(
        &mut self,
        opened: Result<Loaded<OpenedTrack>, LoadRefusal>,
    ) -> Option<Receipt<DispatcherProtocol<ResourceLoad<S>>>> {
        self.opens.drain();
        let due = self.opens.next_due((), 1)?;
        match opened {
            Ok(opened) => due.apply(Dispatched::Loaded(opened)),
            Err(refusal) => due.refuse(refusal),
        }
        self.dispatcher.receipts().next()
    }

    /// Plays the deck's block at `at` and returns the receipts of the batches
    /// it applied; a slot stopped there stood at `stopped_at` seconds.
    pub fn block(
        &mut self,
        at: SessionFrame,
        stopped_at: f64,
    ) -> Result<Vec<Receipt<DeckProtocol>>, PlayError> {
        self.pass(|mixer, level| mixer.block(level, at, stopped_at))
    }

    pub fn end(
        &mut self,
        slot: Slot,
        at: SessionFrame,
    ) -> Result<Vec<Receipt<DeckProtocol>>, PlayError> {
        self.pass(|mixer, level| mixer.end(level, slot, at))
    }

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
            let ScopedReceipt::Scope(scope, receipt) = receipt else {
                panic!("expected a deck receipt");
            };
            assert_eq!(scope, self.scope);
            receipts.push(receipt);
        }
        Ok(receipts)
    }
}

impl<S: 'static> DeckRig<S> {
    /// Opens a fixture through the real dispatcher and retains its resident lane.
    ///
    /// # Errors
    /// Returns the source, lane, or dispatcher admission failure.
    pub async fn load_fixture(
        &mut self,
        item: ResourceLoad<S>,
        position: kithara_platform::time::Duration,
    ) -> Result<Loaded<OpenedTrack>, PlayError> {
        let (lane_sender, lane) = item.lane_channel()?;
        let dispatcher = self.fixture_dispatcher.get_or_insert_with(|| {
            let (sender, inbox) =
                channel::<DispatcherProtocol<ResourceLoad<S>>>(ChannelConfig::builder().build());
            FixtureDispatcher {
                sender,
                driver: Box::pin(kithara_render::dispatch(inbox)),
            }
        });
        dispatcher
            .sender
            .send(
                When::Next,
                Batch {
                    basis: Vec::new(),
                    commands: vec![DispatcherCommand::Load(Box::new(LoadRequest {
                        class: kithara_render::ServiceClass::Warm,
                        item,
                        position,
                        start: TrackSettings::default().lane_start(),
                        inbox: lane,
                    }))],
                },
            )
            .map_err(|error| PlayError::Internal(error.to_string()))?;
        let receipt = futures::future::poll_fn(|context| {
            let _ = dispatcher.driver.as_mut().poll(context);
            dispatcher
                .sender
                .receipts()
                .next()
                .map_or(Poll::Pending, Poll::Ready)
        })
        .await;
        let (outcome, _) = receipt.into();
        match outcome {
            Outcome::Applied {
                data: Dispatched::Loaded(opened),
                ..
            } => {
                self.fixture_lanes.push(lane_sender);
                Ok(opened)
            }
            outcome => Err(PlayError::Internal(format!(
                "fixture load refused: {outcome:?}"
            ))),
        }
    }
}
