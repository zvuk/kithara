use super::Size;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Length {
    Fill,
    FillPortion(u16),
    Shrink,
    Fixed(f32),
}

impl Length {
    pub(crate) const fn fill_factor(self) -> u16 {
        match self {
            Self::Fill => 1,
            Self::FillPortion(factor) => factor,
            Self::Shrink | Self::Fixed(_) => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Limits {
    min: Size,
    max: Size,
    compression: Size<bool>,
}

impl Limits {
    pub(crate) const fn new(min: Size, max: Size) -> Self {
        Self::with_compression(min, max, Size::new(false, false))
    }

    pub(crate) const fn with_compression(min: Size, max: Size, compression: Size<bool>) -> Self {
        Self {
            min,
            max,
            compression,
        }
    }

    pub(crate) const fn min(self) -> Size {
        self.min
    }

    pub(crate) const fn max(self) -> Size {
        self.max
    }

    pub(crate) const fn compression(self) -> Size<bool> {
        self.compression
    }

    pub(crate) fn width(mut self, width: Length) -> Self {
        match width {
            Length::Shrink => self.compression.width = true,
            Length::Fixed(amount) => {
                let width = amount.min(self.max.width).max(self.min.width);
                self.min.width = width;
                self.max.width = width;
                self.compression.width = false;
            }
            Length::Fill | Length::FillPortion(_) => {}
        }
        self
    }

    pub(crate) fn height(mut self, height: Length) -> Self {
        match height {
            Length::Shrink => self.compression.height = true,
            Length::Fixed(amount) => {
                let height = amount.min(self.max.height).max(self.min.height);
                self.min.height = height;
                self.max.height = height;
                self.compression.height = false;
            }
            Length::Fill | Length::FillPortion(_) => {}
        }
        self
    }

    pub(crate) fn shrink(self, size: impl Into<Size>) -> Self {
        let size = size.into();
        Self {
            min: Size::new(
                (self.min.width - size.width).max(0.0),
                (self.min.height - size.height).max(0.0),
            ),
            max: Size::new(
                (self.max.width - size.width).max(0.0),
                (self.max.height - size.height).max(0.0),
            ),
            compression: self.compression,
        }
    }

    pub(crate) const fn loose(self) -> Self {
        Self {
            min: Size::ZERO,
            max: self.max,
            compression: self.compression,
        }
    }

    pub(crate) fn resolve(self, width: Length, height: Length, intrinsic: Size) -> Size {
        let width = match width {
            Length::Fill | Length::FillPortion(_) if !self.compression.width => self.max.width,
            Length::Fixed(amount) => amount.min(self.max.width).max(self.min.width),
            _ => intrinsic.width.min(self.max.width).max(self.min.width),
        };
        let height = match height {
            Length::Fill | Length::FillPortion(_) if !self.compression.height => self.max.height,
            Length::Fixed(amount) => amount.min(self.max.height).max(self.min.height),
            _ => intrinsic.height.min(self.max.height).max(self.min.height),
        };
        Size::new(width, height)
    }
}
