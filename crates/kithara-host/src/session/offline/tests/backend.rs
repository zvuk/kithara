use std::num::NonZeroU32;

use firewheel::{FirewheelConfig, FirewheelContext, channel_config::ChannelCount};
use kithara_platform::time::Duration;
use kithara_test_utils::kithara;

use crate::session::offline::{
    backend::{BackendConfig, OfflineStream},
    task::consts::CHANNELS,
};

#[kithara::test]
fn start_hands_firewheel_the_configured_block_latency_and_rate() {
    let block_frames = NonZeroU32::new(127).expect("fixture block frames");
    let declared_latency = Duration::from_millis(7);
    let sample_rate = NonZeroU32::new(48_000).expect("fixture sample rate");
    let mut ctx = FirewheelContext::new(FirewheelConfig {
        num_graph_outputs: ChannelCount::STEREO,
        ..FirewheelConfig::default()
    });

    let config = BackendConfig::builder()
        .block_frames(block_frames)
        .declared_latency(declared_latency)
        .sample_rate(sample_rate)
        .build();
    let _stream = OfflineStream::start(&mut ctx, config).expect("fixture offline stream");

    let stream = ctx
        .stream_info()
        .expect("an activated context has a stream");
    assert_eq!(stream.max_block_frames, block_frames);
    assert_eq!(stream.sample_rate, sample_rate);
    assert_eq!(
        stream.input_to_output_latency_seconds,
        declared_latency.as_secs_f64()
    );
    assert_eq!(
        stream.num_stream_out_channels,
        u32::try_from(CHANNELS).expect("stereo channel count fits u32")
    );
}

#[kithara::test]
fn render_fills_the_requested_block() {
    let block_frames = NonZeroU32::new(64).expect("fixture block frames");
    let sample_rate = NonZeroU32::new(48_000).expect("fixture sample rate");
    let mut ctx = FirewheelContext::new(FirewheelConfig {
        num_graph_outputs: ChannelCount::STEREO,
        ..FirewheelConfig::default()
    });
    let config = BackendConfig::builder()
        .block_frames(block_frames)
        .declared_latency(Duration::ZERO)
        .sample_rate(sample_rate)
        .build();
    let mut stream = OfflineStream::start(&mut ctx, config).expect("fixture offline stream");

    let frames = 16;
    let mut output = vec![f32::NAN; frames * CHANNELS];
    stream
        .render(0, frames, &mut output)
        .expect("the offline stream renders a block on demand");

    assert!(
        output.iter().all(|sample| sample.is_finite()),
        "every requested frame is written"
    );
}
