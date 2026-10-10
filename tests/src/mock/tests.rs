#![cfg(all(feature = "all", not(target_arch = "wasm32")))]

use std::num::NonZeroU32;

use ::kithara::{audio::mock::TestPcmReader, signal::AudioSpec};
use kithara_test_utils::kithara;

use super::PcmDeck;

#[kithara::test]
fn pcm_deck_preserves_rate_samples_and_file_lifetime() {
    let spec = AudioSpec::new(2, NonZeroU32::new(48_000).expect("test rate"));
    let samples = vec![0.25f32, -0.5, 1.0, -1.0];
    let deck = PcmDeck::new(Box::new(TestPcmReader::with_samples(spec, samples.clone())));
    let path = deck._directory.path().join("deck.wav");
    let bytes = std::fs::read(&path).expect("read PCM deck WAV");
    assert_eq!(&bytes[..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    assert_eq!(&bytes[20..22], &3u16.to_le_bytes());
    assert_eq!(&bytes[22..24], &2u16.to_le_bytes());
    assert_eq!(&bytes[24..28], &48_000u32.to_le_bytes());
    assert_eq!(&bytes[34..36], &32u16.to_le_bytes());
    assert_eq!(&bytes[40..44], &32u32.to_le_bytes());
    assert_eq!(bytes.len(), 76);
    assert_eq!(bytes[44..].len() / (2 * 4), samples.len());
    for (encoded, sample) in bytes[44..]
        .chunks_exact(4)
        .zip(samples.into_iter().flat_map(|sample| [sample, sample]))
    {
        assert_eq!(encoded, &sample.to_le_bytes());
    }
    assert!(path.exists());
    drop(deck);
    assert!(!path.exists());
}
