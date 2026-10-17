use kithara_command::{Batch, Port, Receipt};
use kithara_play::{DeckEvent, DeckMixSettingsChange, Position, TrackCommand};
use kithara_render::bridge::{DeckProtocol, PlaybackFault, Slot, SlotState};

use super::*;
use crate::{AdvanceReason, QueueSettingsChange, Transition};

struct HandoverRig {
    queue: Queue<TestPools>,
    deck: mock::DeckRig<TestPools>,
    loader: mock::DeckRig<TestPools>,
    dir: TestTempDir,
    ids: [TrackId; 3],
    last_lane: Option<kithara_render::LaneId>,
}

impl HandoverRig {
    fn on_pass<Value>(
        &mut self,
        now: SessionFrame,
        run: impl FnOnce(&mut Queue<TestPools>, &mut Outbox<'_, TestPools>) -> Value,
    ) -> Value {
        let output = mock::output(None).get();
        let deck = self.queue.deck.mixer.clone();
        let pass = DeckPass {
            mix: self.queue.deck.mix,
            suspended: false,
            now,
            delivery: FrameCount::new(128),
            output: &output,
            deck: &deck,
        };
        let mut scope = self.deck.ring.scope(self.deck.scope).expect("deck scope");
        let mut out = Outbox::new(&mut scope, &mut self.deck.dispatcher).in_pass(pass);
        self.queue.clock = out.pass().map(|pass| (pass.now, pass.delivery));
        run(&mut self.queue, &mut out)
    }

    fn unrelated_mix(&mut self) -> Seq {
        self.deck
            .ring
            .scope(self.deck.scope)
            .expect("deck scope")
            .send(
                When::Next,
                Batch {
                    basis: Vec::new(),
                    commands: vec![DeckPart::Mix(DeckMixSettingsChange::Level(0.5))],
                },
            )
            .expect("unrelated native sequence")
    }

    fn settle_batch(
        &mut self,
        seq: Seq,
        outcome: &Outcome<DeckProtocol>,
        batch: &mut Batch<DeckProtocol>,
    ) -> kithara_play::Settled {
        with_outbox(&mut self.queue, &mut self.deck, |queue, out| {
            Player::settle(
                queue,
                TrackReceipt::Deck {
                    seq,
                    outcome,
                    batch,
                },
                out,
            )
        })
    }

    #[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
    async fn playing() -> Self {
        hang_tick!();
        let rig = Self::playing_with_sources(["item-1.wav", "item-2.wav", "item-3.wav"]).await;
        hang_reset!();
        rig
    }

    #[kithara::flash(io)]
    #[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
    async fn playing_with_sources(names: [&str; 3]) -> Self {
        Self::playing_with_config(names, DeckMixerConfig::default()).await
    }

    #[kithara::flash(io)]
    #[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
    async fn playing_with_config(names: [&str; 3], config: DeckMixerConfig) -> Self {
        hang_tick!();
        let (mut queue, _, dir) = empty_queue();
        queue.config.mixer = config;
        queue.active = crate::queue::slots::Slots::new(config.slots().get());
        queue.deck = crate::queue::view::DeckObservation::new(config);
        queue.deck.mixer.sample_rate = mock::SAMPLE_RATE.get();
        let deck = mock::DeckRig::new(config).expect("fixture deck");
        let mut rig = Self {
            queue,
            deck,
            loader: mock::DeckRig::new(DeckMixerConfig::default()).expect("fixture loader"),
            dir,
            ids: [
                TrackId::allocate(),
                TrackId::allocate(),
                TrackId::allocate(),
            ],
            last_lane: None,
        };
        for name in names {
            hang_tick!();
            let path = rig.dir.path().join(name);
            mock::write_pcm_wav(&path, &[0.5; 2_048], AudioSpec::new(2, mock::SAMPLE_RATE))
                .expect("offline item fixture");
            hang_reset!();
        }
        for (id, name) in rig.ids.into_iter().zip(names).take(1) {
            hang_tick!();
            let path = rig.dir.path().join(name);
            rig.apply(QueueCommand::Append {
                position: Position::ZERO,
                id,
                source: TrackSource::Uri(path.to_str().expect("fixture path").to_owned()),
            })
            .expect("append item");
            hang_reset!();
        }
        rig.select(0, Transition::None).expect("select first");
        hang_tick!();
        rig.load(0, 64.295).await;
        hang_reset!();
        crate::mock::finish(&mut rig.queue, &mut rig.deck);
        assert_eq!(rig.queue.current, Some(rig.ids[0]));
        let slot = rig.slot(0);
        rig.queue.deck.mixer.slots[usize::from(slot.get())].mark = Some(SlotMark {
            session: SessionFrame::new(128),
            lane: LaneFrame::default(),
            position: Duration::ZERO,
        });
        with_outbox(&mut rig.queue, &mut rig.deck, |queue, out| {
            let current = queue.active_current_index().expect("current fixture");
            queue
                .active
                .get_mut(current)
                .expect("current fixture")
                .track
                .tick(SessionFrame::new(128), out);
        });
        rig.queue.insert_entry(
            rig.ids[1],
            TrackSource::Uri(
                rig.dir
                    .path()
                    .join(names[1])
                    .to_str()
                    .expect("fixture path")
                    .to_owned(),
            ),
            Position::ZERO,
            crate::queue::types::Placement::Append,
        );
        rig
    }

    fn apply(&mut self, command: QueueCommand<TestPools>) -> Result<Option<Seq>, PlayError> {
        with_outbox(&mut self.queue, &mut self.deck, |queue, out| {
            Player::apply(queue, command, out)
        })
    }

    fn select(&mut self, index: usize, transition: Transition) -> Result<Option<Seq>, PlayError> {
        if self.queue.track(self.ids[index]).is_none() {
            self.queue.insert_entry(
                self.ids[index],
                TrackSource::Uri(
                    self.dir
                        .path()
                        .join("item-3.wav")
                        .to_str()
                        .expect("fixture path")
                        .to_owned(),
                ),
                Position::ZERO,
                crate::queue::types::Placement::Append,
            );
        }
        self.apply(QueueCommand::Select {
            id: self.ids[index],
            transition,
        })
    }

    #[kithara::flash(io)]
    #[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
    async fn load(&mut self, index: usize, duration: f64) {
        use kithara_render::{Dispatched, DispatcherCommand};

        self.deck.opens.drain();
        let source = loop {
            hang_tick!();
            let mut due = self
                .deck
                .opens
                .next_due((), 1)
                .expect("queued dispatcher request");
            let command = due.commands_mut().pop().expect("one dispatcher command");
            assert!(due.commands_mut().is_empty());
            let source = match command {
                DispatcherCommand::Load(request) => {
                    let wanted = self
                        .queue
                        .active
                        .iter()
                        .filter(|active| active.item == self.ids[index])
                        .find_map(|active| active.load)
                        .expect("fixture load")
                        .seq();
                    if due.seq() != wanted {
                        due.commands_mut().push(DispatcherCommand::Load(request));
                        due.defer();
                        continue;
                    }
                    let loaded = self
                        .loader
                        .load_request_fixture(
                            self.queue.worker().expect("fixture worker"),
                            *request,
                        )
                        .await
                        .expect("actual queued source");
                    let source = loaded
                        .opened
                        .pcm
                        .next_chunk_info()
                        .expect("prepared source geometry");
                    self.last_lane = Some(loaded.lane);
                    due.apply(Dispatched::Loaded(loaded));
                    Some(source)
                }
                DispatcherCommand::Release(_) => {
                    due.apply(Dispatched::Released);
                    None
                }
                DispatcherCommand::SetPriority(_, _) => {
                    due.apply(Dispatched::Prioritized);
                    None
                }
            };
            let receipt = self
                .deck
                .dispatcher
                .receipts()
                .next()
                .expect("dispatcher answer");
            with_outbox(&mut self.queue, &mut self.deck, |queue, out| {
                Player::settle(queue, TrackReceipt::Loaded(receipt), out)
            });
            hang_reset!();
            if let Some(source) = source {
                break source;
            }
        };
        crate::mock::settle(&mut self.queue, &mut self.deck, SessionFrame::new(0));
        let slot = self.slot(index);
        let snapshot = &mut self.queue.deck.mixer.slots[usize::from(slot.get())];
        snapshot.source = Some(AudioChunkInfo {
            media_end: Some(Duration::from_secs_f64(duration)),
            ..source
        });
        snapshot.mark = Some(SlotMark {
            session: SessionFrame::new(0),
            lane: LaneFrame::default(),
            position: Duration::ZERO,
        });
    }

    #[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
    async fn arm(&mut self, index: usize) -> Slot {
        hang_tick!();
        let id = self.ids[index];
        with_outbox(&mut self.queue, &mut self.deck, |queue, out| {
            queue.load_track(id, Role::Preloaded, Some(&mock::output(None).get()), out)
        })
        .expect("preload item");
        hang_reset!();
        hang_tick!();
        self.load(index, 162.0).await;
        hang_reset!();
        self.slot(index)
    }

    fn slot(&self, index: usize) -> Slot {
        self.queue
            .active
            .iter()
            .find(|active| active.item == self.ids[index])
            .expect("active item")
            .slot
    }

    fn source(&self, index: usize) -> &str {
        self.deck
            .mixer
            .held(self.slot(index))
            .expect("opened resource source URI")
    }

    fn armed(&self) -> Option<TrackId> {
        self.queue.target.map(|target| target.to).or_else(|| {
            self.queue
                .active
                .iter()
                .find(|active| active.role == Role::Preloaded)
                .map(|active| active.item)
        })
    }

    fn entry(&mut self, index: usize, transition: Transition) -> Vec<Receipt<DeckProtocol>> {
        self.select(index, transition)
            .expect("select prepared item");
        self.deck
            .block(SessionFrame::new(128), 0.0)
            .expect("entry block")
    }

    fn settle(&mut self, receipts: Vec<Receipt<DeckProtocol>>) {
        for receipt in receipts {
            let seq = receipt.seq();
            let (outcome, mut batch) = receipt.into();
            with_outbox(&mut self.queue, &mut self.deck, |queue, out| {
                Player::settle(
                    queue,
                    TrackReceipt::Deck {
                        seq,
                        outcome: &outcome,
                        batch: &mut batch,
                    },
                    out,
                )
            });
        }
    }

    fn event(&mut self, event: DeckEvent) {
        with_outbox(&mut self.queue, &mut self.deck, |queue, out| {
            Player::settle(queue, TrackReceipt::Event(event), out)
        });
    }

    fn outgoing_observation(&mut self, index: usize) {
        let slot = self.slot(index);
        let snapshot = &mut self.queue.deck.mixer.slots[usize::from(slot.get())];
        snapshot.position = 62.3;
        let source = snapshot.source.as_mut().expect("declared fixture source");
        let start = 2_747_430;
        let frames = u64::from(source.frames);
        let span = kithara_signal::SourceSpan::new(
            start,
            start.checked_add(frames).expect("fixture source endpoint"),
            source.spec.sample_rate,
            frames,
        )
        .expect("outgoing native source geometry")
        .with_render_revision(source.render_revision)
        .with_mapping_revision(source.mapping_revision);
        source.source_span = Some(span);
        source.timestamp = span.position_at(0).expect("fixture source start");
        source.end_timestamp = span.position_at(frames).expect("fixture source end");
        source.frame_offset = start;
        source.lane_frame = start;
        source.media_end = Some(Duration::from_secs_f64(64.295));
        assert_eq!(source.timestamp, Duration::from_secs_f64(62.3));
        snapshot.mark = Some(SlotMark {
            session: SessionFrame::new(256),
            lane: LaneFrame {
                segment: kithara_signal::SegmentId::FIRST,
                frame: 2_747_430,
            },
            position: Duration::from_secs_f64(62.3),
        });
        with_outbox(&mut self.queue, &mut self.deck, |queue, out| {
            for track in queue.tracks_mut() {
                track.tick(SessionFrame::new(256), out);
            }
        });
        self.queue.publish();
    }

    fn chain(&mut self) {
        let current = self.queue.active_current_index().expect("fixture current");
        let active = self
            .queue
            .active
            .get(current)
            .expect("fixture current resource");
        let snapshot = active.track.snapshot();
        let observation = &self.queue.deck.mixer.slots[usize::from(active.slot.get())];
        assert!(
            self.queue.current_end().is_some(),
            "Chain premise: planned_end={:?}, status={:?}, duration={:?}, visible_origin={:?}, mark={:?}, target={:?}, pending={:?}",
            self.queue.current_end(),
            snapshot.as_ref().status,
            snapshot.as_ref().duration,
            observation.source.and_then(|info| info.visible_origin),
            observation.mark,
            self.queue.target,
            self.queue.pending_handover,
        );
        self.apply(QueueCommand::ConfigureQueue(
            QueueSettingsChange::Gapless(true),
            When::Next,
        ))
        .expect("gapless policy");
        with_outbox(&mut self.queue, &mut self.deck, |queue, out| {
            queue.next_target(
                Transition::None,
                AdvanceReason::NaturalEof,
                true,
                true,
                Some(&mock::output(None).get()),
                out,
            )
        })
        .expect("arm gapless entry");
        crate::mock::settle(&mut self.queue, &mut self.deck, SessionFrame::new(0));
    }

    fn stitch(&mut self) -> Vec<Receipt<DeckProtocol>> {
        self.deck
            .end(self.slot(0), SessionFrame::new(128))
            .expect("stitch block")
    }

    #[kithara::hang_watchdog]
    fn fill_to_room(&mut self, room: usize) {
        let mut port = self.deck.ring.scope(self.deck.scope).expect("deck scope");
        while port.available() > room {
            hang_tick!();
            port.send(
                When::Next,
                Batch {
                    basis: Vec::new(),
                    commands: vec![DeckPart::Mix(DeckMixSettingsChange::Level(1.0))],
                },
            )
            .expect("fill available deck capacity");
            hang_reset!();
        }
        assert_eq!(port.available(), room);
    }

    #[kithara::hang_watchdog]
    fn assert_no_pending_load(&mut self) {
        use kithara_render::{Dispatched, DispatcherCommand};

        self.deck.opens.drain();
        loop {
            let Some(mut due) = self.deck.opens.next_due((), 1) else {
                break;
            };
            hang_tick!();
            let seq = due.seq();
            let command = due.commands_mut().pop().expect("dispatcher command");
            assert!(due.commands_mut().is_empty());
            match command {
                DispatcherCommand::Load(_) => panic!("promotion reuses the opened lane"),
                DispatcherCommand::SetPriority(_, _) => due.apply(Dispatched::Prioritized),
                DispatcherCommand::Release(_) => due.apply(Dispatched::Released),
            }
            let receipt = self
                .deck
                .dispatcher
                .receipts()
                .next()
                .expect("dispatcher maintenance receipt");
            assert_eq!(receipt.seq(), seq);
            with_outbox(&mut self.queue, &mut self.deck, |queue, out| {
                Player::settle(queue, TrackReceipt::Loaded(receipt), out)
            });
            hang_reset!();
        }
    }

    #[kithara::hang_watchdog]
    fn pending_parts(&mut self) -> Vec<DeckPart> {
        self.deck.ring.publish().expect("publish pending commands");
        self.deck.inbox.drain();
        let mut port = self.deck.inbox.scope(self.deck.scope).expect("deck scope");
        let mut parts = Vec::new();
        while let Some(mut due) = port.next_due(SessionFrame::new(256), 1) {
            hang_tick!();
            parts.append(due.commands_mut());
            due.apply(());
            hang_reset!();
        }
        while self.deck.ring.receipt().is_some() {
            hang_tick!();
            hang_reset!();
        }
        parts
    }

    fn only_current(&mut self, index: usize) {
        let id = self.ids[index];
        self.queue
            .tracks
            .records_mut()
            .retain(|record| record.id == id);
        self.queue.current = Some(id);
        self.queue.config.action_at_item_end = ActionAtItemEnd::Advance;
        self.queue.publish();
    }
}

fn detached(parts: &[DeckPart]) -> Vec<Slot> {
    parts
        .iter()
        .filter_map(|part| match part {
            DeckPart::Detach { slot } => Some(*slot),
            _ => None,
        })
        .collect()
}

#[kithara::test(tokio)]
#[case::chain_first(false)]
#[case::stop_first(true)]
async fn a_pause_over_a_held_native_chain_stops_the_stitched_successor(#[case] stop_first: bool) {
    use kithara_render::bridge::Returned;

    let mut rig = HandoverRig::playing().await;
    let outgoing = rig.slot(0);
    let incoming = rig.arm(1).await;
    let source = rig.source(1).to_owned();
    let mut observed = rig.queue.control().subscribe::<QueueEvent>();
    rig.chain();
    let expected = rig
        .queue
        .pending_handover
        .expect("native Chain pending")
        .seq;
    let mut held = rig.stitch();
    assert_eq!(held.len(), 1, "the native end answers only the Chain");
    let receipt = held.pop().expect("held native Chain receipt");
    assert_eq!(receipt.seq(), expected);
    let (outcome, mut batch) = receipt.into();
    assert!(matches!(outcome, Outcome::Applied { at, .. } if at == SessionFrame::new(128)));
    assert!(matches!(batch.commands.as_slice(),
        [DeckPart::Chain { from, to }] if *from == outgoing && *to == incoming));
    assert_eq!(rig.queue.current, Some(rig.ids[0]));

    let paused = rig
        .apply(QueueCommand::Pause { at: When::Next })
        .expect("pause after the native stitch")
        .expect("native Pause sequence");
    let mut stops = rig
        .deck
        .block(SessionFrame::new(256), 0.5)
        .expect("native Stops after the Chain");
    assert!(
        stops.iter().any(|receipt| receipt.seq() == paused
            && matches!(receipt.outcome(), Outcome::Applied { .. }))
    );
    let stopped = stops
        .iter()
        .flat_map(|receipt| receipt.batch().commands.iter())
        .filter(|part| {
            matches!(part,
            DeckPart::Returned(Returned::Stopped { slot, resume: Some(mark) })
                if *slot == incoming && mark.session == SessionFrame::new(256))
        })
        .count();
    assert_eq!(
        stopped, 1,
        "the actual stitched successor receives one native Stop"
    );
    assert!(
        stops
            .iter()
            .flat_map(|receipt| receipt.batch().commands.iter())
            .all(|part| !matches!(part,
            DeckPart::Returned(Returned::Pcm { slot, .. }) if *slot == incoming))
    );
    assert_eq!(rig.deck.mixer.held(incoming), Some(source.as_str()));

    if stop_first {
        rig.settle(std::mem::take(&mut stops));
    }
    assert!(matches!(rig.settle_batch(expected, &outcome, &mut batch),
        kithara_play::Settled::Applied { seq, at }
            if seq == expected && at == SessionFrame::new(128)));
    if !stop_first {
        rig.settle(stops);
    }
    assert_eq!(rig.queue.current, Some(rig.ids[1]));
    assert!(matches!(
        rig.queue.current_track().expect("stitched successor owner").snapshot().as_ref().status,
        PlayingStatus::Paused { at } if at == Duration::from_millis(500)
    ));
    assert_eq!(rig.deck.mixer.held(incoming), Some(source.as_str()));
    assert!(matches!(
        rig.settle_batch(expected, &outcome, &mut batch),
        kithara_play::Settled::Pending
    ));
    let changes = events(&mut observed)
        .into_iter()
        .filter(|event| {
            matches!(event,
        QueueEvent::CurrentTrackChanged { id: Some(id) } if *id == rig.ids[1])
        })
        .count();
    assert_eq!(
        changes, 1,
        "the original Chain publishes its successor once"
    );
}

#[kithara::test(tokio)]
#[case::chain_first(false)]
#[case::remove_first(true)]
#[kithara::hang_watchdog]
async fn removing_a_successor_over_a_held_native_chain_enters_the_next_item(
    #[case] remove_first: bool,
) {
    use kithara_render::{Dispatched, DispatcherCommand, bridge::Returned};

    let mut rig = HandoverRig::playing().await;
    let outgoing = rig.slot(0);
    let outgoing_source = rig.source(0).to_owned();
    let incoming = rig.arm(1).await;
    let incoming_source = rig.source(1).to_owned();
    let source = rig.dir.path().join("item-3.wav");
    rig.apply(QueueCommand::Append {
        position: Position::ZERO,
        id: rig.ids[2],
        source: TrackSource::Uri(source.to_str().expect("fixture source").to_owned()),
    })
    .expect("append the held successor after B");
    let following = rig.arm(2).await;
    let following_source = rig.source(2).to_owned();
    let following_lane = rig.last_lane.expect("opened following lane");
    let mut observed = rig.queue.control().subscribe::<QueueEvent>();
    rig.chain();
    let expected = rig
        .queue
        .pending_handover
        .expect("native Chain pending")
        .seq;
    let mut held = rig.stitch();
    assert_eq!(held.len(), 1, "the native end answers only the Chain");
    let receipt = held.pop().expect("held native Chain receipt");
    assert_eq!(receipt.seq(), expected);
    let (outcome, mut batch) = receipt.into();
    assert!(matches!(outcome, Outcome::Applied { at, .. } if at == SessionFrame::new(128)));
    assert!(matches!(batch.commands.as_slice(),
        [DeckPart::Chain { from, to }] if *from == outgoing && *to == incoming));
    assert_eq!(rig.queue.current, Some(rig.ids[0]));

    let removed_item = rig.ids[1];
    let removed = rig
        .on_pass(SessionFrame::new(128), |queue, out| {
            Player::apply(queue, QueueCommand::Remove(removed_item), out)
        })
        .expect("remove the physically stitched successor")
        .expect("native removal sequence");
    assert!(rig.queue.track(rig.ids[1]).is_none());
    let mut removals = rig
        .deck
        .block(SessionFrame::new(256), 0.5)
        .expect("native removal of B");
    assert!(
        removals.iter().any(|receipt| receipt.seq() == removed
            && matches!(receipt.outcome(), Outcome::Applied { .. }))
    );
    let returned = removals
        .iter()
        .flat_map(|receipt| receipt.batch().commands.iter())
        .filter_map(|part| match part {
            DeckPart::Returned(Returned::Pcm { slot, pcm }) => Some((*slot, &**pcm.src())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        returned,
        vec![(incoming, incoming_source.as_str())],
        "removing B returns only B's original consumer"
    );
    assert_eq!(
        rig.deck.mixer.held(outgoing),
        Some(outgoing_source.as_str())
    );
    assert_eq!(
        rig.deck.mixer.held(following),
        Some(following_source.as_str())
    );

    if remove_first {
        for receipt in std::mem::take(&mut removals) {
            let seq = receipt.seq();
            let (outcome, mut batch) = receipt.into();
            rig.on_pass(SessionFrame::new(256), |queue, out| {
                Player::settle(
                    queue,
                    TrackReceipt::Deck {
                        seq,
                        outcome: &outcome,
                        batch: &mut batch,
                    },
                    out,
                )
            });
        }
    }
    assert!(
        matches!(rig.on_pass(SessionFrame::new(256), |queue, out| Player::settle(queue,
        TrackReceipt::Deck { seq: expected, outcome: &outcome, batch: &mut batch }, out)),
        kithara_play::Settled::Applied { seq, at }
            if seq == expected && at == SessionFrame::new(128))
    );
    if !remove_first {
        for receipt in removals {
            let seq = receipt.seq();
            let (outcome, mut batch) = receipt.into();
            rig.on_pass(SessionFrame::new(256), |queue, out| {
                Player::settle(
                    queue,
                    TrackReceipt::Deck {
                        seq,
                        outcome: &outcome,
                        batch: &mut batch,
                    },
                    out,
                )
            });
        }
    }
    let entry = rig
        .deck
        .block(SessionFrame::new(384), 0.5)
        .expect("native entry past the removed B");
    assert!(
        entry.iter().any(
            |receipt| matches!(receipt.outcome(), Outcome::Applied { .. })
                && receipt
                    .batch()
                    .commands
                    .iter()
                    .any(|part| matches!(part, DeckPart::Start { slot, .. } if *slot == following))
        ),
        "C must enter on its own real native receipt"
    );
    rig.settle(entry);
    assert_eq!(rig.queue.current, Some(rig.ids[2]));
    assert!(rig.queue.track(rig.ids[1]).is_none());
    assert_eq!(rig.slot(2), following);
    assert_eq!(
        rig.deck.mixer.held(following),
        Some(following_source.as_str())
    );

    rig.deck.opens.drain();
    loop {
        let Some(due) = rig.deck.opens.next_due((), 1) else {
            break;
        };
        hang_tick!();
        let seq = due.seq();
        let data = match due.commands() {
            [DispatcherCommand::SetPriority(_, _)] => Dispatched::Prioritized,
            [DispatcherCommand::Release(lane)] => {
                assert_ne!(*lane, following_lane, "removing B cannot release C's lane");
                Dispatched::Released
            }
            _ => panic!("removal must reuse the held C instead of opening another source"),
        };
        due.apply(data);
        let receipt = rig
            .deck
            .dispatcher
            .receipts()
            .next()
            .expect("maintenance answer");
        assert_eq!(receipt.seq(), seq);
        with_outbox(&mut rig.queue, &mut rig.deck, |queue, out| {
            Player::settle(queue, TrackReceipt::Loaded(receipt), out)
        });
        hang_reset!();
    }
    assert!(matches!(
        rig.settle_batch(expected, &outcome, &mut batch),
        kithara_play::Settled::Pending
    ));
    assert_eq!(rig.queue.current, Some(rig.ids[2]));
    let changes = events(&mut observed)
        .into_iter()
        .filter(|event| {
            matches!(event,
        QueueEvent::CurrentTrackChanged { id: Some(id) } if *id == rig.ids[2])
        })
        .count();
    assert_eq!(
        changes, 1,
        "the native C entry publishes the surviving successor once"
    );
}

#[kithara::test(tokio)]
#[case::stale_chain_first(false, false, false)]
#[case::stale_remove_first(false, true, false)]
#[case::refused_chain_first(true, false, false)]
#[case::refused_remove_first(true, true, false)]
#[case::selected_chain_first(false, false, true)]
#[case::selected_remove_first(false, true, true)]
#[kithara::hang_watchdog]
async fn removing_an_unstarted_chain_target_keeps_the_confirmed_current(
    #[case] refused: bool,
    #[case] remove_first: bool,
    #[case] select_following: bool,
) {
    use kithara_render::bridge::{DeckRefusal, Returned};

    let mut rig = HandoverRig::playing().await;
    let outgoing = rig.slot(0);
    let outgoing_source = rig.source(0).to_owned();
    let incoming = rig.arm(1).await;
    let incoming_source = rig.source(1).to_owned();
    let following_item = rig.ids[2];
    let source = rig.dir.path().join("item-3.wav");
    rig.apply(QueueCommand::Append {
        position: Position::ZERO,
        id: following_item,
        source: TrackSource::Uri(source.to_str().expect("fixture source").to_owned()),
    })
    .expect("append following C");
    let following = rig.arm(2).await;
    let following_source = rig.source(2).to_owned();
    let mut observed = rig.queue.control().subscribe::<QueueEvent>();
    rig.chain();
    let pending = rig.queue.pending_handover.expect("parked native Chain");
    assert_eq!(pending.item, rig.ids[1]);
    let held_refusal = if refused {
        rig.deck.ring.publish().expect("publish parked Chain");
        rig.deck.inbox.drain();
        let mut scope = rig.deck.inbox.scope(rig.deck.scope).expect("deck scope");
        let due = scope
            .resume(pending.seq, SessionFrame::new(128), SessionFrame::new(128))
            .expect("the original parked Chain is still unstarted");
        assert_eq!(due.seq(), pending.seq);
        assert!(
            matches!(due.commands(), [DeckPart::Chain { from, to }] if *from == outgoing && *to == incoming)
        );
        due.refuse(DeckRefusal::Deferral);
        let ScopedReceipt::Scope(_, receipt) =
            rig.deck.ring.receipt().expect("owned Chain refusal")
        else {
            panic!("Chain receipt belongs to its deck scope");
        };
        assert!(matches!(
            receipt.outcome(),
            Outcome::Rejected(Rejection::Refused(DeckRefusal::Deferral))
        ));
        Some(receipt)
    } else {
        None
    };
    let removed_item = rig.ids[1];
    let removed = rig
        .on_pass(SessionFrame::new(128), |queue, out| {
            Player::apply(queue, QueueCommand::Remove(removed_item), out)
        })
        .expect("remove pending B")
        .expect("native removal sequence");
    if select_following {
        rig.on_pass(SessionFrame::new(128), |queue, out| {
            Player::apply(
                queue,
                QueueCommand::Select {
                    id: following_item,
                    transition: Transition::None,
                },
                out,
            )
        })
        .expect("explicit C replaces the conditional removal intent");
        let target = rig
            .queue
            .target
            .expect("C selection retains its custody gate");
        assert_eq!(target.reason, AdvanceReason::UserSelect);
        assert!(
            matches!(target.continued, Some(crate::queue::types::Continuation::Removal {
            seq, handover, applied: false, ..
        }) if seq == removed && handover == pending.seq)
        );
    }
    let mut removals = rig
        .deck
        .block(SessionFrame::new(256), 0.5)
        .expect("native B removal");
    let chain = held_refusal.unwrap_or_else(|| {
        let index = removals
            .iter()
            .position(|receipt| receipt.seq() == pending.seq)
            .expect("basis shift answers the unstarted Chain");
        let receipt = removals.remove(index);
        assert!(matches!(
            receipt.outcome(),
            Outcome::Rejected(Rejection::Stale)
        ));
        receipt
    });
    assert!(
        removals.iter().any(|receipt| receipt.seq() == removed
            && matches!(receipt.outcome(), Outcome::Applied { .. }))
    );
    let returned = removals
        .iter()
        .flat_map(|receipt| receipt.batch().commands.iter())
        .filter_map(|part| match part {
            DeckPart::Returned(Returned::Pcm { slot, pcm }) => Some((*slot, &**pcm.src())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(returned, vec![(incoming, incoming_source.as_str())]);
    assert!(
        removals
            .iter()
            .flat_map(|receipt| &receipt.batch().commands)
            .all(|part| !matches!(part,
        DeckPart::Start { slot, .. } | DeckPart::Chain { to: slot, .. } if *slot == following))
    );
    if remove_first {
        for receipt in std::mem::take(&mut removals) {
            let seq = receipt.seq();
            let (outcome, mut batch) = receipt.into();
            rig.on_pass(SessionFrame::new(256), |queue, out| {
                Player::settle(
                    queue,
                    TrackReceipt::Deck {
                        seq,
                        outcome: &outcome,
                        batch: &mut batch,
                    },
                    out,
                )
            });
        }
    }
    let seq = chain.seq();
    let (outcome, mut batch) = chain.into();
    assert_eq!(seq, pending.seq);
    let result = rig.on_pass(SessionFrame::new(256), |queue, out| {
        Player::settle(
            queue,
            TrackReceipt::Deck {
                seq,
                outcome: &outcome,
                batch: &mut batch,
            },
            out,
        )
    });
    assert!(
        matches!(result, kithara_play::Settled::Rejected { seq: answered, reason: Rejection::Stale } if !refused && answered == pending.seq)
            || matches!(result, kithara_play::Settled::Rejected { seq: answered, reason: Rejection::Refused(PlayError::Deck(DeckRefusal::Deferral)) } if refused && answered == pending.seq)
    );
    if !remove_first {
        for receipt in removals {
            let seq = receipt.seq();
            let (outcome, mut batch) = receipt.into();
            rig.on_pass(SessionFrame::new(256), |queue, out| {
                Player::settle(
                    queue,
                    TrackReceipt::Deck {
                        seq,
                        outcome: &outcome,
                        batch: &mut batch,
                    },
                    out,
                )
            });
        }
    }
    assert_eq!(rig.queue.current, Some(rig.ids[0]));
    if select_following {
        assert!(
            rig.queue
                .pending_handover
                .is_some_and(|entry| entry.item == following_item)
        );
        assert_eq!(
            rig.queue
                .target
                .expect("explicit C survives the stale B verdict")
                .reason,
            AdvanceReason::UserSelect
        );
        let entries = rig
            .deck
            .block(SessionFrame::new(384), 0.5)
            .expect("explicit C owner deadline");
        assert_eq!(entries.len(), 1, "one C entry without a repeated B release");
        assert!(entries.iter().any(|receipt| matches!(receipt.outcome(), Outcome::Applied { at, .. } if *at == SessionFrame::new(384))
            && receipt.batch().commands.iter().any(|part| matches!(part, DeckPart::Start { slot, .. } if *slot == following))));
        rig.settle(entries);
        assert_eq!(rig.queue.current, Some(following_item));
        assert_eq!(
            rig.deck.mixer.held(following),
            Some(following_source.as_str())
        );
        assert!(rig.queue.track(removed_item).is_none());
        assert_eq!(events(&mut observed).iter().filter(|event| matches!(event,
            QueueEvent::CurrentTrackAdvance { id: Some(id), reason: AdvanceReason::UserSelect } if *id == following_item)).count(), 1);
        return;
    }
    assert!(rig.queue.target.is_none());
    assert!(rig.queue.pending_handover.is_none());
    assert!(rig.queue.track(removed_item).is_none());
    let preloaded = rig
        .queue
        .active
        .position(|active| active.item == following_item && active.role == Role::Preloaded)
        .expect("C remains preloaded after an unstarted B is removed");
    assert_eq!(
        rig.queue.active.get(preloaded).expect("C owner").slot,
        following
    );
    assert_eq!(
        rig.deck.mixer.held(outgoing),
        Some(outgoing_source.as_str())
    );
    assert_eq!(
        rig.deck.mixer.held(following),
        Some(following_source.as_str())
    );
    let next = rig
        .deck
        .block(SessionFrame::new(384), 0.5)
        .expect("next owner deadline");
    assert!(
        next.iter()
            .flat_map(|receipt| &receipt.batch().commands)
            .all(|part| !matches!(part,
        DeckPart::Start { slot, .. } | DeckPart::Chain { to: slot, .. } if *slot == following))
    );
    rig.settle(next);
    assert_eq!(rig.queue.current, Some(rig.ids[0]));
    assert!(events(&mut observed).iter().all(|event| !matches!(event,
        QueueEvent::CurrentTrackChanged { id: Some(id) } if *id == removed_item || *id == following_item)));
}

#[kithara::test(tokio)]
#[case::stale(false)]
#[case::refused(true)]
#[kithara::hang_watchdog]
async fn removing_an_unstarted_staged_replacement_does_not_select_its_neighbor(
    #[case] refused: bool,
) {
    use kithara_render::{
        Dispatched, DispatcherCommand,
        bridge::{DeckRefusal, Returned},
    };

    let config = DeckMixerConfig::builder()
        .slots(std::num::NonZeroUsize::new(2).expect("two mixer slots"))
        .build();
    let mut rig =
        HandoverRig::playing_with_config(["item-1.wav", "item-2.wav", "item-3.wav"], config).await;
    let old_slot = rig.slot(0);
    let old_source = rig.source(0).to_owned();
    rig.arm(1).await;
    let entry = rig.entry(1, Transition::Crossfade);
    rig.settle(entry);
    let current_slot = rig.slot(1);
    let current_source = rig.source(1).to_owned();
    assert_eq!(rig.queue.current, Some(rig.ids[1]));
    rig.queue.deck.mixer.slots[usize::from(old_slot.get())].state = SlotState::Playing;
    let following_item = rig.ids[2];
    let source = rig.dir.path().join("item-3.wav");
    rig.apply(QueueCommand::Insert {
        id: following_item,
        source: TrackSource::Uri(source.to_str().expect("fixture source").to_owned()),
        after: Some(rig.ids[0]),
    })
    .expect("C follows the outgoing A in queue order");
    rig.select(0, Transition::Crossfade)
        .expect("new A replacement intent");
    rig.load(0, 64.295).await;
    let pending = rig
        .queue
        .pending_handover
        .expect("staged A native replacement");
    assert!(pending.replacement);
    assert_eq!(pending.slot, old_slot);
    let held_refusal = if refused {
        rig.deck.ring.publish().expect("publish A replacement");
        rig.deck.inbox.drain();
        let mut scope = rig.deck.inbox.scope(rig.deck.scope).expect("deck scope");
        let due = scope
            .next_due(SessionFrame::new(128), 1)
            .expect("actual replacement due");
        assert_eq!(due.seq(), pending.seq);
        assert!(
            due.commands()
                .iter()
                .any(|part| matches!(part, DeckPart::Replace { slot, .. } if *slot == old_slot))
        );
        due.refuse(DeckRefusal::Deferral);
        let ScopedReceipt::Scope(_, receipt) =
            rig.deck.ring.receipt().expect("owned replacement refusal")
        else {
            panic!("replacement receipt belongs to its deck scope");
        };
        Some(receipt)
    } else {
        None
    };
    let removed_item = rig.ids[0];
    rig.on_pass(SessionFrame::new(128), |queue, out| {
        Player::apply(queue, QueueCommand::Remove(removed_item), out)
    })
    .expect("remove outgoing and staged A");
    assert!(
        rig.queue.target.is_none(),
        "an unstarted staged replacement cannot conditionally select C"
    );
    let mut replies = rig
        .deck
        .block(SessionFrame::new(256), 0.0)
        .expect("old A detaches");
    let replacement = held_refusal.map_or_else(
        || {
            let index = replies
                .iter()
                .position(|receipt| receipt.seq() == pending.seq)
                .expect("superseded replacement answers");
            let receipt = replies.remove(index);
            assert!(matches!(
                receipt.outcome(),
                Outcome::Rejected(Rejection::Stale)
            ));
            receipt
        },
        |receipt| {
            assert!(matches!(
                receipt.outcome(),
                Outcome::Rejected(Rejection::Refused(DeckRefusal::Deferral))
            ));
            receipt
        },
    );
    assert_eq!(replies.iter().flat_map(|receipt| &receipt.batch().commands).filter(|part| matches!(part,
        DeckPart::Returned(Returned::Pcm { slot, pcm }) if *slot == old_slot && &**pcm.src() == old_source.as_str())).count(), 1);
    replies.push(replacement);
    rig.settle(replies);
    assert_eq!(rig.queue.current, Some(rig.ids[1]));
    assert!(rig.queue.target.is_none());
    assert!(rig.queue.pending_handover.is_none());
    assert!(rig.queue.track(removed_item).is_none());
    assert_eq!(rig.deck.mixer.held(old_slot), None);
    assert_eq!(
        rig.deck.mixer.held(current_slot),
        Some(current_source.as_str())
    );
    rig.deck.opens.drain();
    while let Some(due) = rig.deck.opens.next_due((), 1) {
        hang_tick!();
        let data = match due.commands() {
            [DispatcherCommand::SetPriority(_, _)] => Dispatched::Prioritized,
            [DispatcherCommand::Release(_)] => Dispatched::Released,
            _ => panic!("removing the unstarted replacement must not open C"),
        };
        due.apply(data);
        hang_reset!();
    }
}

#[kithara::test(tokio)]
#[case::refused_conditional(false, false)]
#[case::stale_conditional(true, false)]
#[case::refused_explicit(false, true)]
#[kithara::hang_watchdog]
async fn a_rejected_removal_detach_finishes_its_custody_gate(
    #[case] stale: bool,
    #[case] explicit: bool,
) {
    use kithara_render::bridge::DeckRefusal;

    let mut rig = HandoverRig::playing().await;
    let incoming = rig.arm(1).await;
    let incoming_source = rig.source(1).to_owned();
    let following_item = rig.ids[2];
    let source = rig.dir.path().join("item-3.wav");
    rig.apply(QueueCommand::Append {
        position: Position::ZERO,
        id: following_item,
        source: TrackSource::Uri(source.to_str().expect("fixture source").to_owned()),
    })
    .expect("append C");
    let following = rig.arm(2).await;
    let following_source = rig.source(2).to_owned();
    rig.chain();
    let pending = rig.queue.pending_handover.expect("original Chain");
    let held = rig.stitch();
    let chain = held
        .into_iter()
        .find(|receipt| receipt.seq() == pending.seq)
        .expect("held Applied Chain");
    assert!(
        matches!(chain.outcome(), Outcome::Applied { at, .. } if *at == SessionFrame::new(128))
    );
    let removed_item = rig.ids[1];
    let removed = rig
        .on_pass(SessionFrame::new(128), |queue, out| {
            Player::apply(queue, QueueCommand::Remove(removed_item), out)
        })
        .expect("remove held successor")
        .expect("owned Detach sequence");
    if explicit {
        rig.on_pass(SessionFrame::new(128), |queue, out| {
            Player::apply(
                queue,
                QueueCommand::Select {
                    id: following_item,
                    transition: Transition::None,
                },
                out,
            )
        })
        .expect("explicit C");
    }
    let seq = chain.seq();
    let (outcome, mut batch) = chain.into();
    rig.on_pass(SessionFrame::new(128), |queue, out| {
        Player::settle(
            queue,
            TrackReceipt::Deck {
                seq,
                outcome: &outcome,
                batch: &mut batch,
            },
            out,
        )
    });
    assert_eq!(rig.queue.current, Some(removed_item));
    assert!(rig.queue.pending_handover.is_none());
    assert!(
        matches!(rig.queue.target.expect("C awaits B custody").continued,
        Some(crate::queue::types::Continuation::Removal { seq, applied: false, .. }) if seq == removed)
    );
    rig.deck.ring.publish().expect("publish B removal");
    rig.deck.inbox.drain();
    let stop_seq = {
        let mut scope = rig.deck.inbox.scope(rig.deck.scope).expect("deck scope");
        let stop = scope
            .next_due(SessionFrame::new(256), 1)
            .expect("prior cancellation Stop");
        assert_ne!(stop.seq(), removed);
        assert!(
            stop.commands()
                .iter()
                .any(|part| matches!(part, DeckPart::Stop { slot, .. } if *slot == incoming))
        );
        let seq = stop.seq();
        stop.apply(());
        seq
    };
    let ScopedReceipt::Scope(_, stop_answer) = rig
        .deck
        .ring
        .receipt()
        .expect("prior Stop terminal receipt")
    else {
        panic!("Stop receipt belongs to its deck scope");
    };
    assert_eq!(stop_answer.seq(), stop_seq);
    assert!(
        matches!(stop_answer.outcome(), Outcome::Applied { at, .. } if *at == SessionFrame::new(256))
    );
    let rejected = if stale {
        let basis = {
            let mut scope = rig.deck.inbox.scope(rig.deck.scope).expect("deck scope");
            let due = scope
                .next_due(SessionFrame::new(256), 1)
                .expect("owned B Detach before execution");
            assert_eq!(due.seq(), removed);
            assert!(matches!(due.commands(), [DeckPart::Detach { slot }] if *slot == incoming));
            assert_eq!(due.basis(), &[(incoming, Some(stop_seq))]);
            let basis = due.basis().to_vec();
            due.defer();
            basis
        };
        assert_eq!(
            rig.deck.mixer.held(incoming),
            Some(incoming_source.as_str())
        );
        let superseded = rig
            .deck
            .ring
            .scope(rig.deck.scope)
            .expect("deck scope")
            .send(
                When::Next,
                Batch {
                    basis,
                    commands: Vec::new(),
                },
            )
            .expect("supersede the parked Detach from its actual Stop basis");
        let replies = rig
            .deck
            .block(SessionFrame::new(256), 0.5)
            .expect("native basis rejection");
        assert!(replies.iter().any(|receipt| receipt.seq() == superseded
            && matches!(receipt.outcome(), Outcome::Applied { at, .. } if *at == SessionFrame::new(256))
            && receipt.batch().commands.is_empty()));
        let applied_epoch = replies
            .iter()
            .position(|receipt| receipt.seq() == superseded)
            .expect("superseding epoch receipt");
        let stale_detach = replies
            .iter()
            .position(|receipt| receipt.seq() == removed)
            .expect("parked Detach receipt");
        assert!(applied_epoch < stale_detach);
        let receipt = replies
            .into_iter()
            .find(|receipt| receipt.seq() == removed)
            .expect("original Detach Stale");
        assert!(matches!(
            receipt.outcome(),
            Outcome::Rejected(Rejection::Stale)
        ));
        receipt
    } else {
        let mut scope = rig.deck.inbox.scope(rig.deck.scope).expect("deck scope");
        let due = scope
            .next_due(SessionFrame::new(256), 1)
            .expect("owned B Detach due");
        assert_eq!(due.seq(), removed);
        assert!(
            due.commands()
                .iter()
                .any(|part| matches!(part, DeckPart::Detach { slot } if *slot == incoming))
        );
        due.refuse(DeckRefusal::Deferral);
        let ScopedReceipt::Scope(_, receipt) =
            rig.deck.ring.receipt().expect("owned Detach refusal")
        else {
            panic!("Detach receipt belongs to its deck scope");
        };
        assert!(matches!(
            receipt.outcome(),
            Outcome::Rejected(Rejection::Refused(DeckRefusal::Deferral))
        ));
        receipt
    };
    let seq = rejected.seq();
    let (outcome, mut batch) = rejected.into();
    let original_basis = batch.basis.clone();
    let dependency = batch
        .basis
        .iter_mut()
        .find(|(slot, _)| *slot == incoming)
        .expect("owned Detach basis");
    assert!(
        dependency.1.is_some(),
        "the held Chain has a native B basis"
    );
    dependency.1 = None;
    let foreign = rig.on_pass(SessionFrame::new(256), |queue, out| {
        Player::settle(
            queue,
            TrackReceipt::Deck {
                seq,
                outcome: &outcome,
                batch: &mut batch,
            },
            out,
        )
    });
    assert!(matches!(foreign, kithara_play::Settled::Pending));
    assert!(
        matches!(rig.queue.target.expect("foreign basis cannot cancel C").continued,
        Some(crate::queue::types::Continuation::Removal { seq: retained, applied: false, .. }) if retained == removed)
    );
    assert_eq!(rig.queue.current, Some(removed_item));
    batch.basis = original_basis;
    let result = rig.on_pass(SessionFrame::new(256), |queue, out| {
        Player::settle(
            queue,
            TrackReceipt::Deck {
                seq,
                outcome: &outcome,
                batch: &mut batch,
            },
            out,
        )
    });
    assert!(
        matches!(result, kithara_play::Settled::Rejected { seq: answered, reason: Rejection::Stale } if stale && answered == removed)
            || matches!(result, kithara_play::Settled::Rejected { seq: answered, reason: Rejection::Refused(PlayError::Deck(DeckRefusal::Deferral)) } if !stale && answered == removed)
    );
    assert!(
        rig.queue.target.is_none(),
        "terminal removal refusal cannot leave a pending custody gate"
    );
    assert!(rig.queue.pending_handover.is_none());
    assert_eq!(rig.queue.current, Some(removed_item));
    assert_eq!(
        rig.deck.mixer.held(incoming),
        Some(incoming_source.as_str())
    );
    if !explicit {
        let preload = rig
            .queue
            .active
            .position(|active| active.item == following_item && active.role == Role::Preloaded)
            .expect("conditional C remains available after refused custody");
        assert_eq!(
            rig.queue.active.get(preload).expect("C owner").slot,
            following
        );
        assert_eq!(
            rig.deck.mixer.held(following),
            Some(following_source.as_str())
        );
    }
    let next = rig
        .deck
        .block(SessionFrame::new(384), 0.5)
        .expect("next block after terminal removal");
    assert!(
        next.iter()
            .flat_map(|receipt| &receipt.batch().commands)
            .all(|part| !matches!(part,
        DeckPart::Start { slot, .. } | DeckPart::Chain { to: slot, .. } if *slot == following))
    );
    rig.settle(next);
    assert_eq!(rig.queue.current, Some(removed_item));
}

#[kithara::test(tokio)]
#[case::crossfade(false)]
#[case::chain(true)]
async fn a_native_handover_answers_after_its_player_changes_segment(#[case] chained: bool) {
    let mut rig = HandoverRig::playing().await;
    rig.arm(1).await;
    let incoming = rig.slot(1);
    let mut observed = rig.queue.control().subscribe::<QueueEvent>();
    let receipts = if chained {
        rig.chain();
        rig.stitch()
    } else {
        rig.entry(1, Transition::Crossfade)
    };
    let receipt = receipts
        .into_iter()
        .find(|receipt| {
            receipt.batch().commands.iter().any(|part| {
                matches!(part, DeckPart::Start { slot, .. } if *slot == incoming)
                    || matches!(part, DeckPart::Chain { to, .. } if *to == incoming)
            })
        })
        .expect("native entry receipt");
    let seq = receipt.seq();
    let (outcome, mut batch) = receipt.into();
    assert!(matches!(outcome, Outcome::Applied { at, .. } if at == SessionFrame::new(128)));
    with_outbox(&mut rig.queue, &mut rig.deck, |queue, out| {
        let index = queue.incoming_index(rig.ids[1]).expect("pending incoming");
        queue
            .active
            .get_mut(index)
            .expect("incoming player")
            .track
            .apply(TrackCommand::Seek { to: Position::ZERO }, out)
    })
    .expect("new segment admitted")
    .expect("native adoption sequence");
    assert_eq!(rig.queue.current, Some(rig.ids[0]));
    assert!(matches!(rig.settle_batch(seq, &outcome, &mut batch),
        kithara_play::Settled::Applied { seq: answered, at }
        if answered == seq && at == SessionFrame::new(128)));
    assert_eq!(rig.queue.current, Some(rig.ids[1]));
    assert!(
        matches!(rig.queue.current_track().expect("promoted player").snapshot().as_ref().status,
        PlayingStatus::Playing { since } if since == SessionFrame::new(128))
    );
    assert!(matches!(
        rig.settle_batch(seq, &outcome, &mut batch),
        kithara_play::Settled::Pending
    ));
    let changed = events(&mut observed).into_iter().filter(|event| {
        matches!(event, QueueEvent::CurrentTrackChanged { id: Some(id) } if *id == rig.ids[1])
    }).count();
    assert_eq!(
        changed, 1,
        "the native verdict changes the sounding item once"
    );
}

#[kithara::test(tokio)]
async fn a_native_handover_refusal_survives_its_player_changing_segment() {
    let mut rig = HandoverRig::playing().await;
    rig.arm(1).await;
    let mut observed = rig.queue.control().subscribe::<QueueEvent>();
    let seq = rig
        .select(1, Transition::Crossfade)
        .expect("entry admitted")
        .expect("entry sequence");
    rig.deck.ring.publish().expect("publish entry");
    rig.deck.inbox.drain();
    let mut scope = rig.deck.inbox.scope(rig.deck.scope).expect("deck scope");
    let due = scope
        .next_due(SessionFrame::new(128), 1)
        .expect("native entry due");
    assert_eq!(due.seq(), seq);
    due.refuse(kithara_render::bridge::DeckRefusal::Deferral);
    let ScopedReceipt::Scope(_, receipt) = rig.deck.ring.receipt().expect("native refusal") else {
        panic!("entry receipt belongs to its deck scope");
    };
    let (outcome, mut batch) = receipt.into();
    with_outbox(&mut rig.queue, &mut rig.deck, |queue, out| {
        let index = queue.incoming_index(rig.ids[1]).expect("pending incoming");
        queue
            .active
            .get_mut(index)
            .expect("incoming player")
            .track
            .apply(TrackCommand::Seek { to: Position::ZERO }, out)
    })
    .expect("new segment admitted");
    assert!(matches!(rig.settle_batch(seq, &outcome, &mut batch),
        kithara_play::Settled::Rejected { seq: answered, reason: Rejection::Refused(_) } if answered == seq));
    assert_eq!(rig.queue.current, Some(rig.ids[0]));
    assert!(
        rig.queue.target.is_none(),
        "the refused target is cancelled"
    );
    assert!(events(&mut observed).iter().all(|event| !matches!(
        event,
        QueueEvent::CrossfadeStarted { .. } | QueueEvent::CurrentTrackChanged { .. }
    )));
}

#[kithara::test(tokio)]
async fn an_unrelated_native_receipt_cannot_publish_a_pending_handover() {
    let mut rig = HandoverRig::playing().await;
    rig.arm(1).await;
    let wanted = rig
        .select(1, Transition::Crossfade)
        .expect("entry admitted")
        .expect("entry sequence");
    let other = rig.unrelated_mix();
    assert_ne!(other, wanted);
    let unrelated = rig
        .deck
        .block(SessionFrame::new(0), 0.0)
        .expect("unrelated block");
    assert_eq!(unrelated.len(), 1);
    let receipt = unrelated.into_iter().next().expect("unrelated receipt");
    assert_eq!(receipt.seq(), other);
    rig.settle(vec![receipt]);
    assert_eq!(rig.queue.current, Some(rig.ids[0]));
    assert_eq!(
        rig.queue.target.expect("entry still pending").to,
        rig.ids[1]
    );
    let entry = rig
        .deck
        .block(SessionFrame::new(128), 0.0)
        .expect("entry block");
    rig.settle(entry);
    assert_eq!(rig.queue.current, Some(rig.ids[1]));
}

#[kithara::test(tokio)]
async fn a_repeat_receipt_names_the_new_slot_of_the_same_item() {
    let mut rig = HandoverRig::playing().await;
    rig.apply(QueueCommand::SetRepeat(crate::RepeatMode::One))
        .expect("repeat one");
    rig.queue.config.action_at_item_end = ActionAtItemEnd::None;
    let old_slot = rig.slot(0);
    rig.event(DeckEvent::Ended {
        slot: old_slot,
        at: SessionFrame::new(128),
    });
    with_outbox(&mut rig.queue, &mut rig.deck, |queue, out| {
        queue.next_target(
            Transition::None,
            AdvanceReason::NaturalEof,
            true,
            true,
            Some(&mock::output(None).get()),
            out,
        )
    })
    .expect("repeat entry");
    rig.load(0, 64.295).await;
    let incoming = rig
        .queue
        .incoming_index(rig.ids[0])
        .expect("new repeat owner");
    let new_slot = rig
        .queue
        .active
        .get(incoming)
        .expect("new repeat slot")
        .slot;
    assert_ne!(new_slot, old_slot);
    assert_eq!(
        rig.queue
            .active
            .iter()
            .filter(|active| active.item == rig.ids[0])
            .count(),
        2
    );
    let pending = rig.queue.pending_handover.expect("native repeat Start");
    assert_eq!(pending.slot, new_slot);
    let mut receiver = rig.queue.control().subscribe::<QueueEvent>();
    let receipts = rig
        .deck
        .block(SessionFrame::new(128), 0.0)
        .expect("native repeat entry");
    let receipt = receipts
        .into_iter()
        .find(|receipt| receipt.seq() == pending.seq)
        .expect("repeat native receipt");
    assert!(matches!(receipt.outcome(), Outcome::Applied { .. }));
    let seq = receipt.seq();
    let (outcome, mut batch) = receipt.into();
    assert!(
        batch
            .commands
            .iter()
            .any(|part| matches!(part, DeckPart::Start { slot, .. } if *slot == new_slot))
    );
    assert!(batch.basis.iter().all(|&(slot, _)| slot != old_slot));
    let original = batch.basis.clone();
    batch.basis = vec![(old_slot, None)];
    rig.settle_batch(seq, &outcome, &mut batch);
    assert_eq!(rig.queue.current, Some(rig.ids[0]));
    let current = rig
        .queue
        .active_current_index()
        .expect("old ended repeat owner");
    let old = rig.queue.active.get(current).expect("old repeat slot");
    assert_eq!(old.item, rig.ids[0]);
    assert_eq!(old.slot, old_slot);
    assert_eq!(old.role, Role::Current);
    assert!(
        matches!(old.track.snapshot().status, PlayingStatus::Ended { at } if at == SessionFrame::new(128))
    );
    assert_eq!(
        rig.queue.pending_handover.map(|pending| pending.seq),
        Some(seq)
    );
    assert!(
        events(&mut receiver)
            .iter()
            .all(|event| !matches!(event, QueueEvent::CurrentTrackAdvance { .. }))
    );
    batch.basis = original;
    assert!(
        matches!(rig.settle_batch(seq, &outcome, &mut batch), kithara_play::Settled::Applied { seq: answered, .. } if answered == seq)
    );
    assert_eq!(
        rig.queue
            .current_track()
            .expect("new repeat owner")
            .snapshot()
            .slot,
        Some(new_slot)
    );
    assert_eq!(
        rig.queue.track(rig.ids[0]).map(|entry| entry.status),
        Some(TrackStatus::Consumed)
    );
    assert_eq!(events(&mut receiver).iter().filter(|event| matches!(event,
        QueueEvent::CurrentTrackAdvance { id: Some(id), reason: AdvanceReason::NaturalEof } if *id == rig.ids[0]
    )).count(), 1);
}

#[kithara::test(tokio)]
async fn returning_to_an_outgoing_item_commits_its_new_native_slot() {
    let mut rig = HandoverRig::playing().await;
    let old_slot = rig.slot(0);
    let source = rig.source(0).to_owned();
    rig.arm(1).await;
    let entry = rig.entry(1, Transition::Crossfade);
    rig.settle(entry);
    assert_eq!(rig.queue.current, Some(rig.ids[1]));
    assert_eq!(rig.deck.mixer.held(old_slot), Some(source.as_str()));
    rig.select(0, Transition::Crossfade).expect("return to A");
    rig.load(0, 64.295).await;
    let incoming = rig.queue.incoming_index(rig.ids[0]).expect("new A owner");
    let new_slot = rig.queue.active.get(incoming).expect("new A slot").slot;
    assert_ne!(new_slot, old_slot);
    let pending = rig.queue.pending_handover.expect("new A native entry");
    assert_eq!(pending.slot, new_slot);
    assert_eq!(rig.deck.mixer.held(old_slot), Some(source.as_str()));
    let mut receiver = rig.queue.control().subscribe::<QueueEvent>();
    let receipts = rig
        .deck
        .block(SessionFrame::new(128), 0.0)
        .expect("native return to A");
    let entry = receipts
        .iter()
        .find(|receipt| receipt.seq() == pending.seq)
        .expect("new A verdict");
    assert!(matches!(entry.outcome(), Outcome::Applied { .. }));
    assert!(
        entry
            .batch()
            .basis
            .iter()
            .all(|&(slot, _)| slot != old_slot)
    );
    assert!(
        entry
            .batch()
            .commands
            .iter()
            .any(|part| matches!(part, DeckPart::Start { slot, .. } if *slot == new_slot))
    );
    rig.settle(receipts);
    assert_eq!(rig.queue.current, Some(rig.ids[0]));
    assert_eq!(
        rig.queue
            .current_track()
            .expect("returned A")
            .snapshot()
            .slot,
        Some(new_slot)
    );
    assert_eq!(rig.deck.mixer.held(new_slot), Some(source.as_str()));
    assert_eq!(events(&mut receiver).iter().filter(|event| matches!(event,
        QueueEvent::CurrentTrackAdvance { id: Some(id), reason: AdvanceReason::UserSelect } if *id == rig.ids[0]
    )).count(), 1);
}

#[kithara::test(tokio)]
#[case::sequence(false)]
#[case::basis(true)]
async fn a_foreign_native_receipt_cannot_answer_the_queues_entry(#[case] foreign_basis: bool) {
    let mut rig = HandoverRig::playing().await;
    rig.arm(1).await;
    let receipt = rig
        .entry(1, Transition::Crossfade)
        .into_iter()
        .next()
        .expect("entry receipt");
    let seq = receipt.seq();
    let (outcome, mut batch) = receipt.into();
    let original_basis = batch.basis.clone();
    let other = rig.unrelated_mix();
    let delivered = if foreign_basis {
        batch.basis.retain(|&(slot, _)| slot != rig.slot(1));
        seq
    } else {
        other
    };
    rig.settle_batch(delivered, &outcome, &mut batch);
    assert_eq!(rig.queue.current, Some(rig.ids[0]));
    assert_eq!(
        rig.queue.target.expect("original target remains").to,
        rig.ids[1]
    );
    batch.basis = original_basis;
    assert!(matches!(rig.settle_batch(seq, &outcome, &mut batch),
        kithara_play::Settled::Applied { seq: answered, at }
        if answered == seq && at == SessionFrame::new(128)));
    assert_eq!(rig.queue.current, Some(rig.ids[1]));
}

#[kithara::test(tokio)]
#[kithara::hang_watchdog]
async fn returning_to_an_outgoing_item_replaces_its_old_owner_in_the_same_slot() {
    let config = DeckMixerConfig::builder()
        .slots(std::num::NonZeroUsize::new(2).expect("two mixer slots"))
        .build();
    let mut rig =
        HandoverRig::playing_with_config(["item-1.wav", "item-2.wav", "item-3.wav"], config).await;
    let old_slot = rig.slot(0);
    let old_lane = rig.last_lane.expect("original A lane");
    let source = rig.source(0).to_owned();
    rig.arm(1).await;
    let entry = rig.entry(1, Transition::Crossfade);
    rig.settle(entry);
    assert_eq!(rig.queue.current, Some(rig.ids[1]));
    assert_eq!(rig.deck.mixer.held(old_slot), Some(source.as_str()));
    rig.queue.deck.mixer.slots[usize::from(old_slot.get())].state = SlotState::Playing;
    rig.select(0, Transition::Crossfade)
        .expect("return to A on a full deck");
    rig.load(0, 64.295).await;
    let replacement = rig.queue.incoming_index(rig.ids[0]).expect("new staged A");
    assert!(rig.queue.active.is_replacement(replacement));
    assert_eq!(
        rig.queue.active.get(replacement).expect("staged A").slot,
        old_slot
    );
    assert_eq!(
        rig.queue
            .active
            .iter()
            .filter(|active| active.item == rig.ids[0] && active.slot == old_slot)
            .count(),
        2
    );
    assert_ne!(rig.last_lane, Some(old_lane));
    let pending = rig
        .queue
        .pending_handover
        .expect("replacement native entry");
    assert_eq!(pending.item, rig.ids[0]);
    assert_eq!(pending.slot, old_slot);
    let mut receiver = rig.queue.control().subscribe::<QueueEvent>();
    let receipts = rig
        .deck
        .block(SessionFrame::new(128), 0.0)
        .expect("native same-item same-slot replacement");
    let entry = receipts
        .iter()
        .find(|receipt| receipt.seq() == pending.seq)
        .expect("replacement verdict");
    assert!(matches!(entry.outcome(), Outcome::Applied { .. }));
    assert!(
        entry
            .batch()
            .commands
            .iter()
            .any(|part| matches!(part, DeckPart::Start { slot, .. } if *slot == old_slot))
    );
    assert_eq!(
        entry
            .batch()
            .commands
            .iter()
            .filter(|part| matches!(part,
                DeckPart::Returned(Returned::Pcm { slot, pcm })
                    if *slot == old_slot && &**pcm.src() == source.as_str()
            ))
            .count(),
        1
    );
    rig.settle(receipts);
    assert_eq!(rig.queue.current, Some(rig.ids[0]));
    assert!(rig.queue.active.replacement_index().is_none());
    assert_eq!(
        rig.queue
            .current_track()
            .expect("replacement A owns playback")
            .snapshot()
            .slot,
        Some(old_slot)
    );
    assert_eq!(
        rig.queue
            .active
            .iter()
            .filter(|active| active.item == rig.ids[0] && active.slot == old_slot)
            .count(),
        1
    );
    assert_eq!(rig.deck.mixer.held(old_slot), Some(source.as_str()));
    assert_eq!(events(&mut receiver).iter().filter(|event| matches!(event,
        QueueEvent::CurrentTrackAdvance { id: Some(id), reason: AdvanceReason::UserSelect } if *id == rig.ids[0]
    )).count(), 1);
    rig.deck.opens.drain();
    let mut releases = 0;
    while let Some(due) = rig.deck.opens.next_due((), 1) {
        hang_tick!();
        match due.commands() {
            [kithara_render::DispatcherCommand::Release(lane)] if *lane == old_lane => {
                releases += 1;
                due.apply(kithara_render::Dispatched::Released);
            }
            [kithara_render::DispatcherCommand::SetPriority(_, _)] => {
                due.apply(kithara_render::Dispatched::Prioritized);
            }
            _ => panic!("replacement releases the old A lane and retains the new A"),
        }
        hang_reset!();
    }
    assert_eq!(releases, 1);
}

#[kithara::test(tokio)]
#[kithara::hang_watchdog]
async fn a_held_applied_replacement_commits_its_active_owner_before_a_superseding_target() {
    let config = DeckMixerConfig::builder()
        .slots(std::num::NonZeroUsize::new(2).expect("two mixer slots"))
        .build();
    let mut rig =
        HandoverRig::playing_with_config(["item-1.wav", "item-2.wav", "item-3.wav"], config).await;
    let old_slot = rig.slot(0);
    let source = rig.source(0).to_owned();
    rig.arm(1).await;
    let entry = rig.entry(1, Transition::Crossfade);
    rig.settle(entry);
    assert_eq!(rig.queue.current, Some(rig.ids[1]));
    assert_eq!(rig.deck.mixer.held(old_slot), Some(source.as_str()));
    let outgoing = rig
        .queue
        .active
        .iter()
        .find(|active| active.item == rig.ids[0])
        .expect("outgoing A native owner");
    assert!(matches!(
        outgoing.track.snapshot().as_ref().status,
        PlayingStatus::Playing { .. }
    ));
    rig.queue.deck.mixer.slots[usize::from(old_slot.get())].state = SlotState::Playing;
    rig.select(0, Transition::Crossfade)
        .expect("return to A on a full mixer");
    rig.load(0, 64.295).await;
    let replacement = rig.queue.incoming_index(rig.ids[0]).expect("staged new A");
    assert!(rig.queue.active.is_replacement(replacement));
    assert_eq!(
        rig.queue.active.get(replacement).expect("new A").slot,
        old_slot
    );
    let pending = rig.queue.pending_handover.expect("new A native entry");
    assert!(pending.replacement);
    let applied = rig
        .deck
        .block(SessionFrame::new(128), 0.0)
        .expect("held native A replacement");
    let native = applied
        .iter()
        .find(|receipt| receipt.seq() == pending.seq)
        .expect("new A replacement verdict");
    assert!(matches!(native.outcome(), Outcome::Applied { .. }));
    assert_eq!(
        native
            .batch()
            .commands
            .iter()
            .filter(|part| matches!(part,
                DeckPart::Returned(Returned::Pcm { slot, pcm })
                    if *slot == old_slot && &**pcm.src() == source.as_str()
            ))
            .count(),
        1,
    );
    let mut receiver = rig.queue.control().subscribe::<QueueEvent>();
    rig.select(2, Transition::None)
        .expect("superseding C before A's verdict is delivered");
    assert_eq!(rig.queue.current, Some(rig.ids[1]));
    assert_eq!(rig.queue.target.map(|target| target.to), Some(rig.ids[2]));
    assert_eq!(
        rig.queue.pending_handover.map(|pending| pending.seq),
        Some(pending.seq)
    );
    rig.settle(applied);
    assert_eq!(rig.queue.current, Some(rig.ids[0]));
    assert_eq!(rig.queue.target.map(|target| target.to), Some(rig.ids[2]));
    assert!(rig.queue.pending_handover.is_none());
    let current = rig
        .queue
        .active_current_index()
        .expect("native A current owner");
    assert!(
        !rig.queue.active.is_replacement(current),
        "an applied replacement becomes the active native owner even after its target is superseded",
    );
    assert_eq!(
        rig.queue.active.get(current).expect("active A").slot,
        old_slot
    );
    assert_eq!(
        rig.queue
            .active
            .iter()
            .filter(|active| active.item == rig.ids[0])
            .count(),
        1,
    );
    let waiting = rig
        .queue
        .active
        .replacement_index()
        .expect("only C remains staged");
    assert_eq!(
        rig.queue.active.get(waiting).expect("staged C").item,
        rig.ids[2]
    );
    assert_eq!(
        events(&mut receiver)
            .iter()
            .filter(|event| matches!(event,
                QueueEvent::CurrentTrackAdvance { id: Some(id), reason: AdvanceReason::UserSelect }
                    if *id == rig.ids[0]
            ))
            .count(),
        1
    );
}

#[kithara::test(tokio)]
#[kithara::hang_watchdog]
async fn a_replacement_settles_returned_pcm_before_the_queue_replaces_its_owner() {
    let config = DeckMixerConfig::builder()
        .slots(std::num::NonZeroUsize::new(1).expect("one mixer slot"))
        .build();
    let mut rig =
        HandoverRig::playing_with_config(["item-1.wav", "item-2.wav", "item-3.wav"], config).await;
    let old_lane = rig.last_lane.expect("original opened lane");
    let old_slot = rig.slot(0);
    let source = rig.dir.path().join("item-1.wav");
    assert_eq!(
        rig.deck.mixer.held(old_slot),
        source.to_str(),
        "the existing executor owns the original consumer"
    );
    rig.queue.deck.mixer.slots[usize::from(old_slot.get())].state = SlotState::Playing;
    rig.select(1, Transition::Crossfade)
        .expect("replacement admitted");
    rig.load(1, 162.0).await;
    let replacement = rig
        .queue
        .incoming_index(rig.ids[1])
        .expect("staged incoming");
    assert!(rig.queue.active.is_replacement(replacement));
    let receipt = rig
        .deck
        .block(SessionFrame::new(128), 0.0)
        .expect("replacement block")
        .into_iter()
        .next()
        .expect("native replacement receipt");
    let seq = receipt.seq();
    let (outcome, mut batch) = receipt.into();
    assert_eq!(
        batch
            .commands
            .iter()
            .filter(|part| matches!(part,
                DeckPart::Returned(Returned::Pcm { slot, pcm })
                if *slot == old_slot && &**pcm.src() == source.to_str().expect("source path")
            ))
            .count(),
        1,
        "the executor returns the original consumer exactly once"
    );
    assert!(matches!(rig.settle_batch(seq, &outcome, &mut batch),
        kithara_play::Settled::Applied { seq: answered, at }
        if answered == seq && at == SessionFrame::new(128)));
    assert_eq!(rig.queue.current, Some(rig.ids[1]));
    assert_eq!(rig.queue.active.len(), 1);
    assert_eq!(
        rig.queue
            .current_track()
            .expect("new owner")
            .snapshot()
            .as_ref()
            .slot,
        Some(old_slot)
    );
    rig.deck.opens.drain();
    let mut releases = 0;
    while let Some(due) = rig.deck.opens.next_due((), 1) {
        hang_tick!();
        match due.commands() {
            [kithara_render::DispatcherCommand::Release(lane)] if *lane == old_lane => {
                releases += 1;
                due.apply(kithara_render::Dispatched::Released);
            }
            [kithara_render::DispatcherCommand::SetPriority(_, _)] => {
                due.apply(kithara_render::Dispatched::Prioritized);
            }
            _ => panic!("replacement leaves only original-lane release and priority maintenance"),
        }
        hang_reset!();
    }
    assert_eq!(
        releases, 1,
        "the previous Player releases its answered lane before eviction"
    );
}

#[kithara::test(tokio)]
async fn a_chain_enters_on_its_native_receipt_instead_of_a_player_observation() {
    let mut rig = HandoverRig::playing().await;
    rig.arm(1).await;
    rig.chain();
    let receipt = rig
        .stitch()
        .into_iter()
        .next()
        .expect("native chain receipt");
    let seq = receipt.seq();
    let (outcome, mut batch) = receipt.into();
    with_outbox(&mut rig.queue, &mut rig.deck, |queue, out| {
        let index = queue.incoming_index(rig.ids[1]).expect("chained target");
        queue
            .active
            .get_mut(index)
            .expect("incoming player")
            .track
            .settle(
                TrackReceipt::Deck {
                    seq,
                    outcome: &outcome,
                    batch: &mut batch,
                },
                out,
            )
    });
    assert!(
        matches!(rig.queue.active.get(rig.queue.incoming_index(rig.ids[1]).expect("incoming"))
        .expect("incoming player").track.snapshot().as_ref().status, PlayingStatus::Playing { since }
        if since == SessionFrame::new(128))
    );
    with_outbox(&mut rig.queue, &mut rig.deck, |queue, out| {
        Player::tick(queue, SessionFrame::new(128), out);
    });
    assert_eq!(
        rig.queue.current,
        Some(rig.ids[0]),
        "observation is not the Queue verdict"
    );
    assert!(matches!(rig.settle_batch(seq, &outcome, &mut batch),
        kithara_play::Settled::Applied { seq: answered, at }
        if answered == seq && at == SessionFrame::new(128)));
    assert_eq!(rig.queue.current, Some(rig.ids[1]));
}

#[kithara::test(tokio)]
async fn cancelling_a_parked_chain_answers_it_before_the_predecessors_eof() {
    let mut rig = HandoverRig::playing().await;
    rig.arm(1).await;
    rig.queue.insert_entry(
        rig.ids[2],
        TrackSource::Uri(
            rig.dir
                .path()
                .join("item-3.wav")
                .to_str()
                .expect("third path")
                .to_owned(),
        ),
        Position::ZERO,
        crate::queue::types::Placement::Append,
    );
    rig.arm(2).await;
    rig.chain();
    let cancelled = rig
        .queue
        .pending_handover
        .filter(|pending| pending.item == rig.ids[1])
        .expect("parked native chain")
        .seq;
    rig.select(2, Transition::None)
        .expect("manual selection cancels the chain");
    let receipts = rig
        .deck
        .block(SessionFrame::new(128), 0.0)
        .expect("cancellation block");
    assert!(
        receipts.iter().any(|receipt| receipt.seq() == cancelled
            && matches!(receipt.outcome(), Outcome::Rejected(Rejection::Stale))),
        "a native basis shift answers Chain without predecessor EOF"
    );
    rig.queue.clock = Some((SessionFrame::new(128), FrameCount::new(128)));
    rig.settle(receipts);
    let entry = rig
        .deck
        .block(SessionFrame::new(256), 0.0)
        .expect("manual entry within one block");
    rig.settle(entry);
    assert_eq!(rig.queue.current, Some(rig.ids[2]));
}

#[kithara::test(tokio)]
#[case::native_chain(true)]
#[case::timed_fade(false)]
async fn a_changed_projected_end_only_withdraws_a_timed_handover(#[case] chained: bool) {
    let mut rig = HandoverRig::playing().await;
    rig.arm(1).await;
    rig.apply(QueueCommand::ConfigureQueue(
        QueueSettingsChange::Gapless(chained),
        When::Next,
    ))
    .expect("handover policy");
    with_outbox(&mut rig.queue, &mut rig.deck, |queue, out| {
        queue.next_target(
            if chained {
                Transition::None
            } else {
                Transition::Crossfade
            },
            AdvanceReason::NaturalEof,
            true,
            true,
            Some(&mock::output(None).get()),
            out,
        )
    })
    .expect("automatic handover");
    crate::mock::settle(&mut rig.queue, &mut rig.deck, SessionFrame::new(0));
    let pending = rig
        .queue
        .pending_handover
        .expect("submitted native handover")
        .seq;
    let target = rig.queue.target.expect("waiting target");
    assert_eq!(target.chained, chained);
    let replies = rig
        .deck
        .block(SessionFrame::new(128), 0.0)
        .expect("park handover");
    assert!(replies.iter().all(|receipt| receipt.seq() != pending));
    rig.settle(replies);

    rig.outgoing_observation(0);
    let projected = rig
        .queue
        .auto_bound(target.settings)
        .expect("new projected bound");
    assert_ne!(
        projected, target.bound,
        "the owner observation changes the deadline"
    );
    with_outbox(&mut rig.queue, &mut rig.deck, |queue, out| {
        queue.tick_deadlines(SessionFrame::new(256), Some(&mock::output(None).get()), out)
    })
    .expect("deadline update");
    let replies = rig
        .deck
        .block(SessionFrame::new(256), 0.0)
        .expect("deadline block");
    assert_eq!(rig.queue.current, Some(rig.ids[0]));
    if chained {
        assert_eq!(rig.queue.target.expect("native target").stale, None);
        assert_eq!(rig.queue.pending_handover.expect("same Chain").seq, pending);
        assert!(replies.iter().all(|receipt| receipt.seq() != pending));
        rig.settle(replies);
        let eof = SessionFrame::new(512);
        let replies = rig.deck.end(rig.slot(0), eof).expect("native physical end");
        let answer = replies
            .iter()
            .find(|receipt| receipt.seq() == pending)
            .expect("original Chain answers at EOF");
        assert!(matches!(answer.outcome(), Outcome::Applied { at, .. } if *at == eof));
        assert!(
            matches!(answer.batch().commands.as_slice(), [DeckPart::Chain { from, to }]
            if *from == rig.slot(0) && *to == rig.slot(1))
        );
        rig.settle(replies);
        assert_eq!(rig.queue.current, Some(rig.ids[1]));
    } else {
        assert_eq!(rig.queue.target.expect("timed target").stale, Some(pending));
        assert!(
            replies.iter().any(|receipt| receipt.seq() == pending
                && matches!(receipt.outcome(), Outcome::Rejected(Rejection::Stale))),
            "the old timed entrance is cancelled before physical EOF"
        );
        assert!(
            replies
                .iter()
                .any(|receipt| receipt.batch().commands.iter().any(
                    |part| matches!(part, DeckPart::Returned(Returned::Stopped { slot, .. })
                    if *slot == rig.slot(1))
                )),
            "withdrawal submits a real Stop for B"
        );
        rig.settle(replies);
        assert_ne!(
            rig.queue.pending_handover.map(|pending| pending.seq),
            Some(pending)
        );
        assert_eq!(rig.queue.target.expect("retimed target").bound, projected);
        assert_eq!(rig.queue.current, Some(rig.ids[0]));
    }
}

#[kithara::hang_watchdog]
fn events(receiver: &mut kithara_events::EventReceiver<QueueEvent>) -> Vec<QueueEvent> {
    let mut events = Vec::new();
    while let Ok(envelope) = receiver.try_recv() {
        hang_tick!();
        events.push(envelope.event);
        hang_reset!();
    }
    events
}

#[kithara::test(tokio)]
async fn arm_next_loads_item_and_returns_src() {
    let mut rig = HandoverRig::playing().await;
    let slot = rig.arm(1).await;
    assert_eq!(rig.armed(), Some(rig.ids[1]));
    assert_eq!(
        rig.source(1),
        rig.dir.path().join("item-2.wav").to_str().expect("source")
    );
    assert_eq!(rig.queue.active.get(1).expect("armed track").slot, slot);
    assert_eq!(
        rig.queue.track(rig.ids[1]).expect("record").status,
        TrackStatus::Loaded
    );
}

#[kithara::test(tokio)]
async fn a_rejected_retained_chain_does_not_rewrite_the_new_selection() {
    let mut rig = HandoverRig::playing().await;
    rig.arm(1).await;
    rig.chain();
    let seq = rig
        .queue
        .pending_handover
        .filter(|pending| pending.item == rig.ids[1])
        .expect("accepted chained entry")
        .seq;
    rig.select(2, Transition::None).expect("new selection");
    let target = rig.queue.target.expect("new selection remains pending");
    let index = rig
        .queue
        .incoming_index(rig.ids[1])
        .expect("retained chain");
    with_outbox(&mut rig.queue, &mut rig.deck, |queue, out| {
        queue.handover_settled(
            Some(index),
            &kithara_play::Settled::Rejected {
                seq,
                reason: Rejection::Stale,
            },
            out,
        )
    })
    .expect("retained rejection settles independently");
    let remaining = rig.queue.target.expect("new selection survives");
    assert_eq!(remaining.to, target.to);
    assert_eq!(remaining.bound, target.bound);
    assert_eq!(remaining.continued, target.continued);
    assert_eq!(remaining.stale, target.stale);
}

#[kithara::test(tokio)]
async fn arm_next_idempotent_for_same_index() {
    let mut rig = HandoverRig::playing().await;
    let first = rig.arm(1).await;
    let before = rig.source(1).to_owned();
    let count = rig.queue.active.len();
    with_outbox(&mut rig.queue, &mut rig.deck, |queue, out| {
        queue.load_track(
            rig.ids[1],
            Role::Preloaded,
            Some(&mock::output(None).get()),
            out,
        )
    })
    .expect("repeat preload");
    assert_eq!(rig.slot(1), first);
    assert_eq!(rig.source(1), before);
    assert_eq!(rig.armed(), Some(rig.ids[1]));
    assert_eq!(rig.queue.active.len(), count);
    rig.deck.opens.drain();
    assert!(
        rig.deck.opens.next_due((), 1).is_none(),
        "idempotent preload does not reopen the source"
    );
}

#[kithara::test]
fn arm_next_returns_none_for_empty_slot() {
    let (mut queue, mut deck, _dir) = empty_queue();
    let result = with_outbox(&mut queue, &mut deck, |queue, out| {
        queue.next_target(
            Transition::None,
            AdvanceReason::NaturalEof,
            true,
            true,
            Some(&mock::output(None).get()),
            out,
        )
    })
    .expect("empty queue has no successor");
    assert!(result.is_none());
    assert!(queue.target.is_none());
    assert_eq!(queue.active.len(), 0);
}

#[kithara::test(tokio)]
async fn arm_next_replaces_previously_armed_slot() {
    let mut rig = HandoverRig::playing().await;
    rig.arm(1).await;
    let before = rig.source(1).to_owned();
    rig.select(2, Transition::None)
        .expect("replace preload through selection");
    rig.load(2, 162.0).await;
    assert_ne!(before, rig.source(2));
    assert_eq!(rig.armed(), Some(rig.ids[2]));
}

#[kithara::test(tokio)]
async fn commit_next_advances_index_and_publishes_event() {
    let mut rig = HandoverRig::playing().await;
    rig.arm(1).await;
    let mut receiver = rig.queue.control().subscribe();
    let receipts = rig.entry(1, Transition::None);
    rig.settle(receipts);
    assert_eq!(rig.queue.control().current_index(), Some(1));
    assert_eq!(rig.armed(), None);
    let first_eight: Vec<_> = (0..8).filter_map(|_| receiver.try_recv().ok()).collect();
    assert!(first_eight.iter().any(|envelope| {
        matches!(envelope.event, QueueEvent::CurrentTrackChanged { id: Some(id) } if id == rig.ids[1])
    }), "the current-item announcement is among the first eight events");
}

#[kithara::test(tokio)]
async fn commit_next_index_mismatch_returns_typed_error() {
    let mut rig = HandoverRig::playing().await;
    rig.arm(1).await;
    let unknown = TrackId::allocate();
    let error = with_outbox(&mut rig.queue, &mut rig.deck, |queue, out| {
        queue.apply_command(
            QueueCommand::Select {
                id: unknown,
                transition: Transition::Crossfade,
            },
            Some(&mock::output(None).get()),
            out,
        )
    })
    .expect_err("selection requires a known identity");
    assert!(matches!(error, QueueError::UnknownTrackId(requested) if requested == unknown));
    assert_eq!(rig.armed(), Some(rig.ids[1]));
    assert_eq!(rig.queue.current, Some(rig.ids[0]));
}

#[kithara::test]
fn commit_next_without_arm_returns_not_ready() {
    let (mut queue, mut deck, _dir) = empty_queue();
    let error = with_outbox(&mut queue, &mut deck, |queue, out| {
        queue.send_transition(0, SessionFrame::new(128), out)
    })
    .expect_err("there is no prepared transition");
    assert!(matches!(error, PlayError::NotReady));
}

#[kithara::test(tokio)]
async fn commit_next_publishes_snapshot_before_current_item_changed() {
    let mut rig = HandoverRig::playing().await;
    rig.arm(1).await;
    let mut receiver = rig.queue.control().subscribe();
    let receipts = rig.entry(1, Transition::Crossfade);
    assert_eq!(
        rig.queue.current,
        Some(rig.ids[0]),
        "sending does not publish a new leader"
    );
    assert!(
        events(&mut receiver)
            .iter()
            .all(|event| !matches!(event, QueueEvent::CurrentTrackChanged { .. }))
    );
    rig.settle(receipts);
    assert!(matches!(
        receiver.try_recv().expect("crossfade start").event,
        QueueEvent::CrossfadeStarted { .. }
    ));
    assert_eq!(rig.queue.control().duration_seconds(), Some(162.0));
    assert!(matches!(receiver.try_recv().expect("current change").event,
        QueueEvent::CurrentTrackChanged { id: Some(id) } if id == rig.ids[1]));
    assert!(matches!(receiver.try_recv().expect("advance reason").event,
        QueueEvent::CurrentTrackAdvance { id: Some(id), reason: AdvanceReason::UserSelect } if id == rig.ids[1]));
    assert_eq!(rig.queue.control().duration_seconds(), Some(162.0));
}

#[kithara::test(tokio)]
async fn committed_item_outlives_a_block_the_outgoing_item_was_rendering() {
    let mut rig = HandoverRig::playing().await;
    rig.arm(1).await;
    let receipts = rig.entry(1, Transition::Crossfade);
    rig.settle(receipts);
    rig.outgoing_observation(0);
    assert_eq!(rig.queue.control().duration_seconds(), Some(162.0));
    assert_eq!(rig.queue.control().position_seconds(), Some(0.0));
}

#[kithara::test(tokio)]
async fn a_rejected_fade_in_leaves_the_playhead_on_the_playing_item() {
    let mut rig = HandoverRig::playing().await;
    rig.arm(1).await;
    rig.outgoing_observation(0);
    rig.fill_to_room(0);
    let error = rig
        .select(1, Transition::Crossfade)
        .expect_err("the full deck refuses FadeIn");
    assert!(matches!(error, PlayError::Full("deck")));
    assert_eq!(rig.queue.control().duration_seconds(), Some(64.295));
    assert_eq!(rig.queue.control().position_seconds(), Some(62.3));
}

#[kithara::test(tokio)]
async fn pending_next_state_maps_activated_bool() {
    let mut rig = HandoverRig::playing().await;
    rig.arm(1).await;
    assert!(!rig.queue.current.is_some_and(|id| id == rig.ids[1]));
    let receipts = rig.entry(1, Transition::None);
    assert!(!rig.queue.current.is_some_and(|id| id == rig.ids[1]));
    rig.settle(receipts);
    assert!(rig.queue.current.is_some_and(|id| id == rig.ids[1]));
    assert_eq!(
        rig.queue
            .active
            .iter()
            .find(|active| active.item == rig.ids[1])
            .expect("activated item")
            .role,
        Role::Current
    );
}

#[kithara::test(tokio)]
async fn select_item_on_armed_index_promotes_armed_slot() {
    let mut rig = HandoverRig::playing().await;
    let armed = rig.arm(1).await;
    let source = rig.source(1).to_owned();
    let receipts = rig.entry(1, Transition::None);
    assert!(receipts.iter().all(
        |receipt| !receipt.batch().commands.iter().any(|part| matches!(
            part,
            DeckPart::Detach { .. } | DeckPart::Attach { .. } | DeckPart::Replace { .. }
        ))
    ));
    rig.settle(receipts);
    assert_eq!(rig.queue.control().current_index(), Some(1));
    assert_eq!(rig.armed(), None);
    assert_eq!(rig.slot(1), armed);
    assert_eq!(rig.source(1), source);
    rig.assert_no_pending_load();
}

#[kithara::test(tokio)]
#[should_panic(expected = "promotion reuses the opened lane")]
async fn the_promotion_reuse_check_rejects_an_additional_load() {
    let mut rig = HandoverRig::playing().await;
    rig.arm(1).await;
    rig.select(2, Transition::None)
        .expect("another selection loads");
    rig.assert_no_pending_load();
}

#[kithara::test(tokio)]
async fn unarm_next_clears_when_not_activated_and_unloads() {
    let mut rig = HandoverRig::playing().await;
    rig.arm(1).await;
    let source = rig.source(1).to_owned();
    with_outbox(&mut rig.queue, &mut rig.deck, Queue::cancel_auto).expect("unarm preload");
    let parts = rig.pending_parts();
    assert_eq!(rig.armed(), None);
    assert_eq!(
        source,
        rig.dir.path().join("item-2.wav").to_str().expect("source")
    );
    assert!(
        parts
            .iter()
            .any(|part| matches!(part, DeckPart::Detach { .. }))
    );
}

#[kithara::test(tokio)]
async fn select_item_clears_pending_next_and_unloads_preloaded_track() {
    let mut rig = HandoverRig::playing().await;
    let armed_slot = rig.arm(1).await;
    let source = rig.source(1).to_owned();
    rig.select(2, Transition::None)
        .expect("select another item");
    let parts = rig.pending_parts();
    assert_eq!(detached(&parts), [armed_slot]);
    rig.load(2, 162.0).await;
    crate::mock::finish(&mut rig.queue, &mut rig.deck);
    assert_eq!(rig.armed(), None);
    assert_eq!(rig.queue.control().current_index(), Some(2));
    assert_eq!(
        source,
        rig.dir.path().join("item-2.wav").to_str().expect("source")
    );
}

#[kithara::test(tokio)]
async fn selecting_another_item_cancels_the_successor_preload_instead_of_unloading_it() {
    let mut rig = HandoverRig::playing().await;
    let second_slot = rig.arm(1).await;
    rig.chain();
    let stitched = rig.stitch();
    rig.select(2, Transition::None).expect("selection accepted");
    let commands = rig.pending_parts();
    assert_eq!(detached(&commands), []);
    assert!(
        commands
            .iter()
            .any(|part| matches!(part, DeckPart::Stop { slot, .. } if *slot == second_slot))
    );
    rig.settle(stitched);
    rig.load(2, 162.0).await;
    crate::mock::finish(&mut rig.queue, &mut rig.deck);
    assert_eq!(
        rig.queue.control().current_index(),
        Some(2),
        "the applied selection leads over the successor it fades out"
    );
}

#[kithara::test(tokio)]
#[kithara::hang_watchdog]
async fn a_refused_selection_keeps_a_successor_already_stitched_in_and_reports_it() {
    let mut rig = HandoverRig::playing().await;
    rig.arm(1).await;
    rig.chain();
    let stitched = rig.stitch();
    for _ in 0..rig.deck.dispatcher.available() {
        hang_tick!();
        rig.deck
            .dispatcher
            .send(
                When::Next,
                Batch {
                    basis: Vec::new(),
                    commands: vec![kithara_render::DispatcherCommand::SetPriority(
                        rig.last_lane.expect("opened successor lane"),
                        kithara_play::ServiceClass::Warm,
                    )],
                },
            )
            .expect("fill dispatcher ring without consuming deck capacity");
        hang_reset!();
    }
    assert_eq!(rig.deck.dispatcher.available(), 0);
    assert!(
        matches!(
            rig.select(2, Transition::None),
            Err(PlayError::Full("dispatcher"))
        ),
        "the selection's load is refused"
    );
    assert_eq!(detached(&rig.pending_parts()), []);
    rig.deck.opens.drain();
    while let Some(mut due) = rig.deck.opens.next_due((), 1) {
        hang_tick!();
        assert!(
            !due.commands_mut()
                .iter()
                .any(|command| matches!(command, kithara_render::DispatcherCommand::Load(_))),
            "the refused selection sends no load"
        );
        due.apply(kithara_render::Dispatched::Prioritized);
        hang_reset!();
    }
    rig.settle(stitched);
    assert_eq!(rig.queue.control().current_index(), Some(1));
}

#[kithara::test(tokio)]
async fn re_arming_keeps_a_successor_already_stitched_in_and_reports_it() {
    let mut rig = HandoverRig::playing().await;
    rig.arm(1).await;
    rig.chain();
    let stitched = rig.stitch();
    rig.select(2, Transition::None).expect("re-arm accepted");
    assert_eq!(detached(&rig.pending_parts()), []);
    rig.settle(stitched);
    assert_eq!(rig.queue.control().current_index(), Some(1));
    assert_eq!(rig.armed(), Some(rig.ids[2]));
}

#[kithara::test(tokio)]
async fn a_re_armed_successor_the_audio_thread_stitched_in_is_promoted() {
    let mut rig = HandoverRig::playing().await;
    rig.arm(1).await;
    rig.chain();
    rig.select(2, Transition::None).expect("re-arm accepted");
    rig.load(2, 162.0).await;
    let applied = rig
        .deck
        .block(SessionFrame::new(128), 0.0)
        .expect("third entry");
    rig.event(DeckEvent::Ended {
        slot: rig.slot(0),
        at: SessionFrame::new(128),
    });
    rig.settle(applied);
    assert_eq!(rig.queue.control().current_index(), Some(2));
    assert_eq!(rig.armed(), None);
}

#[kithara::test(tokio)]
#[kithara::hang_watchdog]
async fn a_chained_successor_that_ends_in_its_stitch_block_is_reported() {
    let mut rig = HandoverRig::playing().await;
    let second = rig.arm(1).await;
    rig.chain();
    let applied = rig.stitch();
    rig.event(DeckEvent::Ended {
        slot: rig.slot(0),
        at: SessionFrame::new(128),
    });
    rig.event(DeckEvent::Ended {
        slot: second,
        at: SessionFrame::new(128),
    });
    rig.settle(applied);
    assert_eq!(rig.queue.control().current_index(), Some(1));
    assert_eq!(rig.armed(), None);
}

#[kithara::test(tokio)]
async fn a_withdrawn_successor_that_ends_in_its_stitch_block_is_reported() {
    let mut rig = HandoverRig::playing().await;
    let second = rig.arm(1).await;
    rig.chain();
    let applied = rig.stitch();
    rig.fill_to_room(1);
    assert!(rig.select(2, Transition::None).is_err());
    rig.event(DeckEvent::Ended {
        slot: rig.slot(0),
        at: SessionFrame::new(128),
    });
    rig.event(DeckEvent::Ended {
        slot: second,
        at: SessionFrame::new(128),
    });
    rig.settle(applied);
    assert_eq!(rig.queue.control().current_index(), Some(1));
}

#[kithara::test(tokio)]
async fn a_successor_whose_cancel_the_ring_refused_is_reported_when_stitched_in_later() {
    let mut rig = HandoverRig::playing().await;
    rig.arm(1).await;
    rig.chain();
    rig.fill_to_room(0);
    assert!(with_outbox(&mut rig.queue, &mut rig.deck, Queue::withdraw_transition).is_err());
    let commands = rig.pending_parts();
    assert!(
        !commands
            .iter()
            .any(|part| matches!(part, DeckPart::Stop { .. })),
        "the ring refused cancellation"
    );
    let applied = rig.stitch();
    rig.select(2, Transition::None).expect("select third");
    rig.load(2, 162.0).await;
    crate::mock::finish(&mut rig.queue, &mut rig.deck);
    let third = rig.slot(2);
    rig.event(DeckEvent::Ended {
        slot: third,
        at: SessionFrame::new(256),
    });
    rig.settle(applied);
    assert_eq!(rig.queue.control().current_index(), Some(1));
}

#[kithara::test(tokio)]
async fn a_re_armed_successor_that_fails_in_its_stitch_block_is_reported() {
    let mut rig = HandoverRig::playing().await;
    rig.arm(1).await;
    rig.chain();
    rig.select(2, Transition::None).expect("re-arm third");
    rig.load(2, 162.0).await;
    let applied = rig
        .deck
        .block(SessionFrame::new(128), 0.0)
        .expect("third stitch block");
    rig.event(DeckEvent::Ended {
        slot: rig.slot(0),
        at: SessionFrame::new(128),
    });
    rig.event(DeckEvent::Failed {
        slot: rig.slot(2),
        at: SessionFrame::new(128),
        fault: PlaybackFault::OutputRangeUnavailable,
    });
    rig.settle(applied);
    assert_eq!(rig.queue.control().current_index(), Some(2));
    assert_eq!(rig.armed(), None);
}

#[kithara::test(tokio)]
async fn an_unloaded_successor_whose_cancel_the_ring_refused_leaves_the_question() {
    let mut rig = HandoverRig::playing().await;
    rig.arm(1).await;
    rig.chain();
    rig.fill_to_room(0);
    assert!(with_outbox(&mut rig.queue, &mut rig.deck, Queue::withdraw_transition).is_err());
    rig.pending_parts();
    rig.select(2, Transition::None).expect("re-arm third");
    rig.load(2, 162.0).await;
    let applied = rig
        .deck
        .block(SessionFrame::new(128), 0.0)
        .expect("applied detach and third entry");
    let outgoing = rig.slot(0);
    rig.settle(applied);
    rig.event(DeckEvent::Ended {
        slot: outgoing,
        at: SessionFrame::new(128),
    });
    assert_eq!(rig.queue.control().current_index(), Some(2));
    assert_eq!(rig.armed(), None);
    assert!(
        rig.queue
            .active
            .iter()
            .all(|active| active.item != rig.ids[1])
    );
}

#[kithara::test(tokio)]
async fn eof_playback_stopped_notification_maps_to_item_end_event() {
    let mut rig = HandoverRig::playing().await;
    rig.only_current(0);
    let mut receiver = rig.queue.control().subscribe();
    rig.event(DeckEvent::Ended {
        slot: rig.slot(0),
        at: SessionFrame::new(128),
    });
    assert!(rig.queue.control().current().is_none());
    let observed = events(&mut receiver);
    assert_eq!(observed.len(), 1);
    assert!(matches!(observed[0], QueueEvent::QueueEnded));
    with_outbox(&mut rig.queue, &mut rig.deck, |queue, out| {
        Player::tick(queue, SessionFrame::new(128), out);
    });
    assert!(rig.queue.control().current().is_none());
    assert!(events(&mut receiver).is_empty());
}

#[kithara::test(tokio)]
async fn an_ended_current_keeps_its_successor_uncommitted_until_the_native_receipt() {
    let mut rig = HandoverRig::playing().await;
    rig.arm(1).await;
    rig.chain();
    let pending = rig.queue.pending_handover.expect("pending native Chain");
    let mut receiver = rig.queue.control().subscribe::<QueueEvent>();
    rig.event(DeckEvent::Ended {
        slot: rig.slot(0),
        at: SessionFrame::new(128),
    });
    assert_eq!(
        rig.queue.control().current().map(|entry| entry.id),
        Some(rig.ids[0])
    );
    assert_eq!(
        rig.queue.pending_handover.map(|pending| pending.seq),
        Some(pending.seq)
    );
    assert_eq!(
        rig.queue.track(rig.ids[1]).map(|entry| entry.status),
        Some(TrackStatus::Loaded)
    );
    assert!(events(&mut receiver).iter().all(|event| !matches!(
        event,
        QueueEvent::QueueEnded
            | QueueEvent::CurrentTrackChanged { .. }
            | QueueEvent::CurrentTrackAdvance { .. }
    )));
    let receipts = rig.stitch();
    assert!(receipts.iter().any(|receipt| receipt.seq() == pending.seq
        && matches!(receipt.outcome(), Outcome::Applied { .. })));
    rig.settle(receipts);
    assert_eq!(
        rig.queue.control().current().map(|entry| entry.id),
        Some(rig.ids[1])
    );
    let observed = events(&mut receiver);
    assert_eq!(observed.iter().filter(|event| matches!(
        event,
        QueueEvent::CurrentTrackAdvance { id: Some(id), reason: AdvanceReason::NaturalEof } if *id == rig.ids[1]
    )).count(), 1);
    rig.apply(QueueCommand::Previous(Transition::None))
        .expect("native handover committed A to history");
    assert_eq!(rig.queue.target.map(|target| target.to), Some(rig.ids[0]));
}

#[kithara::test(tokio)]
async fn playback_stopped_notification_does_not_map_to_item_end_event() {
    let mut rig = HandoverRig::playing().await;
    rig.only_current(0);
    let mut receiver = rig.queue.control().subscribe();
    rig.apply(QueueCommand::Pause { at: When::Next })
        .expect("ordinary stop");
    let stopped = rig
        .deck
        .block(SessionFrame::new(128), 0.0)
        .expect("stop block");
    rig.settle(stopped);
    assert!(
        events(&mut receiver).is_empty(),
        "ordinary stop is not a natural end"
    );
}

#[kithara::test(tokio)]
async fn end_of_the_held_slot_is_the_leading_track() {
    let mut rig = HandoverRig::playing().await;
    rig.only_current(0);
    let held = rig.slot(0);
    let current = rig.queue.current_track().expect("leader").snapshot();
    assert_eq!(
        (current.as_ref().item, current.as_ref().slot),
        (rig.ids[0], Some(held))
    );
    assert_eq!(
        rig.source(0),
        rig.dir.path().join("item-1.wav").to_str().expect("source")
    );
    assert_eq!(
        rig.queue.active.get(0).expect("held track").role,
        Role::Current
    );
    let mut receiver = rig.queue.control().subscribe();
    rig.event(DeckEvent::Ended {
        slot: held,
        at: SessionFrame::new(128),
    });
    let observed = events(&mut receiver);
    assert_eq!(observed.len(), 1);
    assert!(matches!(observed[0], QueueEvent::QueueEnded));
}

#[kithara::test(tokio)]
async fn end_of_a_slot_the_phase_does_not_hold_is_a_background_track() {
    let mut rig = HandoverRig::playing().await;
    let background = rig.arm(1).await;
    let active = rig
        .queue
        .active
        .iter()
        .find(|active| active.item == rig.ids[1])
        .expect("background track");
    assert_eq!(
        (active.item, active.slot, active.role),
        (rig.ids[1], background, Role::Preloaded)
    );
    assert_ne!(background, rig.slot(0));
    assert_eq!(
        rig.source(1),
        rig.dir.path().join("item-2.wav").to_str().expect("source")
    );
    let mut receiver = rig.queue.control().subscribe();
    rig.event(DeckEvent::Ended {
        slot: background,
        at: SessionFrame::new(128),
    });
    assert_eq!(rig.queue.current, Some(rig.ids[0]));
    assert!(events(&mut receiver).is_empty());
}

#[kithara::test(tokio)]
async fn the_outgoing_half_of_a_crossfade_is_not_the_leading_track() {
    let mut rig = HandoverRig::playing_with_sources(["item-1.wav"; 3]).await;
    let same = rig.source(0).to_owned();
    rig.queue.tracks.records_mut()[1].source = TrackSource::Uri(same.clone());
    rig.arm(1).await;
    let receipts = rig.entry(1, Transition::Crossfade);
    rig.settle(receipts);
    let outgoing = rig.slot(0);
    let active = rig
        .queue
        .active
        .iter()
        .find(|active| active.item == rig.ids[0])
        .expect("outgoing track");
    assert_eq!(
        (active.item, active.slot, active.role),
        (rig.ids[0], outgoing, Role::Outgoing)
    );
    assert_eq!(rig.source(0), same);
    assert_eq!(rig.source(1), same);
    let mut receiver = rig.queue.control().subscribe();
    rig.event(DeckEvent::Ended {
        slot: outgoing,
        at: SessionFrame::new(128),
    });
    assert_eq!(rig.queue.current, Some(rig.ids[1]));
    assert!(
        events(&mut receiver).is_empty(),
        "an outgoing end never advances a second time"
    );
}

#[kithara::test(tokio)]
async fn the_promoted_half_of_a_crossfade_is_the_leading_track() {
    let mut rig = HandoverRig::playing_with_sources(["item-1.wav"; 3]).await;
    let same = rig.source(0).to_owned();
    rig.queue.tracks.records_mut()[1].source = TrackSource::Uri(same.clone());
    rig.arm(1).await;
    let receipts = rig.entry(1, Transition::Crossfade);
    rig.settle(receipts);
    let promoted = rig.slot(1);
    let active = rig
        .queue
        .active
        .iter()
        .find(|active| active.item == rig.ids[1])
        .expect("promoted track");
    assert_eq!(
        (active.item, active.slot, active.role),
        (rig.ids[1], promoted, Role::Current)
    );
    assert_eq!(rig.source(0), same);
    assert_eq!(rig.source(1), same);
    rig.only_current(1);
    let mut receiver = rig.queue.control().subscribe();
    rig.event(DeckEvent::Ended {
        slot: promoted,
        at: SessionFrame::new(128),
    });
    let observed = events(&mut receiver);
    assert_eq!(observed.len(), 1);
    assert!(matches!(observed[0], QueueEvent::QueueEnded));
}

#[kithara::test(tokio)]
async fn start_of_a_slot_the_phase_does_not_hold_is_a_background_item() {
    let mut rig = HandoverRig::playing().await;
    let background = rig.arm(1).await;
    let mut receiver = rig.queue.control().subscribe();
    with_outbox(&mut rig.queue, &mut rig.deck, |queue, out| {
        queue
            .active
            .iter_mut()
            .find(|active| active.item == rig.ids[1])
            .expect("background item")
            .track
            .apply(
                TrackCommand::Play {
                    at: When::At(SessionFrame::new(128)),
                },
                out,
            )
    })
    .expect("background start");
    let started = rig
        .deck
        .block(SessionFrame::new(128), 0.0)
        .expect("start block");
    rig.settle(started);
    let active = rig
        .queue
        .active
        .iter()
        .find(|active| active.item == rig.ids[1])
        .expect("background item");
    assert_eq!(
        (active.item, active.slot, active.role),
        (rig.ids[1], background, Role::Preloaded)
    );
    assert!(matches!(
        active.track.snapshot().as_ref().status,
        PlayingStatus::Playing { .. }
    ));
    assert_eq!(
        rig.source(1),
        rig.dir.path().join("item-2.wav").to_str().expect("source")
    );
    assert_eq!(rig.queue.current, Some(rig.ids[0]));
    assert!(events(&mut receiver).is_empty());
}

impl HandoverRig {
    fn seed_previous_history(&mut self) {
        if self.queue.track(self.ids[2]).is_none() {
            self.queue.insert_entry(
                self.ids[2],
                TrackSource::Uri(
                    self.dir
                        .path()
                        .join("item-3.wav")
                        .to_str()
                        .expect("third path")
                        .to_owned(),
                ),
                Position::ZERO,
                crate::queue::types::Placement::Append,
            );
        }
        self.queue.navigation.commit(
            Some(self.ids[0]),
            self.ids[2],
            AdvanceReason::UserSelect,
            &self.ids,
        );
        self.queue.navigation.commit(
            Some(self.ids[2]),
            self.ids[1],
            AdvanceReason::UserSelect,
            &self.ids,
        );
        self.queue.navigation.commit(
            Some(self.ids[1]),
            self.ids[0],
            AdvanceReason::UserSelect,
            &self.ids,
        );
        assert_eq!(self.queue.current, Some(self.ids[0]));
    }

    async fn with_previous_history() -> Self {
        let config = DeckMixerConfig::builder()
            .slots(std::num::NonZeroUsize::new(3).expect("three history slots"))
            .build();
        let mut rig =
            Self::playing_with_config(["item-1.wav", "item-2.wav", "item-3.wav"], config).await;
        rig.seed_previous_history();
        rig
    }
}

#[kithara::test(tokio)]
async fn cancelling_previous_preserves_the_uncommitted_history_entry() {
    let mut rig = HandoverRig::with_previous_history().await;
    rig.apply(QueueCommand::Previous(Transition::None))
        .expect("previous request");
    assert_eq!(rig.queue.target.map(|target| target.to), Some(rig.ids[1]));
    assert_eq!(rig.queue.current, Some(rig.ids[0]));
    rig.select(0, Transition::None)
        .expect("cancel previous with current");
    assert_eq!(rig.queue.current, Some(rig.ids[0]));
    rig.apply(QueueCommand::Previous(Transition::None))
        .expect("same previous request");
    assert_eq!(
        rig.queue.target.map(|target| target.to),
        Some(rig.ids[1]),
        "cancellation cannot consume the history entry"
    );
}

#[kithara::test(tokio)]
async fn refused_previous_preserves_the_uncommitted_history_entry() {
    let mut rig = HandoverRig::with_previous_history().await;
    rig.arm(1).await;
    let seq = rig
        .apply(QueueCommand::Previous(Transition::None))
        .expect("previous request")
        .expect("native previous sequence");
    rig.deck.ring.publish().expect("publish previous");
    rig.deck.inbox.drain();
    let mut scope = rig.deck.inbox.scope(rig.deck.scope).expect("deck scope");
    let due = scope
        .next_due(SessionFrame::new(128), 1)
        .expect("native previous due");
    assert_eq!(due.seq(), seq);
    due.refuse(kithara_render::bridge::DeckRefusal::Deferral);
    let ScopedReceipt::Scope(_, receipt) = rig.deck.ring.receipt().expect("native refusal") else {
        panic!("previous receipt belongs to its deck scope");
    };
    let (outcome, mut batch) = receipt.into();
    assert!(matches!(rig.settle_batch(seq, &outcome, &mut batch),
        kithara_play::Settled::Rejected { seq: answered, .. } if answered == seq));
    assert_eq!(rig.queue.current, Some(rig.ids[0]));
    rig.apply(QueueCommand::Previous(Transition::None))
        .expect("same previous after refusal");
    assert_eq!(
        rig.queue.target.map(|target| target.to),
        Some(rig.ids[1]),
        "a native rejection cannot consume the history entry"
    );
}

#[kithara::test(tokio)]
async fn applied_previous_commits_backwards_once_without_bouncing() {
    let mut rig = HandoverRig::with_previous_history().await;
    rig.arm(1).await;
    rig.arm(2).await;
    let mut observed = rig.queue.control().subscribe::<QueueEvent>();
    for index in [1, 2] {
        let before = rig.queue.current;
        rig.apply(QueueCommand::Previous(Transition::None))
            .expect("previous request");
        assert_eq!(
            rig.queue.current, before,
            "submission is not the cursor commit"
        );
        assert_eq!(
            rig.queue.target.map(|target| target.to),
            Some(rig.ids[index])
        );
        if index == 2 {
            rig.load(index, 162.0).await;
        }
        let receipts = rig
            .deck
            .block(SessionFrame::new(128), 0.0)
            .expect("native previous");
        assert!(
            receipts
                .iter()
                .any(|receipt| matches!(receipt.outcome(), Outcome::Applied { .. }))
        );
        rig.settle(receipts);
        assert_eq!(rig.queue.current, Some(rig.ids[index]));
    }
    let changed = events(&mut observed)
        .into_iter()
        .filter_map(|event| match event {
            QueueEvent::CurrentTrackChanged { id } => id,
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        changed,
        vec![rig.ids[1], rig.ids[2]],
        "one cursor commit per native Previous"
    );
}

#[kithara::test(tokio)]
async fn a_late_applied_previous_commits_its_original_history_before_the_new_target() {
    let mut rig = HandoverRig::with_previous_history().await;
    rig.arm(1).await;
    rig.arm(2).await;
    let mut observed = rig.queue.control().subscribe::<QueueEvent>();
    let seq = rig
        .apply(QueueCommand::Previous(Transition::None))
        .expect("previous request")
        .expect("native previous sequence");
    let applied = rig
        .deck
        .block(SessionFrame::new(128), 0.0)
        .expect("held native Previous");
    assert!(applied.iter().any(
        |receipt| receipt.seq() == seq && matches!(receipt.outcome(), Outcome::Applied { .. })
    ));
    rig.select(2, Transition::None)
        .expect("new target before previous answer");
    assert_eq!(rig.queue.current, Some(rig.ids[0]));
    rig.settle(applied);
    assert_eq!(
        rig.queue.current,
        Some(rig.ids[1]),
        "the original native verdict owns its cursor commit"
    );
    assert_eq!(rig.queue.target.map(|target| target.to), Some(rig.ids[2]));
    let incoming = rig.queue.incoming_index(rig.ids[2]).expect("staged C");
    assert!(rig.queue.active.is_replacement(incoming));
    let slot = rig.queue.active.get(incoming).expect("incoming C").slot;
    assert_eq!(
        rig.deck.mixer.held(slot),
        rig.dir.path().join("item-1.wav").to_str(),
        "the replacement victim still owns its actual PCM"
    );
    rig.queue.deck.mixer.slots[usize::from(slot.get())].state = SlotState::Playing;
    rig.load(2, 162.0).await;
    let followup = rig
        .deck
        .block(SessionFrame::new(128), 0.0)
        .expect("new target block");
    rig.settle(followup);
    let incoming = rig.queue.incoming_index(rig.ids[2]);
    let facts = incoming.and_then(|index| {
        rig.queue.active.get(index).map(|active| {
            (
                active.slot,
                rig.queue.active.is_replacement(index),
                active.load,
                active.track.snapshot().as_ref().status,
                rig.deck.mixer.held(active.slot),
                rig.queue.deck.mixer.slots[usize::from(active.slot.get())].state,
            )
        })
    });
    assert_eq!(
        rig.queue.current,
        Some(rig.ids[2]),
        "target={:?}, pending={:?}, incoming={facts:?}",
        rig.queue.target,
        rig.queue.pending_handover
    );
    let changed = events(&mut observed)
        .into_iter()
        .filter_map(|event| match event {
            QueueEvent::CurrentTrackChanged { id } => id,
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        changed,
        vec![rig.ids[1], rig.ids[2]],
        "original Previous then new target each commit once"
    );
    rig.apply(QueueCommand::Previous(Transition::None))
        .expect("history after the new target");
    assert_eq!(rig.queue.target.map(|target| target.to), Some(rig.ids[1]));
}

#[kithara::test(tokio)]
async fn a_current_reselect_restores_its_resource_after_a_held_applied_previous() {
    let mut rig = HandoverRig::with_previous_history().await;
    let original_slot = rig.slot(0);
    let original_source = rig.source(0).to_owned();
    rig.arm(1).await;
    rig.arm(2).await;
    let mut observed = rig.queue.control().subscribe::<QueueEvent>();
    let original = rig
        .apply(QueueCommand::Previous(Transition::None))
        .expect("original previous")
        .expect("native previous sequence");
    let applied = rig
        .deck
        .block(SessionFrame::new(128), 0.0)
        .expect("held native Previous");
    assert!(
        applied.iter().any(|receipt| receipt.seq() == original
            && matches!(receipt.outcome(), Outcome::Applied { .. }))
    );
    rig.select(0, Transition::None)
        .expect("reselect original current");
    assert_eq!(rig.queue.current, Some(rig.ids[0]));
    rig.assert_no_pending_load();
    rig.settle(applied);
    assert_eq!(
        rig.queue.current,
        Some(rig.ids[1]),
        "the real original verdict commits first"
    );
    assert_eq!(
        rig.queue.target.map(|target| target.to),
        Some(rig.ids[0]),
        "reselecting current retains the requested identity across the old verdict"
    );
    let cancel = rig
        .deck
        .block(SessionFrame::new(128), 0.0)
        .expect("native cancellation and entry");
    rig.settle(cancel);
    let entry = rig
        .deck
        .block(SessionFrame::new(128), 0.0)
        .expect("native restored entry");
    rig.settle(entry);
    assert_eq!(rig.queue.current, Some(rig.ids[0]));
    let restored = rig.queue.active_current_index().expect("restored active A");
    assert_eq!(
        rig.queue.active.get(restored).expect("restored A").slot,
        original_slot
    );
    assert_eq!(
        rig.deck.mixer.held(original_slot),
        Some(original_source.as_str())
    );
    assert_eq!(
        rig.queue
            .active
            .iter()
            .filter(|active| active.item == rig.ids[0])
            .count(),
        1,
        "the original resource remains the sole A instance"
    );
    rig.assert_no_pending_load();
    let changed = events(&mut observed)
        .into_iter()
        .filter_map(|event| match event {
            QueueEvent::CurrentTrackChanged { id } => id,
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        changed,
        vec![rig.ids[1], rig.ids[0]],
        "native B then requested A commit once"
    );
    rig.apply(QueueCommand::Previous(Transition::None))
        .expect("history after restored A");
    assert_eq!(rig.queue.target.map(|target| target.to), Some(rig.ids[1]));
}

#[kithara::test(tokio)]
async fn reselecting_current_keeps_an_automatic_chain_owned_by_its_receipt() {
    let mut rig = HandoverRig::playing().await;
    rig.arm(1).await;
    rig.chain();
    let slot = rig.slot(0);
    let source = rig.source(0).to_owned();
    let pending = rig.queue.pending_handover.expect("pending automatic Chain");
    assert!(rig.queue.target.is_some_and(|target| target.auto
        && target.chained
        && target.to == rig.ids[1]
        && target.reason == AdvanceReason::NaturalEof));
    assert_eq!(pending.item, rig.ids[1]);
    assert_eq!(pending.slot, rig.slot(1));
    let mut receiver = rig.queue.control().subscribe::<QueueEvent>();
    rig.select(0, Transition::None)
        .expect("same confirmed current");
    assert_eq!(rig.queue.current, Some(rig.ids[0]));
    assert_eq!(rig.slot(0), slot);
    assert_eq!(rig.deck.mixer.held(slot), Some(source.as_str()));
    assert_eq!(
        rig.queue.pending_handover.map(|pending| pending.seq),
        Some(pending.seq)
    );
    assert!(
        rig.queue
            .target
            .is_some_and(|target| target.auto && target.chained && target.to == rig.ids[1])
    );
    assert_eq!(
        rig.queue.track(rig.ids[1]).map(|entry| entry.status),
        Some(TrackStatus::Loaded)
    );
    assert!(events(&mut receiver).iter().all(|event| !matches!(
        event,
        QueueEvent::CurrentTrackChanged { .. } | QueueEvent::CurrentTrackAdvance { .. }
    )));
    rig.assert_no_pending_load();
    let receipts = rig.stitch();
    assert!(receipts.iter().any(|receipt| receipt.seq() == pending.seq
        && matches!(receipt.outcome(), Outcome::Applied { .. })));
    rig.settle(receipts);
    assert_eq!(rig.queue.current, Some(rig.ids[1]));
    rig.apply(QueueCommand::Previous(Transition::None))
        .expect("successor commits the old current to history");
    assert_eq!(rig.queue.target.map(|target| target.to), Some(rig.ids[0]));
}

#[kithara::test(tokio)]
async fn reselecting_current_preserves_a_held_applied_automatic_chain_and_its_history() {
    let mut rig = HandoverRig::playing().await;
    let old_slot = rig.slot(0);
    let source = rig.source(0).to_owned();
    rig.arm(1).await;
    rig.chain();
    let pending = rig.queue.pending_handover.expect("pending automatic Chain");
    let stitched = rig.stitch();
    let native = stitched
        .iter()
        .find(|receipt| receipt.seq() == pending.seq)
        .expect("held native Chain");
    assert!(matches!(native.outcome(), Outcome::Applied { .. }));
    assert!(native.batch().commands.iter().any(|part| matches!(part,
        DeckPart::Chain { from, to } if *from == old_slot && *to == pending.slot
    )));
    let mut receiver = rig.queue.control().subscribe::<QueueEvent>();
    rig.select(0, Transition::None)
        .expect("same Queue-confirmed current while B's verdict is held");
    assert_eq!(rig.queue.current, Some(rig.ids[0]));
    assert_eq!(
        rig.queue.pending_handover.map(|pending| pending.seq),
        Some(pending.seq)
    );
    assert!(
        rig.queue
            .target
            .is_some_and(|target| target.auto && target.chained && target.to == rig.ids[1])
    );
    assert_eq!(
        rig.queue.track(rig.ids[1]).map(|entry| entry.status),
        Some(TrackStatus::Loaded)
    );
    assert_eq!(rig.deck.mixer.held(old_slot), Some(source.as_str()));
    rig.assert_no_pending_load();
    assert!(events(&mut receiver).iter().all(|event| !matches!(
        event,
        QueueEvent::CurrentTrackChanged { .. } | QueueEvent::CurrentTrackAdvance { .. }
    )));
    rig.settle(stitched);
    assert_eq!(rig.queue.current, Some(rig.ids[1]));
    crate::mock::finish(&mut rig.queue, &mut rig.deck);
    let observed = events(&mut receiver);
    assert_eq!(observed.iter().filter(|event| matches!(event,
        QueueEvent::CurrentTrackAdvance { id: Some(id), reason: AdvanceReason::NaturalEof } if *id == rig.ids[1]
    )).count(), 1);
    assert_eq!(observed.iter().filter(|event| matches!(event, QueueEvent::CurrentTrackChanged { id: Some(id) } if *id == rig.ids[1])).count(), 1);
    rig.apply(QueueCommand::Previous(Transition::None))
        .expect("history points to A");
    assert_eq!(rig.queue.target.map(|target| target.to), Some(rig.ids[0]));
    rig.load(0, 64.295).await;
    let applied = rig
        .deck
        .block(SessionFrame::new(128), 0.0)
        .expect("actual native Previous");
    let previous = rig
        .queue
        .pending_handover
        .expect("native Previous receipt owner");
    assert!(applied.iter().any(|receipt| receipt.seq() == previous.seq
        && matches!(receipt.outcome(), Outcome::Applied { .. })));
    rig.settle(applied);
    assert_eq!(rig.queue.current, Some(rig.ids[0]));
    assert_eq!(events(&mut receiver).iter().filter(|event| matches!(event,
        QueueEvent::CurrentTrackAdvance { id: Some(id), reason: AdvanceReason::UserPrev } if *id == rig.ids[0]
    )).count(), 1);
}

#[kithara::test(tokio)]
async fn reselecting_current_without_a_pending_handover_keeps_the_shortcut() {
    let mut rig = HandoverRig::with_previous_history().await;
    rig.arm(1).await;
    rig.arm(2).await;
    let slot = rig.slot(0);
    let source = rig.source(0).to_owned();
    let mut observed = rig.queue.control().subscribe::<QueueEvent>();
    rig.select(0, Transition::None).expect("same current");
    assert!(rig.queue.target.is_none());
    let applied = rig
        .deck
        .block(SessionFrame::new(128), 0.0)
        .expect("native same-current play");
    rig.settle(applied);
    assert_eq!(rig.queue.current, Some(rig.ids[0]));
    assert_eq!(rig.slot(0), slot);
    assert_eq!(rig.deck.mixer.held(slot), Some(source.as_str()));
    rig.assert_no_pending_load();
    assert!(
        events(&mut observed)
            .iter()
            .all(|event| !matches!(event, QueueEvent::CurrentTrackChanged { .. }))
    );
}

#[kithara::test(tokio)]
#[case::once(false)]
#[case::repeated(true)]
async fn a_current_reselect_keeps_pause_control_until_the_previous_answer(#[case] repeated: bool) {
    let mut rig = HandoverRig::with_previous_history().await;
    rig.arm(1).await;
    rig.arm(2).await;
    let slot = rig.slot(0);
    let source = rig.source(0).to_owned();
    let original = rig
        .apply(QueueCommand::Previous(Transition::None))
        .expect("previous request")
        .expect("native previous sequence");
    let applied = rig
        .deck
        .block(SessionFrame::new(128), 0.0)
        .expect("held native previous");
    assert!(
        applied.iter().any(|receipt| receipt.seq() == original
            && matches!(receipt.outcome(), Outcome::Applied { .. }))
    );
    rig.select(0, Transition::None).expect("reselect current A");
    assert_eq!(rig.queue.current, Some(rig.ids[0]));
    assert!(
        rig.queue.current_track().is_some(),
        "the applied current retains its owner until B answers"
    );
    let current = rig.queue.active_current_index().expect("current A role");
    assert_eq!(rig.queue.active.get(current).expect("current A").slot, slot);
    let paused = rig
        .apply(QueueCommand::Pause { at: When::Next })
        .expect("pause the still-current resource")
        .expect("real native Stop for A");
    assert_ne!(paused, original);
    assert!(
        rig.queue
            .target
            .is_some_and(|target| target.to == rig.ids[0] && !target.playing)
    );
    assert_eq!(rig.deck.mixer.held(slot), Some(source.as_str()));
    rig.assert_no_pending_load();
    rig.settle(applied);
    assert_eq!(
        rig.queue.current,
        Some(rig.ids[1]),
        "original B commits first"
    );
    let continued = rig
        .queue
        .target
        .expect("paused A remains requested")
        .continued;
    assert!(matches!(
        continued,
        Some(crate::queue::types::Continuation::Pause(_))
    ));
    if repeated {
        rig.apply(QueueCommand::Pause { at: When::Next })
            .expect("repeat Pause while A waits for its Stop");
        assert_eq!(
            rig.queue.target.expect("same paused A intent").continued,
            continued
        );
    }
    for stage in ["native cancellation and Stop", "native paused reselect"] {
        let receipts = rig.deck.block(SessionFrame::new(128), 0.0).expect(stage);
        for receipt in receipts {
            let waiting = rig.queue.target.and_then(|target| target.continued);
            let seq = receipt.seq();
            rig.settle(vec![receipt]);
            if let Some(crate::queue::types::Continuation::Pause(expected)) = waiting
                && seq != expected
            {
                assert_eq!(
                    rig.queue.target.and_then(|target| target.continued),
                    waiting,
                    "an unrelated receipt {seq:?} cannot replace outstanding Stop {expected:?}"
                );
            }
        }
    }
    assert_eq!(rig.queue.current, Some(rig.ids[0]));
    let current = rig
        .queue
        .active_current_index()
        .expect("restored current A");
    let active = rig.queue.active.get(current).expect("retained A");
    assert_eq!(active.slot, slot);
    assert!(matches!(
        active.track.snapshot().as_ref().status,
        PlayingStatus::Paused { .. }
    ));
    assert_eq!(rig.deck.mixer.held(slot), Some(source.as_str()));
    rig.assert_no_pending_load();
}

#[kithara::test(tokio)]
async fn a_rejected_previous_leaves_the_reselected_current_and_history_intact() {
    let mut rig = HandoverRig::with_previous_history().await;
    rig.arm(1).await;
    let released_slot = rig.arm(2).await;
    let released_source = rig.source(2).to_owned();
    let slot = rig.slot(0);
    let source = rig.source(0).to_owned();
    let mut observed = rig.queue.control().subscribe::<QueueEvent>();
    let original = rig
        .apply(QueueCommand::Previous(Transition::None))
        .expect("previous request")
        .expect("native previous sequence");
    let maintenance = rig
        .deck
        .block(SessionFrame::new(0), 0.0)
        .expect("native maintenance before Previous");
    assert!(maintenance.iter().all(|receipt| receipt.seq() != original));
    assert_eq!(
        maintenance
            .iter()
            .flat_map(|receipt| &receipt.batch().commands)
            .filter(|part| matches!(part,
                DeckPart::Returned(Returned::Pcm { slot, pcm })
                if *slot == released_slot && &**pcm.src() == released_source.as_str()
            ))
            .count(),
        1,
        "preceding Detach returns the actual unrelated C consumer"
    );
    rig.settle(maintenance);
    assert!(
        rig.queue
            .pending_handover
            .is_some_and(|pending| pending.seq == original)
    );
    rig.deck.ring.publish().expect("publish previous");
    rig.deck.inbox.drain();
    let mut scope = rig.deck.inbox.scope(rig.deck.scope).expect("deck scope");
    let due = scope
        .next_due(SessionFrame::new(128), 1)
        .expect("native previous due");
    assert_eq!(due.seq(), original);
    due.refuse(kithara_render::bridge::DeckRefusal::Deferral);
    let ScopedReceipt::Scope(_, rejected) = rig.deck.ring.receipt().expect("native refusal") else {
        panic!("previous receipt belongs to its deck scope");
    };
    rig.select(0, Transition::None)
        .expect("reselect current while refusal is held");
    assert!(rig.queue.current_track().is_some());
    assert_eq!(rig.queue.current, Some(rig.ids[0]));
    rig.settle(vec![rejected]);
    assert_eq!(rig.queue.current, Some(rig.ids[0]));
    assert!(rig.queue.target.is_none());
    assert_eq!(rig.slot(0), slot);
    assert_eq!(rig.deck.mixer.held(slot), Some(source.as_str()));
    assert!(
        events(&mut observed)
            .iter()
            .all(|event| !matches!(event, QueueEvent::CurrentTrackChanged { .. }))
    );
    rig.assert_no_pending_load();
    rig.apply(QueueCommand::Previous(Transition::None))
        .expect("uncommitted previous stays in history");
    assert_eq!(rig.queue.target.map(|target| target.to), Some(rig.ids[1]));
}

#[kithara::test(tokio)]
async fn cancelling_a_current_reselect_preserves_current_until_the_original_verdict() {
    let mut rig = HandoverRig::with_previous_history().await;
    rig.arm(1).await;
    rig.arm(2).await;
    let slot = rig.slot(0);
    let source = rig.source(0).to_owned();
    let original = rig
        .apply(QueueCommand::Previous(Transition::None))
        .expect("previous request")
        .expect("native previous sequence");
    let applied = rig
        .deck
        .block(SessionFrame::new(128), 0.0)
        .expect("held native previous");
    assert!(
        applied.iter().any(|receipt| receipt.seq() == original
            && matches!(receipt.outcome(), Outcome::Applied { .. }))
    );
    rig.select(0, Transition::None).expect("reselect current A");
    assert!(rig.queue.current_track().is_some());
    rig.select(2, Transition::None)
        .expect("cancel A request with C");
    assert_eq!(rig.queue.current, Some(rig.ids[0]));
    assert!(
        rig.queue.current_track().is_some(),
        "cancelling intent cannot release the applied current"
    );
    let current = rig
        .queue
        .active_current_index()
        .expect("preserved current A");
    let active = rig.queue.active.get(current).expect("current A");
    assert_eq!(active.item, rig.ids[0]);
    assert_eq!(active.slot, slot);
    assert!(
        active.load.is_none(),
        "current A did not request another Load"
    );
    assert_eq!(rig.deck.mixer.held(slot), Some(source.as_str()));
    assert_eq!(rig.queue.target.map(|target| target.to), Some(rig.ids[2]));
    rig.settle(applied);
    assert_eq!(
        rig.queue.current,
        Some(rig.ids[1]),
        "original B's actual verdict survives cancellation of A intent"
    );
    assert_eq!(rig.queue.target.map(|target| target.to), Some(rig.ids[2]));
}

#[kithara::test(tokio)]
async fn a_current_reselect_can_resume_while_its_pause_answer_is_pending() {
    let mut rig = HandoverRig::with_previous_history().await;
    rig.arm(1).await;
    rig.arm(2).await;
    let slot = rig.slot(0);
    let source = rig.source(0).to_owned();
    let mut observed = rig.queue.control().subscribe::<QueueEvent>();
    let original = rig
        .apply(QueueCommand::Previous(Transition::None))
        .expect("previous request")
        .expect("native previous sequence");
    let applied = rig
        .deck
        .block(SessionFrame::new(128), 0.0)
        .expect("held native previous");
    assert!(
        applied.iter().any(|receipt| receipt.seq() == original
            && matches!(receipt.outcome(), Outcome::Applied { .. }))
    );
    rig.select(0, Transition::None).expect("reselect current A");
    rig.apply(QueueCommand::Pause { at: When::Next })
        .expect("paused A intent");
    rig.settle(applied);
    assert_eq!(
        rig.queue.current,
        Some(rig.ids[1]),
        "original B commits first"
    );
    let pause = match rig.queue.target.expect("paused A intent").continued {
        Some(crate::queue::types::Continuation::Pause(seq)) => seq,
        other => panic!("real A Stop is outstanding: {other:?}"),
    };
    let before_resume = rig
        .queue
        .active
        .iter()
        .find(|active| active.item == rig.ids[0])
        .expect("A before resume")
        .track
        .snapshot()
        .as_ref()
        .status;
    rig.select(0, Transition::None)
        .expect("resume the requested A");
    assert!(
        rig.queue
            .target
            .is_some_and(|target| target.to == rig.ids[0] && target.playing)
    );
    assert!(
        rig.queue
            .pending_handover
            .is_some_and(|pending| pending.item == rig.ids[0] && pending.seq != pause)
    );
    let entry = rig.queue.pending_handover.expect("resumed A entry").seq;
    let mut trace = Vec::new();
    for stage in ["native Stop and resumed entry", "resumed entry settlement"] {
        let receipts = rig.deck.block(SessionFrame::new(128), 0.0).expect(stage);
        for receipt in receipts {
            let seq = receipt.seq();
            let outcome = format!("{:?}", receipt.outcome());
            let basis = format!("{:?}", receipt.batch().basis);
            let parts = receipt
                .batch()
                .commands
                .iter()
                .map(|part| match part {
                    DeckPart::Start { slot, .. } => format!("Start({slot:?})"),
                    DeckPart::Stop { slot, .. } => format!("Stop({slot:?})"),
                    DeckPart::Fade { slot, dir, .. } => format!("Fade({slot:?}, {dir:?})"),
                    DeckPart::Returned(Returned::Stopped { slot, resume }) => {
                        format!("Stopped({slot:?}, {resume:?})")
                    }
                    DeckPart::Returned(Returned::Pcm { slot, pcm }) => {
                        format!("Pcm({slot:?}, {})", &**pcm.src())
                    }
                    other => format!("{other:?}"),
                })
                .collect::<Vec<_>>();
            rig.settle(vec![receipt]);
            let status = rig
                .queue
                .active
                .iter()
                .find(|active| active.item == rig.ids[0])
                .map(|active| active.track.snapshot().as_ref().status);
            trace.push(format!("{stage}: {seq:?} {outcome} basis={basis} parts={parts:?} A={status:?} target={:?} pending={:?}",
                rig.queue.target, rig.queue.pending_handover));
        }
    }
    assert_eq!(rig.queue.current, Some(rig.ids[0]));
    let current = rig.queue.active_current_index().expect("resumed current A");
    let active = rig.queue.active.get(current).expect("same A resource");
    assert_eq!(active.slot, slot);
    assert!(
        matches!(
            active.track.snapshot().as_ref().status,
            PlayingStatus::Playing { .. }
        ),
        "A={:?}, before_resume={before_resume:?}, pause={pause:?}, entry={entry:?}, native={trace:#?}",
        active.track.snapshot().as_ref().status
    );
    assert_eq!(rig.deck.mixer.held(slot), Some(source.as_str()));
    rig.assert_no_pending_load();
    let changed = events(&mut observed)
        .into_iter()
        .filter_map(|event| match event {
            QueueEvent::CurrentTrackChanged { id } => id,
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(changed, vec![rig.ids[1], rig.ids[0]]);
    rig.apply(QueueCommand::Previous(Transition::None))
        .expect("history after resumed A");
    assert_eq!(rig.queue.target.map(|target| target.to), Some(rig.ids[1]));
}

#[kithara::test(tokio)]
async fn a_rejected_current_reselect_pause_retires_its_actual_sequence() {
    let mut rig = HandoverRig::with_previous_history().await;
    rig.arm(1).await;
    rig.arm(2).await;
    let slot = rig.slot(0);
    let source = rig.source(0).to_owned();
    let mut observed = rig.queue.control().subscribe::<QueueEvent>();
    let original = rig
        .apply(QueueCommand::Previous(Transition::None))
        .expect("previous request")
        .expect("native previous sequence");
    let applied = rig
        .deck
        .block(SessionFrame::new(128), 0.0)
        .expect("held native previous");
    assert!(
        applied.iter().any(|receipt| receipt.seq() == original
            && matches!(receipt.outcome(), Outcome::Applied { .. }))
    );
    rig.select(0, Transition::None).expect("reselect current A");
    rig.apply(QueueCommand::Pause { at: When::Next })
        .expect("paused A intent");
    let maintenance = rig
        .deck
        .block(SessionFrame::new(128), 0.0)
        .expect("native preceding maintenance held until B answers");
    assert!(maintenance.iter().all(|receipt| receipt.seq() != original));
    rig.settle(applied);
    assert_eq!(
        rig.queue.current,
        Some(rig.ids[1]),
        "original B commits first"
    );
    let pause = match rig.queue.target.expect("paused A intent").continued {
        Some(crate::queue::types::Continuation::Pause(seq)) => seq,
        other => panic!("real A Stop is outstanding: {other:?}"),
    };
    rig.settle(maintenance);
    assert_eq!(
        rig.queue
            .target
            .expect("Stop remains outstanding")
            .continued,
        Some(crate::queue::types::Continuation::Pause(pause))
    );
    rig.deck.ring.publish().expect("publish requested Stop");
    rig.deck.inbox.drain();
    let mut scope = rig.deck.inbox.scope(rig.deck.scope).expect("deck scope");
    let due = scope
        .next_due(SessionFrame::new(128), 1)
        .expect("native Stop due");
    assert_eq!(due.seq(), pause);
    assert!(
        due.commands()
            .iter()
            .any(|part| matches!(part, DeckPart::Stop { slot: stopped, .. }
        if *stopped == slot))
    );
    due.refuse(kithara_render::bridge::DeckRefusal::Deferral);
    let ScopedReceipt::Scope(_, rejected) = rig.deck.ring.receipt().expect("real Stop refusal")
    else {
        panic!("Stop receipt belongs to its deck scope");
    };
    assert_eq!(rejected.seq(), pause);
    assert!(matches!(
        rejected.outcome(),
        Outcome::Rejected(Rejection::Refused(_))
    ));
    rig.settle(vec![rejected]);
    assert!(
        rig.queue.target.is_none(),
        "a terminal refusal cannot retain an outstanding Pause"
    );
    assert!(rig.queue.pending_handover.is_none());
    let released = rig
        .deck
        .block(SessionFrame::new(128), 0.0)
        .expect("native A cancellation");
    assert_eq!(
        released
            .iter()
            .flat_map(|receipt| &receipt.batch().commands)
            .filter(
                |part| matches!(part, DeckPart::Returned(Returned::Pcm { slot: returned, pcm })
            if *returned == slot && &**pcm.src() == source.as_str())
            )
            .count(),
        1
    );
    rig.settle(released);
    assert!(rig.queue.target.is_none());
    assert_eq!(rig.deck.mixer.held(slot), None);
    assert_eq!(rig.queue.current, Some(rig.ids[1]));
    let changed = events(&mut observed)
        .into_iter()
        .filter_map(|event| match event {
            QueueEvent::CurrentTrackChanged { id } => id,
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(changed, vec![rig.ids[1]], "refused A never becomes current");
    rig.assert_no_pending_load();
}
