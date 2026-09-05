use std::{borrow::Cow, io, sync::OnceLock};

use aes::Aes128;
use cbc::{
    Encryptor,
    cipher::{BlockModeEncrypt, KeyIvInit, block_padding::Pkcs7},
};
use kithara_encode::{EncodedTrack, EncoderFactory, PackagedEncodeRequest};
use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};
use kithara_test_macros as kithara;
use rayon::prelude::*;

use crate::{
    context::BuildContext,
    defs::packaged::pools,
    fmp4::{Fmp4Package, GaplessEncoding, mux_audio_track_at},
    hls_manifest::{Manifest, Resource},
    signal::{Pcm, Wave},
};

struct Consts;

impl Consts {
    const CHANNELS: u16 = 2;
    const ENCODERS: usize = 4;
    const IV: [u8; 16] = [0; 16];
    const KEY: [u8; 16] = *b"0123456789abcdef";
    const SAMPLE_RATE: u32 = 44_100;
    const SEGMENT_MILLIS: u64 = 6_000;
    const SEGMENTS: usize = 37;
    const TARGET_DURATION: u8 = 6;
    const TOTAL_MILLIS: u64 = 220_200;
    const VARIANTS: [VariantSpec; 4] = [
        VariantSpec {
            bandwidth: 66_005,
            bit_rate: 64_000,
            codec: AudioCodec::AacLc,
            codecs: "mp4a.40.2",
            label: "slq",
        },
        VariantSpec {
            bandwidth: 134_107,
            bit_rate: 128_000,
            codec: AudioCodec::AacLc,
            codecs: "mp4a.40.2",
            label: "smq",
        },
        VariantSpec {
            bandwidth: 269_930,
            bit_rate: 256_000,
            codec: AudioCodec::AacLc,
            codecs: "mp4a.40.2",
            label: "shq",
        },
        VariantSpec {
            bandwidth: 988_758,
            bit_rate: 512_000,
            codec: AudioCodec::Flac,
            codecs: "fLaC",
            label: "slossless",
        },
    ];
}

struct GaplessConsts;

impl GaplessConsts {
    const BOUNDARY_MILLIS: [u64; 9] = [
        4_000, 8_000, 18_000, 28_000, 38_000, 48_000, 58_000, 68_000, 71_250,
    ];
    const MILLIS_PER_SECOND: u64 = 1_000;
    const TARGET_DURATION: u8 = 10;
}

struct RssConsts;

impl RssConsts {
    const SEGMENT_MILLIS: u64 = 4_000;
    const SEGMENTS: usize = 25;
    const TARGET_DURATION: u8 = 4;
}

#[derive(Clone, Copy)]
struct VariantSpec {
    bandwidth: u64,
    bit_rate: u64,
    codec: AudioCodec,
    codecs: &'static str,
    label: &'static str,
}

struct Variant {
    package: Fmp4Package,
    spec: VariantSpec,
}

struct EncodedVariant {
    spec: VariantSpec,
    track: EncodedTrack,
}

fn encode_track(
    spec: VariantSpec,
    total_frames: usize,
    packets_per_segment: usize,
) -> EncodedTrack {
    let pcm = Pcm::new(
        Consts::SAMPLE_RATE,
        Consts::CHANNELS,
        total_frames,
        Wave::Sawtooth,
    );
    let media_info = MediaInfo::builder()
        .codec(spec.codec)
        .container(ContainerFormat::Fmp4)
        .sample_rate(Consts::SAMPLE_RATE)
        .channels(Consts::CHANNELS)
        .build();
    let pools = pools();
    EncoderFactory::encode_packaged(
        &pools,
        &PackagedEncodeRequest::builder()
            .pcm(&pcm)
            .media_info(media_info)
            .timescale(Consts::SAMPLE_RATE)
            .bit_rate(spec.bit_rate)
            .packets_per_segment(packets_per_segment)
            .encoder_delay(0)
            .trailing_delay(0)
            .build(),
    )
    .unwrap_or_else(|error| {
        panic!(
            "kithara-test-fixtures: {:?} HLS encode failed: {error}",
            spec.codec
        )
    })
}

fn encode(spec: VariantSpec) -> EncodedVariant {
    let frame_samples = EncoderFactory::frame_samples(spec.codec).unwrap_or_else(|error| {
        panic!(
            "kithara-test-fixtures: {:?} frame size failed: {error}",
            spec.codec
        )
    });
    let segment_frames = usize::try_from(Consts::SAMPLE_RATE)
        .expect("invariant: sample rate fits usize")
        * usize::try_from(Consts::SEGMENT_MILLIS).expect("invariant: duration fits usize")
        / usize::try_from(GaplessConsts::MILLIS_PER_SECOND)
            .expect("invariant: millisecond scale fits usize");
    let packets_per_segment = segment_frames.div_ceil(frame_samples);
    let encoded_frames = usize::try_from(Consts::SAMPLE_RATE)
        .expect("invariant: sample rate fits usize")
        * usize::try_from(Consts::TOTAL_MILLIS).expect("invariant: duration fits usize")
        / usize::try_from(GaplessConsts::MILLIS_PER_SECOND)
            .expect("invariant: millisecond scale fits usize");
    // FFmpeg emits one native AAC priming access unit before the source frames.
    let total_frames = if spec.codec == AudioCodec::AacLc {
        encoded_frames
            .checked_sub(frame_samples)
            .expect("invariant: AAC fixture includes native priming")
    } else {
        encoded_frames
    };
    let track = encode_track(spec, total_frames, packets_per_segment);
    EncodedVariant { spec, track }
}

