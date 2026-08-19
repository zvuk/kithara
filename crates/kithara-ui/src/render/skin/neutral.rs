use crate::{
    draw::Rgba,
    error::UiDocError,
    ids::SourceUri,
    module::TextStyle,
    render::theme::RenderPalette,
    shaping::{FontPolicy, TextResources},
    skin::{
        ButtonSkin, CellSkin, CheckboxSkin, ChipSkin, ChromeSkin, ColorRole, CrossfaderSkin,
        DeckSkin, DividerSkin, DragSkin, FaderSkin, GlobalBarSkin, KnobSkin, LayoutPreviewSkin,
        LayoutSkin, MenuSkin, MeterSkin, NavSkin, PopSkin, PortalMapSkin, RangeSkin, ReadoutSkin,
        ScrollSkin, SegmentedSkin, SelectSkin, SkinDoc, StatusDotSkin, SwatchSkin, TabLargeSkin,
        TableSkin, TelemetrySkin, TextInputSkin, TextRoleSkin, TextSkin, ToggleSkin, TreeSkin,
        VisSkin, VuStereoSkin, VuVerticalSkin, WaveSkin, WindowSkin, parse_color,
    },
    text::TextDoc,
};

const CHANNEL_MAX: f32 = 255.0;

/// The three captions painted around a crossfader track.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct CrossfaderLabels {
    pub left: String,
    pub center: String,
    pub right: String,
}

/// Resolved skin consumed by renderers.
#[derive(Clone, Debug, PartialEq, fieldwork::Fieldwork)]
#[non_exhaustive]
#[fieldwork(opt_in, get)]
pub struct Skin {
    pub button: ButtonSkin,
    pub cell: CellSkin,
    pub checkbox: CheckboxSkin,
    pub chip: ChipSkin,
    pub chrome: ChromeSkin,
    pub crossfader: CrossfaderSkin,
    pub deck: DeckSkin,
    pub divider: DividerSkin,
    pub drag: DragSkin,
    pub fader: FaderSkin,
    pub global_bar: GlobalBarSkin,
    pub knob: KnobSkin,
    pub layout_preview: LayoutPreviewSkin,
    pub layout: LayoutSkin,
    pub menu: MenuSkin,
    pub meter: MeterSkin,
    pub nav: NavSkin,
    pub pop: PopSkin,
    pub portal_map: PortalMapSkin,
    pub range: RangeSkin,
    pub readout: ReadoutSkin,
    pub palette: RenderPalette,
    pub segmented: SegmentedSkin,
    pub select: SelectSkin,
    pub status_dot: StatusDotSkin,
    pub scroll: ScrollSkin,
    pub swatch: SwatchSkin,
    pub tab_large: TabLargeSkin,
    pub telemetry: TelemetrySkin,
    pub text_input: TextInputSkin,
    pub text: TextSkin,
    pub toggle: ToggleSkin,
    pub table: TableSkin,
    pub tree: TreeSkin,
    pub crossfader_labels: CrossfaderLabels,
    pub table_footer_rows: String,
    pub tree_search_placeholder: String,
    pub vis: VisSkin,
    pub vu_stereo: VuStereoSkin,
    pub vu_vertical: VuVerticalSkin,
    pub wave: WaveSkin,
    pub window: WindowSkin,
    #[field(get, vis = "pub(crate)")]
    text_resources: TextResources,
    #[field(get, vis = "pub(crate)")]
    document: SkinDoc,
}

/// The one rule that picks between a node's own colour and its active one.
///
/// A node is active or it is not, and the active role only wins while it is;
/// a node naming no active role keeps the base one it declared.
pub(crate) fn active_tone(
    base: Option<ColorRole>,
    active: Option<ColorRole>,
    on: bool,
) -> Option<ColorRole> {
    on.then_some(active).flatten().or(base)
}

