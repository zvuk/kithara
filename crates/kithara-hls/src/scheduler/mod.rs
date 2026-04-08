//! HLS scheduler — the planning + commit FSM that drives the download
//! worker loop.
//!
//! - [`HlsScheduler`] holds the planning state (cursor, ABR, sent-init,
//!   download variant).
//! - [`plan::plan_impl`] decides what to fetch next.
//! - [`commit::commit_segment`] writes results back into the
//!   `StreamIndex`.
//! - [`worker::run_hls_worker`] is the driver loop that polls the
//!   scheduler for work and hands it to [`crate::loading::SegmentLoader`].
//!
//! The actual HTTP download is handled by `kithara_stream::dl::Downloader`
//! and `crate::loading::*` — this module is the orchestrator, not the
//! fetcher.

pub(crate) mod helpers;
mod state;

mod cmd_builder;
mod commit;
mod cursor;
mod plan;
mod size_map;
mod trait_impl;
pub(crate) mod worker;

pub(crate) use cursor::DownloadCursor;
pub(crate) use plan::HlsPlan;
pub(crate) use state::HlsScheduler;
pub(crate) use trait_impl::HlsFetch;
