use std::{path::PathBuf, task::Waker};

use kithara_audio::AudioObserverSlot;
use kithara_command::{Live, Outcome, ScopedReceipt, Seq};
use kithara_events::TrackId;
use kithara_play::{
    CrossfadeCurve, CrossfadeSettings, DeckMixerConfig, DeckPass, HostedDeck, Outbox, Player,
    PlayerConfig, PlayerImpl, ResourceConfig, ResourceLoad, ResourceSrc, Settled, Track,
    TrackCommand, TrackReceipt, TrackSettings, TrackStatus,
};
use kithara_render::{
    Dispatched,
    bridge::{DeckPart, DeckSnapshot, Fade, FadeDir, Slot, SlotMark, SlotState},
};
use kithara_signal::{AudioSpec, SessionFrame};
use kithara_test_utils::{TestTempDir, test::usdt};
use kithara_warp::{Beat, BeatGridQuery, MapPoint, MapPosition};
use num_traits::ToPrimitive;
use ringbuf::{HeapCons, traits::Consumer};

use super::*;
use crate::{HostSettings, MetronomeConfig};

struct ManualDeck {
    tracks: [PlayerImpl<TestPools>; 2],
    loads: [Option<Seq>; 2],
    prep: ResourcePrep<TestPools>,
    mixer: DeckMixerConfig,
}

impl ManualDeck {
    fn new(items: [TrackId; 2]) -> Self {
        Self {
            tracks: [0_u16, 1].map(|slot| {
                PlayerImpl::new(PlayerConfig {
                    item: items[usize::from(slot)],
                    slot: Some(Slot::new(slot)),
                    settings: TrackSettings::default(),
                })
                .expect("valid single-track player")
            }),
            loads: [None; 2],
            prep: ResourcePrep::builder()
                .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
                .block_on_underrun(true)
                .build(),
            mixer: DeckMixerConfig::builder()
                .slots(NonZeroUsize::new(2).expect("two manual voices"))
                .build(),
        }
    }
}

impl HostedDeck<TestPools> for ManualDeck {
    fn worker(&self) -> Option<&PlayWorker<TestPools>> {
        Some(&self.prep.worker)
    }

    fn resource_prep(&self) -> Option<&ResourcePrep<TestPools>> {
        Some(&self.prep)
    }

    fn mixer_config(&self) -> DeckMixerConfig {
        self.mixer
    }

    fn drain(&mut self, _pass: DeckPass<'_>, _out: &mut Outbox<'_, TestPools>) {}

    fn settle(
        &mut self,
        receipt: TrackReceipt<'_, TestPools>,
        _pass: DeckPass<'_>,
        out: &mut Outbox<'_, TestPools>,
    ) {
        match receipt {
            TrackReceipt::Loaded(receipt) => {
                if let Some(index) = self
                    .loads
                    .iter()
                    .position(|seq| *seq == Some(receipt.seq()))
                {
                    self.loads[index] = None;
                    assert!(
                        matches!(
                            receipt.outcome(),
                            Outcome::Applied {
                                data: Dispatched::Loaded(_),
                                ..
                            }
                        ),
                        "the actual source load succeeds"
                    );
                    assert!(
                        !matches!(
                            self.tracks[index].settle(TrackReceipt::Loaded(receipt), out),
                            Settled::Rejected { .. }
                        ),
                        "the owning player accepts its real source receipt"
                    );
                } else {
                    assert!(
                        matches!(
                            receipt.outcome(),
                            Outcome::Applied {
                                data: Dispatched::Prioritized,
                                ..
                            }
                        ),
                        "only the owner's successful priority acknowledgment is payloadless"
                    );
                }
            }
            TrackReceipt::Deck {
                seq,
                outcome,
                batch,
            } => {
                for track in &mut self.tracks {
                    let receipt = TrackReceipt::Deck {
                        seq,
                        outcome,
                        batch: &mut *batch,
                    };
                    if track
                        .snapshot()
                        .slot
                        .is_some_and(|slot| receipt.names(slot))
                    {
                        track.settle(receipt, out);
                    }
                }
            }
            TrackReceipt::Event(event) => {
                for track in &mut self.tracks {
                    let receipt = TrackReceipt::Event(event);
                    if track
                        .snapshot()
                        .slot
                        .is_some_and(|slot| receipt.names(slot))
                    {
                        track.settle(receipt, out);
                    }
                }
            }
        }
    }

