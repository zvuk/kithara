use kithara_warp::BeatsPerMinute;

/// Where a group's effective tempo comes from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TempoSource {
    /// The parent's tempo; also the state of a deck that never owned one.
    Inherited,
    /// This group's own tempo, followed by its members.
    Local(BeatsPerMinute),
}
