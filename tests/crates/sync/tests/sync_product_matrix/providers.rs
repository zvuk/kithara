use super::{cases::*, imports::*};
use crate::providers::sources;
pub(crate) use crate::providers::{HlsProtection, Provider};

pub(crate) const AMBIENT_TRIP_HOP_PROVIDER: Provider = Provider::Rhythm(AMBIENT_TRIP_HOP);
pub(crate) const DOWNTEMPO_HOUSE_PROVIDER: Provider = Provider::Rhythm(DOWNTEMPO_HOUSE);
pub(crate) const TECHNO_BREAKBEAT_PROVIDER: Provider = Provider::Rhythm(TECHNO_BREAKBEAT);
pub(crate) const CROSS_STYLE_PROVIDER: Provider = Provider::Rhythm(CROSS_STYLE);

impl Provider {
    #[cfg(not(target_os = "android"))]
    pub(super) fn control_reference(
        self,
        deck: usize,
    ) -> Option<kithara_test_fixtures::asset::Asset> {
        use kithara_test_fixtures::assets::{
            by_name, rhythm_wav_deck_a_120bpm_48k, rhythm_wav_deck_b_120bpm_48k,
            rhythm_wav_deck_c_120bpm_48k, rhythm_wav_deck_d_120bpm_48k,
        };
        match self {
            Self::Synthetic => Some([
                rhythm_wav_deck_a_120bpm_48k,
                rhythm_wav_deck_b_120bpm_48k,
                rhythm_wav_deck_c_120bpm_48k,
                rhythm_wav_deck_d_120bpm_48k,
            ][deck % 4]()),
            Self::Rhythm(names) => {
                Some(by_name(names[deck % names.len()]).expect("declared score reference"))
            }
            Self::HlsSame(_) => Some(rhythm_wav_deck_a_120bpm_48k()),
            Self::Library(names) => match names[deck % names.len()] {
                "library_flac_song2" | "library_flac_slowtechno" => Some(
                    by_name(names[deck % names.len()])
                        .expect("declared lossless library reference"),
                ),
                _ => None,
            },
            _ => None,
        }
    }

    pub(crate) fn wrong_control_reference(
        self,
        deck: usize,
    ) -> kithara_test_fixtures::asset::Asset {
        use kithara_test_fixtures::assets::{
            rhythm_wav_deck_a_120bpm_48k, rhythm_wav_deck_d_120bpm_48k,
        };
        if matches!(self, Self::Synthetic) && deck % 4 == 3 {
            rhythm_wav_deck_a_120bpm_48k()
        } else {
            rhythm_wav_deck_d_120bpm_48k()
        }
    }

    pub(crate) const fn has_score_markers(self) -> bool {
        matches!(self, Self::Rhythm(_))
    }

    /// The beat grid deck `deck`'s track was rendered or analysed on.
    pub(crate) fn beat_grid(self, deck: usize) -> ArtifactSource<BeatGridModel> {
        match self {
            Self::Library(names) => {
                ArtifactSource::Value(Arc::new(analysed_grid(names[deck % names.len()])))
            }
            Self::Rhythm(names) => {
                let name =
                    names[deck % names.len()].replace("rhythm_wav_", "rhythm_expected_analysis_");
                let asset = kithara_test_fixtures::assets::by_name(&name)
                    .unwrap_or_else(|| panic!("missing score grid {name}"));
                let fingerprint =
                    kithara::analysis::AnalysisFingerprint::new(Some("rhythm-fixture:v1"), None);
                let file = kithara::analysis::AnalysisFile::parse(asset.bytes(), &fingerprint)
                    .expect("fixture score analysis");
                let grid =
                    BeatGridModel::try_from(file.latest().analysis()).expect("fixture score grid");
                ArtifactSource::Value(Arc::new(grid))
            }
            _ => synthetic_grid(),
        }
    }

    /// The second deck `deck`'s track opens at for `start`.
    pub(crate) fn start_seconds(self, deck: usize, start: Start) -> f64 {
        let grid = match self {
            Self::Library(names) => Some(analysed_grid(names[deck % names.len()])),
            _ => None,
        };
        start.seconds(grid.as_ref())
    }
}

pub(crate) type PreparedSources = (Provider, TestServerHelper, Vec<String>);

#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
pub(crate) async fn prepared_sources(provider: Provider) -> PreparedSources {
    let _io = kithara::platform::flash::real_io();
    hang_tick!(format!(
        "prepared_sources: starting server for {provider:?}"
    ));
    let server = time::timeout(__hang_detector.remaining(), TestServerHelper::new()).await;
    hang_tick!(format!(
        "prepared_sources: starting server for {provider:?}"
    ));
    let server = server.expect("fixture server starts within its liveness budget");
    hang_reset!();
    hang_tick!(format!("prepared_sources: materialising {provider:?}"));
    let paths = time::timeout(__hang_detector.remaining(), sources(provider, 4, &server)).await;
    hang_tick!(format!("prepared_sources: materialising {provider:?}"));
    let paths = paths.expect("fixture sources arrive within their liveness budget");
    hang_reset!();
    (provider, server, paths)
}
