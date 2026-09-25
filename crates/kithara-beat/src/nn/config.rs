use kithara_derive::Patch;

/// Policy for turning the beat model's raw logits into events.
#[kithara_config::config(default, update)]
#[derive(Clone, Copy, Debug, PartialEq, Patch)]
#[non_exhaustive]
pub struct BeatConfig {
    /// Logit a frame must exceed to be a peak candidate; `0.0` is an even chance.
    #[config(value, update)]
    #[builder(default = 0.0)]
    pub peak_threshold: f32,
    /// Frames within which consecutive peaks collapse to their mean position.
    #[config(value, update)]
    #[builder(default = 1)]
    pub dedup_width: usize,
    /// Half-width, in model frames, of the max-pool window a frame must win.
    /// The default keeps beats at least 120 ms apart at 50 fps.
    #[config(value, update)]
    #[builder(default = 3)]
    pub peak_half_width: usize,
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use kithara_config::Config as _;
    use kithara_test_utils::kithara;

    use super::{
        BeatConfig, BeatConfigDedupWidthUpdate, BeatConfigPatch, BeatConfigPeakHalfWidthUpdate,
        BeatConfigUpdate,
    };

    #[kithara::test(native, flash(false))]
    fn retained_values_report_builder_and_patch_state() {
        let mut config = BeatConfig::builder().dedup_width(4).build();
        let patch: BeatConfigPatch = serde_yaml_ng::from_str("peak_half_width: 5\n").unwrap();
        config.apply(patch);
        let values = config.values();
        assert_eq!(values.dedup_width, 4);
        assert_eq!(values.peak_half_width, 5);
        assert_eq!(values.peak_threshold, 0.0);
    }

    #[kithara::test(native, flash(false))]
    fn a_patch_writes_only_the_field_it_names() {
        let mut config = BeatConfig::builder().dedup_width(4).build();

        let patch: BeatConfigPatch =
            serde_yaml_ng::from_str("peak_half_width: 5\n").expect("valid patch document");
        config.apply(patch);

        assert_eq!(config.peak_half_width, 5);
        assert_eq!(
            config.dedup_width, 4,
            "an unnamed field keeps its seeded value"
        );
    }

    #[kithara::test(native, flash(false))]
    fn runtime_updates_set_reset_and_preserve_fields_through_patch_apply() {
        let mut config = BeatConfig::builder()
            .dedup_width(8)
            .peak_half_width(9)
            .build();

        config.apply_update(BeatConfigUpdate {
            dedup_width: BeatConfigDedupWidthUpdate::Set { value: 4 },
            ..Default::default()
        });
        assert_eq!(config.dedup_width, 4);
        assert_eq!(config.peak_half_width, 9);

        config.apply_update(BeatConfigUpdate {
            peak_half_width: BeatConfigPeakHalfWidthUpdate::Reset,
            ..Default::default()
        });
        assert_eq!(config.dedup_width, 4);
        assert_eq!(config.peak_half_width, 3);
    }
}
