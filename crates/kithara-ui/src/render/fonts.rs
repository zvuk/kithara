use iced::{
    Font,
    font::{Family, Stretch, Style, Weight},
};

pub const INTER_REGULAR_BYTES: &[u8] = include_bytes!("../../assets/fonts/Inter-Regular.ttf");
pub const INTER_SEMIBOLD_BYTES: &[u8] = include_bytes!("../../assets/fonts/Inter-SemiBold.ttf");
pub const JETBRAINS_MONO_REGULAR_BYTES: &[u8] =
    include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf");
pub const JETBRAINS_MONO_MEDIUM_BYTES: &[u8] =
    include_bytes!("../../assets/fonts/JetBrainsMono-Medium.ttf");
pub const JETBRAINS_MONO_SEMIBOLD_BYTES: &[u8] =
    include_bytes!("../../assets/fonts/JetBrainsMono-SemiBold.ttf");
pub const SPACE_GROTESK_REGULAR_BYTES: &[u8] =
    include_bytes!("../../assets/fonts/SpaceGrotesk-Regular.ttf");
pub const SPACE_GROTESK_MEDIUM_BYTES: &[u8] =
    include_bytes!("../../assets/fonts/SpaceGrotesk-Medium.ttf");
pub const SPACE_GROTESK_SEMIBOLD_BYTES: &[u8] =
    include_bytes!("../../assets/fonts/SpaceGrotesk-SemiBold.ttf");
pub const SPACE_GROTESK_BOLD_BYTES: &[u8] =
    include_bytes!("../../assets/fonts/SpaceGrotesk-Bold.ttf");

pub const FONT_BYTES: [&[u8]; 9] = [
    INTER_REGULAR_BYTES,
    INTER_SEMIBOLD_BYTES,
    SPACE_GROTESK_REGULAR_BYTES,
    SPACE_GROTESK_MEDIUM_BYTES,
    SPACE_GROTESK_SEMIBOLD_BYTES,
    SPACE_GROTESK_BOLD_BYTES,
    JETBRAINS_MONO_REGULAR_BYTES,
    JETBRAINS_MONO_MEDIUM_BYTES,
    JETBRAINS_MONO_SEMIBOLD_BYTES,
];

struct Consts;

impl Consts {
    const INTER_FAMILY: Family = Family::Name("Inter");
    #[cfg(not(target_vendor = "apple"))]
    const MONO_FAMILY: Family = Family::Name("JetBrains Mono");
    #[cfg(target_vendor = "apple")]
    const MONO_FAMILY: Family = Family::Name("Menlo");
    const SPACE_GROTESK_FAMILY: Family = Family::Name("Space Grotesk");
}

pub const SANS: Font = font(Consts::INTER_FAMILY, Weight::Normal);
pub const MONO: Font = font(Consts::MONO_FAMILY, Weight::Normal);

#[must_use]
pub const fn display(weight: Weight) -> Font {
    font(Consts::SPACE_GROTESK_FAMILY, weight)
}

const fn font(family: Family, weight: Weight) -> Font {
    Font {
        family,
        weight,
        stretch: Stretch::Normal,
        style: Style::Normal,
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::FONT_BYTES;

    #[kithara::test]
    fn font_catalog_contains_embedded_bytes() {
        assert!(FONT_BYTES.iter().all(|bytes| !bytes.is_empty()));
    }
}
