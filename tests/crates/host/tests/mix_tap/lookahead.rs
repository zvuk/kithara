//! The session limiter's lookahead, seen through the master tap.
//!
//! The deck plays the 4 410-frame half-scale constant, below the default
//! ceiling, so the limiter keeps every sample's bits and the takes differ only
//! by the delay. Under the realtime sanitizer the same case runs the delayed
//! limiter on the audio callback.

use std::num::NonZeroU32;

use kithara::{
    effects::{LimiterConfig, LookaheadMs},
    host::{HostConfig, HostSettings, Tap},
};
use kithara_integration_tests::offline::{OfflinePlayer, OfflinePlayerOptions, asset_source};
use kithara_test_fixtures::assets;

use super::{
    ROOMY_CAPACITY,
    cases::{BLOCK_FRAMES, SAMPLE_RATE},
    load_source, render_blocks,
};

/// Blocks each take renders: the deck, its delayed tail and silence past it.
const BLOCKS: usize = 20;
const CHANNELS: usize = 2;

/// What graph_out plays over [`BLOCKS`] blocks from the deck's first, under
/// `limiter`, once the master tap has shown it carries the same samples.
async fn take(limiter: LimiterConfig) -> Vec<f32> {
    let rate = NonZeroU32::new(SAMPLE_RATE).expect("nonzero fixture rate");
    let session = HostConfig::offline(kithara_test_utils::bufpool::pools())
        .settings(HostSettings::builder().sample_rate(rate).build())
        .limiter(limiter)
        .build();
    let harness = OfflinePlayer::with_options(
        OfflinePlayerOptions::builder()
            .block_on_underrun(true)
            .build(),
        session,
    )
    .await;
    let harness = load_source(
        harness,
        asset_source(&assets::fixed_constant_wav_cd_half_4410()).into(),
    )
    .await;
    let mut tap = harness
        .host()
        .attach_tap(Tap::Master, ROOMY_CAPACITY)
        .await
        .expect("enable mix tap");
    let rendered = render_blocks(&harness, BLOCKS).await;
    assert_eq!(
        tap.drain(),
        rendered,
        "the master tap carries what graph_out plays"
    );
    assert_eq!(tap.drops(), 0, "the roomy tap drops nothing");
    harness.close().await;
    rendered
}

fn bits(samples: &[f32]) -> Vec<u32> {
    samples.iter().copied().map(f32::to_bits).collect()
}

#[kithara::test(tokio)]
async fn the_master_tap_carries_the_mix_the_lookahead_later() {
    let rate = NonZeroU32::new(SAMPLE_RATE).expect("nonzero fixture rate");
    let lookahead = LimiterConfig::builder()
        .lookahead_ms(LookaheadMs::try_from(10.0).expect("limiter lookahead"))
        .build();
    let delay = lookahead.latency_frames(rate) * CHANNELS;
    let block = BLOCK_FRAMES * CHANNELS;

    let direct = take(LimiterConfig::default()).await;
    let delayed = take(lookahead).await;

    let end = direct
        .iter()
        .rposition(|sample| *sample != 0.0)
        .expect("the take holds the deck")
        + 1;
    let tail = (end + delay - 1) / block;
    assert!(
        direct[tail * block..(tail + 1) * block]
            .iter()
            .all(|sample| *sample == 0.0),
        "the deck's last samples leave the delay in a block the deck no longer feeds"
    );
    assert!(
        delayed[(tail + 1) * block..]
            .iter()
            .all(|sample| *sample == 0.0),
        "the drained delay leaves exact silence after the deck"
    );
    assert!(
        delayed[..delay].iter().all(|sample| *sample == 0.0),
        "the first lookahead frames are silent pre-roll"
    );
    assert_eq!(
        bits(&delayed[delay..]),
        bits(&direct[..direct.len() - delay]),
        "the mix leaves the lookahead later bit for bit, its last samples once"
    );
}
