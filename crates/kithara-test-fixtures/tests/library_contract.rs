#![cfg(not(target_arch = "wasm32"))]

use std::io::Cursor;

use kithara_analysis::{AnalysisFile, AnalysisFingerprint, BeatGridModel};
use kithara_decode::{DecoderConfig, DecoderFactory};
use kithara_resampler::NoResamplerBackend;
#[cfg(all(test, target_os = "android"))]
use kithara_test_dylib as _;
use kithara_test_fixtures::assets::{self, by_name};
use kithara_test_utils::{
    bufpool::{TestPools, pools},
    kithara,
};
use num_traits::ToPrimitive;

mod consts {
    pub(super) const ANALYSIS_FINGERPRINT: &str = "rhythm-fixture:v1";
    /// The reference track sync is heard on: a steady 4/4 at 128 BPM.
    pub(super) const TUNNEL: &str = "library_mp3_zvuk_27390231";
    /// How far one beat interval's tempo may stray from the track's tempo.
    /// A deck synced to the grid plays at the ratio of the two, so this is
    /// the pitch wobble a listener would hear.
    pub(super) const TEMPO_TOLERANCE: f64 = 1e-3;
    /// How far a beat may sit from the one tempo line through all of them.
    pub(super) const LINE_TOLERANCE_SECONDS: f64 = 0.005;
}

fn analysis_of(track: &str) -> AnalysisFile {
    let name = format!("analysis_{track}");
    let asset = by_name(&name).unwrap_or_else(|| panic!("`{name}` is not registered"));
    AnalysisFile::parse(
        asset.bytes(),
        &AnalysisFingerprint::new(Some(consts::ANALYSIS_FINGERPRINT), None),
    )
    .unwrap_or_else(|error| panic!("decode `{name}`: {error}"))
}

/// A grid read against another rate than the track's puts every beat at the
/// wrong second.
#[kithara::test(native, flash(false), timeout(Short))]
fn every_library_track_is_analysed_at_the_rate_it_decodes_at() {
    let tracks = assets::MANIFEST.iter().filter(|entry| {
        entry.name.starts_with("library_") && entry.content_type.starts_with("audio/")
    });
    for entry in tracks {
        let track = by_name(entry.name).unwrap_or_else(|| panic!("`{}` is registered", entry.name));
        let hint = entry.path.rsplit('.').next();
        let config = DecoderConfig::<NoResamplerBackend, TestPools>::builder()
            .pools(pools())
            .build();
        let decoder =
            DecoderFactory::create_with_probe(Cursor::new(track.bytes().to_vec()), hint, config)
                .unwrap_or_else(|error| panic!("open `{}`: {error}", entry.name));

        assert_eq!(
            analysis_of(entry.name)
                .latest()
                .analysis()
                .source_sample_rate(),
            decoder.spec().sample_rate,
            "`{}`",
            entry.name
        );
    }
}

#[kithara::test(native, flash(false))]
fn a_steady_four_four_track_states_every_beat_and_its_bars() {
    let file = analysis_of(consts::TUNNEL);
    let analysis = file.latest().analysis();
    let heard = analysis
        .beat()
        .expect("the reference track has beats")
        .artifact()
        .beats()
        .len();
    let grid = BeatGridModel::try_from(analysis).expect("the reference track states a grid");

    assert_eq!(
        grid.as_raw().beats.len(),
        heard,
        "no beat is lost to the grid"
    );
    assert_eq!(
        grid.as_raw().meter.map(|meter| meter.beats_per_bar.get()),
        Some(4)
    );
}

/// A track played at one tempo states that tempo on every beat: each beat
/// interval runs at the track's tempo, and every beat sits on one line. A
/// grid that wanders instead makes a synced deck change speed, and pitch,
/// while the music does not.
#[kithara::test(native)]
#[case::remote_techno_160997949_1("remote_track_techno_160997949_1", None)]
#[case::app_playlist_151585912("library_mp3_zvuk_151585912", None)]
#[case::app_playlist_125475417("library_mp3_zvuk_125475417", None)]
#[case::app_playlist_138535169("library_mp3_zvuk_138535169", Some(140.0))]
#[case::app_playlist_130432502("library_mp3_zvuk_130432502", None)]
#[case::app_playlist_132017169("library_mp3_zvuk_132017169", None)]
#[case::tunnel("library_mp3_zvuk_27390231", None)]
#[case::newtechno("library_flac_newtechno", None)]
#[case::slowtechno("library_flac_slowtechno", None)]
fn a_one_tempo_track_holds_its_tempo_on_every_beat(
    #[case] track: &str,
    #[case] expected_bpm: Option<f64>,
) {
    let analysis = analysis_of(track);
    let grid = BeatGridModel::try_from(analysis.latest().analysis())
        .unwrap_or_else(|error| panic!("`{track}` states no grid: {error:?}"));
    if let Some(expected) = expected_bpm {
        assert!(
            (grid.as_raw().bpm / expected - 1.0).abs() <= consts::TEMPO_TOLERANCE,
            "`{track}` must state the author's {expected} BPM, got {}",
            grid.as_raw().bpm
        );
    }
    let beats = &grid.as_raw().beats;
    assert!(beats.len() > 2, "`{track}` states {} beats", beats.len());

    let count = f64::from(u32::try_from(beats.len()).expect("beat count fits u32"));
    let mean_ordinal = beats
        .iter()
        .map(|beat| beat.ordinal.to_f64().expect("fixture ordinal fits f64"))
        .sum::<f64>()
        / count;
    let mean_at = beats.iter().map(|beat| beat.at).sum::<f64>() / count;
    let (covariance, variance) = beats.iter().fold((0.0, 0.0), |(cov, var), beat| {
        let ordinal = beat.ordinal.to_f64().expect("fixture ordinal fits f64") - mean_ordinal;
        (cov + ordinal * (beat.at - mean_at), var + ordinal * ordinal)
    });
    let period = covariance / variance;
    let tempo = 60.0 / period;

    let strays: Vec<String> = beats
        .windows(2)
        .filter_map(|pair| {
            let local = 60.0
                * (pair[1].ordinal - pair[0].ordinal)
                    .to_f64()
                    .expect("fixture interval fits f64")
                / (pair[1].at - pair[0].at);
            ((local / tempo - 1.0).abs() > consts::TEMPO_TOLERANCE)
                .then(|| format!("{:.3}s {local:.2}", pair[0].at))
        })
        .collect();
    let off_line = beats
        .iter()
        .map(|beat| {
            (beat.at
                - mean_at
                - (beat.ordinal.to_f64().expect("fixture ordinal fits f64") - mean_ordinal)
                    * period)
                .abs()
        })
        .fold(0.0, f64::max);

    assert!(
        strays.is_empty() && off_line <= consts::LINE_TOLERANCE_SECONDS,
        "`{track}` at {tempo:.3} BPM over {} beats: {} intervals stray past {}, \
         farthest beat {off_line:.4}s off the tempo line; first strays (at, BPM): {:?}",
        beats.len(),
        strays.len(),
        consts::TEMPO_TOLERANCE,
        &strays[..strays.len().min(12)],
    );
}
