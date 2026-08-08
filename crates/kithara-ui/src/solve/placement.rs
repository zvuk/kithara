use super::Size;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Alignment {
    Start,
    Center,
    End,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Padding {
    pub(crate) top: f32,
    pub(crate) right: f32,
    pub(crate) bottom: f32,
    pub(crate) left: f32,
}

impl From<Padding> for Size {
    fn from(padding: Padding) -> Self {
        Self::new(padding.left + padding.right, padding.top + padding.bottom)
    }
}
