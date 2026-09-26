//! Named HLS fixture presets.
//!
//! Every synthetic HLS stream is served through
//! [`TestServerHelper::create_hls`]; a preset is only the
//! [`HlsFixtureBuilder`] a family of tests shares. The returned
//! [`CreatedHls`] hands out typed URLs and the expected-byte oracle.

use kithara::{platform::time::Duration, stream::AudioCodec};
use url::Url;

use crate::{
    CreatedHls, HlsFixtureBuilder, TestServerHelper,
    fixture_protocol::{DataMode, DelayRule, EncryptionRequest, InitMode},
    hls_fixture::{aes128_iv, aes128_key_bytes, aes128_plaintext_segment, init_data},
};

/// Three variants of three 200 kB test-pattern segments. Each segment starts
/// with `V{variant}-SEG-{segment}:TEST_SEGMENT_DATA` and is padded with
/// `0xFF`.
#[must_use]
pub fn test_pattern_ladder() -> HlsFixtureBuilder {
    HlsFixtureBuilder::new().variant_count(3)
}

/// [`test_pattern_ladder`] served on the shared test server.
#[kithara::fixture]
pub async fn test_pattern_hls() -> CreatedHls {
    TestServerHelper::new()
        .await
        .create_hls(test_pattern_ladder())
        .await
        .expect("create the test-pattern ladder")
}

/// The AES-128 key and IV every encrypted synthetic fixture uses.
#[must_use]
pub fn aes128_encryption() -> EncryptionRequest {
    EncryptionRequest {
        key_hex: hex::encode(aes128_key_bytes()),
        iv_hex: Some(hex::encode(aes128_iv())),
    }
}

/// One variant holding a single AES-128 segment whose plaintext is
/// [`aes128_plaintext_segment`].
#[must_use]
pub fn aes128_segment() -> HlsFixtureBuilder {
    let plaintext = aes128_plaintext_segment();
    let segment_size = plaintext.len();
    HlsFixtureBuilder::new()
        .segments_per_variant(1)
        .segment_size(segment_size)
        .data_mode(DataMode::CustomDataPerVariant(vec![plaintext]))
        .encryption(aes128_encryption())
        .head_reported_segment_size(segment_size)
}

/// Three binary ABR variants at 256/512/1024 kbit/s whose variant 2 delays
/// its first segment by `v2_segment0_delay`, optionally with
/// `V{variant}-INIT:` init segments.
#[must_use]
pub fn abr_binary_ladder(init: bool, v2_segment0_delay: Duration) -> HlsFixtureBuilder {
    let init_mode = if init {
        InitMode::Custom((0..3).map(init_data).collect())
    } else {
        InitMode::None
    };
    HlsFixtureBuilder::new()
        .variant_count(3)
        .variant_bandwidths(vec![256_000, 512_000, 1_024_000])
        .data_mode(DataMode::AbrBinary)
        .init_mode(init_mode)
        .head_reported_segment_size(200_000)
        .delay_rules(vec![DelayRule {
            variant: Some(2),
            segment_eq: Some(0),
            segment_gte: None,
            delay_ms: u64::try_from(v2_segment0_delay.as_millis()).expect("delay fits in u64"),
        }])
}

/// Ladder mirroring the production master playlist: three AAC-LC variants
/// under one FLAC lossless, at the bandwidths the real master advertises,
/// over 37 × 6 s.
///
/// Both halves are load-bearing and neither is optional for the tests that
/// take this ladder. The FLAC variant is the far side of the codec boundary
/// an ABR move has to cross; the length is what lets a test seek deep into
/// the track. [`packaged_ladder`] stays the cheaper choice for anything
/// that needs neither.
#[must_use]
pub fn mixed_codec_ladder() -> HlsFixtureBuilder {
    HlsFixtureBuilder::new()
        .variant_count(4)
        .segments_per_variant(37)
        .segment_duration_secs(6.0)
        .variant_bandwidths(vec![66_005, 134_107, 269_930, 988_758])
        .packaged_audio_aac_lc(44_100, 2)
        .override_variant_codec(3, AudioCodec::Flac)
}

/// [`mixed_codec_ladder`] with AES-128 segments. The production DRM master
/// advertises the same four variants at the same bandwidths, so the two
/// differ only by encryption.
#[must_use]
pub fn mixed_codec_ladder_encrypted() -> HlsFixtureBuilder {
    mixed_codec_ladder().encryption(aes128_encryption())
}

/// Serves [`mixed_codec_ladder`], plain or encrypted, and hands back its
/// master URL.
///
/// The two trees this ladder replaces differed only by encryption, so the
/// suites that read both of them take the axis as a flag rather than as two
/// separate setup paths.
pub async fn mixed_codec_ladder_url(server: &TestServerHelper, encrypted: bool) -> Url {
    let ladder = if encrypted {
        mixed_codec_ladder_encrypted()
    } else {
        mixed_codec_ladder()
    };
    server
        .create_hls(ladder)
        .await
        .expect("create the mixed-codec ladder")
        .master_url()
}

/// Three AAC-LC fMP4 variants of three 4 s segments at 1.28/2.56/5.12 Mbit/s:
/// the default synthetic packaged-audio ladder.
#[must_use]
pub fn packaged_ladder() -> HlsFixtureBuilder {
    HlsFixtureBuilder::new()
        .variant_count(3)
        .segments_per_variant(3)
        .segment_duration_secs(4.0)
        .variant_bandwidths(vec![1_280_000, 2_560_000, 5_120_000])
        .packaged_audio_aac_lc(44_100, 2)
}

