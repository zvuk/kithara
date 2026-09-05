use kithara::{
    abr::AbrSettings,
    events::{VariantDuration, VariantIndex, VariantInfo},
    platform::time::Duration,
};

pub(super) fn fast_settings() -> AbrSettings {
    AbrSettings::builder()
        .initial_throughput_bps(Some(2_000_000))
        .min_switch_interval(Duration::ZERO)
        .min_buffer_for_up_switch(Duration::ZERO)
        .build()
}

pub(super) fn variants(bitrates: &[u64]) -> Vec<VariantInfo> {
    bitrates
        .iter()
        .enumerate()
        .map(|(index, bitrate)| VariantInfo {
            variant_index: VariantIndex::new(index),
            bandwidth_bps: Some(*bitrate),
            duration: VariantDuration::Unknown,
            name: None,
            codecs: None,
            container: None,
        })
        .collect()
}
