use std::{collections::BTreeMap, sync::OnceLock};

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

    fn beat_grids(self) -> Vec<Arc<BeatGridModel>> {
        let materialize = |deck| {
            let ArtifactSource::Value(grid) = self.beat_grid(deck) else {
                panic!("fixture grid must be materialized");
            };
            grid
        };
        match self {
            Self::Library(names) | Self::Rhythm(names) => {
                let mut grids = BTreeMap::new();
                names
                    .iter()
                    .enumerate()
                    .map(|(deck, name)| {
                        grids
                            .entry(*name)
                            .or_insert_with(|| materialize(deck))
                            .clone()
                    })
                    .collect()
            }
            _ => vec![materialize(0)],
        }
    }

    pub(crate) fn start_seconds(self, start: Start, grid: &BeatGridModel) -> f64 {
        start.seconds(matches!(self, Self::Library(_)).then_some(grid))
    }
}

pub(crate) struct PreparedSources {
    provider: Provider,
    pub(crate) server: TestServerHelper,
    pub(crate) paths: Vec<String>,
    grids: OnceLock<Vec<Arc<BeatGridModel>>>,
}

impl PreparedSources {
    delegate::delegate! {
        to self {
            #[field(provider)]
            pub(crate) const fn provider(&self) -> Provider;
        }
    }

    pub(crate) fn beat_grid(&self, deck: usize) -> ArtifactSource<BeatGridModel> {
        let grids = self.grids();
        ArtifactSource::Value(grids[deck % grids.len()].clone())
    }

    pub(crate) fn start_seconds(&self, deck: usize, start: Start) -> f64 {
        let grids = self.grids();
        self.provider
            .start_seconds(start, &grids[deck % grids.len()])
    }

    pub(super) fn grids(&self) -> &[Arc<BeatGridModel>] {
        self.grids.get_or_init(|| self.provider.beat_grids())
    }
}

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
    PreparedSources {
        provider,
        server,
        paths,
        grids: OnceLock::new(),
    }
}

#[kithara::test(native)]
fn prepared_grids_preserve_every_model_field_and_the_provider_cycle() {
    let provider = Provider::Rhythm(&[
        "rhythm_wav_ambient_dub_62_aligned",
        "rhythm_wav_house_124_aligned",
        "rhythm_wav_ambient_dub_62_aligned",
    ]);
    let prepared = provider.beat_grids();
    for deck in 0..9 {
        let ArtifactSource::Value(independent) = provider.beat_grid(deck) else {
            panic!("fixture grid must be materialized");
        };
        assert_eq!(
            serde_json::to_vec(prepared[deck % prepared.len()].as_raw())
                .expect("prepared grid serialization"),
            serde_json::to_vec(independent.as_raw()).expect("independent grid serialization"),
        );
    }
}

#[kithara::test(native)]
fn changing_a_delivered_grid_does_not_change_the_prepared_reference() {
    let prepared = Provider::Synthetic.beat_grids();
    let expected = serde_json::to_vec(prepared[0].as_raw()).expect("prepared grid serialization");
    let mut delivered = prepared[0].clone();
    let mut wrong = delivered.as_raw().clone();
    wrong.revision += 1;
    for beat in &mut wrong.beats {
        beat.at += 0.01;
    }
    for beat in &mut wrong.downbeats {
        beat.at += 0.01;
    }
    *Arc::make_mut(&mut delivered) = BeatGridModel::try_from(wrong).expect("valid wrong grid");
    assert_ne!(
        serde_json::to_vec(delivered.as_raw()).expect("changed grid serialization"),
        expected,
    );
    assert_eq!(
        serde_json::to_vec(prepared[0].as_raw()).expect("unchanged grid serialization"),
        expected,
    );
}
