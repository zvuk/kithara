#![cfg(all(feature = "native-fixtures", not(target_arch = "wasm32")))]

#[cfg(all(test, target_os = "android"))]
use kithara_test_dylib as _;
use kithara_test_fixtures::{asset::Asset, assets, unit_fixtures};
use kithara_test_utils::kithara;
use num_traits::AsPrimitive;

#[kithara::test(native)]
#[case::cd_half_1(assets::fixed_constant_wav_cd_half_1, 44_100, 1, 0.5)]
#[case::cd_half_441(assets::fixed_constant_wav_cd_half_441, 44_100, 441, 0.5)]
#[case::cd_half_900(assets::fixed_constant_wav_cd_half_900, 44_100, 900, 0.5)]
#[case::cd_half_1024(assets::fixed_constant_wav_cd_half_1024, 44_100, 1_024, 0.5)]
#[case::cd_half_4096(assets::fixed_constant_wav_cd_half_4096, 44_100, 4_096, 0.5)]
#[case::cd_half_4410(assets::fixed_constant_wav_cd_half_4410, 44_100, 4_410, 0.5)]
#[case::cd_half_22050(assets::fixed_constant_wav_cd_half_22050, 44_100, 22_050, 0.5)]
#[case::cd_half_44100(assets::fixed_constant_wav_cd_half_44100, 44_100, 44_100, 0.5)]
#[case::cd_half_52224(assets::fixed_constant_wav_cd_half_52224, 44_100, 52_224, 0.5)]
#[case::cd_half_55296(assets::fixed_constant_wav_cd_half_55296, 44_100, 55_296, 0.5)]
#[case::cd_half_66150(assets::fixed_constant_wav_cd_half_66150, 44_100, 66_150, 0.5)]
#[case::cd_half_220500(assets::fixed_constant_wav_cd_half_220500, 44_100, 220_500, 0.5)]
#[case::cd_half_441000(assets::fixed_constant_wav_cd_half_441000, 44_100, 441_000, 0.5)]
#[case::cd_half_2646000(assets::fixed_constant_wav_cd_half_2646000, 44_100, 2_646_000, 0.5)]
#[case::cd_quarter_882(assets::fixed_constant_wav_cd_quarter_882, 44_100, 882, 0.25)]
#[case::cd_quarter_4096(assets::fixed_constant_wav_cd_quarter_4096, 44_100, 4_096, 0.25)]
#[case::cd_quarter_4410(assets::fixed_constant_wav_cd_quarter_4410, 44_100, 4_410, 0.25)]
#[case::cd_quarter_44100(assets::fixed_constant_wav_cd_quarter_44100, 44_100, 44_100, 0.25)]
#[case::cd_quarter_52224(assets::fixed_constant_wav_cd_quarter_52224, 44_100, 52_224, 0.25)]
#[case::cd_tenth_1323000(assets::fixed_constant_wav_cd_tenth_1323000, 44_100, 1_323_000, 0.1)]
#[case::cd_fifth_1323000(assets::fixed_constant_wav_cd_fifth_1323000, 44_100, 1_323_000, 0.2)]
#[case::cd_three_tenths_1323000(
    assets::fixed_constant_wav_cd_three_tenths_1323000,
    44_100,
    1_323_000,
    0.3
)]
#[case::cd_two_fifths_1323000(
    assets::fixed_constant_wav_cd_two_fifths_1323000,
    44_100,
    1_323_000,
    0.4
)]
#[case::cd_unity_1323000(assets::fixed_constant_wav_cd_unity_1323000, 44_100, 1_323_000, 1.0)]
#[case::render_half_4096(assets::fixed_constant_wav_render_half_4096, 48_000, 4_096, 0.5)]
#[case::render_half_144000(assets::fixed_constant_wav_render_half_144000, 48_000, 144_000, 0.5)]
#[case::render_half_2880000(assets::fixed_constant_wav_render_half_2880000, 48_000, 2_880_000, 0.5)]
#[case::render_quarter_2880000(
    assets::fixed_constant_wav_render_quarter_2880000,
    48_000,
    2_880_000,
    0.25
)]
fn fixed_constant_wav_matches_every_original_writer_byte(
    #[case] asset: fn() -> Asset,
    #[case] rate: u32,
    #[case] frames: usize,
    #[case] level: f32,
) {
    let mut original = original_header(3, 32, rate, 2, frames * 8);
    for _ in 0..frames * 2 {
        original.extend_from_slice(&level.to_le_bytes());
    }
    assert_eq!(original.len(), 44 + frames * 8);
    assert_asset_bytes(&asset(), &original);
}

