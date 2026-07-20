use iced::{
    Font,
    font::{Family, Stretch, Style, Weight},
};

use crate::skin::{FontFamily, FontWeight};

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

pub const SANS: Font = font(inter_family(), Weight::Normal);
pub const MONO: Font = font(mono_family(), Weight::Normal);

#[must_use]
pub const fn sans(weight: FontWeight) -> Font {
    font(inter_family(), iced_weight(weight))
}

#[must_use]
pub const fn mono(weight: FontWeight) -> Font {
    font(mono_family(), iced_weight(weight))
}

#[must_use]
pub const fn display(weight: FontWeight) -> Font {
    font(display_family(), iced_weight(weight))
}

#[must_use]
pub const fn family(family: FontFamily, weight: FontWeight) -> Font {
    match family {
        FontFamily::Display => display(weight),
        FontFamily::Sans => sans(weight),
        FontFamily::Mono => mono(weight),
    }
}

const fn inter_family() -> Family {
    Family::Name("Inter")
}

#[cfg(not(target_vendor = "apple"))]
const fn mono_family() -> Family {
    Family::Name("JetBrains Mono")
}

#[cfg(target_vendor = "apple")]
const fn mono_family() -> Family {
    Family::Name("Menlo")
}

const fn display_family() -> Family {
    Family::Name("Space Grotesk")
}

const fn font(family: Family, weight: Weight) -> Font {
    Font {
        family,
        weight,
        stretch: Stretch::Normal,
        style: Style::Normal,
    }
}

const fn iced_weight(weight: FontWeight) -> Weight {
    match weight {
        FontWeight::Normal => Weight::Normal,
        FontWeight::Medium => Weight::Medium,
        FontWeight::Semibold => Weight::Semibold,
        FontWeight::Bold => Weight::Bold,
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
