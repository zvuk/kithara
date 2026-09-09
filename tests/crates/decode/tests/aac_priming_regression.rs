use std::io::Cursor;

use kithara::{
    decode::{DecoderConfig, DecoderFactory},
    platform::time::Duration,
    signal::AudioChunk,
};
use kithara_integration_tests::{
    TestServerHelper,
    bufpool_ext::{TestPools, pools},
};
use kithara_test_fixtures::SignalAsset;
use reqwest::Client;

#[kithara::fixture]
async fn aac() -> (TestServerHelper, Vec<u8>) {
    let server = TestServerHelper::new().await;
    let client = Client::new();
    let response = client
        .get(server.signal(SignalAsset::AAC_SAW_1S))
        .send()
        .await
        .expect("fetch /signal aac fixture");
    assert_eq!(response.status(), 200);
    let bytes = response.bytes().await.expect("aac body").to_vec();

    (server, bytes)
}

#[kithara::test(native, tokio, timeout(Duration::from_secs(10)), hang_timeout_secs(1))]
async fn aac_decoder_strips_algorithmic_delay_on_first_chunk(
    #[future(awt)] aac: (TestServerHelper, Vec<u8>),
) {
    let (_server, bytes) = aac;
    let mut decoder = DecoderFactory::create_with_probe(
        Cursor::new(bytes),
        Some("aac"),
        DecoderConfig::<kithara::resampler::NoResamplerBackend, TestPools>::builder()
            .pools(pools())
            .build(),
    )
    .expect("probe AAC decoder");

    // What we pin: the FIRST non-empty chunk delivered by
    // `next_chunk` (which skips empty chunks via the `frames == 0`
    // `continue` in `ComposedDecoder::next_chunk_inner`) must carry
    // real signal, not pure decoder-algorithmic silence.
    //
    // Without `outputDelay` handling, fdk-aac's first decoded frame
    // is all zeros (~1685 frames of lookahead silence at AAC-LC).
    // That zero-filled chunk slips through `frames == 0` because the
    // sample count is non-zero — only the *values* are silent — so
    // it surfaces as chunk 0 and the assertion below trips.
    //
    // With our `outputDelay` strip, the lookahead silence is dropped
    // before the chunk is emitted and the first surfaced chunk
    // starts with real sawtooth content.
    let outcome = decoder.next_chunk().expect("decode chunk 0");
    let chunk = AudioChunk::try_from(outcome).expect("chunk 0 must be a PCM chunk, not EOS");
    assert!(
        !chunk.samples.is_empty(),
        "AAC chunk 0 must not be empty after priming strip",
    );

    let max_abs = chunk
        .samples
        .iter()
        .map(|sample| sample.abs())
        .fold(0.0f32, f32::max);
    assert!(
        max_abs > 0.01,
        "AAC chunk 0 max |sample| = {max_abs:.6} (expected > 0.01). \
         fdk-aac algorithmic delay (`outputDelay`) not stripped — \
         chunk 0 is full of decoder lookahead zeros. See \
         crates/kithara-decode/src/symphonia/aac_fdk.rs.",
    );
}
