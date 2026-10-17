use super::{
    core::{AudioSpec, NonZeroU32, chunk, kithara},
    *,
};

#[kithara::test]
#[case::descending(717, 0)]
#[case::ascending(0, 717)]
fn musical_origin_follows_consumed_pcm_instead_of_the_next_producer_packet(
    #[case] old_origin: u64,
    #[case] next_origin: u64,
) {
    use crate::{test_pools::pools, worker::packet_tests::PacketRing};
    let spec = AudioSpec::new(2, NonZeroU32::new(1_000).expect("millisecond clock"));
    let mut ring = PacketRing::new(spec, Duration::from_secs(2), 4);
    let mut old = chunk(spec, SegmentId::FIRST, 0, old_origin + 500, &[0.25; 8]);
    old.meta.media_origin = Some(Duration::from_millis(old_origin));
    old.meta.visible_origin = Some(Duration::from_millis(old_origin + 10));
    let mut next = chunk(spec, SegmentId::FIRST, 4, next_origin + 504, &[0.75; 8]);
    next.meta.media_origin = Some(Duration::from_millis(next_origin));
    next.meta.visible_origin = Some(Duration::from_millis(next_origin + 10));
    ring.push(PcmPacket::Chunk(Box::new(old)));
    ring.push(PcmPacket::Chunk(Box::new(next)));
    let mut resource = PlayerResource::new(
        PcmConsumer::new(ring.receiver.take().expect("receiver")),
        Arc::from("musical-origin"),
        &pools(),
    )
    .expect("resource");
    let prepared = resource.next_chunk_info().expect("admitted preload packet");
    assert_eq!(
        prepared.media_origin,
        Some(Duration::from_millis(old_origin))
    );
    assert_eq!(
        prepared.visible_origin,
        Some(Duration::from_millis(old_origin + 10))
    );
    assert_eq!(resource.media_origin(), None);
    assert_eq!(resource.visible_origin(), None);
    assert!(resource.mark(SessionFrame::new(0)).is_none());
    let mut budget = 8;
    resource.refresh_mark(&mut budget);
    assert_eq!(
        resource.media_origin(),
        Some(Duration::from_millis(old_origin))
    );
    let mut left = [0.0; 1];
    let mut right = [0.0; 1];
    resource.read(None, &mut [&mut left, &mut right], 0..1, &mut budget);
    assert_eq!(left, [0.25]);
    assert_eq!(right, left);
    assert_eq!(
        resource.consumer.get().receiver.media_origin(),
        Some(Duration::from_millis(next_origin)),
    );
    let prepared = resource.next_chunk_info().expect("next admitted packet");
    assert_eq!(
        prepared.media_origin,
        Some(Duration::from_millis(next_origin))
    );
    assert_eq!(
        prepared.visible_origin,
        Some(Duration::from_millis(next_origin + 10))
    );
    assert_eq!(
        resource.media_origin(),
        Some(Duration::from_millis(old_origin))
    );
    assert_eq!(
        resource
            .mark(SessionFrame::new(1))
            .expect("old mark")
            .position,
        Duration::from_millis(old_origin + 501),
    );

    let mut left = [0.0; 4];
    let mut right = [0.0; 4];
    resource.read(None, &mut [&mut left, &mut right], 0..4, &mut budget);
    assert_eq!(left, [0.25, 0.25, 0.25, 0.75]);
    assert_eq!(right, left);
    assert_eq!(
        resource.media_origin(),
        Some(Duration::from_millis(next_origin))
    );
    assert_eq!(
        resource
            .mark(SessionFrame::new(5))
            .expect("new mark")
            .position,
        Duration::from_millis(next_origin + 505),
    );
}
