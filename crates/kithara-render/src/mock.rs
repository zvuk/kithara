//! The audio-thread side of a deck, driven by a test in place of a `DeckMixer`.

use kithara_command::{LevelInbox, Seq, Target};
use kithara_platform::time::Duration;
use kithara_signal::{SegmentId, SessionFrame};
use num_traits::ToPrimitive;
use ringbuf::traits::Producer;

use crate::{
    LaneFrame,
    bridge::{
        DeckEvent, DeckPart, DeckProtocol, DeckRefusal, MixerInputs, Returned, Slot, SlotMark,
    },
    rt::track::PlayerResource,
};

/// Take the batches the deck's inbox holds, each as its parts, and apply them on `at`, as the
/// mixer does at its next block.
#[must_use]
pub fn take_batches(
    level: &mut LevelInbox<'_, DeckProtocol>,
    at: SessionFrame,
) -> Vec<Vec<DeckPart>> {
    let mut batches: Vec<Vec<DeckPart>> = Vec::new();
    while let Some(mut due) = level.next_due(at, 1) {
        batches.push(std::mem::take(due.commands_mut()));
        due.apply(());
    }
    batches
}

/// Report `event` to the deck's owner as the mixer does.
///
/// # Errors
/// Returns the event when the ring is full.
pub fn report(inputs: &mut MixerInputs, event: DeckEvent) -> Result<(), DeckEvent> {
    inputs.events.try_push(event)
}

/// A deck's mixer with no audio: it holds the consumers attached to its slots and answers every
/// batch the way the mixer does, its parts back as they applied.
pub struct MockDeck {
    inputs: MixerInputs,
    held: Vec<Option<(Box<PlayerResource>, SegmentId)>>,
    armed: Vec<Option<Seq>>,
}

impl MockDeck {
    #[must_use]
    pub fn new(inputs: MixerInputs) -> Self {
        let held = std::iter::repeat_with(|| None)
            .take(inputs.config.slots().get())
            .collect();
        let armed = vec![None; inputs.config.slots().get()];
        Self {
            inputs,
            held,
            armed,
        }
    }

    /// Applies every batch due by `at` on its own frame; a `Stop` reports the slot at
    /// `stopped_at` seconds.
    pub fn block(
        &mut self,
        level: &mut LevelInbox<'_, DeckProtocol>,
        at: SessionFrame,
        stopped_at: f64,
    ) {
        while let Some(deferred) = level.next_deferred() {
            let slot = match deferred.commands() {
                [DeckPart::Chain { from, .. }] => *from,
                [DeckPart::Adopt { slot, .. }] => *slot,
                _ => {
                    deferred.refuse(DeckRefusal::Deferral);
                    continue;
                }
            };
            if self.armed[slot.index()].is_some() {
                deferred.refuse(DeckRefusal::Occupied { slot });
                continue;
            }
            if let Some(seq) = deferred.park() {
                self.armed[slot.index()] = Some(seq);
            }
        }
        let held = &mut self.held;
        while let Some(mut due) = level.next_due(at, 1) {
            let at = due.at();
            let commands = due.commands_mut();
            for _ in 0..commands.len() {
                let part = commands.remove(0);
                if let Some(left) = hold(held, part, at, stopped_at) {
                    commands.push(left);
                }
            }
            due.apply(());
        }
    }

    /// Applies the batch armed on `slot` at its end frame, as `Deck::fire_ended` does.
    pub fn end(&mut self, level: &mut LevelInbox<'_, DeckProtocol>, slot: Slot, at: SessionFrame) {
        let Some(seq) = self.armed[slot.index()].take() else {
            return;
        };
        let Some(mut due) = level.resume(seq, at, at) else {
            return;
        };
        let commands = due.commands_mut();
        for _ in 0..commands.len() {
            let part = commands.remove(0);
            if let Some(left) = hold(&mut self.held, part, at, 0.0) {
                commands.push(left);
            }
        }
        due.apply(());
    }

    /// Reports `event` to the deck's owner as the mixer does.
    ///
    /// # Errors
    /// Returns the event when the ring is full.
    pub fn report(&mut self, event: DeckEvent) -> Result<(), DeckEvent> {
        report(&mut self.inputs, event)
    }

    /// The source of the consumer `slot` holds.
    #[must_use]
    pub fn held(&self, slot: Slot) -> Option<&str> {
        self.held
            .get(usize::from(slot.get()))
            .and_then(Option::as_ref)
            .map(|(pcm, _)| &**pcm.src())
    }
}

