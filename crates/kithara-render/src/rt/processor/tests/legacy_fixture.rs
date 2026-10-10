use std::num::{NonZeroU32, NonZeroUsize};

use firewheel::node::ProcBuffers;
use kithara_command::{ScopedReceipt, Seq, When};
use kithara_platform::{sync::Arc, time::Duration};
use kithara_signal::{AudioSpec, OutputContext, SegmentId, SessionEpoch, SessionFrame};
use kithara_warp::RenderContext;
use num_traits::ToPrimitive;

use super::{TestEnds, TestMixer, mixer_with_shape, send};
use crate::{
    CrossfadeSettings,
    bridge::{DeckPart, Fade, Slot},
    rt::{
        DeckMixerConfig, StreamShape,
        track::{PcmConsumer, PlayerResource},
    },
    test_pools::pools,
    worker::{
        PcmPacket,
        packet_tests::{PacketRing, chunk},
    },
};

pub(in crate::rt) const RATE: NonZeroU32 = NonZeroU32::new(44_100).expect("fixture rate");
pub(in crate::rt) type Resource = (Box<PlayerResource>, PacketRing);
type PreparedSeek = (
    Box<PlayerResource>,
    crate::worker::DecoderNode<SeekSource, crate::test_pools::TestPools>,
    kithara_command::Sender<crate::LaneProtocol>,
    std::sync::mpsc::Receiver<Duration>,
);

pub(in crate::rt) struct SeekSource {
    frame: u64,
    ready: bool,
    spec: AudioSpec,
    seeks: std::sync::mpsc::Sender<Duration>,
}

impl kithara_audio::AudioSource for SeekSource {
    type Chunk = kithara_signal::AudioChunk;

    fn set_host_sample_rate(&mut self, rate: NonZeroU32) {
        self.spec.sample_rate = rate;
    }

    fn host_sample_rate(&self) -> Option<NonZeroU32> {
        Some(self.spec.sample_rate)
    }

