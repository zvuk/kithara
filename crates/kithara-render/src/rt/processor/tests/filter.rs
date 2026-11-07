use kithara_command::Target;
use kithara_config::ConfigOwner;
use kithara_effects::filter::{
    Cutoff, FilterConfig, FilterEffect, FilterError, FilterSettingsChange, Kind,
};

use super::*;
use crate::{
    bridge::{DeckFilterChange, FilterBinding, PlaybackFault},
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
    let context = RenderContext::new_linear(
        OutputContext::new(
            frame_at(start)..frame_at(start + frames),
            shape().sample_rate,
            SessionEpoch::new(0),
            None,
        )
        .expect("range"),
        None,
    )
    .expect("context");
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

pub(super) fn filter(cutoff: f64) -> FilterEffect {
    FilterEffect::new(
        AudioSpec::new(2, shape().sample_rate),
        FilterConfig::builder(pools())
            .kind(Kind::LowPass)
            .cutoff_hz(Cutoff::checked(cutoff).expect("cutoff"))
            .build()
            .expect("settings"),
    )
    .expect("prepared filter")
}

pub(super) fn part(binding: FilterBinding, change: DeckFilterChange) -> DeckPart {
    DeckPart::Filter { binding, change }
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
        PcmConsumer::new(ring.receiver.take().expect("receiver")),
        Arc::from("source format"),
        &pools(),
    )
    .expect("resource");
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
    let empty = binding(&mut ends);
    let installed = send(
        &mut ends,
        at(1),
        vec![part(
            empty,
            DeckFilterChange::Install(Box::new(filter(1_000.0))),
        )],
    );
    let mut reference = filter(1_000.0);
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
        assert_eq!(snapshot.filter.settings(), Some(*reference.config()));
        assert_eq!(
            snapshot.filter.binding().expect("installed").instance(),
            Some(installed)
        );
        assert_eq!(snapshot.metrics.underruns(), 0);
        assert_eq!(snapshot.metrics.event_overflows(), 0);
    }
    let replies = receipts(&mut ends);
    assert_eq!(replies.len(), 2);
    assert_applied_at(outcome_of(&replies, attached), 1);
    assert_applied_at(outcome_of(&replies, installed), 1);
    assert!(ends.deck.events.drain().next().is_none());
}

#[kithara::test]
fn filter_install_is_frame_exact_and_refusal_preserves_settings_and_history() {
    let (mut mixer, mut ends) = prepared();
    let empty = binding(&mut ends);
    let mut reference = filter(1_000.0);
    let settings = *reference.config();
    let installed = send(
        &mut ends,
        at(BLOCK + 5),
        vec![part(
            empty,
            DeckFilterChange::Install(Box::new(filter(1_000.0))),
        )],
    );
    let pcm = render_frames(&mut mixer, BLOCK, 32);
    let mut planes = [vec![1.0; 27], vec![1.0; 27]];
    reference
        .process(&mut planes, 0..27)
        .expect("reference prefix");
    let mut expected = vec![1.0; 5];
    expected.extend_from_slice(&planes[0]);
    assert_eq!(bits(&pcm), bits(&expected));
    assert_ne!(bits(&pcm), bits(&vec![1.0; 32]));
    let replies = receipts(&mut ends);
    assert_applied_at(outcome_of(&replies, installed), BLOCK + 5);
    assert!(matches!(
        &replies[0].2[..],
        [DeckPart::Returned(Returned::Filter(None))]
    ));
    let current = binding(&mut ends);
    let refused = send(
        &mut ends,
        at(BLOCK + 32),
        vec![part(
            current,
            DeckFilterChange::Configure(FilterSettingsChange::GainDb(GainDb::MAX)),
        )],
    );
    let stale = send(
        &mut ends,
        at(BLOCK + 32),
        vec![part(empty, DeckFilterChange::Clear)],
    );
    let pcm = render_frames(&mut mixer, BLOCK + 32, 29);
    let mut planes = [vec![1.0; 29], vec![1.0; 29]];
    reference
        .process(&mut planes, 0..29)
        .expect("reference continuation");
    assert_eq!(bits(&pcm), bits(&planes[0]));
    let replies = receipts(&mut ends);
    assert!(matches!(
        outcome_of(&replies, refused),
        Outcome::Rejected(Rejection::Refused(DeckRefusal::Filter(
            FilterError::Parameters
        )))
    ));
    assert!(matches!(
        outcome_of(&replies, stale),
        Outcome::Rejected(Rejection::Refused(DeckRefusal::FilterOutdated))
    ));
    let snapshot = ends.deck.snapshot.read();
    assert_eq!(snapshot.filter.settings(), Some(settings));
    assert_eq!(snapshot.filter.binding(), Some(current));
    assert_eq!(snapshot.metrics.underruns(), 0);
}