    fn tick(&mut self, pass: DeckPass<'_>, out: &mut Outbox<'_, TestPools>) {
        for track in &mut self.tracks {
            track.tick(pass.now, out);
        }
    }

    fn close(&mut self, out: &mut Outbox<'_, TestPools>) -> Result<(), PlayError> {
        for track in &mut self.tracks {
            track.apply(TrackCommand::Release, out)?;
        }
        Ok(())
    }

    fn hold(&mut self, _waker: Waker) {}

    fn release(&mut self) {}
}

#[kithara::test(tokio)]
async fn a_manual_four_second_batch_reaches_the_host_output_on_its_native_frame() {
    kithara_platform::tokio::task::spawn_blocking(manual_four_second_batch)
        .await
        .expect("the blocking manual Host render completes");
}

const RATE: u32 = 44_100;
const BLOCK: u64 = 512;
const PERIOD: u64 = 22_050;
const FADE_FRAMES: u64 = 4 * RATE as u64;
const SOURCE_FRAMES: usize = 12 * RATE as usize;

struct ManualSources {
    spec: AudioSpec,
    outgoing_path: PathBuf,
    incoming_path: PathBuf,
    outgoing: Vec<f32>,
    incoming: Vec<f32>,
}

struct ManualHosts {
    reference: TestState,
    state: TestState<ManualDeck>,
    id: BeatGridId,
    outgoing_id: TrackId,
    incoming_id: TrackId,
}

#[derive(Clone, Copy)]
struct ManualPlan {
    start: u64,
    at: u64,
    end: u64,
    outgoing_slot: usize,
    incoming_slot: usize,
    outgoing_mark: SlotMark,
    expected_basis: [(Slot, Option<Seq>); 2],
}

struct ManualRecording {
    master: Vec<f32>,
    output: Vec<f32>,
    reference_output: Vec<f32>,
    clock: u64,
    applied: usize,
}

fn manual_four_second_batch() {
    let directory = TestTempDir::new();
    let sources = ManualSources::new(&directory);
    let mut hosts = ManualHosts::new(&sources);
    let plan = hosts.prepare();
    let drops = Arc::new(AtomicU64::new(0));
    let mut taps = hosts.capture_outputs(&plan, &drops);
    let settings = CrossfadeSettings::new(4.0, CrossfadeCurve::EqualPower, 0.7, 0.3)
        .expect("four-second manual fade");
    let transition = hosts.schedule(&plan, settings);
    hosts.state.owner.pass();
    let trace = usdt::scope();
    let recording = hosts.render(&plan, settings, transition, &mut taps, &drops);
    recording.assert_pcm(&sources, &plan, settings);
    let snapshot = hosts.assert_finished(&plan, &recording);
    let grid = hosts.assert_grid(&plan, &recording);
    #[cfg(feature = "usdt")]
    assert_incoming_consumption(&trace, hosts.incoming_id, plan.at, sources.spec);
    drop(trace);
    let ManualPlan { start, at, .. } = plan;
    let spec = sources.spec;
    let clock = recording.clock;
    let applied = recording.applied;
    let output = recording.output;
    let outgoing_id = hosts.outgoing_id;
    let incoming_id = hosts.incoming_id;
    if let Some(root) = std::env::var_os("KITHARA_AUDIO_ARTIFACT_DIR") {
        let root = PathBuf::from(root).join("manual-four-second-host");
        std::fs::create_dir_all(&root).expect("manual take artifact directory");
        kithara_play::mock::write_pcm_wav(&root.join("output.wav"), &output, spec)
            .expect("actual Host Output WAV");
        std::fs::write(root.join("timeline.txt"), format!("start={start}\nend={clock}\nnative_applied={at}\nfade_frames={FADE_FRAMES}\nacknowledgments={applied}\nunderruns={}\nsource_a={outgoing_id:?}\nsource_b={incoming_id:?}\nsettings={settings:?}\nhost_grid={grid:?}\n", snapshot.metrics.underruns())).expect("manual take native timeline");
    }
}

