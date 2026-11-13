use kithara_command::Target;
use kithara_effects::filter::{FilterEffect, FilterKnob, FilterSettings};

use super::*;
use crate::{
    bridge::{DeckMixSettings, DeckMixSettingsChange, FilterBinding},
    worker::packet_tests::{PacketRing, chunk},
};

fn stereo_frames(mixer: &mut TestMixer, start: usize, frames: usize) -> [Vec<f32>; 2] {
    let mut pcm = [vec![0.0; frames], vec![0.0; frames]];
    let [left, right] = &mut pcm;
    let inputs = [];
    let mut outputs = [&mut left[..], &mut right[..]];
    let mut buffers = ProcBuffers {
        inputs: &inputs,
        outputs: &mut outputs,
    };
    mixer.inbox.0.drain();
    let level = mixer.inbox.scope(mixer.mixer.scope).expect("live scope");
    let context = OutputContext::new(
        frame_at(start)..frame_at(start + frames),
        shape().sample_rate,
        SessionEpoch::new(0),
        None,
    )
    .expect("output range");
    mixer.mixer.render_block(
        Some(level),
        Some(&context),
        frame_at(start),
        &mut buffers,
        frames,
    );
    pcm
}

pub(super) fn prepared() -> (TestMixer, TestEnds) {
    let (mut mixer, mut ends) = mixer_with_config(DeckMixerConfig::default(), 32);
    start_dc(&mut mixer, &mut ends, &[A]);
    (mixer, ends)
}

pub(super) fn knob(value: f64) -> FilterKnob {
    FilterKnob::checked(value).expect("knob position")
}

/// The deck filter at `rate`, moved to `start` at once.
pub(super) fn reference(rate: NonZeroU32, start: FilterKnob) -> FilterEffect {
    let mut filter = FilterEffect::new(rate, FilterSettings::default());
    filter.configure(start);
    filter.reset();
    filter
}

pub(super) fn turn(value: f64) -> DeckPart {
    DeckPart::Mix(DeckMixSettingsChange::Filter(knob(value)))
}

pub(super) fn binding(ends: &mut TestEnds) -> FilterBinding {
    ends.deck
        .snapshot
        .read()
        .filter
        .binding()
        .expect("observed output stream")
}

pub(super) fn bits(pcm: &[f32]) -> Vec<u32> {
    pcm.iter().map(|sample| sample.to_bits()).collect()
}

#[kithara::test]
#[case::rate(2, 48_000, 128)]
#[case::rate_split(2, 48_000, 127)]
#[case::channels(1, 44_100, 128)]
#[case::channels_split(1, 44_100, 127)]
#[case::rate_and_channels(1, 48_000, 128)]
#[case::rate_and_channels_split(1, 48_000, 127)]
fn source_format_changes_preserve_pcm_metadata_and_the_deck_filter_rate(
    #[case] channels: u16,
    #[case] rate: u32,
    #[case] prefix: usize,
) {
    let (mut mixer, mut ends) = mixer_with_config(DeckMixerConfig::default(), 32);
    assert_eq!(stereo_frames(&mut mixer, 0, 1), [vec![0.0], vec![0.0]]);
    let initial = AudioSpec::new(2, shape().sample_rate);
    let changed = AudioSpec::new(channels, NonZeroU32::new(rate).expect("source rate"));
    let mut ring = PacketRing::new(initial, Duration::from_secs(1), 4);
    let mut metadata = Vec::new();
    for (index, spec, value) in [
        (0_u64, initial, 0.25),
        (1, changed, -0.125),
        (2, changed, 0.0625),
    ] {
        let mut packet = chunk(
            spec,
            SegmentId::FIRST,
            index * 128,
            index * 128,
            &vec![value; usize::from(spec.channels) * 128],
        );
        packet.meta.source_revision = if index == 0 { 7 } else { 8 };
        packet.meta.visible_origin = Some(Duration::ZERO);
        packet.meta.media_end = Some(Duration::from_secs(1));
        packet.meta.source_byte_offset = Some(1024 + index * 256);
        packet.meta.segment_index = Some(9);
        packet.meta.variant_index = Some(3);
        metadata.push(packet.meta);
        ring.push(PcmPacket::Chunk(Box::new(packet)));
    }
    let resource = PlayerResource::new(
        ring.receiver.take().expect("receiver"),
        Arc::from("source format"),
    );
    let attached = send(
        &mut ends,
        at(1),
        vec![
            DeckPart::Attach {
                slot: A,
                item_id: kithara_events::TrackId::allocate(),
                pcm: Box::new(resource),
                segment: SegmentId::FIRST,
            },
            DeckPart::Start {
                slot: A,
                fade: Fade::Crossfade(
                    CrossfadeSettings::new(0.0, CrossfadeCurve::Linear, 1.0, 0.5)
                        .expect("instant start"),
                ),
            },
        ],
    );
    let turned = send(&mut ends, at(1), vec![turn(-0.5)]);
    let mut reference = reference(initial.sample_rate, FilterKnob::DEFAULT);
    reference.configure(knob(-0.5));
    for (index, (meta, value)) in metadata
        .iter()
        .copied()
        .zip([0.25, -0.125, 0.0625])
        .enumerate()
    {
        let start = 1 + index * 128;
        let mut actual = stereo_frames(&mut mixer, start, prefix);
        if prefix < 128 {
            {
                let snapshot = ends.deck.snapshot.read();
                assert_eq!(snapshot.slots[A.index()].source, Some(meta));
            }
            let last = stereo_frames(&mut mixer, start + prefix, 128 - prefix);
            for (output, suffix) in actual.iter_mut().zip(last) {
                output.extend(suffix);
            }
        }
        let mut expected = [vec![value; 128], vec![value; 128]];
        reference
            .process(&mut expected, 0..128)
            .expect("continuous output-rate reference");
        assert_eq!(
            actual.each_ref().map(|plane| bits(plane)),
            expected.each_ref().map(|plane| bits(plane))
        );
        let snapshot = ends.deck.snapshot.read();
        assert_eq!(
            snapshot.slots[A.index()].source,
            Some(metadata.get(index + 1).copied().unwrap_or(meta))
        );
        assert_eq!(snapshot.sample_rate, initial.sample_rate.get());
        assert_eq!(snapshot.filter.knob().to_bits(), reference.knob().to_bits());
        assert_eq!(snapshot.metrics.underruns(), 0);
        assert_eq!(snapshot.metrics.event_overflows(), 0);
    }
    let replies = receipts(&mut ends);
    assert_eq!(replies.len(), 2);
    assert_applied_at(outcome_of(&replies, attached), 1);
    assert_applied_at(outcome_of(&replies, turned), 1);
    assert!(ends.deck.events.drain().next().is_none());
}

