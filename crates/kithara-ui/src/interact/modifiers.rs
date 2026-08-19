/// Active keyboard modifiers.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct Modifiers {
    alt: bool,
    control: bool,
    logo: bool,
    shift: bool,
}

impl Modifiers {
    #[must_use]
    pub const fn new(alt: bool, control: bool, logo: bool, shift: bool) -> Self {
        Self {
            alt,
            control,
            logo,
            shift,
        }
    }

    #[must_use]
    pub const fn alt(self) -> bool {
        self.alt
    }

    #[must_use]
    pub const fn control(self) -> bool {
        self.control
    }

    #[must_use]
    pub const fn logo(self) -> bool {
        self.logo
    }

    #[must_use]
    pub const fn shift(self) -> bool {
        self.shift
    }
}
