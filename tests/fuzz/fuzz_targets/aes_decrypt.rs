#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

#[derive(Arbitrary, Debug)]
struct AesInput {
    key: [u8; 16],
    iv: [u8; 16],
    data: Vec<u8>,
    is_last: bool,
}

fuzz_target!(|input: AesInput| {
    let block_len = (input.data.len() / 16) * 16;
    if block_len == 0 {
        return;
    }

    let data = &input.data[..block_len];
    let mut ctx = kithara_drm::DecryptContext::new(input.key, input.iv);
    let mut output = vec![0u8; data.len()];
    let _ = kithara_drm::aes128_cbc_process_chunk(data, &mut output, &mut ctx, input.is_last);
});