/// One variant of [`packaged_ladder`] with AES-128 segments.
#[must_use]
pub fn packaged_ladder_encrypted() -> HlsFixtureBuilder {
    packaged_ladder()
        .variant_count(1)
        .variant_bandwidths(vec![1_280_000])
        .encryption(aes128_encryption())
}

/// [`packaged_ladder`] served on the shared test server.
#[kithara::fixture]
pub async fn packaged_hls() -> CreatedHls {
    TestServerHelper::new()
        .await
        .create_hls(packaged_ladder())
        .await
        .expect("create the packaged ladder")
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use kithara::platform::{flash::real_io, time::Duration, tokio};

    use super::packaged_ladder;
    use crate::{TestServerHelper, kithara};

    /// The withhold gate parks a segment GET at the server until the test
    /// releases it, and the in-process `requested()` counter lets the test
    /// observe that the GET actually arrived. Polling that counter under a
    /// hard timeout is synchronization on an observable — a real stall fails
    /// the budget rather than being masked.
    //
    // This drives raw `reqwest` through the platform spawn chokepoint against
    // the shared test server (a real-time island, no flash ambient). The in-flight
    // request and its response live on the REAL clock, invisible to the flash
    // engine (no downloader `real_io` bracket wraps them). Hold a `RealIoScope`
    // across every wait on that real transit — the gated GET reaching the server
    // and the released GET completing — so the virtual clock is PACED to real
    // time while held: each `time::timeout` budget then fires only after the
    // equivalent REAL time, never spuriously ahead of bytes still on the wire.
    // The budget is preserved as a real stall oracle (a genuinely stuck request
    // still exhausts the paced 5s), not relaxed. Off the `flash` feature the
    // scope is a ZST no-op and the clock is already real.
    #[kithara::test(tokio)]
    async fn segment_gate_withholds_get_until_release() {
        let helper = TestServerHelper::new().await;
        let hls = helper
            .create_hls(packaged_ladder())
            .await
            .expect("create packaged ladder");
        let gate = helper.register_segment_gate(hls.token(), 0, 1);
        let url = hls.segment_url(0, 1);
        assert_eq!(gate.requested(), 0, "no GET before the test fires one");

        let _real_io = real_io();

        let fetch = tokio::task::spawn(async move { reqwest::get(url).await.map(|r| r.status()) });

        time::timeout(Duration::from_secs(5), async {
            while gate.requested() == 0 {
                time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("withheld segment GET should reach the server within budget");

        assert!(
            !fetch.is_finished(),
            "withheld segment GET must not complete before release"
        );

        gate.release();

        let status = time::timeout(Duration::from_secs(5), fetch)
            .await
            .expect("released GET should complete within budget")
            .expect("segment GET task joins")
            .expect("segment GET succeeds");
        assert!(
            status.is_success() || status.as_u16() == 206,
            "released segment GET should succeed, got {status}"
        );
        assert_eq!(gate.requested(), 1, "exactly one GET parked on the gate");
    }

    /// The size/HEAD gate makes HEAD report `Content-Length: 0` while
    /// withheld (and counts the HEAD), then the true size after
    /// `release_head()` — without ever parking the HEAD (which would block
    /// stream construction). The body GET stays unblocked throughout.
    //
    // Same raw-reqwest-over-real-socket shape as above: hold a `RealIoScope`
    // across every real HEAD/GET so the virtual clock is paced to real time and
    // no `time::timeout` budget collapses ahead of the in-flight response. The
    // budget stays a real stall oracle, not relaxed (see the sibling test).
    #[kithara::test(tokio)]
    async fn segment_size_gate_reports_zero_until_release_head() {
        let helper = TestServerHelper::new().await;
        let hls = helper
            .create_hls(packaged_ladder())
            .await
            .expect("create packaged ladder");
        let gate = helper.register_segment_gate(hls.token(), 0, 1);
        gate.withhold_head();
        gate.release();
        let url = hls.segment_url(0, 1);
        assert_eq!(
            gate.head_requested(),
            0,
            "no HEAD before the test fires one"
        );

        let _real_io = real_io();

        let withheld = time::timeout(
            Duration::from_secs(5),
            reqwest::Client::new().head(url.clone()).send(),
        )
        .await
        .expect("HEAD must not park while size is withheld")
        .expect("withheld-size HEAD succeeds");
        assert!(withheld.status().is_success());
        assert_eq!(
            withheld
                .headers()
                .get("content-length")
                .and_then(|v| v.to_str().ok()),
            Some("0"),
            "withheld size must be reported as Content-Length: 0"
        );
        assert_eq!(gate.head_requested(), 1, "HEAD reached the gate");

        gate.release_head();
        let revealed = time::timeout(
            Duration::from_secs(5),
            reqwest::Client::new().head(url).send(),
        )
        .await
        .expect("HEAD completes")
        .expect("revealed-size HEAD succeeds");
        let revealed_len: u64 = revealed
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .expect("revealed HEAD carries a content-length");
        assert!(
            revealed_len > 0,
            "released size must report the true (non-zero) length, got {revealed_len}"
        );

        // The body GET is independent of the size gate and stays unblocked.
        let body = time::timeout(Duration::from_secs(5), reqwest::get(hls.segment_url(0, 1)))
            .await
            .expect("body GET completes (never parked by the size gate)")
            .expect("body GET succeeds");
        assert!(body.status().is_success() || body.status().as_u16() == 206);
    }
}
