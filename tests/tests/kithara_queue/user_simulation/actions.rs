use std::time::Duration;

/// One step in a user-simulation scenario. Each variant maps to a
/// single observable mutation through the public `Queue` API; the
/// harness applies the action and then asserts the invariants from
/// the player robustness plan ("Understanding Lock"):
///   (1) no panic + no hang in budget;
///   (2) position after seek really advanced;
///   (3) no spurious auto-advance — track only flips on real EOF;
///   (4) codec preserved across quality switches.
///
/// Values are kept ratio-based (0..1) instead of absolute seconds so a
/// single Action sequence is reusable across short HLS fixtures, longer
/// MP3 assets, and prod tracks of unknown length.
#[derive(Clone, Debug)]
pub(crate) enum Action {
    /// Seek to `ratio * duration`. `ratio` in `0.0..0.95` —
    /// stays clear of the near-end window covered by `SeekNearEnd`.
    SeekRatio(f64),
    /// Seek into the last 5 % of the track (`0.95..0.99`).
    /// Bug #7 (crash near end) lives here.
    SeekNearEnd(f64),
    /// `select(tracks()[idx], Transition::None)`.
    SelectAt(usize),
    /// `return_to_previous(Transition::None)`.
    SelectPrev,
    /// `advance_to_next(Transition::None)`.
    SelectNext,
    /// `current_abr_handle().set_mode(Manual(idx))`.
    SetQuality(usize),
    /// `current_abr_handle().set_mode(Auto(None))`.
    QualityAuto,
    /// `pause()`.
    Pause,
    /// `play()`.
    Resume,
    /// Let the engine render for `at_least` wall-clock duration. Asserts
    /// the reported position advances during the window — Bug #6 (silent
    /// hang after backward-seek) lives here.
    PlayFor(Duration),
}

impl Action {
    /// Short label used in panic messages and decision logs so a failing
    /// scenario is greppable without paging through the Action enum.
    pub(crate) fn label(&self) -> String {
        match self {
            Self::SeekRatio(r) => format!("SeekRatio({r:.3})"),
            Self::SeekNearEnd(r) => format!("SeekNearEnd({r:.3})"),
            Self::SelectAt(i) => format!("SelectAt({i})"),
            Self::SelectPrev => "SelectPrev".into(),
            Self::SelectNext => "SelectNext".into(),
            Self::SetQuality(i) => format!("SetQuality({i})"),
            Self::QualityAuto => "QualityAuto".into(),
            Self::Pause => "Pause".into(),
            Self::Resume => "Resume".into(),
            Self::PlayFor(d) => format!("PlayFor({}ms)", d.as_millis()),
        }
    }
}
