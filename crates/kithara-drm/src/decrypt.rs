#![forbid(unsafe_code)]

//! AES-128-CBC chunk decryption for `ProcessingAssets`.

use aes::Aes128;
use cbc::{
    Decryptor,
    cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7},
};
use tracing::trace;

use crate::DecryptContext;

/// AES block size in bytes.
const AES_BLOCK_SIZE: usize = 16;

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
            .decrypt_padded_mut::<Pkcs7>(&mut output[..input.len()])
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
        use cbc::cipher::block_padding::NoPadding;
        let decryptor = Decryptor::<Aes128>::new((&ctx.key).into(), (&ctx.iv).into());
        let plaintext = decryptor
            .decrypt_padded_mut::<NoPadding>(&mut output[..input.len()])
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

#[cfg(test)]
mod tests {
    use aes::Aes128;
    use cbc::{
        Encryptor,
        cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7},
    };
    use rstest::rstest;

    use super::*;

    fn encrypt_aes128_cbc(plaintext: &[u8], key: &[u8; 16], iv: &[u8; 16]) -> Vec<u8> {
        let encryptor = Encryptor::<Aes128>::new(key.into(), iv.into());
        // Allocate buffer: plaintext + up to one full padding block
        let padded_len = plaintext.len() + (AES_BLOCK_SIZE - plaintext.len() % AES_BLOCK_SIZE);
        let mut buf = vec![0u8; padded_len];
        buf[..plaintext.len()].copy_from_slice(plaintext);
        let ct = encryptor
            .encrypt_padded_mut::<Pkcs7>(&mut buf, plaintext.len())
            .expect("encrypt_padded_mut failed");
        ct.to_vec()
    }

    /// Roundtrip: encrypt → decrypt single chunk.
    #[rstest]
    #[case::hello(b"Hello, DRM world! This is a test of AES-128-CBC.".as_slice(), [0x42u8; 16], [0x13u8; 16])]
    #[case::exact_block(&[0x55u8; 16], [0xAAu8; 16], [0xBBu8; 16])]
    fn test_single_chunk_roundtrip(
        #[case] plaintext: &[u8],
        #[case] key: [u8; 16],
        #[case] iv: [u8; 16],
    ) {
        let ciphertext = encrypt_aes128_cbc(plaintext, &key, &iv);
        let mut ctx = DecryptContext::new(key, iv);

        let mut output = vec![0u8; ciphertext.len()];
        let written = aes128_cbc_process_chunk(&ciphertext, &mut output, &mut ctx, true).unwrap();

        assert_eq!(&output[..written], plaintext);
    }

    #[test]
    fn test_single_chunk_roundtrip_large() {
        let key = [0x01u8; 16];
        let iv = [0x02u8; 16];
        let plaintext: Vec<u8> = (0..1000).map(|i| (i % 256) as u8).collect();

        let ciphertext = encrypt_aes128_cbc(&plaintext, &key, &iv);
        let mut ctx = DecryptContext::new(key, iv);

        let mut output = vec![0u8; ciphertext.len()];
        let written = aes128_cbc_process_chunk(&ciphertext, &mut output, &mut ctx, true).unwrap();

        assert_eq!(written, plaintext.len());
        assert_eq!(&output[..written], &plaintext[..]);
    }

    #[test]
    fn test_empty_input() {
        let mut ctx = DecryptContext::new([0u8; 16], [0u8; 16]);
        let mut output = [0u8; 16];
        let written = aes128_cbc_process_chunk(&[], &mut output, &mut ctx, true).unwrap();
        assert_eq!(written, 0);
    }

    #[test]
    fn test_unaligned_input_fails() {
        let mut ctx = DecryptContext::new([0u8; 16], [0u8; 16]);
        let input = [0u8; 15]; // Not aligned to 16
        let mut output = [0u8; 15];
        let result = aes128_cbc_process_chunk(&input, &mut output, &mut ctx, false);
        assert!(result.is_err());
    }

    /// Multi-chunk CBC IV chaining.
    #[rstest]
    #[case::small_2_chunks(48, 32)]
    #[case::large_4_chunks(256, 64)]
    #[case::uneven_3_chunks(160, 48)]
    fn test_multi_chunk_cbc_chaining(#[case] plaintext_len: usize, #[case] chunk_size: usize) {
        let key = [0x77u8; 16];
        let iv = [0x33u8; 16];

        let plaintext: Vec<u8> = (0..plaintext_len).map(|i| (i % 256) as u8).collect();
        let ciphertext = encrypt_aes128_cbc(&plaintext, &key, &iv);

        let mut ctx = DecryptContext::new(key, iv);
        let mut decrypted = Vec::new();

        let total = ciphertext.len();
        let mut offset = 0;
        while offset < total {
            let end = (offset + chunk_size).min(total);
            let is_last = end == total;
            let chunk = &ciphertext[offset..end];
            let mut output = vec![0u8; chunk.len()];
            let written = aes128_cbc_process_chunk(chunk, &mut output, &mut ctx, is_last).unwrap();
            decrypted.extend_from_slice(&output[..written]);
            offset = end;
        }

        assert_eq!(decrypted, plaintext);
    }
}
