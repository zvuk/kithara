use num_traits::ToPrimitive;

use super::Pt;

/// A toolkit-neutral rectangle in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "iced", derive(kithara_derive::Mirror))]
#[cfg_attr(feature = "iced", mirror(from = iced::Rectangle))]
pub struct Rect {
    #[cfg_attr(feature = "iced", mirror(rename = height))]
    pub h: f32,
    #[cfg_attr(feature = "iced", mirror(rename = width))]
    pub w: f32,
    pub x: f32,
    pub y: f32,
}

impl Rect {
    #[must_use]
    pub fn contains(self, point: Pt) -> bool {
        self.x <= point.x
            && point.x < self.x + self.w
            && self.y <= point.y
            && point.y < self.y + self.h
    }

    #[must_use]
    pub fn uniform_horizontal_index(self, point: Pt, count: usize) -> Option<usize> {
        let last = count.checked_sub(1)?;
        let count = count.to_f32()?;
        let cell_width = (self.contains(point) && self.w > 0.0).then_some(self.w / count)?;
        ((point.x - self.x) / cell_width)
            .floor()
            .to_usize()
            .map(|index| index.min(last))
    }
}