#[kithara::test]
fn a_knob_turn_is_frame_exact_after_a_bit_transparent_center() {
    let (mut mixer, mut ends) = prepared();
    let turned = send(&mut ends, at(BLOCK + 5), vec![turn(0.75)]);
    let pcm = render_frames(&mut mixer, BLOCK, 32);
    let mut filter = reference(shape().sample_rate, FilterKnob::DEFAULT);
    filter.configure(knob(0.75));
    let mut planes = [vec![1.0; 27], vec![1.0; 27]];
    filter.process(&mut planes, 0..27).expect("reference turn");
    let mut expected = vec![1.0; 5];
    expected.extend_from_slice(&planes[0]);
    assert_eq!(bits(&pcm), bits(&expected));
    assert_ne!(bits(&pcm[5..]), bits(&[1.0; 27]));
    let replies = receipts(&mut ends);
    assert_applied_at(outcome_of(&replies, turned), BLOCK + 5);
    let snapshot = ends.deck.snapshot.read();
    assert_eq!(snapshot.filter.authority(), Some(turned));
    assert_eq!(snapshot.filter.program(), None);
    assert_eq!(snapshot.filter.knob().to_bits(), filter.knob().to_bits());
    assert_eq!(
        snapshot.filter.binding().map(|binding| binding.instance()),
        Some(None)
    );
}

/// The knob map holds the cutoff below Nyquist, so no output rate stops the
/// deck or moves its knob.
#[kithara::test]
#[case::rate_32k(32_000)]
#[case::rate_1k(1_000)]
fn low_output_rates_keep_the_deck_playing_at_every_knob_position(#[case] rate: u32) {
    for position in [-1.0, 0.0, 1.0] {
        let mix = DeckMixSettings::builder().filter(knob(position)).build();
        let (mut mixer, mut ends) =
            mixer_with_config(DeckMixerConfig::builder().mix(mix).build(), 32);
        start_dc(&mut mixer, &mut ends, &[A]);
        mixer
            .mixer
            .update_sample_rate(NonZeroU32::new(rate).expect("output rate"));
        let pcm = render_frames(&mut mixer, BLOCK, BLOCK);
        assert!(
            pcm.iter().all(|sample| sample.is_finite()),
            "knob {position} at {rate} Hz"
        );
        if position == 0.0 {
            assert_eq!(bits(&pcm), bits(&[1.0; BLOCK]), "transparent center");
        }
        assert!(ends.deck.events.drain().next().is_none());
        let snapshot = ends.deck.snapshot.read();
        assert_eq!(snapshot.slots[A.index()].state, SlotState::Playing);
        assert_eq!(snapshot.sample_rate, rate);
        assert_eq!(snapshot.filter.knob(), position);
    }
}
