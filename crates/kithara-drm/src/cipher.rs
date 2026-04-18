use bytes::{Bytes, BytesMut};

/// Symmetric cipher that produces unique ciphertext for the same plaintext
/// based on the byte's position.
///
/// Ported from zvqengine.
pub struct UniqueBinaryCipher {
    seed: u64,
}

impl UniqueBinaryCipher {
    // Splitmix64 constants (Steele, Lea & Moler, 2014).
    const SPLITMIX_INCREMENT: u64 = 0x9e3779b97f4a7c15; // golden-ratio derived
    const SPLITMIX_MIX1: u64 = 0xbf58476d1ce4e5b9;
    const SPLITMIX_MIX2: u64 = 0x94d049bb133111eb;
    const SPLITMIX_SHIFT1: u32 = 30;
    const SPLITMIX_SHIFT2: u32 = 27;
    const SPLITMIX_SHIFT3: u32 = 31;

    // Xorshift64* constants (Marsaglia, 2003; Vigna star variant).
    const XORSHIFT_SHIFT_A: u32 = 12;
    const XORSHIFT_SHIFT_B: u32 = 25;
    const XORSHIFT_SHIFT_C: u32 = 27;
    const XORSHIFT_STAR_MUL: u64 = 0x2545f4914f6cdd1d;

    // Keystream extraction parameters.
    const KEYSTREAM_SHIFT: u32 = 56; // extract top byte
    const ROTATION_MASK: u8 = 7; // 3-bit rotation amount

    /// Creates a new cipher instance from a given key string.
    #[must_use]
    pub fn new(key: &str) -> Self {
        Self {
            seed: Self::derive_seed_from_key(key.as_bytes()),
        }
    }

    /// Decrypts the given data.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "keystream_byte and rot are masked to 8-bit / 3-bit before cast"
    )]
    pub fn decrypt(&self, data: &Bytes) -> Bytes {
        let mut out = BytesMut::with_capacity(data.len());
        let mut state = self.seed;

        for (i, &b) in data.iter().enumerate() {
            state = Self::xorshift64_star(state ^ i as u64);
            let keystream_byte = (state >> Self::KEYSTREAM_SHIFT) as u8;
            let rot = (state & u64::from(Self::ROTATION_MASK)) as u8;

            let mixed = Self::ror8(b, rot);
            let plain_byte = mixed.wrapping_sub(keystream_byte);
            out.extend_from_slice(&[plain_byte]);
            state ^= u64::from(b);
        }
        out.freeze()
    }

    #[inline]
    fn ror8(v: u8, r: u8) -> u8 {
        v.rotate_right(u32::from(r) & u32::from(Self::ROTATION_MASK))
    }

    fn derive_seed_from_key(key_bytes: &[u8]) -> u64 {
        const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
        const FNV_PRIME: u64 = 0x100000001b3;

        let mut h = FNV_OFFSET_BASIS;
        for &b in key_bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(FNV_PRIME);
        }

        let mut z = h.wrapping_add(Self::SPLITMIX_INCREMENT);
        z = (z ^ (z >> Self::SPLITMIX_SHIFT1)).wrapping_mul(Self::SPLITMIX_MIX1);
        z = (z ^ (z >> Self::SPLITMIX_SHIFT2)).wrapping_mul(Self::SPLITMIX_MIX2);
        z ^ (z >> Self::SPLITMIX_SHIFT3)
    }

    #[inline]
    fn xorshift64_star(mut x: u64) -> u64 {
        x ^= x >> Self::XORSHIFT_SHIFT_A;
        x ^= x << Self::XORSHIFT_SHIFT_B;
        x ^= x >> Self::XORSHIFT_SHIFT_C;
        x.wrapping_mul(Self::XORSHIFT_STAR_MUL)
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    fn encrypt(cipher: &UniqueBinaryCipher, data: Bytes) -> Bytes {
        let mut out = BytesMut::with_capacity(data.len());
        let mut state = cipher.seed;

        for (i, &b) in data.iter().enumerate() {
            state = UniqueBinaryCipher::xorshift64_star(state ^ i as u64);
            let keystream_byte = (state >> UniqueBinaryCipher::KEYSTREAM_SHIFT) as u8;
            let rot = (state & u64::from(UniqueBinaryCipher::ROTATION_MASK)) as u8;

            let mixed = b.wrapping_add(keystream_byte);
            let cipher_byte =
                mixed.rotate_left(u32::from(rot) & u32::from(UniqueBinaryCipher::ROTATION_MASK));
            out.extend_from_slice(&[cipher_byte]);
            state ^= u64::from(cipher_byte);
        }
        out.freeze()
    }

    fn assert_round_trip(cipher: &UniqueBinaryCipher, plain_str: &str) {
        let plain = Bytes::copy_from_slice(plain_str.as_bytes());
        let enc = encrypt(cipher, plain.clone());
        let dec = cipher.decrypt(&enc);

        assert_eq!(plain, dec, "round-trip failed for: '{plain_str}'");
        if !plain.is_empty() {
            assert_ne!(plain, enc, "ciphertext must differ for: '{plain_str}'");
        }
    }

    #[kithara::test]
    fn round_trip_text() {
        let cipher = UniqueBinaryCipher::new("my super secret key");
        assert_round_trip(&cipher, "");
        assert_round_trip(&cipher, "hello");
        assert_round_trip(&cipher, "Hello, World!");
    }

    #[kithara::test]
    fn different_keys_produce_different_ciphertext() {
        let c1 = UniqueBinaryCipher::new("k1");
        let c2 = UniqueBinaryCipher::new("k2");
        let msg = Bytes::copy_from_slice(b"same message");

        let e1 = encrypt(&c1, msg.clone());
        let e2 = encrypt(&c2, msg.clone());
        assert_ne!(e1, e2);

        assert_eq!(msg, c1.decrypt(&e1));
        assert_eq!(msg, c2.decrypt(&e2));
    }
}
