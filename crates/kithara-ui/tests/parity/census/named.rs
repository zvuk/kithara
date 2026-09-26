use iced::keyboard::key::{Code, Named as IcedKey};
use kithara_ui::interact::{Input, Key, Modifiers};

bitflags::bitflags! {
    /// What a control promises to answer, which the drivers below push at
    /// it on both hosts.
    ///
    /// A drag and a bare press are separate promises. A fader jumps to where
    /// it is pressed and a knob does not: pressing a knob only grips it,
    /// which neither publishes nor draws anything, so the knob promises the
    /// drag the grip starts and not the press.
    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    pub(crate) struct Gestures: u8 {
        const DOUBLE_CLICK = 1 << 0;
        const KEYBOARD = 1 << 1;
        const PRESS = 1 << 2;
        const WHEEL = 1 << 3;
        const DRAG = 1 << 4;
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Row {
    pub(crate) name: &'static str,
    pub(crate) gestures: Gestures,
}

pub(crate) const ROWS: &[Row] = &[
    Row {
        name: "Brand",
        gestures: Gestures::empty(),
    },
    Row {
        name: "Spacer",
        gestures: Gestures::empty(),
    },
    Row {
        name: "Divider",
        gestures: Gestures::empty(),
    },
    Row {
        name: "PresetSelector",
        gestures: Gestures::PRESS,
    },
    Row {
        name: "SettingsButton",
        gestures: Gestures::PRESS,
    },
    Row {
        name: "DeckSummary",
        gestures: Gestures::empty(),
    },
    Row {
        name: "WindowDrag",
        gestures: Gestures::PRESS.union(Gestures::DRAG),
    },
    Row {
        name: "TitleBar",
        gestures: Gestures::empty(),
    },
    Row {
        name: "WindowControls",
        gestures: Gestures::PRESS,
    },
    Row {
        name: "Bpm",
        gestures: Gestures::empty(),
    },
    Row {
        name: "Time",
        gestures: Gestures::empty(),
    },
    Row {
        name: "Scalar",
        gestures: Gestures::empty(),
    },
    Row {
        name: "Wave",
        gestures: Gestures::PRESS,
    },
    Row {
        name: "Vis",
        gestures: Gestures::empty(),
    },
    Row {
        name: "Sprite",
        gestures: Gestures::empty(),
    },
    Row {
        name: "Lottie",
        gestures: Gestures::empty(),
    },
    Row {
        name: "Shader",
        gestures: Gestures::empty(),
    },
    Row {
        // The document binds a custom control to nothing, so the toolkit
        // recognises nothing over it: whatever it answers, it answers for
        // itself, through the registry it was mounted from.
        name: "Custom",
        gestures: Gestures::empty(),
    },
    Row {
        name: "Table",
        gestures: Gestures::PRESS.union(Gestures::DRAG).union(Gestures::WHEEL),
    },
    Row {
        name: "Tree",
        gestures: Gestures::PRESS
            .union(Gestures::DRAG)
            .union(Gestures::KEYBOARD)
            .union(Gestures::WHEEL),
    },
    Row {
        name: "ContextBar",
        gestures: Gestures::PRESS.union(Gestures::KEYBOARD),
    },
    Row {
        name: "Text",
        gestures: Gestures::empty(),
    },
    Row {
        name: "Knob",
        gestures: Gestures::DRAG
            .union(Gestures::DOUBLE_CLICK)
            .union(Gestures::WHEEL),
    },
    Row {
        name: "Chip",
        gestures: Gestures::PRESS,
    },
    Row {
        name: "NavItem",
        gestures: Gestures::PRESS,
    },
    Row {
        name: "Button",
        gestures: Gestures::PRESS,
    },
    Row {
        name: "Glyph",
        gestures: Gestures::empty(),
    },
    Row {
        name: "TabLarge",
        gestures: Gestures::PRESS,
    },
    Row {
        name: "Toggle",
        gestures: Gestures::PRESS,
    },
    Row {
        name: "Checkbox",
        gestures: Gestures::PRESS,
    },
    Row {
        name: "Segmented",
        gestures: Gestures::PRESS,
    },
    Row {
        name: "Select",
        gestures: Gestures::empty(),
    },
    Row {
        name: "StatusDot",
        gestures: Gestures::empty(),
    },
    Row {
        name: "Swatch",
        gestures: Gestures::empty(),
    },
    Row {
        name: "Cell",
        gestures: Gestures::empty(),
    },
    Row {
        name: "Readout",
        gestures: Gestures::empty(),
    },
    Row {
        name: "Meter",
        gestures: Gestures::empty(),
    },
    Row {
        name: "VuVertical",
        gestures: Gestures::PRESS.union(Gestures::DRAG),
    },
    Row {
        name: "VuStereo",
        gestures: Gestures::PRESS.union(Gestures::DRAG),
    },
    Row {
        name: "Fader",
        gestures: Gestures::PRESS.union(Gestures::DRAG),
    },
    Row {
        name: "Crossfader",
        gestures: Gestures::PRESS.union(Gestures::DRAG),
    },
    Row {
        name: "PortalMap",
        gestures: Gestures::empty(),
    },
    Row {
        name: "Range",
        gestures: Gestures::PRESS.union(Gestures::DRAG),
    },
];

/// The single promise one driven sequence measures.
///
/// A drag and a press are separate promises: measuring them together lets
/// one cover for the other. Each is driven on its own host by the event that
/// carries it.
///
/// A drag takes two moves, not one: `ItemDrag` spends the first fixing the
/// point the travel is measured from, so a single move is below every
/// threshold by construction and would measure the sequence, not the host.
///
/// A double click is answered only by what its second press publishes
/// that a lone press does not: a control that takes every press takes both
/// halves of a double click, and that is not answering one. A keyboard
/// promise is driven after a press that gives the control focus, and is
/// kept by any key taken or anything published.
///
/// A gesture may reach nothing but what the control shows - a table
/// scrolled publishes nothing - so what a host shows it took counts beside
/// what the document published.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Named {
    Press,
    Drag,
    Wheel,
    DoubleClick,
    Keyboard,
}

impl Named {
    pub(crate) const ALL: [Self; 5] = [
        Self::Press,
        Self::Drag,
        Self::Wheel,
        Self::DoubleClick,
        Self::Keyboard,
    ];

    pub(crate) fn declared_by(self, gestures: Gestures) -> bool {
        match self {
            Self::Press => gestures.contains(Gestures::PRESS),
            Self::Drag => gestures.contains(Gestures::DRAG),
            Self::Wheel => gestures.contains(Gestures::WHEEL),
            Self::DoubleClick => gestures.contains(Gestures::DOUBLE_CLICK),
            Self::Keyboard => gestures.contains(Gestures::KEYBOARD),
        }
    }
}

/// The keys a keyboard promise is driven with: a step through what the
/// control holds, the key that commits it, and a character typed into it.
pub(crate) const KEYS: [(Stroke, Code); 3] = [
    (
        Stroke::Named(Key::ArrowDown, IcedKey::ArrowDown),
        Code::ArrowDown,
    ),
    (Stroke::Named(Key::Enter, IcedKey::Enter), Code::Enter),
    (Stroke::Typed("a"), Code::KeyA),
];

/// One key of [`KEYS`], spelled for either host.
#[derive(Clone, Copy)]
pub(crate) enum Stroke {
    Named(Key<'static>, IcedKey),
    Typed(&'static str),
}

impl Stroke {
    pub(crate) const fn retained(self) -> Input<'static> {
        let (key, text) = match self {
            Self::Named(key, _) => (key, None),
            Self::Typed(character) => (Key::Character(character), Some(character)),
        };
        Input::KeyPressed {
            key,
            modifiers: Modifiers::new(false, false, false, false),
            text,
        }
    }

    pub(crate) fn immediate(self) -> iced::keyboard::Key {
        match self {
            Self::Named(_, key) => iced::keyboard::Key::Named(key),
            Self::Typed(character) => iced::keyboard::Key::Character(character.into()),
        }
    }
}

/// What a host did with the events carrying the promise: whether the document
/// published anything, and whether the host showed it took them - the
/// immediate tree capturing the event, the retained one drawing something
/// new.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Answer {
    pub(crate) acted: bool,
    pub(crate) took: bool,
}

impl Answer {
    pub(crate) const fn or(self, other: Self) -> Self {
        Self {
            acted: self.acted || other.acted,
            took: self.took || other.took,
        }
    }

    pub(crate) const fn silent(self) -> bool {
        !self.acted && !self.took
    }
}

/// Whether the press alone keeps this promise.
///
/// `HostLayer::handle` answers `Down` and nothing else, because
/// `WindowCommand::Drag` gives the gesture to the window manager: no move
/// ever comes back for the toolkit to answer. Both hosts share that layer,
/// so this is the contract rather than a retained-host gap. It is named
/// here instead of skipped, so the census fails the day a handover starts
/// answering moves.
pub(crate) fn handed_over(name: &str, named: Named) -> bool {
    name == "WindowDrag" && matches!(named, Named::Drag)
}
