use super::Hit;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "iced", derive(kithara_derive::Mirror))]
#[cfg_attr(feature = "iced", mirror(into = iced::mouse::Interaction))]
pub enum CursorShape {
    None,
    Grab,
    Grabbing,
    Pointer,
    #[cfg_attr(feature = "iced", mirror(rename = ResizingDiagonallyDown))]
    ResizeDiagonalDown,
    #[cfg_attr(feature = "iced", mirror(rename = ResizingDiagonallyUp))]
    ResizeDiagonalUp,
    #[cfg_attr(feature = "iced", mirror(rename = ResizingHorizontally))]
    ResizeH,
    #[cfg_attr(feature = "iced", mirror(rename = ResizingVertically))]
    ResizeV,
    Text,
}

#[derive(Clone, Copy)]
pub struct Hover {
    shape: CursorShape,
}

impl Hover {
    #[must_use]
    pub const fn new(shape: CursorShape) -> Self {
        Self { shape }
    }

    #[must_use]
    pub fn cursor(self, active: bool, hit: &Hit) -> CursorShape {
        if active || hit.over() {
            self.shape
        } else {
            CursorShape::None
        }
    }
}
