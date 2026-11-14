#![forbid(unsafe_code)]

use kithara_test_dylib as _;

mod idle_behavior;
mod red_flaky_small_cache_hot_refetch;
mod red_leak_native_drm_seek_resume;
mod startup_no_eager_size_probe_storm;
