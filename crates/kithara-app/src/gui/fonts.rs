use iced::{
    Font,
    font::{Family, Stretch, Style, Weight},
};

pub(crate) const INTER_REGULAR_BYTES: &[u8] =
    include_bytes!("../../assets/fonts/Inter-Regular.ttf");
pub(crate) const INTER_SEMIBOLD_BYTES: &[u8] =
    include_bytes!("../../assets/fonts/Inter-SemiBold.ttf");
pub(crate) const SPACE_GROTESK_MEDIUM_BYTES: &[u8] =
    include_bytes!("../../assets/fonts/SpaceGrotesk-Medium.ttf");
pub(crate) const SPACE_GROTESK_BOLD_BYTES: &[u8] =
    include_bytes!("../../assets/fonts/SpaceGrotesk-Bold.ttf");

mod consts {
    use iced::font::Family;

    pub(super) const INTER_FAMILY: Family = Family::Name("Inter");
    pub(super) const SPACE_GROTESK_FAMILY: Family = Family::Name("Space Grotesk");
}
use consts::{INTER_FAMILY, SPACE_GROTESK_FAMILY};

pub(crate) const SANS: Font = font(INTER_FAMILY, Weight::Normal);
pub(crate) const MONO: Font = font(Family::Monospace, Weight::Normal);

pub(crate) const fn sans(weight: Weight) -> Font {
    font(INTER_FAMILY, weight)
}

pub(crate) const fn display(weight: Weight) -> Font {
    font(SPACE_GROTESK_FAMILY, weight)
}

pub(crate) const fn mono(weight: Weight) -> Font {
    font(Family::Monospace, weight)
}

const fn font(family: Family, weight: Weight) -> Font {
    Font {
        family,
        weight,
        stretch: Stretch::Normal,
        style: Style::Normal,
    }
}
