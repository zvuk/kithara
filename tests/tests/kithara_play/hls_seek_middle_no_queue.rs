#![forbid(unsafe_code)]

use kithara::{
    abr::AbrMode,
    assets::StoreOptions,
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        time::{Duration, sleep},
        tokio::task::yield_now,
    },
    play::{Resource, ResourceConfig},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_integration_tests::{
    PackagedTestServer, SegmentGateHandle, fixture_protocol::DelayRule, offline::OfflinePlayer,
    temp_dir,
};

use crate::common::test_defaults::Consts as Shared;

struct Consts;
impl Consts {
    const SAMPLE_RATE: u32 = Shared::SAMPLE_RATE;
    const BLOCK_FRAMES: usize = Shared::OFFLINE_BLOCK_FRAMES;
    const PRE_SEEK_RENDER_SECS: f64 = 1.5;
    /// Minimum seconds of audio the render loop pumps after the seek
    /// before checking the landing. The loop returns on the actual
    /// position state, not wall time, so the delayed segment fetch is
    /// awaited regardless of `delay_ms`.
    const POST_SEEK_AUDIO_SECS: f64 = 2.0;
    /// Target inside segment 2 (8–12 s); guarantees a cold fetch
    /// because the warmup only covers segments 0–1.
    const SEEK_TARGET_SECS: f64 = 9.0;
    const MIN_POSITION_ADVANCE_POST_SEEK_SECS: f64 = 1.0;
    const GATED_VARIANT: usize = 0;
    const GATED_SEGMENT: usize = 2;
    const GATE_REQUEST_TICKS: u32 = 2_000;
}

#[derive(Clone, Copy, Debug)]
enum SeekScenario {
    NoDelay,
    RealDelay {
        delay_ms: u64,
    },
    Gated {
        label: &'static str,
        nominal_delay_ms: u64,
        hold_ticks: u32,
    },
}

impl SeekScenario {
    fn label(self) -> &'static str {
        match self {
            Self::NoDelay => "no_delay",
            Self::RealDelay { delay_ms: 2_000 } => "congested_2s",
            Self::RealDelay { .. } => "real_delay",
            Self::Gated { label, .. } => label,
        }
    }

    fn nominal_delay_ms(self) -> u64 {
        match self {
            Self::NoDelay => 0,
            Self::RealDelay { delay_ms } => delay_ms,
            Self::Gated {
                nominal_delay_ms, ..
            } => nominal_delay_ms,
        }
    }
}

fn blocks_for_seconds(secs: f64) -> u32 {
    let blocks = (secs * f64::from(Consts::SAMPLE_RATE) / Consts::BLOCK_FRAMES as f64).ceil();
    #[expect(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "positive ceiling fits in u32"
    )]
    let result = blocks as u32;
    result
}

/// Render at least `min_blocks` blocks and return only once
/// `player.position() >= until_position`. Between batches it yields a
/// single virtual tick so the async HLS engine can fetch + decode the
/// delayed segment; under the flash clock that tick advances virtual
/// time without burning real wall time. The loop has no internal wall
/// budget — the `#[kithara::test(... timeout(90s))]` attribute is the
/// only safety bound, so the success path is always the real state
/// (position) being reached rather than a collapsed timer expiring.
#[kithara::flash(true)]
async fn render_until_position(player: &mut OfflinePlayer, min_blocks: u32, until_position: f64) {
    const BATCH: u32 = 16;
    const TICK_MS: u64 = 25;
    let mut rendered = 0u32;
    loop {
        let this = min_blocks.saturating_sub(rendered).clamp(1, BATCH);
        for _ in 0..this {
            let _ = player.render(Consts::BLOCK_FRAMES);
        }
        rendered = rendered.saturating_add(this);
        if player.position() >= until_position && rendered >= min_blocks {
            return;
        }
        sleep(Duration::from_millis(TICK_MS)).await;
    }
}