#[kithara::test]
fn clearing_returns_resident_filter_and_advances_instance_identity() {
    let (mut mixer, mut ends) = prepared();
    let empty = binding(&mut ends);
    send(
        &mut ends,
        at(BLOCK),
        vec![part(
            empty,
            DeckFilterChange::Install(Box::new(filter(1_000.0))),
        )],
    );
    render_frames(&mut mixer, BLOCK, 3);
    assert_eq!(receipts(&mut ends).len(), 1);
    let installed = binding(&mut ends);
    let cleared = send(
        &mut ends,
        at(BLOCK + 3),
        vec![part(installed, DeckFilterChange::Clear)],
    );
    assert_eq!(render_frames(&mut mixer, BLOCK + 3, 7), vec![1.0; 7]);
    let replies = receipts(&mut ends);
    assert_applied_at(outcome_of(&replies, cleared), BLOCK + 3);
    assert!(
        matches!(&replies[0].2[..], [DeckPart::Returned(Returned::Filter(Some(filter)))] if f64::from(filter.config().cutoff_hz()) == 1_000.0)
    );
    let snapshot = ends.deck.snapshot.read();
    assert_eq!(snapshot.filter.settings(), None);
    assert_eq!(
        snapshot
            .filter
            .binding()
            .expect("cleared instance")
            .instance(),
        Some(cleared)
    );
    assert_ne!(snapshot.filter.binding(), Some(empty));
    assert_ne!(snapshot.filter.binding(), Some(installed));
}

#[kithara::test]
fn unsupported_new_nyquist_is_a_visible_terminal_render_failure() {
    let (mut mixer, mut ends) = prepared();
    let empty = binding(&mut ends);
    let expected = *filter(18_000.0).config();
    send(
        &mut ends,
        at(BLOCK),
        vec![part(
            empty,
            DeckFilterChange::Install(Box::new(filter(18_000.0))),
        )],
    );
    render_frames(&mut mixer, BLOCK, 3);
    assert_eq!(receipts(&mut ends).len(), 1);
    mixer
        .mixer
        .update_sample_rate(NonZeroU32::new(32_000).expect("new output rate"));
    assert_eq!(render_frames(&mut mixer, BLOCK + 3, 7), vec![0.0; 7]);
    let events: Vec<_> = ends.deck.events.drain().collect();
    assert!(
        matches!(&events[..], [DeckEvent::Failed { slot, at, fault: PlaybackFault::Source(kithara_audio::TrackFailureKind::Render) }] if *slot == A && *at == frame_at(BLOCK + 3))
    );
    let snapshot = ends.deck.snapshot.read();
    assert_eq!(snapshot.slots[A.index()].state, SlotState::Ended);
    assert_eq!(snapshot.filter.settings(), Some(expected));
    assert_eq!(snapshot.sample_rate, 32_000);
    assert_eq!(snapshot.metrics.underruns(), 0);
    assert_eq!(snapshot.metrics.event_overflows(), 0);
}
