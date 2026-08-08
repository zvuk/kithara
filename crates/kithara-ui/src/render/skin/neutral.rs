use crate::{
    draw::Rgba,
    error::UiDocError,
    ids::SourceUri,
    render::theme::RenderPalette,
    skin::{
        ButtonSkin, CellSkin, CheckboxSkin, ChipSkin, ChromeSkin, ColorRole, CrossfaderSkin,
        DeckSkin, DividerSkin, DragSkin, FaderSkin, GlobalBarSkin, KnobSkin, LayoutPreviewSkin,
        LayoutSkin, MenuSkin, MeterSkin, NavSkin, PopSkin, ReadoutSkin, SegmentedSkin, SelectSkin,
        SkinDoc, StatusDotSkin, SwatchSkin, TabLargeSkin, TelemetrySkin, TextInputSkin, TextSkin,
        ToggleSkin, TrackListSkin, TreeSkin, VisSkin, VuStereoSkin, VuVerticalSkin, WaveSkin,
        WindowSkin, parse_color,
    },
    text::{FontPolicy, TextResources},
};

const CHANNEL_MAX: f32 = 255.0;

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
    pub readout: ReadoutSkin,
    pub palette: RenderPalette,
    pub segmented: SegmentedSkin,
    pub select: SelectSkin,
    pub status_dot: StatusDotSkin,
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
    #[field(get, vis = "pub(crate)")]
    text_resources: TextResources,
    #[field(get, vis = "pub(crate)")]
    document: SkinDoc,
}

impl Skin {
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

    /// Resolves a parsed document into neutral colors and render metrics.
    ///
    /// # Errors
    /// Returns [`UiDocError`] when a palette value or embedded font is invalid.
    pub fn resolve(document: SkinDoc, origin: &SourceUri) -> Result<Self, UiDocError> {
        Self::resolve_with_font_policy(document, origin, FontPolicy::System)
    }

    /// Resolves a parsed document under an explicit font policy.
    ///
    /// # Errors
    /// Returns [`UiDocError`] when a palette value or embedded font is invalid.
    pub fn resolve_with_font_policy(
        document: SkinDoc,
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
            crossfader: document.crossfader.clone(),
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
            segmented: document.segmented,
            select: document.select,
            status_dot: document.status_dot,
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
            tree: document.tree.clone(),
            track_list: document.track_list.clone(),
            layout_preview: document.layout_preview,
            text_resources: TextResources::new(font_policy)?,
            document,
        })
    }
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
