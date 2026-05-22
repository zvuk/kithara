#![cfg(not(target_arch = "wasm32"))]

use bytes::Bytes;
use kithara_app::baked::build_baked_drm_registry;
use kithara_drm::UniqueBinaryCipher;
use kithara_integration_tests::kithara;
use url::Url;

/// Captured fixture: 16-byte plaintext key the zvq.me staging server
/// returns when the request carries `X-Auth-Token` but no
/// `X-Encrypted-Key`.
const STAGE_PLAINTEXT: [u8; 16] = [
    0xd9, 0xd5, 0xf9, 0xd5, 0x1b, 0x88, 0xed, 0x28, 0xe1, 0xef, 0x32, 0x7a, 0x41, 0x15, 0xd6, 0xa3,
];

/// Captured fixture: the same key the staging server returns with
/// `X-Encrypted-Key: aaaaaaaaaaaaaaaa` — encrypted with the cipher
/// keyed on `"BinaryCipherKey" + "aaaaaaaaaaaaaaaa"`.
const STAGE_ENCRYPTED_WITH_AAAA_SEED: [u8; 16] = [
    0xfc, 0x37, 0xb6, 0x64, 0x03, 0x6d, 0x11, 0x1c, 0x4f, 0xcc, 0x0a, 0x19, 0x94, 0x98, 0x0f, 0x5a,
];

/// Pins the staging cipher contract: `UniqueBinaryCipher::new("BinaryCipherKey" +
/// "aaaaaaaaaaaaaaaa").decrypt(captured_encrypted) == captured_plaintext`.
///
/// This is a deterministic offline check — no network, no env vars. It
/// guards both the cipher implementation in `kithara-drm` and the
/// `STAGE_CIPHER_KEY` constant in `kithara_app::drm`; a regression in
/// either is a silent DRM corruption that segment-decode tests can't
/// catch without a live keyserver.
#[kithara::test]
fn stage_unique_cipher_matches_captured_keyserver_response() {
    let secret = format!(
        "{stage_key}{seed}",
        stage_key = "BinaryCipherKey",
        seed = "aaaaaaaaaaaaaaaa"
    );
    let cipher = UniqueBinaryCipher::new(&secret);
    let decrypted = cipher.decrypt(&Bytes::copy_from_slice(&STAGE_ENCRYPTED_WITH_AAAA_SEED));
    assert_eq!(
        decrypted.as_ref(),
        &STAGE_PLAINTEXT,
        "UniqueBinaryCipher::decrypt('BinaryCipherKey' + seed) must \
         reproduce the staging keyserver's plaintext response — a \
         drift here means stage DRM playback will silently emit garbage"
    );
}

/// Hypothesis pin: prove that the current `build.rs`-baked DRM registry
/// emits a seed format that does **not** match the captured stage
/// fixture (`aaaaaaaaaaaaaaaa` — 16-char alphanumeric).
///
/// Pre-`a2224b2dd` `kithara-app/src/drm.rs` shipped
/// `SEED_LEN = 16` with `Alphanumeric` alphabet — that's what the
/// staging keyserver was captured against. The `a2224b2dd` overhaul
/// collapsed every provider into a single `SEED_BYTES = 4` hex-only
/// closure (iOS WAF for prod requires `randomString(of: 8)`),
/// inadvertently breaking the stage flow: stage WAF / encryption
/// pipeline rejects (or replies inconsistently to) the truncated
/// 8-hex salt, so live decrypt drifts even though the cipher
/// algorithm itself is fine (the test above passes).
///
/// This test fails on `a2224b2dd` head — it pins the wrong contract.
/// Once `build.rs` reads `seed.length` + `seed.alphabet` per provider
/// from `drm.toml`, the assertions below will hold and prove that
/// stage requests carry the legacy 16-char alphanumeric salt while
/// prod requests carry the iOS-compatible 8-char hex salt.
#[kithara::test]
fn baked_registry_emits_per_provider_seed_format() {
    let registry = build_baked_drm_registry();

    let stage_url =
        Url::parse("https://ecs-stage-slicer-01.zvq.me/drm/track/0/key.bin").expect("stage url");
    let stage_rule = registry
        .find(&stage_url)
        .expect("zvuk-stage rule must match *.zvq.me");
    let stage_req = stage_rule.build_request();
    let stage_seed = stage_req
        .headers
        .get("X-Encrypted-Key")
        .cloned()
        .expect("stage rule must inject X-Encrypted-Key header");

    let prod_url =
        Url::parse("https://cdn-hls-slicer.zvuk.com/drm/track/0/key.bin").expect("prod url");
    let prod_rule = registry
        .find(&prod_url)
        .expect("zvuk-prod rule must match *.zvuk.com");
    let prod_req = prod_rule.build_request();
    let prod_seed = prod_req
        .headers
        .get("X-Encrypted-Key")
        .cloned()
        .expect("prod rule must inject X-Encrypted-Key header");

    assert_eq!(
        stage_seed.len(),
        16,
        "zvq.me staging server captured fixture uses 16-char alphanumeric \
         salt (`aaaaaaaaaaaaaaaa`); current build.rs emits `{stage_seed}` \
         ({} chars) which is the wrong format for stage WAF",
        stage_seed.len()
    );
    assert!(
        stage_seed.chars().all(|c| c.is_ascii_alphanumeric()),
        "zvq.me stage salt must be alphanumeric (a-z A-Z 0-9), got `{stage_seed}`"
    );

    assert_eq!(
        prod_seed.len(),
        8,
        "zvuk.com prod WAF requires `randomString(of: 8)` iOS format; \
         current build.rs emits `{prod_seed}` ({} chars)",
        prod_seed.len()
    );
    assert!(
        prod_seed.chars().all(|c| c.is_ascii_hexdigit()),
        "zvuk.com prod salt must be lowercase hex (0-9 a-f), got `{prod_seed}`"
    );
}
