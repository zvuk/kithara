#![cfg(feature = "mock")]

use std::ops::Range;

use kithara::{
    host::HostSettingsControl,
    output::{
        OfflineRenderError, OfflineRenderReport, OfflineRenderRequest, OfflineRenderer, RenderSink,
        RenderSinkError,
    },
    platform::CancelToken,
    signal::AudioSpec,
};
use kithara_test_utils::kithara as test_attrs;

#[cfg(all(test, feature = "gui"))]
pub(crate) use crate::engine::render_fixture as render_rig;
use crate::pools::AppHost;

struct Discard;

impl RenderSink for Discard {
    fn write(&mut self, _: &[f32]) -> Result<(), RenderSinkError> {
        Ok(())
    }
}

/// Keeps every sample it is handed.
#[cfg(all(test, feature = "gui"))]
#[derive(Default, fieldwork::Fieldwork)]
#[fieldwork(get, vis = "pub(crate)")]
pub(crate) struct Capture {
    samples: Vec<f32>,
}

#[cfg(all(test, feature = "gui"))]
impl RenderSink for Capture {
    fn write(&mut self, samples: &[f32]) -> Result<(), RenderSinkError> {
        self.samples.extend_from_slice(samples);
        Ok(())
    }
}

/// Apply an exact output range to an offline application host.
///
/// # Errors
/// Returns the host's offline rendering failure.
pub fn render_host(
    host: &mut AppHost,
    frames: Range<u64>,
    cancel: &CancelToken,
) -> Result<OfflineRenderReport, OfflineRenderError> {
    render_into(host, frames, cancel, &mut Discard)
}

/// Render an exact output range of an offline application host into `sink`.
///
/// # Errors
/// Returns the host's offline rendering failure.
#[test_attrs::flash(io)]
pub fn render_into<S: RenderSink>(
    host: &mut AppHost,
    frames: Range<u64>,
    cancel: &CancelToken,
    sink: &mut S,
) -> Result<OfflineRenderReport, OfflineRenderError> {
    let request = OfflineRenderRequest::builder()
        .spec(AudioSpec::new(2, host.sample_rate()))
        .frames(frames)
        .build();
    host.render(&request, cancel, sink)
}
