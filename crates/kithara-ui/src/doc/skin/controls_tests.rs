use kithara_test_utils::kithara;

use super::{FontFamily, FontWeight, controls::*, palette::ColorRole, primitives::TextRoleSkin};
use crate::builtin;

const fn mono(size: f32, spacing: f32, color: ColorRole) -> TextRoleSkin {
    TextRoleSkin {
        size,
        spacing,
        color,
        font: FontFamily::Mono,
        weight: FontWeight::Normal,
    }
}

#[kithara::test]
fn menu_holds_exactly_the_declared_glyph_sizes() {
    assert_eq!(
        builtin::skin_doc().menu,
        MenuSkin {
            icon_color: ColorRole::Text,
            icon_size: 11.0,
            burger_icon_size: 14.0,
            small_icon_size: 10.0,
            cell_icon_size: 9.0,
        }
    );
}

#[kithara::test]
fn the_mono_pair_transcribes_the_design_defaults() {
    let text = builtin::skin_doc().text;

    assert_eq!(text.mono, mono(10.0, 0.0, ColorRole::TextDim));
    assert_eq!(text.caption, mono(7.0, 0.08, ColorRole::Muted));
}
