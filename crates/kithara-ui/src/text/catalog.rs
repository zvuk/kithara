use crate::skin::{FontFamily, FontWeight};

/// An embedded font face owned by `kithara-ui`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum FontId {
    InterRegular,
    InterSemibold,
    JetBrainsMonoRegular,
    JetBrainsMonoMedium,
    JetBrainsMonoSemibold,
    SpaceGroteskRegular,
    SpaceGroteskMedium,
    SpaceGroteskSemibold,
    SpaceGroteskBold,
    Lucide,
}

impl FontId {
    pub(super) const ALL: [Self; 10] = [
        Self::InterRegular,
        Self::InterSemibold,
        Self::JetBrainsMonoRegular,
        Self::JetBrainsMonoMedium,
        Self::JetBrainsMonoSemibold,
        Self::SpaceGroteskRegular,
        Self::SpaceGroteskMedium,
        Self::SpaceGroteskSemibold,
        Self::SpaceGroteskBold,
        Self::Lucide,
    ];

    /// Returns the embedded bytes for this face.
    #[must_use]
    pub const fn bytes(self) -> &'static [u8] {
        match self {
            Self::InterRegular => include_bytes!("../../assets/fonts/Inter-Regular.ttf"),
            Self::InterSemibold => include_bytes!("../../assets/fonts/Inter-SemiBold.ttf"),
            Self::JetBrainsMonoRegular => {
                include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf")
            }
            Self::JetBrainsMonoMedium => {
                include_bytes!("../../assets/fonts/JetBrainsMono-Medium.ttf")
            }
            Self::JetBrainsMonoSemibold => {
                include_bytes!("../../assets/fonts/JetBrainsMono-SemiBold.ttf")
            }
            Self::Lucide => lucide_icons::LUCIDE_FONT_BYTES,
            Self::SpaceGroteskRegular => {
                include_bytes!("../../assets/fonts/SpaceGrotesk-Regular.ttf")
            }
            Self::SpaceGroteskMedium => {
                include_bytes!("../../assets/fonts/SpaceGrotesk-Medium.ttf")
            }
            Self::SpaceGroteskSemibold => {
                include_bytes!("../../assets/fonts/SpaceGrotesk-SemiBold.ttf")
            }
            Self::SpaceGroteskBold => include_bytes!("../../assets/fonts/SpaceGrotesk-Bold.ttf"),
        }
    }

    pub(crate) const fn family_name(self) -> &'static str {
        match self {
            Self::InterRegular | Self::InterSemibold => "Inter",
            Self::JetBrainsMonoRegular
            | Self::JetBrainsMonoMedium
            | Self::JetBrainsMonoSemibold => "JetBrains Mono",
            Self::Lucide => "lucide",
            Self::SpaceGroteskRegular
            | Self::SpaceGroteskMedium
            | Self::SpaceGroteskSemibold
            | Self::SpaceGroteskBold => "Space Grotesk",
        }
    }

    #[cfg(feature = "render")]
    pub(super) const fn index(self) -> usize {
        match self {
            Self::InterRegular => 0,
            Self::InterSemibold => 1,
            Self::JetBrainsMonoRegular => 2,
            Self::JetBrainsMonoMedium => 3,
            Self::JetBrainsMonoSemibold => 4,
            Self::SpaceGroteskRegular => 5,
            Self::SpaceGroteskMedium => 6,
            Self::SpaceGroteskSemibold => 7,
            Self::SpaceGroteskBold => 8,
            Self::Lucide => 9,
        }
    }
}

pub(crate) const fn select(family: FontFamily, weight: FontWeight) -> FontId {
    match (family, weight) {
        (FontFamily::Sans, FontWeight::Normal | FontWeight::Medium) => FontId::InterRegular,
        (FontFamily::Sans, FontWeight::Semibold | FontWeight::Bold) => FontId::InterSemibold,
        (FontFamily::Mono, FontWeight::Normal) => FontId::JetBrainsMonoRegular,
        (FontFamily::Mono, FontWeight::Medium) => FontId::JetBrainsMonoMedium,
        (FontFamily::Mono, FontWeight::Semibold | FontWeight::Bold) => {
            FontId::JetBrainsMonoSemibold
        }
        (FontFamily::Display, FontWeight::Normal) => FontId::SpaceGroteskRegular,
        (FontFamily::Display, FontWeight::Medium) => FontId::SpaceGroteskMedium,
        (FontFamily::Display, FontWeight::Semibold) => FontId::SpaceGroteskSemibold,
        (FontFamily::Display, FontWeight::Bold) => FontId::SpaceGroteskBold,
    }
}