fn encoded_variants() -> &'static [EncodedVariant] {
    static ENCODED: OnceLock<Vec<EncodedVariant>> = OnceLock::new();
    ENCODED.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(Consts::ENCODERS)
            .build()
            .expect("invariant: fixture encoder pool builds")
            .install(|| Consts::VARIANTS.par_iter().copied().map(encode).collect())
    })
}

fn boundaries_at(track: &EncodedTrack, target_millis: impl IntoIterator<Item = u64>) -> Vec<usize> {
    let mut boundaries = Vec::new();
    let mut targets = target_millis.into_iter();
    let mut target = targets.next();
    let mut duration = 0u64;

    for (index, unit) in track.access_units.iter().enumerate() {
        let next_duration = duration.saturating_add(u64::from(unit.duration));
        while let Some(millis) = target {
            let target_duration =
                millis * u64::from(track.timescale) / GaplessConsts::MILLIS_PER_SECOND;
            if next_duration < target_duration {
                break;
            }
            let before = index;
            let after = index + 1;
            let boundary = if before > boundaries.last().copied().unwrap_or(0)
                && target_duration - duration <= next_duration - target_duration
            {
                before
            } else {
                after
            };
            boundaries.push(boundary);
            target = targets.next();
        }
        duration = next_duration;
    }
    assert!(
        target.is_none(),
        "invariant: gapless HLS reaches every boundary"
    );
    boundaries
}

fn long_boundaries(track: &EncodedTrack) -> Vec<usize> {
    boundaries_at(
        track,
        (1..Consts::SEGMENTS)
            .map(|index| {
                u64::try_from(index).expect("invariant: segment index fits u64")
                    * Consts::SEGMENT_MILLIS
            })
            .chain(std::iter::once(Consts::TOTAL_MILLIS)),
    )
}

fn gapless_boundaries(track: &EncodedTrack) -> Vec<usize> {
    boundaries_at(track, GaplessConsts::BOUNDARY_MILLIS)
}

fn rss_boundaries(track: &EncodedTrack) -> Vec<usize> {
    boundaries_at(
        track,
        (1..=RssConsts::SEGMENTS).map(|index| {
            u64::try_from(index).expect("invariant: RSS segment index fits u64")
                * RssConsts::SEGMENT_MILLIS
        }),
    )
}

fn package_variants(
    boundaries_for: impl Fn(&EncodedTrack) -> Vec<usize>,
    encoding: GaplessEncoding,
    label: &str,
) -> Vec<Variant> {
    encoded_variants()
        .iter()
        .map(|variant| {
            let boundaries = boundaries_for(&variant.track);
            let package =
                mux_audio_track_at(&variant.track, encoding, &boundaries).unwrap_or_else(|error| {
                    panic!(
                        "kithara-test-fixtures: {:?} {label} HLS mux failed: {error}",
                        variant.spec.codec
                    )
                });
            Variant {
                package,
                spec: variant.spec,
            }
        })
        .collect()
}

fn variants() -> &'static [Variant] {
    static PACKAGED: OnceLock<Vec<Variant>> = OnceLock::new();
    PACKAGED.get_or_init(|| package_variants(long_boundaries, GaplessEncoding::None, "long"))
}

fn gapless_variants() -> &'static [Variant] {
    static PACKAGED: OnceLock<Vec<Variant>> = OnceLock::new();
    PACKAGED.get_or_init(|| package_variants(gapless_boundaries, GaplessEncoding::Both, "gapless"))
}

fn rss_variants() -> &'static [Variant] {
    static PACKAGED: OnceLock<Vec<Variant>> = OnceLock::new();
    PACKAGED.get_or_init(|| package_variants(rss_boundaries, GaplessEncoding::None, "RSS"))
}

fn encrypt(bytes: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(bytes.len() + 16);
    output.extend_from_slice(bytes);
    output.resize(bytes.len() + 16, 0);
    Encryptor::<Aes128>::new((&Consts::KEY).into(), (&Consts::IV).into())
        .encrypt_padded::<Pkcs7>(&mut output, bytes.len())
        .expect("invariant: one padding block is reserved")
        .to_vec()
}

fn body(bytes: &[u8], encrypted: bool) -> Cow<'_, [u8]> {
    if encrypted {
        Cow::Owned(encrypt(bytes))
    } else {
        Cow::Borrowed(bytes)
    }
}

