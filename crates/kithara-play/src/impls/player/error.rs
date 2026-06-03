/// Internal phase-transition error.
///
/// `WrongPhase` is `pub(crate)` and never surfaced through a public method:
/// public wrappers absorb it to preserve the exact pre-split contracts
/// (silent no-ops from `Idle`/`Stopped`, `None`/`false` queries, and the
/// typed `PlayError` mappings that `kithara_queue::Queue` relies on). See
/// `crates/kithara-play/README.md`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TransitionError {
    /// The requested action is not valid from the current phase.
    WrongPhase,
}
