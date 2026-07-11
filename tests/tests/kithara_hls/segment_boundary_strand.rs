#![cfg(not(target_arch = "wasm32"))]

//! Deterministic Tier-B repro for flake F1 (`stress_chunk_integrity_mmap`):
//! a Symphonia read-ahead byte-strand at a not-ready segment boundary.
//!
//! Original root (now structurally fixed): during a post-seek forward
//! read-ahead, Symphonia's `MediaSourceStream` consumed the tail of segment
//! N into a packet buffer, then the blocking `Stream::read` GAVE UP at the
//! not-yet-delivered segment N+1 (a fixed read-layer budget) → `Interrupted`
//! → the half-read packet was discarded with `read_pos` already advanced →
//! the stranded ~640 frames were swallowed (the saw-tooth jumped).
//!
//! The give-up budget at the read layer is gone: give-up authority moved
//! down into the downloader, which retries a stalled fetch and only surfaces
//! a TERMINAL error once it permanently fails. So the blocking read no longer
//! abandons a packet mid-read at a slow-but-arriving boundary — it WAITS for
//! the bytes. The strand can no longer form by construction; this test now
//! pins that improved contract: a boundary segment delivered slowly (released
//! the moment its GET parks at the gate — i.e. exactly when the read is
//! blocked waiting for it) yields ONE continuous decoded saw-tooth, no
//! swallowed frames.
//!
//! Determinism: no `sleep`, no real-time pacing, no ABR (single variant,
//! manual mode). The `HlsTestServer` segment gate withholds segment N+1's
//! BODY while its size (HEAD) stays known, so the decoder reaches the
//! boundary and blocks on the withheld body. The decode loop runs
//! synchronously on a blocking thread; a release task observes the parked
//! GET on the gate's in-process counter and releases it. After release the
//! blocking read completes — the decoded saw-tooth must stay continuous
//! across the boundary (phase +1 per frame, the same metric
//! `stress_chunk_integrity` asserts).