/// What `part` leaves in the receipt once `held` applied it.
fn hold(
    held: &mut [Option<(Box<PlayerResource>, SegmentId)>],
    part: DeckPart,
    at: SessionFrame,
    stopped_at: f64,
) -> Option<DeckPart> {
    let entry = |held: &mut [Option<(Box<PlayerResource>, SegmentId)>], slot: Slot| {
        held.get_mut(usize::from(slot.get()))
            .and_then(Option::take)
            .map(|(pcm, _)| DeckPart::Returned(Returned::Pcm { slot, pcm }))
    };
    match part {
        DeckPart::Attach { slot, pcm, segment } | DeckPart::Replace { slot, pcm, segment } => {
            let left = entry(held, slot);
            if let Some(entry) = held.get_mut(usize::from(slot.get())) {
                *entry = Some((pcm, segment));
            }
            left
        }
        DeckPart::Detach { slot } => entry(held, slot),
        DeckPart::Stop { slot, .. } => held
            .get(usize::from(slot.get()))
            .and_then(Option::as_ref)
            .map(|(pcm, segment)| {
                let position = Duration::try_from_secs_f64(stopped_at).unwrap_or_default();
                let seconds = position.as_secs_f64();
                DeckPart::Returned(Returned::Stopped {
                    slot,
                    resume: SlotMark {
                        session: at,
                        lane: LaneFrame {
                            segment: *segment,
                            frame: (seconds * f64::from(pcm.spec().sample_rate.get()))
                                .round()
                                .to_u64()
                                .unwrap_or(u64::MAX),
                        },
                        position,
                    },
                })
            }),
        DeckPart::Adopt { slot, segment } => {
            if let Some(Some((_, current))) = held.get_mut(usize::from(slot.get())) {
                *current = segment;
            }
            Some(DeckPart::Adopt { slot, segment })
        }
        DeckPart::Eq(crate::bridge::DeckEqChange::Layout(_)) => None,
        other => Some(other),
    }
}

#[cfg(all(test, feature = "mock"))]
pub(crate) use crate::worker::{mock as pcm_fixture, node_fixture};

/// Wait on the receiver's off-RT gate in a test reader.
pub fn wait_for_packet(receiver: &crate::PcmReceiver) {
    receiver.wait_for_packet();
}

