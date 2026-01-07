# Crate Consolidation in Kithara

This document explains the consolidation of crates in the Kithara workspace and provides guidance for understanding the current architecture.

## Overview

As Kithara evolved, we consolidated several crates to reduce complexity and improve maintainability. This document explains what was consolidated and how to map old concepts to the new structure.

## Consolidated Crates

### 1. `kithara-core` → `kithara-assets`

The functionality previously in `kithara-core` has been merged into `kithara-assets`:

**What was in `kithara-core`:**
- Basic identity types and error definitions
- Minimal foundational types

**What's now in `kithara-assets`:**
- All identity types (`AssetId`, `ResourceKey`)
- Error types (`AssetsError`, `AssetsResult`)
- The foundational `Assets` trait and related types

**Reason for consolidation:**
- `kithara-core` was too minimal to justify a separate crate
- Identity and error types are intrinsically tied to asset management
- Reduced crate count simplifies dependency management

### 2. `kithara-io` → `kithara-stream`

The bridge layer functionality from `kithara-io` has been integrated into `kithara-stream`:

**What was in `kithara-io`:**
- Async-to-sync bridge for decode threads
- `Read + Seek` interface over async storage
- EOF semantics and backpressure handling

**What's now in `kithara-stream`:**
- `io` module containing bridge functionality
- `Reader` type that provides `Read + Seek` interface
- Integrated backpressure and EOF handling in the orchestration layer

**Reason for consolidation:**
- Bridge functionality is inherently part of stream orchestration
- Reduced complexity by having one crate handle all streaming concerns
- Better integration between orchestration and I/O layers

## Current Crate Structure

### Infrastructure Layer
- **`kithara-net`**: HTTP networking with retry, timeout, and range support
- **`kithara-storage`**: Storage primitives (`AtomicResource`, `StreamingResource`)
- **`kithara-assets`**: Persistent disk assets store (includes former `kithara-core`)

### Orchestration Layer  
- **`kithara-stream`**: Stream orchestration with backpressure (includes former `kithara-io`)
- **`kithara-file`**: Progressive file download and playback
- **`kithara-hls`**: HLS VOD orchestration with ABR

### Processing Layer
- **`kithara-decode`**: Audio decoding with Symphonia

## Migration Guide

### For Code Using `kithara-core`

Old code:
```rust
use kithara_core::{AssetId, ResourceKey};
```

New code:
```rust
use kithara_assets::{AssetId, ResourceKey};
```

### For Code Using `kithara-io`

Old code (conceptual):
```rust
use kithara_io::{SomeBridgeType, ReadSeekAdapter};
```

New code:
```rust
use kithara_stream::io::Reader;
// Reader provides Read + Seek interface over async storage
```

### For Understanding the Architecture

The consolidated architecture follows these principles:

1. **Single Responsibility**: Each crate has a clear, focused purpose
2. **Logical Grouping**: Related functionality is grouped together
3. **Minimal Interfaces**: Public APIs are minimal and well-defined
4. **Composition Over Inheritance**: Components compose rather than inherit

## Public Contract Stability

Despite the consolidation, public contracts remain stable:

- **Type names** remain the same where possible
- **Trait definitions** maintain backward compatibility
- **Error types** preserve existing variants
- **Module structure** follows logical grouping

## Benefits of Consolidation

1. **Reduced Complexity**: Fewer crates to manage and understand
2. **Better Cohesion**: Related functionality lives together
3. **Simplified Dependencies**: Cleaner dependency graphs
4. **Easier Testing**: Integrated functionality can be tested together
5. **Improved Documentation**: Single source of truth for related concepts

## Future Considerations

The current crate structure is designed to be stable. Future changes will follow these guidelines:

1. New functionality first evaluates existing crates for fit
2. New crates require clear justification and separation of concerns
3. Public APIs maintain backward compatibility
4. Documentation updates accompany structural changes

## References

- [`kithara/README.md`](../README.md) - Project overview and crate architecture
- [`kithara/docs/architecture.md`](architecture.md) - Detailed architecture documentation
- [`kithara/docs/constraints.md`](constraints.md) - Project constraints and lessons learned
- Individual crate `README.md` files for detailed contract documentation