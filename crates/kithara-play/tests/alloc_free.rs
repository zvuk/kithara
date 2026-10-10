//! A deck renders on the audio thread, which must never allocate: each block measured here
//! renders inside `assert_no_alloc`, so an allocation on the render path aborts the test.
#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::num::{NonZeroU32, NonZeroUsize};

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
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
use kithara_assets::AssetStore;
use kithara_audio::{AudioObserverSlot, AudioRead, ReadOutcome, mock::TestPcmReader};
use kithara_command::{
    Batch, ChannelConfig, LevelInbox, Port, ScopeId, ScopedConfig, ScopedInbox, ScopedSender, When,
    scoped_channel,
};
use kithara_platform::time::Duration;
use kithara_play::{
    CrossfadeSettings, PlayWorker, PlayWorkerConfig, ResourceConfig, ResourceLoad, ResourceSrc,
    TrackSettings, mock,
};
use kithara_render::{
    Open,
    bridge::{DeckPart, DeckProtocol, Fade, FadeDir, SessionInbox, Slot, scope_channels},
    rt::{DeckMixer, DeckMixerConfig, StreamShape, install_render_context, publish_render_context},
};
use kithara_signal::{AudioSpec, OutputContext, SegmentId, SessionEpoch, SessionFrame};
use kithara_test_fixtures::integration_fixtures::constant_half;
use kithara_test_utils::{
    TestTempDir,
    bufpool::{TestPools, pools},
    kithara,
};
use kithara_warp::RenderContext;

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

const SAMPLE_RATE: NonZeroU32 = NonZeroU32::MIN.saturating_add(47_999);
const BLOCK: NonZeroU32 = NonZeroU32::MIN.saturating_add(127);
const ABSENT_FADES: usize = 4;

fn send(ring: &mut ScopedSender<DeckProtocol, DeckProtocol>, scope: ScopeId, part: DeckPart) {
    ring.scope(scope)
        .expect("live deck scope")
        .send(
            When::Next,
            Batch {
                basis: Vec::new(),
                commands: vec![part],
            },
        )
        .expect("the deck channel has room");
}

struct TestSessionInbox(ScopedInbox<DeckProtocol, DeckProtocol>);

impl SessionInbox for TestSessionInbox {
    fn scope(&mut self, id: ScopeId) -> Option<LevelInbox<'_, DeckProtocol>> {
        self.0.scope(id)
    }
}

