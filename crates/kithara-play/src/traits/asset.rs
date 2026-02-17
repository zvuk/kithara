use kithara_platform::{MaybeSend, MaybeSync};

use crate::{metadata::Metadata, time::MediaTime};

#[cfg_attr(
    any(test, feature = "test-utils"),
    unimock::unimock(api = AssetMock)
)]
pub trait Asset: MaybeSend + MaybeSync + 'static {
    fn duration(&self) -> MediaTime;

    fn is_playable(&self) -> bool;

    fn preferred_rate(&self) -> f32 {
        1.0
    }

    fn preferred_volume(&self) -> f32 {
        1.0
    }

    fn metadata(&self) -> Metadata;

    fn url(&self) -> Option<url::Url>;

    fn has_protected_content(&self) -> bool {
        false
    }
}