impl Skin {
    /// The typography one document text style names, with the tone already
    /// selected.
    ///
    /// Both hosts ask the skin rather than keeping a table each: a style the
    /// two answered differently would paint the same document in two
    /// typefaces, which is the one thing the shared base exists to prevent.
    /// There is no wildcard arm, so a new style does not build until it is
    /// given a skin entry.
    pub(crate) fn text_role(
        &self,
        style: TextStyle,
        color: Option<ColorRole>,
        active_color: Option<ColorRole>,
        active: bool,
    ) -> TextRoleSkin {
        let (role, skin_active) = match style {
            TextStyle::Body => (self.text.body, None),
            TextStyle::Brand => (self.text.brand, None),
            TextStyle::BrandSmall => (self.text.brand_small, None),
            TextStyle::Caption => (self.text.caption, None),
            TextStyle::DeckLetter => (self.text.deck_letter, Some(self.text.deck_letter_active)),
            TextStyle::MicroLabel => (self.text.micro_label, None),
            TextStyle::Mono => (self.text.mono, None),
            TextStyle::PivotArrow => (self.text.pivot_arrow, None),
            TextStyle::PivotDuration => (self.text.pivot_duration, None),
            TextStyle::PivotFooter => (self.text.pivot_footer, None),
            TextStyle::PivotLabel => (self.text.pivot_label, None),
            TextStyle::PivotRatio => (self.text.pivot_ratio, None),
            TextStyle::PivotSmall => (self.text.pivot_small, None),
            TextStyle::PivotTitle => (self.text.pivot_title, None),
            TextStyle::PivotTrackArtist => (self.text.pivot_track_artist, None),
            TextStyle::PivotTrackTitle => (self.text.pivot_track_title, None),
            TextStyle::PivotValue => (self.text.pivot_value, None),
            TextStyle::Section => (self.text.section, None),
            TextStyle::Telemetry => (self.text.telemetry, None),
            TextStyle::TrackTitle => (self.text.track_title, None),
            TextStyle::VisFooter | TextStyle::VisMeta => (self.vis.meta, None),
            TextStyle::VisTitle => (self.vis.title, None),
        };
        TextRoleSkin {
            color: active_tone(color, active_color.or(skin_active), active).unwrap_or(role.color),
            ..role
        }
    }

    pub(crate) fn rgba(&self, role: ColorRole) -> Rgba {
        match role {
            ColorRole::Bg => self.palette.bg,
            ColorRole::BgDeep => self.palette.bg_deep,
            ColorRole::BgInset => self.palette.bg_inset,
            ColorRole::BgPanel => self.palette.bg_panel,
            ColorRole::BgFooter => self.palette.bg_footer,
            ColorRole::BgPanel2 => self.palette.bg_panel_2,
            ColorRole::BgSelect => self.palette.bg_select,
            ColorRole::Line => self.palette.line,
            ColorRole::LineDim => self.palette.line_dim,
            ColorRole::LineInner => self.palette.line_inner,
            ColorRole::LineSoft => self.palette.line_soft,
            ColorRole::LineHi => self.palette.line_hi,
            ColorRole::LinePop => self.palette.line_pop,
            ColorRole::Text => self.palette.text,
            ColorRole::TextDim => self.palette.text_dim,
            ColorRole::Muted => self.palette.muted,
            ColorRole::Accent => self.palette.accent,
            ColorRole::AccentStrong => self.palette.accent_strong,
            ColorRole::AccentSoft => self.palette.accent_soft,
            ColorRole::Danger => self.palette.danger,
            ColorRole::Success => self.palette.success,
            ColorRole::Warning => self.palette.warning,
            ColorRole::WaveLow => self.palette.wave_low,
            ColorRole::WaveMid => self.palette.wave_mid,
            ColorRole::WaveHigh => self.palette.wave_high,
            ColorRole::Shadow => self.palette.shadow,
        }
    }

    /// Resolves a parsed document into neutral colors and render metrics,
    /// pulling the crossfader, tree search and table footer captions from
    /// `catalog`.
    ///
    /// # Errors
    /// Returns [`UiDocError`] when a palette value or embedded font is invalid,
    /// or [`UiDocError::UnknownTextKey`] when `catalog` is missing one of those
    /// captions.
    pub fn resolve(
        document: SkinDoc,
        catalog: &TextDoc,
        origin: &SourceUri,
    ) -> Result<Self, UiDocError> {
        Self::resolve_with_font_policy(document, catalog, origin, FontPolicy::System)
    }

