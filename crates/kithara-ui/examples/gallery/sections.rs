#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Tab {
    Atoms,
    Buttons,
    Faders,
    Modules,
    Typography,
    Cells,
    Sizes,
    Tokens,
    Micro,
    Mixer,
    Vis,
    Chrome,
    Titlebars,
    Table,
    Tree,
    Library2,
    Stress,
    Menu,
    Clock,
    Pivot,
    Shader,
    Objects,
    Motion,
    Sprites,
    Lottie,
}

impl Tab {
    pub(super) const ALL: [Self; 25] = [
        Self::Atoms,
        Self::Buttons,
        Self::Faders,
        Self::Modules,
        Self::Typography,
        Self::Cells,
        Self::Sizes,
        Self::Tokens,
        Self::Micro,
        Self::Mixer,
        Self::Vis,
        Self::Chrome,
        Self::Titlebars,
        Self::Table,
        Self::Tree,
        Self::Library2,
        Self::Stress,
        Self::Menu,
        Self::Clock,
        Self::Pivot,
        Self::Shader,
        Self::Objects,
        Self::Motion,
        Self::Sprites,
        Self::Lottie,
    ];

    pub(super) const fn entry(self) -> &'static str {
        match self {
            Self::Atoms => "gallery-atoms.klayout.ron",
            Self::Buttons => "gallery-buttons.klayout.ron",
            Self::Faders => "gallery-faders.klayout.ron",
            Self::Modules => "gallery-modules.klayout.ron",
            Self::Typography => "gallery-typography.klayout.ron",
            Self::Cells => "gallery-cells.klayout.ron",
            Self::Sizes => "gallery-sizes.klayout.ron",
            Self::Tokens => "gallery-tokens.klayout.ron",
            Self::Micro => "gallery-micro.klayout.ron",
            Self::Mixer => "gallery-mixer.klayout.ron",
            Self::Vis => "gallery-vis.klayout.ron",
            Self::Chrome => "gallery-chrome.klayout.ron",
            Self::Titlebars => "gallery-titlebars.klayout.ron",
            Self::Table => "gallery-table.klayout.ron",
            Self::Tree => "gallery-tree.klayout.ron",
            Self::Library2 => "gallery-library2.klayout.ron",
            Self::Stress => "gallery-stress.klayout.ron",
            Self::Menu => "gallery-menu.klayout.ron",
            Self::Clock => "gallery-clock.klayout.ron",
            Self::Pivot => "gallery-pivot.klayout.ron",
            Self::Shader => "gallery-shader.klayout.ron",
            Self::Objects => "gallery-objects.klayout.ron",
            Self::Motion => "gallery-motion.klayout.ron",
            Self::Sprites => "gallery-sprites.klayout.ron",
            Self::Lottie => "gallery-lottie.klayout.ron",
        }
    }

    pub(super) const fn index(self) -> usize {
        match self {
            Self::Atoms => 0,
            Self::Buttons => 1,
            Self::Faders => 2,
            Self::Modules => 3,
            Self::Typography => 4,
            Self::Cells => 5,
            Self::Sizes => 6,
            Self::Tokens => 7,
            Self::Micro => 8,
            Self::Mixer => 9,
            Self::Vis => 10,
            Self::Chrome => 11,
            Self::Titlebars => 12,
            Self::Table => 13,
            Self::Tree => 14,
            Self::Library2 => 15,
            Self::Stress => 16,
            Self::Menu => 17,
            Self::Clock => 18,
            Self::Pivot => 19,
            Self::Shader => 20,
            Self::Objects => 21,
            Self::Motion => 22,
            Self::Sprites => 23,
            Self::Lottie => 24,
        }
    }
}

impl TryFrom<&str> for Tab {
    type Error = ();

    fn try_from(path: &str) -> Result<Self, ()> {
        let slug = path
            .strip_prefix("gallery/")
            .and_then(|rest| rest.strip_suffix("/item"))
            .ok_or(())?;
        match slug {
            "atoms" => Ok(Self::Atoms),
            "buttons" => Ok(Self::Buttons),
            "faders" => Ok(Self::Faders),
            "modules" => Ok(Self::Modules),
            "typography" => Ok(Self::Typography),
            "cells" => Ok(Self::Cells),
            "sizes" => Ok(Self::Sizes),
            "tokens" => Ok(Self::Tokens),
            "micro" => Ok(Self::Micro),
            "mixer" => Ok(Self::Mixer),
            "vis" => Ok(Self::Vis),
            "chrome" => Ok(Self::Chrome),
            "titlebars" => Ok(Self::Titlebars),
            "table" => Ok(Self::Table),
            "tree" => Ok(Self::Tree),
            "library2" => Ok(Self::Library2),
            "stress" => Ok(Self::Stress),
            "menu" => Ok(Self::Menu),
            "clock" => Ok(Self::Clock),
            "pivot" => Ok(Self::Pivot),
            "shader" => Ok(Self::Shader),
            "objects" => Ok(Self::Objects),
            "motion" => Ok(Self::Motion),
            "sprites" => Ok(Self::Sprites),
            "lottie" => Ok(Self::Lottie),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ModuleDemo {
    Deck,
    DeckMicro,
    GlobalBar,
    Telemetry,
    Layout,
}

impl ModuleDemo {
    pub(super) const ALL: [Self; 5] = [
        Self::Deck,
        Self::DeckMicro,
        Self::GlobalBar,
        Self::Telemetry,
        Self::Layout,
    ];

    pub(super) const fn entry(self) -> &'static str {
        match self {
            Self::Deck => "gallery-modules.klayout.ron",
            Self::DeckMicro => "gallery-modules-deck-micro.klayout.ron",
            Self::GlobalBar => "gallery-modules-global-bar.klayout.ron",
            Self::Telemetry => "gallery-modules-telemetry.klayout.ron",
            Self::Layout => "gallery-modules-layout.klayout.ron",
        }
    }

    pub(super) const fn index(self) -> usize {
        match self {
            Self::Deck => 0,
            Self::DeckMicro => 1,
            Self::GlobalBar => 2,
            Self::Telemetry => 3,
            Self::Layout => 4,
        }
    }
}
