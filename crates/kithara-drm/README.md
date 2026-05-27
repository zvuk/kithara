<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-drm.svg)](https://crates.io/crates/kithara-drm)
[![docs.rs](https://docs.rs/kithara-drm/badge.svg)](https://docs.rs/kithara-drm)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-drm

AES-128-CBC segment decryption for encrypted HLS streams. Provides a single processing function that integrates with `kithara-assets`' `ProcessingAssets` layer for transparent decryption on commit.

## Usage

```rust
use kithara_drm::{DecryptContext, aes128_cbc_process_chunk};

let key = [0u8; 16]; // AES-128 key from #EXT-X-KEY
let iv = [0u8; 16];  // IV from playlist or derived from sequence number
let mut ctx = DecryptContext::new(key, iv);

let encrypted: &[u8] = &ciphertext;
let mut output = vec![0u8; encrypted.len()];
let len = aes128_cbc_process_chunk(encrypted, &mut output, &mut ctx, true)?;
let decrypted = &output[..len];
```

## Key Types

<table>
<tr><th>Item</th><th>Kind</th><th>Role</th></tr>
<tr><td><code>DecryptContext</code></td><td>struct</td><td>Holds the 16-byte AES key, IV, and chunk-cursor state for a segment</td></tr>
<tr><td><code>aes128_cbc_process_chunk</code></td><td>fn</td><td>Decrypts a chunk in place; handles PKCS7 padding on the final chunk</td></tr>
<tr><td><code>UniqueBinaryCipher</code></td><td>struct</td><td>Helper cipher used by app-side key processors (e.g. to unwrap server-encrypted keys with an app-embedded master key)</td></tr>
<tr><td><code>KeyProcessor</code></td><td>trait</td><td>Callback invoked before decryption to transform / unwrap a fetched key (in-house DRM hook)</td></tr>
<tr><td><code>KeyProcessorRule</code></td><td>struct (bon-builder)</td><td>Single registry entry: domain pattern + processor</td></tr>
<tr><td><code>KeyProcessorRegistry</code></td><td>struct</td><td>Ordered list of <code>KeyProcessorRule</code>s; <code>find()</code> returns the first match</td></tr>
<tr><td><code>KeyProcessResult</code></td><td>type</td><td>Result type returned by <code>KeyProcessor::process</code></td></tr>
<tr><td><code>DomainMatcher</code></td><td>enum</td><td><code>Exact</code> / <code>Wildcard</code> / <code>All</code> host matcher used by <code>KeyProcessorRegistry</code></td></tr>
<tr><td><code>DrmError</code></td><td>enum</td><td>Crate-level error</td></tr>
</table>

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
- Optional key unwrapping/processing for in-house DRM is also performed by `KeyManager` before building `DecryptContext`.

## Integration

Used by `kithara-hls` via `AssetStore<DecryptContext>`. The HLS crate sets up the processing callback when building the asset store, enabling transparent decryption of AES-128-CBC encrypted segments.

## Domain Matchers

`DomainMatcher::parse(pattern)` recognises three pattern shapes used by `KeyProcessorRegistry` to route key URLs to per-domain rules:

| Pattern | Variant | Matches |
|---------|---------|---------|
| `"zvuk.com"` | `Exact` | host equal to `zvuk.com` only |
| `"*.zvuk.com"` | `Wildcard` | any subdomain of `zvuk.com` (e.g. `cdn.zvuk.com`, `edge.cdn.zvuk.com`), but **not** `zvuk.com` itself |
| `"*"` | `All` | any host |

Registry order matters: `KeyProcessorRegistry::find` returns the first matching rule. Place specific rules first; an `"*"`-rule must be last, otherwise it masks every rule registered after it.
