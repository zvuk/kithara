use kithara_ui::{
    draw::{DrawListBuilder, Rect, Rgba},
    render::{
        CustomSkin, UiEvent,
        custom::{CustomKinds, CustomWidget, Size2, SizeLimits, TextMeasurer},
    },
};

/// Whether this host has a painter for a control, or still mounts it as a
/// correctly-sized empty box.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Paints {
    Yes,
    /// A native pass draws this control after the Vello scene.
    Native,
    /// There is no picture to draw. A window-drag region is a place the hand
    /// grabs the window by, and the immediate host draws nothing for it either,
    /// so an empty scene here is the control working rather than a gap. That
    /// claim is checked on both hosts rather than taken on trust, by
    /// `the_retained_window_drag_region_carries_the_drag_and_draws_nothing` in
    /// `render::masonry` and `a_drag_surface_carries_the_window_and_draws_nothing`
    /// in `render::window::surface`.
    Nothing,
}

/// Every control the shared base draws, and whether Masonry draws it today.
///
/// Native output is counted separately from Vello output, so an intentionally
/// empty Vello scene cannot make a working second-pass control look undrawn.
///
/// Every `ControlSpec` variant has a row. A census that covered only the
/// controls someone remembered to add left the rest invisible: not drawn, and
/// not reported as undrawn either.
pub(crate) const CONTROL_CENSUS: &[(&str, Paints, &str)] = &[
    ("Brand", Paints::Yes, r#"Brand(id: "control")"#),
    ("Spacer", Paints::Yes, r#"Spacer(id: "control")"#),
    ("Divider", Paints::Yes, r#"Divider(id: "control")"#),
    (
        "PresetSelector",
        Paints::Yes,
        r#"PresetSelector(id: "control")"#,
    ),
    (
        "SettingsButton",
        Paints::Yes,
        r#"SettingsButton(id: "control")"#,
    ),
    ("DeckSummary", Paints::Yes, r#"DeckSummary(id: "control")"#),
    (
        "WindowDrag",
        Paints::Nothing,
        r#"WindowDrag(id: "control")"#,
    ),
    (
        "TitleBar",
        Paints::Yes,
        r#"TitleBar(id: "control", label: "KITHARA")"#,
    ),
    (
        "WindowControls",
        Paints::Yes,
        r#"WindowControls(id: "control")"#,
    ),
    (
        // The placeholder names which stand-in a deck shows when no tempo was
        // measured, not a word to display; `time` is the one the shipped decks
        // ask for.
        "Bpm",
        Paints::Yes,
        r#"Bpm(id: "control", placeholder: Some("time"))"#,
    ),
    (
        "Time",
        Paints::Yes,
        r#"Time(id: "control", read: Model(id: "deck.view.zoom"))"#,
    ),
    (
        "Scalar",
        Paints::Yes,
        r#"Scalar(id: "control", read: Model(id: "deck.view.zoom"))"#,
    ),
    (
        "Wave",
        Paints::Yes,
        r#"Wave(id: "control", read: Model(id: "demo.wave"))"#,
    ),
    (
        "Vis",
        Paints::Native,
        r#"Vis(id: "control", read: Model(id: "vis.preset"))"#,
    ),
    (
        "Sprite",
        Paints::Yes,
        r#"Sprite(id: "control", sheet: "spinner", seconds: 1.6, read: Model(id: "ui.clock.seconds"))"#,
    ),
    (
        "Lottie",
        Paints::Yes,
        r#"Lottie(id: "control", artwork: "pulse", seconds: 1.6, read: Model(id: "ui.clock.seconds"))"#,
    ),
    (
        // Unlike `Vis`, a shader is not a second pass beside the scene: the
        // retained host encodes the image draw into the Vello scene itself, and
        // the GPU pass fills the very image that draw points at.
        "Shader",
        Paints::Yes,
        r#"Shader(id: "control", source: "census.wgsl", uniforms: { "level": Model(id: "deck.view.zoom") })"#,
    ),
    (
        // What it draws is the application's, so this says only that the host
        // reached the registered widget and replayed what it drew.
        "Custom",
        Paints::Yes,
        r#"Custom(id: "control", kind: "census-extension")"#,
    ),
    (
        "Table",
        Paints::Yes,
        r#"Table(id: "control", read: Model(id: "library.visible_tracks"), columns: [(id: "title", label: "TITLE", style: Primary, width: 180.0)])"#,
    ),
    (
        "Tree",
        Paints::Yes,
        r#"Tree(id: "control", read: Model(id: "library.tree"), query: Model(id: "library.query"))"#,
    ),
    (
        // The path in view is the strip's own reading, and the fixture never
        // bound one: a strip with no path names nothing, so it drew nothing
        // for a reason that had nothing to do with this host.
        "ContextBar",
        Paints::Yes,
        r#"ContextBar(id: "control", read: Model(id: "library.breadcrumb"), scope_items: ["ALL", "MINE"], scope: Model(id: "library.scope"), write: Model(id: "library.scope"))"#,
    ),
    (
        "Text",
        Paints::Yes,
        r#"Text(id: "control", label: Some("HELLO"))"#,
    ),
    (
        "Knob",
        Paints::Yes,
        r#"Knob(id: "control", read: Parameter(id: "player.output.volume"), write: Parameter(id: "player.output.volume"))"#,
    ),
    (
        "Chip",
        Paints::Yes,
        r#"Chip(id: "control", label: "A", read: Model(id: "ui.menu.open"))"#,
    ),
    (
        "NavItem",
        Paints::Yes,
        r#"NavItem(id: "control", label: "LIBRARY", icon: Playlist, read: Model(id: "ui.menu.open"))"#,
    ),
    (
        "Button",
        Paints::Yes,
        r#"Button(id: "control", label: "PLAY", read: Model(id: "ui.menu.open"))"#,
    ),
    (
        "Glyph",
        Paints::Yes,
        r#"Glyph(id: "control", icon: Playlist)"#,
    ),
    (
        "TabLarge",
        Paints::Yes,
        r#"TabLarge(id: "control", label: "MIXER", read: Model(id: "ui.menu.open"))"#,
    ),
    (
        "Toggle",
        Paints::Yes,
        r#"Toggle(id: "control", read: Model(id: "ui.menu.open"))"#,
    ),
    (
        "Checkbox",
        Paints::Yes,
        r#"Checkbox(id: "control", read: Model(id: "ui.menu.open"))"#,
    ),
    (
        "Segmented",
        Paints::Yes,
        r#"Segmented(id: "control", items: ["A", "B"], read: Model(id: "library.scope"))"#,
    ),
    (
        "Select",
        Paints::Yes,
        r#"Select(id: "control", label: "QUALITY")"#,
    ),
    (
        "StatusDot",
        Paints::Yes,
        r#"StatusDot(id: "control", label: "LIVE")"#,
    ),
    (
        "Swatch",
        Paints::Yes,
        r#"Swatch(id: "control", role: Accent, label: "ACCENT")"#,
    ),
    (
        "Cell",
        Paints::Yes,
        r#"Cell(id: "control", label: Some("A1"))"#,
    ),
    (
        "Readout",
        Paints::Yes,
        r#"Readout(id: "control", label: Some("BPM"), read: Model(id: "library.breadcrumb"))"#,
    ),
    (
        "Meter",
        Paints::Yes,
        r#"Meter(id: "control", read: Model(id: "deck.view.zoom"))"#,
    ),
    (
        "VuVertical",
        Paints::Yes,
        r#"VuVertical(id: "control", read: Telemetry(id: "player.output.levels"))"#,
    ),
    (
        "VuStereo",
        Paints::Yes,
        r#"VuStereo(id: "control", read: Telemetry(id: "player.output.levels"))"#,
    ),
    (
        "Fader",
        Paints::Yes,
        r#"Fader(id: "control", read: Parameter(id: "player.output.volume"), write: Parameter(id: "player.output.volume"))"#,
    ),
    (
        "Crossfader",
        Paints::Yes,
        r#"Crossfader(id: "control", read: Parameter(id: "player.output.volume"), write: Parameter(id: "player.output.volume"))"#,
    ),
    (
        "PortalMap",
        Paints::Yes,
        r#"PortalMap(id: "control", read: Model(id: "pivot.map"))"#,
    ),
    (
        "Range",
        Paints::Yes,
        r#"Range(id: "control", read: Model(id: "pivot.range"), write: Parameter(id: "pivot.range"))"#,
    ),
];

/// The kind the census document names, registered below so the `Custom` row
/// draws the extension it stands for rather than the empty box a host falls to
/// when the registry it was handed does not hold the name.
pub(crate) const CENSUS_KIND: &str = "census-extension";

/// Opaque, so an empty Vello scene cannot be mistaken for ink nothing can see.
const CENSUS_INK: Rgba = Rgba {
    a: 1.0,
    b: 1.0,
    g: 1.0,
    r: 1.0,
};

/// A registered extension that paints, so the `Custom` row answers for the
/// mount path rather than for a widget that chose to draw nothing.
struct CensusExtension;

impl CustomWidget for CensusExtension {
    type Action = ();

    fn measure(&mut self, _text: &mut TextMeasurer<'_>, _limits: SizeLimits) -> Size2 {
        Size2::new(40.0, 40.0)
    }

    fn paint(
        &mut self,
        list: &mut DrawListBuilder,
        _text: &mut TextMeasurer<'_>,
        bounds: Rect,
        skin: &CustomSkin,
    ) {
        list.fill_rect(bounds, skin.color("ink").unwrap_or(CENSUS_INK));
    }
}

pub(crate) fn census_kinds() -> CustomKinds {
    CustomKinds::default().with(CENSUS_KIND, || CensusExtension, |()| UiEvent::OpenSettings)
}

/// The sources the census table names beside the controls themselves. Only the
/// shader row needs one; an entry nobody asks for costs the other rows nothing.
pub(crate) const CENSUS_SOURCES: &[(&str, &str)] = &[(
    "census.wgsl",
    r"
@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    return vec4<f32>(kithara.level.x, position.x / kithara.viewport.x, 0.0, 1.0);
}
",
)];
