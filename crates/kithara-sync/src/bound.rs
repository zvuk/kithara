use kithara_signal::SessionFrame;

/// Which side of a frame an entry is looked for on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Bound {
    /// The earliest entry no earlier than the frame: a press, a resume.
    AtOrAfter(SessionFrame),
    /// The latest entry no later than the frame: an automatic transition.
    AtOrBefore(SessionFrame),
}
