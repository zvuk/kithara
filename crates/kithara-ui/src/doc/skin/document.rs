use serde::{Deserialize, Serialize};

use super::{
    controls::{
        ButtonSkin, CellSkin, CheckboxSkin, ChipSkin, CrossfaderSkin, FaderSkin, KnobSkin,
        MenuSkin, NavSkin, PortalMapSkin, RangeSkin, ReadoutSkin, SegmentedSkin, SelectSkin,
        StatusDotSkin, SwatchSkin, TabLargeSkin, TextInputSkin, TextSkin, ToggleSkin, VisSkin,
        VuStereoSkin, VuVerticalSkin,
    },
    panels::{
        DeckSkin, DividerSkin, DragSkin, GlobalBarSkin, LayoutPreviewSkin, MeterSkin, PopSkin,
        TelemetrySkin, TrackListSkin, TreeSkin, WaveSkin,
    },
    primitives::{ChromeSkin, LayoutSkin, WindowSkin},
};
use crate::{
    doc::ron_io,
    envelope::{self, DocKind},
    error::UiDocError,
    ids::{DocId, SourceUri},
};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct SkinDoc {
    pub button: ButtonSkin,
    pub cell: CellSkin,
    pub checkbox: CheckboxSkin,
    pub chip: ChipSkin,
    pub chrome: ChromeSkin,
    pub crossfader: CrossfaderSkin,
    pub deck: DeckSkin,
    pub divider: DividerSkin,
    pub id: DocId,
    pub drag: DragSkin,
    pub fader: FaderSkin,
    pub global_bar: GlobalBarSkin,
    pub knob: KnobSkin,
    pub layout_preview: LayoutPreviewSkin,
    pub layout: LayoutSkin,
    pub menu: MenuSkin,
    pub meter: MeterSkin,
    pub nav: NavSkin,
    pub palette: PaletteDoc,
    pub pop: PopSkin,
    pub portal_map: PortalMapSkin,
    pub range: RangeSkin,
    pub readout: ReadoutSkin,
    pub segmented: SegmentedSkin,
    pub select: SelectSkin,
    pub status_dot: StatusDotSkin,
    pub schema: String,
    pub swatch: SwatchSkin,
    pub tab_large: TabLargeSkin,
    pub telemetry: TelemetrySkin,
    pub text_input: TextInputSkin,
    pub text: TextSkin,
    pub toggle: ToggleSkin,
    pub track_list: TrackListSkin,
    pub tree: TreeSkin,
    pub vis: VisSkin,
    pub vu_stereo: VuStereoSkin,
    pub vu_vertical: VuVerticalSkin,
    pub wave: WaveSkin,
    pub window: WindowSkin,
    pub version: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct PaletteDoc {
    pub accent: String,
    pub accent_soft: String,
    pub accent_strong: String,
    pub bg: String,
    pub bg_deep: String,
    pub bg_footer: String,
    pub bg_inset: String,
    pub bg_panel: String,
    pub bg_panel_2: String,
    pub bg_select: String,
    pub danger: String,
    pub line: String,
    pub line_dim: String,
    pub line_hi: String,
    pub line_inner: String,
    pub line_pop: String,
    pub line_soft: String,
    pub muted: String,
    pub shadow: String,
    pub success: String,
    pub text: String,
    pub text_dim: String,
    pub warning: String,
    pub wave_high: String,
    pub wave_low: String,
    pub wave_mid: String,
}

impl PaletteDoc {
    fn validate(&self, origin: &SourceUri) -> Result<(), UiDocError> {
        for value in [
            &self.bg,
            &self.bg_deep,
            &self.bg_inset,
            &self.bg_panel,
            &self.bg_footer,
            &self.bg_panel_2,
            &self.bg_select,
            &self.line,
            &self.line_dim,
            &self.line_inner,
            &self.line_soft,
            &self.line_hi,
            &self.line_pop,
            &self.text,
            &self.text_dim,
            &self.muted,
            &self.accent,
            &self.accent_strong,
            &self.accent_soft,
            &self.danger,
            &self.success,
            &self.warning,
            &self.wave_low,
            &self.wave_mid,
            &self.wave_high,
            &self.shadow,
        ] {
            parse_color(value, origin)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum ColorRole {
    Bg,
    BgDeep,
    BgInset,
    BgPanel,
    BgFooter,
    BgPanel2,
    BgSelect,
    Line,
    LineDim,
    LineInner,
    LineSoft,
    LineHi,
    LinePop,
    Text,
    TextDim,
    Muted,
    Accent,
    AccentStrong,
    AccentSoft,
    Danger,
    Success,
    Warning,
    WaveLow,
    WaveMid,
    WaveHigh,
    Shadow,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum FontWeight {
    Normal,
    Medium,
    Semibold,
    Bold,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum FontFamily {
    Display,
    Sans,
    Mono,
}

/// Parses and validates a complete skin document.
///
/// # Errors
/// Returns [`UiDocError`] when the envelope, body, or palette is invalid.
pub fn parse_skin(text: &str, origin: &SourceUri) -> Result<SkinDoc, UiDocError> {
    let envelope = envelope::probe(text, origin)?;
    if envelope.kind != DocKind::Skin {
        return Err(UiDocError::WrongDocKind {
            origin: origin.clone(),
            expected: DocKind::Skin.name(),
            found: envelope.kind.name(),
        });
    }
    let document: SkinDoc =
        ron_io::options()
            .from_str(text)
            .map_err(|source| UiDocError::Syntax {
                origin: origin.clone(),
                source: Box::new(source),
            })?;
    document.palette.validate(origin)?;
    Ok(document)
}

pub(crate) fn parse_color(value: &str, origin: &SourceUri) -> Result<[u8; 4], UiDocError> {
    let digits = value
        .strip_prefix('#')
        .ok_or_else(|| bad_color(origin, value))?;
    if digits.len() != 6 && digits.len() != 8 {
        return Err(bad_color(origin, value));
    }
    let component = |start| {
        let pair = digits
            .get(start..start + 2)
            .ok_or_else(|| bad_color(origin, value))?;
        u8::from_str_radix(pair, 16).map_err(|_| bad_color(origin, value))
    };
    Ok([
        component(0)?,
        component(2)?,
        component(4)?,
        if digits.len() == 8 {
            component(6)?
        } else {
            255
        },
    ])
}

fn bad_color(origin: &SourceUri, value: &str) -> UiDocError {
    UiDocError::BadColor {
        origin: origin.clone(),
        value: value.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::builtin;

    #[kithara::test]
    fn palette_holds_exactly_the_declared_roles() {
        assert_eq!(
            builtin::skin_doc().palette,
            PaletteDoc {
                bg: "#12121f".to_owned(),
                bg_deep: "#0b0b16".to_owned(),
                bg_inset: "#15152a".to_owned(),
                bg_panel: "#20203a".to_owned(),
                bg_footer: "#1b1b32".to_owned(),
                bg_panel_2: "#26264a".to_owned(),
                bg_select: "#26264a".to_owned(),
                line: "#3b3b67".to_owned(),
                line_dim: "#242442".to_owned(),
                line_inner: "#2a2a4c".to_owned(),
                line_soft: "#2a2a4c".to_owned(),
                line_hi: "#4a4a7a".to_owned(),
                line_pop: "#2f2f57".to_owned(),
                text: "#e6e6e6".to_owned(),
                text_dim: "#a7aac2".to_owned(),
                muted: "#6f7189".to_owned(),
                accent: "#bb9442".to_owned(),
                accent_strong: "#d6ad59".to_owned(),
                accent_soft: "#bb94422e".to_owned(),
                danger: "#e64d4d".to_owned(),
                success: "#66cc66".to_owned(),
                warning: "#e6b333".to_owned(),
                wave_low: "#eb298c".to_owned(),
                wave_mid: "#f2d129".to_owned(),
                wave_high: "#2ec7eb".to_owned(),
                shadow: "#000000".to_owned(),
            }
        );
    }
}
