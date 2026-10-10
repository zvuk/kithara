use std::num::NonZeroU32;

use kithara_test_utils::{bufpool::TestPools, kithara};

use super::*;

#[kithara::test]
fn realtime_config_preserves_output_block_default_and_allows_override() {
    let default = HostConfig::<TestPools>::builder().build();
    let HostConfig::Realtime {
        output_block_frames,
        ..
    } = default
    else {
        panic!("default Host config must be realtime");
    };
    assert_eq!(output_block_frames, None);

    let frames = NonZeroU32::new(128).expect("test block size is non-zero");
    let configured = HostConfig::<TestPools>::builder()
        .output_block_frames(frames)
        .build();
    let HostConfig::Realtime {
        output_block_frames,
        ..
    } = configured
    else {
        panic!("realtime builder must create realtime config");
    };
    assert_eq!(output_block_frames, Some(frames));
}

#[kithara::test]
fn host_root_owns_the_configured_sample_rate() {
    let sample_rate = NonZeroU32::new(48_000).expect("test sample rate is non-zero");
    let config = HostConfig::<TestPools>::builder()
        .settings(HostSettings::builder().sample_rate(sample_rate).build())
        .build();
    let root = Host::<TestPools>::session_root(config.settings()).expect("host root");

    assert_eq!(root.view.grid().axis().sample_rate(), sample_rate);
}
