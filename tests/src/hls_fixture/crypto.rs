/// Init segment bytes the synthetic fixtures serve for one variant.
pub fn init_data(variant: usize) -> Vec<u8> {
    format!("V{variant}-INIT:").into_bytes()
}

/// AES-128 encryption key bytes for testing
pub fn aes128_key_bytes() -> Vec<u8> {
    b"0123456789abcdef".to_vec()
}

/// AES-128 initialization vector for testing
pub const fn aes128_iv() -> [u8; 16] {
    [0u8; 16]
}

/// Plaintext segment data for encryption testing
pub fn aes128_plaintext_segment() -> Vec<u8> {
    b"V0-SEG-0:DRM-PLAINTEXT".to_vec()
}
