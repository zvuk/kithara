use kithara_assets::{ChunkSink, ProcessCtx, ResourceProcessor};
use kithara_drm::{DecryptContext, aes128_cbc_process_chunk};
use kithara_platform::sync::Arc;

/// Wrap a [`DecryptContext`] as a per-acquire [`ProcessCtx`] trait object.
pub(crate) fn as_process_ctx(ctx: DecryptContext) -> ProcessCtx {
    Arc::new(DecryptProcessor::new(ctx))
}

/// AES-128-CBC [`ResourceProcessor`] over a [`DecryptContext`].
#[derive(derive_more::Debug)]
pub(crate) struct DecryptProcessor {
    ctx: DecryptContext,
    #[debug("<redacted>")]
    identity: [u8; 32],
}

impl DecryptProcessor {
    /// Build a processor whose identity is the `key||iv` bytes of `ctx`.
    pub(crate) fn new(ctx: DecryptContext) -> Self {
        let mut identity = [0; 32];
        let (key, iv) = identity.split_at_mut(ctx.key.len());
        key.copy_from_slice(&ctx.key);
        iv.copy_from_slice(&ctx.iv);
        Self { ctx, identity }
    }
}

impl ResourceProcessor for DecryptProcessor {
    fn begin(&self) -> Box<dyn ChunkSink> {
        Box::new(CbcSink {
            ctx: self.ctx.clone(),
        })
    }

    fn identity(&self) -> &[u8] {
        &self.identity
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
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test(native, flash(false))]
    fn debug_redacts_key_material() {
        let key = [0xab; 16];
        let iv = [0xcd; 16];
        let processor = DecryptProcessor::new(DecryptContext::new(key, iv));
        let dbg = format!("{processor:?}");
        assert!(dbg.contains("<redacted>"), "expected redaction, got: {dbg}");
        assert!(!dbg.contains("171"), "leaked key byte 0xAB (171): {dbg}");
        assert!(!dbg.contains("205"), "leaked iv byte 0xCD (205): {dbg}");
    }

    #[kithara::test(native, flash(false))]
    fn identity_is_key_followed_by_iv() {
        let key = [0xab; 16];
        let iv = [0xcd; 16];
        let processor = DecryptProcessor::new(DecryptContext::new(key, iv));

        assert_eq!(&processor.identity()[..key.len()], &key);
        assert_eq!(&processor.identity()[key.len()..], &iv);
    }
}
