use crate::expand::ControlSpec;

/// Names every [`ControlSpec`] variant once, and derives both the whole set and
/// the per-value name from that one list. The generated match is exhaustive, so
/// a new variant stops compiling here rather than quietly leaving a document
/// census, a gallery page, or the application short by one control.
macro_rules! control_kinds {
    ($($variant:ident),+ $(,)?) => {
        impl ControlSpec {
            /// Every control a document can name.
            pub const KINDS: &'static [&'static str] = &[$(stringify!($variant)),+];

            /// The document name of this control.
            pub const fn kind(&self) -> &'static str {
                match self {
                    $(ControlSpec::$variant { .. } => stringify!($variant),)+
                }
            }
        }
    };
}

control_kinds!(
    Bpm,
    Brand,
    Button,
    Cell,
    Checkbox,
    Chip,
    ContextBar,
    Crossfader,
    DeckSummary,
    Divider,
    Fader,
    Glyph,
    Knob,
    Lottie,
    Meter,
    NavItem,
    PortalMap,
    PresetSelector,
    Range,
    Readout,
    Scalar,
    Segmented,
    Select,
    SettingsButton,
    Shader,
    Spacer,
    Sprite,
    StatusDot,
    Swatch,
    TabLarge,
    Table,
    Text,
    Time,
    TitleBar,
    Toggle,
    Tree,
    Vis,
    VuStereo,
    VuVertical,
    Wave,
    WindowControls,
    WindowDrag,
);

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn no_control_is_named_twice() {
        let unique: BTreeSet<&str> = ControlSpec::KINDS.iter().copied().collect();
        assert_eq!(
            unique.len(),
            ControlSpec::KINDS.len(),
            "a duplicate name would let one control stand in for another"
        );
    }

    #[kithara::test]
    fn a_value_reports_the_name_the_set_lists() {
        assert_eq!(ControlSpec::Spacer.kind(), "Spacer");
        assert!(ControlSpec::KINDS.contains(&ControlSpec::WindowDrag.kind()));
    }
}
