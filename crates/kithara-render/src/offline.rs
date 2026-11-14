use std::{num::NonZeroU32, ops::Range};

use kithara_command::{LevelInbox, ScopeId};
use kithara_signal::{OutputContext, SessionFrame};
use kithara_test_utils::kithara;
use ringbuf::{
    HeapCons, HeapProd, HeapRb,
    traits::{Consumer, Observer, Producer, Split},
};

use crate::{
    bridge::{DeckPart, DeckProtocol, OfflineDeckError, SessionInbox},
    rt::{
        DeckMixer, OfflineProcessor,
        track::{PlayerResource, PlayerTrack, ReadOutcome},
    },
    worker::wait_for_packet,
};

/// Sole owner of an offline mixer between complete graph turns.
pub struct OfflineDeck<E: SessionInbox> {
    scope: ScopeId,
    mixer: Option<Box<DeckMixer<E>>>,
    prepared: Option<usize>,
    to_processor: HeapProd<(Box<DeckMixer<E>>, usize)>,
    from_processor: HeapCons<Box<DeckMixer<E>>>,
}

impl<E: SessionInbox> OfflineDeck<E> {
    pub(crate) fn channel(scope: ScopeId) -> (Self, OfflineProcessor<E>) {
        let (to_processor, from_owner) = HeapRb::new(1).split();
        let (to_owner, from_processor) = HeapRb::new(1).split();
        (
            Self {
                scope,
                mixer: None,
                prepared: None,
                to_processor,
                from_processor,
            },
            OfflineProcessor::new(to_owner, from_owner),
        )
    }

    /// Apply due deck commands and wait for opted-in PCM before graph processing.
    ///
    /// # Errors
    /// Returns an error if a previous graph turn did not return its mixer.
    pub fn prepare<I: SessionInbox>(
        &mut self,
        inbox: &mut I,
        start: SessionFrame,
        frames: usize,
        sample_rate: NonZeroU32,
    ) -> Result<usize, OfflineDeckError> {
        if frames == 0 {
            return Err(OfflineDeckError::InvalidFrames);
        }
        self.acquire()?;
        let mixer = self.mixer.as_mut().ok_or(OfflineDeckError::MissingMixer)?;
        let frames = prepare_mixer(mixer, inbox.scope(self.scope), start, frames, sample_rate);
        self.prepared = Some(frames);
        Ok(frames)
    }

    /// Reserve the prepared mixer for exactly `frames` graph frames.
    ///
    /// # Errors
    /// Returns an error while the processor still owns the previous turn.
    pub fn reserve_render(&self, frames: usize) -> Result<(), OfflineDeckError> {
        if !self
            .prepared
            .is_some_and(|prepared| frames > 0 && frames <= prepared)
        {
            return Err(OfflineDeckError::InvalidFrames);
        }
        if self.mixer.is_none() {
            return Err(OfflineDeckError::MissingMixer);
        }
        if self.to_processor.is_full() {
            return Err(OfflineDeckError::TurnInFlight);
        }
        Ok(())
    }

    /// Transfers the reserved complete offline turn.
    ///
    /// # Errors
    /// Retains the mixer if the prepared turn or its channel reservation is unavailable.
    pub fn begin_render(&mut self, frames: usize) -> Result<(), OfflineDeckError> {
        self.reserve_render(frames)?;
        let mixer = self.mixer.take().ok_or(OfflineDeckError::MissingMixer)?;
        if let Err((mixer, _)) = self.to_processor.try_push((mixer, frames)) {
            self.mixer = Some(mixer);
            return Err(OfflineDeckError::TurnInFlight);
        }
        self.prepared = None;
        Ok(())
    }

    /// Reclaim the mixer after the graph has completed the declared turn.
    ///
    /// # Errors
    /// Returns an error if the graph failed to return the owning value.
    pub fn end_render(&mut self) -> Result<(), OfflineDeckError> {
        self.acquire()
    }

    fn acquire(&mut self) -> Result<(), OfflineDeckError> {
        if self.mixer.is_none() {
            self.mixer = Some(
                self.from_processor
                    .try_pop()
                    .ok_or(OfflineDeckError::MissingMixer)?,
            );
        }
        Ok(())
    }
}