fn add(
    context: &BuildContext<'_>,
    resources: &mut Vec<Resource>,
    name: &str,
    content_type: &str,
    bytes: &[u8],
) -> io::Result<()> {
    let ext = name
        .rsplit_once('.')
        .map(|(_, ext)| ext)
        .ok_or_else(|| io::Error::other(format!("HLS resource `{name}` has no extension")))?;
    resources.push(Resource {
        content_type: content_type.to_owned(),
        file: context.store(name, ext, bytes)?,
        route: format!("/hls/{name}"),
    });
    Ok(())
}

fn media_playlist(variant: &Variant, encrypted: bool, target_duration: u8) -> String {
    let label = variant.spec.label;
    let mut playlist = format!(
        "#EXTM3U\n#EXT-X-TARGETDURATION:{}\n#EXT-X-ALLOW-CACHE:YES\n\
         #EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-VERSION:6\n#EXT-X-MEDIA-SEQUENCE:1\n",
        target_duration
    );
    if encrypted {
        playlist.push_str(&format!(
            "#EXT-X-KEY:METHOD=AES-128,URI={label}.key,IV=0x{}\n",
            hex::encode_upper(Consts::IV)
        ));
    }
    playlist.push_str(&format!("#EXT-X-MAP:URI=\"init-{label}-a1.mp4\"\n"));
    for (index, duration) in variant.package.segment_durations_secs.iter().enumerate() {
        playlist.push_str(&format!(
            "#EXTINF:{duration:.3},\nsegment-{}-{label}-a1.m4s\n",
            index + 1
        ));
    }
    playlist.push_str("#EXT-X-ENDLIST\n");
    playlist
}

fn master_playlist() -> String {
    let mut master = String::from("#EXTM3U\n");
    for spec in Consts::VARIANTS {
        master.push_str(&format!(
            "#EXT-X-STREAM-INF:PROGRAM-ID=1,BANDWIDTH={},CODECS=\"{}\",AVERAGE-BANDWIDTH={}\n\
             index-{}-a1.m3u8\n",
            spec.bandwidth, spec.codecs, spec.bandwidth, spec.label
        ));
    }
    master
}

fn bundle(
    context: &BuildContext<'_>,
    encrypted: bool,
    variants: &[Variant],
    target_duration: u8,
) -> io::Result<Vec<u8>> {
    let mut resources = Vec::new();
    for variant in variants {
        let label = variant.spec.label;
        if encrypted {
            add(
                context,
                &mut resources,
                &format!("{label}.key"),
                "application/octet-stream",
                &Consts::KEY,
            )?;
        }
        add(
            context,
            &mut resources,
            &format!("init-{label}-a1.mp4"),
            "audio/mp4",
            &body(&variant.package.init_segment, encrypted),
        )?;
        for (index, segment) in variant.package.media_segments.iter().enumerate() {
            add(
                context,
                &mut resources,
                &format!("segment-{}-{label}-a1.m4s", index + 1),
                "audio/mp4",
                &body(segment, encrypted),
            )?;
        }
        add(
            context,
            &mut resources,
            &format!("index-{label}-a1.m3u8"),
            "application/vnd.apple.mpegurl",
            media_playlist(variant, encrypted, target_duration).as_bytes(),
        )?;
    }
    add(
        context,
        &mut resources,
        "master.m3u8",
        "application/vnd.apple.mpegurl",
        master_playlist().as_bytes(),
    )?;
    resources.sort_by(|left, right| left.route.cmp(&right.route));
    toml::to_string(&Manifest {
        master: "/hls/master.m3u8".to_owned(),
        resources,
    })
    .map(String::into_bytes)
    .map_err(io::Error::other)
}

#[kithara::asset(
    ext = "toml",
    content_type = "application/x-kithara-hls-bundle",
    context
)]
#[case::plain(false)]
#[case::drm(true)]
fn long_hls(context: &BuildContext<'_>, encrypted: bool) -> Vec<u8> {
    bundle(context, encrypted, variants(), Consts::TARGET_DURATION)
        .unwrap_or_else(|error| panic!("kithara-test-fixtures: long HLS bundle failed: {error}"))
}

#[kithara::asset(
    ext = "toml",
    content_type = "application/x-kithara-hls-bundle",
    context
)]
#[case::plain(false)]
#[case::drm(true)]
fn gapless_hls(context: &BuildContext<'_>, encrypted: bool) -> Vec<u8> {
    bundle(
        context,
        encrypted,
        gapless_variants(),
        GaplessConsts::TARGET_DURATION,
    )
    .unwrap_or_else(|error| panic!("kithara-test-fixtures: gapless HLS bundle failed: {error}"))
}

#[kithara::asset(
    ext = "toml",
    content_type = "application/x-kithara-hls-bundle",
    context
)]
#[case::plain(false)]
fn rss_hls(context: &BuildContext<'_>, encrypted: bool) -> Vec<u8> {
    bundle(
        context,
        encrypted,
        rss_variants(),
        RssConsts::TARGET_DURATION,
    )
    .unwrap_or_else(|error| panic!("kithara-test-fixtures: RSS HLS bundle failed: {error}"))
}
