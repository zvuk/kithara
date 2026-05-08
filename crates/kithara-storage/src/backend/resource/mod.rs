#![forbid(unsafe_code)]

//! Generic [`Resource<D>`] state machine.
//!
//! - `state` — struct, `Inner`/`CommonState`, ctors, `check_health`.
//! - `io` — `read_at_inner` + `write_at_inner` bodies.
//! - `wait` — `wait_range_inner` body.
//! - `lifecycle` — commit / fail / reactivate / inspect bodies.
//! - `ops` — thin [`ResourceExt`](crate::ResourceExt) delegate impl.

pub(crate) mod io;
pub(crate) mod lifecycle;
pub(crate) mod ops;
pub(crate) mod state;
pub(crate) mod wait;

pub use state::Resource;
