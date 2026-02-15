#![forbid(unsafe_code)]

//! Downloader traits for background data fetching.
//!
//! The architecture separates concerns:
//! - [`DownloaderIo`]: pure I/O executor (Clone + Send, no mutable state)
//! - [`Downloader`]: mutable planner/committer (plan + commit, no I/O)
//! - Backend (in `backend.rs`): generic orchestrator (backpressure, demand, parallelism, yield)
//!
//! On wasm32, `Send` bounds are relaxed via [`MaybeSend`] — a conditional trait
//! that equals `Send` on native and is a no-op on wasm32. This eliminates
//! the need for duplicate trait definitions.

use std::future::Future;

use kithara_storage::MaybeSend;

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
///
/// On wasm32, `Send` bounds are relaxed via [`MaybeSend`].
pub trait DownloaderIo: Clone + MaybeSend + 'static {
    /// Plan descriptor consumed by [`fetch`](Self::fetch).
    type Plan: MaybeSend;
    /// Fetch result passed to [`Downloader::commit`].
    type Fetch: MaybeSend;
    /// Error type.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Execute a single fetch (network I/O).
    fn fetch(&self, plan: Self::Plan) -> impl Future<Output = Result<Self::Fetch, Self::Error>>;
}

/// Background downloader driven by Backend's orchestration loop.
///
/// Implementations provide planning (what to download), streaming steps,
/// and commit logic. Backend handles backpressure, demand priority,
/// parallelism, yield, and cancellation generically.
///
/// On wasm32, `Send` bounds are relaxed via [`MaybeSend`].
pub trait Downloader: MaybeSend + 'static {
    /// Plan descriptor.
    type Plan: MaybeSend;
    /// Fetch result.
    type Fetch: MaybeSend;
    /// Error type.
    type Error: std::error::Error + Send + Sync + 'static;
    /// I/O executor type.
    type Io: DownloaderIo<Plan = Self::Plan, Fetch = Self::Fetch, Error = Self::Error>;

    /// Access the I/O executor (for parallel fetches).
    fn io(&self) -> &Self::Io;

    /// Check for on-demand requests (e.g. seek).
    ///
    /// Returns a plan for immediate execution, bypassing backpressure.
    fn poll_demand(&mut self) -> impl Future<Output = Option<Self::Plan>>;

    /// Plan next work batch.
    fn plan(&mut self) -> impl Future<Output = PlanOutcome<Self::Plan>>;

    /// Streaming step (when [`plan`](Self::plan) returned [`PlanOutcome::Step`]).
    ///
    /// Only needed for downloaders that return [`PlanOutcome::Step`] from [`plan`](Self::plan).
    /// Default: pends forever (never called if `plan` never returns `Step`).
    fn step(&mut self) -> impl Future<Output = Result<StepResult<Self::Fetch>, Self::Error>> {
        std::future::pending()
    }

    /// Commit a fetch result to storage/state.
    fn commit(&mut self, fetch: Self::Fetch);

    /// Whether the downloader should pause (too far ahead of reader).
    fn should_throttle(&self) -> bool;

    /// Wait until throttle condition clears.
    fn wait_ready(&self) -> impl Future<Output = ()>;

    /// Signal that on-demand data is needed (e.g. reader seek).
    ///
    /// Returns a future that resolves when demand is available.
    /// Used by Backend to interrupt a blocked [`step`](Self::step) call
    /// so that [`poll_demand`](Self::poll_demand) can be checked promptly.
    ///
    /// `use<Self>` ensures the returned future does not capture the `&self`
    /// lifetime, allowing Backend to hold it alongside `step(&mut self)`
    /// in the same `tokio::select!`.
    ///
    /// Default: never resolves (no demand signaling).
    fn demand_signal(&self) -> impl Future<Output = ()> + use<Self> {
        std::future::pending()
    }
}