    /// Resolves a parsed document under an explicit font policy.
    ///
    /// # Errors
    /// Returns [`UiDocError`] when a palette value or embedded font is invalid,
    /// or [`UiDocError::UnknownTextKey`] when `catalog` is missing a caption.
    pub fn resolve_with_font_policy(
        document: SkinDoc,
        catalog: &TextDoc,
        origin: &SourceUri,
        font_policy: FontPolicy,
    ) -> Result<Self, UiDocError> {
        Ok(Self {
            palette: RenderPalette {
                bg: color(&document.palette.bg, origin)?,
                bg_deep: color(&document.palette.bg_deep, origin)?,
                bg_inset: color(&document.palette.bg_inset, origin)?,
                bg_panel: color(&document.palette.bg_panel, origin)?,
                bg_footer: color(&document.palette.bg_footer, origin)?,
                bg_panel_2: color(&document.palette.bg_panel_2, origin)?,
                bg_select: color(&document.palette.bg_select, origin)?,
                line: color(&document.palette.line, origin)?,
                line_dim: color(&document.palette.line_dim, origin)?,
                line_inner: color(&document.palette.line_inner, origin)?,
                line_soft: color(&document.palette.line_soft, origin)?,
                line_hi: color(&document.palette.line_hi, origin)?,
                line_pop: color(&document.palette.line_pop, origin)?,
                text: color(&document.palette.text, origin)?,
                text_dim: color(&document.palette.text_dim, origin)?,
                muted: color(&document.palette.muted, origin)?,
                accent: color(&document.palette.accent, origin)?,
                accent_strong: color(&document.palette.accent_strong, origin)?,
                accent_soft: color(&document.palette.accent_soft, origin)?,
                danger: color(&document.palette.danger, origin)?,
                success: color(&document.palette.success, origin)?,
                warning: color(&document.palette.warning, origin)?,
                wave_low: color(&document.palette.wave_low, origin)?,
                wave_mid: color(&document.palette.wave_mid, origin)?,
                wave_high: color(&document.palette.wave_high, origin)?,
                shadow: color(&document.palette.shadow, origin)?,
            },
            layout: document.layout,
            chrome: document.chrome,
            window: document.window,
            text_input: document.text_input,
            knob: document.knob,
            crossfader: document.crossfader,
            crossfader_labels: CrossfaderLabels {
                left: text_field(catalog, "crossfader.left_label", origin)?,
                center: text_field(catalog, "crossfader.center_label", origin)?,
                right: text_field(catalog, "crossfader.right_label", origin)?,
            },
            vu_stereo: document.vu_stereo,
            vu_vertical: document.vu_vertical,
            vis: document.vis,
            toggle: document.toggle,
            checkbox: document.checkbox,
            readout: document.readout,
            chip: document.chip,
            button: document.button,
            nav: document.nav,
            tab_large: document.tab_large,
            text: document.text,
            menu: document.menu,
            pop: document.pop,
            portal_map: document.portal_map,
            range: document.range,
            segmented: document.segmented,
            select: document.select,
            status_dot: document.status_dot,
            scroll: document.scroll,
            swatch: document.swatch,
            cell: document.cell,
            fader: document.fader,
            wave: document.wave,
            deck: document.deck,
            global_bar: document.global_bar,
            divider: document.divider,
            drag: document.drag,
            meter: document.meter,
            telemetry: document.telemetry,
            tree: document.tree,
            tree_search_placeholder: text_field(catalog, "tree.search_placeholder", origin)?,
            table: document.table,
            table_footer_rows: text_field(catalog, "table.footer_rows", origin)?,
            layout_preview: document.layout_preview,
            text_resources: TextResources::new(font_policy)?,
            document,
        })
    }
}

fn text_field(catalog: &TextDoc, key: &str, origin: &SourceUri) -> Result<String, UiDocError> {
    catalog
        .get(key)
        .map(str::to_owned)
        .ok_or_else(|| UiDocError::UnknownTextKey {
            origin: origin.clone(),
            key: key.to_owned(),
            path: format!("skin.{key}"),
        })
}

