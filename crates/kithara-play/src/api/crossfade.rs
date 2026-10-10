#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[non_exhaustive]
pub enum SelectionPlayback {
    #[default]
    Play,
    Pause,
}
