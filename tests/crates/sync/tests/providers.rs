use kithara::platform::sync::Arc;
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper, hls_server::aes128_encryption,
};
use kithara_test_fixtures::{
    asset::Asset,
    assets::{
        by_name, rhythm_fmp4_init_deck_a_120bpm_48k, rhythm_fmp4_media_deck_a_120bpm_48k,
        rhythm_mp3_deck_a_120bpm_48k, rhythm_mp3_deck_b_120bpm_48k, rhythm_wav_deck_a_120bpm_48k,
        rhythm_wav_deck_b_120bpm_48k, rhythm_wav_deck_c_120bpm_48k, rhythm_wav_deck_d_120bpm_48k,
        signal_mp3_sweep_up_60s,
    },
};

const CROSS_STYLE: &[&str] = &[
    "rhythm_wav_ambient_dub_62_aligned",
    "rhythm_wav_downtempo_96_aligned",
    "rhythm_wav_house_124_aligned",
    "rhythm_wav_breakbeat_140_aligned",
];
pub(super) const LIBRARY: &[&str] = &["library_flac_song2", "library_flac_slowtechno"];
/// Richie Hawtin - The Tunnel, a straight-kick track with a steady grid.
pub(super) const TUNNEL: &[&str] = &["library_mp3_zvuk_27390231"];
/// A straight 48 kHz techno track, the Tunnel's counterpart at the session rate.
pub(super) const NEWTECHNO: &[&str] = &["library_flac_newtechno"];
#[derive(Clone, Copy, Debug)]
pub(super) enum Provider {
    Synthetic,
    Rhythm(&'static [&'static str]),
    HlsSame(HlsProtection),
    Library(&'static [&'static str]),
    Mp3Same,
    Mp3Distinct,
    HlsMp3(HlsProtection),
    Sweep,
}

impl Provider {
    pub(super) const ALL: &[Self] = &[
        Self::Synthetic,
        Self::Rhythm(CROSS_STYLE),
        Self::HlsSame(HlsProtection::Plain),
        Self::HlsSame(HlsProtection::Drm),
        Self::Library(LIBRARY),
        Self::Library(TUNNEL),
        Self::Library(NEWTECHNO),
        Self::Mp3Same,
        Self::Mp3Distinct,
        Self::HlsMp3(HlsProtection::Plain),
        Self::HlsMp3(HlsProtection::Drm),
        Self::Sweep,
    ];
}

#[derive(Clone, Copy, Debug)]
pub(super) enum HlsProtection {
    Plain,
    Drm,
}

pub(super) async fn sources(
    provider: Provider,
    decks: usize,
    server: &TestServerHelper,
) -> Vec<String> {
    match provider {
        Provider::Synthetic => cycle_paths(
            &[
                rhythm_wav_deck_a_120bpm_48k(),
                rhythm_wav_deck_b_120bpm_48k(),
                rhythm_wav_deck_c_120bpm_48k(),
                rhythm_wav_deck_d_120bpm_48k(),
            ],
            decks,
        ),
        Provider::Rhythm(assets) => assets
            .iter()
            .cycle()
            .take(decks)
            .map(|name| {
                asset_path(
                    &by_name(name).unwrap_or_else(|| panic!("missing rhythm fixture `{name}`")),
                )
            })
            .collect(),
        Provider::Library(names) => names
            .iter()
            .cycle()
            .take(decks)
            .map(|name| {
                let asset = by_name(name).unwrap_or_else(|| {
                    panic!(
                        "BLOCKED_FIXTURE: library fixture `{name}` is not registered; build without KITHARA_DISABLE_REMOTE_FIXTURES"
                    )
                });
                asset
                    .try_bytes()
                    .unwrap_or_else(|error| panic!("BLOCKED_FIXTURE: {error}"));
                asset_path(&asset)
            })
            .collect(),
        Provider::Mp3Same => cycle_paths(&[rhythm_mp3_deck_a_120bpm_48k()], decks),
        Provider::Sweep => cycle_paths(&[signal_mp3_sweep_up_60s()], decks),
        Provider::Mp3Distinct => cycle_paths(
            &[
                rhythm_mp3_deck_a_120bpm_48k(),
                rhythm_mp3_deck_b_120bpm_48k(),
            ],
            decks,
        ),
        Provider::HlsSame(protection) => {
            let url = hls(
                server,
                rhythm_fmp4_init_deck_a_120bpm_48k(),
                rhythm_fmp4_media_deck_a_120bpm_48k(),
                protection,
            )
            .await;
            vec![url; decks]
        }
        Provider::HlsMp3(protection) => {
            let hls = hls(
                server,
                rhythm_fmp4_init_deck_a_120bpm_48k(),
                rhythm_fmp4_media_deck_a_120bpm_48k(),
                protection,
            )
            .await;
            let mp3 = asset_path(&rhythm_mp3_deck_b_120bpm_48k());
            (0..decks)
                .map(|index| {
                    if index.is_multiple_of(2) {
                        hls.clone()
                    } else {
                        mp3.clone()
                    }
                })
                .collect()
        }
    }
}

fn cycle_paths(assets: &[Asset], count: usize) -> Vec<String> {
    assets
        .iter()
        .cycle()
        .take(count)
        .map(|asset| {
            asset
                .path()
                .expect("native product fixture is materialized on disk")
                .to_str()
                .expect("fixture path is UTF-8")
                .to_owned()
        })
        .collect()
}

fn asset_path(asset: &Asset) -> String {
    asset
        .path()
        .expect("native product fixture is materialized on disk")
        .to_str()
        .expect("fixture path is UTF-8")
        .to_owned()
}

async fn hls(
    server: &TestServerHelper,
    init: Asset,
    media: Asset,
    protection: HlsProtection,
) -> String {
    let mut builder = HlsFixtureBuilder::new()
        .variant_count(1)
        .segments_per_variant(1)
        .segment_duration_secs(12.0)
        .segment_size(media.bytes().len())
        .codecs("fLaC".to_owned())
        .init_data_per_variant(vec![Arc::new(init.bytes().to_vec())])
        .custom_data(Arc::new(media.bytes().to_vec()));
    if matches!(protection, HlsProtection::Drm) {
        builder = builder.encryption(aes128_encryption());
    }
    server
        .create_hls(builder)
        .await
        .expect("register build-time rhythmic fMP4 as HLS")
        .master_url()
        .to_string()
}
