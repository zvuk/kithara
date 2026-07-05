# kithara-drm — Context

Detailed contracts and invariants for the kithara-drm crate; the README is the overview.

## How It Works

HLS wraps `DecryptContext` as a `kithara-assets` `ResourceProcessor` / `ChunkSink`
pair in `kithara-hls/src/decrypt_processor.rs`. When an encrypted segment is
processed:

1. The segment data is read from disk in 64 KB chunks.
2. Each chunk is decrypted via `aes128_cbc_process_chunk()`.
3. The decrypted data is written back to the same location.
4. On the final chunk, PKCS7 padding is removed and the actual output length is returned.

Input must be aligned to the 16-byte AES block size. All operations are in-place (no buffer allocation).

## Key Derivation

IV derivation happens in `kithara-hls`'s `KeyStore`:

- If `#EXT-X-KEY` provides an explicit IV, it is used directly.
- Otherwise, IV is derived from the segment sequence number: `[0u8; 8] || sequence.to_be_bytes()`.
- Optional key unwrapping/processing for in-house DRM is also performed by `KeyStore` before building `DecryptContext`.

## Domain Matchers

`DomainMatcher::parse(pattern)` recognises three pattern shapes used by `KeyProcessorRegistry` to route key URLs to per-domain rules:

| Pattern | Variant | Matches |
|---------|---------|---------|
| `"zvuk.com"` | `Exact` | host equal to `zvuk.com` only |
| `"*.zvuk.com"` | `Wildcard` | any subdomain of `zvuk.com` (e.g. `cdn.zvuk.com`, `edge.cdn.zvuk.com`), but **not** `zvuk.com` itself |
| `"*"` | `All` | any host |

Registry order matters: `KeyProcessorRegistry::find` returns the first matching rule. Place specific rules first; an `"*"`-rule must be last, otherwise it masks every rule registered after it.
