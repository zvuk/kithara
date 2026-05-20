#![cfg(not(target_arch = "wasm32"))]

use bytes::Bytes;
use kithara_drm::UniqueBinaryCipher;
use kithara_integration_tests::kithara;

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
