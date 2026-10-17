#![cfg(not(target_arch = "wasm32"))]

use std::{future::poll_fn, num::NonZeroUsize, task::Poll};

use kithara::{
    assets::{AssetStore, StorageBackend},
    audio::AudioObserverSlot,
    platform::time::Duration,
    play::{
        DispatcherProtocol, LoadRefusal, PlayWorker, PlayWorkerConfig, ResourceConfig,
        ResourceLoad, ResourceSrc,
    },
    sync::SpeedCurve,
    warp::WarpConfig,
};
use kithara_command::{Batch, ChannelConfig, Outcome, Receipt, Rejection, Sender, When, channel};
use kithara_integration_tests::usdt_trace::{self, Scope};
use kithara_render::{Dispatched, DispatcherCommand, LaneStart, LoadRequest, ServiceClass};
use kithara_test_fixtures::fixtures::tone_mp3;
use kithara_test_utils::{TestTempDir, temp_dir};

use crate::bufpool_ext::{TestPools, pools};

fn opened(trace: &Scope) -> usize {
    trace.events_of("source_opened").len()
}

#[kithara::test(tokio, timeout(Duration::from_secs(30)))]
async fn a_load_opens_its_source_once_and_a_load_past_capacity_opens_nothing(
    temp_dir: TestTempDir,
) {
    let worker = PlayWorker::new(
        PlayWorkerConfig::builder(pools())
            .capacity(NonZeroUsize::MIN)
            .build(),
    );
    let trace = usdt_trace::scope();

    let (mut sender, inbox) = channel::<Loads>(ChannelConfig::builder().build());
    let _dispatcher = worker
        .start_dispatcher(inbox)
        .expect("the worker owns its dispatcher");
    sender
        .send(When::Next, load(&worker, &temp_dir, "a.mp3"))
        .expect("first load batch");
    let receipt = answered(&mut sender).await;
    let (
        Outcome::Applied {
            data: Dispatched::Loaded(held),
            ..
        },
        _,
    ) = receipt.into()
    else {
        panic!("the first load fits the worker");
    };
    assert_eq!(opened(&trace), 1, "a load opens its source once");

    sender
        .send(When::Next, load(&worker, &temp_dir, "b.mp3"))
        .expect("second load batch");
    let refused = answered(&mut sender).await;
    assert!(
        matches!(
            refused.outcome(),
            Outcome::Rejected(Rejection::Refused(LoadRefusal::Capacity { capacity: 1 }))
        ),
        "a load past capacity is refused for capacity"
    );
    assert_eq!(
        opened(&trace),
        1,
        "a load the worker cannot hold opens nothing"
    );
    sender
        .send(
            When::Next,
            Batch {
                basis: Vec::new(),
                commands: vec![DispatcherCommand::Release(held.lane)],
            },
        )
        .expect("release lane batch");
    assert!(matches!(
        answered(&mut sender).await.outcome(),
        Outcome::Applied {
            data: Dispatched::Released,
            ..
        }
    ));
    sender
        .send(When::Next, load(&worker, &temp_dir, "c.mp3"))
        .expect("reload batch");
    let reloaded = answered(&mut sender).await;
    assert!(
        matches!(
            reloaded.outcome(),
            Outcome::Applied {
                data: Dispatched::Loaded(_),
                ..
            }
        ),
        "a released lane frees its slot"
    );
    assert_eq!(opened(&trace), 2, "the next load opens its own source");
}

type Loads = DispatcherProtocol<ResourceLoad<TestPools>>;

/// A one-load batch opening a local MP3 from its own file under `dir`.
fn load(worker: &PlayWorker<TestPools>, dir: &TestTempDir, name: &str) -> Batch<Loads> {
    let path = dir.write(name, tone_mp3());
    let store = AssetStore::builder(worker.pools().clone())
        .backend(StorageBackend::Disk {
            root: dir.path().join(format!("{name}.cache")),
        })
        .build();
    let warp = WarpConfig::builder().build();
    let start = LaneStart {
        speed: SpeedCurve::Constant(warp.speed()),
        keylock: warp.keylock(),
        backend: warp.backend(),
    };
    let config = ResourceConfig::for_src(ResourceSrc::Path(path))
        .store(store)
        .worker(worker.clone())
        .warp(warp)
        .build();
    Batch {
        basis: Vec::new(),
        commands: vec![DispatcherCommand::Load(Box::new(LoadRequest {
            class: ServiceClass::Warm,
            item: ResourceLoad::new(config, Box::new(AudioObserverSlot::default().relay())),
            position: Duration::ZERO,
            start,
            inbox: worker.lane_channel().1,
        }))],
    }
}

