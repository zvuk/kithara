use std::num::{NonZeroU32, NonZeroUsize};

use kithara_audio::{Fetch, SeekOutcome, TrackStep, WaitingReason};
use kithara_command::{
    Batch, ChannelConfig, LevelInbox, Outcome, Port, ScopeId, ScopedConfig, ScopedInbox,
    ScopedReceipt, ScopedSender, Seq, When, scoped_channel,
};
use kithara_platform::{sync::Arc, time::Duration};
use kithara_signal::{AudioSpec, SegmentId, SessionFrame};
use kithara_test_utils::{
    bufpool::{TestPools, pools},
    kithara,
};
use kithara_worker::{Task, TickResult};
use num_traits::ToPrimitive;

use super::{DeckMixer, StreamShape};
use crate::{
    bridge::{DeckEnds, DeckPart, DeckProtocol, SessionInbox, Slot, scope_channels},
    mock::{PacketRing, chunk},
    rt::{
        DeckMixerConfig,
        track::{PcmConsumer, PlayerResource},
    },
    worker::PcmPacket,
};

pub type DeckReply = (Seq, Outcome<DeckProtocol>, Vec<DeckPart>);

/// Applies one admitted deck batch without rendering PCM or advancing its position.
#[must_use]
pub fn apply_initial_batch<E: SessionInbox>(
    mixer: &mut DeckMixer<E>,
    inbox: &mut ScopedInbox<DeckProtocol, DeckProtocol>,
) -> bool {
    inbox.drain();
    let Some(mut level) = inbox.scope(mixer.scope) else {
        return false;
    };
    let Some(due) = level.next_due(SessionFrame::new(0), 1) else {
        return false;
    };
    mixer.deck.take_due(due, true);
    true
}

/// Returns the fixture's fixed sample rate and block size.
///
/// # Panics
/// Panics if either fixed geometry value is zero.
#[must_use]
pub fn shape() -> StreamShape {
    StreamShape {
        sample_rate: NonZeroU32::new(44_100).expect("static sample rate"),
        max_block_frames: NonZeroU32::new(512).expect("static block size"),
    }
}

pub struct TestSessionInbox(pub ScopedInbox<DeckProtocol, DeckProtocol>);

impl SessionInbox for TestSessionInbox {
    fn scope(&mut self, id: ScopeId) -> Option<LevelInbox<'_, DeckProtocol>> {
        self.0.scope(id)
    }
}

pub struct TestMixer {
    pub mixer: DeckMixer<TestSessionInbox>,
    pub inbox: TestSessionInbox,
}

impl std::ops::Deref for TestMixer {
    type Target = DeckMixer<TestSessionInbox>;
    fn deref(&self) -> &Self::Target {
        &self.mixer
    }
}

pub struct TestEnds {
    pub ring: ScopedSender<DeckProtocol, DeckProtocol>,
    pub deck: DeckEnds,
}

impl std::ops::Deref for TestEnds {
    type Target = DeckEnds;
    fn deref(&self) -> &Self::Target {
        &self.deck
    }
}

impl std::ops::DerefMut for TestEnds {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.deck
    }
}

#[must_use]
pub fn mixer() -> (TestMixer, TestEnds) {
    mixer_with_config(DeckMixerConfig::default(), 64)
}

#[must_use]
pub fn mixer_with_config(config: DeckMixerConfig, capacity: usize) -> (TestMixer, TestEnds) {
    mixer_with_shape(config, capacity, shape())
}

/// Creates a mixer and command ends with the supplied geometry.
///
/// # Panics
/// Panics if capacity is zero, the scope cannot open, or mixer pool allocation fails.
#[must_use]
pub fn mixer_with_shape(
    config: DeckMixerConfig,
    capacity: usize,
    shape: StreamShape,
) -> (TestMixer, TestEnds) {
    let (mut ring, inbox) = scoped_channel(
        ScopedConfig::builder()
            .scope(
                ChannelConfig::builder()
                    .targets(config.slots().get())
                    .capacity(NonZeroUsize::new(capacity).expect("capacity"))
                    .build(),
            )
            .build(),
    );
    let scope = ring.open(config.slots().get()).expect("deck scope");
    let (deck, inputs) = scope_channels(scope, config);
    let mixer = DeckMixer::new(inputs, shape, &pools()).expect("mixer pools");
    (
        TestMixer {
            mixer,
            inbox: TestSessionInbox(inbox),
        },
        TestEnds { ring, deck },
    )
}

/// Creates a PCM resource containing the samples and optional end-of-track packet.
///
/// # Panics
/// Panics if duration is invalid or the fixture pools cannot allocate the PCM resource.
#[must_use]
pub fn pcm_samples(src: &str, seconds: f64, samples: &[f32], ended: bool) -> Box<PlayerResource> {
    let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("sample rate"));
    let mut ring = PacketRing::new(spec, Duration::from_secs_f64(seconds), 2);
    ring.push(PcmPacket::Chunk(Box::new(chunk(
        spec,
        SegmentId::FIRST,
        0,
        0,
        samples,
    ))));
    if ended {
        let mut end = chunk(
            spec,
            SegmentId::FIRST,
            (samples.len() / 2) as u64,
            (samples.len() / 2) as u64,
            &[],
        );
        end.meta.end_of_track = true;
        ring.push(PcmPacket::Chunk(Box::new(end)));
    }
    Box::new(
        PlayerResource::new(
            PcmConsumer::new(ring.receiver.take().expect("receiver")),
            Arc::from(src),
            &pools(),
        )
        .expect("resource"),
    )
}