fn color(value: &str, origin: &SourceUri) -> Result<Rgba, UiDocError> {
    let [red, green, blue, alpha] = parse_color(value, origin)?;
    Ok(Rgba {
        r: f32::from(red) / CHANNEL_MAX,
        g: f32::from(green) / CHANNEL_MAX,
        b: f32::from(blue) / CHANNEL_MAX,
        a: f32::from(alpha) / CHANNEL_MAX,
    })
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{builtin, module::TextStyle, skin::ColorRole};

    #[kithara::test]
    fn every_text_style_resolves_to_its_own_skin_role() {
        let skin = builtin::skin();

        for (style, role) in [
            (TextStyle::Body, skin.text.body),
            (TextStyle::Brand, skin.text.brand),
            (TextStyle::BrandSmall, skin.text.brand_small),
            (TextStyle::DeckLetter, skin.text.deck_letter),
            (TextStyle::TrackTitle, skin.text.track_title),
            (TextStyle::Telemetry, skin.text.telemetry),
            (TextStyle::MicroLabel, skin.text.micro_label),
            (TextStyle::Section, skin.text.section),
            (TextStyle::Mono, skin.text.mono),
            (TextStyle::PivotArrow, skin.text.pivot_arrow),
            (TextStyle::PivotDuration, skin.text.pivot_duration),
            (TextStyle::PivotFooter, skin.text.pivot_footer),
            (TextStyle::PivotLabel, skin.text.pivot_label),
            (TextStyle::PivotRatio, skin.text.pivot_ratio),
            (TextStyle::PivotSmall, skin.text.pivot_small),
            (TextStyle::PivotTrackArtist, skin.text.pivot_track_artist),
            (TextStyle::PivotTrackTitle, skin.text.pivot_track_title),
            (TextStyle::PivotTitle, skin.text.pivot_title),
            (TextStyle::PivotValue, skin.text.pivot_value),
            (TextStyle::Caption, skin.text.caption),
            (TextStyle::VisFooter, skin.vis.meta),
            (TextStyle::VisMeta, skin.vis.meta),
            (TextStyle::VisTitle, skin.vis.title),
        ] {
            assert_eq!(skin.text_role(style, None, None, false), role, "{style:?}");
        }
    }

    #[kithara::test]
    fn a_node_colour_stands_in_for_the_one_the_role_carries() {
        let skin = builtin::skin();

        assert_eq!(
            skin.text_role(TextStyle::Mono, Some(ColorRole::Text), None, false),
            TextRoleSkin {
                color: ColorRole::Text,
                ..skin.text.mono
            }
        );
    }

    #[kithara::test]
    fn a_node_switches_between_the_two_colours_it_names() {
        let skin = builtin::skin();
        let role = |active| {
            skin.text_role(
                TextStyle::Mono,
                Some(ColorRole::Muted),
                Some(ColorRole::Accent),
                active,
            )
        };

        assert_eq!(
            role(true),
            TextRoleSkin {
                color: ColorRole::Accent,
                ..skin.text.mono
            }
        );
        assert_eq!(
            role(false),
            TextRoleSkin {
                color: ColorRole::Muted,
                ..skin.text.mono
            }
        );
    }

    #[kithara::test]
    fn an_active_node_naming_one_colour_keeps_it() {
        let skin = builtin::skin();

        assert_eq!(
            skin.text_role(TextStyle::Caption, Some(ColorRole::Accent), None, true),
            TextRoleSkin {
                color: ColorRole::Accent,
                ..skin.text.caption
            }
        );
    }

    #[kithara::test]
    fn the_deck_letter_takes_the_active_colour_its_skin_entry_declares() {
        let skin = builtin::skin();
        let base = skin.text_role(TextStyle::DeckLetter, None, None, false);

        assert_eq!(base, skin.text.deck_letter);
        assert_eq!(
            skin.text_role(TextStyle::DeckLetter, None, None, true),
            TextRoleSkin {
                color: skin.text.deck_letter_active,
                ..base
            }
        );
        assert_eq!(
            skin.text_role(TextStyle::DeckLetter, None, Some(ColorRole::Warning), true),
            TextRoleSkin {
                color: ColorRole::Warning,
                ..base
            }
        );
    }

    #[kithara::test]
    fn brand_small_resolves_under_the_display_family_and_never_the_mono_one() {
        let skin = builtin::skin();
        let role = skin.text_role(TextStyle::BrandSmall, None, None, false);

        assert_eq!(role, skin.text.brand_small);
        assert_eq!(
            skin.text_role(TextStyle::BrandSmall, None, None, true),
            role
        );
        assert_ne!(
            role.font, skin.text.mono.font,
            "the mono micro roles are Mono and the brand pair is Display"
        );
    }

    #[kithara::test]
    fn a_style_declaring_no_active_colour_ignores_the_flag() {
        let skin = builtin::skin();

        for style in [
            TextStyle::Body,
            TextStyle::Brand,
            TextStyle::TrackTitle,
            TextStyle::Telemetry,
            TextStyle::MicroLabel,
            TextStyle::Section,
            TextStyle::Mono,
            TextStyle::PivotArrow,
            TextStyle::PivotTitle,
            TextStyle::PivotValue,
            TextStyle::Caption,
            TextStyle::VisFooter,
            TextStyle::VisMeta,
            TextStyle::VisTitle,
        ] {
            assert_eq!(
                skin.text_role(style, None, None, true),
                skin.text_role(style, None, None, false),
                "{style:?}"
            );
        }
    }
}
