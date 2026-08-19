#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Point {
    pub(crate) x: f32,
    pub(crate) y: f32,
}

impl Point {
    pub(crate) const ORIGIN: Self = Self::new(0.0, 0.0);

    pub(crate) const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Size<T = f32> {
    pub(crate) width: T,
    pub(crate) height: T,
}

impl<T> Size<T> {
    pub(crate) const fn new(width: T, height: T) -> Self {
        Self { width, height }
    }
}

impl Size {
    pub(crate) const ZERO: Self = Self::new(0.0, 0.0);

    pub(crate) fn expand(self, other: impl Into<Self>) -> Self {
        let other = other.into();
        Self::new(self.width + other.width, self.height + other.height)
    }
}
