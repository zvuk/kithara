use kithara_test_utils::kithara;

use super::{TrackConfig, TrackConfigPatch};

#[kithara::test]
fn track_config_keeps_the_native_ring_and_preload_defaults() {
    let config = TrackConfig::default();

    assert_eq!(config.audio_buffer_chunks().get(), 10);
    assert_eq!(config.preload_chunks().get(), 3);
}

#[kithara::test]
fn the_document_names_both_live_keys() {
    let config = TrackConfig::builder()
        .preload_chunks(std::num::NonZeroUsize::new(8).expect("preload"))
        .audio_buffer_chunks(std::num::NonZeroUsize::new(20).expect("capacity"))
        .build();
    assert_eq!(
        config.preload_chunks(),
        std::num::NonZeroUsize::new(8).expect("preload")
    );
    assert_eq!(config.audio_buffer_chunks().get(), 20);
}

#[kithara::test]
fn an_absent_key_stays_unset_rather_than_defaulting() {
    let mut config = TrackConfig::builder()
        .audio_buffer_chunks(crate::consts::CAPACITY)
        .build();
    let patch: TrackConfigPatch =
        serde_yaml_ng::from_str("preload_chunks: 8\n").expect("the document types");
    assert!(patch.audio_buffer_chunks.is_none());
    config.apply(patch);
    assert_eq!(
        config.preload_chunks(),
        std::num::NonZeroUsize::new(8).expect("preload")
    );
    assert_eq!(
        config.audio_buffer_chunks(),
        crate::consts::CAPACITY,
        "an unnamed value leaves the owner's capacity unchanged"
    );
}