impl ManualSources {
    fn new(directory: &TestTempDir) -> Self {
        let spec = AudioSpec::new(2, NonZeroU32::new(RATE).expect("fixture rate"));
        let outgoing_path = directory.path().join("manual-outgoing.wav");
        let incoming_path = directory.path().join("manual-incoming.wav");
        let outgoing: Vec<_> = (0..SOURCE_FRAMES)
            .flat_map(|frame| {
                let phase = frame.to_f64().expect("fixture frame") * 220.0 * std::f64::consts::TAU
                    / f64::from(RATE);
                [
                    (0.2 * phase.sin()).to_f32().expect("fixture amplitude"),
                    0.0,
                ]
            })
            .collect();
        let incoming: Vec<_> = (0..SOURCE_FRAMES)
            .flat_map(|frame| {
                let phase = frame.to_f64().expect("fixture frame") * 330.0 * std::f64::consts::TAU
                    / f64::from(RATE);
                [
                    0.0,
                    (0.125 * phase.sin()).to_f32().expect("fixture amplitude"),
                ]
            })
            .collect();
        kithara_play::mock::write_pcm_wav(&outgoing_path, &outgoing, spec).expect("outgoing WAV");
        kithara_play::mock::write_pcm_wav(&incoming_path, &incoming, spec).expect("incoming WAV");
        Self {
            spec,
            outgoing_path,
            incoming_path,
            outgoing,
            incoming,
        }
    }
}

impl ManualHosts {
    fn new(sources: &ManualSources) -> Self {
        let spec = sources.spec;
        let host_settings = HostSettings::builder()
            .sample_rate(spec.sample_rate)
            .metronome(MetronomeConfig::builder().duck(0.0).level(0.05).build())
            .build();
        let mut state = TestState::<ManualDeck>::new(start_route_loss_stream);
        state.settings = Live::new(host_settings).expect("valid actual Host settings");
        state.publish_root();
        let mut reference = test_state(start_route_loss_stream);
        reference.settings = Live::new(host_settings).expect("same valid reference Host settings");
        reference.publish_root();
        let id = BeatGridId::allocate().expect("fixture deck");
        let outgoing_id = TrackId::allocate();
        let incoming_id = TrackId::allocate();
        ask(
            &mut state,
            HostCommand::Register {
                id,
                deck: Box::new(ManualDeck::new([outgoing_id, incoming_id])),
            },
        )
        .expect("register the real manual two-player holder");
        let reference_id = BeatGridId::allocate().expect("reference deck");
        ask(&mut reference, mix_registration(reference_id, 1.0))
            .expect("empty real Queue keeps the reference Host graph active");
        state
            .owner
            .with_deck(id, &mut |deck, out, pass| {
                for (index, path) in [&sources.outgoing_path, &sources.incoming_path]
                    .into_iter()
                    .enumerate()
                {
                    let config = ResourceConfig::for_src(ResourceSrc::Path(path.to_owned()))
                        .store(
                            kithara_assets::AssetStore::builder(pools())
                                .backend(kithara_assets::StorageBackend::Memory)
                                .build(),
                        )
                        .build();
                    let prepared = deck
                        .prep
                        .prepare(config, pass.output)
                        .expect("prepared real source");
                    let seq = deck.tracks[index]
                        .apply(
                            TrackCommand::Load {
                                item: ResourceLoad::new(
                                    prepared,
                                    Box::new(AudioObserverSlot::default().relay()),
                                ),
                                position: Duration::ZERO,
                            },
                            out,
                        )
                        .expect("send one real source load")
                        .expect("the dispatcher returns the actual load sequence");
                    assert!(
                        deck.loads[index].replace(seq).is_none(),
                        "each source opens once"
                    );
                }
                deck.tracks[0]
                    .apply(TrackCommand::Play { at: When::Next }, out)
                    .expect("A starts through its real player");
            })
            .expect("load through the manual holder's borrowed outbox");
        Self {
            reference,
            state,
            id,
            outgoing_id,
            incoming_id,
        }
    }