/// The next receipt from the dispatcher's configured worker owner.
#[kithara_test_utils::kithara::hang_watchdog(ctx = String)]
async fn answered(sender: &mut Sender<Loads>) -> Receipt<Loads> {
    // WHY: The dispatcher may be opening a fixture on an uncounted real thread.
    let _real_io = kithara::platform::flash::real_io();
    let budget = __hang_detector.remaining();
    let receipt = poll_fn(|cx| {
        hang_tick!("dispatcher waits for a load or priority answer".to_owned());
        sender.hold(cx.waker().clone());
        sender.receipts().next().map_or(Poll::Pending, Poll::Ready)
    });
    let receipt = kithara::platform::time::timeout(budget, receipt).await;
    sender.release();
    let receipt = receipt.expect("dispatcher answers within its watchdog budget");
    hang_reset!();
    receipt
}

#[kithara::test(tokio, timeout(Duration::from_secs(30)))]
async fn the_dispatcher_opens_each_load_once_and_answers_a_load_past_capacity_for_capacity(
    temp_dir: TestTempDir,
) {
    let worker = PlayWorker::new(
        PlayWorkerConfig::builder(pools())
            .capacity(NonZeroUsize::MIN)
            .build(),
    );
    let trace = usdt_trace::scope();
    let (mut sender, inbox) = channel::<Loads>(ChannelConfig::builder().build());
    let _dispatcher = worker
        .start_dispatcher(inbox)
        .expect("the worker owns its dispatcher");

    let first = sender
        .send(When::Next, load(&worker, &temp_dir, "a.mp3"))
        .expect("the channel has room");
    let receipt = answered(&mut sender).await;
    assert_eq!(receipt.seq(), first);
    let (
        Outcome::Applied {
            data: Dispatched::Loaded(held),
            ..
        },
        _,
    ) = receipt.into()
    else {
        panic!("the first load fits the worker");
    };
    assert_eq!(opened(&trace), 1, "a load opens its source once");

    let second = sender
        .send(When::Next, load(&worker, &temp_dir, "b.mp3"))
        .expect("the channel has room");
    let receipt = answered(&mut sender).await;
    assert_eq!(receipt.seq(), second);
    assert!(
        matches!(
            receipt.outcome(),
            Outcome::Rejected(Rejection::Refused(LoadRefusal::Capacity { capacity: 1 }))
        ),
        "a load past capacity is answered for capacity: {:?}",
        receipt.outcome()
    );
    assert_eq!(
        opened(&trace),
        1,
        "a load the worker cannot hold opens nothing"
    );
    sender
        .send(
            When::Next,
            Batch {
                basis: Vec::new(),
                commands: vec![DispatcherCommand::SetPriority(
                    held.lane,
                    ServiceClass::Audible,
                )],
            },
        )
        .expect("prioritize resident lane");
    assert!(
        matches!(
            answered(&mut sender).await.outcome(),
            Outcome::Applied {
                data: Dispatched::Prioritized,
                ..
            }
        ),
        "the dispatcher runs while its sender lives"
    );
}

/// The worker refills every deck's ring, and a ring drains while its worker
/// waits for a CPU, so the worker asks the OS to run it ahead of ordinary work.
/// Whether the OS grants that is the machine's policy, not the worker's.
#[kithara::test(tokio, flash(false), timeout(Duration::from_secs(30)))]
async fn the_play_worker_asks_the_os_to_schedule_it_as_an_audio_feed() {
    let trace = usdt_trace::scope();
    let _worker = PlayWorker::new(PlayWorkerConfig::builder(pools()).build());

    trace
        .wait_for(|events| {
            events
                .iter()
                .any(|event| event.probe == "thread_class" && event.field("audio_feed") == Some(1))
        })
        .await;
    drop(trace);
}

