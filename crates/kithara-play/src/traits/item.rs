use std::time::Duration;

use kithara_platform::{MaybeSend, MaybeSync};

use crate::{
    error::PlayError,
    time::MediaTime,
    types::{ItemStatus, TimeRange},
};

pub trait PlayerItem: MaybeSend + MaybeSync + 'static {
    fn status(&self) -> ItemStatus;

    fn error(&self) -> Option<String>;

    fn duration(&self) -> MediaTime;

    fn current_time(&self) -> MediaTime;

    fn loaded_time_ranges(&self) -> Vec<TimeRange>;

    fn seekable_time_ranges(&self) -> Vec<TimeRange>;

    fn is_playback_likely_to_keep_up(&self) -> bool;

    fn is_playback_buffer_full(&self) -> bool;

    fn is_playback_buffer_empty(&self) -> bool;

    fn seek(&self, to: MediaTime) -> Result<(), PlayError>;

    fn seek_with_tolerance(
        &self,
        to: MediaTime,
        tolerance_before: MediaTime,
        tolerance_after: MediaTime,
    ) -> Result<(), PlayError>;

    fn cancel_pending_seeks(&self);

    fn step_by_count(&self, count: i32);

    fn can_play_fast_forward(&self) -> bool;

    fn can_play_reverse(&self) -> bool;

    fn can_step_forward(&self) -> bool;

    fn can_step_backward(&self) -> bool;

    fn preferred_forward_buffer_duration(&self) -> Duration;

    fn set_preferred_forward_buffer_duration(&self, duration: Duration);

    fn preferred_peak_bitrate(&self) -> f64;

    fn set_preferred_peak_bitrate(&self, bitrate: f64);

    fn asset_url(&self) -> Option<url::Url>;

    fn asset_duration(&self) -> MediaTime;

    fn asset_is_playable(&self) -> bool;

    fn asset_metadata(&self) -> crate::metadata::Metadata;
}