    fn prepare(&mut self) -> ManualPlan {
        let id = self.id;
        let outgoing_id = self.outgoing_id;
        let incoming_id = self.incoming_id;
        let mut clock = 0;
        let mut reference_clock = 0;
        let mut outgoing_basis = None;
        let mut incoming_basis = None;
        let deadline = kithara_platform::time::WallInstant::now() + Duration::from_secs(5);
        loop {
            self.state.owner.begin_pass();
            for message in self.state.messages.try_iter() {
                message.run(&mut self.state.owner);
            }
            self.state
                .owner
                .each_deck(&mut |_, deck, out, pass| HostedDeck::tick(deck, pass, out));
            self.state.owner.pass();
            render_block(&mut self.state, clock);
            clock += BLOCK;
            fixture_pass(&mut self.reference, &mut reference_clock);
            self.settle_setup(&mut outgoing_basis, &mut incoming_basis);
            self.state.owner.begin_pass();
            self.state.owner.pass();
            let mut ready = false;
            self.state
                .owner
                .with_deck(id, &mut |deck, _, _| {
                    ready = deck.tracks.iter().any(|track| {
                        let snapshot = track.snapshot();
                        snapshot.item == outgoing_id
                            && matches!(snapshot.status, TrackStatus::Playing { .. })
                    }) && deck.tracks.iter().any(|track| {
                        let snapshot = track.snapshot();
                        snapshot.item == incoming_id && snapshot.status == TrackStatus::Loaded
                    });
                })
                .expect("observe both real loaded tracks");
            if ready {
                break;
            }
            assert!(
                kithara_platform::time::WallInstant::now() < deadline,
                "both sources load before the manual take"
            );
            kithara_platform::thread::yield_now();
        }
        assert!(
            incoming_basis.is_some(),
            "B has an actual applied attachment predecessor batch"
        );
        fixture_pass(&mut self.state, &mut clock);
        fixture_pass(&mut self.reference, &mut reference_clock);
        fixture_pass(&mut self.state, &mut clock);
        fixture_pass(&mut self.reference, &mut reference_clock);
        assert_eq!(
            reference_clock, clock,
            "reference uses every actual setup render range"
        );
        let snapshot = deck_snapshot(&mut self.state, id);
        let outgoing_slot = snapshot
            .slots
            .iter()
            .position(|slot| slot.state == SlotState::Playing)
            .expect("A playing before F");
        let incoming_slot = 1;
        assert_ne!(outgoing_slot, incoming_slot);
        let outgoing_basis =
            outgoing_basis.expect("A's actual setup receipt fixes its predecessor");
        assert_eq!(
            outgoing_basis.0,
            Slot::new(outgoing_slot.to_u16().expect("fixture slot fits u16"))
        );
        let expected_basis = [(Slot::new(1), incoming_basis), outgoing_basis];
        assert_eq!(snapshot.slots[incoming_slot].state, SlotState::Stopped);
        let outgoing_mark = snapshot.slots[outgoing_slot]
            .mark
            .expect("A source position");
        assert_eq!(outgoing_mark.session, SessionFrame::new(clock as i64));
        let start = clock;
        let at = (clock / PERIOD + 2) * PERIOD;
        let end = at + FADE_FRAMES + PERIOD;
        ManualPlan {
            start,
            at,
            end,
            outgoing_slot,
            incoming_slot,
            outgoing_mark,
            expected_basis,
        }
    }