#[kithara::test(native)]
fn broadcast_wav_retains_the_previous_mono_reader_duplication() {
    let source = assets::broadcast_tone_default().bytes();
    assert_eq!(source.len(), 264_600 * size_of::<f32>());
    let mut original = original_header(3, 32, 44_100, 2, 264_600 * 8);
    for sample in source.as_chunks::<4>().0 {
        original.extend_from_slice(sample);
        original.extend_from_slice(sample);
    }
    assert_eq!(original.len(), 2_116_844);
    assert_asset_bytes(&unit_fixtures::broadcast_tone_wav(), &original);
}

#[kithara::test(native)]
fn fused_seam_second_wav_preserves_the_previous_reader_tail_duplication() {
    let source = assets::beat_input_fused_seam().bytes();
    assert_eq!(source.len(), 96_000 * size_of::<f32>());
    let tail = &source[48_000 * size_of::<f32>()..];
    let mut original = original_header(3, 32, 48_000, 2, 48_000 * 8);
    for sample in tail.as_chunks::<4>().0 {
        original.extend_from_slice(sample);
        original.extend_from_slice(sample);
    }
    assert_eq!(original.len(), 384_044);
    assert_asset_bytes(&assets::fused_seam_second_wav_default(), &original);
}

#[kithara::test(native)]
#[case::full(false)]
#[case::half(true)]
fn metronome_sine_wav_preserves_the_previous_reader_amplitude_and_duplication(#[case] half: bool) {
    let source = assets::beat_input_sine_440_long().bytes();
    assert_eq!(source.len(), 882_000 * 2 * size_of::<f32>());
    let mut original = original_header(3, 32, 44_100, 2, 882_000 * 8);
    for frame in source.as_chunks::<8>().0 {
        let sample = f32::from_le_bytes(frame[..4].try_into().expect("complete source sample"));
        let sample = if half { sample / 2.0 } else { sample };
        original.extend_from_slice(&sample.to_le_bytes());
        original.extend_from_slice(&sample.to_le_bytes());
    }
    assert_eq!(original.len(), 7_056_044);
    assert_asset_bytes(&unit_fixtures::metronome_sine_wav(half), &original);
}

#[kithara::test(native)]
#[case::cd_plain(44_100, false)]
#[case::cd_decorated(44_100, true)]
#[case::render_plain(48_000, false)]
#[case::render_decorated(48_000, true)]
fn cochlea_control_wav_retains_all_24_seconds_and_original_sample_bits(
    #[case] sample_rate: u32,
    #[case] decorated: bool,
) {
    let period = f64::from(sample_rate) * 0.5;
    let sample_rate_frames: usize = sample_rate.as_();
    let frames = sample_rate_frames * 24;
    let mut original = original_header(3, 32, sample_rate, 2, frames * 8);
    for frame in 0..frames {
        let frame: f64 = frame.as_();
        let beat = frame / period;
        let phase = beat.fract();
        let ordinal: usize = beat.floor().as_();
        let level = if ordinal.is_multiple_of(4) { 0.9 } else { 0.7 };
        let sample: f32 = if decorated && ordinal.is_multiple_of(3) && (0.3..0.306).contains(&phase)
        {
            0.65
        } else if phase < 0.12 {
            (level * (1.0 - phase / 0.12)).as_()
        } else {
            0.0
        };
        original.extend_from_slice(&sample.to_le_bytes());
        original.extend_from_slice(&sample.to_le_bytes());
    }
    assert_eq!(
        original.len(),
        if sample_rate == 44_100 {
            8_467_244
        } else {
            9_216_044
        }
    );
    assert_asset_bytes(
        &unit_fixtures::cochlea_control_wav(sample_rate, decorated),
        &original,
    );
}

