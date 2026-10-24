#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate - unwraps are acceptable in test code"
)]

use kithara_test_dylib as _;

mod early_stream_close;
mod file_source;
mod html_error_cleanup;
mod injected_downloader;
mod resume_stall_budget;
mod seek_issues_range_request;
mod shared_download;
