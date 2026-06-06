use std::io::Cursor;

use kithara::decode::{DecoderConfig, DecoderFactory, PcmChunk};
use kithara_integration_tests::{SignalFormat, SignalSpec, SignalSpecLength, TestServerHelper};
use kithara_platform::time::Duration;
use reqwest::Client;

#[kithara::test(
    flash(false),
    native,
    tokio,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn aac_decoder_strips_algorithmic_delay_on_first_chunk() {
    let server = TestServerHelper::new().await;
    let client = Client::new();
    let spec = SignalSpec {
        sample_rate: 44_100,
        channels: 2,
        length: SignalSpecLength::Seconds(1.0),
        format: SignalFormat::Aac,
        bit_rate: None,
    };

    let response = client
        .get(server.sawtooth(&spec).await)
        .send()
        .await
        .expect("fetch /signal aac fixture");
    assert_eq!(response.status(), 200);
    let bytes = response.bytes().await.expect("aac body");

    let mut decoder = DecoderFactory::create_with_probe(
        Cursor::new(bytes.to_vec()),
        Some("aac"),
        DecoderConfig::default(),
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
    let chunk = PcmChunk::try_from(outcome).expect("chunk 0 must be a PCM chunk, not EOS");
    assert!(
        !chunk.pcm.is_empty(),
        "AAC chunk 0 must not be empty after priming strip",
    );

    let max_abs = chunk
        .pcm
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
