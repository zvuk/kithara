// Host builds currently only consume these helpers from the Android
// backend, which is gated to `target_os = "android"`. Later refactor
// waves will wire the same helpers into the Symphonia and Apple paths;
// until then the `dead_code` lint would flag unused functions on host.
#![cfg_attr(
    not(any(test, all(feature = "android", target_os = "android"))),
    allow(dead_code)
)]

pub(crate) mod conversion;
pub(crate) mod timeline;
