#![cfg(not(target_arch = "wasm32"))]

use kithara_platform::sync::Arc;
use kithara_signal::SegmentId;
use kithara_test_utils::kithara;

use super::super::{PcmConsumer, PlayerResource, ReadOutcome};
use crate::{
    mock::pcm_fixture::{PcmFixture, chunk},
    test_pools::pools,
    worker::PcmPacket,
};

#[kithara::test(native, tokio)]
async fn epoch_validator_keeps_matching_chunks() {
    let mut fixture = PcmFixture::new(4, false).await;
    let receiver = fixture.receiver.take().expect("packet receiver");
    let mut resource = PlayerResource::new(PcmConsumer::new(receiver), Arc::from("test"), &pools())
        .expect("player resource");
    let segment = SegmentId::FIRST.next();
    resource.select_segment(segment);
    fixture
        .push(PcmPacket::Chunk(Box::new(chunk(segment, &[1.0, 2.0, 3.0]))))
        .expect("matching packet");
    let mut left = [0.0; 3];
    let mut right = [0.0; 3];
    let mut budget = 4;
    let result = resource.read(&mut [&mut left, &mut right], 0..3, &mut budget);
    assert!(matches!(result, ReadOutcome::Full { frames: 3 }));
    assert_eq!(left, [1.0, 2.0, 3.0]);
    assert_eq!(right, left);
}

#[kithara::test(native, tokio)]
async fn epoch_validator_rejects_stale_chunks_after_seek() {
    let mut fixture = PcmFixture::new(4, false).await;
    let receiver = fixture.receiver.take().expect("packet receiver");
    let mut resource = PlayerResource::new(PcmConsumer::new(receiver), Arc::from("test"), &pools())
        .expect("player resource");
    let old = SegmentId::FIRST;
    let current = old.next();
    fixture
        .push(PcmPacket::Chunk(Box::new(chunk(old, &[3.0]))))
        .expect("stale packet");
    fixture
        .push(PcmPacket::Chunk(Box::new(chunk(old, &[1.0]))))
        .expect("first packet");
    fixture
        .push(PcmPacket::Chunk(Box::new(chunk(current, &[2.0]))))
        .expect("current packet");
    resource.select_segment(current);
    let mut left = [-1.0; 1];
    let mut right = [-1.0; 1];
    let mut budget = 4;
    resource.read(&mut [&mut left, &mut right], 0..1, &mut budget);
    assert!(!left.contains(&1.0));
    assert!(!left.contains(&3.0));
    assert!(left.contains(&2.0));
    assert_eq!(right, left);
    assert!(fixture.returned().is_some());
    assert!(fixture.returned().is_some());
    assert!(fixture.returned().is_none());
}
