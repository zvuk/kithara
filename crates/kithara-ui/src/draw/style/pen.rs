/// How a stroke's free ends are shaped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
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
#[non_exhaustive]
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
#[non_exhaustive]
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
            cap: LineCap::Butt,
            join: LineJoin::Miter,
            width,
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

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{LineCap, LineJoin, Pen};

    /// A bare width is a pen, so a caller that has never heard of caps keeps
    /// passing widths.
    #[kithara::test]
    fn a_width_is_a_pen() {
        assert_eq!(Pen::from(1.5), Pen::new(1.5));
        assert_eq!(Pen::new(1.5).cap, LineCap::Butt);
        assert_eq!(Pen::new(1.5).join, LineJoin::Miter);
    }

    #[kithara::test]
    fn shaping_one_end_leaves_the_rest_of_the_pen_alone() {
        let pen = Pen::new(2.0)
            .with_cap(LineCap::Round)
            .with_join(LineJoin::Bevel);

        assert_eq!(pen.cap, LineCap::Round);
        assert_eq!(pen.join, LineJoin::Bevel);
        assert_eq!(pen.width, 2.0);
    }
}
