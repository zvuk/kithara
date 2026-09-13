use kithara_encode::{EncoderFactory, PackagedEncodeRequest, PcmSource};
use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};
use kithara_test_macros as kithara;
use num_traits::ToPrimitive;

use crate::{
    context::BuildContext,
    defs::packaged::pools,
    fmp4::{Fmp4Package, GaplessEncoding, mux_audio_track},
    signal::{Pcm, SweepMode, Wave},
    variant_input::{VariantArtifact, VariantCatalog, VariantInput},
};

struct DelayPaddedPcm<'a> {
    inner: &'a dyn PcmSource,
    encoder_delay_frames: usize,
    trailing_delay_frames: usize,
}

impl DelayPaddedPcm<'_> {
    fn bytes_per_frame(&self) -> usize {
        usize::from(self.inner.channels()) * size_of::<i16>()
    }

    fn encoder_delay_bytes(&self) -> usize {
        self.encoder_delay_frames
            .saturating_mul(self.bytes_per_frame())
    }

    fn trailing_delay_bytes(&self) -> usize {
        self.trailing_delay_frames
            .saturating_mul(self.bytes_per_frame())
    }
}

impl PcmSource for DelayPaddedPcm<'_> {
    delegate::delegate! {
        to self.inner {
            fn channels(&self) -> u16;
            fn sample_rate(&self) -> u32;
        }
    }

    fn read_pcm_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        let Some(total_len) = self.total_byte_len() else {
            return 0;
        };
        if offset >= total_len || buf.is_empty() {
            return 0;
        }

        let writable = (total_len - offset).min(buf.len());
        let window = &mut buf[..writable];
        window.fill(0);

        let encoder_delay_bytes = self.encoder_delay_bytes();
        let inner_len = self.inner.total_byte_len().unwrap_or(0);
        let inner_start = encoder_delay_bytes;
        let inner_end = inner_start.saturating_add(inner_len);
        let copy_start = offset.max(inner_start);
        let copy_end = (offset + writable).min(inner_end);

        if copy_start < copy_end {
            let inner_offset = copy_start - inner_start;
            let dst_offset = copy_start - offset;
            let dst_end = dst_offset + (copy_end - copy_start);
            let _ = self
                .inner
                .read_pcm_at(inner_offset, &mut window[dst_offset..dst_end]);
        }

        writable
    }

    fn total_byte_len(&self) -> Option<usize> {
        self.inner.total_byte_len().map(|inner_len| {
            inner_len
                .saturating_add(self.encoder_delay_bytes())
                .saturating_add(self.trailing_delay_bytes())
        })
    }
}

fn encode(input: &VariantInput) -> Fmp4Package {
    let pcm = Pcm::from_fn(
        input.sample_rate,
        input.channels,
        input.content_frames,
        |frame| {
            input
                .signal
                .sample(frame.saturating_add(input.start_frame), input.sample_rate)
        },
    );
    let padded = DelayPaddedPcm {
        inner: &pcm,
        encoder_delay_frames: input.encoder_delay as usize,
        trailing_delay_frames: input.trailing_delay as usize,
    };
    let media_info = MediaInfo::builder()
        .codec(input.codec)
        .container(ContainerFormat::Fmp4)
        .sample_rate(input.sample_rate)
        .channels(input.channels)
        .build();
    let track = EncoderFactory::encode_packaged(
        &pools(),
        &PackagedEncodeRequest::builder()
            .pcm(&padded)
            .packets_per_segment(input.packets_per_segment)
            .media_info(media_info)
            .timescale(input.timescale)
            .bit_rate(input.bit_rate)
            .encoder_delay(input.encoder_delay)
            .trailing_delay(input.trailing_delay)
            .build(),
    )
    .unwrap_or_else(|error| panic!("HLS fixture {}: {error}", input.key()));
    mux_audio_track(&track, input.gapless_encoding)
        .unwrap_or_else(|error| panic!("HLS fixture {}: {error}", input.key()))
}

#[kithara::asset(
    ext = "toml",
    content_type = "application/x-kithara-hls-variants",
    context
)]
#[case::catalog_with_native_gapless()]
fn hls_variants(context: &BuildContext<'_>) -> Vec<u8> {
    let mut catalog = VariantCatalog::default();
    for input in inputs() {
        let key = input.key();
        if catalog.variants.contains_key(&key) {
            continue;
        }
        catalog
            .frame_samples
            .entry(format!("{:?}", input.codec))
            .or_insert_with(|| {
                EncoderFactory::frame_samples(input.codec).expect("prepared codec frame size")
            });
        let package = encode(&input);
        let init = context
            .store(&format!("{key}/init"), "mp4", &package.init_segment)
            .expect("store prepared HLS init");
        let media = package
            .media_segments
            .iter()
            .enumerate()
            .map(|(index, bytes)| {
                context
                    .store(&format!("{key}/{index}"), "m4s", bytes)
                    .expect("store prepared HLS segment")
            })
            .collect();
        catalog.variants.insert(
            key,
            VariantArtifact {
                init,
                media,
                durations: package.segment_durations_secs,
            },
        );
    }
    toml::to_string(&catalog)
        .expect("serialize HLS variant catalog")
        .into_bytes()
}

