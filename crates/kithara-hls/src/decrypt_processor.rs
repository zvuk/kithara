//! AES-128-CBC adapter binding the `kithara-drm` cipher to the
//! `kithara-assets` `ResourceProcessor` / `ChunkSink` contract.

use std::{fmt, sync::Arc};

use kithara_assets::{ChunkSink, ProcessCtx, ResourceProcessor};
use kithara_drm::{DecryptContext, aes128_cbc_process_chunk};

/// Wrap a [`DecryptContext`] as a per-acquire [`ProcessCtx`] trait object.
pub(crate) fn as_process_ctx(ctx: DecryptContext) -> ProcessCtx {
    Arc::new(DecryptProcessor::new(ctx))
}

/// AES-128-CBC [`ResourceProcessor`] over a [`DecryptContext`].
pub(crate) struct DecryptProcessor {
    ctx: DecryptContext,
    identity: Box<[u8]>,
}

impl fmt::Debug for DecryptProcessor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DecryptProcessor")
            .field("identity", &"<redacted>")
            .field("ctx", &self.ctx)
            .finish()
    }
}

impl DecryptProcessor {
    /// Build a processor whose identity is the `key||iv` bytes of `ctx`.
    pub(crate) fn new(ctx: DecryptContext) -> Self {
        let mut identity = Vec::with_capacity(ctx.key.len() + ctx.iv.len());
        identity.extend_from_slice(&ctx.key);
        identity.extend_from_slice(&ctx.iv);
        Self {
            ctx,
            identity: identity.into_boxed_slice(),
        }
    }
}

impl ResourceProcessor for DecryptProcessor {
    fn identity(&self) -> &[u8] {
        &self.identity
    }

    fn begin(&self) -> Box<dyn ChunkSink> {
        Box::new(CbcSink {
            ctx: self.ctx.clone(),
        })
    }
}

/// Fresh per-commit CBC chaining state: carries the evolving IV that
/// [`aes128_cbc_process_chunk`] advances between 64KB chunks.
struct CbcSink {
    ctx: DecryptContext,
}

impl ChunkSink for CbcSink {
    fn process(&mut self, input: &[u8], output: &mut [u8], is_last: bool) -> Result<usize, String> {
        aes128_cbc_process_chunk(input, output, &mut self.ctx, is_last)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_key_material() {
        let key = [0xAB; 16];
        let iv = [0xCD; 16];
        let processor = DecryptProcessor::new(DecryptContext::new(key, iv));
        let dbg = format!("{processor:?}");
        assert!(dbg.contains("<redacted>"), "expected redaction, got: {dbg}");
        assert!(!dbg.contains("171"), "leaked key byte 0xAB (171): {dbg}");
        assert!(!dbg.contains("205"), "leaked iv byte 0xCD (205): {dbg}");
    }
}
