#![forbid(unsafe_code)]

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

/// AES-128-CBC chunk decryption, for use as `ProcessChunkFn<DecryptContext>`
/// in `ProcessingAssets`.
///
/// Decrypts `input` into `output` using the key and IV from `ctx` and returns
/// the number of bytes written. The last chunk (`is_last = true`) removes PKCS7
/// padding; intermediate chunks decrypt block-by-block (16 in, 16 out).
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

    if !input.len().is_multiple_of(AES_BLOCK_SIZE) {
        return Err(format!(
            "input length {} is not aligned to AES block size {}",
            input.len(),
            AES_BLOCK_SIZE
        ));
    }

    let next_iv: [u8; AES_BLOCK_SIZE] = {
        let mut iv = [0u8; AES_BLOCK_SIZE];
        iv.copy_from_slice(&input[input.len() - AES_BLOCK_SIZE..]);
        iv
    };

    output[..input.len()].copy_from_slice(input);

    if is_last {
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
        Ok(written)
    } else {
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
        ctx.iv = next_iv;
        Ok(written)
    }
}