#[kithara::test(tokio)]
async fn a_deck_applies_fades_for_tracks_it_does_not_hold_without_allocating(
    constant_half: &'static [u8],
) {
    let (mut ring, inbox) = scoped_channel(
        ScopedConfig::builder()
            .scope(ChannelConfig::builder().targets(ABSENT_FADES + 2).build())
            .build(),
    );
    let shape = StreamShape {
        sample_rate: SAMPLE_RATE,
        max_block_frames: BLOCK,
    };
    let config = DeckMixerConfig::builder()
        .slots(NonZeroUsize::new(ABSENT_FADES + 2).expect("held and absent slots"))
        .build();
    let scope = ring.open(config.slots().get()).expect("deck scope");
    let (_ends, inputs) = scope_channels(scope, config);
    let mut deck =
        DeckMixer::<TestSessionInbox>::new(inputs, shape, &pools()).expect("mixer pools");
    let settings = CrossfadeSettings::default();
    let spec = AudioSpec::new(2, SAMPLE_RATE);
    let mut reader = TestPcmReader::with_pcm(spec, 60.0, constant_half);
    let mut samples = vec![0.0; 60 * usize::try_from(SAMPLE_RATE.get()).expect("rate") * 2];
    let mut written = 0;
    while written < samples.len() {
        let ReadOutcome::Frames { count, .. } =
            reader.read(&mut samples[written..]).expect("generated PCM")
        else {
            panic!("generated PCM ended early")
        };
        written += count.get();
    }
    let dir = TestTempDir::new();
    let path = dir.path().join("held.wav");
    mock::write_pcm_wav(&path, &samples, spec).expect("generated float WAV");
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools()).build());
    let load: ResourceLoad<TestPools> = ResourceLoad::new(
        ResourceConfig::for_src(ResourceSrc::Path(path))
            .store(AssetStore::builder(pools()).build())
            .worker(worker.clone())
            .host_sample_rate(SAMPLE_RATE)
            .build(),
        Box::new(AudioObserverSlot::default().relay()),
    );
    let (_sender, lane_inbox) = worker.lane_channel();
    let (opened, _lane, _latency) = load
        .open(
            Duration::ZERO,
            TrackSettings::default().lane_start(),
            lane_inbox,
        )
        .await
        .expect("URI source opens");
    let held = Slot::new(0);
    let mut store = ProcStore::with_capacity(2);
    assert!(
        store.insert(TestSessionInbox(inbox)).is_ok(),
        "session inbox slot"
    );
    install_render_context(&mut store).expect("host render context slot");
    let (logger, _logs) = realtime_logger(RealtimeLoggerConfig::default());
    let mut extra = ProcExtra {
        logger,
        store,
        scratch_buffers: ConstSequentialBuffer::<f32, NUM_SCRATCH_BUFFERS>::new(
            usize::try_from(BLOCK.get()).expect("block"),
        ),
        declick_values: DeclickValues::new(BLOCK),
    };
    let mut info = ProcInfo {
        sample_rate: SAMPLE_RATE,
        frames: usize::try_from(BLOCK.get()).expect("block"),
        in_silence_mask: SilenceMask::default(),
        out_silence_mask: SilenceMask::default(),
        in_constant_mask: ConstantMask::default(),
        out_constant_mask: ConstantMask::default(),
        in_connected_mask: ConnectedMask::default(),
        out_connected_mask: ConnectedMask::default(),
        total_cpu_seconds_recip: 1.0,
        process_to_playback_delay: None,
        did_just_unbypass: false,
        last_marker_instant: InstantSamples(0),
        sample_rate_recip: f64::from(SAMPLE_RATE.get()).recip(),
        clock_samples: InstantSamples(0),
        duration_since_stream_start: Duration::ZERO,
        stream_status: StreamStatus::empty(),
        dropped_frames: 0,
    };
    publish_render_context(
        &mut extra.store,
        RenderContext::new_linear(
            OutputContext::new(
                SessionFrame::new(0)..SessionFrame::new(i64::from(BLOCK.get())),
                SAMPLE_RATE,
                SessionEpoch::new(0),
                None,
            )
            .expect("output block"),
            None,
        )
        .expect("host context"),
    )
    .expect("publish host context");

    let frames = usize::try_from(BLOCK.get()).expect("a block fits in memory");
    let mut out_l = vec![0.0f32; frames];
    let mut out_r = vec![0.0f32; frames];
    let mut render =
        |deck: &mut DeckMixer<TestSessionInbox>, info: &ProcInfo, extra: &mut ProcExtra| {
            let inputs: [&[f32]; 0] = [];
            let mut outputs = [&mut out_l[..], &mut out_r[..]];
            let buffers = ProcBuffers {
                inputs: &inputs,
                outputs: &mut outputs,
            };
            extra
                .store
                .try_get_mut::<TestSessionInbox>()
                .expect("session inbox")
                .0
                .drain();
            let _ = deck.process(info, buffers, extra);
        };

    send(
        &mut ring,
        scope,
        DeckPart::Attach {
            slot: held,
            pcm: opened.pcm,
            segment: SegmentId::FIRST,
        },
    );
    send(
        &mut ring,
        scope,
        DeckPart::Start {
            slot: held,
            fade: Fade::Crossfade(settings),
        },
    );
    ring.publish().expect("publish loaded deck");
    render(&mut deck, &info, &mut extra);

    send(
        &mut ring,
        scope,
        DeckPart::Start {
            slot: Slot::new(1),
            fade: Fade::Crossfade(settings),
        },
    );
    for index in 0..ABSENT_FADES {
        send(
            &mut ring,
            scope,
            DeckPart::Fade {
                slot: Slot::new(u16::try_from(index + 2).expect("fixture slot fits u16")),
                settings,
                dir: FadeDir::Out,
            },
        );
    }
    ring.publish().expect("publish absent-track fades");
    info.clock_samples = InstantSamples(i64::from(BLOCK.get()));
    info.duration_since_stream_start =
        Duration::from_secs_f64(f64::from(BLOCK.get()) / f64::from(SAMPLE_RATE.get()));
    publish_render_context(
        &mut extra.store,
        RenderContext::new_linear(
            OutputContext::new(
                SessionFrame::new(i64::from(BLOCK.get()))
                    ..SessionFrame::new(i64::from(BLOCK.get()) * 2),
                SAMPLE_RATE,
                SessionEpoch::new(0),
                None,
            )
            .expect("next output block"),
            None,
        )
        .expect("host context"),
    )
    .expect("publish next host context");
    let dispatch = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    tracing::dispatcher::with_default(&dispatch, || {
        assert_no_alloc(|| render(&mut deck, &info, &mut extra));
    });
}