    fn settle_setup(
        &mut self,
        outgoing_basis: &mut Option<(Slot, Option<Seq>)>,
        incoming_basis: &mut Option<Seq>,
    ) {
        let id = self.id;
        let outgoing_id = self.outgoing_id;
        let incoming_id = self.incoming_id;
        while let Some(receipt) = self
            .state
            .channel
            .as_mut()
            .expect("setup session channel")
            .receipt()
        {
            match receipt {
                ScopedReceipt::Scope(_, receipt) => {
                    let seq = receipt.seq();
                    let (outcome, mut batch) = receipt.into();
                    assert!(
                        matches!(outcome, Outcome::Applied { .. }),
                        "setup batch applies before the take"
                    );
                    self.state
                        .owner
                        .with_deck(id, &mut |deck, out, pass| {
                            for track in &deck.tracks {
                                let snapshot = track.snapshot();
                                if let Some(slot) = snapshot.slot.filter(|slot| {
                                    batch.basis.iter().any(|(named, _)| named == slot)
                                }) {
                                    if snapshot.item == outgoing_id {
                                        *outgoing_basis = Some((slot, Some(seq)));
                                    } else if snapshot.item == incoming_id {
                                        *incoming_basis = Some(seq);
                                    }
                                }
                            }
                            HostedDeck::settle(
                                deck,
                                TrackReceipt::Deck {
                                    seq,
                                    outcome: &outcome,
                                    batch: &mut batch,
                                },
                                pass,
                                out,
                            );
                        })
                        .expect("the owner settles each real setup receipt");
                }
                ScopedReceipt::Root(receipt) => {
                    crate::session::queue::settle_receipt(&mut self.state, &receipt);
                }
                ScopedReceipt::Closed(_) => panic!("setup keeps its deck registered"),
            }
        }
    }

    fn capture_outputs(&mut self, plan: &ManualPlan, drops: &Arc<AtomicU64>) -> Vec<HeapCons<f32>> {
        let ManualPlan { start, end, .. } = *plan;
        let capacity = usize::try_from((end - start) * 2 + BLOCK * 2).expect("finite tap capacity");
        let mut taps = Vec::new();
        for tap in [Tap::Master, Tap::Output] {
            let (producer, consumer) = HeapRb::<f32>::new(capacity).split();
            let mut outputs = OutputGroup::new();
            outputs.push(MixTapWriter::new(producer, Arc::clone(drops)));
            ask(&mut self.state, HostCommand::AttachOutputs { tap, outputs })
                .expect("attach real Host tap");
            taps.push(consumer);
        }
        ask(
            &mut self.state,
            HostCommand::Configure(
                HostSettingsChange::Metronome(MetronomeConfigChange::Enabled(true)),
                When::Next,
            ),
        )
        .expect("enable the actual Host metronome");
        configure_next(
            &mut self.reference,
            HostSettingsChange::Metronome(MetronomeConfigChange::Enabled(true)),
        )
        .expect("enable the real reference metronome at the same session frame");
        taps
    }

    fn schedule(&mut self, plan: &ManualPlan, settings: CrossfadeSettings) -> Seq {
        let id = self.id;
        let outgoing_id = self.outgoing_id;
        let incoming_id = self.incoming_id;
        let at = plan.at;
        let mut transition = None;
        self.state
            .owner
            .with_deck(id, &mut |deck, out, pass| {
                assert!(SessionFrame::new(at as i64) >= pass.earliest());
                let (_, sent) = out
                    .together(When::At(SessionFrame::new(at as i64)), |out| {
                        for (item, dir) in [(incoming_id, FadeDir::In), (outgoing_id, FadeDir::Out)]
                        {
                            let track = deck
                                .tracks
                                .iter_mut()
                                .find(|track| track.snapshot().item == item)
                                .expect("held manual voice");
                            assert_eq!(
                                track.apply(
                                    TrackCommand::Fade {
                                        at: When::At(SessionFrame::new(at as i64)),
                                        settings,
                                        dir
                                    },
                                    out
                                )?,
                                None
                            );
                        }
                        Ok(())
                    })
                    .expect("one manual Start B and Fade A batch");
                let seq = sent.expect("the two primitives form one batch");
                for track in &mut deck.tracks {
                    if [incoming_id, outgoing_id].contains(&track.snapshot().item) {
                        track.finish_group(Ok(seq));
                    }
                }
                transition = sent;
            })
            .expect("manual batch goes through the canonical owner");
        transition.expect("the two primitives form one batch")
    }

