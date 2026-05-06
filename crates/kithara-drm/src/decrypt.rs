#![forbid(unsafe_code)]

//! AES-128-CBC chunk decryption for `ProcessingAssets`.

use aes::Aes128;
use cbc::{
    Decryptor,
    cipher::{
        BlockModeDecrypt, KeyIvInit,
        block_padding::{NoPadding, Pkcs7},
    },
};
use tracing::trace;

use crate::DecryptContext;

/// AES block size in bytes.
pub(crate) const AES_BLOCK_SIZE: usize = 16;

/// AES-128-CBC chunk decryption function.
///
/// Designed for use as `ProcessChunkFn<DecryptContext>` in `ProcessingAssets`.
///
/// Decrypts `input` into `output` using the key and IV from `ctx`.
/// For the last chunk (`is_last = true`), PKCS7 padding is removed.
/// For intermediate chunks, data is decrypted block-by-block without
/// padding removal (each 16-byte block produces 16 bytes).
///
/// # Arguments
/// - `input`: encrypted bytes (must be aligned to AES block size, except possibly the last chunk)
/// - `output`: buffer for decrypted bytes (same capacity as input)
/// - `ctx`: decryption context with key and IV
/// - `is_last`: true for the final chunk (triggers PKCS7 unpadding)
///
/// # Returns
/// Number of decrypted bytes written to `output`.
///
/// # Errors
///
/// Returns an error string if the input length is not aligned to the AES block
/// size, or if decryption / PKCS7 unpadding fails.
pub fn aes128_cbc_process_chunk(
    input: &[u8],
    output: &mut [u8],
    ctx: &mut DecryptContext,
    is_last: bool,
) -> Result<usize, String> {
    if input.is_empty() {
        return Ok(0);
    }

    // AES-CBC requires input aligned to block size
    if !input.len().is_multiple_of(AES_BLOCK_SIZE) {
        return Err(format!(
            "input length {} is not aligned to AES block size {}",
            input.len(),
            AES_BLOCK_SIZE
        ));
    }

    // Save last ciphertext block BEFORE decryption — needed for CBC IV chaining.
    // In CBC mode the IV for the next chunk is the last ciphertext block of this chunk.
    let next_iv: [u8; AES_BLOCK_SIZE] = {
        let mut iv = [0u8; AES_BLOCK_SIZE];
        iv.copy_from_slice(&input[input.len() - AES_BLOCK_SIZE..]);
        iv
    };

    // Copy input to output buffer for in-place decryption
    output[..input.len()].copy_from_slice(input);

    if is_last {
        // Last chunk: decrypt with PKCS7 unpadding
        let decryptor = Decryptor::<Aes128>::new((&ctx.key).into(), (&ctx.iv).into());
        let plaintext = decryptor
            .decrypt_padded::<Pkcs7>(&mut output[..input.len()])
            .map_err(|e| format!("PKCS7 unpad failed: {e}"))?;
        let written = plaintext.len();
        trace!(
            encrypted = input.len(),
            decrypted = written,
            "aes128_cbc: last chunk decrypted with unpadding"
        );
        // No need to update IV for last chunk — there are no more chunks.
        Ok(written)
    } else {
        // Intermediate chunk: decrypt without unpadding (block-by-block, same size)
        let decryptor = Decryptor::<Aes128>::new((&ctx.key).into(), (&ctx.iv).into());
        let plaintext = decryptor
            .decrypt_padded::<NoPadding>(&mut output[..input.len()])
            .map_err(|e| format!("CBC decrypt failed: {e}"))?;
        let written = plaintext.len();
        trace!(
            encrypted = input.len(),
            decrypted = written,
            "aes128_cbc: intermediate chunk decrypted"
        );
        // Update IV for next chunk: CBC chaining requires the last ciphertext block.
        ctx.iv = next_iv;
        Ok(written)
    }
}
