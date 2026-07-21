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
    Chrome,
    Titlebars,
    Tracklist,
    Tree,
    Library2,
    Stress,
}

impl Tab {
    pub(super) const ALL: [Self; 16] = [
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
        Self::Chrome,
        Self::Titlebars,
        Self::Tracklist,
        Self::Tree,
        Self::Library2,
        Self::Stress,
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
            Self::Chrome => "gallery-chrome.klayout.ron",
            Self::Titlebars => "gallery-titlebars.klayout.ron",
            Self::Tracklist => "gallery-tracklist.klayout.ron",
            Self::Tree => "gallery-tree.klayout.ron",
            Self::Library2 => "gallery-library2.klayout.ron",
            Self::Stress => "gallery-stress.klayout.ron",
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
            Self::Chrome => 10,
            Self::Titlebars => 11,
            Self::Tracklist => 12,
            Self::Tree => 13,
            Self::Library2 => 14,
            Self::Stress => 15,
        }
    }
}

impl TryFrom<&str> for Tab {
    type Error = ();

    fn try_from(path: &str) -> Result<Self, ()> {
        match path {
            "gallery/atoms" => Ok(Self::Atoms),
            "gallery/buttons" => Ok(Self::Buttons),
            "gallery/faders" => Ok(Self::Faders),
            "gallery/modules" => Ok(Self::Modules),
            "gallery/typography" => Ok(Self::Typography),
            "gallery/cells" => Ok(Self::Cells),
            "gallery/sizes" => Ok(Self::Sizes),
            "gallery/tokens" => Ok(Self::Tokens),
            "gallery/micro" => Ok(Self::Micro),
            "gallery/mixer" => Ok(Self::Mixer),
            "gallery/chrome" => Ok(Self::Chrome),
            "gallery/titlebars" => Ok(Self::Titlebars),
            "gallery/tracklist" => Ok(Self::Tracklist),
            "gallery/tree" => Ok(Self::Tree),
            "gallery/library2" => Ok(Self::Library2),
            "gallery/stress" => Ok(Self::Stress),
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