    fn step_track(&mut self) -> kithara_audio::TrackStep<Self::Chunk> {
        if !std::mem::take(&mut self.ready) {
            return kithara_audio::TrackStep::Blocked(kithara_audio::WaitingReason::WaitingDemand);
        }
        kithara_audio::TrackStep::Produced(kithara_audio::Fetch::Data {
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

    fn seek(
        &mut self,
        target: Duration,
    ) -> Result<kithara_audio::SeekOutcome, kithara_audio::AudioReadError> {
        self.seeks.send(target).expect("reader seek observer");
        self.frame = (target.as_secs_f64() * f64::from(self.spec.sample_rate.get()))
            .to_u64()
            .expect("fixture seek frame fits u64");
        self.ready = true;
        Ok(kithara_audio::SeekOutcome::Landed {
            target,
            landed_at: target,
        })
    }
}

pub(in crate::rt) fn prepared_seek(
    spec: AudioSpec,
    segment: SegmentId,
    target: Duration,
) -> PreparedSeek {
    use kithara_worker::Task;

    let (seeks, counts) = std::sync::mpsc::channel();
    let source = SeekSource {
        frame: 0,
        ready: false,
        spec,
        seeks,
    };
    let (mut node, receiver, mut lane) = crate::worker::terminal_node(source, spec, false);
    lane.send(
        When::Next,
        kithara_command::Batch {
            basis: Vec::new(),
            commands: vec![crate::LaneCommand::Segment {
                id: segment,
                from: target,
                speed: kithara_warp::SpeedCurve::Constant(1.0),
            }],
        },
    )
    .expect("control-side seek");
    for _ in 0..16 {
        node.tick();
    }
    assert_eq!(counts.try_iter().collect::<Vec<_>>(), vec![target]);
    assert_eq!(lane.receipts().count(), 1);
    let pcm = PlayerResource::new(PcmConsumer::new(receiver), Arc::from("split.mp3"), &pools())
        .expect("prepared PCM");
    (Box::new(pcm), node, lane, counts)
}

pub(in crate::rt) struct Control {
    pub ends: TestEnds,
    pub packets: Vec<PacketRing>,
}

pub(in crate::rt) fn processor(
    config: DeckMixerConfig,
    frames: u32,
    rate: NonZeroU32,
) -> (TestMixer, Control) {
    let (mixer, ends) = mixer_with_shape(
        config,
        64,
        StreamShape::new(NonZeroU32::new(frames).expect("fixture frames"), rate),
    );
    (
        mixer,
        Control {
            ends,
            packets: Vec::new(),
        },
    )
}

pub(in crate::rt) fn resource(
    input: &'static [u8],
    src: &str,
    seconds: f64,
    spec: AudioSpec,
) -> Resource {
    let frames = (seconds * f64::from(spec.sample_rate.get()))
        .floor()
        .to_usize()
        .expect("fixture frame count fits usize")
        .min(160_000);
    let samples = input
        .chunks_exact(4)
        .cycle()
        .take(frames * usize::from(spec.channels))
        .map(|bytes| f32::from_le_bytes(bytes.try_into().expect("sample bytes")))
        .collect::<Vec<_>>();
    let mut packets = PacketRing::new(spec, Duration::from_secs_f64(seconds), 8);
    let packet = chunk(spec, SegmentId::FIRST, 0, 0, &samples);
    let ended = f64::from(u32::try_from(frames).expect("fixture cap fits u32"))
        >= seconds * f64::from(spec.sample_rate.get()) - 1.0;
    packets.push(PcmPacket::Chunk(Box::new(packet)));
    if ended {
        let frame = u64::try_from(frames).expect("fixture frame count");
        let mut terminal = chunk(spec, SegmentId::FIRST, frame, frame, &[]);
        terminal.meta.end_of_track = true;
        packets.push(PcmPacket::Chunk(Box::new(terminal)));
    }
    from_packets(src, packets)
}

pub(in crate::rt) fn from_packets(src: &str, mut packets: PacketRing) -> Resource {
    let pcm = PlayerResource::new(
        PcmConsumer::new(packets.receiver.take().expect("receiver")),
        Arc::from(src),
        &pools(),
    )
    .expect("resource pools");
    (Box::new(pcm), packets)
}

pub(in crate::rt) fn attach(
    control: &mut Control,
    slot: Slot,
    resource: Resource,
    replace: bool,
) -> Seq {
    let (pcm, packets) = resource;
    control.packets.push(packets);
    push(
        control,
        if replace {
            DeckPart::Replace {
                slot,
                pcm,
                segment: SegmentId::FIRST,
            }
        } else {
            DeckPart::Attach {
                slot,
                pcm,
                segment: SegmentId::FIRST,
            }
        },
    )
}

pub(in crate::rt) fn push(control: &mut Control, part: DeckPart) -> Seq {
    send(&mut control.ends, When::Next, vec![part])
}

pub(in crate::rt) fn chain(control: &mut Control, from: Slot, to: Slot) -> Seq {
    send(
        &mut control.ends,
        When::Deferred,
        vec![DeckPart::Chain { from, to }],
    )
}

pub(in crate::rt) fn start(mixer: &mut TestMixer, slot: Slot) {
    mixer
        .mixer
        .deck
        .tracks
        .at_mut(slot)
        .expect("loaded slot")
        .start(Fade::Crossfade(CrossfadeSettings {
            duration: 0.0,
            ..Default::default()
        }));
}

pub(in crate::rt) fn count(mixer: &TestMixer) -> usize {
    mixer
        .mixer
        .deck
        .tracks
        .slots()
        .filter(|slot| mixer.track(*slot).is_some())
        .count()
}

pub(in crate::rt) fn render(mixer: &mut TestMixer, frames: usize) -> (bool, Vec<f32>, Vec<f32>) {
    let mut left = vec![99.0; frames];
    let mut right = vec![99.0; frames];
    let inputs = [];
    let mut outputs = [&mut left[..], &mut right[..]];
    let mut buffers = ProcBuffers {
        inputs: &inputs,
        outputs: &mut outputs,
    };
    mixer.inbox.0.drain();
    let level = mixer.inbox.0.scope(mixer.mixer.scope).expect("scope");
    let rate = mixer.mixer.deck.sample_rate;
    let context = RenderContext::new_linear(
        OutputContext::new(
            SessionFrame::new(0)..SessionFrame::new(frames as i64),
            rate,
            SessionEpoch::new(0),
            None,
        )
        .expect("output range"),
        None,
    )
    .expect("linear render");
    let rendered = mixer.mixer.render_block(
        Some(level),
        Some(&context),
        SessionFrame::new(0),
        &mut buffers,
        frames,
    );
    (rendered, left, right)
}

pub(in crate::rt) fn applied(
    control: &mut Control,
) -> Vec<kithara_command::Receipt<crate::bridge::DeckProtocol>> {
    std::iter::from_fn(|| control.ends.ring.receipt())
        .filter_map(|receipt| match receipt {
            ScopedReceipt::Scope(_, receipt) => Some(receipt),
            _ => None,
        })
        .collect()
}

pub(in crate::rt) fn slots(value: usize) -> DeckMixerConfig {
    DeckMixerConfig::builder()
        .slots(NonZeroUsize::new(value).expect("slots"))
        .build()
}