    fn render(
        &mut self,
        plan: &ManualPlan,
        settings: CrossfadeSettings,
        transition: Seq,
        taps: &mut [HeapCons<f32>],
        drops: &AtomicU64,
    ) -> ManualRecording {
        let id = self.id;
        let ManualPlan {
            start,
            at,
            end,
            outgoing_slot,
            expected_basis,
            ..
        } = *plan;
        let mut clock = start;
        let mut reference_clock = start;
        let mut rendered = Vec::new();
        let mut reference_output = Vec::new();
        let mut applied = 0;
        while clock < end {
            self.state.owner.begin_pass();
            self.state
                .owner
                .each_deck(&mut |_, deck, out, pass| HostedDeck::tick(deck, pass, out));
            self.state.owner.pass();
            rendered.extend(render_block(&mut self.state, clock));
            clock += BLOCK;
            reference_output.extend(fixture_pass(&mut self.reference, &mut reference_clock));
            assert_eq!(
                reference_clock, clock,
                "reference and actual Host render the same block range"
            );
            while let Some(receipt) = self
                .state
                .channel
                .as_mut()
                .expect("real session channel")
                .receipt()
            {
                match receipt {
                    ScopedReceipt::Scope(_, receipt) => {
                        assert_eq!(
                            receipt.seq(),
                            transition,
                            "no unrelated deck batch during the take"
                        );
                        assert!(
                            matches!(receipt.outcome(), Outcome::Applied { at: actual, .. } if *actual == SessionFrame::new(at as i64))
                        );
                        let (outcome, mut batch) = receipt.into();
                        assert_eq!(
                            batch.basis, expected_basis,
                            "both real predecessor sequences are retained"
                        );
                        assert!(matches!(batch.commands.as_slice(), [
                            DeckPart::Start { slot, fade: Fade::Crossfade(actual_in) },
                            DeckPart::Fade { slot: outgoing, settings: actual_out, dir: FadeDir::Out },
                        ] if *slot == Slot::new(1) && usize::from(outgoing.get()) == outgoing_slot
                            && *actual_in == settings && *actual_out == settings));
                        self.state
                            .owner
                            .with_deck(id, &mut |deck, out, pass| {
                                HostedDeck::settle(
                                    deck,
                                    TrackReceipt::Deck {
                                        seq: transition,
                                        outcome: &outcome,
                                        batch: &mut batch,
                                    },
                                    pass,
                                    out,
                                );
                            })
                            .expect("settle the actual native receipt at its owner");
                        applied += 1;
                    }
                    ScopedReceipt::Root(receipt) => {
                        crate::session::queue::settle_receipt(&mut self.state, &receipt);
                    }
                    ScopedReceipt::Closed(_) => {
                        panic!("the sounding deck cannot close during its fade")
                    }
                }
            }
            self.state.owner.begin_pass();
            self.state.owner.pass();
        }
        assert_eq!(
            applied, 1,
            "one native acknowledgment across the whole envelope and tail"
        );
        let master: Vec<_> = taps[0].pop_iter().collect();
        let output: Vec<_> = taps[1].pop_iter().collect();
        assert_eq!(output, rendered, "the WAV input is the actual Host Output");
        assert_eq!(master.len(), output.len());
        assert_eq!(reference_output.len(), output.len());
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        ManualRecording {
            master,
            output,
            reference_output,
            clock,
            applied,
        }
    }