#[kithara::flash(true)]
async fn render_until_gate_requested(
    player: &mut OfflinePlayer,
    gate: &SegmentGateHandle,
    post_target: f64,
    hold_ticks: u32,
    label: &str,
) {
    const BATCH: u32 = 16;
    for _ in 0..Consts::GATE_REQUEST_TICKS {
        for _ in 0..BATCH {
            let _ = player.render(Consts::BLOCK_FRAMES);
        }
        let position = player.position();
        assert!(
            position < post_target,
            "{label}: seek advanced to {position:.3}s while gated segment was unavailable \
             (post target {post_target:.3}s)"
        );
        if gate.requested() > 0 {
            for _ in 0..hold_ticks {
                for _ in 0..BATCH {
                    let _ = player.render(Consts::BLOCK_FRAMES);
                }
                let held_position = player.position();
                assert!(
                    held_position < post_target,
                    "{label}: seek advanced to {held_position:.3}s before gate release \
                     (post target {post_target:.3}s)"
                );
                sleep(Duration::from_millis(1)).await;
            }
            return;
        }
        yield_now().await;
    }
    panic!(
        "{label}: gated segment v{} s{} was never requested before release",
        Consts::GATED_VARIANT,
        Consts::GATED_SEGMENT
    );
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(90)))]
#[case::no_delay(SeekScenario::NoDelay)]
#[case::good_4g_500ms(SeekScenario::Gated {
    label: "good_4g_500ms_gate",
    nominal_delay_ms: 500,
    hold_ticks: 4,
})]
#[case::congested_2s(SeekScenario::RealDelay { delay_ms: 2_000 })]
#[case::very_slow_5s(SeekScenario::Gated {
    label: "very_slow_5s_gate",
    nominal_delay_ms: 5_000,
    hold_ticks: 8,
})]
#[case::near_broken_10s(SeekScenario::Gated {
    label: "near_broken_10s_gate",
    nominal_delay_ms: 10_000,
    hold_ticks: 16,
})]
async fn hls_seek_middle_lands_under_simulated_slow_connection(#[case] scenario: SeekScenario) {
    let (server, gate) = match scenario {
        SeekScenario::NoDelay => (PackagedTestServer::new().await, None),
        SeekScenario::RealDelay { delay_ms } => (
            PackagedTestServer::with_delay_rules(vec![DelayRule {
                variant: None,
                segment_eq: None,
                segment_gte: Some(Consts::GATED_SEGMENT),
                delay_ms,
            }])
            .await,
            None,
        ),
        SeekScenario::Gated { .. } => {
            let (server, gate) =
                PackagedTestServer::with_segment_gate(Consts::GATED_VARIANT, Consts::GATED_SEGMENT)
                    .await;
            (server, Some(gate))
        }
    };
    let delay_ms = scenario.nominal_delay_ms();
    let label = scenario.label();
    let master = server.url("/master.m3u8");

    let temp = temp_dir();
    let store = StoreOptions::new(temp.path());
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(NetOptions::default(), CancelToken::never()))
            .build(),
    );

    let cfg = {
        let builder = ResourceConfig::for_src(master.as_str())
            .expect("valid master URL")
            .byte_pool(kithara::bufpool::BytePool::default())
            .pcm_pool(kithara::bufpool::PcmPool::default())
            .downloader(downloader.clone())
            .name("t0".to_string())
            .store(store);
        if gate.is_some() {
            builder
                .initial_abr_mode(AbrMode::manual(Consts::GATED_VARIANT))
                .build()
        } else {
            builder.build()
        }
    };

    let resource = Resource::new(cfg)
        .await
        .unwrap_or_else(|e| panic!("Resource::new failed: {e:?}"));

    let mut player = OfflinePlayer::new(Consts::SAMPLE_RATE);
    player.load_and_fadein(resource, "t0");

    let warmup_target = player.position() + Consts::PRE_SEEK_RENDER_SECS;
    render_until_position(
        &mut player,
        blocks_for_seconds(Consts::PRE_SEEK_RENDER_SECS),
        warmup_target,
    )
    .await;
    let pos_before_seek = player.position();
    eprintln!(
        "[{label} delay_ms={delay_ms}] pre-seek position = {pos_before_seek:.3}s (expected >0)"
    );
    assert!(
        pos_before_seek > 0.2,
        "decoder never produced PCM before the seek \
         (pos={pos_before_seek:.3}s, delay_ms={delay_ms})"
    );

    player.seek(Consts::SEEK_TARGET_SECS, 1);
    eprintln!(
        "[{label} delay_ms={delay_ms}] seek issued target={:.1}s epoch=1",
        Consts::SEEK_TARGET_SECS
    );

    let post_target = Consts::SEEK_TARGET_SECS + Consts::MIN_POSITION_ADVANCE_POST_SEEK_SECS;
    if let Some(gate) = gate.as_ref() {
        let hold_ticks = match scenario {
            SeekScenario::Gated { hold_ticks, .. } => hold_ticks,
            SeekScenario::NoDelay | SeekScenario::RealDelay { .. } => 0,
        };
        render_until_gate_requested(&mut player, gate, post_target, hold_ticks, label).await;
        gate.release();
    }

    render_until_position(
        &mut player,
        blocks_for_seconds(Consts::POST_SEEK_AUDIO_SECS),
        post_target,
    )
    .await;
    let pos_after = player.position();
    eprintln!("[{label} delay_ms={delay_ms}] post-seek position = {pos_after:.3}s");

    let advance = pos_after - Consts::SEEK_TARGET_SECS;
    assert!(
        advance >= Consts::MIN_POSITION_ADVANCE_POST_SEEK_SECS,
        "seek did not land under simulated slow connection \
         (delay_ms={delay_ms}, pre-seek={pos_before_seek:.3}s, \
         target={:.3}s, post={pos_after:.3}s, \
         advance={advance:.3}s, expected >= {:.2}s) — \
         the player must wait for the delayed segment and land the seek",
        Consts::SEEK_TARGET_SECS,
        Consts::MIN_POSITION_ADVANCE_POST_SEEK_SECS,
    );

    drop(player);
    drop(downloader);
    drop(temp);
}