pub(crate) fn prepare_mixer<E: SessionInbox>(
    mixer: &mut DeckMixer<E>,
    level: Option<LevelInbox<'_, DeckProtocol>>,
    start: SessionFrame,
    frames: usize,
    rate: NonZeroU32,
) -> usize {
    mixer.update_sample_rate(rate);
    let frames = frames.min(mixer.capacity);
    let Some(mut level) = level.filter(|_| !mixer.retired) else {
        return frames;
    };
    let waits = mixer
        .deck
        .tracks
        .iter_mut()
        .any(|(_, track)| track.waits_offline())
        || level.pending_commands().any(|part| match part {
            DeckPart::Attach { pcm, .. } | DeckPart::Replace { pcm, .. } => pcm.waits_offline(),
            _ => false,
        });
    if !waits {
        return frames;
    }
    mixer.deck.recycle.fill(usize::MAX);
    mixer.deck.arrivals(&mut level);
    loop {
        let Some(due) = level.next_due(start, 1) else {
            break;
        };
        mixer.deck.take_due(due, true, read_pcm);
        mixer.deck.resolve_armed(&mut level, start, start);
    }
    loop {
        mixer.deck.maintain();
        for (_, track) in mixer.deck.tracks.iter_mut() {
            prepare_track(track);
        }
        let ended = mixer.deck.observe_ends(0, start, &mut level);
        mixer
            .deck
            .fire_ended(&mut level, start, start, true, read_pcm);
        if !ended {
            break;
        }
    }
    let commands = level
        .frames_until_due(start)
        .and_then(|frames| usize::try_from(frames).ok())
        .unwrap_or(frames);
    mixer
        .deck
        .tracks
        .iter_mut()
        .filter_map(|(_, track)| {
            prepare_track(track)
                .then(|| track.frames_until_boundary(start))
                .flatten()
        })
        .fold(frames.min(commands), usize::min)
        .max(1)
}

pub(crate) fn prepare_track(track: &mut PlayerTrack) -> bool {
    if !track.can_read() || !track.waits_offline() {
        return false;
    }
    let mut budget = usize::MAX;
    while let Some(receiver) = track.refill(&mut budget) {
        if receiver.peek().is_some() || receiver.is_closed() {
            break;
        }
        wait_for_packet(receiver);
    }
    true
}

