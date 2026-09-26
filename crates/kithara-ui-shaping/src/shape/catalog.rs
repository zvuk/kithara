use crate::{FontFamily, FontWeight};

/// A font face embedded in this crate.
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
    pub(crate) const ALL: [Self; 10] = [
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

    /// Returns the family name this face registers under.
    #[must_use]
    pub const fn family_name(self) -> &'static str {
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

    pub(crate) const fn index(self) -> usize {
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

    /// Returns the embedded face that answers `family` at `weight`.
    #[must_use]
    pub const fn select(family: FontFamily, weight: FontWeight) -> Self {
        match (family, weight) {
            (FontFamily::Sans, FontWeight::Normal | FontWeight::Medium) => Self::InterRegular,
            (FontFamily::Sans, FontWeight::Semibold | FontWeight::Bold) => Self::InterSemibold,
            (FontFamily::Mono, FontWeight::Normal) => Self::JetBrainsMonoRegular,
            (FontFamily::Mono, FontWeight::Medium) => Self::JetBrainsMonoMedium,
            (FontFamily::Mono, FontWeight::Semibold | FontWeight::Bold) => {
                Self::JetBrainsMonoSemibold
            }
            (FontFamily::Display, FontWeight::Normal) => Self::SpaceGroteskRegular,
            (FontFamily::Display, FontWeight::Medium) => Self::SpaceGroteskMedium,
            (FontFamily::Display, FontWeight::Semibold) => Self::SpaceGroteskSemibold,
            (FontFamily::Display, FontWeight::Bold) => Self::SpaceGroteskBold,
        }
    }
}
