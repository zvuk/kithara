use kithara_signal::{SegmentId, SessionFrame};

use super::*;

#[kithara::test]
fn snapshot_frontier_never_trails_position() {
    let spec = AudioSpec::new(2, NonZeroU32::new(1_000).expect("millisecond rate"));
    let mut packets = PacketRing::new(spec, Duration::from_secs(1), 4);
    packets.push(PcmPacket::Chunk(Box::new(chunk(
        spec,
        SegmentId::FIRST,
        0,
        917,
        &[0.5; 2],
    ))));
    packets.push(PcmPacket::Chunk(Box::new(chunk(
        spec,
        SegmentId::FIRST,
        1,
        656,
        &[0.5; 2],
    ))));
    let pcm = PlayerResource::new(
        PcmConsumer::new(packets.receiver.take().expect("receiver")),
        Arc::from("frontier"),
        &pools(),
    )
    .expect("resource");
    let (mut mixer, mut ends) = mixer();
    send(
        &mut ends,
        When::Next,
        vec![DeckPart::Attach {
            item_id: kithara_events::TrackId::allocate(),
            slot: A,
            pcm: Box::new(pcm),
            segment: SegmentId::FIRST,
        }],
    );
    assert!(crate::mock::apply_initial_batch(
        &mut mixer.mixer,
        &mut mixer.inbox.0
    ));
    let track = mixer.mixer.deck.tracks.at_mut(A).expect("loaded slot");
    track.recycle_obsolete(&mut 8);
    assert_eq!(track.position(), 0.917);
    assert_eq!(track.decoded_frontier(), 0.657);
    mixer.mixer.publish(SessionFrame::new(0));

    let snap = ends.deck.snapshot.read().slots[0];
    assert!(
        snap.frontier >= snap.position,
        "frontier {} must cover position {}",
        snap.frontier,
        snap.position
    );
}

#[kithara::test]
fn processor_fade_in_restarts_track_from_zero() {
    let (mut mixer, mut ends) = mixer();
    let segment = SegmentId::FIRST.next();
    let (pcm, _worker, _lane, seeks) = legacy_fixture::prepared_seek(
        AudioSpec::new(2, legacy_fixture::RATE),
        segment,
        Duration::from_secs(12),
    );
    send(
        &mut ends,
        When::Next,
        vec![DeckPart::Attach {
            item_id: kithara_events::TrackId::allocate(),
            slot: A,
            pcm,
            segment,
        }],
    );
    assert!(crate::mock::apply_initial_batch(
        &mut mixer.mixer,
        &mut mixer.inbox.0
    ));
    let track = mixer.mixer.deck.tracks.at_mut(A).expect("loaded slot");
    track.recycle_obsolete(&mut 8);
    assert!(track.position() >= 11.9);
    assert_eq!(track.position(), 12.0);

    send(
        &mut ends,
        When::Next,
        vec![DeckPart::Start {
            slot: A,
            fade: Fade::Crossfade(CrossfadeSettings::default()),
        }],
    );
    assert!(crate::mock::apply_initial_batch(
        &mut mixer.mixer,
        &mut mixer.inbox.0
    ));
    let track = mixer.track(A).expect("track must remain loaded");
    assert_eq!(
        track.position(),
        12.0,
        "starting with a fade must never seek"
    );
    assert!(seeks.try_iter().next().is_none(), "no hidden seek(0)");
}
#[kithara::test]
fn snapshot_carries_the_owner_origin_without_rewriting_raw_pcm_position() {
    let spec = AudioSpec::new(2, NonZeroU32::new(1_000).expect("millisecond source"));
    let mut packets = PacketRing::with_origin(
        spec,
        Duration::from_millis(952),
        4,
        Duration::from_millis(717),
    );
    let data = chunk(spec, SegmentId::FIRST, 0, 1_500, &[0.5; 338]);
    packets.push(PcmPacket::Chunk(Box::new(data)));
    let pcm = PlayerResource::new(
        PcmConsumer::new(packets.receiver.take().expect("receiver")),
        Arc::from("offset"),
        &pools(),
    )
    .expect("resource");
    let (mut mixer, mut ends) = mixer();
    send(
        &mut ends,
        When::Next,
        vec![DeckPart::Attach {
            item_id: kithara_events::TrackId::allocate(),
            slot: A,
            pcm: Box::new(pcm),
            segment: SegmentId::FIRST,
        }],
    );
    assert!(crate::mock::apply_initial_batch(
        &mut mixer.mixer,
        &mut mixer.inbox.0
    ));
    let track = mixer.mixer.deck.tracks.at_mut(A).expect("loaded slot");
    track.recycle_obsolete(&mut 8);
    assert_eq!(track.position(), 1.5);
    mixer.mixer.publish(SessionFrame::new(0));
    let snapshot = ends.deck.snapshot.read().slots[0];
    assert_eq!(snapshot.position, 1.5);
    assert_eq!(
        snapshot.mark.expect("physical mark").position,
        Duration::from_millis(1_500)
    );
    let source = snapshot.source.expect("observed source facts");
    assert_eq!(source.visible_origin, Some(Duration::from_millis(717)));
    assert_eq!(source.media_end, Some(Duration::from_millis(1_669)));
    assert_eq!(snapshot.frontier, 1.669);
}
