#![forbid(unsafe_code)]
#![cfg(not(target_arch = "wasm32"))]

//! Regression: the on-device "track doesn't start" livelock.
//!
//! Field signature (two HE-AAC-v2 zvuk tracks): the reader parks right after
//! the init segment, `current_variant` stays pinned at 0, and the bandwidth
//! controller re-emits `Stay { BufferTooLowForUpSwitch }` on every tick while
//! the switch never lands — a livelock.
//!
//! Root (codec-agnostic, upstream of the decoder): the active variant's first
//! media segment is withheld, so `download_head` never advances and
//! `buffer_ahead` is pinned at `0`. The ABR up-switch buffer-too-low gate then
//! returns `Stay` on every tick even though the throughput estimate trivially
//! justifies a faster variant — no decision, no pending switch, no escape. The
//! gate's premise ("staying grows the buffer") is false for a variant that
//! delivers nothing.
//!
//! Fix: the HLS stall detector (`on_slow` → slot SLOW flag →
//! `segment_stalled_at_boundary`) flags the variant non-viable in `AbrState`
//! (`mark_escape`); `evaluate()` then excludes it from candidacy and skips the
//! buffer gate, producing an `EscapeStalled` up-switch that commits off the
//! existing boundary path.
//!
//! Deterministic trigger: variant 0 (the `auto(0)` start variant) has its
//! first media segment withheld far longer than the test budget while variants
//! 1/2 stay instant and the playlists/init fetch fast — the cached-playlist /
//! inflated-estimate shape that makes the controller want a higher variant
//! immediately. A healthy player escapes to a faster variant and decodes audio.
//!
//! Two acceptance gates, both bus-independent (HLS abr/downloader events are
//! published to a track-scoped child bus, not mirrored to `Audio::events()` in
//! the direct-`Audio`/no-queue path, so a typed-event assertion cannot observe
//! them here):
//!   1. Functional: variant 0 / segment 0 never arrives, so decoding
//!      `MIN_CHUNKS` is impossible WITHOUT switching to another variant.
//!   2. Mechanism: the live `AbrState` leaves variant 0. With `buffer_ahead`
//!      pinned at 0, the buffer-too-low gate bars every normal up-switch, so
//!      the ONLY decision that can move off variant 0 is `EscapeStalled` —
//!      `current_variant != 0` therefore pins the fix specifically.

use std::num::NonZeroUsize;

use kithara::{
    assets::{StorageBackend, StoreOptions},
    audio::{Audio, AudioConfig, ChunkOutcome, PcmReader},
    decode::DecoderBackend,
    hls::{Hls, HlsConfig},
    platform::{time::Duration, tokio::task::spawn_blocking},
    stream::Stream,
};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper, auto, fixture_protocol::DelayRule,
};

struct Consts;
impl Consts {
    const VARIANT_COUNT: usize = 3;
    const SEGMENTS_PER_VARIANT: usize = 8;
    const SEGMENT_DURATION_S: f64 = 2.0;
    /// Variant 0 cheapest so `auto(0)` starts there and even a modest
    /// throughput estimate justifies an up-switch off it.
    const BANDWIDTHS: [u64; 3] = [48_000, 512_000, 2_048_000];
    /// Withhold variant 0 / segment 0 far longer than the whole test —
    /// effectively "this segment never arrives".
    const STALL_MS: u64 = 600_000;
    /// Chunks that prove real playback advanced past startup.
    const MIN_CHUNKS: usize = 10;
    /// Drain budget. A healthy escape fills the ring in well under a second;
    /// the livelock spins on `Pending` for the whole window.
    const DRAIN_BUDGET: Duration = Duration::from_secs(10);
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(40)))]
#[case::symphonia(DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple(DecoderBackend::Apple)
)]
async fn abr_escapes_stalled_initial_variant(#[case] backend: DecoderBackend) {
    let helper = TestServerHelper::new().await;
    let builder = HlsFixtureBuilder::new()
        .variant_count(Consts::VARIANT_COUNT)
        .segments_per_variant(Consts::SEGMENTS_PER_VARIANT)
        .segment_duration_secs(Consts::SEGMENT_DURATION_S)
        .variant_bandwidths(Consts::BANDWIDTHS.to_vec())
        .packaged_audio_aac_lc(44_100, 2)
        .push_delay_rule(DelayRule {
            variant: Some(0),
            segment_eq: Some(0),
            delay_ms: Consts::STALL_MS,
            ..Default::default()
        });
    let url = helper
        .create_hls(builder)
        .await
        .expect("create HLS fixture")
        .master_url();

    let store = StoreOptions::builder()
        .backend(StorageBackend::Memory)
        .cache_capacity(NonZeroUsize::new(64).expect("nonzero"))
        .build();

    let hls_config = HlsConfig::for_url(url)
        .store(store)
        .initial_abr_mode(auto(0))
        .build();
    let config = AudioConfig::<Hls>::for_stream(hls_config)
        .decoder_backend(backend)
        .block_on_underrun(true)
        .build();
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("audio creation");

    // Clone the live ABR handle to read `current_variant` after the drain —
    // the `Arc<AbrState>` it holds outlives `audio` (dropped inside the
    // blocking closure), so the read reflects the last committed variant.
    let abr = audio.abr_handle();

    // Offline-pull drain on a dedicated thread (`block_on_underrun`): a ring
    // underrun blocks in the engine-aware park (`wait_for_fetch`) instead of
    // returning `Pending`, so this thread yields the CPU to the current-thread
    // runtime that drives the HLS fetch/scheduler tasks. A sleep-poll loop is
    // the wrong pattern here: under the flash virtual clock `thread::sleep`
    // collapses to ~0 real time, so the poll loop busy-spins and starves the
    // runtime's in-flight segment fetch — the escape commits and the recreate
    // starts, but the new variant's bytes never land. The engine-aware park is
    // the canonical offline-consumer contract (pull, never sleep-poll).
    let drained = spawn_blocking(move || {
        let deadline = kithara::platform::time::Instant::now() + Consts::DRAIN_BUDGET;
        let mut chunks = 0usize;
        while chunks < Consts::MIN_CHUNKS && kithara::platform::time::Instant::now() < deadline {
            match PcmReader::next_chunk(&mut audio) {
                Ok(ChunkOutcome::Chunk(_)) => chunks += 1,
                Ok(ChunkOutcome::Eof { .. }) => break,
                Ok(ChunkOutcome::Pending { .. }) => break,
                Err(e) => panic!("decode error: {e}"),
            }
        }
        chunks
    })
    .await
    .expect("drain join");

    let final_variant = abr.as_ref().and_then(|h| h.current_variant_index());

    assert!(
        drained >= Consts::MIN_CHUNKS,
        "variant 0 / segment 0 is withheld and the player could not switch to a \
         deliverable variant — decoded only {drained} chunks (expected >= {min}). \
         The buffer-too-low up-switch gate blocks every tick (buffer_ahead pinned \
         at 0 on the non-delivering variant) so no EscapeStalled decision is \
         produced. final_variant={final_variant:?}. backend={backend:?}.",
        min = Consts::MIN_CHUNKS,
    );
    assert!(
        final_variant.is_some_and(|v| v != 0),
        "decoded {drained} chunks but ABR never left variant 0 \
         (final_variant={final_variant:?}) — the chunks must come from a \
         deliverable variant via the EscapeStalled switch, not from reading past \
         the withheld segment on variant 0. backend={backend:?}.",
    );
}
