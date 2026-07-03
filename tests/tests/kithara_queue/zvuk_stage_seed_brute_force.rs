#![cfg(not(target_arch = "wasm32"))]

use aes::{
    Aes128,
    cipher::{Array, BlockCipherDecrypt, KeyInit},
};
use bytes::Bytes;
use kithara::drm::UniqueBinaryCipher;
use kithara_integration_tests::kithara;
use reqwest::Client;

const USER_AGENT: &str = "OpenPlay - com.zvooq.openplay/4.30.0 (iPhone; iOS 17.5; Scale/3.00)";

fn build_client() -> Client {
    Client::builder()
        .danger_accept_invalid_certs(true)
        .no_proxy()
        .build()
        .expect("reqwest client")
}

fn hex_to_bytes(hex: &str) -> Vec<u8> {
    hex.as_bytes()
        .chunks(2)
        .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
        .collect()
}

fn xor_block(a: &[u8], b: &[u8]) -> Vec<u8> {
    a.iter().zip(b).map(|(x, y)| x ^ y).collect()
}

fn analyze_first_block(label: &str, cipher: &[u8], key: &[u8], iv: &[u8]) -> bool {
    let key_arr = Array::try_from(&key[..16]).unwrap();
    let aes = Aes128::new(&key_arr);
    let mut decoded = [0u8; 16];
    decoded.copy_from_slice(&cipher[..16]);
    aes.decrypt_block((&mut decoded).into());
    let xored = xor_block(&decoded, &iv[..16]);
    let typ = std::str::from_utf8(&xored[4..8]).unwrap_or("???");
    let valid = matches!(
        &xored[4..8],
        b"styp" | b"moof" | b"sidx" | b"mdat" | b"ftyp"
    );
    let key_hex: String = key.iter().take(16).map(|b| format!("{b:02x}")).collect();
    let pre: String = xored[..4].iter().map(|b| format!("{b:02x}")).collect();
    println!(
        "  {label:30}  key={key_hex}  → {pre} {typ}{}",
        if valid { "  ✅ FMP4!" } else { "" }
    );
    valid
}

async fn fetch(client: &Client, url: &str, headers: &[(&str, String)]) -> Result<Vec<u8>, String> {
    let mut req = client.get(url);
    for (k, v) in headers {
        req = req.header(*k, v);
    }
    let resp = req.send().await.map_err(|e| format!("send: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    Ok(resp
        .bytes()
        .await
        .map_err(|e| format!("body: {e}"))?
        .to_vec())
}

/// Verify the decrypt-chain on **prod** (where `zvuk_prod_drm_track_plays`
/// is green). Sanity-pins that `UniqueBinaryCipher` + AES-128-CBC + the
/// 8-char hex seed format produce a key that yields fmp4 magic on the
/// first segment of track `180082552`. If this ever stops decrypting to
/// `styp` / `moof`, the prod DRM path is broken — not the test fixture.
///
/// Stage DRM equivalent is parked: server returns keys that don't
/// decrypt their corresponding segments (3/3 tracks tested 2026-05-20);
/// waiting on server-team.
#[kithara::test(tokio)]
#[ignore = "live prod diagnostic — requires VPN + KITHARA_DRM_PROD_*"]
async fn prod_chain_sanity_check() {
    let auth_token =
        std::env::var("KITHARA_DRM_PROD_AUTH_TOKEN").expect("set KITHARA_DRM_PROD_AUTH_TOKEN");
    let sp_zv =
        std::env::var("KITHARA_DRM_PROD_SP_ZV_TOKEN").expect("set KITHARA_DRM_PROD_SP_ZV_TOKEN");
    let prod_cipher_key = std::env::var("KITHARA_DRM_PROD_KEY").expect("set KITHARA_DRM_PROD_KEY");
    let client = build_client();

    let blob = fetch(
        &client,
        "https://zvuk.com/keyserver/api/v1/key?track_id=180082552&quality=lq&content_type=track",
        &[
            ("X-Auth-Token", auth_token.clone()),
            ("X-SP-ZV", sp_zv.clone()),
            ("User-Agent", USER_AGENT.to_string()),
            ("X-Encrypted-Key", "aaaaaaaaaaaaaaaa".to_string()),
        ],
    )
    .await
    .expect("prod key");
    let segment = fetch(
        &client,
        "https://cdn-hls-slicer.zvuk.com/drm/track/180082552_1/segment-1-slq-a1.m4s",
        &[
            ("X-Auth-Token", auth_token.clone()),
            ("X-SP-ZV", sp_zv.clone()),
            ("User-Agent", USER_AGENT.to_string()),
        ],
    )
    .await
    .expect("prod segment");

    let prod_iv = hex_to_bytes("2EC8D6586179CF981FC553B4116442C1");
    let secret = format!("{prod_cipher_key}aaaaaaaaaaaaaaaa");
    let key = UniqueBinaryCipher::new(&secret).decrypt(&Bytes::copy_from_slice(&blob));
    let ok = analyze_first_block("prod default", &segment, &key, &prod_iv);
    assert!(
        ok,
        "prod decrypt chain regressed — investigate before touching stage path"
    );
}