#[cfg(not(target_os = "android"))]
#[kithara::test(native, tokio, timeout(Duration::from_secs(30)))]
#[kithara_test_utils::kithara::hang_watchdog(ctx = String)]
async fn a_nonzero_initial_media_load_reaches_its_raw_source_target_without_reopening() {
    use std::num::NonZeroU32;

    use kithara::{
        audio::{
            AudioDecoderConfig, AudioObserverSlot, DecoderBackend as ObservedBackend,
            DecoderChangeCause, DecoderEvent,
        },
        decode::{DecoderBackend, GaplessMode},
        events::{EventBus, TrackId},
        play::{
            DeckMixSettings, DeckMixerConfig, DeckPass, Outbox, OutputSnapshot, Player,
            PlayerConfig, PlayerImpl, SessionOutputView, Slot, StreamShape, TrackCommand,
            TrackReceipt, TrackSettings, TrackStatus, mock,
        },
        signal::{AudioSpec, FrameCount, SessionFrame},
    };
    use kithara_command::ScopedReceipt;
    use kithara_test_fixtures::assets::rhythm_mp3_deck_b_120bpm_48k;

    fn with_load_pass<R>(
        mixer: &mut mock::MixerRig,
        dispatcher: &mut Sender<Loads>,
        output: &OutputSnapshot,
        now: SessionFrame,
        run: impl FnOnce(&mut Outbox<'_, TestPools>) -> R,
    ) -> R {
        let deck = mixer.ends.snapshot.read();
        let mut scope = mixer.ring.scope(mixer.scope).expect("live mixer scope");
        let mut out = Outbox::new(&mut scope, dispatcher).in_pass(DeckPass {
            mix: DeckMixSettings::default(),
            suspended: false,
            now,
            delivery: FrameCount::new(0),
            output,
            deck,
        });
        run(&mut out)
    }

    let encoded = rhythm_mp3_deck_b_120bpm_48k();
    let rate = NonZeroU32::new(48_000).expect("source and host rate");
    let spec = AudioSpec::new(2, rate);
    let media_origin = spec.duration_for(1_105).expect("tagged recording origin");
    let target = Duration::from_secs(2);
    let expected_raw = media_origin + target;
    let expected_raw_frame = spec
        .frame_at(expected_raw)
        .expect("initial raw target frame");
    let trace = usdt_trace::scope();
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools()).build());
    let bus = EventBus::default();
    let mut decoder_events = bus.subscribe::<DecoderEvent>();
    let config: ResourceConfig<TestPools> = ResourceConfig::for_src(ResourceSrc::Path(
        encoded.path().expect("stored MP3 fixture").to_owned(),
    ))
    .store(
        AssetStore::builder(worker.pools().clone())
            .backend(StorageBackend::Memory)
            .build(),
    )
    .worker(worker.clone())
    .host_sample_rate(rate)
    .events(bus)
    .decoder(
        AudioDecoderConfig::builder()
            .backend(DecoderBackend::Symphonia)
            .gapless_mode(GaplessMode::MediaOnly)
            .build(),
    )
    .build();
    let item = ResourceLoad::new(config, Box::new(AudioObserverSlot::default().relay()));
    let (mut dispatcher, inbox) = channel::<Loads>(ChannelConfig::builder().build());
    let _driver = worker
        .start_dispatcher(inbox)
        .expect("one dispatcher owner");
    let output = SessionOutputView::new(rate).get();
    let shape = StreamShape::new(NonZeroU32::new(64).expect("host block"), rate);
    let mut mixer =
        mock::MixerRig::new(DeckMixerConfig::default(), shape, worker.pools()).expect("real mixer");
    let slot = Slot::new(0);
    let mut player = PlayerImpl::<TestPools>::new(PlayerConfig {
        item: TrackId::allocate(),
        slot: Some(slot),
        settings: TrackSettings::default(),
    })
    .expect("player settings");
    let loaded = with_load_pass(
        &mut mixer,
        &mut dispatcher,
        &output,
        SessionFrame::new(0),
        |out| {
            player.apply(
                TrackCommand::Load {
                    item,
                    position: target,
                },
                out,
            )
        },
    )
    .expect("nonzero media Load")
    .expect("one Load sequence");
    assert_eq!(player.snapshot().position, target);
    let receipt = answered(&mut dispatcher).await;
    assert_eq!(receipt.seq(), loaded);
    let Outcome::Applied {
        data: Dispatched::Loaded(_),
        ..
    } = receipt.outcome()
    else {
        panic!("the real dispatcher opens the tagged MP3");
    };
    assert_eq!(opened(&trace), 1);
    let mut initial_changes = Vec::new();
    while let Ok(envelope) = decoder_events.try_recv() {
        hang_tick!("initial decoder receipt evidence".to_owned());
        if let DecoderEvent::DecoderChanged { backend, cause, .. } = envelope.event {
            initial_changes.push((backend, cause));
        }
        hang_reset!();
    }
    assert_eq!(
        initial_changes,
        vec![(ObservedBackend::Symphonia, DecoderChangeCause::Initial)]
    );
    with_load_pass(
        &mut mixer,
        &mut dispatcher,
        &output,
        SessionFrame::new(0),
        |out| player.settle(TrackReceipt::Loaded(receipt), out),
    );

    let mut left = [0.0; 64];
    let mut right = [0.0; 64];
    mixer
        .block(SessionFrame::new(0), [&mut left, &mut right])
        .expect("actual attachment block");
    let raw = mixer.ends.snapshot.read().slots[usize::from(slot.get())];
    let mark = raw.mark.expect("first real source mark");
    let source = raw.source.expect("first real source timing");
    assert_eq!(source.segment, mark.lane.segment);
    assert_eq!(source.media_origin, Some(media_origin));
    assert_eq!(
        spec.frame_at(mark.position).expect("raw marked frame"),
        expected_raw_frame
    );
    assert_eq!(mark.position, expected_raw);
    let mut attached = Vec::new();
    while let Some(receipt) = mixer.ring.receipt() {
        hang_tick!("actual attachment receipt".to_owned());
        let ScopedReceipt::Scope(scope, receipt) = receipt else {
            panic!("scoped attachment answer");
        };
        assert_eq!(scope, mixer.scope);
        attached.push(receipt);
        hang_reset!();
    }
    assert!(
        !attached.is_empty(),
        "the mixer returns its actual attachment"
    );
    for receipt in attached {
        let seq = receipt.seq();
        let (outcome, mut batch) = receipt.into();
        with_load_pass(
            &mut mixer,
            &mut dispatcher,
            &output,
            SessionFrame::new(0),
            |out| {
                player.settle(
                    TrackReceipt::Deck {
                        seq,
                        outcome: &outcome,
                        batch: &mut batch,
                    },
                    out,
                )
            },
        );
    }
    with_load_pass(
        &mut mixer,
        &mut dispatcher,
        &output,
        SessionFrame::new(0),
        |out| player.tick(SessionFrame::new(0), out),
    );
    assert_eq!(player.snapshot().status, TrackStatus::Loaded);
    assert_eq!(player.snapshot().position, target);
    let started = with_load_pass(
        &mut mixer,
        &mut dispatcher,
        &output,
        SessionFrame::new(64),
        |out| player.apply(TrackCommand::Play { at: When::Next }, out),
    )
    .expect("start loaded source")
    .expect("Start sequence");
    mixer
        .block(SessionFrame::new(64), [&mut left, &mut right])
        .expect("consume actual source PCM");
    assert!(left.iter().chain(&right).all(|sample| sample.is_finite()));
    assert!(left.iter().chain(&right).any(|sample| sample.abs() > 0.0));
    let consumed = mixer.ends.snapshot.read().slots[usize::from(slot.get())];
    let source = consumed.source.expect("consumed source timing");
    assert_eq!(
        source.segment,
        consumed.mark.expect("consumed source mark").lane.segment
    );
    assert_eq!(source.media_origin, Some(media_origin));
    assert_eq!(
        spec.frame_at(consumed.mark.expect("consumed source mark").position)
            .expect("consumed raw frame"),
        expected_raw_frame + 64,
    );
    let mut played = Vec::new();
    while let Some(receipt) = mixer.ring.receipt() {
        hang_tick!("actual Start receipt".to_owned());
        let ScopedReceipt::Scope(scope, receipt) = receipt else {
            panic!("scoped Start answer");
        };
        assert_eq!(scope, mixer.scope);
        played.push(receipt);
        hang_reset!();
    }
    assert!(played.iter().any(|receipt| receipt.seq() == started
        && matches!(receipt.outcome(), Outcome::Applied { at, .. } if *at == SessionFrame::new(64))));
    for receipt in played {
        let seq = receipt.seq();
        let (outcome, mut batch) = receipt.into();
        with_load_pass(
            &mut mixer,
            &mut dispatcher,
            &output,
            SessionFrame::new(128),
            |out| {
                player.settle(
                    TrackReceipt::Deck {
                        seq,
                        outcome: &outcome,
                        batch: &mut batch,
                    },
                    out,
                )
            },
        );
    }
    with_load_pass(
        &mut mixer,
        &mut dispatcher,
        &output,
        SessionFrame::new(128),
        |out| player.tick(SessionFrame::new(128), out),
    );
    assert_eq!(
        player.snapshot().status,
        TrackStatus::Playing {
            since: SessionFrame::new(64)
        }
    );
    assert_eq!(
        spec.frame_at(player.snapshot().position)
            .expect("public media frame after actual PCM"),
        spec.frame_at(target).expect("commanded media frame") + 64,
    );
    assert_eq!(
        opened(&trace),
        1,
        "initial media translation never reopens the source"
    );
}

