use iced::{
    Font,
    font::{Family, Stretch, Style, Weight},
};

pub(crate) const INTER_BYTES: &[u8] = include_bytes!("../../assets/fonts/InterVariable.ttf");
pub(crate) const JETBRAINS_MONO_REGULAR_BYTES: &[u8] =
    include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf");
pub(crate) const JETBRAINS_MONO_MEDIUM_BYTES: &[u8] =
    include_bytes!("../../assets/fonts/JetBrainsMono-Medium.ttf");
pub(crate) const JETBRAINS_MONO_SEMIBOLD_BYTES: &[u8] =
    include_bytes!("../../assets/fonts/JetBrainsMono-SemiBold.ttf");
pub(crate) const SPACE_GROTESK_BYTES: &[u8] =
    include_bytes!("../../assets/fonts/SpaceGrotesk-Variable.ttf");

const INTER_FAMILY: Family = Family::Name("Inter");
const JETBRAINS_MONO_FAMILY: Family = Family::Name("JetBrains Mono");
const SPACE_GROTESK_FAMILY: Family = Family::Name("Space Grotesk");

pub(crate) const SANS: Font = font(INTER_FAMILY, Weight::Normal);
pub(crate) const MONO: Font = font(JETBRAINS_MONO_FAMILY, Weight::Normal);

pub(crate) const fn sans(weight: Weight) -> Font {
    font(INTER_FAMILY, weight)
}

pub(crate) const fn display(weight: Weight) -> Font {
    font(SPACE_GROTESK_FAMILY, weight)
}

pub(crate) const fn mono(weight: Weight) -> Font {
    font(JETBRAINS_MONO_FAMILY, weight)
}

const fn font(family: Family, weight: Weight) -> Font {
    Font {
        family,
        weight,
        stretch: Stretch::Normal,
        style: Style::Normal,
    }
}