struct Profile {
    codecs: Vec<AudioCodec>,
    segments: usize,
    seconds: f64,
    sample_rate: u32,
    signal: Wave,
    bit_rates: Vec<u64>,
    start_frame: usize,
    encoder_delay: u32,
    trailing_delay: u32,
    gapless: GaplessEncoding,
}

impl Profile {
    fn new(codecs: &[AudioCodec], segments: usize, seconds: f64) -> Self {
        Self {
            codecs: codecs.to_vec(),
            segments,
            seconds,
            sample_rate: 44_100,
            signal: Wave::Sawtooth,
            bit_rates: vec![
                match codecs.first().expect("fixture has a codec") {
                    AudioCodec::AacLc => 128_000,
                    AudioCodec::AacHe => 64_000,
                    AudioCodec::AacHeV2 => 32_000,
                    AudioCodec::Flac => 512_000,
                    _ => panic!("unsupported fixture codec"),
                };
                codecs.len()
            ],
            start_frame: 0,
            encoder_delay: 0,
            trailing_delay: 0,
            gapless: GaplessEncoding::None,
        }
    }

    fn inputs(self) -> Vec<VariantInput> {
        let requested = (self.seconds * f64::from(self.sample_rate))
            .round()
            .to_usize()
            .expect("fixture frame count fits usize");
        let frames = self
            .codecs
            .iter()
            .map(|codec| EncoderFactory::frame_samples(*codec).expect("packaged codec frame size"))
            .collect::<Vec<_>>();
        let quantum = frames.iter().copied().fold(1, |common, frame| {
            let (mut a, mut b) = (common, frame);
            while b != 0 {
                (a, b) = (b, a % b);
            }
            common / a * frame
        });
        let segment_frames = requested.div_ceil(quantum).max(1) * quantum;
        self.codecs
            .iter()
            .copied()
            .enumerate()
            .map(|(index, codec)| {
                let content_frames = segment_frames * self.segments;
                let total =
                    content_frames + self.encoder_delay as usize + self.trailing_delay as usize;
                let aligned = total.div_ceil(frames[index]) * frames[index];
                VariantInput::builder()
                    .codec(codec)
                    .sample_rate(self.sample_rate)
                    .channels(2)
                    .content_frames(content_frames)
                    .packets_per_segment(segment_frames / frames[index])
                    .signal(self.signal)
                    .start_frame(self.start_frame)
                    .bit_rate(self.bit_rates[index])
                    .timescale(self.sample_rate)
                    .encoder_delay(self.encoder_delay)
                    .trailing_delay(
                        self.trailing_delay
                            + u32::try_from(aligned - total).expect("padding fits u32"),
                    )
                    .gapless_encoding(self.gapless)
                    .build()
            })
            .collect()
    }
}

