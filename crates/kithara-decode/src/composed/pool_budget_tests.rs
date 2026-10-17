use std::num::NonZeroU32;

use kithara_platform::time::Duration;
use kithara_signal::AudioSpec;
use kithara_test_fixtures::unit_fixtures::trim_silence;
use kithara_test_utils::kithara;

use super::test_stub_codec::ConstFrameCodec;
use crate::{codec::FrameCodec, test_pools::pools};

#[kithara::test]
fn codec_warm_pool_keeps_allocated_bytes_stable(trim_silence: Vec<f32>) {
    let pools = pools();
    for _ in 0..4 {
        let mut buf = pools.get::<f32>();
        buf.ensure_len(2048).unwrap();
    }
    let warmup_bytes = pools.stats().allocated_bytes;

    let mut codec = ConstFrameCodec::new(
        trim_silence.clone(),
        AudioSpec::new(2, NonZeroU32::new(44_100).expect("test rate")),
        1024,
    );
    for _ in 0..200 {
        let mut buf = pools.get::<f32>();
        let frames = codec
            .decode_frame(
                crate::demuxer::Frame {
                    data: &[],
                    pts: Duration::ZERO,
                    duration: Duration::ZERO,
                    packet_desc: &[],
                },
                crate::demuxer::PacketMetadata::default(),
                &mut buf,
            )
            .expect("BUG: decode_frame")
            .frames;
        assert_eq!(frames, 1024);
    }

    assert_eq!(pools.stats().allocated_bytes, warmup_bytes);
}
