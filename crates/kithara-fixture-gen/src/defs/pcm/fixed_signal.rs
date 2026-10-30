use kithara_test_macros as kithara;
use num_traits::{AsPrimitive, ToPrimitive};

use super::unit::wav_header;
use crate::consts;

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream")]
#[case::chirp(true)]
#[case::sine(false)]
fn render_signal(chirp: bool) -> Vec<u8> {
    let length: f64 = consts::RENDER_FRAMES.as_();
    let span = length / 44_100.0;
    (0..consts::RENDER_FRAMES)
        .flat_map(|frame| {
            let frame: f64 = frame.as_();
            let phase = if chirp {
                let time = frame / 44_100.0;
                std::f64::consts::TAU
                    * time.mul_add(220.0, (1_760.0 - 220.0) / (2.0 * span) * time * time)
            } else {
                std::f64::consts::TAU * 440.0 * frame / 44_100.0
            };
            let sample: f32 = (0.5 * phase.sin()).as_();
            [sample; 2]
        })
        .flat_map(f32::to_le_bytes)
        .collect()
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream")]
#[case::outgoing(false)]
#[case::incoming(true)]
fn host_manual_pcm(incoming: bool) -> Vec<u8> {
    let (hz, amplitude) = if incoming {
        (330.0, 0.125)
    } else {
        (220.0, 0.2)
    };
    (0..consts::HOST_FRAMES)
        .flat_map(|frame| {
            let phase =
                frame.to_f64().expect("fixture frame") * hz * std::f64::consts::TAU / 44_100.0;
            let sample = (amplitude * phase.sin())
                .to_f32()
                .expect("fixture amplitude");
            if incoming {
                [0.0, sample]
            } else {
                [sample, 0.0]
            }
        })
        .flat_map(f32::to_le_bytes)
        .collect()
}

#[kithara::asset(
    ext = "wav", content_type = "audio/wav",
    depends_on = ["host_manual_pcm_{case}"]
)]
#[case::outgoing()]
#[case::incoming()]
fn host_manual_wav(inputs: &[&[u8]]) -> Vec<u8> {
    let pcm = inputs[0];
    let mut wav = wav_header(3, 32, 44_100, 2, pcm.len());
    wav.extend_from_slice(pcm);
    wav
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream")]
#[case::default()]
fn stretch_kick_body() -> Vec<u8> {
    let mut phase = 0.0;
    let kick: Vec<f64> = (0..2_400)
        .map(|frame| {
            let seconds = frame.to_f64().expect("frame fits f64") / 48_000.0;
            let hz = 50.0 + 100.0 * (-seconds / 0.02).exp();
            phase += std::f64::consts::TAU * hz / 48_000.0;
            0.9 * (-seconds / 0.05).exp() * phase.sin()
        })
        .collect();
    let mut pcm = vec![0.0_f32; consts::KICK_BODY_FRAMES * 2];
    for index in 0..8 {
        let start = 4_800 + index * 24_000;
        for (frame, sample) in kick.iter().enumerate() {
            let sample = sample.to_f32().expect("kick sample fits f32");
            pcm[(start + frame) * 2..(start + frame + 1) * 2].fill(sample);
        }
    }
    pcm.into_iter().flat_map(f32::to_le_bytes).collect()
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream")]
#[case::default()]
fn warp_native_tone() -> Vec<u8> {
    (0..consts::WARP_FRAMES)
        .flat_map(|frame| {
            let phase = frame.to_f64().expect("position") * std::f64::consts::TAU * 440.0 / 8_000.0;
            let sample = (0.48 * phase.sin()).to_f32().expect("sample");
            [sample; 2]
        })
        .flat_map(f32::to_le_bytes)
        .collect()
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream", format = metronome_format)]
#[case::default()]
fn host_metronome_held() -> Vec<u8> {
    kithara_fixture_media::signal::metronome_held(f32::from(
        kithara_effects::LimiterConfig::default().ceiling(),
    ))
    .into_iter()
    .flat_map(f32::to_le_bytes)
    .collect()
}

#[kithara::asset(
    ext = "f32le", content_type = "application/octet-stream",
    format = metronome_format,
    depends_on = ["host_metronome_held_default"]
)]
#[case::default()]
fn host_metronome_tone(inputs: &[&[u8]]) -> Vec<u8> {
    let held: Vec<_> = inputs[0]
        .as_chunks::<4>()
        .0
        .iter()
        .map(|sample| f32::from_le_bytes(*sample))
        .collect();
    kithara_fixture_media::signal::metronome_tone(&held)
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect()
}

fn metronome_format() -> Vec<u8> {
    let mut format = b"metronome-mix:v1;ceiling=".to_vec();
    format.extend_from_slice(
        &f32::from(kithara_effects::LimiterConfig::default().ceiling()).to_le_bytes(),
    );
    format
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream")]
#[case::outgoing(false)]
#[case::incoming(true)]
fn render_crossfade_pcm(incoming: bool) -> Vec<u8> {
    (0..177_524_usize)
        .flat_map(|frame| {
            let sample = if incoming {
                0.125 + (frame % 89).to_f32().expect("fixture frame") * 0.0007
            } else {
                0.25 + (frame % 97).to_f32().expect("fixture frame") * 0.0005
            };
            [sample; 2]
        })
        .flat_map(f32::to_le_bytes)
        .collect()
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream")]
#[case::default()]
fn render_readiness_pcm() -> Vec<u8> {
    (0_u16..4_096)
        .flat_map(|frame| [f32::from(frame), -f32::from(frame)])
        .flat_map(f32::to_le_bytes)
        .collect()
}

#[kithara::asset(ext = "f32le", content_type = "application/octet-stream")]
#[case::before(0)]
#[case::after(1_024)]
fn render_seek_pcm(start: u16) -> Vec<u8> {
    (start..start + 1_024)
        .flat_map(|frame| [f32::from(frame); 2])
        .flat_map(f32::to_le_bytes)
        .collect()
}

#[cfg(feature = "encoded")]
#[kithara::asset(ext = "f32le", content_type = "application/octet-stream")]
#[case::default()]
fn decoder_opus_pcm() -> Vec<u8> {
    (0_u16..960)
        .flat_map(|frame| {
            let sample = (f32::from(frame) * std::f32::consts::TAU * 440.0 / 48_000.0).sin() * 0.5;
            [sample; 2]
        })
        .flat_map(f32::to_le_bytes)
        .collect()
}

#[cfg(feature = "encoded")]
#[kithara::asset(
    ext = "bin", content_type = "application/octet-stream", fragment,
    format = opus_packet_format,
    depends_on = ["decoder_opus_pcm_default"]
)]
#[case::default()]
fn decoder_opus_packet(inputs: &[&[u8]]) -> Vec<u8> {
    let input: Vec<f32> = inputs[0]
        .as_chunks::<4>()
        .0
        .iter()
        .map(|bytes| f32::from_le_bytes(*bytes))
        .collect();
    let mut encoder = opus::Encoder::new(48_000, opus::Channels::Stereo, opus::Application::Audio)
        .expect("Opus encoder");
    let mut encoded = [0; 4_000];
    let len = encoder
        .encode_float(&input, &mut encoded)
        .expect("encode packet");
    encoded[..len].to_vec()
}

#[cfg(feature = "encoded")]
fn opus_packet_format() -> Vec<u8> {
    format!(
        "opus-packet:v1;encoder={};rate=48000;channels=2;application=audio;frames=960",
        opus::version()
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use num_traits::{AsPrimitive, ToPrimitive};

    use super::*;

    #[kithara::test(native)]
    #[case::chirp(true)]
    #[case::sine(false)]
    fn prepared_render_signal_preserves_every_original_sample_bit(#[case] chirp: bool) {
        let frames = 49_152_usize;
        let rate = 44_100.0_f64;
        let length: f64 = frames.as_();
        let span = length / rate;
        let original: Vec<f32> = (0..frames)
            .flat_map(|frame| {
                let frame: f64 = frame.as_();
                let phase = if chirp {
                    let time = frame / rate;
                    std::f64::consts::TAU
                        * time.mul_add(220.0, (1_760.0 - 220.0) / (2.0 * span) * time * time)
                } else {
                    std::f64::consts::TAU * 440.0 * frame / 44_100.0
                };
                let sample: f32 = (0.5 * phase.sin()).as_();
                [sample, sample]
            })
            .collect();
        let prepared = render_signal(chirp);
        assert_sample_bits(&prepared, &original);
    }

    #[kithara::test(native)]
    #[case::outgoing(false)]
    #[case::incoming(true)]
    fn prepared_host_pcm_and_wav_preserve_the_original_writer_bytes(#[case] incoming: bool) {
        let original: Vec<f32> = (0..529_200_usize)
            .flat_map(|frame| {
                if incoming {
                    let phase =
                        frame.to_f64().expect("fixture frame") * 330.0 * std::f64::consts::TAU
                            / f64::from(44_100);
                    [
                        0.0,
                        (0.125 * phase.sin()).to_f32().expect("fixture amplitude"),
                    ]
                } else {
                    let phase =
                        frame.to_f64().expect("fixture frame") * 220.0 * std::f64::consts::TAU
                            / f64::from(44_100);
                    [
                        (0.2 * phase.sin()).to_f32().expect("fixture amplitude"),
                        0.0,
                    ]
                }
            })
            .collect();
        let prepared = host_manual_pcm(incoming);
        let asset = host_manual_wav(&[&prepared]);
        assert_sample_bits(&prepared, &original);

        let data_bytes = u32::try_from(original.len() * size_of::<f32>()).expect("WAV data size");
        let mut expected = Vec::new();
        expected.extend_from_slice(b"RIFF");
        expected.extend_from_slice(&(36 + data_bytes).to_le_bytes());
        expected.extend_from_slice(b"WAVEfmt ");
        expected.extend_from_slice(&16_u32.to_le_bytes());
        expected.extend_from_slice(&3_u16.to_le_bytes());
        expected.extend_from_slice(&2_u16.to_le_bytes());
        expected.extend_from_slice(&44_100_u32.to_le_bytes());
        expected.extend_from_slice(&352_800_u32.to_le_bytes());
        expected.extend_from_slice(&8_u16.to_le_bytes());
        expected.extend_from_slice(&32_u16.to_le_bytes());
        expected.extend_from_slice(b"data");
        expected.extend_from_slice(&data_bytes.to_le_bytes());
        for sample in original {
            expected.extend_from_slice(&sample.to_le_bytes());
        }
        assert_eq!(expected.len(), 4_233_644);
        assert_eq!(asset, expected);
    }

    #[kithara::test(native)]
    fn prepared_stretch_kick_body_preserves_every_original_sample_bit() {
        let rate = f64::from(48_000);
        let mut phase = 0.0;
        let kick: Vec<f64> = (0..2_400)
            .map(|frame| {
                let seconds = frame.to_f64().expect("frame fits f64") / rate;
                let hz = 50.0 + 100.0 * (-seconds / 0.02).exp();
                phase += std::f64::consts::TAU * hz / rate;
                0.9 * (-seconds / 0.05).exp() * phase.sin()
            })
            .collect();
        let frames = 4_800 + (8 + 4) * 24_000;
        let mut original = vec![0.0_f32; frames * 2];
        for index in 0..8 {
            let start = 4_800 + index * 24_000;
            for (frame, sample) in kick.iter().enumerate() {
                let sample = sample.to_f32().expect("kick sample fits f32");
                original[(start + frame) * 2..(start + frame + 1) * 2].fill(sample);
            }
        }
        assert_sample_bits(&stretch_kick_body(), &original);
    }

    #[kithara::test(native)]
    fn prepared_warp_tone_preserves_every_original_f64_law_sample_bit() {
        let original: Vec<f32> = (0..8_192_u64)
            .flat_map(|position| {
                let phase = position.to_f64().expect("position") * std::f64::consts::TAU * 440.0
                    / f64::from(8_000);
                let sample = (0.48 * phase.sin()).to_f32().expect("sample");
                [sample; 2]
            })
            .collect();
        assert_sample_bits(&warp_native_tone(), &original);
    }

    #[kithara::test(native)]
    fn prepared_metronome_mixes_preserve_the_original_ceiling_and_sample_bits() {
        let ceiling = f32::from(kithara_effects::LimiterConfig::default().ceiling());
        let held: Vec<f32> = (0..8_192_u16)
            .map(|frame| {
                let rise = (f32::from(frame) / 512.0).min(1.0);
                ceiling * 0.5 * (1.0 - (std::f32::consts::PI * rise).cos())
            })
            .collect();
        let tone: Vec<f32> = held
            .iter()
            .zip(0..8_192_u16)
            .map(|(level, frame)| {
                let cycles = (f64::from(frame) * 10_000.0 / f64::from(44_100)).fract();
                let tone = f64::from(*level) * std::f64::consts::TAU.mul_add(cycles, 2.1).cos();
                tone.to_f32().expect("a tone sample fits f32")
            })
            .collect();
        let prepared_held = host_metronome_held();
        assert_sample_bits(&prepared_held, &held);
        assert_sample_bits(&host_metronome_tone(&[&prepared_held]), &tone);
    }

    #[kithara::test(native)]
    fn both_metronome_assets_key_the_observed_canonical_ceiling() {
        let mut expected = b"metronome-mix:v1;ceiling=".to_vec();
        expected.extend_from_slice(
            &f32::from(kithara_effects::LimiterConfig::default().ceiling()).to_le_bytes(),
        );
        for name in ["host_metronome_held_default", "host_metronome_tone_default"] {
            let definition = inventory::iter::<crate::registry::AssetDef>
                .into_iter()
                .find(|definition| definition.accessor_name() == name)
                .expect("registered metronome asset");
            assert_eq!(
                definition.format.expect("ceiling keys the prepared mix")(),
                expected
            );
            if name == "host_metronome_tone_default" {
                assert_eq!(definition.dependencies, &["host_metronome_held_default"]);
            }
        }
    }

    #[kithara::test(native)]
    #[case::outgoing(false)]
    #[case::incoming(true)]
    fn prepared_crossfade_sources_preserve_every_original_sample_bit(#[case] incoming: bool) {
        let end = 512 + 100 + 4 * 44_100 + 512;
        let original: Vec<f32> = (0..end)
            .flat_map(|frame| {
                let sample = if incoming {
                    0.125
                        + num_traits::cast::<usize, f32>(frame % 89).expect("fixture frame")
                            * 0.0007
                } else {
                    0.25 + num_traits::cast::<usize, f32>(frame % 97).expect("fixture frame")
                        * 0.0005
                };
                [sample, sample]
            })
            .collect();
        assert_eq!(original.len(), 177_524 * 2);
        assert_sample_bits(&render_crossfade_pcm(incoming), &original);
    }

    #[kithara::test(native)]
    fn prepared_readiness_source_preserves_all_signed_ramp_bits() {
        let original: Vec<f32> = (0_u16..4_096)
            .flat_map(|frame| [f32::from(frame), -f32::from(frame)])
            .collect();
        assert_eq!(original[1].to_bits(), (-0.0_f32).to_bits());
        assert_sample_bits(&render_readiness_pcm(), &original);
    }

    #[kithara::test(native)]
    #[case::before(0)]
    #[case::after(1_024)]
    fn prepared_seek_sources_preserve_every_original_ramp_bit(#[case] start: u16) {
        let original: Vec<f32> = (start..start + 1_024)
            .flat_map(|frame| [f32::from(frame), f32::from(frame)])
            .collect();
        assert_sample_bits(&render_seek_pcm(start), &original);
    }

    #[cfg(feature = "encoded")]
    #[kithara::test(native)]
    fn prepared_opus_packet_preserves_the_original_pcm_and_encoder_bytes() {
        let input: Vec<f32> = (0_u16..960)
            .flat_map(|frame| {
                let sample =
                    (f32::from(frame) * std::f32::consts::TAU * 440.0 / 48_000.0).sin() * 0.5;
                [sample, sample]
            })
            .collect();
        let prepared = decoder_opus_pcm();
        assert_sample_bits(&prepared, &input);
        let mut encoder =
            opus::Encoder::new(48_000, opus::Channels::Stereo, opus::Application::Audio)
                .expect("Opus encoder");
        let mut encoded = [0; 4_000];
        let len = encoder
            .encode_float(&input, &mut encoded)
            .expect("encode packet");
        assert_eq!(decoder_opus_packet(&[&prepared]), &encoded[..len]);
    }

    #[cfg(feature = "encoded")]
    #[kithara::test(native)]
    fn opus_packet_keys_its_build_encoder_profile_and_prepared_pcm() {
        let definition = inventory::iter::<crate::registry::AssetDef>
            .into_iter()
            .find(|definition| definition.accessor_name() == "decoder_opus_packet_default")
            .expect("registered Opus packet");
        assert_eq!(definition.dependencies, &["decoder_opus_pcm_default"]);
        let expected = format!(
            "opus-packet:v1;encoder={};rate=48000;channels=2;application=audio;frames=960",
            opus::version()
        );
        assert_eq!(
            definition.format.expect("encoder profile keys the packet")(),
            expected.as_bytes()
        );
    }

    fn assert_sample_bits(prepared: &[u8], original: &[f32]) {
        assert_eq!(prepared.len(), size_of_val(original));
        for (index, (actual, expected)) in
            prepared.as_chunks::<4>().0.iter().zip(original).enumerate()
        {
            assert_eq!(*actual, expected.to_le_bytes(), "sample {index}");
        }
    }
}