use kithara::{
    assets::StoreOptions,
    decode::{DecoderBackend, DecoderChunkOutcome, DecoderConfig, DecoderFactory},
    hls::{AbrMode, Hls, HlsConfig},
    platform::{
        CancelToken,
        sync::Arc,
        time::{Duration, Instant},
        tokio,
        tokio::task::spawn_blocking,
    },
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use kithara_integration_tests::{
    SAW_PERIOD, TestTempDir,
    hls_server::{HlsTestServer, HlsTestServerConfig},
    phase_from_f32,
    signal_pcm::{Finite, SignalPcm, signal},
    wav::create_wav_header,
};
use tracing::info;

const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: u16 = 2;
/// Smaller segments than the 200 KB default so the boundary is reached
/// quickly and a single saw period (`SAW_PERIOD` frames) spans many
/// segments — the saw never wraps within one decoded run, so a strand is
/// unambiguous.
const SEGMENT_SIZE: usize = 32_768;
const SEGMENT_COUNT: usize = 8;
/// The boundary the reader crosses INTO. Body withheld until release.
const GATED_SEGMENT: usize = 4;

fn bytes_per_frame() -> usize {
    CHANNELS as usize * size_of::<i16>()
}

/// Frame index of the first PCM frame in `segment` (PCM-relative; the WAV
/// header lives in the init segment and is not counted here).
fn segment_first_frame(segment: usize) -> u64 {
    (segment * SEGMENT_SIZE / bytes_per_frame()) as u64
}

#[kithara::test(
    tokio,
    native,
    timeout(Duration::from_secs(30)),
    tracing("kithara_decode=debug,kithara_hls=debug,kithara_stream=debug")
)]
async fn wav_hls_read_ahead_strand_at_not_ready_boundary_keeps_saw_continuous() {
    let init_segment = Arc::new(create_wav_header(SAMPLE_RATE, CHANNELS, None));
    let pcm = Arc::new(
        SignalPcm::new(
            signal::Sawtooth,
            SAMPLE_RATE,
            CHANNELS,
            Finite::from_segments(SEGMENT_COUNT, SEGMENT_SIZE, CHANNELS),
        )
        .into_vec(),
    );

    let segment_duration = SEGMENT_SIZE as f64
        / (f64::from(SAMPLE_RATE) * f64::from(CHANNELS) * size_of::<i16>() as f64);

    let config = HlsTestServerConfig {
        variant_count: 1,
        segments_per_variant: SEGMENT_COUNT,
        segment_size: SEGMENT_SIZE,
        segment_duration_secs: segment_duration,
        custom_data_per_variant: Some(vec![Arc::clone(&pcm)]),
        init_data_per_variant: Some(vec![Arc::clone(&init_segment)]),
        variant_bandwidths: Some(vec![1_000_000]),
        ..Default::default()
    };

    // Withhold the BODY of segment GATED_SEGMENT; its HEAD (size) stays
    // unblocked so up-front size estimation still learns the layout.
    let (server, gate) = HlsTestServer::with_segment_gate(config, 0, GATED_SEGMENT).await;

    let url = server.url("/master.m3u8");
    info!(%url, gated_segment = GATED_SEGMENT, "WAV-over-HLS fixture with one withheld segment body");

    let temp_dir = TestTempDir::new();
    let cancel = CancelToken::never();

    let hls_config = HlsConfig::for_url(url)
        .store(StoreOptions::new(temp_dir.path()))
        .cancel(cancel)
        // Manual variant 0 — no ABR, no recreate; isolate the read-ahead strand.
        .initial_abr_mode(AbrMode::manual(0))
        .build();

    let stream = Stream::<Hls>::new(hls_config)
        .await
        .expect("build Stream<Hls> over WAV fixture");

    // Seek to a position whose decoded packet straddles the
    // (GATED_SEGMENT-1 → GATED_SEGMENT) boundary: land a little before the
    // boundary so the forward read-ahead crosses into the withheld segment.
    let boundary_frame = segment_first_frame(GATED_SEGMENT);
    let seek_frame = boundary_frame.saturating_sub(2_048);
    let seek_pos = Duration::from_secs_f64(seek_frame as f64 / f64::from(SAMPLE_RATE));
    info!(
        seek_frame,
        boundary_frame,
        ?seek_pos,
        "seeking just before the withheld boundary"
    );

    // The forward read-ahead crosses into the withheld segment, finds it not
    // ready, and the blocking `Stream::read` parks — dispatching the segment's
    // GET, which lands on the gate. The parked GET (`requested() > 0`) is thus
    // the observable proof that the read is *currently blocked waiting for this
    // exact segment*; releasing then drives the wait-then-resume path. (The old
    // contract additionally waited for a decoder `Pending`, but the blocking
    // read no longer surfaces one at a slow boundary — it waits — so that would
    // deadlock; see the module doc.)
    let release_gate = gate.clone();
    let releaser = tokio::task::spawn(async move {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if release_gate.requested() > 0 {
                release_gate.release();
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::task::yield_now().await;
        }
    });

    // Build the decoder and drive decode on a blocking thread so the
    // tokio runtime can drive the HLS peer's segment fetches AND the gate
    // releaser concurrently while the blocking `Stream::read` waits.
    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let decode = spawn_blocking(move || -> (Vec<(u64, Vec<f32>)>, usize) {
        let byte_len = stream.len().unwrap_or(0);
        let byte_map = stream.byte_map();
        let decoder_config = DecoderConfig::<kithara::resampler::NoResamplerBackend>::builder()
            .backend(DecoderBackend::Symphonia)
            .byte_len_handle(Arc::new(std::sync::atomic::AtomicU64::new(byte_len)))
            .maybe_byte_map(byte_map)
            .hint("wav".to_string())
            .build();
        let mut decoder = DecoderFactory::create_from_media_info(stream, &wav_info, decoder_config)
            .expect("build Symphonia WAV decoder over Stream<Hls>");

        let outcome = decoder
            .seek(seek_pos)
            .expect("seek before withheld boundary must not error");
        info!(?outcome, "decoder landed");

        let mut chunks: Vec<(u64, Vec<f32>)> = Vec::new();
        let mut pendings = 0usize;
        let deadline = Instant::now() + Duration::from_secs(25);
        // Pull enough chunks to cross well past the boundary.
        while chunks.len() < 64 {
            match decoder.next_chunk() {
                Ok(DecoderChunkOutcome::Chunk(chunk)) => {
                    chunks.push((chunk.meta.frame_offset, chunk.samples.clone()));
                }
                Ok(DecoderChunkOutcome::Pending(_)) => {
                    // Not expected under the wait-then-resume contract (the
                    // blocking read waits across the slow boundary rather than
                    // surfacing Pending), but counted so the assertions can
                    // document it. The deadline guards a genuine wedge.
                    pendings += 1;
                    assert!(
                        Instant::now() < deadline,
                        "decode stalled at the withheld boundary past the budget"
                    );
                }
                Ok(DecoderChunkOutcome::Eof) => break,
                Err(e) => panic!("decode error pulling across the boundary: {e}"),
            }
        }
        (chunks, pendings)
    });

    let released = releaser.await.expect("release task joins");
    assert!(
        released,
        "withheld segment GET must reach the gate within budget"
    );

    let (chunks, pendings) = spawn_blocking_join(decode).await;

    info!(
        chunk_count = chunks.len(),
        pendings, "decode finished pulling across the boundary"
    );
    // Under the wait-then-resume contract the blocking read does NOT surface a
    // Pending at a slow-but-arriving boundary — it waits — so `pendings` is
    // expected to be 0. The strand was a give-up artifact; its absence is the
    // fix. Continuity (below) is the load-bearing assertion.
    assert_eq!(
        pendings, 0,
        "the blocking read must WAIT across the slow boundary, not surface a \
         give-up Pending (a Pending here means the read abandoned a packet \
         mid-read — the strand bug)"
    );
    assert!(
        chunks.len() >= 8,
        "expected several decoded chunks across the boundary, got {}",
        chunks.len()
    );

    // Reconstruct the decoded saw-tooth in emission order (one value per
    // frame, channel 0) and assert continuity: each frame's phase is the
    // previous +1 mod SAW_PERIOD. A read-ahead strand swallows ~640 frames
    // at the boundary, producing exactly one large phase jump.
    let channels = CHANNELS as usize;
    let mut samples: Vec<f32> = Vec::new();
    for (_off, pcm) in &chunks {
        for frame in pcm.chunks_exact(channels) {
            samples.push(frame[0]);
        }
    }
    assert!(
        samples.len() > 16,
        "not enough decoded frames to check continuity"
    );

    let mut breaks = 0usize;
    let mut worst_jump = 0usize;
    for w in samples.windows(2) {
        let prev = phase_from_f32(w[0]);
        let curr = phase_from_f32(w[1]);
        let expected_asc = (prev + 1) % SAW_PERIOD;
        if curr != expected_asc {
            breaks += 1;
            let jump = curr.abs_diff(prev);
            worst_jump = worst_jump.max(jump.min(SAW_PERIOD - jump));
        }
    }

    info!(
        breaks,
        worst_jump,
        frames = samples.len(),
        "saw-tooth continuity across boundary"
    );
    assert_eq!(
        breaks, 0,
        "saw-tooth DATA discontinuity across the withheld segment boundary: \
         {breaks} break(s), worst phase jump {worst_jump} frames — the read-ahead \
         strand swallowed decoded PCM at the not-ready boundary"
    );
}

/// Join a `spawn_blocking` decode handle, surfacing a panic as a test
/// failure (mirrors `.await.expect(...)` but keeps the call site terse).
async fn spawn_blocking_join<T: Send + 'static>(handle: tokio::task::JoinHandle<T>) -> T {
    handle.await.expect("decode task joins without panicking")
}
