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
<tr><td><code>KeyProcessor</code></td><td>type alias</td><td><code>Arc&lt;dyn Fn(Bytes) -&gt; KeyProcessResult + Send + Sync&gt;</code> invoked before decryption to transform / unwrap a fetched key</td></tr>
<tr><td><code>KeyRequest</code></td><td>struct</td><td>Per-fetch processor plus request headers/query params produced by a rule</td></tr>
<tr><td><code>KeyRequestFactory</code></td><td>type alias</td><td>Factory that returns a fresh <code>KeyRequest</code> for each key fetch</td></tr>
<tr><td><code>KeyProcessorRule</code></td><td>struct (bon-builder)</td><td>Single registry entry: domain matchers + request factory + optional headers/query params</td></tr>
<tr><td><code>KeyProcessorRegistry</code></td><td>struct</td><td>Ordered list of <code>KeyProcessorRule</code>s; <code>find()</code> returns the first match</td></tr>
<tr><td><code>KeyProcessResult</code></td><td>type alias</td><td><code>Result&lt;Bytes, DrmError&gt;</code> returned by a <code>KeyProcessor</code> callback</td></tr>
<tr><td><code>DomainMatcher</code></td><td>enum</td><td><code>Exact</code> / <code>Wildcard</code> / <code>All</code> host matcher used by <code>KeyProcessorRegistry</code></td></tr>
<tr><td><code>DrmError</code></td><td>enum</td><td>Crate-level error</td></tr>
</table>

## Integration

Used by `kithara-hls` through per-resource processing context: encrypted segment
acquires pass `Option<ProcessCtx>` to `acquire_resource_with_ctx`, with
`DecryptContext` wrapped as a `ResourceProcessor`. IV derivation and optional key
unwrapping happen in `kithara-hls`'s `KeyStore` before a `DecryptContext` is
built.

See [CONTEXT.md](CONTEXT.md) for detailed contracts, invariants, and internals.
