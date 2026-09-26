/// How a stroke's free ends are shaped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineCap {
    /// Cut flush with the endpoint.
    Butt,
    /// A half-disc past the endpoint.
    Round,
    /// A half-square past the endpoint.
    Square,
}

/// How a stroke's corners are shaped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineJoin {
    /// Flattened across the corner.
    Bevel,
    /// Carried out to a point.
    Miter,
    /// Rounded off.
    Round,
}

/// How a line is drawn: how wide it is, and what its ends and corners look like.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pen {
    pub cap: LineCap,
    pub join: LineJoin,
    pub width: f32,
}

impl Pen {
    /// A pen of `width` that cuts its ends flush and carries its corners to a
    /// point — what every toolkit draws when asked for nothing in particular.
    #[must_use]
    pub const fn new(width: f32) -> Self {
        Self {
            width,
            cap: LineCap::Butt,
            join: LineJoin::Miter,
        }
    }

    #[must_use]
    pub const fn with_cap(self, cap: LineCap) -> Self {
        Self { cap, ..self }
    }

    #[must_use]
    pub const fn with_join(self, join: LineJoin) -> Self {
        Self { join, ..self }
    }
}

impl From<f32> for Pen {
    fn from(width: f32) -> Self {
        Self::new(width)
    }
}
