use kithara_encode::{
    ContainerFinish, ContainerSession, ContainerWrite, EncodeConfig, EncodeError, EncoderSession,
};
use kithara_stream::{AudioCodec, ContainerFormat};
#[cfg(all(test, target_os = "android"))]
use kithara_test_dylib as _;
use kithara_test_fixtures::unit_fixtures::encode_session;
use kithara_test_utils::kithara;

const CHANNELS: u16 = 2;
const SAMPLE_RATE: u32 = 48_000;

fn apply(write: &ContainerWrite, output: &mut Vec<u8>) {
    let offset = usize::try_from(write.offset).expect("test offset fits usize");
    let end = offset
        .checked_add(write.bytes.len())
        .expect("test write end fits usize");
    output.resize(output.len().max(end), 0);
    output[offset..end].copy_from_slice(&write.bytes);
}

fn apply_all(writes: impl IntoIterator<Item = ContainerWrite>, output: &mut Vec<u8>) {
    for write in writes {
        apply(&write, output);
    }
}

fn finish(finish: ContainerFinish, output: &mut Vec<u8>) -> Vec<u8> {
    apply_all(finish.writes, output);
    output.truncate(usize::try_from(finish.final_len).expect("test length fits usize"));
    output.clone()
}

fn encode(chunks: &[&[f32]]) -> Vec<u8> {
    let config = EncodeConfig::builder()
        .sample_rate(SAMPLE_RATE)
        .channels(CHANNELS)
        .packet_frames(2)
        .build();
    let mut encoder = EncoderSession::new(&config).expect("portable PCM encoder");
    let mut container = ContainerSession::new(&config).expect("portable WAV container");
    let mut output = Vec::new();

    for chunk in chunks {
        for unit in encoder.push(chunk).expect("encode PCM chunk") {
            apply_all(
                container.push(unit).expect("write WAV payload"),
                &mut output,
            );
        }
    }
    for unit in encoder.finish().expect("finish PCM encoder") {
        apply_all(container.push(unit).expect("write WAV tail"), &mut output);
    }
    let done = container.finish().expect("finish WAV container");
    finish(done, &mut output)
}

#[kithara::test(flash(false))]
fn wav_float32_is_portable_and_chunk_invariant(encode_session: Vec<f32>) {
    let samples = encode_session;

    let whole = encode(&[&samples]);
    let ragged = encode(&[&samples[..2], &samples[2..]]);

    assert_eq!(whole, ragged, "PCM chunking must not change the asset");
    assert_eq!(&whole[0..4], b"RIFF");
    assert_eq!(u32::from_le_bytes(whole[4..8].try_into().unwrap()), 60);
    assert_eq!(&whole[8..12], b"WAVE");
    assert_eq!(&whole[12..16], b"fmt ");
    assert_eq!(u16::from_le_bytes(whole[20..22].try_into().unwrap()), 3);
    assert_eq!(u16::from_le_bytes(whole[22..24].try_into().unwrap()), 2);
    assert_eq!(
        u32::from_le_bytes(whole[24..28].try_into().unwrap()),
        48_000
    );
    assert_eq!(
        u32::from_le_bytes(whole[28..32].try_into().unwrap()),
        384_000
    );
    assert_eq!(u16::from_le_bytes(whole[32..34].try_into().unwrap()), 8);
    assert_eq!(u16::from_le_bytes(whole[34..36].try_into().unwrap()), 32);
    assert_eq!(&whole[36..40], b"data");
    assert_eq!(u32::from_le_bytes(whole[40..44].try_into().unwrap()), 24);
    assert_eq!(
        &whole[44..],
        samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect::<Vec<_>>()
    );
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn wav_rejects_a_known_frame_count_past_its_container_limit() {
    let config = EncodeConfig::builder()
        .sample_rate(SAMPLE_RATE)
        .channels(CHANNELS)
        .build();
    let container = ContainerSession::new(&config).expect("portable WAV container");
    let too_many = container.max_frames() + 1;

    let error = container
        .validate_frame_count(too_many)
        .expect_err("RIFF cannot name that payload length");

    assert!(matches!(
        error,
        EncodeError::ContainerLimitExceeded {
            container: ContainerFormat::Wav,
            ..
        }
    ));
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn unsupported_profiles_are_not_silently_substituted() {
    let unsupported_codec = EncodeConfig::builder()
        .codec(AudioCodec::Flac)
        .sample_rate(SAMPLE_RATE)
        .channels(CHANNELS)
        .build();
    let codec_error = EncoderSession::new(&unsupported_codec)
        .expect_err("portable session must not substitute PCM for FLAC");
    assert!(matches!(
        codec_error,
        EncodeError::UnsupportedCodec(AudioCodec::Flac)
    ));

    let unsupported_container = EncodeConfig::builder()
        .container(ContainerFormat::Flac)
        .sample_rate(SAMPLE_RATE)
        .channels(CHANNELS)
        .build();
    let container_error = ContainerSession::new(&unsupported_container)
        .expect_err("portable session must not substitute WAV for FLAC");
    assert!(matches!(
        container_error,
        EncodeError::UnsupportedContainer(ContainerFormat::Flac)
    ));
}