#[cfg(feature = "encoded")]
#[kithara::test(native)]
#[case::plain(false)]
#[case::tagged(true)]
fn mp3_wave_and_tagged_elementary_fragments_preserve_every_wrapper_byte(#[case] tagged: bool) {
    let audio = assets::rhythm_mp3_deck_a_120bpm_48k().bytes();
    let mut wave = Vec::new();
    if tagged {
        let len = u32::try_from(audio.len()).expect("fixture fits an ID3 tag size");
        let syncsafe = (len & 0x0fe0_0000) << 3
            | (len & 0x001f_c000) << 2
            | (len & 0x0000_3f80) << 1
            | (len & 0x0000_007f);
        wave.extend_from_slice(&[b'I', b'D', b'3', 4, 0, 0]);
        wave.extend_from_slice(&syncsafe.to_be_bytes());
        wave.extend_from_slice(audio);
    }
    let metadata_prefix = wave.clone();
    if tagged {
        assert_asset_bytes(&assets::decoder_mp3_prefix_default(), &metadata_prefix);
    }
    wave.extend_from_slice(b"RIFF");
    wave.extend_from_slice(
        &u32::try_from(audio.len() + 36)
            .expect("RIFF length")
            .to_le_bytes(),
    );
    wave.extend_from_slice(b"WAVEfmt ");
    wave.extend_from_slice(&16_u32.to_le_bytes());
    wave.extend_from_slice(&0x55_u16.to_le_bytes());
    wave.extend_from_slice(&2_u16.to_le_bytes());
    wave.extend_from_slice(&48_000_u32.to_le_bytes());
    wave.extend_from_slice(&16_000_u32.to_le_bytes());
    wave.extend_from_slice(&1_u16.to_le_bytes());
    wave.extend_from_slice(&0_u16.to_le_bytes());
    wave.extend_from_slice(b"data");
    wave.extend_from_slice(
        &u32::try_from(audio.len())
            .expect("payload length")
            .to_le_bytes(),
    );
    wave.extend_from_slice(audio);
    let prepared = if tagged {
        assets::decoder_mp3_wave_tagged()
    } else {
        assets::decoder_mp3_wave_plain()
    };
    assert_eq!(
        wave.len(),
        if tagged {
            54 + 2 * audio.len()
        } else {
            44 + audio.len()
        }
    );
    assert_asset_bytes(&prepared, &wave);
    if tagged {
        let mut elementary = metadata_prefix;
        elementary.extend_from_slice(audio);
        assert_eq!(elementary.len(), 10 + 2 * audio.len());
        assert_asset_bytes(&assets::decoder_mp3_tagged_default(), &elementary);
    }
}

#[kithara::test(native)]
fn float64_multichannel_fragment_preserves_every_original_writer_byte() {
    const CHANNELS: u16 = 18;
    const RATE: u32 = 48_000;
    const FRAMES: usize = 1_152 * 3 + 17;
    let data_size = FRAMES * usize::from(CHANNELS) * 8;
    let mut original = original_header(3, 64, RATE, CHANNELS, data_size);
    for index in 0..FRAMES * usize::from(CHANNELS) {
        let sample = f64::from(u16::try_from(index % 31).expect("bounded sample")) / 31.0;
        original.extend_from_slice(&sample.to_le_bytes());
    }
    assert_eq!(original.len(), 500_156);
    assert_asset_bytes(&assets::decoder_float64_wav_default(), &original);
}

fn original_header(format: u16, bits: u16, rate: u32, channels: u16, data_size: usize) -> Vec<u8> {
    let data_bytes = u32::try_from(data_size).expect("WAV data extent");
    let block_align = channels * (bits / 8);
    let mut wav = b"RIFF".to_vec();
    wav.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&format.to_le_bytes());
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&rate.to_le_bytes());
    wav.extend_from_slice(&(rate * u32::from(block_align)).to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&bits.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_bytes.to_le_bytes());
    assert_eq!(wav.len(), 44);
    wav
}

fn assert_asset_bytes(asset: &Asset, original: &[u8]) {
    assert_eq!(asset.bytes(), original);
    assert_eq!(
        std::fs::read(asset.path().expect("prepared asset path")).expect("prepared asset bytes"),
        original
    );
}
