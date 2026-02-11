<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/kithara-drm.svg)](https://crates.io/crates/kithara-drm)
[![Downloads](https://img.shields.io/crates/d/kithara-drm.svg)](https://crates.io/crates/kithara-drm)
[![docs.rs](https://docs.rs/kithara-drm/badge.svg)](https://docs.rs/kithara-drm)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-drm

AES-128-CBC segment decryption for encrypted HLS streams. Provides a single processing function that integrates with `kithara-assets`' `ProcessingAssets` layer for transparent decryption on commit.

## Key Types

| Type | Role |
|------|------|
| `DecryptContext` | Holds 16-byte AES key and IV for a segment |
| `aes128_cbc_process_chunk()` | Decrypts a chunk in-place; handles PKCS7 padding on final chunk |

## How It Works

The decryption function signature matches `ProcessChunkFn<DecryptContext>` from `kithara-assets`. When `ProcessingAssets::commit()` is called on an encrypted segment:

1. The segment data is read from disk in 64 KB chunks.
2. Each chunk is decrypted via `aes128_cbc_process_chunk()`.
3. The decrypted data is written back to the same location.
4. On the final chunk, PKCS7 padding is removed and the actual output length is returned.

Input must be aligned to the 16-byte AES block size. All operations are in-place (no buffer allocation).

## Key Derivation

IV derivation happens in `kithara-hls`'s `KeyManager`:

- If `#EXT-X-KEY` provides an explicit IV, it is used directly.
- Otherwise, IV is derived from the segment sequence number: `[0u8; 8] || sequence.to_be_bytes()`.

## Integration

Used by `kithara-hls` via `AssetsBackend<DecryptContext>`. The HLS crate sets up the processing callback when building the asset store, enabling transparent decryption of AES-128-CBC encrypted segments.