#[cfg(not(target_os = "android"))]
#[kithara::test(native, tokio, timeout(Duration::from_secs(30)))]
#[kithara_test_utils::kithara::hang_watchdog(ctx = String)]
async fn a_nonzero_initial_hls_load_returns_while_its_segment_is_pending_and_admits_only_real_pcm()
{
    use std::{future::Future, num::NonZeroU32, pin::Pin};

    use kithara::{
        audio::AudioEvent,
        events::EventBus,
        platform::{flash::real_io, time, tokio::sync::oneshot},
        signal::AudioSpec,
    };
    use kithara_render::dispatch;

    let _real_io = real_io();
    let rate = NonZeroU32::new(44_100).expect("source and host rate");
    let spec = AudioSpec::new(2, rate);
    let target = Duration::from_millis(5_500);
    let (_helper, hls, gate) = initial_hls::fixture(spec, target).await;

    let temp = temp_dir();
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools()).build());
    let bus = EventBus::default();
    let mut events = bus.subscribe::<AudioEvent>();
    let item = initial_hls::resource(&worker, &hls, temp.path(), rate, bus);
    let (returned, mut observed) = oneshot::channel();
    let (mut sender, inbox) =
        channel::<DispatcherProtocol<initial_hls::ObservedLoad>>(ChannelConfig::builder().build());
    let (_control, lane_inbox) = worker.lane_channel();
    let warp = WarpConfig::builder().build();
    let trace = usdt_trace::scope();
    let load_seq = sender
        .send(
            When::Next,
            Batch {
                basis: Vec::new(),
                commands: vec![DispatcherCommand::Load(Box::new(LoadRequest {
                    class: ServiceClass::Warm,
                    item: initial_hls::ObservedLoad { item, returned },
                    position: target,
                    start: LaneStart {
                        speed: SpeedCurve::Constant(warp.speed()),
                        keylock: warp.keylock(),
                        backend: warp.backend(),
                    },
                    inbox: lane_inbox,
                }))],
            },
        )
        .expect("nonzero initial HLS Load");
    let mut dispatcher = Box::pin(dispatch(inbox));
    hang_tick!("initial Open must return while the target GET is held".to_owned());
    time::timeout(
        __hang_detector.remaining(),
        poll_fn(|cx| {
            assert!(dispatcher.as_mut().poll(cx).is_pending());
            match Pin::new(&mut observed).poll(cx) {
                Poll::Ready(result) => {
                    result.expect("actual ResourceLoad::open returned");
                    Poll::Ready(())
                }
                Poll::Pending => {
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
            }
        }),
    )
    .await
    .expect("Open returns without the target body");
    hang_reset!();
    hang_tick!("the downloader reaches the held target GET".to_owned());
    time::timeout(
        __hang_detector.remaining(),
        poll_fn(|cx| {
            assert!(dispatcher.as_mut().poll(cx).is_pending());
            assert!(sender.receipts().next().is_none(), "Loaded is still closed");
            if gate.requested() > 0 {
                Poll::Ready(())
            } else {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }),
    )
    .await
    .expect("the held target body is actually requested");
    hang_reset!();
    assert_eq!(opened(&trace), 1);
    initial_hls::assert_held_events(&mut events);
    assert!(sender.receipts().next().is_none());

    gate.release();
    hang_tick!("ordinary dispatcher preparation admits the released PCM".to_owned());
    let receipt = time::timeout(
        __hang_detector.remaining(),
        poll_fn(|cx| {
            assert!(dispatcher.as_mut().poll(cx).is_pending());
            sender.receipts().next().map_or_else(
                || {
                    cx.waker().wake_by_ref();
                    Poll::Pending
                },
                Poll::Ready,
            )
        }),
    )
    .await
    .expect("released initial load admits its PCM");
    hang_reset!();
    assert_eq!(receipt.seq(), load_seq);
    let (
        Outcome::Applied {
            data: Dispatched::Loaded(loaded),
            ..
        },
        _,
    ) = receipt.into()
    else {
        panic!("the released initial HLS load succeeds");
    };
    assert!(sender.receipts().next().is_none());
    initial_hls::assert_applied_events(&mut events);

    initial_hls::assert_pcm(&worker, loaded.opened, spec, target);
    poll_fn(|cx| {
        assert!(dispatcher.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    assert!(sender.receipts().next().is_none());
    initial_hls::assert_settled_events(&mut events);
    assert_eq!(
        opened(&trace),
        1,
        "pending continuation does not reopen the source"
    );
}

#[cfg(not(target_os = "android"))]
mod initial_hls {
    use std::{fmt, num::NonZeroU32, path::Path};

    use kithara::{
        abr::AbrMode,
        audio::{AudioDecoderConfig, AudioEvent, SeekLifecycleStage},
        decode::{DecoderBackend, GaplessMode},
        events::{EventBus, EventReceiver, TrackId},
        platform::tokio::sync::oneshot,
        play::{DeckMixerConfig, DeckPart, OpenedTrack, StreamShape, mock},
        signal::{AudioSpec, SegmentId, SessionFrame},
        stream::AudioCodec,
    };
    use kithara_command::Inbox;
    use kithara_integration_tests::{
        CreatedHls, SegmentGateHandle, TestServerHelper,
        hls_fixture::{create_test_downloader, create_test_net},
        hls_server::packaged_ladder,
        test_defaults::packaged_content_frames,
    };
    use kithara_render::{LaneProtocol, LaneTask, Open, OpenResult, bridge::Fade};
    use kithara_test_fixtures::{
        fmp4::GaplessEncoding,
        hls_fixtures::{frame_samples, load_variant},
        signal::Wave,
        variant_input::VariantInput,
    };

    use super::{
        AudioObserverSlot, Duration, LaneStart, PlayWorker, ResourceConfig, ResourceLoad,
        ResourceSrc, TestPools, When,
    };

    pub(super) struct ObservedLoad {
        pub(super) item: ResourceLoad<TestPools>,
        pub(super) returned: oneshot::Sender<()>,
    }

    impl fmt::Debug for ObservedLoad {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_tuple("ObservedLoad")
                .field(&self.item)
                .finish()
        }
    }

    impl Open for ObservedLoad {
        type Opened = <ResourceLoad<TestPools> as Open>::Opened;
        type Lane = <ResourceLoad<TestPools> as Open>::Lane;

        async fn open(
            self,
            position: Duration,
            start: LaneStart,
            inbox: Inbox<LaneProtocol>,
        ) -> OpenResult<Self::Opened, Self::Lane> {
            let (opened, mut lane, latency) = self.item.open(position, start, inbox).await?;
            assert!(matches!(lane.preload_status(), Ok(false)));
            assert!(opened.pcm.next_chunk_info().is_none());
            assert_eq!(opened.pcm.media_origin(), None);
            self.returned
                .send(())
                .expect("the test observes the real open");
            Ok((opened, lane, latency))
        }
    }

    pub(super) async fn fixture(
        spec: AudioSpec,
        target: Duration,
    ) -> (TestServerHelper, CreatedHls, SegmentGateHandle) {
        let rate = spec.sample_rate;
        let expected_frame = spec.frame_at(target).expect("raw target frame");
        let packet_frames = frame_samples(AudioCodec::AacLc).expect("prepared AAC frame size");
        let segment_frames = packaged_content_frames(4 * 44_100, packet_frames, 1)
            .expect("packet-aligned segment frames");
        let package = load_variant(
            &VariantInput::builder()
                .codec(AudioCodec::AacLc)
                .sample_rate(rate.get())
                .channels(2)
                .content_frames(segment_frames * 3)
                .packets_per_segment(segment_frames / packet_frames)
                .signal(Wave::Sawtooth)
                .start_frame(0)
                .bit_rate(128_000)
                .timescale(rate.get())
                .encoder_delay(0)
                .trailing_delay(0)
                .gapless_encoding(GaplessEncoding::None)
                .build(),
        )
        .expect("the same prepared package served by packaged_ladder");
        let segment_start = Duration::from_secs_f64(package.segment_durations_secs[0]);
        let segment_end =
            segment_start + Duration::from_secs_f64(package.segment_durations_secs[1]);
        assert!(segment_start < target && target < segment_end);
        assert!(expected_frame > u64::try_from(segment_frames).expect("segment frames fit u64"));
        assert!(expected_frame < u64::try_from(segment_frames * 2).expect("two segments fit u64"));

        let helper = TestServerHelper::new().await;
        let hls = helper
            .create_hls(packaged_ladder())
            .await
            .expect("packaged HLS");
        let gate = helper.register_segment_gate(hls.token(), 0, 1);
        let headers = create_test_net()
            .head(hls.segment_url(0, 1), None)
            .await
            .expect("the held segment still publishes its true size");
        let served_size: usize = headers
            .get("content-length")
            .expect("segment Content-Length")
            .parse()
            .expect("segment byte length");
        assert_eq!(served_size, package.media_segments[1].len());
        assert!(served_size > 0);
        assert_eq!(gate.head_requested(), 1);
        assert_eq!(gate.requested(), 0);

        (helper, hls, gate)
    }

    pub(super) fn resource(
        worker: &PlayWorker<TestPools>,
        hls: &CreatedHls,
        path: &Path,
        rate: NonZeroU32,
        bus: EventBus,
    ) -> ResourceLoad<TestPools> {
        let config: ResourceConfig<TestPools> = ResourceConfig::for_src(
            ResourceSrc::parse(hls.master_url().as_str()).expect("HLS master URL"),
        )
        .store(kithara_integration_tests::disk_asset_store(path))
        .downloader(create_test_downloader())
        .initial_abr_mode(AbrMode::manual(0))
        .worker(worker.clone())
        .host_sample_rate(rate)
        .events(bus)
        .decoder(
            AudioDecoderConfig::builder()
                .backend(DecoderBackend::Symphonia)
                .gapless_mode(GaplessMode::Disabled)
                .build(),
        )
        .build();
        ResourceLoad::new(config, Box::new(AudioObserverSlot::default().relay()))
    }

    pub(super) fn assert_held_events(events: &mut EventReceiver<AudioEvent>) {
        let mut requests = 0;
        while let Ok(envelope) = events.try_recv() {
            match envelope.event {
                AudioEvent::SeekLifecycle {
                    stage: SeekLifecycleStage::SeekRequest,
                    ..
                } => requests += 1,
                AudioEvent::SeekLifecycle {
                    stage: SeekLifecycleStage::SeekApplied,
                    ..
                }
                | AudioEvent::SeekComplete { .. }
                | AudioEvent::SeekRejected { .. }
                | AudioEvent::TrackFailed { .. }
                | AudioEvent::EndOfStream => {
                    panic!("the held seek cannot settle: {:?}", envelope.event)
                }
                _ => {}
            }
        }
        assert_eq!(requests, 1);
    }

    pub(super) fn assert_applied_events(events: &mut EventReceiver<AudioEvent>) {
        let mut applied = 0;
        while let Ok(envelope) = events.try_recv() {
            match envelope.event {
                AudioEvent::SeekLifecycle {
                    stage: SeekLifecycleStage::SeekApplied,
                    ..
                } => applied += 1,
                AudioEvent::SeekRejected { .. } | AudioEvent::TrackFailed { .. } => {
                    panic!("released initial seek failed: {:?}", envelope.event);
                }
                _ => {}
            }
        }
        assert_eq!(applied, 1);
    }

    pub(super) fn assert_settled_events(events: &mut EventReceiver<AudioEvent>) {
        while let Ok(envelope) = events.try_recv() {
            assert!(
                !matches!(
                    envelope.event,
                    AudioEvent::SeekLifecycle {
                        stage: SeekLifecycleStage::SeekApplied,
                        ..
                    } | AudioEvent::SeekRejected { .. }
                        | AudioEvent::TrackFailed { .. }
                ),
                "settled initial seek cannot settle again: {:?}",
                envelope.event
            );
        }
    }

    pub(super) fn assert_pcm(
        worker: &PlayWorker<TestPools>,
        opened: OpenedTrack,
        spec: AudioSpec,
        target: Duration,
    ) {
        let rate = spec.sample_rate;
        let expected_frame = spec.frame_at(target).expect("raw target frame");
        let first = opened.pcm.next_chunk_info().expect("actual first PCM");
        assert_eq!(first.spec, spec);
        assert_eq!(first.timestamp, target);
        assert_eq!(first.frame_offset, expected_frame);
        assert_eq!(first.media_origin, Some(Duration::ZERO));
        assert_eq!(first.segment_index, Some(1));
        assert_eq!(first.variant_index, Some(0));
        assert_eq!(first.segment, SegmentId::FIRST);
        assert_eq!(first.lane_frame, 0);
        assert_eq!(
            first.source_span.and_then(|span| span.position_at(0)),
            Some(target)
        );
        let shape = StreamShape::new(NonZeroU32::new(64).expect("host block"), rate);
        let mut mixer = mock::MixerRig::new(DeckMixerConfig::default(), shape, worker.pools())
            .expect("real mixer");
        let slot = kithara::play::Slot::new(0);
        mixer
            .send(
                When::Next,
                DeckPart::Attach {
                    item_id: TrackId::allocate(),
                    slot,
                    pcm: opened.pcm,
                    segment: SegmentId::FIRST,
                },
            )
            .expect("attach actual HLS receiver");
        let mut left = [0.0; 64];
        let mut right = [0.0; 64];
        mixer
            .block(SessionFrame::new(0), [&mut left, &mut right])
            .expect("attach block");
        let attached = mixer.ends.snapshot.read().slots[0];
        let mark = attached.mark.expect("first current-segment raw mark");
        let source = attached.source.expect("actual source metadata");
        assert_eq!(mark.lane.segment, first.segment);
        assert_eq!(mark.lane.frame, 0);
        assert_eq!(mark.position, target);
        assert_eq!(source.segment_index, Some(1));
        assert_eq!(source.variant_index, Some(0));
        assert_eq!(source.media_origin, Some(Duration::ZERO));
        assert_eq!(source.segment, mark.lane.segment);
        mixer
            .send(
                When::Next,
                DeckPart::Start {
                    slot,
                    fade: Fade::Declick,
                },
            )
            .expect("start actual HLS PCM");
        mixer
            .block(SessionFrame::new(64), [&mut left, &mut right])
            .expect("consume HLS PCM");
        assert!(left.iter().chain(&right).all(|sample| sample.is_finite()));
        assert!(left.iter().chain(&right).any(|sample| sample.abs() > 0.0));
        let consumed = mixer.ends.snapshot.read().slots[0];
        let mark = consumed.mark.expect("consumed current-segment mark");
        assert_eq!(mark.lane.segment, first.segment);
        assert_eq!(mark.lane.frame, 64);
        assert_eq!(
            spec.frame_at(mark.position).expect("consumed raw frame"),
            expected_frame + 64
        );
        assert_eq!(
            consumed
                .source
                .expect("consumed source metadata")
                .segment_index,
            Some(1)
        );
    }
}
