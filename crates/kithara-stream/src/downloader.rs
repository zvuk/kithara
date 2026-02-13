#![forbid(unsafe_code)]

//! Downloader traits for background data fetching.
//!
//! The architecture separates concerns:
//! - [`DownloaderIo`]: pure I/O executor (Clone + Send, no mutable state)
//! - [`Downloader`]: mutable planner/committer (plan + commit, no I/O)
//! - Backend (in `backend.rs`): generic orchestrator (backpressure, demand, parallelism, yield)

use std::{convert::Infallible, future::Future};

/// Outcome of [`Downloader::plan`].
pub enum PlanOutcome<P> {
    /// Batch of plans for parallel execution via [`DownloaderIo::fetch`].
    Batch(Vec<P>),
    /// Streaming step: call [`Downloader::step`] for continuous streams.
    Step,
    /// Download complete — no more work.
    Complete,
}

/// Result of [`Downloader::step`] (streaming mode).
pub enum StepResult<F> {
    /// Produced an item ready for [`Downloader::commit`].
    Item(F),
    /// Phase changed — caller should re-call [`Downloader::plan`].
    PhaseChange,
}

/// Pure I/O executor. Clone + Send, no mutable state.
///
/// Allows Backend to run multiple fetches in parallel via `tokio::join!`.
pub trait DownloaderIo: Clone + Send + 'static {
    /// Plan descriptor consumed by [`fetch`](Self::fetch).
    type Plan: Send;
    /// Fetch result passed to [`Downloader::commit`].
    type Fetch: Send;
    /// Error type.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Execute a single fetch (network I/O).
    fn fetch(
        &self,
        plan: Self::Plan,
    ) -> impl Future<Output = Result<Self::Fetch, Self::Error>> + Send;
}

/// Background downloader driven by Backend's orchestration loop.
///
/// Implementations provide planning (what to download), streaming steps,
/// and commit logic. Backend handles backpressure, demand priority,
/// parallelism, yield, and cancellation generically.
pub trait Downloader: Send + 'static {
    /// Plan descriptor.
    type Plan: Send;
    /// Fetch result.
    type Fetch: Send;
    /// Error type.
    type Error: std::error::Error + Send + Sync + 'static;
    /// I/O executor type.
    type Io: DownloaderIo<Plan = Self::Plan, Fetch = Self::Fetch, Error = Self::Error>;

    /// Access the I/O executor (for parallel fetches).
    fn io(&self) -> &Self::Io;

    /// Check for on-demand requests (e.g. seek).
    ///
    /// Returns a plan for immediate execution, bypassing backpressure.
    fn poll_demand(&mut self) -> impl Future<Output = Option<Self::Plan>> + Send;

    /// Plan next work batch.
    fn plan(&mut self) -> impl Future<Output = PlanOutcome<Self::Plan>> + Send;

    /// Streaming step (when [`plan`](Self::plan) returned [`PlanOutcome::Step`]).
    ///
    /// Only needed for downloaders that return [`PlanOutcome::Step`] from [`plan`](Self::plan).
    /// Default: pends forever (never called if `plan` never returns `Step`).
    fn step(
        &mut self,
    ) -> impl Future<Output = Result<StepResult<Self::Fetch>, Self::Error>> + Send {
        std::future::pending()
    }

    /// Commit a fetch result to storage/state.
    fn commit(&mut self, fetch: Self::Fetch);

    /// Whether the downloader should pause (too far ahead of reader).
    fn should_throttle(&self) -> bool;

    /// Wait until throttle condition clears.
    fn wait_ready(&self) -> impl Future<Output = ()> + Send;
}

// NoDownload / NoIo — for sources that don't need background downloading.

/// No-op I/O executor.
#[derive(Clone)]
pub struct NoIo;

impl DownloaderIo for NoIo {
    type Plan = Infallible;
    type Fetch = Infallible;
    type Error = NoDownloadError;

    async fn fetch(&self, plan: Self::Plan) -> Result<Self::Fetch, Self::Error> {
        match plan {}
    }
}

/// No-op downloader for sources that manage downloads internally.
pub struct NoDownload;

/// Error type for [`NoDownload`] (never constructed).
#[derive(Debug)]
pub struct NoDownloadError;

impl std::fmt::Display for NoDownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "no download")
    }
}

impl std::error::Error for NoDownloadError {}

impl Downloader for NoDownload {
    type Plan = Infallible;
    type Fetch = Infallible;
    type Error = NoDownloadError;
    type Io = NoIo;

    fn io(&self) -> &Self::Io {
        &NoIo
    }

    async fn poll_demand(&mut self) -> Option<Self::Plan> {
        std::future::pending().await
    }

    async fn plan(&mut self) -> PlanOutcome<Self::Plan> {
        std::future::pending().await
    }

    fn commit(&mut self, fetch: Self::Fetch) {
        match fetch {}
    }

    fn should_throttle(&self) -> bool {
        false
    }

    async fn wait_ready(&self) {}
}
