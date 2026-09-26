/// A toolkit-neutral point in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "iced", derive(kithara_derive::Mirror))]
#[cfg_attr(feature = "iced", mirror(from = iced::Point, into = iced::Point))]
pub struct Pt {
    pub x: f32,
    pub y: f32,
}

impl Pt {
    #[must_use]
    pub fn distance(self, other: Self) -> f32 {
        (self.x - other.x).hypot(self.y - other.y)
    }
}

#[cfg(feature = "list")]
impl From<Pt> for kurbo::Point {
    fn from(point: Pt) -> Self {
        Self::new(f64::from(point.x), f64::from(point.y))
    }
}