    fn assert_finished(&mut self, plan: &ManualPlan, recording: &ManualRecording) -> DeckSnapshot {
        let id = self.id;
        let outgoing_id = self.outgoing_id;
        let incoming_id = self.incoming_id;
        let ManualPlan {
            start,
            at,
            outgoing_slot,
            incoming_slot,
            outgoing_mark,
            ..
        } = *plan;
        let clock = recording.clock;
        let snapshot = deck_snapshot(&mut self.state, id);
        assert_eq!(snapshot.metrics.underruns(), 0);
        assert_eq!(snapshot.metrics.decode_errors(), 0);
        assert_eq!(snapshot.metrics.event_overflows(), 0);
        assert_eq!(snapshot.slots[outgoing_slot].state, SlotState::Stopped);
        assert_eq!(snapshot.slots[incoming_slot].state, SlotState::Playing);
        assert_eq!(
            snapshot.slots[outgoing_slot]
                .mark
                .expect("A final mark")
                .lane
                .frame,
            outgoing_mark.lane.frame + at + FADE_FRAMES - start
        );
        assert_eq!(
            snapshot.slots[incoming_slot]
                .mark
                .expect("B final mark")
                .lane
                .frame,
            clock - at
        );
        self.state
            .owner
            .with_deck(id, &mut |deck, _, _| {
                let a = deck
                    .tracks
                    .iter()
                    .find(|track| track.snapshot().item == outgoing_id)
                    .expect("outgoing remains held")
                    .snapshot();
                let b = deck
                    .tracks
                    .iter()
                    .find(|track| track.snapshot().item == incoming_id)
                    .expect("incoming remains held")
                    .snapshot();
                assert_eq!(
                    a.status,
                    TrackStatus::Faded {
                        at: SessionFrame::new((at + FADE_FRAMES) as i64)
                    }
                );
                assert_eq!(
                    b.status,
                    TrackStatus::Playing {
                        since: SessionFrame::new(at as i64)
                    }
                );
            })
            .expect("player states settle only from their actual native receipts/events");
        snapshot
    }

    fn assert_grid(&self, plan: &ManualPlan, recording: &ManualRecording) -> BeatGridSnapshot {
        let start = plan.start;
        let clock = recording.clock;
        let master = &recording.master;
        let output = &recording.output;
        let grid = self.state.root.grid().clone();
        assert_eq!(grid.state(), BeatGridState::Live);
        for ordinal in start.div_ceil(PERIOD)..clock / PERIOD {
            let expected = ordinal * PERIOD;
            let query = grid.position_at(MapPoint::new(
                grid.stamp(),
                Beat::new(ordinal.to_f64().expect("beat ordinal")).expect("beat"),
            ));
            let BeatGridQuery::Resolved(estimate) = query else {
                panic!("the actual Host grid resolves beat {ordinal}")
            };
            assert_eq!(
                *estimate.value().value(),
                MapPosition::Session(SessionFrame::new(expected as i64))
            );
            let offset = usize::try_from((expected - start) * 2).expect("beat in take");
            assert!(
                output[offset + 2..offset + 512]
                    .iter()
                    .zip(&master[offset + 2..offset + 512])
                    .any(|(output, master)| (output - master).abs() > 0.01),
                "the real metronome sounds at mapped beat {ordinal}"
            );
        }
        grid
    }
}

impl ManualRecording {
    fn assert_pcm(&self, sources: &ManualSources, plan: &ManualPlan, settings: CrossfadeSettings) {
        let ManualPlan {
            start,
            at,
            outgoing_mark,
            ..
        } = *plan;
        let outgoing = &sources.outgoing;
        let incoming = &sources.incoming;
        let master = &self.master;
        let output = &self.output;
        let reference_output = &self.reference_output;
        for (offset, stereo) in master.as_chunks::<2>().0.iter().enumerate() {
            let frame = start + offset as u64;
            let (out_gain, in_gain) = if frame < at {
                (1.0, 0.0)
            } else {
                settings.gains(
                    (frame - at).to_f32().expect("elapsed frame")
                        / (FADE_FRAMES - 1).to_f32().expect("fade duration"),
                )
            };
            let source_a =
                usize::try_from(outgoing_mark.lane.frame + offset as u64).expect("A frame");
            let expected_left = outgoing[source_a * 2] * out_gain;
            let expected_right = if frame < at {
                0.0
            } else {
                incoming[usize::try_from(frame - at).expect("B frame") * 2 + 1] * in_gain
            };
            assert!(
                (stereo[0] - expected_left).abs() < 1e-6,
                "A gain/source custody at Host frame {frame}"
            );
            assert!(
                (stereo[1] - expected_right).abs() < 1e-6,
                "B gain/source custody at Host frame {frame}"
            );
            assert!(
                (output[offset * 2] - (expected_left + reference_output[offset * 2])).abs() < 1e-6,
                "A music and real reference click reach Output at Host frame {frame}"
            );
            assert!(
                (output[offset * 2 + 1] - (expected_right + reference_output[offset * 2 + 1]))
                    .abs()
                    < 1e-6,
                "B music and real reference click reach Output at Host frame {frame}"
            );
        }
    }
}

