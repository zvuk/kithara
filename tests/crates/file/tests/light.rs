#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate - unwraps are acceptable in test code"
)]

#[path = "early_stream_close.rs"]
mod early_stream_close;
#[path = "file_source.rs"]
mod file_source;
#[path = "html_error_cleanup.rs"]
mod html_error_cleanup;
#[path = "resume_stall_budget.rs"]
mod resume_stall_budget;
#[path = "seek_issues_range_request.rs"]
mod seek_issues_range_request;
#[path = "shared_download.rs"]
mod shared_download;
#[path = "waveform_shared_download.rs"]
mod waveform_shared_download;