#[kithara::hang_watchdog]
fn read_pcm(
    reader: &mut PlayerResource,
    context: Option<&OutputContext>,
    buffers: &mut [&mut [f32]],
    range: Range<usize>,
    budget: &mut usize,
) -> ReadOutcome {
    let mut written = 0;
    loop {
        hang_tick!();
        if let Some(receiver) = reader.refill(budget) {
            wait_for_packet(receiver);
        }
        let remaining = range.len() - written;
        let frames = reader
            .frames_until_boundary()
            .filter(|frames| *frames > 0)
            .map_or(remaining, |frames| frames.min(remaining));
        let start = range.start + written;
        let outcome = reader.read(context, buffers, start..start + frames, budget);
        match outcome {
            ReadOutcome::Full { frames } => {
                written += frames;
                if frames == 0 || written == range.len() {
                    return ReadOutcome::Full { frames: written };
                }
                hang_reset!();
            }
            ReadOutcome::Partial { frames } => {
                return ReadOutcome::Partial {
                    frames: written + frames,
                };
            }
            ReadOutcome::Eof | ReadOutcome::Failed(_) if written == 0 => return outcome,
            ReadOutcome::Eof | ReadOutcome::Failed(_) => {
                return ReadOutcome::Partial { frames: written };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_platform::{sync::Arc, time::Duration};
    use kithara_signal::{AudioSpec, SegmentId, SessionFrame};
    use kithara_test_utils::{kithara, test::usdt};

    use super::*;
    use crate::{
        bridge::{DeckPart, Fade, Slot},
        mock::PacketRing,
        rt::{
            processor_fixture::{mixer, send, shape},
            track::PlayerResource,
        },
    };

    #[kithara::test]
    fn nonblocking_preparation_leaves_commands_for_the_graph() {
        let (mut mixer, mut ends) = mixer();
        let rate = shape().sample_rate;
        let mut packets = PacketRing::new(AudioSpec::new(2, rate), Duration::from_secs(1), 1);
        let resource = PlayerResource::new(
            packets.receiver.take().expect("PCM receiver"),
            Arc::from("starved-device"),
        );
        let slot = Slot::new(0);
        send(
            &mut ends,
            kithara_command::When::Next,
            vec![
                DeckPart::Attach {
                    slot,
                    item_id: kithara_events::TrackId::allocate(),
                    pcm: Box::new(resource),
                    segment: SegmentId::FIRST,
                },
                DeckPart::Start {
                    slot,
                    fade: Fade::Declick,
                },
            ],
        );
        mixer.inbox.0.drain();
        let trace = usdt::scope();
        let scope = mixer.mixer.scope;
        let prepared = prepare_mixer(
            &mut mixer.mixer,
            mixer.inbox.0.scope(scope),
            SessionFrame::new(0),
            32,
            rate,
        );
        assert_eq!(prepared, 32);
        assert!(
            mixer.track(slot).is_none(),
            "preparation must not start the producer before the callback"
        );
        assert!(trace.events_of("pcm_wait_started").is_empty());
        drop(trace);
        assert!(
            ends.ring.receipt().is_none(),
            "only the graph may answer the batch"
        );
    }
    #[kithara::test(native)]
    fn offline_tail_keeps_the_gain_law_across_a_delayed_packet() {
        use kithara_platform::thread;
        use kithara_test_utils::{test::usdt, wait_until_blocking};

        let runtime = kithara_platform::tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("fixture runtime");
        let mut packets = runtime.block_on(crate::mock::pcm_fixture::PcmFixture::new(8, true));
        let receiver = packets.receiver.take().expect("PCM receiver");
        let spec = receiver.spec();
        packets
            .push(crate::worker::PcmPacket::Chunk(Box::new(
                crate::worker::packet_tests::chunk(
                    spec,
                    SegmentId::FIRST,
                    0,
                    0,
                    &[0.25, -0.25, 0.5, -0.5],
                ),
            )))
            .expect("first packet");
        let resource = PlayerResource::new(receiver, Arc::from("delayed-tail"));
        let mut track = PlayerTrack::builder()
            .item_id(kithara_events::TrackId::allocate())
            .sample_rate(spec.sample_rate)
            .build(Box::new(resource));
        track.start(Fade::Crossfade(crate::CrossfadeSettings {
            duration: 0.0,
            ..Default::default()
        }));
        let trace = usdt::scope();
        let writer = thread::spawn_named("tail-packet", move || {
            wait_until_blocking(Duration::from_secs(1), "the offline tail waits", || {
                usdt::last("pcm_wait_started").is_some()
            })
            .expect("observe the actual packet wait");
            packets
                .push(crate::worker::PcmPacket::Chunk(Box::new(
                    crate::worker::packet_tests::chunk(
                        spec,
                        SegmentId::FIRST,
                        2,
                        2,
                        &[0.75, -0.75, 1.0, -1.0],
                    ),
                )))
                .expect("delayed packet");
            packets
        });
        let mut left = [99.0; 4];
        let mut right = [99.0; 4];
        let mut budget = usize::MAX;
        assert_eq!(
            track.read_tail(&mut left, &mut right, &mut budget, read_pcm),
            4
        );
        let expected = [
            0.25f32,
            0.5 * (1.0 - 1.0 / 3.0),
            0.75 * (1.0 - 2.0 / 3.0),
            0.0,
        ];
        assert_eq!(left.map(f32::to_bits), expected.map(f32::to_bits));
        assert_eq!(
            right.map(f32::to_bits),
            expected.map(|sample| (-sample).to_bits())
        );
        assert!(!trace.events_of("pcm_wait_started").is_empty());
        assert_eq!(
            trace.events_of("pcm_wait_started").len(),
            trace.events_of("pcm_wait_finished").len()
        );
        drop(trace);
        let _packets = writer.join().expect("packet producer completes");
    }

    #[kithara::test(native)]
    #[case::eof(false)]
    #[case::source_failure(true)]
    fn offline_partial_keeps_its_delayed_terminal_kind(#[case] failure: bool) {
        use kithara_audio::{FailureSource, TrackFailureKind};
        use kithara_platform::thread;
        use kithara_test_utils::{test::usdt, wait_until_blocking};

        let runtime = kithara_platform::tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("fixture runtime");
        let mut packets = runtime.block_on(crate::mock::pcm_fixture::PcmFixture::new(8, true));
        let receiver = packets.receiver.take().expect("PCM receiver");
        let spec = receiver.spec();
        packets
            .push(crate::worker::PcmPacket::Chunk(Box::new(
                crate::worker::packet_tests::chunk(
                    spec,
                    SegmentId::FIRST,
                    0,
                    0,
                    &[0.25, -0.25, 0.5, -0.5],
                ),
            )))
            .expect("physical packet");
        let mut reader = PlayerResource::new(receiver, Arc::from("delayed-terminal"));
        let trace = usdt::scope();
        let writer = thread::spawn_named("tail-terminal", move || {
            wait_until_blocking(Duration::from_secs(1), "the terminal read waits", || {
                usdt::last("pcm_wait_started").is_some()
            })
            .expect("observe the actual packet wait");
            let terminal = if failure {
                crate::worker::PcmPacket::Failed {
                    segment: SegmentId::FIRST,
                    failure: TrackFailureKind::SourceCancelled,
                }
            } else {
                let mut terminal =
                    crate::worker::packet_tests::chunk(spec, SegmentId::FIRST, 2, 2, &[]);
                terminal.meta.end_of_track = true;
                crate::worker::PcmPacket::Chunk(Box::new(terminal))
            };
            packets.push(terminal).expect("delayed terminal");
            packets
        });
        let mut left = [99.0; 4];
        let mut right = [99.0; 4];
        let mut budget = usize::MAX;
        assert_eq!(
            read_pcm(
                &mut reader,
                None,
                &mut [&mut left, &mut right],
                0..4,
                &mut budget
            ),
            ReadOutcome::Partial { frames: 2 }
        );
        assert_eq!(left, [0.25, 0.5, 0.0, 0.0]);
        assert_eq!(right, [-0.25, -0.5, 0.0, 0.0]);
        let expected = if failure {
            ReadOutcome::Failed(FailureSource::Producer {
                failure: TrackFailureKind::SourceCancelled,
            })
        } else {
            ReadOutcome::Eof
        };
        assert_eq!(
            read_pcm(
                &mut reader,
                None,
                &mut [&mut left, &mut right],
                0..4,
                &mut budget
            ),
            expected
        );
        assert!(!trace.events_of("pcm_wait_started").is_empty());
        drop(trace);
        let _packets = writer.join().expect("terminal producer completes");
    }

    #[kithara::test(native, tokio)]
    #[case::empty_packet(false)]
    #[case::future_segment(true)]
    async fn offline_read_stops_at_an_unreadable_packet(#[case] future: bool) {
        let mut packets = crate::mock::pcm_fixture::PcmFixture::new(8, true).await;
        let receiver = packets.receiver.take().expect("PCM receiver");
        let spec = receiver.spec();
        packets
            .push(crate::worker::PcmPacket::Chunk(Box::new(
                crate::worker::packet_tests::chunk(spec, SegmentId::FIRST, 0, 0, &[0.25, -0.25]),
            )))
            .expect("physical packet");
        let segment = if future {
            SegmentId::FIRST.next()
        } else {
            SegmentId::FIRST
        };
        let samples: &[f32] = if future { &[0.75, -0.75] } else { &[] };
        packets
            .push(crate::worker::PcmPacket::Chunk(Box::new(
                crate::worker::packet_tests::chunk(spec, segment, 1, 1, samples),
            )))
            .expect("unreadable packet");
        let mut reader = PlayerResource::new(receiver, Arc::from("boundary-tail"));
        let mut left = [99.0; 2];
        let mut right = [99.0; 2];
        let mut budget = usize::MAX;
        assert_eq!(
            read_pcm(
                &mut reader,
                None,
                &mut [&mut left, &mut right],
                0..2,
                &mut budget
            ),
            ReadOutcome::Full { frames: 1 }
        );
        assert_eq!(left, [0.25, 0.0]);
        assert_eq!(right, [-0.25, 0.0]);
        assert_eq!(
            reader
                .next_chunk_info()
                .expect("boundary stays queued")
                .segment,
            segment
        );
    }
}
