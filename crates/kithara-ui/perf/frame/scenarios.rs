use crate::{Interaction, Scenario};

/// What every host and case of this harness share: the fixture layout, the box
/// each control is mounted alone in, and the table of controls it measures.
pub(crate) struct Consts;

impl Consts {
    pub(crate) const HEIGHT: u16 = 120;
    pub(crate) const LAYOUT: &str = "fixture.klayout.ron";
    pub(crate) const SCENARIOS: &[Scenario] = &[
        Scenario {
            name: "Brand",
            control: r#"Brand(id: "control")"#,
            interaction: Interaction::DataChange,
        },
        Scenario {
            name: "Spacer",
            control: r#"Spacer(id: "control")"#,
            interaction: Interaction::DataChange,
        },
        Scenario {
            name: "Divider",
            control: r#"Divider(id: "control")"#,
            interaction: Interaction::DataChange,
        },
        Scenario {
            name: "PresetSelector",
            control: r#"PresetSelector(id: "control")"#,
            interaction: Interaction::Press,
        },
        Scenario {
            name: "SettingsButton",
            control: r#"SettingsButton(id: "control")"#,
            interaction: Interaction::Press,
        },
        Scenario {
            name: "DeckSummary",
            control: r#"DeckSummary(id: "control")"#,
            interaction: Interaction::DataChange,
        },
        Scenario {
            name: "WindowDrag",
            control: r#"WindowDrag(id: "control")"#,
            interaction: Interaction::Drag,
        },
        Scenario {
            name: "TitleBar",
            control: r#"TitleBar(id: "control", label: "KITHARA")"#,
            interaction: Interaction::DataChange,
        },
        Scenario {
            name: "WindowControls",
            control: r#"WindowControls(id: "control")"#,
            interaction: Interaction::Press,
        },
        Scenario {
            name: "Bpm",
            control: r#"Bpm(id: "control", placeholder: Some("time"))"#,
            interaction: Interaction::DataChange,
        },
        Scenario {
            name: "Time",
            control: r#"Time(id: "control", read: Model(id: "deck.view.zoom"))"#,
            interaction: Interaction::DataChange,
        },
        Scenario {
            name: "Scalar",
            control: r#"Scalar(id: "control", read: Model(id: "deck.view.zoom"))"#,
            interaction: Interaction::DataChange,
        },
        Scenario {
            name: "Wave",
            control: r#"Wave(id: "control", read: Model(id: "demo.wave"))"#,
            interaction: Interaction::Press,
        },
        Scenario {
            name: "Vis",
            control: r#"Vis(id: "control", read: Model(id: "vis.preset"))"#,
            interaction: Interaction::DataChange,
        },
        Scenario {
            name: "Shader",
            control: r#"Shader(id: "control", source: "census.wgsl", uniforms: { "level": Model(id: "deck.view.zoom") })"#,
            interaction: Interaction::DataChange,
        },
        Scenario {
            name: "Custom",
            control: r#"Custom(id: "control", kind: "census-extension")"#,
            interaction: Interaction::DataChange,
        },
        Scenario {
            name: "Table",
            control: r#"Table(id: "control", read: Model(id: "library.visible_tracks"), columns: [(id: "title", label: "TITLE", style: Primary, width: 180.0)])"#,
            interaction: Interaction::Drag,
        },
        Scenario {
            name: "Tree",
            control: r#"Tree(id: "control", read: Model(id: "library.tree"), query: Model(id: "library.query"))"#,
            interaction: Interaction::Drag,
        },
        Scenario {
            name: "ContextBar",
            control: r#"ContextBar(id: "control", read: Model(id: "library.breadcrumb"), scope_items: ["ALL", "MINE"], scope: Model(id: "library.scope"), write: Model(id: "library.scope"))"#,
            interaction: Interaction::PressLeading,
        },
        Scenario {
            name: "Text",
            control: r#"Text(id: "control", label: Some("HELLO"))"#,
            interaction: Interaction::DataChange,
        },
        Scenario {
            name: "Knob",
            control: r#"Knob(id: "control", read: Parameter(id: "player.output.volume"), write: Parameter(id: "player.output.volume"))"#,
            interaction: Interaction::Drag,
        },
        Scenario {
            name: "Chip",
            control: r#"Chip(id: "control", label: "A", read: Model(id: "ui.menu.open"))"#,
            interaction: Interaction::Press,
        },
        Scenario {
            name: "NavItem",
            control: r#"NavItem(id: "control", label: "LIBRARY", icon: Playlist, read: Model(id: "ui.menu.open"))"#,
            interaction: Interaction::Press,
        },
        Scenario {
            name: "Button",
            control: r#"Button(id: "control", label: "PLAY", read: Model(id: "ui.menu.open"))"#,
            interaction: Interaction::Press,
        },
        Scenario {
            name: "Glyph",
            control: r#"Glyph(id: "control", icon: Playlist)"#,
            interaction: Interaction::DataChange,
        },
        Scenario {
            name: "TabLarge",
            control: r#"TabLarge(id: "control", label: "MIXER", read: Model(id: "ui.menu.open"))"#,
            interaction: Interaction::Press,
        },
        Scenario {
            name: "Toggle",
            control: r#"Toggle(id: "control", read: Model(id: "ui.menu.open"))"#,
            interaction: Interaction::Press,
        },
        Scenario {
            name: "Checkbox",
            control: r#"Checkbox(id: "control", read: Model(id: "ui.menu.open"))"#,
            interaction: Interaction::Press,
        },
        Scenario {
            name: "Segmented",
            control: r#"Segmented(id: "control", items: ["A", "B"], read: Model(id: "library.scope"))"#,
            interaction: Interaction::Press,
        },
        Scenario {
            name: "Select",
            control: r#"Select(id: "control", label: "QUALITY")"#,
            interaction: Interaction::DataChange,
        },
        Scenario {
            name: "StatusDot",
            control: r#"StatusDot(id: "control", label: "LIVE")"#,
            interaction: Interaction::DataChange,
        },
        Scenario {
            name: "Swatch",
            control: r#"Swatch(id: "control", role: Accent, label: "ACCENT")"#,
            interaction: Interaction::DataChange,
        },
        Scenario {
            name: "Cell",
            control: r#"Cell(id: "control", label: Some("A1"))"#,
            interaction: Interaction::DataChange,
        },
        Scenario {
            name: "Readout",
            control: r#"Readout(id: "control", label: Some("BPM"), read: Model(id: "library.breadcrumb"))"#,
            interaction: Interaction::DataChange,
        },
        Scenario {
            name: "Meter",
            control: r#"Meter(id: "control", read: Model(id: "deck.view.zoom"))"#,
            interaction: Interaction::DataChange,
        },
        Scenario {
            name: "VuVertical",
            control: r#"VuVertical(id: "control", read: Telemetry(id: "player.output.levels"))"#,
            interaction: Interaction::Drag,
        },
        Scenario {
            name: "VuStereo",
            control: r#"VuStereo(id: "control", read: Telemetry(id: "player.output.levels"))"#,
            interaction: Interaction::Drag,
        },
        Scenario {
            name: "Fader",
            control: r#"Fader(id: "control", read: Parameter(id: "player.output.volume"), write: Parameter(id: "player.output.volume"))"#,
            interaction: Interaction::Drag,
        },
        Scenario {
            name: "Crossfader",
            control: r#"Crossfader(id: "control", read: Parameter(id: "player.output.volume"), write: Parameter(id: "player.output.volume"))"#,
            interaction: Interaction::Drag,
        },
        Scenario {
            name: "PortalMap",
            control: r#"PortalMap(id: "control", read: Model(id: "pivot.map"))"#,
            interaction: Interaction::DataChange,
        },
        Scenario {
            name: "Range",
            control: r#"Range(id: "control", read: Model(id: "pivot.range"), write: Parameter(id: "pivot.range"))"#,
            interaction: Interaction::Drag,
        },
        // A sheet is cut once and a frame of it is one picture draw; an artwork is
        // read once and every frame of it is emitted afresh. Both are driven by a
        // reading that moves, which is the frame their hosts actually pay for.
        Scenario {
            name: "Sprite",
            control: r#"Sprite(id: "control", sheet: "spinner", seconds: 1.6, read: Model(id: "deck.view.zoom"))"#,
            interaction: Interaction::DataChange,
        },
        Scenario {
            name: "Lottie",
            control: r#"Lottie(id: "control", artwork: "pulse", seconds: 1.6, read: Model(id: "deck.view.zoom"))"#,
            interaction: Interaction::DataChange,
        },
    ];
    pub(crate) const WIDTH: u16 = 240;
}