pub fn send(ring: &mut TestEnds, when: When<SessionFrame>, parts: Vec<DeckPart>) -> Seq {
    send_on(ring, when, &[], parts)
}

/// Admits and publishes a batch with the supplied slot basis.
///
/// # Panics
/// Panics if the scope is not live or the batch cannot be admitted or published.
pub fn send_on(
    ring: &mut TestEnds,
    when: When<SessionFrame>,
    basis: &[(Slot, Option<Seq>)],
    commands: Vec<DeckPart>,
) -> Seq {
    let scope = ring.deck.scope;
    let seq = ring
        .ring
        .scope(scope)
        .expect("live scope")
        .send(
            when,
            Batch {
                commands,
                basis: basis.to_vec(),
            },
        )
        .expect("the deck ring has room");
    ring.ring.publish().expect("publish batch");
    seq
}

#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
pub fn receipts(ends: &mut TestEnds) -> Vec<DeckReply> {
    let mut receipts: Vec<DeckReply> = Vec::new();
    while let Some(receipt) = ends.ring.receipt() {
        hang_tick!();
        if let ScopedReceipt::Scope(scope, receipt) = receipt {
            assert_eq!(scope, ends.deck.scope);
            let seq = receipt.seq();
            let (outcome, batch): (Outcome<DeckProtocol>, Batch<DeckProtocol>) = receipt.into();
            receipts.push((seq, outcome, batch.commands));
            hang_reset!();
        }
    }
    receipts
}

/// Finds the verdict for the specified sequence.
///
/// # Panics
/// Panics if no receipt answers the sequence.
#[must_use]
pub fn outcome_of(receipts: &[DeckReply], seq: Seq) -> &Outcome<DeckProtocol> {
    receipts
        .iter()
        .find_map(|(answered, outcome, _)| (*answered == seq).then_some(outcome))
        .expect("the batch is answered")
}

/// Verifies an applied verdict at the exact session frame.
///
/// # Panics
/// Panics if the outcome is not applied at the specified frame.
pub fn assert_applied_at(outcome: &Outcome<DeckProtocol>, frame: usize) {
    assert!(
        matches!(outcome, Outcome::Applied { at, data: () } if *at == SessionFrame::new(frame as i64)),
        "expected applied at {frame}, got {outcome:?}"
    );
}

pub type PreparedSeek = (
    Box<PlayerResource>,
    crate::worker::DecoderNode<SeekSource, TestPools>,
    kithara_command::Sender<crate::LaneProtocol>,
    std::sync::mpsc::Receiver<Duration>,
);

pub struct SeekSource {
    frame: u64,
    ready: bool,
    spec: AudioSpec,
    seeks: std::sync::mpsc::Sender<Duration>,
}

impl kithara_audio::AudioSource for SeekSource {
    type Chunk = kithara_signal::AudioChunk;

    fn host_sample_rate(&self) -> Option<NonZeroU32> {
        Some(self.spec.sample_rate)
    }

    fn seek(&mut self, target: Duration) -> Result<SeekOutcome, kithara_audio::AudioReadError> {
        self.seeks.send(target).expect("reader seek observer");
        self.frame = (target.as_secs_f64() * f64::from(self.spec.sample_rate.get()))
            .to_u64()
            .expect("fixture seek frame fits u64");
        self.ready = true;
        Ok(SeekOutcome::Landed {
            target,
            landed_at: target,
        })
    }

    fn seek_for_render(
        &mut self,
        target: Duration,
    ) -> Result<SeekOutcome, kithara_audio::AudioReadError> {
        self.seek(target)
    }

    fn set_host_sample_rate(&mut self, rate: NonZeroU32) {
        self.spec.sample_rate = rate;
    }

    fn step_track(&mut self) -> TrackStep<Self::Chunk> {
        if !std::mem::take(&mut self.ready) {
            return TrackStep::Blocked(WaitingReason::WaitingDemand);
        }
        TrackStep::Produced(Fetch::Data {
            data: chunk(
                self.spec,
                SegmentId::FIRST,
                self.frame,
                self.frame,
                &[0.5; 2048],
            ),
            source_end: None,
        })
    }
}

#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
pub fn prepared_seek(spec: AudioSpec, segment: SegmentId, target: Duration) -> PreparedSeek {
    let (seeks, counts) = std::sync::mpsc::channel();
    let source = SeekSource {
        spec,
        seeks,
        frame: 0,
        ready: false,
    };
    let (mut node, receiver, mut lane) = crate::mock::terminal_node(source, spec, false);
    lane.send(
        When::Next,
        Batch {
            basis: Vec::new(),
            commands: vec![crate::LaneCommand::Segment {
                id: segment,
                from: target,
                speed: kithara_sync::SpeedCurve::Constant(1.0),
            }],
        },
    )
    .expect("control-side seek");
    for _ in 0..16 {
        hang_tick!();
        if node.tick() == TickResult::Progress {
            hang_reset!();
        }
    }
    assert_eq!(counts.try_iter().collect::<Vec<_>>(), vec![target]);
    assert_eq!(lane.receipts().count(), 1);
    let pcm = PlayerResource::new(PcmConsumer::new(receiver), Arc::from("split.mp3"), &pools())
        .expect("prepared PCM");
    (Box::new(pcm), node, lane, counts)
}