fn inputs() -> Vec<VariantInput> {
    let mut profiles = Vec::new();
    // Default AAC shapes used by HLS, queue, play, and regression suites.
    for (segments, seconds) in [
        (1, 2.0),
        (3, 4.0),
        (4, 1.0),
        (4, 4.0),
        (4, 2.0),
        (8, 2.0),
        (8, 4.0),
        (8, 0.5),
        (12, 6.0),
        (16, 4.0),
        (20, 3.0),
        (20, 4.0),
        (24, 2.0),
        (24, 3.0),
        (28, 6.0),
        (30, 4.0),
        (40, 4.0),
        (50, 4.0),
    ] {
        profiles.push(Profile::new(&[AudioCodec::AacLc], segments, seconds));
    }
    profiles.push(Profile::new(
        &[AudioCodec::AacLc, AudioCodec::Flac],
        37,
        6.0,
    ));
    profiles.push(Profile::new(
        &[AudioCodec::AacLc, AudioCodec::Flac],
        16,
        4.0,
    ));
    profiles.push(Profile::new(&[AudioCodec::AacHeV2], 30, 4.0));
    profiles.push(Profile::new(&[AudioCodec::AacHeV2], 8, 0.5));
    profiles.push(Profile::new(&[AudioCodec::Flac], 8, 0.5));
    profiles.push(Profile::new(&[AudioCodec::Flac], 6, 0.25));
    profiles.push(Profile::new(&[AudioCodec::Flac], 45, 2.0));
    for codec in [AudioCodec::AacLc, AudioCodec::Flac] {
        profiles.push(Profile::new(&[codec], 6, 2.0));
        let mut descending = Profile::new(&[codec], 4, 1.0);
        descending.signal = Wave::SawtoothDescending;
        profiles.push(descending);
    }
    // PCM identity stress profiles keep the original phase for every variant.
    for (segments, seconds) in [
        (3, 2.0),
        (4, 2.0),
        (8, 2.0),
        (6, 6.0),
        (50, 200_000.0 / 176_400.0),
    ] {
        for signal in [Wave::Sawtooth, Wave::SawtoothDescending] {
            let mut profile = Profile::new(&[AudioCodec::Flac], segments, seconds);
            profile.signal = signal;
            profiles.push(profile);
        }
    }
    profiles.push(Profile::new(
        &[AudioCodec::Flac],
        100,
        200_000.0 / 176_400.0,
    ));
    for signal in [Wave::Sawtooth, Wave::SawtoothDescending] {
        let mut profile = Profile::new(&[AudioCodec::AacLc], 4, 2.0);
        profile.signal = signal;
        profiles.push(profile);
    }
    for hz in [440.0, 880.0] {
        let mut profile = Profile::new(&[AudioCodec::AacLc], 4, 2.0);
        profile.signal = Wave::sine(hz);
        profiles.push(profile);
    }
    for (codecs, bit_rate) in [
        (vec![AudioCodec::AacLc], 320_000),
        (vec![AudioCodec::AacHeV2], 32_000),
        (vec![AudioCodec::Flac], 512_000),
        (vec![AudioCodec::AacLc, AudioCodec::Flac], 128_000),
        (vec![AudioCodec::AacLc, AudioCodec::Flac], 320_000),
    ] {
        let mut profile = Profile::new(&codecs, 30, 2.0);
        profile.signal = Wave::sine(440.0);
        profile.bit_rates.fill(bit_rate);
        profiles.push(profile);
    }
    for signal in [
        Wave::sine(441.0),
        Wave::sweep(80.0, 8_000.0, 16 * 22_050, SweepMode::Linear),
    ] {
        let mut profile = Profile::new(
            &[AudioCodec::AacLc, AudioCodec::AacLc, AudioCodec::Flac],
            16,
            0.5,
        );
        profile.signal = signal;
        profile.bit_rates = vec![64_000, 256_000, 768_000];
        profile.gapless = GaplessEncoding::None;
        profiles.push(profile);
    }
    let mut sweep = Profile::new(&[AudioCodec::AacLc], 12, 4.0);
    sweep.signal = Wave::sweep(1_000.0, 5_000.0, 12 * 4 * 44_100, SweepMode::Linear);
    profiles.push(sweep);
    profiles.extend(gapless_profiles());
    profiles.into_iter().flat_map(Profile::inputs).collect()
}

fn gapless_profiles() -> Vec<Profile> {
    let mut profiles = Vec::new();
    // Decoder metadata parity and gapless playback use 48 kHz AAC-LC.
    for gapless in [
        GaplessEncoding::None,
        GaplessEncoding::Edts,
        GaplessEncoding::ItunSmpb,
        GaplessEncoding::Both,
    ] {
        let mut profile = Profile::new(&[AudioCodec::AacLc], 3, 0.5);
        profile.sample_rate = 48_000;
        profile.signal = Wave::sine(1_000.0);
        profile.encoder_delay = 2_112;
        profile.trailing_delay = 960;
        profile.gapless = gapless;
        profiles.push(profile);
    }
    for (hz, start_frame, trailing_delay, gapless) in [
        (480.0, 0, 0, GaplessEncoding::Edts),
        (480.0, 0, 960, GaplessEncoding::Edts),
        (480.0, 69_632, 960, GaplessEncoding::Edts),
        (480.0, 67_888, 960, GaplessEncoding::Edts),
        (480.0, 0, 0, GaplessEncoding::None),
        (480.0, 0, 960, GaplessEncoding::None),
        (480.0, 69_632, 960, GaplessEncoding::None),
        (440.0, 0, 960, GaplessEncoding::Edts),
        (880.0, 0, 960, GaplessEncoding::Edts),
        (3_000.0, 67_888, 960, GaplessEncoding::Edts),
        (3_000.0, 69_632, 960, GaplessEncoding::Edts),
    ] {
        let mut profile = Profile::new(&[AudioCodec::AacLc], 3, 0.5);
        profile.sample_rate = 48_000;
        profile.signal = Wave::sine(hz);
        profile.start_frame = start_frame;
        profile.encoder_delay = 2_112;
        profile.trailing_delay = trailing_delay;
        profile.gapless = gapless;
        profiles.push(profile);
    }
    let mut startup = Profile::new(&[AudioCodec::AacLc], 6, 0.5);
    startup.sample_rate = 48_000;
    startup.signal = Wave::sine(880.0);
    startup.encoder_delay = 2_112;
    startup.trailing_delay = 960;
    startup.gapless = GaplessEncoding::Edts;
    profiles.push(startup);
    for start_frame in [0, 261_120] {
        let mut profile = Profile::new(&[AudioCodec::AacLc], 1, 5.921_088_435_374_15);
        profile.signal = Wave::sine(480.0);
        profile.start_frame = start_frame;
        profile.encoder_delay = 2_112;
        profile.gapless = GaplessEncoding::Edts;
        profiles.push(profile);
    }
    let mut padding = Profile::new(&[AudioCodec::AacLc], 8, 0.5);
    padding.encoder_delay = 2_112;
    padding.trailing_delay = 960;
    padding.gapless = GaplessEncoding::Edts;
    profiles.push(padding);
    profiles
}
