#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

#[derive(Arbitrary, Debug)]
struct AesInput {
    key: [u8; 16],
    iv: [u8; 16],
    /// Raw ciphertext bytes (will be truncated to AES-block alignment).
    data: Vec<u8>,
    is_last: bool,
}

fuzz_target!(|input: AesInput| {
    // Align to 16-byte blocks (AES requirement).
    let block_len = (input.data.len() / 16) * 16;
    if block_len == 0 {
        return;
    }

    let data = &input.data[..block_len];
    let mut ctx = kithara_drm::DecryptContext::new(input.key, input.iv);
    let mut output = vec![0u8; data.len()];
    // We only care that this doesn't panic/crash unexpectedly.
    let _ = kithara_drm::aes128_cbc_process_chunk(data, &mut output, &mut ctx, input.is_last);
});