/// Keep a test receiver parkable while its reader applies the underrun policy.
#[must_use]
pub fn with_blocking_reads<T, B>(mut config: crate::TrackConfig<T, B>) -> crate::TrackConfig<T, B>
where
    T: kithara_stream::StreamType,
    B: kithara_audio::ResamplerBackend,
{
    config.block_on_underrun = true;
    config
}

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU32, NonZeroUsize};

    use kithara_command::{
        Batch, ChannelConfig, Outcome, Port, ScopedConfig, ScopedReceipt, When, scoped_channel,
    };
    use kithara_platform::sync::Arc;
    use kithara_signal::AudioSpec;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        bridge::{Fade, scope_channels},
        rt::{DeckMixerConfig, track::PcmConsumer},
        test_pools::pools,
        worker::packet_tests::PacketRing,
    };

    #[kithara::test]
    fn a_deferred_chain_applies_on_its_leading_slots_end_frame() {
        let config = DeckMixerConfig::default();
        let (mut sender, mut inbox) = scoped_channel::<DeckProtocol, DeckProtocol>(
            ScopedConfig::builder()
                .scope(
                    ChannelConfig::builder()
                        .targets(config.slots().get())
                        .build(),
                )
                .build(),
        );
        let scope = sender.open(config.slots().get()).expect("scope");
        let (_ends, inputs) = scope_channels(scope, config);
        let mut deck = MockDeck::new(inputs);
        let from = Slot::new(0);
        let to = Slot::new(1);
        let seq = sender
            .scope(scope)
            .expect("scope")
            .send(
                When::Deferred,
                Batch {
                    basis: Vec::new(),
                    commands: vec![DeckPart::Chain { from, to }],
                },
            )
            .expect("deferred chain");
        sender.publish().expect("publish");
        inbox.drain();
        deck.block(
            &mut inbox.scope(scope).expect("borrowed level"),
            SessionFrame::new(0),
            0.0,
        );
        assert!(
            sender.receipt().is_none(),
            "the chain waits for the leading slot"
        );
        let at = SessionFrame::new(4_096);
        deck.end(&mut inbox.scope(scope).expect("borrowed level"), from, at);
        let Some(ScopedReceipt::Scope(answered_scope, receipt)) = sender.receipt() else {
            panic!("one scope receipt");
        };
        assert_eq!(answered_scope, scope);
        assert_eq!(receipt.seq(), seq);
        assert!(
            matches!(receipt.outcome(), Outcome::Applied { at: applied, .. } if *applied == at)
        );
        assert!(matches!(receipt.batch().commands.as_slice(),
            [DeckPart::Chain { from: leading, to: following }] if *leading == from && *following == to));
        assert!(sender.receipt().is_none(), "exactly one terminal receipt");
    }

    fn pcm(src: &str) -> Box<PlayerResource> {
        let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("rate"));
        let mut packets = PacketRing::new(spec, Duration::from_secs(1), 1);
        Box::new(
            PlayerResource::new(
                PcmConsumer::new(packets.receiver.take().expect("receiver")),
                Arc::from(src),
                &pools(),
            )
            .expect("resource"),
        )
    }

    #[kithara::test]
    #[case::valid(0.5, Duration::from_millis(500), 22_050)]
    #[case::negative(-1.0, Duration::ZERO, 0)]
    #[case::nonfinite(f64::NAN, Duration::ZERO, 0)]
    #[case::unrepresentable(f64::MAX, Duration::ZERO, 0)]
    fn a_scoped_mock_block_returns_the_adopted_stop_mark_and_resources(
        #[case] stopped_at: f64,
        #[case] position: Duration,
        #[case] lane_frame: u64,
    ) {
        let config = DeckMixerConfig::default();
        let (mut sender, mut inbox) = scoped_channel::<DeckProtocol, DeckProtocol>(
            ScopedConfig::builder()
                .scope(
                    ChannelConfig::builder()
                        .targets(config.slots().get())
                        .capacity(NonZeroUsize::new(2).expect("capacity"))
                        .build(),
                )
                .build(),
        );
        let scope = sender.open(config.slots().get()).expect("scope");
        let (_ends, inputs) = scope_channels(scope, config);
        let mut deck = MockDeck::new(inputs);
        let slot = Slot::new(0);
        let segment = SegmentId::FIRST.next();
        for (frame, commands) in [
            (
                0,
                vec![DeckPart::Attach {
                    slot,
                    pcm: pcm("first"),
                    segment: SegmentId::FIRST,
                }],
            ),
            (
                1,
                vec![
                    DeckPart::Adopt { slot, segment },
                    DeckPart::Stop {
                        slot,
                        fade: Fade::Declick,
                    },
                ],
            ),
            (
                2,
                vec![DeckPart::Replace {
                    slot,
                    pcm: pcm("second"),
                    segment,
                }],
            ),
            (3, vec![DeckPart::Detach { slot }]),
        ] {
            let at = SessionFrame::new(frame);
            let seq = sender
                .scope(scope)
                .expect("scope")
                .send(
                    When::At(at),
                    Batch {
                        basis: Vec::new(),
                        commands,
                    },
                )
                .expect("batch credit is returned after each block");
            sender.publish().expect("publish");
            inbox.drain();
            deck.block(
                &mut inbox.scope(scope).expect("borrowed level"),
                at,
                stopped_at,
            );
            let Some(ScopedReceipt::Scope(answered_scope, receipt)) = sender.receipt() else {
                panic!("one scope receipt");
            };
            assert_eq!(answered_scope, scope);
            assert_eq!(receipt.seq(), seq);
            assert!(
                matches!(receipt.outcome(), Outcome::Applied { at: applied, data: () } if *applied == at)
            );
            let (_, batch): (Outcome<DeckProtocol>, Batch<DeckProtocol>) = receipt.into();
            match frame {
                0 => assert_eq!(deck.held(slot), Some("first")),
                1 => {
                    assert!(
                        matches!(batch.commands.as_slice(), [DeckPart::Adopt { .. }, DeckPart::Returned(Returned::Stopped { slot: stopped, resume })]
                        if *stopped == slot && *resume == SlotMark {
                            session: at,
                            lane: LaneFrame { segment, frame: lane_frame },
                            position,
                        })
                    );
                    assert_eq!(deck.held(slot), Some("first"));
                }
                2 => {
                    assert!(
                        matches!(batch.commands.as_slice(), [DeckPart::Returned(Returned::Pcm { slot: returned, pcm })]
                        if *returned == slot && &**pcm.src() == "first")
                    );
                    assert_eq!(deck.held(slot), Some("second"));
                }
                3 => {
                    assert!(
                        matches!(batch.commands.as_slice(), [DeckPart::Returned(Returned::Pcm { slot: returned, pcm })]
                        if *returned == slot && &**pcm.src() == "second")
                    );
                    assert_eq!(deck.held(slot), None);
                }
                _ => unreachable!(),
            }
            assert!(sender.receipt().is_none(), "exactly one verdict");
        }
    }
}
