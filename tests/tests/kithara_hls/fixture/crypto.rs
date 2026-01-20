//! Encryption and decryption utilities for HLS tests
//!
//! Provides AES-128 encryption helpers for testing encrypted HLS content.

use aes::Aes128;
use cbc::{Encryptor, cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7}};

/// Generate init segment data for variant
pub fn init_data(variant: usize) -> Vec<u8> {
    format!("V{}-INIT:", variant).into_bytes()
}

/// AES-128 encryption key bytes for testing
pub fn aes128_key_bytes() -> Vec<u8> {
    b"0123456789abcdef".to_vec()
}

/// AES-128 initialization vector for testing
pub fn aes128_iv() -> [u8; 16] {
    [0u8; 16]
}

/// Plaintext segment data for encryption testing
pub fn aes128_plaintext_segment() -> Vec<u8> {
    b"V0-SEG-0:DRM-PLAINTEXT".to_vec()
}

/// Encrypted ciphertext using AES-128-CBC
pub fn aes128_ciphertext() -> Vec<u8> {
    let key = aes128_key_bytes();
    let iv = aes128_iv();
    let mut data = aes128_plaintext_segment();
    let plain_len = data.len();
    data.resize(plain_len + 16, 0);
    let encryptor = Encryptor::<Aes128>::new((&key[..16]).into(), (&iv).into());
    let cipher = encryptor
        .encrypt_padded_mut::<Pkcs7>(&mut data, plain_len)
        .expect("aes128 encrypt");
    cipher.to_vec()
}

/// Test init data with extended content
pub fn test_init_data(variant: usize) -> Vec<u8> {
    let prefix = format!("V{}-INIT:", variant);
    let mut data = prefix.into_bytes();
    data.extend(b"TEST_INIT_DATA");
    data
}

/// Test encryption key data
pub fn test_key_data() -> Vec<u8> {
    b"TEST_KEY_DATA_123456".to_vec()
}
