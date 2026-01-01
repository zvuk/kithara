# kithara-core

Minimal core types shared across kithara crates.

## What belongs here
- Identity types (`AssetId`, `ResourceHash`) with URL canonicalization
- Core error types used by multiple crates  
- Basic utilities that all other crates depend on

## What must NOT be here

**⚠️ IMPORTANT: No settings/options in kithara-core**

- `CacheOptions` belongs in `kithara-cache`
- `HlsOptions` belongs in `kithara-hls`
- Any other configuration belongs in its respective domain crate

## Why this restriction

This crate is a shared dependency that all other kithara crates depend on. Adding settings here would:

1. Create circular dependencies when settings need domain-specific types
2. Make the crate a dumping ground for unrelated code
3. Break the architectural principle that settings live with their domain

## Examples of what SHOULD be here

```rust
// ✅ Identity types and canonicalization
let asset_id = AssetId::from_url(&url);
let resource_hash = ResourceHash::from_url(&url);

// ✅ Core errors
let result: CoreResult<()> = Ok(());
```

## Examples of what MUST NOT be here

```rust
// ❌ Settings and configuration
pub struct CacheOptions { /* ... */ }  // Move to kithara-cache
pub struct HlsOptions { /* ... */ }    // Move to kithara-hls

// ❌ Domain-specific logic
pub async fn download_from_cache(...) { /* ... */ }  // Move to kithara-cache
pub fn parse_hls_playlist(...) { /* ... */ }           // Move to kithara-hls
```