#[cfg(feature = "usdt")]
fn assert_incoming_consumption(
    trace: &usdt::Scope,
    incoming_id: TrackId,
    at: u64,
    spec: AudioSpec,
) {
    let events = trace.events();
    let entry = events
        .iter()
        .position(|event| {
            event.probe == "render"
                && event.field("track_id") == Some(incoming_id.as_u64())
                && event
                    .field("output_base")
                    .zip(event.field("range_start"))
                    .and_then(|(base, start)| base.checked_add(start))
                    == Some(at)
        })
        .expect("B's native render interval starts exactly at F");
    let thread = events[entry].thread;
    let next_render = events[entry + 1..]
        .iter()
        .position(|event| {
            event.thread == thread && event.probe == "render" && event.file == events[entry].file
        })
        .map_or(events.len(), |offset| entry + 1 + offset);
    let interval = &events[entry + 1..next_render];
    let consumed_index = interval
        .iter()
        .position(|event| {
            event.thread == thread
                && event.probe == "pcm_consumed"
                && event.field("output_start") == Some(at)
        })
        .expect("B actually consumes PCM inside its own render interval at F");
    let consumed = &interval[consumed_index];
    let output_end = consumed.field("output_end").expect("consumed output end");
    let count = output_end
        .checked_sub(at)
        .filter(|count| *count > 0)
        .expect("nonempty B consumption");
    let render_end = events[entry].field("output_base").expect("B output base")
        + events[entry].field("range_end").expect("B render end");
    assert!(
        output_end <= render_end,
        "B's consumption stays inside its actual render range"
    );
    assert_eq!(consumed.field("source_start"), Some(0));
    assert_eq!(consumed.field("source_end"), Some(count));
    let geometry_end = interval[consumed_index + 1..]
        .iter()
        .position(|event| event.thread == thread && event.probe == "pcm_consumed")
        .map_or(interval.len(), |offset| consumed_index + 1 + offset);
    let consumption = &interval[consumed_index + 1..geometry_end];
    let identity: Vec<_> = consumption
        .iter()
        .filter(|event| event.thread == thread && event.probe == "pcm_consumed_identity")
        .collect();
    assert_eq!(
        identity.len(),
        1,
        "the same consumption has one source identity"
    );
    assert_eq!(identity[0].field("sample_rate"), Some(u64::from(RATE)));
    assert_eq!(identity[0].field("channels"), Some(2));
    let parts: Vec<_> = consumption
        .iter()
        .filter(|event| event.thread == thread && event.probe == "pcm_consumed_geometry")
        .collect();
    assert_eq!(
        parts.len(),
        6,
        "B's real consumption retains every geometry fragment"
    );
    let mut words = [0_u64; 24];
    for (part, event) in parts.iter().enumerate() {
        assert_eq!(event.field("part"), Some(part as u64));
        for (offset, field) in ["first", "second", "third", "fourth"]
            .into_iter()
            .enumerate()
        {
            words[part * 4 + offset] = event.field(field).expect("complete geometry word");
        }
    }
    let span = kithara_signal::SourceSpan::try_from(words).expect("valid consumed B geometry");
    assert_eq!(span.sample_rate(), spec.sample_rate);
    assert_eq!(span.output_frames(), count);
    assert_eq!(span.start(), 0);
    assert_eq!(span.end(), count);
    assert!(span.is_affine());
    for offset in 0..=count {
        let (numerator, denominator) = span
            .source_ratio_at(offset)
            .expect("B source position in consumed range");
        assert_eq!(
            numerator,
            u128::from(offset) * denominator.get(),
            "B's first actual consumption has exact unit source geometry"
        );
    }
}
