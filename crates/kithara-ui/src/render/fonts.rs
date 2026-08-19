use iced::Font;

use crate::{
    backends::font,
    shaping::FontId,
    skin::{FontFamily, FontWeight},
};

pub const INTER_REGULAR_BYTES: &[u8] = FontId::InterRegular.bytes();
pub const INTER_SEMIBOLD_BYTES: &[u8] = FontId::InterSemibold.bytes();
pub const JETBRAINS_MONO_REGULAR_BYTES: &[u8] = FontId::JetBrainsMonoRegular.bytes();
pub const JETBRAINS_MONO_MEDIUM_BYTES: &[u8] = FontId::JetBrainsMonoMedium.bytes();
pub const JETBRAINS_MONO_SEMIBOLD_BYTES: &[u8] = FontId::JetBrainsMonoSemibold.bytes();
pub const LUCIDE_BYTES: &[u8] = FontId::Lucide.bytes();
pub const SPACE_GROTESK_REGULAR_BYTES: &[u8] = FontId::SpaceGroteskRegular.bytes();
pub const SPACE_GROTESK_MEDIUM_BYTES: &[u8] = FontId::SpaceGroteskMedium.bytes();
pub const SPACE_GROTESK_SEMIBOLD_BYTES: &[u8] = FontId::SpaceGroteskSemibold.bytes();
pub const SPACE_GROTESK_BOLD_BYTES: &[u8] = FontId::SpaceGroteskBold.bytes();

pub const FONT_BYTES: [&[u8]; 10] = [
    INTER_REGULAR_BYTES,
    INTER_SEMIBOLD_BYTES,
    SPACE_GROTESK_REGULAR_BYTES,
    SPACE_GROTESK_MEDIUM_BYTES,
    SPACE_GROTESK_SEMIBOLD_BYTES,
    SPACE_GROTESK_BOLD_BYTES,
    JETBRAINS_MONO_REGULAR_BYTES,
    JETBRAINS_MONO_MEDIUM_BYTES,
    JETBRAINS_MONO_SEMIBOLD_BYTES,
    LUCIDE_BYTES,
];

pub const SANS: Font = font(FontFamily::Sans, FontWeight::Normal);
pub const MONO: Font = font(FontFamily::Mono, FontWeight::Normal);
pub const LUCIDE: Font = Font::with_name("lucide");

#[must_use]
pub const fn mono(weight: FontWeight) -> Font {
    font(FontFamily::Mono, weight)
}

#[must_use]
pub const fn display(weight: FontWeight) -> Font {
    font(FontFamily::Display, weight)
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
